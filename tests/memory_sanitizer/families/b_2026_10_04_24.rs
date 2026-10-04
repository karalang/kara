//! B-2026-10-04-24 — a fresh `Option` of a user enum whose arm does not take
//! the payload frees the payload's heap along with the box.

use super::*;

/// `while let Some(item) = stack.pop() { total += 1 }` over `Vec[Nested]`
/// freed each popped box over an unwalked interior, losing the inner `Vec`'s
/// buffer (128 B here; kata 339's explicit-stack arm lost about 4.5 MB). The
/// same held for `Some(_)` and an unmentioned binding in `if let` / `match`.
/// A binding the arm moves on or destructures further keeps the interior, so
/// those spellings are pinned beside it.
#[test]
fn asan_fresh_option_user_enum_payload_freed_with_box() {
    assert_clean_asan_run(
        r#"enum Nested { Int(i64), List(Vec[Nested]) }
fn mk() -> Option[Nested] { let mut l2: Vec[Nested] = Vec.new(); l2.push(Nested.Int(1)); return Some(Nested.List(l2)) }
fn main() {
    let mut stack: Vec[Nested] = Vec.new();
    stack.push(Nested.List(Vec.new()));
    let mut l2: Vec[Nested] = Vec.new();
    l2.push(Nested.Int(1));
    stack.push(Nested.List(l2));
    let mut total = 0;
    while let Some(item) = stack.pop() { total += 1; }
    if let Some(item) = mk() { total += 1; }
    if let Some(_) = mk() { total += 1; }
    match mk() { Some(item) => { total += 1; } None => {} }
    match mk() { Some(_) => { total += 1; } None => {} }
    let mut keep: Vec[Nested] = Vec.new();
    if let Some(item) = mk() { keep.push(item); }
    if let Some(item) = mk() { let k = item; total += 1; }
    match mk() { Some(item) => match item { Nested.List(xs) => { total += 1; } _ => {} }, None => {} }
    println(total);
    println(keep.len());
}
"#,
        &["8", "1"],
        "fresh_option_user_enum_payload",
    );
}

/// The payload's element runs a user `Drop` body: the body ran and the
/// `Vec[D]` buffer leaked.
#[test]
fn asan_fresh_option_user_enum_drop_element_payload_freed() {
    assert_clean_asan_run(
        r#"struct D { id: i64 }
impl Drop for D { fn drop(mut ref self) { println(f"drop {self.id}") } }
enum E { A(i64), B(Vec[D]) }
fn main() {
    let mut stack: Vec[E] = Vec.new();
    let mut v: Vec[D] = Vec.new();
    v.push(D { id: 1 });
    stack.push(E.B(v));
    stack.push(E.A(3));
    let mut total = 0;
    while let Some(item) = stack.pop() { total += 1; }
    println(f"total {total}");
}
"#,
        &["drop 1", "total 2"],
        "fresh_option_user_enum_drop_element",
    );
}
