//! Per-key token-bucket rate limiter for compiled Kāra programs.
//!
//! The AOT-codegen realization of the `RateLimiter` surface the tree-walk
//! interpreter implements with a `RateLimiterEntry { rate, capacity, buckets }`
//! side-table (`src/interpreter/method_call_rate_limiter.rs`). The
//! `RateLimiter` Kāra struct carries only an `i64 handle_id` — the
//! `*mut KaracRateLimiter` round-tripped through `ptrtoint`/`inttoptr`, exactly
//! like `BoundedChannel` / `Semaphore`.
//!
//! **Semantics mirror the interpreter byte-for-byte.** Each key gets a bucket
//! that starts full (`capacity` tokens), so the first `capacity` `try_acquire`
//! calls for a fresh key burst through; thereafter grants are paced by `rate`
//! tokens/second via lazy refill against a monotonic clock. `try_acquire`
//! refills `elapsed * rate` tokens (capped at `capacity`), then consumes one if
//! `>= 1.0`. Non-blocking — never waits (the waiting `acquire` form is
//! event-loop dependent, carved as a follow-on).
//!
//! **Target-independent.** `std::time::Instant` reads the monotonic clock,
//! available on native and on `wasm32-wasip1` (WASI `clock_time_get`), so this
//! module compiles into every archive. The native pool spawns real OS threads,
//! so the per-key bucket map is behind a `Mutex` — a `par {}` branch calling
//! `try_acquire` shares one limiter soundly.
//!
//! **Lifetime — single owner.** No `clone`, no refcount:
//! `karac_runtime_rate_limiter_drop` frees it at the owning binding's scope
//! exit (the `RateLimiter` Drop lowering). Mirrors `BoundedChannel`.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

struct TokenBucket {
    tokens: f64,
    last: Instant,
}

#[repr(C)]
pub struct KaracRateLimiter {
    rate: f64,
    capacity: f64,
    /// Per-key `(tokens, last_refill)`. Keyed by the raw key bytes (the Kāra
    /// `String`'s UTF-8) so no lossy decode is needed. Guarded by the lock for
    /// the native pool's worker threads.
    buckets: Mutex<HashMap<Vec<u8>, TokenBucket>>,
}

// Shared by `ref` across par/spawn branches; the `Mutex` serializes the bucket
// map, so concurrent `try_acquire` is sound.
unsafe impl Send for KaracRateLimiter {}
unsafe impl Sync for KaracRateLimiter {}

/// `karac_runtime_rate_limiter_new(rate, capacity) -> *mut`. Both are clamped
/// non-negative (a 0-rate / 0-capacity limiter never grants), matching the
/// interpreter's `i.max(0)`.
///
/// # Safety
/// FFI entry point. The returned pointer must eventually be released by
/// `karac_runtime_rate_limiter_drop`; codegen guarantees this via the
/// `RateLimiter` Drop scope-exit cleanup.
#[no_mangle]
pub extern "C" fn karac_runtime_rate_limiter_new(
    rate: i64,
    capacity: i64,
) -> *mut KaracRateLimiter {
    Box::into_raw(Box::new(KaracRateLimiter {
        rate: rate.max(0) as f64,
        capacity: capacity.max(0) as f64,
        buckets: Mutex::new(HashMap::new()),
    }))
}

/// `karac_runtime_rate_limiter_try_acquire(rl, key_ptr, key_len) -> u8` — take
/// one token for `key` if the (lazily-refilled) bucket has `>= 1.0` (return
/// `1`, consume), else report limited (`0`). A null handle fails closed.
///
/// # Safety
/// `rl` must be live (or null); `key_ptr` must point to `key_len` readable
/// bytes when `key_len != 0`.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_rate_limiter_try_acquire(
    rl: *mut KaracRateLimiter,
    key_ptr: *const u8,
    key_len: i64,
) -> u8 {
    if rl.is_null() {
        return 0;
    }
    let now = Instant::now();
    let key: Vec<u8> = if key_ptr.is_null() || key_len <= 0 {
        Vec::new()
    } else {
        std::slice::from_raw_parts(key_ptr, key_len as usize).to_vec()
    };
    let rate = (*rl).rate;
    let capacity = (*rl).capacity;
    let mut buckets = (*rl).buckets.lock().unwrap();
    let bucket = buckets.entry(key).or_insert(TokenBucket {
        tokens: capacity,
        last: now,
    });
    let elapsed = now.saturating_duration_since(bucket.last).as_secs_f64();
    bucket.tokens = (bucket.tokens + elapsed * rate).min(capacity);
    bucket.last = now;
    if bucket.tokens >= 1.0 {
        bucket.tokens -= 1.0;
        1
    } else {
        0
    }
}

/// `karac_runtime_rate_limiter_drop(rl)` — free the limiter and all per-key
/// buckets. Single-owner (no refcount); emitted once at the owning binding's
/// scope exit by the `RateLimiter` Drop lowering. Null is a no-op.
///
/// # Safety
/// `rl` must be a live pointer returned by `karac_runtime_rate_limiter_new` (or
/// null); consumes it.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_rate_limiter_drop(rl: *mut KaracRateLimiter) {
    if rl.is_null() {
        return;
    }
    drop(Box::from_raw(rl));
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe fn try_acq(rl: *mut KaracRateLimiter, key: &str) -> u8 {
        karac_runtime_rate_limiter_try_acquire(rl, key.as_ptr(), key.len() as i64)
    }

    #[test]
    fn initial_burst_then_limited() {
        unsafe {
            // rate 1/sec, cap 3: a fresh key bursts 3, then limited (the 4th
            // call happens within microseconds → negligible refill).
            let rl = karac_runtime_rate_limiter_new(1, 3);
            assert_eq!(try_acq(rl, "k"), 1);
            assert_eq!(try_acq(rl, "k"), 1);
            assert_eq!(try_acq(rl, "k"), 1);
            assert_eq!(try_acq(rl, "k"), 0);
            karac_runtime_rate_limiter_drop(rl);
        }
    }

    #[test]
    fn per_key_independence() {
        unsafe {
            let rl = karac_runtime_rate_limiter_new(1, 1);
            assert_eq!(try_acq(rl, "a"), 1);
            assert_eq!(try_acq(rl, "a"), 0);
            // A different key has its own full bucket.
            assert_eq!(try_acq(rl, "b"), 1);
            assert_eq!(try_acq(rl, "b"), 0);
            karac_runtime_rate_limiter_drop(rl);
        }
    }

    #[test]
    fn zero_capacity_never_grants() {
        unsafe {
            let rl = karac_runtime_rate_limiter_new(1000, 0);
            assert_eq!(try_acq(rl, "k"), 0);
            karac_runtime_rate_limiter_drop(rl);
        }
    }

    #[test]
    fn null_handle_fails_closed() {
        unsafe {
            let null = std::ptr::null_mut::<KaracRateLimiter>();
            assert_eq!(try_acq(null, "k"), 0);
            karac_runtime_rate_limiter_drop(null); // no-op
        }
    }

    #[test]
    fn refill_grants_after_wait() {
        unsafe {
            // High rate so a short sleep refills a token deterministically.
            let rl = karac_runtime_rate_limiter_new(1000, 1);
            assert_eq!(try_acq(rl, "k"), 1);
            assert_eq!(try_acq(rl, "k"), 0);
            std::thread::sleep(std::time::Duration::from_millis(5));
            // ~5 tokens refilled (capped at 1) → grant.
            assert_eq!(try_acq(rl, "k"), 1);
            karac_runtime_rate_limiter_drop(rl);
        }
    }
}
