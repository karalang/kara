//! B-2026-09-29-17: a pattern binding that shadows a borrowed param reads as the new binding.

use super::*;

/// B-2026-09-29-17 — a `match` / `if let` / `while let` arm binding or a
/// `let .. else` binding that shadows a `ref` / `mut ref` parameter reads as
/// the new binding, and the parameter is intact after the arm. Before,
/// codegen kept the parameter's borrow tags on the new name and panicked
/// (`expected PointerValue`), or read a wrong `len()` for a `String`.
#[test]
fn interp_pattern_binding_shadowing_a_borrowed_param_reads_the_binding() {
    let out = run(
        r#"fn cm(t: Option[i64], v: mut ref i64) -> i64 { let y = match t { Option.Some(v) => v + 1, Option.None => 0 }; v = v + y; return y }
fn cl(t: Option[i64], v: ref i64) -> i64 { let y = if let Option.Some(v) = t { v } else { 0 }; return y + v }
fn cs(t: Option[String], v: ref String) -> i64 { let y = match t { Option.Some(v) => v.len(), Option.None => 0 }; return y + v.len() }
fn cw(t: Option[i64], v: mut ref i64) -> i64 { let mut o = t; let mut s = 0; while let Option.Some(v) = o { s = s + v; o = Option.None; }; return s + v }
fn ce(t: Option[i64], v: ref i64) -> i64 { let Option.Some(v) = t else { return 0 }; return v }
fn cv(t: Option[i64], v: mut ref Vec[i64]) -> i64 { let y = match t { Option.Some(v) => v, Option.None => 0 }; v.push(y); return v.len() }
fn main() {
    let mut a = 10;
    let b = 20;
    let c = "abcd";
    let mut w: Vec[i64] = Vec.new();
    println(f"{cm(Option.Some(3), mut a)} {a}");
    println(f"{cl(Option.Some(4), b)} {cs(Option.Some("xy"), c)} {cw(Option.Some(5), mut a)} {ce(Option.Some(6), b)}");
    println(f"{cv(Option.Some(7), mut w)} {w[0]}")
}
"#,
    );
    assert_eq!(out, "4 14\n24 6 19 6\n1 7\n");
}
