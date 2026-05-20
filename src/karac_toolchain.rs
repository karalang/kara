//! `karac-toolchain.toml` reader (tracker line 892).
//!
//! Pins the toolchain version a project expects. The file sits at the
//! project (or workspace) root alongside `kara.toml`. Schema (v1):
//!
//! ```toml
//! version = "1.0"           # required — Cargo-style semver constraint
//! targets = ["x86_64-..."]  # optional — list of target triples this
//!                           # project expects to be installed
//! ```
//!
//! Channels, components, and install profiles are post-v1 (the deferred
//! `karaup` work activates them). Today karac only reads the file: if
//! the active toolchain doesn't satisfy `version`, the build emits a
//! version-mismatch diagnostic naming the recommended action. There is
//! no auto-switch — that work is the `karaup` follow-up.

use std::fs;
use std::path::{Path, PathBuf};

/// Canonical toolchain-pin filename.
pub const TOOLCHAIN_FILENAME: &str = "karac-toolchain.toml";

/// Parsed `karac-toolchain.toml`. `version` is the active-toolchain
/// constraint; `targets` is the optional list of target triples this
/// project expects to be installed (used by the v1 reader to surface
/// a note when an obviously-missing target shows up, and consumed by
/// karaup for `karaup install` once that lands).
#[derive(Debug, Clone)]
pub struct ToolchainSpec {
    pub version: semver::VersionReq,
    pub targets: Vec<String>,
}

/// Errors when loading / parsing the file.
#[derive(Debug)]
pub enum ToolchainError {
    /// `fs::read_to_string` failed for a non-not-found reason
    /// (permission denied, IO error, etc.). Missing files are not an
    /// error — the loader returns `Ok(None)`.
    FileRead { path: PathBuf, error: String },
    /// TOML couldn't be parsed.
    InvalidToml { path: PathBuf, message: String },
    /// `version` field is missing.
    MissingVersion { path: PathBuf },
    /// `version` is the wrong TOML type (not a string).
    InvalidVersionType { path: PathBuf },
    /// `version` parses as TOML but isn't a valid semver constraint.
    InvalidVersionConstraint { path: PathBuf, value: String },
    /// `targets` is the wrong TOML type (not an array).
    InvalidTargetsType { path: PathBuf },
    /// A `targets` entry is the wrong shape (not a string).
    InvalidTargetEntry { path: PathBuf, index: usize },
    /// A `targets` entry is empty / whitespace-only.
    EmptyTargetEntry { path: PathBuf, index: usize },
}

impl ToolchainError {
    /// Stable symbolic diagnostic code for downstream tooling.
    pub fn code(&self) -> &'static str {
        match self {
            Self::FileRead { .. } => "E_TOOLCHAIN_FILE_READ",
            Self::InvalidToml { .. } => "E_TOOLCHAIN_INVALID_TOML",
            Self::MissingVersion { .. } => "E_TOOLCHAIN_MISSING_VERSION",
            Self::InvalidVersionType { .. } => "E_TOOLCHAIN_INVALID_VERSION_TYPE",
            Self::InvalidVersionConstraint { .. } => "E_TOOLCHAIN_INVALID_VERSION_CONSTRAINT",
            Self::InvalidTargetsType { .. } => "E_TOOLCHAIN_INVALID_TARGETS_TYPE",
            Self::InvalidTargetEntry { .. } => "E_TOOLCHAIN_INVALID_TARGET_ENTRY",
            Self::EmptyTargetEntry { .. } => "E_TOOLCHAIN_EMPTY_TARGET_ENTRY",
        }
    }
}

impl std::fmt::Display for ToolchainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FileRead { path, error } => {
                write!(f, "cannot read `{}`: {}", path.display(), error)
            }
            Self::InvalidToml { path, message } => {
                write!(f, "invalid TOML in `{}`: {}", path.display(), message)
            }
            Self::MissingVersion { path } => write!(
                f,
                "`{}` is missing the required `version` key",
                path.display()
            ),
            Self::InvalidVersionType { path } => write!(
                f,
                "`{}`: `version` must be a string semver constraint (e.g. \"1.0\" or \">=1.2, <2.0\")",
                path.display()
            ),
            Self::InvalidVersionConstraint { path, value } => write!(
                f,
                "`{}`: `version = \"{}\"` is not a valid semver constraint",
                path.display(),
                value
            ),
            Self::InvalidTargetsType { path } => write!(
                f,
                "`{}`: `targets` must be an array of target-triple strings",
                path.display()
            ),
            Self::InvalidTargetEntry { path, index } => write!(
                f,
                "`{}`: `targets[{}]` must be a string target triple",
                path.display(),
                index
            ),
            Self::EmptyTargetEntry { path, index } => write!(
                f,
                "`{}`: `targets[{}]` is empty — provide a non-empty triple",
                path.display(),
                index
            ),
        }
    }
}

