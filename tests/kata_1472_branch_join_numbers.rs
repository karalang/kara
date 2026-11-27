//! B-2026-11-27-17: an `if` or `match` whose branches give tuples, `Option`s or
//! lists of different number types joined at its first branch, and a value
//! of another branch was narrowed into it silently. `if c { (r, steps) }
//! else { (r, r) }` with `steps: i32` and `r = 3000000000` printed
//! -1294967296 for `.1`, and `if c { [0, 0].to_vec() } else { [steps,
//! 7].to_vec() }` read the `i32` list as `i64`s on the compiled lane
//! (34359738365 where the interpreter printed -3). Kata 1472's differential
//! met the other face of it: a branch's tuple of literals, `ops.push(if r
//! == 0 { (0, 0, s) } else { (r, steps, s) })` into a `Vec[(i64, i32,
//! String)]`, settled its numbers at `i64` where they were written and was
//! refused. Such a join is now refused as two different number types are
//! at the top (design.md § Least upper bound, rule 4), and a branch's tuple
//! of literals takes its numbers from the sibling branch.

use std::path::PathBuf;
use std::process::Command;

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

fn fixture(tag: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-branch-join-numbers-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("p.kara");
    std::fs::write(&path, src).unwrap();
    (dir, path)
}

/// `karac check`'s success and output, and `karac __mir-run`'s stdout.
fn check_and_run(tag: &str, src: &str) -> (bool, String, String) {
    let (dir, path) = fixture(tag, src);
    let check = karac().arg("check").arg(&path).output().unwrap();
    let run = karac().arg("__mir-run").arg(&path).output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        check.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&check.stdout),
            String::from_utf8_lossy(&check.stderr)
        ),
        String::from_utf8_lossy(&run.stdout).into_owned(),
    )
}

/// Kata 1472's form, both ways round and in a `match`, and an `if` of
/// tuples of literals fixed by a later use or left at `i64`.
#[test]
fn a_branch_tuple_of_literals_takes_the_siblings_numbers() {
    let src = "fn main() {
    let steps: i32 = -3;
    let r: i64 = 1;
    let t: (i64, i32) = if r == 0 { (r, steps) } else { (0, 2) };
    let mut ops: Vec[(i64, i32, String)] = Vec.new();
    for c in 0..3 {
        let s: i32 = c as i32;
        ops.push(if c == 0 { (0, 0, f\"c{c}\") } else { (c, s, String.new()) });
        ops.push(match c { 0 => (0, 0, \"z\".to_string()), _ => (c, s, String.new()) });
    }
    let w = if r == 0 { (0, 0) } else { (1, 2) };
    let x: (i32, i32) = w;
    let y = if r == 0 { (0, 0) } else { (1, 2) };
    let e: Vec[i32] = if r == 0 { [0, 0].to_vec() } else { [steps, 7].to_vec() };
    println(f\"{t.1} {ops.len()} {ops[2].1} {ops[5].1} {x.1} {y.0 + 4000000000} {e[0]}\");
}
";
    let (ok, out, run) = check_and_run("literals", src);
    assert!(ok, "check refused a branch tuple of literals:\n{out}");
    assert_eq!(run, "2 6 1 2 2 4000000001 -3\n");
}

/// Two branches of different number types inside a tuple, an `Option`, a
/// list or a nested tuple, in an `if` and a `match`, beside literals or
/// not: each refused.
#[test]
fn a_join_of_different_numbers_inside_is_refused() {
    let src = "fn main() {
    let steps: i32 = 3;
    let r: i64 = 3000000000;
    let a = if steps == 0 { (r, steps) } else { (r, r) };
    let b = match steps { 0 => (r, steps), _ => (r, r) };
    let c = if steps == 0 { Some(steps) } else { Some(r) };
    let d = if steps == 0 { [steps].to_vec() } else { [r].to_vec() };
    let e = if steps == 0 { [steps] } else { [r] };
    let f = if steps == 0 { ((steps, 1), 2) } else { ((r, 1), 2) };
    let g = if steps == 0 { (steps, 1) } else { (r, 1) };
    println(f\"{a.1} {b.1} {c ?? 0} {d[0]} {e[0]} {f.0.0} {g.0}\");
}
";
    let (ok, out, _) = check_and_run("refused", src);
    assert!(!ok, "check accepted a join of different numbers:\n{out}");
    assert_eq!(out.matches("type mismatch in branches").count(), 7, "{out}");
}

/// A literal in a branch's tuple is range-checked against the number the
/// sibling gives it, as a lone literal is.
#[test]
fn a_branch_tuple_literal_out_of_range_is_refused() {
    let src = "fn main() {
    let steps: i32 = 3;
    let r: i64 = 1;
    let v = if r == 0 { (r, steps) } else { (0, 3000000000) };
    let mut ops: Vec[(i64, i32)] = Vec.new();
    ops.push(if r == 0 { (r, steps) } else { (8, 4000000000) });
    println(f\"{v.1} {ops.len()}\");
}
";
    let (ok, out, _) = check_and_run("range", src);
    assert!(!ok, "check accepted an out-of-range literal:\n{out}");
    assert!(
        out.contains("integer literal 3000000000 out of range for 'i32'"),
        "{out}"
    );
    assert!(
        out.contains("integer literal 4000000000 out of range for 'i32'"),
        "{out}"
    );
}

/// Two bare literals of different kinds stay a mismatch, at the top and
/// inside a tuple of literals: pairing a branch's open literals with the
/// sibling's must not make an integer literal a float.
#[test]
fn a_join_of_an_int_and_a_float_literal_stays_refused() {
    let src = "fn main() {
    let c = true;
    let x = if c { 1 } else { 2.5 };
    let y = if c { (1, 2) } else { (2.5, 3) };
    let z = match c { true => (1, 2), false => (2.5, 3) };
    println(f\"{x} {y.0} {z.0}\");
}
";
    let (ok, out, _) = check_and_run("int-float", src);
    assert!(!ok, "check accepted an int literal beside a float:\n{out}");
    assert_eq!(out.matches("type mismatch in branches").count(), 3, "{out}");
}
