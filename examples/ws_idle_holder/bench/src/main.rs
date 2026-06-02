//! Demo 1 slice 3 — bench-harness client for the `ws_idle_holder`
//! flagship demo (phase-6-runtime.md line 170 / 180).
//!
//! Opens N idle `wss://` WebSocket connections against the slice-1/2
//! server, holds them, and measures the three axes the slice plan calls
//! for:
//!   (a) connect-establishment latency — p50 / p95 / p99 / p99.9 (+
//!       min / max / mean), computed locally from the full sample set
//!       (no wrk dependency, so the high percentiles the wrk-based
//!       Parallax harness couldn't reach — see
//!       `docs/investigations/bench_robustness.md` G4 — are exact here);
//!   (b) steady-state RSS / per-connection memory cost — reads the
//!       server's RSS before vs. after the N connections are held, and
//!       divides the delta by N;
//!   (c) P99 latency cliff under churn — closes then reopens a fraction
//!       of the held connections over several rounds, reporting the
//!       reconnect p99 and its ratio to the initial connect p99.
//!
//! Output: a single JSON object on stdout (everything human-readable
//! goes to stderr), so the CI benchmark gate (slice 7, line 168) can be
//! a pure parse-and-threshold check on the numbers.
//!
//! TLS: the demo serves a self-signed test cert (CN=localhost), so the
//! client disables certificate verification — standard for a loopback
//! test rig. This harness never deploys to a reachable address.

use std::net::SocketAddr;
use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use serde::Serialize;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpSocket, TcpStream};
use tokio::process::{Child, Command};
use tokio::sync::Semaphore;
use tokio::time::timeout;
use tokio_rustls::TlsConnector;

type BoxErr = Box<dyn std::error::Error + Send + Sync>;
type Tls = tokio_rustls::client::TlsStream<TcpStream>;

// RFC 6455 §4.1: any 16-byte base64 value is a valid client key; the
// server only echoes back SHA1(key + GUID). A constant is fine for an
// idle holder — the connection's identity, not the key's uniqueness, is
// what the bench measures.
const WS_KEY: &str = "dGhlIHNhbXBsZSBub25jZQ==";

// ── CLI ──────────────────────────────────────────────────────────────

struct Args {
    server_bin: Option<String>,
    addr: Option<String>,
    server_pid: Option<u32>,
    connections: usize,
    concurrency: usize,
    churn_rounds: usize,
    churn_fraction: f64,
    /// Hard cap on reconnects per churn round. 0 = no cap (use the raw
    /// `churn_fraction * N` value). The default 10_000 protects the
    /// 10 % `churn_fraction` default from producing 100 K-burst rounds
    /// at 1 M scale (which overstressed the server's listen backlog
    /// pre-fix and is closer to a synthetic stress event than a
    /// realistic idle-hold churn pattern). At small N the cap doesn't
    /// bind — 50 K × 0.1 = 5 K is well under 10 K — so the cliff-
    /// detection signal is preserved.
    churn_batch_cap: usize,
    hold_secs: u64,
    connect_timeout_ms: u64,
    server_name: String,
    /// Client source IPs to spread connections across (round-robin by
    /// connection index). Empty = let the kernel pick (single implicit
    /// 127.0.0.1 source). Each distinct loopback source IP gets its own
    /// ephemeral-port pool, so N source IPs multiply the connection
    /// ceiling past the ~28K single-tuple `ip_local_port_range` cap —
    /// the path to 100K+ on a single box without root to widen the
    /// range. See README § "Beating the loopback port cap".
    source_ips: Vec<String>,
    /// Active-traffic phase: after the N idle connections are established,
    /// drive request/response echo on this many of them for
    /// `active_secs`, measuring round-trip latency and per-conn memory
    /// under mixed load. 0 = skip the active phase (pure idle hold).
    active_conns: usize,
    /// Duration of the active-traffic phase, seconds.
    active_secs: u64,
    /// Payload bytes per active-traffic message.
    msg_bytes: usize,
    /// Messages per second per active connection (the decided profile is
    /// 1 msg/sec — a chat-app baseline).
    msg_rate: f64,
    /// Handshake-QPS (reconnect-storm) mode: when > 0, skip the idle hold
    /// and instead open+immediately-close connections as fast as
    /// `concurrency` allows for this many seconds, reporting sustained
    /// full-TLS+WS handshakes/sec. Mutually exclusive with the idle-hold
    /// flow.
    handshake_qps_secs: u64,
    /// Spread each active connection's send phase uniformly across the
    /// `1/msg_rate` interval (by connection index) instead of all
    /// connections firing on the same aligned tick. Default off
    /// reproduces a synchronized burst every interval; on approximates
    /// realistic (desynchronized) client arrival, which is what real
    /// WebSocket fleets look like — clients are not phase-locked to a
    /// global tick. Lets the active-traffic latency be read under
    /// realistic arrival vs. a worst-case synchronized burst.
    stagger_arrival: bool,
}

