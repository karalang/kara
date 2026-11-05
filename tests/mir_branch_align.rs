//! `KARAC_MIR_ALIGN_BRANCHES=1` keeps x86 jumps in compiled Kāra code from
//! crossing or ending on a 32-byte line (B-2026-11-05-33): on a
//! Skylake-derived Intel core with the SKX102 microcode fix, a line holding
//! such a jump loses its decoded-uop cache, and kata 661's pixel loop, with
//! a checked add's `jo` across a line, ran 1.5x its aligned time. The
//! assembler pads instead (`-x86-branches-within-32B-boundaries`,
//! `src/backend/mir_aot.rs`). It is opt-in: on another host the padding
//! cost kata 207 2.3x. The test disassembles the program's own functions
//! with `objdump` and checks every jump, and that without the knob some
//! jump does cross, so the check can fail; without `objdump`, a linker, or
//! an x86-64 host it checks only the output.
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

/// Kata 661's smoother, smaller: three passes over a 60 by 60 image.
const SRC: &str = r#"fn smooth(img: Vec[Vec[i64]]) -> Vec[Vec[i64]] {
    let m = img.len();
    let n = img[0].len();
    let mut out: Vec[Vec[i64]] = Vec.new();
    for i in 0..m {
        let mut row: Vec[i64] = Vec.new();
        for j in 0..n {
            let mut sum = 0;
            let mut count = 0;
            for r in (i - 1).max(0)..(i + 2).min(m) {
                for c in (j - 1).max(0)..(j + 2).min(n) {
                    sum += img[r][c];
                    count += 1;
                }
            }
            row.push(sum / count);
        }
        out.push(row);
    }
    out
}
fn main() {
    let mut seed = 661;
    let mut img: Vec[Vec[i64]] = Vec.new();
    for _ in 0..60 {
        let mut row: Vec[i64] = Vec.new();
        for _ in 0..60 {
            seed = (seed * 1103515245 + 12345) % 2147483648;
            row.push(seed / 65536 % 256);
        }
        img.push(row);
    }
    for _ in 0..3 {
        img = smooth(img);
    }
    let mut checksum = 0;
    for row in img.iter() {
        for v in row.iter() {
            checksum = (checksum * 31 + v) % 1000000007;
        }
    }
    println(f"{checksum}");
}
"#;

fn scratch() -> PathBuf {
    let d = std::env::temp_dir().join(format!("karac-branch-align-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Builds `SRC` with the padding on or off; the executable, or `None`
/// without a linker.
fn build(dir: &Path, on: bool) -> Option<PathBuf> {
    let tag = if on { "on" } else { "off" };
    let file = dir.join(format!("{tag}.kara"));
    std::fs::write(&file, SRC).unwrap();
    let exe = dir.join(tag);
    let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
    if on {
        c.env("KARAC_MIR_ALIGN_BRANCHES", "1");
    }
    let built = c
        .args(["build", file.to_str().unwrap(), "-o", exe.to_str().unwrap()])
        .output()
        .unwrap();
    let why = String::from_utf8_lossy(&built.stderr).to_string();
    common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(why)
    })
    .map(|()| exe)
}

/// The jumps in the program's own functions (`karac.*` and `kara.*`;
/// the runtime archive is compiled elsewhere), and those of them that
/// cross a 32-byte line or end on one. `None` when `objdump` is missing.
fn jumps(exe: &Path) -> Option<(usize, Vec<String>)> {
    let out = Command::new("objdump")
        .args(["-d", "--no-show-raw-insn", exe.to_str().unwrap()])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    // (address, mnemonic) of every instruction, in order, with whether
    // it lies in one of the program's functions.
    let mut insns: Vec<(u64, String, bool)> = Vec::new();
    let mut ours = false;
    for line in text.lines() {
        if line.ends_with(">:") {
            ours = line.contains("<karac.") || line.contains("<kara.");
            continue;
        }
        let Some((addr, rest)) = line.trim_start().split_once(":\t") else {
            continue;
        };
        let Ok(at) = u64::from_str_radix(addr, 16) else {
            continue;
        };
        let op = rest.split_whitespace().next().unwrap_or("").to_string();
        insns.push((at, op, ours));
    }
    let mut seen = 0;
    let mut bad = Vec::new();
    for w in insns.windows(2) {
        let ((at, op, ours), (next, _, _)) = (&w[0], &w[1]);
        if !*ours || !op.starts_with('j') {
            continue;
        }
        seen += 1;
        let end = next - 1;
        if at / 32 != end / 32 || next % 32 == 0 {
            bad.push(format!("{op} at {at:#x}..{next:#x}"));
        }
    }
    Some((seen, bad))
}

#[test]
fn mir_x86_jumps_stay_within_32_byte_lines() {
    let want = karac::mir::lower::run_source(SRC)
        .expect("MIR interpreter")
        .output;
    let dir = scratch();
    let (on, off) = (build(&dir, true), build(&dir, false));
    for exe in [&on, &off].into_iter().flatten() {
        let out = Command::new(exe).output().unwrap();
        assert!(out.status.success(), "{out:?}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), want);
    }
    if cfg!(target_arch = "x86_64") {
        if let (Some(on), Some(off)) = (&on, &off) {
            if let (Some((seen, bad)), Some((_, bad_off))) = (jumps(on), jumps(off)) {
                assert!(seen > 20, "only {seen} jumps found: the parse is off");
                assert!(bad.is_empty(), "jumps across a 32-byte line: {bad:?}");
                assert!(
                    !bad_off.is_empty(),
                    "without the knob no jump crosses a line, so the check shows nothing"
                );
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
