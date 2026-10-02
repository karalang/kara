//! B-2026-09-23-21 -- an unannotated `let` bound to a type-qualified
//! associated call's `Array` return owns its elements' heap.

use super::*;

/// B-2026-09-23-21 — `let b = G[i64].mk()` over `-> Array[S, 2]` with a heap
/// `String` per element. Before, the binding recorded no element type, so the
/// build failed outright; this pins that, once it resolves, every element's
/// buffer is freed exactly once.
#[test]
fn asan_type_qualified_assoc_call_array_return_frees_elements() {
    assert_clean_asan_run(
        r#"struct S { n: i64, s: String }
struct G[T] { k: T }
impl[T] G[T] {
    fn mk() -> Array[S, 2] { return [S { n: 1, s: f"aaaaaaaaaaaaaaaaaaaaaaaaaaa{1}" }, S { n: 2, s: f"bbbbbbbbbbbbbbbbbbbbbbbbbbbb{2}" }] }
}
fn main() {
    let b = G[i64].mk();
    println(f"y{b[0].n}{b[0].s}{b[1].s}");
}
"#,
        &["y1aaaaaaaaaaaaaaaaaaaaaaaaaaa1bbbbbbbbbbbbbbbbbbbbbbbbbbbb2"],
        "B-2026-09-23-21 type-qualified assoc call Array return",
    );
}
