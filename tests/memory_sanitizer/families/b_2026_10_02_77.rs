//! B-2026-10-02-77 -- a generic function instantiated at `T = Array[E, N]`
//! hands back the array by value with nothing leaked or freed twice.

use super::*;

/// B-2026-10-02-77 — the `Option[T]` match at `T = Array[i64, 2]` and at a
/// nested scalar array, beside a heap `String` the program owns, so the run
/// has allocations for ASAN to account.
#[test]
fn asan_generic_fn_at_scalar_array_type_hands_back_value() {
    assert_clean_asan_run(
        r#"fn pick[T](a: Option[T], d: T) -> T { match a { Some(x) => x, None => d } }
fn main() {
    let s = f"heap-string-longer-than-sso-{1}";
    let a: Option[Array[i64, 2]] = None;
    let r = pick(a, Array[3, 4]);
    let e: Option[Array[Array[i64, 2], 2]] = Some(Array[Array[7, 8], Array[9, 10]]);
    let g = pick(e, Array[Array[0, 0], Array[0, 0]]);
    println(f"{s} {r[0]}{r[1]} {g[0][1]}{g[1][0]}");
}
"#,
        &["heap-string-longer-than-sso-1 34 89"],
        "B-2026-10-02-77 generic fn at scalar Array type",
    );
}
