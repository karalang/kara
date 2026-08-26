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
    let (chunks, remainder) = bytes.as_chunks::<8>();
    for c in chunks {
        // `as_chunks::<8>()` yields `[u8; 8]` arrays directly — no fallible conversion.
        h = fx_add(h, u64::from_le_bytes(*c));
    }
    for &b in remainder {
        h = fx_add(h, u64::from(b));
    }
    fx_finalize(h)
}

/// Final avalanche applied to every [`fx_hash_bytes`] digest.
///
/// WHY THIS EXISTS, and why karac's Fx digest deliberately no longer matches
/// rustc-hash's byte for byte (B-2026-08-22-29).
///
/// `fx_add` ends in a multiply, and multiplication propagates bits only
/// UPWARD: bit `i` of a product depends on bits `<= i` of the operands. So the
/// LOW bits of an Fx digest are barely mixed -- for keys sharing a prefix and
/// differing near the end (`key-0` .. `key-199999`), they hardly move at all.
///
/// That is harmless in rustc, whose table indexes from the HIGH bits. karac's
/// open-addressed table takes the bucket index from the LOW ones
/// (`hash & (capacity - 1)`, see `runtime/src/map.rs`), so those same weak bits
/// choose the bucket. The measured result was `Map[String, i64, FxBuildHasher]`
/// running 13.7x SLOWER than the SipHash-1-3 default (arm64; ~11x on x86-64) --
/// the exact opposite of the speed-for-safety trade this hasher is offered as,
/// with a user-written FNV-1a at parity with the default as the control.
///
/// Rotating the digest, or moving the bucket index to the high bits, would each
/// fix the index while leaving the OTHER consumer weak: the control byte's tag
/// is drawn from the high bits precisely because the index uses the low ones,
/// and moving the index is a codegen-ABI change replicated across
/// `src/codegen/mono.rs`, `control_flow_for.rs` and `runtime.rs` whose
/// disagreement mode is a silently missed key. A full avalanche fixes every bit
/// for every consumer, in one place that both backends already funnel through.
///
/// This is `moremur`: five dependent ALU ops, negligible against the ~10x it
/// recovers, and Fx keeps its cheap per-byte loop, which is where its speed
/// advantage over SipHash actually lives.
#[inline]
fn fx_finalize(mut h: u64) -> u64 {
    h ^= h >> 27;
    h = h.wrapping_mul(0x3C79_AC49_2BA7_B653);
    h ^= h >> 33;
    h = h.wrapping_mul(0x1C69_B3F7_4AC4_AE35);
    h ^= h >> 27;
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
    sip::<1, 3>(bytes, k0, k1)
}

/// SipHash-**2-4** with an explicit 128-bit key — the STABLE digest behind
/// `StableHash.siphash24(bytes, k0, k1)` (design.md § `Hash` and `Hasher`,
/// stability policy).
///
/// # Why this exists next to a hash that is already SipHash
///
/// [`hash_bytes`] is deliberately UNSTABLE: it is keyed from a per-process
/// random seed, so its digest differs between two runs of the same binary.
/// That is the right default for a `Map`, and exactly wrong for the four use
/// cases design.md enumerates — content addressing, on-disk indexes, snapshot
/// tests, distributed sharding — where the whole point is that the same bytes
/// give the same number tomorrow, on another machine, under another build.
///
/// So this takes its key as an ARGUMENT and reads no process state. Same bytes
/// plus same key gives the same digest, always. `2-4` rather than the `1-3`
/// above because 2-4 is the round count the SipHash paper specifies as the
/// default, and therefore the one another language's `siphash24` will agree
/// with — interoperability is the property being bought here, and it is the
/// reason this is a separate entry point rather than `hash_bytes_with_key`
/// with the seed made explicit.
///
/// Verified against `std`'s (deprecated) `SipHasher`, which is SipHash-2-4 —
/// see the `agrees_with_stds_siphash24` test. That oracle also reproduces the
/// reference vectors from the paper's Appendix A, so agreeing with it is
/// agreeing with the specification.
pub fn siphash24(bytes: &[u8], k0: u64, k1: u64) -> u64 {
    sip::<2, 4>(bytes, k0, k1)
}

