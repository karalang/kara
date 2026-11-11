//! `Duration` below a millisecond (library/time-random-env.md;
//! B-2026-11-11-33): `Duration.us(n)` / `Duration.ns(n)` build one,
//! `as_us()` / `as_ns()` / `as_secs()` read one, and `sleep` of a part of
//! a millisecond pauses that long. Rust's `from_micros` /
//! `from_nanos` / `as_millis` / `as_micros` / `as_nanos` mend to them.
//! Pinned on check, the interpreter, a built binary and the ASan lane.
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "karac-duration-sub-ms-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Scratch(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn asan_available() -> bool {
    if std::env::var_os("KARAC_SKIP_ASAN_TESTS").is_some() {
        return false;
    }
    let dir = Scratch::new("asan-probe");
    let c = dir.path().join("probe.c");
    let exe = dir.path().join("probe");
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

/// The program's stdout on the interpreter, as a built binary and on the
/// ASan lane, each where it can run here.
fn every_lane(name: &str, src: &str, want: &str) {
    let dir = Scratch::new(name);
    let file = dir.path().join("main.kara");
    std::fs::write(&file, src).unwrap();
    let karac = env!("CARGO_BIN_EXE_karac");
    let f = file.to_str().unwrap();

    let out = Command::new(karac)
        .args(["__mir-run", f])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{name}: interpreter: {err}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        want,
        "{name}: interpreter"
    );

    let exe = dir.path().join("main");
    let built = Command::new(karac)
        .args(["build", f, "-o", exe.to_str().unwrap()])
        .output()
        .unwrap();
    let linked = common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&built.stderr).to_string())
    });
    if linked.is_some() {
        let out = Command::new(&exe).stdin(Stdio::null()).output().unwrap();
        assert!(out.status.success(), "{name}: binary: {:?}", out.status);
        assert_eq!(String::from_utf8_lossy(&out.stdout), want, "{name}: binary");
    }

    if !asan_available() {
        eprintln!("{name}: skipping the ASan lane: no ASan-capable cc");
        return;
    }
    let out = Command::new(karac)
        .args(["__mir-llvm-asan", f])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    if out.status.code() == Some(3) && err.contains("Linker failed") {
        let _ = common::link_or_skip(Err(err.into_owned()));
        return;
    }
    assert_eq!(out.status.code(), Some(0), "{name}: ASan:\n{err}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), want, "{name}: ASan");
}

#[test]
fn a_duration_below_a_millisecond_builds_reads_and_sleeps() {
    let src = r#"import std.time.{Duration, Instant, sleep};

fn main() {
    let d = Duration.us(1500);
    println(f"{d.as_secs()} {d.as_ms()} {d.as_us()} {d.as_ns()}");
    println(Duration.ns(999).as_us());
    println(Duration.secs(3).as_us());
    println(Duration.us(1000) == Duration.ms(1));
    println(Duration.ns(1) < Duration.us(1));
    let start = Instant.now();
    sleep(Duration.us(50));
    let took = start.elapsed();
    println(took.as_us() >= 50);
    sleep(Duration.ns(0));
    println(start.elapsed().as_ns() >= took.as_ns());
}
"#;
    let dir = Scratch::new("check");
    let file = dir.path().join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "check: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    every_lane(
        "units",
        src,
        "0 1 1500 1500000\n0\n3000000\ntrue\ntrue\ntrue\ntrue\n",
    );
}

/// Rust's constructors and readers mend to Kāra's, and the mended
/// program runs.
#[test]
fn rusts_sub_millisecond_duration_spellings_mend_and_run() {
    let src = "use std::time::{Duration, Instant};\n\n\
               fn main() {\n    let start = Instant::now();\n    \
               let e = start.elapsed();\n    \
               println!(\"{} {}\", e.as_micros() >= 0, e.as_millis() < 1000);\n    \
               println!(\"{} {}\", Duration::from_nanos(2500).as_micros(), \
               Duration::from_micros(7).as_nanos());\n}\n";
    let dir = Scratch::new("mend");
    let file = dir.path().join("main.kara");
    std::fs::write(&file, src).unwrap();
    let karac = env!("CARGO_BIN_EXE_karac");
    let f = file.to_str().unwrap();
    // The Mend loop: `karac fix` applies one phase's edits a pass.
    let mut mended = src.to_string();
    for _ in 0..6 {
        let _ = Command::new(karac).args(["fix", f]).output().unwrap();
        let now = std::fs::read_to_string(&file).unwrap();
        if now == mended {
            break;
        }
        mended = now;
    }
    for want in [
        "e.as_us() >= 0",
        "e.as_ms() < 1000",
        "Duration.ns(2500).as_us()",
        "Duration.us(7).as_ns()",
    ] {
        assert!(mended.contains(want), "{want}:\n{mended}");
    }
    let check = Command::new(karac).args(["check", f]).output().unwrap();
    assert!(
        check.status.success(),
        "{}\n{mended}",
        String::from_utf8_lossy(&check.stderr)
    );
    every_lane("mend", &mended, "true true\n2 7000\n");
}

