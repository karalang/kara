//! A fixed `Array[u8, N]` handed by value to a library native: the
//! interpreter stopped with "expected a sequence, found Agg(..)" on
//! `File.write`, `File.write_all`, `fs.write_bytes`, `String.from_utf8` and
//! `TcpStream.write_all`, where the build ran the program (B-2026-11-25-32; found by
//! conformance/library/io's files_file_surface and
//! net_tcp_bind_port_zero_echo_and_eof).

mod common;

use std::path::PathBuf;
use std::process::Command;

fn program(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-agg-native-{name}-{}", std::process::id()));
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
fn a_byte_array_passed_by_value_reaches_io_natives_on_every_lane() {
    let runs = lanes("io", PROGRAM);
    assert!(!runs.is_empty());
    for (cmd, code, stdout, stderr) in runs {
        assert_eq!(code, Some(0), "{cmd}: {stderr}");
        assert_eq!(stdout, "5\ntrue\ntrue\nhello!!\nhi\nok\nping\n", "{cmd}");
    }
}

const PROGRAM: &str = r#"import std.io.{File};
import std.net.{TcpListener, TcpStream};
fn main() {
    let f = File.create("x.bin").unwrap();
    println(f.write(b"hello").unwrap());
    println(f.write_all([33u8; 2]).is_ok());
    println(fs.write_bytes("y.bin", [104u8, 105]).is_ok());
    println(fs.read_to_string("x.bin").unwrap());
    println(fs.read_to_string("y.bin").unwrap());
    println(String.from_utf8([111u8, 107]).unwrap());
    let l = TcpListener.bind("127.0.0.1:0").unwrap();
    let c = TcpStream.connect(l.local_addr().unwrap()).unwrap();
    let s = l.accept().unwrap();
    c.write_all(b"ping").unwrap();
    let mut got = [0u8; 4];
    s.read_exact(mut got).unwrap();
    println(String.from_utf8(got).unwrap());
}
"#;
