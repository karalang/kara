//! A response body that is not UTF-8 (docs/library/io.md § HTTP client,
//! B-2026-11-15-8): `Response.body()` and `HttpError.body()` are the body
//! decoded with each invalid sequence replaced by U+FFFD, so the `String`
//! is valid UTF-8, and `Response.bytes()` is the body as it was sent. The
//! compiled program handed back a `String` holding the raw bytes, and
//! `body().chars()` aborted on Rust's `unreachable_unchecked` check.

use std::io::{BufRead, BufReader, Write};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Answers `/ok` with 200 and `/err` with 409, each with the body
/// `ff 41 c3`: a lone continuation byte, `A`, a truncated two-byte sequence.
fn server() -> u16 {
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
                let status = if line.contains(" /ok ") {
                    "200 OK"
                } else {
                    "409 Conflict"
                };
                let mut v = format!("HTTP/1.1 {status}\r\nContent-Length: 3\r\n\r\n").into_bytes();
                v.extend_from_slice(&[0xff, b'A', 0xc3]);
                let _ = out.write_all(&v);
            });
        }
    });
    port
}

const SRC: &str = r#"
fn points(s: String) -> String {
    let mut out = "";
    for ch in s.chars() {
        out = f"{out} {ch as u32}";
    }
    out
}

fn main() {
    let c = Client.new();
    match c.get("http://127.0.0.1:PORT/ok") {
        Ok(r) => {
            let b = r.body();
            let raw = r.bytes();
            println(f"ok: len={b.len()}{points(b)} bytes={raw.len()} {raw[0]},{raw[1]},{raw[2]}");
        }
        Err(e) => println(f"ok: {e.message()}"),
    }
    match c.get("http://127.0.0.1:PORT/err") {
        Ok(_) => println("err: answered"),
        Err(e) => match e.body() {
            Some(b) => println(f"err: {e.status()} len={b.len()}{points(b)}"),
            None => println("err: no body"),
        },
    }
}
"#;

const WANT: &str = "ok: len=7 65533 65 65533 bytes=3 255,65,195\n\
                    err: Some(409) len=7 65533 65 65533\n";

fn check(mode: &str) {
    static N: AtomicUsize = AtomicUsize::new(0);
    let port = server();
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("karac-non-utf8-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, SRC.replace("PORT", &port.to_string())).unwrap();
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
    assert_eq!(String::from_utf8_lossy(&out.stdout), WANT, "{mode}");
}

#[test]
fn interp_non_utf8_body_decodes_lossily_and_bytes_keeps_it() {
    check("__mir-run");
}

#[cfg(feature = "llvm")]
#[test]
fn compiled_non_utf8_body_is_a_valid_string_and_bytes_keeps_it() {
    check("__mir-llvm-run");
}

/// A server's `Request.body()` of a request body that is not UTF-8 is
/// decoded the same way: the compiled server's handler aborted in
/// `body().chars()`.
fn serve_and_post(mode: &str) -> String {
    serve_and_post_src(mode, SERVER_SRC)
}

fn serve_and_post_src(mode: &str, src: &str) -> String {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("karac-non-utf8-srv-{}-{n}", std::process::id()));
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
    for _ in 0..200 {
        let mut line = String::new();
        if err.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        if let Some(p) = line.trim().strip_prefix("BOUND_PORT=") {
            port = p.parse::<u16>().ok();
            break;
        }
    }
    let port = port.expect("the server announces its port");
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.write_all(
        b"POST /x HTTP/1.1\r\nHost: a\r\nContent-Length: 3\r\nConnection: close\r\n\r\n\xffA\xc3",
    )
    .unwrap();
    let mut got = Vec::new();
    let _ = std::io::Read::read_to_end(&mut s, &mut got);
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&dir);
    let text = String::from_utf8_lossy(&got).into_owned();
    text.split("\r\n\r\n")
        .nth(1)
        .unwrap_or_default()
        .to_string()
}

const SERVER_SRC: &str = r#"
struct Response { status: i64, body: String }

fn handle(req: Request) -> Response {
    let b = req.body();
    let mut pts = "";
    for ch in b.chars() {
        pts = f"{pts} {ch as u32}";
    }
    Response { status: 200, body: f"len={b.len()}{pts}" }
}

fn main() {
    let _ = Server.serve("127.0.0.1:0", handle);
}
"#;

#[test]
fn interp_server_request_body_decodes_lossily() {
    assert_eq!(serve_and_post("__mir-run"), "len=7 65533 65 65533");
}

#[cfg(feature = "llvm")]
#[test]
fn compiled_server_request_body_is_a_valid_string() {
    assert_eq!(serve_and_post("__mir-llvm-run"), "len=7 65533 65 65533");
}

/// `Request.bytes()` is the request body exactly as it was sent
/// (docs/library/io.md § HTTP server, B-2026-11-15-25): what a webhook's
/// signature covers, where `body()` has U+FFFD in place of each invalid
/// sequence. A request the program builds has its `with_body` text's bytes.
const BYTES_SRC: &str = r#"
struct Response { status: i64, body: String }

fn handle(req: Request) -> Response {
    let raw = req.bytes();
    let mut out = f"n={raw.len()}";
    for b in raw {
        out = f"{out} {b}";
    }
    let built = Request.new("POST", "/x").with_body("hé").bytes();
    Response { status: 200, body: f"{out} built={built.len()}" }
}

fn main() {
    let _ = Server.serve("127.0.0.1:0", handle);
}
"#;

#[test]
fn interp_server_request_bytes_are_the_body_as_sent() {
    assert_eq!(
        serve_and_post_src("__mir-run", BYTES_SRC),
        "n=3 255 65 195 built=3"
    );
}

#[cfg(feature = "llvm")]
#[test]
fn compiled_server_request_bytes_are_the_body_as_sent() {
    assert_eq!(
        serve_and_post_src("__mir-llvm-run", BYTES_SRC),
        "n=3 255 65 195 built=3"
    );
}
