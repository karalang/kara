//! B-2026-11-09-21 (docs/library/core-types.md, review-thread ruling
//! 2026-11-09): `is_ok()` / `is_err()` on an `Option` are deprecated
//! spellings of `is_some()` / `is_none()`. `karac check` warns, `karac fix`
//! rewrites them, and the program means what it did; a `Result`'s own are
//! untouched.

use std::process::{Command, Output, Stdio};

fn karac(dir: &std::path::Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(dir)
        .args(args)
        .arg("m.kara")
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

#[test]
fn is_ok_and_is_err_on_an_option_warn_and_are_rewritten() {
    let src = "fn main() {
    let v: Vec[i64] = [1, 3, 5];
    let o: Option[i64] = None;
    println(v.binary_search(3).is_ok());
    println(o.is_err());
    let r: Result[i64, String] = Ok(1);
    println(r.is_ok());
}
";
    let dir = std::env::temp_dir().join(format!("karac-option-is-ok-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("m.kara"), src).unwrap();
    let check = karac(&dir, &["check"]);
    let err = String::from_utf8_lossy(&check.stderr).into_owned();
    let before = String::from_utf8_lossy(&karac(&dir, &["__mir-run"]).stdout).into_owned();
    karac(&dir, &["fix"]);
    let text = std::fs::read_to_string(dir.join("m.kara")).unwrap();
    let after_check = karac(&dir, &["check"]);
    let after = String::from_utf8_lossy(&karac(&dir, &["__mir-run"]).stdout).into_owned();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(check.status.success(), "{err}");
    assert_eq!(err.matches("warning[deprecated]").count(), 2, "{err}");
    assert!(
        err.contains(
            "4:13: use of deprecated `is_ok()` on an `Option`: it is a `Result` method, and \
             on an `Option` it means `is_some()`; it becomes an error at the next minor release"
        ) && err.contains("5:13: use of deprecated `is_err()` on an `Option`"),
        "{err}"
    );
    assert!(
        text.contains("println(v.binary_search(3).is_some());")
            && text.contains("println(o.is_none());")
            && text.contains("println(r.is_ok());"),
        "{text}"
    );
    assert!(
        String::from_utf8_lossy(&after_check.stderr).is_empty()
            || String::from_utf8_lossy(&after_check.stderr).contains("All checks passed"),
        "{}",
        String::from_utf8_lossy(&after_check.stderr)
    );
    assert_eq!(before, "true\ntrue\ntrue\n");
    assert_eq!(after, before);
}
