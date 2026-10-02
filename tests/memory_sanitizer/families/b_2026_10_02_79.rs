//! B-2026-10-02-79: a branch tail that hands out an enclosing arm's boxed `Array` payload binding frees it once

use super::*;

/// B-2026-10-02-79: `match x { EArr.A(v) => { if c { v } else { z() } } .. }`
/// over a by-value user-enum param double-freed the payload on the JIT and at
/// -O0, as a function's tail or a `let` value: the enclosing arm's disarm reads
/// only its own tail, which is the `if` and never names `v`. Each `if` branch
/// block (and an `if let` else block) now disarms on its own edge, and a nested
/// `match` arm or `if let` then-tail naming an ENCLOSING binding disarms too.
/// Covers the then and else edges, a rebind, the call-argument guard (`f`), an
/// `else if`, an `if let` with a nested `if`, an `if let` else, a nested `match`,
/// and the `Array[String, 2]` twins, which double-freed as well.
#[test]
fn asan_branch_tail_of_enclosing_boxed_array_payload_freed_once() {
    assert_clean_asan_run(
        r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}" }; }
enum EArr { A(Array[R, 2]), B }
enum EStr { A(Array[String, 2]), B }
fn z() -> Array[R, 2] { return [mk(0), mk(0)]; }
fn zs() -> Array[String, 2] { return [f"z{0}", f"z{1}"]; }
fn eat(a: Array[R, 2]) -> i64 { return a[0].id + a[1].id; }
fn eats(a: Array[String, 2]) -> i64 { return a[0].len() + a[1].len(); }
fn s10(x: EArr, c: bool) -> i64 { let k: Array[R, 2] = match x { EArr.A(v) => { if c { v } else { z() } } EArr.B => z() }; println("mid"); return k[0].id; }
fn s11(x: EArr, c: bool) -> Array[R, 2] { match x { EArr.A(v) => { if c { v } else { z() } } EArr.B => z() } }
fn s12(x: EArr, c: bool) -> Array[R, 2] { match x { EArr.A(v) => { if c { z() } else { v } } EArr.B => z() } }
fn s13(x: EArr, c: bool) -> Array[R, 2] { match x { EArr.A(v) => { if c { let u = v; u } else { z() } } EArr.B => z() } }
fn s14(x: EArr, c: bool) -> i64 { return eat(match x { EArr.A(v) => { if c { v } else { z() } } EArr.B => z() }); }
fn s16(x: EArr, c: bool, d: bool) -> Array[R, 2] { match x { EArr.A(v) => { if c { z() } else if d { v } else { z() } } EArr.B => z() } }
fn s17(x: EArr, c: bool) -> Array[R, 2] { if let EArr.A(v) = x { if c { v } else { z() } } else { z() } }
fn t2(x: EArr, o: Option[i64]) -> Array[R, 2] { match x { EArr.A(v) => { if let Some(q) = o { z() } else { v } } EArr.B => z() } }
fn t3(x: EArr, c: bool) -> Array[R, 2] { match x { EArr.A(v) => { match c { true => v, false => z() } } EArr.B => z() } }
fn u1(x: EStr, c: bool) -> Array[String, 2] { match x { EStr.A(v) => { match c { true => v, false => zs() } } EStr.B => zs() } }
fn u2(x: EStr, c: bool) -> i64 { return eats(match x { EStr.A(v) => { if c { v } else { zs() } } EStr.B => zs() }); }
fn u3(x: EStr, c: bool) -> i64 { let k: Array[String, 2] = match x { EStr.A(v) => { if c { v } else { zs() } } EStr.B => zs() }; return k[0].len(); }
fn u4(x: EStr, o: Option[i64]) -> Array[String, 2] { match x { EStr.A(v) => { if let Some(q) = o { zs() } else { v } } EStr.B => zs() } }
fn main() {
  println(f"a{s10(EArr.A([mk(1), mk(2)]), true)}");
  let b = s11(EArr.A([mk(3), mk(4)]), true); println(f"b{b[0].id}");
  let d = s12(EArr.A([mk(7), mk(8)]), false); println(f"d{d[1].id}");
  let e = s13(EArr.A([mk(9), mk(10)]), true); println(f"e{e[0].id}");
  println(f"f{s14(EArr.A([mk(11), mk(12)]), true)}");
  let g = s16(EArr.A([mk(13), mk(14)]), false, true); println(f"g{g[0].id}");
  let h = s17(EArr.A([mk(15), mk(16)]), true); println(f"h{h[0].id}");
  let i = t2(EArr.A([mk(17), mk(18)]), None); println(f"i{i[0].id}");
  let j = t3(EArr.A([mk(19), mk(20)]), true); println(f"j{j[0].id}");
  let k = u1(EStr.A([f"a{1}", f"bb{2}"]), true); println(f"k{k[1]}");
  println(f"l{u2(EStr.A([f"a{1}", f"bb{2}"]), true)}");
  println(f"m{u3(EStr.A([f"a{1}", f"bb{2}"]), true)}");
  let n = u4(EStr.A([f"a{1}", f"bb{2}"]), None); println(f"n{n[1]}");
  println("end");
}
"#,
        &[
            "mid", "dR1", "dR2", "a1", "b3", "dR3", "dR4", "d8", "dR7", "dR8", "e9", "dR9", "dR10",
            "dR11", "dR12", "f23", "g13", "dR13", "dR14", "h15", "dR15", "dR16", "i17", "dR17",
            "dR18", "j19", "dR19", "dR20", "kbb2", "l5", "m2", "nbb2", "end",
        ],
        "asan_branch_tail_of_enclosing_boxed_array_payload_freed_once",
    );
}
