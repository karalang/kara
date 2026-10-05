//! B-2026-10-05-90: by-value generic-struct param of a concrete impl method

use super::*;

/// B-2026-10-05-90: a by-value param whose type is a generic struct, on a method of
/// a CONCRETE impl over that struct (`impl P[String, Vec[i64]] { fn eat(ref self,
/// o: P[String, Vec[i64]]) }`). The method body compiles under the impl's
/// substitution, which let the monomorph entry-copy rescue fire for `o`; the
/// caller, which decides without a substitution, took the transfer arm and
/// retracted its drop, so the callee freed only its copy and the argument leaked
/// (and a `Drop` field lost its body). Cells cover named and temporary
/// arguments, a field moved out, a hand-back, another generic struct type, an
/// owned `self` beside the param, and a `P[R, i64]` impl whose field runs a
/// body.
#[test]
fn e2e_concrete_impl_generic_struct_param_drops_once() {
    let Some(out) = run_program(
        r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"name-{i}-xxxxxxxxxxxxxxxxxxxxxx" }; }
struct P[A, B] { a: A, b: B }
struct W[A] { a: A, n: i64 }
impl P[String, Vec[i64]] {
    fn eat(ref self, o: P[String, Vec[i64]]) -> i64 { o.b.len() }
    fn both(self, o: P[String, Vec[i64]]) -> i64 { 2 }
    fn take(ref self, o: P[String, Vec[i64]]) -> String { let s = o.a; s }
    fn back(ref self, o: P[String, Vec[i64]]) -> P[String, Vec[i64]] { o }
    fn w(ref self, o: W[String]) -> i64 { o.n }
}
impl P[R, i64] {
    fn eatr(ref self, o: P[R, i64]) -> i64 { o.b }
    fn backr(ref self, o: P[R, i64]) -> P[R, i64] { o }
}
fn mkp(t: String) -> P[String, Vec[i64]] { return P { a: f"{t}-heap-xxxxxxxxxxxxxxxxxxxxxxx", b: vec![7, 8] }; }
fn main() {
    let r = mkp("r");
    let a = mkp("a"); println(f"e{r.eat(a)}"); println("_a1");
    println(f"e{r.eat(mkp("t"))}"); println("_a2");
    let a = mkp("b"); println(f"t{r.take(a)}"); println("_a3");
    let a = mkp("c"); let k = r.back(a); println(f"k{k.b.len()}"); println("_a4");
    let w = W { a: f"w-heap-xxxxxxxxxxxxxxxxxxxxxxxxx", n: 5 }; println(f"w{r.w(w)}"); println("_a5");
    let q = P { a: mk(1), b: 0 };
    let o = P { a: mk(2), b: 3 }; let x = q.eatr(o); println(f"r{x}"); println("_a6");
    let o = P { a: mk(4), b: 5 }; let k2 = q.backr(o); println(f"k{k2.a.id}"); println("_a7");
    let x = q.eatr(P { a: mk(6), b: 7 }); println(f"r{x}"); println("_a8");
    let a = mkp("d"); println(f"b{r.both(a)}"); println("_a9");
    println("end");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        "e2\n_a1\ne2\n_a2\ntb-heap-xxxxxxxxxxxxxxxxxxxxxxx\n_a3\nk2\n_a4\nw5\n_a5\ndR2\nr3\n_a6\nk4\ndR4\n_a7\ndR6\ndR1\nr7\n_a8\nb2\n_a9\nend\n",
        "got:\n{out}"
    );
}
