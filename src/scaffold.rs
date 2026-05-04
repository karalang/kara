//! `karac init` project scaffolding (CR-36).
//!
//! Writes a fixed template into a target directory: `kara.toml`, the entry
//! file (`src/main.kara` or `src/lib.kara`), a starter `_test.kara` alongside
//! it, a title-only `README.md`, and a one-line `.gitignore`. See
//! `docs/design.md § Package System § Project Scaffolding` for the full spec
//! and `brainstorming/brainstorming_v42.md` for the resolution trail.
//!
//! Design invariants enforced here:
//!
//! - **Directory name = package name = root module name.** The caller passes
//!   an already-validated package name; the template interpolates it verbatim.
//! - **Collision rule.** The three *entry* files (`kara.toml`,
//!   `src/main.kara`, `src/lib.kara`) are collision-checked; `--force`
//!   overrides the abort for this trio. The remaining scaffolded files
//!   (`README.md`, `src/*_test.kara`, `.gitignore`) are never overwritten —
//!   if one already exists we leave it alone, even under `--force`. This
//!   matches the `git init` convention where `.gitignore` / `README.md` may
//!   already have been written by the user or another tool, and silently
//!   clobbering them is worse than skipping.
//! - **No VCS side effects.** `.gitignore` is pure text; no `git init`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The template flavor to scaffold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Template {
    /// `src/main.kara` + `src/main_test.kara` with a hello-world entry point.
    Bin,
    /// `src/lib.kara` + `src/lib_test.kara` with a starter `pub fn add`.
    Lib,
}

/// Options driving a scaffold operation.
#[derive(Debug, Clone, Copy)]
pub struct ScaffoldOpts {
    pub template: Template,
    /// When `true`, pre-existing `kara.toml` / `src/main.kara` / `src/lib.kara`
    /// are overwritten instead of aborting the scaffold. Does not affect the
    /// skip-if-exists files (`README.md`, `src/*_test.kara`, `.gitignore` —
    /// never overwritten) or the positional-form non-empty directory check
    /// (which is performed by the caller before scaffolding begins).
    pub force: bool,
}

/// Fatal errors surfaced by `karac init`. Each variant carries enough context
/// for the CLI to render a structured diagnostic; none are recovered from.
#[derive(Debug)]
pub enum ScaffoldError {
    /// Package / directory name is not `[a-z][a-z0-9_]*`. `suggestion` is
    /// populated when a mechanical rewrite (hyphens → underscores, lowercase)
    /// would produce a valid name.
    InvalidName {
        value: String,
        suggestion: Option<String>,
    },
    /// Package name collides with a Kāra reserved keyword.
    ReservedKeyword { value: String },
    /// The positional form `karac init <name>` was given an existing,
    /// non-empty directory. There is no `--force` override for this form by
    /// design — the intent "create this directory" is incompatible with
    /// scaffolding into an existing workspace.
    TargetDirNotEmpty { path: PathBuf },
    /// An entry file (`kara.toml` / `src/main.kara` / `src/lib.kara`) already
    /// exists and `--force` was not passed.
    Collision {
        path: PathBuf,
        file_kind: &'static str,
    },
    /// Generic filesystem error, with the offending path for the diagnostic.
    Io { path: PathBuf, error: String },
}

impl ScaffoldError {
    /// Short machine-readable tag for `--output=json` diagnostics. No new
    /// diagnostic codes were introduced by CR-36 — the CLI maps these tags to
    /// a plain `"scaffold"` phase.
    pub fn tag(&self) -> &'static str {
        match self {
            ScaffoldError::InvalidName { .. } => "invalid_name",
            ScaffoldError::ReservedKeyword { .. } => "reserved_keyword",
            ScaffoldError::TargetDirNotEmpty { .. } => "target_dir_not_empty",
            ScaffoldError::Collision { .. } => "collision",
            ScaffoldError::Io { .. } => "io",
        }
    }
}

