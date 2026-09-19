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

/// `karac_hash_int(v, nbytes) -> u64` — SipHash-1-3 of the low `nbytes`
/// (1..=8) bytes of `v` under the process seed. The entry point compiled
/// INTEGER keys take.
///
/// Same digest as [`karac_hash_bytes`] over those bytes — `karac-hash`'s
/// `sip_int_matches_sip_bytes` proves it — but the key arrives in a REGISTER
/// instead of behind a pointer. That is the whole point: codegen had to spill
/// every integer key to a stack slot purely to have an address to pass, and
/// the callee then walked a run-time-length slice through `as_chunks::<8>()`
/// to re-discover a width the caller knew all along. Measured per 8-byte hash
/// (callgrind, loop overhead differenced): 97 instructions through the slice
/// path, 89 through this one, against 74 for Rust std's SipHash-1-3 on the
/// same input. See B-2026-09-07-42.
///
/// `nbytes` is CLAMPED to 8 rather than checked: this is an FFI boundary, and
/// a wider width would silently read key bytes that do not exist.
///
/// # Safety
/// Nothing is dereferenced; the value arrives by value. `unsafe` only for ABI
/// consistency with its siblings.
#[no_mangle]
pub unsafe extern "C" fn karac_hash_int(v: u64, nbytes: u64) -> u64 {
    karac_hash::hash_int(v, nbytes.min(8) as u32)
}

/// `karac_hash_word(v) -> u64` — [`karac_hash_int`] at the dominant width, with
/// the width pinned so the callee folds to straight-line code instead of
/// carrying both arms and the clamp across the FFI boundary. Measured on
/// kata:170: 106 instructions per probe through `karac_hash_int`, 95 through
/// this. B-2026-09-07-42.
///
/// NAMING: this was `karac_hash_u64` until B-2026-09-19-8, and that name was
/// a miscompile. Codegen synthesizes a per-key-type `karac_hash_{Type}(ptr)`
/// for every Map/Set key and reuses any module function already carrying the
/// name — so for a `u64` key it picked up THIS by-value extern as the map's
/// pointer-taking `hash_fn`, hashed the key slot's ADDRESS, and every `u64`
/// lookup missed. No runtime hash symbol may be spelled `karac_hash_<type>`.
///
/// # Safety
/// Nothing is dereferenced; `unsafe` only for ABI consistency with its
/// siblings.
#[no_mangle]
pub unsafe extern "C" fn karac_hash_word(v: u64) -> u64 {
    karac_hash::hash_u64(v)
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

/// `karac_hash_int_fx(v, nbytes) -> u64` / `karac_hash_word_fx(v) -> u64` — the
/// `FxBuildHasher` siblings of [`karac_hash_int`] / [`karac_hash_word`], same
/// digest as [`karac_hash_bytes_fx`] over the key's little-endian bytes.
///
/// The opt-out needed these more than the default did: the Fx mixing is 12
/// instructions inlined but 50 through the byte-slice boundary, so four fifths
/// of what a user chose `FxBuildHasher` to avoid was the boundary, not the
/// hash. B-2026-09-07-53.
///
/// # Safety
/// Nothing is dereferenced; `unsafe` only for ABI consistency with its
/// siblings.
#[no_mangle]
pub unsafe extern "C" fn karac_hash_int_fx(v: u64, nbytes: u64) -> u64 {
    karac_hash::fx_hash_int(v, nbytes.min(8) as u32)
}

/// See [`karac_hash_int_fx`].
///
/// # Safety
/// Nothing is dereferenced; `unsafe` only for ABI consistency.
#[no_mangle]
pub unsafe extern "C" fn karac_hash_word_fx(v: u64) -> u64 {
    karac_hash::fx_hash_u64(v)
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
