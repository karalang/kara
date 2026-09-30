//! B-2026-09-30-32 -- a discarded collection literal that lowers to a `Vec`
//! frees its buffer, and a `match` arm yielding one runs its element bodies.

use super::*;

/// B-2026-09-30-32 — `let _ = [1, 2];`, `[1, 2];`, `Vec[..]` and the
/// `if` / `match` arms that yield such a literal leaked the buffer on every
/// compiled surface (the aggregate registrar declines a `Vec` handle). The
/// bare-bodied `match` arm also ran no element body compiled. Output is
/// asserted here; the memory side is the ASAN twin's.
#[test]
fn e2e_discarded_vec_literal_frees_buffer_and_runs_bodies() {
    let src = r#"struct W1 { v: i64, s: String }
impl Drop for W1 { fn drop(mut ref self) { println(f"dW1_{self.v}") } }
struct P { v: i64 }
impl Drop for P { fn drop(mut ref self) { println(f"dP{self.v}") } }
fn main() {
    let _ = [1, 2];
    [1, 2];
    let _ = Vec[1, 2];
    Vec[1, 2];
    let _ = [f"a{1}", f"b{2}"];
    println("a");
    let _ = [W1 { v: 1, s: f"a{1}" }];
    println("b");
    [W1 { v: 2, s: f"a{2}" }, W1 { v: 3, s: f"b{3}" }];
    println("c");
    let _ = Vec[P { v: 4 }];
    Vec[P { v: 5 }];
    println("d");
    let _ = [[1, 2], [3]];
    let mut i = 0;
    while i < 3 { [f"x{i}"]; i = i + 1; }
    println("e");
    let c = true;
    if c { [1, 2] } else { [3] };
    if c { [W1 { v: 8, s: f"a{8}" }] } else { [W1 { v: 9, s: f"b{9}" }] };
    println("f");
    let d = 2;
    match d { 1 => [f"p{1}"], _ => [f"q{2}"] };
    match d { 1 => [1], _ => [2, 3] };
    match d { 1 => [W1 { v: 10, s: f"a{10}" }], _ => [W1 { v: 11, s: f"b{11}" }] };
    let _ = match d { 1 => [f"x{1}"], _ => [f"y{2}"] };
    println("end");
}
"#;
    let want = "a\ndW1_1\nb\ndW1_2\ndW1_3\nc\ndP4\ndP5\nd\ne\ndW1_8\nf\ndW1_11\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
