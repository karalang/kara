//! PK1 (probe tool2, pkg_mk.py case reg3): `karac check` resolves the
//! package's `[dev-dependencies]` as `karac test` will, and reports one
//! that cannot resolve as check's own diagnostic, with or without a
//! `_test.kara` in the package; `karac run` does not resolve them.

use std::process::Command;

fn package(tag: &str, manifest: &str, test_file: bool) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-pk1-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("kara.toml"), manifest).unwrap();
    std::fs::write(
        dir.join("src/main.kara"),
        "fn main() {\n    println(1);\n}\n",
    )
    .unwrap();
    if test_file {
        std::fs::write(
            dir.join("src/main_test.kara"),
            "test \"t\" { assert(true); }\n",
        )
        .unwrap();
    }
    dir
}

fn karac(dir: &std::path::Path, args: &[&str]) -> (Option<i32>, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .env_remove("KARAC_REGISTRY_PROXY")
        .output()
        .unwrap();
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn check_reports_an_unresolvable_dev_dependency() {
    let registry = "[package]\nname = \"app\"\n[dev-dependencies]\nproptest = \"0.4\"\n";
    for test_file in [true, false] {
        let dir = package(&format!("reg-{test_file}"), registry, test_file);
        let (code, stdout, stderr) = karac(&dir, &["check"]);
        assert_eq!(code, Some(1), "{test_file}: {stdout}{stderr}");
        assert!(
            stderr.contains("error[E_REGISTRY_DEP_UNSUPPORTED]: registry dependency `proptest`"),
            "{test_file}: {stderr}"
        );
        assert!(stderr.contains("= help:"), "{stderr}");
        assert!(!stdout.contains("dep_resolution_error"), "{stdout}");
        let (code, stdout, _) = karac(&dir, &["check", "--output=json"]);
        assert_eq!(code, Some(1));
        assert!(stdout.starts_with("{\"status\":\"error\""), "{stdout}");
        assert!(
            stdout.contains("\"code\":\"E_REGISTRY_DEP_UNSUPPORTED\""),
            "{stdout}"
        );
        assert!(!stdout.contains("dep_resolution_error"), "{stdout}");
        let _ = std::fs::remove_dir_all(&dir);
    }
    let missing =
        "[package]\nname = \"app\"\n[dev-dependencies]\nq = { path = \"../karac-pk1-none\" }\n";
    let dir = package("path", missing, false);
    let (code, _, stderr) = karac(&dir, &["check"]);
    assert_eq!(code, Some(1), "{stderr}");
    assert!(stderr.contains("error[E_PATH_DEP_NOT_FOUND]"), "{stderr}");
    let (code, stdout, stderr) = karac(&dir, &["run"]);
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    let _ = std::fs::remove_dir_all(&dir);
}
