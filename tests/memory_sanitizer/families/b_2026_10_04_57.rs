//! B-2026-10-04-57 — overwriting a whole tuple element runs the displaced
//! value's `Drop` body; and B-2026-10-04-72 — a named source moved into a
//! tuple element is owned once.

use super::*;

/// `t.0 = <new>` ran the displaced element's body nowhere: the store freed
/// its memory through the memory-only drop while `x = ..` and `w.r = ..` run
/// the body at the store. Covers an owned local, a `mut ref` param, a struct
/// element with a `Drop` field, a user enum and an `Option` element (both
/// overwritten twice, the second time with a payload-free variant), a store in
/// a loop, and a conditional move-out on the path that did not take it.
#[test]
fn asan_tuple_elem_overwrite_runs_displaced_body() {
    assert_clean_asan_run(
        r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct P { r: R, k: i64 }
enum E { A(R), B }
fn mk(i: i64) -> R { R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn viaref(a: mut ref (R, i64)) { a.0 = mk(2); println("f") }
fn moved(c: bool) { let mut t: (R, i64) = (mk(30), 0); if c { let x = t.0; println(f"x{x.id}"); } t.0 = mk(31); println(f"c{t.1}") }
fn main() {
    let mut t: (R, i64) = (mk(5), 0); t.0 = mk(6); println("tuple");
    let mut r: (R, i64) = (mk(1), 7); viaref(mut r); println(f"r{r.1}");
    let mut p: (P, i64) = (P { r: mk(10), k: 1 }, 0); p.0 = P { r: mk(11), k: 2 }; println("p");
    let mut e: (E, i64) = (E.A(mk(20)), 0); e.0 = E.A(mk(21)); println("e1"); e.0 = E.B; println("e2");
    let mut o: (Option[R], i64) = (Some(mk(25)), 0); o.0 = Some(mk(26)); println("o1"); o.0 = None; println("o2");
    let mut l: (R, i64) = (mk(40), 0); for i in 41..43 { l.0 = mk(i); } println(f"l{l.0.id}");
    moved(false);
    println(f"end{t.1}{p.1}{e.1}{o.1}");
}
"#,
        &[
            "dR5", "tuple", "dR1", "f", "r7", "dR2", "dR10", "p", "dR20", "e1", "dR21", "e2",
            "dR25", "o1", "dR26", "o2", "dR40", "dR41", "l42", "dR42", "dR30", "c0", "dR31",
            "end0000", "dR11", "dR6",
        ],
        "tuple_elem_overwrite_displaced_body",
    );
}

/// `t.1 = n` with a named struct, user enum or `Option` source left `n`'s
/// cleanup armed: the element and `n` both freed the payload's heap
/// (`free(): double free detected`), and an all-scalar struct ran `n`'s body
/// at the store and again with the tuple.
#[test]
fn asan_named_source_moved_into_tuple_elem_owned_once() {
    assert_clean_asan_run(
        r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
enum E { A(R), B }
fn mk(i: i64) -> R { R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn main() {
    let mut t: (i64, R) = (0, mk(5)); let n = mk(6); t.1 = n; println(f"t {t.1.s}");
    let mut e: (i64, E) = (0, E.A(mk(7))); let m = E.A(mk(8)); e.1 = m; println("e");
    let mut u: (Option[R], i64) = (Some(mk(9)), 1); let o = Some(mk(10)); u.0 = o; println(f"u{u.1}");
    println(f"end{t.0}{e.0}");
}
"#,
        &[
            "dR5",
            "t heap-string-longer-than-sso-6",
            "dR7",
            "e",
            "dR9",
            "u1",
            "dR10",
            "end00",
            "dR8",
            "dR6",
        ],
        "named_source_into_tuple_elem",
    );
}
