//! The HTTP servers' answers to a client that misbehaves, on both MIR
//! backends (docs/library/io.md § servers): a client that drops its
//! connection mid-response ends only that connection (B-2026-11-07-25), an
//! absolute-form request target is read as its path (B-2026-11-07-26), and
//! a request that does not parse is answered 400 without the handler.
#![cfg(feature = "llvm")]

mod common;

use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::Duration;

/// A scratch directory, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "karac-mir-server-disconnect-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A certificate for `localhost` made for this run, written as `cert.pem`
/// and `key.pem` in `dir`, and the client config that trusts it.
fn certificate(dir: &Path) -> Arc<rustls::ClientConfig> {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    std::fs::write(dir.join("cert.pem"), cert.cert.pem()).unwrap();
    std::fs::write(dir.join("key.pem"), cert.signing_key.serialize_pem()).unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert.cert.der().clone()).unwrap();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    Arc::new(
        rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
}

/// The port the server announces on stderr (`KARAC_ANNOUNCE_PORT`).
fn port(lines: &Receiver<String>) -> u16 {
    let line = lines
        .recv_timeout(Duration::from_secs(60))
        .expect("a BOUND_PORT line within 60s");
    line.trim()
        .strip_prefix("BOUND_PORT=")
        .unwrap_or_else(|| panic!("the server printed {line:?}, not its port"))
        .parse()
        .unwrap()
}

type Client = fn(u16, &Arc<rustls::ClientConfig>) -> String;

/// Runs the server `cmd` with `client` driving it: the server's stdout, how
/// it ended, and the client's transcript.
fn serve(mut cmd: Command, client: Client, tls: &Arc<rustls::ClientConfig>) -> (String, String) {
    let mut child: Child = cmd
        .env("KARAC_ANNOUNCE_PORT", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let err = std::io::BufReader::new(child.stderr.take().unwrap());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in err.lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });
    let mut out = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = out.read_to_string(&mut s);
        s
    });
    let transcript = client(port(&rx), tls);
    let start = std::time::Instant::now();
    while child.try_wait().unwrap().is_none() {
        if start.elapsed() > Duration::from_secs(30) {
            let _ = child.kill();
            panic!("the server did not end: {}", reader.join().unwrap());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let status = child.wait().unwrap();
    let out = format!("{}{status}\n", reader.join().unwrap());
    (out, transcript)
}

/// The server's stdout and the client's transcript on the interpreter,
/// and on the built binary when the runtime archive links, which must
/// match them.
fn on_both(name: &str, src: &str, client: Client) -> (String, String) {
    let dir = Scratch::new(name);
    let tls = certificate(&dir.0);
    let src = src.replace("DIR", dir.0.to_str().unwrap());
    let file = dir.0.join("main.kara");
    std::fs::write(&file, &src).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_karac"));
    cmd.arg("__mir-run").arg(&file);
    let interp = serve(cmd, client, &tls);

    let exe = dir.0.join("main");
    let built = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["build", "--mir", file.to_str().unwrap(), "-o"])
        .arg(&exe)
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&built.stderr).to_string();
    assert!(
        built.status.success() || err.contains("Linker failed"),
        "{name} does not build: {err}"
    );
    let linked = common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(err)
    });
    if linked.is_some() {
        let compiled = serve(Command::new(&exe), client, &tls);
        assert_eq!(
            compiled, interp,
            "{name}: the build and the interpreter differ"
        );
    }
    interp
}

fn tcp(port: u16) -> std::net::TcpStream {
    let s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    s
}

/// The whole of what `s` sends back for `request`, up to the close: the
/// status line, then the body.
fn exchange<S: Read + Write>(mut s: S, request: &str) -> String {
    s.write_all(request.as_bytes()).unwrap();
    let mut got = Vec::new();
    let _ = s.read_to_end(&mut got);
    let got = String::from_utf8_lossy(&got).to_string();
    let status = got.lines().next().unwrap_or("no answer").to_string();
    let body = got.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
    format!("{status} [{body}]\n")
}

/// Each path the handler answers prints it, so a request the handler
/// never sees prints nothing. `/big` is a body many writes long; `/stop`
/// ends the program.
const ROUTE: &str = r#"
import std.process.{exit};

struct Response { status: i64, body: String }

fn route(req: Request) -> Response {
    let path = req.path();
    println(f"handler {path}");
    if path == "/stop" {
        exit(0);
    }
    if path == "/big" {
        let mut s = String.new();
        for _ in 0..200000 {
            s.push_str("0123456789abcdef0123456789abcdef");
        }
        return Response { status: 200, body: s };
    }
    let mut shown = String.new();
    for (k, v) in req.query() {
        shown.push_str(f"{k}={v};");
    }
    Response { status: 200, body: f"{path} {shown}" }
}
"#;

