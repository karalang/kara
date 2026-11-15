//! `(1..=end).chain(start..=n).collect()` into a `Vec` of the ranges'
//! integer type is one fill per range into a vector allocated once at
//! the sum of their lengths (B-2026-11-14-15). The library's `collect`
//! grew it by reallocation. These pin the values (empty and reversed
//! ranges, a chain of chains, both ends of `u8` and `i64`), the order the
//! bounds are evaluated in, and the single allocation, on both lanes.

use std::process::{Command, Output, Stdio};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn on_both_lanes(tag: &str, src: &str, want: &str) {
    let path = std::env::temp_dir().join(format!(
        "karac-range-chain-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    for lane in [&["run"][..], &["run", "--interp"][..]] {
        let out = karac(lane, &path);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            want,
            "{lane:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_chain_of_ranges_collects_every_value_in_order() {
    on_both_lanes(
        "values",
        r#"fn p(n: i64) -> i64 {
    println(f"e{n}");
    n
}

fn main() {
    let a: Vec[i64] = (1..=3).chain(7..=9).collect();
    println(f"{a} {a.len()}");
    let b: Vec[i64] = (p(5)..p(3)).chain(p(1)..=p(2)).collect();
    println(f"{b}");
    let c: Vec[i64] = (0..2).chain(4..4).chain(6..=7).chain(9..=8).collect();
    println(f"{c}");
    let d: Vec[u8] = (250u8..=255u8).chain(0u8..=1u8).collect();
    println(f"{d}");
    let e: Vec[i64] = (i64.MAX - 1..=i64.MAX).chain(i64.MIN..i64.MIN + 2).collect();
    println(f"{e}");
    let n = 6;
    let mut tot = 0;
    for start in 1..=n {
        for end in 1..=n {
            let v: Vec[i64] = if start <= end { (start..=end).collect() } else { (1..=end).chain(start..=n).collect() };
            tot += v.len() * 100 + v.iter().sum();
        }
    }
    println(tot);
}
"#,
        "[1, 2, 3, 7, 8, 9] 6\ne5\ne3\ne1\ne2\n[1, 2]\n[0, 1, 6, 7]\n\
         [250, 251, 252, 253, 254, 255, 0, 1]\n\
         [9223372036854775806, 9223372036854775807, -9223372036854775808, -9223372036854775807]\n\
         13041\n",
    );
}

/// The vector is allocated once, at the chain's length.
#[test]
fn a_chain_of_ranges_collects_into_one_allocation() {
    on_both_lanes(
        "capacity",
        r#"fn main() {
    let a: Vec[i64] = (1..=3).chain(7..=9).chain(20..25).collect();
    println(f"{a.len()} {a.capacity()}");
}
"#,
        "11 11\n",
    );
}
