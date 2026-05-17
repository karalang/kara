//! `kara.toml` project manifest parsing (CR-24 slice 2).
//!
//! The manifest is the canonical project-identity signal for multi-file
//! compilation — see `docs/design.md § Package System`. For v1 the compiler
//! parses only `[package].name` (required) and `[package].edition` (optional),
//! per `brainstorming/brainstorming_v41.md § P1`. Every other field is
//! **ignored, not rejected**: a user's `[dependencies]`, `[workspace]`, or
//! `[build]` table is accepted but has no effect until the package-manager
//! work lands in a later phase. Unknown keys *inside* `[package]` emit a soft
//! warning (so anything outside `{name, edition, version, authors}` surfaces
//! a hint that it is ignored); invalid TOML is a hard error. `version` and
//! `authors` are tolerated silently in v1 — they carry no semantic behavior
//! until the package-manager CR lands, but `karac init` writes them into the
//! canonical template and the scaffolded manifest must not warn on first
//! build (see `docs/design.md § Package System § Required and optional fields`).
//!
//! Project-root discovery walks upward from a starting directory looking for
//! `kara.toml`; the first match wins. If no manifest is found before the
//! filesystem root, callers should emit `E0227 NotInsideKaraProject`. The
//! single-file `karac run file.kara` path does **not** pass through this
//! module — it remains the escape hatch for toy programs and book examples.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Default language edition for a manifest that omits `[package].edition`.
pub const DEFAULT_EDITION: &str = "2026";

/// Canonical manifest filename.
pub const MANIFEST_FILENAME: &str = "kara.toml";

/// The only edition the v1 compiler knows about. Anything else is rejected so
/// a future edition bump produces a clear error instead of silently compiling
/// against the wrong language rules.
const KNOWN_EDITIONS: &[&str] = &["2026"];

/// `[package]` keys recognized in v1. Beyond `name` and `edition` (which drive
/// compilation), `version` and `authors` are accepted silently so that
/// manifests emitted by `karac init` (which writes the canonical template) do
/// not warn on first build. Anything outside this set produces a soft warning.
const KNOWN_PACKAGE_KEYS: &[&str] = &[
    "name",
    "edition",
    "version",
    "authors",
    "profile",
    "kara-version",
];

/// Target execution environment — constrains which effects are legal at
/// `extern` declaration sites and which stdlib layers are available.
///
/// | Profile   | Stdlib layers     | Forbidden at `extern` sites            |
/// |-----------|-------------------|----------------------------------------|
/// | `default` | core + alloc + std| none                                   |
/// | `embedded`| core + alloc      | `allocates(Heap)`                      |
/// | `kernel`  | core only         | `allocates(Heap)`, `panics`, `blocks`, `suspends` |
///
/// Specified as `[package].profile = "embedded"` in `kara.toml`.
/// Absent means `default`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CompileProfile {
    #[default]
    Default,
    Embedded,
    Kernel,
}

impl CompileProfile {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "default" => Some(Self::Default),
            "embedded" => Some(Self::Embedded),
            "kernel" => Some(Self::Kernel),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Embedded => "embedded",
            Self::Kernel => "kernel",
        }
    }
}

/// Parsed manifest surface. Everything beyond `name`, `edition`, and the
/// optional `[test.resources]` table is dropped on the floor in v1;
/// `warnings` carries soft notices about unknown keys encountered inside
/// `[package]` so the CLI can surface them.
#[derive(Debug, Clone)]
pub struct Manifest {
    pub name: String,
    pub edition: String,
    /// Target execution environment. Controls which effects are legal at
    /// `extern` declaration sites and which stdlib layers are available.
    /// Defaults to `CompileProfile::Default` when absent from the manifest.
    pub profile: CompileProfile,
    /// Optional `[test.resources]` table — maps a fully-qualified resource
    /// path (e.g. `"db.UserDB"`) to the shell command that probes whether
    /// the resource is healthy. Used by `karac test` to gate
    /// `#[test(requires = [...])]` tests; missing or empty tables fall
    /// back to the env-var probe (`KARA_RESOURCE_*`). Stored as a
    /// `BTreeMap` so iteration order is stable across runs (only matters
    /// when surfaced in diagnostics, but cheap to guarantee).
    pub test_resources: BTreeMap<String, String>,
    /// `[package].kara-version` — the minimum compiler version this
    /// package requires (MSRV in Rust parlance). Stored as the raw
    /// version string the manifest declared; `None` when the field
    /// is absent. The resolver enforces this against the active
    /// toolchain version per design.md once the version-comparison
    /// pipeline lands (separate slice). For now the field is purely
    /// structural — accepted, surfaced through manifest output, but
    /// not validated against the running compiler.
    pub kara_version: Option<String>,
    pub warnings: Vec<ManifestWarning>,
}

