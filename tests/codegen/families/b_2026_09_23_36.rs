//! B-2026-09-23-36 -- a SHADOWED local `Array` whose generations are not all
//! handed back together (an older generation returned on one exit, a later one
//! on only some, a nested-block shadow over a returned outer local) has one
//! owner per generation on every path.

use super::*;

/// B-2026-09-23-36 — the hand-back class is decided per GENERATION of a
/// shadowed name (the `let` it comes from), and a later generation's per-path
/// drop flag is its own bit. Before: `free(): double free detected in tcache 2`
/// on the JIT, `-O0` and `-O2` for every function here, the older generation
/// staying armed in the callee while the caller freed it too. `inner` is the
/// nested-block shadow, `two_some` has two generations each returned on one
/// exit, `wrapped` hands them back inside `Some`.
#[test]
fn e2e_shadowed_array_generations_handed_back_on_different_exits_have_one_owner() {
    let Some(out) = run_program(
        r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn older(c: bool, b: i64) -> Array[R, 2] { let x: Array[R, 2] = [mkr(b+1), mkr(b+2)]; if c { return x }; let x: Array[R, 2] = [mkr(b+8), mkr(b+9)]; println("dies"); return x }
fn some_exits(c: bool, b: i64) -> Array[R, 2] { let x: Array[R, 2] = [mkr(b+1), mkr(b+2)]; let x: Array[R, 2] = [mkr(b+8), mkr(b+9)]; if c { let w: Array[R, 2] = [mkr(b+5), mkr(b+6)]; return w }; println("dies"); x }
fn inner(b: i64) -> Array[R, 2] { let x: Array[R, 2] = [mkr(b+1), mkr(b+2)]; { let x: Array[R, 2] = [mkr(b+8), mkr(b+9)]; println(f"i{x[0].id}") }; println("dies"); return x }
fn two_some(c: i64, b: i64) -> Array[R, 2] { let x: Array[R, 2] = [mkr(b+1), mkr(b+2)]; if c == 1 { return x }; let x: Array[R, 2] = [mkr(b+3), mkr(b+4)]; if c == 2 { return x }; println("dies"); let w: Array[R, 2] = [mkr(b+5), mkr(b+6)]; return w }
fn wrapped(c: bool, b: i64) -> Option[Array[R, 2]] { let x: Array[R, 2] = [mkr(b+1), mkr(b+2)]; if c { return Some(x) }; let x: Array[R, 2] = [mkr(b+8), mkr(b+9)]; println("dies"); Some(x) }
fn main() {
    let a = older(true, 10); println(f"a{a[0].id}");
    let b = older(false, 20); println(f"b{b[0].id}");
    let c = some_exits(true, 30); println(f"c{c[0].id}");
    let d = some_exits(false, 40); println(f"q{d[0].id}");
    let e = inner(50); println(f"e{e[0].id}");
    for k in 1..4 { let t = two_some(k, 100 * k); println(f"t{t[0].id}") };
    match wrapped(true, 60) { Some(v) => println(f"w{v[0].id}"), None => println("n") };
    match wrapped(false, 70) { Some(v) => println(f"w{v[0].id}"), None => println("n") };
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "a11\nd11\nd12\ndies\nd21\nd22\nb28\nd28\nd29\nd38\nd39\nd31\nd32\nc35\nd35\nd36\ndies\nd41\nd42\nq48\nd48\nd49\ni58\nd58\nd59\ndies\ne51\nd51\nd52\nt101\nd101\nd102\nd201\nd202\nt203\nd203\nd204\nd301\nd302\nd303\nd304\ndies\nt305\nd305\nd306\nw61\nd61\nd62\ndies\nd71\nd72\nw78\nd78\nd79\nend\n", "got:\n{out}");
}
