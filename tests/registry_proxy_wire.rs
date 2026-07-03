// tests/registry_proxy_wire.rs
//
// Integration test for the live-HTTP `HttpProxyClient` (registry-proxy
// tracker line 930). Drives the real `ureq`-backed client against the
// `kara-registry-proxy` reference server over a loopback socket — a full
// request/response round-trip, not canned `MemProxyClient` data — plus a
// hand-rolled "lying" responder to exercise the content-hash integrity
// check. Every `HttpProxyClient` error arm is covered.

use kara_registry_proxy::{serve, FsStore};
use karac::registry_proxy::{HttpProxyClient, ProxyClient, ProxyClientError};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

// ── Fixtures ────────────────────────────────────────────────────

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// A fresh, unique temp store root laid out as `catalog/` + `pkg/`.
fn temp_store() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("kara-regproxy-{}-{n}", std::process::id()));
    std::fs::create_dir_all(dir.join("catalog")).unwrap();
    std::fs::create_dir_all(dir.join("pkg")).unwrap();
    dir
}

fn write_catalog(root: &Path, name: &str, json: &str) {
    std::fs::write(root.join("catalog").join(format!("{name}.json")), json).unwrap();
}

fn write_package(root: &Path, name: &str, version: &str, bytes: &[u8]) {
    let dir = root.join("pkg").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{version}.tar.gz")), bytes).unwrap();
}

/// Start the reference server on an ephemeral loopback port; return its
/// base URL. The server thread is detached (dies with the test process).
fn start_reference_server(root: PathBuf) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || serve(listener, Arc::new(FsStore::new(root))));
    format!("http://{addr}")
}

fn ver(s: &str) -> semver::Version {
    semver::Version::parse(s).unwrap()
}

// ── Happy paths ─────────────────────────────────────────────────

#[test]
fn fetch_catalog_returns_parsed_manifest() {
    let root = temp_store();
    write_catalog(
        &root,
        "serde",
        r#"{ "upstream": "https://github.com/serde-rs/serde", "versions": ["1.0.0", "1.2.3"] }"#,
    );
    let client = HttpProxyClient::new(start_reference_server(root));

    let manifest = client.fetch_catalog("serde").expect("catalog fetch");
    assert_eq!(manifest.package, "serde");
    assert_eq!(manifest.upstream_url, "https://github.com/serde-rs/serde");
    assert_eq!(manifest.versions, vec![ver("1.0.0"), ver("1.2.3")]);
}

#[test]
fn fetch_package_returns_bytes_and_verified_hash() {
    let root = temp_store();
    let tarball = b"\x1f\x8b\x08 not really gzip but distinctive bytes for the digest";
    write_package(&root, "serde", "1.2.3", tarball);
    let base = start_reference_server(root);
    let client = HttpProxyClient::new(base.clone());

    let pkg = client
        .fetch_package("serde", &ver("1.2.3"))
        .expect("package fetch");
    assert_eq!(pkg.package, "serde");
    assert_eq!(pkg.version, ver("1.2.3"));
    assert_eq!(pkg.tarball_bytes, tarball);
    assert_eq!(
        pkg.content_hash,
        format!("blake3:{}", blake3::hash(tarball).to_hex()),
    );
    assert_eq!(pkg.mirror_url, format!("{base}/pkg/serde/1.2.3.tar.gz"));
    // Upstream URL is a catalog-level attribute, not carried by the tarball
    // endpoint — the client leaves it empty here (documented behavior).
    assert_eq!(pkg.upstream_url, "");
}

// ── Not-found arms ──────────────────────────────────────────────

#[test]
fn missing_catalog_is_package_not_found() {
    let root = temp_store();
    let client = HttpProxyClient::new(start_reference_server(root));
    match client.fetch_catalog("ghost") {
        Err(ProxyClientError::PackageNotFound { name }) => assert_eq!(name, "ghost"),
        other => panic!("expected PackageNotFound, got {other:?}"),
    }
}

#[test]
fn missing_version_is_version_not_found() {
    let root = temp_store();
    write_catalog(
        &root,
        "serde",
        r#"{ "upstream": "u", "versions": ["1.0.0"] }"#,
    );
    write_package(&root, "serde", "1.0.0", b"present");
    let client = HttpProxyClient::new(start_reference_server(root));
    match client.fetch_package("serde", &ver("9.9.9")) {
        Err(ProxyClientError::VersionNotFound { name, version }) => {
            assert_eq!(name, "serde");
            assert_eq!(version, ver("9.9.9"));
        }
        other => panic!("expected VersionNotFound, got {other:?}"),
    }
}

// ── Malformed-response arms ─────────────────────────────────────

