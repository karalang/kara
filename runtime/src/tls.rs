//! TLS server-side FFI backing `runtime/stdlib/tls.kara`.
//!
//! Phase 6 line 236 slice 1. Wraps rustls's server-side surface
//! (`ServerConfig` + `ServerConnection`) behind a C ABI keyed by the
//! underlying TCP fd. Slice 2 lands the `runtime/stdlib/tls.kara`
//! types and codegen lowerings that compose this surface into kara
//! source.
//!
//! ## API summary (parallels `event_loop.rs`'s TCP FFI surface)
//!
//! | Symbol | Purpose |
//! |---|---|
//! | `karac_runtime_tls_config_new` | parse PEM cert+key, build `ServerConfig`, return opaque `*mut KaracTlsConfig` |
//! | `karac_runtime_tls_config_free` | drop a previously-built config (called from kara `TlsConfig` Drop) |
//! | `karac_runtime_tls_listener_bind` | bind TCP listener fd (delegates to `karac_runtime_tcp_bind`; config not consumed at bind time) |
//! | `karac_runtime_tls_accept` | raw `accept(2)` + synchronous TLS handshake + register session; returns connection fd |
//! | `karac_runtime_tls_read` | drive rustls inbound packet processor → plaintext into caller buffer |
//! | `karac_runtime_tls_write` | plaintext into rustls writer → ciphertext to socket |
//! | `karac_runtime_tls_close` | remove session from registry + close TCP fd |
//!
//! ## Session storage
//!
//! Per-connection state lives in a global
//! `RwLock<HashMap<SessionKey, Arc<Mutex<TlsSession>>>>` keyed by the TCP
//! socket handle (`SessionKey` = `RawFd` on unix / `RawSocket` on Windows). Read/
//! write paths take the outer `RwLock` read-lock briefly to clone out
//! the `Arc<Mutex<_>>` handle, then drop the outer lock before locking
//! the inner per-session `Mutex`. Accept/close take the outer write
//! lock for insert/remove. At v1 scale (one I/O op per connection at a
//! time, dispatched from the parking primitive) this is enough;
//! DashMap-style sharding lands when 100K+ concurrent operations
//! surface contention.
//!
//! ## Handshake & I/O posture (v1)
//!
//! Handshake runs synchronously inside `karac_runtime_tls_accept` via
//! `Connection::complete_io` against a blocking `TcpStream`. Enough to
//! ship Demo 1 slice 2 (the kara accept-loop sees a fully-established
//! TLS stream). For M1's 100K-connection bench the synchronous
//! handshake is the limiting factor — it occupies a worker thread per
//! concurrent handshake. A non-blocking handshake that re-parks via
//! `karac_park_on_fd` between rustls rounds is a separate slice
//! (follow-on if measurement shows handshake throughput dominates).
//!
//! Steady-state read/write also block against the socket. The caller
//! (codegen lowering for `TlsStream.read` / `.write`) is expected to
//! have parked via `karac_park_on_fd(fd, dir)` BEFORE invoking — same
//! convention as `karac_runtime_tcp_read` / `_write`. Once readable,
//! the TLS read pumps until at least one plaintext byte is decoded
//! (rustls may need multiple read rounds to assemble a complete TLS
//! record).
//!
//! ## Crypto provider
//!
//! Built with `rustls = { default-features = false, features = ["ring", ...] }`.
//! Provider installed explicitly via `ServerConfig::builder_with_provider`
//! at every `tls_config_new` call so there's no process-global
//! `install_default` state to manage (multiple configs with different
//! providers could coexist in principle).

use rustls::pki_types::CertificateDer;
use rustls::{ClientConfig, ClientConnection, ServerConfig, ServerConnection};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

/// fd/handle key for the TLS session registry and the public FFI's internal
/// narrowing. `RawFd` (i32) on Unix, `RawSocket` (u64) on Windows — a Windows
/// `SOCKET` is a pointer-sized kernel handle and must NOT be truncated to i32.
/// The i64 fd ABI narrows to this at each FFI body's top, identically on
/// register + lookup so the key stays consistent. Cfg-aliased so the unix path
/// is byte-identical to before (it was hardcoded `i32`) and the Windows port
/// adds only the wider key.
#[cfg(unix)]
pub(crate) type SessionKey = std::os::unix::io::RawFd;
#[cfg(windows)]
pub(crate) type SessionKey = std::os::windows::io::RawSocket;

/// Reconstruct an owned `TcpStream` from a raw handle, cross-platform.
/// `pub(crate)` so the WS-over-TLS handshake path in `event_loop.rs` shares
/// the same raw-handle discipline.
///
/// # Safety
/// `k` must be a live, owned socket handle not aliased elsewhere for the
/// duration the returned value (or anything derived from it) is used.
#[cfg(unix)]
pub(crate) unsafe fn tcpstream_from_key(k: SessionKey) -> std::net::TcpStream {
    use std::os::unix::io::FromRawFd;
    std::net::TcpStream::from_raw_fd(k)
}
#[cfg(windows)]
pub(crate) unsafe fn tcpstream_from_key(k: SessionKey) -> std::net::TcpStream {
    use std::os::windows::io::FromRawSocket;
    std::net::TcpStream::from_raw_socket(k)
}

/// Reconstruct an owned `TcpListener` from a raw handle, cross-platform.
///
/// # Safety
/// `k` must be a live, owned listener handle not aliased elsewhere for the
/// duration the returned value is used.
#[cfg(unix)]
pub(crate) unsafe fn tcplistener_from_key(k: SessionKey) -> std::net::TcpListener {
    use std::os::unix::io::FromRawFd;
    std::net::TcpListener::from_raw_fd(k)
}
#[cfg(windows)]
pub(crate) unsafe fn tcplistener_from_key(k: SessionKey) -> std::net::TcpListener {
    use std::os::windows::io::FromRawSocket;
    std::net::TcpListener::from_raw_socket(k)
}

/// Relinquish a stream's destructor and return its raw handle as the
/// `SessionKey`, cross-platform (the no-close discipline at the FFI boundary).
#[cfg(unix)]
pub(crate) fn tcpstream_into_key(s: std::net::TcpStream) -> SessionKey {
    use std::os::unix::io::IntoRawFd;
    s.into_raw_fd()
}
#[cfg(windows)]
pub(crate) fn tcpstream_into_key(s: std::net::TcpStream) -> SessionKey {
    use std::os::windows::io::IntoRawSocket;
    s.into_raw_socket()
}

/// Borrow a stream's raw handle as the `SessionKey` WITHOUT relinquishing
/// ownership, cross-platform. Used by the WS-over-TLS handshake worker, which
/// registers a session keyed by the borrowed handle before the upgrade.
#[cfg(unix)]
pub(crate) fn tcpstream_as_key(s: &std::net::TcpStream) -> SessionKey {
    use std::os::unix::io::AsRawFd;
    s.as_raw_fd()
}
#[cfg(windows)]
pub(crate) fn tcpstream_as_key(s: &std::net::TcpStream) -> SessionKey {
    use std::os::windows::io::AsRawSocket;
    s.as_raw_socket()
}

