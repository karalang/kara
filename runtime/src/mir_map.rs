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
//! has never held an entry. A sorted table past a few thousand bytes of
//! entries keeps them in a B+-tree instead ([`tree`], B-2026-11-10-39).

use crate::alloc::{karac_free, karac_realloc_or_panic};
use crate::lean_vec::LeanVec;

mod tree;

/// `n` rounded up to a multiple of 8.
const fn round8(n: usize) -> usize {
    (n + 7) & !7
}

pub type HashFn = unsafe extern "C" fn(*const u8) -> u64;
pub type EqFn = unsafe extern "C" fn(*const u8, *const u8) -> bool;
pub type CmpFn = unsafe extern "C" fn(*const u8, *const u8) -> i32;

/// The first ten fields are read by compiled code, which probes a table
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
    /// The index, for the hashed kinds (0 cells while it is empty): read
    /// by compiled code, which probes it for a scalar key. `cells` points
    /// at `ncells + GROUP` control bytes, the last [`GROUP`] mirroring the
    /// first so a group can be read at any cell, and the `ncells` 4-byte
    /// slots follow them. A control byte is [`CTRL_EMPTY`],
    /// [`CTRL_DELETED`] or the top seven bits of a live slot's hash, so a
    /// probe reads eight cells at a time and touches a slot or an entry
    /// only where the tag matches (B-2026-11-08-8).
    cells: *const u8,
    ncells: usize,
    /// The walk order of a hashed table ([`karac_mir_map_walk_entry`]): the
    /// `i`th entry of a walk is the entry at index `(perm_a * i + perm_c)
    /// mod perm_len`, from `karac_hash::walk_perm(perm_len)`;
    /// `usize::MAX` until the first walk. Compiled code reads them while
    /// `perm_len` is the table's length, and calls
    /// [`karac_mir_map_walk_perm`] to bring them up to date when it is not
    /// (on a 64-bit target through `karac.walk.words`, which a loop runs
    /// once, `src/backend/mir/table.rs`).
    perm_len: std::sync::atomic::AtomicUsize,
    perm_a: std::sync::atomic::AtomicUsize,
    perm_c: std::sync::atomic::AtomicUsize,
    /// `floor((2^64 - 1) / perm_len)`, so compiled code on a 64-bit target
    /// (a 32-bit one divides) takes the walk's remainder by a multiply
    /// rather than a division (B-2026-11-01-44): `karac_hash`'s
    /// `walk_reciprocal`, whose test proves the one correction it needs.
    perm_m: std::sync::atomic::AtomicUsize,
    cap: usize,
    /// A hashed table: each slot's hash is kept after the entries, in the
    /// same block as `buf` ([`MirMap::hashes`]), at `round8(cap *
    /// entry_size)`. One block rather than two (B-2026-11-08-7).
    hashed: bool,
    /// `buf` is the room for [`INLINE_SLOTS`] entries (and their hashes)
    /// allocated with the header, so a small table is one allocation
    /// (B-2026-11-08-7); it moves to a block of its own when it grows.
    data_inline: bool,
    /// `cells` (the index) is the room for [`INLINE_CELLS`] cells
    /// allocated with the header, rather than a block of its own.
    cells_inline: bool,
    /// That room for cells, after the header and its inline entries.
    inline_cells: *mut u8,
    /// The index's cells that are not [`CTRL_EMPTY`], live or deleted: at
    /// most 7/8 of them, so a probe always meets an empty one.
    index_used: usize,
    /// Which slots are holes: empty while there are none (so a table that
    /// never loses an entry never allocates it), else one per slot.
    dead: LeanVec<bool>,
    /// A Fenwick tree over `cap` slots counting the live ones, kept only
    /// while there are holes: index to slot and back in `O(log n)`. Built
    /// when an index is first asked of a holed table, not by the removal
    /// that makes the hole (B-2026-10-24-6): lookups hand back entries, so
    /// a table that is only searched and changed never needs it. Readers
    /// may share a table, so the one that finds it stale builds it while
    /// the others wait ([`MirMap::ensure_live`]).
    live: std::cell::UnsafeCell<LeanVec<i64>>,
    /// Whether `live` counts the current slots: [`LIVE_OK`], [`LIVE_STALE`]
    /// or [`LIVE_BUILDING`].
    live_state: std::sync::atomic::AtomicU8,
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
    /// A sorted table's B+-tree, once it outgrew one array
    /// ([`tree::dense_max`], B-2026-11-10-39); null otherwise. While it has
    /// one, `buf` is null (compiled code's sign to index through
    /// [`karac_mir_map_at`]), `cap` 0, and `slots` its length.
    tree: *mut tree::Tree,
}

