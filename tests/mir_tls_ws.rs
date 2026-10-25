//! The TLS server (`TlsListener` / `TlsStream`), the WebSocket server
//! (`WebSocket`, `WsMessage`) and the HTTP servers over them
//! (`Server.serve_tls`, `serve_ws`, `serve_ws_tls`), M4 item 8, on both
//! MIR backends: each
//! program serves a client this file drives (rustls for TLS, the frames of
//! `karac::ws_frame` for WebSocket), and the interpreter and the built
//! binary must print the same and send the client the same.
#![cfg(feature = "llvm")]

mod common;

use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::Duration;

use karac::ws_frame::{encode, Decoder, Opcode};

/// A scratch directory, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir =
            std::env::temp_dir().join(format!("karac-mir-tls-ws-{name}-{}", std::process::id()));
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

/// The ports a server announces on stderr (`KARAC_ANNOUNCE_PORT`), one
/// per listener bound to port 0, in order.
struct Ports(Receiver<String>);

impl Ports {
    fn next(&self) -> u16 {
        let line = self
            .0
            .recv_timeout(Duration::from_secs(60))
            .expect("a BOUND_PORT line within 60s");
        line.trim()
            .strip_prefix("BOUND_PORT=")
            .unwrap_or_else(|| panic!("the server printed {line:?}, not its port"))
            .parse()
            .unwrap()
    }
}

