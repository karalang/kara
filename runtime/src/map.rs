//! Open-addressing hash map for compiled Kāra programs.
//!
//! ## Safety — the shared C-ABI contract
//!
//! Every `karac_map_*` entry point below is called from compiler-emitted code,
//! and unless its own `# Safety` section says otherwise it requires:
//!
//! * `map` is a live pointer from `karac_map_new` for THIS key/val layout,
//!   not yet passed to any `karac_map_free*` — only the free fns tolerate
//!   null; the access fns dereference unconditionally.
//! * `key` / `val` point to readable blobs of exactly the `key_size` /
//!   `val_size` the map was constructed with; `out_*` pointers are writable
//!   at the same widths.
//! * No concurrent access: the map is single-owner state, never shared across
//!   Kāra tasks without the compiler proving exclusivity (effect system).
//! * The constructed `hash_fn` / `eq_fn` stay valid for the map's lifetime
//!   and agree with each other (eq ⇒ equal hashes).
//!
//!
//! The map is type-erased: keys and values are raw byte blobs. The compiler
//! passes `key_size`/`val_size` at construction time and emits concrete
//! `hash_fn`/`eq_fn` function pointers for the monomorphised K type.
//!
//! Layout: two parallel heap allocations —
//!   `status[capacity]`       — u8 control byte per bucket (see below)
//!   `kv[capacity*(ks+vs)]`   — packed (key, val) pairs, no alignment padding
//!
//! Collision resolution: linear probing. Load factor ceiling 3/4; resize
//! doubles capacity and rehashes, or compacts at the same width when the LIVE
//! count leaves enough headroom (see `next_capacity`). Deletion marks a
//! tombstone only when a probe chain can still run past the bucket — when the
//! next bucket is already EMPTY the slot is released outright instead (see
//! `vacate`). Tombstones count toward the load factor, so a tombstone-heavy
//! table still triggers a resize.
//!
//! ## The control byte (B-2026-07-26-2)
//!
//! An occupied bucket's status byte carries a 7-bit fragment of its key's hash,
//! hashbrown-style, rather than a bare "occupied" flag:
//!
//! ```text
//!   0x00           EMPTY
//!   0x01           TOMBSTONE
//!   0x80 | tag7    OCCUPIED, tag7 = bits 57..63 of the key's hash
//! ```
//!
//! Both sentinels are `< 0x80` and every occupied byte is `>= 0x80`, so "is
//! this bucket occupied" is one high-bit test and a control byte can never be
//! confused with a sentinel.
//!
//! The point is what the probe no longer has to do. With a bare flag, every
//! occupied slot on the probe chain called `eq_fn`, which for a `String` key
//! dereferences a `{ptr,len,cap}` header and then a scattered heap buffer.
//! `karac_eq_String` was measured at 5,988,829 of the #127 Word Ladder kata's
//! 8,483,251 L1-D read misses — 70.6%, more than Rust's entire program at
//! 3,594,102 — while running only 1.09 times per lookup. The cost was never
//! long probe chains; it was one cold key dereference per probed slot. Matching
//! the control byte first rejects a non-matching key without touching it, so
//! `eq_fn` runs on a real hit or a ~1-in-128 tag collision.
//!
//! The tag comes from the HIGH bits because the bucket index takes the low ones
//! (`hash & (capacity - 1)`) — a tag drawn from the same bits the index uses
//! would be constant along a probe chain and reject nothing.
//!
//! **This encoding is a codegen ABI**, not a runtime-private detail: the
//! monomorphized Map/Set probes in `src/codegen/mono.rs` emit their own copies
//! of this loop, and `src/codegen/control_flow_for.rs` / `src/codegen/runtime.rs`
//! walk occupied slots directly. All of them must agree with the constants and
//! helpers below. The failure mode of disagreement is a lookup that misses a
//! present key — a silent wrong answer, not a crash — so `ctrl_of` and
//! `is_occupied` are the single source of truth on this side and
//! `Codegen::emit_map_ctrl_of` / `emit_map_is_occupied` are their mirrors.

use std::alloc::{alloc, dealloc, Layout};
use std::ffi::c_void;
use std::ptr;

extern "C" {
    /// libc `free`, used to release per-entry `Vec[T]` data buffers in
    /// `karac_map_free_with_val_drop_vec`. The codegen-side Vec.push path
    /// allocates the same buffers via libc `malloc` (see
    /// `Codegen::malloc_fn`), so pairing with libc `free` is the matching
    /// alloc/free pair. We avoid `std::alloc::dealloc` here because that
    /// path requires reconstructing the original `Layout`, which the
    /// codegen-emitted Vec.push doesn't record per-buffer.
    fn free(ptr: *mut c_void);
}

const INITIAL_CAPACITY: usize = 16;
/// Vacant and never used — the probe stops here. Must stay 0 so freshly
/// allocated storage reads as empty after a single `write_bytes(_, 0, _)`.
const BUCKET_EMPTY: u8 = 0;
/// Vacated by a `remove` — the probe skips it but must keep going, since a key
/// inserted before the deletion may live further along the chain.
const BUCKET_TOMBSTONE: u8 = 1;
/// Set in every occupied control byte; the low 7 bits hold the hash tag. Also
/// the threshold: `byte >= BUCKET_OCCUPIED_BIT` iff the bucket is occupied.
const BUCKET_OCCUPIED_BIT: u8 = 0x80;

/// The control byte for a bucket holding a key with this hash — see the module
/// header. Takes the TOP 7 bits, because the bucket index consumes the bottom
/// ones and a tag sharing them would be invariant along a probe chain.
#[inline(always)]
fn ctrl_of(hash: u64) -> u8 {
    BUCKET_OCCUPIED_BIT | ((hash >> 57) as u8 & 0x7f)
}

/// Is this control byte an occupied bucket (rather than EMPTY or TOMBSTONE)?
#[inline(always)]
fn is_occupied(status: u8) -> bool {
    status >= BUCKET_OCCUPIED_BIT
}

/// `#[repr(C)]` is load-bearing — codegen-side monomorphized
/// `Map[K, V]` symbols (`src/codegen.rs`, see
/// [`wip-monomorphized-collections.md`](../../docs/implementation_checklist/wip-monomorphized-collections.md))
/// load the `len` / `capacity` / `status` / `kv` fields by direct
/// GEP + load against this layout. Reordering or inserting fields
/// here is an ABI break against codegen; the offsets are pinned by
/// the `karac_map_field_offsets_match_codegen` unit test below.
/// Slice 5's atomic delete of the erased runtime will untether
/// codegen from this layout.
#[repr(C)]
struct KaracMap {
    status: *mut u8,
    kv: *mut u8,
    capacity: usize,
    len: usize,
    tombstones: usize,
    key_size: usize,
    val_size: usize,
    hash_fn: unsafe extern "C" fn(*const c_void) -> u64,
    eq_fn: unsafe extern "C" fn(*const c_void, *const c_void) -> bool,
}

// Maps are local to a single thread; the compiler never moves them across
// thread boundaries without going through Arc/Mutex at a higher level.
unsafe impl Send for KaracMap {}

impl KaracMap {
    unsafe fn new(
        key_size: usize,
        val_size: usize,
        hash_fn: unsafe extern "C" fn(*const c_void) -> u64,
        eq_fn: unsafe extern "C" fn(*const c_void, *const c_void) -> bool,
    ) -> *mut Self {
        unsafe {
            let (status, kv) = Self::alloc_storage(INITIAL_CAPACITY, key_size, val_size);
            let map = Box::new(KaracMap {
                status,
                kv,
                capacity: INITIAL_CAPACITY,
                len: 0,
                tombstones: 0,
                key_size,
                val_size,
                hash_fn,
                eq_fn,
            });
            Box::into_raw(map)
        }
    }

    unsafe fn alloc_storage(
        capacity: usize,
        key_size: usize,
        val_size: usize,
    ) -> (*mut u8, *mut u8) {
        unsafe {
            let status_layout = Layout::array::<u8>(capacity).unwrap();
            let status = alloc(status_layout);
            ptr::write_bytes(status, BUCKET_EMPTY, capacity);

            let kv_size = (key_size + val_size).max(1);
            let kv_layout = Layout::array::<u8>(capacity * kv_size).unwrap();
            let kv = alloc(kv_layout);

            (status, kv)
        }
    }

    unsafe fn free_storage(&mut self) {
        unsafe {
            let status_layout = Layout::array::<u8>(self.capacity).unwrap();
            dealloc(self.status, status_layout);

            let kv_size = (self.key_size + self.val_size).max(1);
            let kv_layout = Layout::array::<u8>(self.capacity * kv_size).unwrap();
            dealloc(self.kv, kv_layout);
        }
    }

    #[inline]
    unsafe fn key_ptr(&self, slot: usize) -> *const c_void {
        unsafe { self.kv.add(slot * (self.key_size + self.val_size)) as *const c_void }
    }

    #[inline]
    unsafe fn val_ptr(&self, slot: usize) -> *const c_void {
        unsafe {
            self.kv
                .add(slot * (self.key_size + self.val_size) + self.key_size)
                as *const c_void
        }
    }

    /// Free the heap `{ptr, len, cap}` buffer whose 24-byte header starts at
    /// `base`, when its `cap > 0` and data pointer is non-null. The canonical
    /// "release one stored Vec/String field" primitive, shared by
    /// `karac_map_free_with_drop_vec` (live-slot walk) and the `remove` /
    /// `remove_old` tombstone paths (which must release the bucket's STORED
    /// key/value the tombstone would otherwise orphan). The codegen-side
    /// `drop_key` / `drop_val` flag asserts the field at `base` follows the
    /// Vec/String layout (offset 0: 8-byte data ptr; offset 16: 8-byte cap) —
    /// never call this on a scalar field.
    #[inline]
    unsafe fn free_heap_field(base: *const u8) {
        unsafe {
            let data_ptr = ptr::read_unaligned(base as *const *mut u8);
            let cap = ptr::read_unaligned(base.add(16) as *const i64);
            if cap > 0 && !data_ptr.is_null() {
                free(data_ptr as *mut c_void);
            }
        }
    }

    /// Release the bucket's STORED key buffer at `slot` (see
    /// `free_heap_field`). Caller must have established the key type is a heap
    /// `{ptr,len,cap}` (codegen `drop_key != 0`).
    #[inline]
    unsafe fn free_stored_key(&self, slot: usize) {
        unsafe {
            Self::free_heap_field(self.key_ptr(slot) as *const u8);
        }
    }

    /// Release the bucket's STORED value buffer at `slot` (see
    /// `free_heap_field`). Caller must have established the value type is a
    /// heap `{ptr,len,cap}` (codegen `drop_val != 0`). NOT used by
    /// `karac_map_remove_old`, which MOVES the value out to the caller.
    #[inline]
    unsafe fn free_stored_val(&self, slot: usize) {
        unsafe {
            Self::free_heap_field(self.val_ptr(slot) as *const u8);
        }
    }

