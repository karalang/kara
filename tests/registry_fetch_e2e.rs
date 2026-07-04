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

/// Lay out a registry store whose catalog advertises `version` but whose
/// tarball file is *absent*. The proxy serves the catalog fine, then 404s on
/// the tarball GET — a deterministic, non-retryable fetch failure that drives
/// the `E_REGISTRY_FETCH_FAILED` diagnostic (wrapping the underlying proxy
/// `not found`). Used to pin the `--output=json` error envelope.
fn build_store_catalog_only(name: &str, version: &str) -> PathBuf {
    let root = unique("store-catonly");
    std::fs::create_dir_all(root.join("catalog")).unwrap();
    write(
        &root.join("catalog").join(format!("{name}.json")),
        format!(r#"{{ "upstream": "https://example.test/{name}", "versions": ["{version}"] }}"#)
            .as_bytes(),
    );
    root
}

/// Lay out a registry store whose catalog lists `version` and marks it
/// `yanked`. No tarball is needed — a fresh resolve refuses the yanked
/// version at *selection* time (registry-proxy follow-up (l)), before any
/// tarball GET. Proves the reference server serves the `yanked` array
/// verbatim and the client honors it end-to-end.
fn build_store_yanked_catalog(name: &str, version: &str) -> PathBuf {
    let root = unique("store-yanked");
    std::fs::create_dir_all(root.join("catalog")).unwrap();
    write(
        &root.join("catalog").join(format!("{name}.json")),
        format!(
            r#"{{ "upstream": "https://example.test/{name}", "versions": ["{version}"], "yanked": ["{version}"] }}"#
        )
        .as_bytes(),
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

/// `karac resolve` (registry-proxy follow-up (j)) drives the same fetch path
/// as `build` — a registry dep resolves to a real `registry` source rather
/// than the unsupported warning — but stays **read-only**: it prints the graph
/// and does NOT write `kara.lock`.
#[test]
fn resolve_lists_a_fetched_registry_dep_read_only() {
    let store = build_store("resolved_dep", "1.0.0");
    let base = start_server(store);
    let cache = unique("cache-resolve");

    let proj = unique("proj-resolve");
    write(
        &proj.join("kara.toml"),
        b"[package]\nname = \"app\"\n\n[dependencies]\nresolved_dep = \"1.0\"\n",
    );
    write(&proj.join("src/main.kara"), b"fn main() {}\n");

    let out = karac()
        .arg("resolve")
        .arg("--output=json")
        .env("KARAC_REGISTRY_PROXY", &base)
        .env("KARAC_REGISTRY_CACHE_ROOT", &cache)
        .current_dir(&proj)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    let line = stdout
        .lines()
        .find(|l| l.trim_start().starts_with('{'))
        .unwrap_or_else(|| {
            panic!("expected a JSON envelope on stdout;\nstdout={stdout}\nstderr={stderr}")
        });
    let v: serde_json::Value = serde_json::from_str(line.trim())
        .unwrap_or_else(|e| panic!("stdout line is not valid JSON ({e});\nline={line}"));

    assert_eq!(v["status"], "ok", "resolve should succeed;\n{v}");
    assert_eq!(v["command"], "resolve", "envelope command;\n{v}");
    let pkgs = v["packages"].as_array().unwrap();
    let dep = pkgs
        .iter()
        .find(|p| p["name"] == "resolved_dep")
        .unwrap_or_else(|| panic!("fetched registry dep missing from resolution;\n{v}"));
    // It resolved to a registry source — i.e. it was fetched, not reported
    // unsupported.
    assert_eq!(
        dep["source"], "registry",
        "the dep should resolve to a registry source;\n{dep}"
    );

    // Read-only: resolve must not persist a lockfile.
    assert!(
        !proj.join("kara.lock").exists(),
        "karac resolve must not write kara.lock (it is read-only)"
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

/// Direct-from-source registry fetch under `--no-proxy` (registry-proxy
/// follow-ups (j)/(k)). With the proxy bypassed, `karac build` fetches the
/// registry dep *directly* from the configured upstream registry
/// (`KARAC_REGISTRY_URL`), which serves the same catalog / pkg protocol. The
/// reference server stands in for that upstream — identical wire protocol,
/// different base URL. Proves the full fetch → extract → resolve → lock →
/// import path runs with no proxy in the loop.
#[test]
fn build_no_proxy_fetches_direct_from_registry() {
    let store = build_store("direct_dep", "1.0.0");
    let base = start_server(store);
    let cache = unique("cache-direct");

    let proj = unique("proj-direct");
    write(
        &proj.join("kara.toml"),
        b"[package]\nname = \"app\"\n\n[dependencies]\ndirect_dep = \"1.0\"\n",
    );
    write(
        &proj.join("src/main.kara"),
        b"import direct_dep.answer;\n\nfn main() {\n    let _ = answer();\n}\n",
    );

    let out = karac()
        .arg("build")
        .arg("--no-proxy")
        // No KARAC_REGISTRY_PROXY — direct-from-source uses the upstream URL.
        .env_remove("KARAC_REGISTRY_PROXY")
        .env("KARAC_REGISTRY_URL", &base)
        .env("KARAC_REGISTRY_CACHE_ROOT", &cache)
        .current_dir(&proj)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);

    // The `--no-proxy` note reports direct-from-source, not warn-and-continue.
    assert!(
        stderr.contains("direct-from-source"),
        "the --no-proxy note should report direct-from-source fetch;\nstderr={stderr}",
    );
    // The dep went down the fetch path, not the unsupported-warning path.
    assert!(
        !stderr.contains("E_REGISTRY_DEP_UNSUPPORTED"),
        "registry dep should have been fetched direct-from-source, not reported unsupported;\nstderr={stderr}\nstdout={stdout}",
    );
    assert!(
        !stderr.contains("E_REGISTRY_FETCH_FAILED"),
        "direct fetch should have succeeded against the live upstream;\nstderr={stderr}",
    );
    assert!(
        !stderr.contains("error["),
        "expected a clean build past resolution;\nstderr={stderr}",
    );

    // Filesystem proof: the tarball was extracted into the cache root, exactly
    // as the proxy path does — the provider stack is shared.
    let extracted = cache.join("direct_dep").join("1.0.0").join("src");
    assert!(
        extracted.join("kara.toml").is_file() && extracted.join("src/lib.kara").is_file(),
        "expected the direct-fetched package to be extracted under {}; entries missing",
        extracted.display(),
    );

    // The lockfile records the dep against a registry source.
    let lock = std::fs::read_to_string(proj.join("kara.lock")).unwrap_or_default();
    assert!(
        lock.contains("direct_dep") && lock.contains("registry"),
        "kara.lock should pin the direct-fetched registry dep;\nlock={lock}",
    );

    let _ = std::fs::remove_dir_all(&proj);
    let _ = std::fs::remove_dir_all(&cache);
}

/// The contract complement for `--no-proxy`: with no upstream registry
/// configured (`KARAC_REGISTRY_URL` unset, no `[build].registry` pin), a
/// registry dep must keep the warn-and-continue contract rather than attempt
/// a direct fetch against nothing. Proves the direct-from-source path is gated
/// on an explicit upstream, symmetric to the proxy path's
/// `explicit_proxy_configured` gate.
#[test]
fn build_no_proxy_without_direct_registry_keeps_unsupported_warning() {
    let proj = unique("proj-direct-unconfigured");
    write(
        &proj.join("kara.toml"),
        b"[package]\nname = \"app\"\n\n[dependencies]\ndirect_dep = \"1.0\"\n",
    );
    write(&proj.join("src/main.kara"), b"fn main() {}\n");

    let out = karac()
        .arg("build")
        .arg("--no-proxy")
        .env_remove("KARAC_REGISTRY_PROXY")
        .env_remove("KARAC_REGISTRY_URL")
        .current_dir(&proj)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&proj);

    assert!(
        stderr.contains("warning[E_REGISTRY_DEP_UNSUPPORTED]"),
        "an unconfigured direct registry must keep the warn-and-continue contract;\nstderr={stderr}",
    );
    assert!(
        !stderr.contains("error[E_REGISTRY"),
        "no registry error should surface without a configured upstream;\nstderr={stderr}",
    );
}

/// `--output=json` must emit a machine-readable error envelope when a registry
/// dependency fails to fetch against a configured proxy. Pins the shape
/// registry-proxy carve-out (m) promised (`docs/implementation_checklist/
/// phase-5-diagnostics.md`): a single `{"status":"error", ...}` object whose
/// one diagnostic carries `severity:"error"`, `phase:"dep_resolution"`, and
/// the `E_REGISTRY_FETCH_FAILED` code, with the underlying proxy `not found`
/// preserved in the notes. A downstream tool (LLM agent, IDE, CI gate)
/// consumes exactly this — so it is frozen here as a golden shape.
#[test]
fn build_output_json_pins_proxy_fetch_error_shape() {
    // Catalog advertises 1.0.0 but the tarball is missing → the proxy 404s the
    // package GET → a non-retryable fetch failure (no backoff wait).
    let store = build_store_catalog_only("ghost_dep", "1.0.0");
    let base = start_server(store);
    let cache = unique("cache-json");

    let proj = unique("proj-json");
    write(
        &proj.join("kara.toml"),
        b"[package]\nname = \"app\"\n\n[dependencies]\nghost_dep = \"1.0\"\n",
    );
    write(&proj.join("src/main.kara"), b"fn main() {}\n");

    let out = karac()
        .arg("build")
        .arg("--output=json")
        .env("KARAC_REGISTRY_PROXY", &base)
        .env("KARAC_REGISTRY_CACHE_ROOT", &cache)
        .current_dir(&proj)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    // The JSON envelope lands on stdout (diagnostics go to stderr only in text
    // mode). Locate the one object line so any incidental stdout notice ahead
    // of it can't break the parse.
    let line = stdout
        .lines()
        .find(|l| l.trim_start().starts_with('{'))
        .unwrap_or_else(|| {
            panic!("expected a JSON envelope on stdout;\nstdout={stdout}\nstderr={stderr}")
        });
    let v: serde_json::Value = serde_json::from_str(line.trim())
        .unwrap_or_else(|e| panic!("stdout line is not valid JSON ({e});\nline={line}"));

    // Top-level envelope: a failed build.
    assert_eq!(
        v["status"], "error",
        "fetch failure must set status=error;\nenvelope={v}"
    );
    let diags = v["diagnostics"]
        .as_array()
        .unwrap_or_else(|| panic!("diagnostics must be an array;\nenvelope={v}"));
    assert!(
        !diags.is_empty(),
        "expected at least one diagnostic;\nenvelope={v}"
    );

    let d = &diags[0];
    assert_eq!(
        d["severity"], "error",
        "a proxy fetch failure is an error;\ndiag={d}"
    );
    assert_eq!(
        d["phase"], "dep_resolution",
        "proxy/registry fetch diagnostics surface under the dep_resolution phase;\ndiag={d}"
    );
    assert_eq!(
        d["code"], "E_REGISTRY_FETCH_FAILED",
        "a proxy fetch failure surfaces the registry-fetch code;\ndiag={d}"
    );
    assert!(
        d["message"].as_str().unwrap_or("").contains("ghost_dep"),
        "the message must name the dependency that failed;\ndiag={d}"
    );
    // The underlying proxy error is preserved in the notes so an operator sees
    // *why* the fetch failed (here: the proxy's 404 body).
    let notes = d["notes"]
        .as_array()
        .unwrap_or_else(|| panic!("notes must be an array;\ndiag={d}"));
    assert!(
        notes.iter().any(|n| n
            .as_str()
            .map(|s| s.contains("underlying error"))
            .unwrap_or(false)),
        "notes must carry the underlying proxy error;\ndiag={d}"
    );

    // A failed fetch must not leave a lockfile pinning the unresolved dep.
    assert!(
        !proj.join("kara.lock").exists(),
        "a failed fetch must not persist a lockfile"
    );

    let _ = std::fs::remove_dir_all(&proj);
    let _ = std::fs::remove_dir_all(&cache);
}

/// A registry dep whose only matching version has been *yanked* must be
/// refused at resolve time (registry-proxy follow-up (l)) — a fresh resolve
/// never selects a withdrawn version. The reference server serves the
/// catalog's `yanked` array verbatim (no server change), and `karac build`
/// surfaces the distinct "yanked" diagnostic (wrapped in
/// `E_REGISTRY_FETCH_FAILED`) rather than fetching a tarball or emitting a
/// misleading "no matching version".
#[test]
fn build_refuses_only_yanked_registry_dep() {
    let store = build_store_yanked_catalog("stale_dep", "1.0.0");
    let base = start_server(store);
    let cache = unique("cache-yanked");

    let proj = unique("proj-yanked");
    write(
        &proj.join("kara.toml"),
        b"[package]\nname = \"app\"\n\n[dependencies]\nstale_dep = \"1.0\"\n",
    );
    write(&proj.join("src/main.kara"), b"fn main() {}\n");

    let out = karac()
        .arg("build")
        .env("KARAC_REGISTRY_PROXY", &base)
        .env("KARAC_REGISTRY_CACHE_ROOT", &cache)
        .current_dir(&proj)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);

    // The build failed on the fetch, surfacing the yank-specific wording.
    assert!(
        stderr.contains("E_REGISTRY_FETCH_FAILED"),
        "a yanked-only dep must fail the fetch;\nstderr={stderr}",
    );
    assert!(
        stderr.contains("yanked"),
        "the diagnostic must explain the version was yanked, not just 'no match';\nstderr={stderr}",
    );
    // It was refused at *selection* — never misreported as a missing version.
    assert!(
        !stderr.contains("E_REGISTRY_NO_MATCHING_VERSION"),
        "a yanked match must not be reported as no-matching-version;\nstderr={stderr}",
    );

    let _ = std::fs::remove_dir_all(&proj);
    let _ = std::fs::remove_dir_all(&cache);
}
