//! Dependency resolution (slice 4 of the PubGrub resolver entry, ships
//! `docs/implementation_checklist/phase-5-diagnostics.md` line 813).
//!
//! Consumes a `DepGraph` from slice 3 and produces a `Resolution` mapping
//! each reachable package to a concrete version, or a structured
//! `ResolverError` that slice 5's diagnostic renderer formats into the
//! "full constraint chain" shape the spec sentence promises.
//!
//! **Scope note.** Path-deps dominate the v1.1 surface: a path-dep has
//! exactly one candidate (its on-disk manifest), so the resolver is a
//! topological walk that pins each package to a sentinel version and
//! validates parent-declared version constraints against the candidate.
//! Registry and git deps require the registry-proxy fetch surface (tracker
//! line 819) before there's a multi-candidate enumeration for PubGrub to
//! choose among — until that lands they surface as
//! `E_REGISTRY_DEP_UNSUPPORTED` / `E_GIT_DEP_UNSUPPORTED` so the user sees
//! a clear "this dep source isn't reachable yet" diagnostic instead of a
//! silent skip. The PubGrub algorithm itself is reserved for the
//! multi-candidate case; for path-only graphs it would solve a degenerate
//! instance, so the v1.1 surface ships the topological walk and leaves the
//! pubgrub-crate integration as the carve-out at slice 7's flip.

use crate::dep_graph::DepGraph;
use crate::manifest::{DependencySpec, GitRef};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Sentinel version assigned to path-deps. Real `[package].version` strings
/// in `kara.toml` aren't structurally captured by the manifest parser
/// today (they're a soft passthrough field — see manifest.rs's "version
/// and authors parse silently" rule), so every path-dep gets the same
/// placeholder. Plain `0.0.0` (no pre-release suffix) so it matches the
/// `*` wildcard — Cargo-style semver excludes pre-releases unless a
/// constraint explicitly opts in, which would surprise users who write
/// `version = "*"` on a path dep. When registry-fetch ships, each package
/// will carry the version its catalog entry advertises and this sentinel
/// falls away.
pub const PATH_DEP_SENTINEL_VERSION: &str = "0.0.0";

/// Concrete dependency-graph resolution: every reachable package mapped
/// to a chosen version + its origin.
#[derive(Debug, Clone)]
pub struct Resolution {
    pub packages: BTreeMap<String, ResolvedPackage>,
}

/// One resolved package's pinned version + the source the resolver picked
/// it from. Slice 7's CLI integration consumes this to drive the module
/// loader; slice 5's diagnostic renderer reads it when echoing the
/// resolution back to the user.
#[derive(Debug, Clone)]
pub struct ResolvedPackage {
    pub name: String,
    pub version: semver::Version,
    pub source: ResolvedSource,
    /// Every parent that declared this dep, with the version constraint it
    /// imposed. Used by the diagnostic renderer (slice 5) to attribute
    /// version-conflict diagnostics to specific dep edges.
    pub declared_by: Vec<DeclarationEdge>,
}

/// Where a resolved package came from. `Root` is the entry-point project
/// itself; the rest mirror `DependencySpec` variants once a concrete
/// version has been pinned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedSource {
    Root,
    Path(PathBuf),
    /// Reserved for slice-7 wiring: a resolved registry dep would carry
    /// the URL of the catalog entry, but registry fetch is line 819
    /// territory and slice 4 errors before constructing this variant.
    /// Listed here so the `Resolution` shape is forward-compatible.
    Registry {
        url: String,
    },
    /// Reserved for slice-7 wiring (see `Registry` carve-out).
    Git {
        url: String,
        reference: Option<GitRef>,
    },
}

/// One edge in the resolution's dependency forest: which parent manifest
/// declared this dep and with what version constraint.
#[derive(Debug, Clone)]
pub struct DeclarationEdge {
    pub parent: String,
    pub req: Option<semver::VersionReq>,
}

