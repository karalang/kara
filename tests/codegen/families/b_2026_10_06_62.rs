//! B-2026-10-06-62, -96, B-2026-10-05-124 — a store after a move re-arms the walk behind a per-path bit.

use super::*;

/// B-2026-10-06-62, B-2026-10-06-105, B-2026-10-05-124 — a tuple, `Vec` or
/// `Option` local moved out whole and then stored into from a DEEPER frame
/// (a loop body, an `if`, a `for`, a `while true` with `break`, a store
/// followed by an early `return`) runs exactly the bodies `--interp` runs:
/// the value a second trip displaces (`a`, `d`, `g`), nothing extra when the
/// store never ran (`a(0)`, `b(false)`, `d(0)`, `e(false)`, `f(0)`, `g(0)`),
/// and a same-frame store after a conditional one (`k`). In `m` a later
/// block's `let mut p` that moves conditionally and is then stored into runs
/// the displaced body (`dR94`) on the path that kept it, though an earlier
/// block's `p` was reassigned.
///
/// Before: the store re-armed the retracted walk statically, so on a path
/// where it never ran the moved value's body ran a second time
/// (B-2026-10-06-105, introduced by B-2026-10-05-110's fix), and a second
/// trip's displaced value ran none (B-2026-10-06-62); in `m` the earlier
/// block's reassignment made the later block's move retract on every path
/// (B-2026-10-05-124).
#[test]
fn e2e_store_after_whole_move_rearms_behind_a_flag() {
    let src = r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mkr(i: i64) -> (R, i64) { (R { id: i }, i) }
fn a(n: i64) { let mut p = mkr(1); let q = p; let mut i = 0; while i < n { p = mkr(i + 5); i = i + 1; } println(f"a{q.1}") }
fn b(c: bool) { let mut p = mkr(10); let q = p; if c { p = mkr(11); } println(f"b{q.1}") }
fn d(n: i64) { let mut v = [R { id: 20 }]; let q = v; let mut i = 0; while i < n { v = [R { id: 21 + i }]; i = i + 1; } println(f"d{q.len()}") }
fn e(c: bool) { let mut o = Some(R { id: 30 }); let q = o; if c { o = Some(R { id: 31 }); } println(f"e{q.is_some()}") }
fn f(n: i64) { let mut p = mkr(40); let q = p; for i in 0..n { if i == 1 { p = mkr(i + 45); } } println(f"f{q.1}") }
fn g(n: i64) { let mut p = mkr(50); let q = p; let mut i = 0; while true { if i >= n { break; } p = mkr(51 + i); i = i + 1; } println(f"g{q.1}") }
fn h(n: i64) { for k in 0..2 { let mut p = mkr(60 + k); let q = p; if n > k { p = mkr(65 + k); } println(f"h{q.1}") } }
fn k(c: bool) { let mut p = mkr(70); let q = p; if c { p = mkr(71); } p = mkr(72); println(f"k{q.1}{p.1}") }
fn l(c: bool) -> i64 { let mut p = mkr(80); let q = p; if c { p = mkr(81); return p.1; } q.1 }
fn m(c: bool, d: bool) {
    { let mut p = mkr(90); p = mkr(91); println(f"m{p.1}") }
    { let mut p = mkr(92); if c { let q = p; println(f"q{q.1}"); } p = mkr(93); println(f"m{p.1}") }
    { let mut p = mkr(94); if d { let q = p; println(f"q{q.1}"); } p = mkr(95); println(f"m{p.1}") }
}
fn main() {
    a(0);
    a(1);
    a(2);
    b(false);
    b(true);
    d(0);
    d(2);
    e(false);
    e(true);
    f(0);
    f(3);
    g(0);
    g(2);
    h(1);
    k(false);
    k(true);
    println(f"l{l(false)}");
    println(f"l{l(true)}");
    m(true, false);
    m(true, true);
    println("end")
}
"#;
    let want = "a1\ndR1\ndR5\na1\ndR1\ndR5\ndR6\na1\ndR1\nb10\ndR10\ndR11\nb10\ndR10\nd1\ndR20\ndR21\ndR22\nd1\ndR20\netrue\ndR30\ndR31\netrue\ndR30\nf40\ndR40\ndR46\nf40\ndR40\ng50\ndR50\ndR51\ndR52\ng50\ndR50\ndR65\nh60\ndR60\nh61\ndR61\nk7072\ndR70\ndR72\ndR71\nk7072\ndR70\ndR72\ndR80\nl80\ndR80\ndR81\nl81\ndR90\nm91\ndR91\nq92\ndR92\nm93\ndR93\ndR94\nm95\ndR95\ndR90\nm91\ndR91\nq92\ndR92\nm93\ndR93\nq94\ndR94\nm95\ndR95\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
