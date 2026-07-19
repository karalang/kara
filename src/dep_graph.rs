//! Dependency-graph materialization (slice 3 of the PubGrub resolver entry,
//! ships `docs/implementation_checklist/phase-5-diagnostics.md` line 813).
//!
//! Transforms the entry-point project's manifest into a flat graph the
//! resolver can consume. Two transformations happen here:
//!
//! 1. **Workspace deref** — replace each `DependencySpec::Workspace` entry
//!    with its concrete equivalent from `[workspace.dependencies]` declared
//!    on the entry-point manifest. Errors with `E_WORKSPACE_DEP_NOT_DECLARED`
//!    when a member writes `workspace = true` for a dep absent from the
//!    workspace table.
//!
//! 2. **Path-dep walking** — recursively load every reachable path-dep
//!    `kara.toml`. Cycle detection rejects loops like `a → b → a` with
//!    `E_DEPENDENCY_CYCLE` naming the chain.
//!
//! 3. **Registry-dep walking** (registry fetch epic slice 3) — when a
//!    [`RegistryProvider`] is supplied, each `DependencySpec::Registry` is
//!    fetched + extracted to disk and recursed into like a path-dep, its
//!    concrete resolution recorded in `registry_resolutions`. Without a
//!    provider, registry deps stop at the leaf and the resolver reports
//!    them as unsupported.
//!
//! 4. **Git-dep walking** (git fetch slice 2) — the same shape, gated on a
//!    [`crate::git_fetch::GitProvider`]: each `DependencySpec::Git` is cloned,
//!    checked out, and recursed into, its resolution recorded in
//!    `git_resolutions`. Without a provider, git deps stop at the leaf.
//!
//! Output: `DepGraph { root_dir, manifests, derived_deps }` — `manifests`
//! is the cache slice 4 (`DependencyProvider::get_dependencies`) will read
//! from; `derived_deps` is the per-manifest map of `[dependencies]` with
//! workspace entries replaced.

use crate::manifest::{self, DependencySpec, Manifest};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

/// Materialized dependency graph rooted at the entry-point project.
///
/// `manifests` is keyed by the canonicalized project-root directory so two
/// path-deps that reach the same target via different relative paths share
/// one entry (no double-counting). `derived_deps[dir]` is the manifest's
/// `[dependencies]` *after* workspace-deref — every `Workspace` entry has
/// been replaced with the concrete spec from the workspace root.
#[derive(Debug)]
pub struct DepGraph {
    pub root_dir: PathBuf,
    pub manifests: BTreeMap<PathBuf, Manifest>,
    pub derived_deps: BTreeMap<PathBuf, BTreeMap<String, DependencySpec>>,
    /// For every `DependencySpec::Registry` the walk actually fetched (only
    /// when a [`RegistryProvider`] was supplied), the concrete resolution:
    /// keyed by `(declaring manifest dir, dep name)`, valued with the
    /// extracted source dir, the selected version, and the upstream URL.
    /// The extracted dir is also a key into `manifests`. Empty when no
    /// provider is configured — the resolver then reports registry deps as
    /// unsupported, preserving the pre-fetch behavior.
    pub registry_resolutions: BTreeMap<(PathBuf, String), RegistryResolution>,
    /// For every registry package the walk fetched (once per package *name*),
    /// its full **candidate set**: every selectable published version paired
    /// with that version's own registry-dep requirements. This is the widened
    /// input PubGrub's global solve draws from (resolver follow-up (a) slice
    /// 3c) — independent of, and richer than, the single version the walk
    /// currently selects into `registry_resolutions`. Empty for a package
    /// whose provider can't enumerate (offline / a non-enumerating provider);
    /// the solver then falls back to the single selected candidate. Empty
    /// overall when no `RegistryProvider` is configured.
    pub registry_candidates: BTreeMap<String, Vec<RegistryCandidate>>,
    /// For every `DependencySpec::Git` the walk actually cloned (only when a
    /// [`crate::git_fetch::GitProvider`] was supplied), the concrete
    /// resolution: keyed by `(declaring manifest dir, dep name)`, valued with
    /// the checked-out source dir, the git URL + ref, and the resolved commit
    /// SHA. Empty when no provider is configured — the resolver then reports
    /// git deps as unsupported, preserving the pre-fetch behavior.
    pub git_resolutions: BTreeMap<(PathBuf, String), GitResolution>,
    /// For every fetched registry package the provider reports a non-empty
    /// **yanked** set for, its yanked published versions, keyed by package name.
    /// Fresh selection already excludes yanked versions, so this is consulted
    /// only to warn when a *pinned* resolved version (from `kara.lock`) turns
    /// out to be yanked (`W_DEPENDENCY_YANKED`, resolver follow-up (h)). Empty
    /// when no `RegistryProvider` is configured, when the provider can't
    /// enumerate yanked versions, or when nothing is yanked.
    pub yanked_versions: BTreeMap<String, Vec<semver::Version>>,
}

/// One fetched-and-extracted registry dependency (see
/// [`DepGraph::registry_resolutions`]).
#[derive(Debug, Clone)]
pub struct RegistryResolution {
    /// Extracted source directory — where the package's `kara.toml` lives.
    pub dir: PathBuf,
    /// Concrete version the provider selected for the requested constraint.
    pub version: semver::Version,
    /// Original upstream source URL (from the catalog), for the lockfile.
    pub upstream_url: String,
}

/// One published version of a registry package plus its own registry-dep
/// requirements — the per-version data PubGrub's global solve needs to
/// backtrack (resolver follow-up (a) slice 3c). Recorded for every selectable
/// published version of each fetched registry package, independent of which
/// version the walk selects + recurses into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryCandidate {
    /// A selectable (non-yanked) published version.
    pub version: semver::Version,
    /// This version's `[dependencies]` **registry** requirements as
    /// `(dep-name, constraint)` forward edges. Path/git deps of a published
    /// package are outside the v1 version-solve domain and are not recorded.
    pub deps: Vec<(String, semver::VersionReq)>,
}

/// One cloned-and-checked-out git dependency (see
/// [`DepGraph::git_resolutions`]).
#[derive(Debug, Clone)]
pub struct GitResolution {
    /// Checked-out source directory — where the package's `kara.toml` lives.
    pub dir: PathBuf,
    /// The git URL the dep was cloned from (verbatim, for the lockfile).
    pub url: String,
    /// The requested ref (branch / tag / rev), or `None` for the default
    /// branch.
    pub reference: Option<crate::manifest::GitRef>,
    /// The commit SHA `HEAD` resolved to after checkout — the reproducibility
    /// hook for a future `LockSource::Git` rev-pin.
    pub resolved_rev: String,
}

/// One materialized registry dependency: an extracted on-disk source tree
/// plus the concrete version + upstream URL the fetch resolved to. Produced
/// by a [`RegistryProvider`].
#[derive(Debug, Clone)]
pub struct MaterializedDep {
    pub root_dir: PathBuf,
    pub version: semver::Version,
    pub upstream_url: String,
}

/// Abstracts fetching + materializing a registry dependency to disk, so the
/// graph walk can recurse into registry deps the same way it recurses into
/// path deps. The production impl (`registry_extract::ProxyRegistryProvider`)
/// composes the proxy client + tarball extraction + on-disk cache; tests use
/// an in-memory stand-in. Mirrors the [`ManifestLoader`] abstraction.
pub trait RegistryProvider {
    /// Fetch the highest version of `name` satisfying `req`, materialize its
    /// source tree on disk, and return where it landed. The `Err` string is
    /// wrapped into a [`DepGraphError::RegistryFetchFailed`] diagnostic.
    fn fetch(&self, name: &str, req: &semver::VersionReq) -> Result<MaterializedDep, String>;

    /// The package's **selectable** (non-yanked) published versions, ascending
    /// — the candidate set the version solver widens over (resolver follow-up
    /// (a) slice 3). The production provider draws this from the registry
    /// catalog; the `Err` string wraps into `RegistryFetchFailed`.
    ///
    /// The default returns an empty vec: a provider that *can't* enumerate
    /// (an offline / vendor-only stand-in, or a test mock) simply offers no
    /// widened set, and the walk falls back to the single [`fetch`](Self::fetch)
    /// candidate. Empty is "no candidates to widen over", not an error — a
    /// provider signals a genuine catalog failure via `Err`.
    fn available_versions(&self, _name: &str) -> Result<Vec<semver::Version>, String> {
        Ok(Vec::new())
    }

    /// Materialize the **exact** `version` of `name` on disk — no range
    /// selection. The solver picks a concrete version from
    /// [`available_versions`](Self::available_versions) and then materializes
    /// precisely that one, so a fresh solve and a lockfile pin fetch identical
    /// trees.
    ///
    /// The default expresses the pin as an `=X.Y.Z` range and delegates to
    /// [`fetch`](Self::fetch); this is correct for any provider whose range
    /// fetch honors an exact requirement. The production provider overrides it
    /// to reach the tarball directly (and, unlike fresh range selection, to
    /// resolve a version even after it was yanked — reproducing a pin must
    /// succeed). The `Err` string wraps into `RegistryFetchFailed`.
    fn fetch_exact(
        &self,
        name: &str,
        version: &semver::Version,
    ) -> Result<MaterializedDep, String> {
        self.fetch(name, &crate::registry_proxy::exact_version_req(version))
    }

