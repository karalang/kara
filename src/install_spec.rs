//! Parse a `karac install <bin-spec>` argument into a typed source descriptor.
//!
//! The CLI accepts a single positional spec string in one of four shapes that
//! mirror the manifest dependency-entry vocabulary (`docs/design.md § Package
//! System > Dependencies`):
//!
//! - `path=<filesystem-path>`      — build from a local source directory
//! - `git=<url>`                   — build from a git repository
//! - `<name>`                      — registry-proxy reference (latest)
//! - `<name>@<version-constraint>` — pinned registry-proxy reference
//!
//! This module is the *spec resolution* slice of tracker line 871. The actual
//! build + install step (carve-out (b) — feed the resolved source into the
//! existing build pipeline, then drop the executable into `~/.kara/bin/`)
//! lands alongside the dependency-resolution wiring. Today the install
//! command parses + echoes the resolved source so CI scripts and downstream
//! tooling can validate the spec shape they intend to ship before the
//! pipeline integration goes live.
//!
//! Cross-references: `src/manifest.rs::DependencySpec` (the manifest-side
//! shape this CLI grammar mirrors), `src/scaffold.rs::validate_package_name`
//! (the canonical package-name rules — reused here so the install spec, the
//! `karac new` scaffolder, and manifest validation agree on a single name
//! vocabulary).

use std::path::PathBuf;

use semver::VersionReq;

use crate::scaffold;

/// Typed install source. Mirrors the three manifest dependency forms
/// (`Path`, `Git`, `Registry`) — the workspace form is meaningless for an
/// install spec (a workspace dep references another workspace member, which
/// doesn't make sense as a CLI install target).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallSource {
    /// `path=<filesystem-path>` — build from a local source directory.
    Path { path: PathBuf },
    /// `git=<url>` — build from a git repository. v1 has no ref selectors
    /// (branch / tag / rev); the install pipeline defaults to the default
    /// branch. Ref selectors land alongside the git-fetch slice (carve-out).
    Git { url: String },
    /// `<name>` or `<name>@<version>` — registry-proxy reference. `version`
    /// is `None` for the unpinned form (the resolver picks the latest
    /// compatible release at install time); `Some(req)` for the pinned form.
    Registry {
        name: String,
        version: Option<VersionReq>,
    },
}

impl InstallSource {
    /// Human-readable label for diagnostics. The format matches the input
    /// grammar so the operator sees their spec parroted back in the canonical
    /// form (whitespace stripped, `Path` etc. normalised).
    pub fn render(&self) -> String {
        match self {
            InstallSource::Path { path } => format!("path={}", path.display()),
            InstallSource::Git { url } => format!("git={url}"),
            InstallSource::Registry {
                name,
                version: None,
            } => name.clone(),
            InstallSource::Registry {
                name,
                version: Some(req),
            } => format!("{name}@{req}"),
        }
    }
}

/// Spec-parsing errors. Symbolic codes prefixed `E_INSTALL_*` so downstream
/// tooling (CI scripts, IDE wrappers) can pattern-match without scraping
/// human prose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallSpecError {
    /// Empty (or whitespace-only) spec string.
    EmptySpec,
    /// `path=` or `git=` with an empty value after the `=`.
    MissingValue { prefix: &'static str },
    /// `<name>@` with no version after the `@`.
    EmptyVersion,
    /// Version constraint did not parse as a `semver::VersionReq`.
    InvalidVersion { raw: String, message: String },
    /// Package name failed `scaffold::validate_package_name` — either the
    /// bare-name form `<name>` or the left half of `<name>@<version>`. The
    /// suggestion (when present) comes from the scaffold module's mechanical
    /// rewrite (hyphens → underscores, lowercase).
    InvalidPackageName {
        name: String,
        suggestion: Option<String>,
    },
    /// Package name matched a Kāra keyword (`fn`, `let`, ...).
    ReservedKeyword { name: String },
}

impl InstallSpecError {
    pub fn code(&self) -> &'static str {
        match self {
            InstallSpecError::EmptySpec => "E_INSTALL_EMPTY_SPEC",
            InstallSpecError::MissingValue { .. } => "E_INSTALL_MISSING_VALUE",
            InstallSpecError::EmptyVersion => "E_INSTALL_EMPTY_VERSION",
            InstallSpecError::InvalidVersion { .. } => "E_INSTALL_INVALID_VERSION",
            InstallSpecError::InvalidPackageName { .. } => "E_INSTALL_INVALID_NAME",
            InstallSpecError::ReservedKeyword { .. } => "E_INSTALL_RESERVED_NAME",
        }
    }
}

