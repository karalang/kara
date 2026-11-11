//! Kata 2111's search arm gathered each chain with
//! `arr.iter().skip(r).step_by(k)`. `skip` jumped since B-2026-11-08-52,
//! but `step_by` advanced the `skip` itself, and `IterSkip` had no
//! `advance_by` of its own, so each step walked the `k - 1` items one at a
//! time: with `k = n = 10^5` the arm ran for minutes. `IterSkip` now
//! passes an advance through to what it wraps, adding the items it has
//! still to skip.

fn run(src: &str) -> String {
    karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output
}

/// A step of 2^62 after a skip of 2^62 is two items, not a hang.
#[test]
fn kata_2111_skip_then_step_by_over_a_range_jumps() {
    let out = run(r#"
fn main() {
    let v: Vec[i64] = (0..=9223372036854775807).skip(4611686018427387904).step_by(4611686018427387904).collect();
    println(f"{v}");
}
"#);
    assert_eq!(out, "[4611686018427387904]\n");
}

/// Every chain of a `Vec`, for every start and step, is the items the
/// walk by index gives, and a chain past the end is empty.
#[test]
fn kata_2111_skip_then_step_by_over_a_vec_is_each_chain() {
    let out = run(r#"
fn main() {
    let arr: Vec[i64] = (0..11).map(|i| i * 10).collect();
    let mut bad = 0;
    let mut items = 0;
    for k in 1..13 {
        for r in 0..13 {
            let chain: Vec[i64] = arr.iter().skip(r).step_by(k).collect();
            let mut want: Vec[i64] = Vec.new();
            let mut i = r;
            while i < arr.len() {
                want.push(arr[i]);
                i += k;
            }
            if chain != want {
                bad += 1;
            }
            items += chain.len();
        }
    }
    let n = (0..10).skip(3).step_by(3).count();
    println(f"{bad} {items} {n}");
}
"#);
    assert_eq!(out, "0 259 3\n");
}
