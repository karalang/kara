//! Counting semaphore for compiled Kāra programs.
//!
//! The AOT-codegen realization of the `Semaphore` surface the tree-walk
//! interpreter implements with a `SemEntry { available, max }` side-table
//! (`src/interpreter/method_call_semaphore.rs`). The `Semaphore` Kāra struct
//! carries only an `i64 handle_id` — the `*mut KaracSemaphore` round-tripped
//! through `ptrtoint`/`inttoptr` — exactly like `BoundedChannel`.
//!
//! **Target-independent.** A permit counter behind a lock has no scheduler
//! dependency, so this module is compiled into every archive (native,
//! sequential wasm, threaded wasm). The native pool spawns real OS threads, so
//! the counter must be thread-safe — the `Mutex` gives correct cross-task
//! permit accounting (a `par {}` branch that acquires/releases is sound).
//!
//! **v1 single-threaded semantics (collapsed), matching the interpreter.** An
//! `acquire` against an exhausted semaphore returns `0` immediately (the
//! codegen lowering builds `Err(SemaphoreError.Timeout)`) rather than parking
//! for `timeout` ms — real parking-with-timeout lands with the network event
//! loop (`design.md`: the `suspends` execution verb), at which point the
//! `timeout` arg becomes load-bearing. Until then `acquire` never blocks on any
//! target. `release` saturates at the initial permit count (returning more
//! permits than were taken would inflate the in-flight budget `new` declared).
//!
//! **Lifetime — single owner.** `Semaphore` has no `clone`, so there is no
//! refcount: `karac_runtime_semaphore_drop` frees it at the owning binding's
//! scope exit (the `Semaphore` Drop lowering). Mirrors `BoundedChannel`.

use std::sync::Mutex;

#[repr(C)]
pub struct KaracSemaphore {
    /// `(available, max)`. `available` is decremented by `acquire` and
    /// incremented by `release` (saturating at `max`).
    state: Mutex<(i64, i64)>,
}

// Shared by `ref` across par/spawn branches; the `Mutex` serializes the
// permit count, so concurrent acquire/release is sound.
unsafe impl Send for KaracSemaphore {}
unsafe impl Sync for KaracSemaphore {}

/// `karac_runtime_semaphore_new(permits) -> *mut`. A negative `permits` clamps
/// to 0 (a semaphore that never grants), matching the interpreter's `i.max(0)`.
///
/// # Safety
/// FFI entry point. The returned pointer must eventually be released by
/// `karac_runtime_semaphore_drop`; codegen guarantees this via the `Semaphore`
/// Drop scope-exit cleanup.
#[no_mangle]
pub extern "C" fn karac_runtime_semaphore_new(permits: i64) -> *mut KaracSemaphore {
    let permits = permits.max(0);
    Box::into_raw(Box::new(KaracSemaphore {
        state: Mutex::new((permits, permits)),
    }))
}

/// `karac_runtime_semaphore_acquire(sem, timeout) -> u8` — take a permit if one
/// is free (return `1`, decrement), else fail closed (`0`). `timeout` is
/// accepted for forward-compat with parking-on-empty but **ignored in v1** (the
/// collapsed non-parking semantics). A null handle (a hand-rolled
/// `Semaphore { handle_id: 0 }`) fails closed.
///
/// # Safety
/// `sem` must be live (or null).
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_semaphore_acquire(
    sem: *mut KaracSemaphore,
    _timeout: i64,
) -> u8 {
    if sem.is_null() {
        return 0;
    }
    let mut state = (*sem).state.lock().unwrap();
    if state.0 > 0 {
        state.0 -= 1;
        1
    } else {
        0
    }
}

/// `karac_runtime_semaphore_release(sem)` — return a permit, saturating at the
/// initial budget (`max`). Null is a no-op.
///
/// # Safety
/// `sem` must be live (or null).
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_semaphore_release(sem: *mut KaracSemaphore) {
    if sem.is_null() {
        return;
    }
    let mut state = (*sem).state.lock().unwrap();
    if state.0 < state.1 {
        state.0 += 1;
    }
}

/// `karac_runtime_semaphore_drop(sem)` — free the semaphore. Single-owner (no
/// refcount); emitted once at the owning binding's scope exit by the
/// `Semaphore` Drop lowering. Null is a no-op.
///
/// # Safety
/// `sem` must be a live pointer returned by `karac_runtime_semaphore_new` (or
/// null); consumes it.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_semaphore_drop(sem: *mut KaracSemaphore) {
    if sem.is_null() {
        return;
    }
    drop(Box::from_raw(sem));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grant_up_to_budget_then_fail() {
        unsafe {
            let s = karac_runtime_semaphore_new(2);
            assert_eq!(karac_runtime_semaphore_acquire(s, 0), 1);
            assert_eq!(karac_runtime_semaphore_acquire(s, 0), 1);
            // Exhausted → fail closed.
            assert_eq!(karac_runtime_semaphore_acquire(s, 0), 0);
            // Release frees one.
            karac_runtime_semaphore_release(s);
            assert_eq!(karac_runtime_semaphore_acquire(s, 0), 1);
            karac_runtime_semaphore_drop(s);
        }
    }

    #[test]
    fn release_saturates_at_budget() {
        unsafe {
            let s = karac_runtime_semaphore_new(1);
            // Release without acquire must not inflate past `max`.
            karac_runtime_semaphore_release(s);
            karac_runtime_semaphore_release(s);
            assert_eq!(karac_runtime_semaphore_acquire(s, 0), 1);
            assert_eq!(karac_runtime_semaphore_acquire(s, 0), 0);
            karac_runtime_semaphore_drop(s);
        }
    }

    #[test]
    fn zero_and_negative_permits_never_grant() {
        unsafe {
            let z = karac_runtime_semaphore_new(0);
            assert_eq!(karac_runtime_semaphore_acquire(z, 0), 0);
            karac_runtime_semaphore_drop(z);
            let n = karac_runtime_semaphore_new(-3);
            assert_eq!(karac_runtime_semaphore_acquire(n, 0), 0);
            karac_runtime_semaphore_drop(n);
        }
    }

    #[test]
    fn null_handle_fails_closed() {
        unsafe {
            let null = std::ptr::null_mut::<KaracSemaphore>();
            assert_eq!(karac_runtime_semaphore_acquire(null, 0), 0);
            karac_runtime_semaphore_release(null); // no-op
            karac_runtime_semaphore_drop(null); // no-op
        }
    }

    #[test]
    fn cross_thread_permit_accounting() {
        unsafe {
            let s = karac_runtime_semaphore_new(100);
            let addr = s as usize;
            let t = std::thread::spawn(move || {
                let s = addr as *mut KaracSemaphore;
                let mut got = 0;
                for _ in 0..100 {
                    got += karac_runtime_semaphore_acquire(s, 0) as i32;
                }
                got
            });
            let got_thread = t.join().unwrap();
            let mut got_main = 0;
            for _ in 0..100 {
                got_main += karac_runtime_semaphore_acquire(s, 0) as i32;
            }
            // Exactly 100 permits total across both threads.
            assert_eq!(got_thread + got_main, 100);
            karac_runtime_semaphore_drop(s);
        }
    }
}
