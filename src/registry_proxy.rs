//! Registry proxy client — typed surface for fetching package metadata and
//! tarballs through `proxy.kara-lang.org` (or a configured mirror).
//!
//! Slice 1 of the registry-proxy entry (phase-5 tracker line 851). The
//! proxy itself is separate infrastructure (not part of `karac`); this
//! module is the client side. v1.1 ships the typed protocol, URL
//! discovery, and an in-memory `MemProxyClient` for tests. The live
//! HTTP fetch lands in a v1.1.x slice once the proxy is deployed
//! (carve-outs at the tracker entry).

use std::collections::BTreeMap;

/// Default proxy URL when neither the environment nor an explicit
/// override is supplied.
pub const DEFAULT_PROXY_URL: &str = "https://proxy.kara-lang.org";

/// Environment variable consulted by `ProxyConfig::from_env`. A
/// non-empty value overrides the default URL; an empty / whitespace
/// value is ignored so a stale shell export doesn't silently break a
/// build.
pub const PROXY_URL_ENV_VAR: &str = "KARAC_REGISTRY_PROXY";

/// Whether the user has opted out of the proxy. `--no-proxy` flips
/// this to `Disabled`; the default setting is `Default`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyMode {
    /// Use the configured proxy URL for registry fetches.
    Default,
    /// `--no-proxy` set; refuse to consult the proxy. Registry deps
    /// must be resolved through direct source URLs (a v1.1.x carve-out)
    /// or fail explicitly.
    Disabled,
}

/// Resolved configuration for the registry proxy client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyConfig {
    pub url: String,
    pub mode: ProxyMode,
}

impl ProxyConfig {
    /// Proxy enabled, default URL.
    pub fn default_enabled() -> Self {
        Self {
            url: DEFAULT_PROXY_URL.to_string(),
            mode: ProxyMode::Default,
        }
    }

    /// Proxy disabled. The URL is still populated (in case the caller
    /// needs to render it in a diagnostic) but no fetch should happen.
    pub fn disabled() -> Self {
        Self {
            url: DEFAULT_PROXY_URL.to_string(),
            mode: ProxyMode::Disabled,
        }
    }

    /// Build a config from the environment. The URL is taken from
    /// `KARAC_REGISTRY_PROXY` when set non-empty; otherwise the default.
    /// The mode comes from explicit CLI input (the caller decides
    /// whether `--no-proxy` was passed).
    pub fn from_env(mode: ProxyMode) -> Self {
        let url = std::env::var(PROXY_URL_ENV_VAR)
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_PROXY_URL.to_string());
        Self { url, mode }
    }

    pub fn is_enabled(&self) -> bool {
        matches!(self.mode, ProxyMode::Default)
    }
}

/// Catalog metadata for a package: every published version (ascending
/// semver order) plus the upstream source URL the proxy is mirroring.
/// The proxy itself preserves the original source URL so the lockfile
/// can record both halves (the upstream for human readability, the
/// proxy URL for fetch reproducibility).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedManifest {
    pub package: String,
    pub upstream_url: String,
    pub versions: Vec<semver::Version>,
}

/// One concrete fetched package: the tarball bytes plus the URLs and
/// content hash needed to reproduce the fetch. `upstream_url` is the
/// original source (e.g. a git URL); `mirror_url` is the proxy mirror
/// reference. Both halves land in `kara.lock` (slice 3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedPackage {
    pub package: String,
    pub version: semver::Version,
    pub upstream_url: String,
    pub mirror_url: String,
    pub tarball_bytes: Vec<u8>,
    pub content_hash: String,
}

/// Failure surface for proxy fetches. The symbolic codes follow the
/// `E_PROXY_*` namespace; the diagnostic renderer in `cli.rs` maps
/// each to a structured payload through the existing `OutputMode`
/// pipeline.
#[derive(Debug, PartialEq, Eq)]
pub enum ProxyClientError {
    /// Proxy is disabled (`--no-proxy`) but a fetch was attempted.
    Disabled,
    /// Network / transport failure reaching the proxy URL.
    Unreachable { url: String, message: String },
    /// The proxy responded but did not know about this package.
    PackageNotFound { name: String },
    /// The proxy responded but did not have the requested version.
    VersionNotFound {
        name: String,
        version: semver::Version,
    },
    /// The proxy responded but the payload did not match the expected
    /// catalog / tarball-envelope shape.
    MalformedResponse { url: String, message: String },
    /// Live HTTP fetch is not yet implemented (v1.1.x carve-out). The
    /// typed surface is ready; the wire-level fetch lands once the
    /// proxy infra is deployed.
    NotImplemented { feature: &'static str },
}

impl ProxyClientError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Disabled => "E_PROXY_DISABLED",
            Self::Unreachable { .. } => "E_PROXY_UNREACHABLE",
            Self::PackageNotFound { .. } => "E_PROXY_PACKAGE_NOT_FOUND",
            Self::VersionNotFound { .. } => "E_PROXY_VERSION_NOT_FOUND",
            Self::MalformedResponse { .. } => "E_PROXY_MALFORMED_RESPONSE",
            Self::NotImplemented { .. } => "E_PROXY_NOT_IMPLEMENTED",
        }
    }
}

