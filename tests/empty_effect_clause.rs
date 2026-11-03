//! The empty effect clause `with ()` (design.md § How a call through a
//! function value gets its effects): written on a function type in a field,
//! a collection element, a parameter or a return type, it declares a value
//! with no effects beyond the default-permitted ones. On a `fn` declaration
//! it is a parse error whose fix-it removes it. The checker and backends are
//! pinned by corpus/core/effect_empty_clause_par_apart and
//! err_effect_empty_clause_exceeded.

fn formatted(src: &str) -> String {
    karac::format_source(src).expect("formats")
}

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
fn empty_clause_parses_on_function_types_in_every_position() {
    let src = "struct Bus { handlers: Vec[Fn(i64) -> i64 with ()], one: Fn() with () }\n\
               fn apply(f: Fn(i64) -> i64 with (), x: i64) -> i64 { f(x) }\n\
               fn make() -> Fn(i64) -> i64 with () { |x: i64| x + 1 }\n\
               fn main() { let fs: Vec[Fn(i64) -> i64 with ()] = Vec.new(); }\n";
    let result = karac::parse(src);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
}

#[test]
fn fmt_round_trips_the_empty_clause() {
    let src = "struct Bus { handlers: Vec[Fn(i64) -> i64 with ()] }\n\
               fn apply(f: Fn(i64) -> i64 with (), x: i64) -> i64 { f(x) }\n\
               fn main() {}\n";
    let out = formatted(src);
    for want in [
        "handlers: Vec[Fn(i64) -> i64 with ()],",
        "fn apply(f: Fn(i64) -> i64 with (), x: i64) -> i64 {",
    ] {
        assert!(out.contains(want), "missing {want:?} in:\n{out}");
    }
    assert_eq!(formatted(&out), out, "formatting is idempotent");
}

#[test]
fn empty_clause_after_a_returned_function_type_is_the_type_s() {
    // `with ()` is never a declaration's clause, so in a return tail it
    // belongs to the returned type; the formatter parenthesizes it.
    let src = "fn make(k: i64) -> Fn(i64) -> i64 with () { |x: i64| x + k }\n\
               fn main() { println(make(1)(2)); }\n";
    let result = karac::parse(src);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    let out = formatted(src);
    assert!(
        out.contains("fn make(k: i64) -> (Fn(i64) -> i64 with ()) {"),
        "{out}"
    );
    assert_eq!(formatted(&out), out, "formatting is idempotent");
}

#[test]
fn empty_clause_on_a_fn_declaration_is_refused_with_a_removing_fix() {
    let src = "fn f(x: i64) -> i64 with () {\n    x + 1\n}\nfn main() { println(f(1)); }\n";
    let result = karac::parse(src);
    assert_eq!(result.errors.len(), 1, "{:?}", result.errors);
    let msg = format!("{:?}", result.errors[0]);
    assert!(
        msg.contains("`with ()` is written only on a function type"),
        "{msg}"
    );
    let out = fixed(src);
    assert!(out.starts_with("fn f(x: i64) -> i64 {\n"), "{out}");
    assert!(karac::parse(&out).errors.is_empty(), "{out}");
}
