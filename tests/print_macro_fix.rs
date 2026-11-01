//! `println!("{}", n)` is Rust syntax, and Kāra's `println` takes one
//! string. Deleting the `!` (the repair a plain `println!("hi")` gets) left
//! `println("{}", n)`, an arity error with no fix, and turned
//! `println!("{n}")` into a call that prints the braces (B-2026-11-01-8).
//! A `println!` / `print!` / `eprintln!` / `panic!` with format arguments
//! now gets the edit that writes them into an f-string, Rust 2021's inline
//! `{name}` captures included; a call the rewrite cannot carry over keeps
//! the message with no fix.

fn repaired(src: &str) -> (String, usize) {
    let result = karac::parse(src);
    let reports = result
        .errors
        .iter()
        .filter(|e| e.message.contains("no macros"))
        .count();
    assert_eq!(
        result.errors.len(),
        reports,
        "only the macros are reported: {:?}",
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
fn print_macros_with_format_arguments_are_rewritten_to_an_f_string() {
    let src = "fn main() {\n\
               let n = 3;\n\
               println!(\"{} and {}\", n, n + 1);\n\
               print!(\"{:03}\\n\", n,);\n\
               eprintln!(\"n={}\", n);\n\
               println!(\"{n} then {n:02}\");\n\
               println!(\"{{{}}}\", n);\n\
               println!(\"plain\");\n\
               if n > 5 {\n\
               panic!(\"bad {}\", n);\n\
               }\n\
               let s = format!(\"{n}!\");\n\
               println(s);\n\
               }\n";
    let (out, reports) = repaired(src);
    assert_eq!(reports, 8);
    for want in [
        "println(f\"{n} and {n + 1}\");",
        "print(f\"{n:03}\\n\");",
        "eprintln(f\"n={n}\");",
        "println(f\"{n} then {n:02}\");",
        "println(f\"{{{n}}}\");",
        "println(\"plain\");",
        "panic(f\"bad {n}\");",
        "let s = f\"{n}!\";",
    ] {
        assert!(out.contains(want), "missing {want:?} in:\n{out}");
    }
    let reparsed = karac::parse(&out);
    assert!(reparsed.errors.is_empty(), "{:?}", reparsed.errors);
    let run = karac::mir::lower::run_source(&out).unwrap_or_else(|e| panic!("build: {e}"));
    assert_eq!(run.output, "3 and 4\n003\n3 then 03\n{3}\nplain\n3!\n");
}

#[test]
fn print_macro_without_a_one_to_one_rewrite_has_no_fix() {
    for call in [
        // A hole that names its argument by position, or by name.
        "println!(\"{0} {0}\", n)",
        "println!(\"{x}\", x = n)",
        // The counts disagree, either way.
        "println!(\"{} {}\", n)",
        "println!(\"{}\", n, n)",
        // A spec an f-string does not take (Debug is deferred).
        "println!(\"{:?}\", n)",
        // A hole holding an expression rather than a name.
        "println!(\"{n + 1}\")",
    ] {
        let src = format!("fn main() {{\nlet n = 1;\n{call};\n}}\n");
        let result = karac::parse(&src);
        assert!(
            result
                .errors
                .iter()
                .any(|e| e.message.contains("into an f-string")),
            "{call}: the message names the f-string: {:?}",
            result.errors
        );
        assert!(
            result.fix_edits.is_empty() && result.fix_diffs.is_empty(),
            "{call}: no edit, and above all not the `!` deleted: {:?}",
            result.fix_edits
        );
    }
}
