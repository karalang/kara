//! B-2026-09-30-92 and B-2026-09-30-93 — an `Array` literal of nested arrays
//! or enum elements whose payload runs a `Drop` body.

use super::*;

/// The interpreter half: an `Array[..]` literal binding with no annotation recorded no element
/// type, so it registered no element walk: a nested `Array[Array[mk(4)], ..]`
/// and an `Array[E.A(mk(3)), E.B]` ran no `Drop` body compiled and leaked
/// (-92, -93), and every DISCARDED spelling of an enum-element or nested
/// literal (`let _ =`, a bare statement, a branch, the tuple twin) ran no
/// body on any backend. Covers bound, discarded and statement spellings,
/// nested arrays, enum elements with a unit variant, a rebind, tuple
/// elements, an array returned from a call and a named local moved into a
/// constructor element.
#[test]
fn interp_array_literal_of_nested_or_enum_drop_elements() {
    let out = run(r#"struct W1 { v: i64, s: String }
impl Drop for W1 { fn drop(mut ref self) { println(f"dW{self.v}") } }
enum E { A(W1), B }
fn mk(n: i64) -> W1 { return W1 { v: n, s: f"heap-string-longer-than-sso-{n}" } }
fn mka() -> Array[E, 2] { return Array[E.A(mk(7)), E.B] }
fn nested_bound() { let a = Array[Array[mk(1)], Array[mk(2)]]; println(f"a{a[1][0].v}") }
fn nested_discard() { let _ = Array[Array[mk(3)], Array[mk(4)]]; println("b") }
fn nested_stmt() { Array[Array[mk(5)], Array[mk(6)]]; println("c") }
fn flat_bound() { let a = Array[mk(8), mk(9)]; println(f"d{a[0].v}") }
fn enum_bound() { let a = Array[E.A(mk(10)), E.B]; println("e") }
fn enum_discard() { let _ = Array[E.A(mk(11)), E.B]; println("f") }
fn enum_vec_discard() { let _ = [E.A(mk(12)), E.B]; println("g") }
fn enum_stmt() { Array[E.A(mk(13)), E.B]; println("h") }
fn enum_tuple_discard() { let _ = (E.A(mk(14)), E.B); println("i") }
fn enum_branch_discard(c: bool) { let _ = if c { Array[E.A(mk(15)), E.B] } else { Array[E.B, E.B] }; println("j") }
fn enum_rebind() { let a = Array[E.A(mk(16)), E.A(mk(17))]; let b = a; println("k") }
fn tuple_elems() { let a = Array[(mk(18), 2), (mk(19), 4)]; println("l") }
fn returned() { let a = mka(); println("m") }
fn moved_arg() { let w = mk(20); let _ = Array[E.A(w), E.B]; let x = mk(21); Array[E.A(x), E.B]; println("n") }
fn main() {
    nested_bound(); nested_discard(); nested_stmt(); flat_bound(); enum_bound(); enum_discard();
    enum_vec_discard(); enum_stmt(); enum_tuple_discard(); enum_branch_discard(true);
    enum_branch_discard(false); enum_rebind(); tuple_elems(); returned(); moved_arg();
    println("end");
}
"#);
    assert_eq!(
        out,
        "a2
dW1
dW2
dW3
dW4
b
dW5
dW6
c
d8
dW8
dW9
dW10
e
dW11
f
dW12
g
dW13
h
dW14
i
dW15
j
j
dW16
dW17
k
dW18
dW19
l
dW7
m
dW20
dW21
n
end
"
    );
}
