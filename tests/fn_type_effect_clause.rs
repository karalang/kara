//! A function type's effect clause holds several effects, space-separated
//! as on a declaration (B-2026-10-25-27): in a struct field, a tuple and a
//! `type` alias. Commas between them, the common slip, get the same
//! machine-applicable "space-separated" diagnostic a declaration's clause
//! gets, where they used to end the field (`Expected Colon, found
//! LeftParen`); a comma that does end the field, tuple element or
//! parameter still does.

use karac::parse;

fn errors(src: &str) -> Vec<String> {
    parse(src).errors.into_iter().map(|e| e.message).collect()
}

/// `src` with every fix-it the parser offers applied, last first.
fn fixed(src: &str) -> String {
    let r = parse(src);
    let mut edits: Vec<_> = r.fix_edits.values().cloned().collect();
    edits.sort_by_key(|e| std::cmp::Reverse(e.offset));
    let mut out = src.to_string();
    for e in edits {
        out.replace_range(e.offset..e.offset + e.length, &e.replacement);
    }
    out
}

#[test]
fn several_effects_in_a_function_type_parse_space_separated() {
    for src in [
        "struct Job { run: Fn(i64) -> String with writes(Stdout) sends(Network) blocks, n: i64 }\n",
        "type JobFn = Fn(i64) -> String with writes(Stdout) sends(Network) blocks;\n",
        "struct Job { run: (Fn(i64) -> String with writes(Stdout) blocks, i64) }\n",
    ] {
        assert_eq!(errors(src), Vec::<String>::new(), "{src}");
    }
}

#[test]
fn commas_between_function_type_effects_carry_the_fix() {
    for (src, want) in [
        (
            "struct Job { run: Fn(i64) -> String with writes(Stdout), sends(Network), blocks, n: i64 }\n",
            "struct Job { run: Fn(i64) -> String with writes(Stdout) sends(Network) blocks, n: i64 }\n",
        ),
        (
            "type JobFn = Fn(i64) -> String with writes(Stdout), sends(Network), blocks;\n",
            "type JobFn = Fn(i64) -> String with writes(Stdout) sends(Network) blocks;\n",
        ),
        (
            "struct Job { run: (Fn(i64) -> String with writes(Stdout), sends(Network)) }\n",
            "struct Job { run: (Fn(i64) -> String with writes(Stdout) sends(Network)) }\n",
        ),
    ] {
        let got = errors(src);
        assert!(!got.is_empty(), "{src}");
        assert!(
            got.iter()
                .all(|m| m.starts_with("effect items are space-separated")),
            "{src}: {got:?}"
        );
        assert_eq!(fixed(src), want);
        assert_eq!(errors(want), Vec::<String>::new(), "{want}");
    }
}

#[test]
fn a_comma_after_a_function_type_clause_still_ends_it() {
    for src in [
        // A field named like a verb.
        "struct Job { run: Fn(i64) -> String with writes(Stdout), blocks: i64 }\n",
        // A tuple's next element.
        "struct Job { run: (Fn(i64) -> String with writes(Stdout), Fn(i64) -> i64) }\n",
        // A parameter named like a verb.
        "fn f(cb: Fn(i64) with writes(Stdout), blocks: i64) { }\n",
    ] {
        assert_eq!(errors(src), Vec::<String>::new(), "{src}");
    }
}

#[test]
fn a_stored_callback_with_several_effects_runs() {
    let src = r#"
import std.time.{sleep, Duration};

struct Job { run: Fn(i64) -> String with writes(Stdout) blocks, n: i64 }

fn work(n: i64) -> String {
    println(f"working {n}");
    sleep(Duration.ms(1));
    f"done {n}"
}

fn main() {
    let j = Job { run: work, n: 4 };
    let f = j.run;
    println(f(j.n));
}
"#;
    let r = karac::mir::lower::run_source(src).unwrap_or_else(|e| panic!("build: {e}"));
    assert_eq!(r.output, "working 4\ndone 4\n", "stderr: {}", r.stderr);
}
