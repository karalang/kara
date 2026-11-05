//! B-2026-11-05-17, the small items of the §17 roll-up: lint-level edge cases
//! (design.md § Lint levels) and `karac explain` on the spec's symbolic codes.

use std::process::{Command, Stdio};

fn karac(args: &[&str], tag: &str, src: &str) -> (Option<i32>, String, String) {
    let path = std::env::temp_dir().join(format!("karac-lee-{tag}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

const QUIET_EXPECT: &str =
    "fn f() {\n    #[expect(deprecated)]\n    let x = 1;\n    println(x);\n}\n\
fn main() { f(); }\n";

#[test]
fn allow_unfulfilled_on_the_function_covers_a_statement_expect() {
    let src = format!("#[allow(unfulfilled_lint_expectation)]\n{QUIET_EXPECT}");
    let (code, _, err) = karac(&["check"], "allow_fn", &src);
    assert_eq!(code, Some(0), "{err}");
    assert!(!err.contains("unfulfilled_lint_expectation"), "{err}");
}

#[test]
fn an_unfulfilled_statement_expect_still_warns_once() {
    let (code, _, err) = karac(&["check"], "plain", QUIET_EXPECT);
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(
        err.matches("warning[unfulfilled_lint_expectation]").count(),
        1,
        "one label, not two: {err}"
    );
}

#[test]
fn allow_unfulfilled_in_an_impl_covers_a_method_statement_expect() {
    let src =
        "struct S {}\n#[allow(unfulfilled_lint_expectation)]\nimpl S {\n    fn m(self) {\n        \
               #[expect(deprecated)]\n        let x = 1;\n        println(x);\n    }\n}\n\
               fn main() { let s = S {}; s.m(); }\n";
    let (code, _, err) = karac(&["check"], "allow_impl", src);
    assert_eq!(code, Some(0), "{err}");
    assert!(!err.contains("unfulfilled_lint_expectation"), "{err}");
}

#[test]
fn a_cli_flag_naming_no_lint_warns() {
    let src = "fn main() { println(1); }\n";
    let (code, _, err) = karac(&["check", "-D", "no_such_lint"], "flag", src);
    assert_eq!(code, Some(0), "{err}");
    assert!(
        err.contains("warning[unknown_lint]: `-D no_such_lint` names no lint"),
        "{err}"
    );
    let (_, _, err) = karac(
        &["check", "-D", "warnings", "-A", "deprecated"],
        "known",
        src,
    );
    assert!(!err.contains("unknown_lint"), "{err}");
}

#[test]
fn an_unknown_lint_attribute_has_one_level_in_its_label() {
    let src = "#[allow(no_such_lint)]\nfn f() {}\nfn main() { f(); }\n";
    let (_, _, err) = karac(&["check"], "attr", src);
    assert!(err.contains("warning[unknown_lint]"), "{err}");
    assert!(
        !err.contains("error[E_UNKNOWN_LINT]"),
        "a warning says it is one: {err}"
    );
}

#[test]
fn a_duplicate_lint_level_has_its_own_code() {
    let src = "#[allow(deprecated, deprecated)]\nfn f() {}\nfn main() { f(); }\n";
    let (code, out, _) = karac(&["check", "--output=json"], "dup", src);
    assert_eq!(code, Some(1), "{out}");
    assert!(out.contains("\"code\":\"E0006\""), "{out}");
}

#[test]
fn explain_answers_the_spec_s_symbolic_codes() {
    for (code, kind) in [
        ("E_NOT_CROSS_TASK", "CrossTaskUnsafeCapture"),
        ("E_DUPLICATE_LINT_LEVEL", "DuplicateLintLevel"),
        ("E_UNKNOWN_ATTRIBUTE", "UnknownAttribute"),
        ("E_FORBIDDEN_LINT_ALLOW", "ForbiddenLintAllow"),
        ("E_EXPECT_ON_UNFULFILLED", "ExpectOnUnfulfilled"),
        ("E_MODULE_LET", "ModuleLet"),
        ("E0006", "DuplicateLintLevel"),
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_karac"))
            .args(["explain", code])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{code}: {text}{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(text.contains(kind), "{code}: {text}");
    }
    // A class name still reads as a class.
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["explain", "EFFECT_UNDECLARED"])
        .output()
        .unwrap();
    assert!(out.status.success());
}

#[test]
fn package_json_carries_program_effects() {
    let dir = std::env::temp_dir().join(format!("karac-lee-pkg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("kara.toml"), "[package]\nname = \"demo\"\n").unwrap();
    std::fs::write(
        dir.join("src/main.kara"),
        "import util;\nfn main() { let x: i32 = \"a\"; util.hi(); println(x); }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/util.kara"),
        "pub fn hi() with writes(Stdout) { println(1); }\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(&dir)
        .args(["build", "--output=json"])
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(text.starts_with("{\"program_effects\":"), "{text}");
}

/// 25 type errors in one file: 20 primaries and a `truncated` marker for the
/// other 5, the same in the batch document and the stream (design.md §17
/// rule 3 and invariant 4).
#[test]
fn the_cascade_cap_stops_a_file_at_twenty_primary_errors() {
    let mut src = String::from("#[deprecated]\nfn old() {}\nfn main() {\n");
    for i in 0..25 {
        src.push_str(&format!("    let x{i}: i64 = \"s\";\n"));
    }
    src.push_str("    old();\n}\n");
    let (code, batch, _) = karac(&["check", "--output=json"], "cap", &src);
    assert_eq!(code, Some(1), "{batch}");
    assert_eq!(batch.matches("\"code\":\"E0200\"").count(), 20, "{batch}");
    assert!(
        batch.contains("{\"phase\":\"typecheck\",\"truncated\":{\"file\":"),
        "{batch}"
    );
    assert!(
        batch.contains("\"suppressed_primary_errors\":5}"),
        "{batch}"
    );
    assert!(
        batch.contains("\"severity\":\"warning\""),
        "a warning is never capped: {batch}"
    );

    let (code, stream, _) = karac(&["check", "--output=jsonl"], "cap", &src);
    assert_eq!(code, Some(1), "{stream}");
    let marker = stream
        .lines()
        .find(|l| l.contains("\"truncated\""))
        .expect("the stream carries the marker");
    assert!(
        marker.starts_with("{\"type\":\"diagnostic\",\"phase\":\"typecheck\","),
        "{marker}"
    );
    assert!(
        marker.contains("\"suppressed_primary_errors\":5}"),
        "{marker}"
    );
    let typecheck = stream
        .lines()
        .find(|l| l.contains("\"phase_complete\"") && l.contains("\"typecheck\""))
        .unwrap();
    assert!(typecheck.contains("\"errors\":20,"), "{typecheck}");
}

#[test]
fn emit_timings_writes_the_phase_times_to_a_file() {
    let out = std::env::temp_dir().join(format!("karac-lee-timings-{}.json", std::process::id()));
    let flag = format!("--emit-timings={}", out.display());
    let (code, stdout, err) = karac(
        &["check", &flag, "--output=json"],
        "timings",
        "fn main() { println(1); }\n",
    );
    let written = std::fs::read_to_string(&out).unwrap_or_default();
    let _ = std::fs::remove_file(&out);
    assert_eq!(code, Some(0), "{stdout}{err}");
    assert!(
        stdout.starts_with("{\"program_effects\":"),
        "stdout is the document alone: {stdout}"
    );
    assert!(written.starts_with("{\"phases\":[{\"phase\":"), "{written}");
    assert!(written.contains("\"total_ms\":"), "{written}");
}
