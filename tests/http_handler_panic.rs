//! A handler that panics (B-2026-11-20-10, review-thread ruling
//! 2026-11-20): the server writes one line to standard error naming the
//! request and the panic, answers `500 Internal Server Error` with
//! `connection: close`, and closes that connection; other connections go
//! on. Under `serve_ws`, a session that panics after the 101 is logged the
//! same way and its connection closes.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const SRC: &str = r#"
fn up(req: Request) -> Response {
    if req.path() == "/boom" {
        panic("handler broke\nsecond line");
    }
    if req.path() == "/ws" {
        return Response { status: 101, body: "" };
    }
    Response { status: 200, body: f"ok {req.path()}" }
}

fn session(ws: own WebSocket) {
    let mut w = ws;
    match w.recv() {
        Ok(WsMessage.Text(t)) => panic(f"session got {t}"),
        _ => {}
    }
}

fn main() {
    let _ = Server.serve_ws("127.0.0.1:PORT", |r| up(r), |ws| session(ws));
}
"#;

fn ask(port: u16, raw: &[u8]) -> String {
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut s = loop {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(s) => break s,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => panic!("connect: {e}"),
        }
    };
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    s.write_all(raw).unwrap();
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    String::from_utf8_lossy(&out).into_owned()
}

fn ws_session(port: u16) -> String {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    s.write_all(
        b"GET /ws HTTP/1.1\r\nHost: h\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
          Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n",
    )
    .unwrap();
    let mut head = Vec::new();
    let mut b = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if s.read(&mut b).unwrap_or(0) == 0 {
            break;
        }
        head.push(b[0]);
    }
    // A masked text frame "hi".
    let mask = [1u8, 2, 3, 4];
    let mut frame = vec![0x81, 0x82];
    frame.extend_from_slice(&mask);
    frame.extend(b"hi".iter().enumerate().map(|(i, c)| c ^ mask[i % 4]));
    s.write_all(&frame).unwrap();
    let mut rest = Vec::new();
    let _ = s.read_to_end(&mut rest);
    let status = String::from_utf8_lossy(&head)
        .lines()
        .next()
        .unwrap_or("")
        .to_string();
    format!("{status} then {} bytes", rest.len())
}

fn check(mode: &str) {
    static N: AtomicUsize = AtomicUsize::new(0);
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        l.local_addr().expect("port").port()
    };
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("karac-handler-panic-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, SRC.replace("PORT", &port.to_string())).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(mode)
        .arg(&file)
        .current_dir(&dir)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let boom = ask(port, b"GET /boom HTTP/1.1\r\nHost: h\r\n\r\n");
    let ok = ask(
        port,
        b"GET /fine HTTP/1.1\r\nHost: h\r\nConnection: close\r\n\r\n",
    );
    let ws = ws_session(port);
    let after = ask(
        port,
        b"GET /again HTTP/1.1\r\nHost: h\r\nConnection: close\r\n\r\n",
    );
    std::thread::sleep(Duration::from_millis(300));
    let _ = child.kill();
    let out = child.wait_with_output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        boom,
        "HTTP/1.1 500 Internal Server Error\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
        "{mode}"
    );
    assert!(ok.ends_with("ok /fine"), "{mode}: {ok}");
    assert_eq!(
        ws, "HTTP/1.1 101 Switching Protocols then 0 bytes",
        "{mode}"
    );
    assert!(after.ends_with("ok /again"), "{mode}: {after}");
    let lines: Vec<&str> = err
        .lines()
        .filter(|l| l.starts_with("http server:"))
        .collect();
    assert_eq!(lines.len(), 2, "{mode}: {err}");
    assert!(
        lines[0].starts_with("http server: the handler for GET /boom panicked at ")
            && lines[0].ends_with(": handler broke\\nsecond line"),
        "{mode}: {}",
        lines[0]
    );
    assert!(
        lines[1].starts_with("http server: the WebSocket session for GET /ws panicked at ")
            && lines[1].ends_with(": session got hi"),
        "{mode}: {}",
        lines[1]
    );
}

#[test]
fn interp_handler_panic_is_logged_and_answered_500() {
    check("__mir-run");
}

#[cfg(feature = "llvm")]
#[test]
fn compiled_handler_panic_is_logged_and_answered_500() {
    check("__mir-llvm-run");
}
