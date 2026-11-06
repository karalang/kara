//! B-2026-11-06-9: core semantics §6.4 removed the RC fallback, so where it
//! would have fired the program is an E0500 and `karac query` reports no RC:
//! no `rc_fallback_decision`, no `rc_ops`, no `rc_values`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn karac(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(dir)
        .args(args)
        .output()
        .expect("karac")
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-norc-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

const CLOSURE_MOVE: &str = "struct Owned { x: i64 }
fn take(o: own Owned) { }
fn main() {
    let o = Owned { x: 1 };
    let _f = || take(o);
    let _u = o;
}
";

const BRANCH_MOVE: &str = "struct Data { value: i64 }
fn consume(d: own Data) {}
fn use_d(d: own Data) {}
#[allow(rc_fallback)]
fn process(d: own Data) {
    match d.value {
        0 => consume(d),
        _ => {}
    }
    use_d(d);
}
fn main() { process(Data { value: 0 }); }
";

#[test]
fn query_reports_no_rc_fallback_where_check_reports_the_move() {
    let dir = scratch("closure");
    std::fs::write(dir.join("c.kara"), CLOSURE_MOVE).unwrap();
    let check = karac(&dir, &["check", "c.kara"]);
    assert!(!check.status.success());
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&check.stdout),
        String::from_utf8_lossy(&check.stderr)
    );
    assert!(said.contains("was moved into a closure"), "{said}");
    let queries = karac(&dir, &["query", "queries", "c.kara"]);
    let out = String::from_utf8_lossy(&queries.stdout);
    assert!(!out.contains("rc_fallback_decision"), "{out}");
    let ownership = karac(&dir, &["query", "ownership", "c.kara.main"]);
    let out = String::from_utf8_lossy(&ownership.stdout);
    assert!(out.contains("\"rc_values\":[]"), "{out}");
}

#[test]
fn cost_summary_counts_no_rc_ops_under_the_core_rules() {
    let dir = scratch("branch");
    std::fs::write(dir.join("b.kara"), BRANCH_MOVE).unwrap();
    let out = karac(&dir, &["query", "cost-summary", "b.kara"]);
    assert!(out.status.success());
    let json = String::from_utf8_lossy(&out.stdout);
    assert!(
        json.contains("\"rc_ops\":{\"count\":0,\"rc\":0,\"arc\":0,\"suppressed\":0}"),
        "{json}"
    );
    assert!(json.contains("\"by_function\":[]"), "{json}");
}
