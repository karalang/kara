//! `eprint` joined the prelude beside `eprintln` (B-2026-11-20-53), so
//! Rust's `eprint!(..)` has a Kāra twin: the `!` is deleted, and a call with
//! format arguments is rewritten to an f-string, as for `eprintln!`.

fn repaired(src: &str) -> String {
    let result = karac::parse(src);
    assert!(
        result
            .errors
            .iter()
            .all(|e| e.message.contains("no macros")),
        "only the macros are reported: {:?}",
        result.errors
    );
    let mut edits: Vec<_> = result.fix_edits.values().cloned().collect();
    edits.extend(result.fix_diffs.values().flatten().cloned());
    assert_eq!(edits.len(), 2, "both calls get a fix: {edits:?}");
    edits.sort_by_key(|e| std::cmp::Reverse(e.offset));
    let mut out = src.to_string();
    for e in edits {
        out.replace_range(e.offset..e.offset + e.length, &e.replacement);
    }
    out
}

#[test]
fn eprint_macro_is_repaired_to_the_prelude_function() {
    let src =
        "fn main() {\n    let n = 3;\n    eprint!(\"plain\");\n    eprint!(\"n={}\", n);\n}\n";
    let out = repaired(src);
    assert!(out.contains("eprint(\"plain\");"), "{out}");
    assert!(out.contains("eprint(f\"n={n}\");"), "{out}");
    let reparsed = karac::parse(&out);
    assert!(reparsed.errors.is_empty(), "{:?}", reparsed.errors);
}
