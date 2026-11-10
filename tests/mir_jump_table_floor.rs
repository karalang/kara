//! A four-way `match` on random data lowers to compares, not a jump table,
//! on x86: LLVM's floor is 4 entries, the AOT pipeline's 5, GCC's x86
//! value (B-2026-11-05-27). LLVM 22 proves `(state / 1024) % 4` is in
//! [0, 3], so kata 155's `if op == 0 .. else if op == 1 .. else if op == 2
//! .. else` became a four-case switch with an unreachable default, and its
//! one indirect jump on random data ran the bench at 690 ms against
//! legacy's 233 (Rust, on LLVM 22, pays it too: 705). The pin builds that
//! dispatch at the default and at `KARAC_MIR_JUMP_TABLE_MIN=4`, asserts
//! the two print the same, and that `main` jumps indirectly only in the
//! second.
#![cfg(all(feature = "llvm", target_arch = "x86_64", target_os = "linux"))]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

const SRC: &str = r#"fn main() {
    let mut st: Vec[i64] = Vec.new();
    let mut state = 12345;
    let mut sz = 0;
    let mut sink = 0;
    let mut i = 0;
    while i < 1000 {
        state = (state * 1103515245 + 12345) & 2147483647;
        let op = (state / 1024) % 4;
        if sz == 0 {
            st.push(state % 1000);
            sz = sz + 1;
        } else if op == 0 {
            st.push(state % 1000);
            sz = sz + 1;
        } else if op == 1 {
            st.pop();
            sz = sz - 1;
        } else if op == 2 {
            sink = sink + st[st.len() - 1];
        } else {
            sink = sink - st[0];
        }
        i = i + 1;
    }
    println(f"{sink}");
}
"#;

/// The indirect jumps (`jmp *...`) in the program's `main`, or `None`
/// when `objdump` is missing.
fn indirect_jumps(exe: &Path) -> Option<usize> {
    let out = Command::new("objdump")
        .args(["-d", "--no-show-raw-insn", exe.to_str().unwrap()])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let mut ours = false;
    let mut n = 0;
    for line in text.lines() {
        if line.ends_with(">:") {
            ours = line.ends_with("<karac.main>:");
        } else if ours && line.contains("jmp") && line.contains('*') {
            n += 1;
        }
    }
    Some(n)
}

fn dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("karac-jump-table-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The binary karac builds for `SRC` with the given floor, and what it
/// prints; `None` when no runtime archive is present to link it.
fn build(d: &Path, floor: Option<&str>) -> Option<(PathBuf, String)> {
    let tag = floor.unwrap_or("default");
    let file = d.join("main.kara");
    let exe = d.join(tag);
    std::fs::write(&file, SRC).unwrap();
    let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
    if let Some(f) = floor {
        c.env("KARAC_MIR_JUMP_TABLE_MIN", f);
    }
    let built = c
        .env("KARAC_AUTO_PAR", "0")
        .args(["build", file.to_str().unwrap(), "-o", exe.to_str().unwrap()])
        .output()
        .unwrap();
    common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&built.stderr).to_string())
    })?;
    let ran = Command::new(&exe).output().unwrap();
    assert!(ran.status.success(), "{tag}: the program failed");
    Some((exe, String::from_utf8_lossy(&ran.stdout).to_string()))
}

#[test]
fn a_four_way_dispatch_lowers_to_compares_on_x86() {
    let d = dir();
    let Some((default, said)) = build(&d, None) else {
        return;
    };
    let (llvm, llvm_said) = build(&d, Some("4")).unwrap();
    assert_eq!(said, llvm_said);
    assert_eq!(said, "-20190\n");
    let Some(ours) = indirect_jumps(&default) else {
        eprintln!("skipping the shape check: no objdump");
        return;
    };
    assert_eq!(ours, 0, "the default build kept a jump table");
    assert_eq!(
        indirect_jumps(&llvm),
        Some(1),
        "LLVM's floor of 4 no longer makes a jump table here: the pin is void"
    );
    let _ = std::fs::remove_dir_all(&d);
}
