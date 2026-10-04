//! B-2026-10-04-74 — a named local moved into a field, `Vec` element or tuple
//! element on one path only is owned once on each path.

use super::*;

/// `let n = mk(2); if c { w.r = n; }` retracted `n`'s drop statically
/// compiled, so the path that never stored lost its body and leaked its
/// `String`, while `--interp` ran `n`'s body at its own death AND with the
/// place on the storing path. Covers the field, `Vec` element and tuple element
/// spellings on both paths, an `if`/`else`, a `match` arm, a nested `if`, a
/// store on one pass of a loop, and a bare block.
#[test]
fn asan_named_local_moved_into_place_on_one_path_owned_once() {
    assert_clean_asan_run(
        r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
struct W { r: R, k: i64 }
fn field(c: bool) { let mut w = W { r: mk(1), k: 0 }; let n = mk(2); if c { w.r = n; } println(f"a{w.k}") }
fn elem(c: bool) { let mut v: Vec[R] = Vec.new(); v.push(mk(3)); let n = mk(4); if c { v[0] = n; } println(f"b{v.len()}") }
fn tup(c: bool) { let mut t: (R, i64) = (mk(5), 0); let n = mk(6); if c { t.0 = n; } println(f"c{t.1}") }
fn ifelse(c: bool) { let mut w = W { r: mk(7), k: 0 }; let n = mk(8); if c { w.r = n; } else { println("no") } println(f"d{w.k}") }
fn arm(k: i64) { let mut v: Vec[R] = Vec.new(); v.push(mk(9)); let n = mk(10); match k { 1 => { v[0] = n; } _ => { println("m") } } println(f"e{v.len()}") }
fn nest(c: bool, d: bool) { let mut w = W { r: mk(11), k: 0 }; let n = mk(12); if c { if d { w.r = n; } } println(f"f{w.r.id}") }
fn inloop() { let mut w = W { r: mk(13), k: 0 }; for i in 0..3 { let n = mk(20 + i); if i == 1 { w.r = n; } } println(f"g{w.r.id}") }
fn bare() { let mut t: (R, i64) = (mk(14), 0); let n = mk(15); { t.0 = n; } println(f"h{t.0.id}") }
fn main() {
    field(true); field(false); elem(true); elem(false); tup(true); tup(false);
    ifelse(true); ifelse(false); arm(1); arm(2);
    nest(true, true); nest(true, false); nest(false, true);
    inloop(); bare();
    println("end");
}
"#,
        &[
            "dR1", "a0", "dR2", "dR2", "a0", "dR1", "dR3", "b1", "dR4", "dR4", "b1", "dR3", "dR5",
            "c0", "dR6", "dR6", "c0", "dR5", "dR7", "d0", "dR8", "no", "dR8", "d0", "dR7", "dR9",
            "e1", "dR10", "m", "dR10", "e1", "dR9", "dR11", "f12", "dR12", "dR12", "f11", "dR11",
            "dR12", "f11", "dR11", "dR20", "dR13", "dR22", "g21", "dR21", "dR14", "h15", "dR15",
            "end",
        ],
        "named_local_moved_into_place_on_one_path",
    );
}
