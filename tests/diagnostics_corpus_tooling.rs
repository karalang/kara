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

const CATALOG_1_2_0: &str = r#"{ "upstream": "https://example.test/http", "versions": ["1.2.0"] }"#;
const CATALOG_1_2_0_YANKED: &str =
    r#"{ "upstream": "https://example.test/http", "versions": ["1.2.0"], "yanked": ["1.2.0"] }"#;

/// A registry proxy on a local socket: `/catalog/...` answers `catalog`,
/// anything else (`/pkg/...`) answers `tarball`, each as (status, body).
fn serve(catalog: (u16, &'static str), tarball: (u16, &'static str)) -> String {
    use std::io::{BufRead, BufReader, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            let _ = reader.read_line(&mut request);
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
            }
            let path = request.split_whitespace().nth(1).unwrap_or("");
            let (status, body) = if path.starts_with("/catalog/") {
                catalog
            } else {
                tarball
            };
            let _ = write!(
                stream,
                "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    format!("http://{addr}")
}

fn reported(out: &str, code: &str) -> bool {
    out.contains(&format!("\"code\":\"{code}\""))
        || out.contains(&format!("\"parent_code\":\"{code}\""))
        || out.contains(&format!("[{code}]"))
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
    // No git cache root to clone into.
    expect(
        "E_GIT_DEP_UNSUPPORTED",
        karac(
            &d,
            &check,
            &[
                ("HOME", None),
                ("USERPROFILE", None),
                ("KARAC_GIT_CACHE_ROOT", None),
            ],
        ),
    );

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

    // A lint forbidden on the command line, allowed in the source.
    let d = root.join("forbid");
    package(&d, "app", "", true);
    std::fs::write(
        d.join("src/main.kara"),
        "#[allow(unused_mut)]\nfn helper() -> i64 {\n    let mut x = 1;\n    x\n}\n\nfn main() {\n    println(helper());\n}\n",
    )
    .unwrap();
    let out = karac(&d, &["check", "-F", "unused_mut", "--output=json"], &[]);
    // E0247 is the JSON `parent_code` of the symbolic code.
    expect("E_FORBIDDEN_LINT_ALLOW", out.clone());
    expect("E0247", out);

    // `#[non_exhaustive]` types used from another package.
    let d = root.join("non_exhaustive");
    package(&d.join("lib"), "lib", "", false);
    std::fs::write(
        d.join("lib/src/lib.kara"),
        "#[non_exhaustive]\npub enum Kind { A, B }\npub fn get() -> Kind { Kind.A }\n\
         #[non_exhaustive]\npub struct Opts { pub port: i64 }\npub fn mk() -> Opts { Opts { port: 3 } }\n",
    )
    .unwrap();
    package(
        &d.join("app"),
        "app",
        "\n[dependencies]\nlib = { path = \"../lib\" }\n",
        true,
    );
    std::fs::write(
        d.join("app/src/main.kara"),
        "import lib.{Kind, Opts, get, mk};\nfn main() {\n    \
         match get() { Kind.A => println(\"a\"), Kind.B => println(\"b\") }\n    \
         let o = Opts { port: 1 };\n    let Opts { port } = mk();\n    println(f\"{o.port} {port}\");\n}\n",
    )
    .unwrap();
    let out = karac(&d.join("app"), &check, &[]);
    expect("E_NON_EXHAUSTIVE_CROSS_PACKAGE_MATCH", out.clone());
    expect("E_NON_EXHAUSTIVE_CROSS_PACKAGE_LITERAL", out.clone());
    // The match's and the literal's JSON `code` fields are E0241 / E0243.
    expect("E0241", out.clone());
    expect("E0243", out.clone());
    expect("E_NON_EXHAUSTIVE_CROSS_PACKAGE_PATTERN", out);

    // An orphan impl: `mid` implements `lib`'s trait for a std type, and
    // owns neither (tests/cross_package_coherence.rs has the full matrix).
    let d = root.join("orphan");
    package(&d.join("lib"), "lib", "", false);
    std::fs::write(
        d.join("lib/src/lib.kara"),
        "pub trait Tw { fn tw(self) -> i64; }\npub struct S { pub n: i64 }\n",
    )
    .unwrap();
    package(
        &d.join("mid"),
        "mid",
        "\n[dependencies]\nlib = { path = \"../lib\" }\n",
        false,
    );
    std::fs::write(
        d.join("mid/src/lib.kara"),
        "import lib.Tw;\nimpl Tw for String { fn tw(self) -> i64 { self.len() } }\n",
    )
    .unwrap();
    package(
        &d.join("app"),
        "app",
        "\n[dependencies]\nlib = { path = \"../lib\" }\nmid = { path = \"../mid\" }\n",
        true,
    );
    std::fs::write(d.join("app/src/main.kara"), "fn main() { println(1); }\n").unwrap();
    expect("E0246", karac(&d.join("app"), &check, &[]));

    // The ratchet's sweep (tests/diagnostics_coverage.rs): the rest of
    // `karac install`, the lockfile, the offline vendor tree, git and path
    // dependency graphs, and the build cache.
    let d = root.join("install");
    package(&d.join("app"), "app", "", true);
    package(&d.join("bad"), "bad", "", true);
    std::fs::write(
        d.join("bad/src/main.kara"),
        "fn main() {\n    let x: i64 = \"s\";\n}\n",
    )
    .unwrap();
    std::fs::write(d.join("root_is_a_file"), "").unwrap();
    let file_root = d.join("root_is_a_file");
    expect(
        "E_INSTALL_BIN_DIR_UNWRITABLE",
        karac(
            &d,
            &["install", "path=app"],
            &[("KARAC_INSTALL_ROOT", file_root.to_str())],
        ),
    );
    let bin_root = d.join("bin");
    expect(
        "E_INSTALL_BUILD_FAILED",
        karac(
            &d,
            &["install", "path=bad"],
            &[("KARAC_INSTALL_ROOT", bin_root.to_str())],
        ),
    );

    let d = root.join("lockfile");
    package(
        &d.join("app"),
        "app",
        "[dependencies]\nlib = { path = \"../lib\" }\n",
        true,
    );
    package(&d.join("lib"), "lib", "", false);
    for (code, lock) in [
        ("E_LOCKFILE_SCHEMA_VERSION", "version = 99\n"),
        ("E_LOCKFILE_INVALID_FIELD", "version = \"one\"\n"),
        (
            "E_LOCKFILE_INVALID_SOURCE",
            "version = 1\n\n[[package]]\nname = \"lib\"\nversion = \"0.1.0\"\n\
             source = \"bogus+x\"\ncontent_hash = \"blake3:00\"\ndependencies = []\n",
        ),
    ] {
        std::fs::write(d.join("app/kara.lock"), lock).unwrap();
        expect(code, karac(&d.join("app"), &check, &[]));
    }
    std::fs::remove_file(d.join("app/kara.lock")).unwrap();
    // An empty vendor tree: present, but without `lib`.
    std::fs::create_dir_all(d.join("app/vendor")).unwrap();
    expect(
        "E_OFFLINE_VENDOR_ENTRY_MISSING",
        karac(&d.join("app"), &["build", "--offline"], &[]),
    );

    let d = root.join("graphs");
    package(&d.join("lib"), "lib", "", false);
    package(&d.join("lib2"), "lib", "", false);
    package(
        &d.join("mid"),
        "mid",
        "[dependencies]\nlib = { path = \"../lib2\" }\n",
        false,
    );
    package(
        &d.join("conflict"),
        "conflict",
        "[dependencies]\nlib = { path = \"../lib\" }\nmid = { path = \"../mid\" }\n",
        true,
    );
    expect(
        "E_DEPENDENCY_SOURCE_CONFLICT",
        karac(&d.join("conflict"), &check, &[]),
    );
    package(
        &d.join("newer"),
        "newer",
        "[dependencies]\nlib = { path = \"../lib\", version = \"2.0\" }\n",
        true,
    );
    expect(
        "E_DEPENDENCY_VERSION_CONFLICT",
        karac(&d.join("newer"), &check, &[]),
    );

    // A git dependency whose repository has no `kara.toml`, cloned from a
    // local repository so no network is needed.
    let d = root.join("git_no_manifest");
    let repo = d.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join("README"), "not a package\n").unwrap();
    let git = |args: &[&str]| {
        Command::new("git")
            .current_dir(&repo)
            .args(["-c", "user.email=t@t", "-c", "user.name=t"])
            .args(args)
            .output()
            .is_ok_and(|o| o.status.success())
    };
    if git(&["init", "-q"]) && git(&["add", "."]) && git(&["commit", "-qm", "init"]) {
        package(
            &d.join("app"),
            "app",
            &format!(
                "[dependencies]\nlib = {{ git = \"file://{}\" }}\n",
                repo.display()
            ),
            true,
        );
        let cache = d.join("cache");
        expect(
            "E_GIT_NO_MANIFEST",
            karac(
                &d.join("app"),
                &check,
                &[("KARAC_GIT_CACHE_ROOT", cache.to_str())],
            ),
        );
    } else {
        panic!("E_GIT_NO_MANIFEST: `git` could not build the test repository");
    }

    // A cache root that is a file cannot be listed.
    let cache_file = root.join("cache_is_a_file");
    std::fs::write(&cache_file, "").unwrap();
    expect(
        "E_CACHE_UNREADABLE",
        karac(
            &root,
            &["cache", "info"],
            &[("KARAC_BUILD_CACHE_ROOT", cache_file.to_str())],
        ),
    );

    // A toolchain pin that is not UTF-8 cannot be read.
    let d = root.join("toolchain_unreadable");
    package(&d, "tc", "", true);
    std::fs::write(d.join("karac-toolchain.toml"), [0xffu8, 0xfe]).unwrap();
    expect("E_TOOLCHAIN_FILE_READ", karac(&d, &check, &[]));

    // A registry proxy that answers wrongly, served from a local socket.
    let d = root.join("proxy");
    for (code, req, catalog, tarball) in [
        ("E_PROXY_UNAUTHORIZED", "1.2", (401, ""), (401, "")),
        ("E_PROXY_PACKAGE_NOT_FOUND", "1.2", (404, ""), (404, "")),
        (
            "E_PROXY_MALFORMED_RESPONSE",
            "1.2",
            (200, "not json"),
            (404, ""),
        ),
        (
            "E_REGISTRY_NO_MATCHING_VERSION",
            "2.0",
            (200, CATALOG_1_2_0),
            (404, ""),
        ),
        (
            "E_REGISTRY_ONLY_YANKED",
            "1.2",
            (200, CATALOG_1_2_0_YANKED),
            (404, ""),
        ),
        (
            "E_PROXY_VERSION_NOT_FOUND",
            "1.2",
            (200, CATALOG_1_2_0),
            (404, ""),
        ),
        (
            "E_REGISTRY_EXTRACT_FAILED",
            "1.2",
            (200, CATALOG_1_2_0),
            (200, "not a tarball"),
        ),
    ] {
        let dir = d.join(code.to_lowercase());
        package(
            &dir,
            "app",
            &format!("[dependencies]\nhttp = \"{req}\"\n"),
            true,
        );
        let url = serve(catalog, tarball);
        let cache = dir.join("cache");
        expect(
            code,
            karac(
                &dir,
                &check,
                &[
                    ("KARAC_REGISTRY_PROXY", Some(url.as_str())),
                    ("KARAC_REGISTRY_CACHE_ROOT", cache.to_str()),
                ],
            ),
        );
    }

    // A lint's deny form from the command line, on its warning pin; the
    // `#[deny]` form is tests/lint_item_attributes.rs (B-2026-11-18-48).
    let pins = Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus/diagnostics");
    for (code, lint, pin) in [
        ("E0278", "must_use", "w0278"),
        ("E0296", "unused_doc_comment", "w0296"),
        ("E0317", "undocumented_unsafe", "w0317"),
        ("E0318", "ffi_float_eq", "w0318"),
        ("E0319", "ambiguous_not_comparison", "w0319"),
    ] {
        expect(
            code,
            karac(
                &pins.join(pin),
                &["check", "-D", lint, "--output=json", "source.kara"],
                &[],
            ),
        );
    }

    // A void run must not pass.
    assert_eq!(seen, 67, "case count changed");
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

/// Review-thread ruling 2026-11-18: `karac check` holds to the toolchain pin
/// as `karac build` does, in package mode and for a single file, with the
/// same code and exit status.
#[test]
fn diagnostics_corpus_tooling_check_enforces_the_toolchain_pin() {
    let d = scratch("check_pin");
    package(&d, "tc", "", true);
    std::fs::write(d.join("karac-toolchain.toml"), "version = \"=99.0.0\"\n").unwrap();
    for args in [&["check"][..], &["check", "src/main.kara"], &["build"]] {
        let out = Command::new(env!("CARGO_BIN_EXE_karac"))
            .current_dir(&d)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .expect("karac");
        let text = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "{args:?}: {text}");
        assert!(
            text.contains("error[E_TOOLCHAIN_VERSION_MISMATCH]"),
            "{args:?}: {text}"
        );
    }
    std::fs::write(d.join("karac-toolchain.toml"), "version = \"*\"\n").unwrap();
    let out = karac(&d, &["check"], &[]);
    assert!(out.contains("All checks passed"), "{out}");
    let _ = std::fs::remove_dir_all(&d);
}
