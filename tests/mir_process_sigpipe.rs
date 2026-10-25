//! A write to a child's stdin after the child exits fails with an error on
//! both MIR backends. The interpreter runs with `SIGPIPE` ignored, as every
//! Rust program does; a built program's `main` skips Rust's start up, so
//! the runtime ignores the signal itself before the first child with a
//! piped stdin starts (`runtime/src/mir_process.rs` `ignore_sigpipe`).
//! Before that, the write killed the built program with exit 141.
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

const SRC: &str = r#"
fn main() {
    let child = Command.new("true").stdin(Stdio.piped()).spawn().unwrap();
    let w = child.stdin().unwrap();
    let _ = child.wait();
    let buf: Vec[u8] = Vec.filled(65536, 7);
    match w.write_all(buf.as_slice()) {
        Ok(_) => println("wrote"),
        Err(_) => println("write failed"),
    }
    println("survived");
}
"#;

#[test]
fn mir_a_write_to_an_exited_childs_stdin_fails_on_both_backends() {
    let dir = std::env::temp_dir().join(format!("karac-mir-sigpipe-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, SRC).unwrap();
    let karac = env!("CARGO_BIN_EXE_karac");
    let want = "write failed\nsurvived\n";
    let interp = Command::new(karac)
        .arg("__mir-run")
        .arg(&file)
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&interp.stdout), want, "interpreter");
    let exe = dir.join("main");
    let built = Command::new(karac)
        .args(["build", "--mir"])
        .arg(&file)
        .arg("-o")
        .arg(&exe)
        .output()
        .unwrap();
    let linked = common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&built.stderr).to_string())
    });
    if linked.is_some() {
        let run = Command::new(&exe).output().unwrap();
        assert_eq!(
            (
                run.status.code(),
                String::from_utf8_lossy(&run.stdout).as_ref()
            ),
            (Some(0), want),
            "built"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