const NO_PAIR: u64 = u64::MAX;

/// The entries a table holds in the header's own allocation, when they
/// fit [`INLINE_DATA_MAX`] bytes.
const INLINE_SLOTS: usize = 4;
const INLINE_DATA_MAX: usize = 256;
/// The index cells the header's allocation holds: the index a table of up
/// to seven entries needs (`index_reserve` keeps it at most 7/8 full).
const INLINE_CELLS: usize = GROUP;
/// Where the inline entries start: past the header, at an alignment
/// `malloc`'s covers for any entry.
const INLINE_AT: usize = (std::mem::size_of::<MirMap>() + 15) & !15;

const LIVE_OK: u8 = 0;
const LIVE_STALE: u8 = 1;
const LIVE_BUILDING: u8 = 2;

/// The holes a hashed table keeps before a removal compacts it, at least.
const COMPACT_MIN_HOLES: usize = 32;

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
    assert!(std::mem::offset_of!(MirMap, perm_m) == 9 * w);
    assert!(std::mem::size_of::<std::sync::atomic::AtomicUsize>() == w);
};

/// The control bytes a probe reads at once, as one word.
pub const GROUP: usize = 8;
/// The control byte of a cell that never held a slot: a probe stops at a
/// group with one.
pub const CTRL_EMPTY: u8 = 0xFF;
/// The control byte of a cell whose slot was removed: a probe goes on.
pub const CTRL_DELETED: u8 = 0x80;
const LSB: u64 = 0x0101_0101_0101_0101;
const MSB: u64 = 0x8080_8080_8080_8080;

/// A live cell's control byte: the top seven bits of its hash.
pub fn ctrl_tag(hv: u64) -> u8 {
    (hv >> 57) as u8
}

/// The bytes of the group `g` that may equal `b`, as their top bits: every
/// one that does, and rarely one more past it (the key decides).
fn match_byte(g: u64, b: u8) -> u64 {
    let x = g ^ LSB.wrapping_mul(b as u64);
    x.wrapping_sub(LSB) & !x & MSB
}

/// The [`CTRL_EMPTY`] bytes of `g`: the only ones with both top bits set.
fn match_empty(g: u64) -> u64 {
    g & (g << 1) & MSB
}

/// The bytes of `g` that hold no live slot.
fn match_free(g: u64) -> u64 {
    g & MSB
}

/// The cell a group's matched top bit names, from the group's first cell.
fn first_cell(pos: usize, m: u64, mask: usize) -> usize {
    (pos + (m.trailing_zeros() / 8) as usize) & mask
}

