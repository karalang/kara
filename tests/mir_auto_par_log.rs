//! Auto-par is invisible to `std.log` (docs/library/log.md; B-2026-10-27-22):
//! a statement or loop that logs still runs as tasks, its records carry the
//! enclosing task's `task_id` (none in `main`), and they reach the backend in
//! the order the sequential program emits them. A task the program starts
//! itself is a real task, with its own id.
#![cfg(feature = "llvm")]

use std::path::PathBuf;
use std::process::{Command, Output};

/// Two statements whose first takes much longer, then a loop whose
/// iterations log: as tasks without holding, `tag: 2` came first and each
/// record carried its task's id.
const SRC: &str = r#"import std.log.{info};

fn work(n: i64) -> i64 {
    let mut s = 0;
    let mut i = 0;
    while i < n {
        s = s + i % 7;
        i = i + 1;
    }
    s
}

fn step(tag: i64, n: i64) -> i64 {
    let r = work(n);
    info("step", tag: tag);
    r
}

fn count(v: ref Vec[i64], k: i64) -> i64 {
    let mut s = 0;
    let mut i = 0;
    while i < v.len() {
        if v[i] % 7 == k {
            s = s + 1;
        }
        i = i + 1;
    }
    s
}

fn main() {
    let a = step(1, 3000000);
    let b = step(2, 10);
    println(a + b);
    let v: Vec[i64] = Vec.filled(1000, 3);
    let mut sum = 0;
    for k in 0..12 {
        info("iter", k: k);
        sum = sum + count(v, k % 7);
    }
    println(sum);
}
"#;

fn write(tag: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-auto-par-log-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    path
}

fn karac(args: &[&str], path: &PathBuf) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .env_remove("KARAC_AUTO_PAR")
        .env("KARAC_AUTO_PAR_DEBUG", "1")
        .args(args)
        .arg(path)
        .output()
        .unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// The JSON records among `err`'s lines, which also hold the auto-par
/// report.
fn records(err: &str) -> String {
    err.lines()
        .filter(|l| l.starts_with('{'))
        .map(|l| format!("{l}\n"))
        .collect()
}

fn expected() -> String {
    let mut out = String::new();
    for tag in [1, 2] {
        out += &format!("{{\"level\":\"info\",\"msg\":\"step\",\"tag\":{tag},\"task_id\":null}}\n");
    }
    for k in 0..12 {
        out += &format!("{{\"level\":\"info\",\"msg\":\"iter\",\"k\":{k},\"task_id\":null}}\n");
    }
    out
}

#[test]
fn mir_auto_par_logs_in_statement_order_as_main() {
    let path = write("main", SRC);
    for lane in [&["run"][..], &["run", "--interp"]] {
        let out = karac(lane, &path);
        let err = text(&out.stderr);
        assert!(out.status.success(), "{lane:?}: {err}");
        assert_eq!(text(&out.stdout), "9000018\n2000\n", "{lane:?}");
        // The statements and the loop still run as tasks.
        assert!(
            err.contains("auto-par: statements as tasks: main@"),
            "{lane:?}: {err}"
        );
        assert!(
            err.contains("auto-par: loops as tasks: main@"),
            "{lane:?}: {err}"
        );
        assert_eq!(records(&err), expected(), "{lane:?}");
    }
}

/// The built binary, where the tasks run on the pool's workers.
#[test]
fn mir_auto_par_logs_in_statement_order_built() {
    let path = write("built", SRC);
    let bin = path.with_file_name("main.out");
    let out = karac(&["build", "-o", bin.to_str().unwrap()], &path);
    let err = text(&out.stderr);
    if !out.status.success() && (err.contains("Linker failed") || err.contains("not found")) {
        eprintln!("skipped: no runtime archive");
        return;
    }
    assert!(out.status.success(), "{err}");
    for _ in 0..5 {
        let run = Command::new(&bin).output().unwrap();
        assert!(run.status.success(), "{}", text(&run.stderr));
        assert_eq!(text(&run.stdout), "9000018\n2000\n");
        assert_eq!(records(&text(&run.stderr)), expected());
    }
}

