//! The hash FFI the compiled backends call — a thin shim over `karac-hash`,
//! which is the single implementation the interpreter also uses (design.md §
//! `Hash` and `Hasher`; B-2026-08-21-6).
//!
//! Codegen used to inline an FxHash byte loop with a compile-time-constant
//! seed into every per-type `hash_fn` it emitted. That is what made every
//! `Map[String, _]` floodable: the constant is in the compiler's source, so
//! colliding keys can be generated offline. The emitted `hash_fn` now reduces
//! its key to bytes and calls in here, so the algorithm and the per-process
//! seed live in ONE place shared with the interpreter rather than in two that
//! can drift.

/// `karac_hash_bytes(ptr, len) -> u64` — SipHash-1-3 of `len` bytes at `ptr`
/// under the process seed.
///
/// A null pointer or zero length hashes the empty input rather than trapping:
/// an empty `String` key is legal and reaches here as `(null, 0)`.
///
/// # Safety
/// `ptr` must be null, or point to `len` initialized readable bytes.
#[no_mangle]
pub unsafe extern "C" fn karac_hash_bytes(ptr: *const u8, len: usize) -> u64 {
    let bytes: &[u8] = if ptr.is_null() || len == 0 {
        &[]
    } else {
        unsafe { core::slice::from_raw_parts(ptr, len) }
    };
    karac_hash::hash_bytes(bytes)
}

/// `karac_hash_bytes_fx(ptr, len) -> u64` — FxHash of `len` bytes at `ptr`,
/// UNSEEDED. The `Map[K, V, FxBuildHasher]` opt-out; see
/// [`karac_hash::fx_hash_bytes`] for what is being given up.
///
/// Same null/empty contract as [`karac_hash_bytes`].
///
/// # Safety
/// `ptr` must be null, or point to `len` initialized readable bytes.
#[no_mangle]
pub unsafe extern "C" fn karac_hash_bytes_fx(ptr: *const u8, len: usize) -> u64 {
    let bytes: &[u8] = if ptr.is_null() || len == 0 {
        &[]
    } else {
        unsafe { core::slice::from_raw_parts(ptr, len) }
    };
    karac_hash::fx_hash_bytes(bytes)
}

/// `karac_stable_siphash24(ptr, len, k0, k1) -> u64` — SipHash-2-4 of `len`
/// bytes at `ptr` under the CALLER's 128-bit key. The compiled backend for
/// `StableHash.siphash24` (design.md § `Hash` and `Hasher`, stability policy).
///
/// Unlike [`karac_hash_bytes`] this reads NO process state, which is the whole
/// contract: content addressing, on-disk indexes, snapshot tests and
/// distributed sharding all need the same bytes to give the same number in a
/// different process, on a different machine, in a later build. The seeded
/// default cannot do that by design, and this is the escape hatch design.md
/// points those users at.
///
/// Same null/empty contract as [`karac_hash_bytes`]: an empty input is legal
/// and arrives as `(null, 0)`.
///
/// # Safety
/// `ptr` must be null, or point to `len` initialized readable bytes.
#[no_mangle]
pub unsafe extern "C" fn karac_stable_siphash24(
    ptr: *const u8,
    len: usize,
    k0: u64,
    k1: u64,
) -> u64 {
    let bytes: &[u8] = if ptr.is_null() || len == 0 {
        &[]
    } else {
        unsafe { core::slice::from_raw_parts(ptr, len) }
    };
    karac_hash::siphash24(bytes, k0, k1)
}

/// `karac_hash_seed() -> u64` — the low half of the process seed, for
/// diagnostics and for the tests that assert a pin took effect. Not used for
/// hashing; the seed reaches the hash through `karac-hash` directly.
#[no_mangle]
pub extern "C" fn karac_hash_seed() -> u64 {
    karac_hash::seed().0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The FFI shim must agree with the crate it wraps — otherwise the
    /// compiled backends and the interpreter would hash the same key
    /// differently, which is the exact class of divergence this whole change
    /// exists to remove.
    #[test]
    fn the_shim_agrees_with_the_shared_crate() {
        let msg = b"the quick brown fox";
        let direct = karac_hash::hash_bytes(msg);
        let viaffi = unsafe { karac_hash_bytes(msg.as_ptr(), msg.len()) };
        assert_eq!(direct, viaffi);
    }

    /// The Fx shim has the same agreement obligation, and must NOT be the
    /// seeded one wearing a different name.
    #[test]
    fn the_fx_shim_agrees_with_the_crate_and_differs_from_the_seeded_one() {
        let msg = b"the quick brown fox";
        let viaffi = unsafe { karac_hash_bytes_fx(msg.as_ptr(), msg.len()) };
        assert_eq!(karac_hash::fx_hash_bytes(msg), viaffi);
        assert_ne!(unsafe { karac_hash_bytes(msg.as_ptr(), msg.len()) }, viaffi);
    }

    /// An empty key arrives as `(null, 0)` from codegen and must hash, not
    /// trap.
    #[test]
    fn a_null_or_empty_key_hashes_the_empty_input() {
        let empty = karac_hash::hash_bytes(&[]);
        assert_eq!(unsafe { karac_hash_bytes(core::ptr::null(), 0) }, empty);
        assert_eq!(unsafe { karac_hash_bytes(b"x".as_ptr(), 0) }, empty);

        let empty_fx = karac_hash::fx_hash_bytes(&[]);
        assert_eq!(
            unsafe { karac_hash_bytes_fx(core::ptr::null(), 0) },
            empty_fx
        );
        assert_eq!(unsafe { karac_hash_bytes_fx(b"x".as_ptr(), 0) }, empty_fx);
    }
}
