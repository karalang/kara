//! Four wasm_wasi miscompiles the probe found by running corpus/core under
//! node's WASI (B-2026-11-15-3, B-2026-11-15-4, B-2026-11-15-5, B-2026-11-15-6), each correct natively:
//! a `dyn` call read its method two pointers past the slot on wasm32's
//! 4-byte pointers and trapped; a `(ref A, ref B)` pair went to a closure
//! as LLVM `{ptr, ptr}`, its second half at 4 where the layout has 8; a
//! `std.log` call's size was `usize` in the runtime and `i64` in the
//! module, which wasm-ld refuses; and `CString.to_str` wrote its view's
//! length as a `usize`, into the pointer's upper half. Each program must
//! print on wasm what it prints natively.
//!
//! Skips (with a notice) when the wasm archive or a node with `node:wasi`
//! is missing; CI's `wasm-e2e` job has both.
#![cfg(feature = "llvm")]
use std::path::{Path, PathBuf};
use std::process::Command;

/// A scratch directory removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir =
            std::env::temp_dir().join(format!("karac-mir-wasm-rows-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Runs a preview1 module under node: argv[2] is the module.
const RUNNER: &str = "\
import { readFile } from 'node:fs/promises';
import { WASI } from 'node:wasi';
const wasi = new WASI({ version: 'preview1', args: [process.argv[2]], env: {}, returnOnExit: true });
const mod = await WebAssembly.compile(await readFile(process.argv[2]));
const inst = await WebAssembly.instantiate(mod, wasi.getImportObject());
process.exitCode = wasi.start(inst);
";

fn wasm_archive() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("target/release/libkarac_runtime_wasm.a")
}

/// Why this host cannot run the tests, if it cannot.
fn skip_reason() -> Option<String> {
    if !wasm_archive().exists() {
        return Some(format!(
            "{} is missing (see CLAUDE.md's archive recipe)",
            wasm_archive().display()
        ));
    }
    match Command::new("node").arg("--version").output() {
        Ok(o) if o.status.success() => None,
        _ => Some("node is not installed".into()),
    }
}

/// `src` built for wasm and run: (stdout, exit code). `None` when skipped.
fn build_and_run(name: &str, src: &str) -> Option<(String, i32)> {
    if let Some(why) = skip_reason() {
        eprintln!("skipping mir_wasm_probe_rows::{name}: {why}");
        return None;
    }
    let dir = Scratch::new(name);
    let file = dir.0.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let runner = dir.0.join("run.mjs");
    std::fs::write(&runner, RUNNER).unwrap();
    let wasm = dir.0.join("main.wasm");
    let karac = env!("CARGO_BIN_EXE_karac");
    let built = Command::new(karac)
        .args(["build", "--target=wasm_wasi"])
        .arg(&file)
        .arg("-o")
        .arg(&wasm)
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{name}: build failed: {}",
        String::from_utf8_lossy(&built.stderr)
    );
    let run = Command::new("node")
        .arg("--no-warnings")
        .arg("--no-turbo-fast-api-calls")
        .arg(&runner)
        .arg(&wasm)
        .output()
        .unwrap();
    Some((
        String::from_utf8(run.stdout).unwrap(),
        run.status.code().unwrap_or(-1),
    ))
}

/// The native mir-llvm JIT's stdout and exit code for `src`: the oracle.
fn native(name: &str, src: &str) -> (String, i32) {
    let dir = Scratch::new(&format!("{name}-native"));
    let file = dir.0.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["__mir-llvm-run"])
        .arg(&file)
        .output()
        .unwrap();
    (
        String::from_utf8(out.stdout).unwrap(),
        out.status.code().unwrap_or(-1),
    )
}

fn agrees(name: &str, src: &str, want: &str) {
    let (nat, code) = native(name, src);
    assert_eq!(nat, want, "{name}: native output");
    assert_eq!(code, 0, "{name}: native exit");
    if let Some((out, code)) = build_and_run(name, src) {
        assert_eq!(out, want, "{name}: wasm output");
        assert_eq!(code, 0, "{name}: wasm exit");
    }
}

#[test]
fn wasm_dyn_call_finds_its_slot() {
    agrees(
        "dyn-call",
        r#"trait Shape { fn area(self) -> f64; fn name(self) -> String; }
struct Circle { r: f64 }
impl Shape for Circle {
    fn area(self) -> f64 { 3.0 * self.r * self.r }
    fn name(self) -> String { "circle".to_string() }
}
struct Sq { s: f64 }
impl Shape for Sq {
    fn area(self) -> f64 { self.s * self.s }
    fn name(self) -> String { "square".to_string() }
}
fn main() {
    let c: dyn Shape = Circle { r: 1.0 };
    println(c.area());
    let shapes: Vec[dyn Shape] = [Circle { r: 2.0 }, Sq { s: 3.0 }];
    for s in shapes.iter() { println(f"{s.name()} {s.area()}"); }
}
"#,
        "3.0\ncircle 12.0\nsquare 9.0\n",
    );
}

#[test]
fn wasm_zip_then_map_reads_both_refs() {
    agrees(
        "zip-map",
        r#"fn main() {
    let a = [1, 2];
    let b = [3, 4];
    for s in a.iter().zip(b.iter()).map(|(x, y)| x + y) { println(s); }
    let v: Vec[i64] = a.iter().zip(b.iter()).map(|p| p.0 * p.1).collect();
    println(f"{v}");
}
"#,
        "4\n6\n[3, 8]\n",
    );
}

#[test]
fn wasm_log_call_links_and_prints() {
    agrees(
        "log",
        r#"import std.log;
fn main() {
    log.info("hello", n: 3);
    println("done");
}
"#,
        // The record goes to stderr on every lane; the build linking at
        // all is the pin.
        "done\n",
    );
}

#[test]
fn wasm_cstring_to_str_keeps_its_length() {
    agrees(
        "cstring",
        r#"fn main() {
    let cs = "abc".to_string().to_cstring().unwrap();
    match cs.to_str() { Ok(t) => println(f"[{t}] {t.len()}"), Err(_) => println("err") }
    println(cs.to_string().unwrap());
}
"#,
        "[abc] 3\nabc\n",
    );
}

/// A trap keeps what was printed before it (B-2026-11-15-23): stdout is
/// written line by line on wasm, since a trap (here node's stack limit)
/// ends the module where nothing can flush. It lost every line before.
#[test]
fn wasm_trap_keeps_the_lines_printed_before_it() {
    let src = r#"fn depth(n: i64) -> i64 { if n == 0 { 0 } else { 1 + depth(n - 1) } }
fn main() {
    println("one");
    println("two");
    println(depth(1000000000));
}
"#;
    if let Some((out, code)) = build_and_run("trap-flush", src) {
        assert_eq!(out, "one\ntwo\n", "wasm stdout before the trap");
        assert_ne!(code, 0, "the trap ends the run");
    }
}
