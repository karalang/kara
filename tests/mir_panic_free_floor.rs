//! A compute program built on the MIR backend links no Rust panic: the
//! print path (`karac_mir_fmt_*`, `karac_mir_print_end`, `karac_mir_flush`),
//! `karac_mir_run_main`, `karac_mir_vec_reserve` and the plain abort report
//! of `karac_mir_abort_at` reach none, so std's panic hook and its DWARF
//! backtrace symbolizer stay out of the binary (B-2026-10-28-36: a kata
//! binary was 366 KB on Linux against 68 KB without them). The hook's
//! "RUST_BACKTRACE" note is the marker: it is in the binary exactly when the
//! hook is. A `panic(msg)` with a formatted message still reports through
//! the full report, which the last test checks against the interpreter's
//! wording. The table natives and the hash seed are held to the same floor
//! (B-2026-10-29-41).
#![cfg(feature = "llvm")]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-panic-free-floor-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Builds `src` sequentially, or `None` (after saying so) when the runtime
/// archive is missing and the run does not require it.
fn build(tag: &str, src: &str) -> Option<PathBuf> {
    let dir = scratch(tag);
    std::fs::write(dir.join("main.kara"), src).unwrap();
    let exe = dir.join("main.out");
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .env("KARAC_AUTO_PAR", "0")
        .env_remove("KARAC_RUNTIME")
        .env_remove("KARAC_FORCE_FULL_RUNTIME")
        .current_dir(&dir)
        .arg("build")
        .arg("main.kara")
        .arg("-o")
        .arg(&exe)
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    if !out.status.success()
        && err.contains("not found")
        && std::env::var_os("KARAC_REQUIRE_RUNTIME_ARCHIVE").is_none()
    {
        eprintln!("skipping: no runtime archive: {err}");
        return None;
    }
    assert!(out.status.success(), "{err}");
    Some(exe)
}

fn run(exe: &Path) -> Output {
    Command::new(exe).output().unwrap()
}

fn links_panic_hook(exe: &Path) -> bool {
    let bytes = std::fs::read(exe).unwrap();
    bytes.windows(14).any(|w| w == b"RUST_BACKTRACE")
}

#[test]
fn mir_print_and_vec_program_links_no_panic_hook() {
    let src = "fn main() {\n    let mut v: Vec[i64] = Vec.new();\n    for i in 0..1000 {\n        v.push(i * i);\n    }\n    let mut t = 0;\n    for x in v {\n        t += x;\n    }\n    println(t);\n    print(\"done\\n\");\n}\n";
    let Some(exe) = build("vec", src) else {
        return;
    };
    let out = run(&exe);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "332833500\ndone\n");
    assert!(!links_panic_hook(&exe), "std's panic hook is linked");
}

#[test]
fn mir_overflow_abort_reports_without_the_panic_hook() {
    let src = "fn bump(x: i64) -> i64 {\n    x * 4611686018427387904\n}\n\nfn main() {\n    println(\"before\");\n    println(bump(3));\n}\n";
    let Some(exe) = build("overflow", src) else {
        return;
    };
    let out = run(&exe);
    assert_eq!(out.status.code(), Some(101));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "before\n");
    // The interpreter's wording (`karac __mir-run`).
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "runtime error: integer overflow\n  at main.kara:2:5\nError return trace:\n  main.kara:2:5\n"
    );
    assert!(!links_panic_hook(&exe), "std's panic hook is linked");
}

#[test]
fn mir_formatted_panic_message_still_reports_in_full() {
    let src = "fn main() {\n    let n = 7;\n    println(\"start\");\n    if n > 3 {\n        panic(f\"too big: {n}\");\n    }\n}\n";
    let Some(exe) = build("panic", src) else {
        return;
    };
    let out = run(&exe);
    assert_eq!(out.status.code(), Some(101));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "start\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "runtime error: too big: 7\n  at main.kara:5:9\nError return trace:\n  main.kara:5:9\n"
    );
}

/// The table natives (`Map`, `Set`, `SortedMap`: insert, remove, lookup,
/// walk) and the hash seed reach no panic either (B-2026-10-29-41: kata 1's
/// hash_map was 358,680 B on Linux against 71,768 B for its brute force).
/// The expected line is a Python mirror's.
#[test]
fn mir_table_program_links_no_panic_hook() {
    let src = "fn main() {\n    let mut m: Map[i64, i64] = Map.new();\n    let mut s: Set[i64] = Set.new();\n    let mut sm: SortedMap[i64, i64] = SortedMap.new();\n    for i in 0..5000 {\n        m.insert(i % 997, i);\n        s.insert(i % 1013);\n        sm.insert(i % 89, i);\n    }\n    for i in 0..300 {\n        m.remove(i * 3);\n        s.remove(i * 2);\n    }\n    let mut total = 0;\n    for (k, v) in m {\n        total += k + v;\n    }\n    for x in s {\n        total += x;\n    }\n    for (k, v) in sm {\n        total += k * v;\n    }\n    println(f\"{total} {m.len()} {s.len()} {sm.len()}\");\n}\n";
    let Some(exe) = build("tables", src) else {
        return;
    };
    let out = run(&exe);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "23346940 697 713 89\n"
    );
    assert!(!links_panic_hook(&exe), "std's panic hook is linked");
}
