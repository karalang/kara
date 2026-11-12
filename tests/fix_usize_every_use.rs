//! B-2026-11-08-49 (kata 1519): LeetCode's Rust template with a nested
//! `fn dfs(u: usize, p: usize, adj: &Vec<Vec<usize>>, ..)`, filled by
//! `adj[e[0] as usize].push(e[1] as usize)` and started with `usize::MAX`.
//! `karac fix` retyped the written `usize`s to `i64` but left the casts and
//! the `usize.MAX` that feed them, so the program it left was refused where
//! the fix itself had made the types disagree. Every `as usize` and every
//! `usize.MAX` now becomes `i64` with them, and the template checks and runs.

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

/// Every check's diagnostics, the text `karac fix` leaves, whether that
/// text checks clean, and what it prints under the MIR interpreter.
fn mend(dir: &Path, src: &str) -> (String, String, bool, String) {
    std::fs::create_dir_all(dir).unwrap();
    let file = dir.join("cell.kara");
    std::fs::write(&file, format!("{src}\n")).unwrap();
    let mut first = String::new();
    for _ in 0..8 {
        first += &karac(dir, &["check", "cell.kara"]).2;
        let before = std::fs::read_to_string(&file).unwrap();
        karac(dir, &["fix", "cell.kara"]);
        if std::fs::read_to_string(&file).unwrap() == before {
            break;
        }
    }
    let text = std::fs::read_to_string(&file).unwrap();
    let (clean, _, _) = karac(dir, &["check", "cell.kara"]);
    let (_, out, _) = karac(dir, &["__mir-run", "cell.kara"]);
    let _ = std::fs::remove_dir_all(dir);
    (first, text, clean, out)
}

#[test]
fn every_usize_cast_and_max_follows_the_written_types_to_i64() {
    let src = r#"struct Solution;

impl Solution {
    pub fn count(n: i32, edges: Vec<Vec<i32>>) -> Vec<i32> {
        fn dfs(u: usize, p: usize, adj: &Vec<Vec<usize>>, ans: &mut Vec<i32>) -> i32 {
            let mut size = 1;
            for &v in &adj[u] {
                if v != p {
                    size += dfs(v, u, adj, ans);
                }
            }
            ans[u] = size;
            size
        }
        let n = n as usize;
        let mut adj = vec![vec![]; n];
        for e in &edges {
            adj[e[0] as usize].push(e[1] as usize);
            adj[e[1] as usize].push(e[0] as usize);
        }
        let mut ans: Vec<i32> = vec![0; n];
        dfs(0, usize::MAX, &adj, &mut ans);
        ans
    }
}

fn main() {
    let a = Solution::count(4, vec![vec![0, 1], vec![1, 2], vec![0, 3]]);
    for x in a.iter() {
        println(f"{x}");
    }
}"#;
    let dir = std::env::temp_dir().join(format!("karac-usize-every-use-{}", std::process::id()));
    let (_, text, clean, out) = mend(&dir, src);
    assert!(clean, "still refused after fix:\n{text}");
    assert!(!text.contains("usize"), "{text}");
    assert!(
        // A list position widens any integer, so the index's `as i64`
        // goes too; the first push's stays, since it is what makes `adj`
        // a list of `i64` lists, and the second push's then goes
        // (B-2026-11-12-5).
        text.contains("adj[e[0]].push(e[1] as i64)")
            && text.contains("adj[e[1]].push(e[0]);")
            && text.contains("i64.MAX"),
        "{text}"
    );
    assert_eq!(out, "4\n2\n1\n1\n", "{text}");
}
