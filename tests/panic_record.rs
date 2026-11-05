//! The panic record's kind (design.md § Panic record, B-2026-11-05-18).
//! Under `--output=json` / `jsonl`, `karac run` asks the program for the
//! record and folds its kind into the `runtime_error` event, and `karac
//! test` puts it on a `test_fail`. In text mode, and in a built binary run
//! directly, stderr has the human report alone. `expect("x")` and
//! `panic("x")` print the same text, so the kind comes from the panic site,
//! not from the message.

use std::path::{Path, PathBuf};
use std::process::Command;

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-panic-record-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Each kind a v2 program can raise today, and a program that raises it.
const CASES: &[(&str, &str)] = &[
    ("explicit", "fn main() {\n    panic(\"wanted\");\n}\n"),
    (
        "unwrap",
        "fn n() -> Result[i64, String] {\n    Err(\"bad\")\n}\n\nfn main() {\n    println(n().expect(\"wanted\"));\n}\n",
    ),
    (
        "unwrap",
        "fn n() -> Option[i64] {\n    None\n}\n\nfn main() {\n    println(n().unwrap());\n}\n",
    ),
    (
        "index_out_of_bounds",
        "fn main() {\n    let v: Vec[i64] = [1];\n    let i = v.len() + 2;\n    println(v[i]);\n}\n",
    ),
    (
        "index_out_of_bounds",
        "fn main() {\n    let s = \"abc\";\n    let i = s.len() + 3;\n    println(s[0..i]);\n}\n",
    ),
    (
        "overflow",
        "fn one() -> i64 {\n    1\n}\n\nfn main() {\n    let x: i64 = 9223372036854775807;\n    println(x + one());\n}\n",
    ),
    (
        "div_by_zero",
        "fn z() -> i64 {\n    0\n}\n\nfn main() {\n    println(10 / z());\n}\n",
    ),
    ("assertion", "fn main() {\n    let a = 1;\n    assert_eq(a, 2);\n}\n"),
    ("unreachable", "fn main() {\n    unreachable(\"nope\");\n}\n"),
];

/// `karac run` with `args` on `src`: (exit code, stdout, stderr).
fn run(src: &str, args: &[&str]) -> (Option<i32>, String, String) {
    let dir = scratch("run");
    std::fs::write(dir.join("main.kara"), src).unwrap();
    let out = karac()
        .arg("run")
        .args(args)
        .arg("main.kara")
        .current_dir(&dir)
        .env_remove("KARAC_PANIC_RECORD")
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn check_kinds(extra: &[&str]) {
    let mut wrong = Vec::new();
    for (kind, src) in CASES {
        let mut args = vec!["--output=jsonl"];
        args.extend_from_slice(extra);
        let (code, stdout, stderr) = run(src, &args);
        let want = format!("\"kind\":\"{kind}\"");
        let event = stdout
            .lines()
            .find(|l| l.starts_with("{\"type\":\"runtime_error\""));
        if code != Some(101)
            || !event.is_some_and(|e| e.contains(&want))
            || stderr.contains("panic_record")
        {
            wrong.push(format!(
                "{kind} {extra:?}: exit {code:?}\n{stdout}\n{stderr}"
            ));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n---\n"));
}

#[test]
fn a_runtime_error_event_carries_the_panic_kind() {
    check_kinds(&[]);
}

#[test]
fn the_interpreter_reports_the_panic_kind() {
    check_kinds(&["--interp"]);
}

#[test]
fn text_mode_prints_the_human_report_alone() {
    for (_, src) in CASES {
        for extra in [&[][..], &["--interp"][..]] {
            let (code, _, stderr) = run(src, extra);
            assert_eq!(code, Some(101), "{stderr}");
            assert!(stderr.contains("runtime error: "), "{stderr}");
            assert!(!stderr.contains('{'), "{stderr}");
        }
    }
}

#[test]
fn json_output_puts_the_kind_on_the_runtime_error() {
    let (_, stdout, _) = run(CASES[1].1, &["--output=json"]);
    assert!(
        stdout.contains("\"message\":\"wanted\"") && stdout.contains("\"kind\":\"unwrap\""),
        "{stdout}"
    );
}

fn package(dir: &Path) {
    std::fs::write(dir.join("kara.toml"), "[package]\nname = \"app\"\n").unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("src/main.kara"),
        "pub fn n() -> Option[i64] {\n    None\n}\n\nfn main() { }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/main_test.kara"),
        "test \"unwraps\" {\n    let x = n().expect(\"needed\");\n    assert(x == 1);\n}\n\n\
         test \"asserts\" {\n    assert(1 == 2);\n}\n\ntest \"passes\" {\n    assert(1 == 1);\n}\n",
    )
    .unwrap();
}

#[test]
fn a_failed_test_carries_the_panic_kind() {
    let dir = scratch("test");
    package(&dir);
    let out = karac()
        .arg("test")
        .current_dir(&dir)
        .env_remove("KARAC_PANIC_RECORD")
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let fail = |name: &str| {
        stdout
            .lines()
            .find(|l| {
                l.contains("\"type\":\"test_fail\"") && l.contains(&format!("\"test\":\"{name}\""))
            })
            .unwrap_or_default()
            .to_string()
    };
    assert!(fail("unwraps").contains("\"kind\":\"unwrap\""), "{stdout}");
    assert!(
        fail("asserts").contains("\"kind\":\"assertion\""),
        "{stdout}"
    );
    assert!(stdout.contains("\"passed\":1"), "{stdout}");
}

/// A built binary is never asked for the record, whatever its environment.
#[cfg(feature = "llvm")]
#[test]
fn a_built_binary_prints_the_human_report_alone() {
    let dir = scratch("build");
    std::fs::write(dir.join("main.kara"), CASES[1].1).unwrap();
    let built = karac()
        .args(["build", "main.kara", "-o", "prog"])
        .current_dir(&dir)
        .output()
        .unwrap();
    if !built.status.success() {
        let _ = std::fs::remove_dir_all(&dir);
        let text = String::from_utf8_lossy(&built.stderr);
        // A checkout without the runtime archives cannot link.
        assert!(text.contains("libkarac_runtime"), "{text}");
        return;
    }
    let out = Command::new(dir.join("prog"))
        .env("KARAC_PANIC_RECORD", "1")
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(101), "{stderr}");
    assert!(stderr.contains("runtime error: wanted"), "{stderr}");
    assert!(!stderr.contains("panic_record"), "{stderr}");
}
