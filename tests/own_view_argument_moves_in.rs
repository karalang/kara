//! B-2026-11-13-81 (core semantics §5.3, §5.4): a view built in place at an `own`
//! parameter (`pick([t.as_str()])`, `pickv(V { s: t.as_str() })`) moves into
//! the callee carrying its own origins, so the result borrows `t`, not the
//! literal. Before the fix the literal was taken for a temporary the result
//! borrowed, and §5.5's "a reference to a temporary outlives its statement"
//! refused the program; binding the literal to a local first was accepted.

use std::path::PathBuf;
use std::process::Command;

fn fixture(tag: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-own-view-arg-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("p.kara");
    std::fs::write(&path, src).unwrap();
    (dir, path)
}

fn check(tag: &str, src: &str) -> (bool, String) {
    let (dir, path) = fixture(tag, src);
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

const TEMP: &str = "a reference to a temporary outlives its statement";

const DEFS: &str = "struct V { s: Str }\n\
    fn pick(other: own Vec[Str]) -> Vec[Str] { other }\n\
    fn pickv(other: own V) -> V { other }\n\
    fn pickt(other: own (Str, i64)) -> (Str, i64) { other }\n\
    fn first(v: Vec[Str]) -> Str { v[0] }\n";

fn program(body: &str) -> String {
    format!("{DEFS}fn main() {{\n    let t = \"hello\".to_string();\n{body}}}\n")
}

#[test]
fn own_view_parameter_takes_a_literal_built_in_place() {
    let src = program(
        "    let r = pick([t.as_str()]);\n    \
         let w = pickv(V { s: t.as_str() });\n    \
         let p = pickt((t.as_str(), 1));\n    \
         println(f\"{r[0]} {w.s} {p.0}\");\n",
    );
    let (ok, err) = check("probe", &src);
    assert!(ok, "check must accept:\n{src}\n{err}");
}

#[test]
fn own_view_parameter_still_refuses_a_temporary_inside_the_literal() {
    for (tag, body) in [
        (
            "vec",
            "    let r = pick([t.to_uppercase().as_str()]);\n    println(r[0]);\n",
        ),
        (
            "struct",
            "    let w = pickv(V { s: t.to_uppercase().as_str() });\n    println(w.s);\n",
        ),
    ] {
        let src = program(body);
        let (ok, err) = check(tag, &src);
        assert!(!ok, "{tag}: check must refuse:\n{src}");
        assert!(err.contains(TEMP), "{tag}: expected `{TEMP}` in: {err}");
    }
}

/// A bare parameter borrows its argument, so the literal there is the
/// temporary the result borrows (§5.4), as before.
#[test]
fn borrowed_view_parameter_still_refuses_a_literal_temporary() {
    let src = program("    let r = first([t.as_str()]);\n    println(r);\n");
    let (ok, err) = check("borrowed", &src);
    assert!(!ok, "check must refuse:\n{src}");
    assert!(err.contains(TEMP), "expected `{TEMP}` in: {err}");
}
