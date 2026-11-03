//! B-2026-11-03-12 — design.md § Mixed-width operands widens the narrower
//! operand of a binary operator within its kind, so `u as i64 + w` (with
//! `w: i64`) casts what would widen anyway, and `lossless_cast` flags it.
//! A cast whose sibling is an unsuffixed literal is what typed that literal:
//! `old as i64 - 97` is not `old - 97` (the latter runs in `u8` and can
//! underflow), so it is left alone, as are a narrowing, a cast to a type the
//! other operand does not have, and a pair of casts.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

#[test]
fn karac_fix_deletes_an_operand_cast_the_widening_makes_redundant() {
    let src = "fn main() {
    let old: u8 = 40;
    let h: i64 = 5;
    let f: f32 = 0.5;
    let x: f64 = 2.0;
    let a = h * 131 + old as i64;
    let b = old as i64 | h;
    let c = x + f as f64;
    let d = old as i64 - 97;
    let e = old as i64 * if h > 2 { 1 } else { 2 };
    let g = old as i64 - f as i64;
    let k = h as u8 + old;
    println(f\"{a} {b} {c} {d} {e} {g} {k}\");
}
";
    let path = std::env::temp_dir().join(format!(
        "karac-cast-lints-operands-{}.kara",
        std::process::id()
    ));
    let file = path.to_str().unwrap();
    std::fs::write(&path, src).unwrap();

    let check = karac(&["check", file]);
    let err = String::from_utf8_lossy(&check.stderr);
    assert!(check.status.success(), "{err}");
    for want in [
        ":6:23: unnecessary cast: 'u8' widens to 'i64' implicitly here",
        ":7:13: unnecessary cast: 'u8' widens to 'i64' implicitly here",
        ":8:17: unnecessary cast: 'f32' widens to 'f64' implicitly here",
    ] {
        assert!(err.contains(want), "missing {want:?} in:\n{err}");
    }
    assert_eq!(err.matches("_cast]").count(), 3, "{err}");

    let fix = karac(&["fix", file]);
    assert!(
        fix.status.success(),
        "{}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let out = std::fs::read_to_string(&path).unwrap();
    for want in [
        "let a = h * 131 + old;",
        "let b = old | h;",
        "let c = x + f;",
        "let d = old as i64 - 97;",
        "let e = old as i64 * if",
        "let g = old as i64 - f as i64;",
        "let k = h as u8 + old;",
    ] {
        assert!(out.contains(want), "missing {want:?} in:\n{out}");
    }
    let run = karac(&["__mir-run", file]);
    let recheck = karac(&["check", file]);
    let _ = std::fs::remove_file(&path);
    assert!(
        !String::from_utf8_lossy(&recheck.stderr).contains("_cast]"),
        "{}",
        String::from_utf8_lossy(&recheck.stderr)
    );
    assert_eq!(
        (
            run.status.code(),
            String::from_utf8_lossy(&run.stdout).as_ref()
        ),
        (Some(0), "695 45 2.5 -57 40 40 45\n"),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}
