//! Fallible / panicking allocation wrappers (phase-8-stdlib-floor item 8).
//!
//! Two entry points sit in front of the system allocator so the compiler can
//! dispatch a single allocation routine to the appropriate failure behaviour:
//!
//! * [`karac_alloc_fallible`] returns a non-null pointer on success and **null**
//!   on failure. The fallible-allocation `try_*` collection companions
//!   (`Vec.try_push`, …) call this and branch on null to build
//!   `Result.Err(AllocError)`.
//! * [`karac_alloc_or_panic`] is the infallible counterpart: it calls the
//!   fallible variant and, on null (OOM), prints a diagnostic and aborts —
//!   replacing the historical behaviour where the panicking collection methods
//!   (`Vec.push`, `Vec.with_capacity`, …) `malloc`'d without a null check and
//!   then dereferenced null (a segfault). It is the symbol those panicking
//!   methods route through.
//!
//! Both use the platform `malloc`'s natural alignment (suitable for any Kāra
//! value), matching the existing collection codegen which allocated via raw
//! `malloc` directly. The `malloc` signature mirrors the other in-crate
//! declarations (`-> *mut u8`) so the redeclaration is consistent.

extern "C" {
    fn malloc(size: usize) -> *mut u8;
    fn realloc(ptr: *mut u8, size: usize) -> *mut u8;
    fn calloc(nmemb: usize, size: usize) -> *mut u8;
}

/// Hard ceiling on any single runtime allocation: `2^61 - 1` bytes (~2.3 EB).
///
/// This can never fire on a satisfiable request — current hardware tops out
/// at 2^57 bytes of virtual address space (x86-64 5-level paging) — so it is
/// not a resource limit but a **provable compile-time invariant**: with every
/// Vec/String buffer capped here, `len <= cap <= cap * elem_size <= 2^61 - 1`
/// holds for every live collection (elements are >= 1 byte; codegen rejects
/// the zero-sized-element case separately), which lets codegen annotate
/// `len` loads with `!range [0, 2^61)` so LLVM can fold len-derived overflow
/// checks (`n + 1`, `l + 1` under a dominating bounds check) that otherwise
/// survive into hot loops (B-2026-07-10-5). Same posture as Rust's
/// `Layout`-size <= `isize::MAX` allocator contract, two powers of two
/// stricter. `u64` (not `usize`) so the wasm32 build — where `usize` is
/// 32-bit and the guard is trivially unreachable — still compiles.
pub const KARAC_MAX_ALLOC_BYTES: u64 = (1u64 << 61) - 1;

/// Fallible allocation — non-null on success, null on failure (OOM).
///
/// A zero-byte request is normalised to one byte so a successful allocation is
/// always a unique non-null pointer; the collection codegen treats a non-null
/// result as success, so a `malloc(0)`-returns-null platform must not be
/// mistaken for OOM. A request beyond [`KARAC_MAX_ALLOC_BYTES`] fails as OOM
/// (null) without touching the allocator — see the const's invariant note.
#[no_mangle]
pub extern "C" fn karac_alloc_fallible(size: usize) -> *mut u8 {
    if size as u64 > KARAC_MAX_ALLOC_BYTES {
        return std::ptr::null_mut();
    }
    let n = if size == 0 { 1 } else { size };
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    if n >= BUF_CACHE_MIN_BYTES && buf_cache::enabled() {
        let p = buf_cache::take(n);
        if !p.is_null() {
            return p;
        }
    }
    unsafe { malloc(n) }
}

/// Panicking allocation — the infallible counterpart of
/// [`karac_alloc_fallible`]. On OOM it writes a diagnostic to stderr and
/// aborts rather than returning null for the caller to dereference. The write
/// uses a `'static` byte slice (no heap allocation on the OOM path, which is
/// exactly what just failed).
#[no_mangle]
pub extern "C" fn karac_alloc_or_panic(size: usize) -> *mut u8 {
    let p = karac_alloc_fallible(size);
    if p.is_null() {
        // Lean raw-`write(2)` diagnostic (see `fatal` / B-2026-06-11-8) — NOT
        // `std::io::stderr()`, which would anchor ~250 KB of std-IO onto every
        // Vec-using binary through this force-kept, hot-path symbol.
        crate::fatal::write_stderr(b"panic: out of memory\n");
        std::process::abort();
    }
    p
}

