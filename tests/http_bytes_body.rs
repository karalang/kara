//! A `Response` whose `body` is `Vec[u8]` (docs/library/io.md § HTTP server,
//! B-2026-11-23-60): a server sends it as it is, `Content-Length` its byte count and
//! `application/octet-stream` when the handler sets no type, and the client
//! fills a `Vec[u8]` body with the bytes as they came. A tracker answering in
//! bencode, a protobuf or CBOR API, a file server: before this a body was a
//! `String`, so a 20-byte hash or a 0xFF could not be sent at all.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The 20-byte SHA-1 of `abc` (FIPS 180-1 appendix A).
const SHA1_ABC: [u8; 20] = [
    0xa9, 0x99, 0x3e, 0x36, 0x47, 0x06, 0x81, 0x6a, 0xba, 0x3e, 0x25, 0x71, 0x78, 0x50, 0xc2, 0x6c,
    0x9c, 0xd0, 0xd8, 0x9d,
];

const SERVER_SRC: &str = r#"
import std.crypto.legacy.{sha1};

struct Response { status: i64, body: Vec[u8], headers: Vec[(String, String)] }

fn handle(req: Request) -> Response {
    let p = req.path();
    if p == "/raw" {
        let body: Vec[u8] = [255u8, 0u8, 65u8, 0u8, 255u8].to_vec();
        Response { status: 200, body: body, headers: Vec.new() }
    } else if p == "/hash" {
        Response { status: 200, body: sha1("abc".bytes()).to_vec(), headers: Vec.new() }
    } else if p == "/typed" {
        let mut h: Vec[(String, String)] = Vec.new();
        h.push(("Content-Type", "text/plain"));
        Response { status: 200, body: "d8:completei1ee".bytes().to_vec(), headers: h }
    } else {
        Response { status: 404, body: Vec.new(), headers: Vec.new() }
    }
}

fn main() {
    let _ = Server.serve("127.0.0.1:0", handle);
}
"#;

/// Starts `src` under `mode`, reads its announced port, and returns the child
/// and the port.
fn start(mode: &str, src: &str, tag: &str) -> (std::process::Child, u16, std::path::PathBuf) {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir =
        std::env::temp_dir().join(format!("karac-bytes-body-{tag}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(mode)
        .arg(&file)
        .current_dir(&dir)
        .env("KARAC_ANNOUNCE_PORT", "1")
        .stderr(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let mut err = BufReader::new(child.stderr.take().unwrap());
    let mut port = None;
    let mut seen = String::new();
    for _ in 0..200 {
        let mut line = String::new();
        if err.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        if let Some(p) = line.trim().strip_prefix("BOUND_PORT=") {
            port = p.parse::<u16>().ok();
            break;
        }
        seen.push_str(&line);
    }
    let Some(port) = port else {
        let _ = child.kill();
        let _ = child.wait();
        panic!("{mode}: the server announces no port:\n{seen}");
    };
    (child, port, dir)
}

/// The raw response to `GET path`, split into its header block (lower-cased)
/// and its body bytes.
fn get(port: u16, path: &str) -> (String, Vec<u8>) {
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(
        s,
        "GET {path} HTTP/1.1\r\nHost: a\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut got = Vec::new();
    let _ = s.read_to_end(&mut got);
    let at = got
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("a header block");
    let head = String::from_utf8_lossy(&got[..at]).to_ascii_lowercase();
    (head, got[at + 4..].to_vec())
}

fn server_sends_bytes(mode: &str) {
    let (mut child, port, dir) = start(mode, SERVER_SRC, "srv");
    let raw = get(port, "/raw");
    let hash = get(port, "/hash");
    let typed = get(port, "/typed");
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(raw.1, [0xff, 0x00, 0x41, 0x00, 0xff], "{mode}: /raw body");
    assert!(raw.0.contains("content-length: 5\r\n"), "{mode}: {}", raw.0);
    assert!(
        raw.0.contains("content-type: application/octet-stream"),
        "{mode}: {}",
        raw.0
    );
    assert_eq!(hash.1, SHA1_ABC, "{mode}: /hash body");
    assert!(
        hash.0.contains("content-length: 20\r\n"),
        "{mode}: {}",
        hash.0
    );
    // A handler's own type wins over the default, and is sent once.
    assert_eq!(typed.1, b"d8:completei1ee", "{mode}: /typed body");
    assert!(
        typed.0.contains("content-type: text/plain\r\n"),
        "{mode}: {}",
        typed.0
    );
    assert_eq!(typed.0.matches("content-type").count(), 1, "{mode}");
}

#[test]
fn interp_server_sends_a_vec_u8_body_as_it_is() {
    server_sends_bytes("__mir-run");
}

#[cfg(feature = "llvm")]
#[test]
fn compiled_server_sends_a_vec_u8_body_as_it_is() {
    server_sends_bytes("__mir-llvm-run");
}

/// Answers every request with 200 and the body `ff 00 41 c3` plus the SHA-1
/// of `abc`: 24 bytes, none of it valid UTF-8 at the start.
fn byte_server() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("port").port();
    std::thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            std::thread::spawn(move || {
                let mut out = conn.try_clone().expect("clone");
                let mut r = BufReader::new(conn);
                let mut line = String::new();
                if r.read_line(&mut line).unwrap_or(0) == 0 {
                    return;
                }
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
                        break;
                    }
                }
                let mut body = vec![0xff, 0x00, 0x41, 0xc3];
                body.extend_from_slice(&SHA1_ABC);
                let mut v = format!(
                    "HTTP/1.1 200 OK\r\nX-Kind: bin\r\nContent-Length: {}\r\n\r\n",
                    body.len()
                )
                .into_bytes();
                v.extend_from_slice(&body);
                let _ = out.write_all(&v);
            });
        }
    });
    port
}

