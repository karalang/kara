//! B-2026-10-04-23: lending a loop element's payload binding to a `ref` parameter reads it in place

use super::*;

/// B-2026-10-04-23: the arm no longer deep-copies `inner` before lending it to
/// `depth_sum`, so the container's own payload is what the callee reads.
/// Nothing may be freed twice or left behind, with `String`-free and
/// `String`-carrying payloads alike.
#[test]
fn asan_ref_param_walk_over_a_loop_element_reads_in_place() {
    assert_clean_asan_run(
        r#"enum Nested {
    Int(i64),
    List(Vec[Nested]),
}

fn depth_sum(items: ref Vec[Nested], depth: i64) -> i64 {
    let mut total = 0;
    for item in items.iter() {
        match item {
            Int(v) => { total += v * depth; }
            List(inner) => { total += depth_sum(inner, depth + 1); }
        }
    }
    return total;
}

fn main() {
    let mut deep: Vec[Nested] = Vec.new();
    deep.push(Int(6));
    let mut mid: Vec[Nested] = Vec.new();
    mid.push(Int(4));
    mid.push(List(deep));
    let mut top: Vec[Nested] = Vec.new();
    top.push(Int(1));
    top.push(List(mid));
    top.push(List(Vec.new()));
    println(depth_sum(top, 1));
    println(depth_sum(top, 2));
    println(top.len());
}"#,
        &["27", "38", "3"],
        "b_2026_10_04_23 walk",
    );
    assert_clean_asan_run(
        r#"enum Doc {
    Word(String),
    Group(Vec[Doc]),
}

fn chars(items: ref Vec[Doc]) -> i64 {
    let mut total = 0;
    for item in items.iter() {
        match item {
            Word(s) => { total += s.len(); }
            Group(inner) => { total += chars(inner); }
        }
    }
    return total;
}

fn main() {
    let mut g: Vec[Doc] = Vec.new();
    g.push(Word("hello".to_string()));
    g.push(Word("worlds".to_string()));
    let mut top: Vec[Doc] = Vec.new();
    top.push(Group(g));
    top.push(Word("!".to_string()));
    println(chars(top));
    println(chars(top));
}"#,
        &["12", "12"],
        "b_2026_10_04_23 strings",
    );
}
