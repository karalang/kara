//! `v.drain(a..b);` as a statement, as Rust writes it to take a range out,
//! draws no `must_use` warning: `drain` removes the range at the call, so the
//! `VecIntoIter` it returns has nothing left to do when it is dropped (kata
//! 1047's scan arm). An iterator from `v.iter()` or `v.into_iter()` still
//! warns, and the range is removed either way.

use std::process::{Command, Stdio};

#[test]
fn a_discarded_vec_drain_is_quiet_and_removes_the_range() {
    let path = std::env::temp_dir().join(format!(
        "karac-must-use-vec-drain-{}.kara",
        std::process::id()
    ));
    std::fs::write(
        &path,
        "fn main() {
    let mut v = [1, 2, 3, 4, 5, 6];
    v.drain(1..3);
    v.iter();
    v.drain(0..1);
    println(f\"{v.len()} {v[0]} {v[2]}\");
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
    assert!(err.contains(":4:5: discarded `VecIter"), "{err}");
    assert!(!err.contains(":3:5:") && !err.contains(":5:5:"), "{err}");
    assert_eq!(err.matches("discarded `").count(), 1, "{err}");
    let run = karac(&["__mir-run", file]);
    let _ = std::fs::remove_file(&path);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "3 4 6\n",
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}