/// Runs the server `cmd` with `client` driving it: the server's stdout and
/// the client's transcript.
fn serve(
    mut cmd: Command,
    client: fn(&Ports, &Arc<rustls::ClientConfig>) -> String,
    tls: &Arc<rustls::ClientConfig>,
) -> (String, String) {
    let mut child: Child = cmd
        .env("KARAC_ANNOUNCE_PORT", "1")
        .env("KARAC_MIR_COROUTINES", "1")
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
    let transcript = client(&Ports(rx), tls);
    // The server ends on its own once the client is done.
    let start = std::time::Instant::now();
    while child.try_wait().unwrap().is_none() {
        if start.elapsed() > Duration::from_secs(30) {
            let _ = child.kill();
            panic!("the server did not end: {}", reader.join().unwrap());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(child.wait().unwrap().success(), "the server failed");
    (reader.join().unwrap(), transcript)
}

/// The program's stdout and the client's transcript on the interpreter
/// and, when the runtime archive links, on the built binary, which must
/// match them.
fn on_both(
    name: &str,
    src: &str,
    client: fn(&Ports, &Arc<rustls::ClientConfig>) -> String,
) -> (String, String) {
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
        .env("KARAC_MIR_COROUTINES", "1")
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

fn tls_stream(
    port: u16,
    config: &Arc<rustls::ClientConfig>,
) -> rustls::StreamOwned<rustls::ClientConnection, std::net::TcpStream> {
    let name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
    let conn = rustls::ClientConnection::new(config.clone(), name).unwrap();
    rustls::StreamOwned::new(conn, tcp(port))
}

const TLS_SRC: &str = r#"
fn show(e: NetError) -> String {
    match e {
        NetError.Tls(TlsFailure.Handshake) => "tls handshake".to_string(),
        NetError.Tls(TlsFailure.BadCertificate) => "tls bad certificate".to_string(),
        NetError.Tls(TlsFailure.Protocol) => "tls protocol".to_string(),
        NetError.Tls(TlsFailure.UnexpectedClose) => "tls unexpected close".to_string(),
        NetError.TimedOut => "timed out".to_string(),
        NetError.InvalidInput => "invalid input".to_string(),
        _ => "other".to_string(),
    }
}

fn main() {
    let cert = fs.read_to_string("DIR/cert.pem").unwrap();
    let key = fs.read_to_string("DIR/key.pem").unwrap();
    match TlsListener.bind_tls("127.0.0.1:0", "not a certificate", key) {
        Ok(_) => println("bad cert accepted"),
        Err(e) => println(f"bad cert: {show(e)}"),
    }
    let l = TlsListener.bind_tls("127.0.0.1:0", cert, key).unwrap();
    println(f"local addr {l.local_addr().is_ok()}");
    let mut buf = [0u8; 64];
    // A line each way, then the client's close_notify.
    let s = l.accept().unwrap();
    let n = s.read(mut buf).unwrap();
    println(f"read {String.from_utf8(buf[0..n]).unwrap()}");
    println(f"wrote {s.write_all("pong\n".bytes()).unwrap()}");
    match s.read(mut buf) {
        Ok(n) => println(f"then {n}"),
        Err(e) => println(f"then {show(e)}"),
    }
    let _ = s.shutdown_write();
    // A client that speaks plain text.
    let p = l.accept().unwrap();
    match p.read(mut buf) {
        Ok(n) => println(f"plain read {n}"),
        Err(e) => println(f"plain: {show(e)}"),
    }
    // A read timeout, then a client that goes without close_notify.
    let t = l.accept().unwrap();
    let _ = t.set_read_timeout(Some(100));
    match t.read(mut buf) {
        Ok(n) => println(f"quiet read {n}"),
        Err(e) => println(f"quiet: {show(e)}"),
    }
    let _ = t.set_read_timeout(None);
    let _ = t.write_all("late\n".bytes());
    match t.read(mut buf) {
        Ok(n) => println(f"dropped read {n}"),
        Err(e) => println(f"dropped: {show(e)}"),
    }
}
"#;

fn tls_client(ports: &Ports, config: &Arc<rustls::ClientConfig>) -> String {
    let port = ports.next();
    let mut t = String::new();
    let mut s = tls_stream(port, config);
    s.write_all(b"ping\n").unwrap();
    let mut line = String::new();
    std::io::BufReader::new(&mut s)
        .read_line(&mut line)
        .unwrap();
    t.push_str(&format!("got {line}"));
    s.conn.send_close_notify();
    s.flush().unwrap();
    let mut rest = Vec::new();
    let _ = s.read_to_end(&mut rest);
    t.push_str(&format!("then {} more\n", rest.len()));

    let mut p = tcp(port);
    p.write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
    // The server keeps the socket until it ends, so read the alert's
    // first byte only.
    let mut first = [0u8; 1];
    let _ = p.read_exact(&mut first);
    t.push_str(&format!("plain answer is an alert {}\n", first[0] == 21));

    let mut q = tls_stream(port, config);
    while q.conn.is_handshaking() {
        q.conn.complete_io(&mut q.sock).unwrap();
    }
    let mut line = String::new();
    std::io::BufReader::new(&mut q)
        .read_line(&mut line)
        .unwrap();
    t.push_str(&format!("got {line}"));
    // Gone without close_notify.
    q.sock.shutdown(std::net::Shutdown::Both).unwrap();
    t
}

#[test]
fn mir_tls_server_serves_a_rustls_client() {
    let (out, transcript) = on_both("tls", TLS_SRC, tls_client);
    assert_eq!(
        out,
        "bad cert: invalid input\nlocal addr true\nread ping\n\nwrote 5\nthen 0\n\
         plain: tls handshake\nquiet: timed out\ndropped: tls unexpected close\n"
    );
    assert_eq!(
        transcript,
        "got pong\nthen 0 more\nplain answer is an alert true\ngot late\n"
    );
}

const WS_SRC: &str = r#"
fn code(c: Option[u16]) -> String {
    match c {
        Some(n) => f"{n}",
        None => "none".to_string(),
    }
}

fn session(ws: mut ref WebSocket) {
    loop {
        match ws.recv() {
            Ok(WsMessage.Text(t)) => {
                println(f"text {t}");
                if t == "bye" {
                    println(f"close {ws.close(1001u16).is_ok()}");
                } else {
                    let _ = ws.send_text(f"echo {t}");
                }
            }
            Ok(WsMessage.Binary(b)) => {
                println(f"binary {b.len()}");
                let _ = ws.send_binary(b);
            }
            Ok(WsMessage.Ping(p)) => println(f"ping {p.len()}"),
            Ok(WsMessage.Pong(p)) => println(f"pong {p.len()}"),
            Ok(WsMessage.Close(c)) => {
                println(f"closed {code(c)}");
                break;
            }
            Err(NetError.Protocol) => println("protocol"),
            Err(_) => {
                println("error");
                break;
            }
        }
    }
    match ws.recv() {
        Ok(WsMessage.Close(c)) => println(f"again {code(c)}"),
        _ => println("again not closed"),
    }
    match ws.send_text("late") {
        Ok(()) => println("late sent"),
        Err(NetError.Protocol) => println("late refused"),
        Err(_) => println("late error"),
    }
}

fn tls_session(l: own TlsListener) {
    let mut ws = WebSocket.accept_tls(l).unwrap();
    session(mut ws);
}

fn main() {
    let l = TcpListener.bind("127.0.0.1:0").unwrap();
    let mut i = 0;
    while i < 3 {
        let mut ws = WebSocket.accept(l).unwrap();
        session(mut ws);
        i += 1;
    }
    match WebSocket.accept(l) {
        Ok(_) => println("plain request upgraded"),
        Err(NetError.Protocol) => println("plain request refused"),
        Err(_) => println("plain request error"),
    }
    let cert = fs.read_to_string("DIR/cert.pem").unwrap();
    let key = fs.read_to_string("DIR/key.pem").unwrap();
    let tl = TlsListener.bind_tls("127.0.0.1:0", cert, key).unwrap();
    let group = TaskGroup.new();
    let h = group.spawn(|| tls_session(tl));
    h.join();
    println("done");
}
"#;

/// A WebSocket client over `S`: the handshake, then frames.
struct Ws<S: Read + Write> {
    s: S,
    frames: Decoder,
}

impl<S: Read + Write> Ws<S> {
    fn open(mut s: S, t: &mut String) -> Ws<S> {
        let key = "dGhlIHNhbXBsZSBub25jZQ==";
        write!(
            s,
            "GET /chat HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\n\
             Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
        )
        .unwrap();
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            s.read_exact(&mut byte).unwrap();
            head.push(byte[0]);
        }
        let head = String::from_utf8(head).unwrap();
        let ok = head.starts_with("HTTP/1.1 101 ")
            && head.contains(&format!(
                "Sec-WebSocket-Accept: {}\r\n",
                karac::ws_frame::accept_key(key)
            ));
        t.push_str(&format!("accept {ok}\n"));
        Ws {
            s,
            frames: Decoder::new(false, 1 << 24),
        }
    }

    fn send(&mut self, fin: bool, op: Opcode, payload: &[u8]) {
        self.s
            .write_all(&encode(fin, op, payload, Some([1, 2, 3, 4])))
            .unwrap();
    }

    /// The next frame, written as the transcript has it.
    fn recv(&mut self, t: &mut String) {
        let mut chunk = [0u8; 65536];
        loop {
            if let Some(f) = self.frames.next().unwrap() {
                let line = match f.opcode {
                    Opcode::Text => format!("text {}", String::from_utf8(f.payload).unwrap()),
                    Opcode::Binary => format!("binary {}", f.payload.len()),
                    Opcode::Pong => format!("pong {}", String::from_utf8(f.payload).unwrap()),
                    Opcode::Close => {
                        format!("close {}", u16::from_be_bytes([f.payload[0], f.payload[1]]))
                    }
                    op => format!("{op:?}"),
                };
                t.push_str(&line);
                t.push('\n');
                return;
            }
            let n = self.s.read(&mut chunk).unwrap();
            assert!(n > 0, "the server closed early");
            self.frames.push(&chunk[..n]);
        }
    }
}

fn ws_client(ports: &Ports, config: &Arc<rustls::ClientConfig>) -> String {
    let port = ports.next();
    let mut t = String::new();
    // Messages, a fragmented one with a ping inside, then the server's
    // close.
    let mut ws = Ws::open(tcp(port), &mut t);
    ws.send(true, Opcode::Text, b"hello");
    ws.recv(&mut t);
    ws.send(false, Opcode::Text, b"fra");
    ws.send(true, Opcode::Ping, b"pp");
    ws.send(true, Opcode::Continuation, b"gment");
    ws.recv(&mut t);
    ws.recv(&mut t);
    let big: Vec<u8> = (0..70_000u32).map(|i| (i % 251) as u8).collect();
    ws.send(true, Opcode::Binary, &big);
    let mut chunk = [0u8; 65536];
    loop {
        if let Some(f) = ws.frames.next().unwrap() {
            t.push_str(&format!("binary echo same {}\n", f.payload == big));
            break;
        }
        let n = ws.s.read(&mut chunk).unwrap();
        ws.frames.push(&chunk[..n]);
    }
    ws.send(true, Opcode::Text, b"bye");
    ws.recv(&mut t);
    ws.send(true, Opcode::Close, &1001u16.to_be_bytes());
    drop(ws);

    // An unmasked frame breaks the protocol.
    let mut ws = Ws::open(tcp(port), &mut t);
    ws.s.write_all(&encode(true, Opcode::Text, b"bare", None))
        .unwrap();
    ws.recv(&mut t);
    drop(ws);

    // Gone without a close frame.
    let ws = Ws::open(tcp(port), &mut t);
    ws.s.shutdown(std::net::Shutdown::Both).unwrap();
    drop(ws);

    // No upgrade at all.
    let mut p = tcp(port);
    p.write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
    let mut answer = String::new();
    let _ = p.read_to_string(&mut answer);
    t.push_str(answer.lines().next().unwrap_or("nothing"));
    t.push('\n');

    // Over TLS, served by a task.
    let port = ports.next();
    let mut ws = Ws::open(tls_stream(port, config), &mut t);
    ws.send(true, Opcode::Text, b"secure");
    ws.recv(&mut t);
    ws.send(true, Opcode::Close, &1000u16.to_be_bytes());
    ws.recv(&mut t);
    t
}

#[test]
fn mir_websocket_server_over_tcp_and_tls() {
    let (out, transcript) = on_both("ws", WS_SRC, ws_client);
    assert_eq!(
        out,
        "text hello\nping 2\ntext fragment\nbinary 70000\ntext bye\nclose true\n\
         closed 1001\nagain 1001\nlate refused\n\
         protocol\nclosed 1002\nagain 1002\nlate refused\n\
         closed 1006\nagain 1006\nlate refused\n\
         plain request refused\n\
         text secure\nclosed 1000\nagain 1000\nlate refused\ndone\n"
    );
    assert_eq!(
        transcript,
        "accept true\ntext echo hello\npong pp\ntext echo fragment\nbinary echo same true\n\
         close 1001\n\
         accept true\nclose 1002\n\
         accept true\n\
         HTTP/1.1 400 Bad Request\n\
         accept true\ntext echo secure\nclose 1000\n"
    );
}

/// The HTTP servers over TLS and WebSocket (`Server.serve_tls`,
/// `serve_ws`, `serve_ws_tls`, runtime/stdlib/source/http_server.kara). A
/// server never returns, so each program ends itself with `exit`, which
/// flushes what it printed.
const SERVE_TLS_SRC: &str = r#"
import std.process.{exit};

struct Response { status: i64, body: String, headers: Vec[(String, String)] }

fn route(req: Request) -> Response {
    let path = req.path();
    if path == "/stop" {
        println("stopping");
        exit(0);
    }
    println(f"{req.method()} {path} {req.body()}");
    let q = req.query();
    let mut shown = String.new();
    for (k, v) in q {
        shown.push_str(f"{k}={v};");
    }
    Response {
        status: if path == "/missing" { 404 } else { 200 },
        body: f"you asked {path} {shown}",
        headers: vec![("content-type".to_string(), "text/plain".to_string()), ("x-served".to_string(), "tls".to_string())],
    }
}

fn main() {
    let cert = fs.read_to_string("DIR/cert.pem").unwrap();
    let key = fs.read_to_string("DIR/key.pem").unwrap();
    match Server.serve_tls("127.0.0.1:0".to_string(), "not a certificate".to_string(), key.clone(), route) {
        Ok(()) => println("bad cert served"),
        Err(e) => println(f"bad cert: {e.message}"),
    }
    match Server.serve_tls("127.0.0.1:0".to_string(), cert, key, route) {
        Ok(()) => println("served"),
        Err(e) => println(f"failed: {e.message}"),
    }
}
"#;

/// One response off `s`: its head and its body, read to its
/// `content-length`, since the server keeps the connection; the error's
/// kind when `s` ends or fails first.
fn response<S: Read>(s: &mut S) -> Result<(String, String), std::io::ErrorKind> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        match s.read(&mut byte) {
            Ok(0) => return Err(std::io::ErrorKind::UnexpectedEof),
            Ok(_) => head.push(byte[0]),
            Err(e) => return Err(e.kind()),
        }
    }
    let head = String::from_utf8(head).unwrap();
    let len = head
        .lines()
        .find_map(|l| {
            l.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(|n| n.trim().parse::<usize>().unwrap())
        })
        .unwrap_or(0);
    let mut body = vec![0u8; len];
    s.read_exact(&mut body).map_err(|e| e.kind())?;
    Ok((
        head.trim_end().to_string(),
        String::from_utf8(body).unwrap(),
    ))
}

