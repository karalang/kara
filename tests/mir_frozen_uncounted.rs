//! A frozen handle is never counted (core semantics §6, B-2026-10-26-15):
//! an alias of a frozen place (`let k = n.kids[i];`) is a borrow, and a
//! worklist the ownership check proved frozen (`w.push(g)`) holds
//! uncounted copies that its drop does not release (`mir::frozen`). Two
//! `par` branches walking one frozen graph then write nothing to it, so
//! ThreadSanitizer on four workers reports nothing.
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

const ALIAS: &str = r#"shared struct Node { val: i64, kids: Vec[Node] }

fn build(depth: i64) -> Node {
    if depth <= 0 { return Node { val: 1, kids: [] }; }
    Node { val: depth, kids: [build(depth - 1), build(depth - 1)] }
}

fn sum(n: frozen Node) -> i64 {
    let mut s: i64 = n.val;
    let mut i: i64 = 0;
    while i < n.kids.len() {
        let k = n.kids[i];
        s = s + sum(k);
        i = i + 1;
    }
    s
}

fn driver(root: frozen Node) -> i64 {
    let (a, b) = par { sum(root), sum(root) };
    a * 1000 + b
}

fn main() {
    let root = build(8);
    println(driver(root));
}
"#;

/// `sum` of a depth-`d` tree is `d + 2 * sum(d - 1)`, from 1 at the leaves.
const ALIAS_WANT: &str = "758758\n";

const WORKLIST: &str = r#"shared struct Node { val: i64, mut kids: Vec[Node] }

fn build(depth: i64) -> Node {
    if depth <= 0 { return Node { val: 1, kids: [] }; }
    Node { val: depth, kids: [build(depth - 1), build(depth - 1)] }
}

fn main() {
    let root = build(5);
    let g = freeze root;
    let (x, y) = par {
        {
            let mut worka: Vec[Node] = Vec.new();
            worka.push(g);
            let mut ia: i64 = 0;
            let mut acca: i64 = 0;
            while ia < worka.len() {
                let na = worka[ia as u64];
                acca = acca + na.val;
                for ka in na.kids { worka.push(ka); }
                ia = ia + 1;
            }
            acca
        },
        {
            let mut workb: Vec[Node] = Vec.new();
            workb.push(g);
            let mut ib: i64 = 0;
            let mut accb: i64 = 0;
            while ib < workb.len() {
                let nb = workb[ib as u64];
                accb = accb + nb.val;
                for kb in nb.kids { workb.push(kb); }
                ib = ib + 1;
            }
            accb
        },
    };
    println(f"{x} {y}");
}
"#;

const WORKLIST_WANT: &str = "89 89\n";

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
fn mir_frozen_alias_is_a_borrow_with_no_count_or_drop() {
    let sum = body(ALIAS, "sum");
    assert!(sum.contains("// k"), "{sum}");
    assert!(!sum.contains("retain("), "{sum}");
    assert!(!sum.contains("drop("), "{sum}");
}

#[test]
fn mir_frozen_worklist_pushes_copies_and_releases_none() {
    for branch in ["main.c0", "main.c1"] {
        let b = body(WORKLIST, branch);
        assert_eq!(b.matches(".__push_frozen(").count(), 2, "{b}");
        assert_eq!(b.matches(".__forget_items(").count(), 1, "{b}");
        assert!(!b.contains("retain("), "{b}");
        assert!(!b.contains("].push("), "{b}");
    }
}

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str, src: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "karac-frozen-uncounted-{name}-{}",
            std::process::id()
        ));
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

fn karac(cmd: &str, file: &Path, threads: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(cmd)
        .arg(file)
        .env("KARAC_THREADS", threads)
        .output()
        .expect("run karac")
}

/// The interpreter, the compiled backend and ASan (whose leak check finds
/// a frozen container's buffer the drop no longer frees, and whose
/// use-after-free check finds a release it should not have made) print
/// `want`, on one worker and on four.
fn runs_everywhere(name: &str, src: &str, want: &str) {
    let dir = Scratch::new(name, src);
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
                want,
                "{cmd} on {threads}"
            );
        }
    }
}

#[test]
fn mir_frozen_alias_traversal_runs_everywhere() {
    runs_everywhere("alias", ALIAS, ALIAS_WANT);
}

#[test]
fn mir_frozen_worklist_traversal_runs_everywhere() {
    runs_everywhere("worklist", WORKLIST, WORKLIST_WANT);
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

/// `src` runs clean under ThreadSanitizer on four workers, five times
/// (before the fix every run of either program reported a race).
fn tsan_clean(name: &str, src: &str, want: &str) {
    if !tsan_available() {
        eprintln!("skipping: no TSan-capable cc");
        return;
    }
    let dir = Scratch::new(name, src);
    for _ in 0..5 {
        let out = karac("__mir-llvm-tsan", &dir.file(), "4");
        let err = String::from_utf8_lossy(&out.stderr);
        let unlinked = err.contains("Linker failed") || err.contains("libkarac_runtime_tsan.a");
        if out.status.code() == Some(3) && unlinked {
            let _ = common::link_or_skip(Err(err.into_owned()));
            return;
        }
        assert_eq!(out.status.code(), Some(0), "stderr:\n{err}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), want);
    }
}

#[test]
fn mir_tsan_frozen_alias_traversal_on_four_workers_is_clean() {
    tsan_clean("tsan-alias", ALIAS, ALIAS_WANT);
}

#[test]
fn mir_tsan_frozen_worklist_traversal_on_four_workers_is_clean() {
    tsan_clean("tsan-worklist", WORKLIST, WORKLIST_WANT);
}
