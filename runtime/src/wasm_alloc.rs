//! Heap unification for the wasm runtime archive (phase-10 WASM build
//! path).
//!
//! Kāra codegen and this runtime share heap blocks across the FFI
//! boundary in both directions: codegen-emitted IR `free`s buffers the
//! runtime allocated with `std::alloc` (e.g. `karac_runtime_json_stringify`
//! output, `file.rs` error payloads), and the runtime `dealloc`s blocks
//! codegen `malloc`'d. On native targets that interop is sound because
//! Rust's platform default allocator IS libc `malloc`. On
//! `wasm32-wasip1`, Rust's default is its own bundled `dlmalloc`
//! instance — a *separate heap* from wasi-libc's `malloc` that the
//! C-level calls in karac IR hit — so every cross-boundary free would
//! corrupt one of the two heaps.
//!
//! Fix at the root: register wasi-libc's `malloc`/`free` as Rust's
//! global allocator for the wasm archive, so there is exactly one heap
//! no matter which side allocates or frees. Alignment contract:
//! wasi-libc's dlmalloc guarantees `max_align_t` (16-byte) alignment
//! from `malloc`; larger alignments route through `aligned_alloc`,
//! whose blocks `free` accepts per C11.
//!
//! Native targets keep the platform default — this module is compiled
//! only for wasm (see the `cfg` at the `mod` declaration in `lib.rs`).

use std::alloc::{GlobalAlloc, Layout};
use std::ffi::c_void;

extern "C" {
    // `*mut u8` (not `*mut c_void`) on `malloc`/`realloc` to match the externs
    // declared in `alloc.rs` / `lib.rs` — all are compiled on wasm, and a
    // signature mismatch trips the `clashing_extern_declarations` lint (the
    // `-D warnings` wasm clippy gate, B-2026-06-11-9). `*mut u8` is the majority
    // form; aligning these outliers removes the clash. ABI-identical regardless
    // (the `cabi_realloc` call site casts `c_void` ↔ `u8`, like `malloc` does).
    fn malloc(size: usize) -> *mut u8;
    fn calloc(nmemb: usize, size: usize) -> *mut c_void;
    fn aligned_alloc(alignment: usize, size: usize) -> *mut c_void;
    fn realloc(ptr: *mut u8, size: usize) -> *mut u8;
    fn free(ptr: *mut c_void);
}

/// `malloc`'s guaranteed alignment in wasi-libc (`max_align_t`).
const MALLOC_ALIGN: usize = 16;

struct LibcAlloc;

unsafe impl GlobalAlloc for LibcAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if layout.align() <= MALLOC_ALIGN {
            malloc(layout.size())
        } else {
            // C11 `aligned_alloc` requires size to be a multiple of the
            // alignment; round up (the Layout API caps align at 2^29, so
            // this cannot overflow a wasm32 usize for any allocatable
            // size).
            let size = layout.size().next_multiple_of(layout.align());
            aligned_alloc(layout.align(), size) as *mut u8
        }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if layout.align() <= MALLOC_ALIGN {
            calloc(1, layout.size()) as *mut u8
        } else {
            let ptr = self.alloc(layout);
            if !ptr.is_null() {
                std::ptr::write_bytes(ptr, 0, layout.size());
            }
            ptr
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
        free(ptr as *mut c_void);
    }
}

#[global_allocator]
static GLOBAL: LibcAlloc = LibcAlloc;

/// 64-bit-size `malloc` shim for karac-emitted IR.
///
/// Codegen declares the C allocator with an i64 size parameter (correct
/// for `size_t` on every 64-bit native target) and passes i64 byte
/// counts at every allocation site. wasm32's `size_t` is i32, and wasm
/// traps signature-mismatched calls (`signature_mismatch:malloc`), so
/// on `--target=wasm_wasi` codegen declares THIS symbol instead (see
/// `src/codegen/driver.rs::c_malloc_symbol`) and the shim narrows the
/// size for the real wasi-libc `malloc`. A count beyond the wasm32
/// address space saturates to `usize::MAX` so `malloc` fails cleanly
/// (null) instead of allocating a wrapped-around tiny block.
///
/// `free` needs no twin — its only parameter is a pointer, which lowers
/// to the correct width from the module datalayout.
#[no_mangle]
pub extern "C" fn __karac_malloc64(size: u64) -> *mut c_void {
    let size = usize::try_from(size).unwrap_or(usize::MAX);
    unsafe { malloc(size) as *mut c_void }
}

