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
