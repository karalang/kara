//! B-2026-10-02-49: an `if let` tail over a user enum's boxed `Array` payload frees it once

use super::*;

/// B-2026-10-02-49: an `if let` whose then-block hands out the arm binding of a
/// user enum's heap-boxed `Array` payload (`if let E.A(v) = x { v } else { .. }`)
/// as the function's tail double-freed the elements compiled, for
/// `Array[String, 2]` as much as for an element with a `Drop` body: the box's drop
/// freed them and so did the caller's result. The `match` spelling already
/// disarmed the box. Covers a re-bind tail (`{ let u = v; u }`), a local and a
/// fresh-temp scrutinee, a `String` array bound by `let`, an `else if let` chain,
/// and the else edge (`l`), whose box must still free its own payload.
#[test]
fn asan_if_let_tail_of_boxed_array_payload_freed_once() {
    assert_clean_asan_run(
        r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}" }; }
enum EArr { A(Array[R, 2]), B }
enum EStr { A(Array[String, 2]), B }
fn z() -> Array[R, 2] { return [mk(0), mk(0)]; }
fn zs() -> Array[String, 2] { return ["a", "b"]; }
fn me(i: i64) -> EArr { return EArr.A([mk(i), mk(i + 1)]); }
fn n8(x: EArr) -> Array[R, 2] { if let EArr.A(v) = x { v } else { z() } }
fn n9(x: EStr) -> Array[String, 2] { if let EStr.A(v) = x { v } else { zs() } }
fn r1(x: EArr) -> Array[R, 2] { if let EArr.A(v) = x { let u = v; u } else { z() } }
fn r2(x: EStr) -> Array[String, 2] { if let EStr.A(v) = x { let u = v; u } else { zs() } }
fn l1() -> Array[R, 2] { let e = me(5); if let EArr.A(v) = e { v } else { z() } }
fn f1() -> Array[R, 2] { if let EArr.A(v) = me(7) { v } else { z() } }
fn m2(x: EStr) -> i64 { let k: Array[String, 2] = if let EStr.A(v) = x { v } else { zs() }; return k[1].len() as i64; }
fn e1(x: EArr, y: EArr) -> Array[R, 2] { if let EArr.A(v) = x { v } else if let EArr.A(w) = y { w } else { z() } }
fn b1(x: EArr) -> Array[R, 2] { if let EArr.A(v) = x { v } else { z() } }
fn main() {
    let a = n8(EArr.A([mk(1), mk(2)])); println(f"a{a[0].id}");
    let b = n9(EStr.A([f"x{1}", f"y{22}"])); println(f"b{b[0]}");
    let c = r1(EArr.A([mk(3), mk(4)])); println(f"c{c[0].id}");
    let d = r2(EStr.A([f"x{1}", f"y{22}"])); println(f"d{d[1]}");
    let e = l1(); println(f"e{e[0].id}");
    let f = f1(); println(f"f{f[0].id}");
    println(f"h{m2(EStr.A([f"x{1}", f"y{22}"]))}");
    let j = e1(EArr.B, EArr.A([mk(11), mk(12)])); println(f"j{j[0].id}");
    let l = b1(EArr.B); println(f"l{l[0].id}");
    println("end");
}
"#,
        &[
            "a1", "dR1", "dR2", "bx1", "c3", "dR3", "dR4", "dy22", "e5", "dR5", "dR6", "f7", "dR7",
            "dR8", "h3", "j11", "dR11", "dR12", "l0", "dR0", "dR0", "end",
        ],
        "asan_if_let_tail_of_boxed_array_payload_freed_once",
    );
}
