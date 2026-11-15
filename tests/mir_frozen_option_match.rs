//! A `match` over a frozen place typed `Option[S]` (B-2026-11-15-93): a
//! `mut` field is readable through a frozen place (design.md § Frozen
//! handles), so a linked-list walk `match n.next { Some(m) => .., None => .. }`
//! checks as `for k in n.kids` does, and its payload binding is a borrow
//! that counts nothing. Two `par` branches walking one frozen list then
//! write nothing to it, so ThreadSanitizer on four workers reports nothing.
//! A shape the admission does not cover (a binding of the whole `Option`)
//! is still refused.
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const WALK: &str = r#"shared struct Node { val: i64, mut next: Option[Node] }

fn build(n: i64) -> Node {
    if n <= 1 {
        return Node { val: 1, next: None };
    }
    Node { val: n, next: Some(build(n - 1)) }
}

fn walk(n: frozen Node) -> i64 {
    match n.next {
        Some(m) => n.val + walk(m),
        None => n.val,
    }
}

fn second(n: frozen Node) -> i64 {
    match n.next {
        Some(m) => m.val,
        _ => -1,
    }
}

fn main() {
    let root = build(50);
    let g = freeze root;
    let (a, b) = par { walk(g), walk(g) };
    println(f"{a} {b} {second(g)}");
}
"#;

const WALK_WANT: &str = "1275 1275 49\n";

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str, src: &str) -> Scratch {
        let dir =
            std::env::temp_dir().join(format!("karac-frozen-option-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        std::fs::write(dir.join("main.kara"), src).expect("write source");
        Scratch(dir)
    }

    fn file(&self) -> PathBuf {
        self.0.join("main.kara")
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

fn karac(cmd: &str, file: &Path, threads: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(cmd)
        .arg(file)
        .env("KARAC_THREADS", threads)
        .output()
        .expect("run karac")
}

/// The elaborated body named `name`, printed.
fn body(src: &str, name: &str) -> String {
    let l = karac::mir::lower::build_source(src).unwrap_or_else(|e| panic!("{e}"));
    let b = l
        .program
        .bodies
        .values()
        .find(|b| b.instance.name == name)
        .unwrap_or_else(|| panic!("no body {name}"));
    karac::mir::pretty::pretty_body(b, &l.tys)
}

#[test]
fn frozen_option_match_checks() {
    let dir = Scratch::new("check", WALK);
    let out = karac("check", &dir.file(), "1");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn frozen_option_match_binding_is_a_borrow_with_no_count_or_drop() {
    for f in ["walk", "second"] {
        let b = body(WALK, f);
        assert!(b.contains("// m"), "{b}");
        assert!(!b.contains("retain("), "{b}");
        assert!(!b.contains("drop("), "{b}");
    }
}

#[test]
fn frozen_option_match_runs_everywhere() {
    let dir = Scratch::new("run", WALK);
    for cmd in ["__mir-run", "__mir-llvm-run", "__mir-llvm-asan"] {
        if cmd == "__mir-llvm-asan" && std::env::var_os("KARAC_SKIP_ASAN_TESTS").is_some() {
            continue;
        }
        for threads in ["1", "4"] {
            let out = karac(cmd, &dir.file(), threads);
            let err = String::from_utf8_lossy(&out.stderr);
            if out.status.code() == Some(3) && err.contains("Linker failed") {
                let _ = common::link_or_skip(Err(err.into_owned()));
                return;
            }
            assert!(out.status.success(), "{cmd} on {threads}: {err}");
            assert_eq!(
                String::from_utf8_lossy(&out.stdout),
                WALK_WANT,
                "{cmd} on {threads}"
            );
        }
    }
}

#[test]
fn frozen_option_bound_whole_is_still_refused() {
    let dir = Scratch::new(
        "whole",
        "shared struct Node { val: i64, mut next: Option[Node] }\n\
         fn nxt(n: frozen Node) -> i64 {\n    match n.next {\n        o => o.is_some() as i64,\n    }\n}\n\
         fn main() {\n    let r = Node { val: 1, next: None };\n    let g = freeze r;\n    println(nxt(g));\n}\n",
    );
    let out = karac("check", &dir.file(), "1");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{err}");
    assert!(err.contains("cannot be projected through `.next`"), "{err}");
}

/// Whether `cc -fsanitize=thread` links and runs here.
fn tsan_available() -> bool {
    if std::env::var_os("KARAC_SKIP_TSAN_TESTS").is_some() {
        return false;
    }
    let dir = Scratch::new("probe", "");
    let c = dir.path().join("probe.c");
    let exe = dir.path().join("probe");
    std::fs::write(&c, "int main(void){return 0;}\n").expect("write probe");
    Command::new("cc")
        .arg("-fsanitize=thread")
        .arg(&c)
        .arg("-o")
        .arg(&exe)
        .status()
        .is_ok_and(|s| s.success())
        && Command::new(&exe).status().is_ok_and(|s| s.success())
}

#[test]
fn mir_tsan_frozen_option_walk_on_four_workers_is_clean() {
    if !tsan_available() {
        eprintln!("skipping: no TSan-capable cc");
        return;
    }
    let dir = Scratch::new("tsan", WALK);
    for _ in 0..5 {
        let out = karac("__mir-llvm-tsan", &dir.file(), "4");
        let err = String::from_utf8_lossy(&out.stderr);
        let unlinked = err.contains("Linker failed") || err.contains("libkarac_runtime_tsan.a");
        if out.status.code() == Some(3) && unlinked {
            let _ = common::link_or_skip(Err(err.into_owned()));
            return;
        }
        assert_eq!(out.status.code(), Some(0), "stderr:\n{err}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), WALK_WANT);
    }
}
