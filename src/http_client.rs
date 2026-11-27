//! The HTTP client both backends run (docs/library/io.md § HTTP client):
//! the compiled program's runtime (`runtime/src/lib.rs`, by `#[path]`) and
//! the MIR interpreter (`src/mir/interp.rs` `http_fetch`). One client, so
//! the two cannot disagree on what a response is (B-2026-11-14-47,
//! B-2026-11-14-48, B-2026-11-14-49).
//!
//! A request is a one-shot hyper `client::conn` exchange on a fresh
//! connection, driven on the calling thread's private current-thread tokio
//! runtime:
//!
//!   - `https://`: TCP + tokio-rustls with ALPN `["h2", "http/1.1"]`; an
//!     `h2`-negotiating origin gets HTTP/2 (`hyper::client::conn::http2`),
//!     anything else HTTP/1.1. SNI + full webpki certificate/hostname
//!     verification against the bundled Mozilla roots;
//!     `KARAC_HTTP_EXTRA_CA=<pem-path>` appends private-CA roots.
//!   - `http://`: plain HTTP/1.1 (h2c prior-knowledge is a server-side
//!     affordance, not assumed of arbitrary origins).
//!
//! Redirects are followed up to 5 hops with the 301/302/303 POST→GET
//! demotion (307/308 keep method and body); a status outside 2xx, an
//! unfollowed 3xx included, is an `Err` that keeps the response, in the
//! message shape `"{url}: status code {n}"`, and a sixth redirect is one
//! that keeps that 3xx, as `"{asked-for url}: too many redirects"`
//! (B-2026-11-15-134, B-2026-11-15-135). A timeout bounds the whole
//! request; one below 1 ms is refused before connecting (B-2026-11-15-136).
//! A 1xx interim response is skipped. A body cut short (fewer bytes than
//! its `Content-Length`, a chunked body with no last chunk, a timeout
//! mid-body) is an `Err`, never a short `Ok`. Bodies are raw bytes: the
//! client asks for no compression and decodes none, so a `Content-Encoding`
//! the server sends anyway reaches the program as sent. Every request
//! carries `User-Agent: kara/<version>` and `Accept: */*` unless the
//! program sets its own.
//!
//! Proxy routing is `src/http_proxy.rs`'s, which both crates also share.

/// A response as the client received it, after any redirects.
pub(crate) struct HttpFetched {
    pub(crate) status: u16,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Vec<u8>,
    /// The URL that answered (the last hop's), which a status outside 2xx
    /// names in its error's message.
    pub(crate) url: String,
    /// The URL the program asked for, when this is the sixth redirect,
    /// which is not followed.
    pub(crate) too_many_redirects: Option<String>,
}

/// The error a final status outside 2xx is, or a sixth redirect
/// (docs/library/io.md § HTTP client: the `HttpError` keeps the response).
/// A 3xx the client did not follow (a 300, a 304, a redirect with no
/// `Location`) is outside 2xx too (B-2026-11-15-134).
pub(crate) fn http_status_error(fetched: &HttpFetched) -> Option<String> {
    if let Some(asked) = &fetched.too_many_redirects {
        return Some(format!("{asked}: too many redirects"));
    }
    (!(200..300).contains(&fetched.status))
        .then(|| format!("{}: status code {}", fetched.url, fetched.status))
}

/// The message for a request's `timeout` below 1 ms, which is refused
/// before connecting, as a socket's `set_read_timeout` refuses one
/// (B-2026-11-15-136).
pub(crate) fn http_timeout_refused(url: &str, timeout_ms: Option<i64>) -> Option<String> {
    timeout_ms
        .is_some_and(|ms| ms < 1)
        .then(|| format!("{url}: timeout must be at least 1 ms"))
}

