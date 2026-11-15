//! The MIR interpreter sends a request through the client the compiled
//! program links (`src/http_client.rs`), so the two backends agree on what
//! a response is (docs/library/io.md § HTTP client):
//!
//! - a body cut short (fewer bytes than its `Content-Length`, a chunked
//!   body with no last chunk, a timeout mid-body) is an `Err`, where the
//!   interpreter returned the bytes it had as a 200 (B-2026-11-14-47);
//! - a chunked body's trailer is legal and read past;
//! - a `100 Continue` before the answer is skipped, where the interpreter
//!   returned the 100 with the real response as its body (B-2026-11-14-48);
//! - both send `User-Agent: kara/<version>` and `Accept: */*` unless the
//!   program sets its own, no `Accept-Encoding`, and decode no compression,
//!   where the interpreter sent `ureq`'s headers and gunzipped the body and
//!   the compiled client sent neither (B-2026-11-14-49).

use std::io::{BufRead, BufReader, Write};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// `zipped-body`, gzipped with no timestamp.
const GZIP: [u8; 31] = [
    31, 139, 8, 0, 0, 0, 0, 0, 2, 255, 171, 202, 44, 40, 72, 77, 209, 77, 202, 79, 169, 4, 0, 113,
    215, 93, 143, 11, 0, 0, 0,
];

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
    let mut headers: Vec<(String, String)> = Vec::new();
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = h.trim_end().split_once(':') {
            headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
    }
    let ok = |body: &[u8]| {
        let mut v =
            format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len()).into_bytes();
        v.extend_from_slice(body);
        v
    };
    let _ = match target.as_str() {
        // Every value the client sent for each header it sets by default.
        "/hdrs" => {
            let seen = |name: &str| {
                let vals: Vec<&str> = headers
                    .iter()
                    .filter(|(k, _)| k == name)
                    .map(|(_, v)| v.as_str())
                    .collect();
                if vals.is_empty() {
                    "-".to_string()
                } else {
                    vals.join("|")
                }
            };
            let body = format!(
                "ua={} accept={} ae={}",
                seen("user-agent"),
                seen("accept"),
                seen("accept-encoding")
            );
            out.write_all(&ok(body.as_bytes()))
        }
        "/cl-short" => out.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nhello"),
        "/chunk-trunc" => {
            out.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n")
        }
        "/slow" => {
            let _ = out.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nxx");
            let _ = out.flush();
            std::thread::sleep(Duration::from_millis(2000));
            out.write_all(b"yy")
        }
        "/trailer" => out.write_all(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\nX-Sum: 5\r\n\r\n",
        ),
        "/continue" => out.write_all(
            b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\nafter-100",
        ),
        "/gzip" => {
            let mut v = format!(
                "HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\n\r\n",
                GZIP.len()
            )
            .into_bytes();
            v.extend_from_slice(&GZIP);
            out.write_all(&v)
        }
        _ => out.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n"),
    };
}

const SRC: &str = r#"
fn show(label: String, r: Result[Response, HttpError]) {
    match r {
        Ok(resp) => println(f"{label}: {resp.status()} {resp.body()}"),
        Err(e) => {
            let m = e.message();
            let cut = m.contains("reading response body failed");
            let late = m.contains("request timed out after 500 ms");
            println(f"{label}: Err status={e.status()} cut={cut} late={late}");
        }
    }
}

fn main() {
    let base = "http://127.0.0.1:PORT";
    let c = Client.new();
    show("defaults", c.get(f"{base}/hdrs"));
    show("own", c.request("GET", f"{base}/hdrs").header("User-Agent", "probe/1").header("accept", "application/json").send());
    show("cl-short", c.get(f"{base}/cl-short"));
    show("chunk-trunc", c.get(f"{base}/chunk-trunc"));
    show("slow", c.request("GET", f"{base}/slow").timeout(500).send());
    show("trailer", c.get(f"{base}/trailer"));
    show("continue", c.get(f"{base}/continue"));
    match c.get(f"{base}/gzip") {
        Ok(r) => {
            let b = r.bytes();
            println(f"gzip: {r.status()} {r.header("Content-Encoding")} len={b.len()} head={b[0]},{b[1]}");
        }
        Err(e) => println(f"gzip: {e.message()}"),
    }
}
"#;

fn want() -> String {
    let v = env!("CARGO_PKG_VERSION");
    format!(
        "defaults: 200 ua=kara/{v} accept=*/* ae=-\n\
         own: 200 ua=probe/1 accept=application/json ae=-\n\
         cl-short: Err status=None cut=true late=false\n\
         chunk-trunc: Err status=None cut=true late=false\n\
         slow: Err status=None cut=false late=true\n\
         trailer: 200 hello\n\
         continue: 200 after-100\n\
         gzip: 200 Some(gzip) len=31 head=31,139\n"
    )
}

fn check(mode: &str) {
    static N: AtomicUsize = AtomicUsize::new(0);
    let port = server();
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("karac-one-client-{}-{n}", std::process::id()));
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
    assert_eq!(String::from_utf8_lossy(&out.stdout), want(), "{mode}");
}

#[test]
fn interp_http_client_is_the_compiled_client() {
    check("__mir-run");
}

#[cfg(feature = "llvm")]
#[test]
fn compiled_http_client_sends_the_documented_headers() {
    check("__mir-llvm-run");
}
