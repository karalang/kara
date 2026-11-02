//! Core semantics §6.3: `+ CrossTask` binds to the nearest function type,
//! so `Fn() -> Fn() -> i64 + CrossTask` marks the inner one, and the spec
//! says to parenthesize to mark the outer. The natural reading,
//! `(Fn() -> Fn() -> i64) + CrossTask`, was a parse error (`Expected =,
//! found +`); only `Fn() -> (Fn() -> i64) + CrossTask` parsed
//! (B-2026-11-02-20). Both now mark the outer type, and the formatter
//! prints a marked function that returns one so that it re-parses with the
//! same marking: it printed `Fn() -> (Fn() -> i64) + CrossTask` as
//! `Fn() -> Fn() -> i64 + CrossTask`, which marks the inner one.

use std::path::PathBuf;
use std::process::Command;

fn fixture(tag: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-cross-task-paren-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("p.kara");
    std::fs::write(&path, src).unwrap();
    (dir, path)
}

fn run(tag: &str, cmd: &str, src: &str) -> (bool, String, String) {
    let (dir, path) = fixture(tag, src);
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(cmd)
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

const S: &str = "shared struct S { mut n: i64 }
";

/// The program, and the same program through the formatter: a marking the
/// formatter moved would flip the verdict on the second.
fn both_forms(tag: &str, body: &str) -> Vec<(String, String)> {
    let src = format!("{S}{body}");
    let formatted = karac::format_source(&src).unwrap_or_else(|| {
        panic!(
            "{tag}: does not parse:\n{src}\n{:#?}",
            karac::parse(&src).errors
        )
    });
    assert_eq!(
        karac::format_source(&formatted).as_deref(),
        Some(formatted.as_str()),
        "{tag}: the formatter is not idempotent"
    );
    vec![(tag.to_string(), src), (format!("{tag}-fmt"), formatted)]
}

fn accepted_and_runs(tag: &str, body: &str, want: &str) {
    for (tag, src) in both_forms(tag, body) {
        let (ok, out, err) = run(&tag, "check", &src);
        assert!(ok, "{tag}: check refused it:\n{src}\n{out}{err}");
        let (_, out, err) = run(&tag, "__mir-run", &src);
        assert_eq!(out, want, "{tag}:\n{src}\n{err}");
    }
}

fn refused(tag: &str, body: &str, needle: &str) {
    for (tag, src) in both_forms(tag, body) {
        let (ok, out, err) = run(&tag, "check", &src);
        let all = out + &err;
        assert!(!ok, "{tag}: check accepted it:\n{src}");
        assert!(
            all.contains("E_NOT_CROSS_TASK") && all.contains(needle),
            "{tag}: want E_NOT_CROSS_TASK with {needle:?}:\n{src}\n{all}"
        );
    }
}

#[test]
fn a_parenthesized_function_type_takes_the_marker() {
    accepted_and_runs(
        "whole-paren",
        "fn go(f: (Fn() -> Fn() -> i64) + CrossTask) -> i64 {
    TaskGroup.new().spawn(|| f()()).join()
}
fn mk() -> (Fn() -> Fn() -> i64) + CrossTask { || || 7 }
fn main() {
    let m: (Fn() -> Fn() -> i64) + CrossTask = || || 8;
    println(go(m));
    println(go(mk()));
    println(go(|| || 9));
}
",
        "8\n7\n9\n",
    );
    // The outer function's own environment is what the marker checks.
    refused(
        "whole-paren-let",
        "fn main() {
    let s = S { n: 0 };
    let m: (Fn() -> Fn() -> i64) + CrossTask = || { let k = s.n; || k };
    println(m()());
}
",
        "a `let` whose type is `+ CrossTask`",
    );
}

#[test]
fn a_parenthesized_return_type_leaves_the_marker_to_the_outer_type() {
    accepted_and_runs(
        "return-paren",
        "fn go(f: Fn() -> (Fn() -> i64) + CrossTask) -> i64 {
    TaskGroup.new().spawn(|| f()()).join()
}
fn main() { println(go(|| || 4)); }
",
        "4\n",
    );
    // Marked outer with its own `with`: the return type stays in parens,
    // and the marker still checks the outer function's environment.
    let go = "fn go[with E](f: Fn() -> (Fn() -> i64) + CrossTask with E) -> i64 with E { f()() }
";
    accepted_and_runs(
        "return-paren-with",
        &format!("{go}fn main() {{ println(go(|| || 5)); }}\n"),
        "5\n",
    );
    refused(
        "return-paren-with-shared",
        &format!(
            "{go}fn main() {{ let s = S {{ n: 0 }}; println(go(|| {{ let k = s.n; || k }})); }}\n"
        ),
        "captures `s`",
    );
}

#[test]
fn an_unparenthesized_marker_marks_the_inner_type() {
    // Controls: the outer type is unmarked, so a capture of it is refused.
    refused(
        "inner",
        "fn go(f: Fn() -> Fn() -> i64 + CrossTask) -> i64 {
    TaskGroup.new().spawn(|| f()()).join()
}
fn main() { println(go(|| || 4)); }
",
        "capture of `f`",
    );
    refused(
        "unmarked",
        "fn go(f: Fn() -> Fn() -> i64) -> i64 {
    TaskGroup.new().spawn(|| f()()).join()
}
fn main() { println(go(|| || 4)); }
",
        "capture of `f`",
    );
}

#[test]
fn the_formatter_prints_the_marker_where_it_binds() {
    let cases = [
        // (written, printed)
        (
            "(Fn() -> Fn() -> i64) + CrossTask",
            "(Fn() -> Fn() -> i64) + CrossTask",
        ),
        (
            "Fn() -> (Fn() -> i64) + CrossTask",
            "(Fn() -> Fn() -> i64) + CrossTask",
        ),
        (
            "Fn() -> Fn() -> i64 + CrossTask",
            "Fn() -> Fn() -> i64 + CrossTask",
        ),
        (
            "(Fn() -> Fn() -> i64 + CrossTask) + CrossTask",
            "(Fn() -> Fn() -> i64 + CrossTask) + CrossTask",
        ),
        (
            "Fn() -> (Fn() -> i64) + CrossTask with reads(Db)",
            "Fn() -> (Fn() -> i64) + CrossTask with reads(Db)",
        ),
        ("(Fn() -> i64) + CrossTask", "Fn() -> i64 + CrossTask"),
    ];
    for (written, printed) in cases {
        let src = format!("fn go(f: {written}) -> i64 {{ 0 }}\n");
        let out = karac::format_source(&src).unwrap_or_else(|| panic!("{written}: does not parse"));
        assert!(
            out.contains(&format!("fn go(f: {printed}) -> i64")),
            "{written}: printed\n{out}"
        );
    }
}
