//! A list literal iterated where it is written keeps its number literals
//! open, as `let v = [0];` does, so the end of the chain fixes them:
//! `let v: Vec[i32] = [0].into_iter().chain([1, 2].into_iter()).collect();`
//! was refused as `C: FromIterator[i64]` (B-2026-11-11-41, kata 1118).
//!
//! The same chain with a `scan` on one side was worse: with the head bound
//! by `let`, the `scan`'s state literal settled to `i64` before the
//! `FromIterator` bound was discharged, the bound was then skipped, and a
//! `Vec[i32]` was built from `i64` items (eight bytes written into each
//! four-byte slot: garbage compiled, an AddressSanitizer abort). It was
//! refused next, and since B-2026-11-11-61 the state literal stays open
//! until the `collect` fixes it, so the chain builds `i32` items.

use std::process::{Command, Output, Stdio};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn temp(tag: &str, src: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "karac-literal-chain-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    path
}

#[test]
fn a_chain_of_literal_lists_collects_into_the_annotated_width() {
    let src = "fn main() {\n    let v: Vec[i32] = [0].into_iter().chain([1, 2].into_iter()).collect();\n    \
               println(v);\n    let w: Vec[i32] = [0].into_iter().chain([1i32].into_iter()).collect();\n    \
               println(w);\n    let x: Vec[u8] = [7].into_iter().chain([250].into_iter()).collect();\n    \
               println(x);\n    let y: Vec[i32] = [5].into_iter().collect();\n    println(y);\n}\n";
    let path = temp("lists", src);
    for lane in [&["run"][..], &["run", "--interp"][..]] {
        let out = karac(lane, &path);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "[0, 1, 2]\n[0, 1]\n[7, 250]\n[5]\n",
            "{lane:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_scan_whose_state_is_a_literal_in_a_chain_collects_i32_items() {
    for (tag, head) in [("let-head", "head"), ("literal-head", "[0]")] {
        let src = format!(
            "fn main() {{\n    let lengths = [31, 28, 31];\n    let head = [0];\n    \
             let starts: Vec[i32] = {head}.into_iter().chain(lengths.iter().scan(0, |total, d| {{\n        \
             let total = total + d;\n        Some((total, total))\n    }})).collect();\n    \
             println(starts);\n}}\n"
        );
        let path = temp(tag, &src);
        for lane in [&["run"][..], &["run", "--interp"][..]] {
            let out = karac(lane, &path);
            assert_eq!(
                String::from_utf8_lossy(&out.stdout),
                "[0, 31, 59, 90]\n",
                "{tag} {lane:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        let _ = std::fs::remove_file(&path);
    }
}
