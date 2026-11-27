//! docs/library/io.md § HTTP client, on both backends:
//!
//! - an obsolete line fold (RFC 9112 §5.2) in a response header is read as
//!   one SP, where the whole response was refused (B-2026-11-27-12);
//! - a response head may carry 100 header fields and be up to 408 KiB, and
//!   an `Err` over either limit names the limit it hit, where both were
//!   `request failed: message head is too large` (B-2026-11-27-13).

use std::io::{BufRead, BufReader, Write};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A loopback server answering one request per connection, by its path.
fn server() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("port").port();
    std::thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            std::thread::spawn(move || answer(conn));
        }
    });
    port
}

/// `n` header fields in all: `n - 2` of the server's own, then
/// `Content-Length` and `Connection`.
fn fields(n: usize) -> String {
    (0..n - 2).map(|i| format!("X-F{i}: v\r\n")).collect()
}

fn answer(conn: std::net::TcpStream) {
    let mut out = conn.try_clone().expect("clone");
    let mut r = BufReader::new(conn);
    let mut line = String::new();
    if r.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let target = line
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_string();
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
            break;
        }
    }
    let extra = match target.as_str() {
        "/fold" => "X-Fold: a\r\n  b\r\n\t c\r\nX-After: d\r\n".to_string(),
        "/h100" => fields(100),
        "/h101" => fields(101),
        "/h20000" => fields(20000),
        "/v300k" => format!("X-Big: {}\r\n", "v".repeat(300 * 1024)),
        "/v500k" => format!("X-Big: {}\r\n", "v".repeat(500 * 1024)),
        _ => String::new(),
    };
    let text =
        format!("HTTP/1.1 200 OK\r\n{extra}Content-Length: 2\r\nConnection: close\r\n\r\nok");
    // The client may close before reading the rest of a head it refused.
    let _ = out.write_all(text.as_bytes());
}

const SRC: &str = r#"
fn show(label: String, r: Result[Response, HttpError]) {
    match r {
        Ok(resp) => {
            let big = match resp.header("x-big") {
                Some(v) => v.len(),
                None => 0,
            };
            println(f"{label}: Ok {resp.status()} fields={resp.headers().len()} fold={resp.header("x-fold")} after={resp.header("x-after")} big={big} {resp.body()}")
        }
        Err(e) => println(f"{label}: Err {e.status()} msg={e.message()}"),
    }
}

fn main() {
    let base = "http://127.0.0.1:PORT";
    let c = Client.new();
    show("fold", c.get(f"{base}/fold"));
    show("h100", c.get(f"{base}/h100"));
    show("h101", c.get(f"{base}/h101"));
    show("h20000", c.get(f"{base}/h20000"));
    show("v300k", c.get(f"{base}/v300k"));
    show("v500k", c.get(f"{base}/v500k"));
}
"#;

fn want(port: u16) -> String {
    let base = format!("http://127.0.0.1:{port}");
    format!(
        "fold: Ok 200 fields=4 fold=Some(a b c) after=Some(d) big=0 ok\n\
         h100: Ok 200 fields=100 fold=None after=None big=0 ok\n\
         h101: Err None msg={base}/h101: response head has more than 100 header fields\n\
         h20000: Err None msg={base}/h20000: response head has more than 100 header fields\n\
         v300k: Ok 200 fields=3 fold=None after=None big={} ok\n\
         v500k: Err None msg={base}/v500k: response head is larger than 408 KiB\n",
        300 * 1024
    )
}

fn check(mode: &str) {
    static N: AtomicUsize = AtomicUsize::new(0);
    let port = server();
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("karac-head-limits-{}-{n}", std::process::id()));
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
    assert_eq!(String::from_utf8_lossy(&out.stdout), want(port), "{mode}");
}

#[test]
fn interp_response_obs_fold_and_head_limits() {
    check("__mir-run");
}

#[cfg(feature = "llvm")]
#[test]
fn compiled_response_obs_fold_and_head_limits() {
    check("__mir-llvm-run");
}
