//! Bump-allocation arena for compiled Kāra programs — the AOT-codegen
//! realization of the `Arena[T]` + `ArenaRef[T]` surface the tree-walk
//! interpreter implements with a side-table `HashMap<i64, Vec<Value>>`
//! (`src/interpreter/method_call_arena.rs`).
//!
//! Like `interner.rs`, the handle is opaque: codegen stores the
//! `*mut KaracArena` returned by `arena_new` directly in the binding's slot
//! and passes it back through the externs. Elements are raw byte blobs
//! (`ptr + len`) — the runtime is fully type-agnostic. Codegen owns the
//! element-type interpretation per monomorphized `Arena[T]` binding:
//! an `i64`/`f64`/`bool` element is its 8-byte value blob, a `String`
//! element is its byte content (copied on push, handed back as a borrowed
//! `cap = 0` view on get), and an all-POD struct element is its by-value
//! byte image (copied back out into a fresh local on get — matching the
//! interpreter, whose `get` clones the stored `Value`).
//!
//! **Stability.** `get(idx)` hands back a **stable borrow pointer**: blobs
//! are individually boxed (`Box<[u8]>`), so growing the index table never
//! moves the bytes, and an arena is append-only except for `rewind`, which
//! truncates — codegen's static checkpoint discipline (a checkpoint only
//! rewinds the arena that minted it; handles minted past the mark are v1
//! undefined-after-rewind per the tracker) matches the interpreter's.
//!
//! **Thread-safe.** All state sits behind a `Mutex` (the `interner.rs`
//! posture); the single-task common case never contends the lock.
//!
//! **Target-independent.** A blob table behind a lock has no scheduler
//! dependency, so this module is compiled unconditionally (like `once.rs`)
//! and the `karac_runtime_arena_*` externs are present in every archive.

use std::ptr;
use std::sync::Mutex;

pub struct KaracArena {
    items: Mutex<Vec<Box<[u8]>>>,
}

impl KaracArena {
    fn new() -> *mut Self {
        Box::into_raw(Box::new(KaracArena {
            items: Mutex::new(Vec::new()),
        }))
    }
}

/// `Arena.new()` — allocate an empty arena and return its opaque handle.
/// The handle is stored in the binding's slot; the local binding's
/// scope-exit `FreeArenaHandle` cleanup reclaims it.
#[no_mangle]
pub extern "C" fn karac_runtime_arena_new() -> *mut KaracArena {
    KaracArena::new()
}

/// `arena.push(value)` — copy `len` bytes into a fresh arena-owned blob and
/// return its index (the `ArenaRef[T]`, minted densely `0, 1, 2, …` like the
/// interpreter's `(handle, index)` pair with the handle erased). A
/// null/invalid handle degrades to `-1` (an index no `get` ever serves).
///
/// # Safety
/// `handle` must be null or a live `*mut KaracArena` from
/// `karac_runtime_arena_new`; `ptr` must point at `len` readable bytes when
/// `len > 0`.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_arena_push(
    handle: *mut KaracArena,
    ptr: *const u8,
    len: i64,
) -> i64 {
    if handle.is_null() || len < 0 || (ptr.is_null() && len > 0) {
        return -1;
    }
    let bytes: &[u8] = if len == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(ptr, len as usize)
    };
    let mut items = (*handle).items.lock().unwrap();
    let idx = items.len() as i64;
    items.push(bytes.into());
    idx
}

/// `arena.get(r)` — stable borrow pointer to the blob at `idx`, with the
/// byte length written through `out_len`. A foreign / out-of-range /
/// post-rewind index degrades to an empty blob (dangling-but-nonnull
/// pointer, `*out_len = 0`) rather than read out of bounds — the same
/// posture as the interpreter's `Unit` degrade.
///
/// # Safety
/// `handle` must be null or a live `*mut KaracArena` from
/// `karac_runtime_arena_new`; `out_len` must be a valid `*mut i64`.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_arena_get(
    handle: *mut KaracArena,
    idx: i64,
    out_len: *mut i64,
) -> *const u8 {
    if !out_len.is_null() {
        *out_len = 0;
    }
    if handle.is_null() || out_len.is_null() {
        return ptr::NonNull::<u8>::dangling().as_ptr();
    }
    let items = (*handle).items.lock().unwrap();
    match usize::try_from(idx).ok().and_then(|i| items.get(i)) {
        Some(bytes) => {
            *out_len = bytes.len() as i64;
            // Stable: the Box<[u8]> buffer never moves (the Vec growing
            // relocates the box headers, not the bytes) and entries live
            // until `arena_free` or a `rewind` past them.
            bytes.as_ptr()
        }
        None => ptr::NonNull::<u8>::dangling().as_ptr(),
    }
}

