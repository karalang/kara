//! B-2026-11-14-6: `Array[i64, 26].filled(0)`, guessed from
//! `Vec.filled(n, v)`, named the receiver `Array[…]` and offered nothing.
//! The message now names the type as written and points at the repeat
//! literal `[v; N]`, which `karac fix` writes when the element and the
//! length can be read off the call.

use std::path::PathBuf;
use std::process::Command;

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

fn fixture(tag: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-array-fill-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("p.kara");
    std::fs::write(&path, src).unwrap();
    (dir, path)
}

#[test]
fn array_filled_names_the_type_and_is_fixed_to_a_repeat_literal() {
    let src =
        "fn main() {\n    let mut count = Array[i64, 26].filled(0);\n    count[3] += 1;\n    \
               let b = Array[u8, 4].filled(9);\n    let flag = true;\n    \
               let c = Array[bool, 3].filled(flag);\n    \
               println(f\"{count[3]} {b[1] + 1u8} {c[2]}\");\n}\n";
    let (dir, path) = fixture("fix", src);
    let first = karac().arg("check").arg(&path).output().unwrap();
    assert!(!first.status.success());
    let err = String::from_utf8_lossy(&first.stderr);
    assert!(
        err.contains("no method 'filled' on `Array[i64, 26]`") && err.contains("(here `[0; 26]`)"),
        "{err}"
    );
    assert!(err.contains("(here `[9u8; 4]`)"), "{err}");
    assert!(!err.contains("[…]"), "{err}");
    karac().arg("fix").arg(&path).output().unwrap();
    let fixed = std::fs::read_to_string(&path).unwrap();
    for edit in ["= [0; 26];", "= [9u8; 4];", "= [flag; 3];"] {
        assert!(fixed.contains(edit), "expected `{edit}` in:\n{fixed}");
    }
    let check = karac().arg("check").arg(&path).output().unwrap();
    assert!(
        check.status.success(),
        "{fixed}\n{}",
        String::from_utf8_lossy(&check.stderr)
    );
    let run = karac().arg("__mir-run").arg(&path).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&run.stdout), "1 10 true\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn array_constructor_without_a_readable_shape_only_hints() {
    let src = "const LEN: i64 = 4;\n\
               fn main() {\n    let s = \"x\";\n    let a = Array[String, 3].filled(s);\n    \
               let b = Array[i64, LEN].filled(0);\n    let c: Array[i64, 4] = Array.filled(4, 7);\n    \
               let d = Array[i64, 2].new();\n}\n";
    let (dir, path) = fixture("hint", src);
    let first = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&first.stderr);
    assert!(
        err.contains("on `Array[String, 3]`") && err.contains("(here `[v; 3]`)"),
        "{err}"
    );
    assert!(
        err.contains("on `Array[i64, LEN]`") && err.contains("(here `[0; LEN]`)"),
        "{err}"
    );
    assert!(
        err.contains("on type 'Array': a fixed array is built with the repeat literal"),
        "{err}"
    );
    assert!(err.contains("no method 'new' on `Array[i64, 2]`"), "{err}");
    karac().arg("fix").arg(&path).output().unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        src,
        "nothing is machine-applicable"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
