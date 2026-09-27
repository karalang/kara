//! B-2026-09-27-104 — a call whose argument is an `if` / `match` that hands a
//! local's `Drop` field over on one arm, sitting anywhere but a statement's
//! top: an f-string part, a condition, `let _ =`, a struct literal field.

use super::*;

/// B-2026-09-27-104 — `show(if c { p.a } else { mk(9) })` NESTED inside an
/// f-string, an `if` condition, `let _ =`, a method call's f-string, a `match`
/// argument, a struct literal field, and the two-hop `x.w.r` spelling, runs
/// each body once on both legs. Only calls at a statement's top were seeded
/// for the conditional move, so the interpreter lost the minting arm's body
/// and compiled code ran the handed-over field's body a second time over its
/// freed name.
#[test]
fn asan_nested_call_with_a_branch_argument_is_freed_once() {
    assert_clean_asan_run_min_allocs(
        r#"struct D { id: i64, name: String }
impl Drop for D { fn drop(mut ref self) { println(f"d{self.id}{self.name}") } }
fn mkd(n: i64) -> D { D { id: n, name: f"n{n}" } }
struct P { a: D, b: i64 }
struct W { r: D, s: D }
struct X { w: W, t: D }
struct Q { k: i64 }
impl Q { fn see(self, d: D) -> i64 { d.id } }
struct K { v: i64 }
fn show(d: D) -> i64 { d.id }
fn fstr(c: bool) { let p = P { a: mkd(10), b: 0 }; println(f"s{show(if c { p.a } else { mkd(19) })}"); }
fn cond(c: bool) { let p = P { a: mkd(20), b: 0 }; if show(if c { p.a } else { mkd(29) }) > 0 { println("pos") } }
fn wild(c: bool) { let p = P { a: mkd(30), b: 0 }; let _ = show(if c { p.a } else { mkd(39) }); println("w"); }
fn meth(c: bool) { let p = P { a: mkd(40), b: 0 }; let q = Q { k: 0 }; println(f"s{q.see(if c { p.a } else { mkd(49) })}"); }
fn mtch(c: bool) { let p = P { a: mkd(50), b: 0 }; println(f"s{show(match c { true => p.a, false => mkd(59) })}"); }
fn slit(c: bool) { let p = P { a: mkd(60), b: 0 }; let k = K { v: show(if c { p.a } else { mkd(69) }) }; println(f"k{k.v}"); }
fn deep(c: bool) { let x = X { w: W { r: mkd(70), s: mkd(71) }, t: mkd(72) }; println(f"s{show(if c { x.w.r } else { mkd(79) })}"); }
fn main() {
    fstr(false); fstr(true);
    cond(false); cond(true);
    wild(false); wild(true);
    meth(false); meth(true);
    mtch(false); mtch(true);
    slit(false); slit(true);
    deep(false); deep(true);
    println("end")
}
"#,
        &[
            "d19n19", "s19", "d10n10", "d10n10", "s10", "d29n29", "pos", "d20n20", "d20n20", "pos",
            "d39n39", "d30n30", "w", "d30n30", "w", "d49n49", "s49", "d40n40", "d40n40", "s40",
            "d59n59", "s59", "d50n50", "d50n50", "s50", "d69n69", "d60n60", "k69", "d60n60", "k60",
            "d79n79", "s79", "d72n72", "d71n71", "d70n70", "d70n70", "s70", "d72n72", "d71n71",
            "end",
        ],
        "asan_nested_call_with_a_branch_argument_is_freed_once",
        30,
    );
}
