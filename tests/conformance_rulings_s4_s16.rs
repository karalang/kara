//! The review thread's rulings on the design.md §4 and §16 conformance pass
//! (2026-11-16): `-O0`..`-O3` are build flags and `KARAC_OPT_LEVEL` outside
//! 0-3 is refused (B-2026-11-16-218), `[build] targets` names what a plain build builds
//! and what `karac run` may run (B-2026-11-16-219), `[package].version` is semver
//! (B-2026-11-16-220), and a text diagnostic names its catalogue code as JSON does
//! (B-2026-11-16-221).

use std::path::{Path, PathBuf};
use std::process::Command;

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-conformance-rulings-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn karac(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> (bool, String, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_karac"));
    cmd.args(args)
        .current_dir(dir)
        .env_remove("KARAC_OPT_LEVEL");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn package(dir: &Path, extra: &str, main: &str) {
    write(
        &dir.join("kara.toml"),
        &format!("[package]\nname = \"app\"\nversion = \"0.1.0\"\n{extra}"),
    );
    write(&dir.join("src/main.kara"), main);
}

const HELLO: &str = "fn main() { println(\"hi\"); }\n";

#[test]
fn an_optimization_level_outside_o0_to_o3_is_refused() {
    let dir = scratch("opt");
    write(&dir.join("hello.kara"), HELLO);
    let (ok, _, err) = karac(&dir, &["build", "-O4", "hello.kara"], &[]);
    assert!(
        !ok && err.contains("unknown optimization level '-O4'"),
        "{err}"
    );
    let (ok, _, err) = karac(&dir, &["build", "hello.kara"], &[("KARAC_OPT_LEVEL", "9")]);
    assert!(
        !ok && err.contains("KARAC_OPT_LEVEL=9: expected 0, 1, 2 or 3"),
        "{err}"
    );
    let (ok, _, err) = karac(
        &dir,
        &["run", "--interp", "hello.kara"],
        &[("KARAC_OPT_LEVEL", "s")],
    );
    assert!(
        !ok && err.contains("KARAC_OPT_LEVEL=s: expected 0, 1, 2 or 3"),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "llvm")]
#[test]
fn every_documented_optimization_level_builds() {
    let dir = scratch("opt-ok");
    write(&dir.join("hello.kara"), HELLO);
    for level in ["-O0", "-O1", "-O2", "-O3"] {
        let (_, out, err) = karac(&dir, &["build", level, "hello.kara"], &[]);
        // The link needs the runtime archive, which a checkout may lack; the
        // flag is settled before it.
        assert!(
            !err.contains("unknown optimization level"),
            "{level}: {out}{err}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_package_version_that_is_not_semver_is_refused() {
    let dir = scratch("semver");
    write(
        &dir.join("kara.toml"),
        "[package]\nname = \"app\"\nversion = \"one\"\n",
    );
    write(&dir.join("src/main.kara"), HELLO);
    let (ok, out, err) = karac(&dir, &["check"], &[]);
    let all = format!("{out}{err}");
    assert!(!ok && all.contains("semver version"), "{all}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn run_refuses_a_package_whose_targets_leave_out_native() {
    let dir = scratch("run-wasm-only");
    package(&dir, "\n[build]\ntargets = [\"wasm_wasi\"]\n", HELLO);
    let (ok, out, err) = karac(&dir, &["run", "--interp", "src/main.kara"], &[]);
    assert!(!ok, "{out}{err}");
    assert!(
        err.contains("declares `[build] targets = [\"wasm_wasi\"]`, which leave out `native`"),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "llvm")]
#[test]
fn a_plain_build_builds_each_declared_target() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/release");
    for archive in ["libkarac_runtime_min.a", "libkarac_runtime_wasm.a"] {
        if !root.join(archive).exists() {
            eprintln!("skipping: {archive} is missing (see CLAUDE.md's archive recipe)");
            return;
        }
    }
    let dir = scratch("build-targets");
    package(
        &dir,
        "\n[build]\ntargets = [\"native\", \"wasm_wasi\"]\n",
        HELLO,
    );
    let (ok, out, err) = karac(&dir, &["build"], &[]);
    assert!(ok, "{out}{err}");
    assert!(dir.join("dist/native/app").exists(), "{out}{err}");
    assert!(dir.join("dist/wasm/app.wasm").exists(), "{out}{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_text_diagnostic_names_its_code_as_json_does() {
    let dir = scratch("codes");
    let src = "pub fn f() { println(1); }\n\
               struct Hidden { x: i64 }\n\
               pub fn g() -> Hidden { Hidden { x: 1 } }\n\
               fn main() { let s = \"a\"; let t = s; let n: i64 = \"x\"; }\n";
    write(&dir.join("m.kara"), src);
    let (ok, _, err) = karac(&dir, &["check", "m.kara"], &[]);
    assert!(!ok, "{err}");
    for code in ["error[E0221]", "error[E0400]", "error[E0200]"] {
        assert!(err.contains(code), "{code}:\n{err}");
    }
    for phase in ["[typecheck]", "[effect]", "[resolve]"] {
        assert!(!err.contains(phase), "{phase}:\n{err}");
    }
    let (_, out, _) = karac(&dir, &["check", "--output=json", "m.kara"], &[]);
    for code in ["\"E0221\"", "\"E0400\"", "\"E0200\""] {
        assert!(out.contains(code), "{code}:\n{out}");
    }
    // A package prints the same codes.
    let pkg = scratch("codes-pkg");
    package(&pkg, "", "import util.g;\nfn main() { g(); }\n");
    write(
        &pkg.join("src/util.kara"),
        "struct Hidden { x: i64 }\npub fn g() -> Hidden { Hidden { x: 1 } }\n",
    );
    let (ok, out, err) = karac(&pkg, &["check"], &[]);
    let all = format!("{out}{err}");
    assert!(!ok && all.contains("error[E0221]"), "{all}");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&pkg);
}
