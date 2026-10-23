//! `karac build` on v2 (M3 item 4, CLI half, B-2026-10-22-14): a single file,
//! a package built from one of its files or from its directory, `install`,
//! the flags v2 refuses by name, and `--legacy` / `KARAC_LEGACY=1`.
//!
//! Without the llvm feature there is no code generator, so the build is the
//! check alone: each test asserts the executable only where one is made.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const LLVM: bool = cfg!(feature = "llvm");

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-build-v2-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn package(tag: &str, util: &str) -> PathBuf {
    let dir = scratch(tag);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("kara.toml"),
        "[package]\nname = \"pkg\"\nversion = \"0.1.0\"\nauthors = []\nedition = \"2026\"\n\n[dependencies]\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/main.kara"),
        "import util.{double};\n\nfn main() {\n    println(f\"{double(21)}\");\n}\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/util.kara"), util).unwrap();
    dir
}

const UTIL: &str = "pub fn double(x: i64) -> i64 {\n    x * 2\n}\n";

fn karac(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .env_remove("KARAC_LEGACY")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// The executable's stdout, after asserting it ran cleanly.
fn ran(exe: &Path) -> String {
    let out = Command::new(exe).output().unwrap();
    assert!(out.status.success(), "{}", text(&out.stderr));
    text(&out.stdout)
}

/// A build that succeeded: `Built: <exe>` with the llvm feature, the
/// check-only note without it.
fn built(out: &Output) {
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    if LLVM {
        assert!(
            text(&out.stdout).starts_with("Built: "),
            "{}",
            text(&out.stdout)
        );
    } else {
        assert!(
            text(&out.stderr).contains("karac build requires the llvm feature"),
            "{}",
            text(&out.stderr)
        );
    }
}

#[test]
fn build_v2_builds_a_file() {
    let dir = scratch("file");
    std::fs::write(
        dir.join("hello.kara"),
        "fn main() {\n    let x = 9223372036854775807;\n    println(f\"hi {x}\");\n}\n",
    )
    .unwrap();
    let out = karac(&dir, &["build", "hello.kara"]);
    built(&out);
    let named = karac(
        &dir,
        &["build", "hello.kara", "-o", "other", "--output=json"],
    );
    assert_eq!(named.status.code(), Some(0), "{}", text(&named.stderr));
    if LLVM {
        assert_eq!(ran(&dir.join("hello")), "hi 9223372036854775807\n");
        assert_eq!(
            text(&named.stdout),
            "{\"status\":\"ok\",\"output\":\"other\"}\n"
        );
        assert_eq!(ran(&dir.join("other")), "hi 9223372036854775807\n");
    }
}

/// A package builds from its directory into `<root>/<name>`, and from one of
/// its files into that file's stem; an error names the file it is in.
#[test]
fn build_v2_builds_a_package() {
    let dir = package("pkg", UTIL);
    let out = karac(&dir, &["build"]);
    built(&out);
    assert!(
        !text(&out.stdout).contains("project: pkg"),
        "{}",
        text(&out.stdout)
    );
    let member = karac(&dir, &["build", "src/main.kara", "-o", "m"]);
    built(&member);
    if LLVM {
        assert_eq!(ran(&dir.join("pkg")), "42\n");
        assert_eq!(ran(&dir.join("m")), "42\n");
    }
    let bad = package(
        "pkg-bad",
        &format!("{UTIL}\npub fn keep(v: Vec[i64]) -> Vec[i64] {{\n    v\n}}\n"),
    );
    let out = karac(&bad, &["build"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        // The package's own path for the file: `src\util.kara` on Windows.
        text(&out.stderr)
            .replace('\\', "/")
            .contains("src/util.kara:6:5:"),
        "{}",
        text(&out.stderr)
    );
    let json = karac(&bad, &["build", "--output=json"]);
    assert!(
        text(&json.stdout)
            .replace("\\\\", "/")
            .contains("\"file\":\"src/util.kara\",\"line\":6"),
        "{}",
        text(&json.stdout)
    );
}

/// `karac install path=<dir>` builds the package on v2 and copies it.
#[test]
fn build_v2_install_builds_on_v2() {
    let dir = package("install", UTIL);
    let root = scratch("install-root");
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .env_remove("KARAC_LEGACY")
        .env("KARAC_INSTALL_ROOT", &root)
        .current_dir(&dir)
        .args(["install", "path=."])
        .output()
        .unwrap();
    if LLVM {
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
        assert!(
            !text(&out.stdout).contains("project: pkg"),
            "{}",
            text(&out.stdout)
        );
        assert_eq!(ran(&root.join("pkg")), "42\n");
    }
}

/// A flag v2 does not honour yet is refused by name, pointing at `--legacy`.
#[test]
fn build_v2_refuses_legacy_only_flags_by_name() {
    let dir = scratch("flags");
    std::fs::write(dir.join("a.kara"), "fn main() {\n    println(\"a\");\n}\n").unwrap();
    let out = karac(
        &dir,
        &["build", "a.kara", "--simd-report", "--target=wasm_browser"],
    );
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        text(&out.stderr),
        "error: the v2 build does not honour `--simd-report` yet; \
         `karac build --legacy` does until M3 deletes it\n\
         error: `--target=wasm_browser` builds the browser wasm bindings, which goes to the \
         archive with the legacy backend at M3 and returns on v2 on demand; \
         `karac build --legacy` builds it until then\n"
    );
    let json = karac(
        &dir,
        &["build", "a.kara", "--crate-type=staticlib", "--output=json"],
    );
    assert_eq!(json.status.code(), Some(1));
    assert!(
        text(&json.stdout).starts_with("{\"severity\":\"error\",\"phase\":\"build\",\"message\":\"error: `--crate-type` builds the exported C ABI, which goes to the archive"),
        "{}",
        text(&json.stdout)
    );
    // The host's own name for itself is not a cross target.
    let native = karac(&dir, &["build", "a.kara", "--target=native"]);
    built(&native);
}

/// Auto-parallelisation goes to the archive with the legacy backend, so the
/// v2 build says it ignores `KARAC_AUTO_PAR=1` rather than building a
/// sequential binary in silence (B-2026-10-22-21).
#[test]
fn build_v2_says_it_ignores_auto_par() {
    let dir = scratch("autopar");
    std::fs::write(dir.join("a.kara"), "fn main() {\n    println(\"a\");\n}\n").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .env_remove("KARAC_LEGACY")
        .env("KARAC_AUTO_PAR", "1")
        .current_dir(&dir)
        .args(["build", "a.kara"])
        .output()
        .unwrap();
    built(&out);
    assert!(
        text(&out.stderr).contains(
            "warning: `KARAC_AUTO_PAR=1` is ignored: auto-parallelisation goes to the archive"
        ),
        "{}",
        text(&out.stderr)
    );
}

/// `--legacy` and `KARAC_LEGACY=1` reach the legacy build, which prints its
/// project banner and takes the flags v2 refuses.
#[test]
fn build_v2_legacy_flag_and_env_reach_legacy() {
    let dir = package("legacy", UTIL);
    let flag = karac(&dir, &["build", "--legacy"]);
    let env = Command::new(env!("CARGO_BIN_EXE_karac"))
        .env("KARAC_LEGACY", "1")
        .current_dir(&dir)
        .arg("build")
        .output()
        .unwrap();
    for out in [flag, env] {
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
        assert!(
            text(&out.stdout).contains("project: pkg"),
            "{}",
            text(&out.stdout)
        );
    }
    let solo = scratch("legacy-solo");
    std::fs::write(solo.join("a.kara"), "fn main() {\n    println(\"a\");\n}\n").unwrap();
    let report = karac(
        &solo,
        &["build", "--legacy", "a.kara", "--concurrency-report"],
    );
    assert_eq!(report.status.code(), Some(0), "{}", text(&report.stderr));
}

/// A package's dependencies are resolved as the legacy build resolves them
/// (kara.lock written), and their modules are part of the program.
#[test]
fn build_v2_builds_a_package_with_a_path_dependency() {
    let dir = scratch("deps");
    let manifest = |name: &str, deps: &str| {
        format!(
            "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nauthors = []\nedition = \"2026\"\n\n[dependencies]\n{deps}"
        )
    };
    std::fs::create_dir_all(dir.join("dep/src")).unwrap();
    std::fs::create_dir_all(dir.join("app/src")).unwrap();
    std::fs::write(dir.join("dep/kara.toml"), manifest("dep", "")).unwrap();
    std::fs::write(
        dir.join("dep/src/lib.kara"),
        "pub fn answer() -> i64 {\n    42\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("app/kara.toml"),
        manifest("app", "dep = { path = \"../dep\" }\n"),
    )
    .unwrap();
    std::fs::write(
        dir.join("app/src/main.kara"),
        "import dep.answer;\n\nfn main() {\n    println(f\"{answer()}\");\n}\n",
    )
    .unwrap();
    let app = dir.join("app");
    let out = karac(&app, &["build"]);
    built(&out);
    assert!(app.join("kara.lock").is_file());
    if LLVM {
        assert_eq!(ran(&app.join("app")), "42\n");
    }
}

/// `--release` strips `dbg()`'s printing (its value stays) and the contracts.
#[test]
fn build_v2_release_strips_dbg_and_contracts() {
    let dir = scratch("release");
    std::fs::write(
        dir.join("r.kara"),
        "fn half(x: i64) -> i64\n    requires x > 0\n{\n    x / 2\n}\n\n\
         fn main() {\n    let x = dbg(1 + 2);\n    println(f\"{x} {half(-4)}\");\n}\n",
    )
    .unwrap();
    let debug = karac(&dir, &["build", "r.kara", "-o", "debug"]);
    built(&debug);
    let release = karac(&dir, &["build", "--release", "r.kara", "-o", "release"]);
    built(&release);
    if LLVM {
        let out = Command::new(dir.join("debug")).output().unwrap();
        assert_eq!(out.status.code(), Some(101), "{}", text(&out.stderr));
        assert!(
            text(&out.stderr).contains("[r.kara:8] 1 + 2 = 3"),
            "{}",
            text(&out.stderr)
        );
        let out = Command::new(dir.join("release")).output().unwrap();
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
        assert_eq!(text(&out.stdout), "3 -2\n");
        assert_eq!(text(&out.stderr), "");
    }
}
