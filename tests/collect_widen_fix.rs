//! B-2026-11-14-12: `w.bytes().iter().map(|b| b - 96).collect()` into a
//! `Vec[i64]` was refused with a bare `FromIterator[u8]` bound error and
//! no remedy, though a `u8` widens into `i64` without loss. The refusal
//! now says that `collect` does not convert its items and how to widen
//! them, and `karac fix` writes it: `(b - 96) as i64` on a `map`
//! closure's one-expression body, or a `.map(|x| x as i64)` of its own.
//! A narrowing (`i64` items into a `Vec[u8]`) gets no such hint.

use std::path::PathBuf;
use std::process::Command;

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

fn fixture(tag: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-collect-widen-{tag}-{}-{}",
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
fn a_widening_collect_refusal_names_the_cast_and_is_fixed() {
    let src = "fn main() {\n    let w = \"abc\".to_string();\n    \
               let v: Vec[i64] = w.bytes().iter().map(|b| b - 96).collect();\n    \
               let u: Vec[i64] = w.bytes().iter().collect();\n    \
               let dict = [\"ab\".to_string(), \"cd\".to_string()];\n    \
               let words: Vec[Vec[i64]] = dict.iter().map(|s| s.bytes().iter().map(|b| b - 96).collect()).collect();\n    \
               println(f\"{v} {u} {words}\");\n}\n";
    let (dir, path) = fixture("fix", src);
    let first = karac().arg("check").arg(&path).output().unwrap();
    assert!(!first.status.success());
    let err = String::from_utf8_lossy(&first.stderr);
    assert!(
        err.contains(
            "`collect` does not convert its items: widen each in the `map`, `(b - 96) as i64`"
        ),
        "{err}"
    );
    assert!(
        err.contains(
            "`collect` does not convert its items: widen each first, `.map(|x| x as i64)`"
        ),
        "{err}"
    );
    karac().arg("fix").arg(&path).output().unwrap();
    let fixed = std::fs::read_to_string(&path).unwrap();
    for edit in [
        "let v: Vec[i64] = w.bytes().iter().map(|b| (b - 96) as i64).collect();",
        "let u: Vec[i64] = w.bytes().iter().map(|x| x as i64).collect();",
        ".map(|b| (b - 96) as i64).collect()).collect();",
    ] {
        assert!(fixed.contains(edit), "expected `{edit}` in:\n{fixed}");
    }
    let check = karac().arg("check").arg(&path).output().unwrap();
    assert!(
        check.status.success(),
        "{fixed}\n{}",
        String::from_utf8_lossy(&check.stderr)
    );
    let run = karac().arg("__mir-run").arg(&path).output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "[1, 2, 3] [97, 98, 99] [[1, 2], [3, 4]]\n"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_narrowing_collect_refusal_offers_no_widening() {
    let src = "fn main() {\n    let small: Vec[u8] = [1, 2].iter().map(|x| x + 1).collect();\n    \
               println(f\"{small}\");\n}\n";
    let (dir, path) = fixture("narrow", src);
    let out = karac().arg("check").arg(&path).output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("`Vec[u8]` implements `FromIterator[u8]`, not `FromIterator[i64]`"),
        "{err}"
    );
    assert!(!err.contains("does not convert its items"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}
