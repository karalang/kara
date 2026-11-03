//! `String.from_utf8(bytes: Slice[u8])` borrows its argument
//! (strings.md; parameters borrow by default, core semantics §4). The
//! ownership pass gave the builtin no modes, so its argument fell to the
//! consume default and a second call on one `Vec[u8]` was refused "value
//! 'v' moved here" (B-2026-11-03-13). Pinned on check, the interpreter, a
//! built binary and the ASan lane (the lent `Vec` is freed once, by its
//! holder).
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "karac-from-utf8-lends-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
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

fn asan_available() -> bool {
    if std::env::var_os("KARAC_SKIP_ASAN_TESTS").is_some() {
        return false;
    }
    let dir = Scratch::new("asan-probe");
    let c = dir.path().join("probe.c");
    let exe = dir.path().join("probe");
    std::fs::write(&c, "int main(void){return 0;}\n").expect("write probe");
    Command::new("cc")
        .arg("-fsanitize=address")
        .arg(&c)
        .arg("-o")
        .arg(&exe)
        .status()
        .is_ok_and(|s| s.success())
        && Command::new(&exe).status().is_ok_and(|s| s.success())
}

/// The program's stdout on the interpreter, as a built binary and on the
/// ASan lane, each where it can run here.
fn every_lane(name: &str, src: &str, want: &str) {
    let dir = Scratch::new(name);
    let file = dir.path().join("main.kara");
    std::fs::write(&file, src).unwrap();
    let karac = env!("CARGO_BIN_EXE_karac");
    let f = file.to_str().unwrap();

    let out = Command::new(karac)
        .args(["__mir-run", f])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{name}: interpreter: {err}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        want,
        "{name}: interpreter"
    );

    let exe = dir.path().join("main");
    let built = Command::new(karac)
        .args(["build", f, "-o", exe.to_str().unwrap()])
        .output()
        .unwrap();
    let linked = common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&built.stderr).to_string())
    });
    if linked.is_some() {
        let out = Command::new(&exe).stdin(Stdio::null()).output().unwrap();
        assert!(out.status.success(), "{name}: binary: {:?}", out.status);
        assert_eq!(String::from_utf8_lossy(&out.stdout), want, "{name}: binary");
    }

    if !asan_available() {
        eprintln!("{name}: skipping the ASan lane: no ASan-capable cc");
        return;
    }
    let out = Command::new(karac)
        .args(["__mir-llvm-asan", f])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    if out.status.code() == Some(3) && err.contains("Linker failed") {
        let _ = common::link_or_skip(Err(err.into_owned()));
        return;
    }
    assert_eq!(out.status.code(), Some(0), "{name}: ASan:\n{err}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), want, "{name}: ASan");
}

#[test]
fn from_utf8_lends_its_bytes() {
    let src = r#"struct Out { stdout: Vec[u8] }
fn main() {
    let v: Vec[u8] = [104u8, 105u8].to_vec();
    println(String.from_utf8(v).is_ok());
    match String.from_utf8(v) {
        Ok(s) => println(s),
        Err(_) => println("err"),
    }
    println(v.len());
    let bad: Vec[u8] = [104u8, 255u8].to_vec();
    println(String.from_utf8(bad).is_err());
    println(bad.len());
    let a = [104u8, 195u8, 169u8];
    println(String.from_utf8(a).unwrap_or(String.new()));
    println(String.from_utf8(a).unwrap_or(String.new()).len());
    let o = Out { stdout: [111u8, 107u8].to_vec() };
    println(String.from_utf8(o.stdout).unwrap_or(String.new()));
    println(o.stdout.len());
    println(String.from_utf8([120u8, 121u8].to_vec()).unwrap_or(String.new()));
}
"#;
    let dir = Scratch::new("check");
    let file = dir.path().join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "check: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    every_lane("lends", src, "true\nhi\n2\ntrue\n2\nhé\n3\nok\n2\nxy\n");
}
