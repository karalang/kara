//! B-2026-09-29-21 -- a struct field moved out and then stored into runs
//! the new value's `Drop` body, generic or not, with no read of freed memory.

use super::*;

/// B-2026-09-29-21 -- moving a field out of a struct local and then
/// storing into it (`let x = q.u; q.u = mk(8);`) lost the new value's `Drop`
/// body on every compiled surface, generic or not, and over a generic struct
/// (`Q[S2]`) also read freed memory. The move masked the field out of the
/// field-bodies walker and, when that emptied it, retracted the walker, so the
/// later store had nothing to re-arm; and a generic struct's field named its
/// type by the declared parameter `U`, which no drop-kind question answers.
#[test]
fn asan_field_moved_out_then_restored_runs_new_value_body() {
    assert_clean_asan_run(
        r#"shared struct Sh { k: i64 }
struct S2 { h: Sh, id: i64 }
impl Drop for S2 { fn drop(mut ref self) { println(f"dS{self.id}") } }
struct Q[U] { u: U }
struct Qn[U] { u: U, n: i64 }
struct Qv[U] { u: U, v: U }
struct W { u: S2 }
fn mk(i: i64) -> S2 { return S2 { h: Sh { k: i }, id: i } }
fn r1() { let mut q = Q { u: mk(9) }; let x = q.u; q.u = mk(8); println(f"x{q.u.id} {x.id}") }
fn r2() { let mut q = Q { u: mk(9) }; let c = q.u.id > 3; if c { let x = q.u; println(f"m{x.id}") } q.u = mk(8); println(f"x{q.u.id}") }
fn r3() { let mut q = W { u: mk(9) }; let x = q.u; q.u = mk(8); println(f"x{q.u.id} {x.id}") }
fn r4() { let mut q = W { u: mk(9) }; let c = q.u.id > 3; if c { let x = q.u; println(f"m{x.id}") } q.u = mk(8); println(f"x{q.u.id}") }
fn r5() { let mut q = Q { u: mk(9) }; let x = q.u; println(f"x{x.id}") }
fn r6() { let mut q = Qn { u: mk(9), n: 1 }; let x = q.u; q.u = mk(8); println(f"x{q.u.id} {x.id} {q.n}") }
fn r7() { let mut q = Qv { u: mk(9), v: mk(5) }; let x = q.u; q.u = mk(8); println(f"x{q.u.id} {x.id}") }
fn k1() { let mut q = W { u: mk(9) }; let x = q.u; let c = x.id > 3; if c { q.u = mk(8); } println(f"x{x.id}") }
fn k2() {
  let mut q = W { u: mk(1) }; let mut i = 0;
  while i < 2 { let x = q.u; println(f"t{x.id}"); q.u = mk(i + 5); i += 1; }
}
fn k3() { let mut q = W { u: mk(9) }; { let x = q.u; println(f"b{x.id}") } q.u = mk(8); println(f"x{q.u.id}") }
fn k4w() -> W { let mut q = W { u: mk(9) }; let x = q.u; println(f"m{x.id}"); q.u = mk(8); return q }
fn k4() { let w = k4w(); println(f"x{w.u.id}") }
fn k7() {
  let mut q = Q { u: mk(9) }; let x = q.u; println(f"x{x.id}")
  q.u = mk(8); let y = q.u; q.u = mk(7); println(f"y{y.id}{q.u.id}")
}
fn k8() { let mut q = W { u: mk(9) }; let x = q.u; println(f"x{x.id}") }
fn main() {
  println("-r1"); r1()
  println("-r2"); r2()
  println("-r3"); r3()
  println("-r4"); r4()
  println("-r5"); r5()
  println("-r6"); r6()
  println("-r7"); r7()
  println("-k1"); k1()
  println("-k2"); k2()
  println("-k3"); k3()
  println("-k4"); k4()
  println("-k7"); k7()
  println("-k8"); k8()
  println("end")
}
"#,
        &[
            "-r1", "x8 9", "dS9", "dS8", "-r2", "m9", "dS9", "x8", "dS8", "-r3", "x8 9", "dS9",
            "dS8", "-r4", "m9", "dS9", "x8", "dS8", "-r5", "x9", "dS9", "-r6", "x8 9 1", "dS9",
            "dS8", "-r7", "x8 9", "dS9", "dS5", "dS8", "-k1", "dS8", "x9", "dS9", "-k2", "t1",
            "dS1", "t5", "dS5", "dS6", "-k3", "b9", "dS9", "x8", "dS8", "-k4", "m9", "dS9", "x8",
            "dS8", "-k7", "x9", "dS9", "y87", "dS8", "dS7", "-k8", "x9", "dS9", "end",
        ],
        "asan_field_moved_out_then_restored_runs_new_value_body",
    );
}
