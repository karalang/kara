//! Allocation failure is a panic (core semantics §10.1, B-2026-11-16-317):
//! "runtime error: capacity overflow", exit 101, and kind `alloc_failure`
//! in the panic record, on the JIT and the interpreter alike. Compiled
//! code used to print `panic: capacity overflow` and abort (exit 134, no
//! record), and the interpreter reported `explicit` or did not fail. A
//! `reserve` whose byte count passed 2^64 set a wrapped capacity and let
//! the next pushes write past the buffer (B-2026-11-16-316); it panics too.

use std::path::PathBuf;
use std::process::Command;

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-alloc-failure-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Programs whose capacity no buffer can hold, each printing nothing.
const CASES: &[(&str, &str)] = &[
    (
        "vec_with_capacity",
        "fn main() {\n    let v: Vec[i64] = Vec.with_capacity(9223372036854775807);\n    println(v.len());\n}\n",
    ),
    (
        "string_with_capacity",
        "fn main() {\n    let s: String = String.with_capacity(9223372036854775807);\n    println(s.len());\n}\n",
    ),
    (
        "vec_reserve_wraps",
        "fn main() {\n    let mut v: Vec[i64] = [1, 2, 3];\n    v.reserve(9223372036854775807);\n    println(v.capacity());\n}\n",
    ),
    (
        "vec_reserve_2_61",
        "fn main() {\n    let mut v: Vec[i64] = Vec.new();\n    v.reserve_exact(1 << 61);\n    println(v.capacity());\n}\n",
    ),
    (
        "string_reserve",
        "fn main() {\n    let mut s = \"ab\".to_string();\n    s.reserve(9223372036854775807);\n    println(s.len());\n}\n",
    ),
];

/// `karac run` with `args` on `src`: (exit code, stdout, stderr).
fn run(src: &str, args: &[&str]) -> (Option<i32>, String, String) {
    let dir = scratch("run");
    std::fs::write(dir.join("main.kara"), src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
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

fn check(extra: &[&str]) {
    let mut wrong = Vec::new();
    for (name, src) in CASES {
        let (code, stdout, stderr) = run(src, extra);
        if code != Some(101)
            || !stdout.is_empty()
            || !stderr.contains("runtime error: capacity overflow")
        {
            wrong.push(format!(
                "{name} {extra:?} text: exit {code:?}\n{stdout}\n{stderr}"
            ));
        }
        let mut args = vec!["--output=jsonl"];
        args.extend_from_slice(extra);
        let (code, stdout, stderr) = run(src, &args);
        let event = stdout
            .lines()
            .find(|l| l.starts_with("{\"type\":\"runtime_error\""));
        if code != Some(101) || !event.is_some_and(|e| e.contains("\"kind\":\"alloc_failure\"")) {
            wrong.push(format!(
                "{name} {extra:?} json: exit {code:?}\n{stdout}\n{stderr}"
            ));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n---\n"));
}

#[test]
fn alloc_failure_is_a_panic_with_its_kind() {
    check(&[]);
}

#[test]
fn the_interpreter_reports_alloc_failure_too() {
    check(&["--interp"]);
}
