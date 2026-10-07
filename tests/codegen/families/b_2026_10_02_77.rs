//! B-2026-10-02-77 -- a generic function instantiated at `T = Array[E, N]`
//! lowers a body pattern over `Option[T]` at the array's real width.

use super::*;

/// B-2026-10-02-77 — `fn pick[T](a: Option[T], d: T) -> T { match a { Some(x)
/// => x, None => d } }` called with `T = Array[i64, 2]`. The free-fn
/// structural binding had a tuple leg and no array leg, so `T` reached the
/// body only through the LLVM subst, `x` was typed at the erased one-word
/// width, and the arm returned `i64 0` against a `[2 x i64]` return type.
///
/// Before: every cell failed `karac build` with "Function return type does
/// not match operand type of return inst!" while `--interp` printed the
/// right line. Scalar elements only (nested scalar arrays included); a heap
/// element is held back by B-2026-10-02-78.
#[test]
fn e2e_generic_fn_at_scalar_array_type_returns_match_over_option() {
    let out = run_program(
        r#"fn pick[T](a: Option[T], d: T) -> T { match a { Some(x) => x, None => d } }
fn pickr[T](a: Option[T], d: T) -> T { return match a { Some(x) => x, None => d } }
fn main() {
    let a: Option[Array[i64, 2]] = None;
    let r = pick(a, Array[3, 4]); println(f"a{r[0]}{r[1]}");
    let b: Option[Array[i64, 2]] = Some(Array[1, 2]);
    let q = pickr(b, Array[3, 4]); println(f"b{q[0]}{q[1]}");
    let c: Option[Array[i64, 2]] = None;
    let s = pickr(c, [5, 6]); println(f"c{s[0]}{s[1]}");
    let d: Option[Array[f64, 2]] = None;
    let f = pick(d, Array[1.5, 2.5]); println(f"d{f[0]} {f[1]}");
    let e: Option[Array[Array[i64, 2], 2]] = Some(Array[Array[7, 8], Array[9, 10]]);
    let g = pick(e, Array[Array[0, 0], Array[0, 0]]); println(f"e{g[0][1]} {g[1][0]}");
    let h: Option[Array[bool, 3]] = None;
    let k = pickr(h, Array[true, false, true]); println(f"h{k[0]}{k[1]}{k[2]}")
}
"#,
    );
    assert_eq!(
        out,
        Some("a34\nb12\nc56\nd1.5 2.5\ne8 9\nhtruefalsetrue\n".to_string()),
        "a generic fn at T = Array must hand back the array value"
    );
}
