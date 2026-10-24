//! `karac test` on v2 (M3 item 2, B-2026-10-22-13): the legacy runner's
//! discovery and JSONL events, each case run on MIR — the JIT by default, the
//! MIR interpreter under `--interp` — with `#[with_provider(R, ctor)]` running
//! the case inside `with_provider[R](ctor, || case())` (B-2026-10-21-5).

use std::path::PathBuf;
use std::process::{Command, Output};

fn package(tag: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-test-v2-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("kara.toml"),
        "[package]\nname = \"pkg\"\nversion = \"0.1.0\"\nauthors = []\nedition = \"2026\"\n\n[dependencies]\n",
    )
    .unwrap();
    for (path, text) in files {
        std::fs::write(dir.join("src").join(path), text).unwrap();
    }
    dir
}

fn karac_test(dir: &PathBuf, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .env_remove("KARAC_LEGACY")
        .env_remove("KARAC_TEST_JIT")
        .current_dir(dir)
        .arg("test")
        .args(args)
        .output()
        .unwrap()
}

/// Output as text, with a Windows path's JSON-escaped `\\` separators
/// written as `/` so one expectation holds on every host.
fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).replace("\\\\", "/")
}

/// The event for `name`: the one line whose `test` field is it.
fn event<'a>(stdout: &'a str, name: &str) -> &'a str {
    let key = format!("\"test\":\"{name}\"");
    let found: Vec<&str> = stdout.lines().filter(|l| l.contains(&key)).collect();
    assert_eq!(found.len(), 1, "{name}: {stdout}");
    found[0]
}

/// Both v2 executors.
const LANES: [&[&str]; 2] = [&[], &["--interp"]];

const MAIN: &str = "import util.{double};\n\nfn main() {\n    println(f\"{double(2)}\");\n}\n";

const UTIL: &str = "pub fn double(x: i64) -> i64 {\n    x * 2\n}\n\npub fn pick(v: ref Vec[i64], i: i64) -> i64 {\n    v[i as usize]\n}\n";

const UTIL_TEST: &str = "import util.{double, pick};\n\ntest \"double doubles\" {\n    assert_eq(double(2), 4);\n}\n\ntest \"double is wrong on purpose\" {\n    assert_eq(double(2), 5);\n}\n\ntest \"pick faults\" {\n    let mut v: Vec[i64] = Vec.new();\n    v.push(1);\n    let _ = pick(v, 3);\n}\n";

const MAIN_TEST: &str = "struct FakeClock { t: i64 }\n\nimpl ClockProvider for FakeClock {\n    fn wall_ns(ref self) -> i64 { self.t }\n}\n\nfn boom() -> FakeClock { panic(\"no clock\") }\n\n#[with_provider(Clock, FakeClock { t: 42 })]\ntest \"fake clock\" {\n    assert_eq(Clock.wall_ns(), 42);\n}\n\n#[with_provider(Clock, boom())]\ntest \"ctor fails\" {\n    assert_eq(Clock.wall_ns(), 1);\n}\n\n#[test(timeout_seconds: 1)]\ntest \"loops briefly\" {\n    let mut i = 0;\n    while true { i += 1; }\n}\n";

#[test]
fn test_v2_reports_each_case() {
    let dir = package(
        "cases",
        &[
            ("main.kara", MAIN),
            ("main_test.kara", MAIN_TEST),
            ("util.kara", UTIL),
            ("util_test.kara", UTIL_TEST),
        ],
    );
    for lane in LANES {
        let out = karac_test(&dir, lane);
        let stdout = text(&out.stdout);
        assert!(
            stdout.starts_with("{\"type\":\"run_start\",\"total_tests\":6}\n"),
            "{lane:?}: {stdout}"
        );
        // A fixture's provider is what the case's resource calls reach.
        assert!(
            event(&stdout, "fake clock").starts_with("{\"type\":\"test_pass\""),
            "{lane:?}: {stdout}"
        );
        assert!(event(&stdout, "double doubles").starts_with("{\"type\":\"test_pass\""));
        let wrong = event(&stdout, "double is wrong on purpose");
        assert!(
            wrong.starts_with("{\"type\":\"test_fail\""),
            "{lane:?}: {wrong}"
        );
        assert!(
            wrong.contains("\"location\":{\"file\":\"src/util_test.kara\",\"line\":8,\"col\":5}"),
            "{lane:?}: {wrong}"
        );
        assert!(wrong.contains("\"message\":\"assertion failed: left != right\""));
        // A fault in the code under test names that code's file.
        let pick = event(&stdout, "pick faults");
        assert!(
            pick.contains("\"location\":{\"file\":\"src/util.kara\",\"line\":6,\"col\":5}"),
            "{lane:?}: {pick}"
        );
        assert!(
            pick.contains("\"message\":\"index out of bounds\""),
            "{lane:?}: {pick}"
        );
        let ctor = event(&stdout, "ctor fails");
        assert!(
            ctor.contains("\"reason\":\"provider_construction_failed\",\"resource\":\"Clock\""),
            "{lane:?}: {ctor}"
        );
        assert!(ctor.contains("no clock"), "{lane:?}: {ctor}");
        assert!(
            event(&stdout, "loops briefly").starts_with("{\"type\":\"test_timeout\""),
            "{lane:?}: {stdout}"
        );
        assert!(
            stdout.contains(
                "\"type\":\"summary\",\"total\":6,\"passed\":2,\"failed\":4,\"skipped\":0"
            ),
            "{lane:?}: {stdout}"
        );
        assert_eq!(out.status.code(), Some(1), "{lane:?}");
    }
}

