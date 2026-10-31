//! `Child.wait_with_output()` takes `own self`, so the compiled call is the
//! child handle's last use and frees it; nothing drops the binding after
//! (B-2026-10-31-26). The pin: the program runs clean under
//! AddressSanitizer, where the handle leaked 32 bytes.
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

const SRC: &str = r#"fn main() {
    let b = Command.new("printf").arg("x").stdout(Stdio.piped()).spawn().unwrap();
    let out = b.wait_with_output().unwrap();
    println(f"[{String.from_utf8(out.stdout).unwrap()}] {out.status.code().unwrap()}");
}
"#;

/// Whether `cc -fsanitize=address` links and runs here.
fn asan_available(dir: &std::path::Path) -> bool {
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

#[test]
fn mir_wait_with_output_frees_the_child_handle() {
    let d = std::env::temp_dir().join(format!("karac-wait-output-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    if !asan_available(&d) {
        eprintln!("skipping: no ASan-capable cc");
        return;
    }
    std::fs::write(d.join("main.kara"), SRC).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(&d)
        .env("ASAN_OPTIONS", "exitcode=86")
        .args(["__mir-llvm-asan", "main.kara"])
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    if out.status.code() == Some(3) && err.contains("Linker failed") {
        let _ = common::link_or_skip(Err(err.into_owned()));
        return;
    }
    assert_eq!(out.status.code(), Some(0), "stderr:\n{err}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "[x] 0\n");
    let _ = std::fs::remove_dir_all(&d);
}
