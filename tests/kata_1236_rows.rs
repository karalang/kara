//! Two rows from kata 1236 (Web Crawler).
//!
//! - B-2026-11-19-36: a closure in an assignment's method chain is over by
//!   the statement's end, as one under a `let` is, so a later move of what
//!   it reads is no move inside a loop.
//! - B-2026-11-19-35: Rust's array `map` on a list literal gets the `.into_iter()`
//!   hop as a fix, and the follow-up fixes collect it into the binding's
//!   type.

use std::process::{Command, Output, Stdio};

fn tmp(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("karac-kata-1236-{tag}-{}.kara", std::process::id()))
}

fn karac(sub: &str, path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(sub)
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

#[test]
fn kata_1236_assignment_chain_closure_ends_with_the_statement() {
    let src = "fn main() {
    let seen: Set[i64] = Set.from([1]);
    let mut n = 0;
    n = [1, 2].iter().filter(|v| seen.contains(v)).count();
    let all: Vec[i64] = seen.into_iter().collect();
    let mut m = 0;
    m += [1, 3].iter().filter(|v| all.contains(v)).count();
    let rest: Vec[i64] = all.into_iter().collect();
    println(f\"{n} {m} {rest.len()}\");
}
";
    let path = tmp("assign");
    std::fs::write(&path, src).expect("write");
    let check = karac("check", &path);
    let run = karac("__mir-run", &path);
    let _ = std::fs::remove_file(&path);
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "1 1 1\n");
}

#[test]
fn kata_1236_list_literal_map_is_fixed_to_a_collected_walk() {
    let src = "fn total(v: Vec[String]) -> i64 { v.len() }
fn main() {
    let urls = [\"http://a\", \"http://b\"].map(|u| u.to_string());
    println(f\"{total(urls)}\");
}
";
    let path = tmp("arrmap");
    std::fs::write(&path, src).expect("write");
    for _ in 0..3 {
        let _ = karac("fix", &path);
    }
    let fixed = std::fs::read_to_string(&path).expect("read");
    let run = karac("__mir-run", &path);
    let _ = std::fs::remove_file(&path);
    assert!(
        fixed.contains(
            "let urls: Vec[String] = [\"http://a\", \"http://b\"].into_iter().map(|u| u.to_string()).collect();"
        ),
        "{fixed}"
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "2\n");
}