    // Find an occupied slot holding `key`. Returns Some(slot) or None.
    unsafe fn lookup(&self, key: *const c_void) -> Option<usize> {
        unsafe {
            let hash = (self.hash_fn)(key);
            let ctrl = ctrl_of(hash);
            let start = (hash as usize) & (self.capacity - 1);
            for i in 0..self.capacity {
                let slot = (start + i) & (self.capacity - 1);
                let s = *self.status.add(slot);
                if s == BUCKET_EMPTY {
                    return None;
                }
                // The tag test rejects a non-matching key WITHOUT dereferencing it,
                // which is the whole point of the control byte — see the module
                // header. It also subsumes the occupancy test: `ctrl >= 0x80` and
                // both sentinels are below it, so a sentinel can never compare
                // equal. Past it, `eq_fn` runs only on a real hit or a ~1/128 tag
                // collision.
                if s == ctrl && (self.eq_fn)(self.key_ptr(slot), key) {
                    return Some(slot);
                }
            }
            None
        }
    }

    // Find the slot to write a new key into. Returns the slot, whether the key
    // already exists (update vs. fresh insert), and the control byte the caller
    // must store — callers cannot derive it themselves without re-hashing.
    unsafe fn find_insert_slot(&self, key: *const c_void) -> (usize, bool, u8) {
        unsafe {
            let hash = (self.hash_fn)(key);
            let ctrl = ctrl_of(hash);
            let start = (hash as usize) & (self.capacity - 1);
            let mut first_tombstone: Option<usize> = None;
            for i in 0..self.capacity {
                let slot = (start + i) & (self.capacity - 1);
                let s = *self.status.add(slot);
                if s == BUCKET_EMPTY {
                    let target = first_tombstone.unwrap_or(slot);
                    return (target, false, ctrl);
                }
                if s == BUCKET_TOMBSTONE {
                    if first_tombstone.is_none() {
                        first_tombstone = Some(slot);
                    }
                    continue;
                }
                // Occupied: same tag-first test as `lookup`.
                if s == ctrl && (self.eq_fn)(self.key_ptr(slot), key) {
                    return (slot, true, ctrl);
                }
            }
            // Should not reach here if resize policy is respected.
            (first_tombstone.unwrap_or(0), false, ctrl)
        }
    }

    unsafe fn insert(&mut self, key: *const c_void, val: *const c_void) {
        unsafe {
            // Resize when (occupied + tombstones) / capacity > 3/4.
            //
            // This bound is LOAD-BEARING beyond load factor: it is what leaves at
            // least a quarter of the buckets EMPTY, which is the termination proof
            // for a linear probe that has no trip counter. Codegen's
            // `MapLookupProbe::Unbounded` / `SlotWalk` forms (B-2026-08-07-16,
            // `KARAC_MAP_PROBE`, off by default) drop their `i >= cap` test and
            // rely on it. Weaken the fraction toward 1 and those forms spin
            // forever on a miss — see that enum before changing it.
            if (self.len + self.tombstones + 1) * 4 > self.capacity * 3 {
                self.resize();
            }
            let (slot, exists, ctrl) = self.find_insert_slot(key);
            let was_tombstone = *self.status.add(slot) == BUCKET_TOMBSTONE;
            let kv_offset = slot * (self.key_size + self.val_size);
            if !exists {
                ptr::copy_nonoverlapping(key as *const u8, self.kv.add(kv_offset), self.key_size);
                self.len += 1;
                if was_tombstone {
                    self.tombstones -= 1;
                }
            }
            ptr::copy_nonoverlapping(
                val as *const u8,
                self.kv.add(kv_offset + self.key_size),
                self.val_size,
            );
            *self.status.add(slot) = ctrl;
        }
    }

    unsafe fn get(&self, key: *const c_void, out_val: *mut c_void) -> bool {
        unsafe {
            if let Some(slot) = self.lookup(key) {
                ptr::copy_nonoverlapping(
                    self.val_ptr(slot) as *const u8,
                    out_val as *mut u8,
                    self.val_size,
                );
                true
            } else {
                false
            }
        }
    }

    unsafe fn remove(&mut self, key: *const c_void, drop_key: bool, drop_val: bool) -> bool {
        unsafe {
            if let Some(slot) = self.lookup(key) {
                // The bool `remove` discards both halves, so free each heap
                // `{ptr,len,cap}` the tombstone would orphan. `free-with-drop`
                // only walks OCCUPIED slots, so a tombstoned buffer leaks
                // otherwise. (The `remove_old` variant instead MOVES the value
                // out to the caller and frees only the key.)
                if drop_key {
                    self.free_stored_key(slot);
                }
                if drop_val {
                    self.free_stored_val(slot);
                }
                self.vacate(slot);
                true
            } else {
                false
            }
        }
    }

    /// Release `slot`, whose key has just been removed: drop the live count and
    /// leave a status byte that keeps every probe chain through this bucket
    /// correct. The single "this bucket's key is gone" primitive — shared by
    /// [`Self::remove`] and `karac_map_remove_old` (the one codegen actually
    /// lowers `Map.remove` / `Set.remove` to), which otherwise open-coded the
    /// same three lines.
    ///
    /// A tombstone is the CONSERVATIVE marking, not the only correct one. It
    /// exists to keep a probe going, because a key hashed to an earlier bucket
    /// may live further along the chain that ran through this slot. But when the
    /// NEXT bucket is already `BUCKET_EMPTY`, no chain continues past this slot
    /// in the first place: any probe that reached here would stop one step later
    /// regardless. So the slot can go straight back to `BUCKET_EMPTY`, and the
    /// tombstone that would have sat there — lengthening every probe through
    /// this bucket for the rest of the table's life — never exists.
    ///
    /// B-2026-08-05-4 — the second, independent half of that fix.
    /// [`Self::next_capacity`]'s ³⁄₁₆ band made the compacting rehash rarer and
    /// recovered the regression on its own; this stops most of the tombstones
    /// that drive the rehash from being created at all. The two STACK rather
    /// than subsume each other — compaction re-hash work on the sliding-window
    /// churn test below, over both axes:
    ///
    /// ```text
    ///                     vacate off   vacate on
    ///     ⅜  band           15.52%       7.06%
    ///     ³⁄₁₆ band           6.29%       1.92%
    /// ```
    ///
    /// On kata:146 (32M-op LRU, 1024 live, key range 4096) the wall-time goes
    /// 238.3 ms → 223.3 ms, landing 6% BELOW the pre-regression baseline of
    /// 237.2 ms: lookups stop walking tombstone runs, which is a win no capacity
    /// policy can buy.
    ///
    /// **No backward collapse.** The same argument extends to the tombstone run
    /// *behind* this slot — each becomes a run end with an EMPTY successor once
    /// its successor is cleared — and that version was built and measured. It is
    /// WORSE: 289.7 ms on kata:146, 30% slower than the one-slot rule. Two
    /// reasons, and the second is the interesting one. The walk is a backward
    /// dependent-load chain the prefetcher cannot follow; and clearing that
    /// aggressively holds the table one doubling SMALLER (4096 buckets rather
    /// than 8192 on kata:146), which makes it denser, which makes a removed
    /// slot's successor EMPTY less often — so the rule undercuts its own
    /// precondition and settles at MORE tombstones than the cheap version.
    /// Cheap and local wins outright here; do not "improve" this into the walk
    /// without re-measuring.
    ///
    /// Cost when the successor is not EMPTY: one status-byte load, on the cache
    /// line the write is about to touch anyway, and a branchless select — the
    /// test is close to a coin flip in steady state, so this deliberately does
    /// not branch on it.
    #[inline]
    unsafe fn vacate(&mut self, slot: usize) {
        unsafe {
            let mask = self.capacity - 1;
            self.len -= 1;
            let run_end = *self.status.add((slot + 1) & mask) == BUCKET_EMPTY;
            *self.status.add(slot) = if run_end {
                BUCKET_EMPTY
            } else {
                BUCKET_TOMBSTONE
            };
            self.tombstones += usize::from(!run_end);
        }
    }

    /// Pick the next table width when the load-factor trigger fires.
    ///
    /// B-2026-07-31-21 — unconditionally doubling made capacity O(TOTAL
    /// removals): the trigger counts tombstones, `remove` only ever adds
    /// them, and no path dropped them without also widening. A sliding-window
    /// map with DISTINCT keys (live size pinned at 1024) measured 297 MB at
    /// 16M ops where Rust's HashMap holds 2.4 MB flat. When the LIVE count is
    /// small, rehash at the SAME capacity instead — `rehash_from` drops every
    /// tombstone, which is all a churn-dominated table needs.
    ///
    /// The same-width branch requires HEADROOM, not merely "the live set
    /// fits": compacting to a table that is still near the ¾ trigger would
    /// re-fire it after a handful of inserts, degenerating to an O(len)
    /// rehash per insert right at the boundary.
    ///
    /// B-2026-08-05-4 — the band that headroom argument picked was ⅜, and ⅜
    /// is too tight. Write the steady state out: a churning table compacted
    /// at width `C` with live `L` accepts `0.75·C − L` more tombstones before
    /// the trigger re-fires, and each compaction costs `C` bucket scans plus
    /// `L` key re-hashes (the per-key `hash_fn` call `rehash_from` cannot
    /// skip). At the ⅜ edge that is `L / (0.75·C − L)` = **1.00 key re-hashes
    /// per operation** — the entire live set re-hashed once per op, amortized.
    /// That is not amortization at all, and it is what the original comment
    /// got wrong in claiming the band bought "the same amortization doubling
    /// provides": doubling pays `2C + L` once and then the WIDER table makes
    /// the next trigger geometrically rarer, where same-width compaction
    /// re-fires at the same cadence forever.
    ///
    /// Halving the band to ³⁄₁₆ drops that to 0.33 key re-hashes per op — 3×
    /// less — for one doubling of steady-state width (≈5.3·live instead of
    /// ≈2.67·live), which measured +8% peak RSS on a 1024-live sliding window,
    /// nowhere near the unbounded growth B-2026-07-31-21 fixed. ³⁄₃₂ would buy
    /// only 0.33 → 0.14 for another doubling of width, and measured slower —
    /// the wider table stops being cache-resident. ³⁄₁₆ is the knee.
    fn next_capacity(&self) -> usize {
        if (self.len + 1) * 16 <= self.capacity * 3 {
            self.capacity
        } else {
            self.capacity * 2
        }
    }

    unsafe fn resize(&mut self) {
        unsafe {
            let new_cap = self.next_capacity();
            let (new_status, new_kv) = Self::alloc_storage(new_cap, self.key_size, self.val_size);

            let old_status = self.status;
            let old_kv = self.kv;
            let old_cap = self.capacity;

            self.status = new_status;
            self.kv = new_kv;
            self.capacity = new_cap;
            self.len = 0;
            self.tombstones = 0;

            self.rehash_from(old_status, old_kv, old_cap);

            let status_layout = Layout::array::<u8>(old_cap).unwrap();
            dealloc(old_status, status_layout);
            let kv_layout =
                Layout::array::<u8>(old_cap * (self.key_size + self.val_size).max(1)).unwrap();
            dealloc(old_kv, kv_layout);
        }
    }

