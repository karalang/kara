//! An iterator chain over an `Array` constant (`MX.iter().filter(..)
//! .map(..).collect()`) returned from a function was refused as borrowing a
//! temporary (B-2026-11-15-120): `MX.iter()` parses as a path call, and the
//! constant was built in a temporary for it, though a view of a constant
//! borrows nothing (core semantics §5.3). A `ref self` method now takes the
//! constant from its static, as `for x in MX` already did.

use std::process::{Command, Output};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .output()
        .unwrap()
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn an_iterator_chain_over_a_constant_returns_its_views() {
    let dir = std::env::temp_dir().join(format!("karac-const-chain-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("const-chain.kara");
    std::fs::write(&path, "const MX: Array[(Str, u16, Str), 3] = [(\"a.example\", 10, \"1.1.1.1\"), (\"b.example\", 20, \"2.2.2.2\"), (\"c.example\", 3, \"3.3.3.3\")];
const NAMES: Array[Str, 3] = [\"x\", \"y\", \"z\"];
fn hosts() -> Vec[(u16, Str)] {
    MX.iter().filter(|m| m.1 > 5).map(|m| (m.1, m.2)).collect()
}
fn names() -> Vec[Str] {
    let it = NAMES.iter();
    it.collect()
}
fn first() -> Str { NAMES.iter().next().unwrap() }
fn main() {
    let hs = hosts();
    for (p, h) in hs { println(f\"{p} {h}\"); }
    println(names().join(\",\"));
    println(first());
    println(f\"{NAMES.len()} {MX.len()}\");
    let mut c = NAMES;
    c[0] = \"w\";
    println(f\"{c[0]} {NAMES[0]}\");
}
").unwrap();
    let check = karac(&["check"], &path);
    assert!(check.status.success(), "{}", text(&check));
    let run = karac(&["__mir-run"], &path);
    assert!(run.status.success(), "{}", text(&run));
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "10 1.1.1.1\n20 2.2.2.2\nx,y,z\nx\n3 3\nw x\n"
    );
}
