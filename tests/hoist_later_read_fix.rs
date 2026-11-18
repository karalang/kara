//! When a later argument reads a value an earlier argument moves into an
//! `own` parameter (`push(acc, acc.len() + i)`), the E0500 fix reads it first
//! in a `let` rather than cloning the moved value: arguments evaluate left to
//! right, so no copy is needed, and in a `fold` a clone per step is quadratic
//! (B-2026-11-17-145).

use karac::{check_source, run_playground};

const PUSH: &str = "fn push(v: own Vec[i64], x: i64) -> Vec[i64] {\n    let mut v = v;\n    v.push(x);\n    v\n}\n";

/// `src` with every machine fix applied until none is left; the result checks
/// clean and prints `want`.
fn fixed(src: &str, want: &str) -> String {
    let mut out = src.to_string();
    for _ in 0..4 {
        let mut fixes: Vec<_> = check_source(&out)
            .into_iter()
            .flat_map(|d| d.fixes)
            .collect();
        if fixes.is_empty() {
            break;
        }
        fixes.sort_by_key(|f| std::cmp::Reverse(f.offset));
        let first = fixes.remove(0);
        out.replace_range(
            first.offset..first.offset + first.length,
            &first.replacement,
        );
    }
    let after = run_playground(&out);
    assert!(after.ok, "{out}\n{:#?}", after.diagnostics);
    assert_eq!(after.stdout.concat(), want, "{out}");
    out
}

#[test]
fn a_later_argument_read_of_a_moved_value_is_hoisted_not_cloned() {
    let src = format!(
        "{PUSH}fn main() {{\n    let acc: Vec[i64] = [1, 2];\n    let r = push(acc, acc.len() + 1);\n    \
         println(r.len());\n}}\n"
    );
    let out = fixed(&src, "3\n");
    assert!(
        out.contains("{ let acc_len = acc.len(); push(acc, acc_len + 1) }"),
        "{out}"
    );
    assert!(!out.contains(".clone()"), "{out}");
}

#[test]
fn a_fold_step_reading_its_moved_accumulator_is_hoisted_not_cloned() {
    let src = format!(
        "{PUSH}fn main() {{\n    let r = (0..5).fold(Vec.new(), |acc: Vec[i64], i| push(acc, acc.len() + i));\n    \
         println(r.len());\n}}\n"
    );
    let out = fixed(&src, "5\n");
    assert!(out.contains("|acc: own Vec[i64], i|"), "{out}");
    assert!(out.contains("let acc_len = acc.len();"), "{out}");
    assert!(!out.contains(".clone()"), "{out}");
}

#[test]
fn a_later_argument_that_is_not_a_plain_read_keeps_the_clone() {
    let src = format!(
        "{PUSH}fn main() {{\n    let acc: Vec[i64] = [1, 2];\n    let r = push(acc, acc[0]);\n    \
         println(r.len());\n}}\n"
    );
    let out = fixed(&src, "3\n");
    assert!(out.contains("push(acc.clone(), acc[0])"), "{out}");
}
