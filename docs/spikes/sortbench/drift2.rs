// Hardened re-measurement of the driftsort baseline, to rule out the harness
// rather than the row. Three guards the first version lacked:
//   1. black_box on the input and the result, so nothing can be elided.
//   2. prints the DISTINCT KEY COUNT actually generated, so "8 distinct keys"
//      is verified rather than assumed.
//   3. cross-checks best-of-N against a bulk timing (total / N), which would
//      diverge if a single fast outlier were being picked up.

use std::collections::HashSet;
use std::hint::black_box;
use std::time::Instant;

fn build(pattern: &str, n: i64) -> Vec<(i64, i64)> {
    let mut v: Vec<(i64, i64)> = Vec::new();
    let mut seed: i64 = 12345;
    let mut i: i64 = 0;
    while i < n {
        seed = (seed * 1103515245 + 12345) % 2147483648;
        let r = seed;
        let k: i64 = match pattern {
            "random" => r,
            "few_unique" => r % 8,
            "sawtooth" => i % 1000,
            "sorted" => i,
            "reverse" => n - i,
            "nearly_sorted" => if r % 100 == 0 { r } else { i },
            _ => unreachable!(),
        };
        v.push((k, i));
        i += 1;
    }
    v
}

fn main() {
    let n: i64 = 150_000;
    let reps = 11;
    for p in ["random", "few_unique", "sawtooth", "sorted", "reverse", "nearly_sorted"] {
        let base = build(p, n);
        let card = base.iter().map(|e| e.0).collect::<HashSet<_>>().len();

        // best-of-N
        let mut best = f64::MAX;
        for _ in 0..reps {
            let mut work = black_box(base.clone());
            let t = Instant::now();
            work.sort_by(|x, y| x.0.cmp(&y.0));
            let e = t.elapsed().as_secs_f64();
            black_box(&work);
            assert!(work.windows(2).all(|w| w[0].0 <= w[1].0));
            if e < best { best = e; }
        }

        // bulk cross-check: time N sorts as one block, divide
        let mut pre: Vec<Vec<(i64, i64)>> = (0..reps).map(|_| base.clone()).collect();
        let t = Instant::now();
        for w in pre.iter_mut() {
            w.sort_by(|x, y| x.0.cmp(&y.0));
        }
        let bulk = t.elapsed().as_secs_f64() / reps as f64;
        black_box(&pre);

        println!(
            "{:<15} card={:<7} best={:.3}ms  bulk_avg={:.3}ms",
            p, card, best * 1000.0, bulk * 1000.0
        );
    }
}
