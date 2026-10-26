//! `format!(..)` is Rust syntax, and Kāra writes an f-string. When the
//! literal's holes are all `{}` / `{:spec}` with one argument each, the
//! error carries the edit that writes that f-string, so `karac fix` repairs
//! the call (B-2026-10-24-12); a call the rewrite cannot carry over as
//! written keeps the message with no fix.

fn repaired(src: &str) -> (String, usize) {
    let result = karac::parse(src);
    let reports = result
        .errors
        .iter()
        .filter(|e| e.message.contains("`format!(...)` is Rust syntax"))
        .count();
    assert_eq!(
        result.errors.len(),
        reports,
        "only the macro is reported: {:?}",
        result.errors
    );
    let mut edits: Vec<_> = result.fix_edits.values().cloned().collect();
    edits.extend(result.fix_diffs.values().flatten().cloned());
    edits.sort_by_key(|e| std::cmp::Reverse(e.offset));
    let mut out = src.to_string();
    for e in edits {
        out.replace_range(e.offset..e.offset + e.length, &e.replacement);
    }
    (out, reports)
}

#[test]
fn format_macro_is_rewritten_to_an_f_string() {
    let src = "fn main() {\n\
               let n = 3;\n\
               let s = format!(\"bias {}, n {:03}\", n + 1, n);\n\
               let t = format!(\"{{{}}} of {}\", s.len(), v(n),);\n\
               let u = format!(\"plain\");\n\
               println(s);\n\
               println(t);\n\
               println(u);\n\
               }\n\
               fn v(n: i64) -> i64 { n * 2 }\n";
    let (out, reports) = repaired(src);
    assert_eq!(reports, 3);
    for want in [
        "let s = f\"bias {n + 1}, n {n:03}\";",
        "let t = f\"{{{s.len()}}} of {v(n)}\";",
        "let u = f\"plain\";",
    ] {
        assert!(out.contains(want), "missing {want:?} in:\n{out}");
    }
    let reparsed = karac::parse(&out);
    assert!(reparsed.errors.is_empty(), "{:?}", reparsed.errors);
    let run = karac::mir::lower::run_source(&out).unwrap_or_else(|e| panic!("build: {e}"));
    assert_eq!(run.output, "bias 4, n 003\n{13} of 6\nplain\n");
}

#[test]
fn format_macro_without_a_one_to_one_rewrite_has_no_fix() {
    for call in [
        // A hole that names its argument, by position or by name.
        "format!(\"{0} {0}\", n)",
        "format!(\"{n}\", n = n)",
        // The counts disagree, either way.
        "format!(\"{} {}\", n)",
        "format!(\"{}\", n, n)",
        // A spec an f-string does not take (Debug is deferred).
        "format!(\"{:?}\", n)",
        // Argument text that cannot sit in a hole as written.
        "format!(\"{}\", g(\"a\"))",
        "format!(\"{}\", g(\"{\"))",
        // The format string is not a plain literal.
        "format!(r\"{}\", n)",
        "format!(f, n)",
    ] {
        let src = format!(
            "fn g(s: String) -> String {{ s }}\nfn main() {{\nlet n = 1;\nlet f = \"{{}}\";\nlet s = {call};\nprintln(s);\n}}\n"
        );
        let result = karac::parse(&src);
        assert!(
            result
                .errors
                .iter()
                .any(|e| e.message.contains("`format!(...)` is Rust syntax")),
            "{call}: {:?}",
            result.errors
        );
        assert!(
            result.fix_edits.is_empty() && result.fix_diffs.is_empty(),
            "{call}: no fix, got {:?} {:?}",
            result.fix_edits,
            result.fix_diffs
        );
    }
}
