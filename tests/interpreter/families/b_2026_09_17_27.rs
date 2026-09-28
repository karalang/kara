//! B-2026-09-17-27 -- a destructuring arm over a boxed `Option`/`Result`
//! tuple payload whose only moved leaves are primitive scalars keeps the box's
//! interior free.

use super::*;

/// B-2026-09-17-27 — the interpreter twin of
/// `asan_tuple_payload_destructure_primitive_leaf_move_keeps_interior_free`.
#[test]
fn test_tuple_payload_destructure_primitive_leaf_move_output() {
    let out = run(r#"struct W { id: i64, name: String }
impl Drop for W { fn drop(mut ref self) { println(f"dW{self.id}/{self.name}") } }
fn mk(i: i64) -> W { return W { id: i, name: f"n{i}" }; }
fn t1(o: Option[(W, i64)]) -> i64 { match o { Some((a, b)) => { return b; } None => { return 0; } } }
fn t2(o: Option[(W, i64)]) -> i64 { match o { Some((a, b)) => { println(f"  {a.id}"); return b; } None => { return 0; } } }
fn t3(o: Option[(i64, W)]) -> i64 { match o { Some((b, a)) => b, None => 0 } }
fn t4(o: Option[(W, i64, i64)]) -> i64 { match o { Some((a, b, c)) => { let z = b; return z + c; } None => { return 0; } } }
fn t5(o: Option[(W, i64)]) -> i64 { if let Some((a, b)) = o { return b; } return 0; }
fn t7(o: Result[(W, i64), i64]) -> i64 { match o { Ok((a, b)) => { return b; } Err(e) => { return e; } } }
fn t8(o: Option[(W, i64)]) -> i64 { match o { Some((_, b)) => { return b; } None => { return 0; } } }
fn t9(o: Option[(W, i64)]) -> i64 { match o { Some((a, b)) if b > 5 => { return b; } _ => { return 0; } } }
fn main() {
    println(f"a{t1(Some((mk(1), 9)))}");
    println(f"b{t2(Some((mk(2), 9)))}");
    println(f"c{t3(Some((9, mk(3))))}");
    println(f"d{t4(Some((mk(4), 9, 1)))}");
    println(f"e{t5(Some((mk(5), 9)))}");
    println(f"g{t7(Ok((mk(7), 9)))}");
    println(f"g{t7(Err(4))}");
    println(f"h{t8(Some((mk(8), 9)))}");
    println(f"i{t9(Some((mk(9), 9)))}");
    println(f"i{t9(Some((mk(10), 2)))}");
    let o: Option[(W, i64)] = Some((mk(11), 9));
    match o { Some((a, b)) => { println(f"j{b}"); } None => {} }
    println("end")
}
"#);
    let got: Vec<&str> = out.lines().collect();
    assert_eq!(
        got,
        vec![
            "dW1/n1", "a9", "  2", "dW2/n2", "b9", "dW3/n3", "c9", "dW4/n4", "d10", "dW5/n5", "e9",
            "dW7/n7", "g9", "g4", "dW8/n8", "h9", "dW9/n9", "i9", "dW10/n10", "i0", "j9",
            "dW11/n11", "end",
        ],
        "got:\n{out}"
    );
}
