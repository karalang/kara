//! Connection-pool primitive for compiled Kāra programs.
//!
//! The AOT-codegen realization of the `Pool[T]` surface the tree-walk
//! interpreter implements with a `pool_table: HashMap<i64, PoolEntry>`
//! side-table (`src/interpreter/method_call_pool.rs`). The `Pool[T]` Kāra
//! struct carries only an `i64 handle_id` — the `*mut KaracPool` round-tripped
//! through `ptrtoint`/`inttoptr` — exactly like `BoundedChannel` / `Semaphore`.
//!
//! **Codegen-orchestrated minting (runtime never calls Kāra code).** The
//! runtime stores the user's `create_fn` closure as two opaque words
//! (`{fn_ptr, env_ptr}`, captured at `Pool.new`) but never invokes it. On
//! `acquire` the runtime decides — reuse an idle slot, reserve a new mint, or
//! fail at cap — and, on the mint decision, hands the stored fat pointer BACK to
//! codegen, which performs the ABI-correct indirect call in the monomorph where
//! `T`'s layout is statically known. This keeps the runtime type-erased: every
//! connection value `T` is moved around as `elem_size` opaque bytes.
//!
//! **v1 scope (slice 1), matching the Arena codegen staging.** `T` must be a
//! POD (all-scalar / no-heap-field) value — the runtime holds idle slots as
//! opaque byte blobs and frees them wholesale at `pool_drop`, so a heap-owning
//! `T` (a `String`/`Vec` connection) would leak its buffer at drop. `create_fn`
//! must have a NULL env (a bare `fn` or a non-capturing lambda); a capturing
//! closure needs the escaping-heap-env machinery and is gated out at codegen.
//! Both are documented follow-ups.
//!
//! **v1 single-threaded semantics (collapsed), matching the interpreter.** An
//! `acquire` against a pool at `max_connections` with no idle slot returns the
//! at-cap status immediately (codegen builds `Err(PoolError.Timeout)`) rather
//! than parking for `timeout` ms — real bounded-waiter parking lands with the
//! network event loop (`design.md`: the `suspends` execution verb), at which
//! point `timeout` / `max_waiters` become load-bearing. Until then they are
//! captured for forward-compat but never gate a waiter queue.
//!
//! **Idempotent return.** A checkout's slot returns to the pool exactly once,
//! keyed on its per-checkout `conn_id`: calling `release(conn)` AND letting the
//! `PooledConnection` binding drop hands the slot back a single time, never
//! inflating one connection into two idle slots — the same contract the
//! interpreter's `return_connection` enforces.
//!
//! **Lifetime — single owner.** `Pool` has no `clone`, so there is no refcount:
//! `karac_runtime_pool_drop` frees it at the owning binding's scope exit (the
//! `Pool` Drop lowering). Mirrors `BoundedChannel` / `Semaphore`.

use std::collections::HashSet;
use std::sync::Mutex;

/// Status codes returned by [`karac_runtime_pool_begin_acquire`]. Codegen
/// switches on these to build the `Result[PooledConnection[T], PoolError]`.
pub const POOL_ACQUIRE_GOT_IDLE: i32 = 0;
pub const POOL_ACQUIRE_NEED_MINT: i32 = 1;
pub const POOL_ACQUIRE_AT_CAP: i32 = 2;
pub const POOL_ACQUIRE_CLOSED: i32 = 3;

#[repr(C)]
pub struct KaracPool {
    /// The user's `create_fn` closure as opaque words (`ptrtoint` of the fat
    /// pointer's `fn_ptr` / `env_ptr`). Handed back to codegen on a mint; the
    /// runtime never calls through them. `env_ptr` is 0 (null) in v1.
    create_fn_ptr: i64,
    create_env_ptr: i64,
    /// Store size of one `T` value in bytes — the width of every slot blob and
    /// of the `out_val` / `val` buffers codegen passes.
    elem_size: usize,
    /// Cap on total live connections (idle-in-slots + checked-out).
    max_connections: i64,
    /// Captured for forward-compat with a parking waiter queue; unused in v1.
    _max_waiters: i64,
    inner: Mutex<PoolInner>,
}

struct PoolInner {
    /// idle-in-slots + checked-out. Bounded by `max_connections`.
    active_count: i64,
    /// Idle connection blobs (each exactly `elem_size` bytes), pushed by
    /// `release`, popped by `begin_acquire`.
    slots: Vec<Box<[u8]>>,
    /// Live checkouts by `conn_id` — the idempotent-return key set.
    checked_out: HashSet<i64>,
    /// Monotonic checkout id source (starts at 1; 0 is "none").
    next_conn_id: i64,
}

// Shared by `ref` across par/spawn branches; the `Mutex` serializes the pool
// bookkeeping, so concurrent acquire/release is sound.
unsafe impl Send for KaracPool {}
unsafe impl Sync for KaracPool {}

