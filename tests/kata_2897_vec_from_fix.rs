//! `Vec::from([2, 6, 5, 8])` (kata 2897, Rust's way to build a `Vec` from an
//! array), which `karac fix` carries as far as `Vec.from(..)`. Kāra has no
//! `Vec.from`, and the check used to refuse it bare, "no associated function
//! 'from' on type 'Vec'". It now says what to write: a list literal is
//! already a `Vec` where no array is asked for, and any other sequence is
//! copied by `.to_vec()` (library/collections.md). The fix writes that, and
//! the call keeps its `Vec` type so nothing after it cascades. An argument
//! that is not a sequence is named with no fix.

fn fixed(src: &str) -> String {
    let mut edits: Vec<karac::SourceFix> = karac::check_source(src)
        .into_iter()
        .flat_map(|d| d.fixes)
        .collect();
    edits.sort_by_key(|e| std::cmp::Reverse(e.offset));
    let mut out = src.to_string();
    for e in edits {
        out.replace_range(e.offset..e.offset + e.length, &e.replacement);
    }
    out
}

fn messages(src: &str) -> Vec<String> {
    karac::check_source(src)
        .into_iter()
        .filter(|d| d.severity == "error")
        .map(|d| d.message)
        .collect()
}

#[test]
fn kata_2897_vec_from_a_literal_or_a_sequence_is_named_and_fixed() {
    let src = "fn total(v: Vec[i64]) -> i64 {\n\
               \x20   v.iter().sum()\n}\n\n\
               fn main() {\n\
               \x20   let a: Array[i64, 3] = [1, 2, 3];\n\
               \x20   let s = a[0..2];\n\
               \x20   let v = Vec.from(a);\n\
               \x20   let w = Vec.from(s);\n\
               \x20   println(f\"{total(v)} {total(w)} {total(Vec.from([4, 5]))}\");\n}\n";
    let said = messages(src);
    assert_eq!(
        said.len(),
        3,
        "one error per call, nothing cascading: {said:?}"
    );
    assert!(
        said.iter().any(
            |m| m.contains("a list literal is already a `Vec`") && m.contains("write `[4, 5]`")
        ),
        "{said:?}"
    );
    assert!(
        said.iter()
            .any(|m| m.contains("copied into a new `Vec` by `.to_vec()`")
                && m.contains("write `a.to_vec()`")),
        "{said:?}"
    );
    let out = fixed(src);
    for want in [
        "let v = a.to_vec();",
        "let w = s.to_vec();",
        "{total([4, 5])}",
    ] {
        assert!(out.contains(want), "missing {want:?} in:\n{out}");
    }
    assert!(
        messages(&out).is_empty(),
        "the fixed source checks clean:\n{out}"
    );
    let run = karac::mir::lower::run_source(&out).unwrap_or_else(|e| panic!("build: {e}"));
    assert_eq!(run.output, "6 3 9\n");
}

#[test]
fn kata_2897_vec_from_a_string_is_named_without_a_fix() {
    let src = "fn main() {\n    let v = Vec.from(\"abc\");\n}\n";
    let said = messages(src);
    assert!(
        said.iter().any(
            |m| m.contains("no associated function 'from' on type 'Vec'")
                && m.contains("`collect`")
        ),
        "{said:?}"
    );
    assert_eq!(fixed(src), src, "no fix for a non-sequence argument");
}