/// Run `f` on this thread's private current-thread tokio runtime for the
/// synchronous client FFI. A dedicated runtime (rather than the serve
/// loops') keeps the client callable from anywhere: a plain thread, a serve
/// handler running under `block_in_place`, or a test harness — `block_on`
/// here never re-enters an outer runtime.
///
/// One per thread, not one per process. In a shared current-thread
/// runtime one of the threads blocked in `block_on` drives the IO driver
/// for all of them, touching sockets the others registered, and
/// ThreadSanitizer reports those accesses as races in
/// tokio's `ScheduledIo` (`apps/service/http_cache` on the TSan lane; a
/// tokio-only program sharing one runtime the same way reports them too,
/// and one runtime per thread reports none). Each thread here drives the
/// sockets it registers. The client opens a connection per request and
/// keeps no IO resource between calls, so nothing crosses runtimes. A
/// thread's runtime goes with the thread, shut down without blocking so
/// that a thread ending inside another runtime's context cannot panic.
fn with_http_client_runtime<R>(f: impl FnOnce(&tokio::runtime::Runtime) -> R) -> R {
    struct ClientRuntime(Option<tokio::runtime::Runtime>);
    impl Drop for ClientRuntime {
        fn drop(&mut self) {
            if let Some(rt) = self.0.take() {
                rt.shutdown_background();
            }
        }
    }
    thread_local! {
        static RT: ClientRuntime = ClientRuntime(Some(
            tokio::runtime::Builder::new_current_thread()
                .enable_io()
                .enable_time()
                .build()
                .expect("current-thread tokio runtime for the http client"),
        ));
    }
    RT.with(|rt| {
        f(rt.0
            .as_ref()
            .expect("the http client runtime is shut down only at thread exit"))
    })
}

/// A failed handshake's error as a sentence. rustls prints several
/// certificate failures by their Rust names (`UnknownIssuer`,
/// `BadSignature`, `Other(OtherError(EndEntityUsedAsCa))`, a signature
/// algorithm's OID bytes), which tell a program's user nothing; those are
/// spelled out here, and the ones rustls already writes as a sentence
/// (expiry, the name check, the key usage) keep its text
/// (B-2026-11-22-45).
fn tls_failure_text(e: &std::io::Error) -> String {
    let Some(err) = e.get_ref().and_then(|i| i.downcast_ref::<rustls::Error>()) else {
        return e.to_string();
    };
    use rustls::CertificateError as C;
    let cert = |why: &str| format!("invalid peer certificate: {why}");
    match err {
        rustls::Error::InvalidCertificate(ce) => match ce {
            C::UnknownIssuer => cert(
                "it does not chain to a trusted root (it is self-signed, its root is neither in \
                 the built-in Mozilla bundle nor in KARAC_HTTP_EXTRA_CA, or the server did not \
                 send its intermediate certificates)",
            ),
            C::BadSignature => cert(
                "a signature does not verify (a certificate's against its issuer's key, or the \
                 server's handshake signature against the certificate's key; an RSA key under \
                 2048 bits fails this way)",
            ),
            C::UnsupportedSignatureAlgorithmContext { .. }
            | C::UnsupportedSignatureAlgorithmForPublicKeyContext { .. } => cert(
                "it is signed with an algorithm this client does not accept (SHA-1 and MD5 \
                 signatures are refused)",
            ),
            C::ExpiredContext { time, not_after } => cert(&format!(
                "it expired at {} (the time now is {})",
                utc_text(not_after.as_secs()),
                utc_text(time.as_secs())
            )),
            C::NotValidYetContext { time, not_before } => cert(&format!(
                "it is not valid until {} (the time now is {})",
                utc_text(not_before.as_secs()),
                utc_text(time.as_secs())
            )),
            C::NotValidForNameContext {
                expected,
                presented,
            } => {
                // rustls writes each presented name as its Rust form
                // (`DnsName("a.test")`, `IpAddress(10.0.0.1)`).
                let names: Vec<String> = presented
                    .iter()
                    .map(|n| {
                        let unwrap = |p: &str| {
                            n.strip_prefix(p)
                                .and_then(|r| r.strip_suffix(')'))
                                .map(|r| r.trim_matches('"').to_string())
                        };
                        if let Some(d) = unwrap("DnsName(") {
                            format!("DNS name {d}")
                        } else if let Some(ip) = unwrap("IpAddress(") {
                            format!("IP address {ip}")
                        } else {
                            n.clone()
                        }
                    })
                    .collect();
                let host = expected.to_str();
                if names.is_empty() {
                    cert(&format!(
                        "it is not valid for \"{host}\": it names no host in its subjectAltName \
                         (a name only in the subject's CN does not count)"
                    ))
                } else {
                    cert(&format!(
                        "it is not valid for \"{host}\"; its subjectAltName lists only {}",
                        names.join(", ")
                    ))
                }
            }
            C::BadEncoding => cert("it is malformed"),
            C::UnhandledCriticalExtension => {
                cert("it has a critical extension this client does not understand")
            }
            C::Revoked => cert("it is revoked"),
            C::Other(other) => {
                let why = match other.to_string().as_str() {
                    "EndEntityUsedAsCa" => {
                        "a certificate that is not a CA issued another one in the chain".to_string()
                    }
                    "CaUsedAsEndEntity" => {
                        "the server presented a CA certificate as its own".to_string()
                    }
                    "PathLenConstraintViolated" => {
                        "the chain is longer than an issuer's path-length limit allows".to_string()
                    }
                    "NameConstraintViolation" => {
                        "the host is outside a name constraint of an issuer".to_string()
                    }
                    "MaximumPathDepthExceeded" | "MaximumPathBuildCallsExceeded" => {
                        "the chain is too long to verify".to_string()
                    }
                    "UnsupportedCertVersion" => "it is not an X.509 v3 certificate".to_string(),
                    "InvalidCertValidity" => "its validity period is malformed".to_string(),
                    name => format!("the chain does not verify ({name})"),
                };
                cert(&why)
            }
            _ => err.to_string(),
        },
        rustls::Error::AlertReceived(rustls::AlertDescription::ProtocolVersion)
        | rustls::Error::PeerIncompatible(
            rustls::PeerIncompatible::ServerDoesNotSupportTls12Or13,
        ) => "the server speaks no TLS version this client accepts (TLS 1.2 or 1.3)".to_string(),
        _ => err.to_string(),
    }
}

