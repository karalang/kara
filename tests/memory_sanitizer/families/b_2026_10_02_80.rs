//! B-2026-10-02-80: a `let`-bound match over a by-value `Option`/`Result` param's whole payload runs the payload's bodies once

use super::*;

/// B-2026-10-02-80: the arm's binding is a view of a payload whose `Drop` bodies the
/// param's own walk runs, and the `let` binding registers a walk of its own, so the
/// compiled surfaces ran every body twice; the nested-`if` and rebind spellings also
/// double-freed. The `let` site now stands the param's walk down on the path that
/// hands the view to it, as a `return` already did.
#[test]
fn asan_let_bound_view_of_callee_owned_param_payload_runs_bodies_once() {
    assert_clean_asan_run(
        r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}" }; }
fn z() -> Array[R, 2] { return [mk(0), mk(0)]; }
fn o1(x: Option[Array[R, 2]]) -> i64 { let k: Array[R, 2] = match x { Some(v) => v, None => z() }; println("mid"); return k[0].id; }
fn o2(x: Option[Array[R, 2]]) -> i64 { let k: Array[R, 2] = if let Some(v) = x { v } else { z() }; println("mid"); return k[0].id; }
fn o3(x: Option[Array[R, 2]]) -> i64 { let k: Array[R, 2] = match x { Some(v) => { println("in"); v }, None => z() }; println("mid"); return k[0].id; }
fn o4(x: Option[Array[R, 2]]) -> Array[R, 2] { let k: Array[R, 2] = match x { Some(v) => v, None => z() }; println("mid"); return k; }
fn o5(x: Option[(R, R)]) -> i64 { let k: (R, R) = match x { Some(v) => v, None => (mk(0), mk(0)) }; println("mid"); return k.0.id; }
fn o6(x: Result[Array[R, 2], String]) -> i64 { let k: Array[R, 2] = match x { Ok(v) => v, Err(_) => z() }; println("mid"); return k[0].id; }
fn o7(x: Option[Array[R, 2]], c: bool) -> i64 { let k: Array[R, 2] = match x { Some(v) => if c { v } else { z() }, None => z() }; println("mid"); return k[0].id; }
fn o8(x: Option[Array[R, 2]]) -> i64 { let k: Array[R, 2] = match x { Some(v) => { let u = v; u }, None => z() }; println("mid"); return k[0].id; }
fn o9(x: Option[Array[R, 2]], c: bool) -> i64 { let k: Array[R, 2] = if let Some(v) = x { if c { v } else { z() } } else { z() }; println("mid"); return k[0].id; }
fn o10(x: Option[Array[R, 2]], y: Option[Array[R, 2]]) -> i64 { let k: Array[R, 2] = match x { Some(v) => v, None => match y { Some(w) => w, None => z() } }; println("mid"); return k[0].id; }
fn o11(x: Option[(R, i64)]) -> i64 { let k: (R, i64) = match x { Some(v) => v, None => (mk(0), 0) }; println("mid"); return k.1; }
fn o12(x: Option[Array[R, 2]]) -> i64 { let k: Array[R, 2] = match x { Some(v) => v, None => z() }; let m = k; println("mid"); return m[1].id; }
fn eat(a: Array[R, 2]) -> i64 { return a[1].id; }
fn main() {
  println(f"a{o1(Some([mk(1), mk(2)]))}");
  println(f"b{o2(Some([mk(3), mk(4)]))}");
  println(f"c{o3(Some([mk(5), mk(6)]))}");
  let t = o4(Some([mk(7), mk(8)]));
  println(f"d{t[0].id}");
  println(f"e{o5(Some((mk(9), mk(10))))}");
  println(f"f{o6(Ok([mk(11), mk(12)]))}");
  println(f"g{o7(Some([mk(13), mk(14)]), true)}");
  println(f"h{o8(Some([mk(15), mk(16)]))}");
  println(f"i{o9(Some([mk(17), mk(18)]), true)}");
  println(f"j{o10(None, Some([mk(19), mk(20)]))}");
  println(f"k{o11(Some((mk(21), 5)))}");
  println(f"l{o12(Some([mk(22), mk(23)]))}");
  println(f"m{o1(None)}");
  println("end");
}
"#,
        &[
            "mid", "dR1", "dR2", "a1", "mid", "dR3", "dR4", "b3", "in", "mid", "dR5", "dR6", "c5",
            "mid", "d7", "dR7", "dR8", "mid", "dR9", "dR10", "e9", "mid", "dR11", "dR12", "f11",
            "mid", "dR13", "dR14", "g13", "mid", "dR15", "dR16", "h15", "mid", "dR17", "dR18",
            "i17", "mid", "dR19", "dR20", "j19", "mid", "dR21", "k5", "mid", "dR22", "dR23", "l23",
            "mid", "dR0", "dR0", "m0", "end",
        ],
        "asan_let_bound_view_of_callee_owned_param_payload_runs_bodies_once",
    );
}
