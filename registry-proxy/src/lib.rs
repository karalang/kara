//! Minimal **reference / dev** registry proxy for the Kāra package manager.
//!
//! This is a filesystem-backed reference implementation of the registry
//! proxy wire protocol (see `docs/registry-proxy-protocol.md`). It serves
//! two endpoints straight off a local directory:
//!
//! - `GET /catalog/<name>`            → the JSON catalog file
//!   `<root>/catalog/<name>.json`, verbatim.
//! - `GET /pkg/<name>/<version>.tar.gz` → the tarball
//!   `<root>/pkg/<name>/<version>.tar.gz`, with a `Karac-Content-Hash:
//!   blake3:<hex>` header computed over the body.
//!
//! **Scope.** It exists to (1) pin the wire protocol *executably* — the
//! `HttpProxyClient` and this server agree by construction — and (2) be the
//! integration-test fixture the client talks to over a real socket, plus a
//! deployable local/self-host mirror. It is deliberately **not** the
//! production `proxy.kara-lang.org`: there is no upstream mirroring,
//! caching, authentication, signature serving, or high-availability
//! (phase-5-diagnostics.md registry-proxy follow-ups c/d/e/f). Requests
//! other than `GET` on the two routes get a `404` / `405`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A fully-formed HTTP response: status + a single body. The reference
/// server never needs chunked encoding or keep-alive, so this is a flat
/// value serialized once via [`Response::to_http_bytes`].
pub struct Response {
    pub status: u16,
    pub reason: &'static str,
    pub content_type: &'static str,
    /// Extra headers beyond `Content-Type` / `Content-Length` (e.g. the
    /// `Karac-Content-Hash` tarball digest).
    pub extra_headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    fn new(status: u16, reason: &'static str, content_type: &'static str, body: Vec<u8>) -> Self {
        Response {
            status,
            reason,
            content_type,
            extra_headers: Vec::new(),
            body,
        }
    }

    fn text(status: u16, reason: &'static str, body: &str) -> Self {
        Response::new(
            status,
            reason,
            "text/plain; charset=utf-8",
            body.as_bytes().to_vec(),
        )
    }

    fn not_found(what: &str) -> Self {
        Response::text(404, "Not Found", &format!("not found: {what}\n"))
    }

    /// Serialize to a raw HTTP/1.1 response with an explicit
    /// `Connection: close` (the server closes after every request).
    pub fn to_http_bytes(&self) -> Vec<u8> {
        let mut head = format!(
            "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n",
            self.status,
            self.reason,
            self.content_type,
            self.body.len(),
        );
        for (k, v) in &self.extra_headers {
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        head.push_str("\r\n");
        let mut out = head.into_bytes();
        out.extend_from_slice(&self.body);
        out
    }
}

/// A single request segment is safe to join onto the store root iff it is
/// a plain name with no path-traversal or separator characters. Guards the
/// filesystem mapping against `..` / absolute-path / separator injection.
fn safe_segment(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && !s.contains('/')
        && !s.contains('\\')
        && !s.contains('\0')
}

/// Filesystem-backed store rooted at a directory laid out as:
///
/// ```text
/// <root>/catalog/<name>.json
/// <root>/pkg/<name>/<version>.tar.gz
/// ```
pub struct FsStore {
    root: PathBuf,
}

impl FsStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        FsStore { root: root.into() }
    }

    /// Route + serve one request. Pure over the filesystem — no socket
    /// state — so it is directly unit-testable and shared by both the
    /// binary and the integration-test server.
    pub fn handle(&self, method: &str, target: &str) -> Response {
        if method != "GET" {
            return Response::text(405, "Method Not Allowed", "only GET is supported\n");
        }
        // Drop any query string before routing.
        let path = target.split('?').next().unwrap_or(target);
        let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

        match segs.as_slice() {
            ["catalog", name] if safe_segment(name) => self.serve_catalog(name),
            ["pkg", name, file] if safe_segment(name) && safe_segment(file) => {
                match file.strip_suffix(".tar.gz") {
                    Some(version) if !version.is_empty() => self.serve_package(name, file, version),
                    _ => Response::not_found(path),
                }
            }
            _ => Response::not_found(path),
        }
    }

    fn serve_catalog(&self, name: &str) -> Response {
        let file = self.root.join("catalog").join(format!("{name}.json"));
        match std::fs::read(&file) {
            Ok(bytes) => Response::new(200, "OK", "application/json", bytes),
            Err(_) => Response::not_found(&format!("catalog for package {name:?}")),
        }
    }

    fn serve_package(&self, name: &str, file: &str, version: &str) -> Response {
        let path = self.root.join("pkg").join(name).join(file);
        match std::fs::read(&path) {
            Ok(bytes) => {
                let hash = format!("blake3:{}", blake3::hash(&bytes).to_hex());
                let mut resp = Response::new(200, "OK", "application/gzip", bytes);
                resp.extra_headers
                    .push(("Karac-Content-Hash".to_string(), hash));
                resp
            }
            Err(_) => Response::not_found(&format!("version {version} of package {name:?}")),
        }
    }
}

/// Parse the request line + drain the header block from a client
/// connection, returning `(method, target)`. Returns `None` on a malformed
/// or empty request. The body (if any) is ignored — the protocol is
/// GET-only.
pub fn read_request(stream: &TcpStream) -> Option<(String, String)> {
    let mut reader = BufReader::new(stream);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).ok()? == 0 {
        return None; // client closed without sending anything
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();

    // Drain headers until the blank line so the client's write completes
    // cleanly before we respond + close.
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            break;
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
    }
    Some((method, target))
}