    /// Move every occupied bucket of a just-replaced table into `self`'s fresh
    /// storage. Shared by [`resize`] and [`try_resize`].
    ///
    /// This deliberately does NOT go through [`insert`] (B-2026-07-26-2), which
    /// is what it used to do. Three of `insert`'s steps are dead weight here,
    /// and rehashing is 72% of the cost of building a map — measured on a
    /// 3,125-key `Map[String, i64]` built 60 times, growth accounted for 21.5ms
    /// of kara's 29.7ms insert total against Rust's 6.8ms of 11.1ms:
    ///
    /// * **The load-factor test cannot fire.** `resize` either doubled the
    ///   capacity (replaying at most `3/4 * old_cap == 3/8 * new_cap` live keys)
    ///   or compacted at the same width (which `next_capacity` only allows when
    ///   the live count is at most `3/8 * cap`), so the
    ///   `(len + tombstones + 1) * 4 > capacity * 3` guard is false at every
    ///   step. Re-evaluating it per key is a branch on a value that cannot move.
    /// * **`find_insert_slot`'s tombstone bookkeeping and `eq_fn` call site are
    ///   unreachable.** The destination came straight from `alloc_storage`, so
    ///   every status byte is `BUCKET_EMPTY`: the probe stops at the first slot
    ///   it touches and the tombstone / occupied arms never run. Keys are also
    ///   unique by construction (they were unique in the old
    ///   table and rehashing preserves that), so no equality test is *needed*
    ///   even in principle — the erased `eq_fn` is an indirect call the
    ///   optimizer cannot see through, and this removes its call site entirely.
    /// * **Two runtime-sized `copy_nonoverlapping`s become one.** `insert`
    ///   copies the key and the value separately because they arrive as two
    ///   caller pointers; here they are already adjacent in the old bucket, so a
    ///   single `key_size + val_size` copy moves both. With non-constant sizes
    ///   each of those lowers to a libc `memcpy` call, so halving the count is a
    ///   real saving — `memcpy` was 7.9% of the profile.
    ///
    /// The one thing that cannot be skipped is the per-key `hash_fn` call: the
    /// bucket layout stores no hash, so a wider table has to re-derive it.
    ///
    /// # Safety
    /// `old_status` / `old_kv` must be the storage arrays of a table with
    /// `old_cap` buckets and the same `key_size` / `val_size` as `self`, and
    /// `self`'s own storage must be freshly allocated (all-`BUCKET_EMPTY`) with
    /// capacity strictly greater than the live count in the old table.
    unsafe fn rehash_from(&mut self, old_status: *const u8, old_kv: *const u8, old_cap: usize) {
        unsafe {
            let kv_size = self.key_size + self.val_size;
            let mask = self.capacity - 1;
            for i in 0..old_cap {
                if !is_occupied(*old_status.add(i)) {
                    continue;
                }
                let src = old_kv.add(i * kv_size);
                let hash = (self.hash_fn)(src as *const c_void);
                let mut slot = (hash as usize) & mask;
                // Terminates: the destination has strictly more buckets than the
                // number of keys being replayed, so an EMPTY slot always exists.
                while *self.status.add(slot) != BUCKET_EMPTY {
                    slot = (slot + 1) & mask;
                }
                ptr::copy_nonoverlapping(src, self.kv.add(slot * kv_size), kv_size);
                // The tag is re-derived, not carried over: it depends only on the
                // hash, which is the one thing this loop must recompute anyway.
                *self.status.add(slot) = ctrl_of(hash);
                self.len += 1;
            }
        }
    }

    /// Fallible sibling of [`alloc_storage`]: returns `None` on OOM — after
    /// releasing any partial allocation — instead of dereferencing a null
    /// `alloc` result (the historical abort/segfault). Backs the growth path of
    /// `karac_map_try_insert` (the `Map.try_insert` fallible-allocation
    /// companion, phase-8-stdlib-floor item 8).
    unsafe fn alloc_storage_fallible(
        capacity: usize,
        key_size: usize,
        val_size: usize,
    ) -> Option<(*mut u8, *mut u8)> {
        unsafe {
            let status_layout = Layout::array::<u8>(capacity).ok()?;
            let status = alloc(status_layout);
            if status.is_null() {
                return None;
            }
            ptr::write_bytes(status, BUCKET_EMPTY, capacity);

            let kv_size = (key_size + val_size).max(1);
            let kv_layout = match Layout::array::<u8>(capacity * kv_size) {
                Ok(l) => l,
                Err(_) => {
                    dealloc(status, status_layout);
                    return None;
                }
            };
            let kv = alloc(kv_layout);
            if kv.is_null() {
                dealloc(status, status_layout);
                return None;
            }
            Some((status, kv))
        }
    }

    /// Fallible sibling of [`resize`]: picks the next width via
    /// [`Self::next_capacity`] and allocates it with
    /// [`alloc_storage_fallible`]. On OOM the map is left **completely
    /// unchanged** — nothing is swapped in, no rehash runs, the old storage is
    /// intact — and the attempted allocation size (status + kv arrays) is
    /// returned as `Err(bytes)`. The new storage is allocated *before* any
    /// `self` field is mutated, so the failure path needs no rollback.
    /// Widen the table so `additional` more entries fit WITHOUT tripping the
    /// insert-time growth guard. B-2026-08-26-22.
    ///
    /// The target follows from that guard rather than from any new policy: an
    /// insert resizes when `(len + tombstones + 1) * 4 > capacity * 3`, so a
    /// table that is to absorb `len + additional` live entries untouched needs
    /// `capacity >= ceil((len + additional) * 4 / 3)`. Tombstones are counted
    /// in too — they occupy probe slots and count toward that same guard, so
    /// ignoring them would under-reserve exactly on the tables that most need
    /// the reservation.
    ///
    /// Capacity stays a power of two (`hash & (capacity - 1)` is the whole
    /// index computation), so the target is rounded up by doubling rather than
    /// taken verbatim.
    ///
    /// Returns `Err(bytes)` if the allocation fails, leaving the map untouched;
    /// `Ok(())` when it grew OR when the reservation was already satisfied —
    /// a reserve that needs no allocation cannot fail.
    unsafe fn reserve_additional(&mut self, additional: usize) -> Result<(), u64> {
        unsafe {
            let target = self
                .len
                .saturating_add(self.tombstones)
                .saturating_add(additional);
            // ceil(target * 4 / 3), saturating so a nonsense `additional`
            // cannot wrap into a small capacity.
            let needed = target.saturating_mul(4).saturating_add(2) / 3;
            if needed <= self.capacity {
                return Ok(());
            }
            let mut new_cap = self.capacity.max(INITIAL_CAPACITY);
            while new_cap < needed {
                match new_cap.checked_mul(2) {
                    Some(n) => new_cap = n,
                    None => return Err(u64::MAX),
                }
            }
            let (new_status, new_kv) =
                match Self::alloc_storage_fallible(new_cap, self.key_size, self.val_size) {
                    Some(pair) => pair,
                    None => {
                        let kv_size = (self.key_size + self.val_size).max(1);
                        let bytes = (new_cap as u64)
                            .saturating_add((new_cap as u64).saturating_mul(kv_size as u64));
                        return Err(bytes);
                    }
                };

            let old_status = self.status;
            let old_kv = self.kv;
            let old_cap = self.capacity;

            self.status = new_status;
            self.kv = new_kv;
            self.capacity = new_cap;
            self.len = 0;
            self.tombstones = 0;

            // Rehashing drops tombstones on the floor, which is why the target
            // above counts them: after this the table holds `len` live entries
            // in `new_cap` buckets with no tombstones at all.
            self.rehash_from(old_status, old_kv, old_cap);

            let status_layout = Layout::array::<u8>(old_cap).unwrap();
            dealloc(old_status, status_layout);
            let kv_layout =
                Layout::array::<u8>(old_cap * (self.key_size + self.val_size).max(1)).unwrap();
            dealloc(old_kv, kv_layout);
            Ok(())
        }
    }

    unsafe fn try_resize(&mut self) -> Result<(), u64> {
        unsafe {
            // Same live-count-driven width choice as `resize` (B-2026-07-31-21);
            // on the fallible path the same-capacity compaction is also the
            // OOM-friendlier allocation.
            let new_cap = self.next_capacity();
            let (new_status, new_kv) =
                match Self::alloc_storage_fallible(new_cap, self.key_size, self.val_size) {
                    Some(pair) => pair,
                    None => {
                        let kv_size = (self.key_size + self.val_size).max(1);
                        let bytes = (new_cap as u64)
                            .saturating_add((new_cap as u64).saturating_mul(kv_size as u64));
                        return Err(bytes);
                    }
                };

            let old_status = self.status;
            let old_kv = self.kv;
            let old_cap = self.capacity;

            self.status = new_status;
            self.kv = new_kv;
            self.capacity = new_cap;
            self.len = 0;
            self.tombstones = 0;

            self.rehash_from(old_status, old_kv, old_cap);

            let status_layout = Layout::array::<u8>(old_cap).unwrap();
            dealloc(old_status, status_layout);
            let kv_layout =
                Layout::array::<u8>(old_cap * (self.key_size + self.val_size).max(1)).unwrap();
            dealloc(old_kv, kv_layout);
            Ok(())
        }
    }
}

struct KaracMapIter {
    map: *const KaracMap,
    index: usize,
}

// ── Public C ABI ─────────────────────────────────────────────────────────────

/// # Safety
/// `hash_fn` / `eq_fn` must be sound to call on every `key_size`-byte blob the
/// caller will ever pass this map, for the map's whole lifetime; they must
/// agree (eq ⇒ equal hashes). The returned pointer owns the map — release it
/// through exactly one `karac_map_free*` call.
#[no_mangle]
pub unsafe extern "C" fn karac_map_new(
    key_size: usize,
    val_size: usize,
    hash_fn: unsafe extern "C" fn(*const c_void) -> u64,
    eq_fn: unsafe extern "C" fn(*const c_void, *const c_void) -> bool,
) -> *mut c_void {
    unsafe { KaracMap::new(key_size, val_size, hash_fn, eq_fn) as *mut c_void }
}

/// # Safety
/// `map` is null (no-op) or a live `karac_map_new` pointer not freed before;
/// after this call it is dangling. Entries are NOT recursively dropped — for
/// heap-owning keys/values codegen must route through the `_with_drop_vec` /
/// `_with_val_drop_fn` variants or the stored buffers leak.
#[no_mangle]
pub unsafe extern "C" fn karac_map_free(map: *mut c_void) {
    unsafe {
        if map.is_null() {
            return;
        }
        let mut m = Box::from_raw(map as *mut KaracMap);
        m.free_storage();
        // Box drop frees the KaracMap allocation itself.
    }
}

