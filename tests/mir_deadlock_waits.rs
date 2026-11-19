//! A deadlock names what the tasks wait on and where (B-2026-11-19-31). A
//! deadlock among `Mutex` waits was reported as "every task waits on a
//! channel that no running task sends to or receives from", naming a
//! channel no task waited on, and with the root waiting at its group's end
//! the report carried no place at all. The report now names the kinds of
//! wait, lists the place of each task other than the root, and places the
//! root's wait for a group's tasks at the group's binding. Checked on the
//! JIT, the MIR interpreter and a built binary.

#![cfg(feature = "llvm")]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn write(tag: &str, src: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("karac-deadlock-waits-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    path
}

fn karac(args: &[&str], path: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path.file_name().unwrap())
        .current_dir(path.parent().unwrap())
        .output()
        .unwrap()
}

/// The report, with the program's name (`main.kara`) written `F`.
fn report(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).replace("main.kara", "F")
}

/// Each lane's report, and its exit code.
fn lanes(tag: &str, src: &str) -> Vec<(String, String, Option<i32>)> {
    let path = write(tag, src);
    let mut seen = Vec::new();
    for (lane, args) in [("jit", &["run"][..]), ("interp", &["run", "--interp"][..])] {
        let out = karac(args, &path);
        seen.push((lane.to_string(), report(&out), out.status.code()));
    }
    let built = karac(&["build"], &path);
    assert!(built.status.success(), "{}", report(&built));
    let exe = path.with_extension("");
    let out = Command::new(&exe)
        .current_dir(path.parent().unwrap())
        .output()
        .unwrap();
    seen.push(("built".to_string(), report(&out), out.status.code()));
    seen
}

const LOCKS: &str = r#"import std.sync.{Mutex, TaskGroup};
import std.time.{Duration, sleep};
fn main() {
    let a = Mutex.new(0);
    let b = Mutex.new(0);
    {
        let group = TaskGroup.new();
        group.spawn(|| { let _x = a.lock(); sleep(Duration.ms(50)); let _y = b.lock(); });
        group.spawn(|| { let _y = b.lock(); sleep(Duration.ms(50)); let _x = a.lock(); });
    }
    println("done");
}
"#;

#[test]
fn a_deadlock_among_lock_waits_names_the_locks_and_their_places() {
    for (lane, err, code) in lanes("locks", LOCKS) {
        assert_eq!(
            err.lines().next().unwrap_or(""),
            "runtime error: deadlock: every task waits on a lock or a task that no running task \
             can free; the other tasks wait on a lock at F:8:78, a lock at F:9:78",
            "{lane}: {err}"
        );
        // The root waits for the group's tasks at the group's binding.
        assert!(err.contains("\n  at F:7:9\n"), "{lane}: {err}");
        assert!(!err.contains("channel"), "{lane}: {err}");
        assert_eq!(code, Some(101), "{lane}");
    }
}

const CHANNELS: &str = r#"fn main() {
    let (tx1, rx1): (Sender[i64], Receiver[i64]) = channel(1);
    let (tx2, rx2): (Sender[i64], Receiver[i64]) = channel(1);
    let group = TaskGroup.new();
    group.spawn(|| { let v = rx1.recv(); let _ = tx2.send(1); });
    group.spawn(|| { let w = rx2.recv(); let _ = tx1.send(2); });
    println("waiting");
}
"#;

#[test]
fn a_deadlock_among_channel_waits_lists_the_other_tasks_places() {
    for (lane, err, code) in lanes("channels", CHANNELS) {
        assert_eq!(
            err.lines().next().unwrap_or(""),
            "runtime error: deadlock: every task waits on a channel or a task that no running \
             task can free; the other tasks wait on a channel at F:5:30, a channel at F:6:30",
            "{lane}: {err}"
        );
        assert!(err.contains("\n  at F:4:5\n"), "{lane}: {err}");
        assert_eq!(code, Some(101), "{lane}");
    }
}
