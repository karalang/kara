//! The MIR backend's task executor (B-2026-10-14-49), the compiled twin of
//! the MIR interpreter's: one thread, a FIFO run queue, cooperative.
//!
//! A task is a poll function and its two arguments. The coroutine transform
//! makes every task root a `resume` body, and codegen wraps each in a poll
//! function that resumes it once and answers whether it returned. A task
//! suspends only in a readiness loop, where `__ready` reported its next
//! suspending call unable to complete; what it saw is the task's `Wait`.
//! When every task in the queue has suspended on a `Wait` with nothing
//! having happened since, the executor sleeps until a deadline passes or a
//! socket is ready; with neither to wait for, the program is deadlocked.
//!
//! # The reactor contract (M4 item 2)
//!
//! Readiness moves from poll(2) over every waiter to one reactor. The natives
//! (`mir_io.rs`, Thread C's), this executor, codegen's readiness loop
//! (`src/backend/mir/coroutine.rs`, `task.rs`) and the MIR interpreter's
//! twin all keep it:
//!
//! - **Fds.** Every socket a native creates (listen, accept, connect, UDP),
//!   and a spawned child's pipes, is `O_NONBLOCK | O_CLOEXEC` from birth.
//! - **Outside a task**, every native keeps blocking semantics: on `EAGAIN`
//!   it calls `karac_mir_io_wait(fd, write, timeout_ms)`, a one-fd blocking
//!   wait answering 0 when ready and 1 when `timeout_ms` passed (-1: none),
//!   and retries. A program that runs no task sees no change.
//! - **Inside a task** a native that would block returns `IO_NOT_READY`
//!   (100, `mir_sock.rs`) having done nothing, and never retries itself.
//!   Kāra code never sees the status.
//! - **Armed calls.** Only a call the readiness loop armed answers
//!   `IO_NOT_READY`: a ready answer from `__ready` sets the thread's armed
//!   bit (`karac_mir_io_arm`), and every native takes it, clearing it, on
//!   every path. An unarmed call (one through a function value, or in a
//!   body that is no coroutine) blocks as outside a task.
//! - **The readiness loop** the coroutine transform puts around a
//!   suspending fd call is `loop { while !__ready(op, fd) { yield }; r =
//!   native(..); if !__io_again() { break } }`. `__ready` asks the fd's
//!   readiness afresh (`poll(fd, 0)`), so it is never stale, and a ready
//!   answer arms the call. A call that still finds nothing (another reader
//!   took the data, a connection reset) answers `IO_NOT_READY`, sets the
//!   thread's again bit ([`set_again`]) and stores nothing; codegen then
//!   leaves its arguments unconsumed and `__io_again` sends the loop back
//!   to `__ready`.
//! - **Connect is two-phase.** On `EINPROGRESS` an armed `connect` keeps
//!   the socket on the running task ([`set_inflight`]) and answers
//!   `IO_IN_PROGRESS` (101), which codegen treats as `IO_NOT_READY`.
//!   `__ready` of the retried connect waits for that fd to be writable, and
//!   the retry ends the connect (`connect_finish`), which reads `SO_ERROR`.
//! - **Close.** A native that closes a socket or pipe fd calls
//!   `karac_mir_io_forget(fd)` first, so the reactor deregisters it before
//!   the number can be reused. A `dup` shares the description, so it gets
//!   the same treatment.
//! - **Timeouts.** A native reads an fd's timeout back with `getsockopt`. Until task
//!   deadlines land (item 5), a task's read on an fd with a timeout falls
//!   back to the blocking `io_wait`.
//! - **Process waits** give the reactor an fd to wait readable on
//!   (`karac_mir_child_wait_fd`: a pidfd on Linux, a kqueue watching the
//!   process with `EVFILT_PROC` / `NOTE_EXIT` on macOS and the BSDs, -1
//!   where neither is had, and the wait then blocks). The wait native
//!   answers `IO_NOT_READY` until the child has exited.
//! - **Wasm and non-unix** have no reactor. There `io_wait` is a sleep
//!   loop, and nothing above changes what a program does.
//!
//! A task waiting on an fd leaves the run queue and parks on the reactor
//! (oneshot `epoll` on Linux, `poll(2)` over the parked fds elsewhere), and
//! a sleeping task goes in a deadline heap. A round of the executor then
//! costs O(runnable) rather than O(tasks): the executor asks the reactor
//! without waiting once a round, and when nothing can run it waits on the
//! reactor until the nearest deadline, waking only the tasks the events
//! name.

use std::cell::UnsafeCell;
use std::cell::{Cell, RefCell};
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU8, AtomicUsize, Ordering};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

/// The executor's maps are keyed by its own counters (task keys, task and
/// group ids) and by descriptors, never by a program's data, so they hash
/// with one multiply rather than SipHash under a random key: SipHash's
/// rounds were inlined into every table operation of every map, in every
/// binary that runs a task (B-2026-10-28-8).
#[derive(Default)]
struct IdHasher(u64);

impl std::hash::Hasher for IdHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.write_u64(u64::from(b));
        }
    }

    fn write_u32(&mut self, n: u32) {
        self.write_u64(u64::from(n));
    }

    fn write_u64(&mut self, n: u64) {
        // Fibonacci hashing: the table indexes by the low bits and tags by
        // the top seven, and a multiply by an odd constant spreads both.
        self.0 = (self.0 ^ n).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }

    fn write_usize(&mut self, n: usize) {
        self.write_u64(n as u64);
    }
}

type IdMap<K, V> = HashMap<K, V, std::hash::BuildHasherDefault<IdHasher>>;

/// A task's poll function: resumes the task once, and answers 1 when it
/// returned.
pub type TaskPoll = unsafe extern "C" fn(*mut u8, *mut u8) -> u8;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Wait {
    /// Runnable: not suspended, or suspended on a plain yield.
    None,
    /// A `sleep_ms` that has not reached its deadline: the task sleeps in
    /// the deadline heap.
    Until(Instant),
    /// A descriptor to become readable (`events` is `POLLIN`) or writable:
    /// the task parks on the reactor.
    Fd(i32, i16),
    /// A socket to become readable, or its read timeout's deadline to pass
    /// (core-semantics §11.10). Unix only, as socket deadlines are.
    #[cfg_attr(not(unix), allow(dead_code))]
    FdBy(i32, i16, Instant),
    /// A join or a channel, which only another task can make ready.
    Task,
    /// As `Task`, or until the deadline passes (a pool's `acquire`).
    TaskBy(Instant),
}

/// A `TaskGroup` (B-2026-10-15-30): how many of its tasks may run at once
/// and how many do. Its handle is a pointer to this, freed once the group
/// has dropped and its last task has returned. Its fields change under the
/// executor's lock.
struct Group {
    /// Never reused, so a deadline that names a group already freed names
    /// none.
    id: u64,
    limit: i64,
    live: i64,
    dropped: bool,
    /// The task that made it (0: none), whose teardown cancels it first
    /// (core-semantics §11.9).
    owner: u64,
    /// When it is cancelled (`with_deadline`, §11.10).
    deadline: Option<Instant>,
    /// It contains its tasks' panics (`TaskGroup.isolating`, §11.12).
    isolating: bool,
    /// Auto-par's (B-2026-10-27-22): its tasks are not tasks to `std.log`
    /// (docs/library/log.md), and their records reach the backend in the
    /// order they were spawned ([`Hold`]).
    hidden: bool,
    /// A hidden group's tasks, by id, in the order they were spawned.
    members: Vec<u64>,
    /// The first of `members` that has not finished: its records go on as
    /// it logs them, and a later task's wait until it is the first.
    next: usize,
}

/// A task of a hidden group, for `std.log` (B-2026-10-27-22): while it is
/// not its group's first unfinished task, the records it and the tasks it
/// starts log wait here; once it is, they go on as its spawner's would.
struct Hold {
    group: *mut Group,
    /// Its place in the group's `members`.
    seq: usize,
    /// Its spawner's hold (0: none).
    up: u64,
    done: bool,
    held: Vec<crate::mir_log::Held>,
}

struct Task {
    /// Never reused; `main` is 1.
    id: u64,
    /// The group that spawned it, or null for `main`.
    group: *mut Group,
    poll: TaskPoll,
    a: *mut u8,
    b: *mut u8,
    /// The deadline of the `sleep_ms` the task waits on, once asked.
    deadline: Option<Instant>,
    /// Its `__ready` waited the pause out: the `sleep_ms` after it returns
    /// at once.
    slept: bool,
    wait: Wait,
    /// The socket of a `connect` it has under way ([`set_inflight`]).
    inflight: Option<i32>,
    /// Its active span stack, swapped into the thread's while it runs: span
    /// context is per task, inherited from the spawner.
    spans: Vec<i64>,
    /// Cancelled (core-semantics §11.9): it is torn down at the wait it is
    /// suspended in, or the next one it reaches.
    cancel: bool,
    /// Its teardown has begun ([`karac_mir_cancelled`]).
    torn: bool,
    /// A frame of its chain is leaving by its cancel exit
    /// ([`karac_mir_unwind`]).
    unwind: bool,
    /// The progress count when its last poll began: a task waiting on
    /// another is worth polling again once the count moved.
    seen: u64,
    /// The root task, which only the first worker polls: it runs most of
    /// a program, whose data the first worker's caches and allocator
    /// arena hold, and the first worker would nap while another worker
    /// ran it (measured: fare_cap's root ran 29 of its 37 ms on other
    /// workers, the first napping, 0.75x of its seq build).
    root: bool,
    /// The `task_id` its `std.log` records carry (0: none): its own, or,
    /// for a task of a hidden group, its spawner's.
    log_id: u64,
    /// The [`Hold`] its `std.log` records go through (0: none): its own in
    /// a hidden group, else its spawner's, so a task started inside an
    /// auto-par task logs in that task's place in statement order.
    hold: u64,
}

/// Whether a queued task can get anywhere if polled now, with the progress
/// count at `progress`: it is runnable, or waits on another task and
/// something happened since its last poll began, or its wait's deadline
/// passed. `now` is the clock, read once a deadline needs it.
fn worth(t: &Task, progress: u64, now: &mut Option<Instant>) -> bool {
    match t.wait {
        Wait::None => true,
        Wait::Task => t.seen != progress,
        Wait::TaskBy(at) => t.seen != progress || at <= *now.get_or_insert_with(Instant::now),
        _ => false,
    }
}

/// The running task's own state, while it runs.
#[derive(Clone, Copy)]
struct Cur {
    id: u64,
    group: *mut Group,
    deadline: Option<Instant>,
    slept: bool,
    wait: Wait,
    inflight: Option<i32>,
    cancel: bool,
    torn: bool,
    unwind: bool,
    /// See [`Task::log_id`].
    log_id: u64,
    /// See [`Task::hold`].
    hold: u64,
}

impl Task {
    /// Its state while it runs.
    fn cur(&self) -> Cur {
        Cur {
            log_id: self.log_id,
            hold: self.hold,
            id: self.id,
            group: self.group,
            deadline: self.deadline,
            slept: self.slept,
            wait: Wait::None,
            inflight: self.inflight,
            cancel: self.cancel,
            torn: self.torn,
            unwind: self.unwind,
        }
    }

    /// Keeps what running it left.
    fn keep(&mut self, c: Cur) {
        self.deadline = c.deadline;
        self.slept = c.slept;
        self.wait = c.wait;
        self.inflight = c.inflight;
        self.cancel = c.cancel;
        self.torn = c.torn;
        self.unwind = c.unwind;
    }
}