/// The absorb loop both round-count variants share, so the block split, the
/// little-endian word decode and the length byte exist ONCE. `C` compression
/// rounds per message word, `D` finalization rounds.
#[inline]
fn sip<const C: usize, const D: usize>(bytes: &[u8], k0: u64, k1: u64) -> u64 {
    let mut st = State::<C, D>::new(k0, k1);
    let len = bytes.len();
    let (chunks, rem) = bytes.as_chunks::<8>();
    for c in chunks {
        // `as_chunks::<8>()` yields `[u8; 8]` arrays directly — no fallible conversion.
        let m = u64::from_le_bytes(*c);
        st.round_msg(m);
    }
    // Final block: the remaining 0..=7 bytes, little-endian, with the low byte
    // of the input LENGTH in the top byte. That length byte is what keeps
    // `"a"` and `"a\0"` apart — without it every trailing-zero extension of a
    // key would collide.
    // `rem` (the 0..=7 trailing bytes) came from the `as_chunks` split above.
    let mut last = (len as u64 & 0xff) << 56;
    for (i, &b) in rem.iter().enumerate() {
        last |= (b as u64) << (8 * i);
    }
    st.round_msg(last);
    st.finish()
}

/// The SipHash permutation state, generic over the two round counts that name
/// the variant: `C` compression rounds per message word and `D` finalization
/// rounds.
///
/// `State<1, 3>` is SipHash-1-3, the `Map`/`Set` default — the tradeoff Rust's
/// `HashMap` also picked over the original paper's 2-4. `State<2, 4>` is the
/// paper's 2-4, used only by [`siphash24`], where matching other
/// implementations is the point.
///
/// The counts are const parameters rather than fields so each variant
/// monomorphizes to a straight-line unrolled permutation, exactly as the
/// hand-written 1-3 did before 2-4 joined it.
struct State<const C: usize, const D: usize> {
    v0: u64,
    v1: u64,
    v2: u64,
    v3: u64,
}

impl<const C: usize, const D: usize> State<C, D> {
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

    /// Absorb one 64-bit message word: xor in, `C` rounds, xor out.
    #[inline]
    fn round_msg(&mut self, m: u64) {
        self.v3 ^= m;
        for _ in 0..C {
            self.sip_round();
        }
        self.v0 ^= m;
    }

