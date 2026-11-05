//! Every diagnostic carries a `concept` id from design.md's concept table,
//! and `karac explain` answers for each id (design.md §17 rule 4 and
//! `karac explain`, B-2026-11-05-28).

use std::path::PathBuf;
use std::process::Command;

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-concept-ids-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The ids design.md's concept table lists.
fn table_ids() -> Vec<String> {
    let design =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/docs/design.md")).unwrap();
    let table = &design[design.find("#### Concept ids").unwrap()..];
    let table = &table[..table.find("#### Error return traces").unwrap()];
    table
        .lines()
        .filter_map(|l| l.strip_prefix("| `"))
        .filter_map(|l| l.split_once('`'))
        .map(|(id, _)| id.to_string())
        .collect()
}

/// Each diagnostic object in a JSON value: one with a `severity`.
fn diagnostics<'a>(v: &'a serde_json::Value, out: &mut Vec<&'a serde_json::Value>) {
    match v {
        serde_json::Value::Object(m) => {
            if m.contains_key("severity") {
                out.push(v);
            }
            m.values().for_each(|x| diagnostics(x, out));
        }
        serde_json::Value::Array(a) => a.iter().for_each(|x| diagnostics(x, out)),
        _ => {}
    }
}

/// The `(code, concept)` of each diagnostic `karac check` reports on `src`,
/// in the batch document and in the stream alike.
fn concepts(src: &str) -> Vec<(String, String)> {
    let dir = scratch("check");
    std::fs::write(dir.join("main.kara"), src).unwrap();
    let ids = table_ids();
    let mut got = Vec::new();
    for mode in ["--output=json", "--output=jsonl"] {
        let out = karac()
            .args(["check", mode, "main.kara"])
            .current_dir(&dir)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let mut found = Vec::new();
        let values: Vec<serde_json::Value> = stdout
            .lines()
            .filter(|l| l.starts_with('{'))
            .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}")))
            .collect();
        for v in &values {
            diagnostics(v, &mut found);
        }
        assert!(!found.is_empty(), "{mode}: no diagnostics\n{stdout}");
        for d in found {
            let concept = d["concept"]
                .as_str()
                .unwrap_or_else(|| panic!("{mode}: {d}"));
            assert!(
                ids.iter().any(|i| i == concept),
                "{concept} is not in the table"
            );
            if mode == "--output=json" {
                got.push((
                    d["code"].as_str().unwrap_or("").to_string(),
                    concept.to_string(),
                ));
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    got
}

#[test]
fn a_diagnostic_carries_its_concept_id() {
    let cases = [
        (
            "fn take(s: own String) { }\nfn main() {\n    let s = \"a\".to_string();\n    take(s);\n    take(s);\n}\n",
            "E0500",
            "ownership/move-semantics",
        ),
        (
            "fn inner() { println(\"x\"); }\nfn mid() { inner(); }\npub fn top() { mid(); }\nfn main() { top(); }\n",
            "E0400",
            "effects/public-function-declaration",
        ),
        (
            "#[deprecated(note: \"x\")]\nfn old() -> i64 { 1 }\nfn main() { println(old()); }\n",
            "W0245",
            "items/deprecation",
        ),
        ("fn main() { let x: i64 = \"a\"; }\n", "E0200", "types/checking"),
        ("fn main() { let = ; }\n", "", "syntax/lexical-structure"),
    ];
    for (src, code, concept) in cases {
        let got = concepts(src);
        assert!(
            got.iter()
                .any(|(c, k)| (code.is_empty() || c == code) && k == concept),
            "{code} {concept}: {got:?}"
        );
    }
}

#[test]
fn a_package_diagnostic_carries_its_concept_id() {
    let dir = scratch("package");
    std::fs::write(dir.join("kara.toml"), "[package]\nname = \"app\"\n").unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/util.kara"), "pub fn n() -> i64 { \"a\" }\n").unwrap();
    std::fs::write(
        dir.join("src/main.kara"),
        "import util.{n};\nfn main() { println(n()); }\n",
    )
    .unwrap();
    let out = karac()
        .args(["check", "--output=json"])
        .current_dir(&dir)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let mut found = Vec::new();
    diagnostics(&v, &mut found);
    assert!(!found.is_empty(), "{stdout}");
    assert!(
        found.iter().all(|d| d["concept"] == "types/checking"),
        "{stdout}"
    );
}

#[test]
fn explain_answers_for_every_concept_id() {
    for id in table_ids() {
        let out = karac().args(["explain", &id]).output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        if !out.status.success() {
            // The retired and deferred rows name no diagnostic.
            let err = String::from_utf8_lossy(&out.stderr);
            assert!(
                id == "ownership/rc-fallback" || id == "performance/allocation-hoisting",
                "{id}: {err}"
            );
            continue;
        }
        assert!(
            text.contains(&format!("concept: {id}")) && text.contains("docs/"),
            "{text}"
        );
        let json = karac()
            .args(["explain", &id, "--format=json"])
            .output()
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
        assert_eq!(v["concept"], id.as_str());
    }
    let out = karac().args(["explain", "E0500"]).output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("concept: ownership/move-semantics"));
}
