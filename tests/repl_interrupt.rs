//! B-2026-11-20-132: `karac::mir::interp::interrupt` (the Jupyter kernel's
//! `interrupt_request`) stops a running cell at its next call, loop
//! back-edge or sleep, as a panic there would: the cell fails with
//! `runtime error: interrupted`, and the session keeps the cells before it.
//! One test, because the flag is process-wide.

use karac::mir::interp::{clear_interrupt, interrupt};
use std::time::{Duration, Instant};

fn interrupted_after(s: &mut karac::repl::Session, cell: &str, after_ms: u64) -> Vec<String> {
    clear_interrupt();
    let t = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(after_ms));
        interrupt();
    });
    let start = Instant::now();
    let out = s.evaluate_cell_captured(cell);
    t.join().unwrap();
    clear_interrupt();
    assert!(
        start.elapsed() < Duration::from_secs(30),
        "`{cell}` ran {:?}",
        start.elapsed()
    );
    let mut all = out.errors.clone();
    all.insert(0, out.stdout);
    all
}

#[test]
fn repl_interrupt_stops_a_cell_as_a_panic_does() {
    let mut s = karac::repl::Session::new();
    for cell in [
        "struct R { id: i64 }",
        "impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id}\"); } }",
        "fn spin(n: i64) -> i64 { let mut i = n; loop { i = i + 1; if i < 0 { return i; } } }",
        "fn down(n: i64) -> i64 { if n < 0 { return n; } down(n + 1) }",
        "let base = 40;",
    ] {
        let out = s.evaluate_cell_captured(cell);
        assert!(out.errors.is_empty(), "{cell}: {:?}", out.errors);
    }
    // What a panic does with a live value, for the interrupt to match.
    let panicked = s.evaluate_cell_captured("let r = R { id: 1 }; panic(\"p\");");
    assert!(
        panicked.errors[0].starts_with("runtime error: p"),
        "{panicked:?}"
    );
    let dropped_on_panic = panicked.stdout.contains("drop 1");

    let cases = [
        // A loop's back-edge.
        "let r = R { id: 1 }; let k = spin(0);",
        // A call (the loop is a recursion with no back-edge).
        "let r = R { id: 1 }; let k = down(0);",
        // Inside a task.
        "let r = R { id: 1 }; let g = TaskGroup.new(); let t = g.spawn(|| spin(1)); println(t.join());",
        // Inside a par block.
        "let r = R { id: 1 }; let (a, b) = par { spin(1), spin(2) }; println(a + b);",
        // During a sleep.
        "let r = R { id: 1 }; sleep_ms(600000);",
    ];
    for cell in cases {
        let out = interrupted_after(&mut s, cell, 300);
        let (stdout, errors) = (&out[0], &out[1..]);
        assert!(
            errors
                .first()
                .is_some_and(|e| e.starts_with("runtime error: interrupted")),
            "{cell}: {out:?}"
        );
        assert_eq!(
            stdout.contains("drop 1"),
            dropped_on_panic,
            "{cell}: drops as a panic does: {stdout:?}"
        );
        // The session is as it was.
        let next = s.evaluate_cell_captured("println(base + 2);");
        assert!(next.errors.is_empty(), "after {cell}: {:?}", next.errors);
        assert_eq!(next.stdout, "42\n", "after {cell}");
    }
}
