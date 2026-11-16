//! docs/library/io.md, from the review-thread rulings of 2026-11-16, on both
//! backends:
//!
//! - `BufReader.lines()` ends at its first error, where it returned the
//!   same `Err` for ever from a stream that keeps failing, so a `for` that
//!   only reported it never ended (B-2026-11-16-209);
//! - an HTTP/1.0 request whose connection the server keeps
//!   (`Connection: keep-alive`) is answered `connection: keep-alive`, where
//!   the response said nothing and a 1.0 client read it as the last
//!   (B-2026-11-16-210).

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

fn scratch(tag: &str, src: &str) -> std::path::PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir =
        std::env::temp_dir().join(format!("karac-io-rulings-{tag}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.kara"), src).unwrap();
    dir
}

const LINES: &str = r#"
fn main() {
    let _ = fs.write_bytes("bad.txt", [0x6fu8, 0x6bu8, 0x0au8, 0xffu8, 0x0au8, 0x41u8, 0x0au8]);
    let r = BufReader.new(File.open("bad.txt").unwrap());
    for l in r.lines() {
        match l {
            Ok(t) => println(f"file line [{t}]"),
            Err(e) => println(f"file err {e}"),
        }
    }
    // The reader itself still reads.
    let mut rest = String.new();
    match r.read_line(mut rest) {
        Ok(n) => println(f"read_line after {n} [{rest.trim()}]"),
        Err(e) => println(f"read_line after err {e}"),
    }
    let s = TcpStream.connect("127.0.0.1:PORT").unwrap();
    let _ = s.set_read_timeout(Some(100));
    let br = BufReader.new(s);
    let mut n = 0;
    for l in br.lines() {
        match l {
            Ok(t) => println(f"socket line [{t}]"),
            Err(e) => println(f"socket err {e}"),
        }
        n = n + 1;
        if n > 5 {
            println("still going");
            break;
        }
    }
    println("loop ended");
}
"#;

fn lines_end_at_first_error(mode: &str) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        if let Ok((mut c, _)) = listener.accept() {
            let _ = c.write_all(b"first\n");
            // Hold the connection open and silent past the reader's timeout.
            std::thread::sleep(Duration::from_secs(3));
        }
    });
    let dir = scratch("lines", &LINES.replace("PORT", &port.to_string()));
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(mode)
        .arg(dir.join("main.kara"))
        .current_dir(&dir)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{mode}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "file line [ok]\n\
         file err invalid UTF-8\n\
         read_line after 2 [A]\n\
         socket line [first]\n\
         socket err timed out\n\
         loop ended\n",
        "{mode}"
    );
}

#[test]
fn interp_lines_end_at_first_error() {
    lines_end_at_first_error("__mir-run");
}

#[cfg(feature = "llvm")]
#[test]
fn compiled_lines_end_at_first_error() {
    lines_end_at_first_error("__mir-llvm-run");
}

const SERVER: &str = r#"
fn main() {
    let _ = Server.serve("127.0.0.1:0".to_string(), |req| Response { status: 200, body: req.path() });
}
"#;

/// One request on a fresh connection; the response head, lowercased and
/// without `date`, and whether the server closed the connection after it.
fn exchange(port: u16, request: &[u8]) -> (String, bool) {
    let mut c = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    c.set_read_timeout(Some(Duration::from_millis(700)))
        .unwrap();
    c.write_all(request).unwrap();
    let mut got = Vec::new();
    let mut buf = [0u8; 1024];
    let closed = loop {
        match c.read(&mut buf) {
            Ok(0) => break true,
            Ok(n) => got.extend_from_slice(&buf[..n]),
            Err(_) => break false,
        }
    };
    let text = String::from_utf8_lossy(&got).to_ascii_lowercase();
    let head: Vec<&str> = text
        .split("\r\n")
        .take_while(|l| !l.is_empty())
        .filter(|l| !l.starts_with("date:"))
        .collect();
    (head.join("|"), closed)
}

fn http10_keep_alive_is_answered(mode: &str) {
    let dir = scratch("keep10", SERVER);
    let mut child = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(mode)
        .arg(dir.join("main.kara"))
        .current_dir(&dir)
        .env("KARAC_ANNOUNCE_PORT", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut err = BufReader::new(child.stderr.take().unwrap());
    let mut line = String::new();
    let mut port = None;
    while err.read_line(&mut line).unwrap_or(0) > 0 {
        if let Some(p) = line.trim().strip_prefix("BOUND_PORT=") {
            port = p.parse::<u16>().ok();
            break;
        }
        line.clear();
    }
    let port = port.unwrap_or_else(|| panic!("{mode}: no BOUND_PORT"));
    let kept = exchange(port, b"GET /a HTTP/1.0\r\nConnection: keep-alive\r\n\r\n");
    let plain = exchange(port, b"GET /b HTTP/1.0\r\n\r\n");
    let v11 = exchange(port, b"GET /c HTTP/1.1\r\nHost: x\r\n\r\n");
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        kept,
        (
            "http/1.1 200 ok|content-type: application/json|content-length: 2|connection: keep-alive"
                .to_string(),
            false
        ),
        "{mode}"
    );
    assert_eq!(
        plain,
        (
            "http/1.1 200 ok|content-type: application/json|content-length: 2|connection: close"
                .to_string(),
            true
        ),
        "{mode}"
    );
    assert_eq!(
        v11,
        (
            "http/1.1 200 ok|content-type: application/json|content-length: 2".to_string(),
            false
        ),
        "{mode}"
    );
}

#[test]
fn interp_http10_keep_alive_is_answered() {
    http10_keep_alive_is_answered("__mir-run");
}

#[cfg(feature = "llvm")]
#[test]
fn compiled_http10_keep_alive_is_answered() {
    http10_keep_alive_is_answered("__mir-llvm-run");
}
