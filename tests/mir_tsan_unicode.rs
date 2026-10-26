//! A ThreadSanitizer link of a program that calls `String.normalize(form)`
//! takes the TSan build of the unicode archive
//! (`libkarac_runtime_tsan_unicode.a`, CLAUDE.md § Runtime archives): the
//! plain TSan archive has no `karac_unicode_*`, and the plain unicode one is
//! not instrumented. Tasks normalizing on four workers run clean. A missing
//! archive skips, as for the other opt-in archives.
#![cfg(feature = "llvm")]

mod common;

use std::path::PathBuf;
use std::process::Command;

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Whether `cc -fsanitize=thread` links and runs here.
fn tsan_available(dir: &Scratch) -> bool {
    if std::env::var_os("KARAC_SKIP_TSAN_TESTS").is_some() {
        return false;
    }
    let c = dir.0.join("probe.c");
    let exe = dir.0.join("probe");
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

#[test]
fn mir_tsan_normalizing_tasks_link_the_tsan_unicode_archive() {
    let dir = Scratch(std::env::temp_dir().join(format!(
        "karac-mir-tsan-unicode-test-{}",
        std::process::id()
    )));
    std::fs::create_dir_all(&dir.0).expect("scratch dir");
    if !tsan_available(&dir) {
        eprintln!("skipping: no TSan-capable cc");
        return;
    }
    let src = dir.0.join("main.kara");
    std::fs::write(
        &src,
        r#"fn nfc_len(k: i64) -> i64 {
    let mut n = 0;
    for _ in 0..50 {
        let s = f"cafe\u{301}-{k}";
        n = n + s.normalize(NormalizationForm.Nfc).len();
    }
    n
}

fn main() {
    let g = TaskGroup.new();
    let mut hs = Vec.new();
    for k in 0..8 {
        hs.push(g.spawn(|| nfc_len(k)));
    }
    let mut t = 0;
    for h in hs {
        t = t + h.join();
    }
    println(t);
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
    let unlinked = err.contains("Linker failed") || err.contains("libkarac_runtime_tsan_unicode.a");
    if out.status.code() == Some(3) && unlinked {
        let _ = common::link_or_skip(Err(err.into_owned()));
        return;
    }
    assert_eq!(out.status.code(), Some(0), "stderr:\n{err}");
    // "café-k" is 7 bytes in NFC, 50 times over eight tasks.
    assert_eq!(String::from_utf8_lossy(&out.stdout), "2800\n");
}
