//! What the HTTP client puts on the wire, on both MIR backends
//! (docs/library/io.md § HTTP client): only http:// and https:// URLs are
//! spoken to (B-2026-11-07-28), and the request inside a proxy's CONNECT
//! tunnel is in origin form, as sent to the origin directly
//! (B-2026-11-07-29).
#![cfg(feature = "llvm")]

mod common;

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// A scratch directory removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir =
            std::env::temp_dir().join(format!("karac-http-wire-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Every proxy variable the client reads, so a run sees only its own.
const PROXY_VARS: &[&str] = &[
    "http_proxy",
    "HTTP_PROXY",
    "https_proxy",
    "HTTPS_PROXY",
    "all_proxy",
    "ALL_PROXY",
    "no_proxy",
    "NO_PROXY",
];

/// A program fetching each of `urls`, printing the body or the error.
fn fetch_program(urls: &[String]) -> String {
    let calls: String = urls
        .iter()
        .map(|u| format!("    fetch(\"{u}\");\n"))
        .collect();
    format!(
        r#"
fn fetch(url: own String) {{
    match Client.new().get(url) {{
        Ok(r) => println(f"{{r.status()}} {{r.body()}}"),
        Err(e) => println(f"error: {{e.message()}}"),
    }}
}}

fn main() {{
{calls}}}
"#
    )
}

/// The program on the interpreter, and built, each run with `vars`.
fn outputs(dir: &Scratch, name: &str, urls: &[String], vars: &[(&str, String)]) -> Vec<String> {
    let file = dir.0.join(format!("{name}.kara"));
    std::fs::write(&file, fetch_program(urls)).unwrap();
    let karac = env!("CARGO_BIN_EXE_karac");
    let mut runs = vec![{
        let mut c = Command::new(karac);
        c.args(["__mir-run", file.to_str().unwrap()]);
        c
    }];
    let exe = dir.0.join(name);
    let built = Command::new(karac)
        .args([
            "build",
            "--mir",
            file.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    if common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&built.stderr).to_string())
    })
    .is_some()
    {
        runs.push(Command::new(&exe));
    }
    runs.into_iter()
        .map(|mut cmd| {
            for v in PROXY_VARS {
                cmd.env_remove(v);
            }
            let out = cmd.envs(vars.iter().cloned()).output().unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8(out.stdout).unwrap()
        })
        .collect()
}

/// A plain HTTP server on loopback answering 200 to anything, counting the
/// connections it took.
fn plain_origin() -> (u16, Arc<AtomicUsize>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let taken = Arc::new(AtomicUsize::new(0));
    let count = taken.clone();
    std::thread::spawn(move || {
        for tcp in listener.incoming() {
            let Ok(mut tcp) = tcp else { return };
            count.fetch_add(1, Ordering::SeqCst);
            let (mut head, mut byte) = (Vec::new(), [0u8; 1]);
            while !head.ends_with(b"\r\n\r\n") {
                match tcp.read(&mut byte) {
                    Ok(1) => head.push(byte[0]),
                    _ => break,
                }
            }
            let _ = tcp
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok");
        }
    });
    (port, taken)
}

/// A URL naming another protocol is refused before any connection, with
/// the same error on both backends; the compiled client used to speak HTTP
/// to it and print the server's 200.
#[test]
fn http_client_refuses_a_scheme_other_than_http_or_https() {
    let dir = Scratch::new("scheme");
    let (port, taken) = plain_origin();
    let urls = [
        format!("ftp://127.0.0.1:{port}/ftpprobe"),
        format!("gopher://127.0.0.1:{port}/gprobe"),
        format!("http://127.0.0.1:{port}/plain"),
    ];
    let want = format!(
        "error: ftp://127.0.0.1:{port}/ftpprobe: Unknown Scheme: unknown scheme 'ftp'\n\
         error: gopher://127.0.0.1:{port}/gprobe: Unknown Scheme: unknown scheme 'gopher'\n\
         200 ok\n"
    );
    let runs = outputs(&dir, "scheme", &urls, &[]);
    for out in &runs {
        assert_eq!(out, &want);
    }
    // Only the http URL reached the server, once per run.
    assert_eq!(taken.load(Ordering::SeqCst), runs.len());
}

