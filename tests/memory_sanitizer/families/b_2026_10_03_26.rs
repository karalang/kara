//! B-2026-10-03-26: a `let` that takes a by-value `Option`/`Result` param through a branch is a view of it on that path

use super::*;

/// B-2026-10-03-26: `let r = if c { a } else { None }` over a by-value `Option[R]` param
/// whose `R` runs a `Drop` body and boxes, with `r` dying in the callee, freed the
/// caller's box inside the callee and the caller freed it again after the call: a
/// segfault at every `-O0` cell that took `a`. The path that did not take `a` lost
/// the param's body instead, because the branch read as an escape and the caller
/// stood its bodies walk down. The branch is now read as the alias `let r = a` is,
/// and `r`'s box, bodies and arm bindings are its own only on the path that built
/// its own value.
#[test]
fn asan_let_bound_branch_view_of_by_value_optres_param_frees_once() {
    assert_clean_asan_run(
        r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct T { id: i64 }
impl Drop for T { fn drop(mut ref self) { println(f"t{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
struct V { s: String }
impl Drop for V { fn drop(mut ref self) { println(f"v{self.s.len()}") } }
fn qv(a: Option[V], c: bool) -> i64 { let r = if c { a } else { Some(V { s: f"fresh-heap-string-longer-than-sso" }) }; match r { Some(x) => println(f"r{x.s.len()}"), None => println("rn") }; 0 }
fn keep(x: R) -> i64 { println(f"k{x.id}"); 0 }
fn qf(a: Option[R], c: bool) -> i64 { let r = if c { a } else { Some(mkr(7)) }; match r { Some(x) => println(f"r{x.id}"), None => println("rn") }; 0 }
fn qm(a: Option[R], k: i64) -> i64 { let r = match k { 0 => a, 1 => None, _ => Some(mkr(8)) }; match r { Some(x) => println(f"r{x.id}"), None => println("rn") }; 0 }
fn qr(a: Option[R], c: bool) -> Option[R] { let r = if c { a } else { None }; r }
fn qt(a: Option[R], c: bool) -> i64 { let r = if c { a } else { None }; match r { Some(x) => keep(x), None => 0 } }
fn qx(a: Result[R, i64], c: bool) -> i64 { let r = if c { a } else { Err(5) }; match r { Ok(x) => println(f"r{x.id}"), Err(e) => println(f"e{e}") }; 0 }
fn qs(a: Option[T], c: bool) -> i64 { let r = if c { a } else { None }; match r { Some(x) => println(f"r{x.id}"), None => println("rn") }; 0 }
fn qb(a: Option[R], c: bool) -> i64 { let r = { if c { a } else { None } }; println(f"m{r.is_some()}"); 0 }
fn ql(a: Option[R], c: bool) -> i64 { let r = if c { a } else { None }; if let Some(x) = r { println(f"l{x.id}") }; 0 }
fn qn(a: Option[R], c: bool) -> i64 { let r = if c { a } else { None }; let q = r; println(f"m{q.is_some()}"); 0 }
fn main() {
  println("-a"); qf(Some(mkr(1)), true); qf(Some(mkr(2)), false);
  println("-b"); qm(Some(mkr(1)), 0); qm(Some(mkr(2)), 1); qm(Some(mkr(3)), 2);
  println("-c"); let o = qr(Some(mkr(1)), true); println(f"o{o.is_some()}"); let p = qr(Some(mkr(2)), false); println(f"p{p.is_some()}");
  println("-d"); qt(Some(mkr(1)), true); qt(Some(mkr(2)), false);
  println("-e"); qx(Ok(mkr(1)), true); qx(Ok(mkr(2)), false);
  println("-f"); qs(Some(T { id: 1 }), true); qs(Some(T { id: 2 }), false);
  println("-g"); qb(Some(mkr(1)), true); qb(Some(mkr(2)), false);
  println("-i"); ql(Some(mkr(1)), true); ql(Some(mkr(2)), false);
  println("-j"); qn(Some(mkr(1)), true); qn(Some(mkr(2)), false);
  println("-k"); qv(Some(V { s: f"caller-heap-string-longer-than-sso-1" }), true); qv(Some(V { s: f"caller-heap-string-longer-than-sso-22" }), false);
  println("end") }
"#,
        &[
            "-a", "r1", "d1", "r7", "d7", "d2", "-b", "r1", "d1", "rn", "d2", "r8", "d8", "d3",
            "-c", "otrue", "d1", "d2", "pfalse", "-d", "k1", "d1", "d2", "-e", "r1", "d1", "e5",
            "d2", "-f", "r1", "t1", "rn", "t2", "-g", "mtrue", "d1", "mfalse", "d2", "-i", "l1",
            "d1", "d2", "-j", "mtrue", "d1", "mfalse", "d2", "-k", "r36", "v36", "r33", "v33",
            "v37", "end",
        ],
        "asan_let_bound_branch_view_of_by_value_optres_param_frees_once",
    );
}
