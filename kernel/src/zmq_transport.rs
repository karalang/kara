//! Production `Transport` implementation backed by the `zmq` crate.
//!
//! Gated on `feature = "real-zmq"` so the default workspace build
//! doesn't require libzmq. Compiled in by the Python shim (slice 6)
//! when building the kernel for actual Jupyter use.
//!
//! Socket-type mapping per the Jupyter Messaging Protocol:
//!
//! - Shell:     ROUTER  (frontend DEALER → kernel ROUTER)
//! - IOPub:     PUB     (kernel PUB → many frontend SUBs)
//! - Stdin:     ROUTER  (kernel asks the frontend for input)
//! - Control:   ROUTER  (high-priority sibling of Shell)
//! - Heartbeat: REP     (frontend REQ → kernel REP echo)

#![cfg(feature = "real-zmq")]

use crate::connection::ConnectionFile;
use crate::transport::{Channel, Transport, TransportError};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

/// Wraps the five ZMQ sockets opened against the connection-file
/// endpoints. Sockets are interior-mutex-guarded — `zmq::Socket` is
/// not `Sync`, so we wrap each in a `Mutex` and the trait borrows it
/// per call. Lock contention is irrelevant in practice (no two
/// threads talk to the same socket — the runtime sends on IOPub from
/// the pump thread and recv on shell/control from the same thread;
/// stdin and heartbeat each have their own thread).
pub struct ZmqTransport {
    /// `zmq::Context` is `Send + Sync`. Held to keep the sockets'
    /// underlying ZMQ context alive for the kernel's lifetime.
    _ctx: zmq::Context,
    shell: Mutex<zmq::Socket>,
    iopub: Mutex<zmq::Socket>,
    stdin: Mutex<zmq::Socket>,
    control: Mutex<zmq::Socket>,
    heartbeat: Mutex<zmq::Socket>,
    closed: AtomicBool,
}

impl ZmqTransport {
    /// Open all five sockets per the connection file. Bind happens
    /// here — any port conflict surfaces as
    /// [`TransportError::Bind`] before the run loop starts.
    pub fn bind(conn: &ConnectionFile) -> Result<Self, TransportError> {
        let ctx = zmq::Context::new();
        let shell = bind_socket(&ctx, Channel::Shell, zmq::ROUTER, conn, conn.shell_port)?;
        let iopub = bind_socket(&ctx, Channel::IoPub, zmq::PUB, conn, conn.iopub_port)?;
        let stdin = bind_socket(&ctx, Channel::Stdin, zmq::ROUTER, conn, conn.stdin_port)?;
        let control = bind_socket(&ctx, Channel::Control, zmq::ROUTER, conn, conn.control_port)?;
        let heartbeat = bind_socket(&ctx, Channel::Heartbeat, zmq::REP, conn, conn.hb_port)?;
        Ok(Self {
            _ctx: ctx,
            shell: Mutex::new(shell),
            iopub: Mutex::new(iopub),
            stdin: Mutex::new(stdin),
            control: Mutex::new(control),
            heartbeat: Mutex::new(heartbeat),
            closed: AtomicBool::new(false),
        })
    }

    fn socket(&self, channel: Channel) -> &Mutex<zmq::Socket> {
        match channel {
            Channel::Shell => &self.shell,
            Channel::IoPub => &self.iopub,
            Channel::Stdin => &self.stdin,
            Channel::Control => &self.control,
            Channel::Heartbeat => &self.heartbeat,
        }
    }
}

fn bind_socket(
    ctx: &zmq::Context,
    channel: Channel,
    kind: zmq::SocketType,
    conn: &ConnectionFile,
    port: u16,
) -> Result<zmq::Socket, TransportError> {
    let socket = ctx.socket(kind).map_err(|e| TransportError::Bind {
        channel,
        message: format!("ctx.socket({kind:?}) failed: {e}"),
    })?;
    let endpoint = conn.endpoint(port);
    socket.bind(&endpoint).map_err(|e| TransportError::Bind {
        channel,
        message: format!("bind {endpoint:?} failed: {e}"),
    })?;
    Ok(socket)
}

impl Transport for ZmqTransport {
    fn recv(
        &self,
        channel: Channel,
        timeout: Option<Duration>,
    ) -> Result<Vec<Vec<u8>>, TransportError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(TransportError::Closed { channel });
        }
        let socket = self.socket(channel).lock().unwrap();
        // ZMQ's `poll` takes a millisecond timeout; negative = block
        // forever. Match our `Option<Duration>` convention.
        let timeout_ms: i64 = timeout.map(|d| d.as_millis() as i64).unwrap_or(-1);
        let mut items = [socket.as_poll_item(zmq::POLLIN)];
        match zmq::poll(&mut items, timeout_ms) {
            Ok(0) => return Err(TransportError::Timeout { channel }),
            Ok(_) => {}
            Err(e) => {
                return Err(TransportError::Io {
                    channel,
                    message: format!("poll failed: {e}"),
                })
            }
        }
        socket.recv_multipart(0).map_err(|e| TransportError::Io {
            channel,
            message: format!("recv_multipart failed: {e}"),
        })
    }

    fn send(&self, channel: Channel, frames: Vec<Vec<u8>>) -> Result<(), TransportError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(TransportError::Closed { channel });
        }
        let socket = self.socket(channel).lock().unwrap();
        socket
            .send_multipart(&frames, 0)
            .map_err(|e| TransportError::Io {
                channel,
                message: format!("send_multipart failed: {e}"),
            })
    }

    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        // Sockets close when the struct drops; we don't explicitly
        // `unbind` because the kernel process exits shortly after.
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }
}
