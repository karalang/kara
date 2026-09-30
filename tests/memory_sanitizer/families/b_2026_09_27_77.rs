//! B-2026-09-27-77 -- a struct field handed back inside a discarded tuple
//! runs its `Drop` body once.

use super::*;

/// B-2026-09-27-77 -- a named struct local handed to a free function (or a
/// method) that returns one of its fields inside a TUPLE, with the result
/// discarded. The escaped-part resolver named the whole result after the
/// field's type (`R`) and hung `karac_drop_R` off the tuple's slot beside the
/// tuple arm's own walk: `onews(w);` over `fn onews(w: Ws) -> (R, i64)` ran
/// `dR` twice and freed its `String` twice, and `(5, w.r)` ran the body over
/// the tuple's first word. Covers the field at each tuple position, a nested
/// tuple, two fields, a whole-field return, `let _ =`, a method, an owned
/// `self`, the bound spelling and a temporary argument.
#[test]
fn asan_struct_field_handed_back_in_discarded_tuple_runs_its_body_once() {
    assert_clean_asan_run(
        r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}" }; }
struct Ws { r: R, n: i64 }
struct W3 { q: R, r: R }
fn one(w: W3) -> (R, i64) { return (w.r, 5); }
fn onews(w: Ws) -> (R, i64) { return (w.r, 5); }
fn back(w: Ws) -> (i64, R) { return (5, w.r); }
fn nest(w: Ws) -> ((R, i64), i64) { return ((w.r, 1), 2); }
fn both(w: W3) -> (R, R) { return (w.q, w.r); }
fn whole(w: Ws) -> R { return w.r; }
struct H { a: i64 }
impl H { fn mf(ref self, w: Ws) -> (R, i64) { return (w.r, 6); } }
impl Ws { fn pair(self) -> (R, i64) { return (self.r, 7); } }
fn main() {
  println("-c1"); let w1 = W3 { q: mk(1), r: mk(2) }; one(w1);
  println("-c2"); let w2 = Ws { r: mk(3), n: 0 }; onews(w2);
  println("-c3"); let w3 = Ws { r: mk(4), n: 0 }; back(w3);
  println("-c4"); let w4 = Ws { r: mk(5), n: 0 }; nest(w4);
  println("-c5"); let w5 = W3 { q: mk(6), r: mk(7) }; both(w5);
  println("-c6"); let w6 = Ws { r: mk(8), n: 0 }; whole(w6);
  println("-c7"); let w7 = Ws { r: mk(9), n: 0 }; let _ = onews(w7);
  println("-c8"); let h = H { a: 1 }; let w8 = Ws { r: mk(10), n: 0 }; h.mf(w8);
  println("-c9"); let w9 = Ws { r: mk(11), n: 0 }; w9.pair();
  println("-c10"); let w10 = Ws { r: mk(12), n: 0 }; let p = onews(w10); println(f"p{p.0.id}{p.1}");
  println("-c11"); onews(Ws { r: mk(13), n: 0 });
  println("end")
}
"#,
        &[
            "-c1", "dR2", "dR1", "-c2", "dR3", "-c3", "dR4", "-c4", "dR5", "-c5", "dR6", "dR7",
            "-c6", "dR8", "-c7", "dR9", "-c8", "dR10", "-c9", "dR11", "-c10", "p125", "dR12",
            "-c11", "dR13", "end",
        ],
        "struct_field_handed_back_in_discarded_tuple_runs_its_body_once",
    );
}
