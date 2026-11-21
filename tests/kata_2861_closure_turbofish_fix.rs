//! `let to32 = |v: ref Vec[i64]| v.iter().map(..).collect::<Vec<i32>>();`
//! (kata 2861, LeetCode's Rust wrapper idiom): deleting the turbofish leaves
//! the closure's `collect` tail with nothing to infer from, which `karac
//! check` then refuses with a type that "does not lower". When the closure
//! is a `let`'s whole initializer with every parameter typed and the
//! turbofish is on its `collect` / `sum` / `product` tail, the fix writes the
//! closure's type onto the binding, as the arrow's fix does for a Rust return
//! type (B-2026-10-29-7). Any other closure keeps the delete-only fix.

fn fixed(src: &str) -> String {
    let result = karac::parse(src);
    let mut edits: Vec<_> = result.fix_edits.values().cloned().collect();
    edits.extend(result.fix_diffs.values().flatten().cloned());
    edits.sort_by_key(|e| std::cmp::Reverse(e.offset));
    let mut out = src.to_string();
    for e in edits {
        out.replace_range(e.offset..e.offset + e.length, &e.replacement);
    }
    out
}

#[test]
fn kata_2861_a_closure_tail_turbofish_moves_onto_the_binding() {
    let src = "fn main() {\n\
               \x20   let stock: Vec[i64] = [3, 4, 5].to_vec();\n\
               \x20   let to32 = |v: Vec[i64]| v.iter().map(|x| x as i32).collect::<Vec<i32>>();\n\
               \x20   let total = |n: i64| { let m = n + 1; (0..m).sum::<i64>() };\n\
               \x20   let small: Vec[i32] = to32(stock);\n\
               \x20   println(f\"{small} {total(3)}\");\n}\n";
    let out = fixed(src);
    for want in [
        "let to32: Fn(ref Vec[i64]) -> Vec[i32] = |v: Vec[i64]| v.iter().map(|x| x as i32).collect();",
        "let total: Fn(i64) -> i64 = |n: i64| { let m = n + 1; (0..m).sum() };",
    ] {
        assert!(out.contains(want), "missing {want:?} in:\n{out}");
    }
    let run = karac::mir::lower::run_source(&out).unwrap_or_else(|e| panic!("build: {e}"));
    assert_eq!(run.output, "[3, 4, 5] 6\n");
}

#[test]
fn kata_2861_an_untyped_parameter_or_another_method_keeps_the_delete() {
    for (src, want) in [
        (
            "let to32 = |v| v.iter().collect::<Vec<i64>>();",
            "let to32 = |v| v.iter().collect();",
        ),
        (
            "let first = |v: Vec[i64]| v.iter().cloned().collect::<Vec<i64>>().len();",
            // mid-chain: the call is rewritten in place, as before
            "let first = |v: Vec[i64]| Vec[i64].from_iter(v.iter().cloned()).len();",
        ),
        (
            "f(|v: Vec[i64]| v.iter().collect::<Vec<i64>>());",
            "f(|v: Vec[i64]| v.iter().collect());",
        ),
    ] {
        let src = format!("fn main() {{\n{src}\n}}\n");
        let out = fixed(&src);
        assert!(out.contains(want), "missing {want:?} in:\n{out}");
    }
}
