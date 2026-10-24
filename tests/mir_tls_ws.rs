//! The TLS server (`TlsListener` / `TlsStream`) and the WebSocket server
//! (`WebSocket`, `WsMessage`), M4 item 8, on both MIR backends: each
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
