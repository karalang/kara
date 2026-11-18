//! Fix bugs the `karac fix` idempotence sweep over corpus/ found: a fix
//! that left a program another error, which `karac fix` then could not get
//! past.
//!
//! - B-2026-11-18-67: E_MODULE_LET offered `const` for `let EMPTY:
//!   Vec[i64] = Vec.new();`, and a `const` refuses a call
//!   (E_MODULE_BINDING_EFFECTFUL_INIT).
//! - B-2026-11-18-68: a bare parameter of an unbounded type parameter,
//!   handed to a generic `own` parameter, was offered `.clone()`, which
//!   `U` does not have (E0236); its `own` is the fix.

use std::process::{Command, Output, Stdio};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

/// `karac check`'s stderr, the file after `karac fix`, and the check after.
fn check_fix_check(tag: &str, src: &str) -> (String, String, String) {
    let path =
        std::env::temp_dir().join(format!("karac-fix-sweep-{tag}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    let before = String::from_utf8_lossy(&karac(&["check"], &path).stderr).into_owned();
    let _ = karac(&["fix"], &path);
    let fixed = std::fs::read_to_string(&path).unwrap();
    let after = String::from_utf8_lossy(&karac(&["check"], &path).stderr).into_owned();
    let _ = std::fs::remove_file(&path);
    (before, fixed, after)
}

#[test]
fn a_module_let_of_vec_new_gets_no_const_fix() {
    let src = "let EMPTY: Vec[i64] = Vec.new();\nfn main() { println(EMPTY.len()); }\n";
    let (before, fixed, after) = check_fix_check("vec-new", src);
    assert!(before.contains("error[E_MODULE_LET]"), "{before}");
    assert!(before.contains("computed at run time"), "{before}");
    assert!(!before.contains("declare it with `const`"), "{before}");
    assert_eq!(fixed, src);
    assert!(
        !after.contains("E_MODULE_BINDING_EFFECTFUL_INIT"),
        "{after}"
    );
}

#[test]
fn a_module_let_of_a_constant_still_gets_the_const_fix() {
    let src = "let LIMIT: i64 = 3 * 4;\nfn main() { println(LIMIT); }\n";
    let (before, fixed, after) = check_fix_check("const-ok", src);
    assert!(before.contains("declare it with `const`"), "{before}");
    assert!(fixed.starts_with("const LIMIT: i64 = 3 * 4;"), "{fixed}");
    assert!(after.contains("All checks passed"), "{after}");
}

#[test]
fn a_bare_parameter_of_an_unbounded_type_parameter_takes_own() {
    let src = "fn passthru[T](x: own T) -> T { x }
fn outer[U](y: U) -> U { passthru(y) }
fn main() {
    let b: Array[String, 2] = outer([f\"a{1}\", f\"b{2}\"]);
    println(b[0]);
}
";
    let (before, fixed, after) = check_fix_check("unbounded", src);
    assert!(
        before.contains("cannot give away parameter `y`")
            && before.contains("`y: own U`")
            && !before.contains(".clone()"),
        "{before}"
    );
    assert!(fixed.contains("fn outer[U](y: own U) -> U"), "{fixed}");
    assert!(after.contains("All checks passed"), "{after}");
}

#[test]
fn a_clone_bounded_type_parameter_also_offers_the_copy() {
    let src = "fn passthru[T](x: own T) -> T { x }
fn outer[U: Clone](y: U) -> U { passthru(y) }
fn main() { println(outer(5)); }
";
    let (before, fixed, _) = check_fix_check("bounded", src);
    assert!(
        before.contains("`y: own U`, or hand on a copy with `.clone()`"),
        "{before}"
    );
    assert!(
        fixed.contains("fn outer[U: Clone](y: own U) -> U"),
        "{fixed}"
    );
}
