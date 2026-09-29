//! B-2026-09-19-45 -- an enum payload holding an array of enums frees
//! its elements, and a match over a named array's element is a clone.

use super::*;

/// B-2026-09-19-45 — a declared `Array[E, N]` enum payload whose element is a
/// user enum, in every position: let-bound, discarded, moved, matched (with a
/// nested `match arr[0]` that binds the element's payload), a fresh-temp
/// argument, a struct field, a two-element and a mixed payload, and a
/// Copy-element array beside them.
///
/// Before: the payload was boxed at construction but the enum's drop fn
/// never freed the box (`Mono`'s declared width read as one word, so the
/// payload stayed classified unboxed): 80 B + 11 B lost per cell.
#[test]
fn e2e_enum_array_of_enum_payload() {
    let Some(out) = run_program(
        r#"struct R { id: i64, s: String }
enum Mono { P(R), Q }
enum Ha { P(Array[Mono, 1]), Q }
enum Hb { P(Array[Mono, 2], i64), Q }
enum Hc { P(Array[Later, 2]), Q }
enum Later { A(String), B }
enum Unit { U1, U2 }
enum Hu { P(Array[Unit, 3]), Q }
struct Ga { h: Ha }
fn eata(h: Ha) -> i64 { return 1 }
fn mk(i: i64) -> R { R { id: i, s: f"ssssssss{i}" } }
fn main() {
    println("let"); { let a: Array[Mono, 1] = [Mono.P(mk(1))]; let h = Ha.P(a); println("  x") }
    println("disc"); { let a: Array[Mono, 1] = [Mono.P(mk(2))]; let _ = Ha.P(a); println("  x") }
    println("move"); { let a: Array[Mono, 1] = [Mono.P(mk(3))]; let h = Ha.P(a); let h2 = h; println("  x") }
    println("arm"); { let a: Array[Mono, 1] = [Mono.P(mk(4))]; let h = Ha.P(a); match h { Ha.P(arr) => { match arr[0] { Mono.P(r) => println(f"  in {r.id}"), Mono.Q => println("  q") } } Ha.Q => {} } }
    println("arg"); { let a: Array[Mono, 1] = [Mono.P(mk(5))]; let n = eata(Ha.P(a)); println(f"  {n}") }
    println("field"); { let a: Array[Mono, 1] = [Mono.P(mk(6))]; let g = Ga { h: Ha.P(a) }; println("  x") }
    println("two"); { let a: Array[Mono, 2] = [Mono.P(mk(7)), Mono.Q]; let h = Hb.P(a, 3); println("  x") }
    println("later"); { let a: Array[Later, 2] = [Later.A(f"llllllll"), Later.B]; let h = Hc.P(a); println("  x") }
    println("unit"); { let a: Array[Unit, 3] = [Unit.U1, Unit.U2, Unit.U1]; let h = Hu.P(a); println("  x") }
    println("end")
}"#,
    ) else {
        return;
    };
    assert_eq!(out, "let\n  x\ndisc\n  x\nmove\n  x\narm\n  in 4\narg\n  1\nfield\n  x\ntwo\n  x\nlater\n  x\nunit\n  x\nend\n", "got:\n{out}");
}

/// B-2026-09-19-45 — `match a[i] { .. }` over a NAMED `Array[E, N]` local
/// whose element is a user enum. The arm that binds the element's payload
/// works on a clone (as the `Vec` spelling always did), and the payload's
/// `Drop` body runs once per element.
///
/// Before: the arm moved the payload out of the array's own slot, so the
/// binding and the array freed one buffer (double free at every opt level).
#[test]
fn e2e_enum_array_of_enum_array_local_scrutinee() {
    let Some(out) = run_program(
        r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"  drop {self.id}") } }
enum Mono { P(R), Q }
fn mk(i: i64) -> R { R { id: i, s: f"ssssssss{i}" } }
fn main() {
    { let a: Array[Mono, 1] = [Mono.P(mk(4))]; match a[0] { Mono.P(r) => println(f"  in {r.id}"), Mono.Q => println("  q") }; println("  after"); }
    { let a: Array[Mono, 2] = [Mono.Q, Mono.P(mk(5))]; match a[0] { Mono.P(r) => println(f"  in {r.id}"), Mono.Q => println("  q") }; println("  after"); }
    { let a: Array[Mono, 2] = [Mono.P(mk(6)), Mono.P(mk(7))]; match a[1] { Mono.P(_) => println("  in"), Mono.Q => println("  q") }; println("  after"); }
    println("end")
}"#,
    ) else {
        return;
    };
    assert_eq!(out, "  in 4\n  drop 4\n  after\n  q\n  drop 5\n  after\n  in\n  drop 6\n  drop 7\n  after\nend\n", "got:\n{out}");
}
