//! `karac check --output=json`'s `public_function_effects` leaves out a gated
//! module's functions that an import splices into the program:
//! `import std.random` brought `next_u64` along, and it was listed as the
//! program's own public function (B-2026-11-25-52).

use std::process::Command;

fn public_function_effects(name: &str, src: &str) -> serde_json::Value {
    let path = std::env::temp_dir().join(format!(
        "karac-pfe-spliced-{name}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", "--output=json", path.to_str().unwrap()])
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let doc: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    doc["public_function_effects"].clone()
}

#[test]
fn public_function_effects_leaves_out_a_spliced_random_function() {
    assert_eq!(
        public_function_effects(
            "random",
            "import std.random;\nfn main() { println(random.next_u64() > 0); }\n",
        ),
        serde_json::json!({})
    );
}

#[test]
fn public_function_effects_keeps_the_programs_own_beside_a_splice() {
    assert_eq!(
        public_function_effects(
            "random_and_own",
            "import std.random;\npub fn roll() -> u64 with writes(RandomSource) {\n    random.next_u64()\n}\nfn main() { println(roll() > 0); }\n",
        ),
        serde_json::json!({"roll": ["writes(RandomSource)"]})
    );
}

/// In a package, a module's public function is keyed by its source path,
/// `a.make`, not by the name the combined source gave it (`make__a`).
#[test]
fn public_function_effects_names_package_functions_by_module_path() {
    let dir = std::env::temp_dir().join(format!("karac-pfe-modules-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("kara.toml"),
        "[package]\nname = \"pfe\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/a.kara"),
        "pub fn make() -> i64 with writes(Stdout) {\n    println(\"a\");\n    1\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/b.kara"),
        "pub fn make() -> i64 {\n    2\n}\n\npub fn only_b() -> i64 {\n    3\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/main.kara"),
        "import a;\nimport b;\nfn main() {\n    println(a.make() + b.make() + b.only_b());\n}\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", "--output=json"])
        .current_dir(&dir)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let doc: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(
        doc["public_function_effects"],
        serde_json::json!({
            "a.make": ["writes(Stdout)"],
            "b.make": [],
            "b.only_b": [],
        }),
        "{stdout}"
    );
}
