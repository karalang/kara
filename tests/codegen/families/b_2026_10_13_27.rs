//! B-2026-10-13-27 -- the standard collections are `Default`, which lets
//! `take[T: Default]` reach a collection place; the legacy backend has no
//! lowering for a collection's `default()` and refuses it rather than
//! leaving the old value in place.

use super::*;

/// Before the refusal, `take(mut b)` on a `Vec[i64]` printed `2 2` (the
/// place kept its elements and the result shared them), and on a
/// `Vec[String]` the run crashed with a double free.
#[test]
fn legacy_take_of_a_collection_is_refused_not_miscompiled() {
    let msg = codegen_error(
        "fn main() {\n    let mut b: Vec[i64] = vec![1, 2];\n    let g = take(mut b);\n    \
         println(f\"{b.len()} {g.len()}\");\n}\n",
    );
    assert!(
        msg.contains("`Vec.default()` (as `take` uses it) is not lowered by this backend"),
        "{msg}"
    );
}
