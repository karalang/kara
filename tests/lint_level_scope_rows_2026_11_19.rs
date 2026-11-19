//! Lint-level and attribute rows from the design.md §17 probe of 2026-11-19:
//! - B-2026-11-19-49: `#[allow(deprecated)]` on a struct, a field, an enum,
//!   a variant or a type alias covers the types it names.
//! - B-2026-11-19-50: two attributes of one scope naming one lint are
//!   `E_DUPLICATE_LINT_LEVEL`, as one attribute naming it twice is.
//! - B-2026-11-19-51: `malformed_diagnostic_attribute` fires on `karac check`,
//!   and an allow on the bearing item or the file suppresses it.
//! - B-2026-11-19-52: `#![...]` before the first item is the module-level
//!   position: its lint levels apply to every item of the file, and only the
//!   lint levels and `rc_budget` may stand there.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// `karac check`'s diagnostic headers for `src`, without file and position,
/// and whether it passed.
fn check(src: &str) -> (bool, Vec<String>) {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("karac-lintscope-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.kara"), src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", "main.kara"])
        .current_dir(&dir)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let err = String::from_utf8_lossy(&out.stderr);
    let heads = err
        .lines()
        .filter(|l| l.starts_with("warning[") || l.starts_with("error["))
        .map(|l| {
            // `warning[lint]: main.kara:L:C: message` -> `warning[lint] L:C message`
            let (head, rest) = l.split_once(": main.kara:").unwrap_or((l, ""));
            let mut parts = rest.splitn(3, ':');
            let (line, col, msg) = (
                parts.next().unwrap_or(""),
                parts.next().unwrap_or(""),
                parts.next().unwrap_or("").trim(),
            );
            format!("{head} {line}:{col} {msg}")
        })
        .collect();
    (out.status.success(), heads)
}

fn heads_with(heads: &[String], needle: &str) -> Vec<String> {
    heads
        .iter()
        .filter(|h| h.contains(needle))
        .cloned()
        .collect()
}

const OLD: &str = "#[deprecated]\nstruct Old {\n    v: i64,\n}\n";

#[test]
fn allow_deprecated_covers_the_types_a_type_item_names() {
    let allowed = format!(
        "{OLD}#[allow(deprecated)]\nstruct Holder {{\n    o: Old,\n}}\nstruct Field {{\n    \
         #[allow(deprecated)]\n    o: Old,\n    #[allow(deprecated)]\n    p: Option[Old],\n}}\n\
         #[allow(deprecated)]\nenum E {{\n    A(Old),\n    B,\n}}\nenum V {{\n    \
         #[allow(deprecated)]\n    A(Old),\n    B,\n}}\n#[allow(deprecated)]\ntype O = Old;\n\
         fn main() {{\n    println(1);\n}}\n"
    );
    let (ok, heads) = check(&allowed);
    assert!(ok, "{heads:#?}");
    assert_eq!(heads_with(&heads, "deprecated"), Vec::<String>::new());
    // Without the allows, each names `Old` once.
    let bare = format!(
        "{OLD}struct Holder {{\n    o: Old,\n}}\nenum E {{\n    A(Old),\n    B,\n}}\n\
         type O = Old;\nfn main() {{\n    println(1);\n}}\n"
    );
    let (ok, heads) = check(&bare);
    assert!(ok, "{heads:#?}");
    let mut lines: Vec<String> = heads_with(&heads, "warning[deprecated]")
        .iter()
        .map(|h| h.split_whitespace().nth(1).unwrap_or("").to_string())
        .collect();
    lines.sort();
    assert_eq!(lines, vec!["12:10", "6:8", "9:7"], "{heads:#?}");
}

#[test]
fn one_lint_named_by_two_attributes_of_a_scope_is_a_duplicate() {
    let (ok, heads) = check(
        "#[deprecated]\nfn old_api() -> i64 {\n    1\n}\n#[allow(deprecated)]\n\
         #[deny(deprecated)]\nfn main() {\n    println(old_api());\n}\n",
    );
    assert!(!ok);
    assert_eq!(
        heads_with(&heads, "E_DUPLICATE_LINT_LEVEL").len(),
        1,
        "{heads:#?}"
    );
    // Two lints, one each, are fine.
    let (ok, heads) = check(
        "#[deprecated]\nfn old_api() -> i64 {\n    1\n}\n#[allow(deprecated)]\n\
         #[allow(unused_mut)]\nfn main() {\n    let mut x = old_api();\n    println(x);\n}\n",
    );
    assert!(ok, "{heads:#?}");
    assert!(heads.is_empty(), "{heads:#?}");
}

