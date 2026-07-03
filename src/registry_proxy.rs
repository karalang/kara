//! Registry proxy client — typed surface for fetching package metadata and
//! tarballs through `proxy.kara-lang.org` (or a configured mirror).
//!
//! The registry-proxy *server* is separate infrastructure (not part of
//! `karac`); this module is the client side. It ships the typed protocol,
//! URL discovery, an in-memory `MemProxyClient` for tests, and — on native
//! targets — a live `ureq`-backed `HttpProxyClient` that performs the real
//! HTTP fetch (tracker line 930). The wire contract it speaks is ratified
//! in `docs/registry-proxy-protocol.md`; the reference server that
//! implements it lives in `registry-proxy/`. On `wasm32` the browser
//! playground has no outbound HTTP surface, so `HttpProxyClient` there
//! returns `NotImplemented` (the proxy is native-only by design).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

/// Default proxy URL when neither the environment nor an explicit
/// override is supplied.
pub const DEFAULT_PROXY_URL: &str = "https://proxy.kara-lang.org";

/// Environment variable consulted by `ProxyConfig::from_env`. A
/// non-empty value overrides the default URL; an empty / whitespace
/// value is ignored so a stale shell export doesn't silently break a
/// build.
pub const PROXY_URL_ENV_VAR: &str = "KARAC_REGISTRY_PROXY";

/// Environment variable carrying the bearer token for an authenticated
/// proxy (registry-proxy follow-up (e)). Per-user and never committed —
/// sourced only from the environment, never from `kara.toml`. A non-empty
/// value is sent as `Authorization: Bearer <token>` on every catalog /
/// package request; an empty / whitespace value is treated as absent so a
/// stray export doesn't send a bogus empty credential.
pub const REGISTRY_TOKEN_ENV_VAR: &str = "KARAC_REGISTRY_TOKEN";

/// Environment variable overriding the on-disk registry tarball cache
/// root (consulted by [`default_registry_cache_root`]). Non-empty wins;
/// otherwise the cache lives under `~/.kara/cache/registry/`.
pub const REGISTRY_CACHE_ROOT_ENV_VAR: &str = "KARAC_REGISTRY_CACHE_ROOT";

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
    /// Bearer token for an authenticated proxy, from `KARAC_REGISTRY_TOKEN`
    /// (per-user, never committed). `None` queries the proxy unauthenticated
    /// (the public `proxy.kara-lang.org` case). Populated by [`Self::resolve`]
    /// / [`Self::from_env`]; the explicit `default_enabled` / `disabled`
    /// constructors leave it `None`.
    pub token: Option<String>,
}

impl ProxyConfig {
    /// Proxy enabled, default URL, no token.
    pub fn default_enabled() -> Self {
        Self {
            url: DEFAULT_PROXY_URL.to_string(),
            mode: ProxyMode::Default,
            token: None,
        }
    }

    /// Proxy disabled. The URL is still populated (in case the caller
    /// needs to render it in a diagnostic) but no fetch should happen.
    pub fn disabled() -> Self {
        Self {
            url: DEFAULT_PROXY_URL.to_string(),
            mode: ProxyMode::Disabled,
            token: None,
        }
    }

    /// Build a config from the environment. The URL is taken from
    /// `KARAC_REGISTRY_PROXY` when set non-empty; otherwise the default.
    /// The mode comes from explicit CLI input (the caller decides
    /// whether `--no-proxy` was passed). Equivalent to [`Self::resolve`]
    /// with no manifest override.
    pub fn from_env(mode: ProxyMode) -> Self {
        Self::resolve(mode, None)
    }

    /// Resolve the effective proxy URL across all three tiers, highest
    /// precedence first:
    ///
    /// 1. the `KARAC_REGISTRY_PROXY` env var, when set non-empty (a
    ///    per-shell override, so a contributor can redirect ad-hoc);
    /// 2. `manifest_proxy_url` — the project's `[build].registry-proxy`
    ///    pin (registry-proxy follow-up (g)), when present and non-empty;
    /// 3. the built-in [`DEFAULT_PROXY_URL`].
    ///
    /// `mode` is decided by the caller (`--no-proxy` → `Disabled`). Keeping
    /// the precedence here means the fetch path has a single place to ask
    /// for the effective URL rather than re-deriving it at each call site.
    pub fn resolve(mode: ProxyMode, manifest_proxy_url: Option<&str>) -> Self {
        let url = std::env::var(PROXY_URL_ENV_VAR)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                manifest_proxy_url
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| DEFAULT_PROXY_URL.to_string());
        Self {
            url,
            mode,
            token: registry_token_from_env(),
        }
    }

    pub fn is_enabled(&self) -> bool {
        matches!(self.mode, ProxyMode::Default)
    }
}

