//! The names the compiler keeps in scope without an import are the names
//! docs/library/README.md § The prelude lists (the review's prelude ruling,
//! 2026-11-26). Every other library type a program names is imported from its
//! `std.*` module, internal, or the post-v1 `kara-data` package's.

use karac::library_imports::{IMPORT_REQUIRED_MODULES, INTERNAL_LIBRARY_NAMES};
use karac::prelude::{PRELUDE_PRIMITIVES, PRELUDE_TYPES, REMOVED_LIBRARY_TYPES};
use std::collections::BTreeSet;

/// README's prelude section, up to the module table.
fn prelude_section() -> String {
    let readme = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/docs/library/README.md"
    ))
    .expect("docs/library/README.md");
    let start = readme.find("## The prelude").expect("§ The prelude");
    let end = start
        + readme[start..]
            .find("Everything else is imported")
            .expect("the module table's lead-in");
    readme[start..end].to_string()
}

/// The capitalised names in backticks on the bullets that list types.
fn documented_types(section: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for line in section.lines() {
        let typed = ["- **Types:**", "- **Error types:**", "- **Other types"]
            .iter()
            .any(|p| line.starts_with(p));
        if !typed {
            continue;
        }
        for piece in line.split('`').skip(1).step_by(2) {
            if piece.chars().next().is_some_and(|c| c.is_ascii_uppercase())
                && piece.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                out.insert(piece.to_string());
            }
        }
    }
    out
}

/// `PRELUDE_TYPES` less what a program cannot name unimported.
fn effective_prelude() -> BTreeSet<String> {
    PRELUDE_TYPES
        .iter()
        .filter(|n| {
            !REMOVED_LIBRARY_TYPES.contains(n)
                && !INTERNAL_LIBRARY_NAMES.iter().any(|(i, _)| i == *n)
                && !IMPORT_REQUIRED_MODULES
                    .iter()
                    .any(|(_, names)| names.contains(n))
        })
        .map(|n| n.to_string())
        .collect()
}

#[test]
fn every_name_in_scope_unimported_is_a_documented_prelude_name() {
    let section = prelude_section();
    let documented: BTreeSet<String> = section
        .split('`')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect();
    let undocumented: Vec<String> = effective_prelude()
        .into_iter()
        .filter(|n| !documented.contains(n))
        .collect();
    assert!(
        undocumented.is_empty(),
        "in scope without an import, but not in docs/library/README.md § The prelude: \
         {undocumented:?}. List it there, or make it import-required \
         (src/library_imports.rs) or internal"
    );
}

#[test]
fn every_documented_prelude_type_is_in_scope_unimported() {
    let effective = effective_prelude();
    // `String` is a primitive keyword; `ParseError` is registered with the
    // numeric parse methods rather than in `PRELUDE_TYPES`.
    let elsewhere = ["ParseError"];
    let missing: Vec<String> = documented_types(&prelude_section())
        .into_iter()
        .filter(|n| {
            !effective.contains(n)
                && !PRELUDE_PRIMITIVES.contains(&n.as_str())
                && !elsewhere.contains(&n.as_str())
        })
        .collect();
    assert!(
        missing.is_empty(),
        "docs/library/README.md § The prelude lists these, but a program must import \
         them: {missing:?}"
    );
}

#[test]
fn readme_lists_a_dozen_prelude_types_at_least() {
    // The parse above found the bullets: a reworded heading would leave both
    // tests above with nothing to compare.
    assert!(documented_types(&prelude_section()).len() >= 12);
}