impl std::fmt::Display for InstallSpecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InstallSpecError::EmptySpec => write!(
                f,
                "install spec is empty — provide one of `path=<path>`, `git=<url>`, `<name>`, or `<name>@<version>`"
            ),
            InstallSpecError::MissingValue { prefix } => {
                write!(f, "`{prefix}=` requires a non-empty value")
            }
            InstallSpecError::EmptyVersion => write!(
                f,
                "`@` separator requires a version constraint (e.g. `my-tool@1.0`)"
            ),
            InstallSpecError::InvalidVersion { raw, message } => write!(
                f,
                "version constraint `{raw}` is not a valid semver requirement: {message}"
            ),
            InstallSpecError::InvalidPackageName { name, suggestion } => match suggestion {
                Some(s) => write!(
                    f,
                    "invalid package name `{name}` — try `{s}` (lowercase identifier matching `[a-z][a-z0-9_]*`)"
                ),
                None => write!(
                    f,
                    "invalid package name `{name}` — must match `[a-z][a-z0-9_]*`"
                ),
            },
            InstallSpecError::ReservedKeyword { name } => write!(
                f,
                "package name `{name}` is a reserved Kāra keyword — pick a different name"
            ),
        }
    }
}

impl std::error::Error for InstallSpecError {}

/// Parse a single `<bin-spec>` string. See module-level docs for the four
/// supported shapes. Leading + trailing whitespace is stripped; an empty or
/// whitespace-only spec produces `EmptySpec`.
pub fn parse_install_spec(raw: &str) -> Result<InstallSource, InstallSpecError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(InstallSpecError::EmptySpec);
    }

    if let Some(rest) = trimmed.strip_prefix("path=") {
        if rest.is_empty() {
            return Err(InstallSpecError::MissingValue { prefix: "path" });
        }
        return Ok(InstallSource::Path {
            path: PathBuf::from(rest),
        });
    }

    if let Some(rest) = trimmed.strip_prefix("git=") {
        if rest.is_empty() {
            return Err(InstallSpecError::MissingValue { prefix: "git" });
        }
        return Ok(InstallSource::Git {
            url: rest.to_string(),
        });
    }

    // Registry form. Splitting on the first `@` lets a future name vocabulary
    // that includes `@` (registry scopes — not v1) cleanly extend by switching
    // to `rsplit_once('@')` while keeping single-`@` specs back-compatible.
    if let Some((name, version_raw)) = trimmed.split_once('@') {
        validate_install_name(name)?;
        if version_raw.is_empty() {
            return Err(InstallSpecError::EmptyVersion);
        }
        let req = VersionReq::parse(version_raw).map_err(|e| InstallSpecError::InvalidVersion {
            raw: version_raw.to_string(),
            message: e.to_string(),
        })?;
        return Ok(InstallSource::Registry {
            name: name.to_string(),
            version: Some(req),
        });
    }

    validate_install_name(trimmed)?;
    Ok(InstallSource::Registry {
        name: trimmed.to_string(),
        version: None,
    })
}

