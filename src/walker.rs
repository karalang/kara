//! File walker for multi-file compilation (CR-24 slice 3).
//!
//! Discovers every `.kara` file under a project's `src/` directory and maps
//! each one to a [`ModulePath`]. The rules are fixed by
//! `docs/design.md § Module System`:
//!
//! - **One file per module path.** No `mod.kara` — it's rejected with a
//!   diagnostic pointing at the directory-tree rule.
//! - **Entry files.** `src/main.kara` (bin) and `src/lib.kara` (lib) are
//!   mutually exclusive. Their items hoist to the crate root, so they're
//!   recorded with an empty `ModulePath`.
//! - **Nested modules.** `src/db/connection.kara` → module `db.connection`.
//!   The directory path becomes the prefix; the file stem is the last
//!   segment.
//! - **Platform suffixes.** Files ending in `_linux` / `_macos` / `_windows`
//!   / `_wasm` are platform-specific; the walker keeps them only when the
//!   target matches. The permissive-walker rule (v41 F1a): only these four
//!   exact tokens trigger filtering — `foo_linux_x86_64.kara` is an ordinary
//!   module named `foo_linux_x86_64`.
//! - **Colocated tests.** `_test.kara` files are companions to their
//!   same-directory siblings. `karac build` skips them (default); `karac
//!   test` includes them (future). The walker merely classifies; the caller
//!   decides whether to surface them via [`WalkerOpts::include_tests`].
//! - **Target-vs-shared collision.** When both `poller.kara` and
//!   `poller_linux.kara` exist, on Linux only the platform file compiles;
//!   on other targets only the shared file compiles. The walker resolves
//!   this per-module-path.
//!
//! The walker does **not** parse file contents. It also does not enforce
//! `E0226 ConflictingPlatformModule` — that diagnostic covers symbol-level
//! conflicts between a platform file and a shared file that both survive
//! target filtering, which is structurally impossible under the rules above
//! but stays reserved for post-parse symbol checks.

use crate::module::ModulePath;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// The directory under the project root that houses compilable source.
pub const SRC_DIRNAME: &str = "src";

/// The file extension for Kāra source files (without the leading dot).
pub const KARA_EXTENSION: &str = "kara";

/// Stem that marks a file as a colocated test companion. A file named
/// `foo_test.kara` is the test companion for module `foo` in the same
/// directory.
pub const TEST_SUFFIX: &str = "test";

/// Entry-file stems. `main.kara` is the binary entry, `lib.kara` the library
/// entry; a package contains at most one of them.
pub const MAIN_STEM: &str = "main";
pub const LIB_STEM: &str = "lib";

/// Rejected filename. `mod.kara` is not a recognized convention in Kāra
/// (unlike Rust); the walker rejects it with a structured diagnostic.
pub const MOD_STEM: &str = "mod";

// ── Target platforms ─────────────────────────────────────────────

/// Target platforms recognized by the platform-suffix mechanism. Keep in
/// sync with `docs/design.md § Module System — Conditional compilation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Platform {
    Linux,
    Macos,
    Windows,
    Wasm,
}

