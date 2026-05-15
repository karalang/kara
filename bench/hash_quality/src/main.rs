//! Hash function quality bench — research artifact for the
//! `wip-hash-quality.md` investigation.
//!
//! Compares 4 non-cryptographic hash families plus `std::HashMap`'s
//! default hasher (SipHash 1-3) on a fixed linear-probe table. The
//! table layout, load factor, and probe strategy are identical
//! across runs — only the hash function varies. This isolates
//! "hash quality drives probe-chain length and cache behavior" from
//! "probing strategy" so the bench measures hash, not table impl.
//!
//! Usage: `./hash_bench <impl> <K> <size> <workload>`
//!   impl     ∈ { fnv1a, fibonacci, wyhash, fxhash, std }
//!   K        ∈ { i64, char, string }
//!   size     ∈ { small, medium, large }       (16 / 1024 / 100000)
//!   workload ∈ { hot, uniform }
//!
//! Prints elapsed seconds for the lookup phase as a single number;
//! hyperfine can swallow this directly via `--show-output`.

use std::collections::HashMap;
use std::hash::Hasher;
use std::time::Instant;

// ── Hash family trait ───────────────────────────────────────────

trait HashFn {
    fn hash_bytes(bytes: &[u8]) -> u64;
}

// ── FNV-1a (the current karac default) ───────────────────────────

struct Fnv1a;
impl HashFn for Fnv1a {
    #[inline]
    fn hash_bytes(bytes: &[u8]) -> u64 {
        const BASIS: u64 = 0xcbf29ce484222325;
        const PRIME: u64 = 0x100000001b3;
        let mut h = BASIS;
        for &b in bytes {
            h ^= b as u64;
            h = h.wrapping_mul(PRIME);
        }
        h
    }
}

// ── Fibonacci hashing (multiplicative, integer-fast-path only) ──

struct Fibonacci;
impl HashFn for Fibonacci {
    #[inline]
    fn hash_bytes(bytes: &[u8]) -> u64 {
        // Read up to 8 bytes as a u64 (little-endian, zero-pad
        // shorter inputs). For longer inputs fall back to a weak
        // chunked accumulation — Fibonacci hashing is genuinely
        // bad for variable-length strings; benching it on String
        // keys is part of the point (it'll lose visibly).
        const KNUTH: u64 = 0x9e3779b97f4a7c15;
        let mut acc: u64 = 0;
        for (i, &b) in bytes.iter().enumerate() {
            let pos = i % 8;
            acc ^= (b as u64) << (pos * 8);
            if pos == 7 {
                acc = acc.wrapping_mul(KNUTH);
            }
        }
        acc.wrapping_mul(KNUTH)
    }
}

// ── Wyhash-lite (multiply-shift-multiply, 8-byte chunks) ─────────

struct Wyhash;
impl HashFn for Wyhash {
    #[inline]
    fn hash_bytes(bytes: &[u8]) -> u64 {
        // Simplified wyhash: 8-byte chunks via wymix.
        const P0: u64 = 0xa0761d6478bd642f;
        const P1: u64 = 0xe7037ed1a0b428db;
        let mut h: u64 = P0 ^ (bytes.len() as u64);
        let mut chunks = bytes.chunks_exact(8);
        for chunk in &mut chunks {
            let w = u64::from_le_bytes(chunk.try_into().unwrap());
            h = wymix(h ^ w, P1);
        }
        let rem = chunks.remainder();
        if !rem.is_empty() {
            let mut last: [u8; 8] = [0; 8];
            last[..rem.len()].copy_from_slice(rem);
            let w = u64::from_le_bytes(last);
            h = wymix(h ^ w, P1);
        }
        h
    }
}

#[inline]
fn wymix(a: u64, b: u64) -> u64 {
    let r = (a as u128).wrapping_mul(b as u128);
    (r as u64) ^ ((r >> 64) as u64)
}

// ── Fxhash (rustc-hash style — rotate-xor-multiply) ─────────────

