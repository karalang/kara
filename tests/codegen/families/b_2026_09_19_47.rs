//! B-2026-09-19-47 -- an `Option` / `Result` enum payload, declared or
//! generic, runs its elements' `Drop` bodies.

use super::*;

/// B-2026-09-19-47 — an `Option` / `Result` payload of a GENERIC enum (`G.X(o)` over
/// `Option[Vec[S1]]`, `Option[Array[S1, 1]]`, `Result[Vec[S1], i64]`,
/// `Option[Option[Vec[S1]]]`, `Option[S1]`) runs the elements' `Drop` bodies,
/// beside the bare `Option[Vec[S1]]` local that always did.
///
/// Before: every container cell was silent on all four surfaces.
#[test]
fn e2e_optres_enum_payload_generic_envelope() {
    let Some(out) = run_program(
        r#"struct S1 { v: i64, s: String }
impl Drop for S1 { fn drop(mut ref self) { println(f"  dS{self.v}") } }
enum G[T] { X(T), Y }
enum Hq { P(Option[Vec[S1]]), Q }
enum Ha { P(Option[Array[S1, 1]]), Q }
enum Hr { P(Result[Vec[S1], i64]), Q }
fn s(v: i64) -> S1 { S1 { v: v, s: f"ssssssss{v}" } }
fn main() {
    println("ctl"); { let mut w: Vec[S1] = []; w.push(s(1)); let o: Option[Vec[S1]] = Option.Some(w); println("  x") }
    println("gvec"); { let mut w: Vec[S1] = []; w.push(s(2)); let o: Option[Vec[S1]] = Option.Some(w); let g = G.X(o); println("  x") }
    println("garr"); { let o: Option[Array[S1, 1]] = Option.Some([s(3)]); let g = G.X(o); println("  x") }
    println("gres"); { let mut w: Vec[S1] = []; w.push(s(4)); let o: Result[Vec[S1], i64] = Result.Ok(w); let g = G.X(o); println("  x") }
    println("gopt2"); { let mut w: Vec[S1] = []; w.push(s(5)); let o: Option[Option[Vec[S1]]] = Option.Some(Option.Some(w)); let g = G.X(o); println("  x") }
    println("gstruct"); { let o: Option[S1] = Option.Some(s(9)); let g = G.X(o); println("  x") }
    println("end")
}"#,
    ) else {
        return;
    };
    assert_eq!(out, "ctl\n  dS1\n  x\ngvec\n  dS2\n  x\ngarr\n  dS3\n  x\ngres\n  dS4\n  x\ngopt2\n  dS5\n  x\ngstruct\n  dS9\n  x\nend\n", "got:\n{out}");
}

/// B-2026-09-19-47 — a DECLARED `Option` / `Result` enum payload (`enum Ho { P(Option[S1]), Q }`
/// and the `Vec`, `Array`, nested, tuple and multi-field spellings) runs its
/// `Drop` bodies let-bound, discarded, returned, as a fresh argument, as a
/// struct field and moved.
///
/// Before: the container spellings were silent on every surface.
#[test]
fn e2e_optres_enum_payload_declared_positions() {
    let Some(out) = run_program(
        r#"struct S1 { v: i64, s: String }
impl Drop for S1 { fn drop(mut ref self) { println(f"  dS{self.v}") } }
enum Hq { P(Option[Vec[S1]]), Q }
enum Ha { P(Option[Array[S1, 1]]), Q }
enum Hr { P(Result[Vec[S1], i64]), Q }
enum Ho { P(Option[S1]), Q }
enum Hoo { P(Option[Option[S1]]), Q }
enum Hot { P(Option[(S1, i64)]), Q }
enum Hm { P(Option[S1], i64), Q }
struct W { h: Ho }
fn s(v: i64) -> S1 { S1 { v: v, s: f"ssssssss{v}" } }
fn mk(v: i64) -> Ho { Ho.P(Option.Some(s(v))) }
fn eat(h: Ho) -> i64 { 1 }
fn main() {
    println("dvec"); { let mut w: Vec[S1] = []; w.push(s(6)); let h = Hq.P(Option.Some(w)); println("  x") }
    println("darr"); { let h = Ha.P(Option.Some([s(7)])); println("  x") }
    println("dres"); { let mut w: Vec[S1] = []; w.push(s(8)); let h = Hr.P(Result.Ok(w)); println("  x") }
    println("dopt"); { let h = Ho.P(Option.Some(s(9))); println("  x") }
    println("dnone"); { let h = Ho.P(Option.None); println("  x") }
    println("dopt2"); { let h = Hoo.P(Option.Some(Option.Some(s(10)))); println("  x") }
    println("dtup"); { let h = Hot.P(Option.Some((s(11), 2))); println("  x") }
    println("dmulti"); { let h = Hm.P(Option.Some(s(12)), 3); println("  x") }
    println("disc"); { let _ = Ho.P(Option.Some(s(13))); println("  x") }
    println("ret"); { let h = mk(14); println("  x") }
    println("arg"); { let n = eat(Ho.P(Option.Some(s(15)))); println(f"  {n}") }
    println("field"); { let w = W { h: Ho.P(Option.Some(s(16))) }; println("  x") }
    println("move"); { let h = Ho.P(Option.Some(s(17))); let h2 = h; println("  x") }
    println("end")
}"#,
    ) else {
        return;
    };
    assert_eq!(out, "dvec\n  dS6\n  x\ndarr\n  dS7\n  x\ndres\n  dS8\n  x\ndopt\n  dS9\n  x\ndnone\n  x\ndopt2\n  dS10\n  x\ndtup\n  dS11\n  x\ndmulti\n  dS12\n  x\ndisc\n  dS13\n  x\nret\n  dS14\n  x\narg\n  dS15\n  1\nfield\n  dS16\n  x\nmove\n  dS17\n  x\nend\n", "got:\n{out}");
}

