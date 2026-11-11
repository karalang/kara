//! `String.from_utf8_lossy(bytes: Slice[u8]) -> String` (strings.md;
//! B-2026-11-11-24): Rust's, each invalid sequence replaced with U+FFFD,
//! borrowing its bytes as `from_utf8` does. Pinned on check, the
//! interpreter, a built binary and the ASan lane.
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "karac-from-utf8-lossy-{name}-{}",
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
fn from_utf8_lossy_replaces_each_invalid_sequence() {
    let src = r#"fn main() {
    let v: Vec[u8] = [104u8, 105u8, 255u8, 226u8, 130u8].to_vec();
    let s = String.from_utf8_lossy(v);
    println(s);
    println(s.len());
    println(v.len());
    println(String.from_utf8_lossy([226u8, 65u8].to_vec()));
    let ok = [240u8, 159u8, 152u8, 128u8, 33u8];
    println(String.from_utf8_lossy(ok));
    let t = "héllo".to_string();
    println(String.from_utf8_lossy(t.bytes()));
    let e: Vec[u8] = Vec.new();
    println(String.from_utf8_lossy(e).len());
    println(String.from_utf8_lossy(v) == s);
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
    every_lane(
        "lossy",
        src,
        "hi\u{FFFD}\u{FFFD}\n8\n5\n\u{FFFD}A\n\u{1F600}!\nhéllo\n0\ntrue\n",
    );
}

/// Rust's spelling mends to it: `String::from_utf8_lossy(&b)` takes the
/// path and borrow edits, and the `Cow`'s `.into_owned()` becomes
/// `.to_string()`, an owned copy of the `String` Kāra already returns.
#[test]
fn rusts_from_utf8_lossy_into_owned_mends_and_runs() {
    let src = "fn main() {\n    let b: Vec<u8> = vec![104, 105, 255];\n    \
               let s = String::from_utf8_lossy(&b).into_owned();\n    \
               let t = String::from_utf8_lossy(&b).to_string();\n    \
               println!(\"{} {}\", s, t);\n}\n";
    let dir = Scratch::new("mend");
    let file = dir.path().join("main.kara");
    std::fs::write(&file, src).unwrap();
    let karac = env!("CARGO_BIN_EXE_karac");
    let f = file.to_str().unwrap();
    // The type checker's word, on the source the parser's edits leave.
    let kara = "fn main() {\n    let b: Vec[u8] = [104, 105];\n    \
                println(String.from_utf8_lossy(b).into_owned());\n}\n";
    std::fs::write(&file, kara).unwrap();
    let check = Command::new(karac).args(["check", f]).output().unwrap();
    std::fs::write(&file, src).unwrap();
    let said = String::from_utf8_lossy(&check.stderr).to_string()
        + &String::from_utf8_lossy(&check.stdout);
    assert!(
        said.contains("no method 'into_owned' on type 'String': the text is already owned"),
        "{said}"
    );
    // The Mend loop: `karac fix` applies one phase's edits a pass (the
    // parser's, then the type checker's), so it runs to a fixed point.
    let mut mended = src.to_string();
    for _ in 0..4 {
        let _ = Command::new(karac).args(["fix", f]).output().unwrap();
        let now = std::fs::read_to_string(&file).unwrap();
        if now == mended {
            break;
        }
        mended = now;
    }
    assert!(
        mended.contains("String.from_utf8_lossy(b).to_string()"),
        "{mended}"
    );
    assert!(!mended.contains("into_owned"), "{mended}");
    let check = Command::new(karac).args(["check", f]).output().unwrap();
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    every_lane("mend", &mended, "hi\u{FFFD} hi\u{FFFD}\n");
}
