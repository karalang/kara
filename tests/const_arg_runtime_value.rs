//! B-2026-11-13-22: the length of `Array[T, n]` is a const generic argument,
//! and design.md § Const generic parameters gives the error for a run-time
//! value there: "const generic argument must be a compile-time constant
//! expression". A lone name the parser routes to a type path was kept as an
//! opaque name instead, so a local or parameter only failed later, as a
//! mismatch between `Array[i64, n]` and `Array[i64, 3]`; a name nothing
//! declares was an "undefined type"; `n + 1` gave three cascading errors.
//! Each is now one error at the argument. The same path left a top-level
//! `const` opaque too, so `Array[i64, SIZE]` refused its own literal; it is
//! now the const's value.

use std::path::Path;
use std::process::{Command, Stdio};

const WHAT: &str = "const generic argument must be a compile-time constant expression";

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

/// `karac check` of `src` (its success and stderr), and its output under
/// the MIR interpreter when it checks clean.
fn check(name: &str, src: &str) -> (bool, String, String) {
    let dir = std::env::temp_dir().join(format!("karac-const-arg-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), src).unwrap();
    let (ok, _, err) = karac(&dir, &["check", "cell.kara"]);
    let out = if ok {
        karac(&dir, &["__mir-run", "cell.kara"]).1
    } else {
        String::new()
    };
    let _ = std::fs::remove_dir_all(&dir);
    (ok, err, out)
}

/// One error, at `col` of line `line`, saying `why`.
fn one_error(name: &str, src: &str, at: &str, why: &str) {
    let (ok, err, _) = check(name, src);
    assert!(!ok, "{name}: accepted");
    assert!(err.contains("1 error(s) found"), "{name}: {err}");
    assert!(
        err.contains(&format!("{at}: {WHAT}: {why}")),
        "{name}: {err}"
    );
}

#[test]
fn const_arg_a_local_or_parameter_is_a_run_time_value() {
    let why = "`n` is a run-time value (a length known only at run time needs a `Vec`)";
    one_error(
        "param",
        "fn f(n: i64) -> i64 { let a: Array[i64, n] = [0; 3]; a.len() }\nfn main() { println(f\"{f(3)}\"); }\n",
        "cell.kara:1:41",
        why,
    );
    one_error(
        "sum",
        "fn f(n: i64) -> i64 { let a: Array[i64, n + 1] = [0; 3]; a.len() }\nfn main() { println(f\"{f(3)}\"); }\n",
        "cell.kara:1:41",
        why,
    );
    // A signature is lowered twice; it is still one report, naming the
    // parameter.
    one_error(
        "sig",
        "fn f(n: i64, a: Array[i64, n]) -> i64 { a.len() + n }\nfn main() { let a: Array[i64, 3] = [0; 3]; println(f\"{f(3, a)}\"); }\n",
        "cell.kara:1:28",
        why,
    );
    one_error(
        "local",
        "fn g(a: Array[i64, 3]) -> i64 { a.len() }\nfn f() -> i64 { let k = 3; let a: Array[i64, k] = [0; 3]; g(a) }\nfn main() { println(f\"{f()}\"); }\n",
        "cell.kara:2:46",
        "`k` is a run-time value (a length known only at run time needs a `Vec`)",
    );
}

#[test]
fn const_arg_a_name_nothing_declares_is_not_an_undefined_type() {
    let why = "`zzz` is not a `const` or a const generic parameter in scope";
    one_error(
        "unbound",
        "fn f() -> i64 { let a: Array[i64, zzz] = [0; 3]; a.len() }\nfn main() { println(f\"{f()}\"); }\n",
        "cell.kara:1:35",
        why,
    );
    one_error(
        "field",
        "struct S { xs: Array[i64, zzz] }\nfn main() { println(\"x\"); }\n",
        "cell.kara:1:27",
        why,
    );
}

#[test]
fn const_arg_a_top_level_const_is_its_value() {
    let (ok, err, out) = check(
        "const",
        "const SIZE: i64 = 3;\nfn f(n: i64) -> i64 { let a: Array[i64, SIZE] = [0; 3]; a.len() + n }\nfn main() { println(f\"{f(3)}\"); }\n",
    );
    assert!(ok, "{err}");
    assert_eq!(out, "6\n");
}
