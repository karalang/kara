//! The Mend tasks under `examples/mend/examples/` are outside the example
//! corpus (their files are deliberately broken), so nothing checked them and
//! two drifted: `two_source_totals`' own solution was refused once a
//! resource needed a provider, and `concurrent_emails`' canned run still
//! answered the task it had before its rewrite (B-2026-11-20-137). Each task
//! pins here: its solution checks with no diagnostic, its last canned
//! response reaches one with a single `karac fix`, and both print the
//! oracle where the task has one.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn karac(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

/// The diagnostics `karac check --output=json` reports, as `code: message`.
fn diagnostics(dir: &Path, file: &str) -> Vec<String> {
    let (_, out) = karac(dir, &["check", "--output=json", file]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"));
    v["diagnostics"]
        .as_array()
        .map(|ds| {
            ds.iter()
                .map(|d| format!("{}: {}", d["code"], d["message"]))
                .collect()
        })
        .unwrap_or_default()
}

fn tasks() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/mend/examples");
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&root)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("task.md").exists())
        .collect();
    dirs.sort();
    dirs
}

#[test]
fn every_mend_task_converges_to_its_oracle() {
    let tasks = tasks();
    assert!(tasks.len() >= 9, "{tasks:?}");
    let mut failures = Vec::new();
    for task in &tasks {
        let name = task.file_name().unwrap().to_string_lossy().into_owned();
        let oracle = std::fs::read_to_string(task.join("expected.txt")).ok();
        let solution = diagnostics(task, "solution.kara");
        if !solution.is_empty() {
            failures.push(format!("{name}: solution.kara: {solution:?}"));
        }
        if let Some(want) = &oracle {
            let (_, got) = karac(task, &["__mir-run", "solution.kara"]);
            if &got != want {
                failures.push(format!("{name}: solution.kara prints {got:?}"));
            }
        }
        let canned: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(task.join("canned_responses.json")).unwrap(),
        )
        .unwrap();
        let last = canned["responses"]
            .as_array()
            .and_then(|r| r.last())
            .and_then(|r| r["kara_source"].as_str())
            .unwrap_or_else(|| panic!("{name}: no canned response"));
        let dir = std::env::temp_dir().join(format!("karac-mend-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("current.kara"), last).unwrap();
        karac(&dir, &["fix", "current.kara"]);
        let left = diagnostics(&dir, "current.kara");
        if !left.is_empty() {
            failures.push(format!(
                "{name}: last canned response after `karac fix`: {left:?}"
            ));
        }
        if let Some(want) = &oracle {
            let (_, got) = karac(&dir, &["__mir-run", "current.kara"]);
            if &got != want {
                failures.push(format!("{name}: fixed canned response prints {got:?}"));
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
