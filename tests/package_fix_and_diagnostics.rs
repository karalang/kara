//! A package as a dogfooder grows one past a single file (the multi-file
//! rehearsal, 2026-10-30): `karac fix` mends a mistyped import in one pass
//! (B-2026-10-30-27) and runs over the whole package with no file
//! (B-2026-10-30-28); `karac check` names every file one way and locates an
//! import cycle (B-2026-10-30-32); a program's own `Stats` warns of no
//! prelude shadowing (B-2026-10-30-34).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

/// A package with `files` (paths under `src/`) in a fresh directory.
fn package(files: &[(&str, &str)]) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("karac-pkgfix-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("kara.toml"),
        "[package]\nname = \"pkg\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
    )
    .unwrap();
    for (path, src) in files {
        let p = root.join("src").join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, src).unwrap();
    }
    root
}

fn karac(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(root)
        .output()
        .unwrap()
}

fn read(root: &Path, path: &str) -> String {
    std::fs::read_to_string(root.join("src").join(path)).unwrap()
}

const STATS: &str = "pub fn analyze(n: i64) -> i64 {
    n * 2
}
";

#[test]
fn one_fix_pass_mends_a_mistyped_import_and_its_use_is_left_alone() {
    let root = package(&[
        ("text/stats.kara", STATS),
        (
            "main.kara",
            "import text.stats.analyse;\n\nfn main() {\n    println(analyze(4));\n}\n",
        ),
    ]);
    let out = karac(&root, &["check"]);
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        err.contains("unknown item `analyse` in module `text.stats`, did you mean `analyze`?"),
        "{err}"
    );
    // The use says what the import's fix binds: no second error, and no
    // correction that offers the misspelled name.
    assert!(!err.contains("undefined name 'analyze'"), "{err}");
    let out = karac(&root, &["fix", "src/main.kara"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(read(&root, "main.kara").contains("import text.stats.analyze;"));
    assert!(read(&root, "main.kara").contains("println(analyze(4));"));
    let out = karac(&root, &["check"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_use_of_the_misspelled_name_is_corrected_once_the_import_is() {
    let root = package(&[
        ("text/stats.kara", STATS),
        (
            "main.kara",
            "import text.stats.analyse;\n\nfn main() {\n    println(analyse(4));\n}\n",
        ),
    ]);
    // Each pass moves forward: the import, then the use.
    for _ in 0..2 {
        let out = karac(&root, &["fix", "src/main.kara"]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    assert!(read(&root, "main.kara").contains("println(analyze(4));"));
    let out = karac(&root, &["check"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn fix_with_no_file_fixes_every_module_of_the_package() {
    let root = package(&[
        ("text/stats.kara", STATS),
        (
            "main.kara",
            "import text.stats.analyse;\n\nfn main() {\n    println(analyze(4));\n}\n",
        ),
        (
            "text/stats_test.kara",
            "test \"doubles\" {\n    assert(!(analyze(2) == 5));\n}\n",
        ),
    ]);
    let dry = karac(&root, &["fix", "--dry-run"]);
    let shown = String::from_utf8_lossy(&dry.stdout).into_owned();
    assert!(
        dry.status.success(),
        "{}",
        String::from_utf8_lossy(&dry.stderr)
    );
    assert!(
        shown.contains("src/main.kara:1:19: `analyse` → `analyze`"),
        "{shown}"
    );
    assert!(
        read(&root, "main.kara").contains("analyse"),
        "--dry-run wrote"
    );
    let out = karac(&root, &["fix"]);
    let said = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        said.contains("applied 1 fix(es) to src/main.kara"),
        "{said}"
    );
    assert!(
        said.contains("applied 1 fix(es) to src/text/stats_test.kara"),
        "{said}"
    );
    assert!(read(&root, "text/stats_test.kara").contains("assert(not (analyze(2) == 5));"));
    let out = karac(&root, &["check"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn fix_with_no_file_outside_a_package_says_to_name_one() {
    let dir = std::env::temp_dir().join(format!("karac-pkgfix-none-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = karac(&dir, &["fix"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("E0227") && err.contains("karac fix <file.kara>"),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Every diagnostic's `file` in `karac check --output=json`, with its code.
fn json_files(root: &Path) -> Vec<(String, Option<String>, Option<i64>)> {
    let out = karac(root, &["check", "--output=json"]);
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    let start = text.find('{').expect("a JSON envelope");
    let v: serde_json::Value = serde_json::from_str(text[start..].trim()).expect("JSON");
    v["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| {
            (
                d["code"].as_str().unwrap_or("").to_string(),
                d["file"].as_str().map(str::to_string),
                d["line"].as_i64(),
            )
        })
        .collect()
}

#[test]
fn resolver_and_typecheck_diagnostics_name_files_one_way() {
    let private = package(&[
        (
            "text/helpers.kara",
            "private fn lower(s: Str) -> String {\n    s.to_lowercase()\n}\n",
        ),
        (
            "report.kara",
            "import text.helpers.lower;\n\npub fn render(s: Str) -> String {\n    lower(s)\n}\n",
        ),
        (
            "main.kara",
            "import report.render;\n\nfn main() {\n    println(render(\"A\"));\n}\n",
        ),
    ]);
    assert_eq!(
        json_files(&private),
        [(
            "E0111".to_string(),
            Some("src/report.kara".to_string()),
            Some(1)
        )]
    );
    let out = karac(&private, &["check"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("error[E0111]: src/report.kara:1:21:"), "{err}");
    assert!(err.contains("  help: mark `lower` as `pub`"), "{err}");
    let typed = package(&[
        (
            "text/stats.kara",
            "pub fn analyze(n: i64) -> i64 {\n    \"x\"\n}\n",
        ),
        (
            "main.kara",
            "import text.stats.analyze;\n\nfn main() {\n    println(analyze(4));\n}\n",
        ),
    ]);
    let files = json_files(&typed);
    assert!(
        files
            .iter()
            .all(|(_, f, _)| f.as_deref() == Some("src/text/stats.kara")),
        "{files:?}"
    );
    let _ = std::fs::remove_dir_all(&private);
    let _ = std::fs::remove_dir_all(&typed);
}

#[test]
fn an_import_cycle_is_located_at_the_import_that_closes_it() {
    let root = package(&[
        (
            "a.kara",
            "import b.g;\n\npub fn f() -> i64 {\n    g() + 1\n}\n",
        ),
        (
            "b.kara",
            "\nimport a.f;\n\npub fn g() -> i64 {\n    2\n}\n\npub fn h() -> i64 {\n    f()\n}\n",
        ),
        (
            "main.kara",
            "import a.f;\n\nfn main() {\n    println(f());\n}\n",
        ),
    ]);
    let files = json_files(&root);
    let cycle: Vec<_> = files.iter().filter(|(c, _, _)| c == "E0223").collect();
    assert_eq!(cycle.len(), 1, "{files:?}");
    let (_, file, line) = cycle[0];
    let at = (file.as_deref(), *line);
    assert!(
        at == (Some("src/b.kara"), Some(2)) || at == (Some("src/a.kara"), Some(1)),
        "{files:?}"
    );
    let out = karac(&root, &["check"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("error[E0223]: src/b.kara:2:1: circular module dependency")
            || err.contains("error[E0223]: src/a.kara:1:1: circular module dependency"),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_programs_own_stats_shadows_no_prelude_type() {
    let root = package(&[(
        "main.kara",
        "struct Stats {\n    n: i64,\n}\n\nfn main() {\n    let s = Stats { n: 3 };\n    println(s.n);\n}\n",
    )]);
    let out = karac(&root, &["check"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{err}");
    assert!(!err.contains("prelude_shadow"), "{err}");
    // A real prelude type still warns.
    let root2 = package(&[(
        "main.kara",
        "struct Ordering {\n    n: i64,\n}\n\nfn main() {\n    let s = Ordering { n: 3 };\n    println(s.n);\n}\n",
    )]);
    let err = String::from_utf8_lossy(&karac(&root2, &["check"]).stderr).into_owned();
    assert!(err.contains("prelude_shadow"), "{err}");
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&root2);
}
