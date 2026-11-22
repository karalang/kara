//! B-2026-11-21-73: parse diagnostics whose help names one determined edit,
//! which the fix coverage audit (2026-11-21) found carrying none. Each now
//! carries that edit, and each pin applies it with `karac fix` and checks
//! the result clean, as `tests/fix_coverage_source_fixes.rs` does for the
//! later phases: the first `check --output=json` record has a `replacement`
//! or `fix_diff`, the fixed text is what the help describes, and it checks
//! with no diagnostic (and, where it runs, prints what the original meant).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn karac(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn scratch(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("karac-parse-fix-{name}-{}", std::process::id()))
}

struct Mended {
    /// Whether the first check's record for `code` carried an edit.
    fixable: bool,
    /// The first check's JSON, for failure messages.
    first: String,
    /// The text `karac fix` leaves.
    text: String,
    /// Whether that text checks with no diagnostic at all.
    clean: bool,
    /// The fixed check's output, for failure messages.
    after: String,
    /// What the fixed text prints under the MIR interpreter.
    out: String,
}

fn mend(name: &str, code: &str, src: &str) -> Mended {
    let dir = scratch(name);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("cell.kara");
    std::fs::write(&file, src).unwrap();
    let (_, first, _) = karac(&dir, &["check", "--output=json", "cell.kara"]);
    let json: serde_json::Value = serde_json::from_str(&first).expect("one JSON document");
    let fixable = json["diagnostics"].as_array().unwrap().iter().any(|d| {
        d["code"] == code
            && (d.get("replacement").is_some_and(|r| !r.is_null())
                || d.get("fix_diff").is_some_and(|r| !r.is_null()))
    });
    karac(&dir, &["fix", "cell.kara"]);
    let text = std::fs::read_to_string(&file).unwrap();
    let (_, after, after_err) = karac(&dir, &["check", "--output=json", "cell.kara"]);
    let clean = serde_json::from_str::<serde_json::Value>(&after)
        .ok()
        .and_then(|j| j["diagnostics"].as_array().map(|d| d.is_empty()))
        .unwrap_or(false);
    let (_, out, _) = karac(&dir, &["__mir-run", "cell.kara"]);
    let _ = std::fs::remove_dir_all(&dir);
    Mended {
        fixable,
        first,
        text,
        clean,
        after: format!("{after}{after_err}"),
        out,
    }
}

fn assert_mended(m: &Mended, fixed: &str, out: Option<&str>) {
    assert!(m.fixable, "no edit on the first record:\n{}", m.first);
    assert_eq!(m.text, fixed);
    assert!(m.clean, "the fixed text does not check clean:\n{}", m.after);
    if let Some(out) = out {
        assert_eq!(m.out, out);
    }
}

#[test]
fn parse_fix_with_underscore_is_deleted() {
    let m = mend(
        "withu",
        "E0005",
        "fn apply(f: Fn(i64) -> i64 with _, x: i64) -> i64 {\n    f(x)\n}\n\nfn main() {\n    println(apply(|v| v + 1, 2));\n}\n",
    );
    assert_mended(
        &m,
        "fn apply(f: Fn(i64) -> i64, x: i64) -> i64 {\n    f(x)\n}\n\nfn main() {\n    println(apply(|v| v + 1, 2));\n}\n",
        Some("3\n"),
    );
}

#[test]
fn parse_fix_track_caller_loses_its_arguments() {
    let m = mend(
        "tcargs",
        "E_TRACK_CALLER_ARGS_NOT_PERMITTED",
        "#[track_caller(always)]\nfn first(v: Vec[i64]) -> i64 {\n    v[0]\n}\n\nfn main() {\n    println(first([7]));\n}\n",
    );
    assert_mended(
        &m,
        "#[track_caller]\nfn first(v: Vec[i64]) -> i64 {\n    v[0]\n}\n\nfn main() {\n    println(first([7]));\n}\n",
        Some("7\n"),
    );
}

#[test]
fn parse_fix_gpu_loses_its_arguments() {
    let m = mend(
        "gpuargs",
        "E_GPU_ARGS_NOT_PERMITTED",
        "#[allow(later_track)]\n#[gpu(fast)]\nfn twice(x: i64) -> i64 {\n    x * 2\n}\n\nfn main() {\n    println(twice(4));\n}\n",
    );
    assert_mended(
        &m,
        "#[allow(later_track)]\n#[gpu]\nfn twice(x: i64) -> i64 {\n    x * 2\n}\n\nfn main() {\n    println(twice(4));\n}\n",
        Some("8\n"),
    );
}

#[test]
fn parse_fix_cold_loses_its_arguments() {
    let m = mend(
        "coldargs",
        "E_MALFORMED_ATTRIBUTE_ARGS",
        "#[cold(rarely)]\nfn fail_code() -> i64 {\n    1\n}\n\nfn main() {\n    println(fail_code());\n}\n",
    );
    assert_mended(
        &m,
        "#[cold]\nfn fail_code() -> i64 {\n    1\n}\n\nfn main() {\n    println(fail_code());\n}\n",
        Some("1\n"),
    );
}

