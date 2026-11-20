//! Fixes found by katas 1745 to 1756, each pinned by the smallest
//! program that shows it.

use std::path::PathBuf;
use std::process::Command;

fn program(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-k1750-{name}-{}", std::process::id()));
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

/// `karac check` refuses `src` with a message holding `msg`, and `karac
/// fix` leaves a program that checks clean, holds `want`, and prints
/// `out`.
fn refused_then_fixed(name: &str, src: &str, msg: &str, want: &str, out_want: &str) {
    let f = program(name, src);
    let (ok, out, err) = karac(&["check"], &f);
    assert!(!ok, "{name}: checked clean before the fix");
    assert!(out.contains(msg) || err.contains(msg), "{name}: {out}{err}");
    let _ = karac(&["fix"], &f);
    let fixed = std::fs::read_to_string(&f).unwrap();
    assert!(fixed.contains(want), "{name}: the fix left\n{fixed}");
    let (ok, out, err) = karac(&["check"], &f);
    assert!(ok, "{name}: after the fix: {out}{err}\n{fixed}");
    let (ok, out, err) = karac(&["run", "--interp"], &f);
    assert!(ok, "{name}: {err}");
    assert_eq!(out, out_want, "{name}");
}

/// A `String` parameter bound to a `Str` local is refused with a
/// message that says why, and the fix views it with `.as_str()` (kata
/// 1750's trim arm; B-2026-11-20-105).
#[test]
fn a_string_param_bound_to_a_str_local_takes_as_str() {
    refused_then_fixed(
        "str_param",
        "fn trimmed(s: String) -> i64 {\n    let mut rest: Str = s;\n    while let Some(r) = rest.strip_prefix(\"a\") {\n        rest = r;\n    }\n    rest.len()\n}\nfn main() {\n    println(trimmed(\"aab\".to_string()));\n}\n",
        "a `String` is passed as a `Str` only at a call",
        "let mut rest: Str = s.as_str();",
        "1\n",
    );
}

/// The same for a local `String` and a field (B-2026-11-20-105).
#[test]
fn a_string_local_or_field_bound_to_a_str_takes_as_str() {
    refused_then_fixed(
        "str_local",
        "struct Word {\n    text: String,\n}\nfn main() {\n    let s = \"abc\".to_string();\n    let v: Str = s;\n    let w = Word { text: \"xy\".to_string() };\n    let t: Str = w.text;\n    println(f\"{v.len()} {t.len()}\");\n}\n",
        "a `String` is passed as a `Str` only at a call",
        "let t: Str = w.text.as_str();",
        "3 2\n",
    );
}

/// `.unwrap()` on `VecDeque.remove`, as Rust's returns an `Option`, is
/// refused with a message naming Kāra's `remove`, and the fix drops it
/// (kata 1756's deque arm; B-2026-11-20-106).
#[test]
fn unwrap_on_vecdeque_remove_is_dropped() {
    refused_then_fixed(
        "deque_remove",
        "fn main() {\n    let mut q: VecDeque[i32] = VecDeque.new();\n    q.push_back(1);\n    q.push_back(2);\n    q.push_back(3);\n    let v = q.remove(1).unwrap();\n    q.push_back(v);\n    println(f\"{v} {q.len()}\");\n}\n",
        "Kāra's `VecDeque.remove` returns the element itself",
        "let v = q.remove(1);",
        "2 3\n",
    );
}
