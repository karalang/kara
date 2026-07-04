//! Extraction of fetched registry tarballs into a source directory.
//!
//! Slice 2 of the registry fetch epic (phase-5-diagnostics.md resolver
//! follow-up (b)). This decouples "get the bytes" ([`crate::registry_proxy`]
//! `fetch_registry_package`) from "put them on disk so the compiler can
//! read the package". Slice 3 wires the two together in the resolver.
//!
//! The proxy serves each package version as a gzip-compressed tarball
//! (`application/gzip`, `Karac-Content-Hash: blake3:<hex>`). Here we
//! decompress (`flate2`) and unpack (`tar`) into a destination directory.
//! Unpacking uses the `tar` crate's default protection: entries whose
//! resolved path would escape the destination (`../…`, absolute paths,
//! escaping symlinks) are refused rather than written outside — the
//! zip-slip guard a package manager must have.

use crate::dep_graph::{MaterializedDep, RegistryProvider};
use crate::registry_proxy::{
    fetch_registry_package, fetch_registry_package_at, list_registry_versions, FetchedPackage,
    ProxyClient,
};
use std::path::{Path, PathBuf};

/// Failure extracting a fetched tarball.
#[derive(Debug)]
pub struct ExtractError {
    message: String,
}

impl ExtractError {
    fn new(message: impl Into<String>) -> Self {
        ExtractError {
            message: message.into(),
        }
    }

    pub fn code(&self) -> &'static str {
        "E_REGISTRY_EXTRACT_FAILED"
    }
}

impl std::fmt::Display for ExtractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ExtractError {}

/// Extract a gzip-compressed tarball (`gz_bytes`) into `dest`, creating
/// `dest` if needed. The archive's own directory structure is mirrored
/// under `dest` (e.g. an entry `mylib/src/lib.kara` lands at
/// `dest/mylib/src/lib.kara`).
///
/// Path-traversal safety: unpacking refuses any entry resolving outside
/// `dest`, so a malicious archive cannot write elsewhere on disk.
pub fn extract_tarball(gz_bytes: &[u8], dest: &Path) -> Result<(), ExtractError> {
    std::fs::create_dir_all(dest)
        .map_err(|e| ExtractError::new(format!("could not create {}: {e}", dest.display())))?;

    let decoder = flate2::read::GzDecoder::new(gz_bytes);
    let mut archive = tar::Archive::new(decoder);
    // Don't restore ownership/permissions from the archive — a mirror's
    // uid/gid/mode bits are meaningless on the fetching machine, and
    // honoring them risks writing unreadable or setuid files.
    archive.set_preserve_permissions(false);
    archive.set_preserve_mtime(false);
    archive
        .unpack(dest)
        .map_err(|e| ExtractError::new(format!("could not extract tarball: {e}")))?;
    Ok(())
}

/// Find the directory holding `kara.toml` within a freshly-extracted
/// tarball: either the extraction root itself, or a single top-level
/// subdirectory (the `<name>-<version>/` wrapper some archives use).
fn find_manifest_root(extract_dir: &Path) -> Option<PathBuf> {
    if extract_dir.join("kara.toml").is_file() {
        return Some(extract_dir.to_path_buf());
    }
    let subdirs: Vec<PathBuf> = std::fs::read_dir(extract_dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    if let [only] = subdirs.as_slice() {
        if only.join("kara.toml").is_file() {
            return Some(only.clone());
        }
    }
    None
}

/// Production [`RegistryProvider`]: resolves a registry dep to bytes via a
/// [`ProxyClient`] ([`fetch_registry_package`]), extracts the tarball into
/// a cache directory, and returns the extracted source root.
///
/// Extraction is idempotent — if the cache dir already holds an extracted
/// tree with a `kara.toml`, it is reused rather than re-extracted. Wrap the
/// `ProxyClient` in a `CachingProxyClient` / `RetryingProxyClient` for
/// tarball caching + transient-failure resilience.
pub struct ProxyRegistryProvider<'a> {
    client: &'a dyn ProxyClient,
    cache_root: PathBuf,
}

impl<'a> ProxyRegistryProvider<'a> {
    pub fn new(client: &'a dyn ProxyClient, cache_root: impl Into<PathBuf>) -> Self {
        Self {
            client,
            cache_root: cache_root.into(),
        }
    }
}

