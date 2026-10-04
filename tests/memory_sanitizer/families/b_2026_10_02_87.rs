//! B-2026-10-02-87 -- a plain-data struct array through a generic
//! `Option[T]` match: nothing on the heap, nothing freed.

use super::*;

/// `Array[S, N]` of plain structs returned through generic `match` arms on
/// both paths: no invalid read or free.
#[test]
fn asan_generic_option_match_at_plain_struct_array() {
    assert_clean_asan_run(
        r#"struct S { id: i64 }
struct T2 { a: S, b: f64, c: bool }
fn p1[T](x: Option[T], d: T) -> T { let k: T = match x { Some(v) => v, None => d }; return k; }
fn p3[T](x: Option[T], d: T) -> T { match x { Some(v) => v, None => d } }
fn pick[T](a: Option[T], d: T) -> T { return match a { Some(x) => x, None => d } }
fn main() {
    let a: Option[Array[S, 2]] = None;
    let r = p1(a, [S { id: 1 }, S { id: 2 }]);
    println(f"got{r[1].id}");
    let b: Option[Array[S, 2]] = Some([S { id: 3 }, S { id: 4 }]);
    let r2 = p3(b, [S { id: 5 }, S { id: 6 }]);
    println(f"got{r2[0].id}");
    let c: Option[Array[T2, 1]] = None;
    let r3 = pick(c, [T2 { a: S { id: 7 }, b: 1.5, c: true }]);
    println(f"got{r3[0].a.id} {r3[0].b} {r3[0].c}");
}
"#,
        &["got2", "got3", "got7 1.5 true"],
        "generic_option_match_plain_struct_array",
    );
}