impl Default for Args {
    fn default() -> Self {
        Args {
            server_bin: None,
            addr: None,
            server_pid: None,
            connections: 100,
            concurrency: 256,
            churn_rounds: 3,
            churn_fraction: 0.1,
            churn_batch_cap: 10_000,
            hold_secs: 1,
            connect_timeout_ms: 10_000,
            server_name: "localhost".to_string(),
            source_ips: Vec::new(),
            active_conns: 0,
            active_secs: 10,
            msg_bytes: 128,
            msg_rate: 1.0,
            handshake_qps_secs: 0,
            stagger_arrival: false,
        }
    }
}

fn usage() -> &'static str {
    "\
ws-idle-holder-bench — open N idle wss:// connections, measure connect
latency, per-connection memory, and P99 cliff under churn.

USAGE:
  ws-idle-holder-bench (--server-bin <path> | --addr <host:port>) [options]

CONNECT TARGET (exactly one):
  --server-bin <path>   Spawn this binary, read BOUND_PORT=<n> from its
                        stdout, and measure its RSS. Server is killed on
                        exit. (The compiled `ws_idle_holder` demo.)
  --addr <host:port>    Connect to an already-running server. Pair with
                        --server-pid to enable RSS measurement.
  --server-pid <pid>    Server PID for RSS (only with --addr).

OPTIONS:
  -n, --connections <N>     idle connections to open      (default 100)
  --concurrency <N>         in-flight handshakes cap      (default 256)
  --churn-rounds <N>        close+reopen rounds; 0=off    (default 3)
  --churn-fraction <f>      fraction churned per round    (default 0.1)
  --churn-batch-cap <N>     max reconnects per round; 0=no cap (default 10000).
                            Caps `churn_fraction * N` so the 10% default
                            doesn't overstress the server at high N.
  --hold-secs <N>           settle time before final RSS  (default 1)
  --connect-timeout-ms <N>  per-connection deadline       (default 10000)
  --server-name <name>      TLS SNI name                  (default localhost)
  --source-ips <a,b,...>    client source IPs to spread connections over
                            (round-robin); each loopback IP has its own
                            ephemeral-port pool, so N IPs raise the
                            ceiling past the ~28K single-tuple port cap.
                            e.g. 127.0.0.2,127.0.0.3,127.0.0.4,127.0.0.5

ACTIVE-TRAFFIC (after the idle hold; measures mixed-load density + latency):
  --active-conns <N>        of the held conns, drive echo on this many  (default 0=off)
  --active-secs <N>         duration of the active phase                (default 10)
  --msg-bytes <N>           payload bytes per message                   (default 128)
  --msg-rate <f>            messages/sec per active connection          (default 1.0)
  --stagger-arrival         spread send phases across the interval (realistic,
                            desynchronized arrival) instead of a synchronized
                            burst every interval                        (default off)

HANDSHAKE-QPS (reconnect-storm; mutually exclusive with the idle hold):
  --handshake-qps-secs <N>  open+close as fast as --concurrency allows for N
                            seconds; report sustained TLS+WS handshakes/sec  (default 0=off)

  -h, --help                this help
"
}

fn parse_args() -> Result<Args, BoxErr> {
    let mut a = Args::default();
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut next = || it.next().ok_or_else(|| format!("missing value for {arg}"));
        match arg.as_str() {
            "--server-bin" => a.server_bin = Some(next()?),
            "--addr" => a.addr = Some(next()?),
            "--server-pid" => a.server_pid = Some(next()?.parse()?),
            "-n" | "--connections" => a.connections = next()?.parse()?,
            "--concurrency" => a.concurrency = next()?.parse()?,
            "--churn-rounds" => a.churn_rounds = next()?.parse()?,
            "--churn-fraction" => a.churn_fraction = next()?.parse()?,
            "--churn-batch-cap" => a.churn_batch_cap = next()?.parse()?,
            "--hold-secs" => a.hold_secs = next()?.parse()?,
            "--connect-timeout-ms" => a.connect_timeout_ms = next()?.parse()?,
            "--server-name" => a.server_name = next()?,
            "--source-ips" => {
                a.source_ips = next()?
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            }
            "--active-conns" => a.active_conns = next()?.parse()?,
            "--active-secs" => a.active_secs = next()?.parse()?,
            "--msg-bytes" => a.msg_bytes = next()?.parse()?,
            "--msg-rate" => a.msg_rate = next()?.parse()?,
            "--stagger-arrival" => a.stagger_arrival = true,
            "--handshake-qps-secs" => a.handshake_qps_secs = next()?.parse()?,
            "-h" | "--help" => {
                eprint!("{}", usage());
                std::process::exit(0);
            }
            other => return Err(format!("unknown arg: {other}\n\n{}", usage()).into()),
        }
    }
    if a.server_bin.is_none() && a.addr.is_none() {
        return Err(format!("must pass --server-bin or --addr\n\n{}", usage()).into());
    }
    if a.server_bin.is_some() && a.addr.is_some() {
        return Err("pass only one of --server-bin / --addr".into());
    }
    if a.concurrency == 0 {
        return Err("--concurrency must be > 0".into());
    }
    if a.handshake_qps_secs > 0 && a.active_conns > 0 {
        return Err(
            "--handshake-qps-secs and --active-conns are mutually exclusive \
                    (one skips the idle hold, the other runs on top of it)"
                .into(),
        );
    }
    // The Kāra demo's `recv_text` reads into a fixed 4096-byte buffer, so a
    // larger echo payload would be truncated server-side and the round-trip
    // read would desync. Cap to keep both impls honest at the same size.
    if a.active_conns > 0 && a.msg_bytes > 4096 {
        return Err(format!(
            "--msg-bytes {} exceeds the server's 4096-byte recv buffer",
            a.msg_bytes
        )
        .into());
    }
    if a.active_conns > 0 && a.msg_rate <= 0.0 {
        return Err("--msg-rate must be > 0 when --active-conns is set".into());
    }
    // Validate source IPs up front so a typo fails fast instead of
    // surfacing as a per-connection bind error mid-run.
    for ip in &a.source_ips {
        format!("{ip}:0")
            .parse::<SocketAddr>()
            .map_err(|e| format!("invalid --source-ips entry {ip:?}: {e}"))?;
    }
    Ok(a)
}

