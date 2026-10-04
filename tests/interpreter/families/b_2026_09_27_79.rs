//! B-2026-09-27-79: a by-value boxed Option param pushed into a local Vec on only some paths

use super::*;

/// B-2026-09-27-79: a by-value boxed `Option[S]` param pushed into a callee-local `Vec` on
/// only some paths (`if c { v.push(a) }`), where `S` has no `Drop` of its own but a field
/// that does, crashed with no output on every compiled surface and lost `d2` / `d4` under
/// `--interp` on the path that does not push. Fixed by B-2026-09-28-67's per-path ownership
/// (5a9ea3ca0); this pins the row's own program, named and temporary arguments on each path.
#[test]
fn interp_boxed_optres_param_pushed_on_some_paths_runs_its_field_body_once() {
    let out = run(r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct S { r: R, s: String }
fn mk(i: i64) -> S { S { r: R { id: i }, s: f"heap-string-longer-than-sso-{i}" } }
fn rb(a: Option[S], c: bool) -> i64 { let mut v: Vec[Option[S]] = Vec.new(); if c { v.push(a) }; println("in"); 5 }
fn main() {
  let a = Some(mk(1)); println(f"k{rb(a, true)}");
  let b = Some(mk(2)); println(f"k{rb(b, false)}");
  println(f"k{rb(Some(mk(3)), true)}");
  println(f"k{rb(Some(mk(4)), false)}");
  println("end")
}
"#);
    assert_eq!(out, "d1\nin\nk5\nin\nd2\nk5\nd3\nin\nk5\nin\nd4\nk5\nend\n");
}
