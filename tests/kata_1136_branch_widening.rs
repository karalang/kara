//! Found checking `karac fix` over kara-katas for kata 1136.
//!
//! B-2026-11-12-6: an integer literal beside a narrower branch took that
//! branch's type whether or not it fit, and an `if` whose branches each
//! widened to a checked expectation had its first branch's type. So
//! `show(if c < 128 { c } else { 65533 })` with `c: u8` stored 65533 in a
//! `u8` and printed 253 (mini_piexif's UserComment decoder, once `karac
//! fix` had deleted the `c as i64` the lint called unnecessary). The arms
//! now have the expected type, and a literal that does not fit its
//! sibling with nothing to widen to is refused.
//!
//! B-2026-11-12-7: `y = if c { a } else { y }` with `a: u8`, `y: i64` was
//! refused although `let y2: i64 = if c { a } else { y }` was accepted:
//! the assignment joined the branches once with no slot to widen to. So
//! `karac fix`, deleting the `a as i64` the lint flagged, left a program
//! that did not check (mini_prettytable, mini_protobuf, mini_csv).

use std::process::{Command, Stdio};

fn karac(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

/// `karac check`'s success and stderr, and what `__mir-run` prints (and,
/// with the backend, `__mir-llvm-run`, which must agree).
fn check_and_run(name: &str, src: &str) -> (bool, String, String) {
    let dir = std::env::temp_dir().join(format!(
        "kata_1136_branch_widening_{name}_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("m.kara"), src).unwrap();
    let check = karac(&dir, &["check", "m.kara"]);
    let err = String::from_utf8_lossy(&check.stderr).into_owned();
    let run = karac(&dir, &["__mir-run", "m.kara"]);
    let out = String::from_utf8_lossy(&run.stdout).into_owned();
    #[cfg(feature = "llvm")]
    if check.status.success() {
        let compiled = karac(&dir, &["__mir-llvm-run", "m.kara"]);
        assert_eq!(String::from_utf8_lossy(&compiled.stdout), out, "{name}");
    }
    let _ = std::fs::remove_dir_all(&dir);
    (check.status.success(), err, out)
}

#[test]
fn a_literal_beside_a_narrower_branch_widens_with_it_to_the_expectation() {
    let src = "fn show(x: i64) -> i64 {
    x
}

fn pick(c: u8) -> i64 {
    if c < 128 { c } else { 65533 }
}

fn main() {
    let bytes: Vec[u8] = [65, 233];
    for c in bytes.iter() {
        let m: i64 = match c { 65 => c, _ => 70000 };
        println(f\"{show(if c < 128 { c } else { 65533 })} {pick(c)} {m}\");
    }
    let s: i8 = -5;
    println(f\"{show(if s < 0 { s } else { 300 })}\");
}
";
    let (ok, err, out) = check_and_run("literal", src);
    assert!(ok, "{err}");
    assert_eq!(out, "65 65 65\n65533 65533 70000\n-5\n");
}

#[test]
fn a_literal_that_does_not_fit_its_sibling_with_nothing_to_widen_to_is_refused() {
    let src = "fn main() {
    let c: u8 = 233;
    let d = if c < 128 { c } else { 65533 };
    println(f\"{d}\");
}
";
    let (ok, err, _) = check_and_run("refused", src);
    assert!(!ok, "a u8 holding 65533 was accepted");
    assert!(
        err.contains("if/else branches have incompatible types: 'u8' and 'i64'"),
        "{err}"
    );
    let src = "fn main() {
    let c: u8 = 233;
    let m = match c { 65 => c, _ => 70000 };
    println(f\"{m}\");
}
";
    let (ok, err, _) = check_and_run("refused_match", src);
    assert!(!ok, "a u8 holding 70000 was accepted");
    assert!(
        err.contains("match arms have incompatible types: 'u8' and 'i64'"),
        "{err}"
    );
    // One that fits still takes the sibling's type.
    let src = "fn main() {
    let c: u8 = 233;
    let d = if c < 128 { c } else { 255 };
    let e: u8 = d;
    println(f\"{e}\");
}
";
    let (ok, err, out) = check_and_run("fits", src);
    assert!(ok, "{err}");
    assert_eq!(out, "255\n");
}

#[test]
fn an_assigned_branch_widens_to_the_place_it_is_stored_in() {
    let src = "struct D {
    q: i64,
}

fn main() {
    let big: u8 = 200;
    let neg: i8 = -5;
    let mut y: i64 = 100000;
    for c in [true, false] {
        y = if c { big } else { y + 1 };
        print(f\"{y} \");
    }
    let mut d = D { q: 0 };
    d.q = if y > 0 { neg } else { d.q };
    let mut v: Vec[i64] = [0, 0];
    v[1] = match neg { -5 => big, _ => y };
    let o: Option[u8] = Some(big);
    y = if let Some(b) = o { b } else { y };
    println(f\"{d.q} {v[1]} {y}\");
}
";
    let (ok, err, out) = check_and_run("assign", src);
    assert!(ok, "{err}");
    assert_eq!(out, "200 201 -5 200 200\n");
}

#[test]
fn an_assigned_branch_of_another_type_is_still_refused() {
    let src = "fn main() {
    let mut y: i64 = 1;
    let c = y > 0;
    y = if c { \"a\" } else { y };
    println(f\"{y}\");
}
";
    let (ok, err, _) = check_and_run("other", src);
    assert!(!ok, "a String stored into an i64 was accepted");
    assert!(err.contains("String"), "{err}");
}

/// B-2026-11-12-8: a cast stored into a binding whose literal no use has
/// fixed yet is what fixes it. `letter = b[j] as i64` after `let mut
/// letter = -1` makes `letter` an `i64`; `lossless_cast` called the cast
/// unnecessary, and `karac fix` deleting it made `letter` a `u8` that `-1`
/// cannot initialize (kata 664's differential). Into a binding whose type
/// is already fixed the cast is still flagged.
#[test]
fn a_cast_that_fixes_an_open_binding_is_not_called_unnecessary() {
    let src = "fn main() {
    let b: Vec[u8] = [3, 7];
    let mut letter = -1;
    for j in 0..2 {
        if b[j] > 5 {
            letter = b[j] as i64;
        }
    }
    let mut y: i64 = 0;
    y = b[0] as i64;
    println(f\"{letter} {y}\");
}
";
    let (ok, err, out) = check_and_run("open", src);
    assert!(ok, "{err}");
    assert_eq!(out, "7 3\n");
    assert_eq!(err.matches("unnecessary cast").count(), 1, "{err}");
    assert!(err.contains(":10:"), "{err}");
}
