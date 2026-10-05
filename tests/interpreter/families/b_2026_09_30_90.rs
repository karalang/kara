//! B-2026-09-30-90: an array literal inside an `Option` / `Result` payload tuple takes its `Array` slot

use super::*;

const PAYLOADS: &str = r#"struct P { n: i64 }
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
"#;

/// B-2026-09-30-90: a bare `[..]` synthesises `Vec`, and only check mode
/// coerces it to `Array[T, N]`. The annotated `let` pushed its slot into a
/// nested tuple (B-2026-10-04-38) but not into a `Some` / `Ok` / `Err`
/// payload, so `Some(([4, 5], 7))` against `Option[(Array[i64, 2], i64)]`
/// failed `found '(Vec[i64], i64)'`. The payload now takes its slot when only
/// check mode can type it, and a `Vec` slot keeps synthesis mode.
#[test]
fn interp_array_literal_in_a_payload_tuple_takes_its_array_slot() {
    let parsed = karac::parse(PAYLOADS);
    let resolved = karac::resolve(&parsed.program);
    let errors = karac::typecheck(&parsed.program, &resolved).errors;
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(run(PAYLOADS), "a:5 7\nb:4\nc:5\nd:5\ne:3\nf:9\n");
}

/// B-2026-09-30-90: the slot is checked, not trusted: a literal of the wrong
/// length is still an error.
#[test]
fn interp_array_literal_in_a_payload_tuple_still_checks_its_length() {
    let src = "fn main() { let a: Option[(Array[i64, 3], i64)] = Some(([4, 5], 7)); }\n";
    let parsed = karac::parse(src);
    let resolved = karac::resolve(&parsed.program);
    let errors = karac::typecheck(&parsed.program, &resolved).errors;
    assert!(
        errors.iter().any(|e| e
            .message
            .contains("array literal has 2 element(s), expected 3")),
        "{errors:?}"
    );
}