/// `arena.get(r)` for by-value element kinds (scalars, all-POD structs) —
/// copy `min(blob_len, dst_len)` bytes into `dst` and zero-fill the
/// remainder up to `dst_len`, so a foreign / out-of-range / post-rewind
/// index degrades to an all-zeroes value (`0` / `0.0` / `false` /
/// zeroed struct) instead of leaving `dst` uninitialized or reading out
/// of bounds. Returns the stored blob's length (`0` on degrade) — matching
/// the interpreter's clone-on-`get` semantics without handing out a
/// pointer the caller would have to guard before loading through.
///
/// # Safety
/// `handle` must be null or a live `*mut KaracArena` from
/// `karac_runtime_arena_new`; `dst` must point at `dst_len` writable bytes
/// when `dst_len > 0`.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_arena_get_copy(
    handle: *mut KaracArena,
    idx: i64,
    dst: *mut u8,
    dst_len: i64,
) -> i64 {
    if dst.is_null() || dst_len <= 0 {
        return 0;
    }
    let out = std::slice::from_raw_parts_mut(dst, dst_len as usize);
    out.fill(0);
    if handle.is_null() {
        return 0;
    }
    let items = (*handle).items.lock().unwrap();
    match usize::try_from(idx).ok().and_then(|i| items.get(i)) {
        Some(bytes) => {
            let n = bytes.len().min(out.len());
            out[..n].copy_from_slice(&bytes[..n]);
            bytes.len() as i64
        }
        None => 0,
    }
}

/// `arena.len()` (and `arena.high_water_mark()`, which is the same number) —
/// count of live items. Null handle degrades to 0.
///
/// # Safety
/// `handle` must be null or a live `*mut KaracArena` from
/// `karac_runtime_arena_new`.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_arena_len(handle: *mut KaracArena) -> i64 {
    if handle.is_null() {
        return 0;
    }
    let items = (*handle).items.lock().unwrap();
    items.len() as i64
}

/// `arena.rewind_to(cp)` — truncate to `mark` items, dropping every blob
/// pushed past the checkpoint (the runtime owns the copies, so truncation
/// drops are type-agnostic). `mark` is clamped to `[0, len]`: a negative
/// mark clears nothing below zero and a stale over-long mark (arena already
/// shorter) is a no-op — the interpreter's clamp semantics. The
/// foreign-checkpoint guard is enforced statically by codegen (a checkpoint
/// binding only rewinds the arena binding that minted it).
///
/// # Safety
/// `handle` must be null or a live `*mut KaracArena` from
/// `karac_runtime_arena_new`.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_arena_rewind(handle: *mut KaracArena, mark: i64) {
    if handle.is_null() {
        return;
    }
    let mut items = (*handle).items.lock().unwrap();
    let mark = mark.clamp(0, items.len() as i64) as usize;
    items.truncate(mark);
}

