//! Rust's `sort_unstable`, `sort_unstable_by` and `sort_unstable_by_key` on a
//! `Vec` are refused by name with the fix that drops `_unstable`, since the
//! library has only the stable sorts and a stable sort is also an unstable
//! one (kata 1051). Each is refused once, with no cascade from its closure,
//! and `karac fix` carries the program to a clean check.

use std::process::{Command, Stdio};

#[test]
fn karac_fix_renames_rust_unstable_sorts_to_the_stable_ones() {
    let path =
        std::env::temp_dir().join(format!("karac-unstable-sort-{}.kara", std::process::id()));
    std::fs::write(
        &path,
        "struct P { a: i64, b: i64 }
fn main() {
    let mut v = [3, 1, 2];
    v.sort_unstable();
    let mut w = [(2, 1), (1, 9), (1, 2)];
    w.sort_unstable_by(|x, y| x.0.cmp(y.0));
    let mut ps = [P { a: 2, b: 0 }, P { a: 1, b: 1 }];
    ps.sort_unstable_by_key(|p| p.a);
    println(f\"{v} {w} {ps[0].a}\");
}
",
    )
    .unwrap();
    let file = path.to_str().unwrap();
    let karac = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_karac"))
            .args(args)
            .stdin(Stdio::null())
            .output()
            .expect("karac")
    };
    let check = karac(&["check", file]);
    let err = String::from_utf8_lossy(&check.stderr).into_owned();
    for want in [
        ":4:7: no method 'sort_unstable' on type 'Vec[i64]'",
        ":6:7: no method 'sort_unstable_by' on type 'Vec[(i64, i64)]'",
        ":8:8: no method 'sort_unstable_by_key' on type 'Vec[P]'",
    ] {
        assert!(err.contains(want), "missing {want:?} in:\n{err}");
    }
    assert_eq!(err.matches("error[").count(), 3, "{err}");
    let fix = karac(&["fix", file]);
    assert!(
        fix.status.success(),
        "{}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let text = std::fs::read_to_string(&path).unwrap();
    let clean = karac(&["check", file]);
    let run = karac(&["__mir-run", file]);
    let _ = std::fs::remove_file(&path);
    assert!(!text.contains("unstable"), "{text}");
    assert!(
        clean.status.success(),
        "{}\n{text}",
        String::from_utf8_lossy(&clean.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "[1, 2, 3] [(1, 9), (1, 2), (2, 1)] 1\n",
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}
