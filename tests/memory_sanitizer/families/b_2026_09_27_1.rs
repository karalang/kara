//! B-2026-09-27-1 -- a generic method that hands a by-value argument back on
//! only some paths frees it once.

use super::*;

/// B-2026-09-27-1 — a GENERIC METHOD that hands a `shared`-field struct back
/// on only some paths (`fn gpk[T](ref self, v: T, c: bool, w: T) -> T { if c
/// { return v } return w }`) kept the caller's cleanup beside the result's:
/// `k.gpk(s, true, w)` read freed memory at both exits and ran a `Drop` body
/// twice (`dS6 dS6 dS5 c5 dS5`), and aborted in `malloc` under `karac run`.
/// The free generic fn has taken the memory per path since B-2026-09-25-40;
/// `compute_handback_safe_params` declined every generic method. Covers both
/// exits, a `Drop`-less struct, a `mut ref self` receiver, a method-own type
/// param on a generic impl, a fresh-temp argument, a discarded result, and a
/// loop body.
#[test]
fn asan_generic_method_conditional_hand_back_freed_once() {
    assert_clean_asan_run(
        r#"shared struct Sh { k: i64 }
struct S2 { h: Sh, id: i64 }
impl Drop for S2 { fn drop(mut ref self) { println(f"dS{self.id}") } }
struct S3 { h: Sh, id: i64 }
struct K { n: i64 }
impl K { fn gpk[T](ref self, v: T, c: bool, w: T) -> T { if c { return v } return w } }
impl K { fn gpm[T](mut ref self, v: T, c: bool, w: T) -> T { self.n = self.n + 1; if c { return v } return w } }
struct Q[U] { u: U }
impl[U] Q[U] { fn qpk[T](ref self, v: T, c: bool, w: T) -> T { if c { return v } return w } }
fn main() {
    let k = K { n: 1 };
    { let s = S3 { h: Sh { k: 98 }, id: 1 }; let w = S3 { h: Sh { k: 4 }, id: 2 }; let t = k.gpk(s, true, w); println(f"a{t.h.k}") }
    { let s = S3 { h: Sh { k: 98 }, id: 1 }; let w = S3 { h: Sh { k: 4 }, id: 2 }; let t = k.gpk(s, false, w); println(f"b{t.h.k}") }
    { let s = S2 { h: Sh { k: 1 }, id: 5 }; let w = S2 { h: Sh { k: 2 }, id: 6 }; let t = k.gpk(s, true, w); println(f"c{t.id}") }
    { let s = S2 { h: Sh { k: 1 }, id: 7 }; let w = S2 { h: Sh { k: 2 }, id: 8 }; let t = k.gpk(s, false, w); println(f"d{t.id}") }
    { let mut m = K { n: 1 }; let s = S2 { h: Sh { k: 1 }, id: 9 }; let w = S2 { h: Sh { k: 2 }, id: 10 }; let t = m.gpm(s, true, w); println(f"e{t.id} {m.n}") }
    { let q = Q { u: 3 }; let s = S2 { h: Sh { k: 1 }, id: 11 }; let w = S2 { h: Sh { k: 2 }, id: 12 }; let t = q.qpk(s, false, w); println(f"f{t.id}") }
    { let w = S2 { h: Sh { k: 2 }, id: 14 }; let t = k.gpk(S2 { h: Sh { k: 1 }, id: 13 }, true, w); println(f"g{t.id}") }
    { let s = S2 { h: Sh { k: 1 }, id: 15 }; let w = S2 { h: Sh { k: 2 }, id: 16 }; k.gpk(s, true, w); println("h") }
    let mut i = 0;
    while i < 2 { let s = S2 { h: Sh { k: 1 }, id: 20 + i }; let w = S2 { h: Sh { k: 2 }, id: 30 + i }; let t = k.gpk(s, i == 0, w); println(f"l{t.id}"); i = i + 1; }
    println("end")
}
"#,
        &[
            "a98", "b4", "dS6", "c5", "dS5", "dS7", "d8", "dS8", "dS10", "e9 2", "dS9", "dS11",
            "f12", "dS12", "dS14", "g13", "dS13", "dS16", "dS15", "h", "dS30", "l20", "dS20",
            "dS21", "l31", "dS31", "end",
        ],
        "asan_generic_method_conditional_hand_back_freed_once",
    );
}