/// `secs` since the Unix epoch as `YYYY-MM-DD HH:MM:SS UTC`.
fn utc_text(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} UTC",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

/// A failed body read's error as a sentence. A close-delimited body over TLS
/// that ends without the server's `close_notify` may have been cut short by
/// whoever closed the connection, so it is an error; hyper reports it as
/// "error reading a body from connection", which does not say why.
fn body_failure_text(e: &hyper::Error) -> String {
    let mut src: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(e);
    while let Some(s) = src {
        if let Some(io) = s.downcast_ref::<std::io::Error>() {
            if io.kind() == std::io::ErrorKind::UnexpectedEof
                && io.to_string().contains("close_notify")
            {
                return "the connection closed without TLS close_notify, so the body may be \
                        cut short"
                    .to_string();
            }
        }
        src = s.source();
    }
    e.to_string()
}

/// Adds the PEM certificates in the file `path` (`KARAC_HTTP_EXTRA_CA`)
/// to `root_store`. A file that cannot be read, that holds a malformed
/// block, or that holds no certificate at all is an error.
fn add_extra_ca(root_store: &mut rustls::RootCertStore, path: &str) -> Result<(), String> {
    let pem = std::fs::read(path)
        .map_err(|e| format!("KARAC_HTTP_EXTRA_CA: cannot read '{path}': {e}"))?;
    let mut added = 0usize;
    for cert in rustls_pemfile::certs(&mut pem.as_slice()) {
        let cert = cert.map_err(|_| {
            format!(
                "KARAC_HTTP_EXTRA_CA: bad PEM in '{path}': a block is malformed (its body \
                 is not valid base64, or it has no END line)"
            )
        })?;
        root_store
            .add(cert)
            .map_err(|e| format!("KARAC_HTTP_EXTRA_CA: rejected cert in '{path}': {e}"))?;
        added += 1;
    }
    // A file with no certificate in it (an empty one, or a key written
    // where the CA was meant to go) would otherwise trust nothing
    // extra in silence, and every request to the private CA's hosts
    // would fail as an unknown issuer instead (B-2026-11-22-46).
    if added == 0 {
        return Err(format!(
            "KARAC_HTTP_EXTRA_CA: no certificate in '{path}' (it must hold one or more PEM `CERTIFICATE` blocks)"
        ));
    }
    Ok(())
}

/// Shared rustls `ClientConfig`: bundled Mozilla roots
/// (`webpki_roots::TLS_SERVER_ROOTS`, so stripped images need no system
/// CA bundle) + optional `KARAC_HTTP_EXTRA_CA` PEM append, `ring`
/// provider, ALPN `["h2", "http/1.1"]`. Built once; the env var is read
/// on first use.
pub(crate) fn http_client_tls_config() -> Result<std::sync::Arc<rustls::ClientConfig>, String> {
    static CFG: std::sync::OnceLock<Result<std::sync::Arc<rustls::ClientConfig>, String>> =
        std::sync::OnceLock::new();
    CFG.get_or_init(|| {
        let mut root_store = rustls::RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        if let Ok(path) = std::env::var("KARAC_HTTP_EXTRA_CA") {
            add_extra_ca(&mut root_store, &path)?;
        }
        let provider = std::sync::Arc::new(rustls::crypto::ring::default_provider());
        let mut config = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("rustls safe default protocol versions are always supported by ring")
            .with_root_certificates(root_store)
            .with_no_client_auth();
        config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        Ok(std::sync::Arc::new(config))
    })
    .clone()
}

