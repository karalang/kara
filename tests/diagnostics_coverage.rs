//! B-2026-11-16-303's ratchet: every code `karac explain` catalogues has a
//! program that produces it, or a recorded reason why none can.
//!
//! A code passes when any of these holds:
//! - a pin: `corpus/diagnostics/<code>/` (which `tests/diagnostics_corpus.rs`
//!   checks still produces it), or any corpus entry whose `meta.toml` says
//!   `expect = "error:<code>"` (which the corpus runner checks);
//! - a code named in a provoking test of `tests/diagnostics_corpus_tooling.rs`
//!   or in the package cases of `tests/diagnostics_corpus.rs`;
//! - `karac explain` marks it retired, or deferred / later-track;
//! - an entry in [`NO_PIN`] below, with its reason.
//!
//! A new code with none of these fails here, so add a pin with it. An entry in
//! [`NO_PIN`] that has since gained a pin, or names no catalogued code, fails
//! too, so the list only shrinks.

use std::collections::BTreeSet;
use std::path::Path;

/// Codes no test can produce, each with the reason, or with the test outside
/// the files above that does produce it.
const NO_PIN: &[(&str, &str)] = &[
    (
        "W0246",
        "fires only on a std library `pub enum` without #[non_exhaustive]; a user enum is never checked",
    ),
    (
        "E_MISSING_NON_EXHAUSTIVE",
        "the deny form of W0246, std library only; tests/typechecker.rs non_exhaustive_slice6_stdlib_error_enum_without_attr_fires produces it",
    ),
    (
        "E_STDLIB_VARIANCE_IMPLICIT",
        "checks the std library's own parametric types, never a user type",
    ),
    (
        "E_INSTALL_BUILD_SPAWN_FAILED",
        "needs the current executable to be unspawnable mid-install",
    ),
    (
        "E_INSTALL_EXE_UNRESOLVABLE",
        "needs std::env::current_exe() to fail",
    ),
    (
        "E_INSTALL_COPY_FAILED",
        "needs a built executable, so an llvm build, and a copy that fails after the bin dir was found writable",
    ),
    (
        "E_INSTALL_NO_EXECUTABLE",
        "reachable only in a karac built without llvm, where the build produces no executable, and the tooling test runs on both legs",
    ),
    (
        "E_VENDOR_COPY_FAILED",
        "needs a copy into the vendor tree to fail after the tree was created",
    ),
    (
        "E_PROXY_DISABLED",
        "the disabled proxy client is never called: --no-proxy refuses a registry dependency first as E_REGISTRY_DEP_UNSUPPORTED",
    ),
    (
        "E_PROXY_NOT_IMPLEMENTED",
        "the proxy client's wasm32 stub; native karac always has a client",
    ),
    (
        "E_LOCKFILE_IO",
        "a kara.lock that exists but cannot be read; a directory or invalid UTF-8 there is skipped, not reported",
    ),
    (
        "E_CACHE_MALFORMED_METADATA",
        "build_cache::lookup has no caller in 0.8, so no build reads cache metadata",
    ),
    (
        "E_CACHE_WRITE_FAILED",
        "build_cache::record has no caller in 0.8, so no build writes the cache",
    ),
    (
        "E_DEPENDENCY_UNSATISFIABLE",
        "PubGrub's derivation-report fallback: the solve fails only when the walk's highest-version \
         picks conflict, and first_version_conflict names that pair first as \
         E_DEPENDENCY_VERSION_CONFLICT (lockfile pins are preferences, not constraints); \
         src/dep_resolver.rs unit tests render it",
    ),
];

fn meta_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            meta_files(&path, out);
        } else if path.file_name().is_some_and(|n| n == "meta.toml") {
            out.push(path);
        }
    }
}

