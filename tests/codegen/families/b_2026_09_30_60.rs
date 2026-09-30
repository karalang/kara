//! B-2026-09-30-60 -- a discarded `Array[..]` literal runs each element's
//! `Drop` body and frees its heap.

use super::*;

/// B-2026-09-30-60 — `let _ = Array[W1 { .. }];` printed `end` on every
/// compiled surface against `--interp`'s `dW1_7 end`: the literal lowers to
/// an `[N x T]` value, which the `Vec`-literal free, the aggregate registrar
/// and the handle-only bodies walk all decline. Covers `let _ =`, the bare
/// statement, an `if` / `match` arm yielding one, a block, a loop, and
/// heap-only (`P`, `String`) and scalar elements.
#[test]
fn e2e_discarded_fixed_array_literal_runs_bodies() {
    let src = r#"struct W1 { v: i64, s: String }
impl Drop for W1 { fn drop(mut ref self) { println(f"dW1_{self.v}") } }
struct P { s: String }
fn mk(n: i64) -> W1 { return W1 { v: n, s: f"ssssssssssssssssssssssssssssss{n}" } }
fn run(c: bool) {
    let _ = Array[W1 { v: 1, s: f"a{1}" }];
    println("s1");
    Array[mk(2), mk(3)];
    println("s2");
    let _ = if c { Array[mk(4)] } else { Array[mk(5)] };
    println("s3");
    match c { true => Array[mk(6)], false => Array[mk(7)] };
    println("s4");
    let _ = { Array[mk(8)] };
    println("s5");
    let _ = Array[P { s: f"p{1}" }, P { s: f"p{2}" }];
    let _ = Array[f"x{1}", f"y{2}"];
    let _ = Array[1, 2];
    println("s6");
}
fn main() {
    run(true);
    run(false);
    let mut i = 0;
    while i < 2 { let _ = Array[mk(10 + i)]; Array[mk(20 + i)]; i = i + 1; }
    println("end")
}
"#;
    let want = "dW1_1\ns1\ndW1_2\ndW1_3\ns2\ndW1_4\ns3\ndW1_6\ns4\ndW1_8\ns5\ns6\ndW1_1\ns1\ndW1_2\ndW1_3\ns2\ndW1_5\ns3\ndW1_7\ns4\ndW1_8\ns5\ns6\ndW1_10\ndW1_20\ndW1_11\ndW1_21\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
