//! B-2026-11-18-2: a fill's literal may still be open at the `let` that
//! copies one element out, and settle later at a method on the copy.
//! `let mut cnt = [0; 3]; .. let b = cnt[1]; b.min(cnt[2] / 2)` returned
//! from an `i32` function (kata 1189's Rust arm) was refused with the bare
//! narrowing message and no fix, where the same fill read directly got
//! "write that literal `0i32`". The element-copy trace now follows a copy
//! whose literal is still open.

use std::path::PathBuf;
use std::process::Command;

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

fn fixture(tag: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-element-copy-fix-{tag}-{}-{}",
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

fn fix_and_run(tag: &str, src: &str, fill: &str, fixed_fill: &str, expect: &str) {
    let (dir, path) = fixture(tag, src);
    let first = karac().arg("check").arg(&path).output().unwrap();
    assert!(!first.status.success());
    let err = String::from_utf8_lossy(&first.stderr);
    assert!(err.contains("write that literal `0i32`"), "{err}");
    let json = karac()
        .args(["check", "--output=json"])
        .arg(&path)
        .output()
        .unwrap();
    let json = String::from_utf8_lossy(&json.stdout);
    assert!(json.contains("\"text\":\"i32\""), "{json}");
    assert!(src.contains(fill));
    karac().arg("fix").arg(&path).output().unwrap();
    let fixed = std::fs::read_to_string(&path).unwrap();
    assert_eq!(fixed, src.replacen(fill, fixed_fill, 1));
    let run = karac().arg("__mir-run").arg(&path).output().unwrap();
    assert!(
        run.status.success(),
        "{fixed}\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), expect);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_element_copy_of_an_open_fill_is_fixed_at_the_fill() {
    let src = "fn least() -> i32 {\n    let c = [0; 3];\n    let b = c[1];\n    \
               b.min(c[2])\n}\n\nfn main() {\n    println(f\"{least()}\");\n}\n";
    fix_and_run("plain", src, "[0; 3]", "[0i32; 3]", "0\n");
}

#[test]
fn an_element_copy_of_a_counted_fill_is_fixed_at_the_fill() {
    let src = "fn least(xs: Vec[i64]) -> i32 {\n    let mut cnt = [0; 3];\n    \
               for x in xs {\n        cnt[x] += 1;\n    }\n    let b = cnt[1];\n    \
               b.min(cnt[2] / 2)\n}\n\nfn main() {\n    \
               println(f\"{least([1, 1, 2, 2, 2, 2, 0])}\");\n}\n";
    fix_and_run("counted", src, "[0; 3]", "[0i32; 3]", "2\n");
}