#[derive(Debug)]
pub enum ResolverError {
    /// A registry dep is present in the graph but the registry-proxy fetch
    /// surface (line 819) isn't shipped. Maps to `E_REGISTRY_DEP_UNSUPPORTED`.
    RegistryDepUnsupported {
        package: String,
        declared_in: PathBuf,
    },
    /// A git dep is present in the graph but git fetch isn't shipped. Maps
    /// to `E_GIT_DEP_UNSUPPORTED`.
    GitDepUnsupported {
        package: String,
        declared_in: PathBuf,
    },
    /// A parent's declared version constraint excludes the candidate
    /// version of a resolved dep. Carries the full constraint chain so
    /// slice 5's renderer can surface "A requires C >=1.0; B requires C
    /// >=2.0" diagnostics. Maps to `E_DEPENDENCY_VERSION_CONFLICT`.
    VersionConflict {
        package: String,
        candidate: semver::Version,
        chain: Vec<ConstraintLink>,
    },
    /// The same package name is declared from two incompatible sources
    /// (e.g. path + registry) inside the graph. Maps to
    /// `E_DEPENDENCY_SOURCE_CONFLICT`.
    SourceConflict {
        package: String,
        first_source: ResolvedSource,
        second_source: ResolvedSource,
    },
    /// A package in the resolved graph declares `[package].kara-version =
    /// "..."` whose constraint excludes the active compiler version. Maps
    /// to `E_TOOLCHAIN_TOO_OLD`. Closes the deferred sub-bullet at line
    /// 842 of `docs/implementation_checklist/phase-5-diagnostics.md`.
    ToolchainTooOld {
        package: String,
        manifest_dir: PathBuf,
        kara_version_req: semver::VersionReq,
        active_version: semver::Version,
    },
}

/// One link in a version-conflict constraint chain: the parent manifest's
/// directory + the constraint it declared on the offending dep.
#[derive(Debug, Clone)]
pub struct ConstraintLink {
    pub parent_dir: PathBuf,
    pub req: semver::VersionReq,
}

impl ResolverError {
    /// Symbolic diagnostic code for slice 5's dispatch table.
    pub fn code(&self) -> &'static str {
        match self {
            Self::RegistryDepUnsupported { .. } => "E_REGISTRY_DEP_UNSUPPORTED",
            Self::GitDepUnsupported { .. } => "E_GIT_DEP_UNSUPPORTED",
            Self::VersionConflict { .. } => "E_DEPENDENCY_VERSION_CONFLICT",
            Self::SourceConflict { .. } => "E_DEPENDENCY_SOURCE_CONFLICT",
            Self::ToolchainTooOld { .. } => "E_TOOLCHAIN_TOO_OLD",
        }
    }
}

impl std::fmt::Display for ResolverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RegistryDepUnsupported {
                package,
                declared_in,
            } => write!(
                f,
                "registry dependency `{}` (declared in `{}/kara.toml`) cannot be resolved — the registry-proxy fetch surface is a v1.1 follow-up",
                package,
                declared_in.display(),
            ),
            Self::GitDepUnsupported {
                package,
                declared_in,
            } => write!(
                f,
                "git dependency `{}` (declared in `{}/kara.toml`) cannot be resolved — git fetch is a v1.1 follow-up",
                package,
                declared_in.display(),
            ),
            Self::VersionConflict {
                package,
                candidate,
                chain,
            } => {
                write!(f, "dependency `{package}` has no version satisfying all declared constraints (candidate `{candidate}`):")?;
                for link in chain {
                    write!(
                        f,
                        " `{}/kara.toml` requires `{}`;",
                        link.parent_dir.display(),
                        link.req,
                    )?;
                }
                Ok(())
            }
            Self::SourceConflict {
                package,
                first_source,
                second_source,
            } => write!(
                f,
                "dependency `{package}` is declared from incompatible sources: {first_source:?} and {second_source:?}",
            ),
            Self::ToolchainTooOld {
                package,
                manifest_dir,
                kara_version_req,
                active_version,
            } => write!(
                f,
                "package `{}` at `{}/kara.toml` requires kara-version `{}` but the active toolchain is `{}`",
                package,
                manifest_dir.display(),
                kara_version_req,
                active_version,
            ),
        }
    }
}

/// The active compiler version. Used as the right-hand operand in every
/// `[package].kara-version` constraint comparison during MSRV enforcement
/// (slice 6). Sourced from `CARGO_PKG_VERSION` at compile time; tests pass
/// a fixed `Version` so they're hermetic against the running compiler's
/// version number.
pub fn active_toolchain_version() -> semver::Version {
    semver::Version::parse(env!("CARGO_PKG_VERSION"))
        .expect("CARGO_PKG_VERSION must be a valid semver string")
}

