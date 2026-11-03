//! `String.push` of a non-ASCII char goes to the runtime through a copy of
//! the header, never the string's own address: one call with that address
//! keeps a local `String` in memory, and every ASCII push then reloads the
//! length the push before stored (kata 38, B-2026-10-31-37). The pins: a
//! string built from ASCII and wide chars prints the same under both
//! backends, and every `karac_mir_string_push_char` call in the IR is
//! handed a `push.wide` slot.
#![cfg(feature = "llvm")]

mod common;

use std::path::PathBuf;
use std::process::Command;

const SRC: &str = r#"fn say(s: String) -> String {
    let mut out = String.new();
    for c in s.chars() {
        out.push(c);
        if c == 'e' {
            out.push('é');
        }
        out.push('-');
    }
    out.push('日');
    out.push('😀');
    out
}

fn main() {
    let mut s = "tree".to_string();
    let mut i = 0;
    while i < 2 {
        s = say(s);
        i = i + 1;
    }
    println(s);
    println(s.len());
}
"#;

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("karac-push-wide-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn a_wide_push_hands_the_runtime_a_copy_of_the_header() {
    let d = dir("pin");
    let file = d.join("main.kara");
    std::fs::write(&file, SRC).unwrap();
    let karac = || {
        let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
        c.env("KARAC_AUTO_PAR", "0");
        c
    };
    let interp = karac()
        .args(["__mir-run", file.to_str().unwrap()])
        .output()
        .unwrap();
    let want = String::from_utf8_lossy(&interp.stdout).to_string();
    assert_eq!(
        want,
        "t---r---eé-é---eé-é---日-😀-日😀\n42\n",
        "{}",
        String::from_utf8_lossy(&interp.stderr)
    );
    let exe = d.join("main");
    let ll = d.join("main.ll");
    let built = karac()
        .env("KARAC_MIR_LL", &ll)
        .args(["build", file.to_str().unwrap(), "-o", exe.to_str().unwrap()])
        .output()
        .unwrap();
    let why = String::from_utf8_lossy(&built.stderr).to_string();
    if common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(why)
    })
    .is_none()
    {
        return;
    }
    let ran = Command::new(&exe).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&ran.stdout), want);
    let ir = std::fs::read_to_string(&ll).unwrap();
    let calls: Vec<&str> = ir
        .lines()
        .filter(|l| l.contains("call void @karac_mir_string_push_char("))
        .collect();
    assert!(!calls.is_empty(), "no wide push in the IR");
    for c in calls {
        assert!(c.contains("(ptr %push.wide"), "{c}");
    }
    let _ = std::fs::remove_dir_all(&d);
}
