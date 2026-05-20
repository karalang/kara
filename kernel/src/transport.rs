// `Channel::Stdin` is reserved for slice 5's `input_request` flow,
// and `TransportError` is constructed entirely by cfg-gated code
// (`InMemoryTransport` under cfg(test); `ZmqTransport` under feature
// = "real-zmq"). Under the default workspace build neither
// construction site is visible, so `dead_code` would fire across the
// whole module — clear it at the module level rather than per item.
#![allow(dead_code)]

//! Transport abstraction over the five Jupyter kernel channels.
//!
//! The Jupyter Messaging Protocol uses ZMQ sockets in production —
//! `Shell` (ROUTER), `IoPub` (PUB), `Stdin` (ROUTER), `Control`
//! (ROUTER), `Heartbeat` (REP). Wrapping them behind a trait gives
//! two payoffs:
//!
//! 1. The slice-3 message pump (see `runtime.rs`) is generic over the
//!    transport, so unit tests drive it through [`InMemoryTransport`]
//!    without binding real sockets — `cargo test -p karac-kernel`
//!    stays self-contained even on hosts without libzmq.
//! 2. The real `ZmqTransport` (cfg-gated on `feature = "real-zmq"`)
//!    is a leaf module — bugs in socket setup can't infect the
//!    dispatch logic, and the codec layer in `wire.rs` doesn't
//!    leak ZMQ-specific types.
//!
//! Multipart frames are passed as `Vec<Vec<u8>>` to match the
//! `wire::Message::{encode, decode}` contract from slice 2.

use std::time::Duration;

/// One of the five Jupyter channels. Used to route a `recv` /
/// `send` call to the right socket inside a [`Transport`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    /// Shell ROUTER — request/reply for `execute_request`,
    /// `kernel_info_request`, `complete_request`, …
    Shell,
    /// IOPub PUB — broadcast of `stream` / `display_data` /
    /// `execute_result` / `error` / status messages.
    IoPub,
    /// Stdin ROUTER — kernel asks the frontend for keyboard input.
    Stdin,
    /// Control ROUTER — high-priority requests (`interrupt_request`,
    /// `shutdown_request`) that must not queue behind a long
    /// `execute_request`.
    Control,
    /// Heartbeat REP — bare echo loop the frontend uses to detect
    /// kernel liveness.
    Heartbeat,
}

/// Errors surfaced by transport operations. Concrete implementations
/// flatten their internal error types into these variants so the
/// message-pump layer doesn't need a generic error parameter.
#[derive(Debug)]
pub enum TransportError {
    /// Socket binding failed at startup (port already in use,
    /// permission denied, malformed endpoint URL, …).
    Bind { channel: Channel, message: String },
    /// Send or receive failed mid-session.
    Io { channel: Channel, message: String },
    /// Recv timed out without a message arriving. Treated as a soft
    /// signal — the message-pump loop checks shutdown flags between
    /// timeouts so the kernel can exit cleanly.
    Timeout { channel: Channel },
    /// The transport has been closed and no further I/O is possible.
    Closed { channel: Channel },
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bind { channel, message } => {
                write!(f, "bind {channel:?}: {message}")
            }
            Self::Io { channel, message } => write!(f, "io {channel:?}: {message}"),
            Self::Timeout { channel } => write!(f, "timeout on {channel:?}"),
            Self::Closed { channel } => write!(f, "{channel:?} closed"),
        }
    }
}

impl std::error::Error for TransportError {}

