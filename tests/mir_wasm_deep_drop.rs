//! B-2026-11-16-311: dropping a deep `shared` tree (two or more handles of
//! its own type, `left` and `right`) recursed once per level, and on
//! wasm_wasi node's engine stack ended a 20000-deep one ("Maximum call stack
//! size exceeded"). The release now keeps the boxes still to release on a
//! stack held in their own handle slots, so the depth costs no frames, and
//! `Drop` bodies run in the order the recursive release ran them.
//!
//! The wasm half skips (with a notice) when the wasm archive or node is
//! missing, as `tests/mir_wasm.rs` does.
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

/// A left chain of 200000 two-child nodes, dropped at the end of `main`.
const DEEP: &str = r#"shared struct T { mut v: i64, mut left: Option[T], mut right: Option[T] }

fn main() {
    let head = T { v: 0, left: None, right: None };
    let mut cur = head;
    for i in 1..200000 {
        let c = T { v: i, left: None, right: None };
        cur.left = Some(c);
        cur = c;
    }
    println(f"built {head.v}");
}
"#;

/// Drop bodies over a tree with a subtree another handle keeps, and a
/// deep three-child tree with no `Drop` body.
const ORDER: &str = r#"shared struct N { name: String, mut left: Option[N], mut right: Option[N] }

impl Drop for N {
    fn drop(mut ref self) {
        println(f"drop {self.name}");
    }
}

shared struct P { v: i64, mut a: Option[P], mut b: Option[P], mut c: Option[P] }

fn leaf(s: own String) -> N { N { name: s, left: None, right: None } }

fn main() {
    let kept = leaf("kept");
    {
        let root = N { name: "root", left: None, right: None };
        let l = N { name: "l", left: Some(leaf("ll")), right: Some(kept) };
        let r = N { name: "r", left: Some(leaf("rl")), right: Some(N { name: "rr", left: None, right: Some(leaf("rrr")) }) };
        root.left = Some(l);
        root.right = Some(r);
        println("dropping root");
    }
    println(f"still {kept.name}");
    let top = P { v: 0, a: None, b: None, c: None };
    let mut cur = top;
    for i in 1..30000 {
        let n = P { v: i, a: Some(P { v: -i, a: None, b: None, c: None }), b: None, c: Some(P { v: i * 2, a: None, b: None, c: None }) };
        cur.b = Some(n);
        cur = n;
    }
    println(f"built {top.v}");
}
"#;

const ORDER_OUT: &str =
    "dropping root\ndrop root\ndrop r\ndrop rr\ndrop rrr\ndrop rl\ndrop l\ndrop ll\n\
                         still kept\nbuilt 0\ndrop kept\n";

fn scratch(tag: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("karac-wasm-deep-drop-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn karac(args: &[&str], file: &Path) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(file)
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
        eprintln!("skipping mir_wasm_deep_drop::{tag}: no wasm archive or node");
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
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let run = Command::new("node")
        .arg("--no-warnings")
        .arg("--no-turbo-fast-api-calls")
        .arg(dir.join("run.mjs"))
        .arg(&wasm)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        run.status.success(),
        "{tag}: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    Some(String::from_utf8_lossy(&run.stdout).into_owned())
}

#[test]
fn wasm_drops_a_deep_two_child_chain() {
    if let Some(out) = on_wasm("deep", DEEP) {
        assert_eq!(out, "built 0\n");
    }
}

#[test]
fn tree_release_keeps_the_drop_order_on_every_lane() {
    let dir = scratch("order");
    let file = dir.join("main.kara");
    std::fs::write(&file, ORDER).unwrap();
    assert_eq!(karac(&["__mir-run"], &file), ORDER_OUT, "interp");
    assert_eq!(karac(&["__mir-llvm-run"], &file), ORDER_OUT, "llvm");
    let _ = std::fs::remove_dir_all(&dir);
    if let Some(out) = on_wasm("order", ORDER) {
        assert_eq!(out, ORDER_OUT, "wasm");
    }
}
