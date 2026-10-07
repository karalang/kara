//! B-2026-09-29-25: a moved match-arm binding and later bindings of the same name.

use super::*;

/// B-2026-09-29-25 — moving a `match` arm binding (`Hr.P(t) => { let u = t }`)
/// no longer silences a LATER binding of the same name. The move cleared the
/// per-path bit keyed by the NAME `t`, and every later `t` in the function had
/// its scope-exit drop guarded on that bit, so a sibling block's plain
/// `let t = S1 { .. }` ran no `Drop` body and leaked its heap on every compiled
/// surface. A registration for a new slot of that name, with no action of the
/// name still live, now stores `true` into the bit. Covers a later struct
/// local passed by value, a plain struct local, a tuple local passed by value,
/// a plain tuple local, a loop whose arm moves the binding on some trips, and
/// the `if let` and read-only-arm spellings that were already right.
#[test]
fn e2e_moved_arm_binding_leaves_later_same_name_locals_their_bodies() {
    let Some(out) = run_program(
        r#"struct S1 { v: i64, s: String }
impl Drop for S1 { fn drop(mut ref self) { println(f"dS{self.v}") } }
enum Hr { P(S1), Q }
fn takr(t: S1) -> i64 { return t.v }
fn takt(t: (S1, i64)) -> i64 { return t.1 }
fn f1() -> i64 {
  { let h = Hr.P(S1 { v: 1, s: "a" }); match h { Hr.P(t) => { let u = t; println(f"n{u.v}") } Hr.Q => {} } }
  { let t = S1 { v: 2, s: "b" }; println(f"k{takr(t)}") }
  { let t = S1 { v: 3, s: "c" }; println(f"n{t.v}") }
  { let t = (S1 { v: 4, s: "d" }, 40); println(f"k{takt(t)}") }
  { let t = (S1 { v: 5, s: "e" }, 50); println(f"n{t.1}") }
  return 0
}
fn f2(c: bool) -> i64 {
  let mut i = 0;
  while i < 2 {
    let h = Hr.P(S1 { v: 10 + i, s: "f" });
    match h { Hr.P(t) => { if c { let u = t; println(f"m{u.v}") } } Hr.Q => {} }
    { let t = S1 { v: 20 + i, s: "g" }; println(f"n{t.v}") }
    i = i + 1;
  };
  return 0
}
fn f4() -> i64 {
  { let h = Hr.P(S1 { v: 40, s: "j" }); if let Hr.P(t) = h { let u = t; println(f"n{u.v}") } }
  { let t = S1 { v: 41, s: "k" }; println(f"n{t.v}") }
  { let h = Hr.P(S1 { v: 42, s: "l" }); match h { Hr.P(t) => { println(f"r{t.v}") } Hr.Q => {} } }
  { let t = S1 { v: 43, s: "m" }; println(f"n{t.v}") }
  return 0
}
fn main() {
  println(f"r {f1()}");
  println(f"r {f2(false)}"); println(f"r {f2(true)}");
  println(f"r {f4()}");
  println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "n1\ndS1\nk2\ndS2\nn3\ndS3\nk40\ndS4\nn50\ndS5\nr 0\ndS10\nn20\ndS20\ndS11\nn21\ndS21\nr 0\nm10\ndS10\nn20\ndS20\nm11\ndS11\nn21\ndS21\nr 0\nn40\ndS40\nn41\ndS41\nr42\ndS42\nn43\ndS43\nr 0\nend\n", "got:\n{out}");
}
