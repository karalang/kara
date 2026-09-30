//! B-2026-09-20-7 -- several parts of a by-value `Option` / `Result`
//! payload handed back together in a tuple or struct literal.

use super::*;

/// B-2026-09-20-7 — the heap-bearing spelling of the codegen fixture
/// `e2e_literal_of_payload_parts_handed_back_runs_each_body_once`: each
/// handed-back part owns a `String`, so a part whose body the caller's walk
/// runs a second time would also free its buffer a second time. Covers the
/// spellings that fix changes on the compiled backends (a named local handed
/// to a callee returning a literal of its payload's parts) beside their
/// fresh-temp twins, and a `Result` whose `Err` arm builds a literal from its
/// own binding, which must not stand the `Ok` arm's mask down.
///
/// Not here: one part from EACH level (`(t.0.0, t.1)`), which runs every
/// body but leaks the inner sibling's buffer on the unfixed tree as well;
/// that is B-2026-09-30-15.
#[test]
fn asan_literal_of_payload_parts_handed_back_frees_each_once() {
    assert_clean_asan_run(
        r#"
struct R { id: i64, t: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct P { a: R, b: R }
fn mk(i: i64) -> R { return R { id: i, t: f"payloadpayload{i}" }; }
fn v8(o: Option[((R, R), i64)]) -> (R, R) { match o { Option.Some(t) => { return (t.0.0, t.0.1); } Option.None => { return (mk(0), mk(0)); } } }
fn h1(o: Option[(R, R)]) -> (R, R) { match o { Option.Some(t) => { return (t.0, t.1); } Option.None => { return (mk(0), mk(0)); } } }
fn sl(o: Option[(R, R)]) -> P { match o { Option.Some(t) => { return P { a: t.1, b: t.0 }; } Option.None => { return P { a: mk(0), b: mk(0) }; } } }
fn r0(o: Result[(R, R), i64]) -> R { match o { Result.Ok(t) => { return t.1; } Result.Err(e) => { return R { id: e, t: f"e{e}" }; } } }
fn rs(o: Result[((R, R), i64), i64]) -> (R, R) { match o { Result.Ok(t) => { return (t.0.0, t.0.1); } Result.Err(e) => { return (mk(e), mk(e)); } } }
fn main() {
    { let g = v8(Option.Some(((mk(1), mk(2)), 3))); println(f"v8t {g.0.t}{g.1.t}") }
    { let a = Option.Some(((mk(3), mk(4)), 3)); let g = v8(a); println(f"v8n {g.0.t}{g.1.t}") }
    { let g = h1(Option.Some((mk(5), mk(6)))); println(f"h1t {g.0.t}{g.1.t}") }
    { let a = Option.Some((mk(7), mk(8))); let g = h1(a); println(f"h1n {g.0.t}{g.1.t}") }
    { let a = Option.Some((mk(15), mk(16))); let g = sl(a); println(f"sln {g.a.t}{g.b.t}") }
    { let a: Result[(R, R), i64] = Result.Ok((mk(17), mk(18))); let g = r0(a); println(f"r0n {g.t}") }
    { let a: Result[((R, R), i64), i64] = Result.Ok(((mk(19), mk(20)), 3)); let g = rs(a); println(f"rsn {g.0.t}{g.1.t}") }
    { let g = rs(Result.Err(21)); println(f"rse {g.0.t}{g.1.t}") }
}
"#,
        &[
            "v8t payloadpayload1payloadpayload2",
            "dR1",
            "dR2",
            "v8n payloadpayload3payloadpayload4",
            "dR3",
            "dR4",
            "h1t payloadpayload5payloadpayload6",
            "dR5",
            "dR6",
            "h1n payloadpayload7payloadpayload8",
            "dR7",
            "dR8",
            "sln payloadpayload16payloadpayload15",
            "dR15",
            "dR16",
            "dR17",
            "r0n payloadpayload18",
            "dR18",
            "rsn payloadpayload19payloadpayload20",
            "dR19",
            "dR20",
            "rse payloadpayload21payloadpayload21",
            "dR21",
            "dR21",
        ],
        "asan_literal_of_payload_parts_handed_back_frees_each_once",
    );
}
