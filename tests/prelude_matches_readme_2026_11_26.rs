//! The names the compiler keeps in scope without an import are the names
//! docs/library/README.md § The prelude lists (the review's prelude ruling,
//! 2026-11-26). Every other library type a program names is imported from its
//! `std.*` module, internal, or the post-v1 `kara-data` package's.

use karac::library_imports::{
    DEFERRED_LIBRARY_NAMES, IMPORT_REQUIRED_MODULES, INTERNAL_LIBRARY_NAMES,
};
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
                && !DEFERRED_LIBRARY_NAMES.iter().any(|(d, _)| d == *n)
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

fn karac(dir: &std::path::Path, args: &[&str]) -> (bool, String) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// The checks that call an import redundant read the same prelude: an
/// import of a name README keeps in a module stays, and `karac fix` keeps
/// it; a typo'd module is fixed to the name's module, not deleted.
#[test]
fn an_import_the_prelude_no_longer_holds_is_kept_by_fix() {
    let dir = std::env::temp_dir().join(format!("karac-prelude-keep-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let kept = "import std.sync.{Semaphore};\n\nfn main() {\n    let _s = Semaphore.new(2);\n    println(\"ok\");\n}\n";
    std::fs::write(dir.join("kept.kara"), kept).unwrap();
    let (ok, out) = karac(&dir, &["check", "kept.kara"]);
    assert!(ok, "the import checks clean:\n{out}");
    karac(&dir, &["fix", "kept.kara"]);
    assert_eq!(
        std::fs::read_to_string(dir.join("kept.kara")).unwrap(),
        kept
    );

    let typo = "import std.sycn.{Semaphore};\n\nfn main() {\n    let _s = Semaphore.new(2);\n    println(\"ok\");\n}\n";
    std::fs::write(dir.join("typo.kara"), typo).unwrap();
    let (ok, out) = karac(&dir, &["check", "typo.kara"]);
    assert!(
        !ok && out.contains("`Semaphore` is `std.sync`'s") && !out.contains("in the prelude"),
        "{out}"
    );
    karac(&dir, &["fix", "typo.kara"]);
    assert_eq!(
        std::fs::read_to_string(dir.join("typo.kara")).unwrap(),
        kept.replace("{Semaphore}", "Semaphore")
    );
    let (ok, out) = karac(&dir, &["check", "typo.kara"]);
    assert!(ok, "the fixed program checks:\n{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A deferred name (checks, runs on no backend) is refused whether it is
/// imported or not, naming the section that schedules it, and `karac fix`
/// does not delete the import as if the name were the prelude's.
#[test]
fn a_deferred_library_name_is_refused_imported_or_not() {
    let dir = std::env::temp_dir().join(format!("karac-prelude-deferred-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (tag, src) in [
        (
            "imported",
            "import std.embedded.{VolatileCell};\n\nfn main() {\n    let r: VolatileCell[i32] = VolatileCell.new(7);\n    println(r.read());\n}\n",
        ),
        (
            "bare",
            "fn main() {\n    let r: VolatileCell[i32] = VolatileCell.new(7);\n    println(r.read());\n}\n",
        ),
    ] {
        let file = format!("{tag}.kara");
        std::fs::write(dir.join(&file), src).unwrap();
        let (ok, out) = karac(&dir, &["check", &file]);
        assert!(
            !ok && out.contains("`VolatileCell` is deferred")
                && out.contains("docs/deferred.md § Volatile access")
                && !out.contains("in the prelude"),
            "{tag}:\n{out}"
        );
        karac(&dir, &["fix", &file]);
        assert_eq!(std::fs::read_to_string(dir.join(&file)).unwrap(), src, "{tag}");
    }
    // `CriticalSectionGuard` runs on both backends and stays in `std.embedded`.
    let guard = "import std.embedded.{CriticalSectionGuard};\n\nfn work() with writes(Hardware) writes(Stdout) {\n    let _g: CriticalSectionGuard = critical_section.acquire();\n    println(1);\n}\n\nfn main() {\n    work();\n}\n";
    std::fs::write(dir.join("guard.kara"), guard).unwrap();
    let (ok, out) = karac(&dir, &["check", "guard.kara"]);
    assert!(ok, "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}
