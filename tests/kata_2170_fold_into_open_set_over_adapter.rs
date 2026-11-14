//! Kata 2170: `nums.iter().skip(1).fold(Map.new(), |m, x| { .. m })` into
//! an annotated `Map[i64, i64]` (and the same with `Set.new()`) was refused
//! with "expected 'Fn(Map[?T1, ?T2], ref i64) -> Map[?T1, ?T2]', found
//! 'Fn(Map[?T1, ?T2], ref i64) -> Map[i64, i64]'". The tail had solved the
//! key and value, but the set's hasher was still open on both sides, so the
//! closure's whole type was never unified with its slot. Over a plain
//! `iter()`, and with a `Vec` or a `SortedMap`, it was accepted.

use std::path::Path;
use std::process::{Command, Stdio};

fn run(tag: &str, src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("karac-kata-2170-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), format!("{src}\n")).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["__mir-run", "cell.kara"])
        .current_dir(Path::new(&dir))
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
    )
}

#[test]
fn kata_2170_fold_into_open_map_over_skip_and_step_by() {
    let (ok, out) = run(
        "map",
        "fn main() {\n    let nums: Vec[i64] = [1, 2, 1, 3, 1];\n    let counts: Map[i64, i64] = nums.iter().skip(0).step_by(2).fold(Map.new(), |m, x| {\n        let mut m = m;\n        m.insert(x, m.get(x).unwrap_or(0) + 1);\n        m\n    });\n    println(f\"{counts.len()} {counts[1]}\");\n}",
    );
    assert!(ok, "{out}");
    assert_eq!(out.trim(), "1 3", "{out}");
}

#[test]
fn kata_2170_fold_into_open_set_over_skip() {
    let (ok, out) = run(
        "set",
        "fn main() {\n    let nums: Vec[i64] = [1, 2, 1, 3];\n    let seen: Set[i64] = nums.iter().skip(1).fold(Set.new(), |m, x| {\n        let mut m = m;\n        m.insert(*x);\n        m\n    });\n    println(f\"{seen.len()} {seen.contains(2)} {seen.contains(4)}\");\n}",
    );
    assert!(ok, "{out}");
    assert_eq!(out.trim(), "3 true false", "{out}");
}

#[test]
fn kata_2170_fold_into_open_set_still_refuses_a_wrong_element() {
    let (ok, out) = run(
        "wrong",
        "fn main() {\n    let nums: Vec[i64] = [1, 2];\n    let seen: Set[String] = nums.iter().skip(1).fold(Set.new(), |m, x| {\n        let mut m = m;\n        m.insert(*x);\n        m\n    });\n    println(seen.len());\n}",
    );
    assert!(!ok, "{out}");
    assert!(out.contains("expected"), "{out}");
}
