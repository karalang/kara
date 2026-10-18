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
        // SAFETY: the task has not answered 1 yet.
        let done = unsafe { (t.poll)(t.a, t.b) } != 0;
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
        crate::mir::karac_mir_flush();
        eprintln!(
            "error: deadlock: every task waits on a channel that no running task sends to or \
             receives from"
        );
        std::process::exit(101);
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
    note(ready, Wait::Fd(fd as i32, events))
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
