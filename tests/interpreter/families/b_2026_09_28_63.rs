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
fn test_nested_param_destructure_leaf_moved_out_runs_its_body_once() {
    let out = run(r#"struct D { id: i64, name: String }
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
}"#);
    assert_eq!(out, "dD101n101\na\ndD2n2\ndD102n102\nb1\ndD103n103\nb2\ndD4n4\ndD104n104\n0\ndD5n5\ndD105n105\n1\nin106\ndD106n106\ngot6\ndD6n6\ndD107n107\ndD7n7\n114\ndD8n8\ndD108n108\nc1\ndD109n109\nc2\ndD10n10\ndD110n110\ne1\ndD111n111\ne2\nend4\ndD1n1\ndD3n3\ndD9n9\ndD11n11\n");
}
