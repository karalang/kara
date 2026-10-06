//! B-2026-10-06-124: an unsolved collection type is pinned by the
//! declared slot it flows into.

use super::*;

/// B-2026-10-06-124: `Vec.new()` / `Map.new()` / `Set.new()` / `vec![]`
/// handed straight to a `ref` or `mut ref` parameter, and an unpinned
/// `let e = Vec.new();` handed to a parameter, a struct field, a return or an
/// annotated `let`, were refused at typecheck; they now take the slot's type,
/// and codegen sees the concrete element type the interpreter does.
#[test]
fn e2e_unsolved_collection_takes_the_slot_type() {
    let src = r#"struct Holder { xs: Vec[i64] }
fn total(xs: ref Vec[i64]) -> i64 {
    let mut s = 0;
    for x in xs.iter() {
        s += x;
    }
    s
}
fn count_m(m: ref Map[i64, String]) -> i64 { m.len() }
fn sz(s: ref Set[i64]) -> i64 { s.len() }
fn grow(xs: mut ref Vec[i64]) -> i64 {
    xs.push(4);
    xs.len()
}
fn owned_len(xs: Vec[i64]) -> i64 { xs.len() }
fn give() -> Vec[i64] {
    let e = Vec.new();
    e
}
fn main() {
    println(f"{total(Vec.new())} {count_m(Map.new())} {sz(Set.new())} {total(vec![])} {grow(mut Vec.new())}");
    let e = Vec.new();
    println(f"{owned_len(e)}");
    let mut f = Vec.new();
    println(f"{grow(mut f)} {total(f)}");
    let g = Vec.new();
    let h = Holder { xs: g };
    let k = Vec.new();
    let typed: Vec[i64] = k;
    println(f"{h.xs.len()} {give().len()} {typed.len()}");
}
"#;
    assert_eq!(
        run_program(src),
        Some("0 0 0 0 1\n0\n1 4\n0 0 0\n".to_string())
    );
}
