//! B-2026-11-16-310: on wasm32 a capacity a program asks for was narrowed
//! to 32 bits rather than reported. `w.try_reserve(4294967297)` returned
//! `Ok` with capacity 4, `Vec.with_capacity(4294967297)` and
//! `Vec.filled(4294967297, 0)` reported a length of 2^32 + 1 over a buffer
//! of a few bytes, and `String.try_with_capacity(i64.MAX)` reported
//! `OutOfMemory` of 4294967295 bytes. The ruling: every point where a
//! count becomes a `usize` checks it, and a capacity no buffer can hold is
//! `CapacityOverflow` in a fallible twin and a "capacity overflow" panic
//! in the others, as natively. On wasm32 a buffer holds at most
//! `isize::MAX` bytes.
//!
//! Natively a reservation whose byte count passed `2^64` wrapped the same
//! way (`reserve(1 << 62)` on an empty `Vec[i64]` gave capacity 2^62 over
//! a one-byte buffer); it now panics too.
//!
//! The wasm half skips (with a notice) when the wasm archive or node is
//! missing, as `tests/mir_wasm.rs` does.
#![cfg(feature = "llvm")]

use std::path::{Path, PathBuf};
use std::process::Command;

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir =
            std::env::temp_dir().join(format!("karac-wasm-capacity-{name}-{}", std::process::id()));
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

/// `body` as `main`'s body, built for wasm and run: (stdout, stderr).
/// `None` when skipped.
fn wasm(name: &str, body: &str) -> Option<(String, String)> {
    if let Some(why) = skip_reason() {
        eprintln!("skipping mir_wasm_capacity::{name}: {why}");
        return None;
    }
    let dir = Scratch::new(name);
    let file = dir.0.join("main.kara");
    std::fs::write(&file, format!("fn main() {{\n{body}\n}}\n")).unwrap();
    let runner = dir.0.join("run.mjs");
    std::fs::write(&runner, RUNNER).unwrap();
    let out = dir.0.join("main.wasm");
    let built = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["build", "--target=wasm_wasi"])
        .arg(&file)
        .arg("-o")
        .arg(&out)
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
        .arg(&out)
        .output()
        .unwrap();
    Some((
        String::from_utf8_lossy(&run.stdout).to_string(),
        String::from_utf8_lossy(&run.stderr).to_string(),
    ))
}

/// `body` as `main`'s body through the native backend: (stdout, stderr).
fn native(name: &str, body: &str) -> (String, String) {
    let dir = Scratch::new(&format!("{name}-native"));
    let file = dir.0.join("main.kara");
    std::fs::write(&file, format!("fn main() {{\n{body}\n}}\n")).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("__mir-llvm-run")
        .arg(&file)
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

/// Each fallible twin asked for a capacity wasm32 cannot hold.
const TWINS: &str = r#"    let mut w: Vec[i64] = Vec.new();
    match w.try_reserve(4294967297) { Ok(_) => println(f"reserve ok {w.capacity()}"), Err(e) => println(f"reserve {e}") }
    match w.try_reserve_exact(4294967297) { Ok(_) => println(f"exact ok {w.capacity()}"), Err(e) => println(f"exact {e}") }
    match w.try_reserve(1 << 58) { Ok(_) => println(f"huge ok {w.capacity()}"), Err(e) => println(f"huge {e}") }
    match Vec[i64].try_with_capacity(4294967297) { Ok(v) => println(f"vec ok {v.capacity()}"), Err(e) => println(f"vec {e}") }
    match String.try_with_capacity(9223372036854775807) { Ok(_) => println("string ok"), Err(e) => println(f"string {e}") }
    let mut s = "ab".to_string();
    match s.try_reserve(4294967297) { Ok(_) => println("sreserve ok"), Err(e) => println(f"sreserve {e}") }
    match VecDeque[i64].try_with_capacity(4294967297) { Ok(_) => println("deque ok"), Err(e) => println(f"deque {e}") }
    println(w.len());"#;

#[test]
fn wasm_fallible_twins_report_capacity_overflow() {
    let Some((out, err)) = wasm("twins", TWINS) else {
        return;
    };
    assert_eq!(
        out,
        "reserve capacity overflow\nexact capacity overflow\nhuge capacity overflow\n\
         vec capacity overflow\nstring capacity overflow\nsreserve capacity overflow\n\
         deque capacity overflow\n0\n",
        "{err}"
    );
}

#[test]
fn native_fallible_twins_overflow_only_past_the_ceiling() {
    let (out, err) = native("twins", TWINS);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 8, "{out}{err}");
    assert_eq!(lines[2], "huge capacity overflow", "{out}");
    assert_eq!(lines[4], "string capacity overflow", "{out}");
    assert_eq!(lines[7], "0", "{out}");
}

/// A count past `u32::MAX` given to a replacement limit means "every
/// match", as natively, not its low 32 bits.
#[test]
fn wasm_replacen_limit_saturates() {
    let body = r#"    println("aaa".replacen("a", "b", 4294967296));"#;
    let (nat, _) = native("replacen", body);
    assert_eq!(nat, "bbb\n");
    if let Some((out, err)) = wasm("replacen", body) {
        assert_eq!(out, "bbb\n", "{err}");
    }
}

/// Each panicking method asked for a capacity wasm32 cannot hold panics
/// with "capacity overflow" before printing anything.
#[test]
fn wasm_panicking_capacities_panic() {
    for (name, body) in [
        (
            "reserve",
            "    let mut w: Vec[i64] = Vec.new();\n    w.reserve(4294967297);\n    println(w.capacity());",
        ),
        (
            "reserve_exact",
            "    let mut w: Vec[i64] = Vec.new();\n    w.push(1);\n    w.reserve_exact(4294967297);\n    println(w.capacity());",
        ),
        (
            "with_capacity",
            "    let w: Vec[i64] = Vec.with_capacity(4294967297);\n    println(w.capacity());",
        ),
        (
            "filled",
            "    let v = Vec.filled(4294967297, 0);\n    println(v.len());",
        ),
        (
            "resize",
            "    let mut v: Vec[i64] = Vec.new();\n    v.resize(4294967297, 0);\n    println(v.len());",
        ),
        (
            "string_with_capacity",
            "    let mut s: String = String.with_capacity(4294967297);\n    s.push_str(\"x\");\n    println(s.len());",
        ),
    ] {
        let Some((out, err)) = wasm(name, body) else {
            return;
        };
        assert_eq!(out, "", "{name}: {err}");
        assert!(err.contains("capacity overflow"), "{name}: {err}");
    }
}

/// Natively a byte count past 2^64 wrapped in `reserve`, leaving a huge
/// capacity over a tiny buffer.
#[test]
fn native_reserve_past_the_word_panics() {
    for (name, body) in [
        (
            "reserve_wrap",
            "    let mut w: Vec[i64] = Vec.new();\n    w.reserve(1 << 62);\n    println(w.capacity());",
        ),
        (
            "reserve_wrap_some",
            "    let mut w: Vec[i64] = Vec.new();\n    w.push(1);\n    w.reserve(1 << 62);\n    println(w.capacity());",
        ),
    ] {
        let (out, err) = native(name, body);
        assert_eq!(out, "", "{name}: {err}");
        assert!(err.contains("capacity overflow"), "{name}: {err}");
    }
}
