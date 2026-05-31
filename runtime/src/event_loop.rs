//! Network event loop. Cross-platform abstraction over the OS-level
//! fd-readiness facilities — `epoll` on Linux, `kqueue` on macOS / BSD,
//! `IOCP` on Windows — via the `mio` crate.
//!
//! See `docs/design.md § Network Event Loop and State-Machine Transform`
//! and `docs/implementation_checklist/phase-6-runtime.md` line 15.
//!
//! ## v1 architectural commitments (per phase-6-runtime.md line 15)
//!
//! - **One event loop per process.** v1 runs exactly one loop; M2 / M3
//!   may shard across multiple loops to reach the 1M+ idle-connection
//!   target. The type is `Sync` — shared via `Arc<EventLoop>` from any
//!   thread — with two interior Mutexes that split the polling and
//!   registration code paths so a long-blocking `run_once` (held by
//!   the background poller thread, slice 3) does not block concurrent
//!   register / deregister calls.
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
use std::collections::{HashMap, VecDeque};
use std::ffi::c_void;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread;
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
// required because the `EventLoop` is shared across threads as
// `Arc<EventLoop>`, so the inner `fds` HashMap (storing `FdState`)
// must itself be `Send` to live inside the `Mutex`.
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

/// Event loop. `Sync` — register / deregister / wake from any thread;
/// `run_once` serializes via an interior Mutex.
///
/// Per the v1 architectural commitment, exactly one loop runs per
/// process. The interior splits state into two independently-locked
/// halves so the long-blocking `run_once` (which holds the `poll`
/// Mutex through the entire `mio::Poll::poll` call) does **not**
/// block registration (which acquires only the `fds` Mutex briefly).
/// This is what makes the background-poller architecture (slice 3)
/// safe: the poller thread blocks indefinitely in `run_once` while
/// other threads continue to register / deregister fds against the
/// same loop.
pub struct EventLoop {
    /// Owned clone of `mio::Poll`'s registry. `mio::Registry` is
    /// `Sync`, so register / deregister calls from arbitrary threads
    /// hit the OS-level registration syscalls without external
    /// synchronization — the only thing we lock is the `fds`
    /// HashMap below.
    registry: mio::Registry,
    /// Cross-thread waker handle. `mio::Waker` is `Sync` (uses
    /// eventfd / pipe / IOCP-post under the hood).
    waker: Arc<mio::Waker>,
    /// Poll instance + events buffer. Only `run_once` touches this;
    /// the Mutex enforces single-polling-thread-at-a-time.
    poll: Mutex<EventLoopPoll>,
    /// Per-fd state + token allocator. Briefly locked by register /
    /// deregister, and during the post-poll wakeup-extraction phase
    /// of `run_once`.
    fds: Mutex<EventLoopFds>,
}

struct EventLoopPoll {
    poll: Poll,
    events: Events,
}

struct EventLoopFds {
    by_token: HashMap<Token, FdState>,
    /// Monotonically increasing source of unique tokens. Reserved
    /// values: `0` is the cross-thread waker (see [`WAKER_TOKEN`]);
    /// user-fd tokens start at `1`.
    next_token: usize,
}

const WAKER_TOKEN: Token = Token(0);

