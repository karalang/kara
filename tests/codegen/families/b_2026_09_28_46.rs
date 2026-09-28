//! B-2026-09-28-46 — a value handed to a GENERIC callee that stores it on
//! only some paths runs its fields' `Drop` bodies once.

use super::*;

/// B-2026-09-28-46 — `fn gst[T](xs: mut ref Vec[T], x: T, c: bool) { if c {
/// xs.push(x); } }` at a struct with no `Drop` of its own whose fields run
/// user bodies lost them on the path that did not store it (everywhere for a
/// fresh temporary, interpreted for a shared-field one) and ran them twice
/// compiled for a named argument on the path that did; a projected
/// shared-field struct (`gst(mut zs, q.z, false)`) double-freed compiled.
/// The mono prologue now adopts the bodies per path as the non-generic one
/// does, and the caller stands a named or projected argument down on every
/// path.
#[test]
fn e2e_generic_callee_stores_field_bodies_struct_on_one_path_runs_them_once() {
    let src = r#"struct D { id: i64, name: String }
impl Drop for D { fn drop(mut ref self) { println(f"dD{self.id}{self.name}") } }
fn mkd(n: i64) -> D { return D { id: n, name: f"n{n}" }; }
struct W { r: D, s: D, b: i64 }
fn mkw(n: i64) -> W { W { r: mkd(n), s: mkd(n + 100), b: n } }
shared struct N { v: i64 }
struct Z { d: D, h: N }
struct Q { z: Z, t: D }
struct X { w: W, t: D }
fn gst[T](xs: mut ref Vec[T], x: T, c: bool) { if c { xs.push(x); } }
fn gown[T](x: T, c: bool) -> i64 { let mut xs: Vec[T] = Vec.new(); if c { xs.push(x); } xs.len() }
fn main() {
    let mut ws: Vec[W] = Vec.new();
    gst(mut ws, mkw(1), false); println("a");
    gst(mut ws, mkw(2), true); println("b");
    let w3 = mkw(3); gst(mut ws, w3, false); println("c");
    let w4 = mkw(4); gst(mut ws, w4, true); println("d");
    let x5 = X { w: mkw(5), t: mkd(305) }; gst(mut ws, x5.w, false); println("e");
    let x6 = X { w: mkw(6), t: mkd(306) }; gst(mut ws, x6.w, true); println("f");
    println(f"g{gown(mkw(7), false)}");
    let w8 = mkw(8); println(f"g{gown(w8, true)}");
    let mut zs: Vec[Z] = Vec.new();
    gst(mut zs, Z { d: mkd(11), h: N { v: 11 } }, false); println("h");
    let z12 = Z { d: mkd(12), h: N { v: 12 } }; gst(mut zs, z12, true); println("i");
    let z13 = Z { d: mkd(13), h: N { v: 13 } }; gst(mut zs, z13, false); println("j");
    let q14 = Q { z: Z { d: mkd(14), h: N { v: 14 } }, t: mkd(314) }; gst(mut zs, q14.z, false); println("k");
    let q15 = Q { z: Z { d: mkd(15), h: N { v: 15 } }, t: mkd(315) }; gst(mut zs, q15.z, true); println("l");
    println(f"{ws.len()} {zs.len()}");
    println("end")
}"#;
    let want = "dD101n101\ndD1n1\na\nb\ndD103n103\ndD3n3\nc\nd\ndD105n105\ndD5n5\ndD305n305\ne\ndD306n306\nf\ndD107n107\ndD7n7\ng0\ndD108n108\ndD8n8\ng1\ndD11n11\nh\ni\ndD13n13\nj\ndD14n14\ndD314n314\nk\ndD315n315\nl\n3 2\ndD12n12\ndD15n15\ndD102n102\ndD2n2\ndD104n104\ndD4n4\ndD106n106\ndD6n6\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
