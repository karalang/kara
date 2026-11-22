//! B-2026-11-21-77: `karac check` runs under the manifest's profile, as
//! `karac run` and `karac build` do, for a one-file package, a file named in
//! a package, a `--targets` sweep and the package's late phases.
//! B-2026-11-21-78: a path dependency's `version` requirement is read
//! against the dependency's own `[package].version`, and the conflict note
//! names the parent package.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-mf-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn karac(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
    )
}

const HARD_ALLOC: &str =
    "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[profile]\npanic_on_alloc_failure = false\n";
const PUSHES: &str = "fn main() {\n    let mut v: Vec[i64] = Vec.new();\n    v.push(1);\n    println(f\"{v.len()}\");\n}\n";

#[test]
fn check_runs_under_the_manifest_profile() {
    for (tag, extra) in [("lone", false), ("multi", true)] {
        let dir = scratch(tag);
        write(&dir.join("kara.toml"), HARD_ALLOC);
        write(&dir.join("src/main.kara"), PUSHES);
        if extra {
            write(&dir.join("src/u.kara"), "pub fn f() -> i64 { 1 }\n");
        }
        for args in [
            &["check"][..],
            &["check", "src/main.kara"][..],
            &["check", "--targets=native"][..],
        ] {
            let (ok, out) = karac(&dir, args);
            assert!(!ok, "{tag} {args:?}: {out}");
            assert!(
                out.contains("E0264") && out.contains("Vec.try_push"),
                "{tag} {args:?}: {out}"
            );
            assert!(
                !out.contains(dir.to_str().unwrap()),
                "{tag} {args:?}: {out}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn path_dependency_requirement_reads_its_declared_version() {
    let dir = scratch("pathver");
    write(
        &dir.join("kara.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nutil = { path = \"util\", version = \">=1.0\" }\n",
    );
    write(
        &dir.join("src/main.kara"),
        "import util.f;\nfn main() { println(f()); }\n",
    );
    write(
        &dir.join("util/kara.toml"),
        "[package]\nname = \"util\"\nversion = \"1.5.0\"\n",
    );
    write(&dir.join("util/src/lib.kara"), "pub fn f() -> i64 { 7 }\n");
    let (ok, out) = karac(&dir, &["check"]);
    assert!(ok, "{out}");
    let (ok, out) = karac(&dir, &["run", "--interp"]);
    assert!(ok && out.contains('7'), "{out}");
    let manifest = std::fs::read_to_string(dir.join("kara.toml")).unwrap();
    write(&dir.join("kara.toml"), &manifest.replace(">=1.0", ">=2.0"));
    let (ok, out) = karac(&dir, &["check"]);
    assert!(!ok, "{out}");
    assert!(
        out.contains("E_DEPENDENCY_VERSION_CONFLICT") && out.contains("candidate `1.5.0`"),
        "{out}"
    );
    assert!(
        out.contains("package `app` requires `util = \">=2.0\"`"),
        "{out}"
    );
    assert!(!out.contains("app/kara.toml"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}
