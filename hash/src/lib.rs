//! SipHash-1-3, the default `Map`/`Set` hasher (design.md § `Hash` and
//! `Hasher`, "Default hasher for v1"), plus the per-process seed both backends
//! read.
//!
//! # Why this is its own crate
//!
//! The interpreter and the compiled backends must hash through ONE
//! implementation, for the reason the Arrow IPC twin and `String.normalize`
//! share theirs: agreement by construction rather than by convention. The
//! interpreter reaches it directly (`karac` depends on this crate); the
//! compiled backends reach it through `karac_hash_bytes` in `karac-runtime`,
//! which is a thin wrapper over [`hash_bytes`]. Neither side open-codes the
//! permutation, so there is no second copy to drift.
//!
//! # Why SipHash-1-3 rather than the FxHash this replaced
//!
//! FxHash is a multiply-and-rotate mixer with a compile-time-constant seed. It
//! is fast and it is trivially floodable: an attacker who knows the constant —
//! and it is in the compiler's source — can generate unlimited colliding keys
//! offline and drive any `Map[String, _]` keyed on request data quadratic.
//! design.md names that exact threat ("the overwhelmingly common attack vector
//! is hash-flooding from adversarial input") and mandates a keyed,
//! DoS-resistant hash seeded per process. SipHash-1-3 is what Rust's own
//! `HashMap` defaults to.
//!
//! The security property comes from the SEED being secret and per-process, not
//! from the function alone. A keyed hash with a published seed is theatre.

#![forbid(unsafe_op_in_unsafe_fn)]

extern crate alloc as alloc_vec_crate;
/// `Vec` for the buffering hasher, routed through `alloc` so this crate stays
/// usable on the runtime's `no_std` targets.
mod alloc_vec {
    pub use super::alloc_vec_crate::vec::Vec;
}

mod seed;
pub use seed::{seed, seed_override_from_env, set_seed, SeedSource};

/// SipHash-1-3 over `bytes` with the process seed — the entry point both
/// backends bottom out in.
///
/// The seed is a 128-bit key split into the two halves SipHash takes; see
/// [`seed`] for how it is chosen and how to pin it.
#[inline]
pub fn hash_bytes(bytes: &[u8]) -> u64 {
    let (k0, k1) = seed();
    hash_bytes_with_key(bytes, k0, k1)
}

/// A [`core::hash::Hasher`] over SipHash-1-3 and the process seed, for callers
/// that build a digest out of heterogeneous pieces rather than one byte slice
/// — the interpreter's `hash_value`, which walks a `Value` tree.
///
/// Buffers what is written and hashes the concatenation on `finish`, so a
/// `KaraHasher` fed some bytes and [`hash_bytes`] called on those same bytes
/// agree by construction. Buffering rather than streaming the permutation is
/// deliberate: it is a handful of lines that cannot be wrong, against a
/// streaming tail-buffer that is the classic place to introduce an off-by-one,
/// and the interpreter's map is not the throughput-critical path (the compiled
/// backends call [`hash_bytes`] directly, with no buffer).
///
/// NOTE this does NOT reproduce `std`'s `SipHasher13` byte-for-byte for typed
/// writes: `write_u64` and friends encode differently. That is fine and
/// intended. The interpreter only has to agree with ITSELF; it never has to
/// match a digest computed by the compiled backends, because those run in a
/// different process with a different seed — which is exactly what design.md
/// § Map means by "iteration order ... varies across process runs".
#[derive(Default)]
pub struct KaraHasher {
    buf: alloc_vec::Vec<u8>,
}

impl KaraHasher {
    pub fn new() -> Self {
        Self::default()
    }
}