/// Diagnostic emitted when the active toolchain doesn't satisfy the
/// declared `version` constraint. Stable symbolic code lets CI / IDE
/// tooling recognize the mismatch.
#[derive(Debug, Clone)]
pub struct ToolchainMismatch {
    pub source: PathBuf,
    pub required: semver::VersionReq,
    pub active: semver::Version,
}

impl ToolchainMismatch {
    pub fn code(&self) -> &'static str {
        "E_TOOLCHAIN_VERSION_MISMATCH"
    }

    /// Human-readable primary diagnostic. Distinct from
    /// `ResolverError::ToolchainTooOld` (which is package-level —
    /// `kara-version` from a dep's manifest); this is project-level
    /// — `karac-toolchain.toml` at the project root.
    pub fn message(&self) -> String {
        format!(
            "active toolchain `{}` does not satisfy `{}` declared in `{}`",
            self.active,
            self.required,
            self.source.display()
        )
    }
}

/// Walk upward from `start_dir` looking for `karac-toolchain.toml`.
/// Returns the path to the file when found, `None` when the search
/// reaches the filesystem root without a hit. Independent walk from
/// `manifest::discover_project_root` because a workspace may pin the
/// toolchain at a parent of the project root.
pub fn discover_toolchain_file(start_dir: &Path) -> Option<PathBuf> {
    let mut cursor = if start_dir.is_absolute() {
        start_dir.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(start_dir)
    };
    loop {
        let candidate = cursor.join(TOOLCHAIN_FILENAME);
        if candidate.is_file() {
            return Some(candidate);
        }
        if !cursor.pop() {
            return None;
        }
    }
}

/// Discover + load + parse the toolchain spec rooted at `start_dir`,
/// or any ancestor. Returns `Ok(None)` when no `karac-toolchain.toml`
/// exists anywhere in the ancestor chain — toolchain pinning is
/// optional. Read / parse failures bubble up as `Err`.
pub fn load_from_start(
    start_dir: &Path,
) -> Result<Option<(PathBuf, ToolchainSpec)>, ToolchainError> {
    let Some(path) = discover_toolchain_file(start_dir) else {
        return Ok(None);
    };
    let source = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            return Err(ToolchainError::FileRead {
                path,
                error: e.to_string(),
            });
        }
    };
    let spec = parse_toolchain_file(&path, &source)?;
    Ok(Some((path, spec)))
}

/// Parse the raw TOML source. Tested directly so the parser surface
/// can be exercised without filesystem juggling.
pub fn parse_toolchain_file(path: &Path, source: &str) -> Result<ToolchainSpec, ToolchainError> {
    let table: toml::Table =
        source
            .parse()
            .map_err(|e: toml::de::Error| ToolchainError::InvalidToml {
                path: path.to_path_buf(),
                message: e.message().to_string(),
            })?;

    let version_raw = match table.get("version") {
        Some(toml::Value::String(s)) => s.clone(),
        Some(_) => {
            return Err(ToolchainError::InvalidVersionType {
                path: path.to_path_buf(),
            });
        }
        None => {
            return Err(ToolchainError::MissingVersion {
                path: path.to_path_buf(),
            });
        }
    };
    let version = semver::VersionReq::parse(version_raw.as_str()).map_err(|_| {
        ToolchainError::InvalidVersionConstraint {
            path: path.to_path_buf(),
            value: version_raw.clone(),
        }
    })?;

    let mut targets = Vec::new();
    if let Some(value) = table.get("targets") {
        let arr = value
            .as_array()
            .ok_or_else(|| ToolchainError::InvalidTargetsType {
                path: path.to_path_buf(),
            })?;
        for (idx, entry) in arr.iter().enumerate() {
            match entry {
                toml::Value::String(s) => {
                    if s.trim().is_empty() {
                        return Err(ToolchainError::EmptyTargetEntry {
                            path: path.to_path_buf(),
                            index: idx,
                        });
                    }
                    targets.push(s.clone());
                }
                _ => {
                    return Err(ToolchainError::InvalidTargetEntry {
                        path: path.to_path_buf(),
                        index: idx,
                    });
                }
            }
        }
    }

    Ok(ToolchainSpec { version, targets })
}

