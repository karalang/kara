//! B-2026-10-03-16 — a closure's by-value param of a heap-bearing user
//! struct or enum is CALLER-RETAINED (the call site keeps the argument and
//! frees it), but the body treated it as its own: a `match` arm taking the
//! payload freed the caller's String (a double free for a named or concrete
//! argument), and a fresh heap-boxed generic enum argument's box had no owner
//! at all (a leak). The param now gets the `for`-binding view model, a boxed
//! generic enum owns a box of its own for the call, the call site owns a fresh
//! boxed argument, and a param returned whole is handed back as a deep copy.

use super::*;

/// B-2026-10-03-16 — closure by-value params of boxed generic, concrete and
/// struct-payload enums, structs, `Option` and `Result`: fresh, constructor
/// and named arguments, a `for` element, read-only and payload-moving arms,
/// `if let`, returned whole, forwarded to a fn, called twice, through an
/// escaping `Fn` value, and the payload returned out of the closure.
#[test]
fn e2e_closure_by_value_enum_param_is_caller_retained() {
    let src = r#"
enum Ho[T] { Full(T), Empty }
enum Hc { Full(String), Empty }
fn mkh(s: String) -> Ho[String] { return Ho.Full(s + "-heap-string-long-enough"); }
fn mkc(s: String) -> Hc { return Hc.Full(s + "-heap-string-long-enough"); }
fn keep(h: Ho[String]) -> i64 { match h { Ho.Full(s) => s.len(), Ho.Empty => 0 } }
fn takes(s: String) -> i64 { s.len() }
struct W { s: String }
struct P { a: String, b: i64 }
enum Hp { Full(P), Empty }
fn mkw(s: String) -> W { return W { s: s + "-heap-string-long-enough" }; }
fn mko(s: String) -> Option[String] { return Some(s + "-heap-string-long-enough"); }
fn mkr(s: String) -> Result[String, String] { return Ok(s + "-heap-string-long-enough"); }
fn mk() -> Fn(Ho[String]) -> i64 { |q: Ho[String]| keep(q) }
fn k1() { let f = |q: Ho[String]| { match q { Ho.Full(s) => s.len(), Ho.Empty => 0 } }; println(f"k1 {f(mkh("a"))}"); }
fn k2() { let h = mkh("bb"); let f = |q: Ho[String]| { match q { Ho.Full(s) => s.len(), Ho.Empty => 0 } }; println(f"k2 {f(h)}"); }
fn k3() { let f = |q: Ho[String]| 5; println(f"k3 {f(mkh("c"))}"); }
fn k4() { let f = |q: Ho[String]| { match q { Ho.Full(s) => takes(s), Ho.Empty => 0 } }; println(f"k4 {f(mkh("d"))}"); }
fn k5() { let f = |q: Ho[String]| q; println(f"k5 {keep(f(mkh("e")))}"); }
fn k6() { let f = |q: Hc| { match q { Hc.Full(s) => s.len(), Hc.Empty => 0 } }; println(f"k6 {f(mkc("f"))}"); }
fn k7() { let f = |q: Ho[String]| keep(q); println(f"k7 {f(mkh("g"))} {f(mkh("gg"))}"); }
fn k8() { let f = |q: Ho[String]| { if let Ho.Full(s) = q { s.len() } else { 0 } }; println(f"k8 {f(mkh("h"))}"); }
fn k9() { let f = |q: Ho[String]| { match q { Ho.Full(s) => s.len(), Ho.Empty => 0 } }; println(f"k9 {f(Ho.Empty)}"); }
fn k10() { let v: Vec[Ho[String]] = [mkh("i"), Ho.Empty]; let f = |q: Ho[String]| { match q { Ho.Full(s) => s.len(), Ho.Empty => 0 } }; for h in v { println(f"k10 {f(h)}") } }
fn k11() { let f = |q: Ho[String]| { let r = match q { Ho.Full(s) => s, Ho.Empty => "e".to_string() }; r.len() }; println(f"k11 {f(mkh("j"))}"); }
fn k12() { let f = |q: Ho[String]| { match q { Ho.Full(s) => s.len(), Ho.Empty => 0 } }; println(f"k12 {f(Ho.Full("k".to_string() + "-heap-string-long-enough"))}"); }
fn k13() { mkh("l"); println("k13"); }
fn k14() { let f = |q: Hc| { match q { Hc.Full(s) => takes(s), Hc.Empty => 0 } }; println(f"k14 {f(mkc("m"))}"); }
fn k15() { let f = |q: Hc| q; let r = f(mkc("n")); println(f"k15 {match r { Hc.Full(s) => s.len(), Hc.Empty => 0 }}"); }
fn k16() { let f = |w: W| takes(w.s); println(f"k16 {f(mkw("o"))}"); }
fn k17() { let w = mkw("pp"); let f = |w: W| w.s.len(); println(f"k17 {f(w)}"); }
fn k18() { let g = mk(); println(f"k18 {g(mkh("q"))}"); }
fn k19() { let f = |o: Option[String]| { match o { Some(s) => s.len(), None => 0 } }; println(f"k19 {f(mko("r"))}"); }
fn k20() { let o = mko("ss"); let f = |o: Option[String]| { match o { Some(s) => takes(s), None => 0 } }; println(f"k20 {f(o)}"); }
fn k21() { let f = |r: Result[String, String]| { match r { Ok(s) => s.len(), Err(e) => e.len() } }; println(f"k21 {f(mkr("t"))}"); }
fn k22() { let f = |h: Hp| { match h { Hp.Full(p) => p.a.len() + p.b, Hp.Empty => 0 } }; println(f"k22 {f(Hp.Full(P { a: "u".to_string() + "-heap-string-long-enough", b: 1 }))}"); }
fn k23() { let f = |h: Hp| { match h { Hp.Full(P { a, b }) => takes(a) + b, Hp.Empty => 0 } }; let h = Hp.Full(P { a: "v".to_string() + "-heap-string-long-enough", b: 2 }); println(f"k23 {f(h)}"); }
fn k24() { let v: Vec[W] = [mkw("w"), mkw("ww")]; let f = |w: W| w.s.len(); for w in v { println(f"k24 {f(w)}") } }
fn k25() { let f = |q: Ho[String]| { match q { Ho.Full(s) => s, Ho.Empty => "e".to_string() } }; let a = f(mkh("x")); let b = f(mkh("xx")); println(f"k25 {a.len()} {b.len()}"); }
fn main() {
    k1()
    k2()
    k3()
    k4()
    k5()
    k6()
    k7()
    k8()
    k9()
    k10()
    k11()
    k12()
    k13()
    k14()
    k15()
    k16()
    k17()
    k18()
    k19()
    k20()
    k21()
    k22()
    k23()
    k24()
    k25()
    println("end")
}"#;
    let want = "k1 25\nk2 26\nk3 5\nk4 25\nk5 25\nk6 25\nk7 25 26\nk8 25\nk9 0\nk10 25\nk10 0\nk11 25\nk12 25\nk13\nk14 25\nk15 25\nk16 25\nk17 26\nk18 25\nk19 25\nk20 26\nk21 25\nk22 26\nk23 27\nk24 25\nk24 26\nk25 25 26\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
