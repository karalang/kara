//! Type-erased capacity-bounded queue for compiled Kāra programs.
//!
//! This is the AOT-codegen realization of the `BoundedChannel[T]` surface the
//! tree-walk interpreter implements with a `VecDeque<Value>` side-table
//! (`src/interpreter/method_call_bounded_channel.rs`). Like `channel.rs` and
//! `map.rs`, the queue is **type-erased**: the payload travels as raw byte
//! blobs and `elem_size` is passed per `send`/`recv` call (not stored at
//! construction) — `T` is statically known at every op site (the typed
//! `BoundedChannel[T]` receiver) but NOT at `BoundedChannel.new()`.
//!
//! **Target-independent.** A bounded queue is a `VecDeque` behind a lock — no
//! scheduler dependency, so this module is compiled unconditionally (like
//! `channel.rs`) and the `karac_runtime_bounded_channel_*` externs are present
//! in every archive (native, sequential wasm, threaded wasm). The native pool
//! spawns real OS threads, so the queue must be thread-safe.
//!
//! **v1 single-threaded semantics (collapsed), matching the interpreter.** The
//! bound is the backpressure: a `send` that finds the buffer at capacity
//! returns `0` (the codegen lowering builds `Err(ChannelError.Full)`); a `recv`
//! on an empty buffer returns `0` (`Option.None`). Both `OnFull` variants
//! (`Block`, `FailFast`) collapse to fail-fast in v1 — real parking-on-full
//! lands with the network event loop (`design.md`: the `suspends` execution
//! verb), at which point the `on_full` arg threaded through `new` becomes
//! load-bearing. Until then `recv`/`send` never block on any target, so unlike
//! `channel.rs` there is no `Condvar` and no platform split.
//!
//! **Lifetime — single owner.** `BoundedChannel[T]` has no `clone` and no
//! Sender/Receiver split, so there is no refcount: the value has one owner and
//! `karac_runtime_bounded_channel_drop` frees it (and any undrained payloads)
//! at that owner's scope exit. Mirrors the `TaskGroup` handle shape (a single
//! `Box` behind an `i64`), not the unbounded channel's two-counter scheme.

use std::collections::VecDeque;
use std::sync::Mutex;

/// `#[repr(C)]` is not load-bearing for field access (codegen never GEPs into
/// this — it holds only the opaque `*mut KaracBoundedChannel` as an `i64` and
/// passes it back through the externs), but kept for a stable layout.
#[repr(C)]
pub struct KaracBoundedChannel {
    /// Max buffered values. A 0-capacity channel always fails `send`.
    capacity: usize,
    /// Type-erased payloads, oldest at the front. Guarded by the lock so the
    /// native pool's worker threads can share the handle safely.
    queue: Mutex<VecDeque<Box<[u8]>>>,
}

// The handle can be shared by `ref` across par/spawn branches; the `Mutex`
// serializes payload access, so concurrent `send`/`recv` is sound.
unsafe impl Send for KaracBoundedChannel {}
unsafe impl Sync for KaracBoundedChannel {}

/// `karac_runtime_bounded_channel_new(capacity, on_full) -> *mut`.
///
/// `capacity` is clamped to be non-negative (a negative bound is treated as 0,
/// matching the interpreter's `i.max(0)`). `on_full` is accepted for
/// forward-compatibility with parking-on-full but is **ignored in v1** — both
/// `Block` (0) and `FailFast` (1) collapse to fail-fast (see module docs).
///
/// # Safety
/// FFI entry point. The returned pointer must eventually be released by
/// `karac_runtime_bounded_channel_drop`; codegen guarantees this via the
/// `BoundedChannel` Drop scope-exit cleanup.
#[no_mangle]
pub extern "C" fn karac_runtime_bounded_channel_new(
    capacity: i64,
    _on_full: u8,
) -> *mut KaracBoundedChannel {
    let bc = Box::new(KaracBoundedChannel {
        capacity: capacity.max(0) as usize,
        queue: Mutex::new(VecDeque::new()),
    });
    Box::into_raw(bc)
}