/// A soft, non-fatal manifest observation — e.g. an unknown `[package]` key.
/// Carries a line number when available so `karac` can point at it.
#[derive(Debug, Clone)]
pub struct ManifestWarning {
    pub line: Option<usize>,
    pub message: String,
}

/// Fatal manifest load / parse errors. `NotInsideKaraProject` maps to
/// `E0227`; the rest are generic manifest diagnostics surfaced by the CLI.
#[derive(Debug)]
pub enum ManifestError {
    NotInsideKaraProject {
        searched_from: PathBuf,
    },
    FileRead {
        path: PathBuf,
        error: String,
    },
    InvalidToml {
        path: PathBuf,
        message: String,
    },
    MissingPackageSection {
        path: PathBuf,
    },
    MissingPackageName {
        path: PathBuf,
    },
    InvalidFieldType {
        path: PathBuf,
        key: String,
        expected: &'static str,
    },
    InvalidPackageName {
        path: PathBuf,
        value: String,
    },
    UnknownEdition {
        path: PathBuf,
        value: String,
    },
    InvalidTestResource {
        path: PathBuf,
        key: String,
        expected: &'static str,
    },
}

impl ManifestError {
    /// Diagnostic code for this error, when one is assigned. Only
    /// `NotInsideKaraProject` has a formal code in CR-24 slice 2 — other
    /// parse-side errors share a generic bucket until the structured
    /// diagnostic registry gains a manifest phase.
    pub fn code(&self) -> Option<&'static str> {
        match self {
            ManifestError::NotInsideKaraProject { .. } => Some("E0227"),
            _ => None,
        }
    }
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ManifestError::NotInsideKaraProject { searched_from } => write!(
                f,
                "not inside a Kāra project: no `{}` found in `{}` or any parent directory. Add a `{}` or run a single file with `karac run file.kara`.",
                MANIFEST_FILENAME,
                searched_from.display(),
                MANIFEST_FILENAME,
            ),
            ManifestError::FileRead { path, error } => {
                write!(f, "cannot read `{}`: {}", path.display(), error)
            }
            ManifestError::InvalidToml { path, message } => {
                write!(f, "invalid TOML in `{}`: {}", path.display(), message)
            }
            ManifestError::MissingPackageSection { path } => write!(
                f,
                "`{}` is missing the required `[package]` section",
                path.display(),
            ),
            ManifestError::MissingPackageName { path } => write!(
                f,
                "`{}` is missing the required `[package].name` key",
                path.display(),
            ),
            ManifestError::InvalidFieldType {
                path,
                key,
                expected,
            } => write!(
                f,
                "`{}`: `[package].{}` must be {}",
                path.display(),
                key,
                expected,
            ),
            ManifestError::InvalidPackageName { path, value } => write!(
                f,
                "`{}`: `[package].name` cannot be empty (got `\"{}\"`)",
                path.display(),
                value,
            ),
            ManifestError::UnknownEdition { path, value } => write!(
                f,
                "`{}`: unknown `[package].edition = \"{}\"`. Supported editions: {}.",
                path.display(),
                value,
                KNOWN_EDITIONS.join(", "),
            ),
            ManifestError::InvalidTestResource {
                path,
                key,
                expected,
            } => write!(
                f,
                "`{}`: `[test.resources].\"{}\"` must be {}",
                path.display(),
                key,
                expected,
            ),
        }
    }
}

