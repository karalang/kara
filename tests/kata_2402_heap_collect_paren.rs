//! Kata 2402's Rust arm wrote its heap as `let mut idle: BinaryHeap<i64> =
//! (0..n).map(..).collect();`. The value's span starts inside the range's
//! parenthesis, so `karac fix` wrapped only `0..n).map(..).collect()` and
//! left `(PriorityQueue.max_first_from(0..n).map(..).collect())`, which
//! collects into a `PriorityQueue` from an unmapped range (B-2026-11-21-40).
//! The wrap now widens back over the opening parentheses first.

use std::path::Path;
use std::process::{Command, Stdio};

fn karac(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg("cell.kara")
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let fixed = std::fs::read_to_string(dir.join("cell.kara")).unwrap_or_default();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
        fixed,
    )
}

fn mend(tag: &str, src: &str) -> (String, String) {
    let dir = std::env::temp_dir().join(format!("karac-kata-2402-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), src).unwrap();
    let mut fixed = String::new();
    for _ in 0..8 {
        let (_, _, now) = karac(&dir, &["fix"]);
        if now == fixed {
            break;
        }
        fixed = now;
    }
    let (ok, out, _) = karac(&dir, &["check"]);
    assert!(ok, "{tag}: {out}\n{fixed}");
    let (ok, out, _) = karac(&dir, &["__mir-run"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(ok, "{tag}: {out}\n{fixed}");
    (fixed, out)
}

#[test]
fn kata_2402_paren_range_heap_collect_wraps_whole_value() {
    let src = "use std::collections::BinaryHeap;\n\nfn main() {\n    let n = 4;\n    let mut idle: BinaryHeap<i64> = (0..n).map(|x| x * 2).collect();\n    println(f\"{idle.pop().unwrap()}\");\n}\n";
    let (fixed, out) = mend("mapped", src);
    assert!(
        fixed.contains("PriorityQueue.max_first_from((0..n).map(|x| x * 2).collect())"),
        "{fixed}"
    );
    assert_eq!(out.trim(), "6", "{fixed}");
}

#[test]
fn kata_2402_paren_range_heap_collect_unmapped() {
    let src = "use std::collections::BinaryHeap;\n\nfn main() {\n    let n = 4;\n    let mut idle: BinaryHeap<i64> = (0..n).collect();\n    println(f\"{idle.pop().unwrap()}\");\n}\n";
    let (fixed, out) = mend("plain", src);
    assert!(
        fixed.contains("PriorityQueue.max_first_from((0..n).collect())"),
        "{fixed}"
    );
    assert_eq!(out.trim(), "3", "{fixed}");
}