/// The response to `request` over a fresh TLS connection: its head's
/// status line and the headers named, and its body.
fn https(port: u16, config: &Arc<rustls::ClientConfig>, request: &str) -> String {
    let mut s = tls_stream(port, config);
    s.write_all(request.as_bytes()).unwrap();
    let (head, body) = match response(&mut s) {
        Ok(r) => r,
        Err(kind) => return format!("no answer ({kind:?})\n"),
    };
    let mut lines = head.lines();
    let mut t = lines.next().unwrap().to_string();
    for l in lines {
        let lower = l.to_ascii_lowercase();
        if ["content-type", "x-served", "connection", "content-length"]
            .iter()
            .any(|h| lower.starts_with(&format!("{h}:")))
        {
            t.push_str(&format!(" | {lower}"));
        }
    }
    format!("{t}\n{body}\n")
}

fn serve_tls_client(ports: &Ports, config: &Arc<rustls::ClientConfig>) -> String {
    // The bad certificate's bind announces nothing.
    let port = ports.next();
    let mut t = https(
        port,
        config,
        "GET /a/b?x=1&y=two HTTP/1.1\r\nHost: localhost\r\n\r\n",
    );
    // A client that speaks plain text is dropped, and the server goes on.
    let mut p = tcp(port);
    p.write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
    let mut first = [0u8; 1];
    let _ = p.read_exact(&mut first);
    t.push_str(&format!("plain answer is an alert {}\n", first[0] == 21));
    drop(p);
    t.push_str(&https(
        port,
        config,
        "POST /missing HTTP/1.1\r\nHost: localhost\r\nContent-Length: 4\r\n\r\ndata",
    ));
    t.push_str(&https(
        port,
        config,
        "GET /stop HTTP/1.1\r\nHost: localhost\r\n\r\n",
    ));
    t
}

