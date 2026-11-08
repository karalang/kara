//! B-2026-11-08-26 — io.md § HTTP server: "A program may declare its own
//! `Response` as `struct Response { status: i64, body: String, headers:
//! Vec[(String, String)] }` to send headers." The servers serve that type
//! through their built-in paths keyed on `Response`, so the documented
//! shape (in any field order, with or without `headers`) draws no
//! `prelude_shadow` warning saying those paths no longer apply; any other
//! `Response` still does.

use std::process::Command;

fn check(tag: &str, fields: &str) -> String {
    let dir = std::env::temp_dir().join(format!("karac-response-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(
        &path,
        format!("struct Response {{ {fields} }}\nfn main() {{ println(\"x\"); }}\n"),
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", path.to_str().unwrap()])
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn the_documented_response_shape_draws_no_prelude_shadow_warning() {
    for (tag, fields) in [
        (
            "headers",
            "status: i64, body: String, headers: Vec[(String, String)]",
        ),
        ("plain", "status: i64, body: String"),
        (
            "reordered",
            "headers: Vec[(String, String)], body: String, status: i64",
        ),
    ] {
        let out = check(tag, fields);
        assert!(!out.contains("prelude_shadow"), "{tag}: {out}");
    }
}

#[test]
fn another_response_still_draws_the_warning() {
    for (tag, fields) in [
        ("code", "code: i64, body: String"),
        ("extra", "status: i64, body: String, at: i64"),
    ] {
        let out = check(tag, fields);
        assert!(out.contains("prelude_shadow"), "{tag}: {out}");
    }
}
