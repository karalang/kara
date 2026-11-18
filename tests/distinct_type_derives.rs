//! Derives on a `distinct type` (B-2026-11-18-22, B-2026-11-18-23). The base
//! type is the distinct type's one field: a derive needs the base to have the
//! trait, the derive chains of design.md §9 apply, and `#[derive(Default)]`
//! gives `Count.default()` = `Count(<base>.default())`.

use karac::{check_source, run_playground};

fn errors(src: &str) -> Vec<String> {
    check_source(src)
        .into_iter()
        .filter(|d| d.severity == "error")
        .map(|d| d.message)
        .collect()
}

#[test]
fn distinct_eq_over_a_float_base_is_refused() {
    let src = "#[derive(Eq, PartialEq)]\ndistinct type M = f64;\nfn main() { println(M(1.0) == M(1.0)); }\n";
    let errs = errors(src);
    assert!(
        errs.iter()
            .any(|m| m.contains("distinct type 'M' derives Eq but its base type 'f64' is not Eq")),
        "{errs:#?}"
    );
}

#[test]
fn distinct_hash_and_ord_over_a_float_base_are_refused() {
    let src = "#[derive(PartialEq, PartialOrd, Ord, Hash)]\ndistinct type M = f64;\nfn main() {}\n";
    let errs = errors(src);
    for t in ["Ord", "Hash"] {
        assert!(
            errs.iter()
                .any(|m| m.contains(&format!("derives {t} but its base type 'f64'"))),
            "{t}: {errs:#?}"
        );
    }
    assert!(
        !errs.iter().any(|m| m.contains("derives PartialOrd")),
        "{errs:#?}"
    );
}

#[test]
fn distinct_eq_over_an_integer_base_checks_clean() {
    let src = "#[derive(PartialEq, Eq, Hash, PartialOrd, Ord)]\ndistinct type Id = i64;\nfn main() { println(Id(1) == Id(1)); }\n";
    let out = run_playground(src);
    assert!(out.ok, "{:#?}", out.diagnostics);
    assert_eq!(out.stdout.concat(), "true\n");
}

#[test]
fn distinct_eq_without_partial_eq_gets_the_implied_derive_fix() {
    let src = "#[derive(Eq)]\ndistinct type Id = i64;\nfn main() {}\n";
    let diags = check_source(src);
    let d = diags
        .iter()
        .find(|d| d.message.contains("implied derive on `Id` is deprecated"))
        .unwrap_or_else(|| panic!("{diags:#?}"));
    assert_eq!(d.fixes.len(), 1, "{d:#?}");
    let f = &d.fixes[0];
    let mut out = src.to_string();
    out.replace_range(f.offset..f.offset + f.length, &f.replacement);
    assert!(out.starts_with("#[derive(Eq, PartialEq)]"), "{out}");
}

#[test]
fn distinct_default_is_the_base_default() {
    let src = "#[derive(Default, Display, Copy, Clone)]\ndistinct type Count = i64;\n\
               #[derive(Default, Display)]\ndistinct type Name = String;\n\
               #[derive(Default)]\nstruct Cfg { n: Count, name: Name }\n\
               fn main() {\n    let c: Count = Count.default();\n    println(f\"{c}\");\n    \
               println(f\"[{Name.default()}]\");\n    let g = Cfg.default();\n    \
               println(f\"{g.n}\");\n}\n";
    let out = run_playground(src);
    assert!(out.ok, "{:#?}", out.diagnostics);
    assert_eq!(out.stdout.concat(), "0\n[]\n0\n");
}

#[test]
fn distinct_default_over_a_non_default_base_is_refused() {
    let src =
        "struct NoDef { x: i64 }\n#[derive(Default)]\ndistinct type W = NoDef;\nfn main() {}\n";
    let errs = errors(src);
    assert!(
        errs.iter().any(|m| m
            .contains("distinct type 'W' — its base type 'NoDef' does not implement 'Default'")),
        "{errs:#?}"
    );
}

#[test]
fn distinct_default_on_a_refined_type_is_refused() {
    let src = "#[derive(Default)]\ndistinct type Pos = i64 where self > 0;\nfn main() {}\n";
    let errs = errors(src);
    assert!(
        errs.iter()
            .any(|m| m.contains("refined distinct type 'Pos'")),
        "{errs:#?}"
    );
}

#[test]
fn distinct_manual_default_impl_still_wins() {
    let src = "#[derive(Display)]\ndistinct type Count = i64;\n\
               impl Default for Count { fn default() -> Count { Count(5) } }\n\
               fn main() { let c: Count = Count.default(); println(f\"{c}\"); }\n";
    let out = run_playground(src);
    assert!(out.ok, "{:#?}", out.diagnostics);
    assert_eq!(out.stdout.concat(), "5\n");
}