/// Relinquish a listener's destructor and return its raw handle (the no-close
/// discipline when transiently borrowing a listener fd at the FFI boundary).
#[cfg(unix)]
pub(crate) fn tcplistener_into_key(l: std::net::TcpListener) -> SessionKey {
    use std::os::unix::io::IntoRawFd;
    l.into_raw_fd()
}
#[cfg(windows)]
pub(crate) fn tcplistener_into_key(l: std::net::TcpListener) -> SessionKey {
    use std::os::windows::io::IntoRawSocket;
    l.into_raw_socket()
}

/// Opaque config wrapper handed back through the FFI. Holds an
/// `Arc<ServerConfig>` so subsequent `tls_accept` calls clone it
/// cheaply into each new `ServerConnection`.
pub struct KaracTlsConfig {
    inner: Arc<ServerConfig>,
}

/// Per-fd session state. The outer `Mutex` ensures one read or write
/// at a time per session (concurrent operations against the same TLS
/// state would corrupt rustls's internal buffers).
///
/// **`pub(crate)`** because slice 3 (`event_loop.rs`'s WebSocket
/// framing FFIs) consults the session via [`lookup_session`] and
/// drives `conn` through rustls's `reader()` / `writer()` /
/// `read_tls` / `write_tls` surface.
///
/// **Phase-8 line 22 (`TlsClientStream`):** `conn` is `rustls::Connection`
/// (the enum over `ServerConnection` + `ClientConnection`) so the same
/// `SESSIONS` map and the same read/write/close paths serve both
/// directions — both inner variants delegate the methods called by
/// `drive_read` / `drive_write` (`reader` / `writer` / `wants_read` /
/// `read_tls` / `wants_write` / `write_tls` / `process_new_packets`).
pub(crate) struct TlsSession {
    pub(crate) conn: rustls::Connection,
}

type SessionRegistry = RwLock<HashMap<SessionKey, Arc<Mutex<TlsSession>>>>;

/// Lazy-initialized global registry mapping TCP fd → TLS session.
fn sessions() -> &'static SessionRegistry {
    static REG: OnceLock<SessionRegistry> = OnceLock::new();
    REG.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Phase 6 line 236 slice 3 — exposed for the WebSocket framing FFIs
/// (`event_loop.rs::ws_send_data_frame` / `_recv_data_frame`) so they
/// can detect a TLS-wrapped connection and route encryption through
/// rustls instead of writing plaintext over the TLS-encrypted socket.
///
/// Returns the `Arc<Mutex<TlsSession>>` for `fd` if it was previously
/// registered via [`karac_runtime_tls_accept`] (or
/// [`register_session_for_fd`] during the WS-over-TLS handshake) and
/// hasn't yet been removed by [`karac_runtime_tls_close`]. Returns
/// `None` for plain-TCP fds. Cloning the Arc is fast and lets callers
/// release the outer `RwLock` before locking the per-session `Mutex`.
pub(crate) fn lookup_session(fd: SessionKey) -> Option<Arc<Mutex<TlsSession>>> {
    let reg = sessions().read().unwrap_or_else(|p| p.into_inner());
    reg.get(&fd).cloned()
}

/// Slice 3 — register a fresh `ServerConnection` against `fd`. Used by
/// [`karac_runtime_ws_accept_tls`] (in `event_loop.rs`) after the TLS
/// handshake completes but before the HTTP upgrade exchange: the WS
/// framing FFIs need the session to be in `SESSIONS` so subsequent
/// recv/send routes through TLS.
///
/// Parallel surface to the inline-insert at the end of
/// [`karac_runtime_tls_accept`]; this version is callable from
/// outside the FFI for cases where the handshake driver lives in a
/// different module.
pub(crate) fn register_session_for_fd(fd: SessionKey, conn: rustls::Connection) {
    let mut reg = sessions().write().unwrap_or_else(|p| p.into_inner());
    reg.insert(fd, Arc::new(Mutex::new(TlsSession { conn })));
}

/// Slice 3 — borrow the `Arc<ServerConfig>` out of a `*mut KaracTlsConfig`
/// pointer. Exposed for [`karac_runtime_ws_accept_tls`] which needs to
/// build a fresh `ServerConnection` per accepted connection.
///
/// # Safety
///
/// `config` must be a non-null pointer obtained from
/// [`karac_runtime_tls_config_new`] and still valid.
pub(crate) unsafe fn clone_config_arc(config: *const KaracTlsConfig) -> Arc<ServerConfig> {
    Arc::clone(&(*config).inner)
}

/// Failure mode for `build_server_config`. Surfaced internally only —
/// the FFI maps every variant to a `null` config pointer / `-1` fd.
/// Slice 2's `TlsError` enum decodes the FFI failure into kara-visible
/// variants if a real consumer needs them.
#[derive(Debug)]
pub(crate) enum ConfigBuildError {
    NoCertsFound,
    NoPrivateKey,
    InvalidPem,
    ProtocolSetup,
    RustlsConfig,
}

/// Parse PEM bytes into a rustls `ServerConfig`. Accepts any of the
/// PEM private-key formats rustls-pemfile recognises (PKCS#8, RSA,
/// SEC1) — `private_key()` returns the first key block it finds.
///
/// Visibility: `pub(crate)` so `karac_runtime_serve_https` in
/// `lib.rs` can reuse the same PEM-parsing path the
/// `TlsListener.bind_tls` FFI uses, rather than re-deriving the
/// rustls config-builder dance.
pub(crate) fn build_server_config(
    cert_pem: &[u8],
    key_pem: &[u8],
) -> Result<ServerConfig, ConfigBuildError> {
    let mut cert_reader = std::io::BufReader::new(cert_pem);
    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut cert_reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| ConfigBuildError::InvalidPem)?;
    if certs.is_empty() {
        return Err(ConfigBuildError::NoCertsFound);
    }

    let mut key_reader = std::io::BufReader::new(key_pem);
    let key = rustls_pemfile::private_key(&mut key_reader)
        .map_err(|_| ConfigBuildError::InvalidPem)?
        .ok_or(ConfigBuildError::NoPrivateKey)?;

    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|_| ConfigBuildError::ProtocolSetup)?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|_| ConfigBuildError::RustlsConfig)?;

    Ok(config)
}

/// Phase-8 line 22 — client-side mirror of [`build_server_config`].
/// Parses `roots_pem` (one or more PEM-encoded trust anchor certs) into
/// a `rustls::RootCertStore` and builds a `ClientConfig` that trusts
/// only those roots. No client auth, safe default protocol versions,
/// `ring` crypto provider — same posture as the server config so client
/// + server use a compatible cipher / version surface.
///
/// Visibility: `pub(crate)` so `karac_runtime_tls_client_connect` can
/// reuse this off the same PEM-parsing path the server config goes
/// through.
pub(crate) fn build_client_config(roots_pem: &[u8]) -> Result<ClientConfig, ConfigBuildError> {
    let mut reader = std::io::BufReader::new(roots_pem);
    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| ConfigBuildError::InvalidPem)?;
    if certs.is_empty() {
        return Err(ConfigBuildError::NoCertsFound);
    }
    let mut roots = rustls::RootCertStore::empty();
    for cert in certs {
        roots
            .add(cert)
            .map_err(|_| ConfigBuildError::RustlsConfig)?;
    }

    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|_| ConfigBuildError::ProtocolSetup)?
        .with_root_certificates(roots)
        .with_no_client_auth();

    Ok(config)
}

