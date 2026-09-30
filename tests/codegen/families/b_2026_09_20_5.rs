//! B-2026-09-20-5 -- a NAMED local handed to a callee that returns a part
//! two or more hops into its `Option` tuple payload (`return t.0.1`).

use super::*;

/// B-2026-09-20-5 — a named `Option` local passed by value to a callee whose
/// arm returns a part below the payload's top level (`t.0.1`, `t.0.0.1`,
/// `t.0.0.a`). The caller's `let`-site walk is amended with the parts the
/// callee takes, but the amendment built a FLAT mask and declined any path
/// deeper than one hop, so the local's walk ran the escapee's body and the
/// caller's result binding ran it again: `dR5 dR6 got:6 dR6` on every compiled
/// surface against the due `dR5 got:6 dR6`. The named-local route now takes
/// the same tree-shaped mask the fresh-temp route has used since
/// B-2026-09-19-33.
///
/// Cells: the one-hop spelling (fixed earlier by B-2026-09-14-6) as a control,
/// both elements at two hops, the fresh-temp twin, three hops, a struct inside
/// the tuple, a scalar-leaf copy read that takes nothing, a method with `self`
/// and an associated fn (the receiver-index adjustment), and the `None` arm.
/// Byte-identical to the interpreter twin
/// `test_named_local_two_hop_payload_handback_runs_each_body_once`.
#[test]
fn e2e_named_local_two_hop_payload_handback_runs_each_body_once() {
    let Some(out) = run_program(
        r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
struct W { a: R, b: R }
struct H { k: i64 }

fn m1(o: Option[(R, R)]) -> R {
    match o { Option.Some(t) => { return t.0; } Option.None => { return R { id: 0 }; } }
}
fn m01(o: Option[((R, R), i64)]) -> R {
    match o { Option.Some(t) => { return t.0.1; } Option.None => { return R { id: 0 }; } }
}
fn m00(o: Option[((R, R), i64)]) -> R {
    match o { Option.Some(t) => { return t.0.0; } Option.None => { return R { id: 0 }; } }
}
fn m3(o: Option[(((R, R), R), i64)]) -> R {
    match o { Option.Some(t) => { return t.0.0.1; } Option.None => { return R { id: 0 }; } }
}
fn viaW(o: Option[((W, R), i64)]) -> R {
    match o { Option.Some(t) => { return t.0.0.a; } Option.None => { return R { id: 0 }; } }
}
fn peek(o: Option[((R, R), i64)]) -> i64 {
    match o { Option.Some(t) => { return t.0.1.id; } Option.None => { return 0; } }
}
impl H {
    fn take(self, o: Option[((R, R), i64)]) -> R {
        match o { Option.Some(t) => { return t.0.1; } Option.None => { return R { id: 0 }; } }
    }
    fn assoc(o: Option[((R, R), i64)]) -> R {
        match o { Option.Some(t) => { return t.0.0; } Option.None => { return R { id: 0 }; } }
    }
}
fn main() {
    println("one"); { let a = Option.Some((R { id: 1 }, R { id: 2 })); let g = m1(a); println(f"  got:{g.id}") }
    println("t01"); { let a = Option.Some(((R { id: 5 }, R { id: 6 }), 9)); let g = m01(a); println(f"  got:{g.id}") }
    println("t00"); { let a = Option.Some(((R { id: 7 }, R { id: 8 }), 9)); let g = m00(a); println(f"  got:{g.id}") }
    println("tmp"); { let g = m01(Option.Some(((R { id: 9 }, R { id: 10 }), 9))); println(f"  got:{g.id}") }
    println("three"); { let a = Option.Some((((R { id: 11 }, R { id: 12 }), R { id: 13 }), 9)); let g = m3(a); println(f"  got:{g.id}") }
    println("viaW"); { let a = Option.Some(((W { a: R { id: 21 }, b: R { id: 22 } }, R { id: 23 }), 9)); let g = viaW(a); println(f"  got:{g.id}") }
    println("peek"); { let a = Option.Some(((R { id: 31 }, R { id: 32 }), 9)); let n = peek(a); println(f"  got:{n}") }
    println("method"); { let h = H { k: 1 }; let a = Option.Some(((R { id: 41 }, R { id: 42 }), 9)); let g = h.take(a); println(f"  got:{g.id}") }
    println("assoc"); { let a = Option.Some(((R { id: 51 }, R { id: 52 }), 9)); let g = H.assoc(a); println(f"  got:{g.id}") }
    println("none"); { let a: Option[((R, R), i64)] = Option.None; let g = m01(a); println(f"  got:{g.id}") }
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "one\n  dR2\n  got:1\n  dR1\nt01\n  dR5\n  got:6\n  dR6\nt00\n  dR8\n  got:7\n  dR7\ntmp\n  dR9\n  got:10\n  dR10\nthree\n  dR11\n  dR13\n  got:12\n  dR12\nviaW\n  dR22\n  dR23\n  got:21\n  dR21\npeek\n  dR31\n  dR32\n  got:32\nmethod\n  dR41\n  got:42\n  dR42\nassoc\n  dR52\n  got:51\n  dR51\nnone\n  got:0\n  dR0\nend\n", "got:\n{out}");
}
