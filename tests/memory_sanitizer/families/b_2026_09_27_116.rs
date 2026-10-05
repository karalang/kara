//! B-2026-09-27-116 -- a method of a CONCRETE impl over a generic struct
//! (`impl Bx[String]` over `struct Bx[T] { v: T, n: i64 }`) was declared
//! against the erased struct, so every call failed module verification under
//! `karac build` while `--interp` ran it.

use super::*;

/// Each heap field is released once, including the one replaced through
/// `mut ref self`.
#[test]
fn asan_concrete_impl_over_generic_struct_receivers() {
    assert_clean_asan_run(
        r#"struct Bx[T] { v: T, n: i64 }
impl Bx[String] {
    fn get(self) -> String { return self.v; }
    fn peek(ref self) -> i64 { self.n }
    fn bump(mut ref self) { self.n = self.n + 1; }
    fn setv(mut ref self, s: String) { self.v = s; }
    fn show(ref self) { println(f"show {self.v} {self.n}") }
    fn me(self) -> Self { self }
}
fn main() {
    let mut b = Bx { v: "first-heap-string-xx".to_string(), n: 3 };
    b.bump();
    b.setv("second-heap-string-x".to_string());
    b.show();
    println(f"{b.peek()}");
    let c = b.me();
    c.show();
    println(c.get());
}
"#,
        &[
            "show second-heap-string-x 4",
            "4",
            "show second-heap-string-x 4",
            "second-heap-string-x",
        ],
        "asan_concrete_impl_over_generic_struct_receivers",
    );
}

/// Per-instantiation impls free their own payloads.
#[test]
fn asan_concrete_impl_per_generic_struct_instantiation() {
    assert_clean_asan_run(
        r#"struct Bx[T] { v: T, n: i64 }
impl Bx[String] { fn get(self) -> String { self.v } fn tag(ref self) -> i64 { 1 } }
impl Bx[Vec[i64]] { fn get(self) -> Vec[i64] { self.v } fn tag(ref self) -> i64 { 2 } }
impl Bx[i64] { fn tag(ref self) -> i64 { 3 } }
fn main() {
    let a = Bx { v: "a-heap-string-long-xxxx".to_string(), n: 1 };
    let b = Bx { v: vec![1, 2], n: 2 };
    let c = Bx { v: 5, n: 3 };
    println(f"{a.tag()} {b.tag()} {c.tag()}");
    println(a.get());
    println(f"{b.get().len()}");
}
"#,
        &["1 2 3", "a-heap-string-long-xxxx", "2"],
        "asan_concrete_impl_per_generic_struct_instantiation",
    );
}

/// The two-parameter struct, including the value carried through `Option[Self]`.
#[test]
fn asan_concrete_impl_over_two_param_generic_struct() {
    assert_clean_asan_run(
        r#"struct P[A, B] { a: A, b: B }
impl P[String, Vec[i64]] {
    fn take(self) -> Vec[i64] { self.b }
    fn name(self) -> String { self.a }
    fn wrap(self) -> Option[Self] { Some(self) }
}
fn main() {
    let p = P { a: "nm-heap-string-long-xx".to_string(), b: vec![1, 2, 3] };
    let v = p.take();
    println(f"{v.len()}");
    let q = P { a: "q-heap-string-long-xxx".to_string(), b: vec![4] };
    println(q.name());
    let w = P { a: "w".to_string(), b: vec![8] };
    match w.wrap() { Some(z) => println(f"{z.take().len()}"), None => println("none") }
}
"#,
        &["3", "q-heap-string-long-xxx", "1"],
        "asan_concrete_impl_over_two_param_generic_struct",
    );
}