#[test]
fn invalid_json_catalog_is_malformed() {
    let root = temp_store();
    write_catalog(&root, "broken", "{ this is not json ");
    let client = HttpProxyClient::new(start_reference_server(root));
    assert!(matches!(
        client.fetch_catalog("broken"),
        Err(ProxyClientError::MalformedResponse { .. })
    ));
}

#[test]
fn catalog_missing_versions_field_is_malformed() {
    let root = temp_store();
    write_catalog(&root, "noversions", r#"{ "upstream": "u" }"#);
    let client = HttpProxyClient::new(start_reference_server(root));
    assert!(matches!(
        client.fetch_catalog("noversions"),
        Err(ProxyClientError::MalformedResponse { .. })
    ));
}

#[test]
fn unparseable_version_string_is_malformed() {
    let root = temp_store();
    write_catalog(
        &root,
        "badver",
        r#"{ "upstream": "u", "versions": ["not.a.version"] }"#,
    );
    let client = HttpProxyClient::new(start_reference_server(root));
    assert!(matches!(
        client.fetch_catalog("badver"),
        Err(ProxyClientError::MalformedResponse { .. })
    ));
}

// ── Transport arm ───────────────────────────────────────────────

#[test]
fn no_server_is_unreachable() {
    // Nothing listens on 127.0.0.1:1 — connection refused → Unreachable.
    let client = HttpProxyClient::new("http://127.0.0.1:1".to_string());
    assert!(matches!(
        client.fetch_catalog("anything"),
        Err(ProxyClientError::Unreachable { .. })
    ));
}

// ── Integrity check: a proxy that advertises the wrong digest ───

#[test]
fn content_hash_mismatch_is_malformed() {
    // A hand-rolled responder that serves a tarball body with a bogus
    // `Karac-Content-Hash`, so the client's integrity check must reject it.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf); // drain request
            let body = b"real tarball bytes";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/gzip\r\n\
                 Karac-Content-Hash: blake3:{}\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n",
                "0".repeat(64), // deliberately wrong digest
                body.len(),
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.write_all(body);
            let _ = stream.flush();
        }
    });

    let client = HttpProxyClient::new(format!("http://{addr}"));
    match client.fetch_package("serde", &ver("1.0.0")) {
        Err(ProxyClientError::MalformedResponse { message, .. }) => {
            assert!(
                message.contains("content-hash mismatch"),
                "unexpected message: {message}"
            );
        }
        other => panic!("expected MalformedResponse (hash mismatch), got {other:?}"),
    }
}

// ── Authentication: Authorization: Bearer <token> (follow-up (e)) ──

/// Spawn a one-shot responder that requires `Authorization: Bearer
/// <expected>`. A matching request gets a valid catalog (200); anything
/// else — missing or wrong token — gets a bare `401 Unauthorized`. Returns
/// the base URL.
fn spawn_auth_responder(expected: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]);
            let authorized = request
                .lines()
                .any(|line| line.trim() == format!("Authorization: Bearer {expected}"));
            if authorized {
                let body = r#"{ "upstream": "u", "versions": ["1.0.0"] }"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body,
                );
                let _ = stream.write_all(response.as_bytes());
            } else {
                let _ = stream.write_all(
                    b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
            }
            let _ = stream.flush();
        }
    });
    format!("http://{addr}")
}

#[test]
fn matching_bearer_token_is_authorized() {
    let base = spawn_auth_responder("s3cr3t");
    let client = HttpProxyClient::with_token(base, Some("s3cr3t".to_string()));
    let manifest = client
        .fetch_catalog("private")
        .expect("authorized catalog fetch");
    assert_eq!(manifest.versions, vec![ver("1.0.0")]);
}

#[test]
fn missing_token_against_private_proxy_is_unauthorized() {
    let base = spawn_auth_responder("s3cr3t");
    // No token supplied — the private proxy rejects with 401.
    let client = HttpProxyClient::new(base.clone());
    match client.fetch_catalog("private") {
        Err(ProxyClientError::Unauthorized { url, status }) => {
            assert_eq!(status, 401);
            assert!(url.contains("/catalog/private"), "unexpected url: {url}");
        }
        other => panic!("expected Unauthorized, got {other:?}"),
    }
}

#[test]
fn wrong_token_is_unauthorized() {
    let base = spawn_auth_responder("s3cr3t");
    let client = HttpProxyClient::with_token(base, Some("wrong-token".to_string()));
    match client.fetch_catalog("private") {
        Err(ProxyClientError::Unauthorized { status, .. }) => assert_eq!(status, 401),
        other => panic!("expected Unauthorized, got {other:?}"),
    }
    assert_eq!(
        ProxyClientError::Unauthorized {
            url: "x".to_string(),
            status: 401,
        }
        .code(),
        "E_PROXY_UNAUTHORIZED",
    );
}
