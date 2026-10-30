//! `karac fix` takes a bare parameter `own` where the body gives it away
//! (B-2026-10-29-45): by an `own self` method (it inserted
//! `#[derive(Copy)]`), by a closure's writing method (no fix), by a
//! closure's `return x` or `let y = x; y` (the caller got E0516), and by a
//! closure it returns that captures it (E0509 at the store).

use std::process::Command;

fn karac(args: &[&str], path: &std::path::Path) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn fixed(name: &str, src: &str) -> String {
    let dir = std::env::temp_dir().join(format!("karac-fixown-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    let (ok, err) = karac(&["check"], &path);
    assert!(!ok, "{name}: check accepted the unfixed program");
    let _ = karac(&["fix"], &path);
    let text = std::fs::read_to_string(&path).unwrap();
    let (ok, err2) = karac(&["check"], &path);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(ok, "{name}: {err}\nafter fix:\n{text}\n{err2}");
    text
}

#[test]
fn karac_fix_takes_a_given_away_bare_parameter_own() {
    for (name, src, want_line, out) in [
        (
            "own_self_closure",
            "struct S { v: i64 }
impl S { fn take(own self) -> i64 { self.v } }
fn main() {
    let f = |t: S, k: i64| t.take() + k;
    println(f\"{f(S { v: 1 }, 2)}\");
}
",
            "let f = |t: own S, k: i64| t.take() + k;",
            "3\n",
        ),
        (
            "own_self_fn",
            "struct S { v: i64 }
impl S { fn take(own self) -> i64 { self.v } }
fn f(t: S) -> i64 { t.take() }
fn main() { println(f\"{f(S { v: 1 })}\"); }
",
            "fn f(t: own S) -> i64 { t.take() }",
            "1\n",
        ),
        (
            "writing_method",
            "fn main() {
    let f = |vv: Vec[i64]| vv.pop();
    println(f\"{f(vec![1, 2])}\");
}
",
            "let f = |vv: own Vec[i64]| vv.pop();",
            "Some(2)\n",
        ),
        (
            "return_and_alias",
            "struct R { v: String }
fn main() {
    let f = |x: R| { return x; };
    let g = |x: R| { let y = x; y };
    let r = f(R { v: \"a\".to_string() });
    let q = g(R { v: \"b\".to_string() });
    println(f\"{r.v} {q.v}\");
}
",
            "let g = |x: own R| { let y = x; y };",
            "a b\n",
        ),
        (
            "returned_closure",
            "fn main() {
    let make = |s: String| |x: i64| x + s.len();
    let mut fs: Vec[Fn(i64) -> i64] = Vec.new();
    fs.push(make(\"abc\".to_string()));
    println(f\"{fs[0](1)}\");
}
",
            "let make = |s: own String| |x: i64| x + s.len();",
            "4\n",
        ),
    ] {
        let text = fixed(name, src);
        assert!(text.contains(want_line), "{name}: {text}");
        assert!(!text.contains("derive(Copy)"), "{name}: {text}");
        assert_eq!(
            karac::mir::lower::run_source(&text)
                .expect("MIR interpreter")
                .output,
            out,
            "{name}"
        );
    }
}

#[test]
fn a_shadowed_or_copy_parameter_is_not_taken_own() {
    let src = "struct S { v: i64 }
impl S { fn take(own self) -> i64 { self.v } }
fn f(t: S) -> i64 { let t = S { v: t.v + 1 }; t.take() }
fn main() {
    let make = |s: String| |s: i64| s + 1;
    let ab = \"ab\".to_string();
    let g = make(ab);
    let n = |k: i64| |x: i64| x + k;
    println(f\"{f(S { v: 1 })} {g(1)} {n(2)(3)}\");
}
";
    let dir = std::env::temp_dir().join(format!("karac-fixown-{}-neg", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    let (ok, err) = karac(&["check"], &path);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(ok, "{err}");
    assert_eq!(
        karac::mir::lower::run_source(src)
            .expect("MIR interpreter")
            .output,
        "2 2 5\n"
    );
}
