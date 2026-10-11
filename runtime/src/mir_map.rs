//! The tables of the MIR backend (`src/codegen/mir/`): `Map`, `Set`,
//! `SortedMap` and `SortedSet`, as the interpreter keeps them
//! (`src/mir/interp.rs`): a sequence of entries in insertion order, or in
//! key order for the sorted kinds, so iteration, `entry_at` and the drop
//! of the whole table all walk the same order on both backends.
//!
//! An entry is `entry_size` bytes with the key at offset 0 (a map's entry
//! is laid out as the tuple `(K, V)`, so a `ref (K, V)` points into it).
//! Compiled code supplies the key glue on each call: `hash` and `eq` for
//! the hashed kinds, `cmp` for the sorted ones, which are searched by
//! bisection. A handle is a pointer to a [`MirMap`], null while the table
//! has never held an entry.

use std::collections::HashMap;

use crate::alloc::karac_realloc_or_panic;

pub type HashFn = unsafe extern "C" fn(*const u8) -> u64;
pub type EqFn = unsafe extern "C" fn(*const u8, *const u8) -> bool;
pub type CmpFn = unsafe extern "C" fn(*const u8, *const u8) -> i32;

pub struct MirMap {
    entry_size: usize,
    buf: *mut u8,
    len: usize,
    cap: usize,
    /// Each entry's hash, for the hashed kinds.
    hashes: Vec<u64>,
    /// The entries of each hash, by index.
    index: HashMap<u64, Vec<usize>>,
}

impl MirMap {
    fn at(&self, i: usize) -> *mut u8 {
        // SAFETY: `i < len` at every caller.
        unsafe { self.buf.add(i * self.entry_size) }
    }

    fn reindex(&mut self) {
        self.index.clear();
        for (i, h) in self.hashes.iter().enumerate() {
            self.index.entry(*h).or_default().push(i);
        }
    }

    fn grow(&mut self) {
        if self.len < self.cap {
            return;
        }
        let cap = (self.cap * 2).max(4);
        let old = if self.cap == 0 {
            std::ptr::null_mut()
        } else {
            self.buf
        };
        self.buf = karac_realloc_or_panic(old, cap * self.entry_size.max(1));
        self.cap = cap;
    }
}

/// The index of the entry whose key equals `key`, or -1. `cmp` (non-null)
/// searches a sorted table; otherwise `hash` and `eq` search a hashed one.
///
/// # Safety
/// `h` is null or a live table; `key` points at a key of its type; the
/// glue is the key type's.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_map_find(
    h: *const MirMap,
    key: *const u8,
    hash: Option<HashFn>,
    eq: Option<EqFn>,
    cmp: Option<CmpFn>,
) -> i64 {
    if h.is_null() {
        return -1;
    }
    // SAFETY: the caller's contract.
    let m = unsafe { &*h };
    if let Some(cmp) = cmp {
        // SAFETY: as above.
        return match unsafe { search(m, key, cmp) } {
            Ok(i) => i as i64,
            Err(_) => -1,
        };
    }
    let (Some(hash), Some(eq)) = (hash, eq) else {
        return -1;
    };
    // SAFETY: as above.
    let hv = unsafe { hash(key) };
    if let Some(is) = m.index.get(&hv) {
        for &i in is {
            // SAFETY: as above.
            if unsafe { eq(m.at(i), key) } {
                return i as i64;
            }
        }
    }
    -1
}

/// # Safety
/// `m` is live; `key` a key of its type; `cmp` its order.
unsafe fn search(m: &MirMap, key: *const u8, cmp: CmpFn) -> Result<usize, usize> {
    let (mut lo, mut hi) = (0, m.len);
    while lo < hi {
        let mid = (lo + hi) / 2;
        // SAFETY: the caller's contract.
        match unsafe { cmp(m.at(mid), key) } {
            0 => return Ok(mid),
            o if o < 0 => lo = mid + 1,
            _ => hi = mid,
        }
    }
    Err(lo)
}

