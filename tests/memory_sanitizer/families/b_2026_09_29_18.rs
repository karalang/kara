//! B-2026-09-29-18 — a projection off a local VIEW of a by-value param
//! (`let O { w, k } = o;` or `let w = o.w;`, then `xs.push(w.r)`) runs the
//! pushed part's `Drop` body once, and the kept path runs it over the live value.

use super::*;

/// B-2026-09-29-18 — the remainder of B-2026-09-29-10. With `w` a view of the
/// param's field, `xs.push(w.r)` ran `r`'s body again when the container
/// died, on all four surfaces, conditionally or not; and once the conditional
/// push was reported, the compiled kept path ran the body over the param's
/// moved-out field (`dD1` with no name). Neighbours: a local container, an
/// else arm that reads or pushes a sibling, a tuple view, an early return, and
/// a by-value `self` receiver's view.
#[test]
fn asan_param_view_projection_pushed_into_container_frees_once() {
    assert_clean_asan_run_min_allocs(
        r#"struct D { id: i64, name: String }
impl Drop for D { fn drop(mut ref self) { println(f"dD{self.id}{self.name}") } }
fn mkd(n: i64) -> D { return D { id: n, name: f"n{n}" }; }
struct W { r: D, s: D, b: i64 }
fn mkw(n: i64) -> W { W { r: mkd(n), s: mkd(n + 100), b: n } }
struct O { w: W, k: i64 }
fn mko(n: i64) -> O { O { w: mkw(n), k: n } }
impl O {
    fn take(self, xs: mut ref Vec[D], c: bool) { let w = self.w; if c { xs.push(w.r); } }
}
fn v5(o: O, c: bool) -> i64 { let O { w, k } = o; let mut ys: Vec[D] = Vec.new(); if c { ys.push(w.r); } ys.len() }
fn v6(xs: mut ref Vec[D], o: O, c: bool) { let w = o.w; if c { xs.push(w.r); } else { println(f"k{w.r.id}"); } }
fn v7(xs: mut ref Vec[D], o: O, c: bool) { let w = o.w; if c { xs.push(w.r); } else { xs.push(w.s); } }
fn v8(xs: mut ref Vec[D], t: (W, i64), c: bool) { let (w, k) = t; if c { xs.push(w.r); } }
fn v9(xs: mut ref Vec[D], o: O, c: bool) { let O { w, k } = o; if c { xs.push(w.r); return; } println("kept"); }
fn u1(xs: mut ref Vec[D], o: O, c: bool) { let O { w, k } = o; if c { xs.push(w.r); } }
fn u2(xs: mut ref Vec[D], o: O) { let O { w, k } = o; xs.push(w.r); }
fn u3(xs: mut ref Vec[D], o: O, c: bool) { let w = o.w; if c { xs.push(w.r); } }
fn u4(xs: mut ref Vec[D], o: O) { let w = o.w; xs.push(w.r); }
fn main() {
    let mut ds: Vec[D] = Vec.new();
    println(f"{v5(mko(1), false)}"); println(f"{v5(mko(2), true)}");
    v6(mut ds, mko(3), false); println("b1"); v6(mut ds, mko(4), true); println("b2");
    v7(mut ds, mko(5), false); println("c1"); v7(mut ds, mko(6), true); println("c2");
    v8(mut ds, (mkw(7), 0), false); println("d1"); v8(mut ds, (mkw(8), 0), true); println("d2");
    v9(mut ds, mko(9), false); println("e1"); v9(mut ds, mko(10), true); println("e2");
    mko(11).take(mut ds, false); println("f1"); mko(12).take(mut ds, true); println("f2");
    u1(mut ds, mko(13), false); println("g1"); u1(mut ds, mko(14), true); println("g2");
    u2(mut ds, mko(15)); println("h1");
    u3(mut ds, mko(16), false); println("i1"); u3(mut ds, mko(17), true); println("i2");
    u4(mut ds, mko(18)); println("j1");
    println(f"end{ds.len()}");
}"#,
        &[
            "dD1n1",
            "dD101n101",
            "0",
            "dD2n2",
            "dD102n102",
            "1",
            "k3",
            "dD3n3",
            "dD103n103",
            "b1",
            "dD104n104",
            "b2",
            "dD5n5",
            "c1",
            "dD106n106",
            "c2",
            "dD7n7",
            "dD107n107",
            "d1",
            "dD108n108",
            "d2",
            "kept",
            "dD9n9",
            "dD109n109",
            "e1",
            "dD110n110",
            "e2",
            "dD11n11",
            "dD111n111",
            "f1",
            "dD112n112",
            "f2",
            "dD13n13",
            "dD113n113",
            "g1",
            "dD114n114",
            "g2",
            "dD115n115",
            "h1",
            "dD16n16",
            "dD116n116",
            "i1",
            "dD117n117",
            "i2",
            "dD118n118",
            "j1",
            "end10",
            "dD4n4",
            "dD105n105",
            "dD6n6",
            "dD8n8",
            "dD10n10",
            "dD12n12",
            "dD14n14",
            "dD15n15",
            "dD17n17",
            "dD18n18",
        ],
        "asan_param_view_projection_pushed_into_container_frees_once",
        40,
    );
}