fn is_code(s: &str) -> bool {
    s.len() > 1
        && s.chars().next().is_some_and(|c| c.is_ascii_uppercase())
        && s.chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// A diagnostic code's shape: `E0204`, `W0300`, `L0001`, `N0503`, or
/// `E_` / `W_` and a symbolic name. Rules out environment variables.
fn is_diag_code(s: &str) -> bool {
    let mut chars = s.chars();
    let lead = chars.next();
    let rest: String = chars.collect();
    matches!(lead, Some('E' | 'W' | 'L' | 'N'))
        && ((rest.len() == 4 && rest.chars().all(|c| c.is_ascii_digit()))
            || (rest.starts_with('_') && is_code(&rest[1..])))
}

/// Every diagnostic-code string literal in `text`.
fn code_literals(text: &str, out: &mut BTreeSet<String>) {
    for (i, part) in text.split('"').enumerate() {
        if i % 2 == 1 && is_diag_code(part) {
            out.insert(part.to_string());
        }
    }
}

fn pinned() -> BTreeSet<String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut pins = BTreeSet::new();
    for entry in std::fs::read_dir(root.join("corpus/diagnostics")).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            pins.insert(path.file_name().unwrap().to_str().unwrap().to_uppercase());
        }
    }
    let mut metas = Vec::new();
    meta_files(&root.join("corpus"), &mut metas);
    assert!(metas.len() > 1000, "only {} corpus entries", metas.len());
    for meta in metas {
        let text = std::fs::read_to_string(&meta).unwrap();
        for line in text.lines() {
            if let Some(code) = line
                .strip_prefix("expect = \"error:")
                .and_then(|r| r.strip_suffix('"'))
            {
                if is_code(code) {
                    pins.insert(code.to_string());
                }
            }
        }
    }
    // Every code literal in a test that provokes codes: the tooling tests
    // (all but the one that only checks descriptions) and the package cases.
    let tooling =
        std::fs::read_to_string(root.join("tests/diagnostics_corpus_tooling.rs")).unwrap();
    for test in tooling.split("#[test]").skip(1) {
        if !test.contains("fn diagnostics_corpus_tooling_unprovoked_codes_are_explained") {
            code_literals(test, &mut pins);
        }
    }
    let corpus_test = std::fs::read_to_string(root.join("tests/diagnostics_corpus.rs")).unwrap();
    let package = corpus_test
        .split("#[test]")
        .find(|t| t.contains("fn diagnostics_corpus_package_codes_are_produced_and_explained"))
        .expect("the package-cases test");
    code_literals(package, &mut pins);
    pins
}

#[test]
fn diagnostics_coverage_every_catalogued_code_is_pinned_or_exempt() {
    let codes = karac::cli::explain::catalogued_code_dispositions();
    assert!(codes.len() > 500, "only {} catalogued codes", codes.len());
    let pins = pinned();
    let allowed: BTreeSet<&str> = NO_PIN.iter().map(|(c, _)| *c).collect();
    assert_eq!(
        allowed.len(),
        NO_PIN.len(),
        "a code is listed twice in NO_PIN"
    );
    let catalogued: BTreeSet<&str> = codes.iter().map(|(c, _)| *c).collect();
    let mut bad = Vec::new();
    for (code, exempt) in &codes {
        let has_pin = pins.contains(*code);
        if allowed.contains(code) {
            if has_pin || exempt.is_some() {
                bad.push(format!(
                    "{code}: in NO_PIN but {}; drop the entry",
                    exempt.unwrap_or("pinned")
                ));
            }
        } else if !has_pin && exempt.is_none() {
            bad.push(format!(
                "{code}: no program produces it. Add corpus/diagnostics/{}/, a \
                 tooling-test case, or a NO_PIN entry with the reason",
                code.to_lowercase()
            ));
        }
    }
    for code in &allowed {
        if !catalogued.contains(code) {
            bad.push(format!("{code}: in NO_PIN but not catalogued"));
        }
    }
    assert!(bad.is_empty(), "{} codes:\n{}", bad.len(), bad.join("\n"));
}
