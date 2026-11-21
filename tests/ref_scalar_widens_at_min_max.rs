//! B-2026-11-21-66: a `ref` to a narrower integer reads as its value and
//! widens at a numeric method's argument, as at a function's argument and
//! at an operator. `mx.max(f)` with `mx` an `i64` (its literal kept the
//! default) and `f: ref i32` from `for (i, f) in flips.iter().enumerate()`
//! was refused "`max` expects an argument of type `i64`, got `ref i32`",
//! with no fix, while `take_i64(f)` and `mx + f` were accepted. Kata
//! 1375's Rust arm, after `karac fix` took the `&` from `(i, &f)`.

use std::path::PathBuf;
use std::process::Command;

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

fn fixture(tag: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("karac-ref-widen-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("p.kara");
    std::fs::write(&path, src).unwrap();
    (dir, path)
}

const PREFIX_ALIGNED: &str = "fn num_times_all_blue(flips: Vec[i32]) -> i32 {
    let mut mx = 0;
    let mut ans = 0;
    for (i, f) in flips.iter().enumerate() {
        mx = mx.max(f);
        if mx == i as i32 + 1 {
            ans += 1;
        }
    }
    ans
}

fn main() {
    println(f\"{num_times_all_blue([3, 2, 4, 1, 5])}\");
    println(f\"{num_times_all_blue([4, 1, 2, 3])}\");
}
";

const MIN_AND_MAX: &str = r#"fn main() {
    let v: Vec[i32] = [3, -7, 2];
    let mut hi: i64 = 0;
    let mut lo: i64 = 0;
    for x in v.iter() {
        hi = hi.max(x);
        lo = lo.min(x);
    }
    let w: Vec[u8] = [200, 9];
    let mut top: u32 = 0;
    for b in w.iter() {
        top = top.max(b);
    }
    println(f"{hi} {lo} {top}");
}
"#;

fn check_and_run(tag: &str, src: &str) -> (bool, String, String) {
    let (dir, path) = fixture(tag, src);
    let check = karac().arg("check").arg(&path).output().unwrap();
    let run = karac().arg("__mir-run").arg(&path).output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        check.status.success(),
        String::from_utf8_lossy(&check.stderr).into_owned()
            + &String::from_utf8_lossy(&check.stdout),
        String::from_utf8_lossy(&run.stdout).into_owned(),
    )
}

#[test]
fn a_ref_to_a_narrower_int_widens_at_max() {
    let (clean, diag, out) = check_and_run("max", PREFIX_ALIGNED);
    assert!(clean, "{diag}");
    assert_eq!(out, "2\n1\n");
}

#[test]
fn a_ref_to_a_narrower_int_widens_at_min_and_unsigned_max() {
    let (clean, diag, out) = check_and_run("minmax", MIN_AND_MAX);
    assert!(clean, "{diag}");
    assert_eq!(out, "3 -7 200\n");
}

/// A `ref` to a wider integer still narrows nothing: refused.
#[test]
fn a_ref_to_a_wider_int_is_still_refused_at_max() {
    let src = "fn main() {
    let v: Vec[i64] = [3, 2];
    let mut mx: i32 = 0;
    for x in v.iter() {
        mx = mx.max(x);
    }
    println(f\"{mx}\");
}
";
    let (clean, diag, _) = check_and_run("wider", src);
    assert!(!clean);
    assert!(
        diag.contains("`max` expects an argument of type `i32`"),
        "{diag}"
    );
}

#[cfg(feature = "llvm")]
#[test]
fn a_ref_to_a_narrower_int_widens_at_max_compiled() {
    for (tag, src, want) in [
        ("max-run", PREFIX_ALIGNED, "2\n1\n"),
        ("minmax-run", MIN_AND_MAX, "3 -7 200\n"),
    ] {
        let (dir, path) = fixture(tag, src);
        let run = karac().arg("run").arg(&path).output().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(String::from_utf8_lossy(&run.stdout), want, "{tag}");
    }
}