impl std::fmt::Display for ProxyClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disabled => {
                write!(f, "registry proxy is disabled (--no-proxy)")
            }
            Self::Unreachable { url, message } => {
                write!(f, "could not reach registry proxy at {url}: {message}")
            }
            Self::PackageNotFound { name } => {
                write!(f, "registry proxy: package {name:?} not found")
            }
            Self::VersionNotFound { name, version } => write!(
                f,
                "registry proxy: version {version} of package {name:?} not found"
            ),
            Self::MalformedResponse { url, message } => {
                write!(
                    f,
                    "registry proxy: malformed response from {url}: {message}"
                )
            }
            Self::NotImplemented { feature } => write!(
                f,
                "{feature} is not yet implemented (registry-proxy fetch ships in v1.1.x once \
                 the proxy infrastructure is deployed)"
            ),
        }
    }
}

impl std::error::Error for ProxyClientError {}

/// Abstract proxy-fetch surface. Production callers will use a
/// `ureq`-backed `HttpProxyClient` once the proxy is deployed; tests
/// use `MemProxyClient` with canned data. The trait is small on
/// purpose — `fetch_catalog` returns version metadata, `fetch_package`
/// returns one concrete tarball.
pub trait ProxyClient {
    fn fetch_catalog(&self, package: &str) -> Result<FetchedManifest, ProxyClientError>;
    fn fetch_package(
        &self,
        package: &str,
        version: &semver::Version,
    ) -> Result<FetchedPackage, ProxyClientError>;
}

/// In-memory `ProxyClient` for tests. Catalogs are keyed by package
/// name; packages are keyed by `(name, version)`. Production code does
/// not construct this directly — it's used by the resolver's unit /
/// integration tests once the fetch wiring lands.
#[derive(Debug, Default)]
pub struct MemProxyClient {
    pub catalogs: BTreeMap<String, FetchedManifest>,
    pub packages: BTreeMap<(String, semver::Version), FetchedPackage>,
}

impl MemProxyClient {
    pub fn new() -> Self {
        Self::default()
    }

    /// Convenience: insert a catalog entry with one or more versions.
    pub fn insert_catalog(
        &mut self,
        package: &str,
        upstream_url: &str,
        versions: Vec<semver::Version>,
    ) {
        self.catalogs.insert(
            package.to_string(),
            FetchedManifest {
                package: package.to_string(),
                upstream_url: upstream_url.to_string(),
                versions,
            },
        );
    }

    /// Convenience: insert a fetched package with the matching content
    /// hash already computed by the caller.
    pub fn insert_package(&mut self, package: FetchedPackage) {
        let key = (package.package.clone(), package.version.clone());
        self.packages.insert(key, package);
    }
}

impl ProxyClient for MemProxyClient {
    fn fetch_catalog(&self, package: &str) -> Result<FetchedManifest, ProxyClientError> {
        self.catalogs
            .get(package)
            .cloned()
            .ok_or_else(|| ProxyClientError::PackageNotFound {
                name: package.to_string(),
            })
    }

    fn fetch_package(
        &self,
        package: &str,
        version: &semver::Version,
    ) -> Result<FetchedPackage, ProxyClientError> {
        self.packages
            .get(&(package.to_string(), version.clone()))
            .cloned()
            .ok_or_else(|| ProxyClientError::VersionNotFound {
                name: package.to_string(),
                version: version.clone(),
            })
    }
}

/// `ProxyClient` that refuses every fetch with `Disabled`. Used when
/// `--no-proxy` is set so the caller still has a uniform interface to
/// call into without branching on the mode at every call site.
pub struct DisabledProxyClient;

impl ProxyClient for DisabledProxyClient {
    fn fetch_catalog(&self, _package: &str) -> Result<FetchedManifest, ProxyClientError> {
        Err(ProxyClientError::Disabled)
    }

    fn fetch_package(
        &self,
        _package: &str,
        _version: &semver::Version,
    ) -> Result<FetchedPackage, ProxyClientError> {
        Err(ProxyClientError::Disabled)
    }
}

