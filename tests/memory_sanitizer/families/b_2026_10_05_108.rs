//! B-2026-10-05-108: a `flat_map` collect of fresh Strings owns each once.

use super::*;

/// The mapped inner's body is pushed straight into the result, so a String it
/// builds is moved into the Vec once. Bound to a hidden loop variable first,
/// the String was also freed at the end of each iteration and the Vec held
/// freed buffers (measured while writing the fix: a double free).
#[test]
fn asan_flat_map_collect_of_fresh_strings() {
    assert_clean_asan_run(
        r#"fn main() {
    let mut sm: SortedMap[String, i64] = SortedMap.new();
    sm.insert(f"key-longer-than-the-inline-limit-{1}", 2);
    sm.insert(f"key-longer-than-the-inline-limit-{2}", 1);
    let a: Vec[String] = sm.iter().flat_map(|(k, n)| (0..n).map(|i| f"{k}-{i}")).collect();
    println(a.len());
    let b: Vec[String] = sm.iter().flat_map(|(k, n)| (0..n).map(|_| k.clone())).collect();
    println(b[2].len());
    let words = [f"word-longer-than-the-inline-limit-{3}", f"word-longer-than-the-inline-limit-{4}"];
    let c: Vec[String] = words.iter().flat_map(|w| (0..2).map(|i| f"{w}{i}")).collect();
    println(c[3]);
    let mut m: Map[i64, String] = Map.new();
    m.insert(7, f"value-longer-than-the-inline-limit-{7}");
    let d: Vec[String] = m.iter().flat_map(|(k, v)| Vec.filled(2, v.clone() + f"{k}").into_iter()).collect();
    println(d[1].len());
    println("end");
}
"#,
        &[
            "3",
            "34",
            "word-longer-than-the-inline-limit-41",
            "37",
            "end",
        ],
        "flat_map_collect_of_fresh_strings",
    );
}
