//! The CLI's help and flags against its behaviour (B-2026-11-16-287).

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-help-drift-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn karac(dir: &PathBuf, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn top_level_help_lists_only_flags_that_work() {
    let dir = scratch("help");
    let help = String::from_utf8_lossy(&karac(&dir, &["help"]).stdout).into_owned();
    // `run` refuses `--sequential` and `build` calls it unknown.
    assert!(!help.contains("--sequential"), "{help}");
    // `install "path = ."` is refused; `path=.` is the form that parses.
    assert!(help.contains("Spec accepts `path=...`"), "{help}");
    assert!(!help.contains("`path = ...`"), "{help}");
    // migrate no longer writes `lock self.field { ... }`.
    assert!(!help.contains("lock self.field"), "{help}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cache_key_takes_spaced_flag_values() {
    let dir = scratch("cache-key");
    let spaced = karac(&dir, &["cache", "key", "--pkg", "x", "--version", "1.0.0"]);
    let joined = karac(&dir, &["cache", "key", "--pkg=x", "--version=1.0.0"]);
    assert!(
        spaced.status.success(),
        "{}",
        String::from_utf8_lossy(&spaced.stderr)
    );
    assert_eq!(spaced.stdout, joined.stdout);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fix_fails_on_a_file_left_with_an_error() {
    let dir = scratch("fix");
    std::fs::write(dir.join("bad.kara"), "fn main() { let x: i64 = \"s\"; }\n").unwrap();
    std::fs::write(dir.join("ok.kara"), "fn main() { println(1); }\n").unwrap();
    let bad = karac(&dir, &["fix", "bad.kara"]);
    assert!(!bad.status.success());
    assert!(
        String::from_utf8_lossy(&bad.stderr).contains("have no machine-applicable fix"),
        "{}",
        String::from_utf8_lossy(&bad.stderr)
    );
    let ok = karac(&dir, &["fix", "ok.kara"]);
    assert!(
        ok.status.success(),
        "{}",
        String::from_utf8_lossy(&ok.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn debug_renders_the_panic_record_run_prints() {
    let dir = scratch("debug");
    std::fs::write(
        dir.join("p.kara"),
        "fn main() { let v: Vec[i64] = Vec.new(); println(v[3]); }\n",
    )
    .unwrap();
    let run = karac(&dir, &["run", "--output=json", "p.kara"]);
    let record = String::from_utf8_lossy(&run.stdout).into_owned();
    assert!(record.contains("index_out_of_bounds"), "{record}");
    let mut debug = Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(&dir)
        .args(["debug", "-"])
        .env("NO_COLOR", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    debug
        .stdin
        .take()
        .unwrap()
        .write_all(record.as_bytes())
        .unwrap();
    let out = debug.wait_with_output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // The kind renders as words, the location as the record gives it.
    assert!(text.contains("panic: index out of bounds"), "{text}");
    assert!(text.contains("at p.kara:1:50"), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}
