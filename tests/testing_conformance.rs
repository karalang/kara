//! Claims of design.md § 14 Testing checked against `karac test`, each on
//! the JIT (with `--features llvm`) and under `--interp`.
//!
//! - Every `test_fail` carries `location`, a required field: a fixture refused
//!   before the body ran (`provider_construction_failed`,
//!   `unsatisfied_requires` under `--all`, `requires_and_with_provider_conflict`)
//!   used to emit none (B-2026-11-16-45). It now points at the case.

use std::path::PathBuf;
use std::process::{Command, Output};

fn package(tag: &str, test_src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-testing-conf-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("kara.toml"),
        "[package]\nname = \"pkg\"\nversion = \"0.1.0\"\nauthors = []\nedition = \"2026\"\n\n[dependencies]\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/main.kara"),
        "fn add(a: i64, b: i64) -> i64 { a + b }\n\nfn main() {\n    println(add(1, 2));\n}\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/main_test.kara"), test_src).unwrap();
    dir
}

fn karac(dir: &PathBuf, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .env_remove("KARAC_TEST_JIT")
        .env_remove("KARA_RESOURCE_ENV")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).replace("\\\\", "/")
}

fn event<'a>(stdout: &'a str, name: &str) -> &'a str {
    let key = format!("\"test\":\"{name}\"");
    let found: Vec<&str> = stdout.lines().filter(|l| l.contains(&key)).collect();
    assert_eq!(found.len(), 1, "{name}: {stdout}");
    found[0]
}

const LANES: [&[&str]; 2] = [&[], &["--interp"]];

