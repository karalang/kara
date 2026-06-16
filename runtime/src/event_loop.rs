//! Network event loop. Cross-platform abstraction over the OS-level
//! fd-readiness facilities — `epoll` on Linux, `kqueue` on macOS / BSD,
//! `IOCP` on Windows — via the `mio` crate.
//!
//! See `docs/design.md § Network Event Loop and State-Machine Transform`
//! and `docs/implementation_checklist/phase-6-runtime.md` line 15.
//!
//! ## v1 architectural commitments (per phase-6-runtime.md line 15)
//!
//! - **Sharded event loops.** v1 ran exactly one loop; Stage B2 realizes
//!   the "M2 / M3 may shard across multiple loops to reach the 1M+
//!   idle-connection target" commitment — the process now runs
//!   `resolve_shard_count()` independent `EventLoop`s (see `EVENT_LOOPS`),
//!   one poller thread each, with connections routed by raw fd. Each loop
//!   is still individually `Sync` — shared via `Arc<EventLoop>` from any
//!   thread — with two interior Mutexes that split the polling and
//!   registration code paths so a long-blocking `run_once` (held by
//!   that shard's background poller thread, slice 3) does not block
//!   concurrent register / deregister calls on the same shard.
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
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, VecDeque};
use std::ffi::c_void;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};
// `AtomicU64` backs `HandshakeStats`, which is part of the TLS handshake
// pool — gated behind the `tls` feature so the lean archive doesn't carry it.
#[cfg(all(unix, feature = "tls"))]
use std::sync::atomic::AtomicU64;
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
    /// Optional deadline. For a **timer** registration (`register_timer`,
    /// A2a-1 — the `suspends` async-sleep substrate) this is `Some(_)` and is
    /// the authoritative expiry instant: the `timers` min-heap drives the
    /// poll-timeout and the expiry scan reads this field to confirm a popped
    /// heap entry's registration is genuinely due. For an **fd** registration
    /// it is `None` (a future fd-with-timeout bound could set it).
    deadline: Option<Instant>,
    /// Per-task cancel flag (slice 5c). Bound at register time for a
    /// `spawn_coro`-driven coroutine (the handler's `cancel: AtomicBool`,
    /// read off the bound slot via `karac_runtime_park_slot_cancel_ptr`);
    /// null for the inline / non-spawn drive and for the degenerate
    /// (non-coroutine) state-machine path. The dispatcher passes it to
    /// `poll_fn` so a coroutine observes *its own* cancellation, and the
    /// cancel-sweep reads it here (no parked-record deref) to find idle
    /// parked-but-never-ready coroutines whose flag has been flipped.
    /// Lives on the registration, not in the parked record, so the parked
    /// record ABI is unchanged across the coroutine and degenerate paths.
    cancel: *const AtomicBool,
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

/// One entry in the [`EventLoopFds::timers`] min-heap (A2a-1, the
/// async-sleep / `suspends` timer substrate).
///
/// Carries only `Copy`, `Send` scalars — the deadline and the raw token —
/// so the heap itself needs no `unsafe Send`. The opaque `parked` pointer
/// lives in the matching [`FdState`] in `by_token`, keyed by this token; the
/// expiry scan re-reads `FdState.deadline` as the authoritative due-time, so
/// a stale heap entry left by a re-armed or cancelled timer is filtered out
/// rather than firing the wrong task. Ordered by `(deadline, token)` and
/// stored under [`Reverse`] so [`BinaryHeap`] yields the *earliest* deadline.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct TimerEntry {
    deadline: Instant,
    token_raw: usize,
}

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

// SAFETY: same justification as `FdState` above — `parked` is an opaque
// pointer the event loop never derefs; it is only carried back to the
// scheduler that registered it. `Send` is required because a shutdown
// drain stashes `Wakeup`s in the `pending` Mutex inside the
// thread-shared `Arc<EventLoop>` for redelivery by the next `run_once`.
unsafe impl Send for Wakeup {}

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
    /// Real-fd wakeups consumed by a shutdown drain
    /// ([`Self::drain_for_shutdown`]), awaiting redelivery. mio
    /// registrations are edge-triggered, so whichever poll observes a
    /// readiness event consumes it — the kernel will not re-report the
    /// edge. The shutdown drains exist to eat a pending edge-armed
    /// *waker* event, but any real fd that became ready in the
    /// shutdown window is swept up by the same non-blocking poll;
    /// discarding it would wedge the parked task behind a
    /// never-again-reported edge. Stashing here lets the next
    /// `run_once` (a post-shutdown direct `poll`, or a restarted
    /// poller/dispatcher) deliver it as if the drain never happened.
    /// Empty in steady state — only a shutdown drain pushes.
    pending: Mutex<Vec<Wakeup>>,
}

struct EventLoopPoll {
    poll: Poll,
    events: Events,
}