struct Fxhash;
impl HashFn for Fxhash {
    #[inline]
    fn hash_bytes(bytes: &[u8]) -> u64 {
        const ROTATE: u32 = 5;
        const SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;
        let mut h: u64 = 0;
        let mut chunks = bytes.chunks_exact(8);
        for chunk in &mut chunks {
            let w = u64::from_le_bytes(chunk.try_into().unwrap());
            h = h.rotate_left(ROTATE) ^ w;
            h = h.wrapping_mul(SEED);
        }
        for &b in chunks.remainder() {
            h = h.rotate_left(ROTATE) ^ (b as u64);
            h = h.wrapping_mul(SEED);
        }
        h
    }
}

// ── Linear-probe table (hash isolated from probe strategy) ──────

struct ProbeTable<H: HashFn> {
    status: Vec<u8>,
    keys: Vec<Vec<u8>>,
    vals: Vec<i64>,
    capacity: usize,
    len: usize,
    _phantom: std::marker::PhantomData<H>,
}

const EMPTY: u8 = 0;
const OCCUPIED: u8 = 1;

impl<H: HashFn> ProbeTable<H> {
    fn new(target_capacity: usize) -> Self {
        // Round up to next power of 2 (so `hash & mask` works).
        let mut cap = 16;
        while cap < target_capacity * 2 {
            cap *= 2;
        }
        Self {
            status: vec![EMPTY; cap],
            keys: vec![Vec::new(); cap],
            vals: vec![0; cap],
            capacity: cap,
            len: 0,
            _phantom: std::marker::PhantomData,
        }
    }

    #[inline]
    fn insert(&mut self, key: &[u8], val: i64) {
        let mask = self.capacity - 1;
        let h = H::hash_bytes(key);
        let mut slot = (h as usize) & mask;
        loop {
            match self.status[slot] {
                EMPTY => {
                    self.keys[slot] = key.to_vec();
                    self.vals[slot] = val;
                    self.status[slot] = OCCUPIED;
                    self.len += 1;
                    return;
                }
                OCCUPIED if self.keys[slot] == key => {
                    self.vals[slot] = val;
                    return;
                }
                _ => slot = (slot + 1) & mask,
            }
        }
    }

    #[inline]
    fn get(&self, key: &[u8]) -> Option<i64> {
        let mask = self.capacity - 1;
        let h = H::hash_bytes(key);
        let mut slot = (h as usize) & mask;
        let mut probes = 0;
        loop {
            match self.status[slot] {
                EMPTY => return None,
                OCCUPIED if self.keys[slot] == key => return Some(self.vals[slot]),
                _ => {
                    slot = (slot + 1) & mask;
                    probes += 1;
                    if probes >= self.capacity {
                        return None;
                    }
                }
            }
        }
    }
}

// ── Workload generators ─────────────────────────────────────────

fn keys_i64(n: usize, workload: &str) -> Vec<Vec<u8>> {
    let mut keys = Vec::with_capacity(n);
    for i in 0..n {
        let k: i64 = match workload {
            "hot" => 42,
            "uniform" => (i as i64).wrapping_mul(2654435761).wrapping_add(7),
            _ => i as i64,
        };
        keys.push(k.to_le_bytes().to_vec());
    }
    keys
}

fn keys_char(n: usize, workload: &str) -> Vec<Vec<u8>> {
    let mut keys = Vec::with_capacity(n);
    for i in 0..n {
        let k: i32 = match workload {
            "hot" => 'a' as i32,
            "uniform" => (((i as u32).wrapping_mul(2654435761) % 26) + 'a' as u32) as i32,
            _ => i as i32,
        };
        keys.push(k.to_le_bytes().to_vec());
    }
    keys
}

fn keys_string(n: usize, workload: &str) -> Vec<Vec<u8>> {
    let mut keys = Vec::with_capacity(n);
    for i in 0..n {
        let s = match workload {
            "hot" => "hello, world".to_string(),
            "uniform" => format!("key_{:08x}_{}", i, "padding".repeat(2)),
            _ => format!("key_{}", i),
        };
        keys.push(s.into_bytes());
    }
    keys
}

// ── Bench harness ───────────────────────────────────────────────

