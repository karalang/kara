//! The per-process hash seed.
//!
//! design.md § `Hash` and `Hasher` requires the default hasher to be "seeded
//! from a per-process random source", and § Map spells out the consequence:
//! "Iteration order is unspecified and varies across process runs". Both
//! properties come from this file.
//!
//! # Reproducibility
//!
//! A genuinely random seed is what makes hash-flooding infeasible, and it is
//! also what makes a `Map`-iterating program's output differ run to run. That
//! is intended — design.md points order-dependent code at `SortedMap` — but a
//! test harness still needs to pin it. `KARAC_HASH_SEED` does that: set it to
//! an integer (decimal, or `0x`-prefixed hex) and the process uses that seed
//! instead of a random one. The compiler's own test suites and the kata A/B
//! harness set it, which is how `karac run` and `karac build` can still be
//! compared byte-for-byte.
//!
//! `KARAC_HASH_SEED=0` is a legal pin, not "unset" — the override is keyed on
//! the variable being PRESENT and parseable.

use core::sync::atomic::{AtomicU64, Ordering};

/// Where the live seed came from. Reported by `karac --version`-adjacent
/// diagnostics and by the tests that assert the override took effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeedSource {
    /// Freshly generated for this process.
    Random,
    /// Pinned via `KARAC_HASH_SEED`.
    Env,
    /// Set programmatically through [`set_seed`].
    Explicit,
}

// The seed is stored as two words plus an "initialized" flag rather than an
// `OnceLock`, so this crate stays `no_std`-compatible for the embedded and
// wasm runtime targets, which `OnceLock` (std) would rule out.
static K0: AtomicU64 = AtomicU64::new(0);
static K1: AtomicU64 = AtomicU64::new(0);
static INIT: AtomicU64 = AtomicU64::new(0);

/// The process seed, generating one on first use.
///
/// Idempotent and thread-safe: concurrent first callers may each compute a
/// candidate, but exactly one wins the `INIT` compare-exchange and every
/// caller then reads the winner's value, so all of them agree. That matters —
/// two tasks disagreeing about the seed would put the same key in two buckets.
#[inline]
pub fn seed() -> (u64, u64) {
    if INIT.load(Ordering::Acquire) == 0 {
        init_once();
    }
    (K0.load(Ordering::Relaxed), K1.load(Ordering::Relaxed))
}

/// Pin the seed explicitly. Intended for tests and for a host embedding that
/// wants reproducibility on its own terms; a no-op once the seed is live, so
/// it cannot re-bucket a `Map` that already has entries in it.
pub fn set_seed(k0: u64, k1: u64) -> bool {
    store(k0, k1, SeedSource::Explicit)
}

fn store(k0: u64, k1: u64, _src: SeedSource) -> bool {
    if INIT
        .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return false;
    }
    K0.store(k0, Ordering::Relaxed);
    K1.store(k1, Ordering::Relaxed);
    INIT.store(2, Ordering::Release);
    true
}

fn init_once() {
    let (k0, k1) = match seed_override_from_env() {
        Some(v) => derive_pair(v),
        None => random_pair(),
    };
    if !store(k0, k1, SeedSource::Random) {
        // Another thread won; spin until it publishes. The window is a handful
        // of instructions, so a yield-free spin is right here.
        while INIT.load(Ordering::Acquire) != 2 {
            core::hint::spin_loop();
        }
    }
}

/// The `KARAC_HASH_SEED` override, parsed. `None` when unset, empty or
/// unparseable — an unparseable value falls back to a random seed rather than
/// failing the process, because a mistyped env var should not make a program
/// refuse to run, and the security posture of the fallback is the STRONGER
/// one.
#[cfg(feature = "std")]
pub fn seed_override_from_env() -> Option<u64> {
    let raw = std::env::var("KARAC_HASH_SEED").ok()?;
    parse_seed(&raw)
}

/// `no_std` builds (embedded, and the wasm archives that drop `std::env`) have
/// no environment to read, so the override is compiled out and the seed is
/// always the random one.
#[cfg(not(feature = "std"))]
pub fn seed_override_from_env() -> Option<u64> {
    None
}

