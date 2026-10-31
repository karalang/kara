//! An auto-par task states the loop's constant bounds (B-2026-10-29-50).
//! Each task takes its run's `lo` and `hi` as captures, which say nothing
//! of the loop's range, so the task clamps them to the bounds it knows: a
//! `for`'s literal or constant-`let` start and end, or a counting
//! `while`'s end and its counter's `let` on the way in when nothing
//! between names the counter. The clamp is a no-op that tells LLVM the
//! counter's range: kata 4's `k % R` in the task went from a signed 64-bit
//! magic division to an unsigned one (865.6M to 855.6M instructions at two
//! workers on Linux, against 599.7M in order). A clamp can only shorten a
//! run, and the output is the in-order program's.
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

const SRC: &str = r#"fn pair(k: i64) -> Array[i64, 2] {
    let mut s = 0;
    let mut j = 0;
    while j < 50 {
        s = s + (j % 3) - (j % 3);
        j = j + 1;
    }
    [k + s, 2 * k + s]
}
fn main() {
    let n = 100003;
    let mut sum = 0;
    let mut k = 5;
    while k < n {
        let p = pair(k % 1000);
        sum = sum + p[0] + p[1];
        k = k + 1;
    }
    let m = sum % 7 + 99990;
    let mut t = 0;
    for i in 3..m {
        let p = pair(i % 1000);
        t = t + p[0] + p[1];
    }
    let mut j = 5;
    j = j - 3;
    let mut u = 0;
    while j < n {
        let p = pair(j % 1000);
        u = u + p[0] + p[1];
        j = j + 1;
    }
    println(f"{sum} {t} {u}");
}
"#;

const WANT: &str = "149849979 149838021 149850006\n";

fn karac(auto: bool) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
    c.env("KARAC_AUTO_PAR", if auto { "1" } else { "0" });
    c
}

/// The bounds each task `@kara.main.apN` clamps its run to, in order: the
/// constants of its `icmp slt` (start) and `icmp sgt` (end) on a capture.
fn clamps(ll: &str) -> Vec<Vec<String>> {
    let ir = std::fs::read_to_string(ll).unwrap();
    let mut out: Vec<Vec<String>> = Vec::new();
    let mut inside = false;
    for l in ir.lines() {
        if l.starts_with("define internal void @kara.main.ap") {
            inside = true;
            out.push(Vec::new());
        } else if l == "}" {
            inside = false;
        } else if inside {
            for (op, side) in [("icmp slt i64 %", "start"), ("icmp sgt i64 %", "end")] {
                let Some(rest) = l.split_once(op).map(|(_, r)| r) else {
                    continue;
                };
                if let Some((_, c)) = rest.split_once(", ") {
                    if c.parse::<i64>().is_ok() {
                        out.last_mut().unwrap().push(format!("{side} {c}"));
                    }
                }
            }
        }
    }
    out
}

#[test]
fn mir_auto_par_task_clamps_its_run_to_the_loops_constant_bounds() {
    let dir = std::env::temp_dir().join(format!("karac-range-facts-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, SRC).unwrap();
    let file = file.to_str().unwrap();

    let seq = karac(false).args(["__mir-run", file]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&seq.stdout), WANT);
    let par = karac(true)
        .env("KARAC_AUTO_PAR_DEBUG", "1")
        .args(["__mir-run", file])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&par.stdout), WANT);
    let err = String::from_utf8_lossy(&par.stderr);
    assert!(
        err.lines()
            .any(|l| l == "auto-par: loops as tasks: main@14, main@21, main@28"),
        "{err}"
    );

    // The `while` from `let mut k = 5` to the constant `n`; the `for` from
    // 3 to a computed end; the `while` whose counter is written after its
    // `let`, so only its end is known.
    let stated: &[&[&str]] = &[&["start 5", "end 100003"], &["start 3"], &["end 100003"]];
    for (range, want) in [(None, stated), (Some("0"), &[&[][..], &[], &[]][..])] {
        let exe = dir.join("main");
        let ll = dir.join("main.ll");
        let mut build = karac(true);
        build.env("KARAC_MIR_LL", &ll);
        if let Some(v) = range {
            build.env("KARAC_AUTO_PAR_RANGE", v);
        }
        let built = build
            .args(["build", file, "-o", exe.to_str().unwrap()])
            .output()
            .unwrap();
        let why = String::from_utf8_lossy(&built.stderr).to_string();
        let got = clamps(ll.to_str().unwrap());
        assert_eq!(got, want, "KARAC_AUTO_PAR_RANGE={range:?}: {why}");
        if common::link_or_skip(if built.status.success() {
            Ok(())
        } else {
            Err(why.clone())
        })
        .is_some()
        {
            let out = Command::new(&exe)
                .env("KARAC_THREADS", "4")
                .output()
                .unwrap();
            assert!(out.status.success(), "{why}");
            assert_eq!(String::from_utf8_lossy(&out.stdout), WANT);
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