    /// The package's **yanked** published versions — the ones the catalog marks
    /// withdrawn. Fresh selection excludes these (they never appear in
    /// [`available_versions`](Self::available_versions)), but a lockfile pin can
    /// still resolve to a now-yanked version (`fetch_exact` is *not*
    /// yanked-filtered — reproducing a pin must succeed), and that is exactly
    /// when the resolver warns (`W_DEPENDENCY_YANKED`, resolver follow-up (h)).
    /// The graph records this set so the post-resolution audit can flag a
    /// pinned-but-yanked version.
    ///
    /// The default returns an empty vec: a provider that can't enumerate offers
    /// no yanked set, so no warning fires. The `Err` string wraps into
    /// `RegistryFetchFailed`.
    fn yanked_versions(&self, _name: &str) -> Result<Vec<semver::Version>, String> {
        Ok(Vec::new())
    }
}

#[derive(Debug)]
pub enum DepGraphError {
    /// A manifest declared `name = { workspace = true }` but the dep wasn't
    /// declared in the entry-point manifest's `[workspace.dependencies]`.
    /// Maps to `E_WORKSPACE_DEP_NOT_DECLARED`.
    WorkspaceDepNotDeclared {
        manifest_dir: PathBuf,
        dep_name: String,
    },
    /// A non-root manifest used `workspace = true` but the entry-point
    /// manifest has no `[workspace.dependencies]` table. Maps to
    /// `E_WORKSPACE_DEP_OUTSIDE_WORKSPACE`.
    WorkspaceDepOutsideWorkspace {
        manifest_dir: PathBuf,
        dep_name: String,
    },
    /// Walking path deps encountered a cycle. `chain` is in dependency
    /// order — the first and last entries are the same directory. Maps to
    /// `E_DEPENDENCY_CYCLE`.
    DependencyCycle { chain: Vec<PathBuf> },
    /// A path dep's directory doesn't exist or contains no `kara.toml`.
    /// Maps to `E_PATH_DEP_NOT_FOUND`.
    PathDepNotFound {
        from_dir: PathBuf,
        dep_name: String,
        target: PathBuf,
    },
    /// Offline-mode walk: a dependency's expected `vendor/<name>/` entry
    /// is missing or has no `kara.toml`. Maps to
    /// `E_OFFLINE_VENDOR_ENTRY_MISSING`. Distinct from `PathDepNotFound`
    /// because the operator action is "run `karac vendor`", not "fix the
    /// manifest path".
    OfflineVendorEntryMissing {
        from_dir: PathBuf,
        dep_name: String,
        expected: PathBuf,
    },
    /// Loading or parsing a transitive `kara.toml` failed. The underlying
    /// `ManifestError` is preserved for the diagnostic renderer (slice 5).
    /// Boxed because `ManifestError` carries multiple `PathBuf` fields per
    /// variant — without the box the enum's stack footprint trips
    /// `clippy::result_large_err`. Maps to `E_PATH_DEP_MANIFEST_INVALID`.
    PathDepManifestInvalid {
        from_dir: PathBuf,
        dep_name: String,
        source: Box<manifest::ManifestError>,
    },
    /// Fetching / materializing a registry dependency failed (catalog or
    /// tarball fetch, no matching version, or extraction). Maps to
    /// `E_REGISTRY_FETCH_FAILED`. Only produced when a `RegistryProvider`
    /// is configured; without one, registry deps surface later as
    /// `E_REGISTRY_DEP_UNSUPPORTED` from the resolver instead.
    RegistryFetchFailed {
        from_dir: PathBuf,
        dep_name: String,
        message: String,
    },
    /// Cloning / checking out a git dependency failed (clone, checkout, or a
    /// missing `kara.toml`). Maps to `E_GIT_FETCH_FAILED`. Only produced when
    /// a [`crate::git_fetch::GitProvider`] is configured; without one, git
    /// deps surface later as `E_GIT_DEP_UNSUPPORTED` from the resolver.
    GitFetchFailed {
        from_dir: PathBuf,
        dep_name: String,
        message: String,
    },
}

impl DepGraphError {
    /// Symbolic diagnostic code per the design.md error-code catalogue. The
    /// resolver's diagnostic renderer (slice 5) maps these into the
    /// structured-diagnostic output.
    pub fn code(&self) -> &'static str {
        match self {
            Self::WorkspaceDepNotDeclared { .. } => "E_WORKSPACE_DEP_NOT_DECLARED",
            Self::WorkspaceDepOutsideWorkspace { .. } => "E_WORKSPACE_DEP_OUTSIDE_WORKSPACE",
            Self::DependencyCycle { .. } => "E_DEPENDENCY_CYCLE",
            Self::PathDepNotFound { .. } => "E_PATH_DEP_NOT_FOUND",
            Self::OfflineVendorEntryMissing { .. } => "E_OFFLINE_VENDOR_ENTRY_MISSING",
            Self::PathDepManifestInvalid { .. } => "E_PATH_DEP_MANIFEST_INVALID",
            Self::RegistryFetchFailed { .. } => "E_REGISTRY_FETCH_FAILED",
            Self::GitFetchFailed { .. } => "E_GIT_FETCH_FAILED",
        }
    }
}

impl std::fmt::Display for DepGraphError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WorkspaceDepNotDeclared {
                manifest_dir,
                dep_name,
            } => write!(
                f,
                "`{}/kara.toml`: dependency `{}` uses `workspace = true` but `{}` is not declared in `[workspace.dependencies]`",
                manifest_dir.display(),
                dep_name,
                dep_name,
            ),
            Self::WorkspaceDepOutsideWorkspace {
                manifest_dir,
                dep_name,
            } => write!(
                f,
                "`{}/kara.toml`: dependency `{}` uses `workspace = true` but the entry-point manifest has no `[workspace.dependencies]` table",
                manifest_dir.display(),
                dep_name,
            ),
            Self::DependencyCycle { chain } => {
                write!(f, "dependency cycle detected: ")?;
                for (i, dir) in chain.iter().enumerate() {
                    if i > 0 {
                        write!(f, " → ")?;
                    }
                    write!(f, "`{}`", dir.display())?;
                }
                Ok(())
            }
            Self::PathDepNotFound {
                from_dir,
                dep_name,
                target,
            } => write!(
                f,
                "`{}/kara.toml`: path dependency `{}` points at `{}` but no `kara.toml` is found there",
                from_dir.display(),
                dep_name,
                target.display(),
            ),
            Self::OfflineVendorEntryMissing {
                from_dir,
                dep_name,
                expected,
            } => write!(
                f,
                "`{}/kara.toml`: offline build expected vendored dependency `{}` at `{}` but no `kara.toml` is found there — run `karac vendor` to populate it",
                from_dir.display(),
                dep_name,
                expected.display(),
            ),
            Self::PathDepManifestInvalid {
                from_dir,
                dep_name,
                source,
            } => write!(
                f,
                "`{}/kara.toml`: path dependency `{}`'s manifest failed to parse: {}",
                from_dir.display(),
                dep_name,
                source,
            ),
            Self::RegistryFetchFailed {
                from_dir,
                dep_name,
                message,
            } => write!(
                f,
                "`{}/kara.toml`: could not fetch registry dependency `{}`: {}",
                from_dir.display(),
                dep_name,
                message,
            ),
            Self::GitFetchFailed {
                from_dir,
                dep_name,
                message,
            } => write!(
                f,
                "`{}/kara.toml`: could not fetch git dependency `{}`: {}",
                from_dir.display(),
                dep_name,
                message,
            ),
        }
    }
}

/// Trait abstracting over the filesystem so the algorithm can be exercised
/// against an in-memory `BTreeMap<PathBuf, Manifest>` in tests without
/// touching `std::env::temp_dir()`. The production loader (`FsLoader`) wraps
/// `manifest::load_from_root`.
pub trait ManifestLoader {
    fn load(&self, dir: &Path) -> Result<Manifest, manifest::ManifestError>;
    fn manifest_exists(&self, dir: &Path) -> bool;
}

/// Production-side loader — reads `kara.toml` from disk via
/// `manifest::load_from_root`.
pub struct FsLoader;

impl ManifestLoader for FsLoader {
    fn load(&self, dir: &Path) -> Result<Manifest, manifest::ManifestError> {
        manifest::load_from_root(dir)
    }
    fn manifest_exists(&self, dir: &Path) -> bool {
        dir.join(manifest::MANIFEST_FILENAME).is_file()
    }
}

/// Materialize the graph rooted at `root_dir`'s manifest. `root_manifest`
/// is the already-parsed entry-point manifest; the function walks its path
/// deps via `loader` and dereferences any `Workspace` entries against
/// `root_manifest.workspace_dependencies`.
pub fn build_dep_graph(
    root_dir: &Path,
    root_manifest: Manifest,
    loader: &dyn ManifestLoader,
) -> Result<DepGraph, DepGraphError> {
    build_dep_graph_with_options(root_dir, root_manifest, loader, DepGraphOptions::default())
}

/// Offline-aware variant of [`build_dep_graph`]. When `offline_root` is
/// `Some(vendor_root)`, every transitive `DependencySpec::Path` is resolved
/// as `vendor_root.join(dep_name)` instead of the manifest-declared relative
/// path. Retained for compatibility with the line-880 offline slice; new
/// callers should prefer `build_dep_graph_with_options`.
pub fn build_dep_graph_with_offline(
    root_dir: &Path,
    root_manifest: Manifest,
    loader: &dyn ManifestLoader,
    offline_root: Option<&Path>,
) -> Result<DepGraph, DepGraphError> {
    build_dep_graph_with_options(
        root_dir,
        root_manifest,
        loader,
        DepGraphOptions {
            offline_root,
            include_dev_deps: false,
            registry_provider: None,
            git_provider: None,
            pins: None,
        },
    )
}

