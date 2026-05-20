//! Kernel runtime — message pump + per-msg-type dispatch.
//!
//! Slices 3 + 4 wire:
//!
//! - **`Kernel::run`** drives a single-threaded shell/control poll
//!   loop, decoding each multipart payload via
//!   [`wire::Message::decode`] and dispatching on `msg_type`.
//! - **`kernel_info_request`** handler — emits the
//!   `kernel_info_reply` on shell + busy/idle status broadcasts on
//!   IOPub. This is the minimum a Jupyter frontend needs to flip the
//!   "kernel ready" indicator.
//! - **`shutdown_request`** (control channel) — closes the
//!   transport so the loop exits cleanly.
//! - **`execute_request`** (slice 4) — routes the cell source
//!   through [`karac::repl::Session`] (`dispatch_magic` for `%`-
//!   prefixed cells, `evaluate_cell_captured` otherwise) and emits
//!   the canonical broadcast triad on IOPub: busy → execute_input
//!   → stream(stdout/stderr) → execute_result/error → idle.
//!
//! Slices 5+ extend the dispatch table (`complete_request`,
//! `is_complete_request`, `interrupt_request`) by adding match
//! arms; the pump itself stays the same.
//!
//! Heartbeat is a separate dedicated thread (see
//! `Kernel::spawn_heartbeat`) — bare echo loop, no signing, no
//! decoding. It cannot share the pump's thread because a long
//! execute_request would starve heartbeat liveness checks and the
//! frontend would mark the kernel dead.

#![allow(dead_code)]

use crate::transport::{Channel, Transport, TransportError};
use crate::wire::{Header, Message, Signer, PROTOCOL_VERSION};
use karac::repl::{ReplOptions, Session};
use serde_json::{json, Value as JsonValue};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Recv timeout on shell/control. Bounded so the loop can check
/// `transport.is_closed()` periodically without blocking forever.
const POLL_TIMEOUT: Duration = Duration::from_millis(250);

/// Recv timeout on the heartbeat REP socket — must be short relative
/// to the frontend's heartbeat ping interval (jupyter_client defaults
/// to 1 s); 100 ms keeps round-trip latency low while still letting
/// the thread observe shutdown promptly.
const HEARTBEAT_POLL_TIMEOUT: Duration = Duration::from_millis(100);

/// Kernel identity reported in `kernel_info_reply`. Lives outside the
/// pump so the in-memory and real-ZMQ entry points construct it the
/// same way.
#[derive(Debug, Clone)]
pub struct KernelInfo {
    /// Display name shown in JupyterLab's kernel picker.
    pub implementation: String,
    /// Semver string for this kernel binary.
    pub implementation_version: String,
    /// Language metadata block in the reply. Wraps name + version +
    /// MIME type + file extension + pygments lexer.
    pub language_name: String,
    pub language_version: String,
    pub banner: String,
}

impl KernelInfo {
    /// Default identity for the Kāra kernel. Pulls the binary's
    /// version from `CARGO_PKG_VERSION` and embeds the wire protocol
    /// version in the banner so users running `jupyter console`
    /// immediately see the protocol level they're talking to.
    pub fn default() -> Self {
        let version = env!("CARGO_PKG_VERSION").to_string();
        Self {
            implementation: "karac".to_string(),
            implementation_version: version.clone(),
            language_name: "kara".to_string(),
            language_version: version,
            banner: format!(
                "Kāra kernel (karac-kernel {} — Jupyter wire protocol {})",
                env!("CARGO_PKG_VERSION"),
                PROTOCOL_VERSION,
            ),
        }
    }
}

/// Internal classification of one cell's run result. Magic and
/// non-magic cells produce the same shape so the iopub broadcast
/// logic in `emit_cell_output` doesn't branch on cell flavor.
#[derive(Debug)]
enum CellOutcome {
    /// Run completed without surfacing diagnostics. `stderr` may
    /// still be non-empty (e.g. auto-clone `perf[…]` notes from a
    /// successful but rewritten cell).
    Ok {
        stdout: String,
        stderr: String,
        /// `EvaluatedCell::effect_footer` text. Empty for pure
        /// cells and magic cells; the broadcast logic suppresses the
        /// `stream` message when empty so notebooks stay quiet.
        effect_footer: String,
    },
    /// Run produced diagnostics or a magic-error reply. `evalue` is
    /// the first error line; `traceback` carries the full list.
    Error {
        stdout: String,
        stderr: String,
        ename: String,
        evalue: String,
        traceback: Vec<String>,
    },
}

/// One running kernel — owns the transport + the dispatch state.
/// Construct with [`Kernel::new`], hand to a thread that calls
/// [`Kernel::run`], drive shutdown by calling `transport.close()`
/// from any thread.
pub struct Kernel<T: Transport + 'static> {
    transport: Arc<T>,
    signer: Signer,
    info: KernelInfo,
    /// Session identifier emitted in every kernel-originated message
    /// header. Seeded once at startup; reused across reply +
    /// broadcast headers so the frontend can correlate them.
    session_id: String,
    /// Monotonically increasing per-process message counter for
    /// `msg_id` generation. We don't depend on the `uuid` crate —
    /// `<session>-<counter>` is unique within a session and matches
    /// `jupyter_client`'s fallback when UUID generation is
    /// unavailable.
    msg_counter: Mutex<u64>,
    /// REPL session driving cell evaluation. Wrapped in a `Mutex`
    /// because slice 5's `interrupt_request` handler will need to
    /// read state from a different thread than the pump; `Session`'s
    /// internal `Value` graph already uses `Arc<RwLock>` so it's
    /// `Send`, but `evaluate_cell_captured` takes `&mut self` so we
    /// need exclusive access for the duration of each cell.
    session: Mutex<Session>,
    /// 1-indexed per-cell counter, advanced on every non-silent
    /// `execute_request`. Atomic so future interrupt handlers can
    /// peek at "which cell is running" without taking the session
    /// lock.
    execution_count: AtomicU64,
}

