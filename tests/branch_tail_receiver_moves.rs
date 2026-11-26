//! An `if`/`else` or `match` whose branch tail is a local moves that local
//! into the expression's value, wherever the expression sits: as a method
//! receiver, a call argument or a tuple element, as already for a `let`
//! initializer and a bare block (core semantics §3.1, B-2026-11-26-14).
//!
//! `if c { b1 } else { mk("a") }.len(); println(b1)` passed `karac check`
//! and then stopped interp and the JIT at the internal MIR move check,
//! while `{ b1 }.len()` and `let s = if c { b1 } else { .. }` were E0500.
//! The then-block tail and each match arm body were walked in the
//! receiver's borrowing mode rather than as consuming tails. The else
//! branch was already consuming through the block walk, so only the
//! last-branch order was caught.

mod common;

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static CALL: AtomicUsize = AtomicUsize::new(0);

const PRELUDE: &str = "fn mk(s: Str) -> String { f\"{s}-x\" }\n\
     fn show(s: ref String) { println(s); }\n";

fn file(src: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-branch-tail-receiver-{}-{}",
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

/// `body`, inside `main` after `let c = true; let b1 = mk("b");`, is
/// refused with E0500 on `b1`, as moved on the one path (`maybe`) or
/// outright.
fn refused(body: &str, maybe: bool) {
    let src = format!("fn main() {{ let c = true; let b1 = mk(\"b\"); {body} }}\n");
    let (ok, out) = check(&src);
    assert!(!ok, "{src}\n{out}");
    assert!(out.contains("E0500"), "{src}\n{out}");
    let want = if maybe {
        "value 'b1' may have been moved"
    } else {
        "value 'b1' moved here, used again here"
    };
    assert!(out.contains(want), "{src}\n{out}");
}

#[test]
fn an_if_receiver_moves_its_branch_tail_in_either_order() {
    refused(
        "let n = if c { b1 } else { mk(\"a\") }.len(); println(f\"{n} {b1}\");",
        true,
    );
    refused(
        "let n = if c { mk(\"a\") } else { b1 }.len(); println(f\"{n} {b1}\");",
        true,
    );
    refused("(if c { b1 } else { mk(\"a\") }).len(); println(b1);", true);
    refused("(if c { mk(\"a\") } else { b1 }).len(); println(b1);", true);
}

#[test]
fn a_match_receiver_moves_its_arm_body_in_either_order() {
    refused(
        "let n = match c { true => b1, false => mk(\"a\") }.len(); println(f\"{n} {b1}\");",
        true,
    );
    refused(
        "let n = match c { true => mk(\"a\"), false => b1 }.len(); println(f\"{n} {b1}\");",
        true,
    );
    refused(
        "match c { true => b1, false => mk(\"a\") }; println(b1);",
        true,
    );
}

#[test]
fn a_branch_tail_as_an_argument_or_element_moves_too() {
    refused("show(if c { b1 } else { mk(\"a\") }); println(b1);", true);
    refused(
        "show(match c { true => b1, false => mk(\"a\") }); println(b1);",
        true,
    );
    refused(
        "println(if c { b1 } else { mk(\"a\") }); println(b1);",
        true,
    );
    refused(
        "let t = (if c { b1 } else { mk(\"a\") }, 1); println(b1);",
        true,
    );
}

#[test]
fn the_forms_that_were_already_refused_stay_refused() {
    refused("let s = if c { b1 } else { mk(\"a\") }; println(b1);", true);
    refused("let n = { b1 }.len(); println(f\"{n} {b1}\");", false);
}

#[test]
fn a_branch_that_only_reads_the_local_or_clones_it_still_runs() {
    // A use inside the branch that is not its tail borrows.
    runs_everywhere(
        "fn main() { let c = true; let b1 = mk(\"b\"); \
         let r = if c { b1.len() } else { 0 }; println(f\"{r} {b1}\"); }\n",
        "3 b-x\n",
    );
    runs_everywhere(
        "fn main() { let c = false; let b1 = mk(\"b\"); \
         let n = if c { b1.clone() } else { mk(\"aa\") }.len(); println(f\"{n} {b1}\"); }\n",
        "4 b-x\n",
    );
    // Moved on every path and not used again: the value drops once.
    runs_everywhere(
        "fn main() { let c = true; let b1 = mk(\"b\"); \
         let n = if c { b1 } else { mk(\"a\") }.len(); println(n); }\n",
        "3\n",
    );
    runs_everywhere(
        "fn main() { let c = false; let b1 = mk(\"b\"); \
         let n = match c { true => mk(\"aa\"), false => b1 }.len(); println(n); }\n",
        "3\n",
    );
}
