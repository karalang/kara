//! `String.normalize` and `std.regex` on `--target=wasm_wasi`
//! (B-2026-11-13-1): both are pure computation, so a wasm build links the
//! opt-in wasm archive its symbols name (`libkarac_runtime_wasm_unicode.a`,
//! `libkarac_runtime_wasm_regex.a`), as a native build links the native
//! ones, and prints what the native build prints. Before, `karac check
//! --targets=wasm_wasi` passed and the build died in wasm-ld on
//! `undefined symbol: karac_unicode_normalize_mir` / `karac_regex_mir`.
//!
//! Skips (with a notice) when the archive or a node with `node:wasi` is
//! missing.
#![cfg(feature = "llvm")]

use std::path::{Path, PathBuf};
use std::process::Command;

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "karac-mir-wasm-opt-in-{name}-{}",
            std::process::id()
        ));
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

fn skip_reason(archive: &str) -> Option<String> {
    for a in ["libkarac_runtime_wasm.a", archive] {
        let p = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/release")
            .join(a);
        if !p.exists() {
            return Some(format!(
                "{} is missing (see CLAUDE.md's archive recipe)",
                p.display()
            ));
        }
    }
    match Command::new("node").arg("--version").output() {
        Ok(o) if o.status.success() => None,
        _ => Some("node is not installed".into()),
    }
}

/// `src` built for wasm and run under node: its stdout. `None` when skipped.
fn wasm(name: &str, archive: &str, src: &str) -> Option<String> {
    if let Some(why) = skip_reason(archive) {
        eprintln!("skipping mir_wasm_opt_in::{name}: {why}");
        return None;
    }
    let dir = Scratch::new(name);
    let file = dir.0.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let runner = dir.0.join("run.mjs");
    std::fs::write(&runner, RUNNER).unwrap();
    let out = dir.0.join("main.wasm");
    let karac = env!("CARGO_BIN_EXE_karac");
    let checked = Command::new(karac)
        .args(["check", "--targets=wasm_wasi"])
        .arg(&file)
        .output()
        .unwrap();
    assert!(
        checked.status.success(),
        "{name}: check failed: {}",
        String::from_utf8_lossy(&checked.stderr)
    );
    let built = Command::new(karac)
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
    assert!(
        run.status.success(),
        "{name}: run failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    Some(String::from_utf8(run.stdout).unwrap())
}

/// The native mir-llvm JIT's stdout: the oracle.
fn native(name: &str, src: &str) -> String {
    let dir = Scratch::new(&format!("{name}-native"));
    let file = dir.0.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["__mir-llvm-run"])
        .arg(&file)
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap()
}

const NORMALIZE: &str = r#"fn show(s: Str) -> String {
    let mut out = String.new();
    for c in s.chars() {
        out.push_str(f"{c as u32} ");
    }
    out
}
fn main() {
    let a = "caf\u{e9}";
    let b = "cafe\u{301}";
    println(f"{a == b}");
    println(f"{a.normalize(NormalizationForm.Nfc) == b.normalize(NormalizationForm.Nfc)}");
    println(f"{a.normalize(NormalizationForm.Nfd).len()} {b.normalize(NormalizationForm.Nfc).len()}");
    println(show("\u{fb01}x".normalize(NormalizationForm.Nfkc).as_str()));
    println(show("\u{e9}".normalize(NormalizationForm.Nfd).as_str()));
}
"#;

const WANT_NORMALIZE: &str = "false\ntrue\n6 5\n102 105 120 \n101 769 \n";

#[test]
fn wasm_normalize_links_the_wasm_unicode_archive() {
    assert_eq!(native("nfc", NORMALIZE), WANT_NORMALIZE, "native");
    if let Some(out) = wasm("nfc", "libkarac_runtime_wasm_unicode.a", NORMALIZE) {
        assert_eq!(out, WANT_NORMALIZE, "wasm");
    }
}

const REGEX: &str = r#"import std.regex.{Match, Regex};

fn main() {
    let re = Regex.compile("a+b").unwrap();
    println(f"{re.is_match("xxaaab")} {re.is_match("ba")}");
    match re.find("zzaab ab") {
        Some(m) => println(f"{m.text} {m.start}"),
        None => println("none"),
    }
    let all: Vec[Match] = re.find_all("ab aab aaab");
    println(all.len());
    match Regex.compile("(") {
        Ok(_) => println("compiled"),
        Err(_) => println("bad pattern"),
    }
}
"#;

const WANT_REGEX: &str = "true false\naab 2\n3\nbad pattern\n";

#[test]
fn wasm_regex_links_the_wasm_regex_archive() {
    assert_eq!(native("rx", REGEX), WANT_REGEX, "native");
    if let Some(out) = wasm("rx", "libkarac_runtime_wasm_regex.a", REGEX) {
        assert_eq!(out, WANT_REGEX, "wasm");
    }
}
