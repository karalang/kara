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
fn asan_transfer_struct_param_let_vec_leaf_runs_elem_drops_once() {
    assert_clean_asan_run_min_allocs(
        r#"struct G[T] { v: T, n: i64 }
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
}"#,
        &[
            "d1", "d2", "10", "2", "d3", "d4", "20", "1", "d5", "30", "d6", "d7", "40", "31",
            "ds8 31", "50", "ds9 31", "ds10 32", "60", "2", "d11", "d12", "70", "end",
        ],
        "asan_transfer_struct_param_let_vec_leaf_runs_elem_drops_once",
        6,
    );
}
