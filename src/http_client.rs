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
//! demotion (307/308 keep method and body); a status of 400 or more is an
//! `Err` that keeps the response, in the message shape
//! `"{url}: status code {n}"`; `timeout_ms > 0` bounds the whole request.
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
    /// The URL that answered (the last hop's), which a status of 400 or
    /// more names in its error's message.
    pub(crate) url: String,
}

/// The error a final status of 400 or more is (docs/library/io.md § HTTP
/// client: the `HttpError` keeps the response).
pub(crate) fn http_status_error(fetched: &HttpFetched) -> Option<String> {
    (fetched.status >= 400).then(|| format!("{}: status code {}", fetched.url, fetched.status))
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
            let pem = std::fs::read(&path)
                .map_err(|e| format!("KARAC_HTTP_EXTRA_CA: cannot read '{path}': {e}"))?;
            for cert in rustls_pemfile::certs(&mut pem.as_slice()) {
                let cert =
                    cert.map_err(|e| format!("KARAC_HTTP_EXTRA_CA: bad PEM in '{path}': {e}"))?;
                root_store
                    .add(cert)
                    .map_err(|e| format!("KARAC_HTTP_EXTRA_CA: rejected cert in '{path}': {e}"))?;
            }
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
/// client runtime, bounded by `timeout_ms` when positive.
pub(crate) fn http_fetch_blocking(
    method: &str,
    url: &str,
    body: &[u8],
    headers: &[(String, String)],
    timeout_ms: i64,
) -> Result<HttpFetched, String> {
    with_http_client_runtime(|rt| {
        rt.block_on(async {
            let fut = http_fetch_redirects(method, url, body, headers);
            if timeout_ms > 0 {
                match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms as u64), fut)
                    .await
                {
                    Ok(r) => r,
                    Err(_) => Err(format!("{url}: request timed out after {timeout_ms} ms")),
                }
            } else {
                fut.await
            }
        })
    })
}

/// Follow up to 5 redirects, demoting to GET and dropping the body on
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
    for _hop in 0..=5 {
        let fetched = http_fetch_once(&method, &url, &body, &headers).await?;
        if matches!(fetched.status, 301 | 302 | 303 | 307 | 308) {
            let loc = fetched
                .headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("location"))
                .map(|(_, v)| v.clone());
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
                if !crate::http_proxy::redirect_keeps_credentials(&first, &url) {
                    headers.retain(|(k, _)| !crate::http_proxy::is_credential_header(k));
                }
                continue;
            }
        }
        // A status of 400 or more is an `Err` that keeps the response,
        // which `http_deliver` makes of it (`http_status_error`).
        return Ok(fetched);
    }
    Err(format!("{url}: too many redirects"))
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
    let uri: hyper::Uri = url.parse().map_err(|e| format!("{url}: bad url: {e}"))?;
    let scheme = uri.scheme_str().unwrap_or("http").to_string();
    // A URL is http:// or https:// (docs/library/io.md § HTTP client); any
    // other scheme is refused before a connection rather than spoken to in
    // HTTP (B-2026-11-07-28).
    if scheme != "http" && scheme != "https" {
        return Err(format!("{url}: Unknown Scheme: unknown scheme '{scheme}'"));
    }
    let host = uri
        .host()
        .ok_or_else(|| format!("{url}: url has no host"))?
        .to_string();
    let port = uri
        .port_u16()
        .unwrap_or(if scheme == "https" { 443 } else { 80 });
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
            .map_err(|e| format!("{url}: TLS handshake failed: {e}"))?;
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
    let (mut sender, conn) =
        hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(io))
            .await
            .map_err(|e| format!("{url}: HTTP/1.1 handshake failed: {e}"))?;
    let conn = tokio::task::spawn(async move {
        let _ = conn.await;
    });
    let default_port = if scheme == "https" { 443 } else { 80 };
    let host_header = if port == default_port {
        host.to_string()
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
    if let Some(host) = host_header {
        builder = builder.header(hyper::header::HOST, host);
    }
    for (k, v) in headers {
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
        .map_err(|e| format!("{url}: reading response body failed: {e}"))?
        .to_bytes()
        .to_vec();
    Ok(HttpFetched {
        status,
        headers,
        body,
        url: url.to_string(),
    })
}
