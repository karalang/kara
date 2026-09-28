//! B-2026-09-19-52 -- a struct-shaped variant's boxed `Array` payload handed
//! to a by-value callee has one owner, on the generic and the mono spelling.

use super::*;

/// B-2026-09-19-52 — fixed by 2fbff99df (B-2026-09-20-55); on its parent this
/// program aborted with `free(): double free detected in tcache 2`, 12
/// valgrind errors from 8 contexts at `-O0`.
#[test]
fn e2e_struct_variant_boxed_array_payload_handed_on_runs() {
    let Some(out) = run_program(
        r#"enum G[T] { S { a: T }, N }
enum M { S { a: Array[String, 2] }, N }
fn eat(a: Array[String, 2]) -> i64 { return a[0].len(); }
fn hand(g: G[Array[String, 2]]) -> i64 {
    match g { G.S { a } => { return eat(a); } G.N => { return 0; } }
}
fn handm(g: M) -> i64 {
    match g { M.S { a } => { return eat(a); } M.N => { return 0; } }
}
fn main() {
    let mut i = 0;
    while i < 2 {
        let v: Array[String, 2] = [f"ab{i}", f"cd"];
        println(f"g:{hand(G.S { a: v })}");
        let w: Array[String, 2] = [f"efg{i}", f"hi"];
        println(f"m:{handm(M.S { a: w })}");
        i = i + 1;
    }
    { let v: Array[String, 2] = [f"jk", f"lm"]; let g: G[Array[String, 2]] = G.S { a: v }; println(f"n:{hand(g)}"); }
    { let v: Array[String, 2] = [f"nop", f"q"]; let g: M = M.S { a: v }; println(f"o:{handm(g)}"); }
    println("end");
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "g:3\nm:4\ng:3\nm:4\nn:2\no:3\nend\n", "got:\n{out}");
}