impl ProxyRegistryProvider<'_> {
    /// Extract a fetched package into `<cache_root>/<name>/<version>/src`
    /// (mirroring the tarball's own layout) and return the materialized dep.
    /// Idempotent: an already-extracted tree with a `kara.toml` is reused.
    /// Shared by [`fetch`](RegistryProvider::fetch) (range selection) and
    /// [`fetch_exact`](RegistryProvider::fetch_exact) (pinned) — the only
    /// difference between them is how `pkg` was chosen upstream.
    fn extract_materialized(
        &self,
        name: &str,
        pkg: FetchedPackage,
    ) -> Result<MaterializedDep, String> {
        let extract_dir = self
            .cache_root
            .join(name)
            .join(pkg.version.to_string())
            .join("src");
        let root = match find_manifest_root(&extract_dir) {
            Some(root) => root, // already extracted — reuse
            None => {
                extract_tarball(&pkg.tarball_bytes, &extract_dir).map_err(|e| e.to_string())?;
                find_manifest_root(&extract_dir).ok_or_else(|| {
                    format!(
                        "extracted tarball for `{name}` {} contains no kara.toml",
                        pkg.version
                    )
                })?
            }
        };
        Ok(MaterializedDep {
            root_dir: root,
            version: pkg.version,
            upstream_url: pkg.upstream_url,
        })
    }
}

