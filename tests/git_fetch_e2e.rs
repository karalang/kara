// tests/git_fetch_e2e.rs
//
// End-to-end proof of the git-dependency fetch path (git-fetch slice 2 —
// dep-graph / resolver / CLI wiring). Builds a throwaway upstream git repo
// on disk, points a consumer project's `[dependencies] foo = { git = "..." }`
// at it, runs a real `karac build`, and verifies the dep is cloned,
// resolved, locked, and made importable — the full path a real `karac build`
// walks against a git source.
//
// The integration counterpart to the unit tests in `git_fetch`: those
// exercise the clone/checkout primitive directly; here the real dep-graph
// walk, resolver, module loader, and multi-package compilation all run for
// real against a genuine `karac` subprocess. Each test skips gracefully when
// `git` is unavailable.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn unique(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("kara-git-e2e-{tag}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(path: &Path, contents: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
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

/// Build a throwaway upstream git repo holding a library package `name` that
/// exposes `pub fn answer() -> i64 { 42 }`, committed on `main`. Returns the
/// repo path (usable directly as a `git clone` source).
fn make_upstream(name: &str) -> PathBuf {
    let repo = unique("upstream");
    git(&["init", "--quiet", "-b", "main"], &repo);
    git(&["config", "user.email", "t@t.test"], &repo);
    git(&["config", "user.name", "Test"], &repo);
    write(
        &repo.join("kara.toml"),
        format!("[package]\nname = \"{name}\"\n").as_bytes(),
    );
    write(
        &repo.join("src/lib.kara"),
        b"pub fn answer() -> i64 { 42 }\n",
    );
    git(&["add", "-A"], &repo);
    git(&["commit", "--quiet", "-m", "init"], &repo);
    repo
}

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

#[test]
fn build_fetches_and_resolves_a_git_dependency() {
    if !git_available() {
        eprintln!("skipping: git not available");
        return;
    }
    let upstream = make_upstream("git_dep");
    let cache = unique("cache");

    // The commit the default branch resolves to — the lockfile must pin it.
    let head_sha = {
        let out = Command::new("git")
            .arg("-C")
            .arg(&upstream)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };

    // A consumer project depending on the git package, importing its `answer`
    // fn so the cloned module is actually compiled.
    let proj = unique("proj");
    write(
        &proj.join("kara.toml"),
        format!(
            "[package]\nname = \"app\"\n\n[dependencies]\ngit_dep = {{ git = \"{}\" }}\n",
            upstream.display()
        )
        .as_bytes(),
    );
    write(
        &proj.join("src/main.kara"),
        b"import git_dep.answer;\n\nfn main() {\n    let _ = answer();\n}\n",
    );

    let out = karac()
        .arg("build")
        .env("KARAC_GIT_CACHE_ROOT", &cache)
        .current_dir(&proj)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);

    // 1. The dep went down the fetch path, not the unsupported warning.
    assert!(
        !stderr.contains("E_GIT_DEP_UNSUPPORTED"),
        "git dep should have been fetched, not reported unsupported;\nstderr={stderr}\nstdout={stdout}",
    );
    // 2. No clone/checkout failure (upstream is a real local repo).
    assert!(
        !stderr.contains("E_GIT_FETCH_FAILED"),
        "clone should have succeeded against the local repo;\nstderr={stderr}",
    );
    // 3. No resolution/typecheck errors — the cloned module hoisted and the
    //    `import git_dep.answer;` resolved, proving `dep_package_walks`
    //    compiled the checkout.
    assert!(
        !stderr.contains("error["),
        "expected a clean build past resolution;\nstderr={stderr}",
    );

    // 4. Filesystem proof: the repo was cloned into the git cache root (some
    //    content-addressed slot beneath it holds the package's kara.toml).
    let cloned_manifest_exists = std::fs::read_dir(&cache)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .any(|e| e.path().join("kara.toml").is_file())
        })
        .unwrap_or(false);
    assert!(
        cloned_manifest_exists,
        "expected a checked-out clone with a kara.toml under {}",
        cache.display(),
    );

    // 5. The lockfile records the dep against a git source, pinned to the
    //    resolved commit SHA (the `#<sha>` fragment — slice 3).
    let lock = std::fs::read_to_string(proj.join("kara.lock")).unwrap_or_default();
    assert!(
        lock.contains("git_dep") && lock.contains("git+"),
        "kara.lock should pin the fetched git dep;\nlock={lock}",
    );
    assert!(
        lock.contains(&format!("#{head_sha}")),
        "kara.lock should pin the resolved commit `{head_sha}` as a #<sha> fragment;\nlock={lock}",
    );

    let _ = std::fs::remove_dir_all(&proj);
    let _ = std::fs::remove_dir_all(&cache);
    let _ = std::fs::remove_dir_all(&upstream);
}

#[test]
fn build_pins_a_git_dependency_to_a_tag() {
    if !git_available() {
        eprintln!("skipping: git not available");
        return;
    }
    let upstream = make_upstream("tagged_dep");
    // Tag the good commit, then push a breaking change past the tag.
    git(&["tag", "v1.0.0"], &upstream);
    write(
        &upstream.join("src/lib.kara"),
        b"pub fn answer() -> i64 { syntax error here }\n",
    );
    git(&["add", "-A"], &upstream);
    git(&["commit", "--quiet", "-m", "break"], &upstream);

    let cache = unique("cache-tag");
    let proj = unique("proj-tag");
    write(
        &proj.join("kara.toml"),
        format!(
            "[package]\nname = \"app\"\n\n[dependencies]\ntagged_dep = {{ git = \"{}\", tag = \"v1.0.0\" }}\n",
            upstream.display()
        )
        .as_bytes(),
    );
    write(
        &proj.join("src/main.kara"),
        b"import tagged_dep.answer;\n\nfn main() {\n    let _ = answer();\n}\n",
    );

    let out = karac()
        .arg("build")
        .env("KARAC_GIT_CACHE_ROOT", &cache)
        .current_dir(&proj)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);

    // The tagged commit is clean, so the build resolves past the dep even
    // though a later commit on the branch is broken — proving the checkout
    // honored the tag, not the branch tip.
    assert!(
        !stderr.contains("E_GIT_FETCH_FAILED") && !stderr.contains("error["),
        "tag-pinned build should compile the good commit;\nstderr={stderr}",
    );

    let _ = std::fs::remove_dir_all(&proj);
    let _ = std::fs::remove_dir_all(&cache);
    let _ = std::fs::remove_dir_all(&upstream);
}

#[test]
fn build_reports_a_bad_git_url() {
    if !git_available() {
        eprintln!("skipping: git not available");
        return;
    }
    let cache = unique("cache-bad");
    let bogus = unique("nowhere").join("no-such-repo");

    let proj = unique("proj-bad");
    write(
        &proj.join("kara.toml"),
        format!(
            "[package]\nname = \"app\"\n\n[dependencies]\nghost = {{ git = \"{}\" }}\n",
            bogus.display()
        )
        .as_bytes(),
    );
    write(&proj.join("src/main.kara"), b"fn main() {}\n");

    let out = karac()
        .arg("build")
        .env("KARAC_GIT_CACHE_ROOT", &cache)
        .current_dir(&proj)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        stderr.contains("E_GIT_FETCH_FAILED"),
        "an unreachable git URL must surface the fetch-failed diagnostic;\nstderr={stderr}",
    );
    assert!(
        !proj.join("kara.lock").exists(),
        "a failed clone must not persist a lockfile",
    );

    let _ = std::fs::remove_dir_all(&proj);
    let _ = std::fs::remove_dir_all(&cache);
}
