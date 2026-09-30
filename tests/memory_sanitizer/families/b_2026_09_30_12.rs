//! B-2026-09-30-12: a payload-binding match or a let-mut reassignment before a hand-off inside a branch.

use super::*;

/// B-2026-09-30-12 — a payload-binding `match` on a `let` local, nested in a
/// branch that may not run, took the local's payload body off every path: the
/// arm's mask swapped the local's walker for a skipping one in all frames, so
/// `let t = E.A(mks(5)); if c { let y = match t { E.A(s) => s.id, .. }; .. }`
/// at `c = false` freed the payload with no `dS5` on either compiled level. An
/// arm that takes every field of its variant now clears the local's per-path
/// bit instead, like B-2026-09-29-116's `let` move.
///
/// The `Option` cell of the codegen twin (`a7`) is left out here: it leaked
/// 2 B per call at `c = false` before and after this row's fix. That leak was
/// B-2026-09-30-48, and `families/b_2026_09_30_48.rs` covers the cell.
#[test]
fn asan_payload_binding_match_in_untaken_branch_leaves_the_local_its_body() {
    assert_clean_asan_run(
        r#"struct S { id: i64, s: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mks(i: i64) -> S { return S { id: i, s: f"s{i}" } }
enum E { A(S), B(i64) }
fn a5(c: bool) -> i64 {
  let t = E.A(mks(5));
  if c { let y = match t { E.A(s) => s.id, E.B(n) => n }; return y }
  return 0
}
fn a6(c: bool) -> i64 {
  let t = E.A(mks(6));
  if c { match t { E.A(s) => { let k = s; println(f"k{k.id}") } E.B(n) => {} } }
  return 0
}
fn a8(c: bool) -> i64 {
  let t = E.A(mks(8));
  match t { E.A(s) => { if c { let k = s; println(f"k{k.id}") } } E.B(n) => {} }
  return 0
}
fn main() {
  println(f"r {a5(false)}"); println(f"r {a5(true)}")
  println(f"r {a6(false)}"); println(f"r {a6(true)}")
  println(f"r {a8(false)}"); println(f"r {a8(true)}")
}
"#,
        &[
            "dS5", "r 0", "dS5", "r 5", "dS6", "r 0", "k6", "dS6", "r 0", "dS8", "r 0", "k8",
            "dS8", "r 0",
        ],
        "asan_payload_binding_match_in_untaken_branch_leaves_the_local_its_body",
    );
}

/// B-2026-09-30-12 — the same arm hand-off across shapes: both fields of a
/// two-field variant, a `let mut` local reassigned after the branch, a loop
/// whose top reassigns what a branch then matches, a doubly nested branch, an
/// `else` arm, and an arm whose value leaves the match.
#[test]
fn asan_payload_binding_match_in_a_branch_across_shapes() {
    assert_clean_asan_run(
        r#"struct S { id: i64, s: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mks(i: i64) -> S { return S { id: i, s: f"s{i}" } }
enum E { A(S), B(i64) }
enum F { P(S, S), Q(S) }
fn c2(c: bool) -> i64 {
  let t = F.P(mks(20), mks(21));
  if c { match t { F.P(x, y) => { let k = x; println(f"k{k.id}") } F.Q(q) => {} } }
  return 0
}
fn c3(c: bool) -> i64 {
  let mut t = E.A(mks(30));
  if c { match t { E.A(s) => { let k = s; println("k") } E.B(n) => {} } }
  t = E.A(mks(31));
  return 0
}
fn c4(c: bool) -> i64 {
  let mut t = E.B(0);
  let mut i = 0;
  while i < 2 { t = E.A(mks(40 + i)); if c and i == 0 { match t { E.A(s) => { let k = s; println("k") } E.B(n) => {} } }; i = i + 1; }
  return 0
}
fn c5(c: bool, d: bool) -> i64 {
  let t = E.A(mks(50));
  if c { if d { match t { E.A(s) => { println(f"k{s.id}") } E.B(n) => {} } } }
  return 0
}
fn c6(c: bool) -> i64 {
  let t = E.A(mks(60));
  if c { match t { E.A(s) => { let k = s; println("k") } E.B(n) => {} } } else { println("e") }
  return 0
}
fn c7(c: bool) -> i64 {
  let t = E.A(mks(70));
  if c { let r = match t { E.A(s) => s, E.B(n) => mks(n) }; println(f"r{r.id}") }
  return 0
}
fn main() {
  println(f"r {c2(false)}"); println(f"r {c2(true)}")
  println(f"r {c3(false)}"); println(f"r {c3(true)}")
  println(f"r {c4(false)}"); println(f"r {c4(true)}")
  println(f"r {c5(false, true)}"); println(f"r {c5(true, false)}"); println(f"r {c5(true, true)}")
  println(f"r {c6(false)}"); println(f"r {c6(true)}")
  println(f"r {c7(false)}"); println(f"r {c7(true)}")
}
"#,
        &[
            "dS21", "dS20", "r 0", "k20", "dS20", "dS21", "r 0", "dS30", "dS31", "r 0", "dS30",
            "k", "dS31", "r 0", "dS40", "dS41", "r 0", "dS40", "k", "dS41", "r 0", "dS50", "r 0",
            "dS50", "r 0", "k50", "dS50", "r 0", "e", "dS60", "r 0", "dS60", "k", "r 0", "dS70",
            "r 0", "r70", "dS70", "r 0",
        ],
        "asan_payload_binding_match_in_a_branch_across_shapes",
    );
}

