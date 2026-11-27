//! The library's socket, TLS, WebSocket and child-process waits carry the
//! execution verbs `docs/library/io.md` gives them (`docs/core-semantics.md`
//! §11.8): `accept`, `connect`, `read`, `write` and UDP `send` and `recv`
//! are `suspends` (B-2026-11-27-10), `Child.wait` is `blocks suspends`, and
//! the binds, which wait on no peer, carry neither verb (B-2026-11-27-72). A public
//! caller has to declare them, and a declared `blocks` covers `suspends`.

use std::path::PathBuf;
use std::process::{Command, Output};

fn check(tag: &str, src: &str) -> Output {
    let dir: PathBuf = std::env::temp_dir().join(format!(
        "karac-socket-waits-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("p.kara");
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    out
}

fn refused(tag: &str, src: &str, expect: &[&str]) {
    let out = check(tag, src);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{tag}: check must refuse: {err}");
    for e in expect {
        assert!(err.contains(e), "{tag}: expected `{e}` in: {err}");
    }
}

#[test]
fn tcp_read_under_a_network_only_clause_is_refused() {
    refused(
        "tcp-read",
        "pub fn pull(s: ref TcpStream) -> i64 with sends(Network) receives(Network) {
             let mut buf = [0u8; 16];
             match s.read(mut buf) {
                 Ok(n) => n,
                 Err(_) => -1,
             }
         }
         fn main() {}",
        &["performs suspends but does not declare it (via call to 'TcpStream.read'"],
    );
    let out = check(
        "tcp-read-no-blocks",
        "pub fn pull(s: ref TcpStream) -> i64 with sends(Network) receives(Network) {
             let mut buf = [0u8; 16];
             match s.read(mut buf) {
                 Ok(n) => n,
                 Err(_) => -1,
             }
         }
         fn main() {}",
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !err.contains("performs blocks"),
        "a socket read does not block: {err}"
    );
}

#[test]
fn undeclared_tcp_read_asks_for_suspends_alone() {
    refused(
        "tcp-read-undeclared",
        "pub fn pull(s: ref TcpStream) -> i64 {
             let mut buf = [0u8; 16];
             match s.read(mut buf) {
                 Ok(n) => n,
                 Err(_) => -1,
             }
         }
         fn main() {}",
        // A read only receives (io.md § Effects; B-2026-11-25-35), and its
        // wait suspends without blocking (B-2026-11-27-10).
        &["Add: with receives(Network) suspends to"],
    );
}

#[test]
fn undeclared_bind_asks_for_its_network_effects_alone() {
    refused(
        "bind-undeclared",
        "pub fn open(addr: Str) -> bool {
             TcpListener.bind(addr).is_ok()
         }
         fn main() {}",
        &["Add: with receives(Network) sends(Network) to"],
    );
}

#[test]
fn child_wait_under_a_process_only_clause_is_refused() {
    refused(
        "child-wait",
        "pub fn finish(c: ref Child) -> bool with sends(ProcessTable) {
             match c.wait() {
                 Ok(st) => st.success(),
                 Err(_) => false,
             }
         }
         fn main() {}",
        &["performs blocks but does not declare it (via call to 'Child.wait'"],
    );
}

#[test]
fn undeclared_child_wait_asks_for_blocks_and_suspends() {
    refused(
        "child-wait-undeclared",
        "pub fn finish(c: ref Child) -> bool {
             match c.wait() {
                 Ok(st) => st.success(),
                 Err(_) => false,
             }
         }
         fn main() {}",
        &["Add: with blocks sends(ProcessTable) suspends"],
    );
}

#[test]
fn websocket_send_under_a_network_only_clause_is_refused() {
    refused(
        "ws-send",
        "pub fn greet(ws: mut ref WebSocket) -> bool with sends(Network) receives(Network) {
             ws.send_text(\"hi\").is_ok()
         }
         fn main() {}",
        &["performs suspends but does not declare it (via call to 'WebSocket.send_text'"],
    );
}

#[test]
fn udp_recv_under_a_network_only_clause_is_refused() {
    refused(
        "udp-recv",
        "pub fn take(s: ref UdpSocket) -> i64 with receives(Network) {
             let mut buf = [0u8; 16];
             match s.recv(mut buf) {
                 Ok(n) => n,
                 Err(_) => -1,
             }
         }
         fn main() {}",
        &["performs suspends but does not declare it (via call to 'UdpSocket.recv'"],
    );
}

