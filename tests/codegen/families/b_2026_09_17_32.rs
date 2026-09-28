//! B-2026-09-17-32 -- a by-value param handed back on only SOME exits runs its
//! `Drop` body exactly once on the exit where it dies inside the callee, when
//! every branch leaves through a `return` statement or the other exit moves an
//! element out of another param's tuple payload.

use super::*;

/// B-2026-09-17-32 — `match o {{ Some(n) => {{ return W {{ id: n }}; }} None =>
/// {{ return d; }} }}` called on the `Some` path: `d` dies inside the callee and
/// one body is owed. Every cell runs both directions (`-dies` and `-back`), over
/// `match`, `if`/`else`, `if let`, three arms, a guard, a side-effecting arm, a
/// heap-bearing param, a generic callee, and a `return t.0` exit.
///
/// Before: every `-dies` cell lost `d`'s body on all four surfaces, agreed, so
/// the A/B rule could not see it. `fn_conditionally_returns_param_bare`
/// declined the callee because a branch ending in `return x;` has no tail and
/// was read as an unknown leaf, and because `t.0` fell to `may_mention`'s
/// declining catch-all. `if k {{ return a; }} return d;` was admitted all along.
#[test]
fn e2e_conditionally_returned_param_all_return_branches_output() {
    let Some(out) = run_program(
        r#"struct W { id: i64 }
impl Drop for W { fn drop(mut ref self) { println(f"dW{self.id}"); } }
struct H { id: i64, s: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}"); } }
fn m2(o: Option[i64], d: W) -> W { match o { Some(n) => { return W { id: n }; } None => { return d; } } }
fn i2(k: bool, d: W) -> W { if k { return W { id: 5 }; } else { return d; } }
fn il(o: Option[i64], d: W) -> W { if let Some(n) = o { return W { id: n }; } else { return d; } }
fn m3(x: i64, d: W) -> W { match x { 0 => { return W { id: 50 }; } 1 => { return d; } _ => { return W { id: 52 }; } } }
fn mg(o: Option[i64], d: W) -> W { match o { Some(n) if n > 0 => { return W { id: n }; } _ => { return d; } } }
fn mh(o: Option[i64], d: H) -> H { match o { Some(n) => { return H { id: n, s: f"hhhhhhhhhhhhhhhhhhhhhhhhhhhh{n}" }; } None => { return d; } } }
fn g8[T](o: Option[(T, i64)], d: T) -> T { match o { Some(t) => { return t.0; } None => { return d; } } }
fn ms(k: bool, d: W) -> W { match k { true => { println("  side"); return W { id: 60 }; } false => { return d; } } }
fn hh(o: Option[(H, i64)], d: H) -> H { match o { Some(t) => { return t.0; } None => { return d; } } }
fn h8(o: Option[(W, i64)], d: W) -> W { match o { Some(t) => { return t.0; } None => { return d; } } }
fn mk(n: i64) -> H { return H { id: n, s: f"kkkkkkkkkkkkkkkkkkkkkkkkkkkkkk{n}" }; }
fn main() {
    println("m2-dies"); let a = m2(Some(7), W { id: 101 }); println(f"  {a.id}");
    println("m2-back"); let a2 = m2(None, W { id: 102 }); println(f"  {a2.id}");
    println("i2-dies"); let b = i2(true, W { id: 103 }); println(f"  {b.id}");
    println("i2-back"); let b2 = i2(false, W { id: 104 }); println(f"  {b2.id}");
    println("il-dies"); let c = il(Some(8), W { id: 105 }); println(f"  {c.id}");
    println("il-back"); let c2 = il(None, W { id: 106 }); println(f"  {c2.id}");
    println("m3-dies0"); let d = m3(0, W { id: 107 }); println(f"  {d.id}");
    println("m3-back"); let d1 = m3(1, W { id: 108 }); println(f"  {d1.id}");
    println("m3-dies2"); let d2 = m3(2, W { id: 109 }); println(f"  {d2.id}");
    println("mg-dies"); let e = mg(Some(3), W { id: 110 }); println(f"  {e.id}");
    println("mg-back"); let e2 = mg(Some(-1), W { id: 111 }); println(f"  {e2.id}");
    println("mh-dies"); let f = mh(Some(4), mk(112)); println(f"  {f.id}");
    println("mh-back"); let f2 = mh(None, mk(113)); println(f"  {f2.id}");
    println("g8-dies"); let g = g8(Some((W { id: 8 }, 9)), W { id: 114 }); println(f"  {g.id}");
    println("g8-back"); let n: Option[(W, i64)] = None; let g2 = g8(n, W { id: 115 }); println(f"  {g2.id}");
    println("ms-dies"); let h = ms(true, W { id: 116 }); println(f"  {h.id}");
    println("ms-back"); let h2 = ms(false, W { id: 117 }); println(f"  {h2.id}");
    for i in 0..2 { let q = mh(Some(i), mk(120 + i)); println(f"  loop{q.id}"); }
    println("h8-dies"); let j = h8(Some((W { id: 9 }, 1)), W { id: 130 }); println(f"  {j.id}");
    println("h8-back"); let nn: Option[(W, i64)] = None; let j2 = h8(nn, W { id: 131 }); println(f"  {j2.id}");
    println("hh-dies"); let k = hh(Some((mk(10), 1)), mk(132)); println(f"  {k.id}");
    println("hh-back"); let nh: Option[(H, i64)] = None; let k2 = hh(nh, mk(133)); println(f"  {k2.id}");
    println("end")
}
"#,
    ) else {
        return;
    };
    let got: Vec<&str> = out.lines().collect();
    assert_eq!(
        got,
        vec![
            "m2-dies", "dW101", "  7", "dW7", "m2-back", "  102", "dW102", "i2-dies", "dW103",
            "  5", "dW5", "i2-back", "  104", "dW104", "il-dies", "dW105", "  8", "dW8", "il-back",
            "  106", "dW106", "m3-dies0", "dW107", "  50", "dW50", "m3-back", "  108", "dW108",
            "m3-dies2", "dW109", "  52", "dW52", "mg-dies", "dW110", "  3", "dW3", "mg-back",
            "  111", "dW111", "mh-dies", "dH112", "  4", "dH4", "mh-back", "  113", "dH113",
            "g8-dies", "dW114", "  8", "dW8", "g8-back", "  115", "dW115", "ms-dies", "  side",
            "dW116", "  60", "dW60", "ms-back", "  117", "dW117", "dH120", "  loop0", "dH0",
            "dH121", "  loop1", "dH1", "h8-dies", "dW130", "  9", "dW9", "h8-back", "  131",
            "dW131", "hh-dies", "dH132", "  10", "dH10", "hh-back", "  133", "dH133", "end",
        ],
        "got:\n{out}"
    );
}
