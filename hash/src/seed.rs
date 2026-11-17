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
// Exported, with [`WORD_STATE`], for compiled code: a word key's hash is
// emitted in line from these (src/backend/mir/key.rs `hash_word`), and
// `karac_hash_word` is called only while this is not yet 2.
#[export_name = "karac_hash_seed_ready"]
#[doc(hidden)]
pub static INIT: AtomicU64 = AtomicU64::new(0);

/// [`crate::word_state`] of the live key, for compiled code that hashes a
/// word in line ([`crate::hash_u64_from_state`]). Written before `INIT`
/// becomes 2, so a reader that saw 2 reads the whole of it.
#[export_name = "karac_hash_word_state"]
pub static WORD_STATE: [AtomicU64; 5] = [const { AtomicU64::new(0) }; 5];

/// The process seed, generating one on first use.
///
/// Idempotent and thread-safe: concurrent first callers may each compute a
/// candidate, but exactly one wins the `INIT` compare-exchange and every
/// caller then reads the winner's value, so all of them agree. That matters —
/// two tasks disagreeing about the seed would put the same key in two buckets.
#[inline]
pub fn seed() -> (u64, u64) {
    // Only 2 means the key is stored. The winner of `store`'s
    // compare-exchange sets 1 before it writes the key, so a caller that saw
    // 1 and read the key anyway hashed with a key that was still 0 or half
    // written, and the `Map` it was filling lost those entries once the real
    // key landed (B-2026-10-03-15). `init_once` waits for 2 in that case.
    if INIT.load(Ordering::Acquire) != 2 {
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
    for (w, v) in WORD_STATE.iter().zip(crate::word_state(k0, k1)) {
        w.store(v, Ordering::Relaxed);
    }
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
/// one. It says so on stderr, once (the seed is read once per process): a
/// mistyped pin used to lose the reproduction without a word
/// (B-2026-11-16-288).
#[cfg(all(feature = "std", not(unix)))]
pub fn seed_override_from_env() -> Option<u64> {
    let raw = std::env::var_os("KARAC_HASH_SEED")?;
    let raw = raw.to_string_lossy();
    let seed = parse_seed(&raw);
    if seed.is_none() && !raw.trim().is_empty() {
        let mut msg = [0u8; MALFORMED_SEED_MAX];
        let n = malformed_seed_warning(raw.as_bytes(), &mut msg);
        eprint!("{}", String::from_utf8_lossy(&msg[..n]));
    }
    seed
}

/// The override through libc's `getenv`: `std::env::var` reaches a panic,
/// which in a compiled program keeps std's panic hook and backtrace
/// symbolizer (~250 KB) in every binary that hashes a key (B-2026-10-29-41).
#[cfg(all(feature = "std", unix))]
pub fn seed_override_from_env() -> Option<u64> {
    let raw = lean::getenv(c"KARAC_HASH_SEED")?;
    let seed = core::str::from_utf8(raw).ok().and_then(parse_seed);
    if seed.is_none() && !raw.trim_ascii().is_empty() {
        let mut msg = [0u8; MALFORMED_SEED_MAX];
        let n = malformed_seed_warning(raw, &mut msg);
        lean::write_stderr(msg.get(..n).unwrap_or_default());
    }
    seed
}

/// The longest malformed value quoted in the warning, and the buffer the
/// warning is built in.
#[cfg(any(feature = "std", target_os = "wasi"))]
const MALFORMED_SEED_QUOTE: usize = 40;
#[cfg(any(feature = "std", target_os = "wasi"))]
const MALFORMED_SEED_MAX: usize = 160;

/// The warning for a `KARAC_HASH_SEED` value that is not a seed, written
/// into `out` without allocating (a compiled program builds it before
/// anything else runs); its length.
#[cfg(any(feature = "std", target_os = "wasi"))]
fn malformed_seed_warning(raw: &[u8], out: &mut [u8; MALFORMED_SEED_MAX]) -> usize {
    let quoted = raw
        .get(..raw.len().min(MALFORMED_SEED_QUOTE))
        .unwrap_or_default();
    let more: &[u8] = if raw.len() > MALFORMED_SEED_QUOTE {
        b"..."
    } else {
        b""
    };
    let parts: [&[u8]; 4] = [
        b"warning: KARAC_HASH_SEED=`",
        quoted,
        more,
        b"` is not a seed (a decimal or 0x-hexadecimal u64), so this run's hash seed is \
          random\n",
    ];
    // Byte by byte through `get_mut`, so no slice bound can reach a panic
    // (a panic path links std's panic hook into every program).
    let mut n = 0;
    for &b in parts.iter().flat_map(|p| p.iter()) {
        if let Some(slot) = out.get_mut(n) {
            *slot = b;
            n += 1;
        }
    }
    n
}

/// The wasm archives build this crate without `std`, but WASI has an
/// environment: read the override through `environ_get`, so a pinned run
/// reproduces on `wasm_wasi` as it does natively (B-2026-11-17-64).
#[cfg(all(not(feature = "std"), target_os = "wasi"))]
pub fn seed_override_from_env() -> Option<u64> {
    let raw = wasi::env_var(b"KARAC_HASH_SEED")?;
    let seed = core::str::from_utf8(&raw).ok().and_then(parse_seed);
    if seed.is_none() && !raw.trim_ascii().is_empty() {
        let mut msg = [0u8; MALFORMED_SEED_MAX];
        let n = malformed_seed_warning(&raw, &mut msg);
        wasi::write_stderr(msg.get(..n).unwrap_or_default());
    }
    seed
}

/// An embedded `no_std` build has no environment to read, so the override is
/// compiled out there.
#[cfg(all(not(feature = "std"), not(target_os = "wasi")))]
pub fn seed_override_from_env() -> Option<u64> {
    None
}

/// Only the `std` build has an environment to read a pin OUT of, so under
/// `no_std` this is dead — and `cargo clippy --target wasm32-wasip1
/// --no-default-features` says so. Gated on `std` OR `test` rather than
/// deleted, because the parsing rules (hex, decimal, whitespace, and `0` as a
/// legal pin) are exactly the part worth unit-testing.
#[cfg(any(feature = "std", test, target_os = "wasi"))]
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
/// ASLR-derived, and the clock, the process id and a stack address supply
/// the rest. This is
/// deliberately NOT a cryptographic RNG — it does not need to be. The seed
/// must be unpredictable to a remote attacker submitting keys, not
/// unguessable by someone with local process introspection, and it must cost
/// nothing at startup and pull in no dependency that has to cross the wasm and
/// Cortex-M target matrix.
#[cfg(feature = "std")]
fn random_pair() -> (u64, u64) {
    // The operating system's random source first (`/dev/urandom` on Unix,
    // WASI's `random_get`), as collections.md's "a random value" reads
    // (B-2026-11-17-64); the mix below where it is not available.
    if let Some(pair) = os_entropy() {
        return pair;
    }
    let (addr, nanos) = heap_address_and_clock();
    // The process id differs between processes, and the address of a stack
    // local carries the stack's ASLR offset, which is independent of the
    // heap's. Neither allocates. This term used to be the current thread's id
    // (B-2026-10-03-11): `std::thread::current()` allocates the main thread's
    // handle and never frees it, so valgrind reported a "possibly lost" block
    // in every compiled program that hashes a key, and the main thread's id is
    // `ThreadId(1)` in every process, so it added nothing where most seeds are
    // drawn.
    let local = 0u8;
    let stack = (core::hint::black_box(&local) as *const u8) as u64;
    let pid = std::process::id() as u64;
    let k0 = splitmix64(addr ^ nanos);
    let k1 = splitmix64(k0 ^ pid ^ stack.rotate_left(29) ^ addr.rotate_left(17));
    (k0, k1)
}

/// The address of a fresh heap allocation and the wall clock in
/// nanoseconds.
#[cfg(all(feature = "std", not(unix)))]
fn heap_address_and_clock() -> (u64, u64) {
    use std::time::{SystemTime, UNIX_EPOCH};
    let boxed = Box::new(0u8);
    let addr = (&*boxed as *const u8) as u64;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    (addr, nanos)
}

/// As above, through libc: `Box::new`'s allocation-failure path and
/// `SystemTime`'s subtraction both reach a panic (B-2026-10-29-41).
#[cfg(all(feature = "std", unix))]
fn heap_address_and_clock() -> (u64, u64) {
    (lean::heap_address(), lean::clock_nanos())
}

/// libc calls in place of std's, none of which can panic.
#[cfg(all(feature = "std", unix))]
mod lean {
    extern "C" {
        #[link_name = "getenv"]
        fn c_getenv(name: *const u8) -> *const u8;
        fn malloc(size: usize) -> *mut core::ffi::c_void;
        fn free(ptr: *mut core::ffi::c_void);
        fn time(t: *mut i64) -> i64;
    }

    /// The value of the environment variable `name`.
    pub(super) fn getenv(name: &core::ffi::CStr) -> Option<&'static [u8]> {
        // SAFETY: `name` is NUL-terminated; nothing here writes the
        // environment.
        let v = unsafe { c_getenv(name.as_ptr().cast()) };
        if v.is_null() {
            None
        } else {
            // SAFETY: `getenv` answers a NUL-terminated string.
            Some(unsafe { core::ffi::CStr::from_ptr(v.cast()) }.to_bytes())
        }
    }

    /// `bytes` to stderr, as far as one `write` takes them.
    pub(super) fn write_stderr(bytes: &[u8]) {
        extern "C" {
            fn write(fd: i32, buf: *const u8, n: usize) -> isize;
        }
        // SAFETY: `bytes` is a live buffer of `bytes.len()` bytes.
        let _ = unsafe { write(2, bytes.as_ptr(), bytes.len()) };
    }

    /// The address of a fresh heap allocation, freed again.
    pub(super) fn heap_address() -> u64 {
        // SAFETY: a one-byte allocation, freed at once; only its address is
        // kept (null when there was none).
        unsafe {
            let p = malloc(1);
            free(p);
            p as u64
        }
    }

    /// The wall clock in nanoseconds since the epoch.
    pub(super) fn clock_nanos() -> u64 {
        #[cfg(all(
            any(target_os = "linux", target_os = "macos"),
            target_pointer_width = "64"
        ))]
        {
            #[repr(C)]
            struct Timespec {
                sec: i64,
                nsec: i64,
            }
            extern "C" {
                fn clock_gettime(clock: i32, t: *mut Timespec) -> i32;
            }
            let mut t = Timespec { sec: 0, nsec: 0 };
            // SAFETY: CLOCK_REALTIME (0 on Linux and macOS) into a
            // 64-bit `timespec`.
            if unsafe { clock_gettime(0, &mut t) } == 0 {
                return (t.sec as u64)
                    .wrapping_mul(1_000_000_000)
                    .wrapping_add(t.nsec as u64);
            }
        }
        // SAFETY: a null result pointer is allowed.
        (unsafe { time(core::ptr::null_mut()) } as u64).wrapping_mul(1_000_000_000)
    }
}

