//! B-2026-09-28-54 — a fresh `shared` argument (a struct literal, a call's
//! result, or a struct whose only droppable part is a `shared` field) is
//! released once, at the end of its statement, on every backend.

use super::*;

/// B-2026-09-28-54 — `nn(N { v: 1 })` and `yn(mky(2))` ran no `Drop` body
/// on any surface and leaked compiled. Codegen now releases a fresh shared
/// literal argument, a call-built struct's shared field, a handle lent to a
/// `ref` param and one passed to a generic callee, at the statement's end;
/// and each link of a passthrough chain (`nn(idn(mkn(18)))`) owns its own
/// reference. The interpreter releases the handle by refcount at the same
/// point, so a callee that dropped it on one path (`idc(mkn(21), false)`)
/// runs its body and one that stored it (`put`) does not.
#[test]
fn e2e_fresh_shared_arg_released_once_at_statement_end() {
    let src = r#"shared struct N { v: i64 }
impl Drop for N { fn drop(mut ref self) { println(f"dN{self.v}") } }
struct Y { h: N, k: i64 }
fn mkn(n: i64) -> N { N { v: n } }
fn mky(n: i64) -> Y { Y { h: N { v: n }, k: n } }
fn nn(n: N) { println(f"n{n.v}"); }
fn nq(n: N) -> i64 { n.v }
fn rq(n: ref N) -> i64 { n.v }
fn yn(y: Y) -> i64 { y.k }
fn keepn(y: Y) -> N { y.h }
fn idn(n: N) -> N { n }
fn idc(n: N, c: bool) -> N { if c { n } else { mkn(0) } }
fn bump(n: N) -> N { N { v: n.v + 100 } }
fn two(a: N, b: N) -> i64 { a.v + b.v }
fn put(vs: mut ref Vec[N], n: N) { vs.push(n); }
fn gn[T](t: T, n: N) -> i64 { n.v }
struct H { c: i64 }
impl H {
    fn m(ref self, n: N) -> i64 { n.v + self.c }
    fn r(ref self, n: ref N) -> i64 { n.v + self.c }
    fn mk(n: N) -> i64 { n.v }
}
fn main() {
    nn(N { v: 1 }); println("a");
    println(f"b{yn(mky(2))}");
    if yn(mky(3)) > 0 { println("c") }
    let n4 = keepn(mky(4)); println(f"d{n4.v}");
    let e = nq(N { v: 5 }) + nq(mkn(6)); println(f"e{e}");
    let f = rq(N { v: 7 }) + rq(mkn(8)); println(f"f{f}");
    println(f"g{two(N { v: 9 }, mkn(10))}");
    let mut vs: Vec[N] = Vec.new();
    put(mut vs, N { v: 11 }); put(mut vs, mkn(12)); println(f"h{vs.len()}");
    let h = H { c: 1 };
    println(f"i{h.m(N { v: 13 })}{h.r(mkn(14))}{H.mk(N { v: 15 })}");
    println(f"j{gn(0, mkn(16))}{gn(0, N { v: 17 })}");
    nn(idn(mkn(18))); nn(idn(N { v: 19 })); println("k");
    nn(idc(mkn(20), true)); nn(idc(mkn(21), false)); println("l");
    nn(bump(mkn(22))); nn(idn(idn(idn(mkn(23))))); println("m");
    let x = idn(idn(N { v: 24 })); println(f"x{x.v}");
    for i in 0..2 { nn(N { v: 30 + i }); }
    println("end");
}"#;
    let want = "n1\ndN1\na\nb2\ndN2\nc\ndN3\nd4\ndN4\ndN6\ndN5\ne11\ndN8\ndN7\nf15\ng19\ndN10\ndN9\nh2\ni141515\ndN15\ndN14\ndN13\nj1617\ndN17\ndN16\nn18\ndN18\nn19\ndN19\nk\nn20\ndN20\nn0\ndN0\ndN21\nl\nn122\ndN122\ndN22\nn23\ndN23\nm\nx24\ndN24\nn30\ndN30\nn31\ndN31\nend\ndN11\ndN12\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