/// Resolve the proxy auth token from [`REGISTRY_TOKEN_ENV_VAR`]. Trimmed;
/// whitespace-only is treated as absent so a stray `export
/// KARAC_REGISTRY_TOKEN=` doesn't send an empty bearer credential. `None`
/// means the proxy is queried unauthenticated.
pub fn registry_token_from_env() -> Option<String> {
    std::env::var(REGISTRY_TOKEN_ENV_VAR)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Whether an *explicit* proxy URL is configured — the `KARAC_REGISTRY_PROXY`
/// env override or a project's `[build].registry-proxy` pin — as opposed to
/// falling back to the built-in [`DEFAULT_PROXY_URL`] placeholder.
///
/// The CLI activates real registry fetch only when this is `true`. The
/// default URL is a not-yet-live placeholder, so a project that declares
/// registry deps but points at no configured proxy keeps the pre-fetch
/// warn-and-continue contract (`E_REGISTRY_DEP_UNSUPPORTED` downgraded to a
/// warning) instead of hard-failing against an address that answers nothing.
/// Mirrors the precedence tiers of [`ProxyConfig::resolve`], minus the
/// default fallback.
pub fn explicit_proxy_configured(manifest_pin: Option<&str>) -> bool {
    let env_set = std::env::var(PROXY_URL_ENV_VAR)
        .ok()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    let pin_set = manifest_pin.map(|s| !s.trim().is_empty()).unwrap_or(false);
    env_set || pin_set
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
    /// The proxy rejected the request as unauthenticated / unauthorized
    /// (HTTP 401 / 403). Either no `KARAC_REGISTRY_TOKEN` was supplied for a
    /// private proxy, or the supplied token was rejected.
    Unauthorized { url: String, status: u16 },
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
    /// Live HTTP fetch is unavailable on this target. Native builds
    /// perform the real fetch; this arm is produced only by the `wasm32`
    /// stub, where the playground has no outbound HTTP surface.
    NotImplemented { feature: &'static str },
}

impl ProxyClientError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Disabled => "E_PROXY_DISABLED",
            Self::Unreachable { .. } => "E_PROXY_UNREACHABLE",
            Self::Unauthorized { .. } => "E_PROXY_UNAUTHORIZED",
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
            Self::Unauthorized { url, status } => write!(
                f,
                "registry proxy at {url} rejected the request (HTTP {status}); \
                 set KARAC_REGISTRY_TOKEN to a valid credential for this proxy"
            ),
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
                "{feature} is unavailable on this target (registry-proxy fetch is native-only; \
                 the wasm playground has no outbound HTTP surface)"
            ),
        }
    }
}

impl std::error::Error for ProxyClientError {}

/// Abstract proxy-fetch surface. Production callers use the `ureq`-backed
/// `HttpProxyClient` (native); tests use `MemProxyClient` with canned data
/// or drive `HttpProxyClient` against the reference server in
/// `registry-proxy/`. The trait is small on purpose — `fetch_catalog`
/// returns version metadata, `fetch_package` returns one concrete tarball.
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
/// the live `HttpProxyClient` for the configured URL. Callers that want
/// on-disk tarball caching wrap the result in [`CachingProxyClient`].
pub fn make_client(config: &ProxyConfig) -> Box<dyn ProxyClient> {
    match config.mode {
        ProxyMode::Disabled => Box::new(DisabledProxyClient),
        ProxyMode::Default => Box::new(HttpProxyClient::with_token(
            config.url.clone(),
            config.token.clone(),
        )),
    }
}

/// Resolve the on-disk registry tarball cache root:
/// `KARAC_REGISTRY_CACHE_ROOT` when set non-empty, else
/// `~/.kara/cache/registry/` (sibling to the build-artifact cache). The
/// directory may not exist yet — [`CachingProxyClient`] creates entry
/// subdirectories lazily. Returns `None` only when neither the override
/// nor a home directory (`$HOME` / `$USERPROFILE`) is available, in which
/// case the caller should skip caching rather than fail the fetch.
pub fn default_registry_cache_root() -> Option<PathBuf> {
    if let Ok(over) = std::env::var(REGISTRY_CACHE_ROOT_ENV_VAR) {
        if !over.trim().is_empty() {
            return Some(PathBuf::from(over.trim()));
        }
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()?;
    Some(
        PathBuf::from(home)
            .join(".kara")
            .join("cache")
            .join("registry"),
    )
}

/// A [`ProxyClient`] decorator that caches fetched **tarballs** on disk so
/// repeated resolves — across projects on the same machine — don't refetch
/// bytes that never change (registry-proxy follow-up (c)).
///
/// Layout: `<root>/<name>/<version>/package.tar.gz` for the bytes and a
/// sibling `meta` file carrying `content_hash`, `mirror_url`, and
/// `upstream_url`. A per-`(name, version)` tarball is content-addressed and
/// immutable, so entries never need invalidation.
///
/// **Only `fetch_package` is cached.** `fetch_catalog` is passed straight
/// through: a catalog's version list grows as new releases publish, so
/// caching it would risk serving a stale set. On a cache hit the stored
/// bytes are re-hashed and checked against the stored `content_hash`; a
/// mismatch (disk corruption / truncation) is treated as a miss and the
/// package is refetched. Cache *writes* are best-effort — a failed write is
/// swallowed so a read-only or full cache never breaks a fetch.
pub struct CachingProxyClient {
    inner: Box<dyn ProxyClient>,
    root: PathBuf,
}

impl CachingProxyClient {
    /// Wrap `inner`, caching tarballs under `root`.
    pub fn new(inner: Box<dyn ProxyClient>, root: impl Into<PathBuf>) -> Self {
        Self {
            inner,
            root: root.into(),
        }
    }

    fn entry_dir(&self, package: &str, version: &semver::Version) -> PathBuf {
        self.root.join(package).join(version.to_string())
    }

    /// Read a cached package if present and integrity-valid. Returns `None`
    /// on any miss (absent, unreadable, malformed meta, or hash mismatch).
    fn read_cached(&self, package: &str, version: &semver::Version) -> Option<FetchedPackage> {
        let dir = self.entry_dir(package, version);
        let tarball_bytes = std::fs::read(dir.join("package.tar.gz")).ok()?;
        let meta = std::fs::read_to_string(dir.join("meta")).ok()?;
        let mut lines = meta.lines();
        let content_hash = lines.next()?.to_string();
        let mirror_url = lines.next()?.to_string();
        let upstream_url = lines.next().unwrap_or("").to_string();

        // Integrity: the cached bytes must still match the stored digest.
        let computed = format!("blake3:{}", blake3::hash(&tarball_bytes).to_hex());
        if computed != content_hash {
            return None;
        }
        Some(FetchedPackage {
            package: package.to_string(),
            version: version.clone(),
            upstream_url,
            mirror_url,
            tarball_bytes,
            content_hash,
        })
    }

    /// Best-effort write of a freshly fetched package into the cache. Any
    /// I/O error is swallowed — caching must never break a fetch.
    fn write_cached(&self, pkg: &FetchedPackage) {
        let dir = self.entry_dir(&pkg.package, &pkg.version);
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        let meta = format!(
            "{}\n{}\n{}\n",
            pkg.content_hash, pkg.mirror_url, pkg.upstream_url
        );
        let _ = std::fs::write(dir.join("package.tar.gz"), &pkg.tarball_bytes);
        let _ = std::fs::write(dir.join("meta"), meta);
    }
}

impl ProxyClient for CachingProxyClient {
    fn fetch_catalog(&self, package: &str) -> Result<FetchedManifest, ProxyClientError> {
        // Catalogs are mutable metadata — always fetch fresh.
        self.inner.fetch_catalog(package)
    }

    fn fetch_package(
        &self,
        package: &str,
        version: &semver::Version,
    ) -> Result<FetchedPackage, ProxyClientError> {
        if let Some(hit) = self.read_cached(package, version) {
            return Ok(hit);
        }
        let pkg = self.inner.fetch_package(package, version)?;
        self.write_cached(&pkg);
        Ok(pkg)
    }
}

/// Bounded exponential-backoff retry policy for the [`RetryingProxyClient`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Number of *retries* after the initial attempt (so total attempts =
    /// `max_retries + 1`). Zero disables retrying.
    pub max_retries: u32,
    /// Delay before the first retry; doubles for each subsequent one
    /// (`base_delay`, `2·base_delay`, `4·base_delay`, …). `Duration::ZERO`
    /// retries with no wait — used by tests to stay fast.
    pub base_delay: Duration,
}

