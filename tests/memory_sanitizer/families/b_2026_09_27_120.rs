//! B-2026-09-27-120 -- with two concrete impls of one generic enum, a method
//! of either is callable on a temp receiver.

use super::*;

/// B-2026-09-27-120 — two impls on one head emit every method of the group
/// under its qualified segment (`G1[String].show`), and the fresh-temp
/// receiver path gated on the bare `G1.show`, which never exists, so
/// `G1.Y([4, 5]).show()` and even `G1.Y(f"..").shm()` (a method only one impl
/// defines) failed with "no handler for method ... on non-identifier
/// receiver". Named receivers already dispatched correctly. The bodies do not
/// `match self` with a wildcard arm, which leaks a boxed payload on its own
/// (B-2026-10-01-46).
#[test]
fn asan_two_concrete_impls_temp_receiver_dispatch() {
    assert_clean_asan_run(
        r#"enum G1[T] { Y(T), N }
impl G1[Vec[i64]] { fn show(self) -> i64 { println("v show"); return 1; } }
impl G1[String] {
    fn show(self) -> i64 { println("s show"); return 2; }
    fn shm(self) -> i64 { println("s shm"); return 3; }
}
fn main() {
    let a = G1.Y([4, 5]).show();
    let b = G1.Y(f"a{1}").shm();
    let c = G1.Y(f"b{2}").show();
    let s: G1[String] = G1.Y(f"c{3}");
    let d = s.show();
    println(f"{a} {b} {c} {d}");
}
"#,
        &["v show", "s shm", "s show", "s show", "1 3 2 2"],
        "B-2026-09-27-120 two concrete impls dispatch on a temp receiver",
    );
}
