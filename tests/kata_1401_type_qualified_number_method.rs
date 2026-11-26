//! B-2026-11-26-3: a type-qualified call of a number's method, `i32.max(a, b)`, was
//! refused as "no associated function 'max' on type 'i32'", though design.md
//! §9 Method resolution makes `T.m(x, ..)` the call `x.m(..)`, and `karac
//! fix` writes Rust's `i32::max(a, b)` so (kata 1401's Rust solution).
//! 2099ac467c (B-2026-11-25-40) made a built-in type's native method the
//! receiver call: a literal receiver takes the type, any other type is
//! refused by name. These pin kata 1401's forms of it.

use std::path::PathBuf;
use std::process::Command;

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

fn fixture(tag: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-type-qualified-number-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("p.kara");
    std::fs::write(&path, src).unwrap();
    (dir, path)
}

/// `karac check`'s success and stderr, and `karac __mir-run`'s stdout.
fn check_and_run(tag: &str, src: &str) -> (bool, String, String) {
    let (dir, path) = fixture(tag, src);
    let check = karac().arg("check").arg(&path).output().unwrap();
    let run = karac().arg("__mir-run").arg(&path).output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        check.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&check.stdout),
            String::from_utf8_lossy(&check.stderr)
        ),
        String::from_utf8_lossy(&run.stdout).into_owned(),
    )
}

/// Kata 1401's Rust solution as `karac fix` leaves it: the nearest point
/// of a rectangle by `i32.max(x1, i32.min(x, x2))`.
#[test]
fn a_number_method_called_on_its_type_is_the_method_call() {
    let src = "fn nearest(x: i32, lo: i32, hi: i32) -> i32 {
    i32.max(lo, i32.min(x, hi))
}

fn main() {
    let a: i32 = -3;
    let b: i64 = 2;
    println(f\"{nearest(5, 1, 3)} {nearest(-4, 1, 3)} {nearest(2, 1, 3)}\");
    println(f\"{i32.abs(a)} {i32.pow(a, 3)} {i64.max(b, 7)} {f64.sqrt(2.25)} {u8.min(200, 9)}\");
}
";
    let (ok, diags, out) = check_and_run("solution", src);
    assert!(ok, "check refused it:\n{diags}");
    assert_eq!(out, "3 1 2\n3 -27 7 1.5 9\n");
}

/// A literal receiver, negated or not, takes the named type, as if
/// suffixed: `u8.min(200, 9)` is a `u8` call, and `u8.min(300, 9)` is out
/// of range.
#[test]
fn a_literal_receiver_takes_the_named_number_type() {
    let src = "fn main() {
    let x: i64 = i64.max(3, 9);
    println(f\"{x} {f64.max(1.5, 0.5)} {u8.min(200, 9)} {i8.max(-3, 2)} {i8.abs(-100)} {f32.max(-1.5, -2.5)}\");
}
";
    let (ok, diags, out) = check_and_run("literal", src);
    assert!(ok, "check refused it:\n{diags}");
    assert_eq!(out, "9 1.5 9 2 100 -1.5\n");
    let (ok, diags, _) = check_and_run(
        "range",
        "fn main() {\n    println(f\"{u8.min(300, 9)}\");\n}\n",
    );
    assert!(!ok);
    assert!(
        diags.contains("integer literal 300 out of range for 'u8'"),
        "{diags}"
    );
}

/// A receiver of another number type is refused by name, since the
/// method it would pick computes in its own width: `i64.pow(a, 2)` with
/// `a: i32` is not `a.pow(2)`.
#[test]
fn a_receiver_of_another_number_type_is_refused() {
    let src = "fn main() {
    let a: i32 = 40000;
    println(f\"{i64.pow(a, 2)}\");
}
";
    let (ok, diags, _) = check_and_run("width", src);
    assert!(!ok, "check accepted a mismatched receiver");
    assert!(
        diags.contains("`i64.pow` takes a `i64` receiver, found `i32`"),
        "{diags}"
    );
}

/// A name that is no method of the type keeps the associated-function
/// refusal and its hints.
#[test]
fn an_unknown_member_keeps_the_associated_function_refusal() {
    let (ok, diags, _) = check_and_run(
        "unknown",
        "fn main() {\n    let a: i32 = 1;\n    println(f\"{i32.frobnicate(a)} {i64.max_value()}\");\n}\n",
    );
    assert!(!ok);
    assert!(
        diags.contains("no associated function 'frobnicate' on type 'i32'"),
        "{diags}"
    );
    assert!(diags.contains("write `i64.MAX`"), "{diags}");
}