impl Platform {
    /// The suffix string (without the leading underscore) used in filenames.
    pub fn as_suffix(self) -> &'static str {
        match self {
            Platform::Linux => "linux",
            Platform::Macos => "macos",
            Platform::Windows => "windows",
            Platform::Wasm => "wasm",
        }
    }

    /// Parse a filename tail token into a [`Platform`]. Returns `None` for
    /// anything outside the v1 allow-list, preserving the permissive-walker
    /// rule (unknown tails are ordinary module-name suffixes, not errors).
    pub fn from_suffix(s: &str) -> Option<Platform> {
        match s {
            "linux" => Some(Platform::Linux),
            "macos" => Some(Platform::Macos),
            "windows" => Some(Platform::Windows),
            "wasm" => Some(Platform::Wasm),
            _ => None,
        }
    }

    /// The platform the compiler itself was built for. Used as the default
    /// target when neither `kara.toml [build].target` nor `--target` is set
    /// (those inputs land in a later slice). `wasm32` maps to `Wasm`;
    /// `linux` / `macos` / `windows` map to their namesakes. Any other host
    /// (e.g. FreeBSD) falls back to `Linux` as the closest v1 analogue.
    pub fn host() -> Platform {
        #[cfg(target_arch = "wasm32")]
        {
            Platform::Wasm
        }
        #[cfg(all(not(target_arch = "wasm32"), target_os = "macos"))]
        {
            Platform::Macos
        }
        #[cfg(all(not(target_arch = "wasm32"), target_os = "windows"))]
        {
            Platform::Windows
        }
        #[cfg(all(
            not(target_arch = "wasm32"),
            not(target_os = "macos"),
            not(target_os = "windows"),
        ))]
        {
            Platform::Linux
        }
    }
}

// ── Walked entries ───────────────────────────────────────────────

/// Which entry file (if any) the project exposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// No entry file. The package has only nested modules — buildable as a
    /// library that re-exports them, not as a standalone binary.
    None,
    /// `src/main.kara`. Executable entry.
    Bin,
    /// `src/lib.kara`. Library entry.
    Lib,
}

/// The role a file plays in the module tree. Test files are companions —
/// same module path as their sibling — and are excluded from `karac build`
/// unless [`WalkerOpts::include_tests`] is set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ModuleRole {
    /// Ordinary module file (e.g. `src/db/connection.kara`).
    Ordinary,
    /// Entry file (`src/main.kara` or `src/lib.kara`). Items hoist to the
    /// crate root; `path` is the empty vector.
    Entry,
    /// Colocated test companion (`src/foo_test.kara`, `src/db/connection_test.kara`).
    Test,
}

/// A single file the walker has classified and kept for this target.
#[derive(Debug, Clone)]
pub struct WalkedModule {
    /// Absolute path to the file.
    pub file: PathBuf,
    /// Dotted module path (empty for entry files).
    pub path: ModulePath,
    pub role: ModuleRole,
    /// `Some(p)` when the file's stem carried a v1 platform suffix; the
    /// target filter has already verified `p` matches the build target.
    /// `None` means a shared (non-platform) file.
    pub platform: Option<Platform>,
}

/// Complete walker output for a project. `modules` is sorted by file path
/// for deterministic downstream diagnostics.
#[derive(Debug, Clone)]
pub struct WalkResult {
    pub src_dir: PathBuf,
    pub modules: Vec<WalkedModule>,
    pub entry: EntryKind,
}

// ── Errors ───────────────────────────────────────────────────────

#[derive(Debug)]
pub enum WalkerError {
    /// `src/` does not exist or is not a directory.
    SrcDirMissing { expected: PathBuf },
    /// Filesystem read failed mid-walk (permissions, broken symlink, etc.).
    Io { path: PathBuf, error: String },
    /// Both `src/main.kara` and `src/lib.kara` exist.
    MixedEntryFiles {
        main_path: PathBuf,
        lib_path: PathBuf,
    },
    /// A `mod.kara` filename was found. Kāra does not recognize `mod.kara`
    /// (module structure is derived from the directory tree).
    ModFileRejected { path: PathBuf },
    /// Two shared files (or a shared and a same-platform file via different
    /// parent paths) claim the same module path. Only triggers when the
    /// walker can't unambiguously pick one for the current target.
    DuplicateModule {
        path: ModulePath,
        first: PathBuf,
        second: PathBuf,
    },
}

impl WalkerError {
    /// Diagnostic code, when one is assigned. Slice 3 introduces no new
    /// codes — `E0226 ConflictingPlatformModule` is a post-parse symbol
    /// check (see module doc-comment); walker-level errors share the
    /// generic bucket until the diagnostic registry grows a `walker` phase.
    pub fn code(&self) -> Option<&'static str> {
        None
    }
}

