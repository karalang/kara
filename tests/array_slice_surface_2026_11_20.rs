//! B-2026-11-21-2: a fixed `Array[T, N]` has the read surface of a `Slice`
//! (collections.md § Array). The checker answered `first` / `get` / `last`
//! on a scalar `Array` with an owned `Option[T]`, so an `Option[ref i64]`
//! slot refused them, and it refused `is_sorted` on an `Array[String, N]`
//! and on an array of tuples. Each now types as on the slice.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Runs `karac run main.kara` on `src`; returns (success, stdout, stderr).
fn run(src: &str) -> (bool, String, String) {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("karac-arrsurf-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.kara"), src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["__mir-run", "main.kara"])
        .current_dir(&dir)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn array_first_get_last_lend_the_element() {
    let (ok, out, err) = run(r#"fn show(o: Option[ref i64]) -> i64 { o ?? -1 }
fn main() {
    let a: Array[i64, 4] = [4, 3, 2, 1];
    let x: Option[ref i64] = a.first();
    println(f"{show(a.first())} {show(a.get(1))} {show(a.last())} {show(a.get(9))} {x.unwrap()}");
}
"#);
    assert!(ok, "{err}");
    assert_eq!(out, "4 3 1 -1 4\n");
}

#[test]
fn array_is_sorted_over_strings_and_tuples() {
    let (ok, out, err) = run(r#"fn main() {
    let s: Array[String, 2] = ["x".to_string(), "y".to_string()];
    let t: Array[(i64, i64), 2] = [(1, 2), (0, 1)];
    println(f"{s.is_sorted()} {t.is_sorted()} {t.first().unwrap().0} {t.get(1).unwrap().1} {t.last().unwrap().0} {t.contains((0, 1))}");
}
"#);
    assert!(ok, "{err}");
    assert_eq!(out, "true false 1 1 0 true\n");
}
