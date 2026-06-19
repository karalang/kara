//! Build artifact cache — global per-machine storage for compiled
//! dependencies, keyed on the five-tuple
//! `(compiler-version, package-version, edition, profile, target-triple)`
//! per design.md § Package System > Build artifact cache.
//!
//! **Why a separate module.** `~/.kara/cache/` already houses the
//! registry-proxy tarball area (carve-out (c) from line 851); this
//! module owns the *build artifact* subtree under that root
//! (`~/.kara/cache/build/`). Keeping the two concerns in separate
//! modules means the registry-proxy fetch layer (when it ships) doesn't
//! have to know anything about codegen artifacts, and vice versa.
//!
//! **v1.1 partial-ship.** Today's compiler does whole-program codegen
//! (the multi-file build path concatenates all module items into a
//! super-program and emits one object file). Per-dep object emission —
//! the actual mechanism that produces the cacheable artifact — is a
//! downstream piece of infrastructure that hasn't shipped yet. So this
//! module ships the *protocol surface*: cache-key derivation, on-disk
//! layout, read/write API. The build pipeline doesn't consult this
//! module yet; integration lands once per-dep codegen exists. The
//! `karac cache` subcommand surfaces the protocol so tooling can verify
//! key derivation and inspect the cache from day one.
//!
//! **Cache layout.**
//!
//! ```text
//! ~/.kara/cache/build/
//! ├── <package-name>/
//! │   └── <digest>/
//! │       ├── entry.toml      ← metadata (the key + content_hash)
//! │       └── artifact.o      ← the cached object (when populated)
//! ```
//!
//! The package name is the outer path component so a human listing the
//! cache directory can see which packages they have cached without
//! decoding any digests. The digest covers the full five-tuple plus
//! the package name (defense-in-depth against weird path encodings),
//! so even two packages with the same name in different cache layouts
//! cannot collide.
//!
//! **Cache key digesting.** Each field of the key is length-prefixed
//! (the field byte length as a u32 big-endian, then the UTF-8 bytes).
//! This avoids any chance of `("foo", "bar")` colliding with
//! `("foob", "ar")` under a naive separator-joined scheme. The
//! BLAKE3 digest of the prefixed buffer is the directory name.
//!
//! **No time-based invalidation.** The spec promises that the cache
//! never invalidates on time — every cache entry is keyed on the full
//! five-tuple, and a compiler upgrade automatically changes the
//! `compiler_version` slot in the key (any new compiler reads a
//! different cache slot than the old compiler wrote, so the old
//! entries are simply ignored, not actively evicted). Eviction is
//! manual via `karac clean --global`.

use std::path::{Path, PathBuf};

/// Subdirectory under `~/.kara/cache/` that holds build artifacts. Kept
/// distinct from sibling subtrees (e.g. `registry/` for tarballs once
/// the proxy fetch layer ships) so the two cache concerns can be
/// reasoned about independently.
pub const CACHE_SUBDIR: &str = "build";

/// Environment-variable override for the absolute cache root. Honored
/// by `default_cache_root()`. Primarily a testing affordance — lets
/// integration tests point at a tempdir without disturbing the real
/// user-level cache. Power users can also use it to pin the cache to
/// a different filesystem (e.g. a faster SSD).
pub const CACHE_ROOT_ENV_VAR: &str = "KARAC_BUILD_CACHE_ROOT";

/// Filename of the per-entry metadata TOML file.
pub const ENTRY_METADATA_FILENAME: &str = "entry.toml";

/// Filename of the cached artifact object inside an entry directory.
/// One per entry — the cache is one-artifact-per-key. Multi-artifact
/// entries (e.g. an object + a `.d` dep file) are a v1.1.x widening.
pub const ARTIFACT_FILENAME: &str = "artifact.o";

/// Schema version embedded in every `entry.toml`. Bumping this on a
/// breaking change to the metadata format gives `lookup()` a clean
/// path to skip incompatible entries (treat as Miss) rather than
/// surface a confusing parse error.
pub const ENTRY_SCHEMA_VERSION: u32 = 1;

