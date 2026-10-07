//! B-2026-09-30-104 / B-2026-09-30-77 — a GENERIC struct boxed in an
//! `Option`/`Result` payload was laid out, walked and width-checked by its
//! NAME, where `v: T` is one erased word. So a `G2[R]` payload was freed as
//! the wrong type (invalid frees and crashes compiled), a `G[String]` leaked
//! its buffer, and a partial destructure skipped the cap-zeroing that keeps
//! the box from freeing the taken field again. Codegen now reads all four at
//! the instantiation (`enum_inst_var_types`, or the scrutinee's type for a
//! fresh temp).

use super::*;

/// B-2026-09-30-104 — `G2[R] { v: T, r: R }` boxed in an `Option`/`Result`, named and as a fresh temp: unbound, bound whole, rebound, wildcard. Every cell crashed or leaked compiled before the fix.
#[test]
fn e2e_generic_struct_boxed_optres_payload_freed_at_instantiation() {
    let src = r#"
struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct G2[T] { v: T, r: R }
fn mk(k: i64) -> R { R { id: k, s: f"a-heap-string-longer-than-sso-{k}" } }
fn mkres(k: i64) -> Result[G2[R], i64] { Result.Ok(G2 { v: mk(k), r: mk(k * 10) }) }
fn a1() -> i64 { let o = Option.Some(G2 { v: mk(1), r: mk(11) }); 1 }
fn a2() -> i64 { let o = Option.Some(G2 { v: mk(2), r: mk(22) }); match o { Option.Some(g) => g.v.id, Option.None => 0 } }
fn a5() -> i64 { let o = Option.Some(G2 { v: mk(5), r: mk(55) }); match o { Option.Some(_) => 5, Option.None => 0 } }
fn a9() -> i64 { let o = Option.Some(G2 { v: mk(9), r: mk(99) }); match o { Option.Some(g) => { let h = g; h.r.id }, Option.None => 0 } }
fn a11() -> i64 { match Option.Some(G2 { v: mk(11), r: mk(111) }) { Option.Some(g) => g.v.id, Option.None => 0 } }
fn b1() -> i64 { let o: Result[G2[R], i64] = Result.Ok(G2 { v: mk(13), r: mk(133) }); 13 }
fn b2() -> i64 { let o: Result[G2[R], i64] = Result.Ok(G2 { v: mk(14), r: mk(144) }); match o { Result.Ok(g) => g.v.id, Result.Err(e) => e } }
fn b9() -> i64 { match mkres(9) { Result.Ok(g) => g.r.id, Result.Err(e) => e } }
fn main() {
    println(a1()); println(a2()); println(a5()); println(a9()); println(a11());
    println(b1()); println(b2()); println(b9());
    println("end")
}"#;
    let want = "d11\nd1\n1\nd22\nd2\n2\nd55\nd5\n5\nd99\nd9\n99\nd111\nd11\n11\nd133\nd13\n13\nd144\nd14\n14\nd90\nd9\n90\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}

/// B-2026-09-30-77 (Option half) — `G[String]` and `G[R]` in an `Option`: the payload was walked at the erased layout, leaking the `String` and losing `R`'s body when bound whole.
#[test]
fn e2e_generic_struct_option_payload_whole_bind_keeps_bodies() {
    let src = r#"
struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct G[T] { v: T, n: i64 }
fn mk(k: i64) -> R { R { id: k, s: f"a-heap-string-longer-than-sso-{k}" } }
fn ms(k: i64) -> String { f"a-heap-string-longer-than-sso-{k}" }
fn c1() -> i64 { let o = Option.Some(G { v: ms(1), n: 1 }); 1 }
fn c2() -> i64 { let o = Option.Some(G { v: ms(2), n: 2 }); match o { Option.Some(g) => g.v.len() + g.n, Option.None => 0 } }
fn c3() -> i64 { let o = Option.Some(G { v: ms(3), n: 3 }); match o { Option.Some(G { v, n }) => v.len() + n, Option.None => 0 } }
fn c4() -> i64 { let o = Option.Some(G { v: ms(4), n: 4 }); match o { Option.Some(G { n, .. }) => n, Option.None => 0 } }
fn c5() -> i64 { let o = Option.Some(G { v: ms(5), n: 5 }); match o { Option.Some(_) => 5, Option.None => 0 } }
fn d1() -> i64 { let o = Option.Some(G { v: mk(1), n: 1 }); 1 }
fn d2() -> i64 { let o = Option.Some(G { v: mk(2), n: 2 }); match o { Option.Some(g) => g.v.id + g.n, Option.None => 0 } }
fn d3() -> i64 { let o = Option.Some(G { v: mk(3), n: 3 }); match o { Option.Some(G { v, n }) => v.id + n, Option.None => 0 } }
fn d4() -> i64 { let o = Option.Some(G { v: mk(4), n: 4 }); match o { Option.Some(G { n, .. }) => n, Option.None => 0 } }
fn main() {
    println(c1()); println(c2()); println(c3()); println(c4()); println(c5());
    println(d1()); println(d2()); println(d3()); println(d4());
    println("end")
}"#;
    let want = "1\n33\n34\n4\n5\nd1\n1\nd2\n4\nd3\n6\nd4\n4\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}

/// B-2026-09-30-104 — a partial destructure (`G3 { v, .. }`) of a boxed generic payload: the destructure suppressor measured the ERASED width, fit it in the area, and skipped, so the box freed the taken field again. Memory-only (no `Drop` bodies) so no -103 ordering is pinned; each arm prints the taken string so -O2 keeps the allocations.
#[test]
fn e2e_generic_struct_boxed_payload_partial_destructure_frees_rest() {
    let src = r#"
struct G3[T] { v: T, w: String }
fn ms(k: i64) -> String { f"a-heap-string-longer-than-sso-{k}" }
fn mkres(k: i64) -> Result[G3[String], i64] { Result.Ok(G3 { v: ms(k), w: ms(k * 10) }) }
fn m1() -> i64 { let o: Result[G3[String], i64] = Result.Ok(G3 { v: ms(1), w: ms(11) }); match o { Result.Ok(G3 { v, .. }) => { println(v); v.len() }, Result.Err(e) => e } }
fn m2() -> i64 { let o: Result[G3[String], i64] = Result.Ok(G3 { v: ms(2), w: ms(22) }); match o { Result.Ok(G3 { w, .. }) => { println(w); w.len() }, Result.Err(e) => e } }
fn m3() -> i64 { match mkres(3) { Result.Ok(G3 { v, .. }) => { println(v); v.len() }, Result.Err(e) => e } }
fn m4() -> i64 { if let Result.Ok(G3 { w, .. }) = mkres(4) { println(w); w.len() } else { 0 } }
fn m5() -> i64 { let o = Option.Some(G3 { v: ms(5), w: ms(55) }); match o { Option.Some(G3 { v, .. }) => { println(v); v.len() }, Option.None => 0 } }
fn m6() -> i64 { match Option.Some(G3 { v: ms(6), w: ms(66) }) { Option.Some(G3 { w, .. }) => { println(w); w.len() }, Option.None => 0 } }
fn main() {
    println(m1()); println(m2()); println(m3()); println(m4()); println(m5()); println(m6());
    println("end")
}"#;
    let want = "a-heap-string-longer-than-sso-1\n31\na-heap-string-longer-than-sso-22\n32\na-heap-string-longer-than-sso-3\n31\na-heap-string-longer-than-sso-40\n32\na-heap-string-longer-than-sso-5\n31\na-heap-string-longer-than-sso-66\n32\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}

/// B-2026-09-30-104 — a whole-bound view of a boxed generic payload handed to a by-value param (`Some(g) => take(g)`): the param is own-by-TRANSFER, so the caller must retract the box's interior walk as every other move of a view does. Freed by name the box freed nothing (a leak, `G[String]`) or the wrong type (`G2[R]`, invalid free); freed at the instantiation without the retraction it is a double free. `takeh` over the non-generic `H2` is the guard.
#[test]
fn e2e_generic_struct_boxed_payload_view_to_by_value_param() {
    let src = r#"
struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct G[T] { v: T, n: i64 }
struct G2[T] { v: T, r: R }
struct H2 { v: R, r: R }
fn mk(k: i64) -> String { f"a-heap-string-longer-than-sso-{k}" }
fn mr(k: i64) -> R { R { id: k, s: mk(k) } }
fn take(g: G[String]) -> i64 { g.v.len() + g.n }
fn takeg[T](g: G[T]) -> i64 { g.n }
fn take2(g: G2[R]) -> i64 { g.v.id + g.r.id }
fn takeh(g: H2) -> i64 { g.v.id + g.r.id }
fn taker(g: G[R]) -> i64 { g.v.id + g.n }
fn mo(k: i64) -> Option[G[String]] { Option.Some(G { v: mk(k), n: k }) }
fn t1() -> i64 { let o = Option.Some(G { v: mk(1), n: 1 }); match o { Option.Some(g) => take(g), Option.None => 0 } }
fn t2() -> i64 { let o = Option.Some(G2 { v: mr(2), r: mr(22) }); match o { Option.Some(g) => take2(g), Option.None => 0 } }
fn t3() -> i64 { let o = Option.Some(H2 { v: mr(3), r: mr(33) }); match o { Option.Some(g) => takeh(g), Option.None => 0 } }
fn t5() -> i64 { let o = Option.Some(G { v: mr(5), n: 5 }); match o { Option.Some(g) => taker(g), Option.None => 0 } }
fn t6() -> i64 { match mo(6) { Option.Some(g) => take(g), Option.None => 0 } }
fn t7() -> i64 { let o: Result[G[String], i64] = Result.Ok(G { v: mk(7), n: 7 }); match o { Result.Ok(g) => take(g), Result.Err(e) => e } }
fn t8() -> i64 { let o = Option.Some(G { v: mk(8), n: 8 }); match o { Option.Some(g) => takeg(g), Option.None => 0 } }
fn t9() -> i64 { let o = Option.Some(G { v: mk(9), n: 9 }); if let Option.Some(g) = o { take(g) } else { 0 } }
fn t10() -> i64 { let o = Option.Some(G2 { v: mr(10), r: mr(100) }); match o { Option.Some(g) => { let a = take2(g); a }, Option.None => 0 } }
fn main() {
    println(t1()); println(t2()); println(t3()); println(t5()); println(t6());
    println(t7()); println(t8()); println(t9()); println(t10());
    println("end")
}"#;
    let want = "32\nd22\nd2\n24\nd33\nd3\n36\nd5\n10\n37\n38\n8\n40\nd100\nd10\n110\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