#[test]
fn malformed_diagnostic_attributes_warn_on_check() {
    let src = "#[diagnostic::on_unimplemented(message: \"x\")]\nfn helper() {}\n\
               #[diagnostic::on_unimplemented(headline: \"x\", message: \"bad {Nope}\")]\n\
               trait T {\n    fn t(self) -> i64;\n}\nfn main() {\n    helper();\n}\n";
    let (ok, heads) = check(src);
    assert!(ok, "{heads:#?}");
    let lint = heads_with(&heads, "warning[malformed_diagnostic_attribute]");
    assert_eq!(lint.len(), 3, "{heads:#?}");
    assert!(
        lint[0].contains("only valid on `trait` declarations"),
        "{lint:#?}"
    );
    // One header, not the message's own prefix again.
    assert!(
        lint.iter().all(|h| h.matches("warning[").count() == 1),
        "{lint:#?}"
    );
    // An allow on the bearing item, or on the file, suppresses it.
    let (_, heads) = check(
        "#[allow(malformed_diagnostic_attribute)]\n\
         #[diagnostic::on_unimplemented(message: \"x\")]\nfn main() {\n    println(1);\n}\n",
    );
    assert!(heads.is_empty(), "{heads:#?}");
    let (_, heads) = check(
        "#![allow(malformed_diagnostic_attribute)]\n\
         #[diagnostic::on_unimplemented(message: \"x\")]\nfn main() {\n    println(1);\n}\n",
    );
    assert!(heads.is_empty(), "{heads:#?}");
}

#[test]
fn an_inner_attribute_is_the_module_level_lint_position() {
    // The allow suppresses the lint in every item of the file.
    let (ok, heads) = check(
        "#![allow(unused_mut)]\nfn a() {\n    let mut y = 2;\n    println(y);\n}\nfn main() {\n    \
         let mut x = 3;\n    println(x);\n    a();\n}\n",
    );
    assert!(ok, "{heads:#?}");
    assert!(heads.is_empty(), "{heads:#?}");
    // A deny raises it; an item's own level overrides the file's.
    let dep = "#[deprecated]\nfn old() -> i64 {\n    1\n}\n";
    let (ok, heads) = check(&format!(
        "#![deny(deprecated)]\n{dep}fn main() {{\n    println(old());\n}}\n"
    ));
    assert!(!ok);
    assert_eq!(
        heads_with(&heads, "error[deprecated]").len(),
        1,
        "{heads:#?}"
    );
    let (ok, heads) = check(&format!(
        "#![deny(deprecated)]\n{dep}#[allow(deprecated)]\nfn main() {{\n    println(old());\n}}\n"
    ));
    assert!(ok && heads.is_empty(), "{heads:#?}");
    // An unfulfilled file-level expectation is reported once, at itself.
    let (ok, heads) =
        check("#![expect(unused_mut)]\nfn a() {\n    println(1);\n}\nfn main() {\n    a();\n}\n");
    assert!(ok, "{heads:#?}");
    assert_eq!(heads, vec!["warning[unfulfilled_lint_expectation] 1:11 the lint `unused_mut` did not fire anywhere in the scope of this `#[expect(unused_mut)]` — either the lint has been fixed (remove the attribute) or the attribute was attached to the wrong scope".to_string()]);
}

#[test]
fn an_inner_attribute_is_checked_against_the_registry() {
    let (ok, heads) = check("#![frobnicate]\nfn main() {\n    println(1);\n}\n");
    assert!(!ok);
    assert_eq!(
        heads_with(&heads, "E_UNKNOWN_ATTRIBUTE").len(),
        1,
        "{heads:#?}"
    );
    let (ok, heads) = check("#![derive(Debug)]\nfn main() {\n    println(1);\n}\n");
    assert!(!ok);
    let named = heads_with(&heads, "is not a module-level attribute");
    assert_eq!(named.len(), 1, "{heads:#?}");
    assert!(
        named[0].contains("on the struct or enum it applies to"),
        "{named:#?}"
    );
    let (ok, heads) = check("#![rc_budget(max: 5)]\nfn main() {\n    println(1);\n}\n");
    assert!(ok, "{heads:#?}");
    let (ok, heads) =
        check("#![allow(unused_mut)]\n#![deny(unused_mut)]\nfn main() {\n    println(1);\n}\n");
    assert!(!ok);
    assert_eq!(
        heads_with(&heads, "E_DUPLICATE_LINT_LEVEL").len(),
        1,
        "{heads:#?}"
    );
}
