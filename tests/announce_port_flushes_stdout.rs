//! docs/library/io.md § Buffering: standard output on a pipe "is written
//! out before anything is written to stderr". The test-harness line
//! `BOUND_PORT=<n>` (`KARAC_ANNOUNCE_PORT`) is a stderr write, and a
//! compiled program wrote it while its stdout still held what it had
//! printed, so a harness that waits for the port and then reads stdout
//! saw nothing (B-2026-11-16-208).

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

const SRC: &str = r#"
fn main() {
    println("before bind");
    let l = TcpListener.bind("127.0.0.1:0").unwrap();
    let c = l.accept().unwrap();
    let _ = c.write_all("bye".bytes());
}
"#;

fn check(mode: &str) {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("karac-announce-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, SRC).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(mode)
        .arg(&file)
        .current_dir(&dir)
        .env("KARAC_ANNOUNCE_PORT", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut err = BufReader::new(child.stderr.take().unwrap());
    let mut port = None;
    let mut line = String::new();
    while err.read_line(&mut line).unwrap_or(0) > 0 {
        if let Some(p) = line.trim().strip_prefix("BOUND_PORT=") {
            port = p.parse::<u16>().ok();
            break;
        }
        line.clear();
    }
    let port = port.unwrap_or_else(|| panic!("{mode}: no BOUND_PORT line"));
    // stdout must already hold the line printed before the bind, while the
    // program waits in `accept`.
    let mut out = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = [0u8; 64];
        let n = out.read(&mut buf).unwrap_or(0);
        let _ = tx.send(String::from_utf8_lossy(&buf[..n]).into_owned());
    });
    let early = rx.recv_timeout(Duration::from_secs(10));
    let mut c = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    let _ = c.write_all(b"x");
    let mut bye = String::new();
    let _ = c.read_to_string(&mut bye);
    let status = child.wait().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(status.success(), "{mode}: {status}");
    assert_eq!(bye, "bye", "{mode}");
    assert_eq!(
        early.as_deref(),
        Ok("before bind\n"),
        "{mode}: stdout before the port line"
    );
}

#[test]
fn interp_announce_port_comes_after_stdout() {
    check("__mir-run");
}

#[cfg(feature = "llvm")]
#[test]
fn compiled_announce_port_comes_after_stdout() {
    check("__mir-llvm-run");
}