fn run<H: HashFn>(keys: Vec<Vec<u8>>, lookup_iters: usize) -> f64 {
    let n = keys.len();
    let mut table = ProbeTable::<H>::new(n);
    for (i, k) in keys.iter().enumerate() {
        table.insert(k, i as i64);
    }
    // Hot phase: M lookups across the inserted keys (cycling).
    let start = Instant::now();
    let mut hit_sum: i64 = 0;
    for i in 0..lookup_iters {
        let k = &keys[i % n];
        if let Some(v) = table.get(k) {
            hit_sum = hit_sum.wrapping_add(v);
        }
    }
    let elapsed = start.elapsed();
    // Use hit_sum so the lookup isn't DCE'd. Bench tail-line prints
    // the elapsed first, hit_sum second (for spot-check / DCE
    // verification).
    eprintln!("hit_sum={}", hit_sum);
    elapsed.as_secs_f64()
}

fn run_std(keys: Vec<Vec<u8>>, lookup_iters: usize) -> f64 {
    let n = keys.len();
    let mut table: HashMap<Vec<u8>, i64> = HashMap::new();
    for (i, k) in keys.iter().enumerate() {
        table.insert(k.clone(), i as i64);
    }
    let start = Instant::now();
    let mut hit_sum: i64 = 0;
    for i in 0..lookup_iters {
        let k = &keys[i % n];
        if let Some(&v) = table.get(k) {
            hit_sum = hit_sum.wrapping_add(v);
        }
    }
    let elapsed = start.elapsed();
    eprintln!("hit_sum={}", hit_sum);
    elapsed.as_secs_f64()
}

// Silence unused warning — `Hasher` import is used by the future
// `default_hasher_direct` configuration (kept around for parity
// with the `std` baseline's documentation; the `run_std` config
// uses `HashMap` internally which goes through the default
// hasher).
#[allow(dead_code)]
fn _ignore() -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    h.write(b"unused");
    h.finish()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 5 {
        eprintln!("usage: hash_bench <impl> <K> <size> <workload>");
        eprintln!("  impl     ∈ {{ fnv1a, fibonacci, wyhash, fxhash, std }}");
        eprintln!("  K        ∈ {{ i64, char, string }}");
        eprintln!("  size     ∈ {{ small, medium, large }}");
        eprintln!("  workload ∈ {{ hot, uniform }}");
        std::process::exit(2);
    }
    let impl_ = &args[1];
    let k_kind = &args[2];
    let size = match args[3].as_str() {
        "small" => 16,
        "medium" => 1024,
        "large" => 100_000,
        _ => {
            eprintln!("bad size: {}", args[3]);
            std::process::exit(2);
        }
    };
    let workload = &args[4];
    // Lookup iter count chosen so each config runs ~50-300ms;
    // scaled inversely with table size since per-lookup cost grows
    // sub-linearly. Tuned by trial.
    let lookup_iters = match args[3].as_str() {
        "small" => 50_000_000,
        "medium" => 10_000_000,
        "large" => 2_000_000,
        _ => unreachable!(),
    };
    let keys = match k_kind.as_str() {
        "i64" => keys_i64(size, workload),
        "char" => keys_char(size, workload),
        "string" => keys_string(size, workload),
        _ => {
            eprintln!("bad K: {}", k_kind);
            std::process::exit(2);
        }
    };
    let elapsed_s = match impl_.as_str() {
        "fnv1a" => run::<Fnv1a>(keys, lookup_iters),
        "fibonacci" => run::<Fibonacci>(keys, lookup_iters),
        "wyhash" => run::<Wyhash>(keys, lookup_iters),
        "fxhash" => run::<Fxhash>(keys, lookup_iters),
        "std" => run_std(keys, lookup_iters),
        _ => {
            eprintln!("bad impl: {}", impl_);
            std::process::exit(2);
        }
    };
    // tab-separated for downstream awk/sort.
    println!(
        "{}\t{}\t{}\t{}\t{:.4}",
        impl_, k_kind, args[3], workload, elapsed_s * 1000.0
    );
}
