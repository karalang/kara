//! B-2026-11-26-9: a value only its slot can type (a `collect()`, a
//! `Vec.filled` of a literal, an empty `[]`, or a tuple holding one) stored
//! by `Vec.insert`, `VecDeque.push_back` or `VecDeque.push_front` is checked
//! against the element type, as `push` checks it. The three inferred the
//! value alone, so `v.insert(0, it.collect())` passed the checker with the
//! collect's target unsolved and stopped at lowering (E0516), and
//! `d.push_back(Vec.filled(3, 0))` into a `VecDeque[Vec[u64]]` was refused as
//! a `Vec[i64]`.

use std::path::PathBuf;
use std::process::Command;

fn program(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-element-store-open-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    file
}

fn karac(args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
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

const STORES: &str = "fn main() {
    let mut v: Vec[(Vec[i32], i32)] = Vec.new();
    v.insert(0, ((0..3).map(|x| x + 1).collect(), 5));
    v.insert(0, ((0..2).map(|_| -7).collect(), 9));
    let mut d: VecDeque[Vec[i32]] = VecDeque.new();
    d.push_back((0..3).map(|x| x * 10).collect());
    d.push_front((0..2).map(|_| 4).collect());
    let mut f: VecDeque[Vec[u64]] = VecDeque.new();
    f.push_back(Vec.filled(3, 18446744073709551615));
    let mut e: Vec[Vec[i32]] = Vec.new();
    e.insert(0, []);
    e.insert(1, (0..4).map(|x| x).collect());
    for (xs, n) in v.iter() {
        let s: i32 = xs.iter().sum();
        println(f\"{s} {n}\");
    }
    for xs in d.iter() {
        println(f\"{xs.len()} {xs[xs.len() - 1]}\");
    }
    println(f\"{f[0][2]} {e[0].len()} {e[1][3]}\");
}
";

// Worked by hand: `v` is [([-7, -7], 9), ([1, 2, 3], 5)] after the two
// inserts at 0, so the sums are -14 and 6; `d` is [[4, 4], [0, 10, 20]];
// `f[0][2]` is u64's MAX; `e` is [[], [0, 1, 2, 3]].
const STORES_EXPECTED: &str = "-14 9
6 5
2 4
3 20
18446744073709551615 0 3
";

#[test]
fn insert_and_push_back_check_an_open_value_against_the_slot_interpreted() {
    let file = program("interp", STORES);
    let f = file.to_str().unwrap();
    let (ok, out) = karac(&["check", f]);
    assert!(ok, "{out}");
    let (ok, out) = karac(&["__mir-run", f]);
    assert!(ok, "{out}");
    assert_eq!(out, STORES_EXPECTED);
}

#[cfg(feature = "llvm")]
#[test]
fn insert_and_push_back_check_an_open_value_against_the_slot_compiled() {
    let file = program("run", STORES);
    let (ok, out) = karac(&["run", file.to_str().unwrap()]);
    assert!(ok, "{out}");
    assert_eq!(out, STORES_EXPECTED);
}

#[test]
fn an_inserted_collect_of_the_wrong_element_is_refused_at_the_slot() {
    // Before the fix the checker passed this and lowering stopped on the
    // unsolved collect target; the slot now names the mismatch.
    let file = program(
        "wrong",
        "fn main() {\n    let mut v: Vec[(Vec[i32], i32)] = Vec.new();\n    v.insert(0, ((0..3).map(|_| \"a\").collect(), 5));\n    println(v.len());\n}\n",
    );
    let (ok, out) = karac(&["check", file.to_str().unwrap()]);
    assert!(!ok, "{out}");
    assert!(
        out.contains("`Vec[i32]` implements `FromIterator[i32]`, not `FromIterator[String]`"),
        "{out}"
    );
    assert!(!out.contains("E0516"), "{out}");
}

#[test]
fn a_fill_pushed_back_is_range_checked_against_its_slot() {
    let file = program(
        "range",
        "fn main() {\n    let mut f: VecDeque[Vec[u64]] = VecDeque.new();\n    f.push_back(Vec.filled(3, -1));\n    println(f.len());\n}\n",
    );
    let (ok, out) = karac(&["check", file.to_str().unwrap()]);
    assert!(!ok, "{out}");
    assert!(
        out.contains("negative integer literal -1 cannot initialize unsigned type 'u64'"),
        "{out}"
    );
}
