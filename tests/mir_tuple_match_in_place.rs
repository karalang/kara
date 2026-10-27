//! A `match` on a tuple of locals whose arms only test the parts and copy
//! `Copy` values out of them (kata 558's `match (top_left, .., bottom_right)`
//! over `shared enum` handles) matches each part where it lies: no tuple is
//! built, so no handle is counted into one and released after the match.
//! Arms that bind a handle, a `ref`, or run a guard that is more than
//! operators over names still match a built tuple. The output is the same
//! either way, on the MIR interpreter and on the built binary.
//! B-2026-10-24-11.
#![cfg(feature = "llvm")]

mod common;

use std::path::PathBuf;
use std::process::{Command, Output};

const SRC: &str = r#"
shared enum Quad {
    Leaf(bool),
    Split(Quad, Quad, Quad, Quad),
}

fn leaf(n: i64) -> Quad {
    Quad.Leaf(n % 2 == 0)
}

fn merge(n: i64) -> Quad {
    let tl = leaf(n);
    let tr = leaf(n + 2);
    let bl = leaf(n + 4);
    let br = leaf(n + 1);
    match (tl, tr, bl, br) {
        (Quad.Leaf(a), Quad.Leaf(b), Quad.Leaf(c), Quad.Leaf(d)) => {
            if a == b and b == c and c == d {
                return Quad.Leaf(a);
            }
        }
        _ => {}
    }
    Quad.Split(tl, tr, bl, br)
}

fn guarded(n: i64) -> i64 {
    let x = leaf(n);
    let y = leaf(n + 2);
    match (x, y) {
        (Quad.Leaf(p), Quad.Leaf(q)) if p == q and p => 2,
        (Quad.Leaf(p), _) if p == false => 1,
        _ => 0,
    }
}

fn copies(n: i64) -> i64 {
    let mut i = n;
    let j = n * 2;
    let r = match (i, j) {
        (0, _) => 100,
        (k, 4) => {
            i = i + 50;
            k + i
        }
        (k, m) if k + 1 == m => k * 10,
        _ => i + j,
    };
    r + i
}

fn binds_a_handle(n: i64) -> i64 {
    let x = leaf(n);
    let y = Quad.Split(leaf(1), leaf(2), leaf(3), leaf(4));
    match (x, y) {
        (Quad.Leaf(true), Quad.Split(a, _, _, _)) => match a {
            Quad.Leaf(v) => if v { 5 } else { 6 },
            _ => 7,
        },
        (whole, _) => match whole {
            Quad.Leaf(_) => 8,
            _ => 9,
        },
    }
}

fn main() {
    let mut leaves = 0;
    let mut total = 0;
    for i in 0..9 {
        match merge(i) {
            Quad.Leaf(_) => leaves = leaves + 1,
            _ => {}
        }
        total = total + guarded(i) + copies(i) + binds_a_handle(i);
    }
    println(f"{leaves} {total}");
    println(f"{copies(0)} {copies(1)} {copies(2)} {copies(3)}");
}
"#;

const OUT: &str = "0 425\n100 11 106 12\n";

fn source(dir: &str) -> (PathBuf, PathBuf) {
    let dir =
        std::env::temp_dir().join(format!("karac-tuple-in-place-{dir}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, SRC).unwrap();
    (dir, file)
}

fn karac() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
    c.env("KARAC_HASH_SEED", "1");
    c
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// The built MIR of `name`, from `KARAC_MIR_DUMP=built`.
fn body<'a>(dump: &'a str, name: &str) -> &'a str {
    let start = dump
        .find(&format!("\nfn {name}("))
        .unwrap_or_else(|| panic!("no body for {name} in the dump"));
    let rest = &dump[start + 1..];
    &rest[..rest.find("\n}\n").unwrap_or(rest.len())]
}

#[test]
fn mir_a_tuple_of_locals_is_matched_where_its_parts_lie() {
    let (dir, file) = source("interp");
    let out = karac()
        .env("KARAC_MIR_DUMP", "built")
        .args(["__mir-run", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(String::from_utf8_lossy(&out.stdout), OUT);
    let dump = format!("\n{}", String::from_utf8_lossy(&out.stderr));
    // Matched in place: no tuple of the parts is declared, so no handle is
    // counted into one.
    for (f, tuple) in [
        (
            "merge",
            "(shared Quad, shared Quad, shared Quad, shared Quad)",
        ),
        ("guarded", "(shared Quad, shared Quad)"),
        ("copies", "(i64, i64)"),
    ] {
        let b = body(&dump, f);
        assert!(
            !b.contains(&format!(": {tuple};")),
            "{f} builds a tuple:\n{b}"
        );
    }
    // An arm that binds a whole handle still matches a built tuple.
    let b = body(&dump, "binds_a_handle");
    assert!(
        b.contains(": (shared Quad, shared Quad);"),
        "binds_a_handle:\n{b}"
    );

    let exe = dir.join("main");
    let built = karac()
        .args([
            "build",
            "--mir",
            file.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&built.stderr).to_string();
    assert!(
        built.status.success() || err.contains("Linker failed"),
        "does not build: {err}"
    );
    if common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(err)
    })
    .is_some()
    {
        let ran = Command::new(&exe).output().unwrap();
        assert!(ran.status.success(), "{}", text(&ran));
        assert_eq!(String::from_utf8_lossy(&ran.stdout), OUT);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
