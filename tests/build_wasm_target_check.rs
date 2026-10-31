//! `karac build --target=wasm_wasi` checks the package for wasm whether or
//! not the binary has a backend (B-2026-10-31-36). Without the `llvm`
//! feature the target was never set, so the package check looked for the
//! host's platform module: a package with only a `_wasm` sibling failed
//! with `unknown module` on every host, and a package with a native
//! sibling passed only where that sibling existed (Linux, macOS), which is
//! how the bug showed on Windows CI alone.

use std::path::PathBuf;
use std::process::Command;

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-build-wasm-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    dir
}

#[test]
fn wasm_build_checks_a_package_whose_only_platform_module_is_wasm() {
    let dir = scratch("only-wasm");
    std::fs::write(dir.join("kara.toml"), "[package]\nname = \"demo\"\n").unwrap();
    std::fs::write(
        dir.join("src/main.kara"),
        "import host.{run};\nfn main() { run(); }\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/host_wasm.kara"), "pub fn run() {}\n").unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(&dir)
        .args(["build", "--target=wasm_wasi", "--output=json"])
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("E0112") && !stdout.contains("\"phase\":\"resolve\""),
        "the wasm sibling resolves `host`; stdout=\n{stdout}",
    );
    if out.status.success() {
        assert!(
            stdout.contains("\"path\":\"host\",\"role\":\"ordinary\",\"platform\":\"wasm\""),
            "wasm build should select host_wasm; stdout=\n{stdout}",
        );
    } else {
        assert!(
            stdout.contains("\"phase\":\"build\"") && !stdout.contains("typecheck"),
            "the only failure is past the check (the wasm toolchain); stdout=\n{stdout}",
        );
    }
}