/// B-2026-09-30-12 — a `let mut` enum local REASSIGNED before a `let` move in
/// an untaken branch (straight-line, and at the top of a loop) lost the payload
/// body on the path that never moved it: the reassignment ran before any
/// hand-off made the per-path bit, so the hand-off fell back to the all-paths
/// retraction. The reassignment now makes the bit itself.
#[test]
fn asan_mut_enum_local_reassigned_before_a_branch_move_keeps_its_body() {
    assert_clean_asan_run(
        r#"struct S { id: i64, s: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mks(i: i64) -> S { return S { id: i, s: f"s{i}" } }
enum E { A(S), B(i64) }
fn r4(c: bool) -> i64 {
  let mut t = E.A(mks(10));
  let mut i = 0;
  while i < 2 {
    t = E.A(mks(11 + i));
    if c { let y = t; println("y") }
    i = i + 1;
  };
  return 0
}
fn r5(c: bool) -> i64 {
  let mut t = E.A(mks(20));
  t = E.A(mks(21));
  if c { let y = t; println("y") }
  return 0
}
fn r6(c: bool) -> i64 {
  let mut t = E.A(mks(30));
  let mut i = 0;
  while i < 2 {
    if c { let y = t; println("y") }
    t = E.A(mks(31 + i));
    i = i + 1;
  };
  return 0
}
fn main() {
  println(f"r {r4(false)}"); println(f"r {r4(true)}")
  println(f"r {r5(false)}"); println(f"r {r5(true)}")
  println(f"r {r6(false)}"); println(f"r {r6(true)}")
}
"#,
        &[
            "dS10", "dS11", "dS12", "r 0", "dS10", "dS11", "y", "dS12", "y", "r 0", "dS20", "dS21",
            "r 0", "dS20", "dS21", "y", "r 0", "dS30", "dS31", "dS32", "r 0", "dS30", "y", "dS31",
            "y", "dS32", "r 0",
        ],
        "asan_mut_enum_local_reassigned_before_a_branch_move_keeps_its_body",
    );
}

/// B-2026-09-30-12 — the `Option` spelling of the reassign-then-branch-move
/// shapes above.
#[test]
fn asan_mut_option_local_reassigned_before_a_branch_move_keeps_its_body() {
    assert_clean_asan_run(
        r#"struct S { id: i64, s: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mks(i: i64) -> S { return S { id: i, s: f"s{i}" } }
enum E { A(S), B(i64) }
fn r4(c: bool) -> i64 {
  let mut t: Option[S] = Some(mks(10));
  let mut i = 0;
  while i < 2 {
    t = Some(mks(11 + i));
    if c { let y = t; println("y") }
    i = i + 1;
  };
  return 0
}
fn r5(c: bool) -> i64 {
  let mut t: Option[S] = Some(mks(20));
  t = Some(mks(21));
  if c { let y = t; println("y") }
  return 0
}
fn r6(c: bool) -> i64 {
  let mut t: Option[S] = Some(mks(30));
  let mut i = 0;
  while i < 2 {
    if c { let y = t; println("y") }
    t = Some(mks(31 + i));
    i = i + 1;
  };
  return 0
}
fn main() {
  println(f"r {r4(false)}"); println(f"r {r4(true)}")
  println(f"r {r5(false)}"); println(f"r {r5(true)}")
  println(f"r {r6(false)}"); println(f"r {r6(true)}")
}
"#,
        &[
            "dS10", "dS11", "dS12", "r 0", "dS10", "dS11", "y", "dS12", "y", "r 0", "dS20", "dS21",
            "r 0", "dS20", "dS21", "y", "r 0", "dS30", "dS31", "dS32", "r 0", "dS30", "y", "dS31",
            "y", "dS32", "r 0",
        ],
        "asan_mut_option_local_reassigned_before_a_branch_move_keeps_its_body",
    );
}