/// Resolve the dependency graph into a concrete `Resolution`. Path-deps
/// dominate today's v1.1 surface; registry/git deps surface as the
/// matching `*_DEP_UNSUPPORTED` error until fetch ships. The function is
/// deterministic — `BTreeMap` iteration order in the input graph drives
/// the output ordering, so diagnostic output is stable across runs.
///
/// `active_toolchain` is the version every reachable manifest's
/// `[package].kara-version` constraint is checked against (slice 6 — MSRV
/// enforcement). A package whose constraint excludes the active version
/// surfaces as `ResolverError::ToolchainTooOld`. Production callers pass
/// `active_toolchain_version()`; tests pass a fixed `Version` so they
/// stay hermetic.
pub fn resolve(
    graph: &DepGraph,
    active_toolchain: &semver::Version,
) -> Result<Resolution, Box<ResolverError>> {
    resolve_with_offline(graph, active_toolchain, None)
}

/// Offline-aware variant of [`resolve`]. When `offline_root` is
/// `Some(vendor_root)`, every transitive path-dep target is rewritten to
/// `vendor_root.join(dep_name)` — matching the redirect performed by
/// [`crate::dep_graph::build_dep_graph_with_offline`] so the resolver looks
/// up the same vendored manifest the graph walk loaded. The root manifest
/// itself is unaffected; only its children's path targets are rewritten.
pub fn resolve_with_offline(
    graph: &DepGraph,
    active_toolchain: &semver::Version,
    offline_root: Option<&std::path::Path>,
) -> Result<Resolution, Box<ResolverError>> {
    // MSRV check runs first — a package whose kara-version excludes the
    // active toolchain can't be built regardless of how the dep graph
    // resolves. Surfacing the toolchain mismatch up front gives a clearer
    // diagnostic than "version conflict" would, since the failure isn't
    // a constraint clash among parents but a compiler-vs-package gap.
    // Walks in deterministic order so multiple violations would report
    // the alphabetically-first one (per-manifest BTreeMap iteration).
    for (manifest_dir, mf) in &graph.manifests {
        let Some(req) = &mf.kara_version else {
            continue;
        };
        if !req.matches(active_toolchain) {
            return Err(Box::new(ResolverError::ToolchainTooOld {
                package: mf.name.clone(),
                manifest_dir: manifest_dir.clone(),
                kara_version_req: req.clone(),
                active_version: active_toolchain.clone(),
            }));
        }
    }

    let mut packages: BTreeMap<String, ResolvedPackage> = BTreeMap::new();
    let sentinel = semver::Version::parse(PATH_DEP_SENTINEL_VERSION)
        .expect("PATH_DEP_SENTINEL_VERSION must parse");

    // Pin the root package first so its slot in the resolution is anchored.
    let root_manifest = graph
        .manifests
        .get(&graph.root_dir)
        .expect("root manifest present in graph");
    let root_name = root_manifest.name.clone();
    packages.insert(
        root_name.clone(),
        ResolvedPackage {
            name: root_name.clone(),
            version: sentinel.clone(),
            source: ResolvedSource::Root,
            declared_by: Vec::new(),
        },
    );

    // Walk every manifest in the graph in deterministic order; for each
    // dependency entry, either resolve it (path-deps) or error out
    // (registry/git deps not yet supported). The walk is per-manifest
    // rather than per-edge because each manifest's path-dep pulls in a
    // specific directory whose name might differ from the dep entry's
    // local name — the dep name in `[dependencies]` becomes the resolved
    // package's identity.
    for (manifest_dir, derived) in &graph.derived_deps {
        let parent_name = graph
            .manifests
            .get(manifest_dir)
            .map(|m| m.name.clone())
            .unwrap_or_else(|| manifest_dir.display().to_string());

        for (dep_name, spec) in derived {
            match spec {
                DependencySpec::Path { path, version } => {
                    let target_dir = match offline_root {
                        Some(vendor_root) => std::fs::canonicalize(vendor_root.join(dep_name))
                            .unwrap_or_else(|_| vendor_root.join(dep_name)),
                        None => resolve_path_dep_target(manifest_dir, path),
                    };
                    let child_manifest = graph.manifests.get(&target_dir).ok_or_else(|| {
                        // This shouldn't happen — slice 3's walk
                        // ensures every path target is present in
                        // graph.manifests. Surface as a source
                        // conflict with a clear message so the user
                        // hears about it instead of a silent miss.
                        Box::new(ResolverError::SourceConflict {
                            package: dep_name.clone(),
                            first_source: ResolvedSource::Path(manifest_dir.clone()),
                            second_source: ResolvedSource::Path(target_dir.clone()),
                        })
                    })?;
                    let resolved_name = child_manifest.name.clone();
                    upsert_path(
                        &mut packages,
                        resolved_name,
                        target_dir,
                        sentinel.clone(),
                        DeclarationEdge {
                            parent: parent_name.clone(),
                            req: version.clone(),
                        },
                    )?;
                }
                DependencySpec::Registry { .. } => {
                    return Err(Box::new(ResolverError::RegistryDepUnsupported {
                        package: dep_name.clone(),
                        declared_in: manifest_dir.clone(),
                    }));
                }
                DependencySpec::Git { .. } => {
                    return Err(Box::new(ResolverError::GitDepUnsupported {
                        package: dep_name.clone(),
                        declared_in: manifest_dir.clone(),
                    }));
                }
                DependencySpec::Workspace => {
                    // Slice 3 derefs every Workspace variant before
                    // populating derived_deps. Surface a clear panic if a
                    // future change leaks one through — the input
                    // invariant is load-bearing.
                    unreachable!(
                        "DependencySpec::Workspace must be dereferenced by slice 3 before resolution"
                    );
                }
            }
        }
    }

    // Validate constraint chains: for each resolved package, intersect
    // every declared version requirement against the candidate version.
    // A path-dep's sentinel version (`0.0.0-path`) only matches `*` and
    // `>=0.0.0` style constraints — if a parent declared a real semver
    // pin like `>=1.0`, slice 4 surfaces a VersionConflict so the user
    // hears about it. (When fetch ships, the candidate set widens; pubgrub
    // crate integration will replace this hand-rolled intersection.)
    for resolved in packages.values() {
        let mut violating = Vec::new();
        for edge in &resolved.declared_by {
            let Some(req) = &edge.req else { continue };
            if !req.matches(&resolved.version) {
                violating.push(ConstraintLink {
                    parent_dir: PathBuf::from(&edge.parent),
                    req: req.clone(),
                });
            }
        }
        if !violating.is_empty() {
            return Err(Box::new(ResolverError::VersionConflict {
                package: resolved.name.clone(),
                candidate: resolved.version.clone(),
                chain: violating,
            }));
        }
    }

    Ok(Resolution { packages })
}

