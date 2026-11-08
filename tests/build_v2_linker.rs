//! `karac build` with no C linker on PATH names the program it could not
//! run and what to install, as a link error (B-2026-11-07-50).
#![cfg(feature = "llvm")]

use std::process::Command;

#[test]
fn build_v2_names_a_missing_linker() {
    let dir = std::env::temp_dir().join(format!("karac-build-v2-linker-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let empty = dir.join("bin");
    std::fs::create_dir_all(&empty).unwrap();
    let src = dir.join("hello.kara");
    std::fs::write(&src, "fn main() {\n    println(\"hi\");\n}\n").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("build")
        .arg(&src)
        .arg("-o")
        .arg(dir.join("hello"))
        .env("PATH", &empty)
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&dir);
    if err.contains("runtime archive") || err.contains("libkarac_runtime") {
        return; // No archive in this checkout: the link is never reached.
    }
    assert!(!out.status.success(), "the build must fail: {err}");
    assert!(
        err.contains(
            "error[link]: karac build needs a C linker: could not run `cc` (not found on PATH)"
        ) && err.contains("build-essential"),
        "{err}"
    );
}
