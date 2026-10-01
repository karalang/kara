//! B-2026-10-01-10 -- a binding discarded below the frame that owns it is
//! freed once, its body fired at the statement.

use super::*;

/// B-2026-10-01-10 — the memory half: the body now fires at the `let _` and
/// the frame's drain skips it on that path (or the action is retired, for an
/// `if let` binding), so each value is still destroyed exactly once and its
/// memory freed once. Same program as the codegen twin.
#[test]
fn asan_discard_in_nested_block_freed_once() {
    assert_clean_asan_run(
        r#"struct W1 { v: i64, s: String }
impl Drop for W1 { fn drop(mut ref self) { println(f"dW1_{self.v}") } }
struct H { w: W1, n: i64 }
enum E { A(W1), B(i64) }
fn mk(n: i64) -> W1 { return W1 { v: n, s: f"ssssssssssssssssssssssssssssss{n}" } }
fn in_if(c: bool) { let a = mk(1); if c { let _ = a; println("x") }; println("y") }
fn in_block() { let a = mk(2); { let _ = a; println("bx") }; println("by") }
fn in_loop() { for i in 0..2 { let a = mk(10 + i); if i == 1 { let _ = a; println("li") }; println("le") } }
fn holder(c: bool) { let h = H { w: mk(3), n: 1 }; if c { let _ = h; println("hs") }; println("hs2") }
fn enum_local(c: bool) { let e = E.A(mk(4)); if c { let _ = e; println("en") }; println("en2") }
fn vec_local(c: bool) { let v = [mk(5), mk(6)]; if c { let _ = v; println("ve") }; println("ve2") }
fn early(c: bool) -> i64 { let a = mk(7); if c { let _ = a; return 1 }; println("r2"); return 0 }
fn main() {
    in_if(true);
    in_if(false);
    in_block();
    in_loop();
    holder(true);
    holder(false);
    enum_local(true);
    enum_local(false);
    vec_local(true);
    vec_local(false);
    println(f"{early(true)}");
    println(f"{early(false)}");
    let o: Result[W1, i64] = Ok(mk(8));
    match o { Ok(w) => { let _ = w; println("arm") }, Err(_) => {} };
    let o2: Result[W1, i64] = Ok(mk(9));
    match o2 { Ok(w) => { if true { let _ = w; println("in") }; println("arm2") }, Err(_) => {} };
    let o3: Result[W1, i64] = Ok(mk(12));
    if let Ok(w) = o3 { let _ = w; println("il") };
    println("end")
}
"#,
        &[
            "dW1_1", "x", "y", "dW1_1", "y", "dW1_2", "bx", "by", "dW1_10", "le", "dW1_11", "li",
            "le", "dW1_3", "hs", "hs2", "dW1_3", "hs2", "dW1_4", "en", "en2", "dW1_4", "en2",
            "dW1_5", "dW1_6", "ve", "ve2", "dW1_5", "dW1_6", "ve2", "dW1_7", "1", "dW1_7", "r2",
            "0", "dW1_8", "arm", "dW1_9", "in", "arm2", "dW1_12", "il", "end",
        ],
        "discard_in_nested_block",
    );
}