#[derive(Default)]
struct Exec {
    /// The tasks that can run, and those waiting on another task.
    queue: VecDeque<Task>,
    /// The tasks off the queue, sleeping or parked, by the key they were
    /// put off with; a key is never reused, so a timer or a descriptor
    /// that names a task already woken names no task.
    off: IdMap<u64, Task>,
    keys: u64,
    /// Deadlines, each with the key of its task: the earliest first, and
    /// in the order the tasks were put off at a tie.
    timers: BinaryHeap<Reverse<(Instant, u64)>>,
    /// The keys of the tasks parked on each descriptor, in the order they
    /// parked.
    parked: IdMap<i32, Vec<u64>>,
    reactor: Reactor,
    /// The running task of the thread that called `karac_mir_task_run`,
    /// worker 0, the only one on one worker.
    cur: Option<Cur>,
    /// The pool's other workers' running tasks, by worker ([`worker`]);
    /// slot 0 is unused.
    curs: Vec<Option<Cur>>,
    /// Each worker's tasks a teardown interrupted ([`poll_nested`]),
    /// outermost first.
    outers: Vec<Vec<Cur>>,
    /// Bumped whenever something may have made a waiting task ready: a
    /// readiness check that passed, a spawn, or a task that returned.
    progress: u64,
    /// How many tasks the pool's workers are polling now, the thread that
    /// runs `karac_mir_task_run` included.
    running: usize,
    /// The pool's workers napping on an empty queue: a task queued wakes
    /// one.
    sleepers: Vec<Sleeper>,
    /// The last task id given out.
    ids: u64,
    /// The live groups, by id.
    groups: IdMap<u64, *mut Group>,
    group_ids: u64,
    /// The deadlines set on groups, with the group's id: one that is not
    /// the group's own any more (it fired, or the group is gone) is stale.
    deadlines: Vec<(Instant, u64)>,
    /// The tasks of hidden groups whose records are not all on, by id.
    holds: IdMap<u64, Hold>,
    /// Called with a task's id when it ends, returned or torn down
    /// ([`on_task_end`]).
    hooks: Vec<fn(u64)>,
    /// The panics of the tasks of isolating groups that panicked (§11.12),
    /// by task id: the message and the location, which the task's handle
    /// takes once its frame has left by its exits ([`task_failure`]).
    panics: IdMap<u64, (String, String)>,
}

impl Exec {
    /// The task this worker runs now.
    #[inline(always)]
    fn cur(&self) -> Option<Cur> {
        match worker() {
            0 => self.cur,
            w => self.curs.get(w).copied().flatten(),
        }
    }

    /// The slot of the task this worker runs.
    #[inline(always)]
    fn cur_mut(&mut self) -> &mut Option<Cur> {
        let w = worker();
        if w == 0 {
            return &mut self.cur;
        }
        if self.curs.len() <= w {
            self.curs.resize(w + 1, None);
        }
        &mut self.curs[w]
    }

    /// This worker's interrupted tasks.
    fn outer_mut(&mut self) -> &mut Vec<Cur> {
        let w = worker();
        if self.outers.len() <= w {
            self.outers.resize_with(w + 1, Vec::new);
        }
        &mut self.outers[w]
    }

    /// Queues `t`, runnable.
    fn wake(&mut self, mut t: Task) {
        t.wait = Wait::None;
        self.push(t);
    }

    /// Something a waiting task may wait on happened. No napping worker
    /// wakes for it: the worker that made it happen polls the tasks it
    /// made worth polling once its own task settles, which keeps a pair of
    /// tasks that wait on each other on one worker.
    #[inline]
    fn progressed(&mut self) {
        self.progress += 1;
    }

    /// Queues `t`, waking a napping worker for it: the first one for the
    /// root task, which no other polls.
    #[inline]
    fn push(&mut self, t: Task) {
        let root = t.root;
        self.queue.push_back(t);
        if root {
            self.rouse_first();
        } else if let Some(w) = self.sleepers.pop() {
            rouse(w);
        }
    }

    /// Wakes the first worker, if it naps.
    fn rouse_first(&mut self) {
        if let Some(i) = self.sleepers.iter().position(|s| s.worker == 0) {
            let w = self.sleepers.swap_remove(i);
            rouse(w);
        }
    }

    /// Takes `t` off the queue, waking by a timer or a descriptor; answers
    /// its key.
    fn put_off(&mut self, t: Task) -> u64 {
        self.keys += 1;
        self.off.insert(self.keys, t);
        self.keys
    }

    /// The task put off with `key`, if it is still off, taken off its
    /// descriptor's list.
    fn take_key(&mut self, key: u64) -> Option<Task> {
        let t = self.off.remove(&key)?;
        if let Wait::Fd(fd, _) | Wait::FdBy(fd, _, _) = t.wait {
            if let Some(keys) = self.parked.get_mut(&fd) {
                keys.retain(|&k| k != key);
                if keys.is_empty() {
                    self.parked.remove(&fd);
                }
            }
        }
        Some(t)
    }

    /// Queues the task put off with `key`, if it is still off; answers
    /// whether it was.
    fn wake_key(&mut self, key: u64) -> bool {
        let Some(t) = self.take_key(key) else {
            return false;
        };
        self.wake(t);
        true
    }

    /// The nearest deadline of a group that still runs tasks.
    fn next_group_deadline(&self) -> Option<Instant> {
        self.deadlines
            .iter()
            .filter(|&&(at, id)| {
                self.groups.get(&id).is_some_and(|&g| {
                    // SAFETY: a registered group is live.
                    let g = unsafe { &*g };
                    g.deadline == Some(at) && g.live > 0
                })
            })
            .map(|&(at, _)| at)
            .min()
    }

    /// The nearest deadline of a task still off, dropping the timers of
    /// tasks a descriptor already woke.
    fn next_deadline(&mut self) -> Option<Instant> {
        while let Some(&Reverse((at, key))) = self.timers.peek() {
            if self.off.contains_key(&key) {
                return Some(at);
            }
            self.timers.pop();
        }
        None
    }

    /// Queues the tasks whose deadline has passed; answers how many.
    fn wake_timers(&mut self) -> usize {
        if self.timers.is_empty() {
            return 0;
        }
        let now = Instant::now();
        let mut n = 0;
        while let Some(at) = self.next_deadline() {
            if at > now {
                break;
            }
            let Some(Reverse((_, key))) = self.timers.pop() else {
                break;
            };
            n += usize::from(self.wake_key(key));
        }
        n
    }

    /// Queues every task parked on `fd`; answers how many.
    fn wake_fd(&mut self, fd: i32) -> usize {
        let keys = self.parked.remove(&fd).unwrap_or_default();
        keys.into_iter().filter(|&k| self.wake_key(k)).count()
    }
}

/// The executor's state (M4 item 10). With one worker (`KARAC_THREADS=1`,
/// or one core) it is the thread's own, as it always was; under the pool
/// (more than one worker, the default) it is one for the process, every
/// worker takes the same lock, and runs its task's poll with the lock
/// released. Both keep the
/// thread-local `RefCell`'s `with` / `borrow_mut` shape it replaced, so a
/// nested borrow, which panics on one worker, would deadlock on several:
/// the code never nests one.
///
/// The lock is a flag of its own rather than a `Mutex`: one worker sets
/// and clears it with plain stores, so a borrow costs what the `RefCell`'s
/// did. A `Mutex` on every access cost a one-worker program about 2x on a
/// task-switching bench, and a choice between a `RefCell` and a `Mutex`
/// at each access still cost it about 18% in instructions.
struct ExecLock {
    held: AtomicBool,
    exec: UnsafeCell<Exec>,
}

// SAFETY: `exec` is reached only through a guard, which holds `held`. The
// raw pointers an `Exec` holds (groups, task frames) are the tasks' own,
// each touched under the lock or by the one worker running its task.
unsafe impl Sync for ExecLock {}
// SAFETY: as for `Sync`: the state moves between workers only under the
// lock.
unsafe impl Send for ExecLock {}

impl ExecLock {
    fn new() -> ExecLock {
        ExecLock {
            held: AtomicBool::new(false),
            exec: UnsafeCell::new(Exec::default()),
        }
    }

    /// Takes the lock: with a compare-and-swap under the pool, and on one
    /// worker by plain stores, refusing a nested borrow as the `RefCell`
    /// did.
    #[inline(always)]
    fn lock(&self, shared: bool) -> ExecGuard<'_> {
        if shared {
            self.lock_shared();
        } else {
            if self.held.load(Ordering::Relaxed) {
                nested_borrow();
            }
            self.held.store(true, Ordering::Relaxed);
        }
        ExecGuard { lock: self }
    }

    /// Kept out of line, so a one-worker borrow carries none of it.
    #[inline(never)]
    fn lock_shared(&self) {
        let mut spins = 0u32;
        loop {
            if !self.held.load(Ordering::Relaxed)
                && self
                    .held
                    .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
            {
                return;
            }
            spins += 1;
            if spins < 64 {
                std::hint::spin_loop();
            } else {
                std::thread::yield_now();
            }
        }
    }

    /// The lock, unless this worker holds it now. Under the pool another
    /// worker's hold is waited out: it is only ever brief, and a caller
    /// that skipped its work then (a group's unregistering) would leave
    /// the state wrong.
    fn try_lock(&self, shared: bool) -> Option<ExecGuard<'_>> {
        if shared {
            self.lock_shared();
        } else {
            if self.held.load(Ordering::Relaxed) {
                return None;
            }
            self.held.store(true, Ordering::Relaxed);
        }
        Some(ExecGuard { lock: self })
    }
}

#[cold]
#[inline(never)]
fn nested_borrow() -> ! {
    panic!("the task executor's state is already borrowed")
}

/// A borrow of the executor's state.
struct ExecGuard<'a> {
    lock: &'a ExecLock,
}

impl std::ops::Deref for ExecGuard<'_> {
    type Target = Exec;
    #[inline(always)]
    fn deref(&self) -> &Exec {
        // SAFETY: the guard holds the lock.
        unsafe { &*self.lock.exec.get() }
    }
}

impl std::ops::DerefMut for ExecGuard<'_> {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Exec {
        // SAFETY: the guard holds the lock, so no other reference exists.
        unsafe { &mut *self.lock.exec.get() }
    }
}

impl Drop for ExecGuard<'_> {
    #[inline(always)]
    fn drop(&mut self) {
        // One store for both kinds: a release store is a plain store on
        // one worker's own flag.
        self.lock.held.store(false, Ordering::Release);
    }
}

thread_local! {
    static LOCAL: ExecLock = ExecLock::new();
}

static SHARED: LazyLock<ExecLock> = LazyLock::new(ExecLock::new);

/// Whether the pool runs tasks on more than one worker thread: decided
/// once, from `KARAC_THREADS` (else the core count), before the first task
/// runs.
#[inline(always)]
pub(crate) fn pooled() -> bool {
    static MODE: AtomicU8 = AtomicU8::new(0);
    match MODE.load(Ordering::Relaxed) {
        1 => false,
        2 => true,
        _ => pooled_first(&MODE),
    }
}

