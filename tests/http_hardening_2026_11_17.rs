//! docs/library/io.md § HTTP server and § HTTP client, from the HTTP probe
//! of 2026-11-17 and the review-thread rulings on it, on both backends:
//!
//! - the server refuses an ambiguous or malformed request before a handler
//!   sees it (B-2026-11-17-44, -46), and limits a request's head, body and
//!   time (-50): 400, 413, 431, 501, 408;
//! - a response is framed by the server: a handler's header holding CR,
//!   LF or NUL is a 500 (-45), `HEAD`, `1xx`, `204` and `304` go without a
//!   body (-47), a status outside 100..=999 is a 500 and a handler's
//!   `Transfer-Encoding` and `Connection` do not reach the wire as framing
//!   (-48); `Connection` is a list (-52); `Expect: 100-continue` is
//!   answered (-53);
//! - a failed `accept` is retried, so running out of descriptors does not
//!   stop the server (-49);
//! - the client sends the body's length whatever the program's headers say,
//!   and one Host, the program's own when it sets one (-54); it
//!   refuses an empty method on both backends (-55), and reads an IPv6
//!   literal, a port over 65535 and a query with no path as RFC 3986
//!   does (-56);
//! - the WebSocket server refuses close codes a peer may not send and a
//!   handshake that is not version 13 or whose key is not 16 bytes (-57).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

fn scratch(tag: &str, src: &str) -> std::path::PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "karac-http-hardening-{tag}-{}-{n}",
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

