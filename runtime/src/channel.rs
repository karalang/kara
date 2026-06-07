//! Type-erased MPSC-shaped channel queue for compiled Kāra programs.
//!
//! This is the AOT-codegen realization of the same `Channel[T]` surface the
//! tree-walk interpreter implements with `Arc<Mutex<VecDeque<Value>>>`
//! (`src/interpreter/method_call_channel.rs`). Like `map.rs`, the queue is
//! **type-erased**: the payload travels as raw byte blobs. `elem_size` is
//! passed per `send`/`recv` call (not stored at construction) — the element
//! type `T` is statically known at every channel-op site (the typed
//! `Sender[T]`/`Receiver[T]` receiver) but NOT at `Channel.new()` (the
//! associated-call dispatch sees only the type name), so threading the size
//! through the ops keeps `channel_new` fully type-agnostic. `send`/`recv`
//! memcpy `elem_size` bytes through caller-owned slots — exactly the shape
//! the spawn result transport (`karac_runtime_task_join`'s `out_slot`) uses.
//!
//! **Target-independent.** A channel is a queue behind a lock — it has no
//! scheduler dependency, so this module is compiled unconditionally (like
//! `map.rs`) and the `karac_runtime_channel_*` externs are present in every
//! archive: native (`scheduler.rs`), sequential wasm (`seq_scheduler.rs`),
//! and threaded wasm (`wasm_threads_scheduler.rs`). The native pool spawns
//! real OS threads, so the channel must be thread-safe even though the v1
//! floor's *scheduling* is non-blocking.
//!
//! **v1 floor — non-blocking, mirrors the interpreter.** `recv` on an empty
//! queue does NOT block (no event loop / parking exists yet): it zero-fills
//! the out slot and returns 0, matching the interpreter's `unwrap_or(Unit)`.
//! Codegen's `recv` lowering ignores the discriminant (the result type is
//! `T`, so the zero-value is the floor's "empty" answer); `try_recv` uses it
//! to build `Some`/`None`. Real parking-on-empty lands with the
//! "yield-to-event-loop on WASM channel-receive" entry (phase-10-targets.md).
//!
//! **Lifetime — refcounted, two ends.** `new` returns a channel with
//! refcount 2 (one `Sender`, one `Receiver` — the pair a `Channel.new()`
//! destructure produces). `clone` (backing `Sender.clone()`) increments;
//! `drop` (emitted at each end's scope exit via
//! `CleanupAction::DropChannelEnd`) decrements and frees at zero. Ordering
//! mirrors `Arc`: `Release` on the decrement, `Acquire` fence before the
//! free, so the freeing thread sees every prior sender's writes.

use std::collections::VecDeque;
use std::sync::atomic::{fence, AtomicUsize, Ordering};
use std::sync::Mutex;

/// `#[repr(C)]` is not load-bearing for field access (codegen never GEPs
/// into this — it only holds the opaque `*mut KaracChannel` and passes it
/// back through the externs), but kept for a stable, inspectable layout.
#[repr(C)]
pub struct KaracChannel {
    refcount: AtomicUsize,
    queue: Mutex<VecDeque<Box<[u8]>>>,
}

// The whole point of a channel is cross-thread transfer; the native pool
// moves the `*mut KaracChannel` to worker threads. Access to the payload is
// serialized by the `Mutex` and the refcount is atomic, so this is sound.
unsafe impl Send for KaracChannel {}
unsafe impl Sync for KaracChannel {}

impl KaracChannel {
    fn new() -> *mut Self {
        let ch = Box::new(KaracChannel {
            // Two ends from a single `Channel.new()`: the Sender and the
            // Receiver the tuple destructure binds. See module docs.
            refcount: AtomicUsize::new(2),
            queue: Mutex::new(VecDeque::new()),
        });
        Box::into_raw(ch)
    }
}

/// `karac_runtime_channel_new() -> *mut KaracChannel`.
///
/// Type-agnostic: the element size is carried per `send`/`recv` call, not
/// stored here (see module docs). Returns a channel with refcount 2.
///
/// # Safety
/// FFI entry point. The returned pointer must eventually be released by
/// exactly two `karac_runtime_channel_drop` calls (or its `clone`-adjusted
/// equivalent); codegen guarantees this via scope-exit cleanup.
#[no_mangle]
pub extern "C" fn karac_runtime_channel_new() -> *mut KaracChannel {
    KaracChannel::new()
}

/// `karac_runtime_channel_clone(ch) -> *mut KaracChannel` — second handle to
/// the same channel (backs `Sender.clone()`). Returns the same pointer with
/// the refcount bumped, mirroring `Arc::clone`.
///
/// # Safety
/// `ch` must be a live pointer returned by `karac_runtime_channel_new`.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_channel_clone(ch: *mut KaracChannel) -> *mut KaracChannel {
    if ch.is_null() {
        return ch;
    }
    // Relaxed is sufficient for an increment: the new reference is created
    // from an existing live one, so a happens-before already exists (Arc's
    // own reasoning).
    (*ch).refcount.fetch_add(1, Ordering::Relaxed);
    ch
}

/// `karac_runtime_channel_drop(ch)` — release one reference; free the channel
/// (and any undrained payloads) when the last end goes away.
///
/// # Safety
/// `ch` must be a live pointer; this consumes one reference.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_channel_drop(ch: *mut KaracChannel) {
    if ch.is_null() {
        return;
    }
    if (*ch).refcount.fetch_sub(1, Ordering::Release) != 1 {
        return;
    }
    // Last reference. Acquire-fence so this thread sees every write made
    // through every other end before we run the destructor (Arc's pattern).
    fence(Ordering::Acquire);
    drop(Box::from_raw(ch));
}

