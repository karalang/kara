//! A machine-applicable fix has one shape in JSON diagnostics (design.md
//! § 17, B-2026-11-05-30): a hint whose `diff` is `{file, line, old, new}`
//! over the source lines the fix touches, beside the byte-level edits it
//! summarizes, `replacement` for one edit and `fix_diff` for several. The
//! 0.8.0 `fix_it` object and `fixes` array are gone.

use std::path::{Path, PathBuf};
use std::process::Command;

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-fix-hint-shape-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The diagnostics `karac check <args>` reports in `dir`, from the batch
/// document (`--output=json`) or the stream (`--output=jsonl`).
fn diagnostics(dir: &Path, mode: &str, args: &[&str]) -> Vec<serde_json::Value> {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(mode)
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    // A path's host separator, `\\` in a JSON string on Windows, as `/`.
    let stdout = String::from_utf8_lossy(&out.stdout).replace("\\\\", "/");
    let mut found = Vec::new();
    for line in stdout.lines().filter(|l| l.starts_with('{')) {
        let v: serde_json::Value =
            serde_json::from_str(line).unwrap_or_else(|e| panic!("{e}: {line}"));
        if v.get("severity").is_some() {
            found.push(v);
        } else if let Some(ds) = v["diagnostics"].as_array() {
            found.extend(ds.iter().cloned());
        }
    }
    assert!(!found.is_empty(), "{mode}: no diagnostics\n{stdout}");
    for d in &found {
        assert!(
            d.get("fix_it").is_none() && d.get("fixes").is_none(),
            "{mode}: the 0.8.0 fix fields are gone: {d}"
        );
    }
    found
}

fn the(diags: &[serde_json::Value], code: &str) -> serde_json::Value {
    diags
        .iter()
        .find(|d| d["code"] == code)
        .unwrap_or_else(|| panic!("no {code} in {diags:?}"))
        .clone()
}

#[test]
fn a_one_edit_fix_is_a_replacement_and_a_hint_diff_of_its_line() {
    let dir = scratch("one");
    let src = "fn bump(v: mut ref Vec[i64]) { v.push(1); }\n\
               fn main() {\n    let mut v: Vec[i64] = Vec.new();\n    bump(v);\n    println(v.len());\n}\n";
    std::fs::write(dir.join("m.kara"), src).unwrap();
    for mode in ["--output=json", "--output=jsonl"] {
        let d = the(&diagnostics(&dir, mode, &["m.kara"]), "E0218");
        let at = src.find("v);").unwrap();
        assert_eq!(
            d["replacement"],
            serde_json::json!({"offset": at, "length": 0, "text": "mut "}),
            "{mode}: {d}"
        );
        assert!(d.get("fix_diff").is_none(), "{mode}: {d}");
        assert_eq!(
            d["hints"][0]["diff"],
            serde_json::json!({"file": "m.kara", "line": 4, "old": "    bump(v);", "new": "    bump(mut v);"}),
            "{mode}: {d}"
        );
    }
    // `karac fix` applies the same edit the diff describes.
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["fix", "m.kara"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let after = std::fs::read_to_string(dir.join("m.kara")).unwrap();
    assert!(after.contains("\n    bump(mut v);\n"), "{after}");
}

#[test]
fn a_several_edit_fix_is_a_fix_diff_and_one_hint_diff_over_its_lines() {
    let dir = scratch("several");
    let src = "let mut my_count: i64 = 0;\n\
               fn bump() { my_count = my_count + 1; }\n\
               fn main() { bump(); }\n";
    std::fs::write(dir.join("r.kara"), src).unwrap();
    let d = the(&diagnostics(&dir, "--output=json", &["r.kara"]), "E0100");
    assert!(d.get("replacement").is_none(), "{d}");
    let edits = d["fix_diff"].as_array().unwrap_or_else(|| panic!("{d}"));
    assert_eq!(edits.len(), 3, "{d}");
    let hints: Vec<&serde_json::Value> = d["hints"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|h| h.get("diff").is_some())
        .collect();
    assert_eq!(hints.len(), 1, "one fix, one diff: {d}");
    assert_eq!(
        hints[0]["diff"],
        serde_json::json!({
            "file": "r.kara",
            "line": 1,
            "old": "let mut my_count: i64 = 0;\nfn bump() { my_count = my_count + 1; }",
            "new": "let mut MY_COUNT: i64 = 0;\nfn bump() { MY_COUNT = MY_COUNT + 1; }",
        }),
        "{d}"
    );
}

#[test]
fn a_package_fix_hint_names_its_own_file_and_line() {
    let dir = scratch("package");
    std::fs::write(dir.join("kara.toml"), "[package]\nname = \"app\"\n").unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("src/util.kara"),
        "pub fn bump(v: mut ref Vec[i64]) { v.push(1); }\n",
    )
    .unwrap();
    let main = "import util.{bump};\nfn main() {\n    let mut v: Vec[i64] = Vec.new();\n    v.push(0);\n    bump(v);\n    println(v.len());\n}\n";
    std::fs::write(dir.join("src/main.kara"), main).unwrap();
    let d = the(&diagnostics(&dir, "--output=json", &[]), "E0218");
    assert_eq!(d["file"], "src/main.kara", "{d}");
    assert_eq!(
        d["replacement"]["offset"],
        main.find("v);").unwrap(),
        "the edit's offset is a byte of the diagnostic's own file: {d}"
    );
    assert_eq!(
        d["hints"][0]["diff"],
        serde_json::json!({"file": "src/main.kara", "line": 5, "old": "    bump(v);", "new": "    bump(mut v);"}),
        "{d}"
    );
}

#[test]
fn a_package_fix_hint_keeps_the_module_qualifier_on_its_line() {
    // The package is checked as one combined text in which `util.` is
    // blanked; the diff is the user's own line, qualifier and all.
    let dir = scratch("qualified");
    std::fs::write(dir.join("kara.toml"), "[package]\nname = \"app\"\n").unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("src/util.kara"),
        "pub fn bump(v: mut ref Vec[i64]) { v.push(1); }\n",
    )
    .unwrap();
    let main = "import util;\nfn main() {\n    let mut v: Vec[i64] = Vec.new();\n    v.push(0);\n    util.bump(v);\n    println(v.len());\n}\n";
    std::fs::write(dir.join("src/main.kara"), main).unwrap();
    let d = the(&diagnostics(&dir, "--output=json", &[]), "E0218");
    assert_eq!(d["replacement"]["offset"], main.find("v);").unwrap(), "{d}");
    assert_eq!(
        d["hints"][0]["diff"],
        serde_json::json!({"file": "src/main.kara", "line": 5, "old": "    util.bump(v);", "new": "    util.bump(mut v);"}),
        "{d}"
    );
}