/// Build a `ServerConfig` from PEM cert + key bytes. Returns an opaque
/// `*mut KaracTlsConfig` on success or null on any parse / build
/// failure. The pointer is freed via `karac_runtime_tls_config_free`.
///
/// # Safety
///
/// `cert_pem` must point to `cert_len` readable bytes; `key_pem` to
/// `key_len` readable bytes. Both buffers are read once during the
/// call and not retained. Null pointers or non-positive lengths
/// return null without reading.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_tls_config_new(
    cert_pem: *const u8,
    cert_len: i64,
    key_pem: *const u8,
    key_len: i64,
) -> *mut KaracTlsConfig {
    if cert_pem.is_null() || cert_len <= 0 || key_pem.is_null() || key_len <= 0 {
        return std::ptr::null_mut();
    }
    let cert_bytes = std::slice::from_raw_parts(cert_pem, cert_len as usize);
    let key_bytes = std::slice::from_raw_parts(key_pem, key_len as usize);
    match build_server_config(cert_bytes, key_bytes) {
        Ok(c) => Box::into_raw(Box::new(KaracTlsConfig { inner: Arc::new(c) })),
        Err(_) => std::ptr::null_mut(),
    }
}

/// Free a config previously returned by `karac_runtime_tls_config_new`.
/// Idempotent for null. Called by slice 2's `impl Drop for TlsConfig`
/// wrapper.
///
/// # Safety
///
/// `config` must be a pointer obtained from `karac_runtime_tls_config_new`
/// or null. Double-free is undefined behaviour (no use-after-free
/// guard).
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_tls_config_free(config: *mut KaracTlsConfig) {
    if !config.is_null() {
        let _ = Box::from_raw(config);
    }
}

/// Bind a TLS listener. Delegates to `karac_runtime_tcp_bind` — TLS
/// state is constructed per-connection at accept time, so the bind
/// step is identical to plain TCP. The `_config` parameter is unused
/// at bind time but kept in the signature so slice 2's `TlsListener.bind_tls`
/// can pass through without an asymmetric extra hop.
///
/// Same `BOUND_PORT=<n>\n` stdout convention as the TCP path when the
/// address ends in `:0`.
///
/// Cross-platform: `karac_runtime_tcp_bind` has both unix and Windows
/// (`#[cfg(windows)]`) bodies, so the delegate works on either.
///
/// # Safety
///
/// `addr_ptr` must point to `addr_len` readable bytes (UTF-8 socket
/// address string). Buffer is read once and not retained.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_tls_listener_bind(
    addr_ptr: *const u8,
    addr_len: i64,
    _config: *mut KaracTlsConfig,
) -> i64 {
    // i64 fd ABI: delegates to `tcp_bind`, which already returns the
    // listener fd (or a negative error code) as i64.
    crate::event_loop::karac_runtime_tcp_bind(addr_ptr, addr_len)
}

/// Raw `accept(2)` followed by a synchronous TLS handshake. Caller
/// (codegen lowering for `TlsListener.accept`) is expected to have
/// parked via `karac_park_on_fd(listener_fd, 0)` before invoking. The
/// handshake itself is blocking — it occupies the calling worker
/// thread until the client completes the TLS round-trip. v1 trade-off,
/// see module-level doc.
///
/// Returns the connection fd on success, registering a fresh
/// `TlsSession` in the per-fd registry. On any failure (accept
/// failure, `ServerConnection::new` error, handshake failure) returns
/// `-1`; the underlying TCP connection — if `accept(2)` succeeded —
/// closes when the local `TcpStream` drops.
///
/// # Safety
///
/// `listener_fd` must come from a successful prior
/// `karac_runtime_tls_listener_bind` (or `karac_runtime_tcp_bind`) call;
/// `config` must be a non-null pointer obtained from
/// `karac_runtime_tls_config_new` and still valid (not yet freed).
/// Passing other values is undefined behaviour — non-negative fds that
/// aren't real listeners surface as an immediate `accept(2)` failure,
/// but the function will attempt the syscall first.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_tls_accept(
    listener_fd: i64,
    config: *mut KaracTlsConfig,
) -> i64 {
    if listener_fd < 0 || config.is_null() {
        return -1;
    }
    // i64 fd ABI → narrow to the platform `SessionKey` (`RawFd` i32 on Unix,
    // `RawSocket` u64 on Windows). The TLS session map is keyed by this
    // narrowed handle (register + lookup narrow identically).
    let listener_key = listener_fd as SessionKey;
    let cfg = &*config;

    let listener = tcplistener_from_key(listener_key);
    let accept_result = listener.accept();
    let _ = tcplistener_into_key(listener);
    let (mut sock, _addr) = match accept_result {
        Ok(p) => p,
        Err(_) => return -1,
    };
    // Disable Nagle: the TLS handshake is a multi-RTT exchange of small
    // records, where Nagle×delayed-ACK injects a ~40 ms stall (full
    // rationale + measurement at `karac_runtime_ws_accept_tls`).
    // Best-effort — failure only forgoes the latency win.
    let _ = sock.set_nodelay(true);

    let mut conn = match ServerConnection::new(Arc::clone(&cfg.inner)) {
        Ok(c) => c,
        Err(_) => return -1,
    };

    if conn.complete_io(&mut sock).is_err() {
        // sock drops here, closing the underlying fd. `conn` drops too;
        // any state it allocated is reclaimed.
        return -1;
    }

    let key = tcpstream_into_key(sock);
    let mut reg = sessions().write().unwrap_or_else(|p| p.into_inner());
    reg.insert(
        key,
        Arc::new(Mutex::new(TlsSession {
            conn: rustls::Connection::Server(conn),
        })),
    );
    // i64 fd ABI: the `SessionKey` (Unix `RawFd` i32 sign-extends; Windows
    // `RawSocket` u64 occupies the full width) widens losslessly at the public
    // return boundary; the session map stays keyed by the same handle.
    key as i64
}

