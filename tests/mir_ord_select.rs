//! A comparator's `Ordering` reaches the library's sort as -1 / 0 / 1 built
//! with a `select` of constants, not `zext(gt) - zext(lt)`: LLVM folds the
//! sort's `< 0` back through the select to the one compare, and does not
//! fold the subtraction, which cost kata 253's partition loop five
//! instructions per element on arm64 (B-2026-11-02-7). The pins: a stable
//! `sort_by` over all three orderings prints the same under both backends,
//! and the comparator adapter's IR has the select and no subtraction.
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const SRC: &str = r#"fn main() {
    let mut v: Vec[(i64, i64)] = Vec.new();
    let mut i = 0;
    while i < 12 {
        v.push(((i * 7) % 5, i));
        i = i + 1;
    }
    v.sort_by(|a, b| a.0.cmp(b.0));
    for p in v {
        print(f"{p.0}:{p.1} ");
    }
    println("");
    v.sort_by(|a, b| b.1.cmp(a.1));
    for p in v {
        print(f"{p.1} ");
    }
    println("");
}
"#;

/// Stable by the first field, then descending by the second.
const EXPECTED: &str = "0:0 0:5 0:10 1:3 1:8 2:1 2:6 2:11 3:4 3:9 4:2 4:7 \n\
                        11 10 9 8 7 6 5 4 3 2 1 0 \n";

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("karac-ord-select-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn karac() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
    c.env("KARAC_AUTO_PAR", "0");
    c
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}

/// The bodies of the comparator adapters (`sort_by.closure#N()`) in `ll`.
fn adapters(ll: &Path) -> Vec<Vec<String>> {
    let ir = std::fs::read_to_string(ll).unwrap();
    let mut found = Vec::new();
    let mut lines = ir.lines();
    while let Some(l) = lines.next() {
        if l.starts_with("define") && l.contains("@\"sort_by.closure#") {
            found.push(
                lines
                    .by_ref()
                    .take_while(|l| *l != "}")
                    .map(str::to_string)
                    .collect(),
            );
        }
    }
    found
}

#[test]
fn a_comparators_order_is_a_select_the_sort_test_folds() {
    let d = dir("pin");
    let file = d.join("main.kara");
    std::fs::write(&file, SRC).unwrap();
    let interp = karac()
        .args(["__mir-run", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(
        out(&interp),
        EXPECTED,
        "{}",
        String::from_utf8_lossy(&interp.stderr)
    );
    let exe = d.join("main");
    let ll = d.join("main.ll");
    let built = karac()
        .env("KARAC_MIR_LL", &ll)
        .args(["build", file.to_str().unwrap(), "-o", exe.to_str().unwrap()])
        .output()
        .unwrap();
    let why = String::from_utf8_lossy(&built.stderr).to_string();
    if common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(why)
    })
    .is_none()
    {
        return;
    }
    let ran = Command::new(&exe).output().unwrap();
    assert_eq!(out(&ran), EXPECTED);
    let found = adapters(&ll);
    assert!(!found.is_empty(), "no comparator adapter in the IR");
    for body in &found {
        let text = body.join("\n");
        assert!(text.contains("select i1"), "{text}");
        assert!(!text.contains("sub i32"), "{text}");
    }
    let _ = std::fs::remove_dir_all(&d);
}