/// The bytes an index of `n` cells takes: control bytes, then slots.
fn index_bytes(n: usize) -> usize {
    n + GROUP + 4 * n
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
        if self.ncells == 0 {
            return None;
        }
        let mask = self.ncells - 1;
        let tag = ctrl_tag(hv);
        let (mut pos, mut stride) = (Self::home(hv, mask), 0);
        loop {
            let g = self.group(pos);
            let mut m = match_byte(g, tag);
            while m != 0 {
                let sl = self.slot_in(first_cell(pos, m, mask));
                // SAFETY: the caller's contract; a tagged cell's slot is
                // below `slots`.
                if unsafe { eq(self.slot_ptr(sl), key) } {
                    return Some(sl);
                }
                m &= m - 1;
            }
            if match_empty(g) != 0 {
                return None;
            }
            stride += GROUP;
            pos = (pos + stride) & mask;
        }
    }

    /// The [`GROUP`] control bytes from cell `pos <= mask`, as a word.
    fn group(&self, pos: usize) -> u64 {
        // SAFETY: the control bytes run `GROUP` past the last cell.
        u64::from_le_bytes(unsafe { self.cells.add(pos).cast::<[u8; GROUP]>().read_unaligned() })
    }

    /// The slots, after the control bytes.
    fn slot_cells(&self) -> *mut u32 {
        // SAFETY: inside the index's block; `ncells + GROUP` is a multiple
        // of 8, so the slots are aligned.
        unsafe { self.cells.cast_mut().add(self.ncells + GROUP).cast::<u32>() }
    }

    /// The slot cell `i < ncells` holds.
    fn slot_in(&self, i: usize) -> usize {
        // SAFETY: `i < ncells`.
        unsafe { *self.slot_cells().add(i) as usize }
    }

    /// Sets cell `i`'s control byte, and its mirror past the last cell.
    fn set_ctrl(&mut self, i: usize, c: u8) {
        let mask = self.ncells - 1;
        // SAFETY: both are control bytes of the index's block.
        unsafe {
            *self.cells.cast_mut().add(i) = c;
            *self
                .cells
                .cast_mut()
                .add((i.wrapping_sub(GROUP) & mask) + GROUP) = c;
        }
    }

    /// Room in `index` for one more slot.
    fn index_reserve(&mut self) {
        if (self.index_used + 1) * 8 > self.ncells * 7 {
            // Twice as many cells as live slots once full, or the same
            // number again when deleted cells filled it.
            let want = ((self.len() + 1) * 8 / 7 + 1)
                .next_power_of_two()
                .max(INLINE_CELLS);
            self.reindex(want);
        }
    }

    /// Rebuilds `index` with `cells` cells over the live slots.
    fn reindex(&mut self, cells: usize) {
        if cells != self.ncells {
            self.free_cells();
            if cells <= INLINE_CELLS {
                self.cells = self.inline_cells;
                self.cells_inline = true;
            } else {
                self.cells =
                    karac_realloc_or_panic(std::ptr::null_mut(), index_bytes(cells)).cast_const();
                self.cells_inline = false;
            }
            self.ncells = cells;
        }
        // SAFETY: the block holds `ncells + GROUP` control bytes.
        unsafe { std::slice::from_raw_parts_mut(self.cells.cast_mut(), cells + GROUP) }
            .fill(CTRL_EMPTY);
        self.index_used = 0;
        if !self.hashed {
            return;
        }
        for sl in 0..self.slots {
            if !self.is_dead(sl) {
                self.index_place(self.hash_at(sl), sl);
            }
        }
    }

    /// Frees the index's block, if it has one of its own, and leaves it
    /// empty.
    fn free_cells(&mut self) {
        if !self.cells_inline && self.ncells != 0 {
            // SAFETY: a block `reindex` allocated.
            unsafe { free(self.cells.cast_mut().cast()) };
        }
        self.cells = std::ptr::null();
        self.ncells = 0;
        self.cells_inline = false;
    }

    /// Where the slots' hashes start, after `cap` entries.
    fn hashes(&self) -> *mut u64 {
        // SAFETY: inside the block `buf` heads, for a hashed table.
        unsafe {
            self.buf
                .add(round8(self.cap * self.entry_size))
                .cast::<u64>()
        }
    }

    fn hash_at(&self, sl: usize) -> u64 {
        // SAFETY: `sl < slots <= cap` at every caller.
        unsafe { *self.hashes().add(sl) }
    }

    fn set_hash(&mut self, sl: usize, hv: u64) {
        // SAFETY: as `hash_at`.
        unsafe { *self.hashes().add(sl) = hv };
    }

    fn is_dead(&self, sl: usize) -> bool {
        self.holes > 0 && self.dead[sl]
    }

    /// Marks slot `sl` a hole, recording which slots are holes from the
    /// first.
    fn mark_dead(&mut self, sl: usize) {
        if self.dead.is_empty() {
            self.dead.resize(self.slots, false);
        }
        self.dead[sl] = true;
    }

    /// Records slot `sl` (hash `hv`) in `index`, which has room.
    fn index_place(&mut self, hv: u64, sl: usize) {
        let mask = self.ncells - 1;
        let (mut pos, mut stride) = (Self::home(hv, mask), 0);
        loop {
            let m = match_free(self.group(pos));
            if m != 0 {
                let i = first_cell(pos, m, mask);
                // SAFETY: `i < ncells`.
                if unsafe { *self.cells.add(i) } == CTRL_EMPTY {
                    self.index_used += 1;
                }
                self.set_ctrl(i, ctrl_tag(hv));
                // SAFETY: as above.
                unsafe { *self.slot_cells().add(i) = sl as u32 };
                return;
            }
            stride += GROUP;
            pos = (pos + stride) & mask;
        }
    }

    /// Forgets slot `sl` (hash `hv`) in `index`.
    fn index_take(&mut self, hv: u64, sl: usize) {
        let mask = self.ncells - 1;
        let tag = ctrl_tag(hv);
        let (mut pos, mut stride) = (Self::home(hv, mask), 0);
        loop {
            let g = self.group(pos);
            let mut m = match_byte(g, tag);
            while m != 0 {
                let i = first_cell(pos, m, mask);
                // SAFETY: `i < ncells`.
                if unsafe { *self.cells.add(i) } == tag && self.slot_in(i) == sl {
                    self.set_ctrl(i, CTRL_DELETED);
                    return;
                }
                m &= m - 1;
            }
            debug_assert!(match_empty(g) == 0, "a live slot is missing from the index");
            stride += GROUP;
            pos = (pos + stride) & mask;
        }
    }

    /// The walk-order words for the current length, brought up to date
    /// first: `(len, a, c)`. `perm_len` is stored last, so a reader that
    /// sees it current sees the `a` and `c` that go with it.
    fn walk_perm(&self) -> (usize, usize, usize) {
        use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};
        let n = self.len();
        if self.perm_len.load(Acquire) != n {
            let (a, c) = if !self.hashed || self.slots == 0 {
                (1, 0)
            } else if self.unkeyed {
                karac_hash::walk_perm_by(n as u64, karac_hash::hash_u64_unkeyed)
            } else {
                karac_hash::walk_perm(n as u64)
            };
            self.perm_a.store(a as usize, Relaxed);
            self.perm_c.store(c as usize, Relaxed);
            self.perm_m
                .store(karac_hash::walk_reciprocal(n as u64) as usize, Relaxed);
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
        if let Some(t) = self.tree() {
            return t.at(i);
        }
        self.slot_ptr(self.slot_of(i))
    }

    /// The sorted table's tree, if it has one.
    fn tree(&self) -> Option<&tree::Tree> {
        // SAFETY: non-null, a live tree this table owns.
        unsafe { self.tree.as_ref() }
    }

    /// The room the header's allocation has for entries, if any: where a
    /// table starts and where `clear` or leaving a tree puts it back.
    fn header_room(&mut self) {
        let es = self.entry_size;
        if round8(INLINE_SLOTS * es) <= INLINE_DATA_MAX {
            // SAFETY: `empty` allocated that room after the header.
            self.buf = unsafe { (self as *mut MirMap).cast::<u8>().add(INLINE_AT) };
            self.cap = INLINE_SLOTS;
            self.data_inline = true;
        } else {
            self.buf = std::ptr::null_mut();
            self.cap = 0;
            self.data_inline = false;
        }
    }

    /// Moves a sorted array's entries into a tree.
    fn make_tree(&mut self, cmp: CmpFn) {
        // SAFETY: `buf` holds `slots` entries in key order (no holes).
        self.tree = unsafe { tree::Tree::build(self.buf, self.slots, self.entry_size, cmp) };
        if self.cap != 0 && !self.data_inline {
            // SAFETY: `buf` came from `karac_realloc_or_panic`.
            unsafe { free(self.buf.cast()) };
        }
        self.buf = std::ptr::null_mut();
        self.cap = 0;
        self.data_inline = false;
    }

    /// Moves a tree whose root is a leaf back into one array.
    fn leave_tree(&mut self) {
        let Some(t) = self.tree() else { return };
        if !t.is_one_leaf() {
            return;
        }
        let (src, n) = t.one_leaf();
        let es = self.entry_size;
        self.header_room();
        if n > self.cap {
            let cap = n.next_power_of_two();
            self.buf = karac_realloc_or_panic(std::ptr::null_mut(), round8(cap * es).max(1));
            self.cap = cap;
            self.data_inline = false;
        }
        // SAFETY: the leaf holds `n` entries; `buf` has room for `cap >= n`.
        unsafe {
            std::ptr::copy_nonoverlapping(src, self.buf, n * es);
            tree::Tree::free(self.tree);
        }
        self.tree = std::ptr::null_mut();
        self.slots = n;
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
        let live = self.ensure_live();
        let n = live.len();
        let (mut pos, mut rem) = (0usize, i as i64 + 1);
        let mut step = n.next_power_of_two();
        while step > 0 {
            let next = pos + step;
            if next <= n && live[next - 1] < rem {
                pos = next;
                rem -= live[next - 1];
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
        let live = self.ensure_live();
        let (mut k, mut n) = (s, 0i64);
        while k > 0 {
            n += live[k - 1];
            k &= k - 1;
        }
        self.remember(n as usize, s);
        n as usize
    }

    /// Counts slot `s` `d` more live, if the tree is current; a stale one
    /// is rebuilt whole when next asked.
    fn add_live(&mut self, s: usize, d: i64) {
        if *self.live_state.get_mut() != LIVE_OK {
            return;
        }
        let live = self.live.get_mut();
        let mut k = s + 1;
        while k <= live.len() {
            live[k - 1] += d;
            k += k & k.wrapping_neg();
        }
    }

    /// Marks the tree as not counting the current slots.
    fn stale_live(&mut self) {
        *self.live_state.get_mut() = LIVE_STALE;
    }

    /// The tree, current: built here, in `O(cap)`, if it is stale. A
    /// table is changed only through `&mut`, so while readers share it
    /// nothing else writes `live`, and the one reader that claims the
    /// build writes it while the rest wait for [`LIVE_OK`].
    fn ensure_live(&self) -> &LeanVec<i64> {
        use std::sync::atomic::Ordering::{Acquire, Release};
        loop {
            match self.live_state.load(Acquire) {
                LIVE_OK => break,
                LIVE_STALE => {
                    if self
                        .live_state
                        .compare_exchange(LIVE_STALE, LIVE_BUILDING, Acquire, Acquire)
                        .is_ok()
                    {
                        // SAFETY: this reader holds LIVE_BUILDING, so no
                        // other reads or writes `live` until it stores
                        // LIVE_OK, and no writer holds the table.
                        self.build_live(unsafe { &mut *self.live.get() });
                        self.live_state.store(LIVE_OK, Release);
                        break;
                    }
                }
                _ => std::hint::spin_loop(),
            }
        }
        // SAFETY: LIVE_OK: nothing writes `live` while a reader holds it.
        unsafe { &*self.live.get() }
    }

    /// The Fenwick tree over the current slots, into `live`.
    fn build_live(&self, live: &mut LeanVec<i64>) {
        live.clear();
        live.resize(self.cap, 0);
        for (n, &dead) in live.as_mut_slice().iter_mut().zip(&self.dead[..self.slots]) {
            if !dead {
                *n += 1;
            }
        }
        for k in 1..=self.cap {
            let up = k + (k & k.wrapping_neg());
            if up <= self.cap {
                live[up - 1] += live[k - 1];
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
                let hv = self.hash_at(r);
                self.set_hash(w, hv);
            }
            w += 1;
        }
        self.slots = w;
        self.holes = 0;
        self.dead.clear();
        self.live.get_mut().clear();
        self.stale_live();
        let cells = self.ncells.max(8);
        self.reindex(cells);
    }

    fn grow(&mut self) {
        if self.slots < self.cap {
            return;
        }
        let cap = (self.cap * 2).max(INLINE_SLOTS);
        let es = self.entry_size;
        let (old_at, new_at) = (round8(self.cap * es), round8(cap * es));
        let hashes = if self.hashed { self.slots * 8 } else { 0 };
        let size = (new_at + if self.hashed { cap * 8 } else { 0 }).max(1);
        if self.cap == 0 || self.data_inline {
            let buf = karac_realloc_or_panic(std::ptr::null_mut(), size);
            // SAFETY: the old room holds `slots` entries and their hashes;
            // the new block has room for `cap` of each.
            if self.cap != 0 {
                unsafe {
                    std::ptr::copy_nonoverlapping(self.buf, buf, self.slots * es);
                    std::ptr::copy_nonoverlapping(self.buf.add(old_at), buf.add(new_at), hashes);
                }
            }
            self.buf = buf;
            self.data_inline = false;
        } else {
            self.buf = karac_realloc_or_panic(self.buf, size);
            // SAFETY: the hashes move up past the larger entry room, inside
            // the block (`copy` allows the overlap).
            unsafe { std::ptr::copy(self.buf.add(old_at), self.buf.add(new_at), hashes) };
        }
        self.cap = cap;
        if self.holes > 0 {
            self.stale_live();
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
        if let Some(t) = m.tree() {
            // SAFETY: as above.
            return unsafe { t.lookup(key) };
        }
        // SAFETY: as above; a sorted array's indices are its slots.
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
    if let Some(t) = m.tree() {
        // SAFETY: the caller's contract.
        return unsafe { t.find(key) }.0;
    }
    let (mut lo, mut hi) = (0, m.len());
    while lo < hi {
        let mid = (lo + hi) / 2;
        // SAFETY: the caller's contract; a sorted array has no holes, so
        // its indices are its slots.
        match unsafe { cmp(m.slot_ptr(mid), key) } {
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
    if let Some(cmp) = cmp {
        // SAFETY: the caller's contract.
        let m = unsafe {
            if (*hp).is_null() {
                *hp = empty(entry_size);
            }
            &mut **hp
        };
        // `dense_max` is at least 16 (two leaves of 8), so a small table
        // skips its divisions.
        if m.tree.is_null() && m.slots >= 16 && m.slots >= tree::dense_max(m.entry_size) {
            m.make_tree(cmp);
        }
        if !m.tree.is_null() {
            m.slots += 1;
            // SAFETY: the tree is live; `entry` an entry of its type.
            return unsafe { (*m.tree).insert(entry) } as i64;
        }
    }
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

/// A new table with no entries, of `entry_size`-byte entries. Allocated
/// as its entries are, not boxed: `Box::new`'s allocation-failure path
/// reaches a panic, which keeps std's panic hook and backtrace symbolizer
/// in every binary with a table (B-2026-10-29-41).
fn empty(entry_size: usize) -> *mut MirMap {
    // The header, then room for `INLINE_SLOTS` entries and their hashes
    // when they are small, then room for `INLINE_CELLS` index cells: a
    // small table is this one allocation (B-2026-11-08-7).
    let data = round8(INLINE_SLOTS * entry_size);
    let inline = data <= INLINE_DATA_MAX;
    let cells_at = INLINE_AT + if inline { data + INLINE_SLOTS * 8 } else { 0 };
    let base = karac_realloc_or_panic(std::ptr::null_mut(), cells_at + index_bytes(INLINE_CELLS));
    let h = base.cast::<MirMap>();
    // SAFETY: a fresh allocation of more than a `MirMap`'s size, aligned as
    // `malloc` aligns, which is at least a `MirMap`'s alignment (and an
    // entry's, at `INLINE_AT`); the inline rooms are inside it.
    unsafe {
        h.write(MirMap {
            buf: if inline {
                base.add(INLINE_AT)
            } else {
                std::ptr::null_mut()
            },
            entry_size,
            slots: 0,
            holes: 0,
            cells: std::ptr::null(),
            ncells: 0,
            perm_len: std::sync::atomic::AtomicUsize::new(usize::MAX),
            perm_a: std::sync::atomic::AtomicUsize::new(1),
            perm_c: std::sync::atomic::AtomicUsize::new(0),
            perm_m: std::sync::atomic::AtomicUsize::new(0),
            cap: if inline { INLINE_SLOTS } else { 0 },
            hashed: false,
            data_inline: inline,
            cells_inline: false,
            inline_cells: base.add(cells_at),
            index_used: 0,
            dead: LeanVec::new(),
            live: std::cell::UnsafeCell::new(LeanVec::new()),
            live_state: std::sync::atomic::AtomicU8::new(LIVE_STALE),
            pair: std::sync::atomic::AtomicU64::new(NO_PAIR),
            unkeyed: false,
            tree: std::ptr::null_mut(),
        })
    };
    h
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
    if hash.is_some() && !m.hashed {
        // A table is hashed from its first entry, before its room has any.
        debug_assert!(m.slots == 0);
        m.hashed = true;
    }
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
    if !m.dead.is_empty() {
        m.dead.push(false);
    }
    if m.holes > 0 {
        m.add_live(at, 1);
    }
    if let Some(hv) = hash {
        // A slot must fit the low half of an `index` cell. Not `assert!`,
        // whose panic keeps std's panic hook in every binary with a table.
        if at >= u32::MAX as usize {
            crate::fatal::write_stderr(b"panic: a hashed table of 2^32 entries\n");
            std::process::abort()
        }
        m.set_hash(at, hv);
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
/// order [`karac_mir_map_walk_perm`] describes. Named `walk_at` before
/// the header's layout changed (B-2026-11-01-44), so a program built
/// against an older archive fails to link.
///
/// # Safety
/// `h` is a live table with more than `i` entries.
#[no_mangle]
pub unsafe extern "C" fn karac_mir_map_walk_entry(h: *const MirMap, i: i64) -> *mut u8 {
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
    if !m.tree.is_null() {
        // SAFETY: as above.
        unsafe { tree_remove(m, i as usize, out) };
        return;
    }
    let s = m.slot_of(i as usize);
    // SAFETY: as above.
    unsafe { remove_slot(m, s, out) }
}

/// Moves the entry at index `i` of a table in a tree to `out`, and the
/// table back into one array once its tree is down to a leaf.
///
/// # Safety
/// As [`karac_mir_map_remove`], with `m` in a tree.
unsafe fn tree_remove(m: &mut MirMap, i: usize, out: *mut u8) {
    // SAFETY: the caller's contract.
    unsafe { (*m.tree).remove_at(i, out) };
    m.slots -= 1;
    m.leave_tree();
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
    if let Some(t) = m.tree() {
        // A lookup handed `at` back, so its key is the table's.
        // SAFETY: as above.
        if let (Ok(i), _, _) = unsafe { t.find(at) } {
            // SAFETY: as above.
            unsafe { tree_remove(m, i, out) };
        }
        return;
    }
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
    if !m.hashed {
        // SAFETY: as above; a sorted table has no holes.
        unsafe {
            if s + 1 < m.slots {
                std::ptr::copy(m.slot_ptr(s + 1), m.slot_ptr(s), (m.slots - s - 1) * es);
            }
        }
        m.slots -= 1;
        return;
    }
    let hv = m.hash_at(s);
    m.index_take(hv, s);
    if s + 1 == m.slots && m.holes == 0 {
        m.slots -= 1;
        return;
    }
    if m.holes == 0 {
        m.mark_dead(s);
        m.holes = 1;
        m.stale_live();
    } else {
        m.mark_dead(s);
        m.holes += 1;
        m.add_live(s, -1);
    }
    // A small table keeps up to `COMPACT_MIN_HOLES` holes: compacting it
    // after every few removes (a sliding window over k keys) cost more
    // than the holes do (B-2026-10-24-6).
    if m.holes > m.len().max(COMPACT_MIN_HOLES) {
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
    if !m.tree.is_null() {
        // SAFETY: a live tree this table owns, not named after.
        unsafe { tree::Tree::free(m.tree) };
        m.tree = std::ptr::null_mut();
        m.header_room();
    }
    m.slots = 0;
    m.free_cells();
    m.index_used = 0;
    m.dead.clear();
    m.holes = 0;
    m.live.get_mut().clear();
    m.stale_live();
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
    // SAFETY: the caller's contract; the table came from `empty`.
    unsafe {
        if (*h).cap != 0 && !(*h).data_inline {
            // `buf` came from `karac_realloc_or_panic`.
            free((*h).buf.cast());
        }
        if !(*h).tree.is_null() {
            tree::Tree::free((*h).tree);
        }
        (*h).free_cells();
        std::ptr::drop_in_place(h);
        karac_free(h.cast());
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

    unsafe extern "C" fn cmp(a: *const u8, b: *const u8) -> i32 {
        // SAFETY: as above.
        let (a, b) = unsafe {
            (
                (a as *const i64).read_unaligned(),
                (b as *const i64).read_unaligned(),
            )
        };
        a.cmp(&b) as i32
    }

    /// An `es`-byte entry: the key, then bytes that name it.
    fn entry(k: i64, es: usize) -> Vec<u8> {
        let mut e = k.to_le_bytes().to_vec();
        e.extend((8..es).map(|j| (k as u8).wrapping_mul(31).wrapping_add(j as u8)));
        e
    }

    /// A table whose entries live with its header (small entries) or in
    /// a block of their own (large ones), hashed or sorted, keeps every
    /// entry's bytes as it grows out of the header's room, loses entries,
    /// is cleared and filled again (B-2026-11-08-7).
    #[test]
    fn entries_survive_the_header_room_growth_and_clear() {
        for es in [8usize, 24, 64, 65, 200] {
            for sorted in [false, true] {
                let mut h: *mut MirMap = std::ptr::null_mut();
                let (hf, ef, cf) = if sorted {
                    (None, None, Some(cmp as CmpFn))
                } else {
                    (Some(hash as HashFn), Some(eq as EqFn), None)
                };
                for round in 0..3i64 {
                    let mut model: Vec<i64> = Vec::new();
                    for n in 0..40i64 {
                        // Keys out of order, so a sorted table shifts.
                        let k = (n * 17 + round) % 97;
                        if model.contains(&k) {
                            continue;
                        }
                        let e = entry(k, es);
                        // SAFETY: a live (or null) table of `es`-byte entries.
                        unsafe { karac_mir_map_insert(&mut h, es as u64, e.as_ptr(), hf, cf) };
                        model.push(k);
                        if sorted {
                            model.sort();
                        }
                        if n % 3 == 2 {
                            let i = (n as usize * 7) % model.len();
                            let mut out = vec![0u8; es];
                            // SAFETY: as above.
                            unsafe { karac_mir_map_remove(h, i as i64, out.as_mut_ptr()) };
                            assert_eq!(out, entry(model.remove(i), es));
                        }
                        for (i, k) in model.iter().enumerate() {
                            // SAFETY: as above.
                            unsafe {
                                let at = karac_mir_map_at(h, i as i64);
                                assert_eq!(std::slice::from_raw_parts(at, es), &entry(*k, es)[..]);
                                let key = k.to_le_bytes();
                                let f = karac_mir_map_find(h, key.as_ptr(), hf, ef, cf);
                                assert_eq!(f as usize, i, "es {es} sorted {sorted} key {k}");
                            }
                        }
                        let gone = 1000i64.to_le_bytes();
                        // SAFETY: as above.
                        let f = unsafe { karac_mir_map_find(h, gone.as_ptr(), hf, ef, cf) };
                        assert_eq!(f, -1);
                    }
                    // SAFETY: as above.
                    unsafe {
                        assert_eq!(karac_mir_map_len(h) as usize, model.len());
                        karac_mir_map_clear(h);
                        assert_eq!(karac_mir_map_len(h), 0);
                    }
                }
                // SAFETY: as above.
                unsafe { karac_mir_map_free(h) };
            }
        }
    }

    unsafe extern "C" fn mix(k: *const u8) -> u64 {
        // SAFETY: the keys are `i64`s.
        let k = unsafe { (k as *const i64).read_unaligned() } as u64;
        (k ^ (k >> 29)).wrapping_mul(0xBF58_476D_1CE4_E5B9) ^ k.rotate_left(41)
    }

    /// The grouped index finds every live key and no removed one through
    /// growth, removals that leave deleted cells, and the rebuilds that
    /// clear them, against a model (B-2026-11-08-8).
    #[test]
    fn the_grouped_index_matches_a_model() {
        let mut h: *mut MirMap = std::ptr::null_mut();
        let mut model: std::collections::BTreeSet<i64> = Default::default();
        let mut rng = 99u64;
        for step in 0..60_000 {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            let k = (rng % 20_000) as i64;
            let kp = (&k as *const i64).cast();
            // SAFETY: a live (or null) table of `i64` entries.
            let at = unsafe { karac_mir_map_find(h, kp, Some(mix), Some(eq), None) };
            assert_eq!(at >= 0, model.contains(&k), "step {step} key {k}");
            if step % 3 == 0 && at >= 0 {
                let mut out = 0i64;
                // SAFETY: as above; `at` is an index of the table.
                unsafe { karac_mir_map_remove(h, at, (&mut out as *mut i64).cast()) };
                assert_eq!(out, k);
                model.remove(&k);
            } else if at < 0 {
                // SAFETY: as above.
                unsafe { karac_mir_map_insert(&mut h, 8, kp, Some(mix), None) };
                model.insert(k);
            }
            if step % 5000 == 0 {
                for q in 0..20_000i64 {
                    // SAFETY: as above.
                    let f = unsafe {
                        karac_mir_map_find(h, (&q as *const i64).cast(), Some(mix), Some(eq), None)
                    };
                    assert_eq!(f >= 0, model.contains(&q), "step {step} probe {q}");
                }
                // SAFETY: as above.
                let m = unsafe { &*h };
                assert!(m.index_used * 8 <= m.ncells * 7);
            }
        }
        // SAFETY: as above.
        unsafe {
            assert_eq!(karac_mir_map_len(h) as usize, model.len());
            karac_mir_map_free(h);
        }
    }

    /// A small table is one allocation, its entries and index in the
    /// header's room; a table that never loses an entry records no holes.
    #[test]
    fn a_small_table_lives_in_its_header() {
        let mut h: *mut MirMap = std::ptr::null_mut();
        for k in 0..4i64 {
            // SAFETY: a live (or null) table of `i64` entries.
            unsafe { karac_mir_map_insert(&mut h, 8, (&k as *const i64).cast(), Some(hash), None) };
        }
        // SAFETY: as above.
        let m = unsafe { &*h };
        let base = h as usize;
        assert!(m.data_inline && m.cells_inline);
        assert_eq!(m.buf as usize, base + INLINE_AT);
        assert!(m.dead.is_empty());
        let k = 4i64;
        // SAFETY: as above.
        unsafe {
            karac_mir_map_insert(&mut h, 8, (&k as *const i64).cast(), Some(hash), None);
            assert!(!(*h).data_inline && (*h).cells_inline);
            for k in 5..8i64 {
                karac_mir_map_insert(&mut h, 8, (&k as *const i64).cast(), Some(hash), None);
            }
            assert!(!(*h).cells_inline);
            karac_mir_map_free(h);
        }
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
