//! B-2026-11-08-57 — `let mut cnt = [0; 26];` whose element literal an
//! operator left at the default (`cnt[c] += sub[c]` with `sub: [i32; 26]`
//! widens, design.md §8 Literals) is refused where it meets an `i32`: the
//! refusal of the whole collection (`cnt` returned) names the binding and
//! carries the literal's suffix, `0i32`, as the machine fix, as the
//! refusal of an element read (`ans[u] = cnt[c]`, B-2026-11-08-53) does.
//! LeetCode 1519's Rust template, carried by `karac fix`.

use std::process::Command;

fn karac(args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .unwrap();
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

const TEMPLATE: &str = "fn dfs(u: i64, p: i64, adj: ref Vec[Vec[i64]], labels: Slice[u8], ans: mut ref Vec[i32]) -> Array[i32, 26] {
    let mut cnt = [0; 26];
    for v in ref adj[u] {
        if v != p {
            let sub = dfs(v, u, adj, labels, mut ans);
            for c in 0..26 {
                cnt[c] += sub[c];
            }
        }
    }
    let c = (labels[u] - b'a') as i64;
    cnt[c] += 1;
    ans[u] = cnt[c];
    cnt
}

fn main() {
    let adj: Vec[Vec[i64]] = [[1, 2], [0, 4, 5], [0, 3, 6], [2], [1], [1], [2]];
    let labels = \"abaedcd\".to_string();
    let mut ans = Vec.filled(7, 0);
    dfs(0, i64.MAX, adj, labels.bytes(), mut ans);
    println(f\"{ans}\");
}
";

#[test]
fn a_repeat_literal_left_at_the_default_is_annotated_by_karac_fix() {
    let dir = std::env::temp_dir().join(format!("karac-repeat-annot-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, TEMPLATE).unwrap();
    let p = path.to_str().unwrap();
    let (ok, out) = karac(&["check", p]);
    assert!(!ok, "{out}");
    assert!(
        out.contains(
            "expected 'Array[i32, 26]', found 'Array[i64, 26]': the elements of `cnt` are 'i64'"
        ) && out.contains("write that literal `0i32`"),
        "{out}"
    );
    karac(&["fix", p]);
    let after = std::fs::read_to_string(&path).unwrap();
    assert!(after.contains("let mut cnt = [0i32; 26];"), "{after}");
    let (ok, out) = karac(&["run", p]);
    assert!(ok && out.ends_with("[2, 1, 1, 1, 1, 1, 1]\n"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}
