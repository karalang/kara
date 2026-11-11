//! The B+-tree a large sorted table keeps its entries in
//! (B-2026-11-10-39). collections.md promises `O(log n)` for every
//! `SortedMap` and `SortedSet` operation, and one sorted array moves every
//! entry after an insert's place: 100000 inserts of falling keys took
//! 1.8 s against 0.018 s for rising ones.
//!
//! A sorted table is one sorted array, which compiled code indexes in
//! line, until it holds [`dense_max`] entries; then its entries move into
//! leaves of up to `lcap` entries each, linked in key order, under inner
//! nodes that keep for each child the number of entries below it and its
//! leftmost leaf. A key is found by comparing it with the first entry of
//! each child's leftmost leaf, an index by the counts, and an insert or a
//! removal moves at most one leaf's entries, so each is `O(log n)`.
//! Compiled code sees a table in a tree by its null `buf` and asks the
//! runtime for an entry by index ([`Tree::at`]), which a walk in order
//! answers from the last leaf it reached, without a descent.
//!
//! Invariants, between calls: every leaf of a tree with inner nodes holds
//! at least one entry (its first entry is a key a search compares with);
//! every inner node but the root has at least [`INNER_MIN`] - 1 children;
//! a new node always goes to the right of the one it split from, and a
//! merge keeps the left node, so a node's leftmost leaf changes only where
//! [`Tree::fix`] refreshes it.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use super::CmpFn;
use crate::alloc::{karac_free, karac_realloc_or_panic};
use crate::lean_vec::LeanVec;

/// The children an inner node has room for.
const FAN: usize = 32;
/// Below this many children an inner node takes some from a sibling or
/// merges with it.
const INNER_MIN: usize = FAN / 4;
/// A leaf's room, in bytes of entries: a leaf moves at most this much on
/// an insert or a removal.
const LEAF_BYTES: usize = 2048;
/// The bytes of entries a sorted table keeps as one array before it moves
/// them into a tree: an insert at its front moves at most this much.
const DENSE_BYTES: usize = 16384;
/// The cursor while it names no leaf.
const NO_CURSOR: u64 = u64::MAX;

/// The entries a leaf of `es`-byte entries has room for.
pub(super) fn lcap_for(es: usize) -> usize {
    (LEAF_BYTES / es.max(1)).clamp(8, 512)
}

/// The entries a sorted table of `es`-byte entries holds as one array at
/// most: twice a leaf's at least, so a table that just left its tree for
/// an array (at most one leaf's worth) does not go back on the next insert.
pub(super) fn dense_max(es: usize) -> usize {
    (DENSE_BYTES / es.max(1)).max(2 * lcap_for(es))
}

#[repr(C)]
pub(super) struct Leaf {
    len: usize,
    next: *mut Leaf,
    prev: *mut Leaf,
    /// Its place in [`Tree::leaves`], which the cursor names.
    id: usize,
}

/// Where a leaf's entries start, at an alignment `malloc`'s covers.
const LEAF_AT: usize = (std::mem::size_of::<Leaf>() + 15) & !15;

#[repr(C)]
struct Inner {
    len: usize,
    kids: Row<*mut u8>,
    /// The entries under each child.
    counts: Row<usize>,
    /// Each child's leftmost leaf (the child itself for a leaf).
    firsts: Row<*mut Leaf>,
}

/// A node's children, indexed and moved with bounds checks that abort
/// rather than panic: the table natives must not link std's panic hook
/// (B-2026-10-29-41, `tests/mir_panic_free_floor.rs`).
#[repr(transparent)]
struct Row<T: Copy>([T; FAN]);

fn overrun() -> ! {
    crate::fatal::write_stderr(
        b"fatal runtime error: a sorted table's node index is out of bounds\n",
    );
    std::process::abort()
}

impl<T: Copy> std::ops::Index<usize> for Row<T> {
    type Output = T;
    #[inline]
    fn index(&self, i: usize) -> &T {
        if i >= FAN {
            overrun()
        }
        // SAFETY: `i < FAN`.
        unsafe { self.0.get_unchecked(i) }
    }
}

impl<T: Copy> std::ops::IndexMut<usize> for Row<T> {
    #[inline]
    fn index_mut(&mut self, i: usize) -> &mut T {
        if i >= FAN {
            overrun()
        }
        // SAFETY: `i < FAN`.
        unsafe { self.0.get_unchecked_mut(i) }
    }
}

impl<T: Copy> Row<T> {
    /// Moves `n` children from `from` to `to` within the row.
    fn shift(&mut self, from: usize, to: usize, n: usize) {
        if from.max(to).saturating_add(n) > FAN {
            overrun()
        }
        // SAFETY: both runs are inside the row; `copy` allows the overlap.
        unsafe { std::ptr::copy(self.0.as_ptr().add(from), self.0.as_mut_ptr().add(to), n) }
    }