#[cold]
#[inline(never)]
fn pooled_first(mode: &AtomicU8) -> bool {
    let n = std::env::var("KARAC_THREADS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or_else(default_threads);
    let pooled = n > 1;
    THREADS.store(n.max(1), Ordering::Relaxed);
    mode.store(if pooled { 2 } else { 1 }, Ordering::Relaxed);
    pooled
}

/// `__pooled()`: whether the pool runs more than one worker, which a
/// function with auto-par sites asks on entry and, when not, runs its
/// in-order twin instead (B-2026-10-26-17).
#[no_mangle]
pub extern "C" fn karac_mir_pooled() -> u8 {
    pooled() as u8
}

/// The workers a program runs on when `KARAC_THREADS` is unset: one per
/// core the process may use (M4, Gowtham 2026-10-26); `KARAC_THREADS=1`
/// opts out. One on wasm, where the pool has no threads to start. One in
/// this crate's unit tests too: they pin the one-worker executor's FIFO
/// order, and the count is decided once per process, so no test can ask
/// for one worker itself (the pool is tested through compiled programs).
fn default_threads() -> usize {
    if cfg!(target_family = "wasm") || cfg!(test) {
        return 1;
    }
    #[cfg(target_os = "linux")]
    {
        linux_cores()
    }
    #[cfg(not(target_os = "linux"))]
    {
        std::thread::available_parallelism().map_or(1, |n| n.get())
    }
}

/// The cores this process may use on Linux: those in its affinity mask,
/// and no more than its cgroup's CPU quota allows (cgroup v2's `cpu.max`,
/// else v1's `cpu.cfs_quota_us` over `cpu.cfs_period_us`), as
/// `available_parallelism` counts them. The quota is read at the root of
/// the cgroup namespace, which is a container's own; std also walks the
/// nested groups through `/proc`, with parsers of its own that weighed
/// 16 KB in every binary, since every program asks (B-2026-10-28-8).
#[cfg(target_os = "linux")]
fn linux_cores() -> usize {
    extern "C" {
        fn sched_getaffinity(pid: i32, size: usize, mask: *mut u64) -> i32;
        fn sysconf(name: i32) -> i64;
    }
    // 1,024 cores; a larger machine answers EINVAL, and counts online ones.
    let mut mask = [0u64; 16];
    // SAFETY: `mask` holds the bytes it is said to.
    let cores =
        if unsafe { sched_getaffinity(0, std::mem::size_of_val(&mask), mask.as_mut_ptr()) } == 0 {
            mask.iter().map(|w| w.count_ones() as usize).sum()
        } else {
            // SAFETY: plain call; _SC_NPROCESSORS_ONLN.
            unsafe { sysconf(84) }.max(1) as usize
        };
    let mut buf = [0u8; 64];
    // "<quota> <period>", or "max <period>" without a quota.
    let v2 = cgroup_file(c"/sys/fs/cgroup/cpu.max", &mut buf);
    let mut words = v2.split(|&b| b == b' ' || b == b'\n');
    let quota = match (whole(words.next()), whole(words.next())) {
        (Some(q), Some(p)) => Some((q, p)),
        // A v1 quota without a limit is -1, which is no whole number.
        _ => whole(Some(cgroup_file(
            c"/sys/fs/cgroup/cpu/cpu.cfs_quota_us",
            &mut buf,
        )))
        .zip(whole(Some(cgroup_file(
            c"/sys/fs/cgroup/cpu/cpu.cfs_period_us",
            &mut [0u8; 64],
        )))),
    };
    match quota {
        Some((q, p)) if p > 0 => cores.min((q / p).max(1)),
        _ => cores.max(1),
    }
}

/// The start of the file at `path` in `buf`, without its trailing newline;
/// empty when it cannot be read.
#[cfg(target_os = "linux")]
fn cgroup_file<'a>(path: &std::ffi::CStr, buf: &'a mut [u8; 64]) -> &'a [u8] {
    extern "C" {
        fn open(path: *const std::ffi::c_char, flags: i32, ...) -> i32;
        fn read(fd: i32, buf: *mut u8, n: usize) -> isize;
        fn close(fd: i32) -> i32;
    }
    // SAFETY: a NUL-terminated path, opened read-only.
    let fd = unsafe { open(path.as_ptr(), 0) };
    if fd < 0 {
        return &[];
    }
    // SAFETY: `buf` holds `buf.len()` bytes; `fd` was just opened.
    let got = unsafe { read(fd, buf.as_mut_ptr(), buf.len()) };
    // SAFETY: as above.
    unsafe { close(fd) };
    let got = &buf[..got.max(0) as usize];
    got.strip_suffix(b"\n").unwrap_or(got)
}

/// `w` as a whole number of decimal digits.
#[cfg(target_os = "linux")]
fn whole(w: Option<&[u8]>) -> Option<usize> {
    let w = w.filter(|w| !w.is_empty() && w.iter().all(u8::is_ascii_digit))?;
    Some(w.iter().fold(0usize, |n, &d| {
        n.saturating_mul(10).saturating_add(usize::from(d - b'0'))
    }))
}

/// How many workers the pool runs (`KARAC_THREADS`, else the core count).
static THREADS: AtomicUsize = AtomicUsize::new(1);

fn threads() -> usize {
    pooled();
    THREADS.load(Ordering::Relaxed)
}

/// The executor's state as one access sees it: this thread's own, or the
/// pool's.
struct ExecRef {
    lock: &'static ExecLock,
    shared: bool,
}

impl ExecRef {
    #[inline(always)]
    fn borrow(&self) -> ExecGuard<'static> {
        self.lock.lock(self.shared)
    }

    #[inline(always)]
    fn borrow_mut(&self) -> ExecGuard<'static> {
        self.borrow()
    }

    /// The state, unless this worker holds it now ([`ExecLock::try_lock`]).
    fn try_borrow_mut(&self) -> Result<ExecGuard<'static>, ()> {
        self.lock.try_lock(self.shared).ok_or(())
    }
}

/// The executor's state, reached as the thread-local `RefCell` it replaced
/// was: `EXEC.with(|e| e.borrow_mut() ..)`.
struct ExecCell;

impl ExecCell {
    #[inline(always)]
    fn get(&self) -> Option<ExecRef> {
        if pooled() {
            return Some(ExecRef {
                lock: &SHARED,
                shared: true,
            });
        }
        // SAFETY: the thread's own state lives as long as the thread;
        // `try_with` fails once its destructors have begun.
        let lock = LOCAL
            .try_with(|l| unsafe { &*(l as *const ExecLock) })
            .ok()?;
        Some(ExecRef {
            lock,
            shared: false,
        })
    }

    #[inline(always)]
    fn with<R>(&self, f: impl FnOnce(&ExecRef) -> R) -> R {
        match self.get() {
            Some(e) => f(&e),
            None => panic!("the task executor's state is gone"),
        }
    }

    /// As `with`, failing once the thread's own state is torn down at
    /// exit.
    #[inline(always)]
    fn try_with<R>(&self, f: impl FnOnce(&ExecRef) -> R) -> Result<R, ()> {
        self.get().map(|e| f(&e)).ok_or(())
    }
}

static EXEC: ExecCell = ExecCell;

/// State a native keeps for the whole program, as the executor's is: this
/// thread's own on one worker, and one copy behind a lock under the pool,
/// where a task can suspend on one worker and resume on another. Declared
/// with [`program_state!`].
pub(crate) struct ProgramState<T: 'static> {
    local: &'static std::thread::LocalKey<RefCell<T>>,
    shared: &'static std::sync::Mutex<T>,
}

impl<T> ProgramState<T> {
    pub(crate) const fn new(
        local: &'static std::thread::LocalKey<RefCell<T>>,
        shared: &'static std::sync::Mutex<T>,
    ) -> ProgramState<T> {
        ProgramState { local, shared }
    }

    /// `f` of the state. The pool's lock is no `RefCell`: a nested `with`
    /// of the same state waits for ever, so none nests one.
    #[inline]
    pub(crate) fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        if pooled() {
            f(&mut self.shared.lock().unwrap_or_else(|p| p.into_inner()))
        } else {
            self.local.with(|c| f(&mut c.borrow_mut()))
        }
    }
}

/// `static NAME: T = init;` as a [`ProgramState`] (`init` is constant).
macro_rules! program_state {
    ($(#[$m:meta])* $vis:vis static $name:ident: $t:ty = $init:expr;) => {
        $(#[$m])*
        $vis static $name: $crate::mir_task::ProgramState<$t> = {
            thread_local! {
                static LOCAL: std::cell::RefCell<$t> = const { std::cell::RefCell::new($init) };
            }
            static SHARED: std::sync::Mutex<$t> = std::sync::Mutex::new($init);
            $crate::mir_task::ProgramState::new(&LOCAL, &SHARED)
        };
    };
}
pub(crate) use program_state;

thread_local! {
    /// This thread's worker number in the pool: 0 for the thread that ran
    /// `karac_mir_task_run`, and for any thread outside the pool.
    static WORKER: Cell<usize> = const { Cell::new(0) };
}

/// This thread's worker number ([`WORKER`]).
#[inline(always)]
fn worker() -> usize {
    if pooled() {
        WORKER.with(Cell::get)
    } else {
        0
    }
}

thread_local! {
    /// Set by a native that answered `IO_NOT_READY` ([`set_again`]) and
    /// taken by the `__io_again` after it.
    static AGAIN: Cell<bool> = const { Cell::new(false) };
}

fn bump() {
    EXEC.with(|e| e.borrow_mut().progressed());
}

/// `TaskGroup.new(limit: n)`: a group that lets at most `limit` of its
/// tasks run at once.
#[no_mangle]
pub extern "C" fn karac_mir_group_new(limit: i64) -> *mut u8 {
    group_new(limit, false)
}

/// A group of at most `limit` running tasks, registered.
fn group_new(limit: i64, isolating: bool) -> *mut u8 {
    EXEC.with(|e| {
        let mut e = e.borrow_mut();
        e.group_ids += 1;
        let g = Box::into_raw(Box::new(Group {
            id: e.group_ids,
            limit,
            live: 0,
            dropped: false,
            owner: e.cur().map_or(0, |c| c.id),
            deadline: None,
            isolating,
            hidden: false,
            members: Vec::new(),
            next: 0,
        }));
        let id = e.group_ids;
        e.groups.insert(id, g);
        g as *mut u8
    })
}

/// `TaskGroup.isolating(limit: n)` (core-semantics §11.12): a group as
/// `karac_mir_group_new` makes one, whose tasks' panics end the task, not
/// the process.
#[no_mangle]
pub extern "C" fn karac_mir_group_isolating(limit: i64) -> *mut u8 {
    group_new(limit, true)
}

/// Marks auto-par's group `g` hidden (B-2026-10-27-22): its tasks log as
/// their spawner, in the order they were spawned.
///
/// # Safety
/// `g` is a live group handle with no task yet.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_group_hide(g: *mut u8) {
    EXEC.with(|e| {
        let _held = e.borrow_mut();
        // SAFETY: the caller's contract, under the executor's lock.
        unsafe { (*(g as *mut Group)).hidden = true };
    });
}

/// A panic (core-semantics §11.12): when the running task is a task of an
/// isolating group, it takes the panic. The first panic's `message()` and
/// `location` are recorded for its handle, the groups the task made are
/// cancelled, as for a cancelled task (§11.9), and its frame chain leaves
/// by its cancel exits, as a teardown does; answers true. A panic while
/// one unwinds keeps the first. Anywhere else answers false, and the
/// caller ends the process.
pub(crate) fn contain_panic(message: impl FnOnce() -> String, location: &str) -> bool {
    let Some(id) = EXEC.with(|e| {
        let e = e.borrow();
        let c = e.cur()?;
        // SAFETY: a group outlives its tasks.
        (!c.group.is_null() && unsafe { (*c.group).isolating }).then_some(c.id)
    }) else {
        return false;
    };
    let fresh = EXEC.with(|e| !e.borrow().panics.contains_key(&id));
    if fresh {
        let message = message();
        EXEC.with(|e| {
            e.borrow_mut()
                .panics
                .insert(id, (message, location.to_string()))
        });
        cancel_owned(id);
    }
    EXEC.with(|e| {
        if let Some(c) = e.borrow_mut().cur_mut().as_mut() {
            c.unwind = true;
        }
    });
    true
}

/// Why the running task, whose frame left by its cancel exits, gave no
/// value: 0 when it is no task of an isolating group, 1 when it was
/// cancelled, and 2 when it panicked, with the panic's message and
/// location, taken (§11.12).
pub(crate) fn task_failure() -> (u8, Option<(String, String)>) {
    EXEC.with(|e| {
        let mut e = e.borrow_mut();
        let Some(c) = e.cur() else {
            return (0, None);
        };
        // SAFETY: a group outlives its tasks.
        if c.group.is_null() || !unsafe { (*c.group).isolating } {
            return (0, None);
        }
        match e.panics.remove(&c.id) {
            Some(p) => (2, Some(p)),
            None => (1, None),
        }
    })
}

/// Frees group `g`, dropped with no task left.
fn free_group(g: *mut Group) {
    // SAFETY: `g` is live, and freed once.
    let g = unsafe { Box::from_raw(g) };
    let _ = EXEC.try_with(|e| {
        if let Ok(mut e) = e.try_borrow_mut() {
            e.groups.remove(&g.id);
            e.deadlines.retain(|&(_, id)| id != g.id);
        }
    });
}

/// `TaskGroup.cancel(g)` (core-semantics §11.9): every task of `g` that
/// has not finished is torn down at the wait it is suspended in, now, or,
/// when it is the running task, at the next wait it reaches.
///
/// # Safety
/// `g` is a live group handle.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_group_cancel(g: *mut u8) {
    let g = g as *mut Group;
    cancel_where(&|t| t.group == g);
}

