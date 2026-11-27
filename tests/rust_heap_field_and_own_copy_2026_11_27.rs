//! Two `karac fix` rewrites of Rust that changed what the program does.
//!
//! - A Rust `BinaryHeap<..>` struct field filled by `.collect()` in a struct
//!   literal is written `PriorityQueue.max_first_from(..collect())`, as a
//!   `let`-typed collect is, so it keeps Rust's largest-first pop order
//!   (kata 1845, B-2026-11-27-3).
//! - `let mut sorted = costs; sorted.sort();` with `costs` a bare parameter
//!   takes `costs` `own`, as the same-name `let mut s = s;` does, rather
//!   than `mut ref` with `mut` at every call, which sorted the caller's list
//!   (kata 1833, B-2026-11-27-5).

mod common;

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static CALL: AtomicUsize = AtomicUsize::new(0);

fn file(src: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-heap-field-own-copy-{}-{}",
        std::process::id(),
        CALL.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    (dir, f)
}

fn karac(args: &[&str], f: &std::path::Path) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(f)
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

/// `karac check`'s verdict and output.
fn check(src: &str) -> (bool, String) {
    let (dir, f) = file(src);
    let (ok, out, err) = karac(&["check"], &f);
    let _ = std::fs::remove_dir_all(&dir);
    (ok, format!("{out}{err}"))
}

/// The file `karac fix` writes for `src`.
fn fixed(src: &str) -> String {
    let (dir, f) = file(src);
    let _ = karac(&["fix"], &f);
    let out = std::fs::read_to_string(&f).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    out
}

fn runs_everywhere(src: &str, want: &str) {
    let (ok, out) = check(src);
    assert!(ok, "check:\n{out}\n{src}");
    let mut lanes = vec!["__mir-run"];
    if cfg!(feature = "llvm") {
        lanes.extend(["__mir-llvm-run", "__mir-llvm-asan"]);
    }
    for cmd in lanes {
        let (dir, f) = file(src);
        let (ok, out, err) = karac(&[cmd], &f);
        let _ = std::fs::remove_dir_all(&dir);
        if cmd != "__mir-run"
            && common::link_or_skip(if ok { Ok(()) } else { Err(err.clone()) }).is_none()
        {
            continue;
        }
        assert!(ok, "{cmd}: {err}\n{src}");
        assert_eq!(out, want, "{cmd}\n{src}");
    }
}

const HEAP: &str = "use std::collections::BinaryHeap;
use std::cmp::Reverse;

struct Top {
    heap: BinaryHeap<i32>,
}

struct Seats {
    heap: BinaryHeap<Reverse<i32>>,
}

fn main() {
    let t = Top { heap: vec![1, 5, 3].into_iter().collect() };
    let t2 = Top { heap: (0..4).map(|x| x * 2).collect() };
    let s = Seats { heap: (1..=5).map(Reverse).collect() };
    println!(\"{}\", t.heap.peek().unwrap());
    println!(\"{}\", t2.heap.peek().unwrap());
    println!(\"{}\", s.heap.peek().unwrap().0);
}
";

#[test]
fn a_rust_heap_field_filled_by_collect_keeps_its_pop_order() {
    let after = fixed(HEAP);
    assert!(
        after.contains("Top { heap: PriorityQueue.max_first_from([1, 5, 3].into_iter().collect()) }")
            && after.contains("Top { heap: PriorityQueue.max_first_from((0..4).map(|x| x * 2).collect()) }")
            && after.contains("Seats { heap: PriorityQueue.max_first_from((1..=5).map(|x| Reverse { value: x }).collect()) }"),
        "{after}"
    );
    // Rust prints 5, 6 and 1.
    runs_everywhere(&after, "5\n6\n1\n");
}

const ICE: &str = "fn max_ice_cream(costs: Vec[i64], coins: i64) -> i64 {
    let mut sorted = costs;
    sorted.sort();
    let mut left = coins;
    let mut n = 0;
    for c in sorted.iter() {
        if *c > left { break; }
        left -= *c;
        n += 1;
    }
    n
}

fn main() {
    println(max_ice_cream([1, 3, 2, 4, 1], 7));
    let costs = [10, 6, 8, 7, 7, 8];
    println(max_ice_cream(costs.clone(), 5));
    println(costs[0]);
}
";

#[test]
fn a_mut_binding_of_a_bare_parameter_under_another_name_takes_it_own() {
    let (ok, out) = check(ICE);
    assert!(
        !ok && out.contains("E0517")
            && out.contains("Rust's `let mut sorted = costs;` takes an owned argument to change: declare it `costs: own ...`"),
        "{out}"
    );
    let after = fixed(ICE);
    assert!(
        after.contains("fn max_ice_cream(costs: own Vec[i64], coins: i64) -> i64 {"),
        "{after}"
    );
    assert!(!after.contains("mut ["), "{after}");
    runs_everywhere(&after, "4\n0\n10\n");
}
