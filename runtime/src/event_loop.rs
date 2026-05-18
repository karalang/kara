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
use std::sync::{Arc, Mutex, OnceLock};
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

// ── Process-global event loop + FFI surface ────────────────────────────────
//
// Phase 6 line 17 (effect-routed task parking) — slice 1: runtime FFI
// surface. The `extern "C"` entry points below are what codegen-emitted
// IR will call into when it lowers a network-effecting function call to
// "register fd with event loop, park current task, yield." The actual
// "park / yield" mechanism (state-machine transform — phase-6 line 18)
// and the scheduler-side wakeup-to-worker-queue glue land as follow-up
// slices; this slice exposes only the ABI codegen will emit against.
//
// **Threading model (v1).** The process-global event loop is wrapped in
// a `Mutex` so register / deregister / poll calls from any thread are
// serialized. v1 prioritizes correctness over throughput here; M2 polish
// layer may split this into a clonable `Registry` handle plus a
// dedicated poller thread (no FFI signature change required — the
// surface below is the contract).
//
// **Platform scope.** The fd-registration FFI fns are `#[cfg(unix)]`
// only — Linux / macOS / BSD. Windows IOCP uses a completion-based
// model rather than fd-readiness, so its FFI surface looks different
// and lands separately. The `poll` and `wake` fns are cross-platform
// (mio's `Poll` / `Waker` work everywhere).

/// Process-global event loop instance, lazily initialized.
/// Per the v1 architectural commitment: exactly one EventLoop per process.
static EVENT_LOOP: OnceLock<Mutex<EventLoop>> = OnceLock::new();

/// Cached handle to the process-global event loop's waker. Populated
/// during the same `OnceLock::get_or_init` that constructs `EVENT_LOOP`,
/// so observing `EVENT_LOOP` initialized implies `EVENT_LOOP_HANDLE` is
/// also set.
static EVENT_LOOP_HANDLE: OnceLock<EventLoopHandle> = OnceLock::new();

fn global_event_loop() -> &'static Mutex<EventLoop> {
    EVENT_LOOP.get_or_init(|| {
        let ev = EventLoop::new().expect("karac_runtime: process-global event loop init failed");
        // `set` may already have been populated by a racing initializer if
        // two threads called this concurrently; the `OnceLock::get_or_init`
        // contract guarantees we are the unique initializer of `EVENT_LOOP`,
        // but the handle write is a separate `OnceLock`, so ignore a
        // duplicate-set error.
        let _ = EVENT_LOOP_HANDLE.set(ev.handle());
        Mutex::new(ev)
    })
}

/// Recover from a poisoned global-event-loop mutex. The runtime's
/// invariants do not depend on the lock being unpoisoned — the inner
/// `EventLoop` is valid regardless of whether a previous holder
/// panicked — so we proceed with the inner value rather than aborting.
fn lock_global_event_loop() -> std::sync::MutexGuard<'static, EventLoop> {
    match global_event_loop().lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

/// Readiness wakeup entry written into the caller-allocated buffer by
/// [`karac_runtime_event_loop_poll`].
///
/// `direction` encoding matches the [`IoDirection`] discriminant:
/// 0 = Read, 1 = Write, 2 = ReadWrite. `repr(C)` pins the layout for
/// the codegen-emitted struct that consumes this on the Kāra side.
#[repr(C)]
pub struct KaracWakeup {
    pub token: u64,
    pub parked: *mut c_void,
    pub direction: u8,
}

/// Register a raw fd with the process-global event loop.
///
/// `direction`: 0 = Read, 1 = Write, 2 = ReadWrite. Any other value
/// returns 0 (invalid input).
///
/// `parked`: opaque pointer the runtime stores and hands back through
/// [`KaracWakeup::parked`] on readiness. The runtime never derefs it;
/// the caller (codegen parking path) owns its lifetime.
///
/// Returns a non-zero registration token on success, 0 on failure.
/// Token 0 is reserved for the cross-thread waker and is never
/// returned by this fn even on success.
///
/// Unix-only — `mio::unix::SourceFd` is the cross-Unix raw-fd wrapper
/// that mio uses to talk to epoll / kqueue. Windows IOCP integration
/// is a separate slice (different fd model).
#[cfg(unix)]
#[no_mangle]
pub extern "C" fn karac_runtime_event_loop_register_fd(
    raw_fd: i32,
    direction: u8,
    parked: *mut c_void,
) -> u64 {
    let dir = match direction {
        0 => IoDirection::Read,
        1 => IoDirection::Write,
        2 => IoDirection::ReadWrite,
        _ => return 0,
    };
    let mut source = mio::unix::SourceFd(&raw_fd);
    let mut guard = lock_global_event_loop();
    match guard.register(&mut source, dir, None, parked) {
        Ok(token) => token.0 as u64,
        Err(_) => 0,
    }
}