/// Only the `std` build has an environment to read a pin OUT of, so under
/// `no_std` this is dead — and `cargo clippy --target wasm32-wasip1
/// --no-default-features` says so. Gated on `std` OR `test` rather than
/// deleted, because the parsing rules (hex, decimal, whitespace, and `0` as a
/// legal pin) are exactly the part worth unit-testing.
#[cfg(any(feature = "std", test))]
fn parse_seed(raw: &str) -> Option<u64> {
    let t = raw.trim();
    if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).ok()
    } else {
        t.parse::<u64>().ok()
    }
}

/// Spread one 64-bit pin across both key halves. A user pinning
/// `KARAC_HASH_SEED=1` should get a well-mixed key, not `k0 = 1, k1 = 0` —
/// SipHash tolerates weak keys, but a pinned run should still look like a
/// realistic bucket distribution or it is a poor stand-in for the random case
/// in tests.
fn derive_pair(v: u64) -> (u64, u64) {
    let k0 = splitmix64(v);
    let k1 = splitmix64(k0);
    (k0, k1)
}

/// SplitMix64 — the standard finalizer, used only to spread a pinned seed and
/// to whiten the entropy source below. Not the hash itself.
fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// Per-process entropy.
///
/// `RandomState`'s own trick: the address of a fresh heap allocation is
/// ASLR-derived, and the monotonic clock supplies the rest. This is
/// deliberately NOT a cryptographic RNG — it does not need to be. The seed
/// must be unpredictable to a remote attacker submitting keys, not
/// unguessable by someone with local process introspection, and it must cost
/// nothing at startup and pull in no dependency that has to cross the wasm and
/// Cortex-M target matrix.
#[cfg(feature = "std")]
fn random_pair() -> (u64, u64) {
    use std::time::{SystemTime, UNIX_EPOCH};
    let boxed = Box::new(0u8);
    let addr = (&*boxed as *const u8) as u64;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let tid = {
        // Thread identity varies between processes on every platform that has
        // it and costs nothing where it does not.
        let t = std::thread::current();
        let id = format!("{:?}", t.id());
        let mut acc = 0u64;
        for b in id.as_bytes() {
            acc = acc.rotate_left(7) ^ (*b as u64);
        }
        acc
    };
    let k0 = splitmix64(addr ^ nanos);
    let k1 = splitmix64(k0 ^ tid ^ addr.rotate_left(17));
    (k0, k1)
}

#[cfg(not(feature = "std"))]
fn random_pair() -> (u64, u64) {
    // No clock and no allocator to mine for entropy. A fixed key is the honest
    // answer on these targets: it is what the platform can support, and the
    // hash-flooding threat model (a network service taking adversarial keys)
    // does not describe a Cortex-M build.
    (0x0706_0504_0302_0100, 0x0f0e_0d0c_0b0a_0908)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_and_decimal_pins_both_parse() {
        assert_eq!(parse_seed("42"), Some(42));
        assert_eq!(parse_seed("0x2a"), Some(42));
        assert_eq!(parse_seed("0X2A"), Some(42));
        assert_eq!(parse_seed("  7  "), Some(7));
        assert_eq!(parse_seed(""), None);
        assert_eq!(parse_seed("banana"), None);
    }

    /// Zero is a legal pin, not a synonym for "unset" — a harness that wants
    /// the all-zero key must be able to ask for it.
    #[test]
    fn zero_is_a_real_pin() {
        assert_eq!(parse_seed("0"), Some(0));
    }

    /// A pinned seed must still produce a well-mixed key pair; `k1 == 0` would
    /// make a pinned test run a poor stand-in for a random one.
    #[test]
    fn a_pinned_seed_is_spread_across_both_key_halves() {
        let (k0, k1) = derive_pair(1);
        assert_ne!(k0, 0);
        assert_ne!(k1, 0);
        assert_ne!(k0, k1);
        assert_ne!(derive_pair(1), derive_pair(2));
        // Deterministic: the whole point of pinning.
        assert_eq!(derive_pair(99), derive_pair(99));
    }
}