/// Walk up from `start_dir` looking for `kara.toml`. Returns the directory
/// containing the manifest (the *project root*), not the manifest path itself.
/// The search stops at the filesystem root; callers that want to surface
/// `E0227` should map `None` to `ManifestError::NotInsideKaraProject`.
pub fn discover_project_root(start_dir: &Path) -> Option<PathBuf> {
    let mut cursor = if start_dir.is_absolute() {
        start_dir.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(start_dir)
    };
    loop {
        if cursor.join(MANIFEST_FILENAME).is_file() {
            return Some(cursor);
        }
        if !cursor.pop() {
            return None;
        }
    }
}

/// Convenience: walk up from `start_dir`, locate the project root, and parse
/// its manifest. Returns `(project_root, manifest)` on success.
pub fn load_from_cwd(start_dir: &Path) -> Result<(PathBuf, Manifest), ManifestError> {
    match discover_project_root(start_dir) {
        Some(root) => {
            let manifest = load_from_root(&root)?;
            Ok((root, manifest))
        }
        None => Err(ManifestError::NotInsideKaraProject {
            searched_from: start_dir.to_path_buf(),
        }),
    }
}

/// Load and parse the `kara.toml` at `project_root/kara.toml`.
pub fn load_from_root(project_root: &Path) -> Result<Manifest, ManifestError> {
    let path = project_root.join(MANIFEST_FILENAME);
    let source = fs::read_to_string(&path).map_err(|e| ManifestError::FileRead {
        path: path.clone(),
        error: e.to_string(),
    })?;
    parse_manifest(&path, &source)
}

/// Parse a manifest source string. The caller supplies `path` only for use
/// in error messages — nothing here touches the filesystem.
pub fn parse_manifest(path: &Path, source: &str) -> Result<Manifest, ManifestError> {
    let table: toml::Table =
        source
            .parse()
            .map_err(|e: toml::de::Error| ManifestError::InvalidToml {
                path: path.to_path_buf(),
                message: e.message().to_string(),
            })?;

    let package = table
        .get("package")
        .ok_or_else(|| ManifestError::MissingPackageSection {
            path: path.to_path_buf(),
        })?
        .as_table()
        .ok_or_else(|| ManifestError::InvalidFieldType {
            path: path.to_path_buf(),
            key: "package".to_string(),
            expected: "a table (e.g. `[package]`)",
        })?;

    let name = match package.get("name") {
        Some(toml::Value::String(s)) => {
            if s.is_empty() {
                return Err(ManifestError::InvalidPackageName {
                    path: path.to_path_buf(),
                    value: s.clone(),
                });
            }
            s.clone()
        }
        Some(_) => {
            return Err(ManifestError::InvalidFieldType {
                path: path.to_path_buf(),
                key: "name".to_string(),
                expected: "a string",
            });
        }
        None => {
            return Err(ManifestError::MissingPackageName {
                path: path.to_path_buf(),
            });
        }
    };

    let profile = match package.get("profile") {
        Some(toml::Value::String(s)) => {
            CompileProfile::parse(s.as_str()).ok_or_else(|| ManifestError::InvalidFieldType {
                path: path.to_path_buf(),
                key: "profile".to_string(),
                expected: "one of \"default\", \"embedded\", or \"kernel\"",
            })?
        }
        Some(_) => {
            return Err(ManifestError::InvalidFieldType {
                path: path.to_path_buf(),
                key: "profile".to_string(),
                expected: "a string (\"default\", \"embedded\", or \"kernel\")",
            });
        }
        None => CompileProfile::Default,
    };

    let edition = match package.get("edition") {
        Some(toml::Value::String(s)) => {
            if !KNOWN_EDITIONS.contains(&s.as_str()) {
                return Err(ManifestError::UnknownEdition {
                    path: path.to_path_buf(),
                    value: s.clone(),
                });
            }
            s.clone()
        }
        Some(_) => {
            return Err(ManifestError::InvalidFieldType {
                path: path.to_path_buf(),
                key: "edition".to_string(),
                expected: "a string",
            });
        }
        None => DEFAULT_EDITION.to_string(),
    };

    // `version` and `authors` are recognized at `[package]` level but carry no
    // v1 semantic behavior — they parse silently so `karac init`'s canonical
    // manifest does not warn on first build. Anything else inside `[package]`
    // produces a soft warning so real typos surface without blocking the build.
    let mut warnings = Vec::new();
    for key in package.keys() {
        if !KNOWN_PACKAGE_KEYS.contains(&key.as_str()) {
            warnings.push(ManifestWarning {
                line: None,
                message: format!(
                    "unknown key `[package].{key}` — ignored in v1 (reserved for a later release)"
                ),
            });
        }
    }
    warnings.sort_by(|a, b| a.message.cmp(&b.message));

    // `[package].kara-version` — optional MSRV constraint. Wrong
    // type is a hard error (typos shouldn't silently disable the
    // constraint); absent is the common case. Per design.md the
    // value is a free-form version string surfaced verbatim in
    // resolution diagnostics; no parsing-time validation today.
    let kara_version = match package.get("kara-version") {
        Some(toml::Value::String(s)) => {
            if s.trim().is_empty() {
                return Err(ManifestError::InvalidFieldType {
                    path: path.to_path_buf(),
                    key: "kara-version".to_string(),
                    expected: "a non-empty version string (e.g. \"1.0\" or \"1.2.3\")",
                });
            }
            Some(s.clone())
        }
        Some(_) => {
            return Err(ManifestError::InvalidFieldType {
                path: path.to_path_buf(),
                key: "kara-version".to_string(),
                expected: "a string version constraint",
            });
        }
        None => None,
    };

    let test_resources = parse_test_resources(path, &table)?;

    Ok(Manifest {
        name,
        edition,
        profile,
        test_resources,
        kara_version,
        warnings,
    })
}

