//! B-2026-11-16-312: a wasm_wasi build optimised and emitted the whole
//! module on one thread, as one unit, where a native build cuts a large
//! program into codegen units (`src/backend/mir_aot/split.rs`). The
//! 21,824-line mini_tldextract took 246 s and 11.4 GB for wasm against
//! 18 s and 1.5 GB natively; cut into units and merged by `wasm-ld -r`, it
//! takes about what the native build takes. This pins that the wasm build
//! is cut, and that the cut module runs as the corpus expects.
//!
//! Skips (with a notice) when the wasm archive or node is missing.
#![cfg(feature = "llvm")]

use std::path::Path;
use std::process::Command;

const RUNNER: &str = "\
import { readFile } from 'node:fs/promises';
import { WASI } from 'node:wasi';
const wasi = new WASI({ version: 'preview1', args: [process.argv[2]], env: {}, preopens: { '.': '.' }, returnOnExit: true });
const mod = await WebAssembly.compile(await readFile(process.argv[2]));
const inst = await WebAssembly.instantiate(mod, wasi.getImportObject());
process.exitCode = wasi.start(inst);
";

#[test]
fn wasm_build_of_a_large_program_is_cut_into_units() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let archive = root.join("target/release/libkarac_runtime_wasm.a");
    let node = Command::new("node").arg("--version").output();
    if !archive.exists() || !node.is_ok_and(|o| o.status.success()) {
        eprintln!(
            "skipping wasm_build_of_a_large_program_is_cut_into_units: no wasm archive or node"
        );
        return;
    }
    let entry = root.join("corpus/apps/cli/mini_tldextract");
    let dir = std::env::temp_dir().join(format!("karac-wasm-split-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let wasm = dir.join("main.wasm");
    let built = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["build", "--target=wasm_wasi"])
        .arg(entry.join("source.kara"))
        .arg("-o")
        .arg(&wasm)
        .env("KARAC_MIR_TIME", "1")
        .env_remove("KARAC_MIR_CODEGEN_UNITS")
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&built.stderr);
    assert!(built.status.success(), "{err}");
    assert!(
        err.lines()
            .any(|l| l.contains("opt + emission,") && l.contains(" units")),
        "the wasm build was not cut into units:\n{}",
        err.lines()
            .filter(|l| l.starts_with("[mir-time]"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    std::fs::write(dir.join("run.mjs"), RUNNER).unwrap();
    let run = Command::new("node")
        .args([
            "--no-warnings",
            "--no-turbo-fast-api-calls",
            "run.mjs",
            "main.wasm",
        ])
        .current_dir(&dir)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let want = std::fs::read_to_string(entry.join("expected.out")).unwrap();
    assert!(
        String::from_utf8_lossy(&run.stdout) == want,
        "the cut module's output differs from expected.out: {}",
        String::from_utf8_lossy(&run.stderr)
    );
}