/// `karac_map_free` variant that recursively drops per-entry Vec / String
/// content before deallocating the bucket storage. Selected when *either*
/// the key type or the value type follows the `{ptr, i64, i64}` runtime
/// layout (`Vec[T]`, `String`, `Set[Vec[T]]`, `Set[String]`,
/// `Map[String, V]`, `Map[K, Vec[T]]`, `Map[String, Vec[T]]`, etc.).
///
/// `drop_key != 0` → for each live entry, free the key's `data` pointer
/// when its `cap > 0`. `drop_val != 0` → same for the value. Both flags
/// may be set simultaneously (e.g. `Map[String, Vec[T]]`). When both
/// are zero the caller should route through plain `karac_map_free`
/// instead — this function still works in that case but loops with no
/// side-effect.
///
/// **Layout contract.** When `drop_key != 0`, key_size MUST be 24 and
/// the key value at each live slot is exactly the karac runtime
/// Vec/String struct (offset 0: 8-byte data pointer; offset 8: 8-byte
/// len; offset 16: 8-byte cap). Same for `drop_val != 0` and val_size.
/// The codegen-side `FreeMapHandle` cleanup arm guards both flags via
/// `llvm_ty_is_vec_struct` against the registered key / value LLVM
/// types.
///
/// **Set[T] handling.** Set lowers to `Map[T, ()]` with `val_size = 0`.
/// For `Set[Vec[T]]` / `Set[String]`, codegen passes `drop_key = 1,
/// drop_val = 0`. The val-side loop is gated by `drop_val != 0` so it
/// never reads the (non-existent) value blob.
///
/// Closes the 2026-05-13 / 2026-05-14 leak class where heap-owning keys
/// or non-Vec heap-owning values in Maps / Sets were never released.
/// Replaces the narrower `karac_map_free_with_val_drop_vec` (val-only)
/// helper.
/// # Safety
/// As `karac_map_free`, plus the **layout contract** above: a nonzero
/// `drop_key` / `drop_val` asserts that half of every live slot is exactly the
/// 24-byte `{ptr,len,cap}` runtime Vec/String struct whose `ptr` (when
/// `cap > 0`) is a live malloc allocation this map uniquely owns — it is
/// passed to `free`, so an alias or a borrowed view here is a double-free.
#[no_mangle]
pub unsafe extern "C" fn karac_map_free_with_drop_vec(
    map: *mut c_void,
    drop_key: i32,
    drop_val: i32,
) {
    unsafe {
        if map.is_null() {
            return;
        }
        let mut m = Box::from_raw(map as *mut KaracMap);
        if drop_key != 0 || drop_val != 0 {
            for slot in 0..m.capacity {
                if !is_occupied(*m.status.add(slot)) {
                    continue;
                }
                if drop_key != 0 {
                    m.free_stored_key(slot);
                }
                if drop_val != 0 {
                    m.free_stored_val(slot);
                }
            }
        }
        m.free_storage();
    }
}

/// `karac_map_free` variant that runs a synthesized per-VALUE drop function
/// on every live entry before deallocating the bucket storage — the
/// "values that aren't Vec/String" leg of the recursive-drop work
/// (deferred gap (d), owned-temp slice 3r). Selected by codegen when the
/// value type owns heap but does NOT follow the `{ptr, i64, i64}` overlay
/// (`Map[K, Holder]`, `Map[K, Map[J, W]]`, `Map[K, Option[String]]`) or
/// follows it but needs per-element recursion (`Map[K, Vec[String]]`,
/// `Map[K, Vec[Vec[T]]]` — the flag-based helper frees only the value's
/// outer buffer).
///
/// `drop_key != 0` keeps the flag-based KEY contract of
/// `karac_map_free_with_drop_vec` (keys are Hash-constrained to the
/// Vec/String overlay or scalars, so the key side never needs a fn).
/// `val_drop_fn` receives a pointer to the value blob IN PLACE (the same
/// address `val_ptr` yields) and must free the value's owned heap without
/// touching the blob storage itself — exactly the synthesized
/// `karac_drop_<T>(ptr)` family's contract. A null fn is tolerated
/// (degrades to `karac_map_free_with_drop_vec(map, drop_key, 0)`).
/// # Safety
/// As `karac_map_free`; `drop_key` carries `karac_map_free_with_drop_vec`'s
/// key-side layout contract. A non-null `val_drop_fn` must be sound to call on
/// a pointer to every live value blob IN PLACE, freeing only the value's owned
/// heap (never the blob storage), and must not touch the map reentrantly.
#[no_mangle]
pub unsafe extern "C" fn karac_map_free_with_val_drop_fn(
    map: *mut c_void,
    drop_key: i32,
    val_drop_fn: Option<unsafe extern "C" fn(*mut c_void)>,
) {
    unsafe {
        if map.is_null() {
            return;
        }
        let mut m = Box::from_raw(map as *mut KaracMap);
        if drop_key != 0 || val_drop_fn.is_some() {
            for slot in 0..m.capacity {
                if !is_occupied(*m.status.add(slot)) {
                    continue;
                }
                if drop_key != 0 {
                    m.free_stored_key(slot);
                }
                if let Some(f) = val_drop_fn {
                    f(m.val_ptr(slot) as *mut c_void);
                }
            }
        }
        m.free_storage();
    }
}

/// # Safety
/// Shared contract above. The key and value bytes are bit-copied in; if the
/// map already owned an equal key, the OLD value is overwritten without being
/// dropped — codegen must use `karac_map_insert_old` when the value owns heap.
#[no_mangle]
pub unsafe extern "C" fn karac_map_insert(
    map: *mut c_void,
    key: *const c_void,
    val: *const c_void,
) {
    unsafe {
        (*(map as *mut KaracMap)).insert(key, val);
    }
}

/// Inserts `key → val`. If `key` already existed, copies the **old** value into
/// `out_old_val` and returns `true`. If it was a fresh insertion, returns `false`
/// and leaves `out_old_val` untouched. Matches `Map.insert → Option[V]` semantics.
/// # Safety
/// Shared contract above; `out_old_val` must be writable for `val_size` bytes.
/// On `true`, ownership of the OLD value's heap (if any) moves to the caller
/// through `out_old_val` — the caller must drop it or it leaks.
#[no_mangle]
pub unsafe extern "C" fn karac_map_insert_old(
    map: *mut c_void,
    key: *const c_void,
    val: *const c_void,
    out_old_val: *mut c_void,
) -> bool {
    unsafe {
        let m = &mut *(map as *mut KaracMap);
        // Resize before probing so find_insert_slot always finds a slot.
        if (m.len + m.tombstones + 1) * 4 > m.capacity * 3 {
            m.resize();
        }
        let (slot, exists, ctrl) = m.find_insert_slot(key);
        let was_tombstone = *m.status.add(slot) == BUCKET_TOMBSTONE;
        let kv_offset = slot * (m.key_size + m.val_size);
        if exists {
            // Copy old value out before overwriting.
            ptr::copy_nonoverlapping(
                m.kv.add(kv_offset + m.key_size),
                out_old_val as *mut u8,
                m.val_size,
            );
        } else {
            ptr::copy_nonoverlapping(key as *const u8, m.kv.add(kv_offset), m.key_size);
            m.len += 1;
            if was_tombstone {
                m.tombstones -= 1;
            }
        }
        ptr::copy_nonoverlapping(
            val as *const u8,
            m.kv.add(kv_offset + m.key_size),
            m.val_size,
        );
        *m.status.add(slot) = ctrl;
        exists
    }
}

/// Fallible sibling of [`karac_map_insert_old`]: the runtime backing for the
/// `Map.try_insert` / `Set.try_insert` / `SortedSet.try_insert` fallible-
/// allocation companions (phase-8-stdlib-floor item 8). Behaves exactly like
/// `karac_map_insert_old` — copies any displaced old value into `out_old_val`,
/// distinguishing a fresh insertion from an update — **except** the load-factor
/// growth routes through [`try_resize`], which leaves the map untouched on OOM
/// instead of aborting. Return code:
///   * `0` — fresh insertion; `out_old_val` untouched (`Ok(None)`).
///   * `1` — updated an existing key; old value copied to `out_old_val`
///     (`Ok(Some(old))`).
///   * `2` — OOM during growth; the map is unchanged, nothing is written to
///     `out_old_val`, and the attempted allocation byte count is stored through
///     `out_failed_bytes` (`Err(AllocError.OutOfMemory{bytes})`).
///
/// Codegen (`compile_map_try_insert`) branches on the code: `2` builds the
/// `Result.Err`; `0`/`1` reuse the panicking `Map.insert` arm's `Option[V]`
/// construction and wrap it in `Result.Ok`. Growth is the *only* allocation an
/// insert performs (the slot write is copy-only), so making `try_resize`
/// fallible makes the whole operation fallible.
/// # Safety
/// As `karac_map_insert_old`; additionally `out_failed_bytes` is null or
/// writable. On the OOM return the map is unchanged and nothing moved.
#[no_mangle]
pub unsafe extern "C" fn karac_map_try_insert(
    map: *mut c_void,
    key: *const c_void,
    val: *const c_void,
    out_old_val: *mut c_void,
    out_failed_bytes: *mut u64,
) -> i32 {
    unsafe {
        let m = &mut *(map as *mut KaracMap);
        // Grow before probing so find_insert_slot always finds a slot — but do it
        // fallibly. On OOM the map is unchanged; report the attempted bytes.
        if (m.len + m.tombstones + 1) * 4 > m.capacity * 3 {
            if let Err(bytes) = m.try_resize() {
                if !out_failed_bytes.is_null() {
                    *out_failed_bytes = bytes;
                }
                return 2;
            }
        }
        let (slot, exists, ctrl) = m.find_insert_slot(key);
        let was_tombstone = *m.status.add(slot) == BUCKET_TOMBSTONE;
        let kv_offset = slot * (m.key_size + m.val_size);
        if exists {
            ptr::copy_nonoverlapping(
                m.kv.add(kv_offset + m.key_size),
                out_old_val as *mut u8,
                m.val_size,
            );
        } else {
            ptr::copy_nonoverlapping(key as *const u8, m.kv.add(kv_offset), m.key_size);
            m.len += 1;
            if was_tombstone {
                m.tombstones -= 1;
            }
        }
        ptr::copy_nonoverlapping(
            val as *const u8,
            m.kv.add(kv_offset + m.key_size),
            m.val_size,
        );
        *m.status.add(slot) = ctrl;
        if exists {
            1
        } else {
            0
        }
    }
}

