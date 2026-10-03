//! B-2026-10-03-36 — a nested variant pattern through a non-generic
//! `shared enum`'s `shared enum` payload (`H.Z(M.My(x))`) tested the outer
//! tag alone, so `H.Z(M.N)` entered the `M.My` arm, and on a match it left
//! `x` and the inner box's release both owning the payload (a double free).
//! The inner tag is now read out of the outer box, and the inner box's
//! moved-out words are zeroed the way a top-level `M.My(x)` zeroes them.

use super::*;

/// B-2026-10-03-36 — matching and non-matching inner variants under `match`
/// and `if let`, two levels deep, a `String` payload, and a `for` loop.
#[test]
fn e2e_nested_shared_enum_pattern_tests_the_inner_tag_and_moves_once() {
    let src = r#"
shared enum H { Z(M), N }
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
}"#;
    let want = "a 2\nb 0\nc 2\nd 2 heap-string-long-enough-4\ne 2\nf 9\ng heap-string-long-enough-7\nh else\ni 2\ni 7\ni 0\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