fn plain_src() -> String {
    format!("{ROUTE}\nfn main() {{\n    let _ = Server.serve(\"127.0.0.1:0\", route);\n}}\n")
}

fn tls_src() -> String {
    format!(
        "{ROUTE}\nfn main() {{\n    let cert = fs.read_to_string(\"DIR/cert.pem\").unwrap();\n    \
         let key = fs.read_to_string(\"DIR/key.pem\").unwrap();\n    \
         let _ = Server.serve_tls(\"127.0.0.1:0\".to_string(), cert, key, route);\n}}\n"
    )
}

/// Asks for `/big` and drops the connection without reading the answer
/// (or sending a TLS `close_notify`): the unread bytes make the close a
/// reset, so the server's next write goes to a peer that has gone.
fn drop_mid_response<S: Write>(mut s: S) {
    s.write_all(b"GET /big HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    s.flush().unwrap();
    std::thread::sleep(Duration::from_millis(100));
    drop(s);
    std::thread::sleep(Duration::from_millis(300));
}

fn plain_client(port: u16, _tls: &Arc<rustls::ClientConfig>) -> String {
    let mut t = String::new();
    t.push_str(&exchange(
        tcp(port),
        "GET http://example.com/z?k=v HTTP/1.1\r\nHost: example.com\r\nConnection: close\r\n\r\n",
    ));
    t.push_str(&exchange(
        tcp(port),
        "GET http://example.com HTTP/1.1\r\nHost: example.com\r\nConnection: close\r\n\r\n",
    ));
    t.push_str(&exchange(tcp(port), "NOT HTTP AT ALL\r\n\r\n"));
    t.push_str(&exchange(
        tcp(port),
        "GET /a HTTP/1.1\r\nHost: x\r\nno colon here\r\n\r\n",
    ));
    drop_mid_response(tcp(port));
    t.push_str(&exchange(
        tcp(port),
        "GET /after HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
    ));
    t.push_str(&exchange(
        tcp(port),
        "GET /stop HTTP/1.1\r\nHost: x\r\n\r\n",
    ));
    t
}

fn tls_stream(
    port: u16,
    config: &Arc<rustls::ClientConfig>,
) -> rustls::StreamOwned<rustls::ClientConnection, std::net::TcpStream> {
    let name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
    let conn = rustls::ClientConnection::new(config.clone(), name).unwrap();
    rustls::StreamOwned::new(conn, tcp(port))
}

fn tls_client(port: u16, tls: &Arc<rustls::ClientConfig>) -> String {
    let mut t = String::new();
    drop_mid_response(tls_stream(port, tls));
    drop_mid_response(tls_stream(port, tls));
    t.push_str(&exchange(
        tls_stream(port, tls),
        "GET /after?k=v HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    ));
    t.push_str(&exchange(
        tls_stream(port, tls),
        "GET /stop HTTP/1.1\r\nHost: localhost\r\n\r\n",
    ));
    t
}

/// An absolute-form target is its path and query (`/` with neither), a
/// request line or header that does not parse is answered 400 and never
/// reaches the handler, and a client gone mid-response leaves the server
/// answering the next one.
#[test]
fn mir_server_reads_absolute_targets_refuses_garbage_and_outlives_a_dropped_client() {
    let (out, transcript) = on_both("plain", &plain_src(), plain_client);
    assert_eq!(
        out,
        "handler /z\nhandler /\nhandler /big\nhandler /after\nhandler /stop\nexit status: 0\n"
    );
    assert_eq!(
        transcript,
        "HTTP/1.1 200 OK [/z k=v;]\n\
         HTTP/1.1 200 OK [/ ]\n\
         HTTP/1.1 400 Bad Request []\n\
         HTTP/1.1 400 Bad Request []\n\
         HTTP/1.1 200 OK [/after ]\n\
         no answer []\n"
    );
}

/// A TLS client that drops its connection mid-response without a
/// `close_notify` ends only that connection: the compiled server died of
/// `SIGPIPE` and refused the next client (B-2026-11-07-25).
#[test]
fn mir_tls_server_outlives_a_client_that_drops_mid_response() {
    let (out, transcript) = on_both("tls", &tls_src(), tls_client);
    assert_eq!(
        out,
        "handler /big\nhandler /big\nhandler /after\nhandler /stop\nexit status: 0\n"
    );
    assert_eq!(transcript, "HTTP/1.1 200 OK [/after k=v;]\nno answer []\n");
}
