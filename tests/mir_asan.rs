//! The M2 ASan/LSan lane for the compiled MIR backend (`karac __mir-llvm-asan`).
//!
//! The first four tests prove the instrument reports: hand-written IR with a
//! leak, a double free and a use-after-free goes through
//! `codegen::mir_asan::compile_asan_object`, and each must be reported, while
//! the same IR without the fault runs clean. The use-after-free is the case the
//! legacy harness could not see (it linked the sanitizer without compiling the
//! code under it). The last test runs a Kāra program end to end.
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
            std::env::temp_dir().join(format!("karac-mir-asan-test-{}-{n}", std::process::id()));
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

/// Whether `cc -fsanitize=address` links and runs here, as the legacy ASan
/// suite probes it.
fn asan_available() -> bool {
    if std::env::var_os("KARAC_SKIP_ASAN_TESTS").is_some() {
        return false;
    }
    let dir = Scratch::new();
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

/// Instrument `ir`, link it, run it: the exit code and stderr, or `None` to skip.
fn run_ir(ir: &str) -> Option<(i32, String)> {
    if !asan_available() {
        eprintln!("skipping: no ASan-capable cc");
        return None;
    }
    let dir = Scratch::new();
    let obj = dir.path().join("main.o");
    let exe = dir.path().join("main");
    karac::codegen::mir_asan::compile_asan_object(ir, &obj).expect("instrument");
    common::link_or_skip(karac::codegen::link_executable_with_sanitizer(
        &obj.to_string_lossy(),
        &exe.to_string_lossy(),
        &["-fsanitize=address"],
    ))?;
    let out = Command::new(&exe)
        .env("ASAN_OPTIONS", "exitcode=86")
        .env("LSAN_OPTIONS", "exitcode=86")
        .output()
        .expect("run");
    Some((
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    ))
}

/// A `main` that allocates 32 bytes, writes them, and then runs `tail`.
fn program(tail: &str) -> String {
    format!(
        "declare ptr @malloc(i64)\n\
         declare void @free(ptr)\n\
         define i32 @main() {{\n\
         entry:\n\
         \x20 %p = call ptr @malloc(i64 32)\n\
         \x20 store volatile i64 7, ptr %p\n\
         {tail}\
         \x20 ret i32 0\n\
         }}\n"
    )
}

#[test]
fn mir_asan_clean_program_runs_clean() {
    let Some((code, err)) = run_ir(&program("  call void @free(ptr %p)\n")) else {
        return;
    };
    assert_eq!(code, 0, "clean IR reported:\n{err}");
}

#[test]
fn mir_asan_reports_a_leak() {
    let Some((code, err)) = run_ir(&program("")) else {
        return;
    };
    assert_eq!(code, 86, "leak not reported:\n{err}");
    assert!(
        err.contains("LeakSanitizer: detected memory leaks"),
        "{err}"
    );
}

#[test]
fn mir_asan_reports_a_double_free() {
    let tail = "  call void @free(ptr %p)\n  call void @free(ptr %p)\n";
    let Some((code, err)) = run_ir(&program(tail)) else {
        return;
    };
    assert_eq!(code, 86, "double free not reported:\n{err}");
    assert!(err.contains("attempting double-free"), "{err}");
}

#[test]
fn mir_asan_reports_a_read_through_a_freed_pointer() {
    let tail = "  call void @free(ptr %p)\n  %x = load volatile i64, ptr %p\n";
    let Some((code, err)) = run_ir(&program(tail)) else {
        return;
    };
    assert_eq!(code, 86, "use after free not reported:\n{err}");
    assert!(err.contains("heap-use-after-free"), "{err}");
}

#[test]
fn mir_asan_runs_a_kara_program() {
    if !asan_available() {
        eprintln!("skipping: no ASan-capable cc");
        return;
    }
    let dir = Scratch::new();
    let src = dir.path().join("main.kara");
    std::fs::write(
        &src,
        "fn main() {\n\
         \x20   let mut s = String.new();\n\
         \x20   s.push_str(\"a string long enough to live on the heap\");\n\
         \x20   let v: Vec[i64] = [1, 2, 3];\n\
         \x20   println(f\"{s} {v.len()}\");\n\
         }\n",
    )
    .expect("write source");
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("__mir-llvm-asan")
        .arg(&src)
        .output()
        .expect("run karac");
    let err = String::from_utf8_lossy(&out.stderr);
    if out.status.code() == Some(3) && err.contains("Linker failed") {
        // A stale archive panics here; a missing one skips.
        let _ = common::link_or_skip(Err(err.into_owned()));
        return;
    }
    assert_eq!(out.status.code(), Some(0), "stderr:\n{err}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "a string long enough to live on the heap 3\n"
    );
}

/// The partition sort of `sort_by` (kata 1665) copies elements between
/// the vector and its scratch and compares against a bitwise copy of the
/// pivot; with heap-owning `String` keys none of that may free or leak.
#[test]
fn mir_asan_partition_sort_of_strings_runs_clean() {
    if !asan_available() {
        eprintln!("skipping: no ASan-capable cc");
        return;
    }
    let dir = Scratch::new();
    let src = dir.path().join("main.kara");
    std::fs::write(
        &src,
        r#"fn main() {
    let mut v: Vec[(String, i64)] = Vec.new();
    let mut i = 0;
    while i < 6000 {
        v.push((f"key-{(i * 7 + 2) % 5}-with-padding-to-live-on-the-heap", i));
        i += 1;
    }
    v.sort_by(|a, b| a.0.cmp(b.0));
    let mut h = 0;
    for (s, x) in v.iter() {
        h = (h * 31 + x + s.len()) % 1000000007;
    }
    println(f"{v[0].0} {v[0].1} {v[5999].0} {v[5999].1} {h}");
}
"#,
    )
    .expect("write source");
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("__mir-llvm-asan")
        .arg(&src)
        .output()
        .expect("run karac");
    let err = String::from_utf8_lossy(&out.stderr);
    if out.status.code() == Some(3) && err.contains("Linker failed") {
        let _ = common::link_or_skip(Err(err.into_owned()));
        return;
    }
    assert_eq!(out.status.code(), Some(0), "stderr:\n{err}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "key-0-with-padding-to-live-on-the-heap 4 key-4-with-padding-to-live-on-the-heap 5996 575934588\n");
}

/// The lane's canary: a Kāra program that leaks a two-node `shared` cycle
/// must be REPORTED. `main` used to run on a hand-switched stack that
/// LeakSanitizer does not scan, and every leak verdict was clean
/// (B-2026-10-18-31).
#[test]
fn mir_asan_reports_a_leaked_shared_cycle() {
    if !asan_available() {
        eprintln!("skipping: no ASan-capable cc");
        return;
    }
    let dir = Scratch::new();
    let src = dir.path().join("main.kara");
    std::fs::write(
        &src,
        "shared struct Node {\n\
         \x20   val: i64,\n\
         \x20   mut next: Option[Node],\n\
         }\n\
         fn main() {\n\
         \x20   let a = Node { val: 1, next: None };\n\
         \x20   let b = Node { val: 2, next: Some(a) };\n\
         \x20   a.next = Some(b);\n\
         \x20   println(f\"{a.val}\");\n\
         }\n",
    )
    .expect("write source");
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("__mir-llvm-asan")
        .arg(&src)
        .env_remove("ASAN_OPTIONS")
        .env_remove("LSAN_OPTIONS")
        .output()
        .expect("run karac");
    let err = String::from_utf8_lossy(&out.stderr);
    if out.status.code() == Some(3) && err.contains("Linker failed") {
        let _ = common::link_or_skip(Err(err.into_owned()));
        return;
    }
    assert_eq!(String::from_utf8_lossy(&out.stdout), "1\n");
    assert_eq!(out.status.code(), Some(86), "leak not reported:\n{err}");
    assert!(
        err.contains("LeakSanitizer: detected memory leaks"),
        "{err}"
    );
}