#[cfg(not(feature = "std"))]
fn random_pair() -> (u64, u64) {
    // WASI's `random_get` on `wasm_wasi`, whose archives build this crate
    // without `std`: a wasm service hashing request keys needs the same
    // flooding protection a native one has (B-2026-11-17-64). An embedded
    // build has no clock and no allocator to mine for entropy, and a fixed
    // key is the honest answer there: the hash-flooding threat model (a
    // network service taking adversarial keys) does not describe a Cortex-M
    // build.
    os_entropy().unwrap_or((0x0706_0504_0302_0100, 0x0f0e_0d0c_0b0a_0908))
}

/// 16 bytes from the operating system's random source, as the two key
/// halves; `None` where there is none or it failed.
#[cfg(all(unix, feature = "std"))]
fn os_entropy() -> Option<(u64, u64)> {
    // `/dev/urandom` through plain `open`/`read`, not `getentropy`: glibc
    // before 2.25 has no `getentropy`, and the cross-compile jobs link
    // against such a sysroot (`undefined reference to getentropy`). The
    // path is a static C string, so nothing is allocated (B-2026-10-03-11).
    extern "C" {
        fn open(path: *const core::ffi::c_char, flags: i32, ...) -> i32;
        fn read(fd: i32, buf: *mut u8, count: usize) -> isize;
        fn close(fd: i32) -> i32;
    }
    const O_RDONLY: i32 = 0;
    let mut b = [0u8; 16];
    // SAFETY: the path is NUL-terminated; `b` is 16 writable bytes and the
    // descriptor is closed before returning.
    unsafe {
        let fd = open(c"/dev/urandom".as_ptr(), O_RDONLY);
        if fd < 0 {
            return None;
        }
        let mut got = 0usize;
        while got < b.len() {
            let n = read(fd, b.as_mut_ptr().add(got), b.len() - got);
            if n <= 0 {
                close(fd);
                return None;
            }
            got += n as usize;
        }
        close(fd);
    }
    Some(halves(&b))
}

