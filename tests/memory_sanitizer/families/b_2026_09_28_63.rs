//! B-2026-09-28-63 — a NESTED struct destructure of a by-value param whose
//! leaf is moved out runs that leaf's `Drop` body once and frees it once.

use super::*;

/// B-2026-09-28-63 — `let O { w: W { r, s, b }, k } = o;` followed by
/// `xs.push(r)` (unconditionally or on one path), `return r`, or a local
/// container ran `r`'s body twice under `--interp` and aborted with
/// `free(): double free detected in tcache 2` compiled: the nested leaf was
/// not an alias of its part (`o.w.r`) in the part scan, and codegen left it
/// a bit-alias of the param's copy with no transfer. The projection spelling
/// `let W { r, s, b } = o.w;` was already right on all four surfaces.
/// Neighbours: a three-level pattern, a renamed leaf under `..` rests, a
/// read-only nested destructure.
#[test]
fn asan_nested_param_destructure_leaf_moved_out_frees_once() {
    assert_clean_asan_run_min_allocs(
        r#"struct D { id: i64, name: String }
impl Drop for D { fn drop(mut ref self) { println(f"dD{self.id}{self.name}") } }
fn mkd(n: i64) -> D { return D { id: n, name: f"n{n}" }; }
struct W { r: D, s: D, b: i64 }
fn mkw(n: i64) -> W { W { r: mkd(n), s: mkd(n + 100), b: n } }
struct O { w: W, k: i64 }
fn mko(n: i64) -> O { O { w: mkw(n), k: n } }
struct T { o: O, z: i64 }
fn n1(xs: mut ref Vec[D], o: O) { let O { w: W { r, s, b }, k } = o; xs.push(r); }
fn n2(xs: mut ref Vec[D], o: O, c: bool) { let O { w: W { r, s, b }, k } = o; if c { xs.push(r); } }
fn n5(o: O, c: bool) -> i64 { let mut xs: Vec[D] = Vec.new(); let O { w: W { r, s, b }, k } = o; if c { xs.push(r); } xs.len() }
fn n6(o: O) -> D { let O { w: W { r, s, b }, k } = o; println(f"in{s.id}"); r }
fn n7(o: O) -> i64 { let O { w: W { r, s, b }, k } = o; r.id + s.id }
fn n8(xs: mut ref Vec[D], t: T, c: bool) { let T { o: O { w: W { r, s, b }, k }, z } = t; if c { xs.push(r); } }
fn n9(xs: mut ref Vec[D], o: O, c: bool) { let O { w: W { r: q, .. }, .. } = o; if c { xs.push(q); } }
fn main() {
    let mut ds: Vec[D] = Vec.new();
    n1(mut ds, mko(1)); println("a");
    n2(mut ds, mko(2), false); println("b1"); n2(mut ds, mko(3), true); println("b2");
    println(f"{n5(mko(4), false)}"); println(f"{n5(mko(5), true)}");
    let d = n6(mko(6)); println(f"got{d.id}");
    println(f"{n7(mko(7))}");
    n8(mut ds, T { o: mko(8), z: 0 }, false); println("c1"); n8(mut ds, T { o: mko(9), z: 0 }, true); println("c2");
    n9(mut ds, mko(10), false); println("e1"); n9(mut ds, mko(11), true); println("e2");
    println(f"end{ds.len()}");
}"#,
        &[
            "dD101n101",
            "a",
            "dD2n2",
            "dD102n102",
            "b1",
            "dD103n103",
            "b2",
            "dD4n4",
            "dD104n104",
            "0",
            "dD5n5",
            "dD105n105",
            "1",
            "in106",
            "dD106n106",
            "got6",
            "dD6n6",
            "dD107n107",
            "dD7n7",
            "114",
            "dD8n8",
            "dD108n108",
            "c1",
            "dD109n109",
            "c2",
            "dD10n10",
            "dD110n110",
            "e1",
            "dD111n111",
            "e2",
            "end4",
            "dD1n1",
            "dD3n3",
            "dD9n9",
            "dD11n11",
        ],
        "asan_nested_param_destructure_leaf_moved_out_frees_once",
        30,
    );
}

