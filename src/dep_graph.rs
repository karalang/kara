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
//!    `E_DEPENDENCY_CYCLE` naming the chain. Registry and git deps stop the
//!    walk at the leaf (the resolver will reach them via the registry-proxy
//!    fetch surface, line 819).
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
#[derive(Debug, Clone, Copy, Default)]
pub struct DepGraphOptions<'a> {
    pub offline_root: Option<&'a Path>,
    pub include_dev_deps: bool,
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

    let mut visiting_stack: Vec<PathBuf> = Vec::new();
    let mut visiting_set: HashSet<PathBuf> = HashSet::new();
    let mut visited: HashSet<PathBuf> = HashSet::new();

    let workspace_deps = root_manifest.workspace_dependencies.clone();

    visit(
        &root_canonical,
        root_manifest,
        loader,
        &workspace_deps,
        options.offline_root,
        options.include_dev_deps,
        true,
        &mut manifests,
        &mut derived_deps,
        &mut visiting_stack,
        &mut visiting_set,
        &mut visited,
    )?;

    Ok(DepGraph {
        root_dir: root_canonical,
        manifests,
        derived_deps,
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
    is_root: bool,
    manifests: &mut BTreeMap<PathBuf, Manifest>,
    derived_deps: &mut BTreeMap<PathBuf, BTreeMap<String, DependencySpec>>,
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
            // Transitive children are never the root — dev-deps stop
            // propagating here even if the root opted them in.
            false,
            manifests,
            derived_deps,
            visiting_stack,
            visiting_set,
            visited,
        )?;
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
            },
        )
    }

    fn empty_manifest(name: &str) -> Manifest {
        Manifest {
            name: name.to_string(),
            edition: "2026".to_string(),
            profile: manifest::CompileProfile::Default,
            test_resources: BTreeMap::new(),
            kara_version: None,
            dependencies: BTreeMap::new(),
            dev_dependencies: BTreeMap::new(),
            workspace_dependencies: BTreeMap::new(),
            target_dependencies: BTreeMap::new(),
            target_dev_dependencies: BTreeMap::new(),
            target_profile_overrides: BTreeMap::new(),
            build_default_target: None,
            lints: manifest::ManifestLints::default(),
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
