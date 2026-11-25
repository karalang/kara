// A Rust host that links the Kāra kernel through its C ABI — the
// pyo3/cxx/uniffi pattern, inverted: Rust consumes Kāra across the stable
// C boundary (there is no stable *Rust* ABI, so C is the durable bridge).
//
// IMPORTANT — link the *cdylib*, not the staticlib. The Kāra runtime is a
// Rust crate that bundles `std`; a `.a` therefore carries std symbols
// (`rust_eh_personality`, the allocator shims, …) that collide with the
// Rust host's own `std` at static-link time ("duplicate symbol"). A `.so`
// encapsulates those internal symbols, so the dynamic linker resolves the
// exported entry points cleanly. C hosts don't hit this (no std to clash
// with) and can use either artifact.
//
// Build + run:
//   karac build kernel.kara --crate-type cdylib -o libkernel.so
//   rustc host.rs -L . -C link-arg=-Wl,-rpath,. -o host_rs
//   ./host_rs        # => add=42 fib=6765 mean=7.50

#[repr(C)]
struct Stats {
    sum: f64,
    count: i64,
}

#[link(name = "kernel", kind = "dylib")]
extern "C" {
    fn karac_runtime_init();
    fn karac_runtime_shutdown();
    fn add(a: i32, b: i32) -> i32;
    fn fib(n: i64) -> i64;
    fn stats_mean(s: Stats) -> f64;
}

fn main() {
    unsafe {
        karac_runtime_init();
        let s = Stats { sum: 30.0, count: 4 };
        println!(
            "add={} fib={} mean={:.2}",
            add(20, 22),
            fib(20),
            stats_mean(s)
        );
        karac_runtime_shutdown();
    }
}
