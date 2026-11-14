//! Stack-overflow detection for compiled Kāra binaries (B-2026-08-20-34).
//!
//! A Kāra AOT binary has its own LLVM `main`; Rust's `lang_start` never runs,
//! so std's guard-page handler — the thing that prints `thread 'main' has
//! overflowed its stack` in a Rust program — is never installed. Exhausting
//! the stack therefore killed the process with a bare `SIGSEGV`: exit 139,
//! **zero bytes** on stderr, nothing naming recursion as the cause. That put
//! Kāra level with `cc -O0` and behind both comparators it measures itself
//! against, on a failure mode its safety positioning implies it should beat.
//!
//! [`karac_runtime_install_stack_guard`] is called once from the prologue of
//! the generated `main`. It installs an alternate signal stack — mandatory,
//! since the fault happens precisely because there is no room left on the
//! normal one — and a `SIGSEGV`/`SIGBUS` handler that reports the overflow
//! and aborts.
//!
//! **A fault outside the stack is left alone.** The handler restores the
//! default disposition and re-raises, so a genuine null dereference still dies
//! as `SIGSEGV` with the same signal and core-dump behaviour it had before.
//! Claiming every segfault is a stack overflow would trade one misleading
//! diagnostic for another.
//!
//! **Availability.** The implementation needs `libc`, which this crate takes
//! under the `net` feature. That is not a conceptual dependency, it is which
//! archives get it: `net` is in the full, lean, GPU, regex and Arrow archives
//! — every NATIVE one — and absent only from the two wasm archives, which
//! have no POSIX signals to install into. If a native archive without `net` is
//! ever added, this gate is what needs revisiting.

/// Install the stack-overflow guard. Idempotent; the second and later calls
/// return immediately. Called from the generated `main`'s prologue, before any
/// user statement.
///
/// A no-op on any target without POSIX signals (both wasm archives), where the
/// call still links so codegen does not need a per-target emission rule.
#[no_mangle]
pub extern "C" fn karac_runtime_install_stack_guard() {
    #[cfg(all(unix, feature = "net"))]
    imp::install();
}

#[cfg(all(unix, feature = "net"))]
pub(crate) mod imp {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    static INSTALLED: AtomicBool = AtomicBool::new(false);
    /// Inclusive low / exclusive high bounds of the main thread's stack.
    /// `0` in `HI` means "bounds unknown" — the handler then declines to
    /// classify anything, so an unrecognised platform degrades to today's
    /// behaviour rather than to a wrong message.
    static STACK_LO: AtomicUsize = AtomicUsize::new(0);
    static STACK_HI: AtomicUsize = AtomicUsize::new(0);

    /// How far BELOW the reported stack base still counts as a stack fault.
    ///
    /// The guard page sits under the mapped stack, and how much of it
    /// `pthread_attr_getstack` folds into the reported region varies by libc
    /// and version. A fault a little below the base is an overflow every time;
    /// nothing else the program can address legitimately lives immediately
    /// under its own stack, so the slack costs no precision that matters.
    const GUARD_SLACK: usize = 1 << 20;

    /// Written with a single `write(2)`. Everything the handler touches must be
    /// async-signal-safe: no allocation, no formatting, no `std::io`.
    const MSG: &[u8] = b"fatal runtime error: stack overflow\n\
note: a recursive call chain or an oversized stack frame exhausted the stack.\n\
note: raise the limit with `ulimit -s`, or make the recursion iterative.\n";

    unsafe extern "C" fn handler(
        sig: libc::c_int,
        info: *mut libc::siginfo_t,
        _ctx: *mut libc::c_void,
    ) {
        unsafe {
            let fault = if info.is_null() {
                0
            } else {
                (*info).si_addr() as usize
            };
            let lo = STACK_LO.load(Ordering::Relaxed);
            let hi = STACK_HI.load(Ordering::Relaxed);
            let on = |lo: usize, hi: usize| {
                hi != 0 && fault < hi && fault >= lo.saturating_sub(GUARD_SLACK)
            };
            // A pool worker's stack counts as well (B-2026-11-14-7).
            let workers = WORKERS_SEEN.load(Ordering::Acquire).min(WORKER_SLOTS);
            let on_worker = WORKER_BOUNDS[..workers]
                .iter()
                .any(|(l, h)| on(l.load(Ordering::Relaxed), h.load(Ordering::Relaxed)));
            if on(lo, hi) || on_worker {
                let _ = libc::write(2, MSG.as_ptr().cast(), MSG.len());
                // 134 == 128 + SIGABRT, the shell's convention for an aborted
                // process, and what a Rust binary exits with on the same
                // failure.
                libc::_exit(134);
            }
            // Not ours. Restore the default disposition and re-raise so the
            // process dies exactly as it would have without this handler.
            libc::signal(sig, libc::SIG_DFL);
            libc::raise(sig);
        }
    }

    pub(super) fn install() {
        install_for(None);
    }

