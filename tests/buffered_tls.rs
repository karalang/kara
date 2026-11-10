//! `BufReader` and `BufWriter` over a `TlsStream` (docs/library/io.md §
//! Buffered I/O; B-2026-11-10-28), on both MIR backends: a buffered read
//! waits inside its task for the handshake and the bytes, as the stream's
//! own read does, and its timeout, a peer gone without `close_notify` and a
//! peer that speaks no TLS come back as `IoError`s. The interpreter and the
//! built binary must print the same and send the client the same. The
//! harness is `tests/mir_tls_ws.rs`'s.
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
        let dir =
            std::env::temp_dir().join(format!("karac-buffered-tls-{name}-{}", std::process::id()));
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

    /// Waits for the server to write `marker` on a stderr line of its
    /// own, skipping any other line.
    fn wait_for(&self, marker: &str) {
        loop {
            let line = self
                .0
                .recv_timeout(Duration::from_secs(60))
                .unwrap_or_else(|_| panic!("the server wrote no {marker:?} within 60s"));
            if line.trim() == marker {
                return;
            }
        }
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

const SRC: &str = r#"import std.io.{BufReader, BufWriter};
import std.sync.{TaskGroup};

fn lines(tag: Str, s: own TlsStream) -> String {
    let r = BufReader.with_capacity(s, 8);
    let mut out = String.new();
    for x in r.lines() {
        match x {
            Ok(t) => out.push_str(f"{tag} {t} "),
            Err(e) => {
                out.push_str(f"{tag} err {e}");
                break;
            }
        }
    }
    eprintln(f"DONE_{tag}");
    out
}

fn main() {
    let cert = fs.read_to_string("DIR/cert.pem").unwrap();
    let key = fs.read_to_string("DIR/key.pem").unwrap();
    let l = TlsListener.bind_tls("127.0.0.1:0", cert, key).unwrap();
    // Two connections, each read in its own task. The client handshakes
    // on the second and sends it its lines before it starts the first, so
    // the first task waits inside its task, the handshake included.
    let a = l.accept().unwrap();
    let b = l.accept().unwrap();
    let g = TaskGroup.new();
    let ha = g.spawn(|| lines("a", a));
    let hb = g.spawn(|| lines("b", b));
    println(ha.join());
    println(hb.join());
    // Written through an eight-byte buffer, then the drop's close_notify.
    let w = l.accept().unwrap();
    let mut bw = BufWriter.with_capacity(w, 8);
    println(f"wrote {bw.write_all("a line longer than the buffer\nand one more\n".bytes()).is_ok()}");
    println(f"flushed {bw.flush().is_ok()}");
    drop(bw);
    // A read timeout.
    let q = l.accept().unwrap();
    let _ = q.set_read_timeout(Some(100));
    let mut qr = BufReader.new(q);
    let mut line = String.new();
    match qr.read_line(mut line) {
        Ok(n) => println(f"quiet read {n}"),
        Err(e) => println(f"quiet err {e}"),
    }
    drop(qr);
    // A line, then a peer gone without close_notify.
    let d = l.accept().unwrap();
    println(lines("dropped", d));
    // A peer that speaks no TLS.
    let p = l.accept().unwrap();
    println(lines("plain", p));
}
"#;

fn client(ports: &Ports, config: &Arc<rustls::ClientConfig>) -> String {
    let port = ports.next();
    let mut t = String::new();
    let mut a = tls_stream(port, config);
    let mut b = tls_stream(port, config);
    b.write_all(b"b1\nb2 is longer than eight\n").unwrap();
    b.conn.send_close_notify();
    b.flush().unwrap();
    ports.wait_for("DONE_b");
    a.write_all(b"a1\n").unwrap();
    a.conn.send_close_notify();
    a.flush().unwrap();
    ports.wait_for("DONE_a");

    let mut w = tls_stream(port, config);
    while w.conn.is_handshaking() {
        w.conn.complete_io(&mut w.sock).unwrap();
    }
    let mut got = String::new();
    let _ = w.read_to_string(&mut got);
    t.push_str(&format!("got {got}"));

    let mut q = tls_stream(port, config);
    while q.conn.is_handshaking() {
        if q.conn.complete_io(&mut q.sock).is_err() {
            break;
        }
    }
    let mut rest = Vec::new();
    let _ = q.read_to_end(&mut rest);
    t.push_str(&format!("quiet sent {}\n", rest.len()));

    // Gone without close_notify: a FIN after the line, the socket kept
    // open so that nothing unread turns it into a reset.
    let mut d = tls_stream(port, config);
    d.write_all(b"late\n").unwrap();
    d.flush().unwrap();
    d.sock.shutdown(std::net::Shutdown::Write).unwrap();

    let mut p = tcp(port);
    p.write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
    let mut first = [0u8; 1];
    let _ = p.read_exact(&mut first);
    t.push_str(&format!("plain answer is an alert {}\n", first[0] == 21));
    drop(d);
    t
}

#[test]
fn buffered_tls_stream_on_both_backends() {
    let (out, transcript) = on_both("lines", SRC, client);
    assert_eq!(
        out,
        "a a1 \nb b1 b b2 is longer than eight \nwrote true\nflushed true\n\
         quiet err timed out\ndropped late dropped err TLS: connection closed unexpectedly\n\
         plain err TLS: handshake failed\n"
    );
    assert_eq!(
        transcript,
        "got a line longer than the buffer\nand one more\nquiet sent 0\n\
         plain answer is an alert true\n"
    );
}
