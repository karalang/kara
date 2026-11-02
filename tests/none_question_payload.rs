//! `None?` under an expected payload type takes that type (B-2026-11-01-31):
//! `let x: i64 = None?;` types the `None` as `Option[i64]`, as
//! `let o: Option[i64] = None;` does, instead of lowering it with a unit
//! payload into invalid MIR.

use std::path::PathBuf;
use std::process::{Command, Output};

fn program(tag: &str, src: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("karac-none-question-{}-{tag}", std::process::id()));
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

fn all(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// `karac check` passes and the MIR interpreter prints `expected`; under
/// `llvm`, `karac run` prints it too.
fn checks_and_prints(tag: &str, src: &str, expected: &str) {
    let path = program(tag, src);
    let out = karac(&["check"], &path);
    assert!(out.status.success(), "{}", all(&out));
    let mut cmds = vec!["__mir-run"];
    if cfg!(feature = "llvm") {
        cmds.push("run");
    }
    for cmd in cmds {
        let out = karac(&[cmd], &path);
        assert!(out.status.success(), "{cmd}: {}", all(&out));
        assert_eq!(String::from_utf8_lossy(&out.stdout), expected, "{cmd}");
    }
}

/// The expected lines are the program's control flow: every `None?`
/// returns `None` from its function or closure, and `d(true)` is 4 * 2.
#[test]
fn none_question_takes_the_expected_payload() {
    let src = "fn f() -> Option[i64] {\n    let x: i64 = None?;\n    Some(x)\n}\n\n\
               fn m() -> Option[i64] {\n    let x: i64 = Option.None?;\n    Some(x)\n}\n\n\
               fn main() {\n    println(f().is_none());\n    println(m().is_none());\n    \
               let c = || {\n        let x: i64 = None?;\n        Some(x)\n    };\n    \
               let d = |b: bool| {\n        let x: i64 = if b { Some(4)? } else { Option.None? };\n        \
               Some(x * 2)\n    };\n    println(c().is_none());\n    println(d(true).unwrap_or(0));\n    \
               println(d(false).is_none());\n}\n";
    checks_and_prints("let", src, "true\ntrue\ntrue\n8\ntrue\n");
}
