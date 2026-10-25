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

use crate::alloc::karac_realloc_or_panic;

pub type HashFn = unsafe extern "C" fn(*const u8) -> u64;
pub type EqFn = unsafe extern "C" fn(*const u8, *const u8) -> bool;
pub type CmpFn = unsafe extern "C" fn(*const u8, *const u8) -> i32;

/// The first nine fields are read by compiled code, which probes a table
/// of scalar keys and counts and walks its entries in line
/// (`src/backend/mir/table.rs`): each is a pointer-sized word, the `k`-th
/// at `k` words, as the assertions after [`NO_PAIR`] check.
#[repr(C)]
pub struct MirMap {
    buf: *mut u8,
    entry_size: usize,
    /// The slots in use, removed ones included: an index is a live
    /// entry's place among the live entries, and a slot its place in
    /// `buf`. Removing from a hashed table leaves a hole rather than
    /// shifting what follows (B-2026-10-12-4); a sorted table never has
    /// one, as bisection needs its entries contiguous.
    slots: usize,
    /// How many slots are holes (`dead` says which).
    holes: usize,
    /// `index`'s cells and their number, kept in step with it for
    /// compiled code (0 cells while it is empty).
    cells: *const u64,
    ncells: usize,
    /// The walk order of a hashed table ([`karac_mir_map_walk_at`]): the
    /// `i`th entry of a walk is the entry at index `(perm_a * i + perm_c)
    /// mod perm_len`, from `karac_hash::walk_perm(perm_len)`. Compiled
    /// code walks in line while `perm_len` is the table's length, and
    /// calls [`karac_mir_map_walk_perm`] to bring the words up to date
    /// when it is not; `usize::MAX` until the first walk.
    perm_len: std::sync::atomic::AtomicUsize,
    perm_a: std::sync::atomic::AtomicUsize,
    perm_c: std::sync::atomic::AtomicUsize,
    cap: usize,
    /// Each slot's hash, for the hashed kinds.
    hashes: Vec<u64>,
    /// An open-addressing table of the live slots, by hash, for the
    /// hashed kinds: a power of two long (or empty), at most half full
    /// counting [`TOMB`]s, probed linearly. A cell holds a slot in its low
    /// half and the top half of the slot's hash in its high half, so a
    /// probe reads neither `hashes` nor `buf` for a key it passes over.
    index: Vec<u64>,
    /// The cells of `index` that are not [`EMPTY`].
    index_used: usize,
    dead: Vec<bool>,
    /// A Fenwick tree over `cap` slots counting the live ones, kept only
    /// while there are holes: index to slot and back in `O(log n)`.
    live: Vec<i64>,
    /// The last index and slot paired while there were holes, as
    /// `index << 32 | slot` ([`NO_PAIR`] for none): a lookup's index is
    /// most often handed straight back to `at`, `remove` or the next
    /// lookup of the same key, which then skip the tree. A removal or a
    /// compaction moves indices, so it forgets the pair; appending a slot
    /// moves none. One word, so a reader racing another reader of the same
    /// table sees a whole pair or none.
    pair: std::sync::atomic::AtomicU64,
    /// The table's type names a hasher (`Map[K, V, FxBuildHasher]`, a
    /// user `BuildHasher`): it walks in an order no process key moves
    /// ([`karac_mir_map_unkeyed`], B-2026-10-24-29).
    unkeyed: bool,
}

const NO_PAIR: u64 = u64::MAX;

// The words compiled code reads, in order.
const _: () = {
    let w = std::mem::size_of::<usize>();
    assert!(std::mem::size_of::<*const u64>() == w);
    assert!(std::mem::offset_of!(MirMap, buf) == 0);
    assert!(std::mem::offset_of!(MirMap, entry_size) == w);
    assert!(std::mem::offset_of!(MirMap, slots) == 2 * w);
    assert!(std::mem::offset_of!(MirMap, holes) == 3 * w);
    assert!(std::mem::offset_of!(MirMap, cells) == 4 * w);
    assert!(std::mem::offset_of!(MirMap, ncells) == 5 * w);
    assert!(std::mem::offset_of!(MirMap, perm_len) == 6 * w);
    assert!(std::mem::offset_of!(MirMap, perm_a) == 7 * w);
    assert!(std::mem::offset_of!(MirMap, perm_c) == 8 * w);
    assert!(std::mem::size_of::<std::sync::atomic::AtomicUsize>() == w);
};

