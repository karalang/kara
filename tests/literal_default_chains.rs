//! A literal binding that kept the default `i64` is refused where it meets
//! an `i32`, and `karac fix` types its literal. Two gaps in reaching it:
//!
//! - B-2026-11-14-17 (kata 1563): `let l = pre[k + 1] - pre[i]` over a fill
//!   whose literal kept the default, passed to an `i32` `max`, named nothing
//!   to fix; the fill's literal is now the fix, as for a read of it.
//! - B-2026-11-14-18 (kata 1567): counters that feed one another
//!   (`best = best.max(positive)`, `positive = negative + 1`) were typed one
//!   per `karac fix` round; the fix of the one refused now types the ones
//!   that feed it.

use std::path::PathBuf;
use std::process::Command;

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

fn fixture(tag: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-literal-chains-{tag}-{}-{}",
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

/// One `karac fix` round makes `src` check, with every `edit` in it, and the
/// program prints `out`.
fn one_round(tag: &str, src: &str, edits: &[&str], out: &str) {
    let (dir, path) = fixture(tag, src);
    let first = karac().arg("check").arg(&path).output().unwrap();
    assert!(!first.status.success(), "{tag}: check must refuse");
    karac().arg("fix").arg(&path).output().unwrap();
    let fixed = std::fs::read_to_string(&path).unwrap();
    let check = karac().arg("check").arg(&path).output().unwrap();
    assert!(
        check.status.success(),
        "{tag}: one round must reach a checking program:\n{fixed}\n{}",
        String::from_utf8_lossy(&check.stderr)
    );
    for edit in edits {
        assert!(
            fixed.contains(edit),
            "{tag}: expected `{edit}` in:\n{fixed}"
        );
    }
    let run = karac().arg("__mir-run").arg(&path).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&run.stdout), out, "{tag}:\n{fixed}");
    let _ = std::fs::remove_dir_all(&dir);
}

const STONE_GAME_V: &str = "fn stone(stone_value: Vec[i32]) -> i32 {
    let n = stone_value.len();
    let mut pre = Vec.filled(n + 1, 0);
    for i in 0..n {
        pre[i + 1] = pre[i] + stone_value[i];
    }
    let mut dp = Vec.filled(n, Vec.filled(n, 0i32));
    for len in 2..n + 1 {
        for i in 0..n - len + 1 {
            let j = i + len - 1;
            for k in i..j {
                let l = pre[k + 1] - pre[i];
                let r = pre[j + 1] - pre[k + 1];
                if l < r {
                    dp[i][j] = dp[i][j].max(l + dp[i][k]);
                } else if l > r {
                    dp[i][j] = dp[i][j].max(r + dp[k + 1][j]);
                } else {
                    dp[i][j] = dp[i][j].max(l + dp[i][k].max(dp[k + 1][j]));
                }
            }
        }
    }
    dp[0][n - 1]
}
fn main() {
    println(stone([6, 2, 3, 4, 5, 5]));
    println(stone([7, 7, 7, 7, 7, 7, 7]));
}
";

#[test]
fn literal_fix_follows_a_difference_of_fill_reads_into_max() {
    let (dir, path) = fixture("names", STONE_GAME_V);
    let check = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&check.stderr);
    assert_eq!(
        err.matches("the elements of `pre` are 'i64' because the literal it is filled with")
            .count(),
        3,
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
    one_round(
        "stone",
        STONE_GAME_V,
        &["let mut pre = Vec.filled(n + 1, 0i32);"],
        "18\n28\n",
    );
}

const COUNTERS: &str = "fn longest(nums: Vec[i32]) -> i32 {
    let mut positive = 0;
    let mut negative = 0;
    let mut best = 0;
    for x in nums {
        if x == 0 {
            positive = 0;
            negative = 0;
        } else if x > 0 {
            positive = positive + 1;
            negative = if negative > 0 { negative + 1 } else { 0 };
        } else {
            let was_positive = positive;
            positive = if negative > 0 { negative + 1 } else { 0 };
            negative = was_positive + 1;
        }
        best = best.max(positive);
    }
    best
}
fn main() {
    println(longest([1, -2, -3, 4]));
    println(longest([0, 1, -2, -3, -4]));
}
";

#[test]
fn literal_fix_types_the_counters_that_feed_the_refused_one() {
    one_round(
        "counters",
        COUNTERS,
        &[
            "let mut positive: i32 = 0;",
            "let mut negative: i32 = 0;",
            "let mut best: i32 = 0;",
        ],
        "4\n3\n",
    );
    // Refused at the `max` argument instead, with `best` already typed.
    one_round(
        "counters-max",
        &COUNTERS.replace("let mut best = 0;", "let mut best: i32 = 0;"),
        &["let mut positive: i32 = 0;", "let mut negative: i32 = 0;"],
        "4\n3\n",
    );
    // The tuple form types each literal.
    one_round(
        "counters-tuple",
        &COUNTERS.replace(
            "let mut positive = 0;\n    let mut negative = 0;\n    let mut best = 0;",
            "let mut (positive, negative, best) = (0, 0, 0);",
        ),
        &["= (0i32, 0i32, 0i32);"],
        "4\n3\n",
    );
}

#[test]
fn literal_fix_leaves_a_counter_that_only_reads_the_refused_one() {
    // `seen` is read from `best`, not into it: typing `best` does not refuse
    // it, so it keeps its default.
    one_round(
        "reader",
        "fn f(xs: Vec[i32]) -> i32 {
    let mut best = 0;
    let mut seen = 0;
    for x in xs {
        best = best.max(x);
        seen = seen + best;
    }
    println(seen);
    best
}
fn main() { println(f([3, 1, 2])); }
",
        &["let mut best: i32 = 0;", "let mut seen = 0;"],
        "9\n3\n",
    );
}