/// Borrowed-key insert for **String-keyed** maps (`key_size == 24`, the
/// `{ptr, i64 len, i64 cap}` layout). `key` points to a *borrowed* String view
/// whose `data` pointer aliases memory the caller owns (e.g. a slice into
/// another String, built by `karac_string_slice_borrow` with `cap == 0`). The
/// map MUST NOT retain that pointer.
///
/// On a fresh insertion the borrowed `{data, len}` is **deep-copied** into a
/// freshly-allocated owned buffer (`alloc(len + 1)`, copy, NUL-terminate,
/// stored as `{owned_ptr, len, cap = len}`) — the same buffer contract as
/// `karac_string_clone` / `karac_string_slice`, so the stored key owns its
/// bytes and is released by `karac_map_free_with_drop_vec`'s `cap > 0`
/// key-drop. On an existing key only the value is overwritten (and the old
/// value copied to `out_old_val`); the borrowed key is discarded with **zero
/// allocation**. Return value mirrors `karac_map_insert_old` (`true` + old
/// value when the key already existed).
///
/// This is the allocation-free counter/lookup-map fast path: callers pass a
/// borrowed slice view instead of a freshly-`malloc`'d owned `String`, so the
/// only allocation across a long run is one per *distinct* key.
/// # Safety
/// As `karac_map_insert_old`, narrowed to a `String` key (`key_size == 24`):
/// `key` may be a BORROWED `{ptr,len,cap}` view — on fresh insertion the
/// runtime clones the bytes into an owned buffer, so the borrow only needs to
/// outlive this call, not the map.
#[no_mangle]
pub unsafe extern "C" fn karac_map_insert_borrowed_str_old(
    map: *mut c_void,
    key: *const c_void,
    val: *const c_void,
    out_old_val: *mut c_void,
) -> bool {
    unsafe {
        let m = &mut *(map as *mut KaracMap);
        debug_assert_eq!(m.key_size, 24, "borrowed-str insert requires a String key");
        if (m.len + m.tombstones + 1) * 4 > m.capacity * 3 {
            m.resize();
        }
        // hash_fn / eq_fn read the borrowed view's {ptr, len} — identical to an
        // owned String key — so probing works unchanged.
        let (slot, exists, ctrl) = m.find_insert_slot(key);
        let was_tombstone = *m.status.add(slot) == BUCKET_TOMBSTONE;
        let kv_offset = slot * (m.key_size + m.val_size);
        if exists {
            ptr::copy_nonoverlapping(
                m.kv.add(kv_offset + m.key_size),
                out_old_val as *mut u8,
                m.val_size,
            );
        } else {
            // Deep-copy the borrowed bytes into an owned, NUL-terminated buffer so
            // the stored key never aliases the caller's source string.
            let src_data = ptr::read_unaligned(key as *const *const u8);
            let src_len = ptr::read_unaligned((key as *const u8).add(8) as *const i64);
            let n = src_len as usize;
            let owned_ptr: *mut u8 = if n == 0 {
                ptr::null_mut()
            } else {
                let layout = Layout::array::<u8>(n + 1).unwrap();
                let p = alloc(layout);
                ptr::copy_nonoverlapping(src_data, p, n);
                *p.add(n) = 0;
                p
            };
            let kslot = m.kv.add(kv_offset);
            ptr::write_unaligned(kslot as *mut *mut u8, owned_ptr);
            ptr::write_unaligned(kslot.add(8) as *mut i64, src_len);
            // cap == len marks an owned buffer the free path will release.
            ptr::write_unaligned(kslot.add(16) as *mut i64, src_len);
            m.len += 1;
            if was_tombstone {
                m.tombstones -= 1;
            }
        }
        ptr::copy_nonoverlapping(
            val as *const u8,
            m.kv.add(kv_offset + m.key_size),
            m.val_size,
        );
        *m.status.add(slot) = ctrl;
        exists
    }
}

/// Returns `true` and copies the value into `out_val` if the key exists.
/// Returns `false` and leaves `out_val` untouched otherwise.
/// # Safety
/// Shared contract above; `out_val` writable for `val_size` bytes. The copy
/// written on `true` is a bit-copy — for a heap-owning value it ALIASES the
/// stored buffer; the caller must not free through it.
#[no_mangle]
pub unsafe extern "C" fn karac_map_get(
    map: *const c_void,
    key: *const c_void,
    out_val: *mut c_void,
) -> bool {
    unsafe { (*(map as *const KaracMap)).get(key, out_val) }
}

/// Returns `true` if the key was present and has been removed.
///
/// `drop_key` / `drop_val` (codegen-set; nonzero = "this half is a heap
/// `{ptr,len,cap}` Vec/String") free the bucket's STORED key / value before
/// the tombstone orphans them — `karac_map_free_with_drop_vec` only walks
/// OCCUPIED slots, so a tombstoned buffer would leak. This variant discards
/// both halves (the presence boolean carries no payload), so both may be
/// freed; contrast `karac_map_remove_old`, which moves the value out and
/// frees only the key. **Not currently wired by codegen** — `Map.remove` /
/// `Set.remove` lower to `karac_map_remove_old` — but kept correct for the
/// exported ABI (see `runtime/src/lib.rs` keep list).
/// # Safety
/// Shared contract above. Nonzero `drop_key` / `drop_val` carry the
/// `karac_map_free_with_drop_vec` layout contract for the STORED key / value,
/// which are freed here (both halves are discarded).
#[no_mangle]
pub unsafe extern "C" fn karac_map_remove(
    map: *mut c_void,
    key: *const c_void,
    drop_key: i32,
    drop_val: i32,
) -> bool {
    unsafe { (*(map as *mut KaracMap)).remove(key, drop_key != 0, drop_val != 0) }
}

/// Removes `key`. If it existed, copies the **old** value into `out_old_val` and
/// returns `true`. Returns `false` and leaves `out_old_val` untouched otherwise.
/// Matches `Map.remove → Option[V]` semantics.
///
/// The value is MOVED OUT to the caller via `out_old_val` (the returned
/// `Some(old)` owns its `{ptr,len,cap}` buffer now), so this variant frees
/// ONLY the bucket's STORED key — never the value. `drop_key` (codegen-set;
/// nonzero = "key is a heap `{ptr,len,cap}` Vec/String") gates that free; the
/// tombstone would otherwise orphan the stored key buffer, since
/// `karac_map_free_with_drop_vec` only walks OCCUPIED slots.
/// # Safety
/// Shared contract above; `out_old_val` writable for `val_size` bytes. On
/// `true` the value's heap moves out to the caller via `out_old_val`; nonzero
/// `drop_key` carries the layout contract for the STORED key, which is freed.
#[no_mangle]
pub unsafe extern "C" fn karac_map_remove_old(
    map: *mut c_void,
    key: *const c_void,
    out_old_val: *mut c_void,
    drop_key: i32,
) -> bool {
    unsafe {
        let m = &mut *(map as *mut KaracMap);
        if let Some(slot) = m.lookup(key) {
            ptr::copy_nonoverlapping(
                m.val_ptr(slot) as *const u8,
                out_old_val as *mut u8,
                m.val_size,
            );
            if drop_key != 0 {
                m.free_stored_key(slot);
            }
            m.vacate(slot);
            true
        } else {
            false
        }
    }
}

/// Collect all live keys into a freshly-`malloc`'d buffer of `len * key_size`
/// bytes, SORTED ascending by `cmp_fn` (a codegen-emitted comparator returning
/// `<0` / `0` / `>0`, the same 3-way sign the interpreter's `value_compare`
/// yields). Writes the key count through `out_len` and returns the buffer (NULL
/// for an empty map). Backs `SortedSet`/`SortedMap`'s ordered observation points
/// — the `for`-loop walks the buffer in order, and `min` / `max` read `buf[0]` /
/// `buf[len-1]`. The buffer holds a bit-copy of each key slot (for a `String`
/// key that is the `{ptr,len,cap}` header — an ALIAS into the map's owned
/// buffer, valid for the read-only ordered walk); the caller frees ONLY the
/// returned buffer via `free`, never the individual keys.
/// # Safety
/// Shared contract above; `out_len` must be writable. `cmp_fn` must be sound
/// on every pair of stored key blobs and totally ordered. The returned buffer
/// (caller frees via `free`) holds BIT-COPIES of the key slots — for `String`
/// keys these alias map-owned buffers, valid only while the map lives and only
/// for reading; freeing a copied key double-frees.
#[no_mangle]
pub unsafe extern "C" fn karac_map_sorted_keys(
    map: *const c_void,
    out_len: *mut usize,
    cmp_fn: unsafe extern "C" fn(*const c_void, *const c_void) -> i32,
) -> *mut u8 {
    unsafe {
        let m = &*(map as *const KaracMap);
        let n = m.len;
        if !out_len.is_null() {
            *out_len = n;
        }
        if n == 0 {
            return ptr::null_mut();
        }
        let ks = m.key_size;
        // Gather pointers to each live key slot, sort by the comparator, then gather
        // the sorted keys into the output buffer. Sorting pointers (not the bytes)
        // keeps the comparator operating on the map's stable key storage.
        let mut keys: Vec<*const u8> = Vec::with_capacity(n);
        for slot in 0..m.capacity {
            if is_occupied(*m.status.add(slot)) {
                keys.push(m.key_ptr(slot) as *const u8);
            }
        }
        keys.sort_by(|&a, &b| cmp_fn(a as *const c_void, b as *const c_void).cmp(&0));
        let buf = alloc(Layout::array::<u8>(n * ks).unwrap());
        if buf.is_null() {
            crate::fatal::write_stderr(b"panic: out of memory\n");
            std::process::abort();
        }
        for (i, &kp) in keys.iter().enumerate() {
            ptr::copy_nonoverlapping(kp, buf.add(i * ks), ks);
        }
        buf
    }
}

/// # Safety
/// Shared contract above.
#[no_mangle]
pub unsafe extern "C" fn karac_map_contains(map: *const c_void, key: *const c_void) -> bool {
    unsafe { (*(map as *const KaracMap)).lookup(key).is_some() }
}

/// Probe-and-insert-on-vacant. Used by `Map.entry(k)` chains whose
/// terminal step is `or_insert` / `or_insert_with` — the codegen knows it
/// will write a default through the returned slot pointer when the key was
/// missing, so the runtime claims the bucket up front.
///
/// On Vacant: writes the key bytes, marks the bucket OCCUPIED, and leaves
/// the value half uninitialised. Returns `false` so the caller overwrites.
/// On Occupied: leaves the bucket alone, returns `true`.
///
/// Resizes before probing so the slot index — and therefore the slot
/// pointer — is stable for the rest of the call. The returned pointer is
/// valid until the next mutating call on the same map (matches the Rust
/// `HashMap::entry` lifetime contract).
/// # Safety
/// Shared contract above; `out_slot_ptr` must be writable. On a Vacant hit the
/// key is bit-copied in and the value slot is returned UNINITIALIZED — the
/// caller must write `val_size` bytes before any read. The returned pointer is
/// into bucket storage: it is invalidated by ANY map mutation (insert / remove
/// / clear / free) and must not outlive the next one.
#[no_mangle]
pub unsafe extern "C" fn karac_map_entry(
    map: *mut c_void,
    key: *const c_void,
    out_slot_ptr: *mut *mut c_void,
) -> bool {
    unsafe {
        let m = &mut *(map as *mut KaracMap);
        if (m.len + m.tombstones + 1) * 4 > m.capacity * 3 {
            m.resize();
        }
        let (slot, exists, ctrl) = m.find_insert_slot(key);
        if !exists {
            let was_tombstone = *m.status.add(slot) == BUCKET_TOMBSTONE;
            let kv_offset = slot * (m.key_size + m.val_size);
            ptr::copy_nonoverlapping(key as *const u8, m.kv.add(kv_offset), m.key_size);
            *m.status.add(slot) = ctrl;
            m.len += 1;
            if was_tombstone {
                m.tombstones -= 1;
            }
        }
        *out_slot_ptr = m.val_ptr(slot) as *mut c_void;
        exists
    }
}

