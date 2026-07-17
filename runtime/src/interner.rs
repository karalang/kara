//! String interner for compiled Kāra programs — the AOT-codegen realization
//! of the `Symbol` + `Interner` surface the tree-walk interpreter implements
//! with a side-table `(Vec<String>, HashMap<String, i64>)`
//! (`src/interpreter/method_call_interner.rs`).
//!
//! Like `once.rs` / `channel.rs`, the handle is opaque: codegen stores the
//! `*mut KaracInterner` returned by `interner_new` directly in the binding's
//! slot and passes it back through the externs. Payloads are raw byte
//! strings (`ptr + len`), matching the Kāra `String` `{ptr, len, cap}`
//! header's first two fields — the runtime never assumes an encoding.
//!
//! **Dedup semantics.** `intern(ptr, len)` returns the existing id when the
//! byte string was interned before (no new storage), otherwise copies the
//! bytes into an owned buffer and mints the next sequential id
//! (`0, 1, 2, …` — mirroring the interpreter, whose fresh id is
//! `strings.len()`). `resolve(id)` hands back a **stable borrow pointer**
//! into the stored buffer: buffers are individually boxed (`Box<[u8]>`), so
//! growing the id→string table never moves the bytes, and an interner is
//! append-only until `interner_free` reclaims everything at once. Codegen
//! wraps the `(ptr, len)` pair as a `cap = 0` (non-owned) `String`, so the
//! borrow is never freed by the caller.
//!
//! **Thread-safe.** All state sits behind a `Mutex`, so an `Interner` shared
//! across tasks serializes cleanly (the single-task common case simply never
//! contends the lock — the `OnceCell`-rides-`OnceLock` posture from
//! `once.rs`).
//!
//! **Target-independent.** A byte-string table behind a lock has no
//! scheduler dependency, so this module is compiled unconditionally (like
//! `once.rs`) and the `karac_runtime_interner_*` externs are present in
//! every archive.

use std::collections::HashMap;
use std::ptr;
use std::sync::Mutex;

/// The interned strings + dedup index, guarded by the interner's `Mutex`.
/// `strings` is the id→bytes lookup (ids are dense, minted sequentially);
/// `index` is the bytes→id dedup map. Each entry's bytes are stored twice
/// (Vec side + map key) — the same duplication the interpreter accepts; a
/// shared-buffer scheme is a size optimization this floor slice skips.
struct Inner {
    strings: Vec<Box<[u8]>>,
    index: HashMap<Box<[u8]>, i64>,
}

pub struct KaracInterner {
    inner: Mutex<Inner>,
}

impl KaracInterner {
    fn new() -> *mut Self {
        let interner = Box::new(KaracInterner {
            inner: Mutex::new(Inner {
                strings: Vec::new(),
                index: HashMap::new(),
            }),
        });
        Box::into_raw(interner)
    }
}

/// `Interner.new()` — allocate an empty interner and return its opaque
/// handle. The handle is stored in the binding's slot; the local binding's
/// scope-exit `FreeInternerHandle` cleanup reclaims it.
#[no_mangle]
pub extern "C" fn karac_runtime_interner_new() -> *mut KaracInterner {
    KaracInterner::new()
}

/// `interner.intern(s)` — return `s`'s existing id if the byte string was
/// interned before, else copy the bytes and mint the next sequential id.
/// A null/invalid handle degrades to `-1` (an id no `resolve` ever serves —
/// it reads back as the empty string), mirroring the interpreter's
/// foreign-handle short-circuit rather than corrupting memory.
///
/// # Safety
/// `handle` must be null or a live `*mut KaracInterner` from
/// `karac_runtime_interner_new`; `ptr` must point at `len` readable bytes
/// when `len > 0`.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_interner_intern(
    handle: *mut KaracInterner,
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
    let mut inner = (*handle).inner.lock().unwrap();
    if let Some(existing) = inner.index.get(bytes) {
        return *existing;
    }
    let fresh = inner.strings.len() as i64;
    let stored: Box<[u8]> = bytes.into();
    inner.strings.push(stored.clone());
    inner.index.insert(stored, fresh);
    fresh
}

/// `interner.resolve(sym)` — stable borrow pointer to the interned bytes at
/// `id`, with the byte length written through `out_len`. A foreign /
/// out-of-range id degrades to the empty string (dangling-but-nonnull
/// pointer, `*out_len = 0`) rather than read out of bounds — the same
/// posture as the interpreter.
///
/// # Safety
/// `handle` must be null or a live `*mut KaracInterner` from
/// `karac_runtime_interner_new`; `out_len` must be a valid `*mut i64`.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_interner_resolve(
    handle: *mut KaracInterner,
    id: i64,
    out_len: *mut i64,
) -> *const u8 {
    if !out_len.is_null() {
        *out_len = 0;
    }
    if handle.is_null() || out_len.is_null() {
        return ptr::NonNull::<u8>::dangling().as_ptr();
    }
    let inner = (*handle).inner.lock().unwrap();
    match usize::try_from(id).ok().and_then(|i| inner.strings.get(i)) {
        Some(bytes) => {
            *out_len = bytes.len() as i64;
            // Stable: the Box<[u8]> buffer never moves (the Vec growing
            // relocates the box headers, not the bytes) and entries live
            // until `interner_free`.
            bytes.as_ptr()
        }
        None => ptr::NonNull::<u8>::dangling().as_ptr(),
    }
}

