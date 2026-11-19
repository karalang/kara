//! `Some(None)` is an `Option[Option[_]]` whose inner variable a sibling
//! of a join solves, whichever comes first (design.md § Least upper bound
//! rule 2; B-2026-11-19-65). It read as an `Option[T]` before, one level
//! short, which only a later sibling's order hid.

use karac::{check_source, run_playground};

fn runs(src: &str, want: &str) {
    let errs: Vec<String> = check_source(src)
        .into_iter()
        .filter(|d| d.severity == "error")
        .map(|d| d.message)
        .collect();
    assert!(errs.is_empty(), "{src}\n{errs:#?}");
    let r = run_playground(src);
    assert!(r.ok, "{src}\n{:#?}", r.diagnostics);
    assert_eq!(r.stdout.concat(), want, "{src}");
}

#[test]
fn list_of_nested_options_in_either_order() {
    runs(
        "fn main() { let x = [Some(None), Some(Some(4))]; println(x); }\n",
        "[Some(None), Some(Some(4))]\n",
    );
    runs(
        "fn main() { let x = [Some(Some(4)), Some(None)]; println(x); }\n",
        "[Some(Some(4)), Some(None)]\n",
    );
    runs(
        "fn main() { let x = [(Some(None), 0), (Some(Some(4)), 1)]; println(x.len()); }\n",
        "2\n",
    );
    runs(
        "fn main() { let x = [Ok(None), Ok(Some(4)), Err(1)]; println(x.len()); }\n",
        "3\n",
    );
}

#[test]
fn branches_of_nested_options_in_either_order() {
    runs(
        "fn main() { let c = true; let x = if c { Some(None) } else { Some(Some(4)) }; println(x); }\n",
        "Some(None)\n",
    );
    runs(
        "fn main() { let k = 1; let x = match k { 0 => Some(None), _ => Some(Some(4)) }; println(x); }\n",
        "Some(Some(4))\n",
    );
}

#[test]
fn some_of_none_keeps_its_level() {
    let errs: Vec<String> = check_source("fn main() { let z: bool = Some(None); }\n")
        .into_iter()
        .filter(|d| d.severity == "error")
        .map(|d| d.message)
        .collect();
    assert!(
        errs.iter().any(|m| m.contains("Option[Option[")),
        "{errs:#?}"
    );
}