/// Deregister a previously registered fd.
///
/// `raw_fd` must match the fd passed at register time. `token` is the
/// value returned by [`karac_runtime_event_loop_register_fd`].
///
/// Returns 0 on success, -1 on error.
#[cfg(unix)]
#[no_mangle]
pub extern "C" fn karac_runtime_event_loop_deregister_fd(raw_fd: i32, token: u64) -> i32 {
    let mut source = mio::unix::SourceFd(&raw_fd);
    let mut guard = lock_global_event_loop();
    match guard.deregister(&mut source, RegistrationToken(token as usize)) {
        Ok(()) => 0,
        Err(_) => -1,
    }
}

/// Drive the event loop once.
///
/// - `max_wait_nanos = -1`: block indefinitely until any wakeup.
/// - `max_wait_nanos = 0`: poll without blocking.
/// - `max_wait_nanos > 0`: wait up to `n` nanoseconds.
/// - Any other negative value: poll without blocking (treated as 0).
///
/// `wakeups_out` is a caller-allocated buffer of capacity `max_wakeups`.
/// Returns the number of wakeups written (bounded by `max_wakeups`).
/// If more than `max_wakeups` events are ready, the excess are dropped
/// — caller can re-poll with `max_wait_nanos = 0` to drain.
///
/// **Caller invariant:** only one thread calls this at a time —
/// typically the scheduler's dedicated event-loop thread. Concurrent
/// calls serialize through the global mutex (correct but contended).
///
/// # Safety
///
/// `wakeups_out` must point to a writable buffer of at least
/// `max_wakeups` × `sizeof(KaracWakeup)` bytes. `max_wakeups = 0`
/// with a null `wakeups_out` is permitted (no writes).
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_event_loop_poll(
    max_wait_nanos: i64,
    wakeups_out: *mut KaracWakeup,
    max_wakeups: usize,
) -> usize {
    let max_wait = match max_wait_nanos {
        -1 => None,
        n if n > 0 => Some(Duration::from_nanos(n as u64)),
        _ => Some(Duration::ZERO),
    };
    let mut guard = lock_global_event_loop();
    let wakeups = match guard.run_once(max_wait) {
        Ok(w) => w,
        Err(_) => return 0,
    };
    let n = wakeups.len().min(max_wakeups);
    for (i, w) in wakeups.iter().take(n).enumerate() {
        // SAFETY: the caller's contract is that `wakeups_out` points
        // to a writable buffer of at least `max_wakeups` elements;
        // we write at offset `i < n <= max_wakeups`, so the offset
        // is in bounds.
        unsafe {
            wakeups_out.add(i).write(KaracWakeup {
                token: w.token.0 as u64,
                parked: w.parked,
                direction: w.direction as u8,
            });
        }
    }
    n
}

/// Wake the process-global event loop from a non-event-loop thread.
///
/// Returns 0 on success, -1 on error. Coalesces with other pending
/// wakes (`mio::Waker`'s documented behavior — multiple wakes between
/// polls produce one event).
///
/// Idempotent before init: if the event loop has not been initialized
/// yet, this triggers init and then wakes. (The new loop has nothing
/// parked, so the wake is a no-op for the next poll but is still
/// "successful.")
#[no_mangle]
pub extern "C" fn karac_runtime_event_loop_wake() -> i32 {
    // Ensure init so EVENT_LOOP_HANDLE is populated.
    let _ = global_event_loop();
    match EVENT_LOOP_HANDLE.get() {
        Some(h) => match h.wake() {
            Ok(()) => 0,
            Err(_) => -1,
        },
        None => -1,
    }
}

// ── Parked-task ABI (Phase 6 line 17 slice 2) ──────────────────────────────
//
// Repr-C ABI codegen-emitted state machines populate at the network-effect
// call boundary. `KaracParkedTask` mirrors `KaracBranch`'s shape — a
// function pointer + an opaque state pointer — but the function returns a
// `KaracPollResult` tag so the runtime can distinguish "task wants to be
// re-polled later" from "task is done." See design.md § Network Event
// Loop and State-Machine Transform > State-Machine Transform —
// Network-Boundary Functions for the lowering shape this ABI implements.

