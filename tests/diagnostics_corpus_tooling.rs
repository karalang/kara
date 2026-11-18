//! B-2026-11-16-303: the package, dependency, toolchain, install and cache
//! codes. They come from manifests, lockfiles and the command line rather
//! than from a program, so a one-file entry under `corpus/diagnostics/`
//! cannot hold them. Each case builds its packages in a scratch directory,
//! runs the command, and checks that the code is reported and that `karac
//! explain` describes it.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac_tool_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A package at `dir` named `name`, with `extra` appended to its manifest.
/// A library (`src/lib.kara`) unless `bin`.
fn package(dir: &Path, name: &str, extra: &str, bin: bool) {
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("kara.toml"),
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n{extra}"),
    )
    .unwrap();
    let (file, text) = if bin {
        ("main.kara", "fn main() {}\n")
    } else {
        ("lib.kara", "pub fn one() -> i64 { 1 }\n")
    };
    std::fs::write(dir.join("src").join(file), text).unwrap();
}

fn karac(dir: &Path, args: &[&str], env: &[(&str, Option<&str>)]) -> String {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_karac"));
    cmd.current_dir(dir).args(args).stdin(Stdio::null());
    cmd.env_remove("KARAC_REGISTRY_PROXY");
    for (k, v) in env {
        match v {
            Some(v) => cmd.env(k, v),
            None => cmd.env_remove(k),
        };
    }
    let out = cmd.output().expect("karac");
    String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr)
}

fn reported(out: &str, code: &str) -> bool {
    out.contains(&format!("\"code\":\"{code}\"")) || out.contains(&format!("[{code}]"))
}

