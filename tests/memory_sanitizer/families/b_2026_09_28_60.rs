//! B-2026-09-28-60 — two `shared enum`s declaring the same variant name
//! crashed `karac build` in about half of all builds (payload offsets from
//! one enum, heap type from another). The ASAN twin of the codegen fixture.

use super::*;

/// B-2026-09-28-60 — three shared enums sharing `Y`, bound by `if let`,
/// `match`, a two-payload variant, a generic instance and a `for` loop.
#[test]
fn asan_shared_enums_sharing_a_variant_name_bind_their_own_payload() {
    assert_clean_asan_run_min_allocs(
        r#"shared enum G[T] { Y(T), N }
shared enum M { Y(Vec[String]), N }
shared enum K { Y(String, i64), N }
fn mkv(s: String) -> Vec[String] { let mut v = Vec.new(); v.push(s); v.push("second-heap-string-long-enough".to_string()); v }
fn hs(k: i64) -> String { f"heap-string-long-enough-{k}" }
fn main() {
    let g: M = M.Y(mkv(hs(3)));
    if let M.Y(x) = g { println(f"a{x.len()} {x[0]}") }
    let g2: M = M.Y(mkv(hs(4)));
    let n2 = match g2 { M.Y(x) => x.len(), M.N => 0 };
    println(f"b{n2}");
    let k: K = K.Y(hs(5), 7);
    if let K.Y(s, n) = k { println(f"c{s.len()} {n}") }
    let k2: K = K.Y(hs(6), 8);
    let n3 = match k2 { K.Y(s, m) => s.len() + m, K.N => 0 };
    println(f"d{n3}");
    let h: G[String] = G.Y(hs(7));
    if let G.Y(s) = h { println(f"e{s.len()}") }
    let mut gs: Vec[M] = Vec.new();
    gs.push(M.Y(mkv(hs(8))));
    gs.push(M.N);
    for g3 in gs { match g3 { M.Y(x) => println(f"f{x.len()}"), M.N => println("fn") } }
    println("end")
}"#,
        &[
            "a2 heap-string-long-enough-3",
            "b2",
            "c25 7",
            "d33",
            "e25",
            "f2",
            "fn",
            "end",
        ],
        "asan_shared_enums_sharing_a_variant_name_bind_their_own_payload",
        10,
    );
}
