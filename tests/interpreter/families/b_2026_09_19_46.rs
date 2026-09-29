//! B-2026-09-19-46 -- a declared tuple enum payload runs its elements'
//! `Drop` bodies.

use super::*;

/// B-2026-09-19-46 — a declared TUPLE enum payload (`enum Ht { P((S1, S1)), Q }`)
/// runs each element's `Drop` body in every position: let-bound, discarded,
/// a bare statement, moved, a fresh-temp argument, a struct field, a match arm
/// that binds the tuple or its elements, a mixed and a three-wide tuple, and
/// the generic spelling `G.X((S1, S1))` beside it.
///
/// Before: the declared spelling ran no element body on any backend, and the
/// generic one ran them compiled but not under `--interp`.
#[test]
fn test_tuple_enum_payload_positions() {
    let out = run(r#"struct S1 { v: i64, s: String }
impl Drop for S1 { fn drop(mut ref self) { println(f"  dS{self.v}") } }
enum Ht { P((S1, S1)), Q }
enum Hm { P((S1, i64)), Q }
enum Hw { P((S1, S1, S1)), Q }
enum G[T] { X(T), Y }
struct Wt { h: Ht }
fn eat(h: Ht) -> i64 { 1 }
fn eatg(g: G[(S1, S1)]) -> i64 { 1 }
fn s(v: i64) -> S1 { S1 { v: v, s: f"ssssssss{v}" } }
fn main() {
    println("let"); { let h = Ht.P((s(1), s(2))); println("  x") }
    println("disc"); { let _ = Ht.P((s(3), s(4))); println("  x") }
    println("stmt"); { Ht.P((s(5), s(6))); println("  x") }
    println("move"); { let h = Ht.P((s(7), s(8))); let h2 = h; println("  x") }
    println("arg"); { let n = eat(Ht.P((s(9), s(10)))); println(f"  {n}") }
    println("field"); { let w = Wt { h: Ht.P((s(11), s(12))) }; println("  x") }
    println("arm"); { let h = Ht.P((s(13), s(14))); match h { Ht.P(t) => println("  in"), Ht.Q => {} } }
    println("armelem"); { let h = Ht.P((s(15), s(16))); match h { Ht.P((a, b)) => println(f"  in {a.v}"), Ht.Q => {} } }
    println("mixed"); { let h = Hm.P((s(17), 5)); println("  x") }
    println("wide"); { let h = Hw.P((s(18), s(19), s(20))); println("  x") }
    println("glet"); { let g = G.X((s(21), s(22))); println("  x") }
    println("gdisc"); { let _ = G.X((s(23), s(24))); println("  x") }
    println("garg"); { let n = eatg(G.X((s(25), s(26)))); println(f"  {n}") }
    println("end")
}"#);
    assert_eq!(out, "let\n  dS1\n  dS2\n  x\ndisc\n  dS3\n  dS4\n  x\nstmt\n  dS5\n  dS6\n  x\nmove\n  dS7\n  dS8\n  x\narg\n  dS9\n  dS10\n  1\nfield\n  dS11\n  dS12\n  x\narm\n  in\n  dS13\n  dS14\narmelem\n  in 15\n  dS15\n  dS16\nmixed\n  dS17\n  x\nwide\n  dS18\n  dS19\n  dS20\n  x\nglet\n  dS21\n  dS22\n  x\ngdisc\n  dS23\n  dS24\n  x\ngarg\n  dS25\n  dS26\n  1\nend\n", "got:\n{out}");
}

/// B-2026-09-19-46 — tuple payload shapes: a nested tuple, a struct-variant
/// field, an enum with its own `Drop`, and the enum held in a `Vec`, an
/// `Option`, a function result and an `if let` binding.
#[test]
fn test_tuple_enum_payload_shapes() {
    let out = run(r#"struct S1 { v: i64, s: String }
impl Drop for S1 { fn drop(mut ref self) { println(f"  dS{self.v}") } }
enum Mono { P(S1), Q }
enum Ht { P((S1, S1)), Q }
enum He { P((Mono, i64)), Q }
enum Hn { P(((S1, i64), S1)), Q }
enum Ho { P((Option[S1], i64)), Q }
enum Hs { P { t: (S1, S1), n: i64 }, Q }
enum Hd { P((S1, S1)), Q }
impl Drop for Hd { fn drop(mut ref self) { println("  dHd") } }
fn takt(t: (S1, S1)) -> i64 { t.0.v }
fn s(v: i64) -> S1 { S1 { v: v, s: f"ssssssss{v}" } }
fn mk(v: i64) -> Ht { Ht.P((s(v), s(v + 1))) }
fn main() {
    println("nested"); { let h = Hn.P(((s(2), 1), s(3))); println("  x") }
    println("structv"); { let h = Hs.P { t: (s(5), s(6)), n: 1 }; println("  x") }
    println("ownbody"); { let h = Hd.P((s(7), s(8))); println("  x") }
    println("vec"); { let v: Vec[Ht] = vec![Ht.P((s(9), s(10)))]; println("  x") }
    println("opt"); { let o = Option.Some(Ht.P((s(11), s(12)))); println("  x") }
    println("ret"); { let h = mk(15); println("  x") }
    println("iflet"); { let h = Ht.P((s(19), s(20))); if let Ht.P(t) = h { println(f"  in {t.1.v}") } }
    println("q"); { let h = Ht.Q; println("  x") }
    println("end")
}"#);
    assert_eq!(out, "nested\n  dS2\n  dS3\n  x\nstructv\n  dS5\n  dS6\n  x\nownbody\n  dHd\n  dS7\n  dS8\n  x\nvec\n  dS9\n  dS10\n  x\nopt\n  dS11\n  dS12\n  x\nret\n  dS15\n  dS16\n  x\niflet\n  in 20\n  dS19\n  dS20\nq\n  x\nend\n", "got:\n{out}");
}

/// B-2026-09-19-46 — a tuple payload whose element is a user enum or an
/// `Option`. Body counts only: both cells leak the payload box on the control
/// tree as well, which is filed separately, so they stay out of the ASAN set.
#[test]
fn test_tuple_enum_payload_enum_and_option_elements() {
    let out = run(r#"struct S1 { v: i64, s: String }
impl Drop for S1 { fn drop(mut ref self) { println(f"  dS{self.v}") } }
enum Mono { P(S1), Q }
enum Ht { P((S1, S1)), Q }
enum He { P((Mono, i64)), Q }
enum Hn { P(((S1, i64), S1)), Q }
enum Ho { P((Option[S1], i64)), Q }
enum Hs { P { t: (S1, S1), n: i64 }, Q }
enum Hd { P((S1, S1)), Q }
impl Drop for Hd { fn drop(mut ref self) { println("  dHd") } }
fn takt(t: (S1, S1)) -> i64 { t.0.v }
fn s(v: i64) -> S1 { S1 { v: v, s: f"ssssssss{v}" } }
fn mk(v: i64) -> Ht { Ht.P((s(v), s(v + 1))) }
fn main() {
    println("enumelem"); { let h = He.P((Mono.P(s(1)), 2)); println("  x") }
    println("optelem"); { let h = Ho.P((Option.Some(s(4)), 1)); println("  x") }
    println("end")
}"#);
    assert_eq!(
        out, "enumelem\n  dS1\n  x\noptelem\n  dS4\n  x\nend\n",
        "got:\n{out}"
    );
}

/// B-2026-09-19-46 — an arm that binds the whole tuple payload and reads it
/// or ignores it runs the elements' bodies once, at the arm's end, as the
/// struct-payload arm beside it does.
#[test]
fn test_tuple_enum_payload_arms() {
    let out = run(r#"struct S1 { v: i64, s: String }
impl Drop for S1 { fn drop(mut ref self) { println(f"  dS{self.v}") } }
enum Ht { P((S1, S1)), Q }
enum Hr { P(S1), Q }
fn s(v: i64) -> S1 { S1 { v: v, s: f"ssssssss{v}" } }
fn main() {
    println("tupkeep"); { let h = Ht.P((s(5), s(6))); match h { Ht.P(t) => { println(f"  n{t.0.v}") } Ht.Q => {} } }
    println("tupunused"); { let h = Ht.P((s(1), s(2))); match h { Ht.P(t) => { println("  in") } Ht.Q => {} } }
    println("rkeep"); { let h = Hr.P(s(3)); match h { Hr.P(t) => { println(f"  n{t.v}") } Hr.Q => {} } }
    println("runused"); { let h = Hr.P(s(4)); match h { Hr.P(t) => { println("  in") } Hr.Q => {} } }
    println("end")
}"#);
    assert_eq!(out, "tupkeep\n  n5\n  dS5\n  dS6\ntupunused\n  in\n  dS1\n  dS2\nrkeep\n  n3\n  dS3\nrunused\n  in\n  dS4\nend\n", "got:\n{out}");
}

/// B-2026-09-19-46 — a tuple payload whose item is an `Array` or a `Vec` of
/// `Drop` values runs each element's body, declared and generic, bound or
/// matched. Codegen's tuple walker descends into a container item; the
/// interpreter's tuple-item walk did not, so this cell pins the two together.
#[test]
fn test_tuple_enum_payload_container_items() {
    let out = run(r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
enum Mr { P((Array[R, 2], i64)), Q }
enum Mv { P((Vec[R], i64)), Q }
enum G[T] { X(T), Y }
fn r(i: i64) -> R { R { id: i, s: f"ssssssssss{i}" } }
fn main() {
    println("arr"); { let m = Mr.P(([r(1), r(2)], 3)); println("  x") }
    println("vec"); { let m = Mv.P(([r(4), r(5)], 3)); println("  x") }
    println("arm"); { let m = Mr.P(([r(6), r(7)], 3)); match m { Mr.P(t) => { println(f"  in {t.1}") } Mr.Q => {} } }
    println("genarr"); { let g = G.X(([r(8), r(9)], 3)); println("  x") }
    println("genvec"); { let g: G[(Vec[R], i64)] = G.X(([r(10)], 3)); println("  x") }
    println("end")
}"#);
    assert_eq!(out, "arr\n  dR1\n  dR2\n  x\nvec\n  dR4\n  dR5\n  x\narm\n  in 3\n  dR6\n  dR7\ngenarr\n  dR8\n  dR9\n  x\ngenvec\n  dR10\n  x\nend\n", "got:\n{out}");
}
