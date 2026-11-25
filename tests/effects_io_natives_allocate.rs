//! The I/O natives that return a new `String` or `Vec`, or grow the one they
//! are given, carry `allocates(Heap)` (design.md § Default-permitted effects:
//! inferred and shown, and `#[no_effect(allocates(Heap))]` proves its
//! absence). `#[no_effect(allocates(Heap))]` accepted a function that read a
//! file, a line of stdin or the arguments (B-2026-11-25-33; found by
//! conformance/library/io's example_tool_skeleton).

use std::path::PathBuf;
use std::process::{Command, Output};

fn check(tag: &str, src: &str) -> Output {
    let dir: PathBuf = std::env::temp_dir().join(format!(
        "karac-io-alloc-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("p.kara");
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    out
}

#[test]
fn no_effect_allocates_refuses_each_io_native_that_builds_a_value() {
    let calls = [
        (
            "read-to-string",
            "match fs.read_to_string(\"x\") { Ok(s) => s.len(), Err(_) => 0 }",
        ),
        (
            "read-bytes",
            "match fs.read_bytes(\"x\") { Ok(v) => v.len(), Err(_) => 0 }",
        ),
        (
            "read-dir",
            "match fs.read_dir(\".\") { Ok(v) => v.len(), Err(_) => 0 }",
        ),
        (
            "read-lines",
            "match fs.read_lines(\"x\") { Ok(v) => v.len(), Err(_) => 0 }",
        ),
        (
            "stdin-line",
            "match stdin.read_line() { Ok(s) => s.len(), Err(_) => 0 }",
        ),
        ("args", "env.args().len()"),
    ];
    for (tag, body) in calls {
        let src = format!(
            "#[no_effect(allocates(Heap))]\nfn f() -> i64 {{ {body} }}\nfn main() {{ println(f()); }}\n"
        );
        let out = check(tag, &src);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{tag}: check must refuse: {err}");
        assert!(
            err.contains("E_NO_EFFECT_VIOLATED") && err.contains("allocates(Heap)"),
            "{tag}: {err}"
        );
    }
}

#[test]
fn a_buffered_read_line_into_a_given_string_allocates() {
    let out = check(
        "bufreader",
        "import std.io.{BufReader};\n\
         #[no_effect(allocates(Heap))]\n\
         fn f(r: mut ref BufReader[File], s: mut ref String) -> i64 { r.read_line(mut s).unwrap_or(0) }\n\
         fn main() {}\n",
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "check must refuse: {err}");
    assert!(err.contains("E_NO_EFFECT_VIOLATED"), "{err}");
}

#[test]
fn a_path_query_that_builds_nothing_still_passes() {
    let out = check(
        "exists",
        "#[no_effect(allocates(Heap))]\nfn f() -> bool { fs.exists(\"x\") }\nfn main() { println(f()); }\n",
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
