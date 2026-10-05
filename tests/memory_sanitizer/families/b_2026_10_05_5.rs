//! B-2026-10-05-5 -- a one-word `Array` field inside an enum payload keeps its
//! element, and the `Drop`-bodied element runs its body once.

use super::*;

/// The packed word now holds the element rather than a constant 0; nothing is
/// leaked or freed twice, and `R`'s body runs once (`d19`).
#[test]
fn asan_one_word_array_field_in_enum_payload() {
    assert_clean_asan_run(
        r#"struct P { n: i64 }
struct S { a: Array[i64, 1], b: i64 }
struct I { a: Array[i64, 1] }
struct S2 { i: I, b: i64 }
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}"); } }
enum E { A((Array[i64, 1], i64)), B }
fn mk() -> Option[(Array[i64, 1], i64)] { Some((Array[6], 7)) }
fn main() {
    let d = Some((Array[5], 7));
    match d { Some(t) => println(f"a:{t.0[0]} {t.1}"), None => {} }
    let r: Result[(Array[i64, 1], i64), i64] = Ok((Array[8], 9));
    match r { Ok((x, y)) => println(f"b:{x[0]} {y}"), Err(e) => println(f"e{e}") }
    match mk() { Some(t) => { let a = t.0; println(f"c:{a[0]} {a.len()}") }, None => {} }
    let n = Some((1, (Array[P { n: 11 }], 2.5)));
    match n { Some(t) => println(f"d:{t.1.0[0].n} {t.1.1} {t.0}"), None => {} }
    match Some(S { a: Array[12], b: 13 }) { Some(s) => println(f"e:{s.a[0]} {s.b}"), None => {} }
    match Some(S2 { i: I { a: Array[14] }, b: 15 }) { Some(s) => { let i = s.i; println(f"f:{i.a[0]} {s.b}") }, None => {} }
    match E.A((Array[16], 17)) { E.A(t) => println(f"g:{t.0[0]} {t.1}"), E.B => {} }
    let mut v: Vec[Option[(Array[bool, 1], i64)]] = Vec.new();
    v.push(Some((Array[true], 18)));
    match v[0] { Some(t) => println(f"h:{t.0[0]} {t.1}"), None => {} }
    let w = Some((Array[R { id: 19 }], 20));
    match w { Some(t) => println(f"i:{t.0[0].id} {t.1}"), None => {} }
    println(f"j:{Some((Array[21], 22)).unwrap().0[0]}");
}
"#,
        &[
            "a:5 7",
            "b:8 9",
            "c:6 1",
            "d:11 2.5 1",
            "e:12 13",
            "f:14 15",
            "g:16 17",
            "h:true 18",
            "i:19 20",
            "d19",
            "j:21",
        ],
        "one_word_array_field_in_enum_payload",
    );
}
