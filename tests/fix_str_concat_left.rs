//! `s + t` with a `Str` on the left (B-2026-11-03-11, found porting the
//! mini_lsm app): the check said only "type 'Str' does not implement trait
//! Add", with no hint and no edit, where `String + Str` already says what
//! to do and `karac fix` appends the `.to_string()`. Now the left operand
//! gets the same treatment: the message names the view on the left and the
//! fix makes it an owned `String`, after which the sum checks and runs.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

#[test]
fn karac_fix_owns_a_str_on_the_left_of_plus() {
    let src = "fn key(prefix: Str, n: i64) -> String {
    prefix + f\"{n}\"
}

fn join(a: Str, b: Str) -> String {
    a + b
}

fn main() {
    println(key(\"m:\", 7));
    println(join(\"x\", \"y\"));
}
";
    let path = std::env::temp_dir().join(format!("karac-str-left-{}.kara", std::process::id()));
    let file = path.to_str().unwrap();
    std::fs::write(&path, src).unwrap();

    let check = karac(&["check", file]);
    let err = String::from_utf8_lossy(&check.stderr);
    assert!(!check.status.success(), "{err}");
    assert_eq!(
        err.matches("needs an owned 'String' on its left").count(),
        2,
        "{err}"
    );
    assert!(!err.contains("does not implement trait Add"), "{err}");

    // With a `Str` on both sides the second run owns the right one too,
    // as `String + Str` already did.
    for _ in 0..2 {
        let fix = karac(&["fix", file]);
        assert!(
            fix.status.success(),
            "{}",
            String::from_utf8_lossy(&fix.stderr)
        );
    }
    let fixed = std::fs::read_to_string(&path).unwrap();
    for want in [
        "prefix.to_string() + f\"{n}\"",
        "a.to_string() + b.to_string()",
    ] {
        assert!(fixed.contains(want), "missing {want:?} in:\n{fixed}");
    }
    let recheck = karac(&["check", file]);
    let run = karac(&["__mir-run", file]);
    let _ = std::fs::remove_file(&path);
    assert!(
        recheck.status.success(),
        "{}",
        String::from_utf8_lossy(&recheck.stderr)
    );
    assert_eq!(
        (
            run.status.code(),
            String::from_utf8_lossy(&run.stdout).as_ref()
        ),
        (Some(0), "m:7\nxy\n"),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}
