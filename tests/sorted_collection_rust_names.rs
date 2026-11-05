//! B-2026-11-05-41: Rust's names for what a sorted collection spells
//! otherwise (`first_key_value`, a sorted set's `first`) used to get a bare
//! `no method`. The diagnostic now names Kāra's method, and the methods the
//! library spec added (`values_mut`, `retain`, `pop_first`, ...) check clean.
//! Their behaviour is pinned by corpus/design/ok_map_retain_values_mut_and_pops.

fn errors(src: &str) -> Vec<String> {
    karac::check_source(src)
        .into_iter()
        .filter(|d| d.severity == "error")
        .map(|d| d.message)
        .collect()
}

#[test]
fn sorted_collection_rust_names_point_at_karas_method() {
    for (decl, call, want) in [
        (
            "SortedMap[i64, i64] = SortedMap.new()",
            "first_key_value",
            "`first()`",
        ),
        (
            "SortedMap[i64, i64] = SortedMap.new()",
            "last_key_value",
            "`last()`",
        ),
        ("SortedSet[i64] = SortedSet.new()", "first", "`min()`"),
        ("SortedSet[i64] = SortedSet.new()", "last", "`max()`"),
    ] {
        let src = format!("fn main() {{\n    let c: {decl};\n    let x = c.{call}();\n}}\n");
        let errs = errors(&src);
        assert_eq!(errs.len(), 1, "{call}: {errs:?}");
        assert!(errs[0].contains(&format!("no method '{call}'")), "{errs:?}");
        assert!(errs[0].contains(want), "{call}: {errs:?}");
    }
}

#[test]
fn sorted_collection_spec_methods_check_clean() {
    let src = "fn main() {\n    let mut m: SortedMap[i64, String] = SortedMap.new();\n    m.insert(1, \"a\".to_string());\n    for v in m.values_mut() {\n        v.push_str(\"!\");\n    }\n    m.retain(|k, v| *k > 0);\n    let a = m.first();\n    let b = m.pop_last();\n    let mut h: Map[i64, i64] = Map.new();\n    h.insert(1, 2);\n    h.retain(|k, v| *v > 1);\n    let ks: Vec[i64] = h.clone().into_keys().collect();\n    let vs: Vec[i64] = h.into_values().collect();\n    let mut s: SortedSet[i64] = SortedSet.new();\n    s.insert(3);\n    s.retain(|x| *x > 1);\n    let p = s.pop_first();\n}\n";
    let errs = errors(src);
    assert!(errs.is_empty(), "{errs:?}");
}
