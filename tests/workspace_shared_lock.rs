//! Workspace members share one `kara.lock` and one output directory
//! (design.md § Workspaces, B-2026-11-13-60). The lock sits at the workspace
//! root and is solved once over every member's requirements, so two members
//! that name one registry package lock one version of it; a member's build
//! goes in the root's `dist/`. Before, each member resolved alone and wrote
//! `<member>/kara.lock` and `<member>/dist/`, so a member allowing `1.0`
//! locked the newest `1.5.0` while its sibling capped at `<1.2` locked
//! `1.0.0`.

use kara_registry_proxy::{serve, FsStore};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;

const LLVM: bool = cfg!(feature = "llvm");

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-ws-lock-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(path: &Path, contents: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

fn karac(args: &[&str], cwd: &Path, envs: &[(&str, &Path)]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_karac"));
    cmd.args(args).current_dir(cwd);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.output().unwrap()
}

fn text(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned() + &String::from_utf8_lossy(&out.stdout)
}

fn targz(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut builder = tar::Builder::new(gz);
    for (path, contents) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, path, *contents).unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap()
}

/// A registry store with `demo_dep` 1.0.0 and 1.5.0, whose `answer()`
/// returns 10 and 15, served on a loopback port. Returns the base URL.
fn registry(dir: &Path) -> String {
    write(
        &dir.join("catalog/demo_dep.json"),
        br#"{ "upstream": "https://example.test/demo_dep", "versions": ["1.0.0", "1.5.0"] }"#,
    );
    for (version, answer) in [("1.0.0", 10), ("1.5.0", 15)] {
        let toml = format!("[package]\nname = \"demo_dep\"\nversion = \"{version}\"\n");
        let lib = format!("pub fn answer() -> i64 {{ {answer} }}\n");
        write(
            &dir.join(format!("pkg/demo_dep/{version}.tar.gz")),
            &targz(&[
                ("kara.toml", toml.as_bytes()),
                ("src/lib.kara", lib.as_bytes()),
            ]),
        );
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let store = dir.to_path_buf();
    std::thread::spawn(move || serve(listener, Arc::new(FsStore::new(store))));
    format!("http://{addr}")
}

/// A binary package `name` that prints `demo_dep.answer()`, depending on
/// `demo_dep = "<req>"`.
fn member(dir: &Path, name: &str, req: &str) {
    write(
        &dir.join("kara.toml"),
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n\n[dependencies]\ndemo_dep = \"{req}\"\n")
            .as_bytes(),
    );
    write(
        &dir.join("src/main.kara"),
        b"import demo_dep.answer;\n\nfn main() {\n    println(f\"{answer()}\");\n}\n",
    );
}

fn locked_versions(lock: &Path) -> Vec<String> {
    let lock = std::fs::read_to_string(lock).unwrap_or_default();
    let mut versions = Vec::new();
    let mut in_demo = false;
    for line in lock.lines() {
        if line.starts_with("name = ") {
            in_demo = line == "name = \"demo_dep\"";
        } else if in_demo && line.starts_with("version = ") {
            versions.push(
                line.trim_start_matches("version = ")
                    .trim_matches('"')
                    .to_string(),
            );
        }
    }
    versions
}

