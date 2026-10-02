//! B-2026-09-27-111 — a read-only `match` / `if let` / `while let` arm over a
//! concrete enum local makes its binding a VIEW of the local's payload, and an
//! arm that REPLACED the local (`g = E.X(..)`, `g = E.Y`, `clr(mut g)`) freed
//! the payload the view still read: garbage and invalid reads on every
//! compiled surface. Such an arm now keeps the owning path. Plain reads of the
//! local inside the arm, a `ref self` method on it and a reassignment after
//! the view's last read stay where they were, as controls.

use super::*;

/// B-2026-09-27-111 — an arm that reassigns or `mut`-passes the concrete enum local it matches over reads no freed payload.
#[test]
fn asan_readonly_enum_arm_that_replaces_scrutinee_owns_payload() {
    assert_clean_asan_run_min_allocs(
        r#"struct R { id: i64, tag: String }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}-heap-string-long-enough-to-allocate" }; }
enum E { X(R), Y }
struct S { e: E }
impl E { fn clear(mut ref self) { self = E.Y; } fn k(ref self) -> i64 { match self { E.X(r) => r.id, E.Y => 0 } } }
fn clr(e: mut ref E) { e = E.Y; }
fn c1() { let mut g = E.X(mk(20)); match g { E.X(t) => { g = E.X(mk(3)); println(f"x{t.tag}") } E.Y => { println("x0") } } }
fn c2() { let mut g = E.X(mk(21)); match g { E.X(t) => { g = E.Y; println(f"x{t.tag}") } E.Y => { println("x0") } } }
fn c3() { let mut g = E.X(mk(22)); match g { E.X(t) => { println(f"x{t.tag}"); g = E.X(mk(4)); } E.Y => { println("x0") } }; match g { E.X(t) => println(t.id), E.Y => println(0) } }
fn c4() { let mut g = E.X(mk(23)); if let E.X(t) = g { g = E.X(mk(5)); println(f"x{t.tag}") } }
fn c5() { let mut g = E.X(mk(24)); match g { E.X(t) => { clr(mut g); println(f"x{t.tag}") } E.Y => { println("x0") } } }
fn c6() { let g = E.X(mk(25)); match g { E.X(t) => { match g { E.X(u) => println(u.id), E.Y => println(0) }; println(f"x{t.tag}") } E.Y => { println("x0") } } }
fn c7() { let mut g = E.X(mk(27)); match g { E.X(t) => { println(f"x{t.tag}") } E.Y => { println("x0") } }; g = E.Y; match g { E.X(t) => println(t.id), E.Y => println(0) } }
fn c8() { let mut g = E.X(mk(28)); while let E.X(t) = g { g = E.Y; println(f"x{t.tag}") } }
fn c9() { let mut s = S { e: E.X(mk(30)) }; match s.e { E.X(t) => { s.e = E.Y; println(f"x{t.tag}") } E.Y => { println("x0") } } }
fn c10() { let mut g = E.X(mk(31)); match g { E.X(t) => { g.clear(); println(f"x{t.tag}") } E.Y => { println("x0") } } }
fn c11() { let g = E.X(mk(32)); match g { E.X(t) => { println(g.k()); println(f"x{t.tag}") } E.Y => { println("x0") } }; println(g.k()) }
fn c12() { let mut g = E.X(mk(33)); match g { E.X(t) => { println(f"x{t.tag}") } E.Y => { println("x0") } }; match g { E.X(t) => println(t.tag), E.Y => println(0) } }
fn c13() { let mut g = E.X(mk(34)); match g { E.X(t) => { let u = t; g = E.X(mk(3)); println(u.tag) } E.Y => { println("x0") } } }
fn main() {
    c1()
    c2()
    c3()
    c4()
    c5()
    c6()
    c7()
    c8()
    c9()
    c10()
    c11()
    c12()
    c13()
    println("end")
}"#,
        &[
            "xt20-heap-string-long-enough-to-allocate",
            "xt21-heap-string-long-enough-to-allocate",
            "xt22-heap-string-long-enough-to-allocate",
            "4",
            "xt23-heap-string-long-enough-to-allocate",
            "xt24-heap-string-long-enough-to-allocate",
            "25",
            "xt25-heap-string-long-enough-to-allocate",
            "xt27-heap-string-long-enough-to-allocate",
            "0",
            "xt28-heap-string-long-enough-to-allocate",
            "xt30-heap-string-long-enough-to-allocate",
            "xt31-heap-string-long-enough-to-allocate",
            "32",
            "xt32-heap-string-long-enough-to-allocate",
            "32",
            "xt33-heap-string-long-enough-to-allocate",
            "t33-heap-string-long-enough-to-allocate",
            "t34-heap-string-long-enough-to-allocate",
            "end",
        ],
        "asan_readonly_enum_arm_that_replaces_scrutinee_owns_payload",
        20,
    );
}
