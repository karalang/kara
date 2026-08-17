//! Front-end phase benchmark for the name-interning spike
//! (`docs/spikes/name-interning.md`).
//!
//! Times each front-end phase separately over N iterations and reports
//! per-phase wall time (min / median) plus heap-allocation counts and bytes
//! from a counting global allocator — the direct measure of the
//! clone-a-`String`-per-lookup traffic interning would remove. `karac build`
//! numbers (bench/compile_speed) are diluted by LLVM; this harness is the
//! front-end-only instrument.
//!
//!   cargo run --release --example bench_frontend -- file.kara [iters]
//!
//! Output is one line per phase: `phase  min_ms  median_ms  allocs  MB`
//! (alloc columns from the LAST iteration; steady-state, not warm-up).

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::Instant;

struct CountingAlloc;

static ALLOC_CALLS: AtomicU64 = AtomicU64::new(0);
static ALLOC_BYTES: AtomicU64 = AtomicU64::new(0);

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOC_CALLS.fetch_add(1, Relaxed);
        ALLOC_BYTES.fetch_add(layout.size() as u64, Relaxed);
        System.alloc(layout)
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout)
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOC_CALLS.fetch_add(1, Relaxed);
        ALLOC_BYTES.fetch_add(new_size as u64, Relaxed);
        System.realloc(ptr, layout, new_size)
    }
}

#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc;

const PHASES: [&str; 6] = [
    "parse",
    "prepare",
    "resolve",
    "typecheck",
    "effectcheck",
    "ownership",
];

fn median(sorted: &[f64]) -> f64 {
    let n = sorted.len();
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().unwrap_or_else(|| {
        eprintln!("usage: bench_frontend <file.kara> [iters]");
        std::process::exit(2);
    });
    let iters: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(10);
    let source = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        eprintln!("error: cannot read {path}: {e}");
        std::process::exit(2);
    });

    // times[phase][iter] in ms; allocs/bytes from the last iteration only.
    let mut times: Vec<Vec<f64>> = vec![Vec::with_capacity(iters); PHASES.len()];
    let mut allocs = [0u64; PHASES.len()];
    let mut bytes = [0u64; PHASES.len()];

    for _ in 0..iters {
        let mut phase = 0;
        let mut clock = |times: &mut Vec<Vec<f64>>, t0: Instant, a0: u64, b0: u64| {
            times[phase].push(t0.elapsed().as_secs_f64() * 1e3);
            allocs[phase] = ALLOC_CALLS.load(Relaxed) - a0;
            bytes[phase] = ALLOC_BYTES.load(Relaxed) - b0;
            phase += 1;
            (
                Instant::now(),
                ALLOC_CALLS.load(Relaxed),
                ALLOC_BYTES.load(Relaxed),
            )
        };

        let (t0, a0, b0) = (
            Instant::now(),
            ALLOC_CALLS.load(Relaxed),
            ALLOC_BYTES.load(Relaxed),
        );
        let parsed = karac::parse(&source);
        assert!(
            parsed.errors.is_empty(),
            "{path}: parse errors — bench input must be clean"
        );
        let mut program = parsed.program;
        let (t0, a0, b0) = clock(&mut times, t0, a0, b0);

        let _ = karac::prepare_for_resolve(&mut program);
        let (t0, a0, b0) = clock(&mut times, t0, a0, b0);

        let resolved = karac::resolve(&program);
        assert!(
            resolved.errors.is_empty(),
            "{path}: resolve errors — bench input must be clean"
        );
        let (t0, a0, b0) = clock(&mut times, t0, a0, b0);

        let tc = karac::typecheck(&program, &resolved);
        assert!(
            tc.errors.is_empty(),
            "{path}: type errors — bench input must be clean"
        );
        let (t0, a0, b0) = clock(&mut times, t0, a0, b0);

        let _effects = karac::effectcheck(&program);
        let (t0, a0, b0) = clock(&mut times, t0, a0, b0);

        let _own = karac::ownershipcheck(&program, &tc);
        let _ = clock(&mut times, t0, a0, b0);
    }

    let lines: usize = source.lines().count();
    println!("# {path} — {lines} lines, {iters} iters");
    println!(
        "{:<12} {:>9} {:>9} {:>12} {:>9}",
        "phase", "min_ms", "med_ms", "allocs", "MB"
    );
    let mut tot_med = 0.0;
    let (mut tot_allocs, mut tot_bytes) = (0u64, 0u64);
    for (i, name) in PHASES.iter().enumerate() {
        let mut sorted = times[i].clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let med = median(&sorted);
        tot_med += med;
        tot_allocs += allocs[i];
        tot_bytes += bytes[i];
        println!(
            "{:<12} {:>9.2} {:>9.2} {:>12} {:>9.2}",
            name,
            sorted[0],
            med,
            allocs[i],
            bytes[i] as f64 / 1e6
        );
    }
    println!(
        "{:<12} {:>9} {:>9.2} {:>12} {:>9.2}",
        "TOTAL",
        "",
        tot_med,
        tot_allocs,
        tot_bytes as f64 / 1e6
    );
}