/// An https origin for `name` that answers with the request line it read.
fn echoing_tls_origin(name: &str) -> (u16, String) {
    let cert = rcgen::generate_simple_self_signed(vec![name.to_string()]).unwrap();
    let pem = cert.cert.pem();
    let der = cert.cert.der().clone();
    let key = rustls::pki_types::PrivateKeyDer::try_from(cert.signing_key.serialize_der()).unwrap();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = Arc::new(
        rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![der], key)
            .unwrap(),
    );
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for tcp in listener.incoming() {
            let Ok(tcp) = tcp else { return };
            let conn = rustls::ServerConnection::new(config.clone()).unwrap();
            let mut tls = rustls::StreamOwned::new(conn, tcp);
            let (mut got, mut chunk) = (Vec::new(), [0u8; 1024]);
            while !got.windows(4).any(|w| w == b"\r\n\r\n") {
                match tls.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => got.extend_from_slice(&chunk[..n]),
                }
            }
            let text = String::from_utf8_lossy(&got);
            let line = text.lines().next().unwrap_or_default();
            let body = line.rsplit_once(' ').map_or(line, |(l, _)| l).to_string();
            let reply = format!(
                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = tls.write_all(reply.as_bytes());
            tls.conn.send_close_notify();
            let _ = tls.flush();
        }
    });
    (port, pem)
}

/// A forward proxy on loopback that tunnels a CONNECT to `name:443` to the
/// loopback `origin`.
fn tunnelling_proxy(name: &'static str, origin: u16) -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for tcp in listener.incoming() {
            let Ok(mut tcp) = tcp else { return };
            std::thread::spawn(move || {
                let (mut head, mut byte) = (Vec::new(), [0u8; 1]);
                while !head.ends_with(b"\r\n\r\n") {
                    match tcp.read(&mut byte) {
                        Ok(1) => head.push(byte[0]),
                        _ => return,
                    }
                }
                let head = String::from_utf8_lossy(&head).into_owned();
                if !head.starts_with(&format!("CONNECT {name}:443 ")) {
                    let _ = tcp.write_all(b"HTTP/1.1 403 Forbidden\r\ncontent-length: 0\r\n\r\n");
                    return;
                }
                let _ = tcp.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n");
                let mut up = std::net::TcpStream::connect(("127.0.0.1", origin)).unwrap();
                let (mut down, mut up_w) = (tcp.try_clone().unwrap(), up.try_clone().unwrap());
                std::thread::spawn(move || {
                    let _ = std::io::copy(&mut down, &mut up_w);
                });
                let _ = std::io::copy(&mut up, &mut tcp);
                let _ = tcp.shutdown(std::net::Shutdown::Both);
            });
        }
    });
    port
}

/// Through a CONNECT tunnel the request is the one sent to the origin
/// directly, in origin form (RFC 9112 §3.2.1); the interpreter used to send
/// `GET https://example.invalid/tunnel?q=1`.
#[test]
fn http_client_sends_origin_form_inside_a_proxy_tunnel() {
    let dir = Scratch::new("tunnel");
    let (origin, pem) = echoing_tls_origin("example.invalid");
    let ca = dir.0.join("ca.pem");
    std::fs::write(&ca, pem).unwrap();
    let proxy = tunnelling_proxy("example.invalid", origin);
    let vars = [
        ("https_proxy", format!("http://127.0.0.1:{proxy}")),
        ("KARAC_HTTP_EXTRA_CA", ca.to_str().unwrap().to_string()),
    ];
    let urls = [
        "https://example.invalid/tunnel?q=1".to_string(),
        "https://other.invalid/z".to_string(),
    ];
    let want = "200 GET /tunnel?q=1\n\
                error: https://other.invalid/z: the proxy refused the tunnel: 403 Forbidden\n";
    for out in outputs(&dir, "tunnel", &urls, &vars) {
        assert_eq!(out, want);
    }
}
