//! `karac build --output=jsonl` on a failing build (B-2026-11-16-286):
//! `codegen` is skipped with the diagnostics that blocked it, and
//! `program_effects` is `null` when effect analysis reported an error.

use std::path::PathBuf;
use std::process::Command;

fn scratch(name: &str, source: &str) -> (PathBuf, PathBuf) {
    let dir =
        std::env::temp_dir().join(format!("karac-build-stream-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("m.kara");
    std::fs::write(&file, source).unwrap();
    (dir, file)
}

fn events(args: &[&str], dir: &PathBuf) -> Vec<serde_json::Value> {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}")))
        .collect()
}

#[test]
fn build_stream_skips_codegen_after_a_type_error() {
    let (dir, _) = scratch("type-error", "fn main() { let x: i64 = \"s\"; }\n");
    let ev = events(&["build", "--output=jsonl", "m.kara"], &dir);
    let n = ev.len();
    assert!(n >= 2, "{ev:?}");
    assert_eq!(ev[n - 2]["type"], "phase_skipped", "{ev:?}");
    assert_eq!(ev[n - 2]["phase"], "codegen");
    assert_eq!(ev[n - 2]["blocking"], serde_json::json!(["d1"]));
    assert_eq!(ev[n - 1]["type"], "build_complete");
    assert_eq!(ev[n - 1]["success"], false);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn program_effects_is_null_when_effect_analysis_reports_an_error() {
    // `f` performs `writes(Stdout)` without declaring it, so `main`'s set
    // would read `[]` where the program does write.
    let (dir, _) = scratch(
        "effect-error",
        "pub fn f() { println(\"x\"); }\nfn main() { f(); }\n",
    );
    let ev = events(&["build", "--output=jsonl", "m.kara"], &dir);
    let done = ev.last().unwrap();
    assert_eq!(done["type"], "build_complete", "{ev:?}");
    assert_eq!(done["program_effects"], serde_json::Value::Null, "{ev:?}");
    let doc = &events(&["check", "--output=json", "m.kara"], &dir)[0];
    assert_eq!(doc["program_effects"], serde_json::Value::Null, "{doc}");
    assert_eq!(
        doc["public_function_effects"]["f"],
        serde_json::json!(["writes(Stdout)"])
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn program_effects_lists_main_after_a_clean_effect_phase() {
    let (dir, _) = scratch(
        "effect-clean",
        "pub fn f() with writes(Stdout) { println(\"x\"); }\nfn main() { f(); }\n",
    );
    let doc = &events(&["check", "--output=json", "m.kara"], &dir)[0];
    assert_eq!(
        doc["program_effects"],
        serde_json::json!(["writes(Stdout)"]),
        "{doc}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
