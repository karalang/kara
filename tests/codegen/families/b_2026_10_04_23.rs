//! B-2026-10-04-23: a match-arm binding over a borrowed `for` element, lent to a `ref` parameter, is not deep-copied first

use super::*;

const WALK: &str = r#"enum Nested {
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
}"#;

/// B-2026-10-04-23: `List(inner) => depth_sum(inner, depth + 1)` over `for item
/// in items.iter()` read as an escape of `inner`, so the arm took an
/// independent deep copy of the payload (`dcopy` / `karac_clone_enum_Nested`)
/// on every call and freed it after: a read-only tree walk copied each
/// subtree once per level above it. A `ref` parameter cannot keep its
/// argument, so the walk now reads the element in place. The answers are
/// `1 + 4*2 + 6*3 = 27`, then from depth 2 (`2 + 4*3 + 6*4 = 38`), and the
/// tree is intact afterwards.
#[test]
fn e2e_ref_param_walk_over_a_loop_element_reads_in_place() {
    assert_eq!(run_program(WALK).as_deref(), Some("27\n38\n3\n"));
    let ir = ir_for(WALK);
    let start = ir.find("@depth_sum(").expect("depth_sum is defined");
    let body = &ir[start..];
    let body = &body[..body.find("\n}\n").expect("depth_sum ends")];
    assert!(
        !body.contains("karac_clone_enum_Nested") && !body.contains("dcopy."),
        "depth_sum must not deep-copy the payload it lends to its own ref param"
    );
}
