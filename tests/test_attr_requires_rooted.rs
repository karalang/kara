//! B-2026-11-16-284, B-2026-11-17-15: each `R` of `#[test(requires: [R])]`
//! must be program-rooted (core-semantics §14.13, "an error otherwise,
//! naming R"). A provider-rooted `R` surfaced as `E0419` inside the
//! generated test harness, at `<generated>`; an undeclared one skipped the
//! case as `unsatisfied_requires`; and the attribute on a plain `fn` passed
//! `karac check`. All three are now errors at the user's attribute. The
//! harness's own provider binding no longer draws `unused_mut`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn package(tag: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-test-requires-rooted-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("kara.toml"),
        "[package]\nname = \"tp\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    for (name, text) in files {
        std::fs::write(dir.join(name), text).unwrap();
    }
    dir
}

fn karac(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .env_remove("KARA_RESOURCE_DB")
        .env_remove("KARA_RESOURCE_STORE_USERDB")
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

const LIB: &str = "pub trait DbT {\n    fn get(ref self, k: i64) -> i64;\n}\n\
                   pub struct RealDb {}\n\
                   impl DbT for RealDb {\n    fn get(ref self, k: i64) -> i64 { k }\n}\n\
                   pub effect resource Db: DbT;\n\
                   fn main() {}\n";

#[test]
fn test_requires_provider_rooted_is_refused_at_the_attribute() {
    let test = "#[test(requires: [Db])]\ntest \"req\" {\n    assert(true);\n}\n";
    let dir = package(
        "provider",
        &[("src/main.kara", LIB), ("src/main_test.kara", test)],
    );
    for args in [&["test"][..], &["check"][..]] {
        let (ok, out) = karac(&dir, args);
        assert!(!ok, "{args:?}: {out}");
        assert!(out.contains("E_TEST_REQUIRES"), "{args:?}: {out}");
        assert!(out.contains("provider-rooted"), "{args:?}: {out}");
        assert!(out.contains("main_test.kara"), "{args:?}: {out}");
        assert!(!out.contains("<generated>"), "{args:?}: {out}");
        assert!(!out.contains("E0419"), "{args:?}: {out}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_requires_unknown_name_is_refused_not_skipped() {
    let test = "#[test(requires: [Nope])]\ntest \"x\" {\n    assert(true);\n}\n";
    let dir = package(
        "unknown",
        &[("src/main.kara", LIB), ("src/main_test.kara", test)],
    );
    let (ok, out) = karac(&dir, &["test"]);
    assert!(!ok, "{out}");
    assert!(out.contains("E_TEST_REQUIRES"), "{out}");
    assert!(out.contains("`Nope`, which is not a resource"), "{out}");
    assert!(!out.contains("unsatisfied_requires"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_requires_program_rooted_module_path_still_skips() {
    // `store.UserDB` names the resource `UserDB` of the module `store`,
    // declared with a default provider: program-rooted, so the case is
    // gated on `KARA_RESOURCE_STORE_USERDB` as before.
    let store = "pub trait Users {\n    fn count(ref self) -> i64;\n}\n\
                 pub struct Real {}\n\
                 impl Users for Real {\n    fn count(ref self) -> i64 { 3 }\n}\n\
                 pub effect resource UserDB: Users = Real {};\n";
    let test = "#[test(requires: [store.UserDB, Clock])]\ntest \"real\" {\n    assert(true);\n}\n";
    let dir = package(
        "rooted",
        &[
            ("src/main.kara", "fn main() {}\n"),
            ("src/store.kara", store),
            ("src/main_test.kara", test),
        ],
    );
    let (ok, out) = karac(&dir, &["test"]);
    assert!(ok, "{out}");
    assert!(out.contains("unsatisfied_requires"), "{out}");
    assert!(out.contains("store.UserDB"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_requires_wrong_module_path_is_unknown() {
    let test = "#[test(requires: [store.Db])]\ntest \"x\" {\n    assert(true);\n}\n";
    let dir = package(
        "wrongmod",
        &[("src/main.kara", LIB), ("src/main_test.kara", test)],
    );
    let (ok, out) = karac(&dir, &["check"]);
    assert!(!ok, "{out}");
    assert!(out.contains("E_TEST_REQUIRES"), "{out}");
    assert!(out.contains("in the module `store`"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_attribute_on_a_plain_fn_is_refused() {
    let dir = std::env::temp_dir().join(format!(
        "karac-test-requires-rooted-plainfn-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("t.kara");
    std::fs::write(
        &file,
        "#[test(requires: [Env])]\nfn t() {\n    assert(true);\n}\n\nfn main() {\n    t();\n}\n",
    )
    .unwrap();
    let (ok, out) = karac(&dir, &["check", "t.kara"]);
    assert!(!ok, "{out}");
    assert!(out.contains("E_TEST_ATTR_INVALID_TARGET"), "{out}");
    assert!(out.contains("t.kara:1:"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_with_provider_harness_draws_no_warning_of_its_own() {
    // A provider error in one case makes the check of the combined package
    // report; before the fix, the generated `let mut __karac_p0` of the
    // other case's `#[with_provider]` was reported with it as `unused_mut`
    // (W0295) at `<generated>`. Measured: the binary before the fix prints
    // that warning for exactly this package.
    let main = "import std.time.{ClockProvider};\npub struct FakeClock { t: i64 }\n\
                impl ClockProvider for FakeClock {\n    fn now(ref self) -> i64 { self.t }\n}\n\
                pub trait DbT {\n    fn get(ref self, k: i64) -> i64;\n}\n\
                pub effect resource Db: DbT;\n\
                fn main() {}\n";
    let test = "#[with_provider(Clock, FakeClock { t: 4 })]\n\
                test \"clock\" {\n    assert_eq(Clock.now(), 4);\n}\n\n\
                test \"bad\" {\n    assert_eq(Db.get(1), 1);\n}\n";
    let dir = package(
        "harness",
        &[("src/main.kara", main), ("src/main_test.kara", test)],
    );
    let (ok, out) = karac(&dir, &["test"]);
    assert!(!ok, "{out}");
    assert!(out.contains("E0419"), "{out}");
    assert!(!out.contains("W0295"), "{out}");
    assert!(!out.contains("__karac_p"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}
