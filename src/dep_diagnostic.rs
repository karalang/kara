//! Dependency-resolution diagnostic renderer (slice 5 of the PubGrub
//! resolver entry, ships `docs/implementation_checklist/phase-5-diagnostics.md`
//! line 813). Spec sentence honoured: *"Conflict diagnostics show the full
//! chain (`A requires C >=1.0; B requires C >=2.0`), not a single 'no
//! solution' message."*
//!
//! Adapts `DepGraphError` (slice 3) and `ResolverError` (slice 4) into the
//! `Diagnostic { code, primary, notes, help }` shape that mirrors the
//! existing rustc-style three-piece rendering used by every lint module
//! (`unsafe_lint`, `must_use_lint`, `missing_must_use_lint`,
//! `missing_track_caller_lint` — see `cli.rs:render_*_lint_diag`). The
//! version-conflict variant emits *one note per violating parent edge* so
//! the full constraint chain shows up in the user's terminal.
//!
//! The renderer is deliberately stateless — it takes an `&ResolverError`
//! or `&DepGraphError` and returns a structured `Diagnostic`. Slice 7
//! plumbs this through `cli.rs`'s emit path so the diagnostic surfaces
//! exactly like every other karac error.

use crate::dep_graph::DepGraphError;
use crate::dep_resolver::{ResolvedSource, ResolverError};

/// Structured diagnostic ready for rustc-style emission. `code` is the
/// symbolic identifier (e.g. `E_DEPENDENCY_VERSION_CONFLICT`); `primary`
/// is the headline; `notes` carry the constraint chain (one entry per
/// violating edge) plus any structural context; `help` is the single-line
/// suggestion the user can act on. Multiple notes are necessary for the
/// version-conflict case — the spec sentence's "full chain" surface
/// requires one row per parent constraint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: &'static str,
    pub primary: String,
    pub notes: Vec<String>,
    pub help: Option<String>,
}

impl Diagnostic {
    /// Render in rustc-style three-piece form to stderr-style output. Used
    /// by tests and by slice 7's CLI wiring.
    pub fn render(&self) -> String {
        let mut out = format!("error[{}]: {}", self.code, self.primary);
        for note in &self.notes {
            out.push_str(&format!("\n   = note: {note}"));
        }
        if let Some(help) = &self.help {
            out.push_str(&format!("\n   = help: {help}"));
        }
        out
    }
}

/// Adapt a `ResolverError` (slice 4) into a structured diagnostic.
pub fn render_resolver_error(err: &ResolverError) -> Diagnostic {
    match err {
        ResolverError::RegistryDepUnsupported {
            package,
            declared_in,
        } => Diagnostic {
            code: err.code(),
            primary: format!(
                "registry dependency `{}` cannot be resolved yet",
                package
            ),
            notes: vec![format!(
                "declared in `{}/kara.toml`",
                declared_in.display()
            )],
            help: Some(
                "registry / proxy fetch ships in a sibling slice (tracker line 819 — `Registry proxy client`); until then, switch to a `path = \"...\"` dependency".to_string()
            ),
        },
        ResolverError::GitDepUnsupported {
            package,
            declared_in,
        } => Diagnostic {
            code: err.code(),
            primary: format!(
                "git dependency `{}` cannot be resolved yet",
                package
            ),
            notes: vec![format!(
                "declared in `{}/kara.toml`",
                declared_in.display()
            )],
            help: Some(
                "git fetch ships alongside registry-proxy fetch (tracker line 819); until then, switch to a `path = \"...\"` dependency that points at a local checkout".to_string()
            ),
        },
        ResolverError::VersionConflict {
            package,
            candidate,
            chain,
        } => {
            // One note per violating parent so the user sees the full
            // constraint chain rather than a single collapsed message.
            // This is the spec-promised surface — the "A requires C >=1.0;
            // B requires C >=2.0" shape lives here.
            let notes = chain
                .iter()
                .map(|link| {
                    format!(
                        "`{}/kara.toml` requires `{} = \"{}\"`",
                        link.parent_dir.display(),
                        package,
                        link.req,
                    )
                })
                .collect();
            Diagnostic {
                code: err.code(),
                primary: format!(
                    "no version of `{}` satisfies all declared constraints (candidate `{}`)",
                    package, candidate,
                ),
                notes,
                help: Some(format!(
                    "every parent's constraint must overlap; relax one of the requirements or pin `{}` to a version compatible with both",
                    package,
                )),
            }
        }
        ResolverError::SourceConflict {
            package,
            first_source,
            second_source,
        } => Diagnostic {
            code: err.code(),
            primary: format!(
                "`{}` is declared from two incompatible sources",
                package
            ),
            notes: vec![
                format!("first source: {}", describe_source(first_source)),
                format!("second source: {}", describe_source(second_source)),
            ],
            help: Some(
                "pick one source (typically the path or registry entry closest to your project) and remove the other from the dependency graph"
                    .to_string(),
            ),
        },
        ResolverError::ToolchainTooOld {
            package,
            manifest_dir,
            kara_version_req,
            active_version,
        } => Diagnostic {
            code: err.code(),
            primary: format!(
                "package `{}` requires a newer toolchain than the one currently in use",
                package,
            ),
            notes: vec![
                format!(
                    "`{}/kara.toml` declares `kara-version = \"{}\"`",
                    manifest_dir.display(),
                    kara_version_req,
                ),
                format!("active toolchain: `{}`", active_version),
            ],
            help: Some(
                "upgrade the karac toolchain (`karaup` when it ships, or rebuild from source) — or relax the package's `[package].kara-version` constraint if the version pin is overly tight"
                    .to_string(),
            ),
        },
    }
}