/// 64-bit-size shims for the fallible / panicking allocation wrappers —
/// the `size_t` twin of [`__karac_malloc64`] for B-2026-06-12-1.
/// `karac_alloc_or_panic` / `karac_alloc_fallible` (`alloc.rs`) take
/// `usize`, which is **i32 on wasm32**, but codegen passes i64 byte counts
/// at every Vec/String growth site; a direct i64 call traps
/// `signature_mismatch:karac_alloc_or_panic`. On wasm, codegen declares
/// THESE i64 shims instead (see `driver.rs::c_alloc_or_panic_symbol` /
/// `c_alloc_fallible_symbol`), which narrow the count (saturating to
/// `usize::MAX` so a >4 GiB request fails cleanly via the wrapper's own
/// null/OOM path) and call the real wrapper. Native targets keep calling
/// the `usize`-as-i64 wrappers directly — no shim, no twin needed.
#[no_mangle]
pub extern "C" fn __karac_alloc_or_panic64(size: u64) -> *mut u8 {
    let size = usize::try_from(size).unwrap_or(usize::MAX);
    crate::alloc::karac_alloc_or_panic(size)
}

#[no_mangle]
pub extern "C" fn __karac_alloc_fallible64(size: u64) -> *mut u8 {
    let size = usize::try_from(size).unwrap_or(usize::MAX);
    crate::alloc::karac_alloc_fallible(size)
}

/// 64-bit-size shim for the panicking reallocation wrapper — the grow-path
/// twin of [`__karac_alloc_or_panic64`], same B-2026-06-12-1 size_t-width
/// rationale: codegen passes an i64 byte count at every grow site, but
/// `karac_realloc_or_panic` takes `usize` (i32 on wasm32). `ptr` stays
/// pointer-width (i32 on wasm32, no narrowing); only the size is narrowed
/// (saturating so a >4 GiB request fails cleanly via the wrapper's OOM path).
#[no_mangle]
pub extern "C" fn __karac_realloc_or_panic64(ptr: *mut u8, size: u64) -> *mut u8 {
    let size = usize::try_from(size).unwrap_or(usize::MAX);
    crate::alloc::karac_realloc_or_panic(ptr, size)
}

/// Component Model **Canonical ABI** reallocation entry point (phase-10
/// "WASM entry-point discovery", rich-type exports). `wasm-tools
/// component new` wires the lifted component to call this exported
/// symbol whenever it must place lowered values (strings, lists, spilled
/// records) into *our* linear memory — both fresh allocations
/// (`old_ptr == 0`) and growth. Signature is the canonical one:
/// `cabi_realloc(old_ptr, old_size, align, new_size) -> ptr`. Backed by
/// the same unified wasi-libc heap as everything else in this archive,
/// so blocks it returns are `free`-compatible with Kāra-emitted IR.
///
/// `wasm-tools` only emits calls with `align` ≤ the natural alignment of
/// the lowered type; for the v1 export surface (records / `option` /
/// `result` / `string` / `list` over scalar and pointer-width fields)
/// that is ≤ `MALLOC_ALIGN` (16), so `realloc`/`malloc` (16-byte
/// guaranteed) satisfies it directly. Larger alignments route through
/// `aligned_alloc` on the fresh path; an over-aligned *grow* (which the
/// canonical ABI never emits for this surface) falls back to
/// alloc-and-copy. `new_size == 0` returns the `align` value as a
/// non-null aligned sentinel, per the canonical-ABI convention.
#[no_mangle]
pub extern "C" fn cabi_realloc(
    old_ptr: *mut c_void,
    old_size: usize,
    align: usize,
    new_size: usize,
) -> *mut c_void {
    if new_size == 0 {
        return align as *mut c_void;
    }
    unsafe {
        if old_ptr.is_null() {
            if align <= MALLOC_ALIGN {
                malloc(new_size) as *mut c_void
            } else {
                let size = new_size.next_multiple_of(align);
                aligned_alloc(align, size)
            }
        } else if align <= MALLOC_ALIGN {
            realloc(old_ptr as *mut u8, new_size) as *mut c_void
        } else {
            // Over-aligned grow: realloc can't preserve >malloc alignment,
            // so allocate fresh, copy the old bytes, free the old block.
            let size = new_size.next_multiple_of(align);
            let fresh = aligned_alloc(align, size);
            if !fresh.is_null() {
                std::ptr::copy_nonoverlapping(
                    old_ptr as *const u8,
                    fresh as *mut u8,
                    old_size.min(new_size),
                );
                free(old_ptr);
            }
            fresh
        }
    }
}