/// Abstraction over the five Jupyter channels. The message pump
/// reads `Shell` / `Control` / `Stdin` requests, writes replies on
/// the same channel, and broadcasts side-effects (`stream`,
/// `execute_result`, status) on `IoPub`. `Heartbeat` has no payload
/// — the transport echoes the bytes the frontend sent. Concrete
/// transports may run the echo in a dedicated thread (real ZMQ) or
/// short-circuit it (`InMemoryTransport`).
pub trait Transport: Send + Sync {
    /// Receive one multipart message from a request channel. Blocks
    /// up to `timeout` (or indefinitely if `None`). The returned
    /// frame list is the verbatim ZMQ payload — identity frames,
    /// `<IDS|MSG>` delimiter, signature, four JSON frames, optional
    /// buffers — ready to feed to `wire::Message::decode`.
    fn recv(
        &self,
        channel: Channel,
        timeout: Option<Duration>,
    ) -> Result<Vec<Vec<u8>>, TransportError>;

    /// Send one multipart message on a channel. `frames` is the
    /// `wire::Message::encode` output — identity frames first (for
    /// `Shell` / `Control` / `Stdin` ROUTER replies) then the signed
    /// payload.
    fn send(&self, channel: Channel, frames: Vec<Vec<u8>>) -> Result<(), TransportError>;

    /// Close all five sockets / channels. Idempotent — calling
    /// `close` on an already-closed transport is a no-op.
    fn close(&self);

    /// True after [`Self::close`] has been called. The message pump
    /// polls this between timeout-bounded `recv` calls so a shutdown
    /// request unblocks the loop.
    fn is_closed(&self) -> bool;
}

#[cfg(test)]
pub(crate) mod testing {
    //! `InMemoryTransport` — drives the message pump under
    //! `#[cfg(test)]` without binding real sockets. Each channel is
    //! backed by two `Mutex<VecDeque<…>>` queues — one for frames the
    //! test feeds into the kernel ("incoming"), one for frames the
    //! kernel produces ("outgoing"). The test inspects the outgoing
    //! queue after driving the pump.

