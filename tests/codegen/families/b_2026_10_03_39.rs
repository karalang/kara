//! B-2026-10-03-39 — a `shared enum` whose payload is a plain user enum
//! (`shared enum H4 { Z(E), N }` over `enum E { A(Vec[String]), B }`) never
//! released that payload: its heap leaked on every release, and so did the
//! box the constructor mallocs for a payload wider than its one-word area.
//! The box now owns the payload; an arm that takes a leaf gives the box a
//! copy, and a whole-payload binding becomes an owned deep copy.

use super::*;

/// B-2026-10-03-39 — construction only, nested and whole-payload arms,
/// `if let` / `let ... else`, aliased handles matched twice, a `ref` param,
/// a heapless boxed payload, a one-word inline payload, a `String` payload
/// beside a scalar, a two-leaf payload, a struct variant, a wildcard leaf,
/// a whole payload pushed into a `Vec`, and `for` loops.
#[test]
fn e2e_shared_enum_releases_its_value_enum_payload() {
    let src = r#"
shared enum H4 { Z(E), N }
enum E { A(Vec[String]), B }
enum Ep { P(i64, i64), Q }
shared enum H5 { Z(Ep), N }
enum Es { S(String), T }
shared enum H6 { Z(Es, i64), N }
fn mkv(s: String) -> Vec[String] { let mut v = Vec.new(); v.push(s); v.push("second-heap-string-long-enough".to_string()); v }
fn hs(k: i64) -> String { f"heap-string-long-enough-{k}" }
enum C { X, Y }
shared enum Hc { Z(C), N }
enum E2 { A(String, Vec[i64]), B }
shared enum H8 { Z(E2), N }
shared enum H7 { Z { e: E, k: i64 }, N }
fn mki(n: i64) -> Vec[i64] { let mut v = Vec.new(); for i in 0..n { v.push(i) }; v }
fn take(h: H4) -> E { match h { H4.Z(e) => e, _ => E.B } }
fn peek(h: ref H4) -> i64 { match h { H4.Z(E.A(x)) => x.len(), _ => 0 } }
fn elen(e: E) -> i64 { match e { E.A(x) => x.len(), E.B => 0 } }
fn main() {
    let a = H4.Z(E.B);
    println("c1");
    let b = H4.Z(E.A(mkv(hs(1))));
    println("c2");
    let n3 = match H4.Z(E.A(mkv(hs(2)))) { H4.Z(E.A(x)) => x.len(), _ => 0 };
    println(f"c3 {n3}");
    let c = H4.Z(E.A(mkv(hs(3))));
    let n4 = match c { H4.Z(e) => 1, _ => 0 };
    println(f"c4 {n4}");
    let d = H4.Z(E.A(mkv(hs(4))));
    let e2 = match d { H4.Z(e) => e, _ => E.B };
    let n5 = match e2 { E.A(x) => x.len(), E.B => 0 };
    println(f"c5 {n5}");
    let g = H4.Z(E.A(mkv(hs(6))));
    let h = g;
    let n6 = match g { H4.Z(E.A(x)) => x.len(), _ => 0 };
    let m6 = match h { H4.Z(E.A(x)) => x.len() + 10, _ => 0 };
    println(f"c6 {n6} {m6}");
    let g7 = H4.Z(E.A(mkv(hs(7))));
    if let H4.Z(E.A(x)) = g7 { println(f"c7 {x.len()} {x[0]}") }
    let p = H5.Z(Ep.P(1, 2));
    let n8 = match p { H5.Z(Ep.P(i, j)) => i + j, _ => 0 };
    println(f"c8 {n8}");
    let mut hv: Vec[H4] = Vec.new();
    hv.push(H4.Z(E.A(mkv(hs(10)))));
    hv.push(H4.Z(E.B));
    hv.push(H4.N);
    for i in hv { let n = match i { H4.Z(E.A(x)) => x.len(), H4.Z(E.B) => 7, H4.N => 0 }; println(f"c10 {n}") }
    let q = H4.Z(E.A(mkv(hs(11))));
    let n11 = match q { H4.Z(E.A(x)) => x[0].len(), _ => 0 };
    println(f"c11 {n11}");
    let r = H6.Z(Es.S(hs(12)), 3);
    let s12 = match r { H6.Z(Es.S(t), k) => f"{t}{k}", _ => "none".to_string() };
    println(f"c12 {s12}");
    let u = H6.Z(Es.S(hs(13)), 4);
    let w = u;
    let s13 = match u { H6.Z(Es.S(t), k) => t, _ => "none".to_string() };
    let t13 = match w { H6.Z(Es.S(t), k) => t, _ => "none".to_string() };
    println(f"c13 {s13} {t13}");
    let dd = H4.Z(E.A(mkv(hs(1))));
    let mut ev: Vec[E] = Vec.new();
    match dd { H4.Z(e) => ev.push(e), _ => {} }
    println(f"d1 {ev.len()}");
    let gg = H4.Z(E.A(mkv(hs(2))));
    if let H4.Z(e) = gg { println(f"d2 {elen(e)}") }
    let g3 = H4.Z(E.A(mkv(hs(3))));
    let H4.Z(e3) = g3 else { return };
    println(f"d3 {elen(e3)}");
    let g4 = H4.Z(E.A(mkv(hs(4))));
    let g4b = g4;
    let nn4 = elen(take(g4));
    let m4 = peek(g4b);
    println(f"d4 {nn4} {m4}");
    let g5 = H4.Z(E.A(mkv(hs(5))));
    println(f"d5 {peek(g5)} {peek(g5)}");
    let g6 = H6.Z(Es.S(hs(6)), 1);
    let s6 = match g6 { H6.Z(e, _) => match e { Es.S(t) => t, Es.T => "t".to_string() }, _ => "n".to_string() };
    println(f"d6 {s6}");
    let gg7 = Hc.Z(C.Y);
    let n7 = match gg7 { Hc.Z(C.Y) => 1, _ => 0 };
    println(f"d7 {n7}");
    let g8 = H8.Z(E2.A(hs(8), mki(3)));
    let s8 = match g8 { H8.Z(E2.A(s, _)) => s, _ => "n".to_string() };
    println(f"d8 {s8}");
    let g9 = H8.Z(E2.A(hs(9), mki(2)));
    let g9b = g9;
    let n9 = match g9 { H8.Z(E2.A(s, v)) => s.len() + v.len(), _ => 0 };
    let m9 = match g9b { H8.Z(E2.A(s, v)) => s.len() + v.len(), _ => 0 };
    println(f"d9 {n9} {m9}");
    let g10 = H7.Z { e: E.A(mkv(hs(10))), k: 5 };
    let n10 = match g10 { H7.Z { e: E.A(x), k } => x.len() + k, _ => 0 };
    println(f"d10 {n10}");
    let g11 = H7.Z { e: E.A(mkv(hs(11))), k: 5 };
    let nn11 = match g11 { H7.Z { e, k } => elen(e) + k, _ => 0 };
    println(f"d11 {nn11}");
    let g12 = H4.Z(E.A(mkv(hs(12))));
    let n12 = match g12 { H4.Z(E.A(_)) => 1, _ => 0 };
    println(f"d12 {n12}");
    let mut hv2: Vec[H4] = Vec.new();
    hv2.push(H4.Z(E.A(mkv(hs(13)))));
    hv2.push(H4.Z(E.B));
    let mut tot = 0;
    for h in hv2 { let k = match h { H4.Z(e) => elen(e), _ => 0 }; tot = tot + k; }
    println(f"d13 {tot}");
}"#;
    let want = "c1\nc2\nc3 2\nc4 1\nc5 2\nc6 2 12\nc7 2 heap-string-long-enough-7\nc8 3\nc10 2\nc10 7\nc10 0\nc11 26\nc12 heap-string-long-enough-123\nc13 heap-string-long-enough-13 heap-string-long-enough-13\nd1 1\nd2 2\nd3 2\nd4 2 2\nd5 2 2\nd6 heap-string-long-enough-6\nd7 1\nd8 heap-string-long-enough-8\nd9 27 27\nd10 7\nd11 7\nd12 1\nd13 2\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
