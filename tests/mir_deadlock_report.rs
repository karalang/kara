//! A deadlock names the blocked operation's site and carries a panic record
//! of kind `deadlock` (B-2026-11-16-202). It printed a bare
//! `error: deadlock: ...` line with no location, and `run --output=json`
//! emitted no record. Every shape of the root task blocking (a `recv`, a
//! `join` on a waiting task, a second `lock` of a held `Mutex`, a `select`
//! with no ready arm) is checked on the JIT and the MIR interpreter; the
//! built binary's stderr is pinned in `tests/mir_tasks.rs`.

use std::path::PathBuf;
use std::process::{Command, Output};

fn write(tag: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-deadlock-report-{}-{tag}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    path
}

fn karac(args: &[&str], path: &PathBuf) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .output()
        .unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

const LANES: [&[&str]; 2] = [&["run"], &["run", "--interp"]];

/// (tag, program, line, column of the blocked operation).
const CASES: [(&str, &str, u32, u32); 4] = [
    (
        "recv",
        r#"import std.sync.{Receiver, Sender}; fn main() {
    let (tx, rx): (Sender[i64], Receiver[i64]) = channel(1);
    println("start");
    let v = rx.recv();
    println("unreached");
}
"#,
        4,
        13,
    ),
    (
        "join",
        r#"import std.sync.{Receiver, Sender, TaskGroup}; fn main() {
    let (tx, rx): (Sender[i64], Receiver[i64]) = channel(1);
    let group = TaskGroup.new();
    let h = group.spawn(|| { let v: i64 = rx.recv().unwrap(); v });
    println(f"{h.join()}");
    tx.send(1).unwrap();
}
"#,
        5,
        16,
    ),
    (
        "lock",
        r#"import std.sync.{Mutex};
fn main() {
    let m = Mutex.new(1);
    let a = m.lock();
    println("start");
    let b = m.lock();
    println("unreached");
}
"#,
        6,
        13,
    ),
    (
        "select",
        r#"import std.sync.{Receiver, Sender}; fn main() {
    let (tx, rx): (Sender[i64], Receiver[i64]) = channel(1);
    let (tx2, rx2): (Sender[i64], Receiver[i64]) = channel(1);
    select {
        v = rx.recv() => println(f"{v}"),
        w = rx2.recv() => println(f"{w}"),
    }
}
"#,
        4,
        5,
    ),
];

#[test]
fn a_deadlock_names_the_blocked_operation() {
    for (tag, src, line, col) in CASES {
        let path = write(tag, src);
        for lane in LANES {
            let out = karac(lane, &path);
            let err = text(&out.stderr).replace('\\', "/");
            assert!(
                err.starts_with("runtime error: deadlock: every task waits"),
                "{tag} {lane:?}: {err}"
            );
            assert!(
                err.contains(&format!("main.kara:{line}:{col}\n")),
                "{tag} {lane:?}: {err}"
            );
            assert!(!err.contains("\nerror: deadlock"), "{tag} {lane:?}: {err}");
            assert_eq!(out.status.code(), Some(101), "{tag} {lane:?}");
        }
    }
}

#[test]
fn a_deadlock_writes_a_deadlock_record() {
    for (tag, src, line, col) in CASES {
        let path = write(&format!("{tag}-json"), src);
        for lane in LANES {
            let mut json = lane.to_vec();
            json.push("--output=json");
            let out = karac(&json, &path);
            let stdout = text(&out.stdout);
            assert!(
                stdout.contains("{\"runtime_errors\":[{\"message\":\"deadlock: every task waits"),
                "{tag} {lane:?}: {stdout}"
            );
            assert!(
                stdout.contains(&format!(
                    "\"line\":{line},\"col\":{col}}},\"kind\":\"deadlock\"}}]}}"
                )),
                "{tag} {lane:?}: {stdout}"
            );
            assert_eq!(out.status.code(), Some(101), "{tag} {lane:?}");
        }
    }
}
