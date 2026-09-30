//! B-2026-09-30-31 -- a discarded call result whose elements run a `Drop`
//! but own no heap runs each element's body once on every backend.

use super::*;

/// B-2026-09-30-31 — `mka();` over `fn mka() -> Array[W1, 1]` with
/// `struct W1 { v: i64 }` printed `end` alone on every compiled surface
/// against `--interp`'s `dW1_40 end`: the discard arms registered the bodies
/// only after the MEMORY walk claimed the value, and that walk declines an
/// element with nothing to free. Covers the statement and `let _` spellings,
/// tuple / nested-tuple / `Array` returns, a passthrough of a named local and
/// of a literal, methods, a discarded `if` / `match` whose
/// arms are calls, and a loop.
#[test]
fn e2e_discarded_heap_free_drop_aggregate_runs_bodies() {
    let src = r#"struct W1 { v: i64 }
impl Drop for W1 { fn drop(mut ref self) { println(f"dW1_{self.v}") } }
fn mka() -> Array[W1, 1] { return [W1 { v: 40 }] }
fn mkv() -> Vec[W1] { return [W1 { v: 41 }] }
fn mkt() -> (W1, i64) { return (W1 { v: 42 }, 7) }
fn mk2() -> ((W1, W1), i64) { return ((W1 { v: 43 }, W1 { v: 44 }), 1) }
fn pa(x: Array[W1, 1]) -> Array[W1, 1] { return x }
fn pt(x: (W1, i64)) -> (W1, i64) { return x }
struct K { n: i64 }
impl K { fn ma(ref self) -> Array[W1, 1] { return [W1 { v: 45 }] } fn mt(ref self) -> (W1, i64) { return (W1 { v: 46 }, 1) } }
fn main() {
    mka();
    println("a")
    mkt();
    println("b")
    let _ = mka();
    println("c")
    let _ = mkt();
    println("d")
    mk2();
    println("e")
    let a: Array[W1, 1] = [W1 { v: 1 }];
    pa(a);
    println("f")
    let t = (W1 { v: 2 }, 3);
    pt(t);
    println("g")
    pa([W1 { v: 4 }]);
    println("h")
    let k = K { n: 0 };
    k.ma();
    k.mt();
    println("j")
    let c = true;
    if c { mka() } else { mka() };
    println("k")
    let m = 2;
    match m { 1 => mkt(), _ => mkt() };
    println("l")
    let mut i = 0;
    while i < 2 { mka(); i = i + 1; }
    println("end")
}
"#;
    let want = "dW1_40\na\ndW1_42\nb\ndW1_40\nc\ndW1_42\nd\ndW1_43\ndW1_44\ne\ndW1_1\nf\ndW1_2\ng\ndW1_4\nh\ndW1_45\ndW1_46\nj\ndW1_40\nk\ndW1_42\nl\ndW1_40\ndW1_40\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