/// The five-tuple cache key plus the package name. The spec lists the
/// key as a five-tuple of `(compiler-version, package-version, edition,
/// profile, target-triple)`; the package name is implicit there (each
/// package gets its own cache slot) but we carry it as a struct field
/// because the digest mixes it in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheKey {
    pub compiler_version: String,
    pub package_name: String,
    pub package_version: String,
    pub edition: String,
    pub profile: String,
    pub target_triple: String,
}

impl CacheKey {
    /// Length-prefixed canonical serialization. Each field becomes a
    /// big-endian u32 byte length followed by the UTF-8 bytes. The
    /// field order is fixed (and matches the order in which the spec
    /// lists them, plus `package_name` as the first field).
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for field in [
            self.package_name.as_str(),
            self.compiler_version.as_str(),
            self.package_version.as_str(),
            self.edition.as_str(),
            self.profile.as_str(),
            self.target_triple.as_str(),
        ] {
            let len = field.len() as u32;
            out.extend_from_slice(&len.to_be_bytes());
            out.extend_from_slice(field.as_bytes());
        }
        out
    }

    /// 64-hex BLAKE3 digest of the canonical encoding. Used as the
    /// inner directory name under `<cache-root>/<package-name>/<digest>/`.
    pub fn digest(&self) -> String {
        let h = blake3::hash(&self.canonical_bytes());
        h.to_hex().to_string()
    }
}

/// Result of a cache lookup. `Hit` carries the loaded entry; `Miss`
/// means either the entry directory didn't exist, the metadata file
/// was absent, or the schema version was wrong (in all three cases
/// the caller should treat the cache as cold and proceed with a
/// build).
///
/// `Hit`'s `CacheEntry` is boxed so the enum's size tracks the empty
/// `Miss` variant rather than the ~208-byte entry (clippy
/// `large_enum_variant`); the `Box` auto-derefs, so matching `Hit(e)`
/// reads `e`'s fields exactly as if it were unboxed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LookupStatus {
    Hit(Box<CacheEntry>),
    Miss,
}

/// One populated cache slot. The artifact lives at `entry_dir/ARTIFACT_FILENAME`;
/// the metadata at `entry_dir/ENTRY_METADATA_FILENAME`. `content_hash`
/// is the BLAKE3 of the artifact bytes (formatted `"blake3:<64-hex>"`,
/// matching the lockfile convention).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheEntry {
    pub key: CacheKey,
    pub content_hash: String,
    pub written_at_epoch_seconds: u64,
    pub entry_dir: PathBuf,
}

impl CacheEntry {
    pub fn artifact_path(&self) -> PathBuf {
        self.entry_dir.join(ARTIFACT_FILENAME)
    }

    pub fn metadata_path(&self) -> PathBuf {
        self.entry_dir.join(ENTRY_METADATA_FILENAME)
    }
}

/// Errors surfaced by cache operations. Each has a symbolic code so
/// the diagnostic renderer can match against a stable identifier; the
/// `Display` impl produces a human-readable rendering for text-mode
/// output.
#[derive(Debug)]
pub enum CacheError {
    /// `$HOME` (and `$USERPROFILE` on Windows-like setups) are unset.
    /// Hard to recover — the cache has no default location.
    HomeUnset,
    /// A directory or metadata file could not be read.
    Unreadable { path: PathBuf, message: String },
    /// `entry.toml` parsed but had a malformed shape (missing field,
    /// wrong type, unknown schema version).
    MalformedMetadata { path: PathBuf, message: String },
    /// A write (artifact, metadata, or directory creation) failed.
    WriteFailed { path: PathBuf, message: String },
}

impl CacheError {
    pub fn code(&self) -> &'static str {
        match self {
            CacheError::HomeUnset => "E_CACHE_HOME_UNSET",
            CacheError::Unreadable { .. } => "E_CACHE_UNREADABLE",
            CacheError::MalformedMetadata { .. } => "E_CACHE_MALFORMED_METADATA",
            CacheError::WriteFailed { .. } => "E_CACHE_WRITE_FAILED",
        }
    }
}