    #[inline]
    fn finish(mut self) -> u64 {
        self.v2 ^= 0xff;
        for _ in 0..D {
            self.sip_round();
        }
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

    /// [`siphash24`] against the same class of oracle: `std`'s (deprecated)
    /// `SipHasher` IS SipHash-2-4, so this checks the STABLE digest against an
    /// independent implementation rather than against constants typed in by
    /// hand.
    ///
    /// Non-zero keys are the point here, unlike the 1-3 test above: this
    /// function's key is a caller ARGUMENT rather than process state, so a
    /// variant that silently ignored `k0`/`k1` — or transposed them — would
    /// still pass a zero-key check while producing a digest nobody else
    /// computes. The keys below include the reference key from the SipHash
    /// paper's Appendix A (`0x0706..0900`, `0x0f0e..0908`), so the vectors the
    /// oracle reproduces are the specified ones.
    #[test]
    fn agrees_with_stds_siphash24() {
        use std::hash::Hasher;
        #[allow(deprecated)]
        use std::hash::SipHasher;
        let keys = [
            (0, 0),
            (0x0706_0504_0302_0100, 0x0f0e_0d0c_0b0a_0908),
            (u64::MAX, 1),
            (1, u64::MAX),
        ];
        for (k0, k1) in keys {
            for n in 0..=40usize {
                let input: Vec<u8> = (0..n).map(|i| (i as u8).wrapping_mul(31)).collect();
                #[allow(deprecated)]
                let mut oracle = SipHasher::new_with_keys(k0, k1);
                oracle.write(&input);
                assert_eq!(
                    siphash24(&input, k0, k1),
                    oracle.finish(),
                    "length {n} under key ({k0:#x}, {k1:#x}) disagrees with std's SipHash-2-4"
                );
            }
        }
    }

    /// The property the whole stable-hash surface is FOR: the digest depends on
    /// the caller's key and the bytes, and on nothing else. A regression that
    /// reintroduced a read of the process seed would pass every fixed-key
    /// comparison above if the oracle were seeded the same way — it is not, but
    /// stating the invariant directly is cheap and it is the one users rely on.
    ///
    /// `KARAC_HASH_SEED` is deliberately NOT set here: this binary seeds itself
    /// randomly like any other process, so a seed-reading `siphash24` would
    /// fail this on nearly every run.
    #[test]
    fn siphash24_is_stable_and_does_not_read_the_process_seed() {
        // Pinned constants, computed by the oracle above. They are what a user
        // doing content addressing writes into a snapshot test, so they must
        // survive a rebuild, a reseed, and a new machine.
        assert_eq!(siphash24(b"", 0, 0), 0x1e92_4b9d_7377_00d7);
        assert_eq!(siphash24(b"kara", 0, 0), 0x7a87_69d5_c8c7_ce3b);
        // The unstable default, on the same bytes, is a different function --
        // asserting they differ is what keeps a future refactor from
        // accidentally routing the stable entry point through the seeded one.
        assert_ne!(siphash24(b"kara", 0, 0), hash_bytes(b"kara"));
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
        // These are karac's OWN digests, not rustc-hash's: `fx_finalize`
        // deliberately diverges from it (B-2026-08-22-29 -- see that fn's docs
        // for why karac's table needs the avalanche and rustc's does not).
        // `b""` still hashes to 0 because the avalanche fixes 0, which is the
        // one value it cannot improve -- a pre-existing wart of the empty key,
        // unchanged by this.
        assert_eq!(fx_hash_bytes(b""), 0x0000_0000_0000_0000);
        assert_eq!(fx_hash_bytes(b"abc"), 0x8A79_FEFD_F5E0_1CA1);
        assert_eq!(fx_hash_bytes(b"12345678"), 0xC599_2C54_F582_2138);
        assert_eq!(fx_hash_bytes(b"123456789"), 0x8452_816B_70A2_28B3);
    }

    /// B-2026-08-22-29 — the LOW bits of an Fx digest must keep spreading as
    /// the table grows, because karac's open-addressed table takes the bucket
    /// index from exactly those bits (`hash & (capacity - 1)`,
    /// `runtime/src/map.rs`).
    ///
    /// Pinned as a DISTRIBUTION property, not a timing one: the symptom was a
    /// 13.7x slowdown, but a benchmark assertion would be flaky and would not
    /// say what broke. This measures the cause.
    ///
    /// WHAT THE CAUSE ACTUALLY IS -- measured, because the first version of
    /// this test asserted the wrong thing and passed with the mixer removed.
    /// Un-finalized Fx does not merely spread the low bits thinly; it hits a
    /// CEILING of about 1000 distinct low-bit patterns for `key-N` keys and
    /// stays there no matter how many bits the mask exposes:
    ///
    ///        keys / mask      raw Fx   finalized
    ///       4096 / 12-bit        985        2597
    ///      16384 / 14-bit       1033       10341
    ///     200000 / 18-bit       2057      126663
    ///
    /// So past ~1024 buckets a growing table cannot spread these keys at all,
    /// and every further insert lengthens a probe chain. That is why the
    /// slowdown scales with map size, and why a small-N test sees nothing --
    /// at 1024 keys over a 10-bit mask the two are indistinguishable (644 vs
    /// 632), which is exactly the trap this comment exists to stop.
    #[test]
    fn fx_low_bits_keep_spreading_as_the_table_grows() {
        const N: usize = 16_384;
        const BITS: u32 = 14;
        let buckets: std::collections::BTreeSet<u64> = (0..N)
            .map(|i| fx_hash_bytes(format!("key-{i}").as_bytes()) & ((1 << BITS) - 1))
            .collect();
        // Un-finalized Fx yields ~1033 here; a well-mixed digest yields ~10300
        // (the coupon-collector expectation for N keys into N slots is
        // N*(1 - 1/e) ~= 10360). The threshold sits far above the ceiling and
        // well below the expectation, so it pins the property rather than one
        // mixer's exact luck.
        assert!(
            buckets.len() > 5_000,
            "low bits stopped spreading: {} distinct buckets for {N} \
             prefix-sharing keys over a {BITS}-bit mask",
            buckets.len()
        );
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
