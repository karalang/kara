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
    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        // SAFETY: `data_ptr()` + `byte_len()` describe a contiguous,
        // initialized byte range for every state (inline bytes live in
        // `self`; heap/static bytes in `data`), borrowed for `self`'s
        // lifetime.
        unsafe { core::slice::from_raw_parts(self.data_ptr(), self.byte_len()) }
    }

    /// Build an inline descriptor from `bytes`. Panics if `bytes` exceeds
    /// [`INLINE_CAPACITY`]. This is the reference encoder — codegen's
    /// inline-construction path (a later slice) emits the equivalent store
    /// sequence, and it anchors the round-trip unit tests below.
    pub fn new_inline(bytes: &[u8]) -> Self {
        assert!(
            bytes.len() <= Self::INLINE_CAPACITY,
            "new_inline: {} bytes exceeds inline capacity {}",
            bytes.len(),
            Self::INLINE_CAPACITY,
        );
        let mut raw = [0u8; 24];
        raw[..bytes.len()].copy_from_slice(bytes);
        // Byte 23 = flag (bit 7) | length (bits 0..=6).
        raw[23] = 0x80 | (bytes.len() as u8);
        let data = u64::from_le_bytes(raw[0..8].try_into().unwrap());
        let len = i64::from_le_bytes(raw[8..16].try_into().unwrap());
        let cap = i64::from_le_bytes(raw[16..24].try_into().unwrap());
        RuntimeKaracString {
            data: data as *mut u8,
            len,
            cap,
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