impl<T: Transport + 'static> Kernel<T> {
    pub fn new(transport: Arc<T>, signer: Signer, info: KernelInfo) -> Self {
        Self::with_repl_options(transport, signer, info, ReplOptions::default())
    }

    /// Construct with explicit REPL options. Used by slice 6's Python
    /// shim to honor `--auto-clone` and other future flags forwarded
    /// from the kernelspec.
    pub fn with_repl_options(
        transport: Arc<T>,
        signer: Signer,
        info: KernelInfo,
        opts: ReplOptions,
    ) -> Self {
        let session_id = format!(
            "kara-kernel-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        Self {
            transport,
            signer,
            info,
            session_id,
            msg_counter: Mutex::new(0),
            session: Mutex::new(Session::with_options(opts)),
            execution_count: AtomicU64::new(0),
        }
    }

    /// Spawn the heartbeat thread. Bare REP echo loop with a short
    /// recv timeout so `transport.is_closed()` is checked promptly.
    /// Returns a join handle the caller can `.join()` on shutdown.
    pub fn spawn_heartbeat(&self) -> JoinHandle<()> {
        let transport = self.transport.clone();
        thread::Builder::new()
            .name("karac-kernel-heartbeat".to_string())
            .spawn(move || heartbeat_loop(transport))
            .expect("OS allows spawning heartbeat thread")
    }

    /// Drive the shell + control message pump. Blocks the calling
    /// thread until the transport closes. The pump alternates a
    /// timeout-bounded recv on shell then control so neither channel
    /// starves the other and shutdown is observed within
    /// `POLL_TIMEOUT` of `transport.close()`.
    pub fn run(&self) {
        while !self.transport.is_closed() {
            // Shell channel: ordinary requests.
            self.pump_once(Channel::Shell);
            if self.transport.is_closed() {
                break;
            }
            // Control channel: high-priority requests.
            self.pump_once(Channel::Control);
        }
    }

    /// Drive one iteration on `channel` — recv with the standard
    /// poll timeout, decode, dispatch. Errors are logged to stderr
    /// (the kernel can't surface them anywhere else — every other
    /// channel is downstream of the pump) and the loop continues so
    /// one bad message doesn't take the kernel down.
    fn pump_once(&self, channel: Channel) {
        match self.transport.recv(channel, Some(POLL_TIMEOUT)) {
            Ok(frames) => match Message::decode(&frames, &self.signer) {
                Ok(msg) => self.dispatch(channel, msg),
                Err(err) => {
                    eprintln!("karac-kernel: dropping malformed message on {channel:?}: {err}");
                }
            },
            Err(TransportError::Timeout { .. }) => {}
            Err(TransportError::Closed { .. }) => {}
            Err(err) => {
                eprintln!("karac-kernel: transport error on {channel:?}: {err}");
            }
        }
    }

    /// Dispatch a decoded request to its handler. Unknown message
    /// types are logged but not replied to — `jupyter_client`
    /// tolerates unknown msg_types as forward-compatibility (newer
    /// frontends may speak protocol versions this kernel doesn't
    /// know about).
    fn dispatch(&self, channel: Channel, msg: Message) {
        match msg.header.msg_type.as_str() {
            "kernel_info_request" => self.handle_kernel_info_request(channel, msg),
            "shutdown_request" => self.handle_shutdown_request(channel, msg),
            "execute_request" => self.handle_execute_request(channel, msg),
            other => {
                // Slices 5+ extend this list (`complete_request`,
                // `is_complete_request`, `interrupt_request`). Silent
                // would hide bugs; log to stderr so anyone wiring up
                // a frontend before those slices ship sees the gap.
                eprintln!(
                    "karac-kernel: unhandled msg_type {other:?} on {channel:?} \
                     (slice 4 ships kernel_info / shutdown / execute only)"
                );
            }
        }
    }

    fn handle_kernel_info_request(&self, channel: Channel, request: Message) {
        // IOPub: busy status before doing work.
        self.broadcast_status("busy", &request.header);

        // Shell/control: the actual reply.
        let content = json!({
            "status": "ok",
            "protocol_version": PROTOCOL_VERSION,
            "implementation": self.info.implementation,
            "implementation_version": self.info.implementation_version,
            "language_info": {
                "name": self.info.language_name,
                "version": self.info.language_version,
                "mimetype": "text/x-kara",
                "file_extension": ".kara",
                "pygments_lexer": "rust",
                "codemirror_mode": "rust",
            },
            "banner": self.info.banner,
            "help_links": [],
        });
        let reply = self.build_reply(&request, "kernel_info_reply", content);
        if let Err(err) = self.transport.send(channel, reply.encode(&self.signer)) {
            eprintln!("karac-kernel: failed to send kernel_info_reply: {err}");
        }

        // IOPub: idle status after the reply.
        self.broadcast_status("idle", &request.header);
    }

    fn handle_shutdown_request(&self, channel: Channel, request: Message) {
        let restart = request
            .content
            .get("restart")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false);
        let reply_content = json!({ "status": "ok", "restart": restart });
        let reply = self.build_reply(&request, "shutdown_reply", reply_content);
        if let Err(err) = self.transport.send(channel, reply.encode(&self.signer)) {
            eprintln!("karac-kernel: failed to send shutdown_reply: {err}");
        }
        self.transport.close();
    }

    /// Route an `execute_request` through the REPL session.
    ///
    /// Iopub broadcast order matches Jupyter's expected sequence so a
    /// frontend cleanly correlates everything to the originating
    /// cell:
    ///
    /// 1. `status: busy`
    /// 2. `execute_input { code, execution_count }` — unless `silent`
    /// 3. `stream(stdout)` with captured `println!` output (skipped
    ///    when empty)
    /// 4. `stream(stderr)` for diagnostic strings + auto-clone perf
    ///    notes (skipped when empty)
    /// 5. `stream(stdout)` with the effect footer (`writes(A) reads(B)`
    ///    — populated by `Session::compute_cell_effect_footer` only
    ///    on successful statement-cell runs, kept on stdout so it
    ///    shows under the cell rather than as a warning)
    /// 6. `error { ename, evalue, traceback }` — only on diagnostic
    ///    failure; the same payload rides the `execute_reply` shell
    ///    response
    /// 7. `status: idle`
    ///
    /// The shell-channel `execute_reply` carries `status` (`ok` /
    /// `error`) + `execution_count` so the frontend can render the
    /// `Out[N]` prompt before the next cell submits.
    fn handle_execute_request(&self, channel: Channel, request: Message) {
        let code = request
            .content
            .get("code")
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_string();
        let silent = request
            .content
            .get("silent")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false);

        // Silent requests don't bump the counter (per Jupyter spec —
        // they exist for `user_expressions`-style probes that the
        // frontend doesn't want appearing in the cell history).
        let execution_count = if silent {
            self.execution_count.load(Ordering::Acquire)
        } else {
            self.execution_count.fetch_add(1, Ordering::AcqRel) + 1
        };

        self.broadcast_status("busy", &request.header);

        if !silent {
            self.broadcast_iopub(
                "execute_input",
                &request.header,
                json!({
                    "code": code,
                    "execution_count": execution_count,
                }),
            );
        }

        let outcome = self.run_cell(&code);

        if !silent {
            self.emit_cell_output(&request.header, &outcome);
        }

        let reply_content = match &outcome {
            CellOutcome::Ok { .. } => json!({
                "status": "ok",
                "execution_count": execution_count,
                "payload": [],
                "user_expressions": {},
            }),
            CellOutcome::Error {
                ename,
                evalue,
                traceback,
                ..
            } => {
                // Iopub broadcast mirrors the shell error so other
                // frontends connected to the same kernel see the
                // failure without polling shell replies.
                if !silent {
                    self.broadcast_iopub(
                        "error",
                        &request.header,
                        json!({
                            "ename": ename,
                            "evalue": evalue,
                            "traceback": traceback,
                        }),
                    );
                }
                json!({
                    "status": "error",
                    "execution_count": execution_count,
                    "ename": ename,
                    "evalue": evalue,
                    "traceback": traceback,
                })
            }
        };

        let reply = self.build_reply(&request, "execute_reply", reply_content);
        if let Err(err) = self.transport.send(channel, reply.encode(&self.signer)) {
            eprintln!("karac-kernel: failed to send execute_reply: {err}");
        }

        self.broadcast_status("idle", &request.header);
    }

    /// Run one cell through the session. `%`-prefixed cells route
    /// through `dispatch_magic`; everything else goes through
    /// `evaluate_cell_captured`. The two paths produce a uniform
    /// [`CellOutcome`] so the broadcast logic above doesn't branch on
    /// cell shape.
    fn run_cell(&self, code: &str) -> CellOutcome {
        let trimmed = code.trim_start();
        if trimmed.starts_with('%') {
            // Magic surface from line 721 — slice 4's load-bearing
            // forward-wiring of `%effects` / `%ownership` / etc.
            let out = self.session.lock().unwrap().dispatch_magic(trimmed);
            if out.ok {
                CellOutcome::Ok {
                    stdout: out.text,
                    stderr: String::new(),
                    effect_footer: String::new(),
                }
            } else {
                CellOutcome::Error {
                    stdout: String::new(),
                    stderr: out.text.clone(),
                    ename: "MagicError".to_string(),
                    evalue: out.text.lines().next().unwrap_or("").to_string(),
                    traceback: out.text.lines().map(|l| l.to_string()).collect(),
                }
            }
        } else {
            let evaluated = self.session.lock().unwrap().evaluate_cell_captured(code);
            // `notes` carries `perf[auto-clone-in-repl]` lines —
            // never silent per the design spec, mirrored to stderr
            // alongside any diagnostic strings.
            let stderr = if evaluated.notes.is_empty() && evaluated.errors.is_empty() {
                String::new()
            } else {
                let mut buf = String::new();
                for e in &evaluated.errors {
                    buf.push_str(e);
                    buf.push('\n');
                }
                for n in &evaluated.notes {
                    buf.push_str(n);
                    buf.push('\n');
                }
                buf
            };
            if evaluated.errors.is_empty() {
                CellOutcome::Ok {
                    stdout: evaluated.stdout,
                    stderr,
                    effect_footer: evaluated.effect_footer,
                }
            } else {
                let evalue = evaluated.errors.first().cloned().unwrap_or_default();
                CellOutcome::Error {
                    stdout: evaluated.stdout,
                    stderr,
                    ename: "CompileError".to_string(),
                    evalue,
                    traceback: evaluated.errors,
                }
            }
        }
    }

    fn emit_cell_output(&self, parent: &Header, outcome: &CellOutcome) {
        let (stdout, stderr, effect_footer) = match outcome {
            CellOutcome::Ok {
                stdout,
                stderr,
                effect_footer,
            } => (stdout.as_str(), stderr.as_str(), effect_footer.as_str()),
            CellOutcome::Error { stdout, stderr, .. } => (stdout.as_str(), stderr.as_str(), ""),
        };
        if !stdout.is_empty() {
            self.broadcast_iopub(
                "stream",
                parent,
                json!({ "name": "stdout", "text": stdout }),
            );
        }
        if !stderr.is_empty() {
            self.broadcast_iopub(
                "stream",
                parent,
                json!({ "name": "stderr", "text": stderr }),
            );
        }
        if !effect_footer.is_empty() {
            // Trailing newline so the footer ends a line cleanly in
            // the cell-output pane — the same convention the REPL's
            // `:effects` meta-command applies on stdout.
            let mut text = effect_footer.to_string();
            if !text.ends_with('\n') {
                text.push('\n');
            }
            self.broadcast_iopub("stream", parent, json!({ "name": "stdout", "text": text }));
        }
    }

    fn broadcast_iopub(&self, msg_type: &str, parent: &Header, content: JsonValue) {
        let msg = self.build_iopub_broadcast(msg_type, parent, content);
        if let Err(err) = self
            .transport
            .send(Channel::IoPub, msg.encode(&self.signer))
        {
            eprintln!("karac-kernel: failed to broadcast {msg_type}: {err}");
        }
    }

    fn broadcast_status(&self, state: &str, parent: &Header) {
        self.broadcast_iopub("status", parent, json!({ "execution_state": state }));
    }

    /// Construct a reply message carrying the original
    /// `parent_header` along with the request's identity frames (so a
    /// ROUTER socket can route the reply back to the originating
    /// DEALER).
    fn build_reply(&self, request: &Message, msg_type: &str, content: JsonValue) -> Message {
        Message {
            identities: request.identities.clone(),
            header: self.new_header(&request.header.username, msg_type),
            parent_header: serde_json::to_value(&request.header).expect("Header serializes"),
            metadata: json!({}),
            content,
            buffers: vec![],
        }
    }

    /// Construct an IOPub broadcast message — no identity frames
    /// (PUB sockets don't route), but `parent_header` is the
    /// triggering request so frontends can correlate the broadcast.
    fn build_iopub_broadcast(
        &self,
        msg_type: &str,
        parent: &Header,
        content: JsonValue,
    ) -> Message {
        Message {
            identities: vec![],
            header: self.new_header(&parent.username, msg_type),
            parent_header: serde_json::to_value(parent).expect("Header serializes"),
            metadata: json!({}),
            content,
            buffers: vec![],
        }
    }

    fn new_header(&self, username: &str, msg_type: &str) -> Header {
        let counter = {
            let mut c = self.msg_counter.lock().unwrap();
            *c += 1;
            *c
        };
        Header {
            msg_id: format!("{}-{counter}", self.session_id),
            username: username.to_string(),
            session: self.session_id.clone(),
            msg_type: msg_type.to_string(),
            version: PROTOCOL_VERSION.to_string(),
            date: iso8601_now(),
        }
    }
}

