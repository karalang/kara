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
//! - **The readiness loop** codegen emits around a suspending fd call is
//!   `loop { while !__ready(op, fd) { yield }; r = native(..); if r !=
//!   IO_NOT_READY { break }; karac_mir_io_stale(fd, write) }`. `__ready`
//!   reads the reactor's cached readiness. An fd's first query registers it
//!   (epoll edge-triggered, kqueue `EV_CLEAR`) and answers ready, so the
//!   first attempt is optimistic. `io_stale` clears the cached bit, and the
//!   next event sets it again.
//! - **Connect is two-phase.** On `EINPROGRESS`, `connect` returns
//!   `IO_IN_PROGRESS` (101) with the fd in an out parameter. The loop waits
//!   for that fd to be writable, then calls `connect_finish(fd)`, which
//!   answers `IO_NOT_READY` while the connect is still under way and
//!   otherwise reads `SO_ERROR` itself.
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
//! A task waiting on an fd leaves the run queue and parks on the reactor,
//! and deadlines go in a heap. A round of the executor then costs O(ready)
//! rather than O(tasks): the executor sleeps on one `epoll_wait` / `kevent`
//! until the nearest deadline, and wakes only the tasks the events name.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// A task's poll function: resumes the task once, and answers 1 when it
/// returned.
pub type TaskPoll = unsafe extern "C" fn(*mut u8, *mut u8) -> u8;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Wait {
    /// Runnable: not suspended, or suspended on a plain yield.
    None,
    /// A `sleep_ms` that has not reached its deadline.
    Until(Instant),
    /// A socket to become readable (`events` is `POLLIN`) or writable.
    Fd(i32, i16),
    /// A join or a channel, which only another task can make ready.
    Task,
}

/// A `TaskGroup` (B-2026-10-15-30): how many of its tasks may run at once
/// and how many do. Its handle is a pointer to this, freed once the group
/// has dropped and its last task has returned.
struct Group {
    limit: i64,
    live: i64,
    dropped: bool,
}

struct Task {
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
    /// Its active span stack, swapped into the thread's while it runs: span
    /// context is per task, inherited from the spawner.
    spans: Vec<i64>,
}

#[derive(Default)]
struct Exec {
    queue: VecDeque<Task>,
    /// The running task's deadline, `slept` flag and wait, while it runs.
    cur: Option<(Option<Instant>, bool, Wait)>,
    /// Bumped whenever something may have made a waiting task ready: a
    /// readiness check that passed, a spawn, or a task that returned.
    progress: u64,
}

thread_local! {
    static EXEC: RefCell<Exec> = RefCell::new(Exec::default());
}

fn bump() {
    EXEC.with(|e| e.borrow_mut().progress += 1);
}

