//! Kata 2036's fold arm carries `(plus, minus, best)` through
//! `nums.iter().skip(1).fold((nums[0], None, nums[0]), ..)`. Three gaps
//! stood in the way:
//!
//! - a bare `None` inside the tuple bound the callee's `B` to `Option[T]`'s
//!   own `T`, and the closure, checked against `B` named back, never solved
//!   it: the accumulator's `Option` is now a fresh variable the closure's
//!   tail solves;
//! - a tail `(x, ..)` with `x: ref i64` against that partly solved
//!   accumulator was refused (`fold(0, |acc, x| x)` too): a scalar `ref`
//!   reads as its value there, as it does against a concrete slot;
//! - `m.map_or(x, |k| k + 1)` with `x: ref i64` solved `U` as `ref i64`
//!   while the closure returned the value, and the interpreter crashed on
//!   `deref of non-reference` (the build lent the value out as an address):
//!   a scalar `ref` for a parameter a function argument returns is now the
//!   value.

use std::path::Path;
use std::process::{Command, Stdio};

fn karac(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Whether `src` checks clean, its check output, and what it prints under
/// the MIR interpreter.
fn run(name: &str, src: &str) -> (bool, String, String) {
    let dir = std::env::temp_dir().join(format!("karac-kata-2036-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), format!("{src}\n")).unwrap();
    let (clean, out, err) = karac(&dir, &["check", "cell.kara"]);
    let (_, printed, run_err) = karac(&dir, &["__mir-run", "cell.kara"]);
    let _ = std::fs::remove_dir_all(&dir);
    (clean, format!("{out}{err}"), format!("{printed}{run_err}"))
}

#[test]
fn kata_2036_fold_tuple_accumulator_with_a_bare_none() {
    let src = "fn main() {\n    let nums: Vec[i64] = [3, -1, 1, 2].to_vec();\n    let (_, _, best) = nums.iter().skip(1).fold((nums[0], None, nums[0]), |(plus, minus, best), x| {\n        let next_plus = minus.map_or(x, |m: i64| x.max(m + x));\n        let next_minus = plus - x;\n        (next_plus, Some(next_minus), best.max(next_plus).max(next_minus))\n    });\n    println(best);\n}";
    let (clean, check, out) = run("tuple-none", src);
    assert!(clean, "{check}");
    assert_eq!(out, "5\n");
}

#[test]
fn kata_2036_fold_from_a_bare_none_is_solved_by_the_closure() {
    let src = "fn main() {\n    let v = [1, 2, 3].to_vec();\n    let r = v.iter().fold(None, |acc, x| Some(x));\n    println(r);\n}";
    let (clean, check, out) = run("bare-none", src);
    assert!(clean, "{check}");
    assert_eq!(out, "Some(3)\n");
}

#[test]
fn kata_2036_generic_none_init_is_solved_by_the_closure() {
    let src = "fn app[A](init: own A, f: own Fn(own A) -> A) -> A {\n    f(init)\n}\n\nfn main() {\n    let r = app(None, |a| Some(1));\n    println(r);\n}";
    let (clean, check, out) = run("generic-none", src);
    assert!(clean, "{check}");
    assert_eq!(out, "Some(1)\n");
}

#[test]
fn kata_2036_fold_literal_accumulator_takes_a_scalar_ref_tail() {
    let src = "fn main() {\n    let nums: Vec[i64] = [3, -1, 1, 2].to_vec();\n    let s = nums.iter().fold(0, |acc, x| x);\n    println(s);\n}";
    let (clean, check, out) = run("literal-acc", src);
    assert!(clean, "{check}");
    assert_eq!(out, "2\n");
}

#[test]
fn kata_2036_map_or_with_a_scalar_ref_default_returns_the_value() {
    let src = "fn pick(m: Option[i64], x: ref i64) -> i64 {\n    m.map_or(x, |k: i64| k + x)\n}\n\nfn main() {\n    let v = 3;\n    println(pick(Some(2), v));\n    println(pick(None, v));\n}";
    let (clean, check, out) = run("map-or-ref", src);
    assert!(clean, "{check}");
    assert_eq!(out, "5\n3\n");
}

#[test]
fn kata_2036_map_or_with_a_scalar_ref_default_inside_a_fold() {
    let src = "fn main() {\n    let nums: Vec[i64] = [3, -1, 1, 2].to_vec();\n    let m: Option[i64] = Some(2);\n    let s = nums.iter().fold(0, |acc, x| {\n        let np = m.map_or(x, |m: i64| x.max(m + x));\n        acc + np\n    });\n    println(s);\n}";
    let (clean, check, out) = run("map-or-fold", src);
    assert!(clean, "{check}");
    assert_eq!(out, "13\n");
}

/// A closure only `map` returns keeps its tail's type: a branch that is
/// the `ref` and one that is a value still collect into a `Vec[i64]`.
#[test]
fn kata_2036_map_closure_mixing_a_ref_and_a_value_branch() {
    let src = "fn main() {\n    let nums: Vec[i64] = [3, -1, 1, 2].to_vec();\n    let even: Vec[i64] = nums.iter().enumerate().map(|(t, x)| if t % 2 == 0 { x } else { -x }).collect();\n    println(even);\n}";
    let (clean, check, out) = run("map-mixed", src);
    assert!(clean, "{check}");
    assert_eq!(out, "[3, 1, 1, -2]\n");
}

/// An annotated `acc: Option[i64]` solves `fold`'s `B` before the tail is
/// checked, so the tail `Some(x)` with `x: ref i64` reads the scalar as its
/// value (B-2026-11-09-36). It was refused with `Option[ref i64]`, and before
/// that the build printed an address for the value.
#[test]
fn kata_2036_annotated_accumulator_against_a_ref_tail_takes_the_value() {
    let src = "fn main() {\n    let v = [1, 2, 3].to_vec();\n    let r = v.iter().fold(None, |acc: Option[i64], x| Some(x));\n    println(r);\n}";
    let (clean, check, out) = run("annotated-ref", src);
    assert!(clean, "{check}");
    assert_eq!(out, "Some(3)\n");
}