/// `TaskGroup.__deadline_at(g, at)` (§11.10): `g` is cancelled once the
/// monotonic clock reads `at` nanoseconds, or at an earlier deadline it
/// has.
///
/// # Safety
/// `g` is a live group handle.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_group_deadline(g: *mut u8, at_ns: i64) {
    let left = at_ns - crate::karac_runtime_clock_monotonic_ns();
    let at = Instant::now() + Duration::from_nanos(left.max(0) as u64);
    EXEC.with(|e| {
        let mut e = e.borrow_mut();
        // SAFETY: the caller's contract; a group's fields change under the
        // executor's lock.
        let g = unsafe { &mut *(g as *mut Group) };
        let at = g.deadline.map_or(at, |d| d.min(at));
        g.deadline = Some(at);
        e.deadlines.push((at, g.id));
    });
}

/// A group's drop. Under the executor its tasks have all returned (the
/// coroutine transform waits for that first); one still running frees the
/// group when it returns.
///
/// # Safety
/// `g` is a live group handle, dropped once.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_group_drop(g: *mut u8) {
    let g = g as *mut Group;
    let free = EXEC.with(|e| {
        let _held = e.borrow_mut();
        // SAFETY: the caller's contract, under the executor's lock.
        unsafe {
            (*g).dropped = true;
            (*g).live == 0
        }
    });
    if free {
        free_group(g);
    }
}

/// `__ready("TaskGroup.spawn_frame", g)` (`end` 0): whether `g` runs fewer
/// than its limit of tasks; `__ready("TaskGroup.end", g)` (`end` 1):
/// whether all its tasks have returned, before it drops.
///
/// # Safety
/// `g` is a live group handle.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_ready_group(g: *mut u8, end: u8) -> u8 {
    let ready = EXEC.with(|e| {
        let _held = e.borrow();
        // SAFETY: the caller's contract, under the executor's lock.
        let g = unsafe { &*(g as *const Group) };
        if end != 0 {
            g.live == 0
        } else {
            g.live < g.limit
        }
    });
    note(ready, Wait::Task)
}

/// Task `t` ended, returned or torn down: its group runs one task fewer,
/// and the end hooks hear of it.
fn task_done(t: &Task) {
    let group = t.group;
    if !group.is_null() {
        // SAFETY: as below.
        if unsafe { (*group).hidden } {
            log_hold_done(t.id);
        }
        let free = EXEC.with(|e| {
            let _held = e.borrow_mut();
            // SAFETY: a group outlives its tasks (`karac_mir_group_drop`);
            // its fields change under the executor's lock.
            unsafe {
                (*group).live -= 1;
                (*group).dropped && (*group).live == 0
            }
        });
        if free {
            free_group(group);
        }
    }
    bump();
    EXEC.with(|e| e.borrow_mut().panics.remove(&t.id));
    let hooks = EXEC.with(|e| e.borrow().hooks.clone());
    for h in hooks {
        h(t.id);
    }
}

/// The running task's id, inside the executor.
#[allow(dead_code)] // for the natives that keep per-task state (M4 item 5)
pub(crate) fn current_task() -> Option<u64> {
    EXEC.with(|e| e.borrow().cur().map(|c| c.id))
}

/// `std.log`'s `task_id` for the running task: 0 for the root task and
/// outside every task (`main`), and an auto-par task's spawner's.
pub(crate) fn log_task_id() -> u64 {
    EXEC.with(|e| e.borrow().cur().map_or(0, |c| c.log_id))
}

/// Whether the running task's `std.log` records go through a [`Hold`]: it
/// is an auto-par task, or runs inside one.
pub(crate) fn holding_logs() -> bool {
    EXEC.with(|e| e.borrow().cur().is_some_and(|c| c.hold != 0))
}

/// Keeps the records [`Hold`]s let go reaching the backend in the order
/// they were let go: taken before the executor's lock, and held until the
/// backend has them.
static LOG_ORDER: std::sync::Mutex<()> = std::sync::Mutex::new(());

impl Exec {
    /// Record `h`, logged through hold `key`: waits in the first hold on
    /// the way up that is not its group's first unfinished task, or, when
    /// there is none, joins `out` for the backend.
    fn route_log(
        &mut self,
        mut key: u64,
        h: crate::mir_log::Held,
        out: &mut Vec<crate::mir_log::Held>,
    ) {
        while let Some(hold) = self.holds.get_mut(&key) {
            // SAFETY: a group outlives its tasks' holds.
            if unsafe { (*hold.group).next } != hold.seq {
                hold.held.push(h);
                return;
            }
            key = hold.up;
        }
        out.push(h);
    }
}

/// A record the running task logged, through its [`Hold`].
pub(crate) fn hold_log(h: crate::mir_log::Held) {
    let _order = LOG_ORDER.lock().unwrap_or_else(|p| p.into_inner());
    let mut out = Vec::new();
    EXEC.with(|e| {
        let mut e = e.borrow_mut();
        let key = e.cur().map_or(0, |c| c.hold);
        e.route_log(key, h, &mut out);
    });
    for h in out {
        h.release();
    }
}

/// A panic is about to end the program (B-2026-10-27-30): every record a
/// [`Hold`] still holds goes to the backend, in statement order, before
/// the panic is reported. A task still running gives what it logged so
/// far. A hold's own records come first, then those of the groups whose
/// tasks it spawned, in the order the groups were made, each group's in
/// its tasks' order.
pub(crate) fn drain_logs() {
    // A panic inside a backend's `emit` while records were being let go:
    // the drain would wait on itself.
    let Ok(_order) = LOG_ORDER.try_lock() else {
        return;
    };
    let mut out = Vec::new();
    let _ = EXEC.try_with(|e| {
        let Ok(mut e) = e.try_borrow_mut() else {
            return;
        };
        // The holds under each spawner's hold, by group and place.
        let mut under: IdMap<u64, Vec<(u64, usize, u64)>> = IdMap::default();
        for (&key, hold) in &e.holds {
            // SAFETY: a group outlives its tasks' holds.
            let group = unsafe { (*hold.group).id };
            under
                .entry(hold.up)
                .or_default()
                .push((group, hold.seq, key));
        }
        for holds in under.values_mut() {
            holds.sort_unstable();
        }
        fn drain(
            e: &mut Exec,
            under: &IdMap<u64, Vec<(u64, usize, u64)>>,
            up: u64,
            out: &mut Vec<crate::mir_log::Held>,
        ) {
            for &(_, _, key) in under.get(&up).map_or(&[][..], Vec::as_slice) {
                if let Some(hold) = e.holds.get_mut(&key) {
                    out.append(&mut hold.held);
                }
                drain(e, under, key, out);
            }
        }
        drain(&mut e, &under, 0, &mut out);
    });
    for h in out {
        h.release();
    }
}

/// The task with hold `key` finished: while its group's first unfinished
/// task has finished, the next one is the first, and the records the new
/// first one held go on.
fn log_hold_done(key: u64) {
    let _order = LOG_ORDER.lock().unwrap_or_else(|p| p.into_inner());
    let mut out = Vec::new();
    EXEC.with(|e| {
        let mut e = e.borrow_mut();
        let Some(hold) = e.holds.get_mut(&key) else {
            return;
        };
        hold.done = true;
        // SAFETY: a group outlives its tasks; under the executor's lock.
        let g = unsafe { &mut *hold.group };
        while let Some(&id) = g.members.get(g.next) {
            let Some(hold) = e.holds.get_mut(&id) else {
                break;
            };
            let (up, done) = (hold.up, hold.done);
            let held = std::mem::take(&mut hold.held);
            for h in held {
                e.route_log(up, h, &mut out);
            }
            if !done {
                break;
            }
            e.holds.remove(&id);
            g.next += 1;
        }
    });
    for h in out {
        h.release();
    }
}

/// Calls `f` with a task's id whenever a task ends, returned or torn down.
#[allow(dead_code)] // for the natives that keep per-task state (M4 item 5)
pub(crate) fn on_task_end(f: fn(u64)) {
    EXEC.with(|e| e.borrow_mut().hooks.push(f));
}

/// Cancels task `id` (core-semantics §11.9), as `TaskGroup.cancel` cancels
/// each task of its group.
#[allow(dead_code)] // for the natives that keep per-task state (M4 item 5)
pub(crate) fn cancel(id: u64) {
    cancel_where(&|t| t.id == id);
}

/// Cancels the tasks of each group task `owner` made, the oldest group
/// first.
fn cancel_owned(owner: u64) {
    let mut owned: Vec<(u64, usize)> = EXEC.with(|e| {
        e.borrow()
            .groups
            .iter()
            // SAFETY: a registered group is live.
            .filter(|(_, &g)| unsafe { (*g).owner } == owner)
            .map(|(&id, &g)| (id, g as usize))
            .collect()
    });
    sort_by_id(&mut owned);
    for (_, g) in owned {
        cancel_where(&|c| c.group == g as *mut Group);
    }
}

/// Sorts `v` by its ids, which are unique: a heapsort, a few hundred bytes
/// where the library's sorts take kilobytes per element type, in every
/// binary that runs a task (B-2026-10-28-8).
fn sort_by_id(v: &mut [(u64, usize)]) {
    fn sift(v: &mut [(u64, usize)], mut i: usize, n: usize) {
        loop {
            let mut c = 2 * i + 1;
            if c >= n {
                return;
            }
            if c + 1 < n && v[c + 1].0 > v[c].0 {
                c += 1;
            }
            if v[i].0 >= v[c].0 {
                return;
            }
            v.swap(i, c);
            i = c;
        }
    }
    let n = v.len();
    for i in (0..n / 2).rev() {
        sift(v, i, n);
    }
    for end in (1..n).rev() {
        v.swap(0, end);
        sift(v, 0, end);
    }
}

/// Cancels each unfinished task `doomed` picks: a suspended one is torn
/// down now, in the order they were spawned, and a running one (or one a
/// teardown interrupted) at the next wait it reaches.
fn cancel_where(doomed: &dyn Fn(&Cur) -> bool) {
    let tasks: Vec<Task> = EXEC.with(|e| {
        let mut e = e.borrow_mut();
        let e = &mut *e;
        for c in e
            .cur
            .iter_mut()
            .chain(e.curs.iter_mut().flatten())
            .chain(e.outers.iter_mut().flatten())
        {
            if doomed(c) && !c.torn {
                c.cancel = true;
            }
        }
        let pick = |t: &Task| !t.cancel && doomed(&t.cur());
        let mut out = Vec::new();
        let queue = std::mem::take(&mut e.queue);
        for t in queue {
            if pick(&t) {
                out.push(t);
            } else {
                e.queue.push_back(t);
            }
        }
        let keys: Vec<u64> = e
            .off
            .iter()
            .filter(|(_, t)| pick(t))
            .map(|(&k, _)| k)
            .collect();
        out.extend(keys.into_iter().filter_map(|k| e.take_key(k)));
        out
    });
    let mut order: Vec<(u64, usize)> = tasks.iter().map(|t| t.id).zip(0..).collect();
    sort_by_id(&mut order);
    let mut tasks: Vec<Option<Task>> = tasks.into_iter().map(Some).collect();
    for (_, i) in order {
        if let Some(t) = tasks[i].take() {
            tear_down(t);
        }
    }
}

