//! B-2026-10-01-19 — a STRUCT field moved out of a heap-boxed `Option`/`Result`
//! payload binding (`Result.Err(e) => { let i = e.inner; .. }` over an `E2 {
//! inner: S }` too wide for the payload area). The arm binding is a deboxed
//! private copy, so the move-out's zero landed in that copy while the box's own
//! memory drop read the box: `inner`'s buffers were freed through `i` and again
//! through the box, an invalid free with no output at -O0. A `{ptr,len,cap}`
//! field (`let v = e.v`) already mirrored its zero into the box.

use super::*;

/// B-2026-10-01-19 — the codegen fixture's program under ASAN.
#[test]
fn asan_boxed_optres_payload_struct_field_move_out() {
    assert_clean_asan_run_min_allocs(
        r#"struct S { v: String, w: Vec[i64] }
struct E2 { inner: S }
struct E4 { inner: S, k: i64 }
struct E5 { inner: S, other: S }
fn mk(k: i64) -> S { S { v: f"err-heap-string-long-enough-{k}", w: [1, 2, 3] } }
fn take(s: S) -> i64 { println(s.v); s.v.len() }
fn a1() -> i64 { let r: Result[i64, E2] = Result.Err(E2 { inner: mk(8) }); match r { Result.Ok(v) => v, Result.Err(e) => { let i = e.inner; println(i.v); i.w.len() + i.v.len() } } }
fn a3() -> i64 { let r: Option[E2] = Option.Some(E2 { inner: mk(8) }); match r { Option.None => 0, Option.Some(e) => { let i = e.inner; println(i.v); i.w.len() + i.v.len() } } }
fn a6() -> i64 { let r: Result[i64, E4] = Result.Err(E4 { inner: mk(8), k: 1 }); match r { Result.Ok(v) => v, Result.Err(e) => { let i = e.inner; println(i.v); i.w.len() + e.k } } }
fn b1() -> i64 { let r: Result[i64, E5] = Result.Err(E5 { inner: mk(8), other: mk(9) }); match r { Result.Ok(v) => v, Result.Err(e) => { let i = e.inner; let o = e.other; println(o.v); i.v.len() + o.w.len() } } }
fn b4() -> i64 { let mut n = 0; for k in 0..3 { let r: Result[i64, E2] = Result.Err(E2 { inner: mk(k) }); match r { Result.Ok(v) => { n = n + v; }, Result.Err(e) => { let i = e.inner; println(i.v); n = n + i.v.len(); } } } n }
fn b6() -> i64 { let r: Result[i64, E2] = Result.Err(E2 { inner: mk(8) }); if let Result.Err(e) = r { let i = e.inner; println(i.v); return i.w.len() } 0 }
fn b7() -> i64 { let r: Result[i64, E5] = Result.Err(E5 { inner: mk(8), other: mk(9) }); match r { Result.Ok(v) => v, Result.Err(e) => { let i = e.inner; let o = e.other; println(o.v.len()); take(i) } } }
fn main() { println(a1()); println(a3()); println(a6()); println(b1()); println(b4()); println(b6()); println(b7()); println("end") }"#,
        &[
            "err-heap-string-long-enough-8",
            "32",
            "err-heap-string-long-enough-8",
            "32",
            "err-heap-string-long-enough-8",
            "4",
            "err-heap-string-long-enough-9",
            "32",
            "err-heap-string-long-enough-0",
            "err-heap-string-long-enough-1",
            "err-heap-string-long-enough-2",
            "87",
            "err-heap-string-long-enough-8",
            "3",
            "29",
            "err-heap-string-long-enough-8",
            "29",
            "end",
        ],
        "asan_boxed_optres_payload_struct_field_move_out",
        3,
    );
}
