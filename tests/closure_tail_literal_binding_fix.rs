//! A closure's tail that reads a binding whose literal kept its default
//! `i64` (`let mut total = 0; .. total += move_cost; .. total`), beside a
//! `return i32.MAX`: Rust typed `total` from the closure's `-> i32`, which
//! `karac fix` drops, so the closure's returns conflicted with no fix. As at a
//! fn's tail (B-2026-11-08-20), the refusal names the binding and `karac fix`
//! writes `let mut total: i32 = 0`, after which the closure is an `i32` one and
//! nothing downstream is refused for it again. Seen in kata 2162's Rust arm.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn scratch(name: &str, src: &str) -> String {
    let path = std::env::temp_dir().join(format!("karac-{name}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    path.to_str().unwrap().to_string()
}

const COOK: &str = "fn cheapest(start_at: i32, move_cost: i32, push_cost: i32) -> i32 {
    let cost = |m: i32| {
        if m > 99 {
            return i32.MAX;
        }
        let mut at = start_at;
        let mut total = 0;
        for d in [m / 10, m % 10] {
            if d != at {
                total += move_cost;
                at = d;
            }
            total += push_cost;
        }
        total
    };
    cost(42).min(cost(100))
}

fn main() {
    println(cheapest(4, 3, 1));
}
";

#[test]
fn a_closure_tail_from_a_defaulted_literal_names_the_binding() {
    let file = scratch("closure-tail-literal-check", COOK);
    let out = karac(&["check", "--output=json", &file]);
    let _ = std::fs::remove_file(&file);
    let json = String::from_utf8_lossy(&out.stdout);
    assert!(
        json.contains("closure returns conflicting types: 'i64' vs 'i32': `total` is an 'i64' because its literal kept the default"),
        "{json}"
    );
    assert!(
        json.contains(
            "\"old\":\"        let mut total = 0;\",\"new\":\"        let mut total: i32 = 0;\""
        ),
        "{json}"
    );
    // One refusal: the closure takes `i32`, so `cost(42).min(..)` is not
    // refused again at the fn's tail.
    assert_eq!(json.matches("\"severity\":\"error\"").count(), 1, "{json}");
}

#[test]
fn karac_fix_annotates_a_closure_tails_defaulted_binding() {
    let file = scratch("closure-tail-literal-fix", COOK);
    let _ = karac(&["fix", &file]);
    let fixed = std::fs::read_to_string(&file).unwrap();
    let check = karac(&["check", &file]);
    let _ = std::fs::remove_file(&file);
    assert!(fixed.contains("let mut total: i32 = 0;"), "{fixed}");
    assert!(
        check.status.success(),
        "{}\n{fixed}",
        String::from_utf8_lossy(&check.stderr)
    );
    let out = karac::mir::lower::run_source(&fixed)
        .expect("MIR interpreter")
        .output;
    // The 4 pushed where the finger rests, then a move to 2 and a push.
    assert_eq!(out, "5\n");
}