/// Phase-8 line 22 — TLS client-side connect + synchronous handshake.
/// Open a TCP connection to `addr`, build a `ClientConfig` whose root
/// store contains the trust anchors in `roots_pem`, build a
/// `ClientConnection` bound to `server_name` for SNI + cert
/// verification, run `complete_io` against the blocking socket to
/// finish the handshake, and register the resulting session in the
/// shared `SESSIONS` map. Returns the connection fd on success or `-1`
/// on any failure (addr / name parse, PEM parse, TCP connect, rustls
/// build, handshake) — the partial TCP connection (if any) is closed
/// when the local `TcpStream` drops.
///
/// The fd is interchangeable with one returned by
/// `karac_runtime_tls_accept`: `karac_runtime_tls_read` / `_write` /
/// `_close` all look up the session by fd and drive rustls through the
/// `rustls::Connection` enum — direction-agnostic.
///
/// Trust posture: only the certs in `roots_pem` are trusted. There is
/// no fallback to system / webpki-roots — the caller is responsible
/// for supplying the right trust anchors. Public-CA trust lands when a
/// real consumer (typically `Client.get("https://...")`) wires
/// webpki-roots as the default; for v1 the explicit PEM keeps the
/// surface minimal and the dep tree small.
///
/// # Safety
///
/// Each of `addr_ptr` / `server_name_ptr` / `roots_pem_ptr` must point
/// at the matching `_len` initialized bytes (or be null with `_len <=
/// 0`, in which case the call fails with `-1`).
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_tls_client_connect(
    addr_ptr: *const u8,
    addr_len: i64,
    server_name_ptr: *const u8,
    server_name_len: i64,
    roots_pem_ptr: *const u8,
    roots_pem_len: i64,
) -> i64 {
    // ── Parse the destination address ──
    if addr_ptr.is_null() || addr_len <= 0 {
        return -1;
    }
    let addr_bytes = std::slice::from_raw_parts(addr_ptr, addr_len as usize);
    let addr_str = match std::str::from_utf8(addr_bytes) {
        Ok(s) => s,
        Err(_) => return -1,
    };
    let socket_addr: std::net::SocketAddr = match addr_str.parse() {
        Ok(a) => a,
        Err(_) => return -1,
    };

    // ── Parse the server name (SNI + cert verification) ──
    if server_name_ptr.is_null() || server_name_len <= 0 {
        return -1;
    }
    let server_name_bytes = std::slice::from_raw_parts(server_name_ptr, server_name_len as usize);
    let server_name_str = match std::str::from_utf8(server_name_bytes) {
        Ok(s) => s,
        Err(_) => return -1,
    };
    // `ServerName::try_from(String)` yields a `ServerName<'static>`
    // (owned), which is what `ClientConnection::new` requires.
    let server_name = match rustls::pki_types::ServerName::try_from(server_name_str.to_owned()) {
        Ok(n) => n,
        Err(_) => return -1,
    };

    // ── Build the `ClientConfig` from the supplied trust anchors ──
    let roots_bytes: &[u8] = if roots_pem_ptr.is_null() || roots_pem_len <= 0 {
        &[]
    } else {
        std::slice::from_raw_parts(roots_pem_ptr, roots_pem_len as usize)
    };
    let client_config = match build_client_config(roots_bytes) {
        Ok(c) => c,
        Err(_) => return -1,
    };

    // ── TCP connect + handshake ──
    // The TCP-connect leg carries the same branchable causes as the
    // plain-TCP client (ECONNREFUSED etc.) — surface them via the stable
    // code (phase-8 line 74). Handshake failures below stay `-1`
    // (decoded as the TlsError default variant, `Protocol`).
    let mut sock = match std::net::TcpStream::connect(socket_addr) {
        Ok(s) => s,
        Err(e) => return crate::event_loop::net_construct_error_code(&e) as i64,
    };
    // Client half of the handshake-latency fix: disable Nagle so the
    // client's small handshake / WS-upgrade records aren't withheld
    // behind the peer's delayed-ACK. The 2×2 probe (REPORT.md §p50)
    // showed client-side Nagle is what leaves the connect-p50 *tail* at
    // ~47 ms after the server-side fix clears the median. Best-effort.
    let _ = sock.set_nodelay(true);
    let mut client_conn = match ClientConnection::new(Arc::new(client_config), server_name) {
        Ok(c) => c,
        Err(_) => return -1,
    };
    if client_conn.complete_io(&mut sock).is_err() {
        // `sock` drops here, closing the partially-open TCP connection.
        return -1;
    }

    // ── Register the session keyed by fd (same map the server side
    // uses; `karac_runtime_tls_read`/`_write`/`_close` reach this
    // through `lookup_session`). ──
    let key = tcpstream_into_key(sock);
    let mut reg = sessions().write().unwrap_or_else(|p| p.into_inner());
    reg.insert(
        key,
        Arc::new(Mutex::new(TlsSession {
            conn: rustls::Connection::Client(client_conn),
        })),
    );
    // i64 fd ABI: see `karac_runtime_tls_accept`'s return note.
    key as i64
}

/// Drive rustls's inbound packet processor: ciphertext from socket →
/// rustls decryption → plaintext into `buf`. Loops until at least one
/// plaintext byte is available or a terminal condition fires (clean
/// peer close, socket error, TLS protocol error).
///
/// Returns:
/// - `n > 0`: decrypted plaintext byte count written to `buf`
/// - `0`: clean EOF (peer's close_notify received OR socket EOF)
/// - `n < 0`: error. Negative-errno style for socket I/O errors; `-1`
///   for non-syscall errors (TLS protocol failure, session lookup
///   miss, invalid input).
///
/// Caller is expected to have parked via `karac_park_on_fd(fd, 0)`
/// BEFORE invoking — the FFI does blocking reads against the socket
/// otherwise.
///
/// # Safety
///
/// `buf_ptr` must point to a writable buffer of at least `buf_len`
/// bytes that lives for the duration of the call. Null buffer with
/// non-zero `buf_len` is rejected via the `buf_len <= 0` early return.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_tls_read(
    stream_fd: i64,
    buf_ptr: *mut u8,
    buf_len: i64,
) -> i64 {
    if stream_fd < 0 {
        return -1;
    }
    if buf_ptr.is_null() || buf_len <= 0 {
        return 0;
    }
    // i64 fd ABI → narrow to the platform `SessionKey`; session map keyed by it.
    let stream_key = stream_fd as SessionKey;

    let session = {
        let reg = sessions().read().unwrap_or_else(|p| p.into_inner());
        match reg.get(&stream_key) {
            Some(s) => Arc::clone(s),
            None => return -1,
        }
    };

    let mut sess = session.lock().unwrap_or_else(|p| p.into_inner());
    let mut sock = tcpstream_from_key(stream_key);
    let result = drive_read(&mut sess.conn, &mut sock, buf_ptr, buf_len as usize);
    let _ = tcpstream_into_key(sock);
    result
}

/// Read-side driver. Loops between rustls's `reader().read()` (which
/// hands back decrypted plaintext) and `read_tls` / `process_new_packets`
/// (which pull more ciphertext from the socket) until plaintext is
/// available or a terminal condition fires.
fn drive_read(
    conn: &mut rustls::Connection,
    sock: &mut std::net::TcpStream,
    buf_ptr: *mut u8,
    buf_len: usize,
) -> i64 {
    // SAFETY: caller's contract — buf_ptr/buf_len describe a valid
    // writable buffer for the duration of the FFI call.
    let buf = unsafe { std::slice::from_raw_parts_mut(buf_ptr, buf_len) };

    loop {
        // Try to read decrypted plaintext first. rustls's reader()
        // returns immediately if plaintext is buffered; if empty, it
        // signals via Ok(0) so we know to pull more ciphertext.
        match conn.reader().read(buf) {
            Ok(0) => {
                if !conn.wants_read() {
                    // No more ciphertext expected (close_notify seen)
                    // and no plaintext buffered — clean EOF.
                    return 0;
                }
                match conn.read_tls(sock) {
                    Ok(0) => return 0, // socket EOF
                    Ok(_) => {
                        if conn.process_new_packets().is_err() {
                            return -1;
                        }
                        continue;
                    }
                    Err(e) => return io_err_to_neg(&e),
                }
            }
            Ok(n) => return n as i64,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // Reader signalled WouldBlock — pull more ciphertext.
                if !conn.wants_read() {
                    return 0;
                }
                match conn.read_tls(sock) {
                    Ok(0) => return 0,
                    Ok(_) => {
                        if conn.process_new_packets().is_err() {
                            return -1;
                        }
                        continue;
                    }
                    Err(e) => return io_err_to_neg(&e),
                }
            }
            Err(e) => return io_err_to_neg(&e),
        }
    }
}

