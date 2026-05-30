//! `__emutls_get_address` — emulated-TLS dispatch implementation
//! for LLJIT consumers.
//!
//! **Why this lives here.** karac codegen lowers `#[thread_local] let
//! mut X: T = init;` to an LLVM `thread_local global`. The AOT path
//! resolves these via native Mach-O `tlv_*` descriptors on macOS arm64
//! (the linker provides everything). The LLJIT path, even with
//! `LocalExec` TLS model annotations in the IR, falls through LLVM's
//! `EmulatedTLS` lowering and emits `__emutls_get_address` calls — a
//! libcompiler-rt builtin that is **not** present in the karac
//! process. JIT lookup fails with
//! `Symbols not found: [___emutls_get_address]` and `lookup_address("main")`
//! returns an error before the program ever runs.
//!
//! v1 testing fix: provide our own `__emutls_get_address` using Rust's
//! `thread_local!` storage. Indices are lazily assigned to control
//! blocks on first access (process-wide atomic counter); per-thread
//! storage is a `HashMap<idx, Box<[u8]>>`. The semantics match what
//! Kāra's `#[thread_local]` is supposed to mean: each task / OS thread
//! gets its own copy of the value, initialized from the control block's
//! initializer pointer (or zeroed if null).
//!
//! **Limits / known good cases.** Correct under single-task /
//! main-thread usage; correct under par-blocks because each worker is
//! a distinct OS thread with its own thread-local store; correct
//! across re-entry. Not optimal — every read/write goes through a
//! HashMap probe. The right long-term fix is teaching LLJIT to use
//! native Mach-O TLS instead of emutls (LLVM TargetMachine option), at
//! which point this whole module collapses to dead code that DCE
//! removes from the helper binary.
//!
//! ABI: matches compiler-rt's `__emutls_get_address` (the symbol
//! signature in LLVM-emitted code). Single argument is a pointer to a
//! `struct __emutls_control` with the layout below.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

/// `struct __emutls_control` per compiler-rt's emutls.h. The `object`
/// field is a union of `uintptr_t index` (before first init — 0) and
/// `void *address` (after, on platforms that resolve to a single
/// process-wide address). For our use we only consume the `index` arm
/// — Rust's `thread_local!` gives us per-thread tables keyed by index.
///
/// The `value` field is a pointer to the initializer or null; size
/// bytes are copied from `value` into a fresh per-thread allocation on
/// first access for that (thread, index) pair.
#[repr(C)]
pub struct EmutlsControl {
    pub size: usize,
    pub align: usize,
    pub object: usize, // `union { uintptr_t index; void *address; }` — we use as index
    pub value: *const u8,
}

static NEXT_IDX: AtomicUsize = AtomicUsize::new(1);

thread_local! {
    static EMUTLS_DATA: RefCell<HashMap<usize, Box<[u8]>>> = RefCell::new(HashMap::new());
}

/// Assign the control block a process-wide index on first call. We
/// race-tolerant via fetch_add; if two threads see `object == 0` at the
/// same moment they'll each grab their own index and one will win the
/// store. The loser's index leaks (no thread will ever use it) but
/// that's a 1-time-per-control-block constant.
fn ensure_idx(c: &mut EmutlsControl) -> usize {
    if c.object == 0 {
        let new_idx = NEXT_IDX.fetch_add(1, Ordering::SeqCst);
        // Tolerate the race: if another thread won, use theirs.
        if c.object == 0 {
            c.object = new_idx;
        }
    }
    c.object
}

/// Resolve the per-thread storage for a TLS-controlled variable. The
/// pointer returned is stable for the lifetime of the thread.
///
/// # Safety
/// `ctrl` must point to a valid `EmutlsControl` whose `size` accurately
/// describes the TLS object's storage requirement, and whose `value`
/// either is null (zero-init) or points to `size` bytes of initializer
/// data. LLVM's emutls lowering guarantees both invariants.
#[no_mangle]
pub unsafe extern "C" fn __emutls_get_address(ctrl: *mut EmutlsControl) -> *mut u8 {
    let c = unsafe { &mut *ctrl };
    let idx = ensure_idx(c);
    let size = c.size;
    let init_ptr = c.value;
    EMUTLS_DATA.with(|t| {
        let mut t = t.borrow_mut();
        let bytes = t.entry(idx).or_insert_with(|| {
            let mut v = vec![0u8; size];
            if !init_ptr.is_null() {
                unsafe {
                    std::ptr::copy_nonoverlapping(init_ptr, v.as_mut_ptr(), size);
                }
            }
            v.into_boxed_slice()
        });
        bytes.as_mut_ptr()
    })
}
