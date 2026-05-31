//! Rust reference impl ("comparator") for the `ws_idle_holder` flagship
//! demo. Mirrors `../src/main.kara`: binds a TLS listener on
//! 127.0.0.1:<ephemeral>, prints `BOUND_PORT=<n>` to stdout so the bench
//! harness can read it back, and accepts WebSocket-over-TLS connections
//! in a loop, holding each idle until the peer closes. No echo — same
//! "idle holder" shape as the Kāra demo.
//!
//! ## Why this exists
//!
//! Phase-6-runtime.md line 170's M1/M2/M3 milestones name the
//! ws-idle-holder workload as the runtime's scale-target. A Rust
//! comparator built on the same network stack (tokio + rustls) gives
//! an "honest perf ceiling" against which the Kāra impl is measured:
//! both runtimes traverse the same kernel surface (`accept(2)`, TLS
//! handshake, RFC 6455 upgrade); the comparison isolates the
//! language-runtime overhead from the IO substrate.
//!
//! ## Design choices (vs. the Kāra impl)
//!
//! - **Async runtime:** `tokio` multi-thread. The Kāra runtime
//!   currently uses a synchronous per-connection handshake worker pool
//!   (`runtime/src/event_loop.rs::ws_handshake_pool_for`) rather than
//!   a fully-async server. The comparator uses the natural tokio
//!   shape (`tokio::spawn` per connection), which is what a competent
//!   Rust developer would write — not a hand-tuned mirror of Kāra's
//!   internal worker layout.
//! - **WS upgrade:** `tokio-tungstenite::accept_async` drives the
//!   RFC 6455 server-side handshake. Equivalent to the Kāra runtime's
//!   `ws_drive_upgrade_handshake` in `runtime/src/event_loop.rs`.
//! - **TLS:** rustls 0.23 with the `ring` crypto provider — exactly
//!   what the Kāra runtime uses (`runtime/src/tls.rs`). No
//!   aws-lc-rs, no native TLS, no openssl link. v1 protocol-versions
//!   default (TLS 1.2 + 1.3), no client auth.
//! - **Listen backlog:** `socket2` with `listen(65535)` on Linux,
//!   `listen(16384)` on macOS — matching the Kāra runtime's
//!   `karac_runtime_tcp_bind` so the comparison isn't distorted by a
//!   smaller listen queue causing the comparator to spuriously hit
//!   SYN-cookie fallback at high concurrency. Linux + macOS values
//!   match `runtime/src/event_loop.rs::KARAC_RUNTIME_TCP_LISTEN_BACKLOG`
//!   (see also `reference_macos_listen_backlog_cap` in user memory).
//! - **Cert + key:** loaded via `include_str!` from
//!   `tests/fixtures/tls/{cert,key}.pem` — the same self-signed test
//!   fixtures the Kāra demo inlines as PEM string literals. Loading
//!   from disk at compile-time keeps the two impls source-equivalent
//!   without re-pasting the PEM.
//!
//! ## Echo on message (Phase 2)
//!
//! The per-connection task echoes any text/binary frame it receives
//! straight back, mirroring the Kāra demo's `handle_connection`. This
//! lets the active-traffic bench drive request/response load through both
//! impls identically. Idle connections send nothing, so the echo branch
//! is never reached on an idle hold — the per-connection density numbers
//! are unchanged, and the same binary serves both idle and active loads
//! (so per-conn memory under traffic stays comparable to the idle
//! baseline with no cross-binary confound).
//!
//! ## What this impl deliberately omits
//!
//! - No structured logging.
//! - No graceful shutdown / max-conn cap.

use std::io::{self, Write};
use std::net::SocketAddr;
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::ServerConfig;
use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::accept_async;

/// Self-signed test cert (CN=localhost, valid through 2036) — same
/// fixture the Kāra demo inlines. Loading from disk at build time means
/// the two impls share a single source-of-truth.
const CERT_PEM: &str = include_str!("../../../../tests/fixtures/tls/cert.pem");
const KEY_PEM: &str = include_str!("../../../../tests/fixtures/tls/key.pem");

/// Listen-queue depth handed to `listen(2)`. Matches the Kāra runtime's
/// per-OS values in `runtime/src/event_loop.rs`:
/// `KARAC_RUNTIME_TCP_LISTEN_BACKLOG`. macOS silently breaks loopback
/// acceptance above ~16384 even when `kern.ipc.somaxconn` is raised
/// (see `reference_macos_listen_backlog_cap`).
#[cfg(target_os = "macos")]
const LISTEN_BACKLOG: i32 = 16384;
#[cfg(not(target_os = "macos"))]
const LISTEN_BACKLOG: i32 = 65535;

fn build_server_config() -> Result<ServerConfig, Box<dyn std::error::Error + Send + Sync>> {
    let certs: Vec<CertificateDer<'static>> =
        rustls_pemfile::certs(&mut CERT_PEM.as_bytes()).collect::<Result<Vec<_>, _>>()?;
    let key: PrivateKeyDer<'static> = rustls_pemfile::private_key(&mut KEY_PEM.as_bytes())?
        .ok_or("no private key in test fixture")?;

    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let cfg = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
    Ok(cfg)
}

/// Bind a TCP listener on 127.0.0.1:0 with an explicit `listen(2)`
/// backlog matching the Kāra runtime. tokio's
/// `TcpListener::bind` uses std's default backlog (1024 on Unix),
/// which is too small for the M3-class N=1M runs the kara side is
/// tuned for; using `socket2` directly lets us match.
fn bind_listener() -> io::Result<std::net::TcpListener> {
    let sock = Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP))?;
    sock.set_reuse_address(true)?;
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    sock.bind(&addr.into())?;
    sock.listen(LISTEN_BACKLOG)?;
    Ok(sock.into())
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let config = build_server_config()?;
    let acceptor = TlsAcceptor::from(Arc::new(config));

    let std_listener = bind_listener()?;
    let port = std_listener.local_addr()?.port();
    std_listener.set_nonblocking(true)?;
    let listener = TcpListener::from_std(std_listener)?;

    // BOUND_PORT line — same convention as the Kāra runtime's
    // `karac_runtime_tcp_bind`, read by the bench harness's
    // server-spawn path.
    writeln!(io::stdout(), "BOUND_PORT={port}")?;
    io::stdout().flush()?;

    loop {
        let (tcp, _peer) = listener.accept().await?;
        // Disable Nagle so the WS upgrade response goes out promptly,
        // matching the bench client's set_nodelay on the connect side.
        let _ = tcp.set_nodelay(true);

        let acceptor = acceptor.clone();
        tokio::spawn(async move {
            // TLS handshake.
            let tls = match acceptor.accept(tcp).await {
                Ok(s) => s,
                Err(_) => return,
            };
            // RFC 6455 upgrade.
            let ws = match accept_async(tls).await {
                Ok(s) => s,
                Err(_) => return,
            };
            // Echo on message; hold idle otherwise. Text/binary frames are
            // echoed straight back (the active-traffic bench's round-trip
            // path); ping/pong/close are left to tungstenite's defaults. An
            // idle connection sends nothing, so this awaits the eventual
            // close exactly as the pure idle holder did — density unchanged.
            let (mut writer, mut reader) = ws.split();
            while let Some(msg) = reader.next().await {
                match msg {
                    Ok(m) if m.is_text() || m.is_binary() => {
                        if writer.send(m).await.is_err() {
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
        });
    }
}
