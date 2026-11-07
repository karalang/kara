// Rust mirror of lis_only.kara (B-2026-11-06-8).
const ROUNDS: i64 = 20;
const COUNT: i64 = 200000;

fn lis(heights: &[i64]) -> i64 {
    let mut tails: Vec<i64> = Vec::new();
    for &h in heights.iter() {
        let mut lo = 0;
        let mut hi = tails.len();
        while lo < hi {
            let mid = (lo + hi) / 2;
            if tails[mid] < h {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        if lo == tails.len() {
            tails.push(h);
        } else {
            tails[lo] = h;
        }
    }
    tails.len() as i64
}

fn main() {
    let mut sink: i64 = 0;
    for round in 0..ROUNDS {
        let mut seed = 354 + round;
        let mut heights: Vec<i64> = Vec::new();
        for _ in 0..COUNT {
            seed = (seed * 1103515245 + 12345) % 2147483648;
            seed = (seed * 1103515245 + 12345) % 2147483648;
            heights.push(seed % 100000 + 1);
        }
        sink = (sink * 31 + lis(&heights) + round) % 1000000007;
    }
    println!("{}", sink);
}
