//! B-2026-10-05-44 -- `.clone()` on a generic-typed binding inside a
//! monomorph had no handler when compiled: a `ref T` param bound to a `Vec`
//! from a temporary argument (`rc(mkv())`), and a leaf of a destructuring `let`
//! (`let (v, n) = (x.clone(), 1); v.clone()`), where `--interp` ran both.

use super::*;

/// `ref T` at `T = Vec[i64]` from a temporary, and destructured leaves at
/// `T = String`, `T = Vec[i64]` and `T = i64`.
#[test]
fn e2e_generic_clone_on_ref_temp_and_destructured_leaf() {
    let src = r#"fn mkv() -> Vec[i64] { vec![1, 2, 3] }
fn rc[T: Clone](x: ref T) -> T { x.clone() }
fn pair[T: Clone](x: ref T) -> T {
    let (v, n) = (x.clone(), 1);
    let w = v.clone();
    w
}
fn tail[T: Clone](x: ref T) -> T {
    let (v, n) = (x.clone(), 1);
    v.clone()
}
fn main() {
    let d = rc(mkv());
    println(f"a:{d.len()} {d[2]}");
    let e = rc(vec![4, 5]);
    println(f"b:{e.len()}");
    println(f"c:{pair("ab".to_string())} {tail("cd".to_string())} {tail(7)}");
    let p = pair(mkv());
    println(f"d:{p.len()}");
}
"#;
    let want = "a:3 3\nb:2\nc:ab cd 7\nd:3\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}

/// The row's own shape: the leaf comes out of a `match` over a generic enum
/// with a diverging arm. Output only -- that destructure's leaf leaks on its
/// own, with or without generics, which is a separate row.
#[test]
fn e2e_generic_clone_on_leaf_destructured_from_match() {
    let src = r#"enum Cnt[T] { More(T, i64), Done }
fn f[T: Clone](e: ref Cnt[T]) -> Option[T] {
    let (v, n) = match e { Cnt.More(v, n) => (v.clone(), n), Cnt.Done => return None };
    let w = v.clone();
    Some(w)
}
fn main() {
    let c = Cnt.More("ab".to_string(), 2);
    match f(c) { Some(s) => println(f"e:{s}"), None => println("e:none") }
}
"#;
    let want = "e:ab\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