/// Synchronous entry point: run the redirect-following fetch on the
/// client runtime, bounded by `timeout_ms` when the request set one. A
/// timeout below 1 ms is refused without connecting.
pub(crate) fn http_fetch_blocking(
    method: &str,
    url: &str,
    body: &[u8],
    headers: &[(String, String)],
    timeout_ms: Option<i64>,
) -> Result<HttpFetched, String> {
    if let Some(e) = http_timeout_refused(url, timeout_ms) {
        return Err(e);
    }
    with_http_client_runtime(|rt| {
        rt.block_on(async {
            let fut = http_fetch_redirects(method, url, body, headers);
            match timeout_ms {
                Some(ms) => {
                    match tokio::time::timeout(std::time::Duration::from_millis(ms as u64), fut)
                        .await
                    {
                        Ok(r) => r,
                        Err(_) => Err(format!("{url}: request timed out after {ms} ms")),
                    }
                }
                None => fut.await,
            }
        })
    })
}

/// Follow up to 5 redirects (a sixth comes back marked, not followed),
/// demoting to GET and dropping the body on
/// 301/302/303 for non-GET/HEAD methods (307/308 preserve both), and
/// dropping the caller's `Authorization` and `Cookie` headers once a hop
/// leaves the first request's origin. Each hop goes through the proxy its
/// own URL names.
async fn http_fetch_redirects(
    method: &str,
    url: &str,
    body: &[u8],
    headers: &[(String, String)],
) -> Result<HttpFetched, String> {
    let mut method = method.to_string();
    let mut url = url.to_string();
    let mut body = body.to_vec();
    let mut headers = headers.to_vec();
    let first = url.clone();
    for hop in 0..=5 {
        let mut fetched = http_fetch_once(&method, &url, &body, &headers).await?;
        if matches!(fetched.status, 301 | 302 | 303 | 307 | 308) {
            let loc = fetched
                .headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("location"))
                .map(|(_, v)| v.clone());
            // The sixth redirect is not followed: the `Err` keeps it
            // and names the URL asked for (B-2026-11-15-135).
            if loc.is_some() && hop == 5 {
                fetched.too_many_redirects = Some(first.clone());
                return Ok(fetched);
            }
            if let Some(loc) = loc {
                url = http_resolve_location(&url, &loc)?;
                let demote = fetched.status == 303
                    || ((fetched.status == 301 || fetched.status == 302)
                        && !method.eq_ignore_ascii_case("GET")
                        && !method.eq_ignore_ascii_case("HEAD"));
                if demote {
                    method = "GET".to_string();
                    body.clear();
                }
                // The caller's credentials go no further once a hop leaves
                // the first request's origin (docs/library/io.md).
                // A program's Host named the first origin's server too.
                if !crate::http_proxy::redirect_keeps_credentials(&first, &url) {
                    headers.retain(|(k, _)| {
                        !crate::http_proxy::is_credential_header(k)
                            && !k.eq_ignore_ascii_case("host")
                    });
                }
                continue;
            }
        }
        // A status outside 2xx is an `Err` that keeps the response, which
        // `http_deliver` makes of it (`http_status_error`).
        return Ok(fetched);
    }
    // Not reached: the sixth hop returns above.
    Err(format!("{url}: too many redirects"))
}

/// `url` with the empty path a URL may have before its query made `/`
/// (`http://h:1?x=1` is `http://h:1/?x=1`, RFC 3986 §3.3), which the URI
/// parser refuses otherwise (B-2026-11-17-56).
fn http_url_form(url: &str) -> std::borrow::Cow<'_, str> {
    if let Some(at) = url.find("://") {
        let rest = &url[at + 3..];
        if let Some(i) = rest.find(['/', '?', '#']) {
            if rest[i..].starts_with('?') {
                let cut = at + 3 + i;
                return format!("{}/{}", &url[..cut], &url[cut..]).into();
            }
        }
    }
    url.into()
}

/// Resolve a `Location` header against the current request URL, as RFC
/// 3986 §5.2 resolves a reference, dot segments removed (`src/http_proxy.rs`,
/// B-2026-11-14-46).
fn http_resolve_location(current: &str, loc: &str) -> Result<String, String> {
    Ok(crate::http_proxy::resolve_location(current, loc))
}