/// `karac_runtime_channel_send(ch, val_ptr, elem_size)` — copy `elem_size`
/// bytes from `*val_ptr` into a fresh blob and enqueue it. Returns nothing
/// (the Kāra `Sender.send` is `-> ()`). `elem_size` is `u64` (ABI-identical
/// on wasm32 + native — the `__karac_malloc64` size_t discipline).
///
/// # Safety
/// `ch` must be live; `val_ptr` must point to at least `elem_size`
/// readable bytes.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_channel_send(
    ch: *mut KaracChannel,
    val_ptr: *const u8,
    elem_size: u64,
) {
    if ch.is_null() {
        return;
    }
    let elem_size = elem_size as usize;
    let mut blob = vec![0u8; elem_size].into_boxed_slice();
    if elem_size != 0 && !val_ptr.is_null() {
        std::ptr::copy_nonoverlapping(val_ptr, blob.as_mut_ptr(), elem_size);
    }
    (*ch).queue.lock().unwrap().push_back(blob);
}

/// `karac_runtime_channel_recv(ch, out_ptr, elem_size) -> u8` — dequeue the
/// front blob into `*out_ptr`. Returns 1 if a value was delivered, 0 if the
/// queue was empty. On empty, `*out_ptr` is zero-filled (`elem_size` bytes)
/// so the `recv` lowering — whose result type is `T` — reads a well-defined
/// zero-value (the floor's non-blocking "empty" answer); `try_recv` reads
/// the discriminant to build `Some`/`None`.
///
/// # Safety
/// `ch` must be live; `out_ptr` must point to at least `elem_size` writable
/// bytes.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_channel_recv(
    ch: *mut KaracChannel,
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

#[cfg(test)]
mod tests {
    use super::*;

    // Helper: round-trip an i64 (elem_size 8) through a channel.
    unsafe fn send_i64(ch: *mut KaracChannel, v: i64) {
        karac_runtime_channel_send(ch, &v as *const i64 as *const u8, 8);
    }
    unsafe fn recv_i64(ch: *mut KaracChannel) -> (u8, i64) {
        let mut out: i64 = -1;
        let got = karac_runtime_channel_recv(ch, &mut out as *mut i64 as *mut u8, 8);
        (got, out)
    }

    #[test]
    fn fifo_round_trip() {
        unsafe {
            let ch = karac_runtime_channel_new();
            send_i64(ch, 10);
            send_i64(ch, 20);
            send_i64(ch, 30);
            assert_eq!(recv_i64(ch), (1, 10));
            assert_eq!(recv_i64(ch), (1, 20));
            assert_eq!(recv_i64(ch), (1, 30));
            karac_runtime_channel_drop(ch);
            karac_runtime_channel_drop(ch);
        }
    }

    #[test]
    fn empty_recv_zero_fills_and_signals() {
        unsafe {
            let ch = karac_runtime_channel_new();
            // Empty queue: discriminant 0, out slot zeroed (not left at -1).
            assert_eq!(recv_i64(ch), (0, 0));
            karac_runtime_channel_drop(ch);
            karac_runtime_channel_drop(ch);
        }
    }

    #[test]
    fn refcount_frees_only_at_zero() {
        unsafe {
            let ch = karac_runtime_channel_new(); // rc = 2
            let ch2 = karac_runtime_channel_clone(ch); // rc = 3, same ptr
            assert_eq!(ch2, ch);
            send_i64(ch, 42);
            karac_runtime_channel_drop(ch); // rc = 2
            karac_runtime_channel_drop(ch); // rc = 1 — still alive
                                            // Still usable through the surviving reference.
            assert_eq!(recv_i64(ch), (1, 42));
            karac_runtime_channel_drop(ch); // rc = 0 — freed (ASAN/miri would flag a leak otherwise)
        }
    }

    #[test]
    fn zero_size_element_round_trips() {
        // Channel[()] — elem_size 0. send/recv must not touch the (possibly
        // dangling) payload pointers but must still track presence via the
        // discriminant.
        unsafe {
            let ch = karac_runtime_channel_new();
            karac_runtime_channel_send(ch, std::ptr::null(), 0);
            let mut sink = 0u8;
            assert_eq!(karac_runtime_channel_recv(ch, &mut sink as *mut u8, 0), 1);
            assert_eq!(karac_runtime_channel_recv(ch, &mut sink as *mut u8, 0), 0);
            karac_runtime_channel_drop(ch);
            karac_runtime_channel_drop(ch);
        }
    }

    #[test]
    fn cross_thread_transfer() {
        // The native pool moves the raw pointer to a worker thread; prove
        // Send/Sync hold in practice (a sender thread + main recv).
        unsafe {
            let ch = karac_runtime_channel_new();
            let ch_addr = ch as usize;
            let producer = std::thread::spawn(move || {
                let ch = ch_addr as *mut KaracChannel;
                for i in 0..100i64 {
                    karac_runtime_channel_send(ch, &i as *const i64 as *const u8, 8);
                }
                karac_runtime_channel_drop(ch);
            });
            producer.join().unwrap();
            // Producer joined, so all 100 are queued; drain in order.
            for expected in 0..100i64 {
                assert_eq!(recv_i64(ch), (1, expected));
            }
            assert_eq!(recv_i64(ch), (0, 0));
            karac_runtime_channel_drop(ch);
        }
    }
}
