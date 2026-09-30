//! B-2026-09-30-95: a value moved into a literal inside a branch keeps its Drop on the other path.

use super::*;

/// B-2026-09-30-95 — a value moved into a struct, tuple or array literal
/// bound INSIDE a branch keeps its body and memory on the path that never
/// built the literal. The compiled let-site retracted the source's drop on
/// every path, so `let a = mk(8); if f { let x = (a, 1); .. }` with `f`
/// false ran no body and leaked the `String` when compiled, while the
/// interpreter, per path by construction, ran it. Covers a by-value param
/// returned from inside the branch, a local that dies there, and a loop.
#[test]
fn asan_value_moved_into_a_literal_inside_a_branch_keeps_its_drop_on_the_other_path() {
    assert_clean_asan_run(
        r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}" }; }
struct P { r: R, n: i64 }
fn ps(a: R, f: bool) -> P { if f { let x = P { r: a, n: 1 }; return x; } return P { r: mk(92), n: 1 }; }
fn pt(a: R, f: bool) -> (R, i64) { if f { let x = (a, 1); return x; } return (mk(93), 1); }
fn pv(a: R, f: bool) -> Vec[R] { if f { let x = [a]; return x; } return [mk(94)]; }
fn ls(k: i64, f: bool) { let a = mk(k); if f { let x = P { r: a, n: 1 }; println(f"in{x.n}"); } println("out") }
fn lt(k: i64, f: bool) { let a = mk(k); if f { let x = (a, 1); println(f"in{x.1}"); } println("out") }
fn lv(k: i64, f: bool) { let a = mk(k); if f { let x: Vec[R] = [a]; println(f"in{x.len()}"); } println("out") }
fn lw(n: i64) { let mut i = 0; while i < n { let a = mk(30 + i); if i == 1 { let x = (a, i); println(f"w{x.1}"); } i = i + 1; } println("wend") }
fn main() {
  let a1 = ps(mk(1), true); println(f"r{a1.n}")
  let a2 = ps(mk(2), false); println(f"r{a2.n}")
  let b1 = pt(mk(3), true); println(f"r{b1.1}")
  let b2 = pt(mk(4), false); println(f"r{b2.1}")
  let c1 = pv(mk(5), true); println(f"r{c1.len()}")
  let c2 = pv(mk(6), false); println(f"r{c2.len()}")
  ls(7, true); ls(8, false)
  lt(9, true); lt(10, false)
  lv(11, true); lv(12, false)
  lw(3)
  println("end")
}
"#,
        &[
            "r1", "dR1", "dR2", "r1", "dR92", "r1", "dR3", "dR4", "r1", "dR93", "r1", "dR5", "dR6",
            "r1", "dR94", "in1", "dR7", "out", "dR8", "out", "in1", "dR9", "out", "dR10", "out",
            "in1", "dR11", "out", "dR12", "out", "dR30", "w1", "dR31", "dR32", "wend", "end",
        ],
        "asan_value_moved_into_a_literal_inside_a_branch_keeps_its_drop_on_the_other_path",
    );
}