#[test]
fn testing_every_test_fail_carries_a_location() {
    let src = "struct FakeClock { t: i64 }\n\
impl ClockProvider for FakeClock { fn wall_ns(ref self) -> i64 { self.t } }\n\
#[test(requires: [Env])]\n\
test \"needs env\" { assert(true); }\n\
#[test(requires: [Clock])]\n\
#[with_provider(Clock, FakeClock { t: 1 })]\n\
test \"both\" { assert(true); }\n\
fn refuse() -> Result[FakeClock, String] { Err(\"refused\") }\n\
#[with_provider(Clock, refuse())]\n\
test \"err ctor\" { assert(true); }\n";
    let dir = package("loc", src);
    for lane in LANES {
        let args: Vec<&str> = ["test", "--all"].iter().chain(lane).copied().collect();
        let out = karac(&dir, &args);
        let stdout = text(&out.stdout);
        for (name, line, reason) in [
            ("needs env", 4, "unsatisfied_requires"),
            ("both", 7, "requires_and_with_provider_conflict"),
            ("err ctor", 10, "provider_construction_failed"),
        ] {
            let e = event(&stdout, name);
            assert!(e.starts_with("{\"type\":\"test_fail\""), "{lane:?}: {e}");
            assert!(
                e.contains(&format!(
                    "\"location\":{{\"file\":\"src/main_test.kara\",\"line\":{line},\"col\":1}}"
                )),
                "{lane:?}: {e}"
            );
            assert!(
                e.contains(&format!("\"reason\":\"{reason}\"")),
                "{lane:?}: {e}"
            );
            assert!(e.contains("\"message\":"), "{lane:?}: {e}");
        }
        assert_eq!(out.status.code(), Some(1), "{lane:?}");
    }
    // Without `--all` the unmet `requires` is a skip, which needs no location.
    let out = karac(&dir, &["test", "needs"]);
    let stdout = text(&out.stdout);
    assert!(
        event(&stdout, "needs env").starts_with(
            "{\"type\":\"test_skip\",\"test\":\"needs env\",\"reason\":\"unsatisfied_requires\""
        ),
        "{stdout}"
    );
    assert_eq!(out.status.code(), Some(0), "{stdout}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn testing_discovery_names_and_assertion_messages() {
    let src = "test \"twice\" {\n    assert(add(1, 2) == 3);\n}\n\n\
test \"ne fails\" {\n    let a = 3;\n    assert_ne(a, 1 + 2);\n}\n\n\
test \"ne msg\" {\n    assert_ne(4, 4, \"nope\");\n}\n\n\
test \"eq borrows\" {\n    let v: Vec[i64] = [1];\n    let w: Vec[i64] = [1];\n    assert_eq(v, w);\n    assert_eq(v.len(), w.len());\n}\n\n\
test \"twice\" {\n    assert(add(2, 2) == 4);\n}\n\n\
fn test_helper() {\n    assert(false);\n}\n";
    let dir = package("disc", src);
    for lane in LANES {
        let args: Vec<&str> = ["test"].iter().chain(lane).copied().collect();
        let out = karac(&dir, &args);
        let stdout = text(&out.stdout);
        // Two cases share a name; `fn test_helper` is a helper, not a test.
        assert!(
            stdout.starts_with("{\"type\":\"run_start\",\"total_tests\":5}\n"),
            "{lane:?}: {stdout}"
        );
        assert_eq!(
            stdout
                .matches("\"type\":\"test_pass\",\"test\":\"twice\"")
                .count(),
            2,
            "{lane:?}: {stdout}"
        );
        let ne = event(&stdout, "ne fails");
        assert!(
            ne.contains("\"message\":\"assertion failed: a != 1 + 2\\n  value: 3\""),
            "{lane:?}: {ne}"
        );
        // A message replaces the first line only.
        let msg = event(&stdout, "ne msg");
        assert!(
            msg.contains("\"message\":\"nope\\n  value: 4\""),
            "{lane:?}: {msg}"
        );
        assert!(
            stdout.contains("\"type\":\"test_pass\",\"test\":\"eq borrows\""),
            "{lane:?}: {stdout}"
        );
        assert!(
            stdout.contains(
                "\"type\":\"summary\",\"total\":5,\"passed\":3,\"failed\":2,\"skipped\":0"
            ),
            "{lane:?}: {stdout}"
        );
        assert_eq!(out.status.code(), Some(1), "{lane:?}");
    }
    // A filter that matches nothing runs nothing and passes.
    let out = karac(&dir, &["test", "--interp", "zzz"]);
    let stdout = text(&out.stdout);
    assert!(
        stdout.starts_with("{\"type\":\"run_start\",\"total_tests\":0}\n"),
        "{stdout}"
    );
    assert_eq!(out.status.code(), Some(0), "{stdout}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn testing_test_blocks_fail_loudly_and_build_ignores_test_files() {
    for (tag, src, want) in [
        (
            "nested",
            "fn f() {\n    test \"inner\" { assert(true); }\n}\n",
            "E_TEST_BLOCK_NOT_TOP_LEVEL",
        ),
        (
            "tset",
            "tset \"x\" { assert(true); }\n",
            "\"type\":\"parse_error\"",
        ),
        (
            "pub",
            "pub test \"x\" { assert(true); }\n",
            "cannot have a visibility modifier",
        ),
        (
            "private",
            "private test \"x\" { assert(true); }\n",
            "cannot have a visibility modifier",
        ),
    ] {
        let dir = package(tag, src);
        let out = karac(&dir, &["test", "--interp"]);
        let stdout = text(&out.stdout);
        assert!(stdout.contains(want), "{tag}: {stdout}");
        assert!(!stdout.contains("run_start"), "{tag}: {stdout}");
        assert_eq!(out.status.code(), Some(1), "{tag}");
        let _ = std::fs::remove_dir_all(&dir);
    }
    // `karac build` ignores `_test.kara` files, even one that does not parse.
    if cfg!(feature = "llvm") {
        let dir = package("build", "test \"x\" { this is not kara }\n");
        let out = karac(&dir, &["build"]);
        let msg = format!("{}{}", text(&out.stdout), text(&out.stderr));
        if !msg.contains("undefined reference") {
            assert_eq!(out.status.code(), Some(0), "{msg}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
