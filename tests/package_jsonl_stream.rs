//! B-2026-11-05-26: a package reports in the shapes a lone file does
//! (design.md § 17). `karac check --output=jsonl` on a package member and
//! `karac build --output=jsonl` in a package stream `build_start`, phase
//! events, `diagnostic` events with ids, and `build_complete`; the check runs
//! every phase, so an ownership error after a type error is reported; and
//! `--output=json` is the batch document, ids and `program_effects`
//! included, its places named by the package's files. A refusal only the
//! module boundaries carry (a parse error in a sibling) streams the same way.
//! Before, a member check printed bare `type_error` lines and stopped at the
//! type checker, and a package build printed the batch entries untyped.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

fn package(tag: &str, main: &str, util: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("karac-pkg-jsonl-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("kara.toml"), "[package]\nname = \"demo\"\n").unwrap();
    std::fs::write(root.join("src/main.kara"), main).unwrap();
    std::fs::write(root.join("src/util.kara"), util).unwrap();
    root
}

fn karac(root: &Path, args: &[&str]) -> (Option<i32>, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

fn events(stdout: &str) -> Vec<Value> {
    // A path's host separator, `\\` in a JSON string on Windows, as `/`.
    stdout
        .replace("\\\\", "/")
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}")))
        .collect()
}

/// The batch `--output=json` document, its paths read as `events` reads
/// the stream's, so the two compare equal on Windows too.
fn document(json: &str) -> Value {
    serde_json::from_str(json.trim().replace("\\\\", "/").as_str()).unwrap()
}

/// The stream is well formed: every line has a `type`, it opens with
/// `build_start` naming the package's files and closes with
/// `build_complete`, whose `total_errors` is the number of error
/// diagnostics. Returns the diagnostics without their `type`.
fn stream_diagnostics(stdout: &str) -> Vec<Value> {
    let ev = events(stdout);
    assert!(
        ev.iter().all(|e| e["type"].is_string()),
        "every line is an event: {stdout}"
    );
    assert_eq!(ev[0]["type"], "build_start", "{stdout}");
    let files = ev[0]["files"].as_array().unwrap();
    assert!(
        files.contains(&Value::from("src/main.kara"))
            && files.contains(&Value::from("src/util.kara")),
        "build_start names the package's files: {stdout}"
    );
    let last = ev.last().unwrap();
    assert_eq!(last["type"], "build_complete", "{stdout}");
    let diags: Vec<Value> = ev
        .iter()
        .filter(|e| e["type"] == "diagnostic")
        .map(|e| {
            let mut e = e.clone();
            e.as_object_mut().unwrap().remove("type");
            e
        })
        .collect();
    let errors = diags.iter().filter(|d| d["severity"] == "error").count();
    assert_eq!(last["total_errors"], Value::from(errors), "{stdout}");
    assert_eq!(last["success"], Value::from(errors == 0), "{stdout}");
    for d in &diags {
        assert!(
            d["id"].is_string() && d["primary"] == true,
            "a diagnostic has an id: {d}"
        );
    }
    diags
}

const MAIN: &str = "import util;\n\nfn main() {\n    let s = String.from(\"a\");\n    let t = s;\n    println(s);\n    let x: i64 = \"q\";\n    util.hi();\n}\n";
const UTIL: &str = "pub fn hi() with writes(Stdout) {\n    println(\"hi\");\n}\n";

#[test]
fn package_member_check_streams_every_phase() {
    let root = package("member", MAIN, UTIL);
    let (code, jsonl) = karac(&root, &["check", "--output=jsonl", "src/main.kara"]);
    let (json_code, json) = karac(&root, &["check", "--output=json", "src/main.kara"]);
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(code, Some(1), "{jsonl}");
    assert_eq!(json_code, Some(1), "{json}");
    let streamed = stream_diagnostics(&jsonl);
    let phases: Vec<&str> = streamed
        .iter()
        .filter_map(|d| d["phase"].as_str())
        .collect();
    assert!(
        phases.contains(&"typecheck") && phases.contains(&"ownership"),
        "the type error and the ownership error after it: {jsonl}"
    );
    assert!(
        streamed.iter().all(|d| d["file"] == "src/main.kara"),
        "places name the package's file: {jsonl}"
    );
    // The batch document: the same diagnostics, and the effects keys.
    let doc = document(&json);
    assert!(doc["program_effects"].is_array(), "{json}");
    assert_eq!(doc["diagnostics"].as_array().unwrap(), &streamed, "{json}");
}

#[test]
fn package_build_streams_the_check() {
    let root = package("build", MAIN, UTIL);
    let (code, jsonl) = karac(&root, &["build", "--output=jsonl"]);
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(code, Some(1), "{jsonl}");
    let streamed = stream_diagnostics(&jsonl);
    assert!(
        streamed.iter().any(|d| d["phase"] == "ownership"),
        "{jsonl}"
    );
}

#[test]
fn package_refusal_streams_with_ids() {
    let root = package("refusal", MAIN, "pub fn hi( {\n}\n");
    let (code, jsonl) = karac(&root, &["check", "--output=jsonl", "src/main.kara"]);
    let (_, json) = karac(&root, &["check", "--output=json", "src/main.kara"]);
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(code, Some(1), "{jsonl}");
    let streamed = stream_diagnostics(&jsonl);
    assert!(
        streamed
            .iter()
            .any(|d| d["phase"] == "parse" && d["file"] == "src/util.kara"),
        "the sibling's parse error: {jsonl}"
    );
    assert!(
        events(&jsonl)
            .iter()
            .any(|e| e["type"] == "phase_skipped" && e["phase"] == "ownership"),
        "the phases after it are skipped: {jsonl}"
    );
    let doc = document(&json);
    assert!(doc["program_effects"].is_null(), "{json}");
    assert_eq!(doc["diagnostics"].as_array().unwrap(), &streamed, "{json}");
}

#[test]
fn clean_package_member_check_streams_success() {
    let root = package(
        "clean",
        "import util;\n\nfn main() {\n    util.hi();\n}\n",
        UTIL,
    );
    let (code, jsonl) = karac(&root, &["check", "--output=jsonl", "src/main.kara"]);
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(code, Some(0), "{jsonl}");
    assert!(stream_diagnostics(&jsonl).is_empty(), "{jsonl}");
}

#[test]
fn clean_project_check_json_carries_effects_and_ids() {
    let root = package(
        "project",
        "import util;\n\nfn main() {\n    util.hi();\n}\n",
        UTIL,
    );
    let (code, json) = karac(&root, &["check", "--output=json"]);
    let (_, jsonl) = karac(&root, &["check", "--output=jsonl"]);
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(code, Some(0), "{json}");
    let doc = document(&json);
    assert_eq!(
        doc["program_effects"],
        serde_json::json!(["writes(Stdout)"]),
        "{json}"
    );
    assert!(doc["diagnostics"].is_array(), "{json}");
    let streamed = stream_diagnostics(&jsonl);
    assert_eq!(doc["diagnostics"].as_array().unwrap(), &streamed, "{jsonl}");
    assert_eq!(
        events(&jsonl).last().unwrap()["program_effects"],
        serde_json::json!(["writes(Stdout)"]),
        "{jsonl}"
    );
}
