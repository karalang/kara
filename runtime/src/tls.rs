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
//! `RwLock<HashMap<i32, Arc<Mutex<TlsSession>>>>` keyed by TCP fd. Read/
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
use rustls::{ServerConfig, ServerConnection};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

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
pub(crate) struct TlsSession {
    pub(crate) conn: ServerConnection,
}

type SessionRegistry = RwLock<HashMap<i32, Arc<Mutex<TlsSession>>>>;

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
pub(crate) fn lookup_session(fd: i32) -> Option<Arc<Mutex<TlsSession>>> {
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
pub(crate) fn register_session_for_fd(fd: i32, conn: ServerConnection) {
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
/// Unix-only — matches the `#[cfg(unix)]` gate on the rest of the
/// raw-fd FFI surface.
///
/// # Safety
///
/// `addr_ptr` must point to `addr_len` readable bytes (UTF-8 socket
/// address string). Buffer is read once and not retained.
#[cfg(unix)]
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_tls_listener_bind(
    addr_ptr: *const u8,
    addr_len: i64,
    _config: *mut KaracTlsConfig,
) -> i32 {
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
#[cfg(unix)]
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_tls_accept(
    listener_fd: i32,
    config: *mut KaracTlsConfig,
) -> i32 {
    use std::os::unix::io::{FromRawFd, IntoRawFd};

    if listener_fd < 0 || config.is_null() {
        return -1;
    }
    let cfg = &*config;

    let listener = std::net::TcpListener::from_raw_fd(listener_fd);
    let accept_result = listener.accept();
    let _ = listener.into_raw_fd();
    let (mut sock, _addr) = match accept_result {
        Ok(p) => p,
        Err(_) => return -1,
    };

    let mut conn = match ServerConnection::new(Arc::clone(&cfg.inner)) {
        Ok(c) => c,
        Err(_) => return -1,
    };

    if conn.complete_io(&mut sock).is_err() {
        // sock drops here, closing the underlying fd. `conn` drops too;
        // any state it allocated is reclaimed.
        return -1;
    }

    let fd = sock.into_raw_fd();
    let mut reg = sessions().write().unwrap_or_else(|p| p.into_inner());
    reg.insert(fd, Arc::new(Mutex::new(TlsSession { conn })));
    fd
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
#[cfg(unix)]
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_tls_read(
    stream_fd: i32,
    buf_ptr: *mut u8,
    buf_len: i64,
) -> i64 {
    use std::os::unix::io::{FromRawFd, IntoRawFd};

    if stream_fd < 0 {
        return -1;
    }
    if buf_ptr.is_null() || buf_len <= 0 {
        return 0;
    }

    let session = {
        let reg = sessions().read().unwrap_or_else(|p| p.into_inner());
        match reg.get(&stream_fd) {
            Some(s) => Arc::clone(s),
            None => return -1,
        }
    };

    let mut sess = session.lock().unwrap_or_else(|p| p.into_inner());
    let mut sock = std::net::TcpStream::from_raw_fd(stream_fd);
    let result = drive_read(&mut sess.conn, &mut sock, buf_ptr, buf_len as usize);
    let _ = sock.into_raw_fd();
    result
}

/// Read-side driver. Loops between rustls's `reader().read()` (which
/// hands back decrypted plaintext) and `read_tls` / `process_new_packets`
/// (which pull more ciphertext from the socket) until plaintext is
/// available or a terminal condition fires.
fn drive_read(
    conn: &mut ServerConnection,
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
#[cfg(unix)]
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_tls_write(
    stream_fd: i32,
    buf_ptr: *const u8,
    buf_len: i64,
) -> i64 {
    use std::os::unix::io::{FromRawFd, IntoRawFd};

    if stream_fd < 0 {
        return -1;
    }
    if buf_ptr.is_null() || buf_len <= 0 {
        return 0;
    }

    let session = {
        let reg = sessions().read().unwrap_or_else(|p| p.into_inner());
        match reg.get(&stream_fd) {
            Some(s) => Arc::clone(s),
            None => return -1,
        }
    };

    let mut sess = session.lock().unwrap_or_else(|p| p.into_inner());
    let buf = std::slice::from_raw_parts(buf_ptr, buf_len as usize);
    let mut sock = std::net::TcpStream::from_raw_fd(stream_fd);
    let result = drive_write(&mut sess.conn, &mut sock, buf);
    let _ = sock.into_raw_fd();
    result
}

fn drive_write(conn: &mut ServerConnection, sock: &mut std::net::TcpStream, buf: &[u8]) -> i64 {
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
#[cfg(unix)]
#[no_mangle]
pub extern "C" fn karac_runtime_tls_close(fd: i32) -> i32 {
    use std::os::unix::io::FromRawFd;

    if fd < 0 {
        return 0;
    }
    {
        let mut reg = sessions().write().unwrap_or_else(|p| p.into_inner());
        reg.remove(&fd);
    }
    // SAFETY: same convention as karac_runtime_tcp_close — reconstruct
    // a TcpStream from the raw fd and let Drop run close(2) on it.
    let _ = unsafe { std::net::TcpStream::from_raw_fd(fd) };
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
#[cfg(unix)]
mod tests {
    //! Unit tests for the TLS FFI surface. End-to-end round-trip uses
    //! `rcgen` (dev-dep) to generate a self-signed cert at test time,
    //! sets up a real TCP listener + a separate thread for the server
    //! side, and connects a `rustls::ClientConnection` directly to
    //! exercise the wire protocol. Test-cert fixtures live under
    //! `tests/fixtures/tls/` (slice 4); the dev-dep generation here is
    //! the slice-1 hermetic-test approach.
    //!
    //! `#[cfg(unix)]` because the FFI surface itself is unix-gated.
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
        use std::os::unix::io::IntoRawFd;
        let listener_fd = listener.into_raw_fd();
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
            use std::os::unix::io::FromRawFd;
            let _ = unsafe { std::net::TcpListener::from_raw_fd(listener_fd) };
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
        use std::os::unix::io::{FromRawFd, IntoRawFd};
        let listener_fd = listener.into_raw_fd();
        let cfg_addr = cfg as usize;

        let server = thread::spawn(move || {
            let fd = unsafe { karac_runtime_tls_accept(listener_fd, cfg_addr as *mut _) };
            // Listener cleanup so the kernel reclaims the port.
            let _ = unsafe { std::net::TcpListener::from_raw_fd(listener_fd) };
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
        use std::os::unix::io::FromRawFd;
        let _ = unsafe { std::net::TcpListener::from_raw_fd(fd) };
    }
}
