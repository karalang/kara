//! B-2026-11-08-38 — a package whose kara.toml declares several
//! `[build] targets` is checked per target by the same check a one-target
//! package gets (the v2 check of its combined source), so it refuses
//! nothing a one-target check passes: `s.split(" ").count()` was refused as
//! "no method 'count' on type 'Vec'". A finding every target makes is
//! reported once under "all targets", with its snippet, as for a file.

use std::process::Command;

fn package(tag: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-pkg-targets-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("kara.toml"),
        "[package]\nname = \"p\"\nversion = \"0.1.0\"\n\n[build]\ntargets = [\"native\", \"wasm_wasi\"]\n",
    )
    .unwrap();
    for (name, src) in files {
        std::fs::write(dir.join("src").join(name), src).unwrap();
    }
    dir
}

fn check(dir: &std::path::Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

#[test]
fn a_two_target_package_checks_a_split_iterator_method() {
    let lone = package(
        "lone",
        &[(
            "main.kara",
            "fn main() {\n    let s = \"a b\".to_string();\n    println(s.split(\" \").count());\n    println(s.split_whitespace().count());\n}\n",
        )],
    );
    let (ok, out) = check(&lone, &[]);
    assert!(
        ok && out.contains("All 2 target(s) checked clean."),
        "{out}"
    );
    let modules = package(
        "modules",
        &[
            (
                "main.kara",
                "import util.{words};\nfn main() { println(words(\"a b\".to_string())); }\n",
            ),
            (
                "util.kara",
                "pub fn words(s: String) -> i64 { s.split(\" \").count() }\n",
            ),
        ],
    );
    let (ok, out) = check(&modules, &[]);
    assert!(
        ok && out.contains("All 2 target(s) checked clean."),
        "{out}"
    );
    let _ = std::fs::remove_dir_all(&lone);
    let _ = std::fs::remove_dir_all(&modules);
}

#[test]
fn a_finding_on_every_target_is_reported_once_with_its_snippet() {
    let dir = package(
        "shared",
        &[(
            "main.kara",
            "fn main() {\n    let s = \"a b\".to_string();\n    println(s.split(\" \").count());\n    let x: i64 = \"no\";\n}\n",
        )],
    );
    let (ok, out) = check(&dir, &[]);
    assert!(!ok, "{out}");
    assert!(out.contains("── all targets ──"), "{out}");
    assert_eq!(
        out.matches("expected 'i64', found 'String'").count(),
        1,
        "{out}"
    );
    // karac prints the host's separator (`src\main.kara` on Windows), as
    // the other package tests' `slash_paths` allow for.
    assert!(
        out.replace('\\', "/")
            .contains("error[typecheck]: src/main.kara:4:18"),
        "{out}"
    );
    assert!(out.contains("4 |     let x: i64 = \"no\";"), "{out}");
    assert!(!out.contains("no method 'count'"), "{out}");
    assert!(out.contains("2 of 2 target(s) failed"), "{out}");
    let (ok, out) = check(&dir, &["--output=json"]);
    assert!(!ok);
    assert!(
        out.contains("\"shared_diagnostics\":[{") && out.contains("\"code\":\"E0200\""),
        "{out}"
    );
    assert!(!out.contains("no method 'count'"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}
