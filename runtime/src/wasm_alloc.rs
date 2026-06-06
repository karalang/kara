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
    fn malloc(size: usize) -> *mut c_void;
    fn calloc(nmemb: usize, size: usize) -> *mut c_void;
    fn aligned_alloc(alignment: usize, size: usize) -> *mut c_void;
    fn free(ptr: *mut c_void);
}

/// `malloc`'s guaranteed alignment in wasi-libc (`max_align_t`).
const MALLOC_ALIGN: usize = 16;

struct LibcAlloc;

unsafe impl GlobalAlloc for LibcAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if layout.align() <= MALLOC_ALIGN {
            malloc(layout.size()) as *mut u8
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
    unsafe { malloc(size) }
}
