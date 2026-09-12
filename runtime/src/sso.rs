//! Small-String Optimization (SSO) encoding — the executable contract.
//!
//! This module is the **single source of truth** for how a Kāra `String`
//! value packs short byte strings inline inside its own `{ptr, len, cap}`
//! descriptor, avoiding a heap allocation. Codegen re-emits this exact
//! logic as LLVM IR (see `src/codegen/sso.rs`), and the runtime FFI
//! decode path (`println` / file / http / … in a later slice) calls these
//! methods directly. Keeping one authoritative Rust implementation, with
//! exhaustive unit tests, lets us pin the contract that both sides must
//! agree on — a layout mismatch between codegen and runtime is silent data
//! corruption, exactly the failure class SSO's staging exists to prevent.
//!
//! See `docs/spikes/small-string-optimization.md` for the campaign design.
//!
//! ## Layout (little-endian, 24 bytes)
//!
//! A `String`/`Vec` descriptor is `{ data: *u8 (8B), len: i64 (8B),
//! cap: i64 (8B) }`. SSO reuses those 24 bytes without changing the
//! struct — three states, distinguished by the **`cap` field**:
//!
//! | state          | discriminant (`cap` viewed as `i64`) | drop action |
//! |----------------|--------------------------------------|-------------|
//! | static-heap    | `cap == 0`                           | none (rodata literal) |
//! | owned-heap     | `cap > 0`                            | `free(data)` |
//! | **inline**     | `cap < 0`  (sign bit set)            | none (bytes live in the struct) |
//!
//! The **inline flag is the sign bit (bit 63) of `cap`**. This choice is
//! load-bearing: it collapses the buffer-free decision to the single
//! signed predicate `cap > 0` ("owned-heap ⇔ signed-positive cap"), which
//! is a *provable no-op today* — no code has ever produced a `cap` with
//! bit 63 set (a real capacity never approaches 2^63 bytes), so `SGT cap,
//! 0` and the old `UGT cap, 0` are identical until inline construction is
//! switched on. `Vec` never sets the flag either, so every accessor here
//! is correctness-safe for `Vec` (it always takes the heap path).
//!
//! When inline, the 24 bytes hold (folly `fbstring` "small" style):
//!   - bytes `0..=22` — up to [`INLINE_CAPACITY`] = 23 data bytes,
//!     contiguous from the struct's own address;
//!   - byte `23` (the most-significant byte of `cap`) — `bit 7` is the
//!     inline flag, `bits 0..=6` hold the inline length (0..=23).
//!
//! Because the inline data overlaps *all three* fields, the length of an
//! inline string is NOT in the `len` field — it is decoded from `cap`'s
//! high byte. Reads that need a String's length or data pointer must
//! therefore route through [`RuntimeKaracString::byte_len`] /
//! [`RuntimeKaracString::data_ptr`] rather than reading the raw fields.

use crate::RuntimeKaracString;

// SSO's inline/heap views of the same 24 bytes only coincide on a
// little-endian target: the flag/length live in `cap`'s *integer* high bits
// (bit 63, bits 56..=62) while the inline data occupies the struct's low
// *bytes* (0..=22). Those two descriptions name the same storage byte
// (byte 23) only under little-endian byte order. Every Kāra target is
// little-endian (x86-64, arm64, wasm32); fail loudly rather than silently
// corrupt if that ever changes.
#[cfg(not(target_endian = "little"))]
compile_error!("SSO string encoding assumes a little-endian target");

/// The inline flag: bit 63 of the `cap` field. Set ⇒ the descriptor's 24
/// bytes hold the string inline; clear ⇒ `data`/`len`/`cap` are a heap
/// (or static-literal) descriptor.
pub const INLINE_FLAG: u64 = 1 << 63;

impl RuntimeKaracString {
    /// Maximum number of bytes storable inline (folly-style full overlay
    /// of the 24-byte descriptor minus the 1-byte flag/length trailer).
    pub const INLINE_CAPACITY: usize = 23;

