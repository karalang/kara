//! `#[test(requires: [..])]` is the one spelling (design.md §Attributes:
//! "There is no `#[name = value]` form and no `key = value` argument").
//! `karac test --help` says so, and `requires = [..]` is refused with a fix
//! to the colon form rather than accepted.

use std::process::Command;

#[test]
fn test_help_spells_requires_with_a_colon() {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["test", "--help"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("#[test(requires: [...])]"), "{stdout}");
    assert!(!stdout.contains("requires ="), "{stdout}");
}

#[test]
fn test_attribute_requires_equals_is_refused_with_a_colon_fix() {
    let src = "fn main() {}\n\n#[test(requires = [db.UserDB, payment.PaymentAPI])]\ntest \"t\" { assert(true); }\n";
    let r = karac::parse(src);
    assert_eq!(r.errors.len(), 1, "{:?}", r.errors);
    let key = karac::resolver::SpanKey::from_span(&r.errors[0].span);
    let mut edits: Vec<(usize, usize, String)> = match r.fix_edits.get(&key) {
        Some(ed) => vec![(ed.offset, ed.length, ed.replacement.clone())],
        None => r
            .fix_diffs
            .get(&key)
            .expect("the `=` form carries a fix")
            .iter()
            .map(|ed| (ed.offset, ed.length, ed.replacement.clone()))
            .collect(),
    };
    edits.sort();
    let mut out = src.to_string();
    for (off, len, rep) in edits.into_iter().rev() {
        out.replace_range(off..off + len, &rep);
    }
    assert!(
        out.contains("#[test(requires: [db.UserDB, payment.PaymentAPI])]"),
        "{out}"
    );
    assert!(karac::parse(&out).errors.is_empty());
}
