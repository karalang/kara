//! B-2026-09-29-103 — a `let` destructure of a CONCRETE instantiation of a
//! generic struct typed each leaf at the bare parameter, so
//! `let G { v, n } = g; v.len()` over a `G[String]` was refused with
//! `no method 'len' on type parameter 'T'` while the `match` spelling of the
//! same destructure typechecked.

/// B-2026-09-29-103 — `let` destructures of `G[String]`, `G[Vec[i64]]`, a
/// two-parameter `P[String, Vec[String]]`, a generic `G[T]` inside a generic
/// fn, a leaf handed back, and a local literal.
#[test]
fn interp_let_destructure_of_concrete_generic_struct_types_leaves_concretely() {
    // `run` skips the typechecker, and the defect WAS the typechecker's
    // refusal, so the checked entry point is the one that can see it.
    let src = r#"struct G[T] { v: T, n: i64 }
struct P[A, B] { a: A, b: B }
fn f1(g: G[String]) -> i64 { let G { v, n } = g; v.len() + n }
fn f2(g: G[Vec[i64]]) -> i64 { let G { v, .. } = g; v.len() }
fn f3(p: P[String, Vec[String]]) -> i64 { let P { a, b } = p; a.len() + b.len() }
fn f4[T](g: G[T]) -> i64 { let G { v, n } = g; n }
fn f5(g: G[String]) -> String { let G { n, v } = g; v }
fn main() {
    let mut w = Vec.new(); w.push(1); w.push(2);
    let mut ws = Vec.new(); ws.push("heap-string-longer-than-sso-xxxxxx");
    println(f"{f1(G { v: "heap-string-longer-than-sso-abcdef", n: 1 })} {f2(G { v: w, n: 0 })} {f3(P { a: "heap-string-longer-than-sso-aaaaaa", b: ws })} {f4(G { v: "x", n: 7 })}");
    let g = G { v: "heap-string-longer-than-sso-abcdef", n: 1 };
    println(f"{f5(g).len()}");
    let G { v, n } = G { v: 5, n: 6 };
    println(f"{v + n}");
}
"#;
    let (out, errs, _, _) = karac::run_program_full_checked(src);
    assert!(errs.is_empty(), "interp errored: {errs:?}");
    assert_eq!(out.join(""), "35 2 35 7\n34\n11\n");
}
