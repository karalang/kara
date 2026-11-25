//! `import data.{Column}` is data.md's own import line. The `data` module
//! is the post-v1 `kara-data` package, so `karac check` says that, in a lone
//! file and in a package without the dependency, rather than a bare
//! "unknown module" (review-thread ask, 2026-11-25).

use std::process::Command;

fn check(dir: &std::path::Path, arg: Option<&str>) -> String {
    let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
    c.arg("check").current_dir(dir);
    if let Some(a) = arg {
        c.arg(a);
    }
    let out = c.output().unwrap();
    assert!(!out.status.success());
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

const WANT: &str =
    "unknown module `data`: `data` is the `kara-data` package, which arrives after v1";

#[test]
fn a_data_import_names_the_kara_data_package() {
    let root = std::env::temp_dir().join(format!("karac-data-import-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let src = "import data.{Column};\nfn main() {\n    let c: Column[i64] = Column.new();\n}\n";
    // A lone file.
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("main.kara"), src).unwrap();
    let lone = check(&root, Some("main.kara"));
    assert!(lone.contains(WANT), "{lone}");
    // A package with no kara-data dependency.
    let pkg = root.join("pkg");
    std::fs::create_dir_all(pkg.join("src")).unwrap();
    std::fs::write(
        pkg.join("kara.toml"),
        "[package]\nname = \"p\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(pkg.join("src/main.kara"), src).unwrap();
    let in_pkg = check(&pkg, None);
    assert!(in_pkg.contains(WANT), "{in_pkg}");
    let _ = std::fs::remove_dir_all(&root);
}