#[test]
fn diagnostics_corpus_tooling_codes_are_reported_and_explained() {
    let root = scratch("all");
    let mut bad = Vec::new();
    let mut seen = 0;
    let mut expect = |code: &str, out: String| {
        seen += 1;
        if !reported(&out, code) {
            bad.push(format!("{code}: not reported:\n{out}"));
        }
        let explained = karac(&root, &["explain", code], &[]);
        if explained.contains("no class page yet") || !explained.contains(code) {
            bad.push(format!("{code}: no description:\n{explained}"));
        }
    };
    let check = ["check", "--output=json"];

    // Path dependencies and workspaces.
    let d = root.join("cycle");
    package(
        &d.join("a"),
        "a",
        "[dependencies]\nb = { path = \"../b\" }\n",
        true,
    );
    package(
        &d.join("b"),
        "b",
        "[dependencies]\na = { path = \"../a\" }\n",
        false,
    );
    expect("E_DEPENDENCY_CYCLE", karac(&d.join("a"), &check, &[]));

    let d = root.join("missing");
    package(
        &d,
        "app",
        "[dependencies]\nlogging = { path = \"../nowhere\" }\n",
        true,
    );
    expect("E_PATH_DEP_NOT_FOUND", karac(&d, &check, &[]));

    let d = root.join("badmanifest");
    package(
        &d.join("app"),
        "app",
        "[dependencies]\nlib = { path = \"../lib\" }\n",
        true,
    );
    std::fs::create_dir_all(d.join("lib")).unwrap();
    std::fs::write(d.join("lib/kara.toml"), "not toml [\n").unwrap();
    expect(
        "E_PATH_DEP_MANIFEST_INVALID",
        karac(&d.join("app"), &check, &[]),
    );

    let d = root.join("ws");
    package(
        &d.join("m"),
        "m",
        "[dependencies]\nhttp = { workspace = true }\n",
        true,
    );
    std::fs::write(
        d.join("kara.toml"),
        "[workspace]\nmembers = [\"m\"]\n[workspace.dependencies]\njson = { path = \"../j\" }\n",
    )
    .unwrap();
    expect(
        "E_WORKSPACE_DEP_NOT_DECLARED",
        karac(&d.join("m"), &check, &[]),
    );

    let d = root.join("nows");
    package(
        &d,
        "app",
        "[dependencies]\nhttp = { workspace = true }\n",
        true,
    );
    expect("E_WORKSPACE_DEP_OUTSIDE_WORKSPACE", karac(&d, &check, &[]));

    let d = root.join("old");
    package(
        &d.join("app"),
        "app",
        "[dependencies]\nlib = { path = \"../lib\" }\n",
        true,
    );
    package(&d.join("lib"), "lib", "kara-version = \">=99.0\"\n", false);
    expect("E_TOOLCHAIN_TOO_OLD", karac(&d.join("app"), &check, &[]));

    // Registry and git dependencies, with nothing to fetch them from.
    let d = root.join("registry");
    package(&d, "app", "[dependencies]\nhttp = \"1.2\"\n", true);
    expect("E_REGISTRY_DEP_UNSUPPORTED", karac(&d, &check, &[]));
    let unreachable = karac(
        &d,
        &check,
        &[("KARAC_REGISTRY_PROXY", Some("http://127.0.0.1:9"))],
    );
    expect("E_PROXY_UNREACHABLE", unreachable.clone());
    expect("E_REGISTRY_FETCH_FAILED", unreachable);

    let d = root.join("git");
    package(
        &d,
        "app",
        "[dependencies]\nlib = { git = \"https://example.invalid/lib\" }\n",
        true,
    );
    let no_git = karac(&d, &check, &[("PATH", Some(""))]);
    expect("E_GIT_UNAVAILABLE", no_git.clone());
    expect("E_GIT_FETCH_FAILED", no_git);

    // A lockfile that does not parse is a warning, and its pins are dropped.
    let d = root.join("lock");
    package(
        &d.join("app"),
        "app",
        "[dependencies]\nlib = { path = \"../lib\" }\n",
        true,
    );
    package(&d.join("lib"), "lib", "", false);
    std::fs::write(d.join("app/kara.lock"), "not = [toml\n").unwrap();
    let out = karac(&d.join("app"), &check, &[]);
    assert!(out.contains("\"status\":\"ok\""), "{out}");
    expect("E_LOCKFILE_PARSE", out);

    // `karac-toolchain.toml`, read by build, run and test.
    let d = root.join("toolchain");
    package(&d, "tc", "", true);
    for (code, pin) in [
        ("E_TOOLCHAIN_INVALID_TOML", "version = \n"),
        ("E_TOOLCHAIN_MISSING_VERSION", "targets = []\n"),
        ("E_TOOLCHAIN_INVALID_VERSION_TYPE", "version = 1\n"),
        (
            "E_TOOLCHAIN_INVALID_VERSION_CONSTRAINT",
            "version = \"not a version\"\n",
        ),
        (
            "E_TOOLCHAIN_INVALID_TARGETS_TYPE",
            "version = \"*\"\ntargets = \"x\"\n",
        ),
        (
            "E_TOOLCHAIN_INVALID_TARGET_ENTRY",
            "version = \"*\"\ntargets = [1]\n",
        ),
        (
            "E_TOOLCHAIN_EMPTY_TARGET_ENTRY",
            "version = \"*\"\ntargets = [\"\"]\n",
        ),
        ("E_TOOLCHAIN_VERSION_MISMATCH", "version = \"=99.0.0\"\n"),
    ] {
        std::fs::write(d.join("karac-toolchain.toml"), pin).unwrap();
        expect(code, karac(&d, &["build", "--output=json"], &[]));
    }

    // Commands over the package.
    let d = root.join("cmds");
    package(
        &d.join("app"),
        "app",
        "[dependencies]\nlib = { path = \"../lib\" }\n",
        true,
    );
    package(&d.join("lib"), "lib", "", false);
    let app = d.join("app");
    expect(
        "E_OFFLINE_NO_VENDOR_DIR",
        karac(&app, &["build", "--offline"], &[]),
    );
    expect(
        "E_UPDATE_UNKNOWN_PACKAGE",
        karac(&app, &["update", "nosuch"], &[]),
    );
    expect(
        "E_UPDATE_ROOT_PACKAGE",
        karac(&app, &["update", "app"], &[]),
    );

    // `karac install` specs and sources.
    let manifest = app.join("kara.toml");
    let file_spec = format!("path={}", manifest.display());
    for (code, spec) in [
        ("E_INSTALL_EMPTY_SPEC", ""),
        ("E_INSTALL_MISSING_VALUE", "path="),
        ("E_INSTALL_EMPTY_VERSION", "tool@"),
        ("E_INSTALL_INVALID_VERSION", "tool@zz"),
        ("E_INSTALL_INVALID_NAME", "Tool"),
        ("E_INSTALL_RESERVED_NAME", "fn"),
        (
            "E_INSTALL_GIT_UNSUPPORTED",
            "git=https://example.invalid/tool",
        ),
        ("E_INSTALL_REGISTRY_UNSUPPORTED", "tool"),
        ("E_INSTALL_PATH_NOT_FOUND", "path=/nonexistent/karac/tool"),
        ("E_INSTALL_PATH_NOT_DIR", file_spec.as_str()),
    ] {
        expect(code, karac(&root, &["install", spec], &[]));
    }
    let lib_spec = format!("path={}", d.join("lib").display());
    expect(
        "E_INSTALL_HOME_UNSET",
        karac(
            &root,
            &["install", &lib_spec],
            &[
                ("HOME", None),
                ("USERPROFILE", None),
                ("KARAC_INSTALL_ROOT", None),
            ],
        ),
    );
    expect(
        "E_CACHE_HOME_UNSET",
        karac(
            &root,
            &["cache", "info"],
            &[
                ("HOME", None),
                ("USERPROFILE", None),
                ("KARAC_BUILD_CACHE_ROOT", None),
            ],
        ),
    );

    // A void run must not pass.
    assert_eq!(seen, 35, "case count changed");
    assert!(bad.is_empty(), "{}", bad.join("\n\n"));
    let _ = std::fs::remove_dir_all(&root);
}

