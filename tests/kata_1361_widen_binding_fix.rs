//! B-2026-11-25-20: `at = parent[at]`, with `let mut at = start;` an `i32`
//! from `for start in 0..n` and `parent: Vec[i64]`, was refused with only
//! the advice to truncate (`as i32`). The narrow side is the binding,
//! typed by its first value, and widening it keeps every value: check now
//! names the binding, and `karac fix` declares it `let mut at: i64 =
//! start;`. Kata 1361's climbing arm.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

#[test]
fn kata_1361_assigning_a_wider_value_widens_the_binding() {
    let src = "fn main() {
    let parent: Vec[i64] = [-1, 0, 0, 1];
    let n: i32 = 4;
    let mut steps = 0;
    for start in 0..n {
        let mut at = start;
        while at != -1 {
            steps += 1;
            at = parent[at];
        }
    }
    println(f\"{steps}\");
}
";
    let path =
        std::env::temp_dir().join(format!("karac-kata-1361-widen-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    let file = path.to_str().unwrap();
    let check = karac(&["check", file]);
    let err = String::from_utf8_lossy(&check.stderr).into_owned();
    karac(&["fix", file]);
    let text = std::fs::read_to_string(&path).unwrap();
    let after = karac(&["check", file]);
    let run = karac(&["__mir-run", file]);
    let _ = std::fs::remove_file(&path);
    assert!(!check.status.success(), "{err}");
    assert!(
        err.contains("declare it `at: i64` where it is bound"),
        "{err}"
    );
    assert!(text.contains("let mut at: i64 = start;"), "{text}");
    assert!(
        after.status.success(),
        "{}",
        String::from_utf8_lossy(&after.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "8\n");
}

/// Kata 1377's climbing arm: `parent` is a fill, `Vec.filled(n + 1, 0)`,
/// whose literal an `i64` stored into it (`parent[u] = u / 2`) settled.
/// The fix offered was the literal's suffix, `0i32`, which then refused
/// that store with no fix of its own; a fill that holds a stored `i64` is
/// fixed by widening the binding instead.
#[test]
fn kata_1377_a_fill_holding_a_stored_i64_widens_the_binding() {
    let src = "fn main() {
    let order: Vec[i64] = [1, 2, 3, 4, 5];
    let n: i32 = 5;
    let mut parent = Vec.filled(n + 1, 0);
    for u in order.iter() {
        parent[u] = u / 2;
    }
    let mut steps = 0;
    for target in 1..n + 1 {
        let mut at = target;
        while at != 1 {
            at = parent[at];
            steps += 1;
        }
    }
    println(f\"{steps}\");
}
";
    let path =
        std::env::temp_dir().join(format!("karac-kata-1377-widen-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    let file = path.to_str().unwrap();
    let check = karac(&["check", file]);
    let err = String::from_utf8_lossy(&check.stderr).into_owned();
    karac(&["fix", file]);
    let text = std::fs::read_to_string(&path).unwrap();
    let after = karac(&["check", file]);
    let run = karac(&["__mir-run", file]);
    let _ = std::fs::remove_file(&path);
    assert!(!check.status.success(), "{err}");
    assert!(
        err.contains("declare it `at: i64` where it is bound"),
        "{err}"
    );
    assert!(!err.contains("0i32"), "{err}");
    assert!(text.contains("let mut at: i64 = target;"), "{text}");
    assert!(text.contains("Vec.filled(n + 1, 0);"), "{text}");
    assert!(
        after.status.success(),
        "{}",
        String::from_utf8_lossy(&after.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "6\n");
}