fn resolve_path_dep_target(from_dir: &std::path::Path, path: &std::path::Path) -> PathBuf {
    let raw = if path.is_absolute() {
        path.to_path_buf()
    } else {
        from_dir.join(path)
    };
    std::fs::canonicalize(&raw).unwrap_or(raw)
}

fn upsert_path(
    packages: &mut BTreeMap<String, ResolvedPackage>,
    name: String,
    dir: PathBuf,
    version: semver::Version,
    edge: DeclarationEdge,
) -> Result<(), Box<ResolverError>> {
    if let Some(existing) = packages.get_mut(&name) {
        match &existing.source {
            ResolvedSource::Path(existing_dir) if existing_dir == &dir => {
                existing.declared_by.push(edge);
                Ok(())
            }
            ResolvedSource::Root => Err(Box::new(ResolverError::SourceConflict {
                package: name.clone(),
                first_source: ResolvedSource::Root,
                second_source: ResolvedSource::Path(dir),
            })),
            other => Err(Box::new(ResolverError::SourceConflict {
                package: name.clone(),
                first_source: other.clone(),
                second_source: ResolvedSource::Path(dir),
            })),
        }
    } else {
        packages.insert(
            name.clone(),
            ResolvedPackage {
                name,
                version,
                source: ResolvedSource::Path(dir),
                declared_by: vec![edge],
            },
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dep_graph::{build_dep_graph, ManifestLoader};
    use crate::manifest::{self, DependencySpec, Manifest};
    use semver::VersionReq;
    use std::path::Path;

    struct MemLoader {
        manifests: BTreeMap<PathBuf, Manifest>,
    }

    impl ManifestLoader for MemLoader {
        fn load(&self, dir: &Path) -> Result<Manifest, manifest::ManifestError> {
            self.manifests.get(dir).cloned().ok_or_else(|| {
                manifest::ManifestError::NotInsideKaraProject {
                    searched_from: dir.to_path_buf(),
                }
            })
        }
        fn manifest_exists(&self, dir: &Path) -> bool {
            self.manifests.contains_key(dir)
        }
    }

    fn empty_manifest(name: &str) -> Manifest {
        Manifest {
            name: name.to_string(),
            edition: "2026".to_string(),
            profile: manifest::CompileProfile::Default,
            test_resources: BTreeMap::new(),
            test_timeout_seconds: None,
            kara_version: None,
            dependencies: BTreeMap::new(),
            dev_dependencies: BTreeMap::new(),
            workspace_dependencies: BTreeMap::new(),
            target_dependencies: BTreeMap::new(),
            target_dev_dependencies: BTreeMap::new(),
            target_profile_overrides: BTreeMap::new(),
            build_default_target: None,
            build_targets: Vec::new(),
            build_registry_proxy: None,
            lints: manifest::ManifestLints::default(),
            release_target_cpu: None,
            release_target_features: None,
            toolchain_wasm_tools: None,
            wasm_pool_size: None,
            wasm_fallback: None,
            wasm_max_memory_pages: None,
            profile_config: manifest::ProfileConfig::default(),
            link_libs: Vec::new(),
            link_search_paths: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn registry(s: &str) -> DependencySpec {
        DependencySpec::Registry {
            version: VersionReq::parse(s).unwrap(),
        }
    }

    fn path_dep(target: &str) -> DependencySpec {
        DependencySpec::Path {
            path: PathBuf::from(target),
            version: None,
        }
    }

    /// Hermetic active-toolchain version for tests. Distinct from
    /// `active_toolchain_version()` (which reads `CARGO_PKG_VERSION`) so
    /// tests stay stable across compiler-version bumps.
    fn test_version() -> semver::Version {
        semver::Version::parse("1.0.0").unwrap()
    }

    #[test]
    fn solo_manifest_resolves_to_root_only() {
        let root = empty_manifest("solo");
        let graph = build_dep_graph(
            &PathBuf::from("/solo"),
            root,
            &MemLoader {
                manifests: BTreeMap::new(),
            },
        )
        .expect("graph");
        let resolution = resolve(&graph, &test_version()).expect("resolve");
        assert_eq!(resolution.packages.len(), 1);
        let solo = &resolution.packages["solo"];
        assert_eq!(solo.source, ResolvedSource::Root);
    }

    #[test]
    fn path_dep_resolves_to_target_dir() {
        let mut root = empty_manifest("root");
        root.dependencies.insert("a".into(), path_dep("a"));
        let a = empty_manifest("a_pkg");
        let graph = build_dep_graph(
            &PathBuf::from("/root"),
            root,
            &MemLoader {
                manifests: BTreeMap::from([(PathBuf::from("/root/a"), a)]),
            },
        )
        .expect("graph");
        let resolution = resolve(&graph, &test_version()).expect("resolve");
        assert_eq!(resolution.packages.len(), 2);
        // The path-dep is identified by the child manifest's [package].name,
        // not the parent's local entry name.
        let a_pkg = &resolution.packages["a_pkg"];
        assert!(matches!(a_pkg.source, ResolvedSource::Path(_)));
        assert_eq!(a_pkg.version.to_string(), PATH_DEP_SENTINEL_VERSION);
    }

    #[test]
    fn registry_dep_surfaces_unsupported_error() {
        let mut root = empty_manifest("root");
        root.dependencies.insert("http".into(), registry("1.0"));
        let graph = build_dep_graph(
            &PathBuf::from("/root"),
            root,
            &MemLoader {
                manifests: BTreeMap::new(),
            },
        )
        .expect("graph");
        let err = resolve(&graph, &test_version()).unwrap_err();
        assert_eq!(err.code(), "E_REGISTRY_DEP_UNSUPPORTED");
        match *err {
            ResolverError::RegistryDepUnsupported { package, .. } => {
                assert_eq!(package, "http");
            }
            other => panic!("expected RegistryDepUnsupported, got {other:?}"),
        }
    }

    #[test]
    fn git_dep_surfaces_unsupported_error() {
        let mut root = empty_manifest("root");
        root.dependencies.insert(
            "json".into(),
            DependencySpec::Git {
                url: "https://example.com/json".into(),
                reference: None,
                version: None,
            },
        );
        let graph = build_dep_graph(
            &PathBuf::from("/root"),
            root,
            &MemLoader {
                manifests: BTreeMap::new(),
            },
        )
        .expect("graph");
        let err = resolve(&graph, &test_version()).unwrap_err();
        assert_eq!(err.code(), "E_GIT_DEP_UNSUPPORTED");
    }

    #[test]
    fn path_dep_with_strict_version_constraint_conflicts() {
        // Parent declares `>=1.0` but the path-dep sentinel is `0.0.0-path`,
        // which doesn't match — VersionConflict surfaces with the chain.
        let mut root = empty_manifest("root");
        root.dependencies.insert(
            "a".into(),
            DependencySpec::Path {
                path: PathBuf::from("a"),
                version: Some(VersionReq::parse(">=1.0").unwrap()),
            },
        );
        let a = empty_manifest("a_pkg");
        let graph = build_dep_graph(
            &PathBuf::from("/root"),
            root,
            &MemLoader {
                manifests: BTreeMap::from([(PathBuf::from("/root/a"), a)]),
            },
        )
        .expect("graph");
        let err = resolve(&graph, &test_version()).unwrap_err();
        assert_eq!(err.code(), "E_DEPENDENCY_VERSION_CONFLICT");
        match *err {
            ResolverError::VersionConflict {
                package,
                candidate,
                chain,
            } => {
                assert_eq!(package, "a_pkg");
                assert_eq!(candidate.to_string(), PATH_DEP_SENTINEL_VERSION);
                assert_eq!(chain.len(), 1);
                assert_eq!(chain[0].req, VersionReq::parse(">=1.0").unwrap());
            }
            other => panic!("expected VersionConflict, got {other:?}"),
        }
    }

    #[test]
    fn path_dep_wildcard_constraint_does_not_conflict() {
        // `*` accepts every version including the sentinel — happy path.
        let mut root = empty_manifest("root");
        root.dependencies.insert(
            "a".into(),
            DependencySpec::Path {
                path: PathBuf::from("a"),
                version: Some(VersionReq::parse("*").unwrap()),
            },
        );
        let a = empty_manifest("a_pkg");
        let graph = build_dep_graph(
            &PathBuf::from("/root"),
            root,
            &MemLoader {
                manifests: BTreeMap::from([(PathBuf::from("/root/a"), a)]),
            },
        )
        .expect("graph");
        let resolution = resolve(&graph, &test_version()).expect("resolve");
        assert!(resolution.packages.contains_key("a_pkg"));
    }

    #[test]
    fn diamond_path_dep_shares_one_resolved_entry() {
        // root → a → c
        // root → b → c
        // c appears once in the resolution with two declared_by edges.
        let mut root = empty_manifest("root");
        root.dependencies.insert("a".into(), path_dep("a"));
        root.dependencies.insert("b".into(), path_dep("b"));
        let mut a = empty_manifest("a_pkg");
        a.dependencies.insert("c".into(), path_dep("/shared"));
        let mut b = empty_manifest("b_pkg");
        b.dependencies.insert("c".into(), path_dep("/shared"));
        let c = empty_manifest("c_pkg");
        let graph = build_dep_graph(
            &PathBuf::from("/root"),
            root,
            &MemLoader {
                manifests: BTreeMap::from([
                    (PathBuf::from("/root/a"), a),
                    (PathBuf::from("/root/b"), b),
                    (PathBuf::from("/shared"), c),
                ]),
            },
        )
        .expect("graph");
        let resolution = resolve(&graph, &test_version()).expect("resolve");
        assert_eq!(resolution.packages.len(), 4);
        let c_resolved = &resolution.packages["c_pkg"];
        assert_eq!(c_resolved.declared_by.len(), 2);
    }

    #[test]
    fn version_conflict_chain_captures_every_violating_edge() {
        // Two parents declare conflicting strict constraints on a shared
        // child — both surface in the conflict chain.
        let mut root = empty_manifest("root");
        root.dependencies.insert("a".into(), path_dep("a"));
        root.dependencies.insert("b".into(), path_dep("b"));
        let mut a = empty_manifest("a_pkg");
        a.dependencies.insert(
            "c".into(),
            DependencySpec::Path {
                path: PathBuf::from("/shared"),
                version: Some(VersionReq::parse(">=1.0").unwrap()),
            },
        );
        let mut b = empty_manifest("b_pkg");
        b.dependencies.insert(
            "c".into(),
            DependencySpec::Path {
                path: PathBuf::from("/shared"),
                version: Some(VersionReq::parse(">=2.0").unwrap()),
            },
        );
        let c = empty_manifest("c_pkg");
        let graph = build_dep_graph(
            &PathBuf::from("/root"),
            root,
            &MemLoader {
                manifests: BTreeMap::from([
                    (PathBuf::from("/root/a"), a),
                    (PathBuf::from("/root/b"), b),
                    (PathBuf::from("/shared"), c),
                ]),
            },
        )
        .expect("graph");
        let err = resolve(&graph, &test_version()).unwrap_err();
        match *err {
            ResolverError::VersionConflict { chain, .. } => {
                assert_eq!(chain.len(), 2, "expected both edges to surface");
            }
            other => panic!("expected VersionConflict, got {other:?}"),
        }
    }

    #[test]
    fn msrv_satisfied_constraint_resolves() {
        // Constraint `>=1.0` against active-toolchain `1.0.0` → satisfied.
        let mut root = empty_manifest("root");
        root.kara_version = Some(VersionReq::parse(">=1.0").unwrap());
        let graph = build_dep_graph(
            &PathBuf::from("/root"),
            root,
            &MemLoader {
                manifests: BTreeMap::new(),
            },
        )
        .expect("graph");
        resolve(&graph, &test_version()).expect("MSRV-satisfied resolution");
    }

    #[test]
    fn msrv_too_old_surfaces_toolchain_too_old() {
        // Constraint `>=2.0` against active-toolchain `1.0.0` → fails.
        let mut root = empty_manifest("root");
        root.kara_version = Some(VersionReq::parse(">=2.0").unwrap());
        let graph = build_dep_graph(
            &PathBuf::from("/root"),
            root,
            &MemLoader {
                manifests: BTreeMap::new(),
            },
        )
        .expect("graph");
        let err = resolve(&graph, &test_version()).unwrap_err();
        assert_eq!(err.code(), "E_TOOLCHAIN_TOO_OLD");
        match *err {
            ResolverError::ToolchainTooOld {
                package,
                kara_version_req,
                active_version,
                ..
            } => {
                assert_eq!(package, "root");
                assert_eq!(kara_version_req, VersionReq::parse(">=2.0").unwrap());
                assert_eq!(active_version, test_version());
            }
            other => panic!("expected ToolchainTooOld, got {other:?}"),
        }
    }

    #[test]
    fn msrv_failure_attributed_to_offending_dep() {
        // Root has no MSRV, but a path-dep requires a newer toolchain.
        // The failure attributes to the dep, not the root.
        let mut root = empty_manifest("root");
        root.dependencies.insert("a".into(), path_dep("a"));
        let mut a = empty_manifest("a_pkg");
        a.kara_version = Some(VersionReq::parse(">=2.0").unwrap());
        let graph = build_dep_graph(
            &PathBuf::from("/root"),
            root,
            &MemLoader {
                manifests: BTreeMap::from([(PathBuf::from("/root/a"), a)]),
            },
        )
        .expect("graph");
        let err = resolve(&graph, &test_version()).unwrap_err();
        match *err {
            ResolverError::ToolchainTooOld {
                package,
                manifest_dir,
                ..
            } => {
                assert_eq!(package, "a_pkg");
                assert_eq!(manifest_dir, PathBuf::from("/root/a"));
            }
            other => panic!("expected ToolchainTooOld, got {other:?}"),
        }
    }

    #[test]
    fn msrv_absent_kara_version_does_not_block() {
        // No kara-version declared anywhere → MSRV check is silent.
        let root = empty_manifest("root");
        let graph = build_dep_graph(
            &PathBuf::from("/root"),
            root,
            &MemLoader {
                manifests: BTreeMap::new(),
            },
        )
        .expect("graph");
        resolve(&graph, &test_version()).expect("absent MSRV resolves cleanly");
    }

    #[test]
    fn msrv_runs_before_dep_resolution() {
        // Root with both a registry dep (would otherwise error) AND an
        // MSRV violation. The MSRV error should win — surfacing it first
        // gives a clearer signal than the registry-unsupported error.
        let mut root = empty_manifest("root");
        root.kara_version = Some(VersionReq::parse(">=2.0").unwrap());
        root.dependencies.insert("http".into(), registry("1.0"));
        let graph = build_dep_graph(
            &PathBuf::from("/root"),
            root,
            &MemLoader {
                manifests: BTreeMap::new(),
            },
        )
        .expect("graph");
        let err = resolve(&graph, &test_version()).unwrap_err();
        assert_eq!(err.code(), "E_TOOLCHAIN_TOO_OLD");
    }

    #[test]
    fn active_toolchain_version_parses_cargo_pkg_version() {
        // The compile-time CARGO_PKG_VERSION must always be a valid
        // semver. Regression pin against a future Cargo.toml bump that
        // accidentally introduces a non-semver string.
        let v = active_toolchain_version();
        assert!(!v.to_string().is_empty());
    }

    #[test]
    fn error_codes_round_trip() {
        let cases = [
            (
                ResolverError::ToolchainTooOld {
                    package: "x".into(),
                    manifest_dir: PathBuf::from("/x"),
                    kara_version_req: VersionReq::parse(">=2.0").unwrap(),
                    active_version: test_version(),
                },
                "E_TOOLCHAIN_TOO_OLD",
            ),
            (
                ResolverError::RegistryDepUnsupported {
                    package: "x".into(),
                    declared_in: PathBuf::from("/x"),
                },
                "E_REGISTRY_DEP_UNSUPPORTED",
            ),
            (
                ResolverError::GitDepUnsupported {
                    package: "x".into(),
                    declared_in: PathBuf::from("/x"),
                },
                "E_GIT_DEP_UNSUPPORTED",
            ),
            (
                ResolverError::SourceConflict {
                    package: "x".into(),
                    first_source: ResolvedSource::Root,
                    second_source: ResolvedSource::Path(PathBuf::from("/x")),
                },
                "E_DEPENDENCY_SOURCE_CONFLICT",
            ),
        ];
        for (err, code) in &cases {
            assert_eq!(err.code(), *code);
        }
    }

    #[test]
    fn resolution_is_deterministic_across_runs() {
        let mut root = empty_manifest("root");
        root.dependencies.insert("a".into(), path_dep("a"));
        root.dependencies.insert("b".into(), path_dep("b"));
        let a = empty_manifest("a_pkg");
        let b = empty_manifest("b_pkg");
        let loader = MemLoader {
            manifests: BTreeMap::from([
                (PathBuf::from("/root/a"), a),
                (PathBuf::from("/root/b"), b),
            ]),
        };
        // BTreeMap iteration is stable across two builds — pin that the
        // resolution slice respects it. Slice 5 / 7 surface this in stable
        // diagnostic output.
        let graph1 = build_dep_graph(
            &PathBuf::from("/root"),
            {
                let mut r = empty_manifest("root");
                r.dependencies.insert("a".into(), path_dep("a"));
                r.dependencies.insert("b".into(), path_dep("b"));
                r
            },
            &loader,
        )
        .expect("graph1");
        let graph2 = build_dep_graph(&PathBuf::from("/root"), root, &loader).expect("graph2");
        let r1 = resolve(&graph1, &test_version()).expect("r1");
        let r2 = resolve(&graph2, &test_version()).expect("r2");
        let names1: Vec<&String> = r1.packages.keys().collect();
        let names2: Vec<&String> = r2.packages.keys().collect();
        assert_eq!(names1, names2);
    }
}
