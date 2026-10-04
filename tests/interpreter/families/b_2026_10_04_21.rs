//! B-2026-10-04-21: a `char` takes the ASCII predicates `is_ascii_digit`, `is_ascii_alphabetic` and `is_ascii_hexdigit`

use super::*;

/// B-2026-10-04-21: `is_ascii_digit`, `is_ascii_alphabetic` and
/// `is_ascii_hexdigit` were typed on integer receivers only, so scanning a
/// `Vec[char]` with `c.is_ascii_digit()` failed to typecheck with `no method
/// 'is_ascii_digit' on type 'char'`. Non-ASCII letters and digits (`٣`, `é`)
/// answer false, as Rust's `char::is_ascii_*` do.
#[test]
fn interp_char_takes_the_ascii_predicates() {
    let out = run(r#"fn main() {
    let cs: Vec[char] = "a7F_g٣é Z0".chars().collect();
    let mut line = String.new();
    for c in cs.iter() {
        let d = if c.is_ascii_digit() { "d" } else { "-" };
        let a = if c.is_ascii_alphabetic() { "a" } else { "-" };
        let h = if c.is_ascii_hexdigit() { "h" } else { "-" };
        line.push_str(f"{d}{a}{h} ");
    }
    println(line);
    let mut pos = 0;
    let text: Vec[char] = "123,45".chars().collect();
    let mut v = 0;
    while text[pos].is_ascii_digit() {
        v = v * 10 + (text[pos] as i64 - 48);
        pos += 1;
    }
    println(f"{v} {pos} {'9'.is_ascii_digit()} {'x'.is_ascii_hexdigit()}");
}"#);
    assert_eq!(
        out,
        "-ah d-h -ah --- -a- --- --- --- -a- d-h \n123 3 true false\n"
    );
}
