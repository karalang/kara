//! B-2026-09-19-31 -- a destructured payload element handed back on one path
//! only: the element left behind runs its `Drop` body at the callee's exit.

use super::*;

/// B-2026-09-19-31 — a by-value `Option`/`Result` param whose destructured
/// tuple payload hands an element back on SOME paths only. The escape set is
/// per arm, so it named every element and the caller stood the whole payload
/// down; the element left behind on the path taken then had its `Drop` body
/// run by nobody, on all four surfaces (`tf` printed `got:6 dR6`, no `dR5`).
/// The callee now owns the elements in that shape, so a `return a` moves `a`
/// and its sibling dies at the frame's exit.
///
/// Cells: both branches with a fresh temp and a named local, the `None` arm,
/// an `if` TAIL yield, a `Result`, a scalar sibling (`cs`), three elements
/// with two hand-backs (`cs3`, including the path that returns neither),
/// a `match` rather than an `if`, and a discarded result. Heap-boxed payloads
/// are declined on both backends and keep their old answer.
/// Byte-identical to the codegen twin
/// `e2e_destructured_payload_conditional_handback_runs_the_left_part`.
#[test]
fn test_destructured_payload_conditional_handback_runs_the_left_part() {
    let out = run(r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }

fn eat(o: Option[(R, R)], k: bool) -> R {
    match o {
        Option.Some((a, b)) => { if k { return a; } return b; }
        Option.None => { return R { id: 0 }; }
    }
}
fn eatT(o: Option[(R, R)], k: bool) -> R {
    match o {
        Option.Some((a, b)) => if k { a } else { b },
        Option.None => R { id: 0 },
    }
}
fn eatRes(o: Result[(R, R), i64], k: bool) -> R {
    match o {
        Result.Ok((a, b)) => { if k { return a; } return b; }
        Result.Err(_) => { return R { id: 0 }; }
    }
}
fn cs(o: Option[(R, i64)], k: bool) -> R {
    match o {
        Option.Some((a, n)) => { if k { return a; } return R { id: n }; }
        Option.None => { return R { id: 0 }; }
    }
}
fn cs3(o: Option[(R, R, i64)], k: bool) -> R {
    match o {
        Option.Some((a, b, n)) => { if k { return a; } if n > 0 { return b; } return R { id: n }; }
        Option.None => { return R { id: 0 }; }
    }
}
fn nested(o: Option[(R, R)], k: i64) -> R {
    match o {
        Option.Some((a, b)) => { match k { 0 => { return a; } _ => { return b; } } }
        Option.None => { return R { id: 0 }; }
    }
}

fn main() {
    println("tf"); { let g = eat(Option.Some((R { id: 5 }, R { id: 6 })), false); println(f"  got:{g.id}") }
    println("tt"); { let g = eat(Option.Some((R { id: 7 }, R { id: 8 })), true); println(f"  got:{g.id}") }
    println("nf"); { let a = Option.Some((R { id: 9 }, R { id: 10 })); let g = eat(a, false); println(f"  got:{g.id}") }
    println("nt"); { let a = Option.Some((R { id: 11 }, R { id: 12 })); let g = eat(a, true); println(f"  got:{g.id}") }
    println("none"); { let g = eat(Option.None, true); println(f"  got:{g.id}") }
    println("tailf"); { let g = eatT(Option.Some((R { id: 19 }, R { id: 20 })), false); println(f"  got:{g.id}") }
    println("tailt"); { let g = eatT(Option.Some((R { id: 21 }, R { id: 22 })), true); println(f"  got:{g.id}") }
    println("resf"); { let g = eatRes(Result.Ok((R { id: 23 }, R { id: 24 })), false); println(f"  got:{g.id}") }
    println("rest"); { let g = eatRes(Result.Ok((R { id: 25 }, R { id: 26 })), true); println(f"  got:{g.id}") }
    println("csf"); { let g = cs(Option.Some((R { id: 27 }, 50)), false); println(f"  got:{g.id}") }
    println("cst"); { let g = cs(Option.Some((R { id: 28 }, 60)), true); println(f"  got:{g.id}") }
    println("csn"); { let a = Option.Some((R { id: 29 }, 70)); let g = cs(a, false); println(f"  got:{g.id}") }
    println("c3a"); { let g = cs3(Option.Some((R { id: 30 }, R { id: 31 }, 1)), true); println(f"  got:{g.id}") }
    println("c3b"); { let g = cs3(Option.Some((R { id: 32 }, R { id: 33 }, 1)), false); println(f"  got:{g.id}") }
    println("c3c"); { let g = cs3(Option.Some((R { id: 34 }, R { id: 35 }, 0)), false); println(f"  got:{g.id}") }
    println("n0"); { let g = nested(Option.Some((R { id: 36 }, R { id: 37 })), 0); println(f"  got:{g.id}") }
    println("n1"); { let g = nested(Option.Some((R { id: 38 }, R { id: 39 })), 1); println(f"  got:{g.id}") }
    println("disc"); { cs(Option.Some((R { id: 40 }, 200)), false); println("  x") }
    println("end")
}
"#);
    assert_eq!(out, "tf\n  dR5\n  got:6\n  dR6\ntt\n  dR8\n  got:7\n  dR7\nnf\n  dR9\n  got:10\n  dR10\nnt\n  dR12\n  got:11\n  dR11\nnone\n  got:0\n  dR0\ntailf\n  dR19\n  got:20\n  dR20\ntailt\n  dR22\n  got:21\n  dR21\nresf\n  dR23\n  got:24\n  dR24\nrest\n  dR26\n  got:25\n  dR25\ncsf\n  dR27\n  got:50\n  dR50\ncst\n  got:28\n  dR28\ncsn\n  dR29\n  got:70\n  dR70\nc3a\n  dR31\n  got:30\n  dR30\nc3b\n  dR32\n  got:33\n  dR33\nc3c\n  dR35\n  dR34\n  got:0\n  dR0\nn0\n  dR37\n  got:36\n  dR36\nn1\n  dR38\n  got:39\n  dR39\ndisc\n  dR40\n  dR200\n  x\nend\n", "got:\n{out}");
}
