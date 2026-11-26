//! B-2026-11-26-78: `let w: (Vec[i32], i64) = ([1, 2].to_vec(), 3)` was
//! refused, "expected '(Vec[i32], i64)', found '(Vec[i64], i64)'", with
//! advice to annotate the binding that already was, though `let v:
//! Vec[i32] = [1, 2].to_vec()` takes `i32` (B-2026-11-24-43). A tuple
//! literal checked against its slots now hands a `[..].to_vec()` element
//! its slot when the slot holds a narrow number, as it already did when
//! the slot held a `Str` (kata 1423's differential).

use std::path::PathBuf;
use std::process::Command;

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

fn fixture(tag: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-to-vec-in-tuple-{tag}-{}-{}",
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

/// `karac check`'s success and output, and `karac __mir-run`'s stdout.
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

/// Kata 1423's fixed cases: each a list of card points and a `k`, the
/// list written `[..].to_vec()` inside a tuple, one level down in a typed
/// `let` and two levels down in an outer `[..].to_vec()`.
#[test]
fn to_vec_in_a_tuple_takes_its_slots_narrow_number() {
    let src = "fn total(v: Vec[i32]) -> i32 {
    let mut s: i32 = 0;
    for x in v {
        s += x;
    }
    s
}

fn main() {
    let w: (Vec[i32], i64) = ([1, 2, 3].to_vec(), 3);
    let cases: Vec[(Vec[i32], i64)] = [
        ([1, 2, 3, 4, 5, 6, 1].to_vec(), 3),
        ([9, 7, 7, 9, 7, 7, 9].to_vec(), 7),
    ].to_vec();
    let bytes: (Vec[u8], Vec[f32]) = ([250, 5].to_vec(), [0.5, 1.5].to_vec());
    let mut sum: i32 = total(w.0.clone());
    for (v, k) in cases {
        sum += total(v) + k as i32;
    }
    println(f\"{sum} {bytes.0[0] + bytes.0[1]} {bytes.1[0] + bytes.1[1]}\");
}
";
    let (ok, out, stdout) = check_and_run("narrow", src);
    assert!(ok, "expected a clean check, got:\n{out}");
    assert_eq!(stdout, "93 255 2.0\n");
}

/// A value of another type in the list is still refused, at the value.
#[test]
fn to_vec_in_a_tuple_still_refuses_a_wrong_element() {
    let src = "fn main() {
    let w: (Vec[i32], i64) = ([1, \"a\"].to_vec(), 3);
    println(f\"{w.1}\");
}
";
    let (ok, out, _) = check_and_run("wrong", src);
    assert!(!ok, "expected a refusal, got a clean check:\n{out}");
}
