//! B-2026-11-19-44: a `wasm_wasi` module's linear-memory shadow stack was
//! rustc's 1 MiB, so a recursion that spills a frame per level (an array
//! local, say) trapped with "memory access out of bounds" thousands of levels
//! short of where it runs natively; six katas did. The default is now 8 MiB,
//! and `karac build --wasm-stack-size=<bytes>` sets it (design.md's wasm
//! note says which stack bounds recursion and how to raise each).
//!
//! The wasm cells skip with a notice when the wasm archive or node is missing.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// Each level keeps a 64-element array that the next level reads through a
/// reference, so every frame lives on the shadow stack: about 600 bytes a
/// level, 3 MiB at 5000 levels.
fn deep(levels: u32) -> String {
    format!(
        "fn sum_down(n: i64, prev: ref Array[i64, 64]) -> i64 {{
    if n == 0 {{
        return prev[63];
    }}
    let mut here = [0; 64];
    let mut i = 0;
    while i < 64 {{
        here[i] = prev[i] + n;
        i = i + 1;
    }}
    sum_down(n - 1, here) + 1
}}

fn main() {{
    let start = [0; 64];
    println(sum_down({levels}, start));
}}
"
    )
}

fn scratch(src: &str) -> (PathBuf, String) {
    let dir = std::env::temp_dir().join(format!(
        "karac-wasm-stack-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    let p = f.to_str().unwrap().to_string();
    (dir, p)
}

#[test]
fn wasm_stack_size_refuses_a_bad_value() {
    let (dir, p) = scratch(&deep(1));
    for (value, says) in [
        ("17", "from 65536 (64 KiB)"),
        ("2147483648", "to 1073741824 (1 GiB)"),
        ("1048577", "multiple of 16"),
        ("8M", "number of bytes"),
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_karac"))
            .args(["build", &format!("--wasm-stack-size={value}"), &p])
            .output()
            .expect("karac");
        let err = String::from_utf8_lossy(&out.stderr).into_owned();
        assert_eq!(out.status.code(), Some(1), "{value}: {err}");
        assert!(err.contains(says), "{value}: {err}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "llvm")]
mod wasm {
    use super::*;

    /// Runs a preview1 module under node: argv[2] is the module.
    const RUNNER: &str = "\
import { readFile } from 'node:fs/promises';
import { WASI } from 'node:wasi';
const wasi = new WASI({ version: 'preview1', args: [process.argv[2]], env: {}, returnOnExit: true });
const mod = await WebAssembly.compile(await readFile(process.argv[2]));
const inst = await WebAssembly.instantiate(mod, wasi.getImportObject());
process.exitCode = wasi.start(inst);
";

    fn skip_reason() -> Option<String> {
        let archive = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/release/libkarac_runtime_wasm.a");
        if !archive.exists() {
            return Some(format!("{} is missing", archive.display()));
        }
        match Command::new("node").arg("--version").output() {
            Ok(o) if o.status.success() => None,
            _ => Some("node is not installed".into()),
        }
    }

    /// Builds `levels` deep for `wasm_wasi` with `flags`, runs it under node
    /// with `node_flags`: (success, stdout, stderr).
    fn run(levels: u32, flags: &[&str], node_flags: &[&str]) -> (bool, String, String) {
        let (dir, p) = scratch(&deep(levels));
        let wasm = dir.join("main.wasm");
        let mut args = vec!["build", "--target=wasm_wasi"];
        args.extend_from_slice(flags);
        args.extend_from_slice(&[&p, "-o", wasm.to_str().unwrap()]);
        let built = Command::new(env!("CARGO_BIN_EXE_karac"))
            .args(&args)
            .output()
            .expect("karac");
        assert!(
            built.status.success(),
            "{}",
            String::from_utf8_lossy(&built.stderr)
        );
        let runner = dir.join("runner.mjs");
        std::fs::write(&runner, RUNNER).unwrap();
        let out = Command::new("node")
            .arg("--no-turbo-fast-api-calls")
            .args(node_flags)
            .arg(&runner)
            .arg(&wasm)
            .output()
            .expect("node");
        let _ = std::fs::remove_dir_all(&dir);
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    #[test]
    fn wasm_default_stack_holds_a_deep_spilling_recursion() {
        if let Some(why) = skip_reason() {
            eprintln!("skipping: {why}");
            return;
        }
        let (ok, out, err) = run(5000, &[], &[]);
        assert!(ok, "{err}");
        assert_eq!(out, "12507500\n");
        // The old 1 MiB stack, asked for by the flag, still traps: the flag
        // reaches the link, and the default is what lets it pass.
        let (ok, _, err) = run(5000, &["--wasm-stack-size=1048576"], &[]);
        assert!(!ok);
        assert!(err.contains("memory access out of bounds"), "{err}");
    }

    #[test]
    fn wasm_stack_size_flag_raises_the_bound() {
        if let Some(why) = skip_reason() {
            eprintln!("skipping: {why}");
            return;
        }
        // 40000 levels need about 24 MiB of shadow stack, and more host
        // stack than node's default, so both are raised.
        let (ok, _, err) = run(40000, &[], &["--stack-size=7000"]);
        assert!(!ok);
        assert!(err.contains("memory access out of bounds"), "{err}");
        let (ok, out, err) = run(
            40000,
            &["--wasm-stack-size=67108864"],
            &["--stack-size=7000"],
        );
        assert!(ok, "{err}");
        assert_eq!(out, "800060000\n");
    }
}
