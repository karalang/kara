//! B-2026-10-04-66 — reassigning a tuple local drops the displaced tuple.

use super::*;

/// B-2026-10-04-66 — reassigning a tuple local drops the DISPLACED tuple
/// before the store: its elements' `Drop` bodies run (a plain struct, a
/// `shared` handle bare, in an `Option` or in a struct field, a nested tuple)
/// and its heap is freed (`String`, `Option[String]`). A value moved out
/// whole runs nothing, a value moved out on one path only runs on the other
/// (`g`/`h`), and a moved element is skipped (`o`, `r`).
///
/// Before: no surface ran a displaced element's body, and compiled code also
/// leaked it (`(H, i64)` 16 B, `(String, i64)` the old string).
#[test]
fn asan_tuple_local_reassign_drops_displaced_value() {
    assert_clean_asan_run_min_allocs(
        r#"shared struct H { id: i64 }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct W { h: H, s: String }
fn mk(i: i64) -> (Option[H], i64) { (Some(H { id: i }), i) }
fn mks(i: i64) -> (H, i64) { (H { id: i }, i) }
fn mkst(i: i64) -> (String, i64) { (f"s{i}xxxxxxxxxxxxxxxxxxxxxxx", i) }
fn mkr(i: i64) -> (R, i64) { (R { id: i }, i) }
fn mkw(i: i64) -> (W, i64) { (W { h: H { id: i }, s: f"w{i}xxxxxxxxxxxxxxxxxxxxxxxx" }, i) }
fn mkn(i: i64) -> ((R, i64), String) { ((R { id: i }, i), f"n{i}xxxxxxxxxxxxxxxxxxxxxxxxx") }
fn main() {
    { let mut p = mkr(1); p = mkr(2); println(f"a{p.1}") }
    { let mut p = mk(3); p = mk(4); println(f"b{p.1}") }
    { let mut p = mkst(5); p = mkst(6); println(f"c{p.1}") }
    { let mut p = (R { id: 7 }, 7); p = (R { id: 8 }, 8); println(f"d{p.1}") }
    { let mut p = mkr(9); for k in 0..2 { p = mkr(k + 10); }; println(f"e{p.1}") }
    { let mut p = mkr(12); let c = true; if c { p = mkr(13); }; println(f"f{p.1}") }
    { let mut pg = mkr(14); let c = true; if c { let qg = pg; println(f"q{qg.1}") }; pg = mkr(15); println(f"g{pg.1}") }
    { let mut p = mkr(16); let c = false; if c { let q = p; println(f"q{q.1}") }; p = mkr(17); println(f"h{p.1}") }
    { let mut p = mks(18); p = mks(19); println(f"i{p.1}") }
    { let mut p = mkn(20); p = mkn(21); println(f"j{p.1}") }
    { let mut p = mkw(22); p = mkw(23); println(f"k{p.1}") }
    { let mut p: (Option[String], i64) = (Some(f"o1xxxxxxxxxxxxxxxxxxxxxxxxxx"), 24); p = (None, 25); println(f"l{p.1}") }
    { let mut p = (mkr(26), mkr(27)); p = (mkr(28), mkr(29)); println(f"m{p.0.1}") }
    { let mut p = mkr(30); p = if true { mkr(31) } else { mkr(32) }; println(f"n{p.1}") }
    { let mut p = mk(33); let a = p.0; p = mk(34); println(f"o{p.1}{a.is_some()}") }
    { let mut p = mk(35); let q = p; p = mk(36); println(f"p{p.1}{q.1}") }
    { let mut p = mkst(37); let a = p.0; p = mkst(38); println(f"r{p.1}{a.len()}") }
    println("end")
}"#,
        &[
            "dR1",
            "a2",
            "dR2",
            "dH3",
            "b4",
            "dH4",
            "c6",
            "dR7",
            "d8",
            "dR8",
            "dR9",
            "dR10",
            "e11",
            "dR11",
            "dR12",
            "f13",
            "dR13",
            "q14",
            "dR14",
            "g15",
            "dR15",
            "dR16",
            "h17",
            "dR17",
            "dH18",
            "i19",
            "dH19",
            "dR20",
            "jn21xxxxxxxxxxxxxxxxxxxxxxxxx",
            "dR21",
            "dH22",
            "k23",
            "dH23",
            "l25",
            "dR26",
            "dR27",
            "m28",
            "dR28",
            "dR29",
            "dR30",
            "n31",
            "dR31",
            "o34true",
            "dH33",
            "dH34",
            "p3635",
            "dH35",
            "dH36",
            "r3826",
            "end",
        ],
        "asan_tuple_local_reassign_drops_displaced_value",
        8,
    );
}