/// `karac_runtime_bounded_channel_send(ch, val_ptr, elem_size) -> u8` — enqueue
/// `*val_ptr` if there is room. Returns `1` when the value was buffered, `0`
/// when the buffer is at capacity (the codegen lowering builds `Ok(())` vs
/// `Err(Full)`). A null handle (a hand-rolled `handle_id: 0` that bypassed
/// `new`) fails closed with `0`. `elem_size` is `u64` (ABI-identical on wasm32
/// + native — the `__karac_malloc64` size_t discipline).
///
/// # Safety
/// `ch` must be live (or null); `val_ptr` must point to at least `elem_size`
/// readable bytes when `elem_size != 0`.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_bounded_channel_send(
    ch: *mut KaracBoundedChannel,
    val_ptr: *const u8,
    elem_size: u64,
) -> u8 {
    if ch.is_null() {
        return 0;
    }
    let elem_size = elem_size as usize;
    let mut queue = (*ch).queue.lock().unwrap();
    if queue.len() >= (*ch).capacity {
        return 0;
    }
    let mut blob = vec![0u8; elem_size].into_boxed_slice();
    if elem_size != 0 && !val_ptr.is_null() {
        std::ptr::copy_nonoverlapping(val_ptr, blob.as_mut_ptr(), elem_size);
    }
    queue.push_back(blob);
    1
}

/// `karac_runtime_bounded_channel_recv(ch, out_ptr, elem_size) -> u8` —
/// **non-blocking** dequeue: pop the front blob into `*out_ptr` (return `1`),
/// else zero-fill `*out_ptr` and return `0` (the lowering builds `Some`/`None`
/// from the discriminant). Never parks. A null handle returns `0`.
///
/// # Safety
/// `ch` must be live (or null); `out_ptr` must point to at least `elem_size`
/// writable bytes when `elem_size != 0`.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_bounded_channel_recv(
    ch: *mut KaracBoundedChannel,
    out_ptr: *mut u8,
    elem_size: u64,
) -> u8 {
    if ch.is_null() {
        return 0;
    }
    let elem_size = elem_size as usize;
    let popped = (*ch).queue.lock().unwrap().pop_front();
    match popped {
        Some(blob) => {
            if elem_size != 0 && !out_ptr.is_null() {
                std::ptr::copy_nonoverlapping(blob.as_ptr(), out_ptr, elem_size);
            }
            1
        }
        None => {
            if elem_size != 0 && !out_ptr.is_null() {
                std::ptr::write_bytes(out_ptr, 0, elem_size);
            }
            0
        }
    }
}

/// `karac_runtime_bounded_channel_drop(ch)` — free the channel and any
/// undrained payloads. Single-owner (no refcount): emitted once at the owning
/// binding's scope exit by the `BoundedChannel` Drop lowering. Null is a no-op.
///
/// # Safety
/// `ch` must be a live pointer returned by `karac_runtime_bounded_channel_new`
/// (or null); consumes it.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_bounded_channel_drop(ch: *mut KaracBoundedChannel) {
    if ch.is_null() {
        return;
    }
    drop(Box::from_raw(ch));
}

#[cfg(test)]
mod tests {
    use super::*;

    // OnFull discriminants (Block=0, FailFast=1 by declaration order); v1
    // ignores them, so the value passed here is immaterial.
    const FAIL_FAST: u8 = 1;

    unsafe fn send_i64(ch: *mut KaracBoundedChannel, v: i64) -> u8 {
        karac_runtime_bounded_channel_send(ch, &v as *const i64 as *const u8, 8)
    }
    unsafe fn recv_i64(ch: *mut KaracBoundedChannel) -> (u8, i64) {
        let mut out: i64 = -1;
        let got = karac_runtime_bounded_channel_recv(ch, &mut out as *mut i64 as *mut u8, 8);
        (got, out)
    }