    use super::*;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Condvar, Mutex};

    #[derive(Default)]
    struct ChannelState {
        incoming: Mutex<VecDeque<Vec<Vec<u8>>>>,
        outgoing: Mutex<VecDeque<Vec<Vec<u8>>>>,
        signal: Condvar,
    }

    #[derive(Default)]
    pub(crate) struct InMemoryTransport {
        shell: ChannelState,
        iopub: ChannelState,
        stdin: ChannelState,
        control: ChannelState,
        heartbeat: ChannelState,
        closed: AtomicBool,
    }

    impl InMemoryTransport {
        pub(crate) fn new() -> Self {
            Self::default()
        }

        fn state(&self, channel: Channel) -> &ChannelState {
            match channel {
                Channel::Shell => &self.shell,
                Channel::IoPub => &self.iopub,
                Channel::Stdin => &self.stdin,
                Channel::Control => &self.control,
                Channel::Heartbeat => &self.heartbeat,
            }
        }

        /// Test helper — push a frame list onto a channel as if the
        /// frontend had sent it. Wakes any pumping thread blocked in
        /// `recv` on the same channel.
        pub(crate) fn push_incoming(&self, channel: Channel, frames: Vec<Vec<u8>>) {
            let state = self.state(channel);
            state.incoming.lock().unwrap().push_back(frames);
            state.signal.notify_all();
        }

        /// Test helper — drain everything the kernel sent on a
        /// channel since the last drain. Empty result means the
        /// kernel hasn't produced anything yet.
        pub(crate) fn drain_outgoing(&self, channel: Channel) -> Vec<Vec<Vec<u8>>> {
            let state = self.state(channel);
            state.outgoing.lock().unwrap().drain(..).collect()
        }

        /// Wake every channel's condvar without pushing a message.
        /// Used by tests that want a timed-out `recv` to unblock
        /// after `close`.
        pub(crate) fn notify_all(&self) {
            self.shell.signal.notify_all();
            self.iopub.signal.notify_all();
            self.stdin.signal.notify_all();
            self.control.signal.notify_all();
            self.heartbeat.signal.notify_all();
        }
    }

    impl Transport for InMemoryTransport {
        fn recv(
            &self,
            channel: Channel,
            timeout: Option<Duration>,
        ) -> Result<Vec<Vec<u8>>, TransportError> {
            let state = self.state(channel);
            let mut queue = state.incoming.lock().unwrap();
            loop {
                if self.closed.load(Ordering::Acquire) {
                    return Err(TransportError::Closed { channel });
                }
                if let Some(frames) = queue.pop_front() {
                    return Ok(frames);
                }
                match timeout {
                    None => {
                        queue = state.signal.wait(queue).unwrap();
                    }
                    Some(dur) => {
                        let (next_queue, result) = state.signal.wait_timeout(queue, dur).unwrap();
                        queue = next_queue;
                        if result.timed_out() {
                            return Err(TransportError::Timeout { channel });
                        }
                    }
                }
            }
        }

        fn send(&self, channel: Channel, frames: Vec<Vec<u8>>) -> Result<(), TransportError> {
            if self.closed.load(Ordering::Acquire) {
                return Err(TransportError::Closed { channel });
            }
            self.state(channel)
                .outgoing
                .lock()
                .unwrap()
                .push_back(frames);
            Ok(())
        }

        fn close(&self) {
            self.closed.store(true, Ordering::Release);
            self.notify_all();
        }

        fn is_closed(&self) -> bool {
            self.closed.load(Ordering::Acquire)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn push_then_recv_round_trips() {
            let t = InMemoryTransport::new();
            t.push_incoming(Channel::Shell, vec![b"frame1".to_vec(), b"frame2".to_vec()]);
            let got = t
                .recv(Channel::Shell, Some(Duration::from_millis(50)))
                .unwrap();
            assert_eq!(got, vec![b"frame1".to_vec(), b"frame2".to_vec()]);
        }

        #[test]
        fn recv_timeout_returns_timeout_error() {
            let t = InMemoryTransport::new();
            let err = t
                .recv(Channel::Shell, Some(Duration::from_millis(10)))
                .unwrap_err();
            assert!(matches!(
                err,
                TransportError::Timeout {
                    channel: Channel::Shell
                }
            ));
        }

        #[test]
        fn send_then_drain() {
            let t = InMemoryTransport::new();
            t.send(Channel::IoPub, vec![b"reply".to_vec()]).unwrap();
            t.send(Channel::IoPub, vec![b"reply2".to_vec()]).unwrap();
            let out = t.drain_outgoing(Channel::IoPub);
            assert_eq!(out.len(), 2);
            assert_eq!(out[0], vec![b"reply".to_vec()]);
            // Second drain produces nothing.
            let out2 = t.drain_outgoing(Channel::IoPub);
            assert!(out2.is_empty());
        }

        #[test]
        fn close_makes_recv_fail() {
            let t = InMemoryTransport::new();
            t.close();
            let err = t.recv(Channel::Shell, None).unwrap_err();
            assert!(matches!(
                err,
                TransportError::Closed {
                    channel: Channel::Shell
                }
            ));
        }

        #[test]
        fn close_unblocks_pending_recv() {
            use std::sync::Arc;
            use std::thread;
            let t = Arc::new(InMemoryTransport::new());
            let t2 = t.clone();
            let handle = thread::spawn(move || t2.recv(Channel::Shell, None));
            // Give the recv thread a moment to enter the wait.
            thread::sleep(Duration::from_millis(20));
            t.close();
            let err = handle.join().unwrap().unwrap_err();
            assert!(matches!(err, TransportError::Closed { .. }));
        }

        #[test]
        fn channels_are_isolated() {
            let t = InMemoryTransport::new();
            t.push_incoming(Channel::Shell, vec![b"shell-msg".to_vec()]);
            // A recv on Control should still time out.
            let err = t
                .recv(Channel::Control, Some(Duration::from_millis(10)))
                .unwrap_err();
            assert!(matches!(err, TransportError::Timeout { .. }));
            // Shell still has its message.
            let got = t
                .recv(Channel::Shell, Some(Duration::from_millis(10)))
                .unwrap();
            assert_eq!(got, vec![b"shell-msg".to_vec()]);
        }
    }
}
