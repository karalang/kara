//! A list of tuple literals walked by a tuple pattern beside an `i32`: the
//! literals keep `i64` (design.md §8 Literals), so `x` in `let (x, y) =
//! (k + dx, k + dy)` is an `i64` and a push into a `Vec[i32]` narrows. The
//! refusal offers the suffix on the first literal in each position when the
//! list is bound by a `let` and walked by name through `.iter()`, and through
//! a tuple `let` as through a plain one (kata 1222's Rust arm,
//! B-2026-11-19-26).

use karac::{check_source, run_playground};

/// The E0200 fixes `src` gets, applied from the last offset back, once each.
fn fixed(src: &str) -> (usize, String) {
    let diags: Vec<_> = check_source(src)
        .into_iter()
        .filter(|d| d.code.as_deref() == Some("E0200"))
        .collect();
    let mut fixes: Vec<_> = diags.iter().flat_map(|d| d.fixes.iter().cloned()).collect();
    fixes.sort_by_key(|f| std::cmp::Reverse(f.offset));
    fixes.dedup_by_key(|f| f.offset);
    let mut out = src.to_string();
    for f in &fixes {
        out.replace_range(f.offset..f.offset + f.length, &f.replacement);
    }
    (diags.len(), out)
}

const NAMED: &str = "fn walk(king: Vec[i32]) -> Vec[Vec[i32]] {\n    let mut ans: Vec[Vec[i32]] = Vec.new();\n    let dirs = [(-1, -1), (0, 1), (1, 0)];\n    for (dx, dy) in dirs.iter() {\n        let mut (x, y) = (king[0] + dx, king[1] + dy);\n        x += dx;\n        ans.push([x, y]);\n    }\n    ans\n}\nfn main() { println(f\"{walk([3, 3])}\"); }\n";

#[test]
fn a_named_list_of_tuple_literals_walked_through_iter_offers_each_positions_suffix() {
    let (n, out) = fixed(NAMED);
    assert_eq!(n, 2, "{out}");
    assert!(
        out.contains("let dirs = [(-1i32, -1i32), (0, 1), (1, 0)];"),
        "{out}"
    );
    let after = run_playground(&out);
    assert!(after.ok, "{out}\n{:#?}", after.diagnostics);
    assert_eq!(after.stdout.concat(), "[[1, 2], [3, 4], [5, 3]]\n");
}

const TUPLE_LET: &str = "fn firsts(k: i32) -> Vec[i32] {\n    let mut v: Vec[i32] = Vec.new();\n    for (dx, dy) in [(-1, 2), (0, 1)] {\n        let (x, y) = (k + dx, k * dy);\n        v.push(x);\n        v.push(y);\n    }\n    v\n}\nfn main() { println(f\"{firsts(5)}\"); }\n";

#[test]
fn a_tuple_let_of_sums_over_a_tuple_literal_binding_offers_its_suffix() {
    let (n, out) = fixed(TUPLE_LET);
    assert_eq!(n, 2, "{out}");
    assert!(out.contains("in [(-1i32, 2i32), (0, 1)]"), "{out}");
    let after = run_playground(&out);
    assert!(after.ok, "{out}\n{:#?}", after.diagnostics);
    assert_eq!(after.stdout.concat(), "[4, 10, 5, 5]\n");
}

/// The control: a list annotated `i64` means `i64`, so nothing in it is
/// offered a suffix.
#[test]
fn an_annotated_list_of_tuples_is_not_offered_a_suffix() {
    let src = NAMED.replace("let dirs = [", "let dirs: Array[(i64, i64), 3] = [");
    let diags: Vec<_> = check_source(&src)
        .into_iter()
        .filter(|d| d.code.as_deref() == Some("E0200"))
        .collect();
    assert!(!diags.is_empty(), "{src}");
    assert!(
        diags
            .iter()
            .all(|d| d.fixes.iter().all(|f| !f.replacement.contains("i32"))),
        "{diags:#?}"
    );
}