    /// Copies `n` children of `src` from `from` to `to` in this row.
    fn take(&mut self, to: usize, src: &Row<T>, from: usize, n: usize) {
        if to.max(from).saturating_add(n) > FAN {
            overrun()
        }
        // SAFETY: both runs are inside their rows, which are two nodes'.
        unsafe {
            std::ptr::copy_nonoverlapping(src.0.as_ptr().add(from), self.0.as_mut_ptr().add(to), n)
        }
    }
}

impl Row<usize> {
    /// The counts of the children in `from..to`.
    fn sum(&self, from: usize, to: usize) -> usize {
        let mut s = 0;
        for k in from..to {
            s += self[k];
        }
        s
    }
}

impl Inner {
    /// Moves `n` children from `from` to `to` within the node.
    fn shift(&mut self, from: usize, to: usize, n: usize) {
        self.kids.shift(from, to, n);
        self.counts.shift(from, to, n);
        self.firsts.shift(from, to, n);
    }

    /// Copies `n` children of `src` from `from` to `to` in this node.
    fn take(&mut self, to: usize, src: &Inner, from: usize, n: usize) {
        self.kids.take(to, &src.kids, from, n);
        self.counts.take(to, &src.counts, from, n);
        self.firsts.take(to, &src.firsts, from, n);
    }
}

/// The `k`th entry of the leaf `l` of `es`-byte entries.
fn entry(l: *mut Leaf, es: usize, k: usize) -> *mut u8 {
    // SAFETY: `k` is below the leaf's room at every caller.
    unsafe { l.cast::<u8>().add(LEAF_AT + k * es) }
}

pub(super) struct Tree {
    root: *mut u8,
    /// The levels of inner nodes: 0 while the root is a leaf.
    height: usize,
    len: usize,
    lcap: usize,
    es: usize,
    cmp: CmpFn,
    /// Every live leaf, by its `id`; a freed leaf's place is null and
    /// waits in `free_ids`.
    leaves: LeanVec<*mut Leaf>,
    free_ids: LeanVec<usize>,
    /// The leaf an index last reached and the index of its first entry, as
    /// `id << 32 | start` ([`NO_CURSOR`] for none): a walk in order asks
    /// for the next index, which is in that leaf or the next. One word, so
    /// a reader racing another reader of the same table sees a whole
    /// cursor or none. A change to the tree forgets it.
    cursor: AtomicU64,
}

/// Allocates `size` bytes, as the table's other blocks are.
fn alloc(size: usize) -> *mut u8 {
    karac_realloc_or_panic(std::ptr::null_mut(), size)
}

impl Tree {
    /// A tree of the `n` sorted `es`-byte entries at `buf`, which it copies.
    ///
    /// # Safety
    /// `buf` holds `n` entries in key order under `cmp`.
    pub(super) unsafe fn build(buf: *const u8, n: usize, es: usize, cmp: CmpFn) -> *mut Tree {
        let t = alloc(std::mem::size_of::<Tree>()).cast::<Tree>();
        let lcap = lcap_for(es);
        // SAFETY: a fresh allocation of a `Tree`'s size, aligned as
        // `malloc` aligns.
        unsafe {
            t.write(Tree {
                root: std::ptr::null_mut(),
                height: 0,
                len: n,
                lcap,
                es,
                cmp,
                leaves: LeanVec::new(),
                free_ids: LeanVec::new(),
                cursor: AtomicU64::new(NO_CURSOR),
            })
        };
        // SAFETY: as above.
        let tr = unsafe { &mut *t };
        // Leaves three quarters full, so the inserts that follow split few.
        let fill = (lcap * 3 / 4).max(1);
        let nl = n.div_ceil(fill).max(1);
        // (node, entries under it, its leftmost leaf)
        let mut level: LeanVec<(*mut u8, usize, *mut Leaf)> = LeanVec::with_capacity(nl);
        let mut prev: *mut Leaf = std::ptr::null_mut();
        let mut at = 0;
        for k in 0..nl {
            let take = n / nl + usize::from(k < n % nl);
            let l = tr.new_leaf();
            // SAFETY: `take` entries remain at `at`; the leaf has room for
            // `lcap >= fill >= take`.
            unsafe {
                std::ptr::copy_nonoverlapping(buf.add(at * es), entry(l, es, 0), take * es);
                (*l).len = take;
                (*l).prev = prev;
                if !prev.is_null() {
                    (*prev).next = l;
                }
            }
            prev = l;
            at += take;
            level.push((l.cast(), take, l));
        }
        // Inner nodes three quarters full, a level at a time.
        while level.len() > 1 {
            let kids = level.len();
            let g = FAN * 3 / 4;
            let nn = kids.div_ceil(g).max(1);
            let mut up: LeanVec<(*mut u8, usize, *mut Leaf)> = LeanVec::with_capacity(nn);
            let mut at = 0;
            for k in 0..nn {
                let take = kids / nn + usize::from(k < kids % nn);
                let p = new_inner();
                let mut total = 0;
                // SAFETY: a fresh inner node; `take <= g < FAN`.
                unsafe {
                    for (c, &(kid, count, first)) in (0..take).map(|c| (c, &level[at + c])) {
                        (&mut *p).kids[c] = kid;
                        (&mut *p).counts[c] = count;
                        (&mut *p).firsts[c] = first;
                        total += count;
                    }
                    (&mut *p).len = take;
                    up.push((p.cast(), total, (&mut *p).firsts[0]));
                }
                at += take;
            }
            level = up;
            tr.height += 1;
        }
        tr.root = level[0].0;
        t
    }

