//! B-2026-10-27-21: `CrossTask` is the bound the compiler computes
//! (design.md § `CrossTask`). `T: CrossTask` holds unless `T` reaches a
//! `shared` handle or another task-local type, a failed bound is
//! `E_NOT_CROSS_TASK` with the path to that leaf, and a hand-written impl is
//! refused. `std.log.init` states it on its backend (docs/library/log.md),
//! since `emit` runs on whichever task logs.

use std::process::Command;

/// `karac check` on `src`: whether it passes, and its stderr.
fn check(tag: &str, src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!(
        "karac-cross-task-bound-{tag}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
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

#[test]
fn log_init_refuses_a_backend_that_holds_a_shared_value() {
    let (ok, err) = check(
        "log-shared",
        "import std.log;\n\
         shared struct Box { n: i64 }\n\
         struct B { b: Box }\n\
         impl log.Backend for B { fn emit(mut ref self, record: log.Record) { } }\n\
         fn main() { log.init(B { b: Box { n: 0 } }); }\n",
    );
    assert!(!ok);
    for want in [
        "main.kara:5:13: error[E_NOT_CROSS_TASK]: `B: CrossTask` is not satisfied: `B` cannot cross a task boundary",
        "note: type `B` reaches `Box` at field 'b'",
        "help: declare `Box` as a `sync` type to allow cross-task sharing",
    ] {
        assert!(err.contains(want), "want {want:?} in\n{err}");
    }
}

#[test]
fn log_init_takes_a_backend_of_owned_values() {
    let src = "import std.log;\n\
               struct B { n: i64 }\n\
               impl log.Backend for B { fn emit(mut ref self, record: log.Record) { println(record.msg); } }\n\
               fn main() { log.init(B { n: 0 }); log.info(\"hi\"); }\n";
    let (ok, err) = check("log-owned", src);
    assert!(ok, "{err}");
    let out = karac::mir::lower::run_source(src).expect("MIR interpreter");
    assert_eq!(out.output, "hi\n");
}

#[test]
fn a_cross_task_bound_is_computed_from_the_type() {
    let (ok, err) = check(
        "bound",
        "shared struct Cell { n: i64 }\n\
         sync struct Shared { n: i64 }\n\
         fn take[T: CrossTask](x: own T) -> i64 { 1 }\n\
         fn main() {\n\
             println(take(5));\n\
             println(take(vec![\"a\".to_string()]));\n\
             println(take(Shared { n: 1 }));\n\
             println(take(Some(Cell { n: 0 })));\n\
         }\n",
    );
    assert!(!ok);
    let errors: Vec<&str> = err
        .lines()
        .filter(|l| l.contains("E_NOT_CROSS_TASK"))
        .collect();
    assert_eq!(errors.len(), 1, "{err}");
    assert!(
        errors[0].contains("main.kara:8:9: error[E_NOT_CROSS_TASK]: `T: CrossTask` is not satisfied: `Option[Cell]` cannot cross a task boundary"),
        "{err}"
    );
}

#[test]
fn a_hand_written_cross_task_impl_is_refused() {
    let (ok, err) = check(
        "impl",
        "struct S { n: i64 }\nimpl CrossTask for S { }\nfn main() { }\n",
    );
    assert!(!ok);
    assert!(
        err.contains(
            "`impl CrossTask for S` is not allowed: `CrossTask` is computed by the compiler"
        ),
        "{err}"
    );
    assert!(!err.contains("undefined type"), "{err}");
}
