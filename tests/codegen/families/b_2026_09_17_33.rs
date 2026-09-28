//! B-2026-09-17-33 -- a by-value param handed back on some exits and CONSUMED
//! by a call on another (`return R { id: eat(r) }`) runs its `Drop` body once
//! on every path, in every call position.

use super::*;

/// B-2026-09-17-33 — `fn pick(r: R, flag: bool) -> R { if flag { return r }
/// return R { name: f"z", id: eat(r) } }`, `eat` taking `x: R` by value and
/// keeping nothing of it. Each cell runs both paths (`d` dies inside, `b` hands
/// back) over consumers that keep nothing (`eat`, `eatn`, a generic `geat`,
/// two params at once, an operator around the call, a tuple result), one that
/// STORES the param (`keep`, which must stay declined and run the body at the
/// vector's drop), all-`return` branches, and the free, associated and method
/// positions.
///
/// Before: `fn_conditionally_returns_param_bare` declined every consuming
/// leaf. The free and associated spellings lost `r`'s body on the dies-inside
/// path (the free one on all four surfaces, the associated one interpreted
/// only), and the compiled associated and method spellings ran it TWICE on the
/// hand-back path.
#[test]
fn e2e_conditionally_returned_param_consumed_by_call_output() {
    let Some(out) = run_program(
        r#"struct R { name: String, id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.name}") } }
fn eat(x: R) -> i64 { return x.id; }
fn eatn(x: R) -> String { return x.name.clone(); }
fn keep(x: R, v: mut ref Vec[R]) -> i64 { let n = x.id; v.push(x); return n; }
fn geat[T](x: T) -> i64 { return 3; }
fn two(a: R, b: R) -> i64 { return a.id + b.id; }
fn p1(r: R, f: bool) -> R { if f { return r } return R { name: eatn(r), id: 90 } }
fn p2(r: R, f: bool, v: mut ref Vec[R]) -> R { if f { return r } return R { name: f"z", id: keep(r, v) } }
fn p3(r: R, f: bool) -> R { if f { return r } return R { name: f"z", id: geat(r) } }
fn p5(r: R, f: bool) -> R { if f { return r } return R { name: f"z", id: two(r, R { name: f"t", id: 1 }) } }
fn p6(r: R, f: bool) -> R { match f { true => { return r; } false => { return R { name: f"z", id: eat(r) + 1 }; } } }
fn p7(r: R, f: bool) -> (R, i64) { if f { return (r, 1) } return (R { name: f"z", id: 0 }, eat(r)) }
fn q1(r: R, f: bool) -> R { if f { return r } return R { name: f"z", id: eat(r) + 1 } }
fn q2(r: R, f: bool) -> R { match f { true => { return r; } false => { return R { name: f"z", id: eat(r) }; } } }
fn q3(r: R, f: bool) -> R { if f { return r; } else { return R { name: f"z", id: eat(r) }; } }
fn fpick(r: R, flag: bool) -> R {
    if flag { return r }
    return R { name: f"z", id: eat(r) }
}
struct P {}
impl P {
    fn pick(r: R, flag: bool) -> R {
        if flag { return r }
        return R { name: f"z", id: eat(r) }
    }
    fn mpick(self, r: R, flag: bool) -> R {
        if flag { return r }
        return R { name: f"z", id: eat(r) }
    }
}
fn main() {
    let mut v: Vec[R] = [];
    println("p1d"); let a = p1(R { name: f"a", id: 1 }, false); println(f"  {a.id}{a.name}");
    println("p1b"); let a2 = p1(R { name: f"b", id: 2 }, true); println(f"  {a2.id}");
    println("p2d"); let b = p2(R { name: f"c", id: 3 }, false, mut v); println(f"  {b.id} {v.len()}");
    println("p2b"); let b2 = p2(R { name: f"d", id: 4 }, true, mut v); println(f"  {b2.id} {v.len()}");
    println("p3d"); let c = p3(R { name: f"e", id: 5 }, false); println(f"  {c.id}");
    println("p3b"); let c2 = p3(R { name: f"f", id: 6 }, true); println(f"  {c2.id}");
    println("p5d"); let e = p5(R { name: f"i", id: 9 }, false); println(f"  {e.id}");
    println("p5b"); let e2 = p5(R { name: f"j", id: 10 }, true); println(f"  {e2.id}");
    println("p6d"); let g = p6(R { name: f"k", id: 11 }, false); println(f"  {g.id}");
    println("p6b"); let g2 = p6(R { name: f"l", id: 12 }, true); println(f"  {g2.id}");
    println("p7d"); let h = p7(R { name: f"m", id: 13 }, false); println(f"  {h.1}");
    println("p7b"); let h2 = p7(R { name: f"n", id: 14 }, true); println(f"  {h2.1}");
    println("q1"); let a = q1(R { name: f"a", id: 1 }, false); println(f"  {a.id}");
    println("q2"); let b = q2(R { name: f"b", id: 2 }, false); println(f"  {b.id}");
    println("q3"); let c = q3(R { name: f"c", id: 3 }, false); println(f"  {c.id}");

    println("free"); let k = fpick(R { name: f"a", id: 1 }, false); println(f"k:{k.id}");
    println("assoc"); let k2 = P.pick(R { name: f"b", id: 2 }, false); println(f"k:{k2.id}");
    let p = P {};
    println("method"); let k3 = p.mpick(R { name: f"c", id: 3 }, false); println(f"k:{k3.id}");
    println("free-back"); let k4 = fpick(R { name: f"d", id: 4 }, true); println(f"k:{k4.id}");
    println("assoc-back"); let k5 = P.pick(R { name: f"e", id: 5 }, true); println(f"k:{k5.id}");
    println("method-back"); let k6 = p.mpick(R { name: f"g", id: 6 }, true); println(f"k:{k6.id}");
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
            "p1d",
            "dR1/a",
            "  90a",
            "dR90/a",
            "p1b",
            "  2",
            "dR2/b",
            "p2d",
            "  3 1",
            "dR3/z",
            "p2b",
            "  4 1",
            "dR4/d",
            "dR3/c",
            "p3d",
            "dR5/e",
            "  3",
            "dR3/z",
            "p3b",
            "  6",
            "dR6/f",
            "p5d",
            "dR1/t",
            "dR9/i",
            "  10",
            "dR10/z",
            "p5b",
            "  10",
            "dR10/j",
            "p6d",
            "dR11/k",
            "  12",
            "dR12/z",
            "p6b",
            "  12",
            "dR12/l",
            "p7d",
            "dR13/m",
            "  13",
            "dR0/z",
            "p7b",
            "  1",
            "dR14/n",
            "q1",
            "dR1/a",
            "  2",
            "dR2/z",
            "q2",
            "dR2/b",
            "  2",
            "dR2/z",
            "q3",
            "dR3/c",
            "  3",
            "dR3/z",
            "free",
            "dR1/a",
            "k:1",
            "dR1/z",
            "assoc",
            "dR2/b",
            "k:2",
            "dR2/z",
            "method",
            "dR3/c",
            "k:3",
            "dR3/z",
            "free-back",
            "k:4",
            "dR4/d",
            "assoc-back",
            "k:5",
            "dR5/e",
            "method-back",
            "k:6",
            "dR6/g",
            "end",
        ],
        "got:\n{out}"
    );
}
