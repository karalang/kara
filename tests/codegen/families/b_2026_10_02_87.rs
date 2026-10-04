//! B-2026-10-02-87 -- a generic `match` over a by-value `Option[T]` param at
//! `T = Array[S, N]` failed module verification (`ret i64` against the array)
//! when `S` is a plain-data struct; B-2026-10-02-77 had admitted scalar
//! elements only.

use super::*;

/// The row's `p1` / `p3` spellings and B-2026-10-03-13's `return match` form
/// at a plain struct element and a nested plain struct with `f64` / `bool`
/// fields, on both the `None` and `Some` paths. Output matches the
/// interpreter.
#[test]
fn e2e_generic_option_match_at_plain_struct_array() {
    let src = r#"struct S { id: i64 }
struct T2 { a: S, b: f64, c: bool }
fn p1[T](x: Option[T], d: T) -> T { let k: T = match x { Some(v) => v, None => d }; return k; }
fn p3[T](x: Option[T], d: T) -> T { match x { Some(v) => v, None => d } }
fn pick[T](a: Option[T], d: T) -> T { return match a { Some(x) => x, None => d } }
fn main() {
    let a: Option[Array[S, 2]] = None;
    let r = p1(a, [S { id: 1 }, S { id: 2 }]);
    println(f"got{r[1].id}");
    let b: Option[Array[S, 2]] = Some([S { id: 3 }, S { id: 4 }]);
    let r2 = p3(b, [S { id: 5 }, S { id: 6 }]);
    println(f"got{r2[0].id}");
    let c: Option[Array[T2, 1]] = None;
    let r3 = pick(c, [T2 { a: S { id: 7 }, b: 1.5, c: true }]);
    println(f"got{r3[0].a.id} {r3[0].b} {r3[0].c}");
}
"#;
    let want = "got2\ngot3\ngot7 1.5 true\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
