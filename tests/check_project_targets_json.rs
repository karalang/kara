//! B-2026-11-05-12: `karac check --output=json` over a package that declares
//! several targets prints ONE object, as a file check does (design.md
//! § Checking several targets): each target's own findings under `targets`,
//! tagged with the target, and a finding on every target once under
//! `shared_diagnostics`. Before, it printed one untagged `{"diagnostics"}`
//! object per target, the type error twice.

use std::path::PathBuf;
use std::process::Command;

fn package(tag: &str, main: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "karac-check-project-json-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("kara.toml"),
        "[package]\nname = \"demo\"\n\n[build]\ntargets = [\"native\", \"wasm_wasi\"]\n",
    )
    .unwrap();
    std::fs::write(root.join("src/main.kara"), main).unwrap();
    root
}

fn check_json(root: &PathBuf) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", "--output=json"])
        .current_dir(root)
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[test]
fn project_check_json_reports_a_finding_on_every_target_once() {
    let root = package(
        "shared",
        "fn main() {\n    let x: i32 = \"text\";\n    println(\"{x}\");\n}\n",
    );
    let (ok, stdout) = check_json(&root);
    let _ = std::fs::remove_dir_all(&root);
    assert!(!ok, "a type error fails the check: {stdout}");
    assert_eq!(
        stdout.lines().count(),
        1,
        "one JSON object for the sweep: {stdout}"
    );
    assert!(
        stdout.starts_with("{\"targets\":[")
            && stdout.contains("{\"target\":\"native\",\"success\":false,\"total_errors\":1,\"diagnostics\":[]}")
            && stdout.contains("{\"target\":\"wasm_wasi\",\"success\":false,\"total_errors\":1,\"diagnostics\":[]}")
            && stdout.trim_end().ends_with("\"success\":false}"),
        "each target tagged, its own list empty: {stdout}"
    );
    let shared = stdout
        .split("\"shared_diagnostics\":[")
        .nth(1)
        .expect("a shared_diagnostics array");
    assert_eq!(
        shared.matches("\"severity\":\"error\"").count(),
        1,
        "the type error reported once, as shared: {stdout}"
    );
}

#[test]
fn project_check_json_passes_with_empty_groups() {
    let root = package("clean", "fn main() {\n    println(\"hi\");\n}\n");
    let (ok, stdout) = check_json(&root);
    let _ = std::fs::remove_dir_all(&root);
    assert!(ok, "hello world checks clean on both targets: {stdout}");
    assert!(
        stdout.contains("\"shared_diagnostics\":[]")
            && stdout.contains("\"target\":\"native\",\"success\":true")
            && stdout.contains("\"target\":\"wasm_wasi\",\"success\":true")
            && stdout.trim_end().ends_with("\"success\":true}"),
        "{stdout}"
    );
}
