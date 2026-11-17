//! B-2026-11-16-324: `isize` and `usize` are the target's pointer width
//! (design.md §5), so 32 bits on wasm32, where they were 64. Each line
//! below prints its 32-bit value under `--target=wasm_wasi` and its 64-bit
//! one natively. MIR's own lengths and indices stay 64-bit words (`u64`)
//! on every target, so a `Vec` indexes the same everywhere.
//!
//! The wasm half skips (with a notice) when the wasm archive or node is
//! missing, as `tests/mir_wasm.rs` does.
#![cfg(feature = "llvm")]

use std::path::{Path, PathBuf};
use std::process::Command;

const SRC: &str = r#"fn xu(v: usize) -> usize { v }
fn xi(v: isize) -> isize { v }
fn xf(v: f64) -> f64 { v }
fn so[T: Display](o: Option[T]) -> String { match o { Some(v) => f"S{v}", None => "N".to_string() } }
fn main() {
    println(usize.MAX);
    println(isize.MAX);
    println(isize.MIN);
    println(xu(usize.MAX).wrapping_add(1));
    println(so(xu(usize.MAX).checked_add(1)));
    println(xi(isize.MAX).wrapping_add(1));
    println((-1i32) as usize);
    println(xu(4294967295) as i64);
    println(so(usize.checked_from(xf(4294967296.0))));
    println(xu(1).leading_zeros());
    let v: Vec[i64] = [10, 20, 30];
    println(v[2]);
    let b = xu(258).to_ne_bytes();
    println(b.len());
    println(usize.from_ne_bytes(b));
    println(isize.from_le_bytes(xi(-2).to_le_bytes()));
}
"#;

const WASM32: &str = "4294967295\n2147483647\n-2147483648\n0\nN\n-2147483648\n4294967295\n\
                      4294967295\nN\n31\n30\n4\n258\n-2\n";
const NATIVE: &str = "18446744073709551615\n9223372036854775807\n-9223372036854775808\n0\nN\n\
                      -9223372036854775808\n18446744073709551615\n4294967295\nS4294967296\n63\n30\n8\n258\n-2\n";

const RUNNER: &str = "\
import { readFile } from 'node:fs/promises';
import { WASI } from 'node:wasi';
const wasi = new WASI({ version: 'preview1', args: [process.argv[2]], env: {}, returnOnExit: true });
const mod = await WebAssembly.compile(await readFile(process.argv[2]));
const inst = await WebAssembly.instantiate(mod, wasi.getImportObject());
process.exitCode = wasi.start(inst);
";

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-wasm-pointer-width-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn native_usize_and_isize_are_64_bit() {
    let dir = scratch("native");
    let file = dir.join("main.kara");
    std::fs::write(&file, SRC).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("__mir-llvm-run")
        .arg(&file)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        NATIVE,
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn wasm32_usize_and_isize_are_32_bit() {
    let archive =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("target/release/libkarac_runtime_wasm.a");
    let node = Command::new("node").arg("--version").output();
    if !archive.exists() || !node.is_ok_and(|o| o.status.success()) {
        eprintln!("skipping wasm32_usize_and_isize_are_32_bit: no wasm archive or node");
        return;
    }
    let dir = scratch("wasm");
    let file = dir.join("main.kara");
    std::fs::write(&file, SRC).unwrap();
    std::fs::write(dir.join("run.mjs"), RUNNER).unwrap();
    let wasm = dir.join("main.wasm");
    let built = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["build", "--target=wasm_wasi"])
        .arg(&file)
        .arg("-o")
        .arg(&wasm)
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let run = Command::new("node")
        .arg("--no-warnings")
        .arg("--no-turbo-fast-api-calls")
        .arg(dir.join("run.mjs"))
        .arg(&wasm)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        WASM32,
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}
