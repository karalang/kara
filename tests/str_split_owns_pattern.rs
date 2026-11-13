//! B-2026-11-13-43 (core semantics §5.4): `split`, `splitn`,
//! `match_indices` and `matches` are methods with a `self` receiver, so
//! their iterator borrows the text only, and a caller may drop the pattern
//! before walking it. The iterators kept a `Str` view of the pattern, which
//! read freed memory (ASan heap-use-after-free); they now keep their own
//! copy. Pinned on the interpreter, a built binary and the ASan lane, with a
//! pattern built at run time and dropped before the walk.
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "karac-str-split-owns-{name}-{}",
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
fn a_split_outlives_its_pattern() {
    let src = r#"fn sep() -> String {
    let mut p = String.new();
    for _ in 0..40 { p.push(';'); }
    p.truncate(1);
    p
}

fn main() {
    let s = "a;b;c;d".to_string();
    let it = { let p = sep(); s.split(p.as_str()) };
    let junk = "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz".to_string();
    for x in it { println(x); }
    let back = { let p = sep(); s.split(p.as_str()) };
    for x in back.rev() { println(x); }
    let firsts = { let p = sep(); s.splitn(2, p.as_str()) };
    for x in firsts { println(x); }
    let found = { let p = sep(); s.match_indices(p.as_str()) };
    for (i, m) in found { println(f"{i} {m}"); }
    let count = { let p = sep(); s.matches(p.as_str()) };
    println(count.count());
    println(junk.len());
}
"#;
    every_lane(
        "split",
        src,
        "a\nb\nc\nd\nd\nc\nb\na\na\nb;c;d\n1 ;\n3 ;\n5 ;\n3\n48\n",
    );
}