/// Optional knobs for [`build_dep_graph_with_options`]. New axes
/// land here so the entry-point shape can grow without bumping the
/// positional arg count past readability.
///
/// - `offline_root`: when `Some(vendor_root)`, every transitive
///   `DependencySpec::Path` is resolved as `vendor_root.join(dep_name)`
///   instead of the manifest-declared relative path (line 880).
/// - `include_dev_deps`: when `true`, the root manifest's
///   `[dev-dependencies]` participate in the walk alongside
///   `[dependencies]`. Transitive manifests' dev-deps are **not**
///   walked — Cargo's dev-deps-don't-propagate rule applies. Drives
///   the line-884 build-vs-test split: build mode passes `false`,
///   test mode passes `true`.
/// - `registry_provider`: when `Some`, the walk fetches + recurses into
///   `DependencySpec::Registry` deps through it (registry fetch epic slice
///   3). When `None`, registry deps are recorded but not walked, and the
///   resolver reports them as unsupported — the pre-fetch behavior.
#[derive(Clone, Copy, Default)]
pub struct DepGraphOptions<'a> {
    pub offline_root: Option<&'a Path>,
    pub include_dev_deps: bool,
    pub registry_provider: Option<&'a dyn RegistryProvider>,
    /// When `Some`, the walk clones + recurses into `DependencySpec::Git`
    /// deps through it (git-fetch slice 2). When `None`, git deps are
    /// recorded but not walked, and the resolver reports them as unsupported
    /// — the pre-fetch behavior. Independent of `registry_provider`: a git
    /// dep is direct-from-source, so it needs no proxy.
    pub git_provider: Option<&'a dyn crate::git_fetch::GitProvider>,
    /// Lockfile version pins (resolver follow-up (d)/(h)): package name → the
    /// version recorded in `kara.lock`. When set, a registry dep with a pin is
    /// fetched at *exactly* that version via `fetch_exact` (which, unlike fresh
    /// range selection, resolves even a yanked release — reproducing a lock must
    /// succeed), and the pinned version is added to the candidate set so the
    /// solver can honor it. `None` (the default) reproduces fresh selection.
    pub pins: Option<&'a BTreeMap<String, semver::Version>>,
}

/// Options-driven entry point. The two thin wrappers above
/// (`build_dep_graph`, `build_dep_graph_with_offline`) delegate here.
pub fn build_dep_graph_with_options(
    root_dir: &Path,
    root_manifest: Manifest,
    loader: &dyn ManifestLoader,
    options: DepGraphOptions<'_>,
) -> Result<DepGraph, DepGraphError> {
    let root_canonical = canonicalize_or_self(root_dir);
    let mut manifests = BTreeMap::new();
    let mut derived_deps = BTreeMap::new();
    let mut registry_resolutions = BTreeMap::new();
    let mut registry_candidates = BTreeMap::new();
    let mut git_resolutions = BTreeMap::new();

    let mut visiting_stack: Vec<PathBuf> = Vec::new();
    let mut visiting_set: HashSet<PathBuf> = HashSet::new();
    let mut visited: HashSet<PathBuf> = HashSet::new();

    // Determine the effective `[workspace.dependencies]` for `workspace = true`
    // derefs. When the entry-point manifest is itself a workspace root, use its
    // own table (the pre-follow-up-(g) behavior). Otherwise walk *upward* to
    // the nearest ancestor `kara.toml` declaring a `[workspace]` table and
    // inherit its shared deps — so building a *member* package resolves
    // `workspace = true` against the parent workspace root, per the spec's
    // actual workspace model (resolver follow-up (g)). No workspace-root
    // ancestor → empty, and a `workspace = true` dep then surfaces
    // `E_WORKSPACE_DEP_OUTSIDE_WORKSPACE` exactly as before.
    let workspace_deps = if root_manifest.is_workspace_root {
        root_manifest.workspace_dependencies.clone()
    } else {
        discover_ancestor_workspace_deps(&root_canonical, loader)
    };

    visit(
        &root_canonical,
        root_manifest,
        loader,
        &workspace_deps,
        options.offline_root,
        options.include_dev_deps,
        options.registry_provider,
        options.git_provider,
        options.pins,
        true,
        &mut manifests,
        &mut derived_deps,
        &mut registry_resolutions,
        &mut registry_candidates,
        &mut git_resolutions,
        &mut visiting_stack,
        &mut visiting_set,
        &mut visited,
    )?;

    // Record each fetched registry package's yanked version set (resolver
    // follow-up (h)). A post-walk pass over the already-collected registry
    // package names — no extra threading through `visit`, since
    // `registry_candidates` already holds every registry package the walk
    // fetched. Best-effort: a provider that can't enumerate (or has no yanked
    // set) contributes nothing, and a fetch error for one package is skipped
    // rather than failing the build — this is advisory warning data, never a
    // hard gate. Only non-empty sets are stored, so the map is empty whenever
    // nothing is yanked.
    let mut yanked_versions: BTreeMap<String, Vec<semver::Version>> = BTreeMap::new();
    if let Some(provider) = options.registry_provider {
        for name in registry_candidates.keys() {
            if let Ok(yanked) = provider.yanked_versions(name) {
                if !yanked.is_empty() {
                    yanked_versions.insert(name.clone(), yanked);
                }
            }
        }
    }

    Ok(DepGraph {
        root_dir: root_canonical,
        manifests,
        derived_deps,
        registry_resolutions,
        registry_candidates,
        git_resolutions,
        yanked_versions,
    })
}