/// Encrypt plaintext from `buf` and push the resulting ciphertext to
/// the socket. Returns the byte count written into the rustls writer
/// (which always equals `buf_len` on success — rustls's writer never
/// short-writes), or a negative-errno-style error on socket / TLS
/// failure.
///
/// Caller is expected to have parked via `karac_park_on_fd(fd, 1)`
/// BEFORE invoking.
///
/// # Safety
///
/// `buf_ptr` must point to a readable buffer of at least `buf_len`
/// bytes for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_tls_write(
    stream_fd: i64,
    buf_ptr: *const u8,
    buf_len: i64,
) -> i64 {
    if stream_fd < 0 {
        return -1;
    }
    if buf_ptr.is_null() || buf_len <= 0 {
        return 0;
    }
    // i64 fd ABI → narrow to the platform `SessionKey`; session map keyed by it.
    let stream_key = stream_fd as SessionKey;

    let session = {
        let reg = sessions().read().unwrap_or_else(|p| p.into_inner());
        match reg.get(&stream_key) {
            Some(s) => Arc::clone(s),
            None => return -1,
        }
    };

    let mut sess = session.lock().unwrap_or_else(|p| p.into_inner());
    let buf = std::slice::from_raw_parts(buf_ptr, buf_len as usize);
    let mut sock = tcpstream_from_key(stream_key);
    let result = drive_write(&mut sess.conn, &mut sock, buf);
    let _ = tcpstream_into_key(sock);
    result
}

fn drive_write(conn: &mut rustls::Connection, sock: &mut std::net::TcpStream, buf: &[u8]) -> i64 {
    // Buffer the plaintext into rustls — this performs encryption
    // into the connection's sendable_tls buffer but doesn't touch the
    // socket. write() never short-writes per rustls's API.
    let n = match conn.writer().write(buf) {
        Ok(n) => n,
        Err(e) => return io_err_to_neg(&e),
    };
    // Now flush ciphertext to the socket. wants_write() reports true
    // until the sendable_tls buffer is drained.
    while conn.wants_write() {
        match conn.write_tls(sock) {
            Ok(_) => {}
            Err(e) => return io_err_to_neg(&e),
        }
    }
    n as i64
}

/// Close a TLS connection: drop the session entry (releases
/// `ServerConnection` and any TLS state) and close the underlying TCP
/// fd. Idempotent for negative fds and for fds not in the registry
/// (the close still runs on the bare TCP side so a leaked fd is
/// reclaimed). Mirrors `karac_runtime_tcp_close`'s contract.
#[no_mangle]
pub extern "C" fn karac_runtime_tls_close(fd: i64) -> i32 {
    if fd < 0 {
        return 0;
    }
    // i64 fd ABI → narrow to the platform `SessionKey`; session map keyed by it.
    let key = fd as SessionKey;
    {
        let mut reg = sessions().write().unwrap_or_else(|p| p.into_inner());
        reg.remove(&key);
    }
    // SAFETY: same convention as karac_runtime_tcp_close — reconstruct a
    // TcpStream from the raw handle and let Drop run the close on it.
    let _ = unsafe { tcpstream_from_key(key) };
    0
}

/// Map a `std::io::Error` to the negative-errno return convention
/// shared with `karac_runtime_tcp_read` / `_write`. EINTR / EAGAIN
/// surface as their actual errno; non-syscall errors fall back to `-1`.
fn io_err_to_neg(e: &std::io::Error) -> i64 {
    let errno = e.raw_os_error().unwrap_or(1);
    if errno > 0 {
        -(errno as i64)
    } else {
        -1
    }
}