/// Adds the `entry_size` bytes at `entry` (a key not in the table) to the
/// table at `*hp`, creating it when null: at its key's place for a sorted
/// table (`cmp` non-null), last for a hashed one. Returns its index.
///
/// # Safety
/// `hp` is writable and holds null or a live table of `entry_size`-byte
/// entries; `entry` is readable for that many bytes; the glue is the key
/// type's.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_map_insert(
    hp: *mut *mut MirMap,
    entry_size: usize,
    entry: *const u8,
    hash: Option<HashFn>,
    cmp: Option<CmpFn>,
) -> i64 {
    // SAFETY: the caller's contract.
    let m = unsafe {
        if (*hp).is_null() {
            *hp = Box::into_raw(Box::new(MirMap {
                entry_size,
                buf: std::ptr::null_mut(),
                len: 0,
                cap: 0,
                hashes: Vec::new(),
                index: HashMap::new(),
            }));
        }
        &mut **hp
    };
    m.grow();
    let es = m.entry_size;
    let at = match cmp {
        // SAFETY: as above.
        Some(cmp) => unsafe { search(m, entry, cmp) }.unwrap_or_else(|i| i),
        None => m.len,
    };
    // SAFETY: `grow` made room for one more entry after `len`.
    unsafe {
        if at < m.len {
            std::ptr::copy(m.at(at), m.at(at + 1), (m.len - at) * es);
        }
        std::ptr::copy_nonoverlapping(entry, m.at(at), es);
    }
    m.len += 1;
    if let Some(hash) = hash {
        // SAFETY: as above.
        let hv = unsafe { hash(entry) };
        m.hashes.push(hv);
        m.index.entry(hv).or_default().push(at);
    }
    at as i64
}

/// The entry at index `i` (in bounds).
///
/// # Safety
/// `h` is a live table with more than `i` entries.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_map_at(h: *const MirMap, i: i64) -> *mut u8 {
    // SAFETY: the caller's contract.
    unsafe { (*h).at(i as usize) }
}

/// The number of entries (0 for a null handle).
///
/// # Safety
/// `h` is null or a live table.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_map_len(h: *const MirMap) -> i64 {
    if h.is_null() {
        0
    } else {
        // SAFETY: the caller's contract.
        unsafe { (*h).len as i64 }
    }
}

/// Moves the entry at index `i` to `out` and closes the gap, keeping the
/// order of the rest.
///
/// # Safety
/// `h` is a live table with more than `i` entries; `out` is writable for
/// an entry.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_map_remove(h: *mut MirMap, i: i64, out: *mut u8) {
    // SAFETY: the caller's contract.
    let m = unsafe { &mut *h };
    let (i, es) = (i as usize, m.entry_size);
    // SAFETY: as above.
    unsafe {
        std::ptr::copy_nonoverlapping(m.at(i), out, es);
        if i + 1 < m.len {
            std::ptr::copy(m.at(i + 1), m.at(i), (m.len - i - 1) * es);
        }
    }
    m.len -= 1;
    if !m.hashes.is_empty() {
        m.hashes.remove(i);
        m.reindex();
    }
}

/// Forgets every entry (their drops are the caller's) and keeps the table.
///
/// # Safety
/// `h` is null or a live table.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_map_clear(h: *mut MirMap) {
    if h.is_null() {
        return;
    }
    // SAFETY: the caller's contract.
    let m = unsafe { &mut *h };
    m.len = 0;
    m.hashes.clear();
    m.index.clear();
}

/// Frees the table (its entries' drops are the caller's, first).
///
/// # Safety
/// `h` is null or a live table, not used after.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_map_free(h: *mut MirMap) {
    if h.is_null() {
        return;
    }
    // SAFETY: the caller's contract; the table came from `Box::into_raw`.
    let m = unsafe { Box::from_raw(h) };
    if m.cap != 0 {
        // SAFETY: `buf` came from `karac_realloc_or_panic`.
        unsafe { free(m.buf.cast()) };
    }
}

extern "C" {
    fn free(ptr: *mut core::ffi::c_void);
}
