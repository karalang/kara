//! B-2026-10-01-7: a boxed `Option` / `Result` param handed back wrapped in a returned value.

use super::*;

/// B-2026-10-01-7 — a by-value boxed `Option` / `Result` param handed back
/// WRAPPED (inside a returned `Vec`, array literal, struct or tuple) frees its
/// box once. The caller's post-call compare that disarms a handed-back box
/// could not see a box inside a returned `Vec`'s heap buffer, and a NAMED
/// `Option` / `Result` argument never reached that compare at all, so the
/// caller's box drop and the result's both freed it (a segfault with no output
/// under the JIT and AOT). Covers fresh-temp and named arguments, `Vec[x]` and
/// `[x]`, a struct and a tuple wrapper, a callee that hands it back on one
/// exit only, a `Result`, a method, an associated function, a generic callee
/// (named and temp), a loop and `None`, with the whole hand-back `id(o)` as
/// a guard (the result keeps the caller's registration there).
#[test]
fn interp_boxed_option_param_handed_back_wrapped_frees_box_once() {
    let out = run(r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}" }; }
struct W { o: Option[R], n: i64 }
fn ov(x: Option[R]) -> Vec[Option[R]] { return Vec[x] }
fn oa(x: Option[R]) -> Vec[Option[R]] { return [x] }
fn ow(x: Option[R]) -> W { return W { o: x, n: 1 } }
fn ot(x: Option[R]) -> (Option[R], i64) { return (x, 1) }
fn oc(x: Option[R], c: bool) -> Vec[Option[R]] { if c { return Vec[x] } return Vec.new() }
fn id(x: Option[R]) -> Option[R] { return x }
fn rv(x: Result[R, i64]) -> Vec[Result[R, i64]] { return Vec[x] }
fn gv[T](x: T) -> Vec[T] { return Vec[x] }
struct H { k: i64 }
impl H {
  fn wv(self, x: Option[R]) -> Vec[Option[R]] { return Vec[x] }
  fn mkv(x: Option[R]) -> Vec[Option[R]] { return Vec[x] }
}
fn main() {
  println("-q1"); let a = ov(Some(mk(1))); println(f"k{a.len()}")
  println("-q2"); let b = oa(Some(mk(2))); println(f"k{b.len()}")
  println("-q7"); let o = Some(mk(3)); let c = ov(o); println(f"k{c.len()}")
  println("-w2"); let o2 = Some(mk(4)); let d = ow(o2); println(f"k{d.n}")
  println("-t2"); let o3 = Some(mk(5)); let e = ot(o3); println(f"k{e.1}")
  println("-c1"); let o4 = Some(mk(6)); let f = oc(o4, true); println(f"k{f.len()}")
  println("-c2"); let o5 = Some(mk(7)); let g = oc(o5, false); println(f"k{g.len()}")
  println("-i1"); let o6 = Some(mk(8)); let h = id(o6); println(f"k{h.is_some()}")
  println("-r2"); let o7: Result[R, i64] = Ok(mk(9)); let r = rv(o7); println(f"k{r.len()}")
  println("-m1"); let hz = H { k: 1 }; let o8 = Some(mk(10)); let m = hz.wv(o8); println(f"k{m.len()}")
  println("-m2"); let o9 = Some(mk(11)); let n = H.mkv(o9); println(f"k{n.len()}")
  println("-g1"); let p = Some(mk(12)); let q = gv(p); println(f"k{q.len()}")
  println("-g2"); let s = gv(Some(mk(13))); println(f"k{s.len()}")
  println("-l1"); for i in 0..2 { let u = Some(mk(20 + i)); let v = ov(u); println(f"k{v.len()}") }
  println("-n1"); let z: Option[R] = None; let y = ov(z); println(f"k{y.len()}")
}
"#);
    assert_eq!(out, "-q1\nk1\ndR1\n-q2\nk1\ndR2\n-q7\nk1\ndR3\n-w2\nk1\ndR4\n-t2\nk1\ndR5\n-c1\nk1\ndR6\n-c2\ndR7\nk0\n-i1\nktrue\ndR8\n-r2\nk1\ndR9\n-m1\nk1\ndR10\n-m2\nk1\ndR11\n-g1\nk1\ndR12\n-g2\nk1\ndR13\n-l1\nk1\ndR20\nk1\ndR21\n-n1\nk1\n");
}
