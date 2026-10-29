//! The turbofish fix deletes `::<T>` / `::[T]`, and in a `let` with no
//! annotation that alone left `let v = it.collect();`, which cannot infer the
//! collection. When the call is the whole initializer of a `let` binding one
//! name, the fix also writes the type onto the binding, as the message asks
//! (B-2026-10-24-13). Anywhere else deleting the list stays the whole fix.

fn fixed(src: &str) -> String {
    let result = karac::parse(src);
    assert!(
        result
            .errors
            .iter()
            .all(|e| e.message.contains("Rust's turbofish")),
        "only turbofish errors: {:?}",
        result.errors
    );
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
fn turbofish_in_an_unannotated_let_moves_its_type_to_the_binding() {
    let src = "fn main() {\n\
               let marks = \"ab\".chars().collect::<Vec<char>>();\n\
               let mut more = \"cd\".chars().collect::[Vec[char]]();\n\
               let total = (1..4).sum::<i64>();\n\
               let ways = (1..4).product::[i64]();\n\
               more.push('e');\n\
               println(f\"{marks.len()} {more.len()} {total} {ways}\");\n\
               }\n";
    let out = fixed(src);
    for want in [
        "let marks: Vec[char] = \"ab\".chars().collect();",
        "let mut more: Vec[char] = \"cd\".chars().collect();",
        "let total: i64 = (1..4).sum();",
        "let ways: i64 = (1..4).product();",
    ] {
        assert!(out.contains(want), "missing {want:?} in:\n{out}");
    }
    let parsed = karac::parse(&out);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let run = karac::mir::lower::run_source(&out).unwrap_or_else(|e| panic!("build: {e}"));
    assert_eq!(run.output, "2 3 6 6\n");
}

#[test]
fn turbofish_outside_a_whole_let_initializer_is_only_deleted() {
    for (src, want) in [
        // Inside an argument.
        (
            "let n = f(\"ab\".chars().collect::<Vec<char>>());",
            "let n = f(\"ab\".chars().collect());",
        ),
        // A placeholder, which an annotation cannot carry.
        (
            "let w = \"ab\".chars().collect::<Vec<_>>();",
            "let w = \"ab\".chars().collect();",
        ),
        // A method whose type argument is not what it returns.
        ("let r = it.unzip::<Vec<i64>>();", "let r = it.unzip();"),
        // A destructuring pattern, and an annotated binding.
        (
            "let (a, b) = p.unzip::<Vec<i64>>();",
            "let (a, b) = p.unzip();",
        ),
        (
            "let v: Vec[i64] = it.collect::<Vec<i64>>();",
            "let v: Vec[i64] = it.collect();",
        ),
    ] {
        let src = format!("fn main() {{\n{src}\n}}\n");
        let out = fixed(&src);
        assert!(out.contains(want), "missing {want:?} in:\n{out}");
    }
}

/// A `collect` whose result is the receiver of a further call has no type to
/// infer from its use, so deleting the list is not offered: the message
/// names the typed `let` instead, at the turbofish (B-2026-10-29-8). This
/// flips the pin that held the delete-only fix for
/// `.collect::<Vec<char>>().len()`.
#[test]
fn turbofish_on_a_receiver_collect_names_a_typed_let_with_no_fix() {
    for (src, shown) in [
        (
            "let text = \"(\" + lab.iter().map(|v| v.to_string()).collect::[Vec[String]]().join(\", \") + \")\";",
            "let v: Vec[String] = it.collect();",
        ),
        (
            "let n = \"ab\".chars().collect::<Vec<char>>().len();",
            "let v: Vec[char] = it.collect();",
        ),
    ] {
        let src = format!("fn main() {{\n{src}\n}}\n");
        let result = karac::parse(&src);
        assert_eq!(result.errors.len(), 1, "{:?}", result.errors);
        assert!(
            result.errors[0].message.contains("is the receiver of a further call")
                && result.errors[0].message.contains(shown),
            "{}",
            result.errors[0].message
        );
        assert!(result.fix_edits.is_empty() && result.fix_diffs.is_empty());
    }
}

/// `s.parse::<i64>()` names the type to parse to, which Kāra writes as the
/// receiver of an associated call: the fix rewrites it to `i64.parse(s)`
/// rather than deleting the one fact the next diagnostic needs
/// (B-2026-10-29-39). This flips the pin that held the delete,
/// `"4".parse::<i64>()` to `"4".parse()`.
#[test]
fn turbofish_on_parse_is_rewritten_to_an_associated_call() {
    let src = "fn main() {\n\
               let piece = \"42\";\n\
               let v = piece.parse::<i64>().unwrap();\n\
               let w: i64 = (\"1\" + \"7\").parse::[i64]().unwrap() + 1;\n\
               println(f\"{v} {w}\");\n\
               }\n";
    let out = fixed(src);
    for want in [
        "let v = i64.parse(piece).unwrap();",
        // A parenthesized receiver keeps its parentheses around the call.
        "let w: i64 = (i64.parse(\"1\" + \"7\")).unwrap() + 1;",
    ] {
        assert!(out.contains(want), "missing {want:?} in:\n{out}");
    }
    let run = karac::mir::lower::run_source(&out).unwrap_or_else(|e| panic!("build: {e}"));
    assert_eq!(run.output, "42 18\n");
}