/// Handle one connection: read the request, serve it, write the response,
/// close. Errors are swallowed (a dropped connection is not fatal to the
/// server).
pub fn handle_connection(mut stream: TcpStream, store: &FsStore) {
    let response = match read_request(&stream) {
        Some((method, target)) => store.handle(&method, &target),
        None => return,
    };
    let bytes = response.to_http_bytes();
    let _ = stream.write_all(&bytes);
    let _ = stream.flush();
    // Read-drain briefly so the client sees our response rather than a RST
    // on some platforms, then close.
    let _ = stream.shutdown(std::net::Shutdown::Write);
    let mut sink = [0u8; 256];
    let _ = stream.read(&mut sink);
}

/// Accept loop: serve every incoming connection on `listener`, one thread
/// per connection, until the listener is dropped/closed. Shared by the
/// binary and the integration tests.
pub fn serve(listener: TcpListener, store: Arc<FsStore>) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let store = Arc::clone(&store);
        std::thread::spawn(move || handle_connection(stream, &store));
    }
}

/// True if `root` looks like a proxy store root (has a `catalog/` or
/// `pkg/` subdirectory). Advisory — used by the binary to warn on an empty
/// root rather than silently 404 everything.
pub fn looks_like_store_root(root: &Path) -> bool {
    root.join("catalog").is_dir() || root.join("pkg").is_dir()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    fn temp_store() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("kara-regproxy-lib-{}-{n}", std::process::id()));
        std::fs::create_dir_all(dir.join("catalog")).unwrap();
        std::fs::create_dir_all(dir.join("pkg")).unwrap();
        dir
    }

    #[test]
    fn catalog_route_serves_file_verbatim() {
        let root = temp_store();
        std::fs::write(
            root.join("catalog").join("serde.json"),
            r#"{"upstream":"u"}"#,
        )
        .unwrap();
        let store = FsStore::new(&root);

        let resp = store.handle("GET", "/catalog/serde");
        assert_eq!(resp.status, 200);
        assert_eq!(resp.content_type, "application/json");
        assert_eq!(resp.body, br#"{"upstream":"u"}"#);
    }

    #[test]
    fn package_route_sets_blake3_content_hash() {
        let root = temp_store();
        let dir = root.join("pkg").join("serde");
        std::fs::create_dir_all(&dir).unwrap();
        let body = b"tarball bytes";
        std::fs::write(dir.join("1.2.3.tar.gz"), body).unwrap();
        let store = FsStore::new(&root);

        let resp = store.handle("GET", "/pkg/serde/1.2.3.tar.gz");
        assert_eq!(resp.status, 200);
        assert_eq!(resp.content_type, "application/gzip");
        assert_eq!(resp.body, body);
        let expected = format!("blake3:{}", blake3::hash(body).to_hex());
        assert_eq!(
            resp.extra_headers,
            vec![("Karac-Content-Hash".to_string(), expected)]
        );
    }

    #[test]
    fn missing_files_are_404() {
        let store = FsStore::new(temp_store());
        assert_eq!(store.handle("GET", "/catalog/ghost").status, 404);
        assert_eq!(store.handle("GET", "/pkg/ghost/1.0.0.tar.gz").status, 404);
    }

    #[test]
    fn non_get_is_405() {
        let store = FsStore::new(temp_store());
        assert_eq!(store.handle("POST", "/catalog/serde").status, 405);
    }

    #[test]
    fn unknown_and_traversal_routes_are_404() {
        let store = FsStore::new(temp_store());
        assert_eq!(store.handle("GET", "/").status, 404);
        assert_eq!(store.handle("GET", "/catalog").status, 404);
        // A non-".tar.gz" package request is not a valid route.
        assert_eq!(store.handle("GET", "/pkg/serde/1.0.0.zip").status, 404);
        // Path traversal must never escape the store root.
        assert_eq!(store.handle("GET", "/catalog/..").status, 404);
        assert_eq!(store.handle("GET", "/pkg/../secret/x.tar.gz").status, 404);
    }

    #[test]
    fn query_string_is_ignored_when_routing() {
        let root = temp_store();
        std::fs::write(root.join("catalog").join("serde.json"), b"{}").unwrap();
        let store = FsStore::new(&root);
        assert_eq!(store.handle("GET", "/catalog/serde?v=2").status, 200);
    }

    #[test]
    fn safe_segment_rejects_traversal() {
        assert!(safe_segment("serde"));
        assert!(safe_segment("1.2.3.tar.gz"));
        assert!(!safe_segment(""));
        assert!(!safe_segment(".."));
        assert!(!safe_segment("a/b"));
        assert!(!safe_segment("a\\b"));
    }

    #[test]
    fn response_serializes_headers_and_body() {
        let mut resp = Response::new(200, "OK", "application/gzip", b"xy".to_vec());
        resp.extra_headers
            .push(("Karac-Content-Hash".to_string(), "blake3:ab".to_string()));
        let raw = String::from_utf8(resp.to_http_bytes()).unwrap();
        assert!(raw.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(raw.contains("Content-Type: application/gzip\r\n"));
        assert!(raw.contains("Content-Length: 2\r\n"));
        assert!(raw.contains("Karac-Content-Hash: blake3:ab\r\n"));
        assert!(raw.ends_with("\r\n\r\nxy"));
    }
}