// ── TLS: no-verify client (self-signed loopback test cert) ───────────

#[derive(Debug)]
struct NoCertVerification {
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for NoCertVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

fn build_connector() -> Result<TlsConnector, BoxErr> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = Arc::new(NoCertVerification {
        provider: provider.clone(),
    });
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    Ok(TlsConnector::from(Arc::new(config)))
}

// ── Connection establishment (TCP + TLS + WS upgrade) ────────────────

async fn establish(
    connector: &TlsConnector,
    addr: &str,
    server_name: ServerName<'static>,
    source_ip: Option<&str>,
) -> Result<Tls, BoxErr> {
    let tcp = match source_ip {
        // Bind a specific loopback source IP so this connection draws
        // from that IP's own ephemeral-port pool (the multi-IP fan-out
        // that lifts the ~28K single-tuple port cap).
        Some(ip) => {
            let sock = TcpSocket::new_v4()?;
            sock.bind(format!("{ip}:0").parse()?)?;
            sock.connect(addr.parse()?).await?
        }
        None => TcpStream::connect(addr).await?,
    };
    tcp.set_nodelay(true).ok();
    let mut tls = connector.connect(server_name, tcp).await?;

    let req = format!(
        "GET / HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\n\
         Connection: Upgrade\r\nSec-WebSocket-Key: {WS_KEY}\r\n\
         Sec-WebSocket-Version: 13\r\n\r\n"
    );
    tls.write_all(req.as_bytes()).await?;
    tls.flush().await?;

    let mut buf: Vec<u8> = Vec::with_capacity(512);
    let mut tmp = [0u8; 512];
    loop {
        let n = tls.read(&mut tmp).await?;
        if n == 0 {
            return Err("connection closed before WebSocket upgrade response".into());
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if buf.len() > 16384 {
            return Err("upgrade response exceeded 16 KiB without header terminator".into());
        }
    }
    let status_line = std::str::from_utf8(&buf)
        .unwrap_or("")
        .lines()
        .next()
        .unwrap_or("");
    if !status_line.contains("101") {
        return Err(format!("expected 101 Switching Protocols, got: {status_line:?}").into());
    }
    Ok(tls)
}

/// Opens `count` connections with at most `concurrency` handshakes
/// in-flight. Returns the established streams (held open) and the
/// per-connection establishment latencies in milliseconds, plus the
/// failure count.
#[allow(clippy::too_many_arguments)]
async fn open_batch(
    connector: &TlsConnector,
    addr: &str,
    server_name: &ServerName<'static>,
    count: usize,
    concurrency: usize,
    connect_timeout: Duration,
    source_ips: &[String],
) -> (Vec<Tls>, Vec<f64>, usize, Vec<String>) {
    let sem = Arc::new(Semaphore::new(concurrency));
    let mut handles = Vec::with_capacity(count);
    for i in 0..count {
        let sem = sem.clone();
        let connector = connector.clone();
        let addr = addr.to_string();
        let sni = server_name.clone();
        // Round-robin the source IP by connection index so the load is
        // even across the pool; None when no --source-ips were given.
        let src_ip = if source_ips.is_empty() {
            None
        } else {
            Some(source_ips[i % source_ips.len()].clone())
        };
        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire_owned().await.expect("semaphore closed");
            let t0 = Instant::now();
            match timeout(
                connect_timeout,
                establish(&connector, &addr, sni, src_ip.as_deref()),
            )
            .await
            {
                Ok(Ok(stream)) => Ok((t0.elapsed(), stream)),
                Ok(Err(e)) => Err(e.to_string()),
                Err(_) => Err(format!(
                    "connect timed out after {}ms",
                    connect_timeout.as_millis()
                )),
            }
        }));
    }

    let mut streams = Vec::with_capacity(count);
    let mut latencies = Vec::with_capacity(count);
    let mut failed = 0usize;
    let mut errors: Vec<String> = Vec::new();
    for h in handles {
        match h.await {
            Ok(Ok((dur, stream))) => {
                latencies.push(dur.as_secs_f64() * 1000.0);
                streams.push(stream);
            }
            Ok(Err(e)) => {
                failed += 1;
                if errors.len() < 5 {
                    errors.push(e);
                }
            }
            Err(join_err) => {
                failed += 1;
                if errors.len() < 5 {
                    errors.push(format!("task panicked: {join_err}"));
                }
            }
        }
    }
    (streams, latencies, failed, errors)
}