/// B-2026-09-28-69 — the spellings -63 left on their older answer: a TUPLE
/// param's element (`let (r, k) = t;`, `let r = t.0;`, a local container,
/// two droppable elements, a nested `t.0.r`, an early `return`, an `else`
/// that reads it, a method's tuple param), a two-step destructure through a
/// local (`let O { w, k } = o; let W { r, .. } = w;` and `let w = o.w;`),
/// and an owned receiver's nested part (`let O { w: W { r, .. }, k } =
/// self;`, `let W { r, .. } = self.w;`), each pushed on only some paths.
/// The path that KEPT the part ran no body on every surface for the
/// destructures and receiver spellings, and compiled-only for `t.0` and a
/// local container: codegen adopted no tuple part, and neither backend
/// followed a part through a destructured local.
#[test]
fn asan_conditionally_pushed_tuple_and_view_parts_free_once() {
    assert_clean_asan_run_min_allocs(
        r#"struct D { id: i64, name: String }
impl Drop for D { fn drop(mut ref self) { println(f"dD{self.id}{self.name}") } }
fn mkd(n: i64) -> D { return D { id: n, name: f"n{n}" }; }
struct W { r: D, s: D, b: i64 }
fn mkw(n: i64) -> W { W { r: mkd(n), s: mkd(n + 100), b: n } }
struct O { w: W, k: i64 }
fn mko(n: i64) -> O { O { w: mkw(n), k: n } }
impl O {
    fn take(self, xs: mut ref Vec[D], c: bool) { let O { w: W { r, s, b }, k } = self; if c { xs.push(r); } }
    fn take2(self, xs: mut ref Vec[D], c: bool) { let W { r, s, b } = self.w; if c { xs.push(r); } }
}
struct H { z: i64 }
impl H { fn m(ref self, xs: mut ref Vec[D], t: (D, i64), c: bool) { let (r, k) = t; if c { xs.push(r); } } }
fn t2(xs: mut ref Vec[D], t: (D, i64), c: bool) { let (r, k) = t; if c { xs.push(r); } }
fn t3(xs: mut ref Vec[D], t: (D, i64), c: bool) { let r = t.0; if c { xs.push(r); } }
fn t4(t: (D, i64), c: bool) -> i64 { let mut xs: Vec[D] = Vec.new(); let (r, k) = t; if c { xs.push(r); } xs.len() }
fn t5(xs: mut ref Vec[D], t: (D, D), c: bool) { let (r, q) = t; if c { xs.push(r); } println("in"); }
fn t6(xs: mut ref Vec[D], t: (W, i64), c: bool) { let r = t.0.r; if c { xs.push(r); } }
fn t7(xs: mut ref Vec[D], t: (D, i64), c: bool) -> i64 { let (r, k) = t; if c { xs.push(r); return 1; } k }
fn t8(xs: mut ref Vec[D], t: (D, i64), c: bool) { let (r, k) = t; if c { xs.push(r); } else { println(f"k{k}{r.id}"); } }
fn n3(xs: mut ref Vec[D], o: O, c: bool) { let O { w, k } = o; let W { r, s, b } = w; if c { xs.push(r); } }
fn n4(xs: mut ref Vec[D], o: O, c: bool) { let w = o.w; let W { r, s, b } = w; if c { xs.push(r); } }
fn main() {
    let mut ds: Vec[D] = Vec.new();
    t2(mut ds, (mkd(1), 1), false); println("a1"); t2(mut ds, (mkd(2), 1), true); println("a2");
    t3(mut ds, (mkd(3), 1), false); println("b1"); t3(mut ds, (mkd(4), 1), true); println("b2");
    println(f"{t4((mkd(5), 1), false)}"); println(f"{t4((mkd(6), 1), true)}");
    t5(mut ds, (mkd(7), mkd(8)), false); println("c1"); t5(mut ds, (mkd(9), mkd(10)), true); println("c2");
    t6(mut ds, (mkw(11), 0), false); println("d1"); t6(mut ds, (mkw(12), 0), true); println("d2");
    println(f"{t7(mut ds, (mkd(13), 9), false)}"); println(f"{t7(mut ds, (mkd(14), 9), true)}");
    t8(mut ds, (mkd(15), 3), false); println("e1"); t8(mut ds, (mkd(16), 3), true); println("e2");
    let h = H { z: 0 };
    h.m(mut ds, (mkd(17), 0), false); println("f1"); h.m(mut ds, (mkd(18), 0), true); println("f2");
    n3(mut ds, mko(19), false); println("g1"); n3(mut ds, mko(20), true); println("g2");
    n4(mut ds, mko(21), false); println("h1"); n4(mut ds, mko(22), true); println("h2");
    mko(23).take(mut ds, false); println("i1"); mko(24).take(mut ds, true); println("i2");
    mko(25).take2(mut ds, false); println("j1"); mko(26).take2(mut ds, true); println("j2");
    println(f"end{ds.len()}");
}"#,
        &[
            "dD1n1",
            "a1",
            "a2",
            "dD3n3",
            "b1",
            "b2",
            "dD5n5",
            "0",
            "dD6n6",
            "1",
            "in",
            "dD7n7",
            "dD8n8",
            "c1",
            "in",
            "dD10n10",
            "c2",
            "dD11n11",
            "dD111n111",
            "d1",
            "dD112n112",
            "d2",
            "dD13n13",
            "9",
            "1",
            "k315",
            "dD15n15",
            "e1",
            "e2",
            "dD17n17",
            "f1",
            "f2",
            "dD19n19",
            "dD119n119",
            "g1",
            "dD120n120",
            "g2",
            "dD21n21",
            "dD121n121",
            "h1",
            "dD122n122",
            "h2",
            "dD23n23",
            "dD123n123",
            "i1",
            "dD124n124",
            "i2",
            "dD25n25",
            "dD125n125",
            "j1",
            "dD126n126",
            "j2",
            "end11",
            "dD2n2",
            "dD4n4",
            "dD9n9",
            "dD12n12",
            "dD14n14",
            "dD16n16",
            "dD18n18",
            "dD20n20",
            "dD22n22",
            "dD24n24",
            "dD26n26",
        ],
        "asan_conditionally_pushed_tuple_and_view_parts_free_once",
        40,
    );
}
