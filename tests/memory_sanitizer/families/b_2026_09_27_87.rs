//! B-2026-09-27-87 — a NAMED `Option` argument whose payload is a struct laid
//! inline with a `shared` field (`Option[ShP]`, `ShP { i: ShIn, n }`) is handed
//! to the callee when the callee gives that payload an owner of its own (a
//! rebind in an arm, an `unwrap`-family result, a push into a container), as
//! the same value passed as a fresh temp already was. The caller's let-site
//! walk used to stay armed beside the callee's owner and read the `shared`
//! field's block after the callee released it.

use super::*;

/// B-2026-09-27-87 — rebind in `if let` / `let .. else` / `match`,
/// `unwrap_or`, `let p = w.unwrap()`, a push into a `mut ref Vec`, the row's own
/// `w.unwrap().n`, and a rebind called in a loop, each with a named argument and
/// a fresh temp. Before the fix every named call read freed memory at `-O0`
/// (`malloc(): unaligned tcache chunk detected` on this program).
#[test]
fn asan_optres_param_shared_field_payload_taken_by_callee_has_one_owner() {
    assert_clean_asan_run_min_allocs(
        r#"
shared struct ShIn { s: String }
struct ShP { i: ShIn, n: i64 }
fn mk(n: i64) -> ShP { return ShP { i: ShIn { s: f"shared-heap-string-longer-than-sso-{n}" }, n: n }; }
fn g1(w: Option[ShP]) -> i64 { if let Some(p) = w { let q = p; return q.n; } return 0; }
fn g2(w: Option[ShP]) -> i64 { return w.unwrap_or(mk(0)).n; }
fn g3(w: Option[ShP]) -> i64 { let Some(p) = w else { return 0; }; let q = p; q.n }
fn g4(w: Option[ShP]) -> i64 { match w { Some(p) => { let q = p; q.n } None => 0 } }
fn g5(w: Option[ShP]) -> i64 { let p = w.unwrap(); p.n }
fn g6(v: mut ref Vec[Option[ShP]], w: Option[ShP]) -> i64 { v.push(w); return v.len(); }
fn g7(w: Option[ShP]) -> i64 { return w.unwrap().n; }
fn main() {
    let a1 = Some(mk(1)); println(f"j{g1(a1)}"); println(f"k{g1(Some(mk(2)))}"); let z: Option[ShP] = None; println(f"z{g1(z)}");
    let a2 = Some(mk(3)); println(f"j{g2(a2)}"); println(f"k{g2(Some(mk(4)))}");
    let a3 = Some(mk(5)); println(f"j{g3(a3)}"); println(f"k{g3(Some(mk(6)))}");
    let a4 = Some(mk(7)); println(f"j{g4(a4)}"); println(f"k{g4(Some(mk(8)))}");
    let a5 = Some(mk(9)); println(f"j{g5(a5)}"); println(f"k{g5(Some(mk(10)))}");
    let mut v: Vec[Option[ShP]] = Vec.new();
    let a6 = Some(mk(11)); println(f"j{g6(mut v, a6)}"); println(f"k{g6(mut v, Some(mk(12)))}");
    let a7 = Some(mk(13)); println(f"j{g7(a7)}"); println(f"k{g7(Some(mk(14)))}");
    let mut i = 20; while i < 23 { let o = Some(mk(i)); println(f"l{g1(o)}"); i = i + 1; }
    println(f"v{v.len()}");
    println("end")
}
"#,
        &[
            "j1", "k2", "z0", "j3", "k4", "j5", "k6", "j7", "k8", "j9", "k10", "j1", "k2", "j13",
            "k14", "l20", "l21", "l22", "v2", "end",
        ],
        "asan_optres_param_shared_field_payload_taken_by_callee_has_one_owner",
        20,
    );
}
