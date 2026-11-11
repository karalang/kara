//! Kata 1119's retain arm copies the text and drops its vowels in place
//! with `out.retain(|c| ..)`, as Rust's `String::retain` does. `Vec`,
//! `VecDeque`, `Map` and `Set` had `retain`; `String` did not, and the
//! call was refused as "no method 'retain' on type 'String'".

#[test]
fn kata_1119_string_retain_keeps_the_accepted_chars_in_order() {
    let src = "fn main() {
    let mut s = \"leetcodeisacommunityforcoders\".to_string();
    s.retain(|c| not \"aeiou\".contains(c.to_string()));
    println(s);
    let mut t = \"a é b ü c\".to_string();
    let mut seen = 0;
    t.retain(|c| {
        seen += 1;
        c != ' '
    });
    println(f\"{t} {t.len()} {seen}\");
    let mut empty = String.new();
    empty.retain(|_| true);
    println(f\"[{empty}]\");
}
";
    let out = karac::mir::lower::run_source(src)
        .unwrap_or_else(|e| panic!("{e}"))
        .output;
    assert_eq!(out, "ltcdscmmntyfrcdrs\naébüc 7 9\n[]\n");
}