/// `TaskGroup.new(limit: n)`: a group that lets at most `limit` of its
/// tasks run at once.
#[no_mangle]
pub extern "C" fn karac_mir_group_new(limit: i64) -> *mut u8 {
    Box::into_raw(Box::new(Group {
        limit,
        live: 0,
        dropped: false,
    })) as *mut u8
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
    // SAFETY: the caller's contract.
    unsafe {
        if (*g).live == 0 {
            drop(Box::from_raw(g));
        } else {
            (*g).dropped = true;
        }
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
    // SAFETY: the caller's contract.
    let g = unsafe { &*(g as *const Group) };
    let ready = if end != 0 {
        g.live == 0
    } else {
        g.live < g.limit
    };
    note(ready, Wait::Task)
}

/// A task of `group` returned.
fn task_done(group: *mut Group) {
    if group.is_null() {
        return;
    }
    // SAFETY: a group outlives its tasks (`karac_mir_group_drop`).
    unsafe {
        (*group).live -= 1;
        if (*group).dropped && (*group).live == 0 {
            drop(Box::from_raw(group));
        }
    }
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
    if !group.is_null() {
        // SAFETY: the caller's contract.
        unsafe { (*group).live += 1 };
    }
    EXEC.with(|e| {
        let mut e = e.borrow_mut();
        e.queue.push_back(Task {
            group,
            poll,
            a,
            b,
            deadline: None,
            slept: false,
            wait: Wait::None,
            spans: crate::mir::current_spans(),
        });
        e.progress += 1;
    });
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
    // Tasks suspended in a row with nothing happening in between.
    let mut idle = 0usize;
    loop {
        let Some(mut t) = EXEC.with(|e| {
            let mut e = e.borrow_mut();
            let t = e.queue.pop_front();
            if let Some(t) = &t {
                e.cur = Some((t.deadline, t.slept, Wait::None));
            }
            t
        }) else {
            return;
        };
        let before = EXEC.with(|e| e.borrow().progress);
        let outer = crate::mir::swap_spans(std::mem::take(&mut t.spans));
        // SAFETY: the task has not answered 1 yet.
        let done = unsafe { (t.poll)(t.a, t.b) } != 0;
        t.spans = crate::mir::swap_spans(outer);
        let (after, cur) = EXEC.with(|e| {
            let mut e = e.borrow_mut();
            (e.progress, e.cur.take())
        });
        if done {
            task_done(t.group);
            bump();
            idle = 0;
            continue;
        }
        if let Some((deadline, slept, wait)) = cur {
            t.deadline = deadline;
            t.slept = slept;
            t.wait = wait;
        }
        if after != before || t.wait == Wait::None {
            idle = 0;
        } else {
            idle += 1;
        }
        let waiting = EXEC.with(|e| {
            let mut e = e.borrow_mut();
            e.queue.push_back(t);
            (idle >= e.queue.len()).then(|| e.queue.iter().map(|t| t.wait).collect::<Vec<_>>())
        });
        if let Some(waits) = waiting {
            block(&waits);
            idle = 0;
        }
    }
}

/// Every task waits: sleeps until the nearest deadline or a socket.
fn block(waits: &[Wait]) {
    let until = waits
        .iter()
        .filter_map(|w| match w {
            Wait::Until(t) => Some(*t),
            _ => None,
        })
        .min();
    let fds: Vec<(i32, i16)> = waits
        .iter()
        .filter_map(|w| match w {
            Wait::Fd(fd, ev) => Some((*fd, *ev)),
            _ => None,
        })
        .collect();
    if until.is_none() && fds.is_empty() {
        crate::mir::deadlock(
            "every task waits on a channel that no running task sends to or receives from",
        );
    }
    let timeout = until.map(|t| t.saturating_duration_since(Instant::now()));
    wait_fds(&fds, timeout);
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

/// Waits until one of `fds` is ready or `timeout` passes (no timeout:
/// until a socket is).
#[cfg(unix)]
fn wait_fds(fds: &[(i32, i16)], timeout: Option<Duration>) {
    let mut p: Vec<PollFd> = fds
        .iter()
        .map(|&(fd, events)| PollFd {
            fd,
            events,
            revents: 0,
        })
        .collect();
    let ms = match timeout {
        // Rounded up, so the deadline has passed when it returns.
        Some(d) => d.as_micros().div_ceil(1000).min(i32::MAX as u128) as i32,
        None => -1,
    };
    // SAFETY: `p` holds `p.len()` initialized entries.
    unsafe { poll(p.as_mut_ptr(), p.len() as u64, ms) };
}

#[cfg(not(unix))]
fn wait_fds(_fds: &[(i32, i16)], timeout: Option<Duration>) {
    std::thread::sleep(timeout.unwrap_or(Duration::from_millis(1)));
}

/// Records the running task's readiness: when `ready` is 0 the task is
/// about to suspend on `wait`. Outside the executor every call is ready.
fn note(ready: bool, wait: Wait) -> u8 {
    EXEC.with(|e| {
        let mut e = e.borrow_mut();
        let Some(cur) = e.cur.as_mut() else {
            return 1;
        };
        if ready {
            cur.2 = Wait::None;
            e.progress += 1;
            1
        } else {
            cur.2 = wait;
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
        let cur = e.cur.as_mut()?;
        let now = Instant::now();
        let at = *cur
            .0
            .get_or_insert_with(|| now + Duration::from_millis(ms.max(0) as u64));
        if now >= at {
            cur.0 = None;
            cur.1 = true;
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
    EXEC.with(|e| e.borrow().cur.as_ref().map(|cur| cur.0.is_some()))
}

/// `__ready` of a call that waits up to `ns` nanoseconds for a condition
/// only the program's other tasks change (a pool's `acquire`,
/// B-2026-10-21-44): `ready` now, or the running task waits by the
/// deadline its first ask sets. Answers the readiness and whether the
/// deadline passed first; the deadline clears once either happens.
pub(crate) fn ready_by(ready: bool, ns: i64) -> (u8, bool) {
    let at = EXEC.with(|e| {
        let mut e = e.borrow_mut();
        let cur = e.cur.as_mut()?;
        if ready {
            cur.0 = None;
            return None;
        }
        let now = Instant::now();
        let at = *cur
            .0
            .get_or_insert_with(|| now + Duration::from_nanos(ns.max(0) as u64));
        if now >= at {
            cur.0 = None;
        }
        Some(at)
    });
    match at {
        None => (note(true, Wait::None), false),
        Some(at) if Instant::now() >= at => (note(true, Wait::None), true),
        Some(at) => (note(false, Wait::Until(at)), false),
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
            e.progress += 1;
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
        Wait::Until(Instant::now() + Duration::from_nanos(left.max(0) as u64))
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
    if EXEC.with(|e| e.borrow().cur.is_some()) || karac_mir_select_epoch() != epoch {
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
        match e.cur.as_mut() {
            Some(cur) => std::mem::take(&mut cur.1),
            None => false,
        }
    })
}

/// `__ready` of a socket call: whether `fd` is readable now (`write` 0)
/// or writable, asked without blocking. A socket with a read timeout is
/// ready at once: its blocking read keeps the timeout itself, where
/// waiting on readiness alone would never time out.
#[no_mangle]
pub extern "C" fn karac_mir_ready_fd(fd: i64, write: u8) -> u8 {
    let events: i16 = if write != 0 { 4 } else { 1 }; // POLLOUT, POLLIN
    if EXEC.with(|e| e.borrow().cur.is_none()) {
        return 1;
    }
    #[cfg(unix)]
    if write == 0 && read_timeout_set(fd as i32) {
        return 1;
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
    // A ready answer arms the call it guards (`mir_sock.rs`), which may
    // then answer `IO_NOT_READY`; only once the readiness loop retries on
    // that status.
    #[cfg(all(feature = "net", unix))]
    if ARM_ON_READY && ready {
        crate::mir_sock::karac_mir_io_arm();
    }
    note(ready, Wait::Fd(fd as i32, events))
}

/// Whether [`karac_mir_ready_fd`] arms the call after a ready answer: off
/// until codegen's readiness loop handles `IO_NOT_READY`, since a call
/// that still found nothing (a connection reset between the readiness
/// check and its `accept`) would otherwise hand the status to Kāra code.
#[cfg(all(feature = "net", unix))]
const ARM_ON_READY: bool = false;

thread_local! {
    /// The mutexes a task holds, by their cells' addresses (B-2026-10-19-23):
    /// kept here so a `Mutex[T]` is its value alone.
    static HELD: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
}

/// `__ready("Mutex[T].acquire", m)`: whether no task holds the mutex at
/// `cell`; only another task's release can change that.
#[no_mangle]
pub extern "C" fn karac_mir_mutex_ready(cell: *mut u8) -> u8 {
    let free = HELD.with(|h| !h.borrow().contains(&(cell as usize)));
    note(free, Wait::Task)
}

/// `Mutex[T].acquire`, run once its `__ready` saw the mutex free: marks it
/// held.
#[no_mangle]
pub extern "C" fn karac_mir_mutex_acquire(cell: *mut u8) {
    let taken = HELD.with(|h| {
        let mut h = h.borrow_mut();
        let held = h.contains(&(cell as usize));
        if !held {
            h.push(cell as usize);
        }
        held
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
        let mut h = h.borrow_mut();
        let at = h.iter().position(|&c| c == cell as usize);
        at.map(|i| h.swap_remove(i))
    });
    if held.is_none() {
        crate::mir::karac_mir_flush();
        eprintln!("error: unlock of a mutex that is not held");
        std::process::exit(101);
    }
    bump();
}

/// Whether the socket `fd` has a read timeout (`SO_RCVTIMEO`).
#[cfg(unix)]
fn read_timeout_set(fd: i32) -> bool {
    use std::os::fd::FromRawFd;
    // SAFETY: `fd` is an open socket the program owns; the stream made
    // over it is never dropped, so the descriptor stays open.
    let s = std::mem::ManuallyDrop::new(unsafe { std::net::TcpStream::from_raw_fd(fd) });
    matches!(s.read_timeout(), Ok(Some(_)))
}

#[cfg(test)]
mod tests {
    use super::*;

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