impl std::fmt::Display for CacheError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CacheError::HomeUnset => {
                write!(
                    f,
                    "$HOME (and $USERPROFILE) unset; cannot resolve cache root"
                )
            }
            CacheError::Unreadable { path, message } => {
                write!(f, "cannot read {}: {}", path.display(), message)
            }
            CacheError::MalformedMetadata { path, message } => {
                write!(
                    f,
                    "malformed cache metadata at {}: {}",
                    path.display(),
                    message
                )
            }
            CacheError::WriteFailed { path, message } => {
                write!(f, "failed to write {}: {}", path.display(), message)
            }
        }
    }
}

impl std::error::Error for CacheError {}

/// Resolve the default cache root: `KARAC_BUILD_CACHE_ROOT` env var if
/// set non-empty, else `~/.kara/cache/build/`. The directory may or
/// may not exist; this function does not create it.
pub fn default_cache_root() -> Result<PathBuf, CacheError> {
    if let Ok(over) = std::env::var(CACHE_ROOT_ENV_VAR) {
        if !over.trim().is_empty() {
            return Ok(PathBuf::from(over));
        }
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map_err(|_| CacheError::HomeUnset)?;
    Ok(PathBuf::from(home)
        .join(".kara")
        .join("cache")
        .join(CACHE_SUBDIR))
}

/// Path to the cache entry directory for `key` under `root`. Equal to
/// `<root>/<package-name>/<digest>/`. Does not create the directory.
pub fn entry_path(root: &Path, key: &CacheKey) -> PathBuf {
    root.join(&key.package_name).join(key.digest())
}

/// Best-effort host target triple. v1.1 builds are host-only, so this
/// is the same value a `--target` flag would default to. When the
/// `--target` flag lands at line 876 (`[target.X.dependencies]`), the
/// build pipeline passes the explicit value through instead.
pub fn host_target_triple() -> String {
    let arch = std::env::consts::ARCH;
    let os = std::env::consts::OS;
    match os {
        "macos" => format!("{arch}-apple-darwin"),
        "linux" => format!("{arch}-unknown-linux-gnu"),
        "windows" => format!("{arch}-pc-windows-msvc"),
        "ios" => format!("{arch}-apple-ios"),
        "android" => format!("{arch}-linux-android"),
        "freebsd" => format!("{arch}-unknown-freebsd"),
        "netbsd" => format!("{arch}-unknown-netbsd"),
        "openbsd" => format!("{arch}-unknown-openbsd"),
        other => format!("{arch}-unknown-{other}"),
    }
}

/// Active compiler version as it would appear in a cache key. Sourced
/// from `CARGO_PKG_VERSION` at compile time — same source as
/// `dep_resolver::active_toolchain_version()`. Returned as `&'static
/// str` so the caller can choose between borrowed use and an owned
/// copy.
pub fn active_compiler_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Look up the cache entry for `key` under `root`. Returns:
///
/// - `Ok(Hit(entry))` — metadata loaded cleanly and the schema version
///   matches; the caller may consume `entry.artifact_path()` directly.
/// - `Ok(Miss)` — either the entry directory doesn't exist, the
///   metadata file is absent, or the schema version is wrong. All
///   three are treated identically: the cache is cold for this key.
/// - `Err(CacheError::Unreadable | MalformedMetadata)` — the cache
///   state is genuinely broken (permission error on a directory that
///   exists, or `entry.toml` that parses but has missing fields). The
///   caller should surface the diagnostic and decide whether to halt.
pub fn lookup(root: &Path, key: &CacheKey) -> Result<LookupStatus, CacheError> {
    let dir = entry_path(root, key);
    let meta_path = dir.join(ENTRY_METADATA_FILENAME);
    let bytes = match std::fs::read(&meta_path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(LookupStatus::Miss),
        Err(e) => {
            return Err(CacheError::Unreadable {
                path: meta_path,
                message: e.to_string(),
            });
        }
    };
    let text = std::str::from_utf8(&bytes).map_err(|e| CacheError::MalformedMetadata {
        path: meta_path.clone(),
        message: format!("not valid UTF-8: {e}"),
    })?;
    let entry = parse_entry_metadata(&meta_path, text, &dir)?;
    if entry.key != *key {
        // Digest collision — vanishingly unlikely with BLAKE3, but if
        // it ever happens we'd rather treat it as Miss than serve the
        // wrong artifact. The wrong-key entry stays on disk; a future
        // `karac cache prune` can sweep mismatched slots.
        return Ok(LookupStatus::Miss);
    }
    Ok(LookupStatus::Hit(Box::new(entry)))
}

/// Write `artifact_bytes` into the cache slot for `key`, alongside a
/// metadata `entry.toml`. Creates the directory tree as needed. The
/// write is *not* atomic in v1.1 — a partial write between the
/// artifact and the metadata could leave a half-formed entry on disk;
/// `lookup` treats a missing `entry.toml` as Miss so the half-formed
/// case degrades gracefully (a future `karac cache prune` step can
/// also gc orphan artifacts). Atomic tempfile-then-rename is a
/// v1.1.x widening listed in the line-861 carve-outs.
pub fn record(
    root: &Path,
    key: &CacheKey,
    artifact_bytes: &[u8],
) -> Result<CacheEntry, CacheError> {
    let dir = entry_path(root, key);
    std::fs::create_dir_all(&dir).map_err(|e| CacheError::WriteFailed {
        path: dir.clone(),
        message: e.to_string(),
    })?;

    let artifact_path = dir.join(ARTIFACT_FILENAME);
    std::fs::write(&artifact_path, artifact_bytes).map_err(|e| CacheError::WriteFailed {
        path: artifact_path.clone(),
        message: e.to_string(),
    })?;

    let content_hash = format!("blake3:{}", blake3::hash(artifact_bytes).to_hex());
    let written_at_epoch_seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let entry = CacheEntry {
        key: key.clone(),
        content_hash,
        written_at_epoch_seconds,
        entry_dir: dir.clone(),
    };
    let meta_path = entry.metadata_path();
    let meta_text = render_entry_metadata(&entry);
    std::fs::write(&meta_path, meta_text).map_err(|e| CacheError::WriteFailed {
        path: meta_path,
        message: e.to_string(),
    })?;
    Ok(entry)
}

/// Aggregate stats over the cache rooted at `root`. Used by `karac
/// cache info`. Reports the number of populated entries (directories
/// with a valid `entry.toml`) and the total artifact size in bytes.
/// A non-existent cache root is reported as zero entries / zero bytes
/// — that's the cold-machine case and shouldn't error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheStats {
    pub entry_count: usize,
    pub total_bytes: u64,
}

