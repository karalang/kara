//! `karac build`'s terminal effects line (design.md § Inference and
//! declarations; B-2026-11-22-39, review ruling 2026-11-22): exactly
//! `Effects: <sorted set>` or `Effects: none`, on stdout, on the line before
//! `Built: <exe>`.
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

fn build_stdout(name: &str, src: &str) -> Option<(String, String)> {
    let dir =
        std::env::temp_dir().join(format!("karac-effects-line-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.kara"));
    std::fs::write(&file, src).unwrap();
    let exe = dir.join(name);
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["build", file.to_str().unwrap(), "-o", exe.to_str().unwrap()])
        .output()
        .unwrap();
    let res = if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).to_string())
    };
    let got = common::link_or_skip(res).map(|_| {
        (
            String::from_utf8_lossy(&out.stdout).to_string(),
            exe.display().to_string(),
        )
    });
    let _ = std::fs::remove_dir_all(&dir);
    got
}

#[test]
fn build_effects_line_is_sorted_and_precedes_built() {
    let Some((stdout, exe)) = build_stdout(
        "both_streams",
        "fn main() {\n    println(\"x\");\n    eprintln(\"y\");\n}\n",
    ) else {
        return;
    };
    assert_eq!(
        stdout,
        format!("Effects: writes(Stderr), writes(Stdout)\nBuilt: {exe}\n")
    );
}

#[test]
fn build_effects_line_says_none_for_a_pure_program() {
    let Some((stdout, exe)) = build_stdout("pure", "fn main() {\n}\n") else {
        return;
    };
    assert_eq!(stdout, format!("Effects: none\nBuilt: {exe}\n"));
}
