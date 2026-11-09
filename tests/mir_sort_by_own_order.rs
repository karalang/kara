//! A `sort_by` whose comparator is the elements' own order or its reverse
//! (`|a, b| a.cmp(b)`, `|a, b| b.cmp(a)`, or a function written so) over
//! integers, `char`s or `bool`s runs the runtime's integer sort, reversed
//! for the descending order, rather than the merge sort that calls the
//! comparator per pair: kata 1058's descending sort ran at about 1.9x
//! Rust's `sort_by` (B-2026-11-06-34). Such a comparator calls nothing and
//! equal elements are the same bits, so the result is the merge sort's.
//! The pins: every shape prints the same under the interpreter and the
//! build, the recognised ones call `karac_mir_sort_int`, and a comparator
//! that is some other order keeps the merge sort.
#![cfg(feature = "llvm")]

mod common;

use std::path::PathBuf;
use std::process::{Command, Output};

const SRC: &str = r#"fn desc(a: ref i32, b: ref i32) -> Ordering { b.cmp(a) }

fn main() {
    let mut v: Vec[i64] = [3, -1, 2, 2, 9, -7, 0];
    v.sort_by(|a, b| b.cmp(a));
    println(v);
    let mut w: Vec[i64] = [3, -1, 2, 2, 9, -7, 0];
    w.sort_by(|a, b| a.cmp(b));
    println(w);
    let mut u: Vec[u8] = [200, 3, 255, 0];
    u.sort_by(|x, y| y.cmp(x));
    println(u);
    let mut c: Vec[char] = ['b', 'z', 'a'];
    c.sort_by(|x, y| y.cmp(x));
    println(c);
    let mut f: Vec[i32] = [5, 1, 4];
    f.sort_by(desc);
    println(f);
    let mut bs: Vec[bool] = [true, false, true];
    bs.sort_by(|a, b| b.cmp(a));
    println(bs);
    let mut e: Vec[i64] = [];
    e.sort_by(|a, b| b.cmp(a));
    println(e);
    let mut parity: Vec[i64] = [1, 2, 3, 4];
    parity.sort_by(|a, b| (a % 2).cmp(b % 2));
    println(parity);
}
"#;

const EXPECTED: &str = "[9, 3, 2, 2, 0, -1, -7]\n\
                        [-7, -1, 0, 2, 2, 3, 9]\n\
                        [255, 200, 3, 0]\n\
                        [z, b, a]\n\
                        [5, 4, 1]\n\
                        [true, true, false]\n\
                        []\n\
                        [2, 4, 1, 3]\n";

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("karac-sort-by-own-{name}-{}", std::process::id()));
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

#[test]
fn sort_by_with_the_elements_own_order_uses_the_integer_sort() {
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
    let ir = std::fs::read_to_string(&ll).unwrap();
    // The seven recognised sorts (the empty one included) call the integer
    // sort;
    // the parity comparator keeps the merge sort.
    let calls = ir
        .lines()
        .filter(|l| l.contains("call") && l.contains("@karac_mir_sort_int"))
        .count();
    assert_eq!(calls, 7, "{ir}");
    assert!(
        ir.contains("@\"merge_sort.by."),
        "the parity sort's merge sort is gone"
    );
    let _ = std::fs::remove_dir_all(&d);
}
