//! Kata 1103's iterator arm hands out gifts of 1, 2, 3, ... candies with
//! Rust's `scan`, ending when the candies run out: `(1..).scan(candies,
//! |left, gift| { if *left <= 0 { return None; } let given =
//! gift.min(*left); *left -= given; Some(given) })`. Since kata 2012 a
//! Rust `scan` closure gets the rewrite to Kāra's shape, `Some((state,
//! item))`, but any `return` in the body withheld it, and this one was
//! advice alone. `return None` ends the iteration and carries no state,
//! so it now stays as written and the rest is rewritten, its later lines
//! indented as the closure's body was; a `return` of anything else still
//! gets the advice alone (B-2026-11-10-32).

use std::process::{Command, Stdio};

/// `src` after `karac fix` until nothing changes, and its `check` stderr.
fn fixed(tag: &str, src: &str) -> (String, String) {
    let path =
        std::env::temp_dir().join(format!("karac-kata1103-{tag}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    for _ in 0..6 {
        let before = std::fs::read_to_string(&path).unwrap();
        let _ = Command::new(env!("CARGO_BIN_EXE_karac"))
            .args(["fix", path.to_str().unwrap()])
            .stdin(Stdio::null())
            .output()
            .expect("karac");
        if std::fs::read_to_string(&path).unwrap() == before {
            break;
        }
    }
    let check = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", path.to_str().unwrap()])
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let out = std::fs::read_to_string(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    (out, String::from_utf8_lossy(&check.stderr).into_owned())
}

#[test]
fn kata_1103_a_scan_guard_returning_none_is_rewritten() {
    let src = "fn handed_out(candies: i64) -> Vec[i64] {
    (1..)
        .scan(candies, |left, gift: i64| {
            if *left <= 0 {
                return None;
            }
            let given = gift.min(*left);
            *left -= given;
            Some(given)
        })
        .collect()
}

fn main() {
    println(f\"{handed_out(7)} {handed_out(10)}\");
}
";
    let (text, check) = fixed("guard", src);
    // The body keeps its place: the rewrite's later lines are indented as
    // the closure's were.
    assert!(text.contains("\n            if left <= 0 {\n"), "{text}");
    assert!(text.contains("\n        })\n        .collect()"), "{text}");
    assert!(text.contains("return None;"), "{text}");
    assert!(text.contains("let left = left - given;"), "{text}");
    assert!(text.contains("Some((left, given))"), "{text}");
    assert!(!text.contains("*left"), "{text}");
    assert!(
        check.contains("All checks passed") || check.is_empty(),
        "{check}"
    );
    let out = karac::mir::lower::run_source(&text)
        .unwrap_or_else(|e| panic!("{e}\n{text}"))
        .output;
    assert_eq!(out, "[1, 2, 3, 1] [1, 2, 3, 4]\n");
}

#[test]
fn kata_1103_a_scan_returning_an_item_early_keeps_the_advice_alone() {
    let src = "fn main() {
    let v: Vec[i64] = [3, 1, 4];
    let a: Vec[i64] = v.iter().scan(0, |m, x| {
        if x > 3 {
            return Some(*m);
        }
        *m += x;
        Some(*m)
    }).collect();
    println(f\"{a}\");
}
";
    let (text, check) = fixed("early-item", src);
    assert_eq!(text, src, "no rewrite past a `return` of an item");
    assert!(
        check.contains("bind each new state with `let m = ..` and return `Some((m, item))`"),
        "{check}"
    );
}
