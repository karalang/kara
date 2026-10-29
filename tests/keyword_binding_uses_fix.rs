//! A binding named by a keyword (`let and = ..`) is recovered as `r#and`
//! with that rename as its fix, and each later use of it before a block
//! (`if and { .. }`) now gets the same rename, so one `karac fix` run leaves
//! a program that checks, with no `else` cascade and no duplicate report
//! inside a closure (B-2026-10-29-13).

#[test]
fn uses_of_a_keyword_named_binding_get_the_same_rename() {
    let src = "fn pick(op: Str, a: bool, b: bool) -> bool {\n\
               \x20   let and = op == \"&\";\n\
               \x20   if and { a and b } else { a or b }\n}\n\n\
               fn many(op: Str, xs: Vec[bool]) -> i64 {\n\
               \x20   let and = op == \"&\";\n\
               \x20   let ys: Vec[bool] = xs.iter().map(|x| if and { x and true } else { x or false }).collect();\n\
               \x20   ys.len() as i64\n}\n\n\
               fn main() {\n\
               \x20   println(f\"{pick(\"&\", true, false)} {pick(\"|\", true, false)}\");\n\
               \x20   println(many(\"&\", [true, false].to_vec()));\n}\n";
    let result = karac::parse(src);
    let at: Vec<(usize, usize)> = result
        .errors
        .iter()
        .map(|e| (e.span.line, e.span.column))
        .collect();
    assert_eq!(at, [(2, 9), (3, 8), (7, 9), (8, 46)], "{:?}", result.errors);
    let mut edits: Vec<_> = result.fix_edits.values().cloned().collect();
    assert_eq!(edits.len(), 4);
    edits.sort_by_key(|e| std::cmp::Reverse(e.offset));
    let mut out = src.to_string();
    for e in edits {
        out.replace_range(e.offset..e.offset + e.length, &e.replacement);
    }
    assert!(out.contains("if r#and { a and b }"), "{out}");
    let run = karac::mir::lower::run_source(&out).unwrap_or_else(|e| panic!("build: {e}"));
    assert_eq!(run.output, "false true\n2\n");
}

#[test]
fn a_keyword_before_a_block_with_no_such_binding_is_not_renamed() {
    // `else {` after a stray keyword is not a binding's use.
    let src = "fn main() {\n    if true { println(1); } else { println(2); }\n}\n";
    assert!(karac::parse(src).errors.is_empty());
    let src = "fn f(x: bool) {\n    if and { println(1); }\n}\n";
    let result = karac::parse(src);
    assert!(result.fix_edits.is_empty(), "{:?}", result.errors);
}
