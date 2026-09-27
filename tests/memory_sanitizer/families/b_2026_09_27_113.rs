//! B-2026-09-27-113 — a fresh-temp struct argument the callee stores into a
//! caller-held container is freed once.

use super::*;

/// B-2026-09-27-113 — the ASAN twin of
/// `e2e_freshtemp_struct_stored_in_caller_container_runs_bodies_once`: the
/// fresh temp's `String` is freed by the caller, once, on every store spelling.
#[test]
fn asan_freshtemp_struct_stored_in_caller_container_is_freed() {
    assert_clean_asan_run_min_allocs(
        r#"
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct S { r: R, s: String }
fn mk(i: i64) -> S { S { r: R { id: i }, s: f"heap-string-longer-than-sso-{i}" } }
struct B { v: Vec[S] }
impl B { fn put(mut ref self, a: S) { self.v.push(a) } }
fn rb(b: mut ref B, a: S) -> i64 { b.v.push(a); 5 }
fn rw[T](b: mut ref Vec[T], a: T) -> i64 { b.push(a); 6 }
fn main() {
    let mut b = B { v: Vec.new() };
    println(f"k{rb(mut b, mk(1))}");
    let x = mk(2);
    println(f"k{rb(mut b, x)}");
    b.put(mk(3));
    let y = mk(4);
    b.put(y);
    let mut w: Vec[S] = Vec.new();
    println(f"k{rw(mut w, mk(5))}");
    println(f"n{b.v.len()} {w.len()}");
    println("end")
}
"#,
        &[
            "k5", "k5", "k6", "n4 1", "d5", "d1", "d2", "d3", "d4", "end",
        ],
        "asan_freshtemp_struct_stored_in_caller_container_is_freed",
        10,
    );
}
