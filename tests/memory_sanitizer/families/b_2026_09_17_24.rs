//! B-2026-09-17-24 -- a generic fn that destructures a tuple `Option` payload
//! element-wise (`Some((a, b))` over `Option[(T, i64)]`) binds `a` at the
//! instantiated type: `return a` lowers, and `b` is read from the right word.

use super::*;

/// B-2026-09-17-24 — the memory half: `Some((a, b)) => return Some(a)` freed
/// `W` twice before the fix (`free(): double free`), and a returned `a` must
/// be freed once by its receiver. The `return b` cells are left out: they
/// still lose 2 B each, which is B-2026-09-17-27 on the concrete twin too.
#[test]
fn asan_generic_tuple_payload_destructure_frees_once() {
    assert_clean_asan_run(
        r#"struct W { id: i64, name: String }
impl Drop for W { fn drop(mut ref self) { println(f"dW{self.id}/{self.name}") } }
fn mk(i: i64) -> W { return W { id: i, name: f"n{i}" }; }
fn gd[T](o: Option[(T, i64)]) -> i64 { match o { Some((a, b)) => { return b; } None => { return 0; } } }
fn e3[T](o: Option[(T, i64)], d: T) -> T { match o { Some((a, b)) => { return a; } None => { return d; } } }
fn e1[T](o: Option[(T, i64)]) -> Option[T] { match o { Some((a, b)) => { return Some(a); } None => { return None; } } }
fn main() {
    let r = e1(Some((mk(2), 9))); match r { Some(w) => { println(f"e{w.id}") } None => { println("e-none") } }
    println(e3(Some((f"generic-string-mu", 9)), f"default-string-nu"));
    let n = e3(None, mk(103)); println(f"e{n.id}");
    println(f"i{gd(Some((7, 9)))}");
    println(f"t{gd(Some((f"str-payload-lambda", 9)))}");
    println(f"none{gd(None)}");
    println("end");
}
"#,
        &[
            "e2",
            "dW2/n2",
            "generic-string-mu",
            "e103",
            "dW103/n103",
            "i9",
            "t9",
            "none0",
            "end",
        ],
        "generic_tuple_payload_destructure",
    );
}
