//! B-2026-10-03-50 -- an uncalled function's same-named nested-envelope
//! param made another function's `if let` leak its payload's heap.

use super::*;

/// B-2026-10-03-50 — `nested_boxed_payload_vars` is keyed by NAME and was
/// never cleared between functions. `rd`'s `Result[Option[R], i64]` param `x`
/// stayed in it, so `ie`'s `Option[Option[R]]` param `x` (emitted after, and
/// never itself a member) took the nested family's retract-on-any-binding
/// rule: its box freed R's struct without R's fields. `rd` is never called;
/// renaming either param made the leak go away.
#[test]
fn asan_same_named_param_in_earlier_fn_keeps_leaf_drop() {
    assert_clean_asan_run(
        r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn rd(x: Result[Option[R], i64]) -> i64 {
    if let Ok(Some(w)) = x { return w.id }
    return 0
}
fn eat(r: R) { println(f"e{r.id}") }
fn ie(x: Option[Option[R]]) -> i64 {
    if let Some(Some(w)) = x { eat(w); return 1 }
    return 0
}
fn main() {
    println(ie(Some(Some(mk(6)))));
    println("end");
}
"#,
        &["e6", "dR6", "1", "end"],
        "same_named_param_in_earlier_fn",
    );
}