/// Tears task `t` down (core-semantics §11.9): the groups it made first,
/// since their tasks may borrow from its frame, then the task itself,
/// resumed cancelled, which leaves its wait by the cancel exits of its
/// frames. A task that has not reached a wait yet runs to its first one.
/// One whose exit suspends, or that waits where no cancel exit is, goes
/// back to wait as before, still cancelled.
fn tear_down(mut t: Task) {
    cancel_owned(t.id);
    t.cancel = true;
    for _ in 0..2 {
        if poll_nested(&mut t) {
            task_done(&t);
            return;
        }
        if t.torn {
            break;
        }
    }
    settle(t);
}

/// Resumes `t` once, from inside whatever runs now; answers whether it
/// returned.
fn poll_nested(t: &mut Task) -> bool {
    EXEC.with(|e| {
        let mut e = e.borrow_mut();
        if let Some(c) = e.cur_mut().take() {
            e.outer_mut().push(c);
        }
        *e.cur_mut() = Some(t.cur());
    });
    let outer = crate::mir::swap_spans(std::mem::take(&mut t.spans));
    // SAFETY: the task has not answered 1 yet.
    let done = unsafe { (t.poll)(t.a, t.b) } != 0;
    t.spans = crate::mir::swap_spans(outer);
    let cur = EXEC.with(|e| {
        let mut e = e.borrow_mut();
        let cur = e.cur_mut().take();
        *e.cur_mut() = e.outer_mut().pop();
        cur
    });
    if let Some(cur) = cur {
        t.keep(cur);
    }
    done
}

/// Puts `t`, which did not return, where its wait says: asleep, parked on
/// its descriptor, or on the queue. Answers whether it went on the queue.
fn settle(t: Task) -> bool {
    match t.wait {
        Wait::Until(at) => {
            EXEC.with(|e| {
                let mut e = e.borrow_mut();
                let key = e.put_off(t);
                e.timers.push(Reverse((at, key)));
                // Only the first worker wakes sleepers: one napping
                // without a deadline to wake for has one now.
                e.rouse_first();
            });
            false
        }
        Wait::Fd(fd, events) => {
            park_fd(t, fd, events);
            false
        }
        // Whichever comes first, the descriptor or the deadline, wakes
        // the task.
        Wait::FdBy(fd, events, at) => {
            if let Some(key) = park_fd(t, fd, events) {
                EXEC.with(|e| {
                    let mut e = e.borrow_mut();
                    e.timers.push(Reverse((at, key)));
                    e.rouse_first();
                });
            }
            false
        }
        _ => {
            // Back on the queue, for this worker or the first that looks:
            // no napping worker wakes for a task that was queued already.
            EXEC.with(|e| e.borrow_mut().queue.push_back(t));
            true
        }
    }
}

/// Cancels each group whose deadline has passed (§11.10).
fn fire_deadlines() {
    loop {
        let due = EXEC.with(|e| {
            let mut e = e.borrow_mut();
            let e = &mut *e;
            let now = Instant::now();
            let i = e.deadlines.iter().position(|&(at, _)| at <= now)?;
            let (at, id) = e.deadlines.swap_remove(i);
            let g = *e.groups.get(&id)?;
            // SAFETY: a registered group is live.
            let gr = unsafe { &mut *g };
            (gr.deadline == Some(at)).then(|| {
                gr.deadline = None;
                g
            })
        });
        match due {
            Some(g) => cancel_where(&|c| c.group == g),
            None => {
                let more = EXEC.with(|e| {
                    let e = e.borrow();
                    let now = Instant::now();
                    e.deadlines.iter().any(|&(at, _)| at <= now)
                });
                if !more {
                    return;
                }
            }
        }
    }
}

/// `__cancelled()`, in a readiness loop resumed from its wait: whether the
/// running task was cancelled, once, when its teardown begins.
#[no_mangle]
pub extern "C" fn karac_mir_cancelled() -> u8 {
    EXEC.with(|e| match e.borrow_mut().cur_mut().as_mut() {
        Some(c) if c.cancel && !c.torn => {
            c.torn = true;
            c.deadline = None;
            c.slept = false;
            c.inflight = None;
            1
        }
        _ => 0,
    })
}

/// `__unwinding()`, after a suspending call: whether it returned by its
/// cancel exit.
#[no_mangle]
pub extern "C" fn karac_mir_unwinding() -> u8 {
    EXEC.with(|e| u8::from(e.borrow().cur().is_some_and(|c| c.unwind)))
}

/// `__unwind(on)`: a cancel exit begins (`on` 0) or ends (`on` 1).
#[no_mangle]
pub extern "C" fn karac_mir_unwind(on: u8) {
    EXEC.with(|e| {
        if let Some(c) = e.borrow_mut().cur_mut().as_mut() {
            c.unwind = on != 0;
        }
    });
}

/// Queues a task of the group `group` (null for `main`).
///
/// # Safety
/// `poll(a, b)` may be called until it answers 1; `group` is null or a
/// live group handle.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_task_spawn(
    group: *mut u8,
    poll: TaskPoll,
    a: *mut u8,
    b: *mut u8,
) {
    let group = group as *mut Group;
    let id = EXEC.with(|e| {
        let mut e = e.borrow_mut();
        if !group.is_null() {
            // SAFETY: the caller's contract, under the executor's lock.
            unsafe { (*group).live += 1 };
        }
        e.ids += 1;
        let id = e.ids;
        // An auto-par task logs as its spawner, in spawn order.
        let up = e.cur().map_or(0, |c| c.hold);
        // SAFETY: as above.
        let (log_id, hold) = if !group.is_null() && unsafe { (*group).hidden } {
            let seq = unsafe {
                (*group).members.push(id);
                (*group).members.len() - 1
            };
            e.holds.insert(
                id,
                Hold {
                    group,
                    seq,
                    up,
                    done: false,
                    held: Vec::new(),
                },
            );
            (e.cur().map_or(0, |c| c.log_id), id)
        } else {
            (id, up)
        };
        e.push(Task {
            id,
            group,
            poll,
            a,
            b,
            deadline: None,
            slept: false,
            wait: Wait::None,
            inflight: None,
            spans: crate::mir::current_spans(),
            cancel: false,
            torn: false,
            unwind: false,
            seen: 0,
            root: false,
            log_id,
            hold,
        });
        e.progress += 1;
        id
    });
    // The pool starts with the first task beside the root, which the main
    // thread runs inline until a worker takes one: a program that reaches
    // no site never leaves one thread (B-2026-10-28-15).
    if id > 1 {
        start_workers();
    }
}

/// Runs the root task `poll(a, b)` and every task spawned meanwhile until
/// all have returned.
///
/// # Safety
/// As [`karac_mir_task_spawn`].
#[no_mangle]
pub unsafe extern "C" fn karac_mir_task_run(poll: TaskPoll, a: *mut u8, b: *mut u8) {
    // SAFETY: the caller's contract.
    unsafe { karac_mir_task_spawn(std::ptr::null_mut(), poll, a, b) };
    EXEC.with(|e| {
        if let Some(t) = e.borrow_mut().queue.back_mut() {
            t.root = true;
            t.log_id = 0;
            t.hold = 0;
        }
    });
    // Tasks that waited on another task in a row with nothing happening in
    // between.
    let mut idle = 0usize;
    // The progress count when that run began: another worker's progress
    // meanwhile ends it too.
    let mut quiet = 0u64;
    // Tasks run since the reactor was last asked.
    let mut since = 0usize;
    loop {
        // Once a round, the parked tasks whose descriptor is ready, and the
        // sleepers whose deadline passed, join the queue.
        let ask = EXEC.with(|e| {
            let e = e.borrow();
            !e.parked.is_empty() && since >= e.queue.len()
        });
        fire_deadlines();
        let mut woken = 0;
        if ask {
            woken += wait_parked(Some(Duration::ZERO));
            since = 0;
        }
        woken += EXEC.with(|e| e.borrow_mut().wake_timers());
        if woken > 0 {
            idle = 0;
        }
        // Under the pool, only a task worth polling: with none, every
        // queued task waits on another and was polled since the last
        // progress, so it waits for more.
        let popped = EXEC.with(|e| {
            let mut e = e.borrow_mut();
            let t = if pooled() {
                let progress = e.progress;
                let mut now = None;
                match e.queue.iter().position(|t| worth(t, progress, &mut now)) {
                    Some(i) => e.queue.remove(i),
                    None if e.queue.is_empty() => None,
                    None => return Err(Some(progress)),
                }
            } else {
                e.queue.pop_front()
            };
            let t = t.ok_or(None)?;
            *e.cur_mut() = Some(t.cur());
            if pooled() {
                e.running += 1;
            }
            Ok(t)
        });
        let mut t = match popped {
            Ok(t) => t,
            Err(Some(quiet)) => {
                block(quiet);
                idle = 0;
                continue;
            }
            Err(None) => {
                // Nothing can run here: another worker runs a task, or every
                // task left sleeps or waits on a descriptor, or none is left.
                // The queue is looked at again with the running count: a task
                // another worker settled since the pop is queued by now.
                let (queued, others) = EXEC.with(|e| {
                    let e = e.borrow();
                    (!e.queue.is_empty(), pooled() && e.running > 0)
                });
                if queued {
                    continue;
                }
                if others {
                    if !idle_wait(None) {
                        nap(None);
                    }
                    continue;
                }
                if !idle_wait(None) {
                    return;
                }
                idle = 0;
                continue;
            }
        };
        since += 1;
        let before = EXEC.with(|e| e.borrow().progress);
        let outer = crate::mir::swap_spans(std::mem::take(&mut t.spans));
        // SAFETY: the task has not answered 1 yet.
        let done = unsafe { (t.poll)(t.a, t.b) } != 0;
        t.spans = crate::mir::swap_spans(outer);
        let (after, cur) = EXEC.with(|e| {
            let mut e = e.borrow_mut();
            (e.progress, e.cur_mut().take())
        });
        if let Some(cur) = cur {
            t.keep(cur);
        }
        if done {
            task_done(&t);
            finished_polling();
            idle = 0;
            continue;
        }
        // Cancelled while it ran, it is torn down at the wait it reached.
        if t.cancel && !t.torn {
            tear_down(t);
            finished_polling();
            idle = 0;
            continue;
        }
        if after != before || t.wait == Wait::None {
            idle = 0;
        }
        if matches!(t.wait, Wait::Task | Wait::TaskBy(_)) && after == before {
            if idle == 0 {
                quiet = before;
            }
            idle += 1;
        }
        t.seen = before;
        let queued = settle(t);
        finished_polling();
        if !queued {
            continue;
        }
        let stuck = EXEC.with(|e| {
            let e = e.borrow();
            if e.progress != quiet {
                idle = 0;
            }
            idle > 0 && idle >= e.queue.len()
        });
        if stuck {
            block(quiet);
            idle = 0;
        }
    }
}

/// How long an idle worker, or the first one waiting on timers and the
/// reactor, waits before it looks again, under the pool.
const POOL_NAP: Duration = Duration::from_micros(200);

/// How long a napping worker waits for a task at most: a task queued
/// wakes it sooner.
const POOL_PARK: Duration = Duration::from_millis(10);

/// How many times a napping worker looks for a wake before it parks.
const POOL_SPINS: u32 = 4096;

/// A napping worker: its number, its thread, and the flag [`rouse`] sets,
/// which it watches while it spins before it parks.
struct Sleeper {
    worker: usize,
    thread: std::thread::Thread,
    woken: std::sync::Arc<AtomicBool>,
}

/// Wakes the napping worker `w`, one alone, so a wake brings one worker
/// to the lock rather than all of them.
fn rouse(w: Sleeper) {
    w.woken.store(true, Ordering::Release);
    w.thread.unpark();
}

thread_local! {
    /// This worker's [`Sleeper::woken`].
    static WOKEN: std::sync::Arc<AtomicBool> = std::sync::Arc::new(AtomicBool::new(false));
}

