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
//! **`recv` blocks (threads-targets); non-blocking on sequential wasm.** On
//! any target with real threads (`any(not(wasm), wasm-threads)`) `recv` on an
//! empty queue **parks the calling thread on a `Condvar`** until a `send`
//! enqueues a value OR the last `Sender` drops (channel *closed* → `recv`
//! returns the zero-value/0-discriminant, so a producer that finishes without
//! sending wakes a blocked receiver instead of hanging it). This is the
//! spec'd source semantics (`design.md`: channel `recv` is `suspends`,
//! thread-blocking at v1) and mirrors the native pool's own `task_join`
//! Condvar-block (no work-helping — same limitation). On **sequential wasm**
//! (`all(wasm, not(wasm-threads))`) there is no other thread to drive the
//! sender, so `recv` stays **non-blocking** (empty → zero-fill + 0) — the
//! cooperative yield-to-event-loop that makes recv "block" there is the
//! phase-10 scheduler entry layered on `seq_scheduler.rs`. `try_recv` is
//! non-blocking on every target (a separate extern). The **interpreter**'s
//! `recv` is a documented non-blocking approximation (single tree-walk
//! thread); `karac run` and `karac build` therefore diverge only on a
//! recv-before-send race, which the structured-concurrency model avoids.
//!
//! **Lifetime — two counters.** `total` (Arc-style, lock-free) tracks every
//! live end for the free decision: `new` = 2 (the `Sender`/`Receiver` pair),
//! `clone` (`Sender.clone()`) +1, every `drop_sender`/`drop_receiver` -1,
//! freed at 0 with the `Arc` ordering (`Release` on the decrement, `Acquire`
//! fence before the free). `senders` (under the queue lock's authority via
//! the `closed` flag) drives *close*: `drop_sender` decrements it and, when it
//! hits 0, sets `closed` under the lock and `notify_all`s blocked receivers.
//! The split is why codegen emits `drop_sender` vs `drop_receiver` (it knows
//! each binding's `Sender`/`Receiver` surface type).

use std::collections::VecDeque;
use std::sync::atomic::{fence, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};

/// Queue + close flag, guarded by the channel's `Mutex`. `closed` is set
/// under the lock when the last `Sender` drops, so a blocked `recv`'s
/// predicate check + the close signal are synchronized (no lost wakeup).
struct Inner {
    queue: VecDeque<Box<[u8]>>,
    closed: bool,
}

/// `#[repr(C)]` is not load-bearing for field access (codegen never GEPs
/// into this — it only holds the opaque `*mut KaracChannel` and passes it
/// back through the externs), but kept for a stable, inspectable layout.
#[repr(C)]
pub struct KaracChannel {
    /// All live ends (Sender + Receiver). Frees the channel at 0.
    total: AtomicUsize,
    /// Live `Sender` ends. Last sender dropping → `closed` (drains blocked
    /// receivers). Fast-path counter; the authoritative close state is
    /// `Inner::closed`, set under the lock.
    senders: AtomicUsize,
    inner: Mutex<Inner>,
    /// Receivers park here when the queue is empty; `send` / last-sender-drop
    /// wake them. Unused on sequential wasm (recv never blocks there).
    not_empty: Condvar,
}

// The whole point of a channel is cross-thread transfer; the native pool
// moves the `*mut KaracChannel` to worker threads. Access to the payload is
// serialized by the `Mutex` and the counters are atomic, so this is sound.
unsafe impl Send for KaracChannel {}
unsafe impl Sync for KaracChannel {}

impl KaracChannel {
    fn new() -> *mut Self {
        let ch = Box::new(KaracChannel {
            // Two ends from a single `Channel.new()`: the Sender and the
            // Receiver the tuple destructure binds. See module docs.
            total: AtomicUsize::new(2),
            senders: AtomicUsize::new(1),
            inner: Mutex::new(Inner {
                queue: VecDeque::new(),
                closed: false,
            }),
            not_empty: Condvar::new(),
        });
        Box::into_raw(ch)
    }

    /// Release one `total` reference; free the channel (and any undrained
    /// payloads) when the last end goes away. Shared tail of both drop FFIs.
    ///
    /// # Safety
    /// `ch` must be live; consumes one `total` reference.
    unsafe fn release_total(ch: *mut KaracChannel) {
        if (*ch).total.fetch_sub(1, Ordering::Release) != 1 {
            return;
        }
        // Last reference. Acquire-fence so this thread sees every write made
        // through every other end before we run the destructor (Arc pattern).
        fence(Ordering::Acquire);
        drop(Box::from_raw(ch));
    }
}

