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

// ── Store builder ───────────────────────────────────────────────

/// What `build_store` produced: one entry per package, with its resolved
/// version count. Returned so the CLI can print a summary.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct BuildReport {
    /// `(package name, number of versions)`, sorted by name.
    pub packages: Vec<(String, usize)>,
}

/// JSON-escape a string for embedding in a double-quoted JSON value. Small
/// hand-rolled escaper so the crate needs no `serde_json` — the only
/// user-controlled string in a catalog is the upstream URL.
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Build a servable proxy store at `out` from a directory of packages at
/// `from`.
///
/// **Input layout** (`from`): one subdirectory per package; inside it, a
/// `<version>.tar.gz` file per release, plus an optional `upstream` (or
/// `upstream.txt`) one-line file naming the package's source URL.
///
/// ```text
/// <from>/mylib/1.0.0.tar.gz
/// <from>/mylib/1.2.3.tar.gz
/// <from>/mylib/upstream          (optional: "https://github.com/me/mylib")
/// ```
///
/// **Output layout** (`out`) is a store root ready for
/// [`serve`] / [`FsStore`]: a generated `catalog/<name>.json`
/// (`{ "upstream", "versions" }`, versions sorted ascending SemVer) plus
/// the tarballs copied to `pkg/<name>/<version>.tar.gz`.
///
/// Versions must be valid SemVer; a mis-named tarball is a hard error so
/// the mistake surfaces rather than silently dropping a release.
pub fn build_store(from: &Path, out: &Path) -> Result<BuildReport, String> {
    let catalog_dir = out.join("catalog");
    let pkg_dir = out.join("pkg");
    std::fs::create_dir_all(&catalog_dir)
        .map_err(|e| format!("could not create {}: {e}", catalog_dir.display()))?;
    std::fs::create_dir_all(&pkg_dir)
        .map_err(|e| format!("could not create {}: {e}", pkg_dir.display()))?;

    // Deterministic package order: sort the source subdirectories by name.
    let mut package_dirs: Vec<(String, PathBuf)> = Vec::new();
    let entries = std::fs::read_dir(from)
        .map_err(|e| format!("could not read source dir {}: {e}", from.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("could not read a source entry: {e}"))?;
        if !entry.path().is_dir() {
            continue; // top-level files (READMEs etc.) are ignored
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if !safe_segment(&name) {
            return Err(format!("invalid package directory name {name:?}"));
        }
        package_dirs.push((name, entry.path()));
    }
    package_dirs.sort();

    let mut report = BuildReport::default();
    for (name, dir) in package_dirs {
        // Collect (version, tarball path) for every <version>.tar.gz.
        let mut versions: Vec<(semver::Version, PathBuf)> = Vec::new();
        let mut upstream = String::new();
        let pkg_entries = std::fs::read_dir(&dir)
            .map_err(|e| format!("could not read {}: {e}", dir.display()))?;
        for entry in pkg_entries {
            let entry = entry.map_err(|e| format!("could not read an entry in {name}: {e}"))?;
            let fname = entry.file_name().to_string_lossy().into_owned();
            if fname == "upstream" || fname == "upstream.txt" {
                upstream = std::fs::read_to_string(entry.path())
                    .map_err(|e| format!("could not read upstream file for {name}: {e}"))?
                    .trim()
                    .to_string();
                continue;
            }
            let Some(version_str) = fname.strip_suffix(".tar.gz") else {
                continue; // ignore non-tarball files
            };
            let version = semver::Version::parse(version_str).map_err(|e| {
                format!("package {name}: tarball {fname:?} is not a valid <semver>.tar.gz ({e})")
            })?;
            versions.push((version, entry.path()));
        }

        if versions.is_empty() {
            eprintln!("warning: package {name:?} has no <version>.tar.gz files — skipping");
            continue;
        }

        versions.sort_by(|a, b| a.0.cmp(&b.0));

        // Copy tarballs into pkg/<name>/<version>.tar.gz.
        let dest_pkg = pkg_dir.join(&name);
        std::fs::create_dir_all(&dest_pkg)
            .map_err(|e| format!("could not create {}: {e}", dest_pkg.display()))?;
        for (version, src) in &versions {
            let dest = dest_pkg.join(format!("{version}.tar.gz"));
            std::fs::copy(src, &dest).map_err(|e| {
                format!("could not copy {} → {}: {e}", src.display(), dest.display())
            })?;
        }

        // Generate catalog/<name>.json.
        let versions_json = versions
            .iter()
            .map(|(v, _)| format!("\"{v}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let json = format!(
            "{{\n  \"upstream\": \"{}\",\n  \"versions\": [{}]\n}}\n",
            json_escape(&upstream),
            versions_json,
        );
        std::fs::write(catalog_dir.join(format!("{name}.json")), json)
            .map_err(|e| format!("could not write catalog for {name}: {e}"))?;

        report.packages.push((name, versions.len()));
    }

    Ok(report)
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

    // ── build_store ─────────────────────────────────────────────

    /// A fresh, empty temp directory (no catalog/pkg pre-created) — used
    /// as a `build_store` source or output root.
    fn temp_dir() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("kara-regproxy-src-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn build_store_generates_catalog_and_copies_tarballs() {
        let from = temp_dir();
        let pkg = from.join("mylib");
        std::fs::create_dir_all(&pkg).unwrap();
        // Deliberately out of order + a two-digit minor to prove SemVer sort.
        std::fs::write(pkg.join("1.9.0.tar.gz"), b"v190").unwrap();
        std::fs::write(pkg.join("1.10.0.tar.gz"), b"v1100").unwrap();
        std::fs::write(pkg.join("1.2.3.tar.gz"), b"v123").unwrap();
        std::fs::write(pkg.join("upstream"), "https://github.com/me/mylib\n").unwrap();

        let out = temp_dir();
        let report = build_store(&from, &out).expect("build");
        assert_eq!(report.packages, vec![("mylib".to_string(), 3)]);

        // Catalog: upstream carried through, versions ascending SemVer
        // (1.2.3 < 1.9.0 < 1.10.0 — lexical order would wrongly put 1.10.0
        // before 1.9.0).
        let catalog = std::fs::read_to_string(out.join("catalog/mylib.json")).unwrap();
        assert!(catalog.contains(r#""upstream": "https://github.com/me/mylib""#));
        assert!(catalog.contains(r#""versions": ["1.2.3", "1.9.0", "1.10.0"]"#));

        // Tarballs copied verbatim under pkg/<name>/<version>.tar.gz.
        assert_eq!(
            std::fs::read(out.join("pkg/mylib/1.10.0.tar.gz")).unwrap(),
            b"v1100"
        );

        // Round-trip: the generated store serves cleanly.
        let store = FsStore::new(&out);
        assert_eq!(store.handle("GET", "/catalog/mylib").status, 200);
        let pkg_resp = store.handle("GET", "/pkg/mylib/1.2.3.tar.gz");
        assert_eq!(pkg_resp.status, 200);
        assert_eq!(pkg_resp.body, b"v123");
    }

    #[test]
    fn build_store_defaults_missing_upstream_to_empty() {
        let from = temp_dir();
        let pkg = from.join("nolink");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(pkg.join("0.1.0.tar.gz"), b"x").unwrap();

        let out = temp_dir();
        build_store(&from, &out).expect("build");
        let catalog = std::fs::read_to_string(out.join("catalog/nolink.json")).unwrap();
        assert!(catalog.contains(r#""upstream": """#));
        assert!(catalog.contains(r#""versions": ["0.1.0"]"#));
    }

    #[test]
    fn build_store_rejects_non_semver_tarball() {
        let from = temp_dir();
        let pkg = from.join("bad");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(pkg.join("not-a-version.tar.gz"), b"x").unwrap();

        let out = temp_dir();
        let err = build_store(&from, &out).unwrap_err();
        assert!(err.contains("not a valid"), "unexpected error: {err}");
    }

    #[test]
    fn build_store_skips_package_dir_with_no_tarballs() {
        let from = temp_dir();
        std::fs::create_dir_all(from.join("empty")).unwrap();
        let out = temp_dir();
        let report = build_store(&from, &out).expect("build");
        assert!(report.packages.is_empty());
        assert!(!out.join("catalog/empty.json").exists());
    }

    #[test]
    fn json_escape_handles_special_characters() {
        assert_eq!(json_escape(r#"a"b\c"#), r#"a\"b\\c"#);
        assert_eq!(json_escape("line\nbreak"), "line\\nbreak");
    }
}
