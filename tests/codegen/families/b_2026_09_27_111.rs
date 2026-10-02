//! B-2026-09-27-111 — a read-only `match` / `if let` / `while let` arm over a
//! concrete enum local makes its binding a VIEW of the local's payload, and an
//! arm that REPLACED the local (`g = E.X(..)`, `g = E.Y`, `clr(mut g)`) freed
//! the payload the view still read: garbage and invalid reads on every
//! compiled surface. Such an arm now keeps the owning path. Plain reads of the
//! local inside the arm, a `ref self` method on it and a reassignment after
//! the view's last read stay where they were, as controls.

use super::*;

/// B-2026-09-27-111 — an arm that reassigns or `mut`-passes the concrete enum local it matches over keeps the owning path, so its binding still reads the payload; reads of the local stay views.
#[test]
fn e2e_readonly_enum_arm_that_replaces_scrutinee_owns_payload() {
    let src = r#"
struct R { id: i64, tag: String }
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
}"#;
    let want = "xt20-heap-string-long-enough-to-allocate\nxt21-heap-string-long-enough-to-allocate\nxt22-heap-string-long-enough-to-allocate\n4\nxt23-heap-string-long-enough-to-allocate\nxt24-heap-string-long-enough-to-allocate\n25\nxt25-heap-string-long-enough-to-allocate\nxt27-heap-string-long-enough-to-allocate\n0\nxt28-heap-string-long-enough-to-allocate\nxt30-heap-string-long-enough-to-allocate\nxt31-heap-string-long-enough-to-allocate\n32\nxt32-heap-string-long-enough-to-allocate\n32\nxt33-heap-string-long-enough-to-allocate\nt33-heap-string-long-enough-to-allocate\nt34-heap-string-long-enough-to-allocate\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
