//! B-2026-09-20-22 — a `Drop` struct owning a `shared` field, bound out of
//! a by-value enum param's payload and moved on (assigned over a local,
//! rebound, returned), ran its `Drop` body twice compiled: once as the arm
//! binding's body and once for the local that received it.

use super::*;

/// B-2026-09-20-22 — arm, rebind, tuple-variant, two-assign, return and
/// `if let` spellings over heap `String`s inside the `shared` field, so the
/// memory half is exercised alongside the body count the stdout pins.
#[test]
fn asan_shared_field_payload_view_moved_on_runs_its_body_once() {
    assert_clean_asan_run_min_allocs(
        r#"shared struct Sh { s: String }
struct Wsh { h: Sh }
impl Drop for Wsh { fn drop(mut ref self) { let hh = self.h; println(f"d:{hh.s.len()}") } }
struct Wsx { h: Sh, s: String }
impl Drop for Wsx { fn drop(mut ref self) { let hh = self.h; println(f"dx:{hh.s.len()}:{self.s.len()}") } }
enum Esh { A(Wsh), B }
enum Esx { A(Wsx), B }
enum E2 { A(Wsh, i64), B }
fn big(p: String, k: i64) -> String { f"{p}-heap-string-longer-than-sso-{k}" }
fn mk(p: String, k: i64) -> Wsh { return Wsh { h: Sh { s: big(p, k) } } }
fn a1(b: Esh) -> i64 { let mut out = mk("o", 1); match b { Esh.A(w) => { out = w; } Esh.B => { } } let hh = out.h; return hh.s.len(); }
fn a2(b: Esx) -> i64 { let mut out = Wsx { h: Sh { s: big("o", 2) }, s: big("os", 2) }; match b { Esx.A(w) => { let m = w; out = m; } Esx.B => { } } return out.s.len(); }
fn a3(b: E2) -> i64 { let mut out = mk("o", 3); match b { E2.A(w, k) => { out = w; } E2.B => { } } let hh = out.h; return hh.s.len(); }
fn a4(b: Esh, c: Esh) -> i64 { let mut out = mk("o", 4); match b { Esh.A(w) => { out = w; } Esh.B => { } } match c { Esh.A(v) => { out = v; } Esh.B => { } } let hh = out.h; return hh.s.len(); }
fn a5(b: Esh) -> i64 { match b { Esh.A(w) => { let m = w; let hh = m.h; return hh.s.len(); } Esh.B => { return 0; } } }
fn a6(b: Esh) -> i64 { let mut out = mk("o", 6); if let Esh.A(w) = b { out = w; } let hh = out.h; return hh.s.len(); }
fn main() {
    println(f"{a1(Esh.A(mk("p", 1)))}");
    println(f"{a1(Esh.B)}");
    println(f"{a2(Esx.A(Wsx { h: Sh { s: big("p", 2) }, s: big("ps", 2) }))}");
    println(f"{a3(E2.A(mk("p", 3), 9))}");
    println(f"{a4(Esh.A(mk("p", 4)), Esh.A(mk("q", 4)))}");
    println(f"{a5(Esh.A(mk("p", 5)))}");
    let e = Esh.A(mk("p", 6));
    println(f"{a6(e)}");
}
"#,
        &[
            "d:31", "d:31", "31", "d:31", "31", "dx:31:32", "dx:31:32", "32", "d:31", "d:31", "31",
            "d:31", "d:31", "d:31", "31", "d:31", "31", "d:31", "31", "d:31",
        ],
        "B-2026-09-20-22 shared-field payload view moved on",
        10,
    );
}