    fn new_leaf(&mut self) -> *mut Leaf {
        let l = alloc(LEAF_AT + self.lcap * self.es).cast::<Leaf>();
        let id = match self.free_ids.pop() {
            Some(id) => {
                self.leaves[id] = l;
                id
            }
            None => {
                self.leaves.push(l);
                self.leaves.len() - 1
            }
        };
        // SAFETY: a fresh allocation of more than a `Leaf`'s size.
        unsafe {
            l.write(Leaf {
                len: 0,
                next: std::ptr::null_mut(),
                prev: std::ptr::null_mut(),
                id,
            })
        };
        l
    }

    /// # Safety
    /// `l` is a live leaf of this tree that nothing names any more.
    unsafe fn free_leaf(&mut self, l: *mut Leaf) {
        // SAFETY: the caller's contract.
        let id = unsafe { (*l).id };
        self.leaves[id] = std::ptr::null_mut();
        self.free_ids.push(id);
        karac_free(l.cast());
    }

    /// Whether the root is a leaf: the table can be one array again.
    pub(super) fn is_one_leaf(&self) -> bool {
        self.height == 0
    }

    fn forget(&self) {
        self.cursor.store(NO_CURSOR, Relaxed);
    }

    fn remember(&self, l: *mut Leaf, start: usize) {
        // SAFETY: `l` is a live leaf.
        let id = unsafe { (*l).id };
        if id <= u32::MAX as usize && start <= u32::MAX as usize {
            self.cursor
                .store(((id as u64) << 32) | start as u64, Relaxed);
        }
    }

    /// The leftmost leaf under `node`, a node `level` levels above the
    /// leaves.
    ///
    /// # Safety
    /// `node` is a live node of this tree at that level.
    unsafe fn first(node: *mut u8, level: usize) -> *mut Leaf {
        if level == 0 {
            node.cast()
        } else {
            // SAFETY: the caller's contract.
            unsafe { (&*node.cast::<Inner>()).firsts[0] }
        }
    }