/// Check the active compiler version against the declared constraint.
/// Returns `Ok(())` when the version matches; `Err(ToolchainMismatch)`
/// otherwise. The caller emits the structured diagnostic from the
/// mismatch — separating verification from rendering keeps the unit
/// tests hermetic against output formatting.
pub fn enforce(
    spec: &ToolchainSpec,
    source: &Path,
    active: &semver::Version,
) -> Result<(), ToolchainMismatch> {
    if spec.version.matches(active) {
        Ok(())
    } else {
        Err(ToolchainMismatch {
            source: source.to_path_buf(),
            required: spec.version.clone(),
            active: active.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn p() -> PathBuf {
        PathBuf::from("karac-toolchain.toml")
    }

    fn ver(s: &str) -> semver::Version {
        semver::Version::parse(s).unwrap()
    }

    #[test]
    fn parses_minimal_spec() {
        let src = r#"version = "1.0""#;
        let spec = parse_toolchain_file(&p(), src).unwrap();
        assert!(spec.version.matches(&ver("1.0.5")));
        assert!(spec.targets.is_empty());
    }

    #[test]
    fn parses_caret_version() {
        let src = r#"version = "^1.2""#;
        let spec = parse_toolchain_file(&p(), src).unwrap();
        assert!(spec.version.matches(&ver("1.2.0")));
        assert!(spec.version.matches(&ver("1.9.0")));
        assert!(!spec.version.matches(&ver("2.0.0")));
    }

    #[test]
    fn parses_range_version() {
        let src = r#"version = ">=1.0, <1.5""#;
        let spec = parse_toolchain_file(&p(), src).unwrap();
        assert!(spec.version.matches(&ver("1.2.0")));
        assert!(!spec.version.matches(&ver("1.5.0")));
        assert!(!spec.version.matches(&ver("0.9.0")));
    }

    #[test]
    fn parses_with_targets_list() {
        let src = r#"version = "1.0"
targets = ["x86_64-apple-darwin", "aarch64-unknown-linux-gnu"]"#;
        let spec = parse_toolchain_file(&p(), src).unwrap();
        assert_eq!(spec.targets.len(), 2);
        assert_eq!(spec.targets[0], "x86_64-apple-darwin");
        assert_eq!(spec.targets[1], "aarch64-unknown-linux-gnu");
    }

    #[test]
    fn empty_targets_list_parses_to_empty_vec() {
        let src = r#"version = "1.0"
targets = []"#;
        let spec = parse_toolchain_file(&p(), src).unwrap();
        assert!(spec.targets.is_empty());
    }

    #[test]
    fn missing_version_field_is_error() {
        let src = r#"targets = ["x86_64-apple-darwin"]"#;
        let err = parse_toolchain_file(&p(), src).unwrap_err();
        assert!(matches!(err, ToolchainError::MissingVersion { .. }));
        assert_eq!(err.code(), "E_TOOLCHAIN_MISSING_VERSION");
    }

    #[test]
    fn version_wrong_type_is_error() {
        let src = r#"version = 1.0"#;
        let err = parse_toolchain_file(&p(), src).unwrap_err();
        assert!(matches!(err, ToolchainError::InvalidVersionType { .. }));
        assert_eq!(err.code(), "E_TOOLCHAIN_INVALID_VERSION_TYPE");
    }

    #[test]
    fn invalid_version_constraint_is_error() {
        let src = r#"version = ">>> bogus""#;
        let err = parse_toolchain_file(&p(), src).unwrap_err();
        match err {
            ToolchainError::InvalidVersionConstraint { value, .. } => {
                assert_eq!(value, ">>> bogus");
            }
            other => panic!("expected InvalidVersionConstraint, got {other:?}"),
        }
    }

    #[test]
    fn targets_must_be_array() {
        let src = r#"version = "1.0"
targets = "x86_64-apple-darwin""#;
        let err = parse_toolchain_file(&p(), src).unwrap_err();
        assert!(matches!(err, ToolchainError::InvalidTargetsType { .. }));
    }

    #[test]
    fn target_entry_must_be_string() {
        let src = r#"version = "1.0"
targets = [42]"#;
        let err = parse_toolchain_file(&p(), src).unwrap_err();
        match err {
            ToolchainError::InvalidTargetEntry { index, .. } => assert_eq!(index, 0),
            other => panic!("expected InvalidTargetEntry, got {other:?}"),
        }
    }

    #[test]
    fn empty_target_entry_rejected() {
        let src = r#"version = "1.0"
targets = ["x86_64-apple-darwin", "   "]"#;
        let err = parse_toolchain_file(&p(), src).unwrap_err();
        match err {
            ToolchainError::EmptyTargetEntry { index, .. } => assert_eq!(index, 1),
            other => panic!("expected EmptyTargetEntry, got {other:?}"),
        }
    }

    #[test]
    fn invalid_toml_is_error() {
        let src = "[[[ not valid";
        let err = parse_toolchain_file(&p(), src).unwrap_err();
        assert!(matches!(err, ToolchainError::InvalidToml { .. }));
    }

    #[test]
    fn enforce_accepts_matching_version() {
        let src = r#"version = "^1.0""#;
        let spec = parse_toolchain_file(&p(), src).unwrap();
        let r = enforce(&spec, &p(), &ver("1.5.0"));
        assert!(r.is_ok());
    }

    #[test]
    fn enforce_rejects_mismatched_version() {
        let src = r#"version = "^1.0""#;
        let spec = parse_toolchain_file(&p(), src).unwrap();
        let mismatch = enforce(&spec, &p(), &ver("2.0.0")).unwrap_err();
        assert_eq!(mismatch.code(), "E_TOOLCHAIN_VERSION_MISMATCH");
        assert_eq!(mismatch.active, ver("2.0.0"));
        let msg = mismatch.message();
        assert!(msg.contains("2.0.0"));
        assert!(msg.contains("karac-toolchain.toml"));
    }

    fn fresh_tempdir(slug: &str) -> PathBuf {
        let tmp = std::env::temp_dir().join(format!(
            "karac-toolchain-test-{slug}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        tmp
    }

    fn write_file(path: &Path, content: &str) {
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
    }

    #[test]
    fn discover_finds_file_in_start_dir() {
        let tmp = fresh_tempdir("discover-flat");
        write_file(&tmp.join(TOOLCHAIN_FILENAME), "version = \"1.0\"");
        let found = discover_toolchain_file(&tmp);
        let cleaned = std::fs::remove_dir_all(&tmp);
        assert!(cleaned.is_ok());
        assert!(found.is_some());
    }

    #[test]
    fn discover_walks_upward_to_ancestor() {
        let tmp = fresh_tempdir("discover-ancestor");
        let nested = tmp.join("a/b/c");
        std::fs::create_dir_all(&nested).unwrap();
        write_file(&tmp.join(TOOLCHAIN_FILENAME), "version = \"1.0\"");
        let found = discover_toolchain_file(&nested);
        let _ = std::fs::remove_dir_all(&tmp);
        assert!(found.is_some());
        assert!(found.unwrap().ends_with(TOOLCHAIN_FILENAME));
    }

    #[test]
    fn discover_returns_none_when_absent() {
        let tmp = fresh_tempdir("discover-none");
        let found = discover_toolchain_file(&tmp);
        let _ = std::fs::remove_dir_all(&tmp);
        assert!(found.is_none());
    }

    #[test]
    fn load_from_start_returns_none_when_absent() {
        let tmp = fresh_tempdir("load-none");
        let r = load_from_start(&tmp);
        let _ = std::fs::remove_dir_all(&tmp);
        assert!(matches!(r, Ok(None)));
    }

    #[test]
    fn load_from_start_round_trips_valid_file() {
        let tmp = fresh_tempdir("load-round-trip");
        write_file(
            &tmp.join(TOOLCHAIN_FILENAME),
            "version = \"^1.5\"\ntargets = [\"x86_64-apple-darwin\"]\n",
        );
        let r = load_from_start(&tmp);
        let _ = std::fs::remove_dir_all(&tmp);
        let (path, spec) = r.unwrap().unwrap();
        assert!(path.ends_with(TOOLCHAIN_FILENAME));
        assert!(spec.version.matches(&ver("1.5.0")));
        assert_eq!(spec.targets, vec!["x86_64-apple-darwin".to_string()]);
    }

    #[test]
    fn error_codes_are_stable() {
        // Downstream tooling matches on these strings. Lock the set.
        let codes = [
            ToolchainError::FileRead {
                path: p(),
                error: "x".into(),
            }
            .code(),
            ToolchainError::InvalidToml {
                path: p(),
                message: "x".into(),
            }
            .code(),
            ToolchainError::MissingVersion { path: p() }.code(),
            ToolchainError::InvalidVersionType { path: p() }.code(),
            ToolchainError::InvalidVersionConstraint {
                path: p(),
                value: "x".into(),
            }
            .code(),
            ToolchainError::InvalidTargetsType { path: p() }.code(),
            ToolchainError::InvalidTargetEntry {
                path: p(),
                index: 0,
            }
            .code(),
            ToolchainError::EmptyTargetEntry {
                path: p(),
                index: 0,
            }
            .code(),
        ];
        assert_eq!(
            codes,
            [
                "E_TOOLCHAIN_FILE_READ",
                "E_TOOLCHAIN_INVALID_TOML",
                "E_TOOLCHAIN_MISSING_VERSION",
                "E_TOOLCHAIN_INVALID_VERSION_TYPE",
                "E_TOOLCHAIN_INVALID_VERSION_CONSTRAINT",
                "E_TOOLCHAIN_INVALID_TARGETS_TYPE",
                "E_TOOLCHAIN_INVALID_TARGET_ENTRY",
                "E_TOOLCHAIN_EMPTY_TARGET_ENTRY",
            ]
        );
    }
}