struct EventLoopFds {
    by_token: HashMap<Token, FdState>,
    /// Min-heap of pending timer deadlines (A2a-1). A `register_timer`
    /// registration has no fd and no mio arming — it lives only here (for the
    /// poll-timeout cap + expiry scan) and in `by_token` (for `parked` +
    /// `take_registration`). Stale entries (cancelled or superseded timers)
    /// are filtered lazily at pop time against `by_token`/`FdState.deadline`,
    /// so cancellation never has to rebuild the heap.
    timers: BinaryHeap<Reverse<TimerEntry>>,
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
                timers: BinaryHeap::new(),
                next_token: 1,
            }),
            pending: Mutex::new(Vec::new()),
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
        self.register_with_cancel(source, direction, deadline, parked, std::ptr::null())
    }

    /// `register` plus a per-task `cancel` flag (slice 5c). The coroutine
    /// park-suspend passes the handler's cancel flag (read off its bound
    /// slot); every other caller registers with a null flag via [`register`]
    /// and falls back to the dispatcher's never-cancelled flag.
    pub fn register_with_cancel<S: mio::event::Source + ?Sized>(
        &self,
        source: &mut S,
        direction: IoDirection,
        deadline: Option<Instant>,
        parked: *mut c_void,
        cancel: *const AtomicBool,
    ) -> io::Result<RegistrationToken> {
        // Allocate the token and publish the map entry under the lock, then
        // perform the `epoll_ctl` ADD *without* holding the fds lock. The
        // map entry must exist before the fd is armed (so a readiness wakeup
        // resolves via `take_registration`), but the syscall itself does not
        // need the lock: `mio::Registry` is `Sync` and the kernel serializes
        // concurrent `epoll_ctl` efficiently. Holding the lock across the
        // syscall serialized every connection's park/unpark on one mutex —
        // the measured cap on parallel dispatch under burst load (a thread
        // sweep plateaued at ~4 dispatchers; releasing the lock here is what
        // lets dispatch scale past it). The fd is not armed until `register`
        // returns, so no wakeup can observe the published-but-unarmed entry.
        let token = {
            let mut fds = self.fds.lock().unwrap_or_else(|p| p.into_inner());
            let token = Token(fds.next_token);
            fds.next_token = fds
                .next_token
                .checked_add(1)
                .expect("event loop token exhaustion (usize wrap)");
            fds.by_token.insert(
                token,
                FdState {
                    parked,
                    direction,
                    deadline,
                    cancel,
                },
            );
            token
        };
        if let Err(e) = self
            .registry
            .register(source, token, direction.to_interest())
        {
            // The fd was never armed — roll back the speculative entry.
            let mut fds = self.fds.lock().unwrap_or_else(|p| p.into_inner());
            fds.by_token.remove(&token);
            return Err(e);
        }
        Ok(RegistrationToken(token.0))
    }

    /// Register a one-shot timer that fires at `deadline`, with no fd.
    ///
    /// The async-sleep / `suspends` substrate (A2a-1). Unlike `register`
    /// there is no `mio::Source` and no `epoll_ctl`: the registration lives in
    /// the `timers` min-heap (driving the poll-timeout cap + expiry scan in
    /// `run_once`) and in `by_token` (so `take_registration` claims it
    /// uniformly with an fd wakeup and the dispatcher resume path is
    /// identical). On or after `deadline`, the next `run_once` surfaces a
    /// `Wakeup` carrying `parked`; `direction` is reported as `Read` (a timer
    /// has no I/O direction and the resumed task does not consult it). Cancel
    /// before firing via `take_registration` (drops the `by_token` entry; the
    /// stale heap entry is filtered lazily at pop). `cancel` is the per-task
    /// cancel flag (null if unbound), stored like any other registration.
    ///
    /// Wakes the loop after publishing so a poller already blocked in
    /// `run_once(None)` re-derives its timeout and observes the new deadline
    /// (an fd ADD is seen by an in-flight `poll`, but a timeout change is not).
    /// Acquires only the `fds` Mutex briefly.
    pub fn register_timer(
        &self,
        deadline: Instant,
        parked: *mut c_void,
        cancel: *const AtomicBool,
    ) -> RegistrationToken {
        let token = {
            let mut fds = self.fds.lock().unwrap_or_else(|p| p.into_inner());
            let token = Token(fds.next_token);
            fds.next_token = fds
                .next_token
                .checked_add(1)
                .expect("event loop token exhaustion (usize wrap)");
            fds.by_token.insert(
                token,
                FdState {
                    parked,
                    direction: IoDirection::Read,
                    deadline: Some(deadline),
                    cancel,
                },
            );
            fds.timers.push(Reverse(TimerEntry {
                deadline,
                token_raw: token.0,
            }));
            token
        };
        // Force a blocked poller to recompute its timeout against the new
        // deadline. Best-effort: a failed wake only delays this timer until
        // the next natural wakeup, never drops it.
        let _ = self.waker.wake();
        RegistrationToken(token.0)
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
        // Remove the map entry under the lock, then `epoll_ctl` DEL without
        // the lock held (same rationale as `register`). A stale wakeup that
        // raced the removal resolves to `None` in `take_registration` (and
        // `run_once` skips tokens absent from the map), so dropping the
        // entry before the syscall is safe and keeps the lock off the
        // syscall path.
        {
            let mut fds = self.fds.lock().unwrap_or_else(|p| p.into_inner());
            fds.by_token.remove(&Token(token.0));
        }
        self.registry.deregister(source)
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

    /// Like [`take_registration`], but also returns the registration's bound
    /// per-task `cancel` flag (null if unbound). The dispatcher and the
    /// cancel-sweep use this so they can hand `poll_fn` the task's own flag.
    fn take_registration_with_cancel(
        &self,
        token: RegistrationToken,
    ) -> Option<(*mut c_void, *const AtomicBool)> {
        let mut fds = self.fds.lock().unwrap_or_else(|p| p.into_inner());
        fds.by_token
            .remove(&Token(token.0))
            .map(|state| (state.parked, state.cancel))
    }

    /// Cancel-sweep phase A — snapshot the tokens of every registration whose
    /// bound `cancel` flag is set. Reads the flag straight off `FdState` (no
    /// parked-record deref), and returns *tokens only* (a `Copy` value that
    /// cannot dangle): phase B re-claims each via [`take_registration_with_cancel`],
    /// the atomic claim that dedups against a concurrent normal fd-wakeup /
    /// completion. The `cancel` flag itself lives on the owning handle, which
    /// outlives the registration, so loading it under the `fds` lock is sound.
    fn collect_cancelled(&self) -> Vec<RegistrationToken> {
        let fds = self.fds.lock().unwrap_or_else(|p| p.into_inner());
        let mut out = Vec::new();
        for (tok, state) in fds.by_token.iter() {
            let cancel = state.cancel;
            // SAFETY: a non-null `cancel` points at the owning handle's flag,
            // live for the registration's lifetime (the handle is freed only
            // after the coroutine tears down, which removes this entry first).
            if !cancel.is_null() && unsafe { (*cancel).load(Ordering::Acquire) } {
                out.push(RegistrationToken(tok.0));
            }
        }
        out
    }

    /// Cancel-sweep phase B — for each snapshotted token, claim it via the
    /// one-shot [`take_registration_with_cancel`] and (only on a win) invoke
    /// its `poll_fn` with the task's own cancel flag. The flag is set (that is
    /// why the token was snapshotted), so the resume shim's pre-resume
    /// cancel-check runs `coro.destroy` → the per-park destroy edge (deregister
    /// fd + drop live heap locals + signal the completion slot) → returns
    /// `Ready`, waking the joiner.
    ///
    /// Runs with **no lock held**: `poll_fn`'s destroy edge calls `deregister`
    /// (which wants the `fds` lock) and `park_slot_signal` (which may unblock a
    /// joiner) — holding `fds` across it would self-deadlock the non-reentrant
    /// mutex. The collect-then-invoke split is exactly what avoids that.
    ///
    /// Race safety: a token whose task completed normally between phase A and
    /// here is already gone from `by_token`, so the claim returns `None` and we
    /// skip it — we never deref a stale snapshot pointer (only the `Copy` token
    /// crosses the lock release). The claim itself does not deref `parked`, so
    /// passing a token for an already-freed record is safe.
    fn sweep_cancelled(&self, disp: &SchedulerDispatcher) {
        for token in self.collect_cancelled() {
            let (parked, cancel) = match self.take_registration_with_cancel(token) {
                Some((p, c)) if !p.is_null() => (p, c),
                _ => continue,
            };
            // SAFETY: we won the one-shot claim, so no normal-dispatch path
            // has signalled/freed this record; it is live for this call.
            let task = unsafe { &*(parked as *const KaracParkedTask) };
            let cancel: &AtomicBool = if cancel.is_null() {
                &disp.cancel
            } else {
                // SAFETY: non-null cancel points at the owning handle's flag,
                // live until the joiner frees the handle (after teardown).
                unsafe { &*cancel }
            };
            let _ = unsafe { (task.poll_fn)(task.state, cancel) };
            disp.polls.fetch_add(1, Ordering::Relaxed);
            disp.ready_observations.fetch_add(1, Ordering::Relaxed);
        }
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
    /// into [`Wakeup`]s. Lock order is consistently poll → fds. The
    /// `pending` Mutex is taken alone, before the poll lock — never
    /// nested.
    pub fn run_once(&self, max_wait: Option<Duration>) -> io::Result<Vec<Wakeup>> {
        // Redeliver wakeups a shutdown drain stashed (see the `pending`
        // field doc). When the stash is non-empty, don't block — sweep
        // fresh events non-blocking and return stash + fresh together,
        // so a blocking caller isn't parked while deliverable readiness
        // sits in hand.
        let mut stashed: Vec<Wakeup> = {
            let mut pending = self.pending.lock().unwrap_or_else(|p| p.into_inner());
            std::mem::take(&mut *pending)
        };
        let max_wait = if stashed.is_empty() {
            max_wait
        } else {
            Some(Duration::ZERO)
        };
        // A2a-1: cap the poll timeout by the earliest pending timer so a task
        // parked on a deadline with no fd activity still wakes on time. An
        // already-due timer collapses the wait to zero (fire on this pass). A
        // stale earliest entry (cancelled timer not yet popped) can only cap
        // *too short* → one extra non-blocking pass, never a missed wakeup.
        let max_wait = {
            let now = Instant::now();
            let fds = self.fds.lock().unwrap_or_else(|p| p.into_inner());
            match fds.timers.peek() {
                Some(Reverse(entry)) => {
                    let until = entry.deadline.saturating_duration_since(now);
                    Some(max_wait.map_or(until, |w| w.min(until)))
                }
                None => max_wait,
            }
        };
        let mut poll_guard = self.poll.lock().unwrap_or_else(|p| p.into_inner());
        let EventLoopPoll {
            ref mut poll,
            ref mut events,
        } = *poll_guard;
        if let Err(e) = poll.poll(events, max_wait) {
            // Don't lose the stash on a poll error — put it back for
            // the next call.
            if !stashed.is_empty() {
                let mut pending = self.pending.lock().unwrap_or_else(|p| p.into_inner());
                let mut restored = stashed;
                restored.extend(pending.drain(..));
                *pending = restored;
            }
            return Err(e);
        }
        let mut fds = self.fds.lock().unwrap_or_else(|p| p.into_inner());
        // Stashed wakeups (oldest readiness) deliver ahead of this
        // poll's fresh events.
        let mut wakeups = std::mem::take(&mut stashed);
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
        // A2a-1: drain expired timers. Re-sample `now` so a long `poll` counts
        // toward expiry. Fire only registrations still live in `by_token`
        // (filters cancelled timers) and genuinely due per `FdState.deadline`
        // (filters a re-armed timer whose deadline advanced past a stale heap
        // entry). The dispatcher then claims each via `take_registration`,
        // exactly as for an fd wakeup.
        let now = Instant::now();
        // Peek-then-pop: the `is_some_and` confines the immutable `peek`
        // borrow to the condition, so the `pop` in the body is conflict-free.
        while fds
            .timers
            .peek()
            .is_some_and(|Reverse(entry)| entry.deadline <= now)
        {
            let Reverse(entry) = fds.timers.pop().expect("peeked due above");
            if let Some(state) = fds.by_token.get(&Token(entry.token_raw)) {
                if state.deadline.is_some_and(|d| d <= now) {
                    wakeups.push(Wakeup {
                        token: RegistrationToken(entry.token_raw),
                        parked: state.parked,
                        direction: state.direction,
                    });
                }
            }
        }
        Ok(wakeups)
    }

    /// Shutdown-path drain: consume a pending edge-armed *waker* event
    /// without losing real-fd readiness.
    ///
    /// Called by `karac_runtime_event_loop_shutdown_background_thread`
    /// and `karac_runtime_scheduler_shutdown_dispatcher` after their
    /// threads have joined. If a poller/dispatcher thread observed the
    /// shutdown flag before the shutdown `wake()` reached its `poll()`,
    /// mio's edge-armed waker leaves an event pending; consuming it
    /// here leaves the loop in a known-clean state for a follow-up
    /// direct poll or a restart. But the same non-blocking poll also
    /// consumes any *real* fd readiness that fired in the shutdown
    /// window — and edge-triggered registrations are never re-reported,
    /// so discarding those wakeups would wedge their parked tasks
    /// (bugs.md, surfaced 2026-06-06). Real wakeups are stashed in
    /// `pending` instead; the next `run_once` on this loop delivers
    /// them ahead of fresh events.
    pub fn drain_for_shutdown(&self) {
        let Ok(wakeups) = self.run_once(Some(Duration::ZERO)) else {
            return;
        };
        if wakeups.is_empty() {
            return;
        }
        let mut pending = self.pending.lock().unwrap_or_else(|p| p.into_inner());
        pending.extend(wakeups);
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

/// Process-global event-loop **shards**, lazily initialized.
///
/// v1 ran exactly one loop per process (see the module header). Stage B2
/// shards the reactor: [`resolve_shard_count`] independent `EventLoop`s, each
/// with its own `mio::Poll` / `epoll` instance, registry, waker, and `fds`
/// map. Connections route to a shard by raw fd ([`shard_of_fd`] = `raw_fd %
/// N`), so a synchronized burst spreads its `epoll_ctl` + `take_registration`
/// traffic across N independent locks and N poller threads instead of
/// serializing on one. This realizes the "M2 / M3 may shard across multiple
/// loops to reach the 1M+ idle target" commitment from the v1 module header.
/// See `wip-bench-day.md` Phase 2 Stage B for the burst-latency diagnosis
/// that motivated it.
static EVENT_LOOPS: OnceLock<Vec<Arc<EventLoop>>> = OnceLock::new();

/// Cached per-shard waker handles, index-aligned with [`EVENT_LOOPS`].
/// Populated in the same `get_or_init` that builds `EVENT_LOOPS`, so
/// observing `EVENT_LOOPS` initialized implies the handles are set.
static EVENT_LOOP_HANDLES: OnceLock<Vec<EventLoopHandle>> = OnceLock::new();

/// Fast-path flags: is *some* reactor thread currently blocked in `run_once`
/// on the shard epolls? Either the standalone background poller
/// ([`POLLER_ACTIVE`], test/embedding path) or the combined scheduler
/// dispatcher ([`DISPATCHER_ACTIVE`], the production poll-AND-dispatch threads,
/// Stage B3). `register_fd` consults these to decide whether to wake the
/// target shard so a blocked reactor thread re-arms interest on the new fd; the
/// direct `event_loop_poll` FFI consults them to short-circuit (a reactor
/// thread owns polling). Each flag is set to `true` *before* its threads are
/// spawned and back to `false` *after* they are joined, so any registration
/// observing `true` is guaranteed a thread is (or is about to be) polling — a
/// stale `true` only costs a harmless spurious wake. Plain atomics keep this
/// off the per-connection mutex path (B2 took a `BACKGROUND_POLLER` mutex here
/// per register).
static POLLER_ACTIVE: AtomicBool = AtomicBool::new(false);
static DISPATCHER_ACTIVE: AtomicBool = AtomicBool::new(false);

/// True when a background poller or the combined dispatcher is driving the
/// shard epolls (see [`POLLER_ACTIVE`] / [`DISPATCHER_ACTIVE`]).
fn reactor_polling_active() -> bool {
    POLLER_ACTIVE.load(Ordering::Acquire) || DISPATCHER_ACTIVE.load(Ordering::Acquire)
}

/// Maximum reactor shard count. The FFI registration token packs the shard
/// index into its top [`TOKEN_SHARD_BITS`] bits (see [`pack_token`]), so the
/// index must fit there — 256 distinct shards. Far above any real core count;
/// it only bounds the env-var override.
const MAX_SHARDS: usize = 1 << TOKEN_SHARD_BITS;

/// Resolve the reactor shard count. Honors `KARAC_REACTOR_SHARDS` (>= 1) when
/// set, then the back-compat `KARAC_DISPATCHER_THREADS` (Stage B3 fused the
/// dispatcher pool into the per-shard combined threads, so this knob now sizes
/// shards = combined threads); otherwise defaults to the machine's available
/// parallelism (floor 1, clamped to [`MAX_SHARDS`]).
fn resolve_shard_count() -> usize {
    for key in ["KARAC_REACTOR_SHARDS", "KARAC_DISPATCHER_THREADS"] {
        if let Ok(s) = std::env::var(key) {
            if let Ok(n) = s.parse::<usize>() {
                if n >= 1 {
                    return n.min(MAX_SHARDS);
                }
            }
        }
    }
    thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(1, MAX_SHARDS)
}

/// All reactor shards, lazily initialized on first access. The shard count is
/// frozen here (read from the environment once) so every consumer —
/// `register_fd`, the background pollers, the dispatcher's token routing —
/// agrees on `N = event_loops().len()`.
fn event_loops() -> &'static [Arc<EventLoop>] {
    EVENT_LOOPS
        .get_or_init(|| {
            // Mask SIGPIPE process-wide before any socket I/O. A karac-compiled
            // `main` bypasses Rust std's runtime init (`lang_start`), which is
            // what normally installs `SIG_IGN` for SIGPIPE; without it a socket
            // write that races the peer's close — routine under a reconnect
            // storm, where clients abort handshakes mid-flight — delivers
            // SIGPIPE, whose default action *silently terminates the process*
            // (exit 141, no core, no stderr, no kernel log). Every socket fd is
            // registered with this reactor before it sees any I/O, so this
            // one-time init fences all TCP/TLS/WS writes. Network-scoped on
            // purpose: compute-only binaries never reach the reactor, so the
            // lean-archive floor is untouched. Mirrors what std does for every
            // Rust program. See phase-7-codegen.md § "Unmasked SIGPIPE".
            #[cfg(unix)]
            unsafe {
                libc::signal(libc::SIGPIPE, libc::SIG_IGN);
            }
            let n = resolve_shard_count();
            let mut loops = Vec::with_capacity(n);
            let mut handles = Vec::with_capacity(n);
            for _ in 0..n {
                let ev = Arc::new(
                    EventLoop::new()
                        .expect("karac_runtime: process-global event loop shard init failed"),
                );
                handles.push(ev.handle());
                loops.push(ev);
            }
            // Separate `OnceLock`; ignore a duplicate-set from a racing
            // initializer (the `EVENT_LOOPS` get_or_init guarantees we are the
            // unique builder, but the handle write is its own lock).
            let _ = EVENT_LOOP_HANDLES.set(handles);
            loops
        })
        .as_slice()
}

/// Number of reactor shards (frozen at first [`event_loops`] call).
fn shard_count() -> usize {
    event_loops().len()
}

/// Map a raw fd to its reactor shard. `raw_fd` is non-negative for any live
/// descriptor; the `as u32` guards the theoretical negative case.
fn shard_of_fd(raw_fd: i32) -> usize {
    (raw_fd as u32 as usize) % shard_count()
}

// ── Registration-token packing ─────────────────────────────────────────────
//
// The FFI registration token (`u64`) returned by `register_fd` packs the
// owning shard index into its top `TOKEN_SHARD_BITS` bits and the shard-local
// `mio::Token` value into the low bits. Two purposes: (1) tokens are globally
// unique across shards even though each shard's `mio::Token`s start at 1 (two
// fds on different shards never collide); (2) the standalone `take_wakeups`
// path stamps the same packed value so a wakeup's token matches the
// `register_fd` return. `deregister_fd` routes by fd (the shard the register
// hashed to) and uses only the unpacked shard-local id. The combined
// dispatcher works on tokens from its own shard's `run_once`, so it needs no
// unpacking at all.

const TOKEN_SHARD_BITS: u32 = 8;
const TOKEN_LOCAL_MASK: u64 = (1u64 << (64 - TOKEN_SHARD_BITS)) - 1;

fn pack_token(shard: usize, local: usize) -> u64 {
    ((shard as u64) << (64 - TOKEN_SHARD_BITS)) | ((local as u64) & TOKEN_LOCAL_MASK)
}

fn unpack_token(packed: u64) -> (usize, usize) {
    (
        (packed >> (64 - TOKEN_SHARD_BITS)) as usize,
        (packed & TOKEN_LOCAL_MASK) as usize,
    )
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
    raw_fd: i64,
    direction: u8,
    parked: *mut c_void,
) -> u64 {
    register_fd_impl(raw_fd, direction, parked, std::ptr::null())
}

/// Register a raw fd with a bound per-task `cancel` flag (slice 5c). Same as
/// [`karac_runtime_event_loop_register_fd`], plus the `cancel: *const
/// AtomicBool` the dispatcher / cancel-sweep hand the coroutine's `poll_fn` so
/// it observes *its own* cooperative cancellation. The coroutine park-suspend
/// reads the flag off its bound completion slot
/// ([`karac_runtime_park_slot_cancel_ptr`]) and passes it here; a null `cancel`
/// is equivalent to the plain `register_fd` (never-cancelled fallback).
///
/// Unix-only, same as the plain register entry.
#[cfg(unix)]
#[no_mangle]
pub extern "C" fn karac_runtime_event_loop_register_fd_cancel(
    raw_fd: i64,
    direction: u8,
    parked: *mut c_void,
    cancel: *const AtomicBool,
) -> u64 {
    register_fd_impl(raw_fd, direction, parked, cancel)
}

#[cfg(unix)]
fn register_fd_impl(
    raw_fd: i64,
    direction: u8,
    parked: *mut c_void,
    cancel: *const AtomicBool,
) -> u64 {
    // i64 fd ABI → narrow to `RawFd` (i32 on Unix) for `mio::unix::SourceFd`
    // and the fd-hash shard routing. The signature is i64 for a uniform
    // cross-platform fd ABI; the Windows registration model (a different
    // slice) narrows to `RawSocket` instead.
    let raw_fd = raw_fd as std::os::unix::io::RawFd;
    let dir = match direction {
        0 => IoDirection::Read,
        1 => IoDirection::Write,
        2 => IoDirection::ReadWrite,
        _ => return 0,
    };
    let mut source = mio::unix::SourceFd(&raw_fd);
    let shard = shard_of_fd(raw_fd);
    let ev = &event_loops()[shard];
    match ev.register_with_cancel(&mut source, dir, None, parked, cancel) {
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
            if reactor_polling_active() {
                // Wake only THIS fd's shard reactor thread — each shard blocks
                // on its own epoll, so a cross-shard wake would be wasted churn.
                // (Covers both the background poller and the combined
                // dispatcher — either may be blocked in `run_once` on this
                // shard and must re-arm interest on the freshly-registered fd.)
                if let Some(h) = EVENT_LOOP_HANDLES.get().and_then(|hs| hs.get(shard)) {
                    let _ = h.wake();
                }
            }
            // Park-vs-cancel race guard (slice 5c). If this fd is registered for
            // a coroutine whose `cancel` flag is ALREADY set — a
            // `TaskGroup.cancel()` that raced the handler reaching its park, so
            // the cancel sweep ran before this registration existed — the idle
            // fd would never wake the dispatcher to observe the flag, hanging
            // the joiner. Request a fresh sweep so the just-parked-but-cancelled
            // coroutine is torn down promptly. Ordering: the entry is inserted
            // (under the `fds` lock, released by `register_with_cancel`) before
            // this `Acquire` load, so either this load sees the flag and we
            // sweep, or `taskgroup_cancel`'s own later request-sweep observes
            // the now-present entry — every cancel transition is covered.
            // SAFETY: a non-null `cancel` points at the owning handle's flag,
            // live for the duration of the ramp that called this.
            if !cancel.is_null() && unsafe { (*cancel).load(Ordering::Acquire) } {
                karac_runtime_request_cancel_sweep();
            }
            pack_token(shard, token.0)
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
pub extern "C" fn karac_runtime_event_loop_deregister_fd(raw_fd: i64, token: u64) -> i32 {
    // i64 fd ABI → narrow to `RawFd` (i32 on Unix); see `register_fd_impl`.
    let raw_fd = raw_fd as std::os::unix::io::RawFd;
    let mut source = mio::unix::SourceFd(&raw_fd);
    // Route by fd (authoritative — `register_fd` placed the entry on
    // `shard_of_fd(raw_fd)`); the token only carries the shard-local id.
    let ev = &event_loops()[shard_of_fd(raw_fd)];
    let (_token_shard, local) = unpack_token(token);
    match ev.deregister(&mut source, RegistrationToken(local)) {
        Ok(()) => 0,
        Err(_) => -1,
    }
}

/// Round-robin reactor shard for a *timer* registration. Unlike an fd
/// (routed by `shard_of_fd`), a timer has no fd, so any shard delivers — every
/// shard runs a poller that drives `run_once` and fires its own `timers` heap.
/// Round-robin spreads timer load across shards rather than piling every sleep
/// on one reactor.
fn next_timer_shard() -> usize {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed) % shard_count()
}

/// Register a one-shot timer that fires `duration_nanos` from now, surfacing
/// `parked` to the dispatcher (via `take_wakeups`) on expiry. The C ABI for
/// the `suspends` async-sleep substrate (A2a-2).
///
/// No fd and no `epoll_ctl`: the registration lives in a reactor shard's timer
/// min-heap (driving its poll-timeout cap + expiry, A2a-1) and in `by_token`
/// (so the dispatcher claims it via the same `take_registration` as an fd
/// wakeup — the resume path is identical). Returns a packed `shard|local`
/// token (non-zero on success), or `0` if the shard count is somehow zero.
/// Cancel before firing with [`karac_runtime_event_loop_cancel_timer`].
/// `cancel` is the per-task cancel flag (null if unbound), stored like any
/// other registration. `register_timer` itself wakes the shard reactor so a
/// poller blocked in `run_once(None)` re-derives its timeout against the new
/// deadline.
#[no_mangle]
pub extern "C" fn karac_runtime_event_loop_register_timer(
    duration_nanos: u64,
    parked: *mut c_void,
    cancel: *const AtomicBool,
) -> u64 {
    let deadline = Instant::now() + Duration::from_nanos(duration_nanos);
    let shard = next_timer_shard();
    let ev = &event_loops()[shard];
    let token = ev.register_timer(deadline, parked, cancel);
    pack_token(shard, token.0)
}

/// Cancel a pending timer before it fires. Unpacks the `shard|local` token and
/// claims the registration with `take_registration` (no fd / no `epoll_ctl`),
/// dropping the parked pointer so the timer never surfaces; the stale heap
/// entry is filtered lazily at the next expiry pop. Returns `0` if a live
/// registration was claimed, `-1` if the timer had already fired or been
/// cancelled (or the token's shard is out of range).
#[no_mangle]
pub extern "C" fn karac_runtime_event_loop_cancel_timer(token: u64) -> i32 {
    let (shard, local) = unpack_token(token);
    if shard >= shard_count() {
        return -1;
    }
    let ev = &event_loops()[shard];
    if ev.take_registration(RegistrationToken(local)).is_some() {
        0
    } else {
        -1
    }
}

/// Windows IOCP bridge — **groundwork PoC** for Phase 6 line 13
/// ([`docs/spikes/windows-iocp-eventloop.md`]). Compile-checked via
/// `cargo check --target x86_64-pc-windows-msvc`; **not yet runtime-validated**
/// (no Windows runtime testing is possible from the macOS dev host). This
/// proves the single highest-risk design decision compiles: bridging a raw
/// `SOCKET` into mio's AFD/IOCP readiness backend through an owned
/// `mio::net::TcpStream`, then recovering the handle WITHOUT closing it.
///
/// The full `#[cfg(windows)]` `register_fd` / `register_fd_cancel` /
/// `deregister_fd` are written against this bridge (see the spike's
/// implementation plan). They are NOT included here because they require the
/// `i32 -> i64` fd-ABI widening (spike Problem 2) which touches codegen and is
/// sequenced separately to avoid colliding with the active codegen agents.
#[cfg(windows)]
// Intentionally unwired groundwork — consumed once the `#[cfg(windows)]`
// register/deregister FFIs land (spike implementation plan, step 2).
#[allow(dead_code)]
pub(crate) mod windows_iocp_bridge {
    use std::os::windows::io::{FromRawSocket, IntoRawSocket, RawSocket};

    /// Adopt a raw `SOCKET` into a mio readiness source **without** taking over
    /// its lifetime. The returned value's `Drop` would `closesocket()` the
    /// handle, so callers MUST hand it to [`release`] after
    /// register/deregister — never let it drop. Getting this wrong is the
    /// Windows analog of the `tcp_close` double-free that wedged the macOS demo
    /// (phase-6-runtime.md RESOLUTION (2)).
    ///
    /// # Safety
    /// `sock` must be a live socket the runtime owns for the duration of the
    /// register/deregister call that consumes the returned source.
    pub(crate) unsafe fn source_from_socket(sock: RawSocket) -> mio::net::TcpStream {
        // `from_raw_socket` adopts the handle; `from_std` does not re-wrap or
        // re-validate. Readiness interest (read/write) is identical whether the
        // socket is a listener or a stream, so a single TcpStream wrapper covers
        // both register sites — to be confirmed on Windows (spike open question).
        let std_sock = std::net::TcpStream::from_raw_socket(sock);
        mio::net::TcpStream::from_std(std_sock)
    }

    /// Recover the raw handle without running the close (`mem::forget` +
    /// `as_raw_socket`, the Windows twin of the unix `IntoRawFd` no-destructor
    /// discipline already used throughout this module).
    pub(crate) fn release(source: mio::net::TcpStream) -> RawSocket {
        source.into_raw_socket()
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
    // If a reactor thread owns polling — the background poller (drain via
    // `take_wakeups`) or the combined dispatcher (drives `poll_fn` itself) —
    // direct FFI poll callers get back an empty result rather than contend for
    // a shard's inner poll Mutex (held for the duration of its blocking call).
    if reactor_polling_active() {
        return 0;
    }
    // Synchronous single-thread fallback (tests / embedding). Production
    // never takes this path — codegen starts the background poller +
    // dispatcher, which short-circuits above.
    unsafe { poll_shards(event_loops(), max_wait, wakeups_out, max_wakeups) }
}

/// Write `wakeups` (from shard `shard`) into the caller buffer at offset
/// `*n`, bounded by `max_wakeups`. Excess wakeups are dropped — documented
/// [`karac_runtime_event_loop_poll`] behavior.
///
/// # Safety
///
/// Same contract as [`karac_runtime_event_loop_poll`]: `wakeups_out` must be
/// writable for `max_wakeups` elements.
unsafe fn write_wakeups_out(
    wakeups_out: *mut KaracWakeup,
    n: &mut usize,
    max_wakeups: usize,
    shard: usize,
    wakeups: Vec<Wakeup>,
) {
    for w in wakeups {
        if *n >= max_wakeups {
            break;
        }
        // SAFETY: caller's contract — `wakeups_out` is writable for
        // `max_wakeups` elements; we write at offset `*n < max_wakeups`.
        unsafe {
            wakeups_out.add(*n).write(KaracWakeup {
                token: pack_token(shard, w.token.0),
                parked: w.parked,
                direction: w.direction as u8,
            });
        }
        *n += 1;
    }
}

/// Sweep-and-wait poll over an explicit shard slice — the synchronous
/// fallback body of [`karac_runtime_event_loop_poll`], parameterized over
/// `loops` so tests can drive it against a private single-shard loop.
///
/// Because each shard owns its own epoll, one thread cannot block-wait on
/// all shards at once, so we sweep every shard non-blocking and, if nothing
/// is ready and the caller allowed waiting, fall back to bounded blocking
/// slices across shards until a wakeup arrives or the deadline elapses. A
/// pending cross-thread `wake()` (which wakes every shard) cuts a slice
/// short, so the loop stays responsive without busy-spinning.
///
/// **The blocking slice delivers.** `run_once` is the readiness sink: mio's
/// epoll/kqueue registrations are edge-triggered, so whichever `run_once`
/// call observes an event *consumes* it — there is no queue behind it
/// (Stage B3 removed it) and the kernel will not re-report the edge. The
/// blocking slice therefore writes its wakeups into the caller buffer
/// exactly like the sweep does. Discarding them (the Stage B2 regression)
/// silently lost any readiness that fired while its own shard was the one
/// blocking — `poll` then waited out its full deadline and returned 0.
///
/// # Safety
///
/// Same contract as [`karac_runtime_event_loop_poll`]: `wakeups_out` must be
/// writable for `max_wakeups` elements.
unsafe fn poll_shards(
    loops: &[Arc<EventLoop>],
    max_wait: Option<Duration>,
    wakeups_out: *mut KaracWakeup,
    max_wakeups: usize,
) -> usize {
    // Per-slice blocking budget when waiting: small enough that the sweep
    // re-checks all shards promptly, large enough to avoid a hot spin.
    const SLICE: Duration = Duration::from_millis(20);
    let deadline = match max_wait {
        Some(d) if d.is_zero() => Some(Instant::now()), // one non-blocking sweep
        Some(d) => Some(Instant::now() + d),
        None => None, // "indefinite" — bounded by the slice loop
    };
    let mut n = 0usize;
    let mut rotate = 0usize;
    loop {
        for (shard, ev) in loops.iter().enumerate() {
            if n >= max_wakeups {
                return n;
            }
            let wakeups = match ev.run_once(Some(Duration::ZERO)) {
                Ok(w) => w,
                Err(_) => continue,
            };
            // SAFETY: forwarding the caller's buffer contract.
            unsafe { write_wakeups_out(wakeups_out, &mut n, max_wakeups, shard, wakeups) };
        }
        if n > 0 {
            return n;
        }
        match deadline {
            Some(d) if Instant::now() >= d => return 0,
            _ => {}
        }
        // Nothing ready yet and waiting is allowed: block on one shard for a
        // bounded slice (rotating so every shard gets blocking attention),
        // capped by any remaining deadline. Anything this blocking call
        // returns is delivered — see "The blocking slice delivers" above.
        let slice = match deadline {
            Some(d) => SLICE.min(d.saturating_duration_since(Instant::now())),
            None => SLICE,
        };
        if !loops.is_empty() {
            let shard = rotate % loops.len();
            if let Ok(wakeups) = loops[shard].run_once(Some(slice)) {
                // SAFETY: forwarding the caller's buffer contract.
                unsafe { write_wakeups_out(wakeups_out, &mut n, max_wakeups, shard, wakeups) };
            }
            rotate += 1;
            if n > 0 {
                return n;
            }
        }
    }
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
    // Ensure init so EVENT_LOOP_HANDLES is populated. Wake every shard — a
    // bare `wake()` has no fd context, so it targets all reactors (used by the
    // poller-shutdown path to unblock every shard's `run_once`).
    let _ = event_loops();
    match EVENT_LOOP_HANDLES.get() {
        Some(handles) => {
            let mut ok = true;
            for h in handles {
                if h.wake().is_err() {
                    ok = false;
                }
            }
            if ok {
                0
            } else {
                -1
            }
        }
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
// An opt-in set of background threads that own event-loop polling — **one
// thread per reactor shard** (Stage B2). Each thread loops on its shard's
// `EventLoop::run_once(None)` indefinitely (blocking in mio's poll inside that
// shard's `poll` Mutex), depositing wakeups into a **single shared**
// `VecDeque<KaracWakeup>` for consumption by a scheduler thread via
// `karac_runtime_event_loop_take_wakeups`. Sharding the *polling* (N epoll
// instances, N `fds` maps) is what removes the single-poller burst-dispatch
// plateau; the output queue stays shared so the dispatcher pool and the
// `take_wakeups` FFI keep their single-queue contract. Each shard packs its
// index into the wakeup token (see [`pack_token`]) so the dispatcher can route
// `take_registration` back to the owning shard.
//
// **No deadlock with registration.** The `EventLoop` splits its inner state
// into two independent Mutexes — `poll` (held by a shard's poller for the
// duration of each blocking poll) and `fds` (held only briefly by register /
// deregister). Concurrent `karac_runtime_event_loop_register_fd` calls acquire
// only the target shard's `fds` Mutex, so a long-blocking poll never stalls
// registration.
//
// **Direct FFI poll coexistence.** While the background poller is running,
// direct `karac_runtime_event_loop_poll` callers short-circuit to return
// 0 immediately — the background poller has authoritative ownership of
// the polling channel and direct callers should drain via `take_wakeups`
// instead. Documented in `karac_runtime_event_loop_poll`'s body.
//
// **Shutdown protocol.** `karac_runtime_event_loop_shutdown_background_thread`
// sets the shutdown flag, fires the cross-thread `wake()` to unblock **every**
// shard's current poll call, signals the queue's `Condvar` to release any
// waiting `take_wakeups` callers, joins all shard threads, and clears the
// global slot. Idempotent — calling on a non-running thread returns -1
// without side effects, so a re-start after shutdown is supported within
// the same process.

/// Internal poller state. Held inside `Arc` so every spawned shard thread can
/// share it with the global slot.
struct EventLoopPoller {
    /// Reactor shards this poller drives — index-aligned with the global
    /// [`event_loops`]. Shard thread `i` polls `event_loops[i]`.
    event_loops: Vec<Arc<EventLoop>>,
    /// Single shared output queue across all shard threads.
    queue: Mutex<VecDeque<KaracWakeup>>,
    notify: Condvar,
    shutdown: AtomicBool,
    /// One `JoinHandle` per shard thread. Wrapped in `Mutex<Vec<_>>` so the
    /// shutdown path can `take()` them independently of the rest of the
    /// poller state.
    handles: Mutex<Vec<thread::JoinHandle<()>>>,
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

fn poller_thread_main(poller: Arc<EventLoopPoller>, shard: usize) {
    let event_loop = Arc::clone(&poller.event_loops[shard]);
    while !poller.shutdown.load(Ordering::Acquire) {
        let wakeups = match event_loop.run_once(None) {
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
                // Pack this shard's index so the dispatcher routes
                // `take_registration` back to the owning reactor.
                token: pack_token(shard, w.token.0),
                parked: w.parked,
                direction: w.direction as u8,
            });
        }
        drop(q);
        // Wake ONE consumer, not all. With multiple dispatcher threads a
        // `notify_all` here is a thundering herd — every push wakes every
        // thread, most find nothing and re-sleep, and the futex churn caps
        // throughput (measured: more dispatcher threads made burst drain
        // *slower* under notify_all). Instead wake one; that consumer wakes
        // another whenever it drains a batch and still sees a backlog
        // (the "wake-a-friend" ramp in `take_wakeups`), so consumers engage
        // in proportion to the queue depth without the all-wake storm.
        poller.notify.notify_one();
    }
}

/// Start the background event-loop poller threads — one per reactor shard.
///
/// Idempotent: a second call while the threads are already running
/// returns 0 without re-spawning. Returns 0 on success.
#[no_mangle]
pub extern "C" fn karac_runtime_event_loop_start_background_thread() -> i32 {
    let mut slot = lock_background_poller_slot();
    if slot.is_some() {
        return 0;
    }
    let poller = Arc::new(EventLoopPoller {
        event_loops: event_loops().to_vec(),
        queue: Mutex::new(VecDeque::new()),
        notify: Condvar::new(),
        shutdown: AtomicBool::new(false),
        handles: Mutex::new(Vec::new()),
    });
    let n = poller.event_loops.len();
    let mut handles = Vec::with_capacity(n);
    // Publish BEFORE spawning so any `register_fd` that races a poller thread
    // into its first blocking `run_once` still wakes it (see `POLLER_ACTIVE`).
    POLLER_ACTIVE.store(true, Ordering::Release);
    for shard in 0..n {
        let poller_for_thread = Arc::clone(&poller);
        let join = thread::Builder::new()
            .name(format!("karac-event-loop-{shard}"))
            .spawn(move || poller_thread_main(poller_for_thread, shard))
            .expect("karac_runtime: failed to spawn event-loop poller thread");
        handles.push(join);
    }
    *poller.handles.lock().unwrap_or_else(|p| p.into_inner()) = handles;
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
    // Wake-a-friend ramp: this consumer took a full batch and the queue
    // still has work — wake one more sibling dispatcher to help drain it.
    // Under a burst this engages consumers one-per-batch in a chain
    // (each helper wakes the next), reaching full parallelism within a few
    // batch times, without the poller having to wake everyone up front.
    // No-op with a single dispatcher thread (nothing waiting to wake).
    if !q.is_empty() {
        poller.notify.notify_one();
    }
    n_out
}

/// Signal every background poller shard thread to stop, unblock each one's
/// `poll` call via the cross-thread waker, join them all, and clear the
/// global slot.
///
/// Returns 0 on success, -1 if no background threads are running.
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
    // `wake()` fans out to every shard, unblocking each poller's `run_once`.
    let _ = karac_runtime_event_loop_wake();
    poller.notify.notify_all();
    let joins: Vec<_> =
        std::mem::take(&mut *poller.handles.lock().unwrap_or_else(|p| p.into_inner()));
    for h in joins {
        let _ = h.join();
    }
    // Cleared after join so a concurrent `register_fd` never observes `false`
    // while a poller thread is still blocked in `run_once`.
    POLLER_ACTIVE.store(false, Ordering::Release);
    // Drain any pending waker event on each shard. If a poller thread observed
    // the shutdown flag *before* our `wake()` was delivered to its `poll()`
    // call, mio's edge-armed waker leaves the event pending — the next thread
    // to poll that shard would receive it as a spurious empty wakeup. A
    // non-blocking drain per shard consumes it and leaves every event
    // loop in a known-clean state; real-fd readiness swept up alongside the
    // waker event is stashed for redelivery, not discarded (see
    // `drain_for_shutdown`). BACKGROUND_POLLER is already None here (we
    // took the Arc out at the top), so these polls don't compete with any
    // background polling.
    for ev in event_loops() {
        ev.drain_for_shutdown();
    }
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
pub extern "C" fn karac_runtime_test_bind_and_print_port() -> i64 {
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
    // but with no double-ownership window. Widened to i64 so the fd ABI
    // is uniform across platforms (a Windows `SOCKET` is pointer-sized).
    // On Unix `RawFd` is a small non-negative i32 that sign-extends
    // losslessly (and negative error codes sign-extend correctly too,
    // keeping the `fd >= 0` success test intact).
    listener.into_raw_fd() as i64
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

/// Map an `io::Error` from a network *construction* syscall (bind /
/// listen / accept / connect) to a Kāra-stable negative error code that
/// codegen's `build_fd_construct_result` decodes into a named
/// `TcpError` / `TlsError` variant (phase-8 line 74).
///
/// **Why a stable code rather than `-errno`.** Raw errno numbers are
/// platform-specific (`EADDRINUSE` is 48 on macOS, 98 on Linux), so a
/// `fd == -48` comparison baked into codegen would be wrong on Linux.
/// `std::io::ErrorKind` already normalizes the OS errno into
/// platform-independent names — we map those to a small fixed code
/// space here, and codegen branches on the *code*, never on a raw
/// errno. The catch-all is `-1` (decoded as `Other`, carrying the code
/// so the i32 payload is still a usable signal).
///
/// Code space (negative so `fd >= 0` stays the success test):
///   -1 → Other (catch-all)      -3 → ConnectionRefused (ECONNREFUSED)
///   -2 → AddrInUse (EADDRINUSE)  -4 → PermissionDenied (EACCES)
pub(crate) fn net_construct_error_code(e: &std::io::Error) -> i32 {
    use std::io::ErrorKind;
    match e.kind() {
        ErrorKind::AddrInUse => -2,
        ErrorKind::ConnectionRefused => -3,
        ErrorKind::PermissionDenied => -4,
        _ => -1,
    }
}

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
pub unsafe extern "C" fn karac_runtime_tcp_bind(addr_ptr: *const u8, addr_len: i64) -> i64 {
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
    // `bind`/`listen` failures carry a meaningful cause (EADDRINUSE when
    // the port is taken, EACCES for a privileged port) — surface it via
    // the stable code so callers can branch (phase-8 line 74).
    if let Err(e) = socket.bind(&socket_addr.into()) {
        return net_construct_error_code(&e) as i64;
    }
    if let Err(e) = socket.listen(KARAC_RUNTIME_TCP_LISTEN_BACKLOG) {
        return net_construct_error_code(&e) as i64;
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
    // i64 fd ABI: Unix `RawFd` (i32) sign-extends losslessly. See
    // `karac_runtime_test_bind_and_print_port` for the rationale.
    listener.into_raw_fd() as i64
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
pub extern "C" fn karac_runtime_tcp_accept(listener_fd: i64) -> i64 {
    use std::os::unix::io::{FromRawFd, IntoRawFd, RawFd};
    if listener_fd < 0 {
        return -1;
    }
    // i64 → RawFd (i32 on Unix). The signature is i64 for a uniform
    // cross-platform fd ABI (Windows `SOCKET` is pointer-sized); the
    // Unix body narrows back to `RawFd` for the `std::net` wrappers.
    let listener_fd = listener_fd as RawFd;
    // SAFETY: the listener_fd must come from a successful
    // `karac_runtime_tcp_bind` call (or equivalent). We construct a
    // borrowed TcpListener via from_raw_fd, accept() through it, then
    // immediately into_raw_fd() to give the fd back without running
    // the destructor (the listener stays open for further accepts).
    let listener = unsafe { std::net::TcpListener::from_raw_fd(listener_fd) };
    let result: i64 = match listener.accept() {
        Ok((conn, _addr)) => conn.into_raw_fd() as i64,
        Err(e) => net_construct_error_code(&e) as i64,
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
/// On failure returns a Kāra-stable negative error code (see
/// [`net_construct_error_code`]): `-3` for `ConnectionRefused` (the
/// common "server not up yet" case a reconnect loop branches on), `-1`
/// for any other cause. UTF-8 / parse failures return `-1` (no OS
/// error to classify). Codegen's `build_fd_construct_result` decodes
/// the code into the matching `TcpError` variant (phase-8 line 74).
///
/// # Safety
///
/// `addr_ptr` must point to `addr_len` readable bytes for the duration
/// of the call (the kara `String`'s `{ptr, len}`), or be null with
/// `addr_len <= 0` (rejected via the early return). The bytes are read
/// once and not retained.
#[cfg(unix)]
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_tcp_connect(addr_ptr: *const u8, addr_len: i64) -> i64 {
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
        Ok(sock) => sock.into_raw_fd() as i64,
        // ECONNREFUSED (server not up) vs a fatal cause is exactly the
        // distinction a reconnect loop needs — surface it (line 74).
        Err(e) => net_construct_error_code(&e) as i64,
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
    stream_fd: i64,
    buf_ptr: *mut u8,
    buf_len: i64,
) -> i64 {
    use std::io::Read;
    use std::os::unix::io::{FromRawFd, IntoRawFd, RawFd};
    if stream_fd < 0 {
        return -1;
    }
    if buf_ptr.is_null() || buf_len <= 0 {
        return 0;
    }
    let buf = std::slice::from_raw_parts_mut(buf_ptr, buf_len as usize);
    // i64 fd ABI → narrow to `RawFd` (i32) for the Unix `std::net` wrapper.
    // SAFETY: the stream_fd must come from a successful
    // `karac_runtime_tcp_accept` call (or equivalent). Borrowed
    // TcpStream wrapper avoids destructor while reading.
    let mut stream = std::net::TcpStream::from_raw_fd(stream_fd as RawFd);
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
    stream_fd: i64,
    buf_ptr: *const u8,
    buf_len: i64,
) -> i64 {
    use std::io::Write;
    use std::os::unix::io::{FromRawFd, IntoRawFd, RawFd};
    if stream_fd < 0 {
        return -1;
    }
    if buf_ptr.is_null() || buf_len <= 0 {
        return 0;
    }
    let buf = std::slice::from_raw_parts(buf_ptr, buf_len as usize);
    // i64 fd ABI → narrow to `RawFd` (i32) for the Unix `std::net` wrapper.
    // SAFETY: the stream_fd must come from a successful
    // `karac_runtime_tcp_accept` call (or equivalent). Borrowed
    // TcpStream wrapper avoids destructor while writing.
    let mut stream = std::net::TcpStream::from_raw_fd(stream_fd as RawFd);
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
pub extern "C" fn karac_runtime_tcp_close(fd: i64) -> i32 {
    use std::os::unix::io::{FromRawFd, RawFd};
    if fd < 0 {
        return 0;
    }
    // i64 fd ABI → narrow to `RawFd` (i32) for the Unix close-on-drop path.
    let fd = fd as RawFd;
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
    fd: i64,
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
    fd: i64,
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
    fd: i64,
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
    fd: i64,
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
unsafe fn ws_send_masked_data_frame(fd: i64, msg_ptr: *const u8, msg_len: i64, opcode: u8) -> i64 {
    use std::os::unix::io::{FromRawFd, IntoRawFd, RawFd};
    if fd < 0 || msg_len < 0 {
        return -1;
    }
    // i64 fd ABI → narrow to `RawFd` (i32 on Unix); see `register_fd_impl`.
    let fd = fd as RawFd;
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
unsafe fn ws_send_data_frame(fd: i64, msg_ptr: *const u8, msg_len: i64, opcode: u8) -> i64 {
    use std::os::unix::io::{FromRawFd, IntoRawFd, RawFd};
    if fd < 0 || msg_len < 0 {
        return -1;
    }
    // i64 fd ABI → narrow to `RawFd` (i32 on Unix); see `register_fd_impl`.
    // The TLS session map is keyed by this narrowed fd (register + lookup
    // both narrow identically), so the key stays consistent on Unix.
    let fd = fd as RawFd;
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
    // Gated behind the `tls` feature: the lean archive has no TLS
    // sessions, so this path can never fire there and is compiled out.
    #[cfg(feature = "tls")]
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
    fd: i64,
    out_ptr: *mut u8,
    out_max_len: i64,
    accept_opcode: u8,
) -> i64 {
    use std::os::unix::io::{FromRawFd, IntoRawFd, RawFd};
    if fd < 0 || out_max_len < 0 {
        return -1;
    }
    // i64 fd ABI → narrow to `RawFd` (i32 on Unix); see `ws_send_data_frame`
    // for the TLS-session-key consistency note.
    let fd = fd as RawFd;
    if out_ptr.is_null() && out_max_len > 0 {
        return -1;
    }

    // Phase 6 line 236 slice 3 — TLS-aware dispatch. Same shape as
    // `ws_send_data_frame`: route through rustls when the fd has a
    // session in the TLS registry. The frame-parser closure is
    // generic over Read+Write so the same body services both
    // transports. Gated behind the `tls` feature (see `ws_send_data_frame`).
    #[cfg(feature = "tls")]
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
    fd: i64,
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
    fd: i64,
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
// Handshake validation (RFC 6455 §4.2.1, line-128 hardening):
// `ws_validate_handshake` requires `Upgrade: websocket`,
// `Connection: Upgrade` (token-in-list), and `Sec-WebSocket-Version`
// offering `13` before the `Sec-WebSocket-Key`, and answers a
// non-conforming request with `400 Bad Request` (missing
// Upgrade/Connection/Key) or `426 Upgrade Required` +
// `Sec-WebSocket-Version: 13` (bad version) rather than upgrading
// anyway. The `KARAC_WS_HANDSHAKE_TIMEOUT_MS` read-timeout
// (default 10 s, `0` disables) is applied to the socket for the
// pre-`101` phase only and cleared on success.
//
// **Slowloris caveat — per-read, not whole-request.** The timeout
// is `SO_RCVTIMEO`: it re-arms on every successful `read(2)`, so it
// reaps a *silently stalled* peer but NOT a peer that dribbles one
// byte per interval (that resets the clock each read). Such a peer
// is bounded only by the 8 KiB request cap below (≈ 8192 × timeout
// worst case). A whole-request wall-clock deadline + a bound on the
// TLS handshake pool's work queue is the carved pre-public-v1 P0
// follow-on (phase-8 line 128, residual-slowloris entry); it does
// NOT affect Demo 1 (completed-handshake idle connections).
//
// v1 limitations (deferred to follow-on slices):
//
// - **Request line / `Host` not enforced.** The method (`GET`),
//   HTTP version, and `Host:` header are not checked — every real
//   client sends them and rejecting buys no protocol safety. The
//   three header checks above are what distinguish a WebSocket
//   upgrade from arbitrary HTTP.
// - **No subprotocol / extension negotiation.** The 101 response
//   never echoes `Sec-WebSocket-Protocol` or `Sec-WebSocket-Extensions`
//   (tracked as a carved follow-on under phase-8 line 128).
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
///
/// `deadline` is a **whole-request wall-clock bound** checked
/// before every read: it closes the dribble-slowloris hole that
/// the per-read `SO_RCVTIMEO` alone leaves open (a peer feeding
/// one byte per timeout interval re-arms the per-read clock
/// forever, but cannot outlast the total deadline). With the
/// caller's per-read timeout `t` and deadline `now + t`, total
/// read time is bounded to ≈ `2t` (the in-flight read can run one
/// extra `t` past the deadline before the next pre-read check
/// fires). `None` disables the bound (handshake-timeout opt-out).
#[cfg(unix)]
fn ws_read_http_request<R: std::io::Read>(
    stream: &mut R,
    deadline: Option<std::time::Instant>,
) -> Option<Vec<u8>> {
    const MAX_REQUEST_SIZE: usize = 8 * 1024;
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        if let Some(d) = deadline {
            if std::time::Instant::now() >= d {
                return None;
            }
        }
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

/// Find the raw (trimmed) value bytes of an HTTP header named
/// `name_lower` (which MUST be supplied lowercase, e.g.
/// `b"sec-websocket-version"`). Header-name lookup is
/// case-insensitive per RFC 7230 §3.2; the returned value has
/// leading/trailing linear whitespace stripped. Returns `None`
/// if the header is absent or has an empty value.
fn ws_header_value<'a>(request: &'a [u8], name_lower: &[u8]) -> Option<&'a [u8]> {
    for line in request.split(|&b| b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        // The request line (`GET /ws HTTP/1.1`) and the terminating
        // blank line carry no colon — skip them rather than aborting
        // the whole search.
        let colon = match line.iter().position(|&b| b == b':') {
            Some(c) => c,
            None => continue,
        };
        let (name, rest) = line.split_at(colon);
        if name.len() != name_lower.len() {
            continue;
        }
        if !name
            .iter()
            .zip(name_lower.iter())
            .all(|(l, r)| l.to_ascii_lowercase() == *r)
        {
            continue;
        }
        // rest[0] is the ':'; trim LWS from both ends of the value.
        let mut value = &rest[1..];
        while let Some((b, tail)) = value.split_first() {
            if *b == b' ' || *b == b'\t' {
                value = tail;
            } else {
                break;
            }
        }
        while let Some((b, head)) = value.split_last() {
            if *b == b' ' || *b == b'\t' {
                value = head;
            } else {
                break;
            }
        }
        if value.is_empty() {
            return None;
        }
        return Some(value);
    }
    None
}

/// True if the comma-separated header `name_lower` carries
/// `token_lower` (supplied lowercase) as one of its tokens,
/// matched case-insensitively. Used for the RFC 6455 §4.2.1
/// `Upgrade: websocket` and `Connection: Upgrade` checks, where
/// the value is a 1#token list (`Connection: keep-alive, Upgrade`
/// is legal and must still match the `upgrade` token).
fn ws_header_has_token(request: &[u8], name_lower: &[u8], token_lower: &[u8]) -> bool {
    let value = match ws_header_value(request, name_lower) {
        Some(v) => v,
        None => return false,
    };
    value.split(|&b| b == b',').any(|tok| {
        let tok = ws_trim_ascii(tok);
        tok.len() == token_lower.len()
            && tok
                .iter()
                .zip(token_lower.iter())
                .all(|(l, r)| l.to_ascii_lowercase() == *r)
    })
}

/// Strip leading/trailing ASCII spaces and tabs from a byte slice.
fn ws_trim_ascii(mut s: &[u8]) -> &[u8] {
    while let Some((b, tail)) = s.split_first() {
        if *b == b' ' || *b == b'\t' {
            s = tail;
        } else {
            break;
        }
    }
    while let Some((b, head)) = s.split_last() {
        if *b == b' ' || *b == b'\t' {
            s = head;
        } else {
            break;
        }
    }
    s
}

/// Why a WebSocket opening-handshake request was rejected. Maps
/// to the HTTP status the server writes back before closing the
/// connection (RFC 6455 §4.2.2 / §4.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WsHandshakeReject {
    /// Missing/invalid `Upgrade`, `Connection`, or `Sec-WebSocket-Key`
    /// header — the request is not a well-formed WebSocket upgrade.
    /// Answered with `400 Bad Request`.
    BadRequest(&'static str),
    /// `Sec-WebSocket-Version` is absent or does not offer `13`.
    /// RFC 6455 §4.4 requires a `426 Upgrade Required` carrying a
    /// `Sec-WebSocket-Version: 13` header so the client can retry.
    UnsupportedVersion,
}

impl WsHandshakeReject {
    /// The complete HTTP response bytes to write back on rejection.
    fn response_bytes(self) -> &'static [u8] {
        match self {
            WsHandshakeReject::BadRequest(_) => {
                b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"
            }
            WsHandshakeReject::UnsupportedVersion => {
                b"HTTP/1.1 426 Upgrade Required\r\n\
                  Connection: close\r\n\
                  Sec-WebSocket-Version: 13\r\n\
                  Content-Length: 0\r\n\r\n"
            }
        }
    }

    /// Short diagnostic string for the runtime-side `Err(String)`
    /// log path (never sent to the client).
    fn log_reason(self) -> &'static str {
        match self {
            WsHandshakeReject::BadRequest(why) => why,
            WsHandshakeReject::UnsupportedVersion => {
                "Sec-WebSocket-Version missing or not 13 (RFC 6455 §4.4)"
            }
        }
    }
}

/// Validate a client's RFC 6455 §4.2.1 opening-handshake request.
/// On success returns the `Sec-WebSocket-Key` value (to be fed
/// into the `Sec-WebSocket-Accept` digest); on failure returns the
/// rejection class so the caller can write the matching HTTP
/// status. Checked in RFC order: `Upgrade: websocket`, then
/// `Connection: Upgrade`, then `Sec-WebSocket-Version: 13`, then
/// `Sec-WebSocket-Key` presence.
///
/// The request line (method / HTTP version) and `Host` are NOT
/// enforced: every real WebSocket client issues `GET ... HTTP/1.1`
/// and a `Host`, and rejecting on them buys no protocol safety
/// while risking false negatives against lenient proxies. The
/// three header checks here are what actually distinguish a
/// WebSocket upgrade from an arbitrary HTTP request.
pub(crate) fn ws_validate_handshake(request: &[u8]) -> Result<&[u8], WsHandshakeReject> {
    if !ws_header_has_token(request, b"upgrade", b"websocket") {
        return Err(WsHandshakeReject::BadRequest(
            "missing or invalid Upgrade: websocket header",
        ));
    }
    if !ws_header_has_token(request, b"connection", b"upgrade") {
        return Err(WsHandshakeReject::BadRequest(
            "missing or invalid Connection: Upgrade header",
        ));
    }
    if !ws_header_has_token(request, b"sec-websocket-version", b"13") {
        return Err(WsHandshakeReject::UnsupportedVersion);
    }
    extract_sec_websocket_key(request).ok_or(WsHandshakeReject::BadRequest(
        "missing Sec-WebSocket-Key header",
    ))
}

/// Read `KARAC_WS_HANDSHAKE_TIMEOUT_MS` into a socket read-timeout
/// for the opening handshake. Default 10 s; `0` disables (returns
/// `None`); unparseable / negative values fall back to the
/// default. Pure over the raw string so the parse matrix is
/// unit-testable without touching the environment.
///
/// Unlike the connection-cap half of the line-124 serve-loop
/// hardening, this timeout is **on by default**: it governs only
/// the pre-`101` handshake phase and is cleared on the socket the
/// instant the upgrade completes, so it never reaps an established
/// idle connection — the Demo 1 1M-idle-connection workload is
/// structurally unaffected. The returned value serves a dual role:
/// as the socket's per-read `SO_RCVTIMEO` (reaping a silent stall)
/// *and* as the basis for the whole-request wall-clock deadline
/// `now + t` threaded into `ws_read_http_request` / `DeadlineStream`
/// (reaping a byte-per-interval dribbler). Total handshake-read
/// time is therefore bounded to ≈ `2t`.
fn ws_handshake_timeout_from_raw(raw: Option<&str>) -> Option<std::time::Duration> {
    const DEFAULT_MS: u64 = 10_000;
    let ms = match raw {
        None => DEFAULT_MS,
        Some(s) => match s.trim().parse::<u64>() {
            Ok(0) => return None,
            Ok(n) => n,
            Err(_) => DEFAULT_MS,
        },
    };
    Some(std::time::Duration::from_millis(ms))
}

/// Live read of `ws_handshake_timeout_from_raw` against the
/// process environment.
fn ws_handshake_timeout() -> Option<std::time::Duration> {
    ws_handshake_timeout_from_raw(
        std::env::var("KARAC_WS_HANDSHAKE_TIMEOUT_MS")
            .ok()
            .as_deref(),
    )
}

/// Parse `KARAC_WS_MAX_PENDING_HANDSHAKES` into the TLS handshake
/// pool's in-flight cap. **Off by default** (`None` = unbounded):
/// unset, `0`, or unparseable/negative all yield `None`; a positive
/// integer yields `Some(n)`. Off-by-default mirrors the line-124
/// connection-cap posture — a silent cap must never throttle the
/// 1M-idle Demo. Pure over the raw string for unit testing.
#[cfg(all(unix, feature = "tls"))]
fn ws_max_pending_from_raw(raw: Option<&str>) -> Option<usize> {
    match raw {
        Some(s) => match s.trim().parse::<usize>() {
            Ok(0) => None,
            Ok(n) => Some(n),
            Err(_) => None,
        },
        None => None,
    }
}

/// Live read of `ws_max_pending_from_raw` against the environment.
#[cfg(all(unix, feature = "tls"))]
fn ws_max_pending_handshakes() -> Option<usize> {
    ws_max_pending_from_raw(
        std::env::var("KARAC_WS_MAX_PENDING_HANDSHAKES")
            .ok()
            .as_deref(),
    )
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
pub extern "C" fn karac_runtime_ws_accept(listener_fd: i64) -> i64 {
    use std::os::unix::io::{FromRawFd, IntoRawFd, RawFd};

    if listener_fd < 0 {
        return -1;
    }
    // i64 fd ABI → narrow to `RawFd` (i32 on Unix); see `register_fd_impl`.
    let listener_fd = listener_fd as RawFd;

    // Accept the underlying TCP connection (same shape as
    // `karac_runtime_tcp_accept`).
    let listener = unsafe { std::net::TcpListener::from_raw_fd(listener_fd) };
    let accept_result = listener.accept();
    let _ = listener.into_raw_fd();

    let mut conn = match accept_result {
        Ok((c, _addr)) => c,
        Err(_) => return -1,
    };

    // Disable Nagle on the accepted socket — same handshake-latency fix
    // as `karac_runtime_ws_accept_tls`: the plain-TCP WS upgrade is the
    // same multi-RTT small-record exchange that Nagle×delayed-ACK
    // stalls. Best-effort.
    let _ = conn.set_nodelay(true);

    // Bound the opening-handshake read so a client that completes
    // the TCP connect but stalls (or dribbles) the HTTP upgrade
    // request can't pin this accept indefinitely (slowloris). Two
    // layers: the per-read `set_read_timeout` reaps a silently-
    // stalled peer, and `deadline` (a whole-request wall-clock
    // bound passed into the read loop) reaps a byte-per-interval
    // dribbler that would otherwise re-arm the per-read clock
    // forever. Both cover only the pre-`101` phase and are dropped
    // the instant the handshake succeeds, so an established idle
    // connection never inherits either. `set_read_timeout` failure
    // is non-fatal — fall back to the kernel's default socket
    // timeout.
    let handshake_timeout = ws_handshake_timeout();
    if handshake_timeout.is_some() {
        let _ = conn.set_read_timeout(handshake_timeout);
    }
    let deadline = handshake_timeout.map(|t| std::time::Instant::now() + t);

    // Run the shared RFC 6455 §4.2 upgrade exchange (header
    // validation + Sec-WebSocket-Accept + 101 response). Same code
    // path the WS-over-TLS handshake drives, so both transports get
    // identical validation and rejection-status behaviour.
    if ws_drive_upgrade_handshake(&mut conn, deadline).is_err() {
        return -1;
    }

    // Clear the handshake read-timeout: subsequent framed I/O on
    // this fd parks on read-readiness through the event loop and
    // must not inherit the bounded handshake deadline.
    if handshake_timeout.is_some() {
        let _ = conn.set_read_timeout(None);
    }

    // Return the connection fd; ownership of close-on-drop
    // belongs to the kara `WebSocket` value the caller
    // constructs from this fd (slice-9e.1 + slice-9d Drop chain).
    // i64 fd ABI: Unix `RawFd` (i32) sign-extends losslessly.
    conn.into_raw_fd() as i64
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

#[cfg(all(unix, feature = "tls"))]
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

#[cfg(all(unix, feature = "tls"))]
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

#[cfg(all(unix, feature = "tls"))]
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

/// A `Read + Write` adapter enforcing a whole-operation wall-clock
/// `deadline` on top of a borrowed stream's per-read `SO_RCVTIMEO`.
/// Checked before every `read`; once the deadline passes, reads
/// fail with `TimedOut` so a caller looping on the inner stream
/// (rustls `complete_io`) terminates instead of being held open by
/// a peer dribbling its `ClientHello`. Writes/flushes pass straight
/// through — the slowloris vector is read-side. (The HTTP-upgrade
/// read enforces the same deadline directly inside
/// `ws_read_http_request`, so the upgrade phase does not need this
/// wrapper.)
#[cfg(all(unix, feature = "tls"))]
struct DeadlineStream<'a, S> {
    inner: &'a mut S,
    deadline: Option<std::time::Instant>,
}

#[cfg(all(unix, feature = "tls"))]
impl<'a, S> DeadlineStream<'a, S> {
    fn new(inner: &'a mut S, deadline: Option<std::time::Instant>) -> Self {
        Self { inner, deadline }
    }
}

#[cfg(all(unix, feature = "tls"))]
impl<S: std::io::Read> std::io::Read for DeadlineStream<'_, S> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if let Some(d) = self.deadline {
            if std::time::Instant::now() >= d {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "ws handshake deadline exceeded",
                ));
            }
        }
        self.inner.read(buf)
    }
}

#[cfg(all(unix, feature = "tls"))]
impl<S: std::io::Write> std::io::Write for DeadlineStream<'_, S> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.inner.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
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
#[cfg(all(unix, feature = "tls"))]
unsafe fn ws_handshake_conn_tls(
    conn_fd: i32,
    config: *mut crate::tls::KaracTlsConfig,
) -> Result<i32, (HandshakeStep, String)> {
    use std::os::unix::io::{AsRawFd, FromRawFd, IntoRawFd};

    // Take ownership of the accepted connection fd.
    let mut sock = std::net::TcpStream::from_raw_fd(conn_fd);

    // Bound the handshake phase (TLS `complete_io` + the HTTP
    // upgrade over `TlsConnIo`, both of which read through `sock`)
    // so a peer that stalls the `ClientHello` or dribbles the
    // upgrade request can't pin a worker thread indefinitely
    // (slowloris). Two layers: the per-read `set_read_timeout`
    // reaps a silent stall, and `deadline` (a whole-request
    // wall-clock bound, enforced via `DeadlineStream` over the TLS
    // handshake and inside `ws_read_http_request` over the upgrade)
    // reaps a byte-per-interval dribbler that would re-arm the
    // per-read clock forever. Both cover only the pre-`101` phase
    // and are dropped on success before the fd is handed to the
    // kara `WebSocket`. Same `KARAC_WS_HANDSHAKE_TIMEOUT_MS` knob as
    // the plain-TCP path.
    let handshake_timeout = ws_handshake_timeout();
    if handshake_timeout.is_some() {
        let _ = sock.set_read_timeout(handshake_timeout);
    }
    let deadline = handshake_timeout.map(|t| std::time::Instant::now() + t);

    // Build a fresh ServerConnection using the borrowed config.
    let config_arc = crate::tls::clone_config_arc(config);
    let mut conn = match rustls::ServerConnection::new(config_arc) {
        Ok(c) => c,
        Err(e) => return Err((HandshakeStep::TlsConfig, format!("{e}"))),
    };

    // Drive the TLS handshake to completion against the blocking
    // socket, wrapped so the whole-request deadline also bounds a
    // dribbled `ClientHello`. complete_io loops until handshaking
    // is done.
    if let Err(e) = conn.complete_io(&mut DeadlineStream::new(&mut sock, deadline)) {
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
                let _ = crate::tls::karac_runtime_tls_close(fd as i64);
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
        ws_drive_upgrade_handshake(&mut transport, deadline)
    };

    if let Err(reason) = upgrade_outcome {
        // HTTP upgrade failed (malformed request, missing key
        // header, etc.). Pull the session out of the registry and
        // close the fd.
        let _ = sock.into_raw_fd();
        let _ = crate::tls::karac_runtime_tls_close(fd as i64);
        return Err((HandshakeStep::WsUpgrade, reason));
    }

    // Clear the handshake read-timeout before handing the fd over:
    // post-upgrade framed I/O parks on read-readiness through the
    // event loop and must not inherit the bounded handshake deadline.
    if handshake_timeout.is_some() {
        let _ = sock.set_read_timeout(None);
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
#[cfg(all(unix, feature = "tls"))]
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
    /// In-flight cap on the `work` queue (`KARAC_WS_MAX_PENDING_HANDSHAKES`,
    /// **off by default**). When `Some(cap)`, the accept-drain stops
    /// accepting once `work.len() >= cap`, leaving excess connections in
    /// the OS accept backlog (backpressure) rather than growing the queue
    /// unbounded — the line-128 residual-slowloris carve's defense against
    /// a flood saturating the fixed worker pool. Off-by-default mirrors the
    /// line-124 connection-cap disposition so the 1M-idle Demo never sees a
    /// silent cap.
    max_pending: Option<usize>,
}

/// Snapshot-able handshake-pool counters used by the once-per-second
/// reporter thread. All counters are `Relaxed` — we only need
/// monotonic visibility, not cross-thread ordering. The per-step
/// `*_failed` counters localise *where* a connection died (TLS handshake
/// vs WS-upgrade vs session-lookup); `first_errors` captures the literal
/// rustls / parse error text for the first few failures so the cause is
/// inspectable from the stats stream alone — added 2026-05-30 as step
/// (a) of the macOS bench-client TLS+WS-upgrade diagnosis plan.
#[cfg(all(unix, feature = "tls"))]
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
#[cfg(all(unix, feature = "tls"))]
const FIRST_ERRORS_CAP: usize = 32;

#[cfg(all(unix, feature = "tls"))]
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
#[cfg(all(unix, feature = "tls"))]
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
#[cfg(all(unix, feature = "tls"))]
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
#[cfg(all(unix, feature = "tls"))]
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
#[cfg(all(unix, feature = "tls"))]
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
#[cfg(all(unix, feature = "tls"))]
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
#[cfg(all(unix, feature = "tls"))]
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
        max_pending: ws_max_pending_handshakes(),
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
#[cfg(all(unix, feature = "tls"))]
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_ws_accept_tls(
    listener_fd: i64,
    config: *mut crate::tls::KaracTlsConfig,
) -> i64 {
    use std::os::unix::io::{FromRawFd, IntoRawFd, RawFd};

    if listener_fd < 0 || config.is_null() {
        return -1;
    }
    // i64 fd ABI → narrow to `RawFd` (i32 on Unix) for the internal
    // handshake-pool keying + `std::net` wrappers; see `register_fd_impl`.
    // The conn fd produced by the pool is narrowed too and re-widened to
    // i64 only at this public return boundary.
    let listener_fd = listener_fd as RawFd;

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
            // Drain the backlog: accept until WouldBlock (or a real error),
            // or until the in-flight cap is reached.
            loop {
                // Backpressure (line-128 residual-slowloris carve): when a
                // cap is configured, stop draining once the pending `work`
                // queue is full so a connection flood can't grow it
                // unbounded behind the fixed worker pool. Excess connections
                // stay in the OS accept backlog and are picked up on a later
                // tick as workers drain the queue. Off by default → this
                // check is skipped and behaviour is unchanged.
                if let Some(cap) = pool.max_pending {
                    let pending = pool.work.lock().unwrap_or_else(|p| p.into_inner()).len();
                    if pending >= cap {
                        break;
                    }
                }
                let (sock, _addr) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(_) => break,
                };
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
                // Disable Nagle on the accepted socket. The TLS handshake
                // + RFC 6455 upgrade is a multi-round-trip exchange of
                // small records; with Nagle on, a server record can sit
                // withheld behind an unacked prior segment until the
                // peer's ~40 ms delayed-ACK timer fires — the classic
                // fixed handshake-latency floor. Every other comparator
                // in the bench (Go forces it runtime-wide, .NET/Node/
                // Phoenix via their stacks) runs nodelay; Kāra omitted
                // it. Best-effort: a failure here only forgoes the
                // latency win, never breaks the connection.
                let _ = sock.set_nodelay(true);
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
            return fd as i64;
        }
        // No completed handshake yet — wait briefly, then loop back to
        // re-drain the backlog and re-check.
        let (mut g, _timeout) = pool
            .cv_done
            .wait_timeout(d, Duration::from_millis(5))
            .unwrap_or_else(|p| p.into_inner());
        if let Some(fd) = g.pop_front() {
            return fd as i64;
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
/// Shared by both [`karac_runtime_ws_accept`] (plain TCP) and
/// [`karac_runtime_ws_accept_tls`] (TLS via `TlsConnIo`), so the
/// two transports run identical RFC 6455 §4.2.1 validation and
/// rejection-status behaviour. Generic over `Read + Write` with no
/// TLS types, so it compiles without the `tls` feature.
#[cfg(unix)]
fn ws_drive_upgrade_handshake<S: std::io::Read + std::io::Write>(
    stream: &mut S,
    deadline: Option<std::time::Instant>,
) -> Result<(), String> {
    let request = match ws_read_http_request(stream, deadline) {
        Some(b) => b,
        None => return Err("ws_read_http_request: IO error or EOF before complete request".into()),
    };
    // RFC 6455 §4.2.1 validation: require Upgrade/Connection/Version
    // before the key, and answer a bad request with the matching
    // HTTP status (400 / 426) rather than upgrading anyway.
    let key = match ws_validate_handshake(&request) {
        Ok(k) => k,
        Err(reject) => {
            let _ = stream.write_all(reject.response_bytes());
            let preview = String::from_utf8_lossy(&request[..request.len().min(256)])
                .replace('\r', "\\r")
                .replace('\n', "\\n");
            return Err(format!(
                "handshake rejected ({}) in {}B request; preview={preview:?}",
                reject.log_reason(),
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

// ── Scheduler dispatcher (Phase 6 line 17 slice 4; Stage B3 combined model) ──
//
// **Combined poll-and-dispatch, one thread per reactor shard.** Each
// dispatcher thread owns a shard: it blocks in that shard's
// `EventLoop::run_once`, and for every readiness wakeup resolves the parked
// task from *its own* shard `fds` map and invokes
// `(task.poll_fn)(task.state, cancel)` inline — no shared queue, no condvar
// handoff. The `parked` field is interpreted as `*const KaracParkedTask`, the
// codegen convention (phase-6 line 18).
//
// **Why combined (Stage B3).** B2 sharded the *polling* but kept a single
// shared wakeup queue feeding a separate dispatcher pool. A CPU-isolated
// re-measure (server 0.28/8 cores used under a 10K synchronized burst, p50
// ~24 ms, flat across shard AND dispatcher counts) showed the serializer was
// that shared queue + its serial condvar wake-a-friend ramp: neither more
// pollers nor more dispatchers fed the idle cores. Fusing poll and dispatch
// per shard removes the handoff entirely — each shard drains its own ready fds
// in parallel, and the thread that polled an fd runs its `poll_fn` with a warm
// cache. See `wip-bench-day.md` Phase 2 Stage B. (The standalone background
// poller + `take_wakeups` queue path is retained, unchanged, for the
// test/embedding surface; it is not on the dispatcher's path.)
//
// **Polls its own shards — does NOT use the background poller.** Unlike B2,
// `start_dispatcher` does not start the background poller (the two would
// double-poll the same epolls and contend on each shard's poll Mutex). The
// combined threads ARE the pollers while the dispatcher runs.
//
// **One thread per shard.** The shard count (`resolve_shard_count`) fixes the
// thread count: you cannot have two threads block on one `mio::Poll`. Tune via
// `KARAC_REACTOR_SHARDS` (or the back-compat `KARAC_DISPATCHER_THREADS`).
//
// **Cancel routing.** The dispatcher passes each `poll_fn` the task's
// *own* `cancel` flag, bound on the registration (`FdState.cancel`,
// via `register_fd_cancel`) — null falls back to the process-global
// `disp.cancel` (still "never cancelled"). This is what makes
// `TaskGroup.cancel()` reach a parked coroutine (slice 5c). The FFI
// surface stays stable because the cancel pointer rides on the
// registration, not the dispatcher's signature.
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
    /// Process-global "never cancelled" flag — the fallback the dispatcher
    /// passes to `poll_fn` when a registration carries no per-task cancel
    /// flag (`FdState.cancel` null: the inline / non-spawn drive and the
    /// degenerate state-machine path). A `spawn_coro` coroutine instead
    /// gets its own handle's flag, routed via `register_fd_cancel` (slice 5c).
    cancel: AtomicBool,
    /// Per-shard "a cancel-sweep is requested" flags — one `AtomicBool` per
    /// reactor shard (indexed by `shard`). Set (all shards) by
    /// [`karac_runtime_request_cancel_sweep`] after `TaskGroup.cancel()`
    /// flips child flags, then the cross-thread waker unblocks each shard's
    /// `run_once` so it observes its flag and sweeps. Per-shard (not one
    /// global bool) so the first shard to `swap(false)` doesn't starve the
    /// others — each shard independently consumes its own request.
    sweep_requested: Vec<AtomicBool>,
    /// Counters for test verification + diagnostics. Updated unsynchronized
    /// (Relaxed) — they only need monotonic-write visibility, not strict
    /// ordering against other operations.
    polls: std::sync::atomic::AtomicU64,
    ready_observations: std::sync::atomic::AtomicU64,
    err_observations: std::sync::atomic::AtomicU64,
    pending_observations: std::sync::atomic::AtomicU64,
    /// Join handles for the combined poll-and-dispatch threads — one per
    /// reactor shard. Each polls its shard's epoll and invokes `poll_fn` on
    /// the shard's own claimed tasks, so concurrent connections are serviced
    /// across cores with no shared queue. The loop body is concurrency-safe by
    /// construction (the one-shot `take_registration` claim guarantees each
    /// wakeup's `poll_fn` runs at most once, counters are atomic, per-conn TLS
    /// sessions are independently locked).
    handles: Mutex<Vec<thread::JoinHandle<()>>>,
}

static SCHEDULER_DISPATCHER: Mutex<Option<Arc<SchedulerDispatcher>>> = Mutex::new(None);

fn lock_scheduler_dispatcher_slot(
) -> std::sync::MutexGuard<'static, Option<Arc<SchedulerDispatcher>>> {
    SCHEDULER_DISPATCHER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn dispatcher_thread_main(disp: Arc<SchedulerDispatcher>, shard: usize) {
    let event_loop = Arc::clone(&event_loops()[shard]);
    while !disp.shutdown.load(Ordering::Acquire) {
        // Block on THIS shard's epoll and dispatch its ready fds inline — no
        // shared queue, no condvar handoff. `run_once(None)` returns as soon as
        // ≥1 fd is ready (so burst latency is unaffected by any timeout) or
        // when `shutdown_dispatcher` wakes every shard to unblock it.
        let wakeups = match event_loop.run_once(None) {
            Ok(w) => w,
            Err(_) => {
                // Transient poll error — re-check shutdown and continue.
                continue;
            }
        };
        for w in wakeups {
            // One-shot claim from this shard's OWN map (the combined thread
            // knows its shard, so `w.token` is the shard-local `mio::Token` —
            // no packing/routing needed). Resolving from the live map (removing
            // the token) is what makes dispatch safe against (a) duplicate /
            // stale wakeups for the same fd (the second `take` returns `None` →
            // skip, so a task's `poll_fn` runs at most once per registration)
            // and (b) concurrent frees: the caller frees its parked record only
            // after `poll_fn` signals it, strictly after this `take`+invoke, so
            // the pointer is live for the duration of the call. `None` ⇒ the
            // registration was already claimed or deregistered — skip it.
            let (parked, task_cancel) = match event_loop.take_registration_with_cancel(w.token) {
                Some((p, c)) if !p.is_null() => (p, c),
                _ => continue,
            };
            // SAFETY: `take_registration` returned the parked pointer from the
            // live map; the codegen / caller convention keeps the
            // `KaracParkedTask` (and its state) alive until `poll_fn` signals
            // completion, which happens inside this call. We invoke `poll_fn`
            // but never deref `state` ourselves.
            let task = unsafe { &*(parked as *const KaracParkedTask) };
            // Per-task cancel routing: hand the coroutine *its own* cancel
            // flag (bound on its registration via `register_with_cancel`), so
            // its resume shim observes cancellation. Null when unbound (the
            // inline / non-spawn drive, and the degenerate state-machine path)
            // → fall back to the dispatcher's never-cancelled flag, preserving
            // the pre-routing behavior.
            let cancel: &AtomicBool = if task_cancel.is_null() {
                &disp.cancel
            } else {
                // SAFETY: a non-null `task_cancel` points at the owning
                // handle's `cancel` AtomicBool, which outlives the coroutine
                // (freed by the joiner only after the coroutine completes /
                // tears down — strictly after this `poll_fn` returns).
                unsafe { &*task_cancel }
            };
            let result = unsafe { (task.poll_fn)(task.state, cancel) };
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
                    // Unknown discriminant — treat as Err for accounting.
                    disp.err_observations.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        // Drain a cancel-sweep request for THIS shard. Clear-before-snapshot:
        // `swap(false)` precedes the sweep, so a `cancel` flag flipped during
        // the sweep re-arms the *next* iteration (its `request_cancel_sweep`
        // re-sets this flag + re-wakes) rather than being lost. The sweep
        // proactively tears down cancelled coroutines parked on idle fds that
        // would otherwise never produce a readiness wakeup — without it,
        // `TaskGroup.cancel()` + group-drop/join would hang on an idle peer.
        if disp.sweep_requested[shard].swap(false, Ordering::AcqRel) {
            event_loop.sweep_cancelled(&disp);
        }
    }
}

/// Start the combined scheduler dispatcher — one poll-and-dispatch thread per
/// reactor shard ([`resolve_shard_count`]).
///
/// Does NOT start the background poller: the combined threads own polling
/// while the dispatcher runs (starting both would double-poll the shards). The
/// background poller / `take_wakeups` path remains available for standalone
/// use, just not on this path. Idempotent: a second call while running returns
/// 0 without re-spawning.
///
/// Returns 0 on success.
#[no_mangle]
pub extern "C" fn karac_runtime_scheduler_start_dispatcher() -> i32 {
    let mut slot = lock_scheduler_dispatcher_slot();
    if slot.is_some() {
        return 0;
    }
    let n_threads = shard_count();
    let disp = Arc::new(SchedulerDispatcher {
        shutdown: AtomicBool::new(false),
        cancel: AtomicBool::new(false),
        sweep_requested: (0..n_threads).map(|_| AtomicBool::new(false)).collect(),
        polls: std::sync::atomic::AtomicU64::new(0),
        ready_observations: std::sync::atomic::AtomicU64::new(0),
        err_observations: std::sync::atomic::AtomicU64::new(0),
        pending_observations: std::sync::atomic::AtomicU64::new(0),
        handles: Mutex::new(Vec::new()),
    });
    let mut handles = Vec::with_capacity(n_threads);
    // Publish BEFORE spawning so a `register_fd` racing a thread into its first
    // blocking `run_once` still wakes it (see `DISPATCHER_ACTIVE`).
    DISPATCHER_ACTIVE.store(true, Ordering::Release);
    for shard in 0..n_threads {
        let disp_for_thread = Arc::clone(&disp);
        let join = thread::Builder::new()
            .name(format!("karac-reactor-{shard}"))
            .spawn(move || dispatcher_thread_main(disp_for_thread, shard))
            .expect("karac_runtime: failed to spawn scheduler dispatcher thread");
        handles.push(join);
    }
    *disp.handles.lock().unwrap_or_else(|p| p.into_inner()) = handles;
    *slot = Some(disp);
    0
}

/// Request a cancel-sweep on every reactor shard and wake them. Called by
/// `karac_runtime_taskgroup_cancel` *after* it flips the child handles'
/// `cancel` flags, so the Release ordering here publishes those flips to the
/// sweeping dispatcher (which consumes the request with an `AcqRel` swap).
///
/// Sets all shards because a group's children may be parked on any shard. A
/// flip that lands after a given sweep's snapshot is not lost: every flip is
/// paired with this call, which re-sets the flags and re-wakes, so a fresh
/// sweep is guaranteed after the last flip. No-op (cheap) when the dispatcher
/// isn't running — cancellation of parked coroutines is a dispatcher-mode
/// feature; the standalone-poller path does not sweep.
///
/// Returns 0 on success (including the no-dispatcher no-op).
#[no_mangle]
pub extern "C" fn karac_runtime_request_cancel_sweep() -> i32 {
    if let Some(disp) = lock_scheduler_dispatcher_slot().as_ref() {
        for f in &disp.sweep_requested {
            f.store(true, Ordering::Release);
        }
    }
    // Fan out the waker to all shards so each unblocks `run_once` and observes
    // its request. Harmless if no dispatcher is running (no thread is parked
    // in `run_once`); the armed waker is drained on next poll / shutdown.
    karac_runtime_event_loop_wake()
}

/// Signal the dispatcher to stop, unblock every shard's `run_once` via the
/// cross-thread waker, join the threads, clear the global slot. Returns 0 on
/// success, -1 if no dispatcher is running.
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
    // The combined threads block in `run_once(None)`; wake every shard so each
    // observes the shutdown flag and exits. `wake()` fans out to all shards.
    let _ = karac_runtime_event_loop_wake();
    let joins: Vec<_> =
        std::mem::take(&mut *disp.handles.lock().unwrap_or_else(|p| p.into_inner()));
    for h in joins {
        let _ = h.join();
    }
    // Cleared after join so a concurrent `register_fd` never observes `false`
    // while a combined thread is still blocked in `run_once`.
    DISPATCHER_ACTIVE.store(false, Ordering::Release);
    // Drain any pending waker event each shard's exit left armed (mio's
    // edge-armed waker), so a follow-up direct poll / re-start sees a clean
    // loop; real-fd readiness swept up alongside the waker event is stashed
    // for redelivery, not discarded (see `drain_for_shutdown`). The
    // dispatcher slot is already None, so these don't race a live combined
    // thread.
    for ev in event_loops() {
        ev.drain_for_shutdown();
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
    /// Per-task cancel flag, bound by `karac_runtime_spawn_coro` to the
    /// owning `KaracTaskHandle`'s `cancel: AtomicBool`. Null until bound
    /// (the inline / non-spawn park drive never binds it — its coroutine
    /// observes the dispatcher's never-cancelled fallback instead). The
    /// codegen park-suspend reads this through
    /// [`karac_runtime_park_slot_cancel_ptr`] and stores it in the parked
    /// record's `cancel` field, so the dispatcher (and the cancel-sweep)
    /// can hand the coroutine its own flag. `AtomicPtr` (not a plain raw
    /// field) keeps `KaracParkSlot` `Sync` and lets the bind store through
    /// a shared `&KaracParkSlot`.
    cancel: AtomicPtr<AtomicBool>,
}

/// Allocate a fresh park slot. Returns an owning raw pointer the caller
/// must release exactly once via [`karac_runtime_park_slot_free`].
#[no_mangle]
pub extern "C" fn karac_runtime_park_slot_new() -> *mut KaracParkSlot {
    Box::into_raw(Box::new(KaracParkSlot {
        done: Mutex::new(false),
        cv: Condvar::new(),
        cancel: AtomicPtr::new(std::ptr::null_mut()),
    }))
}

/// Bind the slot's per-task cancel flag. Called by
/// [`karac_runtime_spawn_coro`] with the owning handle's `cancel` flag
/// before the coroutine ramps. Storing through the `AtomicPtr` is safe
/// against the shared `&KaracParkSlot` that `wait` / `signal` later take.
///
/// # Safety
///
/// `slot` must be a live pointer from [`karac_runtime_park_slot_new`];
/// `cancel` must outlive the coroutine (the handle is freed only by the
/// joiner, strictly after the coroutine completes / is torn down).
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_park_slot_bind_cancel(
    slot: *mut KaracParkSlot,
    cancel: *const AtomicBool,
) {
    if slot.is_null() {
        return;
    }
    // SAFETY: caller's contract — `slot` is live.
    let s = unsafe { &*slot };
    s.cancel.store(cancel as *mut AtomicBool, Ordering::Release);
}

/// Read the slot's bound per-task cancel flag (null if unbound). The
/// codegen park-suspend calls this to populate the parked record's
/// `cancel` field; the dispatcher then hands it to `poll_fn`.
///
/// # Safety
///
/// `slot` must be a live pointer from [`karac_runtime_park_slot_new`].
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_park_slot_cancel_ptr(
    slot: *const KaracParkSlot,
) -> *const AtomicBool {
    if slot.is_null() {
        return std::ptr::null();
    }
    // SAFETY: caller's contract — `slot` is live.
    let s = unsafe { &*slot };
    s.cancel.load(Ordering::Acquire) as *const AtomicBool
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

    /// Regression: the reactor's one-time init must mask SIGPIPE. A
    /// karac-compiled `main` bypasses std's `lang_start`, so without this
    /// the default SIGPIPE disposition (terminate) survives, and a socket
    /// write racing a peer close kills the server silently (observed: a
    /// cross-box reconnect storm at concurrency 4000 terminated the demo
    /// server with exit 141 = 128 + SIGPIPE, no core, no stderr). Asserts
    /// the disposition is `SIG_IGN` after the reactor has been touched.
    #[cfg(unix)]
    #[test]
    fn reactor_init_masks_sigpipe() {
        let _g = ffi_test_guard();
        // Touch the reactor — triggers the one-time init that installs the
        // SIG_IGN. Idempotent; safe even if another test got here first.
        let _ = event_loops();
        unsafe {
            let mut cur: libc::sigaction = std::mem::zeroed();
            assert_eq!(
                libc::sigaction(libc::SIGPIPE, std::ptr::null(), &mut cur),
                0,
                "sigaction query failed"
            );
            assert_eq!(
                cur.sa_sigaction,
                libc::SIG_IGN,
                "reactor init must leave SIGPIPE masked (SIG_IGN)"
            );
        }
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

    // ── A2a-1: timer wheel ─────────────────────────────────────────

    /// A registered timer fires on or after its deadline, surfacing a
    /// `Wakeup` that carries the exact `parked` pointer and token, and
    /// no earlier than the deadline.
    #[test]
    fn timer_fires_at_deadline_carrying_parked() {
        let ev = EventLoop::new().unwrap();
        let parked = 0xA11CE_usize as *mut c_void;
        let tok = ev.register_timer(
            Instant::now() + Duration::from_millis(40),
            parked,
            std::ptr::null(),
        );

        let start = Instant::now();
        let mut got: Option<Wakeup> = None;
        // Loop: the register-time waker makes the first poll return empty;
        // the timer fires on a later pass once the deadline elapses. The 2s
        // outer bound keeps a regression from hanging the suite.
        while start.elapsed() < Duration::from_secs(2) {
            let wakeups = ev.run_once(Some(Duration::from_millis(100))).unwrap();
            if let Some(w) = wakeups.into_iter().next() {
                got = Some(w);
                break;
            }
        }
        let elapsed = start.elapsed();
        let w = got.expect("timer must fire within 2s");
        assert_eq!(w.parked as usize, 0xA11CE, "wakeup carries the parked ptr");
        assert_eq!(w.token.0, tok.0, "wakeup carries the timer's token");
        assert!(
            elapsed >= Duration::from_millis(35),
            "timer fired early ({elapsed:?} < ~40ms deadline)"
        );
        assert!(
            elapsed < Duration::from_secs(1),
            "timer fired far too late: {elapsed:?}"
        );
    }

    /// The core fix: a timer must cap an otherwise-infinite `run_once(None)`
    /// so a task parked on a deadline with no fd activity still wakes. Runs
    /// the blocking poll on a watchdog thread — a broken cap blocks forever,
    /// which this surfaces as a `recv_timeout` failure instead of a hang.
    #[test]
    fn timer_caps_blocking_poll_none() {
        let ev = std::sync::Arc::new(EventLoop::new().unwrap());
        ev.register_timer(
            Instant::now() + Duration::from_millis(40),
            0xBEEF_usize as *mut c_void,
            std::ptr::null(),
        );
        let ev2 = std::sync::Arc::clone(&ev);
        let (tx, rx) = std::sync::mpsc::channel();
        let h = thread::spawn(move || loop {
            let wakeups = ev2.run_once(None).unwrap();
            if !wakeups.is_empty() {
                let _ = tx.send(wakeups[0].parked as usize);
                return;
            }
        });
        let got = rx.recv_timeout(Duration::from_secs(2));
        assert_eq!(
            got,
            Ok(0xBEEF),
            "run_once(None) with a pending timer must wake on the deadline, not block forever"
        );
        h.join().unwrap();
    }

    /// An already-past deadline fires on the very next poll (no blocking).
    #[test]
    fn past_due_timer_fires_immediately() {
        let ev = EventLoop::new().unwrap();
        let tok = ev.register_timer(
            Instant::now() - Duration::from_millis(5),
            0xDEAD_usize as *mut c_void,
            std::ptr::null(),
        );
        let start = Instant::now();
        let wakeups = ev.run_once(Some(Duration::from_secs(2))).unwrap();
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "past-due timer must not block"
        );
        assert_eq!(wakeups.len(), 1);
        assert_eq!(wakeups[0].token.0, tok.0);
    }

    /// Cancelling a timer (claiming its registration before it fires) must
    /// drop it: the stale heap entry is filtered at pop, never fires.
    #[test]
    fn cancelled_timer_never_fires() {
        let ev = EventLoop::new().unwrap();
        let tok = ev.register_timer(
            Instant::now() + Duration::from_millis(30),
            0xC0FFEE_usize as *mut c_void,
            std::ptr::null(),
        );
        // Cancel by claiming the by_token entry (the dispatcher-uniform path).
        let claimed = ev.take_registration(RegistrationToken(tok.0));
        assert_eq!(claimed, Some(0xC0FFEE_usize as *mut c_void));

        let start = Instant::now();
        while start.elapsed() < Duration::from_millis(150) {
            let wakeups = ev.run_once(Some(Duration::from_millis(40))).unwrap();
            assert!(
                wakeups.is_empty(),
                "cancelled timer must not fire, got {wakeups:?}"
            );
        }
    }

    /// With two timers, the earlier deadline fires first.
    #[test]
    fn earliest_deadline_fires_first() {
        let ev = EventLoop::new().unwrap();
        let far = ev.register_timer(
            Instant::now() + Duration::from_millis(800),
            0x0F_usize as *mut c_void,
            std::ptr::null(),
        );
        let near = ev.register_timer(
            Instant::now() + Duration::from_millis(40),
            0x0E_usize as *mut c_void,
            std::ptr::null(),
        );
        let start = Instant::now();
        let mut first: Option<Wakeup> = None;
        while start.elapsed() < Duration::from_secs(2) {
            let wakeups = ev.run_once(Some(Duration::from_millis(100))).unwrap();
            if let Some(w) = wakeups.into_iter().next() {
                first = Some(w);
                break;
            }
        }
        let w = first.expect("near timer must fire");
        assert_eq!(w.token.0, near.0, "earlier deadline must fire first");
        assert_ne!(w.token.0, far.0);
    }

    /// A fired timer is claimed exactly once via `take_registration` — the
    /// dispatcher path is identical to an fd wakeup.
    #[test]
    fn fired_timer_claimed_once() {
        let ev = EventLoop::new().unwrap();
        let tok = ev.register_timer(
            Instant::now() + Duration::from_millis(30),
            0x5107_usize as *mut c_void,
            std::ptr::null(),
        );
        let start = Instant::now();
        let mut fired = false;
        while start.elapsed() < Duration::from_secs(2) {
            if !ev
                .run_once(Some(Duration::from_millis(60)))
                .unwrap()
                .is_empty()
            {
                fired = true;
                break;
            }
        }
        assert!(fired, "timer must fire");
        assert_eq!(
            ev.take_registration(RegistrationToken(tok.0)),
            Some(0x5107_usize as *mut c_void),
            "dispatcher claims the parked ptr"
        );
        assert_eq!(
            ev.take_registration(RegistrationToken(tok.0)),
            None,
            "second claim is a no-op (one-shot)"
        );
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

    /// Regression (bugs.md, 2026-06-06): a shutdown drain must not eat
    /// real fd readiness. mio registrations are edge-triggered, so the
    /// pre-fix `let _ = ev.run_once(Some(ZERO))` drain consumed — and
    /// discarded — any real-fd event that fired in the shutdown window;
    /// a follow-up poll never saw that readiness again and the parked
    /// task wedged. `drain_for_shutdown` stashes real wakeups for the
    /// next `run_once` to deliver. A pre-armed waker event is still
    /// consumed silently (the drain's original purpose) — covered by
    /// the sibling test below. A byte written to a `socketpair(2)` end
    /// makes the registered peer readable synchronously, so the single
    /// non-blocking drain deterministically sweeps the real event.
    #[cfg(unix)]
    #[test]
    fn shutdown_drain_stashes_real_fd_readiness_for_next_poll() {
        use std::io::Write;
        use std::os::unix::io::AsRawFd;

        let (mut writer, reader) = std::os::unix::net::UnixStream::pair().unwrap();
        let ev = EventLoop::new().unwrap();

        let marker: u64 = 0xFEED_FACE_0BAD_F00D;
        let parked = std::ptr::addr_of!(marker) as *mut c_void;
        let raw = reader.as_raw_fd();
        let mut source = mio::unix::SourceFd(&raw);
        let token = ev
            .register(&mut source, IoDirection::Read, None, parked)
            .unwrap();

        // Make the registered end readable, then drain as the shutdown
        // paths do. The drain must sweep the event (edge consumed) and
        // stash it rather than discard it. Drain twice: a second drain's
        // inner `run_once` takes the stash and must re-stash it, so a
        // double shutdown (poller then dispatcher) loses nothing either.
        writer.write_all(&[1]).unwrap();
        ev.drain_for_shutdown();
        ev.drain_for_shutdown();

        // A blocking follow-up poll must surface the stashed wakeup
        // immediately — both the delivery and the don't-block-while-
        // holding-deliverable-readiness conversion.
        let start = Instant::now();
        let wakeups = ev.run_once(Some(Duration::from_secs(2))).unwrap();
        let elapsed = start.elapsed();
        assert_eq!(
            wakeups.len(),
            1,
            "the drained real-fd readiness must be redelivered, not lost"
        );
        assert_eq!(wakeups[0].token, token);
        assert_eq!(wakeups[0].parked, parked);
        assert_eq!(wakeups[0].direction, IoDirection::Read);
        assert!(
            elapsed < Duration::from_secs(1),
            "run_once with a non-empty stash must not block, took {elapsed:?}"
        );

        // Stash is one-shot: redelivery happened, nothing left behind.
        let again = ev.run_once(Some(Duration::ZERO)).unwrap();
        assert!(
            again.is_empty(),
            "stash must be cleared after redelivery, got {again:?}"
        );

        ev.deregister(&mut source, token).unwrap();
    }

    /// Sibling to the above: the drain's original purpose — consuming a
    /// pending edge-armed *waker* event so a follow-up poll doesn't see
    /// a spurious wakeup — still holds. Waker events are filtered, never
    /// stashed.
    #[test]
    fn shutdown_drain_still_consumes_waker_event_silently() {
        let ev = EventLoop::new().unwrap();
        ev.handle().wake().unwrap();
        ev.drain_for_shutdown();
        let wakeups = ev.run_once(Some(Duration::ZERO)).unwrap();
        assert!(
            wakeups.is_empty(),
            "a drained waker event must not be stashed or redelivered, got {wakeups:?}"
        );
    }

    /// Combined window: waker armed AND a real fd ready when the drain
    /// runs — the exact shutdown-window shape from bugs.md. The waker
    /// event is eaten; the real readiness survives to the next poll.
    #[cfg(unix)]
    #[test]
    fn shutdown_drain_separates_waker_from_real_readiness() {
        use std::io::Write;
        use std::os::unix::io::AsRawFd;

        let (mut writer, reader) = std::os::unix::net::UnixStream::pair().unwrap();
        let ev = EventLoop::new().unwrap();

        let raw = reader.as_raw_fd();
        let mut source = mio::unix::SourceFd(&raw);
        let token = ev
            .register(&mut source, IoDirection::Read, None, std::ptr::null_mut())
            .unwrap();

        writer.write_all(&[1]).unwrap();
        ev.handle().wake().unwrap();
        ev.drain_for_shutdown();

        let wakeups = ev.run_once(Some(Duration::ZERO)).unwrap();
        assert_eq!(
            wakeups.len(),
            1,
            "exactly the real-fd wakeup must survive the drain, got {wakeups:?}"
        );
        assert_eq!(wakeups[0].token, token);

        ev.deregister(&mut source, token).unwrap();
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

        let listener =
            unsafe { std::net::TcpListener::from_raw_fd(fd as std::os::unix::io::RawFd) };
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
            drop(std::net::TcpStream::from_raw_fd(
                fd as std::os::unix::io::RawFd,
            ));
        }

        // Closed port → ConnectionRefused stable code (-3) since the
        // line-74 enrichment. (`net_construct_error_codes_surface_from_
        // real_syscalls` is the dedicated pin; this asserts it here too so
        // the connect path's failure classification stays covered.)
        let dead = b"127.0.0.1:1";
        let dead_fd = unsafe { karac_runtime_tcp_connect(dead.as_ptr(), dead.len() as i64) };
        assert_eq!(
            dead_fd, -3,
            "connect to a closed port should classify ConnectionRefused (-3), got {dead_fd}"
        );

        // Malformed address → -1 (parse failure, no OS error to classify).
        let bad = b"not an address";
        let bad_fd = unsafe { karac_runtime_tcp_connect(bad.as_ptr(), bad.len() as i64) };
        assert_eq!(
            bad_fd, -1,
            "connect to an unparseable address should return -1"
        );
    }

    /// `net_construct_error_code` maps platform-normalized
    /// `io::ErrorKind`s to the stable codes codegen decodes (phase-8
    /// line 74). Pins the contract the codegen-side `build_fd_construct_
    /// result` select chain mirrors.
    #[test]
    fn net_construct_error_code_maps_io_error_kinds() {
        use std::io::{Error, ErrorKind};
        assert_eq!(
            net_construct_error_code(&Error::from(ErrorKind::AddrInUse)),
            -2
        );
        assert_eq!(
            net_construct_error_code(&Error::from(ErrorKind::ConnectionRefused)),
            -3
        );
        assert_eq!(
            net_construct_error_code(&Error::from(ErrorKind::PermissionDenied)),
            -4
        );
        // Any other cause → -1 (decoded as the default `Other` variant).
        assert_eq!(
            net_construct_error_code(&Error::from(ErrorKind::TimedOut)),
            -1
        );
        assert_eq!(
            net_construct_error_code(&Error::from(ErrorKind::ConnectionReset)),
            -1
        );
    }

    /// End-to-end: `karac_runtime_tcp_connect` to a closed port returns
    /// the `ConnectionRefused` stable code (-3), and a live bind +
    /// rebind on the same port returns the `AddrInUse` code (-2). Pins
    /// the runtime half of line 74's cause classification.
    #[cfg(unix)]
    #[test]
    fn net_construct_error_codes_surface_from_real_syscalls() {
        // Closed port → ConnectionRefused (-3).
        let dead = b"127.0.0.1:1";
        let code = unsafe { karac_runtime_tcp_connect(dead.as_ptr(), dead.len() as i64) };
        assert_eq!(
            code, -3,
            "connect to closed port should classify ConnectionRefused (-3)"
        );

        // Occupy a port, then bind the same port again → AddrInUse (-2).
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").expect("harness listener bind failed");
        let port = listener.local_addr().expect("local_addr").port();
        let addr = format!("127.0.0.1:{port}");
        let code = unsafe { karac_runtime_tcp_bind(addr.as_ptr(), addr.len() as i64) };
        assert_eq!(
            code, -2,
            "rebind of an in-use port should classify AddrInUse (-2)"
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

    // ── Reactor sharding (Stage B2) ───────────────────────────────────

    #[test]
    fn token_pack_unpack_round_trips() {
        // Boundary shards (0, max) and local ids round-trip exactly.
        for &shard in &[0usize, 1, 7, MAX_SHARDS - 1] {
            for &local in &[1usize, 2, 1234, TOKEN_LOCAL_MASK as usize] {
                let packed = pack_token(shard, local);
                assert_eq!(
                    unpack_token(packed),
                    (shard, local),
                    "round-trip {shard}/{local}"
                );
            }
        }
    }

    #[test]
    fn token_pack_keeps_local_and_shard_in_disjoint_bit_ranges() {
        // Same local id on different shards yields distinct tokens (the
        // property `register_fd` relies on so cross-shard registrations never
        // collide), and the low bits recover the shard-local `mio::Token`.
        let a = pack_token(0, 5);
        let b = pack_token(3, 5);
        assert_ne!(
            a, b,
            "same local id, different shard → distinct packed token"
        );
        assert_eq!(unpack_token(a).1, 5);
        assert_eq!(unpack_token(b).1, 5);
        assert_eq!(unpack_token(b).0, 3);
        // A shard-0 local token equals its bare local value (no high bits set).
        assert_eq!(pack_token(0, 42), 42);
    }

    #[test]
    fn shard_of_fd_is_in_range_and_deterministic() {
        let n = shard_count();
        assert!(n >= 1);
        for fd in [0i32, 1, 2, 7, 255, 4096, i32::MAX] {
            let s = shard_of_fd(fd);
            assert!(s < n, "fd {fd} → shard {s} must be < {n}");
            assert_eq!(s, shard_of_fd(fd), "routing is deterministic");
        }
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
        let raw_fd = std_listener.as_raw_fd() as i64;

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

        // Wake is callable and fans out to every shard; coalesces with any
        // pending wake. (Under Stage B2 sharding a bare `wake()` has no fd
        // context, so it targets all reactors — see `karac_runtime_event_loop_wake`.)
        let wake = karac_runtime_event_loop_wake();
        assert_eq!(wake, 0, "wake should report success");

        // A non-blocking poll after the wake returns 0 immediately: the waker
        // event is filtered out at the `EventLoop` layer, leaving no
        // user-facing wakeup. (Pre-B2 this asserted that a bare `wake()`
        // unblocks an *indefinite* direct poll; that semantic does not
        // generalize across N independent epolls — one thread cannot block-wait
        // on every shard at once — and the direct-poll path is a test/embedding
        // fallback, not a production path. Production drains via the background
        // poller + dispatcher, where each shard's poller observes its own
        // waker.)
        let start = Instant::now();
        let n2 = unsafe { karac_runtime_event_loop_poll(0, buf.as_mut_ptr(), buf.len()) };
        let elapsed = start.elapsed();
        assert_eq!(n2, 0, "wake event filtered → empty wakeups");
        assert!(
            elapsed < Duration::from_millis(100),
            "non-blocking poll after wake should return immediately, took {elapsed:?}"
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

    /// Regression (Stage B2, CI run 27048824708): the synchronous poll
    /// fallback's *blocking* slice used to discard the wakeups its
    /// `run_once` returned. mio registrations are edge-triggered, so
    /// readiness that fired while the fd's own shard was the one blocking
    /// was consumed and lost — `poll` then waited out its full deadline
    /// and returned 0 (the `ffi_round_trip_register_poll_deregister_wake`
    /// failure mode; 1-in-shard-count odds per run, which is why ubuntu's
    /// 4-core runners hit it and 18-core dev machines rarely did).
    ///
    /// Drive `poll_shards` against a private single-shard loop with the
    /// connect delayed past the first non-blocking sweep, so the readiness
    /// can *only* be observed by a blocking slice — deterministic
    /// regardless of machine core count, no FFI guard needed.
    #[test]
    fn poll_blocking_slice_delivers_wakeups_observed_mid_slice() {
        let bind_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let mut listener = TcpListener::bind(bind_addr).unwrap();
        let local = listener.local_addr().unwrap();

        let ev = Arc::new(EventLoop::new().unwrap());
        let marker: u64 = 0xB10C_51CE_DE11_4E25;
        let parked = std::ptr::addr_of!(marker) as *mut c_void;
        let token = ev
            .register(&mut listener, IoDirection::Read, None, parked)
            .unwrap();

        // Fire readiness ~60ms in: the first non-blocking sweep (t≈0)
        // sees nothing, so only a 20ms blocking slice can observe it.
        let connector = thread::spawn(move || {
            thread::sleep(Duration::from_millis(60));
            let _stream = std::net::TcpStream::connect(local).unwrap();
            thread::sleep(Duration::from_millis(50));
        });

        let loops = [Arc::clone(&ev)];
        let mut buf: [KaracWakeup; 4] = std::array::from_fn(|_| KaracWakeup {
            token: 0,
            parked: std::ptr::null_mut(),
            direction: 0,
        });
        let n = unsafe {
            poll_shards(
                &loops,
                Some(Duration::from_secs(2)),
                buf.as_mut_ptr(),
                buf.len(),
            )
        };
        assert!(
            n >= 1,
            "expected the blocking slice to deliver the wakeup, got {n}"
        );
        let w = &buf[0];
        assert_eq!(w.token, pack_token(0, token.0));
        assert_eq!(w.parked, parked);
        assert_eq!(w.direction, IoDirection::Read as u8);

        connector.join().unwrap();
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
        listener_fd: i64,
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
        let listener_fd = listener.as_raw_fd() as i64;

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
        let raw_fd = listener.as_raw_fd() as i64;

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

    /// A2a-2: a timer registered through the C ABI fires through the real
    /// background poller and surfaces via `take_wakeups`, carrying its packed
    /// token + parked pointer, no earlier than the deadline. Exercises the full
    /// production path register_timer → poller `run_once` expiry → queue →
    /// take_wakeups (vs the A2a-1 unit tests that drive `run_once` directly).
    #[test]
    fn background_thread_delivers_timer_wakeup() {
        let _guard = start_background_poller_for_test();
        let marker: u64 = 0x713E_5117_713E_5117;
        let parked = std::ptr::addr_of!(marker) as *mut c_void;

        let start = Instant::now();
        let token = karac_runtime_event_loop_register_timer(40_000_000, parked, std::ptr::null());
        assert_ne!(token, 0, "register_timer returns a non-zero packed token");

        let mut buf: [KaracWakeup; 4] = unsafe { std::mem::zeroed() };
        let n = unsafe {
            karac_runtime_event_loop_take_wakeups(buf.as_mut_ptr(), buf.len(), 2_000_000_000)
        };
        let elapsed = start.elapsed();
        assert!(n >= 1, "expected the timer wakeup via the poller, got {n}");
        assert_eq!(buf[0].token, token, "wakeup carries the timer's token");
        assert_eq!(buf[0].parked, parked, "wakeup carries the parked pointer");
        assert!(
            elapsed >= Duration::from_millis(30),
            "timer fired early ({elapsed:?} < ~40ms)"
        );
        assert!(
            elapsed < Duration::from_secs(1),
            "timer fired late: {elapsed:?}"
        );
    }

    /// A cancelled timer never surfaces a wakeup, and a second cancel no-ops.
    #[test]
    fn cancel_timer_prevents_wakeup() {
        let _guard = start_background_poller_for_test();
        let marker: u64 = 0xCA11_CA11_CA11_CA11;
        let parked = std::ptr::addr_of!(marker) as *mut c_void;

        let token = karac_runtime_event_loop_register_timer(60_000_000, parked, std::ptr::null());
        assert_ne!(token, 0);
        assert_eq!(
            karac_runtime_event_loop_cancel_timer(token),
            0,
            "cancel claims the live timer"
        );

        let mut buf: [KaracWakeup; 4] = unsafe { std::mem::zeroed() };
        let n = unsafe {
            karac_runtime_event_loop_take_wakeups(buf.as_mut_ptr(), buf.len(), 200_000_000)
        };
        assert_eq!(n, 0, "a cancelled timer must never surface a wakeup");
        assert_eq!(
            karac_runtime_event_loop_cancel_timer(token),
            -1,
            "second cancel of an already-claimed timer is a no-op"
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
        listener_fd: i64,
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
        let listener_fd = listener.as_raw_fd() as i64;

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

    // ── Slice 5c: cancel-sweep of an idle parked task ──────────────────────

    /// State for the cancel-sweep test. `poll_fn` returns Pending until it is
    /// invoked with a *set* cancel flag, at which point it records the
    /// invocation and returns Ready — mirroring the codegen resume shim's
    /// cancel-check → destroy → signal. `invocations` counts every poll_fn call
    /// so the test can assert the sweep invokes it exactly once (one-shot claim).
    #[cfg(unix)]
    struct CancelSweepState {
        fd: i64,
        token: std::sync::atomic::AtomicU64,
        cancelled: std::sync::atomic::AtomicBool,
        invocations: std::sync::atomic::AtomicU64,
    }
    #[cfg(unix)]
    unsafe impl Send for CancelSweepState {}
    #[cfg(unix)]
    unsafe impl Sync for CancelSweepState {}

    #[cfg(unix)]
    unsafe extern "C" fn cancel_sweep_poll_fn(
        state_ptr: *mut c_void,
        cancel: *const std::sync::atomic::AtomicBool,
    ) -> u8 {
        // SAFETY: caller passes the live `*const CancelSweepState`.
        let st = unsafe { &*(state_ptr as *const CancelSweepState) };
        st.invocations.fetch_add(1, Ordering::AcqRel);
        // SAFETY: a non-null cancel is the test's `AtomicBool`, live for the
        // duration of the test (pinned below).
        let is_cancelled = !cancel.is_null() && unsafe { (*cancel).load(Ordering::Acquire) };
        if is_cancelled {
            // Mirror the destroy edge: deregister + signal completion.
            let tok = st.token.swap(0, Ordering::AcqRel);
            if tok != 0 {
                let _ = karac_runtime_event_loop_deregister_fd(st.fd, tok);
            }
            st.cancelled.store(true, Ordering::Release);
            KaracPollResult::Ready as u8
        } else {
            KaracPollResult::Pending as u8
        }
    }

    /// The dispatcher cancel-sweep tears down a task parked on an **idle** fd
    /// (one that never becomes ready, so the normal dispatch loop never visits
    /// it) once its per-task `cancel` flag is set and a sweep is requested. This
    /// is the unit-level analog of the `coroutine_taskgroup_cancel_*` E2E tests,
    /// independent of codegen: it pins (a) per-task cancel routing — `poll_fn`
    /// receives the flag bound via `register_fd_cancel`, not the dispatcher's
    /// global — and (b) the sweep reaching an idle registration.
    #[cfg(unix)]
    #[test]
    fn cancel_sweep_tears_down_idle_parked_task() {
        let _guard = start_scheduler_for_test();
        use std::os::fd::AsRawFd;

        // An idle listener — bound but with no incoming connection, so its fd
        // never becomes readable and the dispatcher's normal loop never visits
        // the parked task. Only the sweep can reach it.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let listener_fd = listener.as_raw_fd() as i64;

        let state = Box::new(CancelSweepState {
            fd: listener_fd,
            token: std::sync::atomic::AtomicU64::new(0),
            cancelled: std::sync::atomic::AtomicBool::new(false),
            invocations: std::sync::atomic::AtomicU64::new(0),
        });
        let task = Box::new(KaracParkedTask {
            poll_fn: cancel_sweep_poll_fn,
            state: &*state as *const CancelSweepState as *mut c_void,
        });
        let task_ptr = &*task as *const KaracParkedTask as *mut c_void;

        // The per-task cancel flag, bound on the registration (the codegen path
        // does this via the coroutine's slot).
        let cancel = Box::new(std::sync::atomic::AtomicBool::new(false));
        let cancel_ptr = &*cancel as *const AtomicBool;

        let token =
            karac_runtime_event_loop_register_fd_cancel(listener_fd, 0, task_ptr, cancel_ptr);
        assert_ne!(token, 0, "register_fd_cancel should succeed");
        state.token.store(token, Ordering::Release);

        // No sweep yet: the flag is clear, so even a sweep request would find
        // nothing. Flip the flag, then request the sweep (the order
        // `taskgroup_cancel` uses).
        cancel.store(true, Ordering::Release);
        let rc = karac_runtime_request_cancel_sweep();
        assert_eq!(rc, 0, "request_cancel_sweep should succeed");

        // The sweep must drive the idle parked task to its cancelled arm. A
        // broken sweep (or per-task routing) leaves `cancelled` false → timeout.
        let start = Instant::now();
        while !state.cancelled.load(Ordering::Acquire) {
            if start.elapsed() > Duration::from_secs(2) {
                panic!("cancel sweep did not tear down the idle parked task within 2s");
            }
            thread::sleep(Duration::from_millis(10));
        }

        // One-shot claim: the sweep took the registration before invoking
        // poll_fn, so poll_fn ran exactly once (no double-invoke from a stray
        // re-sweep). A second request now finds nothing.
        let _ = karac_runtime_request_cancel_sweep();
        thread::sleep(Duration::from_millis(50));
        assert_eq!(
            state.invocations.load(Ordering::Acquire),
            1,
            "poll_fn must be invoked exactly once by the sweep (one-shot claim)"
        );

        // Drop order: nothing else references these now (the registration was
        // taken + deregistered by the cancelled arm).
        drop(task);
        drop(state);
        drop(cancel);
    }

    // ── Stage B3 regression: reap-on-peer-close through an inline,
    // re-registering poll_fn ────────────────────────────────────────────
    //
    // The recv-loop coroutine shape that the demo's per-connection handler
    // compiles to: park on read → (dispatcher resumes poll_fn) → recv →
    // re-register for the next read → park again. When the peer disconnects
    // (TCP EOF), the re-parked task MUST be re-woken and driven to its EOF
    // arm (which, in the demo, drops the WebSocket and closes the fd). Stage
    // B3's combined poll-and-dispatch ran poll_fn INLINE on the shard's only
    // polling thread and dropped the EOF edge (armed-but-mapless +
    // edge-triggered), wedging the task parked forever at 0% CPU — the
    // observed fd-leak-on-disconnect (1M connections stuck in CLOSE-WAIT).
    //
    // Unlike the slot-signal tests above (which re-register on the *test's*
    // thread via `park_slot_wait`), this poll_fn re-registers from *inside*
    // the dispatcher's inline invocation — the exact path B3 broke.
    #[cfg(unix)]
    struct ReparkState {
        fd: i64,
        /// Points to the owning `KaracParkedTask` so poll_fn can re-park
        /// itself. Set once before registration; read-only thereafter.
        task_ptr: *mut c_void,
        last_token: std::sync::atomic::AtomicU64,
        data_reads: std::sync::atomic::AtomicU64,
        /// Set when poll_fn observes EOF (the reap path).
        completed: std::sync::atomic::AtomicBool,
    }
    // SAFETY: fields are atomics or set-once-before-publish; the fd's
    // registrations all hash to one shard, so its poll_fn never runs
    // concurrently with itself.
    #[cfg(unix)]
    unsafe impl Send for ReparkState {}
    #[cfg(unix)]
    unsafe impl Sync for ReparkState {}

    #[cfg(unix)]
    unsafe extern "C" fn repark_recv_poll_fn(
        state_ptr: *mut c_void,
        _cancel: *const std::sync::atomic::AtomicBool,
    ) -> u8 {
        use std::io::Read;
        use std::os::fd::FromRawFd;
        // SAFETY: caller passes the live `*const ReparkState`.
        let st = unsafe { &*(state_ptr as *const ReparkState) };
        // Resume edge: deregister the prior arming (epoll DEL), mirroring the
        // codegen coroutine's deregister-after-park.
        let tok = st.last_token.swap(0, Ordering::AcqRel);
        if tok != 0 {
            let _ = karac_runtime_event_loop_deregister_fd(st.fd, tok);
        }
        // Read without owning the fd (ManuallyDrop ⇒ no close here). The fd is
        // ready on resume; non-blocking so a spurious wake re-parks instead of
        // blocking the dispatcher thread.
        let mut stream = std::mem::ManuallyDrop::new(unsafe {
            std::net::TcpStream::from_raw_fd(st.fd as std::os::unix::io::RawFd)
        });
        let mut buf = [0u8; 64];
        match stream.read(&mut buf) {
            Ok(0) => {
                // EOF — peer closed. The demo's analogue drops + closes here.
                st.completed.store(true, Ordering::Release);
                KaracPollResult::Ready as u8
            }
            Ok(_) => {
                st.data_reads.fetch_add(1, Ordering::Relaxed);
                let t = karac_runtime_event_loop_register_fd(st.fd, 0, st.task_ptr);
                st.last_token.store(t, Ordering::Release);
                KaracPollResult::Pending as u8
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                let t = karac_runtime_event_loop_register_fd(st.fd, 0, st.task_ptr);
                st.last_token.store(t, Ordering::Release);
                KaracPollResult::Pending as u8
            }
            Err(_) => {
                st.completed.store(true, Ordering::Release);
                KaracPollResult::Err as u8
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn dispatcher_reaps_connection_when_peer_closes_after_repark() {
        let _guard = start_scheduler_for_test();
        use std::io::Write;
        use std::os::fd::{FromRawFd, IntoRawFd};

        // Connected TCP pair: accept the server side, keep the client side.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let local = listener.local_addr().unwrap();
        let connector = thread::spawn(move || std::net::TcpStream::connect(local).unwrap());
        let (server, _addr) = listener.accept().unwrap();
        let mut client = connector.join().unwrap();
        server.set_nonblocking(true).unwrap();
        let server_fd = server.into_raw_fd() as i64;

        let state = Box::new(ReparkState {
            fd: server_fd,
            task_ptr: std::ptr::null_mut(),
            last_token: std::sync::atomic::AtomicU64::new(0),
            data_reads: std::sync::atomic::AtomicU64::new(0),
            completed: std::sync::atomic::AtomicBool::new(false),
        });
        let task = Box::new(KaracParkedTask {
            poll_fn: repark_recv_poll_fn,
            state: &*state as *const ReparkState as *mut c_void,
        });
        let task_ptr = &*task as *const KaracParkedTask as *mut c_void;
        // Wire the self-reference (single-threaded, before the fd is
        // registered, so the dispatcher cannot observe a null task_ptr).
        unsafe {
            (*(&*state as *const ReparkState as *mut ReparkState)).task_ptr = task_ptr;
        }

        // Initial park.
        let tok = karac_runtime_event_loop_register_fd(server_fd, 0, task_ptr);
        assert_ne!(tok, 0);
        state.last_token.store(tok, Ordering::Release);

        // One message → dispatcher resumes → reads it → re-parks.
        client.write_all(b"hi").unwrap();
        let start = Instant::now();
        while state.data_reads.load(Ordering::Acquire) == 0 {
            if start.elapsed() > Duration::from_secs(2) {
                panic!("dispatcher never delivered the first message");
            }
            thread::sleep(Duration::from_millis(5));
        }

        // Peer closes → EOF. The re-parked task MUST be re-woken and reach
        // its EOF arm. The B3 lost-wake leaves it parked forever.
        drop(client);
        let start = Instant::now();
        while !state.completed.load(Ordering::Acquire) {
            if start.elapsed() > Duration::from_secs(3) {
                panic!(
                    "dispatcher did not reap the connection on peer close — EOF edge lost \
                     after re-register (Stage B3 inline-dispatch lost-wake)"
                );
            }
            thread::sleep(Duration::from_millis(10));
        }

        // Cleanup: drop any live registration, then close the fd.
        let tok = state.last_token.swap(0, Ordering::AcqRel);
        if tok != 0 {
            let _ = karac_runtime_event_loop_deregister_fd(server_fd, tok);
        }
        // SAFETY: reclaim ownership to close exactly once.
        unsafe {
            drop(std::net::TcpStream::from_raw_fd(
                server_fd as std::os::unix::io::RawFd,
            ));
        }
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
            let fd = l.as_raw_fd() as i64;
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

    /// Parallel-dispatch correctness: with the dispatcher running N worker
    /// threads behind a herd-free (`notify_one` + wake-a-friend) handoff, a
    /// batch of many independent parked tasks — registered concurrently,
    /// fired concurrently — must ALL be driven to completion exactly once.
    /// The one-shot `take_registration` claim has to route each wakeup to a
    /// single dispatcher thread: no wakeup dropped, double-driven, or stolen
    /// by a sibling. A fan-out/handoff regression fails here (a task never
    /// completes) within the bounded timeout rather than hanging or
    /// corrupting. Exercises the wake-a-friend ramp under real concurrency.
    #[cfg(unix)]
    #[test]
    fn dispatcher_drives_many_concurrent_parked_tasks_across_threads() {
        let _guard = start_scheduler_for_test();
        use std::os::fd::AsRawFd;

        const K: usize = 64;
        let mut listeners = Vec::with_capacity(K);
        let mut addrs = Vec::with_capacity(K);
        let mut states: Vec<Box<SchedulerTestState>> = Vec::with_capacity(K);
        let mut tasks: Vec<Box<KaracParkedTask>> = Vec::with_capacity(K);
        let cancel = std::sync::atomic::AtomicBool::new(false);

        for _ in 0..K {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.set_nonblocking(true).unwrap();
            let addr = l.local_addr().unwrap();
            let fd = l.as_raw_fd() as i64;
            let mut s = Box::new(SchedulerTestState {
                tag: 0,
                listener_fd: fd,
                token: 0,
                completed: std::sync::atomic::AtomicBool::new(false),
            });
            let task = Box::new(KaracParkedTask {
                poll_fn: scheduler_test_poll_fn,
                state: &mut *s as *mut SchedulerTestState as *mut c_void,
            });
            let task_ptr = &*task as *const KaracParkedTask as *mut c_void;
            let tok = karac_runtime_event_loop_register_fd(fd, 0, task_ptr);
            assert_ne!(tok, 0);
            s.token = tok;
            // Initial park (tag 0 -> 1, Pending) before any readiness fires —
            // the fd is not readable until we connect below.
            assert_eq!(
                unsafe { (task.poll_fn)(task.state, &cancel) },
                KaracPollResult::Pending as u8
            );
            listeners.push(l);
            addrs.push(addr);
            states.push(s);
            tasks.push(task);
        }

        // Fire all K fds concurrently — maximizes the chance of multiple
        // dispatcher threads claiming wakeups at the same instant.
        let connectors: Vec<_> = addrs
            .iter()
            .map(|a| {
                let a = *a;
                thread::spawn(move || {
                    let _s = std::net::TcpStream::connect(a).unwrap();
                    thread::sleep(Duration::from_millis(100));
                })
            })
            .collect();

        let start = Instant::now();
        loop {
            let done = states
                .iter()
                .filter(|s| s.completed.load(Ordering::Acquire))
                .count();
            if done == K {
                break;
            }
            if start.elapsed() > Duration::from_secs(5) {
                panic!("dispatcher drove only {done}/{K} tasks to completion within 5s");
            }
            thread::sleep(Duration::from_millis(10));
        }

        for c in connectors {
            c.join().unwrap();
        }
        // Keep registrations / states / listeners alive until completion is
        // observed (the dispatcher derefs the parked pointers).
        drop(tasks);
        drop(states);
        drop(listeners);
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
        let lfd = listener.as_raw_fd() as i64;

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
        let listener_fd = listener.as_raw_fd() as i64;

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
    fn loopback_pair() -> (i64, std::net::TcpStream) {
        use std::os::unix::io::IntoRawFd;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local_addr");
        let client_handle =
            std::thread::spawn(move || std::net::TcpStream::connect(addr).expect("client connect"));
        let (server_side, _) = listener.accept().expect("accept loopback");
        let client_side = client_handle.join().expect("client thread join");
        let server_fd = server_side.into_raw_fd();
        (server_fd as i64, client_side)
    }

    /// Close a raw fd at test end (reconstruct + drop). Mirrors
    /// the cleanup convention in `karac_runtime_tcp_close`.
    #[cfg(unix)]
    fn close_fd(fd: i64) {
        use std::os::unix::io::{FromRawFd, RawFd};
        let _ = unsafe { std::net::TcpStream::from_raw_fd(fd as RawFd) };
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
        let listener_fd = listener.into_raw_fd() as i64;

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
        let listener_fd = listener.into_raw_fd() as i64;

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

    // ── line-128 handshake hardening (RFC 6455 §4.2.1 validation) ───────

    const VALID_WS_REQUEST: &[u8] = b"GET /ws HTTP/1.1\r\n\
        Host: 127.0.0.1\r\n\
        Upgrade: websocket\r\n\
        Connection: Upgrade\r\n\
        Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
        Sec-WebSocket-Version: 13\r\n\
        \r\n";

    #[test]
    fn test_ws_validate_handshake_accepts_complete_request() {
        let key = super::ws_validate_handshake(VALID_WS_REQUEST).expect("valid request");
        assert_eq!(key, b"dGhlIHNhbXBsZSBub25jZQ==");
    }

    #[test]
    fn test_ws_validate_handshake_rejects_missing_upgrade() {
        let req = b"GET /ws HTTP/1.1\r\n\
            Host: 127.0.0.1\r\n\
            Connection: Upgrade\r\n\
            Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
            Sec-WebSocket-Version: 13\r\n\r\n";
        assert!(matches!(
            super::ws_validate_handshake(req),
            Err(super::WsHandshakeReject::BadRequest(_))
        ));
    }

    #[test]
    fn test_ws_validate_handshake_rejects_missing_connection() {
        let req = b"GET /ws HTTP/1.1\r\n\
            Host: 127.0.0.1\r\n\
            Upgrade: websocket\r\n\
            Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
            Sec-WebSocket-Version: 13\r\n\r\n";
        assert!(matches!(
            super::ws_validate_handshake(req),
            Err(super::WsHandshakeReject::BadRequest(_))
        ));
    }

    #[test]
    fn test_ws_validate_handshake_rejects_bad_version() {
        let req = b"GET /ws HTTP/1.1\r\n\
            Host: 127.0.0.1\r\n\
            Upgrade: websocket\r\n\
            Connection: Upgrade\r\n\
            Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
            Sec-WebSocket-Version: 8\r\n\r\n";
        assert_eq!(
            super::ws_validate_handshake(req),
            Err(super::WsHandshakeReject::UnsupportedVersion)
        );
    }

    #[test]
    fn test_ws_validate_handshake_rejects_missing_version() {
        let req = b"GET /ws HTTP/1.1\r\n\
            Host: 127.0.0.1\r\n\
            Upgrade: websocket\r\n\
            Connection: Upgrade\r\n\
            Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n";
        assert_eq!(
            super::ws_validate_handshake(req),
            Err(super::WsHandshakeReject::UnsupportedVersion)
        );
    }

    #[test]
    fn test_ws_validate_handshake_rejects_missing_key() {
        let req = b"GET /ws HTTP/1.1\r\n\
            Host: 127.0.0.1\r\n\
            Upgrade: websocket\r\n\
            Connection: Upgrade\r\n\
            Sec-WebSocket-Version: 13\r\n\r\n";
        assert!(matches!(
            super::ws_validate_handshake(req),
            Err(super::WsHandshakeReject::BadRequest(_))
        ));
    }

    #[test]
    fn test_ws_validate_handshake_connection_token_in_list() {
        // `Connection: keep-alive, Upgrade` is legal — the Upgrade
        // token must still be found inside the comma list (some
        // proxies/clients add keep-alive). Header names are
        // case-insensitive too.
        let req = b"GET /ws HTTP/1.1\r\n\
            host: 127.0.0.1\r\n\
            UPGRADE: WebSocket\r\n\
            Connection: keep-alive, Upgrade\r\n\
            Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
            Sec-WebSocket-Version: 13\r\n\r\n";
        assert!(super::ws_validate_handshake(req).is_ok());
    }

    #[test]
    fn test_ws_validate_handshake_version_token_in_list() {
        // A client may offer multiple versions; 13 in the list is a
        // match.
        let req = b"GET /ws HTTP/1.1\r\n\
            Host: 127.0.0.1\r\n\
            Upgrade: websocket\r\n\
            Connection: Upgrade\r\n\
            Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
            Sec-WebSocket-Version: 8, 13\r\n\r\n";
        assert!(super::ws_validate_handshake(req).is_ok());
    }

    #[test]
    fn test_ws_handshake_reject_response_status_lines() {
        assert!(super::WsHandshakeReject::BadRequest("x")
            .response_bytes()
            .starts_with(b"HTTP/1.1 400 Bad Request\r\n"));
        let v426 = super::WsHandshakeReject::UnsupportedVersion.response_bytes();
        assert!(v426.starts_with(b"HTTP/1.1 426 Upgrade Required\r\n"));
        // RFC 6455 §4.4 requires the supported version be advertised.
        assert!(v426
            .windows(b"Sec-WebSocket-Version: 13\r\n".len())
            .any(|w| w == b"Sec-WebSocket-Version: 13\r\n"));
    }

    #[test]
    fn test_ws_handshake_timeout_from_raw_parse_matrix() {
        use std::time::Duration;
        // Default when unset.
        assert_eq!(
            super::ws_handshake_timeout_from_raw(None),
            Some(Duration::from_millis(10_000))
        );
        // Explicit value.
        assert_eq!(
            super::ws_handshake_timeout_from_raw(Some("2500")),
            Some(Duration::from_millis(2500))
        );
        // `0` disables.
        assert_eq!(super::ws_handshake_timeout_from_raw(Some("0")), None);
        // Whitespace trimmed.
        assert_eq!(
            super::ws_handshake_timeout_from_raw(Some("  750  ")),
            Some(Duration::from_millis(750))
        );
        // Garbage falls back to default.
        assert_eq!(
            super::ws_handshake_timeout_from_raw(Some("not-a-number")),
            Some(Duration::from_millis(10_000))
        );
    }

    // `all(unix, …)` mirrors the helper's own gate — its only consumer is
    // the unix-gated TLS handshake pool, so it doesn't exist on Windows.
    #[cfg(all(unix, feature = "tls"))]
    #[test]
    fn test_ws_max_pending_from_raw_parse_matrix() {
        // Off by default (unset → unbounded).
        assert_eq!(super::ws_max_pending_from_raw(None), None);
        // `0` disables (explicit off).
        assert_eq!(super::ws_max_pending_from_raw(Some("0")), None);
        // Positive integer → cap.
        assert_eq!(super::ws_max_pending_from_raw(Some("1024")), Some(1024));
        // Whitespace trimmed.
        assert_eq!(super::ws_max_pending_from_raw(Some("  64 ")), Some(64));
        // Garbage / negative → off (never a silent partial cap).
        assert_eq!(super::ws_max_pending_from_raw(Some("nope")), None);
        assert_eq!(super::ws_max_pending_from_raw(Some("-5")), None);
    }

    #[cfg(unix)]
    #[test]
    fn test_ws_accept_rejects_bad_version_with_426() {
        use std::io::{Read, Write};
        use std::os::unix::io::IntoRawFd;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        let listener_fd = listener.into_raw_fd() as i64;

        let client_handle = std::thread::spawn(move || {
            let mut conn = std::net::TcpStream::connect(addr).expect("client connect");
            conn.set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            // Otherwise-valid upgrade, but an unsupported version.
            let req = b"GET /ws HTTP/1.1\r\n\
                Host: 127.0.0.1\r\n\
                Upgrade: websocket\r\n\
                Connection: Upgrade\r\n\
                Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
                Sec-WebSocket-Version: 7\r\n\r\n";
            conn.write_all(req).expect("write request");
            let mut resp = Vec::new();
            let mut chunk = [0u8; 256];
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
        assert_eq!(conn_fd, -1, "bad version must not upgrade");

        let resp = client_handle.join().expect("client thread");
        let resp_str = String::from_utf8_lossy(&resp);
        assert!(
            resp_str.starts_with("HTTP/1.1 426 Upgrade Required\r\n"),
            "expected 426 for unsupported version; got: {resp_str}"
        );
        assert!(
            resp_str.contains("Sec-WebSocket-Version: 13\r\n"),
            "426 must advertise the supported version; got: {resp_str}"
        );
        close_fd(listener_fd);
    }

    #[cfg(unix)]
    #[test]
    fn test_ws_read_http_request_times_out_on_stalled_peer() {
        // Slowloris mechanism: a peer that connects but never sends
        // the upgrade request must be reaped by the socket read-
        // timeout, not block until the kernel default fires. This
        // exercises the timeout path `karac_runtime_ws_accept` /
        // `ws_handshake_conn_tls` apply via `set_read_timeout`
        // without mutating the process-global env var.
        use std::time::{Duration, Instant};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("local_addr");

        let client_handle = std::thread::spawn(move || {
            let conn = std::net::TcpStream::connect(addr).expect("client connect");
            // Hold the connection open but send nothing for a while.
            std::thread::sleep(Duration::from_millis(800));
            drop(conn);
        });

        let (mut server_conn, _) = listener.accept().expect("accept");
        server_conn
            .set_read_timeout(Some(Duration::from_millis(100)))
            .expect("set_read_timeout");
        let start = Instant::now();
        let result = super::ws_read_http_request(&mut server_conn, None);
        let elapsed = start.elapsed();

        assert!(
            result.is_none(),
            "stalled handshake read must return None (reaped by timeout)"
        );
        assert!(
            elapsed < Duration::from_millis(600),
            "read should be reaped near the 100ms timeout, not wait for the peer; took {elapsed:?}"
        );
        client_handle.join().expect("client thread");
    }

    #[cfg(unix)]
    #[test]
    fn test_ws_read_http_request_deadline_reaps_dribbler() {
        // Dribble slowloris (the residual the per-read SO_RCVTIMEO
        // alone does NOT close): a peer sending one byte slightly
        // faster than the per-read timeout re-arms that clock on
        // every read and would otherwise be held open up to the
        // 8 KiB cap. The whole-request `deadline` must reap it. Here
        // the per-read timeout is 200ms and the client dribbles
        // every 50ms (so each read succeeds, the per-read timeout
        // never fires), while the 300ms wall-clock deadline forces
        // termination.
        use std::io::Write;
        use std::time::{Duration, Instant};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("local_addr");

        let client_handle = std::thread::spawn(move || {
            let mut conn = std::net::TcpStream::connect(addr).expect("client connect");
            // Never send \r\n\r\n; dribble single bytes for ~2s or
            // until the server hangs up (write error after the
            // deadline reaps the read + drops the socket).
            for _ in 0..40 {
                if conn.write_all(b"x").is_err() {
                    break;
                }
                let _ = conn.flush();
                std::thread::sleep(Duration::from_millis(50));
            }
        });

        let (mut server_conn, _) = listener.accept().expect("accept");
        server_conn
            .set_read_timeout(Some(Duration::from_millis(200)))
            .expect("set_read_timeout");
        let deadline = Some(Instant::now() + Duration::from_millis(300));
        let start = Instant::now();
        let result = super::ws_read_http_request(&mut server_conn, deadline);
        let elapsed = start.elapsed();
        drop(server_conn);

        assert!(
            result.is_none(),
            "a dribbling peer must be reaped by the whole-request deadline"
        );
        // Bounded near the deadline (≤ deadline + one per-read
        // timeout), NOT held until the 8 KiB cap (which at 50ms/byte
        // would be ~6.8 minutes).
        assert!(
            elapsed < Duration::from_millis(900),
            "deadline must bound total handshake-read time; took {elapsed:?}"
        );
        client_handle.join().expect("client thread");
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

    // Mirrors the helper's `#[cfg(unix)]` gate (`/dev/urandom` reader).
    #[cfg(unix)]
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
