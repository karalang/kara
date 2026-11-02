//! An auto-par task's signed sum keeps no prefix compares when the sum
//! cannot overflow however the loop runs (B-2026-11-02-17): it starts at a
//! constant, the loop's range is constant, and each term is bounded (kata
//! 7's `sum + (r as i64)` with `r: i32`), so no prefix of the in-order sum
//! leaves `i64` and the join has nothing to find. Kata 7 (K cut to 5M,
//! callgrind, Linux, two workers) went from 1650.7M to 1630.6M
//! instructions, against 1610.5M in order. A start that is not a constant,
//! or one near the type's end, keeps both compares, and the join still
//! finds an overflow where the in-order loop does.
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

/// Sums `((k + s) % 5) - 1` as an `i32` widened to `i64` over
/// `0..300000`, with the sum starting at `start`.
fn src(start: &str) -> String {
    format!(
        r#"fn val(k: i64) -> i32 {{
    let mut s = 0;
    let mut j = 0;
    while j < 20 {{
        s = s + (j % 3);
        j = j + 1;
    }}
    (((k + s) % 5) - 1) as i32
}}
fn seed() -> i64 {{
    7
}}
fn main() {{
    let mut sum: i64 = {start};
    let mut k: i64 = 0;
    while k < 300000 {{
        let r = val(k);
        sum = sum + (r as i64);
        k = k + 1;
    }}
    println(f"{{sum}}");
}}
"#
    )
}

/// The task function's signed `i64` compares, in the IR `src` builds to
/// (with `env` set), and the built program's output on four workers
/// (`None` without a linker).
fn build(
    dir: &std::path::Path,
    name: &str,
    src: &str,
    env: &[(&str, &str)],
) -> (usize, Option<std::process::Output>) {
    let file = dir.join(format!("{name}.kara"));
    std::fs::write(&file, src).unwrap();
    let exe = dir.join(name);
    let ll = dir.join(format!("{name}.ll"));
    let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
    c.env("KARAC_AUTO_PAR", "1");
    for (k, v) in env {
        c.env(k, v);
    }
    let built = c
        .env("KARAC_MIR_LL", &ll)
        .args(["build", file.to_str().unwrap(), "-o", exe.to_str().unwrap()])
        .output()
        .unwrap();
    let why = String::from_utf8_lossy(&built.stderr).to_string();
    let ir = std::fs::read_to_string(&ll).unwrap_or_else(|_| panic!("{name}: no IR\n{why}"));
    let task: Vec<&str> = ir
        .lines()
        .skip_while(|l| !(l.starts_with("define") && l.contains("@kara.main.ap0(")))
        .take_while(|l| *l != "}")
        .collect();
    assert!(!task.is_empty(), "{name}: no task function\n{why}");
    let compares = task
        .iter()
        .filter(|l| l.contains("icmp slt i64") || l.contains("icmp sgt i64"))
        .count();
    let ran = common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(why.clone())
    })
    .map(|()| {
        Command::new(&exe)
            .env("KARAC_THREADS", "4")
            .output()
            .unwrap()
    });
    (compares, ran)
}

fn in_order(dir: &std::path::Path, name: &str, src: &str) -> std::process::Output {
    let file = dir.join(format!("{name}-seq.kara"));
    std::fs::write(&file, src).unwrap();
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .env("KARAC_AUTO_PAR", "0")
        .args(["__mir-run", file.to_str().unwrap()])
        .output()
        .unwrap()
}

#[test]
fn mir_auto_par_bounded_sum_keeps_no_prefix_compares() {
    let dir = std::env::temp_dir().join(format!("karac-bounded-sum-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // 60000 rounds of -1, 0, 1, 2, 3.
    let quiet = src("0");
    let seq = in_order(&dir, "quiet", &quiet);
    assert_eq!(String::from_utf8_lossy(&seq.stdout), "300000\n", "{seq:?}");
    let (none, ran) = build(&dir, "quiet", &quiet, &[]);
    let (kept, ran_k) = build(&dir, "kept", &quiet, &[("KARAC_AUTO_PAR_BOUNDED_SUM", "0")]);
    // The opt-out is the control: the same loop keeps the least and
    // greatest prefix compares, two signed compares.
    assert_eq!(none + 2, kept, "bounded {none}, opted out {kept}");
    // A start that is not a constant keeps them too.
    let seeded = src("seed()");
    let (seeded_c, ran_s) = build(&dir, "seeded", &seeded, &[]);
    assert_eq!(seeded_c, kept, "seeded {seeded_c}, opted out {kept}");
    for (r, want) in [(ran, "300000\n"), (ran_k, "300000\n"), (ran_s, "300007\n")] {
        if let Some(r) = r {
            assert!(r.status.success(), "{r:?}");
            assert_eq!(String::from_utf8_lossy(&r.stdout), want);
        }
    }

    // A start within reach of `i64`'s end keeps the compares, and the
    // sum overflows where the in-order loop does.
    let over = src("9223372036854775000");
    let seq_o = in_order(&dir, "over", &over);
    assert_eq!(seq_o.status.code(), Some(101), "{seq_o:?}");
    let (over_c, ran_o) = build(&dir, "over", &over, &[]);
    assert_eq!(over_c, kept, "near the end {over_c}, opted out {kept}");
    if let Some(o) = ran_o {
        assert_eq!(o.status.code(), Some(101), "{o:?}");
        assert!(o.stdout.is_empty(), "{o:?}");
        let (want, got) = (
            String::from_utf8_lossy(&seq_o.stderr),
            String::from_utf8_lossy(&o.stderr),
        );
        let first = |s: &str| s.lines().next().unwrap_or("").to_string();
        assert_eq!(first(&got), first(&want), "{got}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
