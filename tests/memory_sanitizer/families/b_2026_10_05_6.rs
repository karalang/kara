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
fn asan_match_on_takeover_rebind_of_option_param_runs_its_body() {
    assert_clean_asan_run(
        r#"struct R { id: i64, name: String }
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
"#,
        &[
            "m11", "dR11", "dR12", "q1", "_a1", "m21", "q2", "dR21", "dR22", "_a2", "m1", "dR1",
            "dR2", "q3", "_a3", "m41", "dR41", "q4", "_a4", "m42", "dR42", "q4", "_a5", "m13",
            "dR13", "dR14", "q1", "_a6", "m61", "dR61", "q6", "_a7", "g71", "dR71", "q7", "_a8",
            "m81", "dR81", "q8", "_a9", "m82", "dR82", "q8", "_b1", "end",
        ],
        "asan_match_on_takeover_rebind_of_option_param_runs_its_body",
    );
}
