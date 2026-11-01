//! A sequence's length loads with `!range [0, i64::MAX)` on mir-llvm
//! (part of B-2026-10-31-38). Without it LLVM cannot tell `nums.len()` is
//! not negative, so in kata 1031's `for i in 0..n { prefix[i + 1] =
//! prefix[i] + nums[i] }` over `Vec.filled(n + 1, 0)` both `prefix`
//! checks survived (the fill's empty path left the length a phi of
//! `n + 1` and 0) and `nums`' buffer pointer was reloaded every trip. The
//! pins: the program prints the same under both backends, and the IR's
//! length loads carry the range, unless `KARAC_MIR_LEN_RANGE=0`.
#![cfg(feature = "llvm")]

mod common;

use std::path::PathBuf;
use std::process::Command;

const SRC: &str = r#"fn last_prefix(nums: ref Vec[i64]) -> i64 {
    let n = nums.len();
    let mut prefix: Vec[i64] = Vec.filled(n + 1, 0);
    for i in 0..n {
        prefix[i + 1] = prefix[i] + nums[i];
    }
    prefix[n]
}
fn main() {
    let cases: Vec[(Vec[i64], i64)] = [([1, 2, 3, 4].to_vec(), 1), (Vec.new(), 2)].to_vec();
    for (nums, k) in cases.iter() {
        println(f"{last_prefix(nums) * k}");
    }
}
"#;

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("karac-len-range-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn karac() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
    c.env("KARAC_AUTO_PAR", "0");
    c
}

#[test]
fn mir_len_range_marks_length_loads() {
    for (flag, ranged) in [(None, true), (Some("0"), false)] {
        let d = dir(if ranged { "on" } else { "off" });
        let file = d.join("main.kara");
        std::fs::write(&file, SRC).unwrap();
        let file = file.to_str().unwrap();
        let interp = karac().args(["__mir-run", file]).output().unwrap();
        assert_eq!(
            String::from_utf8_lossy(&interp.stdout),
            "10\n0\n",
            "{}",
            String::from_utf8_lossy(&interp.stderr)
        );
        let exe = d.join("main");
        let mut build = karac();
        build.env("KARAC_MIR_LL", d.join("main.ll"));
        if let Some(v) = flag {
            build.env("KARAC_MIR_LEN_RANGE", v);
        }
        let built = build
            .args(["build", file, "-o", exe.to_str().unwrap()])
            .output()
            .unwrap();
        let why = String::from_utf8_lossy(&built.stderr).to_string();
        let Some(()) = common::link_or_skip(if built.status.success() {
            Ok(())
        } else {
            Err(why)
        }) else {
            return;
        };
        let ran = Command::new(&exe).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&ran.stdout), "10\n0\n");
        let ir = std::fs::read_to_string(d.join("main.ll")).unwrap();
        let body: Vec<&str> = ir
            .lines()
            .skip_while(|l| !(l.starts_with("define") && l.contains("@kara.last_prefix(")))
            .take_while(|l| *l != "}")
            .collect();
        assert!(!body.is_empty(), "no last_prefix in the IR");
        let marked = body
            .iter()
            .filter(|l| l.contains("load i64") && l.contains("!range"))
            .count();
        if ranged {
            assert!(marked >= 1, "{}", body.join("\n"));
        } else {
            assert_eq!(marked, 0, "{}", body.join("\n"));
        }
        let _ = std::fs::remove_dir_all(&d);
    }
}
