//! B-2026-09-29-12: unwrap of a by-value Option/Result param runs its payload's body once.

use super::*;

/// B-2026-09-29-12 — `unwrap` / `expect` / `unwrap_err` on a by-value
/// `Option` / `Result` parameter is lowered to the `match` it stands for, so
/// the payload has one owner: the binding the unwrap produced. Before, the
/// interpreter ran every body twice and a boxed `Option` double-freed compiled.
#[test]
fn e2e_unwrap_of_a_by_value_optres_param_runs_its_body_once() {
    let Some(out) = run_program(
        r#"struct S { id: i64, s: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mks(i: i64) -> S { return S { id: i, s: f"heap-string-longer-than-sso-{i}" } }
struct P { id: i64 }
impl Drop for P { fn drop(mut ref self) { println(f"dP{self.id}") } }
struct W { id: i64, s: String }
fn mkw(i: i64) -> W { return W { id: i, s: f"heap-string-longer-than-sso-{i}" } }
struct H { k: i64 }
impl H {
    fn take(self, t: Option[S]) -> i64 { let x = t.expect("boom"); return x.id + self.k }
    fn back(t: Result[i64, S]) -> S { t.unwrap_err() }
}
fn cu(t: Option[S]) { let x = t.unwrap(); println(f"u{x.id}") }
fn cr(t: Option[S]) -> S { return t.unwrap() }
fn cp(t: Option[P]) { let x = t.unwrap(); println(f"p{x.id}") }
fn ck(t: Result[S, i64]) { let x = t.unwrap(); println(f"k{x.id}") }
fn cw(t: Option[W]) -> i64 { let x = t.unwrap(); return x.id + x.s.len() }
fn main() {
    cu(Option.Some(mks(1)));
    let a = Option.Some(mks(2));
    cu(a);
    let r = cr(Option.Some(mks(3)));
    println(f"r{r.id}");
    cp(Option.Some(P { id: 4 }));
    ck(Result.Ok(mks(5)));
    let h = H { k: 10 };
    println(f"t{h.take(Option.Some(mks(6)))}");
    let b = H.back(Result.Err(mks(7)));
    println(f"b{b.id}");
    println(f"w{cw(Option.Some(mkw(8)))}");
    let w = Option.Some(mkw(9));
    println(f"w{cw(w)}");
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out, "u1\ndS1\nu2\ndS2\nr3\ndS3\np4\ndP4\nk5\ndS5\ndS6\nt16\nb7\ndS7\nw37\nw38\nend\n",
        "got:\n{out}"
    );
}
