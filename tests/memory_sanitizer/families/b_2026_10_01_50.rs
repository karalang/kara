//! B-2026-10-01-50: a user type named like a seeded generic param (`T`) captured `Option`/`Result`'s payload.

use super::*;

/// B-2026-10-01-50 — the memory half of the codegen fixture of the same name.
/// With a user `struct T { w: W1 }` declared, a `Some(w)` / `Ok(w)` arm that
/// discards `w` only inside an `if` leaked the payload's `String` at
/// `c = false` (valgrind: 31 B definitely lost per call). The gate that decides
/// whether an arm consumes the payload resolved the seeded `Option[+T]` /
/// `Result[+T, +E]` param name `T` against the user type.
#[test]
fn asan_user_type_named_like_seeded_param_does_not_leak_payload() {
    assert_clean_asan_run(
        r#"struct W1 { v: i64, s: String }
impl Drop for W1 { fn drop(mut ref self) { println(f"dW1_{self.v}") } }
fn mk(n: i64) -> W1 { return W1 { v: n, s: f"ssssssssssssssssssssssssssssss{n}" } }
enum E { A(W1), B }
struct T { w: W1 }
fn a8(c: bool, n: i64) { let o = Some(mk(n)); match o { Some(w) => { if c { let _ = E.A(w); println("a") }; println("b") }, None => {} }; println("e") }
fn o8(c: bool, n: i64) { let o = Some(mk(n)); match o { Some(w) => { if c { let _ = Some(w); println("a") }; println("b") }, None => {} }; println("e") }
fn r8(c: bool, n: i64) { let o: Result[W1, i64] = Ok(mk(n)); match o { Ok(w) => { if c { let _ = Some(w); println("a") }; println("b") }, Err(v) => println(f"{v}") }; println("e") }
fn i8(c: bool, n: i64) { let o = Some(mk(n)); if let Some(w) = o { if c { let _ = E.A(w); println("a") }; println("b") }; println("e") }
fn main() {
  a8(false, 1); a8(true, 2); o8(false, 3); o8(true, 4); r8(false, 5); r8(true, 6); i8(false, 7); i8(true, 8);
  println("end")
}
"#,
        &[
            "b", "dW1_1", "e", "dW1_2", "a", "b", "e", "b", "dW1_3", "e", "dW1_4", "a", "b", "e",
            "b", "dW1_5", "e", "dW1_6", "a", "b", "e", "b", "dW1_7", "e", "dW1_8", "a", "b", "e",
            "end",
        ],
        "B-2026-10-01-50 seeded-param name capture",
    );
}
