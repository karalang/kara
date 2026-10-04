//! B-2026-09-28-59 — a by-value arm binding out of a `shared enum` TOOK the
//! `String` / `Vec` payload by zeroing it in the shared object, so a second
//! handle (`let h = g`), or a second `match` on the same handle, read it
//! empty. The object now keeps a copy and the binding keeps the original.

use super::*;

/// B-2026-09-28-59 — every payload shape the copy covers, read through a
/// second handle after a by-value arm binding: `String`, `Vec` of strings,
/// scalars, `shared` handles and plain structs, the generic box, a nested
/// shared enum, a struct variant, `if let`, a loop, and a binding moved on.
#[test]
fn e2e_shared_enum_arm_binding_leaves_the_payload_for_other_handles() {
    let src = r#"shared enum M { Y(Vec[String]), N }
shared enum S { T(String), U }
shared enum I { V(Vec[i64]), W }
shared struct Node { v: i64 }
shared enum R { Q(Vec[Node]), Z }
shared enum G[T] { Y(T), N }
shared enum H { Z(M), N }
shared enum P { A { v: Vec[String], k: i64 }, B }
struct St { s: String }
shared enum Q2 { K(Vec[St]), L }
fn mkv(s: String) -> Vec[String] { let mut v: Vec[String] = Vec.new(); v.push(s + "-heap-string-long-enough"); v }
fn hs(s: String) -> String { s + "-heap-string-long-enough" }
fn take(v: Vec[String]) -> i64 { v.len() }
fn first(m: M) -> Vec[String] { match m { M.Y(x) => x, M.N => Vec.new() } }
struct Pt { a: i64, b: i64 }
shared enum Q4 { K(Vec[Pt]), L }
fn c1() { let g = M.Y(mkv("a")); let h = g; match g { M.Y(x) => { println(f"c1a {x.len()}") } M.N => {} }; match h { M.Y(x) => { println(f"c1b {x.len()}") } M.N => {} }; }
fn c2() { let g = S.T(hs("a")); let h = g; let s = match g { S.T(s) => s, S.U => "u".to_string() }; match h { S.T(t) => { println(f"c2 {s.len()} {t.len()}") } S.U => {} }; }
fn c3() { let g = I.V([1, 2, 3]); let h = g; match g { I.V(x) => { println(f"c3a {x.len()}") } I.W => {} }; match h { I.V(x) => { println(f"c3b {x.len()} {x[2]}") } I.W => {} }; }
fn c4() { let mut ns: Vec[Node] = Vec.new(); ns.push(Node { v: 7 }); let g = R.Q(ns); let h = g; match g { R.Q(x) => { println(f"c4a {x.len()}") } R.Z => {} }; match h { R.Q(x) => { println(f"c4b {x.len()} {x[0].v}") } R.Z => {} }; }
fn c5() { let g: G[Vec[String]] = G.Y(mkv("a")); let h = g; if let G.Y(x) = g { println(f"c5a {x.len()}") }; if let G.Y(x) = h { println(f"c5b {x.len()}") }; }
fn c6() { let g: G[String] = G.Y(hs("a")); let h = g; match g { G.Y(x) => { println(f"c6a {x.len()}") } G.N => {} }; match h { G.Y(x) => { println(f"c6b {x.len()}") } G.N => {} }; }
fn c7() { let g = H.Z(M.Y(mkv("a"))); let h = g; match g { H.Z(M.Y(x)) => { println(f"c7a {x.len()}") } _ => {} }; match h { H.Z(M.Y(x)) => { println(f"c7b {x.len()}") } _ => {} }; }
fn c8() { let g = M.Y(mkv("a")); let h = g; let n = match g { M.Y(x) => take(x), M.N => 0 }; let v = first(h); println(f"c8 {n} {v.len()}"); }
fn c9() { let g = M.Y(mkv("a")); let mut i = 0; while i < 3 { match g { M.Y(x) => { println(f"c9 {x.len()}") } M.N => {} }; i = i + 1; }; }
fn c10() { let g = P.A { v: mkv("a"), k: 2 }; let h = g; match g { P.A { v, k } => { println(f"c10a {v.len()} {k}") } P.B => {} }; match h { P.A { v, .. } => { println(f"c10b {v.len()}") } P.B => {} }; }
fn c11() { let g = M.Y(mkv("a")); let h = g; let Some(n) = (match g { M.Y(x) => Some(x.len()), M.N => None }) else { return }; match h { M.Y(x) => { println(f"c11 {n} {x.len()}") } M.N => {} }; }
fn c12() { let mut q: Vec[St] = Vec.new(); q.push(St { s: hs("a") }); let g = Q2.K(q); let h = g; match g { Q2.K(x) => { println(f"c12a {x.len()}") } Q2.L => {} }; match h { Q2.K(x) => { println(f"c12b {x.len()}") } Q2.L => {} }; }
fn c13() { let g = M.Y(mkv("a")); let mut out: Vec[Vec[String]] = Vec.new(); match g { M.Y(x) => { out.push(x) } M.N => {} }; match g { M.Y(x) => { out.push(x) } M.N => {} }; println(f"c13 {out.len()} {out[0].len()} {out[1].len()}"); }
fn c14() { let g = S.T(hs("a")); let h = g; if let S.T(s) = g { println(f"c14a {s}") }; if let S.T(s) = h { println(f"c14b {s}") }; }
fn c16() { let mut vv: Vec[Vec[String]] = Vec.new(); vv.push(mkv("a")); let g: G[Vec[Vec[String]]] = G.Y(vv); let h = g; match g { G.Y(x) => { println(f"c16a {x.len()} {x[0].len()}") } G.N => {} }; match h { G.Y(x) => { println(f"c16b {x.len()} {x[0].len()}") } G.N => {} }; }
fn c17() { let g = S.T(hs("a")); let mut i = 0; let mut tot = 0; while i < 100 { match g { S.T(s) => { tot = tot + s.len(); } S.U => {} }; i = i + 1; }; println(f"c17 {tot}"); }
fn c19() { let mut q: Vec[Pt] = Vec.new(); q.push(Pt { a: 1, b: 2 }); let g = Q4.K(q); let h = g; match g { Q4.K(x) => { println(f"c19a {x.len()}") } Q4.L => {} }; match h { Q4.K(x) => { println(f"c19b {x.len()} {x[0].b}") } Q4.L => {} }; }
fn main() {
    c1();
    c2();
    c3();
    c4();
    c5();
    c6();
    c7();
    c8();
    c9();
    c10();
    c11();
    c12();
    c13();
    c14();
    c16();
    c17();
    c19();
    println("end")
}"#;
    let want = "c1a 1\nc1b 1\nc2 25 25\nc3a 3\nc3b 3 3\nc4a 1\nc4b 1 7\nc5a 1\nc5b 1\nc6a 25\nc6b 25\nc7a 1\nc7b 1\nc8 1 1\nc9 1\nc9 1\nc9 1\nc10a 1 2\nc10b 1\nc11 1 1\nc12a 1\nc12b 1\nc13 2 1 1\nc14a a-heap-string-long-enough\nc14b a-heap-string-long-enough\nc16a 1 1\nc16b 1 1\nc17 2500\nc19a 1\nc19b 1 2\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
