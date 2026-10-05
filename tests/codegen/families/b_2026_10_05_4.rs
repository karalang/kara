//! B-2026-10-05-4 — a `shared enum` whose payload is a plain user enum
//! holding a `shared` handle (`shared enum H { Z(V), .. }` over
//! `enum V { Z(M), .. }`) leaked that payload on every release, and an arm
//! that moved the payload out of the box (`H.Z(v) => keepv(v)`, or a nested
//! `H.Z(V.Z(M.My(x)))` leaf) left a second handle to the box reading freed
//! memory.

use super::*;

/// B-2026-10-05-4 — release with no match, a whole payload moved into a
/// by-value param, a `shared` leaf, a nested `Vec` leaf two boxes down, each
/// with and without a second handle; struct-variant spellings, `if let`, a
/// payload handed out of a function, and a `Vec` of such enums.
#[test]
fn e2e_shared_enum_value_payload_with_shared_handle_is_owned_once() {
    let src = r#"
shared struct S { v: Vec[String] }
shared enum M { My(Vec[String]), N }
enum V { Z(M), Q { s: S, t: String }, N }
shared enum H { Z(V), W { v: V, k: i64 }, N }
fn mlen(m: ref M) -> i64 { match m { M.My(x) => x.len(), M.N => 0 } }
fn vlen(v: ref V) -> i64 { match v { V.Z(m) => mlen(m), V.Q { s, t } => s.v.len() + t.len(), V.N => -1 } }
fn getv(h: H) -> V { match h { H.Z(v) => v, H.W { v, k } => v, H.N => V.N } }
fn mk(k: i64) -> H { if k > 0 { H.Z(V.Z(M.My(["x", "y"]))) } else { H.W { v: V.Q { s: S { v: ["p"] }, t: "tt" }, k: 3 } } }
fn keepv(v: V) -> i64 { vlen(v) }
fn keepm(m: M) -> i64 { mlen(m) }
fn main() {
    { let s = H.Z(V.Z(M.N)); println("a") }
    { let s = mk(1); match s { H.Z(v) => println(f"b{keepv(v)}"), _ => println("o") } }
    { let s = mk(1); match s { H.Z(V.Z(m)) => println(f"c{keepm(m)}"), _ => println("o") } }
    { let s = mk(1); match s { H.Z(V.Z(M.My(x))) => println(f"d{x.len()}"), _ => println("o") } }
    { let s = mk(1); let t = s; match s { H.Z(v) => println(f"e{keepv(v)}"), _ => println("o") }; match t { H.Z(v) => println(f"e{vlen(v)}"), _ => println("o") } }
    { let s = mk(1); let t = s; match s { H.Z(V.Z(m)) => println(f"f{keepm(m)}"), _ => println("o") }; match t { H.Z(V.Z(m)) => println(f"f{mlen(m)}"), _ => println("o") } }
    { let s = mk(1); let t = s; match s { H.Z(V.Z(M.My(x))) => println(f"g{x.len()}"), _ => println("o") }; match t { H.Z(V.Z(M.My(x))) => println(f"g{x.len()}"), _ => println("o") } }
    { let s = H.Z(V.N); match s { H.Z(v) => println(f"h{vlen(v)}"), _ => println("o") } }
    { let mut i = 0; while i < 3 { let s = mk(i); match s { H.Z(v) => println(f"i{keepv(v)}"), _ => println("o") }; i = i + 1; } }
    { let s = mk(0); println(f"j{match s { H.W { v, k } => vlen(v) + k, _ => 0 }}") }
    { let s = mk(0); let t = s; let v = getv(s); println(f"k{vlen(v)}"); match t { H.W { v: V.Q { s, t }, k } => println(f"k{s.v.len()}{t}"), _ => println("o") } }
    { let s = mk(1); let t = s; if let H.Z(V.Z(M.My(x))) = s { println(f"l{x.len()}") }; if let H.Z(V.Z(M.My(x))) = t { println(f"l{x.len()}") } }
    { let s = mk(1); let t = s; let v = getv(s); let w = getv(t); println(f"m{vlen(v)}{vlen(w)}") }
    { let hs: Vec[H] = [mk(1), mk(0), mk(1)]; let mut n = 0; for h in hs { n = n + vlen(getv(h)); }; println(f"n{n}") }
    { let s = mk(0); let t = s; match s { H.W { v: V.Q { s, t }, k } => println(f"o{s.v.len()}{t}"), _ => println("o") }; match t { H.W { v: V.Q { s, t }, k } => println(f"o{s.v.len()}{t}"), _ => println("o") } }
    { let s = mk(1); let t = s; match s { H.Z(V.Z(m)) => { let q = m; println(f"p{mlen(q)}") }, _ => println("o") }; println(f"p{vlen(getv(t))}") }
    println("end")
}
"#;
    let want = "a\nb2\nc2\nd2\ne2\ne2\nf2\nf2\ng2\ng2\nh-1\no\ni2\ni2\nj6\nk3\nk1tt\nl2\nl2\nm22\nn7\no1tt\no1tt\np2\np2\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
