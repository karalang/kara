//! An auto-par loop that pushes exactly once per iteration collects in
//! place (B-2026-11-02-8): `acc` is reserved once for every push before
//! any task runs, each task stores into its own window past `acc`'s
//! length, and the join counts the windows in, in iteration order. The
//! chunked form it replaces had each task grow a `Vec` of its own that
//! the join then appended, holding the collected elements twice at peak.
//! A push under a condition keeps the chunks, and so does
//! `KARAC_AUTO_PAR_WINDOW=0`.
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

/// Pushes one element per iteration of `0..n` onto a `Vec` that already
/// holds 7, or with `cond`, only the iterations whose value is a multiple
/// of 3. `peak` prints the process's peak resident memory first.
fn src(n: u64, cond: bool) -> String {
    src_with(n, cond, "")
}

fn src_with(n: u64, cond: bool, peak: &str) -> String {
    let push = if cond {
        "if w % 3 == 0 {\n            parts.push(w);\n        }"
    } else {
        "parts.push((t * 1000003 + w) % 1000000007);"
    };
    format!(
        r#"fn work(t: i64) -> i64 {{
    let mut s = t;
    let mut j = 0;
    while j < 30 {{
        s = (s * 31 + j) % 1000003;
        j = j + 1;
    }}
    s
}}
fn main() {{
    let mut parts: Vec[i64] = Vec.new();
    parts.push(7);
    for t in 0..{n} {{
        let w = work(t);
        {push}
    }}
    {peak}
    let mut h: i64 = 0;
    for x in parts.iter() {{
        h = (h * 131 + x) % 1000000007;
    }}
    println(f"{{parts.len()}} {{h}} {{parts[0]}}");
}}
"#
    )
}

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("karac-par-window-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

/// The in-order interpreter's output.
fn in_order(dir: &Path, src: &str) -> String {
    let file = dir.join("seq.kara");
    std::fs::write(&file, src).unwrap();
    let out = karac()
        .env("KARAC_AUTO_PAR", "0")
        .args(["__mir-run", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Builds `src` (`window` false sets `KARAC_AUTO_PAR_WINDOW=0`): whether
/// the loop ran as tasks, whether its task grows a `Vec` (the chunked
/// form), and the executable (`None` without a linker).
fn build(dir: &Path, name: &str, src: &str, window: bool) -> (bool, bool, Option<PathBuf>) {
    let file = dir.join(format!("{name}.kara"));
    std::fs::write(&file, src).unwrap();
    let exe = dir.join(name);
    let ll = dir.join(format!("{name}.ll"));
    let mut c = karac();
    if !window {
        c.env("KARAC_AUTO_PAR_WINDOW", "0");
    }
    let built = c
        .env("KARAC_AUTO_PAR", "1")
        .env("KARAC_AUTO_PAR_DEBUG", "1")
        .env("KARAC_MIR_LL", &ll)
        .args(["build", file.to_str().unwrap(), "-o", exe.to_str().unwrap()])
        .output()
        .unwrap();
    let why = String::from_utf8_lossy(&built.stderr).to_string();
    let ir = std::fs::read_to_string(&ll).unwrap_or_else(|_| panic!("{name}: no IR\n{why}"));
    let tasks = why.contains("auto-par: reductions: main@");
    let task: Vec<&str> = ir
        .lines()
        .skip_while(|l| !(l.starts_with("define") && l.contains("@kara.main.ap0(")))
        .take_while(|l| *l != "}")
        .collect();
    let grows = task.iter().any(|l| {
        // A push grows through its element size's `grow1.<size>`.
        l.contains("@karac_realloc_or_panic")
            || l.contains("@karac_mir_vec_reserve")
            || l.contains("@grow1.")
    });
    let ran = common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(why.clone())
    });
    (tasks, grows, ran.map(|()| exe))
}

fn run(exe: &Path, threads: &str) -> String {
    let out = Command::new(exe)
        .env("KARAC_THREADS", threads)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn mir_auto_par_window_collects_in_place() {
    let dir = scratch("in-place");
    let s = src(200000, false);
    let want = in_order(&dir, &s);
    assert_eq!(want, "200001 770083063 7\n");
    let (tasks, grows, exe) = build(&dir, "window", &s, true);
    assert!(tasks, "the loop runs as tasks");
    assert!(!grows, "an in-place task grows no Vec of its own");
    let (tasks_c, grows_c, exe_c) = build(&dir, "chunks", &s, false);
    assert!(
        tasks_c && grows_c,
        "KARAC_AUTO_PAR_WINDOW=0 keeps the chunks"
    );
    if let (Some(exe), Some(exe_c)) = (exe, exe_c) {
        for threads in ["1", "2", "4"] {
            assert_eq!(run(&exe, threads), want, "{threads} workers");
            assert_eq!(run(&exe_c, threads), want, "{threads} workers, chunked");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn mir_auto_par_window_conditional_push_keeps_chunks() {
    let dir = scratch("cond");
    let s = src(200000, true);
    let want = in_order(&dir, &s);
    assert_eq!(want, "66667 544304051 7\n");
    let (tasks, grows, exe) = build(&dir, "cond", &s, true);
    assert!(tasks, "the loop runs as tasks");
    assert!(grows, "a conditional push keeps a Vec per task");
    if let Some(exe) = exe {
        for threads in ["1", "4"] {
            assert_eq!(run(&exe, threads), want, "{threads} workers");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Peak resident memory of `exe` on four workers, in KiB, as the program
/// reads its own `VmHWM`. A child's `ru_maxrss` would count the pages it
/// shared with this (large) test process between its fork and its exec.
#[cfg(target_os = "linux")]
fn peak_kib(exe: &Path) -> i64 {
    let out = run(exe, "4");
    let line = out
        .lines()
        .find(|l| l.starts_with("VmHWM:"))
        .unwrap_or_else(|| panic!("no VmHWM in {out}"));
    line.trim_start_matches("VmHWM:")
        .trim()
        .trim_end_matches("kB")
        .trim()
        .parse()
        .unwrap()
}

/// Kata 270's shape: 2M `i64`s (16 MiB) collected on four workers peak at
/// one copy in place, where the chunks held two (B-2026-11-02-8 measured
/// 20.9 against 37.5 MB on Linux).
#[cfg(target_os = "linux")]
#[test]
fn mir_auto_par_window_peak_holds_one_copy() {
    let dir = scratch("peak");
    let peak = r#"let status = fs.read_to_string("/proc/self/status").unwrap();
    for l in status.lines() {
        if l.starts_with("VmHWM") {
            println(l);
        }
    }"#;
    let s = src_with(2000000, false, peak);
    let (_, _, exe) = build(&dir, "window", &s, true);
    let (_, _, exe_c) = build(&dir, "chunks", &s, false);
    if let (Some(exe), Some(exe_c)) = (exe, exe_c) {
        let (w, c) = (peak_kib(&exe), peak_kib(&exe_c));
        assert!(w + 8 * 1024 < c, "in place {w} KiB, chunked {c} KiB");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Whether `cc -fsanitize=address` links and runs here.
fn asan_available(dir: &Path) -> bool {
    if std::env::var_os("KARAC_SKIP_ASAN_TESTS").is_some() {
        return false;
    }
    let c = dir.join("probe.c");
    let exe = dir.join("probe");
    std::fs::write(&c, "int main(void){return 0;}\n").expect("write probe");
    Command::new("cc")
        .arg("-fsanitize=address")
        .arg(&c)
        .arg("-o")
        .arg(&exe)
        .status()
        .is_ok_and(|s| s.success())
        && Command::new(&exe).status().is_ok_and(|s| s.success())
}

/// A task that panics mid-run: the join stops at its handle with `acc`
/// holding the windows joined before it, and the windows never absorbed
/// (elements with no drop) leave nothing to free.
const PANICS: &str = r#"fn spin(t: i64) -> i64 {
    let mut s = t;
    let mut j = 0;
    while j < 200 {
        s = (s * 31 + j) % 1000003;
        j = j + 1;
    }
    if t == 150000 {
        panic("boom at 150000");
    }
    s
}
fn main() {
    let mut parts: Vec[i64] = Vec.new();
    for t in 0..200000 {
        parts.push(spin(t));
    }
    println(f"{parts.len()}");
}
"#;

/// The collecting loop in a task its group cancels while the task waits.
/// A body that would suspend here keeps its loops in order today
/// (`suspends_as_value`); this pins that the cancel exit stays clean
/// whichever form the loop takes.
const CANCELLED: &str = r#"import std.time.{sleep, Duration};
fn spin(t: i64) -> i64 {
    let mut s = t;
    let mut j = 0;
    while j < 2000 {
        s = (s * 31 + j) % 1000003;
        j = j + 1;
    }
    s
}
fn collect() -> i64 {
    let mut parts: Vec[i64] = Vec.new();
    for t in 0..60000 {
        parts.push(spin(t));
    }
    parts.len()
}
fn main() {
    let g = TaskGroup.new();
    let h = g.spawn(|| { let (a, b) = par { collect(), 1 }; a + b });
    sleep(Duration.ms(20));
    g.cancel();
    println(h.try_join().is_err());
}
"#;

fn asan_case(tag: &str, src: &str, code: i32, out: Option<&str>, err: &str) {
    let d = scratch(tag);
    if !asan_available(&d) {
        eprintln!("skipping: no ASan-capable cc");
        return;
    }
    std::fs::write(d.join("main.kara"), src).unwrap();
    for threads in ["1", "4"] {
        let o = karac()
            .current_dir(&d)
            .env("ASAN_OPTIONS", "exitcode=86")
            .env("KARAC_THREADS", threads)
            .args(["__mir-llvm-asan", "main.kara"])
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&o.stderr);
        if o.status.code() == Some(3) && stderr.contains("Linker failed") {
            let _ = common::link_or_skip(Err(stderr.into_owned()));
            return;
        }
        assert!(
            !stderr.contains("Sanitizer"),
            "{threads} workers:\n{stderr}"
        );
        assert_eq!(o.status.code(), Some(code), "{threads} workers:\n{stderr}");
        let stdout = String::from_utf8_lossy(&o.stdout);
        match out {
            Some(out) => assert_eq!(stdout, out, "{threads} workers"),
            None => assert!(
                stdout == "true\n" || stdout == "false\n",
                "{threads} workers: {stdout}"
            ),
        }
        assert!(stderr.contains(err), "{threads} workers:\n{stderr}");
    }
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn mir_auto_par_window_panic_is_asan_clean() {
    asan_case("panic", PANICS, 101, Some(""), "boom at 150000");
}

#[test]
fn mir_auto_par_window_cancel_is_asan_clean() {
    // On one worker the task can run to its end before the cancel lands.
    asan_case("cancel", CANCELLED, 0, None, "");
}
