//! B-2026-09-26-28 -- `assert_eq` / `assert_ne` free a fresh `String` or
//! `Vec` operand once the comparison passes.

use super::*;

/// B-2026-09-26-28 — before, each fresh operand below leaked its buffer
/// (valgrind at `-O0`: one block per operand): a branch or block wrapper whose
/// tails mint, and a plain call result, on either side. The binding operand
/// `s` and the mixed branch `if .. { s2 } else { mk(6) }` are owned elsewhere
/// and must NOT be freed here, so they guard the other direction (`s` is read
/// again afterwards).
#[test]
fn asan_assert_eq_frees_fresh_operands() {
    assert_clean_asan_run(
        r#"fn mk(n: i64) -> String { f"w{n}" }
fn ids() -> Vec[i64] { let mut v: Vec[i64] = Vec.new(); v.push(1); v }
fn main() {
    let c = mk(1).len() > 0;
    assert_eq(if c { f"w{5}" } else { f"z" }, f"w5");
    assert_ne(if c { f"w{5}" } else { f"z" }, f"q");
    assert_eq(f"w5", match 1 { 0 => f"a", _ => f"w{5}" });
    assert_eq({ f"w{5}" }, f"w5");
    assert_eq(mk(5), f"w5");
    assert_ne(f"q", mk(5));
    let mut i = 0;
    while i < 3 { assert_eq(if i >= 0 { mk(5) } else { f"z" }, mk(5)); i = i + 1; }
    assert_eq(ids().len(), 1);
    let s = f"w{5}";
    assert_eq(s.clone(), f"w5");
    let s2 = f"w{5}";
    assert_eq(if c { s2 } else { mk(6) }, s.clone());
    println(s);
    println("end")
}
"#,
        &["w5", "end"],
        "B-2026-09-26-28 assert_eq frees fresh String operands",
    );
}
