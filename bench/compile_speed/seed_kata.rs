// Seed kata (algorithmic corpus member) — plain copy from
// kara-katas/leetcode/1-100/1-two-sum/bench/hash_map.rs (2026-07-22).
// No sync infrastructure by design (roadmap Track 5): kara-katas evolves
// independently; re-copy if the kata materially changes.

//! Benchmark workload — hash-map O(n) Two Sum.
//!
//! Algorithmic mirror of bench/hash_map.py. Uses std::collections::HashMap.
//! See ../README.md § Benchmarks for what the numbers mean.

use std::collections::HashMap;

fn two_sum(nums: &[i64], target: i64) -> Option<(usize, usize)> {
    let mut seen: HashMap<i64, usize> = HashMap::new();
    for (i, &num) in nums.iter().enumerate() {
        let complement = target - num;
        if let Some(&j) = seen.get(&complement) {
            return Some((j, i));
        }
        seen.insert(num, i);
    }
    None
}

fn main() {
    const N: usize = 5000;
    let data: [i64; N] = std::array::from_fn(|i| ((i as i64) * 7) % 1000);
    let target: i64 = -1;

    let mut sum: i64 = 0;
    for _ in 0..10 {
        sum += match two_sum(&data, target) {
            Some((i, j)) => i as i64 + j as i64,
            None => -2,
        };
    }
    println!("{}", sum);
}