// ── WebSocket client framing (active-traffic mode) ───────────────────
//
// The harness rolls its own RFC 6455 framing — it never pulled in
// tungstenite. For active traffic we only need to send text frames and
// read the echoed frame back. Client→server frames MUST be masked (RFC
// 6455 §5.3); server→client frames arrive unmasked. The masking key need
// not be cryptographically random on a loopback rig — the server XORs
// with whatever key the frame carries — so a fixed key keeps the harness
// RNG-free.
const WS_MASK_KEY: [u8; 4] = [0x21, 0x9a, 0x4c, 0x7e];

/// Write a single masked WebSocket text frame carrying `payload`.
async fn ws_send_text_masked(stream: &mut Tls, payload: &[u8]) -> Result<(), BoxErr> {
    let mut frame: Vec<u8> = Vec::with_capacity(payload.len() + 14);
    frame.push(0x81); // FIN + text opcode (0x1)
    let len = payload.len();
    if len < 126 {
        frame.push(0x80 | (len as u8)); // MASK bit + 7-bit length
    } else if len <= u16::MAX as usize {
        frame.push(0x80 | 126);
        frame.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        frame.push(0x80 | 127);
        frame.extend_from_slice(&(len as u64).to_be_bytes());
    }
    frame.extend_from_slice(&WS_MASK_KEY);
    for (i, b) in payload.iter().enumerate() {
        frame.push(b ^ WS_MASK_KEY[i % 4]);
    }
    stream.write_all(&frame).await?;
    stream.flush().await?;
    Ok(())
}

/// Read one WebSocket data frame from the server and return its payload.
/// Control frames (ping/pong) are skipped; a close frame is an error.
/// Servers don't mask, but the unmask path is honored defensively.
async fn ws_read_frame(stream: &mut Tls) -> Result<Vec<u8>, BoxErr> {
    loop {
        let mut hdr = [0u8; 2];
        stream.read_exact(&mut hdr).await?;
        let opcode = hdr[0] & 0x0f;
        let masked = (hdr[1] & 0x80) != 0;
        let payload_len = match hdr[1] & 0x7f {
            126 => {
                let mut l = [0u8; 2];
                stream.read_exact(&mut l).await?;
                u16::from_be_bytes(l) as usize
            }
            127 => {
                let mut l = [0u8; 8];
                stream.read_exact(&mut l).await?;
                u64::from_be_bytes(l) as usize
            }
            n => n as usize,
        };
        let mut mask = [0u8; 4];
        if masked {
            stream.read_exact(&mut mask).await?;
        }
        let mut payload = vec![0u8; payload_len];
        if payload_len > 0 {
            stream.read_exact(&mut payload).await?;
        }
        if masked {
            for (i, b) in payload.iter_mut().enumerate() {
                *b ^= mask[i % 4];
            }
        }
        match opcode {
            0x1 | 0x2 => return Ok(payload), // text / binary
            0x8 => return Err("server sent a close frame".into()), // close
            0x9 | 0xa => continue,           // ping / pong — skip
            other => return Err(format!("unexpected ws opcode {other:#x}").into()),
        }
    }
}

// ── Active-traffic phase ─────────────────────────────────────────────

/// Aggregated result of the active-traffic phase.
struct ActiveOutcome {
    streams: Vec<Tls>,
    roundtrip_ms: Vec<f64>,
    sent: usize,
    echoed: usize,
    failed: usize,
}