/// Panicking reallocation — the grow-path counterpart of
/// [`karac_alloc_or_panic`]. Resizes a buffer to `size` bytes, letting the
/// system allocator extend it in place where it can (avoiding the
/// malloc-new + memcpy + free-old churn — and the transient old+new 2× peak —
/// the collection grow paths used to emit). `ptr` may be null, in which case
/// this is exactly `karac_alloc_or_panic(size)` (C guarantees
/// `realloc(NULL, n) == malloc(n)`); the grow codegen relies on that for the
/// first growth of an empty buffer. On OOM the original buffer is left intact
/// (per C), but since the panicking contract aborts there is no recovery path
/// to observe it — the same lean raw-`write(2)` diagnostic + abort as
/// `karac_alloc_or_panic`. **Never** call this on a non-heap pointer (a string
/// literal's rodata view); the String grow path guards that with a `cap > 0`
/// check and takes a fresh malloc+copy for the `cap == 0` static/null case.
#[no_mangle]
pub extern "C" fn karac_realloc_or_panic(ptr: *mut u8, size: usize) -> *mut u8 {
    // `realloc(NULL, n) == malloc(n)` (C11 7.22.3.5) — delegate the null case
    // to the malloc wrapper so a first-growth / exact-reserve allocation is
    // eligible for the recycling cache below. Same ceiling check, same
    // OOM-abort, so behaviour is unchanged; only the allocation source can
    // differ (parked buffer instead of fresh pages).
    if ptr.is_null() {
        return karac_alloc_or_panic(size);
    }
    // Same `KARAC_MAX_ALLOC_BYTES` ceiling as the malloc wrappers — the grow
    // path must uphold the identical `cap * elem_size` bound or the codegen
    // `!range` len-load invariant breaks on the first oversized grow.
    if size as u64 > KARAC_MAX_ALLOC_BYTES {
        crate::fatal::write_stderr(b"panic: out of memory\n");
        std::process::abort();
    }
    let n = if size == 0 { 1 } else { size };
    let p = unsafe { realloc(ptr, n) };
    if p.is_null() {
        crate::fatal::write_stderr(b"panic: out of memory\n");
        std::process::abort();
    }
    p
}

/// Panicking **zeroed** allocation — the `calloc` counterpart of
/// [`karac_alloc_or_panic`]. Allocates `count * size` bytes, all initialised to
/// zero, and aborts on OOM. This is the runtime half of the `Vec.filled(n, 0)`
/// codegen fast path: it matches rust's `vec![0; n]` (`__rust_alloc_zeroed`),
/// which the OS can serve from lazily-zeroed pages — strictly cheaper than the
/// `malloc + memset`/store-loop the general fill path emits (B-2026-07-08-7).
///
/// Taking `(count, size)` rather than a pre-multiplied byte count is deliberate:
/// `calloc` performs the `count * size` multiply with a built-in overflow check
/// (returning null on overflow), so an oversized request fails cleanly through
/// the shared OOM-abort path instead of wrapping around to a tiny allocation.
#[no_mangle]
pub extern "C" fn karac_alloc_zeroed_or_panic(count: usize, size: usize) -> *mut u8 {
    // Normalise a zero-byte request to one element so a successful allocation is
    // always a unique non-null pointer (same discipline as the fallible malloc
    // wrapper); `calloc(0, _)`/`calloc(_, 0)` may legitimately return null.
    let (count, size) = if count == 0 || size == 0 {
        (1, 1)
    } else {
        (count, size)
    };
    // `calloc` already refuses a wrapping `count * size`, but the total must
    // also respect the `KARAC_MAX_ALLOC_BYTES` ceiling (see the const's
    // invariant note) — a non-wrapping product beyond it must fail the same
    // way, not reach the allocator.
    match (count as u64).checked_mul(size as u64) {
        Some(total) if total <= KARAC_MAX_ALLOC_BYTES => {}
        _ => {
            crate::fatal::write_stderr(b"panic: out of memory\n");
            std::process::abort();
        }
    }
    let p = unsafe { calloc(count, size) };
    if p.is_null() {
        crate::fatal::write_stderr(b"panic: out of memory\n");
        std::process::abort();
    }
    p
}

