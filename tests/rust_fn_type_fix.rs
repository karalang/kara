//! Rust's spellings of a function type, `impl Fn(..)`, `fn(..)` (a function
//! pointer), `FnMut(..)` and `FnOnce(..)`, are each reported with the edit
//! that writes Kāra's (`Fn(..)`, `MutFn(..)`, `OnceFn(..)`), and the type is
//! parsed as Kāra's, so nothing else is reported and one `karac fix` run
//! repairs the signature (B-2026-10-29-1).

fn repaired(src: &str) -> (String, Vec<String>) {
    let result = karac::parse(src);
    let messages: Vec<String> = result.errors.iter().map(|e| e.message.clone()).collect();
    let mut edits: Vec<_> = result.fix_edits.values().cloned().collect();
    edits.extend(result.fix_diffs.values().flatten().cloned());
    assert_eq!(
        edits.len(),
        messages.len(),
        "one edit per report: {messages:?}"
    );
    edits.sort_by_key(|e| std::cmp::Reverse(e.offset));
    let mut out = src.to_string();
    for e in edits {
        out.replace_range(e.offset..e.offset + e.length, &e.replacement);
    }
    (out, messages)
}

fn program(ty: &str) -> String {
    format!(
        "fn apply(f: {ty}, x: i64) -> i64 {{\n    f(x)\n}}\n\n\
         fn adder(n: i64) -> {ty} {{\n    |v| v + n\n}}\n\n\
         fn main() {{\n    println(apply(|v| v + 1, 4));\n    println(adder(3)(4));\n}}\n"
    )
}

#[test]
fn rust_function_type_spellings_are_rewritten_to_kara() {
    for (ty, reports, fixed) in [
        ("impl Fn(i64) -> i64", 1, "Fn(i64) -> i64"),
        ("fn(i64) -> i64", 1, "Fn(i64) -> i64"),
        ("impl fn(i64) -> i64", 2, "Fn(i64) -> i64"),
        ("impl FnOnce(i64) -> i64", 2, "OnceFn(i64) -> i64"),
        ("FnOnce(i64) -> i64", 1, "OnceFn(i64) -> i64"),
    ] {
        let src = program(ty);
        let (out, messages) = repaired(&src);
        // Reported in each of the two signatures.
        assert_eq!(messages.len(), 2 * reports, "{ty}: {messages:?}");
        assert!(
            messages.iter().all(|m| m.contains("Rust")),
            "{ty}: {messages:?}"
        );
        assert_eq!(out, program(fixed), "{ty}");
        let run = karac::mir::lower::run_source(&out).unwrap_or_else(|e| panic!("{ty}: {e}"));
        assert_eq!(run.output, "5\n7\n", "{ty}");
    }
}

#[test]
fn rust_fn_mut_is_rewritten_to_mut_fn() {
    let src = "fn apply(f: impl FnMut(i64) -> i64) -> i64 {\n    0\n}\n";
    let (out, messages) = repaired(src);
    assert_eq!(messages.len(), 2, "{messages:?}");
    assert!(
        messages[0].starts_with("`impl FnMut(..)` is Rust syntax"),
        "{messages:?}"
    );
    assert_eq!(out, "fn apply(f: MutFn(i64) -> i64) -> i64 {\n    0\n}\n");
}

#[test]
fn kara_names_and_non_function_uses_are_left_alone() {
    // Kāra's own spellings parse as before.
    for ty in ["Fn(i64) -> i64", "MutFn(i64) -> i64", "OnceFn(i64) -> i64"] {
        let src = format!("fn apply(f: {ty}) -> i64 {{\n    0\n}}\n");
        let result = karac::parse(&src);
        assert!(result.errors.is_empty(), "{ty}: {:?}", result.errors);
    }
    // `impl Trait` that is not a function type, and a type named `FnMut`
    // with no `(` after it, are not taken for one.
    let src = "trait Show {\n    fn show(self) -> String;\n}\n\
               struct FnMut {}\n\
               fn f(x: impl Show, y: FnMut) -> i64 {\n    0\n}\n";
    let result = karac::parse(src);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(result.fix_edits.is_empty());
}
