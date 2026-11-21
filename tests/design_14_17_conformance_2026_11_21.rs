//! The 2026-11-21 conformance pass over design.md § 14 (Testing) and § 17
//! (Tooling contract), /mnt/project-files/conformance/design-14-17/:
//!
//! - the cascade cap counts each module file of a package on its own, as it
//!   counts each file (B-2026-11-21-93): a package whose two files hold 22 type
//!   errors each reports 20 of each, with a marker per file naming it;
//! - `karac test --output=jsonl` and `--output=json` are accepted, as § 17's
//!   panic record promises, and another output is refused (B-2026-11-21-94).

use std::path::PathBuf;
use std::process::{Command, Output};

fn package(tag: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-d1417-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("kara.toml"),
        "[package]\nname = \"pkg\"\nversion = \"0.1.0\"\nauthors = []\nedition = \"2026\"\n\n[dependencies]\n",
    )
    .unwrap();
    for (name, src) in files {
        std::fs::write(dir.join("src").join(name), src).unwrap();
    }
    dir
}

fn karac(dir: &PathBuf, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .env_remove("KARAC_TEST_JIT")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap()
}

fn bad_lets(n: usize) -> String {
    (0..n)
        .map(|i| format!("    let a{i}: i64 = \"s{i}\";\n"))
        .collect()
}

#[test]
fn d1417_the_cascade_cap_counts_each_file_of_a_package() {
    let main = format!("import other;\nfn main() {{\n{}}}\n", bad_lets(22));
    let other = format!("pub fn g() {{\n{}}}\n", bad_lets(22));
    let dir = package("cap", &[("main.kara", &main), ("other.kara", &other)]);
    let out = karac(&dir, &["check", "--output=json"]);
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let diags = doc["diagnostics"].as_array().unwrap();
    for file in ["src/main.kara", "src/other.kara"] {
        let errors = diags
            .iter()
            .filter(|d| d["severity"] == "error" && d["file"] == file)
            .count();
        assert_eq!(
            errors,
            20,
            "{file}: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        let marker: Vec<_> = diags
            .iter()
            .filter(|d| d["truncated"]["file"] == file)
            .collect();
        assert_eq!(marker.len(), 1, "{file}");
        assert_eq!(marker[0]["truncated"]["suppressed_primary_errors"], 2);
        assert_eq!(marker[0]["phase"], "typecheck");
    }
    // The stream carries the same markers.
    let out = karac(&dir, &["check", "--output=jsonl"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let markers = stdout
        .lines()
        .filter(|l| l.contains("\"truncated\":"))
        .count();
    assert_eq!(markers, 2, "{stdout}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn d1417_karac_test_takes_its_output_flag() {
    let dir = package(
        "out",
        &[
            ("main.kara", "fn main() {}\n"),
            ("main_test.kara", "test \"t\" { assert(true); }\n"),
        ],
    );
    for flag in ["--output=jsonl", "--output=json"] {
        let out = karac(&dir, &["test", "--interp", flag]);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{flag}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            stdout
                .lines()
                .next()
                .unwrap()
                .starts_with("{\"type\":\"run_start\""),
            "{stdout}"
        );
        assert!(stdout.contains("\"type\":\"test_pass\""), "{stdout}");
    }
    let out = karac(&dir, &["test", "--output=text"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("writes JSONL"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}
