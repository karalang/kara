//! B-2026-09-29-2 — a by-value `Option`/`Result` param whose payload is
//! destructured into its fields (`Some(S { r, s }) => r.id`) and whose field
//! bindings are only read keeps the payload's `Drop` bodies with the caller,
//! as the interpreter does. A projection off a field binding was resolved from
//! the PAYLOAD's type, where `r.id` names no field, so it scored as the payload
//! escaping; both ends of the call then left the body to the callee, which has
//! no owner for a destructured field, and a fresh temp's body ran on no
//! compiled surface.

use super::*;

/// B-2026-09-29-2 — `match`, `if let`, `let .. else`, `while let`, a guard, a
/// `..` rest, renamed and swapped field bindings, a `Result`, a read after a
/// later statement, a field moved out beside a read one, and a conditional
/// move, each with a fresh temp and a named argument, plus a loop. Before the
/// fix every fresh-temp body except the moved-out ones was lost at `-O0` and
/// `-O2`.
#[test]
fn e2e_optres_param_struct_subpattern_field_read_runs_temp_body_at_caller() {
    let src = r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct S { r: R, s: String }
struct T { a: R, b: R }
fn mks(i: i64) -> S { S { r: R { id: i }, s: f"heap-string-longer-than-sso-{i}" } }
fn mkt(i: i64) -> T { T { a: R { id: i }, b: R { id: i + 100 } } }
fn keep(x: R) { println(f"kp{x.id}") }
fn eats(x: String) { println("es") }
fn f1(o: Option[S]) -> i64 { match o { Some(S { r, s }) => r.id, None => 0 } }
fn f2(o: Option[S]) -> i64 { match o { Some(S { r, .. }) => r.id, None => 0 } }
fn f3(o: Option[S]) -> i64 { if let Some(S { r, s }) = o { r.id } else { 0 } }
fn f4(o: Option[S]) -> i64 { if let Some(S { r: q, s: _ }) = o { q.id } else { 0 } }
fn f5(o: Option[S]) -> i64 { let Some(S { r, s }) = o else { return 0 }; println("mid"); r.id }
fn f6(o: Option[S]) -> i64 { match o { Some(S { r, s }) => { eats(s); r.id } None => 0 } }
fn f7(o: Option[S]) -> i64 { if let Some(S { r, s }) = o { if r.id > 5 { keep(r); return 5; } return r.id; } 0 }
fn f8(o: Option[T]) -> i64 { match o { Some(T { a, b }) => { keep(b); a.id } None => 0 } }
fn f9(o: Option[T]) -> i64 { match o { Some(T { a: x, .. }) => x.id, None => 0 } }
fn f10(o: Option[S]) -> i64 { let mut t = 0; while let Some(S { r, s }) = o { t = r.id; break; } t }
fn f11(o: Option[S]) -> i64 { match o { Some(S { r, s }) if r.id > 3 => r.id, Some(S { r, s }) => 0 - r.id, None => 0 } }
fn f12(o: Option[S]) -> i64 { match o { Some(S { r: s, s: r }) => s.id, None => 0 } }
fn f13(o: Result[S, i64]) -> i64 { match o { Ok(S { r, s }) => r.id, Err(e) => e } }
fn f14(o: Option[S]) -> i64 { let v = match o { Some(S { r, s }) => r.id, None => 0 }; println("post"); v }
fn main() {
    println(f"a{f1(Some(mks(1)))}"); let x1 = Some(mks(2)); println(f"b{f1(x1)}");
    println(f"a{f2(Some(mks(3)))}"); let x2 = Some(mks(4)); println(f"b{f2(x2)}");
    println(f"a{f3(Some(mks(5)))}"); let x3 = Some(mks(6)); println(f"b{f3(x3)}");
    println(f"a{f4(Some(mks(7)))}"); let x4 = Some(mks(8)); println(f"b{f4(x4)}");
    println(f"a{f5(Some(mks(9)))}"); let x5 = Some(mks(10)); println(f"b{f5(x5)}");
    println(f"a{f6(Some(mks(11)))}"); let x6 = Some(mks(12)); println(f"b{f6(x6)}");
    println(f"a{f7(Some(mks(3)))}"); let x7 = Some(mks(14)); println(f"b{f7(x7)}");
    println(f"a{f8(Some(mkt(15)))}"); let x8 = Some(mkt(16)); println(f"b{f8(x8)}");
    println(f"a{f9(Some(mkt(17)))}"); let x9 = Some(mkt(18)); println(f"b{f9(x9)}");
    println(f"a{f10(Some(mks(19)))}"); let x10 = Some(mks(20)); println(f"b{f10(x10)}");
    println(f"a{f11(Some(mks(21)))}"); let x11 = Some(mks(2)); println(f"b{f11(x11)}");
    println(f"a{f12(Some(mks(23)))}"); let x12 = Some(mks(24)); println(f"b{f12(x12)}");
    println(f"a{f13(Ok(mks(25)))}"); let x13: Result[S, i64] = Ok(mks(26)); println(f"b{f13(x13)}");
    println(f"a{f14(Some(mks(27)))}"); let x14 = Some(mks(28)); println(f"b{f14(x14)}");
    let mut i = 30; while i < 33 { println(f"l{f1(Some(mks(i)))}"); i = i + 1; }
    println("end")
}"#;
    let want = "d1\na1\nb2\nd2\nd3\na3\nb4\nd4\nd5\na5\nb6\nd6\nd7\na7\nb8\nd8\nmid\nd9\na9\nmid\nb10\nd10\nes\nd11\na11\nes\nb12\nd12\nd3\na3\nkp14\nb5\nd14\nkp115\nd115\nd15\na15\nkp116\nb16\nd116\nd16\nd117\nd17\na17\nb18\nd118\nd18\nd19\na19\nb20\nd20\nd21\na21\nb-2\nd2\nd23\na23\nb24\nd24\nd25\na25\nb26\nd26\npost\nd27\na27\npost\nb28\nd28\nd30\nl30\nd31\nl31\nd32\nl32\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    if let Some(aot) = run_program(src) {
        assert_eq!(aot, want, "AOT");
    }
}
