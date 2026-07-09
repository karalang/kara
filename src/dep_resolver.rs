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
    /// A resolved registry dep: the upstream catalog URL plus the on-disk
    /// directory the fetched tarball was extracted to. The URL feeds the
    /// lockfile (reproducibility); `dir` is where the CLI module loader
    /// (`dep_package_walks`) reads the package's source to compile it.
    Registry {
        url: String,
        /// Source root of the extracted tarball. Machine-local and
        /// transient, so it is intentionally *not* recorded in `kara.lock`
        /// (the lock keys reproducibility on `url` + content hash). Empty
        /// only in the reserved/pre-fetch forward-compat shape.
        dir: PathBuf,
    },
    /// A resolved git dep: the clone URL + requested ref plus the on-disk
    /// directory the repo was checked out to. The URL + ref feed the
    /// lockfile (reproducibility); `dir` is where the CLI module loader
    /// (`dep_package_walks`) reads the package's source to compile it —
    /// machine-local and transient, so intentionally *not* recorded in
    /// `kara.lock` (mirrors `Registry`'s `dir`).
    Git {
        url: String,
        reference: Option<GitRef>,
        dir: PathBuf,
        /// The commit SHA `HEAD` resolved to after checkout. Unlike `dir`
        /// (machine-local, transient), this IS persisted to `kara.lock` as a
        /// `#<sha>` fragment on the git source string — it's the
        /// reproducibility pin (git-fetch slice 3). Empty only in test
        /// fixtures that don't exercise a real clone.
        resolved_rev: String,
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
    /// PubGrub's global solve proved the *widened* candidate set (all
    /// published versions + their per-version deps, resolver follow-up (a)
    /// slice 3d) has no compatible assignment, and the per-package
    /// [`first_version_conflict`] detector couldn't name the conflict on its
    /// own (it only inspects each package's single selected version against
    /// its direct constraints, so it misses cross-version / transitive
    /// conflicts). `report` is PubGrub's rendered derivation-tree explanation.
    /// Maps to `E_DEPENDENCY_UNSATISFIABLE`.
    UnsatisfiableGraph { report: String },
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
            Self::UnsatisfiableGraph { .. } => "E_DEPENDENCY_UNSATISFIABLE",
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
            Self::UnsatisfiableGraph { report } => write!(
                f,
                "dependency version solving found no compatible set of versions:\n{report}",
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
                DependencySpec::Registry { version: req } => {
                    // A registry dep resolves only if the graph walk actually
                    // fetched it (a `RegistryProvider` was configured). If not,
                    // it stays unsupported — the pre-fetch behavior.
                    match graph
                        .registry_resolutions
                        .get(&(manifest_dir.clone(), dep_name.clone()))
                    {
                        Some(res) => {
                            let child_manifest =
                                graph.manifests.get(&res.dir).ok_or_else(|| {
                                    Box::new(ResolverError::RegistryDepUnsupported {
                                        package: dep_name.clone(),
                                        declared_in: manifest_dir.clone(),
                                    })
                                })?;
                            let resolved_name = child_manifest.name.clone();
                            upsert_registry(
                                &mut packages,
                                resolved_name,
                                res.version.clone(),
                                res.upstream_url.clone(),
                                res.dir.clone(),
                                DeclarationEdge {
                                    parent: parent_name.clone(),
                                    req: Some(req.clone()),
                                },
                            )?;
                        }
                        None => {
                            return Err(Box::new(ResolverError::RegistryDepUnsupported {
                                package: dep_name.clone(),
                                declared_in: manifest_dir.clone(),
                            }));
                        }
                    }
                }
                DependencySpec::Git { version, .. } => {
                    // A git dep resolves only if the graph walk actually
                    // cloned it (a `GitProvider` was configured). If not, it
                    // stays unsupported — the pre-fetch behavior.
                    match graph
                        .git_resolutions
                        .get(&(manifest_dir.clone(), dep_name.clone()))
                    {
                        Some(res) => {
                            let child_manifest =
                                graph.manifests.get(&res.dir).ok_or_else(|| {
                                    Box::new(ResolverError::GitDepUnsupported {
                                        package: dep_name.clone(),
                                        declared_in: manifest_dir.clone(),
                                    })
                                })?;
                            let resolved_name = child_manifest.name.clone();
                            upsert_git(
                                &mut packages,
                                resolved_name,
                                res.url.clone(),
                                res.reference.clone(),
                                res.dir.clone(),
                                res.resolved_rev.clone(),
                                sentinel.clone(),
                                DeclarationEdge {
                                    parent: parent_name.clone(),
                                    req: version.clone(),
                                },
                            )?;
                        }
                        None => {
                            return Err(Box::new(ResolverError::GitDepUnsupported {
                                package: dep_name.clone(),
                                declared_in: manifest_dir.clone(),
                            }));
                        }
                    }
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

    // Resolver follow-up (a) slice 3d: PubGrub's global solve is the authority
    // for version *selection*, run over the **widened** candidate set the graph
    // walk recorded (`graph.registry_candidates` — every published version of
    // each registry package + that version's own per-version deps). This is
    // where backtracking goes live: a diamond whose highest version is
    // incompatible with a sibling's constraint now resolves to a compatible
    // *lower* version instead of erroring.
    //
    // `first_version_conflict` (the precise per-package constraint-chain
    // detector, slice 2) is folded in as the diagnostic authority *on failure*
    // — it names the exact violating parents when the conflict is expressible
    // per-package, and PubGrub's derivation-tree report covers the
    // cross-version / transitive conflicts the single-candidate detector can't
    // see. On success PubGrub's selection already satisfies every constraint,
    // so the per-package check is redundant and skipped.
    match select_versions_with_pubgrub(
        &mut packages,
        &root_name,
        &sentinel,
        &graph.registry_candidates,
    ) {
        PubgrubOutcome::Solved => Ok(Resolution { packages }),
        PubgrubOutcome::NoSolution(report) => {
            // Prefer the precise per-package chain; fall back to PubGrub's
            // derivation report when the conflict only exists across versions.
            if let Some(err) = first_version_conflict(&packages) {
                Err(err)
            } else {
                Err(Box::new(ResolverError::UnsatisfiableGraph { report }))
            }
        }
        PubgrubOutcome::Inconclusive => {
            // The solver couldn't reach a verdict (should not happen with the
            // in-memory provider, whose fetch is infallible). The classic
            // per-package check is then the sole authority — exactly the
            // pre-3d behavior.
            if let Some(err) = first_version_conflict(&packages) {
                Err(err)
            } else {
                Ok(Resolution { packages })
            }
        }
    }
}

/// Outcome of the PubGrub version selection (resolver follow-up (a) slice 3d).
enum PubgrubOutcome {
    /// PubGrub found a compatible assignment; the selected versions have been
    /// written back into `packages`.
    Solved,
    /// PubGrub proved the graph unsatisfiable. Carries its rendered
    /// derivation-tree explanation, used only when the per-package
    /// [`first_version_conflict`] detector can't name the conflict itself.
    NoSolution(String),
    /// PubGrub couldn't reach a verdict (an internal solver error unrelated to
    /// the constraint set). The caller falls back to the classic per-package
    /// check as the sole authority.
    Inconclusive,
}

/// The per-package version-conflict detector extracted from [`resolve_with_offline`].
/// Returns the first (deterministic BTreeMap order) package whose pinned
/// version violates one or more declared constraints, with the full chain of
/// violating parent edges — or `None` when every package satisfies all its
/// constraints.
fn first_version_conflict(
    packages: &BTreeMap<String, ResolvedPackage>,
) -> Option<Box<ResolverError>> {
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
            return Some(Box::new(ResolverError::VersionConflict {
                package: resolved.name.clone(),
                candidate: resolved.version.clone(),
                chain: violating,
            }));
        }
    }
    None
}

