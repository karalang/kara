//! `partition_point` on a slice and `rposition` on a `Vec` or slice
//! iterator, as Rust has them, and a `Vec` receiver finding the library's
//! `Slice[T]` methods (`v.partition_point(f)`, `v.clone_from_slice(w)`),
//! as collections.md says it does (kata 1053, whose arms search the rising tail
//! with `partition_point` and find the last fall with
//! `windows(2).iter().rposition(..)`). Both are library source
//! (runtime/stdlib/source/collections.kara), the same on every backend; the
//! kata's corpus entry runs them compiled.

use std::process::{Command, Stdio};

const PROGRAM: &str = "fn main() {
    let v: Vec[i64] = [1, 2, 2, 4, 7, 9];
    let below = v.partition_point(|x| x < 4);
    let at_most = v[1..].partition_point(|x| x <= 2);
    let all = v.partition_point(|x| x < 100);
    let none = v.partition_point(|x| x < 0);
    let empty: Vec[i64] = [];
    println(f\"{below} {at_most} {all} {none} {empty.partition_point(|x| x < 1)}\");
    let w: Vec[i64] = [5, 1, 4, 3, 6, 2];
    let fall = w.windows(2).iter().rposition(|p| p[0] > p[1]);
    let odd = w.iter().rposition(|x| x % 2 == 1);
    let big = w.iter().rposition(|x| x > 10);
    let mut it = w.iter();
    it.next();
    let from_second = it.rposition(|x| x == 1);
    let tail = w.as_slice()[2..].iter().rposition(|x| x > 3);
    println(f\"{fall} {odd} {big} {from_second} {tail}\");
    let mut c: Vec[i64] = [0, 0];
    c.clone_from_slice(w[1..3]);
    println(f\"{c}\");
}
";

const WANT: &str = "3 2 6 0 0\nSome(4) Some(3) None Some(0) Some(2)\n[1, 4]\n";

#[test]
fn partition_point_and_rposition_search_as_rust_does() {
    let path = std::env::temp_dir().join(format!("karac-ppoint-{}.kara", std::process::id()));
    std::fs::write(&path, PROGRAM).unwrap();
    let file = path.to_str().unwrap();
    let karac = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_karac"))
            .args(args)
            .stdin(Stdio::null())
            .output()
            .expect("karac")
    };
    let check = karac(&["check", file]);
    let run = karac(&["__mir-run", file]);
    let _ = std::fs::remove_file(&path);
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        WANT,
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}
