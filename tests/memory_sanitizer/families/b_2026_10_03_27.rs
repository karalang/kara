//! B-2026-10-03-27: a `let` that takes one of two by-value `Option`/`Result` params through a branch is a view of each

use super::*;

/// B-2026-10-03-27: `let r = if c { a } else { b }` over two by-value `Option[R]` params
/// whose `R` runs a `Drop` body, with `r` dying in the callee, ran NEITHER param's
/// body compiled (`mtrue mtrue`, where `--interp` printed `mtrue d2 d1 mtrue d4 d3`):
/// the escape walk let a branch `let` alias one param only, so both read as
/// escaping and the caller stood both bodies down while the callee's `r` was a
/// view on every path. The binding now aliases every param its tails name.
#[test]
fn asan_let_bound_branch_view_of_two_by_value_optres_params_runs_both_bodies() {
    assert_clean_asan_run(
        r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn keep(x: R) -> i64 { println(f"k{x.id}"); 0 }
fn qu(a: Option[R], b: Option[R], c: bool) -> i64 { let r = if c { a } else { b }; println(f"m{r.is_some()}"); 0 }
fn qm(a: Option[R], b: Option[R], c: bool) -> i64 { let r = if c { a } else { b }; match r { Some(x) => println(f"r{x.id}"), None => println("rn") }; 0 }
fn qr(a: Option[R], b: Option[R], c: bool) -> Option[R] { let r = if c { a } else { b }; r }
fn qt(a: Option[R], b: Option[R], c: bool) -> i64 { let r = if c { a } else { b }; match r { Some(x) => keep(x), None => 0 } }
fn q3(a: Option[R], b: Option[R], k: i64) -> i64 { let r = match k { 0 => a, 1 => b, _ => Some(mkr(9)) }; match r { Some(x) => println(f"r{x.id}"), None => println("rn") }; 0 }
fn qn(a: Option[R], b: Option[R], c: bool) -> i64 { let r = if c { a } else { b }; let q = r; println(f"m{q.is_some()}"); 0 }
fn ql(a: Option[R], b: Option[R], c: bool) -> i64 { let r = if c { a } else { b }; if let Some(x) = r { println(f"l{x.id}") }; 0 }
fn qx(a: Result[R, i64], b: Result[R, i64], c: bool) -> i64 { let r = if c { a } else { b }; match r { Ok(x) => println(f"r{x.id}"), Err(e) => println(f"e{e}") }; 0 }
fn main() {
  println("-h"); qu(Some(mkr(1)), Some(mkr(2)), true); qu(Some(mkr(3)), Some(mkr(4)), false);
  println("-m"); qm(Some(mkr(1)), Some(mkr(2)), true); qm(Some(mkr(3)), None, false);
  println("-r"); let o = qr(Some(mkr(1)), Some(mkr(2)), true); println(f"o{o.is_some()}"); let p = qr(Some(mkr(3)), Some(mkr(4)), false); println(f"p{p.is_some()}");
  println("-t"); qt(Some(mkr(1)), Some(mkr(2)), true); qt(Some(mkr(3)), Some(mkr(4)), false);
  println("-3"); q3(Some(mkr(1)), Some(mkr(2)), 0); q3(Some(mkr(3)), Some(mkr(4)), 1); q3(Some(mkr(5)), Some(mkr(6)), 2);
  println("-n"); qn(Some(mkr(1)), Some(mkr(2)), true); qn(Some(mkr(3)), Some(mkr(4)), false);
  println("-l"); ql(Some(mkr(1)), Some(mkr(2)), true); ql(Some(mkr(3)), Some(mkr(4)), false);
  println("-x"); qx(Ok(mkr(1)), Ok(mkr(2)), true); qx(Ok(mkr(3)), Err(7), false);
  println("end") }
"#,
        &[
            "-h", "mtrue", "d2", "d1", "mtrue", "d4", "d3", "-m", "r1", "d2", "d1", "rn", "d3",
            "-r", "d2", "otrue", "d1", "d3", "ptrue", "d4", "-t", "k1", "d2", "d1", "k4", "d4",
            "d3", "-3", "r1", "d2", "d1", "r4", "d4", "d3", "r9", "d9", "d6", "d5", "-n", "mtrue",
            "d2", "d1", "mtrue", "d4", "d3", "-l", "l1", "d2", "d1", "l4", "d4", "d3", "-x", "r1",
            "d2", "d1", "e7", "d3", "end",
        ],
        "asan_let_bound_branch_view_of_two_by_value_optres_params_runs_both_bodies",
    );
}