/// Drive request/response echo on the given `active` streams for
/// `duration`: each connection sends a `msg_bytes` text frame every
/// `1/msg_rate` seconds and awaits the echo, recording the round-trip.
/// The streams are returned (kept alive) so the caller can hold them for
/// the post-phase RSS read and a clean teardown.
async fn run_active_traffic(
    active: Vec<Tls>,
    msg_bytes: usize,
    msg_rate: f64,
    duration: Duration,
    stagger_arrival: bool,
) -> ActiveOutcome {
    let payload: Arc<Vec<u8>> = Arc::new(vec![b'k'; msg_bytes]);
    let interval = Duration::from_secs_f64(1.0 / msg_rate);
    let deadline = Instant::now() + duration;
    let n_active = active.len();

    let mut handles = Vec::with_capacity(active.len());
    for (idx, mut stream) in active.into_iter().enumerate() {
        let payload = payload.clone();
        // Phase offset for this connection: with --stagger-arrival, spread
        // the first send uniformly across [0, interval) by connection index
        // so the aggregate arrival is smooth rather than a synchronized
        // burst on every tick. Without it, all connections share a phase
        // (tokio's interval fires its first tick immediately), so each
        // interval is a thundering-herd burst — the worst case, not a
        // realistic client population.
        let phase = if stagger_arrival && n_active > 0 {
            interval.mul_f64(idx as f64 / n_active as f64)
        } else {
            Duration::ZERO
        };
        handles.push(tokio::spawn(async move {
            let mut lats: Vec<f64> = Vec::new();
            let (mut sent, mut echoed, mut failed) = (0usize, 0usize, 0usize);
            if !phase.is_zero() {
                tokio::time::sleep(phase).await;
            }
            let mut tick = tokio::time::interval(interval);
            // Don't fire a burst to catch up if a tick is missed — pace it.
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                if Instant::now() >= deadline {
                    break;
                }
                let t0 = Instant::now();
                sent += 1;
                if ws_send_text_masked(&mut stream, &payload).await.is_err() {
                    failed += 1;
                    break;
                }
                match timeout(Duration::from_secs(10), ws_read_frame(&mut stream)).await {
                    Ok(Ok(_)) => {
                        echoed += 1;
                        lats.push(t0.elapsed().as_secs_f64() * 1000.0);
                    }
                    _ => {
                        failed += 1;
                        break;
                    }
                }
            }
            (stream, lats, sent, echoed, failed)
        }));
    }

    let mut out = ActiveOutcome {
        streams: Vec::with_capacity(handles.len()),
        roundtrip_ms: Vec::new(),
        sent: 0,
        echoed: 0,
        failed: 0,
    };
    for h in handles {
        match h.await {
            Ok((stream, lats, sent, echoed, failed)) => {
                out.streams.push(stream);
                out.roundtrip_ms.extend(lats);
                out.sent += sent;
                out.echoed += echoed;
                out.failed += failed;
            }
            Err(_) => out.failed += 1, // task panicked; the stream is gone
        }
    }
    out
}

// ── Handshake-QPS (reconnect-storm) mode ─────────────────────────────

/// For `duration`, open+immediately-close connections as fast as
/// `concurrency` allows, counting completed full TLS+WS handshakes.
/// Returns (completed, failed, per-handshake latencies in ms).
#[allow(clippy::too_many_arguments)]
async fn run_handshake_qps(
    connector: &TlsConnector,
    addr: &str,
    server_name: &ServerName<'static>,
    concurrency: usize,
    connect_timeout: Duration,
    source_ips: &[String],
    duration: Duration,
) -> (usize, usize, Vec<f64>) {
    let deadline = Instant::now() + duration;
    let sem = Arc::new(Semaphore::new(concurrency));
    let completed = Arc::new(AtomicUsize::new(0));
    let failed = Arc::new(AtomicUsize::new(0));
    let lats: Arc<Mutex<Vec<f64>>> = Arc::new(Mutex::new(Vec::new()));
    let mut idx = 0usize;

    // Spawn detached tasks bounded by the semaphore. Tasks update the
    // atomics + push their latency, so we don't retain a handle each (which
    // would grow unbounded over a long, high-QPS window).
    while Instant::now() < deadline {
        let permit = sem.clone().acquire_owned().await.expect("semaphore closed");
        let connector = connector.clone();
        let addr = addr.to_string();
        let sni = server_name.clone();
        let src_ip = if source_ips.is_empty() {
            None
        } else {
            Some(source_ips[idx % source_ips.len()].clone())
        };
        idx += 1;
        let (completed, failed, lats) = (completed.clone(), failed.clone(), lats.clone());
        tokio::spawn(async move {
            let _permit = permit; // released on task end
            let t0 = Instant::now();
            match timeout(
                connect_timeout,
                establish(&connector, &addr, sni, src_ip.as_deref()),
            )
            .await
            {
                Ok(Ok(stream)) => {
                    let dt = t0.elapsed().as_secs_f64() * 1000.0;
                    completed.fetch_add(1, Ordering::Relaxed);
                    if let Ok(mut v) = lats.lock() {
                        v.push(dt);
                    }
                    drop(stream); // immediate close — this is a churn workload
                }
                _ => {
                    failed.fetch_add(1, Ordering::Relaxed);
                }
            }
        });
    }

    // Drain: acquiring every permit means all in-flight tasks have ended.
    let _ = sem.acquire_many(concurrency as u32).await;
    let lats = Arc::try_unwrap(lats)
        .map(|m| m.into_inner().unwrap_or_default())
        .unwrap_or_default();
    (
        completed.load(Ordering::Relaxed),
        failed.load(Ordering::Relaxed),
        lats,
    )
}

// ── Stats ────────────────────────────────────────────────────────────

/// Nearest-rank percentile on an ascending-sorted slice. `p` in [0,100].
fn percentile(sorted: &[f64], p: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = (p / 100.0 * sorted.len() as f64).ceil() as usize;
    let idx = rank.saturating_sub(1).min(sorted.len() - 1);
    Some(sorted[idx])
}

