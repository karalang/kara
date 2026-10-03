//! B-2026-10-03-12 -- `assert_eq` / `assert_ne` compare their operands the way
//! the `==` operator does, by TYPE, not by LLVM shape.

use super::*;

/// B-2026-10-03-12 — `compile_assert_eq` routed only enums to a type-directed
/// comparator and sent everything else to the shape-directed `compile_binop`,
/// where a `Vec`, a `String` and a struct whose first field is a pointer all
/// look like `{ptr, i64, i64}`. Before, compiled: two equal `Vec[String]`
/// failed `assert_eq`, `[1, 2]` and `[1, 9]` failed `assert_ne`, a struct
/// differing only in its last field failed `assert_ne`, a three-element tuple
/// panicked the compiler and an `Array[String, 2]` was refused. `--interp`
/// printed every line.
#[test]
fn e2e_assert_eq_compares_by_type() {
    let run = run_program_capturing(
        r#"shared struct Node { v: i64 }
struct T3 { h: Node, a: i64, b: i64 }
fn names(t: String) -> Vec[String] { let mut v: Vec[String] = Vec.new(); v.push(t); v }
fn main() {
    let a = names(f"a{1}");
    let b = names(f"a{1}");
    assert_eq(a.clone(), b.clone()); println("vs eq")
    assert_ne(a.clone(), names(f"z{2}")); println("vs ne")
    let mut x: Vec[i64] = Vec.new(); x.push(1); x.push(2);
    let mut y: Vec[i64] = Vec.new(); y.push(1); y.push(9);
    assert_ne(x.clone(), y.clone()); println("vi ne")
    assert_eq(x[0..2], x[0..2]); println("sl eq")
    let t = (1, 2, 3);
    assert_ne(t, (1, 2, 4)); println("tu ne")
    assert_eq(t, (1, 2, 3)); println("tu eq")
    let n = Node { v: 1 };
    assert_ne(T3 { h: n, a: 1, b: 2 }, T3 { h: n, a: 1, b: 3 }); println("st ne")
    assert_eq([f"a{1}", f"b{2}"], [f"a{1}", f"b{2}"]); println("ar eq")
    println(a.len() + b.len())
}
"#,
    );
    let Some(run) = run else { return };
    assert_eq!(
        run.stdout, "vs eq\nvs ne\nvi ne\nsl eq\ntu ne\ntu eq\nst ne\nar eq\n2\n",
        "assert_eq / assert_ne must compare by type; stderr: {}",
        run.stderr
    );
    assert!(run.status.success(), "stderr: {}", run.stderr);
}

/// B-2026-10-03-12 — the other direction: an `assert_eq` over two UNEQUAL
/// `Vec[i64]` whose first bytes agree must FAIL. Before, the byte compare read
/// `len` bytes of an element buffer, so `[1, 2] == [1, 9]` held and the
/// assertion passed silently. A `Vec` operand is not formatted as text in the
/// failure record (it used to print its element buffer's bytes).
#[test]
fn e2e_assert_eq_fails_on_unequal_vec() {
    let run = run_program_capturing(
        r#"fn main() {
    let mut x: Vec[i64] = Vec.new(); x.push(1); x.push(2);
    let mut y: Vec[i64] = Vec.new(); y.push(1); y.push(9);
    println("before")
    assert_eq(x, y);
    println("WRONG: unequal vecs compared equal")
}
"#,
    );
    let Some(run) = run else { return };
    assert_eq!(run.stdout, "before\n", "stderr: {}", run.stderr);
    assert!(
        !run.status.success(),
        "the failed assert must exit non-zero"
    );
    assert!(
        run.stderr.contains(r#""left":null,"right":null"#),
        "a Vec operand is not formatted as a string; stderr: {}",
        run.stderr
    );
}