/// Starts the pool's other workers once, when it runs more than one.
fn start_workers() {
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.load(Ordering::Relaxed) || !pooled() || STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    // While this is the only thread: see [`Reactor::open`].
    EXEC.with(|e| e.borrow_mut().reactor.open());
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    crate::alloc::small::start();
    spawn_worker(1);
}

/// Starts worker `w`, which starts the next one before it looks for work:
/// the main thread pays for one thread's creation, not one per core
/// (B-2026-10-28-15).
fn spawn_worker(w: usize) {
    if w >= threads() {
        return;
    }
    // A failed start leaves the pool with the workers it has.
    let _ = std::thread::Builder::new()
        .name(format!("karac-worker-{w}"))
        // As deep as the main thread's: a poll runs the task's whole
        // call chain.
        .stack_size(8 << 20)
        .spawn(move || {
            spawn_worker(w + 1);
            worker_loop(w)
        });
}

/// A task this worker polled is settled: it no longer counts as running.
#[inline(always)]
fn finished_polling() {
    if !pooled() {
        return;
    }
    EXEC.with(|e| {
        let mut e = e.borrow_mut();
        e.running -= 1;
        // The first worker naps while others run: the last of them done
        // with nothing left worth polling, it may be the one to finish the
        // program or find the tasks deadlocked. And the root task, which
        // only it polls, may be worth polling now.
        let progress = e.progress;
        let mut now = None;
        let root = e
            .queue
            .iter()
            .any(|t| t.root && worth(t, progress, &mut now));
        if root || e.running == 0 && !e.queue.iter().any(|t| worth(t, progress, &mut now)) {
            e.rouse_first();
        }
    });
}

/// Naps until a task is queued, progress is made or [`POOL_PARK`] passes:
/// the worker joins the sleepers unless the queue holds a task it would
/// run now ([`mine`]), or, with `quiet`, the progress count moved from it.
fn nap(quiet: Option<u64>) {
    let woken = WOKEN.with(std::sync::Arc::clone);
    let me = worker();
    let napping = EXEC.with(|e| {
        let mut e = e.borrow_mut();
        let progress = e.progress;
        let mut now = None;
        let napping = !e
            .queue
            .iter()
            .any(|t| mine(t, me) && worth(t, progress, &mut now))
            && quiet.is_none_or(|q| progress == q);
        if napping {
            woken.store(false, Ordering::Relaxed);
            e.sleepers.push(Sleeper {
                worker: me,
                thread: std::thread::current(),
                woken: std::sync::Arc::clone(&woken),
            });
        }
        napping
    });
    if napping {
        // A while spinning first: a task that waits on another one running
        // now is usually ready again within microseconds, sooner than a
        // parked thread wakes.
        let mut spins = 0;
        while !woken.load(Ordering::Acquire) {
            spins += 1;
            if spins > POOL_SPINS {
                std::thread::park_timeout(POOL_PARK);
                break;
            }
            if spins % 64 == 0 {
                std::thread::yield_now();
            } else {
                std::hint::spin_loop();
            }
        }
        EXEC.with(|e| e.borrow_mut().sleepers.retain(|s| s.worker != me));
    }
}

/// Whether worker `w` polls `t`: the first polls every task, the others
/// all but the root.
#[inline(always)]
fn mine(t: &Task, w: usize) -> bool {
    w == 0 || !t.root
}

/// One of the pool's other workers: runs what the queue holds, and naps
/// when it holds nothing. The first worker alone wakes sleepers, asks the
/// reactor and reports a deadlock. It lives as long as the process, napping
/// once no task is left.
fn worker_loop(w: usize) {
    WORKER.with(|c| c.set(w));
    loop {
        // Only a task worth polling: the first worker polls every waiting
        // one again, so it alone can tell they all wait for ever.
        let Some((mut t, before)) = EXEC.with(|e| {
            let mut e = e.borrow_mut();
            let progress = e.progress;
            let mut now = None;
            let at = e
                .queue
                .iter()
                .position(|t| mine(t, w) && worth(t, progress, &mut now));
            let t = at.and_then(|i| e.queue.remove(i))?;
            *e.cur_mut() = Some(t.cur());
            e.running += 1;
            Some((t, progress))
        }) else {
            nap(None);
            continue;
        };
        let outer = crate::mir::swap_spans(std::mem::take(&mut t.spans));
        // SAFETY: the task has not answered 1 yet.
        let done = unsafe { (t.poll)(t.a, t.b) } != 0;
        t.spans = crate::mir::swap_spans(outer);
        if let Some(cur) = EXEC.with(|e| e.borrow_mut().cur_mut().take()) {
            t.keep(cur);
        }
        if done {
            task_done(&t);
        } else if t.cancel && !t.torn {
            tear_down(t);
        } else {
            t.seen = before;
            settle(t);
        }
        finished_polling();
    }
}

/// Every task in the queue waits on another, with nothing having happened
/// since each last ran: waits for a sleeper, a descriptor or a timed
/// wait's deadline, or reports the deadlock when there is none.
///
/// `quiet` is the progress count when the run of waiting tasks began: a
/// change since, on any worker, means they may not all wait any more.
fn block(quiet: u64) {
    let (others, by, runnable) = EXEC.with(|e| {
        let e = e.borrow();
        let by = e
            .queue
            .iter()
            .filter_map(|t| match t.wait {
                Wait::TaskBy(at) => Some(at),
                _ => None,
            })
            .min();
        // A task polled since the last progress, on any worker, that still
        // waits needs more progress; one that was not may be ready.
        let progress = e.progress;
        let mut now = None;
        (
            pooled() && e.running > 0,
            by,
            e.queue.iter().any(|t| worth(t, progress, &mut now)),
        )
    });
    // A task another worker polls now may be what the queue waits on;
    // meanwhile this worker still wakes sleepers and asks the reactor.
    if others {
        if !idle_wait(by) {
            nap(Some(quiet));
        }
        return;
    }
    if runnable {
        return;
    }
    if !idle_wait(by) {
        crate::mir::deadlock(
            "every task waits on a channel that no running task sends to or receives from",
        );
    }
}

/// Waits until `by`, the nearest sleeper's deadline or a parked
/// descriptor, whichever comes first, and queues what woke; false when
/// there is none of them to wait for.
fn idle_wait(by: Option<Instant>) -> bool {
    // Read under one lock with the running count: a worker puts its task
    // to sleep before it stops counting as running, so with none running
    // every timer is in the heap already, and with some running the wait
    // is short. Read apart, a sleeper another worker settled in between
    // was missed until the wait's old deadline (B-2026-10-27-5).
    let (next, group, parked, others) = EXEC.with(|e| {
        let mut e = e.borrow_mut();
        (
            e.next_deadline(),
            e.next_group_deadline(),
            !e.parked.is_empty(),
            pooled() && e.running > 0,
        )
    });
    // A group's deadline cancels its tasks (§11.10).
    let until = [next, by, group].into_iter().flatten().min();
    if until.is_none() && !parked {
        return false;
    }
    let mut timeout = until.map(|t| t.saturating_duration_since(Instant::now()));
    if others {
        // Other workers put tasks to sleep or park them meanwhile, and only
        // this one wakes sleepers: wait a little at a time. With none
        // running and the queue empty, nothing else can change.
        timeout = Some(timeout.map_or(POOL_NAP, |d| d.min(POOL_NAP)));
    }
    if parked {
        wait_parked(timeout);
    } else if let Some(d) = timeout {
        std::thread::sleep(d);
    }
    EXEC.with(|e| e.borrow_mut().wake_timers());
    fire_deadlines();
    true
}

/// The reactor's entries, reached through [`REACTOR`] so that a binary
/// that never waits on a descriptor (an auto-par kata) links neither, nor
/// the tables they grow: [`fd_wait`], which makes every descriptor wait,
/// is the only reference to them (B-2026-10-28-8).
struct ReactorOps {
    park: fn(Task, i32, i16) -> Option<u64>,
    wait: fn(Option<Duration>) -> usize,
}

static REACTOR_OPS: ReactorOps = ReactorOps {
    park,
    wait: wait_reactor,
};

/// Null until the first descriptor wait, then [`REACTOR_OPS`].
static REACTOR: AtomicPtr<ReactorOps> = AtomicPtr::new(std::ptr::null_mut());

/// The wait for `fd` to be ready for `events`, or until `by` at most.
fn fd_wait(fd: i32, events: i16, by: Option<Instant>) -> Wait {
    if REACTOR.load(Ordering::Relaxed).is_null() {
        // Opaque to the optimizer: seeing the one address ever stored,
        // it would call the entries directly, and link them everywhere.
        let ops = std::hint::black_box(&REACTOR_OPS as *const ReactorOps);
        REACTOR.store(ops as *mut ReactorOps, Ordering::Release);
    }
    match by {
        Some(at) => Wait::FdBy(fd, events, at),
        None => Wait::Fd(fd, events),
    }
}

fn reactor() -> Option<&'static ReactorOps> {
    // SAFETY: null, or the address of a static.
    unsafe { REACTOR.load(Ordering::Acquire).as_ref() }
}

/// [`park`], once a descriptor wait was made; before, there is no task to
/// park, and one would only be queued.
fn park_fd(t: Task, fd: i32, events: i16) -> Option<u64> {
    match reactor() {
        Some(r) => (r.park)(t, fd, events),
        None => {
            EXEC.with(|e| e.borrow_mut().wake(t));
            None
        }
    }
}

/// [`wait_reactor`], once a descriptor wait was made; before, no task is
/// parked, and the wait only sleeps out `timeout`.
fn wait_parked(timeout: Option<Duration>) -> usize {
    match reactor() {
        Some(r) => (r.wait)(timeout),
        None => {
            if let Some(d) = timeout {
                std::thread::sleep(d);
            }
            0
        }
    }
}

/// Parks `t` on `fd` until it is ready for `events`: with the reactor
/// watching it for every waiter's events. Answers the task's key; a
/// descriptor the reactor cannot watch leaves the task runnable, to ask
/// its readiness again.
fn park(t: Task, fd: i32, events: i16) -> Option<u64> {
    EXEC.with(|e| {
        let mut e = e.borrow_mut();
        let e = &mut *e;
        let want = e.parked.get(&fd).map_or(events, |keys| {
            keys.iter()
                .fold(events, |m, k| match e.off.get(k).map(|t| t.wait) {
                    Some(Wait::Fd(_, ev) | Wait::FdBy(_, ev, _)) => m | ev,
                    _ => m,
                })
        });
        if e.reactor.watch(fd, want) {
            let key = e.put_off(t);
            e.parked.entry(fd).or_default().push(key);
            Some(key)
        } else {
            e.wake(t);
            None
        }
    })
}

/// Asks the reactor, waiting up to `timeout` (none: until a descriptor is
/// ready), and queues the tasks parked on each ready descriptor; answers
/// how many.
fn wait_reactor(timeout: Option<Duration>) -> usize {
    let waiter = EXEC.with(|e| e.borrow().reactor.waiter());
    let ready = waiter.wait(timeout);
    EXEC.with(|e| {
        let mut e = e.borrow_mut();
        e.reactor.took(&ready);
        ready.into_iter().map(|fd| e.wake_fd(fd)).sum()
    })
}

/// Deregisters `fd` from the reactor before it is closed, so its number
/// can be reused, and queues the tasks parked on it, which then find it
/// closed.
#[no_mangle]
pub extern "C" fn karac_mir_io_forget(fd: i64) {
    // A socket closed while the thread's state is torn down at exit has
    // nothing left to tell.
    let _ = EXEC.try_with(|e| {
        if let Ok(mut e) = e.try_borrow_mut() {
            e.reactor.forget(fd as i32);
            e.wake_fd(fd as i32);
        }
    });
}

/// A call answered `IO_NOT_READY`, having done nothing: the readiness loop
/// around it goes back to its `__ready`.
pub(crate) fn set_again() {
    AGAIN.with(|a| a.set(true));
}

/// `__io_again()`, after a call in a readiness loop: whether it answered
/// `IO_NOT_READY` ([`set_again`]), clearing the bit.
#[no_mangle]
pub extern "C" fn karac_mir_io_again() -> u8 {
    u8::from(AGAIN.with(|a| a.replace(false)))
}