/// One request/response exchange on a fresh connection: TCP connect (to the
/// proxy the environment names, if any, through a CONNECT tunnel for https),
/// TLS + ALPN when `https`, then hyper h2 or h1 `client::conn` by the
/// negotiated protocol.
async fn http_fetch_once(
    method: &str,
    url: &str,
    body: &[u8],
    headers: &[(String, String)],
) -> Result<HttpFetched, String> {
    let uri: hyper::Uri = http_url_form(url)
        .parse()
        .map_err(|e| format!("{url}: bad url: {e}"))?;
    let scheme = uri.scheme_str().unwrap_or("http").to_string();
    // A URL is http:// or https:// (docs/library/io.md § HTTP client); any
    // other scheme is refused before a connection rather than spoken to in
    // HTTP (B-2026-11-07-28).
    if scheme != "http" && scheme != "https" {
        return Err(format!("{url}: Unknown Scheme: unknown scheme '{scheme}'"));
    }
    // An IPv6 literal's brackets belong to the URL and the Host header,
    // not to the address connected to or the TLS name (B-2026-11-17-56).
    let host = uri
        .host()
        .ok_or_else(|| format!("{url}: url has no host"))?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_string();
    // A port that does not fit 16 bits is no URL, not a default port.
    // The URI parser reads a port that does not fit 16 bits as none, so
    // the authority's own text decides.
    let written = uri
        .authority()
        .map(|a| a.as_str().rsplit('@').next().unwrap_or_default())
        .map(|a| a.rsplit(']').next().unwrap_or_default())
        .and_then(|a| a.rsplit_once(':'))
        .map(|(_, p)| p)
        .filter(|p| !p.is_empty());
    let port = match written {
        Some(p) => p
            .parse::<u16>()
            .map_err(|_| format!("{url}: bad url: invalid port {p}"))?,
        None if scheme == "https" => 443,
        None => 80,
    };
    let route = crate::http_proxy::proxy_for(&scheme, &host).map_err(|e| format!("{url}: {e}"))?;
    let tcp = match &route {
        None => tokio::net::TcpStream::connect((host.as_str(), port))
            .await
            .map_err(|e| format!("{url}: connect failed: {e}"))?,
        Some(r) => tokio::net::TcpStream::connect((r.host.as_str(), r.port))
            .await
            .map_err(|e| {
                format!(
                    "{url}: {}",
                    crate::http_proxy::proxy_unreachable(r, &e.to_string())
                )
            })?,
    };
    if scheme == "https" {
        let tcp = match &route {
            Some(r) => http_connect_tunnel(tcp, r, &host, port, url).await?,
            None => tcp,
        };
        let config = http_client_tls_config()?;
        let connector = tokio_rustls::TlsConnector::from(config);
        let sni = rustls::pki_types::ServerName::try_from(host.clone())
            .map_err(|e| format!("{url}: invalid TLS server name: {e}"))?;
        let tls = connector
            .connect(sni, tcp)
            .await
            .map_err(|e| format!("{url}: TLS handshake failed: {}", tls_failure_text(&e)))?;
        let negotiated_h2 = tls.get_ref().1.alpn_protocol() == Some(b"h2".as_slice());
        if negotiated_h2 {
            http_exchange_h2(tls, method, &uri, url, body, headers).await
        } else {
            http_exchange_h1(
                tls, method, &uri, &host, port, &scheme, url, body, headers, false,
            )
            .await
        }
    } else if let Some(r) = &route {
        // An http request goes to the proxy in absolute form, with the
        // proxy's credentials beside the caller's headers.
        let mut headers = headers.to_vec();
        if let Some(auth) = r.authorization() {
            headers.push(("Proxy-Authorization".to_string(), auth));
        }
        http_exchange_h1(
            tcp, method, &uri, &host, port, &scheme, url, body, &headers, true,
        )
        .await
    } else {
        http_exchange_h1(
            tcp, method, &uri, &host, port, &scheme, url, body, headers, false,
        )
        .await
    }
}