    /// The pool workers whose stacks the handler knows, by `[lo, hi)`. A
    /// worker past the last slot keeps the plain `SIGSEGV`.
    const WORKER_SLOTS: usize = 1024;
    #[allow(clippy::declare_interior_mutable_const)]
    const UNSET: (AtomicUsize, AtomicUsize) = (AtomicUsize::new(0), AtomicUsize::new(0));
    static WORKER_BOUNDS: [(AtomicUsize, AtomicUsize); WORKER_SLOTS] = [UNSET; WORKER_SLOTS];
    static WORKERS_SEEN: AtomicUsize = AtomicUsize::new(0);

    /// Extends the guard to the calling thread, a pool worker: its stack's
    /// bounds join the main thread's, and it gets an alternate signal stack
    /// of its own (one is per thread) for the handler to run on. A no-op
    /// when the program installed no guard (a sanitizer lane, which reports
    /// an overflow itself).
    pub(crate) fn guard_this_thread() {
        if !INSTALLED.load(Ordering::SeqCst) {
            return;
        }
        // SAFETY: reads the calling thread's own attributes.
        let Some((lo, hi)) = (unsafe { main_stack_bounds() }) else {
            return;
        };
        let i = WORKERS_SEEN.fetch_add(1, Ordering::AcqRel);
        if i >= WORKER_SLOTS {
            return;
        }
        WORKER_BOUNDS[i].0.store(lo, Ordering::Relaxed);
        WORKER_BOUNDS[i].1.store(hi, Ordering::Release);
        // SAFETY: an anonymous mapping handed to this thread's sigaltstack;
        // the thread lives as long as the process, so it is never unmapped.
        unsafe { alt_stack() };
    }

    /// Gives the calling thread an alternate signal stack; `false` when none
    /// could be made.
    unsafe fn alt_stack() -> bool {
        unsafe {
            let size = std::cmp::max(libc::SIGSTKSZ, 32 * 1024);
            let mem = libc::mmap(
                std::ptr::null_mut(),
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANON,
                -1,
                0,
            );
            if mem == libc::MAP_FAILED {
                return false;
            }
            let ss = libc::stack_t {
                ss_sp: mem,
                ss_flags: 0,
                ss_size: size,
            };
            libc::sigaltstack(&ss, std::ptr::null_mut()) == 0
        }
    }

    /// Installs the guard for the stack `[lo, hi)`, or the main thread's
    /// when `None`.
    pub(crate) fn install_for(bounds: Option<(usize, usize)>) {
        if INSTALLED.swap(true, Ordering::SeqCst) {
            return;
        }
        unsafe {
            if let Some((lo, hi)) = bounds.or_else(|| main_stack_bounds()) {
                STACK_LO.store(lo, Ordering::Relaxed);
                STACK_HI.store(hi, Ordering::Relaxed);
            }
            // The alternate stack is the whole point: when the fault is
            // "no stack left", a handler running on the normal stack faults
            // again and the process dies silently anyway.
            if !alt_stack() {
                return;
            }
            let mut act: libc::sigaction = std::mem::zeroed();
            act.sa_sigaction = handler as *const () as usize;
            act.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK | libc::SA_NODEFER;
            libc::sigemptyset(&mut act.sa_mask);
            libc::sigaction(libc::SIGSEGV, &act, std::ptr::null_mut());
            libc::sigaction(libc::SIGBUS, &act, std::ptr::null_mut());
        }
    }

    /// Bounds of the calling thread's stack: the main thread's at install,
    /// and a pool worker's from `guard_this_thread`, since an auto-par loop
    /// runs a deep recursion on a worker too (B-2026-11-14-7).
    #[cfg(target_os = "linux")]
    unsafe fn main_stack_bounds() -> Option<(usize, usize)> {
        unsafe {
            let mut attr: libc::pthread_attr_t = std::mem::zeroed();
            if libc::pthread_getattr_np(libc::pthread_self(), &mut attr) != 0 {
                return None;
            }
            let mut base: *mut libc::c_void = std::ptr::null_mut();
            let mut size: libc::size_t = 0;
            let rc = libc::pthread_attr_getstack(&attr, &mut base, &mut size);
            libc::pthread_attr_destroy(&mut attr);
            if rc != 0 || base.is_null() || size == 0 {
                return None;
            }
            Some((base as usize, base as usize + size))
        }
    }

    #[cfg(target_os = "macos")]
    unsafe fn main_stack_bounds() -> Option<(usize, usize)> {
        let me = unsafe { libc::pthread_self() };
        // macOS reports the stack's HIGH address (stacks grow down).
        let hi = unsafe { libc::pthread_get_stackaddr_np(me) } as usize;
        let size = unsafe { libc::pthread_get_stacksize_np(me) };
        if hi == 0 || size == 0 {
            return None;
        }
        Some((hi - size, hi))
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    unsafe fn main_stack_bounds() -> Option<(usize, usize)> {
        None
    }
}
