//! B-2026-09-27-99 — a `Drop` field of a by-value struct PARAM that the
//! callee hands on to another owner (`keep(w.r)`, a push into a local `Vec`
//! or `Map`), and a field a callee returns out of a call-result argument.

use super::*;

/// B-2026-09-27-99 — a param's field handed to a callee that keeps it
/// (`let k = keep(w.r)`, `keep(w.s);`, inside a tuple, two hops down, through a
/// method and a generic callee) or pushed into a container the function
/// declares (`xs.push(w.r)`, the alias spelling, `m.insert(w.b, w.r)`) runs its
/// body once, for a named and a call-result argument alike; and so does a field
/// returned out of a call-result argument (`retr(mkw(100))`). The part scan
/// reported only an ALIAS handed to a keeping callee, so the caller's walk ran
/// the part's body again, and codegen's call-result argument walk was not
/// masked by the parts the callee hands on at all.
#[test]
fn asan_param_field_handed_to_another_owner_is_freed_once() {
    assert_clean_asan_run_min_allocs(
        r#"struct D { id: i64, name: String }
impl Drop for D { fn drop(mut ref self) { println(f"d{self.id}{self.name}") } }
fn mkd(n: i64) -> D { D { id: n, name: f"n{n}" } }
struct W { r: D, s: D, b: i64 }
struct X { w: W, t: D }
fn mkw(n: i64) -> W { W { r: mkd(n), s: mkd(n + 1), b: n } }
fn keep(d: D) -> D { d }
fn retr(w: W) -> D { w.r }
struct H { k: i64 }
impl H { fn f(ref self, w: W) -> i64 { let k = keep(w.r); k.id } }
fn gk[T](w: W, t: T) -> i64 { let k = keep(w.r); k.id }
fn keepf(w: W) -> i64 { let k = keep(w.r); k.id }
fn keeps(w: W) -> i64 { keep(w.s); w.b }
fn tup(w: W) -> i64 { let t = (keep(w.r), 5); t.1 }
fn pushf(w: W) -> i64 { let mut xs: Vec[D] = Vec.new(); xs.push(w.r); xs.len() }
fn aliasp(w: W) -> i64 { let r = w.r; let mut xs: Vec[D] = Vec.new(); xs.push(r); xs.len() }
fn ins(w: W) -> i64 { let mut m: Map[i64, D] = Map.new(); m.insert(w.b, w.r); m.len() }
fn hop2(x: X) -> i64 { let k = keep(x.w.r); k.id }
fn main() {
    println(f"a{keepf(mkw(10))}");
    println(f"b{keeps(mkw(20))}");
    println(f"c{tup(mkw(30))}");
    println(f"e{pushf(mkw(40))}");
    println(f"f{aliasp(mkw(50))}");
    println(f"g{ins(mkw(60))}");
    println(f"h{hop2(X { w: mkw(70), t: mkd(79) })}");
    let w = mkw(80);
    println(f"i{keepf(w)}");
    let v = mkw(90);
    println(f"j{pushf(v)}");
    let d = retr(mkw(100));
    println(f"k{d.id}");
    let h = H { k: 0 };
    println(f"l{h.f(mkw(110))}");
    println(f"m{gk(mkw(120), 1)}");
    println("end")
}
"#,
        &[
            "d10n10", "d11n11", "a10", "d21n21", "d20n20", "b20", "d30n30", "d31n31", "c5",
            "d40n40", "d41n41", "e1", "d50n50", "d51n51", "f1", "d60n60", "d61n61", "g1", "d70n70",
            "d79n79", "d71n71", "h70", "d80n80", "i80", "d81n81", "d90n90", "j1", "d91n91",
            "d101n101", "k100", "d100n100", "d110n110", "d111n111", "l110", "d120n120", "d121n121",
            "m120", "end",
        ],
        "asan_param_field_handed_to_another_owner_is_freed_once",
        40,
    );
}