/// Opens a tunnel to `host:port` through the proxy `tcp` is connected to: the
/// CONNECT request, then the proxy's answer up to its blank line, read a byte
/// at a time so nothing of the tunnelled stream is consumed. A non-2xx answer
/// is an error carrying the proxy's status.
async fn http_connect_tunnel(
    mut tcp: tokio::net::TcpStream,
    route: &crate::http_proxy::ProxyRoute,
    host: &str,
    port: u16,
    url: &str,
) -> Result<tokio::net::TcpStream, String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let unreachable = |e: std::io::Error| {
        format!(
            "{url}: {}",
            crate::http_proxy::proxy_unreachable(route, &e.to_string())
        )
    };
    tcp.write_all(crate::http_proxy::connect_request(route, host, port).as_bytes())
        .await
        .map_err(unreachable)?;
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") && head.len() < 65536 {
        if tcp.read(&mut byte).await.map_err(unreachable)? == 0 {
            break;
        }
        head.push(byte[0]);
    }
    match crate::http_proxy::connect_refusal(&head) {
        Some(why) => Err(format!("{url}: {why}")),
        None => Ok(tcp),
    }
}

/// HTTP/1.1 exchange over any duplex IO (plain TCP or a TLS stream that
/// negotiated `http/1.1`). Origin-form request target (absolute-form when
/// `absolute`, for a request through a proxy) + explicit `Host` (with port
/// when non-default).
#[allow(clippy::too_many_arguments)]
async fn http_exchange_h1<T>(
    io: T,
    method: &str,
    uri: &hyper::Uri,
    host: &str,
    port: u16,
    scheme: &str,
    url: &str,
    body: &[u8],
    headers: &[(String, String)],
    absolute: bool,
) -> Result<HttpFetched, String>
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let read = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let io = CountingIo {
        inner: io,
        read: read.clone(),
    };
    let (mut sender, conn) = hyper::client::conn::http1::Builder::new()
        .allow_obsolete_multiline_headers_in_responses(true)
        .max_headers(MAX_RESPONSE_HEADERS)
        .max_buf_size(MAX_RESPONSE_HEAD)
        .handshake(hyper_util::rt::TokioIo::new(io))
        .await
        .map_err(|e| format!("{url}: HTTP/1.1 handshake failed: {e}"))?;
    let conn = tokio::task::spawn(async move {
        let _ = conn.await;
    });
    let default_port = if scheme == "https" { 443 } else { 80 };
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    let host_header = if port == default_port {
        host
    } else {
        format!("{host}:{port}")
    };
    let target = if absolute {
        uri.to_string()
    } else {
        uri.path_and_query()
            .map(|p| p.as_str().to_string())
            .unwrap_or_else(|| "/".to_string())
    };
    let fetched = async {
        let req = http_build_request(method, &target, Some(&host_header), body, headers, url)?;
        let resp = sender.send_request(req).await.map_err(|e| {
            if head_too_large(&e) {
                head_limit_text(url, read.load(std::sync::atomic::Ordering::Relaxed))
            } else {
                format!("{url}: request failed: {e}")
            }
        })?;
        http_collect_response(resp, url).await
    }
    .await;
    http_close_conn(sender, conn).await;
    fetched
}

/// The most header fields an HTTP/1.1 response head may carry, and the most
/// bytes the client buffers while it waits for the head to end: hyper's own
/// defaults (100 fields; 8 KiB plus 4 KiB per field), set here by name so
/// the limits the docs state are the ones the client runs with
/// (docs/library/io.md § HTTP client, B-2026-11-27-13).
const MAX_RESPONSE_HEADERS: usize = 100;
const MAX_RESPONSE_HEAD: usize = 8192 + 4096 * MAX_RESPONSE_HEADERS;

/// hyper reports both limits as one parse error, "message head is too
/// large" (`Parse::TooLarge`, for httparse's `TooManyHeaders` and for a
/// buffer that filled before the head ended). `is_parse_too_large` needs
/// hyper's `server` feature, which the compiler's own build does not turn
/// on, so the kind is read from the error's text.
fn head_too_large(e: &hyper::Error) -> bool {
    e.is_parse() && e.to_string().starts_with("message head is too large")
}

/// Which of the two limits a response head hit. A head that had not filled
/// the buffer when hyper gave up ran out of header slots: httparse refuses
/// the 101st field as soon as it is buffered, long before 408 KiB arrive.
fn head_limit_text(url: &str, bytes_read: usize) -> String {
    if bytes_read < MAX_RESPONSE_HEAD {
        format!("{url}: response head has more than {MAX_RESPONSE_HEADERS} header fields")
    } else {
        format!(
            "{url}: response head is larger than {} KiB",
            MAX_RESPONSE_HEAD / 1024
        )
    }
}

