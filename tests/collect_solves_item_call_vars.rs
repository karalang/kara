//! design.md § Type inference (B-2026-11-06-7): a `collect` into a written
//! collection is a later use that solves the variables its items left open,
//! so `.map(|_| Vec.new()).collect()` into a `Vec[Vec[String]]` fixes the
//! inner `Vec`'s parameter. The unsolved-variable report runs after the
//! `collect`'s bound has been discharged, not before it.

use std::path::PathBuf;
use std::process::Command;

fn karac(tag: &str, sub: &str, src: &str) -> (bool, String) {
    let path: PathBuf = std::env::temp_dir().join(format!(
        "karac-collect-item-vars-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(sub)
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

const FILLED: &str = "fn main() {\n    let tops: Vec[i64] = [1, 2, 3];\n    \
    let mut cells: Vec[Vec[String]] = tops.iter().map(|_| Vec.new()).collect();\n    \
    cells[0].push(\"x\".to_string());\n    cells[2].push(\"y\".to_string());\n    \
    cells[2].push(\"z\".to_string());\n    println(cells);\n}\n";

#[test]
fn a_written_collect_fixes_an_empty_vec_item_s_parameter() {
    let (ok, text) = karac("check", "check", FILLED);
    assert!(ok, "{text}");
}

#[test]
fn a_written_collect_of_empty_vec_items_runs() {
    let (ok, text) = karac("run", "__mir-run", FILLED);
    assert!(ok, "{text}");
    assert_eq!(text.trim(), "[[x], [], [y, z]]");
}

#[test]
fn an_unwritten_collect_of_empty_vec_items_is_still_refused() {
    let src = "fn main() {\n    let tops: Vec[i64] = [1, 2, 3];\n    \
        let cells = tops.iter().map(|_| Vec.new()).collect();\n    println(1);\n}\n";
    let (ok, text) = karac("unwritten", "check", src);
    assert!(!ok, "{text}");
    assert!(
        text.contains(":3:37: cannot infer type parameter 'T' of `Vec[T]`"),
        "{text}"
    );
}