/// Keeps `fd`, the socket of a connect under way, on the running task: the
/// `connect` it retries ends that connect ([`take_inflight`]).
#[cfg_attr(not(all(feature = "net", unix)), allow(dead_code))]
pub(crate) fn set_inflight(fd: i32) {
    EXEC.with(|e| {
        if let Some(cur) = e.borrow_mut().cur_mut().as_mut() {
            cur.inflight = Some(fd);
        }
    });
}

/// The socket of the running task's connect under way, taken.
#[cfg_attr(not(all(feature = "net", unix)), allow(dead_code))]
pub(crate) fn take_inflight() -> Option<i32> {
    EXEC.with(|e| {
        e.borrow_mut()
            .cur_mut()
            .as_mut()
            .and_then(|c| c.inflight.take())
    })
}

#[cfg(unix)]
#[repr(C)]
struct PollFd {
    fd: i32,
    events: i16,
    revents: i16,
}

#[cfg(unix)]
extern "C" {
    fn poll(fds: *mut PollFd, n: u64, timeout: i32) -> i32;
}

/// `timeout` in whole milliseconds, rounded up so a deadline has passed
/// when a wait returns; -1 for none.
#[cfg(unix)]
fn timeout_ms(timeout: Option<Duration>) -> i32 {
    match timeout {
        Some(d) => d.as_micros().div_ceil(1000).min(i32::MAX as u128) as i32,
        None => -1,
    }
}

/// The reactor on Linux: one `epoll` set, each descriptor watched oneshot
/// for the events its parked tasks wait on, and re-armed when a task parks
/// on it again.
#[cfg(all(feature = "net", target_os = "linux"))]
#[derive(Default)]
struct Reactor {
    /// The `epoll` descriptor, made when the first task parks.
    ep: Option<i32>,
    /// The descriptors registered with it.
    known: std::collections::HashSet<i32, std::hash::BuildHasherDefault<IdHasher>>,
}

#[cfg(all(feature = "net", target_os = "linux"))]
impl Reactor {
    /// The `epoll` descriptor, made on the first call.
    ///
    /// Moved above the numbers a program's own descriptors take, so they
    /// are numbered as on the interpreter, which has no reactor descriptor.
    /// That grows the descriptor table, which in a process with more than
    /// one thread waits out an RCU grace period: 48 ms measured, under the
    /// executor's lock, with every worker stalled behind it (B-2026-10-27-5).
    /// So the pool opens it in [`start_workers`], before any other thread.
    fn open(&mut self) -> Option<i32> {
        if let Some(ep) = self.ep {
            return Some(ep);
        }
        // SAFETY: plain syscall.
        let mut ep = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
        if ep < 0 {
            return None;
        }
        // SAFETY: plain syscalls on the descriptor just made.
        let high = unsafe { libc::fcntl(ep, libc::F_DUPFD_CLOEXEC, 100) };
        if high >= 0 {
            // SAFETY: as above.
            unsafe { libc::close(ep) };
            ep = high;
        }
        self.ep = Some(ep);
        Some(ep)
    }

    fn watch(&mut self, fd: i32, events: i16) -> bool {
        let Some(ep) = self.open() else {
            return false;
        };
        let mut want = libc::EPOLLONESHOT as u32;
        if events & 1 != 0 {
            want |= libc::EPOLLIN as u32;
        }
        if events & 4 != 0 {
            want |= libc::EPOLLOUT as u32;
        }
        let ctl = |op: i32| {
            let mut ev = libc::epoll_event {
                events: want,
                u64: fd as u64,
            };
            // SAFETY: `ev` is initialized; a bad `fd` is an error return.
            if unsafe { libc::epoll_ctl(ep, op, fd, &mut ev) } == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error().raw_os_error())
            }
        };
        // A descriptor closed without `io_forget` left the set by itself,
        // and a new one may have its number.
        let done = match ctl(if self.known.contains(&fd) {
            libc::EPOLL_CTL_MOD
        } else {
            libc::EPOLL_CTL_ADD
        }) {
            Ok(()) => true,
            Err(Some(libc::ENOENT)) => ctl(libc::EPOLL_CTL_ADD).is_ok(),
            Err(Some(libc::EEXIST)) => ctl(libc::EPOLL_CTL_MOD).is_ok(),
            Err(_) => false,
        };
        if done {
            self.known.insert(fd);
        } else {
            self.known.remove(&fd);
        }
        done
    }

    fn forget(&mut self, fd: i32) {
        if let (Some(ep), true) = (self.ep, self.known.remove(&fd)) {
            // SAFETY: plain syscall; a closed `fd` is an error return.
            unsafe { libc::epoll_ctl(ep, libc::EPOLL_CTL_DEL, fd, std::ptr::null_mut()) };
        }
    }

    /// What a wait needs, taken under the executor's lock so the wait
    /// itself runs without it.
    fn waiter(&self) -> Waiter {
        Waiter(self.ep)
    }

    /// Takes note of the descriptors a wait found ready.
    fn took(&mut self, _ready: &[i32]) {}
}

/// A wait on the Linux reactor, run without the executor's lock: the
/// `epoll` set is the kernel's, and a task that parks meanwhile registers
/// with it on another worker.
#[cfg(all(feature = "net", target_os = "linux"))]
struct Waiter(Option<i32>);

#[cfg(all(feature = "net", target_os = "linux"))]
impl Waiter {
    fn wait(self, timeout: Option<Duration>) -> Vec<i32> {
        let Some(ep) = self.0 else {
            if let Some(d) = timeout {
                std::thread::sleep(d);
            }
            return Vec::new();
        };
        let mut evs = [libc::epoll_event { events: 0, u64: 0 }; 256];
        // SAFETY: `evs` holds 256 entries.
        let n = unsafe { libc::epoll_wait(ep, evs.as_mut_ptr(), 256, timeout_ms(timeout)) };
        evs[..n.max(0) as usize]
            .iter()
            .map(|ev| {
                let fd = { ev.u64 };
                fd as i32
            })
            .collect()
    }
}

/// The reactor elsewhere: `poll(2)` over the parked descriptors, only once
/// nothing can run or once a round.
#[cfg(not(all(feature = "net", target_os = "linux")))]
#[derive(Default)]
struct Reactor {
    fds: IdMap<i32, i16>,
}

#[cfg(not(all(feature = "net", target_os = "linux")))]
impl Reactor {
    fn open(&mut self) -> Option<i32> {
        None
    }

    fn watch(&mut self, fd: i32, events: i16) -> bool {
        self.fds.insert(fd, events);
        true
    }

    fn forget(&mut self, fd: i32) {
        self.fds.remove(&fd);
    }

    /// What a wait needs, taken under the executor's lock so the wait
    /// itself runs without it: the parked descriptors.
    fn waiter(&self) -> Waiter {
        Waiter(self.fds.iter().map(|(&fd, &ev)| (fd, ev)).collect())
    }

    /// Takes the descriptors a wait found ready off the watch list.
    fn took(&mut self, ready: &[i32]) {
        for fd in ready {
            self.fds.remove(fd);
        }
    }
}

/// A wait over the parked descriptors, run without the executor's lock.
#[cfg(not(all(feature = "net", target_os = "linux")))]
struct Waiter(Vec<(i32, i16)>);

#[cfg(not(all(feature = "net", target_os = "linux")))]
impl Waiter {
    #[cfg(unix)]
    fn wait(self, timeout: Option<Duration>) -> Vec<i32> {
        let mut p: Vec<PollFd> = self
            .0
            .iter()
            .map(|&(fd, events)| PollFd {
                fd,
                events,
                revents: 0,
            })
            .collect();
        // SAFETY: `p` holds `p.len()` initialized entries.
        unsafe { poll(p.as_mut_ptr(), p.len() as u64, timeout_ms(timeout)) };
        p.iter().filter(|p| p.revents != 0).map(|p| p.fd).collect()
    }

    #[cfg(not(unix))]
    fn wait(self, timeout: Option<Duration>) -> Vec<i32> {
        std::thread::sleep(timeout.unwrap_or(Duration::from_millis(1)));
        self.0.into_iter().map(|(fd, _)| fd).collect()
    }
}

/// Records the running task's readiness: when `ready` is 0 the task is
/// about to suspend on `wait`. Outside the executor every call is ready.
fn note(ready: bool, wait: Wait) -> u8 {
    EXEC.with(|e| {
        let mut e = e.borrow_mut();
        let Some(cur) = e.cur_mut().as_mut() else {
            return 1;
        };
        if ready {
            cur.wait = Wait::None;
            e.progress += 1;
            1
        } else {
            cur.wait = wait;
            0
        }
    })
}

/// `__ready` of a join or a channel operation, whose readiness codegen
/// worked out: only another task can change it.
#[no_mangle]
pub extern "C" fn karac_mir_ready(ready: u8) -> u8 {
    note(ready != 0, Wait::Task)
}

/// `__ready("sleep_ms", ms)`: whether the running task's pause is over,
/// its deadline set when first asked. Once it is, the `sleep_ms` that
/// follows returns at once.
#[no_mangle]
pub extern "C" fn karac_mir_ready_sleep(ms: i64) -> u8 {
    let at = EXEC.with(|e| {
        let mut e = e.borrow_mut();
        let cur = e.cur_mut().as_mut()?;
        let now = Instant::now();
        let at = *cur
            .deadline
            .get_or_insert_with(|| now + Duration::from_millis(ms.max(0) as u64));
        if now >= at {
            cur.deadline = None;
            cur.slept = true;
        }
        Some(at)
    });
    match at {
        None => 1,
        Some(at) => note(Instant::now() >= at, Wait::Until(at)),
    }
}

/// Whether the running task waits by a deadline (a pool's `acquire`, as
/// [`ready_by`] keeps it), or `None` outside the executor.
pub(crate) fn deadline_set() -> Option<bool> {
    EXEC.with(|e| e.borrow().cur().as_ref().map(|cur| cur.deadline.is_some()))
}

/// `__ready` of a call that waits up to `ns` nanoseconds for a condition
/// only the program's other tasks change (a pool's `acquire`,
/// B-2026-10-21-44): `ready` now, or the running task waits by the
/// deadline its first ask sets. Answers the readiness and whether the
/// deadline passed first; the deadline clears once either happens.
pub(crate) fn ready_by(ready: bool, ns: i64) -> (u8, bool) {
    let at = EXEC.with(|e| {
        let mut e = e.borrow_mut();
        let cur = e.cur_mut().as_mut()?;
        if ready {
            cur.deadline = None;
            return None;
        }
        let now = Instant::now();
        let at = *cur
            .deadline
            .get_or_insert_with(|| now + Duration::from_nanos(ns.max(0) as u64));
        if now >= at {
            cur.deadline = None;
        }
        Some(at)
    });
    match at {
        None => (note(true, Wait::None), false),
        Some(at) if Instant::now() >= at => (note(true, Wait::None), true),
        Some(at) => (note(false, Wait::TaskBy(at)), false),
    }
}

/// Moves on whenever a channel changes (a value sent or received, an end
/// dropped): what a `select` with no arm ready waits on.
static CHAN_EPOCH: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// A channel changed: a `select` waiting on the epoch may go on, and the
/// task that changed it made progress, so the executor polls the others
/// again before it concludes they all wait.
pub(crate) fn chan_changed() {
    CHAN_EPOCH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    EXEC.with(|e| {
        if let Ok(mut e) = e.try_borrow_mut() {
            e.progressed();
        }
    });
}

/// `__select_epoch()`: the count of channel changes so far.
#[no_mangle]
pub extern "C" fn karac_mir_select_epoch() -> i64 {
    CHAN_EPOCH.load(std::sync::atomic::Ordering::Relaxed)
}

/// Whether the monotonic clock has reached `until` (-1: never).
fn select_due(until: i64) -> bool {
    until >= 0 && crate::karac_runtime_clock_monotonic_ns() >= until
}

