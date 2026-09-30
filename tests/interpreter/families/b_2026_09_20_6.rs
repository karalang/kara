//! B-2026-09-20-6 -- a projection that starts at a FIELD of a by-value
//! `Option` / `Result` struct payload and crosses a level (`w.p.1`, `w.p.s`).

use super::*;

/// B-2026-09-20-6 — a callee returns a part of its by-value `Option`/`Result`
/// param's struct payload that sits below the payload's top level. The mask
/// both call routes build for a struct payload was a flat set of field
/// indices, and a path that crossed a level answered "cannot say", so the
/// callee's arm walk ran every body and the caller's result ran the escapee's
/// again (`dR31 dR32 got:32 dR32` for a fresh temp) or the sibling ran nowhere
/// (`got:92 dR92` once the payload boxes). The mask is now a tree, so the
/// leaf is masked and the rest of the payload stays with the caller.
///
/// Cells: a one-hop control, field-then-tuple and field-then-field at the
/// inline and the boxed width, each as a fresh temp and a named local, a
/// `Result` head (both arms), a scalar-leaf copy read, and the `None` arm.
/// Byte-identical to the codegen twin
/// `e2e_struct_rooted_payload_projection_runs_each_body_once`.
#[test]
fn test_struct_rooted_payload_projection_runs_each_body_once() {
    let out = run(r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
struct P { r: R, s: R }
struct W { p: (R, R), n: i64 }
struct W2 { p: P, n: i64 }
struct Wb { p: (R, R), n: i64, pad: i64 }
struct W2b { p: P, n: i64, pad: i64 }
struct W1 { r: R, s: R }

fn one(o: Option[W1]) -> R { match o { Option.Some(w) => { return w.s; } Option.None => { return R { id: 0 }; } } }
fn v3(o: Option[W]) -> R { match o { Option.Some(w) => { return w.p.1; } Option.None => { return R { id: 0 }; } } }
fn v4(o: Option[W2]) -> R { match o { Option.Some(w) => { return w.p.s; } Option.None => { return R { id: 0 }; } } }
fn v9(o: Option[Wb]) -> R { match o { Option.Some(w) => { return w.p.1; } Option.None => { return R { id: 0 }; } } }
fn v10(o: Option[W2b]) -> R { match o { Option.Some(w) => { return w.p.s; } Option.None => { return R { id: 0 }; } } }
fn r3(o: Result[W, i64]) -> R { match o { Result.Ok(w) => { return w.p.1; } Result.Err(e) => { return R { id: e }; } } }
fn peek(o: Option[W]) -> i64 { match o { Option.Some(w) => { return w.p.1.id; } Option.None => { return 0; } } }
fn main() {
    println("one"); { let a = Option.Some(W1 { r: R { id: 1 }, s: R { id: 2 } }); let g = one(a); println(f"  got:{g.id}") }
    println("v3 temp"); { let g = v3(Option.Some(W { p: (R { id: 31 }, R { id: 32 }), n: 1 })); println(f"  got:{g.id}") }
    println("v3 named"); { let a = Option.Some(W { p: (R { id: 33 }, R { id: 34 }), n: 1 }); let g = v3(a); println(f"  got:{g.id}") }
    println("v4 temp"); { let g = v4(Option.Some(W2 { p: P { r: R { id: 41 }, s: R { id: 42 } }, n: 1 })); println(f"  got:{g.id}") }
    println("v4 named"); { let a = Option.Some(W2 { p: P { r: R { id: 43 }, s: R { id: 44 } }, n: 1 }); let g = v4(a); println(f"  got:{g.id}") }
    println("v9 temp"); { let g = v9(Option.Some(Wb { p: (R { id: 91 }, R { id: 92 }), n: 1, pad: 0 })); println(f"  got:{g.id}") }
    println("v9 named"); { let a = Option.Some(Wb { p: (R { id: 93 }, R { id: 94 }), n: 1, pad: 0 }); let g = v9(a); println(f"  got:{g.id}") }
    println("v10 temp"); { let g = v10(Option.Some(W2b { p: P { r: R { id: 101 }, s: R { id: 102 } }, n: 1, pad: 0 })); println(f"  got:{g.id}") }
    println("v10 named"); { let a = Option.Some(W2b { p: P { r: R { id: 103 }, s: R { id: 104 } }, n: 1, pad: 0 }); let g = v10(a); println(f"  got:{g.id}") }
    println("r3 temp"); { let g = r3(Result.Ok(W { p: (R { id: 61 }, R { id: 62 }), n: 1 })); println(f"  got:{g.id}") }
    println("r3 named"); { let a: Result[W, i64] = Result.Ok(W { p: (R { id: 63 }, R { id: 64 }), n: 1 }); let g = r3(a); println(f"  got:{g.id}") }
    println("r3 err"); { let g = r3(Result.Err(7)); println(f"  got:{g.id}") }
    println("peek"); { let a = Option.Some(W { p: (R { id: 71 }, R { id: 72 }), n: 1 }); let n = peek(a); println(f"  got:{n}") }
    println("none"); { let a: Option[W] = Option.None; let g = v3(a); println(f"  got:{g.id}") }
    println("end")
}
"#);
    assert_eq!(out, "one\n  dR1\n  got:2\n  dR2\nv3 temp\n  dR31\n  got:32\n  dR32\nv3 named\n  dR33\n  got:34\n  dR34\nv4 temp\n  dR41\n  got:42\n  dR42\nv4 named\n  dR43\n  got:44\n  dR44\nv9 temp\n  dR91\n  got:92\n  dR92\nv9 named\n  dR93\n  got:94\n  dR94\nv10 temp\n  dR101\n  got:102\n  dR102\nv10 named\n  dR103\n  got:104\n  dR104\nr3 temp\n  dR61\n  got:62\n  dR62\nr3 named\n  dR63\n  got:64\n  dR64\nr3 err\n  got:7\n  dR7\npeek\n  dR71\n  dR72\n  got:72\nnone\n  got:0\n  dR0\nend\n", "got:\n{out}");
}
