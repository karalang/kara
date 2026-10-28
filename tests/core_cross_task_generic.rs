//! B-2026-10-27-33: a generic item that spawns or sends a value of its own
//! type parameter must itself require `T: CrossTask`, and the error is at
//! that generic, not after monomorphization (design.md § `CrossTask`). The
//! sites are a task's captures and result, a channel send and a provider
//! that may be shared; `karac fix` writes the bound, after which the
//! caller's `shared` argument is refused at the call.

use std::process::Command;

fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-cross-task-generic-{tag}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `karac check` on `src`: whether it passes, and its stderr.
fn check(tag: &str, src: &str) -> (bool, String) {
    let dir = scratch(tag);
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&file)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

const ROW: &str = "import std.sync.{TaskGroup};\n\
                   shared struct Cell { n: i64 }\n\
                   fn run_all[U](x: own U) -> i64 {\n\
                       let g = TaskGroup.new();\n\
                       let h = g.spawn(|| { let y = x; 1 });\n\
                       h.join()\n\
                   }\n\
                   fn main() { println(run_all(Cell { n: 1 })); }\n";

#[test]
fn a_task_capturing_an_unbounded_type_parameter_is_refused_at_the_generic() {
    let (ok, err) = check("row", ROW);
    assert!(!ok);
    for want in [
        "main.kara:5:30: error[E_NOT_CROSS_TASK]: capture of `x` by the task passed to `TaskGroup.spawn` (type `U`) cannot cross a task boundary",
        "note: type parameter `U` is not bounded by `CrossTask`, so a caller may give it a `shared` value",
        "help: declare `U: CrossTask` where `U` is declared",
    ] {
        assert!(err.contains(want), "want {want:?} in\n{err}");
    }
}

#[test]
fn karac_fix_writes_the_bound_and_the_shared_argument_is_then_refused_at_the_call() {
    let dir = scratch("fix");
    let file = dir.join("main.kara");
    std::fs::write(&file, ROW).unwrap();
    let karac = env!("CARGO_BIN_EXE_karac");
    let fix = Command::new(karac).arg("fix").arg(&file).output().unwrap();
    assert!(fix.status.success());
    let after = std::fs::read_to_string(&file).unwrap();
    assert!(
        after.contains("fn run_all[U: CrossTask](x: own U) -> i64 {"),
        "{after}"
    );
    let out = Command::new(karac)
        .arg("check")
        .arg(&file)
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!out.status.success());
    assert!(
        err.contains("main.kara:8:21: error[E_NOT_CROSS_TASK]: `U: CrossTask` is not satisfied: `Cell` cannot cross a task boundary"),
        "{err}"
    );
}

#[test]
fn a_task_result_and_a_channel_send_of_a_type_parameter_need_the_bound() {
    let (ok, err) = check(
        "result-send",
        "import std.sync.{TaskGroup, Channel};\n\
         fn back[U](x: own U) -> U {\n\
             let g = TaskGroup.new();\n\
             let h = g.spawn(|| x);\n\
             h.join()\n\
         }\n\
         fn send_it[V](x: own V) -> i64 {\n\
             let (tx, rx) = Channel.new();\n\
             tx.send(x);\n\
             1\n\
         }\n\
         fn main() { println(back(1) + send_it(2)); }\n",
    );
    assert!(!ok);
    for want in [
        "main.kara:4:20: error[E_NOT_CROSS_TASK]: capture of `x` by the task passed to `TaskGroup.spawn` (type `U`)",
        "main.kara:9:1: error[E_NOT_CROSS_TASK]: value sent across a channel (type `V`) cannot cross a task boundary",
    ] {
        assert!(err.contains(want), "want {want:?} in\n{err}");
    }
    // `back`'s capture and result name one declaration: one report.
    assert_eq!(err.matches("type parameter `U`").count(), 1, "{err}");
}

#[test]
fn a_bounded_type_parameter_crosses_and_runs() {
    let src = "import std.sync.{TaskGroup};\n\
               fn twice[U: CrossTask + Clone](x: own U) -> U {\n\
                   let g = TaskGroup.new();\n\
                   let h = g.spawn(|| x.clone());\n\
                   h.join()\n\
               }\n\
               fn main() { println(twice(21) * 2); }\n";
    let (ok, err) = check("bounded", src);
    assert!(ok, "{err}");
    let out = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(out.output, "42\n");
}

#[test]
fn a_select_send_of_a_type_parameter_needs_the_bound() {
    let src = |bound: &str| {
        format!(
            "import std.sync.{{channel, Sender}};\n\
             fn put[U{bound}](tx: ref Sender[U], v: own U) -> i64 {{\n\
                 select {{\n\
                     r = tx.send(v) => if r.is_ok() {{ 1 }} else {{ 0 }},\n\
                 }}\n\
             }}\n\
             fn main() {{ let (tx, rx) = channel(4); println(put(tx, 3)); }}\n"
        )
    };
    let (ok, err) = check("select", &src(""));
    assert!(!ok);
    let want = "main.kara:3:1: type parameter 'U' is passed where 'T: CrossTask' is required, but its declaration does not bound it by `CrossTask`";
    assert!(err.contains(want), "want {want:?} in\n{err}");
    let bounded = src(": CrossTask");
    let (ok, err) = check("select-bounded", &bounded);
    assert!(ok, "{err}");
    let out = karac::mir::lower::run_source(&bounded).expect("MIR interpreter");
    assert_eq!(out.output, "1\n");
}
