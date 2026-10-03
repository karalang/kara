//! B-2026-10-02-59 / B-2026-10-03-17 -- a `shared enum` is an RC handle to
//! the move checker and to `.clone()`, and a cloned handle passed by value is
//! released once.

use super::*;

/// B-2026-10-03-17 — `h.clone()` on a shared handle returns a `+1` that no
/// binding holds. Passed straight to a by-value param it was never released,
/// so the whole object leaked: `sg(a.clone())` over a `shared struct`, and
/// (once B-2026-10-02-59 admitted the call) `eg(e.clone())` over a
/// `shared enum`. Covers a free fn, a method argument, a passthrough callee,
/// an index receiver, a field receiver, a discarded clone, and a clone stored
/// into a `Vec` literal, which already had an owner. The enum's arm binds
/// its `String` payload with `_`: a BY-VALUE arm binding out of a shared enum
/// still empties the shared object (B-2026-09-28-59, open).
#[test]
fn asan_cloned_shared_handle_argument_released_once() {
    assert_clean_asan_run(
        r#"shared struct S { s: String, k: i64 }
shared enum E { P(i64, String), Q }
impl S { fn take(self, o: S) -> i64 { return o.k + self.k; } }
fn sg(s: S) -> i64 { return s.k; }
fn eg(e: E) -> i64 { match e { E.P(n, _) => { return n; } E.Q => { return 0; } } }
fn idn(s: S) -> S { return s; }
struct H { s: S }
fn main() {
    let a = S { s: f"a{1}", k: 10 };
    let e = E.P(3, f"bb{2}");
    println(f"c1 {sg(a.clone())} {eg(e.clone())}");
    println(f"c2 {a.take(a.clone())}");
    println(f"c3 {sg(idn(a.clone()))}");
    let v: Vec[S] = [a.clone()];
    let w: Vec[E] = [e.clone(), e.clone()];
    println(f"c4 {sg(v[0].clone())} {eg(w[1].clone())} {w.len()}");
    let h = H { s: a.clone() };
    println(f"c5 {sg(h.s.clone())}");
    a.clone();
    e.clone();
    println(f"c6 {sg(a)} {eg(e)} {a.k} {eg(e)}");
}
"#,
        &[
            "c1 10 3",
            "c2 20",
            "c3 10",
            "c4 10 3 2",
            "c5 10",
            "c6 10 3 10 3",
        ],
        "B-2026-10-03-17 cloned shared handle passed by value is released once",
    );
}
