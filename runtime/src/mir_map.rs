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
    /// The slots in use, removed ones included: an index is a live
    /// entry's place among the live entries, and a slot its place in
    /// `buf`. Removing from a hashed table leaves a hole rather than
    /// shifting what follows (B-2026-10-12-4); a sorted table never has
    /// one, as bisection needs its entries contiguous.
    slots: usize,
    cap: usize,
    /// Each slot's hash, for the hashed kinds.
    hashes: Vec<u64>,
    /// The slots of each hash.
    index: HashMap<u64, Vec<usize>>,
    /// Which slots are holes, and how many.
    dead: Vec<bool>,
    holes: usize,
    /// A Fenwick tree over `cap` slots counting the live ones, kept only
    /// while there are holes: index to slot and back in `O(log n)`.
    live: Vec<i64>,
}

impl MirMap {
    fn len(&self) -> usize {
        self.slots - self.holes
    }

    fn slot_ptr(&self, s: usize) -> *mut u8 {
        // SAFETY: `s < slots` at every caller.
        unsafe { self.buf.add(s * self.entry_size) }
    }

    /// The entry at index `i < len`.
    fn at(&self, i: usize) -> *mut u8 {
        self.slot_ptr(self.slot_of(i))
    }

    /// The slot of the live entry at index `i`.
    fn slot_of(&self, i: usize) -> usize {
        if self.holes == 0 {
            return i;
        }
        // The least slot with `i + 1` live slots up to it.
        let n = self.live.len();
        let (mut pos, mut rem) = (0usize, i as i64 + 1);
        let mut step = n.next_power_of_two();
        while step > 0 {
            let next = pos + step;
            if next <= n && self.live[next - 1] < rem {
                pos = next;
                rem -= self.live[next - 1];
            }
            step /= 2;
        }
        pos
    }

    /// The index of the live entry in slot `s`.
    fn index_of(&self, s: usize) -> usize {
        if self.holes == 0 {
            return s;
        }
        // The live slots before `s`.
        let (mut k, mut n) = (s, 0i64);
        while k > 0 {
            n += self.live[k - 1];
            k &= k - 1;
        }
        n as usize
    }

    fn add_live(&mut self, s: usize, d: i64) {
        let mut k = s + 1;
        while k <= self.live.len() {
            self.live[k - 1] += d;
            k += k & k.wrapping_neg();
        }
    }

    /// The Fenwick tree over the current slots, built in `O(cap)`.
    fn rebuild_live(&mut self) {
        self.live.clear();
        self.live.resize(self.cap, 0);
        for s in 0..self.slots {
            if !self.dead[s] {
                self.live[s] += 1;
            }
        }
        for k in 1..=self.cap {
            let up = k + (k & k.wrapping_neg());
            if up <= self.cap {
                self.live[up - 1] += self.live[k - 1];
            }
        }
    }

    /// Closes every hole, keeping the order of the live entries.
    fn compact(&mut self) {
        let es = self.entry_size;
        let mut w = 0;
        for r in 0..self.slots {
            if self.dead[r] {
                continue;
            }
            if w != r {
                // SAFETY: both slots are below `slots`; `w < r`.
                unsafe { std::ptr::copy(self.slot_ptr(r), self.slot_ptr(w), es) };
                self.hashes[w] = self.hashes[r];
            }
            w += 1;
        }
        self.slots = w;
        self.holes = 0;
        self.hashes.truncate(w);
        self.dead.clear();
        self.dead.resize(w, false);
        self.live.clear();
        self.index.clear();
        for (s, h) in self.hashes.iter().enumerate() {
            self.index.entry(*h).or_default().push(s);
        }
    }

