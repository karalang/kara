//! B-2026-09-27-51 — an unmutated `let mut` rebind of an owned by-value
//! param runs the param's Drop bodies once.

use super::*;

/// B-2026-09-27-51 — `let mut c = a;` of an owned by-value param that is never
/// mutated (an `Option` / `Result`, plain struct, `Vec`, tuple, enum, generic
/// `T`, or a method param), handed back whole, pushed, or returned on one path,
/// runs each Drop body once. The interpreter's name-keyed param walks treated
/// the `let mut` alias as a fresh owner and ran every body twice; codegen
/// agreed on the plain-struct spellings too. Both now demote the rebind to
/// `let` through one shared predicate (`demoted_param_rebind_names`).
#[test]
fn e2e_unmutated_mut_rebind_of_owned_param_runs_body_once() {
    let src = r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct S { r: R, s: String }
fn mk(i: i64) -> S { S { r: R { id: i }, s: f"heap-string-longer-than-sso-{i}" } }
enum E { A(R), B }
struct H { n: i64 }
impl H { fn rb(ref self, a: R) -> R { let mut c = a; c } }
fn ro(a: Option[S]) -> Option[S] { let mut c = a; c }
fn rp(a: Option[S]) -> i64 { let mut c = a; let mut v: Vec[Option[S]] = Vec.new(); v.push(c); println("in"); 5 }
fn rr(a: R) -> R { let mut c = a; c }
fn rs(a: S) -> S { let mut c = a; c }
fn rv(a: Vec[R]) -> Vec[R] { let mut c = a; c }
fn rt(a: (R, i64)) -> (R, i64) { let mut c = a; c }
fn re(a: E) -> E { let mut c = a; c }
fn rg[T](a: T) -> T { let mut c = a; c }
fn rq(a: Result[S, i64]) -> Result[S, i64] { let mut c = a; c }
fn rk(a: R, k: bool) -> R { let mut c = a; if k { return c; }; R { id: 0 } }
fn main() {
    let a: Option[S] = Some(mk(1));
    let x1 = ro(a);
    let y1 = ro(Some(mk(2)));
    println("k1");
    let b: Option[S] = Some(mk(3));
    println(f"k{rp(b)}");
    println(f"k{rp(Some(mk(4)))}");
    let q = R { id: 5 };
    let x2 = rr(q);
    let y2 = rr(R { id: 6 });
    println("k2");
    let x3 = rs(mk(7));
    println(f"k{x3.r.id} {x3.s}");
    let x4 = rv(vec![R { id: 8 }, R { id: 9 }]);
    println(f"k{x4.len()}");
    let x5 = rt((R { id: 10 }, 4));
    println(f"k{x5.1}");
    let x6 = re(E.A(R { id: 11 }));
    println("k3");
    let x7 = rg(R { id: 12 });
    let y7 = rg(mk(13));
    println(f"k{x7.id} {y7.s}");
    let h = H { n: 1 };
    let x8 = h.rb(R { id: 14 });
    let p = R { id: 15 };
    let y8 = h.rb(p);
    println(f"k{x8.id} {y8.id}");
    let x9 = rk(R { id: 16 }, true);
    let y9 = rk(R { id: 17 }, false);
    println(f"k{x9.id} {y9.id}");
    let x10 = rq(Ok(mk(18)));
    let y10 = rq(Err(3));
    println("k4");
    println("end")
}"#;
    let want = "d1\nd2\nk1\nd3\nin\nk5\nd4\nin\nk5\nd5\nd6\nk2\nk7 heap-string-longer-than-sso-7\nd7\nk2\nd8\nd9\nk4\nd10\nd11\nk3\nk12 heap-string-longer-than-sso-13\nd13\nd12\nk14 15\nd15\nd14\nd17\nk16 0\nd0\nd16\nd18\nk4\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    if let Some(aot) = run_program(src) {
        assert_eq!(aot, want, "AOT");
    }
}