/// Discriminator returned by codegen-emitted poll functions.
///
/// `repr(u8)` pins the discriminant width for the codegen ABI: poll
/// functions return one of `0`, `1`, `2` and Kāra-side code (the future
/// scheduler integration) maps it back to this enum. `#[non_exhaustive]`
/// signals that variants may be added — `Cancelled` is a likely future
/// addition once cooperative cancellation lowering matures — without a
/// breaking ABI change (existing 0 / 1 / 2 keep their discriminants).
#[repr(u8)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum KaracPollResult {
    /// Task wants to be re-polled later. The poll function has registered
    /// itself with the event loop (or a channel, or whatever it is
    /// waiting on) and stored the registration in its state struct.
    Pending = 0,
    /// Task completed successfully. The state struct's return-slot
    /// holds the task's value; the runtime can free the state struct.
    Ready = 1,
    /// Task completed with an error. The state struct's return-slot
    /// holds the error; the runtime can free the state struct.
    Err = 2,
}

/// Parked-task ABI value carrying a state machine's poll function plus
/// the opaque state pointer it operates on.
///
/// Codegen emits this struct at every network-effect call boundary as
/// part of the state-machine lowering (phase 6 line 18). The runtime
/// drives the task by calling `poll_fn(state, cancel)` and inspecting
/// the returned [`KaracPollResult`] discriminant. The state struct's
/// lifetime spans from the network-boundary function's entry until
/// `poll_fn` returns `Ready` or `Err` (or the task is cancelled —
/// codegen emits the state-struct destructor on every exit path).
///
/// **Field layout.** Two pointer-width fields, no padding, `repr(C)`
/// — matches `KaracBranch`'s shape exactly so the runtime can share
/// dispatch machinery between the two task representations when the
/// scheduler integration lands.
///
/// **Cancellation.** The `*const AtomicBool` cancel pointer passed at
/// each poll mirrors the [`KaracBranch::func`] convention. The poll
/// function reads it at every yield point to observe cooperative
/// cancellation; on a true read, the function unwinds via the state
/// struct's destructor and returns `Err` (or a future `Cancelled`
/// variant — see [`KaracPollResult`]'s `non_exhaustive`).
#[repr(C)]
pub struct KaracParkedTask {
    pub poll_fn: unsafe extern "C" fn(*mut c_void, *const std::sync::atomic::AtomicBool) -> u8,
    pub state: *mut c_void,
}

// SAFETY: KaracParkedTask is `Send` because the codegen-emitted state
// struct's captured locals are subject to the cross-task-safe check
// when the surrounding network-boundary function is itself called
// from a `par {}` / `spawn()` boundary (the existing structural
// cross-task-safe enumeration covers this — see design.md §
// Structured Concurrency Lifetime Guarantees). The state pointer is
// type-erased here; soundness comes from the codegen check, not from
// runtime inspection.
unsafe impl Send for KaracParkedTask {}

#[cfg(test)]
mod tests {
    use super::*;
    use mio::net::TcpListener;
    use std::net::SocketAddr;
    use std::thread;

    /// Serializes FFI tests within a single test binary. The FFI entry
    /// points go through the **process-global** event loop, so two FFI
    /// tests running in parallel race on its state. Acquire this lock
    /// at the start of any test that touches the global event loop
    /// (`karac_runtime_event_loop_*` entries or the parked-task driver
    /// loop below).
    static FFI_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn ffi_test_guard() -> std::sync::MutexGuard<'static, ()> {
        FFI_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

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

    // ── FFI surface (Phase 6 line 17 slice 1) ─────────────────────────
    //
    // The FFI fns go through the **process-global** event loop. Multiple
    // FFI tests share that global, so each acquires `FFI_TEST_LOCK` at
    // entry to serialize within the test binary. Internal-API tests
    // above use locally constructed `EventLoop` instances so they don't
    // need the lock.

