//! A `flat_map` closure whose body is an `if` or a `match` giving a list
//! from each branch gets the `.into_iter()` fix after the whole expression
//! (B-2026-11-10-3). Kata 1089 wrote `|x| if x == 0 { [0, 0] } else { [x] }`,
//! as Rust's `flat_map` takes any `IntoIterator`; `karac check` said to
//! iterate it with `.into_iter()` but `karac fix` had nothing to apply,
//! because the fix only knew where a call or a list ends.

use std::path::PathBuf;
use std::process::Command;

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

fn fixture(tag: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("kata-1089-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    (dir, path)
}

/// `src` is refused with the `.into_iter()` message, `karac fix` writes
/// `fixed` into it, and the fixed program checks clean and prints `printed`.
fn fixed_by_into_iter(tag: &str, src: &str, fixed: &str, printed: &str) {
    let (dir, path) = fixture(tag, src);
    let out = karac().arg("check").arg(&path).output().unwrap();
    let text =
        String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    assert!(
        !out.status.success() && text.contains("iterate it with `.into_iter()`"),
        "{tag}: check must refuse with the hint: {text}"
    );
    let out = karac().arg("fix").arg(&path).output().unwrap();
    assert!(
        out.status.success(),
        "{tag}: fix: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let after = std::fs::read_to_string(&path).unwrap();
    assert!(
        after.contains(fixed),
        "{tag}: fix must write `{fixed}`:\n{after}"
    );
    let out = karac().arg("check").arg(&path).output().unwrap();
    assert!(
        out.status.success(),
        "{tag}: the fixed program must check: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = karac().arg("__mir-run").arg(&path).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), printed, "{tag}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_flat_map_if_of_lists_takes_into_iter_after_the_if() {
    fixed_by_into_iter(
        "if",
        "fn main() {\n    let arr = [1, 0, 2];\n    let out: Vec[i64] = arr\n        .iter()\n        .flat_map(|x| if x == 0 { [0, 0] } else { [x] })\n        .collect();\n    println(f\"{out}\");\n}\n",
        "else { [x] }.into_iter())",
        "[1, 0, 0, 2]\n",
    );
}

#[test]
fn a_flat_map_match_of_lists_takes_into_iter_after_the_match() {
    fixed_by_into_iter(
        "match",
        "fn main() {\n    let arr = [1, 0, 2];\n    let out: Vec[i64] = arr\n        .iter()\n        .flat_map(|x| match x {\n            0 => [0, 0],\n            _ => [x],\n        })\n        .collect();\n    println(f\"{out}\");\n}\n",
        "}.into_iter())",
        "[1, 0, 0, 2]\n",
    );
}
