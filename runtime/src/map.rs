//! Open-addressing hash map for compiled Kāra programs.
//!
//! The map is type-erased: keys and values are raw byte blobs. The compiler
//! passes `key_size`/`val_size` at construction time and emits concrete
//! `hash_fn`/`eq_fn` function pointers for the monomorphised K type.
//!
//! Layout: two parallel heap allocations —
//!   `status[capacity]`       — u8 per bucket: EMPTY | OCCUPIED | TOMBSTONE
//!   `kv[capacity*(ks+vs)]`   — packed (key, val) pairs, no alignment padding
//!
//! Collision resolution: linear probing. Load factor ceiling 3/4; resize
//! doubles capacity and rehashes. Deletion marks tombstones; tombstones count
//! toward the load factor so a tombstone-heavy table still triggers a resize.

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
const BUCKET_EMPTY: u8 = 0;
const BUCKET_OCCUPIED: u8 = 1;
const BUCKET_TOMBSTONE: u8 = 2;

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

    unsafe fn alloc_storage(
        capacity: usize,
        key_size: usize,
        val_size: usize,
    ) -> (*mut u8, *mut u8) {
        let status_layout = Layout::array::<u8>(capacity).unwrap();
        let status = alloc(status_layout);
        ptr::write_bytes(status, BUCKET_EMPTY, capacity);

        let kv_size = (key_size + val_size).max(1);
        let kv_layout = Layout::array::<u8>(capacity * kv_size).unwrap();
        let kv = alloc(kv_layout);

        (status, kv)
    }

    unsafe fn free_storage(&mut self) {
        let status_layout = Layout::array::<u8>(self.capacity).unwrap();
        dealloc(self.status, status_layout);

        let kv_size = (self.key_size + self.val_size).max(1);
        let kv_layout = Layout::array::<u8>(self.capacity * kv_size).unwrap();
        dealloc(self.kv, kv_layout);
    }

    #[inline]
    unsafe fn key_ptr(&self, slot: usize) -> *const c_void {
        self.kv.add(slot * (self.key_size + self.val_size)) as *const c_void
    }

    #[inline]
    unsafe fn val_ptr(&self, slot: usize) -> *const c_void {
        self.kv
            .add(slot * (self.key_size + self.val_size) + self.key_size) as *const c_void
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
        let data_ptr = ptr::read_unaligned(base as *const *mut u8);
        let cap = ptr::read_unaligned(base.add(16) as *const i64);
        if cap > 0 && !data_ptr.is_null() {
            free(data_ptr as *mut c_void);
        }
    }

    /// Release the bucket's STORED key buffer at `slot` (see
    /// `free_heap_field`). Caller must have established the key type is a heap
    /// `{ptr,len,cap}` (codegen `drop_key != 0`).
    #[inline]
    unsafe fn free_stored_key(&self, slot: usize) {
        Self::free_heap_field(self.key_ptr(slot) as *const u8);
    }

    /// Release the bucket's STORED value buffer at `slot` (see
    /// `free_heap_field`). Caller must have established the value type is a
    /// heap `{ptr,len,cap}` (codegen `drop_val != 0`). NOT used by
    /// `karac_map_remove_old`, which MOVES the value out to the caller.
    #[inline]
    unsafe fn free_stored_val(&self, slot: usize) {
        Self::free_heap_field(self.val_ptr(slot) as *const u8);
    }

    // Find an occupied slot holding `key`. Returns Some(slot) or None.
    unsafe fn lookup(&self, key: *const c_void) -> Option<usize> {
        let hash = (self.hash_fn)(key);
        let start = (hash as usize) & (self.capacity - 1);
        for i in 0..self.capacity {
            let slot = (start + i) & (self.capacity - 1);
            match *self.status.add(slot) {
                BUCKET_EMPTY => return None,
                BUCKET_OCCUPIED if (self.eq_fn)(self.key_ptr(slot), key) => {
                    return Some(slot);
                }
                _ => {} // TOMBSTONE or non-matching OCCUPIED — keep probing
            }
        }
        None
    }

    // Find the slot to write a new key into. Also returns whether the key
    // already exists (update vs. fresh insert).
    unsafe fn find_insert_slot(&self, key: *const c_void) -> (usize, bool) {
        let hash = (self.hash_fn)(key);
        let start = (hash as usize) & (self.capacity - 1);
        let mut first_tombstone: Option<usize> = None;
        for i in 0..self.capacity {
            let slot = (start + i) & (self.capacity - 1);
            match *self.status.add(slot) {
                BUCKET_EMPTY => {
                    let target = first_tombstone.unwrap_or(slot);
                    return (target, false);
                }
                BUCKET_TOMBSTONE => {
                    if first_tombstone.is_none() {
                        first_tombstone = Some(slot);
                    }
                }
                BUCKET_OCCUPIED => {
                    if (self.eq_fn)(self.key_ptr(slot), key) {
                        return (slot, true);
                    }
                }
                _ => unreachable!(),
            }
        }
        // Should not reach here if resize policy is respected.
        (first_tombstone.unwrap_or(0), false)
    }

    unsafe fn insert(&mut self, key: *const c_void, val: *const c_void) {
        // Resize when (occupied + tombstones) / capacity > 3/4.
        if (self.len + self.tombstones + 1) * 4 > self.capacity * 3 {
            self.resize();
        }
        let (slot, exists) = self.find_insert_slot(key);
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
        *self.status.add(slot) = BUCKET_OCCUPIED;
    }

    unsafe fn get(&self, key: *const c_void, out_val: *mut c_void) -> bool {
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

    unsafe fn remove(&mut self, key: *const c_void, drop_key: bool, drop_val: bool) -> bool {
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
            *self.status.add(slot) = BUCKET_TOMBSTONE;
            self.len -= 1;
            self.tombstones += 1;
            true
        } else {
            false
        }
    }

    unsafe fn resize(&mut self) {
        let new_cap = self.capacity * 2;
        let (new_status, new_kv) = Self::alloc_storage(new_cap, self.key_size, self.val_size);

        let old_status = self.status;
        let old_kv = self.kv;
        let old_cap = self.capacity;

        self.status = new_status;
        self.kv = new_kv;
        self.capacity = new_cap;
        self.len = 0;
        self.tombstones = 0;

        for i in 0..old_cap {
            if *old_status.add(i) == BUCKET_OCCUPIED {
                let kv_size = self.key_size + self.val_size;
                let key = old_kv.add(i * kv_size) as *const c_void;
                let val = old_kv.add(i * kv_size + self.key_size) as *const c_void;
                self.insert(key, val);
            }
        }

        let status_layout = Layout::array::<u8>(old_cap).unwrap();
        dealloc(old_status, status_layout);
        let kv_layout =
            Layout::array::<u8>(old_cap * (self.key_size + self.val_size).max(1)).unwrap();
        dealloc(old_kv, kv_layout);
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

    /// Fallible sibling of [`resize`]: doubles capacity via
    /// [`alloc_storage_fallible`]. On OOM the map is left **completely
    /// unchanged** — nothing is swapped in, no rehash runs, the old storage is
    /// intact — and the attempted allocation size (status + kv arrays) is
    /// returned as `Err(bytes)`. The new storage is allocated *before* any
    /// `self` field is mutated, so the failure path needs no rollback.
    unsafe fn try_resize(&mut self) -> Result<(), u64> {
        let new_cap = self.capacity * 2;
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

        for i in 0..old_cap {
            if *old_status.add(i) == BUCKET_OCCUPIED {
                let kv_size = self.key_size + self.val_size;
                let key = old_kv.add(i * kv_size) as *const c_void;
                let val = old_kv.add(i * kv_size + self.key_size) as *const c_void;
                self.insert(key, val);
            }
        }

        let status_layout = Layout::array::<u8>(old_cap).unwrap();
        dealloc(old_status, status_layout);
        let kv_layout =
            Layout::array::<u8>(old_cap * (self.key_size + self.val_size).max(1)).unwrap();
        dealloc(old_kv, kv_layout);
        Ok(())
    }
}