const CLIENT_SRC: &str = r#"
struct Response { status: i64, body: Vec[u8], headers: Vec[(String, String)] }

fn main() {
    let c = Client.new();
    match c.get("http://127.0.0.1:PORT/x") {
        Ok(r) => {
            let mut out = f"get {r.status} n={r.body.len()}:";
            for b in r.body {
                out = f"{out} {b}";
            }
            println(out);
            for (k, v) in r.headers {
                if k.to_lowercase() == "x-kind" {
                    println(f"header {v}");
                }
            }
        }
        Err(e) => println(f"get: {e.message()}"),
    }
    match c.post("http://127.0.0.1:PORT/y", "q") {
        Ok(r) => println(f"post {r.status} n={r.body.len()} first={r.body[0]} last={r.body[23]}"),
        Err(e) => println(f"post: {e.message()}"),
    }
}
"#;

const CLIENT_WANT: &str = "get 200 n=24: 255 0 65 195 169 153 62 54 71 6 129 106 186 62 37 113 120 80 194 108 156 208 216 157\n\
                           header bin\n\
                           post 200 n=24 first=255 last=157\n";

fn client_fills_bytes(mode: &str) {
    static N: AtomicUsize = AtomicUsize::new(0);
    let port = byte_server();
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("karac-bytes-cli-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, CLIENT_SRC.replace("PORT", &port.to_string())).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(mode)
        .arg(&file)
        .current_dir(&dir)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{mode}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), CLIENT_WANT, "{mode}");
}

#[test]
fn interp_client_fills_a_vec_u8_body_with_the_bytes_sent() {
    client_fills_bytes("__mir-run");
}

#[cfg(feature = "llvm")]
#[test]
fn compiled_client_fills_a_vec_u8_body_with_the_bytes_sent() {
    client_fills_bytes("__mir-llvm-run");
}

/// Any other body type is refused at the handler, naming the two it may be.
#[test]
fn a_slice_u8_response_body_is_refused_naming_string_and_vec_u8() {
    let src = r#"
struct Response { status: i64, body: Slice[u8], headers: Vec[(String, String)] }

fn handle(req: Request) -> Response {
    Response { status: 200, body: req.bytes().as_slice(), headers: Vec.new() }
}

fn main() {
    let _ = Server.serve("127.0.0.1:0", handle);
}
"#;
    let dir = std::env::temp_dir().join(format!("karac-bytes-shape-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&file)
        .current_dir(&dir)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success(), "accepted:\n{text}");
    assert!(
        text.contains("declares `body: Slice[u8]`")
            && text.contains("`String` (text) or `Vec[u8]` (bytes sent as they are)"),
        "{text}"
    );
}