/// An HTTP/1.1 connection's transport, counting the bytes read from it, so
/// that a refused response head can say which limit it hit.
struct CountingIo<T> {
    inner: T,
    read: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl<T: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for CountingIo<T> {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let before = buf.filled().len();
        let polled = std::pin::Pin::new(&mut this.inner).poll_read(cx, buf);
        if let std::task::Poll::Ready(Ok(())) = polled {
            this.read.fetch_add(
                buf.filled().len() - before,
                std::sync::atomic::Ordering::Relaxed,
            );
        }
        polled
    }
}

impl<T: tokio::io::AsyncWrite + Unpin> tokio::io::AsyncWrite for CountingIo<T> {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }

    fn poll_write_vectored(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.get_mut().inner).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

/// Close a client connection before its request's `block_on` returns: drop
/// the request half, and drive the connection task until hyper shuts the
/// socket. The client runtime is per thread (`with_http_client_runtime`)
/// and runs only inside a request, so a connection task left behind would
/// hold its socket open until that thread's next request, perhaps never.
async fn http_close_conn<S>(sender: S, conn: tokio::task::JoinHandle<()>) {
    drop(sender);
    let _ = conn.await;
}

/// HTTP/2 exchange over a TLS stream that negotiated `h2` via ALPN. The
/// absolute URI carries `:scheme` / `:authority`; hyper's h2 client
/// conn maps it to pseudo-headers (no `Host` header on h2).
async fn http_exchange_h2<T>(
    io: T,
    method: &str,
    uri: &hyper::Uri,
    url: &str,
    body: &[u8],
    headers: &[(String, String)],
) -> Result<HttpFetched, String>
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (mut sender, conn) = hyper::client::conn::http2::handshake(
        hyper_util::rt::TokioExecutor::new(),
        hyper_util::rt::TokioIo::new(io),
    )
    .await
    .map_err(|e| format!("{url}: HTTP/2 handshake failed: {e}"))?;
    let conn = tokio::task::spawn(async move {
        let _ = conn.await;
    });
    let fetched = async {
        let req = http_build_request(method, &uri.to_string(), None, body, headers, url)?;
        let resp = sender
            .send_request(req)
            .await
            .map_err(|e| format!("{url}: request failed: {e}"))?;
        http_collect_response(resp, url).await
    }
    .await;
    http_close_conn(sender, conn).await;
    fetched
}

/// The `User-Agent` a request carries unless it sets its own.
const HTTP_USER_AGENT: &str = concat!("kara/", env!("CARGO_PKG_VERSION"));

/// Assemble the hyper request: method, target (origin-form for h1,
/// absolute for h2), optional `Host`, caller headers, the client's defaults, `Full` body.
fn http_build_request(
    method: &str,
    target: &str,
    host_header: Option<&str>,
    body: &[u8],
    headers: &[(String, String)],
    url: &str,
) -> Result<hyper::Request<http_body_util::Full<bytes::Bytes>>, String> {
    let method = hyper::Method::from_bytes(method.as_bytes())
        .map_err(|e| format!("{url}: bad method: {e}"))?;
    let mut builder = hyper::Request::builder().method(method).uri(target);
    // One Host: the program's own when it sets one (a virtual host
    // reached by address, `curl -H 'Host: ..'`), else the URL's. HTTP/2
    // has none (its `:authority` is the URL's).
    let own_host = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("host"))
        .map(|(_, v)| v.as_str());
    if let Some(host) = host_header {
        builder = builder.header(hyper::header::HOST, own_host.unwrap_or(host));
    }
    // The body's length is the client's to send: a program's own
    // `Content-Length` (one copied from a request it received, say) would
    // mis-frame the body; its `Host` is sent once, above, in place of the
    // URL's (docs/library/io.md § HTTP client, B-2026-11-17-54).
    for (k, v) in headers {
        if k.eq_ignore_ascii_case("content-length") || k.eq_ignore_ascii_case("host") {
            continue;
        }
        builder = builder.header(k.as_str(), v.as_str());
    }
    // The client's own headers, each unless the program set one
    // (docs/library/io.md § HTTP client, B-2026-11-14-49). No
    // `Accept-Encoding`: the client decodes no compression.
    let set = |name: &str| headers.iter().any(|(k, _)| k.eq_ignore_ascii_case(name));
    if !set("user-agent") {
        builder = builder.header(hyper::header::USER_AGENT, HTTP_USER_AGENT);
    }
    if !set("accept") {
        builder = builder.header(hyper::header::ACCEPT, "*/*");
    }
    builder
        .body(http_body_util::Full::new(bytes::Bytes::from(body.to_vec())))
        .map_err(|e| format!("{url}: bad request: {e}"))
}

