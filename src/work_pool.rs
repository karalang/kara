//! A bounded pool of helper threads for the blocking calls a task waits
//! on without holding its thread: the HTTP client's requests
//! (docs/library/io.md § HTTP client). The MIR interpreter
//! (`src/mir/interp.rs`) and the compiled backend's runtime share it by
//! `#[path]`, as they do `tls_conn.rs`. Plain `std`, no `karac` types.
//!
//! **Interim.** A request runs to its end on a helper, and the task that
//! made it parks on its [`Ticket`] until it is done. At most [`HELPERS`]
//! run at once; the rest queue, and their tasks stay parked. The end state
//! is the client on non-blocking sockets driven by the executor's reactor,
//! with no helper at all.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

/// How many helpers run jobs at once. Excess jobs queue.
pub const HELPERS: usize = 64;

type Job = Box<dyn FnOnce() + Send>;

/// The queue the helpers take jobs from, and how many helpers there are
/// and how many of them wait for a job.
struct Pool {
    queue: Mutex<Queue>,
    ready: Condvar,
}

#[derive(Default)]
struct Queue {
    jobs: VecDeque<Job>,
    helpers: usize,
    idle: usize,
}

fn pool() -> &'static Pool {
    static POOL: OnceLock<Pool> = OnceLock::new();
    POOL.get_or_init(|| Pool {
        queue: Mutex::new(Queue::default()),
        ready: Condvar::new(),
    })
}

/// A job's result, once a helper has run it. Dropping the ticket before
/// then does not stop the job; its result is dropped when it ends.
pub struct Ticket<T> {
    inner: Arc<Inner<T>>,
}

struct Inner<T> {
    done: AtomicBool,
    result: Mutex<Option<T>>,
}

impl<T> Clone for Ticket<T> {
    fn clone(&self) -> Self {
        Ticket {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<T> std::fmt::Debug for Ticket<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ticket")
            .field("done", &self.done())
            .finish()
    }
}

impl<T> Ticket<T> {
    /// Whether the job has ended, so [`Ticket::take`] has its result.
    pub fn done(&self) -> bool {
        self.inner.done.load(Ordering::Acquire)
    }

    /// The job's result, once it has ended, unless it panicked or a clone
    /// took it first.
    pub fn take(&self) -> Option<T> {
        if !self.done() {
            return None;
        }
        self.inner.result.lock().ok()?.take()
    }
}

/// Runs `job` on a helper, and `notify` after its result is in the ticket
/// (the compiled backend wakes its reactor there).
pub fn submit<T, F, N>(job: F, notify: N) -> Ticket<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
    N: FnOnce() + Send + 'static,
{
    let inner = Arc::new(Inner {
        done: AtomicBool::new(false),
        result: Mutex::new(None),
    });
    let out = Arc::clone(&inner);
    let run: Job = Box::new(move || {
        // A job that panics ends with no result, so its ticket is done and
        // its `take` finds nothing, and the helper goes on.
        let v = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)).ok();
        if let Ok(mut r) = out.result.lock() {
            *r = v;
        }
        out.done.store(true, Ordering::Release);
        notify();
    });
    let p = pool();
    let mut q = p.queue.lock().unwrap_or_else(|e| e.into_inner());
    q.jobs.push_back(run);
    if q.idle == 0 && q.helpers < HELPERS {
        q.helpers += 1;
        let spawned = std::thread::Builder::new()
            .name("karac-helper".into())
            .spawn(helper);
        if spawned.is_err() {
            // The jobs wait for a helper that does exist; with none at
            // all, this one runs here.
            q.helpers -= 1;
            if q.helpers == 0 {
                let job = q.jobs.pop_back();
                drop(q);
                if let Some(job) = job {
                    job();
                }
                return Ticket { inner };
            }
        }
    }
    drop(q);
    p.ready.notify_one();
    Ticket { inner }
}

/// A helper: runs queued jobs, and waits for the next one.
fn helper() {
    let p = pool();
    let mut q = p.queue.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        if let Some(job) = q.jobs.pop_front() {
            drop(q);
            job();
            q = p.queue.lock().unwrap_or_else(|e| e.into_inner());
            continue;
        }
        q.idle += 1;
        q = p.ready.wait(q).unwrap_or_else(|e| e.into_inner());
        q.idle -= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn work_pool_runs_more_jobs_than_helpers() {
        let tickets: Vec<_> = (0..HELPERS * 2)
            .map(|i| {
                submit(
                    move || {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                        i * 2
                    },
                    || {},
                )
            })
            .collect();
        for (i, t) in tickets.iter().enumerate() {
            while !t.done() {
                std::thread::yield_now();
            }
            assert_eq!(t.take(), Some(i * 2));
            assert_eq!(t.take(), None);
        }
        let q = pool().queue.lock().unwrap();
        assert!(q.helpers <= HELPERS);
    }
}