/// An `index` cell that never held a slot: a probe stops here.
const EMPTY: u64 = u32::MAX as u64;
/// An `index` cell whose slot was removed: a probe goes on past it.
const TOMB: u64 = (u32::MAX - 1) as u64;

/// The `index` cell of slot `sl`, whose hash is `hv`.
fn cell(hv: u64, sl: usize) -> u64 {
    (hv & !0xFFFF_FFFF) | sl as u64
}

/// The slot an `index` cell holds, or [`EMPTY`] or [`TOMB`].
fn cell_slot(c: u64) -> u64 {
    c & 0xFFFF_FFFF
}

impl MirMap {
    /// Where the probe for `hv` starts in an `index` of `mask + 1` cells.
    fn home(hv: u64, mask: usize) -> usize {
        (hv.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 32) as usize & mask
    }

    /// The live slot whose key has hash `hv` and equals `key`.
    ///
    /// # Safety
    /// `key` is a key of the table's type and `eq` its equality.
    unsafe fn find_slot(&self, hv: u64, key: *const u8, eq: EqFn) -> Option<usize> {
        if self.index.is_empty() {
            return None;
        }
        let mask = self.index.len() - 1;
        let mut i = Self::home(hv, mask);
        let tag = hv >> 32;
        loop {
            let c = self.index[i];
            match cell_slot(c) {
                EMPTY => return None,
                TOMB => {}
                sl if c >> 32 == tag => {
                    let sl = sl as usize;
                    // SAFETY: the caller's contract; `sl < slots`.
                    if unsafe { eq(self.slot_ptr(sl), key) } {
                        return Some(sl);
                    }
                }
                _ => {}
            }
            i = (i + 1) & mask;
        }
    }

    /// Room in `index` for one more slot.
    fn index_reserve(&mut self) {
        if (self.index_used + 1) * 2 > self.index.len() {
            let want = ((self.len() + 1) * 4).next_power_of_two().max(8);
            self.reindex(want);
        }
    }

    /// Rebuilds `index` with `cells` cells over the live slots.
    fn reindex(&mut self, cells: usize) {
        self.index.clear();
        self.index.resize(cells, EMPTY);
        self.cells = self.index.as_ptr();
        self.ncells = cells;
        self.index_used = 0;
        for sl in 0..self.hashes.len() {
            if !self.dead[sl] {
                self.index_place(self.hashes[sl], sl);
            }
        }
    }

    /// Records slot `sl` (hash `hv`) in `index`, which has room.
    fn index_place(&mut self, hv: u64, sl: usize) {
        let mask = self.index.len() - 1;
        let mut i = Self::home(hv, mask);
        loop {
            match cell_slot(self.index[i]) {
                EMPTY => {
                    self.index[i] = cell(hv, sl);
                    self.index_used += 1;
                    return;
                }
                TOMB => {
                    self.index[i] = cell(hv, sl);
                    return;
                }
                _ => i = (i + 1) & mask,
            }
        }
    }

    /// Forgets slot `sl` (hash `hv`) in `index`.
    fn index_take(&mut self, hv: u64, sl: usize) {
        let mask = self.index.len() - 1;
        let mut i = Self::home(hv, mask);
        while cell_slot(self.index[i]) != sl as u64 {
            debug_assert!(
                cell_slot(self.index[i]) != EMPTY,
                "a live slot is missing from the index"
            );
            i = (i + 1) & mask;
        }
        self.index[i] = TOMB;
    }