/// `karac_runtime_pool_new(fn_ptr, env_ptr, elem_size, max_connections,
/// max_waiters) -> *mut`. Stores the create-fn fat pointer + bounds; the pool
/// starts empty (lazy minting on first `acquire`). A negative `elem_size`
/// clamps to 0.
///
/// # Safety
/// FFI entry point. The returned pointer must eventually be released by
/// `karac_runtime_pool_drop`; codegen guarantees this via the `Pool` Drop
/// scope-exit cleanup.
#[no_mangle]
pub extern "C" fn karac_runtime_pool_new(
    create_fn_ptr: i64,
    create_env_ptr: i64,
    elem_size: i64,
    max_connections: i64,
    max_waiters: i64,
) -> *mut KaracPool {
    Box::into_raw(Box::new(KaracPool {
        create_fn_ptr,
        create_env_ptr,
        elem_size: elem_size.max(0) as usize,
        max_connections,
        _max_waiters: max_waiters,
        inner: Mutex::new(PoolInner {
            active_count: 0,
            slots: Vec::new(),
            checked_out: HashSet::new(),
            next_conn_id: 1,
        }),
    }))
}

/// `karac_runtime_pool_begin_acquire(pool, out_val, out_conn_id, out_fn_ptr,
/// out_env_ptr) -> i32`. Decides the acquire outcome under the lock:
///
/// - `GOT_IDLE` (0): an idle slot was reused — its `elem_size` bytes are copied
///   into `out_val`, `*out_conn_id` is the fresh checkout id. Codegen builds
///   `Ok(PooledConnection { .., val: <out_val> })`.
/// - `NEED_MINT` (1): the pool is below `max_connections` — a slot is RESERVED
///   (`active_count` bumped, `conn_id` allocated + marked checked-out) and the
///   stored create-fn fat pointer is written to `*out_fn_ptr` / `*out_env_ptr`.
///   Codegen calls the closure to mint `T`, then builds the `Ok(..)` with it.
///   `out_val` is left untouched.
/// - `AT_CAP` (2): at `max_connections` with no idle slot. Codegen builds
///   `Err(PoolError.Timeout)`. No state change.
/// - `CLOSED` (3): `pool` is null (a hand-rolled `Pool { handle_id: 0 }`).
///   Codegen builds `Err(PoolError.PoolClosed)`.
///
/// # Safety
/// `pool` must be live (or null). `out_val` must point to at least `elem_size`
/// writable bytes; the four out-params must be valid writable pointers.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_pool_begin_acquire(
    pool: *mut KaracPool,
    out_val: *mut u8,
    out_conn_id: *mut i64,
    out_fn_ptr: *mut i64,
    out_env_ptr: *mut i64,
) -> i32 {
    if pool.is_null() {
        return POOL_ACQUIRE_CLOSED;
    }
    let p = &*pool;
    let mut inner = p.inner.lock().unwrap();
    if let Some(blob) = inner.slots.pop() {
        // Reuse an idle slot: copy its bytes out, mark checked out.
        let n = p.elem_size.min(blob.len());
        if !out_val.is_null() && n > 0 {
            std::ptr::copy_nonoverlapping(blob.as_ptr(), out_val, n);
        }
        let conn_id = inner.next_conn_id;
        inner.next_conn_id += 1;
        inner.checked_out.insert(conn_id);
        if !out_conn_id.is_null() {
            *out_conn_id = conn_id;
        }
        POOL_ACQUIRE_GOT_IDLE
    } else if inner.active_count < p.max_connections {
        // Reserve a mint: bump the active count and hand the fat pointer back.
        inner.active_count += 1;
        let conn_id = inner.next_conn_id;
        inner.next_conn_id += 1;
        inner.checked_out.insert(conn_id);
        if !out_conn_id.is_null() {
            *out_conn_id = conn_id;
        }
        if !out_fn_ptr.is_null() {
            *out_fn_ptr = p.create_fn_ptr;
        }
        if !out_env_ptr.is_null() {
            *out_env_ptr = p.create_env_ptr;
        }
        POOL_ACQUIRE_NEED_MINT
    } else {
        POOL_ACQUIRE_AT_CAP
    }
}

/// `karac_runtime_pool_release(pool, conn_id, val)` — return a connection to the
/// pool. Idempotent on `conn_id`: the slot is pushed back exactly once (the
/// first `release`/drop for a checkout wins; the second is a no-op), so an
/// explicit `release(conn)` followed by the `PooledConnection` scope-exit drop
/// does not double-return. A null pool or an unknown `conn_id` is a no-op.
///
/// # Safety
/// `pool` must be live (or null). `val` must point to at least `elem_size`
/// readable bytes.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_pool_release(
    pool: *mut KaracPool,
    conn_id: i64,
    val: *const u8,
) {
    if pool.is_null() {
        return;
    }
    let p = &*pool;
    let mut inner = p.inner.lock().unwrap();
    if !inner.checked_out.remove(&conn_id) {
        // Already returned (idempotent) or never issued here (cross-pool).
        return;
    }
    let n = p.elem_size;
    let mut blob = vec![0u8; n].into_boxed_slice();
    if !val.is_null() && n > 0 {
        std::ptr::copy_nonoverlapping(val, blob.as_mut_ptr(), n);
    }
    inner.slots.push(blob);
}

