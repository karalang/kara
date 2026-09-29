//! B-2026-09-29-10 — a PROJECTION of a by-value param pushed straight into a
//! `mut ref` container (`xs.push(w.r)`, `xs.push(t.0)`) runs that part's
//! `Drop` body once.

use super::*;

/// B-2026-09-29-10 — `fn f(xs: mut ref Vec[D], w: W, c: bool) { if c {
/// xs.push(w.r); } }` ran `r`'s body twice on the kept path (once in the
/// container, once as the rest of the param dropped), where the bound-local
/// spelling `let r = w.r; if c { xs.push(r); }` was already right: the part
/// scan only noted an IDENTIFIER argument as a hand-over, so the projection
/// was never masked out of the caller's drop. Neighbours: the unconditional
/// push, a local container, an else arm that only reads the part, a two-level
/// projection, tuple elements, and a by-value `self` receiver.
#[test]
fn asan_param_projection_pushed_into_container_frees_once() {
    assert_clean_asan_run_min_allocs(
        r#"struct D { id: i64, name: String }
impl Drop for D { fn drop(mut ref self) { println(f"dD{self.id}{self.name}") } }
fn mkd(n: i64) -> D { return D { id: n, name: f"n{n}" }; }
struct W { r: D, s: D, b: i64 }
fn mkw(n: i64) -> W { W { r: mkd(n), s: mkd(n + 100), b: n } }
struct O { w: W, k: i64 }
fn mko(n: i64) -> O { O { w: mkw(n), k: n } }
impl W {
    fn give(self, xs: mut ref Vec[D], c: bool) { if c { xs.push(self.r); } }
    fn give_all(self, xs: mut ref Vec[D]) { xs.push(self.s); }
}
fn f5(xs: mut ref Vec[D], w: W, c: bool) { if c { xs.push(w.r); } }
fn f6(w: W, c: bool) -> i64 { let mut xs: Vec[D] = Vec.new(); if c { xs.push(w.r); } xs.len() }
fn f7(xs: mut ref Vec[D], w: W) { xs.push(w.r); }
fn f8(xs: mut ref Vec[D], w: W, c: bool) { if c { xs.push(w.r); } else { println(f"k{w.r.id}"); } }
fn g5(xs: mut ref Vec[D], o: O, c: bool) { if c { xs.push(o.w.r); } }
fn a5(xs: mut ref Vec[D], t: (D, i64), c: bool) { if c { xs.push(t.0); } }
fn a7(xs: mut ref Vec[D], t: (D, D)) { xs.push(t.1); }
fn main() {
    let mut ds: Vec[D] = Vec.new();
    f5(mut ds, mkw(1), false); println("a1"); f5(mut ds, mkw(2), true); println("a2");
    println(f"{f6(mkw(3), false)}"); println(f"{f6(mkw(4), true)}");
    f7(mut ds, mkw(5)); println("b1");
    f8(mut ds, mkw(6), false); println("c1"); f8(mut ds, mkw(7), true); println("c2");
    g5(mut ds, mko(8), false); println("d1"); g5(mut ds, mko(9), true); println("d2");
    a5(mut ds, (mkd(10), 0), false); println("e1"); a5(mut ds, (mkd(11), 0), true); println("e2");
    a7(mut ds, (mkd(12), mkd(13))); println("f1");
    mkw(14).give(mut ds, false); println("g1"); mkw(15).give(mut ds, true); println("g2");
    mkw(16).give_all(mut ds); println("h1");
    println(f"end{ds.len()}");
}"#,
        &[
            "dD1n1",
            "dD101n101",
            "a1",
            "dD102n102",
            "a2",
            "dD3n3",
            "dD103n103",
            "0",
            "dD4n4",
            "dD104n104",
            "1",
            "dD105n105",
            "b1",
            "k6",
            "dD6n6",
            "dD106n106",
            "c1",
            "dD107n107",
            "c2",
            "dD8n8",
            "dD108n108",
            "d1",
            "dD109n109",
            "d2",
            "dD10n10",
            "e1",
            "e2",
            "dD12n12",
            "f1",
            "dD14n14",
            "dD114n114",
            "g1",
            "dD115n115",
            "g2",
            "dD16n16",
            "h1",
            "end8",
            "dD2n2",
            "dD5n5",
            "dD7n7",
            "dD9n9",
            "dD11n11",
            "dD13n13",
            "dD15n15",
            "dD116n116",
        ],
        "asan_param_projection_pushed_into_container_frees_once",
        40,
    );
}
