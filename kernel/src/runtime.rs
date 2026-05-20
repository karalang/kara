//! Kernel runtime — message pump + per-msg-type dispatch.
//!
//! Slice 3 wires:
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
//!
//! Slices 4+ extend the dispatch table (`execute_request`,
//! `complete_request`, `is_complete_request`, `interrupt_request`)
//! by adding match arms; the pump itself stays the same.
//!
//! Heartbeat is a separate dedicated thread (see
//! `Kernel::spawn_heartbeat`) — bare echo loop, no signing, no
//! decoding. It cannot share the pump's thread because a long
//! execute_request would starve heartbeat liveness checks and the
//! frontend would mark the kernel dead.

#![allow(dead_code)]

use crate::transport::{Channel, Transport, TransportError};
use crate::wire::{Header, Message, Signer, PROTOCOL_VERSION};
use serde_json::{json, Value as JsonValue};
use std::sync::Arc;
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
    msg_counter: std::sync::Mutex<u64>,
}

impl<T: Transport + 'static> Kernel<T> {
    pub fn new(transport: Arc<T>, signer: Signer, info: KernelInfo) -> Self {
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
            msg_counter: std::sync::Mutex::new(0),
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
            other => {
                // Slices 4+ extend this list. Silent in this slice
                // would hide bugs; log via stderr so anyone wiring up
                // a frontend before slice 4 ships sees the gap.
                eprintln!(
                    "karac-kernel: unhandled msg_type {other:?} on {channel:?} \
                     (slice 3 ships kernel_info / shutdown only)"
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

    fn broadcast_status(&self, state: &str, parent: &Header) {
        let content = json!({ "execution_state": state });
        let msg = self.build_iopub_broadcast("status", parent, content);
        if let Err(err) = self
            .transport
            .send(Channel::IoPub, msg.encode(&self.signer))
        {
            eprintln!("karac-kernel: failed to broadcast {state} status: {err}");
        }
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

    #[test]
    fn unhandled_msg_type_does_not_crash() {
        // Slice 3 only ships kernel_info + shutdown handlers;
        // unknown messages are logged and skipped. Verify the pump
        // returns cleanly and produces no shell output.
        let transport = Arc::new(InMemoryTransport::new());
        let signer = Signer::new("k");
        let kernel = Kernel::new(transport.clone(), signer.clone(), KernelInfo::default());
        let request = build_request(&signer, "execute_request", json!({}), vec![]);
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
