//! A closure literal handed to a named call that is itself an argument of
//! another call (`show(twice(|| s.len()))`, `println(twice(|| s.len()))`)
//! ends with its statement, as one at the root call does: the inner call
//! runs it, if at all, before it returns, and passes on a result that holds
//! no closure. A later move of what it reads was refused with E0500 "moved
//! inside a loop" in straight-line code (B-2026-11-19-113).

use std::process::{Command, Output, Stdio};

fn karac(tag: &str, args: &[&str], src: &str) -> Output {
    let path = std::env::temp_dir().join(format!(
        "karac-nested-call-closure-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).expect("write");
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    out
}

fn text(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr)
}

const TWICE: &str = "fn twice(f: Fn() -> i64) -> i64 { f() + f() }
fn show(n: i64) { println(n); }
";

#[test]
fn closure_in_a_nested_call_argument_ends_with_the_statement() {
    for (tag, call) in [
        ("show", "show(twice(|| s.len()));"),
        ("println", "println(twice(|| s.len()));"),
        ("deeper", "show(twice(|| s.len()) + twice(|| s.len() * 2));"),
    ] {
        let src = format!(
            "{TWICE}fn main() {{\n    let s = \"abc\".to_string();\n    {call}\n    let t = s;\n    println(t);\n}}\n"
        );
        let check = karac(tag, &["check"], &src);
        assert!(check.status.success(), "{tag}: {}", text(&check));
        let run = karac(tag, &["__mir-run"], &src);
        assert!(
            String::from_utf8_lossy(&run.stdout).ends_with("abc\n"),
            "{tag}: {}",
            text(&run)
        );
    }
}

#[test]
fn closure_a_nested_call_keeps_still_moves_its_capture() {
    let src = "fn keep(f: escaping Fn() -> i64) -> Fn() -> i64 { f }
fn call0(f: Fn() -> i64) -> i64 { f() }
fn show(n: i64) { println(n); }
fn main() {
    let s = \"abc\".to_string();
    show(call0(keep(|| s.len())));
    let t = s;
    println(t);
}
";
    let check = karac("escaping", &["check"], src);
    assert!(
        !check.status.success() && text(&check).contains("moved into an escaping closure"),
        "{}",
        text(&check)
    );
}
