//! B-2026-11-18-55: a `std.log` record's `task_id` is numbered from 1 in
//! the order the program starts tasks, and a task the compiler's auto-par
//! creates is not a task to the program (docs/library/log.md § Span
//! context). Both lanes numbered every task they spawned, so two adjacent
//! `info` lines that auto-par ran side by side took ids, and a later `par`
//! block's branches logged as 4 and 5 instead of 1 and 2.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

const ADJACENT: &str = "import std.log.{info};\nfn work(i: i64) -> i64 { info(\"in\", i: i); i }\nfn main() {\n    info(\"a\");\n    info(\"b\");\n    par { work(1), work(2) };\n}\n";
const ADJACENT_OUT: &str = "{\"level\":\"info\",\"msg\":\"a\",\"task_id\":null}\n{\"level\":\"info\",\"msg\":\"b\",\"task_id\":null}\n{\"level\":\"info\",\"msg\":\"in\",\"i\":1,\"task_id\":1}\n{\"level\":\"info\",\"msg\":\"in\",\"i\":2,\"task_id\":2}\n";
const BACKEND: &str = "import std.log;\nimport std.json.{Json};\nimport std.log.{debug, info, warn, Backend, Record, Level};\nstruct Mine { n: i64 }\nimpl Backend for Mine {\n    fn emit(mut ref self, record: Record) {\n        self.n += 1;\n        let mut fs = String.new();\n        for (k, v) in record.fields { fs.push_str(f\" {k}={v.clone().stringify()}\"); }\n        println(f\"#{self.n} {record.msg}{fs} task={record.task_id ?? -1} trace={record.trace_id.is_some()}\");\n        if record.msg == \"inner\" { return; }\n        warn(\"inner\", from: \"emit\");\n    }\n}\nfn work(i: i64) -> i64 { warn(\"in task\", i: i); i }\nfn main() {\n    info(\"before init\", x: 1);\n    debug(\"never\");\n    log.init(Mine { n: 0 }, level: Level.Warn);\n    info(\"filtered\");\n    warn(\"w\", a: [1, 2]);\n    par { work(1), work(2) };\n}\n";

fn run(lane: &str, src: &str) -> String {
    let dir = std::env::temp_dir().join(format!(
        "karac-log-task-ids-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(lane)
        .arg(&f)
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // The default backend writes to stderr; the order across the two
    // streams is not the point here.
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    )
}

fn ids(out: &str) -> Vec<String> {
    out.lines()
        .filter_map(|l| l.split("task=").nth(1))
        .map(|s| s.split(' ').next().unwrap().to_string())
        .collect()
}

#[test]
fn auto_par_log_lines_take_no_task_id() {
    assert_eq!(run("__mir-run", ADJACENT), ADJACENT_OUT);
}

#[test]
fn a_custom_backend_sees_the_first_par_as_tasks_one_and_two() {
    assert_eq!(ids(&run("__mir-run", BACKEND)), ["-1", "1", "2"]);
}

/// The compiled `par` branches run on worker threads, so their two lines
/// come in either order; the ids are fixed by start order, not run order.
#[cfg(feature = "llvm")]
fn sorted_lines(out: &str) -> Vec<String> {
    let mut v: Vec<String> = out.lines().map(str::to_string).collect();
    v.sort();
    v
}

#[cfg(feature = "llvm")]
#[test]
fn compiled_log_task_ids_skip_auto_par_tasks() {
    assert_eq!(
        sorted_lines(&run("__mir-llvm-run", ADJACENT)),
        sorted_lines(ADJACENT_OUT)
    );
    // Each branch's own line names its task: `i=1` is task 1, `i=2` task 2.
    let out = run("__mir-llvm-run", BACKEND);
    let mut pairs: Vec<String> = out
        .lines()
        .filter(|l| l.contains("in task"))
        .map(|l| {
            let i = l.split(" i=").nth(1).unwrap().split(' ').next().unwrap();
            let t = l.split("task=").nth(1).unwrap().split(' ').next().unwrap();
            format!("{i}:{t}")
        })
        .collect();
    pairs.sort();
    assert_eq!(pairs, ["1:1", "2:2"], "{out}");
    assert_eq!(ids(&out).first().map(String::as_str), Some("-1"), "{out}");
}
