//! `karac build --mir`: the MIR backend compiled ahead of time (redesign M2).
//!
//! The IR `codegen::mir::compile_mir_ir` produces goes through
//! `codegen::mir_aot::compile_object` and the ordinary runtime link, and the
//! binary must print what the MIR interpreter prints, at every optimisation
//! level the build offers.
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A scratch directory removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Scratch {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("karac-mir-build-test-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Scratch(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const PROGRAM: &str = r#"
struct P { name: String, n: i64 }
fn main() {
    let mut v: Vec[P] = Vec.new();
    for i in 0..5 { v.push(P { name: f"p{i}", n: i * i }); }
    let total = v.iter().map(|p| p.n).sum();
    let mut m: Map[String, i64] = Map.new();
    for p in v.iter() { m[p.name.clone()] = p.n; }
    println(f"hello {total} {m.len()} {v[3].name}");
}
"#;

/// Build `src` at `opt`, link and run it: its stdout, or `None` to skip.
fn build_and_run(src: &str, opt: u8) -> Option<String> {
    let lowered = karac::mir::lower::build_source(src).expect("MIR");
    let ir = karac::codegen::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR");
    let dir = Scratch::new();
    let obj = dir.path().join("main.o");
    let exe = dir.path().join("main");
    karac::codegen::mir_aot::compile_object(&ir, &obj, opt).expect("object");
    common::link_or_skip(karac::codegen::link_executable(
        &obj.to_string_lossy(),
        &exe.to_string_lossy(),
    ))?;
    let out = Command::new(&exe).output().expect("run");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(String::from_utf8(out.stdout).expect("utf-8"))
}

#[test]
fn mir_build_matches_the_interpreter_at_every_opt_level() {
    for opt in 0..=3 {
        if let Some(out) = build_and_run(PROGRAM, opt) {
            assert_eq!(out, "hello 30 5 p3\n", "at -O{opt}");
        }
    }
}

/// The CLI: `karac build --mir <file> -o <path>` writes the executable there.
#[test]
fn karac_build_mir_writes_the_executable() {
    let dir = Scratch::new();
    let src = dir.path().join("hello.kara");
    std::fs::write(&src, PROGRAM).unwrap();
    let exe = dir.path().join("hello");
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args([
            "build",
            "--mir",
            src.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if common::link_or_skip(if out.status.success() {
        Ok(())
    } else {
        Err(stderr.to_string())
    })
    .is_none()
    {
        return;
    }
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("Built: {}\n", exe.display())
    );
    let run = Command::new(&exe).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&run.stdout), "hello 30 5 p3\n");
}