#[test]
fn mir_serve_tls_answers_https_requests() {
    let (out, transcript) = on_both("serve-tls", SERVE_TLS_SRC, serve_tls_client);
    assert_eq!(
        out,
        "bad cert: 127.0.0.1:0: bind failed: invalid input\n\
         GET /a/b \nPOST /missing data\nstopping\n"
    );
    assert_eq!(
        transcript,
        "HTTP/1.1 200 OK | content-type: text/plain | x-served: tls | content-length: 25\n\
         you asked /a/b x=1;y=two;\n\
         plain answer is an alert true\n\
         HTTP/1.1 404 Not Found | content-type: text/plain | x-served: tls \
         | content-length: 19\nyou asked /missing \n\
         no answer (UnexpectedEof)\n"
    );
}

const SERVE_WS_SRC: &str = r#"
import std.process.{exit};

fn route(req: Request) -> Response {
    let path = req.path();
    println(f"request {path}");
    match path.as_str() {
        "/chat" => Response { status: 101, body: "unused".to_string() },
        "/health" => Response { status: 200, body: "ok".to_string() },
        _ => Response { status: 404, body: "nope".to_string() },
    }
}

fn chat(ws: own WebSocket) {
    let mut ws = ws;
    loop {
        match ws.recv() {
            Ok(WsMessage.Text(t)) => {
                println(f"text {t}");
                if t == "bye" {
                    println(f"close {ws.close(1000u16).is_ok()}");
                    exit(0);
                }
                let _ = ws.send_text(f"echo {t}");
            }
            Ok(WsMessage.Close(c)) => {
                match c {
                    Some(n) => println(f"closed {n}"),
                    None => println("closed"),
                }
                break;
            }
            Ok(_) => {}
            Err(_) => {
                println("error");
                break;
            }
        }
    }
}

