//! What a v2 package build refuses before it builds (B-2026-10-25-18): an
//! import of a module the package does not have, when the package has only
//! its entry module, and a cross `[build].target`. Neither reaches code
//! generation, so these need no backend.

use std::path::{Path, PathBuf};
use std::process::Command;

fn package(tag: &str, manifest: &str, main: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-project-v2-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("kara.toml"), manifest).unwrap();
    std::fs::write(dir.join("src/main.kara"), main).unwrap();
    dir
}

fn karac(dir: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

/// A package whose only module imports a module that does not exist is
/// refused by check, run and build alike, as a package of several modules
/// is; before, the lone module's check bound the import and passed.
#[test]
fn project_v2_lone_module_unknown_import_is_refused() {
    let dir = package(
        "unknown-import",
        "[package]\nname = \"demo\"\n",
        "import nope.thing;\nfn main() {\n    println(\"ran\");\n}\n",
    );
    for args in [
        &["check"][..],
        &["check", "src/main.kara"],
        &["run", "src/main.kara"],
        &["build"],
        &["build", "src/main.kara"],
    ] {
        let (code, stdout, stderr) = karac(&dir, args);
        assert_eq!(code, 1, "{args:?}: stdout={stdout} stderr={stderr}");
        assert!(
            stderr.contains("E0112") && stderr.contains("nope"),
            "{args:?}: {stderr}"
        );
        assert!(!stdout.contains("ran"), "{args:?}: {stdout}");
    }
    let _ = std::fs::remove_dir_all(dir);
}

/// `[build].target` naming another platform's triple is refused by name: the
/// v2 build makes a binary for the host only (B-2026-10-23-16), and building
/// one for the host with that target's dependencies would be wrong silently.
#[test]
fn project_v2_cross_build_target_is_refused_by_name() {
    let cross = if cfg!(target_os = "macos") {
        "x86_64-unknown-linux-gnu"
    } else {
        "x86_64-apple-darwin"
    };
    let dir = package(
        "cross-target",
        &format!("[package]\nname = \"demo\"\n\n[build]\ntarget = \"{cross}\"\n"),
        "fn main() {}\n",
    );
    let (code, _, stderr) = karac(&dir, &["build"]);
    assert_eq!(code, 1, "{stderr}");
    assert!(
        stderr.contains(&format!("`[build].target = \"{cross}\"`")),
        "{stderr}"
    );
    let _ = std::fs::remove_dir_all(dir);
}
