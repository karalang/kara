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

use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use serde::Serialize;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
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
    hold_secs: u64,
    connect_timeout_ms: u64,
    server_name: String,
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
            hold_secs: 1,
            connect_timeout_ms: 10_000,
            server_name: "localhost".to_string(),
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
  --hold-secs <N>           settle time before final RSS  (default 1)
  --connect-timeout-ms <N>  per-connection deadline       (default 10000)
  --server-name <name>      TLS SNI name                  (default localhost)
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
            "--hold-secs" => a.hold_secs = next()?.parse()?,
            "--connect-timeout-ms" => a.connect_timeout_ms = next()?.parse()?,
            "--server-name" => a.server_name = next()?,
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
) -> Result<Tls, BoxErr> {
    let tcp = TcpStream::connect(addr).await?;
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
async fn open_batch(
    connector: &TlsConnector,
    addr: &str,
    server_name: &ServerName<'static>,
    count: usize,
    concurrency: usize,
    connect_timeout: Duration,
) -> (Vec<Tls>, Vec<f64>, usize, Vec<String>) {
    let sem = Arc::new(Semaphore::new(concurrency));
    let mut handles = Vec::with_capacity(count);
    for _ in 0..count {
        let sem = sem.clone();
        let connector = connector.clone();
        let addr = addr.to_string();
        let sni = server_name.clone();
        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire_owned().await.expect("semaphore closed");
            let t0 = Instant::now();
            match timeout(connect_timeout, establish(&connector, &addr, sni)).await {
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
struct Report {
    ok: bool,
    config: Config,
    connect: ConnectReport,
    memory: MemoryReport,
    #[serde(skip_serializing_if = "Option::is_none")]
    churn: Option<ChurnReport>,
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

    eprintln!(
        "[bench] opening {} connections (concurrency {})...",
        args.connections, args.concurrency
    );
    let connect_timeout = Duration::from_millis(args.connect_timeout_ms);
    let connect_start = Instant::now();
    let (mut held, latencies, failed, errors) = open_batch(
        &connector,
        &addr,
        &server_name,
        args.connections,
        args.concurrency,
        connect_timeout,
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

    // Churn: close+reopen a fraction each round; watch the reconnect tail.
    let churn = if args.churn_rounds > 0 && established > 0 {
        let batch = ((established as f64 * args.churn_fraction).round() as usize).max(1);
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
        ok: established > 0 && failed == 0,
        config: Config {
            target: addr,
            connections: args.connections,
            concurrency: args.concurrency,
            churn_rounds: args.churn_rounds,
            churn_fraction: args.churn_fraction,
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
