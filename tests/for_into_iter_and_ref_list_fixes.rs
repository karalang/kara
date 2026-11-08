//! Two `karac fix` repairs mini_dask needed by hand.
//!
//! A move out of a bare `for` element over an owned collection got the
//! `.clone()` fix even when nothing used the collection after the loop, so
//! every element was copied out of a vector about to drop; the message
//! itself names `.into_iter()` (B-2026-11-07-30). It is now the fix when no
//! later use, and no loop around the loop, would see the collection
//! consumed.
//!
//! An unannotated `let chain = [k];` over `ref i64` elements is a
//! `Vec[ref i64]`, refused with no fix where a `Vec[i64]` is wanted; the
//! fix annotated the `let` (B-2026-11-07-31). Since design.md §8
//! (B-2026-11-08-19) such a literal is a `Vec[i64]` as written, so both
//! programs check with no fix.

use std::process::{Command, Output, Stdio};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

/// `karac check` refuses `src`; `karac fix` repairs it so that it checks
/// clean, contains `want`, lacks `not`, and prints `out` under `__mir-run`.
fn fixed_to(tag: &str, src: &str, want: &str, not: &str, out: &str) {
    let path = std::env::temp_dir().join(format!(
        "karac-into-iter-ref-list-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).expect("write");
    let before = karac(&["check"], &path);
    assert!(!before.status.success(), "{tag}: check accepted it");
    let fix = karac(&["fix"], &path);
    assert!(
        fix.status.success(),
        "{tag}: fix: {}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let text = std::fs::read_to_string(&path).expect("read");
    let after = karac(&["check"], &path);
    let run = karac(&["__mir-run"], &path);
    let _ = std::fs::remove_file(&path);
    assert!(
        after.status.success(),
        "{tag}: still refused after the fix:\n{}\n{text}",
        String::from_utf8_lossy(&after.stdout)
    );
    assert!(text.contains(want), "{tag}: {text}");
    assert!(not.is_empty() || !text.contains(not), "{tag}: {text}");
    assert_eq!(String::from_utf8_lossy(&run.stdout), out, "{tag}");
}

#[test]
fn an_owned_vec_parameter_iterated_once_gets_into_iter() {
    fixed_to(
        "r1",
        "#[derive(Clone)]\nenum Value { Int(i64), List(Vec[i64]) }\n\nfn lists(args: own Vec[Value]) -> Vec[Vec[i64]] {\n    let mut ls: Vec[Vec[i64]] = Vec.new();\n    for a in args {\n        match a {\n            Value.List(items) => ls.push(items),\n            Value.Int(_) => {}\n        }\n    }\n    ls\n}\n\nfn main() {\n    println(lists([Value.List([1, 2]), Value.Int(3)]).len());\n}\n",
        "for a in args.into_iter() {",
        "items.clone()",
        "1\n",
    );
}

#[test]
fn an_owned_vec_used_after_the_loop_keeps_the_clone() {
    fixed_to(
        "r1b",
        "#[derive(Clone)]\nenum Value { Int(i64), List(Vec[i64]) }\n\nfn lists(args: own Vec[Value]) -> Vec[Vec[i64]] {\n    let mut ls: Vec[Vec[i64]] = Vec.new();\n    for a in args {\n        match a {\n            Value.List(items) => ls.push(items),\n            Value.Int(_) => {}\n        }\n    }\n    println(args.len());\n    ls\n}\n\nfn main() {\n    println(lists([Value.List([1, 2]), Value.Int(3)]).len());\n}\n",
        "ls.push(items.clone())",
        ".into_iter()",
        "2\n1\n",
    );
}

#[test]
fn an_owned_vec_iterated_in_an_outer_loop_keeps_the_clone() {
    fixed_to(
        "r1c",
        "#[derive(Clone)]\nenum Value { Int(i64), List(Vec[i64]) }\n\nfn main() {\n    let vals = [Value.List([1, 2]), Value.Int(3)];\n    let mut ls: Vec[Vec[i64]] = Vec.new();\n    for _ in 0..2 {\n        for a in vals {\n            if let Value.List(items) = a {\n                ls.push(items);\n            }\n        }\n    }\n    println(ls.len());\n}\n",
        "ls.push(items.clone());",
        ".into_iter()",
        "2\n",
    );
}

#[test]
fn a_vec_declared_inside_the_outer_loop_gets_into_iter() {
    fixed_to(
        "r1d",
        "fn main() {\n    let mut out: Vec[String] = Vec.new();\n    for _ in 0..2 {\n        let mut names: Vec[String] = Vec.new();\n        names.push(\"a\".to_string());\n        for n in names {\n            out.push(n);\n        }\n    }\n    println(out.len());\n}\n",
        "for n in names.into_iter() {",
        "n.clone()",
        "2\n",
    );
}

#[test]
fn a_field_of_an_owned_element_gets_into_iter_beside_another_field() {
    fixed_to(
        "r1e",
        "struct Step { func: i64, inputs: Vec[String] }\nfn main() {\n    let steps: Vec[Step] = [Step { func: 1, inputs: [\"a\".to_string()] }];\n    let mut out: Vec[String] = Vec.new();\n    for step in steps.into_iter() {\n        for input in step.inputs {\n            out.push(input);\n        }\n        println(step.func);\n    }\n    println(out.len());\n}\n",
        "for input in step.inputs.into_iter() {",
        "input.clone()",
        "1\n1\n",
    );
}

#[test]
fn a_use_past_a_returning_branch_does_not_keep_the_clone() {
    fixed_to(
        "ret",
        "#[derive(Clone)]\nenum Value { Int(i64), List(Vec[i64]) }\n\nfn count(lists: bool, args: own Vec[Value]) -> i64 {\n    if lists {\n        let mut ls: Vec[Vec[i64]] = Vec.new();\n        for a in args {\n            if let Value.List(items) = a {\n                ls.push(items);\n            }\n        }\n        return ls.len();\n    }\n    args.len()\n}\n\nfn main() {\n    println(count(true, [Value.List([1]), Value.Int(2)]));\n    println(count(false, [Value.Int(2)]));\n}\n",
        "for a in args.into_iter() {",
        "items.clone()",
        "1\n1\n",
    );
}

/// `src` checks clean as written and prints `out` under `__mir-run`.
fn accepted_as_written(tag: &str, src: &str, out: &str) {
    let path = std::env::temp_dir().join(format!(
        "karac-into-iter-ref-list-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).expect("write");
    let check = karac(&["check"], &path);
    let run = karac(&["__mir-run"], &path);
    let _ = std::fs::remove_file(&path);
    assert!(
        check.status.success(),
        "{tag}: refused:\n{}",
        String::from_utf8_lossy(&check.stdout)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), out, "{tag}");
}

#[test]
fn an_unannotated_list_of_copy_refs_is_a_list_of_values() {
    accepted_as_written("r2", "fn chains(order: Vec[i64]) -> Vec[Vec[i64]] {\n    let mut out: Vec[Vec[i64]] = Vec.new();\n    for k in order.iter() {\n        let chain = [k];\n        out.push(chain);\n    }\n    out\n}\n\nfn main() {\n    println(chains([1, 2]).len());\n}\n", "2\n");
}

#[test]
fn an_unannotated_mut_list_of_copy_refs_is_a_list_of_values() {
    accepted_as_written("r2b", "struct Unit { chain: Vec[i64] }\nfn main() {\n    let order: Vec[i64] = [3, 1, 2];\n    let mut units: Vec[Unit] = Vec.new();\n    for k in order.iter() {\n        let mut chain = [k];\n        chain.insert(0, k);\n        units.push(Unit { chain: chain });\n    }\n    println(units[2].chain.len());\n}\n", "2\n");
}
