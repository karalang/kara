//! Kata 1128's harness builds its dominoes with `(0..n).map(|i| if i % 2
//! == 0 { [4, 7] } else { [7, 4] }).collect()` into a `Vec[Vec[i32]]`. The
//! closure is checked against `map`'s return parameter, which no use had
//! fixed yet, and the numbers it gives were typed `i64` there, so the
//! `collect` was refused: `Vec[Vec[i32]]` implements
//! `FromIterator[Vec[i32]]`, not `FromIterator[Vec[i64]]`. A number a
//! closure gives, or a list or tuple of them, is now open as a `let`'s
//! value is, and the `collect`'s target fixes it before the body's
//! literals take their defaults (B-2026-11-11-86).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "kata_1128_closure_literal_collect_{name}_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Whether `karac __mir-run` succeeded, its stdout, and its stderr.
fn run(name: &str, src: &str) -> (bool, String, String) {
    let dir = scratch(name);
    std::fs::write(dir.join("m.kara"), src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["__mir-run", "m.kara"])
        .current_dir(Path::new(&dir))
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn a_closure_number_takes_the_collected_item_type() {
    let src = "fn main() {
    let c = true;
    let a: Vec[i32] = (0..3).map(|i| 5).collect();
    let b: Vec[Vec[i32]] = (0..2).map(|i| [4, 7]).collect();
    let t: Vec[(i32, f32)] = (0..2).map(|i| (1, 0.5)).collect();
    let d: Vec[u8] = (0..2).map(|i| { let z = i * 2; 9 }).collect();
    let e: Vec[Vec[i32]] = (0..4).map(|i| if i % 2 == 0 { [4, 7] } else { [7, 4] }).collect();
    let f: Vec[Vec[i32]] = (0..3).map(|i| match i { 0 => [1, 2], _ => [3, 4] }).collect();
    let g: Vec[(i32, i32)] = (0..2).map(|i| if c { (4, 7) } else { (7, 4) }).collect();
    let h: Vec[f32] = (0..2).map(|i| 2.5).collect();
    println(f\"{a} {b} {t} {d}\");
    println(f\"{e} {f} {g} {h}\");
}
";
    let (ok, out, err) = run("into", src);
    assert!(ok, "{err}");
    assert_eq!(
        out,
        "[5, 5, 5] [[4, 7], [4, 7]] [(1, 0.5), (1, 0.5)] [9, 9]\n\
         [[4, 7], [7, 4], [4, 7], [7, 4]] [[1, 2], [3, 4], [3, 4]] [(4, 7), (4, 7)] [2.5, 2.5]\n"
    );
}

#[test]
fn a_later_use_fixes_a_closure_number_and_unfixed_ones_default() {
    let src = "fn main() {
    let w = (0..3).map(|i| 5).collect();
    let t: Vec[i32] = w;
    let s: i64 = (0..3).map(|i| 5).sum();
    let big = (0..2).map(|i| 5000000000).collect();
    let u: Vec[i64] = big;
    let mut seen: Vec[i16] = Vec.new();
    for k in (0..3).map(|i| -2) {
        seen.push(k);
    }
    println(f\"{t} {s} {u} {seen}\");
}
";
    let (ok, out, err) = run("later", src);
    assert!(ok, "{err}");
    assert_eq!(out, "[5, 5, 5] 15 [5000000000, 5000000000] [-2, -2, -2]\n");
}

#[test]
fn a_closure_number_past_the_collected_type_is_refused() {
    let src = "fn main() {
    let v: Vec[i32] = (0..3).map(|i| 3000000000).collect();
    println(f\"{v}\");
}
";
    let (ok, _, err) = run("wide", src);
    assert!(!ok);
    assert!(
        err.contains("integer literal 3000000000 out of range for 'i32'"),
        "{err}"
    );
}

const CLK: &str = "effect resource Clk: ClkP;
trait ClkP { fn now(ref self) -> i64; }
struct F { t: Vec[i64] }
impl ClkP for F { fn now(ref self) -> i64 { self.t[0] } }
";

#[test]
fn a_with_provider_closure_number_takes_its_default() {
    let src = format!(
        "{CLK}
fn main() {{
    let tg = TaskGroup.new();
    let h = with_provider[Clk](F {{ t: [1] }}, || tg.spawn(|| 7));
    let x = Some(8);
    let y = with_provider[Clk](F {{ t: [1] }}, || x);
    println(f\"{{h.join()}} {{y.unwrap()}}\");
}}
"
    );
    let (ok, out, err) = run("provider", &src);
    assert!(ok, "{err}");
    assert_eq!(out, "7 8\n");
}
