//! B-2026-11-08-40 — `karac::run_playground` (the playground, and the book
//! test's gate for `kara,run` blocks) runs exactly the programs `karac
//! check` and the editor accept. It ran the older static checks, so a bare
//! `for` that moves its elements out (E0515 on the command line) ran and
//! printed a result, while an atomic operation without the deprecated
//! `MemoryOrdering` argument, which the library specifies, was refused.

use karac::run_playground;

#[test]
fn a_bare_for_moving_its_elements_out_does_not_run() {
    let r = run_playground(
        "fn square(n: i64) -> i64 { n * n }\n\
         fn main() {\n    \
             let mut pool: TaskGroup = TaskGroup.new();\n    \
             let mut handles: Vec[TaskHandle[i64]] = Vec.new();\n    \
             handles.push(pool.spawn(|| square(3)));\n    \
             let mut total = 0;\n    \
             for handle in handles {\n        \
                 total = total + handle.join();\n    \
             }\n    \
             println(total);\n\
         }\n",
    );
    assert!(!r.ok && r.stdout.is_empty(), "{:?}", r.stdout);
    assert!(
        r.diagnostics
            .iter()
            .any(|d| d.code.as_deref() == Some("E0515") && d.severity == "error"),
        "{:?}",
        r.diagnostics.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
}

#[test]
fn an_atomic_without_an_ordering_runs() {
    let r = run_playground(
        "sync struct Counter { count: Atomic[i64] }\n\
         fn bump(counter: ref Counter) { let _ = counter.count.fetch_add(1); }\n\
         fn main() {\n    \
             let counter: Counter = Counter { count: Atomic.new(0) };\n    \
             par { bump(counter), bump(counter), bump(counter) };\n    \
             println(counter.count.load());\n\
         }\n",
    );
    assert!(
        r.ok,
        "{:?}",
        r.diagnostics.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
    assert_eq!(r.stdout.concat(), "3\n");
}
