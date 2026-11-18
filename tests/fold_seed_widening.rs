//! A `fold` whose seed takes a narrow integer from a value
//! (`fold((vals[0], 1), |(s, c), k| (s + k, c + 1))`, `vals: Vec[i32]`, `i64`
//! items) is refused at `s + k`; the fix widens the seed (`vals[0] as i64`),
//! where the old advice `as i32` would truncate the sum (B-2026-11-18-44).

use karac::{check_source, run_playground};

const SRC: &str = "fn total(vals: Vec[i32], kids: Vec[i64]) -> i64 {\n    let (s, c) = kids.iter().fold((vals[0], 1), |(s, c), k| (s + k, c + 1));\n    s + c\n}\nfn main() { println(total([5], [10, 20])); }\n";

#[test]
fn a_fold_seed_narrower_than_its_accumulator_is_widened() {
    let d = check_source(SRC)
        .into_iter()
        .find(|d| d.code.as_deref() == Some("E0200"))
        .expect("E0200");
    assert!(d.message.contains("its seed `vals[0]`"), "{d:#?}");
    assert!(!d.message.contains("'as i32'"), "{d:#?}");
    let mut fixes = d.fixes.clone();
    assert!(!fixes.is_empty(), "{d:#?}");
    fixes.sort_by_key(|f| std::cmp::Reverse(f.offset));
    let mut out = SRC.to_string();
    for f in fixes {
        out.replace_range(f.offset..f.offset + f.length, &f.replacement);
    }
    assert!(out.contains("fold((vals[0] as i64, 1)"), "{out}");
    let after = run_playground(&out);
    assert!(after.ok, "{out}\n{:#?}", after.diagnostics);
    assert_eq!(after.stdout.concat(), "38\n");
}
