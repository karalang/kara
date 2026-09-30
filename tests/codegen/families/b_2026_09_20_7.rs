//! B-2026-09-20-7 -- several parts of a by-value `Option` / `Result`
//! payload handed back together in a tuple or struct literal.

use super::*;

/// B-2026-09-20-7 — an arm that returns SEVERAL parts of its by-value
/// `Option`/`Result` param's payload in a tuple or struct LITERAL
/// (`Some(t) => return (t.0.0, t.0.1)`). The shared escape predicate
/// (`fn_escaping_param_payload_part_paths`) denoted only a bare projection,
/// so a literal reported no path at all: `--interp` ran every handed-back
/// part's body in the caller's walk and again from the result
/// (`dR81 dR82 got:81,82 dR81 dR82`), and the compiled backends doubled every
/// NAMED-local cell. It now reports each literal element's path, and a root
/// yielded inside a literal (`R { id: e }` in an `Err(e)` arm) reports
/// nothing rather than "whole".
///
/// Cells: every sibling at two hops and at one, one of each level (`mix`),
/// a struct literal, the arm-tail spelling, a scalar leaf beside a moved part,
/// a non-projection element, a boxed struct-rooted payload, a `Result` head
/// with both arms, and the `None` arm, each as a fresh temp and a named local
/// where both exist. Byte-identical to its twin in the other suite.
#[test]
fn e2e_literal_of_payload_parts_handed_back_runs_each_body_once() {
    let Some(out) = run_program(
        r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
struct P { a: R, b: R }
struct Wb { p: (R, R), n: i64, pad: i64, q: i64 }
fn v8(o: Option[((R, R), i64)]) -> (R, R) { match o { Option.Some(t) => { return (t.0.0, t.0.1); } Option.None => { return (R { id: 0 }, R { id: 0 }); } } }
fn h1(o: Option[(R, R)]) -> (R, R) { match o { Option.Some(t) => { return (t.0, t.1); } Option.None => { return (R { id: 0 }, R { id: 0 }); } } }
fn mix(o: Option[((R, R), R)]) -> (R, R) { match o { Option.Some(t) => { return (t.0.0, t.1); } Option.None => { return (R { id: 0 }, R { id: 0 }); } } }
fn sl(o: Option[(R, R)]) -> P { match o { Option.Some(t) => { return P { a: t.1, b: t.0 }; } Option.None => { return P { a: R { id: 0 }, b: R { id: 0 } }; } } }
fn tl(o: Option[((R, R), i64)]) -> (R, R) { match o { Option.Some(t) => (t.0.0, t.0.1), Option.None => (R { id: 0 }, R { id: 0 }) } }
fn sc(o: Option[(R, R)]) -> (i64, R) { match o { Option.Some(t) => { return (t.0.id, t.1); } Option.None => { return (0, R { id: 0 }); } } }
fn nl(o: Option[(R, R)]) -> (R, R) { match o { Option.Some(t) => { return (t.1, R { id: 99 }); } Option.None => { return (R { id: 0 }, R { id: 0 }); } } }
fn bx(o: Option[Wb]) -> (R, R) { match o { Option.Some(w) => { return (w.p.0, w.p.1); } Option.None => { return (R { id: 0 }, R { id: 0 }); } } }
fn rs(o: Result[((R, R), i64), i64]) -> (R, R) { match o { Result.Ok(t) => { return (t.0.0, t.0.1); } Result.Err(e) => { return (R { id: e }, R { id: e }); } } }
fn main() {
    println("v8 temp"); { let g = v8(Option.Some(((R { id: 81 }, R { id: 82 }), 3))); println(f"  got:{g.0.id},{g.1.id}") }
    println("v8 named"); { let a = Option.Some(((R { id: 83 }, R { id: 84 }), 3)); let g = v8(a); println(f"  got:{g.0.id},{g.1.id}") }
    println("h1 temp"); { let g = h1(Option.Some((R { id: 11 }, R { id: 12 }))); println(f"  got:{g.0.id},{g.1.id}") }
    println("h1 named"); { let a = Option.Some((R { id: 13 }, R { id: 14 })); let g = h1(a); println(f"  got:{g.0.id},{g.1.id}") }
    println("mix temp"); { let g = mix(Option.Some(((R { id: 31 }, R { id: 32 }), R { id: 33 }))); println(f"  got:{g.0.id},{g.1.id}") }
    println("mix named"); { let a = Option.Some(((R { id: 34 }, R { id: 35 }), R { id: 36 })); let g = mix(a); println(f"  got:{g.0.id},{g.1.id}") }
    println("sl temp"); { let g = sl(Option.Some((R { id: 41 }, R { id: 42 }))); println(f"  got:{g.a.id},{g.b.id}") }
    println("sl named"); { let a = Option.Some((R { id: 43 }, R { id: 44 })); let g = sl(a); println(f"  got:{g.a.id},{g.b.id}") }
    println("tl temp"); { let g = tl(Option.Some(((R { id: 51 }, R { id: 52 }), 1))); println(f"  got:{g.0.id},{g.1.id}") }
    println("tl named"); { let a = Option.Some(((R { id: 53 }, R { id: 54 }), 1)); let g = tl(a); println(f"  got:{g.0.id},{g.1.id}") }
    println("sc temp"); { let g = sc(Option.Some((R { id: 61 }, R { id: 62 }))); println(f"  got:{g.0},{g.1.id}") }
    println("nl temp"); { let g = nl(Option.Some((R { id: 71 }, R { id: 72 }))); println(f"  got:{g.0.id},{g.1.id}") }
    println("nl named"); { let a = Option.Some((R { id: 73 }, R { id: 74 })); let g = nl(a); println(f"  got:{g.0.id},{g.1.id}") }
    println("bx temp"); { let g = bx(Option.Some(Wb { p: (R { id: 91 }, R { id: 92 }), n: 1, pad: 2, q: 3 })); println(f"  got:{g.0.id},{g.1.id}") }
    println("bx named"); { let a = Option.Some(Wb { p: (R { id: 93 }, R { id: 94 }), n: 1, pad: 2, q: 3 }); let g = bx(a); println(f"  got:{g.0.id},{g.1.id}") }
    println("rs temp"); { let g = rs(Result.Ok(((R { id: 21 }, R { id: 22 }), 3))); println(f"  got:{g.0.id},{g.1.id}") }
    println("rs named"); { let a: Result[((R, R), i64), i64] = Result.Ok(((R { id: 23 }, R { id: 24 }), 3)); let g = rs(a); println(f"  got:{g.0.id},{g.1.id}") }
    println("rs err"); { let g = rs(Result.Err(7)); println(f"  got:{g.0.id},{g.1.id}") }
    println("none"); { let a: Option[(R, R)] = Option.None; let g = h1(a); println(f"  got:{g.0.id},{g.1.id}") }
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "v8 temp\n  got:81,82\n  dR81\n  dR82\nv8 named\n  got:83,84\n  dR83\n  dR84\nh1 temp\n  got:11,12\n  dR11\n  dR12\nh1 named\n  got:13,14\n  dR13\n  dR14\nmix temp\n  dR32\n  got:31,33\n  dR31\n  dR33\nmix named\n  dR35\n  got:34,36\n  dR34\n  dR36\nsl temp\n  got:42,41\n  dR41\n  dR42\nsl named\n  got:44,43\n  dR43\n  dR44\ntl temp\n  got:51,52\n  dR51\n  dR52\ntl named\n  got:53,54\n  dR53\n  dR54\nsc temp\n  dR61\n  got:61,62\n  dR62\nnl temp\n  dR71\n  got:72,99\n  dR72\n  dR99\nnl named\n  dR73\n  got:74,99\n  dR74\n  dR99\nbx temp\n  got:91,92\n  dR91\n  dR92\nbx named\n  got:93,94\n  dR93\n  dR94\nrs temp\n  got:21,22\n  dR21\n  dR22\nrs named\n  got:23,24\n  dR23\n  dR24\nrs err\n  got:7,7\n  dR7\n  dR7\nnone\n  got:0,0\n  dR0\n  dR0\nend\n", "got:\n{out}");
}
