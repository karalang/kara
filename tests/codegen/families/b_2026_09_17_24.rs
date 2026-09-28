//! B-2026-09-17-24 -- a generic fn that destructures a tuple `Option` payload
//! element-wise (`Some((a, b))` over `Option[(T, i64)]`) binds `a` at the
//! instantiated type: `return a` lowers, and `b` is read from the right word.

use super::*;

/// B-2026-09-17-24 — the typechecker recorded no type for a leaf typed by a
/// bare generic parameter, so the monomorph sized `a` at one word. `return a`
/// failed module verification (`ret i64` against `W`'s struct), and a
/// heap-bearing `T` boxed the payload without the debox firing, so `b` read
/// `0` (`if let`, a tail arm) or a pointer (a three-element tuple) where
/// `--interp` reads `9`. `Some((a, b)) => return Some(a)` then ran `W`'s body
/// twice: `Some(a)` read as an entry-copying free-fn call.
#[test]
fn e2e_generic_tuple_payload_destructure_binds_the_instantiated_type() {
    let Some(out) = run_program(
        r#"struct W { id: i64, name: String }
impl Drop for W { fn drop(mut ref self) { println(f"dW{self.id}/{self.name}") } }
fn mk(i: i64) -> W { return W { id: i, name: f"n{i}" }; }
fn gd[T](o: Option[(T, i64)]) -> i64 { match o { Some((a, b)) => { return b; } None => { return 0; } } }
fn e3[T](o: Option[(T, i64)], d: T) -> T { match o { Some((a, b)) => { return a; } None => { return d; } } }
fn e1[T](o: Option[(T, i64)]) -> Option[T] { match o { Some((a, b)) => { return Some(a); } None => { return None; } } }
fn gl[T](o: Option[(T, i64)]) -> i64 { if let Some((a, b)) = o { return b; } return 0 }
fn g3[T](o: Option[(i64, T, i64)]) -> i64 { match o { Some((x, a, b)) => { return x + b; } None => { return 0; } } }
fn gs[T](o: Option[(T, i64)]) -> i64 { match o { Some((a, b)) => { println(f"  s{b}"); b } None => { 0 } } }
fn main() {
    println(f"d{gd(Some((mk(1), 9)))}");
    let r = e1(Some((mk(2), 9))); match r { Some(w) => { println(f"e{w.id}") } None => { println("e-none") } }
    println(e3(Some((f"generic-string-mu", 9)), f"default-string-nu"));
    let n = e3(None, mk(103)); println(f"e{n.id}");
    println(f"l{gl(Some((mk(3), 9)))}");
    println(f"3{g3(Some((1, mk(5), 9)))}");
    println(f"s{gs(Some((mk(6), 9)))}");
    println(f"i{gd(Some((7, 9)))}");
    println(f"t{gd(Some((f"str-payload-lambda", 9)))}");
    println(f"none{gd(None)}");
    println("end");
}
"#,
    ) else {
        return;
    };
    let got: Vec<&str> = out.lines().collect();
    assert_eq!(
        got,
        [
            "dW1/n1",
            "d9",
            "e2",
            "dW2/n2",
            "generic-string-mu",
            "e103",
            "dW103/n103",
            "dW3/n3",
            "l9",
            "dW5/n5",
            "310",
            "  s9",
            "dW6/n6",
            "s9",
            "i9",
            "t9",
            "none0",
            "end"
        ],
        "got:\n{out}"
    );
}