impl Default for RetryPolicy {
    /// Three retries starting at 200 ms (→ 200 ms, 400 ms, 800 ms): enough
    /// to ride out a transient blip without stalling a build for long.
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay: Duration::from_millis(200),
        }
    }
}

/// A [`ProxyClient`] decorator that retries **transport failures** with
/// bounded exponential backoff (registry-proxy follow-up (h)), so a
/// transient network blip doesn't fail a build.
///
/// Only [`ProxyClientError::Unreachable`] is retried — it is the one
/// non-deterministic outcome. `PackageNotFound` / `VersionNotFound` /
/// `MalformedResponse` are deterministic answers from a reachable proxy, so
/// retrying them would only waste time; they propagate immediately. Wrap
/// the live client with this, typically inside a [`CachingProxyClient`] so
/// a cache hit skips the network (and the retries) entirely:
/// `CachingProxyClient::new(Box::new(RetryingProxyClient::new(inner, policy)), root)`.
pub struct RetryingProxyClient {
    inner: Box<dyn ProxyClient>,
    policy: RetryPolicy,
}

impl RetryingProxyClient {
    /// Wrap `inner`, retrying transport failures per `policy`.
    pub fn new(inner: Box<dyn ProxyClient>, policy: RetryPolicy) -> Self {
        Self { inner, policy }
    }

    /// Run `op`, retrying while it returns `Unreachable` and attempts remain.
    fn with_retries<T>(
        &self,
        mut op: impl FnMut() -> Result<T, ProxyClientError>,
    ) -> Result<T, ProxyClientError> {
        let mut attempt: u32 = 0;
        loop {
            match op() {
                Ok(value) => return Ok(value),
                Err(err) => {
                    let retryable = matches!(err, ProxyClientError::Unreachable { .. });
                    if !retryable || attempt >= self.policy.max_retries {
                        return Err(err);
                    }
                    // Exponential backoff: base · 2^attempt, saturating so a
                    // large `max_retries` can't overflow.
                    let factor = 2u32.checked_pow(attempt).unwrap_or(u32::MAX);
                    let delay = self.policy.base_delay.saturating_mul(factor);
                    if !delay.is_zero() {
                        std::thread::sleep(delay);
                    }
                    attempt += 1;
                }
            }
        }
    }
}

impl ProxyClient for RetryingProxyClient {
    fn fetch_catalog(&self, package: &str) -> Result<FetchedManifest, ProxyClientError> {
        self.with_retries(|| self.inner.fetch_catalog(package))
    }

    fn fetch_package(
        &self,
        package: &str,
        version: &semver::Version,
    ) -> Result<FetchedPackage, ProxyClientError> {
        self.with_retries(|| self.inner.fetch_package(package, version))
    }
}

// ── Registry fetch orchestration ────────────────────────────────

/// Failure resolving a registry dependency to concrete bytes.
#[derive(Debug)]
pub enum RegistryFetchError {
    /// A catalog or tarball fetch failed at the proxy layer.
    Proxy(ProxyClientError),
    /// The catalog was fetched but no published version satisfies the
    /// requested constraint. `available` is the catalog's version list, for
    /// a "found X.Y.Z, none match `req`" diagnostic.
    NoMatchingVersion {
        name: String,
        req: semver::VersionReq,
        available: Vec<semver::Version>,
    },
}

impl RegistryFetchError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Proxy(e) => e.code(),
            Self::NoMatchingVersion { .. } => "E_REGISTRY_NO_MATCHING_VERSION",
        }
    }
}