impl std::fmt::Display for ScaffoldError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScaffoldError::InvalidName { value, suggestion } => {
                write!(
                    f,
                    "project name `{value}` is not a valid Kāra package name\n  note: package names must match [a-z][a-z0-9_]* (snake_case, no hyphens)"
                )?;
                if let Some(s) = suggestion {
                    write!(f, "\n  help: did you mean `{s}`?")?;
                }
                Ok(())
            }
            ScaffoldError::ReservedKeyword { value } => write!(
                f,
                "project name `{value}` is a reserved Kāra keyword\n  help: pick a different name",
            ),
            ScaffoldError::TargetDirNotEmpty { path } => write!(
                f,
                "target directory `{}` already exists and is not empty\n  help: to scaffold into an existing directory, `cd` into it and run `karac init` with no positional argument",
                path.display(),
            ),
            ScaffoldError::Collision { path, file_kind } => write!(
                f,
                "`{}` already exists ({})\n  help: pass `--force` to overwrite",
                path.display(),
                file_kind,
            ),
            ScaffoldError::Io { path, error } => write!(
                f,
                "failed to write `{}`: {}",
                path.display(),
                error,
            ),
        }
    }
}

/// Kāra reserved keywords. Source of truth is the lexer's keyword match in
/// `src/lexer.rs#scan_identifier`; we duplicate a subset here because depending
/// on the lexer from scaffold would force the project to parse just to validate
/// a directory name. Drift is mitigated by a unit test that spot-checks a few
/// keywords are actually rejected (and the canonical list is short — syntax
/// changes ship with an update to this list in the same CR).
const RESERVED_KEYWORDS: &[&str] = &[
    "fn",
    "struct",
    "enum",
    "trait",
    "impl",
    "mod",
    "use",
    "const",
    "type",
    "distinct",
    "pub",
    "private",
    "if",
    "else",
    "match",
    "while",
    "for",
    "in",
    "loop",
    "return",
    "break",
    "continue",
    "defer",
    "errdefer",
    "asm",
    "global_asm",
    "let",
    "mut",
    "own",
    "ref",
    "weak",
    "lock",
    "effect",
    "resource",
    "verb",
    "reads",
    "writes",
    "sends",
    "receives",
    "allocates",
    "panics",
    "blocks",
    "suspends",
    "with",
    "transparent",
    "stable",
    "seq",
    "par",
    "yield",
    "as",
    "where",
    "dyn",
    "requires",
    "ensures",
    "invariant",
    "unsafe",
    "extern",
    "shared",
    "layout",
    "group",
    "true",
    "false",
    "providers",
    "alias",
    "independent",
    "self",
    "Self",
];

/// Validate a package name against `[a-z][a-z0-9_]*` and the reserved-keyword
/// list. Returns `Ok(())` on success; on failure the returned
/// `InvalidName.suggestion` is populated when a mechanical rewrite would
/// produce a valid name (hyphens → underscores, lowercase).
pub fn validate_package_name(name: &str) -> Result<(), ScaffoldError> {
    if name.is_empty() {
        return Err(ScaffoldError::InvalidName {
            value: name.to_string(),
            suggestion: None,
        });
    }

    if RESERVED_KEYWORDS.contains(&name) {
        return Err(ScaffoldError::ReservedKeyword {
            value: name.to_string(),
        });
    }

    let first = name.as_bytes()[0];
    let first_ok = first.is_ascii_lowercase();
    let body_ok = name
        .bytes()
        .skip(1)
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');

    if first_ok && body_ok {
        return Ok(());
    }

    let suggestion = build_name_suggestion(name);
    Err(ScaffoldError::InvalidName {
        value: name.to_string(),
        suggestion,
    })
}