impl std::fmt::Display for WalkerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WalkerError::SrcDirMissing { expected } => write!(
                f,
                "project has no `{}/` directory (looked for `{}`). Add a `{SRC_DIRNAME}/main.kara` or `{SRC_DIRNAME}/lib.kara` to make it compilable.",
                SRC_DIRNAME,
                expected.display(),
            ),
            WalkerError::Io { path, error } => {
                write!(f, "cannot read `{}`: {}", path.display(), error)
            }
            WalkerError::MixedEntryFiles {
                main_path,
                lib_path,
            } => write!(
                f,
                "a Kāra package cannot contain both `{}` and `{}` — a package is either an executable or a library, never both.",
                main_path.display(),
                lib_path.display(),
            ),
            WalkerError::ModFileRejected { path } => write!(
                f,
                "`{}` is not a recognized filename — Kāra derives module structure from the directory tree. Rename to the parent module's name (e.g. `db/mod.kara` → delete it and keep `db.kara` at the sibling level).",
                path.display(),
            ),
            WalkerError::DuplicateModule {
                path,
                first,
                second,
            } => write!(
                f,
                "module `{}` is claimed by both `{}` and `{}`. Each module path must map to exactly one source file per target.",
                format_module_path(path),
                first.display(),
                second.display(),
            ),
        }
    }
}

fn format_module_path(path: &[String]) -> String {
    if path.is_empty() {
        "<crate root>".to_string()
    } else {
        path.join(".")
    }
}

// ── Options ──────────────────────────────────────────────────────

/// Walker configuration.
#[derive(Debug, Clone, Copy)]
pub struct WalkerOpts {
    /// Target platform for platform-suffix filtering.
    pub target: Platform,
    /// If `true`, test files are returned in the result. Default `false`
    /// (build mode) drops them.
    pub include_tests: bool,
}

impl Default for WalkerOpts {
    fn default() -> Self {
        WalkerOpts {
            target: Platform::host(),
            include_tests: false,
        }
    }
}

// ── Entry point ──────────────────────────────────────────────────

/// Walk `project_root/src/` and return every `.kara` file classified and
/// filtered per [`WalkerOpts`]. See the module doc-comment for the rules.
pub fn walk_project(project_root: &Path, opts: WalkerOpts) -> Result<WalkResult, WalkerError> {
    let src_dir = project_root.join(SRC_DIRNAME);
    if !src_dir.is_dir() {
        return Err(WalkerError::SrcDirMissing { expected: src_dir });
    }

    let mut files: Vec<PathBuf> = Vec::new();
    collect_kara_files(&src_dir, &mut files)?;
    files.sort();

    // Pass 1 — classify every file. Reject `mod.kara` and enforce
    // main/lib exclusivity as we see them, so the error points at the
    // first offending pair.
    let mut classified: Vec<Classified> = Vec::with_capacity(files.len());
    let mut main_path: Option<PathBuf> = None;
    let mut lib_path: Option<PathBuf> = None;

    for file in &files {
        let rel = match file.strip_prefix(&src_dir) {
            Ok(r) => r.to_path_buf(),
            Err(_) => continue,
        };
        let c = classify_file(&rel, file.clone())?;
        if let ClassifiedKind::EntryMain = c.kind {
            if main_path.is_none() {
                main_path = Some(file.clone());
            }
        }
        if let ClassifiedKind::EntryLib = c.kind {
            if lib_path.is_none() {
                lib_path = Some(file.clone());
            }
        }
        classified.push(c);
    }

    if let (Some(m), Some(l)) = (&main_path, &lib_path) {
        return Err(WalkerError::MixedEntryFiles {
            main_path: m.clone(),
            lib_path: l.clone(),
        });
    }

    // Pass 2 — bucket by module path (ignoring platform suffix) and
    // role, then collapse each bucket down to a single file per target.
    // Bucketing by role keeps tests from colliding with their sibling:
    // `foo.kara` and `foo_test.kara` share a module path but different
    // roles and must not dedup against each other.
    let mut bucket: HashMap<(ModulePath, ModuleRole), Vec<Classified>> = HashMap::new();
    for c in classified {
        bucket
            .entry((c.module_path.clone(), c.role()))
            .or_default()
            .push(c);
    }

    let mut modules: Vec<WalkedModule> = Vec::new();
    for ((path, role), candidates) in bucket {
        if role == ModuleRole::Test && !opts.include_tests {
            continue;
        }
        if let Some(picked) = pick_for_target(&path, &candidates, opts.target)? {
            modules.push(picked);
        }
    }

    modules.sort_by(|a, b| a.file.cmp(&b.file));

    let entry = match (&main_path, &lib_path) {
        (Some(_), None) => EntryKind::Bin,
        (None, Some(_)) => EntryKind::Lib,
        (None, None) => EntryKind::None,
        (Some(_), Some(_)) => unreachable!("exclusivity checked above"),
    };

    Ok(WalkResult {
        src_dir,
        modules,
        entry,
    })
}

