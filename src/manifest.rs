//! `kara.toml` project manifest parsing (CR-24 slice 2).
//!
//! The manifest is the canonical project-identity signal for multi-file
//! compilation — see `docs/design.md § Package System`. For v1 the compiler
//! parses only `[package].name` (required) and `[package].edition` (optional),
//! per `brainstorming/brainstorming_v41.md § P1`. Every other field is
//! **ignored, not rejected**: a user's `[dependencies]`, `[workspace]`, or
//! `[build]` table is accepted but has no effect until the package-manager
//! work lands in a later phase. (Carve-outs that have since landed:
//! `[dependencies]`/`[dev-dependencies]` and the `[target.*]` overlays are
//! structurally parsed, `[build].target` selects the default build triple,
//! and `[build].targets` declares the v1 target matrix for `karac check`
//! multi-target verification — see `parse_build_targets`.) Unknown keys *inside* `[package]` emit a soft
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
    /// `[test].timeout_seconds` — package-wide per-test timeout default
    /// for `karac test`, in seconds (numeric, no unit suffix). `None`
    /// when absent, in which case the runner falls back to the
    /// `KARAC_TEST_TIMEOUT_SECS` env var and then the built-in 30 s
    /// default. Precedence (phase-7 line 847 sub-steps 2+3): a per-test
    /// `#[test(timeout_seconds = N)]` attribute > this manifest value >
    /// the env var > 30 s.
    pub test_timeout_seconds: Option<u64>,
    /// `[package].kara-version` — the minimum compiler version this
    /// package requires (MSRV in Rust parlance). Lifted at parse time
    /// from the raw string into a `semver::VersionReq` (Cargo-style
    /// constraint vocabulary, same as `DependencySpec` versions) so the
    /// resolver can intersect it against the active toolchain version
    /// in a uniform way. `None` when the field is absent.
    pub kara_version: Option<semver::VersionReq>,
    /// `[dependencies]` table — structured capture of every entry. v1
    /// parses but does not resolve; the resolver (PubGrub) attaches as
    /// a future slice and consumes this map. `BTreeMap` keeps iteration
    /// stable for deterministic diagnostic output. Empty when the table
    /// is absent or empty.
    pub dependencies: BTreeMap<String, DependencySpec>,
    /// `[dev-dependencies]` — same shape as `dependencies`. Resolver
    /// will include these only when building test artifacts (see the
    /// `[dev-dependencies]` excluded-from-non-test-builds entry in the
    /// 5.5 tracker). Empty when the table is absent.
    pub dev_dependencies: BTreeMap<String, DependencySpec>,
    /// `[workspace.dependencies]` — declared on the workspace-root
    /// manifest. Members reference these via `name = { workspace = true }`
    /// entries; the graph-materialization slice (`src/dep_graph.rs`)
    /// dereferences a member's `Workspace` spec by looking up the
    /// matching key here. Empty when the manifest carries no
    /// `[workspace]` table or no nested `dependencies` sub-table.
    pub workspace_dependencies: BTreeMap<String, DependencySpec>,
    /// `[target.<triple>.dependencies]` — per-target dependency
    /// overlays keyed by target triple. The build pipeline picks the
    /// matching entry (if any) for the active target and merges it
    /// onto `dependencies` before dep graph materialization. Empty
    /// when no `[target.*.dependencies]` block is declared. See
    /// `merge_target_overlay`.
    pub target_dependencies: BTreeMap<String, BTreeMap<String, DependencySpec>>,
    /// `[target.<triple>.dev-dependencies]` — per-target dev-dep
    /// overlays, same merge contract as `target_dependencies` but
    /// activated only under test-mode resolution (line 884).
    pub target_dev_dependencies: BTreeMap<String, BTreeMap<String, DependencySpec>>,
    /// `[target.<triple>.profile]` — per-target compile-profile
    /// override. When the active target matches one of these keys,
    /// the entry replaces `profile` for the build pipeline (extern-
    /// site effect rules + stdlib layer gating follow the override).
    /// Empty when no `[target.*.profile]` is declared.
    pub target_profile_overrides: BTreeMap<String, CompileProfile>,
    /// `[build].target` — default target triple for `karac build`
    /// when `--target` isn't passed. `None` falls back to the host
    /// triple (`build_cache::host_target_triple`). Captured at parse
    /// time so the build pipeline reads it without re-walking the
    /// TOML table.
    pub build_default_target: Option<String>,
    /// `[build].targets` — the v1 compilation targets this package
    /// declares (closed set: `target::V1_TARGETS`). Drives `karac
    /// check` multi-target verification: with two or more declared
    /// targets, check runs the full pipeline once per target,
    /// parameterizing the target-provided resource set each time
    /// (design.md § Cross-target Compilation > `karac check` Under
    /// Multiple Targets). Empty when undeclared — check then runs
    /// single-pass under the default (`native`) target. Distinct from
    /// `[build].target` (a rustc-style triple selecting the manifest
    /// overlay for `karac build`).
    pub build_targets: Vec<String>,
    /// `[lints]` table — project-wide lint posture, the global mirror of
    /// source-level `#[allow(...)]` / `#[deny(...)]`. Empty struct when
    /// the table is absent. The CLI lifts this into the typechecker's
    /// `CliLintOverrides` so resolution flows through the same cascade
    /// as the per-source `#[allow]` family (source attribute beats CLI
    /// flag beats `[lints]` beats registry default). Today exposes one
    /// knob, `allow_unstable_api`, with more lifted as the surface
    /// grows (e.g., a future `allow = ["lint_name"]` array).
    ///
    /// Phase-8 line 49 / design.md § v1 Positioning > Stable surface
    /// vs. unstable extension points.
    pub lints: ManifestLints,
    /// `[release] target-cpu = "<name>"` — project-declared CPU baseline
    /// override for codegen (phase-10 `--target-cpu`; design.md § CPU
    /// Baseline Targeting). Lowest tier of the precedence chain:
    /// `--target-cpu` CLI flag, then `KARAC_TARGET_CPU` env var, then
    /// this value, then the per-target default table in
    /// `codegen/driver.rs`. The name
    /// is validated against LLVM's per-target CPU registry at build
    /// time, not here — the manifest layer has no LLVM access (codegen
    /// containment) and the valid set depends on the active target.
    /// `None` when the table or key is absent.
    pub release_target_cpu: Option<String>,
    /// `[release] target-features = "<+feat,-feat,…>"` — project-declared
    /// feature-string override for codegen (phase-10 `--target-features`;
    /// design.md § CPU Baseline Targeting > Feature-string override).
    /// Lowest tier of its own precedence chain (`--target-features` CLI
    /// flag, then `KARAC_TARGET_FEATURES` env var, then this value),
    /// resolved independently of `release_target_cpu`'s chain. Token
    /// shape (`+`/`-` prefixes, names in LLVM's per-target feature
    /// registry) is validated at build time, not here — same containment
    /// rationale as `release_target_cpu` above. `None` when absent.
    pub release_target_features: Option<String>,
    /// `[toolchain] wasm-tools = "<version>"` — exact-version pin for the
    /// external `wasm-tools` binary that `--bindings component` shells out
    /// to for embedded-WIT componentization (design.md § Component Model
    /// emission — the spec stays out of the compiler; the pin keeps builds
    /// reproducible). Checked verbatim against `wasm-tools --version` at
    /// build time by `componentize::resolve_wasm_tools`; a mismatch is a
    /// hard error there, not here (the manifest layer never probes PATH).
    /// `None` when the table or key is absent — any discovered version is
    /// then accepted.
    pub toolchain_wasm_tools: Option<String>,
    /// `[wasm] pool-size = <n>` — worker-pool size baked into the JS glue
    /// for `--features wasm-threads` builds (phase-10 wasm-threads entry;
    /// design.md § WASM Concurrency Lowering). Overrides the load-time
    /// `navigator.hardwareConcurrency` default. The manifest *tunes* the
    /// threaded build; it can never *enable* one — the COOP/COEP
    /// deployment contract belongs at the CLI flag where it's visible.
    /// `None` when absent.
    pub wasm_pool_size: Option<u32>,
    /// `[wasm] fallback = false` — opt out of the SAB-unavailable
    /// graceful degradation: instead of `console.warn` + loading the
    /// sequential module, the glue hard-errors at load time. Both
    /// modules are still emitted (the artifact set never depends on
    /// deploy-environment knobs). `None`/absent means fallback enabled.
    pub wasm_fallback: Option<bool>,
    /// `[wasm] max-memory-pages = <n>` — `--max-memory` (in 64 KiB wasm
    /// pages) for the threaded module's shared memory. Shared memories
    /// must declare a maximum; default mirrors rustc's own
    /// wasm32-wasip1-threads target default (16384 pages = 1 GiB).
    /// `None` when absent.
    pub wasm_max_memory_pages: Option<u32>,
    pub warnings: Vec<ManifestWarning>,
}

/// `[lints]` table contents lifted from the manifest. Defaults are
/// "no global override" — equivalent to no `[lints]` block at all.
/// Each field maps to a CLI-side lint override in
/// `crate::lints::CliLintOverrides`.
#[derive(Debug, Clone, Default)]
pub struct ManifestLints {
    /// `[lints].allow_unstable_api = true` — globally suppresses the
    /// `unstable_api` lint, opting the entire build into the
    /// `#[unstable]`-gated stdlib surface. Phase-8 line 49 prereq 4.
    /// Source-level `#[deny(unstable_api)]` still wins per the
    /// cascade's "inner scope is most specific authority" rule
    /// ([`crate::lints::effective_level_for_module_lint`]).
    pub allow_unstable_api: bool,
}

/// One `[dependencies]` (or `[dev-dependencies]`) entry. Three shapes are
/// recognized today; `[target.X.dependencies]` and `[workspace.dependencies]`
/// have their own future slices (see line 836 / line 1129 in
/// `docs/implementation_checklist/phase-5-diagnostics.md`).
///
/// - `Registry { version }`: the bare-string shorthand `name = "1.2"` or
///   `name = { version = "1.2" }`. Version strings parse as Cargo-style
///   comparators via `semver::VersionReq` — bare `"1.2"` means `^1.2.0`
///   (i.e. `>=1.2.0, <2.0.0`); `"=1.2.3"` exact; `">=1.0, <1.5"` range.
/// - `Path { path, version }`: `name = { path = "../foo" }`, optionally with
///   a `version` for publication compatibility.
/// - `Git { url, reference, version }`: `name = { git = "https://…" }` with
///   at most one of `branch` / `tag` / `rev`, optionally with `version`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DependencySpec {
    Registry {
        version: semver::VersionReq,
    },
    Path {
        path: PathBuf,
        version: Option<semver::VersionReq>,
    },
    Git {
        url: String,
        reference: Option<GitRef>,
        version: Option<semver::VersionReq>,
    },
    /// `name = { workspace = true }` — the entry's source is the
    /// workspace root's `[workspace.dependencies]` table. v1's slice 1
    /// captures the intent; the workspace-resolver slice will dereference
    /// it against the workspace root before the resolver runs.
    Workspace,
}

