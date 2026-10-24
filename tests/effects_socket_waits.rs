//! The library's socket, TLS, WebSocket and child-process waits carry the
//! execution verbs `docs/library/io.md` gives them: `accept`, `connect`,
//! `read`, `write`, UDP `recv` and `Child.wait` are `blocks suspends`, and the
//! binds and UDP sends `blocks` (`docs/core-semantics.md` §11.8). A public
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
        &[
            "performs blocks() but does not declare it (via call to 'TcpStream.read'",
            "performs suspends() but does not declare it (via call to 'TcpStream.read'",
        ],
    );
}

#[test]
fn undeclared_tcp_read_asks_for_blocks_and_suspends() {
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
        &["Add: with sends(Network) receives(Network) blocks suspends"],
    );
}

#[test]
fn undeclared_bind_asks_for_its_network_effects_and_blocks() {
    refused(
        "bind-undeclared",
        "pub fn open(addr: Str) -> bool {
             TcpListener.bind(addr).is_ok()
         }
         fn main() {}",
        &["Add: with sends(Network) receives(Network) blocks"],
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
        &["performs blocks() but does not declare it (via call to 'Child.wait'"],
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
        &["Add: with sends(ProcessTable) blocks suspends"],
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
        &["performs blocks() but does not declare it (via call to 'WebSocket.send_text'"],
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
        &["performs blocks() but does not declare it (via call to 'UdpSocket.recv'"],
    );
}

#[test]
fn declared_blocks_covers_every_socket_and_process_wait() {
    let out = check(
        "declared-blocks",
        "pub fn pull(s: ref TcpStream) -> i64 with sends(Network) receives(Network) blocks {
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
