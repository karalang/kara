//! B-2026-10-04-57: overwriting a whole tuple element runs the displaced value's Drop body in the interpreter

use super::*;

/// The interpreter half of B-2026-10-04-57: `t.0 = <new>` read nothing out of
/// the old element, so its body ran nowhere, where the field (`w.r = ..`) and
/// index (`v[i] = ..`) spellings run it at the store. Same program as the
/// memory-sanitizer fixture, so the two backends are pinned to one answer.
#[test]
fn interp_tuple_elem_overwrite_runs_displaced_body() {
    let out = run(r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct P { r: R, k: i64 }
enum E { A(R), B }
fn mk(i: i64) -> R { R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn viaref(a: mut ref (R, i64)) { a.0 = mk(2); println("f") }
fn moved(c: bool) { let mut t: (R, i64) = (mk(30), 0); if c { let x = t.0; println(f"x{x.id}"); } t.0 = mk(31); println(f"c{t.1}") }
fn main() {
    let mut t: (R, i64) = (mk(5), 0); t.0 = mk(6); println("tuple");
    let mut r: (R, i64) = (mk(1), 7); viaref(mut r); println(f"r{r.1}");
    let mut p: (P, i64) = (P { r: mk(10), k: 1 }, 0); p.0 = P { r: mk(11), k: 2 }; println("p");
    let mut e: (E, i64) = (E.A(mk(20)), 0); e.0 = E.A(mk(21)); println("e1"); e.0 = E.B; println("e2");
    let mut o: (Option[R], i64) = (Some(mk(25)), 0); o.0 = Some(mk(26)); println("o1"); o.0 = None; println("o2");
    let mut l: (R, i64) = (mk(40), 0); for i in 41..43 { l.0 = mk(i); } println(f"l{l.0.id}");
    moved(false);
    println(f"end{t.1}{p.1}{e.1}{o.1}");
}
"#);
    assert_eq!(out, "dR5\ntuple\ndR1\nf\nr7\ndR2\ndR10\np\ndR20\ne1\ndR21\ne2\ndR25\no1\ndR26\no2\ndR40\ndR41\nl42\ndR42\ndR30\nc0\ndR31\nend0000\ndR11\ndR6\n");
}