// ── Large-buffer recycling cache (phase-10 · allocator buffer recycling) ──
//
// Fresh multi-megabyte Vec buffers allocated in a loop pay vm_allocate +
// first-touch page faults every iteration — and under parallel writers that
// fault storm serializes on the kernel VM lock (the GPU-SLIP-4g finding: the
// fresh-buffer LBM kata spent ~115 ms/run in faults that manual buffer reuse
// eliminated). This cache makes the reuse idiom transparent: codegen routes
// Vec/String buffer releases through [`karac_free_buf`], large buffers park
// here still mapped, and the next big-enough allocation gets its pages back
// fault-free through `karac_alloc_fallible`.
//
// Soundness rests on one property: every cached pointer is a live malloc
// allocation whose USABLE size — per the platform allocator (`malloc_size` /
// `malloc_usable_size`), not the caller's word — was recorded at insert, and
// a hit is only served when `usable >= requested`. A free-site hint that
// mis-states the logical size can therefore cost a recycling opportunity,
// never correctness. Routing is opportunistic for the same reason: free
// sites still calling libc `free` and allocation sites still calling
// `malloc` mix freely with routed ones — everything is malloc-backed either
// way, in both directions.
//
// Platform gate: the cache needs the allocator's usable-size query, so it is
// compiled for macOS + Linux only; elsewhere (wasm32, Windows) the entry
// points degrade to plain `free`/`malloc`.