#[derive(Serialize)]
struct LatencyStats {
    samples: usize,
    min_ms: Option<f64>,
    mean_ms: Option<f64>,
    p50_ms: Option<f64>,
    p95_ms: Option<f64>,
    p99_ms: Option<f64>,
    p999_ms: Option<f64>,
    max_ms: Option<f64>,
}

impl LatencyStats {
    fn from(mut v: Vec<f64>) -> Self {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let n = v.len();
        let mean = if n == 0 {
            None
        } else {
            Some(v.iter().sum::<f64>() / n as f64)
        };
        LatencyStats {
            samples: n,
            min_ms: v.first().copied(),
            mean_ms: mean,
            p50_ms: percentile(&v, 50.0),
            p95_ms: percentile(&v, 95.0),
            p99_ms: percentile(&v, 99.0),
            p999_ms: percentile(&v, 99.9),
            max_ms: v.last().copied(),
        }
    }
}

// ── RSS ──────────────────────────────────────────────────────────────

/// Resident set size of `pid` in KiB. Linux via `/proc/<pid>/status`
/// (`VmRSS:`), otherwise via `ps -o rss=` (macOS/BSD, also KiB).
fn read_rss_kb(pid: u32) -> Option<u64> {
    if let Ok(status) = std::fs::read_to_string(format!("/proc/{pid}/status")) {
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("VmRSS:") {
                return rest.split_whitespace().next()?.parse().ok();
            }
        }
    }
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

// ── Server lifecycle ─────────────────────────────────────────────────

/// Spawns the demo server, waits for `BOUND_PORT=<n>` on its stdout, and
/// returns the live child handle, the 127.0.0.1:<port> address, and PID.
async fn spawn_server(bin: &str) -> Result<(Child, String, Option<u32>), BoxErr> {
    let mut child = Command::new(bin)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("failed to spawn server {bin:?}: {e}"))?;
    let pid = child.id();
    let stdout = child.stdout.take().ok_or("server stdout not captured")?;
    let mut lines = BufReader::new(stdout).lines();

    let port: u16 = timeout(Duration::from_secs(15), async {
        while let Some(line) = lines.next_line().await? {
            if let Some(rest) = line.strip_prefix("BOUND_PORT=") {
                return rest.trim().parse::<u16>().map_err(BoxErr::from);
            }
        }
        Err("server exited before printing BOUND_PORT".into())
    })
    .await
    .map_err(|_| "timed out waiting 15s for server BOUND_PORT")??;

    // Keep draining stdout so the demo never blocks on a full pipe.
    tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });

    Ok((child, format!("127.0.0.1:{port}"), pid))
}

// ── Report ───────────────────────────────────────────────────────────

#[derive(Serialize)]
struct Config {
    target: String,
    connections: usize,
    concurrency: usize,
    churn_rounds: usize,
    churn_fraction: f64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    source_ips: Vec<String>,
}

#[derive(Serialize)]
struct ConnectReport {
    established: usize,
    failed: usize,
    #[serde(flatten)]
    latency: LatencyStats,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    sample_errors: Vec<String>,
}

#[derive(Serialize)]
struct MemoryReport {
    available: bool,
    server_pid: Option<u32>,
    rss_before_kb: Option<u64>,
    rss_after_kb: Option<u64>,
    per_conn_bytes: Option<f64>,
}

#[derive(Serialize)]
struct ChurnReport {
    rounds: usize,
    batch: usize,
    reconnect_established: usize,
    reconnect_failed: usize,
    #[serde(flatten)]
    latency: LatencyStats,
    /// reconnect p99 / initial-connect p99; >> 1 signals a cliff.
    cliff_ratio: Option<f64>,
}

#[derive(Serialize)]
struct ActiveTrafficReport {
    active_conns: usize,
    msg_bytes: usize,
    msg_rate: f64,
    duration_secs: u64,
    messages_sent: usize,
    messages_echoed: usize,
    echo_failures: usize,
    /// Round-trip latency (send → echo received), per message.
    #[serde(flatten)]
    roundtrip: LatencyStats,
    /// Server RSS while the active phase ran, and per-conn bytes derived
    /// from it (delta over the pre-active idle baseline / total held). The
    /// headline check is whether this stays within ~10% of the idle
    /// per-conn baseline — i.e. traffic doesn't blow up memory.
    rss_during_kb: Option<u64>,
    per_conn_bytes_active: Option<f64>,
}

#[derive(Serialize)]
struct HandshakeQpsReport {
    duration_secs: u64,
    concurrency: usize,
    handshakes_completed: usize,
    handshakes_failed: usize,
    /// Sustained full TLS+WS handshakes per second over the window.
    qps: f64,
    /// Per-handshake latency under the storm.
    #[serde(flatten)]
    latency: LatencyStats,
}

#[derive(Serialize)]
struct Report {
    ok: bool,
    config: Config,
    connect: ConnectReport,
    memory: MemoryReport,
    #[serde(skip_serializing_if = "Option::is_none")]
    churn: Option<ChurnReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    active_traffic: Option<ActiveTrafficReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    handshake_qps: Option<HandshakeQpsReport>,
}