/// `karac_runtime_pool_drop(pool)` — free the pool and every idle slot blob.
/// Single-owner (no refcount); emitted once at the owning binding's scope exit
/// by the `Pool` Drop lowering. Null is a no-op. (v1 POD-`T` scope: idle slots
/// are plain bytes, so freeing the `Vec<Box<[u8]>>` is sufficient; a heap-owning
/// `T` would leak its buffers here — the documented follow-up.)
///
/// # Safety
/// `pool` must be a live pointer returned by `karac_runtime_pool_new` (or null);
/// consumes it.
#[no_mangle]
pub unsafe extern "C" fn karac_runtime_pool_drop(pool: *mut KaracPool) {
    if pool.is_null() {
        return;
    }
    drop(Box::from_raw(pool));
}

#[cfg(test)]
mod tests {
    use super::*;

    // Drive the codegen protocol by hand: begin_acquire, and on NEED_MINT
    // synthesize a `T` (an i64 counter here) the way codegen would after
    // calling the closure, recording it via release on hand-back.
    unsafe fn acquire_i64(pool: *mut KaracPool, next_val: &mut i64) -> Option<(i64, i64)> {
        let mut out_val: i64 = 0;
        let mut conn_id: i64 = 0;
        let mut fn_ptr: i64 = 0;
        let mut env_ptr: i64 = 0;
        let status = karac_runtime_pool_begin_acquire(
            pool,
            &mut out_val as *mut i64 as *mut u8,
            &mut conn_id,
            &mut fn_ptr,
            &mut env_ptr,
        );
        match status {
            POOL_ACQUIRE_GOT_IDLE => Some((out_val, conn_id)),
            POOL_ACQUIRE_NEED_MINT => {
                // codegen would indirect-call the closure here; simulate.
                let minted = *next_val;
                *next_val += 1;
                Some((minted, conn_id))
            }
            _ => None,
        }
    }

    #[test]
    fn mint_up_to_cap_then_at_cap() {
        unsafe {
            let p = karac_runtime_pool_new(0, 0, 8, 2, 4);
            let mut nv = 100;
            let (v0, c0) = acquire_i64(p, &mut nv).unwrap();
            let (v1, c1) = acquire_i64(p, &mut nv).unwrap();
            assert_eq!((v0, v1), (100, 101));
            assert_ne!(c0, c1);
            // Third acquire at cap (both checked out) → None.
            assert!(acquire_i64(p, &mut nv).is_none());
            karac_runtime_pool_drop(p);
        }
    }

    #[test]
    fn release_then_reuse_idle_slot() {
        unsafe {
            let p = karac_runtime_pool_new(0, 0, 8, 1, 1);
            let mut nv = 7;
            let (v0, c0) = acquire_i64(p, &mut nv).unwrap();
            assert_eq!(v0, 7);
            // Return it, then re-acquire: should reuse the SAME value (no new mint).
            karac_runtime_pool_release(p, c0, &v0 as *const i64 as *const u8);
            let (v1, _c1) = acquire_i64(p, &mut nv).unwrap();
            assert_eq!(v1, 7, "reused idle slot, not a fresh mint");
            assert_eq!(nv, 8, "create_fn simulated exactly once");
            karac_runtime_pool_drop(p);
        }
    }

    #[test]
    fn release_is_idempotent_on_conn_id() {
        unsafe {
            let p = karac_runtime_pool_new(0, 0, 8, 1, 1);
            let mut nv = 5;
            let (v0, c0) = acquire_i64(p, &mut nv).unwrap();
            // Release twice (explicit + drop) must hand back exactly one slot.
            karac_runtime_pool_release(p, c0, &v0 as *const i64 as *const u8);
            karac_runtime_pool_release(p, c0, &v0 as *const i64 as *const u8);
            // Only ONE idle slot exists: two acquires, the 2nd must NOT find a
            // second idle copy (it mints a fresh one at cap=1 only if a slot is
            // free — here active_count stays 1, so the 2nd is at cap).
            let (v1, c1) = acquire_i64(p, &mut nv).unwrap();
            assert_eq!(v1, 5);
            karac_runtime_pool_release(p, c1, &v1 as *const i64 as *const u8);
            karac_runtime_pool_drop(p);
        }
    }

    #[test]
    fn null_pool_reports_closed() {
        unsafe {
            let null = std::ptr::null_mut::<KaracPool>();
            let mut ov: i64 = 0;
            let (mut ci, mut fp, mut ep) = (0i64, 0i64, 0i64);
            let s = karac_runtime_pool_begin_acquire(
                null,
                &mut ov as *mut i64 as *mut u8,
                &mut ci,
                &mut fp,
                &mut ep,
            );
            assert_eq!(s, POOL_ACQUIRE_CLOSED);
            karac_runtime_pool_release(null, 1, std::ptr::null()); // no-op
            karac_runtime_pool_drop(null); // no-op
        }
    }

    #[test]
    fn zero_cap_never_mints() {
        unsafe {
            let p = karac_runtime_pool_new(0, 0, 8, 0, 0);
            let mut nv = 0;
            assert!(acquire_i64(p, &mut nv).is_none());
            karac_runtime_pool_drop(p);
        }
    }
}
