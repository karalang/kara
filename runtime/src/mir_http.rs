//! The MIR backend's HTTP client calls inside a task (docs/library/io.md
//! § HTTP client). `__ready` of `Client.get` / `post` or
//! `RequestBuilder.send` hands the request to a helper thread
//! (`src/work_pool.rs`, which the MIR interpreter shares by `#[path]`) and
//! parks the task on a socket the helper writes once the answer is in; the
//! call after it takes the answer instead of sending the request itself.
//! Outside a task the call sends it, as before.
//!
//! **Interim**, as `work_pool` says: the end state is the client on
//! non-blocking sockets driven by the executor's reactor.

#[path = "../../src/work_pool.rs"]
mod work_pool;

use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};

/// What a request came to: the response, or the client's message.
type Answer = Result<crate::HttpFetched, String>;

/// A task's request on a helper, and the end of the socket pair the helper
/// writes a byte to once it is done.
struct Inflight {
    task: TaskKey,
    ticket: work_pool::Ticket<Answer>,
    wake: Arc<UnixStream>,
}

/// A task, by the thread whose executor runs it and its id there.
type TaskKey = (std::thread::ThreadId, u64);

fn key(task: u64) -> TaskKey {
    (std::thread::current().id(), task)
}

static INFLIGHT: Mutex<Vec<Inflight>> = Mutex::new(Vec::new());

fn inflight() -> std::sync::MutexGuard<'static, Vec<Inflight>> {
    INFLIGHT.lock().unwrap_or_else(|e| e.into_inner())
}

/// Drops task `task`'s request, when it ends with one still in flight (a
/// cancel): the helper finishes it and its answer is dropped.
fn forget(task: u64) {
    let task = key(task);
    let mut v = inflight();
    v.retain(|e| e.task != task);
}

/// `__ready` of a client call with [`crate::karac_runtime_http_client_mir_send`]'s
/// `op` and arguments: under the executor, the request goes to a helper the
/// first time it is asked, and it is ready once the helper's answer is in.
/// A request that cannot be read (an unknown builder, a URL that is not
/// UTF-8) is ready at once, for the call to report.
///
/// # Safety
/// As `karac_runtime_http_client_mir_send`'s.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_ready_http(
    op: i32,
    a: *const u8,
    a_len: u64,
    b: *const u8,
    b_len: u64,
    handle: i64,
) -> u8 {
    let Some(task) = crate::mir_task::current_task().map(key) else {
        return crate::mir_task::karac_mir_ready(1);
    };
    thread_local! {
        static HOOKED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    // Each executor's own tasks end on its thread.
    if !HOOKED.with(|h| h.replace(true)) {
        crate::mir_task::on_task_end(forget);
    }
    let fd = {
        let v = inflight();
        match v.iter().find(|e| e.task == task) {
            Some(e) if e.ticket.done() => return crate::mir_task::karac_mir_ready(1),
            Some(e) => Some(e.wake.as_raw_fd()),
            None => None,
        }
    };
    let fd = match fd {
        Some(fd) => fd,
        None => {
            // SAFETY: the caller's contract.
            let job = match unsafe {
                crate::http_job(op, a, a_len as usize, b, b_len as usize, handle)
            } {
                Ok(job) => job,
                Err(_) => return crate::mir_task::karac_mir_ready(1),
            };
            let Ok((mine, mut theirs)) = UnixStream::pair() else {
                return crate::mir_task::karac_mir_ready(1);
            };
            let _ = mine.set_nonblocking(true);
            // The helper keeps the reading end open until it has written,
            // so a task that ends first never makes that write a SIGPIPE.
            let mine = Arc::new(mine);
            let keep = Arc::clone(&mine);
            let ticket = work_pool::submit(
                move || crate::http_run(job),
                move || {
                    let _ = theirs.write_all(&[1]);
                    drop(keep);
                },
            );
            let fd = mine.as_raw_fd();
            inflight().push(Inflight {
                task,
                ticket,
                wake: mine,
            });
            fd
        }
    };
    crate::mir_task::karac_mir_ready_fd(i64::from(fd), 0)
}

/// The answer to the running task's request, when its `__ready` sent one:
/// the call after it delivers this instead of sending the request again.
pub(crate) fn take_answer() -> Option<Answer> {
    let task = key(crate::mir_task::current_task()?);
    let e = {
        let mut v = inflight();
        let i = v.iter().position(|e| e.task == task)?;
        v.swap_remove(i)
    };
    // Its readiness saw the helper's byte, which comes after the answer.
    while !e.ticket.done() {
        std::thread::yield_now();
    }
    Some(
        e.ticket
            .take()
            .unwrap_or_else(|| Err("the request failed".to_string())),
    )
}