/// Git-dep ref selector — at most one is honored per entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitRef {
    Branch(String),
    Tag(String),
    Rev(String),
}

/// Recognized keys inside an inline-table dependency entry. Anything outside
/// this set produces a soft warning so typos surface without rejecting the
/// build. `workspace` is reserved for the future workspace-dependencies slice;
/// today it produces the same soft warning as any other unknown key (with a
/// hint pointing at the future-slice entry).
const KNOWN_DEPENDENCY_KEYS: &[&str] = &[
    "version",
    "path",
    "git",
    "branch",
    "tag",
    "rev",
    "workspace",
];

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
    /// `[dependencies]` or `[dev-dependencies]` table value is the wrong
    /// shape (e.g. an integer in place of a string-or-inline-table).
    InvalidDependencySpec {
        path: PathBuf,
        table: &'static str,
        name: String,
        message: String,
    },
    /// `[build].targets` entry problem — unknown v1 target name or a
    /// duplicate entry. Hard error rather than soft warning: a typo'd
    /// target would otherwise silently drop a target from the `karac
    /// check` verification matrix.
    InvalidBuildTargets {
        path: PathBuf,
        message: String,
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
            ManifestError::InvalidDependencySpec {
                path,
                table,
                name,
                message,
            } => write!(
                f,
                "`{}`: `[{}].{}`: {}",
                path.display(),
                table,
                name,
                message,
            ),
            ManifestError::InvalidBuildTargets { path, message } => {
                write!(f, "`{}`: `[build].targets`: {}", path.display(), message)
            }
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

    // `[package].kara-version` — optional MSRV constraint. Wrong
    // type is a hard error (typos shouldn't silently disable the
    // constraint); absent is the common case. Slice 6 of the
    // PubGrub-resolver entry lifts the raw string into a structured
    // `VersionReq` so the resolver intersects it uniformly against
    // the active toolchain. Parse failure surfaces as an
    // `InvalidFieldType` so the diagnostic shape stays consistent
    // with every other manifest-side validation.
    let kara_version = match package.get("kara-version") {
        Some(toml::Value::String(s)) => {
            if s.trim().is_empty() {
                return Err(ManifestError::InvalidFieldType {
                    path: path.to_path_buf(),
                    key: "kara-version".to_string(),
                    expected: "a non-empty version constraint (e.g. \"1.0\" or \">=1.2, <2.0\")",
                });
            }
            match semver::VersionReq::parse(s) {
                Ok(req) => Some(req),
                Err(_) => {
                    return Err(ManifestError::InvalidFieldType {
                        path: path.to_path_buf(),
                        key: "kara-version".to_string(),
                        expected: "a valid Cargo-style semver constraint (e.g. \"1.0\", \"^1.2\", \">=1.0, <2.0\")",
                    });
                }
            }
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
    let test_timeout_seconds = parse_test_timeout_seconds(path, &table)?;
    let dependencies = parse_dependencies_table(path, &table, "dependencies", &mut warnings)?;
    let dev_dependencies =
        parse_dependencies_table(path, &table, "dev-dependencies", &mut warnings)?;
    let workspace_dependencies = parse_workspace_dependencies(path, &table, &mut warnings)?;
    let (target_dependencies, target_dev_dependencies, target_profile_overrides) =
        parse_target_tables(path, &table, &mut warnings)?;
    let build_default_target = parse_build_default_target(path, &table)?;
    let build_targets = parse_build_targets(path, &table)?;
    let lints = parse_lints_table(path, &table, &mut warnings)?;
    let (release_target_cpu, release_target_features) =
        parse_release_table(path, &table, &mut warnings)?;
    let toolchain_wasm_tools = parse_toolchain_table(path, &table, &mut warnings)?;
    let (wasm_pool_size, wasm_fallback, wasm_max_memory_pages) =
        parse_wasm_table(path, &table, &mut warnings)?;

    // Stable order across package-key + dependency warnings — same sort key
    // (message string) used as before, but now applied after the full
    // accumulation so diagnostic output is deterministic regardless of which
    // table contributed a given warning.
    warnings.sort_by(|a, b| a.message.cmp(&b.message));

    Ok(Manifest {
        name,
        edition,
        profile,
        test_resources,
        test_timeout_seconds,
        kara_version,
        dependencies,
        dev_dependencies,
        workspace_dependencies,
        target_dependencies,
        target_dev_dependencies,
        target_profile_overrides,
        build_default_target,
        build_targets,
        lints,
        release_target_cpu,
        release_target_features,
        toolchain_wasm_tools,
        wasm_pool_size,
        wasm_fallback,
        wasm_max_memory_pages,
        warnings,
    })
}

/// Parse the `[release]` table when present. Recognised keys at v1:
/// `target-cpu` (non-empty string — the CPU baseline override) and
/// `target-features` (non-empty string — the feature-string override),
/// both per design.md § CPU Baseline Targeting. Unknown keys soft-warn
/// (reserved for later release-profile knobs); a wrong-typed or empty
/// value for a known key hard-errors so a typo can't silently drop the
/// override. Absent table → `(None, None)`.
fn parse_release_table(
    path: &Path,
    table: &toml::Table,
    warnings: &mut Vec<ManifestWarning>,
) -> Result<(Option<String>, Option<String>), ManifestError> {
    let Some(value) = table.get("release") else {
        return Ok((None, None));
    };
    let release_table = value
        .as_table()
        .ok_or_else(|| ManifestError::InvalidFieldType {
            path: path.to_path_buf(),
            key: "release".to_string(),
            expected: "a table (e.g. `[release]`)",
        })?;
    let mut target_cpu = None;
    let mut target_features = None;
    for (key, val) in release_table {
        match key.as_str() {
            "target-cpu" => match val {
                toml::Value::String(s) if !s.trim().is_empty() => {
                    target_cpu = Some(s.trim().to_string());
                }
                _ => {
                    return Err(ManifestError::InvalidFieldType {
                        path: path.to_path_buf(),
                        key: "release.target-cpu".to_string(),
                        expected: "a non-empty CPU name string (e.g. \"apple-m1\", \"x86-64-v3\")",
                    });
                }
            },
            "target-features" => match val {
                toml::Value::String(s) if !s.trim().is_empty() => {
                    target_features = Some(s.trim().to_string());
                }
                _ => {
                    return Err(ManifestError::InvalidFieldType {
                        path: path.to_path_buf(),
                        key: "release.target-features".to_string(),
                        expected:
                            "a non-empty feature list string (e.g. \"+aes,-outline-atomics\")",
                    });
                }
            },
            other => warnings.push(ManifestWarning {
                line: None,
                message: format!(
                    "unknown key `[release].{other}` — ignored in v1 (reserved for a later release)"
                ),
            }),
        }
    }
    Ok((target_cpu, target_features))
}

/// Parse the `[toolchain]` table when present. Recognised keys at v1:
/// `wasm-tools` (non-empty string — the exact version the discovered
/// `wasm-tools` binary must report, e.g. `"1.251.0"`), per design.md
/// § Component Model emission. Unknown keys soft-warn (reserved for
/// later external-tool pins, e.g. `wit-bindgen`); a wrong-typed or
/// empty value hard-errors so a typo can't silently drop the pin.
/// Absent table → `None`.
fn parse_toolchain_table(
    path: &Path,
    table: &toml::Table,
    warnings: &mut Vec<ManifestWarning>,
) -> Result<Option<String>, ManifestError> {
    let Some(value) = table.get("toolchain") else {
        return Ok(None);
    };
    let toolchain_table = value
        .as_table()
        .ok_or_else(|| ManifestError::InvalidFieldType {
            path: path.to_path_buf(),
            key: "toolchain".to_string(),
            expected: "a table (e.g. `[toolchain]`)",
        })?;
    let mut wasm_tools = None;
    for (key, val) in toolchain_table {
        match key.as_str() {
            "wasm-tools" => match val {
                toml::Value::String(s) if !s.trim().is_empty() => {
                    wasm_tools = Some(s.trim().to_string());
                }
                _ => {
                    return Err(ManifestError::InvalidFieldType {
                        path: path.to_path_buf(),
                        key: "toolchain.wasm-tools".to_string(),
                        expected: "a non-empty exact version string (e.g. \"1.251.0\")",
                    });
                }
            },
            other => warnings.push(ManifestWarning {
                line: None,
                message: format!(
                    "unknown key `[toolchain].{other}` — ignored in v1 (reserved for a later release)"
                ),
            }),
        }
    }
    Ok(wasm_tools)
}

/// Parse the `[wasm]` table when present. Recognised keys at v1 (all
/// `--features wasm-threads` tuning knobs — phase-10 wasm-threads entry;
/// design.md § WASM Concurrency Lowering): `pool-size` (positive integer
/// — worker-pool size baked into the glue, overriding the load-time
/// `navigator.hardwareConcurrency` default), `fallback` (bool — `false`
/// makes the glue hard-error instead of console.warn + sequential when
/// SAB is unavailable), and `max-memory-pages` (positive integer —
/// `--max-memory` for the threaded module's shared memory, in 64 KiB
/// pages). Unknown keys soft-warn (reserved for later wasm knobs); a
/// wrong-typed or non-positive value for a known key hard-errors so a
/// typo can't silently drop the override. Absent table → all `None`.
fn parse_wasm_table(
    path: &Path,
    table: &toml::Table,
    warnings: &mut Vec<ManifestWarning>,
) -> Result<(Option<u32>, Option<bool>, Option<u32>), ManifestError> {
    let Some(value) = table.get("wasm") else {
        return Ok((None, None, None));
    };
    let wasm_table = value
        .as_table()
        .ok_or_else(|| ManifestError::InvalidFieldType {
            path: path.to_path_buf(),
            key: "wasm".to_string(),
            expected: "a table (e.g. `[wasm]`)",
        })?;
    let mut pool_size = None;
    let mut fallback = None;
    let mut max_memory_pages = None;
    // Shared shape for the two positive-integer keys: TOML integer,
    // in 1..=u32::MAX, hard error otherwise.
    let parse_positive_u32 = |key: &str, val: &toml::Value| -> Result<u32, ManifestError> {
        match val {
            toml::Value::Integer(i) if *i > 0 && *i <= i64::from(u32::MAX) => Ok(*i as u32),
            _ => Err(ManifestError::InvalidFieldType {
                path: path.to_path_buf(),
                key: format!("wasm.{key}"),
                expected: "a positive integer",
            }),
        }
    };
    for (key, val) in wasm_table {
        match key.as_str() {
            "pool-size" => pool_size = Some(parse_positive_u32("pool-size", val)?),
            "fallback" => match val {
                toml::Value::Boolean(b) => fallback = Some(*b),
                _ => {
                    return Err(ManifestError::InvalidFieldType {
                        path: path.to_path_buf(),
                        key: "wasm.fallback".to_string(),
                        expected: "a boolean (e.g. `fallback = false`)",
                    });
                }
            },
            "max-memory-pages" => {
                max_memory_pages = Some(parse_positive_u32("max-memory-pages", val)?);
            }
            other => warnings.push(ManifestWarning {
                line: None,
                message: format!(
                    "unknown key `[wasm].{other}` — ignored in v1 (reserved for a later release)"
                ),
            }),
        }
    }
    Ok((pool_size, fallback, max_memory_pages))
}

/// Parse the `[lints]` table when present. Recognised keys at v1:
/// `allow_unstable_api` (bool). Unknown keys soft-warn; non-bool
/// values for known keys hard-error so a typo (`= "true"`) doesn't
/// silently no-op. Absent table → `ManifestLints::default()`.
fn parse_lints_table(
    path: &Path,
    table: &toml::Table,
    warnings: &mut Vec<ManifestWarning>,
) -> Result<ManifestLints, ManifestError> {
    let Some(value) = table.get("lints") else {
        return Ok(ManifestLints::default());
    };
    let lints_table = value
        .as_table()
        .ok_or_else(|| ManifestError::InvalidFieldType {
            path: path.to_path_buf(),
            key: "lints".to_string(),
            expected: "a table (e.g. `[lints]`)",
        })?;
    let mut out = ManifestLints::default();
    for (key, val) in lints_table {
        match key.as_str() {
            "allow_unstable_api" => match val {
                toml::Value::Boolean(b) => out.allow_unstable_api = *b,
                _ => {
                    return Err(ManifestError::InvalidFieldType {
                        path: path.to_path_buf(),
                        key: "lints.allow_unstable_api".to_string(),
                        expected: "a boolean (`true` or `false`)",
                    });
                }
            },
            other => warnings.push(ManifestWarning {
                line: None,
                message: format!(
                    "unknown key `[lints].{other}` — ignored in v1 (reserved for a later release)"
                ),
            }),
        }
    }
    Ok(out)
}

/// Parse `[workspace.dependencies]` (when present) into a stable-ordered map.
/// The table sits one level deeper than `[dependencies]` — under the
/// `[workspace]` namespace — but the value-shape grammar is identical, so the
/// dependency-spec parser is reused verbatim. Members reference these entries
/// via `name = { workspace = true }` on their own `[dependencies]`.
fn parse_workspace_dependencies(
    path: &Path,
    table: &toml::Table,
    warnings: &mut Vec<ManifestWarning>,
) -> Result<BTreeMap<String, DependencySpec>, ManifestError> {
    let Some(workspace) = table.get("workspace") else {
        return Ok(BTreeMap::new());
    };
    let ws_table = workspace
        .as_table()
        .ok_or_else(|| ManifestError::InvalidFieldType {
            path: path.to_path_buf(),
            key: "workspace".to_string(),
            expected: "a table (e.g. `[workspace]`)",
        })?;
    let Some(deps) = ws_table.get("dependencies") else {
        return Ok(BTreeMap::new());
    };
    let deps_table = deps
        .as_table()
        .ok_or_else(|| ManifestError::InvalidFieldType {
            path: path.to_path_buf(),
            key: "workspace.dependencies".to_string(),
            expected: "a table (e.g. `[workspace.dependencies]`)",
        })?;
    let mut out = BTreeMap::new();
    for (name, raw) in deps_table {
        let spec = parse_dependency_value(path, "workspace.dependencies", name, raw, warnings)?;
        // Workspace = true is meaningless inside [workspace.dependencies]
        // (the entry is itself a workspace declaration). Reject so a user
        // doesn't write a recursive `workspace = true` inside the workspace
        // root and confuse themselves.
        if matches!(spec, DependencySpec::Workspace) {
            return Err(ManifestError::InvalidDependencySpec {
                path: path.to_path_buf(),
                table: "workspace.dependencies",
                name: name.clone(),
                message: "`workspace = true` inside `[workspace.dependencies]` is meaningless — \
                          the entry itself is the workspace declaration"
                    .to_string(),
            });
        }
        out.insert(name.clone(), spec);
    }
    Ok(out)
}

/// Type alias for the three per-triple maps `parse_target_tables`
/// returns — `target -> dependencies`, `target -> dev-dependencies`,
/// `target -> profile`. The alias keeps the function signature within
/// clippy's `type_complexity` threshold without obscuring the shape
/// (the field declarations on `Manifest` document the intent).
type TargetTables = (
    BTreeMap<String, BTreeMap<String, DependencySpec>>,
    BTreeMap<String, BTreeMap<String, DependencySpec>>,
    BTreeMap<String, CompileProfile>,
);

/// Parse the `[target.<triple>]` namespace into three stable-ordered maps:
/// `target -> dependencies`, `target -> dev-dependencies`, and
/// `target -> profile`. Recognized sub-keys inside each triple are
/// `dependencies`, `dev-dependencies`, and `profile`; anything else soft-
/// warns so typos surface without rejecting the build. Each sub-table is
/// parsed with the same vocabulary as the top-level `[dependencies]` /
/// `[dev-dependencies]` / `[package].profile` tables — the dispatch is
/// purely "scope by target triple", not a new shape.
fn parse_target_tables(
    path: &Path,
    table: &toml::Table,
    warnings: &mut Vec<ManifestWarning>,
) -> Result<TargetTables, ManifestError> {
    let mut deps_per_target: BTreeMap<String, BTreeMap<String, DependencySpec>> = BTreeMap::new();
    let mut dev_deps_per_target: BTreeMap<String, BTreeMap<String, DependencySpec>> =
        BTreeMap::new();
    let mut profile_per_target: BTreeMap<String, CompileProfile> = BTreeMap::new();

    let Some(value) = table.get("target") else {
        return Ok((deps_per_target, dev_deps_per_target, profile_per_target));
    };
    let target_table = value
        .as_table()
        .ok_or_else(|| ManifestError::InvalidFieldType {
            path: path.to_path_buf(),
            key: "target".to_string(),
            expected: "a table (e.g. `[target.\"x86_64-apple-darwin\"]`)",
        })?;

    for (triple, raw) in target_table {
        let triple_table = raw
            .as_table()
            .ok_or_else(|| ManifestError::InvalidFieldType {
                path: path.to_path_buf(),
                key: format!("target.{triple}"),
                expected: "a table (e.g. `[target.\"x86_64-apple-darwin\"]`)",
            })?;

        for key in triple_table.keys() {
            if !matches!(
                key.as_str(),
                "dependencies" | "dev-dependencies" | "profile"
            ) {
                warnings.push(ManifestWarning {
                    line: None,
                    message: format!(
                        "unknown key `[target.{triple}].{key}` — ignored in v1 (recognized: dependencies, dev-dependencies, profile)"
                    ),
                });
            }
        }

        if let Some(deps_value) = triple_table.get("dependencies") {
            let deps_inner =
                deps_value
                    .as_table()
                    .ok_or_else(|| ManifestError::InvalidFieldType {
                        path: path.to_path_buf(),
                        key: format!("target.{triple}.dependencies"),
                        expected: "a table (e.g. `[target.\"x86_64-apple-darwin\".dependencies]`)",
                    })?;
            let mut out = BTreeMap::new();
            for (name, raw_dep) in deps_inner {
                let spec =
                    parse_dependency_value(path, "target.dependencies", name, raw_dep, warnings)?;
                out.insert(name.clone(), spec);
            }
            if !out.is_empty() {
                deps_per_target.insert(triple.clone(), out);
            }
        }

        if let Some(dev_value) = triple_table.get("dev-dependencies") {
            let dev_inner =
                dev_value
                    .as_table()
                    .ok_or_else(|| ManifestError::InvalidFieldType {
                        path: path.to_path_buf(),
                        key: format!("target.{triple}.dev-dependencies"),
                        expected:
                            "a table (e.g. `[target.\"x86_64-apple-darwin\".dev-dependencies]`)",
                    })?;
            let mut out = BTreeMap::new();
            for (name, raw_dep) in dev_inner {
                let spec = parse_dependency_value(
                    path,
                    "target.dev-dependencies",
                    name,
                    raw_dep,
                    warnings,
                )?;
                out.insert(name.clone(), spec);
            }
            if !out.is_empty() {
                dev_deps_per_target.insert(triple.clone(), out);
            }
        }

        if let Some(profile_value) = triple_table.get("profile") {
            let s = match profile_value {
                toml::Value::String(s) => s,
                _ => {
                    return Err(ManifestError::InvalidFieldType {
                        path: path.to_path_buf(),
                        key: format!("target.{triple}.profile"),
                        expected: "a string (\"default\", \"embedded\", or \"kernel\")",
                    });
                }
            };
            let parsed = CompileProfile::parse(s.as_str()).ok_or_else(|| {
                ManifestError::InvalidFieldType {
                    path: path.to_path_buf(),
                    key: format!("target.{triple}.profile"),
                    expected: "one of \"default\", \"embedded\", or \"kernel\"",
                }
            })?;
            profile_per_target.insert(triple.clone(), parsed);
        }
    }

    Ok((deps_per_target, dev_deps_per_target, profile_per_target))
}

/// Parse `[build].target` — the default target triple for `karac build`
/// when `--target` isn't passed. Wrong-type / empty values are hard
/// errors (typos shouldn't silently disable the default); absent is
/// the common case. Other `[build]` keys remain ignored — the v1
/// vocabulary is intentionally narrow until a follow-up entry widens it.
fn parse_build_default_target(
    path: &Path,
    table: &toml::Table,
) -> Result<Option<String>, ManifestError> {
    let Some(build_value) = table.get("build") else {
        return Ok(None);
    };
    let build_table = build_value
        .as_table()
        .ok_or_else(|| ManifestError::InvalidFieldType {
            path: path.to_path_buf(),
            key: "build".to_string(),
            expected: "a table (e.g. `[build]`)",
        })?;
    let Some(target_value) = build_table.get("target") else {
        return Ok(None);
    };
    let s = match target_value {
        toml::Value::String(s) => s,
        _ => {
            return Err(ManifestError::InvalidFieldType {
                path: path.to_path_buf(),
                key: "build.target".to_string(),
                expected: "a string target triple (e.g. \"x86_64-apple-darwin\")",
            });
        }
    };
    if s.trim().is_empty() {
        return Err(ManifestError::InvalidFieldType {
            path: path.to_path_buf(),
            key: "build.target".to_string(),
            expected: "a non-empty target triple string",
        });
    }
    Ok(Some(s.clone()))
}

/// Parse `[build].targets` — the package's declared v1 compilation
/// targets, the trigger for `karac check` multi-target verification
/// (design.md § Cross-target Compilation > `karac check` Under
/// Multiple Targets). Every entry must name a member of the closed v1
/// set (`target::V1_TARGETS`); unknown names and duplicates are hard
/// errors — a soft-warn-and-ignore posture would let a typo silently
/// drop a target from a CI verification matrix, which is exactly the
/// failure the field exists to prevent. Absent is the common case
/// (single-target package, checked under `native`).
fn parse_build_targets(path: &Path, table: &toml::Table) -> Result<Vec<String>, ManifestError> {
    let Some(build_value) = table.get("build") else {
        return Ok(Vec::new());
    };
    // Wrong-shaped `[build]` is already rejected by
    // `parse_build_default_target`; a non-table here is unreachable in
    // practice but kept total for call-order independence.
    let Some(build_table) = build_value.as_table() else {
        return Ok(Vec::new());
    };
    let Some(targets_value) = build_table.get("targets") else {
        return Ok(Vec::new());
    };
    let toml::Value::Array(entries) = targets_value else {
        return Err(ManifestError::InvalidFieldType {
            path: path.to_path_buf(),
            key: "build.targets".to_string(),
            expected: "an array of v1 target names (e.g. [\"native\", \"wasm_browser\"])",
        });
    };
    let mut out: Vec<String> = Vec::new();
    for entry in entries {
        let toml::Value::String(name) = entry else {
            return Err(ManifestError::InvalidBuildTargets {
                path: path.to_path_buf(),
                message: format!(
                    "entries must be strings naming v1 targets ({})",
                    crate::target::V1_TARGETS.join(", "),
                ),
            });
        };
        if !crate::target::is_v1_target_name(name) {
            return Err(ManifestError::InvalidBuildTargets {
                path: path.to_path_buf(),
                message: format!(
                    "unknown target '{}'. Valid targets: {}",
                    name,
                    crate::target::V1_TARGETS.join(", "),
                ),
            });
        }
        if out.iter().any(|t| t == name) {
            return Err(ManifestError::InvalidBuildTargets {
                path: path.to_path_buf(),
                message: format!("duplicate target '{name}'"),
            });
        }
        out.push(name.clone());
    }
    Ok(out)
}

/// Merge `[target.<triple>]` overlays onto a manifest for the given
/// active target. Returns a copy of `manifest` with:
///
/// - `dependencies` extended by `target_dependencies[triple]`
/// - `dev_dependencies` extended by `target_dev_dependencies[triple]`
/// - `profile` overridden by `target_profile_overrides[triple]` (if set)
///
/// A dep that appears in both the base table and the per-target overlay
/// is replaced by the overlay entry — the target-specific spec wins, on
/// the same "more specific = later, wins" principle Cargo uses. Profile
/// overlay is wholesale replacement (the override is itself the new
/// profile).
///
/// `active_target = None` is a no-op — the same manifest is returned
/// unchanged. Callers that want the host triple as a fallback should
/// supply it explicitly (e.g. `build_cache::host_target_triple()`).
pub fn merge_target_overlay(manifest: &Manifest, active_target: Option<&str>) -> Manifest {
    let mut merged = manifest.clone();
    let Some(triple) = active_target else {
        return merged;
    };
    if let Some(overlay) = manifest.target_dependencies.get(triple) {
        for (name, spec) in overlay {
            merged.dependencies.insert(name.clone(), spec.clone());
        }
    }
    if let Some(overlay) = manifest.target_dev_dependencies.get(triple) {
        for (name, spec) in overlay {
            merged.dev_dependencies.insert(name.clone(), spec.clone());
        }
    }
    if let Some(override_profile) = manifest.target_profile_overrides.get(triple) {
        merged.profile = *override_profile;
    }
    merged
}

/// Parse `[dependencies]` or `[dev-dependencies]` into a stable-ordered map.
/// The `table_name` parameter selects which TOML table to look at and feeds
/// into diagnostic messages. Unknown-key soft warnings inside a dep entry
/// append to `warnings` so the CLI surfaces them alongside `[package]` notices.
fn parse_dependencies_table(
    path: &Path,
    table: &toml::Table,
    table_name: &'static str,
    warnings: &mut Vec<ManifestWarning>,
) -> Result<BTreeMap<String, DependencySpec>, ManifestError> {
    let Some(value) = table.get(table_name) else {
        return Ok(BTreeMap::new());
    };
    let deps_table = value
        .as_table()
        .ok_or_else(|| ManifestError::InvalidFieldType {
            path: path.to_path_buf(),
            key: table_name.to_string(),
            expected: "a table (e.g. `[dependencies]`)",
        })?;
    let mut out = BTreeMap::new();
    for (name, raw) in deps_table {
        let spec = parse_dependency_value(path, table_name, name, raw, warnings)?;
        out.insert(name.clone(), spec);
    }
    Ok(out)
}

fn parse_dependency_value(
    path: &Path,
    table: &'static str,
    name: &str,
    value: &toml::Value,
    warnings: &mut Vec<ManifestWarning>,
) -> Result<DependencySpec, ManifestError> {
    let invalid = |message: String| ManifestError::InvalidDependencySpec {
        path: path.to_path_buf(),
        table,
        name: name.to_string(),
        message,
    };

    match value {
        toml::Value::String(version) => {
            let trimmed = version.trim();
            if trimmed.is_empty() {
                return Err(invalid(
                    "version constraint is empty — use a non-empty semver string (e.g. \"1.0\")"
                        .to_string(),
                ));
            }
            let req = parse_version_req(version, &invalid)?;
            Ok(DependencySpec::Registry { version: req })
        }
        toml::Value::Table(entry) => {
            parse_dependency_inline_table(path, table, name, entry, warnings, &invalid)
        }
        _ => Err(invalid(
            "expected a version string or an inline table (e.g. `{ version = \"1.0\" }` or \
             `{ path = \"../foo\" }`)"
                .to_string(),
        )),
    }
}

/// Parse a Cargo-style semver constraint string into a `VersionReq`. The
/// `semver` crate accepts the same syntax Cargo uses (bare `"1.2"` → `^1.2.0`;
/// `"=1.2.3"` exact; `">=1.0, <1.5"` range; `"*"` wildcard); any malformed
/// form lands here as a focused `InvalidDependencySpec` diagnostic naming the
/// offending input and the parser's failure message.
fn parse_version_req(
    raw: &str,
    invalid: &dyn Fn(String) -> ManifestError,
) -> Result<semver::VersionReq, ManifestError> {
    semver::VersionReq::parse(raw).map_err(|e| {
        invalid(format!(
            "version constraint `{raw}` is not a valid semver requirement: {e}"
        ))
    })
}

fn parse_dependency_inline_table(
    _path: &Path,
    table: &'static str,
    name: &str,
    entry: &toml::Table,
    warnings: &mut Vec<ManifestWarning>,
    invalid: &dyn Fn(String) -> ManifestError,
) -> Result<DependencySpec, ManifestError> {
    // Soft-warn on unknown keys so typos surface without blocking the build.
    for key in entry.keys() {
        if KNOWN_DEPENDENCY_KEYS.contains(&key.as_str()) {
            continue;
        }
        warnings.push(ManifestWarning {
            line: None,
            message: format!("unknown key `[{table}].{name}.{key}` — ignored in v1"),
        });
    }

    // `workspace = true` is the workspace-inheritance form. Only `true` is
    // legal (per design.md § Package System > Workspaces) and it cannot be
    // combined with another source key — the workspace root is the source.
    if let Some(ws) = entry.get("workspace") {
        let toml::Value::Boolean(true) = ws else {
            return Err(invalid(
                "`workspace` must be the literal value `true`".to_string(),
            ));
        };
        for forbidden in ["version", "path", "git", "branch", "tag", "rev"] {
            if entry.contains_key(forbidden) {
                return Err(invalid(format!(
                    "`workspace = true` cannot be combined with `{forbidden}` — the workspace root is the source"
                )));
            }
        }
        return Ok(DependencySpec::Workspace);
    }

    let get_string = |key: &'static str| -> Result<Option<String>, ManifestError> {
        match entry.get(key) {
            None => Ok(None),
            Some(toml::Value::String(s)) => {
                if s.trim().is_empty() {
                    Err(invalid(format!(
                        "`{key}` is empty — provide a non-empty string"
                    )))
                } else {
                    Ok(Some(s.clone()))
                }
            }
            Some(_) => Err(invalid(format!("`{key}` must be a string"))),
        }
    };

    let version_raw = get_string("version")?;
    let version = match version_raw {
        Some(s) => Some(parse_version_req(&s, invalid)?),
        None => None,
    };
    let path_field = get_string("path")?;
    let git = get_string("git")?;
    let branch = get_string("branch")?;
    let tag = get_string("tag")?;
    let rev = get_string("rev")?;

    // Mutual-exclusion: path vs git.
    if path_field.is_some() && git.is_some() {
        return Err(invalid(
            "`path` and `git` are mutually exclusive — pick one source".to_string(),
        ));
    }

    // Refs (branch/tag/rev) only apply to git deps and at most one is allowed.
    let ref_count = [&branch, &tag, &rev].iter().filter(|r| r.is_some()).count();
    if ref_count > 1 {
        return Err(invalid(
            "at most one of `branch`, `tag`, `rev` may be set".to_string(),
        ));
    }
    if ref_count > 0 && git.is_none() {
        return Err(invalid(
            "`branch` / `tag` / `rev` are only valid on a git dependency".to_string(),
        ));
    }

    if let Some(url) = git {
        let reference = if let Some(b) = branch {
            Some(GitRef::Branch(b))
        } else if let Some(t) = tag {
            Some(GitRef::Tag(t))
        } else {
            rev.map(GitRef::Rev)
        };
        return Ok(DependencySpec::Git {
            url,
            reference,
            version,
        });
    }

    if let Some(p) = path_field {
        return Ok(DependencySpec::Path {
            path: PathBuf::from(p),
            version,
        });
    }

    // Neither path nor git — must be a registry entry, which requires `version`.
    match version {
        Some(v) => Ok(DependencySpec::Registry { version: v }),
        None => Err(invalid(
            "missing required field — provide `version`, `path`, or `git`".to_string(),
        )),
    }
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

/// Parse the optional `[test].timeout_seconds = N` key — `karac test` uses it
/// as the package-wide per-test timeout default (phase-7 line 847 sub-step 2).
/// Numeric, no unit suffix (seconds); must be a positive integer. Wrong
/// shapes (the `[test]` parent isn't a table, the value isn't an integer, or
/// the integer is `<= 0`) are hard errors so a manifest typo surfaces
/// immediately rather than silently falling back to the env-var / 30 s
/// default. Returns `None` when the key (or the `[test]` table) is absent.
fn parse_test_timeout_seconds(
    path: &Path,
    table: &toml::Table,
) -> Result<Option<u64>, ManifestError> {
    let Some(test_value) = table.get("test") else {
        return Ok(None);
    };
    let test_table = test_value
        .as_table()
        .ok_or_else(|| ManifestError::InvalidFieldType {
            path: path.to_path_buf(),
            key: "test".to_string(),
            expected: "a table (e.g. `[test]`)",
        })?;
    let Some(value) = test_table.get("timeout_seconds") else {
        return Ok(None);
    };
    let n =
        value
            .as_integer()
            .filter(|n| *n > 0)
            .ok_or_else(|| ManifestError::InvalidFieldType {
                path: path.to_path_buf(),
                key: "test.timeout_seconds".to_string(),
                expected: "a positive integer (seconds)",
            })?;
    Ok(Some(n as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p() -> PathBuf {
        PathBuf::from("kara.toml")
    }

    /// Terse `VersionReq` construction for assertions. Test failures from a
    /// bad literal would surface as a panic on the next line so a `.unwrap()`
    /// here is fine — these strings are owned by the test source.
    fn req(s: &str) -> semver::VersionReq {
        semver::VersionReq::parse(s).unwrap()
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
        // `[dependencies]`, `[dev-dependencies]` are structurally captured
        // and `[build].target` is captured as build_default_target (line
        // 882) — well-formed entries emit no warnings.
        assert!(m.warnings.is_empty());
        assert_eq!(m.build_default_target.as_deref(), Some("x86_64-linux"));
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

    // ── [test].timeout_seconds (phase-7 line 847 sub-step 2) ───────

    #[test]
    fn no_test_timeout_seconds_is_none() {
        let src = r#"[package]
name = "hello"
"#;
        let m = parse_manifest(&p(), src).unwrap();
        assert_eq!(m.test_timeout_seconds, None);
    }

    #[test]
    fn test_timeout_seconds_parses() {
        let src = r#"[package]
name = "hello"

[test]
timeout_seconds = 5
"#;
        let m = parse_manifest(&p(), src).unwrap();
        assert_eq!(m.test_timeout_seconds, Some(5));
    }

    #[test]
    fn test_timeout_seconds_coexists_with_test_resources() {
        // Both live under the same `[test]` table — parsing one must not
        // disturb the other.
        let src = r#"[package]
name = "hello"

[test]
timeout_seconds = 12

[test.resources]
"db.UserDB" = "pg_isready"
"#;
        let m = parse_manifest(&p(), src).unwrap();
        assert_eq!(m.test_timeout_seconds, Some(12));
        assert_eq!(
            m.test_resources.get("db.UserDB").map(String::as_str),
            Some("pg_isready"),
        );
    }

    #[test]
    fn test_timeout_seconds_rejects_non_integer() {
        let src = r#"[package]
name = "hello"

[test]
timeout_seconds = "5"
"#;
        let err = parse_manifest(&p(), src).unwrap_err();
        match err {
            ManifestError::InvalidFieldType { key, .. } => {
                assert_eq!(key, "test.timeout_seconds")
            }
            other => panic!("expected InvalidFieldType, got {other:?}"),
        }
    }

    #[test]
    fn test_timeout_seconds_rejects_zero() {
        let src = r#"[package]
name = "hello"

[test]
timeout_seconds = 0
"#;
        let err = parse_manifest(&p(), src).unwrap_err();
        match err {
            ManifestError::InvalidFieldType { key, .. } => {
                assert_eq!(key, "test.timeout_seconds")
            }
            other => panic!("expected InvalidFieldType, got {other:?}"),
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
        assert_eq!(m.kara_version, Some(req("1.0")));
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
        assert_eq!(m.kara_version, Some(req("1.2.3")));
    }

    #[test]
    fn kara_version_accepts_caret_constraint() {
        // Slice 6 lifted the raw string into a VersionReq so the resolver
        // can intersect it against the active toolchain version.
        let src = r#"[package]
name = "hello"
kara-version = "^1.0"
"#;
        let m = parse_manifest(&p(), src).unwrap();
        assert_eq!(m.kara_version, Some(req("^1.0")));
    }

    #[test]
    fn kara_version_malformed_string_is_hard_error() {
        // Slice 6: lifting to VersionReq adds parse-shape validation on
        // top of the existing non-empty / non-whitespace checks.
        let src = r#"[package]
name = "hello"
kara-version = "not-a-version"
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

    // ── PubGrub resolver slice 1: dependency parsing ──────────────

    #[test]
    fn dependencies_absent_yields_empty_map() {
        let src = r#"[package]
name = "hello"
"#;
        let m = parse_manifest(&p(), src).unwrap();
        assert!(m.dependencies.is_empty());
        assert!(m.dev_dependencies.is_empty());
    }

    #[test]
    fn dependency_bare_string_is_registry_shorthand() {
        let src = r#"[package]
name = "hello"

[dependencies]
http = "1.2"
"#;
        let m = parse_manifest(&p(), src).unwrap();
        assert_eq!(
            m.dependencies.get("http"),
            Some(&DependencySpec::Registry {
                version: req("1.2")
            }),
        );
    }

    #[test]
    fn dependency_inline_table_version_only_is_registry() {
        let src = r#"[package]
name = "hello"

[dependencies]
http = { version = "1.2" }
"#;
        let m = parse_manifest(&p(), src).unwrap();
        assert_eq!(
            m.dependencies.get("http"),
            Some(&DependencySpec::Registry {
                version: req("1.2")
            }),
        );
    }

    #[test]
    fn dependency_path_form_parses() {
        let src = r#"[package]
name = "hello"

[dependencies]
logging = { path = "../logging" }
"#;
        let m = parse_manifest(&p(), src).unwrap();
        assert_eq!(
            m.dependencies.get("logging"),
            Some(&DependencySpec::Path {
                path: PathBuf::from("../logging"),
                version: None,
            }),
        );
    }

    #[test]
    fn dependency_path_with_version_parses() {
        let src = r#"[package]
name = "hello"

[dependencies]
logging = { path = "../logging", version = "0.2" }
"#;
        let m = parse_manifest(&p(), src).unwrap();
        assert_eq!(
            m.dependencies.get("logging"),
            Some(&DependencySpec::Path {
                path: PathBuf::from("../logging"),
                version: Some(req("0.2")),
            }),
        );
    }

    #[test]
    fn dependency_git_no_ref_parses() {
        let src = r#"[package]
name = "hello"

[dependencies]
json = { git = "https://example.com/json-kara" }
"#;
        let m = parse_manifest(&p(), src).unwrap();
        assert_eq!(
            m.dependencies.get("json"),
            Some(&DependencySpec::Git {
                url: "https://example.com/json-kara".to_string(),
                reference: None,
                version: None,
            }),
        );
    }

    #[test]
    fn dependency_git_branch_parses() {
        let src = r#"[package]
name = "hello"

[dependencies]
json = { git = "https://example.com/json-kara", branch = "main" }
"#;
        let m = parse_manifest(&p(), src).unwrap();
        assert_eq!(
            m.dependencies.get("json"),
            Some(&DependencySpec::Git {
                url: "https://example.com/json-kara".to_string(),
                reference: Some(GitRef::Branch("main".to_string())),
                version: None,
            }),
        );
    }

    #[test]
    fn dependency_git_tag_parses() {
        let src = r#"[package]
name = "hello"

[dependencies]
json = { git = "https://example.com/json-kara", tag = "v1.0" }
"#;
        let m = parse_manifest(&p(), src).unwrap();
        assert_eq!(
            m.dependencies.get("json"),
            Some(&DependencySpec::Git {
                url: "https://example.com/json-kara".to_string(),
                reference: Some(GitRef::Tag("v1.0".to_string())),
                version: None,
            }),
        );
    }

    #[test]
    fn dependency_git_rev_parses() {
        let src = r#"[package]
name = "hello"

[dependencies]
json = { git = "https://example.com/json-kara", rev = "abc123" }
"#;
        let m = parse_manifest(&p(), src).unwrap();
        assert_eq!(
            m.dependencies.get("json"),
            Some(&DependencySpec::Git {
                url: "https://example.com/json-kara".to_string(),
                reference: Some(GitRef::Rev("abc123".to_string())),
                version: None,
            }),
        );
    }

    #[test]
    fn dependency_git_with_version_parses() {
        // The spec's example: registry version constraint + git as the
        // override fetch source. Captured as Git with a populated `version`.
        let src = r#"[package]
name = "hello"

[dependencies]
json = { version = "0.8", git = "https://example.com/json-kara" }
"#;
        let m = parse_manifest(&p(), src).unwrap();
        assert_eq!(
            m.dependencies.get("json"),
            Some(&DependencySpec::Git {
                url: "https://example.com/json-kara".to_string(),
                reference: None,
                version: Some(req("0.8")),
            }),
        );
    }

    #[test]
    fn dev_dependencies_parse_with_same_shape() {
        let src = r#"[package]
name = "hello"

[dev-dependencies]
proptest = "0.4"
mocktest = { path = "../mocktest" }
"#;
        let m = parse_manifest(&p(), src).unwrap();
        assert!(m.dependencies.is_empty());
        assert_eq!(m.dev_dependencies.len(), 2);
        assert_eq!(
            m.dev_dependencies.get("proptest"),
            Some(&DependencySpec::Registry {
                version: req("0.4")
            }),
        );
        assert_eq!(
            m.dev_dependencies.get("mocktest"),
            Some(&DependencySpec::Path {
                path: PathBuf::from("../mocktest"),
                version: None,
            }),
        );
    }

    #[test]
    fn multiple_dependencies_preserve_sorted_order() {
        // BTreeMap iteration is alphabetic — useful regression pin for the
        // resolver's diagnostic output, which surfaces constraint chains in
        // a deterministic order.
        let src = r#"[package]
name = "hello"

[dependencies]
zebra = "1.0"
alpha = "0.1"
mango = { path = "../mango" }
"#;
        let m = parse_manifest(&p(), src).unwrap();
        let names: Vec<&String> = m.dependencies.keys().collect();
        assert_eq!(names, vec!["alpha", "mango", "zebra"]);
    }

    #[test]
    fn dependency_path_and_git_mutually_exclusive() {
        let src = r#"[package]
name = "hello"

[dependencies]
broken = { path = "../broken", git = "https://example.com/broken" }
"#;
        let err = parse_manifest(&p(), src).unwrap_err();
        match err {
            ManifestError::InvalidDependencySpec { name, message, .. } => {
                assert_eq!(name, "broken");
                assert!(
                    message.contains("mutually exclusive"),
                    "expected mutual-exclusion message; got `{message}`",
                );
            }
            other => panic!("expected InvalidDependencySpec, got {other:?}"),
        }
    }

    #[test]
    fn dependency_branch_and_tag_rejected() {
        let src = r#"[package]
name = "hello"

[dependencies]
broken = { git = "https://example.com/broken", branch = "main", tag = "v1.0" }
"#;
        let err = parse_manifest(&p(), src).unwrap_err();
        match err {
            ManifestError::InvalidDependencySpec { name, message, .. } => {
                assert_eq!(name, "broken");
                assert!(
                    message.contains("at most one of"),
                    "expected ref-arity message; got `{message}`",
                );
            }
            other => panic!("expected InvalidDependencySpec, got {other:?}"),
        }
    }

    #[test]
    fn dependency_branch_without_git_rejected() {
        let src = r#"[package]
name = "hello"

[dependencies]
broken = { version = "1.0", branch = "main" }
"#;
        let err = parse_manifest(&p(), src).unwrap_err();
        match err {
            ManifestError::InvalidDependencySpec { name, message, .. } => {
                assert_eq!(name, "broken");
                assert!(
                    message.contains("only valid on a git dependency"),
                    "expected git-only message; got `{message}`",
                );
            }
            other => panic!("expected InvalidDependencySpec, got {other:?}"),
        }
    }

    #[test]
    fn dependency_missing_source_rejected() {
        let src = r#"[package]
name = "hello"

[dependencies]
broken = { }
"#;
        let err = parse_manifest(&p(), src).unwrap_err();
        match err {
            ManifestError::InvalidDependencySpec { name, message, .. } => {
                assert_eq!(name, "broken");
                assert!(
                    message.contains("missing required field"),
                    "expected missing-source message; got `{message}`",
                );
            }
            other => panic!("expected InvalidDependencySpec, got {other:?}"),
        }
    }

    #[test]
    fn dependency_empty_version_rejected() {
        let src = r#"[package]
name = "hello"

[dependencies]
broken = ""
"#;
        let err = parse_manifest(&p(), src).unwrap_err();
        match err {
            ManifestError::InvalidDependencySpec { name, message, .. } => {
                assert_eq!(name, "broken");
                assert!(
                    message.contains("empty"),
                    "expected empty-version message; got `{message}`",
                );
            }
            other => panic!("expected InvalidDependencySpec, got {other:?}"),
        }
    }

    #[test]
    fn dependency_wrong_value_type_rejected() {
        let src = r#"[package]
name = "hello"

[dependencies]
broken = 42
"#;
        let err = parse_manifest(&p(), src).unwrap_err();
        match err {
            ManifestError::InvalidDependencySpec { name, message, .. } => {
                assert_eq!(name, "broken");
                assert!(
                    message.contains("version string or an inline table"),
                    "expected wrong-shape message; got `{message}`",
                );
            }
            other => panic!("expected InvalidDependencySpec, got {other:?}"),
        }
    }

    #[test]
    fn dependency_version_field_wrong_type_rejected() {
        let src = r#"[package]
name = "hello"

[dependencies]
broken = { version = 42 }
"#;
        let err = parse_manifest(&p(), src).unwrap_err();
        match err {
            ManifestError::InvalidDependencySpec { name, message, .. } => {
                assert_eq!(name, "broken");
                assert!(
                    message.contains("`version` must be a string"),
                    "expected version-type message; got `{message}`",
                );
            }
            other => panic!("expected InvalidDependencySpec, got {other:?}"),
        }
    }

    #[test]
    fn dependency_table_wrong_shape_rejected() {
        // Top-level `dependencies = "..."` must precede the `[package]`
        // header so TOML treats it as a top-level scalar (otherwise it would
        // land inside the `[package]` table per TOML's continuation rule).
        let src = r#"dependencies = "not-a-table"

[package]
name = "hello"
"#;
        let err = parse_manifest(&p(), src).unwrap_err();
        match err {
            ManifestError::InvalidFieldType { key, .. } => assert_eq!(key, "dependencies"),
            other => panic!("expected InvalidFieldType, got {other:?}"),
        }
    }

    #[test]
    fn dependency_unknown_key_soft_warns() {
        let src = r#"[package]
name = "hello"

[dependencies]
http = { version = "1.0", features = ["derive"] }
"#;
        let m = parse_manifest(&p(), src).unwrap();
        assert_eq!(
            m.dependencies.get("http"),
            Some(&DependencySpec::Registry {
                version: req("1.0")
            }),
        );
        assert_eq!(m.warnings.len(), 1);
        assert!(
            m.warnings[0].message.contains("features"),
            "expected unknown-key warning to mention `features`; got `{}`",
            m.warnings[0].message,
        );
    }

    #[test]
    fn dependency_workspace_form_parses() {
        // `workspace = true` (design.md § Workspaces) is captured as a
        // dedicated variant — the workspace-resolver slice dereferences it
        // against `[workspace.dependencies]` at the workspace root before
        // the resolver runs. Slice 1 only captures the intent.
        let src = r#"[package]
name = "hello"

[dependencies]
http = { workspace = true }
"#;
        let m = parse_manifest(&p(), src).unwrap();
        assert_eq!(m.dependencies.get("http"), Some(&DependencySpec::Workspace));
        assert!(m.warnings.is_empty(), "got warnings: {:?}", m.warnings);
    }

    #[test]
    fn dependency_workspace_false_rejected() {
        let src = r#"[package]
name = "hello"

[dependencies]
http = { workspace = false }
"#;
        let err = parse_manifest(&p(), src).unwrap_err();
        match err {
            ManifestError::InvalidDependencySpec { name, message, .. } => {
                assert_eq!(name, "http");
                assert!(
                    message.contains("must be the literal value `true`"),
                    "expected workspace-true-only message; got `{message}`",
                );
            }
            other => panic!("expected InvalidDependencySpec, got {other:?}"),
        }
    }

    #[test]
    fn dependency_workspace_with_version_rejected() {
        let src = r#"[package]
name = "hello"

[dependencies]
http = { workspace = true, version = "1.0" }
"#;
        let err = parse_manifest(&p(), src).unwrap_err();
        match err {
            ManifestError::InvalidDependencySpec { name, message, .. } => {
                assert_eq!(name, "http");
                assert!(
                    message.contains("cannot be combined"),
                    "expected combination-rejection message; got `{message}`",
                );
            }
            other => panic!("expected InvalidDependencySpec, got {other:?}"),
        }
    }

    // ── PubGrub resolver slice 2: semver-constraint vocabulary ────

    #[test]
    fn semver_caret_constraint_parses() {
        let src = r#"[package]
name = "hello"

[dependencies]
http = "^1.2.0"
"#;
        let m = parse_manifest(&p(), src).unwrap();
        // Caret matches `^1.2.0` — same as the bare-string `"1.2"` shorthand.
        assert_eq!(
            m.dependencies.get("http"),
            Some(&DependencySpec::Registry {
                version: req("^1.2.0")
            }),
        );
    }

    #[test]
    fn semver_exact_constraint_parses() {
        let src = r#"[package]
name = "hello"

[dependencies]
http = "=1.2.3"
"#;
        let m = parse_manifest(&p(), src).unwrap();
        assert_eq!(
            m.dependencies.get("http"),
            Some(&DependencySpec::Registry {
                version: req("=1.2.3")
            }),
        );
    }

    #[test]
    fn semver_range_constraint_parses() {
        let src = r#"[package]
name = "hello"

[dependencies]
http = ">=1.0, <1.5"
"#;
        let m = parse_manifest(&p(), src).unwrap();
        assert_eq!(
            m.dependencies.get("http"),
            Some(&DependencySpec::Registry {
                version: req(">=1.0, <1.5")
            }),
        );
    }

    #[test]
    fn semver_tilde_constraint_parses() {
        let src = r#"[package]
name = "hello"

[dependencies]
http = "~1.2"
"#;
        let m = parse_manifest(&p(), src).unwrap();
        assert_eq!(
            m.dependencies.get("http"),
            Some(&DependencySpec::Registry {
                version: req("~1.2")
            }),
        );
    }

    #[test]
    fn semver_wildcard_constraint_parses() {
        let src = r#"[package]
name = "hello"

[dependencies]
http = "*"
"#;
        let m = parse_manifest(&p(), src).unwrap();
        assert_eq!(
            m.dependencies.get("http"),
            Some(&DependencySpec::Registry { version: req("*") }),
        );
    }

    #[test]
    fn semver_bare_one_segment_parses() {
        // Bare `"1"` is `^1` — `>=1.0.0, <2.0.0`. Useful regression pin so
        // the resolver doesn't insist on three segments.
        let src = r#"[package]
name = "hello"

[dependencies]
http = "1"
"#;
        let m = parse_manifest(&p(), src).unwrap();
        assert_eq!(
            m.dependencies.get("http"),
            Some(&DependencySpec::Registry { version: req("1") }),
        );
    }

    #[test]
    fn semver_malformed_string_is_hard_error() {
        let src = r#"[package]
name = "hello"

[dependencies]
http = "not-a-version"
"#;
        let err = parse_manifest(&p(), src).unwrap_err();
        match err {
            ManifestError::InvalidDependencySpec { name, message, .. } => {
                assert_eq!(name, "http");
                assert!(
                    message.contains("not a valid semver requirement"),
                    "expected semver parse failure; got `{message}`",
                );
            }
            other => panic!("expected InvalidDependencySpec, got {other:?}"),
        }
    }

    #[test]
    fn semver_malformed_inline_table_version_is_hard_error() {
        let src = r#"[package]
name = "hello"

[dependencies]
http = { version = ">>> bogus" }
"#;
        let err = parse_manifest(&p(), src).unwrap_err();
        match err {
            ManifestError::InvalidDependencySpec { name, message, .. } => {
                assert_eq!(name, "http");
                assert!(
                    message.contains("not a valid semver requirement"),
                    "expected semver parse failure; got `{message}`",
                );
            }
            other => panic!("expected InvalidDependencySpec, got {other:?}"),
        }
    }

    #[test]
    fn semver_constraint_matches_compatible_version() {
        // Pin that the parsed VersionReq is the same object pubgrub will
        // intersect — a `0.8` constraint accepts `0.8.5` (caret default) and
        // rejects `0.7.0`. Compares using semver's own matches semantics.
        let src = r#"[package]
name = "hello"

[dependencies]
http = "0.8"
"#;
        let m = parse_manifest(&p(), src).unwrap();
        let DependencySpec::Registry { version: vr } = m.dependencies.get("http").unwrap() else {
            panic!("expected Registry spec");
        };
        assert!(vr.matches(&semver::Version::parse("0.8.5").unwrap()));
        assert!(!vr.matches(&semver::Version::parse("0.7.0").unwrap()));
        assert!(!vr.matches(&semver::Version::parse("1.0.0").unwrap()));
    }

    #[test]
    fn warnings_remain_sorted_across_package_and_dependency_sources() {
        // Regression pin: warnings from `[package]` + `[dependencies]` should
        // merge into a single sorted list so diagnostic output is
        // deterministic regardless of insertion order.
        let src = r#"[package]
name = "hello"
zzhomepage = "https://example.com"

[dependencies]
http = { version = "1.0", aaunknown = "x" }
"#;
        let m = parse_manifest(&p(), src).unwrap();
        assert_eq!(m.warnings.len(), 2);
        let messages: Vec<&str> = m.warnings.iter().map(|w| w.message.as_str()).collect();
        let mut sorted = messages.clone();
        sorted.sort();
        assert_eq!(messages, sorted);
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

    // ── line 882: [target.X.dependencies] / [target.X.profile] ───

    #[test]
    fn target_dependencies_parse_into_per_triple_map() {
        let src = r#"[package]
name = "hello"

[target."x86_64-apple-darwin".dependencies]
http = "1.0"

[target."aarch64-unknown-linux-gnu".dependencies]
http = "2.0"
"#;
        let m = parse_manifest(&p(), src).unwrap();
        assert!(m.dependencies.is_empty());
        assert!(m.warnings.is_empty());
        let mac = m.target_dependencies.get("x86_64-apple-darwin").unwrap();
        assert_eq!(
            mac.get("http"),
            Some(&DependencySpec::Registry {
                version: req("1.0")
            })
        );
        let lin = m
            .target_dependencies
            .get("aarch64-unknown-linux-gnu")
            .unwrap();
        assert_eq!(
            lin.get("http"),
            Some(&DependencySpec::Registry {
                version: req("2.0")
            })
        );
    }

    #[test]
    fn target_dev_dependencies_parse_into_per_triple_map() {
        let src = r#"[package]
name = "hello"

[target."x86_64-apple-darwin".dev-dependencies]
proptest = "0.4"
"#;
        let m = parse_manifest(&p(), src).unwrap();
        assert!(m.dev_dependencies.is_empty());
        let mac = m
            .target_dev_dependencies
            .get("x86_64-apple-darwin")
            .unwrap();
        assert_eq!(
            mac.get("proptest"),
            Some(&DependencySpec::Registry {
                version: req("0.4")
            })
        );
    }

    #[test]
    fn target_profile_override_parses_to_compile_profile() {
        // `profile` is a string key inside the per-triple table — pin
        // both supported values (`embedded` and `kernel`) so a future
        // refactor that drops a profile from the parser surfaces here.
        let src = r#"[package]
name = "hello"
profile = "default"

[target."thumbv7em-none-eabi"]
profile = "embedded"

[target."x86_64-apple-darwin"]
profile = "kernel"
"#;
        let m = parse_manifest(&p(), src).unwrap();
        assert_eq!(m.profile, CompileProfile::Default);
        assert_eq!(
            m.target_profile_overrides.get("thumbv7em-none-eabi"),
            Some(&CompileProfile::Embedded)
        );
        assert_eq!(
            m.target_profile_overrides.get("x86_64-apple-darwin"),
            Some(&CompileProfile::Kernel)
        );
    }

    #[test]
    fn target_unknown_inner_key_soft_warns() {
        let src = r#"[package]
name = "hello"

[target."x86_64-apple-darwin"]
opt-level = 3
"#;
        let m = parse_manifest(&p(), src).unwrap();
        assert_eq!(m.warnings.len(), 1);
        assert!(
            m.warnings[0].message.contains("target.x86_64-apple-darwin"),
            "warning should name the triple; got: {}",
            m.warnings[0].message
        );
        assert!(m.warnings[0].message.contains("opt-level"));
    }

    #[test]
    fn target_invalid_profile_value_is_hard_error() {
        let src = r#"[package]
name = "hello"

[target."x86_64-apple-darwin"]
profile = "high-speed"
"#;
        let err = parse_manifest(&p(), src).unwrap_err();
        match err {
            ManifestError::InvalidFieldType { key, .. } => {
                assert_eq!(key, "target.x86_64-apple-darwin.profile");
            }
            other => panic!("expected InvalidFieldType, got {other:?}"),
        }
    }

    #[test]
    fn target_invalid_dependency_shape_is_hard_error() {
        let src = r#"[package]
name = "hello"

[target."x86_64-apple-darwin".dependencies]
http = 42
"#;
        let err = parse_manifest(&p(), src).unwrap_err();
        assert!(matches!(err, ManifestError::InvalidDependencySpec { .. }));
    }

    #[test]
    fn target_section_must_be_table_when_not_a_triple() {
        // `[target]` value at the manifest root must be a table — a
        // scalar surfaces as the generic InvalidFieldType. Top-level
        // keys must come before any [table] section in TOML, so the
        // pre-table position is the only way to write this shape.
        let src = r#"target = 42

[package]
name = "hello"
"#;
        let err = parse_manifest(&p(), src).unwrap_err();
        match err {
            ManifestError::InvalidFieldType { key, .. } => assert_eq!(key, "target"),
            other => panic!("expected InvalidFieldType on `target`, got {other:?}"),
        }
    }

    #[test]
    fn build_default_target_captured_from_build_section() {
        let src = r#"[package]
name = "hello"

[build]
target = "x86_64-apple-darwin"
"#;
        let m = parse_manifest(&p(), src).unwrap();
        assert_eq!(
            m.build_default_target.as_deref(),
            Some("x86_64-apple-darwin")
        );
    }

    #[test]
    fn build_default_target_absent_is_none() {
        let src = r#"[package]
name = "hello"
"#;
        let m = parse_manifest(&p(), src).unwrap();
        assert_eq!(m.build_default_target, None);
    }

    #[test]
    fn build_default_target_wrong_type_is_hard_error() {
        let src = r#"[package]
name = "hello"

[build]
target = 42
"#;
        let err = parse_manifest(&p(), src).unwrap_err();
        match err {
            ManifestError::InvalidFieldType { key, .. } => assert_eq!(key, "build.target"),
            other => panic!("expected InvalidFieldType on `build.target`, got {other:?}"),
        }
    }

    #[test]
    fn build_default_target_empty_string_is_hard_error() {
        let src = r#"[package]
name = "hello"

[build]
target = "   "
"#;
        let err = parse_manifest(&p(), src).unwrap_err();
        match err {
            ManifestError::InvalidFieldType { key, .. } => assert_eq!(key, "build.target"),
            other => panic!("expected InvalidFieldType on `build.target`, got {other:?}"),
        }
    }

    // ── `[build].targets` — multi-target check matrix (phase-10) ──

    #[test]
    fn build_targets_captured_in_declaration_order() {
        let src = r#"[package]
name = "hello"

[build]
targets = ["wasm_browser", "native"]
"#;
        let m = parse_manifest(&p(), src).unwrap();
        assert_eq!(m.build_targets, vec!["wasm_browser", "native"]);
        assert!(m.warnings.is_empty());
    }

    #[test]
    fn build_targets_absent_is_empty() {
        let src = r#"[package]
name = "hello"
"#;
        let m = parse_manifest(&p(), src).unwrap();
        assert!(m.build_targets.is_empty());
    }

    #[test]
    fn build_targets_unknown_name_is_hard_error() {
        // A soft warning would silently drop the target from a CI
        // verification matrix — the field's whole job is to prevent that.
        let src = r#"[package]
name = "hello"

[build]
targets = ["native", "wasm_wsi"]
"#;
        let err = parse_manifest(&p(), src).unwrap_err();
        match err {
            ManifestError::InvalidBuildTargets { message, .. } => {
                assert!(message.contains("unknown target 'wasm_wsi'"), "{message}");
                assert!(message.contains("wasm_wasi"), "valid set listed: {message}");
            }
            other => panic!("expected InvalidBuildTargets, got {other:?}"),
        }
    }

    #[test]
    fn build_targets_duplicate_is_hard_error() {
        let src = r#"[package]
name = "hello"

[build]
targets = ["native", "native"]
"#;
        let err = parse_manifest(&p(), src).unwrap_err();
        match err {
            ManifestError::InvalidBuildTargets { message, .. } => {
                assert!(message.contains("duplicate target 'native'"), "{message}");
            }
            other => panic!("expected InvalidBuildTargets, got {other:?}"),
        }
    }

    #[test]
    fn build_targets_non_array_is_hard_error() {
        let src = r#"[package]
name = "hello"

[build]
targets = "native"
"#;
        let err = parse_manifest(&p(), src).unwrap_err();
        match err {
            ManifestError::InvalidFieldType { key, .. } => assert_eq!(key, "build.targets"),
            other => panic!("expected InvalidFieldType on `build.targets`, got {other:?}"),
        }
    }

    #[test]
    fn build_targets_non_string_entry_is_hard_error() {
        let src = r#"[package]
name = "hello"

[build]
targets = ["native", 42]
"#;
        let err = parse_manifest(&p(), src).unwrap_err();
        match err {
            ManifestError::InvalidBuildTargets { message, .. } => {
                assert!(message.contains("entries must be strings"), "{message}");
            }
            other => panic!("expected InvalidBuildTargets, got {other:?}"),
        }
    }

    #[test]
    fn release_target_cpu_parses() {
        let src = r#"[package]
name = "hello"

[release]
target-cpu = "apple-m4"
"#;
        let m = parse_manifest(&p(), src).unwrap();
        assert_eq!(m.release_target_cpu.as_deref(), Some("apple-m4"));
        assert!(m.warnings.is_empty());
    }

    #[test]
    fn release_target_cpu_absent_is_none() {
        let src = r#"[package]
name = "hello"
"#;
        let m = parse_manifest(&p(), src).unwrap();
        assert!(m.release_target_cpu.is_none());
        // An empty [release] table is also fine.
        let src = "[package]\nname = \"hello\"\n\n[release]\n";
        let m = parse_manifest(&p(), src).unwrap();
        assert!(m.release_target_cpu.is_none());
    }

    #[test]
    fn release_target_cpu_wrong_type_is_hard_error() {
        // A typo'd value must not silently drop the override — same
        // posture as `[build].targets`.
        for src in [
            "[package]\nname = \"hello\"\n\n[release]\ntarget-cpu = 42\n",
            "[package]\nname = \"hello\"\n\n[release]\ntarget-cpu = \"\"\n",
        ] {
            let err = parse_manifest(&p(), src).unwrap_err();
            match err {
                ManifestError::InvalidFieldType { key, .. } => {
                    assert_eq!(key, "release.target-cpu")
                }
                other => panic!("expected InvalidFieldType on `release.target-cpu`, got {other:?}"),
            }
        }
    }

    #[test]
    fn release_unknown_key_soft_warns() {
        let src = r#"[package]
name = "hello"

[release]
target-cpu = "x86-64-v3"
lto = "fat"
"#;
        let m = parse_manifest(&p(), src).unwrap();
        assert_eq!(m.release_target_cpu.as_deref(), Some("x86-64-v3"));
        assert!(
            m.warnings
                .iter()
                .any(|w| w.message.contains("unknown key `[release].lto`")),
            "expected a soft warning for the unknown key, got: {:?}",
            m.warnings,
        );
    }

    #[test]
    fn release_target_features_parses() {
        let src = r#"[package]
name = "hello"

[release]
target-cpu = "apple-m4"
target-features = "+aes,-outline-atomics"
"#;
        let m = parse_manifest(&p(), src).unwrap();
        assert_eq!(m.release_target_cpu.as_deref(), Some("apple-m4"));
        assert_eq!(
            m.release_target_features.as_deref(),
            Some("+aes,-outline-atomics")
        );
        assert!(m.warnings.is_empty());
        // Absent key → None (independent of target-cpu's presence).
        let src = "[package]\nname = \"hello\"\n\n[release]\ntarget-cpu = \"apple-m4\"\n";
        let m = parse_manifest(&p(), src).unwrap();
        assert!(m.release_target_features.is_none());
    }

    #[test]
    fn release_target_features_wrong_type_is_hard_error() {
        for src in [
            "[package]\nname = \"hello\"\n\n[release]\ntarget-features = 42\n",
            "[package]\nname = \"hello\"\n\n[release]\ntarget-features = \"\"\n",
        ] {
            let err = parse_manifest(&p(), src).unwrap_err();
            match err {
                ManifestError::InvalidFieldType { key, .. } => {
                    assert_eq!(key, "release.target-features")
                }
                other => {
                    panic!("expected InvalidFieldType on `release.target-features`, got {other:?}")
                }
            }
        }
    }

    #[test]
    fn toolchain_wasm_tools_parses() {
        let src = r#"[package]
name = "hello"

[toolchain]
wasm-tools = "1.251.0"
"#;
        let m = parse_manifest(&p(), src).unwrap();
        assert_eq!(m.toolchain_wasm_tools.as_deref(), Some("1.251.0"));
        assert!(m.warnings.is_empty());
        // Absent table → None; empty [toolchain] table is also fine.
        let src = "[package]\nname = \"hello\"\n";
        let m = parse_manifest(&p(), src).unwrap();
        assert!(m.toolchain_wasm_tools.is_none());
        let src = "[package]\nname = \"hello\"\n\n[toolchain]\n";
        let m = parse_manifest(&p(), src).unwrap();
        assert!(m.toolchain_wasm_tools.is_none());
    }

    #[test]
    fn toolchain_wasm_tools_wrong_type_is_hard_error_and_unknown_key_soft_warns() {
        // A typo'd pin must not silently accept any version — same posture
        // as `[release].target-cpu`.
        for src in [
            "[package]\nname = \"hello\"\n\n[toolchain]\nwasm-tools = 1\n",
            "[package]\nname = \"hello\"\n\n[toolchain]\nwasm-tools = \"\"\n",
        ] {
            let err = parse_manifest(&p(), src).unwrap_err();
            match err {
                ManifestError::InvalidFieldType { key, .. } => {
                    assert_eq!(key, "toolchain.wasm-tools")
                }
                other => {
                    panic!("expected InvalidFieldType on `toolchain.wasm-tools`, got {other:?}")
                }
            }
        }
        let src = "[package]\nname = \"hello\"\n\n[toolchain]\nwit-bindgen = \"0.40.0\"\n";
        let m = parse_manifest(&p(), src).unwrap();
        assert!(m.toolchain_wasm_tools.is_none());
        assert!(
            m.warnings
                .iter()
                .any(|w| w.message.contains("unknown key `[toolchain].wit-bindgen`")),
            "expected a soft warning for the unknown key, got: {:?}",
            m.warnings,
        );
    }

    #[test]
    fn wasm_table_parses() {
        let src = r#"[package]
name = "hello"

[wasm]
pool-size = 8
fallback = false
max-memory-pages = 4096
"#;
        let m = parse_manifest(&p(), src).unwrap();
        assert_eq!(m.wasm_pool_size, Some(8));
        assert_eq!(m.wasm_fallback, Some(false));
        assert_eq!(m.wasm_max_memory_pages, Some(4096));
        assert!(m.warnings.is_empty());
        // Absent table → all None; empty [wasm] table is also fine.
        let src = "[package]\nname = \"hello\"\n";
        let m = parse_manifest(&p(), src).unwrap();
        assert!(m.wasm_pool_size.is_none());
        assert!(m.wasm_fallback.is_none());
        assert!(m.wasm_max_memory_pages.is_none());
        let src = "[package]\nname = \"hello\"\n\n[wasm]\n";
        let m = parse_manifest(&p(), src).unwrap();
        assert!(m.wasm_pool_size.is_none());
    }

    #[test]
    fn wasm_table_wrong_type_is_hard_error() {
        // A typo'd knob must not silently drop the override — same
        // posture as `[release].target-cpu`. Non-positive integers are
        // the integer-key typo shape (`pool-size = 0` can't mean
        // anything; `-1` is a sign error).
        for (src, key) in [
            (
                "[package]\nname = \"hello\"\n\n[wasm]\npool-size = \"8\"\n",
                "wasm.pool-size",
            ),
            (
                "[package]\nname = \"hello\"\n\n[wasm]\npool-size = 0\n",
                "wasm.pool-size",
            ),
            (
                "[package]\nname = \"hello\"\n\n[wasm]\nfallback = \"false\"\n",
                "wasm.fallback",
            ),
            (
                "[package]\nname = \"hello\"\n\n[wasm]\nmax-memory-pages = -1\n",
                "wasm.max-memory-pages",
            ),
        ] {
            let err = parse_manifest(&p(), src).unwrap_err();
            match err {
                ManifestError::InvalidFieldType { key: k, .. } => assert_eq!(k, key),
                other => panic!("expected InvalidFieldType on `{key}`, got {other:?}"),
            }
        }
    }

    #[test]
    fn wasm_table_unknown_key_soft_warns() {
        let src = "[package]\nname = \"hello\"\n\n[wasm]\nstack-size = 4\n";
        let m = parse_manifest(&p(), src).unwrap();
        assert!(m.wasm_pool_size.is_none());
        assert!(
            m.warnings
                .iter()
                .any(|w| w.message.contains("unknown key `[wasm].stack-size`")),
            "expected a soft warning for the unknown key, got: {:?}",
            m.warnings,
        );
    }

    #[test]
    fn merge_target_overlay_extends_dependencies() {
        let mut m = parse_manifest(
            &p(),
            r#"[package]
name = "hello"

[dependencies]
core = "1.0"

[target."x86_64-apple-darwin".dependencies]
mac-only = "0.1"
"#,
        )
        .unwrap();
        // Without overlay, dependencies has core but not mac-only.
        assert_eq!(m.dependencies.len(), 1);
        assert!(m.dependencies.contains_key("core"));

        // Active = Linux: no overlay activates.
        let linux = merge_target_overlay(&m, Some("x86_64-unknown-linux-gnu"));
        assert_eq!(linux.dependencies.len(), 1);
        assert!(!linux.dependencies.contains_key("mac-only"));

        // Active = mac: overlay merges.
        let mac = merge_target_overlay(&m, Some("x86_64-apple-darwin"));
        assert_eq!(mac.dependencies.len(), 2);
        assert!(mac.dependencies.contains_key("mac-only"));
        assert!(mac.dependencies.contains_key("core"));

        // None active triple is a no-op.
        let none = merge_target_overlay(&m, None);
        assert_eq!(none.dependencies.len(), 1);

        // Confirm the base manifest is untouched (merge returns a copy).
        m.dependencies.insert("touched".into(), registry_dep("0.1"));
        let after = merge_target_overlay(&m, Some("x86_64-apple-darwin"));
        assert!(after.dependencies.contains_key("touched"));
    }

    #[test]
    fn merge_target_overlay_overrides_profile() {
        let m = parse_manifest(
            &p(),
            r#"[package]
name = "hello"
profile = "default"

[target."thumbv7em-none-eabi"]
profile = "embedded"
"#,
        )
        .unwrap();
        assert_eq!(m.profile, CompileProfile::Default);
        let active = merge_target_overlay(&m, Some("thumbv7em-none-eabi"));
        assert_eq!(active.profile, CompileProfile::Embedded);
        // Non-matching triple leaves the default profile intact.
        let other = merge_target_overlay(&m, Some("x86_64-apple-darwin"));
        assert_eq!(other.profile, CompileProfile::Default);
    }

    #[test]
    fn merge_target_overlay_overlay_dep_replaces_base() {
        // Same name in base + overlay → overlay entry wins (most-specific).
        let m = parse_manifest(
            &p(),
            r#"[package]
name = "hello"

[dependencies]
http = "1.0"

[target."x86_64-apple-darwin".dependencies]
http = "2.0"
"#,
        )
        .unwrap();
        let mac = merge_target_overlay(&m, Some("x86_64-apple-darwin"));
        let DependencySpec::Registry { version: vr } = mac.dependencies.get("http").unwrap() else {
            panic!("expected Registry");
        };
        // ^2.0 matches 2.x but not 1.x — pins the overlay won.
        assert!(vr.matches(&semver::Version::parse("2.1.0").unwrap()));
        assert!(!vr.matches(&semver::Version::parse("1.9.0").unwrap()));
    }

    fn registry_dep(s: &str) -> DependencySpec {
        DependencySpec::Registry { version: req(s) }
    }
}
