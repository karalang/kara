//! B-2026-10-05-107: a by-value param destructured out of a tuple literal runs its body once

use super::*;

/// B-2026-10-05-107 — a by-value param destructured out of a tuple literal
/// (`let (v, n) = (a, 1)`), directly or out of an `if` / `match` whose leaves
/// are such literals, and a generic `T` param selected through a local (`let v
/// = if c { a } else { b }; v`). The hand-back analysis read every destructured
/// name as a PART of the param, so the caller ran the returned value's `Drop`
/// body beside the result's (`_10`..`_80`). The leaf the callee did NOT return
/// ran its body twice as well: once at its own death inside the callee, once in
/// the caller (`_90`..`_170`, `_1`, `_190`, `_200`). Each value now runs its
/// body once.
#[test]
fn interp_param_destructured_out_of_a_tuple_literal_runs_its_body_once() {
    let out = run(r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
fn mk(i: i64) -> R { R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
enum E { A(R), B }
fn eat(r: R) { println(f"eat{r.id}") }
fn gsel[T](a: T, b: T, c: bool) -> T { let (v, n) = if c { (a, 1) } else { (b, 2) }; v }
fn csel(a: R, b: R, c: bool) -> R { let (v, n) = if c { (a, 1) } else { (b, 2) }; v }
fn gtup[T](a: T, b: T) -> T { let (v, n) = (a, 1); v }
fn ctup(a: R, b: R) -> R { let (v, n) = (a, 1); v }
fn gif[T](a: T, b: T, c: bool) -> T { let v = if c { a } else { b }; v }
fn gmat[T](a: T, b: T, c: bool) -> T { let v = match c { true => a, false => b }; v }
fn both(a: R, b: R) -> R { let (v, w) = (a, b); v }
fn none(a: R, b: R) -> i64 { let (v, w) = (a, b); println("in"); 3 }
fn eatw(a: R, b: R) -> R { let (v, w) = (a, b); eat(w); v }
fn swap(a: R, b: R) -> (R, R) { let (v, w) = (a, b); (w, v) }
fn rebw(a: R, b: R) -> R { let (v, w) = (a, b); let z = w; v }
fn cret(a: R, c: bool) -> R { let (v, n) = (a, 1); if c { return v } mk(9) }
fn gw[T](a: T, b: T) -> T { let (v, w) = (a, b); w }
fn vecs(a: Vec[R], b: Vec[R]) -> Vec[R] { let (v, w) = (a, b); v }
fn enums(a: E, b: E) -> E { let (v, w) = (a, b); v }
struct K { z: i64 }
impl K { fn m(ref self, a: R, b: R) -> R { let (v, w) = (a, b); v } }
fn main() {
    let x1 = gsel(mk(10), mk(11), true); println(f"_{x1.id}");
    let x2 = gsel(mk(20), mk(21), false); println(f"_{x2.id}");
    let x3 = csel(mk(30), mk(31), true); println(f"_{x3.id}");
    let x4 = csel(mk(40), mk(41), false); println(f"_{x4.id}");
    let x5 = gtup(mk(50), mk(51)); println(f"_{x5.id}");
    let x6 = ctup(mk(60), mk(61)); println(f"_{x6.id}");
    let x7 = gif(mk(70), mk(71), false); println(f"_{x7.id}");
    let x8 = gmat(mk(80), mk(81), true); println(f"_{x8.id}");
    let x9 = both(mk(90), mk(91)); println(f"_{x9.id}");
    let n10 = none(mk(100), mk(101)); println(f"_{n10}");
    let x11 = eatw(mk(110), mk(111)); println(f"_{x11.id}");
    let x12 = swap(mk(120), mk(121)); println(f"_{x12.0.id}");
    let x13 = rebw(mk(130), mk(131)); println(f"_{x13.id}");
    let x14 = cret(mk(140), true); println(f"_{x14.id}");
    let x15 = cret(mk(150), false); println(f"_{x15.id}");
    let x16 = gw(mk(160), mk(161)); println(f"_{x16.id}");
    let p = mk(170); let q = mk(171); let x17 = both(p, q); println(f"_{x17.id}");
    let x18 = vecs(Vec[mk(180)], Vec[mk(181)]); println(f"_{x18.len()}");
    let x19 = enums(E.A(mk(190)), E.A(mk(191))); match x19 { E.A(r) => println(f"_{r.id}"), E.B => println("b") }
    let k = K { z: 0 }; let x20 = k.m(mk(200), mk(201)); println(f"_{x20.id}");
    println("end")
}
"#);
    assert_eq!(out, "d11\n_10\nd10\nd20\n_21\nd21\nd31\n_30\nd30\nd40\n_41\nd41\nd51\n_50\nd50\nd61\n_60\nd60\nd70\n_71\nd71\nd81\n_80\nd80\nd91\n_90\nd90\nin\nd101\nd100\n_3\neat111\nd111\n_110\nd110\n_121\nd121\nd120\nd131\n_130\nd130\n_140\nd140\nd150\n_9\nd9\nd160\n_161\nd161\nd171\n_170\nd170\nd181\n_1\nd180\nd191\n_190\nd190\nd201\n_200\nd200\nend\n", "got:\n{out}");
}
