//! When an open literal is fixed must not depend on the order of its uses
//! (design.md §8 Literals: a literal takes its default only at the end of
//! the body, after every use has had its chance to fix it).
//!
//! - B-2026-11-14-1: a comparison beside a type that widens into the
//!   default (`i < n` with `n: i32`) fixes nothing, and no longer settles
//!   the literal where it stands, so a later argument still fixes it.
//! - B-2026-11-12-12: a literal stored into an element of an open fill
//!   (`t[1] = 4` after `Vec.filled(3, 0)`) joins the fill's literal instead
//!   of settling it, so a later `let r: i32 = t[1]` still fixes it.
//! - B-2026-11-12-2: a value read out of a tuple a literal was pushed into
//!   (`graph[a].push((b, 0))`, then `for (next, color) in graph[n]`) and
//!   narrowed into an `i32` names that literal and offers its suffix,
//!   which `karac fix` applies.

use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

fn run(args: &[&str], src: &str) -> (Output, String) {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("karac-lit-order-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(&file)
        .current_dir(&dir)
        .output()
        .unwrap();
    let after = std::fs::read_to_string(&file).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (out, after)
}

fn text(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string() + &String::from_utf8_lossy(&o.stdout)
}

/// Each use order of B-2026-11-14-1 and B-2026-11-12-12, accepted, with
/// what it prints.
const ORDERS: &[(&str, &str, &str)] = &[
    (
        "compare-then-argument",
        "fn take(x: i32) -> i32 { x + 1 }\n\
         fn main() {\n    let n: i32 = 3;\n    let i = 0;\n    \
         if i < n { println(f\"{take(i)}\"); }\n}\n",
        "1\n",
    ),
    (
        "argument-then-compare",
        "fn take(x: i32) -> i32 { x + 1 }\n\
         fn main() {\n    let n: i32 = 3;\n    let i = 0;\n    \
         println(f\"{take(i)}\");\n    if i < n { println(\"lt\"); }\n}\n",
        "1\nlt\n",
    ),
    (
        "compare-only-keeps-default",
        "fn main() {\n    let n: i32 = 3;\n    let i = 0;\n    \
         if i < n { println(\"lt\"); }\n    let w: i64 = i;\n    println(w);\n}\n",
        "lt\n0\n",
    ),
    (
        "kata-1533-loop",
        "fn probe(x: i32) -> i32 { x * 2 }\n\
         fn main() {\n    let n: i32 = 4;\n    let mut i = 0;\n    \
         while i < n {\n        if probe(i) > 3 { break; }\n        i += 1;\n    }\n    \
         println(f\"{probe(i)} {i}\");\n}\n",
        "4 2\n",
    ),
    (
        "fill-store-then-read",
        "fn main() {\n    let mut t = Vec.filled(3, 0);\n    t[1] = 4;\n    \
         let r: i32 = t[1];\n    println(r);\n}\n",
        "4\n",
    ),
    (
        "fill-read-then-store",
        "fn main() {\n    let mut t = Vec.filled(3, 0);\n    let r: i32 = t[1];\n    \
         t[1] = 4;\n    println(f\"{r} {t[1]}\");\n}\n",
        "0 4\n",
    ),
    (
        "fill-store-keeps-default",
        "fn main() {\n    let mut t = Vec.filled(3, 0);\n    t[1] = 4;\n    \
         let w: i64 = t[1] + t[0];\n    println(w);\n}\n",
        "4\n",
    ),
];

fn orders_run(mode: &str) {
    for (name, src, want) in ORDERS {
        let (out, _) = run(&[mode], src);
        assert!(out.status.success(), "{name} under {mode}: {}", text(&out));
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            *want,
            "{name} under {mode}"
        );
    }
}

#[test]
fn literal_use_orders_are_accepted_under_check() {
    for (name, src, _) in ORDERS {
        let (out, _) = run(&["check"], src);
        assert!(out.status.success(), "{name}: {}", text(&out));
    }
}

#[test]
fn interp_literal_use_orders() {
    orders_run("__mir-run");
}

#[cfg(feature = "llvm")]
#[test]
fn compiled_literal_use_orders() {
    orders_run("__mir-llvm-run");
}

/// A comparison that left the literal open is still judged for mixed
/// signedness, at the type the literal ended with; a store that does not
/// fit the type a later read fixes is out of range there.
#[test]
fn deferred_literal_uses_are_still_judged() {
    let (out, _) = run(
        &["check"],
        "fn main() {\n    let n: i32 = 3;\n    let i = 0;\n    \
         if i < n { println(\"lt\"); }\n    let z: u32 = i;\n    println(z);\n}\n",
    );
    let t = text(&out);
    assert!(!out.status.success(), "{t}");
    assert!(t.contains("cannot compare 'u32' and 'i32'"), "{t}");
    let (out, _) = run(
        &["check"],
        "fn main() {\n    let mut t = Vec.filled(3, 0);\n    t[1] = 4000000000;\n    \
         let r: i32 = t[1];\n    println(r);\n}\n",
    );
    let t = text(&out);
    assert!(!out.status.success(), "{t}");
    assert!(
        t.contains("integer literal 4000000000 out of range for 'i32'"),
        "{t}"
    );
}

const PUSHED: &str = "fn main() {\n    \
    let edges: Vec[(i64, i64)] = [(0, 1), (1, 2)];\n    \
    let mut graph = Vec.filled(3, Vec.new());\n    \
    for (a, b) in edges.iter() {\n        graph[a].push((b, 0));\n        graph[b].push((a, 1));\n    }\n    \
    let mut q: Vec[(i64, i32)] = Vec.new();\n    \
    let seen = [false, false];\n    \
    for (next, color) in ref graph[1] {\n        if not seen[color] {\n            q.push((next, color));\n        }\n    }\n    \
    println(f\"{q.len()} {q[0].1} {q[1].1}\");\n}\n";

#[test]
fn pushed_tuple_literal_is_named_and_fixed() {
    let (out, _) = run(&["check"], PUSHED);
    let t = text(&out);
    assert!(!out.status.success(), "{t}");
    assert!(
        t.contains("`color` is an 'i64' because the literal it is bound to took the default")
            && t.contains("write that literal `0i32`"),
        "{t}"
    );
    let (out, fixed) = run(&["fix"], PUSHED);
    assert!(out.status.success(), "{}", text(&out));
    assert!(fixed.contains("graph[a].push((b, 0i32));"), "{fixed}");
    let (out, _) = run(&["__mir-run"], &fixed);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "2 1 0\n");
}
