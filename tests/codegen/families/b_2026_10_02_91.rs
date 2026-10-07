//! B-2026-10-02-91 — an `if let` or `while let` over a `for` loop's concrete
//! heap-enum element freed the payload twice on every compiled surface, and a
//! read of the element after the `if let` saw it emptied. The `match` leg
//! already treated the loop binding as a view of the container's slot (and
//! deep-copied it when a payload binding escapes); the `if let` / `while let`
//! legs now ask the same two questions.

use super::*;

/// B-2026-10-02-91 — `if let` / `while let` over a `for` loop's heap-enum
/// element frees the payload once: read-only, as an expression, payload moved
/// to a callee or pushed into a Vec, the element read again (by `ref` and by
/// value) after the `if let`, a struct payload whole and destructured, over
/// `iter()`, and `while let` reading or moving the payload.
#[test]
fn e2e_if_let_over_for_loop_heap_enum_element_frees_once() {
    let src = r#"
enum Hc { Full(String), Empty }
struct P { a: String, b: i64 }
enum Hp { Full(P), Empty }
fn takes(s: String) -> i64 { s.len() }
fn keepc(h: Hc) -> i64 { match h { Hc.Full(s) => s.len(), Hc.Empty => 0 } }
fn gr(h: ref Hc) -> i64 { match h { Hc.Full(s) => s.len(), Hc.Empty => 0 } }
fn mkc(s: String) -> Hc { return Hc.Full(s + "-heap-string-long-enough"); }
fn e1() { let v: Vec[Hc] = [mkc("a"), Hc.Empty]; for h in v { if let Hc.Full(s) = h { println(f"e1 {s.len()}") } } }
fn e2() { let v: Vec[Hc] = [mkc("bb"), Hc.Empty]; for h in v { let a = if let Hc.Full(s) = h { s.len() } else { 0 }; println(f"e2 {a}") } }
fn e3() { let v: Vec[Hc] = [mkc("ccc")]; for h in v { if let Hc.Full(s) = h { println(f"e3 {takes(s)}") } } }
fn e4() { let v: Vec[Hc] = [mkc("d")]; let mut out: Vec[String] = []; for h in v { if let Hc.Full(s) = h { out.push(s) } }; println(f"e4 {out[0]}"); }
fn e5() { let v: Vec[Hc] = [mkc("e")]; for h in v { if let Hc.Full(s) = h { println(f"e5 {s.len()}") }; println(f"e5 {gr(h)}") } }
fn e6() { let v: Vec[Hc] = [mkc("f")]; for h in v { let mut i = 0; while let Hc.Full(s) = h { println(f"e6 {s.len()}"); i = i + 1; if i > 1 { break } } } }
fn e7() { let v: Vec[Hp] = [Hp.Full(P { a: "g".to_string() + "-heap-string-long-enough", b: 1 })]; for h in v { if let Hp.Full(p) = h { println(f"e7 {p.a} {p.b}") } } }
fn e8() { let v: Vec[Hp] = [Hp.Full(P { a: "h".to_string() + "-heap-string-long-enough", b: 2 })]; for h in v { if let Hp.Full(P { a, b }) = h { println(f"e8 {a} {b}") } } }
fn e9() { let v: Vec[Hc] = [mkc("i")]; for h in v { if let Hc.Full(s) = h { println(f"e9 {s.len()}") }; println(f"e9 {keepc(h)}") } }
fn e10() { let v: Vec[Hc] = [mkc("j")]; for h in v.iter() { if let Hc.Full(s) = h { println(f"e10 {s.len()}") } } }
fn e11() { let v: Vec[Hc] = [mkc("k")]; for h in v { let mut n = 0; while let Hc.Full(s) = h { n = n + takes(s); if n > 0 { break } }; println(f"e11 {n}") } }
fn main() {
    e1();
    e2();
    e3();
    e4();
    e5();
    e6();
    e7();
    e8();
    e9();
    e10();
    e11();
    println("end")
}"#;
    let want = "e1 25\ne2 26\ne2 0\ne3 27\ne4 d-heap-string-long-enough\ne5 25\ne5 25\ne6 25\ne6 25\ne7 g-heap-string-long-enough 1\ne8 h-heap-string-long-enough 2\ne9 25\ne9 25\ne10 25\ne11 25\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
