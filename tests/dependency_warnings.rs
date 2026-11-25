//! A dependency's warnings in a dependent package's commands.
//!
//! B-2026-11-24-52: design.md § `#[deprecated]` names the defining package
//! of a `pub` item, so the migration target is clear across re-exports. The
//! warning a dependent gets names the item by its path in that package and
//! the package: `lib.load_config` from package `lib`.
//!
//! B-2026-11-24-54: a dependency's own warnings (a `must_use` result it
//! discards, its own use of its deprecated item) are its author's, shown
//! when that package is checked. A dependent's `check` and `run` show only
//! warnings about its own code, as Cargo caps a dependency's lints. Its
//! errors still stop the dependent's command.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static CALL: AtomicUsize = AtomicUsize::new(0);

const LIB: &str = "#[deprecated(since: \"1.2.0\", note: \"use `parse_config` instead\")]\n\
pub fn load_config(p: Str) -> i64 { 2 }\n\
pub fn parse_config(p: Str) -> i64 { 3 }\n\
#[must_use]\npub fn mu() -> i64 { 1 }\n\
pub fn calls_mu() -> i64 { mu(); 2 }\n\
pub fn calls_old() -> i64 { load_config(\"z\") }\n";

const UTIL: &str = "#[deprecated(note: \"gone\")]\npub fn old2() -> i64 { 5 }\n";

/// `lib` (with a `util` module) and `app`, which depends on it, under a
/// fresh directory; `lib_src` replaces lib.kara.
fn packages(lib_src: &str, main: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-dependency-warnings-{}-{}",
        std::process::id(),
        CALL.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let manifest = |name: &str, deps: &str| {
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n\n[dependencies]\n{deps}")
    };
    std::fs::create_dir_all(dir.join("lib/src")).unwrap();
    std::fs::create_dir_all(dir.join("app/src")).unwrap();
    std::fs::write(dir.join("lib/kara.toml"), manifest("lib", "")).unwrap();
    std::fs::write(dir.join("lib/src/lib.kara"), lib_src).unwrap();
    std::fs::write(dir.join("lib/src/util.kara"), UTIL).unwrap();
    std::fs::write(
        dir.join("app/kara.toml"),
        manifest("app", "lib = { path = \"../lib\" }\n"),
    )
    .unwrap();
    std::fs::write(dir.join("app/src/main.kara"), main).unwrap();
    dir
}

fn karac(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
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

const USES_DEPRECATED: &str = "import lib.{load_config, parse_config};\n\
fn main() { println(load_config(\"x\") + parse_config(\"y\")); }\n";

#[test]
fn a_dependencys_deprecated_item_is_named_by_its_package_and_path() {
    let dir = packages(LIB, USES_DEPRECATED);
    let (ok, out) = karac(&dir.join("app"), &["check"]);
    assert!(ok, "{out}");
    assert!(
        out.contains(
            "warning[deprecated]: src/main.kara:2:21: use of deprecated item `lib.load_config` \
             from package `lib`: use `parse_config` instead (since 1.2.0)"
        ),
        "{out}"
    );
}

#[test]
fn an_item_of_a_dependencys_module_is_named_by_its_module_path() {
    let dir = packages(
        LIB,
        "import lib.util.{old2};\nfn main() { println(old2()); }\n",
    );
    let (ok, out) = karac(&dir.join("app"), &["check"]);
    assert!(ok, "{out}");
    assert!(
        out.contains("use of deprecated item `lib.util.old2` from package `lib`: gone"),
        "{out}"
    );
}

#[test]
fn a_deprecated_item_of_the_same_package_names_no_package() {
    let dir = packages(LIB, "import other.{loc};\nfn main() { println(loc()); }\n");
    std::fs::write(
        dir.join("app/src/other.kara"),
        "#[deprecated]\npub fn loc() -> i64 { 1 }\n",
    )
    .unwrap();
    let (ok, out) = karac(&dir.join("app"), &["check"]);
    assert!(ok, "{out}");
    assert!(
        out.contains("use of deprecated item `loc` — suppress"),
        "{out}"
    );
    assert!(!out.contains("from package"), "{out}");
}

#[test]
fn a_dependent_check_shows_none_of_the_dependencys_own_warnings() {
    let dir = packages(LIB, USES_DEPRECATED);
    let (ok, out) = karac(&dir.join("app"), &["check"]);
    assert!(ok, "{out}");
    assert_eq!(
        out.matches("warning[").count(),
        1,
        "only app's own use of the deprecated item:\n{out}"
    );
    assert!(!out.contains("must_use"), "{out}");
    assert!(!out.contains("lib.kara"), "{out}");
}

#[test]
fn a_dependent_json_check_shows_none_of_the_dependencys_own_warnings() {
    let dir = packages(LIB, USES_DEPRECATED);
    let (ok, out) = karac(&dir.join("app"), &["check", "--output=json"]);
    assert!(ok, "{out}");
    assert_eq!(out.matches("\"severity\":\"warning\"").count(), 1, "{out}");
    assert!(out.contains("\"file\":\"src/main.kara\""), "{out}");
    assert!(!out.contains("must_use"), "{out}");
}

#[test]
fn a_dependent_run_shows_none_of_the_dependencys_own_warnings() {
    let dir = packages(LIB, USES_DEPRECATED);
    let (ok, out) = karac(&dir.join("app"), &["run", "--interp"]);
    assert!(ok, "{out}");
    assert!(out.contains("5\n"), "{out}");
    assert_eq!(out.matches("warning[").count(), 1, "{out}");
    assert!(!out.contains("must_use"), "{out}");
}

#[test]
fn the_dependency_still_sees_its_own_warnings() {
    let dir = packages(LIB, USES_DEPRECATED);
    let (ok, out) = karac(&dir.join("lib"), &["check"]);
    assert!(ok, "{out}");
    assert!(
        out.contains("warning[must_use]: src/lib.kara:6:28: discarded return value of `mu`"),
        "{out}"
    );
    assert!(
        out.contains(
            "warning[deprecated]: src/lib.kara:7:29: use of deprecated item `load_config`:"
        ),
        "its own use names no package:\n{out}"
    );
}

#[test]
fn a_dependencys_error_still_stops_the_dependent() {
    let lib = format!("{LIB}pub fn bad() -> i64 {{ \"s\" }}\n");
    let dir = packages(&lib, USES_DEPRECATED);
    let (ok, out) = karac(&dir.join("app"), &["check"]);
    assert!(!ok, "{out}");
    assert!(out.contains("error"), "{out}");
}
