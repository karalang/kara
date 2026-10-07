//! B-2026-10-01-45: reassigned `let mut` collection of a by-value param

use super::*;

/// B-2026-10-01-45: a `let mut` collection local built from a by-value param
/// (`let mut v = Vec[x]`), reassigned whole on some paths and then returned, ran
/// the param's `Drop` body in the caller AND over the returned value on the path
/// that did not reassign it. The caller now stands down on every path and the
/// local owns the param, so the reassignment that displaces it runs its body
/// there. Cells cover a loop, an early return, a mixed and a two-param literal,
/// a struct with a `shared` field, a method, and a forwarding wrapper.
#[test]
fn asan_param_collection_reassigned_on_one_path_runs_the_param_body_once() {
    assert_clean_asan_run(
        r#"shared struct Nd { v: i64 }
struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}" }; }
struct Q { id: i64, n: Nd }
impl Drop for Q { fn drop(mut ref self) { println(f"dQ{self.id}") } }
fn mq(i: i64) -> Q { return Q { id: i, n: Nd { v: i } }; }
fn f8(x: R, c: bool) -> Vec[R] { let mut v = Vec[x]; if c { v = Vec[mk(8)]; } return v }
fn g8(x: R, c: bool) -> Vec[R] { let mut v = Vec[x]; if c { v = Vec[mk(8)]; println("re"); } v.push(mk(9)); return v }
fn h8(x: R, n: i64) -> Vec[R] { let mut v = Vec[x]; let mut i = 0; while i < n { v = Vec[mk(20 + i)]; i = i + 1; } return v }
fn s8(x: R, c: bool) -> Vec[R] { let mut v = Vec[x]; if c { v = Vec[mk(8)]; return v } println("tail"); return v }
fn fm(x: R, c: bool) -> Vec[R] { let mut v = Vec[x, mk(30)]; if c { v = Vec[mk(8)]; } return v }
fn f2(x: R, y: R, c: bool) -> Vec[R] { let mut v = Vec[x, y]; if c { v = Vec[mk(8)]; } return v }
fn fq(x: Q, c: bool) -> Vec[Q] { let mut v = Vec[x]; if c { v = Vec[mq(8)]; } return v }
fn fe(x: R, c: bool) -> Vec[R] { let mut v = Vec[x]; if c { v = Vec.new(); } return v }
fn w8(x: R, c: bool) -> Vec[R] { return f8(x, c) }
struct S { k: i64 }
impl S { fn m8(ref self, x: R, c: bool) -> Vec[R] { let mut v = Vec[x]; if c { v = Vec[mk(8)]; } return v } }
fn main() {
  println("ft"); let a = f8(mk(1), true); println(f"k{a.len()}");
  println("ff"); let b = f8(mk(2), false); println(f"k{b.len()}");
  println("gt"); let c = g8(mk(3), true); println(f"k{c.len()}");
  println("gf"); let d = g8(mk(4), false); println(f"k{d.len()}");
  println("h2"); let e = h8(mk(5), 2); println(f"k{e.len()}");
  println("st"); let g = s8(mk(6), true); println(f"k{g.len()}");
  println("sf"); let h = s8(mk(7), false); println(f"k{h.len()}");
  println("mf"); let i = fm(mk(10), false); println(f"k{i.len()}");
  println("2t"); let j = f2(mk(11), mk(12), true); println(f"k{j.len()}");
  println("qf"); let k = fq(mq(13), false); println(f"k{k.len()}");
  println("qt"); let l = fq(mq(14), true); println(f"k{l.len()}");
  println("ef"); let m = fe(mk(15), false); println(f"k{m.len()}");
  println("wt"); let n = w8(mk(16), true); println(f"k{n.len()}");
  println("wf"); let o = w8(mk(17), false); println(f"k{o.len()}");
  let s = S { k: 1 };
  println("mt"); let p = s.m8(mk(18), true); println(f"k{p.len()}");
  println("mf2"); let q = s.m8(mk(19), false); println(f"k{q.len()}");
  let w = mk(20); println("nf"); let r = f8(w, false); println(f"k{r.len()}");
  println("end")
}
"#,
        &[
            "ft", "dR1", "k1", "dR8", "ff", "k1", "dR2", "gt", "dR3", "re", "k2", "dR8", "dR9",
            "gf", "k2", "dR4", "dR9", "h2", "dR5", "dR20", "k1", "dR21", "st", "dR6", "k1", "dR8",
            "sf", "tail", "k1", "dR7", "mf", "k2", "dR10", "dR30", "2t", "dR11", "dR12", "k1",
            "dR8", "qf", "k1", "dQ13", "qt", "dQ14", "k1", "dQ8", "ef", "k1", "dR15", "wt", "dR16",
            "k1", "dR8", "wf", "k1", "dR17", "mt", "dR18", "k1", "dR8", "mf2", "k1", "dR19", "nf",
            "k1", "dR20", "end",
        ],
        "asan_param_collection_reassigned_on_one_path_runs_the_param_body_once",
    );
}
