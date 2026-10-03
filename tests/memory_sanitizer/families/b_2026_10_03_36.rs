//! B-2026-10-03-36 — a nested variant pattern through a `shared enum`'s
//! `shared enum` payload entered the wrong arm and freed the matched payload
//! twice. The ASAN twin of the codegen fixture.

use super::*;

/// B-2026-10-03-36 — matching and non-matching inner variants under `match`
/// and `if let`, two levels deep, a `String` payload, and a `for` loop.
#[test]
fn asan_nested_shared_enum_pattern_tests_the_inner_tag_and_moves_once() {
    assert_clean_asan_run_min_allocs(
        r#"shared enum H { Z(M), N }
shared enum M { My(Vec[String]), N }
shared enum H3 { W(H), N }
shared enum Hs { Z(Ms), N }
shared enum Ms { S(String), T }
fn mkv(s: String) -> Vec[String] { let mut v = Vec.new(); v.push(s); v.push("second-heap-string-long-enough".to_string()); v }
fn hs(k: i64) -> String { f"heap-string-long-enough-{k}" }
fn main() {
    let a = H.Z(M.My(mkv(hs(1))));
    let na = match a { H.Z(M.My(x)) => x.len(), _ => 0 };
    println(f"a {na}");
    let b = H.Z(M.N);
    let nb = match b { H.Z(M.My(x)) => x.len(), _ => 0 };
    println(f"b {nb}");
    let c = H.Z(M.My(mkv(hs(3))));
    let nc = match c { H.Z(M.My(x)) => x.len(), H.Z(M.N) => 5, H.N => 0 };
    println(f"c {nc}");
    let d = H.Z(M.My(mkv(hs(4))));
    if let H.Z(M.My(x)) = d { println(f"d {x.len()} {x[0]}") }
    let e = H3.W(H.Z(M.My(mkv(hs(5)))));
    let ne = match e { H3.W(H.Z(M.My(x))) => x.len(), _ => 0 };
    println(f"e {ne}");
    let f = H3.W(H.Z(M.N));
    let nf = match f { H3.W(H.Z(M.My(x))) => x.len(), H3.W(H.Z(M.N)) => 9, _ => 0 };
    println(f"f {nf}");
    let g = Hs.Z(Ms.S(hs(7)));
    let sg = match g { Hs.Z(Ms.S(t)) => t, _ => "none".to_string() };
    println(f"g {sg}");
    let h = Hs.Z(Ms.T);
    if let Hs.Z(Ms.S(t)) = h { println(f"h {t}") } else { println("h else") }
    let mut hv: Vec[H] = Vec.new();
    hv.push(H.Z(M.My(mkv(hs(9)))));
    hv.push(H.Z(M.N));
    hv.push(H.N);
    for i in hv { let n = match i { H.Z(M.My(x)) => x.len(), H.Z(M.N) => 7, H.N => 0 }; println(f"i {n}") }
    println("end")
}"#,
        &[
            "a 2",
            "b 0",
            "c 2",
            "d 2 heap-string-long-enough-4",
            "e 2",
            "f 9",
            "g heap-string-long-enough-7",
            "h else",
            "i 2",
            "i 7",
            "i 0",
            "end",
        ],
        "asan_nested_shared_enum_pattern_tests_the_inner_tag_and_moves_once",
        20,
    );
}
