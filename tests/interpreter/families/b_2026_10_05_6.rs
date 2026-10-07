//! B-2026-10-05-6: match on a takeover rebind of a by-value Option param

use super::*;

/// B-2026-10-05-6: a `let mut` rebind of a by-value `Option` param that is MATCHED
/// on (the arm only reading its payload) and then reassigned. The match was not
/// admitted as a read of the rebind, so the callee was not seen to take its value
/// over and compiled no body ran for it. The callee's local now owns the value, and
/// the param spelling agrees with the local one (`q3`): the matched payload dies
/// right after the `match`, on both backends. Cells cover `Option[Array[R, 2]]`
/// and `Option[R]`, temporary and named arguments, a guard, a match inside a
/// branch, a branch reassign, and the unmutated control `q2`.
#[test]
fn interp_match_on_takeover_rebind_of_option_param_runs_its_body() {
    let out = run(r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}" }; }
fn q1(x: Option[Array[R, 2]]) { let mut h = x; match h { Some(a) => println(f"m{a[0].id}"), None => println("n") } h = None; println("q1"); }
fn q2(x: Option[Array[R, 2]]) { let h = x; match h { Some(a) => println(f"m{a[0].id}"), None => println("n") } println("q2"); }
fn q3() { let mut h: Option[Array[R, 2]] = Some([mk(1), mk(2)]); match h { Some(a) => println(f"m{a[0].id}"), None => println("n") } h = None; println("q3"); }
fn q4(x: Option[R]) { let mut h = x; match h { Some(a) => println(f"m{a.id}"), None => println("n") } h = None; println("q4"); }
fn q6(x: Option[R], c: bool) { let mut h = x; if c { match h { Some(a) => println(f"m{a.id}"), None => println("n") } } h = None; println("q6"); }
fn q7(x: Option[R]) { let mut h = x; match h { Some(a) if a.id > 3 => println(f"g{a.id}"), _ => println("o") } h = None; println("q7"); }
fn q8(x: Option[R], c: bool) { let mut h = x; match h { Some(a) => println(f"m{a.id}"), None => println("n") } if c { h = None; } println("q8"); }
fn main() {
    q1(Some([mk(11), mk(12)])); println("_a1");
    q2(Some([mk(21), mk(22)])); println("_a2");
    q3(); println("_a3");
    q4(Some(mk(41))); println("_a4");
    let o = Some(mk(42)); q4(o); println("_a5");
    let o: Option[Array[R, 2]] = Some([mk(13), mk(14)]); q1(o); println("_a6");
    q6(Some(mk(61)), true); println("_a7");
    q7(Some(mk(71))); println("_a8");
    q8(Some(mk(81)), false); println("_a9");
    let o = Some(mk(82)); q8(o, true); println("_b1");
    println("end")
}
"#);
    assert_eq!(
        out,
        "m11\ndR11\ndR12\nq1\n_a1\nm21\nq2\ndR21\ndR22\n_a2\nm1\ndR1\ndR2\nq3\n_a3\nm41\ndR41\nq4\n_a4\nm42\ndR42\nq4\n_a5\nm13\ndR13\ndR14\nq1\n_a6\nm61\ndR61\nq6\n_a7\ng71\ndR71\nq7\n_a8\nm81\ndR81\nq8\n_a9\nm82\ndR82\nq8\n_b1\nend\n",
        "got:\n{out}"
    );
}
