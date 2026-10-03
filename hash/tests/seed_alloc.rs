//! Drawing the random seed must not leave anything allocated (B-2026-10-03-11).
//!
//! The entropy source used to mix in `std::thread::current()`, which allocates
//! the main thread's handle on first use and never frees it. Valgrind reports
//! that block as "possibly lost" in every compiled program that hashes a `Map`
//! or `Set` key, so a leak check on such a program fails for a reason that is
//! not the program's.
//!
//! Two details decide how this has to be measured:
//!
//! * `harness = false` (see Cargo.toml), because the block is only allocated
//!   for the MAIN thread. libtest runs each test on a thread whose handle
//!   already exists, where the unfixed code allocates nothing.
//! * glibc's own count of bytes in use (`mallinfo2().uordblks`), not a counting
//!   `#[global_allocator]`. std allocates thread handles through `System`
//!   directly, so a counting global allocator sees 0 bytes on the unfixed code
//!   too. That makes the check glibc-only; elsewhere it reports a skip. The
//!   measurement runs in a child with glibc's per-thread cache off (see
//!   `main`).

#[cfg(all(target_os = "linux", target_env = "gnu"))]
mod glibc {
    // Only `uordblks` is read; the rest give the struct glibc's layout.
    #[allow(dead_code)]
    #[repr(C)]
    struct MallInfo2 {
        arena: usize,
        ordblks: usize,
        smblks: usize,
        hblks: usize,
        hblkhd: usize,
        usmblks: usize,
        fsmblks: usize,
        uordblks: usize,
        fordblks: usize,
        keepcost: usize,
    }

    extern "C" {
        fn mallinfo2() -> MallInfo2;
    }

    /// Bytes the C allocator currently has handed out.
    pub fn in_use() -> usize {
        unsafe { mallinfo2() }.uordblks
    }
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn main() {
    // glibc counts a chunk parked in its per-thread cache as still in use, so
    // the `Box` and `String` the entropy source frees would read as leaked.
    // Run the measurement in a child with that cache turned off.
    const TUNABLE: &str = "glibc.malloc.tcache_count=0";
    if std::env::var("GLIBC_TUNABLES").ok().as_deref() != Some(TUNABLE) {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .env("GLIBC_TUNABLES", TUNABLE)
            .env_remove("KARAC_HASH_SEED")
            .status()
            .expect("re-running seed_alloc with the tcache off");
        std::process::exit(status.code().unwrap_or(1));
    }
    // The random path is the one under test; a pinned seed never reaches it.
    assert!(std::env::var_os("KARAC_HASH_SEED").is_none());
    let before = glibc::in_use();
    let digest = karac_hash::hash_bytes(b"key");
    let after = glibc::in_use();
    assert_eq!(
        after as isize - before as isize,
        0,
        "drawing the hash seed left {} bytes allocated",
        after as isize - before as isize
    );
    // The seed is live from here on, so the same key hashes the same way.
    assert_eq!(karac_hash::hash_bytes(b"key"), digest);
    println!("seed_alloc: ok (0 bytes left allocated by the first hash)");
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn main() {
    println!("seed_alloc: skipped (needs glibc's mallinfo2)");
}
