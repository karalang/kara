//! B-2026-10-03-32 / B-2026-09-29-98 -- `<int>.parse` and
//! `<int>.from_str_radix` parse AT the receiver's own width, and exist for
//! `i128` / `u128`.

use super::*;

/// Before, every width parsed as an `i64`: `u8.parse("300")` was `Some(300)`
/// (a `u8` holding 300; `Some(44)` compiled), `u8.parse("-1")` `Some(-1)`,
/// `i32.parse("3000000000")` `Some(3000000000)`, a `u64` above `i64::MAX`
/// was `None`, and `i128.parse` / `u128.parse` did not type-check. The
/// codegen fixture of the same name runs this program compiled.
#[test]
fn test_int_parse_at_the_receivers_width() {
    let src = r#"fn s8(o: Option[u8]) -> String { match o { Some(v) => f"S{v}", None => "N" } }
fn main() {
    println(f"{s8(u8.parse("300".to_string()))} {s8(u8.parse("-1".to_string()))} {s8(u8.parse(" 255 ".to_string()))} {s8(u8.from_str_radix("1ff".to_string(), 16))} {s8(u8.from_str_radix("ff".to_string(), 16))}");
    let a: Option[u64] = u64.parse("18446744073709551615".to_string());
    match a { Some(v) => println(f"S{v}"), None => println("N") }
    let b: Option[i32] = i32.parse("3000000000".to_string());
    match b { Some(v) => println(f"S{v}"), None => println("N") }
    let c: Option[i32] = i32.parse("-2147483648".to_string());
    match c { Some(v) => println(f"S{v}"), None => println("N") }
    let d = i128.parse("-170141183460469231731687303715884105728".to_string());
    match d { Some(v) => println(f"S{v}"), None => println("N") }
    let e = u128.parse("340282366920938463463374607431768211455".to_string());
    match e { Some(v) => println(f"S{v}"), None => println("N") }
    let f = i128.from_str_radix("-ff".to_string(), 16);
    match f { Some(v) => println(f"S{v}"), None => println("N") }
    let g = u128.parse("340282366920938463463374607431768211456".to_string());
    match g { Some(v) => println(f"S{v}"), None => println("N") }
    let h = i64.parse("abc".to_string());
    match h { Some(v) => println(f"S{v}"), None => println("N") }
    let k = i8.from_str_radix("-80".to_string(), 16);
    match k { Some(v) => println(f"S{v}"), None => println("N") }
    match u16.parse("65536".to_string()) { Some(v) => println(f"S{v}"), None => println("N") }
}
"#;
    let parsed = karac::parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    let terrs: Vec<String> = typed.errors.iter().map(|e| e.to_string()).collect();
    assert!(terrs.is_empty(), "type errors: {terrs:?}");
    assert_eq!(
        run(src),
        "N N S255 N S255\nS18446744073709551615\nN\nS-2147483648\nS-170141183460469231731687303715884105728\nS340282366920938463463374607431768211455\nS-255\nN\nN\nS-128\nN\n"
    );
}
