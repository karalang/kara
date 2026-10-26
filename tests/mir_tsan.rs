//! The M4 ThreadSanitizer lane for the compiled MIR backend
//! (`karac __mir-llvm-tsan`, item 11), which the pool (on by default,
//! `KARAC_THREADS` workers) must be clean under.
//!
//! The first two tests prove the instrument reports: hand-written IR in
//! which two threads write one global goes through
//! `backend::mir_asan::compile_tsan_object` and must be reported, while
//! the same writes ordered by the join run clean. The last runs a Kāra
//! program's tasks on four workers end to end, against the TSan-built
//! runtime archive (`libkarac_runtime_tsan.a`, CLAUDE.md § Runtime
//! archives); a missing archive skips, as for the other opt-in archives.
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A scratch directory removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Scratch {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("karac-mir-tsan-test-{}-{n}", std::process::id()));
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

/// Whether `cc -fsanitize=thread` links and runs here.
fn tsan_available() -> bool {
    if std::env::var_os("KARAC_SKIP_TSAN_TESTS").is_some() {
        return false;
    }
    let dir = Scratch::new();
    let c = dir.path().join("probe.c");
    let exe = dir.path().join("probe");
    std::fs::write(&c, "int main(void){return 0;}\n").expect("write probe");
    Command::new("cc")
        .arg("-fsanitize=thread")
        .arg(&c)
        .arg("-o")
        .arg(&exe)
        .status()
        .is_ok_and(|s| s.success())
        && Command::new(&exe).status().is_ok_and(|s| s.success())
}

/// Instrument `ir`, link it, run it: the exit code and stderr, or `None` to skip.
fn run_ir(ir: &str) -> Option<(i32, String)> {
    if !tsan_available() {
        eprintln!("skipping: no TSan-capable cc");
        return None;
    }
    let dir = Scratch::new();
    let obj = dir.path().join("main.o");
    let exe = dir.path().join("main");
    karac::backend::mir_asan::compile_tsan_object(ir, &obj).expect("instrument");
    common::link_or_skip(karac::backend::driver::link_executable_with_sanitizer(
        &obj.to_string_lossy(),
        &exe.to_string_lossy(),
        &["-fsanitize=thread"],
    ))?;
    let out = Command::new(&exe)
        .env("TSAN_OPTIONS", "exitcode=86")
        .output()
        .expect("run");
    Some((
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    ))
}

/// A `main` that starts a thread writing `@x`, runs `before` and joins it,
/// then runs `after`.
fn program(before: &str, after: &str) -> String {
    format!(
        "@x = global i64 0\n\
         declare i32 @pthread_create(ptr, ptr, ptr, ptr)\n\
         declare i32 @pthread_join(i64, ptr)\n\
         define ptr @bump(ptr %a) {{\n\
         entry:\n\
         \x20 %v = load i64, ptr @x\n\
         \x20 %w = add i64 %v, 1\n\
         \x20 store i64 %w, ptr @x\n\
         \x20 ret ptr null\n\
         }}\n\
         define i32 @main() {{\n\
         entry:\n\
         \x20 %t = alloca i64\n\
         \x20 %r = call i32 @pthread_create(ptr %t, ptr null, ptr @bump, ptr null)\n\
         {before}\
         \x20 %h = load i64, ptr %t\n\
         \x20 %j = call i32 @pthread_join(i64 %h, ptr null)\n\
         {after}\
         \x20 ret i32 0\n\
         }}\n"
    )
}

#[test]
fn mir_tsan_reports_two_threads_writing_one_global() {
    let Some((code, err)) = run_ir(&program("  %b = call ptr @bump(ptr null)\n", "")) else {
        return;
    };
    assert_eq!(code, 86, "race not reported:\n{err}");
    assert!(err.contains("ThreadSanitizer: data race"), "{err}");
}

#[test]
fn mir_tsan_writes_ordered_by_a_join_run_clean() {
    let Some((code, err)) = run_ir(&program("", "  %b = call ptr @bump(ptr null)\n")) else {
        return;
    };
    assert_eq!(code, 0, "ordered writes reported:\n{err}");
}

/// Spawned tasks and `par` branches, each allocating, on four workers: the
/// pool's handoffs (its lock, its queues, the join) order every access.
#[test]
fn mir_tsan_runs_a_kara_programs_tasks_on_four_workers() {
    if !tsan_available() {
        eprintln!("skipping: no TSan-capable cc");
        return;
    }
    let dir = Scratch::new();
    let src = dir.path().join("main.kara");
    std::fs::write(
        &src,
        r#"fn sq(x: i64) -> i64 {
    let mut v: Vec[i64] = Vec.new();
    for i in 0..1000 {
        v.push(i * x);
    }
    v.len() * x
}

fn main() {
    let g = TaskGroup.new();
    let mut hs = Vec.new();
    for k in 0..16 {
        hs.push(g.spawn(|| sq(k)));
    }
    let mut t = 0;
    for h in hs {
        t = t + h.join();
    }
    let (a, b) = par { sq(3), sq(4) };
    println(f"{t} {a + b}");
}
"#,
    )
    .expect("write source");
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("__mir-llvm-tsan")
        .arg(&src)
        .env("KARAC_THREADS", "4")
        .output()
        .expect("run karac");
    let err = String::from_utf8_lossy(&out.stderr);
    let unlinked = err.contains("Linker failed") || err.contains("libkarac_runtime_tsan.a");
    if out.status.code() == Some(3) && unlinked {
        // A stale archive panics here; a missing one skips.
        let _ = common::link_or_skip(Err(err.into_owned()));
        return;
    }
    assert_eq!(out.status.code(), Some(0), "stderr:\n{err}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "120000 7000\n");
}
