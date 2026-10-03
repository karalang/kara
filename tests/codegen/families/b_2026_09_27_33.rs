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
fn e2e_closure_captured_aggregate_to_by_value_callee_frees_once() {
    let src = r#"
enum Ho[T] { Full(T), Empty }
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
}"#;
    let want = "c1 25\nc2 26\nc2 0\nc3 27\nc4 25 25\nc5 25 25\nc6 25\nc7 125\nc8 25\nc8 26\nc9 25\nc10 never called\nc11 25\nc12 25\nc13 25\nc13 25\nc14 26 n-heap-string-long-enough\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}

/// B-2026-10-02-76 — a closure body that CONSUMES its captured heap-bearing
/// struct or enum by any route other than a by-value call -- a `match` or
/// `if let` that takes the payload, `let g = h`, a destructure -- freed it a
/// second time on every compiled surface, because the capture is a bit-copy of
/// a value the enclosing frame still frees. The capture now gets a `for`
/// binding's model: a boxed generic enum owns a box of its own for the call, a
/// concrete enum or struct is a loop-element alias. Read-only and payload-
/// moving arms, called once and twice, nested, inside a `for`, returning the
/// payload, with an outer use after the call (d12), returning the capture
/// whole (d13) and through an `own` closure (d14).
#[test]
fn e2e_closure_consuming_its_capture_in_the_body_frees_once() {
    let src = r#"
enum Ho[T] { Full(T), Empty }
enum Hc { Full(String), Empty }
struct W { s: String }
fn keep(h: Ho[String]) -> i64 { match h { Ho.Full(s) => s.len(), Ho.Empty => 0 } }
fn keepc(h: Hc) -> i64 { match h { Hc.Full(s) => s.len(), Hc.Empty => 0 } }
fn takes(s: String) -> i64 { s.len() }
fn mkh(s: String) -> Ho[String] { return Ho.Full(s + "-heap-string-long-enough"); }
fn mkc(s: String) -> Hc { return Hc.Full(s + "-heap-string-long-enough"); }
fn mkw(s: String) -> W { return W { s: s + "-heap-string-long-enough" }; }
fn d1() { let h = mkh("a"); let f = || { match h { Ho.Full(s) => s.len(), Ho.Empty => 0 } }; println(f"d1 {f()}"); }
fn d2() { let h = mkc("bb"); let f = || { match h { Hc.Full(s) => s.len(), Hc.Empty => 0 } }; println(f"d2 {f()} {f()}"); }
fn d3() { let h = mkc("ccc"); let f = || { match h { Hc.Full(s) => takes(s), Hc.Empty => 0 } }; println(f"d3 {f()}"); }
fn d4() { let h = mkh("d"); let f = || { match h { Ho.Full(s) => takes(s), Ho.Empty => 0 } }; println(f"d4 {f()} {f()}"); }
fn d5() { let h = mkh("e"); let f = || { if let Ho.Full(s) = h { s.len() } else { 0 } }; println(f"d5 {f()}"); }
fn d6() { let h = mkc("f"); let f = || { let g = h; keepc(g) }; println(f"d6 {f()} {f()}"); }
fn d7() { let h = mkh("g"); let f = || { let g = h; keep(g) }; println(f"d7 {f()}"); }
fn d8() { let w = mkw("h"); let f = || { let W { s } = w; takes(s) }; println(f"d8 {f()}"); }
fn d9() { let h = mkh("i"); let f = || { match h { Ho.Full(s) => s, Ho.Empty => "e".to_string() } }; let r = f(); println(f"d9 {r}"); }
fn d10() { let h = mkh("j"); let f = || { let g = || { match h { Ho.Full(s) => s.len(), Ho.Empty => 0 } }; g() }; println(f"d10 {f()}"); }
fn d11() { let v: Vec[Ho[String]] = [mkh("kk"), Ho.Empty]; for g in v { let f = || { match g { Ho.Full(s) => s.len(), Ho.Empty => 0 } }; println(f"d11 {f()}") } }
fn d12() { let h = mkc("l"); let f = || keepc(h); println(f"d12 {f()}"); println(f"d12 {keepc(h)}"); }
fn d13() { let h = mkh("m"); let f = || h; let r = f(); println(f"d13 {keep(r)}"); }
fn d14() { let h = mkh("n"); let f = own || { match h { Ho.Full(s) => s.len(), Ho.Empty => 0 } }; println(f"d14 {f()}"); }
fn main() {
    d1()
    d2()
    d3()
    d4()
    d5()
    d6()
    d7()
    d8()
    d9()
    d10()
    d11()
    d12()
    d13()
    d14()
    println("end")
}"#;
    let want = "d1 25\nd2 26 26\nd3 27\nd4 25 25\nd5 25\nd6 25 25\nd7 25\nd8 25\nd9 i-heap-string-long-enough\nd10 25\nd11 26\nd11 0\nd12 25\nd12 25\nd13 25\nd14 25\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