/// Buffers below this stay on the plain `free`/`malloc` path. Page-fault
/// amortization only matters at multi-MB sizes, and small hot frees (String
/// churn) must not pay a cache lock or an allocator usable-size lookup.
#[cfg(any(target_os = "macos", target_os = "linux"))]
const BUF_CACHE_MIN_BYTES: usize = 1 << 20; // 1 MiB

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod buf_cache {
    use super::BUF_CACHE_MIN_BYTES;
    use core::sync::atomic::{AtomicU8, Ordering};

    // NO `std::sync` / `std::env` in here. Everything in this module is
    // transitively reachable from the force-kept hot-path symbols
    // (`karac_free_buf`, `karac_alloc_fallible`), and OnceLock / Mutex /
    // `env::var_os` all carry latent panic paths that anchor the ~250 KB
    // std panic/IO `__TEXT` cluster onto EVERY Vec/String binary — the
    // B-2026-06-11-8 class, re-caught for this module by
    // `e2e_vec_binary_stays_lean_no_heavy_runtime_floor` (33 KB → 285 KB).
    // Same discipline as `fatal`: libc `getenv`, hand-rolled tri-state
    // once, and a raw CAS spinlock (held for a 16-slot scan, ~ns; the
    // guarded ops are rare multi-MB alloc/free events).

    /// Total slot count — the working set this serves is a handful of large
    /// buffers (ping-pong grid pairs, per-connection buffers), not a general
    /// allocator tier.
    const SLOTS: usize = 16;
    /// Aggregate ceiling on parked bytes: bounds RSS retention on
    /// memory-tight hosts. `KARAC_BUF_CACHE=0` disables parking entirely.
    const MAX_TOTAL: usize = 128 << 20; // 128 MiB
    /// Per size-bracket slot cap (brackets = `ilog2(usable)`): a third
    /// same-size buffer is freed, not parked, so one shape can't squat the
    /// whole cache.
    const BRACKET_SLOTS: usize = 2;

    extern "C" {
        /// The platform allocator's usable-size query — ground truth for how
        /// many bytes a malloc pointer really owns.
        #[cfg_attr(target_os = "macos", link_name = "malloc_size")]
        #[cfg_attr(target_os = "linux", link_name = "malloc_usable_size")]
        fn platform_malloc_usable(ptr: *const core::ffi::c_void) -> usize;
    }

    #[derive(Clone, Copy)]
    struct Slot {
        /// Parked buffer address (`0` = empty slot). Stored as `usize` only
        /// for `const`-init; leak scanners (LSan, macOS `leaks`) read roots
        /// conservatively by bit pattern, so parked buffers still count as
        /// reachable-from-global, not leaked.
        ptr: usize,
        usable: usize,
    }

    struct Cache {
        slots: [Slot; SLOTS],
        total: usize,
    }

    struct SpinLocked {
        lock: AtomicU8,
        cache: core::cell::UnsafeCell<Cache>,
    }
    // Safety: `cache` is only touched between a successful CAS on `lock`
    // and the release store — a critical section with Acquire/Release
    // ordering.
    unsafe impl Sync for SpinLocked {}

    static CACHE: SpinLocked = SpinLocked {
        lock: AtomicU8::new(0),
        cache: core::cell::UnsafeCell::new(Cache {
            slots: [Slot { ptr: 0, usable: 0 }; SLOTS],
            total: 0,
        }),
    };

    /// Run `f` with the cache locked. Plain CAS spin — the critical
    /// sections are a bounded 16-slot scan.
    fn with_cache<R>(f: impl FnOnce(&mut Cache) -> R) -> R {
        while CACHE
            .lock
            .compare_exchange_weak(0, 1, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        let r = f(unsafe { &mut *CACHE.cache.get() });
        CACHE.lock.store(0, Ordering::Release);
        r
    }

    /// Panic-free env-flag read: 0 = unknown, 1 = yes, 2 = no. libc
    /// `getenv`, not `std::env` (see the module-head no-std-sync note).
    fn env_flag(state: &AtomicU8, name: &[u8], default_on: bool) -> bool {
        match state.load(Ordering::Relaxed) {
            1 => return true,
            2 => return false,
            _ => {}
        }
        extern "C" {
            fn getenv(name: *const u8) -> *const u8;
        }
        // (callers pass NUL-terminated literals; no assert — this module
        // links zero panic paths, even debug-only ones.)
        let v = unsafe { getenv(name.as_ptr()) };
        let on = if v.is_null() {
            default_on
        } else {
            // set: "0" means off when default-on; "1" means on when
            // default-off; anything else keeps the default.
            let b0 = unsafe { *v };
            let b1 = if b0 != 0 { unsafe { *v.add(1) } } else { 1 };
            if default_on {
                !(b0 == b'0' && b1 == 0)
            } else {
                b0 == b'1' && b1 == 0
            }
        };
        state.store(if on { 1 } else { 2 }, Ordering::Relaxed);
        on
    }

    pub(super) fn enabled() -> bool {
        static ENABLED: AtomicU8 = AtomicU8::new(0);
        env_flag(&ENABLED, b"KARAC_BUF_CACHE\0", true)
    }

    /// `KARAC_BUF_CACHE_STATS=1` — print hit/miss/park/reject counters to
    /// stderr at process exit. Diagnostic only (atexit + atomics); the
    /// counters are dead weight otherwise (relaxed increments on the
    /// large-buffer path only, never on the small-free fast path).
    pub(super) mod stats {
        use core::sync::atomic::{AtomicU64, AtomicU8, Ordering::Relaxed};

        pub static TAKE_HIT: AtomicU64 = AtomicU64::new(0);
        pub static TAKE_MISS: AtomicU64 = AtomicU64::new(0);
        pub static PUT_PARKED: AtomicU64 = AtomicU64::new(0);
        pub static PUT_REJECTED: AtomicU64 = AtomicU64::new(0);

        pub fn on() -> bool {
            static ON: AtomicU8 = AtomicU8::new(0);
            let first = ON.load(Relaxed) == 0;
            let on = super::env_flag(&ON, b"KARAC_BUF_CACHE_STATS\0", false);
            if on && first {
                // Hand-rolled, PANIC-FREE formatting into a stack buffer +
                // raw `write_stderr`. Not `format!`, and not even
                // `fatal::eprint_fmt` — the `core::fmt::Write` machinery
                // carries a `slice_index_fail` panic path, and ANY panic
                // path reachable from the force-kept hot symbols links
                // `rust_begin_unwind` → the ~250 KB std panic/backtrace
                // cluster onto every Vec binary (B-2026-06-11-8 class;
                // `-why_live` chain: karac_alloc_or_panic → take → dump →
                // StackMsg::write_str → slice_index_fail → panic_fmt).
                // Caught by e2e_vec_binary_stays_lean_no_heavy_runtime_floor.
                extern "C" fn dump() {
                    use core::sync::atomic::Ordering::Relaxed;
                    let mut buf = [0u8; 160];
                    let mut pos = 0usize;
                    fn put(buf: &mut [u8; 160], pos: &mut usize, bytes: &[u8]) {
                        for &b in bytes {
                            if let Some(slot) = buf.get_mut(*pos) {
                                *slot = b;
                                *pos += 1;
                            }
                        }
                    }
                    fn put_u64(buf: &mut [u8; 160], pos: &mut usize, mut v: u64) {
                        let mut tmp = [0u8; 20];
                        let mut n = 0usize;
                        loop {
                            if let Some(d) = tmp.get_mut(n) {
                                *d = b'0' + (v % 10) as u8;
                                n += 1;
                            }
                            v /= 10;
                            if v == 0 {
                                break;
                            }
                        }
                        while n > 0 {
                            n -= 1;
                            if let Some(&d) = tmp.get(n) {
                                put(buf, pos, &[d]);
                            }
                        }
                    }
                    put(&mut buf, &mut pos, b"karac-buf-cache: take hit=");
                    put_u64(&mut buf, &mut pos, super::stats::TAKE_HIT.load(Relaxed));
                    put(&mut buf, &mut pos, b" miss=");
                    put_u64(&mut buf, &mut pos, super::stats::TAKE_MISS.load(Relaxed));
                    put(&mut buf, &mut pos, b" | put parked=");
                    put_u64(&mut buf, &mut pos, super::stats::PUT_PARKED.load(Relaxed));
                    put(&mut buf, &mut pos, b" rejected=");
                    put_u64(&mut buf, &mut pos, super::stats::PUT_REJECTED.load(Relaxed));
                    put(&mut buf, &mut pos, b"\n");
                    crate::fatal::write_stderr(buf.get(..pos).unwrap_or(&[]));
                }
                extern "C" {
                    fn atexit(f: extern "C" fn()) -> i32;
                }
                unsafe { atexit(dump) };
            }
            on
        }

        pub fn bump(c: &AtomicU64) {
            if on() {
                c.fetch_add(1, Relaxed);
            }
        }
    }

    /// Best-fit take: the smallest parked buffer with `usable >= size`, and
    /// `usable <= 2 * size` so a small request can't drain a huge buffer
    /// (waste guard — the pow2-bracket discipline in miss/hit form). Null on
    /// miss; on a hit the pages come back still mapped, which is the whole
    /// point.
    pub(super) fn take(size: usize) -> *mut u8 {
        let got = with_cache(|c| {
            // NB: no slice indexing / ilog2 / arithmetic that can panic
            // anywhere in this module — a single core panic path here
            // references the std panicking+backtrace cluster and re-anchors
            // ~250 KB onto every Vec binary (see the module-head note).
            let mut best: Option<(usize, usize)> = None; // (slot idx, usable)
            for (i, s) in c.slots.iter().enumerate() {
                if s.ptr != 0
                    && s.usable >= size
                    && s.usable <= size.saturating_mul(2)
                    && best.is_none_or(|(_, bu)| s.usable < bu)
                {
                    best = Some((i, s.usable));
                }
            }
            match best {
                Some((i, _)) => match c.slots.get_mut(i) {
                    Some(slot) => {
                        let s = *slot;
                        *slot = Slot { ptr: 0, usable: 0 };
                        c.total = c.total.saturating_sub(s.usable);
                        s.ptr as *mut u8
                    }
                    None => core::ptr::null_mut(),
                },
                None => core::ptr::null_mut(),
            }
        });
        if got.is_null() {
            stats::bump(&stats::TAKE_MISS);
        } else {
            stats::bump(&stats::TAKE_HIT);
        }
        got
    }

    /// Park `ptr`. Returns `false` when the buffer doesn't qualify (too
    /// small by ground truth, cache full, bracket full, or total cap hit) —
    /// the caller then frees it normally.
    pub(super) fn put(ptr: *mut u8) -> bool {
        let usable = unsafe { platform_malloc_usable(ptr as *const _) };
        let parked = usable >= BUF_CACHE_MIN_BYTES
            && with_cache(|c| {
                if c.total.saturating_add(usable) > MAX_TOTAL {
                    return false;
                }
                // Panic-free ilog2 (usable >= 1 MiB here, but `ilog2()`
                // carries a zero-panic path that must not be linked).
                let bracket = usize::BITS - 1 - usable.leading_zeros();
                let mut empty: Option<usize> = None;
                let mut in_bracket = 0usize;
                for (i, s) in c.slots.iter().enumerate() {
                    if s.ptr == 0 {
                        if empty.is_none() {
                            empty = Some(i);
                        }
                    } else if usize::BITS - 1 - s.usable.leading_zeros() == bracket {
                        in_bracket = in_bracket.saturating_add(1);
                    }
                }
                if in_bracket >= BRACKET_SLOTS {
                    return false;
                }
                let Some(slot) = empty.and_then(|i| c.slots.get_mut(i)) else {
                    return false;
                };
                *slot = Slot {
                    ptr: ptr as usize,
                    usable,
                };
                c.total = c.total.saturating_add(usable);
                true
            });
        if parked {
            stats::bump(&stats::PUT_PARKED);
        } else {
            stats::bump(&stats::PUT_REJECTED);
        }
        parked
    }

    /// Test-only: release every parked buffer back to libc and report how
    /// many slots were occupied. Serializes test interference on the global
    /// cache (tests take `TEST_LOCK` around their whole body).
    #[cfg(test)]
    pub(super) fn drain_for_test() -> usize {
        extern "C" {
            fn free(ptr: *mut core::ffi::c_void);
        }
        with_cache(|c| {
            let mut n = 0;
            for s in c.slots.iter_mut() {
                if s.ptr != 0 {
                    unsafe { free(s.ptr as *mut core::ffi::c_void) };
                    *s = Slot { ptr: 0, usable: 0 };
                    n += 1;
                }
            }
            c.total = 0;
            n
        })
    }
}

/// Recycling-aware buffer release — the free-side counterpart of the cache
/// hook in [`karac_alloc_fallible`]. Codegen routes Vec/String data-buffer
/// frees here instead of libc `free` (scope-exit cleanup, overwrite-free,
/// and the synthesized drop fns).
///
/// `bytes_hint` is the logical buffer size when the emitting site knows it
/// (`cap * elem_size`), or `0` for "unknown — ask the allocator". A positive
/// hint below the cache threshold short-circuits straight to `free` without
/// taking the lock or querying the allocator, which keeps small hot frees at
/// libc cost; the hint is never trusted for sizing (see the cache's
/// soundness note). Null is a no-op, matching `free(NULL)`.
#[no_mangle]
pub extern "C" fn karac_free_buf(ptr: *mut u8, bytes_hint: usize) {
    if ptr.is_null() {
        return;
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    if (bytes_hint == 0 || bytes_hint >= BUF_CACHE_MIN_BYTES)
        && buf_cache::enabled()
        && buf_cache::put(ptr)
    {
        return;
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let _ = bytes_hint;
    extern "C" {
        fn free(ptr: *mut core::ffi::c_void);
    }
    unsafe { free(ptr as *mut core::ffi::c_void) }
}

#[cfg(test)]
mod tests {
    use super::*;

    // These tests intentionally leak the small allocations — declaring a
    // `free` shim here would clash with the crate's existing `free` extern
    // (signature redeclaration), and a few leaked bytes in a unit test that
    // exits immediately is harmless.

    #[test]
    fn fallible_small_alloc_is_non_null() {
        assert!(!karac_alloc_fallible(64).is_null());
    }

    #[test]
    fn fallible_zero_size_is_non_null() {
        // A zero-size request must still yield a usable non-null pointer.
        assert!(!karac_alloc_fallible(0).is_null());
    }

    #[test]
    fn or_panic_returns_non_null_on_success() {
        assert!(!karac_alloc_or_panic(128).is_null());
    }

    #[test]
    fn zeroed_or_panic_is_non_null_and_zeroed() {
        let p = karac_alloc_zeroed_or_panic(16, 8);
        assert!(!p.is_null());
        // All 16 * 8 bytes must be zero — the whole point of the calloc path.
        let all_zero = (0..16 * 8).all(|i| unsafe { *p.add(i) } == 0);
        assert!(all_zero);
    }

    #[test]
    fn zeroed_or_panic_zero_dims_are_non_null() {
        // A degenerate 0-count / 0-size request must still yield a usable pointer.
        assert!(!karac_alloc_zeroed_or_panic(0, 8).is_null());
        assert!(!karac_alloc_zeroed_or_panic(16, 0).is_null());
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn fallible_beyond_cap_is_null() {
        // One past KARAC_MAX_ALLOC_BYTES is refused up front (null, no
        // allocator call) — the invariant the codegen `!range` len-load
        // annotation rests on.
        assert!(karac_alloc_fallible((KARAC_MAX_ALLOC_BYTES + 1) as usize).is_null());
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn fallible_at_cap_reaches_allocator() {
        // Exactly at the cap the guard passes; the request then fails in the
        // allocator itself (no machine can satisfy 2^61 - 1 bytes), which is
        // the same null the caller handles. Pin only that it does not panic.
        let _ = karac_alloc_fallible(KARAC_MAX_ALLOC_BYTES as usize);
    }
}

/// Recycling-cache tests. The cache is process-global, so every test here
/// serializes on `cache_lock()` and drains before AND after its body —
/// without that, parallel test threads see each other's parked buffers.
#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod buf_cache_tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    fn cache_lock() -> MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        let g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        buf_cache::drain_for_test();
        g
    }

    const MB: usize = 1 << 20;

    #[test]
    fn free_buf_roundtrip_recycles_pointer() {
        let _g = cache_lock();
        let p = karac_alloc_or_panic(2 * MB);
        unsafe {
            *p = 0xA5;
            *p.add(2 * MB - 1) = 0x5A;
        }
        karac_free_buf(p, 2 * MB);
        // Same-size request must come back as the SAME buffer, pages intact.
        let q = karac_alloc_fallible(2 * MB);
        assert_eq!(q, p, "same-size alloc after free_buf must recycle");
        karac_free_buf(q, 2 * MB);
        buf_cache::drain_for_test();
    }

    #[test]
    fn small_hint_short_circuits_to_libc_free() {
        let _g = cache_lock();
        let p = karac_alloc_or_panic(64 * 1024);
        karac_free_buf(p, 64 * 1024);
        assert_eq!(buf_cache::drain_for_test(), 0, "sub-MB free must not park");
    }

    #[test]
    fn zero_hint_consults_allocator_ground_truth() {
        let _g = cache_lock();
        // hint 0 = "unknown" — a genuinely large buffer must still park...
        let p = karac_alloc_or_panic(2 * MB);
        karac_free_buf(p, 0);
        assert_eq!(
            buf_cache::drain_for_test(),
            1,
            "large buffer with hint 0 must park"
        );
        // ...and a small one must not, whatever the (absent) hint says.
        let s = karac_alloc_or_panic(4096);
        karac_free_buf(s, 0);
        assert_eq!(
            buf_cache::drain_for_test(),
            0,
            "small buffer with hint 0 must not park"
        );
    }

    #[test]
    fn waste_guard_rejects_oversized_slot() {
        let _g = cache_lock();
        let big = karac_alloc_or_panic(8 * MB);
        karac_free_buf(big, 8 * MB);
        // A 1 MiB request must NOT be served from the parked 8 MiB buffer
        // (usable > 2 * request) — it takes the fresh-malloc path instead.
        let q = karac_alloc_fallible(MB);
        assert_ne!(q, big, "waste guard must not serve an 8 MiB slot for 1 MiB");
        karac_free_buf(q, MB); // sub-threshold? exactly 1 MiB parks — drained below.
        assert!(
            buf_cache::drain_for_test() >= 1,
            "the 8 MiB buffer stayed parked"
        );
    }

    #[test]
    fn bracket_cap_limits_same_size_slots() {
        let _g = cache_lock();
        let a = karac_alloc_or_panic(2 * MB);
        let b = karac_alloc_or_panic(2 * MB);
        let c = karac_alloc_or_panic(2 * MB);
        karac_free_buf(a, 2 * MB);
        karac_free_buf(b, 2 * MB);
        karac_free_buf(c, 2 * MB); // third same-bracket buffer: freed, not parked
        assert_eq!(buf_cache::drain_for_test(), 2, "bracket cap is 2 slots");
    }

    #[test]
    fn total_cap_bounds_parked_bytes() {
        let _g = cache_lock();
        // 96 MiB parks; a further 48 MiB would exceed the 128 MiB total cap
        // and must be freed instead. Untouched pages → no real RSS cost here.
        let a = karac_alloc_or_panic(96 * MB);
        let b = karac_alloc_or_panic(48 * MB);
        karac_free_buf(a, 96 * MB);
        karac_free_buf(b, 48 * MB);
        assert_eq!(
            buf_cache::drain_for_test(),
            1,
            "total cap must reject the second"
        );
    }

    #[test]
    fn concurrent_churn_is_safe() {
        let _g = cache_lock();
        let threads: Vec<_> = (0..8)
            .map(|t| {
                std::thread::spawn(move || {
                    for i in 0..64 {
                        let sz = (1 + (t + i) % 4) * MB;
                        let p = karac_alloc_or_panic(sz);
                        unsafe {
                            *p = t as u8;
                            *p.add(sz - 1) = i as u8;
                        }
                        karac_free_buf(p, sz);
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        buf_cache::drain_for_test();
    }
}
