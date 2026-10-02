//! B-2026-10-02-65 -- a fresh `Tensor` temporary passed to a GENERIC callee
//! is freed by the caller.

use super::*;

/// B-2026-10-02-65 — `compile_generic_call` had no twin of `compile_call`'s
/// `track_tensor_var` arm, so a fresh tensor temp handed to a monomorph
/// (`eat(make())` at `T = Tensor`, a concrete tensor param on a generic fn,
/// and a `ref Tensor` one) leaked its whole block, 56 B per call. A callee
/// that hands the param back (`id`) or stores it (`keepv`) leaves the block
/// to the result, so the caller must not free it there.
#[test]
fn asan_tensor_temp_to_generic_callee_freed_once() {
    assert_clean_asan_run(
        r#"fn make() -> Tensor[f64, [2, 2]] {
    let t: Tensor[f64, [2, 2]] = Tensor.full([2, 2], 9.0);
    t
}
fn eat[T](t: T) -> i64 { 1 }
fn first[T](t: Tensor[f64, [2, 2]], u: T) -> f64 { t[0, 0] }
fn peek[U](t: ref Tensor[f64, [2, 2]], u: U) -> f64 { t[0, 0] }
fn id[T](t: T) -> T { t }
fn keepv[T](t: T) -> Vec[T] { let mut v: Vec[T] = Vec.new(); v.push(t); v }
fn main() {
    println(eat(make()));
    println(eat(make()));
    println(first(make(), 1));
    println(peek(make(), 1));
    let a = id(make());
    println(a[0, 0]);
    let v = keepv(make());
    println(v.len());
    println("end");
}
"#,
        &["1", "1", "9", "9", "9", "1", "end"],
        "tensor_temp_to_generic_callee",
    );
}
