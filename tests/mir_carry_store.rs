//! A loop that loads back the word its previous trip stored keeps the
//! value in a register (B-2026-10-31-38): `dp[c] = cost + min(dp[c],
//! dp[c - 1])` behind an overflow check. Each program here is built with
//! the forwarding on (`KARAC_MIR_CARRY_STORE=report`, which counts the
//! loads it replaced) and off (`=0`), and both binaries must print what the
//! MIR interpreter prints.
#![cfg(feature = "llvm")]

mod common;

use std::path::PathBuf;
use std::process::Command;

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("karac-carry-store-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Builds `src` with the forwarding reported and off; both must print
/// `expected`, as the interpreter does, and `forwarded` loads are replaced.
fn check(tag: &str, src: &str, expected: &str, forwarded: usize) {
    let dir = scratch(tag);
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let interp = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["__mir-run", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&interp.stdout),
        expected,
        "{interp:?}"
    );
    for on in ["report", "0"] {
        let exe = dir.join(format!("main{on}"));
        let built = Command::new(env!("CARGO_BIN_EXE_karac"))
            .env("KARAC_MIR_CARRY_STORE", on)
            .env("KARAC_AUTO_PAR", "0")
            .args(["build", file.to_str().unwrap(), "-o", exe.to_str().unwrap()])
            .output()
            .unwrap();
        let ok = if built.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&built.stderr).to_string())
        };
        if common::link_or_skip(ok).is_none() {
            return;
        }
        if on == "report" {
            let said: usize = String::from_utf8_lossy(&built.stderr)
                .lines()
                .filter_map(|l| l.strip_prefix("carry-store: "))
                .filter_map(|l| l.split(' ').next()?.parse::<usize>().ok())
                .sum();
            assert_eq!(
                said,
                forwarded,
                "{}",
                String::from_utf8_lossy(&built.stderr)
            );
        }
        let ran = Command::new(&exe).output().unwrap();
        assert_eq!(ran.status.code(), interp.status.code(), "{on}: {ran:?}");
        assert_eq!(ran.stdout, interp.stdout, "KARAC_MIR_CARRY_STORE={on}");
        assert_eq!(ran.stderr, interp.stderr, "KARAC_MIR_CARRY_STORE={on}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Kata 64's DP: forward over each row, `dp[c - 1]` the word the trip
/// before stored; and the fill loop's `dp[j - 1]`.
#[test]
fn mir_carry_store_min_path_sum_rows() {
    let src = r#"fn min_path_sum(m: i64, n: i64, k: i64) -> i64 {
    let mut dp: Vec[i64] = Vec.new();
    let mut j = 0;
    while j < n {
        let cost = ((j * 3 + k) % 13) + 1;
        if j == 0 {
            dp.push(cost);
        } else {
            let prev = dp[j - 1];
            dp.push(prev + cost);
        }
        j = j + 1;
    }
    let mut i = 1;
    while i < m {
        dp[0] = dp[0] + (((i * 7 + k) % 13) + 1);
        let mut c = 1;
        while c < n {
            let cost = ((i * 7 + c * 3 + k) % 13) + 1;
            dp[c] = cost + min(dp[c], dp[c - 1]);
            c = c + 1;
        }
        i = i + 1;
    }
    dp[n - 1]
}
fn main() {
    let mut acc: i64 = 0;
    let mut k: i64 = 0;
    while k < 2000 {
        acc = (acc * 131 + min_path_sum(2 + k % 9, 2 + (k / 9) % 9, k)) % 1000000007;
        k = k + 1;
    }
    println(f"{acc}");
}
"#;
    check("rows", src, "256350813\n", 1);
}

/// A loop walking down, `dp[c + 1]` stored by the trip before, and a
/// prefix sum are forwarded; a loop with a second store, to another vector
/// the pass cannot tell apart from the first, is left alone.
#[test]
fn mir_carry_store_down_prefix_and_two_stores() {
    let src = r#"fn down(n: i64, k: i64) -> i64 {
    let mut dp: Vec[i64] = Vec.filled(n, k % 7 + 1);
    let mut c = n - 2;
    while c >= 0 {
        dp[c] = dp[c] + 2 * dp[c + 1] % 1000;
        c = c - 1;
    }
    dp[0]
}
fn prefix(n: i64, k: i64) -> i64 {
    let mut xs: Vec[i64] = Vec.new();
    for i in 0..n {
        xs.push(i * i - 7 * i + k);
    }
    let mut pre: Vec[i64] = Vec.filled(n + 1, 0);
    for i in 0..n {
        pre[i + 1] = pre[i] + xs[i];
    }
    pre[n] - pre[n / 2]
}
fn two(n: i64, k: i64) -> i64 {
    let mut a: Vec[i64] = Vec.filled(n, k % 5 + 3);
    let mut b: Vec[i64] = Vec.filled(n, 0);
    for i in 1..n {
        a[i] = a[i - 1] + a[i];
        b[i] = a[i] - b[i - 1];
    }
    a[n - 1] * 3 + b[n - 1]
}
fn main() {
    let mut acc: i64 = 0;
    let mut k: i64 = 0;
    while k < 300 {
        let n = 2 + k % 37;
        acc = (acc * 131 + down(n, k) + prefix(n, k) + two(n, k)) % 1000000007;
        k = k + 1;
    }
    println(f"{acc}");
}
"#;
    check("shapes", src, "464564642\n", 2);
}
