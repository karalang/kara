//! An auto-par task keeps its wide sum's run in the sum's own type
//! (B-2026-10-28-35): each term is a checked `i64` add and two compares,
//! and only a run that leaves `i64` folds into the `i128` base. The fold
//! is the overflow test's set edge, which codegen weighs as rare, so its
//! `i128` values stay out of the loop's registers: kata 4 (this shape,
//! `sum = sum + p[0] + p[1]` over a call per iteration) went from 1,086M
//! to 866M instructions at two workers on Linux, against 600M in order.
//! The output is the in-order program's, on the interpreter and built.
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
    let mut sum = 0;
    let mut k = 0;
    while k < 100000 {
        let p = pair(k % 1000);
        sum = sum + p[0] + p[1];
        k = k + 1;
    }
    println(f"{sum}");
}
"#;

const WANT: &str = "149850000\n";

const RARE: &str = r#"!{!"branch_weights", i32 1, i32 2000}"#;

fn karac(auto: bool) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
    c.env("KARAC_AUTO_PAR", if auto { "1" } else { "0" });
    c
}

/// The lines of the task's function in the IR at `ll`.
fn task_fn(ll: &str) -> Vec<String> {
    let ir = std::fs::read_to_string(ll).unwrap();
    ir.lines()
        .skip_while(|l| !(l.starts_with("define") && l.contains("@kara.main.ap0(")))
        .take_while(|l| *l != "}")
        .map(str::to_string)
        .collect()
}

#[test]
fn mir_auto_par_task_sums_in_its_type_and_folds_rarely() {
    let dir = std::env::temp_dir().join(format!("karac-narrow-run-{}", std::process::id()));
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
            .any(|l| l == "auto-par: reductions: main@13 (sum at 15)"),
        "{err}"
    );

    for (cold, weighted) in [(None, true), (Some("0"), false)] {
        let exe = dir.join("main");
        let ll = dir.join("main.ll");
        let mut build = karac(true);
        build.env("KARAC_MIR_LL", &ll);
        if let Some(v) = cold {
            build.env("KARAC_MIR_COLD_OVERFLOW", v);
        }
        let built = build
            .args(["build", file, "-o", exe.to_str().unwrap()])
            .output()
            .unwrap();
        let why = String::from_utf8_lossy(&built.stderr).to_string();
        let task = task_fn(ll.to_str().unwrap());
        // The run's two terms are `i64` adds that report overflow, not
        // `i128` adds.
        let adds = task
            .iter()
            .filter(|l| l.contains("@llvm.sadd.with.overflow.i64("))
            .count();
        assert!(adds >= 2, "{why}\n{}", task.join("\n"));
        let rare = task.iter().filter(|l| l.contains("], !prof !")).count();
        if weighted {
            assert!(rare >= 2, "{}", task.join("\n"));
            let ir = std::fs::read_to_string(&ll).unwrap();
            assert!(ir.lines().any(|l| l.ends_with(RARE)), "no {RARE}");
        } else {
            assert_eq!(rare, 0, "{}", task.join("\n"));
        }
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
