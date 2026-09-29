//! B-2026-09-29-40: `let t = t.unwrap()` that rebinds a by-value param's own name.

use super::*;

/// B-2026-09-29-40 — `let t = t.unwrap()` (and `expect` / `unwrap_err`) on a
/// by-value `Option` / `Result` param lowers to the `match` both backends
/// model, so the payload's body runs once. Before, the rebind kept the method
/// spelling: `--interp` ran the body twice and compiled double-freed. The
/// same cells pin the two neighbours the lowering routes through: a take that
/// rebinds the name after a take on another path (`mr`, `mp`), and a
/// conditionally returned param shadowed at the body's top level (`sq`).
#[test]
fn asan_unwrap_that_rebinds_the_param_name_runs_the_payload_body_once() {
    assert_clean_asan_run(
        r#"struct S { id: i64, s: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mks(i: i64) -> S { return S { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn ua(t: Option[S]) -> i64 { let t = t.unwrap(); println(f"ua{t.id}"); return 1 }
fn ub(t: Option[S], c: bool) -> i64 { if c { let t = t.unwrap(); println(f"ub{t.id}") }; return 2 }
fn uc(t: Option[S]) -> S { let t = t.expect("boom"); println(f"uc{t.id}"); return t }
fn ro(t: Result[S, S]) -> i64 { let t = t.unwrap(); println(f"ro{t.id}"); return 3 }
fn re(t: Result[S, S]) -> i64 { let t = t.unwrap_err(); println(f"re{t.id}"); return 4 }
fn ug(t: Option[S], c: bool) -> i64 { if c { let t = t.unwrap(); println(f"then{t.id}"); return 5 } else { let t = t.unwrap(); println(f"else{t.id}"); return 6 } }
fn ur(t: Option[S], c: bool) -> S { if c { let t = t.unwrap(); println(f"keep{t.id}"); return t }; let t = t.unwrap(); println(f"tail{t.id}"); return t }
fn mr(t: Option[S], c: bool) -> S { if c { let u = match t { Option.Some(v) => v, n => n.unwrap() }; return u }; let t = match t { Option.Some(v) => v, n => n.unwrap() }; println(f"mr{t.id}"); return t }
fn mp(t: Option[S], c: bool) -> i64 { if c { let t = match t { Option.Some(v) => v, n => n.unwrap() }; return t.id }; let t = match t { Option.Some(v) => v, n => n.unwrap() }; println(f"mp{t.id}"); return 7 }
fn sq(s: S, c: bool) -> Option[S] { if c { return Option.Some(s) }; let s = mks(51); println(f"sq{s.id}"); return Option.None }
fn main() {
  let a = ua(Option.Some(mks(1)));
  let b = ub(Option.Some(mks(2)), false) + ub(Option.Some(mks(3)), true);
  let c = uc(Option.Some(mks(4))); println(f"got{c.id}");
  let d = ro(Result.Ok(mks(5))) + re(Result.Err(mks(6)));
  let g = ug(Option.Some(mks(7)), true) + ug(Option.Some(mks(8)), false);
  let r1 = ur(Option.Some(mks(9)), true); let r2 = ur(Option.Some(mks(10)), false); println(f"ur{r1.id}{r2.id}");
  let m1 = mr(Option.Some(mks(11)), true); let m2 = mr(Option.Some(mks(12)), false); println(f"mr{m1.id}{m2.id}");
  let p = mp(Option.Some(mks(13)), true) + mp(Option.Some(mks(14)), false);
  let x = sq(mks(15), false); let y = sq(mks(16), true); println("sq done");
  println(f"sum{a + b + d + g + p}");
  println("end")
}
"#,
        &[
            "ua1", "dS1", "dS2", "ub3", "dS3", "uc4", "got4", "dS4", "ro5", "dS5", "re6", "dS6",
            "then7", "dS7", "else8", "dS8", "keep9", "tail10", "ur910", "dS10", "dS9", "mr12",
            "mr1112", "dS12", "dS11", "dS13", "mp14", "dS14", "sq51", "dS51", "dS15", "dS16",
            "sq done", "sum43", "end",
        ],
        "asan_unwrap_that_rebinds_the_param_name_runs_the_payload_body_once",
    );
}
