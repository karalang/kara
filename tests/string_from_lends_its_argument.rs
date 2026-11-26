//! `String.from(s)` with `s` a `String` copies it: `String.from` takes a
//! `Str` (design.md §9 Conversion traits: `impl From[Str] for String`), and
//! a `String` passed at a `Str` slot is lent, as at any other call
//! (B-2026-11-26-5).
//!
//! The item of a `for` over a local collection of strings was refused as a
//! move out of a borrowed place (E0516, with `.clone()` advice that made a
//! second copy), and an owned local or field passed to `String.from` was
//! moved, so a later use was E0500. A `String` the argument itself makes
//! (an f-string, a call) is still the result, with no copy.

mod common;

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static CALL: AtomicUsize = AtomicUsize::new(0);

const PRELUDE: &str = "struct P { name: String }\n";

fn file(src: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-string-from-lends-{}-{}",
        std::process::id(),
        CALL.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, format!("{PRELUDE}{src}")).unwrap();
    (dir, f)
}

/// `karac check` on the prelude and `src`: whether it passed, and its output.
fn check(src: &str) -> (bool, String) {
    let (dir, f) = file(src);
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&f)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// The program's stdout under `cmd`, after `karac check` accepted it;
/// `None` when a compiled backend cannot link.
fn run(src: &str, cmd: &str) -> Option<String> {
    let (ok, out) = check(src);
    assert!(ok, "check:\n{out}\n{src}");
    let (dir, f) = file(src);
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(cmd)
        .arg(&f)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    if cmd != "__mir-run" {
        common::link_or_skip(if out.status.success() {
            Ok(())
        } else {
            Err(err.clone())
        })?;
    }
    assert!(out.status.success(), "{cmd}: {err}\n{src}");
    Some(String::from_utf8(out.stdout).unwrap())
}

fn runs_everywhere(src: &str, want: &str) {
    let mut lanes = vec!["__mir-run"];
    if cfg!(feature = "llvm") {
        lanes.extend(["__mir-llvm-run", "__mir-llvm-asan"]);
    }
    for cmd in lanes {
        if let Some(out) = run(src, cmd) {
            assert_eq!(out, want, "{cmd}\n{src}");
        }
    }
}

#[test]
fn the_item_of_a_for_over_a_local_collection_is_copied() {
    runs_everywhere(
        "fn main() { let mut cases: Vec[String] = Vec.new(); \
         for s in [\"1101\", \"10\", \"1\"] { cases.push(String.from(s)); } \
         println(f\"{cases.len()} {cases[0]} {cases[2]}\"); }\n",
        "3 1101 1\n",
    );
    runs_everywhere(
        "fn main() { let words = [\"ab\", \"c\"]; let mut n = 0; \
         for s in words { let o = String.from(s); n += o.len(); } println(n); }\n",
        "3\n",
    );
    runs_everywhere(
        "fn main() { let words: Vec[String] = [\"ab\", \"c\"].to_vec(); let mut n = 0; \
         for s in words { let o = String.from(s); n += o.len(); } println(f\"{n} {words.len()}\"); }\n",
        "3 2\n",
    );
}

#[test]
fn an_owned_local_field_or_element_is_lent_and_kept() {
    runs_everywhere(
        "fn main() { let a = String.from(\"xy\"); let b = String.from(a); \
         println(f\"{a} {b}\"); }\n",
        "xy xy\n",
    );
    runs_everywhere(
        "fn main() { let p = P { name: String.from(\"n\") }; let q = String.from(p.name); \
         println(f\"{p.name} {q}\"); }\n",
        "n n\n",
    );
    runs_everywhere(
        "fn main() { let v: Vec[String] = [\"x\", \"y\"].to_vec(); let w = String.from(v[1]); \
         println(f\"{w} {v[1]}\"); }\n",
        "y y\n",
    );
    // The copy is its own value: changing it leaves the original.
    runs_everywhere(
        "fn main() { let a = String.from(\"xy\"); let mut b = String.from(a); b.push_str(\"z\"); \
         println(f\"{a} {b}\"); }\n",
        "xy xyz\n",
    );
}

#[test]
fn a_string_the_argument_makes_is_the_result() {
    runs_everywhere(
        "fn mk() -> String { String.from(\"m\") }\n\
         fn main() { let n = 3; let a = String.from(f\"t{n}\"); let b = String.from(mk()); \
         let c = String.from('z'); println(f\"{a} {b} {c}\"); }\n",
        "t3 m z\n",
    );
}