#[allow(clippy::too_many_arguments)]
fn visit(
    dir: &Path,
    manifest_doc: Manifest,
    loader: &dyn ManifestLoader,
    workspace_deps: &BTreeMap<String, DependencySpec>,
    offline_root: Option<&Path>,
    include_dev_deps: bool,
    registry_provider: Option<&dyn RegistryProvider>,
    git_provider: Option<&dyn crate::git_fetch::GitProvider>,
    pins: Option<&BTreeMap<String, semver::Version>>,
    is_root: bool,
    manifests: &mut BTreeMap<PathBuf, Manifest>,
    derived_deps: &mut BTreeMap<PathBuf, BTreeMap<String, DependencySpec>>,
    registry_resolutions: &mut BTreeMap<(PathBuf, String), RegistryResolution>,
    registry_candidates: &mut BTreeMap<String, Vec<RegistryCandidate>>,
    git_resolutions: &mut BTreeMap<(PathBuf, String), GitResolution>,
    visiting_stack: &mut Vec<PathBuf>,
    visiting_set: &mut HashSet<PathBuf>,
    visited: &mut HashSet<PathBuf>,
) -> Result<(), DepGraphError> {
    visiting_stack.push(dir.to_path_buf());
    visiting_set.insert(dir.to_path_buf());

    // Derive the manifest's effective dependencies — every `Workspace`
    // entry replaced with the corresponding entry from `workspace_deps`.
    // Dev-deps participate only at the root and only when explicitly
    // requested by the caller (test-mode resolution, tracker line 884).
    // Cargo's "dev-deps don't propagate" rule: a transitive dep's own
    // dev-deps never affect the parent build, even in test mode.
    let mut derived = BTreeMap::new();
    for (name, spec) in &manifest_doc.dependencies {
        let resolved = deref_workspace(dir, name, spec, workspace_deps)?;
        derived.insert(name.clone(), resolved);
    }
    if is_root && include_dev_deps {
        for (name, spec) in &manifest_doc.dev_dependencies {
            // A name appearing in both base and dev tables is unusual but
            // allowed — the dev entry wins, matching Cargo's behavior.
            let resolved = deref_workspace(dir, name, spec, workspace_deps)?;
            derived.insert(name.clone(), resolved);
        }
    }
    derived_deps.insert(dir.to_path_buf(), derived.clone());
    manifests.insert(dir.to_path_buf(), manifest_doc);

    // Recurse into each path dep.
    let derived_owned: Vec<(String, DependencySpec)> = derived.into_iter().collect();
    for (dep_name, spec) in &derived_owned {
        let DependencySpec::Path { path, .. } = spec else {
            continue;
        };
        // Offline mode redirects every transitive path-dep at the flat
        // `vendor/<dep-name>/` layout produced by `karac vendor`. A missing
        // vendor entry surfaces the offline-specific diagnostic below; the
        // resolver later treats the vendored manifest as the source of
        // truth, so version mismatches between the manifest-declared path
        // and the vendored copy don't apply.
        let target = match offline_root {
            Some(vendor_root) => vendor_root.join(dep_name),
            None => resolve_path_dep_dir(dir, path),
        };
        let canonical_target = canonicalize_or_self(&target);

        // Cycle detection runs *before* the loader is consulted: a back-edge
        // to an in-flight node is a cycle, regardless of whether the loader
        // could otherwise find its manifest (the root manifest isn't in the
        // loader at all, for instance).
        if visiting_set.contains(&canonical_target) {
            let cycle_start = visiting_stack
                .iter()
                .position(|p| p == &canonical_target)
                .unwrap_or(0);
            let mut chain: Vec<PathBuf> = visiting_stack[cycle_start..].to_vec();
            chain.push(canonical_target);
            return Err(DepGraphError::DependencyCycle { chain });
        }
        if visited.contains(&canonical_target) {
            continue;
        }

        if !loader.manifest_exists(&canonical_target) {
            return Err(if offline_root.is_some() {
                DepGraphError::OfflineVendorEntryMissing {
                    from_dir: dir.to_path_buf(),
                    dep_name: dep_name.clone(),
                    expected: canonical_target,
                }
            } else {
                DepGraphError::PathDepNotFound {
                    from_dir: dir.to_path_buf(),
                    dep_name: dep_name.clone(),
                    target: canonical_target,
                }
            });
        }
        let child_manifest =
            loader
                .load(&canonical_target)
                .map_err(|e| DepGraphError::PathDepManifestInvalid {
                    from_dir: dir.to_path_buf(),
                    dep_name: dep_name.clone(),
                    source: Box::new(e),
                })?;
        visit(
            &canonical_target,
            child_manifest,
            loader,
            workspace_deps,
            offline_root,
            include_dev_deps,
            registry_provider,
            git_provider,
            pins,
            // Transitive children are never the root — dev-deps stop
            // propagating here even if the root opted them in.
            false,
            manifests,
            derived_deps,
            registry_resolutions,
            registry_candidates,
            git_resolutions,
            visiting_stack,
            visiting_set,
            visited,
        )?;
    }

    // Recurse into each registry dep — but only when a provider is
    // configured. Without one, registry deps stay recorded-only and the
    // resolver reports `E_REGISTRY_DEP_UNSUPPORTED`, preserving the
    // pre-fetch behavior. Offline mode never fetches (a registry dep must
    // be vendored as a path-dep to build offline), so it is skipped too.
    if let (Some(provider), None) = (registry_provider, offline_root) {
        for (dep_name, spec) in &derived_owned {
            let DependencySpec::Registry { version: req } = spec else {
                continue;
            };
            // A lockfile pin fetches EXACTLY the recorded version via
            // `fetch_exact` (not yanked-filtered — reproducing a lock must
            // succeed, even for a since-yanked release); an unpinned dep does
            // fresh range selection via `fetch` (which excludes yanked).
            let pinned = pins.and_then(|p| p.get(dep_name));
            let materialized = match pinned {
                Some(v) => provider.fetch_exact(dep_name, v),
                None => provider.fetch(dep_name, req),
            }
            .map_err(|message| DepGraphError::RegistryFetchFailed {
                from_dir: dir.to_path_buf(),
                dep_name: dep_name.clone(),
                message,
            })?;
            let canonical_target = canonicalize_or_self(&materialized.root_dir);
            registry_resolutions.insert(
                (dir.to_path_buf(), dep_name.clone()),
                RegistryResolution {
                    dir: canonical_target.clone(),
                    version: materialized.version.clone(),
                    upstream_url: materialized.upstream_url.clone(),
                },
            );

            // Record this package's full candidate set (all published
            // versions + their per-version registry deps) once per package
            // *name* — the widened input for PubGrub's global solve (slice
            // 3c). The candidate set is req-independent, so recording it once
            // suffices no matter how many parents declare the package. This is
            // pure data collection; the selected-version fetch/recurse above
            // is unchanged, so the walk's shape (and today's resolution) is
            // preserved.
            if !registry_candidates.contains_key(dep_name) {
                let mut candidates = record_registry_candidates(provider, loader, dep_name);
                // A pinned version may be absent from `available_versions` (it
                // was yanked): add it as a candidate so the solver can honor the
                // lock. Its deps come from its own just-fetched manifest at
                // `canonical_target` (the `fetch_exact` above materialized it).
                if let Some(pinned_v) = pinned {
                    if !candidates.iter().any(|c| &c.version == pinned_v) {
                        let deps = loader
                            .load(&canonical_target)
                            .map(|m| registry_deps_of(&m))
                            .unwrap_or_default();
                        candidates.push(RegistryCandidate {
                            version: pinned_v.clone(),
                            deps,
                        });
                    }
                }
                registry_candidates.insert(dep_name.clone(), candidates);
            }

            // Same cycle / dedup discipline as path-deps, keyed by the
            // extracted source dir.
            if visiting_set.contains(&canonical_target) {
                let cycle_start = visiting_stack
                    .iter()
                    .position(|p| p == &canonical_target)
                    .unwrap_or(0);
                let mut chain: Vec<PathBuf> = visiting_stack[cycle_start..].to_vec();
                chain.push(canonical_target);
                return Err(DepGraphError::DependencyCycle { chain });
            }
            if visited.contains(&canonical_target) {
                continue;
            }
            let child_manifest = loader.load(&canonical_target).map_err(|e| {
                DepGraphError::PathDepManifestInvalid {
                    from_dir: dir.to_path_buf(),
                    dep_name: dep_name.clone(),
                    source: Box::new(e),
                }
            })?;
            visit(
                &canonical_target,
                child_manifest,
                loader,
                workspace_deps,
                offline_root,
                include_dev_deps,
                registry_provider,
                git_provider,
                pins,
                false,
                manifests,
                derived_deps,
                registry_resolutions,
                registry_candidates,
                git_resolutions,
                visiting_stack,
                visiting_set,
                visited,
            )?;
        }
    }

    // Recurse into each git dep — same provider-gated, offline-skipped
    // discipline as registry deps, but keyed on the cloned checkout dir.
    // A git dep is direct-from-source, so it is orthogonal to the proxy;
    // the provider is active whenever the caller isn't offline.
    if let (Some(provider), None) = (git_provider, offline_root) {
        for (dep_name, spec) in &derived_owned {
            let DependencySpec::Git { url, reference, .. } = spec else {
                continue;
            };
            let materialized = provider.fetch(url, reference.as_ref()).map_err(|e| {
                DepGraphError::GitFetchFailed {
                    from_dir: dir.to_path_buf(),
                    dep_name: dep_name.clone(),
                    message: e.to_string(),
                }
            })?;
            let canonical_target = canonicalize_or_self(&materialized.root_dir);
            git_resolutions.insert(
                (dir.to_path_buf(), dep_name.clone()),
                GitResolution {
                    dir: canonical_target.clone(),
                    url: url.clone(),
                    reference: reference.clone(),
                    resolved_rev: materialized.resolved_rev.clone(),
                },
            );

            if visiting_set.contains(&canonical_target) {
                let cycle_start = visiting_stack
                    .iter()
                    .position(|p| p == &canonical_target)
                    .unwrap_or(0);
                let mut chain: Vec<PathBuf> = visiting_stack[cycle_start..].to_vec();
                chain.push(canonical_target);
                return Err(DepGraphError::DependencyCycle { chain });
            }
            if visited.contains(&canonical_target) {
                continue;
            }
            let child_manifest = loader.load(&canonical_target).map_err(|e| {
                DepGraphError::PathDepManifestInvalid {
                    from_dir: dir.to_path_buf(),
                    dep_name: dep_name.clone(),
                    source: Box::new(e),
                }
            })?;
            visit(
                &canonical_target,
                child_manifest,
                loader,
                workspace_deps,
                offline_root,
                include_dev_deps,
                registry_provider,
                git_provider,
                pins,
                false,
                manifests,
                derived_deps,
                registry_resolutions,
                registry_candidates,
                git_resolutions,
                visiting_stack,
                visiting_set,
                visited,
            )?;
        }
    }

    visiting_set.remove(dir);
    visiting_stack.pop();
    visited.insert(dir.to_path_buf());
    Ok(())
}

/// Dereference a `DependencySpec::Workspace` against the workspace-level
/// `[workspace.dependencies]` table. Non-workspace specs pass through
/// unchanged.
fn deref_workspace(
    manifest_dir: &Path,
    dep_name: &str,
    spec: &DependencySpec,
    workspace_deps: &BTreeMap<String, DependencySpec>,
) -> Result<DependencySpec, DepGraphError> {
    if !matches!(spec, DependencySpec::Workspace) {
        return Ok(spec.clone());
    }
    if workspace_deps.is_empty() {
        return Err(DepGraphError::WorkspaceDepOutsideWorkspace {
            manifest_dir: manifest_dir.to_path_buf(),
            dep_name: dep_name.to_string(),
        });
    }
    workspace_deps
        .get(dep_name)
        .cloned()
        .ok_or_else(|| DepGraphError::WorkspaceDepNotDeclared {
            manifest_dir: manifest_dir.to_path_buf(),
            dep_name: dep_name.to_string(),
        })
}

fn resolve_path_dep_dir(from_dir: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        from_dir.join(path)
    }
}

/// Walk upward from `start_dir`'s parent to the filesystem root, returning the
/// `[workspace.dependencies]` of the nearest ancestor `kara.toml` that declares
/// a `[workspace]` table (empty when there is none). This implements the spec's
/// workspace model: a member package's `workspace = true` deps are declared
/// once in a *parent* workspace root, not in the member's own manifest
/// (resolver follow-up (g)). Non-workspace-root ancestor manifests are passed
/// over — a regular package sitting between the member and the root doesn't
/// stop the walk — and a malformed ancestor manifest is skipped rather than
/// failing the member build. `start_dir` is expected canonical so
/// `Path::ancestors` yields real parent directories.
fn discover_ancestor_workspace_deps(
    start_dir: &Path,
    loader: &dyn ManifestLoader,
) -> BTreeMap<String, DependencySpec> {
    for ancestor in start_dir.ancestors().skip(1) {
        if !loader.manifest_exists(ancestor) {
            continue;
        }
        if let Ok(mf) = loader.load(ancestor) {
            if mf.is_workspace_root {
                return mf.workspace_dependencies;
            }
        }
    }
    BTreeMap::new()
}