impl RegistryProvider for ProxyRegistryProvider<'_> {
    fn fetch(&self, name: &str, req: &semver::VersionReq) -> Result<MaterializedDep, String> {
        let pkg = fetch_registry_package(self.client, name, req).map_err(|e| e.to_string())?;
        self.extract_materialized(name, pkg)
    }

    fn available_versions(&self, name: &str) -> Result<Vec<semver::Version>, String> {
        list_registry_versions(self.client, name).map_err(|e| e.to_string())
    }

    fn fetch_exact(
        &self,
        name: &str,
        version: &semver::Version,
    ) -> Result<MaterializedDep, String> {
        let pkg =
            fetch_registry_package_at(self.client, name, version).map_err(|e| e.to_string())?;
        self.extract_materialized(name, pkg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    fn temp_dir() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("kara-extract-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Build a gzip-compressed tarball in memory from `(path, contents)`
    /// entries, using the same `tar` + `flate2` crates the extractor reads.
    fn make_targz(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut builder = tar::Builder::new(gz);
        for (path, contents) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, path, *contents).unwrap();
        }
        let gz = builder.into_inner().unwrap();
        gz.finish().unwrap()
    }

    #[test]
    fn extracts_nested_files_with_content() {
        let targz = make_targz(&[
            ("mylib/kara.toml", b"[package]\nname = \"mylib\"\n"),
            ("mylib/src/lib.kara", b"pub fn hi() -> i64 { 42 }\n"),
        ]);
        let dest = temp_dir();
        extract_tarball(&targz, &dest).expect("extract");

        assert_eq!(
            std::fs::read_to_string(dest.join("mylib/kara.toml")).unwrap(),
            "[package]\nname = \"mylib\"\n"
        );
        assert_eq!(
            std::fs::read_to_string(dest.join("mylib/src/lib.kara")).unwrap(),
            "pub fn hi() -> i64 { 42 }\n"
        );
    }

    #[test]
    fn creates_missing_destination() {
        let targz = make_targz(&[("a.txt", b"x")]);
        let dest = temp_dir().join("does/not/exist/yet");
        extract_tarball(&targz, &dest).expect("extract into fresh dir");
        assert_eq!(std::fs::read_to_string(dest.join("a.txt")).unwrap(), "x");
    }

    #[test]
    fn garbage_bytes_are_an_error_not_a_panic() {
        let dest = temp_dir();
        let err = extract_tarball(b"this is not a gzip stream at all", &dest).unwrap_err();
        assert_eq!(err.code(), "E_REGISTRY_EXTRACT_FAILED");
    }

    #[test]
    fn traversal_entry_does_not_escape_destination() {
        // Hand-craft a tar entry named "../escape.txt" (bypassing the
        // builder's path validation) to prove the unpack guard refuses it.
        let mut header = tar::Header::new_gnu();
        let body = b"pwned";
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        // set_path rejects "..", so write the name straight into the header.
        let name = b"../escape.txt";
        header.as_old_mut().name[..name.len()].copy_from_slice(name);
        header.set_cksum();

        let mut tar_bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_bytes);
            builder.append(&header, &body[..]).unwrap();
            builder.finish().unwrap();
        }
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(&tar_bytes).unwrap();
        let targz = gz.finish().unwrap();

        let parent = temp_dir();
        let dest = parent.join("pkg");
        // Extraction succeeds (the guard skips the bad entry) but nothing is
        // written outside `dest`.
        let _ = extract_tarball(&targz, &dest);
        assert!(
            !parent.join("escape.txt").exists(),
            "traversal entry escaped the destination directory"
        );
    }

    // ── ProxyRegistryProvider ───────────────────────────────────

    use crate::registry_proxy::{FetchedPackage, MemProxyClient};

    fn ver(s: &str) -> semver::Version {
        semver::Version::parse(s).unwrap()
    }

    fn make_mem_client(package: &str, version: &str, entries: &[(&str, &[u8])]) -> MemProxyClient {
        let targz = make_targz(entries);
        let content_hash = format!("blake3:{}", blake3::hash(&targz).to_hex());
        let mut mem = MemProxyClient::new();
        mem.insert_catalog(package, "https://up/pkg", vec![ver(version)]);
        mem.insert_package(FetchedPackage {
            package: package.to_string(),
            version: ver(version),
            upstream_url: String::new(),
            mirror_url: "m".to_string(),
            tarball_bytes: targz,
            content_hash,
        });
        mem
    }

    #[test]
    fn proxy_provider_fetches_extracts_and_locates_manifest() {
        let mem = make_mem_client(
            "http",
            "1.2.3",
            &[
                ("kara.toml", b"[package]\nname = \"http\"\n"),
                ("src/lib.kara", b"pub fn hi() -> i64 { 1 }\n"),
            ],
        );
        let cache = temp_dir();
        let provider = ProxyRegistryProvider::new(&mem, &cache);

        let dep = provider
            .fetch("http", &semver::VersionReq::parse("^1.0").unwrap())
            .expect("fetch");
        assert_eq!(dep.version, ver("1.2.3"));
        // upstream_url stitched from the catalog by fetch_registry_package.
        assert_eq!(dep.upstream_url, "https://up/pkg");
        assert!(dep.root_dir.join("kara.toml").is_file());
        assert!(dep.root_dir.join("src/lib.kara").is_file());
    }

    #[test]
    fn proxy_provider_finds_manifest_in_wrapper_subdir() {
        // Some archives wrap everything under a `<name>-<version>/` dir.
        let mem = make_mem_client(
            "http",
            "2.0.0",
            &[("http-2.0.0/kara.toml", b"[package]\nname = \"http\"\n")],
        );
        let cache = temp_dir();
        let provider = ProxyRegistryProvider::new(&mem, &cache);

        let dep = provider
            .fetch("http", &semver::VersionReq::parse("^2").unwrap())
            .expect("fetch");
        assert!(dep.root_dir.join("kara.toml").is_file());
        assert!(dep.root_dir.ends_with("http-2.0.0"));
    }

    #[test]
    fn proxy_provider_no_matching_version_errors() {
        let mem = make_mem_client("http", "1.0.0", &[("kara.toml", b"x")]);
        let cache = temp_dir();
        let provider = ProxyRegistryProvider::new(&mem, &cache);
        let err = provider
            .fetch("http", &semver::VersionReq::parse("^2.0").unwrap())
            .unwrap_err();
        assert!(err.contains("no version"), "unexpected error: {err}");
    }

    // ── candidate-set trait methods (resolver follow-up (a) slice 3b) ──

    /// A multi-version mem client: a catalog listing every `versions` entry
    /// (optionally marking some `yanked`), plus a per-version tarball whose
    /// `kara.toml` records the version so a test can prove *which* one was
    /// materialized.
    fn make_multi_mem_client(package: &str, versions: &[&str], yanked: &[&str]) -> MemProxyClient {
        let mut mem = MemProxyClient::new();
        let vers: Vec<semver::Version> = versions.iter().map(|s| ver(s)).collect();
        let yanks: Vec<semver::Version> = yanked.iter().map(|s| ver(s)).collect();
        mem.insert_catalog_with_yanked(package, "https://up/pkg", vers.clone(), yanks);
        for v in &vers {
            let manifest = format!("[package]\nname = \"{package}\"\nversion = \"{v}\"\n");
            let targz = make_targz(&[("kara.toml", manifest.as_bytes())]);
            let content_hash = format!("blake3:{}", blake3::hash(&targz).to_hex());
            mem.insert_package(FetchedPackage {
                package: package.to_string(),
                version: v.clone(),
                upstream_url: String::new(),
                mirror_url: "m".to_string(),
                tarball_bytes: targz,
                content_hash,
            });
        }
        mem
    }

    #[test]
    fn proxy_provider_available_versions_lists_selectable_ascending() {
        // Catalog out of order with one yanked — the provider surfaces the
        // selectable set, sorted, for the solver to widen over.
        let mem = make_multi_mem_client("http", &["2.0.0", "1.0.0", "1.9.0", "1.2.0"], &["1.9.0"]);
        let cache = temp_dir();
        let provider = ProxyRegistryProvider::new(&mem, &cache);
        let versions = provider.available_versions("http").expect("list");
        assert_eq!(versions, vec![ver("1.0.0"), ver("1.2.0"), ver("2.0.0")]);
    }

    #[test]
    fn proxy_provider_available_versions_unknown_package_errors() {
        let mem = MemProxyClient::new();
        let cache = temp_dir();
        let provider = ProxyRegistryProvider::new(&mem, &cache);
        let err = provider.available_versions("ghost").unwrap_err();
        assert!(
            err.contains("ghost") || err.contains("not found"),
            "err: {err}"
        );
    }

    #[test]
    fn proxy_provider_fetch_exact_materializes_the_named_version() {
        // Highest published is 2.0.0; the solver may have backtracked to 1.2.0.
        // fetch_exact must materialize precisely 1.2.0, extracted + located.
        let mem = make_multi_mem_client("http", &["1.0.0", "1.2.0", "2.0.0"], &[]);
        let cache = temp_dir();
        let provider = ProxyRegistryProvider::new(&mem, &cache);
        let dep = provider
            .fetch_exact("http", &ver("1.2.0"))
            .expect("fetch_exact");
        assert_eq!(dep.version, ver("1.2.0"));
        assert_eq!(dep.upstream_url, "https://up/pkg");
        let manifest = std::fs::read_to_string(dep.root_dir.join("kara.toml")).unwrap();
        assert!(
            manifest.contains("version = \"1.2.0\""),
            "materialized the wrong version's tree: {manifest}"
        );
    }

    #[test]
    fn proxy_provider_fetch_exact_resolves_a_yanked_pin() {
        // A yanked version is absent from available_versions but must still
        // materialize by exact pin — reproducing a lock can't fail on yank.
        let mem = make_multi_mem_client("http", &["1.0.0", "1.9.0"], &["1.9.0"]);
        let cache = temp_dir();
        let provider = ProxyRegistryProvider::new(&mem, &cache);
        assert!(
            !provider
                .available_versions("http")
                .unwrap()
                .contains(&ver("1.9.0")),
            "yanked version must not be in the selectable set"
        );
        let dep = provider
            .fetch_exact("http", &ver("1.9.0"))
            .expect("fetch yanked pin");
        assert_eq!(dep.version, ver("1.9.0"));
    }

    #[test]
    fn proxy_provider_fetch_exact_unpublished_version_errors() {
        let mem = make_multi_mem_client("http", &["1.0.0", "1.2.0"], &[]);
        let cache = temp_dir();
        let provider = ProxyRegistryProvider::new(&mem, &cache);
        let err = provider.fetch_exact("http", &ver("1.5.0")).unwrap_err();
        assert!(err.contains("no version"), "unexpected error: {err}");
    }

    // ── trait defaults (a provider that only implements `fetch`) ──

    /// A minimal provider implementing *only* `fetch` — it exercises the
    /// default `available_versions` (empty) and default `fetch_exact`
    /// (delegates to `fetch` via an `=X.Y.Z` range).
    struct FetchOnlyProvider {
        version: semver::Version,
    }

    impl RegistryProvider for FetchOnlyProvider {
        fn fetch(&self, _name: &str, _req: &semver::VersionReq) -> Result<MaterializedDep, String> {
            Ok(MaterializedDep {
                root_dir: PathBuf::from("/x"),
                version: self.version.clone(),
                upstream_url: "u".to_string(),
            })
        }
    }

    #[test]
    fn default_available_versions_is_empty() {
        let p = FetchOnlyProvider {
            version: ver("1.0.0"),
        };
        assert!(p.available_versions("anything").unwrap().is_empty());
    }

    #[test]
    fn default_fetch_exact_delegates_to_fetch() {
        // The default routes the exact pin through `fetch` as an `=X.Y.Z`
        // range; this stub ignores the req and returns its canned dep, proving
        // the delegation path is wired.
        let p = FetchOnlyProvider {
            version: ver("3.1.4"),
        };
        let dep = p.fetch_exact("anything", &ver("3.1.4")).expect("delegated");
        assert_eq!(dep.version, ver("3.1.4"));
        assert_eq!(dep.root_dir, PathBuf::from("/x"));
    }
}