/// Parse the optional `[test.resources]` sub-table — `karac test` uses these
/// shell commands (when present) instead of the env-var probe to decide
/// whether a `#[test(requires = [...])]` resource is healthy. Wrong shapes
/// (the `[test]` parent isn't a table, `resources` isn't a table, or a value
/// isn't a string) are hard errors so typos in the manifest surface
/// immediately rather than silently disabling the override.
fn parse_test_resources(
    path: &Path,
    table: &toml::Table,
) -> Result<BTreeMap<String, String>, ManifestError> {
    let Some(test_value) = table.get("test") else {
        return Ok(BTreeMap::new());
    };
    let test_table = test_value
        .as_table()
        .ok_or_else(|| ManifestError::InvalidFieldType {
            path: path.to_path_buf(),
            key: "test".to_string(),
            expected: "a table (e.g. `[test]`)",
        })?;
    let Some(resources_value) = test_table.get("resources") else {
        return Ok(BTreeMap::new());
    };
    let resources_table =
        resources_value
            .as_table()
            .ok_or_else(|| ManifestError::InvalidFieldType {
                path: path.to_path_buf(),
                key: "test.resources".to_string(),
                expected: "a table (e.g. `[test.resources]`)",
            })?;
    let mut out = BTreeMap::new();
    for (key, value) in resources_table {
        match value {
            toml::Value::String(s) => {
                out.insert(key.clone(), s.clone());
            }
            _ => {
                return Err(ManifestError::InvalidTestResource {
                    path: path.to_path_buf(),
                    key: key.clone(),
                    expected: "a string (the shell command that probes the resource)",
                });
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p() -> PathBuf {
        PathBuf::from("kara.toml")
    }

    #[test]
    fn parses_minimum_manifest() {
        let src = r#"[package]
name = "hello"
"#;
        let m = parse_manifest(&p(), src).unwrap();
        assert_eq!(m.name, "hello");
        assert_eq!(m.edition, DEFAULT_EDITION);
        assert!(m.warnings.is_empty());
    }

    #[test]
    fn parses_name_and_edition() {
        let src = r#"[package]
name = "hello"
edition = "2026"
"#;
        let m = parse_manifest(&p(), src).unwrap();
        assert_eq!(m.name, "hello");
        assert_eq!(m.edition, "2026");
    }

    #[test]
    fn ignored_sections_do_not_break_parse() {
        let src = r#"[package]
name = "hello"

[dependencies]
http = "1.2"
json = { version = "0.8", git = "https://example.com/json-kara" }

[dev-dependencies]
proptest = "0.4"

[build]
target = "x86_64-linux"
"#;
        let m = parse_manifest(&p(), src).unwrap();
        assert_eq!(m.name, "hello");
        // No warnings — unknown *sections* are silent, only unknown keys inside
        // [package] warn.
        assert!(m.warnings.is_empty());
    }

    #[test]
    fn unknown_package_key_soft_warns() {
        let src = r#"[package]
name = "hello"
homepage = "https://example.com"
"#;
        let m = parse_manifest(&p(), src).unwrap();
        assert_eq!(m.name, "hello");
        assert_eq!(m.warnings.len(), 1);
        assert!(m.warnings[0].message.contains("homepage"));
    }

    #[test]
    fn version_and_authors_parse_silently() {
        let src = r#"[package]
name = "hello"
version = "0.1.0"
authors = ["alice"]
edition = "2026"
"#;
        let m = parse_manifest(&p(), src).unwrap();
        assert_eq!(m.name, "hello");
        assert_eq!(m.edition, "2026");
        assert!(
            m.warnings.is_empty(),
            "canonical scaffolded manifest must not warn: {:?}",
            m.warnings,
        );
    }

    #[test]
    fn missing_package_section_errors() {
        let src = r#"[dependencies]
http = "1.2"
"#;
        let err = parse_manifest(&p(), src).unwrap_err();
        assert!(matches!(err, ManifestError::MissingPackageSection { .. }));
    }

    #[test]
    fn missing_name_errors() {
        let src = r#"[package]
edition = "2026"
"#;
        let err = parse_manifest(&p(), src).unwrap_err();
        assert!(matches!(err, ManifestError::MissingPackageName { .. }));
    }

    #[test]
    fn empty_name_errors() {
        let src = r#"[package]
name = ""
"#;
        let err = parse_manifest(&p(), src).unwrap_err();
        assert!(matches!(err, ManifestError::InvalidPackageName { .. }));
    }

    #[test]
    fn wrong_name_type_errors() {
        let src = r#"[package]
name = 42
"#;
        let err = parse_manifest(&p(), src).unwrap_err();
        match err {
            ManifestError::InvalidFieldType { key, .. } => assert_eq!(key, "name"),
            other => panic!("expected InvalidFieldType, got {other:?}"),
        }
    }

    #[test]
    fn unknown_edition_errors() {
        let src = r#"[package]
name = "hello"
edition = "1999"
"#;
        let err = parse_manifest(&p(), src).unwrap_err();
        assert!(matches!(err, ManifestError::UnknownEdition { .. }));
    }

    #[test]
    fn invalid_toml_is_hard_error() {
        let src = "[[[not valid toml";
        let err = parse_manifest(&p(), src).unwrap_err();
        assert!(matches!(err, ManifestError::InvalidToml { .. }));
    }

    #[test]
    fn not_inside_kara_project_has_e0227() {
        let err = ManifestError::NotInsideKaraProject {
            searched_from: PathBuf::from("/tmp/nowhere"),
        };
        assert_eq!(err.code(), Some("E0227"));
    }

    #[test]
    fn no_test_resources_table_yields_empty_map() {
        let src = r#"[package]
name = "hello"
"#;
        let m = parse_manifest(&p(), src).unwrap();
        assert!(m.test_resources.is_empty());
    }

    #[test]
    fn test_resources_table_parses() {
        let src = r#"[package]
name = "hello"

[test.resources]
"db.UserDB" = "pg_isready -d $DATABASE_URL"
"payment.PaymentAPI" = "curl -sf $PAYMENT_API_URL/health"
"#;
        let m = parse_manifest(&p(), src).unwrap();
        assert_eq!(m.test_resources.len(), 2);
        assert_eq!(
            m.test_resources.get("db.UserDB").map(String::as_str),
            Some("pg_isready -d $DATABASE_URL"),
        );
        assert_eq!(
            m.test_resources
                .get("payment.PaymentAPI")
                .map(String::as_str),
            Some("curl -sf $PAYMENT_API_URL/health"),
        );
    }

    #[test]
    fn test_resources_value_must_be_string() {
        let src = r#"[package]
name = "hello"

[test.resources]
"db.UserDB" = 42
"#;
        let err = parse_manifest(&p(), src).unwrap_err();
        match err {
            ManifestError::InvalidTestResource { key, .. } => assert_eq!(key, "db.UserDB"),
            other => panic!("expected InvalidTestResource, got {other:?}"),
        }
    }

    // ── kara-version MSRV slice 1 (parser-only capture) ────────────

    #[test]
    fn kara_version_absent_is_none() {
        let src = r#"[package]
name = "hello"
"#;
        let m = parse_manifest(&p(), src).unwrap();
        assert!(m.kara_version.is_none());
    }

    #[test]
    fn kara_version_captured_when_present() {
        let src = r#"[package]
name = "hello"
kara-version = "1.0"
"#;
        let m = parse_manifest(&p(), src).unwrap();
        assert_eq!(m.kara_version.as_deref(), Some("1.0"));
        // No warning — `kara-version` is a recognised key.
        assert!(m.warnings.is_empty(), "got warnings: {:?}", m.warnings);
    }

    #[test]
    fn kara_version_accepts_semver_triple() {
        let src = r#"[package]
name = "hello"
kara-version = "1.2.3"
"#;
        let m = parse_manifest(&p(), src).unwrap();
        assert_eq!(m.kara_version.as_deref(), Some("1.2.3"));
    }

    #[test]
    fn kara_version_accepts_caret_constraint() {
        // Cargo-style constraint strings — the parser stores the raw
        // string; resolution-time interpretation is a follow-up
        // slice. Today only the parse-time capture is pinned.
        let src = r#"[package]
name = "hello"
kara-version = "^1.0"
"#;
        let m = parse_manifest(&p(), src).unwrap();
        assert_eq!(m.kara_version.as_deref(), Some("^1.0"));
    }

    #[test]
    fn kara_version_wrong_type_is_hard_error() {
        let src = r#"[package]
name = "hello"
kara-version = 1.0
"#;
        let err = parse_manifest(&p(), src).unwrap_err();
        match err {
            ManifestError::InvalidFieldType { key, .. } => {
                assert_eq!(key, "kara-version");
            }
            other => panic!("expected InvalidFieldType, got {other:?}"),
        }
    }

    #[test]
    fn kara_version_empty_string_is_hard_error() {
        // Empty version string is meaningless and is more likely a
        // mistake than an intentional "no constraint". Hard-error
        // rather than silently accepting.
        let src = r#"[package]
name = "hello"
kara-version = ""
"#;
        let err = parse_manifest(&p(), src).unwrap_err();
        match err {
            ManifestError::InvalidFieldType { key, .. } => {
                assert_eq!(key, "kara-version");
            }
            other => panic!("expected InvalidFieldType, got {other:?}"),
        }
    }

    #[test]
    fn kara_version_whitespace_only_is_hard_error() {
        let src = r#"[package]
name = "hello"
kara-version = "   "
"#;
        let err = parse_manifest(&p(), src).unwrap_err();
        match err {
            ManifestError::InvalidFieldType { key, .. } => {
                assert_eq!(key, "kara-version");
            }
            other => panic!("expected InvalidFieldType, got {other:?}"),
        }
    }

    #[test]
    fn kara_version_recognised_key_does_not_warn() {
        // Regression pin: `kara-version` was added to
        // KNOWN_PACKAGE_KEYS. If a future refactor drops it, the
        // unknown-key warning would silently fire and this test
        // catches that.
        let src = r#"[package]
name = "hello"
kara-version = "1.0"
"#;
        let m = parse_manifest(&p(), src).unwrap();
        assert!(
            m.warnings.is_empty(),
            "kara-version should not produce unknown-key warning; got: {:?}",
            m.warnings
        );
    }
}
