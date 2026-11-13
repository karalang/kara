//! A script inside a package but outside its `src/` gets the package's
//! dependencies wherever it is run from (design.md § 4, Finding the package,
//! B-2026-11-13-30). It was "this file belongs to no package": only the
//! package's `src/` modules were given the dependencies.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn fixture(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("karac-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let util = root.join("util");
    let app = root.join("app");
    std::fs::create_dir_all(util.join("src")).unwrap();
    std::fs::create_dir_all(app.join("src")).unwrap();
    std::fs::create_dir_all(app.join("scripts")).unwrap();
    std::fs::write(
        util.join("kara.toml"),
        "[package]\nname = \"util\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
    )
    .unwrap();
    std::fs::write(
        util.join("src/lib.kara"),
        "pub fn hello() -> String { \"hello from util\" }\n",
    )
    .unwrap();
    std::fs::write(
        app.join("kara.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n\
         [dependencies]\nutil = { path = \"../util\" }\n",
    )
    .unwrap();
    std::fs::write(app.join("src/main.kara"), "fn main() { }\n").unwrap();
    root
}

fn karac(args: &[&str], cwd: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap()
}

fn text(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned() + &String::from_utf8_lossy(&out.stdout)
}

const SCRIPT: &str = "import util.hello;\nfn main() { println(hello()); }\n";

#[test]
fn a_script_in_a_package_runs_with_its_dependencies_from_anywhere() {
    let root = fixture("script-deps-run");
    let app = root.join("app");
    std::fs::write(app.join("scripts/s.kara"), SCRIPT).unwrap();
    let from_pkg = karac(&["run", "scripts/s.kara"], &app);
    let script = app.join("scripts/s.kara");
    let from_outside = karac(&["run", script.to_str().unwrap()], &root);
    let _ = std::fs::remove_dir_all(&root);
    for out in [from_pkg, from_outside] {
        assert!(out.status.success(), "{}", text(&out));
        assert_eq!(String::from_utf8_lossy(&out.stdout), "hello from util\n");
    }
}

#[test]
fn a_script_in_a_package_is_checked_with_its_dependencies() {
    let root = fixture("script-deps-check");
    let app = root.join("app");
    std::fs::write(app.join("scripts/s.kara"), SCRIPT).unwrap();
    std::fs::write(
        app.join("scripts/bad.kara"),
        "import util.hello;\nfn main() { let n: i64 = hello(); println(n); }\n",
    )
    .unwrap();
    std::fs::write(
        app.join("scripts/nope.kara"),
        "import util.nope;\nfn main() { }\n",
    )
    .unwrap();
    let ok = karac(&["check", "scripts/s.kara"], &app);
    let json = karac(&["check", "--output=json", "scripts/s.kara"], &app);
    let bad = karac(&["check", "scripts/bad.kara"], &app);
    let nope = karac(&["run", "scripts/nope.kara"], &app);
    let _ = std::fs::remove_dir_all(&root);
    assert!(ok.status.success(), "{}", text(&ok));
    assert!(text(&ok).contains("All checks passed."), "{}", text(&ok));
    assert!(json.status.success(), "{}", text(&json));
    assert!(
        text(&json).contains("\"diagnostics\":[]"),
        "{}",
        text(&json)
    );
    // The script's own errors, not "belongs to no package".
    let bad = text(&bad);
    assert!(bad.contains("expected 'i64', found 'String'"), "{bad}");
    assert!(bad.contains("scripts/bad.kara:2:"), "{bad}");
    assert!(!bad.contains("belongs to no package"), "{bad}");
    assert!(!nope.status.success());
    let nope = text(&nope);
    assert!(nope.contains("error[E0113]"), "{nope}");
    assert!(nope.contains("available: hello"), "{nope}");
}

#[test]
fn a_script_does_not_take_the_package_s_own_errors() {
    // The package's `src/` is not part of the script's program, so an error
    // there is not the script's.
    let root = fixture("script-deps-own");
    let app = root.join("app");
    std::fs::write(app.join("scripts/s.kara"), SCRIPT).unwrap();
    std::fs::write(
        app.join("src/main.kara"),
        "fn main() { let n: i64 = \"no\"; println(n); }\n",
    )
    .unwrap();
    let run = karac(&["run", "scripts/s.kara"], &app);
    let check = karac(&["check", "scripts/s.kara"], &app);
    let _ = std::fs::remove_dir_all(&root);
    assert!(run.status.success(), "{}", text(&run));
    assert_eq!(String::from_utf8_lossy(&run.stdout), "hello from util\n");
    assert!(check.status.success(), "{}", text(&check));
}
