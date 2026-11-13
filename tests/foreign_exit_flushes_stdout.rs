//! B-2026-11-13-75: standard output is buffered for a pipe or a file, and a
//! program that ended through a foreign `exit()` left the buffer unwritten,
//! so compiled code printed nothing that came before it, while the MIR
//! interpreter printed it. The runtime now writes the buffer from an
//! `atexit` handler, on one worker and on the pool.

use std::path::PathBuf;
use std::process::Command;

const SRC: &str = "unsafe extern \"C\" { fn exit(code: i32) with panics; }\n\
fn main() {\n    println(\"a\");\n    unsafe { exit(3); }\n    println(\"b\");\n}\n";

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-foreign-exit-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.kara"), SRC).unwrap();
    dir
}

/// stdout and exit code of `cmd` under `KARAC_THREADS=threads`.
fn outcome(mut cmd: Command, threads: &str) -> (String, Option<i32>, String) {
    let out = cmd.env("KARAC_THREADS", threads).output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.code(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn foreign_exit_keeps_earlier_output_in_the_interpreter() {
    let dir = scratch("interp");
    for threads in ["1", "4"] {
        let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
        c.arg("__mir-run").arg(dir.join("main.kara"));
        let (out, code, err) = outcome(c, threads);
        assert_eq!((out.as_str(), code), ("a\n", Some(3)), "{threads}: {err}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn foreign_exit_flushes_stdout_in_a_built_binary() {
    if !cfg!(feature = "llvm") || !cfg!(unix) {
        return;
    }
    let dir = scratch("build");
    let built = Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(&dir)
        .args(["build", "main.kara", "-o", "main.out"])
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&built.stderr);
    if !built.status.success() {
        assert!(
            err.contains("Linker failed")
                && std::env::var_os("KARAC_REQUIRE_RUNTIME_ARCHIVE").is_none(),
            "does not build: {err}"
        );
        eprintln!("skipping the run: no runtime archive");
        return;
    }
    for threads in ["1", "4"] {
        let (out, code, err) = outcome(Command::new(dir.join("main.out")), threads);
        assert_eq!((out.as_str(), code), ("a\n", Some(3)), "{threads}: {err}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn foreign_exit_flushes_stdout_under_the_jit_runner() {
    if !cfg!(feature = "llvm") || !cfg!(unix) {
        return;
    }
    let dir = scratch("jit");
    for threads in ["1", "4"] {
        let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
        c.arg("__mir-llvm-run").arg(dir.join("main.kara"));
        let (out, code, err) = outcome(c, threads);
        assert_eq!((out.as_str(), code), ("a\n", Some(3)), "{threads}: {err}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
