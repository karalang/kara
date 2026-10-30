//! The `unused_effect` lint (design.md § Effects, B-2026-10-30-22).
//!
//! A declaration may name more than its body performs, so a library can
//! reserve an effect. The lint, allowed by default, reports each such
//! effect; `#[warn]` / `#[deny]` on the function or its `impl` block, or
//! `-W` / `-D` on the command line, turn it on, and `karac fix` writes the
//! clause without them.

use std::path::PathBuf;
use std::process::Command;

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

const PRELUDE: &str = "effect resource Db;
effect resource Log;

trait Source {
    fn load(self) -> i64;
}
effect resource UserDB: Source;
struct Fixed { n: i64 }
impl Source for Fixed {
    fn load(self) -> i64 { self.n }
}

// Reserves `writes(Db)`, as a library would, for the callers below.
#[allow(unused_effect)]
fn touch() -> i64 with writes(Db) { 1 }
";

fn fixture(tag: &str, body: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-unused-effect-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("p.kara");
    std::fs::write(&path, format!("{PRELUDE}\n{body}\nfn main() {{ }}\n")).unwrap();
    (dir, path)
}

/// `karac check`'s verdict and its diagnostics, with `flags` first.
fn check(tag: &str, flags: &[&str], body: &str) -> (bool, String) {
    let (dir, path) = fixture(tag, body);
    let out = karac()
        .arg("check")
        .args(flags)
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned() + &String::from_utf8_lossy(&out.stdout),
    )
}

fn lint_lines(out: &str) -> Vec<&str> {
    out.lines()
        .filter(|l| l.contains("[unused_effect]"))
        .collect()
}

#[test]
fn unused_effect_is_allowed_by_default() {
    let (ok, out) = check(
        "default",
        &[],
        "pub fn reserve(a: i64) -> i64 with writes(Db) { a }",
    );
    assert!(ok, "{out}");
    assert!(lint_lines(&out).is_empty(), "{out}");
}

#[test]
fn unused_effect_warn_reports_the_reserved_effect() {
    let (ok, out) = check(
        "warn",
        &[],
        "#[warn(unused_effect)]
pub fn reserve(a: i64) -> i64 with writes(Db) { a }",
    );
    assert!(ok, "a warning does not fail the check: {out}");
    let lines = lint_lines(&out);
    assert_eq!(lines.len(), 1, "{out}");
    assert!(
        lines[0].starts_with("warning[unused_effect]")
            && lines[0].contains("`reserve` declares `writes(Db)` but never performs it"),
        "{out}"
    );
}

#[test]
fn unused_effect_names_only_the_unused_part_of_a_clause() {
    let (_, out) = check(
        "partial",
        &[],
        "#[warn(unused_effect)]
pub fn partial() with writes(Stdout) reads(Log, Db) writes(Db) {
    println(f\"{touch()}\");
}",
    );
    let lines = lint_lines(&out);
    assert_eq!(lines.len(), 1, "{out}");
    assert!(
        lines[0].contains("declares `reads(Log)`, `reads(Db)` but never performs them"),
        "{out}"
    );
}

/// What the body reaches through a callee, a resource's method, or a
/// function it names as a value is performed.
#[test]
fn unused_effect_counts_what_the_body_reaches() {
    let (_, out) = check(
        "reached",
        &["-W", "unused_effect"],
        "pub fn via_call() -> i64 with writes(Db) { touch() }
pub fn via_resource() -> i64 with reads(UserDB) { UserDB.load() }
pub fn via_value() -> i64 with writes(Db) {
    let g = touch;
    g()
}",
    );
    assert!(lint_lines(&out).is_empty(), "{out}");
}

/// A call through a function-typed parameter, or through a generic bound,
/// is charged to each caller at the value or type it passes (design.md §
/// Effects), so the function itself performs nothing there, and a clause
/// naming that effect is reported like any other.
#[test]
fn unused_effect_reports_what_only_a_parameter_brings() {
    let (_, out) = check(
        "param",
        &["-W", "unused_effect"],
        "pub fn apply(f: Fn() -> i64 with writes(Db)) -> i64 with writes(Db) { f() }
pub trait Act { fn act(self) with writes(Db); }
pub fn via_bound[T: Act](t: T) with writes(Db) { t.act(); }",
    );
    let lines = lint_lines(&out);
    assert_eq!(lines.len(), 2, "{out}");
    assert!(lines[0].contains("`apply` declares `writes(Db)`"), "{out}");
    assert!(
        lines[1].contains("`via_bound` declares `writes(Db)`"),
        "{out}"
    );
}

#[test]
fn unused_effect_command_line_and_attributes() {
    let body = "pub fn reserve(a: i64) -> i64 with writes(Db) { a }
#[allow(unused_effect)]
pub fn kept(a: i64) -> i64 with writes(Log) { a }";
    let (ok, out) = check("cli_warn", &["-W", "unused_effect"], body);
    assert!(ok, "{out}");
    let lines = lint_lines(&out);
    assert_eq!(lines.len(), 1, "the `#[allow]` beats `-W`: {out}");
    assert!(lines[0].contains("`reserve`"), "{out}");

    let (ok, out) = check("cli_deny", &["-D", "unused_effect"], body);
    assert!(!ok, "{out}");
    assert!(
        lint_lines(&out)
            .iter()
            .any(|l| l.starts_with("error[unused_effect]")),
        "{out}"
    );

    let (ok, out) = check(
        "impl_deny",
        &[],
        "struct S {}
#[deny(unused_effect)]
impl S {
    fn get(self) -> i64 with reads(Db) { 1 }
}",
    );
    assert!(
        !ok,
        "the `impl` block's `#[deny]` covers its methods: {out}"
    );
    assert!(out.contains("`S.get` declares `reads(Db)`"), "{out}");
}

#[test]
fn unused_effect_fix_removes_the_unused_effects() {
    let (dir, path) = fixture(
        "fix",
        "#[warn(unused_effect)]
pub fn reserve(a: i64) -> i64 with writes(Db) { a }
#[warn(unused_effect)]
pub fn partial() with writes(Stdout) reads(Log, Db) {
    println(\"hi\");
}",
    );
    let out = karac().arg("fix").arg(&path).output().unwrap();
    let fixed = std::fs::read_to_string(&path).unwrap();
    let recheck = karac()
        .arg("check")
        .args(["-D", "unused_effect"])
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        fixed.contains("pub fn reserve(a: i64) -> i64 { a }"),
        "the whole clause goes, with its space:\n{fixed}"
    );
    assert!(
        fixed.contains("pub fn partial() with writes(Stdout) {"),
        "the used effect stays:\n{fixed}"
    );
    assert!(
        recheck.status.success(),
        "{}",
        String::from_utf8_lossy(&recheck.stderr)
    );
}
