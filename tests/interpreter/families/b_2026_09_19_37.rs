//! B-2026-09-19-37 -- a local binding shadows a unit enum variant of the
//! same name.

use super::*;

/// B-2026-09-19-37 — a local binding whose name is also a unit variant's
/// (`let Uc = 7`) shadows the variant: plain and `shared` enums, annotated,
/// tuple-destructured, `String` and struct values, then the variant itself
/// once the local's scope has closed.
///
/// Before: `--interp` bound nothing, so the later read built the VARIANT
/// (`nUc`, plus a `dU` body no compiled surface ran) and `Pc + 1` died with a
/// runtime type error. The compiled surfaces were right.
#[test]
fn test_local_binding_shadows_unit_variant_name() {
    let out = run(r#"shared enum U { Ua, Ub, Uc }
impl Drop for U { fn drop(mut ref self) { println(f"  dU") } }
enum Plain { Pa, Pb, Pc }
struct P2 { x: i64 }
fn show(p: Plain) -> String { match p { Plain.Pa => "a", Plain.Pb => "b", Plain.Pc => "c" } }
fn main() {
    println("shadow-shared"); { let Uc = 7; let s = Uc; println(f"  n{s}") } println("  out")
    println("shadow-plain"); { let Pc = 9; let s = Pc; println(f"  n{s}") } println("  out")
    println("annotated"); { let Pc: i64 = 4; println(f"  n{Pc}") }
    println("tuple"); { let (Pc, q) = (5, 6); println(f"  n{Pc} {q}") }
    println("arith"); { let Pc = 9; let t = Pc + 1; println(f"  n{t}") }
    println("string"); { let Uc = "hi"; println(f"  n{Uc.len()}") }
    println("struct"); { let Uc = P2 { x: 3 }; println(f"  n{Uc.x}") }
    println("real-variant"); { let v = Pc; println(f"  {show(v)}") }
    println("after-scope"); println(f"  {show(Pc)}")
    println("end")
}"#);
    assert_eq!(out, "shadow-shared\n  n7\n  out\nshadow-plain\n  n9\n  out\nannotated\n  n4\ntuple\n  n5 6\narith\n  n10\nstring\n  n2\nstruct\n  n3\nreal-variant\n  c\nafter-scope\n  c\nend\n", "got:\n{out}");
}
