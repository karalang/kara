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
    pub warnings: Vec<ManifestWarning>,
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
    let dependencies = parse_dependencies_table(path, &table, "dependencies", &mut warnings)?;
    let dev_dependencies =
        parse_dependencies_table(path, &table, "dev-dependencies", &mut warnings)?;
    let workspace_dependencies = parse_workspace_dependencies(path, &table, &mut warnings)?;

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
        kara_version,
        dependencies,
        dev_dependencies,
        workspace_dependencies,
        warnings,
    })
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
        // `[dependencies]` and `[dev-dependencies]` are now structurally
        // captured (slice 1 of the PubGrub resolver) — well-formed entries
        // emit no warnings. Unknown sections like `[build]` remain silent.
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
}
