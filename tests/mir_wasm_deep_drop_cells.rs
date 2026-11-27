//! B-2026-11-18-37: the drained release of a deep `shared` structure
//! (B-2026-11-17-138, -139, B-2026-11-18-28) still fell back to the
//! recursive glue when a node's handles sat in a `Mutex`, a `OnceLock` /
//! `OnceCell` or a `Set`, and on wasm_wasi node's engine stack ended a
//! 200000-deep chain through any of them ("Maximum call stack size
//! exceeded"). The plan now reaches a `Mutex`'s value in place, a
//! `OnceLock`'s held value through the runtime's handle (the cell is freed
//! once its parts are taken), and a `Set`'s keys as a map's values are.
//! `Drop` bodies keep the recursive release's order on every lane.
//!
//! The wasm half skips (with a notice) when the wasm archive or node is
//! missing, as `tests/mir_wasm_deep_drop.rs` does.
#![cfg(feature = "llvm")]

use std::path::{Path, PathBuf};
use std::process::Command;

const RUNNER: &str = "\
import { readFile } from 'node:fs/promises';
import { WASI } from 'node:wasi';
const wasi = new WASI({ version: 'preview1', args: [process.argv[2]], env: {}, returnOnExit: true });
const mod = await WebAssembly.compile(await readFile(process.argv[2]));
const inst = await WebAssembly.instantiate(mod, wasi.getImportObject());
process.exitCode = wasi.start(inst);
";

const HASH_EQ: &str = r#"
impl Hash for M {
    fn hash[H: Hasher](self, hasher: mut ref H) {
        hasher.write_i64(self.v);
    }
}

impl PartialEq for M {
    fn eq(self, other: M) -> bool { self.v == other.v }
}

impl Eq for M {}
"#;

/// A 200000-deep chain whose every link sits in the cell `link` builds.
fn deep(field: &str, empty: &str, link: &str) -> String {
    format!(
        "import std.sync.{{Mutex, OnceLock}}; shared struct M {{ v: i64, next: {field} }}
{HASH_EQ}
fn main() {{
    let mut head = M {{ v: 0, next: {empty} }};
    let mut i = 1;
    while i < 200000 {{
        {link}
        head = M {{ v: i, next: c }};
        i += 1;
    }}
    println(f\"built {{head.v}}\");
}}
"
    )
}

/// Drop bodies over a tree whose handles sit in each cell, with a node a
/// handle outside keeps. (Run natively only: the wasm lane's hash seed is
/// not pinned, and a `Set`'s order follows it.)
const ORDER: &str = r#"import std.sync.{Mutex, OnceLock};
shared struct C { name: String, mut lock: Option[Mutex[C]], once: OnceLock[C], mut set: Set[C] }

impl Drop for C {
    fn drop(mut ref self) {
        println(f"drop {self.name}");
    }
}

impl Hash for C {
    fn hash[H: Hasher](self, hasher: mut ref H) {
        self.name.hash(hasher);
    }
}

impl PartialEq for C {
    fn eq(self, other: C) -> bool { self.name == other.name }
}

impl Eq for C {}

fn c(s: own String) -> C { C { name: s, lock: None, once: OnceLock.new(), set: Set.new() } }

fn main() {
    let kept = c("kept");
    {
        let root = c("root");
        let a = c("a");
        a.lock = Some(Mutex.new(c("a.lock")));
        let _ = a.once.set(c("a.once"));
        root.lock = Some(Mutex.new(a));
        let b = c("b");
        b.set.insert(c("b.s1"));
        b.set.insert(kept);
        b.set.insert(c("b.s2"));
        let _ = root.once.set(b);
        root.set.insert(c("s1"));
        root.set.insert(c("s0"));
        println("dropping root");
    }
    println(f"still {kept.name}");
}
"#;

/// With the hash seed pinned to 1: a `Set`'s order is its keys' hashes'.
const ORDER_OUT: &str =
    "dropping root\ndrop root\ndrop s1\ndrop s0\ndrop b\ndrop b.s2\ndrop b.s1\n\
                         drop a\ndrop a.once\ndrop a.lock\nstill kept\ndrop kept\n";

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-wasm-deep-drop-cells-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn karac(args: &[&str], file: &Path) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(file)
        .env("KARAC_HASH_SEED", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// `src` built for wasm and run under node: stdout, or `None` when skipped.
fn on_wasm(tag: &str, src: &str) -> Option<String> {
    let archive =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("target/release/libkarac_runtime_wasm.a");
    let node = Command::new("node").arg("--version").output();
    if !archive.exists() || !node.is_ok_and(|o| o.status.success()) {
        eprintln!("skipping mir_wasm_deep_drop_cells::{tag}: no wasm archive or node");
        return None;
    }
    let dir = scratch(tag);
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    std::fs::write(dir.join("run.mjs"), RUNNER).unwrap();
    let wasm = dir.join("main.wasm");
    let built = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["build", "--target=wasm_wasi"])
        .arg(&file)
        .arg("-o")
        .arg(&wasm)
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "wasm build {tag}: {}",
        String::from_utf8_lossy(&built.stderr)
    );
    let run = Command::new("node")
        .arg("--no-warnings")
        .arg("--no-turbo-fast-api-calls")
        .arg(dir.join("run.mjs"))
        .arg(&wasm)
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&run.stdout).into_owned();
    assert!(
        run.status.success(),
        "wasm run {tag}: {out}{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
    Some(out)
}

#[test]
fn deep_chains_through_a_mutex_a_once_lock_and_a_set_drop_on_wasm() {
    let chains = [
        (
            "mutex",
            deep("Option[Mutex[M]]", "None", "let c = Some(Mutex.new(head));"),
        ),
        (
            "once",
            deep(
                "OnceLock[M]",
                "OnceLock.new()",
                "let c: OnceLock[M] = OnceLock.new();\n        let _ = c.set(head);",
            ),
        ),
        (
            "set",
            deep(
                "Set[M]",
                "Set.new()",
                "let mut c: Set[M] = Set.new();\n        c.insert(head);",
            ),
        ),
    ];
    for (tag, src) in chains {
        if let Some(out) = on_wasm(tag, &src) {
            assert_eq!(out, "built 199999\n", "{tag}");
        }
    }
}

#[test]
fn drop_bodies_through_cells_keep_the_recursive_order_on_every_lane() {
    let dir = scratch("order");
    let file = dir.join("main.kara");
    std::fs::write(&file, ORDER).unwrap();
    assert_eq!(karac(&["__mir-run"], &file), ORDER_OUT, "interp");
    assert_eq!(karac(&["__mir-llvm-run"], &file), ORDER_OUT, "llvm");
    // The sanitizer lane: no leak, double free or bad access in the cells'
    // drained release.
    let asan = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/release/libkarac_runtime.a");
    if asan.exists() {
        assert_eq!(karac(&["__mir-llvm-asan"], &file), ORDER_OUT, "asan");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