    /// The walk-order words for the current length, brought up to date
    /// first: `(len, a, c)`. `perm_len` is stored last, so a reader that
    /// sees it current sees the `a` and `c` that go with it.
    fn walk_perm(&self) -> (usize, usize, usize) {
        use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};
        let n = self.len();
        if self.perm_len.load(Acquire) != n {
            let (a, c) = if self.hashes.is_empty() {
                (1, 0)
            } else if self.unkeyed {
                karac_hash::walk_perm_by(n as u64, karac_hash::hash_u64_unkeyed)
            } else {
                karac_hash::walk_perm(n as u64)
            };
            self.perm_a.store(a as usize, Relaxed);
            self.perm_c.store(c as usize, Relaxed);
            self.perm_len.store(n, Release);
        }
        (n, self.perm_a.load(Relaxed), self.perm_c.load(Relaxed))
    }

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
        let p = self.pair.load(std::sync::atomic::Ordering::Relaxed);
        if p != NO_PAIR && (p >> 32) as usize == i {
            return (p & 0xFFFF_FFFF) as usize;
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
        self.remember(i, pos);
        pos
    }

    fn remember(&self, i: usize, s: usize) {
        self.pair.store(
            ((i as u64) << 32) | s as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
    }

    fn forget(&self) {
        self.pair
            .store(NO_PAIR, std::sync::atomic::Ordering::Relaxed);
    }

    /// The index of the live entry in slot `s`.
    fn index_of(&self, s: usize) -> usize {
        if self.holes == 0 {
            return s;
        }
        let p = self.pair.load(std::sync::atomic::Ordering::Relaxed);
        if p != NO_PAIR && (p & 0xFFFF_FFFF) as usize == s {
            return (p >> 32) as usize;
        }
        // The live slots before `s`.
        let (mut k, mut n) = (s, 0i64);
        while k > 0 {
            n += self.live[k - 1];
            k &= k - 1;
        }
        self.remember(n as usize, s);
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
        self.forget();
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
        let cells = self.index.len().max(8);
        self.reindex(cells);
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
    // SAFETY: as above.
    unsafe { karac_mir_map_find_hv(h, key, hv, eq) }
}

/// [`karac_mir_map_find`] on a hashed table, with the key's hash `hv`
/// computed by the caller: compiled code hashes a scalar key inline, so
/// a `get` and the `insert` after it hash it once.
///
/// # Safety
/// As [`karac_mir_map_find`]; `hv` is `key`'s hash under the table's glue.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_map_find_hv(
    h: *const MirMap,
    key: *const u8,
    hv: u64,
    eq: EqFn,
) -> i64 {
    if h.is_null() {
        return -1;
    }
    // SAFETY: the caller's contract.
    let m = unsafe { &*h };
    // SAFETY: as above.
    match unsafe { m.find_slot(hv, key, eq) } {
        Some(sl) => m.index_of(sl) as i64,
        None => -1,
    }
}

/// The entry whose key equals `key`, or null: [`karac_mir_map_find`]
/// handing back where the entry is rather than its index, which a table
/// with holes would have to count.
///
/// # Safety
/// As [`karac_mir_map_find`].
#[no_mangle]
pub unsafe extern "C" fn karac_mir_map_lookup(
    h: *const MirMap,
    key: *const u8,
    hash: Option<HashFn>,
    eq: Option<EqFn>,
    cmp: Option<CmpFn>,
) -> *mut u8 {
    if h.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: the caller's contract.
    let m = unsafe { &*h };
    if let Some(cmp) = cmp {
        // SAFETY: as above; a sorted table's indices are its slots.
        return match unsafe { search(m, key, cmp) } {
            Ok(i) => m.slot_ptr(i),
            Err(_) => std::ptr::null_mut(),
        };
    }
    let (Some(hash), Some(eq)) = (hash, eq) else {
        return std::ptr::null_mut();
    };
    // SAFETY: as above.
    unsafe { karac_mir_map_lookup_hv(h, key, hash(key), eq) }
}