/// The registry-dep `(name, constraint)` edges a manifest declares — the
/// per-version dependency data PubGrub needs to solve. Path/git deps of a
/// published package are outside the v1 version-solve domain and are excluded.
fn registry_deps_of(manifest: &Manifest) -> Vec<(String, semver::VersionReq)> {
    manifest
        .dependencies
        .iter()
        .filter_map(|(name, spec)| match spec {
            DependencySpec::Registry { version: req } => Some((name.clone(), req.clone())),
            _ => None,
        })
        .collect()
}

/// Collect a registry package's full candidate set: every selectable
/// published version (via [`RegistryProvider::available_versions`]) paired
/// with that version's own registry-dep requirements (read from the version's
/// materialized manifest via [`RegistryProvider::fetch_exact`]). The widened
/// input for PubGrub's global solve (resolver follow-up (a) slice 3c).
///
/// **Best-effort.** This data is advisory for the solver — the selected
/// version fetched + recursed by the caller owns build correctness — so a
/// failure never propagates: a provider that can't enumerate yields an empty
/// set (the solver falls back to the single selected candidate), and any
/// individual version whose tarball/manifest can't be fetched or parsed is
/// dropped rather than failing the build.
///
/// **Cost.** This eagerly materializes every published version's manifest, so
/// a package with N releases incurs N fetches (cached by the production
/// provider's tarball cache). Slice 3d / a later optimization can make this
/// lazy by fetching a candidate's deps only when the solver actually
/// considers it.
fn record_registry_candidates(
    provider: &dyn RegistryProvider,
    loader: &dyn ManifestLoader,
    name: &str,
) -> Vec<RegistryCandidate> {
    let Ok(versions) = provider.available_versions(name) else {
        return Vec::new();
    };
    let mut candidates = Vec::new();
    for version in versions {
        let Ok(materialized) = provider.fetch_exact(name, &version) else {
            continue;
        };
        let dir = canonicalize_or_self(&materialized.root_dir);
        let Ok(manifest) = loader.load(&dir) else {
            continue;
        };
        candidates.push(RegistryCandidate {
            version,
            deps: registry_deps_of(&manifest),
        });
    }
    candidates
}