fn heartbeat_loop<T: Transport>(transport: Arc<T>) {
    while !transport.is_closed() {
        match transport.recv(Channel::Heartbeat, Some(HEARTBEAT_POLL_TIMEOUT)) {
            Ok(frames) => {
                // REP socket: echo whatever the frontend sent.
                if let Err(err) = transport.send(Channel::Heartbeat, frames) {
                    eprintln!("karac-kernel: heartbeat send failed: {err}");
                    break;
                }
            }
            Err(TransportError::Timeout { .. }) => continue,
            Err(TransportError::Closed { .. }) => break,
            Err(err) => {
                eprintln!("karac-kernel: heartbeat recv error: {err}");
                break;
            }
        }
    }
}

/// Format the current UTC time as an ISO 8601 string with
/// microsecond precision — matches Python's `datetime.utcnow().isoformat()`
/// which is the shape `jupyter_client` emits.
///
/// Implementation note: we avoid pulling in `chrono` / `time` for one
/// timestamp; the calendar math is straightforward for UTC (no DST
/// shifts, no leap-second adjustments at this resolution).
fn iso8601_now() -> String {
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let total_secs = dur.as_secs();
    let micros = dur.subsec_micros();
    // Days since epoch + seconds within the day.
    let days = (total_secs / 86_400) as i64;
    let secs_in_day = total_secs % 86_400;
    let h = secs_in_day / 3600;
    let m = (secs_in_day % 3600) / 60;
    let s = secs_in_day % 60;
    let (year, month, day) = civil_date_from_days(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:06}",
        year, month, day, h, m, s, micros
    )
}

