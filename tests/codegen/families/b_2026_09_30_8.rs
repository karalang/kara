//! B-2026-09-30-8 — a `match` / `if let` struct pattern over a CONCRETE
//! instantiation of a generic struct (`match g { G { v, n } => .. }` with
//! `g: G[String]`, a param, a local or a fresh temp) moved the `String` leaf
//! out without zeroing the source's cap, because the cleanup suppressor read
//! the field's declared type `T`: the leaf's free and the monomorph's struct
//! drop both freed it, `free(): double free detected in tcache 2` at -O0.

use super::*;

/// B-2026-09-30-8 — leaves bound, handed back and moved out of `G[String]`,
/// `G2[String]` and `G2[Vec[i64]]` from params, fresh temps, an unannotated
/// and an annotated local and a temp scrutinee, plus a generic fn's leaf
/// typed by its own `T` (which owns nothing and must not be zeroed).
#[test]
fn e2e_generic_struct_match_leaf_moves_out_once() {
    let src = r#"struct G[T] { v: T, n: i64 }
struct G2[T] { v: T, s: String }
fn mk(p: String, k: i64) -> String {
    f"{p}-heap-string-longer-than-sso-{k}"
}
fn a1(g: G[String]) -> i64 { match g { G { v, n } => v.len() + n } }
fn a2(g: G2[String]) -> i64 { match g { G2 { s, .. } => s.len() } }
fn a3(g: G2[String]) -> i64 { match g { G2 { v, s } => v.len() + s.len() } }
fn a4(g: G2[Vec[i64]]) -> i64 { if let G2 { v, s } = g { v.len() + s.len() } else { 0 } }
fn a5(g: G[String]) -> String { match g { G { v, .. } => v } }
fn a6[T](g: G2[T]) -> i64 { match g { G2 { s, .. } => s.len() } }
fn a7[T](g: G[T]) -> i64 { match g { G { v, n } => { let w = v; n } } }
fn a8() -> i64 { let g = G { v: mk("l", 8), n: 2 }; match g { G { v, n } => v.len() + n } }
fn main() {
    println(f"{a1(G { v: mk("a", 1), n: 2 })}");
    let x1 = G { v: mk("a", 1), n: 2 };
    println(f"{a1(x1)}");
    println(f"{a2(G2 { v: mk("a", 2), s: mk("b", 2) })} {a3(G2 { v: mk("a", 3), s: mk("b", 3) })}");
    let mut w = Vec.new(); w.push(1); w.push(2);
    println(f"{a4(G2 { v: w, s: mk("b", 4) })}");
    println(a5(G { v: mk("a", 5), n: 0 }));
    println(f"{a6(G2 { v: mk("a", 6), s: mk("b", 6) })} {a7(G { v: mk("a", 7), n: 7 })} {a8()}");
    let g: G[String] = G { v: mk("m", 9), n: 1 };
    match g { G { v, n } => println(f"{v.len() + n}") }
    let k = match G { v: mk("t", 10), n: 1 } { G { v, n } => v.len() + n };
    println(f"{k}");
}
"#;
    let want = "33\n33\n31 62\n33\na-heap-string-longer-than-sso-5\n31 7 33\n32\n33\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    if let Some(aot) = run_program(src) {
        assert_eq!(aot, want, "AOT");
    }
}
