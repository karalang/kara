//! Literal typing rows that remained after B-2026-11-18-12 (design.md §8
//! Literals: a literal defaults only at the end of the body, after every use
//! has had its chance to fix it):
//! - B-2026-11-18-34: a list of lists keeps its literals open for a read, as
//!   a flat list does: `let g = [[1, 2], [3, 4]]; let c: i32 = g[1][0]` and
//!   the inline `[[1, 2], [3, 4]][1][k]` make the lists `i32`, where they
//!   were refused as a narrowing from `i64`.
//! - B-2026-11-18-35: an indexed inline list as an `if` branch takes the
//!   other branch's type, as a bare literal branch does: with `x: i32`,
//!   `if b { [0, 45, 90][k] } else { x }` is an `i32`.

use karac::check_source;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

fn errors(src: &str) -> Vec<String> {
    check_source(src)
        .into_iter()
        .filter(|d| d.severity == "error")
        .map(|d| d.message)
        .collect()
}

fn modes() -> Vec<&'static str> {
    let mut m = vec!["__mir-run"];
    if cfg!(feature = "llvm") {
        m.push("__mir-llvm-run");
    }
    m
}

/// `src`'s stdout under each backend, `None` for a compiled lane with no
/// runtime archive.
fn outputs(src: &str) -> Vec<(&'static str, Option<String>)> {
    static N: AtomicUsize = AtomicUsize::new(0);
    modes()
        .into_iter()
        .map(|mode| {
            let n = N.fetch_add(1, Ordering::SeqCst);
            let dir =
                std::env::temp_dir().join(format!("karac-litnest-{}-{n}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("main.kara"), src).unwrap();
            let out = Command::new(env!("CARGO_BIN_EXE_karac"))
                .args([mode, "main.kara"])
                .current_dir(&dir)
                .output()
                .unwrap();
            let _ = std::fs::remove_dir_all(&dir);
            let err = String::from_utf8_lossy(&out.stderr).into_owned();
            if mode != "__mir-run" && err.contains("runtime archive") {
                return (mode, None);
            }
            assert!(out.status.success(), "{mode}: {err}\n{src}");
            (
                mode,
                Some(String::from_utf8_lossy(&out.stdout).into_owned()),
            )
        })
        .collect()
}

fn assert_prints(src: &str, want: &str) {
    assert!(errors(src).is_empty(), "{src}\n{:#?}", errors(src));
    for (mode, out) in outputs(src) {
        if let Some(out) = out {
            assert_eq!(out, want, "{mode}\n{src}");
        }
    }
}

#[test]
fn a_list_of_lists_takes_its_read_s_type() {
    assert_prints(
        "fn take_i32(x: i32) -> i32 {\n    x * 2\n}\nfn main() {\n    let k = 1;\n    \
         let g = [[1, 2], [3, 4]];\n    let c: i32 = g[1][0];\n    \
         let d: i32 = [[1, 2], [3, 4]][1][k];\n    let fl = [[1.5, 2.5], [3.5]];\n    \
         let f: f32 = fl[0][1];\n    let big = [[3000000000, 1]];\n    \
         let h = [[[5, 6]], [[7, 8]]];\n    println(take_i32(h[1][0][1]));\n    \
         println(f\"{c} {d} {f} {big[0][0]}\");\n    let plain = [[1, 2], [3]];\n    \
         println(plain[0][0] + 3000000000);\n}\n",
        "16\n3 4 2.5 3000000000\n3000000001\n",
    );
    // The read's type range-checks each inner literal.
    let es = errors(
        "fn main() {\n    let g = [[1, 300]];\n    let b: u8 = g[0][0];\n    println(b);\n}\n",
    );
    assert_eq!(es.len(), 1, "{es:#?}");
    assert!(es[0].contains("300 out of range for 'u8'"), "{es:#?}");
}

#[test]
fn an_indexed_inline_list_branch_takes_the_other_branch_s_type() {
    assert_prints(
        "fn pick(b: bool, k: i64, x: i32) -> i32 {\n    \
         let d = if b { [0, 45, 90][k] } else { x };\n    \
         let e = if b { x } else { [1, 2, 3][k] };\n    \
         let f = if b {\n        let m = 1;\n        [[7, 8], [9, 10]][m][k - 1]\n    } else {\n        \
         x\n    };\n    d + e + f\n}\nfn main() {\n    println(pick(true, 2, 5));\n    \
         println(pick(false, 2, 5));\n    \
         let w = if true { [3000000000, 1][0] } else { 2 };\n    println(w);\n}\n",
        "105\n13\n3000000000\n",
    );
    let es = errors(
        "fn pick(b: bool, k: i64, x: u8) -> u8 {\n    if b { [0, 300][k] } else { x }\n}\n\
         fn main() {\n    println(pick(true, 1, 5));\n}\n",
    );
    assert_eq!(es.len(), 1, "{es:#?}");
    assert!(es[0].contains("300 out of range for 'u8'"), "{es:#?}");
}
