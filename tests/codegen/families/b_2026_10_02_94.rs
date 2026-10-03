//! B-2026-10-02-94 — a closure parameter named like an enclosing local that
//! is consumed after the closure segfaulted every compiled surface. The
//! use-after-move graph read the PARAM's consume as a move of the outer local
//! (closure params had no scope of their own there), promoted the outer local
//! to an RC box, and codegen then read the param's plain slot through that
//! box's name-keyed entry. Params now get their own scope in the graph, and the
//! body compile does not inherit the enclosing fn's RC-box entry for a name a
//! param shadows (which still matters when the outer local is promoted for a
//! real reason, s9 and s13).

use super::*;

/// B-2026-10-02-94 — a closure param shadowing an enclosing local of the same
/// name: concrete enum, boxed generic enum, struct and `String`; the outer use
/// before the call, the param only read, nested, called twice, and beside an
/// outer local the ownership pass promotes for a real reason (s9, s13), with
/// renamed-param and capture-only controls (s11, s12).
#[test]
fn e2e_closure_param_shadowing_an_outer_local_runs() {
    let src = r#"
enum Hc { Full(String), Empty }
enum Ho[T] { Full(T), Empty }
struct W { s: String }
fn keepc(h: Hc) -> i64 { match h { Hc.Full(s) => s.len(), Hc.Empty => 0 } }
fn keep(h: Ho[String]) -> i64 { match h { Ho.Full(s) => s.len(), Ho.Empty => 0 } }
fn gr(h: ref Hc) -> i64 { match h { Hc.Full(s) => s.len(), Hc.Empty => 0 } }
fn takew(w: W) -> i64 { w.s.len() }
fn takes(s: String) -> i64 { s.len() }
fn mkc(s: String) -> Hc { return Hc.Full(s + "-heap-string-long-enough"); }
fn mkh(s: String) -> Ho[String] { return Ho.Full(s + "-heap-string-long-enough"); }
fn mkw(s: String) -> W { return W { s: s + "-heap-string-long-enough" }; }
fn s1() { let h = mkc("a"); let f = |h: Hc| keepc(h); println(f"s1 {f(mkc("xx"))}"); println(f"s1 {keepc(h)}"); }
fn s2() { let h = mkh("b"); let f = |h: Ho[String]| keep(h); println(f"s2 {f(mkh("xx"))}"); println(f"s2 {keep(h)}"); }
fn s3() { let h = mkw("c"); let f = |h: W| takew(h); println(f"s3 {f(mkw("xx"))}"); println(f"s3 {takew(h)}"); }
fn s4() { let h = "d".to_string() + "-heap-string-long-enough"; let f = |h: String| takes(h); println(f"s4 {f("xx".to_string() + "yy")}"); println(f"s4 {takes(h)}"); }
fn s5() { let h = mkc("e"); let f = |h: Hc| keepc(h); println(f"s5 {keepc(h)}"); println(f"s5 {f(mkc("xx"))}"); }
fn s6() { let h = mkc("f"); let f = |h: Hc| gr(h); println(f"s6 {f(mkc("xx"))}"); println(f"s6 {keepc(h)}"); }
fn s7() { let h = mkc("g"); let f = |k: i64| { let g = |h: Hc| keepc(h); g(mkc("xx")) + k }; println(f"s7 {f(100)}"); println(f"s7 {keepc(h)}"); }
fn s8() { let h = mkc("h"); let f = |h: Hc| keepc(h); println(f"s8 {f(mkc("xx"))} {f(mkc("yyy"))}"); println(f"s8 {gr(h)}"); println(f"s8 {keepc(h)}"); }
fn s9() { let h = mkc("i"); let g = || keepc(h); println(f"s9 {g()}"); println(f"s9 {keepc(h)}"); let f = |h: Hc| keepc(h); println(f"s9 {f(mkc("xx"))}"); }
fn s10() { let h = mkh("j"); let f = |h: Ho[String]| { match h { Ho.Full(s) => s.len(), Ho.Empty => 0 } }; println(f"s10 {f(mkh("xx"))}"); println(f"s10 {keep(h)}"); }
fn s11() { let h = mkc("k"); let g = || keepc(h); println(f"s11 {g()}"); println(f"s11 {keepc(h)}"); }
fn s12() { let h = mkc("l"); let g = || keepc(h); println(f"s12 {g()}"); println(f"s12 {keepc(h)}"); let f = |q: Hc| keepc(q); println(f"s12 {f(mkc("xx"))}"); }
fn s13() { let h = mkc("m"); let g = || keepc(h); println(f"s13 {g()}"); println(f"s13 {keepc(h)}"); let f = |h: Hc| gr(h); println(f"s13 {f(mkc("xx"))}"); }
fn main() {
    s1()
    s2()
    s3()
    s4()
    s5()
    s6()
    s7()
    s8()
    s9()
    s10()
    s11()
    s12()
    s13()
    println("end")
}"#;
    let want = "s1 26\ns1 25\ns2 26\ns2 25\ns3 26\ns3 25\ns4 4\ns4 25\ns5 25\ns5 26\ns6 26\ns6 25\ns7 126\ns7 25\ns8 26 27\ns8 25\ns8 25\ns9 25\ns9 25\ns9 26\ns10 26\ns10 25\ns11 25\ns11 25\ns12 25\ns12 25\ns12 26\ns13 25\ns13 25\ns13 26\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
