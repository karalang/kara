//! B-2026-10-02-31 — a `Map` unwrapped by `?` leaked, and once owned was
//! freed twice by a later move. The ASAN twin of the codegen fixture.

use super::*;

/// B-2026-10-02-31 — the codegen fixture's table under ASAN.
#[test]
fn asan_question_unwrapped_map_has_one_owner() {
    assert_clean_asan_run_min_allocs(
        r#"fn hs(k: i64) -> String { f"heap-string-long-enough-{k}" }
fn mk(k: i64) -> Option[Map[String, i64]] { if k > 5 { let mut m = Map.new(); m.insert(hs(k), k); Option.Some(m) } else { Option.None } }
fn mkr(k: i64) -> Result[Map[String, i64], String] { if k > 5 { let mut m = Map.new(); m.insert(hs(k), k); Result.Ok(m) } else { Result.Err(hs(0)) } }
fn mks(k: i64) -> Option[Set[String]] { if k > 5 { let mut s: Set[String] = Set.new(); s.insert(hs(k)); Option.Some(s) } else { Option.None } }
fn mkv(k: i64) -> Option[Map[String, Vec[String]]] { if k > 5 { let mut m: Map[String, Vec[String]] = Map.new(); m.insert(hs(k), [hs(1), hs(2)]); Option.Some(m) } else { Option.None } }
fn cnt(m: Map[String, i64]) -> i64 { m.len() }
fn h1(k: i64) -> Option[i64] { let x = mk(k)?; Option.Some(x.len()) }
fn h2(k: i64) -> Option[i64] { let r = mk(k); let x = r?; Option.Some(x.len()) }
fn h3(k: i64) -> Result[i64, String] { let x = mkr(k)?; Result.Ok(x.len()) }
fn h4(k: i64) -> Option[i64] { let x = mk(k)?; let y = x; Option.Some(y.len() + 10) }
fn h5(k: i64) -> Option[i64] { let x = mks(k)?; Option.Some(x.len() + 20) }
fn h6(k: i64) -> Option[i64] { let x = mkv(k)?; Option.Some(x.len() + 30) }
fn g7(k: i64) -> Option[Map[String, i64]] { let x = mk(k)?; Option.Some(x) }
fn h7(k: i64) -> Option[i64] { match g7(k) { Option.Some(m) => Option.Some(m.len() + 40), Option.None => Option.None } }
fn h8(k: i64) -> Option[i64] { let mut x = mk(k)?; x.insert(hs(99), 1); Option.Some(x.len() + 50) }
fn h9(k: i64) -> Option[i64] { let x = mk(k)?; Option.Some(cnt(x) + 60) }
fn show(o: Option[i64]) { match o { Option.Some(v) => println(v), Option.None => println("none") } }
fn main() {
    show(h1(8)); show(h1(2));
    show(h2(8)); show(h2(2));
    match h3(8) { Result.Ok(v) => println(v), Result.Err(e) => println(e) };
    match h3(2) { Result.Ok(v) => println(v), Result.Err(e) => println(e) };
    show(h4(8)); show(h4(2));
    show(h5(8)); show(h5(2));
    show(h6(8)); show(h6(2));
    show(h7(8)); show(h7(2));
    show(h8(8)); show(h8(2));
    show(h9(8)); show(h9(2));
    println("end");
}"#,
        &[
            "1",
            "none",
            "1",
            "none",
            "1",
            "heap-string-long-enough-0",
            "11",
            "none",
            "21",
            "none",
            "31",
            "none",
            "41",
            "none",
            "52",
            "none",
            "61",
            "none",
            "end",
        ],
        "asan_question_unwrapped_map_has_one_owner",
        20,
    );
}
