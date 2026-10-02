//! B-2026-09-27-33 — a non-escaping closure that hands a CAPTURED heap-bearing
//! user struct or enum to a by-value callee freed it twice on every compiled
//! surface (a SEGFAULT for the boxed generic enum): the env holds a bit-copy of
//! a value the enclosing frame still owns and frees at its scope exit, and the
//! callee freed the same heap. The body's move now hands the callee its own
//! copy (a boxed generic enum through the use-after-move copy; a struct
//! through its entry copy, because the whole-program transfer gate no longer
//! admits a captured argument). Called once, twice, never, through a `let`
//! rebind, a nested closure, a `for` binding, and with an outer use (c13).
//! Controls: the concrete enum (c11) and `String` (c12) were already right, and
//! a closure-local shadow (c14) is an ordinary move.
//! No `Drop` anywhere: a copy of a `Drop`-bodied capture would run a second
//! body, which is B-2026-09-27-16's question and is left untouched.

use super::*;

/// B-2026-09-27-33 — a closure that hands a captured heap-bearing struct or boxed generic enum to a by-value callee frees it once.
#[test]
fn asan_closure_captured_aggregate_to_by_value_callee_frees_once() {
    assert_clean_asan_run_min_allocs(
        r#"enum Ho[T] { Full(T), Empty }
enum Hc { Full(String), Empty }
struct W { s: String }
fn keep(h: Ho[String]) -> i64 { match h { Ho.Full(s) => s.len(), Ho.Empty => 0 } }
fn keepc(h: Hc) -> i64 { match h { Hc.Full(s) => s.len(), Hc.Empty => 0 } }
fn takes(s: String) -> i64 { s.len() }
fn takew(w: W) -> i64 { w.s.len() }
fn mkh(s: String) -> Ho[String] { return Ho.Full(s + "-heap-string-long-enough"); }
fn mkw(s: String) -> W { return W { s: s + "-heap-string-long-enough" }; }
fn c1() { let h = mkh("a"); let f = || keep(h); println(f"c1 {f()}"); }
fn c2() { let v: Vec[Ho[String]] = [mkh("bb"), Ho.Empty]; for g in v { let f = || keep(g); println(f"c2 {f()}") } }
fn c3() { let w = mkw("ccc"); let f = || takew(w); println(f"c3 {f()}"); }
fn c4() { let h = mkh("d"); let f = || keep(h); println(f"c4 {f()} {f()}"); }
fn c5() { let w = mkw("e"); let f = || takew(w); println(f"c5 {f()} {f()}"); }
fn c6() { let w = mkw("f"); let f = || { let u = w; takew(u) }; println(f"c6 {f()}"); }
fn c7() { let h = mkh("g"); let f = |k: i64| keep(h) + k; println(f"c7 {f(100)}"); }
fn c8() { let ws: Vec[W] = [mkw("h"), mkw("hh")]; for w in ws { let f = || takew(w); println(f"c8 {f()}") } }
fn c9() { let h = mkh("i"); let f = || { let g = || keep(h); g() }; println(f"c9 {f()}"); }
fn c10() { let h = mkh("j"); let f = || keep(h); println("c10 never called"); }
fn c11() { let h = Hc.Full("k".to_string() + "-heap-string-long-enough"); let f = || keepc(h); println(f"c11 {f()}"); }
fn c12() { let s = "l".to_string() + "-heap-string-long-enough"; let f = || takes(s); println(f"c12 {f()}"); }
fn c13() { let w = mkw("m"); let f = || takew(w); println(f"c13 {f()}"); println(f"c13 {takew(w)}"); }
fn c14() { let w = mkw("n"); let f = || { let w = mkw("nn"); takew(w) }; println(f"c14 {f()} {w.s}"); }
fn main() {
    c1()
    c2()
    c3()
    c4()
    c5()
    c6()
    c7()
    c8()
    c9()
    c10()
    c11()
    c12()
    c13()
    c14()
    println("end")
}"#,
        &[
            "c1 25",
            "c2 26",
            "c2 0",
            "c3 27",
            "c4 25 25",
            "c5 25 25",
            "c6 25",
            "c7 125",
            "c8 25",
            "c8 26",
            "c9 25",
            "c10 never called",
            "c11 25",
            "c12 25",
            "c13 25",
            "c13 25",
            "c14 26 n-heap-string-long-enough",
            "end",
        ],
        "asan_closure_captured_aggregate_to_by_value_callee_frees_once",
        30,
    );
}