// ── Main ─────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<(), BoxErr> {
    let args = parse_args()?;
    let connector = build_connector()?;
    let server_name = ServerName::try_from(args.server_name.clone())
        .map_err(|e| format!("invalid --server-name {:?}: {e}", args.server_name))?
        .to_owned();

    // Resolve target + server PID, spawning the demo if asked.
    let mut child: Option<Child> = None;
    let (addr, server_pid) = match (&args.server_bin, &args.addr) {
        (Some(bin), _) => {
            eprintln!("[bench] spawning server: {bin}");
            let (c, addr, pid) = spawn_server(bin).await?;
            eprintln!("[bench] server up at {addr} (pid {pid:?})");
            child = Some(c);
            (addr, pid)
        }
        (None, Some(addr)) => (addr.clone(), args.server_pid),
        _ => unreachable!("parse_args guarantees exactly one target"),
    };

    let rss_before = server_pid.and_then(read_rss_kb);
    let connect_timeout = Duration::from_millis(args.connect_timeout_ms);

    // Handshake-QPS (reconnect-storm) mode: skip the idle hold entirely.
    // Open+immediately-close connections as fast as --concurrency allows
    // for the window and report sustained full TLS+WS handshakes/sec — the
    // "reconnect storm survivability" number.
    if args.handshake_qps_secs > 0 {
        eprintln!(
            "[bench] handshake-QPS storm: {}s at concurrency {}...",
            args.handshake_qps_secs, args.concurrency
        );
        let storm_start = Instant::now();
        let (completed, hs_failed, lats) = run_handshake_qps(
            &connector,
            &addr,
            &server_name,
            args.concurrency,
            connect_timeout,
            &args.source_ips,
            Duration::from_secs(args.handshake_qps_secs),
        )
        .await;
        let elapsed = storm_start.elapsed().as_secs_f64();
        let qps = if elapsed > 0.0 {
            completed as f64 / elapsed
        } else {
            0.0
        };
        eprintln!("[bench] handshake-QPS: {completed} done, {hs_failed} failed, {qps:.0}/sec");
        let report = Report {
            ok: completed > 0 && hs_failed == 0,
            config: Config {
                target: addr,
                connections: args.connections,
                concurrency: args.concurrency,
                churn_rounds: args.churn_rounds,
                churn_fraction: args.churn_fraction,
                source_ips: args.source_ips.clone(),
            },
            // No conns are held in storm mode — the headline lives in the
            // handshake_qps section. Keep `connect` minimal but valid.
            connect: ConnectReport {
                established: 0,
                failed: 0,
                latency: LatencyStats::from(Vec::new()),
                sample_errors: Vec::new(),
            },
            memory: MemoryReport {
                available: false,
                server_pid,
                rss_before_kb: rss_before,
                rss_after_kb: None,
                per_conn_bytes: None,
            },
            churn: None,
            active_traffic: None,
            handshake_qps: Some(HandshakeQpsReport {
                duration_secs: args.handshake_qps_secs,
                concurrency: args.concurrency,
                handshakes_completed: completed,
                handshakes_failed: hs_failed,
                qps,
                latency: LatencyStats::from(lats),
            }),
        };
        if let Some(mut c) = child {
            let _ = c.kill().await;
        }
        println!("{}", serde_json::to_string_pretty(&report)?);
        if !report.ok {
            std::process::exit(1);
        }
        return Ok(());
    }

    eprintln!(
        "[bench] opening {} connections (concurrency {})...",
        args.connections, args.concurrency
    );
    let connect_start = Instant::now();
    let (mut held, latencies, failed, errors) = open_batch(
        &connector,
        &addr,
        &server_name,
        args.connections,
        args.concurrency,
        connect_timeout,
        &args.source_ips,
    )
    .await;
    let established = held.len();
    eprintln!(
        "[bench] established {}/{} in {:.2}s ({} failed)",
        established,
        args.connections,
        connect_start.elapsed().as_secs_f64(),
        failed
    );

    let connect_latency = LatencyStats::from(latencies);
    let initial_p99 = connect_latency.p99_ms;

    // Hold idle, then measure steady-state RSS.
    if args.hold_secs > 0 {
        tokio::time::sleep(Duration::from_secs(args.hold_secs)).await;
    }
    let rss_after = server_pid.and_then(read_rss_kb);
    let per_conn_bytes = match (rss_before, rss_after) {
        (Some(b), Some(a)) if established > 0 && a >= b => {
            Some((a - b) as f64 * 1024.0 / established as f64)
        }
        _ => None,
    };
    if let (Some(b), Some(a)) = (rss_before, rss_after) {
        eprintln!("[bench] server RSS {b} KiB -> {a} KiB (held {established} conns)");
    } else {
        eprintln!("[bench] server RSS unavailable (no server pid)");
    }

    // Active-traffic phase: drive request/response echo on a subset of the
    // held connections while the rest stay idle — the "1M idle + 10K
    // active" mixed-load profile. Measures round-trip latency under load
    // and whether per-conn memory stays near the idle baseline.
    let active_traffic = if args.active_conns > 0 && established > 0 {
        let n_active = args.active_conns.min(held.len());
        eprintln!(
            "[bench] active traffic: {n_active} conns x {:.2} msg/s x {}B for {}s...",
            args.msg_rate, args.msg_bytes, args.active_secs
        );
        // Peel off the active subset (the tail); the rest stay held idle.
        let active: Vec<Tls> = held.split_off(held.len() - n_active);
        let outcome = run_active_traffic(
            active,
            args.msg_bytes,
            args.msg_rate,
            Duration::from_secs(args.active_secs),
            args.stagger_arrival,
        )
        .await;
        // RSS at the end of the active phase vs the idle baseline (rss_before).
        let rss_during = server_pid.and_then(read_rss_kb);
        let per_conn_bytes_active = match (rss_before, rss_during) {
            (Some(b), Some(a)) if established > 0 && a >= b => {
                Some((a - b) as f64 * 1024.0 / established as f64)
            }
            _ => None,
        };
        eprintln!(
            "[bench] active traffic: {} sent / {} echoed / {} failed; rss_during {:?} KiB",
            outcome.sent, outcome.echoed, outcome.failed, rss_during
        );
        let roundtrip = LatencyStats::from(outcome.roundtrip_ms);
        let report = ActiveTrafficReport {
            active_conns: n_active,
            msg_bytes: args.msg_bytes,
            msg_rate: args.msg_rate,
            duration_secs: args.active_secs,
            messages_sent: outcome.sent,
            messages_echoed: outcome.echoed,
            echo_failures: outcome.failed,
            roundtrip,
            rss_during_kb: rss_during,
            per_conn_bytes_active,
        };
        // Return the active streams to `held` for a clean teardown.
        held.extend(outcome.streams);
        Some(report)
    } else {
        None
    };

    // Churn: close+reopen a fraction each round; watch the reconnect tail.
    let churn = if args.churn_rounds > 0 && established > 0 {
        let batch_from_fraction =
            ((established as f64 * args.churn_fraction).round() as usize).max(1);
        // Cap reconnects per round at `churn_batch_cap` so the 10 %
        // default doesn't produce 100 K-burst rounds at 1 M scale (which
        // overstressed the listen backlog pre-fix and is closer to a
        // synthetic stress event than a realistic idle-hold churn pattern).
        // 0 = no cap; preserves the raw fraction behavior.
        let batch = if args.churn_batch_cap > 0 {
            batch_from_fraction.min(args.churn_batch_cap)
        } else {
            batch_from_fraction
        };
        eprintln!(
            "[bench] churn: {} rounds x {} conns (close+reopen)...",
            args.churn_rounds, batch
        );
        let mut reconnect_lat: Vec<f64> = Vec::new();
        let mut re_failed = 0usize;
        for _ in 0..args.churn_rounds {
            let drop_n = batch.min(held.len());
            held.truncate(held.len() - drop_n); // dropping closes those sockets
            tokio::time::sleep(Duration::from_millis(50)).await;
            let (new_streams, lat, f, _e) = open_batch(
                &connector,
                &addr,
                &server_name,
                drop_n,
                args.concurrency,
                connect_timeout,
                &args.source_ips,
            )
            .await;
            re_failed += f;
            reconnect_lat.extend(lat);
            held.extend(new_streams);
        }
        let re_established = reconnect_lat.len();
        let stats = LatencyStats::from(reconnect_lat);
        let cliff_ratio = match (stats.p99_ms, initial_p99) {
            (Some(re), Some(init)) if init > 0.0 => Some(re / init),
            _ => None,
        };
        Some(ChurnReport {
            rounds: args.churn_rounds,
            batch,
            reconnect_established: re_established,
            reconnect_failed: re_failed,
            latency: stats,
            cliff_ratio,
        })
    } else {
        None
    };

    let report = Report {
        ok: established > 0
            && failed == 0
            && active_traffic.as_ref().is_none_or(|a| a.echo_failures == 0),
        config: Config {
            target: addr,
            connections: args.connections,
            concurrency: args.concurrency,
            churn_rounds: args.churn_rounds,
            churn_fraction: args.churn_fraction,
            source_ips: args.source_ips.clone(),
        },
        connect: ConnectReport {
            established,
            failed,
            latency: connect_latency,
            sample_errors: errors,
        },
        memory: MemoryReport {
            available: per_conn_bytes.is_some(),
            server_pid,
            rss_before_kb: rss_before,
            rss_after_kb: rss_after,
            per_conn_bytes,
        },
        churn,
        active_traffic,
        handshake_qps: None,
    };

    // Drop held connections before killing the server (clean close).
    drop(held);
    if let Some(mut c) = child {
        let _ = c.kill().await;
    }

    println!("{}", serde_json::to_string_pretty(&report)?);
    if !report.ok {
        std::process::exit(1);
    }
    Ok(())
}
