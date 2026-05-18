//! Network event loop. Cross-platform abstraction over the OS-level
//! fd-readiness facilities — `epoll` on Linux, `kqueue` on macOS / BSD,
//! `IOCP` on Windows — via the `mio` crate.
//!
//! See `docs/design.md § Network Event Loop and State-Machine Transform`
//! and `docs/implementation_checklist/phase-6-runtime.md` line 15.
//!
//! ## v1 architectural commitments (per phase-6-runtime.md line 15)
//!
//! - **Single OS thread per event loop.** v1 runs exactly one loop per
//!   process; M2 / M3 may shard across multiple loops to reach the 1M+
//!   idle-connection target. `EventLoop` itself is `!Sync` and pinned
//!   to its constructing thread; cross-thread interaction goes through
//!   the clonable [`EventLoopHandle`].
//! - **Registration / de-registration are crate-internal.** The public
//!   language surface stays effect-typed (`sends(Network)` /
//!   `receives(Network)`); codegen lowers those effects into runtime
//!   calls. End-user Kāra code never sees this module's API.
//! - **Per-fd state holds the parked-task pointer + I/O direction +
//!   optional deadline.** Readiness wakeups carry the parked pointer
//!   back to the scheduler so it can resume the right task. The
//!   `deadline` field is stored so the timer-wheel work (M2 polish
//!   layer) can drive expiry-based wakeups without re-shaping the
//!   per-fd state.
//! - **Cross-thread wakeup via `mio::Waker`.** Under the hood `mio` uses
//!   `eventfd` on Linux, a pipe pair on BSD / macOS, and a posted IOCP
//!   completion packet on Windows — the three OS primitives the
//!   phase-6 entry calls out by name.

use mio::{Events, Interest, Poll, Token};
use std::collections::HashMap;
use std::ffi::c_void;
use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Direction(s) of I/O readiness we are polling on a given fd.
///
/// `repr(u8)` pins the discriminant width for stable FFI when the
/// codegen parking-emit path (phase-6 line 17) starts passing this
/// enum across the runtime boundary.
#[repr(u8)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum IoDirection {
    Read = 0,
    Write = 1,
    ReadWrite = 2,
}

impl IoDirection {
    fn to_interest(self) -> Interest {
        match self {
            IoDirection::Read => Interest::READABLE,
            IoDirection::Write => Interest::WRITABLE,
            IoDirection::ReadWrite => Interest::READABLE.add(Interest::WRITABLE),
        }
    }
}

/// Opaque handle returned by [`EventLoop::register`]. Caller hands it
/// back to [`EventLoop::deregister`] to remove the registration and
/// receives it through [`Wakeup`] when the fd becomes ready.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct RegistrationToken(usize);

/// Per-fd state stored inside the loop.
///
/// `parked` is type-erased to `*mut c_void` because the runtime
/// representation of a parked task is owned by the codegen parking
/// path (phase-6 line 17 — effect-routed task parking), not by the
/// event loop. The loop only stores and forwards it; correlating it
/// back to a concrete task type is the scheduler's responsibility.
struct FdState {
    parked: *mut c_void,
    direction: IoDirection,
    /// Optional deadline. Stored as part of the v1 per-fd state per
    /// the phase-6 line 15 spec. Timer-driven expiry is the M2 polish
    /// layer's work (timer wheel); v1 stores the field so M2 hooks
    /// into the existing per-fd state map without re-shaping it.
    #[allow(dead_code)]
    deadline: Option<Instant>,
}

// SAFETY: `parked` is a pointer owned by the codegen parking path /
// scheduler that registered the fd. The event loop never derefs it;
// it only stores it and hands it back through `Wakeup` when the fd
// becomes ready. The owner guarantees the pointer is valid from the
// `register` call until the corresponding `deregister` returns (or
// until the readiness wakeup is observed and consumed). `Send` is
// required because the `EventLoop` may be moved across threads
// before `run_once` is first called (though after that, the
// architectural commitment pins it to one thread).
unsafe impl Send for FdState {}

/// A readiness wakeup surfaced by [`EventLoop::run_once`].
///
/// Carries the parked-task pointer the caller registered with so the
/// scheduler can resume the right task. The `direction` field reports
/// which side of a `ReadWrite` registration actually fired (`mio`
/// surfaces these independently — a `ReadWrite` registration can wake
/// up with just `Read` or just `Write` ready).
#[derive(Debug)]
pub struct Wakeup {
    pub token: RegistrationToken,
    pub parked: *mut c_void,
    pub direction: IoDirection,
}

