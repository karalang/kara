//! B-2026-10-05-73 / B-2026-10-05-74 -- `x.clone()` on a type parameter bound
//! to an `Option`, a `Result` or a tuple failed to build: no dispatch arm for an
//! `Option`, and a tuple clone returned into a signature lowered as `i64`.
//! `--interp` ran both.

use super::*;

/// `ref T` at `T = Option[String]`, `Option[i64]` and a `None` (-73).
#[test]
fn e2e_generic_clone_through_ref_t_at_option() {
    let src = r#"fn dup[T: Clone](x: ref T) -> T { return x.clone(); }
fn main() {
    let a: Option[String] = Some("aa".to_string());
    let b: Option[i64] = Some(7);
    let n: Option[String] = None;
    let a2 = dup(a);
    let b2 = dup(b);
    let n2 = dup(n);
    println(f"a:{a2.unwrap()} {b2.unwrap()} {n2.is_none()} {a.is_some()}");
}
"#;
    let want = "a:aa 7 true true\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}

/// `ref T` at three tuple shapes, one all-scalar (-74).
#[test]
fn e2e_generic_clone_through_ref_t_at_tuple() {
    let src = r#"fn dup[T: Clone](x: ref T) -> T { return x.clone(); }
fn main() {
    let o: (i64, String) = (1, "a".to_string());
    let t: (String, String) = ("p".to_string(), "q".to_string());
    let u: (i64, i64) = (3, 4);
    let o2 = dup(o);
    let t2 = dup(t);
    let u2 = dup(u);
    println(f"b:{o2.0} {o2.1} {t2.0} {t2.1} {u2.0} {u2.1} {o.1}");
}
"#;
    let want = "b:1 a p q 3 4 a\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}

/// `Option[String]` and `Option[Vec[String]]` share an LLVM layout and a head
/// name, so they need distinct symbols; and a `Result` either way round.
#[test]
fn e2e_generic_clone_through_ref_t_shared_layout_and_result() {
    let src = r#"fn dup[T: Clone](x: ref T) -> T { return x.clone(); }
fn main() {
    let a: Option[String] = Some("aa".to_string());
    let ov: Option[Vec[String]] = Some(vec!["v1".to_string(), "v2".to_string()]);
    let a2 = dup(a);
    let ov2 = dup(ov);
    println(f"c:{a2.unwrap()}");
    match ov2 { Some(v) => println(f"d:{v.len()} {v[0]} {v[1]}"), None => println("d:none") }
    let r: Result[String, i64] = Ok("good".to_string());
    let e: Result[i64, String] = Err("bad".to_string());
    let r2 = dup(r);
    let e2 = dup(e);
    match r2 { Ok(s) => println(f"e:{s}"), Err(x) => println(f"e:{x}") }
    match e2 { Ok(s) => println(f"f:{s}"), Err(x) => println(f"f:{x}") }
}
"#;
    let want = "c:aa\nd:2 v1 v2\ne:good\nf:bad\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}

/// `mut ref T`, and the method form `self.v.clone()` on `G[Option[i64]]` and
/// `G[(i64, String)]`.
#[test]
fn e2e_generic_clone_through_mut_ref_t_and_impl_field() {
    let src = r#"fn dupm[T: Clone](x: mut ref T) -> T { return x.clone(); }
struct G[T] { v: T }
impl[T: Clone] G[T] { fn get(ref self) -> T { return self.v.clone(); } }
fn main() {
    let mut a: Option[String] = Some("m".to_string());
    let mut t: (i64, String) = (2, "mt".to_string());
    let a2 = dupm(mut a);
    let t2 = dupm(mut t);
    println(f"g:{a2.unwrap()} {t2.0} {t2.1}");
    let g = G { v: Some(5) };
    let x = g.get();
    let k = G { v: (1, "tk".to_string()) };
    let z = k.get();
    println(f"h:{x.unwrap()} {z.0} {z.1}");
}
"#;
    let want = "g:m 2 mt\nh:5 1 tk\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}

/// The method form at `Option[String]` and `Option[Vec[String]]` in one
/// program. Output only: the struct literals themselves leak on their own,
/// with or without `get`, which is a separate row.
#[test]
fn e2e_generic_clone_impl_field_at_option_heap_payloads() {
    let src = r#"struct G[T] { v: T }
impl[T: Clone] G[T] { fn get(ref self) -> T { return self.v.clone(); } }
fn main() {
    let h = G { v: Some("hs".to_string()) };
    let k = G { v: Some(vec!["a".to_string(), "bb".to_string()]) };

    let y = h.get();
    let z = k.get();
    match y { Some(s) => println(f"{s}"), None => println("none") }
    match z { Some(v) => println(f"{v.len()} {v[1]}"), None => println("none") }
}
"#;
    let want = "hs\n2 bb\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
