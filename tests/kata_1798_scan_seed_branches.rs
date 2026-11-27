//! B-2026-11-27-25: a `scan` reduced to one item whose closure stops the scan on
//! one branch (`if c > reach + 1 { None } else { Some((reach + c, reach +
//! c)) }`), or yields its new state as an expression written twice, or is
//! read with `unwrap_or(..)`, still has its seed's type, but the narrowing
//! at a later use said only "write an explicit 'as i32'" with no fix (kata
//! 1798's scan arm). The seed's literal now takes the suffix, as the plain
//! `Some((s, s))` form did since B-2026-11-20-47.

use std::path::PathBuf;
use std::process::Command;

fn program(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-k1798-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    f
}

fn karac(args: &[&str], f: &PathBuf) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(f)
        .output()
        .expect("karac");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// `karac fix` on `src`, then the fixed source, whether it checks clean,
/// and its output under the interpreter.
fn fixed(name: &str, src: &str) -> (String, bool, String) {
    let f = program(name, src);
    let _ = karac(&["fix"], &f);
    let text = std::fs::read_to_string(&f).unwrap();
    let (ok, _, _) = karac(&["check"], &f);
    let (_, out, _) = karac(&["run", "--interp"], &f);
    let _ = std::fs::remove_dir_all(f.parent().unwrap());
    (text, ok, out)
}

/// Kata 1798's scan arm: a branch that stops the scan, the state written
/// twice as an expression, and the reduction read with `unwrap_or(0)`.
#[test]
fn a_scan_that_stops_on_a_branch_takes_the_seeds_suffix() {
    let src = "fn reach(coins: Vec[i32]) -> i32 {\n    let reach = coins.iter().scan(0, |reach, c| if c > reach + 1 { None } else { Some((reach + c, reach + c)) }).last().unwrap_or(0);\n    reach + 1\n}\n\nfn main() {\n    println(reach([1, 1, 4, 10].to_vec()));\n}\n";
    let (text, ok, out) = fixed("stops", src);
    assert!(text.contains(".scan(0i32, "), "{text}");
    assert!(ok, "{text}");
    assert_eq!(out, "8\n");
}

/// The plain `Some((s, s))` form read with `unwrap_or`, and a block tail
/// whose `if` has an `else if`.
#[test]
fn a_scan_read_with_unwrap_or_takes_the_seeds_suffix() {
    let src = "fn top(v: Vec[i32]) -> i32 {\n    let best = v.iter().scan(0, |s, x| {\n        let s = s + x;\n        if s > 100 {\n            None\n        } else if s < -100 {\n            None\n        } else {\n            Some((s, s))\n        }\n    }).max().unwrap_or(0);\n    best * 2\n}\n\nfn main() {\n    println(top([3, 4, -2].to_vec()));\n}\n";
    let (text, ok, out) = fixed("unwrap_or", src);
    assert!(text.contains(".scan(0i32, "), "{text}");
    assert!(ok, "{text}");
    assert_eq!(out, "14\n");
}

/// An item that is not the state (`Some((s + x, x as i64))`) need not have
/// the seed's type, so the seed's suffix is not offered.
#[test]
fn a_scan_whose_item_is_not_its_state_is_left_alone() {
    let src = "fn top(v: Vec[i32]) -> i32 {\n    let best = v.iter().scan(0, |s, x| if x > 9 { None } else { Some((s + x, x as i64)) }).last().unwrap_or(0);\n    best + 1\n}\n\nfn main() {\n    println(top([3, 4].to_vec()));\n}\n";
    let (text, ok, _) = fixed("not_state", src);
    assert!(!text.contains("0i32"), "{text}");
    assert!(!ok, "{text}");
}
