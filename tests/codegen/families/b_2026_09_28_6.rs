//! B-2026-09-28-6 — a bare seeded constructor discarded in statement position
//! (`Some(mk(1));`) runs its payload's `Drop` body under `--interp` as it does
//! compiled; a discarded NESTED envelope (`Option[Option[S]]`) frees every box;
//! and a nested constructor over an owned param (`Some(Some(x))`) leaves the
//! body to the caller at every position.

use super::*;

/// B-2026-09-28-6 — bare `Some`/`Ok`/`Err` discards ran no body under
/// `--interp` (compiled ran one); a nested `Some(Some(..))` / `Some(Ok(..))`
/// discard leaked both boxes compiled; and `Some(Some(x))` over a param ran
/// the body in the frame AND in the caller (`d11 in1 d11`) on every backend.
#[test]
fn e2e_seeded_ctor_discard_and_nested_param_view_run_once() {
    let src = r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct S { r: R, s: String }
fn mk(i: i64) -> S { S { r: R { id: i }, s: f"heap-string-longer-than-sso-{i}" } }
fn mm(i: i64) -> Option[Option[S]] { Some(Some(mk(i))) }
fn mr(i: i64) -> Option[Result[S, i64]] { Some(Ok(mk(i))) }
fn c1(x: S) { Some(Some(x)); println("in1") }
fn c2(x: S) { let _ = Some(Some(x)); println("in2") }
fn c3(x: S) -> bool { Some(Some(x)).is_some() }
fn c4(x: S) { let o = Some(Some(x)); println("in4") }
fn c5(x: S) { let o = Some(Some(x)); match o { Some(Some(s)) => println(f"m{s.r.id}"), _ => println("x") } println("in5") }
fn c6(x: S) { let o = Some(Some(x)); let o2 = o; println("in6") }
fn main() {
    Some(mk(1));
    println("a");
    Ok(mk(2));
    println("b");
    Err(mk(3));
    println("c");
    Some((mk(4), 4));
    println("e");
    let r = mk(5);
    Some(r);
    println("f");
    Some(Some(mk(6)));
    println("g");
    let _ = Some(Some(mk(7)));
    println("h");
    mm(8);
    println("i");
    mr(9);
    println("j");
    let b = Some(Some(mk(10))).is_some();
    println(f"k{b}");
    c1(mk(11));
    println("o1");
    c2(mk(12));
    println("o2");
    let t = c3(mk(13));
    println(f"o3{t}");
    c4(mk(14));
    println("o4");
    c5(mk(15));
    println("o5");
    c6(mk(16));
    println("o6");
    println("end")
}"#;
    let want = "d1\na\nd2\nb\nd3\nc\nd4\ne\nd5\nf\nd6\ng\nd7\nh\nd8\ni\nd9\nj\nd10\nktrue\nin1\nd11\no1\nin2\nd12\no2\nd13\no3true\nin4\nd14\no4\nm15\nin5\nd15\no5\nin6\nd16\no6\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    if let Some(aot) = run_program(src) {
        assert_eq!(aot, want, "AOT");
    }
}