    /// Whether the descriptor's 24 bytes are wholly covered by its three
    /// fields — the precondition the inline overlay cannot do without.
    ///
    /// `{*mut u8, i64, i64}` is gapless only when the pointer is 8 bytes.
    /// At any narrower pointer width the `i64` at offset 8 leaves a hole
    /// (wasm32: bytes 4..=7) that belongs to no field, and the overlay's
    /// bytes 0..=22 run straight through it.
    ///
    /// **A hole is fatal to the scheme, not merely awkward to encode.**
    /// Writing those bytes is easy enough — [`write_inline`] does it
    /// through a raw byte pointer. Keeping them is not: codegen moves a
    /// `String` as an LLVM aggregate value, and `load {ptr, i64, i64}`
    /// reads three FIELDS, not 24 bytes. `String.substring`'s SSO arm has
    /// the runtime write the descriptor into a slot and then loads it right
    /// back (`src/codegen/vec_method.rs`, `ss.load`), so on a padded target
    /// the hole's contents are dropped one instruction after being written,
    /// and every later store of that value writes three fields again.
    /// Making 32-bit work would mean turning every descriptor move in the
    /// compiler into a 24-byte `memcpy` — on the value type that moves most,
    /// to pessimise the 64-bit path this optimization exists to speed up.
    ///
    /// So inline construction is gated on this instead: where it is false,
    /// nothing ever builds an inline descriptor, no reader can meet one, and
    /// `String` behaves exactly as it did before SSO. Codegen gates its own
    /// construction sites on the same condition (`sso_on`), and the two
    /// construction entrypoints here refuse independently, so a gap on
    /// either side degrades to the heap path rather than to corruption.
    /// Measured as B-2026-09-12-20.
    pub const DESCRIPTOR_IS_GAPLESS: bool =
        core::mem::size_of::<Self>() == core::mem::size_of::<*mut u8>() + 16;

    /// True when the string is stored inline (no heap buffer).
    #[inline]
    pub fn is_inline(&self) -> bool {
        (self.cap as u64) & INLINE_FLAG != 0
    }

    /// True when the string's buffer is a static `.rodata` literal
    /// (`cap == 0`, flag clear) — it must NOT be freed.
    #[inline]
    pub fn is_static(&self) -> bool {
        self.cap == 0
    }

    /// True when the string owns a heap buffer that a drop must `free`.
    ///
    /// This is the signed predicate `cap > 0`, and it is exactly the gate
    /// codegen emits (`IntPredicate::SGT`): inline (`cap < 0`) and static
    /// (`cap == 0`) both answer `false`; only an owned malloc'd buffer
    /// (`cap > 0`) answers `true`.
    #[inline]
    pub fn is_owned_heap(&self) -> bool {
        self.cap > 0
    }

    /// The string's byte length, decoded from wherever the live state
    /// keeps it: `cap`'s high byte for inline, the `len` field otherwise.
    #[inline]
    pub fn byte_len(&self) -> usize {
        if self.is_inline() {
            (((self.cap as u64) >> 56) & 0x7f) as usize
        } else {
            self.len as usize
        }
    }

    /// A pointer to the string's first data byte. For an inline string
    /// this is the descriptor's own address (the bytes live there); for a
    /// heap/static string it is the `data` field.
    ///
    /// The inline pointer is valid only while `self` stays put — an inline
    /// descriptor is self-referential, so a *copy* of it must be re-read
    /// through this accessor, never have a previously-taken pointer reused.
    #[inline]
    pub fn data_ptr(&self) -> *const u8 {
        if self.is_inline() {
            self as *const Self as *const u8
        } else {
            self.data as *const u8
        }
    }

