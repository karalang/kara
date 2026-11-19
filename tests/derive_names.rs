//! `#[derive(..)]` takes a closed table of built-in traits and the traits a
//! `comptime fn derive_<name>` generates (design.md §9 Derive): an unknown
//! name is E_DERIVE_UNKNOWN with the nearest built-in name offered, a derive
//! on an item that is not a struct, enum or distinct type is E_DERIVE_TARGET,
//! and a trait listed twice is E_DERIVE_DUPLICATE, fixed by removing the
//! second (B-2026-11-19-13).

use karac::{check_source, run_playground, PlaygroundDiagnostic};

fn errors(src: &str) -> Vec<PlaygroundDiagnostic> {
    check_source(src)
        .into_iter()
        .filter(|d| d.severity == "error")
        .collect()
}

fn apply(src: &str, d: &PlaygroundDiagnostic) -> String {
    let mut out = src.to_string();
    let mut fixes = d.fixes.clone();
    fixes.sort_by_key(|f| std::cmp::Reverse(f.offset));
    for f in fixes {
        out.replace_range(f.offset..f.offset + f.length, &f.replacement);
    }
    out
}

#[test]
fn an_unknown_derive_name_is_refused() {
    let src = "#[derive(Frobnicate)]\nstruct P { x: i64 }\nfn main() { println(P { x: 1 }.x); }\n";
    let es = errors(src);
    assert_eq!(es.len(), 1, "{es:#?}");
    assert_eq!(es[0].code.as_deref(), Some("E_DERIVE_UNKNOWN"), "{es:#?}");
    assert!(es[0].fixes.is_empty(), "{es:#?}");
}

#[test]
fn a_misspelt_derive_is_fixed_to_the_nearest_name() {
    let src = "#[derive(PartilEq, Eq)]\nstruct P { x: i64 }\nfn main() { println(P { x: 1 } == P { x: 1 }); }\n";
    let es = errors(src);
    let d = es
        .iter()
        .find(|d| d.code.as_deref() == Some("E_DERIVE_UNKNOWN"))
        .expect("E_DERIVE_UNKNOWN");
    assert!(d.message.contains("did you mean `PartialEq`"), "{d:#?}");
    let out = apply(src, d);
    assert!(out.starts_with("#[derive(PartialEq, Eq)]"), "{out}");
    let r = run_playground(&out);
    assert!(r.ok, "{out}\n{:#?}", r.diagnostics);
    assert_eq!(r.stdout.concat(), "true\n");
}

#[test]
fn a_derive_on_a_non_type_item_is_refused() {
    let src = "#[derive(Clone)]\nfn f() { }\n#[derive(Copy)]\nconst XY: i64 = 1;\n#[derive(Debug)]\ntrait T { }\nfn main() { f(); println(XY); }\n";
    let es = errors(src);
    let targets = es
        .iter()
        .filter(|d| d.code.as_deref() == Some("E_DERIVE_TARGET"))
        .count();
    assert_eq!(targets, 3, "{es:#?}");
}

#[test]
fn a_trait_derived_twice_is_removed_by_the_fix() {
    let src = "#[derive(PartialEq, Debug, PartialEq)]\nstruct Q { x: i64 }\nfn main() { println(Q { x: 1 } == Q { x: 2 }); }\n";
    let es = errors(src);
    assert_eq!(es.len(), 1, "{es:#?}");
    assert_eq!(es[0].code.as_deref(), Some("E_DERIVE_DUPLICATE"), "{es:#?}");
    let out = apply(src, &es[0]);
    assert!(out.starts_with("#[derive(PartialEq, Debug)]\n"), "{out}");
    let r = run_playground(&out);
    assert!(r.ok, "{out}\n{:#?}", r.diagnostics);
    assert_eq!(r.stdout.concat(), "false\n");
}

#[test]
fn a_derive_a_comptime_fn_backs_is_accepted() {
    let src = "comptime fn derive_describe(comptime T: Type) -> Vec[Item] {\n    [ast.item(\"impl \" + T.name() + \" { fn describe(self) -> String { \\\"w\\\" } }\")]\n}\n#[derive(Describe, Clone)]\nstruct Widget { id: i64 }\nfn main() { println(Widget { id: 7 }.describe()); }\n";
    let es = errors(src);
    assert!(es.is_empty(), "{es:#?}");
}