/// Single-threaded event loop.
///
/// Per the v1 architectural commitment, exactly one loop runs per
/// process. The type is `!Sync` (via `Poll`'s own non-`Sync` bound)
/// and intentionally not `Send`-erased to other threads after `run_once`
/// has been called — cross-thread interaction goes through
/// [`EventLoopHandle`].
pub struct EventLoop {
    poll: Poll,
    waker: Arc<mio::Waker>,
    events: Events,
    fds: HashMap<Token, FdState>,
    /// Monotonically increasing source of unique tokens. Reserved
    /// values: `0` is the cross-thread waker (see [`WAKER_TOKEN`]);
    /// user-fd tokens start at `1`.
    next_token: usize,
}

const WAKER_TOKEN: Token = Token(0);

impl EventLoop {
    /// Construct a new event loop. Allocates the underlying `mio::Poll`
    /// and registers the cross-thread waker.
    pub fn new() -> io::Result<Self> {
        let poll = Poll::new()?;
        let waker = Arc::new(mio::Waker::new(poll.registry(), WAKER_TOKEN)?);
        Ok(EventLoop {
            poll,
            waker,
            events: Events::with_capacity(256),
            fds: HashMap::new(),
            next_token: 1,
        })
    }

    /// Return a clonable handle that other threads can use to wake
    /// the loop. The handle holds the `mio::Waker` internally; clones
    /// are cheap (a single `Arc` bump).
    pub fn handle(&self) -> EventLoopHandle {
        EventLoopHandle {
            waker: Arc::clone(&self.waker),
        }
    }

    /// Register a source with the loop.
    ///
    /// `parked` is the opaque task pointer that will be returned
    /// through [`Wakeup`] when the fd becomes ready. The event loop
    /// stores it but does not deref it; lifetime is the caller's
    /// responsibility (the codegen parking path / scheduler).
    pub fn register<S: mio::event::Source + ?Sized>(
        &mut self,
        source: &mut S,
        direction: IoDirection,
        deadline: Option<Instant>,
        parked: *mut c_void,
    ) -> io::Result<RegistrationToken> {
        let token = Token(self.next_token);
        self.next_token = self
            .next_token
            .checked_add(1)
            .expect("event loop token exhaustion (usize wrap)");
        self.poll
            .registry()
            .register(source, token, direction.to_interest())?;
        self.fds.insert(
            token,
            FdState {
                parked,
                direction,
                deadline,
            },
        );
        Ok(RegistrationToken(token.0))
    }

    /// Remove a registration.
    ///
    /// `mio::Registry::deregister` takes the source itself (the OS
    /// needs the fd, not just our token), so the caller must hand
    /// back the source it registered. Removing the token from our
    /// internal map is unconditional — a `RegistrationToken` produced
    /// by this loop is always present unless it has already been
    /// deregistered, in which case removing again is a silent no-op.
    pub fn deregister<S: mio::event::Source + ?Sized>(
        &mut self,
        source: &mut S,
        token: RegistrationToken,
    ) -> io::Result<()> {
        self.poll.registry().deregister(source)?;
        self.fds.remove(&Token(token.0));
        Ok(())
    }

    /// Drive the loop once.
    ///
    /// Blocks until at least one fd is ready, the cross-thread waker
    /// fires, or `max_wait` elapses. Returns the readiness wakeups
    /// observed in this iteration.
    ///
    /// - `max_wait = None`: block until any wakeup.
    /// - `max_wait = Some(Duration::ZERO)`: poll without blocking.
    /// - `max_wait = Some(d)`: block up to `d`.
    ///
    /// Cross-thread waker events are filtered out of the returned
    /// `Vec` — they exist only to unblock `poll` so the caller can
    /// re-check any external state (new registrations queued by the
    /// scheduler, cancellation, shutdown). An empty return with no
    /// readiness wakeups indicates a waker or timeout wakeup.
    pub fn run_once(&mut self, max_wait: Option<Duration>) -> io::Result<Vec<Wakeup>> {
        self.poll.poll(&mut self.events, max_wait)?;
        let mut wakeups = Vec::new();
        for event in self.events.iter() {
            if event.token() == WAKER_TOKEN {
                continue;
            }
            let Some(state) = self.fds.get(&event.token()) else {
                continue;
            };
            let direction = if event.is_readable() && event.is_writable() {
                IoDirection::ReadWrite
            } else if event.is_readable() {
                IoDirection::Read
            } else if event.is_writable() {
                IoDirection::Write
            } else {
                state.direction
            };
            wakeups.push(Wakeup {
                token: RegistrationToken(event.token().0),
                parked: state.parked,
                direction,
            });
        }
        Ok(wakeups)
    }

