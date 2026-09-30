//! B-2026-09-20-26 -- a user enum's `Array` payload rebound into a local inside
//! the arm runs each heap-bearing element's `Drop` body once and frees it once.

use super::*;

/// B-2026-09-20-26 — the heap-bearing spelling of the rebinding double: each
/// element's `Drop` body reads its `String`, so a second run after the free
/// would read freed memory. Covers the consuming arm, a read-only arm and the
/// `if let` spelling.
#[test]
fn asan_rebound_array_payload_runs_each_body_once() {
    assert_clean_asan_run_min_allocs(
        r#"struct H { id: i64, s: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}:{self.s.len()}") } }
enum Eh { A(Array[H, 2]), B }
fn mk(i: i64) -> H { H { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn main() {
    let e: Eh = Eh.A([mk(1), mk(2)]);
    match e { Eh.A(v) => { let u = v; println(f"au{u[0].id}") } Eh.B => { println("no") } }
    let f: Eh = Eh.A([mk(3), mk(4)]);
    match f { Eh.A(v) => { println(f"r{v[1].s.len()}") } Eh.B => { println("no") } }
    let g: Eh = Eh.A([mk(5), mk(6)]);
    if let Eh.A(v) = g { let u = v; println(f"il{u[1].id}") }
    println("end")
}
"#,
        &[
            "au1", "dH1:29", "dH2:29", "r29", "dH3:29", "dH4:29", "il6", "dH5:29", "dH6:29", "end",
        ],
        "asan_rebound_array_payload_runs_each_body_once",
        6,
    );
}
