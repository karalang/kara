//! Type-erased write-once cell for compiled Kāra programs — the AOT-codegen
//! realization of the `OnceLock[T]` / `OnceCell[T]` surface the tree-walk
//! interpreter implements with a side-table `Option<Value>`
//! (`src/interpreter/method_call_once.rs`).
//!
//! Like `channel.rs` / `map.rs`, the payload is **type-erased**: the value
//! travels as a raw byte blob and `value_size` is passed per `set` call (not
//! stored at construction). The element type `T` is statically known at every
//! `set` / `get` site (the typed `OnceLock[T]` / `OnceCell[T]` receiver) but
//! NOT at `OnceLock.new()` (the associated-call dispatch sees only the type
//! name), so threading the size through `set` keeps `once_new` type-agnostic.
//!
//! **Write-once semantics.** `set` succeeds exactly once: the first caller
//! copies its `value_size` bytes into an owned heap buffer and the cell is
//! sealed; every later `set` fails (returns 0) without overwriting, so the
//! caller can recover its rejected value (the codegen `Err(AlreadySetError {
//! rejected })` arm). `get` returns a **stable borrow pointer** into that
//! buffer (the buffer is never moved or reallocated after `set`, so the
//! pointer is valid for the cell's lifetime) or null when unset — mirroring
//! `Map.get`'s alias-into-container shape.
//!
//! **Thread-safe (shared by both primitives).** `OnceLock[T]` is the
//! thread-safe primitive; `set` races between sibling tasks produce exactly
//! one winner and the rest observe the failure. `OnceCell[T]` is spec'd as
//! single-task-no-lock for speed, but the typechecker's cross-task rules
//! already guarantee an `OnceCell` never crosses a task boundary, so it can
//! ride this same synchronized primitive at v1 with no observable difference
//! (the `Mutex` is simply never contended for an `OnceCell`). One primitive,
//! one ABI — the codegen dispatch is identical for both receiver types.
//!
//! **Target-independent.** A write-once cell behind a lock has no scheduler
//! dependency, so this module is compiled unconditionally (like `channel.rs`)
//! and the `karac_runtime_once_*` externs are present in every archive.

use std::alloc::{alloc, dealloc, Layout};
use std::ptr;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Mutex;

/// The sealed value buffer, guarded by the cell's `Mutex`. `ptr`/`size`
/// describe an owned heap allocation of exactly `size` bytes; `None` until
/// the winning `set`. The pointer is never mutated after it is installed
/// (write-once), so `get` can hand it out as a stable borrow.
struct Inner {
    value: Option<(*mut u8, usize)>,
}

/// `#[repr(C)]` is not load-bearing for field access (codegen never GEPs into
/// this — it holds only the opaque `*mut KaracOnce` and passes it back through
/// the externs) but kept for a stable, inspectable layout.
#[repr(C)]
pub struct KaracOnce {
    /// Fast path for `is_set` / `get`'s presence test: 0 = empty, 1 = set.
    /// Set with `Release` under the lock after the buffer is installed; read
    /// with `Acquire` so a reader that observes 1 also observes the bytes.
    state: AtomicU8,
    inner: Mutex<Inner>,
}

// A `OnceLock` is shared across tasks (module-level bindings, `par struct`
// fields); the `Mutex` serializes `set` and the value is immutable after
// sealing, so this is sound.
unsafe impl Send for KaracOnce {}
unsafe impl Sync for KaracOnce {}

impl KaracOnce {
    fn new() -> *mut Self {
        let cell = Box::new(KaracOnce {
            state: AtomicU8::new(0),
            inner: Mutex::new(Inner { value: None }),
        });
        Box::into_raw(cell)
    }
}

/// `OnceLock.new()` / `OnceCell.new()` — allocate an empty write-once cell and
/// return its opaque handle. The handle is stored in the binding's slot (a
/// local alloca, or — for a module-level binding — an LLVM global initialized
/// in `__karac_static_init`).
#[no_mangle]
pub extern "C" fn karac_runtime_once_new() -> *mut KaracOnce {
    KaracOnce::new()
}

