//! The 2026-11-21 HTTP/1.1 and WebSocket conformance pass
//! (/mnt/project-files/conformance/http/), on both backends
//! (B-2026-11-21-22, -23):
//!
//! - a request field value holding a control other than HTAB is refused,
//!   so `Transfer-Encoding:<VT>chunked` is not read as chunked by this
//!   server while a peer that keeps the byte reads it as no coding;
//! - a chunked body's lines end in CRLF, its extensions follow RFC 9112
//!   §7.1.1's grammar and its trailers are field lines;
//! - an HTTP/1.0 request with a Transfer-Encoding is answered, then its
//!   connection closes (RFC 9112 §6.1);
//! - a request target outside visible ASCII is refused;
//! - a WebSocket handshake without `Connection: Upgrade` is refused 400
//!   (RFC 6455 §4.2.1), on `serve_ws` and `WebSocket.accept` alike.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

fn scratch(tag: &str, src: &str) -> std::path::PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "karac-http-conformance-{tag}-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.kara"), src).unwrap();
    dir
}

/// A running server program and the port it announced.
struct Served {
    child: Child,
    port: u16,
    dir: std::path::PathBuf,
}

impl Drop for Served {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn serve(mode: &str, tag: &str, src: &str) -> Served {
    let dir = scratch(tag, src);
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_karac"));
    cmd.arg(mode)
        .arg(dir.join("main.kara"))
        .current_dir(&dir)
        .env("KARAC_ANNOUNCE_PORT", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
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
    let Some(port) = port else {
        let _ = child.kill();
        let mut out = String::new();
        let _ = child.stdout.take().unwrap().read_to_string(&mut out);
        panic!("{mode}: no BOUND_PORT; last stderr {line:?}; stdout {out:?}");
    };
    // Keep the pipe drained so the server never blocks on it.
    std::thread::spawn(move || {
        let mut sink = Vec::new();
        let _ = err.read_to_end(&mut sink);
    });
    Served { child, port, dir }
}

/// Sends `request` on a fresh connection and reads until the server
/// closes it or `wait` passes with nothing new: what came, and whether the
/// server closed.
fn exchange(port: u16, request: &[u8], wait: Duration) -> (String, bool) {
    let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
    c.set_read_timeout(Some(wait)).unwrap();
    c.write_all(request).unwrap();
    let mut got = Vec::new();
    let mut buf = [0u8; 65536];
    let closed = loop {
        match c.read(&mut buf) {
            Ok(0) => break true,
            Ok(n) => got.extend_from_slice(&buf[..n]),
            // A reset after a refusal is a close too.
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => break true,
            Err(_) => break false,
        }
    };
    (strip_date(&String::from_utf8_lossy(&got)), closed)
}

fn strip_date(s: &str) -> String {
    s.split("\r\n")
        .filter(|l| !l.to_ascii_lowercase().starts_with("date:"))
        .collect::<Vec<_>>()
        .join("\r\n")
}

const ECHO: &str = r#"
fn handle(req: Request) -> Response {
    if req.path() == "/ws" {
        return Response { status: 101, body: String.new() };
    }
    Response { status: 200, body: f"{req.method()} {req.path()} body={req.body()}" }
}

fn echo(ws: own WebSocket) {
    let mut ws = ws;
    loop {
        match ws.recv() {
            Ok(WsMessage.Text(t)) => { if ws.send_text(t).is_err() { break; } }
            Ok(WsMessage.Close(_)) => { break; }
            Ok(_) => {}
            Err(_) => { break; }
        }
    }
}

fn main() {
    let _ = Server.serve_ws("127.0.0.1:0", handle, echo);
}
"#;

const ACCEPT: &str = r#"
fn main() {
    let l = TcpListener.bind("127.0.0.1:0").unwrap();
    loop {
        match WebSocket.accept(l) {
            Ok(ws) => { let mut ws = ws; let _ = ws.close(1000u16); }
            Err(_) => {}
        }
    }
}
"#;

fn refusal(status: &str) -> String {
    format!("HTTP/1.1 {status}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
}

fn ok(body: &str, close: bool) -> String {
    format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/plain; charset=utf-8\r\ncontent-length: {}\r\n{}\r\n{body}",
        body.len(),
        if close { "connection: close\r\n" } else { "" }
    )
}

fn upgrade(connection: &str) -> String {
    format!(
        "GET /ws HTTP/1.1\r\nHost: h\r\nUpgrade: websocket\r\nConnection: {connection}\r\n\
         Sec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n"
    )
}

fn framing_is_read_one_way(mode: &str) {
    let s = serve(mode, "cells", ECHO);
    let short = Duration::from_millis(600);
    let post = |rest: &str| format!("POST /p HTTP/1.1\r\nHost: h\r\n{rest}");
    let chunked = |body: &str| post(&format!("Transfer-Encoding: chunked\r\n\r\n{body}"));
    let bad = refusal("400 Bad Request");
    let ws_bad = "HTTP/1.1 400 Bad Request\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";
    let cells: Vec<(&str, String, String, bool)> = vec![
        ("VT before chunked", post("Transfer-Encoding:\x0bchunked\r\n\r\n3\r\nabc\r\n0\r\n\r\n"), bad.clone(), true),
        ("control in a value", post("X-A: a\x01b\r\n\r\n"), bad.clone(), true),
        ("bare LF after a size", chunked("3\nabc\r\n0\r\n\r\n"), bad.clone(), true),
        ("bare LF in an extension", chunked("3;x\nabc\r\n0\r\n\r\n"), bad.clone(), true),
        ("bare LF after data", chunked("3\r\nabc\n0\r\n\r\n"), bad.clone(), true),
        ("bad extension", chunked("3;\x01\r\nabc\r\n0\r\n\r\n"), bad.clone(), true),
        ("bad trailer", chunked("3\r\nabc\r\n0\r\nnot a field\r\n\r\n"), bad.clone(), true),
        ("non-ASCII target", "GET /\u{e9} HTTP/1.1\r\nHost: h\r\n\r\n".into(), bad.clone(), true),
        ("extension", chunked("3;n=\"a b\"\r\nabc\r\n0\r\nX-T: 1\r\n\r\n"), ok("POST /p body=abc", false), false),
        (
            "HTTP/1.0 with TE",
            "POST /t HTTP/1.0\r\nConnection: keep-alive\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n0\r\n\r\n".into(),
            ok("POST /t body=abc", true),
            true,
        ),
        ("no Connection: Upgrade", upgrade("keep-alive"), ws_bad.into(), true),
    ];
    let mut wrong = Vec::new();
    for (cell, req, want, closed) in &cells {
        let got = exchange(s.port, req.as_bytes(), short);
        if got != (want.clone(), *closed) {
            wrong.push(format!("{cell}: got {got:?}\n   want {:?}", (want, closed)));
        }
    }
    let up = exchange(s.port, upgrade("keep-alive, Upgrade").as_bytes(), short).0;
    if !up.starts_with("HTTP/1.1 101 ") {
        wrong.push(format!("Connection list with Upgrade: {up:?}"));
    }
    assert!(wrong.is_empty(), "{mode}:\n{}", wrong.join("\n"));
}

#[test]
fn interp_framing_is_read_one_way() {
    framing_is_read_one_way("__mir-run");
}

#[cfg(feature = "llvm")]
#[test]
fn compiled_framing_is_read_one_way() {
    framing_is_read_one_way("__mir-llvm-run");
}

fn accept_wants_connection_upgrade(mode: &str) {
    let s = serve(mode, "accept", ACCEPT);
    let short = Duration::from_millis(600);
    let (refused, _) = exchange(s.port, upgrade("keep-alive").as_bytes(), short);
    assert!(refused.starts_with("HTTP/1.1 400 "), "{mode}: {refused:?}");
    let (up, _) = exchange(s.port, upgrade("Upgrade").as_bytes(), short);
    assert!(up.starts_with("HTTP/1.1 101 "), "{mode}: {up:?}");
}

#[test]
fn interp_accept_wants_connection_upgrade() {
    accept_wants_connection_upgrade("__mir-run");
}

#[cfg(feature = "llvm")]
#[test]
fn compiled_accept_wants_connection_upgrade() {
    accept_wants_connection_upgrade("__mir-llvm-run");
}