/// Adapt a `DepGraphError` (slice 3) into a structured diagnostic. Same
/// rustc-style shape as the resolver-error renderer so a single slice-7
/// dispatcher can format either error type uniformly.
pub fn render_dep_graph_error(err: &DepGraphError) -> Diagnostic {
    match err {
        DepGraphError::WorkspaceDepNotDeclared {
            manifest_dir,
            dep_name,
        } => Diagnostic {
            code: err.code(),
            primary: format!(
                "`{}` uses `workspace = true` but `{}` is not declared at the workspace level",
                dep_name, dep_name,
            ),
            notes: vec![format!(
                "declared in `{}/kara.toml`",
                manifest_dir.display()
            )],
            help: Some(format!(
                "add `{} = \"<version>\"` to the workspace root's `[workspace.dependencies]` table",
                dep_name,
            )),
        },
        DepGraphError::WorkspaceDepOutsideWorkspace {
            manifest_dir,
            dep_name,
        } => Diagnostic {
            code: err.code(),
            primary: format!(
                "`{}` uses `workspace = true` but the entry-point manifest has no `[workspace.dependencies]` table",
                dep_name,
            ),
            notes: vec![format!(
                "declared in `{}/kara.toml`",
                manifest_dir.display()
            )],
            help: Some(
                "either declare `[workspace.dependencies]` on the entry-point manifest or replace `workspace = true` with a direct version / path / git source"
                    .to_string(),
            ),
        },
        DepGraphError::DependencyCycle { chain } => {
            let chain_str = chain
                .iter()
                .map(|p| format!("`{}`", p.display()))
                .collect::<Vec<_>>()
                .join(" → ");
            Diagnostic {
                code: err.code(),
                primary: "dependency cycle detected in path dependencies".to_string(),
                notes: vec![format!("cycle: {}", chain_str)],
                help: Some(
                    "remove the back-edge by either replacing one of the path dependencies with a published version or restructuring the package boundary"
                        .to_string(),
                ),
            }
        }
        DepGraphError::PathDepNotFound {
            from_dir,
            dep_name,
            target,
        } => Diagnostic {
            code: err.code(),
            primary: format!(
                "path dependency `{}` points at a directory with no `kara.toml`",
                dep_name
            ),
            notes: vec![
                format!("declared in `{}/kara.toml`", from_dir.display()),
                format!("expected `kara.toml` at `{}`", target.display()),
            ],
            help: Some(format!(
                "check the relative path or create `{}/kara.toml`",
                target.display(),
            )),
        },
        DepGraphError::OfflineVendorEntryMissing {
            from_dir,
            dep_name,
            expected,
        } => Diagnostic {
            code: err.code(),
            primary: format!(
                "offline build is missing the vendored copy of dependency `{}`",
                dep_name
            ),
            notes: vec![
                format!("declared in `{}/kara.toml`", from_dir.display()),
                format!("expected `kara.toml` at `{}`", expected.display()),
                "offline mode resolves every transitive path-dep against `./vendor/<name>/`".to_string(),
            ],
            help: Some(
                "run `karac vendor` to populate `./vendor/` from the current resolution, then re-run with `--offline`"
                    .to_string(),
            ),
        },
        DepGraphError::PathDepManifestInvalid {
            from_dir,
            dep_name,
            source,
        } => Diagnostic {
            code: err.code(),
            primary: format!(
                "path dependency `{}`'s `kara.toml` failed to parse",
                dep_name
            ),
            notes: vec![
                format!("declared in `{}/kara.toml`", from_dir.display()),
                format!("underlying error: {}", source),
            ],
            help: Some(
                "fix the parse error reported by the underlying manifest diagnostic"
                    .to_string(),
            ),
        },
        DepGraphError::RegistryFetchFailed {
            from_dir,
            dep_name,
            message,
        } => Diagnostic {
            code: err.code(),
            primary: format!("could not fetch registry dependency `{}`", dep_name),
            notes: vec![
                format!("declared in `{}/kara.toml`", from_dir.display()),
                format!("underlying error: {}", message),
            ],
            help: Some(
                "check the registry proxy URL and your network connection, or pin a version that the proxy publishes"
                    .to_string(),
            ),
        },
    }
}

