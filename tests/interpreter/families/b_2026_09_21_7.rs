//! B-2026-09-21-7 — a struct field moved by one path of a `match` / `if let` keeps its body on the others.

use super::*;

/// A struct field moved out of a named local by ONE arm of several (or by an
/// `if let` / `while let` that may miss) moves only on that path. The compiled
/// backends masked the field out of the local's field-bodies walk statically,
/// so every path that fell through to a sibling arm, failed the arm's guard or
/// missed the `if let` lost the field's `Drop` body (memory stayed clean). It
/// now masks through a per-path runtime flag. Covers a guarded arm beside an
/// unguarded one, a wildcard fallback, a refutable literal sub-pattern, an arm
/// binding both fields, enum and `Vec` fields, a loop, a `let`-bound match
/// value, `if let`, `while let` and a nested struct field.
#[test]
fn interp_struct_field_moved_by_one_arm_keeps_its_body_on_the_others() {
    let out = run(r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
fn mk(i: i64) -> R { R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn eat(r: R) { println(f"eat{r.id}") }
struct S3 { a: R, b: R, k: i64 }
enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("dE") } }
struct Se { e: E, b: R }
struct Sv { v: Vec[R], b: R }
struct In { r: R, k: i64 }
struct Out { i: In, b: R }
fn g1(t: i64) -> i64 { let s = S3 { a: mk(1), b: mk(2), k: t }; match s { S3 { a, .. } if s.k > 900 => { return a.id; } S3 { b, .. } => { return b.id; } } }
fn g2(t: i64) -> i64 { let s = S3 { a: mk(1), b: mk(2), k: t }; match s { S3 { a, .. } if s.k > 900 => { eat(a); 1 } _ => { 0 } } }
fn g3(t: i64) -> i64 { let s = S3 { a: mk(1), b: mk(2), k: t }; match s { S3 { a, k, .. } if k > 900 => { a.id } S3 { b, k: 1, .. } => { b.id } S3 { .. } => { 0 } } }
fn g4(t: i64) -> i64 { let s = S3 { a: mk(1), b: mk(2), k: t }; match s { S3 { a, b, k } if k > 900 => { eat(b); a.id } S3 { b, .. } => { b.id } } }
fn g5(t: i64) -> i64 { let s = Se { e: E.A(mk(1)), b: mk(2) }; match s { Se { e, .. } if t > 900 => { match e { E.A(r) => r.id, E.B => 0 } } Se { b, .. } => { b.id } } }
fn g6(t: i64) -> i64 { let s = Sv { v: vec![mk(1), mk(3)], b: mk(2) }; match s { Sv { v, .. } if t > 900 => { v.len() } Sv { b, .. } => { b.id } } }
fn g7(t: i64) -> i64 { let mut n = 0; for i in 0..2 { let s = S3 { a: mk(1 + i * 10), b: mk(2 + i * 10), k: t }; match s { S3 { a, k, .. } if k > 900 => { n = n + a.id; } S3 { b, .. } => { n = n + b.id; } } } n }
fn g8(t: i64) -> i64 { let s = S3 { a: mk(1), b: mk(2), k: t }; let x = match s { S3 { a, k, .. } if k > 900 => { a } S3 { b, .. } => { b } }; x.id }
fn g9(t: i64) -> i64 { let s = S3 { a: mk(1), b: mk(2), k: t }; if let S3 { a, k: 1000, .. } = s { return a.id; } 0 }
fn g10(t: i64) -> i64 { let s = S3 { a: mk(1), b: mk(2), k: t }; while let S3 { a, k: 1000, .. } = s { return a.id; } 0 }
fn g11(t: i64) -> i64 { let o = Out { i: In { r: mk(1), k: t }, b: mk(2) }; match o { Out { i: In { r, k }, .. } if k > 900 => { r.id } Out { b, .. } => { b.id } } }
fn main() {
    println(f"g1 {g1(1)} {g1(1000)}");
    println(f"g2 {g2(1)} {g2(1000)}");
    println(f"g3 {g3(1)} {g3(1000)} {g3(5)}");
    println(f"g4 {g4(1)} {g4(1000)}");
    println(f"g5 {g5(1)} {g5(1000)}");
    println(f"g6 {g6(1)} {g6(1000)}");
    println(f"g7 {g7(1)} {g7(1000)}");
    println(f"g8 {g8(1)} {g8(1000)}");
    println(f"g9 {g9(1)} {g9(1000)}");
    println(f"g10 {g10(1)} {g10(1000)}");
    println(f"g11 {g11(1)} {g11(1000)}");
    println("end")
}
"#);
    assert_eq!(out, "d2\nd1\nd1\nd2\ng1 2 1\nd2\nd1\neat1\nd1\nd2\ng2 0 1\nd2\nd1\nd1\nd2\nd2\nd1\ng3 2 1 0\nd2\nd1\neat2\nd2\nd1\ng4 2 1\nd2\ndE\nd1\ndE\nd1\nd2\ng5 2 1\nd2\nd1\nd3\nd1\nd3\nd2\ng6 2 2\nd2\nd1\nd12\nd11\nd1\nd2\nd11\nd12\ng7 14 12\nd1\nd2\nd2\nd1\ng8 2 1\nd2\nd1\nd1\nd2\ng9 0 1\nd2\nd1\nd1\nd2\ng10 0 1\nd2\nd1\nd1\nd2\ng11 2 1\nend\n");
}