/// `interner.len()` — number of distinct strings interned so far (= the next
/// id to be minted). Null handle degrades to 0.
///
/// # Safety
/// `handle` must be null or a live `*mut KaracInterner` from
/// `karac_runtime_interner_new`.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_interner_len(handle: *mut KaracInterner) -> i64 {
    if handle.is_null() {
        return 0;
    }
    let inner = (*handle).inner.lock().unwrap();
    inner.strings.len() as i64
}

/// Free an interner and every stored byte string. Called by codegen's
/// scope-exit `FreeInternerHandle` cleanup for a local binding. Null-handle
/// is a no-op. Any `resolve` borrow pointers are dead after this — codegen's
/// borrow discipline (a `cap = 0` `String` view is never freed and never
/// outlives the frame that owns the interner binding) upholds that.
///
/// # Safety
/// `handle` must be null or a live `*mut KaracInterner` from
/// `karac_runtime_interner_new`; consumes it (must not be used afterward).
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_interner_free(handle: *mut KaracInterner) {
    if handle.is_null() {
        return;
    }
    drop(Box::from_raw(handle));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intern_dedups_and_resolve_roundtrips() {
        unsafe {
            let h = karac_runtime_interner_new();
            assert_eq!(karac_runtime_interner_len(h), 0);

            let a = karac_runtime_interner_intern(h, b"alpha".as_ptr(), 5);
            let b = karac_runtime_interner_intern(h, b"beta".as_ptr(), 4);
            let a2 = karac_runtime_interner_intern(h, b"alpha".as_ptr(), 5);
            assert_eq!(a, 0);
            assert_eq!(b, 1);
            assert_eq!(a2, a);
            assert_eq!(karac_runtime_interner_len(h), 2);

            let mut len: i64 = -1;
            let p = karac_runtime_interner_resolve(h, a, &mut len);
            assert_eq!(len, 5);
            assert_eq!(std::slice::from_raw_parts(p, 5), b"alpha");

            karac_runtime_interner_free(h);
        }
    }

    #[test]
    fn resolve_pointers_stay_stable_across_growth() {
        unsafe {
            let h = karac_runtime_interner_new();
            let id = karac_runtime_interner_intern(h, b"pinned".as_ptr(), 6);
            let mut len: i64 = 0;
            let before = karac_runtime_interner_resolve(h, id, &mut len);
            // Force several rounds of Vec growth.
            for i in 0..64u8 {
                let s = [b'x', i];
                karac_runtime_interner_intern(h, s.as_ptr(), 2);
            }
            let after = karac_runtime_interner_resolve(h, id, &mut len);
            assert_eq!(before, after);
            assert_eq!(std::slice::from_raw_parts(after, 6), b"pinned");
            karac_runtime_interner_free(h);
        }
    }

    #[test]
    fn out_of_range_resolve_degrades_to_empty() {
        unsafe {
            let h = karac_runtime_interner_new();
            let mut len: i64 = 99;
            let p = karac_runtime_interner_resolve(h, 7, &mut len);
            assert!(!p.is_null());
            assert_eq!(len, 0);
            let p2 = karac_runtime_interner_resolve(h, -3, &mut len);
            assert!(!p2.is_null());
            assert_eq!(len, 0);
            karac_runtime_interner_free(h);
        }
    }

    #[test]
    fn empty_string_interns_and_dedups() {
        unsafe {
            let h = karac_runtime_interner_new();
            let a = karac_runtime_interner_intern(h, ptr::null(), 0);
            let b = karac_runtime_interner_intern(h, ptr::null(), 0);
            assert_eq!(a, 0);
            assert_eq!(b, 0);
            let mut len: i64 = 5;
            let p = karac_runtime_interner_resolve(h, a, &mut len);
            assert!(!p.is_null());
            assert_eq!(len, 0);
            karac_runtime_interner_free(h);
        }
    }

    #[test]
    fn null_handle_ops_are_safe() {
        unsafe {
            assert_eq!(
                karac_runtime_interner_intern(ptr::null_mut(), b"x".as_ptr(), 1),
                -1
            );
            let mut len: i64 = 3;
            let p = karac_runtime_interner_resolve(ptr::null_mut(), 0, &mut len);
            assert!(!p.is_null());
            assert_eq!(len, 0);
            assert_eq!(karac_runtime_interner_len(ptr::null_mut()), 0);
            karac_runtime_interner_free(ptr::null_mut());
        }
    }
}
