//! B-2026-10-03-32 / B-2026-09-29-98 -- `<int>.parse` and
//! `<int>.from_str_radix` parse AT the receiver's own width, and exist for
//! `i128` / `u128`.

use super::*;

/// The interpreter fixture's program, compiled. Before, codegen parsed every
/// width through `karac_runtime_parse_i64` and stored the i64 in the payload,
/// so `u8.parse("300")` was `Some(44)`, `u8.parse("-1")` `Some(255)` and
/// `i32.parse("3000000000")` `Some(-1294967296)`.
#[test]
fn e2e_int_parse_at_the_receivers_width() {
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
    let Some(run) = run_program_capturing(src) else {
        return;
    };
    assert_eq!(
        run.stdout,
        "N N S255 N S255\nS18446744073709551615\nN\nS-2147483648\nS-170141183460469231731687303715884105728\nS340282366920938463463374607431768211455\nS-255\nN\nN\nS-128\nN\n",
        "stderr: {}",
        run.stderr
    );
    assert!(run.status.success(), "stderr: {}", run.stderr);
}
