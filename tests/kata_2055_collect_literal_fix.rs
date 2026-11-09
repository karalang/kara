//! Kata 2055's Rust arm: LeetCode's template keeps its plate counts in
//! `let mut prefix = vec![0; n + 1];`, which `karac fix` spells
//! `Vec.filled(n + 1, 0)`, and returns
//! `queries.iter().map(|q| .. prefix[r] - prefix[l] ..).collect()` from a
//! function that returns `Vec<i32>`. The literal took the default `i64`, and
//! the `collect` was refused as "trait bound `C: FromIterator[i64]` is not
//! satisfied" with no fix. The `map` closure's results read the filled
//! collection's elements, so the fix is the fill's literal suffix, as for a
//! narrowing return (B-2026-11-08-53).

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn write(name: &str, src: &str) -> String {
    let path = std::env::temp_dir().join(format!("karac-{name}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    path.to_str().unwrap().to_string()
}

#[test]
fn kata_2055_collect_of_filled_elements_into_narrower_vec_gets_the_fill_suffix() {
    let src = "fn spans(n: i64, queries: Vec[Vec[i32]]) -> Vec[i32] {
    let mut prefix = Vec.filled(n + 1, 0);
    for i in 0..n {
        prefix[i + 1] = prefix[i] + 2;
    }
    queries
        .iter()
        .map(|q| {
            let (l, r) = (q[0] as i64, q[1] as i64);
            if l < r {
                prefix[r] - prefix[l]
            } else {
                0
            }
        })
        .collect()
}

fn main() {
    println(spans(5, [[1, 4], [3, 2]]));
}
";
    let file = write("k2055-collect", src);
    let check = karac(&["check", &file]);
    let msg = String::from_utf8_lossy(&check.stderr).into_owned();
    assert!(
        msg.contains("the elements of `prefix` are 'i64' because the literal it is filled with took the default"),
        "{msg}"
    );
    let _ = karac(&["fix", &file]);
    let fixed = std::fs::read_to_string(&file).unwrap();
    let check = karac(&["check", &file]);
    let _ = std::fs::remove_file(&file);
    assert!(
        check.status.success(),
        "{}\n{fixed}",
        String::from_utf8_lossy(&check.stderr)
    );
    assert!(fixed.contains("Vec.filled(n + 1, 0i32)"), "{fixed}");
    let out = karac::mir::lower::run_source(&fixed)
        .expect("MIR interpreter")
        .output;
    assert_eq!(out, "[6, 0]\n");
}

#[test]
fn kata_2055_collect_of_lengths_into_narrower_vec_gets_no_fill_suffix() {
    let src = "fn sizes(rows: Vec[Vec[i64]]) -> Vec[i32] {
    let zero = Vec.filled(2, 0);
    rows.iter().map(|r| r.len() + zero.len()).collect()
}

fn main() {
    println(sizes([[1], [2, 3]]));
}
";
    let file = write("k2055-lengths", src);
    let dry = karac(&["fix", "--dry-run", &file]);
    let _ = std::fs::remove_file(&file);
    let plan = String::from_utf8_lossy(&dry.stdout).into_owned();
    assert!(!plan.contains("0i32"), "{plan}");
}