/// Discover example names under `<project_root>/examples/`. Returns a sorted
/// list of names, one per discovered example:
/// - `examples/<name>.kara` → `name`
/// - `examples/<name>/src/main.kara` → `name`
///
/// Returns an empty vec when the `examples/` directory does not exist.
pub fn walk_examples(project_root: &Path) -> Vec<String> {
    let examples_dir = project_root.join("examples");
    if !examples_dir.is_dir() {
        return Vec::new();
    }
    let mut names: Vec<String> = Vec::new();
    if let Ok(entries) = fs::read_dir(&examples_dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()) == Some(KARA_EXTENSION) {
                if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                    names.push(stem.to_string());
                }
            } else if p.is_dir() && p.join(SRC_DIRNAME).join("main.kara").exists() {
                if let Some(stem) = p.file_name().and_then(|s| s.to_str()) {
                    names.push(stem.to_string());
                }
            }
        }
    }
    names.sort_unstable();
    names
}

// ── Internal: classification ─────────────────────────────────────

/// Intermediate classification for one discovered file.
#[derive(Debug, Clone)]
struct Classified {
    file: PathBuf,
    /// Module path *without* the platform suffix stripped and without the
    /// `_test` suffix — i.e. the path the item ends up mounted at.
    module_path: ModulePath,
    platform: Option<Platform>,
    kind: ClassifiedKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClassifiedKind {
    /// Plain non-entry module file.
    Ordinary,
    /// `src/main.kara` at exactly the top level.
    EntryMain,
    /// `src/lib.kara` at exactly the top level.
    EntryLib,
    /// Any file whose stem ends in `_test` (after optional platform-suffix
    /// stripping). Always companioned to its same-directory sibling.
    Test,
}

impl Classified {
    fn role(&self) -> ModuleRole {
        match self.kind {
            ClassifiedKind::Ordinary => ModuleRole::Ordinary,
            ClassifiedKind::EntryMain | ClassifiedKind::EntryLib => ModuleRole::Entry,
            ClassifiedKind::Test => ModuleRole::Test,
        }
    }
}

/// Classify one `.kara` file given its path relative to `src/`. The file's
/// stem drives platform / test detection; the parent directory drives the
/// module-path prefix.
fn classify_file(rel: &Path, file: PathBuf) -> Result<Classified, WalkerError> {
    let stem = match rel.file_stem().and_then(|s| s.to_str()) {
        Some(s) => s.to_string(),
        None => {
            return Err(WalkerError::Io {
                path: file,
                error: "filename is not valid UTF-8".to_string(),
            });
        }
    };

    // `mod.kara` is rejected at any depth (`src/mod.kara`, `src/db/mod.kara`, ...).
    if stem == MOD_STEM {
        return Err(WalkerError::ModFileRejected { path: file });
    }

    // Strip platform suffix first, then test suffix. This ordering matches
    // the brainstorming-v41 F1a algorithm ("split stem at the last
    // underscore") and makes `greet_test_linux.kara` a Linux-specific test
    // for `greet` rather than a non-test module named `greet_test`.
    let (post_platform, platform) = split_platform(&stem);
    let (base_stem, is_test) = split_test(post_platform);

    let parent_segments: Vec<String> = rel
        .parent()
        .map(|p| {
            p.components()
                .filter_map(|c| c.as_os_str().to_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    // Entry-file detection: `src/main.kara` or `src/lib.kara` at the top
    // level only. `src/foo/main.kara` is an ordinary module named
    // `foo.main`, not an entry file — entry semantics require the file to
    // sit directly in `src/`.
    let at_top_level = parent_segments.is_empty();
    let is_platform = platform.is_some();

    if at_top_level && !is_test && !is_platform {
        if base_stem == MAIN_STEM {
            return Ok(Classified {
                file,
                module_path: Vec::new(),
                platform: None,
                kind: ClassifiedKind::EntryMain,
            });
        }
        if base_stem == LIB_STEM {
            return Ok(Classified {
                file,
                module_path: Vec::new(),
                platform: None,
                kind: ClassifiedKind::EntryLib,
            });
        }
    }

    // For test files, the module path matches the sibling they test.
    // For non-tests, the module path is parent_segments + base_stem.
    // Entry-file test companions (`main_test.kara` / `lib_test.kara` at
    // top level) share the crate root — their `module_path` is empty, the
    // same empty vector used for `main.kara` / `lib.kara`.
    let module_path =
        if is_test && at_top_level && (base_stem == MAIN_STEM || base_stem == LIB_STEM) {
            Vec::new()
        } else {
            let mut segs = parent_segments;
            segs.push(base_stem.to_string());
            segs
        };

    let kind = if is_test {
        ClassifiedKind::Test
    } else {
        ClassifiedKind::Ordinary
    };

    Ok(Classified {
        file,
        module_path,
        platform,
        kind,
    })
}

/// Split a filename stem into (remaining_stem, Some(platform)) if the tail
/// after the last underscore is one of the v1 platform tokens. Otherwise
/// returns (stem, None) unchanged (the permissive-walker rule).
fn split_platform(stem: &str) -> (&str, Option<Platform>) {
    if let Some((base, tail)) = stem.rsplit_once('_') {
        if let Some(p) = Platform::from_suffix(tail) {
            return (base, Some(p));
        }
    }
    (stem, None)
}

/// Split a (post-platform) stem into (module_base, is_test). A stem is a
/// test companion iff its trailing segment (split by `_`) is exactly `test`.
fn split_test(stem: &str) -> (&str, bool) {
    if let Some((base, tail)) = stem.rsplit_once('_') {
        if tail == TEST_SUFFIX {
            return (base, true);
        }
    }
    (stem, false)
}

// ── Internal: target filtering ──────────────────────────────────

/// Given every file that claims `(module_path, role)`, pick the one that
/// belongs on the current target. Returns `Ok(None)` when every candidate
/// is filtered out (e.g. a module defined only via `_linux.kara` when the
/// target is macOS — no fallback exists, so the module simply doesn't
/// compile).
fn pick_for_target(
    path: &ModulePath,
    candidates: &[Classified],
    target: Platform,
) -> Result<Option<WalkedModule>, WalkerError> {
    // Partition into target-match and shared (no platform). Any non-matching
    // platform files are dropped silently — they're intended for a different
    // target.
    let mut matching_platform: Vec<&Classified> = Vec::new();
    let mut shared: Vec<&Classified> = Vec::new();
    for c in candidates {
        match c.platform {
            Some(p) if p == target => matching_platform.push(c),
            Some(_) => continue,
            None => shared.push(c),
        }
    }

    // On the matching target, the platform file wins and the shared file
    // (if any) is suppressed — matches the design.md collision rule.
    if !matching_platform.is_empty() {
        if matching_platform.len() > 1 {
            return Err(WalkerError::DuplicateModule {
                path: path.clone(),
                first: matching_platform[0].file.clone(),
                second: matching_platform[1].file.clone(),
            });
        }
        let c = matching_platform[0];
        return Ok(Some(WalkedModule {
            file: c.file.clone(),
            path: c.module_path.clone(),
            role: c.role(),
            platform: c.platform,
        }));
    }

    // Off-target: only the shared file compiles. Two shared files claiming
    // the same path is a real duplicate and fails here.
    match shared.len() {
        0 => Ok(None),
        1 => {
            let c = shared[0];
            Ok(Some(WalkedModule {
                file: c.file.clone(),
                path: c.module_path.clone(),
                role: c.role(),
                platform: None,
            }))
        }
        _ => Err(WalkerError::DuplicateModule {
            path: path.clone(),
            first: shared[0].file.clone(),
            second: shared[1].file.clone(),
        }),
    }
}

// ── Internal: filesystem walk ───────────────────────────────────

fn collect_kara_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), WalkerError> {
    let entries = fs::read_dir(dir).map_err(|e| WalkerError::Io {
        path: dir.to_path_buf(),
        error: e.to_string(),
    })?;
    for entry in entries {
        let entry = entry.map_err(|e| WalkerError::Io {
            path: dir.to_path_buf(),
            error: e.to_string(),
        })?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|e| WalkerError::Io {
            path: path.clone(),
            error: e.to_string(),
        })?;
        if file_type.is_dir() {
            collect_kara_files(&path, out)?;
        } else if file_type.is_file()
            && path
                .extension()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s == KARA_EXTENSION)
        {
            out.push(path);
        }
    }
    Ok(())
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_platform_recognizes_four_suffixes() {
        assert_eq!(
            split_platform("poller_linux"),
            ("poller", Some(Platform::Linux))
        );
        assert_eq!(
            split_platform("poller_macos"),
            ("poller", Some(Platform::Macos))
        );
        assert_eq!(
            split_platform("poller_windows"),
            ("poller", Some(Platform::Windows)),
        );
        assert_eq!(
            split_platform("poller_wasm"),
            ("poller", Some(Platform::Wasm))
        );
    }

    #[test]
    fn split_platform_permissive_rule() {
        // Unknown tails: not treated as platform.
        assert_eq!(split_platform("foo_bar"), ("foo_bar", None));
        // Compound suffix (post-v1 extension): stays intact in v1.
        assert_eq!(
            split_platform("foo_linux_x86_64"),
            ("foo_linux_x86_64", None),
        );
        // Last underscore only — `foo_macos_helper` → tail `helper`, not platform.
        assert_eq!(
            split_platform("foo_macos_helper"),
            ("foo_macos_helper", None),
        );
        // Stem with no underscore.
        assert_eq!(split_platform("lonely"), ("lonely", None));
    }

    #[test]
    fn split_test_recognizes_suffix() {
        assert_eq!(split_test("foo_test"), ("foo", true));
        assert_eq!(split_test("connection_test"), ("connection", true));
        assert_eq!(split_test("foo_bar"), ("foo_bar", false));
        assert_eq!(split_test("foo"), ("foo", false));
    }

    #[test]
    fn host_is_one_of_four() {
        // Just prove the fn compiles on this host and returns a valid variant.
        let h = Platform::host();
        assert!(matches!(
            h,
            Platform::Linux | Platform::Macos | Platform::Windows | Platform::Wasm,
        ));
    }

    #[test]
    fn platform_round_trip() {
        for p in [
            Platform::Linux,
            Platform::Macos,
            Platform::Windows,
            Platform::Wasm,
        ] {
            assert_eq!(Platform::from_suffix(p.as_suffix()), Some(p));
        }
        assert_eq!(Platform::from_suffix("bsd"), None);
    }
}
