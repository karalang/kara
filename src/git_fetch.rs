//! Git dependency fetching — the direct-from-source path (registry-proxy
//! follow-up (k) / phase-5 resolver follow-up (c)).
//!
//! A `[dependencies] foo = { git = "https://…" }` entry has no proxy in the
//! loop: its source *is* the upstream repository, so the client clones it
//! directly. This module is the atomic "git dep → on-disk source root" step
//! the dep-graph walk builds on, mirroring `registry_proxy`'s
//! `fetch_registry_package` / `RegistryProvider`:
//!
//! - [`GitProvider`] is the trait the graph walk consumes (a `None` provider
//!   preserves the pre-fetch `E_GIT_DEP_UNSUPPORTED` behavior).
//! - [`GitCliProvider`] is the production impl. It shells out to the `git`
//!   binary — the same lightweight approach the rest of the CLI uses (see
//!   `workspace_has_uncommitted_changes` in `cli.rs`) rather than linking
//!   libgit2 into every build.
//!
//! Clones land under a content-addressed cache directory keyed by
//! `blake3(url \0 ref)`, so distinct URL/ref pairs never collide and a repeat
//! resolve reuses an existing checkout instead of re-cloning.

use crate::manifest::GitRef;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A git dependency materialized on disk: the manifest root plus the exact
/// commit the ref resolved to (recorded in `kara.lock` for reproducibility).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializedGitDep {
    /// Directory holding the checked-out package's `kara.toml`.
    pub root_dir: PathBuf,
    /// The full commit SHA `HEAD` points at after checkout.
    pub resolved_rev: String,
}

/// Abstraction over "clone a git URL at a ref → on-disk source root".
/// Mirrors [`crate::dep_graph::RegistryProvider`]; threaded into the graph
/// walk so a git dep is fetched and recursed into exactly like a path dep.
pub trait GitProvider {
    /// Clone `url`, check out `reference` (or the default branch when
    /// `None`), and return where the manifest landed.
    fn fetch(
        &self,
        url: &str,
        reference: Option<&GitRef>,
    ) -> Result<MaterializedGitDep, GitFetchError>;
}

/// Failure cloning or checking out a git dependency.
#[derive(Debug)]
pub enum GitFetchError {
    /// The `git` binary could not be launched (not installed / not on PATH).
    GitUnavailable { message: String },
    /// A `git` subcommand exited non-zero. `step` names which one.
    CommandFailed {
        step: &'static str,
        url: String,
        message: String,
    },
    /// The clone succeeded but the tree carries no `kara.toml`.
    NoManifest { url: String },
}

impl GitFetchError {
    /// Symbolic diagnostic code, mirroring the registry-fetch error codes.
    pub fn code(&self) -> &'static str {
        match self {
            Self::GitUnavailable { .. } => "E_GIT_UNAVAILABLE",
            Self::CommandFailed { .. } => "E_GIT_FETCH_FAILED",
            Self::NoManifest { .. } => "E_GIT_NO_MANIFEST",
        }
    }
}

impl std::fmt::Display for GitFetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GitUnavailable { message } => {
                write!(f, "could not run `git` (is it installed?): {message}")
            }
            Self::CommandFailed { step, url, message } => {
                write!(f, "git {step} of `{url}` failed: {message}")
            }
            Self::NoManifest { url } => {
                write!(
                    f,
                    "git dependency `{url}` has no `kara.toml` at its repository root"
                )
            }
        }
    }
}

impl std::error::Error for GitFetchError {}

/// The checkout target for a [`GitRef`] — a branch name, tag, or commit SHA.
/// `git checkout` resolves all three uniformly, so the inner string is the
/// argument regardless of variant.
fn ref_target(reference: &GitRef) -> &str {
    match reference {
        GitRef::Branch(s) | GitRef::Tag(s) | GitRef::Rev(s) => s,
    }
}

/// Content-addressed cache slot for a `(url, ref)` pair: `blake3(url \0 ref)`
/// hex. Distinct refs of the same repo get distinct slots, so a branch and a
/// pinned rev never clobber each other.
fn cache_slot(cache_root: &Path, url: &str, reference: Option<&GitRef>) -> PathBuf {
    let mut hasher = blake3::Hasher::new();
    hasher.update(url.as_bytes());
    hasher.update(&[0]);
    hasher.update(reference.map(ref_target).unwrap_or("").as_bytes());
    cache_root.join(hasher.finalize().to_hex().as_str())
}

/// Production [`GitProvider`]: shells out to `git clone` + `git checkout`
/// into a per-`(url, ref)` slot beneath `cache_root`.
pub struct GitCliProvider {
    cache_root: PathBuf,
}