/// Read-only lookup variant used to lower `Map.entry(k)` chains whose
/// terminal step is `and_modify` — the codegen runs the closure only when
/// the key is present, and never inserts. Distinct C ABI from
/// `karac_map_entry` so the runtime can keep the pure / mutating contracts
/// separate.
///
/// On Occupied: writes the value-half pointer to `out_slot_ptr`, returns
/// `true`. On Vacant: leaves `out_slot_ptr` untouched, returns `false`.
/// Pointer lifetime matches `karac_map_entry`'s contract.
/// # Safety
/// Shared contract above; `out_slot_ptr` must be writable. The pointer written
/// on `true` has `karac_map_entry`'s lifetime contract: dead after the next
/// map mutation.
#[no_mangle]
pub unsafe extern "C" fn karac_map_lookup_slot(
    map: *mut c_void,
    key: *const c_void,
    out_slot_ptr: *mut *mut c_void,
) -> bool {
    unsafe {
        let m = &*(map as *const KaracMap);
        if let Some(slot) = m.lookup(key) {
            *out_slot_ptr = m.val_ptr(slot) as *mut c_void;
            true
        } else {
            false
        }
    }
}

/// Reserve room for `additional` more entries in `map`, growing (and
/// rehashing) if the current table could not absorb them without tripping the
/// insert-time growth guard. B-2026-08-26-22 — the runtime half of
/// `Map.reserve`, which design.md § Fallible Allocation's panicking/fallible
/// table names for `Map` / `Set` / `VecDeque` / `SortedSet`.
///
/// A `Map` is an opaque handle behind this FFI, so unlike `Vec.reserve` there
/// is no `{ptr,len,cap}` for codegen to do capacity arithmetic on — the whole
/// operation has to live here.
///
/// PANICS on allocation failure, matching `karac_map_insert`; the fallible twin
/// is [`karac_map_try_reserve`]. A reservation that is already satisfied
/// allocates nothing and cannot fail.
///
/// `additional` IS SIGNED, AND MUST STAY SIGNED. Kāra's `reserve` treats a
/// non-positive argument as a no-op (`Vec` and `String` clamp it in codegen),
/// and Kāra integers arrive here as `i64`. Typing this parameter `u64` instead
/// reinterprets `reserve(-5)` as a reservation of 18 quintillion entries, which
/// saturates the capacity search and aborts the process with
/// `panic: out of memory` — measured, not hypothetical.
/// # Safety
/// `map` must be a live `karac_map_new` handle.
#[no_mangle]
pub unsafe extern "C" fn karac_map_reserve(map: *mut c_void, additional: i64) {
    unsafe {
        if map.is_null() || additional <= 0 {
            return;
        }
        let m = &mut *(map as *mut KaracMap);
        if m.reserve_additional(additional as usize).is_err() {
            // Same abort path the byte allocators take (`karac_alloc_or_panic`):
            // the default profile's contract is that OOM aborts, and the
            // fallible twin below is how a caller opts out of it.
            crate::fatal::write_stderr(b"panic: out of memory\n");
            std::process::abort();
        }
    }
}

/// Fallible twin of [`karac_map_reserve`]. Returns `true` on success; on
/// failure returns `false`, writes the byte count that could not be allocated
/// through `out_failed_bytes` (when non-null), and leaves the map UNCHANGED —
/// the same all-or-nothing contract `karac_map_try_insert` offers.
/// # Safety
/// As [`karac_map_reserve`]; additionally `out_failed_bytes` is null or
/// writable.
#[no_mangle]
pub unsafe extern "C" fn karac_map_try_reserve(
    map: *mut c_void,
    additional: i64,
    out_failed_bytes: *mut u64,
) -> bool {
    unsafe {
        if map.is_null() || additional <= 0 {
            return true;
        }
        let m = &mut *(map as *mut KaracMap);
        match m.reserve_additional(additional as usize) {
            Ok(()) => true,
            Err(bytes) => {
                if !out_failed_bytes.is_null() {
                    *out_failed_bytes = bytes;
                }
                false
            }
        }
    }
}

/// # Safety
/// Shared contract above (live map pointer).
#[no_mangle]
pub unsafe extern "C" fn karac_map_len(map: *const c_void) -> u64 {
    unsafe { (*(map as *const KaracMap)).len as u64 }
}

/// Removes every entry from `map`. Resets `len` and `tombstones` to 0 and
/// zeroes the status array so every bucket reads as `BUCKET_EMPTY`. The bucket
/// capacity is preserved — matches the Rust `HashMap::clear` contract. The
/// `kv` byte buffer is left untouched (its contents become unreachable but
/// remain allocated for reuse on subsequent inserts).
/// # Safety
/// Shared contract above. Entries are NOT recursively dropped — the
/// `_with_drop_vec` / `_with_val_drop_fn` variants own that, same split as the
/// free family.
#[no_mangle]
pub unsafe extern "C" fn karac_map_clear(map: *mut c_void) {
    unsafe {
        let m = &mut *(map as *mut KaracMap);
        ptr::write_bytes(m.status, BUCKET_EMPTY, m.capacity);
        m.len = 0;
        m.tombstones = 0;
    }
}

/// `karac_map_clear` variant that releases per-entry `Vec`/`String` heap
/// buffers before resetting the table — the in-place sibling of
/// `karac_map_free_with_drop_vec` (same `cap > 0` key/value `{ptr,len,cap}`
/// free, but the bucket storage is *kept* and reset to empty rather than
/// deallocated). Selected by codegen's `Map.clear` arm whenever the key or
/// value type follows the heap-owning `{ptr,len,cap}` layout.
///
/// Without this, `Map[String, V].clear()` (and `Map[K, Vec[T]]`, etc.) leaked
/// every live entry's heap buffer: plain `karac_map_clear` only zeroes the
/// status bytes, so the buffers become unreachable (the eventual map-free
/// frees only *occupied* slots, and after a clear there are none). Shared-half
/// refcounts are decremented codegen-side before this call, mirroring the
/// free path.
/// # Safety
/// As `karac_map_clear`, with `karac_map_free_with_drop_vec`'s layout contract
/// on nonzero `drop_key` / `drop_val` (stored halves are freed, map survives
/// empty).
#[no_mangle]
pub unsafe extern "C" fn karac_map_clear_with_drop_vec(
    map: *mut c_void,
    drop_key: i32,
    drop_val: i32,
) {
    unsafe {
        if map.is_null() {
            return;
        }
        let m = &mut *(map as *mut KaracMap);
        if drop_key != 0 || drop_val != 0 {
            let entry_stride = m.key_size + m.val_size;
            for slot in 0..m.capacity {
                if !is_occupied(*m.status.add(slot)) {
                    continue;
                }
                if drop_key != 0 {
                    let key_base = m.kv.add(slot * entry_stride);
                    let data_ptr = ptr::read_unaligned(key_base as *const *mut u8);
                    let cap = ptr::read_unaligned(key_base.add(16) as *const i64);
                    if cap > 0 && !data_ptr.is_null() {
                        free(data_ptr as *mut c_void);
                    }
                }
                if drop_val != 0 {
                    let val_base = m.kv.add(slot * entry_stride + m.key_size);
                    let data_ptr = ptr::read_unaligned(val_base as *const *mut u8);
                    let cap = ptr::read_unaligned(val_base.add(16) as *const i64);
                    if cap > 0 && !data_ptr.is_null() {
                        free(data_ptr as *mut c_void);
                    }
                }
            }
        }
        ptr::write_bytes(m.status, BUCKET_EMPTY, m.capacity);
        m.len = 0;
        m.tombstones = 0;
    }
}

/// `karac_map_clear` variant for a VALUE with a synthesized drop fn
/// (slice 3r, deferred gap (d)) — the clear sibling of
/// `karac_map_free_with_val_drop_fn`: runs `val_drop_fn` on every live
/// entry's value blob (and frees `{ptr,len,cap}` keys per `drop_key`)
/// before resetting the statuses. The map stays alive and reusable.
/// # Safety
/// As `karac_map_clear`, with `karac_map_free_with_val_drop_fn`'s contracts:
/// key side by layout flag, value side by in-place drop fn.
#[no_mangle]
pub unsafe extern "C" fn karac_map_clear_with_val_drop_fn(
    map: *mut c_void,
    drop_key: i32,
    val_drop_fn: Option<unsafe extern "C" fn(*mut c_void)>,
) {
    unsafe {
        if map.is_null() {
            return;
        }
        let m = &mut *(map as *mut KaracMap);
        if drop_key != 0 || val_drop_fn.is_some() {
            for slot in 0..m.capacity {
                if !is_occupied(*m.status.add(slot)) {
                    continue;
                }
                if drop_key != 0 {
                    m.free_stored_key(slot);
                }
                if let Some(f) = val_drop_fn {
                    f(m.val_ptr(slot) as *mut c_void);
                }
            }
        }
        ptr::write_bytes(m.status, BUCKET_EMPTY, m.capacity);
        m.len = 0;
        m.tombstones = 0;
    }
}

/// # Safety
/// `map` must outlive the returned iterator, which borrows it unconditionally.
/// Any map mutation invalidates the iterator (buckets may rehash); the next
/// `karac_map_iter_next` after one is undefined. Release with
/// `karac_map_iter_free`.
#[no_mangle]
pub unsafe extern "C" fn karac_map_iter_new(map: *const c_void) -> *mut c_void {
    let iter = Box::new(KaracMapIter {
        map: map as *const KaracMap,
        index: 0,
    });
    Box::into_raw(iter) as *mut c_void
}

/// Advances the iterator. Copies the next key into `out_key` and value into
/// `out_val`. Returns `true` if a pair was written, `false` when exhausted.
/// # Safety
/// `iter` is a live `karac_map_iter_new` pointer whose map has not been
/// mutated or freed since; `out_key` / `out_val` writable at the map's widths.
/// Copies are bit-copies — heap-owning halves ALIAS stored buffers.
#[no_mangle]
pub unsafe extern "C" fn karac_map_iter_next(
    iter: *mut c_void,
    out_key: *mut c_void,
    out_val: *mut c_void,
) -> bool {
    unsafe {
        let it = &mut *(iter as *mut KaracMapIter);
        let m = &*it.map;
        while it.index < m.capacity {
            let i = it.index;
            it.index += 1;
            if is_occupied(*m.status.add(i)) {
                let kv_size = m.key_size + m.val_size;
                ptr::copy_nonoverlapping(m.kv.add(i * kv_size), out_key as *mut u8, m.key_size);
                ptr::copy_nonoverlapping(
                    m.kv.add(i * kv_size + m.key_size),
                    out_val as *mut u8,
                    m.val_size,
                );
                return true;
            }
        }
        false
    }
}

