//! Kata 2512's `binary_search` with an owned `String` argument, asked of
//! two sorted lists in turn: the search only compares its argument, so
//! the argument stays usable after it (B-2026-11-23-65).

use std::path::PathBuf;
use std::process::Command;

fn program(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-k2512-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    f
}

fn karac(args: &[&str], f: &PathBuf) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(f)
        .output()
        .expect("karac");
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

const TWO_LISTS: &str = "fn kind(good: Vec[String], bad: Vec[String], w: String) -> i64 {
    if good.binary_search(w).is_some() {
        3
    } else if bad.binary_search(w).is_some() {
        -1
    } else {
        0
    }
}

fn main() {
    let good: Vec[String] = [\"a\".to_string(), \"b\".to_string()].to_vec();
    let bad: Vec[String] = [\"c\".to_string()].to_vec();
    let w = \"c\".to_string();
    let first = good.binary_search(w);
    let again = good.as_slice().binary_search(w);
    println(f\"{kind(good, bad, w)} {first.is_some()} {again.is_some()} {w}\");
}
";

#[test]
fn binary_search_borrows_an_owned_string_argument() {
    let f = program("two_lists", TWO_LISTS);
    let (ok, out) = karac(&["check"], &f);
    assert!(ok, "check refused the search's argument: {out}");
    let (ok, out) = karac(&["run", "--interp"], &f);
    assert!(ok, "run: {out}");
    assert_eq!(out, "-1 false false c\n");
}
