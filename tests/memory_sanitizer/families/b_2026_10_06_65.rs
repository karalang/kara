//! B-2026-10-06-65: the ordered key index of a `String`-keyed `SortedMap`

use super::*;

/// The index holds bit copies of each key, so a `String` key's copy reads the
/// map's own buffer. Removing a key has to take it out of the index before
/// the buffer is freed, `clear` has to empty the index, and the index dies
/// with the map. Each ordered query below runs right after one of those, on
/// keys long enough to own a heap buffer.
#[test]
fn asan_sorted_map_string_key_index() {
    assert_clean_asan_run(
        r#"fn pick(m: ref SortedMap[String, i64], at: String) -> String {
    match m.ceiling(at) {
        Some((k, v)) => return f"{k}={v}",
        None => return "none",
    }
}

fn main() {
    let mut m: SortedMap[String, i64] = SortedMap.new();
    m.insert(f"pear-with-a-name-longer-than-inline-{1}", 1);
    m.insert(f"apple-with-a-name-longer-than-inline-{2}", 2);
    m.insert(f"fig-with-a-name-longer-than-inline-{3}", 3);
    println(pick(m, "b"));
    m.remove(f"fig-with-a-name-longer-than-inline-{3}");
    println(pick(m, "b"));
    m.insert(f"banana-with-a-name-longer-than-inline-{4}", 4);
    println(pick(m, "b"));
    m.clear();
    println(pick(m, "b"));
    m.insert(f"kiwi-with-a-name-longer-than-inline-{5}", 5);
    match m.max() {
        Some((k, v)) => println(f"max {k}={v}"),
        None => println("max none"),
    }
}
"#,
        &[
            "fig-with-a-name-longer-than-inline-3=3",
            "pear-with-a-name-longer-than-inline-1=1",
            "banana-with-a-name-longer-than-inline-4=4",
            "none",
            "max kiwi-with-a-name-longer-than-inline-5=5",
        ],
        "sorted_map_string_key_index",
    );
}
