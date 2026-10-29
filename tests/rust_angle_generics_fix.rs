//! Rust's generic arguments in a type, `Result<(), String>`, are reported
//! once at the `<` with the edit that writes Kāra's brackets
//! (`Result[(), String]`), and the list then parses as Kāra's, so nothing
//! cascades and one `karac fix` run repairs the type (B-2026-10-29-23).

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
fn rust_angle_generics_in_types_are_rewritten_to_brackets() {
    let src = "struct R {\n    n: i64,\n}\n\n\
               impl R {\n    fn check(ref self) -> Result<(), String> {\n        Ok(())\n    }\n\n\
               \x20   fn other(ref self) -> i64 {\n        self.n\n    }\n}\n\n\
               fn nest(x: Option<Vec<Vec<i64>>>, m: Map<String, Vec<i64>>) -> i64 {\n\
               \x20   let v: Vec<i64> = Vec.new();\n\
               \x20   let k = 3 as i64 < 4;\n\
               \x20   if k { v.len() as i64 + 1 } else { 0 }\n}\n\n\
               fn main() {\n    let r = R { n: 1 };\n    println(r.other());\n\
               \x20   println(nest(None, Map.new()));\n}\n";
    let (out, messages) = repaired(src);
    assert_eq!(messages.len(), 4, "one report per outer list: {messages:?}");
    assert!(
        messages[0].starts_with("`Result<..>` is Rust's spelling"),
        "{messages:?}"
    );
    for want in [
        "-> Result[(), String] {",
        "x: Option[Vec[Vec[i64]]], m: Map[String, Vec[i64]]",
        "let v: Vec[i64] = Vec.new();",
        // A comparison after a cast is left alone.
        "let k = 3 as i64 < 4;",
    ] {
        assert!(out.contains(want), "missing {want:?} in:\n{out}");
    }
    let reparsed = karac::parse(&out);
    assert!(reparsed.errors.is_empty(), "{:?}", reparsed.errors);
    let run = karac::mir::lower::run_source(&out).unwrap_or_else(|e| panic!("build: {e}"));
    assert_eq!(run.output, "1\n1\n");
}

#[test]
fn an_unclosed_angle_list_is_reported_with_no_fix() {
    let src = "fn f(x: Vec<i64) -> i64 {\n    0\n}\n";
    let result = karac::parse(src);
    assert!(
        result.errors[0]
            .message
            .starts_with("`Vec<..>` is Rust's spelling"),
        "{:?}",
        result.errors
    );
    assert!(result.fix_edits.is_empty() && result.fix_diffs.is_empty());
}