struct KaracMapIter {
    map: *const KaracMap,
    index: usize,
}

// ── Public C ABI ─────────────────────────────────────────────────────────────

#[no_mangle]
pub unsafe extern "C" fn karac_map_new(
    key_size: usize,
    val_size: usize,
    hash_fn: unsafe extern "C" fn(*const c_void) -> u64,
    eq_fn: unsafe extern "C" fn(*const c_void, *const c_void) -> bool,
) -> *mut c_void {
    KaracMap::new(key_size, val_size, hash_fn, eq_fn) as *mut c_void
}

#[no_mangle]
pub unsafe extern "C" fn karac_map_free(map: *mut c_void) {
    if map.is_null() {
        return;
    }
    let mut m = Box::from_raw(map as *mut KaracMap);
    m.free_storage();
    // Box drop frees the KaracMap allocation itself.
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
#[no_mangle]
pub unsafe extern "C" fn karac_map_free_with_drop_vec(
    map: *mut c_void,
    drop_key: i32,
    drop_val: i32,
) {
    if map.is_null() {
        return;
    }
    let mut m = Box::from_raw(map as *mut KaracMap);
    if drop_key != 0 || drop_val != 0 {
        for slot in 0..m.capacity {
            if *m.status.add(slot) != BUCKET_OCCUPIED {
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
#[no_mangle]
pub unsafe extern "C" fn karac_map_free_with_val_drop_fn(
    map: *mut c_void,
    drop_key: i32,
    val_drop_fn: Option<unsafe extern "C" fn(*mut c_void)>,
) {
    if map.is_null() {
        return;
    }
    let mut m = Box::from_raw(map as *mut KaracMap);
    if drop_key != 0 || val_drop_fn.is_some() {
        for slot in 0..m.capacity {
            if *m.status.add(slot) != BUCKET_OCCUPIED {
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

#[no_mangle]
pub unsafe extern "C" fn karac_map_insert(
    map: *mut c_void,
    key: *const c_void,
    val: *const c_void,
) {
    (*(map as *mut KaracMap)).insert(key, val);
}

/// Inserts `key → val`. If `key` already existed, copies the **old** value into
/// `out_old_val` and returns `true`. If it was a fresh insertion, returns `false`
/// and leaves `out_old_val` untouched. Matches `Map.insert → Option[V]` semantics.
#[no_mangle]
pub unsafe extern "C" fn karac_map_insert_old(
    map: *mut c_void,
    key: *const c_void,
    val: *const c_void,
    out_old_val: *mut c_void,
) -> bool {
    let m = &mut *(map as *mut KaracMap);
    // Resize before probing so find_insert_slot always finds a slot.
    if (m.len + m.tombstones + 1) * 4 > m.capacity * 3 {
        m.resize();
    }
    let (slot, exists) = m.find_insert_slot(key);
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
    *m.status.add(slot) = BUCKET_OCCUPIED;
    exists
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
#[no_mangle]
pub unsafe extern "C" fn karac_map_try_insert(
    map: *mut c_void,
    key: *const c_void,
    val: *const c_void,
    out_old_val: *mut c_void,
    out_failed_bytes: *mut u64,
) -> i32 {
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
    let (slot, exists) = m.find_insert_slot(key);
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
    *m.status.add(slot) = BUCKET_OCCUPIED;
    if exists {
        1
    } else {
        0
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
#[no_mangle]
pub unsafe extern "C" fn karac_map_insert_borrowed_str_old(
    map: *mut c_void,
    key: *const c_void,
    val: *const c_void,
    out_old_val: *mut c_void,
) -> bool {
    let m = &mut *(map as *mut KaracMap);
    debug_assert_eq!(m.key_size, 24, "borrowed-str insert requires a String key");
    if (m.len + m.tombstones + 1) * 4 > m.capacity * 3 {
        m.resize();
    }
    // hash_fn / eq_fn read the borrowed view's {ptr, len} — identical to an
    // owned String key — so probing works unchanged.
    let (slot, exists) = m.find_insert_slot(key);
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
    *m.status.add(slot) = BUCKET_OCCUPIED;
    exists
}

/// Returns `true` and copies the value into `out_val` if the key exists.
/// Returns `false` and leaves `out_val` untouched otherwise.
#[no_mangle]
pub unsafe extern "C" fn karac_map_get(
    map: *const c_void,
    key: *const c_void,
    out_val: *mut c_void,
) -> bool {
    (*(map as *const KaracMap)).get(key, out_val)
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
#[no_mangle]
pub unsafe extern "C" fn karac_map_remove(
    map: *mut c_void,
    key: *const c_void,
    drop_key: i32,
    drop_val: i32,
) -> bool {
    (*(map as *mut KaracMap)).remove(key, drop_key != 0, drop_val != 0)
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
#[no_mangle]
pub unsafe extern "C" fn karac_map_remove_old(
    map: *mut c_void,
    key: *const c_void,
    out_old_val: *mut c_void,
    drop_key: i32,
) -> bool {
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
        *m.status.add(slot) = BUCKET_TOMBSTONE;
        m.len -= 1;
        m.tombstones += 1;
        true
    } else {
        false
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
#[no_mangle]
pub unsafe extern "C" fn karac_map_sorted_keys(
    map: *const c_void,
    out_len: *mut usize,
    cmp_fn: unsafe extern "C" fn(*const c_void, *const c_void) -> i32,
) -> *mut u8 {
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
        if *m.status.add(slot) == BUCKET_OCCUPIED {
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

#[no_mangle]
pub unsafe extern "C" fn karac_map_contains(map: *const c_void, key: *const c_void) -> bool {
    (*(map as *const KaracMap)).lookup(key).is_some()
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
#[no_mangle]
pub unsafe extern "C" fn karac_map_entry(
    map: *mut c_void,
    key: *const c_void,
    out_slot_ptr: *mut *mut c_void,
) -> bool {
    let m = &mut *(map as *mut KaracMap);
    if (m.len + m.tombstones + 1) * 4 > m.capacity * 3 {
        m.resize();
    }
    let (slot, exists) = m.find_insert_slot(key);
    if !exists {
        let was_tombstone = *m.status.add(slot) == BUCKET_TOMBSTONE;
        let kv_offset = slot * (m.key_size + m.val_size);
        ptr::copy_nonoverlapping(key as *const u8, m.kv.add(kv_offset), m.key_size);
        *m.status.add(slot) = BUCKET_OCCUPIED;
        m.len += 1;
        if was_tombstone {
            m.tombstones -= 1;
        }
    }
    *out_slot_ptr = m.val_ptr(slot) as *mut c_void;
    exists
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
#[no_mangle]
pub unsafe extern "C" fn karac_map_lookup_slot(
    map: *mut c_void,
    key: *const c_void,
    out_slot_ptr: *mut *mut c_void,
) -> bool {
    let m = &*(map as *const KaracMap);
    if let Some(slot) = m.lookup(key) {
        *out_slot_ptr = m.val_ptr(slot) as *mut c_void;
        true
    } else {
        false
    }
}

#[no_mangle]
pub unsafe extern "C" fn karac_map_len(map: *const c_void) -> u64 {
    (*(map as *const KaracMap)).len as u64
}

/// Removes every entry from `map`. Resets `len` and `tombstones` to 0 and
/// zeroes the status array so every bucket reads as `BUCKET_EMPTY`. The bucket
/// capacity is preserved — matches the Rust `HashMap::clear` contract. The
/// `kv` byte buffer is left untouched (its contents become unreachable but
/// remain allocated for reuse on subsequent inserts).
#[no_mangle]
pub unsafe extern "C" fn karac_map_clear(map: *mut c_void) {
    let m = &mut *(map as *mut KaracMap);
    ptr::write_bytes(m.status, BUCKET_EMPTY, m.capacity);
    m.len = 0;
    m.tombstones = 0;
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
#[no_mangle]
pub unsafe extern "C" fn karac_map_clear_with_drop_vec(
    map: *mut c_void,
    drop_key: i32,
    drop_val: i32,
) {
    if map.is_null() {
        return;
    }
    let m = &mut *(map as *mut KaracMap);
    if drop_key != 0 || drop_val != 0 {
        let entry_stride = m.key_size + m.val_size;
        for slot in 0..m.capacity {
            if *m.status.add(slot) != BUCKET_OCCUPIED {
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

/// `karac_map_clear` variant for a VALUE with a synthesized drop fn
/// (slice 3r, deferred gap (d)) — the clear sibling of
/// `karac_map_free_with_val_drop_fn`: runs `val_drop_fn` on every live
/// entry's value blob (and frees `{ptr,len,cap}` keys per `drop_key`)
/// before resetting the statuses. The map stays alive and reusable.
#[no_mangle]
pub unsafe extern "C" fn karac_map_clear_with_val_drop_fn(
    map: *mut c_void,
    drop_key: i32,
    val_drop_fn: Option<unsafe extern "C" fn(*mut c_void)>,
) {
    if map.is_null() {
        return;
    }
    let m = &mut *(map as *mut KaracMap);
    if drop_key != 0 || val_drop_fn.is_some() {
        for slot in 0..m.capacity {
            if *m.status.add(slot) != BUCKET_OCCUPIED {
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
#[no_mangle]
pub unsafe extern "C" fn karac_map_iter_next(
    iter: *mut c_void,
    out_key: *mut c_void,
    out_val: *mut c_void,
) -> bool {
    let it = &mut *(iter as *mut KaracMapIter);
    let m = &*it.map;
    while it.index < m.capacity {
        let i = it.index;
        it.index += 1;
        if *m.status.add(i) == BUCKET_OCCUPIED {
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

#[no_mangle]
pub unsafe extern "C" fn karac_map_iter_free(iter: *mut c_void) {
    if !iter.is_null() {
        drop(Box::from_raw(iter as *mut KaracMapIter));
    }
}

#[cfg(test)]
mod tests {
    use super::KaracMap;
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
    }

    use std::ffi::c_void;

    unsafe extern "C" fn i64_hash(k: *const c_void) -> u64 {
        // Trivial identity-ish hash; adequate for a correctness test.
        let v = std::ptr::read_unaligned(k as *const i64);
        (v as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
    }
    unsafe extern "C" fn i64_eq(a: *const c_void, b: *const c_void) -> bool {
        std::ptr::read_unaligned(a as *const i64) == std::ptr::read_unaligned(b as *const i64)
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
}
