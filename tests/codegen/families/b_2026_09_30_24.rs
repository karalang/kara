//! B-2026-09-30-24 — a by-value STRUCT param the callee holds by TRANSFER
//! (a generic struct such as `G[R]`, whose erased `T` field fails the entry
//! copy) was treated as a caller-held view when a `match` destructured it, so
//! the leaf ran its `Drop` body nowhere (the arm's mask had taken the field out
//! of the param's own walk), and a heap-bearing leaf (`G[Rs]`) was freed by
//! the leaf and again by the param's drop.

use super::*;

/// B-2026-09-30-24 — `match g { G { v, n } => .. }` over a by-transfer
/// `G[R]` / `G[Rs]` / `G[E]` / `G[Vec[Rs]]` param, in a generic and a concrete
/// callee, with the leaf unused, read, rebound, handed to a by-value callee,
/// and alive on a path that returns early, plus a call-result argument.
#[test]
fn e2e_transfer_struct_param_match_leaf_runs_its_drop_once() {
    let src = r#"
struct G[T] { v: T, n: i64 }
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct Rs { id: i64, s: String }
impl Drop for Rs { fn drop(mut ref self) { println(f"ds{self.id} {self.s.len()}") } }
enum E { A(R), B }
fn mk(k: i64) -> String { f"a-heap-string-longer-than-sso-{k}" }
fn eat(r: R) -> i64 { r.id }
fn a1[T](g: G[T]) -> i64 { match g { G { v, n } => { let w = v; n } } }
fn a2(g: G[R]) -> i64 { match g { G { v, n } => n } }
fn a3(g: G[R]) -> i64 { match g { G { v, n } => { println(f"r{v.id}"); n } } }
fn a4(g: G[Rs]) -> i64 { match g { G { v, n } => n } }
fn a5(g: G[R]) -> i64 { match g { G { v, n } => eat(v) + n } }
fn a6(g: G[Rs]) -> i64 { match g { G { v, n } => { if n > 0 { return n } v.id } } }
fn a7(g: G[E]) -> i64 { match g { G { v, n } => { let w = v; n } } }
fn a8(g: G[Vec[Rs]]) -> i64 { match g { G { v, n } => { let w = v; n } } }
fn mkg(k: i64) -> G[R] { G { v: R { id: k }, n: 1 } }
fn main() {
    println(a1(G { v: R { id: 1 }, n: 1 }));
    println(a1(G { v: Rs { id: 2, s: mk(2) }, n: 1 }));
    println(a2(G { v: R { id: 3 }, n: 1 }));
    println(a3(G { v: R { id: 4 }, n: 1 }));
    println(a4(G { v: Rs { id: 5, s: mk(5) }, n: 1 }));
    println(a5(G { v: R { id: 6 }, n: 1 }));
    println(a6(G { v: Rs { id: 7, s: mk(7) }, n: 1 }));
    println(a6(G { v: Rs { id: 8, s: mk(8) }, n: 0 }));
    println(a7(G { v: E.A(R { id: 9 }), n: 1 }));
    println(a8(G { v: [Rs { id: 10, s: mk(10) }, Rs { id: 11, s: mk(11) }], n: 1 }));
    println(a2(mkg(12)));
    println("end");
}"#;
    let want = "d1\n1\nds2 31\n1\nd3\n1\nr4\nd4\n1\nds5 31\n1\nd6\n7\nds7 31\n1\nds8 31\n8\nd9\n1\nds10 32\nds11 32\n1\nd12\n1\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    if let Some(aot) = run_program(src) {
        assert_eq!(aot, want, "AOT");
    }
}
