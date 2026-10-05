//! B-2026-10-05-13 -- a `Self`-typed parameter in a concrete impl
//! (`fn cself(self, o: Self)`) reached codegen as a path naming no type: a
//! generic enum target failed module verification, a plain struct's `o.a`
//! had no field layout, and a trait impl over `Vec[i64]` had no `o.len()`.

use super::*;

/// The `Self` argument and the receiver are each released once.
#[test]
fn asan_self_param_in_concrete_generic_enum_impl() {
    assert_clean_asan_run(
        r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}"); } }
enum G[T] { X(T), Y }
impl G[R] { fn cself(self, o: Self) -> i64 { 1 } }
fn mk(i: i64) -> G[R] { G.X(R { id: i, s: f"s{i}" }) }
fn main() { let d = mk(4); println(f"{d.cself(mk(5))}"); println("end") }
"#,
        &["dR5", "1", "dR4", "end"],
        "asan_self_param_in_concrete_generic_enum_impl",
    );
}

/// `Self` parameters on a plain struct release each String once.
#[test]
fn asan_self_param_in_plain_struct_impl() {
    assert_clean_asan_run(
        r#"struct Q { a: String, n: i64 }
impl Q {
    fn both(self, o: Self) -> String { o.a }
    fn pair(ref self, o: ref Self) -> i64 { self.n + o.n }
    fn tup(ref self, t: (Self, i64)) -> i64 { t.1 + t.0.n }
}
fn main() {
    let r = Q { a: "r".to_string(), n: 1 };
    let s = Q { a: "s-heap-string-long-xxx".to_string(), n: 2 };
    println(f"{r.pair(s)}");
    let t = Q { a: "t-heap-string-long-xxx".to_string(), n: 5 };
    println(f"{r.tup((t, 3))}");
    println(r.both(s));
}
"#,
        &["3", "8", "s-heap-string-long-xxx"],
        "asan_self_param_in_plain_struct_impl",
    );
}