/// Convert "days since 1970-01-01" to (year, month, day) in the
/// proleptic Gregorian calendar. Algorithm from Howard Hinnant's
/// "date" paper (`days_from_civil` inverse), constant time, handles
/// negative days correctly (pre-1970 timestamps).
fn civil_date_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (year, m as u32, d as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::testing::InMemoryTransport;
    use crate::wire::{Message, Signer};
    use serde_json::json;

    fn build_request(
        signer: &Signer,
        msg_type: &str,
        content: JsonValue,
        identities: Vec<Vec<u8>>,
    ) -> Vec<Vec<u8>> {
        let msg = Message {
            identities,
            header: Header {
                msg_id: "test-msg-1".to_string(),
                username: "tester".to_string(),
                session: "test-session".to_string(),
                msg_type: msg_type.to_string(),
                version: PROTOCOL_VERSION.to_string(),
                date: "2026-05-19T00:00:00.000000".to_string(),
            },
            parent_header: json!({}),
            metadata: json!({}),
            content,
            buffers: vec![],
        };
        msg.encode(signer)
    }

    fn run_one_pass<T: Transport + 'static>(kernel: &Kernel<T>) {
        // Drive one shell-channel poll iteration. Used by tests
        // instead of `kernel.run()` so the test thread doesn't block
        // forever.
        kernel.pump_once(Channel::Shell);
    }

    #[test]
    fn kernel_info_request_round_trip() {
        let transport = Arc::new(InMemoryTransport::new());
        let signer = Signer::new("test-key");
        let kernel = Kernel::new(transport.clone(), signer.clone(), KernelInfo::default());

        // Frontend sends kernel_info_request on shell.
        let request = build_request(
            &signer,
            "kernel_info_request",
            json!({}),
            vec![b"client-A".to_vec()],
        );
        transport.push_incoming(Channel::Shell, request);

        run_one_pass(&kernel);

        // Verify the reply landed on shell.
        let outgoing_shell = transport.drain_outgoing(Channel::Shell);
        assert_eq!(outgoing_shell.len(), 1, "expected exactly one shell reply");
        let reply = Message::decode(&outgoing_shell[0], &signer).unwrap();
        assert_eq!(reply.header.msg_type, "kernel_info_reply");
        assert_eq!(reply.identities, vec![b"client-A".to_vec()]);
        assert_eq!(reply.content["status"], "ok");
        assert_eq!(reply.content["protocol_version"], PROTOCOL_VERSION);
        assert_eq!(reply.content["implementation"], "karac");
        assert_eq!(reply.content["language_info"]["name"], "kara");
        assert_eq!(reply.content["language_info"]["file_extension"], ".kara");
        // parent_header should be the request header so the frontend
        // can correlate.
        assert_eq!(reply.parent_header["msg_id"], "test-msg-1");
        assert_eq!(reply.parent_header["msg_type"], "kernel_info_request");
    }

    #[test]
    fn kernel_info_request_broadcasts_busy_then_idle_on_iopub() {
        let transport = Arc::new(InMemoryTransport::new());
        let signer = Signer::new("k");
        let kernel = Kernel::new(transport.clone(), signer.clone(), KernelInfo::default());

        let request = build_request(&signer, "kernel_info_request", json!({}), vec![]);
        transport.push_incoming(Channel::Shell, request);
        run_one_pass(&kernel);

        let iopub = transport.drain_outgoing(Channel::IoPub);
        assert_eq!(iopub.len(), 2, "expected busy + idle broadcast");
        let busy = Message::decode(&iopub[0], &signer).unwrap();
        let idle = Message::decode(&iopub[1], &signer).unwrap();
        assert_eq!(busy.header.msg_type, "status");
        assert_eq!(busy.content["execution_state"], "busy");
        assert_eq!(idle.header.msg_type, "status");
        assert_eq!(idle.content["execution_state"], "idle");
        // IOPub broadcasts carry the triggering header so the
        // frontend renders them under the right cell.
        assert_eq!(busy.parent_header["msg_id"], "test-msg-1");
        assert_eq!(idle.parent_header["msg_id"], "test-msg-1");
        // IOPub broadcasts have no identity frames.
        assert!(busy.identities.is_empty());
        assert!(idle.identities.is_empty());
    }

    #[test]
    fn shutdown_request_closes_transport_and_replies() {
        let transport = Arc::new(InMemoryTransport::new());
        let signer = Signer::new("k");
        let kernel = Kernel::new(transport.clone(), signer.clone(), KernelInfo::default());

        let request = build_request(
            &signer,
            "shutdown_request",
            json!({"restart": false}),
            vec![b"client-B".to_vec()],
        );
        transport.push_incoming(Channel::Control, request);
        kernel.pump_once(Channel::Control);

        let outgoing = transport.drain_outgoing(Channel::Control);
        assert_eq!(outgoing.len(), 1);
        let reply = Message::decode(&outgoing[0], &signer).unwrap();
        assert_eq!(reply.header.msg_type, "shutdown_reply");
        assert_eq!(reply.content["restart"], false);
        assert!(transport.is_closed());
    }

    #[test]
    fn shutdown_request_with_restart_echoes_restart_flag() {
        let transport = Arc::new(InMemoryTransport::new());
        let signer = Signer::new("");
        let kernel = Kernel::new(transport.clone(), signer.clone(), KernelInfo::default());
        let request = build_request(
            &signer,
            "shutdown_request",
            json!({"restart": true}),
            vec![],
        );
        transport.push_incoming(Channel::Control, request);
        kernel.pump_once(Channel::Control);
        let out = transport.drain_outgoing(Channel::Control);
        let reply = Message::decode(&out[0], &signer).unwrap();
        assert_eq!(reply.content["restart"], true);
    }

    /// Drain `IoPub` from an `InMemoryTransport` and return
    /// `(msg_type, decoded_message)` pairs in broadcast order so
    /// tests can assert both the sequence and per-message content
    /// without re-decoding inline.
    fn drain_iopub_in_memory(
        transport: &InMemoryTransport,
        signer: &Signer,
    ) -> Vec<(String, Message)> {
        transport
            .drain_outgoing(Channel::IoPub)
            .into_iter()
            .map(|frames| {
                let m = Message::decode(&frames, signer).unwrap();
                (m.header.msg_type.clone(), m)
            })
            .collect()
    }

    #[test]
    fn execute_request_simple_println_round_trip() {
        // Pure-expression / statement cells go through
        // `Session::evaluate_cell_captured`; captured stdout lands on
        // iopub as `stream(stdout)`, and the shell reply is `ok`.
        let transport = Arc::new(InMemoryTransport::new());
        let signer = Signer::new("k");
        let kernel = Kernel::new(transport.clone(), signer.clone(), KernelInfo::default());
        let request = build_request(
            &signer,
            "execute_request",
            json!({
                "code": "println(\"hello kara\");",
                "silent": false,
                "store_history": true,
                "user_expressions": {},
                "allow_stdin": false,
                "stop_on_error": false,
            }),
            vec![b"client-1".to_vec()],
        );
        transport.push_incoming(Channel::Shell, request);
        run_one_pass(&kernel);

        // Shell: one execute_reply with status=ok, execution_count=1.
        let shell = transport.drain_outgoing(Channel::Shell);
        assert_eq!(shell.len(), 1);
        let reply = Message::decode(&shell[0], &signer).unwrap();
        assert_eq!(reply.header.msg_type, "execute_reply");
        assert_eq!(reply.content["status"], "ok", "shell reply: {reply:?}");
        assert_eq!(reply.content["execution_count"], 1);
        assert_eq!(reply.identities, vec![b"client-1".to_vec()]);

        // IOPub: busy → execute_input → stream(stdout) → idle. The
        // effect footer for a `println` cell may also fire as a
        // second `stream(stdout)`, so accept either 4 or 5 frames as
        // long as the broadcast shape is right.
        let iopub = drain_iopub_in_memory(&transport, &signer);
        let kinds: Vec<&str> = iopub.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(kinds[0], "status");
        assert_eq!(kinds[1], "execute_input");
        assert_eq!(kinds[2], "stream");
        assert_eq!(*kinds.last().unwrap(), "status");

        // busy
        assert_eq!(iopub[0].1.content["execution_state"], "busy");
        // execute_input echoes the code + count
        assert_eq!(iopub[1].1.content["code"], "println(\"hello kara\");");
        assert_eq!(iopub[1].1.content["execution_count"], 1);
        // First `stream` carries the captured output on stdout.
        assert_eq!(iopub[2].1.content["name"], "stdout");
        let text = iopub[2].1.content["text"].as_str().unwrap();
        assert!(text.contains("hello kara"), "stdout was {text:?}");
        // idle
        assert_eq!(iopub.last().unwrap().1.content["execution_state"], "idle");
    }

    #[test]
    fn execute_request_increments_execution_count_across_cells() {
        let transport = Arc::new(InMemoryTransport::new());
        let signer = Signer::new("k");
        let kernel = Kernel::new(transport.clone(), signer.clone(), KernelInfo::default());

        for n in 1..=3 {
            transport.push_incoming(
                Channel::Shell,
                build_request(
                    &signer,
                    "execute_request",
                    json!({"code": format!("let _x{n} = {n};"), "silent": false}),
                    vec![],
                ),
            );
            run_one_pass(&kernel);
            let shell = transport.drain_outgoing(Channel::Shell);
            let reply = Message::decode(&shell[0], &signer).unwrap();
            assert_eq!(reply.content["execution_count"], n);
            // drop the iopub broadcasts between cells
            let _ = transport.drain_outgoing(Channel::IoPub);
        }
    }

    #[test]
    fn execute_request_silent_skips_execute_input_and_iopub_output() {
        // Per the Jupyter spec, `silent: true` requests don't bump
        // the counter and don't broadcast `execute_input` /
        // `stream` / `error`. Only the busy/idle status pair fires
        // (so frontends still see kernel activity).
        let transport = Arc::new(InMemoryTransport::new());
        let signer = Signer::new("k");
        let kernel = Kernel::new(transport.clone(), signer.clone(), KernelInfo::default());
        transport.push_incoming(
            Channel::Shell,
            build_request(
                &signer,
                "execute_request",
                json!({"code": "println!(\"shh\")", "silent": true}),
                vec![],
            ),
        );
        run_one_pass(&kernel);

        let shell = transport.drain_outgoing(Channel::Shell);
        let reply = Message::decode(&shell[0], &signer).unwrap();
        assert_eq!(reply.content["execution_count"], 0);

        let kinds: Vec<String> = drain_iopub_in_memory(&transport, &signer)
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        assert_eq!(kinds, ["status", "status"], "only busy/idle on silent");
    }

    #[test]
    fn execute_request_compile_error_routes_through_error_channels() {
        let transport = Arc::new(InMemoryTransport::new());
        let signer = Signer::new("k");
        let kernel = Kernel::new(transport.clone(), signer.clone(), KernelInfo::default());
        transport.push_incoming(
            Channel::Shell,
            build_request(
                &signer,
                "execute_request",
                // Syntactically broken — guaranteed parse error.
                json!({"code": "let x = ;", "silent": false}),
                vec![],
            ),
        );
        run_one_pass(&kernel);

        let shell = transport.drain_outgoing(Channel::Shell);
        let reply = Message::decode(&shell[0], &signer).unwrap();
        assert_eq!(reply.content["status"], "error");
        assert_eq!(reply.content["ename"], "CompileError");
        assert!(reply.content["evalue"].is_string());
        assert!(reply.content["traceback"].is_array());

        let iopub = drain_iopub_in_memory(&transport, &signer);
        let kinds: Vec<&str> = iopub.iter().map(|(k, _)| k.as_str()).collect();
        // status busy → execute_input → stream(stderr) → error → status idle
        assert_eq!(
            kinds,
            ["status", "execute_input", "stream", "error", "status"],
            "iopub broadcast sequence on error"
        );
        // stream is stderr, not stdout, for diagnostics.
        assert_eq!(iopub[2].1.content["name"], "stderr");
        // The iopub `error` broadcast mirrors the shell reply.
        assert_eq!(iopub[3].1.content["ename"], "CompileError");
    }

    #[test]
    fn execute_request_successful_cell_streams_only_stdout() {
        // Successful cells route captured `println` output (and any
        // non-empty effect footer) through `stream(stdout)` — never
        // `stream(stderr)`. The stderr stream is reserved for
        // diagnostics + `perf[…]` notes.
        let transport = Arc::new(InMemoryTransport::new());
        let signer = Signer::new("k");
        let kernel = Kernel::new(transport.clone(), signer.clone(), KernelInfo::default());
        transport.push_incoming(
            Channel::Shell,
            build_request(
                &signer,
                "execute_request",
                json!({"code": "println(\"side effect\");", "silent": false}),
                vec![],
            ),
        );
        run_one_pass(&kernel);

        let iopub = drain_iopub_in_memory(&transport, &signer);
        let stream_frames: Vec<&Message> = iopub
            .iter()
            .filter(|(k, _)| k == "stream")
            .map(|(_, m)| m)
            .collect();
        assert!(
            !stream_frames.is_empty(),
            "expected at least one stream frame for a println cell"
        );
        for frame in &stream_frames {
            assert_eq!(
                frame.content["name"], "stdout",
                "successful cell must not route output through stderr"
            );
        }
    }

    #[test]
    fn execute_request_magic_cell_routes_through_dispatch_magic() {
        // `%`-prefixed cells go through `Session::dispatch_magic`;
        // an unknown magic should return an error MagicOutput which
        // the handler routes to `status=error` on shell + stream
        // (stderr) + error on iopub.
        let transport = Arc::new(InMemoryTransport::new());
        let signer = Signer::new("k");
        let kernel = Kernel::new(transport.clone(), signer.clone(), KernelInfo::default());
        transport.push_incoming(
            Channel::Shell,
            build_request(
                &signer,
                "execute_request",
                json!({"code": "%totally-not-a-real-magic", "silent": false}),
                vec![],
            ),
        );
        run_one_pass(&kernel);

        let shell = transport.drain_outgoing(Channel::Shell);
        let reply = Message::decode(&shell[0], &signer).unwrap();
        assert_eq!(reply.content["status"], "error");
        assert_eq!(reply.content["ename"], "MagicError");
    }

    #[test]
    fn execute_request_session_state_persists_across_cells() {
        // Two cells: the first declares a binding, the second uses
        // it. The shared `Session` in the Kernel keeps the binding
        // live — the second cell sees `x = 7`.
        let transport = Arc::new(InMemoryTransport::new());
        let signer = Signer::new("k");
        let kernel = Kernel::new(transport.clone(), signer.clone(), KernelInfo::default());

        transport.push_incoming(
            Channel::Shell,
            build_request(
                &signer,
                "execute_request",
                json!({"code": "let x = 7;", "silent": false}),
                vec![],
            ),
        );
        run_one_pass(&kernel);
        let _ = transport.drain_outgoing(Channel::Shell);
        let _ = transport.drain_outgoing(Channel::IoPub);

        transport.push_incoming(
            Channel::Shell,
            build_request(
                &signer,
                "execute_request",
                json!({"code": "println(x);", "silent": false}),
                vec![],
            ),
        );
        run_one_pass(&kernel);

        let shell = transport.drain_outgoing(Channel::Shell);
        let reply = Message::decode(&shell[0], &signer).unwrap();
        assert_eq!(
            reply.content["status"], "ok",
            "second cell failed: {reply:?}"
        );

        let iopub = drain_iopub_in_memory(&transport, &signer);
        let stdout_text = iopub
            .iter()
            .find(|(k, m)| k == "stream" && m.content["name"] == "stdout")
            .map(|(_, m)| m.content["text"].as_str().unwrap_or("").to_string())
            .unwrap_or_default();
        assert!(stdout_text.contains("7"), "expected '7' in {stdout_text:?}");
    }

    #[test]
    fn execute_request_empty_code_is_clean_noop() {
        // An empty cell is legal and should return cleanly without
        // an `execute_result` or `error`. Frontends submit empty
        // cells during shutdown sometimes.
        let transport = Arc::new(InMemoryTransport::new());
        let signer = Signer::new("k");
        let kernel = Kernel::new(transport.clone(), signer.clone(), KernelInfo::default());
        transport.push_incoming(
            Channel::Shell,
            build_request(
                &signer,
                "execute_request",
                json!({"code": "", "silent": false}),
                vec![],
            ),
        );
        run_one_pass(&kernel);

        let shell = transport.drain_outgoing(Channel::Shell);
        let reply = Message::decode(&shell[0], &signer).unwrap();
        assert_eq!(reply.content["status"], "ok");

        // IOPub: busy → execute_input → idle (no stream lines).
        let iopub = drain_iopub_in_memory(&transport, &signer);
        let kinds: Vec<&str> = iopub.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(kinds, ["status", "execute_input", "status"]);
    }

    #[test]
    fn unhandled_msg_type_does_not_crash() {
        // Slice 4 handles kernel_info / shutdown / execute. Anything
        // else (slice 5's complete_request, is_complete_request,
        // interrupt_request) is logged and skipped. Verify the pump
        // returns cleanly and produces no shell output.
        let transport = Arc::new(InMemoryTransport::new());
        let signer = Signer::new("k");
        let kernel = Kernel::new(transport.clone(), signer.clone(), KernelInfo::default());
        let request = build_request(&signer, "complete_request", json!({}), vec![]);
        transport.push_incoming(Channel::Shell, request);
        run_one_pass(&kernel);
        assert!(transport.drain_outgoing(Channel::Shell).is_empty());
        assert!(transport.drain_outgoing(Channel::IoPub).is_empty());
    }

    #[test]
    fn malformed_message_is_skipped() {
        let transport = Arc::new(InMemoryTransport::new());
        let signer = Signer::new("k");
        let kernel = Kernel::new(transport.clone(), signer.clone(), KernelInfo::default());
        // Frames missing the <IDS|MSG> delimiter.
        transport.push_incoming(
            Channel::Shell,
            vec![b"garbage".to_vec(), b"more-garbage".to_vec()],
        );
        run_one_pass(&kernel);
        // Pump survives; nothing was sent.
        assert!(transport.drain_outgoing(Channel::Shell).is_empty());
    }

    #[test]
    fn heartbeat_echoes_payload() {
        let transport = Arc::new(InMemoryTransport::new());
        // Push an echo target then close so the loop exits after one
        // iteration.
        transport.push_incoming(Channel::Heartbeat, vec![b"ping".to_vec()]);
        let handle = {
            let t = transport.clone();
            thread::spawn(move || heartbeat_loop(t))
        };
        // Wait a tick for the heartbeat thread to consume + echo.
        for _ in 0..20 {
            let out = transport.drain_outgoing(Channel::Heartbeat);
            if !out.is_empty() {
                assert_eq!(out[0], vec![b"ping".to_vec()]);
                transport.close();
                handle.join().unwrap();
                return;
            }
            thread::sleep(Duration::from_millis(5));
        }
        transport.close();
        handle.join().unwrap();
        panic!("heartbeat thread never echoed");
    }

    #[test]
    fn iso8601_now_has_expected_shape() {
        let s = iso8601_now();
        // Format: YYYY-MM-DDTHH:MM:SS.uuuuuu — length 26.
        assert_eq!(s.len(), 26, "got {s:?}");
        assert_eq!(s.as_bytes()[4], b'-');
        assert_eq!(s.as_bytes()[7], b'-');
        assert_eq!(s.as_bytes()[10], b'T');
        assert_eq!(s.as_bytes()[13], b':');
        assert_eq!(s.as_bytes()[16], b':');
        assert_eq!(s.as_bytes()[19], b'.');
    }

    #[test]
    fn civil_date_handles_known_epochs() {
        // Unix epoch.
        assert_eq!(civil_date_from_days(0), (1970, 1, 1));
        // First day of 2000.
        assert_eq!(civil_date_from_days(10_957), (2000, 1, 1));
        // Leap day 2024.
        assert_eq!(civil_date_from_days(19_782), (2024, 2, 29));
        // 2026-05-19 — match the tracker's "today's date".
        // Days from 1970-01-01 to 2026-05-19 = 20_592.
        assert_eq!(civil_date_from_days(20_592), (2026, 5, 19));
    }

    #[test]
    fn kernel_info_default_pulls_version_from_crate() {
        let info = KernelInfo::default();
        assert_eq!(info.implementation, "karac");
        assert_eq!(info.language_name, "kara");
        assert_eq!(info.implementation_version, env!("CARGO_PKG_VERSION"));
        assert!(info.banner.contains(env!("CARGO_PKG_VERSION")));
        assert!(info.banner.contains(PROTOCOL_VERSION));
    }
}
