//! B-2026-09-17-23 -- a GENERIC fn's by-value `Option`/`Result` param whose
//! payload is a tuple carrying a heap-bearing `Drop` type (`Option[(T, i64)]`
//! at `T = W`) runs that element's `Drop` body once, and frees it once.

use super::*;

/// B-2026-09-17-23 — the memory half: before the fix each call leaked the
/// payload's `String` (2-3 B a cell, 18 B over this program at `-O0`) because
/// the interior's only registered owner was a callee arm that frees nothing.
/// Same program as the codegen fixture.
#[test]
fn asan_generic_tuple_option_param_frees_heap_payload_once() {
    assert_clean_asan_run(
        r#"struct W { id: i64, name: String }
impl Drop for W { fn drop(mut ref self) { println(f"dW{self.id}/{self.name}") } }
fn mk(i: i64) -> W { return W { id: i, name: f"n{i}" }; }
fn eat[T](t: (T, i64)) -> i64 { return t.1 }
fn gh[T](o: Option[(T, i64)]) -> i64 { match o { Some(t) => { return t.1; } None => { return 0; } } }
fn gr[T](o: Option[(T, i64)]) -> i64 { match o { Some(t) => { println("  r"); t.1 } None => { 0 } } }
fn gn[T](o: Option[(T, i64)]) -> i64 { match o { Some(_) => { return 1; } None => { return 0; } } }
fn gx[T](o: Option[(T, i64)]) -> i64 { println("  x"); return 5 }
fn ge[T](o: Option[(T, i64)]) -> i64 { match o { Some(t) => { return eat(t); } None => { return 0; } } }
fn gi[T](o: Option[(T, i64)]) -> i64 { if let Some(t) = o { return t.1; } return 0 }
fn gg[T](o: Option[(T, i64)]) -> i64 { match o { Some(t) if t.1 > 5 => { return t.1; } Some(t) => { return 1; } None => { return 0; } } }
fn gs[T](o: Option[(T, i64)]) -> i64 { match o { Some(t) => { let x = t.0; return 3; } None => { return 0; } } }
fn gret[T](o: Option[(T, i64)]) -> Option[(T, i64)] { return o }
fn main() {
    println(f"gh{gh(Some((mk(1), 9)))}");
    println(f"gr{gr(Some((mk(2), 9)))}");
    println(f"gn{gn(Some((mk(4), 9)))}");
    println(f"gx{gx(Some((mk(5), 9)))}");
    println(f"ge{ge(Some((mk(6), 9)))}");
    println(f"gi{gi(Some((mk(8), 9)))}");
    println(f"gg{gg(Some((mk(11), 9)))}");
    println(f"gg{gg(Some((mk(12), 2)))}");
    println(f"gs{gs(Some((mk(13), 9)))}");
    let r = gret(Some((mk(14), 9))); println("  ret");
    let o: Option[(W, i64)] = Some((mk(15), 9)); println(f"named{gh(o)}");
    for i in 0..2 { println(f"lp{gh(Some((mk(20 + i), i)))}"); }
    let s: Option[(String, i64)] = Some((f"str-payload-kappa", 4)); println(f"str{gh(s)}");
    println(f"none{gh(None)}");
    println("end");
}
"#,
        &[
            "dW1/n1", "gh9", "  r", "dW2/n2", "gr9", "dW4/n4", "gn1", "  x", "dW5/n5", "gx5",
            "dW6/n6", "ge9", "dW8/n8", "gi9", "dW11/n11", "gg9", "dW12/n12", "gg1", "dW13/n13",
            "gs3", "dW14/n14", "  ret", "named9", "dW15/n15", "dW20/n20", "lp0", "dW21/n21", "lp1",
            "str4", "none0", "end",
        ],
        "generic_tuple_option_param_heap_payload",
    );
}
