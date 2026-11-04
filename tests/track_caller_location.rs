//! `#[track_caller]` on the program's own functions (design.md §10): a
//! panic in a tracked function is reported at the call into it, and
//! through a chain of tracked functions at the first untracked caller's
//! call (B-2026-11-04-20). The isolating-group half, where the place lands
//! in `TaskFailed.Panicked`, is the corpus pin
//! `corpus/core/panic_track_caller_reports_the_call`.

use karac::mir::interp::Outcome;

/// The report a process-ending panic writes on stderr, with `run_source`'s
/// file name dropped.
fn panic_report(src: &str) -> (String, String) {
    let r = karac::mir::lower::run_source(src).unwrap_or_else(|e| panic!("build: {e}"));
    assert!(
        !matches!(r.outcome, Outcome::Error(_)),
        "run: {:?}\n--- output so far\n{}",
        r.outcome,
        r.output
    );
    (r.output, r.stderr.replace("<unknown>:", ""))
}

#[test]
fn track_caller_wrapper_reports_its_caller() {
    let src = r#"
#[track_caller]
fn must(o: Option[i64]) -> i64 {
    o.unwrap()
}

fn main() {
    println(must(Some(3)));
    let v = must(None);
    println(v);
}
"#;
    let (out, err) = panic_report(src);
    assert_eq!(out, "3\n");
    assert!(
        err.starts_with("runtime error: called unwrap() on None\n  at 9:13\n"),
        "stderr: {err}"
    );
}

#[test]
fn track_caller_is_transitive_and_stops_at_an_untracked_caller() {
    let src = r#"
#[track_caller]
fn must(o: Option[i64]) -> i64 {
    o.unwrap()
}

#[track_caller]
fn outer(o: Option[i64]) -> i64 {
    must(o) + 1
}

fn plain(o: Option[i64]) -> i64 {
    must(o)
}

fn main() {
    println(outer(Some(4)));
    println(plain(Some(5)));
    let a = outer(None);
    println(a);
}
"#;
    let (out, err) = panic_report(src);
    assert_eq!(out, "5\n5\n");
    assert!(
        err.starts_with("runtime error: called unwrap() on None\n  at 19:13\n"),
        "stderr: {err}"
    );
    let src2 = src.replace("let a = outer(None);", "let a = plain(None);");
    let (_, err) = panic_report(&src2);
    assert!(
        err.starts_with("runtime error: called unwrap() on None\n  at 13:5\n"),
        "stderr: {err}"
    );
}

#[test]
fn track_caller_places_its_own_panic_at_the_call() {
    let src = r#"
#[track_caller]
fn check(n: i64) -> i64 {
    if n < 0 {
        panic("negative")
    }
    100 / n
}

fn main() {
    println(check(5));
    println(check(-1));
}
"#;
    let (out, err) = panic_report(src);
    assert_eq!(out, "20\n");
    assert!(
        err.starts_with("runtime error: negative\n  at 12:13\n"),
        "stderr: {err}"
    );
}
