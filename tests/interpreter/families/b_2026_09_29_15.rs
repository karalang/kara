//! B-2026-09-29-15: a payload taken from a by-value param inside a branch runs its body on every path.

use super::*;

/// B-2026-09-29-15 — a `match` / `if let` (or an `unwrap`, lowered to one)
/// that takes a by-value `Option` / `Result` / user-enum parameter's payload
/// from inside a branch runs the payload's body once on EVERY path. Before,
/// the caller stood down for the handed-out variant and the path that never
/// reached the `match` ran no body on any surface.
#[test]
fn interp_branch_take_of_a_by_value_param_payload_runs_its_body_on_every_path() {
    let out = run(r#"struct S { id: i64, s: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mks(i: i64) -> S { return S { id: i, s: f"heap-string-longer-than-sso-{i}" } }
enum G { A(S), B(i64) }
struct H { k: i64 }
impl H {
    fn mp(ref self, t: Option[S], c: bool) -> i64 { if c { let y = match t { Option.Some(v) => v, Option.None => mks(0) }; println(f"m{y.id}") }; return self.k }
    fn tk(t: Option[S], c: bool) -> i64 { if c { let y = t.unwrap(); println(f"a{y.id}") }; return 1 }
}
fn cp(t: Option[S], c: bool) -> i64 { if c { let y = match t { Option.Some(v) => v, Option.None => panic("n") }; println(f"p{y.id}") }; return 1 }
fn cg(t: G, c: bool) -> i64 { if c { let y = match t { G.A(v) => v, G.B(n) => mks(n) }; println(f"g{y.id}") }; return 1 }
fn cl(t: Option[S], c: bool) -> i64 { if c { let y = if let Option.Some(v) = t { v } else { mks(0) }; println(f"l{y.id}") }; return 1 }
fn cu(t: Option[S], c: bool) -> i64 { if c { let y = t.unwrap(); println(f"u{y.id}") }; return 1 }
fn cr(t: Result[S, i64], c: bool) -> i64 { if c { let y = match t { Result.Ok(v) => v, Result.Err(e) => mks(e) }; println(f"r{y.id}") } else { println("rn") }; return 1 }
fn main() {
    let a = cp(Option.Some(mks(1)), false) + cp(Option.Some(mks(2)), true);
    println(f"a{a}");
    let o = Option.Some(mks(3));
    println(f"b{cp(o, false)}");
    let g = cg(G.A(mks(4)), false) + cg(G.A(mks(5)), true) + cg(G.B(6), false);
    println(f"g{g}");
    let l = cl(Option.Some(mks(7)), false) + cl(Option.Some(mks(8)), true);
    println(f"l{l}");
    let u = cu(Option.Some(mks(9)), false) + cu(Option.Some(mks(10)), true);
    println(f"u{u}");
    let r = cr(Result.Ok(mks(11)), false) + cr(Result.Ok(mks(12)), true) + cr(Result.Err(13), true);
    println(f"r{r}");
    let h = H { k: 7 };
    let n = Option.Some(mks(14));
    let m = h.mp(n, false) + h.mp(Option.Some(mks(15)), false) + h.mp(Option.Some(mks(16)), true);
    println(f"m{m}");
    let t = H.tk(Option.Some(mks(17)), false) + H.tk(Option.Some(mks(18)), true);
    println(f"t{t}");
    println("end")
}
"#);
    assert_eq!(
        out,
        "dS1\np2\ndS2\na2\ndS3\nb1\ndS4\ng5\ndS5\ng3\ndS7\nl8\ndS8\nl2\ndS9\nu10\ndS10\nu2\nrn\ndS11\nr12\ndS12\nr13\ndS13\nr3\ndS14\ndS15\nm16\ndS16\nm21\ndS17\na18\ndS18\nt2\nend\n"
    );
}
