//! B-2026-09-19-50 -- a declared `Array[E, N]` enum payload whose element is
//! a user enum runs the element's payload `Drop` bodies on every backend.

use super::*;

/// B-2026-09-19-50 — a declared `Array[Mono, N]` payload over a user enum `Mono { P(R), Q }`
/// runs the elements' payload `Drop` bodies at every position: let-bound,
/// discarded, moved, a match arm, a fresh-temp argument, a struct field, a
/// wider array, a two-field variant, beside the struct-element control.
///
/// Before: every compiled surface ran them and `--interp` ran none, except the
/// struct field, which ran none anywhere.
#[test]
fn test_array_of_enums_payload_positions() {
    let out = run(r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
enum Mono { P(R), Q }
enum Ha { P(Array[Mono, 1]), Q }
enum Hb { P(Array[Mono, 2]), Q }
enum Hc { P(Array[Mono, 1], i64), Q }
enum Hs { P(Array[R, 2]), Q }
struct Ga { h: Ha }
fn eata(h: Ha) -> i64 { return 1 }
fn eatb(h: Hb) -> i64 { return 1 }
fn r(i: i64) -> R { R { id: i, s: f"ssssssss{i}" } }
fn main() {
    println("let"); { let h = Ha.P([Mono.P(r(1))]); println("  x") }
    println("disc"); { let _ = Ha.P([Mono.P(r(2))]); println("  x") }
    println("move"); { let h = Ha.P([Mono.P(r(3))]); let h2 = h; println("  x") }
    println("arm"); { let h = Ha.P([Mono.P(r(4))]); match h { Ha.P(a) => { println("  in") } Ha.Q => {} } }
    println("arg"); { let n = eata(Ha.P([Mono.P(r(5))])); println(f"  {n}") }
    println("field"); { let g = Ga { h: Ha.P([Mono.P(r(6))]) }; println("  x") }
    println("wlet"); { let h = Hb.P([Mono.P(r(7)), Mono.Q]); println("  x") }
    println("warg"); { let n = eatb(Hb.P([Mono.P(r(8)), Mono.P(r(9))])); println(f"  {n}") }
    println("mlet"); { let h = Hc.P([Mono.P(r(10))], 3); println("  x") }
    println("slet"); { let h = Hs.P([r(11), r(12)]); println("  x") }
    println("end")
}"#);
    assert_eq!(out, "let\n  dR1\n  x\ndisc\n  dR2\n  x\nmove\n  dR3\n  x\narm\n  in\n  dR4\narg\n  dR5\n  1\nfield\n  dR6\n  x\nwlet\n  dR7\n  x\nwarg\n  dR8\n  dR9\n  1\nmlet\n  dR10\n  x\nslet\n  dR11\n  dR12\n  x\nend\n", "got:\n{out}");
}

/// B-2026-09-19-50 — the same payload as a bare statement, a unit element, a function
/// result, arms that pass, index or `if let` the binding, held in a `Vec`, an
/// `Option`, a tuple and a nested struct field, an element enum with its own
/// `Drop`, the generic spelling, and a field moved out.
#[test]
fn test_array_of_enums_payload_shapes() {
    let out = run(r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
enum Mono { P(R), Q }
enum Md { P(R), Q }
impl Drop for Md { fn drop(mut ref self) { println("  dMd") } }
enum Ha { P(Array[Mono, 1]), Q }
enum Hd { P(Array[Md, 1]), Q }
enum G[T] { X(T), Y }
struct Ga { h: Ha }
struct Gb { g: Ga, k: i64 }
fn r(i: i64) -> R { R { id: i, s: f"ssssssss{i}" } }
fn mk(i: i64) -> Ha { Ha.P([Mono.P(r(i))]) }
fn taka(a: Array[Mono, 1]) -> i64 { 1 }
fn main() {
    println("stmt"); { Ha.P([Mono.P(r(1))]); println("  x") }
    println("unit"); { let h = Ha.P([Mono.Q]); println("  x") }
    println("ret"); { let h = mk(2); println("  x") }
    println("armpass"); { let h = Ha.P([Mono.P(r(4))]); match h { Ha.P(a) => { let n = taka(a); println(f"  n{n}") } Ha.Q => {} } }
    println("armelem"); { let h = Ha.P([Mono.P(r(5))]); match h { Ha.P(a) => { match a[0] { Mono.P(x) => println(f"  in {x.id}"), Mono.Q => {} } } Ha.Q => {} } }
    println("iflet"); { let h = Ha.P([Mono.P(r(6))]); if let Ha.P(a) = h { println("  in") } }
    println("vec"); { let mut v: Vec[Ha] = []; v.push(Ha.P([Mono.P(r(7))])); println("  x") }
    println("opt"); { let o = Option.Some(Ha.P([Mono.P(r(8))])); println("  x") }
    println("tup"); { let t = (Ha.P([Mono.P(r(9))]), 1); println("  x") }
    println("nested"); { let b = Gb { g: Ga { h: Ha.P([Mono.P(r(10))]) }, k: 1 }; println("  x") }
    println("owndrop"); { let h = Hd.P([Md.P(r(11))]); println("  x") }
    println("generic"); { let g = G.X([Mono.P(r(13))]); println("  x") }
    println("fieldmv"); { let g = Ga { h: Ha.P([Mono.P(r(14))]) }; let h = g.h; println("  x") }
    println("end")
}"#);
    assert_eq!(out, "stmt\n  dR1\n  x\nunit\n  x\nret\n  dR2\n  x\narmpass\n  n1\n  dR4\narmelem\n  in 5\n  dR5\niflet\n  in\n  dR6\nvec\n  dR7\n  x\nopt\n  dR8\n  x\ntup\n  dR9\n  x\nnested\n  dR10\n  x\nowndrop\n  dMd\n  dR11\n  x\ngeneric\n  dR13\n  x\nfieldmv\n  dR14\n  x\nend\n", "got:\n{out}");
}
