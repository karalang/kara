//! `let row = |r: i64| -> Vec[String] { ..collect() };`: deleting Rust's
//! closure return type leaves a `collect` tail that cannot infer, so when the
//! closure is a `let`'s whole initializer with every parameter typed and a
//! `collect` / `sum` / `product` tail, the fix moves the type onto the
//! binding as an `Fn` (B-2026-10-29-7). Any other closure keeps the
//! delete-only fix.

fn fixed(src: &str) -> String {
    let result = karac::parse(src);
    let mut edits: Vec<_> = result.fix_edits.values().cloned().collect();
    edits.extend(result.fix_diffs.values().flatten().cloned());
    edits.sort_by_key(|e| std::cmp::Reverse(e.offset));
    let mut out = src.to_string();
    for e in edits {
        out.replace_range(e.offset..e.offset + e.length, &e.replacement);
    }
    out
}

#[test]
fn a_collect_closure_moves_its_return_type_onto_the_binding() {
    let src = "fn main() {\n\
               \x20   let levels: Vec[Vec[String]] = [[\"a\".to_string(), \"b\".to_string()].to_vec(), [\"c\".to_string(), \"d\".to_string()].to_vec()].to_vec();\n\
               \x20   let row = |r: i64| -> Vec[String] { levels.iter().map(|l| l[r].clone()).collect() };\n\
               \x20   let total = |n: i64| -> i64 (0..n).sum();\n\
               \x20   let twice = |s: String| -> String { s + s };\n\
               \x20   println(row(1).join(\",\"));\n\
               \x20   println(total(4));\n\
               \x20   println(twice(\"ab\".to_string()));\n}\n";
    let out = fixed(src);
    for want in [
        "let row: Fn(i64) -> Vec[String] = |r: i64| { levels",
        "let total: Fn(i64) -> i64 = |n: i64| (0..n).sum();",
        // Not a `collect` / `sum` / `product` tail: the type is only deleted.
        "let twice = |s: String| { s + s };",
    ] {
        assert!(out.contains(want), "missing {want:?} in:\n{out}");
    }
    let run = karac::mir::lower::run_source(&out).unwrap_or_else(|e| panic!("build: {e}"));
    assert_eq!(run.output, "b,d\n6\nabab\n");
}

#[test]
fn an_untyped_parameter_or_a_typed_binding_keeps_the_delete() {
    for (src, want) in [
        (
            "let row = |r| -> Vec[i64] { v.iter().collect() };",
            "let row = |r| { v.iter().collect() };",
        ),
        (
            "let row: Fn(i64) -> Vec[i64] = |r: i64| -> Vec[i64] { v.iter().collect() };",
            "let row: Fn(i64) -> Vec[i64] = |r: i64| { v.iter().collect() };",
        ),
        (
            "f(|r: i64| -> Vec[i64] { v.iter().collect() });",
            "f(|r: i64| { v.iter().collect() });",
        ),
    ] {
        let src = format!("fn main() {{\n{src}\n}}\n");
        let out = fixed(&src);
        assert!(out.contains(want), "missing {want:?} in:\n{out}");
    }
}