/// [`karac_mir_map_lookup`] on a hashed table, with the key's hash `hv`
/// computed by the caller, as for [`karac_mir_map_find_hv`].
///
/// # Safety
/// As [`karac_mir_map_find_hv`].
#[no_mangle]
pub unsafe extern "C" fn karac_mir_map_lookup_hv(
    h: *const MirMap,
    key: *const u8,
    hv: u64,
    eq: EqFn,
) -> *mut u8 {
    if h.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: the caller's contract.
    let m = unsafe { &*h };
    // SAFETY: as above.
    match unsafe { m.find_slot(hv, key, eq) } {
        Some(sl) => m.slot_ptr(sl),
        None => std::ptr::null_mut(),
    }
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
    entry_size: u64,
    entry: *const u8,
    hash: Option<HashFn>,
    cmp: Option<CmpFn>,
) -> i64 {
    let entry_size = entry_size as usize;
    // SAFETY: the caller's contract.
    let hv = hash.map(|hash| unsafe { hash(entry) });
    // SAFETY: as above.
    unsafe {
        let s = insert(hp, entry_size, entry, hv, cmp);
        (**hp).index_of(s) as i64
    }
}

/// [`karac_mir_map_insert`] into a hashed table, with the key's hash `hv`
/// computed by the caller, as for [`karac_mir_map_find_hv`]. Returns
/// where the new entry is, not its index.
///
/// # Safety
/// As [`karac_mir_map_insert`]; `hv` is the entry key's hash.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_map_insert_hv(
    hp: *mut *mut MirMap,
    entry_size: u64,
    entry: *const u8,
    hv: u64,
) -> *mut u8 {
    let entry_size = entry_size as usize;
    // SAFETY: the caller's contract.
    unsafe {
        let s = insert(hp, entry_size, entry, Some(hv), None);
        (**hp).slot_ptr(s)
    }
}

/// A new table with no entries, of `entry_size`-byte entries.
fn empty(entry_size: usize) -> *mut MirMap {
    Box::into_raw(Box::new(MirMap {
        buf: std::ptr::null_mut(),
        entry_size,
        slots: 0,
        holes: 0,
        cells: std::ptr::null(),
        ncells: 0,
        perm_len: std::sync::atomic::AtomicUsize::new(usize::MAX),
        perm_a: std::sync::atomic::AtomicUsize::new(1),
        perm_c: std::sync::atomic::AtomicUsize::new(0),
        cap: 0,
        hashes: Vec::new(),
        index: Vec::new(),
        index_used: 0,
        dead: Vec::new(),
        live: Vec::new(),
        pair: std::sync::atomic::AtomicU64::new(NO_PAIR),
        unkeyed: false,
    }))
}

/// The table at `h` (null for one that never held an entry), made to walk
/// in an order no process key moves, because its type names a hasher
/// (`Map[K, V, FxBuildHasher]`, a user `BuildHasher`): the walk's `(a, c)`
/// come from a fixed key, not the process's (collections.md § Hashing,
/// B-2026-10-24-29). Returns the table, created when `h` is null.
///
/// # Safety
/// `h` is null or a live table of `entry_size`-byte entries.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_map_unkeyed(h: *mut MirMap, entry_size: u64) -> *mut MirMap {
    let h = if h.is_null() {
        empty(entry_size as usize)
    } else {
        h
    };
    // SAFETY: the caller's contract.
    let m = unsafe { &mut *h };
    m.unkeyed = true;
    m.perm_len
        .store(usize::MAX, std::sync::atomic::Ordering::Release);
    h
}

/// The table a clone of `h` starts as: null, or an empty one that walks
/// as `h` does when `h` is [`karac_mir_map_unkeyed`].
///
/// # Safety
/// `h` is null or a live table of `entry_size`-byte entries.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_map_like(h: *const MirMap, entry_size: u64) -> *mut MirMap {
    // SAFETY: the caller's contract.
    if h.is_null() || !unsafe { &*h }.unkeyed {
        return std::ptr::null_mut();
    }
    // SAFETY: a fresh table.
    unsafe { karac_mir_map_unkeyed(std::ptr::null_mut(), entry_size) }
}