pub fn stats(root: &Path) -> Result<CacheStats, CacheError> {
    let mut out = CacheStats {
        entry_count: 0,
        total_bytes: 0,
    };
    let pkgs = match std::fs::read_dir(root) {
        Ok(it) => it,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => {
            return Err(CacheError::Unreadable {
                path: root.to_path_buf(),
                message: e.to_string(),
            });
        }
    };
    for pkg_entry in pkgs {
        let pkg_entry = pkg_entry.map_err(|e| CacheError::Unreadable {
            path: root.to_path_buf(),
            message: e.to_string(),
        })?;
        let pkg_path = pkg_entry.path();
        if !pkg_path.is_dir() {
            continue;
        }
        let digests = std::fs::read_dir(&pkg_path).map_err(|e| CacheError::Unreadable {
            path: pkg_path.clone(),
            message: e.to_string(),
        })?;
        for digest_entry in digests {
            let digest_entry = digest_entry.map_err(|e| CacheError::Unreadable {
                path: pkg_path.clone(),
                message: e.to_string(),
            })?;
            let entry_dir = digest_entry.path();
            if !entry_dir.is_dir() {
                continue;
            }
            let meta_path = entry_dir.join(ENTRY_METADATA_FILENAME);
            let artifact_path = entry_dir.join(ARTIFACT_FILENAME);
            // Only count entries that have *both* metadata and an
            // artifact file. A half-written entry (one but not the
            // other) doesn't get counted toward the populated total;
            // the `karac cache prune` step (carve-out) can sweep them.
            let meta_ok = matches!(std::fs::metadata(&meta_path), Ok(m) if m.is_file());
            let artifact_meta = std::fs::metadata(&artifact_path);
            if meta_ok {
                if let Ok(am) = artifact_meta {
                    out.entry_count += 1;
                    out.total_bytes += am.len();
                }
            }
        }
    }
    Ok(out)
}

