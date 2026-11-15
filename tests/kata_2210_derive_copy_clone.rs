//! Kata 2210 (count hills and valleys), the trend arm: B-2026-11-15-81. The
//! fix for a moved value whose type could be `Copy` wrote `#[derive(Copy)]`,
//! which the check then warned about at once, as `Copy` needs `Clone` and
//! a derive never adds another; a second `karac fix` round added `Clone`.
//! The fix now writes `#[derive(Copy, Clone)]` in one round.

use std::path::Path;
use std::process::{Command, Stdio};

fn karac(tag: &str, src: &str, args: &[&str]) -> (bool, String, String) {
    let dir = std::env::temp_dir().join(format!("karac-kata-2210-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), format!("{src}\n")).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg("cell.kara")
        .current_dir(Path::new(&dir))
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let fixed = std::fs::read_to_string(dir.join("cell.kara")).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
        fixed,
    )
}

const TREND: &str = "enum Trend {\n    Unknown,\n    Rising,\n    Falling,\n}\n\nfn turns(nums: Vec[i64]) -> i64 {\n    let mut trend = Trend.Unknown;\n    let mut turns = 0;\n    for i in 1..nums.len() {\n        let step = if nums[i] > nums[i - 1] {\n            Trend.Rising\n        } else if nums[i] < nums[i - 1] {\n            Trend.Falling\n        } else {\n            continue;\n        };\n        match (trend, step) {\n            (Trend.Rising, Trend.Falling) | (Trend.Falling, Trend.Rising) => turns += 1,\n            _ => {}\n        }\n        trend = step;\n    }\n    turns\n}\n\nfn main() {\n    println(f\"{turns([2, 4, 1, 1, 6, 5])}\");\n}";

#[test]
fn kata_2210_copy_fix_writes_clone_too() {
    let (ok, out, _) = karac("help", TREND, &["check"]);
    assert!(!ok, "{out}");
    assert!(
        out.contains("add `#[derive(Copy, Clone)]` to 'Trend'"),
        "{out}"
    );
    let (_, _, fixed) = karac("fix", TREND, &["fix"]);
    assert!(
        fixed.contains("#[derive(Copy, Clone)]\nenum Trend"),
        "{fixed}"
    );
    let (ok, out, _) = karac("again", &fixed, &["check", "--output=json"]);
    assert!(
        ok && out.contains("\"diagnostics\":[]"),
        "one round leaves nothing: {out}"
    );
    let (ok, out, _) = karac("run", &fixed, &["__mir-run"]);
    assert!(ok, "{out}");
    assert_eq!(out.trim(), "3", "{out}");
}

#[test]
fn kata_2210_clone_fix_stays_clone() {
    // a `String` field rules out `Copy`: the fix is still `Clone` alone
    let src = "struct Name {\n    text: String,\n}\n\nfn take(n: own Name) -> i64 {\n    n.text.len()\n}\n\nfn main() {\n    let n = Name { text: \"ab\" };\n    println(f\"{take(n)}\");\n    println(f\"{take(n)}\");\n}";
    let (_, out, fixed) = karac("clone", src, &["fix"]);
    assert!(!fixed.contains("Copy"), "{out}\n{fixed}");
}
