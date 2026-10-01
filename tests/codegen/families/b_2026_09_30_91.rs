//! B-2026-09-30-91 / B-2026-09-30-78 — a GENERIC struct (`G[R]`) in an `Option`/`Result`
//! payload was asked about by its declaration, where `v: T` names no type. So
//! codegen left the source's body walk armed beside the arm's own leaf (`R`'s
//! body ran twice for `Ok(G { v, n })`), and the interpreter never armed the
//! walk at all (no body unless the struct was bound whole). Both now read the
//! fields at the instantiation.

use super::*;

/// B-2026-09-30-91 — full, wildcard and `if let` destructures on both `Result` sides and `Option`, a rebound leaf, an unbound payload, a nested `Vec[R]`, and a whole binding.
#[test]
fn e2e_generic_struct_optres_payload_runs_drop_body_once() {
    let src = r#"
struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct G[T] { v: T, n: i64 }
fn mk(k: i64) -> R { R { id: k, s: f"a-heap-string-longer-than-sso-{k}" } }
fn c1() -> i64 { let o: Result[G[R], i64] = Result.Ok(G { v: mk(1), n: 1 }); match o { Result.Ok(G { v, n }) => v.id + n, Result.Err(e) => e } }
fn c2() -> i64 { let o: Result[i64, G[R]] = Result.Err(G { v: mk(2), n: 1 }); match o { Result.Ok(k) => k, Result.Err(G { v, n }) => v.id + n } }
fn c3() -> i64 { let o = Option.Some(G { v: mk(3), n: 1 }); match o { Option.Some(G { v, n }) => v.id + n, Option.None => 0 } }
fn c4() -> i64 { let o = Option.Some(G { v: mk(4), n: 1 }); match o { Option.Some(G { v: _, n }) => n, Option.None => 0 } }
fn c5() -> i64 { let o = Option.Some(G { v: mk(5), n: 1 }); 5 }
fn c6() -> i64 { let o: Result[G[R], i64] = Result.Ok(G { v: mk(6), n: 1 }); if let Result.Ok(G { v, n }) = o { v.id + n } else { 0 } }
fn c7() -> i64 { let o: Option[G[R]] = Option.Some(G { v: mk(7), n: 1 }); match o { Option.Some(G { v, n }) => { let w = v; w.id + n }, Option.None => 0 } }
fn c8() -> i64 { let o: Result[G[Vec[R]], i64] = Result.Ok(G { v: [mk(8), mk(88)], n: 1 }); 8 }
fn c9() -> i64 { let o: Result[G[R], i64] = Result.Ok(G { v: mk(9), n: 1 }); match o { Result.Ok(g) => g.n, Result.Err(e) => e } }
fn main() {
    println(c1()); println(c2()); println(c3()); println(c4()); println(c5());
    println(c6()); println(c7()); println(c8()); println(c9());
    println("end")
}"#;
    let want = "d1\n2\nd2\n3\nd3\n4\nd4\n1\nd5\n5\nd6\n7\nd7\n8\nd8\nd88\n8\nd9\n1\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