fn main() {
    let cert = fs.read_to_string("DIR/cert.pem").unwrap();
    let key = fs.read_to_string("DIR/key.pem").unwrap();
    let r = if "MODE" == "tls" {
        Server.serve_ws_tls("127.0.0.1:0".to_string(), cert, key, route, chat)
    } else {
        Server.serve_ws("127.0.0.1:0".to_string(), route, chat)
    };
    match r {
        Ok(()) => println("served"),
        Err(e) => println(f"failed: {e.message}"),
    }
}
"#;

/// The status line, and body, of `request` sent over `s`.
fn http_over<S: Read + Write>(mut s: S, request: &str) -> String {
    s.write_all(request.as_bytes()).unwrap();
    match response(&mut s) {
        Ok((head, body)) => format!("{} [{body}]\n", head.lines().next().unwrap()),
        Err(kind) => format!("no answer ({kind:?})\n"),
    }
}

/// Two sessions at once, with plain requests served while both are open,
/// over `connect`'s streams.
fn upgrades<S: Read + Write>(connect: &dyn Fn() -> S) -> String {
    let mut t = String::new();
    let mut one = Ws::open(connect(), &mut t);
    let mut two = Ws::open(connect(), &mut t);
    t.push_str(&http_over(
        connect(),
        "GET /health HTTP/1.1\r\nHost: x\r\n\r\n",
    ));
    two.send(true, Opcode::Text, b"two");
    two.recv(&mut t);
    one.send(true, Opcode::Text, b"one");
    one.recv(&mut t);
    two.send(true, Opcode::Close, &1001u16.to_be_bytes());
    two.recv(&mut t);
    drop(two);
    // A 101 to a request that is no handshake is sent as it is, and a
    // handshake the handler refuses gets the handler's answer.
    t.push_str(&http_over(
        connect(),
        "GET /chat HTTP/1.1\r\nHost: x\r\n\r\n",
    ));
    t.push_str(&http_over(
        connect(),
        "GET /elsewhere HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n",
    ));
    one.send(true, Opcode::Text, b"bye");
    one.recv(&mut t);
    one.send(true, Opcode::Close, &1000u16.to_be_bytes());
    t
}