/// Derive a snake_case suggestion from an invalid name. Returns `None` if no
/// mechanical fix produces a valid identifier (e.g., name starts with a digit
/// with no letters to promote, or contains non-ASCII characters).
fn build_name_suggestion(name: &str) -> Option<String> {
    let lowered: String = name
        .chars()
        .map(|c| {
            if c == '-' {
                '_'
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect();
    // If the lowercased/hyphen-replaced form is a pure-ASCII identifier, offer
    // it. Otherwise give up — we'd rather the user pick a new name than see an
    // awkward machine-mangled suggestion.
    if lowered.is_empty() {
        return None;
    }
    let first = lowered.as_bytes()[0];
    if !first.is_ascii_lowercase() {
        return None;
    }
    let body_ok = lowered
        .bytes()
        .skip(1)
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if !body_ok {
        return None;
    }
    if RESERVED_KEYWORDS.contains(&lowered.as_str()) {
        return None;
    }
    if lowered == name {
        // Shouldn't happen (caller already rejected `name` as invalid), but
        // guard against suggesting the identical string.
        return None;
    }
    Some(lowered)
}

/// Prepare the target directory for the positional `karac init <name>` form.
/// Creates `./<name>/` if absent; returns `TargetDirNotEmpty` if it already
/// exists with any entries. An empty existing directory is accepted — same
/// rule cargo uses for `cargo init`.
pub fn prepare_new_target_dir(target: &Path) -> Result<(), ScaffoldError> {
    match fs::metadata(target) {
        Ok(meta) => {
            if !meta.is_dir() {
                return Err(ScaffoldError::TargetDirNotEmpty {
                    path: target.to_path_buf(),
                });
            }
            if !dir_is_empty(target)? {
                return Err(ScaffoldError::TargetDirNotEmpty {
                    path: target.to_path_buf(),
                });
            }
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(target).map_err(|e| ScaffoldError::Io {
                path: target.to_path_buf(),
                error: e.to_string(),
            })
        }
        Err(e) => Err(ScaffoldError::Io {
            path: target.to_path_buf(),
            error: e.to_string(),
        }),
    }
}

fn dir_is_empty(path: &Path) -> Result<bool, ScaffoldError> {
    let mut entries = fs::read_dir(path).map_err(|e| ScaffoldError::Io {
        path: path.to_path_buf(),
        error: e.to_string(),
    })?;
    Ok(entries.next().is_none())
}

/// Scaffold a project at `target_dir` with `package_name` (already validated
/// by `validate_package_name`). Writes five files per the template table in
/// design.md § Project Scaffolding.
pub fn scaffold_project(
    target_dir: &Path,
    package_name: &str,
    opts: ScaffoldOpts,
) -> Result<(), ScaffoldError> {
    let manifest_path = target_dir.join("kara.toml");
    let (entry_rel, entry_body, test_rel, test_body): (&str, String, &str, String) =
        match opts.template {
            Template::Bin => (
                "src/main.kara",
                bin_main_body(),
                "src/main_test.kara",
                bin_main_test_body(),
            ),
            Template::Lib => (
                "src/lib.kara",
                lib_body(),
                "src/lib_test.kara",
                lib_test_body(),
            ),
        };
    let entry_path = target_dir.join(entry_rel);
    let test_path = target_dir.join(test_rel);
    let readme_path = target_dir.join("README.md");
    let gitignore_path = target_dir.join(".gitignore");

    if !opts.force {
        check_no_collision(&manifest_path, "package manifest")?;
        check_no_collision(&entry_path, "entry file")?;
        // Only check the opposite-template entry file when we have reason to:
        // a stray `src/lib.kara` shouldn't block a `--bin` scaffold. The
        // resolution in brainstorming_v42.md covers `src/main.kara` OR
        // `src/lib.kara` because a package is one or the other, never both —
        // scaffolding a bin into a dir that already holds lib.kara is still a
        // collision the user should know about.
        let other_entry = match opts.template {
            Template::Bin => target_dir.join("src/lib.kara"),
            Template::Lib => target_dir.join("src/main.kara"),
        };
        check_no_collision(&other_entry, "entry file")?;
    }

    fs::create_dir_all(target_dir.join("src")).map_err(|e| ScaffoldError::Io {
        path: target_dir.join("src"),
        error: e.to_string(),
    })?;

    write_file(&manifest_path, &manifest_template(package_name))?;
    write_file(&entry_path, &entry_body)?;

    // README, the companion test file, and `.gitignore` are never
    // overwritten — skip if present regardless of --force. Silently
    // clobbering a user's README (or a `.gitignore` that `git init` already
    // wrote) is worse than leaving the stock template off the disk.
    write_file_unless_exists(&test_path, &test_body)?;
    write_file_unless_exists(&readme_path, &readme_template(package_name))?;
    write_file_unless_exists(&gitignore_path, gitignore_template())?;

    Ok(())
}

fn write_file_unless_exists(path: &Path, contents: &str) -> Result<(), ScaffoldError> {
    if path.exists() {
        return Ok(());
    }
    write_file(path, contents)
}

fn check_no_collision(path: &Path, file_kind: &'static str) -> Result<(), ScaffoldError> {
    if path.exists() {
        Err(ScaffoldError::Collision {
            path: path.to_path_buf(),
            file_kind,
        })
    } else {
        Ok(())
    }
}

fn write_file(path: &Path, contents: &str) -> Result<(), ScaffoldError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| ScaffoldError::Io {
            path: parent.to_path_buf(),
            error: e.to_string(),
        })?;
    }
    fs::write(path, contents).map_err(|e| ScaffoldError::Io {
        path: path.to_path_buf(),
        error: e.to_string(),
    })
}

