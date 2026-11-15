//! Kata 2177: `Some(n).map(|x| (0..x).collect()).unwrap_or(Vec.new())`
//! returned as a `Vec[i64]` was refused with "expected '?T3', found
//! 'Vec[?T4]'": the `Option`'s element was still the `collect`'s open
//! output, and a slot that is one open variable did not take an open value
//! handed to it. `unwrap_or_else(|| Vec.new())` was refused the same way,
//! through its `OnceFn` slot.

use std::path::Path;
use std::process::{Command, Stdio};

fn run(tag: &str, src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("karac-kata-2177-{tag}-{}", std::process::id()));
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
fn kata_2177_collect_in_map_meets_unwrap_or() {
    let (ok, out) = run(
        "or",
        "fn three(num: i64) -> Vec[i64] {\n    Some(num / 3).filter(|x| 3 * x == num).map(|x| ((x - 1)..(x + 2)).collect()).unwrap_or(Vec.new())\n}\nfn main() {\n    println(f\"{three(33)} {three(4)}\");\n}",
    );
    assert!(ok, "{out}");
    assert_eq!(out.trim(), "[10, 11, 12] []", "{out}");
}

#[test]
fn kata_2177_collect_in_map_meets_unwrap_or_else() {
    let (ok, out) = run(
        "else",
        "fn upto(n: i64) -> Vec[i64] {\n    Some(n).map(|x| (0..x).collect()).unwrap_or_else(|| Vec.new())\n}\nfn main() {\n    println(f\"{upto(3)}\");\n}",
    );
    assert!(ok, "{out}");
    assert_eq!(out.trim(), "[0, 1, 2]", "{out}");
}

#[test]
fn kata_2177_open_option_still_refuses_a_wrong_default() {
    let (ok, out) = run(
        "wrong",
        "fn upto(n: i64) -> Vec[i64] {\n    Some(n).map(|x| (0..x).collect()).unwrap_or(\"none\")\n}\nfn main() {\n    println(f\"{upto(3)}\");\n}",
    );
    assert!(!ok, "{out}");
    assert!(out.contains("expected"), "{out}");
}
