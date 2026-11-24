//! `Vec.into_iter` walks a cursor over the vector it took: `next` and
//! `next_back` each read one element out through a raw pointer and move
//! their end past it, and the iterator's drop drops what is left front to
//! back, then forgets every slot with `set_len(0)`. It used to turn the
//! vector round on the first `next` (B-2026-11-24-32), and before that to
//! push every element, one at a time, into a second, growing vector.
//! Every walk gives what it gave before: front to back, back to front, the
//! two ends alternately, `drain(..).rev()`, and the elements a partial
//! walk leaves drop front to back, each once (the ASan run checks).
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

fn run(src: &str, cmd: &str) -> Option<String> {
    let dir =
        std::env::temp_dir().join(format!("karac-vec-into-iter-{cmd}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(cmd)
        .arg(&file)
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
    assert!(out.status.success(), "{cmd}: {err}");
    Some(String::from_utf8(out.stdout).unwrap())
}

const SRC: &str = "struct Loud { n: i64 }\n\
impl Drop for Loud {\n\
    fn drop(mut ref self) { print(f\"d{self.n} \"); }\n\
}\n\
fn main() {\n\
    let v = [1, 2, 3, 4, 5];\n\
    let fwd: Vec[i64] = v.clone().into_iter().collect();\n\
    let back: Vec[i64] = v.clone().into_iter().rev().collect();\n\
    println(f\"{fwd} {back}\");\n\
    let mut it = v.clone().into_iter();\n\
    let a = it.next();\n\
    let b = it.next_back();\n\
    let c = it.next();\n\
    let d = it.next_back();\n\
    println(f\"{a} {b} {c} {d} {it.len()} {it.next()} {it.next()} {it.next_back()}\");\n\
    let mut w = [10, 20, 30, 40, 50, 60].into_iter();\n\
    let _ = w.next();\n\
    let _ = w.next_back();\n\
    let rest: Vec[i64] = w.collect();\n\
    println(f\"{rest}\");\n\
    {\n\
        let mut l = [Loud { n: 1 }, Loud { n: 2 }, Loud { n: 3 }, Loud { n: 4 }].into_iter();\n\
        let first = l.next();\n\
        print(\"took \");\n\
        drop(first);\n\
        println(\"then:\");\n\
    }\n\
    println(\"\");\n\
    {\n\
        let mut l = [Loud { n: 1 }, Loud { n: 2 }, Loud { n: 3 }, Loud { n: 4 }].into_iter();\n\
        let last = l.next_back();\n\
        drop(last);\n\
        println(\"then:\");\n\
    }\n\
    println(\"\");\n\
    let mut u = [1, 2, 3, 4, 5, 6, 7];\n\
    let mid: Vec[i64] = u.drain(2..5).rev().collect();\n\
    println(f\"{mid} {u}\");\n\
    let s: Vec[String] = [\"x\".to_string(), \"y\".to_string()].into_iter().map(|t| t + \"!\").collect();\n\
    println(f\"{s}\");\n\
}\n\
";

#[test]
fn vec_into_iter_walks_either_end_and_drops_what_is_left_in_order() {
    for cmd in ["__mir-run", "__mir-llvm-run", "__mir-llvm-asan"] {
        if let Some(out) = run(SRC, cmd) {
            assert_eq!(out, "[1, 2, 3, 4, 5] [5, 4, 3, 2, 1]\nSome(1) Some(5) Some(2) Some(4) 1 Some(3) None None\n[20, 30, 40, 50]\ntook d1 then:\nd2 d3 d4 \nd4 then:\nd1 d2 d3 \n[5, 4, 3] [1, 2, 6, 7]\n[x!, y!]\n", "{cmd}");
        }
    }
}
