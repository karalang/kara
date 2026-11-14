//! A redirect's `Location` is resolved as RFC 3986 §5.2 resolves a
//! reference, on the MIR interpreter and the compiled client alike
//! (B-2026-11-14-46): `?page=2` keeps the current path, and `.` / `..`
//! segments are removed (`../d` from `/a/b/c` is `/a/d`). Both clients sent
//! `?page=2` to `/`, and the compiled one sent the dot segments as they were
//! (`/a/b/../d`), so a server that does not normalize answered 404 there
//! while the interpreter, whose URL parser removed them, reached the page.

use std::io::{BufRead, BufReader, Write};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A loopback server: `/a/b/c` redirects to `../d`, `/q` to `?page=2`,
/// `/dot` to `/x/./y/../../echo`, `/far` to `//127.0.0.1:PORT/up/../echo`,
/// and anything else answers 200 with the request target it was sent,
/// exactly as it came, so an unresolved `..` shows.
fn server() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("port").port();
    std::thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            std::thread::spawn(move || {
                let mut out = conn.try_clone().expect("clone");
                let mut r = BufReader::new(conn);
                loop {
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
                    let loc = match target.as_str() {
                        "/a/b/c" => Some("../d".to_string()),
                        "/q" => Some("?page=2".to_string()),
                        "/dot" => Some("/x/./y/../../echo".to_string()),
                        "/far" => Some(format!("//127.0.0.1:{port}/up/../echo")),
                        _ => None,
                    };
                    let reply = match loc {
                        Some(l) => format!(
                            "HTTP/1.1 302 Found\r\nLocation: {l}\r\nContent-Length: 0\r\n\r\n"
                        ),
                        None => format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{target}",
                            target.len()
                        ),
                    };
                    if out.write_all(reply.as_bytes()).is_err() {
                        return;
                    }
                }
            });
        }
    });
    port
}

const SRC: &str = r#"
fn main() {
    let c = Client.new();
    for p in ["a/b/c", "q", "dot", "far"] {
        match c.get(f"http://127.0.0.1:PORT/{p}") {
            Ok(r) => println(f"{p}: {r.status()} {r.body()}"),
            Err(e) => println(f"{p}: {e.message()}"),
        }
    }
}
"#;

const WANT: &str = "a/b/c: 200 /a/d\nq: 200 /q?page=2\ndot: 200 /echo\nfar: 200 /echo\n";

fn check(mode: &str) {
    static N: AtomicUsize = AtomicUsize::new(0);
    let port = server();
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("karac-redir-res-{}-{n}", std::process::id()));
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
fn interp_redirect_location_resolves_per_rfc_3986() {
    check("__mir-run");
}

#[cfg(feature = "llvm")]
#[test]
fn compiled_redirect_location_resolves_per_rfc_3986() {
    check("__mir-llvm-run");
}
