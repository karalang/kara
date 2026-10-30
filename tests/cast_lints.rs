//! design.md § `as` casts: `redundant_cast` flags an `as` to the type its
//! operand already has (`true as bool` included, which was refused), and
//! `lossless_cast` flags a lossless widening where a binding, argument or
//! return wants the target type, so the conversion happens implicitly. Both
//! fixes delete the `as`; `karac fix` applies them and the program prints
//! the same. A cast that changes the binding's type, an integer to `f64`
//! and a narrowing are left alone (B-2026-10-30-14).

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

#[test]
fn karac_fix_deletes_an_unneeded_cast() {
    let src = "struct P { n: i32 }
fn wide(x: i64) -> i64 { x * 2 }
fn back(b: u8) -> i64 {
    return b as i64;
}
fn main() {
    let x: i64 = 5;
    let a = x as i64;
    let p = P { n: 7 };
    let b: i64 = p.n as i64;
    let t = true;
    let c = t as bool;
    let s = \"abc\".to_string();
    let w = wide(s.len() as i64);
    let f: f32 = 1.5;
    let g: f64 = f as f64;
    let kept = p.n as i64;
    let k: f64 = p.n as f64;
    let narrow = x as i32;
    println(f\"{a} {b} {c} {w} {g} {back(200)} {kept} {k} {narrow}\");
}
";
    let path = std::env::temp_dir().join(format!("karac-cast-lints-{}.kara", std::process::id()));
    let file = path.to_str().unwrap();
    std::fs::write(&path, src).unwrap();

    let check = karac(&["check", file]);
    let err = String::from_utf8_lossy(&check.stderr);
    assert!(check.status.success(), "{err}");
    for want in [
        ":4:12: unnecessary cast: 'u8' widens to 'i64' implicitly here",
        ":8:13: redundant cast: the value is already 'i64'",
        ":10:18: unnecessary cast: 'i32' widens to 'i64' implicitly here",
        ":12:13: redundant cast: the value is already 'bool'",
        ":14:18: redundant cast: the value is already 'i64'",
        ":16:18: unnecessary cast: 'f32' widens to 'f64' implicitly here",
    ] {
        assert!(err.contains(want), "missing {want:?} in:\n{err}");
    }
    assert_eq!(err.matches("_cast]").count(), 6, "{err}");
    let json = karac(&["check", "--output=json", file]);
    let json = String::from_utf8_lossy(&json.stdout);
    assert_eq!(json.matches("\"W0291\"").count(), 6, "{json}");

    let fix = karac(&["fix", file]);
    assert!(
        fix.status.success(),
        "{}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let out = std::fs::read_to_string(&path).unwrap();
    for want in [
        "return b;",
        "let a = x;",
        "let b: i64 = p.n;",
        "let c = t;",
        "wide(s.len())",
        "let g: f64 = f;",
        "let kept = p.n as i64;",
        "let k: f64 = p.n as f64;",
        "let narrow = x as i32;",
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
        (Some(0), "5 7 true 6 1.5 200 7 7.0 5\n"),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}
