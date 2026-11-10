//! `karac run` of a program split into codegen units (B-2026-11-10-5). The
//! JIT compiled a module on one thread, so a large program's run waited on
//! one core for what its build spreads over all of them (mini_tldextract:
//! 92 s against a 16 s build). A program the build would split is now
//! compiled the same way, one object per unit on its own thread, and the
//! runner links the objects. `KARAC_MIR_CODEGEN_UNITS` forces the split on a
//! program this small.
#![cfg(feature = "llvm")]

use std::path::PathBuf;
use std::process::{Command, Output};

/// Calls that cross units both ways, a trait method, a closure, a recursive
/// function, strings built in one unit and printed in another, and a
/// sorted map, so a symbol the split left in another object is resolved
/// across the objects or the run fails.
const PROGRAM: &str = r#"struct Point {
    x: i64,
    y: i64,
}

trait Area {
    fn area(self) -> i64;
}

impl Area for Point {
    fn area(self) -> i64 {
        self.x * self.y
    }
}

fn fib(n: i64) -> i64 {
    if n < 2 {
        return n;
    }
    fib(n - 1) + fib(n - 2)
}

fn label(i: i64) -> String {
    f"item-{i}"
}

fn collect(n: i64) -> Vec[String] {
    let mut out: Vec[String] = Vec.new();
    for i in 0..n {
        out.push(label(i));
    }
    out
}

fn tally(words: own Vec[String]) -> SortedMap[String, i64] {
    let mut m: SortedMap[String, i64] = SortedMap.new();
    for w in words.into_iter() {
        let k = w.len();
        m.insert(w, k);
    }
    m
}

fn scale(v: Vec[i64], by: i64) -> Vec[i64] {
    v.iter().map(|x| x * by).collect()
}

fn main() {
    let p = Point { x: 6, y: 7 };
    println(f"area {p.area()}");
    println(f"fib {fib(20)}");
    let words = collect(4);
    for (k, v) in tally(words) {
        println(f"{k} {v}");
    }
    let s = scale([1, 2, 3], 10);
    println(f"{s[0]} {s[1]} {s[2]}");
}
"#;

const EXPECTED: &str = "area 42\nfib 6765\nitem-0 6\nitem-1 6\nitem-2 6\nitem-3 6\n10 20 30\n";

fn write(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-jit-units-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, PROGRAM).unwrap();
    path
}

/// Run `karac <cmd> <path>` with `units` codegen units, and say whether
/// any handoff file the run named after its pid is still there afterwards.
fn run(cmd: &str, path: &PathBuf, units: &str, handoff: &str) -> (Output, Vec<String>) {
    let child = Command::new(env!("CARGO_BIN_EXE_karac"))
        .env("KARAC_MIR_CODEGEN_UNITS", units)
        .env("KARAC_MIR_TIME", "1")
        .env_remove("KARAC_MIR_LL")
        .env_remove("KARAC_DEBUG_INFO")
        .args([cmd])
        .arg(path)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let prefix = format!("{handoff}{}", child.id());
    let out = child.wait_with_output().unwrap();
    let left = std::fs::read_dir(std::env::temp_dir())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(&prefix))
        .collect();
    (out, left)
}

fn check(cmd: &str, handoff: &str) {
    let path = write(cmd.trim_start_matches('_'));
    for units in ["1", "3"] {
        let (out, left) = run(cmd, &path, units, handoff);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            EXPECTED,
            "`karac {cmd}` at {units} units; stderr:\n{stderr}"
        );
        assert_eq!(out.status.code(), Some(0), "stderr:\n{stderr}");
        assert_eq!(
            stderr.contains("jit objects, 3 units"),
            units == "3",
            "only a split program goes through objects; stderr:\n{stderr}"
        );
        assert!(left.is_empty(), "handoff files left behind: {left:?}");
    }
}

#[test]
fn run_v2_a_split_program_runs_from_objects_compiled_per_unit() {
    check("run", "karac_run_");
}

#[test]
fn mir_llvm_run_a_split_program_runs_from_objects_compiled_per_unit() {
    check("__mir-llvm-run", "karac-mir-");
}
