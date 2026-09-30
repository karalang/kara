//! B-2026-09-30-86 — the `Option`/`Result` method consumers (`unwrap`,
//! `expect`, `unwrap_err`, `unwrap_or`, `map`, and the rest in `calls.rs`)
//! rebuilt the payload from the first THREE words only. A `Result` payload is
//! up to five words inline, so every field past the third read as nothing:
//! `r.unwrap().n` over `S { v: String, n: i64 }` was 0 at -O0 and garbage at
//! -O2, while `match` on the same value was right.

use super::*;

/// B-2026-09-30-86 — a four-word `String`-bearing payload and a four-word scalar one, through `unwrap` (named and temp receivers), `expect`, `unwrap_err`, `unwrap_or` and `map`.
#[test]
fn e2e_result_method_consumers_rebuild_every_payload_word() {
    let src = r#"
struct S { v: String, n: i64 }
struct W { a: i64, b: i64, c: i64, d: i64 }
fn mk(k: i64) -> String { f"a-heap-string-longer-than-sso-{k}" }
fn mkr(k: i64) -> Result[S, i64] { Result.Ok(S { v: mk(k), n: k }) }
fn mke(k: i64) -> Result[i64, S] { Result.Err(S { v: mk(k), n: k }) }
fn mkw(k: i64) -> Result[W, i64] { Result.Ok(W { a: k, b: k + 1, c: k + 2, d: k + 3 }) }
fn main() {
    let r = mkr(1);
    let g = r.unwrap();
    println(f"{g.n} {g.v.len()}");
    println(mkr(2).unwrap().n);
    let g3 = mkr(3).expect("m");
    println(g3.n);
    let e = mke(4).unwrap_err();
    println(e.n);
    let u = mkr(5).unwrap_or(S { v: mk(0), n: 0 });
    println(u.n);
    let w = mkw(6).unwrap();
    println(f"{w.a} {w.b} {w.c} {w.d}");
    let wd = mkw(7).map(|x| x.d);
    println(wd.unwrap());
    let w8 = mkw(8).unwrap_or(W { a: 0, b: 0, c: 0, d: 0 });
    println(w8.d);
    println("end");
}"#;
    let want = "1 31\n2\n3\n4\n5\n6 7 8 9\n10\n11\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
