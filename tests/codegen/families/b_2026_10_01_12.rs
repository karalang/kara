//! B-2026-10-01-12 -- an `Option`/`Result`/enum payload binding moved into
//! a collection literal (`Some(w) => [w]`) leaves the parameter's frame, so
//! the caller's walk over the argument stands down.

use super::*;

/// B-2026-10-01-12 — `Some(w) => [w]` over a by-value param ran `w`'s body
/// twice under `--interp`: once with the array, once in the caller's walk over
/// the fresh `Some(..)` argument. The payload-escape predicate knew a tuple
/// and a struct literal but not a collection literal. A NAMED argument and a
/// user enum ran it twice on every backend. Also covered: braced through a
/// local, `Vec[..]`, `if let`, a two-element array, a nested tuple, `Result`,
/// a method, the `None` path, and an `Array` result returned bare or by
/// `return`.
#[test]
fn e2e_payload_binding_moved_into_collection_literal_runs_body_once() {
    let src = r#"struct W1 { v: i64, s: String }
impl Drop for W1 { fn drop(mut ref self) { println(f"dW1_{self.v}") } }
fn mk(n: i64) -> W1 { return W1 { v: n, s: f"ssssssssssssssssssssssssssssss{n}" } }
enum E { A(W1), B }
struct H { k: i64 }
impl H {
    fn arr(ref self, o: Option[W1]) {
        let t = match o { Some(w) => [w], None => [mk(0)] };
        println(f"me{t[0].v}")
    }
}
fn arr(o: Option[W1]) {
    let t = match o { Some(w) => [w], None => [mk(0)] };
    println(f"ar{t[0].v}")
}
fn braced(o: Option[W1]) {
    let t = match o { Some(w) => { let z = [w]; z }, None => [mk(0)] };
    println(f"al{t[0].v}")
}
fn vec(o: Option[W1]) {
    let t = match o { Some(w) => Vec[w], None => Vec[mk(0)] };
    println(f"ve{t[0].v}")
}
fn iflet(o: Option[W1]) {
    let t = if let Some(w) = o { [w] } else { [mk(0)] };
    println(f"il{t[0].v}")
}
fn two(o: Option[W1]) {
    let t = match o { Some(w) => [w, mk(50)], None => [mk(0), mk(1)] };
    println(f"tw{t[0].v}")
}
fn nest(o: Option[W1]) {
    let t = match o { Some(w) => [(w, 1)], None => [(mk(0), 0)] };
    println(f"ne{t[0].1}")
}
fn res(o: Result[W1, String]) {
    let t = match o { Ok(w) => [w], Err(_) => [mk(0)] };
    println(f"rs{t[0].v}")
}
fn uenum(o: E) {
    let t = match o { E.A(w) => [w], E.B => [mk(0)] };
    println(f"ue{t[0].v}")
}
fn ret(o: Option[W1]) -> Array[W1, 1] { match o { Some(w) => [w], None => [mk(0)] } }
fn ret2(o: Option[W1]) -> Array[W1, 1] {
    match o { Some(w) => return [w], None => {} };
    [mk(0)]
}
fn main() {
    arr(Some(mk(1)));
    arr(None);
    braced(Some(mk(2)));
    vec(Some(mk(3)));
    iflet(Some(mk(4)));
    two(Some(mk(5)));
    nest(Some(mk(6)));
    res(Ok(mk(7)));
    uenum(E.A(mk(8)));
    let h = H { k: 1 };
    h.arr(Some(mk(9)));
    let o = Some(mk(10));
    arr(o);
    let r = ret(Some(mk(11)));
    println(f"rt{r[0].v}");
    let r2 = ret2(Some(mk(12)));
    println(f"r2{r2[0].v}");
    println("end")
}
"#;
    let want = "ar1\ndW1_1\nar0\ndW1_0\nal2\ndW1_2\nve3\ndW1_3\nil4\ndW1_4\ntw5\ndW1_5\ndW1_50\nne1\ndW1_6\nrs7\ndW1_7\nue8\ndW1_8\nme9\ndW1_9\nar10\ndW1_10\nrt11\ndW1_11\nr212\ndW1_12\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