    /// Borrow the string's bytes, tag-aware. Safe view over the live state.
    ///
    /// The empty-with-null-data case is handled explicitly rather than
    /// falling through to `from_raw_parts`: the canonical empty String is
    /// `{null, 0, 0}` — what `karac_string_clone` and `karac_string_slice`
    /// both produce for an empty result — and `from_raw_parts(null, 0)` is
    /// UB even at length zero (Rust requires a non-null, aligned pointer
    /// regardless of length; the debug precondition check aborts on it).
    /// Found by `slice_into_boundary_is_exactly_inline_capacity` sweeping
    /// `n` from 0, which is exactly the state a length sweep hits first.
    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        let len = self.byte_len();
        let ptr = self.data_ptr();
        if len == 0 || ptr.is_null() {
            return &[];
        }
        // SAFETY: `data_ptr()` + `byte_len()` describe a contiguous,
        // initialized byte range for every state (inline bytes live in
        // `self`; heap/static bytes in `data`), borrowed for `self`'s
        // lifetime. Null/empty is handled above.
        unsafe { core::slice::from_raw_parts(ptr, len) }
    }

    /// Build an inline descriptor from `bytes` as a VALUE. Panics if `bytes`
    /// exceeds [`INLINE_CAPACITY`].
    ///
    /// Convenience over [`write_inline`], and the form the round-trip unit
    /// tests below are written against. **Runtime code should call
    /// `write_inline` instead**: returning the descriptor by value hands it to
    /// a typed move, which carries fields rather than bytes, so on a padded
    /// descriptor any content byte sitting in the hole would be lost on the
    /// way out. That is moot wherever inline construction is actually reachable
    /// — [`DESCRIPTOR_IS_GAPLESS`](Self::DESCRIPTOR_IS_GAPLESS) gates it, and a
    /// gapless descriptor has no hole to lose — but the value form is the one
    /// with the extra assumption, so it is the one not to build on.
    ///
    /// Codegen owns no copy of this encoding: its construction sites call
    /// `karac_string_try_inline_into` / `karac_string_slice_into`, which write
    /// through an out-pointer. This module is the only encoder there is.
    pub fn new_inline(bytes: &[u8]) -> Self {
        let mut slot = core::mem::MaybeUninit::<Self>::uninit();
        // SAFETY: `write_inline` initialises all 24 bytes of the descriptor
        // (data, then the flag/length trailer, then zero-fill), so the value
        // is fully initialised on return. The overlong case panics before
        // any write, and never reaches `assume_init`.
        unsafe {
            Self::write_inline(slot.as_mut_ptr(), bytes);
            slot.assume_init()
        }
    }

    /// Write an inline descriptor for `bytes` into `out`. Panics if `bytes`
    /// exceeds [`INLINE_CAPACITY`].
    ///
    /// **This, not [`new_inline`], is the form runtime code must use**, and
    /// the reason is padding. The overlay covers all 24 bytes of the
    /// descriptor, but on a target where `{*mut u8, i64, i64}` has a hole —
    /// any pointer width below 64, e.g. wasm32, where the `i64` at offset 8
    /// leaves bytes 4..=7 unaddressed by any field — bytes inside that hole
    /// belong to no field at all. Building the value field-by-field cannot
    /// write them, and Rust does not promise a *move* of the finished value
    /// carries padding either. Writing straight through `out as *mut u8`
    /// sidesteps both: the bytes land in the caller's storage, hole included,
    /// and nothing copies the value afterwards.
    ///
    /// That was not a hypothetical. Before this existed, the encoder packed
    /// the first eight content bytes into a `u64` and stored it through the
    /// `data` field, so on wasm32 the cast to a 4-byte pointer truncated
    /// bytes 4..=7 away and every inline string of length >= 5 came back with
    /// a four-byte hole of zeros (B-2026-09-12-20).
    ///
    /// # Safety
    ///
    /// `out` must point to a writable, suitably-aligned region of at least
    /// `size_of::<Self>()` bytes. It need not be initialised.
    pub unsafe fn write_inline(out: *mut Self, bytes: &[u8]) {
        assert!(
            bytes.len() <= Self::INLINE_CAPACITY,
            "write_inline: {} bytes exceeds inline capacity {}",
            bytes.len(),
            Self::INLINE_CAPACITY,
        );
        // SAFETY: the caller guarantees `out` is writable for 24 bytes. The
        // three writes below cover `0..len`, `len..23` and byte 23, i.e. the
        // whole descriptor, so no byte is left uninitialised.
        unsafe {
            let raw = out as *mut u8;
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), raw, bytes.len());
            // Zero the unused content bytes. Not cosmetic: `as_bytes` reads
            // only `byte_len()` of them, but a descriptor that is memcmp'd or
            // hashed as 24 opaque bytes would otherwise see leftover stack.
            core::ptr::write_bytes(raw.add(bytes.len()), 0, Self::INLINE_CAPACITY - bytes.len());
            // Byte 23 = flag (bit 7) | length (bits 0..=6).
            raw.add(Self::INLINE_CAPACITY)
                .write(0x80 | (bytes.len() as u8));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RuntimeKaracString;

    /// The encoding is layout-pinned to the 24-byte `{ptr, i64, i64}`
    /// descriptor shared with codegen; a size/offset drift breaks the
    /// codegen↔runtime contract.
    #[test]
    fn descriptor_layout_pinned() {
        assert_eq!(core::mem::size_of::<RuntimeKaracString>(), 24);
        assert_eq!(core::mem::align_of::<RuntimeKaracString>(), 8);
    }

    /// The canonical empty String is `{null, 0, 0}`, and `as_bytes` must
    /// survive it. `from_raw_parts(null, 0)` is UB even at length zero, so
    /// this is a real abort, not a pedantic one — it aborted the suite when
    /// `slice_into_boundary_is_exactly_inline_capacity` first swept n from 0.
    #[test]
    fn as_bytes_handles_the_canonical_null_empty_string() {
        let empty = RuntimeKaracString {
            data: core::ptr::null_mut(),
            len: 0,
            cap: 0,
        };
        assert_eq!(empty.byte_len(), 0);
        assert!(empty.data_ptr().is_null());
        assert_eq!(empty.as_bytes(), b"");
    }

    #[test]
    fn static_state_is_not_inline_not_owned() {
        // A static-literal descriptor: cap == 0, real (rodata) data ptr.
        let lit = b"hello";
        let s = RuntimeKaracString {
            data: lit.as_ptr() as *mut u8,
            len: lit.len() as i64,
            cap: 0,
        };
        assert!(!s.is_inline());
        assert!(s.is_static());
        assert!(!s.is_owned_heap());
        assert_eq!(s.byte_len(), 5);
        assert_eq!(s.as_bytes(), b"hello");
    }

    #[test]
    fn owned_heap_state_frees() {
        // An owned-heap descriptor: cap > 0. (No real malloc needed — we
        // only exercise the discriminant + length/data decode.)
        let buf = b"a longer heap-allocated string";
        let s = RuntimeKaracString {
            data: buf.as_ptr() as *mut u8,
            len: buf.len() as i64,
            cap: 64,
        };
        assert!(!s.is_inline());
        assert!(!s.is_static());
        assert!(s.is_owned_heap());
        assert_eq!(s.byte_len(), buf.len());
        assert_eq!(s.as_bytes(), buf);
    }

    #[test]
    fn inline_roundtrip_all_lengths() {
        for n in 0..=RuntimeKaracString::INLINE_CAPACITY {
            let bytes: Vec<u8> = (0..n).map(|i| b'A' + (i % 26) as u8).collect();
            let s = RuntimeKaracString::new_inline(&bytes);
            assert!(s.is_inline(), "len {n} should be inline");
            assert!(!s.is_static(), "inline is not static (len {n})");
            assert!(!s.is_owned_heap(), "inline is not owned-heap (len {n})");
            assert_eq!(s.byte_len(), n, "decoded length (len {n})");
            assert_eq!(s.as_bytes(), &bytes[..], "decoded bytes (len {n})");
        }
    }

    #[test]
    fn inline_flag_is_cap_sign_bit() {
        let s = RuntimeKaracString::new_inline(b"hi");
        // Sign bit set ⇒ cap reads negative as i64, and the owned-heap
        // gate (`cap > 0`) correctly excludes it.
        assert!(s.cap < 0);
        assert!(!s.is_owned_heap());
        assert_eq!((s.cap as u64) & INLINE_FLAG, INLINE_FLAG);
    }

    #[test]
    fn empty_inline_is_distinct_from_static_empty() {
        let inline_empty = RuntimeKaracString::new_inline(b"");
        assert!(inline_empty.is_inline());
        assert_eq!(inline_empty.byte_len(), 0);
        assert_eq!(inline_empty.as_bytes(), b"");
    }

    #[test]
    #[should_panic(expected = "exceeds inline capacity")]
    fn new_inline_rejects_overlong() {
        let too_long = [b'x'; RuntimeKaracString::INLINE_CAPACITY + 1];
        let _ = RuntimeKaracString::new_inline(&too_long);
    }

    #[test]
    fn max_inline_length_uses_full_capacity() {
        let bytes = [b'z'; RuntimeKaracString::INLINE_CAPACITY];
        let s = RuntimeKaracString::new_inline(&bytes);
        assert_eq!(s.byte_len(), RuntimeKaracString::INLINE_CAPACITY);
        assert_eq!(s.as_bytes(), &bytes[..]);
        // The whole 23-byte payload survives the pack/unpack round trip.
    }
}
