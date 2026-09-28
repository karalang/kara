//! B-2026-09-20-21 -- a `let` that rebinds a name, in a sibling block or in
//! the same scope, starts from a clean slate: the earlier binding's boxed
//! struct-payload facts no longer make a boxed TUPLE payload read as one.

use super::*;

/// B-2026-09-20-21 — the second `a` / `b` is a boxed `Option[(R, R)]` that
/// `ttake` takes over. Before: the first binding of the name was a boxed
/// `Option[Q]` struct payload, its facts survived the rebind, the arg site
/// kept the box with the caller, and the callee freed it too, so the
/// surviving element ran its body twice and the process aborted with a double
/// free on jit, `-O0` and `-O2`.
#[test]
fn e2e_rebound_name_does_not_inherit_boxed_struct_payload_facts() {
    let Some(out) = run_program(
        r#"struct R { id: i64, v: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}v{self.v}") } }
struct Q { r: R, s: R }
fn mkr(k: i64) -> R { return R { id: k, v: k * 10 } }
fn peek(o: Option[Q]) { match o { Option.Some(t) => { println(f"p{t.r.v}.{t.s.v}") } Option.None => { println("n") } } }
fn ttake(o: Option[(R, R)]) { match o { Option.Some(t) => { let x = t.0; println(f"t{x.v}.{t.1.v}") } Option.None => { println("n") } } }
fn main() {
    { let a = Option.Some(Q { r: mkr(1), s: mkr(2) }); peek(a) }
    println("out1");
    { let a = Option.Some((mkr(3), mkr(4))); ttake(a) }
    println("out2");
    let b = Option.Some(Q { r: mkr(5), s: mkr(6) });
    peek(b);
    let b = Option.Some((mkr(7), mkr(8)));
    ttake(b);
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        "p10.20\nd2v20\nd1v10\nout1\nt30.40\nd3v30\nd4v40\nout2\np50.60\nd6v60\nd5v50\nt70.80\nd7v70\nd8v80\nend\n",
        "got:\n{out}"
    );
}
