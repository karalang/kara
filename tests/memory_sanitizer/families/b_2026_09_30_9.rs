//! B-2026-09-30-9 — a struct pattern over a GENERIC struct nested inside a
//! variant payload (`E.A { x: G { v, n }, k }` over `E[G[String]]`,
//! `Some(G { v, n })` over `Option[G[String]]`) was sized from the struct's
//! ERASED declaration, one word per field, so a `String` field came back one
//! word wide: the debox missed the heap box and `v` was rebuilt out of the
//! envelope words. That freed a pointer nothing allocated (`free(): invalid
//! pointer`), or read a wrong value (0 at -O0, -1 at -O2), and some spellings
//! did not compile at all. The typechecker now records the pattern's
//! instantiated type at its span and codegen sizes it from that.

use super::*;

/// B-2026-09-30-9 — nested generic struct patterns under a generic enum's struct variant, a non-generic enum's struct and tuple variants, `Vec`, wildcard and `..` fields, `Option`/`Result`/tuple payloads, `if let`, and a loop.
#[test]
fn asan_nested_generic_struct_pattern_in_payload_sizes_the_instantiation() {
    assert_clean_asan_run_min_allocs(
        r#"struct G[T] { v: T, n: i64 }
enum E[T] { A { x: T, k: i64 }, B }
fn mk(k: i64) -> String { f"a-heap-string-longer-than-sso-{k}" }
enum F { A { x: G[String], k: i64 }, B }
enum F2 { A(G[String], i64), B }
fn a1(e: E[G[String]]) -> i64 { match e { E.A { x: G { v, n }, k } => v.len() + n + k, E.B => 0 } }
fn a2(e: F) -> i64 { match e { F.A { x: G { v, n }, k } => v.len() + n + k, F.B => 0 } }
fn a3(e: F2) -> i64 { match e { F2.A(G { v, n }, k) => v.len() + n + k, F2.B => 0 } }
fn a5(e: E[G[Vec[i64]]]) -> i64 { match e { E.A { x: G { v, n }, k } => v.len() + n + k, E.B => 0 } }
fn a7(e: E[G[String]]) -> i64 { match e { E.A { x: G { v: _, n }, k } => n + k, E.B => 0 } }
fn a8(e: E[G[String]]) -> i64 { match e { E.A { x: G { n, .. }, k } => n + k, E.B => 0 } }
fn main() {
    println(a1(E.A { x: G { v: mk(1), n: 1 }, k: 2 }));
    println(a1(E.B));
    println(a2(F.A { x: G { v: mk(2), n: 1 }, k: 2 }));
    println(a3(F2.A(G { v: mk(3), n: 1 }, 2)));
    println(a5(E.A { x: G { v: [1, 2, 3], n: 1 }, k: 20 }));
    println(a7(E.A { x: G { v: "a-heap-string-longer-than-sso-4", n: 30 }, k: 2 }));
    println(a8(E.A { x: G { v: "a-heap-string-longer-than-sso-5", n: 40 }, k: 2 }));
    let o = Option.Some(G { v: "a-heap-string-longer-than-sso-6", n: 50 });
    let r1 = match o { Option.Some(G { v, n }) => v.len() + n, Option.None => 0 };
    println(r1);
    let r: Result[G[String], String] = Result.Ok(G { v: "a-heap-string-longer-than-sso-7", n: 60 });
    let r2 = match r { Result.Ok(G { v, n }) => v.len() + n, Result.Err(e) => e.len() };
    println(r2);
    let t = Option.Some((G { v: mk(8), n: 70 }, 5));
    let r3 = match t { Option.Some((G { v, n }, k)) => v.len() + n + k, Option.None => 0 };
    println(r3);
    let q = Option.Some(G { v: "a-heap-string-longer-than-sso-9", n: 80 });
    if let Option.Some(G { v, n }) = q { println(v.len() + n); }
    let vs = [Option.Some(G { v: "a-heap-string-longer-than-sso-10", n: 1 }), Option.None, Option.Some(G { v: "a-heap-string-longer-than-sso-11", n: 2 })];
    let mut s = 0;
    for w in vs {
        match w {
            Option.Some(G { v, n }) => { s = s + v.len() + n; }
            Option.None => {}
        }
    }
    println(s);
    println("end");
}"#,
        &[
            "34", "0", "34", "34", "24", "32", "42", "81", "91", "106", "111", "67", "end",
        ],
        "asan_nested_generic_struct_pattern_in_payload_sizes_the_instantiation",
        10,
    );
}

/// B-2026-09-30-9 — the spellings that did not compile before the fix: a generic caller, a two-level nest, and a leaf moved out as the arm's value.
#[test]
fn asan_nested_generic_struct_pattern_in_payload_compiles_every_spelling() {
    assert_clean_asan_run_min_allocs(
        r#"struct G[T] { v: T, n: i64 }
enum E[T] { A { x: T, k: i64 }, B }
fn mk(k: i64) -> String { f"a-heap-string-longer-than-sso-{k}" }
fn a4[T](e: E[G[T]]) -> i64 { match e { E.A { x: G { v, n }, k } => n + k, E.B => 0 } }
fn a6(e: E[G[G[String]]]) -> i64 { match e { E.A { x: G { v: G { v, n: m }, n }, k } => v.len() + m + n + k, E.B => 0 } }
fn a9(e: E[G[String]]) -> String { match e { E.A { x: G { v, n }, k } => v, E.B => "none" } }
fn main() {
    println(a4(E.A { x: G { v: mk(1), n: 10 }, k: 2 }));
    println(a6(E.A { x: G { v: G { v: mk(2), n: 10 }, n: 100 }, k: 2 }));
    println(a9(E.A { x: G { v: mk(3), n: 1 }, k: 2 }));
    println(a9(E.B));
    println(a6(E.B));
    println("end");
}"#,
        &[
            "12",
            "143",
            "a-heap-string-longer-than-sso-3",
            "none",
            "0",
            "end",
        ],
        "asan_nested_generic_struct_pattern_in_payload_compiles_every_spelling",
        1,
    );
}