/// Copy `elem_size` bytes out of a dequeued `blob` into `out_ptr` (the
/// got-a-value path). Shared by `recv` and `try_recv`.
#[inline]
unsafe fn deliver(blob: &[u8], out_ptr: *mut u8, elem_size: usize) {
    if elem_size != 0 && !out_ptr.is_null() {
        std::ptr::copy_nonoverlapping(blob.as_ptr(), out_ptr, elem_size);
    }
}

/// Zero-fill `out_ptr` (the empty/closed path) so a `-> T` `recv` lowering
/// reads a well-defined zero-value.
#[inline]
unsafe fn deliver_empty(out_ptr: *mut u8, elem_size: usize) {
    if elem_size != 0 && !out_ptr.is_null() {
        std::ptr::write_bytes(out_ptr, 0, elem_size);
    }
}

/// `karac_runtime_channel_new() -> *mut KaracChannel`.
///
/// Type-agnostic: the element size is carried per `send`/`recv` call, not
/// stored here (see module docs). Returns a channel with `total` 2 / 1 sender.
///
/// # Safety
/// FFI entry point. The returned pointer must eventually be released by
/// `karac_runtime_channel_drop_sender` + `_drop_receiver` (clone-adjusted);
/// codegen guarantees this via scope-exit cleanup.
#[no_mangle]
pub extern "C" fn karac_runtime_channel_new() -> *mut KaracChannel {
    KaracChannel::new()
}

/// `karac_runtime_channel_clone(ch) -> *mut KaracChannel` — second `Sender`
/// handle to the same channel (backs `Sender.clone()`; `Receiver` has no
/// clone). Returns the same pointer, bumping both the `senders` (close) and
/// `total` (lifetime) counts.
///
/// # Safety
/// `ch` must be a live pointer returned by `karac_runtime_channel_new`.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_channel_clone(ch: *mut KaracChannel) -> *mut KaracChannel {
    if ch.is_null() {
        return ch;
    }
    // Relaxed: the new reference is created from an existing live one, so a
    // happens-before already exists (Arc's own reasoning). The receiver-side
    // `closed` check re-reads under the lock, so a sender count bumped here is
    // observed correctly there.
    (*ch).senders.fetch_add(1, Ordering::Relaxed);
    (*ch).total.fetch_add(1, Ordering::Relaxed);
    ch
}

/// `karac_runtime_channel_drop_sender(ch)` — drop a `Sender` end. Decrements
/// `senders`; when the last sender goes, marks the channel `closed` under the
/// lock and wakes every blocked `recv` (which then returns the closed/empty
/// answer instead of hanging). Always releases one `total` reference.
///
/// # Safety
/// `ch` must be live; consumes one sender + one `total` reference.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_channel_drop_sender(ch: *mut KaracChannel) {
    if ch.is_null() {
        return;
    }
    if (*ch).senders.fetch_sub(1, Ordering::Release) == 1 {
        // Last sender. Set `closed` UNDER the lock so a receiver's
        // empty-and-not-closed check + park is synchronized against it (no
        // lost wakeup), then wake all parked receivers.
        {
            let mut inner = (*ch).inner.lock().unwrap();
            inner.closed = true;
        }
        (*ch).not_empty.notify_all();
    }
    KaracChannel::release_total(ch);
}

/// `karac_runtime_channel_drop_receiver(ch)` — drop the `Receiver` end. No
/// effect on close/wake (an unbounded `send` never blocks, so there is no
/// parked sender to wake). Releases one `total` reference.
///
/// # Safety
/// `ch` must be live; consumes one `total` reference.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_channel_drop_receiver(ch: *mut KaracChannel) {
    if ch.is_null() {
        return;
    }
    KaracChannel::release_total(ch);
}

/// `karac_runtime_channel_send(ch, val_ptr, elem_size)` — copy `elem_size`
/// bytes from `*val_ptr` into a fresh blob, enqueue it, and wake one parked
/// receiver. Returns nothing (the Kāra `Sender.send` is `-> ()`). `elem_size`
/// is `u64` (ABI-identical on wasm32 + native — the `__karac_malloc64` size_t
/// discipline).
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
    // Enqueue UNDER the lock (sets the non-empty condition the receiver's
    // park predicate checks), then signal one waiter.
    (*ch).inner.lock().unwrap().queue.push_back(blob);
    (*ch).not_empty.notify_one();
}

/// `karac_runtime_channel_recv(ch, out_ptr, elem_size) -> u8` — **blocking**
/// receive (on threads-targets): dequeue the front blob into `*out_ptr`,
/// parking on the `Condvar` while the queue is empty and the channel is open.
/// Returns 1 once a value is delivered, or 0 if the channel closed empty (the
/// last `Sender` dropped without sending) — in which case `*out_ptr` is
/// zero-filled. The `recv` lowering's result type is `T`, so it uses the
/// out slot regardless; the 0 case is the floor's zero-value answer.
///
/// On **sequential wasm** there is no thread to drive a sender, so this is
/// non-blocking (one-shot pop, else 0) — see module docs.
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
    channel_recv_impl(ch, out_ptr, elem_size as usize)
}