#[test]
fn parse_fix_inline_string_becomes_its_keyword() {
    let m = mend(
        "inlstr",
        "E_MALFORMED_ATTRIBUTE_ARGS",
        "#[inline = \"never\"]\nfn one() -> i64 {\n    1\n}\n\nfn main() {\n    println(one());\n}\n",
    );
    assert_mended(
        &m,
        "#[inline(never)]\nfn one() -> i64 {\n    1\n}\n\nfn main() {\n    println(one());\n}\n",
        Some("1\n"),
    );
}

#[test]
fn parse_fix_no_effect_string_becomes_its_verbs() {
    let m = mend(
        "noeffstr",
        "E_NO_EFFECT_STRING_VALUE",
        "#[no_effect = \"panics\"]\nfn add(a: i64, b: i64) -> i64 {\n    a.wrapping_add(b)\n}\n\nfn main() {\n    println(add(2, 3));\n}\n",
    );
    assert_mended(
        &m,
        "#[no_effect(panics)]\nfn add(a: i64, b: i64) -> i64 {\n    a.wrapping_add(b)\n}\n\nfn main() {\n    println(add(2, 3));\n}\n",
        Some("5\n"),
    );
}

#[test]
fn parse_fix_profile_string_value_becomes_a_name() {
    let m = mend(
        "profeq",
        "E_PROFILE_STRING_VALUE",
        "#[profile = \"embedded\"]\nfn one() -> i64 {\n    1\n}\n\nfn main() {\n    println(one());\n}\n",
    );
    assert_mended(
        &m,
        "#[profile(embedded)]\nfn one() -> i64 {\n    1\n}\n\nfn main() {\n    println(one());\n}\n",
        Some("1\n"),
    );
}

#[test]
fn parse_fix_profile_string_argument_becomes_a_name() {
    let m = mend(
        "profarg",
        "E_PROFILE_STRING_VALUE",
        "#[profile(\"embedded\", kernel)]\nfn one() -> i64 {\n    1\n}\n\nfn main() {\n    println(one());\n}\n",
    );
    assert_mended(
        &m,
        "#[profile(embedded, kernel)]\nfn one() -> i64 {\n    1\n}\n\nfn main() {\n    println(one());\n}\n",
        Some("1\n"),
    );
}

#[test]
fn parse_fix_assoc_type_attribute_is_deleted() {
    let m = mend(
        "assocattr",
        "E_ATTR_ON_ASSOC_TYPE_DECL",
        "trait Container {\n    #[doc(hidden)]\n    type Item;\n}\n\nfn main() {\n    println(1);\n}\n",
    );
    assert_mended(
        &m,
        "trait Container {\n    type Item;\n}\n\nfn main() {\n    println(1);\n}\n",
        Some("1\n"),
    );
}

#[test]
fn parse_fix_interior_nul_escape_is_removed() {
    let m = mend(
        "cnul",
        "E_INTERIOR_NUL_IN_C_STRING",
        "fn main() {\n    let s = c\"ab\\x00cd\";\n    println(s.len());\n}\n",
    );
    assert_mended(
        &m,
        "fn main() {\n    let s = c\"abcd\";\n    println(s.len());\n}\n",
        Some("4\n"),
    );
}

#[test]
fn parse_fix_keyword_binding_is_escaped_at_its_uses() {
    let m = mend(
        "kwuse",
        "E_RESERVED_KEYWORD",
        "fn main() {\n    let mut match = 3;\n    match += 1;\n    println(match);\n}\n",
    );
    assert_mended(
        &m,
        "fn main() {\n    let mut r#match = 3;\n    r#match += 1;\n    println(r#match);\n}\n",
        Some("4\n"),
    );
}

#[test]
fn parse_fix_keyword_parameter_is_escaped_at_its_uses() {
    let m = mend(
        "kwparam",
        "E_RESERVED_KEYWORD",
        "fn bump(loop: i64) -> i64 {\n    loop + 1\n}\n\nfn main() {\n    println(bump(1));\n}\n",
    );
    assert_mended(
        &m,
        "fn bump(r#loop: i64) -> i64 {\n    r#loop + 1\n}\n\nfn main() {\n    println(bump(1));\n}\n",
        Some("2\n"),
    );
}

#[test]
fn parse_fix_mut_parameter_of_a_required_method_loses_mut() {
    let m = mend(
        "traitmut",
        "E0002",
        "trait Step {\n    fn step(mut n: i64) -> i64;\n}\n\nstruct One {}\n\nimpl Step for One {\n    fn step(n: i64) -> i64 {\n        n + 1\n    }\n}\n\nfn main() {\n    println(One.step(1));\n}\n",
    );
    assert_mended(
        &m,
        "trait Step {\n    fn step(n: i64) -> i64;\n}\n\nstruct One {}\n\nimpl Step for One {\n    fn step(n: i64) -> i64 {\n        n + 1\n    }\n}\n\nfn main() {\n    println(One.step(1));\n}\n",
        Some("2\n"),
    );
}
