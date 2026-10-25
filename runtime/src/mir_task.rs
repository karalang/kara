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

use std::cell::{Cell, RefCell};
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, VecDeque};
use std::time::{Duration, Instant};

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
    /// The socket of a `connect` it has under way ([`set_inflight`]).
    inflight: Option<i32>,
    /// Its active span stack, swapped into the thread's while it runs: span
    /// context is per task, inherited from the spawner.
    spans: Vec<i64>,
}

/// The running task's own state, while it runs.
#[derive(Clone, Copy)]
struct Cur {
    deadline: Option<Instant>,
    slept: bool,
    wait: Wait,
    inflight: Option<i32>,
}

#[derive(Default)]
struct Exec {
    /// The tasks that can run, and those waiting on another task.
    queue: VecDeque<Task>,
    /// The tasks off the queue, sleeping or parked, by the key they were
    /// put off with; a key is never reused, so a timer or a descriptor
    /// that names a task already woken names no task.
    off: HashMap<u64, Task>,
    keys: u64,
    /// Deadlines, each with the key of its task: the earliest first, and
    /// in the order the tasks were put off at a tie.
    timers: BinaryHeap<Reverse<(Instant, u64)>>,
    /// The keys of the tasks parked on each descriptor, in the order they
    /// parked.
    parked: HashMap<i32, Vec<u64>>,
    reactor: Reactor,
    cur: Option<Cur>,
    /// Bumped whenever something may have made a waiting task ready: a
    /// readiness check that passed, a spawn, or a task that returned.
    progress: u64,
}

impl Exec {
    /// Queues `t`, runnable.
    fn wake(&mut self, mut t: Task) {
        t.wait = Wait::None;
        self.queue.push_back(t);
    }

    /// Takes `t` off the queue, waking by a timer or a descriptor; answers
    /// its key.
    fn put_off(&mut self, t: Task) -> u64 {
        self.keys += 1;
        self.off.insert(self.keys, t);
        self.keys
    }