/// Blocking `recv` body for threads-targets: park on the `Condvar` while the
/// queue is empty and the channel is open.
#[cfg(any(not(target_family = "wasm"), feature = "wasm-threads"))]
unsafe fn channel_recv_impl(ch: *mut KaracChannel, out_ptr: *mut u8, elem_size: usize) -> u8 {
    let mut inner = (*ch).inner.lock().unwrap();
    loop {
        if let Some(blob) = inner.queue.pop_front() {
            deliver(&blob, out_ptr, elem_size);
            return 1;
        }
        if inner.closed {
            deliver_empty(out_ptr, elem_size);
            return 0;
        }
        // Empty and open → park until `send` / last-sender-drop signals.
        inner = (*ch).not_empty.wait(inner).unwrap();
    }
}

/// Non-blocking `recv` body for sequential wasm: there is no other thread to
/// drive a sender, so parking would deadlock — one-shot pop, else 0 (the
/// cooperative yield-to-event-loop that makes recv "block" there is a separate
/// phase-10 entry).
#[cfg(all(target_family = "wasm", not(feature = "wasm-threads")))]
unsafe fn channel_recv_impl(ch: *mut KaracChannel, out_ptr: *mut u8, elem_size: usize) -> u8 {
    match (*ch).inner.lock().unwrap().queue.pop_front() {
        Some(blob) => {
            deliver(&blob, out_ptr, elem_size);
            1
        }
        None => {
            deliver_empty(out_ptr, elem_size);
            0
        }
    }
}

/// `karac_runtime_channel_try_recv(ch, out_ptr, elem_size) -> u8` —
/// **non-blocking** receive (every target): pop the front blob if present
/// (return 1), else zero-fill + return 0 immediately. Backs `Receiver.
/// try_recv() -> Option[T]` (codegen builds `Some`/`None` from the
/// discriminant). Never parks, never consults `closed`.
///
/// # Safety
/// `ch` must be live; `out_ptr` must point to at least `elem_size` writable
/// bytes.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_channel_try_recv(
    ch: *mut KaracChannel,
    out_ptr: *mut u8,
    elem_size: u64,
) -> u8 {
    if ch.is_null() {
        return 0;
    }
    let elem_size = elem_size as usize;
    let popped = (*ch).inner.lock().unwrap().queue.pop_front();
    match popped {
        Some(blob) => {
            deliver(&blob, out_ptr, elem_size);
            1
        }
        None => {
            deliver_empty(out_ptr, elem_size);
            0
        }
    }
}

