//! `TcpStream.read_to_end` (docs/library/io.md § Reading to the end,
//! B-2026-11-13-73): everything up to the peer's close, appended to the
//! buffer across reads larger than one chunk, with the count appended; an
//! FTP data connection is read this way. There was no such method, and a
//! Rust `data.read_to_end(&mut got)` had no Kāra spelling short of a `read`
//! loop. On the MIR interpreter and as a built binary alike.
#![cfg(feature = "llvm")]

mod common;

use std::process::{Command, Stdio};

#[test]
fn tcp_read_to_end_appends_everything_up_to_the_close() {
    let dir = std::env::temp_dir().join(format!("karac-read-to-end-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(
        &file,
        r#"fn main() {
    let l = TcpListener.bind("127.0.0.1:0").unwrap();
    let c = TcpStream.connect(l.local_addr().unwrap()).unwrap();
    let s = l.accept().unwrap();
    let block = Vec.filled(5000, 7u8);
    for _ in 0..4 {
        s.write_all(block).unwrap();
    }
    s.write_all("end".bytes()).unwrap();
    drop(s);
    let mut got: Vec[u8] = "pre:".bytes().to_vec();
    let n = c.read_to_end(mut got).unwrap();
    println(f"{n} {got.len()} {got[4]} {got[got.len() - 1]}");
    let mut more = Vec.new();
    println(f"{c.read_to_end(mut more).unwrap()} {more.len()}");
}
"#,
    )
    .unwrap();
    let karac = env!("CARGO_BIN_EXE_karac");
    let want = "20003 20007 7 100\n0 0\n";
    let interp = Command::new(karac)
        .args(["__mir-run", file.to_str().unwrap()])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        interp.status.success(),
        "{}",
        String::from_utf8_lossy(&interp.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&interp.stdout), want);
    let exe = dir.join("main");
    let built = Command::new(karac)
        .args(["build", file.to_str().unwrap(), "-o", exe.to_str().unwrap()])
        .output()
        .unwrap();
    let linked = common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&built.stderr).to_string())
    });
    if linked.is_some() {
        let ran = Command::new(&exe).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&ran.stdout), want);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