/// B-2026-09-19-47 — a match arm over a declared or generic `Option` payload runs the body
/// once: read in the arm, ignored with `_`, rebound, destructured to the
/// inner value, moved out of it, through `if let`, and a `Vec` inside.
/// A binding handed BY VALUE to a callee is left out: it loses its body on
/// both backends today, which is its own row.
#[test]
fn e2e_optres_enum_payload_arms() {
    let Some(out) = run_program(
        r#"struct S1 { v: i64, s: String }
impl Drop for S1 { fn drop(mut ref self) { println(f"  dS{self.v}") } }
enum Ho { P(Option[S1]), Q }
enum Hq { P(Option[Vec[S1]]), Q }
enum G[T] { X(T), Y }
fn s(v: i64) -> S1 { S1 { v: v, s: f"ssssssss{v}" } }
fn tako(o: Option[S1]) -> i64 { 1 }
fn main() {
    println("read"); { let h = Ho.P(Option.Some(s(1))); match h { Ho.P(o) => { println(f"  in {o.is_some()}") } Ho.Q => {} } }
    println("unused"); { let h = Ho.P(Option.Some(s(2))); match h { Ho.P(_) => { println("  in") } Ho.Q => {} } }
    println("rebind"); { let h = Ho.P(Option.Some(s(3))); match h { Ho.P(o) => { let u = o; println("  in") } Ho.Q => {} } }
    println("nested"); { let h = Ho.P(Option.Some(s(5))); match h { Ho.P(Option.Some(r)) => { println(f"  r{r.v}") } Ho.P(Option.None) => {} Ho.Q => {} } }
    println("nestedmv"); { let h = Ho.P(Option.Some(s(6))); match h { Ho.P(Option.Some(r)) => { let u = r; println(f"  r{u.v}") } Ho.P(Option.None) => {} Ho.Q => {} } }
    println("iflet"); { let h = Ho.P(Option.Some(s(7))); if let Ho.P(o) = h { println("  in") } }
    println("vread"); { let mut w: Vec[S1] = []; w.push(s(8)); let h = Hq.P(Option.Some(w)); match h { Hq.P(o) => { println("  in") } Hq.Q => {} } }
    println("gread"); { let g = G.X(Option.Some(s(9))); match g { G.X(o) => { println("  in") } G.Y => {} } }
    println("grebind"); { let g = G.X(Option.Some(s(11))); match g { G.X(o) => { let u = o; println("  in") } G.Y => {} } }
    println("end")
}"#,
    ) else {
        return;
    };
    assert_eq!(out, "read\n  in true\n  dS1\nunused\n  in\n  dS2\nrebind\n  dS3\n  in\nnested\n  r5\n  dS5\nnestedmv\n  r6\n  dS6\niflet\n  in\n  dS7\nvread\n  in\n  dS8\ngread\n  in\n  dS9\ngrebind\n  dS11\n  in\nend\n", "got:\n{out}");
}