impl GitCliProvider {
    pub fn new(cache_root: impl Into<PathBuf>) -> Self {
        Self {
            cache_root: cache_root.into(),
        }
    }

    /// Run a `git` subcommand, mapping a launch failure to `GitUnavailable`
    /// and a non-zero exit to `CommandFailed`.
    fn run_git(&self, step: &'static str, url: &str, args: &[&str]) -> Result<(), GitFetchError> {
        let output =
            Command::new("git")
                .args(args)
                .output()
                .map_err(|e| GitFetchError::GitUnavailable {
                    message: e.to_string(),
                })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(GitFetchError::CommandFailed {
                step,
                url: url.to_string(),
                message: stderr.trim().to_string(),
            });
        }
        Ok(())
    }

    /// `git -C <dir> rev-parse HEAD` → the checked-out commit SHA.
    fn resolve_head(&self, url: &str, dir: &Path) -> Result<String, GitFetchError> {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["rev-parse", "HEAD"])
            .output()
            .map_err(|e| GitFetchError::GitUnavailable {
                message: e.to_string(),
            })?;
        if !output.status.success() {
            return Err(GitFetchError::CommandFailed {
                step: "rev-parse",
                url: url.to_string(),
                message: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }
}

impl GitProvider for GitCliProvider {
    fn fetch(
        &self,
        url: &str,
        reference: Option<&GitRef>,
    ) -> Result<MaterializedGitDep, GitFetchError> {
        let dir = cache_slot(&self.cache_root, url, reference);

        // Reuse an intact prior checkout (a `.git` + a manifest), mirroring
        // the registry extractor's idempotent "already extracted → reuse".
        let intact = dir.join(".git").exists() && dir.join("kara.toml").is_file();
        if !intact {
            // Clear any partial/failed prior attempt so `git clone` (which
            // refuses a non-empty target) starts clean.
            if dir.exists() {
                std::fs::remove_dir_all(&dir).map_err(|e| GitFetchError::CommandFailed {
                    step: "clean",
                    url: url.to_string(),
                    message: e.to_string(),
                })?;
            }
            if let Some(parent) = dir.parent() {
                std::fs::create_dir_all(parent).map_err(|e| GitFetchError::CommandFailed {
                    step: "clean",
                    url: url.to_string(),
                    message: e.to_string(),
                })?;
            }
            let dir_str = dir.to_string_lossy();
            // Full (non-shallow) clone so an arbitrary rev/tag is reachable
            // for the checkout below.
            self.run_git("clone", url, &["clone", "--quiet", url, &dir_str])?;
            if let Some(reference) = reference {
                let dir_arg = dir.to_string_lossy();
                self.run_git(
                    "checkout",
                    url,
                    &[
                        "-C",
                        &dir_arg,
                        "-c",
                        "advice.detachedHead=false",
                        "checkout",
                        "--quiet",
                        ref_target(reference),
                    ],
                )?;
            }
        }

        if !dir.join("kara.toml").is_file() {
            return Err(GitFetchError::NoManifest {
                url: url.to_string(),
            });
        }
        let resolved_rev = self.resolve_head(url, &dir)?;
        Ok(MaterializedGitDep {
            root_dir: dir,
            resolved_rev,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    fn temp_dir(tag: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("kara-gitfetch-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn git(args: &[&str], cwd: &Path) {
        let out = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Build a throwaway upstream git repo with a `kara.toml` + a lib, commit
    /// it, and optionally tag it. Returns the repo path (usable as a
    /// `file://`-less local clone source — `git clone <path>` works directly).
    fn make_upstream(name: &str, extra_file: Option<(&str, &str)>) -> PathBuf {
        let repo = temp_dir("upstream");
        git(&["init", "--quiet", "-b", "main"], &repo);
        git(&["config", "user.email", "t@t.test"], &repo);
        git(&["config", "user.name", "Test"], &repo);
        std::fs::write(
            repo.join("kara.toml"),
            format!("[package]\nname = \"{name}\"\n"),
        )
        .unwrap();
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(
            repo.join("src/lib.kara"),
            b"pub fn answer() -> i64 { 42 }\n",
        )
        .unwrap();
        if let Some((path, contents)) = extra_file {
            std::fs::write(repo.join(path), contents).unwrap();
        }
        git(&["add", "-A"], &repo);
        git(&["commit", "--quiet", "-m", "init"], &repo);
        repo
    }

    #[test]
    fn clones_default_branch_and_resolves_head() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let upstream = make_upstream("gitdep", None);
        let cache = temp_dir("cache");
        let provider = GitCliProvider::new(&cache);

        let mat = provider
            .fetch(&upstream.to_string_lossy(), None)
            .expect("fetch");
        assert!(mat.root_dir.join("kara.toml").is_file());
        assert!(mat.root_dir.join("src/lib.kara").is_file());
        // HEAD SHA is a 40-char hex string.
        assert_eq!(mat.resolved_rev.len(), 40, "rev={}", mat.resolved_rev);
        assert!(mat.resolved_rev.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn checks_out_a_tag() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let upstream = make_upstream("tagged", None);
        git(&["tag", "v1.0.0"], &upstream);
        // Add a second commit so HEAD != the tag, proving checkout moved us.
        std::fs::write(upstream.join("extra.txt"), b"later\n").unwrap();
        git(&["add", "-A"], &upstream);
        git(&["commit", "--quiet", "-m", "second"], &upstream);

        let cache = temp_dir("cache");
        let provider = GitCliProvider::new(&cache);
        let mat = provider
            .fetch(
                &upstream.to_string_lossy(),
                Some(&GitRef::Tag("v1.0.0".to_string())),
            )
            .expect("fetch");
        // The tagged commit predates extra.txt.
        assert!(!mat.root_dir.join("extra.txt").exists());
        assert!(mat.root_dir.join("kara.toml").is_file());
    }

    #[test]
    fn checks_out_a_specific_rev() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let upstream = make_upstream("revpin", None);
        // Capture the first commit SHA, then add a second.
        let first = {
            let out = Command::new("git")
                .arg("-C")
                .arg(&upstream)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        std::fs::write(upstream.join("extra.txt"), b"later\n").unwrap();
        git(&["add", "-A"], &upstream);
        git(&["commit", "--quiet", "-m", "second"], &upstream);

        let cache = temp_dir("cache");
        let provider = GitCliProvider::new(&cache);
        let mat = provider
            .fetch(
                &upstream.to_string_lossy(),
                Some(&GitRef::Rev(first.clone())),
            )
            .expect("fetch");
        assert_eq!(mat.resolved_rev, first);
        assert!(!mat.root_dir.join("extra.txt").exists());
    }

    #[test]
    fn reuses_an_existing_checkout_without_recloning() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let upstream = make_upstream("reuse", None);
        let cache = temp_dir("cache");
        let provider = GitCliProvider::new(&cache);
        let url = upstream.to_string_lossy().to_string();

        let first = provider.fetch(&url, None).expect("fetch 1");
        // Drop a sentinel into the checkout; a reuse must not blow it away
        // (a re-clone would).
        let sentinel = first.root_dir.join("SENTINEL");
        std::fs::write(&sentinel, b"x").unwrap();
        let second = provider.fetch(&url, None).expect("fetch 2");
        assert_eq!(first.root_dir, second.root_dir);
        assert!(sentinel.is_file(), "reuse must not re-clone over the slot");
    }

    #[test]
    fn missing_manifest_is_an_error() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        // A repo with no kara.toml.
        let repo = temp_dir("nomanifest");
        git(&["init", "--quiet", "-b", "main"], &repo);
        git(&["config", "user.email", "t@t.test"], &repo);
        git(&["config", "user.name", "Test"], &repo);
        std::fs::write(repo.join("README.md"), b"hi\n").unwrap();
        git(&["add", "-A"], &repo);
        git(&["commit", "--quiet", "-m", "init"], &repo);

        let cache = temp_dir("cache");
        let provider = GitCliProvider::new(&cache);
        let err = provider
            .fetch(&repo.to_string_lossy(), None)
            .expect_err("should reject a manifest-less repo");
        assert_eq!(err.code(), "E_GIT_NO_MANIFEST");
    }

    #[test]
    fn nonexistent_url_fails_the_clone() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let cache = temp_dir("cache");
        let provider = GitCliProvider::new(&cache);
        let missing = temp_dir("does-not-exist").join("nope-repo");
        let err = provider
            .fetch(&missing.to_string_lossy(), None)
            .expect_err("cloning a nonexistent path must fail");
        assert_eq!(err.code(), "E_GIT_FETCH_FAILED");
    }

    #[test]
    fn distinct_refs_get_distinct_cache_slots() {
        let cache = temp_dir("cache");
        let url = "https://example.test/repo.git";
        let a = cache_slot(&cache, url, Some(&GitRef::Tag("v1".to_string())));
        let b = cache_slot(&cache, url, Some(&GitRef::Tag("v2".to_string())));
        let none = cache_slot(&cache, url, None);
        assert_ne!(a, b);
        assert_ne!(a, none);
        // Stable: same inputs → same slot.
        assert_eq!(
            a,
            cache_slot(&cache, url, Some(&GitRef::Tag("v1".to_string())))
        );
    }
}