#[test]
fn workspace_members_lock_one_version_at_the_root() {
    let base = scratch("versions");
    let url = registry(&base.join("store"));
    let proxy = PathBuf::from(&url);
    let cache = base.join("cache");
    let envs = [
        ("KARAC_REGISTRY_PROXY", proxy.as_path()),
        ("KARAC_REGISTRY_CACHE_ROOT", cache.as_path()),
    ];

    // Control: `wide` alone takes the newest version its `1.0` allows.
    let alone = base.join("alone");
    member(&alone, "wide", "1.0");
    let solo = karac(&["build"], &alone, &envs);
    assert!(solo.status.success(), "{}", text(&solo));
    assert_eq!(locked_versions(&alone.join("kara.lock")), ["1.5.0"]);

    // The same package as a member beside `narrow`, which caps at `<1.2`.
    let ws = base.join("ws");
    write(
        &ws.join("kara.toml"),
        b"[workspace]\nmembers = [\"wide\", \"narrow\"]\n",
    );
    member(&ws.join("wide"), "wide", "1.0");
    member(&ws.join("narrow"), "narrow", ">=1.0, <1.2");
    let built = karac(&["build"], &ws.join("wide"), &envs);
    let lock_after_wide = locked_versions(&ws.join("kara.lock"));
    let at_root = karac(&["build"], &ws, &envs);
    let wide_lock = ws.join("wide/kara.lock").exists();
    let wide_dist = ws.join("wide/dist").exists();
    let exe = ws.join("dist/native/wide");
    let ran = LLVM.then(|| Command::new(&exe).output().unwrap());
    let _ = std::fs::remove_dir_all(&base);

    assert!(built.status.success(), "{}", text(&built));
    assert!(at_root.status.success(), "{}", text(&at_root));
    assert_eq!(lock_after_wide, ["1.0.0"], "one solve over both members");
    assert!(!wide_lock, "a member writes no kara.lock of its own");
    assert!(!wide_dist, "a member's output goes in the root's dist/");
    if let Some(ran) = ran {
        assert_eq!(String::from_utf8_lossy(&ran.stdout), "10\n");
    }
}

#[test]
fn a_path_only_member_builds_into_the_root_dist() {
    let ws = scratch("dist");
    write(
        &ws.join("kara.toml"),
        b"[workspace]\nmembers = [\"app\", \"util\"]\n",
    );
    write(
        &ws.join("app/kara.toml"),
        b"[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nutil = { path = \"../util\" }\n",
    );
    write(
        &ws.join("app/src/main.kara"),
        b"import util.hello;\nfn main() { println(hello()); }\n",
    );
    write(
        &ws.join("util/kara.toml"),
        b"[package]\nname = \"util\"\nversion = \"0.1.0\"\n",
    );
    write(
        &ws.join("util/src/lib.kara"),
        b"pub fn hello() -> String { \"from util\" }\n",
    );
    let built = karac(&["build"], &ws.join("app"), &[]);
    let root_lock = std::fs::read_to_string(ws.join("kara.lock")).unwrap_or_default();
    let member_lock = ws.join("app/kara.lock").exists();
    let member_dist = ws.join("app/dist").exists();
    let exe = ws.join("dist/native/app").exists();
    let cleaned = karac(&["clean"], &ws.join("app"), &[]);
    let dist_after_clean = ws.join("dist").exists();
    let _ = std::fs::remove_dir_all(&ws);

    assert!(built.status.success(), "{}", text(&built));
    assert!(
        root_lock.contains("name = \"app\"") && root_lock.contains("name = \"util\""),
        "the root's kara.lock covers the members;\n{root_lock}"
    );
    assert!(!member_lock, "a member writes no kara.lock of its own");
    assert!(!member_dist, "a member's output goes in the root's dist/");
    assert_eq!(exe, LLVM, "the executable is at dist/native/app");
    assert!(cleaned.status.success(), "{}", text(&cleaned));
    assert!(
        !dist_after_clean,
        "`karac clean` in a member removes the shared dist/"
    );
}

#[test]
fn a_member_conflict_is_reported_from_either_member() {
    let base = scratch("conflict");
    let url = registry(&base.join("store"));
    let proxy = PathBuf::from(&url);
    let cache = base.join("cache");
    let envs = [
        ("KARAC_REGISTRY_PROXY", proxy.as_path()),
        ("KARAC_REGISTRY_CACHE_ROOT", cache.as_path()),
    ];
    let ws = base.join("ws");
    write(
        &ws.join("kara.toml"),
        b"[workspace]\nmembers = [\"old\", \"new\"]\n",
    );
    member(&ws.join("old"), "old", "=1.0.0");
    member(&ws.join("new"), "new", "=1.5.0");
    let out = karac(&["build"], &ws.join("old"), &envs);
    let _ = std::fs::remove_dir_all(&base);
    assert!(!out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("demo_dep"), "{}", text(&out));
}
