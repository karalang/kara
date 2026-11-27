//! B-2026-11-26-2: a `for` over a list literal, or over its `.iter()`,
//! leaves the list's numbers open for the loop's uses to fix (design.md §8
//! Literals), as `let v = [1, 2]; for a in v` does. They were settled to
//! `i64` when the loop walked the literal, so `for a in [1, 2] { take(a) }`
//! with `take(x: i32)` was refused and needed `1i32`.

use std::path::PathBuf;
use std::process::Command;

fn program(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-for-literal-open-{name}-{}",
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

// Kata 1396's harness replays LeetCode's example through `i32` parameters;
// `[250, 255]` takes `u8` from `take_u8`; a loop no use fixes keeps `i64`.
const LOOPS: &str = "struct B {
    total: i32,
}

impl B {
    fn check_in(mut ref self, id: i32, station_name: String, t: i32) {
        self.total += id + t;
        println(f\"{station_name} {id} {t}\");
    }
}

fn take(x: i32) -> i32 {
    x + 1
}

fn take_u8(x: u8) -> u8 {
    x
}

fn main() {
    let mut b = B { total: 0 };
    for (rider, start, end) in [(10, 3, 8), (5, 10, 16), (2, 21, 30)] {
        b.check_in(rider, \"Leyton\".to_string(), start);
        b.check_in(rider, \"Paradise\".to_string(), end);
    }
    println(b.total);
    for a in [1, 2] {
        println(take(a));
    }
    for (a, c) in [(1, 2), (3, 4)] {
        println(take(a) + take(c));
    }
    for a in [5, 6].iter() {
        println(take(a));
    }
    for a in [250, 255] {
        println(take_u8(a));
    }
    let mut n = 0;
    for a in [7, 8] {
        n += a;
    }
    println(n);
}
";

// Worked by hand: the riders' ids and times sum to 10+3+10+8 + 5+10+5+16
// + 2+21+2+30 = 122.
const LOOPS_EXPECTED: &str = "Leyton 10 3
Paradise 10 8
Leyton 5 10
Paradise 5 16
Leyton 2 21
Paradise 2 30
122
2
3
5
9
6
7
250
255
15
";

#[test]
fn a_for_over_a_list_literal_leaves_its_numbers_open_interpreted() {
    let file = program("interp", LOOPS);
    let f = file.to_str().unwrap();
    let (ok, out) = karac(&["check", f]);
    assert!(ok, "{out}");
    let (ok, out) = karac(&["__mir-run", f]);
    assert!(ok, "{out}");
    assert_eq!(out, LOOPS_EXPECTED);
}

#[cfg(feature = "llvm")]
#[test]
fn a_for_over_a_list_literal_leaves_its_numbers_open_compiled() {
    let file = program("run", LOOPS);
    let (ok, out) = karac(&["run", file.to_str().unwrap()]);
    assert!(ok, "{out}");
    assert_eq!(out, LOOPS_EXPECTED);
}

#[test]
fn a_loop_literal_is_range_checked_against_the_type_its_use_fixes() {
    let file = program(
        "range",
        "fn take_u8(x: u8) -> u8 {\n    x\n}\n\nfn main() {\n    for a in [250, 300] {\n        println(take_u8(a));\n    }\n}\n",
    );
    let (ok, out) = karac(&["check", file.to_str().unwrap()]);
    assert!(!ok, "{out}");
    assert!(
        out.contains("integer literal 300 out of range for 'u8'"),
        "{out}"
    );
}