fn describe_source(source: &ResolvedSource) -> String {
    match source {
        ResolvedSource::Root => "the entry-point project (`Root`)".to_string(),
        ResolvedSource::Path(dir) => format!("path `{}`", dir.display()),
        ResolvedSource::Registry { url, .. } => format!("registry `{url}`"),
        ResolvedSource::Git { url, reference } => match reference {
            Some(r) => format!("git `{url}` (ref `{r:?}`)"),
            None => format!("git `{url}`"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dep_resolver::ConstraintLink;
    use semver::{Version, VersionReq};
    use std::path::PathBuf;

    #[test]
    fn registry_dep_unsupported_renders_with_help() {
        let err = ResolverError::RegistryDepUnsupported {
            package: "http".into(),
            declared_in: PathBuf::from("/proj"),
        };
        let diag = render_resolver_error(&err);
        assert_eq!(diag.code, "E_REGISTRY_DEP_UNSUPPORTED");
        assert!(diag.primary.contains("http"));
        assert!(diag.notes.iter().any(|n| n.contains("/proj")));
        assert!(diag.help.as_ref().unwrap().contains("tracker line 819"));
    }

    #[test]
    fn git_dep_unsupported_renders_with_help() {
        let err = ResolverError::GitDepUnsupported {
            package: "json".into(),
            declared_in: PathBuf::from("/proj"),
        };
        let diag = render_resolver_error(&err);
        assert_eq!(diag.code, "E_GIT_DEP_UNSUPPORTED");
        assert!(diag.primary.contains("json"));
    }

    #[test]
    fn version_conflict_renders_one_note_per_violating_edge() {
        // The spec-promised surface: "A requires C >=1.0; B requires C >=2.0"
        // — each parent gets its own note, not a collapsed message.
        let err = ResolverError::VersionConflict {
            package: "C".into(),
            candidate: Version::parse("0.0.0").unwrap(),
            chain: vec![
                ConstraintLink {
                    parent_dir: PathBuf::from("/proj/a"),
                    req: VersionReq::parse(">=1.0").unwrap(),
                },
                ConstraintLink {
                    parent_dir: PathBuf::from("/proj/b"),
                    req: VersionReq::parse(">=2.0").unwrap(),
                },
            ],
        };
        let diag = render_resolver_error(&err);
        assert_eq!(diag.code, "E_DEPENDENCY_VERSION_CONFLICT");
        assert_eq!(diag.notes.len(), 2, "one note per violating parent");
        assert!(diag.notes[0].contains("/proj/a"));
        assert!(diag.notes[0].contains(">=1.0"));
        assert!(diag.notes[1].contains("/proj/b"));
        assert!(diag.notes[1].contains(">=2.0"));
        assert!(diag.help.is_some());
    }

    #[test]
    fn version_conflict_render_string_contains_full_chain() {
        let err = ResolverError::VersionConflict {
            package: "C".into(),
            candidate: Version::parse("0.0.0").unwrap(),
            chain: vec![
                ConstraintLink {
                    parent_dir: PathBuf::from("A"),
                    req: VersionReq::parse(">=1.0").unwrap(),
                },
                ConstraintLink {
                    parent_dir: PathBuf::from("B"),
                    req: VersionReq::parse(">=2.0").unwrap(),
                },
            ],
        };
        let rendered = render_resolver_error(&err).render();
        // Both constraint chains should appear in the final rendered string.
        assert!(rendered.contains("A/kara.toml"), "{rendered}");
        assert!(rendered.contains("B/kara.toml"), "{rendered}");
        assert!(rendered.contains(">=1.0"));
        assert!(rendered.contains(">=2.0"));
    }

    #[test]
    fn source_conflict_renders_both_origins() {
        let err = ResolverError::SourceConflict {
            package: "shared".into(),
            first_source: ResolvedSource::Path(PathBuf::from("/local/a")),
            second_source: ResolvedSource::Registry {
                url: "https://registry/x".into(),
                dir: PathBuf::from("/cache/x"),
            },
        };
        let diag = render_resolver_error(&err);
        assert_eq!(diag.code, "E_DEPENDENCY_SOURCE_CONFLICT");
        assert_eq!(diag.notes.len(), 2);
        assert!(diag.notes[0].contains("/local/a"));
        assert!(diag.notes[1].contains("https://registry/x"));
    }

    #[test]
    fn dep_graph_workspace_not_declared_renders() {
        let err = DepGraphError::WorkspaceDepNotDeclared {
            manifest_dir: PathBuf::from("/proj"),
            dep_name: "http".into(),
        };
        let diag = render_dep_graph_error(&err);
        assert_eq!(diag.code, "E_WORKSPACE_DEP_NOT_DECLARED");
        assert!(diag.primary.contains("http"));
        assert!(diag.help.is_some());
    }

    #[test]
    fn dep_graph_workspace_outside_workspace_renders() {
        let err = DepGraphError::WorkspaceDepOutsideWorkspace {
            manifest_dir: PathBuf::from("/proj"),
            dep_name: "http".into(),
        };
        let diag = render_dep_graph_error(&err);
        assert_eq!(diag.code, "E_WORKSPACE_DEP_OUTSIDE_WORKSPACE");
    }

    #[test]
    fn dep_graph_cycle_renders_chain_with_arrows() {
        let err = DepGraphError::DependencyCycle {
            chain: vec![
                PathBuf::from("/root"),
                PathBuf::from("/root/a"),
                PathBuf::from("/root"),
            ],
        };
        let diag = render_dep_graph_error(&err);
        assert_eq!(diag.code, "E_DEPENDENCY_CYCLE");
        let note = &diag.notes[0];
        assert!(
            note.contains("→"),
            "chain should use arrow separator: {note}"
        );
        assert!(note.contains("/root"));
        assert!(note.contains("/root/a"));
    }

    #[test]
    fn dep_graph_path_not_found_renders() {
        let err = DepGraphError::PathDepNotFound {
            from_dir: PathBuf::from("/proj"),
            dep_name: "missing".into(),
            target: PathBuf::from("/proj/missing"),
        };
        let diag = render_dep_graph_error(&err);
        assert_eq!(diag.code, "E_PATH_DEP_NOT_FOUND");
        assert!(diag.notes.iter().any(|n| n.contains("/proj/missing")));
    }

    #[test]
    fn dep_graph_path_manifest_invalid_renders() {
        let err = DepGraphError::PathDepManifestInvalid {
            from_dir: PathBuf::from("/proj"),
            dep_name: "broken".into(),
            source: Box::new(crate::manifest::ManifestError::MissingPackageName {
                path: PathBuf::from("/proj/broken/kara.toml"),
            }),
        };
        let diag = render_dep_graph_error(&err);
        assert_eq!(diag.code, "E_PATH_DEP_MANIFEST_INVALID");
        assert!(diag.notes.iter().any(|n| n.contains("underlying error")));
    }

    #[test]
    fn toolchain_too_old_renders_with_upgrade_guidance() {
        let err = ResolverError::ToolchainTooOld {
            package: "modernlib".into(),
            manifest_dir: PathBuf::from("/proj/modernlib"),
            kara_version_req: VersionReq::parse(">=2.0").unwrap(),
            active_version: Version::parse("1.0.0").unwrap(),
        };
        let diag = render_resolver_error(&err);
        assert_eq!(diag.code, "E_TOOLCHAIN_TOO_OLD");
        assert!(diag.primary.contains("modernlib"));
        // The two notes name the declared constraint and the active version.
        assert!(diag.notes.iter().any(|n| n.contains(">=2.0")));
        assert!(diag.notes.iter().any(|n| n.contains("active toolchain")));
        assert!(diag.notes.iter().any(|n| n.contains("1.0.0")));
        // Help line mentions both upgrade paths so the user has a choice.
        let help = diag.help.as_ref().unwrap();
        assert!(help.contains("upgrade") || help.contains("relax"));
    }

    #[test]
    fn diagnostic_render_format_matches_rustc_style() {
        let diag = Diagnostic {
            code: "E_TEST",
            primary: "primary message".to_string(),
            notes: vec!["note one".to_string(), "note two".to_string()],
            help: Some("the help".to_string()),
        };
        let rendered = diag.render();
        // Three-piece rustc-style: header line, indented note lines, indented help.
        assert!(rendered.starts_with("error[E_TEST]: primary message"));
        assert!(rendered.contains("\n   = note: note one"));
        assert!(rendered.contains("\n   = note: note two"));
        assert!(rendered.contains("\n   = help: the help"));
    }

    #[test]
    fn diagnostic_without_help_renders_without_help_line() {
        let diag = Diagnostic {
            code: "E_TEST",
            primary: "msg".to_string(),
            notes: Vec::new(),
            help: None,
        };
        let rendered = diag.render();
        assert!(!rendered.contains("help:"));
    }
}