/// Validate a registry-form name. Reuses `scaffold::validate_package_name` so
/// the install CLI, the manifest, and the scaffolder all agree on a single
/// vocabulary — adding a new rule in one place updates all three.
fn validate_install_name(name: &str) -> Result<(), InstallSpecError> {
    match scaffold::validate_package_name(name) {
        Ok(()) => Ok(()),
        Err(scaffold::ScaffoldError::InvalidName { value, suggestion }) => {
            Err(InstallSpecError::InvalidPackageName {
                name: value,
                suggestion,
            })
        }
        Err(scaffold::ScaffoldError::ReservedKeyword { value }) => {
            Err(InstallSpecError::ReservedKeyword { name: value })
        }
        Err(other) => {
            // `validate_package_name` only returns `InvalidName` / `ReservedKeyword`
            // today (see `src/scaffold.rs`). If a new variant appears we surface a
            // generic `InvalidPackageName` so the install CLI still produces a
            // meaningful diagnostic — the canonical message lives upstream.
            Err(InstallSpecError::InvalidPackageName {
                name: name.to_string(),
                suggestion: Some(format!("{other:?}")),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_spec_errors() {
        let err = parse_install_spec("").unwrap_err();
        assert_eq!(err.code(), "E_INSTALL_EMPTY_SPEC");
    }

    #[test]
    fn whitespace_only_spec_errors() {
        let err = parse_install_spec("   \t  ").unwrap_err();
        assert_eq!(err.code(), "E_INSTALL_EMPTY_SPEC");
    }

    #[test]
    fn leading_and_trailing_whitespace_is_trimmed() {
        let src = parse_install_spec("  my_tool  ").unwrap();
        assert_eq!(
            src,
            InstallSource::Registry {
                name: "my_tool".to_string(),
                version: None,
            }
        );
    }

    #[test]
    fn path_spec_parses() {
        let src = parse_install_spec("path=./tools/my_tool").unwrap();
        assert_eq!(
            src,
            InstallSource::Path {
                path: PathBuf::from("./tools/my_tool"),
            }
        );
    }

    #[test]
    fn path_spec_accepts_absolute_path() {
        let src = parse_install_spec("path=/usr/local/src/my_tool").unwrap();
        assert_eq!(
            src,
            InstallSource::Path {
                path: PathBuf::from("/usr/local/src/my_tool"),
            }
        );
    }

    #[test]
    fn path_spec_empty_value_errors() {
        let err = parse_install_spec("path=").unwrap_err();
        assert_eq!(err.code(), "E_INSTALL_MISSING_VALUE");
        assert!(format!("{err}").contains("`path=`"));
    }

    #[test]
    fn git_spec_parses_https_url() {
        let src = parse_install_spec("git=https://github.com/example/my_tool").unwrap();
        assert_eq!(
            src,
            InstallSource::Git {
                url: "https://github.com/example/my_tool".to_string(),
            }
        );
    }

    #[test]
    fn git_spec_parses_ssh_url() {
        let src = parse_install_spec("git=git@github.com:example/my_tool.git").unwrap();
        assert_eq!(
            src,
            InstallSource::Git {
                url: "git@github.com:example/my_tool.git".to_string(),
            }
        );
    }

    #[test]
    fn git_spec_empty_value_errors() {
        let err = parse_install_spec("git=").unwrap_err();
        assert_eq!(err.code(), "E_INSTALL_MISSING_VALUE");
        assert!(format!("{err}").contains("`git=`"));
    }

    #[test]
    fn bare_name_parses_as_registry_unpinned() {
        let src = parse_install_spec("my_tool").unwrap();
        assert_eq!(
            src,
            InstallSource::Registry {
                name: "my_tool".to_string(),
                version: None,
            }
        );
    }

    #[test]
    fn name_at_version_parses_as_registry_pinned() {
        let src = parse_install_spec("my_tool@1.2.3").unwrap();
        let InstallSource::Registry { name, version } = src else {
            panic!("expected Registry, got {src:?}");
        };
        assert_eq!(name, "my_tool");
        let req = version.expect("version should be Some(_)");
        assert!(req.matches(&semver::Version::new(1, 2, 3)));
    }

    #[test]
    fn name_at_caret_version_parses() {
        let src = parse_install_spec("my_tool@^1.0").unwrap();
        let InstallSource::Registry { version, .. } = src else {
            panic!("expected Registry");
        };
        let req = version.expect("version should be Some(_)");
        // `^1.0` accepts 1.x but not 2.0.
        assert!(req.matches(&semver::Version::new(1, 4, 0)));
        assert!(!req.matches(&semver::Version::new(2, 0, 0)));
    }

    #[test]
    fn name_at_range_version_parses() {
        let src = parse_install_spec("my_tool@>=1.0, <1.5").unwrap();
        let InstallSource::Registry { version, .. } = src else {
            panic!("expected Registry");
        };
        let req = version.expect("version should be Some(_)");
        assert!(req.matches(&semver::Version::new(1, 4, 0)));
        assert!(!req.matches(&semver::Version::new(1, 5, 0)));
    }

    #[test]
    fn name_with_trailing_at_and_no_version_errors() {
        let err = parse_install_spec("my_tool@").unwrap_err();
        assert_eq!(err.code(), "E_INSTALL_EMPTY_VERSION");
    }

    #[test]
    fn name_at_garbage_version_errors() {
        let err = parse_install_spec("my_tool@not-a-version").unwrap_err();
        assert_eq!(err.code(), "E_INSTALL_INVALID_VERSION");
        assert!(format!("{err}").contains("`not-a-version`"));
    }

    #[test]
    fn registry_name_starting_with_digit_errors() {
        let err = parse_install_spec("0tool").unwrap_err();
        assert_eq!(err.code(), "E_INSTALL_INVALID_NAME");
    }

    #[test]
    fn registry_name_with_hyphen_errors_with_underscore_suggestion() {
        let err = parse_install_spec("my-tool").unwrap_err();
        assert_eq!(err.code(), "E_INSTALL_INVALID_NAME");
        let InstallSpecError::InvalidPackageName { suggestion, .. } = &err else {
            panic!("expected InvalidPackageName, got {err:?}");
        };
        assert_eq!(suggestion.as_deref(), Some("my_tool"));
    }

    #[test]
    fn registry_name_uppercase_errors_with_lowercase_suggestion() {
        let err = parse_install_spec("MyTool").unwrap_err();
        assert_eq!(err.code(), "E_INSTALL_INVALID_NAME");
        let InstallSpecError::InvalidPackageName { suggestion, .. } = &err else {
            panic!("expected InvalidPackageName");
        };
        assert_eq!(suggestion.as_deref(), Some("mytool"));
    }

    #[test]
    fn registry_name_keyword_errors() {
        let err = parse_install_spec("fn").unwrap_err();
        assert_eq!(err.code(), "E_INSTALL_RESERVED_NAME");
    }

    #[test]
    fn pinned_name_validates_left_half() {
        // The `0tool` (registry-name-invalid) check fires before any version
        // parsing — keeps the diagnostic focused on the actual mistake.
        let err = parse_install_spec("0tool@1.0").unwrap_err();
        assert_eq!(err.code(), "E_INSTALL_INVALID_NAME");
    }

    #[test]
    fn render_round_trips_registry_unpinned() {
        let src = parse_install_spec("my_tool").unwrap();
        assert_eq!(src.render(), "my_tool");
    }

    #[test]
    fn render_round_trips_registry_pinned() {
        let src = parse_install_spec("my_tool@^1.0").unwrap();
        // VersionReq's Display is canonical (`^1.0`), so render-then-parse is
        // a fixed point modulo whitespace.
        assert_eq!(src.render(), "my_tool@^1.0");
    }

    #[test]
    fn render_round_trips_path() {
        let src = parse_install_spec("path=./tools/my_tool").unwrap();
        assert_eq!(src.render(), "path=./tools/my_tool");
    }

    #[test]
    fn render_round_trips_git() {
        let src = parse_install_spec("git=https://example.com/x.git").unwrap();
        assert_eq!(src.render(), "git=https://example.com/x.git");
    }

    #[test]
    fn error_codes_are_stable() {
        // Downstream tooling pattern-matches on these codes. Pin the
        // surface so a rename surfaces here rather than as a silent
        // CI-script breakage in the wild.
        assert_eq!(InstallSpecError::EmptySpec.code(), "E_INSTALL_EMPTY_SPEC");
        assert_eq!(
            InstallSpecError::MissingValue { prefix: "path" }.code(),
            "E_INSTALL_MISSING_VALUE"
        );
        assert_eq!(
            InstallSpecError::EmptyVersion.code(),
            "E_INSTALL_EMPTY_VERSION"
        );
        assert_eq!(
            InstallSpecError::InvalidVersion {
                raw: "x".into(),
                message: "y".into(),
            }
            .code(),
            "E_INSTALL_INVALID_VERSION"
        );
        assert_eq!(
            InstallSpecError::InvalidPackageName {
                name: "X".into(),
                suggestion: None,
            }
            .code(),
            "E_INSTALL_INVALID_NAME"
        );
        assert_eq!(
            InstallSpecError::ReservedKeyword { name: "fn".into() }.code(),
            "E_INSTALL_RESERVED_NAME"
        );
    }
}
