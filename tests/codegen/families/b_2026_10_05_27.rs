//! B-2026-10-05-27 — a generic shared enum at `Option` of a shared type releases the inner handle.

use super::*;

/// B-2026-10-05-27 — `shared enum G[T] { Y(T), N }` instantiated at
/// `Option[M]` (`M` a `shared enum`) releases the handle its boxed payload
/// holds when the last reference goes (`a`, `b`, `n`), and a whole-payload arm
/// binding `G.Y(o)` owns its own reference, so it can be read twice (`c`),
/// returned (`d`, `e`), moved into a `let` (`f`), read through an alias (`g`),
/// rebound in a loop (`k`) or pushed into a `Vec` (`p`) without either owner
/// releasing it twice.
///
/// Before: the release fn freed the payload box without releasing the handle
/// inside it, losing `M` (40 B) and its strings on every construction.
#[test]
fn e2e_shgen_option_shared_payload_releases_handle() {
    let src = r#"shared enum G[T] { Y(T), N }
shared enum M { My(Vec[String]), N }
fn mkv(s: String) -> Vec[String] { [s.clone() + "1", s + "2"] }
fn m_len(m: M) -> i64 { match m { M.My(v) => v.len(), M.N => 0 } }
fn olen(o: Option[M]) -> i64 { match o { Some(m) => m_len(m), None => 0 } }
fn a() -> i64 { let h: G[Option[M]] = G.Y(Some(M.My(mkv("a")))); 1 }
fn b() -> i64 { let x = M.My(mkv("b")); let h: G[Option[M]] = G.Y(Some(x)); match x { M.My(v) => v.len(), M.N => 0 } }
fn c() -> i64 { let h: G[Option[M]] = G.Y(Some(M.My(mkv("c")))); match h { G.Y(o) => olen(o) + olen(o), G.N => 0 } }
fn d() -> Option[M] { let h: G[Option[M]] = G.Y(Some(M.My(mkv("d")))); match h { G.Y(o) => o, G.N => None } }
fn e() -> Option[M] { let h: G[Option[M]] = G.Y(Some(M.My(mkv("e")))); if let G.Y(o) = h { return o } None }
fn f() -> i64 { let h: G[Option[M]] = G.Y(Some(M.My(mkv("f")))); let o2 = match h { G.Y(o) => o, G.N => None }; olen(o2) }
fn g() -> i64 { let h: G[Option[M]] = G.Y(Some(M.My(mkv("g")))); let h2 = h; let x = match h { G.Y(o) => olen(o), G.N => 0 }; let y = match h2 { G.Y(Some(m)) => m_len(m), _ => 0 }; x + y }
fn k() -> i64 { let mut t = 0; let mut i = 0; while i < 3 { let h: G[Option[M]] = G.Y(Some(M.My(mkv("k")))); t = t + match h { G.Y(o) => { let q = o; olen(q) }, G.N => 0 }; i = i + 1; } t }
fn n() -> i64 { let h: G[Option[M]] = G.Y(None); let z: G[Option[M]] = G.N; let x = match h { G.Y(o) => olen(o), G.N => 7 }; let y = match z { G.Y(o) => olen(o), G.N => 7 }; x + y }
fn p() -> Vec[Option[M]] { let h: G[Option[M]] = G.Y(Some(M.My(mkv("p")))); let mut v: Vec[Option[M]] = []; match h { G.Y(o) => { v.push(o) }, G.N => {} } v }
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
    println(f"k{k()}");
    println(f"n{n()}");
    let v = p();
    println(f"p{v.len()}")
}
"#;
    let want = "a1\nb2\nc4\nd2\netrue\nf2\ng4\nk6\nn7\np1\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
