//! Rust's `sort_by_cached_key` on a `Vec` is refused by name with the fix
//! that renames it to `sort_by_key`: both are the same stable sort by a key,
//! and how often the key is called is unspecified (docs/library/collections.md).
//! It is refused once, with no cascade from its closure, and `karac fix`
//! carries the program to a clean check whose run keeps ties in order
//! (kata 2191).

use std::process::{Command, Stdio};

#[test]
fn kata_2191_karac_fix_renames_sort_by_cached_key() {
    let path = std::env::temp_dir().join(format!(
        "karac-sort-by-cached-key-{}.kara",
        std::process::id()
    ));
    std::fs::write(
        &path,
        "fn digits(n: i64) -> i64 {
    n % 10
}
fn main() {
    let mut v = [21, 13, 31, 2, 11];
    v.sort_by_cached_key(|x| digits(x));
    println(f\"{v}\");
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
    let want = ":6:7: no method 'sort_by_cached_key' on type 'Vec[i64]'";
    assert!(err.contains(want), "missing {want:?} in:\n{err}");
    assert_eq!(err.matches("error[").count(), 1, "{err}");
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
    assert!(text.contains("v.sort_by_key(|x| digits(x));"), "{text}");
    assert!(
        clean.status.success(),
        "{}\n{text}",
        String::from_utf8_lossy(&clean.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "[21, 31, 11, 2, 13]\n",
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}
