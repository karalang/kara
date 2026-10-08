//! Named and default parameters (design.md § Named and default parameters):
//! a `;` in a parameter list starts the named parameters, only those take
//! defaults, a call passes them by label in any order, and the arguments are
//! evaluated in the order written. Both interpreters run the same programs,
//! and `karac check` reports each broken label rule by name.

use std::path::PathBuf;
use std::process::Command;

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

fn write(tag: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-named-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("p.kara");
    std::fs::write(&path, src).unwrap();
    path
}

/// Runs `src` on the tree-walk interpreter and the MIR interpreter and
/// asserts both print `expected`.
fn runs_on_both(tag: &str, src: &str, expected: &str) {
    let path = write(tag, src);
    for args in [&["run", "--interp"][..], &["__mir-run"][..]] {
        let out = karac().args(args).arg(&path).output().unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            expected,
            "{tag} under {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let out = karac().arg("check").arg(&path).output().unwrap();
    assert!(
        out.status.success(),
        "{tag}: check: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `karac check` rejects `src` with each of `expect` in its output.
fn check_rejects(tag: &str, src: &str, expect: &[&str]) {
    let path = write(tag, src);
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{tag}: check must reject");
    for e in expect {
        assert!(err.contains(e), "{tag}: expected `{e}` in: {err}");
    }
}

const CONNECT: &str = r#"
fn connect(host: String; port: i64 = 443, timeout_ms: i64 = 5000) -> i64 {
    port + timeout_ms
}
fn tick(label: String, v: i64) -> i64 {
    println(label);
    v
}
"#;

#[test]
fn named_arguments_fill_defaults_in_any_order() {
    let src = format!(
        "{CONNECT}fn main() {{
    println(f\"{{connect(\"db\")}}\");
    println(f\"{{connect(\"db\", timeout_ms: 1000)}}\");
    println(f\"{{connect(\"db\", timeout_ms: 1000, port: 8443)}}\");
    println(f\"{{connect(\"db\", port: 1, timeout_ms: 2)}}\");
}}
"
    );
    runs_on_both("order", &src, "5443\n1443\n9443\n3\n");
}

#[test]
fn named_arguments_are_evaluated_in_the_order_written() {
    let src = format!(
        "{CONNECT}fn main() {{
    let r = connect(\"db\", timeout_ms: tick(\"first\", 1), port: tick(\"second\", 20));
    println(f\"{{r}}\");
}}
"
    );
    runs_on_both("eval-order", &src, "first\nsecond\n21\n");
}

#[test]
fn methods_take_named_arguments_after_the_receiver() {
    let src = r#"
fn tick(label: String, v: i64) -> i64 {
    println(label);
    v
}
struct M { n: i64 }
impl M {
    fn get(ref self, key: i64; fallback: i64 = 0, scale: i64 = 1) -> i64 {
        (key + fallback + self.n) * scale
    }
    fn make(; n: i64) -> M { M { n: n } }
}
fn main() {
    let m = M.make(n: 100);
    println(f"{m.get(1)}");
    println(f"{m.get(1, scale: 2)}");
    println(f"{m.get(1, scale: tick("s", 3), fallback: tick("f", 10))}");
}
"#;
    runs_on_both("method", src, "101\n202\ns\nf\n333\n");
}

#[test]
fn a_named_parameter_without_a_default_is_required_and_literals_take_its_type() {
    let src = r#"
fn small(; n: u8 = 1, m: u8) -> u8 { n + m }
fn main() {
    println(f"{small(m: 2)} {small(m: 250, n: 5)}");
}
"#;
    runs_on_both("required", src, "3 255\n");
}

#[test]
fn an_option_default_and_a_function_value_take_only_positional_arguments() {
    let src = r#"
fn find(items: Vec[i64], target: i64; start: Option[i64] = None) -> i64 {
    match start { Some(s) => s + target, None => target }
}
fn connect(host: String; port: i64 = 443) -> i64 { port }
fn main() {
    let v: Vec[i64] = Vec.new();
    println(f"{find(v.clone(), 3)} {find(v, 3, start: Some(10))}");
    let c = connect;
    println(f"{c("x")}");
}
"#;
    runs_on_both("value", src, "3 13\n443\n");
}

#[test]
fn check_names_the_label_rule_a_call_breaks() {
    let src = r#"
fn connect(host: String; port: i64 = 443, timeout_ms: i64) -> i64 { port + timeout_ms }
fn main() {
    let a = connect(host: "db", timeout_ms: 1);
    let b = connect("db");
    let c = connect("db", timeout_ms: 1, timeout_ms: 2);
    let d = connect("db", tls: 1, timeout_ms: 2);
    let e = connect("db", 443, 1);
    println(f"{a}{b}{c}{d}{e}");
}
"#;
    check_rejects(
        "labels",
        src,
        &[
            "`host` is a positional parameter; pass it without a label",
            "missing named argument(s) `timeout_ms`",
            "`timeout_ms` is passed twice",
            "no named parameter `tls`",
            "too many positional arguments: `port` and the parameters after it are named",
        ],
    );
}

#[test]
fn check_names_the_label_rule_a_method_call_breaks() {
    let src = r#"
struct M { n: i64 }
impl M {
    fn get(ref self, key: i64; fallback: i64 = 0, scale: i64) -> i64 { key + fallback + scale }
}
fn main() {
    let m = M { n: 1 };
    let a = m.get(1);
    let b = m.get(1, 2, 3);
    println(f"{a}{b}");
}
"#;
    check_rejects(
        "method-labels",
        src,
        &[
            "missing named argument(s) `scale`",
            "`fallback` and the parameters after it are named",
        ],
    );
}

#[test]
fn check_rejects_a_default_on_a_positional_parameter() {
    let src = "fn g(a: i64 = 1, b: i64 = 2) -> i64 { a + b }\nfn main() { println(f\"{g()}\") }\n";
    check_rejects(
        "positional-default",
        src,
        &["only a named parameter can have a default; write `;` before `a`"],
    );
}

#[test]
fn a_parameter_list_has_at_most_one_semicolon() {
    let src = "fn f(a: i64; b: i64; c: i64) -> i64 { a }\nfn main() { println(f\"{f(1, b: 2, c: 3)}\") }\n";
    check_rejects("two-semis", src, &["a parameter list has at most one `;`"]);
}

#[test]
fn the_formatter_keeps_the_semicolon() {
    let src = "fn f(a: i64; b: i64 = 2) -> i64 {\n    a + b\n}\n\nfn main() {\n    println(f\"{f(1, b: 3)}\");\n}\n";
    let path = write("fmt", src);
    let out = karac().arg("fmt").arg(&path).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), src);
}