    fn grow(&mut self) {
        if self.slots < self.cap {
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
        if self.holes > 0 {
            self.rebuild_live();
        }
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
        for &sl in is {
            // SAFETY: as above.
            if unsafe { eq(m.slot_ptr(sl), key) } {
                return m.index_of(sl) as i64;
            }
        }
    }
    -1
}

/// # Safety
/// `m` is live; `key` a key of its type; `cmp` its order.
unsafe fn search(m: &MirMap, key: *const u8, cmp: CmpFn) -> Result<usize, usize> {
    let (mut lo, mut hi) = (0, m.len());
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
                slots: 0,
                cap: 0,
                hashes: Vec::new(),
                index: HashMap::new(),
                dead: Vec::new(),
                holes: 0,
                live: Vec::new(),
            }));
        }
        &mut **hp
    };
    m.grow();
    let es = m.entry_size;
    let at = match cmp {
        // SAFETY: as above.
        Some(cmp) => unsafe { search(m, entry, cmp) }.unwrap_or_else(|i| i),
        None => m.slots,
    };
    // SAFETY: `grow` made room for one more slot after `slots`; a sorted
    // table has no holes, so its indices are its slots.
    unsafe {
        if at < m.slots {
            std::ptr::copy(m.slot_ptr(at), m.slot_ptr(at + 1), (m.slots - at) * es);
        }
        std::ptr::copy_nonoverlapping(entry, m.slot_ptr(at), es);
    }
    m.slots += 1;
    m.dead.push(false);
    if m.holes > 0 {
        m.add_live(at, 1);
    }
    if let Some(hash) = hash {
        // SAFETY: as above.
        let hv = unsafe { hash(entry) };
        m.hashes.push(hv);
        m.index.entry(hv).or_default().push(at);
    }
    m.index_of(at) as i64
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
        unsafe { (*h).len() as i64 }
    }
}

/// An index into the table at `h` picked by `op` under the key order
/// `cmp`: 0 the least entry, 1 the greatest, 2 the greatest at most `key`
/// (floor), 3 the least at least `key` (ceiling), or -1 when there is
/// none; 4 and 5 (a sorted table only) the number of entries less than,
/// or at most, `key`, the bounds of a range. A sorted table is bisected,
/// a hashed one scanned.
///
/// # Safety
/// `h` is null or a live table; `key` points at a key of its type when
/// `op` is 2 or more; `cmp` is the key type's order.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_map_pick(
    h: *const MirMap,
    key: *const u8,
    cmp: CmpFn,
    sorted: bool,
    op: i64,
) -> i64 {
    if h.is_null() {
        return if op >= 4 { 0 } else { -1 };
    }
    // SAFETY: the caller's contract.
    let m = unsafe { &*h };
    let n = m.len();
    if sorted {
        // SAFETY: as above.
        let at = |k| unsafe { search(m, k, cmp) };
        return match op {
            0 if n > 0 => 0,
            1 if n > 0 => n as i64 - 1,
            2 => match at(key) {
                Ok(i) => i as i64,
                Err(i) => i as i64 - 1,
            },
            3 => match at(key) {
                Ok(i) => i as i64,
                Err(i) if i < n => i as i64,
                Err(_) => -1,
            },
            4 => at(key).unwrap_or_else(|i| i) as i64,
            5 => match at(key) {
                Ok(i) => i as i64 + 1,
                Err(i) => i as i64,
            },
            _ => -1,
        };
    }
    let mut best: Option<usize> = None;
    for i in 0..n {
        // SAFETY: as above.
        let c = |a: *const u8, b: *const u8| unsafe { cmp(a, b) };
        let ok = match op {
            2 => c(m.at(i), key) <= 0,
            3 => c(m.at(i), key) >= 0,
            _ => true,
        };
        if !ok {
            continue;
        }
        best = match best {
            None => Some(i),
            Some(b) => {
                let o = c(m.at(i), m.at(b));
                let greater = matches!(op, 1 | 2);
                if (greater && o > 0) || (!greater && o < 0) {
                    Some(i)
                } else {
                    Some(b)
                }
            }
        };
    }
    best.map_or(-1, |i| i as i64)
}

