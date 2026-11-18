//! `karac vendor` and `karac update` fetch a git dependency as `karac build`
//! does, and `karac build --offline` reads it back from `vendor/` (design.md
//! § Vendoring). Both commands used to resolve with no registry or git
//! provider: the dependency was only a warning, `vendor` copied 0 packages
//! and `update` wrote a lock without it, both exiting 0. Each test skips when
//! `git` is unavailable.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-vendor-git-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(path: &Path, contents: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

fn git(args: &[&str], cwd: &Path) -> bool {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// A git repo holding package `dep` (`pub fn f() -> i64 { 41 }`) beside a
/// project `app` that depends on it by `file://` URL; `None` without git.
fn project(root: &Path) -> Option<PathBuf> {
    let repo = root.join("dep");
    write(
        &repo.join("kara.toml"),
        "[package]\nname = \"dep\"\nversion = \"0.1.0\"\n",
    );
    write(&repo.join("src/lib.kara"), "pub fn f() -> i64 { 41 }\n");
    let ok = git(&["init", "--quiet", "-b", "main"], &repo)
        && git(&["config", "user.email", "t@t.test"], &repo)
        && git(&["config", "user.name", "Test"], &repo)
        && git(&["add", "-A"], &repo)
        && git(&["commit", "--quiet", "-m", "init"], &repo);
    if !ok {
        return None;
    }
    let url = format!("file://{}", repo.display()).replace('\\', "/");
    let app = root.join("app");
    write(
        &app.join("kara.toml"),
        &format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\n\
             dep = {{ git = \"{url}\" }}\n"
        ),
    );
    write(
        &app.join("src/main.kara"),
        "import dep.f;\nfn main() { println(f() + 1); }\n",
    );
    Some(app)
}

fn karac(args: &[&str], cwd: &Path, cache: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(cwd)
        .env("KARAC_GIT_CACHE_ROOT", cache)
        .env_remove("KARAC_REGISTRY_PROXY")
        .env_remove("KARAC_REGISTRY_URL")
        .output()
        .unwrap()
}

fn text(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned() + &String::from_utf8_lossy(&out.stdout)
}

#[test]
fn update_locks_a_git_dependency_at_its_commit() {
    let root = scratch("update");
    let Some(app) = project(&root) else {
        eprintln!("skipping: git not available");
        return;
    };
    let out = karac(&["update"], &app, &root.join("cache"));
    let lock = std::fs::read_to_string(app.join("kara.lock")).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&root);
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        !text(&out).contains("E_GIT_DEP_UNSUPPORTED"),
        "{}",
        text(&out)
    );
    assert!(
        lock.contains("name = \"dep\"") && lock.contains("source = \"git+file://"),
        "{lock}"
    );
    assert!(lock.contains('#'), "the lock pins the commit: {lock}");
}

#[test]
fn vendor_copies_a_git_dependency_and_offline_build_reads_it() {
    let root = scratch("vendor");
    let Some(app) = project(&root) else {
        eprintln!("skipping: git not available");
        return;
    };
    let cache = root.join("cache");
    let update = karac(&["update"], &app, &cache);
    let vendor = karac(&["vendor"], &app, &cache);
    let vendored = app.join("vendor/dep");
    let copied = vendored.join("src/lib.kara").is_file();
    let no_repo = !vendored.join(".git").exists();

    // With the upstream repo and the clone cache gone, only `vendor/` holds
    // the dependency.
    std::fs::remove_dir_all(root.join("dep")).unwrap();
    let _ = std::fs::remove_dir_all(&cache);
    let lock_before = std::fs::read_to_string(app.join("kara.lock")).unwrap_or_default();
    let resolve = karac(&["resolve", "--offline"], &app, &cache);
    // The build needs the backend; the default leg proves the offline
    // resolution alone.
    #[cfg(feature = "llvm")]
    let built = {
        let build = karac(&["build", "--offline"], &app, &cache);
        let ran = Command::new(app.join("dist/native/app")).output();
        (build, ran)
    };
    let lock_after = std::fs::read_to_string(app.join("kara.lock")).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&root);

    assert!(update.status.success(), "{}", text(&update));
    assert!(vendor.status.success(), "{}", text(&vendor));
    assert!(
        text(&vendor).contains("copied 1 package"),
        "{}",
        text(&vendor)
    );
    assert!(copied, "vendor/dep holds the package's source");
    assert!(no_repo, "vendor/dep carries no .git");
    assert!(resolve.status.success(), "{}", text(&resolve));
    assert!(text(&resolve).contains("vendor"), "{}", text(&resolve));
    #[cfg(feature = "llvm")]
    {
        let (build, ran) = built;
        assert!(build.status.success(), "{}", text(&build));
        let ran = ran.expect("the offline build's binary runs");
        assert_eq!(String::from_utf8_lossy(&ran.stdout).trim(), "42");
    }
    assert!(lock_before.contains("git+file://"), "{lock_before}");
    assert_eq!(
        lock_before, lock_after,
        "offline, kara.lock keeps the git pin"
    );
}