/// `cell.set(val)` — seal the cell with `value_size` bytes copied from `src`.
/// Returns 1 when THIS call won (the cell was empty and is now sealed), 0 when
/// the cell was already set (no copy; the caller keeps `val` for the
/// `AlreadySetError` arm). Thread-safe: the `Mutex` makes the empty-check +
/// install atomic, so concurrent `set`s produce exactly one winner.
///
/// # Safety
/// `handle` must be a live `*mut KaracOnce` from `karac_runtime_once_new`;
/// `src` must point at `value_size` readable bytes (a caller-owned stack slot).
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_once_set(
    handle: *mut KaracOnce,
    src: *const u8,
    value_size: i64,
) -> u8 {
    if handle.is_null() || src.is_null() || value_size < 0 {
        return 0;
    }
    let cell = &*handle;
    let mut inner = cell.inner.lock().unwrap();
    if inner.value.is_some() {
        return 0; // already set — reject, leave the stored value untouched.
    }
    let size = value_size as usize;
    let buf = if size == 0 {
        // Zero-sized `T` (e.g. `Unit`): no allocation, but still "set". Use a
        // dangling-but-nonnull marker so `get` returns non-null.
        ptr::NonNull::<u8>::dangling().as_ptr()
    } else {
        let layout = Layout::from_size_align(size, 8).expect("once value layout");
        let p = alloc(layout);
        if p.is_null() {
            std::alloc::handle_alloc_error(layout);
        }
        ptr::copy_nonoverlapping(src, p, size);
        p
    };
    inner.value = Some((buf, size));
    // Release: a reader that observes state==1 (Acquire) sees the installed
    // buffer bytes too.
    cell.state.store(1, Ordering::Release);
    1
}

/// `cell.get()` — return a stable borrow pointer into the sealed value, or
/// null when the cell is unset. Codegen wraps a non-null result as
/// `Some(ref T)` (loading `T` shallowly through the pointer) and null as
/// `None` — the `Map.get` alias-into-container shape.
///
/// # Safety
/// `handle` must be a live `*mut KaracOnce` from `karac_runtime_once_new`.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_once_get(handle: *mut KaracOnce) -> *mut u8 {
    if handle.is_null() {
        return ptr::null_mut();
    }
    let cell = &*handle;
    if cell.state.load(Ordering::Acquire) == 0 {
        return ptr::null_mut();
    }
    let inner = cell.inner.lock().unwrap();
    match inner.value {
        Some((p, _)) => p,
        None => ptr::null_mut(),
    }
}

/// `cell.is_set()` — advisory presence test. 1 when sealed, 0 when empty.
///
/// # Safety
/// `handle` must be a live `*mut KaracOnce` from `karac_runtime_once_new`.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_once_is_set(handle: *mut KaracOnce) -> u8 {
    if handle.is_null() {
        return 0;
    }
    (*handle).state.load(Ordering::Acquire)
}

/// Free a write-once cell and its sealed value buffer. Called by codegen's
/// scope-exit `FreeOnceHandle` cleanup for a local binding; a module-level
/// binding lives for the whole process and is never freed. Null-handle is a
/// no-op. The stored `T` may itself own heap (a `String`'s char buffer): that
/// is handled by the codegen drop path BEFORE this call for the heap-`T` slice
/// — at the scalar-`T` v1 floor there is no inner heap to reclaim, so freeing
/// the value buffer + control block is complete.
///
/// # Safety
/// `handle` must be null or a live `*mut KaracOnce` from
/// `karac_runtime_once_new`; consumes it (must not be used afterward).
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_once_free(handle: *mut KaracOnce) {
    if handle.is_null() {
        return;
    }
    let cell = Box::from_raw(handle);
    let inner = cell.inner.into_inner().unwrap();
    if let Some((p, size)) = inner.value {
        if size != 0 {
            let layout = Layout::from_size_align(size, 8).expect("once value layout");
            dealloc(p, layout);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_get_is_set_roundtrip() {
        unsafe {
            let cell = karac_runtime_once_new();
            assert_eq!(karac_runtime_once_is_set(cell), 0);
            assert!(karac_runtime_once_get(cell).is_null());

            let v: i64 = 42;
            let won = karac_runtime_once_set(cell, &v as *const i64 as *const u8, 8);
            assert_eq!(won, 1);
            assert_eq!(karac_runtime_once_is_set(cell), 1);

            let got = karac_runtime_once_get(cell);
            assert!(!got.is_null());
            assert_eq!(*(got as *const i64), 42);

            // Second set fails, leaves the stored value untouched.
            let v2: i64 = 99;
            let won2 = karac_runtime_once_set(cell, &v2 as *const i64 as *const u8, 8);
            assert_eq!(won2, 0);
            assert_eq!(*(karac_runtime_once_get(cell) as *const i64), 42);

            karac_runtime_once_free(cell);
        }
    }

    #[test]
    fn zero_sized_value_still_sets() {
        unsafe {
            let cell = karac_runtime_once_new();
            let won = karac_runtime_once_set(cell, ptr::NonNull::<u8>::dangling().as_ptr(), 0);
            assert_eq!(won, 1);
            assert_eq!(karac_runtime_once_is_set(cell), 1);
            assert!(!karac_runtime_once_get(cell).is_null());
            karac_runtime_once_free(cell);
        }
    }

    #[test]
    fn free_null_is_noop() {
        unsafe {
            karac_runtime_once_free(ptr::null_mut());
        }
    }
}
