//! B-2026-11-27-16 — in a package, a library name used only in a test
//! companion (`src/main_test.kara`) or another module gets the import in
//! that file, after that file's own imports. The fix-it used to land after
//! the combined source's import header, which is `src/main.kara` at offset
//! 0, and `karac fix` applied nothing.

use std::path::{Path, PathBuf};
use std::process::Command;

fn karac(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
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

fn package(tag: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-pkg-import-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("kara.toml"),
        "[package]\nname = \"pin\"\nversion = \"0.1.0\"\nauthors = []\nedition = \"2026\"\n\n[dependencies]\n",
    )
    .unwrap();
    for (name, text) in files {
        std::fs::write(dir.join(name), text).unwrap();
    }
    dir
}

const MAIN: &str = "import util;\n\nfn main() {\n    println(util.twice(2));\n}\n";
const UTIL: &str = "// The helpers.\n\npub fn twice(n: i64) -> i64 with blocks {\n    let g = TaskGroup.new();\n    g.spawn(|| n * 2).join()\n}\n";
const TEST: &str = "import util;\n\ntest \"twice\" {\n    let g = TaskGroup.new();\n    assert_eq(g.spawn(|| util.twice(3)).join(), 6);\n}\n";

#[test]
fn a_test_companion_and_a_module_get_their_own_imports() {
    let dir = package(
        "own",
        &[
            ("src/main.kara", MAIN),
            ("src/util.kara", UTIL),
            ("src/main_test.kara", TEST),
        ],
    );
    let (ok, json) = karac(&dir, &["check", "--output=json"]);
    assert!(!ok, "accepted:\n{json}");
    // Each use's fix-it is placed in its own file: none names main.kara.
    for line in json.lines().filter(|l| l.contains("\"E0520\"")) {
        assert!(
            line.contains("\"file\":\"src/util.kara\"")
                || line.contains("\"file\":\"src/main_test.kara\""),
            "{line}"
        );
        assert!(!line.contains("src/main.kara"), "{line}");
    }
    karac(&dir, &["fix"]);
    let read = |f: &str| std::fs::read_to_string(dir.join(f)).unwrap();
    assert_eq!(read("src/main.kara"), MAIN);
    assert_eq!(
        read("src/util.kara"),
        UTIL.replace(
            "// The helpers.\n\n",
            "// The helpers.\n\nimport std.sync.{TaskGroup};\n\n"
        )
    );
    assert_eq!(
        read("src/main_test.kara"),
        TEST.replace(
            "import util;\n",
            "import util;\nimport std.sync.{TaskGroup};\n"
        )
    );
    let (ok, out) = karac(&dir, &["check"]);
    assert!(ok, "{out}");
    let (ok, out) = karac(&dir, &["test"]);
    assert!(ok && out.contains("\"passed\":1"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn karac_fix_of_the_test_file_alone_adds_its_import() {
    let dir = package(
        "alone",
        &[
            ("src/main.kara", "fn main() {\n    println(1);\n}\n"),
            ("src/main_test.kara", "test \"group\" {\n    let g = TaskGroup.new();\n    assert_eq(g.spawn(|| 1).join(), 1);\n}\n"),
        ],
    );
    karac(&dir, &["fix", "src/main_test.kara"]);
    assert_eq!(
        std::fs::read_to_string(dir.join("src/main_test.kara")).unwrap(),
        "import std.sync.{TaskGroup};\n\ntest \"group\" {\n    let g = TaskGroup.new();\n    assert_eq(g.spawn(|| 1).join(), 1);\n}\n"
    );
    let (ok, out) = karac(&dir, &["test"]);
    assert!(ok, "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}
