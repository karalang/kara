//! A `shared` box carries a weak count only when some `weak` type names
//! its type (B-2026-11-01-24). With both counts a tree node's 24-byte body
//! (a value and two children) took a 40-byte allocation, which macOS's
//! allocator rounds to 48 where 32 fits exactly: kata 124 peaked at 1.48x
//! legacy's memory. A node no `weak` handle can reach now has the strong
//! count alone, and one a `weak` handle can reach keeps both.
#![cfg(feature = "llvm")]

mod common;

use std::path::PathBuf;
use std::process::{Command, Output};

fn scratch(name: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-mir-shared-header-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    (dir, file)
}

/// The IR `karac build` emits for `src`, and the built program's output
/// (`None` when no linker is here).
fn build_and_run(name: &str, src: &str) -> (String, Option<Output>) {
    let (dir, file) = scratch(name, src);
    let ll = dir.join("main.ll");
    let bin = dir.join("main");
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .env("KARAC_MIR_LL", &ll)
        .args(["build", file.to_str().unwrap(), "-o"])
        .arg(&bin)
        .output()
        .unwrap();
    let ir = std::fs::read_to_string(&ll).unwrap_or_else(|_| {
        panic!(
            "{name}: no IR written: {}",
            String::from_utf8_lossy(&out.stderr)
        )
    });
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    if !out.status.success() {
        if common::link_or_skip(Err(err.clone())).is_none() {
            let _ = std::fs::remove_dir_all(&dir);
            return (ir, None);
        }
        panic!("{name}: build failed: {err}");
    }
    let run = Command::new(&bin).output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (ir, Some(run))
}

/// The program's stdout under AddressSanitizer, which must be clean;
/// `None` when no ASan-capable `cc` or archive is here.
fn asan_stdout(name: &str, src: &str) -> Option<String> {
    if std::env::var_os("KARAC_SKIP_ASAN_TESTS").is_some() {
        return None;
    }
    let (dir, file) = scratch(&format!("{name}-asan"), src);
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("__mir-llvm-asan")
        .arg(&file)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    if out.status.code() == Some(3) && err.contains("Linker failed") {
        common::link_or_skip(Err(err))?;
        return None;
    }
    assert!(out.status.success(), "{name} under ASan: {err}");
    Some(String::from_utf8(out.stdout).unwrap())
}

/// A tree node (24-byte body) with cloned strong handles; with `weak`, a
/// `weak` field names it too, and weak handles outlive the nodes.
fn tree_src(kind: &str, weak: bool) -> String {
    let (watch, main) = if weak {
        (
            r#"
struct Watch {
    of: weak Node,
}

fn alive(w: Watch) -> bool {
    match w.of {
        Option.Some(_) => true,
        Option.None => false,
    }
}
"#,
            r#"
fn main() {
    let (w1, w2) = {
        let leaf = Node { val: 2, left: None, right: None };
        let root = Node { val: 1, left: Some(leaf.clone()), right: Some(leaf.clone()) };
        let w1 = Watch { of: leaf };
        let w2 = Watch { of: root };
        let again = root.clone();
        println(f"sum {sum(Some(again))} alive {alive(w1)} {alive(w2)}");
        (w1, w2)
    };
    println(f"after {alive(w1)} {alive(w2)}");
}
"#,
        )
    } else {
        (
            "",
            r#"
fn main() {
    {
        let leaf = Node { val: 2, left: None, right: None };
        let root = Node { val: 1, left: Some(leaf.clone()), right: Some(leaf.clone()) };
        let again = root.clone();
        println(f"sum {sum(Some(again))}");
    }
    println("after");
}
"#,
        )
    };
    format!(
        r#"{kind} struct Node {{
    val: i64,
    left: Option[Node],
    right: Option[Node],
}}

impl Drop for Node {{
    fn drop(mut ref self) {{
        println(f"drop {{self.val}}");
    }}
}}

fn sum(n: Option[Node]) -> i64 {{
    match n {{
        Option.Some(x) => x.val + sum(x.left) + sum(x.right),
        Option.None => 0,
    }}
}}
{watch}{main}"#
    )
}

fn header_case(kind: &str, weak: bool, bytes: u64, want: &str) {
    let src = tree_src(kind, weak);
    let name = format!("{kind}-{weak}");
    let (ir, run) = build_and_run(&name, &src);
    let other = if bytes == 32 { 40 } else { 32 };
    assert!(
        ir.contains(&format!("malloc(i64 {bytes})")),
        "{name}: no {bytes}-byte box\n{ir}"
    );
    assert!(
        !ir.contains(&format!("malloc(i64 {other})")),
        "{name}: a {other}-byte box"
    );
    if let Some(run) = run {
        assert!(run.status.success(), "{name}: {run:?}");
        assert_eq!(String::from_utf8_lossy(&run.stdout), want, "{name}");
    }
    if let Some(out) = asan_stdout(&name, &src) {
        assert_eq!(out, want, "{name} under ASan");
    }
}

const STRONG_OUT: &str = "sum 5\ndrop 1\ndrop 2\nafter\n";
const WEAK_OUT: &str = "sum 5 alive true true\ndrop 1\ndrop 2\nafter false false\n";

/// No `weak` type names `Node`: its box is the strong count and the body.
#[test]
fn mir_shared_header_without_weak_is_the_strong_count_alone() {
    header_case("shared", false, 32, STRONG_OUT);
    header_case("sync", false, 32, STRONG_OUT);
}

/// A `weak` field names `Node`: its box keeps both counts, and weak
/// handles that outlive the nodes still read as gone.
#[test]
fn mir_shared_header_with_weak_keeps_both_counts() {
    header_case("shared", true, 40, WEAK_OUT);
    header_case("sync", true, 40, WEAK_OUT);
}