fn serve_ws_client(ports: &Ports, _config: &Arc<rustls::ClientConfig>) -> String {
    let port = ports.next();
    upgrades(&|| tcp(port))
}

fn serve_ws_tls_client(ports: &Ports, config: &Arc<rustls::ClientConfig>) -> String {
    let port = ports.next();
    upgrades(&|| tls_stream(port, config))
}

const SERVE_WS_OUT: &str = "request /chat\nrequest /chat\nrequest /health\ntext two\ntext one\n\
     closed 1001\nrequest /chat\nrequest /elsewhere\ntext bye\nclose true\n";

const SERVE_WS_TRANSCRIPT: &str = "accept true\naccept true\nHTTP/1.1 200 OK [ok]\n\
     text echo two\ntext echo one\nclose 1001\n\
     HTTP/1.1 101 Switching Protocols [unused]\nHTTP/1.1 404 Not Found [nope]\nclose 1000\n";

#[test]
fn mir_serve_ws_upgrades_while_serving() {
    let src = SERVE_WS_SRC.replace("MODE", "plain");
    let (out, transcript) = on_both("serve-ws", &src, serve_ws_client);
    assert_eq!(out, SERVE_WS_OUT);
    assert_eq!(transcript, SERVE_WS_TRANSCRIPT);
}

#[test]
fn mir_serve_ws_tls_upgrades_while_serving() {
    let src = SERVE_WS_SRC.replace("MODE", "tls");
    let (out, transcript) = on_both("serve-ws-tls", &src, serve_ws_tls_client);
    assert_eq!(out, SERVE_WS_OUT);
    assert_eq!(transcript, SERVE_WS_TRANSCRIPT);
}

