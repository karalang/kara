//! `karac build` and `karac run` check the program on one thread while the
//! MIR builder lowers it on another (B-2026-10-31-7). The check still
//! decides: its error ends the command with its own diagnostic, never the
//! builder's, and its warnings still reach the JSON success object.

use std::path::PathBuf;
use std::process::{Command, Output};

fn program(tag: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-check-alongside-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    path
}

fn karac(args: &[&str], path: &PathBuf) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .current_dir(path.parent().unwrap())
        .env("KARAC_AUTO_PAR", "0")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap()
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// The builder refuses this program too; the check's diagnostic is the one
/// reported, on every run.
#[test]
fn a_check_error_is_reported_instead_of_the_builders() {
    let path = program(
        "type-error",
        "fn main() {\n    let x: i64 = \"s\";\n    println(x);\n}\n",
    );
    for cmd in ["build", "run"] {
        for _ in 0..5 {
            let out = karac(&[cmd], &path);
            let all = text(&out);
            assert_eq!(out.status.code(), Some(1), "{cmd}: {all}");
            assert!(all.contains("error[typecheck]"), "{cmd}: {all}");
            assert!(
                all.contains("expected 'i64', found 'String'"),
                "{cmd}: {all}"
            );
            assert!(!all.contains("error[mir]"), "{cmd}: {all}");
        }
    }
}

#[test]
fn a_built_program_still_carries_the_checks_warnings() {
    if !cfg!(feature = "llvm") {
        return;
    }
    let path = program(
        "warning",
        "fn main() {\n    let x: i64 = 3;\n    println(x as i64);\n}\n",
    );
    let out = karac(&["build", "--output=json"], &path);
    let all = text(&out);
    assert!(out.status.success(), "{all}");
    assert!(all.contains("\"status\":\"ok\""), "{all}");
    assert!(all.contains("\"lint_name\":\"redundant_cast\""), "{all}");
    let out = karac(&["run"], &path);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "3\n");
    assert!(String::from_utf8_lossy(&out.stderr).contains("warning[redundant_cast]"));
}