    #[cfg(unix)]
    #[test]
    fn ffi_round_trip_register_poll_deregister_wake() {
        let _guard = ffi_test_guard();
        use std::os::fd::AsRawFd;

        // Bind a std-lib listener so we can pull a raw fd; set it
        // non-blocking so accept calls from a worker don't strand on
        // a slow client.
        let std_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        std_listener.set_nonblocking(true).unwrap();
        let local = std_listener.local_addr().unwrap();
        let raw_fd = std_listener.as_raw_fd();

        // Round-trip marker stored in the parked pointer.
        let marker: u64 = 0xC0DE_FACE_C0DE_FACE;
        let parked = std::ptr::addr_of!(marker) as *mut c_void;

        // Register READ direction.
        let token = karac_runtime_event_loop_register_fd(raw_fd, 0, parked);
        assert_ne!(token, 0, "register should return a non-zero token");

        // Invalid direction → returns 0.
        let bad_token = karac_runtime_event_loop_register_fd(raw_fd, 99, parked);
        assert_eq!(bad_token, 0, "invalid direction byte should return 0");

        // Trigger fd readability from another thread.
        let connector = thread::spawn(move || {
            let _stream = std::net::TcpStream::connect(local).unwrap();
            thread::sleep(Duration::from_millis(50));
        });

        // Poll into a caller-allocated buffer. SAFETY: buffer of 4
        // `KaracWakeup`s lives on the stack for the duration of the
        // call.
        let mut buf: [KaracWakeup; 4] = [
            KaracWakeup {
                token: 0,
                parked: std::ptr::null_mut(),
                direction: 0,
            },
            KaracWakeup {
                token: 0,
                parked: std::ptr::null_mut(),
                direction: 0,
            },
            KaracWakeup {
                token: 0,
                parked: std::ptr::null_mut(),
                direction: 0,
            },
            KaracWakeup {
                token: 0,
                parked: std::ptr::null_mut(),
                direction: 0,
            },
        ];
        let n = unsafe {
            karac_runtime_event_loop_poll(
                2_000_000_000, // 2 s safety bound
                buf.as_mut_ptr(),
                buf.len(),
            )
        };
        assert!(n >= 1, "expected at least one fd-readiness wakeup, got {n}");
        let w = &buf[0];
        assert_eq!(w.token, token);
        assert_eq!(w.parked, parked);
        assert_eq!(w.direction, IoDirection::Read as u8);

        connector.join().unwrap();

        // Deregister succeeds; second call on the same fd is harmless
        // at our layer (we silently remove our map entry; mio may error
        // depending on its own state).
        let dereg = karac_runtime_event_loop_deregister_fd(raw_fd, token);
        assert_eq!(dereg, 0, "deregister should report success");

        // Wake is callable; coalesces with any pending wake.
        let wake = karac_runtime_event_loop_wake();
        assert_eq!(wake, 0, "wake should report success");

        // A subsequent poll with a long max_wait should return very
        // quickly because the wake is pending. The returned count
        // may legitimately be 0 (the wake event is filtered out of
        // the wakeups buffer at the EventLoop layer).
        let start = Instant::now();
        let n2 =
            unsafe { karac_runtime_event_loop_poll(2_000_000_000, buf.as_mut_ptr(), buf.len()) };
        let elapsed = start.elapsed();
        assert_eq!(n2, 0, "wake event filtered → empty wakeups");
        assert!(
            elapsed < Duration::from_secs(1),
            "wake should unblock poll well under 1s, took {elapsed:?}"
        );

        // Non-blocking poll returns 0 immediately when nothing is
        // pending.
        let start = Instant::now();
        let n3 = unsafe { karac_runtime_event_loop_poll(0, buf.as_mut_ptr(), buf.len()) };
        let elapsed = start.elapsed();
        assert_eq!(n3, 0);
        assert!(
            elapsed < Duration::from_millis(100),
            "non-blocking poll should return immediately, took {elapsed:?}"
        );
    }

    // ── Parked-task ABI (Phase 6 line 17 slice 2) ─────────────────────

    #[test]
    fn karac_poll_result_discriminants_match_codegen_abi() {
        // Discriminants are part of the codegen ABI — codegen emits raw
        // `u8` returns that the runtime maps back through this enum.
        // Pinning them here catches accidental reordering.
        assert_eq!(KaracPollResult::Pending as u8, 0);
        assert_eq!(KaracPollResult::Ready as u8, 1);
        assert_eq!(KaracPollResult::Err as u8, 2);
        assert_eq!(std::mem::size_of::<KaracPollResult>(), 1);
    }