/// A program's own `Duration` keeps its plain "no method": the rename
/// is offered only where the library's reader is.
#[test]
fn a_programs_own_duration_gets_no_reader_rename() {
    let src = "struct Duration { ms: i64 }\n\
               fn main() {\n    let d = Duration { ms: 5 };\n    println(d.as_millis());\n}\n";
    let dir = Scratch::new("own");
    let file = dir.path().join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", file.to_str().unwrap()])
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        said.contains("no method 'as_millis' on type 'Duration'"),
        "{said}"
    );
    assert!(!said.contains("as_ms()"), "{said}");
}

/// `Duration` adds, subtracts and scales (B-2026-11-11-72): a backoff
/// that doubles, a total of phases, a span less another.
#[test]
fn a_duration_adds_subtracts_and_scales() {
    let src = r#"import std.time.{Duration, Instant, sleep};

fn main() {
    println((Duration.ms(1) + Duration.us(5)).as_us());
    println((Duration.ms(3) - Duration.ms(5)).as_us());
    println((Duration.ms(2) * 3).as_ms());
    println((Duration.secs(1) / 3).as_ns());
    let mut delay = Duration.us(100);
    let mut total = Duration.ns(0);
    for _ in 0..4 {
        total += delay;
        delay = delay * 2;
    }
    println(total.as_us());
    total -= Duration.us(500);
    println(total.as_us());
    let start = Instant.now();
    sleep(Duration.us(20) * 2);
    println(start.elapsed() >= Duration.us(40));
}
"#;
    every_lane(
        "arith",
        src,
        "1005\n-2000\n6\n333333333\n1500\n1000\ntrue\n",
    );
}

/// `src` after `karac fix` until nothing changes, and whether it checks.
fn mended(tag: &str, src: &str) -> (String, bool) {
    let dir = Scratch::new(tag);
    let file = dir.path().join("main.kara");
    std::fs::write(&file, src).unwrap();
    let karac = env!("CARGO_BIN_EXE_karac");
    let f = file.to_str().unwrap();
    let mut text = src.to_string();
    for _ in 0..6 {
        let _ = Command::new(karac).args(["fix", f]).output().unwrap();
        let now = std::fs::read_to_string(&file).unwrap();
        if now == text {
            break;
        }
        text = now;
    }
    let check = Command::new(karac).args(["check", f]).output().unwrap();
    (text, check.status.success())
}

/// Rust's pause, `thread::sleep(d)` under `use std::thread;`, mends to
/// `std.time.sleep` (B-2026-11-11-73): the import line becomes
/// `import std.time.sleep;` and the call loses its module.
#[test]
fn rusts_thread_sleep_under_use_std_thread_mends_to_std_time_sleep() {
    let src = "use std::thread;\nuse std::time::Duration;\n\nfn main() {\n    \
               thread::sleep(Duration::from_millis(1));\n    \
               thread::sleep(Duration::from_micros(5));\n    println!(\"ok\");\n}\n";
    let (text, ok) = mended("use-thread", src);
    assert!(ok, "{text}");
    assert!(text.contains("import std.time.sleep;\n"), "{text}");
    assert!(!text.contains("thread"), "{text}");
    assert_eq!(text.matches("    sleep(Duration.").count(), 2, "{text}");
    every_lane("use-thread-run", &text, "ok\n");
}

/// A bare `std::thread::sleep(d)` gets the import once, however many
/// calls there are, and `use std::thread::sleep;` becomes the import.
#[test]
fn rusts_std_thread_sleep_paths_get_one_import() {
    let src = "use std::time::{Duration, Instant};\n\nfn main() {\n    \
               let s = Instant::now();\n    std::thread::sleep(Duration::from_micros(30));\n    \
               std::thread::sleep(Duration::from_micros(30));\n    \
               println!(\"{}\", s.elapsed().as_micros() >= 60);\n}\n";
    let (text, ok) = mended("std-thread", src);
    assert!(ok, "{text}");
    assert_eq!(text.matches("import std.time.sleep;").count(), 1, "{text}");
    assert!(!text.contains("thread"), "{text}");
    every_lane("std-thread-run", &text, "true\n");

    let src = "use std::thread::sleep;\nuse std::time::Duration;\n\nfn main() {\n    \
               sleep(Duration::from_millis(1));\n    println!(\"ok\");\n}\n";
    let (text, ok) = mended("use-sleep", src);
    assert!(ok, "{text}");
    assert!(text.starts_with("import std.time.sleep;\n"), "{text}");
}

/// `use std::thread;` stays while the program spawns through it: the
/// spawn has its own diagnostic, and the line is not the pause's alone.
#[test]
fn use_std_thread_with_a_spawn_is_left_to_the_spawns_diagnostic() {
    let src = "use std::thread;\n\nfn main() {\n    let h = thread::spawn(|| 1);\n    \
               thread::sleep(std::time::Duration::from_millis(1));\n}\n";
    let (text, ok) = mended("spawn", src);
    assert!(!ok, "{text}");
    assert!(text.contains("import std.thread;"), "{text}");
    assert!(!text.contains("std.time.sleep"), "{text}");
}
