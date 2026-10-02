//! B-2026-09-23-21 -- an unannotated `let` bound to an associated function's
//! `Array` (or tuple) return resolves its element type on every spelling of
//! the call, including the type-qualified `H[i64].mk()`.

use super::*;

/// B-2026-09-23-21 — the row's own spelling, `let b = H.mk(); b[0].n`, was
/// fixed by B-2026-09-30-25's two-segment callee arm. The type-qualified
/// spelling of the same call parses as a `MethodCall` on a one-segment
/// generic `Path`, which the let-site resolvers sent to the value-typing
/// `inferred_receiver_type`, so `b` recorded no element type.
///
/// Before: every `H[i64]` cell failed the build (and the JIT) with "cannot
/// resolve field 'n'" while `--interp` printed the right line.
#[test]
fn e2e_type_qualified_assoc_call_array_return_resolves_element() {
    let out = run_program(
        r#"struct P { n: i64 }
struct Q[T] { n: T }
struct S { n: i64, s: String }
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct H { k: i64 }
impl H { fn mk() -> Array[P, 2] { return [P { n: 1 }, P { n: 2 }] } }
struct G[T] { k: T }
impl[T] G[T] {
    fn mk() -> Array[P, 2] { return [P { n: 3 }, P { n: 4 }] }
    fn mkq(x: T, y: T) -> Array[Q[T], 2] { return [Q { n: x }, Q { n: y }] }
    fn mkr() -> Array[R, 2] { return [R { id: 5 }, R { id: 6 }] }
    fn pr() -> (S, S) { return (S { n: 7, s: "a" }, S { n: 8, s: "b" }) }
}
fn main() {
    let a = H.mk(); println(f"a{a[0].n}{a[1].n}");
    let b = G[i64].mk(); println(f"b{b[0].n}{b[1].n}");
    let c = G[i64].mkq(9, 10); println(f"c{c[0].n} {c[1].n}");
    let t = G[i64].pr(); println(f"t{t.0.n}{t.1.s}");
    let r = G[i64].mkr(); println(f"r{r[0].id}{r[1].id}");
}
"#,
    );
    assert_eq!(
        out,
        Some("a12\nb34\nc9 10\nt7b\nr56\nd5\nd6\n".to_string()),
        "type-qualified associated calls must resolve their element type"
    );
}
