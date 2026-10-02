//! B-2026-10-02-50: a `let`-bound match or if-let over a boxed `Array` payload frees it once

use super::*;

/// B-2026-10-02-50: `let k: Array[R, 2] = match x { EArr.A(v) => v, EArr.B => z() }`
/// over a by-value user-enum param, where `R` runs a `Drop` body, double-freed the
/// elements on the JIT and at -O0: the box's drop and `k` both freed them. The arm
/// tail disarmed the box only when the match was the frame's result, because a
/// call argument (`eat(match x { .. })`, cell `h`, kept here as the guard) leaves
/// the bodies with the box. A `let` binding registers its own element walk, so it
/// takes the bodies too. Covers the `if let` spelling, a rebind arm tail, a local
/// scrutinee, the `B` edge, a `let` the function then returns, and the `String`
/// array, which never doubled.
#[test]
fn asan_let_bound_branch_of_boxed_array_payload_freed_once() {
    assert_clean_asan_run(
        r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}" }; }
enum EArr { A(Array[R, 2]), B }
enum EStr { A(Array[String, 2]), B }
fn z() -> Array[R, 2] { return [mk(0), mk(0)]; }
fn zs() -> Array[String, 2] { return ["a", "b"]; }
fn n10(x: EArr) -> i64 { let k: Array[R, 2] = match x { EArr.A(v) => v, EArr.B => z() }; println("mid"); return k[0].id; }
fn n11(x: EStr) -> i64 { let k: Array[String, 2] = match x { EStr.A(v) => v, EStr.B => zs() }; return k[0].len() as i64; }
fn i10(x: EArr) -> i64 { let k: Array[R, 2] = if let EArr.A(v) = x { v } else { z() }; println("mid"); return k[0].id; }
fn r10(x: EArr) -> i64 { let k: Array[R, 2] = match x { EArr.A(v) => { let u = v; u } EArr.B => z() }; println("mid"); return k[1].id; }
fn l10() -> i64 { let e = EArr.A([mk(5), mk(6)]); let k: Array[R, 2] = match e { EArr.A(v) => v, EArr.B => z() }; println("mid"); return k[0].id; }
fn b10(x: EArr) -> i64 { let k: Array[R, 2] = match x { EArr.A(v) => v, EArr.B => z() }; println("mid"); return k[0].id; }
fn t10(x: EArr) -> Array[R, 2] { let k: Array[R, 2] = match x { EArr.A(v) => v, EArr.B => z() }; println("mid"); return k; }
fn eat(a: Array[R, 2]) -> i64 { return a[1].id; }
fn c10(x: EArr) -> i64 { let k = eat(match x { EArr.A(v) => v, EArr.B => z() }); println("mid"); return k; }

fn main() {
    println(f"a{n10(EArr.A([mk(1), mk(2)]))}");
    println(f"b{n11(EStr.A([f"x{1}", f"y{22}"]))}");
    println(f"c{i10(EArr.A([mk(3), mk(4)]))}");
    println(f"d{r10(EArr.A([mk(5), mk(6)]))}");
    println(f"e{l10()}");
    println(f"f{b10(EArr.B)}");
    let t = t10(EArr.A([mk(7), mk(8)])); println(f"g{t[0].id}");
    println(f"h{c10(EArr.A([mk(9), mk(10)]))}");
    println("end");
}
"#,
        &[
            "mid", "dR1", "dR2", "a1", "b2", "mid", "dR3", "dR4", "c3", "mid", "dR5", "dR6", "d6",
            "mid", "dR5", "dR6", "e5", "mid", "dR0", "dR0", "f0", "mid", "g7", "dR7", "dR8", "mid",
            "dR9", "dR10", "h10", "end",
        ],
        "asan_let_bound_branch_of_boxed_array_payload_freed_once",
    );
}
