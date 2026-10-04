//! B-2026-10-01-15 / B-2026-10-02-55 / B-2026-09-29-112 -- a field read on an
//! element of a fixed `Array` failed the build with "cannot resolve field"
//! when codegen had recorded no element type for the array: a binding whose
//! initializer is a `match`, `if` or block, a `for` binding over an array of
//! structs, and an array held in an unannotated tuple local.

use super::*;

/// B-2026-10-02-55 / B-2026-10-01-15 — `let k = match x { Some(t) => t, None
/// => z() }; k[1].id`, the `if` and block spellings, and a user-enum arm. The
/// annotated spelling always compiled; the unannotated one now registers the
/// same element type and agrees with it, drops included.
#[test]
fn e2e_branch_initialized_array_element_field_read() {
    let src = r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}" }; }
enum EArr { A(Array[R, 2]), B }
fn z() -> Array[R, 2] { return [mk(0), mk(0)]; }
fn fr(i: i64) -> Array[R, 1] { return [mk(i)]; }
fn n12(x: Option[Array[R, 2]]) -> i64 { let k = match x { Some(t) => t, None => z() }; return k[1].id; }
fn e1(x: EArr) -> i64 { let k = match x { EArr.A(v) => v, EArr.B => z() }; return k[0].id + k[1].id; }
fn main() {
    println(f"got{n12(Some([mk(1), mk(2)]))}");
    { let c = true; let z = if c { fr(60) } else { fr(61) }; println(f"r{z[0].id}"); }
    { let a = { let q = 3; fr(q) }; println(f"b{a[0].id}"); }
    println(f"e{e1(EArr.A([mk(5), mk(6)]))}");
    println(f"e{e1(EArr.B)}");
    println("end");
}
"#;
    let want = "dR1\ndR2\ngot2\nr60\ndR60\nb3\ndR3\ndR5\ndR6\ne11\ndR0\ndR0\ne0\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}

/// B-2026-10-01-15 — `for q in l { q.id }` over a named array of structs,
/// annotated or not. The loop borrows the array, so the elements' bodies run
/// when `l` dies, after the loop.
#[test]
fn e2e_for_over_struct_array_reads_element_field() {
    let src = r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}" }; }
fn f2(i: i64) -> Array[R, 2] { return [mk(i), mk(i + 1)]; }
fn main() {
    { let l = f2(80); for q in l { println(f"q{q.id} {q.name}") } println("r"); }
    { let l: Array[R, 2] = [mk(90), mk(91)]; for q in l { println(f"q{q.id}") } println("r"); }
    println("end");
}
"#;
    let want = "q80 h80\nq81 h81\ndR80\ndR81\nr\nq90\nq91\ndR90\ndR91\nr\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}

/// B-2026-09-29-112 — `let t = (a, 7); t.0[0].id` and `let t = (a, b);
/// t.1[1].id`, plus a nested struct field and a `String` field through the
/// same projection.
#[test]
fn e2e_array_in_unannotated_tuple_element_field_read() {
    let src = r#"struct W { w: i64, k: i64 }
struct D { id: i64, w: W, s: String }
impl Drop for D { fn drop(mut ref self) { println(f"dD{self.id}") } }
fn mkd(n: i64) -> D { return D { id: n, w: W { w: n * 10, k: n * 100 }, s: f"sssssssssssssssssssssssssssssssssss{n}" } }
fn main() {
    { let a: Array[D, 2] = [mkd(1), mkd(2)]; let t = (a, 7); println(f"t {t.1} {t.0[0].id}") }
    { let a: Array[D, 2] = [mkd(3), mkd(4)]; let b: Array[D, 2] = [mkd(5), mkd(6)]; let t = (a, b); println(f"t {t.0[0].id} {t.1[1].id}") }
    { let a: Array[D, 2] = [mkd(7), mkd(8)]; let t = (9, a); println(f"t {t.1[1].w.w} {t.1[0].w.k} {t.1[1].s.len()}") }
    println("end");
}
"#;
    let want = "t 7 1\ndD1\ndD2\nt 3 6\ndD3\ndD4\ndD5\ndD6\nt 80 700 36\ndD7\ndD8\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
