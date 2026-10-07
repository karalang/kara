//! B-2026-10-06-134 — a `shared enum`'s boxed `Option[shared]` payload is released through its box.

use super::*;

/// B-2026-10-06-134 — a non-generic `shared enum` whose payload is an
/// `Option` of a `shared` type (`H { Y(Option[M]), N }`, `Hs` over a shared
/// struct, a self-referential list `L { Cons(i64, Option[L]), Nil }`)
/// releases the handle its payload box holds and frees the box, for `Some`
/// (`a`) and `None` (`b`), and a whole-payload arm binding owns its own
/// reference: read twice (`c`), returned (`d`, `e`, `tail`), moved into a
/// `let` (`f`), read through an alias (`g`) and walked recursively (`k`).
///
/// Before: the release treated the payload word as the handle itself, when
/// the constructor had stored a pointer to a box holding the `Option`; it
/// decremented the box's tag word, leaked the handle and a `None` box, and
/// a binding read twice read freed memory (`c` printed `c2`).
#[test]
fn asan_shared_enum_option_shared_payload_box_release() {
    assert_clean_asan_run_min_allocs(
        r#"shared struct S { name: String, n: i64 }
shared enum H { Y(Option[M]), N }
shared enum Hs { Y(Option[S]), N }
shared enum M { My(Vec[String]), N }
shared enum L { Cons(i64, Option[L]), Nil }
fn mkv(s: String) -> Vec[String] { [s.clone() + "1", s + "2"] }
fn m_len(m: M) -> i64 { match m { M.My(v) => v.len(), M.N => 0 } }
fn olen(o: Option[M]) -> i64 { match o { Some(m) => m_len(m), None => 0 } }
fn a() -> i64 { let h = H.Y(Some(M.My(mkv("a")))); 1 }
fn b() -> i64 { let h = H.Y(None); 2 }
fn c() -> i64 { let h = H.Y(Some(M.My(mkv("c")))); match h { H.Y(o) => olen(o) + olen(o), H.N => 0 } }
fn d() -> Option[M] { let h = H.Y(Some(M.My(mkv("d")))); match h { H.Y(o) => o, H.N => None } }
fn e() -> Option[M] { let h = H.Y(Some(M.My(mkv("e")))); if let H.Y(o) = h { return o } None }
fn f() -> i64 { let h = H.Y(Some(M.My(mkv("f")))); let o2 = match h { H.Y(o) => o, H.N => None }; olen(o2) }
fn g() -> i64 { let h = Hs.Y(Some(S { name: "abc".to_string(), n: 3 })); let h2 = h; let x = match h { Hs.Y(o) => match o { Some(s) => s.n, None => 0 }, Hs.N => 0 }; let y = match h2 { Hs.Y(Some(s)) => s.n, _ => 0 }; x + y }
fn sum(l: L) -> i64 { match l { L.Cons(v, n) => match n { Some(x) => v + sum(x), None => v }, L.Nil => 0 } }
fn tail(l: L) -> Option[L] { match l { L.Cons(_, n) => n, L.Nil => None } }
fn k() -> i64 { let mut l = L.Cons(0, None); let mut i = 1; while i < 5 { l = L.Cons(i, Some(l)); i = i + 1; } let t = tail(l); sum(l) + match t { Some(x) => sum(x), None => 0 } }
fn main() {
    println(f"a{a()}");
    println(f"b{b()}");
    println(f"c{c()}");
    let od = d();
    println(f"d{olen(od)}");
    let oe = e();
    println(f"e{oe.is_some()}");
    println(f"f{f()}");
    println(f"g{g()}");
    println(f"k{k()}")
}"#,
        &["a1", "b2", "c4", "d2", "etrue", "f2", "g6", "k16"],
        "asan_shared_enum_option_shared_payload_box_release",
        8,
    );
}