    /// The child of `p` whose keys `key` falls among: the last whose
    /// leftmost leaf's first key is at most `key`, or the first.
    ///
    /// # Safety
    /// `p` is a live inner node; `key` a key of the table's type.
    unsafe fn kid_for(&self, p: *const Inner, key: *const u8) -> usize {
        // SAFETY: the caller's contract; a leaf below an inner node is
        // never empty.
        unsafe {
            let (mut lo, mut hi) = (0, (&*p).len);
            while hi - lo > 1 {
                let mid = (lo + hi) / 2;
                if (self.cmp)(entry((&*p).firsts[mid], self.es, 0), key) <= 0 {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            lo
        }
    }

    /// The place of `key` in the leaf `l`: `Ok` where an entry has it,
    /// `Err` where it would go.
    ///
    /// # Safety
    /// `l` is a live leaf; `key` a key of the table's type.
    unsafe fn bisect(&self, l: *mut Leaf, key: *const u8) -> Result<usize, usize> {
        // SAFETY: the caller's contract.
        let (mut lo, mut hi) = (0, unsafe { (*l).len });
        while lo < hi {
            let mid = (lo + hi) / 2;
            // SAFETY: as above; `mid < len`.
            match unsafe { (self.cmp)(entry(l, self.es, mid), key) } {
                0 => return Ok(mid),
                o if o < 0 => lo = mid + 1,
                _ => hi = mid,
            }
        }
        Err(lo)
    }

    /// The index of the entry whose key equals `key` (`Ok`), or the index
    /// it would take (`Err`); and where that is, as a leaf and a place in
    /// it.
    ///
    /// # Safety
    /// `key` is a key of the table's type.
    pub(super) unsafe fn find(&self, key: *const u8) -> (Result<usize, usize>, *mut Leaf, usize) {
        let mut node = self.root;
        let mut rank = 0;
        for _ in 0..self.height {
            let p = node.cast::<Inner>();
            // SAFETY: a live inner node.
            unsafe {
                let j = self.kid_for(p, key);
                rank += (&mut *p).counts.sum(0, j);
                node = (&mut *p).kids[j];
            }
        }
        let l = node.cast::<Leaf>();
        // SAFETY: a live leaf.
        match unsafe { self.bisect(l, key) } {
            Ok(k) => (Ok(rank + k), l, k),
            Err(k) => (Err(rank + k), l, k),
        }
    }

    /// The entry whose key equals `key`, or null.
    ///
    /// # Safety
    /// As [`Tree::find`].
    pub(super) unsafe fn lookup(&self, key: *const u8) -> *mut u8 {
        // As `find`, without counting the entries before the leaf.
        let mut node = self.root;
        for _ in 0..self.height {
            let p = node.cast::<Inner>();
            // SAFETY: the caller's contract; a live inner node.
            unsafe { node = (&mut *p).kids[self.kid_for(p, key)] };
        }
        let l = node.cast::<Leaf>();
        // SAFETY: as above; a live leaf.
        match unsafe { self.bisect(l, key) } {
            Ok(k) => entry(l, self.es, k),
            Err(_) => std::ptr::null_mut(),
        }
    }

    /// The entry at index `i < len`: from the leaf the last index reached,
    /// or the one before or after it, else by the counts.
    pub(super) fn at(&self, i: usize) -> *mut u8 {
        let c = self.cursor.load(Relaxed);
        if c != NO_CURSOR {
            let (id, start) = ((c >> 32) as usize, (c & 0xFFFF_FFFF) as usize);
            let l = self.leaves[id];
            // SAFETY: a cursor names a live leaf until a change forgets it.
            unsafe {
                let n = (*l).len;
                if i.wrapping_sub(start) < n {
                    return entry(l, self.es, i - start);
                }
                let next = (*l).next;
                if i == start + n && !next.is_null() {
                    self.remember(next, i);
                    return entry(next, self.es, 0);
                }
                let prev = (*l).prev;
                if i + 1 == start && !prev.is_null() {
                    let pn = (*prev).len;
                    self.remember(prev, start - pn);
                    return entry(prev, self.es, pn - 1);
                }
            }
        }
        let (mut node, mut i, mut start) = (self.root, i, 0);
        for _ in 0..self.height {
            let p = node.cast::<Inner>();
            let mut j = 0;
            // SAFETY: a live inner node, whose counts sum past `i`.
            unsafe {
                while i >= (&mut *p).counts[j] {
                    i -= (&mut *p).counts[j];
                    start += (&mut *p).counts[j];
                    j += 1;
                }
                node = (&mut *p).kids[j];
            }
        }
        let l = node.cast::<Leaf>();
        self.remember(l, start);
        entry(l, self.es, i)
    }

    /// Adds the entry at `e`, whose key the table does not hold; returns
    /// its index.
    ///
    /// # Safety
    /// `e` is readable for an entry of the table's type.
    pub(super) unsafe fn insert(&mut self, e: *const u8) -> usize {
        self.forget();
        // SAFETY: the caller's contract.
        let (rank, split) = unsafe { self.ins(self.root, self.height, e) };
        if let Some((kid, count)) = split {
            let old = self.root;
            let p = new_inner();
            // SAFETY: a fresh inner node; `old` and `kid` are this level's.
            unsafe {
                (&mut *p).len = 2;
                (&mut *p).kids[0] = old;
                (&mut *p).kids[1] = kid;
                (&mut *p).counts[0] = self.len + 1 - count;
                (&mut *p).counts[1] = count;
                (&mut *p).firsts[0] = Self::first(old, self.height);
                (&mut *p).firsts[1] = Self::first(kid, self.height);
            }
            self.root = p.cast();
            self.height += 1;
        }
        self.len += 1;
        rank
    }

    /// Adds `e` under `node`, `level` levels above the leaves: its index
    /// among the entries under `node`, and the node `node` split off to
    /// its right, with the entries under that, if it split.
    ///
    /// # Safety
    /// As [`Tree::insert`]; `node` is a live node at `level`.
    unsafe fn ins(
        &mut self,
        node: *mut u8,
        level: usize,
        e: *const u8,
    ) -> (usize, Option<(*mut u8, usize)>) {
        if level == 0 {
            let l = node.cast::<Leaf>();
            // SAFETY: the caller's contract.
            let pos = unsafe { self.bisect(l, e) }.unwrap_or_else(|k| k);
            // SAFETY: as above.
            return unsafe { self.leaf_insert(l, pos, e) };
        }
        let p = node.cast::<Inner>();
        // SAFETY: the caller's contract.
        unsafe {
            let j = self.kid_for(p, e);
            let before = (&mut *p).counts.sum(0, j);
            let (r, split) = self.ins((&mut *p).kids[j], level - 1, e);
            match split {
                None => {
                    (&mut *p).counts[j] += 1;
                    (before + r, None)
                }
                Some((kid, count)) => {
                    (&mut *p).counts[j] = (&mut *p).counts[j] + 1 - count;
                    let first = Self::first(kid, level - 1);
                    (before + r, self.inner_insert(p, j + 1, kid, count, first))
                }
            }
        }
    }

    /// Puts `e` at `pos` in the leaf `l`, splitting it when full: rising
    /// keys past the last leaf's end start a new leaf and falling keys
    /// before the first leaf's start take the leaf's place, so a run of
    /// either fills leaves whole; any other split halves the leaf.
    ///
    /// # Safety
    /// `l` is a live leaf; `pos <= len`; `e` an entry.
    unsafe fn leaf_insert(
        &mut self,
        l: *mut Leaf,
        pos: usize,
        e: *const u8,
    ) -> (usize, Option<(*mut u8, usize)>) {
        let es = self.es;
        // SAFETY: the caller's contract; every move stays inside a leaf's
        // room of `lcap` entries.
        unsafe {
            let n = (*l).len;
            if n < self.lcap {
                std::ptr::copy(entry(l, es, pos), entry(l, es, pos + 1), (n - pos) * es);
                std::ptr::copy_nonoverlapping(e, entry(l, es, pos), es);
                (*l).len = n + 1;
                return (pos, None);
            }
            let r = self.new_leaf();
            let mid = if pos == n && (*l).next.is_null() {
                n
            } else if pos == 0 && (*l).prev.is_null() {
                0
            } else {
                n / 2
            };
            std::ptr::copy_nonoverlapping(entry(l, es, mid), entry(r, es, 0), (n - mid) * es);
            (*r).len = n - mid;
            (*l).len = mid;
            (*r).next = (*l).next;
            (*r).prev = l;
            if !(*l).next.is_null() {
                (*(*l).next).prev = r;
            }
            (*l).next = r;
            let (into, at) = if pos < mid || (pos == mid && mid < n) {
                (l, pos)
            } else {
                (r, pos - mid)
            };
            let m = (*into).len;
            std::ptr::copy(entry(into, es, at), entry(into, es, at + 1), (m - at) * es);
            std::ptr::copy_nonoverlapping(e, entry(into, es, at), es);
            (*into).len = m + 1;
            (pos, Some((r.cast(), (*r).len)))
        }
    }

    /// Puts the child `kid` (`count` entries, leftmost leaf `first`) at
    /// `at` in `p`, splitting `p` in half when full: the new right half and
    /// its entries, if it split.
    ///
    /// # Safety
    /// `p` is a live inner node; `at <= len`.
    unsafe fn inner_insert(
        &mut self,
        p: *mut Inner,
        at: usize,
        kid: *mut u8,
        count: usize,
        first: *mut Leaf,
    ) -> Option<(*mut u8, usize)> {
        // SAFETY: the caller's contract.
        unsafe {
            let n = (&mut *p).len;
            if n < FAN {
                put_kid(p, at, kid, count, first);
                return None;
            }
            let r = new_inner();
            let mid = n / 2;
            let m = n - mid;
            let (pm, rm) = (&mut *p, &mut *r);
            rm.take(0, pm, mid, m);
            rm.len = m;
            pm.len = mid;
            if at <= mid {
                put_kid(p, at, kid, count, first);
            } else {
                put_kid(r, at - mid, kid, count, first);
            }
            let rm = &*r;
            let total = rm.counts.sum(0, rm.len);
            Some((r.cast(), total))
        }
    }

    /// Moves the entry at index `i < len` to `out`.
    ///
    /// # Safety
    /// `out` is writable for an entry.
    pub(super) unsafe fn remove_at(&mut self, i: usize, out: *mut u8) {
        self.forget();
        // SAFETY: the caller's contract.
        unsafe { self.rm(self.root, self.height, i, out) };
        self.len -= 1;
        while self.height > 0 {
            let p = self.root.cast::<Inner>();
            // SAFETY: the root is a live inner node.
            unsafe {
                if (&mut *p).len != 1 {
                    break;
                }
                self.root = (&mut *p).kids[0];
                karac_free(p.cast());
            }
            self.height -= 1;
        }
    }

    /// # Safety
    /// As [`Tree::remove_at`]; `node` is a live node at `level` with more
    /// than `i` entries under it.
    unsafe fn rm(&mut self, node: *mut u8, level: usize, mut i: usize, out: *mut u8) {
        let es = self.es;
        // SAFETY: the caller's contract.
        unsafe {
            if level == 0 {
                let l = node.cast::<Leaf>();
                let n = (*l).len;
                std::ptr::copy_nonoverlapping(entry(l, es, i), out, es);
                std::ptr::copy(entry(l, es, i + 1), entry(l, es, i), (n - i - 1) * es);
                (*l).len = n - 1;
                return;
            }
            let p = node.cast::<Inner>();
            let mut j = 0;
            while i >= (&mut *p).counts[j] {
                i -= (&mut *p).counts[j];
                j += 1;
            }
            self.rm((&mut *p).kids[j], level - 1, i, out);
            (&mut *p).counts[j] -= 1;
            self.fix(p, j, level - 1);
        }
    }

    /// After a removal under the child `j` of `p` (a child at `level`):
    /// a child left short takes from a sibling, or merges with it when the
    /// two fit one node (the left one kept); and the leftmost leaves `p`
    /// records for them are refreshed.
    ///
    /// # Safety
    /// `p` is a live inner node and `j` one of its children.
    unsafe fn fix(&mut self, p: *mut Inner, j: usize, level: usize) {
        let es = self.es;
        // SAFETY: the caller's contract; the moves stay inside the rooms
        // of the two siblings, which together hold more than one node's
        // room when they do not merge.
        unsafe {
            let kid_len = |k: *mut u8| {
                if level == 0 {
                    (*k.cast::<Leaf>()).len
                } else {
                    (*k.cast::<Inner>()).len
                }
            };
            let (min, cap) = if level == 0 {
                ((self.lcap / 4).max(1), self.lcap)
            } else {
                (INNER_MIN, FAN)
            };
            if kid_len((&mut *p).kids[j]) >= min || (&mut *p).len < 2 {
                (&mut *p).firsts[j] = Self::first((&mut *p).kids[j], level);
                return;
            }
            let a = if j + 1 < (&mut *p).len { j } else { j - 1 };
            let b = a + 1;
            let (ka, kb) = ((&mut *p).kids[a], (&mut *p).kids[b]);
            let (na, nb) = (kid_len(ka), kid_len(kb));
            if na + nb <= cap {
                // Merge `b` into `a`.
                if level == 0 {
                    let (la, lb) = (ka.cast::<Leaf>(), kb.cast::<Leaf>());
                    std::ptr::copy_nonoverlapping(entry(lb, es, 0), entry(la, es, na), nb * es);
                    (*la).len = na + nb;
                    (*la).next = (*lb).next;
                    if !(*lb).next.is_null() {
                        (*(*lb).next).prev = la;
                    }
                    self.free_leaf(lb);
                } else {
                    let (ia, ib) = (&mut *ka.cast::<Inner>(), &mut *kb.cast::<Inner>());
                    ia.take(na, ib, 0, nb);
                    ia.len = na + nb;
                    karac_free((ib as *mut Inner).cast());
                }
                (&mut *p).counts[a] += (&mut *p).counts[b];
                take_kid(p, b);
                (&mut *p).firsts[a] = Self::first(ka, level);
                return;
            }
            // Even the two out.
            let want = (na + nb) / 2;
            let moved = if level == 0 {
                let (la, lb) = (ka.cast::<Leaf>(), kb.cast::<Leaf>());
                if na < want {
                    let k = want - na;
                    std::ptr::copy_nonoverlapping(entry(lb, es, 0), entry(la, es, na), k * es);
                    std::ptr::copy(entry(lb, es, k), entry(lb, es, 0), (nb - k) * es);
                    (*la).len = na + k;
                    (*lb).len = nb - k;
                    k as isize
                } else {
                    let k = na - want;
                    std::ptr::copy(entry(lb, es, 0), entry(lb, es, k), nb * es);
                    std::ptr::copy_nonoverlapping(entry(la, es, want), entry(lb, es, 0), k * es);
                    (*la).len = want;
                    (*lb).len = nb + k;
                    -(k as isize)
                }
            } else {
                let (ia, ib) = (&mut *ka.cast::<Inner>(), &mut *kb.cast::<Inner>());
                if na < want {
                    let k = want - na;
                    let c = ib.counts.sum(0, k);
                    ia.take(na, ib, 0, k);
                    ib.shift(k, 0, nb - k);
                    ia.len = want;
                    ib.len = nb - k;
                    c as isize
                } else {
                    let k = na - want;
                    let c = ia.counts.sum(want, na);
                    ib.shift(0, k, nb);
                    ib.take(0, ia, want, k);
                    ia.len = want;
                    ib.len = nb + k;
                    -(c as isize)
                }
            };
            (&mut *p).counts[a] = (&mut *p).counts[a].wrapping_add_signed(moved);
            (&mut *p).counts[b] = (&mut *p).counts[b].wrapping_add_signed(-moved);
            (&mut *p).firsts[a] = Self::first(ka, level);
            (&mut *p).firsts[b] = Self::first(kb, level);
        }
    }

    /// Frees every node, and the tree.
    ///
    /// # Safety
    /// `t` came from [`Tree::build`] and is not used after.
    pub(super) unsafe fn free(t: *mut Tree) {
        // SAFETY: the caller's contract.
        unsafe {
            let tr = &mut *t;
            for &l in tr.leaves.as_slice() {
                if !l.is_null() {
                    karac_free(l.cast());
                }
            }
            free_inners(tr.root, tr.height);
            std::ptr::drop_in_place(t);
            karac_free(t.cast());
        }
    }

    /// The root leaf's entries, while the root is a leaf: `(entries, len)`.
    pub(super) fn one_leaf(&self) -> (*const u8, usize) {
        let l = self.root.cast::<Leaf>();
        // SAFETY: the root is a live leaf at height 0.
        (entry(l, self.es, 0), unsafe { (*l).len })
    }

    #[cfg(test)]
    pub(super) fn height(&self) -> usize {
        self.height
    }
}

/// # Safety
/// `node` is a live node `level` levels above the leaves, freed with every
/// inner node under it (the leaves are freed by id).
unsafe fn free_inners(node: *mut u8, level: usize) {
    if level == 0 {
        return;
    }
    let p = node.cast::<Inner>();
    // SAFETY: the caller's contract.
    unsafe {
        for k in 0..(&mut *p).len {
            free_inners((&mut *p).kids[k], level - 1);
        }
    }
    karac_free(p.cast());
}

fn new_inner() -> *mut Inner {
    let p = alloc(std::mem::size_of::<Inner>()).cast::<Inner>();
    // SAFETY: a fresh allocation of an `Inner`'s size.
    unsafe {
        p.write(Inner {
            len: 0,
            kids: Row([std::ptr::null_mut(); FAN]),
            counts: Row([0; FAN]),
            firsts: Row([std::ptr::null_mut(); FAN]),
        })
    };
    p
}

/// Puts a child at `at` in `p`, which has room.
///
/// # Safety
/// `p` is a live inner node with fewer than [`FAN`] children; `at <= len`.
unsafe fn put_kid(p: *mut Inner, at: usize, kid: *mut u8, count: usize, first: *mut Leaf) {
    // SAFETY: the caller's contract.
    unsafe {
        let p = &mut *p;
        let n = p.len;
        p.shift(at, at + 1, n - at);
        p.kids[at] = kid;
        p.counts[at] = count;
        p.firsts[at] = first;
        p.len = n + 1;
    }
}

/// Takes the child at `at` out of `p`.
///
/// # Safety
/// `p` is a live inner node; `at < len`.
unsafe fn take_kid(p: *mut Inner, at: usize) {
    // SAFETY: the caller's contract.
    unsafe {
        let p = &mut *p;
        let n = p.len;
        p.shift(at + 1, at, n - at - 1);
        p.len = n - 1;
    }
}

#[cfg(test)]
mod tests {
    use super::super::*;
    use super::lcap_for;

    unsafe extern "C" fn cmp(a: *const u8, b: *const u8) -> i32 {
        // SAFETY: the keys are `i64`s.
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

    struct T {
        h: *mut MirMap,
        es: usize,
    }

    impl T {
        fn insert(&mut self, k: i64) -> i64 {
            let e = entry(k, self.es);
            // SAFETY: a live (or null) table of `es`-byte entries.
            unsafe {
                karac_mir_map_insert(&mut self.h, self.es as u64, e.as_ptr(), None, Some(cmp))
            }
        }
        fn find(&self, k: i64) -> i64 {
            // SAFETY: as above.
            unsafe { karac_mir_map_find(self.h, (&k as *const i64).cast(), None, None, Some(cmp)) }
        }
        fn at(&self, i: usize) -> Vec<u8> {
            // SAFETY: as above, with more than `i` entries.
            unsafe {
                std::slice::from_raw_parts(karac_mir_map_at(self.h, i as i64), self.es).to_vec()
            }
        }
        fn pick(&self, k: i64, op: i64) -> i64 {
            // SAFETY: as above.
            unsafe { karac_mir_map_pick(self.h, (&k as *const i64).cast(), cmp, true, op) }
        }
        fn in_tree(&self) -> bool {
            // SAFETY: as above.
            !self.h.is_null() && unsafe { !(*self.h).tree.is_null() }
        }
    }

    /// The table matches a `Vec` model, read every way compiled code reads
    /// it: by key, by index in order, backwards and at random, and through
    /// `pick`, as it grows into a tree, loses entries back into one array,
    /// and is cleared; with falling and rising runs, which split leaves
    /// unevenly (B-2026-11-10-39).
    #[test]
    fn a_sorted_table_in_a_tree_matches_a_model() {
        for es in [8usize, 16, 40, 200] {
            let mut t = T {
                h: std::ptr::null_mut(),
                es,
            };
            let mut model: Vec<i64> = Vec::new();
            let mut rng = 0x9E37_79B9u64 ^ es as u64;
            let mut next = || {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                rng
            };
            let (mut treed, mut left) = (false, false);
            let check = |t: &T, model: &[i64], full: bool, r: u64| {
                // SAFETY: a live table.
                assert_eq!(unsafe { karac_mir_map_len(t.h) } as usize, model.len());
                let n = model.len();
                if full {
                    for (i, k) in model.iter().enumerate() {
                        assert_eq!(t.at(i), entry(*k, es), "es {es} at {i}");
                    }
                    for (i, k) in model.iter().enumerate().rev() {
                        assert_eq!(t.at(i), entry(*k, es), "es {es} back at {i}");
                    }
                }
                for q in 0..8u64 {
                    if n == 0 {
                        break;
                    }
                    let i = (r.rotate_left(q as u32 * 7) as usize) % n;
                    assert_eq!(t.at(i), entry(model[i], es));
                    assert_eq!(t.find(model[i]), i as i64);
                    let probe = model[i] + 1;
                    let lt = model.partition_point(|&k| k < probe);
                    let le = model.partition_point(|&k| k <= probe);
                    assert_eq!(t.pick(probe, 4), lt as i64);
                    assert_eq!(t.pick(probe, 5), le as i64);
                    assert_eq!(t.pick(probe, 2), le as i64 - 1);
                    let ceil = if lt < n { lt as i64 } else { -1 };
                    assert_eq!(t.pick(probe, 3), ceil);
                    if !model.contains(&probe) {
                        assert_eq!(t.find(probe), -1);
                    }
                }
                if n > 0 {
                    assert_eq!(t.pick(0, 0), 0);
                    assert_eq!(t.pick(0, 1), n as i64 - 1);
                }
            };
            for round in 0..2 {
                // Falling keys, then rising ones past them, then a mix.
                let base = 1_000_000i64;
                for k in 0..3000 {
                    let key = base - 2 * k;
                    let i = t.insert(key);
                    let at = model.partition_point(|&x| x < key);
                    assert_eq!(i as usize, at);
                    model.insert(at, key);
                    treed |= t.in_tree();
                }
                for k in 0..3000 {
                    let key = base + 1 + 2 * k;
                    let i = t.insert(key);
                    assert_eq!(i as usize, model.len());
                    model.push(key);
                }
                check(&t, &model, true, next());
                for step in 0..20_000 {
                    let r = next();
                    let n = model.len();
                    if n > 0 && (r % 9 < 4 || (round == 1 && r % 9 < 6)) {
                        let i = (r >> 8) as usize % n;
                        let mut out = vec![0u8; es];
                        if r % 2 == 0 {
                            // SAFETY: a live table with more than `i` entries.
                            unsafe { karac_mir_map_remove(t.h, i as i64, out.as_mut_ptr()) };
                        } else {
                            let k = model[i];
                            // SAFETY: as above; the lookup's entry is live.
                            unsafe {
                                let at = karac_mir_map_lookup(
                                    t.h,
                                    (&k as *const i64).cast(),
                                    None,
                                    None,
                                    Some(cmp),
                                );
                                assert!(!at.is_null());
                                karac_mir_map_remove_entry(t.h, at, out.as_mut_ptr());
                            }
                        }
                        assert_eq!(out, entry(model.remove(i), es));
                    } else {
                        let key = (r >> 12) as i64 % 2_000_000;
                        if model.binary_search(&key).is_err() {
                            let i = t.insert(key);
                            let at = model.partition_point(|&x| x < key);
                            assert_eq!(i as usize, at);
                            model.insert(at, key);
                        }
                    }
                    treed |= t.in_tree();
                    left |= treed && !t.in_tree();
                    if step % 997 == 0 {
                        check(&t, &model, step % 4 == 0, r);
                    }
                }
                check(&t, &model, true, next());
                // Rising keys in among the low ones, so the left of the tree
                // grows its nodes past half full; then down to a handful
                // from the front (round 0) or the back (round 1), which
                // short the nodes at that end against full siblings.
                let low = model.first().copied().unwrap_or(0);
                for k in 0..4000 {
                    let key = low - 1 - 2 * k;
                    if model.binary_search(&key).is_err() {
                        t.insert(key);
                        model.insert(model.partition_point(|&x| x < key), key);
                    }
                    let key = low - 1 - 2 * (4000 - k) + 1;
                    if model.binary_search(&key).is_err() {
                        t.insert(key);
                        model.insert(model.partition_point(|&x| x < key), key);
                    }
                }
                check(&t, &model, true, next());
                while model.len() > 3 {
                    let i = if round == 0 { 0 } else { model.len() - 1 };
                    let mut out = vec![0u8; es];
                    // SAFETY: a live table with an entry `i`.
                    unsafe { karac_mir_map_remove(t.h, i as i64, out.as_mut_ptr()) };
                    assert_eq!(out, entry(model.remove(i), es));
                    if model.len().is_multiple_of(1500) {
                        check(&t, &model, true, next());
                    }
                }
                check(&t, &model, true, next());
                assert!(!t.in_tree());
                // SAFETY: a live table.
                unsafe { karac_mir_map_clear(t.h) };
                model.clear();
                check(&t, &model, true, next());
            }
            assert!(
                treed && left,
                "es {es}: the table never went into a tree and back"
            );
            // SAFETY: as above.
            unsafe { karac_mir_map_free(t.h) };
        }
    }

    /// A falling run fills its leaves whole and keeps the tree shallow: an
    /// insert at the front moves one leaf's entries at most.
    #[test]
    fn a_falling_run_fills_leaves() {
        let mut t = T {
            h: std::ptr::null_mut(),
            es: 16,
        };
        for k in 0..200_000i64 {
            assert_eq!(t.insert(-k), 0);
        }
        // SAFETY: a live table in a tree.
        let tr = unsafe { &*(*t.h).tree };
        let lcap = lcap_for(16);
        let live = tr.leaves.as_slice().iter().filter(|l| !l.is_null()).count();
        assert!(
            live * lcap <= 200_000 + 2 * lcap * 4,
            "{live} leaves of {lcap}"
        );
        assert!(tr.height() <= 3, "height {}", tr.height());
        for i in 0..200_000usize {
            assert_eq!(t.at(i), entry(i as i64 - 199_999, 16));
        }
        // SAFETY: as above.
        unsafe { karac_mir_map_free(t.h) };
    }
}