/// `karac_runtime_channel_pending(ch) -> u64` — number of queued, not-yet-
/// received blobs. Used by the host-async `animation_frames` producer to
/// coalesce: the requestAnimationFrame callback only feeds a fresh `()` tick
/// when the worker has drained the previous one, so a slow consumer (per-frame
/// compute over the frame budget) cannot grow an unbounded backlog of frame
/// tokens — the channel holds at most one pending tick. Read under the queue
/// lock; never parks.
///
/// # Safety
/// `ch` must be live (or null, which reports 0).
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_channel_pending(ch: *mut KaracChannel) -> u64 {
    if ch.is_null() {
        return 0;
    }
    (*ch).inner.lock().unwrap().queue.len() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    // Helper: round-trip an i64 (elem_size 8) through a channel.
    unsafe fn send_i64(ch: *mut KaracChannel, v: i64) {
        karac_runtime_channel_send(ch, &v as *const i64 as *const u8, 8);
    }
    // Blocking recv (parks while empty + open on this native test target).
    unsafe fn recv_i64(ch: *mut KaracChannel) -> (u8, i64) {
        let mut out: i64 = -1;
        let got = karac_runtime_channel_recv(ch, &mut out as *mut i64 as *mut u8, 8);
        (got, out)
    }
    // Non-blocking try_recv.
    unsafe fn try_recv_i64(ch: *mut KaracChannel) -> (u8, i64) {
        let mut out: i64 = -1;
        let got = karac_runtime_channel_try_recv(ch, &mut out as *mut i64 as *mut u8, 8);
        (got, out)
    }

    #[test]
    fn fifo_round_trip() {
        unsafe {
            let ch = karac_runtime_channel_new();
            send_i64(ch, 10);
            send_i64(ch, 20);
            send_i64(ch, 30);
            // Values present → recv returns immediately (no park).
            assert_eq!(recv_i64(ch), (1, 10));
            assert_eq!(recv_i64(ch), (1, 20));
            assert_eq!(recv_i64(ch), (1, 30));
            karac_runtime_channel_drop_sender(ch);
            karac_runtime_channel_drop_receiver(ch);
        }
    }

    #[test]
    fn try_recv_empty_is_nonblocking() {
        unsafe {
            let ch = karac_runtime_channel_new();
            // Empty + open: try_recv must NOT park — returns 0, out zeroed.
            assert_eq!(try_recv_i64(ch), (0, 0));
            karac_runtime_channel_drop_sender(ch);
            karac_runtime_channel_drop_receiver(ch);
        }
    }

    #[test]
    fn recv_blocks_until_send() {
        // Load-immune blocking evidence: main parks in recv with the queue
        // empty; a worker sleeps, then sends. The recv returning the value at
        // all proves it blocked (a non-blocking recv would have returned 0
        // before the worker ran).
        unsafe {
            let ch = karac_runtime_channel_new();
            let ch_addr = ch as usize;
            let worker = std::thread::spawn(move || {
                let ch = ch_addr as *mut KaracChannel;
                std::thread::sleep(std::time::Duration::from_millis(40));
                karac_runtime_channel_send(ch, &777i64 as *const i64 as *const u8, 8);
                karac_runtime_channel_drop_sender(ch);
            });
            // Blocks ~40ms, then receives 777.
            assert_eq!(recv_i64(ch), (1, 777));
            worker.join().unwrap();
            karac_runtime_channel_drop_receiver(ch);
        }
    }

    #[test]
    fn recv_wakes_on_close_without_value() {
        // A blocked recv must NOT hang forever when the producer finishes
        // without sending: the last sender drop closes the channel and wakes
        // the receiver with the empty/0 answer.
        unsafe {
            let ch = karac_runtime_channel_new();
            let ch_addr = ch as usize;
            let worker = std::thread::spawn(move || {
                let ch = ch_addr as *mut KaracChannel;
                std::thread::sleep(std::time::Duration::from_millis(40));
                // No send — just drop the sender (channel closes).
                karac_runtime_channel_drop_sender(ch);
            });
            // Parks, then wakes with closed/empty (0) instead of hanging.
            assert_eq!(recv_i64(ch), (0, 0));
            worker.join().unwrap();
            karac_runtime_channel_drop_receiver(ch);
        }
    }

    #[test]
    fn cloned_sender_keeps_channel_open() {
        // Two senders; dropping one must NOT close the channel. The surviving
        // sender's value is still received; close only fires at the last
        // sender drop.
        unsafe {
            let ch = karac_runtime_channel_new(); // senders=1, total=2
            let ch2 = karac_runtime_channel_clone(ch); // senders=2, total=3, same ptr
            assert_eq!(ch2, ch);
            send_i64(ch, 42);
            karac_runtime_channel_drop_sender(ch); // senders=1 — still open
            assert_eq!(recv_i64(ch), (1, 42));
            // Queue empty, still one sender → try_recv non-blocking 0 (a
            // blocking recv here would park, which we don't want in a test).
            assert_eq!(try_recv_i64(ch), (0, 0));
            karac_runtime_channel_drop_sender(ch); // senders=0 — closes
                                                   // Now closed + empty → blocking recv returns immediately.
            assert_eq!(recv_i64(ch), (0, 0));
            karac_runtime_channel_drop_receiver(ch); // total=0 — freed
        }
    }

    #[test]
    fn zero_size_element_round_trips() {
        // Channel[()] — elem_size 0. send/recv must not touch the (possibly
        // dangling) payload pointers but must still track presence.
        unsafe {
            let ch = karac_runtime_channel_new();
            karac_runtime_channel_send(ch, std::ptr::null(), 0);
            let mut sink = 0u8;
            assert_eq!(karac_runtime_channel_recv(ch, &mut sink as *mut u8, 0), 1);
            // Close so the follow-up recv doesn't park.
            karac_runtime_channel_drop_sender(ch);
            assert_eq!(karac_runtime_channel_recv(ch, &mut sink as *mut u8, 0), 0);
            karac_runtime_channel_drop_receiver(ch);
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
                karac_runtime_channel_drop_sender(ch);
            });
            producer.join().unwrap();
            // Producer joined (sender dropped → closed), so all 100 are
            // queued; drain in order, then the closed/empty 0.
            for expected in 0..100i64 {
                assert_eq!(recv_i64(ch), (1, expected));
            }
            assert_eq!(recv_i64(ch), (0, 0));
            karac_runtime_channel_drop_receiver(ch);
        }
    }
}
