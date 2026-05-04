//! Integration tests for CR-24 slice 2 — `kara.toml` manifest loading and
//! project-root discovery. Unit tests for the parser itself live next to the
//! code in `src/manifest.rs#tests`; here we exercise the filesystem side of
//! `discover_project_root` and `load_from_root`.

use karac::manifest::{
    self, discover_project_root, load_from_cwd, load_from_root, DEFAULT_EDITION, MANIFEST_FILENAME,
};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

static TEST_ID: AtomicU32 = AtomicU32::new(0);

/// Scratch directory under `std::env::temp_dir()` that is cleaned up when
/// the guard is dropped. Using a per-test subdir keeps the tests parallel-safe
/// without pulling in the `tempfile` crate.
struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    fn new(tag: &str) -> Self {
        let id = TEST_ID.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "karac-manifest-test-{}-{}-{}",
            std::process::id(),
            tag,
            id,
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create scratch dir");
        ScratchDir { path }
    }

    fn write(&self, rel: &str, contents: &str) -> PathBuf {
        let full = self.path.join(rel);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut f = fs::File::create(&full).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        full
    }

    fn root(&self) -> &Path {
        &self.path
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[test]
fn discover_finds_manifest_in_start_dir() {
    let scratch = ScratchDir::new("discover-here");
    scratch.write(MANIFEST_FILENAME, "[package]\nname = \"x\"\n");
    let found = discover_project_root(scratch.root()).expect("manifest should be found");
    assert_eq!(found, scratch.root());
}

#[test]
fn discover_walks_up_to_parent() {
    let scratch = ScratchDir::new("walk-up");
    scratch.write(MANIFEST_FILENAME, "[package]\nname = \"x\"\n");
    let src_dir = scratch.root().join("src").join("db");
    fs::create_dir_all(&src_dir).unwrap();
    let found = discover_project_root(&src_dir).expect("walk-up should find parent manifest");
    assert_eq!(found, scratch.root());
}

#[test]
fn discover_returns_none_when_no_manifest() {
    let scratch = ScratchDir::new("no-manifest");
    // No kara.toml written.
    let found = discover_project_root(scratch.root());
    // The walk-up may legitimately hit a `kara.toml` somewhere above the
    // temp dir (unlikely, but possible in odd CI setups). Accept either
    // `None` or a root outside our scratch dir — what we're really verifying
    // is that nothing inside the scratch dir claims to be a project root.
    if let Some(p) = found {
        assert!(
            !p.starts_with(scratch.root()),
            "unexpected manifest found inside scratch: {p:?}",
        );
    }
}

#[test]
fn load_from_root_parses_minimum_manifest() {
    let scratch = ScratchDir::new("min-load");
    scratch.write(
        MANIFEST_FILENAME,
        r#"[package]
name = "hello"
"#,
    );
    let m = load_from_root(scratch.root()).unwrap();
    assert_eq!(m.name, "hello");
    assert_eq!(m.edition, DEFAULT_EDITION);
}

#[test]
fn load_from_root_propagates_missing_package() {
    let scratch = ScratchDir::new("missing-package");
    scratch.write(MANIFEST_FILENAME, "[dependencies]\nhttp = \"1.2\"\n");
    let err = load_from_root(scratch.root()).unwrap_err();
    assert!(matches!(
        err,
        manifest::ManifestError::MissingPackageSection { .. }
    ));
}

#[test]
fn load_from_root_surfaces_invalid_toml() {
    let scratch = ScratchDir::new("bad-toml");
    scratch.write(MANIFEST_FILENAME, "[[[not valid");
    let err = load_from_root(scratch.root()).unwrap_err();
    assert!(matches!(err, manifest::ManifestError::InvalidToml { .. }));
}

#[test]
fn load_from_cwd_returns_e0227_when_no_manifest() {
    let scratch = ScratchDir::new("cwd-e0227");
    // No manifest in scratch, and we walk up from the isolated scratch dir.
    // If a parent happens to have one we can't detect E0227 reliably, so
    // only check the error code when discovery actually fails.
    if let Err(err) = load_from_cwd(scratch.root()) {
        assert_eq!(err.code(), Some("E0227"));
    }
}

#[test]
fn unknown_sections_are_silently_ignored() {
    let scratch = ScratchDir::new("ignored-sections");
    scratch.write(
        MANIFEST_FILENAME,
        r#"[package]
name = "hello"
edition = "2026"

[dependencies]
http = "1.2"
json = { version = "0.8", git = "https://example.com/json-kara" }

[dev-dependencies]
proptest = "0.4"

[build]
target = "x86_64-linux"

[workspace]
members = ["core", "cli"]
"#,
    );
    let m = load_from_root(scratch.root()).unwrap();
    assert_eq!(m.name, "hello");
    assert_eq!(m.edition, "2026");
    assert!(
        m.warnings.is_empty(),
        "unexpected warnings: {:?}",
        m.warnings
    );
}

#[test]
fn unknown_package_keys_soft_warn_but_do_not_fail() {
    // `homepage` is outside the v1 allow-list (`name`, `edition`, `version`,
    // `authors`) — it must still soft-warn so typos surface. CR-36 expanded
    // the allow-list to include `version` / `authors`; this test covers the
    // genuinely-unknown path that remains.
    let scratch = ScratchDir::new("unknown-package-keys");
    scratch.write(
        MANIFEST_FILENAME,
        r#"[package]
name = "hello"
homepage = "https://example.com"
documentation = "https://example.com/docs"
"#,
    );
    let m = load_from_root(scratch.root()).unwrap();
    assert_eq!(m.name, "hello");
    assert_eq!(m.warnings.len(), 2);
    assert!(m.warnings.iter().any(|w| w.message.contains("homepage")));
    assert!(m
        .warnings
        .iter()
        .any(|w| w.message.contains("documentation")));
    for w in &m.warnings {
        assert!(w.message.contains("ignored"));
    }
}

#[test]
fn canonical_scaffolded_manifest_has_no_warnings() {
    // Matches what `karac init` writes — must round-trip through the parser
    // with zero warnings so fresh projects don't see manifest noise on first
    // `karac build`. See CR-36 T3d.
    let scratch = ScratchDir::new("scaffolded-manifest");
    scratch.write(
        MANIFEST_FILENAME,
        r#"[package]
name = "hello"
version = "0.1.0"
authors = []
edition = "2026"

[dependencies]
"#,
    );
    let m = load_from_root(scratch.root()).unwrap();
    assert_eq!(m.name, "hello");
    assert_eq!(m.edition, "2026");
    assert!(m.warnings.is_empty(), "{:?}", m.warnings);
}
