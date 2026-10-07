//! B-2026-10-05-9: by-value param shadowed by a scalar projection

use super::*;

/// B-2026-10-05-9: a by-value param SHADOWED by a top-level `let` (`fn c1(s:
/// S) -> i64 { let s = s.r.id; s }`). The name-matching "returns the param"
/// walk read the tail `s` as handing the param back, so the caller stood down
/// and nobody ran its `Drop` body: compiled for a temporary argument, the
/// interpreter for a named one. Callers with the program now ask the per-param
/// fate first. Cells cover temporary and named arguments, a part returned
/// through the shadow (still the result's), an `Option` param, a method
/// projection, a struct with no heap field, an owned-`self` method, a shadow
/// inside a branch, a non-shadowing read, and an arithmetic shadow.
#[test]
fn e2e_param_shadowed_by_scalar_projection_keeps_its_body() {
    let Some(out) = run_program(
        r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct S { r: R, s: String }
fn mks(i: i64) -> S { S { r: R { id: i }, s: f"ssssssssssssssssssssssssssss{i}" } }
fn c1(s: S) -> i64 { let s = s.r.id; s }
fn c5(s: S) -> R { let s = s.r; s }
fn c6(x: Option[R]) -> i64 { let x = 5; x }
fn c8(s: S) -> i64 { let s = s.s.len(); s }
fn c9(s: R) -> i64 { let s = s.id; s }
struct W { v: i64 } impl W { fn m(self, s: S) -> i64 { let s = s.r.id; s } }
fn c10(s: S, b: bool) -> i64 { if b { let s = s.r.id; return s; } 0 }
fn c11(s: S) -> S { let t = s.r.id; println(f"t{t}"); s }
fn c12(s: S) -> i64 { let s = s.r.id + 1; s }
fn main() {
    println(f"k{c1(mks(3))}"); println("_a1");
    let a = mks(6); println(f"k{c1(a)}"); println("_a2");
    let a = mks(6); let n = c1(a); println(f"k{n}"); println("_a3");
    let r = c5(mks(7)); println(f"k{r.id}"); println("_a4");
    let a = mks(7); let r = c5(a); println(f"k{r.id}"); println("_a5");
    println(f"k{c6(Some(R { id: 8 }))}"); println("_a6");
    let o = Some(R { id: 9 }); println(f"k{c6(o)}"); println("_a7");
    println(f"k{c8(mks(10))}"); println("_a8");
    let a = R { id: 11 }; println(f"k{c9(a)}"); println("_a9");
    println(f"k{c9(R { id: 12 })}"); println("_b1");
    let w = W { v: 1 }; println(f"k{w.m(mks(14))}"); println("_b3");
    println(f"k{c10(mks(15), true)}"); println("_b4");
    let r = c11(mks(16)); println(f"k{r.r.id}"); println("_b5");
    let a = mks(17); println(f"k{c12(a)}"); println("_b6");
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "d3\nk3\n_a1\nk6\nd6\n_a2\nd6\nk6\n_a3\nk7\nd7\n_a4\nk7\nd7\n_a5\nd8\nk5\n_a6\nk5\nd9\n_a7\nd10\nk30\n_a8\nk11\nd11\n_a9\nd12\nk12\n_b1\nd14\nk14\n_b3\nd15\nk15\n_b4\nt16\nk16\nd16\n_b5\nk18\nd17\n_b6\nend\n", "got:\n{out}");
}