impl EventLoop {
    /// Construct a new event loop. Allocates the underlying `mio::Poll`,
    /// clones its registry handle, and registers the cross-thread waker.
    pub fn new() -> io::Result<Self> {
        let poll = Poll::new()?;
        let registry = poll.registry().try_clone()?;
        let waker = Arc::new(mio::Waker::new(poll.registry(), WAKER_TOKEN)?);
        Ok(EventLoop {
            registry,
            waker,
            poll: Mutex::new(EventLoopPoll {
                poll,
                events: Events::with_capacity(256),
            }),
            fds: Mutex::new(EventLoopFds {
                by_token: HashMap::new(),
                next_token: 1,
            }),
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
    ///
    /// Acquires only the `fds` Mutex briefly — concurrent `run_once`
    /// calls are unaffected (different Mutex).
    pub fn register<S: mio::event::Source + ?Sized>(
        &self,
        source: &mut S,
        direction: IoDirection,
        deadline: Option<Instant>,
        parked: *mut c_void,
    ) -> io::Result<RegistrationToken> {
        let mut fds = self.fds.lock().unwrap_or_else(|p| p.into_inner());
        let token = Token(fds.next_token);
        fds.next_token = fds
            .next_token
            .checked_add(1)
            .expect("event loop token exhaustion (usize wrap)");
        // mio::Registry is Sync — safe to call without holding any
        // additional lock. We still hold the fds lock through this
        // call so the HashMap insert and OS-level registration appear
        // atomic to other threads.
        self.registry
            .register(source, token, direction.to_interest())?;
        fds.by_token.insert(
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
    ///
    /// Acquires only the `fds` Mutex briefly.
    pub fn deregister<S: mio::event::Source + ?Sized>(
        &self,
        source: &mut S,
        token: RegistrationToken,
    ) -> io::Result<()> {
        let mut fds = self.fds.lock().unwrap_or_else(|p| p.into_inner());
        self.registry.deregister(source)?;
        fds.by_token.remove(&Token(token.0));
        Ok(())
    }

    /// Atomically remove a registration by token and return its `parked`
    /// pointer, or `None` if the token is no longer registered.
    ///
    /// This is the one-shot dispatch primitive: the scheduler dispatcher
    /// calls it to claim a wakeup's task. Removing the token from
    /// `by_token` under the `fds` lock guarantees that (a) a duplicate or
    /// stale wakeup for the same token resolves to `None` and is skipped
    /// (so the task's `poll_fn` runs at most once per registration), and
    /// (b) the returned `parked` pointer reflects the *current* live
    /// registration, never a value captured into a since-superseded
    /// wakeup. Combined with the caller-frees-after-signal lifetime
    /// contract, this is what prevents the dispatcher from dereferencing a
    /// parked record that another thread has freed.
    ///
    /// Does NOT touch the OS-level epoll/kqueue registration — the caller
    /// path issues `deregister` (with the fd) after its park completes;
    /// since `run_once` skips events whose token isn't in `by_token`, the
    /// removal here already stops further wakeups in the interim.
    pub fn take_registration(&self, token: RegistrationToken) -> Option<*mut c_void> {
        let mut fds = self.fds.lock().unwrap_or_else(|p| p.into_inner());
        fds.by_token
            .remove(&Token(token.0))
            .map(|state| state.parked)
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
    ///
    /// **Locking.** Holds the `poll` Mutex throughout (so only one
    /// thread polls at a time). Acquires the `fds` Mutex briefly
    /// after the poll syscall returns, to translate ready events
    /// into [`Wakeup`]s. Lock order is consistently poll → fds.
    pub fn run_once(&self, max_wait: Option<Duration>) -> io::Result<Vec<Wakeup>> {
        let mut poll_guard = self.poll.lock().unwrap_or_else(|p| p.into_inner());
        let EventLoopPoll {
            ref mut poll,
            ref mut events,
        } = *poll_guard;
        poll.poll(events, max_wait)?;
        let fds = self.fds.lock().unwrap_or_else(|p| p.into_inner());
        let mut wakeups = Vec::new();
        for event in events.iter() {
            if event.token() == WAKER_TOKEN {
                continue;
            }
            let Some(state) = fds.by_token.get(&event.token()) else {
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
        self.fds
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .by_token
            .len()
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
// **Threading model (v1, slice 3+).** The process-global event loop is
// stored as an `Arc<EventLoop>` and is itself `Sync`. Register /
// deregister calls from any thread acquire only the inner `fds` Mutex
// briefly; `run_once` acquires the inner `poll` Mutex for the duration
// of the blocking poll. Because the two locks are independent, a
// long-blocking poll (the background poller thread in slice 3+) does
// not block concurrent registrations.
//
// **Platform scope.** The fd-registration FFI fns are `#[cfg(unix)]`
// only — Linux / macOS / BSD. Windows IOCP uses a completion-based
// model rather than fd-readiness, so its FFI surface looks different
// and lands separately. The `poll` and `wake` fns are cross-platform
// (mio's `Poll` / `Waker` work everywhere).

/// Process-global event loop instance, lazily initialized.
/// Per the v1 architectural commitment: exactly one EventLoop per process.
static EVENT_LOOP: OnceLock<Arc<EventLoop>> = OnceLock::new();

/// Cached handle to the process-global event loop's waker. Populated
/// during the same `OnceLock::get_or_init` that constructs `EVENT_LOOP`,
/// so observing `EVENT_LOOP` initialized implies `EVENT_LOOP_HANDLE` is
/// also set.
static EVENT_LOOP_HANDLE: OnceLock<EventLoopHandle> = OnceLock::new();

fn global_event_loop() -> &'static Arc<EventLoop> {
    EVENT_LOOP.get_or_init(|| {
        let ev = EventLoop::new().expect("karac_runtime: process-global event loop init failed");
        let arc = Arc::new(ev);
        // `set` may already have been populated by a racing initializer if
        // two threads called this concurrently; the `OnceLock::get_or_init`
        // contract guarantees we are the unique initializer of `EVENT_LOOP`,
        // but the handle write is a separate `OnceLock`, so ignore a
        // duplicate-set error.
        let _ = EVENT_LOOP_HANDLE.set(arc.handle());
        arc
    })
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

// SAFETY: `parked` is opaque to the runtime — stored at register time
// and handed back through this value at wakeup time. The original
// caller (codegen parking path) owns the pointer's lifetime and any
// thread-safety concerns; the runtime moves `KaracWakeup` across
// threads only when the background poller (slice 3) queues a wakeup
// for consumption by a scheduler thread, and the pointer crosses
// unchanged.
unsafe impl Send for KaracWakeup {}

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
    let ev = global_event_loop();
    match ev.register(&mut source, dir, None, parked) {
        Ok(token) => {
            // Wake the background poller so it re-evaluates its interest
            // list. Without this, a poller blocked in `run_once(None)`
            // (epoll_wait / kevent) would not observe a freshly-registered
            // fd until some *other* fd fires — and an fd that is already
            // readable at registration time (e.g. a listener re-armed in an
            // accept loop while connections sit in the backlog) would be
            // silently missed, wedging the task parked on it.
            //
            // **Gated on a background poller actually existing.** mio's
            // waker is coalescing AND edge-triggered: a `wake()` issued
            // before any `poll()` call leaves a pending waker event that
            // the NEXT `poll()` consumes immediately (returning 0 user-
            // facing wakeups, since WAKER_TOKEN is filtered out in
            // `run_once`). That breaks the synchronous-FFI case where the
            // same thread registers an fd and then calls
            // `karac_runtime_event_loop_poll` itself: the prefired wake
            // would race ahead of any real readiness event, returning 0
            // wakeups well under the caller's `max_wait_nanos`. Gating
            // the wake on `BACKGROUND_POLLER` being installed restricts
            // the wake to the only state it's needed for (a poller
            // blocked in `run_once`) and leaves the synchronous-poll
            // path race-free.
            if BACKGROUND_POLLER
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .is_some()
            {
                if let Some(h) = EVENT_LOOP_HANDLE.get() {
                    let _ = h.wake();
                }
            }
            token.0 as u64
        }
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
    let ev = global_event_loop();
    match ev.deregister(&mut source, RegistrationToken(token as usize)) {
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
    // If the background poller thread is running it owns polling — direct
    // FFI poll callers get back an empty result so they fall through to
    // `karac_runtime_event_loop_take_wakeups` instead of contending for
    // the inner poll Mutex (which the background thread holds for the
    // duration of its blocking call).
    if BACKGROUND_POLLER
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .is_some()
    {
        return 0;
    }
    let ev = global_event_loop();
    let wakeups = match ev.run_once(max_wait) {
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

// ── Background event-loop poller + wakeup queue (Phase 6 line 17 slice 3) ──
//
// An opt-in background thread that owns event-loop polling. Once started,
// the thread loops on `EventLoop::run_once(None)` indefinitely (blocking
// in mio's poll inside the inner `poll` Mutex), depositing wakeups into
// an internal `VecDeque<KaracWakeup>` for consumption by a scheduler
// thread via `karac_runtime_event_loop_take_wakeups`.
//
// **No deadlock with registration.** The `EventLoop` refactor that
// landed alongside this section splits the inner state into two
// independent Mutexes — `poll` (held by the background thread for the
// duration of each blocking poll) and `fds` (held only briefly by
// register / deregister). Concurrent `karac_runtime_event_loop_register_fd`
// calls from any thread acquire only the `fds` Mutex, so the long-blocking
// poll does not stall registration.
//
// **Direct FFI poll coexistence.** While the background poller is running,
// direct `karac_runtime_event_loop_poll` callers short-circuit to return
// 0 immediately — the background poller has authoritative ownership of
// the polling channel and direct callers should drain via `take_wakeups`
// instead. Documented in `karac_runtime_event_loop_poll`'s body.
//
// **Shutdown protocol.** `karac_runtime_event_loop_shutdown_background_thread`
// sets the shutdown flag, fires the cross-thread `wake()` to unblock the
// current poll call, signals the queue's `Condvar` to release any
// waiting `take_wakeups` callers, joins the thread, and clears the
// global slot. Idempotent — calling on a non-running thread returns -1
// without side effects, so a re-start after shutdown is supported within
// the same process.

/// Internal poller state. Held inside `Arc` so the spawned thread can
/// share it with the global slot.
struct EventLoopPoller {
    event_loop: Arc<EventLoop>,
    queue: Mutex<VecDeque<KaracWakeup>>,
    notify: Condvar,
    shutdown: AtomicBool,
    /// `JoinHandle` for the spawned thread. Wrapped in `Mutex<Option<_>>`
    /// so the shutdown path can `take()` it independently of the rest
    /// of the poller state.
    handle: Mutex<Option<thread::JoinHandle<()>>>,
}

/// Global slot for the background poller. `None` until the first
/// `karac_runtime_event_loop_start_background_thread` call; cleared
/// back to `None` on shutdown so the thread can be re-started later
/// within the same process. `Mutex<Option<Arc<_>>>` rather than
/// `OnceLock` for exactly this restart capability.
static BACKGROUND_POLLER: Mutex<Option<Arc<EventLoopPoller>>> = Mutex::new(None);

fn lock_background_poller_slot() -> std::sync::MutexGuard<'static, Option<Arc<EventLoopPoller>>> {
    BACKGROUND_POLLER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn poller_thread_main(poller: Arc<EventLoopPoller>) {
    while !poller.shutdown.load(Ordering::Acquire) {
        let wakeups = match poller.event_loop.run_once(None) {
            Ok(w) => w,
            Err(_) => {
                // Treat transient poll errors as a yield — re-check
                // shutdown and continue.
                continue;
            }
        };
        if wakeups.is_empty() {
            continue;
        }
        let mut q = poller.queue.lock().unwrap_or_else(|p| p.into_inner());
        for w in wakeups {
            q.push_back(KaracWakeup {
                token: w.token.0 as u64,
                parked: w.parked,
                direction: w.direction as u8,
            });
        }
        drop(q);
        poller.notify.notify_all();
    }
}

/// Start the background event-loop poller thread.
///
/// Idempotent: a second call while the thread is already running
/// returns 0 without re-spawning. Returns 0 on success.
#[no_mangle]
pub extern "C" fn karac_runtime_event_loop_start_background_thread() -> i32 {
    let mut slot = lock_background_poller_slot();
    if slot.is_some() {
        return 0;
    }
    let event_loop = Arc::clone(global_event_loop());
    let poller = Arc::new(EventLoopPoller {
        event_loop,
        queue: Mutex::new(VecDeque::new()),
        notify: Condvar::new(),
        shutdown: AtomicBool::new(false),
        handle: Mutex::new(None),
    });
    let poller_for_thread = Arc::clone(&poller);
    let join = thread::Builder::new()
        .name("karac-event-loop".to_string())
        .spawn(move || poller_thread_main(poller_for_thread))
        .expect("karac_runtime: failed to spawn event-loop poller thread");
    *poller.handle.lock().unwrap_or_else(|p| p.into_inner()) = Some(join);
    *slot = Some(poller);
    0
}

/// Drain up to `max` wakeups from the background poller's queue into
/// the caller's buffer.
///
/// `timeout_nanos`:
/// - `-1`: block indefinitely until at least one wakeup arrives.
/// - `0`: non-blocking — return immediately, even if the queue is empty.
/// - `n > 0`: block up to `n` nanoseconds.
/// - Any other negative value: treated as 0 (non-blocking).
///
/// Returns the number of wakeups written. 0 means "queue was empty at
/// timeout" (or the background thread is not running).
///
/// # Safety
///
/// `out` must point to a writable buffer of at least `max ×
/// sizeof(KaracWakeup)` bytes. `max = 0` with `out = null` is permitted
/// (no writes).
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_event_loop_take_wakeups(
    out: *mut KaracWakeup,
    max: usize,
    timeout_nanos: i64,
) -> usize {
    let poller = {
        let slot = lock_background_poller_slot();
        match slot.as_ref() {
            Some(p) => Arc::clone(p),
            None => return 0,
        }
    };
    let mut q = poller.queue.lock().unwrap_or_else(|p| p.into_inner());
    if q.is_empty() {
        match timeout_nanos {
            -1 => {
                q = poller.notify.wait(q).unwrap_or_else(|p| p.into_inner());
            }
            n if n > 0 => {
                let (g, _) = poller
                    .notify
                    .wait_timeout(q, Duration::from_nanos(n as u64))
                    .unwrap_or_else(|p| p.into_inner());
                q = g;
            }
            _ => {
                // Non-blocking — return empty.
            }
        }
    }
    let mut n_out = 0;
    while n_out < max {
        match q.pop_front() {
            Some(w) => {
                // SAFETY: caller's contract — `out` is writable for
                // `max` entries; we write at offset `n_out < max`.
                unsafe {
                    out.add(n_out).write(w);
                }
                n_out += 1;
            }
            None => break,
        }
    }
    n_out
}

/// Signal the background poller thread to stop, unblock its `poll`
/// call via the cross-thread waker, join the thread, and clear the
/// global slot.
///
/// Returns 0 on success, -1 if no background thread is running.
/// A second shutdown after a successful shutdown returns -1 (the slot
/// is empty).
#[no_mangle]
pub extern "C" fn karac_runtime_event_loop_shutdown_background_thread() -> i32 {
    let poller = {
        let mut slot = lock_background_poller_slot();
        match slot.take() {
            Some(p) => p,
            None => return -1,
        }
    };
    poller.shutdown.store(true, Ordering::Release);
    let _ = karac_runtime_event_loop_wake();
    poller.notify.notify_all();
    let join = poller
        .handle
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .take();
    if let Some(h) = join {
        let _ = h.join();
    }
    // Drain any pending waker event. If the poller thread observed
    // the shutdown flag *before* our `wake()` was delivered to its
    // `poll()` call (i.e., the thread had already returned from one
    // `run_once` and was about to check the flag for the next loop
    // iteration), mio's edge-armed waker leaves the event pending —
    // the next thread to call `poll()` would receive it as a spurious
    // empty wakeup. A non-blocking `run_once` here consumes it and
    // leaves the event loop in a known-clean state for follow-up
    // callers. BACKGROUND_POLLER is already None at this point (we
    // took the Arc out at the top of this fn), so this `run_once`
    // doesn't compete with any background polling.
    let ev = global_event_loop();
    let _ = ev.run_once(Some(Duration::ZERO));
    0
}

// ── Test-only FFI (Phase 6 line 17 park-and-wake E2E) ─────────────────────
//
// `karac_runtime_test_bind_and_print_port` exists so a kara binary can
// exercise the parking primitive (`karac_park_on_fd`) end-to-end
// without a real stdlib `TcpListener` (M1 stdlib work). It binds a TCP
// listener on 127.0.0.1:0, prints `BOUND_PORT=<n>` to stdout (the
// `tests/http_server.rs` precedent for port-readback), and returns the
// raw fd — leaking the listener so the fd stays open for the parking
// primitive to register against. The test harness reads the port,
// connects to it from a worker thread to trigger readability, and
// asserts the binary returns Ready and exits cleanly. Behind a cargo
// feature so the symbol never lands in production binaries; the test
// harness builds with `--features test-helpers`.

/// Bind a TCP listener on 127.0.0.1:0, print `BOUND_PORT=<port>` to
/// stdout, leak the listener (so the fd outlives this call), and
/// return the raw fd. Returns -1 on failure.
///
/// Unix-only — matches the `karac_runtime_event_loop_register_fd` /
/// `_deregister_fd` `#[cfg(unix)]` gate (raw-fd model). Windows IOCP
/// integration is a separate slice (different fd model).
#[cfg(all(unix, feature = "test-helpers"))]
#[no_mangle]
pub extern "C" fn karac_runtime_test_bind_and_print_port() -> i32 {
    use std::os::unix::io::IntoRawFd;
    let listener = match std::net::TcpListener::bind("127.0.0.1:0") {
        Ok(l) => l,
        Err(_) => return -1,
    };
    let port = match listener.local_addr() {
        Ok(addr) => addr.port(),
        Err(_) => return -1,
    };
    println!("BOUND_PORT={port}");
    // Flush so the harness's BufReader sees the line promptly; the
    // binary will immediately call `karac_park_on_fd` after this and
    // block in `take_wakeups`, so without a flush the line could sit
    // in the stdout buffer indefinitely.
    use std::io::Write;
    let _ = std::io::stdout().flush();
    // IntoRawFd consumes the listener and returns the raw fd without
    // running the destructor — equivalent to mem::forget + as_raw_fd
    // but with no double-ownership window.
    listener.into_raw_fd()
}

// ── TCP listener FFI (stdlib `TcpListener.bind` / `.accept`) ──────────────
//
// Two always-on FFIs (no feature gate) backing `runtime/stdlib/tcp.kara`'s
// `TcpListener.bind(addr) -> TcpListener` and `TcpListener.accept(self)
// -> i32`. The codegen lowering for `TcpListener.accept` calls
// `karac_park_on_fd(self.fd, 0u8)` *before* invoking
// `karac_runtime_tcp_accept` so the parking happens at the kara state-
// machine level; this FFI does the *raw* accept(2) only — no parking,
// no event-loop interaction.
//
// **BOUND_PORT convention.** When the address is `127.0.0.1:0` (or any
// other ephemeral-port form), `karac_runtime_tcp_bind` emits a
// `BOUND_PORT=<n>\n` line to stdout before returning, matching the
// established v1 convention from `Server.serve_static`. Smoke tests
// read the port back from stdout.

/// Default `listen(2)` backlog for `karac_runtime_tcp_bind`. The kernel
/// silently caps this at the system tunable (`somaxconn`), so passing
/// a large literal lets the OS pick the real ceiling without the
/// runtime needing to read it. 65535 matches what nginx / envoy use
/// by default and is the value the M3 1M-connection benchmark wants
/// (see phase-6-runtime.md line 209 follow-on for the diagnosis —
/// SYN-cookie fallback at ~93K conns, 17% churn-timeout rate — and
/// `docs/investigations/demo1_m3_verification.json` for the gate).
///
/// **macOS cap.** Darwin's `solisten()` silently corrupts the listen
/// queue when the requested backlog crosses the `i16` boundary
/// (32768) — `kern.ipc.somaxconn` advertises 65535 but the actual
/// kernel path turns the listener into a SYN black hole at that
/// value (`connect(2)` returns ETIMEDOUT, no SYN_RCVD entry shows
/// up in netstat, and accept(2) never fires). Confirmed by
/// `tcp_bind_produces_connectable_listener` regression: 16384 works,
/// 32768 hangs, 65535 hangs. Cap macOS at 16384 — well above any
/// realistic single-instance demo workload and below the broken
/// threshold. Linux is unaffected and stays at 65535. 2026-05-29.
#[cfg(target_os = "macos")]
const KARAC_RUNTIME_TCP_LISTEN_BACKLOG: i32 = 16384;
#[cfg(not(target_os = "macos"))]
const KARAC_RUNTIME_TCP_LISTEN_BACKLOG: i32 = 65535;

/// Bind a TCP listener on `addr` (e.g. `"127.0.0.1:0"` for ephemeral-
/// port binding). On success, print `BOUND_PORT=<port>` to stdout if
/// the bound port was ephemeral (caller asked for `:0`), then return
/// the raw fd via `IntoRawFd::into_raw_fd` (no destructor — the fd
/// outlives this call so the caller can park-and-accept against it).
///
/// `addr_ptr` + `addr_len` are a borrowed byte slice (Kāra `String`
/// shape — not null-terminated). Returns -1 on UTF-8 / parse / bind
/// failure.
///
/// Unix-only — matches the `#[cfg(unix)]` gate on the rest of the
/// raw-fd FFI surface. Windows IOCP integration is a separate slice.
///
/// # Safety
///
/// `addr_ptr` must point to a readable buffer of at least `addr_len`
/// bytes (`addr_ptr` + `addr_len` describing a `&[u8]` that lives for
/// the duration of the call) OR `addr_ptr` may be null in which case
/// `addr_len` must be `0` (the function returns -1 in this case).
/// The buffer is read once during the call and not retained.
#[cfg(unix)]
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_tcp_bind(addr_ptr: *const u8, addr_len: i64) -> i32 {
    use socket2::{Domain, Socket, Type};
    use std::os::unix::io::IntoRawFd;
    if addr_ptr.is_null() || addr_len <= 0 {
        return -1;
    }
    let bytes = std::slice::from_raw_parts(addr_ptr, addr_len as usize);
    let addr_str = match std::str::from_utf8(bytes) {
        Ok(s) => s,
        Err(_) => return -1,
    };
    // Parse the addr as a literal SocketAddr (host:port with host an IP
    // literal). The kara stdlib never passes hostnames to this FFI — the
    // demos use `127.0.0.1:0` / `0.0.0.0:<port>` shape — so a strict
    // parse here is fine and avoids dragging DNS resolution into the
    // listen-bind path.
    let socket_addr: std::net::SocketAddr = match addr_str.parse() {
        Ok(a) => a,
        Err(_) => return -1,
    };
    let domain = if socket_addr.is_ipv4() {
        Domain::IPV4
    } else {
        Domain::IPV6
    };
    let socket = match Socket::new(domain, Type::STREAM, None) {
        Ok(s) => s,
        Err(_) => return -1,
    };
    // SO_REUSEADDR mirrors what std::net::TcpListener::bind sets
    // implicitly on Unix; preserve that for parity with prior behavior.
    let _ = socket.set_reuse_address(true);
    if socket.bind(&socket_addr.into()).is_err() {
        return -1;
    }
    if socket.listen(KARAC_RUNTIME_TCP_LISTEN_BACKLOG).is_err() {
        return -1;
    }
    let listener: std::net::TcpListener = socket.into();
    // Only print BOUND_PORT for ephemeral-port binds; a fixed-port
    // bind doesn't need the readback since the caller already knows
    // the port. Treat `addr_str` ending in `:0` (or `:00...`) as the
    // ephemeral marker — the cheapest correct check is to look at
    // the bound port relative to the requested port.
    if addr_str.rsplit(':').next() == Some("0") {
        if let Ok(local) = listener.local_addr() {
            println!("BOUND_PORT={}", local.port());
            use std::io::Write;
            let _ = std::io::stdout().flush();
        }
    }
    listener.into_raw_fd()
}

/// Raw `accept(2)` on a listener fd. Does NOT park — the caller is
/// expected to have already parked via `karac_park_on_fd(listener_fd,
/// 0)` so the listener is known readable. Returns the new connection
/// fd on success, -1 on failure (incl. `EAGAIN` / `EWOULDBLOCK` —
/// which signals the readiness assumption was wrong).
///
/// The accepted socket is returned via `IntoRawFd::into_raw_fd` (no
/// destructor — caller owns the close on drop).
#[cfg(unix)]
#[no_mangle]
pub extern "C" fn karac_runtime_tcp_accept(listener_fd: i32) -> i32 {
    use std::os::unix::io::{FromRawFd, IntoRawFd};
    if listener_fd < 0 {
        return -1;
    }
    // SAFETY: the listener_fd must come from a successful
    // `karac_runtime_tcp_bind` call (or equivalent). We construct a
    // borrowed TcpListener via from_raw_fd, accept() through it, then
    // immediately into_raw_fd() to give the fd back without running
    // the destructor (the listener stays open for further accepts).
    let listener = unsafe { std::net::TcpListener::from_raw_fd(listener_fd) };
    let result = match listener.accept() {
        Ok((conn, _addr)) => conn.into_raw_fd(),
        Err(_) => -1,
    };
    // Release ownership of the listener fd back to the caller.
    let _ = listener.into_raw_fd();
    result
}

/// Open a plain-TCP client connection to `addr` (an `IP:port` literal,
/// e.g. `127.0.0.1:8080`) and return the connected socket fd, or `-1`
/// on UTF-8 / parse / connect failure. The client mirror of
/// `karac_runtime_tcp_bind`'s server side — the only TCP *initiation*
/// primitive (accept handles inbound; this handles outbound).
///
/// v1 does a blocking `connect(2)` inline (same posture as the TLS
/// client `karac_runtime_tls_client_connect`, which this is the
/// handshake-free subset of). The returned fd backs a `TcpStream`, so
/// subsequent `karac_runtime_tcp_read` / `_write` reach it through the
/// same park-then-syscall path an accepted fd uses.
///
/// The fd is returned via `IntoRawFd::into_raw_fd` (no destructor —
/// the caller owns the close on `TcpStream` drop).
///
/// Returns a bare `-1` on every failure at v1; enriching to `-errno`
/// (so callers can branch `ConnectionRefused` vs fatal) is phase-8
/// line 74.
///
/// # Safety
///
/// `addr_ptr` must point to `addr_len` readable bytes for the duration
/// of the call (the kara `String`'s `{ptr, len}`), or be null with
/// `addr_len <= 0` (rejected via the early return). The bytes are read
/// once and not retained.
#[cfg(unix)]
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_tcp_connect(addr_ptr: *const u8, addr_len: i64) -> i32 {
    use std::os::unix::io::IntoRawFd;
    if addr_ptr.is_null() || addr_len <= 0 {
        return -1;
    }
    let bytes = std::slice::from_raw_parts(addr_ptr, addr_len as usize);
    let addr_str = match std::str::from_utf8(bytes) {
        Ok(s) => s,
        Err(_) => return -1,
    };
    // Strict literal `SocketAddr` parse — same posture as
    // `karac_runtime_tcp_bind` / `_tls_client_connect`: the stdlib
    // never hands a hostname to this FFI, so no DNS resolution path.
    let socket_addr: std::net::SocketAddr = match addr_str.parse() {
        Ok(a) => a,
        Err(_) => return -1,
    };
    match std::net::TcpStream::connect(socket_addr) {
        Ok(sock) => sock.into_raw_fd(),
        Err(_) => -1,
    }
}

// ── TCP stream read/write FFI (stdlib `TcpStream.read` / `.write`) ────────
//
// Always-on FFIs (no feature gate) backing `runtime/stdlib/tcp.kara`'s
// `TcpStream.read(self, mut Slice[u8]) -> i64` and
// `TcpStream.write(self, Slice[u8]) -> i64`. Same convention as
// `karac_runtime_tcp_accept`: the codegen lowering parks via
// `karac_park_on_fd(self.fd, direction)` BEFORE invoking these — so the
// FFIs themselves are pure-syscall (no parking, no event-loop
// interaction). Returns byte count on success; -1 on failure.

/// Raw `read(2)` on a connection fd into the caller-provided buffer.
/// Does NOT park — the caller (codegen lowering for `TcpStream.read`)
/// is expected to have already parked via
/// `karac_park_on_fd(stream_fd, 0)` so the connection is known
/// read-ready. Returns the byte count read on success (0 on clean
/// EOF) or `-errno` on syscall failure — slice 9b's `Result[i64,
/// TcpError]` wrapping decodes the negative return into the
/// matching `TcpError` variant (Interrupted for EINTR=4,
/// Other(errno) otherwise). `EAGAIN` / `EWOULDBLOCK` surface as
/// `-EAGAIN` here too (the readiness assumption was wrong); the
/// parking primitive's readiness check should normally prevent it.
///
/// Unix-only.
///
/// # Safety
///
/// `buf_ptr` must point to a writable buffer of at least `buf_len`
/// bytes that lives for the duration of the call OR `buf_ptr` may be
/// null in which case `buf_len` must be `0` (the function returns 0
/// in this case). The buffer is written to once during the call and
/// not retained.
#[cfg(unix)]
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_tcp_read(
    stream_fd: i32,
    buf_ptr: *mut u8,
    buf_len: i64,
) -> i64 {
    use std::io::Read;
    use std::os::unix::io::{FromRawFd, IntoRawFd};
    if stream_fd < 0 {
        return -1;
    }
    if buf_ptr.is_null() || buf_len <= 0 {
        return 0;
    }
    let buf = std::slice::from_raw_parts_mut(buf_ptr, buf_len as usize);
    // SAFETY: the stream_fd must come from a successful
    // `karac_runtime_tcp_accept` call (or equivalent). Borrowed
    // TcpStream wrapper avoids destructor while reading.
    let mut stream = std::net::TcpStream::from_raw_fd(stream_fd);
    let result = match stream.read(buf) {
        Ok(n) => n as i64,
        Err(e) => {
            let errno = e.raw_os_error().unwrap_or(1);
            if errno > 0 {
                -(errno as i64)
            } else {
                -1
            }
        }
    };
    // Release ownership of the stream fd back to the caller.
    let _ = stream.into_raw_fd();
    result
}

/// Raw `write(2)` on a connection fd from the caller-provided
/// buffer. Does NOT park — the caller (codegen lowering for
/// `TcpStream.write`) is expected to have already parked via
/// `karac_park_on_fd(stream_fd, 1)` so the connection is known
/// write-ready. Returns the byte count written on success or
/// `-errno` on syscall failure — symmetric with `tcp_read`. Slice
/// 9b's `Result[i64, TcpError]` wrapping decodes the negative
/// return into the matching `TcpError` variant.
///
/// v1 issues a single `write(2)` call — partial writes return the
/// short count, the caller can loop if needed. A future
/// `write_all` variant (slice 9c) wraps the loop using the
/// Interrupted/Other distinction from `TcpError`.
///
/// Unix-only.
///
/// # Safety
///
/// `buf_ptr` must point to a readable buffer of at least `buf_len`
/// bytes that lives for the duration of the call OR `buf_ptr` may
/// be null in which case `buf_len` must be `0` (the function
/// returns 0 in this case). The buffer is read once during the
/// call and not retained.
#[cfg(unix)]
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_tcp_write(
    stream_fd: i32,
    buf_ptr: *const u8,
    buf_len: i64,
) -> i64 {
    use std::io::Write;
    use std::os::unix::io::{FromRawFd, IntoRawFd};
    if stream_fd < 0 {
        return -1;
    }
    if buf_ptr.is_null() || buf_len <= 0 {
        return 0;
    }
    let buf = std::slice::from_raw_parts(buf_ptr, buf_len as usize);
    // SAFETY: the stream_fd must come from a successful
    // `karac_runtime_tcp_accept` call (or equivalent). Borrowed
    // TcpStream wrapper avoids destructor while writing.
    let mut stream = std::net::TcpStream::from_raw_fd(stream_fd);
    let result = match stream.write(buf) {
        Ok(n) => n as i64,
        Err(e) => {
            let errno = e.raw_os_error().unwrap_or(1);
            if errno > 0 {
                -(errno as i64)
            } else {
                -1
            }
        }
    };
    // Release ownership of the stream fd back to the caller.
    let _ = stream.into_raw_fd();
    result
}

// ── TCP close FFI (stdlib `TcpStream` / `TcpListener` Drop dispatch) ─────
//
// Phase 6 line 17 slice 9d. Called by the codegen-emitted bodies of
// `@TcpStream.drop` and `@TcpListener.drop` when a kara binding goes
// out of scope. `close(2)` releases the kernel-side socket resource;
// without this the per-connection fd leaks until process exit (the
// kernel reaps fds on `_exit`, but inside a long-running server the
// fd table eventually fills).
//
// **Idempotence and double-close.** A `-1` fd is a no-op (returns 0,
// matching the per-method convention of using `-1` as the "no-fd"
// sentinel: `bind` returns `TcpListener { fd: -1 }` on bind failure;
// the wrapper structures created by `accept` use the same sentinel
// for accept failure). A double-close on a valid fd surfaces as
// `EBADF` from the kernel; the helper does NOT try to detect that
// — under Prereq.1-5 + Slice 9d the user-Drop dispatch fires once
// per binding scope-exit per the existing `CleanupAction::UserDrop`
// drain; move-suppression for the broader cleanup-action family
// (see phase-7-codegen.md tracker entry) closes the double-drop
// surface for value-move patterns.
//
// **#[cfg(unix)] gate.** Mirrors the bind / accept / read / write
// FFIs — Windows IOCP path lands in a separate slice (phase-6 line
// 17 slice 10).

#[cfg(unix)]
#[no_mangle]
pub extern "C" fn karac_runtime_tcp_close(fd: i32) -> i32 {
    use std::os::unix::io::FromRawFd;
    if fd < 0 {
        return 0;
    }
    // SAFETY: reconstructing the `TcpStream` from the raw fd and
    // letting it drop (no `into_raw_fd()` here, unlike the bind /
    // accept / read / write FFIs which release ownership back to
    // the caller) invokes the kernel-side `close(2)` and releases
    // the fd. Both `TcpStream::from_raw_fd` and
    // `TcpListener::from_raw_fd` route through the same OS close on
    // drop; using `TcpStream` here is fine for either listener or
    // stream — the Rust-side type only governs the API surface, not
    // the underlying fd's kind.
    let _ = unsafe { std::net::TcpStream::from_raw_fd(fd) };
    0
}

// ── WebSocket framing FFI (stdlib `WebSocket.send_text` / `.recv_text`) ──
//
// Phase 6 line 17 slice 9e.1 — RFC 6455 frame encode/decode for text
// frames. v1 scope: TEXT frames only (opcode 0x1), FIN=1 unfragmented,
// server-side convention (unmasked send, masked recv). Binary frames,
// fragmentation, control frames (close/ping/pong), and client-side
// masked send land in slice 9e.3.
//
// Convention matches `karac_runtime_tcp_read` / `_write` — the caller
// (codegen lowering for `WebSocket.send_text` / `.recv_text`) is
// responsible for parking via `karac_park_on_fd(fd, direction)` BEFORE
// invoking these. The FFIs themselves do blocking reads / writes; the
// initial park ensures the first read returns immediately, and short
// frames (under the kernel's socket buffer size, ~64 KiB on Linux /
// macOS defaults) typically complete in one syscall. For larger
// frames the loop-read pattern in `read_exact_or_eof` blocks the
// worker thread briefly until the kernel delivers the rest — fine
// for v1's connection-per-thread baseline, but a re-park-on-partial
// follow-on slice will need to land for the M1 100K-connection
// target if the OS-buffer-fits-in-one-read assumption is violated.

/// Helper: read exactly `buf.len()` bytes from `stream`, or detect EOF.
/// Returns `Ok(true)` on full read, `Ok(false)` on EOF (peer closed
/// before all bytes arrived), `Err` on syscall failure. Loops past
/// `EINTR` per the standard convention.
#[cfg(unix)]
fn ws_read_exact_or_eof<R: std::io::Read>(stream: &mut R, buf: &mut [u8]) -> std::io::Result<bool> {
    let mut got = 0;
    while got < buf.len() {
        match stream.read(&mut buf[got..]) {
            Ok(0) => return Ok(false),
            Ok(n) => got += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(true)
}

/// Internal helper: write a single unmasked frame (FIN=1, RSV=000,
/// any opcode) header + payload to `stream`. Used by the
/// server→client send paths (text, binary) and by the in-flight
/// control-frame replies generated inside the recv loop
/// (auto-pong on inbound ping, close response on inbound close).
/// Returns `true` on success, `false` on any write failure.
#[cfg(unix)]
fn ws_write_unmasked_frame<W: std::io::Write>(stream: &mut W, opcode: u8, payload: &[u8]) -> bool {
    debug_assert!(opcode <= 0x0F);
    // Worst-case header: 1 fin/opcode + 1 mask/len-marker + 8
    // extended-len = 10 bytes.
    let mut header: [u8; 10] = [0; 10];
    header[0] = 0x80 | opcode; // FIN=1, RSV=000, opcode
    let len = payload.len();
    let header_len: usize = if len < 126 {
        header[1] = len as u8;
        2
    } else if len < 65536 {
        header[1] = 126;
        let be = (len as u16).to_be_bytes();
        header[2] = be[0];
        header[3] = be[1];
        4
    } else {
        header[1] = 127;
        let be = (len as u64).to_be_bytes();
        header[2..10].copy_from_slice(&be);
        10
    };
    if stream.write_all(&header[..header_len]).is_err() {
        return false;
    }
    if !payload.is_empty() && stream.write_all(payload).is_err() {
        return false;
    }
    true
}

/// Encode a TEXT frame (FIN=1, opcode=0x1, MASK=0) and write it to
/// `fd`. Server→client convention — frames are NOT masked. Payload
/// length is encoded per RFC 6455 §5.2: 7-bit inline for `< 126`,
/// 7+16-bit extended for `< 65536`, 7+64-bit extended otherwise.
///
/// Returns `msg_len` on success (matching the `karac_runtime_tcp_write`
/// convention), `-1` on any write failure. Caller should have parked
/// on write-readiness via `karac_park_on_fd(fd, 1)` first.
///
/// # Safety
///
/// `msg_ptr` must point to at least `msg_len` valid bytes when
/// `msg_len > 0`; the helper reads from this region without
/// additional bounds checking. `fd` must be a kernel-side socket
/// descriptor.
#[cfg(unix)]
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_ws_send_text(
    fd: i32,
    msg_ptr: *const u8,
    msg_len: i64,
) -> i64 {
    ws_send_data_frame(fd, msg_ptr, msg_len, 0x1)
}

/// BINARY counterpart to `karac_runtime_ws_send_text` — same shape
/// but uses opcode `0x2`. Phase 6 line 17 slice 9e.3.
///
/// # Safety
///
/// Same constraints as `karac_runtime_ws_send_text` — `msg_ptr` must
/// point to `msg_len` valid bytes when `msg_len > 0`; `fd` must be a
/// kernel-side socket descriptor.
#[cfg(unix)]
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_ws_send_binary(
    fd: i32,
    msg_ptr: *const u8,
    msg_len: i64,
) -> i64 {
    ws_send_data_frame(fd, msg_ptr, msg_len, 0x2)
}

/// Slice 9e.4 — client-side masked send. RFC 6455 §5.1 requires
/// client→server frames to be masked; v1 server-only methods
/// (`send_text` / `send_binary`) use the unmasked convention.
/// These dedicated masked variants are for kara binaries acting
/// as WebSocket clients.
///
/// The 4-byte mask key is read from `/dev/urandom` per RFC 6455
/// §10.3 (must be unpredictable to prevent cache poisoning). If
/// `/dev/urandom` is unavailable (very unlikely on Unix; rare
/// in containers / chroot environments), the FFI falls back to
/// the system clock's nanos field hashed with a small LCG —
/// not cryptographically strong but better than fail-closed at
/// the connection layer.
///
/// # Safety
///
/// Same constraints as `karac_runtime_ws_send_text` — `msg_ptr`
/// must point to `msg_len` valid bytes when `msg_len > 0`; `fd`
/// must be a kernel-side socket descriptor.
#[cfg(unix)]
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_ws_send_text_masked(
    fd: i32,
    msg_ptr: *const u8,
    msg_len: i64,
) -> i64 {
    ws_send_masked_data_frame(fd, msg_ptr, msg_len, 0x1)
}

/// BINARY counterpart to `karac_runtime_ws_send_text_masked`.
///
/// # Safety
///
/// Same constraints as the text variant.
#[cfg(unix)]
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_ws_send_binary_masked(
    fd: i32,
    msg_ptr: *const u8,
    msg_len: i64,
) -> i64 {
    ws_send_masked_data_frame(fd, msg_ptr, msg_len, 0x2)
}

/// Read 4 bytes of cryptographically-strong randomness for the
/// client-side mask key. `/dev/urandom` is the unix-portable
/// path; fall back to a clock-derived seed if reading fails
/// (defensive — keep the connection alive over fail-closed).
#[cfg(unix)]
fn ws_generate_mask_key() -> [u8; 4] {
    use std::io::Read;
    let mut key = [0u8; 4];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        if f.read_exact(&mut key).is_ok() {
            return key;
        }
    }
    // Fallback: time-derived LCG mix. Not cryptographically
    // strong but harder to predict than a hardcoded constant.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0xDEADBEEFCAFEBABE);
    let mut x = nanos.wrapping_mul(0x5DEECE66D).wrapping_add(0xB);
    for k in key.iter_mut() {
        x = x.wrapping_mul(0x5DEECE66D).wrapping_add(0xB);
        *k = (x >> 24) as u8;
    }
    key
}

/// Write a single masked client→server data frame: FIN=1,
/// opcode, MASK=1, mask key, masked payload. The mask key is
/// generated per-call via `ws_generate_mask_key`. Returns
/// `payload.len()` on success, -1 on write failure.
#[cfg(unix)]
fn ws_write_masked_frame<W: std::io::Write>(stream: &mut W, opcode: u8, payload: &[u8]) -> bool {
    debug_assert!(opcode <= 0x0F);
    // Header: 1 fin/opcode + 1 mask/len-marker + up to 8 extended-len + 4 mask key.
    let mut header: [u8; 14] = [0; 14];
    header[0] = 0x80 | opcode; // FIN=1
    let len = payload.len();
    let header_len: usize = if len < 126 {
        header[1] = 0x80 | (len as u8); // MASK=1
        2
    } else if len < 65536 {
        header[1] = 0x80 | 126;
        let be = (len as u16).to_be_bytes();
        header[2] = be[0];
        header[3] = be[1];
        4
    } else {
        header[1] = 0x80 | 127;
        let be = (len as u64).to_be_bytes();
        header[2..10].copy_from_slice(&be);
        10
    };
    let mask_key = ws_generate_mask_key();
    header[header_len..header_len + 4].copy_from_slice(&mask_key);
    let total_header_len = header_len + 4;
    if stream.write_all(&header[..total_header_len]).is_err() {
        return false;
    }
    if !payload.is_empty() {
        // Mask into a scratch buffer (payload is `&[u8]`; we
        // can't mutate the caller's buffer). For modest message
        // sizes a single allocation is fine; v2 may want a
        // chunked masker for huge payloads.
        let masked: Vec<u8> = payload
            .iter()
            .enumerate()
            .map(|(i, b)| b ^ mask_key[i % 4])
            .collect();
        if stream.write_all(&masked).is_err() {
            return false;
        }
    }
    true
}

/// Shared body for masked text + binary send.
#[cfg(unix)]
unsafe fn ws_send_masked_data_frame(fd: i32, msg_ptr: *const u8, msg_len: i64, opcode: u8) -> i64 {
    use std::os::unix::io::{FromRawFd, IntoRawFd};
    if fd < 0 || msg_len < 0 {
        return -1;
    }
    if msg_ptr.is_null() && msg_len > 0 {
        return -1;
    }
    let mut stream = std::net::TcpStream::from_raw_fd(fd);
    let payload: &[u8] = if msg_len > 0 {
        std::slice::from_raw_parts(msg_ptr, msg_len as usize)
    } else {
        &[]
    };
    let result = if ws_write_masked_frame(&mut stream, opcode, payload) {
        msg_len
    } else {
        -1
    };
    let _ = stream.into_raw_fd();
    result
}

/// Shared body for text + binary send. `opcode` is `0x1` (text) or
/// `0x2` (binary); the helper builds an unmasked single-frame
/// (FIN=1) payload write.
#[cfg(unix)]
unsafe fn ws_send_data_frame(fd: i32, msg_ptr: *const u8, msg_len: i64, opcode: u8) -> i64 {
    use std::os::unix::io::{FromRawFd, IntoRawFd};
    if fd < 0 || msg_len < 0 {
        return -1;
    }
    if msg_ptr.is_null() && msg_len > 0 {
        return -1;
    }
    let payload: &[u8] = if msg_len > 0 {
        std::slice::from_raw_parts(msg_ptr, msg_len as usize)
    } else {
        &[]
    };

    // Phase 6 line 236 slice 3 — TLS-aware dispatch. If the fd was
    // registered via `karac_runtime_ws_accept_tls` (or any other
    // path that called `tls::register_session_for_fd`), the WS
    // framing must encrypt through rustls. Otherwise plain TCP.
    if let Some(session) = crate::tls::lookup_session(fd) {
        let mut sess = session.lock().unwrap_or_else(|p| p.into_inner());
        let mut sock = std::net::TcpStream::from_raw_fd(fd);
        let mut transport = TlsConnIo {
            conn: &mut sess.conn,
            sock: &mut sock,
        };
        let result = if ws_write_unmasked_frame(&mut transport, opcode, payload) {
            msg_len
        } else {
            -1
        };
        let _ = sock.into_raw_fd();
        return result;
    }

    let mut stream = std::net::TcpStream::from_raw_fd(fd);
    let result = if ws_write_unmasked_frame(&mut stream, opcode, payload) {
        msg_len
    } else {
        -1
    };
    let _ = stream.into_raw_fd();
    result
}

/// Read one DATA frame (TEXT or BINARY, depending on
/// `accept_opcode`) from `fd`, transparently handling any control
/// frames (ping/pong/close) that arrive ahead of it. Returns the
/// payload byte count on success; `0` on graceful EOF or after a
/// close frame round-trip completed; `-1` on any protocol error /
/// IO error / oversize-payload.
///
/// Control-frame handling (RFC 6455 §5.5):
///
/// - **Ping (0x9)**: respond with a pong frame carrying the same
///   payload (RFC 6455 §5.5.2), then loop back to read the next
///   frame. The kara caller never sees the ping.
/// - **Pong (0xA)**: discard payload, loop back. Pongs are
///   unsolicited keepalive replies; v1 just drops them.
/// - **Close (0x8)**: respond with an empty close frame (RFC 6455
///   §5.5.1's close handshake — server sends a close back),
///   return 0 to the caller (matches the EOF return convention so
///   `n == 0` is the universal "connection ended cleanly" signal).
/// - Other control opcodes (0xB..=0xF): reserved by RFC 6455 §5.5
///   for future use; treated as protocol violation → -1.
/// - All control frames must satisfy FIN=1 and payload length ≤
///   125 per RFC 6455 §5.5; violations → -1.
///
/// Slice 9e.4 — fragmentation reassembly per RFC 6455 §5.4. A
/// data message can span multiple frames: the first frame carries
/// the message opcode (text 0x1 / binary 0x2) with FIN=0; zero or
/// more continuation frames follow with opcode=0x0 and FIN=0; the
/// final continuation frame has opcode=0x0 and FIN=1. Each
/// fragment's payload is appended (after unmasking) to the
/// caller's `out_ptr` buffer; the loop returns the accumulated
/// byte count when FIN=1 closes the message. Total reassembled
/// length is bounded by `out_max_len` — exceeding it returns -1.
/// Control frames (ping/pong/close) MAY be interleaved between
/// data fragments per §5.4 and continue to be handled
/// transparently inside the loop without affecting the data
/// reassembly state.
#[cfg(unix)]
unsafe fn ws_recv_data_frame(
    fd: i32,
    out_ptr: *mut u8,
    out_max_len: i64,
    accept_opcode: u8,
) -> i64 {
    use std::os::unix::io::{FromRawFd, IntoRawFd};
    if fd < 0 || out_max_len < 0 {
        return -1;
    }
    if out_ptr.is_null() && out_max_len > 0 {
        return -1;
    }

    // Phase 6 line 236 slice 3 — TLS-aware dispatch. Same shape as
    // `ws_send_data_frame`: route through rustls when the fd has a
    // session in the TLS registry. The frame-parser closure is
    // generic over Read+Write so the same body services both
    // transports.
    if let Some(session) = crate::tls::lookup_session(fd) {
        let mut sess = session.lock().unwrap_or_else(|p| p.into_inner());
        let mut sock = std::net::TcpStream::from_raw_fd(fd);
        let mut transport = TlsConnIo {
            conn: &mut sess.conn,
            sock: &mut sock,
        };
        let result = ws_recv_data_frame_inner(&mut transport, out_ptr, out_max_len, accept_opcode);
        let _ = sock.into_raw_fd();
        return result;
    }

    let mut stream = std::net::TcpStream::from_raw_fd(fd);
    let result = ws_recv_data_frame_inner(&mut stream, out_ptr, out_max_len, accept_opcode);
    let _ = stream.into_raw_fd();
    result
}

/// Frame-parser body extracted from `ws_recv_data_frame` so the same
/// reassembly logic services both plain-TCP and TLS-wrapped
/// transports. Generic over `Read + Write` so the closure path
/// remains a thin dispatch wrapper that picks the transport and
/// forwards.
///
/// # Safety
///
/// `out_ptr` must point to `out_max_len` writable bytes when
/// `out_max_len > 0`. Same contract as the public `ws_recv_data_frame`.
#[cfg(unix)]
unsafe fn ws_recv_data_frame_inner<S: std::io::Read + std::io::Write>(
    stream: &mut S,
    out_ptr: *mut u8,
    out_max_len: i64,
    accept_opcode: u8,
) -> i64 {
    // Reassembly state. `accumulated` is the running byte count
    // written into `out_ptr`. `in_fragment` flips to true once we've
    // consumed a FIN=0 data frame; while set, we expect continuation
    // frames (opcode 0x0) until a FIN=1 continuation closes the
    // message. Before `in_fragment` is set, a FIN=1 data frame with
    // `opcode == accept_opcode` returns immediately (single-frame
    // message — the slice 9e.3 fast path).
    {
        let mut accumulated: u64 = 0;
        let mut in_fragment = false;
        loop {
            let mut header2 = [0u8; 2];
            match ws_read_exact_or_eof(&mut *stream, &mut header2) {
                Ok(true) => {}
                Ok(false) => return 0,
                Err(_) => return -1,
            }
            let fin = (header2[0] & 0x80) != 0;
            let rsv = header2[0] & 0x70;
            let opcode = header2[0] & 0x0F;
            let masked = (header2[1] & 0x80) != 0;
            let len7 = (header2[1] & 0x7F) as u64;

            if rsv != 0 || !masked {
                return -1;
            }

            let payload_len: u64 = match len7 {
                0..=125 => len7,
                126 => {
                    let mut buf = [0u8; 2];
                    match ws_read_exact_or_eof(&mut *stream, &mut buf) {
                        Ok(true) => u16::from_be_bytes(buf) as u64,
                        _ => return -1,
                    }
                }
                127 => {
                    let mut buf = [0u8; 8];
                    match ws_read_exact_or_eof(&mut *stream, &mut buf) {
                        Ok(true) => u64::from_be_bytes(buf),
                        _ => return -1,
                    }
                }
                _ => return -1,
            };

            let mut mask_key = [0u8; 4];
            if !ws_read_exact_or_eof(&mut *stream, &mut mask_key).unwrap_or(false) {
                return -1;
            }

            let is_control = opcode >= 0x8;
            if is_control {
                // RFC 6455 §5.5: control frames MUST be FIN=1 with
                // length ≤ 125. They may be interleaved with data
                // fragments per §5.4, so we handle them without
                // touching the reassembly state.
                if !fin || payload_len > 125 {
                    return -1;
                }
                let mut ctrl_payload = [0u8; 125];
                let slice = &mut ctrl_payload[..payload_len as usize];
                if payload_len > 0 && !ws_read_exact_or_eof(&mut *stream, slice).unwrap_or(false) {
                    return -1;
                }
                for (i, byte) in slice.iter_mut().enumerate() {
                    *byte ^= mask_key[i % 4];
                }
                match opcode {
                    0x8 => {
                        let _ = ws_write_unmasked_frame(&mut *stream, 0x8, &[]);
                        return 0;
                    }
                    0x9 => {
                        if !ws_write_unmasked_frame(&mut *stream, 0xA, slice) {
                            return -1;
                        }
                        continue;
                    }
                    0xA => {
                        continue;
                    }
                    _ => return -1,
                }
            }

            // Data frame. RFC 6455 §5.4 fragmentation:
            //   - First frame of a message: opcode = data
            //     opcode (text=0x1, binary=0x2); FIN may be 0
            //     or 1.
            //   - Continuation frames: opcode = 0x0; FIN may
            //     be 0 (more to come) or 1 (final fragment).
            //   - Mixing: if we've started a fragmented data
            //     message, the next data frame MUST be a
            //     continuation; conversely a continuation
            //     frame is only legal mid-fragment.
            if in_fragment {
                if opcode != 0x0 {
                    return -1;
                }
            } else if opcode != accept_opcode {
                return -1;
            }

            // Bounds check: accumulated + payload_len must fit
            // in the caller's buffer. Overflow-safe via saturating
            // u64 add (out_max_len is i64 but ≥ 0).
            let new_total = accumulated.saturating_add(payload_len);
            if new_total > out_max_len as u64 {
                return -1;
            }

            if payload_len > 0 {
                let off = accumulated as usize;
                let payload_usize = payload_len as usize;
                let frag_slice = std::slice::from_raw_parts_mut(out_ptr.add(off), payload_usize);
                if !ws_read_exact_or_eof(&mut *stream, frag_slice).unwrap_or(false) {
                    return -1;
                }
                for (i, byte) in frag_slice.iter_mut().enumerate() {
                    *byte ^= mask_key[i % 4];
                }
            }
            accumulated = new_total;

            if fin {
                return accumulated as i64;
            }
            in_fragment = true;
        }
    }
}

/// Read one TEXT frame from `fd`, transparently handling any
/// preceding control frames per RFC 6455 §5.5 (pings auto-
/// answered with pongs, close frames trigger a close-handshake
/// reply + return 0). Returns the unmasked payload byte count on
/// success; `0` on graceful EOF / close round-trip; `-1` on any
/// protocol error / IO error / oversize-payload.
///
/// Caller should have parked on read-readiness via
/// `karac_park_on_fd(fd, 0)` first.
///
/// # Safety
///
/// `out_ptr` must point to at least `out_max_len` writable bytes
/// when `out_max_len > 0`. The helper writes payload bytes into
/// this region (unmasked) and writes nothing on error. `fd` must
/// be a kernel-side socket descriptor.
#[cfg(unix)]
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_ws_recv_text(
    fd: i32,
    out_ptr: *mut u8,
    out_max_len: i64,
) -> i64 {
    ws_recv_data_frame(fd, out_ptr, out_max_len, 0x1)
}

/// BINARY counterpart to `karac_runtime_ws_recv_text` — same
/// shape but accepts opcode `0x2` instead of `0x1`. Phase 6
/// line 17 slice 9e.3.
///
/// # Safety
///
/// Same constraints as `karac_runtime_ws_recv_text`.
#[cfg(unix)]
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_ws_recv_binary(
    fd: i32,
    out_ptr: *mut u8,
    out_max_len: i64,
) -> i64 {
    ws_recv_data_frame(fd, out_ptr, out_max_len, 0x2)
}

// ── WebSocket HTTP upgrade handshake FFI (stdlib `WebSocket.accept`) ─────
//
// Phase 6 line 17 slice 9e.2 — RFC 6455 §4.2 server-side handshake.
// Accepts a TCP connection on the listener, reads the HTTP/1.1
// Upgrade request, validates the `Sec-WebSocket-Key` header,
// computes the `Sec-WebSocket-Accept` response per the RFC's
// SHA-1 + Base64 recipe, writes the 101 Switching Protocols
// response. Returns the upgraded connection fd on success, `-1`
// on any failure (accept error, IO error, malformed request,
// missing key header, response write error). The caller is
// responsible for parking on listener-readable via
// `karac_park_on_fd(listener_fd, 0)` BEFORE invoking this — same
// convention as `karac_runtime_tcp_accept`.
//
// v1 limitations (deferred to follow-on slices):
//
// - **No request validation beyond `Sec-WebSocket-Key` presence.**
//   The RFC mandates `Upgrade: websocket`, `Connection: Upgrade`,
//   `Sec-WebSocket-Version: 13`, and `Host:` headers; v1 accepts
//   any request that contains a valid-shaped Sec-WebSocket-Key.
//   A real production deployment behind a stricter handshake
//   validator should add these checks (and respond with 400 on
//   failure rather than upgrading anyway).
// - **No subprotocol / extension negotiation.** The 101 response
//   never echoes `Sec-WebSocket-Protocol` or `Sec-WebSocket-Extensions`.
//   Slice 9e.3 may revisit if a use case surfaces.
// - **Blocking reads with no timeout.** A malicious client that
//   connects but never sends an HTTP request will hang the
//   worker thread until the kernel times out the socket (typically
//   minutes). Production deployments should set `SO_RCVTIMEO`
//   or run the handshake on a dedicated tasked pool.
// - **8 KiB request size limit.** The hand-rolled header parser
//   reads into a fixed buffer; requests larger than 8 KiB fail
//   with `-1`. RFC 6455 doesn't mandate a size; real browsers
//   stay well under this.

const WS_HANDSHAKE_GUID: &[u8] = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// Hand-rolled SHA-1 (RFC 3174). Returns the 20-byte digest. v1
/// avoids pulling the `sha1` crate as a dependency — the
/// algorithm is well-known and bounded; the slice 9e.2 use case
/// hashes a single fixed-size input (Sec-WebSocket-Key + GUID)
/// so the implementation doesn't need to be streaming or
/// performance-tuned. If future work needs SHA-1 elsewhere
/// (e.g., for HTTP digest auth), factor this out to a sibling
/// `hash.rs`.
fn sha1(message: &[u8]) -> [u8; 20] {
    let mut h0: u32 = 0x67452301;
    let mut h1: u32 = 0xEFCDAB89;
    let mut h2: u32 = 0x98BADCFE;
    let mut h3: u32 = 0x10325476;
    let mut h4: u32 = 0xC3D2E1F0;

    // Pad the message: append 0x80, then zeros, then 8-byte
    // big-endian length-in-bits. Final length is a multiple of 64.
    let bit_len = (message.len() as u64) * 8;
    let mut padded: Vec<u8> = Vec::with_capacity(message.len() + 64 + 8);
    padded.extend_from_slice(message);
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    // Process each 64-byte chunk.
    for chunk in padded.chunks_exact(64) {
        let mut w = [0u32; 80];
        for (i, word) in chunk.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let mut a = h0;
        let mut b = h1;
        let mut c = h2;
        let mut d = h3;
        let mut e = h4;
        for (i, &wi) in w.iter().enumerate() {
            let (f, k): (u32, u32) = if i < 20 {
                ((b & c) | ((!b) & d), 0x5A827999)
            } else if i < 40 {
                (b ^ c ^ d, 0x6ED9EBA1)
            } else if i < 60 {
                ((b & c) | (b & d) | (c & d), 0x8F1BBCDC)
            } else {
                (b ^ c ^ d, 0xCA62C1D6)
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        h0 = h0.wrapping_add(a);
        h1 = h1.wrapping_add(b);
        h2 = h2.wrapping_add(c);
        h3 = h3.wrapping_add(d);
        h4 = h4.wrapping_add(e);
    }

    let mut out = [0u8; 20];
    out[0..4].copy_from_slice(&h0.to_be_bytes());
    out[4..8].copy_from_slice(&h1.to_be_bytes());
    out[8..12].copy_from_slice(&h2.to_be_bytes());
    out[12..16].copy_from_slice(&h3.to_be_bytes());
    out[16..20].copy_from_slice(&h4.to_be_bytes());
    out
}

/// Standard Base64 encode (RFC 4648, no URL-safe alternate
/// alphabet). v1 avoids the `base64` crate for the same reason
/// as `sha1` above — bounded one-call use, well-known algorithm.
fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    let mut chunks = input.chunks_exact(3);
    for chunk in chunks.by_ref() {
        let b0 = chunk[0] as u32;
        let b1 = chunk[1] as u32;
        let b2 = chunk[2] as u32;
        let combined = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((combined >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((combined >> 12) & 0x3F) as usize] as char);
        out.push(TABLE[((combined >> 6) & 0x3F) as usize] as char);
        out.push(TABLE[(combined & 0x3F) as usize] as char);
    }
    let rem = chunks.remainder();
    match rem.len() {
        0 => {}
        1 => {
            let b0 = rem[0] as u32;
            out.push(TABLE[((b0 >> 2) & 0x3F) as usize] as char);
            out.push(TABLE[((b0 << 4) & 0x3F) as usize] as char);
            out.push('=');
            out.push('=');
        }
        2 => {
            let b0 = rem[0] as u32;
            let b1 = rem[1] as u32;
            let combined = (b0 << 8) | b1;
            out.push(TABLE[((combined >> 10) & 0x3F) as usize] as char);
            out.push(TABLE[((combined >> 4) & 0x3F) as usize] as char);
            out.push(TABLE[((combined << 2) & 0x3F) as usize] as char);
            out.push('=');
        }
        _ => unreachable!(),
    }
    out
}

/// Read HTTP request bytes from `stream` until the canonical
/// `\r\n\r\n` end-of-headers marker is found, or the buffer
/// fills up. Returns the accumulated bytes including the
/// trailing `\r\n\r\n` on success; `None` on IO error, EOF
/// before complete request, or oversize request.
#[cfg(unix)]
fn ws_read_http_request<R: std::io::Read>(stream: &mut R) -> Option<Vec<u8>> {
    const MAX_REQUEST_SIZE: usize = 8 * 1024;
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => return None,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    return Some(buf);
                }
                if buf.len() >= MAX_REQUEST_SIZE {
                    return None;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        }
    }
}

/// Find the value of the `Sec-WebSocket-Key` header in an HTTP
/// request byte slice. Header lookup is case-insensitive per
/// RFC 7230. Returns `None` if the header is missing or
/// malformed.
fn extract_sec_websocket_key(request: &[u8]) -> Option<&[u8]> {
    // Split on \r\n to walk header lines. We could parse more
    // strictly but the v1 use case only needs this one header.
    for line in request.split(|&b| b == b'\n') {
        let line = if let Some(stripped) = line.strip_suffix(b"\r") {
            stripped
        } else {
            line
        };
        // Case-insensitive name match on "sec-websocket-key:".
        const NAME: &[u8] = b"sec-websocket-key:";
        if line.len() < NAME.len() {
            continue;
        }
        let name_part = &line[..NAME.len()];
        let mut matches = true;
        for (lhs, rhs) in name_part.iter().zip(NAME.iter()) {
            if lhs.to_ascii_lowercase() != *rhs {
                matches = false;
                break;
            }
        }
        if matches {
            // Skip whitespace after the colon.
            let mut rest = &line[NAME.len()..];
            while let Some((b, tail)) = rest.split_first() {
                if *b == b' ' || *b == b'\t' {
                    rest = tail;
                } else {
                    break;
                }
            }
            if rest.is_empty() {
                return None;
            }
            return Some(rest);
        }
    }
    None
}

/// Server-side WebSocket handshake. Mirrors
/// `karac_runtime_tcp_accept` for the listener-side park-then-
/// accept flow, but adds the HTTP upgrade exchange before
/// returning. Returns the upgraded connection fd on success,
/// `-1` on any failure.
///
/// # Safety
///
/// `listener_fd` must be a valid listener socket descriptor;
/// passing a non-socket fd produces undefined behaviour from
/// `accept`. Caller is responsible for parking on
/// listener-readability before invoking.
#[cfg(unix)]
#[no_mangle]
pub extern "C" fn karac_runtime_ws_accept(listener_fd: i32) -> i32 {
    use std::io::Write;
    use std::os::unix::io::{FromRawFd, IntoRawFd};

    if listener_fd < 0 {
        return -1;
    }

    // Accept the underlying TCP connection (same shape as
    // `karac_runtime_tcp_accept`).
    let listener = unsafe { std::net::TcpListener::from_raw_fd(listener_fd) };
    let accept_result = listener.accept();
    let _ = listener.into_raw_fd();

    let mut conn = match accept_result {
        Ok((c, _addr)) => c,
        Err(_) => return -1,
    };

    // Read the HTTP request headers.
    let request = match ws_read_http_request(&mut conn) {
        Some(bytes) => bytes,
        None => return -1,
    };

    // Extract Sec-WebSocket-Key.
    let key = match extract_sec_websocket_key(&request) {
        Some(k) => k,
        None => {
            // Best-effort 400 response so a misbehaving client
            // sees a friendly error rather than a silent close.
            let resp =
                b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\nContent-Length: 0\r\n\r\n";
            let _ = conn.write_all(resp);
            return -1;
        }
    };

    // Compute Sec-WebSocket-Accept per RFC 6455 §4.2.2:
    //   accept = base64(sha1(key + GUID))
    let mut digest_input: Vec<u8> = Vec::with_capacity(key.len() + WS_HANDSHAKE_GUID.len());
    digest_input.extend_from_slice(key);
    digest_input.extend_from_slice(WS_HANDSHAKE_GUID);
    let digest = sha1(&digest_input);
    let accept = base64_encode(&digest);

    // Write the 101 Switching Protocols response.
    let response = format!(
        "HTTP/1.1 101 Switching Protocols\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Accept: {}\r\n\
         \r\n",
        accept
    );
    if conn.write_all(response.as_bytes()).is_err() {
        return -1;
    }

    // Return the connection fd; ownership of close-on-drop
    // belongs to the kara `WebSocket` value the caller
    // constructs from this fd (slice-9e.1 + slice-9d Drop chain).
    conn.into_raw_fd()
}

// ── Phase 6 line 236 slice 3 — WebSocket-over-TLS support ────────────────
//
// `TlsConnIo` adapter wraps a borrowed `(ServerConnection, TcpStream)` pair
// in a `Read + Write` surface so the existing generic helpers
// (`ws_write_unmasked_frame<W: Write>`, `ws_read_exact_or_eof<R: Read>`,
// `ws_read_http_request<R: Read>`, `ws_recv_data_frame_inner<S: Read+Write>`)
// can drive both plain-TCP and TLS-wrapped transports without
// duplicating the WS framing logic.
//
// **Read path** pumps rustls's incoming-packet processor until the
// reader yields plaintext or hits EOF:
//
//   - `conn.reader().read(buf)` → if `Ok(n)` with n>0, return.
//   - `Ok(0)` means no plaintext buffered; loop pulls ciphertext via
//     `conn.read_tls(sock)` then `conn.process_new_packets()`.
//   - `Err(WouldBlock)` from the reader treated same as Ok(0).
//
// **Write path** symmetric: write plaintext via `conn.writer()`, then
// drain `conn.wants_write()` ciphertext via `conn.write_tls(sock)`.
// `flush` ensures the ciphertext buffer is fully drained.

#[cfg(unix)]
struct TlsConnIo<'a> {
    // Phase-8 line 22: widened from `&mut ServerConnection` to
    // `&mut rustls::Connection` (the enum over Server + Client) so the
    // shared `SESSIONS` map can carry both directions — `tls.rs`'s
    // `TlsSession.conn` switched to the enum at the same time. The
    // method calls below (`reader` / `writer` / `wants_read` /
    // `wants_write` / `read_tls` / `write_tls` / `process_new_packets`)
    // are all available on the enum; both inner variants delegate.
    conn: &'a mut rustls::Connection,
    sock: &'a mut std::net::TcpStream,
}

#[cfg(unix)]
impl<'a> std::io::Read for TlsConnIo<'a> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            match self.conn.reader().read(buf) {
                Ok(0) => {
                    if !self.conn.wants_read() {
                        return Ok(0); // clean EOF (close_notify)
                    }
                    match self.conn.read_tls(self.sock) {
                        Ok(0) => return Ok(0), // socket EOF
                        Ok(_) => {
                            if self.conn.process_new_packets().is_err() {
                                return Err(std::io::Error::new(
                                    std::io::ErrorKind::InvalidData,
                                    "tls process_new_packets failed",
                                ));
                            }
                            continue;
                        }
                        Err(e) => return Err(e),
                    }
                }
                Ok(n) => return Ok(n),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if !self.conn.wants_read() {
                        return Ok(0);
                    }
                    match self.conn.read_tls(self.sock) {
                        Ok(0) => return Ok(0),
                        Ok(_) => {
                            if self.conn.process_new_packets().is_err() {
                                return Err(std::io::Error::new(
                                    std::io::ErrorKind::InvalidData,
                                    "tls process_new_packets failed",
                                ));
                            }
                            continue;
                        }
                        Err(e) => return Err(e),
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }
}

#[cfg(unix)]
impl<'a> std::io::Write for TlsConnIo<'a> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.conn.writer().write(buf)?;
        self.flush()?;
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        while self.conn.wants_write() {
            self.conn.write_tls(self.sock)?;
        }
        Ok(())
    }
}

/// Run the rustls handshake + RFC 6455 HTTP upgrade on an
/// already-accepted connection fd, registering the TLS session keyed by
/// fd. Returns the ready connection fd on success, or -1 (closing the fd)
/// on any failure. This is the former inline body of
/// `karac_runtime_ws_accept_tls` minus the `accept(2)` — now run by the
/// handshake worker pool so handshakes overlap instead of serializing on
/// the accept thread.
///
/// # Safety
/// `conn_fd` must be a freshly-accepted, owned TCP connection fd; `config`
/// must be a valid `*mut KaracTlsConfig` for the call's duration.
#[cfg(unix)]
unsafe fn ws_handshake_conn_tls(
    conn_fd: i32,
    config: *mut crate::tls::KaracTlsConfig,
) -> Result<i32, (HandshakeStep, String)> {
    use std::os::unix::io::{AsRawFd, FromRawFd, IntoRawFd};

    // Take ownership of the accepted connection fd.
    let mut sock = std::net::TcpStream::from_raw_fd(conn_fd);

    // Build a fresh ServerConnection using the borrowed config.
    let config_arc = crate::tls::clone_config_arc(config);
    let mut conn = match rustls::ServerConnection::new(config_arc) {
        Ok(c) => c,
        Err(e) => return Err((HandshakeStep::TlsConfig, format!("{e}"))),
    };

    // Drive the TLS handshake to completion against the blocking
    // socket. complete_io loops until handshaking is done.
    if let Err(e) = conn.complete_io(&mut sock) {
        return Err((HandshakeStep::TlsHandshake, format!("{e}")));
    }

    // Register the session keyed by the fd BEFORE the HTTP upgrade
    // so the TlsConnIo wrapper used by the upgrade machinery finds
    // it. The fd ownership stays with `sock` for now; we'll
    // `into_raw_fd` it at the end.
    let fd = sock.as_raw_fd();
    // Phase-8 line 22 widening: `register_session_for_fd` now takes
    // `rustls::Connection`; wrap the freshly-built `ServerConnection`.
    crate::tls::register_session_for_fd(fd, rustls::Connection::Server(conn));

    // HTTP upgrade exchange over TLS. The `TlsConnIo` wrapper drives
    // the rustls session against the existing socket. Look up the
    // session we just inserted so the wrapper holds a borrowed
    // ServerConnection.
    let upgrade_outcome = {
        let session = match crate::tls::lookup_session(fd) {
            Some(s) => s,
            None => {
                // Couldn't find what we just inserted — shouldn't
                // happen, but failure-mode the connection clean.
                let _ = sock.into_raw_fd();
                let _ = crate::tls::karac_runtime_tls_close(fd);
                return Err((
                    HandshakeStep::SessionLookup,
                    format!("session lookup miss for fd {fd} immediately after register"),
                ));
            }
        };
        let mut sess = session.lock().unwrap_or_else(|p| p.into_inner());
        let mut transport = TlsConnIo {
            conn: &mut sess.conn,
            sock: &mut sock,
        };
        ws_drive_upgrade_handshake(&mut transport)
    };

    if let Err(reason) = upgrade_outcome {
        // HTTP upgrade failed (malformed request, missing key
        // header, etc.). Pull the session out of the registry and
        // close the fd.
        let _ = sock.into_raw_fd();
        let _ = crate::tls::karac_runtime_tls_close(fd);
        return Err((HandshakeStep::WsUpgrade, reason));
    }

    // Success — relinquish the TcpStream's destructor so the fd
    // stays open. The kara `WebSocket` value the caller constructs
    // owns close-on-drop now.
    Ok(sock.into_raw_fd())
}

/// Per-listener pool that offloads the (slow, I/O-bound) TLS handshake +
/// WS upgrade off the accept-loop thread. `work` holds freshly-accepted
/// raw connection fds awaiting handshake; `done` holds fully-upgraded
/// connection fds awaiting pickup by `karac_runtime_ws_accept_tls`.
struct WsHandshakePool {
    work: Mutex<VecDeque<i32>>,
    cv_work: Condvar,
    done: Mutex<VecDeque<i32>>,
    cv_done: Condvar,
    /// Some(...) iff `KARAC_WS_STATS` is set at pool init. Reporter
    /// thread dumps a line/second so the queue-depth-vs-throughput
    /// hypothesis behind the connect-tail at 1 M can be empirically
    /// verified without instrumenting the bench.
    stats: Option<Arc<HandshakeStats>>,
}

/// Snapshot-able handshake-pool counters used by the once-per-second
/// reporter thread. All counters are `Relaxed` — we only need
/// monotonic visibility, not cross-thread ordering. The per-step
/// `*_failed` counters localise *where* a connection died (TLS handshake
/// vs WS-upgrade vs session-lookup); `first_errors` captures the literal
/// rustls / parse error text for the first few failures so the cause is
/// inspectable from the stats stream alone — added 2026-05-30 as step
/// (a) of the macOS bench-client TLS+WS-upgrade diagnosis plan.
struct HandshakeStats {
    submitted: AtomicU64,
    completed: AtomicU64,
    failed: AtomicU64,
    tls_config_failed: AtomicU64,
    tls_handshake_failed: AtomicU64,
    session_lookup_failed: AtomicU64,
    ws_upgrade_failed: AtomicU64,
    handshake_nanos_sum: AtomicU64,
    workers: u32,
    /// Up to `FIRST_ERRORS_CAP` literal failure messages. The reporter
    /// drains and prints these once per tick so the first cohort of
    /// errors is always seen in the stats stream.
    first_errors: Mutex<Vec<String>>,
}

/// Cap on the number of literal failure messages buffered in
/// `HandshakeStats::first_errors`. Bench-rate limited — a hot loop of
/// errors mustn't grow the buffer unboundedly. 32 covers diagnosing a
/// failure mode that fires every connection (we only need a few) while
/// keeping the worker-side allocation bounded.
const FIRST_ERRORS_CAP: usize = 32;

impl HandshakeStats {
    fn new_if_enabled(workers: usize) -> Option<Arc<Self>> {
        if std::env::var_os("KARAC_WS_STATS").is_some() {
            Some(Arc::new(Self {
                submitted: AtomicU64::new(0),
                completed: AtomicU64::new(0),
                failed: AtomicU64::new(0),
                tls_config_failed: AtomicU64::new(0),
                tls_handshake_failed: AtomicU64::new(0),
                session_lookup_failed: AtomicU64::new(0),
                ws_upgrade_failed: AtomicU64::new(0),
                handshake_nanos_sum: AtomicU64::new(0),
                workers: workers as u32,
                first_errors: Mutex::new(Vec::with_capacity(FIRST_ERRORS_CAP)),
            }))
        } else {
            None
        }
    }

    /// Push a failure message into `first_errors` (no-op once full).
    /// Called from the worker thread on every failed handshake; cheap
    /// to call when the buffer is already capped because the early
    /// length check sidesteps the lock contention.
    fn record_error(&self, step: HandshakeStep, msg: String) {
        // Saturating-fast path: avoid even taking the lock once the
        // buffer is full (the common case after the first cohort).
        if let Ok(g) = self.first_errors.lock() {
            if g.len() >= FIRST_ERRORS_CAP {
                return;
            }
            drop(g);
        }
        let line = format!("{step:?}: {msg}");
        let mut g = self.first_errors.lock().unwrap_or_else(|p| p.into_inner());
        if g.len() < FIRST_ERRORS_CAP {
            g.push(line);
        }
    }
}

/// Which step of `ws_handshake_conn_tls` a failure occurred at. Used
/// only for the `KARAC_WS_STATS` instrumentation — the FFI's `i32`
/// return shape is unchanged.
#[derive(Debug, Clone, Copy)]
enum HandshakeStep {
    TlsConfig,
    TlsHandshake,
    SessionLookup,
    WsUpgrade,
}

/// Reporter loop: every second, dump pool counters + current queue
/// depths to stderr. The format is grep-friendly (`[karac_ws_stats]`
/// prefix, space-separated `k=v` pairs) so a bench run can be
/// post-processed cheaply.
fn ws_stats_reporter(stats: Arc<HandshakeStats>, pool: Arc<WsHandshakePool>) {
    let mut last_submitted = 0u64;
    let mut last_completed = 0u64;
    let mut last_nanos = 0u64;
    loop {
        thread::sleep(Duration::from_secs(1));
        let s = stats.submitted.load(Ordering::Relaxed);
        let c = stats.completed.load(Ordering::Relaxed);
        let f = stats.failed.load(Ordering::Relaxed);
        let f_tls_cfg = stats.tls_config_failed.load(Ordering::Relaxed);
        let f_tls_hs = stats.tls_handshake_failed.load(Ordering::Relaxed);
        let f_lookup = stats.session_lookup_failed.load(Ordering::Relaxed);
        let f_ws_up = stats.ws_upgrade_failed.load(Ordering::Relaxed);
        let ns = stats.handshake_nanos_sum.load(Ordering::Relaxed);
        let work_depth = pool
            .work
            .lock()
            .map(|q| q.len())
            .unwrap_or_else(|p| p.into_inner().len());
        let done_depth = pool
            .done
            .lock()
            .map(|q| q.len())
            .unwrap_or_else(|p| p.into_inner().len());
        let delta_s = s.saturating_sub(last_submitted);
        let delta_c = c.saturating_sub(last_completed);
        let delta_ns = ns.saturating_sub(last_nanos);
        let in_flight = s.saturating_sub(c);
        let mean_ms = if delta_c > 0 {
            (delta_ns as f64 / delta_c as f64) / 1_000_000.0
        } else {
            0.0
        };
        eprintln!(
            "[karac_ws_stats] submit_total={} done_total={} failed_total={} fail_tls_config={} fail_tls_handshake={} fail_session_lookup={} fail_ws_upgrade={} in_flight={} work_q={} done_q={} workers={} submit_per_s={} done_per_s={} mean_handshake_ms={:.2}",
            s, c, f, f_tls_cfg, f_tls_hs, f_lookup, f_ws_up, in_flight, work_depth, done_depth, stats.workers, delta_s, delta_c, mean_ms
        );
        // Drain captured first-error messages so they show up at most
        // once each. The buffer is capped at FIRST_ERRORS_CAP so this is
        // bounded work per tick.
        let drained: Vec<String> = {
            let mut g = stats.first_errors.lock().unwrap_or_else(|p| p.into_inner());
            std::mem::take(&mut *g)
        };
        for line in drained {
            eprintln!("[karac_ws_stats:err] {line}");
        }
        last_submitted = s;
        last_completed = c;
        last_nanos = ns;
    }
}

/// Per-listener-fd handshake pools, created lazily on first
/// `karac_runtime_ws_accept_tls` call for that listener.
static WS_HANDSHAKE_POOLS: OnceLock<Mutex<HashMap<i32, Arc<WsHandshakePool>>>> = OnceLock::new();

/// Handshake worker count per listener. Handshakes block in socket reads
/// waiting on the peer, so this is oversubscribed relative to core count
/// to overlap many in-flight handshakes. Override with
/// `KARAC_WS_ACCEPT_THREADS`; default `max(32, 2 × num_cpus)`, floored
/// at 1. The `2×` oversubscription handles the real-network case where
/// rustls handshakes block on peer reads; the `max(32, …)` floor keeps
/// the M1/M2-era default behaviour on small dev boxes.
///
/// Surfaced at M3 1M (2026-05-30) in the connect-tail-latency
/// investigation: r8g.4xlarge's 16 vCPU got 32 workers, so the queue
/// math was `(concurrency 512 / workers 32) × T_handshake 29ms = 464ms`
/// mean (matched observed 466ms). On a 32+ vCPU box, the old hardcoded
/// 32 would have been undersized; scaling with CPUs keeps the worker
/// pool sized to the actual handshake-CPU throughput available.
fn ws_handshake_thread_count() -> usize {
    if let Some(n) = std::env::var("KARAC_WS_ACCEPT_THREADS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|&n| n >= 1)
    {
        return n;
    }
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(16);
    (cpus * 2).max(32)
}

/// Worker loop: pop a raw accepted fd, run the handshake + upgrade, push
/// the ready fd onto the done queue (or drop it on failure).
fn ws_handshake_worker(config_addr: usize, pool: Arc<WsHandshakePool>) {
    loop {
        let conn_fd = {
            let mut q = pool.work.lock().unwrap_or_else(|p| p.into_inner());
            loop {
                if let Some(fd) = q.pop_front() {
                    break fd;
                }
                q = pool.cv_work.wait(q).unwrap_or_else(|p| p.into_inner());
            }
        };
        // SAFETY: `config_addr` is the KaracTlsConfig pointer captured at
        // pool start; in the v1 accept-loop shape the TlsListener (and its
        // config) outlive the process, so the pointer stays valid.
        let t0 = pool.stats.as_ref().map(|_| Instant::now());
        let outcome = unsafe {
            ws_handshake_conn_tls(conn_fd, config_addr as *mut crate::tls::KaracTlsConfig)
        };
        if let (Some(stats), Some(t0)) = (pool.stats.as_ref(), t0) {
            stats
                .handshake_nanos_sum
                .fetch_add(t0.elapsed().as_nanos() as u64, Ordering::Relaxed);
            stats.completed.fetch_add(1, Ordering::Relaxed);
            if let Err((step, _)) = &outcome {
                stats.failed.fetch_add(1, Ordering::Relaxed);
                match step {
                    HandshakeStep::TlsConfig => &stats.tls_config_failed,
                    HandshakeStep::TlsHandshake => &stats.tls_handshake_failed,
                    HandshakeStep::SessionLookup => &stats.session_lookup_failed,
                    HandshakeStep::WsUpgrade => &stats.ws_upgrade_failed,
                }
                .fetch_add(1, Ordering::Relaxed);
            }
        }
        let ready = match outcome {
            Ok(fd) => fd,
            Err((step, msg)) => {
                // Handshake/upgrade failed; `ws_handshake_conn_tls` already
                // closed the fd. Capture the literal error text into the
                // stats buffer so the reporter can dump it.
                if let Some(stats) = pool.stats.as_ref() {
                    stats.record_error(step, msg);
                }
                continue;
            }
        };
        let mut d = pool.done.lock().unwrap_or_else(|p| p.into_inner());
        d.push_back(ready);
        drop(d);
        pool.cv_done.notify_one();
    }
}

/// Get (or lazily start) the handshake pool for `listener_fd`.
#[cfg(unix)]
fn ws_handshake_pool_for(
    listener_fd: i32,
    config: *mut crate::tls::KaracTlsConfig,
) -> Arc<WsHandshakePool> {
    let map_mutex = WS_HANDSHAKE_POOLS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = map_mutex.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(pool) = map.get(&listener_fd) {
        return Arc::clone(pool);
    }

    // Set the listener non-blocking so the accept-drain loop in the FFI
    // returns `WouldBlock` once the backlog is empty instead of blocking
    // the accept thread there. epoll readiness (the codegen park) is
    // unaffected by the fd's blocking mode.
    {
        use std::os::unix::io::{FromRawFd, IntoRawFd};
        // SAFETY: `listener_fd` is a live TCP listener owned by the kara
        // TlsListener; we borrow it transiently (into_raw_fd before drop)
        // only to flip O_NONBLOCK.
        let l = unsafe { std::net::TcpListener::from_raw_fd(listener_fd) };
        let _ = l.set_nonblocking(true);
        let _ = l.into_raw_fd();
    }

    let workers = ws_handshake_thread_count();
    let stats = HandshakeStats::new_if_enabled(workers);
    let pool = Arc::new(WsHandshakePool {
        work: Mutex::new(VecDeque::new()),
        cv_work: Condvar::new(),
        done: Mutex::new(VecDeque::new()),
        cv_done: Condvar::new(),
        stats: stats.clone(),
    });
    let config_addr = config as usize;
    for _ in 0..workers {
        let pool_for_thread = Arc::clone(&pool);
        thread::Builder::new()
            .name("karac-ws-handshake".to_string())
            .stack_size(512 * 1024)
            .spawn(move || ws_handshake_worker(config_addr, pool_for_thread))
            .expect("karac_runtime: failed to spawn ws handshake worker thread");
    }
    if let Some(stats) = stats {
        let pool_for_stats = Arc::clone(&pool);
        thread::Builder::new()
            .name("karac-ws-stats".to_string())
            .stack_size(64 * 1024)
            .spawn(move || ws_stats_reporter(stats, pool_for_stats))
            .expect("karac_runtime: failed to spawn ws stats reporter thread");
    }
    map.insert(listener_fd, Arc::clone(&pool));
    pool
}

/// Phase 6 line 236 slice 3 — server-side WebSocket-over-TLS accept.
///
/// Mirror of [`karac_runtime_ws_accept`] but the connection is
/// TLS-wrapped: TCP accept → rustls handshake → HTTP upgrade exchange
/// (over the encrypted transport) → register the session in
/// `tls::SESSIONS` so subsequent `ws_recv_text` / `ws_send_text` route
/// encryption through rustls automatically. Returns the upgraded
/// connection fd, or `-1` for invalid args.
///
/// **Concurrent handshake pool (Demo 1 M1 fix, 2026-05-29).** The
/// `accept(2)` stays on the calling (accept-loop) thread — it is cheap and
/// the codegen has already parked the caller on listener-readability — but
/// the *slow* part (rustls handshake + RFC 6455 upgrade, which blocks in
/// socket reads waiting for the peer to drive its half) runs on a
/// per-listener pool of worker threads so K handshakes proceed
/// concurrently. Pre-fix this was inline and serialized on the single
/// accept thread, so one slow peer's handshake reads stalled the whole
/// accept loop and throughput collapsed under load (diagnosed in
/// `docs/investigations/demo1_m1_verification.md`: at ~77K held the accept
/// thread sat blocked in `wait_woken` socket reads at ~7 conn/s with the
/// CPU idle). The ABI is unchanged — still returns a ready fd or -1 — so
/// codegen and the `accept_tls` stdlib builtin are untouched. Pool size:
/// `KARAC_WS_ACCEPT_THREADS` (default 32; handshakes are I/O-bound so this
/// is oversubscribed vs. core count). Session registration happens after
/// the rustls handshake but before the HTTP upgrade (in
/// `ws_handshake_conn_tls`), so the upgrade runs over TLS.
///
/// # Safety
///
/// `listener_fd` must be a valid TCP listener socket; `config` must be a
/// non-null pointer obtained from `karac_runtime_tls_config_new`.
#[cfg(unix)]
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_ws_accept_tls(
    listener_fd: i32,
    config: *mut crate::tls::KaracTlsConfig,
) -> i32 {
    use std::os::unix::io::{FromRawFd, IntoRawFd};

    if listener_fd < 0 || config.is_null() {
        return -1;
    }

    let pool = ws_handshake_pool_for(listener_fd, config);

    // Each call: (1) drain the accept backlog (non-blocking) and submit
    // every pending connection to the handshake workers so K handshakes
    // overlap, then (2) return the next completed handshake. The caller
    // (codegen) has already parked on listener-readability, so there is
    // normally ≥1 pending on entry; draining the rest fills the pipeline.
    // The done-wait uses a short timeout and re-drains on each tick so a
    // spurious park wakeup (0 accepted) or a lull while every worker is
    // mid-handshake can never wedge acceptance — new connections are
    // picked up within the timeout regardless.
    loop {
        {
            let listener = std::net::TcpListener::from_raw_fd(listener_fd);
            let mut submitted = 0usize;
            // Drain the backlog: accept until WouldBlock (or a real error).
            while let Ok((sock, _addr)) = listener.accept() {
                // Force the accepted connection into blocking mode. On
                // Linux, `accept(2)` always returns a blocking socket
                // regardless of the listener's flags. On macOS / BSD,
                // accepted sockets *inherit* `O_NONBLOCK` from the
                // listener, and `ws_handshake_pool_for` flipped this
                // listener non-blocking so the accept-drain loop above
                // returns `WouldBlock` instead of stalling. Without
                // this reset, the handshake worker's first
                // `TlsConnIo::read` returns `WouldBlock` before the
                // peer's request lands, `ws_read_http_request` reads it
                // as a hard IO error, and every connection fails at the
                // WS-upgrade step with no TLS-layer signal. Diagnosed
                // 2026-05-30 (M3 macOS-at-1M leg): server-side
                // `fail_ws_upgrade=N`, mean handshake ~0.5ms — fast
                // enough to be the first-read failure. The blocking
                // mode is then load-bearing in the handshake worker's
                // synchronous `complete_io` and HTTP read. Regression
                // test: `tls::tests::ws_accept_tls_succeeds_with_nonblocking_listener`.
                let _ = sock.set_nonblocking(false);
                let raw = sock.into_raw_fd();
                let mut q = pool.work.lock().unwrap_or_else(|p| p.into_inner());
                q.push_back(raw);
                drop(q);
                submitted += 1;
            }
            let _ = listener.into_raw_fd();
            if submitted > 0 {
                if let Some(stats) = pool.stats.as_ref() {
                    stats
                        .submitted
                        .fetch_add(submitted as u64, Ordering::Relaxed);
                }
                pool.cv_work.notify_all();
            }
        }

        let mut d = pool.done.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(fd) = d.pop_front() {
            return fd;
        }
        // No completed handshake yet — wait briefly, then loop back to
        // re-drain the backlog and re-check.
        let (mut g, _timeout) = pool
            .cv_done
            .wait_timeout(d, Duration::from_millis(5))
            .unwrap_or_else(|p| p.into_inner());
        if let Some(fd) = g.pop_front() {
            return fd;
        }
    }
}

/// Drive the RFC 6455 server-side HTTP upgrade exchange over the
/// supplied `Read + Write` transport. Returns `Ok(())` on a complete
/// 101 handshake; `Err(reason)` on any failure (IO error, malformed
/// request, missing Sec-WebSocket-Key). The error string is fed into
/// `HandshakeStats::first_errors` by the caller so the failure mode
/// is inspectable from `KARAC_WS_STATS=1`.
///
/// Shared with [`karac_runtime_ws_accept_tls`]; the plain-TCP
/// equivalent lives inline in [`karac_runtime_ws_accept`] above
/// and predates the generic-transport refactor.
#[cfg(unix)]
fn ws_drive_upgrade_handshake<S: std::io::Read + std::io::Write>(
    stream: &mut S,
) -> Result<(), String> {
    let request = match ws_read_http_request(stream) {
        Some(b) => b,
        None => return Err("ws_read_http_request: IO error or EOF before complete request".into()),
    };
    let key = match extract_sec_websocket_key(&request) {
        Some(k) => k,
        None => {
            let resp =
                b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\nContent-Length: 0\r\n\r\n";
            let _ = stream.write_all(resp);
            let preview = String::from_utf8_lossy(&request[..request.len().min(256)])
                .replace('\r', "\\r")
                .replace('\n', "\\n");
            return Err(format!(
                "Sec-WebSocket-Key header not found in {}B request; preview={preview:?}",
                request.len()
            ));
        }
    };
    let mut digest_input: Vec<u8> = Vec::with_capacity(key.len() + WS_HANDSHAKE_GUID.len());
    digest_input.extend_from_slice(key);
    digest_input.extend_from_slice(WS_HANDSHAKE_GUID);
    let digest = sha1(&digest_input);
    let accept = base64_encode(&digest);

    let response = format!(
        "HTTP/1.1 101 Switching Protocols\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Accept: {}\r\n\
         \r\n",
        accept
    );
    if let Err(e) = stream.write_all(response.as_bytes()) {
        return Err(format!("write_all(response): {e}"));
    }
    // Flush for TLS — the rustls writer buffers ciphertext until
    // explicit flush drains it through write_tls. For plain TCP
    // this is a no-op.
    if let Err(e) = stream.flush() {
        return Err(format!("flush: {e}"));
    }
    Ok(())
}

// ── Scheduler dispatcher (Phase 6 line 17 slice 4) ────────────────────────
//
// A background dispatcher thread that drains the background poller's
// wakeup queue and invokes `(task.poll_fn)(task.state, cancel)` on
// each wakeup. The `parked` field of each `KaracWakeup` is interpreted
// as `*const KaracParkedTask` — this is the convention that codegen
// (when state-machine lowering for network-boundary functions lands —
// phase-6 line 18) will follow when registering fds with the event
// loop.
//
// **Pairing with the background poller.** The dispatcher is opt-in
// and requires the background poller to be running. Calling
// `karac_runtime_scheduler_start_dispatcher` will auto-start the
// poller if it isn't already running — see the body.
//
// **Cancel routing.** v1 ships with a single process-global "never
// cancelled" `AtomicBool` that the dispatcher passes to each
// `poll_fn` invocation. Per-par-block cancel routing (so a parked
// task inside a fail-fast `par {}` observes its block's cancel flag)
// is later integration work — the FFI surface stays stable because
// the cancel pointer comes from the task's own state, not from the
// dispatcher's signature.
//
// **Lifetime convention.** The codegen is responsible for keeping the
// `KaracParkedTask` alive — and its `state` struct alive — from the
// `register_fd` call until `poll_fn` returns `Ready` or `Err`. The
// dispatcher does no allocation or freeing of task / state structs;
// it only invokes `poll_fn` through the type-erased pointers.

/// Internal dispatcher state. Held inside `Arc` so the spawned thread
/// can share it with the global slot.
struct SchedulerDispatcher {
    shutdown: AtomicBool,
    /// Per-process "never cancelled" flag. v1 placeholder — passed to
    /// every `poll_fn` invocation. When per-par-block cancel routing
    /// lands, parked tasks will carry the appropriate per-block flag
    /// in their `state` struct and `poll_fn` will read it from there
    /// instead of (or in addition to) this arg.
    cancel: AtomicBool,
    /// Counters for test verification + diagnostics. Updated unsynchronized
    /// (Relaxed) — they only need monotonic-write visibility, not strict
    /// ordering against other operations.
    polls: std::sync::atomic::AtomicU64,
    ready_observations: std::sync::atomic::AtomicU64,
    err_observations: std::sync::atomic::AtomicU64,
    pending_observations: std::sync::atomic::AtomicU64,
    handle: Mutex<Option<thread::JoinHandle<()>>>,
}

static SCHEDULER_DISPATCHER: Mutex<Option<Arc<SchedulerDispatcher>>> = Mutex::new(None);

fn lock_scheduler_dispatcher_slot(
) -> std::sync::MutexGuard<'static, Option<Arc<SchedulerDispatcher>>> {
    SCHEDULER_DISPATCHER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn dispatcher_thread_main(disp: Arc<SchedulerDispatcher>) {
    // Drain wakeups in small batches; a short timeout makes shutdown
    // responsive without busy-spinning.
    let mut buf: [KaracWakeup; 16] = unsafe { std::mem::zeroed() };
    while !disp.shutdown.load(Ordering::Acquire) {
        // 100ms timeout — bounded enough that shutdown takes effect
        // within a poll cycle, brief enough that the dispatcher
        // doesn't wake up needlessly when idle. The background poller
        // delivers wakeups via the queue's Condvar, so this isn't a
        // busy-loop.
        let n = unsafe {
            karac_runtime_event_loop_take_wakeups(buf.as_mut_ptr(), buf.len(), 100_000_000)
        };
        for i in 0..n {
            // SAFETY: indices 0..n were written by take_wakeups.
            let w = unsafe { std::ptr::read(buf.as_ptr().add(i)) };
            // One-shot claim: resolve the parked record from the *live*
            // registration map (removing the token) rather than trusting
            // the pointer captured into the wakeup. This is what makes the
            // dispatcher safe against (a) duplicate / stale wakeups for the
            // same fd (the second `take` returns `None` → skip, so a task's
            // `poll_fn` runs at most once per registration) and (b)
            // concurrent frees: the caller frees its parked record only
            // after `poll_fn` signals it, which is strictly after this
            // `take`+invoke, so the pointer is live for the duration of the
            // call. A `None` here means the registration was already
            // claimed (or deregistered) — the wakeup is spent, skip it.
            let parked =
                match global_event_loop().take_registration(RegistrationToken(w.token as usize)) {
                    Some(p) if !p.is_null() => p,
                    _ => continue,
                };
            // SAFETY: `take_registration` returned the parked pointer from
            // the live map; the codegen / caller convention keeps the
            // `KaracParkedTask` (and its state) alive until `poll_fn`
            // signals completion, which happens inside this call. The
            // dispatcher invokes `poll_fn` but never derefs `state` itself.
            let task = unsafe { &*(parked as *const KaracParkedTask) };
            let result = unsafe { (task.poll_fn)(task.state, &disp.cancel) };
            disp.polls.fetch_add(1, Ordering::Relaxed);
            match result {
                0 => {
                    disp.pending_observations.fetch_add(1, Ordering::Relaxed);
                }
                1 => {
                    disp.ready_observations.fetch_add(1, Ordering::Relaxed);
                }
                2 => {
                    disp.err_observations.fetch_add(1, Ordering::Relaxed);
                }
                _ => {
                    // Unknown discriminant — treat as Err for
                    // accounting purposes.
                    disp.err_observations.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }
}

/// Start the scheduler dispatcher thread.
///
/// Auto-starts the background poller if it isn't already running —
/// the dispatcher's `take_wakeups` calls would otherwise return 0
/// forever. Idempotent: a second call while running returns 0
/// without re-spawning.
///
/// Returns 0 on success.
#[no_mangle]
pub extern "C" fn karac_runtime_scheduler_start_dispatcher() -> i32 {
    let mut slot = lock_scheduler_dispatcher_slot();
    if slot.is_some() {
        return 0;
    }
    // Auto-start the background poller — take_wakeups depends on it.
    let _ = karac_runtime_event_loop_start_background_thread();

    let disp = Arc::new(SchedulerDispatcher {
        shutdown: AtomicBool::new(false),
        cancel: AtomicBool::new(false),
        polls: std::sync::atomic::AtomicU64::new(0),
        ready_observations: std::sync::atomic::AtomicU64::new(0),
        err_observations: std::sync::atomic::AtomicU64::new(0),
        pending_observations: std::sync::atomic::AtomicU64::new(0),
        handle: Mutex::new(None),
    });
    let disp_for_thread = Arc::clone(&disp);
    let join = thread::Builder::new()
        .name("karac-scheduler-dispatcher".to_string())
        .spawn(move || dispatcher_thread_main(disp_for_thread))
        .expect("karac_runtime: failed to spawn scheduler dispatcher thread");
    *disp.handle.lock().unwrap_or_else(|p| p.into_inner()) = Some(join);
    *slot = Some(disp);
    0
}

/// Signal the dispatcher to stop, join the thread, clear the global
/// slot. Returns 0 on success, -1 if no dispatcher is running.
///
/// Does NOT stop the background poller; the poller has its own
/// shutdown FFI and may be used independently of the dispatcher.
#[no_mangle]
pub extern "C" fn karac_runtime_scheduler_shutdown_dispatcher() -> i32 {
    let disp = {
        let mut slot = lock_scheduler_dispatcher_slot();
        match slot.take() {
            Some(d) => d,
            None => return -1,
        }
    };
    disp.shutdown.store(true, Ordering::Release);
    // The dispatcher's `take_wakeups` call has a 100ms timeout, so
    // shutdown takes effect within one poll cycle without further
    // signaling. (No need to wake or notify here.)
    let join = disp.handle.lock().unwrap_or_else(|p| p.into_inner()).take();
    if let Some(h) = join {
        let _ = h.join();
    }
    0
}

/// Snapshot of the scheduler dispatcher's atomic counters.
///
/// `#[repr(C)]` pins the layout for callers reading through FFI.
/// Counter semantics:
/// - `polls`: total number of `poll_fn` invocations the dispatcher has
///   made since process start (cumulative; never decreases).
/// - `ready_observations`: count of poll calls that returned `Ready` (1).
/// - `err_observations`: count of poll calls that returned `Err` (2)
///   or any unknown non-zero discriminant.
/// - `pending_observations`: count of poll calls that returned
///   `Pending` (0).
///
/// Invariant: `polls == ready_observations + err_observations +
/// pending_observations`. The counters are read with `Relaxed`
/// ordering (each independently), so a snapshot can transiently
/// observe the sum mismatching the total by one if a poll completes
/// between reads. Treat the values as approximate for diagnostics;
/// don't rely on cross-counter consistency.
#[repr(C)]
pub struct KaracSchedulerStats {
    pub polls: u64,
    pub ready_observations: u64,
    pub err_observations: u64,
    pub pending_observations: u64,
}

/// Read the dispatcher's counter snapshot into the caller's buffer.
///
/// Returns 0 on success, -1 if the dispatcher is not running. On -1
/// the contents of `*out` are unspecified — callers must check the
/// return value before reading.
///
/// # Safety
///
/// `out` must point to a writable `KaracSchedulerStats`. The fn writes
/// the four counters as one atomic write per field (no struct-level
/// atomicity).
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_scheduler_stats_snapshot(
    out: *mut KaracSchedulerStats,
) -> i32 {
    let disp = {
        let slot = lock_scheduler_dispatcher_slot();
        match slot.as_ref() {
            Some(d) => Arc::clone(d),
            None => return -1,
        }
    };
    let snapshot = KaracSchedulerStats {
        polls: disp.polls.load(Ordering::Relaxed),
        ready_observations: disp.ready_observations.load(Ordering::Relaxed),
        err_observations: disp.err_observations.load(Ordering::Relaxed),
        pending_observations: disp.pending_observations.load(Ordering::Relaxed),
    };
    // SAFETY: caller guarantees `out` is writable for one
    // `KaracSchedulerStats`.
    unsafe {
        out.write(snapshot);
    }
    0
}

// ── Per-park completion slot (Phase 6 line 170 async-sched slice 2/3) ──
//
// The dispatcher-yield model splits a single park across two threads: the
// *caller* thread runs the leaf poll-fn's `state_0` (register fd, return
// Pending) then blocks waiting for readiness; the *dispatcher* thread runs
// `state_1` when the fd actually fires (routed by the wakeup's `parked`
// pointer) and signals the caller to resume. `KaracParkSlot` is the
// hand-off primitive between them — a one-shot condvar.
//
// **Why a per-park slot instead of the global wakeup queue.** The
// pre-slice-2 codegen blocked `state_1` on the *unfiltered* global
// `take_wakeups`, so two concurrently-parked tasks stole each other's
// wakeups (the accept-loop-wedges-at-1 P0 blocker). Routing each fd
// wakeup through the dispatcher to the correct `parked` pointer — which
// then signals *that park's own slot* — eliminates the stealing: a
// wakeup for fd A can only ever signal A's slot.
//
// **One-shot / no lost wakeup.** `done` is set under the mutex before
// `notify_one`, and `wait` re-checks `done` under the mutex, so a signal
// that races ahead of the wait is not lost (the wait observes `done ==
// true` and returns immediately). The caller frees the slot only after
// `wait` returns, by which point `signal` has released the mutex — so the
// free never races a live `signal`.
#[repr(C)]
pub struct KaracParkSlot {
    done: Mutex<bool>,
    cv: Condvar,
}

/// Allocate a fresh park slot. Returns an owning raw pointer the caller
/// must release exactly once via [`karac_runtime_park_slot_free`].
#[no_mangle]
pub extern "C" fn karac_runtime_park_slot_new() -> *mut KaracParkSlot {
    Box::into_raw(Box::new(KaracParkSlot {
        done: Mutex::new(false),
        cv: Condvar::new(),
    }))
}

/// Block until the slot is signaled. Returns immediately if it was
/// already signaled before this call (no lost wakeup).
///
/// # Safety
///
/// `slot` must be a live pointer returned by
/// [`karac_runtime_park_slot_new`] and not yet freed.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_park_slot_wait(slot: *mut KaracParkSlot) {
    if slot.is_null() {
        return;
    }
    // SAFETY: caller's contract — `slot` is live.
    let s = unsafe { &*slot };
    let mut done = s.done.lock().unwrap_or_else(|p| p.into_inner());
    while !*done {
        done = s.cv.wait(done).unwrap_or_else(|p| p.into_inner());
    }
}

/// Signal the slot, unblocking a (current or future) waiter. Idempotent:
/// signaling twice is harmless (the second is a no-op `done = true`).
///
/// # Safety
///
/// `slot` must be a live pointer returned by
/// [`karac_runtime_park_slot_new`] and not yet freed. Called from the
/// dispatcher thread via the leaf poll-fn's `state_1`.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_park_slot_signal(slot: *mut KaracParkSlot) {
    if slot.is_null() {
        return;
    }
    // SAFETY: caller's contract — `slot` is live until the matching
    // `wait` returns, which cannot happen before this `signal` releases
    // the mutex below.
    let s = unsafe { &*slot };
    let mut done = s.done.lock().unwrap_or_else(|p| p.into_inner());
    *done = true;
    s.cv.notify_one();
}

/// Free a park slot. Idempotent on null; must be called exactly once per
/// [`karac_runtime_park_slot_new`], after the matching `wait` returns.
///
/// # Safety
///
/// `slot` must be a pointer returned by [`karac_runtime_park_slot_new`]
/// that has not already been freed. No outstanding `signal` may still be
/// executing against it (guaranteed by the wait/signal mutex hand-off).
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_park_slot_free(slot: *mut KaracParkSlot) {
    if slot.is_null() {
        return;
    }
    // SAFETY: caller's contract — `slot` came from `park_slot_new` and is
    // freed exactly once.
    drop(unsafe { Box::from_raw(slot) });
}

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
        let ev = EventLoop::new().unwrap();
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

        let ev = EventLoop::new().unwrap();

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

    /// Regression: `karac_runtime_tcp_bind` must produce a listener
    /// whose `accept(2)` queue actually receives incoming SYNs from
    /// `connect(2)` on the bound port. Pins the bind path end-to-end
    /// (parse → socket2::Socket → bind → listen → into TcpListener →
    /// into_raw_fd). Sets the listener to non-blocking + polls so a
    /// broken accept queue (the diagnosed regression — kernel silently
    /// ignored SYNs to the bound port) fails fast instead of hanging.
    #[cfg(unix)]
    #[test]
    fn tcp_bind_produces_connectable_listener() {
        use std::os::unix::io::FromRawFd;

        let addr = b"127.0.0.1:0";
        let fd = unsafe { karac_runtime_tcp_bind(addr.as_ptr(), addr.len() as i64) };
        assert!(fd > 0, "tcp_bind should return a positive fd, got {fd}");

        let listener = unsafe { std::net::TcpListener::from_raw_fd(fd) };
        let port = listener
            .local_addr()
            .expect("bound listener local_addr")
            .port();
        assert!(port > 0, "ephemeral port must be non-zero");
        listener
            .set_nonblocking(true)
            .expect("set listener nonblocking");

        // Worker connects with a deadline; the test thread polls accept
        // until the connection arrives (or the polling budget elapses).
        let connector = thread::spawn(move || {
            let target: std::net::SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
            std::net::TcpStream::connect_timeout(&target, Duration::from_secs(3))
                .map_err(|e| e.to_string())
        });

        let mut accepted = None;
        let start = std::time::Instant::now();
        while start.elapsed() < Duration::from_secs(5) {
            match listener.accept() {
                Ok(c) => {
                    accepted = Some(c);
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(50));
                }
                Err(e) => panic!("accept errored: {e}"),
            }
        }
        let connect_result = connector.join().expect("connector thread joins");
        assert!(
            accepted.is_some(),
            "listener.accept() did not yield a connection within 5s — \
             karac_runtime_tcp_bind's listener is unreachable. \
             connect_result = {connect_result:?}"
        );
        connect_result.expect("connector should have completed successfully");
    }

    /// `karac_runtime_tcp_connect` must open a real connection to a
    /// listening peer (returns a positive fd that the peer accepts) and
    /// must fail with `-1` against a closed port. Client-side mirror of
    /// `tcp_bind_produces_connectable_listener`.
    #[cfg(unix)]
    #[test]
    fn tcp_connect_reaches_a_listener() {
        // Harness listener owns the server side.
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").expect("harness listener bind failed");
        let port = listener.local_addr().expect("local_addr").port();
        listener.set_nonblocking(true).expect("nonblocking");

        let addr = format!("127.0.0.1:{port}");
        let fd = unsafe { karac_runtime_tcp_connect(addr.as_ptr(), addr.len() as i64) };
        assert!(
            fd > 0,
            "tcp_connect to a live listener should return a positive fd, got {fd}"
        );

        // The listener must observe the connection the FFI just opened.
        let mut accepted = None;
        let start = std::time::Instant::now();
        while start.elapsed() < Duration::from_secs(3) {
            match listener.accept() {
                Ok(c) => {
                    accepted = Some(c);
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(20));
                }
                Err(e) => panic!("accept errored: {e}"),
            }
        }
        assert!(
            accepted.is_some(),
            "listener did not accept the connection karac_runtime_tcp_connect opened",
        );
        // Reclaim the raw fd into a std TcpStream so its Drop closes it.
        unsafe {
            use std::os::unix::io::FromRawFd;
            drop(std::net::TcpStream::from_raw_fd(fd));
        }

        // Closed port → connect fails with -1 (the v1 bare sentinel;
        // line 74 enriches this to -errno).
        let dead = b"127.0.0.1:1";
        let dead_fd = unsafe { karac_runtime_tcp_connect(dead.as_ptr(), dead.len() as i64) };
        assert_eq!(
            dead_fd, -1,
            "connect to a closed port should return -1, got {dead_fd}"
        );

        // Malformed address → -1 (parse failure).
        let bad = b"not an address";
        let bad_fd = unsafe { karac_runtime_tcp_connect(bad.as_ptr(), bad.len() as i64) };
        assert_eq!(
            bad_fd, -1,
            "connect to an unparseable address should return -1"
        );
    }

    #[test]
    fn poll_timeout_returns_empty_wakeups() {
        let ev = EventLoop::new().unwrap();
        let wakeups = ev.run_once(Some(Duration::from_millis(10))).unwrap();
        assert!(wakeups.is_empty(), "no fds registered → no wakeups");
    }

    #[test]
    fn tokens_are_distinct_across_registrations() {
        // Bind two listeners to different ports, register both, verify
        // tokens differ. Also checks `next_token` increments correctly.
        let mut l1 = TcpListener::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let mut l2 = TcpListener::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let ev = EventLoop::new().unwrap();
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

    // ── Background poller thread (Phase 6 line 17 slice 3) ─────────────

    /// Test-only guard that shuts down the background poller on drop.
    /// Holds the FFI test lock so background-poller tests serialize
    /// against the other FFI tests. The drop order matters: shutdown
    /// runs first (while the FFI lock is still held), then the FFI lock
    /// releases.
    struct BackgroundPollerTestGuard {
        _ffi: std::sync::MutexGuard<'static, ()>,
    }

    impl Drop for BackgroundPollerTestGuard {
        fn drop(&mut self) {
            let _ = karac_runtime_event_loop_shutdown_background_thread();
        }
    }

    fn start_background_poller_for_test() -> BackgroundPollerTestGuard {
        let _ffi = ffi_test_guard();
        // Ensure clean start: a prior test that aborted abnormally could
        // have left the thread running.
        let _ = karac_runtime_event_loop_shutdown_background_thread();
        let rc = karac_runtime_event_loop_start_background_thread();
        assert_eq!(rc, 0, "start_background_thread should report success");
        BackgroundPollerTestGuard { _ffi }
    }

    #[cfg(unix)]
    #[test]
    fn background_thread_drains_wakeups_via_take() {
        let _guard = start_background_poller_for_test();
        use std::os::fd::AsRawFd;

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let local = listener.local_addr().unwrap();
        let raw_fd = listener.as_raw_fd();

        let marker: u64 = 0xBEEF_0F0F_BEEF_0F0F;
        let parked = std::ptr::addr_of!(marker) as *mut c_void;
        let token = karac_runtime_event_loop_register_fd(raw_fd, 0, parked);
        assert_ne!(token, 0, "register should return a non-zero token");

        let connector = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            let _stream = std::net::TcpStream::connect(local).unwrap();
            thread::sleep(Duration::from_millis(50));
        });

        let mut buf: [KaracWakeup; 4] = unsafe { std::mem::zeroed() };
        let n = unsafe {
            karac_runtime_event_loop_take_wakeups(buf.as_mut_ptr(), buf.len(), 2_000_000_000)
        };
        assert!(
            n >= 1,
            "expected at least one wakeup via background thread, got {n}"
        );
        let w = &buf[0];
        assert_eq!(w.token, token);
        assert_eq!(w.parked, parked);
        assert_eq!(w.direction, IoDirection::Read as u8);

        connector.join().unwrap();
        let dereg = karac_runtime_event_loop_deregister_fd(raw_fd, token);
        assert_eq!(dereg, 0);
    }

    #[test]
    fn background_thread_take_nonblocking_returns_zero_on_empty_queue() {
        let _guard = start_background_poller_for_test();
        let mut buf: [KaracWakeup; 4] = unsafe { std::mem::zeroed() };
        let n = unsafe { karac_runtime_event_loop_take_wakeups(buf.as_mut_ptr(), buf.len(), 0) };
        assert_eq!(n, 0);
    }

    #[test]
    fn background_thread_take_with_timeout_unblocks_on_empty() {
        let _guard = start_background_poller_for_test();
        let mut buf: [KaracWakeup; 4] = unsafe { std::mem::zeroed() };
        let start = Instant::now();
        let n = unsafe {
            karac_runtime_event_loop_take_wakeups(buf.as_mut_ptr(), buf.len(), 100_000_000)
        };
        let elapsed = start.elapsed();
        assert_eq!(n, 0);
        assert!(
            elapsed >= Duration::from_millis(80),
            "should wait ~100ms before timing out, only waited {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_millis(500),
            "should not wait much longer than 100ms, waited {elapsed:?}"
        );
    }

    #[test]
    fn background_thread_start_is_idempotent() {
        let _guard = start_background_poller_for_test();
        let rc = karac_runtime_event_loop_start_background_thread();
        assert_eq!(rc, 0);
    }

    #[test]
    fn background_thread_shutdown_returns_minus_one_when_not_running() {
        let _guard = ffi_test_guard();
        let _ = karac_runtime_event_loop_shutdown_background_thread();
        let rc = karac_runtime_event_loop_shutdown_background_thread();
        assert_eq!(rc, -1);
    }

    #[cfg(unix)]
    #[test]
    fn direct_ffi_poll_short_circuits_when_background_is_running() {
        let _guard = start_background_poller_for_test();
        // With the background poller owning polling, direct FFI poll
        // returns 0 immediately so callers don't contend for the
        // inner poll Mutex.
        let mut buf: [KaracWakeup; 4] = unsafe { std::mem::zeroed() };
        let start = Instant::now();
        let n =
            unsafe { karac_runtime_event_loop_poll(2_000_000_000, buf.as_mut_ptr(), buf.len()) };
        let elapsed = start.elapsed();
        assert_eq!(n, 0);
        assert!(
            elapsed < Duration::from_millis(100),
            "direct poll should return immediately, took {elapsed:?}"
        );
    }

    // ── Scheduler dispatcher (Phase 6 line 17 slice 4) ─────────────────

    /// Test-only guard that shuts down the scheduler dispatcher AND
    /// the background poller on drop. Holds the FFI test lock so
    /// dispatcher tests serialize against the rest of the FFI tests.
    struct SchedulerTestGuard {
        _ffi: std::sync::MutexGuard<'static, ()>,
    }

    impl Drop for SchedulerTestGuard {
        fn drop(&mut self) {
            // Dispatcher first (depends on the poller's queue), then
            // poller. Both are idempotent on already-stopped state.
            let _ = karac_runtime_scheduler_shutdown_dispatcher();
            let _ = karac_runtime_event_loop_shutdown_background_thread();
        }
    }

    fn start_scheduler_for_test() -> SchedulerTestGuard {
        let _ffi = ffi_test_guard();
        // Ensure clean start.
        let _ = karac_runtime_scheduler_shutdown_dispatcher();
        let _ = karac_runtime_event_loop_shutdown_background_thread();
        let rc = karac_runtime_scheduler_start_dispatcher();
        assert_eq!(rc, 0, "scheduler dispatcher should start");
        SchedulerTestGuard { _ffi }
    }

    /// State for the end-to-end scheduler test. The state machine has
    /// two states: state 0 registers the listener's fd and returns
    /// Pending; state 1 sets `completed = true` and returns Ready.
    /// The initial poll (state 0) is invoked by the test thread; the
    /// re-poll (state 1) is invoked by the dispatcher.
    #[cfg(unix)]
    #[repr(C)]
    struct SchedulerTestState {
        tag: u8,
        listener_fd: i32,
        token: u64,
        completed: std::sync::atomic::AtomicBool,
    }

    #[cfg(unix)]
    unsafe extern "C" fn scheduler_test_poll_fn(
        state_ptr: *mut c_void,
        _cancel: *const std::sync::atomic::AtomicBool,
    ) -> u8 {
        // SAFETY: caller passes a valid `*mut SchedulerTestState` that
        // lives across both invocations. Both sequential, never
        // concurrent (initial call from test thread returns Pending
        // before the dispatcher starts polling).
        let state = unsafe { &mut *(state_ptr as *mut SchedulerTestState) };
        match state.tag {
            0 => {
                // Register the fd; the `parked` pointer points to the
                // KaracParkedTask wrapping this state — that's the
                // codegen convention slice 4 implements.
                let task_ptr = state as *mut SchedulerTestState as *mut c_void;
                // The actual parked pointer passed to register_fd
                // points to the KaracParkedTask, not the state. The
                // caller (test thread) supplies that pointer; we just
                // store the registration token so we can deregister
                // on Ready.
                state.tag = 1;
                let _ = task_ptr; // silence unused (used by caller via FFI)
                KaracPollResult::Pending as u8
            }
            1 => {
                // Cleanup: deregister the fd, signal completion.
                let _ = karac_runtime_event_loop_deregister_fd(state.listener_fd, state.token);
                state.completed.store(true, Ordering::Release);
                KaracPollResult::Ready as u8
            }
            _ => KaracPollResult::Err as u8,
        }
    }

    #[cfg(unix)]
    #[test]
    fn dispatcher_drives_parked_task_to_completion_on_wakeup() {
        let _guard = start_scheduler_for_test();
        use std::os::fd::AsRawFd;

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let local = listener.local_addr().unwrap();
        let listener_fd = listener.as_raw_fd();

        // Build state + parked task. Box pins them so their address
        // doesn't move while the dispatcher holds the pointer.
        let mut state = Box::new(SchedulerTestState {
            tag: 0,
            listener_fd,
            token: 0,
            completed: std::sync::atomic::AtomicBool::new(false),
        });
        let task = Box::new(KaracParkedTask {
            poll_fn: scheduler_test_poll_fn,
            state: &mut *state as *mut SchedulerTestState as *mut c_void,
        });
        let task_ptr = &*task as *const KaracParkedTask as *mut c_void;

        // Register the listener fd with `parked = &task` per the
        // dispatcher convention.
        let token = karac_runtime_event_loop_register_fd(listener_fd, 0, task_ptr);
        assert_ne!(token, 0, "register should succeed");
        state.token = token;

        // Initial poll (state 0): just bumps tag to 1. (We register
        // BEFORE this in the test because the hand-rolled state-machine
        // doesn't have a tag-0 register step in this layout — the test
        // owns registration. In a real codegen-emitted state machine,
        // the tag-0 case would do the register itself.)
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let initial = unsafe { (task.poll_fn)(task.state, &cancel) };
        assert_eq!(initial, KaracPollResult::Pending as u8);

        // Trigger fd readability.
        let connector = thread::spawn(move || {
            let _stream = std::net::TcpStream::connect(local).unwrap();
            thread::sleep(Duration::from_millis(50));
        });

        // Spin-wait for the dispatcher to drive completion. Bounded
        // by 2s so a broken dispatcher fails the test instead of
        // hanging it.
        let start = Instant::now();
        while !state.completed.load(Ordering::Acquire) {
            if start.elapsed() > Duration::from_secs(2) {
                panic!("dispatcher did not drive task to completion within 2s");
            }
            thread::sleep(Duration::from_millis(10));
        }

        connector.join().unwrap();

        // Drop order: connector joined → state still pinned → task
        // still pinned. Now safe to drop in test cleanup.
        drop(task);
        drop(state);
    }

    // Async-scheduler integration slice 1 (phase 6 line 170 P0 blocker):
    // the load-bearing invariant the whole dispatcher-yield model rests
    // on — the dispatcher must drive MULTIPLE concurrently-parked tasks
    // to completion, routing each fd-readiness wakeup to the correct
    // task by its `parked` pointer. The single-task test above proves
    // the mechanism; this proves it doesn't mis-route across tasks (the
    // exact failure the codegen's current blocking-spin model hits: two
    // tasks blocked on one global queue steal each other's wakeups). The
    // runtime side is already correct here — the demo wedge is purely
    // that codegen never routes through this dispatcher.
    #[cfg(unix)]
    #[test]
    fn dispatcher_drives_two_concurrent_parked_tasks_to_completion() {
        let _guard = start_scheduler_for_test();
        use std::os::fd::AsRawFd;

        // Two independent listeners → two distinct fds → two distinct
        // parked tasks, registered concurrently.
        let make = || {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.set_nonblocking(true).unwrap();
            let addr = l.local_addr().unwrap();
            let fd = l.as_raw_fd();
            (l, addr, fd)
        };
        let (l0, addr0, fd0) = make();
        let (l1, addr1, fd1) = make();

        let mut s0 = Box::new(SchedulerTestState {
            tag: 0,
            listener_fd: fd0,
            token: 0,
            completed: std::sync::atomic::AtomicBool::new(false),
        });
        let mut s1 = Box::new(SchedulerTestState {
            tag: 0,
            listener_fd: fd1,
            token: 0,
            completed: std::sync::atomic::AtomicBool::new(false),
        });
        let t0 = Box::new(KaracParkedTask {
            poll_fn: scheduler_test_poll_fn,
            state: &mut *s0 as *mut SchedulerTestState as *mut c_void,
        });
        let t1 = Box::new(KaracParkedTask {
            poll_fn: scheduler_test_poll_fn,
            state: &mut *s1 as *mut SchedulerTestState as *mut c_void,
        });
        let t0_ptr = &*t0 as *const KaracParkedTask as *mut c_void;
        let t1_ptr = &*t1 as *const KaracParkedTask as *mut c_void;

        let tok0 = karac_runtime_event_loop_register_fd(fd0, 0, t0_ptr);
        let tok1 = karac_runtime_event_loop_register_fd(fd1, 0, t1_ptr);
        assert_ne!(tok0, 0);
        assert_ne!(tok1, 0);
        assert_ne!(tok0, tok1, "distinct registrations get distinct tokens");
        s0.token = tok0;
        s1.token = tok1;

        // Park both (tag 0 → 1, Pending) before any readiness fires.
        let cancel = std::sync::atomic::AtomicBool::new(false);
        assert_eq!(
            unsafe { (t0.poll_fn)(t0.state, &cancel) },
            KaracPollResult::Pending as u8
        );
        assert_eq!(
            unsafe { (t1.poll_fn)(t1.state, &cancel) },
            KaracPollResult::Pending as u8
        );

        // Trigger both fds (order intentionally interleaved).
        let c0 = thread::spawn(move || {
            let _s = std::net::TcpStream::connect(addr0).unwrap();
            thread::sleep(Duration::from_millis(50));
        });
        let c1 = thread::spawn(move || {
            let _s = std::net::TcpStream::connect(addr1).unwrap();
            thread::sleep(Duration::from_millis(50));
        });

        // Both must complete — neither task's wakeup may be stolen by
        // the other. Bounded so a mis-routing regression fails rather
        // than hangs.
        let start = Instant::now();
        loop {
            let done0 = s0.completed.load(Ordering::Acquire);
            let done1 = s1.completed.load(Ordering::Acquire);
            if done0 && done1 {
                break;
            }
            if start.elapsed() > Duration::from_secs(3) {
                panic!("dispatcher did not drive BOTH tasks to completion within 3s (done0={done0}, done1={done1})");
            }
            thread::sleep(Duration::from_millis(10));
        }

        c0.join().unwrap();
        c1.join().unwrap();
        drop(t0);
        drop(t1);
        drop(s0);
        drop(s1);
        drop(l0);
        drop(l1);
    }

    /// Parked-record poll-fn that mirrors the codegen leaf park's
    /// `state_1`: the dispatcher invokes it on readiness; it signals the
    /// per-park completion slot (passed as the `state` pointer) and
    /// returns Ready. Used by the accept-loop re-park repro below.
    #[cfg(unix)]
    unsafe extern "C" fn accept_loop_signal_poll(
        state_ptr: *mut c_void,
        _cancel: *const std::sync::atomic::AtomicBool,
    ) -> u8 {
        unsafe {
            karac_runtime_park_slot_signal(state_ptr as *mut KaracParkSlot);
        }
        KaracPollResult::Ready as u8
    }

    /// Async-sched slice 2/3 regression: the accept-loop shape must drain
    /// MANY connections without wedging. This mirrors the demo's `loop {
    /// park(listener); accept(); }` purely at the runtime layer (no TLS,
    /// no codegen): each iteration allocates a completion slot, registers
    /// the listener fd with a parked record whose poll-fn signals that
    /// slot, blocks on the slot, then deregisters + re-registers next
    /// iteration. The connector bursts all K connections up front, so by
    /// the time the loop re-registers the listener it is already readable
    /// (a connection sits in the backlog) — the exact case where a poller
    /// blocked in `run_once(None)` would miss the registration unless
    /// `register_fd` wakes it. A regression here (lost wakeup on re-park)
    /// reproduces the intermittent multi-connection wedge the bench
    /// harness surfaced; the bounded timeout fails loudly instead of
    /// hanging.
    #[cfg(unix)]
    #[test]
    fn accept_loop_re_park_drains_burst_connections_without_wedging() {
        let _guard = start_scheduler_for_test();
        use std::os::fd::AsRawFd;

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let local = listener.local_addr().unwrap();
        let lfd = listener.as_raw_fd();

        const K: usize = 40;
        // Burst all K connections up front (no pacing) so the listener
        // backlog is full while the accept loop drains — every re-park
        // finds the listener already readable.
        let connector = thread::spawn(move || {
            let mut held = Vec::with_capacity(K);
            for _ in 0..K {
                if let Ok(s) = std::net::TcpStream::connect(local) {
                    held.push(s);
                }
            }
            // Hold the client ends open while the loop drains/registers
            // them (idle conns). The loop completes well under this.
            thread::sleep(Duration::from_secs(1));
            held
        });

        // Hard self-timeout watchdog: a wedge blocks the loop *inside*
        // `park_slot_wait` (an FFI call that never returns), so the loop
        // body can't re-check elapsed time — a regression would hang
        // forever. A cancellable watchdog thread aborts the process if the
        // loop hasn't finished by the deadline, reporting how far it got.
        // Bounds the test (never hangs) and pinpoints the wedge iteration.
        let progress = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let watchdog_progress = std::sync::Arc::clone(&progress);
        let watchdog_done = std::sync::Arc::clone(&done);
        let watchdog = thread::spawn(move || {
            let start = Instant::now();
            while start.elapsed() < Duration::from_secs(8) {
                if watchdog_done.load(Ordering::Acquire) {
                    return; // loop finished cleanly — stand down.
                }
                thread::sleep(Duration::from_millis(50));
            }
            eprintln!(
                "WATCHDOG: accept loop wedged — only {}/{K} connections accepted in 8s",
                watchdog_progress.load(Ordering::Relaxed)
            );
            std::process::abort();
        });

        let mut accepted: Vec<std::net::TcpStream> = Vec::with_capacity(K);
        while accepted.len() < K {
            // Mirror the codegen leaf park, including the heap state the
            // demo frees each iteration: a boxed parked record (freed at
            // end of the iteration) + slot + register (wakes poller) +
            // wait + deregister + free. The per-iteration heap alloc/free
            // of the parked record (so its address can be reused) is the
            // key stressor distinguishing this from a stack-record loop —
            // it's what makes a stale / duplicate wakeup deref a freed
            // record, which one-shot dispatch must prevent.
            let slot = karac_runtime_park_slot_new();
            let mut task = Box::new(KaracParkedTask {
                poll_fn: accept_loop_signal_poll,
                state: slot as *mut c_void,
            });
            let parked = &mut *task as *mut KaracParkedTask as *mut c_void;
            let token = karac_runtime_event_loop_register_fd(lfd, 0, parked);
            assert_ne!(
                token,
                0,
                "register should succeed at iter {}",
                accepted.len()
            );
            unsafe {
                karac_runtime_park_slot_wait(slot);
            }
            let _ = karac_runtime_event_loop_deregister_fd(lfd, token);
            unsafe {
                karac_runtime_park_slot_free(slot);
            }
            drop(task); // free the per-iteration parked record (demo frees state)
                        // The park returned, so the listener should be readable now.
            match listener.accept() {
                Ok((s, _)) => {
                    accepted.push(s);
                    progress.store(accepted.len(), Ordering::Relaxed);
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    // Spurious / already-drained — loop re-parks.
                }
                Err(e) => panic!("accept failed: {e}"),
            }
        }

        // Loop finished — stand the watchdog down before it aborts.
        done.store(true, Ordering::Release);
        let _ = watchdog.join();
        let _ = connector.join();
        assert_eq!(accepted.len(), K, "all connections must be accepted");
    }

    #[test]
    fn dispatcher_start_is_idempotent() {
        let _guard = start_scheduler_for_test();
        let rc = karac_runtime_scheduler_start_dispatcher();
        assert_eq!(rc, 0);
    }

    #[test]
    fn dispatcher_shutdown_returns_minus_one_when_not_running() {
        let _guard = ffi_test_guard();
        let _ = karac_runtime_scheduler_shutdown_dispatcher();
        let rc = karac_runtime_scheduler_shutdown_dispatcher();
        assert_eq!(rc, -1);
    }

    #[test]
    fn scheduler_stats_snapshot_returns_minus_one_when_dispatcher_not_running() {
        let _guard = ffi_test_guard();
        let _ = karac_runtime_scheduler_shutdown_dispatcher();
        let mut stats = KaracSchedulerStats {
            polls: 0,
            ready_observations: 0,
            err_observations: 0,
            pending_observations: 0,
        };
        let rc = unsafe { karac_runtime_scheduler_stats_snapshot(&mut stats) };
        assert_eq!(rc, -1, "should report not-running");
    }

    #[cfg(unix)]
    #[test]
    fn scheduler_stats_track_dispatcher_polls() {
        let _guard = start_scheduler_for_test();
        use std::os::fd::AsRawFd;

        // Initial snapshot — dispatcher just started, counters at 0.
        let mut before = KaracSchedulerStats {
            polls: 0,
            ready_observations: 0,
            err_observations: 0,
            pending_observations: 0,
        };
        let rc = unsafe { karac_runtime_scheduler_stats_snapshot(&mut before) };
        assert_eq!(rc, 0);
        assert_eq!(before.polls, 0);
        assert_eq!(before.ready_observations, 0);

        // Drive one parked task to completion (same shape as the
        // earlier dispatcher test).
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let local = listener.local_addr().unwrap();
        let listener_fd = listener.as_raw_fd();

        let mut state = Box::new(SchedulerTestState {
            tag: 0,
            listener_fd,
            token: 0,
            completed: std::sync::atomic::AtomicBool::new(false),
        });
        let task = Box::new(KaracParkedTask {
            poll_fn: scheduler_test_poll_fn,
            state: &mut *state as *mut SchedulerTestState as *mut c_void,
        });
        let task_ptr = &*task as *const KaracParkedTask as *mut c_void;
        let token = karac_runtime_event_loop_register_fd(listener_fd, 0, task_ptr);
        assert_ne!(token, 0);
        state.token = token;
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let _initial = unsafe { (task.poll_fn)(task.state, &cancel) };

        let connector = thread::spawn(move || {
            let _stream = std::net::TcpStream::connect(local).unwrap();
            thread::sleep(Duration::from_millis(50));
        });

        let start = Instant::now();
        while !state.completed.load(Ordering::Acquire) {
            if start.elapsed() > Duration::from_secs(2) {
                panic!("dispatcher did not drive task to completion within 2s");
            }
            thread::sleep(Duration::from_millis(10));
        }
        connector.join().unwrap();

        // After-snapshot. Dispatcher should have polled exactly once
        // (the resume after fd readiness), observing Ready. Counters
        // are monotonic, so we assert lower bounds rather than exact
        // equality — a spurious extra poll would be unusual but not
        // an outright bug.
        let mut after = KaracSchedulerStats {
            polls: 0,
            ready_observations: 0,
            err_observations: 0,
            pending_observations: 0,
        };
        let rc = unsafe { karac_runtime_scheduler_stats_snapshot(&mut after) };
        assert_eq!(rc, 0);
        assert!(
            after.polls >= 1,
            "dispatcher should have polled at least once, got {}",
            after.polls
        );
        assert!(
            after.ready_observations >= 1,
            "at least one Ready observation expected, got {}",
            after.ready_observations
        );
        // The total invariant — polls = ready + err + pending.
        assert_eq!(
            after.polls,
            after.ready_observations + after.err_observations + after.pending_observations,
            "polls should equal sum of category observations"
        );

        drop(task);
        drop(state);
    }

    // ── Phase 6 line 17 slice 9e.1 — WebSocket framing tests ────────────
    //
    // Wire-format correctness for `karac_runtime_ws_send_text` /
    // `_recv_text` (RFC 6455 §5.2 + §5.3). Each test sets up a
    // loopback TCP socket pair, drives one side via the FFI under
    // test, validates the other side observes the expected wire
    // bytes (for send) OR observes correctly-unmasked bytes after
    // the FFI's read (for recv).

    #[cfg(unix)]
    fn loopback_pair() -> (i32, std::net::TcpStream) {
        use std::os::unix::io::IntoRawFd;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local_addr");
        let client_handle =
            std::thread::spawn(move || std::net::TcpStream::connect(addr).expect("client connect"));
        let (server_side, _) = listener.accept().expect("accept loopback");
        let client_side = client_handle.join().expect("client thread join");
        let server_fd = server_side.into_raw_fd();
        (server_fd, client_side)
    }

    /// Close a raw fd at test end (reconstruct + drop). Mirrors
    /// the cleanup convention in `karac_runtime_tcp_close`.
    #[cfg(unix)]
    fn close_fd(fd: i32) {
        use std::os::unix::io::FromRawFd;
        let _ = unsafe { std::net::TcpStream::from_raw_fd(fd) };
    }

    #[cfg(unix)]
    #[test]
    fn test_ws_send_text_encodes_short_frame_unmasked() {
        use std::io::Read;
        let (server_fd, mut client) = loopback_pair();
        let payload = b"hello";
        let n = unsafe {
            super::karac_runtime_ws_send_text(server_fd, payload.as_ptr(), payload.len() as i64)
        };
        assert_eq!(n, payload.len() as i64);
        client
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        // Loop-read until we have a full 2-byte header + 5-byte
        // payload. A single read() can return fewer bytes than the
        // kernel-buffered frame; loop until we have what we need
        // (same pattern as the extended-length test).
        let mut buf = [0u8; 16];
        let mut got = 0;
        while got < 7 {
            let m = client.read(&mut buf[got..]).expect("read frame");
            if m == 0 {
                break;
            }
            got += m;
        }
        assert!(got >= 7, "expected ≥7 bytes (header+payload); got {}", got);
        // FIN=1, opcode=0x1 (text), RSV=000.
        assert_eq!(buf[0], 0x81);
        // MASK=0 (server→client), len=5 inline.
        assert_eq!(buf[1], 0x05);
        assert_eq!(&buf[2..7], payload);
        close_fd(server_fd);
    }

    #[cfg(unix)]
    #[test]
    fn test_ws_send_text_uses_extended_2byte_length_for_200_byte_payload() {
        use std::io::Read;
        let (server_fd, mut client) = loopback_pair();
        // 200 bytes lands in the 7+16-bit extended range
        // (126..=65535). Payload contents distinguishable from
        // pure repetition so we can verify byte-for-byte
        // identity, not just length.
        let payload: Vec<u8> = (0..200u8).map(|i| (i & 0x7F) | 0x40).collect();
        let n = unsafe {
            super::karac_runtime_ws_send_text(server_fd, payload.as_ptr(), payload.len() as i64)
        };
        assert_eq!(n, payload.len() as i64);
        client
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut buf = vec![0u8; 256];
        let mut got = 0;
        while got < 4 + payload.len() {
            let m = client.read(&mut buf[got..]).expect("read frame");
            if m == 0 {
                break;
            }
            got += m;
        }
        assert!(got >= 4 + payload.len());
        assert_eq!(buf[0], 0x81);
        // Len-marker 126 signals extended 2-byte length follows.
        assert_eq!(buf[1], 0x7E);
        let ext_len = u16::from_be_bytes([buf[2], buf[3]]);
        assert_eq!(ext_len as usize, payload.len());
        assert_eq!(&buf[4..4 + payload.len()], &payload[..]);
        close_fd(server_fd);
    }

    #[cfg(unix)]
    #[test]
    fn test_ws_recv_text_decodes_masked_client_frame() {
        use std::io::Write;
        let (server_fd, mut client) = loopback_pair();
        let payload = b"client-to-server";
        let mask_key = [0xA5u8, 0x37, 0x91, 0x4C];
        let mut frame = Vec::with_capacity(2 + 4 + payload.len());
        frame.push(0x81);
        frame.push(0x80 | (payload.len() as u8));
        frame.extend_from_slice(&mask_key);
        for (i, b) in payload.iter().enumerate() {
            frame.push(b ^ mask_key[i % 4]);
        }
        client.write_all(&frame).expect("write client frame");
        let mut out = [0u8; 64];
        let n = unsafe {
            super::karac_runtime_ws_recv_text(server_fd, out.as_mut_ptr(), out.len() as i64)
        };
        assert_eq!(n, payload.len() as i64);
        assert_eq!(&out[..n as usize], payload);
        close_fd(server_fd);
    }

    #[cfg(unix)]
    #[test]
    fn test_ws_recv_text_rejects_unmasked_client_frame() {
        use std::io::Write;
        let (server_fd, mut client) = loopback_pair();
        // Client→server frames MUST be masked per RFC 6455 §5.1.
        // An unmasked client frame is a protocol violation.
        let payload = b"unmasked";
        let mut frame = Vec::with_capacity(2 + payload.len());
        frame.push(0x81);
        frame.push(payload.len() as u8); // MASK=0 — invalid for c→s
        frame.extend_from_slice(payload);
        client.write_all(&frame).expect("write client frame");
        let mut out = [0u8; 64];
        let n = unsafe {
            super::karac_runtime_ws_recv_text(server_fd, out.as_mut_ptr(), out.len() as i64)
        };
        assert_eq!(n, -1);
        close_fd(server_fd);
    }

    #[cfg(unix)]
    #[test]
    fn test_ws_recv_text_rejects_binary_opcode_in_v1() {
        use std::io::Write;
        let (server_fd, mut client) = loopback_pair();
        // Slice 9e.1 scope: text frames only. Binary (opcode 0x2),
        // close (0x8), ping (0x9), pong (0xA) — all rejected.
        // Slice 9e.3 lifts this restriction.
        let payload = b"binary";
        let mask_key = [0u8; 4];
        let mut frame = Vec::with_capacity(2 + 4 + payload.len());
        frame.push(0x82); // FIN=1, opcode=0x2 (binary)
        frame.push(0x80 | (payload.len() as u8));
        frame.extend_from_slice(&mask_key);
        frame.extend_from_slice(payload);
        client.write_all(&frame).expect("write client frame");
        let mut out = [0u8; 64];
        let n = unsafe {
            super::karac_runtime_ws_recv_text(server_fd, out.as_mut_ptr(), out.len() as i64)
        };
        assert_eq!(n, -1);
        close_fd(server_fd);
    }

    #[cfg(unix)]
    #[test]
    fn test_ws_round_trip_recv_then_send() {
        use std::io::{Read, Write};
        // Full bidirectional round-trip from the FFI's perspective:
        // FFI recvs a masked client frame, then sends the same
        // payload back as a server frame. Harness verifies the
        // server frame arrives correctly.
        let (server_fd, mut client) = loopback_pair();
        let server_thread = std::thread::spawn(move || {
            let mut in_buf = [0u8; 128];
            let n_recv = unsafe {
                super::karac_runtime_ws_recv_text(
                    server_fd,
                    in_buf.as_mut_ptr(),
                    in_buf.len() as i64,
                )
            };
            assert!(n_recv > 0, "recv_text failed: {}", n_recv);
            let recvd: Vec<u8> = in_buf[..n_recv as usize].to_vec();
            let n_send = unsafe {
                super::karac_runtime_ws_send_text(server_fd, recvd.as_ptr(), recvd.len() as i64)
            };
            assert_eq!(n_send, recvd.len() as i64);
            (recvd, server_fd)
        });

        let payload = b"echo-this-back";
        let mask_key = [0x12u8, 0x34, 0x56, 0x78];
        let mut frame = Vec::with_capacity(2 + 4 + payload.len());
        frame.push(0x81);
        frame.push(0x80 | (payload.len() as u8));
        frame.extend_from_slice(&mask_key);
        for (i, b) in payload.iter().enumerate() {
            frame.push(b ^ mask_key[i % 4]);
        }
        client.write_all(&frame).expect("write client frame");
        client
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut response = [0u8; 64];
        let mut got = 0;
        while got < 2 + payload.len() {
            let m = client.read(&mut response[got..]).expect("read response");
            if m == 0 {
                break;
            }
            got += m;
        }
        assert!(got >= 2 + payload.len());
        assert_eq!(response[0], 0x81); // FIN+text from FFI's send
        assert_eq!(response[1], payload.len() as u8); // MASK=0 + len
        assert_eq!(&response[2..2 + payload.len()], payload);
        let (_, fd) = server_thread.join().expect("server thread");
        close_fd(fd);
    }

    // ── Phase 6 line 17 slice 9e.2 — WebSocket handshake tests ──────────

    #[test]
    fn test_sha1_rfc3174_test_vectors() {
        // RFC 3174 sample digests.
        // "abc" → A9993E364706816ABA3E25717850C26C9CD0D89D
        let digest = super::sha1(b"abc");
        let hex: String = digest.iter().map(|b| format!("{:02x}", b)).collect();
        assert_eq!(hex, "a9993e364706816aba3e25717850c26c9cd0d89d");
        // Empty input → DA39A3EE5E6B4B0D3255BFEF95601890AFD80709
        let digest = super::sha1(b"");
        let hex: String = digest.iter().map(|b| format!("{:02x}", b)).collect();
        assert_eq!(hex, "da39a3ee5e6b4b0d3255bfef95601890afd80709");
    }

    #[test]
    fn test_base64_rfc4648_test_vectors() {
        // RFC 4648 §10 test vectors.
        assert_eq!(super::base64_encode(b""), "");
        assert_eq!(super::base64_encode(b"f"), "Zg==");
        assert_eq!(super::base64_encode(b"fo"), "Zm8=");
        assert_eq!(super::base64_encode(b"foo"), "Zm9v");
        assert_eq!(super::base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(super::base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(super::base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn test_ws_handshake_accept_value_rfc6455_example() {
        // RFC 6455 §1.3 worked example:
        //   key = "dGhlIHNhbXBsZSBub25jZQ=="
        //   accept = "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        let key = b"dGhlIHNhbXBsZSBub25jZQ==";
        let mut input: Vec<u8> = Vec::with_capacity(key.len() + super::WS_HANDSHAKE_GUID.len());
        input.extend_from_slice(key);
        input.extend_from_slice(super::WS_HANDSHAKE_GUID);
        let digest = super::sha1(&input);
        let accept = super::base64_encode(&digest);
        assert_eq!(accept, "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    #[test]
    fn test_extract_sec_websocket_key_case_insensitive() {
        // Header name matching is case-insensitive per RFC 7230 §3.2.
        let req = b"GET / HTTP/1.1\r\nHost: example.com\r\nSEC-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n";
        let key = super::extract_sec_websocket_key(req).expect("key present");
        assert_eq!(key, b"dGhlIHNhbXBsZSBub25jZQ==");
    }

    #[test]
    fn test_extract_sec_websocket_key_missing_returns_none() {
        let req = b"GET / HTTP/1.1\r\nHost: example.com\r\nUpgrade: websocket\r\n\r\n";
        assert!(super::extract_sec_websocket_key(req).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn test_ws_accept_full_handshake_round_trip() {
        use std::io::{Read, Write};
        // Set up a real listener, hand-roll a client that sends an
        // RFC 6455 upgrade request, drive the FFI to accept +
        // upgrade, validate the 101 response on the client side,
        // confirm the returned conn fd is usable for subsequent
        // framed-message exchange.

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        use std::os::unix::io::IntoRawFd;
        let listener_fd = listener.into_raw_fd();

        let client_handle = std::thread::spawn(move || {
            let mut conn = std::net::TcpStream::connect(addr).expect("client connect");
            conn.set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            // Standard browser-shape Upgrade request.
            let req = b"GET /ws HTTP/1.1\r\n\
                        Host: 127.0.0.1\r\n\
                        Upgrade: websocket\r\n\
                        Connection: Upgrade\r\n\
                        Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
                        Sec-WebSocket-Version: 13\r\n\
                        \r\n";
            conn.write_all(req).expect("write request");

            // Read response and find the end-of-headers marker.
            let mut resp = Vec::new();
            let mut chunk = [0u8; 256];
            while !resp.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = conn.read(&mut chunk).expect("read response");
                if n == 0 {
                    break;
                }
                resp.extend_from_slice(&chunk[..n]);
                if resp.len() > 4096 {
                    break;
                }
            }
            (resp, conn)
        });

        // Server side: invoke the FFI under test.
        let conn_fd = super::karac_runtime_ws_accept(listener_fd);
        assert!(
            conn_fd >= 0,
            "ws_accept should return a valid conn fd; got {}",
            conn_fd
        );

        let (resp, _client_conn) = client_handle.join().expect("client thread");
        let resp_str = String::from_utf8_lossy(&resp);
        // 101 status line.
        assert!(
            resp_str.starts_with("HTTP/1.1 101 Switching Protocols\r\n"),
            "expected 101 status; got: {}",
            resp_str
        );
        // Sec-WebSocket-Accept value matches the RFC example.
        assert!(
            resp_str.contains("Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n"),
            "expected the RFC's worked Sec-WebSocket-Accept value; got: {}",
            resp_str
        );
        // Required Upgrade/Connection headers for protocol switch.
        assert!(resp_str.contains("Upgrade: websocket\r\n"));
        assert!(resp_str.contains("Connection: Upgrade\r\n"));

        // Cleanup.
        close_fd(conn_fd);
        close_fd(listener_fd);
    }

    #[cfg(unix)]
    #[test]
    fn test_ws_accept_returns_minus_one_when_key_missing() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        use std::os::unix::io::IntoRawFd;
        let listener_fd = listener.into_raw_fd();

        let client_handle = std::thread::spawn(move || {
            let mut conn = std::net::TcpStream::connect(addr).expect("client connect");
            conn.set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            // Request without Sec-WebSocket-Key.
            let req = b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n";
            conn.write_all(req).expect("write request");
            let mut resp = Vec::new();
            let mut chunk = [0u8; 256];
            // Read until the connection closes (the server sends a
            // 400 then closes) or we accumulate something useful.
            for _ in 0..10 {
                match conn.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => resp.extend_from_slice(&chunk[..n]),
                    Err(_) => break,
                }
                if resp.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            resp
        });

        let conn_fd = super::karac_runtime_ws_accept(listener_fd);
        assert_eq!(
            conn_fd, -1,
            "ws_accept should return -1 when Sec-WebSocket-Key is missing"
        );

        let resp = client_handle.join().expect("client thread");
        let resp_str = String::from_utf8_lossy(&resp);
        // The FFI sends a best-effort 400 before returning -1.
        assert!(
            resp_str.starts_with("HTTP/1.1 400 Bad Request\r\n"),
            "expected 400 response for missing key; got: {}",
            resp_str
        );

        close_fd(listener_fd);
    }

    // ── Phase 6 line 17 slice 9e.3 — control frames + binary ────────────

    /// Encode a masked client→server frame with the given opcode.
    /// Used only inside slice-9e.3 tests to drive control frames
    /// at the FFI's recv side. Mirror of the codegen-side encode
    /// (`ws_write_unmasked_frame`) but adds the 4-byte mask key
    /// + payload-mask step per RFC 6455 §5.3.
    fn encode_masked_client_frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
        debug_assert!(opcode <= 0x0F);
        let len = payload.len();
        let mut frame: Vec<u8> = Vec::with_capacity(2 + 4 + len);
        frame.push(0x80 | opcode); // FIN=1
                                   // Inline 7-bit length is sufficient for control-frame
                                   // tests (always < 126 per RFC 6455 §5.5) and short
                                   // binary tests. Extended lengths use the same encoder as
                                   // `ws_write_unmasked_frame` if needed.
        if len < 126 {
            frame.push(0x80 | (len as u8));
        } else if len < 65536 {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&(len as u16).to_be_bytes());
        } else {
            frame.push(0x80 | 127);
            frame.extend_from_slice(&(len as u64).to_be_bytes());
        }
        let mask_key = [0xA5u8, 0x37, 0x91, 0x4C];
        frame.extend_from_slice(&mask_key);
        for (i, b) in payload.iter().enumerate() {
            frame.push(b ^ mask_key[i % 4]);
        }
        frame
    }

    #[cfg(unix)]
    #[test]
    fn test_ws_recv_text_auto_responds_to_ping() {
        use std::io::{Read, Write};
        let (server_fd, mut client) = loopback_pair();

        // Client sends: ping("hi") then text("hello"). The FFI
        // should auto-reply with pong("hi"), then return the
        // unmasked "hello" payload.
        let ping = encode_masked_client_frame(0x9, b"hi");
        let text = encode_masked_client_frame(0x1, b"hello");
        let mut frames = Vec::new();
        frames.extend_from_slice(&ping);
        frames.extend_from_slice(&text);
        client.write_all(&frames).expect("write frames");

        let mut out = [0u8; 32];
        let n = unsafe {
            super::karac_runtime_ws_recv_text(server_fd, out.as_mut_ptr(), out.len() as i64)
        };
        assert_eq!(n, 5);
        assert_eq!(&out[..5], b"hello");

        // Validate the auto-pong arrived on the client side.
        // Server→client pong: 0x8A 0x02 'h' 'i'.
        client
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut got = 0;
        let mut pong_buf = [0u8; 16];
        while got < 4 {
            let m = client.read(&mut pong_buf[got..]).expect("read pong");
            if m == 0 {
                break;
            }
            got += m;
        }
        assert!(got >= 4);
        assert_eq!(pong_buf[0], 0x8A, "pong opcode FIN=1");
        assert_eq!(pong_buf[1], 0x02, "pong length 2, MASK=0");
        assert_eq!(&pong_buf[2..4], b"hi");

        close_fd(server_fd);
    }

    #[cfg(unix)]
    #[test]
    fn test_ws_recv_text_discards_pong_frame() {
        use std::io::Write;
        let (server_fd, mut client) = loopback_pair();

        // Pong then text. The FFI should discard the pong and
        // return the text payload.
        let pong = encode_masked_client_frame(0xA, b"pongdata");
        let text = encode_masked_client_frame(0x1, b"x");
        let mut frames = Vec::new();
        frames.extend_from_slice(&pong);
        frames.extend_from_slice(&text);
        client.write_all(&frames).expect("write frames");

        let mut out = [0u8; 16];
        let n = unsafe {
            super::karac_runtime_ws_recv_text(server_fd, out.as_mut_ptr(), out.len() as i64)
        };
        assert_eq!(n, 1);
        assert_eq!(out[0], b'x');

        close_fd(server_fd);
    }

    #[cfg(unix)]
    #[test]
    fn test_ws_recv_text_close_returns_zero_and_replies() {
        use std::io::{Read, Write};
        let (server_fd, mut client) = loopback_pair();

        let close = encode_masked_client_frame(0x8, &[]);
        client.write_all(&close).expect("write close");

        let mut out = [0u8; 16];
        let n = unsafe {
            super::karac_runtime_ws_recv_text(server_fd, out.as_mut_ptr(), out.len() as i64)
        };
        assert_eq!(n, 0, "close frame should surface as graceful EOF (0)");

        // Server→client close response: 0x88 0x00.
        client
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut buf = [0u8; 4];
        let mut got = 0;
        while got < 2 {
            let m = client.read(&mut buf[got..]).expect("read close response");
            if m == 0 {
                break;
            }
            got += m;
        }
        assert_eq!(buf[0], 0x88);
        assert_eq!(buf[1], 0x00);

        close_fd(server_fd);
    }

    #[cfg(unix)]
    #[test]
    fn test_ws_recv_text_rejects_oversize_control_frame() {
        use std::io::Write;
        let (server_fd, mut client) = loopback_pair();

        // Control frame > 125 bytes — protocol violation per
        // RFC 6455 §5.5.
        let oversize: Vec<u8> = vec![0u8; 200];
        let frame = encode_masked_client_frame(0x9, &oversize);
        client.write_all(&frame).expect("write frame");

        let mut out = [0u8; 256];
        let n = unsafe {
            super::karac_runtime_ws_recv_text(server_fd, out.as_mut_ptr(), out.len() as i64)
        };
        assert_eq!(n, -1);

        close_fd(server_fd);
    }

    #[cfg(unix)]
    #[test]
    fn test_ws_send_binary_encodes_opcode_2() {
        use std::io::Read;
        let (server_fd, mut client) = loopback_pair();
        let payload = b"\x00\x01\x02\x03\xFF";
        let n = unsafe {
            super::karac_runtime_ws_send_binary(server_fd, payload.as_ptr(), payload.len() as i64)
        };
        assert_eq!(n, payload.len() as i64);
        client
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut buf = [0u8; 16];
        let mut got = 0;
        while got < 2 + payload.len() {
            let m = client.read(&mut buf[got..]).expect("read frame");
            if m == 0 {
                break;
            }
            got += m;
        }
        assert_eq!(buf[0], 0x82, "FIN=1 + opcode=0x2 (binary)");
        assert_eq!(buf[1], payload.len() as u8, "MASK=0 + inline len");
        assert_eq!(&buf[2..2 + payload.len()], payload);
        close_fd(server_fd);
    }

    #[cfg(unix)]
    #[test]
    fn test_ws_recv_binary_decodes_masked_binary_frame() {
        use std::io::Write;
        let (server_fd, mut client) = loopback_pair();
        let payload = b"\xDE\xAD\xBE\xEF";
        let frame = encode_masked_client_frame(0x2, payload);
        client.write_all(&frame).expect("write frame");

        let mut out = [0u8; 16];
        let n = unsafe {
            super::karac_runtime_ws_recv_binary(server_fd, out.as_mut_ptr(), out.len() as i64)
        };
        assert_eq!(n, payload.len() as i64);
        assert_eq!(&out[..n as usize], payload);
        close_fd(server_fd);
    }

    #[cfg(unix)]
    #[test]
    fn test_ws_recv_binary_rejects_text_frame() {
        // Symmetric to slice 9e.1's `recv_text_rejects_binary` —
        // a text frame on the binary recv path returns -1
        // (mismatched opcode → protocol violation for this method).
        use std::io::Write;
        let (server_fd, mut client) = loopback_pair();
        let frame = encode_masked_client_frame(0x1, b"text");
        client.write_all(&frame).expect("write frame");
        let mut out = [0u8; 16];
        let n = unsafe {
            super::karac_runtime_ws_recv_binary(server_fd, out.as_mut_ptr(), out.len() as i64)
        };
        assert_eq!(n, -1);
        close_fd(server_fd);
    }

    #[cfg(unix)]
    #[test]
    fn test_ws_recv_text_rejects_orphan_continuation_frame() {
        // Slice 9e.4 lifted the "fragmented frames return -1"
        // gate from slice 9e.3, but a continuation frame (opcode
        // 0x0) is only legal mid-fragment. A continuation
        // arriving outside an in-progress fragmented message is
        // a protocol violation per RFC 6455 §5.4.
        use std::io::Write;
        let (server_fd, mut client) = loopback_pair();
        // FIN=1, opcode=0x0 (continuation) without a preceding
        // FIN=0 data frame — illegal.
        let payload = b"abc";
        let frame = encode_masked_client_frame(0x0, payload);
        client.write_all(&frame).expect("write frame");
        let mut out = [0u8; 16];
        let n = unsafe {
            super::karac_runtime_ws_recv_text(server_fd, out.as_mut_ptr(), out.len() as i64)
        };
        assert_eq!(n, -1);
        close_fd(server_fd);
    }

    /// Encode a masked client→server frame with explicit FIN bit.
    /// Slice-9e.4 fragmentation tests need to control FIN
    /// separately from opcode.
    fn encode_masked_client_frame_with_fin(opcode: u8, payload: &[u8], fin: bool) -> Vec<u8> {
        debug_assert!(opcode <= 0x0F);
        let len = payload.len();
        let mut frame: Vec<u8> = Vec::with_capacity(2 + 4 + len);
        let fin_bit = if fin { 0x80 } else { 0x00 };
        frame.push(fin_bit | opcode);
        if len < 126 {
            frame.push(0x80 | (len as u8));
        } else if len < 65536 {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&(len as u16).to_be_bytes());
        } else {
            frame.push(0x80 | 127);
            frame.extend_from_slice(&(len as u64).to_be_bytes());
        }
        let mask_key = [0xA5u8, 0x37, 0x91, 0x4C];
        frame.extend_from_slice(&mask_key);
        for (i, b) in payload.iter().enumerate() {
            frame.push(b ^ mask_key[i % 4]);
        }
        frame
    }

    #[cfg(unix)]
    #[test]
    fn test_ws_recv_text_reassembles_multi_fragment_message() {
        // RFC 6455 §5.4: a text message split into three frames
        // — first(FIN=0, op=0x1), continuation(FIN=0, op=0x0),
        // final(FIN=1, op=0x0). recv_text should reassemble into
        // a single payload.
        use std::io::Write;
        let (server_fd, mut client) = loopback_pair();
        let f1 = encode_masked_client_frame_with_fin(0x1, b"Hel", false);
        let f2 = encode_masked_client_frame_with_fin(0x0, b"lo, ", false);
        let f3 = encode_masked_client_frame_with_fin(0x0, b"world!", true);
        let mut all = Vec::new();
        all.extend_from_slice(&f1);
        all.extend_from_slice(&f2);
        all.extend_from_slice(&f3);
        client.write_all(&all).expect("write fragments");

        let mut out = [0u8; 64];
        let n = unsafe {
            super::karac_runtime_ws_recv_text(server_fd, out.as_mut_ptr(), out.len() as i64)
        };
        assert_eq!(n, 13, "reassembled len = 3 + 4 + 6");
        assert_eq!(&out[..13], b"Hello, world!");
        close_fd(server_fd);
    }

    #[cfg(unix)]
    #[test]
    fn test_ws_recv_text_fragmentation_with_interleaved_ping() {
        // RFC 6455 §5.4 allows control frames to be interleaved
        // between data fragments. The FFI should auto-respond to
        // the ping without disrupting the in-progress fragment
        // reassembly.
        use std::io::{Read, Write};
        let (server_fd, mut client) = loopback_pair();
        let f1 = encode_masked_client_frame_with_fin(0x1, b"foo", false);
        let ping = encode_masked_client_frame_with_fin(0x9, b"hi", true);
        let f2 = encode_masked_client_frame_with_fin(0x0, b"bar", true);
        let mut all = Vec::new();
        all.extend_from_slice(&f1);
        all.extend_from_slice(&ping);
        all.extend_from_slice(&f2);
        client.write_all(&all).expect("write frames");

        let mut out = [0u8; 16];
        let n = unsafe {
            super::karac_runtime_ws_recv_text(server_fd, out.as_mut_ptr(), out.len() as i64)
        };
        assert_eq!(n, 6, "reassembled len = 3 + 3");
        assert_eq!(&out[..6], b"foobar");

        // The interleaved ping should have produced a pong
        // response on the client side.
        client
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut pong_buf = [0u8; 8];
        let mut got = 0;
        while got < 4 {
            let m = client.read(&mut pong_buf[got..]).expect("read pong");
            if m == 0 {
                break;
            }
            got += m;
        }
        assert_eq!(pong_buf[0], 0x8A);
        assert_eq!(pong_buf[1], 0x02);
        assert_eq!(&pong_buf[2..4], b"hi");
        close_fd(server_fd);
    }

    #[cfg(unix)]
    #[test]
    fn test_ws_recv_text_fragmentation_overflows_buffer_returns_minus_one() {
        // Reassembled total exceeds caller's buffer → -1.
        use std::io::Write;
        let (server_fd, mut client) = loopback_pair();
        let f1 = encode_masked_client_frame_with_fin(0x1, b"first", false);
        let f2 = encode_masked_client_frame_with_fin(0x0, b"second", true);
        let mut all = Vec::new();
        all.extend_from_slice(&f1);
        all.extend_from_slice(&f2);
        client.write_all(&all).expect("write fragments");

        // 8-byte buffer can't hold "firstsecond" (11 bytes).
        let mut out = [0u8; 8];
        let n = unsafe {
            super::karac_runtime_ws_recv_text(server_fd, out.as_mut_ptr(), out.len() as i64)
        };
        assert_eq!(n, -1);
        close_fd(server_fd);
    }

    #[cfg(unix)]
    #[test]
    fn test_ws_recv_text_rejects_mid_fragment_non_continuation() {
        // After a FIN=0 data start, the next data frame MUST be
        // a continuation (opcode 0x0). A new text frame (opcode
        // 0x1) interleaved during a fragmented message is a
        // protocol violation.
        use std::io::Write;
        let (server_fd, mut client) = loopback_pair();
        let f1 = encode_masked_client_frame_with_fin(0x1, b"abc", false);
        // A new text frame instead of a continuation — illegal.
        let f2 = encode_masked_client_frame_with_fin(0x1, b"def", true);
        let mut all = Vec::new();
        all.extend_from_slice(&f1);
        all.extend_from_slice(&f2);
        client.write_all(&all).expect("write frames");
        let mut out = [0u8; 32];
        let n = unsafe {
            super::karac_runtime_ws_recv_text(server_fd, out.as_mut_ptr(), out.len() as i64)
        };
        assert_eq!(n, -1);
        close_fd(server_fd);
    }

    #[cfg(unix)]
    #[test]
    fn test_ws_recv_binary_reassembles_fragmented_message() {
        // Same fragmentation behaviour applies to recv_binary
        // (the shared `ws_recv_data_frame` helper switches on
        // `accept_opcode` for the start frame only).
        use std::io::Write;
        let (server_fd, mut client) = loopback_pair();
        let f1 = encode_masked_client_frame_with_fin(0x2, &[0xDE, 0xAD], false);
        let f2 = encode_masked_client_frame_with_fin(0x0, &[0xBE, 0xEF], true);
        let mut all = Vec::new();
        all.extend_from_slice(&f1);
        all.extend_from_slice(&f2);
        client.write_all(&all).expect("write fragments");

        let mut out = [0u8; 16];
        let n = unsafe {
            super::karac_runtime_ws_recv_binary(server_fd, out.as_mut_ptr(), out.len() as i64)
        };
        assert_eq!(n, 4);
        assert_eq!(&out[..4], &[0xDE, 0xAD, 0xBE, 0xEF]);
        close_fd(server_fd);
    }

    #[cfg(unix)]
    #[test]
    fn test_ws_send_text_masked_encodes_mask1_frame() {
        // Send a masked client-side text frame; client side
        // (acting as the WebSocket peer here) validates the
        // wire format and unmasks the payload.
        use std::io::Read;
        let (server_fd, mut client) = loopback_pair();
        let payload = b"client-message";
        let n = unsafe {
            super::karac_runtime_ws_send_text_masked(
                server_fd,
                payload.as_ptr(),
                payload.len() as i64,
            )
        };
        assert_eq!(n, payload.len() as i64);

        client
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let total = 2 + 4 + payload.len(); // header + mask + payload
        let mut buf = vec![0u8; total + 4];
        let mut got = 0;
        while got < total {
            let m = client.read(&mut buf[got..]).expect("read frame");
            if m == 0 {
                break;
            }
            got += m;
        }
        assert!(got >= total, "expected ≥{} bytes; got {}", total, got);
        assert_eq!(buf[0], 0x81, "FIN=1 + opcode=0x1 (text)");
        assert_eq!(buf[1], 0x80 | (payload.len() as u8), "MASK=1 + inline len");
        let mask_key = [buf[2], buf[3], buf[4], buf[5]];
        let mut unmasked: Vec<u8> = buf[6..6 + payload.len()].to_vec();
        for (i, b) in unmasked.iter_mut().enumerate() {
            *b ^= mask_key[i % 4];
        }
        assert_eq!(&unmasked[..], payload);
        close_fd(server_fd);
    }

    #[cfg(unix)]
    #[test]
    fn test_ws_send_binary_masked_uses_opcode_2() {
        use std::io::Read;
        let (server_fd, mut client) = loopback_pair();
        let payload = b"\x01\x02\x03";
        let n = unsafe {
            super::karac_runtime_ws_send_binary_masked(
                server_fd,
                payload.as_ptr(),
                payload.len() as i64,
            )
        };
        assert_eq!(n, payload.len() as i64);
        client
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut buf = [0u8; 16];
        let want = 2 + 4 + payload.len();
        let mut got = 0;
        while got < want {
            let m = client.read(&mut buf[got..]).expect("read frame");
            if m == 0 {
                break;
            }
            got += m;
        }
        assert_eq!(buf[0], 0x82, "FIN=1 + opcode=0x2 (binary)");
        assert_eq!(buf[1], 0x80 | (payload.len() as u8), "MASK=1 + inline len");
        // Validate via unmask round-trip.
        let mask_key = [buf[2], buf[3], buf[4], buf[5]];
        let mut unmasked: Vec<u8> = buf[6..6 + payload.len()].to_vec();
        for (i, b) in unmasked.iter_mut().enumerate() {
            *b ^= mask_key[i % 4];
        }
        assert_eq!(&unmasked[..], payload);
        close_fd(server_fd);
    }

    #[test]
    fn test_ws_generate_mask_key_is_nonzero_and_varies() {
        // Defensive: the mask key generator should produce
        // non-zero values most of the time, and successive calls
        // should differ. Don't strictly require non-zero on
        // every call (4 bytes of zero is technically valid,
        // just extremely unlikely from /dev/urandom or LCG); use
        // a small sample to assert variance.
        let mut all_same_as_first = true;
        let first = super::ws_generate_mask_key();
        for _ in 0..8 {
            let next = super::ws_generate_mask_key();
            if next != first {
                all_same_as_first = false;
                break;
            }
        }
        assert!(
            !all_same_as_first,
            "mask key generator returned the same 4 bytes 9 times in a row — \
             not random; got {:?}",
            first
        );
    }

    // ── Per-park completion slot (async-sched slice 2/3) ───────────────

    #[test]
    fn park_slot_signal_then_wait_does_not_block() {
        // Signal-before-wait must not be lost: `done` is set under the
        // mutex, so the subsequent wait observes it and returns at once.
        let slot = karac_runtime_park_slot_new();
        unsafe {
            karac_runtime_park_slot_signal(slot);
        }
        let start = Instant::now();
        unsafe {
            karac_runtime_park_slot_wait(slot);
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_millis(200),
            "wait after a prior signal should return promptly, took {elapsed:?}"
        );
        unsafe {
            karac_runtime_park_slot_free(slot);
        }
    }

    #[test]
    fn park_slot_wait_unblocks_on_cross_thread_signal() {
        // The real hand-off shape: the caller blocks in `wait`; another
        // thread (standing in for the dispatcher) signals; the caller
        // resumes. Frees only after `wait` returns, mirroring the codegen
        // lifetime contract.
        let slot = karac_runtime_park_slot_new();
        // Move the raw pointer across the thread boundary via usize so it
        // is `Send` — the runtime owns the lifetime per the FFI contract.
        let slot_addr = slot as usize;
        let signaler = thread::spawn(move || {
            thread::sleep(Duration::from_millis(30));
            unsafe {
                karac_runtime_park_slot_signal(slot_addr as *mut KaracParkSlot);
            }
        });
        let start = Instant::now();
        unsafe {
            karac_runtime_park_slot_wait(slot);
        }
        let elapsed = start.elapsed();
        signaler.join().unwrap();
        assert!(
            elapsed >= Duration::from_millis(20),
            "wait should block until the signal (~30ms), only blocked {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "wait should unblock shortly after the signal, took {elapsed:?}"
        );
        unsafe {
            karac_runtime_park_slot_free(slot);
        }
    }

    #[test]
    fn park_slot_free_is_null_safe() {
        unsafe {
            karac_runtime_park_slot_free(std::ptr::null_mut());
            karac_runtime_park_slot_wait(std::ptr::null_mut());
            karac_runtime_park_slot_signal(std::ptr::null_mut());
        }
    }
}