impl std::fmt::Display for RegistryFetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Proxy(e) => write!(f, "{e}"),
            Self::NoMatchingVersion {
                name,
                req,
                available,
            } => {
                let versions = available
                    .iter()
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                write!(
                    f,
                    "no version of `{name}` matches `{req}` (available: {})",
                    if versions.is_empty() {
                        "none".to_string()
                    } else {
                        versions
                    },
                )
            }
        }
    }
}

impl std::error::Error for RegistryFetchError {}

/// Pick the **highest** available version satisfying `req`. Uses the semver
/// crate's Cargo-compatible matching, so pre-releases are excluded unless
/// `req` explicitly opts into the same base version. `None` when nothing
/// matches.
pub fn select_version(
    req: &semver::VersionReq,
    available: &[semver::Version],
) -> Option<semver::Version> {
    available.iter().filter(|v| req.matches(v)).max().cloned()
}

/// Resolve one registry dependency to a concrete [`FetchedPackage`]: fetch
/// the catalog, pick the highest version satisfying `req`, then fetch that
/// version's tarball. This is the atomic "registry dep → bytes" step the
/// resolver's fetch path builds on; it composes over any [`ProxyClient`]
/// (typically a `CachingProxyClient` wrapping a `RetryingProxyClient`).
///
/// The tarball endpoint does not carry the upstream source URL, so this
/// stitches it in from the catalog manifest — closing the gap where a
/// bare `fetch_package` leaves `FetchedPackage.upstream_url` empty.
pub fn fetch_registry_package(
    client: &dyn ProxyClient,
    name: &str,
    req: &semver::VersionReq,
) -> Result<FetchedPackage, RegistryFetchError> {
    let manifest = client
        .fetch_catalog(name)
        .map_err(RegistryFetchError::Proxy)?;
    let version = select_version(req, &manifest.versions).ok_or_else(|| {
        RegistryFetchError::NoMatchingVersion {
            name: name.to_string(),
            req: req.clone(),
            available: manifest.versions.clone(),
        }
    })?;
    let mut pkg = client
        .fetch_package(name, &version)
        .map_err(RegistryFetchError::Proxy)?;
    if pkg.upstream_url.is_empty() {
        pkg.upstream_url = manifest.upstream_url;
    }
    Ok(pkg)
}

/// `ureq`-backed proxy client. Performs real HTTPS GETs against the two
/// registry-proxy endpoints (see `docs/registry-proxy-protocol.md`):
///
/// - `GET <url>/catalog/<name>` → `{ "upstream": "...", "versions": [...] }`
/// - `GET <url>/pkg/<name>/<version>.tar.gz` → the tarball, with a
///   `Karac-Content-Hash: blake3:<hex>` header the client verifies against
///   the body it received.
///
/// Transport failures map to [`ProxyClientError::Unreachable`], `404`s to
/// `PackageNotFound` / `VersionNotFound`, and any other non-2xx status or
/// malformed payload to `MalformedResponse`.
#[cfg(not(target_arch = "wasm32"))]
pub struct HttpProxyClient {
    pub url: String,
    /// Bearer token sent as `Authorization: Bearer <token>` on every
    /// request when present (registry-proxy follow-up (e)). `None` queries
    /// the proxy unauthenticated.
    pub token: Option<String>,
}

#[cfg(not(target_arch = "wasm32"))]
impl HttpProxyClient {
    /// Construct an unauthenticated client for `url`.
    pub fn new(url: String) -> Self {
        Self { url, token: None }
    }

    /// Construct a client that authenticates each request with `token`
    /// (when `Some`). This is the constructor `make_client` uses so a
    /// `KARAC_REGISTRY_TOKEN`-configured proxy is reached authenticated.
    pub fn with_token(url: String, token: Option<String>) -> Self {
        Self { url, token }
    }

    /// The configured base URL with any trailing slash removed, so the
    /// endpoint paths join cleanly.
    fn base(&self) -> &str {
        self.url.trim_end_matches('/')
    }

    /// Build a GET request for `url`, attaching the bearer token when one is
    /// configured. Centralizes header injection so both endpoints stay in
    /// sync.
    fn authed_get(&self, url: &str) -> ureq::Request {
        let req = ureq::get(url);
        match &self.token {
            Some(token) => req.set("Authorization", &format!("Bearer {token}")),
            None => req,
        }
    }
}

/// Parse the catalog JSON envelope into a [`FetchedManifest`]. Any missing
/// field, wrong type, or unparseable version string surfaces as
/// [`ProxyClientError::MalformedResponse`].
#[cfg(not(target_arch = "wasm32"))]
fn parse_catalog(
    url: &str,
    package: &str,
    body: &str,
) -> Result<FetchedManifest, ProxyClientError> {
    let malformed = |message: String| ProxyClientError::MalformedResponse {
        url: url.to_string(),
        message,
    };

    let json: serde_json::Value =
        serde_json::from_str(body).map_err(|e| malformed(format!("invalid JSON: {e}")))?;
    let upstream = json
        .get("upstream")
        .and_then(|v| v.as_str())
        .ok_or_else(|| malformed("missing string field \"upstream\"".to_string()))?;
    let versions_json = json
        .get("versions")
        .and_then(|v| v.as_array())
        .ok_or_else(|| malformed("missing array field \"versions\"".to_string()))?;

    let mut versions = Vec::with_capacity(versions_json.len());
    for entry in versions_json {
        let s = entry
            .as_str()
            .ok_or_else(|| malformed("\"versions\" entries must be strings".to_string()))?;
        let parsed = semver::Version::parse(s)
            .map_err(|e| malformed(format!("invalid semver version {s:?}: {e}")))?;
        versions.push(parsed);
    }

    Ok(FetchedManifest {
        package: package.to_string(),
        upstream_url: upstream.to_string(),
        versions,
    })
}

