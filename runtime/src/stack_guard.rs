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
mod imp {
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
            if hi != 0 && fault < hi && fault >= lo.saturating_sub(GUARD_SLACK) {
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
        if INSTALLED.swap(true, Ordering::SeqCst) {
            return;
        }
        unsafe {
            if let Some((lo, hi)) = main_stack_bounds() {
                STACK_LO.store(lo, Ordering::Relaxed);
                STACK_HI.store(hi, Ordering::Relaxed);
            }
            // The alternate stack is the whole point: when the fault is
            // "no stack left", a handler running on the normal stack faults
            // again and the process dies silently anyway.
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
                return;
            }
            let ss = libc::stack_t {
                ss_sp: mem,
                ss_flags: 0,
                ss_size: size,
            };
            if libc::sigaltstack(&ss, std::ptr::null_mut()) != 0 {
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

    /// Bounds of the MAIN thread's stack. Spawned tasks run on pool threads
    /// with their own stacks; this guard deliberately covers the main thread,
    /// which is where a deep user recursion runs.
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
