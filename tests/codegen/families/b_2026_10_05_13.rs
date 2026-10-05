//! B-2026-10-05-13 -- a `Self`-typed parameter in a concrete impl
//! (`fn cself(self, o: Self)`) reached codegen as a path naming no type: a
//! generic enum target failed module verification, a plain struct's `o.a`
//! had no field layout, and a trait impl over `Vec[i64]` had no `o.len()`.

use super::*;

/// The row's program: the argument's `Drop` body runs in the method and the
/// receiver's at the end of `main`.
#[test]
fn e2e_self_param_in_concrete_generic_enum_impl() {
    let src = r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}"); } }
enum G[T] { X(T), Y }
impl G[R] { fn cself(self, o: Self) -> i64 { 1 } }
fn mk(i: i64) -> G[R] { G.X(R { id: i, s: f"s{i}" }) }
fn main() { let d = mk(4); println(f"{d.cself(mk(5))}"); println("end") }
"#;
    let want = "dR5\n1\ndR4\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}

/// By-value, `ref` and tuple-nested `Self` parameters on a plain struct.
#[test]
fn e2e_self_param_in_plain_struct_impl() {
    let src = r#"struct Q { a: String, n: i64 }
impl Q {
    fn both(self, o: Self) -> String { o.a }
    fn pair(ref self, o: ref Self) -> i64 { self.n + o.n }
    fn tup(ref self, t: (Self, i64)) -> i64 { t.1 + t.0.n }
}
fn main() {
    let r = Q { a: "r".to_string(), n: 1 };
    let s = Q { a: "s-heap-string-long-xxx".to_string(), n: 2 };
    println(f"{r.pair(s)}");
    let t = Q { a: "t-heap-string-long-xxx".to_string(), n: 5 };
    println(f"{r.tup((t, 3))}");
    println(r.both(s));
}
"#;
    let want = "3\n8\ns-heap-string-long-xxx\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}

/// A trait method taking `o: ref Self`, implemented for `Vec[i64]` and a struct.
#[test]
fn e2e_self_param_in_trait_impl_over_vec() {
    let src = r#"trait Join { fn joined(ref self, o: ref Self) -> i64; }
impl Join for Vec[i64] { fn joined(ref self, o: ref Self) -> i64 { self.len() + o.len() } }
struct W { n: i64 }
impl Join for W { fn joined(ref self, o: ref Self) -> i64 { self.n + o.n } }
fn main() {
    let a = vec![1, 2];
    let b = vec![3];
    println(f"{a.joined(b)}");
    let w = W { n: 4 };
    let x = W { n: 5 };
    println(f"{w.joined(x)}");
}
"#;
    let want = "3\n9\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
