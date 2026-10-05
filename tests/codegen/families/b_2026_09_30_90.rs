//! B-2026-09-30-90: an array literal inside an `Option` / `Result` payload tuple takes its `Array` slot

use super::*;

/// B-2026-09-30-90: the compiled half of the interpreter fixture of the same
/// name. `Some(([4, 5], 7))` against `Option[(Array[i64, 2], i64)]` used to be
/// rejected by the typechecker on every surface; it now builds and reads the
/// same values the interpreter does.
#[test]
fn e2e_array_literal_in_a_payload_tuple_takes_its_array_slot() {
    let Some(out) = run_program(
        r#"struct P { n: i64 }
fn f() -> Option[(Array[i64, 2], i64)] { return Some(([4, 5], 7)) }
fn main() {
    let a: Option[(Array[i64, 2], i64)] = Some(([4, 5], 7));
    match a { Some(t) => println(f"a:{t.0[1]} {t.1}"), None => {} }
    let b: Result[(Array[i64, 2], i64), i64] = Ok(([4, 5], 7));
    match b { Ok(t) => println(f"b:{t.0[0]}"), Err(_) => {} }
    let c: Result[i64, (Array[i64, 2], i64)] = Err(([4, 5], 7));
    match c { Ok(_) => {}, Err(t) => println(f"c:{t.0[1]}") }
    let d: Option[(i64, (Array[P, 2], i64))] = Some((1, ([P { n: 5 }, P { n: 6 }], 7)));
    match d { Some(t) => println(f"d:{t.1.0[0].n}"), None => {} }
    let e: Option[(Vec[i64], i64)] = Some(([4, 5, 6], 7));
    match e { Some(t) => println(f"e:{t.0.len()}"), None => {} }
    match f() { Some(t) => println(f"f:{t.0[0] + t.0[1]}"), None => {} }
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "a:5 7\nb:4\nc:5\nd:5\ne:3\nf:9\n");
}
