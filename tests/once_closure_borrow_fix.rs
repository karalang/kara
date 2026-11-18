//! E0235's fix borrows a capture a returned closure reads before the
//! statement. A shadowing `let best = ref best;` stands for the rest of the
//! block, so when a later statement writes `best` the borrow takes its own
//! name and the statement reads it; and E0500 names a use after the capture,
//! not the table's init loop before it (B-2026-11-17-150).

use karac::{check_source, run_playground};

const WRITES_LATER: &str = "fn main() {\n    let n = 3;\n    let mut best: Vec[Vec[i64]] = Vec.new();\n    for i in 0..n {\n        best.push([i, 0, 0]);\n    }\n    for step in 0..2 {\n        let total: i64 = (0..n).flat_map(|v| (0..n).map(|w| best[v][w])).sum();\n        println(total);\n        best[step][step] = total + 1;\n    }\n}\n";
const READS_ONLY: &str = "fn main() {\n    let n = 3;\n    let best: Vec[Vec[i64]] = [[0, 0, 0], [1, 0, 0], [2, 0, 0]];\n    let total: i64 = (0..n).flat_map(|v| (0..n).map(|w| best[v][w])).sum();\n    println(total);\n}\n";

fn e0235_fix(src: &str) -> String {
    let fixes: Vec<_> = check_source(src)
        .into_iter()
        .filter(|d| d.code.as_deref() == Some("E0235"))
        .flat_map(|d| d.fixes)
        .collect();
    assert_eq!(fixes.len(), 1, "{fixes:#?}");
    let f = &fixes[0];
    let mut out = src.to_string();
    out.replace_range(f.offset..f.offset + f.length, &f.replacement);
    out
}

#[test]
fn a_borrow_for_a_returned_closure_takes_a_new_name_when_the_block_writes_the_value_later() {
    let out = e0235_fix(WRITES_LATER);
    assert!(out.contains("let best_ref = ref best;"), "{out}");
    assert!(out.contains("best_ref[v][w]"), "{out}");
    assert!(out.contains("best[step][step] = total + 1;"), "{out}");
    let after = run_playground(&out);
    assert!(after.ok, "{out}\n{:#?}", after.diagnostics);
    assert_eq!(after.stdout.concat(), "3\n7\n");
}

#[test]
fn a_borrow_for_a_returned_closure_shadows_when_nothing_uses_the_value_later() {
    let out = e0235_fix(READS_ONLY);
    assert!(out.contains("let best = ref best;"), "{out}");
    let after = run_playground(&out);
    assert!(after.ok, "{out}\n{:#?}", after.diagnostics);
    assert_eq!(after.stdout.concat(), "3\n");
}

#[test]
fn an_escaping_capture_names_the_use_after_it_not_one_before() {
    let d = check_source(WRITES_LATER)
        .into_iter()
        .find(|d| d.code.as_deref() == Some("E0500"))
        .expect("E0500");
    assert_eq!(d.line, 10, "{d:#?}");
}
