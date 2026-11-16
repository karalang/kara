//! Claims of design.md § 17 Tooling contract checked against the binary.
//!
//! - A successful `karac build --output=jsonl` printed one untyped
//!   `{"status":"ok",...}` line instead of the event stream, and
//!   `--output=json` left `program_effects` out of its document
//!   (B-2026-11-16-46). The stream now runs `build_start` through the checks'
//!   phases, then `codegen`, then `build_complete` with `output`; a failure
//!   in code generation or at link is the `codegen` phase's diagnostic.

use serde_json::Value;
use std::path::PathBuf;
use std::process::{Command, Output};

const LLVM: bool = cfg!(feature = "llvm");

fn file(tag: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("karac-tooling-conf-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    (dir, f)
}

fn karac(dir: &PathBuf, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
    c.current_dir(dir).args(args);
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().unwrap()
}

fn events(stdout: &[u8]) -> Vec<Value> {
    String::from_utf8_lossy(stdout)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}")))
        .collect()
}

const WARNS: &str = "#[deprecated]\nfn old() -> i64 { 1 }\n\nfn main() {\n    println(old());\n}\n";

#[test]
fn tooling_a_passing_build_streams_its_events() {
    let (dir, _) = file("stream", WARNS);
    let out = karac(
        &dir,
        &["build", "--output=jsonl", "main.kara", "-o", "m"],
        &[],
    );
    let evs = events(&out.stdout);
    assert!(evs.iter().all(|e| e["type"].is_string()), "{evs:?}");
    assert_eq!(evs[0]["type"], "build_start", "{evs:?}");
    let last = evs.last().unwrap();
    assert_eq!(last["type"], "build_complete", "{evs:?}");
    assert_eq!(last["build_id"], evs[0]["build_id"], "{evs:?}");
    assert_eq!(last["total_warnings"], 1, "{evs:?}");
    assert_eq!(
        last["program_effects"],
        serde_json::json!(["writes(Stdout)"])
    );
    let diags: Vec<&Value> = evs.iter().filter(|e| e["type"] == "diagnostic").collect();
    assert_eq!(diags.len(), 1, "{evs:?}");
    assert_eq!(diags[0]["id"], "d1");
    assert_eq!(diags[0]["code"], "W0245");
    // Invariant 3: `total_errors` is the sum of the phases' errors.
    let sum: u64 = evs
        .iter()
        .filter(|e| e["type"] == "phase_complete")
        .map(|e| e["errors"].as_u64().unwrap())
        .sum();
    assert_eq!(last["total_errors"].as_u64(), Some(sum), "{evs:?}");
    if LLVM {
        assert_eq!(out.status.code(), Some(0), "{evs:?}");
        assert_eq!(last["success"], true, "{evs:?}");
        assert_eq!(last["output"], "m", "{evs:?}");
        let phases: Vec<&str> = evs
            .iter()
            .filter(|e| e["type"] == "phase_complete")
            .map(|e| e["phase"].as_str().unwrap())
            .collect();
        assert_eq!(phases.last(), Some(&"codegen"), "{phases:?}");
    }
    // The batch document of the same build: the stream's diagnostics and
    // its effects.
    let out = karac(
        &dir,
        &["build", "--output=json", "main.kara", "-o", "m"],
        &[],
    );
    let doc: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(doc["program_effects"], last["program_effects"], "{doc}");
    let mut d = diags[0].clone();
    d.as_object_mut().unwrap().remove("type");
    assert_eq!(doc["diagnostics"], serde_json::json!([d]), "{doc}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn tooling_a_codegen_failure_ends_the_stream() {
    if !LLVM {
        return;
    }
    let (dir, _) = file("linkfail", "fn main() {\n    println(\"hi\");\n}\n");
    let out = karac(
        &dir,
        &["build", "--output=jsonl", "main.kara"],
        &[("KARAC_RUNTIME", "/nonexistent/libkarac_runtime.a")],
    );
    let evs = events(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{evs:?}");
    let n = evs.len();
    assert_eq!(evs[n - 3]["type"], "diagnostic", "{evs:?}");
    assert_eq!(evs[n - 3]["phase"], "codegen", "{evs:?}");
    assert_eq!(evs[n - 3]["id"], "d1", "{evs:?}");
    assert_eq!(evs[n - 2]["phase"], "codegen", "{evs:?}");
    assert_eq!(evs[n - 2]["errors"], 1, "{evs:?}");
    assert_eq!(evs[n - 1]["type"], "build_complete", "{evs:?}");
    assert_eq!(evs[n - 1]["success"], false, "{evs:?}");
    assert_eq!(evs[n - 1]["total_errors"], 1, "{evs:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn tooling_fmt_is_idempotent_and_check_exits_one() {
    let src = "import std.fs.{read_to_string};\nimport std.collections.{Map};\n\
struct P { x: i64 }\nimpl P {\n    fn zeta(self) -> i64 { self.x }\n    \
fn new() -> P { P { x: 1 } }\n    #[inline]\n    fn beta(self) -> i64 { 2 }\n}\n\
fn main() { let p = P.new(); println(p.beta()); }\n";
    let (dir, _) = file("fmt", src);
    let once = karac(&dir, &["fmt", "main.kara"], &[]);
    assert_eq!(once.status.code(), Some(0));
    let formatted = String::from_utf8_lossy(&once.stdout).to_string();
    // A single file prints to stdout and is left as it was.
    assert_eq!(std::fs::read_to_string(dir.join("main.kara")).unwrap(), src);
    // Imports sorted; constructors before methods, each group alphabetical.
    let order = [
        "import std.collections",
        "import std.fs",
        "fn new",
        "fn beta",
        "fn zeta",
    ];
    let at: Vec<usize> = order.iter().map(|s| formatted.find(s).unwrap()).collect();
    assert!(at.windows(2).all(|w| w[0] < w[1]), "{formatted}");
    assert!(
        formatted.contains("    #[inline]\n    fn beta"),
        "{formatted}"
    );
    let check = karac(&dir, &["fmt", "--check", "main.kara"], &[]);
    assert_eq!(check.status.code(), Some(1));
    std::fs::write(dir.join("main.kara"), &formatted).unwrap();
    let twice = karac(&dir, &["fmt", "main.kara"], &[]);
    assert_eq!(String::from_utf8_lossy(&twice.stdout), formatted);
    let check = karac(&dir, &["fmt", "--check", "main.kara"], &[]);
    assert_eq!(check.status.code(), Some(0));
    let _ = std::fs::remove_dir_all(&dir);
}
