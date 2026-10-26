//! A reserved keyword written as a `for` binding, a match-arm binding or a
//! name before a bitwise operator gets the reserved-keyword error once per
//! occurrence and nothing else: the keyword is read as the `r#` name it
//! fixes to, as it already was in `let`, closure and parameter patterns.
//! Before, `for and in v {}` went on to `Expected expression, found
//! RightBrace` and a false "top-level statements" error at a statement
//! inside `main`, and `match 3 { and => {} }` to the first of those
//! (B-2026-10-24-31).

#[test]
fn keyword_bindings_report_once_and_fix_to_raw_names() {
    let src = "fn count(pairs: SortedMap[i64, i64], x: i64) -> i64 {\n\
               let mut triples = 0;\n\
               for (and, count) in pairs { if and & x == 0 { triples += count; } }\n\
               triples\n\
               }\n\
               fn main() {\n\
               let v = [1, 2];\n\
               for and in v { println(and); }\n\
               match 3 { and => println(and) }\n\
               match 3 { and if and > 1 => println(\"big\"), _ => {} }\n\
               match 4 { and @ 4 => println(and), _ => {} }\n\
               let m: SortedMap[i64, i64] = SortedMap.new();\n\
               println(count(m, 1));\n\
               }\n";
    let result = karac::parse(src);
    let msgs: Vec<_> = result.errors.iter().map(|e| e.message.as_str()).collect();
    assert_eq!(msgs.len(), 10, "one error per `and`: {msgs:#?}");
    assert!(
        msgs.iter()
            .all(|m| m.contains("'and' is a reserved keyword")),
        "no cascade: {msgs:#?}"
    );
    let mut edits: Vec<_> = result.fix_edits.values().cloned().collect();
    edits.sort_by_key(|e| std::cmp::Reverse(e.offset));
    let mut fixed = src.to_string();
    for e in edits {
        fixed.replace_range(e.offset..e.offset + e.length, &e.replacement);
    }
    let reparsed = karac::parse(&fixed);
    assert!(reparsed.errors.is_empty(), "{:?}\n{fixed}", reparsed.errors);
    let run = karac::mir::lower::run_source(&fixed).unwrap_or_else(|e| panic!("build: {e}"));
    assert_eq!(run.output, "1\n2\n3\nbig\n4\n0\n");
}