#[cfg(target_os = "wasi")]
fn os_entropy() -> Option<(u64, u64)> {
    let mut b = [0u8; 16];
    wasi::random(&mut b).then(|| halves(&b))
}

#[cfg(not(any(all(unix, feature = "std"), target_os = "wasi")))]
fn os_entropy() -> Option<(u64, u64)> {
    None
}

#[cfg(any(all(unix, feature = "std"), target_os = "wasi"))]
fn halves(b: &[u8; 16]) -> (u64, u64) {
    let mut lo = [0u8; 8];
    let mut hi = [0u8; 8];
    lo.copy_from_slice(&b[..8]);
    hi.copy_from_slice(&b[8..]);
    (u64::from_le_bytes(lo), u64::from_le_bytes(hi))
}

/// The two WASI preview 1 calls the seed needs, imported directly so the
/// `no_std` wasm build has them.
#[cfg(target_os = "wasi")]
mod wasi {
    use crate::alloc_vec::Vec;

    #[link(wasm_import_module = "wasi_snapshot_preview1")]
    extern "C" {
        fn random_get(buf: *mut u8, len: usize) -> i32;
        fn environ_sizes_get(count: *mut usize, size: *mut usize) -> i32;
        fn environ_get(environ: *mut *mut u8, buf: *mut u8) -> i32;
        fn fd_write(fd: i32, iovs: *const Ciovec, iovs_len: usize, nwritten: *mut usize) -> i32;
    }