#[test]
fn declared_blocks_covers_every_socket_and_process_wait() {
    let out = check(
        "declared-blocks",
        "import std.net.{TcpListener, TcpStream, TlsStream, WebSocket};
import std.process.{Child};

pub fn pull(s: ref TcpStream) -> i64 with sends(Network) receives(Network) blocks {
             let mut buf = [0u8; 16];
             match s.read(mut buf) {
                 Ok(n) => n,
                 Err(_) => -1,
             }
         }
         pub fn pull_tls(s: ref TlsStream) -> i64 with sends(Network) receives(Network) blocks {
             let mut buf = [0u8; 16];
             match s.read(mut buf) {
                 Ok(n) => n,
                 Err(_) => -1,
             }
         }
         pub fn take(s: ref UdpSocket) -> i64 with receives(Network) blocks {
             let mut buf = [0u8; 16];
             match s.recv(mut buf) {
                 Ok(n) => n,
                 Err(_) => -1,
             }
         }
         pub fn finish(c: ref Child) -> bool with sends(ProcessTable) blocks {
             match c.wait() {
                 Ok(st) => st.success(),
                 Err(_) => false,
             }
         }
         pub fn serve(l: ref TcpListener) -> bool with sends(Network) receives(Network) blocks {
             match WebSocket.accept(l) {
                 Ok(ws) => {
                     let mut ws = ws;
                     ws.send_text(\"hi\").is_ok()
                 }
                 Err(_) => false,
             }
         }
         pub fn open(addr: Str) -> bool with sends(Network) receives(Network) blocks {
             TcpListener.bind(addr).is_ok() and UdpSocket.bind(addr).is_ok()
         }
         fn main() {}",
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// §11.8 cs.851: `suspends` alone is the promise a socket waiter can make,
/// since every socket wait suspends its task (B-2026-11-27-10).
#[test]
fn declared_suspends_alone_covers_every_socket_wait() {
    let out = check(
        "declared-suspends",
        "import std.net.{TcpListener, TcpStream, TlsStream, WebSocket};

pub fn pull(s: ref TcpStream) -> i64 with sends(Network) receives(Network) suspends {
             let mut buf = [0u8; 16];
             match s.read(mut buf) {
                 Ok(n) => n,
                 Err(_) => -1,
             }
         }
         pub fn pull_tls(s: ref TlsStream) -> i64 with sends(Network) receives(Network) suspends {
             let mut buf = [0u8; 16];
             match s.read(mut buf) {
                 Ok(n) => n,
                 Err(_) => -1,
             }
         }
         pub fn swap(s: ref UdpSocket, buf: mut Slice[u8]) -> i64 with sends(Network) receives(Network) suspends {
             let _ = s.send(buf);
             match s.recv(mut buf) {
                 Ok(n) => n,
                 Err(_) => -1,
             }
         }
         pub fn serve(l: ref TcpListener) -> bool with sends(Network) receives(Network) suspends {
             match WebSocket.accept(l) {
                 Ok(ws) => {
                     let mut ws = ws;
                     ws.send_text(\"hi\").is_ok()
                 }
                 Err(_) => false,
             }
         }
         pub fn dial(addr: Str) -> bool with sends(Network) receives(Network) suspends {
             TcpStream.connect(addr).is_ok()
         }
         fn main() {}",
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Binding, `shutdown_write` and a UDP `connect` wait on no peer, so they
/// carry neither execution verb: a clause with only their resource effects
/// covers them (B-2026-11-27-72).
#[test]
fn bind_shutdown_and_udp_connect_carry_no_wait_verb() {
    let out = check(
        "bind-no-wait",
        "import std.net.{TcpListener, TcpStream, UdpSocket};

pub fn open(addr: Str) -> bool with sends(Network) receives(Network) {
             TcpListener.bind(addr).is_ok() and UdpSocket.bind(addr).is_ok()
         }
         pub fn half(s: ref TcpStream) -> bool with sends(Network) {
             s.shutdown_write().is_ok()
         }
         pub fn aim(s: ref UdpSocket, addr: Str) -> bool with sends(Network) {
             s.connect(addr).is_ok()
         }
         fn main() {}",
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
