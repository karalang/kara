//! B-2026-10-01-23 -- a boxed `Option`/`Result` payload binding re-wrapped in a
//! discarded USER-enum constructor (`let _ = E.A(w);`) is freed once.

use super::*;

/// B-2026-10-01-23 — the memory half: one free of each payload's heap. Same
/// program as the codegen twin.
#[test]
fn asan_boxed_payload_rewrapped_in_discarded_user_enum_freed_once() {
    assert_clean_asan_run(
        r#"struct W1 { v: i64, s: String }
impl Drop for W1 { fn drop(mut ref self) { println(f"dW1_{self.v}") } }
fn mk(n: i64) -> W1 { return W1 { v: n, s: f"ssssssssssssssssssssssssssssss{n}" } }
enum E { A(W1), B }
enum D { A(W1), B }
impl Drop for D { fn drop(mut ref self) { println("dD") } }
fn a1() { let o = Some(mk(1)); match o { Some(w) => { let _ = E.A(w); println("a1") }, None => {} }; println("a1e") }
fn a2() { let o = Some(mk(2)); match o { Some(w) => { E.A(w); println("a2") }, None => {} }; println("a2e") }
fn a3() { let o = Some(mk(3)); match o { Some(w) => { let _ = D.A(w); println("a3") }, None => {} }; println("a3e") }
fn a5() { let o: Result[W1, i64] = Ok(mk(5)); match o { Ok(w) => { let _ = E.A(w); println("a5") }, Err(_) => {} }; println("a5e") }
fn a7() { let o = Some(mk(7)); if let Some(w) = o { let _ = E.A(w); println("a7") }; println("a7e") }
fn a8(c: bool) { let o = Some(mk(8)); match o { Some(w) => { if c { let _ = E.A(w); println("a8") }; println("a8b") }, None => {} }; println("a8e") }
fn main() { a1(); a2(); a3(); a5(); a7(); a8(true); println("end") }
"#,
        &[
            "dW1_1", "a1", "a1e", "dW1_2", "a2", "a2e", "dD", "dW1_3", "a3", "a3e", "dW1_5", "a5",
            "a5e", "dW1_7", "a7", "a7e", "dW1_8", "a8", "a8b", "a8e", "end",
        ],
        "boxed_payload_rewrapped_in_discarded_user_enum",
    );
}