    /// WASI's `ciovec`: one buffer for `fd_write`.
    #[repr(C)]
    struct Ciovec {
        buf: *const u8,
        len: usize,
    }

    /// `bytes` to stderr, as far as one `fd_write` takes them.
    pub(super) fn write_stderr(bytes: &[u8]) {
        let iov = Ciovec {
            buf: bytes.as_ptr(),
            len: bytes.len(),
        };
        let mut written = 0usize;
        // SAFETY: one `ciovec` over the live `bytes`, and a writable count.
        let _ = unsafe { fd_write(2, &iov, 1, &mut written) };
    }

    /// Fill `b` from `random_get`; `false` when it failed.
    pub(super) fn random(b: &mut [u8]) -> bool {
        // SAFETY: `b` is `b.len()` writable bytes.
        unsafe { random_get(b.as_mut_ptr(), b.len()) == 0 }
    }

    /// The value of the environment variable `name`.
    pub(super) fn env_var(name: &[u8]) -> Option<Vec<u8>> {
        let (mut count, mut size) = (0usize, 0usize);
        // SAFETY: two writable out-parameters.
        if unsafe { environ_sizes_get(&mut count, &mut size) } != 0 {
            return None;
        }
        let mut ptrs: Vec<*mut u8> = Vec::with_capacity(count);
        let mut buf: Vec<u8> = Vec::with_capacity(size);
        // SAFETY: `count` pointer slots and `size` bytes, as
        // `environ_sizes_get` asked for; both are filled by the call.
        unsafe {
            if environ_get(ptrs.as_mut_ptr(), buf.as_mut_ptr()) != 0 {
                return None;
            }
            ptrs.set_len(count);
            buf.set_len(size);
        }
        // Each entry is `NAME=value` and NUL-terminated, laid end to end.
        buf.split(|&c| c == 0)
            .filter_map(|e| e.strip_prefix(name)?.strip_prefix(b"="))
            .map(|v| v.to_vec())
            .next()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(all(unix, feature = "std"))]
    #[test]
    fn unix_entropy_comes_from_the_os_and_differs_per_draw() {
        let a = super::os_entropy().expect("/dev/urandom read");
        let b = super::os_entropy().expect("/dev/urandom read");
        assert_ne!(a, b);
    }

    #[test]
    fn hex_and_decimal_pins_both_parse() {
        assert_eq!(parse_seed("42"), Some(42));
        assert_eq!(parse_seed("0x2a"), Some(42));
        assert_eq!(parse_seed("0X2A"), Some(42));
        assert_eq!(parse_seed("  7  "), Some(7));
        assert_eq!(parse_seed(""), None);
        assert_eq!(parse_seed("banana"), None);
    }

    #[cfg(feature = "std")]
    #[test]
    fn a_malformed_seed_warning_quotes_the_value() {
        let mut out = [0u8; MALFORMED_SEED_MAX];
        let n = malformed_seed_warning(b"abc", &mut out);
        assert_eq!(
            core::str::from_utf8(&out[..n]).unwrap(),
            "warning: KARAC_HASH_SEED=`abc` is not a seed (a decimal or 0x-hexadecimal u64), \
             so this run's hash seed is random\n"
        );
        let long = [b'9'; 100];
        let n = malformed_seed_warning(&long, &mut out);
        let text = core::str::from_utf8(&out[..n]).unwrap();
        assert!(text.contains(&format!("`{}...`", "9".repeat(40))), "{text}");
        assert!(text.ends_with("random\n"), "{text}");
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
