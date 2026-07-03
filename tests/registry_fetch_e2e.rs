// tests/registry_fetch_e2e.rs
//
// End-to-end proof of the registry-proxy fetch path (registry-fetch epic
// slice 4 — CLI activation). Spins up the `kara-registry-proxy` reference
// server over a loopback socket, points `karac build` at it with
// `KARAC_REGISTRY_PROXY`, and verifies that a `[dependencies]` registry
// entry is fetched, extracted, resolved, locked, and made importable — the
// full path a real `karac build` walks against a live proxy.
//
// This is the integration counterpart to the unit tests in `registry_proxy`
// / `registry_extract` / `dep_resolver`: those exercise each layer with
// in-memory stand-ins; here the real `ureq`-backed `HttpProxyClient`,
// on-disk cache, tar extraction, and multi-module compilation all run for
// real against a genuine `karac` subprocess.

use kara_registry_proxy::{serve, FsStore};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn unique(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("kara-fetch-e2e-{tag}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(path: &Path, contents: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

/// Build a gzip-compressed tarball in memory from `(path, contents)` entries.
fn make_targz(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut builder = tar::Builder::new(gz);
    for (path, contents) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, path, *contents).unwrap();
    }
    let gz = builder.into_inner().unwrap();
    gz.finish().unwrap()
}

/// Lay out a registry store (`catalog/` + `pkg/`) holding a single library
/// package `name` at `version`, whose tarball carries a `kara.toml` + a
/// `src/lib.kara` exposing `pub fn answer() -> i64 { 42 }`.
fn build_store(name: &str, version: &str) -> PathBuf {
    let root = unique("store");
    std::fs::create_dir_all(root.join("catalog")).unwrap();
    std::fs::create_dir_all(root.join("pkg").join(name)).unwrap();

    write(
        &root.join("catalog").join(format!("{name}.json")),
        format!(r#"{{ "upstream": "https://example.test/{name}", "versions": ["{version}"] }}"#)
            .as_bytes(),
    );

    let kara_toml = format!("[package]\nname = \"{name}\"\n");
    let lib = b"pub fn answer() -> i64 { 42 }\n";
    let targz = make_targz(&[("kara.toml", kara_toml.as_bytes()), ("src/lib.kara", lib)]);
    write(
        &root
            .join("pkg")
            .join(name)
            .join(format!("{version}.tar.gz")),
        &targz,
    );
    root
}

/// Start the reference server on an ephemeral loopback port; return the base
/// URL. The thread is detached (dies with the test process).
fn start_server(root: PathBuf) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || serve(listener, Arc::new(FsStore::new(root))));
    format!("http://{addr}")
}

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

#[test]
fn build_fetches_and_resolves_a_registry_dependency() {
    let store = build_store("demo_dep", "1.0.0");
    let base = start_server(store);
    let cache = unique("cache");

    // A project depending on the registry package by version constraint, and
    // importing its `answer` fn so the fetched module is actually compiled.
    let proj = unique("proj");
    write(
        &proj.join("kara.toml"),
        b"[package]\nname = \"app\"\n\n[dependencies]\ndemo_dep = \"1.0\"\n",
    );
    write(
        &proj.join("src/main.kara"),
        b"import demo_dep.answer;\n\nfn main() {\n    let _ = answer();\n}\n",
    );

    let out = karac()
        .arg("build")
        .env("KARAC_REGISTRY_PROXY", &base)
        .env("KARAC_REGISTRY_CACHE_ROOT", &cache)
        .current_dir(&proj)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);

    // 1. The dep was NOT reported unsupported — it went down the fetch path.
    assert!(
        !stderr.contains("E_REGISTRY_DEP_UNSUPPORTED"),
        "registry dep should have been fetched, not reported unsupported;\nstderr={stderr}\nstdout={stdout}",
    );
    // 2. No fetch failure surfaced (proxy is live and serves the package).
    assert!(
        !stderr.contains("E_REGISTRY_FETCH_FAILED"),
        "fetch should have succeeded against the live proxy;\nstderr={stderr}",
    );
    // 3. No resolution/typecheck errors — the fetched module hoisted and the
    //    `import demo_dep.answer;` resolved, proving `dep_package_walks`
    //    compiled the extracted source root.
    assert!(
        !stderr.contains("error["),
        "expected a clean build past resolution;\nstderr={stderr}",
    );

    // 4. Filesystem proof: the tarball was extracted into the cache root.
    let extracted = cache.join("demo_dep").join("1.0.0").join("src");
    assert!(
        extracted.join("kara.toml").is_file() && extracted.join("src/lib.kara").is_file(),
        "expected the fetched package to be extracted under {}; entries missing",
        extracted.display(),
    );

    // 5. The lockfile records the dep against a registry source.
    let lock = std::fs::read_to_string(proj.join("kara.lock")).unwrap_or_default();
    assert!(
        lock.contains("demo_dep") && lock.contains("registry"),
        "kara.lock should pin the fetched registry dep;\nlock={lock}",
    );

    let _ = std::fs::remove_dir_all(&proj);
    let _ = std::fs::remove_dir_all(&cache);
}

#[test]
fn build_without_configured_proxy_keeps_unsupported_warning() {
    // The contract complement: with no `KARAC_REGISTRY_PROXY` (and the
    // built-in placeholder URL not being live), the registry dep must still
    // warn-and-continue rather than attempt a fetch against the placeholder.
    let proj = unique("proj-noproxy");
    write(
        &proj.join("kara.toml"),
        b"[package]\nname = \"app\"\n\n[dependencies]\ndemo_dep = \"1.0\"\n",
    );
    write(&proj.join("src/main.kara"), b"fn main() {}\n");

    let out = karac()
        .arg("build")
        .env_remove("KARAC_REGISTRY_PROXY")
        .current_dir(&proj)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&proj);

    assert!(
        stderr.contains("warning[E_REGISTRY_DEP_UNSUPPORTED]"),
        "unconfigured proxy must keep the warn-and-continue contract;\nstderr={stderr}",
    );
    assert!(
        !stderr.contains("error[E_REGISTRY"),
        "no registry error should surface without a configured proxy;\nstderr={stderr}",
    );
}
