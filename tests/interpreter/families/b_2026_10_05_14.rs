//! B-2026-10-05-14: `--interp` appends to a named `String` in place

use super::*;

/// B-2026-10-05-14: every receiver shape `push` / `push_str` can reach,
/// read back after the appends. A local, a `mut ref` parameter, a closure
/// capture, a binding shadowed in an inner block, an `iter_mut` element (a
/// slot reference, which takes the read-modify-write path) and a struct
/// field (not a bare name, which never took the in-place path). The argument
/// runs exactly once per call.
const SHAPES: &str = r#"struct W { s: String }
fn add(out: mut ref String, c: char) { out.push(c); out.push_str("-"); }
fn tick(n: mut ref i64) -> char { n += 1; return 'k'; }
fn main() {
    let mut a = String.new();
    for i in 0..5 { a.push('a'); }
    a.push_str("bc");
    println(a);
    let mut b = String.new();
    add(mut b, 'x');
    add(mut b, 'y');
    println(b);
    let mut c = String.from("c");
    let mut app = |t: String| { c.push_str(t); };
    app("1");
    app("2");
    println(c);
    let mut d = String.from("outer");
    {
        let mut d = String.from("inner");
        d.push('!');
        println(d);
    }
    d.push('?');
    println(d);
    let mut v: Vec[String] = [String.from("p"), String.from("q")];
    for s in v.iter_mut() { s.push('+'); }
    println(f"{v[0]} {v[1]}");
    let mut w = W { s: String.from("w") };
    w.s.push('.');
    println(w.s);
    let mut n = 0;
    let mut e = String.new();
    e.push(tick(mut n));
    e.push(tick(mut n));
    println(f"{e} {n}");
}
"#;

#[test]
fn interp_string_push_appends_in_place_on_every_receiver_shape() {
    assert_eq!(
        run_no_errors(SHAPES),
        "aaaaabc\nx-y-\nc12\ninner!\nouter?\np+ q+\nw.\nkk 2\n"
    );
}

/// B-2026-10-05-14: a long string built one `push` at a time, read back
/// whole. Before the fix each `push` copied the string, so this was
/// quadratic; the assertion is on the result only, so the test cannot flake
/// on a slow machine.
#[test]
fn interp_string_built_one_char_at_a_time_is_complete() {
    let out = run_no_errors(
        r#"fn main() {
    let mut s = String.new();
    for i in 0..20000 { s.push(if i % 2 == 0 { 'a' } else { 'b' }); }
    let mut t = String.new();
    for i in 0..2000 { t.push_str("xyz"); }
    println(f"{s.len()} {t.len()} {s.char_at(19999)} {t.char_at(5999)}");
}
"#,
    );
    assert_eq!(out, "20000 6000 Some(b) Some(z)\n");
}
