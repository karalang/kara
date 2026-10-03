//! Threads that draw the seed at the same moment all hash with the same key.
//!
//! `seed()` used to skip initialization whenever `INIT` was non-zero, but the
//! winner of the initializing compare-exchange sets `INIT` to 1 BEFORE it
//! stores the key and to 2 after. A thread arriving inside that window read
//! a key that was still 0 (or half stored), hashed its first keys with it,
//! and the `Map` it was filling lost those entries once the real key landed.
//! An auto-parallelised loop whose body builds a `Map` is exactly that: four
//! workers make their first hash together. Measured on a sum over a `Map`
//! program: about one process in 5,000 lost an entry or two.
//!
//! The window is only a few instructions wide, so this re-runs itself as
//! many short child processes (`harness = false`, see Cargo.toml), each of
//! which releases a pack of threads at a barrier to make its first hash at
//! once.

use std::sync::{Arc, Barrier};

const CHILDREN: usize = 400;
const THREADS: usize = 16;

fn child() -> bool {
    let barrier = Arc::new(Barrier::new(THREADS));
    let handles: Vec<_> = (0..THREADS)
        .map(|_| {
            let b = Arc::clone(&barrier);
            std::thread::spawn(move || {
                b.wait();
                karac_hash::hash_bytes(b"key")
            })
        })
        .collect();
    let digests: Vec<u64> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let settled = karac_hash::hash_bytes(b"key");
    digests.iter().all(|&d| d == settled)
}

fn main() {
    if std::env::var_os("SEED_RACE_CHILD").is_some() {
        std::process::exit(if child() { 0 } else { 1 });
    }
    let exe = std::env::current_exe().unwrap();
    let mut disagreed = 0;
    for _ in 0..CHILDREN {
        let status = std::process::Command::new(&exe)
            .env("SEED_RACE_CHILD", "1")
            .env("KARAC_HASH_SEED", "7")
            .status()
            .expect("running a seed_race child");
        if !status.success() {
            disagreed += 1;
        }
    }
    assert_eq!(
        disagreed, 0,
        "{disagreed} of {CHILDREN} processes had threads hash the same key differently"
    );
    println!("seed_race: ok ({CHILDREN} processes x {THREADS} threads agreed on the key)");
}
