//! B-2026-09-28-36 -- destructuring a tuple payload whose elements include a
//! sub-word integer, `bool` or `char` compiles and binds each element at its
//! real width.

use super::*;

/// B-2026-09-28-36 — the interpreter twin of
/// `e2e_narrow_tuple_payload_destructure_output`, same source and output.
#[test]
fn test_narrow_tuple_payload_destructure_output() {
    let out = run(r#"enum N { P((i32, String)), Q }
fn f(o: Option[(bool, String)]) -> i64 { match o { Some((b, s)) => { if b { return s.len(); } return 0; } None => { return 0; } } }
fn main() {
    let g = Some((3i32, 5));
    if let Some((a, b)) = g { println(f"a{a}{b}"); }
    let o: Option[(bool, i32, String)] = Some((true, 7i32, f"heap-string-longer-than-sso-o"));
    match o { Some((b, k, s)) => println(f"b{b}{k}{s}"), None => println("none") }
    let r: Result[(u8, char), i64] = Ok((200u8, 'x'));
    match r { Ok((u, c)) => println(f"c{u}{c}"), Err(e) => println(f"e{e}") }
    let n = N.P((-4i32, f"heap-string-longer-than-sso-n"));
    match n { N.P((k, s)) => println(f"d{k}{s}"), N.Q => println("q") }
    let w: Option[(bool, String, String)] = Some((false, f"heap-string-longer-than-sso-w1", f"w2"));
    if let Some((b, s, t)) = w { println(f"e{b}{s}{t}"); }
    println(f"f{f(Some((true, f"heap-string-longer-than-sso-f")))}");
    println("end")
}
"#);
    let got: Vec<&str> = out.lines().collect();
    assert_eq!(
        got,
        vec![
            "a35",
            "btrue7heap-string-longer-than-sso-o",
            "c200x",
            "d-4heap-string-longer-than-sso-n",
            "efalseheap-string-longer-than-sso-w1w2",
            "f29",
            "end",
        ],
        "got:\n{out}"
    );
}