/// Moves the entry at index `i` to `out`, keeping the order of the rest:
/// a sorted table closes the gap, a hashed one leaves a hole, closing
/// them all once they outnumber the live entries.
///
/// # Safety
/// `h` is a live table with more than `i` entries; `out` is writable for
/// an entry.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_map_remove(h: *mut MirMap, i: i64, out: *mut u8) {
    // SAFETY: the caller's contract.
    let m = unsafe { &mut *h };
    let es = m.entry_size;
    let s = m.slot_of(i as usize);
    // SAFETY: as above.
    unsafe { std::ptr::copy_nonoverlapping(m.slot_ptr(s), out, es) };
    if m.hashes.is_empty() {
        // SAFETY: as above; a sorted table has no holes.
        unsafe {
            if s + 1 < m.slots {
                std::ptr::copy(m.slot_ptr(s + 1), m.slot_ptr(s), (m.slots - s - 1) * es);
            }
        }
        m.slots -= 1;
        m.dead.pop();
        return;
    }
    let hv = m.hashes[s];
    if let Some(v) = m.index.get_mut(&hv) {
        v.retain(|&x| x != s);
        if v.is_empty() {
            m.index.remove(&hv);
        }
    }
    if s + 1 == m.slots && m.holes == 0 {
        m.slots -= 1;
        m.hashes.pop();
        m.dead.pop();
        return;
    }
    if m.holes == 0 {
        m.dead[s] = true;
        m.holes = 1;
        m.rebuild_live();
    } else {
        m.dead[s] = true;
        m.holes += 1;
        m.add_live(s, -1);
    }
    if m.holes > m.len() {
        m.compact();
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
    m.slots = 0;
    m.hashes.clear();
    m.index.clear();
    m.dead.clear();
    m.holes = 0;
    m.live.clear();
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

#[cfg(test)]
mod tests {
    use super::*;

    unsafe extern "C" fn hash(k: *const u8) -> u64 {
        // SAFETY: the keys are `i64`s.
        unsafe { (k as *const i64).read_unaligned() as u64 % 7 }
    }
    unsafe extern "C" fn eq(a: *const u8, b: *const u8) -> bool {
        // SAFETY: as above.
        unsafe { (a as *const i64).read_unaligned() == (b as *const i64).read_unaligned() }
    }

    /// Removing from a hashed table keeps indices dense and in insertion
    /// order, against a `Vec` model, through holes and compactions.
    #[test]
    fn remove_keeps_insertion_order() {
        let mut h: *mut MirMap = std::ptr::null_mut();
        let mut model: Vec<i64> = Vec::new();
        let mut next = 0i64;
        let mut rng = 12345u64;
        let (mut holed, mut compacted) = (false, false);
        for step in 0..4000 {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            let len = model.len();
            if len == 0 || rng % 5 < 2 || (step > 2500 && rng % 5 < 1) {
                let k = next;
                next += 1;
                // SAFETY: a live (or null) table of `i64` entries.
                let i = unsafe {
                    karac_mir_map_insert(&mut h, 8, (&k as *const i64).cast(), Some(hash), None)
                };
                assert_eq!(i as usize, model.len());
                model.push(k);
            } else {
                let i = (rng / 5) as usize % len;
                let mut out = 0i64;
                // SAFETY: as above.
                unsafe {
                    let k = model[i];
                    let f = karac_mir_map_find(
                        h,
                        (&k as *const i64).cast(),
                        Some(hash),
                        Some(eq),
                        None,
                    );
                    assert_eq!(f as usize, i);
                    karac_mir_map_remove(h, i as i64, (&mut out as *mut i64).cast());
                }
                assert_eq!(out, model.remove(i));
                // SAFETY: as above.
                let holes = unsafe { (*h).holes };
                compacted |= holed && holes == 0;
                holed |= holes > 1;
            }
            // SAFETY: as above.
            unsafe {
                assert_eq!(karac_mir_map_len(h) as usize, model.len());
                for (i, k) in model.iter().enumerate() {
                    assert_eq!(
                        (karac_mir_map_at(h, i as i64) as *const i64).read_unaligned(),
                        *k
                    );
                }
            }
        }
        assert!(
            holed && compacted,
            "the walk never left holes or closed them"
        );
        // SAFETY: as above.
        unsafe { karac_mir_map_free(h) };
    }
}
