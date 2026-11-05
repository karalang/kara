//! B-2026-11-05-31: the five style lints of design.md § `karac fix` and
//! `karac explain`, each a warning with a machine-applicable fix that
//! `karac fix` applies: a counter `while` becomes a `for` over a range,
//! `x = x + e` becomes `x += e`, a final `return e;` becomes `e`, a redundant
//! `.to_string()` / `.iter()` on a literal goes, and so does `ref` on a
//! parameter. They are off in `check` unless `-W NAME` asks for one, and on
//! in `karac fix`. `karac lint --list` lists them (§ Lint levels).

use std::path::PathBuf;
use std::process::{Command, Output};

fn karac(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .expect("karac")
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn scratch(tag: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-style-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    f
}

/// `karac check`'s stderr, then the file after `karac fix`, the stderr of a
/// check of the fixed file, and both versions' `run --interp` output.
struct Fixed {
    warnings: String,
    after: String,
    recheck: String,
    before_out: String,
    after_out: String,
}

fn fix(tag: &str, lint: &str, src: &str) -> Fixed {
    let f = scratch(tag, src);
    let p = f.to_str().unwrap();
    let warnings = text(&karac(&["check", "-W", lint, p]).stderr);
    let before = karac(&["run", "--interp", p]);
    assert!(before.status.success(), "{tag}: {}", text(&before.stderr));
    let fixed = karac(&["fix", p]);
    assert!(fixed.status.success(), "{tag}: {}", text(&fixed.stderr));
    let after = std::fs::read_to_string(&f).unwrap();
    let recheck = text(&karac(&["check", "-W", lint, p]).stderr);
    let run = karac(&["run", "--interp", p]);
    assert!(
        run.status.success(),
        "{tag}: {after}\n{}",
        text(&run.stderr)
    );
    let _ = std::fs::remove_dir_all(f.parent().unwrap());
    Fixed {
        warnings,
        after,
        recheck,
        before_out: text(&before.stdout),
        after_out: text(&run.stdout),
    }
}

fn assert_fixed(tag: &str, lint: &str, src: &str, want: &str) {
    let r = fix(tag, lint, src);
    assert!(
        r.warnings.contains(&format!("[{lint}]")) || r.warnings.contains(lint),
        "{tag}: no {lint} warning:\n{}",
        r.warnings
    );
    assert_eq!(r.after, want, "{tag}: the fix");
    assert!(
        !r.recheck.contains(lint),
        "{tag}: still warns:\n{}",
        r.recheck
    );
    assert_eq!(
        r.before_out, r.after_out,
        "{tag}: the fix changed the output"
    );
}

#[test]
fn style_lint_counter_while_becomes_a_for_over_a_range() {
    assert_fixed(
        "while",
        "counter_while_loop",
        "fn main() {\n    let v = [3, 1, 4];\n    let mut i = 0;\n    while i < v.len() {\n        println(v[i]);\n        i += 1;\n    }\n    let mut j = 1;\n    while j <= 3 {\n        println(j);\n        j += 1;\n    }\n}\n",
        "fn main() {\n    let v = [3, 1, 4];\n    for i in 0..v.len() {\n        println(v[i]);\n    }\n    for j in 1..=3 {\n        println(j);\n    }\n}\n",
    );
}

#[test]
fn style_lint_manual_compound_assign_becomes_an_operator_assign() {
    assert_fixed(
        "compound",
        "manual_compound_assign",
        "fn main() {\n    let mut x = 10;\n    x = x + 2;\n    x = x * (3 - 1);\n    x = x % 7;\n    println(x);\n    let mut s = \"a\";\n    s = s + \"b\";\n    println(s);\n}\n",
        "fn main() {\n    let mut x = 10;\n    x += 2;\n    x *= (3 - 1);\n    x %= 7;\n    println(x);\n    let mut s = \"a\";\n    s += \"b\";\n    println(s);\n}\n",
    );
}

#[test]
fn style_lint_needless_return_becomes_the_tail() {
    assert_fixed(
        "return",
        "needless_return",
        "fn double(n: i64) -> i64 {\n    let m = n * 2;\n    return m;\n}\n\nfn main() {\n    println(double(21));\n}\n",
        "fn double(n: i64) -> i64 {\n    let m = n * 2;\n    m\n}\n\nfn main() {\n    println(double(21));\n}\n",
    );
}

#[test]
fn style_lint_redundant_literal_method_is_removed() {
    assert_fixed(
        "literal",
        "redundant_literal_method",
        "fn main() {\n    let s: String = \"hi\".to_string();\n    println(s);\n    for x in [1, 2].iter() {\n        println(x);\n    }\n}\n",
        "fn main() {\n    let s: String = \"hi\";\n    println(s);\n    for x in [1, 2] {\n        println(x);\n    }\n}\n",
    );
}

#[test]
fn style_lint_redundant_ref_param_is_removed() {
    assert_fixed(
        "ref",
        "redundant_ref_param",
        "fn size(v: ref Vec[i64]) -> i64 {\n    v.len()\n}\n\nfn main() {\n    let v: Vec[i64] = Vec.new();\n    println(size(v));\n}\n",
        "fn size(v: Vec[i64]) -> i64 {\n    v.len()\n}\n\nfn main() {\n    let v: Vec[i64] = Vec.new();\n    println(size(v));\n}\n",
    );
}

/// Shapes where the rewrite would change the program, or not compile, stay
/// silent.
#[test]
fn style_lints_stay_silent_where_the_rewrite_would_not_hold() {
    let src = "struct P { x: i64 }\n\
fn early(n: i64) -> i64 {\n    if n > 0 {\n        return n;\n    }\n    0\n}\n\
fn pick(a: Option[i64]) -> i64 {\n    return match a { Some(x) => x, None => 0 } + 1;\n}\n\
fn is_a(c: ref char) -> bool {\n    *c == 'a'\n}\n\
fn apply(f: ref Fn(i64) -> i64, x: i64) -> i64 {\n    f(x)\n}\n\
fn main() {\n\
    let mut i = 0;\n    while i < 3 {\n        i += 1;\n    }\n    println(i);\n\
    let mut k = 0;\n    while k < 5 {\n        if k == 2 {\n            k += 2;\n            continue;\n        }\n        k += 1;\n    }\n\
    let mut v: Vec[i64] = Vec.new();\n    v.push(1);\n    let mut j = 0;\n    while j < v.len() {\n        if v.len() < 4 {\n            v.push(j);\n        }\n        j += 1;\n    }\n\
    let mut s = \"a\";\n    s = \"b\" + s;\n    println(s);\n\
    let mut y = 1;\n    y = 2 + y;\n    println(y);\n\
    let n = 5.to_string();\n    println(n);\n\
    println(early(3));\n    println(pick(Some(1)));\n    println(is_a('a'));\n    println(apply(|x| x + 1, 1));\n\
    for p in [\"a\", \"b\"].iter() {\n        println(*p);\n    }\n\
}\n";
    let f = scratch("silent", src);
    let out = karac(&[
        "check",
        "-W",
        "counter_while_loop",
        "-W",
        "manual_compound_assign",
        "-W",
        "needless_return",
        "-W",
        "redundant_literal_method",
        "-W",
        "redundant_ref_param",
        f.to_str().unwrap(),
    ]);
    let _ = std::fs::remove_dir_all(f.parent().unwrap());
    let err = text(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{err}");
    for lint in [
        "counter_while_loop",
        "manual_compound_assign",
        "needless_return",
        "redundant_literal_method",
        "redundant_ref_param",
    ] {
        assert!(!err.contains(lint), "{lint} fired:\n{err}");
    }
}

#[test]
fn style_lints_follow_lint_level_attributes_and_flags() {
    let src = "#[allow(needless_return)]\nfn a() -> i64 {\n    return 1;\n}\n\n\
#[expect(needless_return)]\nfn b() -> i64 {\n    return 2;\n}\n\n\
fn c() -> i64 {\n    return 3;\n}\n\n\
fn main() {\n    println(a() + b() + c());\n}\n";
    let f = scratch("levels", src);
    let p = f.to_str().unwrap();
    let off = karac(&["check", p]);
    let plain = karac(&["check", "-W", "needless_return", "--output=json", p]);
    let denied = karac(&["check", "-D", "needless_return", p]);
    let _ = std::fs::remove_dir_all(f.parent().unwrap());
    // Off unless asked for, as `karac fix` does.
    assert!(
        !text(&off.stderr).contains("needless_return"),
        "{}",
        text(&off.stderr)
    );
    let json = text(&plain.stdout);
    // Only `c` warns: `a` allows it (the attribute beats the flag), `b`
    // expects it, and the expectation is fulfilled.
    assert_eq!(json.matches("\"needless_return\"").count(), 1, "{json}");
    assert!(json.contains("\"code\":\"W0295\""), "{json}");
    assert!(json.contains("\"fix_diff\":["), "{json}");
    assert!(!json.contains("unfulfilled_lint_expectation"), "{json}");
    assert_eq!(plain.status.code(), Some(0), "{json}");
    assert_eq!(denied.status.code(), Some(1), "{}", text(&denied.stderr));
}

#[test]
fn style_lints_are_in_the_lint_list() {
    let list = text(&karac(&["lint", "--list"]).stdout);
    for name in [
        "counter_while_loop",
        "manual_compound_assign",
        "needless_return",
        "redundant_literal_method",
        "redundant_ref_param",
    ] {
        assert!(
            list.lines().any(|l| l.starts_with(&format!("{name} "))),
            "no {name} in:\n{list}"
        );
    }
}
