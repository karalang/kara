//! B-2026-10-03-38 — a nested variant pattern through a VALUE enum's
//! `shared enum` payload freed the matched payload twice. The ASAN twin of
//! the codegen fixture.

use super::*;

/// B-2026-10-03-38 — the codegen fixture's table under ASAN.
#[test]
fn asan_value_enum_nested_shared_enum_pattern_moves_once() {
    assert_clean_asan_run_min_allocs(
        r#"enum V { Z(M), N }
shared enum M { My(Vec[String]), N }
enum Vs { Z(Ms), N }
shared enum Ms { S(String), T }
enum V2 { P(i64, M), N }
enum Vf { Z { m: M, k: i64 }, N }
fn mkv(s: String) -> Vec[String] { let mut v = Vec.new(); v.push(s); v.push("second-heap-string-long-enough".to_string()); v }
fn hs(k: i64) -> String { f"heap-string-long-enough-{k}" }
fn cnt(v: ref V) -> i64 { match v { V.Z(M.My(x)) => x.len(), _ => 0 } }
fn main() {
    let n0 = match V.Z(M.My(mkv(hs(0)))) { V.Z(M.My(x)) => x.len(), _ => 0 };
    println(f"x5 {n0}");
    let a = V.Z(M.My(mkv(hs(1))));
    let na = match a { V.Z(M.My(x)) => x.len(), _ => 0 };
    println(f"a {na}");
    let b = V.Z(M.N);
    let nb = match b { V.Z(M.My(x)) => x.len(), _ => 0 };
    println(f"b {nb}");
    let c = V.Z(M.My(mkv(hs(3))));
    let nc = match c { V.Z(M.My(x)) => x.len(), V.Z(M.N) => 5, V.N => 0 };
    println(f"c {nc}");
    let d = V.Z(M.My(mkv(hs(4))));
    if let V.Z(M.My(x)) = d { println(f"d {x.len()} {x[0]}") }
    let g = Vs.Z(Ms.S(hs(7)));
    let sg = match g { Vs.Z(Ms.S(t)) => t, _ => "none".to_string() };
    println(f"g {sg}");
    let h = Vs.Z(Ms.T);
    if let Vs.Z(Ms.S(t)) = h { println(f"h {t}") } else { println("h else") }
    let p = V2.P(3, M.My(mkv(hs(8))));
    let np = match p { V2.P(k, M.My(x)) => k + x.len(), _ => 0 };
    println(f"p {np}");
    let m = M.My(mkv(hs(10)));
    let al = m;
    let q = V.Z(al);
    let nq = match q { V.Z(M.My(x)) => x.len(), _ => 0 };
    let nm = match m { M.My(x) => x.len(), _ => 9 };
    println(f"alias {nq} {nm}");
    let f = Vf.Z { m: M.My(mkv(hs(11))), k: 4 };
    let nf = match f { Vf.Z { m: M.My(x), k } => x.len() + k, _ => 0 };
    println(f"sv {nf}");
    let e = V.Z(M.My(mkv(hs(13))));
    let V.Z(M.My(xe)) = e else { return };
    println(f"le {xe.len()}");
    let r = V.Z(M.My(mkv(hs(14))));
    let nr = cnt(r) + cnt(r);
    println(f"ref {nr}");
    let mut hv: Vec[V] = Vec.new();
    hv.push(V.Z(M.My(mkv(hs(9)))));
    hv.push(V.Z(M.N));
    hv.push(V.N);
    for i in hv { let n = match i { V.Z(M.My(x)) => x.len(), V.Z(M.N) => 7, V.N => 0 }; println(f"i {n}") }
    println("end")
}"#,
        &[
            "x5 2",
            "a 2",
            "b 0",
            "c 2",
            "d 2 heap-string-long-enough-4",
            "g heap-string-long-enough-7",
            "h else",
            "p 5",
            "alias 2 2",
            "sv 6",
            "le 2",
            "ref 4",
            "i 2",
            "i 7",
            "i 0",
            "end",
        ],
        "asan_value_enum_nested_shared_enum_pattern_moves_once",
        20,
    );
}
