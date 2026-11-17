//! An allocation failure in a task of an isolating group is that task's
//! panic (core-semantics §11.12, B-2026-11-17-86): its handle holds
//! `Err(Panicked { message })` with "capacity overflow" or "out of
//! memory", and the group's other tasks and `main` run on. Built code used
//! to end the process with exit 101 for every one of these, and the
//! interpreter aborted in Rust's own allocator on `filled`, `resize` and
//! `repeat` with a count the allocator refuses.
//!
//! Each count asks for about 2^60 bytes: under the 2^61 ceiling, so it
//! reaches the allocator, and past any address space, so the allocator
//! refuses it on every host.
#![cfg(feature = "llvm")]

use std::path::PathBuf;
use std::process::Command;

/// (name, `big`'s body over `n`, `big(3)`, the message `b` panics with).
const CASES: &[(&str, &str, i64, &str)] = &[
    (
        "with_capacity_over",
        "let v: Vec[i64] = Vec.with_capacity(n); v.len()",
        0,
        "capacity overflow",
    ),
    (
        "with_capacity_refused",
        "let v: Vec[i64] = Vec.with_capacity(n >> 6); v.len()",
        0,
        "out of memory",
    ),
    (
        "reserve_over",
        "let mut v: Vec[i64] = [1]; v.reserve(n); v.len()",
        1,
        "capacity overflow",
    ),
    (
        "string_with_capacity_over",
        "let s = String.with_capacity(n); s.len()",
        0,
        "capacity overflow",
    ),
    (
        "string_reserve_over",
        "let mut s = \"ab\".to_string(); s.reserve(n); s.len()",
        2,
        "capacity overflow",
    ),
    (
        "filled_refused",
        "let v = Vec.filled(n >> 6, 0); v.len()",
        0,
        "out of memory",
    ),
    (
        "resize_refused",
        "let mut v: Vec[i64] = Vec.new(); v.resize(n >> 6, 0); v.len()",
        0,
        "out of memory",
    ),
    (
        "repeat_refused",
        "let s = \"ab\".repeat(n >> 4); s.len()",
        0,
        "out of memory",
    ),
];

fn program(body: &str) -> String {
    format!(
        r#"fn big(n: i64) -> i64 {{
    {body}
}}

fn main() {{
    let g = TaskGroup.isolating();
    let a = g.spawn(|| big(3));
    let b = g.spawn(|| big(9223372036854775807));
    let ra = a.join();
    let rb = b.join();
    match ra {{
        Ok(v) => println(f"a {{v}}"),
        Err(_) => println("a failed"),
    }}
    match rb {{
        Ok(v) => println(f"b {{v}}"),
        Err(TaskFailed.Panicked {{ message, location }}) => println(f"b panicked: {{message}}"),
        Err(TaskFailed.Cancelled) => println("b cancelled"),
    }}
    println("main goes on");
}}
"#
    )
}

fn scratch(name: &str, lane: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-isolating-alloc-{lane}-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Every case on one lane: the wrong ones, each with its output.
fn wrong_on(lane: &[&str]) -> Vec<String> {
    let mut wrong = Vec::new();
    for (name, body, a, msg) in CASES {
        // The interpreter's `with_capacity` reserves nothing, so a count
        // under the ceiling cannot fail there.
        if lane == ["__mir-run"] && *name == "with_capacity_refused" {
            continue;
        }
        let dir = scratch(name, lane[0].trim_start_matches('_'));
        let file = dir.join("main.kara");
        std::fs::write(&file, program(body)).unwrap();
        let out = Command::new(env!("CARGO_BIN_EXE_karac"))
            .args(lane)
            .arg(&file)
            .output()
            .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let got = String::from_utf8_lossy(&out.stdout);
        let want = format!("a {a}\nb panicked: {msg}\nmain goes on\n");
        if got != want || out.status.code() != Some(0) {
            wrong.push(format!(
                "{name} {lane:?}: exit {:?}\n{got}{}",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr)
            ));
        }
    }
    wrong
}

#[test]
fn built_isolating_task_takes_its_alloc_failure() {
    let wrong = wrong_on(&["__mir-llvm-run"]);
    assert!(wrong.is_empty(), "{}", wrong.join("\n---\n"));
}

#[test]
fn interp_isolating_task_takes_its_alloc_failure() {
    let wrong = wrong_on(&["__mir-run"]);
    assert!(wrong.is_empty(), "{}", wrong.join("\n---\n"));
}
