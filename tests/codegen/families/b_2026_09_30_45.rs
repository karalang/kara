//! B-2026-09-30-45 — a `let` that destructures a struct source into a `Vec`
//! leaf handed the leaf the buffer but not the elements' `Drop` bodies, which
//! stayed on the source's walk. On a by-value param held by TRANSFER
//! (`G[Vec[R]]`) that walk runs at the callee's exit, after the leaf had freed
//! the buffer (garbage ids, an invalid read); on a local source it runs at the
//! statement, before the leaf's own reads.

use super::*;

/// B-2026-09-30-45 — `let G { v, n } = g;` over a by-transfer `G[Vec[R]]` /
/// `G[Vec[Rs]]` param, concrete and generic, with the leaf unused, read,
/// indexed and rebound, plus the local-source spelling.
#[test]
fn e2e_transfer_struct_param_let_vec_leaf_runs_elem_drops_once() {
    let src = r#"
struct G[T] { v: T, n: i64 }
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct Rs { id: i64, s: String }
impl Drop for Rs { fn drop(mut ref self) { println(f"ds{self.id} {self.s.len()}") } }
fn mk(k: i64) -> String { f"a-heap-string-longer-than-sso-{k}" }
fn a1(g: G[Vec[R]]) -> i64 { let G { v, n } = g; n }
fn a2(g: G[Vec[R]]) -> i64 { let G { v, n } = g; println(v.len()); n }
fn a3(g: G[Vec[R]]) -> i64 { let G { v, n } = g; let w = v; println(w.len()); n }
fn a4[T](g: G[T]) -> i64 { let G { v, n } = g; n }
fn a5(g: G[Vec[Rs]]) -> i64 { let G { v, n } = g; println(v[0].s.len()); n }
fn a6(g: G[Vec[Rs]]) -> i64 { let G { v, n } = g; n }
fn main() {
    println(a1(G { v: [R { id: 1 }, R { id: 2 }], n: 10 }));
    println(a2(G { v: [R { id: 3 }, R { id: 4 }], n: 20 }));
    println(a3(G { v: [R { id: 5 }], n: 30 }));
    println(a4(G { v: [R { id: 6 }, R { id: 7 }], n: 40 }));
    println(a5(G { v: [Rs { id: 8, s: mk(8) }], n: 50 }));
    println(a6(G { v: [Rs { id: 9, s: mk(9) }, Rs { id: 10, s: mk(10) }], n: 60 }));
    let g = G { v: [R { id: 11 }, R { id: 12 }], n: 70 };
    let G { v, n } = g;
    println(v.len());
    println(n);
    println("end");
}"#;
    let want = "d1\nd2\n10\n2\nd3\nd4\n20\n1\nd5\n30\nd6\nd7\n40\n31\nds8 31\n50\nds9 31\nds10 32\n60\n2\nd11\nd12\n70\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    if let Some(aot) = run_program(src) {
        assert_eq!(aot, want, "AOT");
    }
}
