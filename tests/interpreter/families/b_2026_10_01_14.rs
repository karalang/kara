//! B-2026-10-01-14: closure hand-back

use super::*;

/// B-2026-10-01-14: a closure that hands its by-value struct param back (`|x: R| x`,
/// `{ return x; }`, `{ let y = x; y }`) ran the argument's `Drop` body at the call and
/// again on the result, and compiled `return x` double-freed. Both backends now ask a
/// closure the hand-back questions they ask a named fn: the result owns the body, the
/// compiled closure copies the param at entry, and a discarded result runs it.
#[test]
fn interp_closure_handing_back_its_by_value_param_runs_its_body_once() {
    let out = run(r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}" }; }
fn named() {
    let h = |x: R| x;
    let w = mk(1);
    let r = h(w);
    println(f"r{r.id}");
}
fn fresh() {
    let h = |x: R| x;
    let r = h(mk(2));
    println(f"r{r.id}");
}
fn explicit_return() {
    let h = |x: R| { return x; };
    let r = h(mk(3));
    println(f"r{r.id}");
}
fn rebind() {
    let h = |x: R| { let y = x; y };
    let r = h(mk(4));
    println(f"r{r.id}");
}
fn discarded() {
    let h = |x: R| x;
    h(mk(5));
    let w = mk(6);
    h(w);
    println("-");
}
fn chained() {
    let h = |x: R| x;
    let a = h(mk(7));
    let b = h(a);
    println(f"r{b.id}");
}
fn pushed() {
    let h = |x: R| x;
    let mut v: Vec[R] = Vec.new();
    v.push(h(mk(8)));
    println(f"r{v[0].id}");
}
fn array_param() {
    let f = |x: Array[R, 1]| x;
    let w = mk(9);
    let r = f([w]);
    println("a");
}
fn main() {
    named();
    fresh();
    explicit_return();
    rebind();
    discarded();
    chained();
    pushed();
    array_param();
    println("end");
}
"#);
    assert_eq!(
        out,
        "r1\ndR1\nr2\ndR2\nr3\ndR3\nr4\ndR4\ndR5\ndR6\n-\nr7\ndR7\nr8\ndR8\ndR9\na\nend\n"
    );
}