#[cfg(test)]
mod tests {
    //! Unit tests for the TLS FFI surface. End-to-end round-trip uses
    //! `rcgen` (dev-dep) to generate a self-signed cert at test time,
    //! sets up a real TCP listener + a separate thread for the server
    //! side, and connects a `rustls::ClientConnection` directly to
    //! exercise the wire protocol. Test-cert fixtures live under
    //! `tests/fixtures/tls/` (slice 4); the dev-dep generation here is
    //! the slice-1 hermetic-test approach.
    //!
    //! Cross-platform (step 4b, 2026-06-17): the TLS FFI surface is now
    //! cross-platform, so these run on Windows too — the raw listener/stream
    //! handle plumbing goes through the cfg-aliased `tcplistener_*`/`tcpstream_*`
    //! helpers (the `SessionKey` is `RawFd` on unix / `RawSocket` on Windows).
    use super::*;
    use std::net::TcpStream;
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    /// Generate a fresh self-signed PEM cert + private key. CN =
    /// "localhost", valid forever (rcgen's default). Returns
    /// (cert_pem_bytes, key_pem_bytes).
    fn gen_test_cert() -> (Vec<u8>, Vec<u8>) {
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])
            .expect("rcgen: generate_simple_self_signed");
        let cert_pem = cert.cert.pem().into_bytes();
        let key_pem = cert.signing_key.serialize_pem().into_bytes();
        (cert_pem, key_pem)
    }

    /// Build a `rustls::ClientConfig` that trusts the supplied
    /// server certificate (and nothing else). Used by round-trip
    /// tests to drive a client-side handshake against the FFI.
    fn build_test_client_config(server_cert_pem: &[u8]) -> Arc<rustls::ClientConfig> {
        use rustls::pki_types::CertificateDer;
        let mut reader = std::io::BufReader::new(server_cert_pem);
        let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut reader)
            .collect::<Result<Vec<_>, _>>()
            .expect("parse server cert");
        let mut roots = rustls::RootCertStore::empty();
        for c in certs {
            roots.add(c).expect("add root");
        }
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let cfg = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("protocol")
            .with_root_certificates(roots)
            .with_no_client_auth();
        Arc::new(cfg)
    }

    #[test]
    fn config_new_with_valid_pem_returns_non_null() {
        let (cert_pem, key_pem) = gen_test_cert();
        let cfg = unsafe {
            karac_runtime_tls_config_new(
                cert_pem.as_ptr(),
                cert_pem.len() as i64,
                key_pem.as_ptr(),
                key_pem.len() as i64,
            )
        };
        assert!(!cfg.is_null(), "valid PEM should produce a non-null config");
        unsafe { karac_runtime_tls_config_free(cfg) };
    }

    #[test]
    fn config_new_with_garbage_cert_returns_null() {
        let (_cert_pem, key_pem) = gen_test_cert();
        let cert_pem = b"not a real certificate\n";
        let cfg = unsafe {
            karac_runtime_tls_config_new(
                cert_pem.as_ptr(),
                cert_pem.len() as i64,
                key_pem.as_ptr(),
                key_pem.len() as i64,
            )
        };
        assert!(
            cfg.is_null(),
            "garbage cert PEM should produce a null config"
        );
    }

    #[test]
    fn config_new_with_garbage_key_returns_null() {
        let (cert_pem, _key_pem) = gen_test_cert();
        let key_pem = b"not a real key\n";
        let cfg = unsafe {
            karac_runtime_tls_config_new(
                cert_pem.as_ptr(),
                cert_pem.len() as i64,
                key_pem.as_ptr(),
                key_pem.len() as i64,
            )
        };
        assert!(
            cfg.is_null(),
            "garbage key PEM should produce a null config"
        );
    }

    #[test]
    fn config_new_with_null_or_empty_returns_null() {
        let cert_pem = b"x";
        let key_pem = b"y";
        // Null cert pointer.
        let cfg = unsafe {
            karac_runtime_tls_config_new(
                std::ptr::null(),
                10,
                key_pem.as_ptr(),
                key_pem.len() as i64,
            )
        };
        assert!(cfg.is_null());
        // Null key pointer.
        let cfg = unsafe {
            karac_runtime_tls_config_new(
                cert_pem.as_ptr(),
                cert_pem.len() as i64,
                std::ptr::null(),
                10,
            )
        };
        assert!(cfg.is_null());
        // Zero-length cert.
        let cfg = unsafe {
            karac_runtime_tls_config_new(
                cert_pem.as_ptr(),
                0,
                key_pem.as_ptr(),
                key_pem.len() as i64,
            )
        };
        assert!(cfg.is_null());
    }

    #[test]
    fn config_free_handles_null() {
        // Idempotent for null per the doc — no crash.
        unsafe { karac_runtime_tls_config_free(std::ptr::null_mut()) };
    }

    #[test]
    fn round_trip_echo() {
        let (cert_pem, key_pem) = gen_test_cert();
        let cfg = unsafe {
            karac_runtime_tls_config_new(
                cert_pem.as_ptr(),
                cert_pem.len() as i64,
                key_pem.as_ptr(),
                key_pem.len() as i64,
            )
        };
        assert!(!cfg.is_null());

        // Bind directly via stdlib (don't print BOUND_PORT — we know
        // the port via local_addr). Using std::net::TcpListener to
        // keep the test independent of the BOUND_PORT stdout side
        // effect from karac_runtime_tcp_bind.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        // Server thread: accept once via the FFI, then echo one
        // message back.
        let cfg_addr = cfg as usize;
        let listener_fd = tcplistener_into_key(listener) as i64;
        let server = thread::spawn(move || {
            let fd = unsafe { karac_runtime_tls_accept(listener_fd, cfg_addr as *mut _) };
            assert!(fd >= 0, "tls_accept failed");
            // Read up to 64 bytes
            let mut buf = vec![0u8; 64];
            let n = unsafe { karac_runtime_tls_read(fd, buf.as_mut_ptr(), buf.len() as i64) };
            assert!(n > 0, "tls_read returned {}", n);
            // Echo back
            let w = unsafe { karac_runtime_tls_write(fd, buf.as_ptr(), n) };
            assert_eq!(w, n);
            // Close
            karac_runtime_tls_close(fd);
            // Also close the listener fd to free the kernel resource.
            let _ = unsafe { tcplistener_from_key(listener_fd as SessionKey) };
            n as usize
        });

        // Give the server thread a moment to enter accept.
        thread::sleep(Duration::from_millis(50));

        // Client side using rustls directly.
        let client_cfg = build_test_client_config(&cert_pem);
        let server_name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
        let mut client = rustls::ClientConnection::new(client_cfg, server_name).unwrap();
        let mut sock = TcpStream::connect(("127.0.0.1", port)).unwrap();
        client.complete_io(&mut sock).unwrap();

        let msg = b"hello via tls";
        client.writer().write_all(msg).unwrap();
        while client.wants_write() {
            client.write_tls(&mut sock).unwrap();
        }

        // Read the echo back.
        let mut echo = vec![0u8; msg.len()];
        let mut received = 0usize;
        while received < msg.len() {
            if client.wants_read() {
                client.read_tls(&mut sock).unwrap();
                client.process_new_packets().unwrap();
            }
            let n = client.reader().read(&mut echo[received..]).unwrap_or(0);
            if n == 0 && !client.wants_read() {
                break;
            }
            received += n;
        }

        assert_eq!(&echo[..received], msg, "echo mismatch");
        let server_n = server.join().unwrap();
        assert_eq!(server_n, msg.len());

        unsafe { karac_runtime_tls_config_free(cfg) };
    }

    /// Regression for the macOS-only `karac_runtime_ws_accept_tls`
    /// failure (2026-05-30, phase-6-runtime.md line 231): on BSD-derived
    /// kernels accepted sockets inherit `O_NONBLOCK` from the listener,
    /// and the handshake-pool initializer flips the listener
    /// non-blocking so the FFI's accept-drain loop returns `WouldBlock`
    /// on an empty backlog. Without the reset the synchronous handshake
    /// worker's first `TlsConnIo::read` returned `WouldBlock` before
    /// the peer's request landed, so every connection failed at the
    /// WS-upgrade step (server-side stats: `fail_ws_upgrade = N`, mean
    /// ~0.5 ms — far too fast to be a real failure). Linux did not hit
    /// this because Linux's `accept(2)` returns blocking sockets
    /// regardless of the listener's flags.
    ///
    /// What this test pins: a full client TLS handshake + RFC 6455 WS
    /// upgrade against `karac_runtime_ws_accept_tls` returns a
    /// non-negative fd. With the `sock.set_nonblocking(false)` line
    /// removed from the accept-drain loop, this test deadlocks on
    /// macOS (worker reads `WouldBlock`, returns -1, client times out
    /// reading the 101 response).
    #[test]
    fn ws_accept_tls_succeeds_with_nonblocking_listener() {
        use std::io::{Read, Write};

        let (cert_pem, key_pem) = gen_test_cert();
        let cfg = unsafe {
            karac_runtime_tls_config_new(
                cert_pem.as_ptr(),
                cert_pem.len() as i64,
                key_pem.as_ptr(),
                key_pem.len() as i64,
            )
        };
        assert!(!cfg.is_null());

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let cfg_addr = cfg as usize;
        let listener_fd = tcplistener_into_key(listener) as i64;

        // Server thread: invoke the FFI under test. With the fix in
        // place this returns a positive fd; without it, the handshake
        // worker fails and the FFI's loop never produces a completed
        // fd, so we bound the wait by joining with a deadline.
        let server = thread::spawn(move || unsafe {
            crate::event_loop::karac_runtime_ws_accept_tls(listener_fd, cfg_addr as *mut _)
        });

        // Give the worker pool a moment to spin up + flip the listener
        // non-blocking before we open the client.
        thread::sleep(Duration::from_millis(50));

        // Client side: rustls handshake + RFC 6455 upgrade request.
        let client_cfg = build_test_client_config(&cert_pem);
        let server_name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
        let mut client = rustls::ClientConnection::new(client_cfg, server_name).unwrap();
        let mut sock = TcpStream::connect(("127.0.0.1", port)).unwrap();
        client.complete_io(&mut sock).unwrap();

        let req = b"GET / HTTP/1.1\r\n\
                    Host: localhost\r\n\
                    Upgrade: websocket\r\n\
                    Connection: Upgrade\r\n\
                    Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
                    Sec-WebSocket-Version: 13\r\n\
                    \r\n";
        client.writer().write_all(req).unwrap();
        while client.wants_write() {
            client.write_tls(&mut sock).unwrap();
        }

        // Read the 101 response. Bounded by a wall-clock deadline; on
        // the pre-fix macOS path the server never sends a response.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut buf = Vec::with_capacity(512);
        let mut tmp = [0u8; 256];
        sock.set_read_timeout(Some(Duration::from_secs(5))).ok();
        loop {
            if std::time::Instant::now() >= deadline {
                panic!(
                    "ws_accept_tls did not send 101 within 5s — accepted socket likely \
                     left in non-blocking mode (macOS O_NONBLOCK inheritance regression). \
                     Buffered so far: {:?}",
                    String::from_utf8_lossy(&buf)
                );
            }
            if client.wants_read() {
                if client.read_tls(&mut sock).is_err() {
                    panic!("client read_tls failed; buffered: {buf:?}");
                }
                client.process_new_packets().unwrap();
            }
            match client.reader().read(&mut tmp) {
                Ok(0) => {
                    if !client.wants_read() {
                        break;
                    }
                }
                Ok(n) => {
                    buf.extend_from_slice(&tmp[..n]);
                    if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
                Err(e) => panic!("client read failed: {e}; buffered: {buf:?}"),
            }
        }

        let status_line = std::str::from_utf8(&buf)
            .unwrap_or("")
            .lines()
            .next()
            .unwrap_or("");
        assert!(
            status_line.contains("101"),
            "expected 101 Switching Protocols, got: {status_line:?}; full response: {:?}",
            String::from_utf8_lossy(&buf)
        );

        let server_fd = server.join().expect("server thread");
        assert!(
            server_fd >= 0,
            "karac_runtime_ws_accept_tls returned {server_fd} (expected non-negative fd)"
        );

        // Cleanup: close the upgraded connection fd.
        karac_runtime_tls_close(server_fd);
        unsafe { karac_runtime_tls_config_free(cfg) };
    }

    /// **p50 handshake-latency probe (`#[ignore]`d — run by hand).**
    /// Drives the *real* `karac_runtime_ws_accept_tls` server path with
    /// N sequential client connects, timing each full establish (TCP
    /// connect → rustls handshake → RFC 6455 upgrade → 101 received —
    /// the same "connection established" boundary the bench harness
    /// measures), and prints connect-latency percentiles. Sequential by
    /// design: the ~40 ms Nagle×delayed-ACK handshake stall is a
    /// *per-connection fixed cost* that is clearest with no concurrent
    /// traffic to piggyback ACKs, so this isolates the latency *floor*
    /// (the bench's flat p50), not establishment throughput.
    ///
    /// Falsification protocol for the missing-`TCP_NODELAY` hypothesis:
    /// run this once on the current build (server omits `set_nodelay`),
    /// note p50; then with the one-line server-side `set_nodelay(true)`
    /// in `karac_runtime_ws_accept_tls`'s accept-drain loop, run again.
    /// A floor that collapses isolates Nagle as the cause. (Caveat:
    /// loopback delayed-ACK timing is platform-dependent — a *positive*
    /// delta here is decisive; a null result on macOS does not clear the
    /// hypothesis for the Linux rig.)
    ///
    ///   cargo test -p karac-runtime --features tls --release \
    ///     -- --ignored --nocapture handshake_latency_probe
    ///
    /// `KARAC_PROBE_N` overrides the connection count (default 200).
    #[test]
    #[ignore = "latency probe; run by hand with --ignored --nocapture"]
    #[cfg(feature = "tls")]
    fn handshake_latency_probe() {
        use std::io::{Read, Write};
        use std::time::Instant;

        let n: usize = std::env::var("KARAC_PROBE_N")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(200);

        let (cert_pem, key_pem) = gen_test_cert();
        let cfg = unsafe {
            karac_runtime_tls_config_new(
                cert_pem.as_ptr(),
                cert_pem.len() as i64,
                key_pem.as_ptr(),
                key_pem.len() as i64,
            )
        };
        assert!(!cfg.is_null());

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let cfg_addr = cfg as usize;
        let listener_fd = tcplistener_into_key(listener) as i64;

        // Server: accept + handshake N connections through the real FFI,
        // closing each upgraded fd so sessions don't accumulate.
        let server = thread::spawn(move || {
            for _ in 0..n {
                let fd = unsafe {
                    crate::event_loop::karac_runtime_ws_accept_tls(listener_fd, cfg_addr as *mut _)
                };
                if fd < 0 {
                    break;
                }
                karac_runtime_tls_close(fd);
            }
        });

        // Let the worker pool spin up + flip the listener non-blocking.
        thread::sleep(Duration::from_millis(50));

        let client_cfg = build_test_client_config(&cert_pem);
        let server_name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
        let req = b"GET / HTTP/1.1\r\n\
                    Host: localhost\r\n\
                    Upgrade: websocket\r\n\
                    Connection: Upgrade\r\n\
                    Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
                    Sec-WebSocket-Version: 13\r\n\
                    \r\n";

        let mut samples_us: Vec<u128> = Vec::with_capacity(n);
        for _ in 0..n {
            let start = Instant::now();
            let mut client =
                rustls::ClientConnection::new(Arc::clone(&client_cfg), server_name.clone())
                    .unwrap();
            let mut sock = TcpStream::connect(("127.0.0.1", port)).unwrap();
            client.complete_io(&mut sock).unwrap();
            client.writer().write_all(req).unwrap();
            while client.wants_write() {
                client.write_tls(&mut sock).unwrap();
            }
            // Read until the 101 terminator — connection "established".
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut buf = Vec::with_capacity(256);
            let mut tmp = [0u8; 256];
            loop {
                assert!(Instant::now() < deadline, "probe: no 101 within 5s");
                if client.wants_read() {
                    client.read_tls(&mut sock).unwrap();
                    client.process_new_packets().unwrap();
                }
                match client.reader().read(&mut tmp) {
                    Ok(0) if !client.wants_read() => break,
                    Ok(0) => {}
                    Ok(n) => {
                        buf.extend_from_slice(&tmp[..n]);
                        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(e) => panic!("probe: client read failed: {e}"),
                }
            }
            samples_us.push(start.elapsed().as_micros());
            drop(sock);
        }

        server.join().expect("probe server thread");
        unsafe { karac_runtime_tls_config_free(cfg) };

        samples_us.sort_unstable();
        let pct = |p: f64| -> f64 {
            let idx = ((p / 100.0) * (samples_us.len() as f64 - 1.0)).round() as usize;
            samples_us[idx] as f64 / 1000.0
        };
        let mean = samples_us.iter().sum::<u128>() as f64 / samples_us.len() as f64 / 1000.0;
        eprintln!(
            "handshake_latency_probe: n={} connects (sequential, loopback TLS+WS)\n  \
             mean {:.2} ms | p50 {:.2} ms | p90 {:.2} ms | p99 {:.2} ms | max {:.2} ms",
            samples_us.len(),
            mean,
            pct(50.0),
            pct(90.0),
            pct(99.0),
            pct(100.0),
        );
    }

    /// Phase-8 line 22 — same round trip as `round_trip_echo`, but the
    /// CLIENT side now goes through `karac_runtime_tls_client_connect`
    /// and the shared `karac_runtime_tls_read` / `_write` / `_close`
    /// FFIs instead of driving a `rustls::ClientConnection` directly.
    /// What this pins: the client handshake completes; the resulting
    /// fd registers in the shared `SESSIONS` map (the
    /// `Connection::Client` variant); the existing read/write paths
    /// handle the client direction because `drive_read` / `drive_write`
    /// operate on the `rustls::Connection` enum.
    #[test]
    fn round_trip_via_client_connect_ffi() {
        let (cert_pem, key_pem) = gen_test_cert();
        let cfg = unsafe {
            karac_runtime_tls_config_new(
                cert_pem.as_ptr(),
                cert_pem.len() as i64,
                key_pem.as_ptr(),
                key_pem.len() as i64,
            )
        };
        assert!(!cfg.is_null());

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let cfg_addr = cfg as usize;
        let listener_fd = tcplistener_into_key(listener) as i64;
        let server = thread::spawn(move || {
            let fd = unsafe { karac_runtime_tls_accept(listener_fd, cfg_addr as *mut _) };
            assert!(fd >= 0, "tls_accept failed");
            let mut buf = vec![0u8; 64];
            let n = unsafe { karac_runtime_tls_read(fd, buf.as_mut_ptr(), buf.len() as i64) };
            assert!(n > 0, "server tls_read returned {}", n);
            let w = unsafe { karac_runtime_tls_write(fd, buf.as_ptr(), n) };
            assert_eq!(w, n);
            karac_runtime_tls_close(fd);
            let _ = unsafe { tcplistener_from_key(listener_fd as SessionKey) };
            n as usize
        });

        thread::sleep(Duration::from_millis(50));

        // Client side via the NEW FFI.
        let addr = format!("127.0.0.1:{port}");
        let server_name = "localhost";
        let client_fd = unsafe {
            karac_runtime_tls_client_connect(
                addr.as_ptr(),
                addr.len() as i64,
                server_name.as_ptr(),
                server_name.len() as i64,
                cert_pem.as_ptr(),
                cert_pem.len() as i64,
            )
        };
        assert!(client_fd >= 0, "tls_client_connect failed: {client_fd}");

        let msg = b"hello via client-connect ffi";
        let w = unsafe { karac_runtime_tls_write(client_fd, msg.as_ptr(), msg.len() as i64) };
        assert_eq!(w, msg.len() as i64, "client tls_write returned {}", w);

        let mut echo = vec![0u8; msg.len()];
        let n = unsafe { karac_runtime_tls_read(client_fd, echo.as_mut_ptr(), echo.len() as i64) };
        assert!(n > 0, "client tls_read returned {}", n);
        assert_eq!(&echo[..n as usize], &msg[..n as usize], "echo mismatch");

        karac_runtime_tls_close(client_fd);
        let server_n = server.join().unwrap();
        assert_eq!(server_n, msg.len());

        unsafe { karac_runtime_tls_config_free(cfg) };
    }

    /// `karac_runtime_tls_client_connect` returns `-1` when the
    /// destination address fails to parse — earliest failure point,
    /// before any PEM parsing or TCP work.
    #[test]
    fn client_connect_with_bad_addr_returns_minus_one() {
        let (cert_pem, _key_pem) = gen_test_cert();
        let bad = b"not-an-address";
        let name = b"localhost";
        let fd = unsafe {
            karac_runtime_tls_client_connect(
                bad.as_ptr(),
                bad.len() as i64,
                name.as_ptr(),
                name.len() as i64,
                cert_pem.as_ptr(),
                cert_pem.len() as i64,
            )
        };
        assert_eq!(fd, -1);
    }

    /// `karac_runtime_tls_client_connect` returns `-1` when the
    /// supplied roots PEM doesn't contain any usable certificate
    /// (parse succeeds but yields no certs → `NoCertsFound`).
    #[test]
    fn client_connect_with_garbage_roots_returns_minus_one() {
        // Use a port that is almost certainly closed so even if PEM
        // parsing succeeded the connect would fail — but the PEM
        // failure should fire first.
        let addr = b"127.0.0.1:1";
        let name = b"localhost";
        let roots = b"not a pem certificate";
        let fd = unsafe {
            karac_runtime_tls_client_connect(
                addr.as_ptr(),
                addr.len() as i64,
                name.as_ptr(),
                name.len() as i64,
                roots.as_ptr(),
                roots.len() as i64,
            )
        };
        assert_eq!(fd, -1);
    }

    #[test]
    fn handshake_failure_returns_minus_one() {
        let (cert_pem, key_pem) = gen_test_cert();
        let cfg = unsafe {
            karac_runtime_tls_config_new(
                cert_pem.as_ptr(),
                cert_pem.len() as i64,
                key_pem.as_ptr(),
                key_pem.len() as i64,
            )
        };
        assert!(!cfg.is_null());

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let listener_fd = tcplistener_into_key(listener) as i64;
        let cfg_addr = cfg as usize;

        let server = thread::spawn(move || {
            let fd = unsafe { karac_runtime_tls_accept(listener_fd, cfg_addr as *mut _) };
            // Listener cleanup so the kernel reclaims the port.
            let _ = unsafe { tcplistener_from_key(listener_fd as SessionKey) };
            fd
        });

        // Connect with raw TCP and send garbage — the TLS handshake
        // can't decode it and returns an error.
        thread::sleep(Duration::from_millis(50));
        let mut sock = TcpStream::connect(("127.0.0.1", port)).unwrap();
        sock.write_all(b"this is definitely not a TLS ClientHello message body")
            .unwrap();
        // Close to ensure the server stops waiting for more bytes.
        drop(sock);

        let fd = server.join().unwrap();
        assert_eq!(fd, -1, "garbage client should fail handshake");

        unsafe { karac_runtime_tls_config_free(cfg) };
    }

    #[test]
    fn read_write_on_unknown_fd_returns_minus_one() {
        // A high fd that's not in the registry.
        let mut buf = [0u8; 16];
        let r = unsafe { karac_runtime_tls_read(99999, buf.as_mut_ptr(), buf.len() as i64) };
        assert_eq!(r, -1);
        let w = unsafe { karac_runtime_tls_write(99999, buf.as_ptr(), buf.len() as i64) };
        assert_eq!(w, -1);
    }

    #[test]
    fn close_on_negative_fd_is_noop() {
        let r = karac_runtime_tls_close(-1);
        assert_eq!(r, 0);
    }

    #[test]
    fn listener_bind_delegates_to_tcp_bind() {
        // Bind to ephemeral port via the TLS FFI; verify it returns a
        // valid (positive) fd. Config can be null — listener_bind
        // doesn't read it (handshake is per-connection).
        let addr = "127.0.0.1:0";
        let fd = unsafe {
            karac_runtime_tls_listener_bind(addr.as_ptr(), addr.len() as i64, std::ptr::null_mut())
        };
        assert!(fd >= 0);
        let _ = unsafe { tcplistener_from_key(fd as SessionKey) };
    }
}