    #[test]
    fn fifo_within_capacity() {
        unsafe {
            let ch = karac_runtime_bounded_channel_new(2, FAIL_FAST);
            assert_eq!(send_i64(ch, 10), 1);
            assert_eq!(send_i64(ch, 20), 1);
            assert_eq!(recv_i64(ch), (1, 10));
            assert_eq!(recv_i64(ch), (1, 20));
            karac_runtime_bounded_channel_drop(ch);
        }
    }

    #[test]
    fn send_full_returns_zero() {
        unsafe {
            let ch = karac_runtime_bounded_channel_new(2, FAIL_FAST);
            assert_eq!(send_i64(ch, 1), 1);
            assert_eq!(send_i64(ch, 2), 1);
            // Buffer at capacity → fail closed.
            assert_eq!(send_i64(ch, 3), 0);
            // Draining one frees a slot.
            assert_eq!(recv_i64(ch), (1, 1));
            assert_eq!(send_i64(ch, 3), 1);
            karac_runtime_bounded_channel_drop(ch);
        }
    }

    #[test]
    fn recv_empty_returns_zero() {
        unsafe {
            let ch = karac_runtime_bounded_channel_new(4, FAIL_FAST);
            assert_eq!(recv_i64(ch), (0, 0)); // empty → None, out zeroed
            karac_runtime_bounded_channel_drop(ch);
        }
    }

    #[test]
    fn zero_capacity_always_full() {
        unsafe {
            let ch = karac_runtime_bounded_channel_new(0, FAIL_FAST);
            assert_eq!(send_i64(ch, 1), 0);
            assert_eq!(recv_i64(ch), (0, 0));
            karac_runtime_bounded_channel_drop(ch);
        }
    }

    #[test]
    fn negative_capacity_clamps_to_zero() {
        unsafe {
            let ch = karac_runtime_bounded_channel_new(-5, FAIL_FAST);
            assert_eq!(send_i64(ch, 1), 0);
            karac_runtime_bounded_channel_drop(ch);
        }
    }

    #[test]
    fn null_handle_fails_closed() {
        unsafe {
            let null = std::ptr::null_mut::<KaracBoundedChannel>();
            assert_eq!(send_i64(null, 1), 0);
            assert_eq!(recv_i64(null), (0, -1)); // recv leaves out untouched on null
            karac_runtime_bounded_channel_drop(null); // no-op
        }
    }

    #[test]
    fn zero_size_element() {
        unsafe {
            let ch = karac_runtime_bounded_channel_new(2, FAIL_FAST);
            assert_eq!(
                karac_runtime_bounded_channel_send(ch, std::ptr::null(), 0),
                1
            );
            let mut sink = 0u8;
            assert_eq!(
                karac_runtime_bounded_channel_recv(ch, &mut sink as *mut u8, 0),
                1
            );
            assert_eq!(
                karac_runtime_bounded_channel_recv(ch, &mut sink as *mut u8, 0),
                0
            );
            karac_runtime_bounded_channel_drop(ch);
        }
    }

    #[test]
    fn cross_thread_transfer() {
        unsafe {
            let ch = karac_runtime_bounded_channel_new(128, FAIL_FAST);
            let ch_addr = ch as usize;
            let producer = std::thread::spawn(move || {
                let ch = ch_addr as *mut KaracBoundedChannel;
                for i in 0..100i64 {
                    karac_runtime_bounded_channel_send(ch, &i as *const i64 as *const u8, 8);
                }
            });
            producer.join().unwrap();
            for expected in 0..100i64 {
                assert_eq!(recv_i64(ch), (1, expected));
            }
            assert_eq!(recv_i64(ch), (0, 0));
            karac_runtime_bounded_channel_drop(ch);
        }
    }
}
