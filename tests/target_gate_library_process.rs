//! A host resource used inside the library's own text is refused on a target
//! that lacks it, at the program's last call on the chain (B-2026-11-05-11):
//! `Command.status()` reaches `ProcessTable` only in the library's body, and
//! that diagnostic, in text no file holds, was dropped, so `wasm_wasi` passed
//! a program that spawns a process. It is reported once per call, however
//! many library functions below it use the resource.

use std::process::{Command, Stdio};

#[test]
fn wasm_wasi_refuses_a_process_spawned_through_the_library() {
    let path = std::env::temp_dir().join(format!(
        "karac-target-gate-process-{}.kara",
        std::process::id()
    ));
    std::fs::write(
        &path,
        "import std.process.{Command};
fn spawn_it() -> i64 { match Command.new(\"true\").status() { Ok(_) => 1, Err(_) => 0 } }
fn helper() -> i64 { spawn_it() }
fn main() { println(helper()); }
",
    )
    .unwrap();
    let file = path.to_str().unwrap();
    let karac = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_karac"))
            .args(args)
            .stdin(Stdio::null())
            .output()
            .expect("karac")
    };
    let wasi = karac(&["check", "--target=wasm_wasi", file]);
    let native = karac(&["check", file]);
    let _ = std::fs::remove_file(&path);
    let err =
        String::from_utf8_lossy(&wasi.stderr).into_owned() + &String::from_utf8_lossy(&wasi.stdout);
    assert!(!wasi.status.success(), "{err}");
    assert!(
        err.contains(":2:30: target `wasm_wasi` does not provide resource 'ProcessTable'"),
        "{err}"
    );
    assert!(
        err.contains("main \u{2192} helper \u{2192} spawn_it"),
        "{err}"
    );
    // Once: no provider binds `ProcessTable`, so none is offered (B-2026-11-08-39).
    assert_eq!(err.matches("'ProcessTable'").count(), 1, "{err}");
    assert!(
        native.status.success(),
        "{}",
        String::from_utf8_lossy(&native.stderr)
    );
}