    #[test]
    fn karac_parked_task_layout_pinned() {
        // Two pointer-width fields, no padding — `repr(C)` shape that
        // codegen will emit a struct literal against.
        let ptr = std::mem::size_of::<usize>();
        assert_eq!(std::mem::size_of::<KaracParkedTask>(), 2 * ptr);
        assert_eq!(std::mem::align_of::<KaracParkedTask>(), ptr);
    }

    // ── End-to-end driver test ────────────────────────────────────────
    //
    // Hand-rolls a 2-state machine that simulates what codegen will emit
    // for a network-boundary function. State 0: register a fd with the
    // event loop, return `Pending`. State 1: deregister, return `Ready`.
    // The test drives the state machine through the FFI surface in a
    // tight loop, proving the full ABI works end-to-end without needing
    // a production scheduler integration.

    #[cfg(unix)]
    #[repr(C)]
    struct HandRolledState {
        tag: u8,
        listener_fd: i32,
        token: u64,
        ready_observed: bool,
    }

    #[cfg(unix)]
    unsafe extern "C" fn hand_rolled_poll_fn(
        state_ptr: *mut c_void,
        _cancel: *const std::sync::atomic::AtomicBool,
    ) -> u8 {
        // SAFETY: the test constructs `state_ptr` as the address of a
        // valid `HandRolledState` stack value living through the entire
        // driver loop below.
        let state = unsafe { &mut *(state_ptr as *mut HandRolledState) };
        match state.tag {
            0 => {
                // Register the listener fd for read readiness.
                let token = karac_runtime_event_loop_register_fd(
                    state.listener_fd,
                    0,
                    std::ptr::null_mut(),
                );
                assert_ne!(token, 0, "register should succeed");
                state.token = token;
                state.tag = 1;
                KaracPollResult::Pending as u8
            }
            1 => {
                // The driver has observed readiness and re-polled us.
                state.ready_observed = true;
                let dereg = karac_runtime_event_loop_deregister_fd(state.listener_fd, state.token);
                assert_eq!(dereg, 0, "deregister should succeed");
                KaracPollResult::Ready as u8
            }
            _ => KaracPollResult::Err as u8,
        }
    }

    #[cfg(unix)]
    #[test]
    fn parked_task_drives_to_completion_through_ffi_surface() {
        let _guard = ffi_test_guard();
        use std::os::fd::AsRawFd;

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let local = listener.local_addr().unwrap();
        let listener_fd = listener.as_raw_fd();

        let mut state = HandRolledState {
            tag: 0,
            listener_fd,
            token: 0,
            ready_observed: false,
        };
        let task = KaracParkedTask {
            poll_fn: hand_rolled_poll_fn,
            state: &mut state as *mut HandRolledState as *mut c_void,
        };
        let cancel = std::sync::atomic::AtomicBool::new(false);

        let connector = thread::spawn(move || {
            // Give the driver a moment to register before the connect
            // makes the listener readable.
            thread::sleep(Duration::from_millis(50));
            let _stream = std::net::TcpStream::connect(local).unwrap();
            thread::sleep(Duration::from_millis(50));
        });

        // Test-only driver loop: invoke poll_fn, pump the event loop on
        // Pending, repeat until Ready / Err. Bounded by an iteration
        // count so a broken state machine fails the test rather than
        // hanging it forever.
        let mut wakeup_buf: [KaracWakeup; 4] = unsafe { std::mem::zeroed() };
        let mut iterations = 0;
        let final_result = loop {
            iterations += 1;
            assert!(iterations <= 8, "driver loop ran more than 8 iterations");

            // SAFETY: poll_fn / state pair is valid for the lifetime of
            // this test fn; `cancel` lives on the stack throughout.
            let raw = unsafe { (task.poll_fn)(task.state, &cancel) };
            if raw == KaracPollResult::Ready as u8 || raw == KaracPollResult::Err as u8 {
                break raw;
            }
            // Pending — drive the event loop. SAFETY: wakeup_buf has
            // 4 entries; bound passed matches.
            let _ = unsafe {
                karac_runtime_event_loop_poll(
                    2_000_000_000,
                    wakeup_buf.as_mut_ptr(),
                    wakeup_buf.len(),
                )
            };
        };

        assert_eq!(final_result, KaracPollResult::Ready as u8);
        assert!(state.ready_observed, "state machine reached the ready arm");
        assert_eq!(state.tag, 1, "state machine ended in state 1");

        connector.join().unwrap();
    }
}
