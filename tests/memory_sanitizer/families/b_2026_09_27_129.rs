//! B-2026-09-27-129 — the receiver of an `is_some` / `is_none` / `is_ok` /
//! `is_err` probe that is a fresh temporary dies at the probe: its payload's
//! `Drop` body runs there and its box is freed.

use super::*;

/// B-2026-09-27-129 — the ASAN twin of
/// `e2e_probed_fresh_temp_optres_runs_payload_body`: each probed temp's box and
/// `String` are freed once, at the probe.
#[test]
fn asan_probed_fresh_temp_optres_is_freed() {
    assert_clean_asan_run_min_allocs(
        r#"
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct S { r: R, s: String }
fn mk(i: i64) -> S { S { r: R { id: i }, s: f"heap-string-longer-than-sso-{i}" } }
fn mk2(i: i64) -> Option[S] { Some(mk(i)) }
fn mkr(i: i64) -> Option[R] { Some(R { id: i }) }
fn mkok(i: i64) -> Result[S, i64] { Ok(mk(i)) }
fn mkerr(i: i64) -> Result[i64, S] { Err(mk(i)) }
fn wrapo[T](x: T) -> Option[T] { Some(x) }
struct F { n: i64 }
impl F { fn mko(ref self, i: i64) -> Option[S] { Some(mk(i + self.n)) } }
fn main() {
    let a = mk2(1).is_some();
    println(f"a{a}");
    let b = mk2(2).is_none();
    println(f"b{b}");
    if mk2(3).is_some() { println("y3") }
    let c = mkr(4).is_some();
    println(f"c{c}");
    let d = mkok(5).is_ok() and mkerr(6).is_err();
    println(f"d{d}");
    let e = mkok(7).is_err();
    println(f"e{e}");
    let g = wrapo(mk(8)).is_some();
    println(f"g{g}");
    let f = F { n: 100 };
    let h = f.mko(9).is_some();
    println(f"h{h}");
    let k = Some(mk(10)).is_some();
    println(f"k{k}");
    let mut v = vec![mk(11), mk(12)];
    let p = v.pop().is_some();
    println(f"p{p} {v.len()}");
    let mut i = 13;
    while mk2(i).is_some() and i < 15 { i += 1; }
    println(f"i{i}");
    println("end")
}
"#,
        &[
            "d1", "atrue", "d2", "bfalse", "d3", "y3", "d4", "ctrue", "d5", "d6", "dtrue", "d7",
            "efalse", "d8", "gtrue", "d109", "htrue", "d10", "ktrue", "d12", "ptrue 1", "d11",
            "d13", "d14", "d15", "i15", "end",
        ],
        "asan_probed_fresh_temp_optres_is_freed",
        20,
    );
}
