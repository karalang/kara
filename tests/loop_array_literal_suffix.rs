//! `for step in [60, 15, 5, 1].iter()` beside an `i32` accumulator: the
//! literals keep `i64` (design.md §8 Literals), so `diff / step` narrows. The
//! refusal offers the first literal's suffix through `.iter()` as it already
//! did for a bare array (B-2026-11-17-143).

use karac::{check_source, run_playground};

const SRC: &str = "fn ops(d: i32) -> i32 {\n    let mut diff = d;\n    let mut ops: i32 = 0;\n    for step in [60, 15, 5, 1].iter() {\n        ops += diff / step;\n        diff %= step;\n    }\n    ops\n}\nfn main() { println(ops(125)); }\n";

#[test]
fn an_iterated_array_literal_beside_an_i32_offers_the_first_literals_suffix() {
    let diags: Vec<_> = check_source(SRC)
        .into_iter()
        .filter(|d| d.code.as_deref() == Some("E0200"))
        .collect();
    assert_eq!(diags.len(), 2, "{diags:#?}");
    let f = &diags[0].fixes[0];
    assert!(diags
        .iter()
        .all(|d| d.fixes.len() == 1 && d.fixes[0].offset == f.offset));
    let mut out = SRC.to_string();
    out.replace_range(f.offset..f.offset + f.length, &f.replacement);
    assert!(out.contains("[60i32, 15, 5, 1].iter()"), "{out}");
    let after = run_playground(&out);
    assert!(after.ok, "{out}\n{:#?}", after.diagnostics);
    assert_eq!(after.stdout.concat(), "3\n");
}
