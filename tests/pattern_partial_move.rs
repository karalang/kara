//! A destructuring pattern that takes some parts out of a local moves
//! those parts, not the local (core semantics §3.6, §4.6): after
//! `let P { a, .. } = p`, `p.k` and `p.b` are still usable and `p` as a
//! whole is not, as after `let a = p.a` (B-2026-11-25-45).
//!
//! Each of `let (a, _, _) = t`, `let P { a, .. } = p` and `match p {
//! P { a, .. } => .. }` made the whole root unusable, so a later `t.2`,
//! `p.k` or `p.b.n` was E0500. The remaining parts drop at the root's scope
//! end in the §7.8 order, skipping the moved one (§7.9), which the drop
//! schedule already did.

mod common;

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static CALL: AtomicUsize = AtomicUsize::new(0);

const PRELUDE: &str = "struct D { n: i64 }\n\
     impl Drop for D { fn drop(mut ref self) { println(f\"drop {self.n}\"); } }\n\
     struct Q { x: D, y: D }\n\
     struct P { a: D, b: D, k: i64 }\n\
     struct N { a: D, inner: Q }\n\
     fn mk() -> P { P { a: D { n: 1 }, b: D { n: 2 }, k: 5 } }\n\
     fn show(p: ref P) { println(p.k); }\n";

fn file(src: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-pattern-partial-move-{}-{}",
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

/// `src` is refused with E0500 naming `part`.
fn refused(src: &str, part: &str) {
    let (ok, out) = check(src);
    assert!(!ok, "{src}\n{out}");
    assert!(out.contains("E0500"), "{src}\n{out}");
    assert!(
        out.contains(&format!("value '{part}' moved here, used again here")),
        "{src}\n{out}"
    );
}

#[test]
fn a_tuple_pattern_moves_only_its_element() {
    runs_everywhere(
        "fn main() { let t = (D { n: 1 }, D { n: 2 }, 7); let (a, _, _) = t; \
         println(t.2); println(t.1.n); println(a.n); }\n",
        "7\n2\n1\ndrop 1\ndrop 2\n",
    );
}

#[test]
fn a_let_struct_pattern_moves_only_its_field() {
    runs_everywhere(
        "fn main() { let p = mk(); let P { a, .. } = p; \
         println(p.k); println(p.b.n); println(a.n); }\n",
        "5\n2\n1\ndrop 1\ndrop 2\n",
    );
}

#[test]
fn a_match_arm_pattern_moves_only_its_field() {
    runs_everywhere(
        "fn main() { let p = mk(); match p { P { a, .. } => println(a.n) } \
         println(p.k); println(p.b.n); }\n",
        "1\ndrop 1\n5\n2\ndrop 2\n",
    );
}

#[test]
fn several_parts_ref_parts_nested_parts_and_if_let() {
    // Two moved fields leave `k`; a `ref` field is only borrowed.
    runs_everywhere(
        "fn main() { let p = mk(); let P { a, b, .. } = p; println(p.k); println(a.n + b.n); }\n",
        "5\n3\ndrop 2\ndrop 1\n",
    );
    runs_everywhere(
        "fn main() { let p = mk(); let P { a, b: ref b, .. } = p; \
         println(p.b.n); println(b.n); println(a.n); }\n",
        "2\n2\n1\ndrop 1\ndrop 2\n",
    );
    // A nested pattern moves the innermost part.
    runs_everywhere(
        "fn main() { let s = N { a: D { n: 1 }, inner: Q { x: D { n: 3 }, y: D { n: 4 } } }; \
         let N { inner: Q { x, .. }, .. } = s; println(s.inner.y.n); println(x.n); }\n",
        "4\n3\ndrop 3\ndrop 4\ndrop 1\n",
    );
    // Two patterns take disjoint parts; `if let` moves its part too.
    runs_everywhere(
        "fn main() { let p = mk(); let P { a, .. } = p; let P { b, .. } = p; \
         println(a.n + b.n + p.k); }\n",
        "8\ndrop 2\ndrop 1\n",
    );
    runs_everywhere(
        "fn main() { let p = mk(); if let P { a, .. } = p { println(a.n); } println(p.b.n); }\n",
        "1\ndrop 1\n2\ndrop 2\n",
    );
    // Restoring the moved part makes the root whole again (§3.6).
    runs_everywhere(
        "fn main() { let mut p = mk(); let P { a, .. } = p; p.a = D { n: 9 }; show(p); \
         println(a.n); }\n",
        "5\n1\ndrop 1\ndrop 2\ndrop 9\n",
    );
}

#[test]
fn the_whole_root_and_the_moved_part_stay_refused() {
    // `err_partial_then_whole`, through a pattern: the root as a whole.
    refused(
        "fn main() { let p = mk(); let P { a, .. } = p; show(p); }\n",
        "p.a",
    );
    refused(
        "fn main() { let p = mk(); let P { a, .. } = p; let q = p; }\n",
        "p.a",
    );
    refused(
        "fn main() { let p = mk(); match p { P { a, .. } => println(a.n) } show(p); }\n",
        "p.a",
    );
    refused(
        "fn main() { let t = (D { n: 1 }, D { n: 2 }, 7); let (a, _, _) = t; let u = t; }\n",
        "t.0",
    );
    // The moved part itself, named as the one the use overlaps.
    refused(
        "fn main() { let p = mk(); let P { a, b, .. } = p; println(p.k); println(p.b.n); }\n",
        "p.b",
    );
    refused(
        "fn main() { let p = mk(); let P { a, .. } = p; let P { a: x, .. } = p; }\n",
        "p.a",
    );
    refused(
        "fn main() { let p = mk(); match p { P { a, .. } => println(a.n) } println(p.a.n); }\n",
        "p.a",
    );
    let (ok, out) =
        check("fn main() { let p = mk(); for i in 0..2 { let P { a, .. } = p; println(a.n); } }\n");
    assert!(!ok && out.contains("inside a loop"), "{out}");
    // A pattern's move is repaired by `ref` on its binding.
    let (_, out) = check("fn main() { let p = mk(); let P { a, .. } = p; show(p); }\n");
    assert!(
        out.contains("a binding in that pattern moves its part out"),
        "{out}"
    );
}
