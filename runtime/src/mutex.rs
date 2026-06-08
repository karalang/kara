//! Blocking (futex-style) mutex backing `lock` blocks — the slow-path half.
//!
//! `lock` codegen (`src/codegen/method_call.rs::compile_lock_block` +
//! `CleanupAction::ReleaseMutex` in `src/codegen/runtime.rs`) owns the
//! **uncontended fast path** inline; this module is only called on contention.
//! The lock flag (field 0 of the `{ i64 lockflag, T value }` Mutex aggregate)
//! carries a 3-state value — Drepper's "Futexes Are Tricky" protocol, with a
//! bucketed `std` `Condvar` standing in for the OS futex so the same source
//! works on every archive (native, lean, wasm, wasm-threads):
//!
//! ```text
//!   0 = free
//!   1 = locked, no known waiters
//!   2 = locked, waiter(s) parked
//! ```
//!
//! - **Acquire** (codegen): `cmpxchg(0 -> 1)`. Success → held, no runtime call
//!   (this is the whole point — uncontended locking stays inline, ~spinlock
//!   cost, no regression). Failure → `karac_runtime_mutex_lock`, which marks
//!   the flag contended (`2`) and parks until it wins the lock.
//! - **Release** (codegen): `xchg(-> 0)` and read the prior state. Prior `1`
//!   means no waiters → inline-only, no call. Prior `2` means a waiter is
//!   parked → `karac_runtime_mutex_unlock_wake` notifies the bucket.
//!
//! The bucket's `Mutex` is the serialization point that closes the classic
//! lost-wakeup window: a waiter re-checks the flag *under* the bucket lock
//! before sleeping, and the unlock notifies *while holding* that same bucket
//! lock — so an unlock that lands between a waiter's flag-check and its sleep
//! cannot be missed (the notify blocks until the waiter is actually parked).

#![allow(clippy::missing_safety_doc)]

// ── Threads-capable targets: native (any feature set) + wasm32 with threads ──
#[cfg(any(not(target_family = "wasm"), feature = "wasm-threads"))]
mod parking {
    use std::sync::atomic::{AtomicI64, Ordering::SeqCst};
    use std::sync::{Condvar, Mutex};

    struct Bucket {
        mtx: Mutex<()>,
        cv: Condvar,
    }

    /// Fixed bucket array — addresses hash into it (à la `parking_lot`). A
    /// collision only costs a spurious wakeup (the woken waiter re-checks its
    /// own flag and re-parks); correctness never depends on the hash. `const`
    /// init keeps it allocation-free and available before `main`.
    const N: usize = 64;
    static BUCKETS: [Bucket; N] = [const {
        Bucket {
            mtx: Mutex::new(()),
            cv: Condvar::new(),
        }
    }; N];

    #[inline]
    fn bucket_for(addr: usize) -> &'static Bucket {
        // Drop the low bits (mutex flags are ≥8-byte aligned) before indexing.
        &BUCKETS[(addr >> 4) % N]
    }

    /// Block until this thread holds the lock at `flag`. Called by codegen
    /// only after its inline `cmpxchg(0 -> 1)` failed. Marks the flag contended
    /// (`2`) and parks on the bucketed condvar, re-marking contended on each
    /// retry so an unlock always observes `2` and wakes us.
    pub unsafe fn lock(flag: *mut i64) {
        let a = AtomicI64::from_ptr(flag);
        // Mark contended and read the prior state. If it was already free
        // (`0`), we just took the lock; otherwise park-retry until it is.
        let mut c = a.swap(2, SeqCst);
        while c != 0 {
            let b = bucket_for(flag as usize);
            let guard = b.mtx.lock().unwrap_or_else(|p| p.into_inner());
            // Only sleep if the lock is still held-contended. The check is
            // under the bucket lock, and `unlock_wake` notifies under the same
            // lock, so a release racing this check cannot be lost: either we
            // observe `!= 2` here and retry the swap immediately, or we sleep
            // and the release's notify (which must take the bucket lock) wakes
            // us once we are parked.
            if a.load(SeqCst) == 2 {
                let _unused = b.cv.wait(guard).unwrap_or_else(|p| p.into_inner());
            }
            c = a.swap(2, SeqCst);
        }
    }

    /// Wake waiters parked on the lock at `flag`. Called by codegen release
    /// only when its `xchg(-> 0)` observed prior state `2`. `notify_all`
    /// (not `notify_one`) because distinct addresses may share a bucket — each
    /// woken waiter re-checks its own flag and re-parks if it is not the one.
    pub unsafe fn unlock_wake(flag: *mut i64) {
        let b = bucket_for(flag as usize);
        let _guard = b.mtx.lock().unwrap_or_else(|p| p.into_inner());
        b.cv.notify_all();
    }
}

// ── Sequential wasm (wasm32-wasip1, no threads) ──
// Single cooperative thread: a `lock` is always uncontended at acquire, so
// codegen's inline `cmpxchg(0 -> 1)` succeeds and these are never reached on a
// correct program. There is no other thread to wake, so a `Condvar` would just
// hang — provide a benign, atomic-free fallback (single-threaded, so a plain
// volatile read/write is sound) that keeps the symbols in the archive and lets
// a stray re-entrant call fail the same way the old spinlock did, not silently
// corrupt.
#[cfg(all(target_family = "wasm", not(feature = "wasm-threads")))]
mod parking {
    pub unsafe fn lock(flag: *mut i64) {
        loop {
            if core::ptr::read_volatile(flag) == 0 {
                core::ptr::write_volatile(flag, 2);
                return;
            }
            core::hint::spin_loop();
        }
    }

    pub unsafe fn unlock_wake(_flag: *mut i64) {}
}

/// Slow-path lock acquire. See module docs / `parking::lock`.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_mutex_lock(flag: *mut i64) {
    parking::lock(flag);
}

/// Slow-path unlock wake. See module docs / `parking::unlock_wake`.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_mutex_unlock_wake(flag: *mut i64) {
    parking::unlock_wake(flag);
}