impl core::hash::Hasher for KaraHasher {
    fn write(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    fn finish(&self) -> u64 {
        hash_bytes(&self.buf)
    }
}

/// FxHash — the UNSEEDED, non-DoS-resistant hasher `Map[K, V, FxBuildHasher]`
/// opts into (design.md § `Hash` and `Hasher`, "Default hasher for v1":
/// "Users who pin the hasher (`Map[K, V, FxBuildHasher]`) opt out of both the
/// DoS-resistance guarantee and the version-stability escape hatch").
///
/// Same rotate-xor-multiply mixer rustc-hash uses, and the same one karac's
/// codegen inlined for every map before SipHash-1-3 became the default. The
/// multiplier is a compile-time constant and there is no key, which is
/// precisely the property being opted into: iteration order is then stable
/// across runs of one binary, and colliding keys can be generated offline by
/// anyone who reads this source. Never reach for it on a map keyed by input
/// you did not produce.
///
/// Eight-byte little-endian chunks, then the 0..=7 tail one byte at a time.
/// The tail is NOT length-mixed, so `b"a"` and `b"a\0"` collide — an
/// acceptable weakness for a hasher whose whole contract is "fast and
/// unkeyed", and a further reason this is opt-in rather than the default.
pub fn fx_hash_bytes(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0;
    let mut chunks = bytes.chunks_exact(8);
    for c in &mut chunks {
        // `chunks_exact(8)` yields exactly 8 bytes; the conversion cannot fail.
        h = fx_add(h, u64::from_le_bytes(c.try_into().unwrap()));
    }
    for &b in chunks.remainder() {
        h = fx_add(h, u64::from(b));
    }
    h
}

/// FxHash multiplier — rustc-hash's, and the one karac's codegen carried as
/// `FXHASH_SEED`. Named a "seed" there, but it is a fixed odd multiplier, not
/// a key: it is identical in every process and every build.
const FX_MULTIPLIER: u64 = 0x517c_c1b7_2722_0a95;

#[inline]
fn fx_add(h: u64, word: u64) -> u64 {
    (h.rotate_left(5) ^ word).wrapping_mul(FX_MULTIPLIER)
}

/// A [`core::hash::Hasher`] over [`fx_hash_bytes`], the unseeded sibling of
/// [`KaraHasher`]. Buffers for the same reason, and agrees with
/// [`fx_hash_bytes`] on the same bytes by the same construction.
#[derive(Default)]
pub struct FxHasher {
    buf: alloc_vec::Vec<u8>,
}

impl FxHasher {
    pub fn new() -> Self {
        Self::default()
    }
}

impl core::hash::Hasher for FxHasher {
    fn write(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    fn finish(&self) -> u64 {
        fx_hash_bytes(&self.buf)
    }
}

/// SipHash-1-3 with an explicit key. Exposed so tests can pin a key without
/// touching process state, and so the KATs below can run the published
/// reference vectors.
pub fn hash_bytes_with_key(bytes: &[u8], k0: u64, k1: u64) -> u64 {
    let mut st = State::new(k0, k1);
    let len = bytes.len();
    let mut chunks = bytes.chunks_exact(8);
    for c in &mut chunks {
        // `chunks_exact(8)` yields exactly 8 bytes, so the array conversion
        // cannot fail; `unwrap` here is unreachable rather than fallible.
        let m = u64::from_le_bytes(c.try_into().unwrap());
        st.round_msg(m);
    }
    // Final block: the remaining 0..=7 bytes, little-endian, with the low byte
    // of the input LENGTH in the top byte. That length byte is what keeps
    // `"a"` and `"a\0"` apart — without it every trailing-zero extension of a
    // key would collide.
    let rem = chunks.remainder();
    let mut last = (len as u64 & 0xff) << 56;
    for (i, &b) in rem.iter().enumerate() {
        last |= (b as u64) << (8 * i);
    }
    st.round_msg(last);
    st.finish()
}

/// The SipHash permutation state. `1` compression round per message word and
/// `3` finalization rounds — the "1-3" in the name, and the tradeoff Rust's
/// `HashMap` also picked over the original paper's 2-4.
struct State {
    v0: u64,
    v1: u64,
    v2: u64,
    v3: u64,
}

impl State {
    #[inline]
    fn new(k0: u64, k1: u64) -> Self {
        Self {
            v0: k0 ^ 0x736f_6d65_7073_6575,
            v1: k1 ^ 0x646f_7261_6e64_6f6d,
            v2: k0 ^ 0x6c79_6765_6e65_7261,
            v3: k1 ^ 0x7465_6462_7974_6573,
        }
    }

    /// One SipRound. Deliberately written as the reference ARX sequence rather
    /// than anything cleverer — this is the only place the algorithm lives, so
    /// it should read like the spec.
    #[inline]
    fn sip_round(&mut self) {
        self.v0 = self.v0.wrapping_add(self.v1);
        self.v1 = self.v1.rotate_left(13);
        self.v1 ^= self.v0;
        self.v0 = self.v0.rotate_left(32);
        self.v2 = self.v2.wrapping_add(self.v3);
        self.v3 = self.v3.rotate_left(16);
        self.v3 ^= self.v2;
        self.v0 = self.v0.wrapping_add(self.v3);
        self.v3 = self.v3.rotate_left(21);
        self.v3 ^= self.v0;
        self.v2 = self.v2.wrapping_add(self.v1);
        self.v1 = self.v1.rotate_left(17);
        self.v1 ^= self.v2;
        self.v2 = self.v2.rotate_left(32);
    }

    /// Absorb one 64-bit message word: xor in, one round, xor out.
    #[inline]
    fn round_msg(&mut self, m: u64) {
        self.v3 ^= m;
        self.sip_round();
        self.v0 ^= m;
    }