/// Build the runtime `ProxyClient` for the current `ProxyConfig`. When
/// `Disabled`, returns the `DisabledProxyClient`. When enabled, returns
/// an `HttpProxyClient` that currently surfaces `NotImplemented` for
/// every fetch — the typed surface is stable today; live HTTP lands in
/// the v1.1.x slice once the proxy is deployed.
pub fn make_client(config: &ProxyConfig) -> Box<dyn ProxyClient> {
    match config.mode {
        ProxyMode::Disabled => Box::new(DisabledProxyClient),
        ProxyMode::Default => Box::new(HttpProxyClient::new(config.url.clone())),
    }
}

/// `ureq`-backed proxy client. v1.1 ships only the constructor + the
/// `NotImplemented` failure surface for each fetch method, since the
/// proxy infrastructure is a separate deployment that hasn't shipped
/// yet. Once it's live, the fetch methods get the actual HTTP wire
/// implementation without changing the trait signature.
#[cfg(not(target_arch = "wasm32"))]
pub struct HttpProxyClient {
    #[allow(dead_code)]
    pub url: String,
}

#[cfg(not(target_arch = "wasm32"))]
impl HttpProxyClient {
    pub fn new(url: String) -> Self {
        Self { url }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl ProxyClient for HttpProxyClient {
    fn fetch_catalog(&self, _package: &str) -> Result<FetchedManifest, ProxyClientError> {
        Err(ProxyClientError::NotImplemented {
            feature: "registry proxy catalog fetch",
        })
    }

    fn fetch_package(
        &self,
        _package: &str,
        _version: &semver::Version,
    ) -> Result<FetchedPackage, ProxyClientError> {
        Err(ProxyClientError::NotImplemented {
            feature: "registry proxy package fetch",
        })
    }
}

/// wasm32 stub — the browser playground (tracker line 703) has no
/// outbound HTTP surface; fetches surface as `NotImplemented` so user
/// code calling a registry-fetch path fails cleanly rather than
/// compile-erroring. The proxy infrastructure is native-only by design.
#[cfg(target_arch = "wasm32")]
pub struct HttpProxyClient {
    pub url: String,
}

#[cfg(target_arch = "wasm32")]
impl HttpProxyClient {
    pub fn new(url: String) -> Self {
        Self { url }
    }
}

#[cfg(target_arch = "wasm32")]
impl ProxyClient for HttpProxyClient {
    fn fetch_catalog(&self, _package: &str) -> Result<FetchedManifest, ProxyClientError> {
        Err(ProxyClientError::NotImplemented {
            feature: "registry proxy catalog fetch (wasm32)",
        })
    }

    fn fetch_package(
        &self,
        _package: &str,
        _version: &semver::Version,
    ) -> Result<FetchedPackage, ProxyClientError> {
        Err(ProxyClientError::NotImplemented {
            feature: "registry proxy package fetch (wasm32)",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes tests that mutate the process-wide `PROXY_URL_ENV_VAR`.
    /// Without this, the `from_env_*` tests race under cargo's default
    /// parallel execution — one test's `set_var` / `remove_var` can land
    /// between a sibling's `set_var` and its `from_env` read, corrupting
    /// either assertion (observed as an intermittent
    /// `from_env_ignores_whitespace_only_var` failure). Acquire with
    /// `unwrap_or_else(|e| e.into_inner())` so a panicked test (poisoned
    /// mutex) doesn't cascade-fail the rest. Mirrors `build_cache.rs`'s
    /// `CACHE_ROOT_ENV_LOCK` and `runtime/src/lib.rs`'s
    /// `FRAME_TRACKING_ENV_LOCK`.
    static PROXY_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn v(s: &str) -> semver::Version {
        semver::Version::parse(s).unwrap()
    }

    #[test]
    fn default_enabled_uses_default_url() {
        let c = ProxyConfig::default_enabled();
        assert_eq!(c.url, DEFAULT_PROXY_URL);
        assert_eq!(c.mode, ProxyMode::Default);
        assert!(c.is_enabled());
    }

    #[test]
    fn disabled_marks_mode() {
        let c = ProxyConfig::disabled();
        assert_eq!(c.mode, ProxyMode::Disabled);
        assert!(!c.is_enabled());
    }

    #[test]
    fn from_env_uses_default_when_unset() {
        let _g = PROXY_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var(PROXY_URL_ENV_VAR);
        let c = ProxyConfig::from_env(ProxyMode::Default);
        assert_eq!(c.url, DEFAULT_PROXY_URL);
    }

    #[test]
    fn from_env_uses_var_when_nonempty() {
        let _g = PROXY_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var(PROXY_URL_ENV_VAR, "https://mirror.example.com");
        let c = ProxyConfig::from_env(ProxyMode::Default);
        assert_eq!(c.url, "https://mirror.example.com");
        std::env::remove_var(PROXY_URL_ENV_VAR);
    }

    #[test]
    fn from_env_ignores_whitespace_only_var() {
        let _g = PROXY_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var(PROXY_URL_ENV_VAR, "   ");
        let c = ProxyConfig::from_env(ProxyMode::Default);
        assert_eq!(c.url, DEFAULT_PROXY_URL);
        std::env::remove_var(PROXY_URL_ENV_VAR);
    }

    #[test]
    fn mem_client_returns_canned_catalog() {
        let mut client = MemProxyClient::new();
        client.insert_catalog(
            "http",
            "https://upstream.example/http",
            vec![v("1.0.0"), v("1.2.3")],
        );
        let m = client.fetch_catalog("http").unwrap();
        assert_eq!(m.package, "http");
        assert_eq!(m.upstream_url, "https://upstream.example/http");
        assert_eq!(m.versions, vec![v("1.0.0"), v("1.2.3")]);
    }

    #[test]
    fn mem_client_missing_catalog_is_package_not_found() {
        let client = MemProxyClient::new();
        let err = client.fetch_catalog("nope").unwrap_err();
        assert_eq!(err.code(), "E_PROXY_PACKAGE_NOT_FOUND");
        assert!(matches!(err, ProxyClientError::PackageNotFound { .. }));
    }

    #[test]
    fn mem_client_returns_canned_package() {
        let mut client = MemProxyClient::new();
        let pkg = FetchedPackage {
            package: "http".to_string(),
            version: v("1.2.3"),
            upstream_url: "https://upstream.example/http".to_string(),
            mirror_url: "https://proxy.kara-lang.org/http/1.2.3".to_string(),
            tarball_bytes: vec![0xde, 0xad, 0xbe, 0xef],
            content_hash: "blake3:cafe".to_string(),
        };
        client.insert_package(pkg.clone());
        let got = client.fetch_package("http", &v("1.2.3")).unwrap();
        assert_eq!(got, pkg);
    }

    #[test]
    fn mem_client_missing_version_is_version_not_found() {
        let mut client = MemProxyClient::new();
        client.insert_catalog("http", "https://upstream.example/http", vec![v("1.0.0")]);
        let err = client.fetch_package("http", &v("9.9.9")).unwrap_err();
        assert_eq!(err.code(), "E_PROXY_VERSION_NOT_FOUND");
    }

    #[test]
    fn disabled_client_refuses_every_call() {
        let client = DisabledProxyClient;
        assert_eq!(
            client.fetch_catalog("anything").unwrap_err().code(),
            "E_PROXY_DISABLED"
        );
        assert_eq!(
            client
                .fetch_package("anything", &v("1.0.0"))
                .unwrap_err()
                .code(),
            "E_PROXY_DISABLED"
        );
    }

    #[test]
    fn make_client_selects_disabled_when_mode_disabled() {
        let client = make_client(&ProxyConfig::disabled());
        let err = client.fetch_catalog("anything").unwrap_err();
        assert_eq!(err.code(), "E_PROXY_DISABLED");
    }

    #[test]
    fn make_client_selects_http_when_mode_default() {
        let client = make_client(&ProxyConfig::default_enabled());
        // v1.1 surface: HTTP client surfaces NotImplemented uniformly
        // until the proxy infrastructure ships. The mode-routing test
        // pins that we're getting the HTTP client (not the Disabled
        // one) by checking for E_PROXY_NOT_IMPLEMENTED rather than
        // E_PROXY_DISABLED.
        let err = client.fetch_catalog("anything").unwrap_err();
        assert_eq!(err.code(), "E_PROXY_NOT_IMPLEMENTED");
    }

    #[test]
    fn all_error_codes_round_trip() {
        // Stability pin: each variant's code must remain the documented
        // E_PROXY_* string so the diagnostic renderer doesn't drift.
        let cases: Vec<(ProxyClientError, &str)> = vec![
            (ProxyClientError::Disabled, "E_PROXY_DISABLED"),
            (
                ProxyClientError::Unreachable {
                    url: "x".into(),
                    message: "y".into(),
                },
                "E_PROXY_UNREACHABLE",
            ),
            (
                ProxyClientError::PackageNotFound { name: "z".into() },
                "E_PROXY_PACKAGE_NOT_FOUND",
            ),
            (
                ProxyClientError::VersionNotFound {
                    name: "z".into(),
                    version: v("1.0.0"),
                },
                "E_PROXY_VERSION_NOT_FOUND",
            ),
            (
                ProxyClientError::MalformedResponse {
                    url: "x".into(),
                    message: "y".into(),
                },
                "E_PROXY_MALFORMED_RESPONSE",
            ),
            (
                ProxyClientError::NotImplemented { feature: "x" },
                "E_PROXY_NOT_IMPLEMENTED",
            ),
        ];
        for (err, expected) in cases {
            assert_eq!(err.code(), expected);
        }
    }
}
