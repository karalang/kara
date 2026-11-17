//! B-2026-11-17-64: on `wasm_wasi` the default hasher's key is random per
//! process, from WASI's `random_get`, and `KARAC_HASH_SEED` pins it as it does
//! natively (collections.md § Hashing, stability policy). The wasm archives
//! build `karac-hash` without `std`, which used to give them a fixed key and
//! no override: every run iterated a `Map` in one order, and a pinned run
//! reproduced nothing.
//!
//! Skips (with a notice) when the wasm archive or node is missing, as
//! `tests/mir_wasm.rs` does.
#![cfg(feature = "llvm")]

use std::path::{Path, PathBuf};
use std::process::Command;

/// A preview1 runner that hands the module `KARAC_HASH_SEED` when the test
/// sets it, and nothing else of the environment.
const RUNNER: &str = "\
import { readFile } from 'node:fs/promises';
import { WASI } from 'node:wasi';
const env = process.env.KARAC_HASH_SEED === undefined ? {} : { KARAC_HASH_SEED: process.env.KARAC_HASH_SEED };
const wasi = new WASI({ version: 'preview1', args: [process.argv[2]], env, returnOnExit: true });
const mod = await WebAssembly.compile(await readFile(process.argv[2]));
const inst = await WebAssembly.instantiate(mod, wasi.getImportObject());
process.exitCode = wasi.start(inst);
";

/// Sixteen keys in a default-hashed `Map`, printed in iteration order.
const PROGRAM: &str = "\
fn main() {
    let mut m: Map[String, i64] = Map.new();
    let mut i = 0;
    while i < 16 {
        m.insert(f\"k{i}\", i);
        i = i + 1;
    }
    let mut line = String.new();
    for (k, _) in m {
        line.push_str(f\"{k} \");
    }
    println(line);
}
";

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn wasm_archive() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("target/release/libkarac_runtime_wasm.a")
}

/// The scratch directory holding the built module and the runner, or `None`
/// when this host cannot run wasm.
fn built(name: &str) -> Option<Scratch> {
    if !wasm_archive().exists() {
        eprintln!(
            "skipping {name}: {} is missing (see CLAUDE.md's archive recipe)",
            wasm_archive().display()
        );
        return None;
    }
    if !Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        eprintln!("skipping {name}: node is not installed");
        return None;
    }
    let dir = std::env::temp_dir().join(format!("karac-wasm-seed-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = Scratch(dir);
    std::fs::write(dir.0.join("main.kara"), PROGRAM).unwrap();
    std::fs::write(dir.0.join("run.mjs"), RUNNER).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["build", "--target=wasm_wasi"])
        .arg(dir.0.join("main.kara"))
        .arg("-o")
        .arg(dir.0.join("main.wasm"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{name}: build failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(dir)
}

/// One run of the module, with `KARAC_HASH_SEED` set to `seed` if given.
fn run(dir: &Scratch, seed: Option<&str>) -> String {
    let mut cmd = Command::new("node");
    cmd.arg("--no-warnings")
        .arg("--no-turbo-fast-api-calls")
        .arg(dir.0.join("run.mjs"))
        .arg(dir.0.join("main.wasm"))
        .env_remove("KARAC_HASH_SEED");
    if let Some(s) = seed {
        cmd.env("KARAC_HASH_SEED", s);
    }
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "wasm run failed");
    String::from_utf8(out.stdout).unwrap()
}

/// The native JIT's line for the same program under the same pin.
fn native(seed: &str) -> String {
    let dir = std::env::temp_dir().join(format!("karac-wasm-seed-native-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let dir = Scratch(dir);
    std::fs::write(dir.0.join("main.kara"), PROGRAM).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("__mir-llvm-run")
        .arg(dir.0.join("main.kara"))
        .env("KARAC_HASH_SEED", seed)
        .output()
        .unwrap();
    assert!(out.status.success(), "native run failed");
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn wasm_default_hasher_key_differs_between_runs() {
    let Some(dir) = built("random") else {
        return;
    };
    // Sixteen keys have 16! orders; five runs that all agree would mean the
    // key is fixed.
    let runs: Vec<String> = (0..5).map(|_| run(&dir, None)).collect();
    assert!(
        runs.iter().any(|r| r != &runs[0]),
        "five unpinned wasm runs iterated in one order: {:?}",
        runs[0]
    );
}

#[test]
fn wasm_hash_seed_pins_the_order_native_pins() {
    let Some(dir) = built("pinned") else {
        return;
    };
    let one = run(&dir, Some("1"));
    assert_eq!(
        one,
        run(&dir, Some("1")),
        "a pinned wasm run is reproducible"
    );
    assert_eq!(one, native("1"), "the pin gives wasm the native order");
    assert_eq!(run(&dir, Some("2")), native("2"));
    assert_ne!(one, run(&dir, Some("2")), "two pins, two orders");
}

/// B-2026-11-16-288 on `wasm_wasi`: a `KARAC_HASH_SEED` that is not a seed
/// warns on stderr, as it does natively, and the run goes on with a random
/// key.
#[test]
fn wasm_malformed_hash_seed_warns() {
    let Some(dir) = built("malformed") else {
        return;
    };
    let out = Command::new("node")
        .arg("--no-warnings")
        .arg("--no-turbo-fast-api-calls")
        .arg(dir.0.join("run.mjs"))
        .arg(dir.0.join("main.wasm"))
        .env("KARAC_HASH_SEED", "abc")
        .output()
        .unwrap();
    assert!(out.status.success(), "wasm run failed");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("warning: KARAC_HASH_SEED=`abc` is not a seed"),
        "{stderr}"
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).split(' ').count(), 17);
}