/// Canonicalize a directory path if the OS permits — otherwise fall back to
/// the input path. Canonicalization unifies `./a/../b` with `b`, preventing
/// double-visits via path aliasing; when it fails (e.g. the path doesn't
/// exist yet — surfaced separately as `PathDepNotFound`), the raw form is
/// good enough.
fn canonicalize_or_self(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use semver::VersionReq;

    /// In-memory loader for tests. Indexed by directory; `load` returns the
    /// pre-baked manifest, `manifest_exists` just checks key presence.
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

    /// Convenience wrapper: build the graph with `include_dev_deps`
    /// activated. Used by the line-884 tests below.
    fn build_test_mode_graph(
        root_dir: &Path,
        root_manifest: Manifest,
        loader: &dyn ManifestLoader,
    ) -> Result<DepGraph, DepGraphError> {
        build_dep_graph_with_options(
            root_dir,
            root_manifest,
            loader,
            DepGraphOptions {
                offline_root: None,
                include_dev_deps: true,
                registry_provider: None,
                git_provider: None,
                pins: None,
            },
        )
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
            release_cpu_baseline: None,
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

    fn v(s: &str) -> semver::Version {
        semver::Version::parse(s).unwrap()
    }

    /// In-memory `RegistryProvider`: maps a package name to a canned
    /// extracted dir + version + upstream URL. The dirs also key `MemLoader`.
    struct MockRegistryProvider {
        fetches: BTreeMap<String, (PathBuf, semver::Version, String)>,
    }

    impl RegistryProvider for MockRegistryProvider {
        fn fetch(&self, name: &str, _req: &VersionReq) -> Result<MaterializedDep, String> {
            self.fetches
                .get(name)
                .map(|(dir, ver, up)| MaterializedDep {
                    root_dir: dir.clone(),
                    version: ver.clone(),
                    upstream_url: up.clone(),
                })
                .ok_or_else(|| format!("no such package {name}"))
        }
    }

    #[test]
    fn registry_dep_fetched_and_recursed_with_provider() {
        let mut root = empty_manifest("app");
        root.dependencies.insert("http".into(), registry("^1.0"));
        // The fetched `http` package declares its own registry dep on `log`.
        let mut http_mf = empty_manifest("http");
        http_mf.dependencies.insert("log".into(), registry("^0.4"));
        let log_mf = empty_manifest("log");

        let loader = MemLoader {
            manifests: BTreeMap::from([
                (PathBuf::from("/reg/http"), http_mf),
                (PathBuf::from("/reg/log"), log_mf),
            ]),
        };
        let provider = MockRegistryProvider {
            fetches: BTreeMap::from([
                (
                    "http".to_string(),
                    (
                        PathBuf::from("/reg/http"),
                        v("1.2.0"),
                        "https://up/http".to_string(),
                    ),
                ),
                (
                    "log".to_string(),
                    (
                        PathBuf::from("/reg/log"),
                        v("0.4.1"),
                        "https://up/log".to_string(),
                    ),
                ),
            ]),
        };
        let graph = build_dep_graph_with_options(
            &PathBuf::from("/app"),
            root,
            &loader,
            DepGraphOptions {
                offline_root: None,
                include_dev_deps: false,
                registry_provider: Some(&provider),
                git_provider: None,
                pins: None,
            },
        )
        .expect("build");

        // Both fetched packages — including the transitive `log` — are in the
        // graph's manifests.
        assert!(graph.manifests.contains_key(&PathBuf::from("/reg/http")));
        assert!(graph.manifests.contains_key(&PathBuf::from("/reg/log")));

        let http_res = graph
            .registry_resolutions
            .get(&(PathBuf::from("/app"), "http".to_string()))
            .expect("http resolution");
        assert_eq!(http_res.version, v("1.2.0"));
        assert_eq!(http_res.upstream_url, "https://up/http");

        // Transitive: `http`'s manifest dir declared `log`.
        let log_res = graph
            .registry_resolutions
            .get(&(PathBuf::from("/reg/http"), "log".to_string()))
            .expect("log resolution");
        assert_eq!(log_res.version, v("0.4.1"));

        // Slice 3c: this provider only implements `fetch` (available_versions
        // defaults to empty), so the candidate set is recorded-but-empty — the
        // solver falls back to the single selected version.
        assert_eq!(
            graph.registry_candidates.get("http"),
            Some(&Vec::new()),
            "non-enumerating provider must record an empty candidate set"
        );
    }

    /// In-memory `RegistryProvider` that also enumerates versions and fetches
    /// exact ones — so the slice-3c candidate-set recording can be exercised.
    /// `selected` is what range `fetch` returns; `exact` maps each published
    /// version to its own extracted dir (keying `MemLoader`).
    struct MultiVersionMockProvider {
        versions: BTreeMap<String, Vec<semver::Version>>,
        selected: BTreeMap<String, (PathBuf, semver::Version)>,
        exact: BTreeMap<(String, semver::Version), PathBuf>,
    }

    impl RegistryProvider for MultiVersionMockProvider {
        fn fetch(&self, name: &str, _req: &VersionReq) -> Result<MaterializedDep, String> {
            self.selected
                .get(name)
                .map(|(dir, ver)| MaterializedDep {
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
        ) -> Result<MaterializedDep, String> {
            self.exact
                .get(&(name.to_string(), version.clone()))
                .map(|dir| MaterializedDep {
                    root_dir: dir.clone(),
                    version: version.clone(),
                    upstream_url: format!("https://up/{name}"),
                })
                .ok_or_else(|| format!("no {name}@{version}"))
        }
    }

    #[test]
    fn yanked_versions_recorded_from_provider() {
        // A provider that reports `http`'s 1.1.0 as yanked → the graph records
        // it in `yanked_versions` so a post-resolution audit can flag a pin on
        // it (follow-up (h)). Fresh selection (`available_versions`) still
        // excludes the yanked version.
        struct YankAwareProvider;
        impl RegistryProvider for YankAwareProvider {
            fn fetch(&self, name: &str, _req: &VersionReq) -> Result<MaterializedDep, String> {
                Ok(MaterializedDep {
                    root_dir: PathBuf::from("/reg/http-1.0.0"),
                    version: v("1.0.0"),
                    upstream_url: format!("https://up/{name}"),
                })
            }
            fn available_versions(&self, _name: &str) -> Result<Vec<semver::Version>, String> {
                Ok(vec![v("1.0.0")]) // 1.1.0 is yanked, so excluded here
            }
            fn fetch_exact(
                &self,
                name: &str,
                version: &semver::Version,
            ) -> Result<MaterializedDep, String> {
                Ok(MaterializedDep {
                    root_dir: PathBuf::from("/reg/http-1.0.0"),
                    version: version.clone(),
                    upstream_url: format!("https://up/{name}"),
                })
            }
            fn yanked_versions(&self, name: &str) -> Result<Vec<semver::Version>, String> {
                if name == "http" {
                    Ok(vec![v("1.1.0")])
                } else {
                    Ok(vec![])
                }
            }
        }

        let mut root = empty_manifest("app");
        root.dependencies.insert("http".into(), registry("^1.0"));
        let loader = MemLoader {
            manifests: BTreeMap::from([(PathBuf::from("/reg/http-1.0.0"), empty_manifest("http"))]),
        };
        let graph = build_dep_graph_with_options(
            &PathBuf::from("/app"),
            root,
            &loader,
            DepGraphOptions {
                offline_root: None,
                include_dev_deps: false,
                registry_provider: Some(&YankAwareProvider),
                git_provider: None,
                pins: None,
            },
        )
        .expect("graph");
        assert_eq!(
            graph.yanked_versions.get("http"),
            Some(&vec![v("1.1.0")]),
            "the provider's yanked set is recorded per package name"
        );
    }

    #[test]
    fn registry_candidate_set_recorded_with_per_version_deps() {
        // `http` publishes three versions, each declaring a different `log`
        // constraint. The walk selects 1.1.0 (highest `^1.0`) and recurses
        // into it, but slice 3c also records the *full* candidate set — every
        // version paired with that version's own registry deps — so PubGrub
        // (slice 3d) can backtrack.
        let mut root = empty_manifest("app");
        root.dependencies.insert("http".into(), registry("^1.0"));

        let mut http_10 = empty_manifest("http");
        http_10.dependencies.insert("log".into(), registry("^0.3"));
        let mut http_11 = empty_manifest("http");
        http_11.dependencies.insert("log".into(), registry("^0.4"));
        let mut http_20 = empty_manifest("http");
        http_20.dependencies.insert("log".into(), registry("^0.5"));

        let loader = MemLoader {
            manifests: BTreeMap::from([
                (PathBuf::from("/reg/http-1.0.0"), http_10),
                (PathBuf::from("/reg/http-1.1.0"), http_11),
                (PathBuf::from("/reg/http-2.0.0"), http_20),
                (PathBuf::from("/reg/log"), empty_manifest("log")),
            ]),
        };
        let provider = MultiVersionMockProvider {
            versions: BTreeMap::from([
                ("http".to_string(), vec![v("1.0.0"), v("1.1.0"), v("2.0.0")]),
                ("log".to_string(), vec![v("0.4.1")]),
            ]),
            selected: BTreeMap::from([
                (
                    "http".to_string(),
                    (PathBuf::from("/reg/http-1.1.0"), v("1.1.0")),
                ),
                ("log".to_string(), (PathBuf::from("/reg/log"), v("0.4.1"))),
            ]),
            exact: BTreeMap::from([
                (
                    ("http".to_string(), v("1.0.0")),
                    PathBuf::from("/reg/http-1.0.0"),
                ),
                (
                    ("http".to_string(), v("1.1.0")),
                    PathBuf::from("/reg/http-1.1.0"),
                ),
                (
                    ("http".to_string(), v("2.0.0")),
                    PathBuf::from("/reg/http-2.0.0"),
                ),
                (("log".to_string(), v("0.4.1")), PathBuf::from("/reg/log")),
            ]),
        };

        let graph = build_dep_graph_with_options(
            &PathBuf::from("/app"),
            root,
            &loader,
            DepGraphOptions {
                offline_root: None,
                include_dev_deps: false,
                registry_provider: Some(&provider),
                git_provider: None,
                pins: None,
            },
        )
        .expect("build");

        // Selected-version behavior is unchanged: the walk still pinned 1.1.0.
        let http_res = graph
            .registry_resolutions
            .get(&(PathBuf::from("/app"), "http".to_string()))
            .expect("http resolution");
        assert_eq!(http_res.version, v("1.1.0"));

        // The widened candidate set: all three versions, each with its own
        // `log` constraint.
        let http_candidates = graph
            .registry_candidates
            .get("http")
            .expect("http candidates");
        let got: Vec<(semver::Version, Vec<(String, VersionReq)>)> = http_candidates
            .iter()
            .map(|c| (c.version.clone(), c.deps.clone()))
            .collect();
        assert_eq!(
            got,
            vec![
                (
                    v("1.0.0"),
                    vec![("log".to_string(), VersionReq::parse("^0.3").unwrap())]
                ),
                (
                    v("1.1.0"),
                    vec![("log".to_string(), VersionReq::parse("^0.4").unwrap())]
                ),
                (
                    v("2.0.0"),
                    vec![("log".to_string(), VersionReq::parse("^0.5").unwrap())]
                ),
            ]
        );

        // The leaf `log` package's candidate set is recorded too (one version,
        // no deps).
        let log_candidates = graph
            .registry_candidates
            .get("log")
            .expect("log candidates");
        assert_eq!(log_candidates.len(), 1);
        assert_eq!(log_candidates[0].version, v("0.4.1"));
        assert!(log_candidates[0].deps.is_empty());
    }

    #[test]
    fn registry_dep_without_provider_is_recorded_not_fetched() {
        let mut root = empty_manifest("app");
        root.dependencies.insert("http".into(), registry("^1.0"));
        let loader = MemLoader {
            manifests: BTreeMap::new(),
        };
        let graph = build_dep_graph(&PathBuf::from("/app"), root, &loader).expect("build");
        // The dep is recorded but never fetched — no provider was given.
        assert!(graph.derived_deps[&PathBuf::from("/app")].contains_key("http"));
        assert!(graph.registry_resolutions.is_empty());
        // No provider → no candidate set recorded either (slice 3c).
        assert!(graph.registry_candidates.is_empty());
    }

    #[test]
    fn registry_fetch_failure_surfaces_diagnostic() {
        let mut root = empty_manifest("app");
        root.dependencies.insert("ghost".into(), registry("^1.0"));
        let loader = MemLoader {
            manifests: BTreeMap::new(),
        };
        // Provider knows nothing about `ghost`.
        let provider = MockRegistryProvider {
            fetches: BTreeMap::new(),
        };
        let err = build_dep_graph_with_options(
            &PathBuf::from("/app"),
            root,
            &loader,
            DepGraphOptions {
                offline_root: None,
                include_dev_deps: false,
                registry_provider: Some(&provider),
                git_provider: None,
                pins: None,
            },
        )
        .unwrap_err();
        assert_eq!(err.code(), "E_REGISTRY_FETCH_FAILED");
    }

    #[test]
    fn solo_manifest_yields_single_entry_graph() {
        let mut root = empty_manifest("solo");
        root.dependencies.insert("http".into(), registry("1.0"));
        let loader = MemLoader {
            manifests: BTreeMap::new(),
        };
        let graph = build_dep_graph(&PathBuf::from("/solo"), root, &loader).expect("build");
        assert_eq!(graph.manifests.len(), 1);
        assert!(graph.manifests.contains_key(&PathBuf::from("/solo")));
        let derived = &graph.derived_deps[&PathBuf::from("/solo")];
        assert_eq!(derived.get("http"), Some(&registry("1.0")));
    }

    #[test]
    fn workspace_deref_replaces_workspace_entry() {
        let mut root = empty_manifest("root");
        root.dependencies
            .insert("http".into(), DependencySpec::Workspace);
        root.workspace_dependencies
            .insert("http".into(), registry("1.5"));
        // A manifest declaring `[workspace.dependencies]` is a workspace root —
        // the parser sets this whenever a `[workspace]` table is present.
        root.is_workspace_root = true;
        let loader = MemLoader {
            manifests: BTreeMap::new(),
        };
        let graph = build_dep_graph(&PathBuf::from("/root"), root, &loader).expect("build");
        let derived = &graph.derived_deps[&PathBuf::from("/root")];
        assert_eq!(derived.get("http"), Some(&registry("1.5")));
    }

    #[test]
    fn workspace_deref_missing_declaration_errors() {
        let mut root = empty_manifest("root");
        root.dependencies
            .insert("http".into(), DependencySpec::Workspace);
        root.workspace_dependencies
            .insert("json".into(), registry("1.0"));
        root.is_workspace_root = true;
        let loader = MemLoader {
            manifests: BTreeMap::new(),
        };
        let err = build_dep_graph(&PathBuf::from("/root"), root, &loader).unwrap_err();
        match err {
            DepGraphError::WorkspaceDepNotDeclared { dep_name, .. } => {
                assert_eq!(dep_name, "http");
            }
            other => panic!("expected WorkspaceDepNotDeclared, got {other:?}"),
        }
    }

    #[test]
    fn workspace_form_outside_workspace_errors() {
        // No [workspace.dependencies] declared anywhere → `workspace = true`
        // is a hard error per design.md.
        let mut root = empty_manifest("root");
        root.dependencies
            .insert("http".into(), DependencySpec::Workspace);
        let loader = MemLoader {
            manifests: BTreeMap::new(),
        };
        let err = build_dep_graph(&PathBuf::from("/root"), root, &loader).unwrap_err();
        assert!(matches!(
            err,
            DepGraphError::WorkspaceDepOutsideWorkspace { .. }
        ));
        assert_eq!(err.code(), "E_WORKSPACE_DEP_OUTSIDE_WORKSPACE");
    }

    #[test]
    fn path_dep_walks_transitively() {
        // root → A → B; all three should appear in the graph.
        let mut root = empty_manifest("root");
        root.dependencies.insert("a".into(), path_dep("a"));
        let mut a = empty_manifest("a");
        a.dependencies.insert("b".into(), path_dep("b"));
        let b = empty_manifest("b");
        let loader = MemLoader {
            manifests: BTreeMap::from([
                (PathBuf::from("/root/a"), a),
                (PathBuf::from("/root/a/b"), b),
            ]),
        };
        let graph = build_dep_graph(&PathBuf::from("/root"), root, &loader).expect("build");
        assert_eq!(graph.manifests.len(), 3);
        assert!(graph.manifests.contains_key(&PathBuf::from("/root")));
        assert!(graph.manifests.contains_key(&PathBuf::from("/root/a")));
        assert!(graph.manifests.contains_key(&PathBuf::from("/root/a/b")));
    }

    #[test]
    fn path_dep_cycle_detected() {
        // root → a → root  (root points back to itself via path = "..")
        let mut root = empty_manifest("root");
        root.dependencies.insert("a".into(), path_dep("a"));
        let mut a = empty_manifest("a");
        a.dependencies.insert("root".into(), path_dep("/root"));
        let loader = MemLoader {
            manifests: BTreeMap::from([(PathBuf::from("/root/a"), a)]),
        };
        let err = build_dep_graph(&PathBuf::from("/root"), root, &loader).unwrap_err();
        match err {
            DepGraphError::DependencyCycle { chain } => {
                assert_eq!(chain.first(), Some(&PathBuf::from("/root")));
                assert_eq!(chain.last(), Some(&PathBuf::from("/root")));
                // Chain visibly closes: first == last.
                assert!(chain.len() >= 3);
            }
            other => panic!("expected DependencyCycle, got {other:?}"),
        }
    }

    #[test]
    fn path_dep_self_cycle_detected() {
        // a → a (a's own manifest points at itself via `path = "."`).
        // `Path::components()` skips `CurDir` segments, so `/root/a/.` and
        // `/root/a` compare equal via `PartialEq` — the cycle detector catches
        // the self-reference even without fs canonicalization. The reported
        // chain shows the back-edge in its raw textual form (`/root/a/.`) so
        // a reader can trace exactly which dep entry produced the loop.
        let mut root = empty_manifest("root");
        root.dependencies.insert("a".into(), path_dep("a"));
        let mut a = empty_manifest("a");
        a.dependencies.insert("a_self".into(), path_dep("."));
        let loader = MemLoader {
            manifests: BTreeMap::from([(PathBuf::from("/root/a"), a)]),
        };
        let err = build_dep_graph(&PathBuf::from("/root"), root, &loader).unwrap_err();
        match err {
            DepGraphError::DependencyCycle { chain } => {
                assert_eq!(chain.first(), Some(&PathBuf::from("/root/a")));
                // Last entry is the raw `.`-joined form; equal under
                // `Path::components()` but byte-distinct.
                assert_eq!(chain.last(), Some(&PathBuf::from("/root/a/.")));
            }
            other => panic!("expected DependencyCycle, got {other:?}"),
        }
    }

    #[test]
    fn path_dep_missing_kara_toml_errors() {
        let mut root = empty_manifest("root");
        root.dependencies.insert("a".into(), path_dep("missing"));
        let loader = MemLoader {
            manifests: BTreeMap::new(),
        };
        let err = build_dep_graph(&PathBuf::from("/root"), root, &loader).unwrap_err();
        assert_eq!(err.code(), "E_PATH_DEP_NOT_FOUND");
        match err {
            DepGraphError::PathDepNotFound { dep_name, .. } => {
                assert_eq!(dep_name, "a");
            }
            other => panic!("expected PathDepNotFound, got {other:?}"),
        }
    }

    #[test]
    fn registry_dep_stops_recursion() {
        // root has only a registry dep — no recursion into anything;
        // graph contains only the root.
        let mut root = empty_manifest("root");
        root.dependencies.insert("http".into(), registry("1.0"));
        let loader = MemLoader {
            manifests: BTreeMap::new(),
        };
        let graph = build_dep_graph(&PathBuf::from("/root"), root, &loader).expect("build");
        assert_eq!(graph.manifests.len(), 1);
    }

    #[test]
    fn diamond_dependency_visits_shared_node_once() {
        // root → a → shared
        // root → b → shared
        // Graph should contain four entries, not five (shared appears once).
        let mut root = empty_manifest("root");
        root.dependencies.insert("a".into(), path_dep("a"));
        root.dependencies.insert("b".into(), path_dep("b"));
        let mut a = empty_manifest("a");
        a.dependencies.insert("shared".into(), path_dep("/shared"));
        let mut b = empty_manifest("b");
        b.dependencies.insert("shared".into(), path_dep("/shared"));
        let shared = empty_manifest("shared");
        let loader = MemLoader {
            manifests: BTreeMap::from([
                (PathBuf::from("/root/a"), a),
                (PathBuf::from("/root/b"), b),
                (PathBuf::from("/shared"), shared),
            ]),
        };
        let graph = build_dep_graph(&PathBuf::from("/root"), root, &loader).expect("build");
        assert_eq!(graph.manifests.len(), 4);
        assert!(graph.manifests.contains_key(&PathBuf::from("/shared")));
    }

    #[test]
    fn child_manifest_parse_failure_surfaces_path_dep_invalid() {
        // Simulate a child manifest that fails to load — the in-memory
        // loader uses `NotInsideKaraProject` as its failure path, which
        // wraps cleanly through `PathDepManifestInvalid`.
        let mut root = empty_manifest("root");
        root.dependencies.insert("a".into(), path_dep("a"));
        // /root/a exists (manifest_exists returns true) but load() fails.
        struct PartialLoader;
        impl ManifestLoader for PartialLoader {
            fn load(&self, dir: &Path) -> Result<Manifest, manifest::ManifestError> {
                Err(manifest::ManifestError::MissingPackageName {
                    path: dir.join("kara.toml"),
                })
            }
            fn manifest_exists(&self, _dir: &Path) -> bool {
                true
            }
        }
        let err = build_dep_graph(&PathBuf::from("/root"), root, &PartialLoader).unwrap_err();
        match err {
            DepGraphError::PathDepManifestInvalid { dep_name, .. } => {
                assert_eq!(dep_name, "a");
            }
            other => panic!("expected PathDepManifestInvalid, got {other:?}"),
        }
    }

    #[test]
    fn workspace_deref_chain_through_path_dep() {
        // root declares workspace.dependencies; a member at /root/member
        // reaches workspace.dependencies via path-dep walk + workspace=true.
        let mut root = empty_manifest("root");
        root.dependencies
            .insert("member".into(), path_dep("member"));
        root.workspace_dependencies
            .insert("http".into(), registry("2.0"));
        root.is_workspace_root = true;
        let mut member = empty_manifest("member");
        member
            .dependencies
            .insert("http".into(), DependencySpec::Workspace);
        let loader = MemLoader {
            manifests: BTreeMap::from([(PathBuf::from("/root/member"), member)]),
        };
        let graph = build_dep_graph(&PathBuf::from("/root"), root, &loader).expect("build");
        let member_derived = &graph.derived_deps[&PathBuf::from("/root/member")];
        // The member's `workspace = true` http entry was dereferenced against
        // the root's workspace_dependencies even though the member itself
        // declared no `[workspace.dependencies]`.
        assert_eq!(member_derived.get("http"), Some(&registry("2.0")));
    }

    #[test]
    fn workspace_deps_inherited_from_ancestor_root() {
        // Follow-up (g): the entry point is a *member* (`/ws/core`) whose own
        // manifest is not the workspace root; `workspace = true` derefs against
        // `[workspace.dependencies]` on the nearest ancestor `[workspace]` root
        // (`/ws`), discovered by walking upward.
        let mut member = empty_manifest("core");
        member
            .dependencies
            .insert("http".into(), DependencySpec::Workspace);
        let mut ws_root = empty_manifest("ws");
        ws_root.is_workspace_root = true;
        ws_root
            .workspace_dependencies
            .insert("http".into(), registry("1.5"));
        let loader = MemLoader {
            manifests: BTreeMap::from([(PathBuf::from("/ws"), ws_root)]),
        };
        let graph = build_dep_graph(&PathBuf::from("/ws/core"), member, &loader).expect("build");
        let derived = &graph.derived_deps[&PathBuf::from("/ws/core")];
        assert_eq!(
            derived.get("http"),
            Some(&registry("1.5")),
            "member's `workspace = true` must deref against the ancestor root's workspace deps",
        );
    }

    #[test]
    fn workspace_ancestor_root_missing_declaration_errors() {
        // The ancestor workspace root exists but doesn't declare the requested
        // dep → E_WORKSPACE_DEP_NOT_DECLARED (root found, dep absent) — distinct
        // from the no-root-at-all case below.
        let mut member = empty_manifest("core");
        member
            .dependencies
            .insert("http".into(), DependencySpec::Workspace);
        let mut ws_root = empty_manifest("ws");
        ws_root.is_workspace_root = true;
        ws_root
            .workspace_dependencies
            .insert("json".into(), registry("1.0"));
        let loader = MemLoader {
            manifests: BTreeMap::from([(PathBuf::from("/ws"), ws_root)]),
        };
        let err = build_dep_graph(&PathBuf::from("/ws/core"), member, &loader).unwrap_err();
        assert_eq!(err.code(), "E_WORKSPACE_DEP_NOT_DECLARED");
    }

    #[test]
    fn workspace_walk_passes_over_non_root_ancestor() {
        // A regular package sits between the member and the workspace root:
        // /ws (root) / mid (ordinary pkg) / core (member). The upward walk must
        // skip `mid` (not a workspace root) and inherit from `/ws`.
        let mut member = empty_manifest("core");
        member
            .dependencies
            .insert("http".into(), DependencySpec::Workspace);
        let mid = empty_manifest("mid"); // not a workspace root
        let mut ws_root = empty_manifest("ws");
        ws_root.is_workspace_root = true;
        ws_root
            .workspace_dependencies
            .insert("http".into(), registry("3.0"));
        let loader = MemLoader {
            manifests: BTreeMap::from([
                (PathBuf::from("/ws/mid"), mid),
                (PathBuf::from("/ws"), ws_root),
            ]),
        };
        let graph =
            build_dep_graph(&PathBuf::from("/ws/mid/core"), member, &loader).expect("build");
        let derived = &graph.derived_deps[&PathBuf::from("/ws/mid/core")];
        assert_eq!(derived.get("http"), Some(&registry("3.0")));
    }

    #[test]
    fn workspace_no_ancestor_root_surfaces_outside_error() {
        // The member's ancestors exist but none declares `[workspace]` → the
        // `workspace = true` dep is still a hard error, exactly as before (g)
        // (proves the walk doesn't manufacture a phantom workspace root).
        let mut member = empty_manifest("core");
        member
            .dependencies
            .insert("http".into(), DependencySpec::Workspace);
        let mid = empty_manifest("mid"); // ordinary package, not a workspace root
        let loader = MemLoader {
            manifests: BTreeMap::from([(PathBuf::from("/ws/mid"), mid)]),
        };
        let err = build_dep_graph(&PathBuf::from("/ws/mid/core"), member, &loader).unwrap_err();
        assert_eq!(err.code(), "E_WORKSPACE_DEP_OUTSIDE_WORKSPACE");
    }

    #[test]
    fn error_codes_round_trip() {
        // Light pin — keeps the symbolic codes stable for the renderer.
        let err = DepGraphError::WorkspaceDepNotDeclared {
            manifest_dir: PathBuf::from("/x"),
            dep_name: "y".into(),
        };
        assert_eq!(err.code(), "E_WORKSPACE_DEP_NOT_DECLARED");
        let err = DepGraphError::DependencyCycle {
            chain: vec![PathBuf::from("/a"), PathBuf::from("/a")],
        };
        assert_eq!(err.code(), "E_DEPENDENCY_CYCLE");
        let err = DepGraphError::PathDepManifestInvalid {
            from_dir: PathBuf::from("/x"),
            dep_name: "y".into(),
            source: Box::new(manifest::ManifestError::MissingPackageName {
                path: PathBuf::from("/x/kara.toml"),
            }),
        };
        assert_eq!(err.code(), "E_PATH_DEP_MANIFEST_INVALID");
        let err = DepGraphError::OfflineVendorEntryMissing {
            from_dir: PathBuf::from("/x"),
            dep_name: "y".into(),
            expected: PathBuf::from("/x/vendor/y"),
        };
        assert_eq!(err.code(), "E_OFFLINE_VENDOR_ENTRY_MISSING");
    }

    #[test]
    fn offline_root_redirects_path_dep_walk_to_vendor() {
        // Root manifest declares `child = { path = "libs/child" }`; offline
        // mode should look at `/root/vendor/child/` instead of
        // `/root/libs/child/`.
        let mut root = empty_manifest("root");
        root.dependencies
            .insert("child".into(), path_dep("libs/child"));
        let child = empty_manifest("child");
        // The MemLoader only knows about the vendor path — proving the
        // offline walk consulted vendor, not the manifest-declared path.
        let loader = MemLoader {
            manifests: BTreeMap::from([(PathBuf::from("/root/vendor/child"), child)]),
        };
        let graph = build_dep_graph_with_offline(
            &PathBuf::from("/root"),
            root,
            &loader,
            Some(&PathBuf::from("/root/vendor")),
        )
        .expect("offline build");
        assert!(graph
            .manifests
            .contains_key(&PathBuf::from("/root/vendor/child")));
        assert!(!graph
            .manifests
            .contains_key(&PathBuf::from("/root/libs/child")));
    }

    #[test]
    fn offline_root_missing_vendor_entry_surfaces_focused_error() {
        // Manifest declares a path-dep; vendor/ has no entry for it.
        let mut root = empty_manifest("root");
        root.dependencies
            .insert("child".into(), path_dep("libs/child"));
        let loader = MemLoader {
            manifests: BTreeMap::new(),
        };
        let err = build_dep_graph_with_offline(
            &PathBuf::from("/root"),
            root,
            &loader,
            Some(&PathBuf::from("/root/vendor")),
        )
        .unwrap_err();
        match err {
            DepGraphError::OfflineVendorEntryMissing {
                dep_name, expected, ..
            } => {
                assert_eq!(dep_name, "child");
                assert_eq!(expected, PathBuf::from("/root/vendor/child"));
            }
            other => panic!("expected OfflineVendorEntryMissing, got {other:?}"),
        }
    }

    #[test]
    fn offline_root_walks_transitive_deps_from_vendor_flat_layout() {
        // root → child → grandchild — each transitive path-dep should be
        // re-rooted at vendor/<name>/ regardless of the manifest's declared
        // path. This is the "flat vendor layout" Cargo also follows.
        let mut root = empty_manifest("root");
        root.dependencies
            .insert("child".into(), path_dep("libs/child"));
        let mut child = empty_manifest("child");
        child
            .dependencies
            .insert("grandchild".into(), path_dep("../grandchild"));
        let grandchild = empty_manifest("grandchild");
        let loader = MemLoader {
            manifests: BTreeMap::from([
                (PathBuf::from("/root/vendor/child"), child),
                (PathBuf::from("/root/vendor/grandchild"), grandchild),
            ]),
        };
        let graph = build_dep_graph_with_offline(
            &PathBuf::from("/root"),
            root,
            &loader,
            Some(&PathBuf::from("/root/vendor")),
        )
        .expect("offline transitive walk");
        assert!(graph
            .manifests
            .contains_key(&PathBuf::from("/root/vendor/grandchild")));
    }

    // ── line 884: dev-deps excluded from non-test builds ─────────

    #[test]
    fn dev_deps_excluded_from_build_mode_walk() {
        // Build mode: include_dev_deps=false (default). The root's
        // dev_dependencies must NOT appear in derived_deps and the
        // dev-dep's manifest must NOT be loaded.
        let mut root = empty_manifest("root");
        root.dependencies
            .insert("real".into(), path_dep("libs/real"));
        root.dev_dependencies
            .insert("test-only".into(), path_dep("libs/test-only"));
        let real = empty_manifest("real");
        let test_only = empty_manifest("test-only");
        let loader = MemLoader {
            manifests: BTreeMap::from([
                (PathBuf::from("/root/libs/real"), real),
                (PathBuf::from("/root/libs/test-only"), test_only),
            ]),
        };
        let graph = build_dep_graph(&PathBuf::from("/root"), root, &loader).expect("graph");
        let derived = &graph.derived_deps[&PathBuf::from("/root")];
        assert!(derived.contains_key("real"));
        assert!(
            !derived.contains_key("test-only"),
            "dev-deps must be excluded from build mode; got: {derived:?}",
        );
        assert!(!graph
            .manifests
            .contains_key(&PathBuf::from("/root/libs/test-only")));
    }

    #[test]
    fn dev_deps_included_in_test_mode_walk() {
        // Test mode: include_dev_deps=true. The root's dev_dependencies
        // appear in derived_deps and the dev-dep's manifest IS loaded.
        let mut root = empty_manifest("root");
        root.dependencies
            .insert("real".into(), path_dep("libs/real"));
        root.dev_dependencies
            .insert("test-only".into(), path_dep("libs/test-only"));
        let real = empty_manifest("real");
        let test_only = empty_manifest("test-only");
        let loader = MemLoader {
            manifests: BTreeMap::from([
                (PathBuf::from("/root/libs/real"), real),
                (PathBuf::from("/root/libs/test-only"), test_only),
            ]),
        };
        let graph = build_test_mode_graph(&PathBuf::from("/root"), root, &loader).expect("graph");
        let derived = &graph.derived_deps[&PathBuf::from("/root")];
        assert!(derived.contains_key("real"));
        assert!(derived.contains_key("test-only"));
        assert!(graph
            .manifests
            .contains_key(&PathBuf::from("/root/libs/test-only")));
    }

    #[test]
    fn transitive_dev_deps_do_not_propagate_even_in_test_mode() {
        // Cargo's "dev-deps don't propagate" rule: a transitive child's
        // dev_dependencies must never participate in the parent build,
        // even when the parent opted into include_dev_deps.
        let mut root = empty_manifest("root");
        root.dependencies
            .insert("child".into(), path_dep("libs/child"));
        let mut child = empty_manifest("child");
        child
            .dev_dependencies
            .insert("child-test-only".into(), path_dep("../child-test-only"));
        let child_test_only = empty_manifest("child-test-only");
        let loader = MemLoader {
            manifests: BTreeMap::from([
                (PathBuf::from("/root/libs/child"), child),
                (PathBuf::from("/root/libs/child-test-only"), child_test_only),
            ]),
        };
        let graph = build_test_mode_graph(&PathBuf::from("/root"), root, &loader).expect("graph");
        // child appears (root's regular dep) but child-test-only does not
        // (transitive dev-dep never propagates).
        assert!(graph
            .manifests
            .contains_key(&PathBuf::from("/root/libs/child")));
        assert!(
            !graph
                .manifests
                .contains_key(&PathBuf::from("/root/libs/child-test-only")),
            "transitive dev-deps must NOT load even in test mode; manifests: {:?}",
            graph.manifests.keys().collect::<Vec<_>>(),
        );
    }
}