/// A passing suite exits 0, and the filter picks cases by name.
#[test]
fn test_v2_passes_and_filters() {
    let dir = package(
        "pass",
        &[
            ("main.kara", MAIN),
            ("util.kara", UTIL),
            ("util_test.kara", UTIL_TEST),
        ],
    );
    for lane in LANES {
        let mut args = lane.to_vec();
        args.push("doubles");
        let out = karac_test(&dir, &args);
        let stdout = text(&out.stdout);
        assert!(
            stdout.starts_with("{\"type\":\"run_start\",\"total_tests\":1}\n"),
            "{stdout}"
        );
        assert!(stdout.contains("\"passed\":1,\"failed\":0"), "{stdout}");
        assert_eq!(out.status.code(), Some(0), "{lane:?}");
    }
}

/// Code in a test file that only the v2 rules reject (the tree check before
/// them is legacy's) fails the run before any case runs, with the diagnostic
/// naming the test file.
#[test]
fn test_v2_check_errors_stop_the_run() {
    let bad = format!("{UTIL_TEST}\nfn keep(v: Vec[i64]) -> Vec[i64] {{\n    v\n}}\n");
    let dir = package(
        "check",
        &[
            ("main.kara", MAIN),
            ("util.kara", UTIL),
            ("util_test.kara", &bad),
        ],
    );
    let out = karac_test(&dir, &[]);
    let stdout = text(&out.stdout);
    assert!(!stdout.contains("run_start"), "{stdout}");
    assert!(
        stdout.contains("\"file\":\"src/util_test.kara\",\"line\":18"),
        "{stdout}"
    );
    assert_eq!(out.status.code(), Some(1));
}

/// `--legacy` and `KARAC_LEGACY=1` reach the legacy runner, whose
/// `assert_eq` failure carries `left` and `right`.
#[test]
fn test_v2_legacy_flag_and_env_reach_legacy() {
    let dir = package(
        "legacy",
        &[
            ("main.kara", MAIN),
            ("util.kara", UTIL),
            ("util_test.kara", UTIL_TEST),
        ],
    );
    let v2 = karac_test(&dir, &["wrong"]);
    assert!(
        !text(&v2.stdout).contains("\"left\":"),
        "{}",
        text(&v2.stdout)
    );
    let flag = karac_test(&dir, &["--legacy", "wrong"]);
    let env = Command::new(env!("CARGO_BIN_EXE_karac"))
        .env("KARAC_LEGACY", "1")
        .current_dir(&dir)
        .args(["test", "wrong"])
        .output()
        .unwrap();
    for out in [flag, env] {
        let stdout = text(&out.stdout);
        assert!(
            stdout.contains("\"left\":\"4\",\"right\":\"5\""),
            "{stdout}"
        );
        assert_eq!(out.status.code(), Some(1));
    }
}

/// A fixture's constructor that is a path naming a function is called, and a
/// `Result` it returns is unwrapped, its `Err` failing the case as
/// `provider_construction_failed` (core semantics §14.8).
#[test]
fn test_v2_fixture_paths_and_results() {
    let fixtures = "struct FakeClock { t: i64 }\n\nimpl FakeClock {\n    fn new() -> FakeClock { FakeClock { t: 7 } }\n    fn try_new(ok: bool) -> Result[FakeClock, String] {\n        if ok { Ok(FakeClock { t: 9 }) } else { Err(\"refused\") }\n    }\n}\n\nimpl ClockProvider for FakeClock {\n    fn wall_ns(ref self) -> i64 { self.t }\n}\n\n#[with_provider(Clock, FakeClock.new)]\ntest \"path\" {\n    assert_eq(Clock.wall_ns(), 7);\n}\n\n#[with_provider(Clock, FakeClock.try_new(true))]\ntest \"ok result\" {\n    assert_eq(Clock.wall_ns(), 9);\n}\n\n#[with_provider(Clock, FakeClock.try_new(false))]\ntest \"err result\" {\n    assert_eq(Clock.wall_ns(), 9);\n}\n";
    let dir = package(
        "fixtures",
        &[
            ("main.kara", MAIN),
            ("main_test.kara", fixtures),
            ("util.kara", UTIL),
        ],
    );
    for lane in LANES {
        let out = karac_test(&dir, lane);
        let stdout = text(&out.stdout);
        assert!(
            event(&stdout, "path").starts_with("{\"type\":\"test_pass\""),
            "{lane:?}: {stdout}"
        );
        assert!(
            event(&stdout, "ok result").starts_with("{\"type\":\"test_pass\""),
            "{lane:?}: {stdout}"
        );
        let err = event(&stdout, "err result");
        assert!(
            err.contains("\"reason\":\"provider_construction_failed\",\"resource\":\"Clock\""),
            "{lane:?}: {err}"
        );
        assert_eq!(out.status.code(), Some(1), "{lane:?}");
    }
}
