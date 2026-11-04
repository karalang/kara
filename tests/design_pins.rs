//! `corpus/design/`: programs that pin design.md rules outside the core
//! semantics, each with a spec expectation (its `meta.toml` note names the
//! section). The corpus runner runs them on every backend; this test keeps
//! them in `cargo test`, as `core_pins` does for `corpus/core/`.
//!
//! An `ok_` pin's run must print its `expected.out`. An `err_` pin must be
//! refused by `karac check`, and its first error must be on the line, and
//! carry the text, its `expected.out` gives as `ERROR at line N: <text>`.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

#[test]
fn design_pins_hold() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus/design");
    let mut pins: Vec<_> = std::fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    pins.sort();
    let mut bad = Vec::new();
    let mut seen = 0;
    for dir in &pins {
        let name = dir.file_name().unwrap().to_str().unwrap();
        let src = dir.join("source.kara");
        let src = src.to_str().unwrap();
        let want = std::fs::read_to_string(dir.join("expected.out")).unwrap();
        seen += 1;
        if name.starts_with("err_") {
            let (line, text) = want
                .trim_end()
                .strip_prefix("ERROR at line ")
                .and_then(|r| r.split_once(": "))
                .unwrap_or_else(|| panic!("{name}: expected.out is not `ERROR at line N: ..`"));
            let out = karac(&["check", src]);
            let err = String::from_utf8_lossy(&out.stderr);
            let first = err.lines().find(|l| l.starts_with("error")).unwrap_or("");
            if out.status.success()
                || !first.contains(&format!(":{line}:"))
                || !first.contains(text)
            {
                bad.push(format!("{name}: want line {line} {text:?}, got:\n{err}"));
            }
        } else {
            let out = karac(&["__mir-run", src]);
            let got = String::from_utf8_lossy(&out.stdout);
            if got != want {
                bad.push(format!(
                    "{name}: printed {got:?}, want {want:?}\n{}",
                    String::from_utf8_lossy(&out.stderr)
                ));
            }
        }
    }
    assert!(seen > 0, "no pins under {}", root.display());
    assert!(bad.is_empty(), "{}", bad.join("\n\n"));
}