/// # Safety
/// `iter` is null (no-op) or a live `karac_map_iter_new` pointer, not freed
/// twice; dangling after this call.
#[no_mangle]
pub unsafe extern "C" fn karac_map_iter_free(iter: *mut c_void) {
    unsafe {
        if !iter.is_null() {
            drop(Box::from_raw(iter as *mut KaracMapIter));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ctrl_of, is_occupied, KaracMap, BUCKET_EMPTY, BUCKET_OCCUPIED_BIT, BUCKET_TOMBSTONE,
    };
    use std::mem::offset_of;

    /// Codegen-side monomorphized `Map[K, V]` symbols load
    /// `KaracMap.len` / `KaracMap.capacity` / `KaracMap.status` /
    /// `KaracMap.kv` by direct GEP + load against this struct's
    /// `#[repr(C)]` layout. The offsets are hardcoded in
    /// `src/codegen.rs` (see `KARAC_MAP_LEN_OFFSET` etc.). Any
    /// reorder / insert / type-change of `KaracMap` fields breaks
    /// the ABI; this test catches the drift before runtime/binary
    /// diverge.
    #[test]
    fn karac_map_field_offsets_match_codegen() {
        assert_eq!(offset_of!(KaracMap, status), 0);
        assert_eq!(offset_of!(KaracMap, kv), 8);
        assert_eq!(offset_of!(KaracMap, capacity), 16);
        assert_eq!(offset_of!(KaracMap, len), 24);
        assert_eq!(offset_of!(KaracMap, tombstones), 32);
        // The tail fields were unpinned until B-2026-07-26-2 even though
        // codegen already GEP'd `val_size` (the Set contains stride) and
        // `hash_fn` (the Set contains probe). The monomorphized String-key
        // Map probe adds `key_size` and `eq_fn`, so pin all four: drift here
        // is a silent wrong-bucket miscompile, not a link error.
        assert_eq!(offset_of!(KaracMap, key_size), 40);
        assert_eq!(offset_of!(KaracMap, val_size), 48);
        assert_eq!(offset_of!(KaracMap, hash_fn), 56);
        assert_eq!(offset_of!(KaracMap, eq_fn), 64);
    }

    /// Sibling of the offsets test for the CONTROL BYTE (B-2026-07-26-2). The
    /// same argument applies: `src/codegen/mono.rs` emits its own probe loops
    /// against this encoding, so drift is a lookup that misses a present key —
    /// a silent wrong answer, not a link error or a crash.
    ///
    /// This side cannot see codegen's constants (different crate), so it pins
    /// the properties codegen relies on. Codegen's mirrors are
    /// `Codegen::BUCKET_EMPTY` / `BUCKET_TOMBSTONE` / `BUCKET_OCCUPIED_BIT` and
    /// `emit_map_ctrl_of` / `emit_map_is_occupied`.
    #[test]
    fn control_byte_encoding_matches_codegen() {
        // The literal values codegen hardcodes.
        assert_eq!(BUCKET_EMPTY, 0x00);
        assert_eq!(BUCKET_TOMBSTONE, 0x01);
        assert_eq!(BUCKET_OCCUPIED_BIT, 0x80);

        // Neither sentinel may read as occupied — this is what lets a lookup
        // test occupancy and hash tag in a single compare against `ctrl_of`.
        assert!(!is_occupied(BUCKET_EMPTY));
        assert!(!is_occupied(BUCKET_TOMBSTONE));

        // Every control byte is occupied, for every possible hash, and carries
        // the TOP 7 bits — low bits belong to the bucket index, and a tag drawn
        // from them would be invariant along a probe chain.
        for shift in 0..64 {
            let h = 1u64 << shift;
            assert!(is_occupied(ctrl_of(h)), "ctrl_of(1<<{shift}) not occupied");
        }
        assert!(is_occupied(ctrl_of(0)));
        assert!(is_occupied(ctrl_of(u64::MAX)));
        assert_eq!(ctrl_of(0), 0x80);
        assert_eq!(ctrl_of(u64::MAX), 0xff);
        // Bits below 57 must not reach the tag.
        assert_eq!(ctrl_of((1 << 57) - 1), 0x80);
        assert_eq!(ctrl_of(1 << 57), 0x81);
    }

    /// The tag must not break the map under the conditions it changes: probe
    /// chains that walk past tombstones and non-matching occupied buckets, and
    /// resizes that re-derive every control byte. Drives enough keys through
    /// insert / lookup / remove / re-insert to force several growths, then
    /// asserts every surviving key is still findable and every removed one is
    /// not — the exact failure mode a wrong tag produces.
    #[test]
    fn tagged_probe_survives_tombstones_and_resize() {
        unsafe extern "C" fn hash_i64(k: *const c_void) -> u64 {
            unsafe {
                // Deliberately WEAK in the low bits so buckets collide and probe
                // chains get long: the tag lives in the high bits, so this is the
                // shape that exercises it.
                let v = *(k as *const i64) as u64;
                v.wrapping_mul(0x9e37_79b9_7f4a_7c15)
            }
        }
        unsafe extern "C" fn eq_i64(a: *const c_void, b: *const c_void) -> bool {
            unsafe { *(a as *const i64) == *(b as *const i64) }
        }

        unsafe {
            let m = KaracMap::new(8, 8, hash_i64, eq_i64);
            let map = &mut *m;
            const N: i64 = 2000;

            for k in 0..N {
                let v = k * 7;
                map.insert(
                    &k as *const i64 as *const c_void,
                    &v as *const i64 as *const c_void,
                );
            }
            assert_eq!(map.len, N as usize);

            // Remove every third key, leaving tombstones mid-chain.
            for k in (0..N).step_by(3) {
                assert!(
                    map.remove(&k as *const i64 as *const c_void, false, false),
                    "remove({k}) missed a present key"
                );
            }

            // Survivors must still be findable THROUGH those tombstones.
            for k in 0..N {
                let mut out: i64 = -1;
                let found = map.get(
                    &k as *const i64 as *const c_void,
                    &mut out as *mut i64 as *mut c_void,
                );
                if k % 3 == 0 {
                    assert!(!found, "removed key {k} still found");
                } else {
                    assert!(found, "live key {k} not found after tombstoning");
                    assert_eq!(out, k * 7, "key {k} read the wrong value");
                }
            }

            // Re-inserting reclaims tombstones and forces more growth; every
            // control byte is re-derived on each resize.
            for k in (0..N).step_by(3) {
                let v = k * 11;
                map.insert(
                    &k as *const i64 as *const c_void,
                    &v as *const i64 as *const c_void,
                );
            }
            assert_eq!(map.len, N as usize);
            for k in 0..N {
                let mut out: i64 = -1;
                assert!(
                    map.get(
                        &k as *const i64 as *const c_void,
                        &mut out as *mut i64 as *mut c_void
                    ),
                    "key {k} lost after re-insert"
                );
                assert_eq!(out, if k % 3 == 0 { k * 11 } else { k * 7 });
            }

            // A key that was never inserted must still miss — the direction a
            // too-permissive tag test would break.
            for k in N..N + 100 {
                let mut out: i64 = -1;
                assert!(
                    !map.get(
                        &k as *const i64 as *const c_void,
                        &mut out as *mut i64 as *mut c_void
                    ),
                    "absent key {k} was found"
                );
            }

            map.free_storage();
            drop(Box::from_raw(m));
        }
    }

    use std::ffi::c_void;

    unsafe extern "C" fn i64_hash(k: *const c_void) -> u64 {
        unsafe {
            // Trivial identity-ish hash; adequate for a correctness test.
            let v = std::ptr::read_unaligned(k as *const i64);
            (v as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        }
    }
    unsafe extern "C" fn i64_eq(a: *const c_void, b: *const c_void) -> bool {
        unsafe {
            std::ptr::read_unaligned(a as *const i64) == std::ptr::read_unaligned(b as *const i64)
        }
    }

    /// `karac_map_try_insert` success paths: fresh insert returns 0, an update
    /// returns 1 and copies the old value out, growth across many inserts (which
    /// drives `try_resize`) stays correct, and every key round-trips through
    /// `get`. OOM (code 2) is not reachable in a unit test without an allocator
    /// shim, so the E2E + interpreter-oracle codegen tests cover the shape; here
    /// the invariant is that the fallible path is behavior-identical to
    /// `insert_old` on the success branch.
    #[test]
    fn try_insert_fresh_update_and_growth() {
        unsafe {
            let map = super::karac_map_new(8, 8, i64_hash, i64_eq);
            let mut old: i64 = 0;
            let mut failed: u64 = 0;
            // 64 fresh inserts (forces several try_resize growths from cap 8).
            for i in 0..64i64 {
                let v = i * 10;
                let code = super::karac_map_try_insert(
                    map,
                    &i as *const i64 as *const c_void,
                    &v as *const i64 as *const c_void,
                    &mut old as *mut i64 as *mut c_void,
                    &mut failed as *mut u64,
                );
                assert_eq!(code, 0, "fresh insert of {i} should return 0");
            }
            assert_eq!(super::karac_map_len(map), 64);
            // Update an existing key: returns 1 with the old value copied out.
            let k = 7i64;
            let nv = 9999i64;
            old = -1;
            let code = super::karac_map_try_insert(
                map,
                &k as *const i64 as *const c_void,
                &nv as *const i64 as *const c_void,
                &mut old as *mut i64 as *mut c_void,
                &mut failed as *mut u64,
            );
            assert_eq!(code, 1, "update should return 1");
            assert_eq!(old, 70, "old value of key 7 was 7*10");
            assert_eq!(super::karac_map_len(map), 64, "update must not grow len");
            // Every key round-trips, and the updated one reads the new value.
            for i in 0..64i64 {
                let mut got: i64 = -1;
                let hit = super::karac_map_get(
                    map,
                    &i as *const i64 as *const c_void,
                    &mut got as *mut i64 as *mut c_void,
                );
                assert!(hit, "key {i} must be present");
                let expected = if i == 7 { 9999 } else { i * 10 };
                assert_eq!(got, expected, "value for key {i}");
            }
            super::karac_map_free(map);
        }
    }
    /// B-2026-07-31-21 — capacity must track the LIVE count, not the total
    /// removal count. A sliding-window workload with DISTINCT keys (live size
    /// pinned at 1024) used to ratchet capacity once per ~¾·capacity removals
    /// forever: the growth trigger counts tombstones, `remove` only adds
    /// them, and `resize` unconditionally doubled — 297 MB RSS at 16M ops
    /// where Rust's HashMap holds 2.4 MB. With the live-count-driven
    /// `next_capacity`, a tombstone-dominated table compacts at the SAME
    /// width (dropping tombstones) and capacity stays bounded by the live
    /// set. 200k ops here would have doubled past 65k buckets before the
    /// fix; the bound below fails immediately on the unconditional-doubling
    /// code.
    ///
    /// B-2026-08-05-4 added the SECOND assertion. That regression is not a
    /// memory bug but an amortization one — a churning table re-firing the
    /// same-width compacting rehash over and over — and the width assertion
    /// above cannot see it: the steady state is 8192 buckets whether or not the
    /// rehash storm is happening, so a test that pins only the width reports
    /// green through the entire regression. What has to be pinned is the rehash
    /// WORK. Counting `hash_fn` calls measures it exactly, because there are
    /// only two sources: one per map operation (the probe) and one per key per
    /// compaction (`rehash_from`, which cannot skip the call — buckets store no
    /// hash). Everything above the operation count is compaction.
    #[test]
    fn churn_with_distinct_keys_keeps_capacity_bounded() {
        use std::ffi::c_void;
        use std::sync::atomic::{AtomicU64, Ordering};
        static HASH_CALLS: AtomicU64 = AtomicU64::new(0);
        unsafe extern "C" fn hash_i64(p: *const c_void) -> u64 {
            unsafe {
                HASH_CALLS.fetch_add(1, Ordering::Relaxed);
                // splitmix64 — a real mix so the probe chains look like
                // production, not sequential-cluster worst cases.
                let mut z = (*(p as *const i64) as u64).wrapping_add(0x9e3779b97f4a7c15);
                z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
                z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
                z ^ (z >> 31)
            }
        }
        unsafe extern "C" fn eq_i64(a: *const c_void, b: *const c_void) -> bool {
            unsafe { *(a as *const i64) == *(b as *const i64) }
        }
        unsafe {
            let map = KaracMap::new(8, 8, hash_i64, eq_i64) as *mut c_void;
            let window: i64 = 1024;
            let total: i64 = 200_000;
            let mut t: i64 = 0;
            while t < total {
                let val: i64 = t * 3;
                super::karac_map_insert(
                    map,
                    &t as *const i64 as *const c_void,
                    &val as *const i64 as *const c_void,
                );
                if t >= window {
                    let old = t - window;
                    let removed =
                        super::karac_map_remove(map, &old as *const i64 as *const c_void, 0, 0);
                    assert!(removed, "key {old} should have been present");
                }
                t += 1;
            }
            let m = &*(map as *const KaracMap);
            assert_eq!(m.len, window as usize, "live size must stay the window");

            // WIDTH — the live set is 1024 and same-width compaction keeps live
            // at or under 3/16 of capacity (B-2026-08-05-4), so the steady state
            // is EXACTLY 8192 buckets: 1025 * 16 / 3 = 5467, and the next power
            // of two above that is 8192.
            //
            // Pinned as equality, not a bound, because the two ways this can
            // break fail in opposite directions and both matter. Unconditional
            // doubling (the B-2026-07-31-21 ratchet) reaches 262144 within these
            // 200k ops and trips the upper side. Narrowing the band back to 3/8
            // settles at 4096 and trips the lower side.
            assert_eq!(
                m.capacity, 8192,
                "steady-state width moved: capacity {} means the same-width \
                 band is no longer 3/16 of a 1024-live table",
                m.capacity
            );

            // WORK — the amortization guard, and the one that actually
            // discriminates B-2026-08-05-4. Width alone does not: `vacate`
            // leaves the steady state at 8192 either way, so the equality above
            // passes with the rehash storm still present. One hash_fn call is
            // inherent per map operation (the probe); every call beyond that is
            // a compaction re-hashing the live set. Pinned as a ratio so it
            // reads as what it is — rehash overhead as a fraction of the
            // workload.
            let ops = (total + (total - window)) as u64; // inserts + removes
            let hashes = HASH_CALLS.load(Ordering::Relaxed);
            let overhead = hashes.saturating_sub(ops) as f64 / ops as f64;
            // Measured on this workload, all four combinations:
            //     3/16 band + vacate (shipping)    1.92%
            //     3/8  band + vacate               7.06%
            //     3/16 band, vacate disabled       6.29%
            //     3/8  band, vacate disabled      15.52%  <- what this bug was
            //                                             filed against
            // Both fixes are load-bearing: dropping either one lands above 6%.
            // 4% is the only threshold that catches both single-fix
            // regressions, and the workload is fully deterministic (fixed key
            // sequence, fixed hash), so 1.92% vs 4% is a hard margin, not a
            // statistical one.
            assert!(
                overhead < 0.04,
                "compaction re-hash overhead is {:.1}% of {ops} ops \
                 ({hashes} hash calls) — a churning table is re-hashing its \
                 live set again",
                overhead * 100.0
            );
            // Correctness after many same-width compactions: every live key
            // still resolves to its value.
            let probe: i64 = total - 10;
            let mut out: i64 = 0;
            let found = super::karac_map_get(
                map,
                &probe as *const i64 as *const c_void,
                &mut out as *mut i64 as *mut c_void,
            );
            assert!(found, "live key {probe} must still be present");
            assert_eq!(out, probe * 3);
            super::karac_map_free(map);
        }
    }

    /// Identity hash: key `k` starts its probe at bucket `k & (cap - 1)`, so a
    /// test can place keys in exactly the buckets it names. Every control byte
    /// is `0x80` for small keys (the tag lives in bits 57..63), which is fine —
    /// `eq_fn` then does the discriminating, same as a real tag collision.
    unsafe extern "C" fn hash_identity(p: *const c_void) -> u64 {
        unsafe { *(p as *const i64) as u64 }
    }
    unsafe extern "C" fn eq_i64_t(a: *const c_void, b: *const c_void) -> bool {
        unsafe { *(a as *const i64) == *(b as *const i64) }
    }

    unsafe fn put(map: *mut c_void, k: i64, v: i64) {
        unsafe {
            super::karac_map_insert(
                map,
                &k as *const i64 as *const c_void,
                &v as *const i64 as *const c_void,
            );
        }
    }
    unsafe fn del(map: *mut c_void, k: i64) -> bool {
        unsafe {
            let mut old: i64 = 0;
            super::karac_map_remove_old(
                map,
                &k as *const i64 as *const c_void,
                &mut old as *mut i64 as *mut c_void,
                0,
            )
        }
    }

    /// B-2026-08-05-4 — `vacate` releases a bucket outright instead of
    /// tombstoning it whenever the next bucket is already EMPTY. Both arms are
    /// pinned here against the exact bucket layout `hash_identity` gives,
    /// because the failure mode of getting this wrong is a lookup that misses a
    /// PRESENT key — a silent wrong answer, not a crash.
    #[test]
    fn remove_releases_the_slot_when_no_chain_runs_past_it() {
        unsafe {
            let map = KaracMap::new(8, 8, hash_identity, eq_i64_t) as *mut c_void;
            let m = &*(map as *const KaracMap);
            assert_eq!(m.capacity, 16);

            // Cluster in buckets 3,4,5 with bucket 6 EMPTY.
            put(map, 3, 30);
            put(map, 4, 40);
            put(map, 5, 50);

            // Removing the run END: bucket 6 is EMPTY, so nothing probes past
            // bucket 5 and it goes straight back to EMPTY.
            assert!(del(map, 5));
            assert_eq!(*m.status.add(5), BUCKET_EMPTY, "run end must be released");
            assert_eq!(m.tombstones, 0, "no tombstone should have been created");
            assert!(is_occupied(*m.status.add(4)), "bucket 4 is untouched");

            // Removing from the MIDDLE: bucket 5 is EMPTY again now, so this is
            // once more a run end. Re-fill it first so bucket 4 is genuinely
            // interior, and check the conservative arm.
            put(map, 5, 55);
            assert!(del(map, 4));
            assert_eq!(
                *m.status.add(4),
                BUCKET_TOMBSTONE,
                "an interior bucket must stay a tombstone — key 5 probes past it"
            );
            assert_eq!(m.tombstones, 1);
            // ...and key 5, whose chain runs through the tombstone, still resolves.
            let mut out: i64 = 0;
            let k5: i64 = 5;
            assert!(super::karac_map_get(
                map,
                &k5 as *const i64 as *const c_void,
                &mut out as *mut i64 as *mut c_void
            ));
            assert_eq!(out, 55);

            // Now remove the run end. Bucket 6 is EMPTY, so bucket 5 is
            // released — but the bucket-4 tombstone behind it deliberately
            // STAYS. Collapsing it too is correct and was measured 30% slower
            // (see `vacate`), so the one-slot rule is the shipped behaviour and
            // is pinned as such: a change that starts clearing bucket 4 here is
            // a perf regression even though it is not a correctness one.
            assert!(del(map, 5));
            assert_eq!(*m.status.add(5), BUCKET_EMPTY);
            assert_eq!(
                *m.status.add(4),
                BUCKET_TOMBSTONE,
                "vacate must stay one-slot-local — no backward collapse walk"
            );
            assert_eq!(m.tombstones, 1);
            assert!(is_occupied(*m.status.add(3)), "bucket 3 is still live");
            assert_eq!(m.len, 1);
            // Whatever the run behind looks like, key 3 still resolves.
            let k3: i64 = 3;
            assert!(super::karac_map_get(
                map,
                &k3 as *const i64 as *const c_void,
                &mut out as *mut i64 as *mut c_void
            ));
            assert_eq!(out, 30);

            super::karac_map_free(map);
        }
    }

    /// The safety net for [`remove_releases_the_slot_when_no_chain_runs_past_it`]:
    /// a long interleaved insert/remove/get tape cross-checked against
    /// `std::collections::HashMap`. The hash deliberately takes only 16 distinct
    /// values, so probe chains are long and tombstone runs are common — exactly
    /// the shape where releasing a bucket too eagerly would truncate a chain and
    /// lose a live key. Deterministic (fixed LCG seed), so a failure reproduces.
    #[test]
    fn churn_against_reference_map_never_loses_a_key() {
        unsafe extern "C" fn hash_colliding(p: *const c_void) -> u64 {
            unsafe { (*(p as *const i64) as u64) % 16 }
        }
        use std::collections::HashMap;
        unsafe {
            let map = KaracMap::new(8, 8, hash_colliding, eq_i64_t) as *mut c_void;
            let mut reference: HashMap<i64, i64> = HashMap::new();
            let mut state: u64 = 0x243f_6a88_85a3_08d3;
            for step in 0..200_000u32 {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let key = ((state >> 33) % 512) as i64;
                // Skew toward removal so the table stays churn-dominated.
                match (state >> 17) % 5 {
                    0 | 1 => {
                        let val = key * 7 + step as i64;
                        put(map, key, val);
                        reference.insert(key, val);
                    }
                    2 | 3 => {
                        assert_eq!(
                            del(map, key),
                            reference.remove(&key).is_some(),
                            "step {step}: remove({key}) presence disagreed"
                        );
                    }
                    _ => {
                        let mut got: i64 = i64::MIN;
                        let hit = super::karac_map_get(
                            map,
                            &key as *const i64 as *const c_void,
                            &mut got as *mut i64 as *mut c_void,
                        );
                        match reference.get(&key) {
                            Some(&want) => {
                                assert!(hit, "step {step}: live key {key} was not found");
                                assert_eq!(got, want, "step {step}: wrong value for {key}");
                            }
                            None => assert!(!hit, "step {step}: absent key {key} was found"),
                        }
                    }
                }
                assert_eq!(
                    super::karac_map_len(map) as usize,
                    reference.len(),
                    "step {step}: len diverged"
                );
            }
            // Final full sweep: every reference key must still resolve.
            for (&k, &want) in reference.iter() {
                let mut got: i64 = i64::MIN;
                let hit = super::karac_map_get(
                    map,
                    &k as *const i64 as *const c_void,
                    &mut got as *mut i64 as *mut c_void,
                );
                assert!(hit, "final sweep: live key {k} was lost");
                assert_eq!(got, want, "final sweep: wrong value for {k}");
            }
            super::karac_map_free(map);
        }
    }
}