/// `__ready("__select_park", epoch, until)` (core-semantics §11.11): a
/// `select` with no arm ready goes on once a channel has changed since
/// `epoch`, or the monotonic clock reads `until` (-1: no deadline).
#[no_mangle]
pub extern "C" fn karac_mir_ready_select(epoch: i64, until: i64) -> u8 {
    let ready = karac_mir_select_epoch() != epoch || select_due(until);
    let wait = if until >= 0 {
        let left = until - crate::karac_runtime_clock_monotonic_ns();
        Wait::TaskBy(Instant::now() + Duration::from_nanos(left.max(0) as u64))
    } else {
        Wait::Task
    };
    note(ready, wait)
}

/// `__select_park(epoch, until)`. Under the executor its `__ready` waited.
/// Outside one no other task can change a channel, so the wait is for the
/// deadline, and with none the program can never go on.
#[no_mangle]
pub extern "C" fn karac_mir_select_park(epoch: i64, until: i64) {
    if EXEC.with(|e| e.borrow().cur().is_some()) || karac_mir_select_epoch() != epoch {
        return;
    }
    if until < 0 {
        crate::mir::deadlock(
            "a `select` waits on channels that no running task sends to or receives from",
        );
    }
    let left = until - crate::karac_runtime_clock_monotonic_ns();
    if left > 0 {
        std::thread::sleep(Duration::from_nanos(left as u64));
    }
}

/// Whether the running task's `__ready` already waited its pause out; the
/// flag is cleared, so `sleep_ms` asks it once.
pub(crate) fn take_slept() -> bool {
    EXEC.with(|e| {
        let mut e = e.borrow_mut();
        match e.cur_mut().as_mut() {
            Some(cur) => std::mem::take(&mut cur.slept),
            None => false,
        }
    })
}

/// Arms the call a ready answer guards (`mir_sock.rs`): it may then answer
/// `IO_NOT_READY`, which the readiness loop retries on.
fn arm() {
    #[cfg(all(feature = "net", unix))]
    crate::mir_sock::karac_mir_io_arm();
}

/// `__ready` of a socket call: whether `fd` is readable now (`write` 0)
/// or writable, asked without blocking. A read on a socket with a read
/// timeout also ends at the deadline its first ask sets (§11.10), and the
/// read that follows then fails `TimedOut` ([`take_timed_out`]).
/// A call with no descriptor to wait on (a child's wait where the platform
/// gives none) is ready at once, unarmed, so it blocks.
#[no_mangle]
pub extern "C" fn karac_mir_ready_fd(fd: i64, write: u8) -> u8 {
    let events: i16 = if write != 0 { 4 } else { 1 }; // POLLOUT, POLLIN
    if EXEC.with(|e| e.borrow().cur().is_none()) {
        return 1;
    }
    if fd < 0 {
        return note(true, Wait::None);
    }
    #[cfg(unix)]
    if write == 0 {
        if let Some(t) = read_timeout(fd as i32) {
            return ready_read_by(fd as i32, t);
        }
    }
    #[cfg(unix)]
    let ready = {
        let mut p = PollFd {
            fd: fd as i32,
            events,
            revents: 0,
        };
        // SAFETY: one initialized entry.
        let n = unsafe { poll(&mut p, 1, 0) };
        n != 0
    };
    #[cfg(not(unix))]
    let ready = true;
    if ready {
        arm();
    }
    note(ready, fd_wait(fd as i32, events, None))
}

/// `__ready("TcpStream.connect", ..)`: a fresh connect is ready at once,
/// armed; one the running task has under way ([`set_inflight`]) once its
/// socket is writable.
#[no_mangle]
pub extern "C" fn karac_mir_ready_connect() -> u8 {
    match EXEC.with(|e| e.borrow().cur().map(|c| c.inflight)) {
        None => 1,
        Some(Some(fd)) => karac_mir_ready_fd(i64::from(fd), 1),
        Some(None) => {
            arm();
            note(true, Wait::None)
        }
    }
}

program_state! {
    /// The mutexes tasks hold, by their cells' addresses (B-2026-10-19-23),
    /// each with the task holding it: kept here so a `Mutex[T]` is its
    /// value alone.
    static HELD: Vec<(usize, u64)> = Vec::new();
}

/// The running task's id, or 0 outside the executor.
fn task_id() -> u64 {
    current_task().unwrap_or(0)
}

/// `__ready("Mutex[T].acquire", m)`: whether no task holds the mutex at
/// `cell`; only another task's release can change that. Under the pool the
/// `__ready` that finds it free takes it for the running task, so another
/// worker's task cannot take it before this one's `acquire` runs.
#[no_mangle]
pub extern "C" fn karac_mir_mutex_ready(cell: *mut u8) -> u8 {
    let cell = cell as usize;
    let me = if pooled() { task_id() } else { 0 };
    let free = HELD.with(|h| {
        let free = h.iter().all(|&(c, _)| c != cell);
        if free && pooled() {
            h.push((cell, me));
        }
        free
    });
    note(free, Wait::Task)
}

/// `Mutex[T].acquire`, run once its `__ready` saw the mutex free: marks it
/// held (under the pool, its `__ready` did).
#[no_mangle]
pub extern "C" fn karac_mir_mutex_acquire(cell: *mut u8) {
    let cell = cell as usize;
    let me = if pooled() { task_id() } else { 0 };
    let taken = HELD.with(|h| match h.iter().find(|&&(c, _)| c == cell) {
        Some(&(_, t)) => !(pooled() && t == me),
        None => {
            h.push((cell, me));
            false
        }
    });
    if taken {
        crate::mir::deadlock("lock of a mutex that is already held");
    }
}

/// `Mutex[T].release`: frees the mutex at `cell` for the next task waiting
/// on it, in the order they parked.
#[no_mangle]
pub extern "C" fn karac_mir_mutex_release(cell: *mut u8) {
    let held = HELD.with(|h| {
        let at = h.iter().position(|&(c, _)| c == cell as usize);
        at.map(|i| h.swap_remove(i))
    });
    if held.is_none() {
        crate::mir::karac_mir_flush();
        eprintln!("error: unlock of a mutex that is not held");
        std::process::exit(101);
    }
    bump();
}

/// The socket `fd`'s read timeout (`SO_RCVTIMEO`), if it has one.
#[cfg(unix)]
fn read_timeout(fd: i32) -> Option<Duration> {
    use std::os::fd::FromRawFd;
    // SAFETY: `fd` is an open socket the program owns; the stream made
    // over it is never dropped, so the descriptor stays open. `SO_RCVTIMEO`
    // reads the same on a UDP socket.
    let s = std::mem::ManuallyDrop::new(unsafe { std::net::TcpStream::from_raw_fd(fd) });
    s.read_timeout().ok().flatten()
}

/// [`karac_mir_ready_fd`] of a read on `fd`, whose read timeout is `t`:
/// readable now, or the running task waits until it is or the deadline
/// passes. A passed deadline leaves `fd` for [`take_timed_out`].
#[cfg(unix)]
fn ready_read_by(fd: i32, t: Duration) -> u8 {
    let mut p = PollFd {
        fd,
        events: 1,
        revents: 0,
    };
    // SAFETY: one initialized entry.
    let readable = unsafe { poll(&mut p, 1, 0) } != 0;
    read_by(fd, readable, 1, t)
}

/// The `__ready` of a call on `fd` whose readiness its own layer worked
/// out (`mir_tls.rs`): `ready`, or the running task waits for `fd` to
/// turn writable (`out`) or readable; with `by`, a read timeout, until
/// that deadline at most, as [`ready_read_by`]'s.
#[cfg(all(feature = "tls", unix))]
pub(crate) fn ready_io(fd: i32, ready: bool, out: bool, by: Option<Duration>) -> u8 {
    if EXEC.with(|e| e.borrow().cur().is_none()) {
        return 1;
    }
    let events: i16 = if out { 4 } else { 1 }; // POLLOUT, POLLIN
    match by {
        Some(t) => read_by(fd, ready, events, t),
        None => note(ready, fd_wait(fd, events, None)),
    }
}

/// [`ready_read_by`] with the readiness given and the wait on `events`.
#[cfg(unix)]
fn read_by(fd: i32, readable: bool, events: i16, t: Duration) -> u8 {
    let now = Instant::now();
    let at = EXEC.with(|e| {
        let mut e = e.borrow_mut();
        let cur = e.cur_mut().as_mut().expect("asked inside the executor");
        let at = *cur.deadline.get_or_insert(now + t);
        if readable || now >= at {
            cur.deadline = None;
        }
        at
    });
    if readable {
        note(true, Wait::None)
    } else if now >= at {
        TIMED_OUT.with(|c| c.set(fd));
        note(true, Wait::None)
    } else {
        note(false, fd_wait(fd, events, Some(at)))
    }
}

#[cfg(unix)]
thread_local! {
    /// The socket whose suspended read reached its deadline with nothing
    /// to read, or -1.
    static TIMED_OUT: std::cell::Cell<i32> = const { std::cell::Cell::new(-1) };
}

/// Whether the read about to run on `fd` follows a wait that reached the
/// read timeout's deadline, and so fails `TimedOut` (`mir_sock::drive`).
#[cfg(all(feature = "net", unix))]
pub(crate) fn take_timed_out(fd: i32) -> bool {
    TIMED_OUT.with(|c| {
        let hit = c.get() == fd;
        if hit {
            c.set(-1);
        }
        hit
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pool's own core count agrees with std's on the host it runs on
    /// (B-2026-10-28-8), and reads a cgroup file's number back.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_cores_counts_as_available_parallelism() {
        assert_eq!(
            linux_cores(),
            std::thread::available_parallelism().map_or(1, |n| n.get())
        );
        assert_eq!(whole(Some(&b"150000"[..])), Some(150000));
        assert_eq!(whole(Some(&b"-1"[..])), None);
        assert_eq!(whole(Some(&b"max"[..])), None);
        assert_eq!(
            cgroup_file(c"/nonexistent/cpu.max", &mut [0u8; 64]),
            &b""[..]
        );
    }

    static mut LOG: Vec<(usize, u32)> = Vec::new();

    /// Task `a` (its id) yields `b` times, then returns.
    unsafe extern "C" fn yielder(a: *mut u8, b: *mut u8) -> u8 {
        let id = a as usize;
        // SAFETY: `b` points at this task's own counter.
        let n = unsafe { &mut *(b as *mut u32) };
        // SAFETY: single-threaded test.
        unsafe { (*std::ptr::addr_of_mut!(LOG)).push((id, *n)) };
        if *n == 0 {
            return 1;
        }
        *n -= 1;
        0
    }

    unsafe extern "C" fn root(_a: *mut u8, b: *mut u8) -> u8 {
        // SAFETY: `b` holds the two children's counters.
        let c = b as *mut u32;
        unsafe {
            karac_mir_task_spawn(
                std::ptr::null_mut(),
                yielder,
                std::ptr::without_provenance_mut(1),
                c as *mut u8,
            );
            karac_mir_task_spawn(
                std::ptr::null_mut(),
                yielder,
                std::ptr::without_provenance_mut(2),
                c.add(1) as *mut u8,
            );
        }
        1
    }

    #[test]
    fn tasks_run_in_fifo_order() {
        let mut counters = [1u32, 2];
        // SAFETY: the counters outlive the run.
        unsafe { karac_mir_task_run(root, std::ptr::null_mut(), counters.as_mut_ptr() as *mut u8) };
        // SAFETY: the run is over.
        let log = unsafe { (*std::ptr::addr_of!(LOG)).clone() };
        assert_eq!(log, vec![(1, 1), (2, 2), (1, 0), (2, 1), (2, 0)]);
    }

    #[test]
    fn outside_the_executor_every_call_is_ready() {
        assert_eq!(karac_mir_ready(0), 1);
        assert_eq!(karac_mir_ready_sleep(10_000), 1);
        assert!(!take_slept());
    }
}
