//! B-2026-10-06-106 -- an index store into a module-level `let mut` array
//! failed `karac build`. Overwriting a heap element must free the old value
//! exactly once.

use super::*;

#[test]
fn asan_index_store_into_module_mut_string_array() {
    assert_clean_asan_run(
        r#"let mut NAMES: Array[String, 2] = ["a", "bb"];
let mut COUNTS: Array[i64, 2] = [1, 2];
fn main() {
    NAMES[0] = f"z{1}";
    NAMES[0] = f"w{2}";
    NAMES[1] = f"v{3}";
    COUNTS[1] = 9;
    println(f"{NAMES[0]} {NAMES[1]} {COUNTS[1]}");
}
"#,
        &["w2 v3 9"],
        "asan_index_store_into_module_mut_string_array",
    );
}
