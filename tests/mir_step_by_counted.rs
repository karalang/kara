//! B-2026-11-12-17: `for i in (a..b).step_by(k)` over `i64` went through
//! the library's `IterStepBy[Range[i64]].next` (a `first` flag,
//! `advance_by(k - 1)` and two `Option`s) per item, about 1.2x the same
//! loop written with `while` (kata 1144). It is now a counting loop that
//! steps while the distance left exceeds the step. Each line pairs the
//! loop with the library iterator's own `collect`, so the two agree,
//! including a step below 1, an empty range and the `i64` extremes.

use std::process::{Command, Stdio};

const SRC: &str = r#"fn walk(r: Vec[i64]) -> String {
    let mut s = String.new();
    for x in r {
        s.push_str(f" {x}");
    }
    s
}

fn main() {
    let n = 10;
    let mut a: Vec[i64] = Vec.new();
    for i in (0..n).step_by(3) { a.push(i); }
    println(f"{walk(a)} |{walk((0..n).step_by(3).collect())}");
    let mut b: Vec[i64] = Vec.new();
    for i in (0..=9).step_by(3) { b.push(i); }
    println(f"{walk(b)} |{walk((0..=9).step_by(3).collect())}");
    let mut c: Vec[i64] = Vec.new();
    for i in (1..=n).step_by(n) { c.push(i); }
    println(f"{walk(c)} |{walk((1..=n).step_by(n).collect())}");
    let mut d: Vec[i64] = Vec.new();
    for i in (2..6).step_by(0) { d.push(i); }
    for i in (2..5).step_by(-4) { d.push(i); }
    println(f"{walk(d)} |{walk((2..6).step_by(0).collect())}{walk((2..5).step_by(-4).collect())}");
    let mut e: Vec[i64] = Vec.new();
    for i in (5..5).step_by(2) { e.push(i); }
    for i in (6..=5).step_by(2) { e.push(i); }
    for _ in (0..7).step_by(2) { e.push(-1); }
    println(f"{walk(e)} |{walk((5..5).step_by(2).collect())}");
    let mut f: Vec[i64] = Vec.new();
    for i in (9223372036854775800..9223372036854775807).step_by(3) { f.push(i); }
    for i in (9223372036854775800..=9223372036854775807).step_by(7) { f.push(i); }
    for i in (-9223372036854775807 - 1..9223372036854775807).step_by(9223372036854775807) { f.push(i); }
    println(walk(f));
    let mut g: Vec[i64] = Vec.new();
    'outer: for i in (0..100).step_by(7) {
        if i % 2 == 1 { continue; }
        for j in (0..i).step_by(5) {
            if j > 10 { continue 'outer; }
            g.push(j);
        }
        if i > 40 { break; }
        g.push(-i);
    }
    println(walk(g));
}
"#;

const WANT: &str = r#" 0 3 6 9 | 0 3 6 9
 0 3 6 9 | 0 3 6 9
 1 | 1
 2 3 4 5 2 3 4 | 2 3 4 5 2 3 4
 -1 -1 -1 -1 |
 9223372036854775800 9223372036854775803 9223372036854775806 9223372036854775800 9223372036854775807 -9223372036854775808 -1 9223372036854775806
 0 0 5 10 -14 0 5 10 0 5 10 0 5 10 0 5 10 0 5 10 0 5 10
"#;

fn run(sub: &str, tag: &str) -> (String, String) {
    let dir = std::env::temp_dir().join(format!("karac-step-by-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), SRC).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args([sub, "cell.kara"])
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn step_by_range_loop_interpreted() {
    let (out, err) = run("__mir-run", "interp");
    assert_eq!(out, WANT, "{err}");
}

#[test]
fn step_by_range_loop_compiled() {
    if !cfg!(feature = "llvm") {
        return;
    }
    let (out, err) = run("__mir-llvm-run", "llvm");
    if err.contains("Linker failed") && std::env::var_os("KARAC_REQUIRE_RUNTIME_ARCHIVE").is_none()
    {
        eprintln!("skipping: no runtime archive");
        return;
    }
    assert_eq!(out, WANT, "{err}");
}
