//! B-2026-10-04-31 — an arm binding over a `shared enum` whose payload is a
//! `Map` / `Set` took the handle by zeroing it in the shared object, so a
//! second `match` (or a second handle) read a null table and segfaulted. The
//! object now keeps a clone and the binding keeps the original. The release
//! of such a payload also gained the per-value drop fn its shared-struct twin
//! has, so `Map[String, Vec[String]]` no longer strands its Strings.

use super::*;

/// B-2026-10-04-31 — `Map` / `Set` payloads read twice through one handle and
/// through two, with `String`, `Vec[String]` and `shared` values, a struct
/// variant, a mixed tuple variant, `if let`, a loop, a by-value callee that
/// returns the map, a wildcard arm, a binding moved into a `Vec`, and a
/// payload released with no match at all.
#[test]
fn e2e_shared_enum_map_payload_arm_binding_leaves_a_copy() {
    let src = r#"shared struct Node { v: i64 }
shared enum Mm { K(Map[i64, String]), L }
shared enum Ms { K(Set[String]), L }
shared enum Mk { K(Map[String, Vec[String]]), L }
shared enum Mn { K(Map[i64, Node]), L }
shared enum Mv { K { m: Map[i64, i64], n: i64 }, L }
shared enum Mt { K(Map[i64, String], String), L }
fn hs(s: String) -> String { s + "-heap-string-long-enough" }
fn mk() -> Map[i64, String] { let mut m: Map[i64, String] = Map.new(); m.insert(1, hs("a")); m }
fn take(g: Mm) -> Map[i64, String] { match g { Mm.K(x) => x, Mm.L => Map.new() } }
fn main() {
    { let g = Mm.K(mk()); match g { Mm.K(x) => { println(f"d1a {x.len()}") } Mm.L => {} }; match g { Mm.K(x) => { println(f"d1b {x.len()}") } Mm.L => {} }; }
    { let g = Mm.K(mk()); let h = g; match g { Mm.K(x) => { println(f"d2a {x.len()}") } Mm.L => {} }; match h { Mm.K(x) => { println(f"d2b {x.len()}") } Mm.L => {} }; }
    { let mut s: Set[String] = Set.new(); s.insert(hs("s")); let g = Ms.K(s); match g { Ms.K(x) => { println(f"d3a {x.len()}") } Ms.L => {} }; match g { Ms.K(x) => { println(f"d3b {x.len()}") } Ms.L => {} }; }
    { let mut m: Map[String, Vec[String]] = Map.new(); m.insert(hs("k"), [hs("v")]); let g = Mk.K(m); match g { Mk.K(x) => { println(f"d4a {x.len()}") } Mk.L => {} }; match g { Mk.K(x) => { println(f"d4b {x.len()}") } Mk.L => {} }; }
    { let mut m: Map[i64, Node] = Map.new(); m.insert(1, Node { v: 9 }); let g = Mn.K(m); match g { Mn.K(x) => { println(f"d5a {x.len()}") } Mn.L => {} }; match g { Mn.K(x) => { println(f"d5b {x.len()}") } Mn.L => {} }; }
    { let mut m: Map[i64, i64] = Map.new(); m.insert(1, 2); let g = Mv.K { m: m, n: 3 }; match g { Mv.K { m, n } => { println(f"d6a {m.len()} {n}") } Mv.L => {} }; match g { Mv.K { m, n } => { println(f"d6b {m.len()} {n}") } Mv.L => {} }; }
    { let g = Mt.K(mk(), hs("t")); match g { Mt.K(x, s) => { println(f"d7a {x.len()} {s.len()}") } Mt.L => {} }; match g { Mt.K(x, s) => { println(f"d7b {x.len()} {s.len()}") } Mt.L => {} }; }
    { let g = Mm.K(mk()); if let Mm.K(x) = g { println(f"d8a {x.len()}") }; if let Mm.K(x) = g { println(f"d8b {x.len()}") }; }
    { let g = Mm.K(mk()); let h = g; let m = take(g); println(f"d9 {m.len()}"); match h { Mm.K(x) => { println(f"d9b {x.len()}") } Mm.L => {} }; }
    { let g = Mm.K(mk()); let mut t = 0; for i in 0..3 { match g { Mm.K(x) => { t = t + x.len(); } Mm.L => {} } }; println(f"d10 {t}"); }
    { let g = Mm.K(mk()); match g { Mm.K(_) => { println("d11a") } Mm.L => {} }; match g { Mm.K(x) => { println(f"d11b {x.len()}") } Mm.L => {} }; }
    { let g = Mm.K(mk()); let v: Vec[Map[i64, String]] = match g { Mm.K(x) => [x], Mm.L => [] }; match g { Mm.K(x) => { println(f"d12 {v.len()} {x.len()}") } Mm.L => {} }; }
    { let mut m: Map[String, Vec[String]] = Map.new(); m.insert(hs("k"), [hs("v")]); let g = Mk.K(m); println("d13"); }
    println("end")
}"#;
    let want = "d1a 1\nd1b 1\nd2a 1\nd2b 1\nd3a 1\nd3b 1\nd4a 1\nd4b 1\nd5a 1\nd5b 1\nd6a 1 3\nd6b 1 3\nd7a 1 25\nd7b 1 25\nd8a 1\nd8b 1\nd9 1\nd9b 1\nd10 3\nd11a\nd11b 1\nd12 1 1\nd13\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
