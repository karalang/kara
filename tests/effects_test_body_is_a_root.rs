//! B-2026-11-17-102: a test body is a root that supplies nothing, as `main`
//! is (core-semantics §14.6). A test that uses a provider-rooted resource
//! with no provider bound was refused with `E0419` at `<generated>`, on the
//! harness's call of the case, with a message naming `main`. It is now
//! refused at the use in the test body, naming the test body; a test whose
//! `#[with_provider]` binds the resource still passes.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn package(tag: &str, test_file: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("karac-test-body-root-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("kara.toml"),
        "[package]\nname = \"tp\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/main.kara"),
        "pub trait DbT {\n    fn get(ref self, k: i64) -> i64;\n}\n\
         pub effect resource Db: DbT;\n\n\
         fn main() {\n    println(\"hi\");\n}\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/main_test.kara"), test_file).unwrap();
    dir
}

fn karac_test(dir: &Path) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("test")
        .current_dir(dir)
        .stdin(Stdio::null())
        .env_remove("KARAC_LEGACY")
        .env_remove("KARAC_TEST_JIT")
        .output()
        .expect("karac");
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// The `E0419` diagnostics in `out`, as `file:line:column`.
fn no_provider_sites(out: &str) -> Vec<String> {
    out.lines()
        .filter(|l| l.contains("\"code\":\"E_NO_PROVIDER\",\"parent_code\":\"E0419\""))
        .map(|l| {
            let field = |k: &str| {
                let at = l.find(&format!("\"{k}\":")).unwrap() + k.len() + 3;
                l[at..]
                    .split([',', '}'])
                    .next()
                    .unwrap()
                    .trim_matches('"')
                    .to_string()
            };
            format!("{}:{}:{}", field("file"), field("line"), field("column"))
        })
        .collect()
}

#[test]
fn a_test_body_without_a_provider_is_refused_at_its_use() {
    let dir = package(
        "use",
        "fn helper() -> i64 {\n    Db.get(2)\n}\n\n\
         test \"bad\" {\n    assert_eq(Db.get(1), 1);\n}\n\n\
         test \"via helper\" {\n    assert_eq(helper(), 2);\n}\n\n\
         test \"fine\" {\n    assert_eq(1, 1);\n}\n",
    );
    let (ok, out) = karac_test(&dir);
    assert!(!ok, "{out}");
    let sites = no_provider_sites(&out);
    assert_eq!(sites.len(), 2, "{out}");
    assert!(
        sites.iter().all(|s| s.contains("main_test.kara")),
        "{sites:?}\n{out}"
    );
    // The direct use, and the call of the helper that uses it.
    assert!(sites.iter().any(|s| s.contains(":6:")), "{sites:?}");
    assert!(sites.iter().any(|s| s.contains(":10:")), "{sites:?}");
    assert!(!out.contains("<generated>"), "{out}");
    assert!(
        out.contains("since a test body takes no hidden provider"),
        "{out}"
    );
    assert!(!out.contains("since `main` takes"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_test_body_whose_with_provider_binds_the_resource_passes() {
    let dir = package(
        "bound",
        "struct FakeDb { base: i64 }\n\n\
         impl DbT for FakeDb {\n    fn get(ref self, k: i64) -> i64 { self.base + k }\n}\n\n\
         #[with_provider(Db, FakeDb { base: 10 })]\n\
         test \"bound\" {\n    assert_eq(Db.get(1), 11);\n}\n",
    );
    let (ok, out) = karac_test(&dir);
    assert!(ok, "{out}");
    assert!(no_provider_sites(&out).is_empty(), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn main_without_a_provider_still_names_main() {
    let dir = package("main", "test \"fine\" {\n    assert_eq(1, 1);\n}\n");
    std::fs::write(
        dir.join("src/main.kara"),
        "pub trait DbT {\n    fn get(ref self, k: i64) -> i64;\n}\n\
         pub effect resource Db: DbT;\n\n\
         fn main() {\n    println(f\"{Db.get(1)}\");\n}\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", "src/main.kara"])
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success(), "{text}");
    assert!(
        text.contains("since `main` takes no hidden provider"),
        "{text}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
