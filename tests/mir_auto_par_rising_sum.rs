//! An auto-par task's wide sum whose terms are never negative keeps no
//! prefix compares (B-2026-10-31-39): its run only rises, so its least
//! prefix stays 0 and its greatest is the run itself. Kata 9's count
//! (`let inc: i64 = if r { 1 } else { 0 }; sum = sum + inc`) went from
//! 352.8M to 332.8M instructions at two workers on Linux, against 297.7M
//! in order. A term that can be negative keeps both compares, and the
//! join still finds an overflow where the in-order loop does.
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

/// Counts the multiples of 3 below 300000 with a term of `one` or `other`,
/// starting the sum at `start`.
fn src(other: &str, start: &str) -> String {
    format!(
        r#"fn hit(k: i64) -> bool {{
    let mut s = 0;
    let mut j = 0;
    while j < 20 {{
        s = s + (j % 3);
        j = j + 1;
    }}
    (k + s) % 3 == 1
}}
fn main() {{
    let mut sum: i64 = {start};
    let mut k: i64 = 0;
    while k < 300000 {{
        let r = hit(k);
        let inc: i64 = if r {{ 1 }} else {{ {other} }};
        sum = sum + inc;
        k = k + 1;
    }}
    println(f"{{sum}}");
}}
"#
    )
}

fn karac(auto: bool) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
    c.env("KARAC_AUTO_PAR", if auto { "1" } else { "0" });
    c
}

/// The task function's signed `i64` compares, in the IR `src` builds to,
/// and the built program's output on four workers (`None` without a
/// linker).
fn build(dir: &std::path::Path, name: &str, src: &str) -> (usize, Option<std::process::Output>) {
    let file = dir.join(format!("{name}.kara"));
    std::fs::write(&file, src).unwrap();
    let exe = dir.join(name);
    let ll = dir.join(format!("{name}.ll"));
    let built = karac(true)
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
    karac(false)
        .args(["__mir-run", file.to_str().unwrap()])
        .output()
        .unwrap()
}

#[test]
fn mir_auto_par_rising_sum_keeps_no_prefix_compares() {
    let dir = std::env::temp_dir().join(format!("karac-rising-sum-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let rising = src("0", "0");
    let falling = src("-1", "0");
    let seq = in_order(&dir, "rising", &rising);
    assert_eq!(String::from_utf8_lossy(&seq.stdout), "100000\n");
    let seq_f = in_order(&dir, "falling", &falling);
    assert_eq!(String::from_utf8_lossy(&seq_f.stdout), "-100000\n");

    let (up, ran) = build(&dir, "rising", &rising);
    let (both, ran_f) = build(&dir, "falling", &falling);
    // The same loop but for the term's sign: the rising task drops the
    // least and greatest prefix compares, two signed compares.
    assert_eq!(up + 2, both, "rising {up}, falling {both}");
    if let (Some(r), Some(f)) = (ran, ran_f) {
        assert!(r.status.success() && f.status.success(), "{r:?} {f:?}");
        assert_eq!(String::from_utf8_lossy(&r.stdout), "100000\n");
        assert_eq!(String::from_utf8_lossy(&f.stdout), "-100000\n");
    }

    // A rising sum that leaves `i64` overflows where the in-order loop
    // does: the join reads the greatest prefix as the run.
    let over = src("0", "9223372036854700000");
    let seq_o = in_order(&dir, "over", &over);
    assert_eq!(seq_o.status.code(), Some(101), "{seq_o:?}");
    let (_, ran_o) = build(&dir, "over", &over);
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