// ── Templates ───────────────────────────────────────────────────

fn manifest_template(package_name: &str) -> String {
    format!(
        "[package]\nname = \"{package_name}\"\nversion = \"0.1.0\"\nauthors = []\nedition = \"2026\"\n\n[dependencies]\n"
    )
}

fn bin_main_body() -> String {
    "fn main() {\n    println(\"Hello, world!\")\n}\n".to_string()
}

fn bin_main_test_body() -> String {
    "test \"placeholder\" {\n    assert_eq(1 + 1, 2);\n}\n".to_string()
}

fn lib_body() -> String {
    "/// Adds two integers.\npub fn add(a: i64, b: i64) -> i64 {\n    a + b\n}\n".to_string()
}

fn lib_test_body() -> String {
    "test \"add sums two integers\" {\n    assert_eq(add(2, 3), 5);\n}\n".to_string()
}

fn readme_template(package_name: &str) -> String {
    format!("# {package_name}\n")
}

fn gitignore_template() -> &'static str {
    "/dist/\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_accepts_snake_case() {
        assert!(validate_package_name("hello").is_ok());
        assert!(validate_package_name("hello_world").is_ok());
        assert!(validate_package_name("my_app_2").is_ok());
        assert!(validate_package_name("a").is_ok());
    }

    #[test]
    fn validate_rejects_empty() {
        let err = validate_package_name("").unwrap_err();
        assert!(matches!(
            err,
            ScaffoldError::InvalidName {
                suggestion: None,
                ..
            }
        ));
    }

    #[test]
    fn validate_rejects_hyphen_with_suggestion() {
        let err = validate_package_name("my-app").unwrap_err();
        match err {
            ScaffoldError::InvalidName { value, suggestion } => {
                assert_eq!(value, "my-app");
                assert_eq!(suggestion, Some("my_app".to_string()));
            }
            other => panic!("expected InvalidName, got {other:?}"),
        }
    }

    #[test]
    fn validate_rejects_uppercase_with_suggestion() {
        let err = validate_package_name("MyApp").unwrap_err();
        match err {
            ScaffoldError::InvalidName {
                suggestion: Some(s),
                ..
            } => {
                assert_eq!(s, "myapp");
            }
            other => panic!("expected InvalidName with suggestion, got {other:?}"),
        }
    }

    #[test]
    fn validate_rejects_leading_digit_no_suggestion() {
        let err = validate_package_name("0foo").unwrap_err();
        assert!(matches!(
            err,
            ScaffoldError::InvalidName {
                suggestion: None,
                ..
            }
        ));
    }

    #[test]
    fn validate_rejects_reserved_keyword() {
        for kw in &["fn", "let", "mut", "pub", "mod", "if"] {
            let err = validate_package_name(kw).unwrap_err();
            assert!(
                matches!(err, ScaffoldError::ReservedKeyword { .. }),
                "expected ReservedKeyword for `{kw}`, got {err:?}",
            );
        }
    }

    #[test]
    fn validate_rejects_non_ascii() {
        let err = validate_package_name("café").unwrap_err();
        assert!(matches!(err, ScaffoldError::InvalidName { .. }));
    }

    #[test]
    fn manifest_template_contains_expected_keys() {
        let t = manifest_template("hello");
        assert!(t.contains("name = \"hello\""));
        assert!(t.contains("version = \"0.1.0\""));
        assert!(t.contains("authors = []"));
        assert!(t.contains("edition = \"2026\""));
        assert!(t.contains("[dependencies]"));
    }
}