/// Run PubGrub over the resolved packages and adopt its selected versions
/// (resolver follow-up (a) slices 2 + 3d). Builds the forward dependency edges
/// (inverting each package's `declared_by`) and a candidate registry, then
/// calls [`crate::pubgrub_solve::solve`]. On success it writes each
/// dependency's selected version back into `packages` (the root keeps its
/// sentinel) and returns [`PubgrubOutcome::Solved`]; on a proven conflict it
/// leaves `packages` unchanged and returns [`PubgrubOutcome::NoSolution`] with
/// PubGrub's derivation report; on a spurious solver error,
/// [`PubgrubOutcome::Inconclusive`].
///
/// **Candidate set (slice 3d).** For each registry package the walk recorded a
/// widened candidate set in `registry_candidates` (every published version +
/// that version's own registry deps), the registry carries *all* of them, so
/// PubGrub genuinely chooses and backtracks. For a package with no recorded
/// candidates — a path/git dep (sentinel version), or a registry package whose
/// provider couldn't enumerate — it falls back to the single pinned version
/// with the package's graph-derived forward deps, exactly the slice-2 shape.
///
/// **Limitation.** The walk only fetches the *selected* version's subtree, so a
/// non-selected candidate whose transitive deps were never fetched appears to
/// PubGrub as depending on an unknown (version-less) package and is avoided —
/// backtracking is complete within the fetched candidate set but not beyond it.
/// A fully lazy `DependencyProvider` (fetch-on-demand during the solve) is the
/// future optimization that closes this.
fn select_versions_with_pubgrub(
    packages: &mut BTreeMap<String, ResolvedPackage>,
    root_name: &str,
    root_version: &semver::Version,
    registry_candidates: &BTreeMap<String, Vec<crate::dep_graph::RegistryCandidate>>,
) -> PubgrubOutcome {
    use crate::pubgrub_solve::{solve, CandidateVersion, PackageCandidates, SolveError};

    // Forward edges: parent package name → [(dependency name, constraint)].
    // `declared_by` is the reverse map (who required me); invert it. A path/git
    // dep with no version constraint contributes `*`.
    let mut deps_of: BTreeMap<String, Vec<(String, semver::VersionReq)>> = BTreeMap::new();
    for pkg in packages.values() {
        for edge in &pkg.declared_by {
            let req = edge.req.clone().unwrap_or(semver::VersionReq::STAR);
            deps_of
                .entry(edge.parent.clone())
                .or_default()
                .push((pkg.name.clone(), req));
        }
    }

    // The candidate registry: the widened published set where the walk recorded
    // one, else a single pinned candidate with the package's graph-derived deps.
    let registry: BTreeMap<String, PackageCandidates> = packages
        .values()
        .filter(|p| p.name != root_name)
        .map(|p| {
            let versions = match registry_candidates.get(&p.name) {
                Some(cands) if !cands.is_empty() => cands
                    .iter()
                    .map(|c| CandidateVersion {
                        version: c.version.clone(),
                        deps: c.deps.clone(),
                    })
                    .collect(),
                _ => vec![CandidateVersion {
                    version: p.version.clone(),
                    deps: deps_of.get(&p.name).cloned().unwrap_or_default(),
                }],
            };
            (p.name.clone(), PackageCandidates { versions })
        })
        .collect();
    let root_deps = deps_of.get(root_name).cloned().unwrap_or_default();

    match solve(root_name, root_version, &root_deps, &registry) {
        Ok(selected) => {
            for (name, version) in selected {
                if name == root_name {
                    continue;
                }
                if let Some(p) = packages.get_mut(&name) {
                    p.version = version;
                }
            }
            PubgrubOutcome::Solved
        }
        Err(SolveError::NoSolution(report)) => PubgrubOutcome::NoSolution(report),
        Err(SolveError::Internal(_)) => PubgrubOutcome::Inconclusive,
    }
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

/// Insert or merge a resolved *registry* package. The first fetch of a
/// given package name pins the version; later declarers append their
/// constraint edge (the post-walk constraint-validation pass flags any
/// edge whose requirement the pinned version doesn't satisfy). A name
/// already resolved from a different source kind (root / path) is a
/// `SourceConflict`.
fn upsert_registry(
    packages: &mut BTreeMap<String, ResolvedPackage>,
    name: String,
    version: semver::Version,
    url: String,
    dir: PathBuf,
    edge: DeclarationEdge,
) -> Result<(), Box<ResolverError>> {
    if let Some(existing) = packages.get_mut(&name) {
        match &existing.source {
            ResolvedSource::Registry { .. } => {
                existing.declared_by.push(edge);
                Ok(())
            }
            other => Err(Box::new(ResolverError::SourceConflict {
                package: name.clone(),
                first_source: other.clone(),
                second_source: ResolvedSource::Registry { url, dir },
            })),
        }
    } else {
        packages.insert(
            name.clone(),
            ResolvedPackage {
                name,
                version,
                source: ResolvedSource::Registry { url, dir },
                declared_by: vec![edge],
            },
        );
        Ok(())
    }
}

/// Insert or merge a resolved *git* package. Like a path dep, a git dep is
/// source-pinned (by ref, not semver), so it takes the same sentinel
/// `version`; later declarers of the same name append their constraint edge.
/// A name already resolved from a different source kind is a `SourceConflict`.
#[allow(clippy::too_many_arguments)]
fn upsert_git(
    packages: &mut BTreeMap<String, ResolvedPackage>,
    name: String,
    url: String,
    reference: Option<GitRef>,
    dir: PathBuf,
    resolved_rev: String,
    version: semver::Version,
    edge: DeclarationEdge,
) -> Result<(), Box<ResolverError>> {
    if let Some(existing) = packages.get_mut(&name) {
        match &existing.source {
            ResolvedSource::Git { .. } => {
                existing.declared_by.push(edge);
                Ok(())
            }
            other => Err(Box::new(ResolverError::SourceConflict {
                package: name.clone(),
                first_source: other.clone(),
                second_source: ResolvedSource::Git {
                    url,
                    reference,
                    dir,
                    resolved_rev,
                },
            })),
        }
    } else {
        packages.insert(
            name.clone(),
            ResolvedPackage {
                name,
                version,
                source: ResolvedSource::Git {
                    url,
                    reference,
                    dir,
                    resolved_rev,
                },
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
            is_workspace_root: false,
            target_dependencies: BTreeMap::new(),
            target_dev_dependencies: BTreeMap::new(),
            target_profile_overrides: BTreeMap::new(),
            build_default_target: None,
            build_targets: Vec::new(),
            build_registry_proxy: None,
            build_registry: None,
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
            lib_name: None,
            lib_crate_type: None,
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

    /// In-memory `RegistryProvider` for resolver tests — maps a name to a
    /// canned extracted dir + version + upstream URL.
    struct MockRegistryProvider {
        fetches: BTreeMap<String, (PathBuf, semver::Version, String)>,
    }

    impl crate::dep_graph::RegistryProvider for MockRegistryProvider {
        fn fetch(
            &self,
            name: &str,
            _req: &VersionReq,
        ) -> Result<crate::dep_graph::MaterializedDep, String> {
            self.fetches
                .get(name)
                .map(|(dir, ver, up)| crate::dep_graph::MaterializedDep {
                    root_dir: dir.clone(),
                    version: ver.clone(),
                    upstream_url: up.clone(),
                })
                .ok_or_else(|| format!("no such package {name}"))
        }
    }

    /// In-memory git provider keyed by clone URL → (checkout dir, resolved
    /// rev). Lets the graph + resolver wiring be tested without a real `git`.
    struct MockGitProvider {
        clones: BTreeMap<String, (PathBuf, String)>,
    }

    impl crate::git_fetch::GitProvider for MockGitProvider {
        fn fetch(
            &self,
            url: &str,
            _reference: Option<&GitRef>,
        ) -> Result<crate::git_fetch::MaterializedGitDep, crate::git_fetch::GitFetchError> {
            self.clones
                .get(url)
                .map(|(dir, rev)| crate::git_fetch::MaterializedGitDep {
                    root_dir: dir.clone(),
                    resolved_rev: rev.clone(),
                })
                .ok_or_else(|| crate::git_fetch::GitFetchError::CommandFailed {
                    step: "clone",
                    url: url.to_string(),
                    message: "no such repo".to_string(),
                })
        }
    }

    #[test]
    fn fetched_registry_dep_resolves_to_registry_source() {
        let mut root = empty_manifest("app");
        root.dependencies.insert("http".into(), registry("^1.0"));
        let http_mf = empty_manifest("http");

        let loader = MemLoader {
            manifests: BTreeMap::from([(PathBuf::from("/reg/http"), http_mf)]),
        };
        let provider = MockRegistryProvider {
            fetches: BTreeMap::from([(
                "http".to_string(),
                (
                    PathBuf::from("/reg/http"),
                    semver::Version::parse("1.4.2").unwrap(),
                    "https://up/http".to_string(),
                ),
            )]),
        };
        let graph = crate::dep_graph::build_dep_graph_with_options(
            &PathBuf::from("/app"),
            root,
            &loader,
            crate::dep_graph::DepGraphOptions {
                offline_root: None,
                include_dev_deps: false,
                registry_provider: Some(&provider),
                git_provider: None,
            },
        )
        .expect("graph");

        let resolution = resolve(&graph, &test_version()).expect("resolve");
        let http = resolution.packages.get("http").expect("http resolved");
        assert_eq!(http.version, semver::Version::parse("1.4.2").unwrap());
        assert_eq!(
            http.source,
            ResolvedSource::Registry {
                url: "https://up/http".to_string(),
                dir: PathBuf::from("/reg/http"),
            }
        );
    }

    #[test]
    fn fetched_registry_dep_version_conflict_is_reported() {
        // Root declares http `^1.0`; a transitive registry dep `mid`
        // re-declares http at `^2.0`. The provider pins http to 1.4.2, which
        // can't satisfy `^2.0`, so the validation pass flags the conflict.
        let mut root = empty_manifest("app");
        root.dependencies.insert("http".into(), registry("^1.0"));
        root.dependencies.insert("mid".into(), registry("^1.0"));
        let mut mid = empty_manifest("mid");
        mid.dependencies.insert("http".into(), registry("^2.0"));

        let loader = MemLoader {
            manifests: BTreeMap::from([
                (PathBuf::from("/reg/mid"), mid),
                (PathBuf::from("/reg/http"), empty_manifest("http")),
            ]),
        };
        // Provider returns http@1.4.2 for any req (a single cached catalog);
        // the `^2.0` declarer's constraint won't match.
        let provider = MockRegistryProvider {
            fetches: BTreeMap::from([
                (
                    "http".to_string(),
                    (
                        PathBuf::from("/reg/http"),
                        semver::Version::parse("1.4.2").unwrap(),
                        "u".to_string(),
                    ),
                ),
                (
                    "mid".to_string(),
                    (
                        PathBuf::from("/reg/mid"),
                        semver::Version::parse("1.0.0").unwrap(),
                        "u".to_string(),
                    ),
                ),
            ]),
        };
        let graph = crate::dep_graph::build_dep_graph_with_options(
            &PathBuf::from("/app"),
            root,
            &loader,
            crate::dep_graph::DepGraphOptions {
                offline_root: None,
                include_dev_deps: false,
                registry_provider: Some(&provider),
                git_provider: None,
            },
        )
        .expect("graph");
        let err = resolve(&graph, &test_version()).unwrap_err();
        assert_eq!(err.code(), "E_DEPENDENCY_VERSION_CONFLICT");
    }

    /// Multi-version registry mock (mirrors the `dep_graph` one) — enumerates
    /// versions and fetches exact ones so `registry_candidates` gets populated,
    /// which is what activates PubGrub backtracking (slice 3d).
    struct MultiVersionMockProvider {
        versions: BTreeMap<String, Vec<semver::Version>>,
        selected: BTreeMap<String, (PathBuf, semver::Version)>,
        exact: BTreeMap<(String, semver::Version), PathBuf>,
    }

    impl crate::dep_graph::RegistryProvider for MultiVersionMockProvider {
        fn fetch(
            &self,
            name: &str,
            _req: &VersionReq,
        ) -> Result<crate::dep_graph::MaterializedDep, String> {
            self.selected
                .get(name)
                .map(|(dir, ver)| crate::dep_graph::MaterializedDep {
                    root_dir: dir.clone(),
                    version: ver.clone(),
                    upstream_url: format!("https://up/{name}"),
                })
                .ok_or_else(|| format!("no such package {name}"))
        }
        fn available_versions(&self, name: &str) -> Result<Vec<semver::Version>, String> {
            Ok(self.versions.get(name).cloned().unwrap_or_default())
        }
        fn fetch_exact(
            &self,
            name: &str,
            version: &semver::Version,
        ) -> Result<crate::dep_graph::MaterializedDep, String> {
            self.exact
                .get(&(name.to_string(), version.clone()))
                .map(|dir| crate::dep_graph::MaterializedDep {
                    root_dir: dir.clone(),
                    version: version.clone(),
                    upstream_url: format!("https://up/{name}"),
                })
                .ok_or_else(|| format!("no {name}@{version}"))
        }
    }

    fn ver(s: &str) -> semver::Version {
        semver::Version::parse(s).unwrap()
    }

    #[test]
    fn pubgrub_backtracks_to_lower_version_over_a_capped_sibling() {
        // root wants `x ^1.0` (highest is 1.9.0) and `y ^1.0`; but `y` caps
        // `x < 1.5`. The eager walk pins x@1.9.0 (its highest ^1.0 match) — so
        // the pre-3d per-package check would reject 1.9.0 against y's `< 1.5`.
        // With the widened candidate set, PubGrub backtracks to x@1.0.0, which
        // satisfies both, and the resolve SUCCEEDS.
        let mut root = empty_manifest("app");
        root.dependencies.insert("x".into(), registry("^1.0"));
        root.dependencies.insert("y".into(), registry("^1.0"));

        let mut y_mf = empty_manifest("y");
        y_mf.dependencies.insert("x".into(), registry("<1.5"));

        let loader = MemLoader {
            manifests: BTreeMap::from([
                (PathBuf::from("/reg/x-1.9.0"), empty_manifest("x")),
                (PathBuf::from("/reg/x-1.0.0"), empty_manifest("x")),
                (PathBuf::from("/reg/y"), y_mf),
            ]),
        };
        let provider = MultiVersionMockProvider {
            versions: BTreeMap::from([
                ("x".to_string(), vec![ver("1.0.0"), ver("1.9.0")]),
                ("y".to_string(), vec![ver("1.0.0")]),
            ]),
            selected: BTreeMap::from([
                (
                    "x".to_string(),
                    (PathBuf::from("/reg/x-1.9.0"), ver("1.9.0")),
                ),
                ("y".to_string(), (PathBuf::from("/reg/y"), ver("1.0.0"))),
            ]),
            exact: BTreeMap::from([
                (
                    ("x".to_string(), ver("1.0.0")),
                    PathBuf::from("/reg/x-1.0.0"),
                ),
                (
                    ("x".to_string(), ver("1.9.0")),
                    PathBuf::from("/reg/x-1.9.0"),
                ),
                (("y".to_string(), ver("1.0.0")), PathBuf::from("/reg/y")),
            ]),
        };

        let graph = crate::dep_graph::build_dep_graph_with_options(
            &PathBuf::from("/app"),
            root,
            &loader,
            crate::dep_graph::DepGraphOptions {
                offline_root: None,
                include_dev_deps: false,
                registry_provider: Some(&provider),
                git_provider: None,
            },
        )
        .expect("graph");

        let resolution =
            resolve(&graph, &test_version()).expect("resolve should backtrack, not error");
        assert_eq!(
            resolution.packages.get("x").expect("x resolved").version,
            ver("1.0.0"),
            "PubGrub must backtrack x from 1.9.0 to 1.0.0 to satisfy y's `< 1.5` cap"
        );
        assert_eq!(
            resolution.packages.get("y").expect("y resolved").version,
            ver("1.0.0")
        );
    }

    #[test]
    fn unsatisfiable_graph_error_renders_derivation_report() {
        // The fallback diagnostic for a cross-version conflict the per-package
        // detector can't name: it carries PubGrub's derivation report verbatim.
        let err = ResolverError::UnsatisfiableGraph {
            report:
                "because a 1.0.0 depends on b ^2.0\nand b has no version, version solving failed"
                    .to_string(),
        };
        assert_eq!(err.code(), "E_DEPENDENCY_UNSATISFIABLE");
        let shown = err.to_string();
        assert!(shown.contains("no compatible set"));
        assert!(shown.contains("because a 1.0.0 depends on b ^2.0"));

        let diag = crate::dep_diagnostic::render_resolver_error(&err);
        assert_eq!(diag.code, "E_DEPENDENCY_UNSATISFIABLE");
        // Each line of the report becomes its own note.
        assert_eq!(diag.notes.len(), 2);
        assert!(diag.notes[0].contains("because a 1.0.0 depends on b ^2.0"));
    }

    #[test]
    fn fetched_git_dep_resolves_to_git_source() {
        let mut root = empty_manifest("app");
        root.dependencies.insert(
            "lib".into(),
            DependencySpec::Git {
                url: "https://git/lib".into(),
                reference: Some(GitRef::Tag("v1.0".into())),
                version: None,
            },
        );
        let lib_mf = empty_manifest("lib");
        let loader = MemLoader {
            manifests: BTreeMap::from([(PathBuf::from("/git/lib"), lib_mf)]),
        };
        let provider = MockGitProvider {
            clones: BTreeMap::from([(
                "https://git/lib".to_string(),
                (PathBuf::from("/git/lib"), "abc123def".to_string()),
            )]),
        };
        let graph = crate::dep_graph::build_dep_graph_with_options(
            &PathBuf::from("/app"),
            root,
            &loader,
            crate::dep_graph::DepGraphOptions {
                offline_root: None,
                include_dev_deps: false,
                registry_provider: None,
                git_provider: Some(&provider),
            },
        )
        .expect("graph");

        // The graph recorded the concrete resolution (dir + url + rev) so a
        // future lockfile rev-pin has what it needs.
        let res = graph
            .git_resolutions
            .values()
            .next()
            .expect("a git resolution");
        assert_eq!(res.resolved_rev, "abc123def");
        assert_eq!(res.url, "https://git/lib");

        let resolution = resolve(&graph, &test_version()).expect("resolve");
        let lib = resolution.packages.get("lib").expect("lib resolved");
        assert_eq!(
            lib.source,
            ResolvedSource::Git {
                url: "https://git/lib".to_string(),
                reference: Some(GitRef::Tag("v1.0".to_string())),
                dir: PathBuf::from("/git/lib"),
                resolved_rev: "abc123def".to_string(),
            }
        );
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
    fn pubgrub_selection_resolves_a_multi_level_graph() {
        // Resolver follow-up (a) slice 2: resolution now flows through PubGrub's
        // version selection (`select_versions_with_pubgrub`). Exercise the
        // forward-edge inversion + selection over a non-trivial shape —
        // root → a, root → b, a → c — and confirm every reachable package is
        // present at its pinned version. With one candidate per package the
        // selection is a fixed point, so this equals the walk's pins; the test
        // locks that the PubGrub path returns a complete, uncorrupted
        // resolution (slice 3 adds multi-candidate backtracking-through-resolve
        // coverage once the candidate set widens).
        let mut root = empty_manifest("root");
        root.dependencies.insert("a".into(), path_dep("a"));
        root.dependencies.insert("b".into(), path_dep("b"));
        let mut a = empty_manifest("a_pkg");
        a.dependencies.insert("c".into(), path_dep("c"));
        let b = empty_manifest("b_pkg");
        let c = empty_manifest("c_pkg");
        let graph = build_dep_graph(
            &PathBuf::from("/root"),
            root,
            &MemLoader {
                manifests: BTreeMap::from([
                    (PathBuf::from("/root/a"), a),
                    (PathBuf::from("/root/b"), b),
                    (PathBuf::from("/root/a/c"), c),
                ]),
            },
        )
        .expect("graph");
        let resolution = resolve(&graph, &test_version()).expect("resolve");
        // root + a_pkg + b_pkg + c_pkg.
        assert_eq!(resolution.packages.len(), 4);
        for name in ["root", "a_pkg", "b_pkg", "c_pkg"] {
            let pkg = resolution
                .packages
                .get(name)
                .unwrap_or_else(|| panic!("`{name}` missing from resolution"));
            assert_eq!(
                pkg.version.to_string(),
                PATH_DEP_SENTINEL_VERSION,
                "`{name}` should keep the path-dep sentinel through PubGrub selection",
            );
        }
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