/// The codes a test cannot provoke without a network, a full disk or a
/// damaged cache still have their own description.
#[test]
fn diagnostics_corpus_tooling_unprovoked_codes_are_explained() {
    let dir = std::env::temp_dir();
    for code in [
        "E_CACHE_UNREADABLE",
        "E_CACHE_MALFORMED_METADATA",
        "E_CACHE_WRITE_FAILED",
        "E_DEPENDENCY_SOURCE_CONFLICT",
        "E_DEPENDENCY_UNSATISFIABLE",
        "E_DEPENDENCY_VERSION_CONFLICT",
        "E_GIT_DEP_UNSUPPORTED",
        "E_GIT_NO_MANIFEST",
        "E_INSTALL_BIN_DIR_UNWRITABLE",
        "E_INSTALL_BUILD_FAILED",
        "E_INSTALL_BUILD_SPAWN_FAILED",
        "E_INSTALL_COPY_FAILED",
        "E_INSTALL_EXE_UNRESOLVABLE",
        "E_INSTALL_NO_EXECUTABLE",
        "E_LOCKFILE_IO",
        "E_LOCKFILE_SCHEMA_VERSION",
        "E_LOCKFILE_INVALID_SOURCE",
        "E_LOCKFILE_INVALID_FIELD",
        "E_OFFLINE_VENDOR_ENTRY_MISSING",
        "E_PROXY_DISABLED",
        "E_PROXY_MALFORMED_RESPONSE",
        "E_PROXY_NOT_IMPLEMENTED",
        "E_PROXY_PACKAGE_NOT_FOUND",
        "E_PROXY_UNAUTHORIZED",
        "E_PROXY_VERSION_NOT_FOUND",
        "E_REGISTRY_EXTRACT_FAILED",
        "E_REGISTRY_NO_MATCHING_VERSION",
        "E_REGISTRY_ONLY_YANKED",
        "E_TOOLCHAIN_FILE_READ",
        "E_VENDOR_COPY_FAILED",
    ] {
        let out = karac(&dir, &["explain", code], &[]);
        assert!(
            !out.contains("no class page yet") && out.contains(code),
            "{code}: {out}"
        );
    }
}
