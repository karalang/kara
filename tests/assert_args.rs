//! B-2026-11-13-36: `assert`'s condition was never checked, so `assert(1)`
//! or `assert("x")` passed check and failed in MIR ("invalid MIR: assigning
//! i64 to _2 of type bool"); `assert_eq(1, "a")` passed check and failed in
//! MIR ("of two types is not lowered yet"); and `assert_eq(a, b, msg)` was
//! "without its operands". design.md §14 Assertions: the condition is a
//! `bool`, the operands are checked as `a == b` is, and each assertion takes
//! an optional `String` or `Str` message, evaluated only when it fails.

use std::path::Path;
use std::process::{Command, Stdio};

fn karac(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// `karac check` of `src` (success, stderr), and the MIR interpreter's
/// stdout and stderr when it checks clean.
fn run(name: &str, src: &str) -> (bool, String, String, String) {
    let dir = std::env::temp_dir().join(format!("karac-assert-args-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), src).unwrap();
    let (ok, _, err) = karac(&dir, &["check", "cell.kara"]);
    let (out, run_err) = if ok {
        let (_, o, e) = karac(&dir, &["__mir-run", "cell.kara"]);
        (o, e)
    } else {
        (String::new(), String::new())
    };
    let _ = std::fs::remove_dir_all(&dir);
    (ok, err, out, run_err)
}

fn refused(name: &str, src: &str, want: &str) {
    let (ok, err, _, _) = run(name, src);
    assert!(!ok, "{name}: accepted");
    assert!(err.contains("1 error(s) found"), "{name}: {err}");
    assert!(err.contains(want), "{name}: {err}");
}

#[test]
fn assert_condition_must_be_bool() {
    refused(
        "int",
        "fn main() { assert(1); }\n",
        "cell.kara:1:20: expected 'bool', found 'i64'",
    );
    refused(
        "str",
        "fn main() { assert(\"x\"); }\n",
        "expected 'bool', found 'String'",
    );
    refused(
        "opt",
        "fn main() { assert(Some(1)); println(9); }\n",
        "expected 'bool', found 'Option[i64]'",
    );
}

#[test]
fn assert_eq_operands_are_checked_as_equality() {
    refused(
        "mixed",
        "fn main() { assert_eq(1, \"a\".to_string()); }\n",
        "cannot compare 'i64' and 'String'",
    );
}

#[test]
fn assert_arity_and_message_type() {
    refused(
        "four",
        "fn main() { assert_eq(1, 2, \"x\", \"y\"); }\n",
        "`assert_eq` takes two operands and an optional message, but 4 arguments were given",
    );
    refused(
        "none",
        "fn main() { assert(); }\n",
        "`assert` takes a `bool` condition and an optional message, but 0 arguments were given",
    );
    refused(
        "int_msg",
        "fn main() { assert(true, 5); }\n",
        "`assert`'s message is a `String` or `Str`, shown when it fails; found 'i64'",
    );
}

#[test]
fn assert_message_replaces_the_first_line() {
    let (ok, err, _, run_err) = run(
        "fstr",
        "fn main() { let n = 3; assert(n == 4, f\"n was {n}\"); }\n",
    );
    assert!(ok, "{err}");
    assert!(run_err.contains("n was 3"), "{run_err}");
    assert!(!run_err.contains("assertion failed"), "{run_err}");

    let (ok, err, _, run_err) = run("eq_msg", "fn main() { assert_eq(1, 2, \"ctx\"); }\n");
    assert!(ok, "{err}");
    assert!(run_err.contains("ctx\n  left:  1\n  right: 2"), "{run_err}");

    // A message on a passing assertion is not evaluated or printed.
    let (ok, err, out, run_err) = run(
        "pass",
        "fn main() { let s = \"m\".to_string(); assert(1 == 1, s); assert_ne(1, 2, \"ne\"); println(\"fine\"); }\n",
    );
    assert!(ok, "{err}");
    assert_eq!(out, "fine\n", "{run_err}");
}
