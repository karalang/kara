//! The `karac check` gaps kata 1054 (Distant Barcodes) found writing its
//! arms the way Rust writes them:
//!
//! - `std::iter::repeat(v).take(c)` was fixed to `repeat(v)`, which named
//!   nothing: the library now has `repeat` and `once` (library/iterators.md,
//!   Sources), and a program's own function of either name keeps it;
//! - `*counts.entry(b).or_default() += 1` was "no method 'or_default'" with
//!   no fix; it now draws one that writes the default out, `or_insert(0)`
//!   for a number.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn write(tag: &str, src: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("karac-k1054-{tag}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    path
}

/// `check` succeeds and `__mir-run` prints `want`.
fn accepted(tag: &str, src: &str, want: &str) {
    let path = write(tag, src);
    let file = path.to_str().unwrap();
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
        want,
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}

/// `check` reports `message`, and after `karac fix` the file holds every
/// one of `fixed`, checks clean and `__mir-run` prints `want`.
fn fixed(tag: &str, src: &str, message: &str, fixed: &[&str], want: &str) {
    let path = write(tag, src);
    let file = path.to_str().unwrap();
    let check = karac(&["check", file]);
    let err = String::from_utf8_lossy(&check.stderr).into_owned();
    for _ in 0..3 {
        karac(&["fix", file]);
    }
    let text = std::fs::read_to_string(&path).unwrap();
    let clean = karac(&["check", file]);
    let run = karac(&["__mir-run", file]);
    let _ = std::fs::remove_file(&path);
    assert!(err.contains(message), "missing {message:?} in:\n{err}");
    for f in fixed {
        assert!(text.contains(f), "missing {f:?} in:\n{text}");
    }
    assert!(
        clean.status.success(),
        "{}\n{text}",
        String::from_utf8_lossy(&clean.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        want,
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn repeat_and_once_start_a_chain() {
    accepted(
        "repeat-once",
        "fn main() {
    let groups: Vec[(i64, i64)] = [(3, 2), (1, 1), (2, 3)];
    let order: Vec[i64] = groups.iter().flat_map(|(v, c)| repeat(v).take(c)).collect();
    let words: Vec[String] = once(\"x\".to_string()).chain(repeat(\"y\".to_string()).take(2)).collect();
    println(f\"{order} {words} {once(4).count()}\");
}
",
        "[3, 3, 1, 2, 2, 2] [x, y, y] 1\n",
    );
}

#[test]
fn a_program_s_own_repeat_or_once_keeps_the_name() {
    accepted(
        "own-repeat",
        "fn repeat(n: i64, f: own MutFn()) {
    let mut g = f;
    for _ in 0..n {
        g();
    }
}

fn once(x: i64) -> i64 {
    x + 1
}

fn main() {
    let mut k = 0;
    repeat(3, || { k += 2; });
    println(f\"{k} {once(4)}\");
}
",
        "6 5\n",
    );
}

#[test]
fn karac_fix_takes_std_iter_repeat_to_the_library_s() {
    fixed(
        "std-repeat",
        "fn main() {
    let v: Vec[i64] = std::iter::repeat(7).take(3).collect();
    println(f\"{v}\");
}
",
        "`std::iter::repeat` is a Rust path",
        &["repeat(7).take(3)"],
        "[7, 7, 7]\n",
    );
}

#[test]
fn karac_fix_writes_out_the_default_an_entry_s_or_default_puts_in() {
    fixed(
        "or-default",
        "fn main() {
    let mut counts: Map[i64, i64] = Map.new();
    let mut groups: SortedMap[i64, Vec[i64]] = SortedMap.new();
    let mut names: Map[i64, String] = Map.new();
    for b in [3, 1, 3] {
        *counts.entry(b).or_default() += 1;
        groups.entry(b % 2).or_default().push(b);
        names.entry(b).or_default().push_str(\"ab\");
    }
    println(f\"{counts[3]} {groups} {names[1]}\");
}
",
        "Kāra's `Entry` puts in the value it is given (library/collections.md), so `or_insert(0)` is `or_default()`",
        &[
            "counts.entry(b).or_insert(0)",
            "groups.entry(b % 2).or_insert(Vec.new())",
            "names.entry(b).or_insert(String.new())",
        ],
        "2 {1: [3, 1, 3]} ab\n",
    );
}