#[cfg(not(target_arch = "wasm32"))]
impl ProxyClient for HttpProxyClient {
    fn fetch_catalog(&self, package: &str) -> Result<FetchedManifest, ProxyClientError> {
        let url = format!("{}/catalog/{}", self.base(), package);
        let response = match self.authed_get(&url).call() {
            Ok(r) => r,
            Err(ureq::Error::Status(404, _)) => {
                return Err(ProxyClientError::PackageNotFound {
                    name: package.to_string(),
                })
            }
            Err(ureq::Error::Status(status @ (401 | 403), _)) => {
                return Err(ProxyClientError::Unauthorized { url, status })
            }
            Err(ureq::Error::Status(code, _)) => {
                return Err(ProxyClientError::MalformedResponse {
                    url,
                    message: format!("proxy returned unexpected status {code}"),
                })
            }
            Err(ureq::Error::Transport(t)) => {
                return Err(ProxyClientError::Unreachable {
                    url,
                    message: t.to_string(),
                })
            }
        };
        let body = response
            .into_string()
            .map_err(|e| ProxyClientError::MalformedResponse {
                url: url.clone(),
                message: format!("could not read response body: {e}"),
            })?;
        parse_catalog(&url, package, &body)
    }

    fn fetch_package(
        &self,
        package: &str,
        version: &semver::Version,
    ) -> Result<FetchedPackage, ProxyClientError> {
        use std::io::Read;

        let url = format!("{}/pkg/{}/{}.tar.gz", self.base(), package, version);
        let response = match self.authed_get(&url).call() {
            Ok(r) => r,
            Err(ureq::Error::Status(404, _)) => {
                return Err(ProxyClientError::VersionNotFound {
                    name: package.to_string(),
                    version: version.clone(),
                })
            }
            Err(ureq::Error::Status(status @ (401 | 403), _)) => {
                return Err(ProxyClientError::Unauthorized { url, status })
            }
            Err(ureq::Error::Status(code, _)) => {
                return Err(ProxyClientError::MalformedResponse {
                    url,
                    message: format!("proxy returned unexpected status {code}"),
                })
            }
            Err(ureq::Error::Transport(t)) => {
                return Err(ProxyClientError::Unreachable {
                    url,
                    message: t.to_string(),
                })
            }
        };

        let advertised_hash = response.header("Karac-Content-Hash").map(str::to_string);

        let mut tarball_bytes = Vec::new();
        response
            .into_reader()
            .read_to_end(&mut tarball_bytes)
            .map_err(|e| ProxyClientError::MalformedResponse {
                url: url.clone(),
                message: format!("could not read tarball body: {e}"),
            })?;

        // Integrity check: the body we received must match the digest the
        // proxy advertised. A mismatch means a corrupted or tampered
        // transfer, so refuse it rather than cache a bad tarball.
        let computed_hash = format!("blake3:{}", blake3::hash(&tarball_bytes).to_hex());
        if let Some(advertised) = &advertised_hash {
            if advertised != &computed_hash {
                return Err(ProxyClientError::MalformedResponse {
                    url,
                    message: format!(
                        "content-hash mismatch: proxy advertised {advertised}, \
                         computed {computed_hash}"
                    ),
                });
            }
        }

        Ok(FetchedPackage {
            package: package.to_string(),
            version: version.clone(),
            // The tarball endpoint does not carry the upstream source URL;
            // it is a package-level attribute delivered by `fetch_catalog`
            // (`FetchedManifest.upstream_url`). The resolver stitches it
            // into `kara.lock` from the manifest.
            upstream_url: String::new(),
            mirror_url: url,
            tarball_bytes,
            content_hash: advertised_hash.unwrap_or(computed_hash),
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

    /// Token-aware constructor mirroring the native client's signature so
    /// `make_client` is cfg-agnostic. The wasm stub performs no HTTP, so the
    /// token is discarded.
    pub fn with_token(url: String, _token: Option<String>) -> Self {
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
    fn resolve_reads_token_from_env() {
        let _g = PROXY_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var(REGISTRY_TOKEN_ENV_VAR, "  tok-123  ");
        let c = ProxyConfig::resolve(ProxyMode::Default, None);
        // Trimmed, non-empty → carried on the config.
        assert_eq!(c.token.as_deref(), Some("tok-123"));
        std::env::remove_var(REGISTRY_TOKEN_ENV_VAR);
    }

    #[test]
    fn resolve_token_absent_when_env_unset() {
        let _g = PROXY_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var(REGISTRY_TOKEN_ENV_VAR);
        let c = ProxyConfig::resolve(ProxyMode::Default, None);
        assert_eq!(c.token, None);
    }

    #[test]
    fn resolve_ignores_whitespace_only_token() {
        let _g = PROXY_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var(REGISTRY_TOKEN_ENV_VAR, "   ");
        let c = ProxyConfig::resolve(ProxyMode::Default, None);
        assert_eq!(
            c.token, None,
            "whitespace-only token must be treated as absent"
        );
        std::env::remove_var(REGISTRY_TOKEN_ENV_VAR);
    }

    #[test]
    fn http_client_constructors_thread_token() {
        assert_eq!(HttpProxyClient::new("http://x".to_string()).token, None);
        assert_eq!(
            HttpProxyClient::with_token("http://x".to_string(), Some("t".to_string())).token,
            Some("t".to_string()),
        );
    }

    #[test]
    fn resolve_env_beats_manifest_and_default() {
        let _g = PROXY_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var(PROXY_URL_ENV_VAR, "https://env.example");
        let c = ProxyConfig::resolve(ProxyMode::Default, Some("https://manifest.example"));
        assert_eq!(c.url, "https://env.example");
        std::env::remove_var(PROXY_URL_ENV_VAR);
    }

    #[test]
    fn resolve_manifest_beats_default_when_env_unset() {
        let _g = PROXY_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var(PROXY_URL_ENV_VAR);
        let c = ProxyConfig::resolve(ProxyMode::Default, Some("https://manifest.example"));
        assert_eq!(c.url, "https://manifest.example");
    }

    #[test]
    fn resolve_falls_back_to_default_when_env_and_manifest_absent() {
        let _g = PROXY_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var(PROXY_URL_ENV_VAR);
        let c = ProxyConfig::resolve(ProxyMode::Default, None);
        assert_eq!(c.url, DEFAULT_PROXY_URL);
    }

    #[test]
    fn resolve_ignores_whitespace_only_env_and_manifest() {
        let _g = PROXY_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Whitespace env is ignored; a whitespace manifest value is too, so
        // both fall through to the default.
        std::env::set_var(PROXY_URL_ENV_VAR, "  ");
        let c = ProxyConfig::resolve(ProxyMode::Default, Some("   "));
        assert_eq!(c.url, DEFAULT_PROXY_URL);
        std::env::remove_var(PROXY_URL_ENV_VAR);
    }

    #[test]
    fn resolve_preserves_mode() {
        let _g = PROXY_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var(PROXY_URL_ENV_VAR);
        let c = ProxyConfig::resolve(ProxyMode::Disabled, Some("https://manifest.example"));
        assert_eq!(c.mode, ProxyMode::Disabled);
        assert_eq!(c.url, "https://manifest.example");
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
    #[cfg(not(target_arch = "wasm32"))]
    fn make_client_selects_http_when_mode_default() {
        // Mode `Default` must route to the live `HttpProxyClient`, not the
        // `Disabled` one. Point it at a guaranteed-dead local address so
        // the real fetch fails fast with a transport error rather than
        // touching the network: `E_PROXY_UNREACHABLE` (not
        // `E_PROXY_DISABLED`) proves we got the HTTP client.
        let config = ProxyConfig {
            url: "http://127.0.0.1:1".to_string(),
            mode: ProxyMode::Default,
            token: None,
        };
        let client = make_client(&config);
        let err = client.fetch_catalog("anything").unwrap_err();
        assert_eq!(err.code(), "E_PROXY_UNREACHABLE");
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

    // ── CachingProxyClient ──────────────────────────────────────

    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use std::sync::Arc;

    static CACHE_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn temp_cache_root() -> std::path::PathBuf {
        let n = CACHE_COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
        let dir = std::env::temp_dir().join(format!("kara-regcache-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Test `ProxyClient` that counts calls and serves one canned package.
    struct CountingClient {
        inner: MemProxyClient,
        package_calls: Arc<AtomicUsize>,
        catalog_calls: Arc<AtomicUsize>,
    }

    impl ProxyClient for CountingClient {
        fn fetch_catalog(&self, package: &str) -> Result<FetchedManifest, ProxyClientError> {
            self.catalog_calls.fetch_add(1, AtomicOrdering::Relaxed);
            self.inner.fetch_catalog(package)
        }
        fn fetch_package(
            &self,
            package: &str,
            version: &semver::Version,
        ) -> Result<FetchedPackage, ProxyClientError> {
            self.package_calls.fetch_add(1, AtomicOrdering::Relaxed);
            self.inner.fetch_package(package, version)
        }
    }

    /// Build a counting client serving one package whose `content_hash` is
    /// the real digest of its bytes (so the cache's integrity check passes).
    fn counting_client_with_pkg() -> (CountingClient, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let bytes = b"a distinctive tarball payload for the digest".to_vec();
        let content_hash = format!("blake3:{}", blake3::hash(&bytes).to_hex());
        let mut mem = MemProxyClient::new();
        mem.insert_package(FetchedPackage {
            package: "mylib".to_string(),
            version: v("1.0.0"),
            upstream_url: "https://up.example/mylib".to_string(),
            mirror_url: "https://proxy.example/pkg/mylib/1.0.0.tar.gz".to_string(),
            tarball_bytes: bytes,
            content_hash,
        });
        mem.insert_catalog("mylib", "https://up.example/mylib", vec![v("1.0.0")]);
        let package_calls = Arc::new(AtomicUsize::new(0));
        let catalog_calls = Arc::new(AtomicUsize::new(0));
        let client = CountingClient {
            inner: mem,
            package_calls: Arc::clone(&package_calls),
            catalog_calls: Arc::clone(&catalog_calls),
        };
        (client, package_calls, catalog_calls)
    }

    #[test]
    fn cache_miss_fetches_then_hit_serves_without_refetch() {
        let (client, pkg_calls, _) = counting_client_with_pkg();
        let cache = CachingProxyClient::new(Box::new(client), temp_cache_root());
        let ver = v("1.0.0");

        let first = cache.fetch_package("mylib", &ver).expect("miss");
        let second = cache.fetch_package("mylib", &ver).expect("hit");

        // Inner reached exactly once — the second call is served from disk.
        assert_eq!(pkg_calls.load(AtomicOrdering::Relaxed), 1);
        assert_eq!(first.tarball_bytes, second.tarball_bytes);
        assert_eq!(first.content_hash, second.content_hash);
        assert_eq!(first.mirror_url, second.mirror_url);
        // Upstream URL round-trips through the meta file.
        assert_eq!(second.upstream_url, "https://up.example/mylib");
    }

    #[test]
    fn corrupted_cache_entry_is_refetched() {
        let (client, pkg_calls, _) = counting_client_with_pkg();
        let root = temp_cache_root();
        let cache = CachingProxyClient::new(Box::new(client), &root);
        let ver = v("1.0.0");

        cache
            .fetch_package("mylib", &ver)
            .expect("miss populates cache");
        // Corrupt the cached tarball so its hash no longer matches the meta.
        let tarball = root.join("mylib").join("1.0.0").join("package.tar.gz");
        std::fs::write(&tarball, b"corrupted").unwrap();

        cache.fetch_package("mylib", &ver).expect("refetch");
        // Miss on the corrupted entry → inner reached a second time.
        assert_eq!(pkg_calls.load(AtomicOrdering::Relaxed), 2);
    }

    #[test]
    fn catalog_is_never_cached() {
        let (client, _, cat_calls) = counting_client_with_pkg();
        let cache = CachingProxyClient::new(Box::new(client), temp_cache_root());

        cache.fetch_catalog("mylib").expect("first");
        cache.fetch_catalog("mylib").expect("second");
        // Both calls reach the inner client — catalogs are mutable metadata.
        assert_eq!(cat_calls.load(AtomicOrdering::Relaxed), 2);
    }

    #[test]
    fn missing_package_error_is_not_cached() {
        let (client, pkg_calls, _) = counting_client_with_pkg();
        let cache = CachingProxyClient::new(Box::new(client), temp_cache_root());
        let ver = v("9.9.9"); // not in the canned inner

        assert!(cache.fetch_package("mylib", &ver).is_err());
        assert!(cache.fetch_package("mylib", &ver).is_err());
        // Errors are never cached, so both attempts hit the inner client.
        assert_eq!(pkg_calls.load(AtomicOrdering::Relaxed), 2);
    }

    #[test]
    fn cache_root_honors_env_override() {
        let _g = PROXY_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var(REGISTRY_CACHE_ROOT_ENV_VAR, "/tmp/kara-custom-cache");
        assert_eq!(
            default_registry_cache_root(),
            Some(std::path::PathBuf::from("/tmp/kara-custom-cache"))
        );
        std::env::remove_var(REGISTRY_CACHE_ROOT_ENV_VAR);
    }

    // ── RetryingProxyClient ─────────────────────────────────────

    /// Test client that returns `Unreachable` for its first `fail_first`
    /// calls, then succeeds. Counts total calls so a test can assert how
    /// many attempts the retry wrapper made.
    struct FlakyClient {
        calls: Arc<AtomicUsize>,
        fail_first: usize,
        /// When true, fail with a non-retryable `PackageNotFound` instead of
        /// the retryable `Unreachable`, to prove non-transport errors don't
        /// retry.
        fail_non_retryable: bool,
    }

    impl FlakyClient {
        fn err(&self) -> ProxyClientError {
            if self.fail_non_retryable {
                ProxyClientError::PackageNotFound {
                    name: "x".to_string(),
                }
            } else {
                ProxyClientError::Unreachable {
                    url: "x".to_string(),
                    message: "flaky".to_string(),
                }
            }
        }
    }

    impl ProxyClient for FlakyClient {
        fn fetch_catalog(&self, package: &str) -> Result<FetchedManifest, ProxyClientError> {
            let n = self.calls.fetch_add(1, AtomicOrdering::Relaxed);
            if n < self.fail_first {
                return Err(self.err());
            }
            Ok(FetchedManifest {
                package: package.to_string(),
                upstream_url: "u".to_string(),
                versions: vec![v("1.0.0")],
            })
        }
        fn fetch_package(
            &self,
            package: &str,
            version: &semver::Version,
        ) -> Result<FetchedPackage, ProxyClientError> {
            let n = self.calls.fetch_add(1, AtomicOrdering::Relaxed);
            if n < self.fail_first {
                return Err(self.err());
            }
            Ok(FetchedPackage {
                package: package.to_string(),
                version: version.clone(),
                upstream_url: String::new(),
                mirror_url: "m".to_string(),
                tarball_bytes: vec![1, 2, 3],
                content_hash: "blake3:x".to_string(),
            })
        }
    }

    /// Zero-delay policy so retry tests never actually sleep.
    fn instant_policy(max_retries: u32) -> RetryPolicy {
        RetryPolicy {
            max_retries,
            base_delay: Duration::ZERO,
        }
    }

    fn flaky(calls: &Arc<AtomicUsize>, fail_first: usize, non_retryable: bool) -> FlakyClient {
        FlakyClient {
            calls: Arc::clone(calls),
            fail_first,
            fail_non_retryable: non_retryable,
        }
    }

    #[test]
    fn retries_transient_unreachable_then_succeeds() {
        let calls = Arc::new(AtomicUsize::new(0));
        // Fails twice (Unreachable), succeeds on the third attempt.
        let client = RetryingProxyClient::new(Box::new(flaky(&calls, 2, false)), instant_policy(3));
        let manifest = client
            .fetch_catalog("http")
            .expect("should succeed after retries");
        assert_eq!(manifest.package, "http");
        assert_eq!(calls.load(AtomicOrdering::Relaxed), 3); // 1 initial + 2 retries
    }

    #[test]
    fn gives_up_after_max_retries() {
        let calls = Arc::new(AtomicUsize::new(0));
        // Always Unreachable; 2 retries → 3 total attempts, then Err.
        let client = RetryingProxyClient::new(
            Box::new(flaky(&calls, usize::MAX, false)),
            instant_policy(2),
        );
        let err = client.fetch_catalog("http").unwrap_err();
        assert_eq!(err.code(), "E_PROXY_UNREACHABLE");
        assert_eq!(calls.load(AtomicOrdering::Relaxed), 3);
    }

    #[test]
    fn does_not_retry_non_transport_errors() {
        let calls = Arc::new(AtomicUsize::new(0));
        // PackageNotFound is deterministic — must not be retried.
        let client =
            RetryingProxyClient::new(Box::new(flaky(&calls, usize::MAX, true)), instant_policy(3));
        let err = client.fetch_catalog("http").unwrap_err();
        assert_eq!(err.code(), "E_PROXY_PACKAGE_NOT_FOUND");
        assert_eq!(calls.load(AtomicOrdering::Relaxed), 1); // no retries
    }

    #[test]
    fn max_retries_zero_makes_a_single_attempt() {
        let calls = Arc::new(AtomicUsize::new(0));
        let client = RetryingProxyClient::new(
            Box::new(flaky(&calls, usize::MAX, false)),
            instant_policy(0),
        );
        assert!(client.fetch_catalog("http").is_err());
        assert_eq!(calls.load(AtomicOrdering::Relaxed), 1);
    }

    #[test]
    fn retry_applies_to_fetch_package_too() {
        let calls = Arc::new(AtomicUsize::new(0));
        let client = RetryingProxyClient::new(Box::new(flaky(&calls, 1, false)), instant_policy(3));
        let pkg = client
            .fetch_package("http", &v("1.0.0"))
            .expect("should succeed after one retry");
        assert_eq!(pkg.package, "http");
        assert_eq!(calls.load(AtomicOrdering::Relaxed), 2); // 1 initial + 1 retry
    }

    #[test]
    fn default_policy_is_three_retries() {
        let p = RetryPolicy::default();
        assert_eq!(p.max_retries, 3);
        assert_eq!(p.base_delay, Duration::from_millis(200));
    }

    // ── select_version / fetch_registry_package ─────────────────

    fn req(s: &str) -> semver::VersionReq {
        semver::VersionReq::parse(s).unwrap()
    }

    #[test]
    fn select_version_picks_highest_match() {
        let avail = vec![v("1.0.0"), v("1.2.0"), v("1.9.0"), v("2.0.0")];
        // Caret excludes the 2.x major, so the highest 1.x wins.
        assert_eq!(select_version(&req("^1.0"), &avail), Some(v("1.9.0")));
        assert_eq!(select_version(&req("=1.2.0"), &avail), Some(v("1.2.0")));
        assert_eq!(select_version(&req(">=2.0"), &avail), Some(v("2.0.0")));
    }

    #[test]
    fn select_version_returns_none_when_nothing_matches() {
        let avail = vec![v("1.0.0"), v("1.2.0")];
        assert_eq!(select_version(&req("^3"), &avail), None);
    }

    #[test]
    fn select_version_excludes_prerelease_unless_opted_in() {
        let avail = vec![v("1.0.0-rc.1")];
        // A plain caret must not select a pre-release (Cargo semantics).
        assert_eq!(select_version(&req("^1.0"), &avail), None);
        // An explicit pre-release comparator on the same base does match.
        assert_eq!(
            select_version(&req(">=1.0.0-rc.1"), &avail),
            Some(v("1.0.0-rc.1"))
        );
    }

    fn pkg_with_empty_upstream(version: &str, bytes: &[u8]) -> FetchedPackage {
        FetchedPackage {
            package: "http".to_string(),
            version: v(version),
            upstream_url: String::new(),
            mirror_url: format!("https://proxy.example/pkg/http/{version}.tar.gz"),
            tarball_bytes: bytes.to_vec(),
            content_hash: format!("blake3:{}", blake3::hash(bytes).to_hex()),
        }
    }

    #[test]
    fn fetch_registry_package_selects_and_stitches_upstream() {
        let mut mem = MemProxyClient::new();
        mem.insert_catalog(
            "http",
            "https://github.com/kara/http",
            vec![v("1.0.0"), v("1.2.0"), v("1.9.0"), v("2.0.0")],
        );
        // Provide the tarball for the version `^1.0` will select (1.9.0).
        mem.insert_package(pkg_with_empty_upstream("1.9.0", b"tarball-1.9.0"));

        let pkg = fetch_registry_package(&mem, "http", &req("^1.0")).expect("fetch");
        assert_eq!(pkg.version, v("1.9.0"));
        assert_eq!(pkg.tarball_bytes, b"tarball-1.9.0");
        // Upstream URL was empty on the tarball and stitched from the catalog.
        assert_eq!(pkg.upstream_url, "https://github.com/kara/http");
    }

    #[test]
    fn fetch_registry_package_no_matching_version() {
        let mut mem = MemProxyClient::new();
        mem.insert_catalog("http", "u", vec![v("1.0.0"), v("1.2.0")]);
        let err = fetch_registry_package(&mem, "http", &req("^3")).unwrap_err();
        assert_eq!(err.code(), "E_REGISTRY_NO_MATCHING_VERSION");
        assert!(matches!(err, RegistryFetchError::NoMatchingVersion { .. }));
    }

    #[test]
    fn fetch_registry_package_unknown_package_is_proxy_error() {
        let mem = MemProxyClient::new();
        let err = fetch_registry_package(&mem, "ghost", &req("^1")).unwrap_err();
        assert_eq!(err.code(), "E_PROXY_PACKAGE_NOT_FOUND");
    }
}
