//! B-2026-10-06-64 — a struct local moved out and then reassigned drops in its declaration position.

use super::*;

/// B-2026-10-06-64 — a struct local with its own `Drop` that is moved out
/// whole and then reassigned in the SAME block drops its new value in the
/// binding's own declaration position: after every binding declared later
/// (`a`, `d`, `e`, `k`), across a second move and store (`c`), under
/// shadowing (`h`), and after a store that displaces a value (`n`).
///
/// Before: the reassignment re-registered the binding's drop at the end of
/// the block, so the new value dropped first (`a` printed `dR2 dR1`).
/// AOT only: `--interp` is still wrong on this shape (B-2026-10-05-112).
#[test]
fn e2e_moved_struct_reassign_drops_in_declaration_position() {
    let src = r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct S { name: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.name}") } }
fn a() { let mut r = R { id: 1 }; let q = r; r = R { id: 2 }; println(f"a{r.id}{q.id}") }
fn c() { let mut r = R { id: 3 }; let q = r; r = R { id: 4 }; let w = r; r = R { id: 5 }; println(f"c{r.id}{q.id}{w.id}") }
fn d() { let mut r = R { id: 6 }; let z = R { id: 7 }; let q = r; r = R { id: 8 }; println(f"d{r.id}{z.id}{q.id}") }
fn e() { let z = R { id: 9 }; let mut r = R { id: 10 }; let q = r; let y = R { id: 11 }; r = R { id: 12 }; println(f"e{z.id}{r.id}{q.id}{y.id}") }
fn h() { let mut r = R { id: 13 }; let q = r; let mut r = R { id: 14 }; let w = r; r = R { id: 15 }; println(f"h{q.id}{w.id}{r.id}") }
fn k() { let mut s = S { name: "a".to_string() }; let t = s; let u = S { name: "u".to_string() }; s = S { name: "b".to_string() }; println(f"k{t.name}{u.name}{s.name}") }
fn n() { let mut r = R { id: 20 }; let q = r; r = R { id: 21 }; r = R { id: 22 }; println(f"n{q.id}{r.id}") }
fn main() {
    a()
    c()
    d()
    e()
    h()
    k()
    n()
}
"#;
    let want = "a21\ndR1\ndR2\nc534\ndR4\ndR3\ndR5\nd876\ndR6\ndR7\ndR8\ne9121011\ndR11\ndR10\ndR12\ndR9\nh131415\ndR14\ndR15\ndR13\nkaub\ndSu\ndSa\ndSb\ndR21\nn2022\ndR20\ndR22\n";
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