/// Returns the new entry's slot.
///
/// # Safety
/// As [`karac_mir_map_insert`], with `hv` the hash for a hashed table.
unsafe fn insert(
    hp: *mut *mut MirMap,
    entry_size: usize,
    entry: *const u8,
    hash: Option<u64>,
    cmp: Option<CmpFn>,
) -> usize {
    // SAFETY: the caller's contract.
    let m = unsafe {
        if (*hp).is_null() {
            *hp = empty(entry_size);
        }
        &mut **hp
    };
    m.grow();
    if hash.is_some() {
        m.index_reserve();
    }
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
    if let Some(hv) = hash {
        // A slot must fit the low half of an `index` cell.
        assert!((at as u64) < TOMB, "a hashed table of 2^32 entries");
        m.hashes.push(hv);
        m.index_place(hv, at);
    }
    at
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

/// Brings the walk-order words of the table at `h` up to date for its
/// length: `karac_hash::walk_perm` of it for a hashed table, so a walk
/// visits the entries in an order that changes from one process to the
/// next and that `KARAC_HASH_SEED` pins, the order the MIR interpreter
/// walks too (collections.md § Map, B-2026-10-23-19); 1 and 0, the order
/// of the indices, for a sorted one.
///
/// # Safety
/// `h` is a live table.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_map_walk_perm(h: *const MirMap) {
    // SAFETY: the caller's contract.
    unsafe { (*h).walk_perm() };
}

/// The `i`th entry of a walk over the table at `h` (in bounds), in the
/// order [`karac_mir_map_walk_perm`] describes.
///
/// # Safety
/// `h` is a live table with more than `i` entries.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_map_walk_at(h: *const MirMap, i: i64) -> *mut u8 {
    // SAFETY: the caller's contract.
    let m = unsafe { &*h };
    let (n, a, c) = m.walk_perm();
    m.at(karac_hash::walk_index(i as u64, n as u64, a as u64, c as u64) as usize)
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
    let s = m.slot_of(i as usize);
    // SAFETY: as above.
    unsafe { remove_slot(m, s, out) }
}

/// [`karac_mir_map_remove`] of the entry at `at`, which a lookup handed
/// back.
///
/// # Safety
/// `h` is a live table and `at` one of its live entries; `out` is
/// writable for an entry.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_map_remove_entry(h: *mut MirMap, at: *const u8, out: *mut u8) {
    // SAFETY: the caller's contract.
    let m = unsafe { &mut *h };
    let s = (at as usize - m.buf as usize) / m.entry_size.max(1);
    // SAFETY: as above.
    unsafe { remove_slot(m, s, out) }
}

/// Moves the live entry in slot `s` to `out`, as [`karac_mir_map_remove`].
///
/// # Safety
/// As [`karac_mir_map_remove`], with `s` a live slot.
unsafe fn remove_slot(m: &mut MirMap, s: usize, out: *mut u8) {
    let es = m.entry_size;
    m.forget();
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
    m.index_take(hv, s);
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
    m.cells = std::ptr::null();
    m.ncells = 0;
    m.index_used = 0;
    m.dead.clear();
    m.holes = 0;
    m.live.clear();
    m.forget();
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
        // Start with a few hundred entries, so the walk removes from a
        // large table as well as a small one.
        for _ in 0..200 {
            let k = next;
            next += 1;
            // SAFETY: a live (or null) table of `i64` entries.
            unsafe { karac_mir_map_insert(&mut h, 8, (&k as *const i64).cast(), Some(hash), None) };
            model.push(k);
        }
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
                // A removed key, and one never added, are absent.
                for gone in [out, next] {
                    // SAFETY: as above.
                    let f = unsafe {
                        karac_mir_map_find(
                            h,
                            (&gone as *const i64).cast(),
                            Some(hash),
                            Some(eq),
                            None,
                        )
                    };
                    assert_eq!(f, -1);
                }
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
