//! B-2026-10-04-64 — element `Drop` bodies of a `for` loop over a fresh `Vec`.

use super::*;

/// A `for` loop over a FRESH `Vec` (a call result) takes each element by
/// value, so an element whose type runs a user `Drop` body runs it at the end
/// of its iteration, and the elements a `break` or `return` leaves unvisited
/// run theirs as the loop exits. Before the fix no backend ran any of them
/// (the memory was freed; only the bodies were lost). Covers a plain read,
/// `break`, `continue`, a conditional `push` and a conditional by-value call
/// (the element moved on one path only), a whole move into a `let`, `return`
/// from inside the loop, a match arm that moves an enum payload out, and the
/// row's read-only arm over a `Vec` payload.
#[test]
fn interp_for_loop_over_fresh_vec_runs_element_bodies_per_iteration() {
    let out = run(r#"struct D { n: i64, s: String }
impl Drop for D { fn drop(mut ref self) { println(f"d{self.n}") } }
fn d(n: i64) -> D { D { n: n, s: f"heap-string-longer-than-sso-{n}" } }
fn mk(b: i64) -> Vec[D] { vec![d(b), d(b + 1), d(b + 2)] }
fn eat(x: D) { println(f"eat{x.n}") }
enum E { One(D), Many(Vec[D]), Nothing }
fn me() -> Vec[E] { vec![E.One(d(50)), E.Many(vec![d(51), d(52)]), E.Nothing] }
fn count(xs: ref Vec[D]) -> i64 { xs.len() }
fn find() -> i64 { for item in mk(40) { if item.n == 41 { return item.n } } 0 }
fn main() {
    let mut k = 0;
    for item in mk(1) { k += item.n; }
    println(f"k{k}");
    for item in mk(4) { if item.n == 5 { println("brk"); break; } println(f"b{item.n}"); }
    println("A");
    for item in mk(7) { if item.n == 8 { continue; } println(f"c{item.n}"); }
    println("B");
    let mut held: Vec[D] = Vec.new();
    for item in mk(10) { if item.n == 11 { held.push(item); } }
    println(f"held{held.len()}");
    for item in mk(13) { if item.n != 14 { eat(item); } }
    println("C");
    for item in mk(16) { let x = item; println(f"x{x.n}"); }
    println(f"f{find()}");
    for e in me() { match e { E.One(p) => eat(p), _ => println("other") } }
    println("D");
    for e in me() { match e { E.Many(xs) => { let n = count(xs); println(f"many{n}") } _ => println("other") } }
    println("end")
}
"#);
    assert_eq!(
        out,
        "d1
d2
d3
k6
b4
d4
brk
d5
d6
A
c7
d7
d8
c9
d9
B
d10
d12
held1
d11
eat13
d13
d14
eat15
d15
C
x16
d16
x17
d17
x18
d18
d40
d41
d42
f41
eat50
d50
other
d51
d52
other
D
other
d50
many2
d51
d52
other
end
"
    );
}
