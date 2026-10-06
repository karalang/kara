//! B-2026-10-06-40: `len` / `is_empty` on a fixed array of heap elements

use super::*;

/// Asking a `String` or `Vec` array its length reads no element, so the
/// elements are still freed exactly once when the array dies.
#[test]
fn asan_array_len_of_heap_elements() {
    assert_clean_asan_run(
        r#"fn main() {
    let s: Array[String, 2] = [f"string-longer-than-the-inline-limit-{1}", f"short{2}"];
    let v: Array[Vec[i64], 2] = [[1, 2, 3], [4]];
    println(f"{s.len()} {s.is_empty()} {v.len()} {v.is_empty()}");
    println(s[0]);
}
"#,
        &["2 false 2 false", "string-longer-than-the-inline-limit-1"],
        "array_len_of_heap_elements",
    );
}
