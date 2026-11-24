//! B-2026-11-24-24 (kata 2447): `[(x, 1)].to_vec()` into a `Vec[(i64, i64)]`, with
//! `x` the `ref i64` that `nums.iter()` yields, was refused as a
//! `Vec[(ref i64, i64)]`, though `[x].to_vec()` into a `Vec[i64]` and
//! `(x, 1)` into an `(i64, i64)` read the `Copy` value through its
//! reference. A literal of tuples now takes the slot's tuple of scalars,
//! which also types its number literals: `[(1, 2)].to_vec()` into a
//! `Vec[(i32, i32)]` was refused as a `Vec[(i64, i64)]`.

use std::path::PathBuf;
use std::process::Command;

fn program(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-k2447-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    f
}

fn karac(args: &[&str], f: &PathBuf) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(f)
        .output()
        .expect("karac");
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

#[test]
fn a_tuple_literal_holding_a_ref_copy_value_takes_the_slots_tuple() {
    let f = program(
        "ref",
        "fn main() {
    let nums = [3, 4].to_vec();
    for x in nums.iter() {
        let next: Vec[(i64, i64)] = [(x, 1)].to_vec();
        println(f\"{next}\");
    }
}
",
    );
    let (ok, out) = karac(&["check"], &f);
    assert!(ok, "{out}");
    let (ok, out) = karac(&["run", "--interp"], &f);
    assert!(ok, "{out}");
    assert_eq!(out, "[(3, 1)]\n[(4, 1)]\n");
}

#[test]
fn a_tuple_literals_numbers_take_the_slots_width() {
    let f = program(
        "width",
        "fn main() {
    let nums: Vec[i32] = [5i32, 6].to_vec();
    let a: Vec[(i32, i32)] = [(1, 2), (3, 4)].to_vec();
    for x in nums.iter() {
        let b: Vec[(i32, bool)] = [(x, true), (2, false)].to_vec();
        println(f\"{b}\");
    }
    println(f\"{a}\");
}
",
    );
    let (ok, out) = karac(&["check"], &f);
    assert!(ok, "{out}");
    let (ok, out) = karac(&["run", "--interp"], &f);
    assert!(ok, "{out}");
    assert_eq!(
        out,
        "[(5, true), (2, false)]\n[(6, true), (2, false)]\n[(1, 2), (3, 4)]\n"
    );
}

#[test]
fn a_tuple_literal_that_cannot_fit_the_slot_is_still_refused() {
    let f = program(
        "wide",
        "fn main() {
    let y: i64 = 7;
    let c: Vec[(i32, i32)] = [(1, y)].to_vec();
    println(f\"{c}\");
}
",
    );
    let (ok, out) = karac(&["check"], &f);
    assert!(!ok, "{out}");
}
