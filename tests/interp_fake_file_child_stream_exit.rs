//! `Stdio.from_file` of a file a `FileSystem` fake opened stops the program
//! (io.md: a fake's file cannot become a child process's standard stream).
//! The build printed one `error:` line and exited 101; the interpreter
//! reported an internal run error and exited 3 (B-2026-11-25-34).

mod common;

use std::path::PathBuf;
use std::process::Command;

fn program(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-fake-file-stream-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    file
}

/// Exit code, stdout and stderr of `src` on each lane.
fn lanes(name: &str, src: &str) -> Vec<(&'static str, Option<i32>, String, String)> {
    let file = program(name, src);
    let mut cmds = vec!["__mir-run"];
    if cfg!(feature = "llvm") {
        cmds.push("__mir-llvm-run");
    }
    let mut out = Vec::new();
    for cmd in cmds {
        let o = Command::new(env!("CARGO_BIN_EXE_karac"))
            .arg(cmd)
            .arg(&file)
            .current_dir(file.parent().unwrap())
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&o.stderr).to_string();
        if cmd == "__mir-llvm-run"
            && stderr.contains("undefined reference")
            && common::link_or_skip(Err(stderr.clone())).is_none()
        {
            continue;
        }
        out.push((
            cmd,
            o.status.code(),
            String::from_utf8_lossy(&o.stdout).to_string(),
            stderr,
        ));
    }
    let _ = std::fs::remove_dir_all(file.parent().unwrap());
    out
}

#[test]
fn a_fake_file_as_a_child_stream_stops_with_101_on_every_lane() {
    let runs = lanes("fake", PROGRAM);
    assert!(!runs.is_empty());
    for (cmd, code, stdout, stderr) in runs {
        assert_eq!(code, Some(101), "{cmd}: {stderr}");
        assert_eq!(stdout, "", "{cmd}");
        assert!(
            stderr.starts_with("error: Stdio.from_file of a file a FileSystem provider opened"),
            "{cmd}: {stderr}"
        );
    }
}

const PROGRAM: &str = r#"import std.io.{File, FileSystemProvider, OpenOptions};
import std.process.{Command, Stdio};
struct MemFs {}
impl FileSystemProvider for MemFs {
    fn open(mut ref self, path: Str, opts: ref OpenOptions) -> Result[i64, IoError] { Ok(7) }
    fn close(mut ref self, h: i64) {}
}
fn main() {
    with_provider[FileSystem](MemFs {}, || {
        let f = File.create("out.txt").unwrap();
        match Command.new("echo").arg("hi").stdout(Stdio.from_file(f)).status() {
            Ok(s) => println(f"ran {s.success()}"),
            Err(e) => println(f"err[{e}]"),
        }
    });
}
"#;
