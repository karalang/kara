//! B-2026-10-05-29: generic callee shadowing its by-value param

use super::*;

/// B-2026-10-05-29: a GENERIC callee that shadows its by-value param with an
/// unrelated value (`fn g1[T](s: T, n: i64) -> i64 { let s = n; s }`). The
/// name-matching "returns the param" walks read the tail `s` as the param
/// handed back, and the per-param fate that settles the non-generic spelling
/// does not reach a generic callee, so the caller stood down and the
/// argument's `Drop` body ran nowhere, on both backends. Compiled, the
/// `Option` param's temporary also needs the escape walk behind the monomorph
/// call to see the shadow. Cells cover temporary and named arguments, a
/// struct with a heap field, an `Option` param, the unshadowed control, a
/// param moved to a local before the shadow, and an arithmetic shadow.
#[test]
fn asan_generic_param_shadowed_by_unrelated_let_keeps_its_body() {
    assert_clean_asan_run(
        r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct S { r: R, s: String }
fn mks(i: i64) -> S { S { r: R { id: i }, s: f"ssssssssssssssssssssssssssss{i}" } }
fn g1[T](s: T, n: i64) -> i64 { let s = n; s }
fn g2[T](s: Option[T], n: i64) -> i64 { let s = n; s }
fn g3[T](s: T, n: i64) -> i64 { let t = n; t }
fn g8[T](s: T, n: i64) -> i64 { let m = s; let s = n; s }
fn g9[T](s: T, n: i64) -> i64 { let s = n + 1; s }
fn main() {
    println(f"k{g1(R { id: 13 }, 4)}"); println("_a1");
    let a = R { id: 14 }; println(f"k{g1(a, 4)}"); println("_a2");
    println(f"k{g1(mks(20), 4)}"); println("_a3");
    let a = mks(21); let n = g1(a, 5); println(f"k{n}"); println("_a4");
    println(f"k{g2(Some(R { id: 22 }), 4)}"); println("_a5");
    let o = Some(R { id: 23 }); println(f"k{g2(o, 4)}"); println("_a6");
    println(f"k{g3(R { id: 24 }, 4)}"); println("_a7");
    println(f"k{g8(R { id: 25 }, 4)}"); println("_a8");
    let a = R { id: 26 }; println(f"k{g9(a, 4)}"); println("_a9");
    println("end")
}
"#,
        &[
            "d13", "k4", "_a1", "k4", "d14", "_a2", "d20", "k4", "_a3", "d21", "k5", "_a4", "d22",
            "k4", "_a5", "k4", "d23", "_a6", "d24", "k4", "_a7", "d25", "k4", "_a8", "k5", "d26",
            "_a9", "end",
        ],
        "asan_generic_param_shadowed_by_unrelated_let_keeps_its_body",
    );
}
