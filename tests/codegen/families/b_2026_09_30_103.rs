//! B-2026-09-30-103 — a destructure of an `Option`/`Result` STRUCT payload
//! that binds only some of its `Drop`-bearing fields. A leaf the arm only
//! reads is a VIEW of the husk, which runs every field's body in reverse
//! field order (design.md § Match Arm Binding Modes, § Field drop order) --
//! the order both backends already gave a user enum's payload. A leaf the arm
//! takes owns its field, and the fields it leaves stay with the husk. The
//! interpreter stood the whole payload walk down for any such arm (losing the
//! unbound fields' bodies, and running a read-only leaf at the arm's end);
//! codegen zeroed the bodies flag for a taking arm and for a `Result` whose
//! `Err` arm hands back a scalar.

use super::*;

/// B-2026-09-30-103 — read-only leaves (`m1`..`m4`, `m7`, `y8`, `y9`, `yb`) keep their field with the husk; taken leaves (`m6`, `y6`, `y7`) leave the rest with it; a `Result` with a scalar `Err` arm (`m5`, `yc`) is read-only too.
#[test]
fn e2e_optres_struct_payload_partial_destructure_bodies() {
    let src = r#"
struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct H2 { v: R, r: R, n: i64 }
struct G2[T] { v: T, r: R }
struct In { p: R, q: R }
struct H3 { i: In, k: R }
fn mk(k: i64) -> R { R { id: k, s: f"a-heap-string-longer-than-sso-{k}" } }
fn h(k: i64) -> H2 { H2 { v: mk(k), r: mk(k * 10 + k), n: 1 } }
fn m1() -> i64 { let o = Option.Some(h(1)); match o { Option.Some(H2 { v, .. }) => v.id, Option.None => 0 } }
fn m2() -> i64 { let o = Option.Some(h(2)); match o { Option.Some(H2 { v, r, .. }) => v.id + r.id, Option.None => 0 } }
fn m3() -> i64 { let o = Option.Some(h(3)); match o { Option.Some(H2 { v: _, r, n }) => r.id + n, Option.None => 0 } }
fn m4() -> i64 { let o = Option.Some(h(4)); if let Option.Some(H2 { v, .. }) = o { println(v.id) } 0 }
fn m5() -> i64 { let o: Result[H2, i64] = Result.Ok(h(5)); match o { Result.Ok(H2 { r, .. }) => r.id, Result.Err(e) => e } }
fn m6() -> i64 { let o = Option.Some(h(6)); match o { Option.Some(H2 { v, .. }) => { let w = v; w.id }, Option.None => 0 } }
fn m7() -> i64 { let o = Option.Some(G2 { v: mk(7), r: mk(77) }); match o { Option.Some(G2 { v, .. }) => v.id, Option.None => 0 } }
fn y6() -> i64 { let o: Result[H2, i64] = Result.Ok(h(8)); match o { Result.Ok(H2 { v, .. }) => { let w = v; w.id }, Result.Err(e) => e } }
fn y7() -> i64 { let o = Option.Some(h(9)); if let Option.Some(H2 { v, .. }) = o { let w = v; println(w.id) } 0 }
fn y8() -> i64 { let o = Option.Some(H3 { i: In { p: mk(12), q: mk(13) }, k: mk(14) }); match o { Option.Some(H3 { i: In { p, .. }, .. }) => p.id, Option.None => 0 } }
fn y9() -> i64 { let o = Option.Some(h(15)); match o { Option.Some(H2 { v, n, .. }) if n > 0 => v.id, Option.Some(H2 { r, .. }) => r.id, Option.None => 0 } }
fn yb() -> i64 { let o = Option.Some(h(16)); match o { Option.Some(H2 { v, n, .. }) => { println(v.id); n }, Option.None => 0 } }
fn yc() -> i64 { let o: Result[H2, R] = Result.Ok(h(17)); match o { Result.Ok(H2 { v, .. }) => v.id, Result.Err(e) => e.id } }
fn main() {
    println(m1()); println(m2()); println(m3()); println(m4()); println(m5()); println(m6()); println(m7());
    println(y6()); println(y7()); println(y8()); println(y9()); println(yb()); println(yc());
    println("end")
}"#;
    let want = "d11\nd1\n1\nd22\nd2\n24\nd33\nd3\n34\n4\nd44\nd4\n0\nd55\nd5\n55\nd6\nd66\n6\nd77\nd7\n7\nd8\nd88\n8\n9\nd9\nd99\n0\nd14\nd13\nd12\n12\nd165\nd15\n15\n16\nd176\nd16\n1\nd187\nd17\n17\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
