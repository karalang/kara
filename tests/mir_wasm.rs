//! `karac build --target=wasm_wasi` (M3 item 4, B-2026-10-22-23): a
//! plain wasm32-wasip1 module built through MIR, linked against
//! `libkarac_runtime_wasm.a` and run under node's WASI. Each program must
//! print what it prints natively, byte for byte.
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
            std::env::temp_dir().join(format!("karac-mir-wasm-{name}-{}", std::process::id()));
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
        eprintln!("skipping mir_wasm::{name}: {why}");
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
fn wasm_hello_vec_and_format() {
    agrees(
        "hello",
        "fn main() {\n    let v = vec![1, 2, 3];\n    println(f\"hello {v.len()} {v[2]}\");\n}\n",
        "hello 3 3\n",
    );
}

/// `str` views written by the runtime (`karac_mir_str_view`) are two
/// 64-bit words on every target; written as `usize` they left the length
/// in the pointer's high half on wasm and every split piece read empty.
#[test]
fn wasm_split_pieces_and_parse() {
    agrees(
        "split",
        r#"fn main() {
    let log = "0:start:3";
    let parts: Vec[Str] = log.split(":").collect();
    println(f"{parts.len()} [{parts[0]}] [{parts[1]}] [{parts[2]}]");
    println(f"{i64.parse(parts[2])}");
    let s = log.slice(2, 7);
    println(f"<{s}> {s.len()} {log.find(":")}");
}
"#,
        "3 [0] [start] [3]\nOk(3)\n<start> 5 Some(1)\n",
    );
}

/// `Map` hashes keys through `karac_hash_bytes`, whose length is `usize`
/// on the C side: a 32-bit parameter on wasm, which the backend now
/// narrows (it was a link-time signature mismatch).
#[test]
fn wasm_map_of_strings() {
    agrees(
        "map",
        r#"fn main() {
    let mut m: Map[String, i64] = Map.new();
    for w in "a b a c b a".split(" ") {
        let k = w.to_string();
        let n = m.get(k).unwrap_or(0);
        m.insert(k, n + 1);
    }
    let n = m.len();
    let mut keys: Vec[String] = Vec.new();
    for (k, _) in m { keys.push(k.clone()); }
    keys.sort();
    let mut line = String.new();
    for k in keys { line.push_str(k); }
    println(f"{line} {n}");
}
"#,
        "abc 3\n",
    );
}

#[test]
fn wasm_structs_enums_and_closures() {
    agrees(
        "adt",
        r#"enum Shape { Sq(i64), Rect(i64, i64) }
struct Acc { total: i64 }
fn area(s: Shape) -> i64 {
    match s { Shape.Sq(a) => a * a, Shape.Rect(w, h) => w * h }
}
fn main() {
    let shapes = vec![Shape.Sq(3), Shape.Rect(2, 5)];
    let mut acc = Acc { total: 0 };
    for s in shapes { acc.total += area(s); }
    let k = 4;
    let add = |x: i64| x + k;
    println(f"{acc.total} {add(acc.total)}");
}
"#,
        "19 23\n",
    );
}

/// A panic reaches the WASI exit code, as natively.
#[test]
fn wasm_panic_exits_nonzero() {
    let src = "fn main() {\n    let v: Vec[i64] = Vec.new();\n    println(f\"{v[3]}\");\n}\n";
    let (_, native_code) = native("panic", src);
    assert_ne!(native_code, 0);
    if let Some((_, code)) = build_and_run("panic", src) {
        assert_eq!(code, native_code, "wasm exit code");
    }
}

#[test]
fn wasm_build_refuses_other_wasm_targets() {
    let dir = Scratch::new("refuse");
    let file = dir.0.join("main.kara");
    std::fs::write(&file, "fn main() {}\n").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["build", "--target=wasm_browser"])
        .arg(&file)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("`--target=wasm_browser`"), "{err}");
}

/// A package (`karac build` with no file, from its root) builds for wasm
/// too: one module from every file under `src/`, named after the package.
#[test]
fn wasm_package_build() {
    let dir = Scratch::new("package");
    let root = &dir.0;
    std::fs::create_dir_all(root.join("src/geo")).unwrap();
    std::fs::write(
        root.join("kara.toml"),
        "[package]\nname = \"geo_app\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/main.kara"),
        "import geo.shapes.{area, Sq};\nimport util.{twice};\nfn main() {\n    \
         let s = Sq { side: 3 };\n    println(f\"area {area(s)} twice {twice(21)}\");\n}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/util.kara"),
        "pub fn twice(x: i64) -> i64 { x * 2 }\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/geo/shapes.kara"),
        "pub struct Sq { pub side: i64 }\npub fn area(s: Sq) -> i64 { s.side * s.side }\n",
    )
    .unwrap();
    if let Some(why) = skip_reason() {
        eprintln!("skipping mir_wasm::wasm_package_build: {why}");
        return;
    }
    let built = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["build", "--target=wasm_wasi"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&built.stderr)
    );
    let wasm = root.join("geo_app.wasm");
    assert!(wasm.exists(), "no {}", wasm.display());
    let runner = root.join("run.mjs");
    std::fs::write(&runner, RUNNER).unwrap();
    let run = Command::new("node")
        .arg("--no-warnings")
        .arg(&runner)
        .arg(&wasm)
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&run.stdout), "area 9 twice 42\n");
    assert_eq!(run.status.code(), Some(0));
}
