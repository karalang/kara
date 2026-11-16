//! design.md §4 and §16 conformance (the review thread's pass of
//! 2026-11-16): `karac run --manifest` gives the file the named package's
//! dependencies (B-2026-11-16-198), `--target-cpu=native` builds for the host
//! and `--target-cpu=help` lists each registry once (B-2026-11-16-199), and an
//! import of a module only other platforms' files define names them
//! (B-2026-11-16-200).

use std::path::{Path, PathBuf};
use std::process::Command;

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-targets-modules-{}-{tag}",
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

fn karac(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn manifest(name: &str, deps: &str) -> String {
    format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n\n[dependencies]\n{deps}")
}

#[test]
fn run_with_manifest_gives_the_file_that_packages_dependencies() {
    let dir = scratch("manifest");
    write(&dir.join("dep/kara.toml"), &manifest("dep", ""));
    write(
        &dir.join("dep/src/lib.kara"),
        "pub fn hello() -> i64 { 7 }\n",
    );
    write(
        &dir.join("app/kara.toml"),
        &manifest("app", "dep = { path = \"../dep\" }\n"),
    );
    write(
        &dir.join("app/src/main.kara"),
        "fn main() { println(1); }\n",
    );
    // The file lies outside the package, so only `--manifest` names it.
    write(
        &dir.join("elsewhere/m.kara"),
        "import dep.hello;\nfn main() { println(hello()); }\n",
    );
    let toml = dir.join("app/kara.toml");
    let toml = toml.to_str().unwrap();
    let (ok, out, err) = karac(
        &dir.join("elsewhere"),
        &["run", "--interp", "--manifest", toml, "m.kara"],
    );
    assert!(ok, "{out}{err}");
    assert_eq!(out.trim(), "7", "{err}");
    let (ok, _, err) = karac(&dir.join("elsewhere"), &["run", "--interp", "m.kara"]);
    assert!(!ok && err.contains("unknown module `dep`"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_module_only_other_platforms_define_is_named_with_them() {
    let dir = scratch("platform");
    let other = if cfg!(target_os = "macos") {
        "linux"
    } else {
        "macos"
    };
    write(&dir.join("kara.toml"), &manifest("app", ""));
    write(
        &dir.join("src/main.kara"),
        "import poller.name;\nfn main() { println(name()); }\n",
    );
    write(
        &dir.join(format!("src/poller_{other}.kara")),
        "pub fn name() -> String { \"elsewhere\" }\n",
    );
    let (ok, out, err) = karac(&dir, &["check"]);
    let all = format!("{out}{err}");
    assert!(!ok, "{all}");
    assert!(
        all.contains("E0112")
            && all.contains(&format!(
                "it is defined only for {other} (`poller_{other}.kara`)"
            )),
        "the refusal names the platforms that define the module:\n{all}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "llvm")]
#[test]
fn target_cpu_native_is_the_hosts_cpu() {
    let dir = scratch("native");
    write(&dir.join("hello.kara"), "fn main() { println(\"hi\"); }\n");
    let (_, out, err) = karac(&dir, &["build", "--target-cpu=native", "hello.kara"]);
    // The link needs the runtime archive, which a checkout may lack; the
    // CPU is settled before it.
    assert!(!err.contains("unknown CPU"), "{out}{err}");
    let (ok, _, err) = karac(
        &dir,
        &[
            "build",
            "--target=wasm_wasi",
            "--target-cpu=native",
            "hello.kara",
        ],
    );
    assert!(
        !ok && err.contains("--target-cpu=native names the build host's CPU"),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "llvm")]
#[test]
fn target_cpu_help_lists_each_registry_once_and_names_native() {
    let dir = scratch("help");
    let (ok, out, err) = karac(&dir, &["build", "--target-cpu=help"]);
    assert!(ok, "{err}");
    assert_eq!(
        out.matches("Supported CPUs and features for target `native`:")
            .count(),
        1,
        "{out}"
    );
    assert!(out.contains("`native` is also accepted"), "{out}");
    assert!(
        out.contains("Supported CPUs and features for target `wasm_wasi`:"),
        "the listing carries the wasm32 registry:\n{out}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