/// Free an arena and every stored blob. Called by codegen's scope-exit
/// `FreeArenaHandle` cleanup for a local binding. Null-handle is a no-op.
/// Any `get` borrow pointers are dead after this — codegen's borrow
/// discipline (a borrowed view never outlives the frame that owns the arena
/// binding) upholds that.
///
/// # Safety
/// `handle` must be null or a live `*mut KaracArena` from
/// `karac_runtime_arena_new`; consumes it (must not be used afterward).
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_arena_free(handle: *mut KaracArena) {
    if handle.is_null() {
        return;
    }
    drop(Box::from_raw(handle));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_get_roundtrip_and_len() {
        unsafe {
            let h = karac_runtime_arena_new();
            assert_eq!(karac_runtime_arena_len(h), 0);

            let a = karac_runtime_arena_push(h, 10i64.to_le_bytes().as_ptr(), 8);
            let b = karac_runtime_arena_push(h, 20i64.to_le_bytes().as_ptr(), 8);
            assert_eq!(a, 0);
            assert_eq!(b, 1);
            assert_eq!(karac_runtime_arena_len(h), 2);

            let mut len: i64 = -1;
            let p = karac_runtime_arena_get(h, a, &mut len);
            assert_eq!(len, 8);
            let mut buf = [0u8; 8];
            buf.copy_from_slice(std::slice::from_raw_parts(p, 8));
            assert_eq!(i64::from_le_bytes(buf), 10);

            karac_runtime_arena_free(h);
        }
    }

    #[test]
    fn get_pointers_stay_stable_across_growth() {
        unsafe {
            let h = karac_runtime_arena_new();
            let idx = karac_runtime_arena_push(h, b"pinned".as_ptr(), 6);
            let mut len: i64 = 0;
            let before = karac_runtime_arena_get(h, idx, &mut len);
            for i in 0..64u8 {
                let s = [b'x', i];
                karac_runtime_arena_push(h, s.as_ptr(), 2);
            }
            let after = karac_runtime_arena_get(h, idx, &mut len);
            assert_eq!(before, after);
            assert_eq!(std::slice::from_raw_parts(after, 6), b"pinned");
            karac_runtime_arena_free(h);
        }
    }

    #[test]
    fn rewind_truncates_and_clamps() {
        unsafe {
            let h = karac_runtime_arena_new();
            for v in [1i64, 2, 3, 4] {
                karac_runtime_arena_push(h, v.to_le_bytes().as_ptr(), 8);
            }
            karac_runtime_arena_rewind(h, 2);
            assert_eq!(karac_runtime_arena_len(h), 2);
            // Pre-mark item still readable.
            let mut len: i64 = 0;
            let p = karac_runtime_arena_get(h, 1, &mut len);
            assert_eq!(len, 8);
            let mut buf = [0u8; 8];
            buf.copy_from_slice(std::slice::from_raw_parts(p, 8));
            assert_eq!(i64::from_le_bytes(buf), 2);
            // Post-mark index degrades to empty.
            let q = karac_runtime_arena_get(h, 3, &mut len);
            assert!(!q.is_null());
            assert_eq!(len, 0);
            // Clamped: negative clears everything below zero → no panic;
            // over-long mark is a no-op.
            karac_runtime_arena_rewind(h, 99);
            assert_eq!(karac_runtime_arena_len(h), 2);
            karac_runtime_arena_rewind(h, -5);
            assert_eq!(karac_runtime_arena_len(h), 0);
            karac_runtime_arena_free(h);
        }
    }

    #[test]
    fn get_copy_roundtrips_and_zero_fills_on_degrade() {
        unsafe {
            let h = karac_runtime_arena_new();
            let idx = karac_runtime_arena_push(h, 42i64.to_le_bytes().as_ptr(), 8);
            let mut dst = [0xffu8; 8];
            let n = karac_runtime_arena_get_copy(h, idx, dst.as_mut_ptr(), 8);
            assert_eq!(n, 8);
            assert_eq!(i64::from_le_bytes(dst), 42);
            // Out-of-range: dst is zero-filled, returns 0.
            let mut dst2 = [0xffu8; 8];
            let n2 = karac_runtime_arena_get_copy(h, 9, dst2.as_mut_ptr(), 8);
            assert_eq!(n2, 0);
            assert_eq!(i64::from_le_bytes(dst2), 0);
            // Short blob into a longer dst: tail zero-filled.
            let sid = karac_runtime_arena_push(h, b"ab".as_ptr(), 2);
            let mut dst3 = [0xffu8; 4];
            let n3 = karac_runtime_arena_get_copy(h, sid, dst3.as_mut_ptr(), 4);
            assert_eq!(n3, 2);
            assert_eq!(&dst3, b"ab\0\0");
            karac_runtime_arena_free(h);
        }
    }

    #[test]
    fn out_of_range_get_degrades_to_empty() {
        unsafe {
            let h = karac_runtime_arena_new();
            let mut len: i64 = 99;
            let p = karac_runtime_arena_get(h, 7, &mut len);
            assert!(!p.is_null());
            assert_eq!(len, 0);
            let p2 = karac_runtime_arena_get(h, -3, &mut len);
            assert!(!p2.is_null());
            assert_eq!(len, 0);
            karac_runtime_arena_free(h);
        }
    }

    #[test]
    fn null_handle_ops_are_safe() {
        unsafe {
            assert_eq!(
                karac_runtime_arena_push(ptr::null_mut(), b"x".as_ptr(), 1),
                -1
            );
            let mut len: i64 = 3;
            let p = karac_runtime_arena_get(ptr::null_mut(), 0, &mut len);
            assert!(!p.is_null());
            assert_eq!(len, 0);
            assert_eq!(karac_runtime_arena_len(ptr::null_mut()), 0);
            karac_runtime_arena_rewind(ptr::null_mut(), 0);
            karac_runtime_arena_free(ptr::null_mut());
        }
    }
}
