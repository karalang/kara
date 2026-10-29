//! Two Rust spellings of a mutable binding, each reported once with the
//! edit that writes Kāra's, and parsed on so nothing cascades:
//! `ref mut m` in a pattern is Kāra's `mut ref m` (B-2026-10-29-34), and
//! `for x in mut v` is `for x in v.iter_mut()` (B-2026-10-29-16).

fn repaired(src: &str) -> (String, Vec<String>) {
    let result = karac::parse(src);
    let messages: Vec<String> = result.errors.iter().map(|e| e.message.clone()).collect();
    let mut edits: Vec<_> = result.fix_edits.values().cloned().collect();
    edits.extend(result.fix_diffs.values().flatten().cloned());
    edits.sort_by_key(|e| std::cmp::Reverse(e.offset));
    let mut out = src.to_string();
    for e in edits {
        out.replace_range(e.offset..e.offset + e.length, &e.replacement);
    }
    (out, messages)
}

#[test]
fn rust_ref_mut_in_a_pattern_is_swapped_to_mut_ref() {
    let src = "enum Dt {\n    NoDt,\n    Map(Vec[String]),\n}\n\n\
               fn add(d: mut ref Dt, s: Str) {\n\
               \x20   if let Dt.Map(ref mut m) = d {\n        m.push(s.to_string());\n    }\n}\n\n\
               fn bump(v: mut ref Vec[Option[i64]]) {\n\
               \x20   match v[0] {\n        Some(ref mut n) => *n += 1,\n        None => {}\n    }\n}\n\n\
               fn main() {\n    let mut d = Dt.Map(Vec.new());\n    add(mut d, \"a\");\n\
               \x20   let mut v = [Some(1)].to_vec();\n    bump(mut v);\n\
               \x20   match d {\n        Dt.Map(m) => println(m.len()),\n        Dt.NoDt => {}\n    }\n\
               \x20   println(v[0].unwrap());\n}\n";
    let (out, messages) = repaired(src);
    assert_eq!(
        messages,
        [
            "`ref mut m` is Rust syntax -- in Kāra write `mut ref m`",
            "`ref mut n` is Rust syntax -- in Kāra write `mut ref n`",
        ],
    );
    assert!(out.contains("Dt.Map(mut ref m) = d"), "{out}");
    assert!(out.contains("Some(mut ref n) =>"), "{out}");
    let run = karac::mir::lower::run_source(&out).unwrap_or_else(|e| panic!("build: {e}"));
    assert_eq!(run.output, "1\n2\n");
}

#[test]
fn rust_for_in_mut_is_rewritten_to_iter_mut() {
    let src = "fn main() {\n    let mut v: Vec[i64] = Vec.new();\n    v.push(1);\n\
               \x20   for x in mut v {\n        *x += 1;\n    }\n    println(f\"{v[0]}\");\n}\n";
    let (out, messages) = repaired(src);
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(
        messages[0].starts_with("`mut v` after `in` is not Kāra"),
        "{messages:?}"
    );
    assert!(out.contains("for x in v.iter_mut() {"), "{out}");
    let run = karac::mir::lower::run_source(&out).unwrap_or_else(|e| panic!("build: {e}"));
    assert_eq!(run.output, "2\n");
}

#[test]
fn for_in_mut_over_a_larger_expression_has_no_fix() {
    // Appending `.iter_mut()` to `0..3` would bind to the `3` alone.
    let src = "fn main() {\n    for x in mut 0..3 {\n        println(x);\n    }\n}\n";
    let result = karac::parse(src);
    assert_eq!(result.errors.len(), 1, "{:?}", result.errors);
    assert!(result.fix_edits.is_empty() && result.fix_diffs.is_empty());
}