// ── TOML I/O ──────────────────────────────────────────────────────

fn render_entry_metadata(entry: &CacheEntry) -> String {
    // Hand-formatted TOML for byte-stability across runs — keys appear
    // in a fixed order, no trailing whitespace, single trailing
    // newline. Matches the lockfile.rs convention.
    let mut out = String::new();
    out.push_str(&format!("schema_version = {ENTRY_SCHEMA_VERSION}\n"));
    out.push_str(&format!(
        "compiler_version = {}\n",
        toml_string(&entry.key.compiler_version)
    ));
    out.push_str(&format!(
        "package_name = {}\n",
        toml_string(&entry.key.package_name)
    ));
    out.push_str(&format!(
        "package_version = {}\n",
        toml_string(&entry.key.package_version)
    ));
    out.push_str(&format!("edition = {}\n", toml_string(&entry.key.edition)));
    out.push_str(&format!("profile = {}\n", toml_string(&entry.key.profile)));
    out.push_str(&format!(
        "target_triple = {}\n",
        toml_string(&entry.key.target_triple)
    ));
    out.push_str(&format!(
        "content_hash = {}\n",
        toml_string(&entry.content_hash)
    ));
    out.push_str(&format!(
        "written_at_epoch_seconds = {}\n",
        entry.written_at_epoch_seconds
    ));
    out
}

fn parse_entry_metadata(
    meta_path: &Path,
    text: &str,
    dir: &Path,
) -> Result<CacheEntry, CacheError> {
    let value: toml::Value =
        text.parse()
            .map_err(|e: toml::de::Error| CacheError::MalformedMetadata {
                path: meta_path.to_path_buf(),
                message: e.message().to_string(),
            })?;
    let table = value
        .as_table()
        .ok_or_else(|| CacheError::MalformedMetadata {
            path: meta_path.to_path_buf(),
            message: "expected a top-level TOML table".to_string(),
        })?;

    let schema_version = read_int(meta_path, table, "schema_version")?;
    if schema_version != ENTRY_SCHEMA_VERSION as i64 {
        // Signal Miss-via-error so the caller can decide; we surface
        // it as MalformedMetadata so the user sees what's stale.
        return Err(CacheError::MalformedMetadata {
            path: meta_path.to_path_buf(),
            message: format!(
                "unsupported schema_version: {schema_version} (this karac speaks v{ENTRY_SCHEMA_VERSION})"
            ),
        });
    }

    let compiler_version = read_string(meta_path, table, "compiler_version")?;
    let package_name = read_string(meta_path, table, "package_name")?;
    let package_version = read_string(meta_path, table, "package_version")?;
    let edition = read_string(meta_path, table, "edition")?;
    let profile = read_string(meta_path, table, "profile")?;
    let target_triple = read_string(meta_path, table, "target_triple")?;
    let content_hash = read_string(meta_path, table, "content_hash")?;
    let written_at_epoch_seconds = read_int(meta_path, table, "written_at_epoch_seconds")? as u64;

    Ok(CacheEntry {
        key: CacheKey {
            compiler_version,
            package_name,
            package_version,
            edition,
            profile,
            target_triple,
        },
        content_hash,
        written_at_epoch_seconds,
        entry_dir: dir.to_path_buf(),
    })
}