    /// Queues the task put off with `key`, if it is still off, and takes
    /// it off its descriptor's list; answers whether it was.
    fn wake_key(&mut self, key: u64) -> bool {
        let Some(t) = self.off.remove(&key) else {
            return false;
        };
        if let Wait::Fd(fd, _) | Wait::FdBy(fd, _, _) = t.wait {
            if let Some(keys) = self.parked.get_mut(&fd) {
                keys.retain(|&k| k != key);
                if keys.is_empty() {
                    self.parked.remove(&fd);
                }
            }
        }
        self.wake(t);
        true
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

thread_local! {
    static EXEC: RefCell<Exec> = RefCell::new(Exec::default());
    /// Set by a native that answered `IO_NOT_READY` ([`set_again`]) and
    /// taken by the `__io_again` after it.
    static AGAIN: Cell<bool> = const { Cell::new(false) };
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
            inflight: None,
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
    // Tasks that waited on another task in a row with nothing happening in
    // between.
    let mut idle = 0usize;
    // Tasks run since the reactor was last asked.
    let mut since = 0usize;
    loop {
        // Once a round, the parked tasks whose descriptor is ready, and the
        // sleepers whose deadline passed, join the queue.
        let ask = EXEC.with(|e| {
            let e = e.borrow();
            !e.parked.is_empty() && since >= e.queue.len()
        });
        let mut woken = 0;
        if ask {
            woken += wait_reactor(Some(Duration::ZERO));
            since = 0;
        }
        woken += EXEC.with(|e| e.borrow_mut().wake_timers());
        if woken > 0 {
            idle = 0;
        }
        let Some(mut t) = EXEC.with(|e| {
            let mut e = e.borrow_mut();
            let t = e.queue.pop_front();
            if let Some(t) = &t {
                e.cur = Some(Cur {
                    deadline: t.deadline,
                    slept: t.slept,
                    wait: Wait::None,
                    inflight: t.inflight,
                });
            }
            t
        }) else {
            // Nothing can run: every task left sleeps or waits on a
            // descriptor, or none is left.
            if !idle_wait(None) {
                return;
            }
            idle = 0;
            continue;
        };
        since += 1;
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
        if let Some(cur) = cur {
            t.deadline = cur.deadline;
            t.slept = cur.slept;
            t.wait = cur.wait;
            t.inflight = cur.inflight;
        }
        if after != before || t.wait == Wait::None {
            idle = 0;
        }
        match t.wait {
            Wait::Until(at) => {
                EXEC.with(|e| {
                    let mut e = e.borrow_mut();
                    let key = e.put_off(t);
                    e.timers.push(Reverse((at, key)));
                });
                continue;
            }
            Wait::Fd(fd, events) => {
                park(t, fd, events);
                continue;
            }
            // Whichever comes first, the descriptor or the deadline, wakes
            // the task.
            Wait::FdBy(fd, events, at) => {
                if let Some(key) = park(t, fd, events) {
                    EXEC.with(|e| e.borrow_mut().timers.push(Reverse((at, key))));
                }
                continue;
            }
            Wait::Task | Wait::TaskBy(_) if after == before => idle += 1,
            _ => {}
        }
        let stuck = EXEC.with(|e| {
            let mut e = e.borrow_mut();
            e.queue.push_back(t);
            idle >= e.queue.len()
        });
        if stuck {
            block();
            idle = 0;
        }
    }
}

/// Every task in the queue waits on another, with nothing having happened
/// since each last ran: waits for a sleeper, a descriptor or a timed
/// wait's deadline, or reports the deadlock when there is none.
fn block() {
    let (by, runnable) = EXEC.with(|e| {
        let e = e.borrow();
        let by = e
            .queue
            .iter()
            .filter_map(|t| match t.wait {
                Wait::TaskBy(at) => Some(at),
                _ => None,
            })
            .min();
        (by, e.queue.iter().any(|t| t.wait == Wait::None))
    });
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
    let (next, parked) = EXEC.with(|e| {
        let mut e = e.borrow_mut();
        (e.next_deadline(), !e.parked.is_empty())
    });
    let until = match (next, by) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    if until.is_none() && !parked {
        return false;
    }
    let timeout = until.map(|t| t.saturating_duration_since(Instant::now()));
    if parked {
        wait_reactor(timeout);
    } else if let Some(d) = timeout {
        std::thread::sleep(d);
    }
    EXEC.with(|e| e.borrow_mut().wake_timers());
    true
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
    let ready = EXEC.with(|e| e.borrow_mut().reactor.wait(timeout));
    EXEC.with(|e| {
        let mut e = e.borrow_mut();
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
        if let Some(cur) = e.borrow_mut().cur.as_mut() {
            cur.inflight = Some(fd);
        }
    });
}

/// The socket of the running task's connect under way, taken.
#[cfg_attr(not(all(feature = "net", unix)), allow(dead_code))]
pub(crate) fn take_inflight() -> Option<i32> {
    EXEC.with(|e| e.borrow_mut().cur.as_mut().and_then(|c| c.inflight.take()))
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
    known: std::collections::HashSet<i32>,
}

#[cfg(all(feature = "net", target_os = "linux"))]
impl Reactor {
    fn watch(&mut self, fd: i32, events: i16) -> bool {
        let ep = match self.ep {
            Some(ep) => ep,
            None => {
                // SAFETY: plain syscall.
                let mut ep = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
                if ep < 0 {
                    return false;
                }
                // Moved above the numbers a program's own descriptors take,
                // so they are numbered as on the interpreter, which has no
                // reactor descriptor.
                // SAFETY: plain syscalls on the descriptor just made.
                let high = unsafe { libc::fcntl(ep, libc::F_DUPFD_CLOEXEC, 100) };
                if high >= 0 {
                    // SAFETY: as above.
                    unsafe { libc::close(ep) };
                    ep = high;
                }
                self.ep = Some(ep);
                ep
            }
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

    fn wait(&mut self, timeout: Option<Duration>) -> Vec<i32> {
        let Some(ep) = self.ep else {
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
    fds: HashMap<i32, i16>,
}

#[cfg(not(all(feature = "net", target_os = "linux")))]
impl Reactor {
    fn watch(&mut self, fd: i32, events: i16) -> bool {
        self.fds.insert(fd, events);
        true
    }

    fn forget(&mut self, fd: i32) {
        self.fds.remove(&fd);
    }

    #[cfg(unix)]
    fn wait(&mut self, timeout: Option<Duration>) -> Vec<i32> {
        let mut p: Vec<PollFd> = self
            .fds
            .iter()
            .map(|(&fd, &events)| PollFd {
                fd,
                events,
                revents: 0,
            })
            .collect();
        // SAFETY: `p` holds `p.len()` initialized entries.
        unsafe { poll(p.as_mut_ptr(), p.len() as u64, timeout_ms(timeout)) };
        let ready: Vec<i32> = p.iter().filter(|p| p.revents != 0).map(|p| p.fd).collect();
        for fd in &ready {
            self.fds.remove(fd);
        }
        ready
    }

    #[cfg(not(unix))]
    fn wait(&mut self, timeout: Option<Duration>) -> Vec<i32> {
        std::thread::sleep(timeout.unwrap_or(Duration::from_millis(1)));
        self.fds.drain().map(|(fd, _)| fd).collect()
    }
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
        let cur = e.cur.as_mut()?;
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
    EXEC.with(|e| e.borrow().cur.as_ref().map(|cur| cur.deadline.is_some()))
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
    if EXEC.with(|e| e.borrow().cur.is_none()) {
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
    note(ready, Wait::Fd(fd as i32, events))
}

/// `__ready("TcpStream.connect", ..)`: a fresh connect is ready at once,
/// armed; one the running task has under way ([`set_inflight`]) once its
/// socket is writable.
#[no_mangle]
pub extern "C" fn karac_mir_ready_connect() -> u8 {
    match EXEC.with(|e| e.borrow().cur.map(|c| c.inflight)) {
        None => 1,
        Some(Some(fd)) => karac_mir_ready_fd(i64::from(fd), 1),
        Some(None) => {
            arm();
            note(true, Wait::None)
        }
    }
}

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
    if EXEC.with(|e| e.borrow().cur.is_none()) {
        return 1;
    }
    let events: i16 = if out { 4 } else { 1 }; // POLLOUT, POLLIN
    match by {
        Some(t) => read_by(fd, ready, events, t),
        None => note(ready, Wait::Fd(fd, events)),
    }
}

/// [`ready_read_by`] with the readiness given and the wait on `events`.
#[cfg(unix)]
fn read_by(fd: i32, readable: bool, events: i16, t: Duration) -> u8 {
    let now = Instant::now();
    let at = EXEC.with(|e| {
        let mut e = e.borrow_mut();
        let cur = e.cur.as_mut().expect("asked inside the executor");
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
        note(false, Wait::FdBy(fd, events, at))
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
