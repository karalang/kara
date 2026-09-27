//! B-2026-09-27-50 — a `let mut` rebind of an owned by-value param that the
//! callee then reassigns runs the param's Drop bodies once.

use super::*;

/// B-2026-09-27-50 — `let mut c = a; ... c = <new>;` over an owned by-value
/// param (a boxed `Option` / `Result`, plain struct, heap struct, enum, `Vec`,
/// generic `T`, or a method param; named and fresh-temp arguments) runs the
/// param's body once, at the reassignment, and the new value's body once.
/// The caller kept its own walk over a value the callee's rebind had already
/// freed as the displaced value: every body ran twice on the interpreter and
/// the compiled program crashed. One AST predicate
/// (`param_reassigned_rebind_local`) now tells both backends that the callee
/// takes the param over. The last call is the unconditional local-container
/// push, whose fresh temp the caller now frees after the callee's entry copy.
#[test]
fn e2e_reassigned_mut_rebind_of_owned_param_runs_body_once() {
    let src = r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct S { r: R, s: String }
fn mk(i: i64) -> S { S { r: R { id: i }, s: f"heap-string-longer-than-sso-{i}" } }
enum E { A(R), B }
struct H { k: i64 }
impl H { fn rb(ref self, a: S) -> i64 { let mut c = a; c = mk(90); println("im"); self.k } }
fn ro(a: Option[S]) -> i64 { let mut c = a; c = Some(mk(91)); println("io"); 1 }
fn rr(a: R) -> i64 { let mut c = a; c = R { id: 92 }; println("ir"); 2 }
fn rs(a: S) -> i64 { let mut c = a; println("mid"); c = mk(93); println("is"); 3 }
fn re(a: E) -> i64 { let mut c = a; c = E.A(R { id: 94 }); println("ie"); 4 }
fn rq(a: Result[S, i64]) -> i64 { let mut c = a; c = Ok(mk(95)); println("iq"); 6 }
fn rv(a: Vec[R]) -> i64 { let mut c = a; c = vec![R { id: 96 }]; println("iv"); c.len() }
fn rg[T](a: T, b: T) -> i64 { let mut c = a; c = b; println("ig"); 7 }
fn rp(a: S) -> i64 { let mut v: Vec[S] = Vec.new(); v.push(a); 8 }
fn main() {
    let a: Option[S] = Some(mk(1));
    println(f"k{ro(a)}");
    println(f"k{ro(Some(mk(2)))}");
    let b = R { id: 3 };
    println(f"k{rr(b)}");
    println(f"k{rr(R { id: 4 })}");
    let c = mk(5);
    println(f"k{rs(c)}");
    println(f"k{rs(mk(6))}");
    let d = E.A(R { id: 7 });
    println(f"k{re(d)}");
    println(f"k{re(E.A(R { id: 8 }))}");
    let e: Result[S, i64] = Ok(mk(9));
    println(f"k{rq(e)}");
    println(f"k{rq(Ok(mk(10)))}");
    let f: Vec[R] = vec![R { id: 11 }];
    println(f"k{rv(f)}");
    println(f"k{rv(vec![R { id: 12 }])}");
    let g = mk(13);
    println(f"k{rg(g, mk(14))}");
    println(f"k{rg(mk(15), mk(16))}");
    let h = H { k: 5 };
    let m = mk(17);
    println(f"k{h.rb(m)}");
    println(f"k{h.rb(mk(18))}");
    println(f"k{rp(mk(19))}");
    println("end")
}"#;
    let want = "d1\nd91\nio\nk1\nd2\nd91\nio\nk1\nd3\nd92\nir\nk2\nd4\nd92\nir\nk2\nmid\nd5\nd93\nis\nk3\nmid\nd6\nd93\nis\nk3\nd7\nd94\nie\nk4\nd8\nd94\nie\nk4\nd9\nd95\niq\nk6\nd10\nd95\niq\nk6\nd11\niv\nd96\nk1\nd12\niv\nd96\nk1\nd13\nig\nd14\nk7\nd15\nig\nd16\nk7\nd17\nd90\nim\nk5\nd18\nd90\nim\nk5\nd19\nk8\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    if let Some(aot) = run_program(src) {
        assert_eq!(aot, want, "AOT");
    }
}