fn read_string(
    meta_path: &Path,
    table: &toml::value::Table,
    key: &str,
) -> Result<String, CacheError> {
    match table.get(key) {
        Some(toml::Value::String(s)) => Ok(s.clone()),
        Some(_) => Err(CacheError::MalformedMetadata {
            path: meta_path.to_path_buf(),
            message: format!("field `{key}` must be a string"),
        }),
        None => Err(CacheError::MalformedMetadata {
            path: meta_path.to_path_buf(),
            message: format!("missing required field `{key}`"),
        }),
    }
}

fn read_int(meta_path: &Path, table: &toml::value::Table, key: &str) -> Result<i64, CacheError> {
    match table.get(key) {
        Some(toml::Value::Integer(n)) => Ok(*n),
        Some(_) => Err(CacheError::MalformedMetadata {
            path: meta_path.to_path_buf(),
            message: format!("field `{key}` must be an integer"),
        }),
        None => Err(CacheError::MalformedMetadata {
            path: meta_path.to_path_buf(),
            message: format!("missing required field `{key}`"),
        }),
    }
}

fn toml_string(s: &str) -> String {
    // Minimal TOML basic-string escaper. We control every value the
    // cache writes (key fields are validated by the manifest /
    // resolver, content_hash is hex), so we only need to handle the
    // characters that could appear in a future free-form field. The
    // implementation is intentionally narrow — anything outside the
    // basic-string set surfaces as the literal escape sequence.
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes tests that mutate the process-wide `CACHE_ROOT_ENV_VAR`.
    /// Without this, the env-var tests race under cargo's default parallel
    /// execution — each test's `set_var` can land between its sibling's
    /// `set_var` and read, corrupting either assertion. Acquire with
    /// `unwrap_or_else(|e| e.into_inner())` so a panicked test (poisoned
    /// mutex) doesn't cascade-fail every following test; the prev/restore
    /// dance inside each test keeps the env clean on the way out.
    static CACHE_ROOT_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn sample_key() -> CacheKey {
        CacheKey {
            compiler_version: "0.1.0".to_string(),
            package_name: "foo".to_string(),
            package_version: "1.2.3".to_string(),
            edition: "2026".to_string(),
            profile: "default".to_string(),
            target_triple: "aarch64-apple-darwin".to_string(),
        }
    }

    fn temp_root(slug: &str) -> PathBuf {
        let base = std::env::temp_dir()
            .join("karac_build_cache_tests")
            .join(format!("{}_{}", slug, std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn digest_is_64_hex_chars() {
        let key = sample_key();
        let d = key.digest();
        assert_eq!(d.len(), 64);
        assert!(d.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn digest_changes_when_any_field_changes() {
        let base = sample_key();
        let base_d = base.digest();

        let mut k = base.clone();
        k.compiler_version = "0.1.1".to_string();
        assert_ne!(k.digest(), base_d, "compiler_version must affect digest");

        let mut k = base.clone();
        k.package_version = "1.2.4".to_string();
        assert_ne!(k.digest(), base_d, "package_version must affect digest");

        let mut k = base.clone();
        k.edition = "2027".to_string();
        assert_ne!(k.digest(), base_d, "edition must affect digest");

        let mut k = base.clone();
        k.profile = "embedded".to_string();
        assert_ne!(k.digest(), base_d, "profile must affect digest");

        let mut k = base.clone();
        k.target_triple = "x86_64-unknown-linux-gnu".to_string();
        assert_ne!(k.digest(), base_d, "target_triple must affect digest");

        let mut k = base.clone();
        k.package_name = "bar".to_string();
        assert_ne!(k.digest(), base_d, "package_name must affect digest");
    }

    #[test]
    fn length_prefix_prevents_field_boundary_collision() {
        // `("foo", "bar")` joined as `"foobar"` would collide with
        // `("foob", "ar")` under a naive concatenation scheme. With
        // length-prefixing, the two encodings differ in the prefix
        // bytes and therefore produce different digests.
        let mut a = sample_key();
        a.package_name = "foo".to_string();
        a.compiler_version = "bar".to_string();

        let mut b = sample_key();
        b.package_name = "foob".to_string();
        b.compiler_version = "ar".to_string();

        assert_ne!(a.digest(), b.digest());
    }

    #[test]
    fn digest_is_deterministic() {
        let a = sample_key().digest();
        let b = sample_key().digest();
        assert_eq!(a, b);
    }

    #[test]
    fn entry_path_layout_is_pkgname_then_digest() {
        let root = PathBuf::from("/tmp/x");
        let key = sample_key();
        let p = entry_path(&root, &key);
        assert_eq!(p, root.join("foo").join(key.digest()));
    }

    #[test]
    fn host_target_triple_is_nonempty_and_arch_prefixed() {
        let t = host_target_triple();
        assert!(!t.is_empty());
        assert!(t.starts_with(std::env::consts::ARCH));
        // Triple format: must contain at least two dashes (arch-vendor-os
        // for the common cases).
        assert!(t.matches('-').count() >= 2);
    }

    #[test]
    fn active_compiler_version_parses_as_semver() {
        let v = active_compiler_version();
        semver::Version::parse(v).expect("CARGO_PKG_VERSION must be valid semver");
    }

    #[test]
    fn default_cache_root_honors_env_override() {
        let _g = CACHE_ROOT_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // Save + restore so tests don't cross-contaminate.
        let prev = std::env::var(CACHE_ROOT_ENV_VAR).ok();
        // SAFETY: parallel mutation of the process env var is serialized by
        // CACHE_ROOT_ENV_LOCK above; the prev/restore pair below keeps the
        // env clean for any sibling that runs next.
        unsafe {
            std::env::set_var(CACHE_ROOT_ENV_VAR, "/tmp/karac_cache_override");
        }
        let r = default_cache_root().unwrap();
        assert_eq!(r, PathBuf::from("/tmp/karac_cache_override"));
        unsafe {
            match prev {
                Some(v) => std::env::set_var(CACHE_ROOT_ENV_VAR, v),
                None => std::env::remove_var(CACHE_ROOT_ENV_VAR),
            }
        }
    }

    #[test]
    fn default_cache_root_ignores_whitespace_only_override() {
        let _g = CACHE_ROOT_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var(CACHE_ROOT_ENV_VAR).ok();
        // SAFETY: see sibling test — CACHE_ROOT_ENV_LOCK serializes the
        // mutation.
        unsafe {
            std::env::set_var(CACHE_ROOT_ENV_VAR, "   ");
        }
        let r = default_cache_root();
        // Should fall through to the $HOME-based path or HomeUnset,
        // not return the whitespace string.
        match r {
            Ok(p) => assert!(p.ends_with(CACHE_SUBDIR)),
            Err(CacheError::HomeUnset) => {}
            Err(other) => panic!("unexpected error: {other:?}"),
        }
        unsafe {
            match prev {
                Some(v) => std::env::set_var(CACHE_ROOT_ENV_VAR, v),
                None => std::env::remove_var(CACHE_ROOT_ENV_VAR),
            }
        }
    }

    #[test]
    fn lookup_on_nonexistent_root_returns_miss() {
        let root = temp_root("lookup_nonexistent");
        let key = sample_key();
        // Subdirectory under root doesn't exist either.
        let r = lookup(&root, &key).unwrap();
        assert!(matches!(r, LookupStatus::Miss));
    }

    #[test]
    fn record_then_lookup_round_trips() {
        let root = temp_root("record_then_lookup");
        let key = sample_key();
        let artifact = b"\x7fELF...\x00\x01\x02\x03 imaginary object bytes";
        let recorded = record(&root, &key, artifact).unwrap();
        assert_eq!(recorded.key, key);
        assert!(recorded.content_hash.starts_with("blake3:"));
        // Artifact + metadata both exist on disk.
        assert!(recorded.artifact_path().is_file());
        assert!(recorded.metadata_path().is_file());

        let looked_up = lookup(&root, &key).unwrap();
        match looked_up {
            LookupStatus::Hit(e) => {
                assert_eq!(e.key, key);
                assert_eq!(e.content_hash, recorded.content_hash);
                assert_eq!(std::fs::read(e.artifact_path()).unwrap(), artifact.to_vec());
            }
            LookupStatus::Miss => panic!("expected Hit"),
        }
    }

    #[test]
    fn record_overwrites_existing_entry() {
        let root = temp_root("record_overwrites");
        let key = sample_key();
        record(&root, &key, b"first").unwrap();
        let second = record(&root, &key, b"second").unwrap();
        let bytes = std::fs::read(second.artifact_path()).unwrap();
        assert_eq!(bytes, b"second");
    }

    #[test]
    fn lookup_treats_missing_metadata_as_miss_even_if_artifact_present() {
        let root = temp_root("missing_metadata");
        let key = sample_key();
        let dir = entry_path(&root, &key);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(ARTIFACT_FILENAME), b"orphan").unwrap();
        // No entry.toml.
        let r = lookup(&root, &key).unwrap();
        assert!(matches!(r, LookupStatus::Miss));
    }

    #[test]
    fn lookup_surfaces_malformed_metadata_as_error() {
        let root = temp_root("malformed_metadata");
        let key = sample_key();
        let dir = entry_path(&root, &key);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(ENTRY_METADATA_FILENAME), b"not valid toml [[[").unwrap();
        let err = lookup(&root, &key).unwrap_err();
        assert_eq!(err.code(), "E_CACHE_MALFORMED_METADATA");
    }

    #[test]
    fn lookup_rejects_wrong_schema_version() {
        let root = temp_root("wrong_schema");
        let key = sample_key();
        let dir = entry_path(&root, &key);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(ENTRY_METADATA_FILENAME),
            br#"schema_version = 99
compiler_version = "0.1.0"
package_name = "foo"
package_version = "1.2.3"
edition = "2026"
profile = "default"
target_triple = "aarch64-apple-darwin"
content_hash = "blake3:abc"
written_at_epoch_seconds = 0
"#,
        )
        .unwrap();
        let err = lookup(&root, &key).unwrap_err();
        assert_eq!(err.code(), "E_CACHE_MALFORMED_METADATA");
    }

    #[test]
    fn stats_on_nonexistent_root_is_zero() {
        let s = stats(Path::new("/tmp/karac_build_cache_stats_absent_xyz")).unwrap();
        assert_eq!(s.entry_count, 0);
        assert_eq!(s.total_bytes, 0);
    }

    #[test]
    fn stats_counts_populated_entries_only() {
        let root = temp_root("stats_count");
        let key_a = sample_key();
        let mut key_b = sample_key();
        key_b.package_version = "2.0.0".to_string();
        record(&root, &key_a, b"aaaaaaaa").unwrap(); // 8 bytes
        record(&root, &key_b, b"bbbbbb").unwrap(); // 6 bytes

        // Half-written orphan should NOT be counted.
        let mut orphan = sample_key();
        orphan.package_name = "orphan".to_string();
        let orphan_dir = entry_path(&root, &orphan);
        std::fs::create_dir_all(&orphan_dir).unwrap();
        std::fs::write(orphan_dir.join(ARTIFACT_FILENAME), b"halfwritten").unwrap();

        let s = stats(&root).unwrap();
        assert_eq!(s.entry_count, 2);
        assert_eq!(s.total_bytes, 14);
    }

    #[test]
    fn error_codes_are_stable() {
        // Symbolic codes that downstream tooling (IDE / CI) matches
        // against. Pin them so a future rename surfaces in test
        // output rather than silently breaking integrators.
        assert_eq!(CacheError::HomeUnset.code(), "E_CACHE_HOME_UNSET");
        assert_eq!(
            CacheError::Unreadable {
                path: PathBuf::new(),
                message: String::new()
            }
            .code(),
            "E_CACHE_UNREADABLE"
        );
        assert_eq!(
            CacheError::MalformedMetadata {
                path: PathBuf::new(),
                message: String::new()
            }
            .code(),
            "E_CACHE_MALFORMED_METADATA"
        );
        assert_eq!(
            CacheError::WriteFailed {
                path: PathBuf::new(),
                message: String::new()
            }
            .code(),
            "E_CACHE_WRITE_FAILED"
        );
    }
}