    #[cfg(test)]
    fn registered_count(&self) -> usize {
        self.fds.len()
    }
}

/// Cross-thread waker handle.
///
/// Clone freely across threads. Calling [`wake`](Self::wake) interrupts
/// the corresponding [`EventLoop::run_once`] call (or makes the next
/// call return immediately if the loop is not currently parked).
/// Multiple `wake` calls between polls coalesce into a single wakeup —
/// this is `mio::Waker`'s documented semantics, not an accident.
#[derive(Clone)]
pub struct EventLoopHandle {
    waker: Arc<mio::Waker>,
}

impl EventLoopHandle {
    pub fn wake(&self) -> io::Result<()> {
        self.waker.wake()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mio::net::TcpListener;
    use std::net::SocketAddr;
    use std::thread;

    #[test]
    fn new_succeeds() {
        let _ev = EventLoop::new().expect("new event loop");
    }

    #[test]
    fn cross_thread_wake_unblocks_poll() {
        let mut ev = EventLoop::new().unwrap();
        let handle = ev.handle();

        let woke = thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            handle.wake().unwrap();
        });

        // Should return well before the 2s safety bound because the
        // waker fires at ~20ms.
        let start = Instant::now();
        let wakeups = ev.run_once(Some(Duration::from_secs(2))).unwrap();
        let elapsed = start.elapsed();

        // Waker tokens are filtered out, so the visible wakeups list
        // is empty; the fact that `poll` returned early is the proof.
        assert!(wakeups.is_empty());
        assert!(
            elapsed < Duration::from_secs(1),
            "expected cross-thread wake to unblock poll well under 1s, took {elapsed:?}"
        );

        woke.join().unwrap();
    }

    #[test]
    fn fd_readiness_carries_parked_pointer_back() {
        let bind_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let mut listener = TcpListener::bind(bind_addr).unwrap();
        let local = listener.local_addr().unwrap();

        let mut ev = EventLoop::new().unwrap();

        // Use a stack-allocated u64 as the "parked task" stand-in. The
        // loop never derefs it; we just check round-trip identity.
        let marker: u64 = 0xDEAD_BEEF_CAFE_F00D;
        let parked = std::ptr::addr_of!(marker) as *mut c_void;

        let token = ev
            .register(&mut listener, IoDirection::Read, None, parked)
            .unwrap();

        let connector = thread::spawn(move || {
            let _stream = std::net::TcpStream::connect(local).unwrap();
            thread::sleep(Duration::from_millis(50));
        });

        let wakeups = ev.run_once(Some(Duration::from_secs(2))).unwrap();
        assert_eq!(wakeups.len(), 1, "exactly one fd-readiness wakeup expected");
        assert_eq!(wakeups[0].token, token);
        assert_eq!(wakeups[0].parked, parked);
        assert_eq!(wakeups[0].direction, IoDirection::Read);

        connector.join().unwrap();

        ev.deregister(&mut listener, token).unwrap();
        assert_eq!(
            ev.registered_count(),
            0,
            "deregister should remove the fd from internal state"
        );
    }

    #[test]
    fn poll_timeout_returns_empty_wakeups() {
        let mut ev = EventLoop::new().unwrap();
        let wakeups = ev.run_once(Some(Duration::from_millis(10))).unwrap();
        assert!(wakeups.is_empty(), "no fds registered → no wakeups");
    }

    #[test]
    fn tokens_are_distinct_across_registrations() {
        // Bind two listeners to different ports, register both, verify
        // tokens differ. Also checks `next_token` increments correctly.
        let mut l1 = TcpListener::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let mut l2 = TcpListener::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let mut ev = EventLoop::new().unwrap();
        let t1 = ev
            .register(&mut l1, IoDirection::Read, None, std::ptr::null_mut())
            .unwrap();
        let t2 = ev
            .register(&mut l2, IoDirection::Read, None, std::ptr::null_mut())
            .unwrap();
        assert_ne!(t1, t2);
        ev.deregister(&mut l1, t1).unwrap();
        ev.deregister(&mut l2, t2).unwrap();
    }

    #[test]
    fn io_direction_to_interest_covers_all_arms() {
        assert_eq!(IoDirection::Read.to_interest(), Interest::READABLE);
        assert_eq!(IoDirection::Write.to_interest(), Interest::WRITABLE);
        assert_eq!(
            IoDirection::ReadWrite.to_interest(),
            Interest::READABLE.add(Interest::WRITABLE)
        );
    }
}
