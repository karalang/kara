//! B-2026-09-28-49 — a local moved inside a branch by a builtin sink or a
//! `let` rebind keeps its `Drop` body on the path that did not move it.

use super::*;

/// B-2026-09-28-49 — `let b = E { .. }; if c { es.push(b); }` and
/// `if c { let k = b; }` retracted `b`'s action on every path, so the path
/// that never moved it ran no body (and, for a struct whose memory rides the
/// `Drop` wrapper, freed nothing) on every compiled surface. The move now
/// keeps the action and clears a per-path flag, as a nested `return` already
/// did; the neighbours it exposed ride along: a reassignment after such a move
/// (both backends), a flag cleared by passing the value to a function on the
/// other arm, and the same clear after `if k { return Some(r); } eat(r);`.
#[test]
fn asan_local_moved_in_one_branch_frees_once_on_every_path() {
    assert_clean_asan_run_min_allocs(
        r#"struct E { id: i64 }
impl Drop for E { fn drop(mut ref self) { println(f"dE{self.id}") } }
struct D { id: i64, name: String }
impl Drop for D { fn drop(mut ref self) { println(f"dD{self.id}{self.name}") } }
fn mkd(n: i64) -> D { return D { id: n, name: f"n{n}" }; }
struct W { d: D, k: i64 }
fn flag(n: i64) -> bool { n > 100 }
fn eat(e: E) { println(f"ate{e.id}"); }
fn u4(xs: mut ref Vec[D], c: bool) { let r = mkd(4); if c { xs.push(r); } println("u4"); }
fn hb(k: bool, n: i64) -> Option[E] { let r = E { id: n }; if k { return Option.Some(r); } eat(r); println("hb"); Option.None }
fn main() {
    let mut es: Vec[E] = Vec.new();
    let b = E { id: 2 }; if flag(2) { es.push(b); } println("b");
    let b2 = E { id: 102 }; if flag(102) { es.push(b2); } println("b2");
    let c = E { id: 3 }; if flag(3) { let k = c; println(f"k{k.id}"); } println("c");
    let c2 = E { id: 103 }; if flag(103) { let k = c2; println(f"k{k.id}"); } println("c2");
    let mut ds: Vec[D] = Vec.new();
    let r9 = mkd(9); if ds.len() == 5 { ds.push(r9); } println("d9");
    let r10 = mkd(10); if ds.len() == 0 { ds.push(r10); } println("d10");
    u4(mut ds, false); u4(mut ds, true);
    let mut m: Map[i64, E] = Map.new();
    let e5 = E { id: 5 }; if flag(5) { m.insert(5, e5); } println("m5");
    let e6 = E { id: 106 }; if flag(106) { m.insert(6, e6); } println("m6");
    let w7 = W { d: mkd(7), k: 7 }; if flag(7) { let q = w7; println(f"q{q.k}"); } println("w7");
    let mut ws: Vec[W] = Vec.new();
    let w11 = W { d: mkd(11), k: 11 }; if flag(11) { ws.push(w11); } println("w11");
    let w12 = W { d: mkd(112), k: 12 }; if flag(112) { ws.push(w12); } println("w12");
    let mut i = 0;
    while i < 3 { let e = E { id: 20 + i }; if i == 1 { es.push(e); } i = i + 1; }
    println("loop");
    let f1 = E { id: 30 }; if flag(30) { es.push(f1); } else { eat(f1); } println("f1");
    let f2 = E { id: 131 }; if flag(131) { es.push(f2); } else { eat(f2); } println("f2");
    let h1 = hb(true, 40); let h2 = hb(false, 41); println("h");
    let mut e3 = E { id: 150 }; if flag(150) { es.push(e3); } e3 = E { id: 51 }; println(f"e{e3.id}");
    let mut w2 = W { d: mkd(160), k: 60 }; if flag(160) { let q = w2; println(f"q{q.k}"); } w2 = W { d: mkd(61), k: 61 }; println(f"w{w2.k}");
    let mut w3 = W { d: mkd(170), k: 70 }; if flag(170) { ws.push(w3); } w3 = W { d: mkd(71), k: 71 }; println(f"w{w3.k}");
    match h1 { Option.Some(x) => println(f"x{x.id}"), Option.None => println("n") }
    match h2 { Option.Some(x) => println(f"x{x.id}"), Option.None => println("n") }
    println(f"{es.len()} {ds.len()} {m.len()} {ws.len()}");
    println("end")
}"#,
        &[
            "dE2",
            "b",
            "b2",
            "dE3",
            "c",
            "k103",
            "dE103",
            "c2",
            "dD9n9",
            "d9",
            "d10",
            "dD4n4",
            "u4",
            "u4",
            "dE5",
            "m5",
            "m6",
            "dD7n7",
            "w7",
            "dD11n11",
            "w11",
            "w12",
            "dE20",
            "dE22",
            "loop",
            "ate30",
            "dE30",
            "f1",
            "f2",
            "ate41",
            "dE41",
            "hb",
            "h",
            "e51",
            "dE51",
            "q60",
            "dD160n160",
            "w61",
            "dD61n61",
            "w71",
            "dD71n71",
            "x40",
            "dE40",
            "n",
            "4 2 1 2",
            "dD112n112",
            "dD170n170",
            "dE106",
            "dD10n10",
            "dD4n4",
            "dE102",
            "dE21",
            "dE131",
            "dE150",
            "end",
        ],
        "asan_local_moved_in_one_branch_frees_once_on_every_path",
        30,
    );
}