fn serve(mode: &str, tag: &str, src: &str, files: Option<u64>) -> Served {
    let dir = scratch(tag, src);
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_karac"));
    cmd.arg(mode)
        .arg(dir.join("main.kara"))
        .current_dir(&dir)
        .env("KARAC_ANNOUNCE_PORT", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(n) = files {
        use std::os::unix::process::CommandExt;
        // SAFETY: setrlimit is async-signal-safe and touches only the child.
        unsafe {
            cmd.pre_exec(move || {
                let lim = libc::rlimit {
                    rlim_cur: n,
                    rlim_max: n,
                };
                if libc::setrlimit(libc::RLIMIT_NOFILE, &lim) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
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
struct Response { status: i64, body: String, headers: Vec[(String, String)] }

fn hv(name: own String, v: own String) -> Vec[(String, String)] {
    let mut h: Vec[(String, String)] = Vec.new();
    h.push((name, v));
    h
}

fn handle(req: Request) -> Response {
    let p = req.path();
    if p == "/204" { return Response { status: 204, body: "nobody", headers: Vec.new() }; }
    if p == "/304" { return Response { status: 304, body: "nobody", headers: Vec.new() }; }
    if p == "/inject" { return Response { status: 200, body: "i", headers: hv("X-I".to_string(), "a\r\nSet-Cookie: evil=1".to_string()) }; }
    if p == "/injname" { return Response { status: 200, body: "i", headers: hv("X-I: z\r\nX-J".to_string(), "b".to_string()) }; }
    if p == "/te" { return Response { status: 200, body: "hello", headers: hv("Transfer-Encoding".to_string(), "chunked".to_string()) }; }
    if p == "/conn" { return Response { status: 200, body: "c", headers: hv("Connection".to_string(), "close".to_string()) }; }
    if p == "/s99" { return Response { status: 99, body: "x", headers: Vec.new() }; }
    if p == "/s-1" { return Response { status: -1, body: "x", headers: Vec.new() }; }
    Response { status: 200, body: f"{req.method()} {p} n={req.bytes().len()} body={req.body()}", headers: hv("Content-Type".to_string(), "text/plain".to_string()) }
}

fn main() {
    match Server.serve("127.0.0.1:0", handle) {
        Ok(_) => println("served"),
        Err(e) => println(f"serve: {e.message()}"),
    }
}
"#;

fn refusal(status: &str) -> String {
    format!("HTTP/1.1 {status}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
}

fn ok(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    )
}

const SERVER_ERROR: &str = "HTTP/1.1 500 Internal Server Error\r\ncontent-type: application/json\r\ncontent-length: 0\r\n\r\n";

fn server_refuses_and_frames(mode: &str) {
    let s = serve(mode, "cells", ECHO, None);
    let short = Duration::from_millis(600);
    let h = |rest: &str| format!("POST /p HTTP/1.1\r\nHost: h\r\n{rest}");
    let bad = refusal("400 Bad Request");
    // (cell, request, expected bytes, closed)
    let cells: Vec<(&str, String, String, bool)> = vec![
        (
            "S10 CL+TE",
            h("Content-Length: 6\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\nGET /smuggled HTTP/1.1\r\nHost: h\r\n\r\n"),
            bad.clone(),
            true,
        ),
        ("S13 two CL", h("Content-Length: 5\r\nContent-Length: 6\r\n\r\nhello!"), bad.clone(), true),
        ("S17 +CL", h("Content-Length: +5\r\n\r\nhello"), bad.clone(), true),
        (
            "S11 gzip",
            h("Transfer-Encoding: gzip, chunked\r\n\r\n0\r\n\r\n"),
            refusal("501 Not Implemented"),
            true,
        ),
        ("S04 space colon", h("X-A : 1\r\n\r\n"), bad.clone(), true),
        ("S48 empty name", h(": v\r\n\r\n"), bad.clone(), true),
        ("S29 CR in value", h("X-A: a\rb\r\n\r\n"), bad.clone(), true),
        ("S27 HTTP/2.0", "GET / HTTP/2.0\r\nHost: h\r\n\r\n".into(), bad.clone(), true),
        ("S27c HTTP/1.1x", "GET / HTTP/1.1x\r\nHost: h\r\n\r\n".into(), bad.clone(), true),
        ("S25 no Host", "GET / HTTP/1.1\r\n\r\n".into(), bad.clone(), true),
        ("S26 two Hosts", "GET / HTTP/1.1\r\nHost: a\r\nHost: b\r\n\r\n".into(), bad.clone(), true),
        (
            "S32 big head",
            format!("GET / HTTP/1.1\r\nHost: h\r\nX-L: {}\r\n\r\n", "a".repeat(70_000)),
            refusal("431 Request Header Fields Too Large"),
            true,
        ),
        (
            "big body",
            h("Content-Length: 16777217\r\n\r\n"),
            refusal("413 Content Too Large"),
            true,
        ),
        (
            "/inject",
            "GET /inject HTTP/1.1\r\nHost: h\r\n\r\n".into(),
            SERVER_ERROR.into(),
            false,
        ),
        (
            "/injname",
            "GET /injname HTTP/1.1\r\nHost: h\r\n\r\n".into(),
            SERVER_ERROR.into(),
            false,
        ),
        ("/s99", "GET /s99 HTTP/1.1\r\nHost: h\r\n\r\n".into(), SERVER_ERROR.into(), false),
        ("/s-1", "GET /s-1 HTTP/1.1\r\nHost: h\r\n\r\n".into(), SERVER_ERROR.into(), false),
        (
            "S34 HEAD",
            "HEAD /e HTTP/1.1\r\nHost: h\r\n\r\n".into(),
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\ncontent-length: 17\r\n\r\n".into(),
            false,
        ),
        (
            "S35 204",
            "GET /204 HTTP/1.1\r\nHost: h\r\n\r\n".into(),
            "HTTP/1.1 204 No Content\r\n\r\n".into(),
            false,
        ),
        (
            "S35b 304",
            "GET /304 HTTP/1.1\r\nHost: h\r\n\r\n".into(),
            "HTTP/1.1 304 Not Modified\r\n\r\n".into(),
            false,
        ),
        (
            "S38 handler TE",
            "GET /te HTTP/1.1\r\nHost: h\r\n\r\n".into(),
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 5\r\n\r\nhello".into(),
            false,
        ),
        (
            "S39 handler close",
            "GET /conn HTTP/1.1\r\nHost: h\r\n\r\n".into(),
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 1\r\nconnection: close\r\n\r\nc".into(),
            true,
        ),
        (
            "S52 keep-alive, close",
            "GET /k HTTP/1.1\r\nHost: h\r\nConnection: keep-alive, close\r\n\r\n".into(),
            ok("GET /k n=0 body=").replace("\r\n\r\n", "\r\nconnection: close\r\n\r\n"),
            true,
        ),
        (
            "chunked still served",
            h("Transfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n0\r\n\r\n"),
            ok("POST /p n=3 body=abc"),
            false,
        ),
    ];
    let mut wrong = Vec::new();
    for (cell, req, want, closed) in &cells {
        let got = exchange(s.port, req.as_bytes(), short);
        if got != (want.clone(), *closed) {
            wrong.push(format!("{cell}: got {got:?}\n   want {:?}", (want, closed)));
        }
    }
    assert!(wrong.is_empty(), "{mode}:\n{}", wrong.join("\n"));
}

#[test]
fn interp_server_refuses_and_frames() {
    server_refuses_and_frames("__mir-run");
}

#[cfg(feature = "llvm")]
#[test]
fn compiled_server_refuses_and_frames() {
    server_refuses_and_frames("__mir-llvm-run");
}

/// S55: the interim 100 comes before the body is sent, then the answer.
fn expect_continue_is_answered(mode: &str) {
    let s = serve(mode, "expect", ECHO, None);
    let mut c = TcpStream::connect(("127.0.0.1", s.port)).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    c.write_all(
        b"POST /x HTTP/1.1\r\nHost: h\r\nContent-Length: 5\r\nExpect: 100-continue\r\n\r\n",
    )
    .unwrap();
    let mut buf = [0u8; 256];
    let n = c.read(&mut buf).unwrap();
    assert_eq!(
        String::from_utf8_lossy(&buf[..n]),
        "HTTP/1.1 100 Continue\r\n\r\n",
        "{mode}"
    );
    c.write_all(b"hello").unwrap();
    let n = c.read(&mut buf).unwrap();
    assert_eq!(
        strip_date(&String::from_utf8_lossy(&buf[..n])),
        ok("POST /x n=5 body=hello"),
        "{mode}"
    );
}

#[test]
fn interp_expect_continue_is_answered() {
    expect_continue_is_answered("__mir-run");
}

#[cfg(feature = "llvm")]
#[test]
fn compiled_expect_continue_is_answered() {
    expect_continue_is_answered("__mir-llvm-run");
}

/// A head not whole 30 s after its first byte is answered 408; a
/// connection that sends nothing is closed silently at 60 s; a connection
/// that is sending its head holds no other (-50). One test of about a
/// minute per backend.
fn slow_connections_time_out(mode: &str) {
    let s = serve(mode, "timeouts", ECHO, None);
    let started = Instant::now();
    let port = s.port;
    let partial = std::thread::spawn(move || {
        let got = exchange(port, b"GET /x HTTP/1.1\r\nHo", Duration::from_secs(75));
        (got, started.elapsed())
    });
    let silent = std::thread::spawn(move || {
        let got = exchange(port, b"", Duration::from_secs(75));
        (got, started.elapsed())
    });
    // Meanwhile the server answers others.
    std::thread::sleep(Duration::from_millis(500));
    let other = exchange(
        port,
        b"GET /o HTTP/1.1\r\nHost: h\r\nConnection: close\r\n\r\n",
        Duration::from_secs(5),
    );
    assert_eq!(
        other.0,
        ok("GET /o n=0 body=").replace("\r\n\r\n", "\r\nconnection: close\r\n\r\n"),
        "{mode}"
    );
    let ((partial, at), (silent, silent_at)) = (partial.join().unwrap(), silent.join().unwrap());
    assert_eq!(partial, (refusal("408 Request Timeout"), true), "{mode}");
    assert!(
        (Duration::from_secs(29)..Duration::from_secs(45)).contains(&at),
        "{mode}: 408 after {at:?}"
    );
    assert_eq!(silent, (String::new(), true), "{mode}");
    assert!(
        (Duration::from_secs(59)..Duration::from_secs(75)).contains(&silent_at),
        "{mode}: closed after {silent_at:?}"
    );
}

#[test]
fn interp_slow_connections_time_out() {
    slow_connections_time_out("__mir-run");
}

#[cfg(feature = "llvm")]
#[test]
fn compiled_slow_connections_time_out() {
    slow_connections_time_out("__mir-llvm-run");
}

/// With 128 descriptors and 200 connections held open, `accept` fails
/// with EMFILE; once they close, the server answers again (-49).
fn accept_failures_are_retried(mode: &str) {
    let s = serve(mode, "emfile", ECHO, Some(128));
    let mut held = Vec::new();
    for _ in 0..200 {
        if let Ok(mut c) = TcpStream::connect(("127.0.0.1", s.port)) {
            let _ = c.write_all(b"GET /x HTTP/1.1\r\nHo");
            held.push(c);
        }
    }
    std::thread::sleep(Duration::from_millis(500));
    drop(held);
    // The connections the server did accept see their peers go; the ones
    // still queued are accepted and closed as the pause allows.
    let deadline = Instant::now() + Duration::from_secs(20);
    let got = loop {
        let got = exchange(
            s.port,
            b"GET /after HTTP/1.1\r\nHost: h\r\nConnection: close\r\n\r\n",
            Duration::from_secs(3),
        );
        if got.0.starts_with("HTTP/1.1 200") || Instant::now() > deadline {
            break got;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    assert_eq!(
        got.0,
        ok("GET /after n=0 body=").replace("\r\n\r\n", "\r\nconnection: close\r\n\r\n"),
        "{mode}"
    );
}

#[test]
fn interp_accept_failures_are_retried() {
    accept_failures_are_retried("__mir-run");
}

#[cfg(feature = "llvm")]
#[test]
fn compiled_accept_failures_are_retried() {
    accept_failures_are_retried("__mir-llvm-run");
}

/// A raw listener that records the head and body of each request it gets
/// and answers 200 with a two-byte body.
fn capture() -> (u16, std::sync::mpsc::Receiver<String>) {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for c in l.incoming() {
            let Ok(mut c) = c else { continue };
            let _ = c.set_read_timeout(Some(Duration::from_millis(400)));
            let mut got = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                match c.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => got.extend_from_slice(&buf[..n]),
                }
                if let Some(h) = got.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&got[..h]).to_ascii_lowercase();
                    let len = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length: "))
                        .and_then(|v| v.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if got.len() >= h + 4 + len {
                        break;
                    }
                }
            }
            let _ =
                c.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok");
            let mut lines: Vec<String> = String::from_utf8_lossy(&got)
                .split("\r\n")
                .map(str::to_string)
                .filter(|l| !l.starts_with("user-agent") && !l.starts_with("accept"))
                .collect();
            lines.retain(|l| !l.is_empty());
            let _ = tx.send(lines.join("|"));
        }
    });
    (port, rx)
}

const CLIENT: &str = r#"
fn st(tag: Str, r: Result[Response, HttpError]) {
    match r {
        Ok(resp) => println(f"{tag} OK {resp.status()}"),
        Err(e) => println(f"{tag} ERR {e.message()}"),
    }
}
fn main() {
    let c = Client.new();
    let a = "http://127.0.0.1:PORT";
    st("cl-short", c.request("POST".to_string(), f"{a}/cl").header("Content-Length".to_string(), "1".to_string()).body("hello".to_string()).send());
    st("cl-long", c.request("POST".to_string(), f"{a}/cl2").header("content-length".to_string(), "50".to_string()).body("hello".to_string()).send());
    st("host", c.request("GET".to_string(), f"{a}/h").header("Host".to_string(), "evil.example".to_string()).send());
    st("query-only", c.request("GET".to_string(), "http://127.0.0.1:PORT?x=1".to_string()).send());
    st("empty-method", c.request("".to_string(), f"{a}/em").send());
    st("big-port", c.request("GET".to_string(), "http://127.0.0.1:99999/".to_string()).send());
}
"#;

fn client_sends_its_own_framing(mode: &str) {
    let (port, rx) = capture();
    let dir = scratch("client", &CLIENT.replace("PORT", &port.to_string()));
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(mode)
        .arg(dir.join("main.kara"))
        .current_dir(&dir)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert_eq!(
        stdout,
        format!(
            "cl-short OK 200\ncl-long OK 200\nhost OK 200\nquery-only OK 200\n\
             empty-method ERR http://127.0.0.1:{port}/em: bad method: invalid HTTP method\n\
             big-port ERR http://127.0.0.1:99999/: bad url: invalid port 99999\n"
        ),
        "{mode}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // The empty method is refused after the connection is made, so it
    // leaves one that sent nothing.
    let got: Vec<String> = rx.try_iter().filter(|r| !r.is_empty()).collect();
    let hp = format!("host: 127.0.0.1:{port}");
    assert_eq!(
        got,
        vec![
            format!("POST /cl HTTP/1.1|{hp}|content-length: 5|hello"),
            format!("POST /cl2 HTTP/1.1|{hp}|content-length: 5|hello"),
            "GET /h HTTP/1.1|host: evil.example".to_string(),
            format!("GET /?x=1 HTTP/1.1|{hp}"),
        ],
        "{mode}"
    );
}

#[test]
fn interp_client_sends_its_own_framing() {
    client_sends_its_own_framing("__mir-run");
}

#[cfg(feature = "llvm")]
#[test]
fn compiled_client_sends_its_own_framing() {
    client_sends_its_own_framing("__mir-llvm-run");
}

/// An IPv6 literal reaches its address (-56), when the host has one.
fn client_reaches_an_ipv6_literal(mode: &str) {
    let Ok(l) = std::net::TcpListener::bind("[::1]:0") else {
        return;
    };
    let port = l.local_addr().unwrap().port();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        if let Ok((mut c, _)) = l.accept() {
            let mut buf = [0u8; 4096];
            let n = c.read(&mut buf).unwrap_or(0);
            let _ = tx.send(String::from_utf8_lossy(&buf[..n]).to_string());
            let _ =
                c.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok");
        }
    });
    let src = r#"
fn main() {
    match Client.new().get("http://[::1]:PORT/six".to_string()) {
        Ok(r) => println(f"OK {r.status()} {r.body()}"),
        Err(e) => println(f"ERR {e.message()}"),
    }
}
"#;
    let dir = scratch("ipv6", &src.replace("PORT", &port.to_string()));
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(mode)
        .arg(dir.join("main.kara"))
        .current_dir(&dir)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "OK 200 ok\n",
        "{mode}"
    );
    let head = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(
        head.starts_with("GET /six HTTP/1.1\r\n")
            && head
                .to_ascii_lowercase()
                .contains(&format!("\r\nhost: [::1]:{port}\r\n")),
        "{mode}: {head:?}"
    );
}

#[test]
fn interp_client_reaches_an_ipv6_literal() {
    client_reaches_an_ipv6_literal("__mir-run");
}

#[cfg(feature = "llvm")]
#[test]
fn compiled_client_reaches_an_ipv6_literal() {
    client_reaches_an_ipv6_literal("__mir-llvm-run");
}

const WS: &str = r#"
fn main() {
    let l = TcpListener.bind("127.0.0.1:0").unwrap();
    loop {
        match WebSocket.accept(l) {
            Ok(ws) => {
                let mut ws = ws;
                loop {
                    match ws.recv() {
                        Ok(WsMessage.Text(t)) => { let _ = ws.send_text(t); }
                        Ok(WsMessage.Close(c)) => { println(f"close {c}"); break; }
                        Ok(_) => {}
                        Err(e) => { println(f"err {e}"); break; }
                    }
                }
            }
            Err(e) => println(f"accept {e}"),
        }
    }
}
"#;

fn handshake(version: Option<&str>, key: &str) -> String {
    let mut r = format!("GET /ws HTTP/1.1\r\nHost: h\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\n");
    if let Some(v) = version {
        r.push_str(&format!("Sec-WebSocket-Version: {v}\r\n"));
    }
    r.push_str("\r\n");
    r
}

/// A masked client frame.
fn frame(op: u8, payload: &[u8]) -> Vec<u8> {
    let mask = [1u8, 2, 3, 4];
    let mut f = vec![0x80 | op, 0x80 | payload.len() as u8];
    f.extend_from_slice(&mask);
    f.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
    f
}

/// What the server sends after the handshake: the close frame's code.
fn close_answer(port: u16, payload: &[u8]) -> Option<u16> {
    let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    c.write_all(handshake(Some("13"), "dGhlIHNhbXBsZSBub25jZQ==").as_bytes())
        .unwrap();
    c.write_all(&frame(8, payload)).unwrap();
    let mut got = Vec::new();
    let mut buf = [0u8; 1024];
    while let Ok(n) = c.read(&mut buf) {
        if n == 0 {
            break;
        }
        got.extend_from_slice(&buf[..n]);
    }
    let h = got.windows(4).position(|w| w == b"\r\n\r\n")? + 4;
    let f = &got[h..];
    (f.len() >= 4 && f[0] == 0x88).then(|| u16::from_be_bytes([f[2], f[3]]))
}

fn websocket_close_and_handshake(mode: &str) {
    let s = serve(mode, "ws", WS, None);
    let short = Duration::from_millis(800);
    let key = "dGhlIHNhbXBsZSBub25jZQ==";
    let upgrade = |v: Option<&str>, k: &str| exchange(s.port, handshake(v, k).as_bytes(), short).0;
    assert_eq!(
        upgrade(Some("8"), key),
        "HTTP/1.1 426 Upgrade Required\r\nSec-WebSocket-Version: 13\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
        "{mode}"
    );
    assert!(upgrade(None, key).starts_with("HTTP/1.1 426 "), "{mode}");
    assert!(
        upgrade(Some("13"), "short").starts_with("HTTP/1.1 400 "),
        "{mode}"
    );
    let post = exchange(
        s.port,
        handshake(Some("13"), key)
            .replacen("GET", "POST", 1)
            .as_bytes(),
        short,
    );
    assert!(post.0.starts_with("HTTP/1.1 400 "), "{mode}: {post:?}");
    assert!(
        upgrade(Some("13"), key).starts_with("HTTP/1.1 101 "),
        "{mode}"
    );
    assert_eq!(
        close_answer(s.port, &1000u16.to_be_bytes()),
        Some(1000),
        "{mode}"
    );
    for bad in [1005u16, 999, 5000, 1015] {
        assert_eq!(
            close_answer(s.port, &bad.to_be_bytes()),
            Some(1002),
            "{mode}: {bad}"
        );
    }
    assert_eq!(
        close_answer(s.port, &[0x03, 0xe8, 0xff]),
        Some(1007),
        "{mode}"
    );
}

#[test]
fn interp_websocket_close_and_handshake() {
    websocket_close_and_handshake("__mir-run");
}

#[cfg(feature = "llvm")]
#[test]
fn compiled_websocket_close_and_handshake() {
    websocket_close_and_handshake("__mir-llvm-run");
}
