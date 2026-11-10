//! A tuple literal's numbers are literals: they stay open until a use
//! fixes them, through a destructuring `let` and through a push into a
//! `PriorityQueue` (whose `Ord` bound is checked later), as a bare literal
//! does (design.md §8 Literals). `let t = (0, 0); let (d, u) = t;
//! take_i32(d)` and kata 2093's Dijkstra (`heap.push((0, 0))`, then `return
//! d` in an `i32` function) were refused as narrowing an `i64`
//! (B-2026-11-10-40). Each pair below has the form that checks and one
//! where an earlier use fixed the number as `i64`, which stays refused.

use std::process::{Command, Output, Stdio};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn temp(tag: &str, src: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "karac-literal-tuple-open-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    path
}

/// The default executor (the JIT on an llvm build, the interpreter
/// otherwise) and the interpreter.
const LANES: [&[&str]; 2] = [&["run"], &["run", "--interp"]];

fn runs(tag: &str, src: &str, expected: &str) {
    let path = temp(tag, src);
    for lane in LANES {
        let out = karac(lane, &path);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            expected,
            "{tag} {lane:?}; stderr:\n{stderr}"
        );
        assert!(out.status.success(), "{tag} {lane:?}; stderr:\n{stderr}");
    }
    let _ = std::fs::remove_file(&path);
}

fn narrows(tag: &str, src: &str) {
    let path = temp(tag, src);
    let out = karac(&["check"], &path);
    let _ = std::fs::remove_file(&path);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success() && stderr.contains("from 'i64' to 'i32' would narrow"),
        "{tag}: {stderr}"
    );
}

fn in_i32_fn(body: &str) -> String {
    format!("fn take(n: i32) -> i32 {{\n    n\n}}\n\nfn f() -> i32 {{\n    {body}\n}}\n\nfn main() {{\n    println(f());\n}}\n")
}

#[test]
fn a_tuple_literal_bound_by_let_is_fixed_by_a_later_use() {
    runs(
        "let",
        &in_i32_fn("let t = (7, (2, 3));\n    let (d, u) = t;\n    take(d) + take((u.1))"),
        "10\n",
    );
    narrows(
        "let-fixed",
        &in_i32_fn("let t = (0, 0);\n    let big: i64 = 1;\n    let s = t.0 + big;\n    take(t.0)"),
    );
}

#[test]
fn a_tuple_literal_pushed_into_a_heap_is_fixed_by_a_later_use() {
    runs(
        "heap",
        &in_i32_fn(
            "let mut h = PriorityQueue.new();\n    h.push((4, 0));\n    let (d, u) = h.pop().unwrap();\n    take(d)",
        ),
        "4\n",
    );
    narrows(
        "heap-fixed",
        &in_i32_fn(
            "let mut h = PriorityQueue.new();\n    let big: i64 = 1;\n    h.push((big, 0));\n    take(h.pop().unwrap().0)",
        ),
    );
}

#[test]
fn kata_2093_dijkstra_returns_the_popped_distance_as_i32() {
    runs("dijkstra", KATA_2093, "15\n");
}

const KATA_2093: &str = r#"fn shortest(dist: mut ref Vec[i32], toll: i32) -> i32 {
    let mut heap = PriorityQueue.new();
    heap.push((0, 0));
    while let Some((d, u)) = heap.pop() {
        if u >= 3 {
            return d;
        }
        let nd = d + toll;
        dist[u + 1] = nd;
        heap.push((nd, u + 1));
    }
    -1
}

fn main() {
    let mut dist = Vec.filled(4, 0i32);
    println(shortest(mut dist, 5));
}
"#;
