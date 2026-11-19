//! A range of integer literals carried through iterator closures keeps the
//! default `i64` beside the narrower integers it is combined with, and is
//! refused far away with no fix. The fix is the suffix on each range's
//! first literal.
//!
//! - B-2026-11-19-33: `(0..1 << n).map(|i| start ^ i).collect()` into a
//!   `Vec[i32]` (kata 1238), refused at `collect`.
//! - B-2026-11-19-29: three ranges through `flat_map` / `filter_map` into a
//!   tuple handed to `contains` on a `Set[(i32, i32)]` (kata 1222).

use std::process::{Command, Output, Stdio};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

/// `karac check`'s output, the file after `karac fix` (up to three rounds),
/// the check after, and `__mir-run`'s stdout.
fn check_fix_run(tag: &str, src: &str) -> (String, String, String, String) {
    let path = std::env::temp_dir().join(format!(
        "karac-range-suffix-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    let before = String::from_utf8_lossy(&karac(&["check"], &path).stderr).into_owned();
    for _ in 0..3 {
        let was = std::fs::read_to_string(&path).unwrap();
        let _ = karac(&["fix"], &path);
        if std::fs::read_to_string(&path).unwrap() == was {
            break;
        }
    }
    let fixed = std::fs::read_to_string(&path).unwrap();
    let after = String::from_utf8_lossy(&karac(&["check"], &path).stderr).into_owned();
    let run = String::from_utf8_lossy(&karac(&["__mir-run"], &path).stdout).into_owned();
    let _ = std::fs::remove_file(&path);
    (before, fixed, after, run)
}

#[test]
fn a_mapped_literal_range_collected_into_vec_i32_takes_the_suffix() {
    let src = "fn circ(n: i32, start: i32) -> Vec[i32] {\n\
               \x20   (0..1 << n).map(|i| start ^ i ^ (i >> 1)).collect()\n\
               }\n\
               fn main() { println(circ(2, 3)); }\n";
    let (before, fixed, after, run) = check_fix_run("collect", src);
    assert!(
        before.contains("comes from the range starting `0`"),
        "{before}"
    );
    assert!(fixed.contains("(0i32..1 << n).map("), "{fixed}");
    assert!(after.contains("All checks passed"), "{after}");
    assert_eq!(run, "[3, 2, 0, 1]\n");
}

#[test]
fn three_ranges_through_flat_map_and_filter_map_take_the_suffix() {
    let src = "fn attackers(queens: Vec[Vec[i32]], king: Vec[i32]) -> Vec[Vec[i32]] {\n\
               \x20   let placed: Set[(i32, i32)] = queens.iter().map(|q| (q[0], q[1])).collect();\n\
               \x20   let (kx, ky) = (king[0], king[1]);\n\
               \x20   (-1..=1)\n\
               \x20       .flat_map(|dx| (-1..=1).map(|dy| (dx, dy)))\n\
               \x20       .filter(|(dx, dy)| dx != 0 or dy != 0)\n\
               \x20       .filter_map(|(dx, dy)| (1..8).map(|k| (kx + k * dx, ky + k * dy)).find(|p| placed.contains(p)))\n\
               \x20       .map(|(x, y)| [x, y])\n\
               \x20       .collect()\n\
               }\n\
               fn main() { println(attackers([[0, 1], [1, 0], [4, 0], [0, 4], [3, 3], [2, 4]], [0, 0])); }\n";
    let (before, fixed, after, run) = check_fix_run("contains", src);
    assert!(
        before.contains("the 3 ranges these items come from"),
        "{before}"
    );
    assert!(fixed.contains("(-1i32..=1)\n"), "{fixed}");
    assert!(fixed.contains(".flat_map(|dx| (-1i32..=1)"), "{fixed}");
    assert!(fixed.contains("(1i32..8).map("), "{fixed}");
    assert!(after.contains("All checks passed"), "{after}");
    assert_eq!(run, "[[0, 1], [1, 0], [3, 3]]\n");
}

#[test]
fn a_range_ending_at_an_i64_binding_takes_no_suffix() {
    let src = "fn g(n: i64, start: i32) -> Vec[i32] { (0..n).map(|i| start ^ i).collect() }\n\
               fn main() { println(g(3, 1)); }\n";
    let (before, fixed, _, _) = check_fix_run("i64-end", src);
    assert!(before.contains("FromIterator[i64]"), "{before}");
    assert!(!before.contains("comes from the range"), "{before}");
    assert_eq!(fixed, src);
}

#[test]
fn an_i64_operand_in_the_closure_takes_no_suffix() {
    let src = "fn g(big: i64) -> Vec[i32] { (0..4).map(|i| big + i).collect() }\n\
               fn main() { println(g(3)); }\n";
    let (before, fixed, _, _) = check_fix_run("i64-operand", src);
    assert!(!before.contains("comes from the range"), "{before}");
    assert_eq!(fixed, src);
}