    #[inline]
    fn finish(mut self) -> u64 {
        self.v2 ^= 0xff;
        self.sip_round();
        self.sip_round();
        self.sip_round();
        self.v0 ^ self.v1 ^ self.v2 ^ self.v3
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The implementation is checked against an ORACLE, not against constants
    /// transcribed by hand: Rust's `DefaultHasher` IS SipHash-1-3, and with a
    /// zero key `Hasher::write` + `finish` is exactly this function's
    /// contract. Comparing to it catches a transposed rotation constant or a
    /// wrong round count, which still produce plausible-looking avalanche and
    /// which copied vectors would only catch if the copying were itself
    /// correct.
    ///
    /// Every length from 0 through 40 crosses the 8-byte block boundary five
    /// times, so tail handling and the length byte are both exercised at every
    /// offset rather than at one convenient size.
    #[test]
    fn agrees_with_stds_siphash13_on_a_zero_key() {
        use std::hash::Hasher;
        for n in 0..=40usize {
            let input: Vec<u8> = (0..n).map(|i| (i as u8).wrapping_mul(31)).collect();
            let mut oracle = std::collections::hash_map::DefaultHasher::new();
            oracle.write(&input);
            assert_eq!(
                hash_bytes_with_key(&input, 0, 0),
                oracle.finish(),
                "length {n} disagrees with std's SipHash-1-3"
            );
        }
    }

    /// The Fx digest is PINNED to constants, and the pin is what proves it
    /// never reads the process seed: this test binary seeds itself randomly
    /// like any other process, so a `fx_hash_bytes` that consulted `seed()`
    /// would fail here on almost every run rather than intermittently in the
    /// field. It doubles as a change-detector for the mixer.
    ///
    /// The four lengths straddle the 8-byte chunk boundary (empty, short tail,
    /// exactly one chunk, one chunk + tail) so a broken remainder loop cannot
    /// slip past.
    #[test]
    fn fx_is_pinned_and_therefore_unseeded() {
        assert_eq!(fx_hash_bytes(b""), 0x0000_0000_0000_0000);
        assert_eq!(fx_hash_bytes(b"abc"), 0x62FD_7437_241E_1ADF);
        assert_eq!(fx_hash_bytes(b"12345678"), 0xE871_2E7A_8344_2085);
        assert_eq!(fx_hash_bytes(b"123456789"), 0x8980_204C_4B0A_C4D4);
    }

    /// The two selectors must actually select different things — otherwise
    /// `Map[K, V, FxBuildHasher]` would resolve and mean nothing.
    #[test]
    fn fx_and_siphash_disagree() {
        let m = b"the quick brown fox";
        assert_ne!(fx_hash_bytes(m), hash_bytes(m));
        assert_ne!(fx_hash_bytes(m), hash_bytes_with_key(m, 0, 0));
    }

    /// `FxHasher` (the tree-walk interpreter's route) and `fx_hash_bytes`
    /// (codegen's, through `karac_hash_bytes_fx`) must agree on the same
    /// bytes, for the same reason `KaraHasher` and `hash_bytes` must.
    #[test]
    fn fx_hasher_agrees_with_fx_hash_bytes() {
        use core::hash::Hasher;
        for n in 0..=24usize {
            let input: Vec<u8> = (0..n).map(|i| (i as u8).wrapping_mul(17)).collect();
            let mut h = FxHasher::new();
            h.write(&input);
            assert_eq!(h.finish(), fx_hash_bytes(&input), "length {n} disagrees");
        }
    }

    /// A trailing NUL must not collide with its unpadded form. This is the
    /// property the length byte in the final block buys, and the one a
    /// hand-rolled tail is most likely to drop.
    #[test]
    fn length_is_mixed_so_zero_extension_does_not_collide() {
        assert_ne!(
            hash_bytes_with_key(b"a", 1, 2),
            hash_bytes_with_key(b"a\0", 1, 2)
        );
        assert_ne!(
            hash_bytes_with_key(b"", 1, 2),
            hash_bytes_with_key(b"\0", 1, 2)
        );
    }

    /// Different keys must give different digests for the same input —
    /// otherwise the per-process seed buys nothing and the whole point of
    /// this change is lost.
    #[test]
    fn the_key_actually_keys_the_hash() {
        let m = b"the quick brown fox";
        assert_ne!(
            hash_bytes_with_key(m, 0, 0),
            hash_bytes_with_key(m, 1, 0),
            "changing k0 must change the digest"
        );
        assert_ne!(
            hash_bytes_with_key(m, 0, 0),
            hash_bytes_with_key(m, 0, 1),
            "changing k1 must change the digest"
        );
    }

    /// Every input length across the 8-byte block boundary, so a tail-handling
    /// bug cannot hide in a length this crate happens not to test.
    #[test]
    fn every_tail_length_is_distinct() {
        let mut seen = std::collections::HashSet::new();
        for n in 0..=24usize {
            let input: Vec<u8> = (0..n).map(|i| i as u8).collect();
            assert!(
                seen.insert(hash_bytes_with_key(&input, 7, 9)),
                "length {n} collided with a shorter prefix"
            );
        }
    }
}
