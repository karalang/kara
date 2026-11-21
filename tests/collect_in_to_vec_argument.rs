//! Found writing kata 2713 (Maximum Strictly Increasing Cells in a Matrix).
//!
//! B-2026-11-20-138: `f([(0..n).map(g).collect()].to_vec())`, a `collect()` as the
//! only element of an array literal turned `to_vec()` and handed to a
//! `Vec[Vec[i64]]` parameter, passed `karac check` with its target left
//! open and was refused at lowering, "an expression whose type does not
//! lower (Type(UnknownParam("C")))". The bound `let rows: Vec[Vec[i64]] =
//! [...].to_vec();` already solved it. A literal whose every element waits
//! for a type now takes the parameter's element type, as a `Str` element
//! does (B-2026-11-09-49).

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static N: AtomicUsize = AtomicUsize::new(0);

fn check(src: &str) -> String {
    let n = N.fetch_add(1, Ordering::SeqCst);
    let path = std::env::temp_dir().join(format!(
        "karac-collect-to-vec-arg-{}-{n}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", path.to_str().unwrap()])
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr)
}

fn run(src: &str) -> String {
    karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output
}

const ONE_COLLECT: &str = "fn shape(grid: Vec[Vec[i64]]) -> i64 {
    grid.len() * 100 + grid[0].len()
}

fn main() {
    println(f\"{shape([(0..5).map(|_| 7).collect()].to_vec())}\");
}
";

const TWO_COLLECTS: &str = "fn total(grid: Vec[Vec[i64]]) -> i64 {
    let mut sum = 0;
    for row in grid.iter() {
        for v in row.iter() {
            sum += v;
        }
    }
    sum
}

fn main() {
    let n = total([(0..3).map(|i| i * 10).collect(), (1..4).collect()].to_vec());
    println(f\"{n}\");
}
";

const STRINGS: &str = "fn lengths(rows: Vec[Vec[String]]) -> i64 {
    rows[0].len() + rows[1].len()
}

fn main() {
    println(f\"{lengths([\"a b c\".split(\" \").map(|w| w.to_string()).collect(), []].to_vec())}\");
}
";

#[test]
fn a_lone_collect_in_to_vec_takes_the_parameters_element_type() {
    assert_eq!(check(ONE_COLLECT).trim(), "All checks passed.");
    assert_eq!(run(ONE_COLLECT), "105\n");
}

#[test]
fn two_collects_in_to_vec_take_the_parameters_element_type() {
    assert_eq!(check(TWO_COLLECTS).trim(), "All checks passed.");
    assert_eq!(run(TWO_COLLECTS), "36\n");
}

#[test]
fn a_collect_beside_an_empty_literal_takes_the_parameters_element_type() {
    assert_eq!(check(STRINGS).trim(), "All checks passed.");
    assert_eq!(run(STRINGS), "3\n");
}