/// A written `par`'s branches are tasks the program started: real tasks,
/// each record with its own id, in no order relative to its siblings.
#[test]
fn mir_written_par_logs_as_its_tasks() {
    let src = SRC.replace(
        "    let a = step(1, 3000000);\n    let b = step(2, 10);\n",
        "    let (a, b) = par { step(1, 3000000), step(2, 10) };\n",
    );
    let path = write("written", &src);
    let out = karac(&["run"], &path);
    let err = text(&out.stderr);
    assert!(out.status.success(), "{err}");
    assert_eq!(text(&out.stdout), "9000018\n2000\n");
    let recs = records(&err);
    let mut ids: Vec<&str> = recs
        .lines()
        .filter(|l| l.contains("\"msg\":\"step\""))
        .map(|l| l.rsplit_once("\"task_id\":").unwrap().1)
        .collect();
    assert_eq!(ids.len(), 2, "{recs}");
    assert!(ids.iter().all(|i| *i != "null}"), "{recs}");
    ids.dedup();
    assert_eq!(ids.len(), 2, "{recs}");
}

/// A later statement's panic ends the program while an earlier
/// statement's task still holds its record: the record reaches the
/// backend before the panic is reported, as it would run in order
/// (B-2026-10-27-30).
const PANICS: &str = r#"import std.log.{info};

fn inner(k: i64) -> i64 {
    let mut s = 0;
    let mut j = 0;
    while j < 3 {
        s = s + (k + j) % 7;
        j = j + 1;
    }
    s
}

fn work(n: i64) -> i64 {
    let mut s = 0;
    let mut i = 0;
    while i < n {
        s = s + inner(i);
        i = i + 1;
    }
    s
}

fn step(tag: i64, n: i64) -> i64 {
    let r = work(n);
    info("step", tag: tag);
    r
}

fn boom(n: i64) -> i64 {
    let r = work(n);
    let v: Vec[i64] = Vec.new();
    r + v[r]
}

fn main() {
    let a = step(1, 10);
    let b = boom(3000000);
    println(a + b);
}
"#;

#[test]
fn mir_auto_par_logs_before_a_later_panic() {
    let path = write("panics", PANICS);
    let record = "{\"level\":\"info\",\"msg\":\"step\",\"tag\":1,\"task_id\":null}\n";
    let check = |lane: &str, out: &Output| {
        let err = text(&out.stderr);
        assert_eq!(out.status.code(), Some(101), "{lane}: {err}");
        let at = err
            .find(record)
            .unwrap_or_else(|| panic!("{lane}: no record in:\n{err}"));
        let panic = err.find("runtime error: index out of bounds").unwrap();
        assert!(at < panic, "{lane}: the record after the panic:\n{err}");
    };
    for lane in [&["run"][..], &["run", "--interp"]] {
        let out = karac(lane, &path);
        let err = text(&out.stderr);
        assert!(
            err.contains("auto-par: statements as tasks: main@"),
            "{lane:?}: {err}"
        );
        check(&format!("{lane:?}"), &out);
    }
    // Built, the first statement's own group holds the record while its
    // long loop runs, and the second statement panics at once.
    let src = PANICS
        .replace("step(1, 10)", "step(1, 30000000)")
        .replace("boom(3000000)", "boom(10)");
    let path = write("panics-built", &src);
    let bin = path.with_file_name("main.out");
    let out = karac(&["build", "-o", bin.to_str().unwrap()], &path);
    let err = text(&out.stderr);
    if !out.status.success() && (err.contains("Linker failed") || err.contains("not found")) {
        eprintln!("skipped: no runtime archive");
        return;
    }
    assert!(out.status.success(), "{err}");
    assert!(
        err.contains("auto-par: statements as tasks: main@"),
        "{err}"
    );
    for _ in 0..3 {
        check("built", &Command::new(&bin).output().unwrap());
    }
}
