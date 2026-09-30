//! B-2026-09-30-42 — a by-value struct param held by TRANSFER (`G[R]`, whose
//! erased `T` field fails the entry copy) lost a field's `Drop` body when the
//! field was moved out by projection (`let w = g.v`): the move masked the
//! field out of the param's own walk, and the destination stood down as if the
//! caller still ran it. A destructure on ONE branch (`if c { match g { G { v,
//! n } => n } } else { 0 }`, and the `let` spelling) masked the walk
//! statically, so the path that never destructured lost the body instead.

use super::*;

/// B-2026-09-30-42 — projection moves out of a by-transfer `G[Rs]` param
/// (read, handed to a by-value callee, returned) and branch-local `match` /
/// `let` destructures of `G[R]`, `G[Rs]` and `G[Vec[R]]`, each branch taken.
#[test]
fn e2e_transfer_struct_param_field_move_and_branch_destructure_run_drop_once() {
    let src = r#"
struct G[T] { v: T, n: i64 }
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct Rs { id: i64, s: String }
impl Drop for Rs { fn drop(mut ref self) { println(f"ds{self.id} {self.s.len()}") } }
fn mk(k: i64) -> String { f"a-heap-string-longer-than-sso-{k}" }
fn eat(r: Rs) -> i64 { r.id }
fn giveg(g: G[Rs]) -> i64 { g.n }
fn a1(g: G[Rs]) -> i64 { let w = g.v; println(w.s.len()); g.n }
fn a2(g: G[Rs]) -> i64 { let w = g.v; eat(w) + g.n }
fn a3(g: G[Rs]) -> Rs { let w = g.v; w }
fn a4(g: G[R], c: bool) -> i64 { if c { match g { G { v, n } => n } } else { 0 } }
fn a5(g: G[Rs], c: bool) -> i64 { if c { let G { v, n } = g; n } else { 0 } }
fn a6(g: G[Vec[R]], c: bool) -> i64 { if c { let G { v, n } = g; println(v.len()); n } else { 0 } }
fn a7(g: G[Rs], c: bool) -> i64 { if c { match g { G { v, n } => eat(v) + n } } else { giveg(g) } }
fn main() {
    println(a1(G { v: Rs { id: 1, s: mk(1) }, n: 10 }));
    println(a2(G { v: Rs { id: 2, s: mk(2) }, n: 20 }));
    let r = a3(G { v: Rs { id: 3, s: mk(3) }, n: 30 });
    println(r.id);
    println(a4(G { v: R { id: 4 }, n: 40 }, true));
    println(a4(G { v: R { id: 5 }, n: 50 }, false));
    println(a5(G { v: Rs { id: 6, s: mk(6) }, n: 60 }, true));
    println(a5(G { v: Rs { id: 7, s: mk(7) }, n: 70 }, false));
    println(a6(G { v: [R { id: 8 }, R { id: 9 }], n: 80 }, true));
    println(a6(G { v: [R { id: 10 }], n: 90 }, false));
    println(a7(G { v: Rs { id: 11, s: mk(11) }, n: 100 }, true));
    println(a7(G { v: Rs { id: 12, s: mk(12) }, n: 110 }, false));
    println("end");
}"#;
    let want = "31\nds1 31\n10\nds2 31\n22\n3\nds3 31\nd4\n40\nd5\n0\nds6 31\n60\nds7 31\n0\n2\nd8\nd9\n80\nd10\n0\nds11 32\n111\nds12 32\n110\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    if let Some(aot) = run_program(src) {
        assert_eq!(aot, want, "AOT");
    }
}
