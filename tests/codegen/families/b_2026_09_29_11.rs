//! B-2026-09-29-11 — a field moved out of a LOCAL that was itself moved out of
//! a by-value param (`let w = o.w; let r = w.r;`) frees the field's heap.

use super::*;

/// B-2026-09-29-11 — `let w = o.w; let r = w.r;` and `let O { w, k } = o; let r
/// = w.r;` leaked `r`'s String buffer on the compiled backends (2 B per call
/// at -O0, bodies right everywhere): `w` took the param's callee-owned memory
/// but was never marked as carrying it, so the projection out of it registered
/// no free while `w`'s own drop skipped the cap-zeroed field. Neighbours: the
/// conditional push on both paths, the unconditional push, a return, a
/// nested destructure, and a rebind of the moved leaf.
#[test]
fn e2e_field_moved_out_of_param_derived_local_runs_its_body_once() {
    let src = r#"struct D { id: i64, name: String }
impl Drop for D { fn drop(mut ref self) { println(f"dD{self.id}{self.name}") } }
fn mkd(n: i64) -> D { return D { id: n, name: f"n{n}" }; }
struct W { r: D, s: D, b: i64 }
fn mkw(n: i64) -> W { W { r: mkd(n), s: mkd(n + 100), b: n } }
struct O { w: W, k: i64 }
fn mko(n: i64) -> O { O { w: mkw(n), k: n } }
fn g1(o: O) { let w = o.w; let r = w.r; println(f"{r.id}"); }
fn g2(o: O) { let O { w, k } = o; let r = w.r; println(f"{r.id}"); }
fn g3(xs: mut ref Vec[D], o: O, c: bool) { let w = o.w; let r = w.r; if c { xs.push(r); } }
fn g4(xs: mut ref Vec[D], o: O, c: bool) { let O { w, k } = o; let r = w.r; if c { xs.push(r); } }
fn g5(xs: mut ref Vec[D], o: O) { let w = o.w; let r = w.r; xs.push(r); }
fn g6(o: O) -> D { let w = o.w; let r = w.r; println("in"); r }
fn g7(o: O) -> i64 { let O { w, k } = o; let W { r, s, b } = w; r.id + s.id }
fn g8(o: O) { let w = o.w; let r = w.r; let q = r; println(f"{q.id}"); }
fn main() {
    let mut ds: Vec[D] = Vec.new();
    g1(mko(1)); println("a");
    g2(mko(2)); println("b");
    g3(mut ds, mko(3), false); println("c1"); g3(mut ds, mko(4), true); println("c2");
    g4(mut ds, mko(5), false); println("d1"); g4(mut ds, mko(6), true); println("d2");
    g5(mut ds, mko(7)); println("e");
    let z = g6(mko(8)); println(f"f{z.id}");
    println(f"{g7(mko(9))}");
    g8(mko(10)); println("h");
    println(f"end{ds.len()}");
}"#;
    let want = "1\ndD101n101\ndD1n1\na\n2\ndD102n102\ndD2n2\nb\ndD3n3\ndD103n103\nc1\ndD104n104\nc2\ndD5n5\ndD105n105\nd1\ndD106n106\nd2\ndD107n107\ne\nin\ndD108n108\nf8\ndD8n8\ndD109n109\ndD9n9\n118\n10\ndD110n110\ndD10n10\nh\nend3\ndD4n4\ndD6n6\ndD7n7\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