/// Drain a hyper response into status + header pairs + raw body bytes.
/// Bodies stay raw (binary-safe); `Response.text()` reinterprets
/// Kāra-side. Mid-stream errors surface as `Err` (the whole-body
/// collect either completes or fails — no partial-body lenience to
/// keep h1 and h2 behavior identical).
async fn http_collect_response(
    resp: hyper::Response<hyper::body::Incoming>,
    url: &str,
) -> Result<HttpFetched, String> {
    use http_body_util::BodyExt;
    let status = resp.status().as_u16();
    let mut headers: Vec<(String, String)> = Vec::new();
    for (name, value) in resp.headers() {
        headers.push((
            name.as_str().to_string(),
            String::from_utf8_lossy(value.as_bytes()).into_owned(),
        ));
    }
    let body = resp
        .into_body()
        .collect()
        .await
        .map_err(|e| {
            format!(
                "{url}: reading response body failed: {}",
                body_failure_text(&e)
            )
        })?
        .to_bytes()
        .to_vec();
    Ok(HttpFetched {
        status,
        headers,
        body,
        url: url.to_string(),
        too_many_redirects: None,
    })
}

#[cfg(test)]
mod tls_text_tests {
    use super::*;

    fn io(e: rustls::Error) -> std::io::Error {
        std::io::Error::new(std::io::ErrorKind::InvalidData, e)
    }

    #[test]
    fn tls_failure_text_spells_out_the_rust_names() {
        let t = tls_failure_text(&io(rustls::Error::InvalidCertificate(
            rustls::CertificateError::UnknownIssuer,
        )));
        assert!(t.contains("does not chain to a trusted root"), "{t}");
        let t = tls_failure_text(&io(rustls::Error::InvalidCertificate(
            rustls::CertificateError::BadSignature,
        )));
        assert!(t.contains("a signature does not verify"), "{t}");
        let t = tls_failure_text(&io(rustls::Error::AlertReceived(
            rustls::AlertDescription::ProtocolVersion,
        )));
        assert!(t.contains("TLS 1.2 or 1.3"), "{t}");
        let t = tls_failure_text(&io(rustls::Error::InvalidCertificate(
            rustls::CertificateError::NotValidForNameContext {
                expected: rustls::pki_types::ServerName::try_from("good.test").unwrap(),
                presented: vec![
                    "DnsName(\"other.test\")".into(),
                    "IpAddress(10.0.0.1)".into(),
                ],
            },
        )));
        assert_eq!(
            t,
            "invalid peer certificate: it is not valid for \"good.test\"; its subjectAltName \
             lists only DNS name other.test, IP address 10.0.0.1"
        );
        let t = tls_failure_text(&io(rustls::Error::InvalidCertificate(
            rustls::CertificateError::ExpiredContext {
                time: rustls::pki_types::UnixTime::since_unix_epoch(
                    std::time::Duration::from_secs(1_795_343_909),
                ),
                not_after: rustls::pki_types::UnixTime::since_unix_epoch(
                    std::time::Duration::from_secs(1_795_257_503),
                ),
            },
        )));
        assert_eq!(
            t,
            "invalid peer certificate: it expired at 2026-11-21 10:38:23 UTC (the time now is \
             2026-11-22 10:38:29 UTC)"
        );
    }

    #[test]
    fn utc_text_matches_the_civil_calendar() {
        assert_eq!(utc_text(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(utc_text(951_782_400), "2000-02-29 00:00:00 UTC");
        assert_eq!(utc_text(4_102_444_799), "2099-12-31 23:59:59 UTC");
    }

    #[test]
    fn extra_ca_file_without_a_certificate_is_an_error() {
        let dir = std::env::temp_dir().join(format!("karac-extra-ca-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let empty = dir.join("empty.pem");
        std::fs::write(&empty, "").unwrap();
        let key_only = dir.join("key.pem");
        std::fs::write(
            &key_only,
            "-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n",
        )
        .unwrap();
        for p in [&empty, &key_only] {
            let mut store = rustls::RootCertStore::empty();
            let e = add_extra_ca(&mut store, p.to_str().unwrap()).unwrap_err();
            assert!(
                e.starts_with("KARAC_HTTP_EXTRA_CA: no certificate in"),
                "{e}"
            );
        }
        let mut store = rustls::RootCertStore::empty();
        let e = add_extra_ca(&mut store, dir.join("missing.pem").to_str().unwrap()).unwrap_err();
        assert!(e.contains("cannot read"), "{e}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
