//! A reserved keyword written as a local before a bitwise compound
//! assignment (`|=`, `&=`, `^=`, `<<=`, `>>=`) gets the reserved-keyword
//! error once and the `r#` fix, as it already did before `=`, `+=` and the
//! arithmetic compound assignments. Before, `or |= nums[i];` in LeetCode's
//! Rust template for kata 2044 was reported twice with no edit, so `karac
//! fix` escaped every other `or` and stopped there.

fn fix_all(src: &str) -> String {
    let result = karac::parse(src);
    let mut edits: Vec<_> = result.fix_edits.values().cloned().collect();
    edits.sort_by_key(|e| std::cmp::Reverse(e.offset));
    let mut fixed = src.to_string();
    for e in edits {
        fixed.replace_range(e.offset..e.offset + e.length, &e.replacement);
    }
    fixed
}

#[test]
fn kata_2044_keyword_before_bitwise_compound_assignment_fixes_to_raw_name() {
    let src = "fn main() {\n\
               let nums = [3, 5, 6];\n\
               let mut or = 0;\n\
               for i in 0..3 {\n\
               or |= nums[i];\n\
               }\n\
               let mut and = 7;\n\
               and &= 5;\n\
               let mut type = 1;\n\
               type ^= 3;\n\
               type <<= 2;\n\
               type >>= 1;\n\
               println(or);\n\
               println(and);\n\
               println(type);\n\
               }\n";
    let result = karac::parse(src);
    let msgs: Vec<_> = result.errors.iter().map(|e| e.message.as_str()).collect();
    assert_eq!(msgs.len(), 11, "one error per keyword use: {msgs:#?}");
    assert!(
        msgs.iter().all(|m| m.contains("is a reserved keyword")),
        "no cascade: {msgs:#?}"
    );
    let fixed = fix_all(src);
    let reparsed = karac::parse(&fixed);
    assert!(reparsed.errors.is_empty(), "{:?}\n{fixed}", reparsed.errors);
    let run = karac::mir::lower::run_source(&fixed).unwrap_or_else(|e| panic!("build: {e}"));
    assert_eq!(run.output, "7\n5\n4\n");
}
