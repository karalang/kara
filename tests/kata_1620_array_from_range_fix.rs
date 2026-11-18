//! Found writing kata 1620's ★ arm (B-2026-11-18-52, the remainder of
//! B-2026-11-18-51).
//!
//! `let mut best = [0, 0]` filled by `best = [x, y]` from `for x in
//! 0..=50`, returned as `best.to_vec()` from a `Vec[i32]` function, was
//! refused with no machine fix: the `.to_vec()` call hid the binding from
//! the literal-suffix fix. Now `karac fix` suffixes the array's literal
//! (`[0i32, 0]`), which makes the assignment's narrowing offer the range's
//! (`0i32..=50`), and the program runs.

use std::process::{Command, Output, Stdio};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

/// `karac check`'s stderr, the file once `karac fix` ran, a second fix's
/// stdout, and what the fixed program prints on each backend.
fn check_fix_run(tag: &str, src: &str) -> (String, String, String, Vec<String>) {
    let path = std::env::temp_dir().join(format!(
        "karac-kata-1620-array-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    let check = karac(&["check"], &path);
    let err = String::from_utf8_lossy(&check.stderr).into_owned();
    let _ = karac(&["fix"], &path);
    let fixed = std::fs::read_to_string(&path).unwrap();
    let again = karac(&["fix"], &path);
    let again = String::from_utf8_lossy(&again.stdout).into_owned()
        + &String::from_utf8_lossy(&again.stderr);
    let backends: &[&str] = if cfg!(feature = "llvm") {
        &["__mir-run", "__mir-llvm-run"]
    } else {
        &["__mir-run"]
    };
    let outs = backends
        .iter()
        .map(|b| {
            let run = karac(&[b], &path);
            String::from_utf8_lossy(&run.stdout).into_owned()
                + &String::from_utf8_lossy(&run.stderr)
        })
        .collect();
    let _ = std::fs::remove_file(&path);
    (err, fixed, again, outs)
}

#[test]
fn an_array_filled_from_a_range_and_returned_by_to_vec_is_fixed() {
    let src = "fn best(t: Slice[i32]) -> Vec[i32] {
    let mut best = [0, 0];
    let mut most = 0;
    for x in 0..=3 {
        for y in 0..=3 {
            let quality = (t[0] - x) * (t[1] - y);
            if quality > most {
                most = quality;
                best = [x, y];
            }
        }
    }
    best.to_vec()
}

fn main() {
    println(best([5, 7]));
    println(best([1, 1]));
}
";
    let (err, fixed, again, outs) = check_fix_run("to-vec", src);
    assert!(
        err.contains("the elements of `best` are 'i64'") && err.contains("`0i32`"),
        "{err}"
    );
    assert!(
        fixed.contains("let mut best = [0i32, 0];")
            && fixed.contains("for x in 0i32..=3 {")
            && fixed.contains("for y in 0i32..=3 {"),
        "{fixed}"
    );
    assert!(!again.contains("applied"), "{again}");
    for out in outs {
        assert_eq!(out, "[0, 0]\n[3, 3]\n");
    }
}

#[test]
fn a_cloned_literal_array_is_fixed_on_its_literal() {
    let src = "fn pair(t: Slice[i32]) -> Vec[i32] {
    let mut out = [0, 0];
    let k = 3;
    if t[0] > 2 {
        out = [k + 1, k];
    }
    out.clone()
}

fn main() {
    println(pair([5]));
}
";
    let (err, fixed, again, outs) = check_fix_run("clone", src);
    assert!(err.contains("the elements of `out` are 'i64'"), "{err}");
    assert!(fixed.contains("let mut out = [0i32, 0];"), "{fixed}");
    assert!(!again.contains("applied"), "{again}");
    for out in outs {
        assert_eq!(out, "[4, 3]\n");
    }
}