/// `Server.serve` answers each connection in a task of its own, keeping it
/// for the client's next request (B-2026-10-25-41): a client that stalls
/// halfway through a request holds up no other, a handler that panics ends
/// only its own connection, and a connection is kept until the client asks
/// it closed.
const SERVE_SRC: &str = r#"
import std.process.{exit};

fn route(req: Request) -> Response {
    let path = req.path();
    println(f"request {path}");
    if path == "/boom" {
        let none: Option[i64] = None;
        println(f"unreachable {none.unwrap()}");
    }
    if path == "/stop" {
        exit(0);
    }
    Response { status: 200, body: f"you asked {path}" }
}

fn main() {
    match Server.serve("127.0.0.1:0".to_string(), route) {
        Ok(()) => println("served"),
        Err(e) => println(f"failed: {e.message}"),
    }
}
"#;

/// Each response's status line, its `connection` header when it has one,
/// and its body.
fn ask(s: &mut std::net::TcpStream, request: &str) -> String {
    s.write_all(request.as_bytes()).unwrap();
    match response(s) {
        Ok((head, body)) => {
            let mut lines = head.lines();
            let mut t = lines.next().unwrap().to_string();
            for l in lines {
                let lower = l.to_ascii_lowercase();
                if lower.starts_with("connection:") {
                    t.push_str(&format!(" | {lower}"));
                }
            }
            format!("{t} [{body}]\n")
        }
        Err(kind) => format!("no answer ({kind:?})\n"),
    }
}

fn serve_client(ports: &Ports, _config: &Arc<rustls::ClientConfig>) -> String {
    let port = ports.next();
    let get = |path: &str| format!("GET {path} HTTP/1.1\r\nHost: x\r\n\r\n");
    let mut t = String::new();
    // Two requests on one connection, the second pipelined behind a third.
    let mut kept = tcp(port);
    t.push_str(&ask(&mut kept, &get("/a")));
    t.push_str(&ask(&mut kept, &format!("{}{}", get("/b"), get("/c"))));
    t.push_str(&response(&mut kept).map_or("none".into(), |(_, b)| format!("then [{b}]\n")));
    // A request that stops halfway holds only its own connection.
    let mut stalled = tcp(port);
    stalled.write_all(b"GET /slow HT").unwrap();
    t.push_str(&ask(&mut tcp(port), &get("/d")));
    // A handler's panic ends its own connection and no other.
    t.push_str(&ask(&mut tcp(port), &get("/boom")));
    t.push_str(&ask(&mut kept, &get("/e")));
    // A client that asks the connection closed has it closed.
    let mut closing = tcp(port);
    t.push_str(&ask(
        &mut closing,
        "GET /f HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
    ));
    let mut rest = Vec::new();
    t.push_str(&format!(
        "then {:?}\n",
        closing.read_to_end(&mut rest).map(|_| rest.len())
    ));
    t.push_str(&ask(&mut tcp(port), "GET /g HTTP/1.0\r\n\r\n"));
    t.push_str(&ask(&mut stalled, "TP/1.1\r\nHost: x\r\n\r\n"));
    t.push_str(&ask(&mut tcp(port), &get("/stop")));
    t
}

#[test]
fn mir_serve_answers_connections_concurrently_with_keep_alive() {
    let (out, transcript) = on_both("serve", SERVE_SRC, serve_client);
    assert_eq!(
        out,
        "request /a\nrequest /b\nrequest /c\nrequest /d\nrequest /boom\nrequest /e\n\
         request /f\nrequest /g\nrequest /slow\nrequest /stop\n"
    );
    assert_eq!(
        transcript,
        "HTTP/1.1 200 OK [you asked /a]\nHTTP/1.1 200 OK [you asked /b]\n\
         then [you asked /c]\nHTTP/1.1 200 OK [you asked /d]\n\
         no answer (UnexpectedEof)\nHTTP/1.1 200 OK [you asked /e]\n\
         HTTP/1.1 200 OK | connection: close [you asked /f]\nthen Ok(0)\n\
         HTTP/1.1 200 OK | connection: close [you asked /g]\n\
         HTTP/1.1 200 OK [you asked /slow]\nno answer (UnexpectedEof)\n"
    );
}
