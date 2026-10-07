//! B-2026-10-04-50: by-value param rebind reassigned on some path

use super::*;

/// B-2026-10-04-50: a `let mut` rebind of a by-value param that the body
/// reassigns whole on SOME path but not on every one (inside an `if`, both
/// arms, a `for` / `while` loop, a `match` arm). The caller kept the value, and
/// the reassignment displaced it anyway: `--interp` ran the displaced body in
/// the callee and again in the caller, and compiled the displaced memory was
/// freed twice (a crash for any type with a heap field). The rebind now owns
/// the value on every path and drops at the end of the call. Cells cover a
/// struct, a Drop enum, `Option[R]`, `Option[Array[R, 2]]`, both arms, loops,
/// a returned rebind on both paths, a field read, a field store before the
/// reassignment, a read on the other arm, a `Vec`, and a tail field read.
#[test]
fn asan_branch_reassigned_param_rebind_owns_the_value_once() {
    assert_clean_asan_run(
        r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}" }; }
enum E { A(R), N }
impl Drop for E { fn drop(mut ref self) { println("dE") } }
fn f1(a: R, b: bool) { let mut c = a; if b { c = mk(9); } println("in"); }
fn f2(a: E, b: bool) { let mut c = a; if b { c = E.N; } println("in"); }
fn f3(a: Option[R], b: bool) { let mut c = a; if b { c = None; } println("in"); }
fn f4(a: Option[Array[R, 2]], b: bool) { let mut c = a; if b { c = None; } println("in"); }
fn f5(a: R, b: bool) { let mut c = a; if b { c = mk(8); } else { c = mk(9); } println("in"); }
fn f6(a: R) { let mut c = a; for i in 0..2 { c = mk(10 + i); } println("in"); }
fn f7(a: R, b: bool) -> R { let mut c = a; if b { c = mk(9); } println("in"); return c; }
fn f8(a: R, b: bool) { let mut c = a; if b { c = mk(9); } println(f"in{c.id}"); }
fn f9(a: R, b: bool) { let mut c = a; println("p"); if b { c = mk(9); println("q"); } println("in"); }
fn f10(a: Vec[R], b: bool) { let mut c = a; if b { c = Vec.new(); } println("in"); }
fn f11(a: R, n: i64) { let mut c = a; match n { 1 => { c = mk(9); } _ => {} } println("in"); }
fn f12(a: R) { let mut c = a; let mut i = 0; while i < 2 { c = mk(20 + i); i = i + 1; } println("in"); }
fn f13(a: R, b: bool) { let mut c = a; if b { c.id = 7; c = mk(9); } println("in"); }
fn f14(a: R, b: bool) { let mut c = a; if b { c = mk(9); } else { println(f"e{c.id}"); } println("in"); }
fn f15(a: R, b: bool) -> i64 { let mut c = a; if b { c = mk(9); } println("in"); c.id }
fn main() {
  f1(mk(1), true); println("k_s1t");
  f1(mk(1), false); println("k_s1f");
  f2(E.A(mk(1)), true); println("k_s2t");
  f2(E.A(mk(1)), false); println("k_s2f");
  f3(Some(mk(1)), true); println("k_s3t");
  f3(Some(mk(1)), false); println("k_s3f");
  f4(Some([mk(1), mk(2)]), true); println("k_s4t");
  f4(Some([mk(1), mk(2)]), false); println("k_s4f");
  f5(mk(1), true); println("k_s5t");
  f6(mk(1)); println("k_s6");
  let r = f7(mk(1), true); println(f"k_s7t{r.id}");
  let r = f7(mk(1), false); println(f"k_s7f{r.id}");
  f8(mk(1), true); println("k_s8t");
  let x = mk(1); f9(x, true); println("k_s9t");
  let mut v: Vec[R] = Vec.new(); v.push(mk(1)); v.push(mk(2)); f10(v, true); println("k_u1t");
  let mut v: Vec[R] = Vec.new(); v.push(mk(1)); v.push(mk(2)); f10(v, false); println("k_u1f");
  f11(mk(1), 1); println("k_u2t");
  f11(mk(1), 2); println("k_u2f");
  f12(mk(1)); println("k_u3");
  f13(mk(1), true); println("k_u5t");
  f14(mk(1), false); println("k_u6t");
  let n = f15(mk(1), true); println(f"k_u8t{n}");
  println("end")
}
"#,
        &[
            "dR1", "in", "dR9", "k_s1t", "in", "dR1", "k_s1f", "dE", "dR1", "in", "dE", "k_s2t",
            "in", "dE", "dR1", "k_s2f", "dR1", "in", "k_s3t", "in", "dR1", "k_s3f", "dR1", "dR2",
            "in", "k_s4t", "in", "dR1", "dR2", "k_s4f", "dR1", "in", "dR8", "k_s5t", "dR1", "dR10",
            "in", "dR11", "k_s6", "dR1", "in", "k_s7t9", "dR9", "in", "k_s7f1", "dR1", "dR1",
            "in9", "dR9", "k_s8t", "p", "dR1", "q", "in", "dR9", "k_s9t", "dR1", "dR2", "in",
            "k_u1t", "in", "dR1", "dR2", "k_u1f", "dR1", "in", "dR9", "k_u2t", "in", "dR1",
            "k_u2f", "dR1", "dR20", "in", "dR21", "k_u3", "dR7", "in", "dR9", "k_u5t", "e1", "in",
            "dR1", "k_u6t", "dR1", "in", "dR9", "k_u8t9", "end",
        ],
        "asan_branch_reassigned_param_rebind_owns_the_value_once",
    );
}
