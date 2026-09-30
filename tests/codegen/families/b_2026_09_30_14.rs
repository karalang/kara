//! B-2026-09-30-14 — a struct-pattern leaf typed by a generic fn's OWN `T`
//! (`fn f[T](g: G[T]) -> T { match g { G { v, .. } => v } }`) had no recorded
//! type, so at `T = String` it registered no buffer free, the match cleanup
//! could not disarm the source for it, and handing it back freed the buffer
//! twice. Beside it, a `let w = x` of a value typed by the monomorph's `T`
//! took `x`'s buffer and registered no free of its own, leaking it.

use super::*;

/// B-2026-09-30-14 — leaves of `G[T]`, `G[G[T]]` and `E[T]` handed back,
/// rebound then handed back, pushed, and returned from an `if let`, plus a
/// bare-`T` rebind that dies in the callee (straight, on one branch, in a
/// loop, out of an arm), at `T = String` and `T = Vec[String]`.
#[test]
fn e2e_generic_fn_struct_leaf_handed_back_and_t_rebind_owned_once() {
    let src = r#"
struct G[T] { v: T, n: i64 }
enum E[T] { A { x: T, k: i64 }, B }
fn mk(p: String, k: i64) -> String { f"{p}-heap-string-longer-than-sso-{k}" }
fn mv(k: i64) -> Vec[String] { let mut v = Vec.new(); v.push(mk("e", k)); v.push(mk("f", k)); v }
fn h1[T](g: G[T]) -> T { match g { G { v, .. } => v } }
fn h2[T](g: G[T]) -> T { match g { G { v, n } => { let w = v; w } } }
fn h3[T](g: G[T]) -> Vec[T] { let mut o = Vec.new(); match g { G { v, .. } => o.push(v) } o }
fn h4[T](g: G[T]) -> T { if let G { v, .. } = g { v } else { panic("no") } }
fn h5[T](g: G[G[T]]) -> T { match g { G { v: G { v, .. }, .. } => v } }
fn h6[T](e: E[T]) -> Option[T] { match e { E.A { x, .. } => Some(x), E.B => None } }
fn k1[T](g: G[T]) -> i64 { match g { G { v, n } => { let w = v; n } } }
fn k2[T](x: T) -> i64 { let w = x; 3 }
fn k3[T](x: T, c: bool) -> i64 { if c { let w = x; 1 } else { 0 } }
fn k4[T](xs: Vec[T]) -> i64 { let mut n = 0; for x in xs { let w = x; n = n + 1; } n }
fn k5[T](e: E[T]) -> i64 { match e { E.A { x, k } => { let w = x; k } E.B => 0 } }
fn main() {
    println(h1(G { v: mk("a", 1), n: 2 }));
    let g = G { v: mk("b", 2), n: 1 };
    println(h1(g));
    println(h2(G { v: mk("c", 3), n: 2 }));
    let o = h3(G { v: mk("d", 4), n: 2 });
    println(o[0]);
    println(h4(G { v: mk("e", 5), n: 2 }));
    println(h5(G { v: G { v: mk("f", 6), n: 1 }, n: 2 }));
    match h6(E.A { x: mk("g", 7), k: 2 }) { Some(s) => println(s), None => println("none") }
    let w = h1(G { v: mv(8), n: 1 });
    println(f"{w.len()} {w[1]}");
    println(f"{k1(G { v: mk("h", 9), n: 2 })} {k2(mk("i", 10))} {k2(mv(11))} {k3(mk("j", 12), true)} {k3(mk("j", 13), false)} {k4(mv(14))} {k5(E.A { x: mk("k", 15), k: 4 })}");
}
"#;
    let want = "a-heap-string-longer-than-sso-1\nb-heap-string-longer-than-sso-2\nc-heap-string-longer-than-sso-3\nd-heap-string-longer-than-sso-4\ne-heap-string-longer-than-sso-5\nf-heap-string-longer-than-sso-6\ng-heap-string-longer-than-sso-7\n2 f-heap-string-longer-than-sso-8\n2 3 3 1 0 2 4\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    if let Some(aot) = run_program(src) {
        assert_eq!(aot, want, "AOT");
    }
}
