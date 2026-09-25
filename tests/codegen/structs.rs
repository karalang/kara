//! structs, fields, SoA layouts, tuples, repr/ABI -- fixtures for `tests/codegen.rs`.
//!
//! Split out of `tests/codegen.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test codegen` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test codegen structs::
//!
//! New fixtures about structs, fields, SoA layouts, tuples, repr/ABI belong in this file.

use super::*;

/// Adversarial for the field-projection guard's PRECISION: it must not
/// over-reject. (a) Projecting a NON-capturing closure field out of a local
/// struct is sound (null env) and must compile. (b) A struct that has a
/// capturing closure field AND a plain field must still let you return the
/// *other*, non-closure field — `capturing_fields` records only `f`, so
/// `return h.g` (an i64) is untouched.
#[test]
fn field_projection_guard_does_not_over_reject() {
    assert!(
        ir_result(
            "struct H { f: Fn(i64) -> i64 }\n\
                 fn make() -> Fn(i64) -> i64 { let h = H { f: |x| x * 2i64 }; h.f }\n"
        )
        .is_ok(),
        "projecting a NON-capturing closure field must compile"
    );
    assert!(
            ir_result(
                "struct H { f: Fn(i64) -> i64, g: i64 }\n\
                 fn make(k: i64) -> i64 { let h = H { f: |x| x + k, g: 99i64 }; h.g }\n"
            )
            .is_ok(),
            "returning a non-closure field of a struct that also has a capturing closure field must compile"
        );
}

/// B-2026-08-11-34 — the three constructs `prepare_for_resolve` exists
/// for, pinned through this file's own IR harness.
///
/// Each is ordinary Kāra that `karac build` compiles and runs correctly,
/// and each used to fail in any harness that resolved the raw parse tree:
/// multi-assignment tripped the resolver's `unreachable!()` outright, and
/// a trait default method / an `impl Trait` parameter reached codegen as
/// an unresolvable method call whose diagnostic says "this is a codegen
/// bug" — sending the reader into the backend over a missing front-end
/// pass. 103 of the 109 codegen-invoking harnesses in `tests/` had drifted
/// off some part of the sequence; this pins the three that bite.
#[test]
fn desugar_dependent_constructs_reach_codegen_through_the_harness() {
    // Multi-assignment. Without the pass this is a PANIC, not an Err —
    // `StmtKind::MultiAssign is removed by the desugar pass before
    // reaching this phase` — so a plain `is_ok()` is the whole assertion.
    assert!(
        ir_result("fn main() { let mut a: i64 = 1; let mut b: i64 = 2; a, b = b, a; println(a); }")
            .is_ok(),
        "multi-assignment must survive the harness"
    );

    // A trait DEFAULT METHOD the impl does not override.
    let ir = ir_result(
        "trait Greet {\n\
                 fn name(self) -> String;\n\
                 fn greet(self) -> String { return \"hi \" + self.name(); }\n\
             }\n\
             struct P { n: String }\n\
             impl Greet for P { fn name(self) -> String { return self.n; } }\n\
             fn main() { let p: P = P { n: \"bob\" }; println(p.greet()); }\n",
    )
    .expect("a trait default method must survive the harness");
    assert!(
        ir.contains("greet"),
        "the synthesized body should be emitted"
    );

    // An argument-position `impl Trait`.
    assert!(
        ir_result(
            "trait Shape { fn area(self) -> i64; }\n\
                 struct Sq { s: i64 }\n\
                 impl Shape for Sq { fn area(self) -> i64 { return self.s * self.s; } }\n\
                 fn show(x: impl Shape) { println(x.area()); }\n\
                 fn main() { show(Sq { s: 4 }); }\n",
        )
        .is_ok(),
        "an `impl Trait` parameter must survive the harness"
    );
}

#[test]
fn test_e2e_place_struct_arg_escaping_field_runs_one_body() {
    let out = run_program(
        r#"
struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] }; }
struct Cd { r: R, z: i64 }
fn cEsc(h: Cd) -> R { let Cd { r, z } = h; println("in"); return r; }
fn zEsc(h: Cd) -> i64 { println("in5"); return h.z; }
struct Dd { a: R, b: R }
fn dEsc(h: Dd) -> R { let Dd { a, b } = h; println("in2"); return a; }
fn pEsc(h: Cd) -> R { println("in3"); return h.r; }
struct Gd[T] { r: T, z: i64 }
fn gEsc[T](h: Gd[T]) -> T { let Gd { r, z } = h; println("in4"); return r; }
fn main() {
  let g1 = Cd { r: mk(13), z: 9 };  let o1 = cEsc(g1); println(f"got{o1.id}");
  let g2 = Dd { a: mk(31), b: mk(32) }; let o2 = dEsc(g2); println(f"got{o2.id}");
  let g3 = Cd { r: mk(41), z: 9 };  let o3 = pEsc(g3); println(f"got{o3.id}");
  let g4 = Gd { r: mk(81), z: 9 };  let o4 = gEsc(g4); println(f"got{o4.id}");
  let g5 = Cd { r: mk(91), z: 9 };  let _ = cEsc(g5); println("after");
  let g6 = Cd { r: mk(51), z: 5 };  let v6 = zEsc(g6); println(f"gotz{v6}");
  println("end");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
                out, "in\ngot13\ndR13\nin2\ndR32\ngot31\ndR31\nin3\ngot41\ndR41\nin4\ngot81\ndR81\nin\ndR91\nafter\nin5\ndR51\ngotz5\nend\n",
                "each escaping field dies once, at its owner in the caller, and the \
                 non-escaping sibling `b` keeps its body inside the call; got {out:?}"
            );
    }
}

#[test]
fn e2e_method_on_indexed_tuple_element_vec() {
    // B-2026-07-20-4: calling a method on an indexed element of a
    // tuple-element Vec (`t.0[i].len()`) failed codegen loud
    // ("indexed-receiver method ... requires the indexed container to be a
    // named variable") while the interpreter ran it. The indexed-receiver
    // method dispatch now hoists a `TupleIndex` container to a synth Vec
    // identifier (the sibling of its `FieldAccess` hoist).
    if let Some(out) = run_program(
        "fn make() -> (Vec[String], Vec[i64]) {\n\
                 let mut a: Vec[String] = Vec.new();\n\
                 a.push(\"alpha\"); a.push(\"beta\");\n\
                 let mut b: Vec[i64] = Vec.new();\n\
                 b.push(100);\n\
                 (a, b)\n\
             }\n\
             fn main() {\n\
                 let t = make();\n\
                 println(t.0[0].len());\n\
                 println(t.0[1].len());\n\
             }",
    ) {
        assert_eq!(out, "5\n4\n");
    }
}

// ── Structs ──────────────────────────────────────────────────

#[test]
fn test_ir_struct_declaration() {
    // Use function params so LLVM cannot constant-fold the insertvalue instructions.
    let ir = ir_for(
        r#"
struct Point { x: i64, y: i64 }
fn make_point(x: i64, y: i64) -> Point { Point { x: x, y: y } }
"#,
    );
    assert!(
        ir.contains("insertvalue"),
        "struct init should use insertvalue"
    );
}

#[test]
fn test_ir_struct_field_access() {
    let ir = ir_for(
        r#"
struct Point { x: i64, y: i64 }
fn x_coord(p: Point) -> i64 { p.x }
"#,
    );
    assert!(
        ir.contains("extractvalue"),
        "field access should use extractvalue"
    );
}

#[test]
fn test_ir_struct_field_by_name() {
    let ir = ir_for(
        r#"
struct Rect { width: i64, height: i64 }
fn area(r: Rect) -> i64 { r.width * r.height }
"#,
    );
    assert!(ir.contains("smul.with.overflow"));
    assert!(ir.contains("extractvalue"));
}

#[test]
fn test_ir_struct_init_and_use() {
    let ir = ir_for(
        r#"
struct Vec2 { x: f64, y: f64 }
fn magnitude_sq(v: Vec2) -> f64 { v.x * v.x + v.y * v.y }
fn make_vec(x: f64, y: f64) -> Vec2 { Vec2 { x: x, y: y } }
"#,
    );
    assert!(ir.contains("fmul"));
    assert!(ir.contains("fadd"));
}

// ── Tuples ───────────────────────────────────────────────────

#[test]
fn test_ir_tuple_create_and_index() {
    let ir = ir_for(
        r#"
fn swap(a: i64, b: i64) -> (i64, i64) { (b, a) }
fn first(t: (i64, i64)) -> i64 { t.0 }
"#,
    );
    assert!(ir.contains("insertvalue"));
    assert!(ir.contains("extractvalue"));
}

/// B-2026-09-03-13 — TWO ORDINARY TUPLE LOCALS SEGFAULTED THE PROGRAM, because the
/// per-tuple `Drop`-body walker `__karac_dropelems_tuple_*` was named after the elements
/// it VISITS and nothing else.
///
/// `let t = (0, mk(1));` in one function and `let t = ("sss", mk(2));` in another is the
/// whole repro — no destructure, no move-out, no mask. Both tuples have exactly one
/// body-bearing element, `R` at index 1, so both resolved to the symbol `..._1_R`; the
/// first one emitted the body, GEPping element 1 at its own `{i64, R}` offset, and the
/// second reused it against `{Vec, R}`. The walker then read a `String` header from the
/// middle of the struct and `memmove`d from a null pointer: SIGSEGV on all three compiled
/// surfaces, where `--interp` is correct.
///
/// THE ELEMENTS THE WALKER STEPS OVER ARE THE ONES THAT MOVE THE OFFSETS, and they were
/// the ones the name left out. The hazard had been seen in a narrower form — the surviving
/// element is mangled by its full `TypeExpr` rather than its head name, so `(Vec[Res],
/// i64)` and `(Res, i64)` stopped colliding — but that fixed the type of the element the
/// walker TOUCHES.
///
/// THE KEY IS THE LLVM AGGREGATE TYPE, NOT THE SOURCE TYPES, and the first cut of the fix
/// is why: keying on `display_mangle_te` of every element left this program still crashing,
/// because the element `TypeExpr`s reaching the emitter are UNRESOLVED for exactly the
/// elements in question — `0` and `"sss"` both mangle to the empty string, so both walkers
/// were still named `..._1_R$in_R`. `agg_ty` is what the GEPs are emitted against, so
/// keying on it makes the symbol and the offsets agree by construction.
///
/// Cells: `scalar-first` and `heap-first` are the crashing pair (their ORDER does not
/// matter — measured both ways). `masked` is the same collision reached through
/// B-2026-08-03-3's move-out mask (`let x = t.0` over `(R, Option[R])` leaves surviving
/// target `[1]`), and `unmasked-peer` is the `(i64, Option[R])` local that shared its
/// symbol. All four surfaces agree on this output.
#[test]
fn e2e_tuple_elem_bodies_walker_is_keyed_by_layout() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}/{self.xs.len()}") } }
fn mk(id: i64) -> R { let mut v: Vec[i64] = Vec.new(); v.push(id); return R { id: id, tag: f"t{id}", xs: v } }

fn a1() { let t = (0, mk(1));     println("  e1") }
fn a2() { let t = ("sss", mk(2)); println("  e2") }
fn a3() { let t = (mk(3), Option.Some(mk(33))); let x = t.0; println(f"  e3 {x.id}") }
fn a4() { let t = (0, Option.Some(mk(4)));                   println("  e4") }

fn main() {
    println("scalar-first");  a1(); println("scalar-first end")
    println("heap-first");    a2(); println("heap-first end")
    println("masked");        a3(); println("masked end")
    println("unmasked-peer"); a4(); println("unmasked-peer end")
    println("done")
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"scalar-first
dR1/t1/1
  e1
scalar-first end
heap-first
dR2/t2/1
  e2
heap-first end
masked
dR33/t33/1
  e3 3
dR3/t3/1
masked end
unmasked-peer
dR4/t4/1
  e4
unmasked-peer end
done
"#
    );
}

/// B-2026-09-03-14 — THE THREE ARMS B-2026-09-02-43's OWNER MASK DID NOT REACH.
///
/// That row taught `finish_place_source_tuple_destructure` to mask the elements whose
/// leaf took a body out of the owner's `__karac_dropbodies_*` walk, and recorded them
/// from ONE site: the enum / nested-struct BINDING arm. Three other spellings hand a
/// body away at the same statement and recorded nothing, so the owner kept running it.
/// Measured against `--interp` on the commit that fixed -43:
///
///   * `wild` / `wildidx` — `let (_, k) = h.pe;`. The wildcard arm runs the discarded
///     element's body ON THE SPOT and never cap-zeroes (the aggregate keeps the MEMORY,
///     by design), so this one's defect looks unlike the row's: TWO bodies both reading
///     a LIVE value, rather than a husk and a live one. `wildidx` is the same shape with
///     the wildcard at index 1, since the recorded index has to be the element's own.
///   * `nested` — `let ((r, a), b) = h.pe;`. The recursion cap-zeroes the inner leaf it
///     takes, but the call site discarded the inner set outright, so nothing recorded
///     the OUTER element and the owner's walk still descended into the field: `dR4//0`
///     before the live read. -43's defect exactly, one level down.
///   * `owndrop` — a parent with its own `impl Drop`. It has NO per-binding bodies
///     action to replace: it runs its field bodies from inside `karac_drop_<T>`, the
///     type-level wrapper registered as a separate `OwnWrapper` action
///     (B-2026-09-01-40), so the disarm's replace/suppress pair found nothing.
///     `dHd5 dR5//0 b5/t5 dR5/t5/1` against the interpreter's three lines.
///
/// `wildopt` IS THE OVER-REACH CONTROL, and it is why the wildcard arm asks a question
/// rather than masking unconditionally. Over `(i64, Option[R])` the discard helper
/// DECLINES — its `Option`/`Result` handling is not the parent walk's — so the body it
/// would silence is the only one there is. The signal is
/// `run_discarded_leaf_user_drop_bodies`' `ran_bodies`, and the single `bool` it used to
/// return meant `took_memory`, which is ALWAYS false at a call site passing
/// `free_memory: false`; gating on that silently disabled the whole `wild` fix instead.
/// The helper now reports both answers separately.
///
/// `plain` and `param` are the unchanged legs: -43's own shape stays at one live body,
/// and a by-value param source keeps the caller-retained body fired after the callee
/// returns. Every cell renders `self.tag` and `self.xs.len()`, because on an
/// `R { id: i64 }` a husk and a live value print the same thing and a count-only
/// assertion cannot tell them apart.
///
/// `d1` (the `owndrop` cell) carries `#[allow(partial_move_of_drop_struct)]` since
/// B-2026-09-01-43: moving a tuple field out of a parent that declares its own `Drop`
/// is exactly the shape design.md § Part 8 `Drop` rejects, so at `Deny` the cell no
/// longer compiles without the opt-out. Keeping it is deliberate — the drop placement
/// it pins stays reachable through that attribute, so the coverage still guards
/// programs someone can write.
///
/// Twin of `tests/interpreter.rs`'s
/// `test_destructure_owner_mask_reaches_the_remaining_arms`, pinned to the same string.
/// `e2e_local_struct_tuple_field_destructure_masks_the_owner` pins B-2026-09-02-43's own
/// cells; these are the ones it does not reach.
#[test]
fn e2e_destructure_owner_mask_reaches_the_remaining_arms() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}/{self.xs.len()}") } }

struct H  { pe: (R, i64) }
struct Hi { pe: (i64, R) }
struct Hw { pe: (i64, Option[R]) }
struct Hn { pe: ((R, i64), i64) }
struct Hd { pe: (R, i64), n: i64 }
impl Drop for Hd { fn drop(mut ref self) { println(f"dHd{self.n}") } }

fn mk(id: i64) -> R { let mut v: Vec[i64] = Vec.new(); v.push(id); return R { id: id, tag: f"t{id}", xs: v } }

fn w1() { let h = H  { pe: (mk(1), 0) };            let (_, k) = h.pe;      println("  end") }
fn w2() { let h = Hi { pe: (0, mk(2)) };            let (k, _) = h.pe;      println("  end") }
fn w3() { let h = Hw { pe: (0, Option.Some(mk(3))) }; let (k, _) = h.pe;    println("  end") }
fn n1() { let h = Hn { pe: ((mk(4), 0), 1) };       let ((r, a), b) = h.pe; println(f"  b{r.id}/{r.tag}"); println("  end") }
#[allow(partial_move_of_drop_struct)]
fn d1() { let h = Hd { pe: (mk(5), 0), n: 5 };      let (r, k) = h.pe;      println(f"  b{r.id}/{r.tag}"); println("  end") }
fn c1() { let h = H  { pe: (mk(6), 0) };            let (r, k) = h.pe;      println(f"  b{r.id}/{r.tag}"); println("  end") }
fn c2(h: H) { let (r, k) = h.pe; println(f"  b{r.id}/{r.tag}"); println("  end") }

fn main() {
    println("wild");     w1();                      println("wild end")
    println("wildidx");  w2();                      println("wildidx end")
    println("wildopt");  w3();                      println("wildopt end")
    println("nested");   n1();                      println("nested end")
    println("owndrop");  d1();                      println("owndrop end")
    println("plain");    c1();                      println("plain end")
    println("param");    c2(H { pe: (mk(7), 0) });  println("param end")
    println("done")
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"wild
dR1/t1/1
  end
wild end
wildidx
dR2/t2/1
  end
wildidx end
wildopt
dR3/t3/1
  end
wildopt end
nested
  b4/t4
dR4/t4/1
  end
nested end
owndrop
dHd5
  b5/t5
dR5/t5/1
  end
owndrop end
plain
  b6/t6
dR6/t6/1
  end
plain end
param
  b7/t7
  end
dR7/t7/1
param end
done
"#
    );
}

/// B-2026-09-04-29 — a by-value param destructure leaf REBOUND (`let c = b;`)
/// runs the payload's body exactly once.
///
/// The caller retains a by-value param's field bodies (it runs them on its temp
/// after the call), which is why the callee's own leaves are param views taking
/// memory-only drops. The let-site rebind gave `c` a bodies walker of its own,
/// so the body ran in the callee and again in the caller — on both compiled
/// backends, for the identifier source (`rebind`, `rebindu`, `rebindcall`,
/// `twice`) and its projection (`prebind`). `orebind` / `porebind` are the
/// `Option` twins (always correct — the boxed path — kept as controls), `direct`
/// the un-rebound leaf.
///
/// Interpreter twin `test_param_destructure_leaf_rebind_runs_body_once`; ASAN twin `asan_param_destructure_leaf_rebind_is_balanced`.
#[test]
fn e2e_param_destructure_leaf_rebind_runs_body_once() {
    let src = r#"struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}") } }
fn mk(n: i64) -> R { return R { id: n, tag: f"t{n}" }; }
struct HoRes { a: R, b: Result[R, String] }
struct HoOpt { a: R, b: Option[R] }
struct WrapR { inner: HoRes }
struct WrapO { inner: HoOpt }
fn eat(x: R) { println(f"  eat{x.id}") }

fn rebind(h: HoRes)    { let HoRes { a, b } = h; let c = b;
                         match c { Result.Ok(r) => println(f"  ok{r.tag}"), Result.Err(e) => println(f"  er{e}") } }
fn rebindu(h: HoRes)   { let HoRes { a, b } = h; let c = b; println("  m") }
fn rebindcall(h: HoRes){ let HoRes { a, b } = h; let c = b;
                         match c { Result.Ok(r) => eat(r), Result.Err(e) => println(f"  er{e}") } }
fn orebind(h: HoOpt)   { let HoOpt { a, b } = h; let c = b;
                         match c { Option.Some(r) => println(f"  ok{r.tag}"), Option.None => println("  none") } }
fn prebind(w: WrapR)   { let HoRes { a, b } = w.inner; let c = b;
                         match c { Result.Ok(r) => println(f"  ok{r.tag}"), Result.Err(e) => println(f"  er{e}") } }
fn porebind(w: WrapO)  { let HoOpt { a, b } = w.inner; let c = b;
                         match c { Option.Some(r) => println(f"  ok{r.tag}"), Option.None => println("  none") } }
fn direct(h: HoRes)    { let HoRes { a, b } = h;
                         match b { Result.Ok(r) => println(f"  ok{r.tag}"), Result.Err(e) => println(f"  er{e}") } }
fn twice(h: HoRes)     { let HoRes { a, b } = h; let c = b; let d = c;
                         match d { Result.Ok(r) => println(f"  ok{r.tag}"), Result.Err(e) => println(f"  er{e}") } }

fn main() {
  println("rebind");     rebind(HoRes { a: mk(1), b: Result.Ok(mk(101)) })
  println("rebindu");    rebindu(HoRes { a: mk(2), b: Result.Ok(mk(102)) })
  println("rebindcall"); rebindcall(HoRes { a: mk(3), b: Result.Ok(mk(103)) })
  println("orebind");    orebind(HoOpt { a: mk(4), b: Option.Some(mk(104)) })
  println("prebind");    prebind(WrapR { inner: HoRes { a: mk(5), b: Result.Ok(mk(105)) } })
  println("porebind");   porebind(WrapO { inner: HoOpt { a: mk(6), b: Option.Some(mk(106)) } })
  println("direct");     direct(HoRes { a: mk(7), b: Result.Ok(mk(107)) })
  println("twice");      twice(HoRes { a: mk(8), b: Result.Ok(mk(108)) })
  println("done")
}
"#;
    assert_eq!(
        run_program(src).as_deref(),
        Some(
            r#"rebind
  okt101
dR101/t101
dR1/t1
rebindu
  m
dR102/t102
dR2/t2
rebindcall
  eat103
dR103/t103
dR3/t3
orebind
  okt104
dR104/t104
dR4/t4
prebind
  okt105
dR105/t105
dR5/t5
porebind
  okt106
dR106/t106
dR6/t6
direct
  okt107
dR107/t107
dR7/t7
twice
  okt108
dR108/t108
dR8/t8
done
"#
        )
    );
}

/// B-2026-09-04-2 — a struct destructure whose SOURCE IS A PROJECTION hands
/// each leaf its own `Drop` body, so the body lands at the LEAF's last use
/// rather than at the projection ROOT's.
///
/// `place_body_src` answered only for a bare identifier, so
/// `let HoRes { a, b } = w.inner;` reached none of the ownership branches and
/// the leaves took nothing. The root's field-bodies walk stayed armed, and
/// because the destructure is usually the root's LAST USE it fired THERE —
/// ahead of the reads of the bindings it had just aliased:
///
///     let w = Wrap { inner: HoRes { a: mk(9), b: Result.Ok(mk(109)) } };
///     let HoRes { a, b } = w.inner;
///     println(f"  rd{a.id}")
///       --interp   ->           rd9  dR9/t9
///       compiled   ->   dR109  dR9   rd9
///
/// With explicit markers around the statement the compiled backends put both
/// bodies BETWEEN "before-destructure" and "after-destructure", which is what
/// makes this a placement defect and not an ordering preference: `a`'s body
/// ran while `a` was still live and about to be read. The read returned 9, so
/// no memory error appears and valgrind stayed at `0 bytes in use at exit`
/// both before and after — a body releasing a resource would have released it
/// before the binding's last use, the silent-wrong-value profile.
///
/// THE ORACLE IS THE NAMED-LOCAL SIBLING, PER SHAPE — `local` here. The
/// filing row nominated it too but read it only on the `Result` shape, where
/// it runs one body, and concluded the compiled column's two bodies were "the
/// odd one out". Measured across the second-field types the row left open,
/// the sibling AGREES on all four surfaces at a DIFFERENT string per shape:
/// `Result` -> `rd dR`; `Option[R]` and plain `R` -> `dR1xx rd dR`; a
/// non-`Drop` `String` -> `rd dR`. The interpreter's projection output
/// already equalled every one of those, so the correction is codegen-only —
/// there was no count question to settle, only a placement one.
///
/// NOT `Result`-SPECIFIC, which the row suspected. `str` carries no
/// `Option`/`Result` anywhere and still printed `a`'s body before the read:
/// any `Drop`-bearing FIRST field reproduces it and the second field's type
/// only changes the count. `two` pins the two-hop root the row lists as
/// unmeasured; `live` pins that a later read of the root does not change the
/// answer.
///
/// WHAT THIS DELIBERATELY DOES NOT DO: give a `Result` leaf the payload's
/// real body. `res`/`two`/`live` run no `dR1xx`, exactly as `local` does not
/// — that is B-2026-09-03-15's `Result` deferral, and `nodest`/`move` are
/// carried here as the controls showing the UNdestructured and whole-move
/// spellings still run it. Before this fix the compiled side ran it for the
/// projection spelling alone; converging on the sibling removes that, which
/// is a backend split closing rather than a body being lost.
///
/// B-2026-09-04-1 — `local` (the owned-local source) now runs the leaf's body,
/// `dR102`, at the destructure. The PROJECTION cells (`res`, `two`, `live`)
/// followed with B-2026-09-04-21: the leaf now owns the field there too
/// (transferred on a move, copied when the root is read again), so each
/// runs the unused leaf's body at the destructure.
#[test]
fn e2e_projection_source_struct_destructure_hands_each_leaf_its_body() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}") } }
fn mk(n: i64) -> R { return R { id: n, tag: f"t{n}" }; }

struct HoRes { a: R, b: Result[R, String] }
struct HoOpt { a: R, b: Option[R] }
struct HoPln { a: R, b: R }
struct HoStr { a: R, b: String }
struct WrapR { inner: HoRes }
struct WrapO { inner: HoOpt }
struct WrapP { inner: HoPln }
struct WrapS { inner: HoStr }
struct Outer { h: WrapR }

fn cell_res()   { let w = WrapR { inner: HoRes { a: mk(1), b: Result.Ok(mk(101)) } }; let HoRes { a, b } = w.inner; println(f"  rd{a.id}") }
fn cell_local() { let h = HoRes { a: mk(2), b: Result.Ok(mk(102)) };                  let HoRes { a, b } = h;       println(f"  rd{a.id}") }
fn cell_opt()   { let w = WrapO { inner: HoOpt { a: mk(3), b: Option.Some(mk(103)) } }; let HoOpt { a, b } = w.inner; println(f"  rd{a.id}") }
fn cell_pln()   { let w = WrapP { inner: HoPln { a: mk(4), b: mk(104) } };            let HoPln { a, b } = w.inner; println(f"  rd{a.id}") }
fn cell_str()   { let w = WrapS { inner: HoStr { a: mk(5), b: "plain" } };            let HoStr { a, b } = w.inner; println(f"  rd{a.id}") }
fn cell_two()   { let g = Outer { h: WrapR { inner: HoRes { a: mk(6), b: Result.Ok(mk(106)) } } }; let HoRes { a, b } = g.h.inner; println(f"  rd{a.id}") }
fn cell_live()  { let w = WrapR { inner: HoRes { a: mk(7), b: Result.Ok(mk(107)) } }; let HoRes { a, b } = w.inner; println(f"  rd{a.id}"); println(f"  w{w.inner.a.id}") }
fn cell_nodest(){ let w = WrapR { inner: HoRes { a: mk(8), b: Result.Ok(mk(108)) } }; println(f"  rd{w.inner.a.id}") }
fn cell_move()  { let w = WrapR { inner: HoRes { a: mk(9), b: Result.Ok(mk(109)) } }; let h = w.inner; println(f"  rd{h.a.id}") }

fn main() {
    println("res");    cell_res()
    println("local");  cell_local()
    println("opt");    cell_opt()
    println("pln");    cell_pln()
    println("str");    cell_str()
    println("two");    cell_two()
    println("live");   cell_live()
    println("nodest"); cell_nodest()
    println("move");   cell_move()
    println("done")
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"res
dR101/t101
  rd1
dR1/t1
local
dR102/t102
  rd2
dR2/t2
opt
dR103/t103
  rd3
dR3/t3
pln
dR104/t104
  rd4
dR4/t4
str
  rd5
dR5/t5
two
dR106/t106
  rd6
dR6/t6
live
dR107/t107
  rd7
dR7/t7
  w7
nodest
  rd8
dR108/t108
dR8/t8
move
  rd9
dR109/t109
dR9/t9
done
"#
    );
}

/// Twin of `tests/interpreter.rs`'s
/// `test_projection_source_tuple_destructure_is_a_view`, pinned to the same
/// string.
#[test]
fn e2e_projection_source_tuple_destructure_is_a_view() {
    let Some(out) = run_program(
        r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct H  { pe: (R, i64) }
struct Hs { pe: (R, i64), name: String }
struct G  { h: H }

fn f1(h: H)   { let (r, k) = h.pe;   let m = r; println(f"  b{m.id}") }
fn f2(hs: Hs) { let (r, k) = hs.pe;  let m = r; println(f"  b{m.id} {hs.name}") }
fn f3(g: G)   { let (r, k) = g.h.pe; let m = r; println(f"  b{m.id}") }
fn f4(h: H)   { let (r, k) = h.pe;   println(f"  b{r.id}") }
fn f5(h: H)   { let h2 = h; let (r, k) = h2.pe; let m = r; println(f"  b{m.id}") }
fn f6(h: ref H) { println(f"  b{h.pe.0.id}") }

fn main() {
    println("plain");    f1(H  { pe: (R { id: 1 }, 0) });                println("plain end")
    println("ownstr");   f2(Hs { pe: (R { id: 2 }, 0), name: "n" });     println("ownstr end")
    println("twohop");   f3(G  { h: H { pe: (R { id: 3 }, 0) } });       println("twohop end")
    println("norebind"); f4(H  { pe: (R { id: 4 }, 0) });                println("norebind end")
    println("rebind");   f5(H  { pe: (R { id: 5 }, 0) });                println("rebind end")
    println("refparam"); let h6 = H { pe: (R { id: 6 }, 0) }; f6(h6);    println("refparam end")
    println("done")
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"plain
  b1
dR1
plain end
ownstr
  b2 n
dR2
ownstr end
twohop
  b3
dR3
twohop end
norebind
  b4
dR4
norebind end
rebind
  b5
dR5
rebind end
refparam
  b6
dR6
refparam end
done
"#
    );
}

/// B-2026-09-02-44 — a projection destructure whose root INHERITED view-ness
/// from an owned param (`let h2 = h; let (r, k) = h2.pe;`) binds views too, exactly
/// as one rooted at the param itself does since B-2026-09-02-40.
///
/// The concept was already on both sides and already transitive: `let h2 = h;`
/// writes `h2` into codegen's `param_view_locals` and the interpreter's
/// `owned_param_names_stack`, which is why `let h2 = h; let x = h2.pe;` and
/// `let t2 = t; let x = t2.0;` were correct before this. The destructure gate was
/// the ONE place asking the narrower question — "is the root a PARAMETER" —
/// against `current_fn_param_names` rather than the union its own
/// `expr_is_param_view` reads.
///
/// WHAT THE FILING ROW GOT WRONG, and why these cells are pinned here rather than
/// only as `rebind` in `e2e_projection_source_tuple_destructure_is_a_view`. The row described all four surfaces as
/// agreed-and-wrong at two bodies and read that agreement as both backends
/// declining for one reason. It held with the trailing `let m = r;` and nowhere
/// else: `norebind` (the same shape without it) measured ONE body interpreted
/// against TWO on all three compiled surfaces, and `method` did the same. The
/// interpreter had been retracting its own slots for an inherited root all along —
/// only its PROPAGATION onto the bound names was withheld — so the withholding was
/// not holding the backends together, which was its whole justification. A live
/// run-vs-build split sat inside a row filed as agreed because one spelling was
/// measured and the neighbouring one was not.
///
/// `local` and `noproj` are the over-reach controls, failing in opposite
/// directions: a local tuple source owns its element and must keep its single
/// body, and a rebind with no projection at all must not lose the caller's.
///
/// THE LOCAL PROJECTION CONTROL IS DELIBERATELY ABSENT. `let h = H { … };
/// let h2 = h; let (r, k) = h2.pe;` prints the body BEFORE the binding is read on
/// all three compiled surfaces — B-2026-09-02-43, an open row about a cap-zeroed
/// husk, unrelated to view-ness and untouched here. Pinning it would make this
/// twin fail when that row is fixed, and would claim a cell this change does not
/// own.
///
/// Twin of `tests/interpreter.rs`'s
/// `test_inherited_param_view_root_destructure_is_a_view`, pinned to the same string.
#[test]
fn e2e_inherited_param_view_root_destructure_is_a_view() {
    let Some(out) = run_program(
        r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct H    { pe: (R, i64) }
struct Hold { n: i64 }

fn g1(h: H)         { let h2 = h; let (r, k) = h2.pe; println(f"  b{r.id}") }
fn g2(h: H)         { let h2 = h; let (r, k) = h2.pe; let m = r; println(f"  b{m.id}") }
fn g3(h: H)         { let h2 = h; let h3 = h2; let (r, k) = h3.pe; let m = r; println(f"  b{m.id}") }
fn g4(t: (R, i64))  { let t2 = t; let (r, k) = t2; let m = r; println(f"  b{m.id}") }
impl Hold { fn take(ref self, h: H) { let h2 = h; let (r, k) = h2.pe; let m = r; println(f"  b{m.id}") } }
fn g6()             { let t = (R { id: 26 }, 0); let t2 = t; let (r, k) = t2; let m = r; println(f"  b{m.id}") }
fn g7(h: H)         { let h2 = h; println("  np") }

fn main() {
    println("norebind"); g1(H { pe: (R { id: 21 }, 0) });                     println("norebind end")
    println("rebind");   g2(H { pe: (R { id: 22 }, 0) });                     println("rebind end")
    println("twohop");   g3(H { pe: (R { id: 23 }, 0) });                     println("twohop end")
    println("tupreb");   g4((R { id: 24 }, 0));                               println("tupreb end")
    println("method");   let o = Hold { n: 0 }; o.take(H { pe: (R { id: 25 }, 0) }); println("method end")
    println("local");    g6();                                                println("local end")
    println("noproj");   g7(H { pe: (R { id: 27 }, 0) });                     println("noproj end")
    println("done")
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"norebind
  b21
dR21
norebind end
rebind
  b22
dR22
rebind end
twohop
  b23
dR23
twohop end
tupreb
  b24
dR24
tupreb end
method
  b25
dR25
method end
local
  b26
dR26
local end
noproj
  np
dR27
noproj end
done
"#
    );
}

/// B-2026-09-02-43 — a `let`-destructure over a LOCAL struct's tuple field
/// (`let h = H {{ pe: (mk(21), 0) }}; let (r, k) = h.pe;`) hands each element's
/// `Drop` body to its leaf, so the owner's walk must stop running it.
///
/// It did not. The owner's walk fired the same body a SECOND time, against the slot
/// `zero_tuple_elem_cap_at` had just emptied, and fired it FIRST — before the live
/// read. The user body therefore observed a value whose `String` read empty and
/// whose `Vec` read length zero: `dR21//0`, then `b21/t21/1`, then the real
/// `dR21/t21/1`, on all three compiled surfaces against one live body under
/// `--interp`.
///
/// WHY EVERY EXISTING TEST IN THIS FAMILY MISSES IT, and why this one renders. The
/// husk is ZEROED, not freed, so nothing double-frees: valgrind and LSan are clean
/// at `-O0` and `-O2` alike, and the memory-sanitizer suite cannot see it. The
/// regression tests around it assert body COUNTS over an `R {{ id: i64 }}` — a
/// struct with nothing that can read empty — so a body running on a husk is
/// indistinguishable from one running on the live value. `R` here carries a
/// `String` and a `Vec` and the body prints both, which is the only instrument that
/// detects this class at all. A count-only assertion would have passed on the
/// defect before the fix, because the COUNT was also wrong but in a way the
/// `--interp` comparison already covered.
///
/// The repair is the mask this arm never consulted. `skip.nested[i].here` means
/// "indices masked inside field i's own walker" — inner FIELD indices for a struct
/// field, ELEMENT indices for a tuple one — and the struct recursion honoured it
/// while the tuple arm called the unmasked emitter.
///
/// `baretuple` and `paramroot` are the over-reach controls on the two legs this
/// does not touch: a bare tuple LOCAL was always correct (no field hop), and a
/// PARAM root takes the other leg of `owner_runs_bodies`, where the source keeps
/// the body and the leaf must not gain one. `sibling` proves the mask is scoped to
/// the destructured field — `other` still runs, on a live value — and `bothelems`
/// that a tuple whose elements are BOTH taken masks both. `noDestr` reads the
/// element without destructuring at all and must keep the owner's single body.
///
/// WIDENED TO ANY DEPTH by B-2026-09-03-11. The one-hop restriction this test
/// originally carried was a KEYING limit on both sides, not an ownership judgement:
/// codegen's `struct_moved_nested_field_bodies` was keyed outer-field -> inner
/// indices, and the interpreter's flat mask keys `(name, field)` and its writer
/// only fired when the projected object was an IDENTIFIER. So `let (r, k) = g.h.pe`
/// recorded nothing anywhere, and BOTH backends doubled -- which is why it could
/// not be folded in here and had to move with the interpreter's own path-keyed
/// mask. `twohop` and `threehop` pin the widening; `outersib` and `midsib` pin that
/// it stays scoped, a `Drop` sibling at the OUTER and at the INTERMEDIATE level
/// each keeping its body; `deepNoDestr` reads through the whole chain without
/// destructuring and must keep the owner's single body.
///
/// Twin of `tests/interpreter.rs`'s
/// `test_local_struct_tuple_field_destructure_masks_the_owner`, pinned to the same string.
#[test]
fn e2e_local_struct_tuple_field_destructure_masks_the_owner() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}/{self.xs.len()}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] }; }
struct H  { pe: (R, i64) }
struct H2 { pe: (R, i64), other: R }
struct H3 { pe: (R, R) }
struct G  { h: H }
struct G2 { h: H, outer: R }
struct G3 { h: H2 }
struct G4 { g: G }

fn n1() { let h = H { pe: (mk(21), 0) }; let (r, k) = h.pe; let m = r; println(f"  b{m.id}/{m.tag}/{m.xs.len()}") }
fn n2() { let h = H { pe: (mk(22), 0) }; let (r, k) = h.pe; println(f"  b{r.id}/{r.tag}") }
fn n3() { let t = (mk(23), 0); let (r, k) = t; let m = r; println(f"  b{m.id}/{m.tag}") }
fn n4() { let h = H2 { pe: (mk(24), 0), other: mk(94) }; let (r, k) = h.pe; println(f"  b{r.id}/{r.tag}") }
fn n5() { let h = H3 { pe: (mk(25), mk(95)) }; let (a, b) = h.pe; println(f"  b{a.id}/{b.id}") }
fn n6() { let h = H { pe: (mk(26), 0) }; println(f"  b{h.pe.0.id}/{h.pe.0.tag}") }
fn n7(h: H) { let (r, k) = h.pe; println(f"  b{r.id}/{r.tag}") }
fn n8() { let g = G { h: H { pe: (mk(31), 0) } }; let (r, k) = g.h.pe; let m = r; println(f"  b{m.id}/{m.tag}/{m.xs.len()}") }
fn n9() { let g = G2 { h: H { pe: (mk(32), 0) }, outer: mk(82) }; let (r, k) = g.h.pe; println(f"  b{r.id}/{r.tag}") }
fn n10() { let g = G3 { h: H2 { pe: (mk(33), 0), other: mk(83) } }; let (r, k) = g.h.pe; println(f"  b{r.id}/{r.tag}") }
fn n11() { let g = G4 { g: G { h: H { pe: (mk(34), 0) } } }; let (r, k) = g.g.h.pe; println(f"  b{r.id}/{r.tag}") }
fn n12() { let g = G { h: H { pe: (mk(35), 0) } }; println(f"  b{g.h.pe.0.id}/{g.h.pe.0.tag}") }

fn main() {
    println("local1hop");   n1();                      println("local1hop end")
    println("norebind");    n2();                      println("norebind end")
    println("baretuple");   n3();                      println("baretuple end")
    println("sibling");     n4();                      println("sibling end")
    println("bothelems");   n5();                      println("bothelems end")
    println("noDestr");     n6();                      println("noDestr end")
    println("paramroot");   n7(H { pe: (mk(27), 0) }); println("paramroot end")
    println("twohop");      n8();                      println("twohop end")
    println("outersib");    n9();                      println("outersib end")
    println("midsib");      n10();                     println("midsib end")
    println("threehop");    n11();                     println("threehop end")
    println("deepNoDestr"); n12();                     println("deepNoDestr end")
    println("done")
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"local1hop
  b21/t21/1
dR21/t21/1
local1hop end
norebind
  b22/t22
dR22/t22/1
norebind end
baretuple
  b23/t23
dR23/t23/1
baretuple end
sibling
dR94/t94/1
  b24/t24
dR24/t24/1
sibling end
bothelems
  b25/95
dR95/t95/1
dR25/t25/1
bothelems end
noDestr
  b26/t26
dR26/t26/1
noDestr end
paramroot
  b27/t27
dR27/t27/1
paramroot end
twohop
  b31/t31/1
dR31/t31/1
twohop end
outersib
dR82/t82/1
  b32/t32
dR32/t32/1
outersib end
midsib
dR83/t83/1
  b33/t33
dR33/t33/1
midsib end
threehop
  b34/t34
dR34/t34/1
threehop end
deepNoDestr
  b35/t35
dR35/t35/1
deepNoDestr end
done
"#
    );
}

/// B-2026-09-03-32 / B-2026-09-04-23 / B-2026-09-05-25 — a destructure
/// DESTROYS THE FIELDS IT DISCARDS INSIDE THE STATEMENT, before an unread
/// leaf's NLL death; several discards die in reverse declaration order;
/// and a block's per-name move masks die with the block.
///
/// `one` is -32's cell: the wildcard `b` and the never-read `a` both fall
/// due at the destructure, and the frame's LIFO drain ran the leaf first
/// (registered later) where the interpreter destroys the discard while
/// binding the pattern — `dR42 dR142` against `dR142 dR42`. Codegen now
/// fires the consumed source's residual walk inline
/// (`fire_struct_field_bodies_now`) and retracts it. `two` is the mirror
/// (`a: _`), `three` both wildcards (the INTERPRETER's half: it ran them in
/// pattern order, now reverse declaration like the whole-value drop),
/// `four` the later-use control that was always agreed, `five` the
/// binding spelling, `six` the undestructured control, `seven` the
/// by-value param whose walk stays at the callee's exit on every surface,
/// `nine` the `Option`-typed face (-04-23's cell shape).
///
/// `eight` and `ten` are -22: every block here reuses `h`, and the
/// per-name move masks were never dropped at block exit, so a later block
/// destructuring `h` with the OPPOSITE wildcard composed both masks and its
/// residual walk skipped every field — `dR11` was lost outright (clean in
/// isolation, which is how it hid).
#[test]
fn e2e_destructure_discard_dies_in_the_statement_and_masks_die_with_the_block() {
    let Some(out) = run_program(
            "struct R { id: i64, tag: String, xs: Vec[i64] }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             struct Two { a: R, b: R }\n\
             struct Ho2 { a: R, b: Option[R] }\n\
             fn mk(k: i64) -> R { return R { id: k, tag: f\"t{k}\", xs: [k] } }\n\
             fn pDes(h: Two) { let Two { a, b: _ } = h; println(\"in\") }\n\
             fn main() {\n\
             \x20   { let h: Two = Two { a: mk(1), b: mk(101) }; let Two { a, b: _ } = h; println(\"one\") }\n\
             \x20   { let h: Two = Two { a: mk(2), b: mk(102) }; let Two { a: _, b } = h; println(\"two\") }\n\
             \x20   { let h: Two = Two { a: mk(3), b: mk(103) }; let Two { a: _, b: _ } = h; println(\"three\") }\n\
             \x20   { let h: Two = Two { a: mk(4), b: mk(104) }; let Two { a, b: _ } = h; println(f\"use{a.id}\"); println(\"four\") }\n\
             \x20   { let h: Two = Two { a: mk(5), b: mk(105) }; let Two { a, b } = h; println(\"five\") }\n\
             \x20   { let h: Two = Two { a: mk(6), b: mk(106) }; println(\"six\") }\n\
             \x20   { pDes(Two { a: mk(7), b: mk(107) }); println(\"seven\") }\n\
             \x20   { let h: Two = Two { a: mk(8), b: mk(108) }; let Two { a, b: _ } = h; let q: Two = Two { a: mk(9), b: mk(109) }; println(\"eight\") }\n\
             \x20   { let h: Ho2 = Ho2 { a: mk(10), b: Option.Some(mk(110)) }; let Ho2 { a, b: _ } = h; println(\"nine\") }\n\
             \x20   { let h: Two = Two { a: mk(11), b: mk(111) }; let Two { a: _, b } = h; println(\"ten\") }\n\
             \x20   println(\"end\")\n\
             }\n\
             ",
        ) else {
            return;
        };
    assert_eq!(out, "dR101\ndR1\none\ndR2\ndR102\ntwo\ndR103\ndR3\nthree\ndR104\nuse4\ndR4\nfour\ndR105\ndR5\nfive\ndR106\ndR6\nsix\nin\ndR107\ndR7\nseven\ndR108\ndR8\ndR109\ndR9\neight\ndR110\ndR10\nnine\ndR11\ndR111\nten\nend\n");
}

/// B-2026-09-03-12 — a tuple bound out of a PLACE (`let x = h.pe;`) records its
/// element types, so the binding runs the element's `Drop` body and can be
/// projected.
///
/// TWO FAILURES FROM ONE MISSING RECORD, and the row was filed for the smaller.
/// `x.0.id` failed `karac build` with the loud "cannot resolve field ... its type
/// was not recorded for codegen" — a run-vs-build divergence, but one that stops
/// the build. The same absent record ALSO cost the element its body outright:
/// `bound` ran ONE body under `--interp` and ZERO on all three compiled surfaces, a
/// user `Drop` that never fires and says nothing. That half was found by
/// re-measuring the row's own repro with a body that RENDERS, the instrument
/// B-2026-09-02-43 established for this family, and it is why the cells here print
/// a `String` and a `Vec.len()` rather than counting.
///
/// THE NAIVE FIX DOUBLE-FREES, which is what the `bound` cell really guards.
/// Recording the element `TypeExpr`s alone gives this binding a bodies walker while
/// the owning struct's `NestedTuple` drop still frees the same buffers — an
/// immediate `free(): double free detected in tcache 2`. The binding has to take
/// the MEMORY with the bodies, exactly as the DESTRUCTURE spelling of the identical
/// source already does per element. So a future change that keeps the record and
/// drops the cap-zeroing passes a body-count assertion and aborts at runtime; this
/// cell fails instead.
///
/// TWO REGISTRIES, not one, which is why `project` and `bound` are separate cells.
/// The full `TypeExpr`s drive the drop walk; a `TupleIndex` RECEIVER is typed from
/// the parallel per-element NAMES registry, whose `.or_else` chain is this exact
/// family of gaps filed one at a time — annotation, literal, whole rebind
/// (B-2026-09-02-39), call result (B-2026-08-28-3). The place source was the one
/// member never added.
///
/// `literal`, `nobind` and `paramroot` are the over-reach controls: a
/// literal-bound tuple and an unbound chain read both worked before and must not
/// move, and a PARAM root's element body belongs to the caller and must not gain a
/// second owner here. `destr` and `rebound` are the shapes that inherit from this
/// binding, and `deep` is the two-hop source.
///
/// Twin of `tests/interpreter.rs`'s
/// `test_place_source_tuple_binding_records_its_elements`, pinned to the same string.
#[test]
fn e2e_place_source_tuple_binding_records_its_elements() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}/{self.xs.len()}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] }; }
struct H { pe: (R, i64) }
struct G { h: H }

fn u1()      { let h = H { pe: (mk(41), 0) }; let x = h.pe; println("  bound") }
fn u2()      { let h = H { pe: (mk(42), 0) }; let x = h.pe; println(f"  b{x.0.id}/{x.0.tag}") }
fn u3()      { let h = H { pe: (mk(43), 0) }; let x = h.pe; let (a, b) = x; println(f"  b{a.id}/{a.tag}") }
fn u4()      { let g = G { h: H { pe: (mk(44), 0) } }; let x = g.h.pe; println(f"  b{x.0.id}/{x.0.tag}") }
fn u5()      { let t = (mk(45), 0); let x = t; println(f"  b{x.0.id}/{x.0.tag}") }
fn u6()      { let h = H { pe: (mk(46), 0) }; println(f"  b{h.pe.0.id}/{h.pe.0.tag}") }
fn u7(h: H)  { let x = h.pe; println(f"  b{x.0.id}/{x.0.tag}") }
fn u8()      { let h = H { pe: (mk(48), 0) }; let x = h.pe; let y = x; println(f"  b{y.0.id}/{y.0.tag}") }

fn main() {
    println("bound");     u1();                      println("bound end")
    println("project");   u2();                      println("project end")
    println("destr");     u3();                      println("destr end")
    println("deep");      u4();                      println("deep end")
    println("literal");   u5();                      println("literal end")
    println("nobind");    u6();                      println("nobind end")
    println("paramroot"); u7(H { pe: (mk(47), 0) }); println("paramroot end")
    println("rebound");   u8();                      println("rebound end")
    println("done")
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"bound
dR41/t41/1
  bound
bound end
project
  b42/t42
dR42/t42/1
project end
destr
  b43/t43
dR43/t43/1
destr end
deep
  b44/t44
dR44/t44/1
deep end
literal
  b45/t45
dR45/t45/1
literal end
nobind
  b46/t46
dR46/t46/1
nobind end
paramroot
  b47/t47
dR47/t47/1
paramroot end
rebound
  b48/t48
dR48/t48/1
rebound end
done
"#
    );
}

/// B-2026-09-02-39 — A TUPLE PARAM'S ELEMENT TYPES WERE NEVER RECORDED, so
/// any element a NAME cannot spell was invisible to codegen.
///
/// `tuple_var_elem_type_exprs` — the registry `place_chain_tuple_tes` prefers
/// and every tuple-element consumer reads through — was populated from
/// exactly ONE place: a tuple ANNOTATION on a `let`. A by-value tuple PARAM
/// has a full declared type and registered nothing; a whole rebind and a
/// destructure leaf inherited nothing. The names-derived fallback covers a
/// FLAT element by name and renders anything a name cannot spell — a NESTED
/// tuple above all — as an EMPTY `Path`, which every consumer reads as "no
/// such leaf" and skips.
///
/// `param` / `rebind` / `leaf` all FAILED TO LOWER before this fix, with
/// `cannot resolve field 'id' on this receiver (its type was not recorded for
/// codegen)`, while `--interp` printed every one of them.
///
/// THE CONTROLS ARE WHAT LOCALIZED THE FAULT, and they are the reason the fix
/// is a registration rather than anything downstream:
/// - `flat` — `t.0.id` on a flat tuple param. Always worked; the NAME
///   spelling suffices for a single-segment path.
/// - `annotated` — the identical nested read off an ANNOTATED LOCAL. Already
///   lowered and printed BEFORE the fix, which proves the whole consumer
///   chain handles a nested element correctly once the `TypeExpr`s are
///   present. An annotation is therefore also a working user-side workaround.
///
/// `reuse_param` / `reuse_local` are the hygiene half, and they are not
/// decoration. `tuple_var_elem_tes()` prefers this registry WHOLESALE, so a
/// stale entry does not merely add detail — it WINS over the next function's
/// correct names-derived spelling. Nothing cleared the registry per function;
/// the leak predates this row (the annotated-`let` site has always written
/// there), but registering every tuple param turns a shape you had to
/// construct on purpose into one any two functions sharing a param name would
/// hit. These two cells reuse the name `t` at a DIFFERENT tuple type and must
/// both print their own.
///
/// Twin of `tests/interpreter.rs`'s
/// `test_tuple_param_element_types_are_recorded`, pinned to the same string —
/// and the interpreter was right throughout, so that twin is the oracle this
/// backend converged to rather than a second pin on new behaviour.
#[test]
fn e2e_tuple_param_element_types_are_recorded() {
    let Some(out) = run_program(
        r#"struct R { id: i64 }
struct Z { tag: String }

// FAILING TODAY
fn q1(t: ((R, i64), i64)) { println(f"q1 {t.0.0.id}") }
fn q2(t: (R, i64))        { let t2 = t; println(f"q2 {t2.0.id}") }
fn q3(t: ((R, i64), i64)) { let (inner, y) = t; println(f"q3 {inner.0.id}") }
// CONTROLS THAT WORK
fn q4(t: (R, i64))        { println(f"q4 {t.0.id}") }
fn q5() { let t: ((R, i64), i64) = ((R { id: 5 }, 0), 0); println(f"q5 {t.0.0.id}") }
// CROSS-FUNCTION NAME REUSE: `t` is a DIFFERENT tuple type here
fn q6(t: (Z, i64))        { println(f"q6 {t.0.tag}") }
fn q7() { let t = (Z { tag: "z7" }, 0); println(f"q7 {t.0.tag}") }

fn main() {
    q1(((R { id: 1 }, 0), 0))
    q2((R { id: 2 }, 0))
    q3(((R { id: 3 }, 0), 0))
    q4((R { id: 4 }, 0))
    q5()
    q6((Z { tag: "z6" }, 0))
    q7()
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"q1 1
q2 2
q3 3
q4 4
q5 5
q6 z6
q7 z7
"#
    );
}

/// B-2026-08-01-12 — a struct destructure of an OWNED param
/// (`let Holder { r } = h;` inside the callee) binds views of the
/// callee's entry copy: the conceptual value's Drop body fires
/// CALLER-side, once, at the arg's statement end (identifier arg: the
/// caller binding's NLL fire; fresh literal arg: the fresh-arg temp
/// fire). Codegen already behaved this way — this pin is the parity
/// target the interpreter's new destructure gate meets (pre-fix
/// `karac run` fired the bound field's body a second time inside the
/// callee). Twin of `tests/interpreter.rs`'s
/// `test_param_struct_destructure_single_caller_fire`.
#[test]
fn e2e_param_struct_destructure_single_caller_fire() {
    let Some(out) = run_program(
        "struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"drop {self.id} {self.name}\")\n\
             \x20   }\n\
             }\n\
             struct Holder { r: Res }\n\
             fn take(h: Holder) {\n\
             \x20   let Holder { r } = h;\n\
             \x20   println(f\"got {r.id}\");\n\
             \x20   println(\"take done\");\n\
             }\n\
             fn main() {\n\
             \x20   println(\"a\");\n\
             \x20   let x = Holder { r: Res { id: 5, name: f\"y{5}\" } };\n\
             \x20   take(x);\n\
             \x20   println(\"b\");\n\
             \x20   take(Holder { r: Res { id: 7, name: f\"y{7}\" } });\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(
        out,
        "a\ngot 5\ntake done\ndrop 5 y5\nb\ngot 7\ntake done\ndrop 7 y7\nend\n"
    );
}

/// B-2026-08-27-48 — the compiled-backend parity target for a struct
/// DESTRUCTURED out of an owned TUPLE param: the user `Drop` body fires
/// once, caller-side. Codegen already behaved this way; the interpreter
/// fired a second time inside the callee (`drop 41` twice under
/// `karac run --interp`, once here). Pinning it so the two backends stay
/// joined on all three argument shapes — fresh tuple literal, place
/// argument, and a place argument spelled like the callee's own param.
/// Twin of `tests/interpreter.rs`'s
/// `test_tuple_param_destructure_single_caller_fire`.
#[test]
fn e2e_tuple_param_destructure_single_caller_fire() {
    let Some(out) = run_program(
        "struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"drop {self.id} {self.name}\")\n\
             \x20   }\n\
             }\n\
             fn take(p: (Res, i64)) {\n\
             \x20   let (r, n) = p;\n\
             \x20   println(f\"got {r.id} {n}\");\n\
             \x20   println(\"take done\");\n\
             }\n\
             fn main() {\n\
             \x20   println(\"a\");\n\
             \x20   take((Res { id: 5, name: f\"y{5}\" }, 1));\n\
             \x20   println(\"b\");\n\
             \x20   let q = (Res { id: 7, name: f\"y{7}\" }, 2);\n\
             \x20   take(q);\n\
             \x20   println(\"c\");\n\
             \x20   let p = (Res { id: 9, name: f\"y{9}\" }, 3);\n\
             \x20   take(p);\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "a\ngot 5 1\ntake done\ndrop 5 y5\nb\ngot 7 2\ntake done\ndrop 7 y7\nc\ngot 9 3\ntake done\ndrop 9 y9\nend\n");
}

/// B-2026-08-27-48, method leg — the parity target for the same
/// destructure inside an IMPL METHOD, tuple and struct pattern alike.
/// Both fire once here, which is what made the interpreter's zero-fire
/// method shapes visible as a divergence. Twin of
/// `tests/interpreter.rs`'s
/// `test_method_param_destructure_single_caller_fire`.
#[test]
fn e2e_method_param_destructure_single_caller_fire() {
    let Some(out) = run_program(
        "struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"drop {self.id} {self.name}\")\n\
             \x20   }\n\
             }\n\
             struct Holder { r: Res }\n\
             struct W { v: i64 }\n\
             impl W {\n\
             \x20   fn tup(ref self, p: (Res, i64)) {\n\
             \x20       let (r, n) = p;\n\
             \x20       println(f\"tup {r.id} {n} {self.v}\");\n\
             \x20   }\n\
             \x20   fn strc(ref self, h: Holder) {\n\
             \x20       let Holder { r } = h;\n\
             \x20       println(f\"strc {r.id} {self.v}\");\n\
             \x20   }\n\
             }\n\
             fn main() {\n\
             \x20   let w = W { v: 100 };\n\
             \x20   println(\"a\");\n\
             \x20   w.tup((Res { id: 5, name: f\"y{5}\" }, 1));\n\
             \x20   println(\"b\");\n\
             \x20   w.strc(Holder { r: Res { id: 7, name: f\"y{7}\" } });\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(
        out,
        "a\ntup 5 1 100\ndrop 5 y5\nb\nstrc 7 100\ndrop 7 y7\nend\n"
    );
}

/// B-2026-08-01-23 — container-in-container element Drop bodies:
/// `Vec[Vec[Res]]` inner elements (the te-driven recursive
/// `__karac_dropelems_vecof_*` walker) and `Map[i64, Vec[Res]]`
/// value-Vec elements (the map walker's nested-value arm) fire at the
/// outer binding's death — pre-fix both were silent on both backends
/// (memory freed). Twin of `tests/interpreter.rs`'s
/// `test_nested_container_elem_bodies`.
#[test]
fn e2e_nested_container_elem_bodies() {
    let Some(out) = run_program(
        "struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"drop {self.id} {self.name}\")\n\
             \x20   }\n\
             }\n\
             fn main() {\n\
             \x20   println(\"a\");\n\
             \x20   let mut vv: Vec[Vec[Res]] = Vec.new();\n\
             \x20   let mut inner: Vec[Res] = Vec.new();\n\
             \x20   inner.push(Res { id: 7, name: f\"q{7}\" });\n\
             \x20   vv.push(inner);\n\
             \x20   println(\"b\");\n\
             \x20   let mut m: Map[i64, Vec[Res]] = Map.new();\n\
             \x20   let mut mi: Vec[Res] = Vec.new();\n\
             \x20   mi.push(Res { id: 8, name: f\"r{8}\" });\n\
             \x20   let _ = m.insert(1, mi);\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "a\ndrop 7 q7\nb\ndrop 8 r8\nend\n");
}

/// B-2026-08-25-2 — a `std.cli` type NAMED in a struct field, a function
/// return type, and a binding, with its `String` fields read back.
///
/// `std.cli`'s layouts are declared for every program (see
/// `layout_only_stdlib_programs`), so `Parser` lowers at its real six-field
/// shape instead of silently collapsing to `i64`. Before that, this program
/// passed `karac check` and failed `karac build` with `cannot resolve field
/// 'program_name' on this receiver (its type was not recorded for
/// codegen)`, while `--interp` printed the right answer.
///
/// Reading the fields BACK is what gives this teeth. A variant that merely
/// names `Parser` and never touches it built fine even against the broken
/// compiler — every reference was `i64`-wide and consistently so, so the
/// collapse was unobservable. Any regression test for this class has to
/// round-trip a field's value.
#[test]
fn e2e_stdlib_cli_type_named_but_uncalled_lowers_at_real_layout() {
    let Some(out) = run_program(
        "struct Holder { p: Parser, n: i64 }\n\
             fn mk(nm: String) -> Parser {\n\
             \x20   return Parser { program_name: nm, about_text: \"abt\",\n\
             \x20       version_text: \"1.0\", args: Vec.new(), flags: Vec.new(),\n\
             \x20       subcommands: Vec.new() };\n\
             }\n\
             fn name_of(h: Holder) -> String { return h.p.program_name; }\n\
             fn main() {\n\
             \x20   let h = Holder { p: mk(\"demo\"), n: 7 };\n\
             \x20   println(name_of(h));\n\
             \x20   let p2 = mk(\"second\");\n\
             \x20   println(p2.about_text);\n\
             \x20   println(p2.version_text);\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "demo\nabt\n1.0\n");
}

/// A tuple param returned WHOLE, bound to a local, then destructured at the
/// call site runs its element's user `Drop` body (B-2026-08-28-18).
///
/// Both compiled backends printed NOTHING for this shape while the
/// interpreter printed one body — a genuine run-vs-build divergence, unlike
/// most of the family. The passthrough half was never at fault:
/// `fn_returns_param` is TRUE here, so the caller-side fresh-temp walk
/// correctly declines and hands ownership to the caller's consumer. The loss
/// was downstream, at the destructure of the LOCAL holding the call result.
///
/// FIXED INCIDENTALLY by B-2026-08-28-3 (`0f058f1`), which taught the
/// let-site to resolve a tuple element's struct type when the tuple came
/// from a CALL — exactly the missing piece, since a leaf whose type cannot
/// be named registers no body. Bisected to that boundary (`919f31e` before
/// it still prints nothing), and pinned here because nothing else does: -3's
/// own fixtures assert the RESOLUTION, not this body count, so the shape
/// would have been free to regress silently.
///
/// `passthrough` is the row's repro. `builds-its-own` is the control the row
/// used to isolate it — the same destructure of a call result whose callee
/// CONSTRUCTS the tuple was always correct on all three backends, so the
/// defect needed the callee to have RECEIVED the tuple as a param and passed
/// it through, not merely to have returned one. Keeping both is what
/// distinguishes "the destructure lost the leaf" from "the passthrough guard
/// dropped the value".
#[test]
fn e2e_passthrough_tuple_destructured_at_the_call_site_runs_its_body() {
    const DROPPER: &str = "struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id}\"); } }\n";
    for (label, body, want) in [
        // The row's own repro.
        (
            "passthrough",
            "fn take(p: (R, i64)) -> (R, i64) { p }\n\
                 fn main() { let x = take((R { id: 41 }, 1)); let (r, n) = x;\n\
                 \x20            println(f\"{r.id}\"); }\n",
            "41\ndrop 41\n",
        ),
        // The same shape destructured DIRECTLY off the call, with no local
        // in between.
        (
            "passthrough-no-local",
            "fn take(p: (R, i64)) -> (R, i64) { p }\n\
                 fn main() { let (r, n) = take((R { id: 42 }, 1)); println(f\"{r.id}\"); }\n",
            "42\ndrop 42\n",
        ),
        // CONTROL — the callee BUILDS the tuple rather than passing one
        // through. Always correct, and what isolates the defect to the
        // passthrough.
        (
            "builds-its-own",
            "fn make() -> (R, i64) { return (R { id: 43 }, 1); }\n\
                 fn main() { let y = make(); let (a, b) = y; println(f\"{a.id}\"); }\n",
            "43\ndrop 43\n",
        ),
        // CONTROL — the STRUCT spelling of the passthrough, which shares the
        // let-site resolution the fix touched.
        (
            "struct-passthrough",
            "struct W { r: R, n: i64 }\n\
                 fn take(w: W) -> W { w }\n\
                 fn main() { let x = take(W { r: R { id: 44 }, n: 1 }); let W { r, n } = x;\n\
                 \x20            println(f\"{r.id}\"); }\n",
            "44\ndrop 44\n",
        ),
    ] {
        let prog = format!("{DROPPER}{body}");
        assert_eq!(run_program(&prog).as_deref(), Some(want), "{label}");
    }
}

/// B-2026-08-28-23 — a NESTED projection out of an owned struct param runs
/// the escaping field's body once.
///
/// The two fixtures above mask a TOP-LEVEL field. `fn take(w: W) -> R {
/// w.inner.r }` names something one level further in, and the analysis
/// behind both — `fn_returns_param_part_paths` — classified a projection only
/// off the WHOLE param and declined a projection-of-a-projection by
/// construction, so nothing was masked and the pre-existing double body
/// survived. Closing it widened the ANALYSIS to report a PATH, and both
/// caller-side masks to take a tree instead of a flat index set.
///
/// `sibling` is the row that forbids the cheap fix. Reporting the one-level
/// PREFIX (`inner`) instead of the path would mask the whole subtree, and
/// `s` — which really does die inside the call — would lose its only body.
/// That is a false escape, the direction the analysis is explicitly built to
/// avoid, so the prefix is not an approximation of the path but a different
/// and wrong answer.
///
/// `nested-destructure` came along for free and is pinned because it did:
/// the same whole-param gate made `let W { inner, n } = w; let I { r } =
/// inner; r` unclassifiable, and carrying paths through the alias table
/// fixes both shapes with one change.
///
/// `own-drop-mid` checks the intermediate's OWN body still fires — the mask
/// removes a leaf from the walk, not the level it hangs off — and
/// `whole-inner` is the one-level control that was already correct and must
/// stay so: it is the answer the widening had to leave untouched while
/// adding the deeper one.
#[test]
fn e2e_nested_projection_runs_the_returned_fields_body_once() {
    const DROPPER: &str = "struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id}\"); } }\n\
             struct I { r: R }\n\
             struct W { inner: I, n: i64 }\n";
    for (label, body, want) in [
        // The row's own repro.
        (
            "nested-projection",
            "fn take(w: W) -> R { w.inner.r }\n\
                 fn main() { let x = take(W { inner: I { r: R { id: 41 } }, n: 1 });\n\
                 \x20            println(f\"{x.id}\"); }\n",
            "41\ndrop 41\n",
        ),
        // A SIBLING of the escaping field dies in the call and keeps its
        // body — what makes the mask a path rather than its prefix.
        (
            "sibling",
            "struct I2 { r: R, s: R }\n\
                 struct W2 { inner: I2, n: i64 }\n\
                 fn take(w: W2) -> R { w.inner.r }\n\
                 fn main() { let x = take(W2 { inner: I2 { r: R { id: 42 }, s: R { id: 52 } },\n\
                 \x20                          n: 2 });\n\
                 \x20            println(f\"{x.id}\"); }\n",
            "drop 52\n42\ndrop 42\n",
        ),
        // The destructure spelling of the same path, fixed by the same
        // change.
        (
            "nested-destructure",
            "fn take(w: W) -> R { let W { inner, n } = w; let I { r } = inner; r }\n\
                 fn main() { let x = take(W { inner: I { r: R { id: 43 } }, n: 3 });\n\
                 \x20            println(f\"{x.id}\"); }\n",
            "43\ndrop 43\n",
        ),
        // The INTERMEDIATE declares its own `Drop` — masking a leaf under it
        // must not silence the level itself.
        (
            "own-drop-mid",
            "struct Id { r: R }\n\
                 impl Drop for Id { fn drop(mut ref self) { println(\"drop Id\"); } }\n\
                 struct Wd { inner: Id, n: i64 }\n\
                 #[allow(partial_move_of_drop_struct)]\n\
                 fn take(w: Wd) -> R { w.inner.r }\n\
                 fn main() { let x = take(Wd { inner: Id { r: R { id: 44 } }, n: 4 });\n\
                 \x20            println(f\"{x.id}\"); }\n",
            "drop Id\n44\ndrop 44\n",
        ),
        // CONTROL — a ONE-level projection, already correct before this and
        // the answer the length-1 filter preserves.
        (
            "whole-inner",
            "fn take(w: W) -> I { w.inner }\n\
                 fn main() { let x = take(W { inner: I { r: R { id: 45 } }, n: 5 });\n\
                 \x20            println(f\"{x.r.id}\"); }\n",
            "45\ndrop 45\n",
        ),
        // CONTROL — nothing escapes, so the body belongs in the call.
        (
            "nothing-escapes-control",
            "fn take(w: W) -> i64 { w.n }\n\
                 fn main() { let g = take(W { inner: I { r: R { id: 46 } }, n: 6 });\n\
                 \x20            println(f\"{g}\"); }\n",
            "drop 46\n6\n",
        ),
    ] {
        let prog = format!("{DROPPER}{body}");
        assert_eq!(run_program(&prog).as_deref(), Some(want), "{label}");
    }
}

/// B-2026-08-25-35 — the end-to-end payoff: a user struct in a `PriorityQueue`.
///
/// Needs BOTH halves of that row's fix. The bound gate above lets `Task` in at
/// all; then `outranks` compares two INDEXED elements (`self.xs[i] > self.xs[j]`),
/// and resolving that element's type name walked the DECLARED field type — `Vec[T]`
/// on `struct PriorityQueue[=T]` — yielding `T`, the impl's type PARAMETER rather
/// than the monomorph's argument. `T` names no struct, so the ordered-comparison
/// dispatch declined and codegen failed with "Unsupported struct binary op: Gt".
/// Same class as B-2026-08-25-28: a type expr left in terms of the impl's parameter
/// inside a monomorph.
///
/// Both directions and `peek`/`pop`/`len` appear because `outranks` is the one
/// branch that differs between them, and because a comparison that silently read
/// index 0 without the heap property holding would still look right on a min-first
/// queue of three.
///
/// Twin of `tests/interpreter.rs`'s
/// `test_priority_queue_of_a_derived_ord_struct`. Sized worker for the same
/// reason the sibling PriorityQueue E2Es state: on-demand monomorphization
/// recurses through the callee chain.
#[test]
fn e2e_priority_queue_of_a_derived_ord_struct() {
    let src = r#"
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Task { pri: i64, id: i64 }
fn main() {
    let mut q: PriorityQueue[Task] = PriorityQueue.new();
    q.push(Task { pri: 3, id: 30 });
    q.push(Task { pri: 1, id: 10 });
    q.push(Task { pri: 2, id: 20 });
    match q.peek() { Some(t) => { println(t.id); } None => {} }
    println(q.len());
    while q.len() > 0 {
        match q.pop() { Some(t) => { println(t.id); } None => {} }
    }
    let mut m: PriorityQueue[Task] = PriorityQueue.max_first();
    m.push(Task { pri: 3, id: 30 });
    m.push(Task { pri: 1, id: 10 });
    match m.peek() { Some(t) => { println(t.id); } None => {} }
}
"#;
    let out = std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(move || run_program(src))
        .expect("failed to spawn sized worker")
        .join()
        .expect("compile worker panicked");
    assert_eq!(out.as_deref(), Some("10\n3\n10\n20\n30\n30\n"));
}

/// B-2026-08-01-20 — a FIELD-assign displaces the old field value,
/// whose Drop bodies now fire before the store (both backends were
/// silent; the memory side always freed). Struct fields with Drop
/// work and user value-enum fields alike; the binding-target
/// displacement has fired since the B-2026-07-30-11 arc. Twin of
/// `tests/interpreter.rs`'s `test_field_assign_displaced_bodies`.
#[test]
fn e2e_field_assign_displaced_bodies() {
    let Some(out) = run_program(
        "struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"drop {self.id} {self.name}\")\n\
             \x20   }\n\
             }\n\
             struct Holder { r: Res }\n\
             struct Outer { h: Holder }\n\
             fn main() {\n\
             \x20   println(\"a\");\n\
             \x20   let mut o = Outer { h: Holder { r: Res { id: 9, name: f\"z{9}\" } } };\n\
             \x20   o.h = Holder { r: Res { id: 5, name: f\"y{5}\" } };\n\
             \x20   println(f\"held {o.h.r.id}\");\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "a\ndrop 9 z9\nheld 5\ndrop 5 y5\nend\n");
}

/// B-2026-08-01-30 leg A — a DEEP-CHAIN field-assign (`o.h.r = <new>`)
/// displaces the old field value, whose Drop body now fires before the
/// store (pre-fix: silent on both backends — the -20 emitter was
/// Identifier-base only) and whose heap frees (pre-fix: the nested
/// plain-parent store was a BARE overwrite; the leak was DCE-masked for
/// write-only old values and real for any read one). Twin of
/// `tests/interpreter.rs`'s `test_deep_chain_field_assign_displaced_bodies`;
/// the memory side is pinned by `tests/memory_sanitizer.rs`'s
/// `asan_nested_field_store_displaced_old_freed`.
#[test]
fn e2e_deep_chain_field_assign_displaced_bodies() {
    let Some(out) = run_program(
        "struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"drop {self.id} {self.name}\")\n\
             \x20   }\n\
             }\n\
             struct Holder { r: Res }\n\
             struct Outer { h: Holder }\n\
             fn main() {\n\
             \x20   println(\"a\");\n\
             \x20   let mut o = Outer { h: Holder { r: Res { id: 9, name: f\"z{9}\" } } };\n\
             \x20   o.h.r = Res { id: 5, name: f\"y{5}\" };\n\
             \x20   println(f\"held {o.h.r.id}\");\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "a\ndrop 9 z9\nheld 5\ndrop 5 y5\nend\n");
}

/// B-2026-08-01-35 — a field store through a FIELD-ROOTED indexed
/// container (`o.hs[i].field = x`, the Vec itself a struct field) was
/// SILENTLY DROPPED under karac build: `nested_store_place_ptr`'s
/// Index arm resolved bare-Identifier containers only, so the store
/// exited through compile_field_store's no-op tail while the
/// interpreter applied the write (stale reads, no diagnostic, no
/// crash). Scalar, String (variable index), and struct fields alike;
/// the struct-field leg also pins the displaced old value's memory
/// release (drop 5 y5 is the NEW value at o's NLL death — the old z9
/// buffer frees silently via the nested store's old-value drop). Twin
/// of `tests/interpreter.rs`'s
/// `test_field_rooted_indexed_container_field_store`.
#[test]
fn e2e_field_rooted_indexed_container_field_store() {
    let Some(out) = run_program(
        "struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"drop {self.id} {self.name}\")\n\
             \x20   }\n\
             }\n\
             struct Hi { r: Res }\n\
             struct Oi { hs: Vec[Hi] }\n\
             struct Ps { id: i64, name: String }\n\
             struct Os { hs: Vec[Ps] }\n\
             fn main() {\n\
             \x20   println(\"a\");\n\
             \x20   let mut o = Oi { hs: Vec.new() };\n\
             \x20   o.hs.push(Hi { r: Res { id: 9, name: f\"z{9}\" } });\n\
             \x20   o.hs[0].r = Res { id: 5, name: f\"y{5}\" };\n\
             \x20   println(f\"held {o.hs[0].r.id}\");\n\
             \x20   let mut p = Os { hs: Vec.new() };\n\
             \x20   p.hs.push(Ps { id: 9, name: f\"z{9}\" });\n\
             \x20   p.hs[0].id = 4;\n\
             \x20   let i = 0;\n\
             \x20   p.hs[i].name = f\"w{6}\";\n\
             \x20   println(f\"ps {p.hs[0].id} {p.hs[0].name}\");\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "a\nheld 5\ndrop 5 y5\nps 4 w6\nend\n");
}

/// B-2026-08-02-10 — methods on TUPLE-ELEMENT receivers (`t.0.push(x)`,
/// `t.0.len()`) loud-bailed under karac build while the interpreter ran
/// them. compile_method_call's fall-through now re-dispatches through a
/// synth identifier registered from the binding's ANNOTATED element
/// TypeExpr (the new tuple_var_elem_type_exprs registry — the names
/// registry erases generic args and is rejected as a synth source).
/// Unannotated bindings keep a loud, actionable hint. Twin of
/// `tests/interpreter.rs`'s `test_tuple_elem_method_receivers`.
#[test]
fn e2e_tuple_elem_method_receivers() {
    let Some(out) = run_program(
        "fn main() {\n\
             \x20   let v: Vec[i64] = Vec.new();\n\
             \x20   let mut t: (Vec[i64], i64) = (v, 3);\n\
             \x20   t.0.push(10);\n\
             \x20   t.0.push(20);\n\
             \x20   println(f\"len {t.0.len()} v0 {t.0[0]} v1 {t.0[1]}\");\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "len 2 v0 10 v1 20\nend\n");
}

/// B-2026-08-01-19 — storing an owned param into a local container
/// FIELD (`o.h = h;`) fired the caller-retained value's body TWICE on
/// both backends (o's bodies walk at its death + the caller's NLL
/// fire). The field-store now retracts the base binding's bodies
/// action, leaving exactly the caller's single fire; o's memory
/// registration survives so the moved copy's heap still frees. The
/// displaced old field's body (`drop 9 z9`) fires per B-2026-08-01-20 —
/// the displaced value is a real local, distinct from the param. Twin of
/// `tests/interpreter.rs`'s `test_param_field_store_single_caller_fire`.
#[test]
fn e2e_param_field_store_single_caller_fire() {
    let Some(out) = run_program(
        "struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"drop {self.id} {self.name}\")\n\
             \x20   }\n\
             }\n\
             struct Holder { r: Res }\n\
             struct Outer { h: Holder }\n\
             fn take(h: Holder) {\n\
             \x20   let mut o = Outer { h: Holder { r: Res { id: 9, name: f\"z{9}\" } } };\n\
             \x20   o.h = h;\n\
             \x20   println(f\"held {o.h.r.id}\");\n\
             \x20   println(\"take done\");\n\
             }\n\
             fn main() {\n\
             \x20   println(\"a\");\n\
             \x20   let x = Holder { r: Res { id: 5, name: f\"y{5}\" } };\n\
             \x20   take(x);\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "a\ndrop 9 z9\nheld 5\ntake done\ndrop 5 y5\nend\n");
}

/// B-2026-09-06-34 — the fields a `..` rest covers in a struct `let`
/// destructure are discarded leaves on the terms an explicit `_` field
/// already is: dropped at the destructure, in reverse declaration order.
/// The interpreter never gave them a slot (`dR9 dR8` for `let S3 { a, .. }
/// = s` against `dR10 dR9 dR8` compiled); codegen lost them over a FRESH
/// source (`lit_a`, `call_a`: `mid dR5` while `lit_wild` ran the body). A
/// param-view field (`view_a`, `view_w`) stays the caller's body. The
/// param cell reads nothing off its leaf and the pin names first-declared
/// fields only — see B-2026-09-06-33 and the crash row filed with this one.
///
/// Twin of `tests/interpreter.rs`'s `test_struct_destructure_rest_fields_run_their_bodies_once`, pinned to the same string.
#[test]
fn e2e_struct_destructure_rest_fields_run_their_bodies_once() {
    let Some(out) = run_program(
        r#"struct R { id: i64, name: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"n{i}", xs: [i] }; }
struct S3 { a: R, b: R }
struct S4 { a: R, b: R, c: R, n: i64 }
fn mks(i: i64) -> S3 { return S3 { a: mk(i), b: mk(i + 1) }; }

fn local_a(i: i64) -> i64 { let s = S3 { a: mk(i), b: mk(i + 1) }; let S3 { a, .. } = s; println("  mid"); return a.id; }
fn local_ab(i: i64) -> i64 { let s = S4 { a: mk(i), b: mk(i + 1), c: mk(i + 2), n: 4 }; let S4 { a, b, .. } = s; println("  mid"); return a.id + b.id; }
fn local_all_rest(i: i64) -> i64 { let s = S3 { a: mk(i), b: mk(i + 1) }; let S3 { .. } = s; println("  mid"); return 1; }
fn local_wild(i: i64) -> i64 { let s = S3 { a: mk(i), b: mk(i + 1) }; let S3 { a, b: _ } = s; println("  mid"); return a.id; }
fn lit_a(i: i64) -> i64 { let S3 { a, .. } = S3 { a: mk(i), b: mk(i + 1) }; println("  mid"); return a.id; }
fn lit_wild(i: i64) -> i64 { let S3 { a, b: _ } = S3 { a: mk(i), b: mk(i + 1) }; println("  mid"); return a.id; }
fn call_a(i: i64) -> i64 { let S3 { a, .. } = mks(i); println("  mid"); return a.id; }
fn param_a(s: S3) -> i64 { let S3 { a, .. } = s; println("  mid"); return 1; }
fn view_w(r: R) -> i64 { let s = S3 { a: mk(92), b: r }; let S3 { a, b: _ } = s; println("  mid"); return a.id; }
fn view_a(r: R) -> i64 { let s = S3 { a: mk(90), b: r }; let S3 { a, .. } = s; println("  mid"); return a.id; }

fn main() {
    println("local_a"); let v1 = local_a(1); println(f"  v={v1}");
    println("local_ab"); let v2 = local_ab(10); println(f"  v={v2}");
    println("local_all_rest"); let v3 = local_all_rest(20); println(f"  v={v3}");
    println("local_wild"); let v4 = local_wild(30); println(f"  v={v4}");
    println("lit_a"); let v5 = lit_a(40); println(f"  v={v5}");
    println("lit_wild"); let v6 = lit_wild(50); println(f"  v={v6}");
    println("call_a"); let v7 = call_a(60); println(f"  v={v7}");
    println("param_a"); let v8 = param_a(S3 { a: mk(70), b: mk(71) }); println(f"  v={v8}");
    println("view_a"); let v9 = view_a(mk(80)); println(f"  v={v9}");
    println("view_w"); let v10 = view_w(mk(82)); println(f"  v={v10}");
    println("end");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"local_a
  dR2
  mid
  dR1
  v=1
local_ab
  dR12
  mid
  dR11
  dR10
  v=21
local_all_rest
  dR21
  dR20
  mid
  v=1
local_wild
  dR31
  mid
  dR30
  v=30
lit_a
  dR41
  mid
  dR40
  v=40
lit_wild
  dR51
  mid
  dR50
  v=50
call_a
  dR61
  mid
  dR60
  v=60
param_a
  mid
  dR71
  dR70
  v=1
view_a
  mid
  dR90
  dR80
  v=90
view_w
  mid
  dR92
  dR82
  v=92
end
"#
    );
}

/// B-2026-09-06-41 — reading a scalar field off a leaf destructured out of
/// a by-value param whose type has its own `Drop` (`let S3 { a, b } = s;
/// return b.id;`) panicked under `--interp` inside `R.drop`: the caller's
/// fresh-temp walk masked the returned part `s.b.id` into the argument
/// value and the body then read a missing field. Codegen was right on
/// every cell; this pins the compiled side against the interpreter twin
/// so the caller-retains placement (`mid`, then the fields in reverse
/// declaration order) stays agreed on the destructure, rest, wildcard,
/// scalar-read, `String`-length, whole-field and named-argument spellings.
///
/// Twin of `tests/interpreter.rs`'s `test_scalar_read_off_a_param_destructure_leaf_does_not_mask_the_walk`, pinned to the same string.
#[test]
fn e2e_scalar_read_off_a_param_destructure_leaf_does_not_mask_the_walk() {
    let Some(out) = run_program(
        r#"struct R { id: i64, name: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"n{i}", xs: [i] }; }
struct S3 { a: R, b: R }

fn full_ret(s: S3) -> i64 { let S3 { a, b } = s; println("  mid"); return b.id; }
fn full_let(s: S3) -> i64 { let S3 { a, b } = s; println("  mid"); let x = a.id + b.id; return x; }
fn rest_ret(s: S3) -> i64 { let S3 { a, .. } = s; println("  mid"); return a.id; }
fn wild_ret(s: S3) -> i64 { let S3 { a, b: _ } = s; println("  mid"); return a.id; }
fn rest_len(s: S3) -> i64 { let S3 { a, .. } = s; println("  mid"); let n = a.name.len(); return n; }
fn rest_none(s: S3) -> i64 { let S3 { a, .. } = s; println("  mid"); return 1; }
fn whole_b(s: S3) -> R { let S3 { a, b } = s; println("  mid"); return b; }

fn main() {
    println("full_ret"); let v1 = full_ret(S3 { a: mk(1), b: mk(2) }); println(f"  v={v1}");
    println("full_let"); let v2 = full_let(S3 { a: mk(3), b: mk(4) }); println(f"  v={v2}");
    println("rest_ret"); let v3 = rest_ret(S3 { a: mk(5), b: mk(6) }); println(f"  v={v3}");
    println("wild_ret"); let v4 = wild_ret(S3 { a: mk(7), b: mk(8) }); println(f"  v={v4}");
    println("rest_len"); let v5 = rest_len(S3 { a: mk(9), b: mk(10) }); println(f"  v={v5}");
    println("rest_none"); let v6 = rest_none(S3 { a: mk(11), b: mk(12) }); println(f"  v={v6}");
    println("whole_b"); let r7 = whole_b(S3 { a: mk(13), b: mk(14) }); println(f"  v={r7.id}");
    println("named"); let s8 = S3 { a: mk(15), b: mk(16) }; let v8 = full_ret(s8); println(f"  v={v8}");
    println("end");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"full_ret
  mid
  dR2
  dR1
  v=2
full_let
  mid
  dR4
  dR3
  v=7
rest_ret
  mid
  dR6
  dR5
  v=5
wild_ret
  mid
  dR8
  dR7
  v=7
rest_len
  mid
  dR10
  dR9
  v=2
rest_none
  mid
  dR12
  dR11
  v=1
whole_b
  mid
  dR13
  v=14
  dR14
named
  mid
  dR16
  dR15
  v=16
end
"#
    );
}

/// B-2026-09-06-45 — a rebind of `self` NESTED in a branch of an
/// owned-`self` method ran the receiver's own `Drop` body twice on every
/// surface: the local owns the receiver on the path that rebinds and runs
/// its bodies, while the caller's retained walk ran them again
/// (`dE dR1 dE` for an own-`Drop` enum receiver, `dS1 dR12 dS1 dR12` for a
/// struct). B-2026-09-06-42's stand-down is top-level only on purpose,
/// because a nested rebind leaves the receiver with the caller on the other
/// paths and an unconditional stand-down would LOSE the body there. Both
/// halves now land together: the caller stands down for the whole call and
/// the callee frame carries the receiver's own wrapper, guarded by the
/// per-path flag the rebind clears. Pins the match, bare, `let mut`,
/// loop-nested, arm-nested, both-branch and struct spellings on both paths,
/// with the fresh-temp receiver, the top-level rebind, a plain owned
/// receiver and a `ref self` method as controls.
///
/// Twin of `tests/interpreter.rs`'s `test_nested_self_rebind_runs_each_body_once`, pinned to the same string.
#[test]
fn e2e_nested_self_rebind_runs_each_body_once() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] }; }
struct S { r: R, n: i64 }
impl Drop for S { fn drop(mut ref self) { println(f"  dS{self.n}") } }
enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("  dE") } }

impl E {
    fn cond_match(self, c: bool) -> i64 {
        if c { let e = self; match e { E.A(r) => { return r.id; } E.B => { return 0; } } }
        else { match self { E.A(r) => { return r.id + 100; } E.B => { return 100; } } }
    }
    fn cond_bare(self, c: bool) -> i64 { if c { let e = self; return 7; } return 0; }
    fn cond_mut(self, c: bool) -> i64 { if c { let mut e = self; match e { E.A(r) => { return r.id; } E.B => { return 0; } } } else { return 5; } }
    fn cond_loop(self, n: i64) -> i64 { let mut i = 0; while i < n { let e = self; return 9; } return 0; }
    fn cond_arm(self, k: i64) -> i64 { match k { 1 => { let e = self; return 1; } _ => { return 2; } } }
    fn top_let(self) -> i64 { let e = self; match e { E.A(r) => { return r.id; } E.B => { return 0; } } }
    fn plain(self, c: bool) -> i64 { if c { return 1; } return 2; }
    fn borrowed(ref self, c: bool) -> i64 { if c { return 1; } return 2; }
}

impl S {
    fn cond_struct(self, c: bool) -> i64 { if c { let s2 = self; return s2.n; } return self.n + 100; }
    fn both_arms(self, c: bool) -> i64 { if c { let s1 = self; return s1.n; } else { let s2 = self; return s2.n + 50; } }
}

fn main() {
    println("enum_true"); let a = E.A(mk(1)); let v1 = a.cond_match(true); println(f"  v={v1}");
    println("enum_false"); let b = E.A(mk(2)); let v2 = b.cond_match(false); println(f"  v={v2}");
    println("bare_true"); let c = E.A(mk(3)); let v3 = c.cond_bare(true); println(f"  v={v3}");
    println("bare_false"); let d = E.A(mk(4)); let v4 = d.cond_bare(false); println(f"  v={v4}");
    println("mut_true"); let e = E.A(mk(5)); let v5 = e.cond_mut(true); println(f"  v={v5}");
    println("loop_once"); let f = E.A(mk(6)); let v6 = f.cond_loop(1); println(f"  v={v6}");
    println("loop_zero"); let g = E.A(mk(7)); let v7 = g.cond_loop(0); println(f"  v={v7}");
    println("arm_taken"); let h = E.A(mk(8)); let v8 = h.cond_arm(1); println(f"  v={v8}");
    println("arm_other"); let i = E.A(mk(9)); let v9 = i.cond_arm(3); println(f"  v={v9}");
    println("temp_true"); let v10 = E.A(mk(10)).cond_match(true); println(f"  v={v10}");
    println("temp_false"); let v11 = E.A(mk(11)).cond_match(false); println(f"  v={v11}");
    println("struct_true"); let j = S { r: mk(12), n: 1 }; let v12 = j.cond_struct(true); println(f"  v={v12}");
    println("struct_false"); let k = S { r: mk(13), n: 2 }; let v13 = k.cond_struct(false); println(f"  v={v13}");
    println("both_arms"); let l = S { r: mk(14), n: 3 }; let v14 = l.both_arms(false); println(f"  v={v14}");
    println("top_let"); let m = E.A(mk(15)); let v15 = m.top_let(); println(f"  v={v15}");
    println("plain"); let n = E.A(mk(16)); let v16 = n.plain(true); println(f"  v={v16}");
    println("borrowed"); let o = E.A(mk(17)); let v17 = o.borrowed(true); println(f"  v={v17}");
    println("end");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"enum_true
  dE
  dR1
  v=1
enum_false
  dR2
  dE
  v=102
bare_true
  dE
  dR3
  v=7
bare_false
  dE
  v=0
mut_true
  dE
  dR5
  v=5
loop_once
  dE
  dR6
  v=9
loop_zero
  dE
  v=0
arm_taken
  dE
  dR8
  v=1
arm_other
  dE
  v=2
temp_true
  dE
  dR10
  v=10
temp_false
  dR11
  dE
  v=111
struct_true
  dS1
  dR12
  v=1
struct_false
  dS2
  dR13
  v=102
both_arms
  dS3
  dR14
  v=53
top_let
  dE
  dR15
  v=15
plain
  dE
  dR16
  v=1
borrowed
  dE
  dR17
  v=1
end
"#
    );
}

/// B-2026-09-06-64 — a SELF-REFERENTIAL struct crashed `karac build` with a
/// COMPILER stack overflow. Ten lines, no `Drop` impl, no method, no
/// rebind: `struct Node { id: i64, next: Option[Node], tag: String }` plus a
/// `main` that builds one. `karac run --interp` ran the same program
/// correctly, so a program could be developed under the interpreter and
/// then fail to compile at all — and a node with an `Option[Self]` field is
/// the canonical linked structure.
///
/// Two unbounded recursions, each closed the way its own family already
/// closes one. The copy-support walk admitted the type through the
/// boxed-envelope disjunct, which asks nothing about the payload, so the
/// entry copy — unrolled INLINE — recursed struct -> `Option` payload ->
/// struct forever; it now consults the same cycle stack the `Vec` arm has
/// consulted since B-2026-07-28-3. And `emit_struct_drop_synthesis` caches
/// its function only after classifying every field, while classifying an
/// `Option[Node]` field re-enters that synthesis, so the re-entry now takes
/// a forward declaration of the symbol the outer call is about to define.
///
/// Cells: the bare local (no `Drop`), a `Drop`-bearing local, a free
/// function that rebinds the param, one that only reads it, the
/// `Drop`-free struct's rebind, an owned-`self` rebind, and
/// `Option[Option[i64]]` as the boxed-envelope control that must stay
/// copy-supported (B-2026-08-07-2 shape 3 — an earlier attempt at this fix
/// declined it and leaked its 32-byte envelope at -O0).
///
/// Twin of `tests/interpreter.rs`'s `test_self_referential_struct_compiles`, pinned to the same string.
#[test]
fn e2e_self_referential_struct_compiles() {
    let Some(out) = run_program(
        r#"struct Node { id: i64, next: Option[Node], tag: String }
impl Drop for Node { fn drop(mut ref self) { println(f"  dN{self.id}") } }
struct Plain { id: i64, next: Option[Plain], tag: String }
struct Env { id: i64, inner: Option[Option[i64]] }
fn mkn(i: i64) -> Node { return Node { id: i, next: Option.None, tag: f"t{i}" }; }
fn mkp(i: i64) -> Plain { return Plain { id: i, next: Option.None, tag: f"p{i}" }; }
fn top(n: Node) -> i64 { let m = n; return m.id; }
fn read(n: Node) -> i64 { return n.id; }
fn topp(p: Plain) -> i64 { let m = p; return m.id; }
fn enve(e: Env) -> i64 { return e.id; }
impl Node { fn take(self) -> i64 { let m = self; return m.id; } }

fn main() {
    println("bare_local"); let a = mkp(1); println(f"  v={a.id}");
    println("drop_local"); let b = mkn(2); println(f"  v={b.id}");
    println("free_fn_rebind"); println(f"  v={top(mkn(3))}");
    println("free_fn_read"); println(f"  v={read(mkn(4))}");
    println("plain_struct_rebind"); println(f"  v={topp(mkp(5))}");
    println("owned_self_rebind"); println(f"  v={mkn(6).take()}");
    println("boxed_envelope"); println(f"  v={enve(Env { id: 7, inner: Option.Some(Option.Some(8)) })}");
    println("end");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"bare_local
  v=1
drop_local
  v=2
  dN2
free_fn_rebind
  dN3
  v=3
free_fn_read
  dN4
  v=4
plain_struct_rebind
  v=5
owned_self_rebind
  dN6
  v=6
boxed_envelope
  v=7
end
"#
    );
}

/// B-2026-09-07-6 — a whole-tuple argument handed back by a METHOD or ASSOC fn
/// lost its element's `Drop` body on every compiled backend:
/// `h.thrut((mkq(1), 7))` over
/// `impl Hold { fn thrut(ref self, t: (Q, i64)) -> (Q, i64) { return t; } }`
/// printed `v=7` where `--interp` printed `v=7 dQ1`, with memory balanced — so
/// no leak gate could see it. The FREE-function twin was correct throughout.
///
/// The row's own "where to start" was the argument registrar's element types,
/// and passing them changed nothing. The defect is on the RESULT side:
/// `tuple_binding_elem_tes` recovers a `let`'s element types from the callee's
/// declared return type through `fn_return_type_exprs`, which is keyed by FREE
/// function only — its comment names the method-call RHS as a deferred tail
/// that "leaks but never double-frees". With no element types the binding gets
/// no bodies walker at all, so the body ran ZERO times. A method that MINTS the
/// tuple (`fn mkt(ref self) -> (Q, i64)`) loses it identically, which is what
/// identifies the result binding rather than the argument.
///
/// `find_function_ast` resolves a `Type.method` key against the impl blocks —
/// the same fallback B-2026-09-06-70 gave the parameter-side question.
///
/// The NAMED-tuple spelling needed the other half: with the result binding
/// owning the elements, the caller's `t` had to stand its element walk down,
/// and the method leg's gate for that suppression asked only the STORE route.
/// `let t = (mkq(6), 3); let e = h.thrut(t);` ran two bodies until the return
/// routes joined it — the same disjunction the free leg has had since
/// B-2026-08-31-46.
///
/// Cells: the method and assoc hand-backs, the free control, a method that
/// mints its tuple, a method that consumes it, and both named-tuple spellings.
///
/// Twin of `tests/interpreter.rs`'s `test_whole_tuple_argument_to_a_method`, pinned to the same string.
#[test]
fn e2e_whole_tuple_argument_to_a_method() {
    let Some(out) = run_program(
        r#"struct Q { id: i64, name: String }
impl Drop for Q { fn drop(mut ref self) { println(f"  dQ{self.id}") } }
fn mkq(i: i64) -> Q { return Q { id: i, name: f"q{i}" }; }
struct Hold { n: i64 }
impl Hold { fn thrut(ref self, t: (Q, i64)) -> (Q, i64) { return t; } }
impl Hold { fn mkt(ref self) -> (Q, i64) { return (mkq(4), 1); } }
impl Hold { fn eatt(ref self, t: (Q, i64)) -> i64 { return t.1; } }
impl Q { fn passt(t: (Q, i64)) -> (Q, i64) { return t; } }
fn passt(t: (Q, i64)) -> (Q, i64) { return t; }
fn main() {
  let h = Hold { n: 0 };
  println("method_tuple"); let a = h.thrut((mkq(1), 7)); println(f"  v={a.1}");
  println("assoc_tuple"); let b = Q.passt((mkq(2), 8)); println(f"  v={b.1}");
  println("free_tuple"); let c = passt((mkq(3), 9)); println(f"  v={c.1}");
  println("method_mints"); let d = h.mkt(); println(f"  v={d.1}");
  println("method_eats"); println(f"  v={h.eatt((mkq(5), 2))}");
  println("named_tuple_method"); let t = (mkq(6), 3); let e = h.thrut(t); println(f"  v={e.1}");
  println("named_tuple_assoc"); let u = (mkq(7), 4); let f = Q.passt(u); println(f"  v={f.1}");
  println("end");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"method_tuple
  v=7
  dQ1
assoc_tuple
  v=8
  dQ2
free_tuple
  v=9
  dQ3
method_mints
  v=1
  dQ4
method_eats
  dQ5
  v=2
named_tuple_method
  v=3
  dQ6
named_tuple_assoc
  v=4
  dQ7
end
"#
    );
}

/// B-2026-09-13-27 — a tuple literal carrying a FIELD PROJECTED out of a
/// local (`let w = (t.r, 1);`) runs the moved leaf's `Drop` body ONCE, and at
/// the binding that owns it rather than at the source.
///
/// The row was filed as a run-vs-build count divergence — two bodies under
/// `--interp` against one compiled — and the count was the smaller half. The
/// compiled body ran at the SOURCE's drop, ahead of the `w` that owns the
/// value, so the single answer was in the wrong place; and where no named
/// source existed to fire it (`(mkw(7).r, 1)`), or where the RHS was an `if`
/// wrapping the literal, the compiled backends ran it ZERO times. Three
/// defects composing into a plausible-looking one.
///
/// The `if`-RHS cells (2, 4, 7, 9, 10) covered a SEPARATE and wider gap,
/// measured on a clean `origin/main`: a `let`-bound tuple whose RHS was an
/// `if` got no bodies walker at all, so the whole-local and fresh-minting
/// elements lost the body identically with no projection in sight.
///
/// The `pre` / `post` markers are load-bearing and not decoration: they are
/// what distinguishes "one body" from "one body at the right time", which is
/// the distinction the row's count-only measurement could not make. Cell 5 (the
/// UNMOVED sibling field) and the whole-local controls pin the other side —
/// standing a source's walk down must not take a field that never moved.
///
/// The last cell is the STRUCT-literal spelling of cell 1, fixed by
/// B-2026-09-01-17 and asserted here as the reference the tuple spelling now
/// matches. Twin in the other backend's suite under the same name, with the
/// same table, so a one-sided change shows up as a diff rather than as a drift.
#[test]
fn e2e_tuple_literal_of_a_projected_field_runs_one_body_at_the_owner() {
    let hdr = "struct D { a: String, b: i64 }\n\
                   impl Drop for D { fn drop(mut ref self) { println(f\"dD{self.b}\") } }\n\
                   struct W { r: D, s: D, b: i64 }\n\
                   struct V { r: D, b: i64 }\n\
                   fn pay() -> String { return \"heap\"; }\n\
                   fn mkd(n: i64) -> D { return D { a: pay(), b: n }; }\n\
                   fn mkw(n: i64) -> W { return W { r: mkd(n), s: mkd(n + 100), b: n }; }\n";
    for (label, stmts, want) in [
        (
            "projected field, read later",
            "let t = mkw(7);\n\
                 println(\"pre\");\n\
                 let w = (t.r, 1);\n\
                 println(\"post\");\n\
                 println(f\"idx{w.1}\");",
            "pre\ndD107\npost\nidx1\ndD7\nend\n",
        ),
        (
            "projected field through an if",
            "let t = mkw(7);\n\
                 println(\"pre\");\n\
                 let w = if n == 0 { (t.r, 1) } else { (mkd(2), 2) };\n\
                 println(\"post\");\n\
                 println(f\"idx{w.1}\");",
            "pre\ndD107\npost\nidx1\ndD7\nend\n",
        ),
        (
            "projected field, never read",
            "let t = mkw(7);\n\
                 println(\"pre\");\n\
                 let w = (t.r, 1);\n\
                 println(\"post\");",
            "pre\ndD7\ndD107\npost\nend\n",
        ),
        (
            "projected field through an if, never read",
            "let t = mkw(7);\n\
                 println(\"pre\");\n\
                 let w = if n == 0 { (t.r, 1) } else { (mkd(2), 2) };\n\
                 println(\"post\");",
            "pre\ndD7\ndD107\npost\nend\n",
        ),
        (
            "the SIBLING field projected instead",
            "let t = mkw(7);\n\
                 println(\"pre\");\n\
                 let w = (t.s, 1);\n\
                 println(\"post\");\n\
                 println(f\"idx{w.1}\");",
            "pre\ndD7\npost\nidx1\ndD107\nend\n",
        ),
        (
            // B-2026-09-14-16 — was `pre post idx1 dD7 end`, which PINNED
            // the loss: `s`'s body ran nowhere, on this backend and the
            // interpreter alike. It now runs at the projection, which is
            // where the temp's live range ends — exactly where the NAMED
            // source above prints it. The interpreter twin of this cell
            // moved in the same commit; the loss was agreed, so one side
            // moving alone would have made a divergence of it.
            "fresh-temp projection, no named source",
            "println(\"pre\");\n\
                 let w = (mkw(7).r, 1);\n\
                 println(\"post\");\n\
                 println(f\"idx{w.1}\");",
            "pre\ndD107\npost\nidx1\ndD7\nend\n",
        ),
        (
            // B-2026-09-14-16 — the projection inside an `if` ARM moved too, and
            // had to: codegen reaches its per-element consuming site through the
            // taken arm, so leaving the interpreter out of the branch arms was
            // measured as a fresh run-vs-build divergence rather than a
            // conservative omission. The stash is keyed on the projection's object
            // SPAN, so descending into both arms is safe without knowing which ran.
            "fresh-temp projection through an if",
            "println(\"pre\");\n\
                 let w = if n == 0 { (mkw(7).r, 1) } else { (mkd(2), 2) };\n\
                 println(\"post\");\n\
                 println(f\"idx{w.1}\");",
            "pre\ndD107\npost\nidx1\ndD7\nend\n",
        ),
        (
            "control: whole-local element",
            "let d = mkd(7);\n\
                 println(\"pre\");\n\
                 let w = (d, 1);\n\
                 println(\"post\");\n\
                 println(f\"idx{w.1}\");",
            "pre\npost\nidx1\ndD7\nend\n",
        ),
        (
            "control: whole-local element through an if",
            "let d = mkd(7);\n\
                 println(\"pre\");\n\
                 let w = if n == 0 { (d, 1) } else { (mkd(2), 2) };\n\
                 println(\"post\");\n\
                 println(f\"idx{w.1}\");",
            "pre\npost\nidx1\ndD7\nend\n",
        ),
        (
            "control: fresh element through an if",
            "println(\"pre\");\n\
                 let w = if n == 0 { (mkd(7), 1) } else { (mkd(2), 2) };\n\
                 println(\"post\");\n\
                 println(f\"idx{w.1}\");",
            "pre\npost\nidx1\ndD7\nend\n",
        ),
        (
            "control: the STRUCT-literal sibling of cell 1",
            "let t = mkw(7);\n\
                 println(\"pre\");\n\
                 let w = V { r: t.r, b: 1 };\n\
                 println(\"post\");\n\
                 println(f\"idx{w.b}\");",
            "pre\ndD107\npost\nidx1\ndD7\nend\n",
        ),
    ] {
        let src = format!("{hdr}fn main() {{\nlet n = 0;\n{stmts}\nprintln(\"end\");\n}}\n");
        assert_eq!(run_program(&src).as_deref(), Some(want), "[{label}]");
    }
}

/// B-2026-08-01-8 — a MIXED fresh+place tuple discard (`let _ = (r,
/// 20);` where `r` is a Drop-bearing struct binding): the moved place
/// element's source retracts and the discarded tuple temp's element
/// walk becomes the single owner, firing the body with the value INTACT
/// and freeing its heap once. Pre-fix `karac build` fired the source's
/// wrapper over a zeroed slot (`drop 2 ` with an empty name) and leaked
/// the moved String; `karac run` fired intact via the source's NLL walk
/// — a three-way divergence. All-fresh (a) and scalar-projection (c)
/// shapes pin the unchanged behavior. Twin of `tests/interpreter.rs`'s
/// `test_mixed_place_tuple_discard_single_intact_fire`.
#[test]
fn e2e_mixed_place_tuple_discard_single_intact_fire() {
    let Some(out) = run_program(
        "struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"drop {self.id} {self.name}\")\n\
             \x20   }\n\
             }\n\
             fn mk(n: i64) -> Res {\n\
             \x20   return Res { id: n, name: f\"r{n}\" };\n\
             }\n\
             fn main() {\n\
             \x20   println(\"a: all-fresh tuple discard\");\n\
             \x20   let _ = (mk(1), 10);\n\
             \x20   println(\"b: mixed fresh+place tuple discard\");\n\
             \x20   let r = mk(2);\n\
             \x20   let _ = (r, 20);\n\
             \x20   println(\"c: place still-owned after\");\n\
             \x20   let s = mk(3);\n\
             \x20   let _ = (s.id, 30);\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(
        out,
        "a: all-fresh tuple discard\ndrop 1 r1\n\
             b: mixed fresh+place tuple discard\ndrop 2 r2\n\
             c: place still-owned after\ndrop 3 r3\nend\n"
    );
}

#[test]
fn test_e2e_free_fn_constructor_receiver_was_never_affected() {
    // Control pinning the SCOPE of the defect. A free generic constructor
    // called from `main` binds `T` from concrete arguments, so its receiver
    // instantiation was never parametric and this spelling passed before
    // the fix. Keeping it here documents where the boundary is, so a future
    // reader does not widen the fix chasing a case that never broke.
    let src = "\
struct Bag[=T] { xs: Vec[T] }
impl[T: Ord] Bag[T] {
    fn swap2(mut ref self, i: i64, j: i64) { self.xs.swap(i, j); }
    fn arrange(mut ref self) { let n = self.xs.len(); if n > 1 { self.swap2(0, n - 1); } }
    fn inner(self) -> Vec[T] { let mut b = self; b.arrange(); b.xs }
}
fn mkbag[T: Ord](v: Vec[T]) -> Bag[T] { Bag { xs: v } }
fn main() { let a = mkbag([\"x\",\"y\",\"z\"]).inner(); println(a[0]); let b = mkbag([1,2,3]).inner(); println(b[0]); }";
    assert_eq!(run_program(src).as_deref(), Some("z\n3\n"));
}

/// B-2026-08-13-21 — the same declared-widths rule at every OTHER position a
/// tuple literal can appear in: RETURN (both spellings), ARGUMENT, and
/// STRUCT FIELD. B-2026-08-13-17 fixed the `let` position and left the
/// consuming machinery shared; only the per-position staging was missing.
///
/// Unlike that row, these were LOUD — the layout mismatch reached the module
/// verifier rather than producing a wrong number (`ret { i8, i32 }` against a
/// `{ i64, i64 }` signature; "Call parameter type does not match function
/// signature"; "Invalid InsertValueInst operands"). `--interp` ran all of
/// them correctly throughout, so each was a run-vs-build divergence too.
///
/// Four staging sites, because a tuple literal reaches codegen through four
/// distinct paths and each carries the declared type differently: the
/// function body's final expression (`func.return_type`), the explicit
/// `return` operand (`fn_return_type_exprs` for the function being
/// compiled), the call-argument BY-VALUE path (`fn_asts` param — the
/// `ref`-param branch passes a pointer and returns early, so it is not the
/// one a tuple takes), and BOTH struct-literal builders — the plain one
/// builds its aggregate with `insertvalue` while the shared one GEPs and
/// stores, and `P { t: (b, d) }` goes through the former.
///
/// Lines 03/04/05 pin what must NOT change: a nested tuple return gets its
/// own declared widths, a `String` element keeps the compiled value's own
/// layout, and an already-matching tuple is untouched. Line 09 covers a
/// `shared struct`, which is the GEP+store builder. Line 11 passes a tuple
/// by `ref`, which must keep taking the pointer path.
#[test]
fn test_e2e_tuple_positions_use_declared_element_widths() {
    let src = r#"
struct P { t: (i64, i64), n: i64 }
shared struct S { t: (i64, i64) }
fn take(t: (i64, i64)) -> i64 { return t.0 + t.1; }
fn take2(a: i64, t: (i64, i64), b: i64) -> i64 { return a + t.0 + t.1 + b; }
fn takeref(t: ref (i64, i64)) -> i64 { return t.0 + t.1; }
fn giveret() -> (i64, i64) { let b: u8 = 200u8; let d: u32 = 4000000000u32; return (b, d); }
fn givetail() -> (i64, i64) { let b: u8 = 200u8; let d: u32 = 4000000000u32; (b, d) }
fn givenest() -> ((i64, i64), i64) { let b: u8 = 200u8; let d: u32 = 4000000000u32; return ((b, d), 7); }
fn giveheap() -> (String, i64) { let b: u8 = 200u8; return ("hi", b); }
fn givesame() -> (i64, i64) { return (1, 2); }

fn main() {
    let b: u8 = 200u8;
    let d: u32 = 4000000000u32;
    let r1 = giveret();    println(f"01 {r1.0} {r1.1}");
    let r2 = givetail();   println(f"02 {r2.0} {r2.1}");
    let r3 = givenest();   println(f"03 {r3.0.0} {r3.0.1} {r3.1}");
    let r4 = giveheap();   println(f"04 {r4.0} {r4.1}");
    let r5 = givesame();   println(f"05 {r5.0} {r5.1}");
    println(f"06 {take((b, d))}");
    println(f"07 {take2(1, (b, d), 2)}");
    let p = P { t: (b, d), n: 7 };
    println(f"08 {p.t.0} {p.t.1} {p.n}");
    let s = S { t: (b, d) };
    println(f"09 {s.t.0} {s.t.1}");
    let lt: (i64, i64) = (b, d);
    println(f"10 {lt.0} {lt.1}");
    println(f"11 {takeref(lt)}");
    let un = (b, d);
    println(f"12 {un.0} {un.1}");
}
"#;
    assert_eq!(
        run_program(src).as_deref(),
        Some(
            "01 200 4000000000\n\
                 02 200 4000000000\n\
                 03 200 4000000000 7\n\
                 04 hi 200\n\
                 05 1 2\n\
                 06 4000000200\n\
                 07 4000000203\n\
                 08 200 4000000000 7\n\
                 09 200 4000000000\n\
                 10 200 4000000000\n\
                 11 4000000200\n\
                 12 200 4000000000\n"
        ),
    );
}

/// B-2026-08-13-21 — the boundary that decided WHERE the tail-return staging
/// goes. A nested block's tail is compiled by the same helper as a function
/// body's, so staging the function's return type inside that helper would
/// overwrite an inner `let`'s own annotation: here `inner` is declared
/// `(u8, u32)` inside a fn returning `(i64, i64)`, and it must keep its own
/// widths. The staging therefore sits at the function-body final expression,
/// the only tail that is unambiguously the return.
#[test]
fn test_e2e_nested_block_tail_keeps_its_own_tuple_annotation() {
    assert_eq!(
        run_program(
            "fn f() -> (i64, i64) {\n\
                     let b: u8 = 200u8;\n\
                     let d: u32 = 4000000000u32;\n\
                     let inner: (u8, u32) = { (b, d) };\n\
                     println(f\"inner {inner.0} {inner.1}\");\n\
                     return (b, d);\n\
                 }\n\
                 fn main() { let t = f(); println(f\"outer {t.0} {t.1}\"); }"
        )
        .as_deref(),
        Some("inner 200 4000000000\nouter 200 4000000000\n"),
    );
}

/// B-2026-08-13-17 — an ANNOTATED tuple binding must lay its aggregate out
/// at the DECLARED element widths, not at the compiled values' widths.
///
/// There were two sources of truth for one aggregate: `compile_tuple` built
/// the struct type from the element VALUES while every read resolved element
/// types from the ANNOTATION. So `let t: (i64, i64) = (b, d)` with `b: u8`
/// laid out `{i8, i32}`, and the read — trusting `{i64, i64}` — sign-extended
/// the i8 it actually found and printed 200 as -56 on both compiled
/// backends, at every optimization level.
///
/// The UNANNOTATED line is the control and must stay right: it was already
/// correct precisely because nothing downstream disagreed with the values,
/// and B-2026-08-13-15 fixed its read side. A fix that made the aggregate
/// follow the annotation could plausibly have disturbed it.
///
/// The other lines pin the boundaries the fix must NOT cross. `narrow`
/// declares slots at the source width (no coercion owed). `mixed` puts a
/// `String` beside a widened scalar — heap elements keep the compiled
/// value's own layout verbatim, since substituting a separately-lowered
/// type there would risk a mismatch where nothing was wrong. `float` pins
/// the int→float leg. `nested` pins that an inner annotated tuple gets its
/// OWN declared widths rather than inheriting the outer annotation.
///
/// Line 10 is a DIRECT destructure, whose pattern is a tuple rather than a
/// binding. Its declared layout has to come from the annotation alone —
/// there is no variable name to key a per-variable registry off — which is
/// why the staging sits outside the binding-pattern arm that its siblings
/// live in.
///
/// Values carry the high bit (200, 4000000000) because that is the only
/// region where sext and zext differ — the same reason B-2026-08-13-15 uses
/// them, and the reason a probe with 97 would report this as working.
#[test]
fn test_e2e_annotated_tuple_uses_declared_element_widths() {
    let src = r#"
fn main() {
    let b: u8 = 200u8;
    let d: u32 = 4000000000u32;
    let n: i64 = 7;

    let t: (i64, i64) = (b, d);
    println(f"01 {t.0} {t.1}");

    let un = (b, d);
    println(f"02 {un.0} {un.1}");

    let nest: ((i64, i64), i64) = ((b, d), n);
    println(f"03 {nest.0.0} {nest.0.1} {nest.1}");

    let narrow: (u8, u8) = (200u8, 7u8);
    println(f"04 {narrow.0} {narrow.1}");

    let s: String = "hi";
    let mixed: (String, i64) = (s, b);
    println(f"05 {mixed.0} {mixed.1}");

    let f: f64 = 1.5;
    let fl: (f64, i64) = (f, b);
    println(f"06 {fl.0} {fl.1}");

    let pair: (i64, i64) = (b, d);
    let (x, y) = pair;
    println(f"07 {x} {y}");

    let same: (i64, i64) = (n, n);
    println(f"08 {same.0} {same.1}");

    let three: (i64, i64, i64) = (b, d, n);
    println(f"09 {three.0} {three.1} {three.2}");

    let (p, q): (i64, i64) = (b, d);
    println(f"10 {p} {q}");
}
"#;
    assert_eq!(
        run_program(src).as_deref(),
        Some(
            "01 200 4000000000\n\
                 02 200 4000000000\n\
                 03 200 4000000000 7\n\
                 04 200 7\n\
                 05 hi 200\n\
                 06 1.5 200\n\
                 07 200 4000000000\n\
                 08 7 7\n\
                 09 200 4000000000 7\n\
                 10 200 4000000000\n"
        ),
    );
}

#[test]
fn test_e2e_arena_struct_elements() {
    // The primary arena use case — an all-POD struct (AST-node / ECS-row
    // shape) bump-allocated and read back by field. `get` copies the
    // byte image into a fresh local (matching the interpreter's
    // clone-on-`get`), so field access works like any struct binding.
    // Mirrors the interpreter `test_arena_get_struct_field_access`.
    let out = run_program(
        r#"
struct Node { val: i64, next: i64 }
fn main() {
    let a: Arena[Node] = Arena.new();
    let r = a.push(Node { val: 7, next: 99 });
    let n = a.get(r);
    println(n.val);
    println(n.next);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "7\n99");
    }
}

#[test]
fn test_e2e_arena_chained_get_field() {
    // Regression: `arena.get(r).field` chained (field access directly on
    // the struct-returning get) read the `i64 0` placeholder — the
    // generic `compile_field_access` tail couldn't resolve the element
    // struct's field index because `type_name_of_expr` had no arm for a
    // compiler-builtin `arena.get(...)` MethodCall. The bound form
    // (`let n = arena.get(r); n.field`) always worked (n is registered).
    // A silent miscompile (wrong value, not a crash) that shipped with
    // the Arena codegen slice; fixed by resolving the arena-get element
    // struct name in `type_name_of_expr`.
    let out = run_program(
        r#"
struct Node { val: i64, kids: i64 }
fn main() {
    let a: Arena[Node] = Arena.new();
    let r = a.push(Node { val: 42, kids: 7 });
    println(a.get(r).val);
    println(a.get(r).kids);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "42\n7");
    }
}

#[test]
fn test_e2e_destructuring_function_parameters() {
    // B-2026-08-17-23 — design.md § Destructuring in Function and Closure
    // Parameters opens with "Any irrefutable pattern may appear in
    // parameter position", and roadmap.md marks it done, but codegen's
    // prologue bound only the whole-parameter slot and never walked the
    // pattern: every shape died with `Undefined variable '<leaf>'` on BOTH
    // compiled backends while `karac check` passed clean and `--interp`
    // ran correctly. Every shape the ledger row enumerated is pinned here.
    let src = "struct Point { x: i64, y: i64 }\n\
                   struct H { s: String, n: i64 }\n\
                   fn add((a, b): (i64, i64)) -> i64 { a + b }\n\
                   fn y_only((_, y): (i64, i64)) -> i64 { y }\n\
                   fn only_a((a, _): (i64, i64)) -> i64 { a }\n\
                   fn nested(((a, b), c): ((i64, i64), i64)) -> i64 { a + b + c }\n\
                   fn px(Point { x, y }: Point) -> i64 { x + y }\n\
                   fn distance(Point { x: x1, y: y1 }: Point, Point { x: x2, y: y2 }: Point) \
                   -> i64 { (x2 - x1) + (y2 - y1) }\n\
                   fn take(H { s, n }: H) -> String { s + n.to_string() }\n\
                   fn main() {\n\
                       println(add((3, 4)));\n\
                       println(y_only((3, 4)));\n\
                       println(only_a((3, 4)));\n\
                       println(nested(((1, 2), 3)));\n\
                       println(px(Point { x: 1, y: 2 }));\n\
                       println(distance(Point { x: 1, y: 1 }, Point { x: 4, y: 6 }));\n\
                       println(take(H { s: \"v\", n: 9 }));\n\
                   }";
    let parsed = karac::parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    if let Some(out) = run_program(src) {
        assert_eq!(out, "7\n4\n3\n6\n3\n8\nv9\n");
    }
}

#[test]
fn test_e2e_module_const_field_then_method_receiver() {
    // B-2026-08-17-21 — `let ORIGIN: Point = …; ORIGIN.x.to_string()`,
    // the shape E_MODULE_BINDING_NAMING forces every module constant
    // into. It used to fail `karac check` with "type 'Point' is not
    // callable" (the parser's greedy uppercase-Path rooting fell past a
    // 2-segment-only rescue); widening the rescue exposed a second
    // layer here in codegen, where the field-receiver path looked the
    // outer name up in `variables` and a module binding has no slot —
    // its storage is a program-lifetime LLVM global. Both halves are
    // pinned by this test: it must check clean AND run.
    let src = "struct Point { x: i64, y: i64 }\n\
                   let ORIGIN: Point = Point { x: 5, y: 9 };\n\
                   fn main() {\n\
                       println(ORIGIN.x.to_string());\n\
                       println(ORIGIN.y.to_string());\n\
                       println(f\"{ORIGIN.x.to_string()}\");\n\
                       let LOCAL = Point { x: 1, y: 2 };\n\
                       println(LOCAL.x.to_string());\n\
                   }";
    let parsed = karac::parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    assert!(
        typed.errors.is_empty(),
        "`CONST.field.method()` must typecheck clean, got: {:?}",
        typed.errors.iter().map(|e| &e.message).collect::<Vec<_>>(),
    );
    if let Some(out) = run_program(src) {
        assert_eq!(out, "5\n9\n5\n1\n");
    }
}

/// `forget(x)` (FFI ownership-handoff primitive, additive-interop
/// Slice 4) consumes `x` and suppresses its destructor. A user-`Drop`
/// counter makes suppression observable at codegen: with `forget(b)`
/// only `a` drops (one "d"); the counterfactual without `forget` drops
/// both (two "d"). Proves codegen's drop-suppression intercept, not
/// just the interpreter path.
#[test]
fn forget_suppresses_destructor() {
    let base = "struct G { id: i64 }\n\
                    impl Drop for G { fn drop(mut ref self) { println(\"d\"); } }\n";
    let with_forget = format!(
        "{base}fn main() {{\n\
             \x20 let a = G {{ id: 1 }};\n\
             \x20 let b = G {{ id: 2 }};\n\
             \x20 println(a.id); println(b.id);\n\
             \x20 forget(b);\n\
             \x20 println(\"end\");\n\
             }}\n"
    );
    let without = format!(
        "{base}fn main() {{\n\
             \x20 let a = G {{ id: 1 }};\n\
             \x20 let b = G {{ id: 2 }};\n\
             \x20 println(a.id); println(b.id);\n\
             \x20 println(\"end\");\n\
             }}\n"
    );
    if let Some(out) = run_program(&with_forget) {
        let drops = out.lines().filter(|l| *l == "d").count();
        assert_eq!(drops, 1, "forget(b) must suppress b's drop; stdout:\n{out}");
    }
    if let Some(out) = run_program(&without) {
        let drops = out.lines().filter(|l| *l == "d").count();
        assert_eq!(drops, 2, "without forget both drop; stdout:\n{out}");
    }
}

/// `#[repr(transparent)]` for distinct-type FFI (design.md § the same). The
/// wrapper's ABI shape IS its inner field's, so a `Fd = i32` passes to an
/// `extern "C"` `i32` param via `.raw()` with no conversion, and a single-
/// field-struct wrapper passes via field access — both round-trip through
/// libc `abs`. The layout guarantee (`size_of[Wrapper] == size_of[Inner]`) is
/// verified alongside. The attribute needs no dedicated codegen: distinct
/// types are already zero-cost and a single-field struct already lays out as
/// its field; the value the attribute adds is the typechecker's single-field
/// guarantee that makes the pass-through sound.
#[test]
fn repr_transparent_ffi_and_layout() {
    let src = "#[repr(transparent)] distinct type Fd = i32;\n\
                   #[repr(transparent)] struct Handle { inner: i32 }\n\
                   unsafe extern \"C\" { fn abs(v: i32) -> i32; }\n\
                   fn main() {\n\
                   \x20   let fd: Fd = Fd(-7i32);\n\
                   \x20   println(unsafe { abs(fd.raw()) });\n\
                   \x20   let h = Handle { inner: -12i32 };\n\
                   \x20   println(unsafe { abs(h.inner) });\n\
                   \x20   println(size_of[Fd]());\n\
                   \x20   println(size_of[Handle]());\n\
                   \x20   println(size_of[i32]());\n\
                   }\n";
    if let Some(out) = run_program(src) {
        assert_eq!(
            out, "7\n12\n4\n4\n4\n",
            "FFI round-trip + layout pass-through"
        );
    }
}

/// Regression (B-2026-07-03-2): a method declared `-> Self` returning a
/// struct must lower to a prototype whose LLVM return type matches the
/// concrete aggregate — the static-constructor, inherent-method, and
/// trait-impl forms all produce correct field values through `karac
/// build`. Before the fix `-> Self` hit the unknown-name `i64`
/// fall-through in `llvm_return_type` (module-verify failure), and once
/// past that the call-site result typed as the abstract `Self` so field
/// reads returned 0. Each result is bound to a local before the field
/// read (the chained `expr.method().field` aggregate form is a separate,
/// pre-existing codegen gap — B-2026-07-03-3). `twice` is called on a
/// literal-bound receiver, not on a static-`make()` result, to avoid the
/// distinct auto-par materialization gap B-2026-07-03-4.
#[test]
fn e2e_method_self_return_struct_lowers_and_reads_fields() {
    if let Some(out) = run_program(
        "struct W { v: i64 }\n\
             trait Dbl { fn twice(self) -> Self; }\n\
             impl W {\n\
                 fn id(self) -> Self { self }\n\
                 fn make() -> Self { W { v: 7 } }\n\
             }\n\
             impl Dbl for W { fn twice(self) -> Self { W { v: self.v + self.v } } }\n\
             fn main() {\n\
                 let a = W { v: 5 };\n\
                 let b = a.id();\n\
                 let c = W.make();\n\
                 let e = W { v: 6 };\n\
                 let d = e.twice();\n\
                 println(f\"{b.v}\");\n\
                 println(f\"{c.v}\");\n\
                 println(f\"{d.v}\");\n\
             }",
    ) {
        assert_eq!(out, "5\n7\n12\n");
    }
}

/// Regression (B-2026-07-03-3): a chained `expr.method().field` where the
/// method returns a plain struct reads the correct field under `karac
/// build`. Before the fix `type_name_of_expr` had no `MethodCall`/`Call`
/// arm, so `field_index_for` returned `None` and the generic tail of
/// `compile_field_access` emitted the `i64 0` placeholder — a silent
/// miscompile (the bound form `let b = e.m(); b.field` always worked).
/// Covers a scalar field, a heap (String) field, a later multi-struct
/// field, a double method chain, and a free-function call chain.
#[test]
fn e2e_chained_call_struct_field_access() {
    if let Some(out) = run_program(
            "struct P { x: i64, y: i64 }\n\
             struct N { name: String, id: i64 }\n\
             impl P {\n\
             \x20   fn shift(self) -> P { P { x: self.x + 1, y: self.y + 10 } }\n\
             }\n\
             impl N {\n\
             \x20   fn relabel(self) -> N { N { name: \"chained heap field ok\", id: self.id + 1 } }\n\
             }\n\
             fn mk(n: i64) -> P { P { x: n, y: n * 2 } }\n\
             fn main() {\n\
             \x20   let p = P { x: 1, y: 2 };\n\
             \x20   println(f\"{p.shift().x}\");\n\
             \x20   let p2 = P { x: 1, y: 2 };\n\
             \x20   println(f\"{p2.shift().y}\");\n\
             \x20   let p3 = P { x: 1, y: 2 };\n\
             \x20   println(f\"{p3.shift().shift().x}\");\n\
             \x20   println(f\"{mk(7).y}\");\n\
             \x20   let n = N { name: \"start\", id: 5 };\n\
             \x20   println(f\"{n.relabel().name}\");\n\
             \x20   let n2 = N { name: \"start\", id: 5 };\n\
             \x20   println(f\"{n2.relabel().id}\");\n\
             }",
        ) {
            // p.shift().x=2, .y=12, .shift().x=3; mk(7).y=14;
            // n.relabel().name/.id
            assert_eq!(out, "2\n12\n3\n14\nchained heap field ok\n6\n");
        }
}

#[test]
fn e2e_pool_acquire_reuse_at_cap_and_struct_element() {
    // `Pool[T]` codegen (phase-8 backend platform). `Pool.new(create_fn,
    // max, waiters)` stores the bare-fn `create_fn` fat pointer in the
    // runtime; `acquire` mints via a codegen-orchestrated indirect call
    // (runtime hands the fat pointer back), reuses a released idle slot, or
    // fails at cap with `Err(Timeout)`. Covers i64 and a POD-struct element.
    // Byte-identical to `karac run --interp`.
    if let Some(out) = run_program(
            "fn make_int() -> i64 { 42i64 }\n\
             struct Conn { fd: i64, port: i64 }\n\
             fn make_conn() -> Conn { Conn { fd: 3i64, port: 8080i64 } }\n\
             fn main() {\n\
             \x20   let p: Pool[i64] = Pool.new(make_int, 1i64, 2i64);\n\
             \x20   match p.acquire(0i64) {\n\
             \x20       Ok(c1) => { println(c1.val.to_string()); p.release(c1); }\n\
             \x20       Err(_e) => println(\"t1\"),\n\
             \x20   }\n\
             \x20   match p.acquire(0i64) {\n\
             \x20       Ok(c2) => {\n\
             \x20           match p.acquire(0i64) {\n\
             \x20               Ok(_c3) => println(\"unexpected\"),\n\
             \x20               Err(_e) => println(\"atcap\"),\n\
             \x20           }\n\
             \x20           println(c2.val.to_string());\n\
             \x20           p.release(c2);\n\
             \x20       }\n\
             \x20       Err(_e) => println(\"t2\"),\n\
             \x20   }\n\
             \x20   let sp: Pool[Conn] = Pool.new(make_conn, 2i64, 4i64);\n\
             \x20   match sp.acquire(0i64) {\n\
             \x20       Ok(sc) => { println(f\"{sc.val.fd}\"); println(f\"{sc.val.port}\"); sp.release(sc); }\n\
             \x20       Err(_e) => println(\"t3\"),\n\
             \x20   }\n\
             }",
        ) {
            // acquire mints 42; release; re-acquire reuses 42; nested acquire at
            // cap=1 → atcap; struct pool mints Conn{3,8080}.
            assert_eq!(out, "42\natcap\n42\n3\n8080\n");
        }
}

/// B-2026-07-03-7 (codegen side): `Vec[Struct].sort()` and
/// `Vec[Enum].sort()` for a `#[derive(Ord)]` user type. Pre-fix codegen
/// errored "Vec.sort() in codegen supports integer, String, float, tuple,
/// and nested-Vec element types; use sort_by(...)". Now the recursive
/// `karac_cmp_<T>` family orders struct fields / enum variants + payloads
/// in DECLARATION order (B-2026-07-03-12 semantics), so `karac build`
/// agrees with `karac run`:
///   - struct `Rect { width, height }` sorts by `width` FIRST (declaration
///     order) — not alphabetically by `height`.
///   - enum `Shape { Circle(i64), Rect(i64,i64), Unit }` orders by variant
///     DISCRIMINANT (declaration order), then payload fields in order — the
///     scalar-in-payload-word load path.
///   - struct `Named { name: String, age }` exercises a heap (String)
///     leading field.
#[test]
fn e2e_struct_sort_declaration_order() {
    if let Some(out) = run_program(
            "#[derive(Eq, Ord)]\n\
             struct Rect { width: i64, height: i64 }\n\
             #[derive(Eq, Ord)]\n\
             struct Named { name: String, age: i64 }\n\
             fn main() {\n\
             \x20   let mut v: Vec[Rect] = Vec.new();\n\
             \x20   v.push(Rect { width: 2, height: 1 });\n\
             \x20   v.push(Rect { width: 1, height: 9 });\n\
             \x20   v.sort();\n\
             \x20   let mut i = 0;\n\
             \x20   while i < v.len() { let r = ref v[i]; println(f\"{r.width},{r.height}\"); i = i + 1; };\n\
             \x20   let mut n: Vec[Named] = Vec.new();\n\
             \x20   n.push(Named { name: \"bob-long-payload-string-here\", age: 30 });\n\
             \x20   n.push(Named { name: \"amy-long-payload-string-here\", age: 99 });\n\
             \x20   n.push(Named { name: \"amy-long-payload-string-here\", age: 20 });\n\
             \x20   n.sort();\n\
             \x20   let mut j = 0;\n\
             \x20   while j < n.len() { let p = ref n[j]; println(f\"{p.age}\"); j = j + 1; };\n\
             }",
        ) {
            // Rect sorted by width: (1,9) (2,1). Named sorted by name then age:
            // amy/20, amy/99, bob/30 → ages 20 99 30.
            assert_eq!(out, "1,9\n2,1\n20\n99\n30\n");
        }
}

/// B-2026-07-03-7 (codegen side): `<`, `<=`, `>`, `>=` on a
/// `#[derive(Ord)]` struct / enum lower through the `karac_cmp_<T>`
/// declaration-order comparator (result compared against zero). Pre-fix
/// codegen errored "Unsupported struct binary op: Lt". A non-Ord operand is
/// a clean type error (not covered here — it never reaches codegen).
#[test]
fn e2e_ordered_operators_on_derived_ord_struct_and_enum() {
    if let Some(out) = run_program(
        "#[derive(Eq, Ord)]\n\
             struct P { a: i64, b: i64 }\n\
             #[derive(Eq, Ord)]\n\
             enum Pri { Low, Med, High }\n\
             fn main() {\n\
             \x20   let x = P { a: 1, b: 2 };\n\
             \x20   let y = P { a: 1, b: 3 };\n\
             \x20   println(f\"{x < y}\");\n\
             \x20   println(f\"{y > x}\");\n\
             \x20   println(f\"{x <= x}\");\n\
             \x20   println(f\"{y < x}\");\n\
             \x20   println(f\"{Pri.Low < Pri.High}\");\n\
             \x20   println(f\"{Pri.High < Pri.Low}\");\n\
             }",
    ) {
        assert_eq!(out, "true\ntrue\ntrue\nfalse\ntrue\nfalse\n");
    }
}

#[test]
fn e2e_refinement_struct_field_read_and_cast_out() {
    // The non-generic siblings of the projection rules: a refinement
    // over a *struct* reads the base's fields, and a refinement over a
    // numeric base casts out to another numeric type. Both were
    // typecheck errors pre-fix ("no field 'name' on type 'Adult'",
    // "cannot cast 'PositiveQty' to 'f64'").
    if let Some(out) = run_program(
        "pub type PositiveQty = i64 where self > 0;\n\
             pub struct Person { name: String, age: i64 }\n\
             pub type Adult = Person where self.age > 17;\n\
             pub fn label(p: Adult) -> String { p.name }\n\
             pub fn half(q: PositiveQty) -> f64 { (q as f64) / 2.0 }\n\
             fn main() {\n\
                 let p = Person { name: \"ada\", age: 36 };\n\
                 println(label(p as Adult));\n\
                 println(half(9 as PositiveQty));\n\
             }",
    ) {
        assert_eq!(out, "ada\n4.5\n");
    }
}

#[test]
fn e2e_refinement_typed_struct_fields_layout() {
    // A struct whose fields name refinement aliases (`email: BoundedText`
    // where `BoundedText = String`, `price: PositivePrice` where `= f64`)
    // must size those fields at the BASE layout, not the `i64` unknown-name
    // fall-through. The refinement-base maps are now populated before
    // `build_struct_types`; pre-fix the field collapsed to `i64` and
    // construction tripped LLVM verification ("Invalid InsertValueInst").
    if let Some(out) = run_program(
            "pub type BoundedText = String where self.len() >= 1;\n\
             pub type PositivePrice = f64 where self > 0.0;\n\
             pub type PositiveQty = i64 where self > 0;\n\
             pub struct Row { name: BoundedText, price: PositivePrice, qty: PositiveQty }\n\
             fn main() {\n\
                 let r = Row { name: \"widget\" as BoundedText, price: 2.5 as PositivePrice, qty: 3 };\n\
                 println(f\"{r.name} {r.price} x{r.qty}\");\n\
             }",
        ) {
            assert_eq!(out, "widget 2.5 x3\n");
        }
}

#[test]
fn e2e_with_provider_constructor_call_provider() {
    // `with_provider[R](Type.new(args), || ...)` — the provider expression
    // is a constructor (associated-function) call, not a bare identifier or
    // struct literal. Codegen infers the concrete provider type from the
    // call's return type (`fn_return_type_names`). Pre-fix: "cannot infer
    // concrete provider type at codegen". The `examples/weave` FX provider.
    if let Some(out) = run_program(
        "pub trait Rate { fn factor(ref self) -> i64; }\n\
             pub effect resource Fx: Rate;\n\
             pub struct Fixed { f: i64 }\n\
             impl Fixed { pub fn new(f: i64) -> Fixed { Fixed { f: f } } }\n\
             impl Rate for Fixed { fn factor(ref self) -> i64 { self.f } }\n\
             pub fn scale(x: i64) -> i64 with reads(Fx) { x * Fx.factor() }\n\
             fn main() {\n\
                 with_provider[Fx](Fixed.new(3), || {\n\
                     println(f\"{scale(14)}\");\n\
                 });\n\
             }",
    ) {
        assert_eq!(out, "42\n");
    }
}

// ── Nested-place + compound-field assignment write-back (codegen) ──
//
// Regression: assignment to a value-type struct field through a projection
// (`o.inner.x = v`, depth >= 2; `v[i].field = v` on plain-struct elements)
// and compound assignment on field/index targets (`o.count += 1`) were
// silently dropped — codegen's assignment lowering only stored back for a
// bare-identifier target. Interpreter sibling fixed in 62a92b39; this is the
// codegen half. Pre-existing; surfaced by the Tangle dogfooding project.

#[test]
fn e2e_nested_plain_struct_field_write() {
    if let Some(out) = run_program(
        "struct Inner { x: i64 }\n\
             struct Outer { inner: Inner }\n\
             fn main() {\n\
                 let mut o = Outer { inner: Inner { x: 1 } };\n\
                 o.inner.x = 99;\n\
                 println(o.inner.x);\n\
             }",
    ) {
        assert_eq!(out, "99\n");
    }
}

#[test]
fn e2e_nested_plain_struct_field_write_three_levels() {
    if let Some(out) = run_program(
        "struct A { x: i64 }\n\
             struct B { a: A }\n\
             struct C { b: B }\n\
             fn main() {\n\
                 let mut c = C { b: B { a: A { x: 1 } } };\n\
                 c.b.a.x = 42;\n\
                 println(c.b.a.x);\n\
             }",
    ) {
        assert_eq!(out, "42\n");
    }
}

#[test]
fn e2e_compound_assign_on_field_and_nested() {
    if let Some(out) = run_program(
        "struct Inner { x: i64 }\n\
             struct Outer { mut count: i64, inner: Inner }\n\
             fn main() {\n\
                 let mut o = Outer { count: 0, inner: Inner { x: 5 } };\n\
                 o.count += 10;\n\
                 o.inner.x += 100;\n\
                 println(o.count);\n\
                 println(o.inner.x);\n\
             }",
    ) {
        assert_eq!(out, "10\n105\n");
    }
}

#[test]
fn test_e2e_user_struct_shadows_always_injected_stdlib_type() {
    // #34 (phase-12 self-hosting, parser stage): a user `struct Parser` (+
    // `impl Parser { fn new(items: Vec[i64]) }`) collides with the
    // always-injected `std.cli` `Parser` (whose `new` takes a `String`).
    // Before the fix, `Parser.new(vec)` resolved to cli's impl via
    // `env.impls` and type-errored `expected 'String', found 'Vec<i64>'`
    // (the #6 stdlib-collision pattern, now hit through an associated-fn
    // call rather than struct-literal construction). The collision-skip in
    // `register_baked_stdlib` drops the cli module when the user redefines a
    // type it exports, so `Parser.new` resolves to the user impl. The skip is
    // gated to NOT fire when a stdlib module self-compiles (`compiling_stdlib`).
    if let Some(out) = run_program(
        "struct Parser { items: Vec[i64], pos: i64 }\n\
             impl Parser {\n\
                 fn new(items: Vec[i64]) -> Parser { Parser { items: items, pos: 0 } }\n\
                 fn first(ref self) -> i64 { self.items[self.pos] }\n\
             }\n\
             fn main() {\n\
                 let mut v: Vec[i64] = Vec.new();\n\
                 v.push(7);\n\
                 let p = Parser.new(v);\n\
                 println(p.first().to_string());\n\
             }",
    ) {
        assert_eq!(out, "7\n");
    }
}

#[test]
fn test_e2e_recursive_struct_by_value_field_layout() {
    // #36 (phase-12 self-hosting, parser stage): a struct embedded BY VALUE
    // in another struct DECLARED EARLIER in source, in a recursive cycle
    // broken only by a shared-enum (pointer) edge. Source-order struct-type
    // building laid `Wrap` out before `Inner`, so `Wrap.inner: Inner`
    // collapsed to the `i64` placeholder → `Invalid InsertValueInst` at
    // module verification (the #1 family, extended to struct-by-value-field).
    // `build_struct_types` now builds in topological dependency order
    // (`Inner` before `Wrap`); the shared enum `Link` field is a pointer, so
    // it's not a build-order dep and the cycle resolves.
    if let Some(out) = run_program(
        "struct Wrap { inner: Inner, tag: i64 }\n\
             shared enum Link { Leaf(i64), Chain(Wrap) }\n\
             struct Inner { value: i64, next: Link }\n\
             fn main() {\n\
                 let w = Wrap { inner: Inner { value: 5, next: Link.Leaf(9) }, tag: 7 };\n\
                 println(w.tag.to_string());\n\
             }",
    ) {
        assert_eq!(out, "7\n");
    }
}

#[test]
fn test_e2e_user_struct_shadows_stdlib_tracing_span() {
    // Regression for self-hosting blocker #6: a user `struct Span` — the
    // single most natural name for a lexer/compiler, and what every
    // self-hosting stage uses — collided with the always-injected
    // `std.tracing` `struct Span { name, span_id, parent_id, fields }`.
    // codegen's `struct_types` is flat name-keyed, so `declare_stdlib_program`
    // overwrote the user's `Span` and built the user's literal against the
    // tracing layout → `Invalid InsertValueInst operands` at verification.
    // Fix: skip declaring+compiling a real-source stdlib module (tracing)
    // when the user redefines one of its exported type names. Here the user
    // `Span { line, column, offset, length }` (constructed inside a method
    // that returns it — the `make_spanned` shape that triggered the bug)
    // must build and read back its fields correctly with tracing skipped.
    if let Some(out) = run_program(
        "struct Span { line: i64, column: i64, offset: i64, length: i64 }\n\
             struct Lexer { start: i64, current: i64, line: i64 }\n\
             impl Lexer {\n\
                 fn span(ref self) -> Span {\n\
                     Span { line: self.line, column: 1, offset: self.start,\n\
                            length: self.current - self.start }\n\
                 }\n\
             }\n\
             fn main() {\n\
                 let lx = Lexer { start: 2, current: 5, line: 1 };\n\
                 let sp = lx.span();\n\
                 println(f\"{sp.offset} {sp.length} {sp.line} {sp.column}\");\n\
             }",
    ) {
        assert_eq!(out, "2 3 1 1\n");
    }
}

/// Positive guard: the `for (i, x)` enumerate DESTRUCTURE path (the handled
/// case) must keep compiling and producing the right answer — the
/// B-2026-07-14-7 loud-bail must not regress it.
#[test]
fn e2e_for_enumerate_destructure_still_works() {
    if let Some(out) = run_program(
        "fn main() {\n\
                 let a: Vec[i64] = Vec[10i64, 20i64, 30i64];\n\
                 let mut s = 0i64;\n\
                 for (i, x) in a.iter().enumerate() { s = s + i * x; }\n\
                 println(f\"{s}\");\n\
             }\n",
    ) {
        // 0*10 + 1*20 + 2*30 = 80
        assert_eq!(out, "80\n");
    }
}

#[test]
fn test_e2e_size_of_user_struct() {
    // `struct Point { x: i64, y: i64 }` → 16 bytes on a 64-bit target.
    let out = run_program(
        "struct Point { x: i64, y: i64 }\n\
             fn main() { println(size_of[Point]()); }",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "16");
    }
}

#[test]
fn test_e2e_align_of_user_union() {
    let out = run_program(
        "#[repr(C)] union FloatBits { f: f32, bits: u32 }\n\
             fn main() { println(align_of[FloatBits]()); }",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "4");
    }
}

#[test]
fn test_e2e_union_and_struct_raw_pointer_fields_are_copy() {
    // B-2026-08-12-7, the direct pins. A `*const T` field is as Copy as a
    // `*mut T` one, and the same predicate backs `#[derive(Copy)]` on a
    // struct — so a Copy struct holding a raw pointer must also be
    // accepted. Both were rejected before the fix.
    let out = run_program(
        "#[repr(C)] union Slot { c: *const u8, m: *mut i64, n: i64 }\n\
             #[derive(Copy, Clone)] struct Handle { p: *mut u8, tag: i64 }\n\
             fn main() {\n\
                 println(size_of[Slot]());\n\
                 let mut buf: Array[u8, 2] = [1u8, 2u8];\n\
                 let h = Handle { p: buf.as_mut_ptr(), tag: 7 };\n\
                 let g = h;\n\
                 println(g.tag + h.tag);\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(out, "8\n14\n");
    }
}

#[test]
fn test_e2e_union_field_assignment_persists_through_read() {
    // Slice 2a's `assigning_lhs` flag makes union-field assignment
    // unconditionally safe (no `unsafe { }` required); the read
    // back is what trips the gate. Pin the codegen contract that
    // the store actually persists into the storage slot rather
    // than landing in a discarded SSA register.
    let out = run_program(
        "#[repr(C)] union BitsLR { l: u32, r: u32 }\n\
             fn main() {\n\
                 let mut u = BitsLR { l: 1u32 };\n\
                 u.r = 7u32;\n\
                 let v = unsafe { u.l };\n\
                 println(v);\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "7");
    }
}

#[test]
fn test_ir_ptr_const_on_field_place_compiles() {
    // `ptr.const(place)` over a field-access place must resolve to a GEP'd
    // field address (`ptr_place_addr`), not fall through — matching the
    // typechecker's place-validator, which already accepts field / index /
    // tuple-index places.
    let ir = ir_for(
        "struct Reg { status: i32, control: i32 } \
             fn main() { let r: Reg = Reg { status: 1, control: 2 }; \
             let p: *const i32 = ptr.const(r.control); }",
    );
    assert!(
        !ir.contains("method dispatch fell through"),
        "ptr.const on a field place must not fall through; got IR:\n{ir}"
    );
}

#[test]
fn test_e2e_struct_field_access() {
    let out = run_program(
        r#"
struct Point { x: i64, y: i64 }
fn sum(p: Point) -> i64 { p.x + p.y }
fn main() {
    let p = Point { x: 3, y: 4 };
    println(sum(p));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "7");
    }
}

#[test]
fn test_e2e_multiple_structs() {
    let out = run_program(
        r#"
struct Vec2 { x: i64, y: i64 }
fn dot(a: Vec2, b: Vec2) -> i64 { a.x * b.x + a.y * b.y }
fn main() {
    let a = Vec2 { x: 1, y: 2 };
    let b = Vec2 { x: 3, y: 4 };
    println(dot(a, b));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "11"); // 1*3 + 2*4 = 11
    }
}

/// B-2026-06-21-3: an **un-annotated** fn value read out of a struct field
/// (`let g = h.f;`, no `: Fn(..)`) and called. The lowering pass records the
/// field-read expression's inferred `Fn(..)` type in `fn_value_typed_exprs`,
/// so `let_binding_fn_value_type` registers `g` in `closure_fn_types` and
/// `g(x)` lowers to an indirect call. Before this it fell through to the
/// unknown-callee path and silently returned 0.
#[test]
fn fn_value_struct_field_unannotated_extraction() {
    let out = run_program(
        "fn doubler(n: i64) -> i64 { n * 2i64 }\n\
             struct H { f: Fn(i64) -> i64 }\n\
             fn main() { let h = H { f: doubler }; let g = h.f; println(f\"{g(21i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("42\n"));
}

/// B-2026-06-21-2: a fn value stored in a struct field (`H { f: doubler }`)
/// — the field initializer lowers to a fat pointer matching the
/// `f: Fn(...)` field slot. The explicit-annotation extraction path
/// (`let g: Fn(..) = h.f`); the un-annotated form is covered above
/// (B-2026-06-21-3).
#[test]
fn fn_value_struct_field_annotated_extraction() {
    let out = run_program(
            "fn doubler(n: i64) -> i64 { n * 2i64 }\n\
             struct H { f: Fn(i64) -> i64 }\n\
             fn main() { let h = H { f: doubler }; let g: Fn(i64) -> i64 = h.f; println(f\"{g(21i64)}\"); }\n",
        );
    assert_eq!(out.as_deref(), Some("42\n"));
}

#[test]
fn test_ir_bug8_method_call_chain_field_emits_load_and_dec() {
    // IR-level gate for the MethodCall arm. Same shape as
    // `test_ir_bug8_call_chain_field_emits_load_and_dec` (the
    // free-fn version) but with a MethodCall receiver. Pin the
    // `sh_call_val` GEP label and the typed printf %val operand.
    let ir = ir_for(
        r#"
shared struct S { val: i64 }
struct Holder { tag: i64 }
impl Holder {
    fn make(self) -> S {
        let s = S { val: 99 };
        s
    }
}
fn main() {
    let h = Holder { tag: 0 };
    println(h.make().val);
}
"#,
    );
    assert!(
        ir.contains("sh_call_val"),
        "main should GEP into the method-call result's `val` field; \
             label `sh_call_val` not found in:\n{}",
        ir
    );
    let main_body = ir.split("define i32 @main()").nth(1).expect("main fn body");
    assert!(
        main_body.contains("karac_runtime_i64_to_str") && main_body.contains("i64 %val"),
        "main's integer print call should receive the loaded field value; \
             not found in main body:\n{}",
        main_body
    );
}

#[test]
fn test_ir_bug8_call_chain_field_emits_load_and_dec() {
    // IR-level gate. The call-chain field-access path must emit a
    // typed load of the field (not the `i64 0` fall-through) and
    // an `rc_dec` against the call-result temp so the RC the
    // callee handed us is released. Before the fix neither was
    // emitted; after the fix both are present in `@main`.
    let ir = ir_for(
        r#"
shared struct Node { val: i64 }
fn helper() -> Node {
    let n = Node { val: 42 };
    n
}
fn main() {
    println(helper().val);
}
"#,
    );
    // The fixed lowering names the field load through the
    // `sh_call_<field>` label and emits an `rc_dec` against the
    // call-result pointer (`%call`).
    assert!(
        ir.contains("sh_call_val"),
        "main should GEP into the call result's field"
    );
    // The `printf` call should be parameterized on the loaded
    // field, not on a constant `i64 0`.
    let main_body = ir.split("define i32 @main()").nth(1).expect("main fn body");
    assert!(
        main_body.contains("karac_runtime_i64_to_str") && main_body.contains("i64 %val"),
        "main's integer print call should receive the loaded field value"
    );
}

/// B-2026-08-27-22 — a METHOD CALL on an element of an SoA-laid-out
/// container read the wrong data, and a mutating one wrote to the wrong
/// place. Both silent; both correct under AoS and the interpreter.
///
/// `compile_indexed_receiver_method` lowers an index receiver to a single
/// element POINTER and binds a synth identifier to it. An SoA element has
/// no such pointer — its fields live one buffer per `layout` group — so the
/// lowering GEP'd into whichever group buffer it landed on:
/// `entities[5].clone()` returned `100 1000 200 2000`, the cold group at
/// indices 1 and 2. The element is now materialised through the same
/// `compile_soa_index_read` reassembly `entities[i].x` already uses.
///
/// `mut4` IS THE HALF THE ROW DID NOT NAME, and it needs more than the
/// read fix. A materialised element is a COPY, so a `mut ref self` method
/// writes into the copy and the container never sees it — `entities[4]
/// .bump()` was a silent no-op even once the read was right. The scatter
/// back on a `MutRef` receiver is what closes that, and `mut4` is the only
/// line that can tell a correct write-back from no write-back at all.
///
/// `neighbour3` / `neighbour5` are the write-back's blast radius: the
/// scatter must land on element 4 alone. Writing a whole materialised
/// element back through the group buffers is exactly the operation that
/// could smear across its neighbours, and index 5 sits past the cap
/// 0→4→8 realloc boundary so a mis-strided store shows up there first.
///
/// RUN UNDER BOTH LAYOUTS AND COMPARED, like
/// `test_e2e_soa_whole_element_matches_aos`. That differential is what
/// caught this in the first place: every line here passes under AoS, so a
/// single-layout fixture asserts nothing about the bug.
#[test]
fn test_e2e_soa_element_method_matches_aos() {
    let body = r#"
#[derive(Clone)]
struct Entity { x: i64, y: i64, vx: i64, vy: i64 }
LAYOUT
impl Entity {
    fn sum(ref self) -> i64 { return self.x + self.y + self.vx + self.vy; }
    fn bump(mut ref self) { self.x = self.x + 1000; self.vy = self.vy + 7; }
}
fn main() {
    let mut entities: Vec[Entity] = Vec.new();
    let mut i: i64 = 0;
    while i < 6 {
        entities.push(Entity { x: i, y: i * 10, vx: i * 100, vy: i * 1000 });
        i = i + 1;
    }
    let c = entities[5].clone();
    println(f"clone5 {c.x} {c.y} {c.vx} {c.vy}");
    let d = entities[3].clone();
    println(f"clone3 {d.x} {d.y} {d.vx} {d.vy}");
    println(f"read5 {entities[5].sum()}");
    println(f"read0 {entities[0].sum()}");
    entities[4].bump();
    println(f"mut4 {entities[4].x} {entities[4].y} {entities[4].vx} {entities[4].vy}");
    println(f"neighbour3 {entities[3].x} {entities[3].vy}");
    println(f"neighbour5 {entities[5].x} {entities[5].vy}");
    println(f"field5 {entities[5].vx}");
}
"#;
    let soa_src = body.replace(
        "LAYOUT",
        "layout entities: Vec[Entity] {\n    group pos { x, y }\n    group vel { vx, vy }\n}",
    );
    let aos_src = body.replace("LAYOUT", "");
    let expected = vec![
        "clone5 5 50 500 5000",
        "clone3 3 30 300 3000",
        "read5 5555",
        "read0 0",
        "mut4 1004 40 400 4007",
        "neighbour3 3 3000",
        "neighbour5 5 5000",
        "field5 500",
    ];
    if let Some(aos) = run_program(&aos_src) {
        let lines: Vec<&str> = aos.trim().lines().collect();
        assert_eq!(lines, expected, "AoS baseline output mismatch");
    }
    if let Some(soa) = run_program(&soa_src) {
        let lines: Vec<&str> = soa.trim().lines().collect();
        assert_eq!(lines, expected, "SoA element-method output must match AoS");
    }
}

#[test]
fn test_e2e_soa_whole_element_matches_aos() {
    // Whole-element binding `let e = entities[i]` on a SoA-laid-out
    // Vec[Entity] must materialize exactly the values an AoS
    // Vec[Entity] (no layout block) produces. Running the identical
    // program with and without the layout block and asserting equal
    // output validates both the new SoA read path
    // (compile_soa_index_read) AND the previously-untestable
    // push/growth decomposition — 6 pushes cross the cap 0→4→8
    // realloc boundary for every group buffer, so a mis-reallocated
    // group would surface as a read mismatch here.
    //
    // Reads FIELDS off the index directly (`entities[i].x`). It bound the
    // whole element until direct field access on an Index receiver worked
    // for plain-struct Vecs; that gap is gone (verified in both layouts on
    // both backends), and the whole-element form is not available any more
    // regardless, since it moves a non-`Copy` element out of a container
    // (B-2026-08-26-21). NEITHER replacement for it works here: `ref` cannot
    // borrow what has no contiguous storage, but `entities[i].clone()` now
    // works under SoA: it used to read back `100 1000 200 2000` for element
    // 5 — the cold group at the wrong indices — because a method call on an
    // index receiver lowered to a single element pointer an SoA layout does
    // not have. Fixed in B-2026-08-27-22 by materialising the element;
    // `test_e2e_soa_element_method_matches_aos` covers it.
    let body = r#"
struct Entity { x: i64, y: i64, vx: i64, vy: i64 }
LAYOUT
fn main() {
    let mut entities: Vec[Entity] = Vec.new();
    let mut i: i64 = 0;
    while i < 6 {
        entities.push(Entity { x: i, y: i * 10, vx: i * 100, vy: i * 1000 });
        i = i + 1;
    }
    println(entities.len());
    println(entities[0].x);
    println(entities[4].y);
    println(entities[5].vx);
    println(entities[5].vy);
    println(entities[3].x);
    println(entities[3].y);
    println(entities[3].vx);
    println(entities[3].vy);
}
"#;
    let soa_src = body.replace(
        "LAYOUT",
        "layout entities: Vec[Entity] {\n    group pos { x, y }\n    group vel { vx, vy }\n}",
    );
    let aos_src = body.replace("LAYOUT", "");

    let expected = vec!["6", "0", "40", "500", "5000", "3", "30", "300", "3000"];

    if let Some(aos) = run_program(&aos_src) {
        let lines: Vec<&str> = aos.trim().lines().collect();
        assert_eq!(lines, expected, "AoS baseline output mismatch");
    }
    if let Some(soa) = run_program(&soa_src) {
        let lines: Vec<&str> = soa.trim().lines().collect();
        assert_eq!(lines, expected, "SoA read output must match AoS baseline");
    }
}

#[test]
fn test_e2e_soa_indexed_field_access() {
    // The headline SoA access shape `entities[i].field` (direct field
    // access on an indexed SoA element) — materialize-then-extract via
    // the compile_field_access SoA branch. Field reads span both hot
    // groups (pos: x/y, vel: vx/vy) and an index past the first
    // realloc (i=4, 5).
    let src = r#"
struct Entity { x: i64, y: i64, vx: i64, vy: i64 }
layout entities: Vec[Entity] {
    group pos { x, y }
    group vel { vx, vy }
}
fn main() {
    let mut entities: Vec[Entity] = Vec.new();
    let mut i: i64 = 0;
    while i < 6 {
        entities.push(Entity { x: i, y: i * 10, vx: i * 100, vy: i * 1000 });
        i = i + 1;
    }
    println(entities[0].x);
    println(entities[4].y);
    println(entities[5].vx);
    println(entities[5].vy);
}
"#;
    if let Some(out) = run_program(src) {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["0", "40", "500", "5000"]);
    }
}

#[test]
fn test_e2e_soa_literal_construction_field_read() {
    // B-2026-07-10-7: a LITERAL-constructed SoA binding
    // (`let world: Vec[E] = [E{..}, ..]`) then a field/index read. Before the
    // fix the literal built an AoS `{ptr,len,cap}` header into the SoA slot,
    // so `world[i].field` decoded it as the multi-group SoA struct and
    // dereferenced a garbage group pointer → SIGSEGV (compiled binary only;
    // the interpreter computed it correctly — a run-vs-build divergence).
    // The literal is now decomposed through the `push` path (per-group backing
    // arrays allocated + populated). Covers >4 elements (past the first
    // realloc), a cold group, and both bare `[..]` and prefix `Vec[..]` forms.
    let src = r#"
struct Entity { x: i64, y: i64, vx: i64, vy: i64 }
layout world: Vec[Entity] {
    group pos { x, y }
    group vel { vx }
    cold { vy }
}
fn main() {
    let world: Vec[Entity] = [
        Entity { x: 0, y: 1, vx: 2, vy: 3 },
        Entity { x: 10, y: 11, vx: 12, vy: 13 },
        Entity { x: 20, y: 21, vx: 22, vy: 23 },
        Entity { x: 30, y: 31, vx: 32, vy: 33 },
        Entity { x: 40, y: 41, vx: 42, vy: 43 },
        Entity { x: 50, y: 51, vx: 52, vy: 53 }
    ];
    println(world.len());
    println(world[0].x);
    println(world[4].y);
    println(world[5].vx);
    println(world[5].vy);
    let e = world[2];
    println(e.vy);
}
"#;
    if let Some(out) = run_program(src) {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["6", "0", "41", "52", "53", "23"]);
    }
}

#[test]
fn test_e2e_soa_pop_returns_last_element() {
    // SoA `pop` materializes the trailing element across every
    // group (and the cold buffer, if present) by scatter-reading
    // each group's sub-struct at len-1, then decrements the shared
    // len. The returned value is wrapped in `Option[T]`.
    let src = r#"
struct Entity { x: i64, y: i64, hp: i64 }
layout entities: Vec[Entity] {
    group physics { x, y }
    group combat { hp }
}
fn main() {
    let mut entities: Vec[Entity] = Vec.new();
    entities.push(Entity { x: 1, y: 10, hp: 100 });
    entities.push(Entity { x: 2, y: 20, hp: 200 });
    entities.push(Entity { x: 3, y: 30, hp: 300 });
    println(entities.len());
    match entities.pop() {
        Some(e) => {
            println(e.x);
            println(e.y);
            println(e.hp);
        }
        None => println(-1),
    }
    println(entities.len());
}
"#;
    if let Some(out) = run_program(src) {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["3", "3", "30", "300", "2"]);
    }
}

#[test]
fn test_e2e_soa_pop_on_empty_returns_none() {
    // Empty SoA: `pop` must return `None` and leave len untouched
    // (= 0). Exercises the cap=0 / len=0 short-circuit alongside
    // the Option-tag merge.
    let src = r#"
struct Entity { x: i64, y: i64 }
layout entities: Vec[Entity] {
    group physics { x, y }
}
fn main() {
    let mut entities: Vec[Entity] = Vec.new();
    match entities.pop() {
        Some(e) => println(e.x),
        None => println(-1),
    }
    println(entities.len());
}
"#;
    if let Some(out) = run_program(src) {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["-1", "0"]);
    }
}

/// B-2026-09-14-29 — an index-assign over an `Array[T, N]` runs the
/// DISPLACED element's `Drop` body, at the store, on every compiled
/// surface. The interpreter always did, so this was a run/build
/// divergence the kata A/B gate could see.
///
/// The `Vec[D]` cell is the ORACLE rather than a control: it was correct
/// throughout, and it is what identified the owner as the drop emitter's
/// Vec-only element-type lookup rather than the store path. If a future
/// change regresses the two together, the Array and Vec cells move as a
/// pair and that is the tell.
///
/// The no-heap `F` cell isolates the BODY half with nothing to leak, so a
/// fix that only closed the memory half fails here while the sanitizer
/// suite stays green.
/// B-2026-09-15-28 — a `Vec[T]`-typed struct FIELD assigned a fresh
/// container runs the DISPLACED elements' `Drop` bodies.
///
/// `emit_displaced_field_bodies` keys on the field's HEAD type name, so a
/// container field answered `"Vec"` — no declared struct, no enum layout —
/// and the gate returned before any bodies ran. The surviving generation
/// fired at scope exit and the displaced one never did, so this printed
/// `dD3 dD4 mid` where the IDENTIFIER position (correct since
/// B-2026-09-14-23) printed all four.
///
/// Both backends were silent rather than divergent — the interpreter's
/// displaced-field `match` let a `Value::Array` fall through its struct and
/// enum arms — so both halves landed in ONE commit. Moving one alone turns
/// an agreed gap into a run-vs-build divergence, which is what
/// B-2026-09-15-23 measured when a sibling was tried codegen-first; the
/// interpreter twin is `test_field_assign_runs_the_displaced_containers_element_bodies`
/// and asserts this same string.
///
/// BODIES ONLY: the displaced buffer's memory is already reclaimed by
/// `compile_field_store`'s old-value drop, which is why this row measured
/// valgrind-clean before the fix and still does after (`All heap blocks
/// were freed`, 0 errors).
#[test]
fn e2e_field_assign_runs_the_displaced_containers_element_bodies() {
    const H: &str = "struct D { id: i64, s: String }\n\
             impl Drop for D { fn drop(mut ref self) { println(f\"dD{self.id}\") } }\n\
             fn mkd(n: i64) -> D { return D { id: n, s: f\"heap-{n}\" }; }\n";
    // The FIELD position — the row's shape.
    assert_eq!(
        run_program(&format!(
            "{H}struct H2 {{ v: Vec[D] }}\n\
                 fn main() {{\n\
                 \x20   let mut h: H2 = H2 {{ v: [mkd(1), mkd(2)] }};\n\
                 \x20   h.v = [mkd(3), mkd(4)];\n\
                 \x20   println(\"mid\");\n\
                 }}"
        ))
        .as_deref(),
        Some("dD1\ndD2\ndD3\ndD4\nmid\n")
    );
    // THE ORACLE — the identifier position, correct before and after.
    assert_eq!(
        run_program(&format!(
            "{H}fn main() {{\n\
                 \x20   let mut v: Vec[D] = [mkd(1), mkd(2)];\n\
                 \x20   v = [mkd(3), mkd(4)];\n\
                 \x20   println(\"mid\");\n\
                 }}"
        ))
        .as_deref(),
        Some("dD1\ndD2\ndD3\ndD4\nmid\n")
    );
}

#[test]
fn test_e2e_byvalue_aggregate_param_transferred_out_struct() {
    // #14: the STRUCT half — proves the gap was never enum-specific. An owned
    // by-value struct param consumed into a returned struct literal
    // (`Wrap { t: t }`) previously double-freed the inner String buffer the
    // caller's source `x` also frees.
    if let Some(out) = run_program(
        r#"
struct Inner { s: String }
struct Wrap { t: Inner }
fn wrap(t: Inner) -> Wrap { Wrap { t: t } }
fn main() {
    let x = Inner { s: "hi".to_string() };
    let w = wrap(x);
    println(w.t.s);
}
"#,
    ) {
        assert_eq!(out, "hi\n");
    }
}

#[test]
fn test_e2e_indexed_receiver_user_struct_method() {
    // Vec[Counter] indexed receiver dispatching through `Counter.bump`.
    // Verifies var_type_names wiring for synth identifiers and that
    // the mut-ref-self method writes back through the elem pointer.
    let out = run_program(
        r#"
struct Counter { n: i64 }
impl Counter {
    fn bump(mut ref self) { self.n = self.n + 1; }
    fn read(ref self) -> i64 { self.n }
}
fn main() {
    let mut v: Vec[Counter] = Vec.new();
    v.push(Counter { n: 10 });
    v.push(Counter { n: 20 });
    v[0].bump();
    v[0].bump();
    v[1].bump();
    println(v[0].read());
    println(v[1].read());
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["12", "21"]);
    }
}

// ── SoA layout codegen ────────────────────────────────────────

#[test]
fn test_ir_soa_layout_type() {
    let ir = ir_for(
        r#"
struct Entity { x: f64, y: f64, hp: i64 }
layout entities: Vec[Entity] {
    group physics { x, y }
    group combat { hp }
}
fn main() {
    let entities: Vec[Entity] = Vec.new();
}
"#,
    );
    // SoA Vec with 2 groups → { ptr, ptr, i64, i64 }
    assert!(
        ir.contains("{ ptr, ptr, i64, i64 }"),
        "expected SoA struct {{ ptr, ptr, i64, i64 }}, got:\n{}",
        ir
    );
}

#[test]
fn test_e2e_soa_by_value_param_read_across_function() {
    // B-2026-06-19-14 slice 1: pass a SoA-laid-out `Vec[E]` BY VALUE
    // (not `ref`) to another function. The param `es` matches `layout es`,
    // so its signature type is the 4-field SoA struct (not AoS
    // `{ptr,len,cap}`) — without the fix the caller marshalled a 4-field
    // value into a 3-field param slot (LLVM "Call parameter type does not
    // match function signature" verification failure). Caller-retains
    // ownership (like an owned by-value AoS Vec param), so `main`'s `es`
    // frees the buffers once after the call; the callee borrows.
    let out = run_program(
        r#"
struct E { x: f64, y: f64 }
layout es: Vec[E] { group g1 { x } group g2 { y } }
fn sumall(es: Vec[E]) -> f64 {
    let mut s = 0.0;
    let mut i = 0;
    while i < es.len() {
        let e = es[i];
        s = s + e.x + es[i].y;
        i = i + 1;
    }
    s
}
fn main() {
    let mut es: Vec[E] = Vec.new();
    es.push(E { x: 1.0, y: 2.0 });
    es.push(E { x: 3.0, y: 4.0 });
    es.push(E { x: 5.0, y: 6.0 });
    println(sumall(es));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out.trim(),
            "21",
            "SoA Vec read by value across a function boundary"
        );
    }
}

#[test]
fn test_e2e_soa_by_value_param_caller_different_name() {
    // Per-layout monomorphization slice 2: pass a SoA-laid-out `Vec[E]` BY
    // VALUE to a helper whose param name does NOT match the layout block.
    // The caller binding is `grid` (`layout grid`); the callee param is
    // `data`. The name-keyed by-value path can't lower `data` SoA (no
    // `layout data` block), so without slice 2 the caller marshals a
    // 4-field SoA struct into a 3-field AoS param slot — an LLVM signature
    // mismatch. Slice 2's forward layout-flow inference monomorphizes
    // `process$soa_grid` with `data` lowered as the SoA struct, keyed on
    // the caller's argument layout rather than the param name.
    let out = run_program(
        r#"
struct E { x: f64, y: f64 }
layout grid: Vec[E] { group g1 { x } group g2 { y } }
fn process(data: Vec[E]) -> f64 {
    let mut s = 0.0;
    let mut i = 0;
    while i < data.len() {
        let e = data[i];
        s = s + e.x + data[i].y;
        i = i + 1;
    }
    s
}
fn main() {
    let mut grid: Vec[E] = Vec.new();
    grid.push(E { x: 1.0, y: 2.0 });
    grid.push(E { x: 3.0, y: 4.0 });
    grid.push(E { x: 5.0, y: 6.0 });
    println(process(grid));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out.trim(),
            "21",
            "SoA Vec by value into a differently-named param (layout-mono)"
        );
    }
}

#[test]
fn test_e2e_soa_two_layouts_through_one_helper_distinct_monos() {
    // Per-layout monomorphization slice 2 (collision-free mangling): two
    // distinct `layout` blocks (`grid`, `coll`) over the same element type
    // flow through ONE helper `process`. Each call monomorphizes a distinct
    // symbol (`process$data_soa_grid`, `process$data_soa_coll`) — the param
    // name is part of the layout mangle, so different layout assignments
    // can't collide into one symbol. Both must compute correctly: grid sums
    // (1+2)+(3+4)+(5+6)=21, coll sums (1+1)+(2+2)=6 — printed on two lines.
    let out = run_program(
        r#"
struct E { x: f64, y: f64 }
layout grid: Vec[E] { group g1 { x } group g2 { y } }
layout coll: Vec[E] { group c1 { x } group c2 { y } }
fn process(data: Vec[E]) -> f64 {
    let mut s = 0.0;
    let mut i = 0;
    while i < data.len() {
        s = s + data[i].x + data[i].y;
        i = i + 1;
    }
    s
}
fn main() {
    let mut grid: Vec[E] = Vec.new();
    grid.push(E { x: 1.0, y: 2.0 });
    grid.push(E { x: 3.0, y: 4.0 });
    grid.push(E { x: 5.0, y: 6.0 });
    let mut coll: Vec[E] = Vec.new();
    coll.push(E { x: 1.0, y: 1.0 });
    coll.push(E { x: 2.0, y: 2.0 });
    println(process(grid));
    println(process(coll));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out.trim(),
            "21\n6",
            "two distinct layouts through one helper produce distinct correct monos"
        );
    }
}

#[test]
fn test_e2e_soa_return_value_caller_different_name() {
    // Per-layout monomorphization slice 3 (SoA returns / backward
    // inference): a helper `build_grid()` builds and RETURNS a `Vec[E]`,
    // bound by the caller into a SoA `grid` (`layout grid`). The returned
    // local is named `out` — a DIFFERENT name from both the layout block
    // and the receiving binding — so the name-keyed model can't lower it
    // SoA (no `layout out`). Backward inference reads the receiving
    // binding's layout, monomorphizes `build_grid$ret_soa_grid` to return
    // the 4-field SoA struct, seeds `out` as `Soa(grid)` so its
    // construction/pushes/tail all lower SoA, and moves the buffers out to
    // the caller (callee suppresses its own `FreeSoaGroups`). `main` reads
    // through the SoA struct: (1+2)+(3+4)+(5+6) = 21.
    let out = run_program(
        r#"
struct E { x: f64, y: f64 }
layout grid: Vec[E] { group g1 { x } group g2 { y } }
fn build_grid() -> Vec[E] {
    let mut out: Vec[E] = Vec.new();
    out.push(E { x: 1.0, y: 2.0 });
    out.push(E { x: 3.0, y: 4.0 });
    out.push(E { x: 5.0, y: 6.0 });
    out
}
fn main() {
    let grid: Vec[E] = build_grid();
    let mut s = 0.0;
    let mut i = 0;
    while i < grid.len() {
        s = s + grid[i].x + grid[i].y;
        i = i + 1;
    }
    println(s);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out.trim(),
            "21",
            "SoA Vec returned across a function boundary, bound by a differently-named local"
        );
    }
}

#[test]
fn test_e2e_soa_return_two_layouts_through_one_builder_distinct_monos() {
    // Per-layout monomorphization slice 3 (backward distinctness): ONE
    // no-arg builder `make()` is called twice, its result bound into two
    // different SoA layouts (`grid`, `coll`). Each `let` monomorphizes a
    // distinct return-SoA symbol (`make$ret_soa_grid`,
    // `make$ret_soa_coll`) keyed on the receiving binding's layout — the
    // backward analog of the forward two-layouts-through-one-helper test.
    // Both must read correctly: grid (1+2)+(3+4)=10, coll the same data
    // grouped differently → also 10. Printed on two lines.
    let out = run_program(
        r#"
struct E { x: f64, y: f64 }
layout grid: Vec[E] { group g1 { x } group g2 { y } }
layout coll: Vec[E] { group c1 { x } group c2 { y } }
fn make() -> Vec[E] {
    let mut buf: Vec[E] = Vec.new();
    buf.push(E { x: 1.0, y: 2.0 });
    buf.push(E { x: 3.0, y: 4.0 });
    buf
}
fn sum_grid(g: Vec[E]) -> f64 {
    let mut s = 0.0;
    let mut i = 0;
    while i < g.len() {
        s = s + g[i].x + g[i].y;
        i = i + 1;
    }
    s
}
fn main() {
    let grid: Vec[E] = make();
    let coll: Vec[E] = make();
    println(sum_grid(grid));
    println(sum_grid(coll));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out.trim(),
            "10\n10",
            "one builder bound into two layouts produces two distinct correct return-SoA monos"
        );
    }
}

// ── SoA cross-function, slice 6 (Slipstream full-SoA proof) ────

#[test]
fn test_e2e_soa_counted_loop_fill_returned() {
    // Slice 6: a SoA `Vec[E]` filled by a COUNTED loop and returned. The
    // `presize` lowering pass rewrites `let mut grid = Vec.new()` into
    // `Vec.with_capacity(n)` when a `while i < n { grid.push(..) }` fills it
    // (the `init_grid`/`fan_collide` shape). The SoA let path only recognized
    // `Vec.new`, so the capacity-rewritten binding fell through to the AoS
    // `{ptr,len,cap}` path while its layout was the 4-field SoA struct — an
    // LLVM return-type / use-site mismatch. `is_vec_with_capacity_call` now
    // routes it to the SoA constructor. build(3): x=0,1,2 y=0,2,4 → 9.
    let out = run_program(
        r#"
struct E { x: f64, y: f64 }
layout grid: Vec[E] { group g1 { x } group g2 { y } }
fn build(n: i64) -> Vec[E] {
    let mut grid: Vec[E] = Vec.new();
    let mut i = 0;
    while i < n { grid.push(E { x: i as f64, y: (i * 2) as f64 }); i = i + 1; }
    grid
}
fn main() with panics {
    let g: Vec[E] = build(3);
    let mut s = 0.0;
    let mut i = 0;
    while i < g.len() { s = s + g[i].x + g[i].y; i = i + 1; }
    println(s);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out.trim(),
            "9",
            "SoA Vec filled by a counted loop (presize -> with_capacity) must lower SoA"
        );
    }
}

#[test]
fn test_e2e_soa_returned_local_name_matches_layout() {
    // Slice 6: a builder whose RETURNED local is named after a `layout` block
    // (`grid`). The base (AoS-return) symbol name-matched the local SoA in its
    // body while its signature returned the AoS `{ptr,len,cap}` — a body/sig
    // mismatch. The fix suppresses `seed_binding_site_layout`'s origin
    // name-match for a returned local (its layout is the return mono's, not
    // its name); the SoA-returning specialization is the `$ret_soa_grid` mono.
    // make(): grid[0].x + grid[1].y = 1 + 4 = 5.
    let out = run_program(
        r#"
struct E { x: f64, y: f64 }
layout grid: Vec[E] { group g1 { x } group g2 { y } }
fn make() -> Vec[E] {
    let mut grid: Vec[E] = Vec.new();
    grid.push(E { x: 1.0, y: 2.0 });
    grid.push(E { x: 3.0, y: 4.0 });
    grid
}
fn main() with panics {
    let g: Vec[E] = make();
    println(g[0].x + g[1].y);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out.trim(),
            "5",
            "a returned local named like a layout block must stay base/sig-consistent"
        );
    }
}

#[test]
fn test_e2e_soa_reassign_carried_buffer() {
    // Slice 6: the carried-grid double-buffer move `grid = bump(grid)` — a
    // reassignment (not a `let`) of an existing SoA binding to a
    // layout-returning call, the heart of a stateful sim's per-frame loop.
    // The let arm had `compile_soa_let_from_call`; the assignment arm did not,
    // so the call returned the AoS struct (no backward mono) and the 3-field
    // value was stored into the 4-field SoA slot → garbage group pointers →
    // SIGSEGV on the next index. `compile_soa_assign_from_call` parks the
    // return layout, frees the OLD groups (caller-retains by-value param), and
    // stores the new SoA header. Run twice (the loop shape): x0 1->3, x1 3->5
    // → 8.
    let out = run_program(
        r#"
struct E { x: f64, y: f64 }
layout grid: Vec[E] { group g1 { x } group g2 { y } }
fn bump(g: Vec[E]) -> Vec[E] {
    let mut out: Vec[E] = Vec.new();
    let mut i = 0;
    while i < g.len() { let e = g[i]; out.push(E { x: e.x + 1.0, y: e.y }); i = i + 1; }
    out
}
fn main() with panics {
    let mut grid: Vec[E] = Vec.new();
    grid.push(E { x: 1.0, y: 2.0 });
    grid.push(E { x: 3.0, y: 4.0 });
    grid = bump(grid);
    grid = bump(grid);
    println(grid[0].x + grid[1].x);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out.trim(),
            "8",
            "SoA reassignment from a layout-returning call (carried-buffer double-buffer)"
        );
    }
}

#[test]
fn test_e2e_soa_tail_call_soa_return() {
    // Slice 6: a SoA-returning function whose body ENDS IN a layout-returning
    // call (`substep`'s `fan_stream(coll, …)` shape). The tail-identifier
    // return is seeded by `soa_return_local_names`; the tail-CALL had no
    // path, so the callee returned AoS while the signature was SoA. The
    // tail-call propagation in `compile_tail_final_expr` flows the function's
    // return layout to the tail call. step()->bump(): r[0].x = 1 + 1 = 2.
    let out = run_program(
        r#"
struct E { x: f64, y: f64 }
layout grid: Vec[E] { group g1 { x } group g2 { y } }
fn bump(g: Vec[E]) -> Vec[E] {
    let mut out: Vec[E] = Vec.new();
    let mut i = 0;
    while i < g.len() { let e = g[i]; out.push(E { x: e.x + 1.0, y: e.y }); i = i + 1; }
    out
}
fn step(g: Vec[E]) -> Vec[E] with panics { bump(g) }
fn main() with panics {
    let mut grid: Vec[E] = Vec.new();
    grid.push(E { x: 1.0, y: 2.0 });
    let r: Vec[E] = step(grid);
    println(r[0].x);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out.trim(),
            "2",
            "a SoA-returning fn whose tail is a layout-returning call must propagate the layout"
        );
    }
}

// ── SoA branch-leaf / multi-return (follow-on) ─────────────────

#[test]
fn test_e2e_soa_early_return_then_tail() {
    // Follow-on (branch-leaf / multi-`return` SoA returns): a guard-clause
    // helper — an EARLY `return a;` plus a tail `b`, the two flowing to a
    // SoA return. `soa_return_local_names` previously caught only the single
    // tail `b`, so the early `return a` lowered AoS against the SoA-patched
    // return signature → LLVM "return type does not match operand type"
    // verify failure. The recursive collector now seeds BOTH; the early
    // return's branch-safe move-out (`neutralize_moved_soa_groups_slot`)
    // keeps the fall-through free intact. pick(true) -> a -> 1 + 2 = 3.
    let out = run_program(
        r#"
struct E { x: f64, y: f64 }
layout grid: Vec[E] { group g1 { x } group g2 { y } }
fn pick(flag: bool) -> Vec[E] {
    let mut a: Vec[E] = Vec.new();
    a.push(E { x: 1.0, y: 2.0 });
    if flag {
        return a;
    }
    let mut b: Vec[E] = Vec.new();
    b.push(E { x: 3.0, y: 4.0 });
    b
}
fn main() with panics {
    let grid: Vec[E] = pick(true);
    println(grid[0].x + grid[0].y);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out.trim(),
            "3",
            "an early `return` SoA local must lower SoA like the tail (multi-return)"
        );
    }
}

#[test]
fn test_e2e_soa_if_else_both_return() {
    // Follow-on: BOTH arms of an if/else are explicit `return`s of distinct
    // SoA locals — neither is the block's tail expression, so both rely on
    // the recursive return collector. pick(true) -> a (1+2=3);
    // pick(false) -> b (3+4=7).
    let out = run_program(
        r#"
struct E { x: f64, y: f64 }
layout grid: Vec[E] { group g1 { x } group g2 { y } }
fn pick(flag: bool) -> Vec[E] {
    if flag {
        let mut a: Vec[E] = Vec.new();
        a.push(E { x: 1.0, y: 2.0 });
        return a;
    } else {
        let mut b: Vec[E] = Vec.new();
        b.push(E { x: 3.0, y: 4.0 });
        return b;
    }
}
fn main() with panics {
    let g1: Vec[E] = pick(true);
    let g2: Vec[E] = pick(false);
    println(g1[0].x + g1[0].y);
    println(g2[0].x + g2[0].y);
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(
            lines,
            vec!["3", "7"],
            "both explicit-`return` arms of an if/else must lower SoA"
        );
    }
}

#[test]
fn test_e2e_soa_branch_leaf_bare_tails() {
    // Follow-on: branch-leaf BARE tails (`if flag { a } else { b }`, no
    // `return` keyword) — the function value is the `If`, whose then/else
    // tail leaves are the returned locals. The recursive collector reaches
    // them through the branch blocks; before, only a directly-bare body
    // `final_expr` qualified, so neither leaf seeded SoA. Same expectations
    // as the explicit-return form: pick(true) -> 3, pick(false) -> 7.
    let out = run_program(
        r#"
struct E { x: f64, y: f64 }
layout grid: Vec[E] { group g1 { x } group g2 { y } }
fn pick(flag: bool) -> Vec[E] {
    if flag {
        let mut a: Vec[E] = Vec.new();
        a.push(E { x: 1.0, y: 2.0 });
        a
    } else {
        let mut b: Vec[E] = Vec.new();
        b.push(E { x: 3.0, y: 4.0 });
        b
    }
}
fn main() with panics {
    let g1: Vec[E] = pick(true);
    let g2: Vec[E] = pick(false);
    println(g1[0].x + g1[0].y);
    println(g2[0].x + g2[0].y);
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(
            lines,
            vec!["3", "7"],
            "branch-leaf bare-tail SoA returns must lower SoA at every leaf"
        );
    }
}

// ── Struct equality ───────────────────────────────────────────

#[test]
fn test_e2e_struct_equality() {
    let out = run_program(
        r#"
#[derive(Eq)]
struct Point { x: i64, y: i64 }
fn main() {
    let a = Point { x: 1, y: 2 };
    let b = Point { x: 1, y: 2 };
    let c = Point { x: 3, y: 4 };
    if a == b { println(1); } else { println(0); }
    if a == c { println(1); } else { println(0); }
    if a != c { println(1); } else { println(0); }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["1", "0", "1"]);
    }
}

#[test]
fn test_e2e_struct_equality_mixed_types() {
    let out = run_program(
        r#"
#[derive(Eq)]
struct Pair { name: String, value: i64 }
fn main() {
    let a = Pair { name: "hello", value: 42 };
    let b = Pair { name: "hello", value: 42 };
    let c = Pair { name: "world", value: 42 };
    if a == b { println(1); } else { println(0); }
    if a == c { println(1); } else { println(0); }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["1", "0"]);
    }
}

#[test]
fn test_e2e_derive_default_struct() {
    // End-to-end: derived `Config.default()` zero-fills primitive
    // fields (book appendix C example).
    let out = run_program(
        r#"
#[derive(Default)]
struct Config { timeout_ms: i64, retries: i64, verbose: bool }
fn main() {
    let c = Config.default();
    println(c.timeout_ms);
    println(c.verbose);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "0\nfalse");
    }
}

#[test]
fn test_e2e_tuple_projection() {
    // B-2026-06-11-7: a chained tuple index `t.1.1` failed to PARSE (the
    // lexer ate `1.1` as a float). B-2026-06-11-6: a struct field read
    // THROUGH a tuple element `t.1.name` compiled to the `i64 0` placeholder
    // under AOT (interp was correct) because `type_name_of_expr` couldn't
    // resolve the tuple element's struct type. Both fixed: the lexer treats
    // a `.`-preceded number as a tuple index, and a tuple binding's element
    // type names are recorded so the field access resolves. Covers nested
    // tuple index, struct-through-tuple (multi-field), and an annotated tuple.
    let out = run_program(
        r#"
struct Inner { name: String, age: i64 }
fn main() {
    let nt = (1i64, (2i64, 3i64));
    println(nt.1.0);
    println(nt.1.1);
    let t = (9i64, Inner { name: f"in-{2}", age: 7i64 });
    println(t.1.name);
    println(t.1.age);
    let a: (i64, Inner) = (5i64, Inner { name: f"ann-{4}", age: 8i64 });
    println(a.1.name);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "2\n3\nin-2\n7\nann-4\n");
    }
}

#[test]
fn test_ir_a_gpu_buffer_struct_field_is_freed_when_the_struct_dies() {
    // GPU-SLIP-4b-4. `GpuBuffer[S]` is a nameable type, so a buffer can
    // live in a struct field — and a field is reclaimed by the struct's
    // drop, not by any `let`. Without the `synth_drop` classifier arm
    // nothing frees it: the storage goes away and the DEVICE allocation
    // stays, which LeakSanitizer cannot see (it is not in the host heap)
    // and which surfaces only as a GPU eventually refusing to allocate.
    //
    // The program deliberately binds NO buffer to a local, so every
    // `karac_runtime_gpu_free_soa` in the IR must have come from the
    // struct drop. Zero occurrences is the pre-fix state.
    let src = r#"
struct Body { mass: f32, speed: f32 }
struct Sim  { grid: GpuBuffer[Body], step: i32 }

fn main() {
    let bodies: Vec[Body] = [Body { mass: 1.0, speed: 2.0 }];
    let sim = Sim { grid: gpu.upload(bodies), step: 0 };
    println(sim.step);
}
"#;
    let ir = ir_for_with_ownership(src);
    assert!(
        ir.contains("karac_runtime_gpu_free_soa"),
        "a GpuBuffer struct field must be freed by the struct's drop; got no \
             free at all:\n{ir}"
    );
    assert!(
        ir.contains("__karac_drop_struct_Sim"),
        "the free must live in the struct's drop fn, not at a binding:\n{ir}"
    );
}

#[test]
fn test_ir_a_declared_gpu_buffer_field_lowers_to_the_two_word_handle() {
    // The declared type must lower to the same `{ i64 handle, i64 n }` the
    // upload produces. Before the lowering arm existed, `GpuBuffer[S]` hit
    // the unknown-name `i64` default and `Sim` lowered to `{ i64, i32 }` —
    // half a handle, and the reduction then read an integer where it
    // expected the aggregate.
    let src = r#"
struct Body { mass: f32, speed: f32 }
struct Sim  { grid: GpuBuffer[Body], step: i32 }

fn main() {
    let bodies: Vec[Body] = [Body { mass: 1.0, speed: 2.0 }];
    let sim = Sim { grid: gpu.upload(bodies), step: 0 };
    println(f"{gpu.sum(sim.grid.mass)}");
}
"#;
    let ir = ir_for_with_ownership(src);
    assert!(
        ir.contains("{ { i64, i64 }, i32 }"),
        "`Sim` must lower with the two-word buffer handle inline; got:\n{ir}"
    );
}

#[test]
fn test_ir_gpu_upload_unlayouted_defaults_to_single_interleaved_group() {
    // GPU-SLIP-4h: `gpu.upload` on a plain `Vec[S]` (no `layout` block for
    // `S` anywhere) synthesizes ONE interleaved device group — the WGSL
    // binds a single `array<G_aos>` in/out pair (hand-wgpu-equal access,
    // verbatim upload/download copies) instead of one buffer per field.
    // Codegen-only: no GPU is touched, so this is CI-safe.
    let src = r#"
struct P2 { a: f32, b: f32 }

#[gpu]
fn stepp(p: P2) -> P2 {
    P2 { a: p.a + p.b, b: p.b }
}

fn main() {
    let mut v: Vec[P2] = Vec.new();
    v.push(P2 { a: 1.0, b: 2.0 });
    let buf = gpu.upload(v);
    let buf2 = gpu.dispatch(stepp, buf);
    let back = gpu.download(buf2);
    println(back.len());
}
"#;
    let ir = ir_for_with_ownership(src);
    assert!(
        ir.contains("aos_in: array<G_aos>") && ir.contains("aos_out: array<G_aos>"),
        "expected the default single interleaved group (aos_in/aos_out over array<G_aos>) \
             in the embedded WGSL; got:\n{ir}"
    );
    assert!(
        !ir.contains("a_in: array<f32>"),
        "per-field buffers must not be emitted for the un-layouted default; got:\n{ir}"
    );
    assert!(
        ir.contains("karac_runtime_gpu_upload_soa"),
        "upload must route through the resident upload entry; got:\n{ir}"
    );
    assert!(
        ir.contains("karac_runtime_gpu_download_soa"),
        "download must route through the resident download entry; got:\n{ir}"
    );
}

#[test]
fn test_ir_struct_destructure_rest_field_freed() {
    // A heap field dropped by a `..` rest is also discard-freed.
    let src = format!(
            "{STRUCT_DESTRUCTURE_PRELUDE}fn main() {{\n    let Point {{ count, .. }} = make();\n    println(count);\n}}\n"
        );
    let ir = ir_for_with_ownership(&src);
    assert!(
        ir.contains("__destructure_discard_") && main_free_count(&ir) >= 1,
        "field dropped by `..` rest must be freed; got:\n{ir}"
    );
}

#[test]
fn test_e2e_headerless_member_primitive_field_write() {
    // Primitive field writes on cluster members are an allowed
    // shape (`cluster_verify_assign` falls through to the generic
    // prim-write scan) and compile through the `sh_{field}_ptr`
    // store site — which must GEP the headerless twin when phase D
    // engages. A missed conversion writes 8 bytes high (into the
    // link slot) and the walk would chase a corrupted pointer.
    let out = run_program_with_ownership(
        r#"
shared struct ListNode { mut val: i64, mut next: Option[ListNode] }
fn build_and_sum(n: i64) -> i64 {
    let dummy = ListNode { val: 0, next: None };
    let mut tail = dummy;
    let mut i = 1;
    while i <= n {
        let node = ListNode { val: i, next: None };
        tail.next = Some(node);
        tail = node;
        i = i + 1;
    }
    dummy.val = 1000;
    tail.val = tail.val + 7;
    let mut sum = dummy.val;
    let mut cur = dummy.next;
    while cur.is_some() {
        let x = cur.unwrap();
        sum = sum + x.val;
        cur = x.next;
    }
    sum
}
fn main() { println(build_and_sum(5)); }
"#,
    );
    // 1000 (dummy.val) + 1+2+3+4 + (5+7) = 1022.
    assert_eq!(out.as_deref(), Some("1022\n"));
}

#[test]
fn test_ir_headerless_abi_program_wide() {
    // Phase C2b: the full kata-#2 pipeline under the program-wide
    // gate — every ListNode in the program is headerless and every
    // count op is gone. Builders allocate without the rc word; the
    // borrowing adder emits ZERO count ops INCLUDING the param
    // exit decs (skipped two-sidedly with the caller\'s arg incs);
    // the caller adopts all three call results (l1/l2 via the
    // sanctioned-arg channel) and drops them by option-guarded
    // free-walk.
    let ir = ir_for_with_ownership(
        r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
fn from_three(a: i64, b: i64, c: i64) -> Option[ListNode] {
    let dummy = ListNode { val: 0, next: None };
    let mut tail = dummy;
    let mut i = 0;
    while i < 3 {
        let mut v = a;
        if i == 1 { v = b; }
        if i == 2 { v = c; }
        let node = ListNode { val: v, next: None };
        tail.next = Some(node);
        tail = node;
        i = i + 1;
    }
    dummy.next
}
fn add_two_numbers(l1: Option[ListNode], l2: Option[ListNode]) -> Option[ListNode] {
    let dummy = ListNode { val: 0, next: None };
    let mut tail = dummy;
    let mut a = l1;
    let mut b = l2;
    let mut carry: i64 = 0;
    loop {
        let mut s: i64 = carry;
        let mut done = true;
        if let Some(n) = a {
            s = s + n.val;
            a = n.next;
            done = false;
        }
        if let Some(n) = b {
            s = s + n.val;
            b = n.next;
            done = false;
        }
        if done and s == 0 {
            break;
        }
        let node = ListNode { val: s % 10, next: None };
        tail.next = Some(node);
        tail = node;
        carry = s / 10;
    }
    dummy.next
}
fn main() {
    let l1 = from_three(2, 4, 3);
    let l2 = from_three(5, 6, 4);
    let out = add_two_numbers(l1, l2);
    match out {
        Some(node) => { println(node.val); }
        None => {}
    }
}
"#,
    );
    let builder = function_body(&ir, "from_three").expect("builder body");
    assert!(
        builder.contains("hl_alloc") && !builder.contains("rc_alloc"),
        "builder allocates headerless; body:\n{builder}"
    );
    let adder = function_body(&ir, "add_two_numbers").expect("adder body");
    assert!(
        adder.contains("hl_alloc") && !adder.contains("rc_alloc"),
        "adder\'s own cluster headerless; body:\n{adder}"
    );
    assert!(
        !adder.contains("rc_inc") && !adder.contains("rc_dec") && !adder.contains("opt_rc_cleanup"),
        "adder fully count-free incl. param exit decs; body:\n{adder}"
    );
    let main_body = function_body(&ir, "main").expect("main body");
    assert!(
        !main_body.contains("opt.arg.inc"),
        "borrowed-position args take no inc; body:\n{main_body}"
    );
    assert!(
        main_body.contains("acw_loop"),
        "adopted results free-walk; body:\n{main_body}"
    );
    assert!(
        !main_body.contains("rc_inc") && !main_body.contains("opt_rc_cleanup"),
        "caller count-free; body:\n{main_body}"
    );
}

#[test]
fn test_e2e_headerless_abi_full_pipeline() {
    // C2b end-to-end: the whole composition (headerless builders →
    // borrowed walks → sanctioned-arg adopted holders → free-walk
    // drops), repeated with chain reuse. A layout disagreement
    // anywhere mis-GEPs val/next by 8 bytes (wrong sum); a count
    // op on a headerless node corrupts a field (wrong sum / UAF).
    let out = run_program_with_ownership(
        r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
fn from_three(a: i64, b: i64, c: i64) -> Option[ListNode] {
    let dummy = ListNode { val: 0, next: None };
    let mut tail = dummy;
    let mut i = 0;
    while i < 3 {
        let mut v = a;
        if i == 1 { v = b; }
        if i == 2 { v = c; }
        let node = ListNode { val: v, next: None };
        tail.next = Some(node);
        tail = node;
        i = i + 1;
    }
    dummy.next
}
fn add_two_numbers(l1: Option[ListNode], l2: Option[ListNode]) -> Option[ListNode] {
    let dummy = ListNode { val: 0, next: None };
    let mut tail = dummy;
    let mut a = l1;
    let mut b = l2;
    let mut carry: i64 = 0;
    loop {
        let mut s: i64 = carry;
        let mut done = true;
        if let Some(n) = a {
            s = s + n.val;
            a = n.next;
            done = false;
        }
        if let Some(n) = b {
            s = s + n.val;
            b = n.next;
            done = false;
        }
        if done and s == 0 {
            break;
        }
        let node = ListNode { val: s % 10, next: None };
        tail.next = Some(node);
        tail = node;
        carry = s / 10;
    }
    dummy.next
}
fn sum_chain(head: Option[ListNode]) -> i64 {
    let mut sum = 0;
    let mut cur = head;
    while cur.is_some() {
        let x = cur.unwrap();
        sum = sum + x.val;
        cur = x.next;
    }
    sum
}
fn main() {
    let l1 = from_three(2, 4, 3);
    let l2 = from_three(5, 6, 4);
    let mut total = 0;
    let mut iter = 0;
    while iter < 100 {
        let r = add_two_numbers(l1, l2);
        total = total + sum_chain(r);
        iter = iter + 1;
    }
    total = total + sum_chain(l1) + sum_chain(l2);
    println(total);
}
"#,
    );
    // 100 * 15 + 9 + 15 = 1524.
    assert_eq!(out.as_deref(), Some("1524\n"));
}

// ── B-2026-07-13-6: lexical scoping — a `let` shadowing an outer binding
// in a nested scope must NOT leak past the scope. `variables` is a flat
// map with no scope stack; `snapshot_var_env`/`restore_var_env` checkpoint
// it at each nested-scope entry. Pre-fix these printed the inner shadow's
// value after the scope (silent build/run divergence; interp scoped right).

#[test]
fn test_e2e_shadow_nested_block_reverts() {
    let out = run_program(
        "fn main() {\n\
             let x = 11;\n\
             {\n\
                 let x = 22;\n\
                 println(x.to_string());\n\
             }\n\
             println(x.to_string());\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "22\n11");
    }
}

#[test]
fn test_e2e_shadow_double_nested_block_reverts() {
    let out = run_program(
        "fn main() {\n\
             let x = 1;\n\
             { let x = 2; { let x = 3; println(x.to_string()); } println(x.to_string()); }\n\
             println(x.to_string());\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "3\n2\n1");
    }
}

#[test]
fn test_with_provider_e2e_nested_same_resource_innermost_wins() {
    // LIFO push/pop semantics. Outer push binds resource R to a
    // provider whose `get()` returns 1; inner push rebinds R to
    // a provider whose `get()` returns 2. Inside the inner scope,
    // R.get() must walk to the inner frame (head) and return 2;
    // after the inner scope pops, R.get() in the outer scope must
    // return 1 again. This test pins the runtime stack walk
    // (`karac_provider_lookup` returning the *first* matching frame
    // at innermost-first order) end to end through codegen.
    let src = "pub trait Reader { fn get(ref self) -> i64; }\n\
            pub struct Data { x: i64 }\n\
            impl Reader for Data { fn get(ref self) -> i64 { self.x } }\n\
            pub effect resource D: Reader;\n\
            fn main() {\n\
              let outer = Data { x: 1 };\n\
              let inner = Data { x: 2 };\n\
              with_provider[D](outer, || {\n\
                println(D.get());\n\
                with_provider[D](inner, || {\n\
                  println(D.get());\n\
                });\n\
                println(D.get());\n\
              });\n\
            }";
    let Some(out) = run_program(src) else {
        eprintln!("skipping nested with_provider e2e: runtime/linker unavailable");
        return;
    };
    let lines: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        lines.len(),
        3,
        "expected 3 println outputs (outer, inner, outer-restored); got: {:?}",
        lines
    );
    assert_eq!(lines[0].trim(), "1", "outer scope: D.get should return 1");
    assert_eq!(
        lines[1].trim(),
        "2",
        "inner scope: D.get should return 2 (innermost wins)"
    );
    assert_eq!(
        lines[2].trim(),
        "1",
        "after inner pop: D.get should return 1 again (outer restored)"
    );
}

// ── Codegen bug regression tests ─────────────────────────────────
//
// Each test below pins an entry surfaced through
// `docs/implementation_checklist/bugs.md`. Tests gated `#[ignore]`
// pin a still-open bug (running with `--include-ignored` or
// `cargo test --features llvm -- --ignored <name>` exercises the
// failing path); ungated tests are the regression gate after the
// underlying fix has landed.

/// Regression gate for the previously-latent "Provider struct
/// identity collision in codegen's `var_type_names`" bug (bugs.md
/// entry). Two distinct user types that lower to the same LLVM
/// struct shape used to collide in the LLVM-struct-identity reverse
/// lookup at `let p = Provider.new()` (the UFCS-associated-fn
/// fallback path in `compile_let`). The `var_type_names` mapping
/// would pick an arbitrary match in HashMap iteration order, so
/// `with_provider[R](p, || R.method())` routed to whichever
/// provider's vtable iteration produced first.
///
/// Fix: in the fallback path of `compile_let`, prefer the source-AST
/// identity for UFCS calls of the shape `Target.fn(...)` whose LLVM
/// return type matches `Target`'s LLVM struct identity. The bare
/// LLVM-identity reverse-lookup remains as a final fallback for any
/// other call shape that yields a struct value.
///
/// Repro: two providers `ProvA` / `ProvB` with identical `{ i64 }`
/// LLVM shape, each with a `pub fn new()` associated-fn constructor.
/// `with_provider[Ra](ProvA.new(), …)` and `with_provider[Rb](
/// ProvB.new(), …)` must each dispatch to its own impl — pre-fix,
/// both `Ra.record(0)` and `Rb.record(0)` routed to the same impl
/// (e.g., "100\n100" instead of "100\n200").
#[test]
fn test_var_type_names_struct_identity_collision_repro() {
    let out = run_program(
        r#"
pub trait Recorder { fn record(ref self, value: i64) -> i64; }

pub struct ProvA { x: i64 }
impl ProvA { pub fn new() -> ProvA { ProvA { x: 1 } } }
impl Recorder for ProvA { fn record(ref self, value: i64) -> i64 { 100 } }

pub struct ProvB { x: i64 }
impl ProvB { pub fn new() -> ProvB { ProvB { x: 2 } } }
impl Recorder for ProvB { fn record(ref self, value: i64) -> i64 { 200 } }

pub effect resource Ra: Recorder;
pub effect resource Rb: Recorder;

fn main() {
    let a = ProvA.new();
    let b = ProvB.new();
    with_provider[Ra](a, || {
        with_provider[Rb](b, || {
            println(Ra.record(0));
            println(Rb.record(0));
        });
    });
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(
            lines,
            vec!["100", "200"],
            "expected each `with_provider[R]` to route to its own impl \
                 (ProvA.record => 100, ProvB.record => 200); got {:?}. \
                 If both lines match (e.g., both `200`), the LLVM-struct- \
                 identity reverse-lookup at compile_let collided ProvA's \
                 binding onto ProvB's name (or vice versa).",
            lines
        );
    }
}

/// Regression gate for the previously-latent "chained-field
/// `println` returns 0" bug (bugs.md entry). `println(o.inner.name)`
/// where `Outer { inner: Inner }` and `Inner { name: String }` used
/// to emit a load that resolved to 0 at runtime regardless of the
/// field value. Single-level access (`o.field`) worked; the gap was
/// at chain-depth ≥ 2 because `field_index_for` only resolved an
/// `Identifier` / `SelfValue` object — a `FieldAccess` object
/// (`o.inner` in `o.inner.name`) returned `None`, falling through
/// to the constant-zero fallback in `compile_field_access`.
///
/// Fix: track per-field user-type names in
/// `struct_field_type_names` at struct-declaration time, and walk
/// `FieldAccess` chains in a new `type_name_of_expr` helper used by
/// `field_index_for`. The helper returns the inner struct's name
/// for `o.inner` so `name` resolves in `Inner`'s field registry.
#[test]
fn test_chained_field_access_returns_zero_repro() {
    let out = run_program(
        r#"
struct Inner { name: String }
struct Outer { inner: Inner }
fn main() {
    let o = Outer { inner: Inner { name: "alice" } };
    println(o.inner.name);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out.trim(),
            "alice",
            "expected `o.inner.name` to load the actual String value; \
                 got {:?}. If the output is `0`, the chain-depth ≥ 2 load \
                 path zeroed the value regardless of the field contents.",
            out.trim()
        );
    }
}

/// Outer `lbl: { inner: { break lbl 7; 0 } }` evaluates to 7. The
/// inner labeled block's tail (`0`) never runs because `break lbl`
/// transfers control past the inner exit straight to the outer exit.
/// Stresses the label-aware frame walk (LBC1) — the resolver
/// guarantees `lbl` resolves to the outer block, and codegen's
/// `compile_break` rev-walk picks the matching frame, not the
/// innermost.
#[test]
fn test_labeled_block_break_from_nested_block_e2e() {
    let out = run_program(
        r#"
fn main() {
    let x: i64 = lbl: { inner: { break lbl 7; 0 } };
    println(x);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "7");
    }
}

/// **Latent-bug regression gate.** `outer: while ... { inner: while
/// ... { break outer; } }` exits the outer loop, not just the inner.
/// Pre-slice codegen always picked `loop_stack.last()` regardless of
/// label, so `break outer` would have broken only `inner` and the
/// outer-loop termination would never have observed the inner break.
/// Today's label-aware lookup (LBC1 side-effect) closes the gap; the
/// post-loop println marker must print `done` in one shot.
#[test]
fn test_labeled_loop_nested_break_outer_e2e() {
    let out = run_program(
        r#"
fn main() {
    let mut count = 0;
    outer: while true {
        inner: while true {
            count = count + 1;
            break outer ();
        }
        // Without the latent-bug fix, the outer-loop body would
        // re-enter `inner` here every iteration. With the fix, the
        // `break outer` transfers control past this point straight to
        // the outer loop's exit BB.
        count = count + 100;
    }
    println(count);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "1");
    }
}

#[test]
fn test_state_struct_type_emitted_for_method_with_self() {
    // A method whose body calls a network-effect callee gets a
    // `%kara.state.Hub.run` LLVM struct type. `self` carries the
    // impl block's target type — for a `shared struct Hub`, that's
    // a pointer-sized handle, so the state struct = `{ i32, ptr }`.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             shared struct Hub { count: i64 }
             impl Hub {
                 fn run(self) { fetch(); }
             }",
    );
    assert!(
        ir.contains("%kara.state.Hub.run"),
        "expected state struct %kara.state.Hub.run for impl method:\n{ir}"
    );
    let line = ir
        .lines()
        .find(|l| l.starts_with("%kara.state.Hub.run = type {"))
        .unwrap_or_else(|| panic!("no Hub.run state struct type def:\n{ir}"));
    // Shared struct handles are pointers.
    assert!(
        line.contains("ptr"),
        "expected self to lower to a pointer-sized handle for shared struct: {line}"
    );
}

#[test]
fn test_state_struct_type_not_emitted_for_pure_function() {
    // A pure function that calls no network-effect callee gets no
    // entry in `state_struct_layouts` (slice-4 presence rule) and
    // therefore no `%kara.state.*` type in the IR.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn pure_helper(x: i64) -> i64 { x + 1 }",
    );
    assert!(
        !ir.contains("%kara.state.pure_helper"),
        "pure function must not emit a state struct type:\n{ir}"
    );
}

#[test]
fn test_state_constructor_body_calls_malloc() {
    // Constructor body must call malloc to allocate the state
    // struct on the heap. The exact malloc call shape includes
    // the size operand and the result-name; pin the substring.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver() { fetch(); }",
    );
    let body = extract_fn_ir(&ir, "__kara_state_new_driver");
    assert!(
        body.contains("call ptr @malloc"),
        "constructor must call malloc:\n{body}"
    );
    // The result is bound to %state.alloc for downstream use.
    assert!(
        body.contains("%state.alloc"),
        "constructor must bind malloc result to %state.alloc:\n{body}"
    );
}

#[test]
fn test_state_constructor_initializes_tag_to_zero() {
    // Constructor must store 0 into state struct field 0 (the
    // i32 yield-point tag) before returning the pointer. This
    // ensures the next poll-fn invocation routes to the entry
    // arm `state_0` via slice 7's switch dispatch.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver() { fetch(); }",
    );
    let body = extract_fn_ir(&ir, "__kara_state_new_driver");
    assert!(
        body.contains("store i32 0, ptr %tag_init_ptr"),
        "constructor must initialize tag = 0:\n{body}"
    );
    // The GEP for the tag-init pointer references the state struct
    // type, keeping the named type referenced from the constructor.
    assert!(
        body.contains("getelementptr inbounds %kara.state.driver"),
        "constructor must GEP into the state struct's typed field 0:\n{body}"
    );
}

#[test]
fn test_state_constructor_not_emitted_for_pure_function() {
    // Pure functions (no network-effect calls) have no state-struct
    // entry and therefore no constructor.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn pure_helper(x: i64) -> i64 { x + 1 }",
    );
    assert!(
        !ir.contains("@__kara_state_new_pure_helper"),
        "pure function must not emit a state constructor:\n{ir}"
    );
}

// ── Phase 6 line 26 slice 8d: caller-side network-boundary intercept ─
//
// When the caller (a non-network-boundary function) calls a network-
// boundary function, codegen replaces the direct `call @<name>(args)`
// with the state-machine invocation shape: constructor → poll loop →
// free. The synchronous spin-loop is a v1 placeholder; slice 8e+
// replaces the busy-loop with a yield to the line-17 scheduler.

#[test]
fn test_caller_side_intercept_calls_state_constructor() {
    // A non-network-boundary `main` calling a network-boundary
    // `driver` should NOT emit a direct `call void @driver()` —
    // instead it must call the state-struct constructor.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver() { fetch(); }
             fn main() { driver(); }",
    );
    // The intercept replaces the direct call. Find `main`'s body
    // and check.
    let main_body = extract_fn_ir(&ir, "main");
    assert!(
        main_body.contains("call ptr @__kara_state_new_driver()"),
        "main must call state constructor instead of direct driver():\n{main_body}"
    );
    // The direct `call void @driver()` should NOT appear in main.
    assert!(
        !main_body.contains("call void @driver()"),
        "main must NOT direct-call @driver after the intercept:\n{main_body}"
    );
}

#[test]
fn test_caller_arg_storing_no_args_no_field_writes() {
    // A no-arg function has no arg-store sites — main's body must
    // not contain any kara.argN.field_ptr GEPs.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver() { fetch(); }
             fn main() { driver(); }",
    );
    let main_body = extract_fn_ir(&ir, "main");
    assert!(
        !main_body.contains("kara.arg0.field_ptr"),
        "no-arg call must not emit arg-store sites:\n{main_body}"
    );
}

#[test]
fn test_method_call_intercept_stores_receiver_into_field_1() {
    // The receiver (`obj` in `obj.run()`) becomes `self` and stores
    // into state struct field 1 — `self` is at layout position 0
    // per slice 4, so field 1 in the struct (after the i32 tag).
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             struct Hub { count: i64 }
             impl Hub {
                 fn run(self) { fetch(); }
             }
             fn main() {
                 let h = Hub { count: 0 };
                 h.run();
             }",
    );
    let main_body = extract_fn_ir(&ir, "main");
    assert!(
        main_body
            .contains("getelementptr inbounds %kara.state.Hub.run, ptr %kara.state, i32 0, i32 1"),
        "method intercept must GEP into state struct field 1 for self:\n{main_body}"
    );
    // The receiver SSA value is stored into field 1.
    assert!(
        main_body.contains("store") && main_body.contains(", ptr %kara.self.field_ptr"),
        "method intercept must store the receiver into the self.field_ptr:\n{main_body}"
    );
}

#[test]
fn test_return_value_caller_side_loads_terminal_field() {
    // Caller-side intercept loads the terminal field after the
    // done block (and before `@free`). The load result is the
    // call's return value.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver() -> i64 with sends(Network) receives(Network) { fetch(); 0 }
             fn app_main() -> i64 { driver() }",
    );
    let main_body = extract_fn_ir(&ir, "app_main");
    // Load from the terminal field with named GEP.
    assert!(
        main_body.contains("%kara.return.field_ptr"),
        "caller must GEP into terminal field:\n{main_body}"
    );
    assert!(
        main_body.contains("load i64, ptr %kara.return.field_ptr"),
        "caller must load i64 from terminal field:\n{main_body}"
    );
    // The load must happen BEFORE the @free call — once freed, the
    // pointer is no longer dereferenceable.
    let load_pos = main_body
        .find("load i64, ptr %kara.return.field_ptr")
        .unwrap();
    let free_pos = main_body.find("call void @free").unwrap();
    assert!(
        load_pos < free_pos,
        "caller must load terminal field before @free:\n{main_body}"
    );
}

#[test]
fn test_8ai_bool_return_state_struct_terminal_field_is_i1() {
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver() -> bool with sends(Network) receives(Network) { fetch(); true }",
    );
    let line = ir
        .lines()
        .find(|l| l.starts_with("%kara.state.driver = type {"))
        .unwrap_or_else(|| panic!("no driver state struct in IR:\n{ir}"));
    assert!(
        line.contains("i32, i1"),
        "bool return: state struct must be {{ i32 tag, i1 terminal }}:\n{line}"
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    assert!(
        body.contains("store i1 false, ptr %kara.return.field_ptr"),
        "bool return: terminal arm must store typed-zero i1 placeholder:\n{body}"
    );
}

#[test]
fn test_8ai_user_struct_return_state_struct_terminal_field_is_struct() {
    // Concrete (non-shared) user struct: `Hub { count: i64 }`
    // registers as an anonymous LLVM struct type `{ i64 }`
    // (codegen uses `context.struct_type(...)`, not
    // `opaque_struct_type`, so structs are referenced
    // structurally rather than by name). The terminal field
    // embeds the struct inline; the placeholder is a
    // `zeroinitializer` of that anonymous shape.
    let ir = ir_for_with_state_struct_layouts(
            "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             struct Hub { count: i64 }
             fn driver() -> Hub with sends(Network) receives(Network) { fetch(); Hub { count: 0 } }",
        );
    let line = ir
        .lines()
        .find(|l| l.starts_with("%kara.state.driver = type {"))
        .unwrap_or_else(|| panic!("no driver state struct in IR:\n{ir}"));
    assert!(
        line.contains("i32, { i64 }"),
        "Hub return: state struct must include Hub's structural shape as terminal field:\n{line}"
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    assert!(
        body.contains("store { i64 } zeroinitializer, ptr %kara.return.field_ptr"),
        "Hub return: terminal arm must store zeroinitializer placeholder:\n{body}"
    );
}

#[test]
fn test_body_splitting_8q_nested_binary() {
    // `a = (a + b) * 2;` — a binary whose lhs is itself a binary.
    // The materialiser recurses; LLVM auto-suffixes the inner
    // `binop.assign_rhs` name to keep the two int-arith results
    // distinct in the IR.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver(a: i64, b: i64) with sends(Network) receives(Network) {
                 a = (a + b) * 2;
                 fetch();
             }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    // Inner add: a + b. LLVM auto-suffixes the inner name → the
    // first `binop.assign_rhs` instance gets the bare name and the
    // outer gets `binop.assign_rhs1` (or vice versa depending on
    // emission order — both are valid). Assert by counting.
    let add_count = body.matches("add i64 %a.assign_rhs, %b.assign_rhs").count();
    assert_eq!(
        add_count, 1,
        "nested binary inner-add `a + b` must appear exactly once:\n{body}"
    );
    let mul_count = body.matches("mul i64 %binop.assign_rhs").count();
    assert_eq!(
        mul_count, 1,
        "nested binary outer-mul over the inner add result must appear once:\n{body}"
    );
}

#[test]
fn test_body_splitting_8t_nested_cf_descent_finds_inner_yield() {
    // `if a { if b { fetch(); } }` — descent recurses through nested
    // CF to find the inner yield. Equivalent to one yield-point;
    // post-CF call lands in state_1.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn take(n: i64) {}
             fn driver(a: bool, b: bool) with sends(Network) receives(Network) {
                 if a { if b { fetch(); } }
                 take(5);
             }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    let transition_pos = body
        .find("store i32 1, ptr %state_0.next_tag_ptr")
        .expect("state_0 transition should be present");
    let take_pos = body
        .find("call void @take(i64 5)")
        .expect("take call should be present");
    assert!(
        take_pos > transition_pos,
        "take(5) must land in state_1 (after nested-CF yield):\n{body}"
    );
}

#[test]
fn test_state_destructor_not_emitted_when_all_fields_primitive() {
    // A primitive-only captured-local set (`i64`-typed param) has
    // no heap-bearing field. Slice 8u skips destructor emission
    // entirely — the destructor would have an `entry: ret void`
    // body that's effectively dead code, and the absence is the
    // fast "no cleanup to do" signal for the future use sites.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver(n: i64) { fetch(); }",
    );
    assert!(
        !ir.contains("@__kara_state_drop_driver"),
        "primitive-only captures must not emit a destructor:\n{ir}"
    );
}

#[test]
fn test_state_destructor_not_emitted_for_pure_function() {
    // Pure functions (no network-effect calls) have no state-struct
    // entry and therefore no destructor.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn pure_helper(x: i64) -> i64 { x + 1 }",
    );
    assert!(
        !ir.contains("@__kara_state_drop_pure_helper"),
        "pure function must not emit a destructor:\n{ir}"
    );
}

#[test]
fn test_state_destructor_returns_void() {
    // Destructor's terminator is `ret void` — caller pairs the call
    // with the state-struct's own `free`, so the destructor itself
    // doesn't free the state pointer (matches the constructor's
    // caller-allocates / caller-frees discipline).
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver(items: Vec[i64]) { fetch(); }",
    );
    let body = extract_fn_ir(&ir, "__kara_state_drop_driver");
    assert!(
        body.contains("ret void"),
        "destructor must end with ret void:\n{body}"
    );
    // Sanity: the destructor must NOT free the state ptr itself —
    // that's the caller's job. A `call void @free(ptr %state)` shape
    // would indicate the destructor doubled-up the free.
    assert!(
        !body.contains("call void @free(ptr %state)"),
        "destructor must not free the state ptr (caller does):\n{body}"
    );
}

// ── Phase 6 line 26 slice 8v Phase 1: skip generics from base-name emission ──
//
// Polymorphic yielding functions (`fn driver[T](item: T) { fetch(); }`)
// get an entry in `state_struct_layouts` under their base name with
// a captured-local field whose `type_name` references the
// unsubstituted type parameter (`Some("T")`). Slices 5 / 6 / 8c / 8u
// run before any per-mono `type_subst` is active, so
// `llvm_type_for_name("T")` would fall through to the i64 default —
// producing dead-and-broken state-struct LLVM types / poll-fns /
// constructors / destructors that the caller-side intercept can't
// reach (the `compile_call` dispatch short-circuits through
// `compile_generic_call` for generics, bypassing the slice-8d
// intercept). Slice 8v Phase 1 detects generic entries via
// `is_generic_fn_key` and skips them entirely from all four
// base-name passes — removing the dead IR without affecting any
// non-generic state-machine emission. Per-mono emission + caller-
// side intercept routing land in slice 8v Phase 2.

#[test]
fn test_state_machine_skips_polymorphic_state_struct_type() {
    // Polymorphic yielding fn produces no `%kara.state.<base>`
    // LLVM struct type — slice 5's iteration over
    // `state_struct_layouts` now filters via `is_generic_fn_key`.
    // Per-mono emission DOES land under the mangled key (e.g.
    // `%"kara.state.driver$i64"`) once slice 8v Phase 2's
    // `compile_generic_call` orchestrator runs; the assertion
    // matches the full base-name shape `%kara.state.driver = type`
    // exactly, so per-mono variants (`%"kara.state.driver$..."`)
    // don't trip the filter.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver[T](item: T) { fetch(); }
             fn caller() { driver(42i64); }",
    );
    assert!(
        !ir.contains("%kara.state.driver = type"),
        "polymorphic driver must not emit a base-name state struct type:\n{ir}"
    );
    // Anchor global mirrors the state-struct type — base-name
    // must not appear. Per-mono anchors (e.g.
    // `@"__kara_state_type_anchor_driver$i64"`) are expected and
    // get a separate test in the Phase 2 section below.
    assert!(
        !ir.contains("@__kara_state_type_anchor_driver = "),
        "polymorphic driver must not emit a base-name state-struct anchor global:\n{ir}"
    );
}

#[test]
fn test_state_machine_skips_polymorphic_state_constructor() {
    // Polymorphic yielding fn produces no `__kara_state_new_<base>`
    // constructor — slice 8c's iteration now filters generics.
    // Per-mono `@"__kara_state_new_driver$i64"()` is expected and
    // tested separately in Phase 2's section.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver[T](item: T) { fetch(); }
             fn caller() { driver(42i64); }",
    );
    assert!(
        !ir.contains("@__kara_state_new_driver("),
        "polymorphic driver must not emit a base-name state constructor:\n{ir}"
    );
}

#[test]
fn test_state_machine_skips_polymorphic_state_destructor() {
    // Polymorphic yielding fn whose captured-local would be
    // heap-bearing on every monomorphization (here `items: Vec[T]`,
    // type_name `Some("Vec")` — classifies as `FieldDrop::VecOrString`)
    // still emits no base-name destructor — slice 8v Phase 1 skips
    // the polymorphic key before the classification runs. Per-mono
    // destructor emission lands the right shape at the mangled key
    // in slice 8v Phase 2. Function need only be declared; the
    // Phase 1 skip fires during the base-name iteration over
    // `state_struct_layouts`, independent of any call sites.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver[T](items: Vec[T]) { fetch(); }",
    );
    assert!(
        !ir.contains("@__kara_state_drop_driver("),
        "polymorphic driver must not emit a base-name state destructor:\n{ir}"
    );
}

#[test]
fn test_ir_f64_parse_calls_runtime_extern() {
    let ir = ir_for(
        "fn p(s: String) -> f64 {\n\
                 match f64.parse(s) {\n\
                     Some(x) => x,\n\
                     None => -1.0,\n\
                 }\n\
             }",
    );
    assert!(
        ir.contains("call i8 @karac_runtime_parse_f64"),
        "f64.parse should call the runtime extern:\n{ir}"
    );
}

#[test]
fn test_e2e_modbind_struct_field_read_annotated() {
    // Uppercase-receiver field access — `CFG.max` / nested
    // `OUTER.inner.field` on a value binding. The parser consumes the
    // uppercase-led dotted chain greedily into a `Path`, so the read
    // lands in `resolve_path_type`, which now walks the binding's struct
    // fields. Codegen's `compile_path_expr` always lowered this shape
    // correctly, but `karac build` exited on a typecheck error
    // (`expected 'i64', found 'Config'`) before the typechecker walk
    // landed. Assert the typecheck gate is CLEAN — the slice-10
    // `run_program` harness ignores typecheck errors, so the runtime
    // output alone would pass vacuously. phase-8-stdlib-floor.md
    // "Uppercase-receiver field access" entry.
    let src = "struct Inner { field: i64 }\n\
                   struct Config { max: i64, count: i64 }\n\
                   struct Outer { inner: Inner }\n\
                   let CFG: Config = Config { max: 64, count: 7 };\n\
                   fn main() {\n\
                       let m: i64 = CFG.max;\n\
                       let c: i64 = CFG.count;\n\
                       let OUTER: Outer = Outer { inner: Inner { field: 9 } };\n\
                       let n: i64 = OUTER.inner.field;\n\
                       println(f\"{m} {c} {n}\");\n\
                   }";
    // The real CLI gate: typecheck must produce no errors.
    let parsed = karac::parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    assert!(
        typed.errors.is_empty(),
        "uppercase-receiver field read must typecheck clean, got: {:?}",
        typed.errors.iter().map(|e| &e.message).collect::<Vec<_>>(),
    );
    // And the binary produces the right values.
    let output = run_program(src).expect("compile + run failed");
    assert_eq!(output, "64 7 9\n");
}

#[test]
fn test_e2e_shadow_struct_a_to_struct_b() {
    // struct-A→struct-B shadow. `var_type_names` / field metadata for the
    // old struct must be replaced so `p.<field>` resolves against B.
    if let Some(out) = run_program(
        "struct A { x: i64 }\n\
             struct B { y: i64 }\n\
             fn main() {\n\
             let p = A { x: 1i64 };\n\
             let p = B { y: 2i64 };\n\
             println(p.y);\n\
             }",
    ) {
        assert_eq!(out, "2\n");
    }
}

#[test]
fn test_e2e_uppercase_local_struct_method() {
    // Uppercase local struct receiver — non-module-binding case
    // exercising the same parser path. The bug was wider than
    // module bindings; any uppercase local also hits it.
    let output = run_program(
        "struct Counter { n: i64 }\n\
             impl Counter {\n\
                 fn doubled(self) -> i64 { self.n * 2 }\n\
             }\n\
             fn main() {\n\
                 let C: Counter = Counter { n: 21 };\n\
                 println(C.doubled());\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "42\n");
}

// ── Slice 10: composite-initializer codegen (design.md §1280-1297) ─
//
// Module-level `let X: T = INIT;` where INIT is a struct literal,
// tuple literal, array literal, repeat literal, or enum
// unit-variant path. Each shape lowers to an LLVM constant
// initializer of the matching aggregate type. Field / index /
// method dispatch inside function bodies sees the binding via the
// reseeded `var_type_names` / `vec_elem_types` side tables — the
// reseed runs at the start of every function body so the binding's
// declared type is visible to the use-site dispatchers without
// being declared inside the function.

#[test]
fn test_e2e_modbind_struct_literal() {
    // Struct literal init → `@DEFAULT_CFG = internal constant`
    // with each field's constant value lowered into its slot.
    // The Path-form access `DEFAULT_CFG.max` routes through the
    // module-binding path-arm to a `load + extractvalue` against
    // the global.
    let output = run_program(
        "struct Config { max: i64, count: i64 }\n\
             let DEFAULT_CFG: Config = Config { max: 64, count: 7 };\n\
             fn main() {\n\
                 println(DEFAULT_CFG.max);\n\
                 println(DEFAULT_CFG.count);\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "64\n7\n");
}

#[test]
fn test_e2e_modbind_struct_literal_field_order_in_source() {
    // Source-order != declaration-order — the codegen path must
    // pivot through `struct_field_names` to land each value in
    // the right slot.
    let output = run_program(
        "struct Pair { a: i64, b: i64 }\n\
             let PR: Pair = Pair { b: 22, a: 11 };\n\
             fn main() { println(PR.a); println(PR.b); }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "11\n22\n");
}

#[test]
fn test_e2e_modbind_struct_with_negative_field() {
    // `Unary { Neg, Integer }` inside a struct field flows through
    // the value-driven recursion. Verifies the recursive surface
    // doesn't lose the sign at the nested position.
    let output = run_program(
        "struct Range { lo: i64, hi: i64 }\n\
             let SPAN: Range = Range { lo: -7, hi: 3 };\n\
             fn main() { println(SPAN.lo); println(SPAN.hi); }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "-7\n3\n");
}

#[test]
fn test_ir_modbind_struct_emits_constant_struct() {
    // The IR contains the struct constant with the right field
    // values. `internal constant { i64, i64 } { i64 N, i64 M }`
    // is the canonical shape; LLVM collapses all-zero aggregates
    // to `zeroinitializer`, so we use a non-zero value here to
    // exercise the explicit-field path.
    let ir = ir_for(
        "struct Pt { x: i64, y: i64 }\n\
             let CORNER: Pt = Pt { x: 3, y: 7 };\n\
             fn main() { println(CORNER.x); }",
    );
    assert!(
        ir.contains("@CORNER = internal constant { i64, i64 } { i64 3, i64 7 }"),
        "expected @CORNER const struct in IR, got:\n{}",
        ir
    );
}

#[test]
fn test_e2e_defer_in_nested_block_fires_at_inner_block_exit() {
    // Bare `{ ... }` (ExprKind::Block) routes through
    // `compile_block_with_frame` in slice 1.5, so a defer
    // inside the nested block scopes to that block — fires
    // at the inner block's end-of-emission, before any
    // outer-block statements that follow. Expected stream:
    // `before` → `in-block` → defer body (`inner-defer`) →
    // `after`.
    let out = run_program(
        r#"
fn main() {
    println("before");
    {
        defer { println("inner-defer"); }
        println("in-block");
    };
    println("after");
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["before", "in-block", "inner-defer", "after"]);
    }
}

/// All four scheduler FFI declarations are emitted (regression
/// guard against accidental removal from `Codegen::new`).
#[test]
fn test_scheduler_ffi_declarations_present() {
    let ir = ir_for("fn nop() {}");
    for sym in [
        "declare ptr @karac_runtime_spawn",
        "declare i8 @karac_runtime_task_join",
        "declare void @karac_runtime_task_handle_free",
        "declare i8 @karac_runtime_task_state",
    ] {
        assert!(ir.contains(sym), "expected `{sym}` declaration; ir:\n{ir}");
    }
}

#[test]
fn test_e2e_large_n_sort_by_named_struct_field() {
    // Runtime-path (N>64) named-struct field comparator. The inline
    // thunk re-compiles `a.v.cmp(b.v)`, so it must register the
    // closure params' Kāra type name (`Score`) for the `.v` field
    // access to resolve — the same registration the mono path (N≤64)
    // already did. Before the fix the thunk's compare lowered to a
    // constant → an always-equal comparator → `karac_vec_sort_by`
    // left the 100 records in their original (descending-key) order,
    // so `bad` would be 99 instead of 0. Tuples (numeric `.0`) were
    // never affected; this is the struct-only witness.
    let out = run_program(
        r#"
struct Score { v: i64 }
fn main() {
    let mut v: Vec[Score] = Vec.new();
    let mut i: i64 = 0;
    while i < 100 {
        v.push(Score { v: (i * 37 + 5) % 100 });
        i = i + 1;
    }
    v.sort_by(|a, b| a.v.cmp(b.v));
    let mut bad: i64 = 0;
    let mut prev: i64 = -1;
    let mut j: i64 = 0;
    while j < 100 {
        let s = v.get(j).unwrap();
        if s.v < prev { bad = bad + 1; }
        prev = s.v;
        j = j + 1;
    }
    println(bad);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "0");
    }
}

#[test]
fn test_e2e_large_n_sort_by_named_struct_field_is_stable() {
    // Stability on the runtime path with a named-struct element: 90
    // records keyed by `k = i % 9` (duplicates) with a strictly
    // increasing `tag = i`. Sorting by `k` alone must keep equal-key
    // records in original (`tag`-ascending) order. 0 ⇒ stable sort
    // AND the struct-field comparator resolved correctly.
    let out = run_program(
        r#"
struct Rec { k: i64, tag: i64 }
fn main() {
    let mut v: Vec[Rec] = Vec.new();
    let mut i: i64 = 0;
    while i < 90 {
        v.push(Rec { k: i % 9, tag: i });
        i = i + 1;
    }
    v.sort_by(|a, b| a.k.cmp(b.k));
    let mut bad: i64 = 0;
    let mut pk: i64 = -1;
    let mut pt: i64 = -1;
    let mut j: i64 = 0;
    while j < 90 {
        let r = v.get(j).unwrap();
        if r.k < pk { bad = bad + 1; }
        if r.k == pk {
            if r.tag < pt { bad = bad + 1; }
        }
        pk = r.k;
        pt = r.tag;
        j = j + 1;
    }
    println(bad);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "0");
    }
}

#[test]
fn e2e_a_derived_key_still_hashes_and_compares_structurally() {
    // The counterweight to the test above: adding user-impl dispatch must
    // not disturb the derive path, which is what almost every Map key uses.
    // Same struct, same operations, derives instead of impls — every answer
    // is the structural one.
    let out = run_program(
        r#"
#[derive(Hash, Eq, PartialEq)]
struct Item { id: i64, tag: i64 }
fn main() {
    let mut m: Map[Item, i64] = Map.new();
    m.insert(Item { id: 1, tag: 100 }, 10);
    m.insert(Item { id: 1, tag: 999 }, 11);
    println(m.len());
    match m.get(Item { id: 1, tag: 0 }) { Some(v) => println(v), None => println(-1) }
    let mut s: Set[Item] = Set.new();
    s.insert(Item { id: 5, tag: 1 });
    s.insert(Item { id: 5, tag: 2 });
    println(s.len());
}
"#,
    );
    assert_eq!(
        out.expect("the derived key must still build").trim(),
        "2
-1
2"
    );
}

#[test]
fn test_binop_rejects_heterogeneous_types_with_structured_error() {
    // Bug surfaced during c.4 bench: `assert_eq(m.get("a"), 1)`
    // where the LHS is `Option[i64]` (a 4-i64 struct) and the RHS
    // is `i64` panicked codegen at `compile_binop_typed`'s
    // `into_int_value()` — the struct branch above only fires when
    // both operands are structs, the float branch covers mixed
    // float, and the int path assumed both sides were ints.
    // Post-fix: a structured Err with the operand type and a hint
    // pointing at the typechecker gap. User-facing message must
    // mention `Eq` (the op) and `non-comparable type` so the
    // diagnostic is searchable.
    let src = r#"
fn main() {
    let mut m: Map[String, i64] = Map.new();
    m.insert("a", 1);
    assert_eq(m.get("a"), 1);
}
"#;
    let mut parsed = karac::parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);
    let err = compile_to_ir(&parsed.program, None, None)
        .expect_err("expected codegen to reject Option[i64] == i64")
        .message;
    assert!(
        err.contains("Binary op Eq"),
        "expected mention of Eq op; got: {err}"
    );
    assert!(
        err.contains("non-comparable type"),
        "expected mention of non-comparable type; got: {err}"
    );
    assert!(
        err.contains("typechecker gap"),
        "expected hint pointing at typechecker; got: {err}"
    );
}

#[test]
fn test_ir_refinement_param_uses_base_layout() {
    // A refinement over `i64` lowers its parameter to an `i64`, not a
    // pointer / struct — the base-layout resolution at work.
    let ir = ir_for(
        "type Even = i64 where self % 2 == 0;
             fn takes(e: Even) -> i64 { e + e }",
    );
    assert!(
        ir.contains("i64 @takes(i64") || ir.contains("define i64 @takes(i64"),
        "refinement param should lower to its i64 base:\n{ir}"
    );
}

// ── Distinct types — constructor + .raw() (zero-cost) ──────────

#[test]
fn test_e2e_distinct_constructor_and_raw() {
    // `UserId(42)` wraps a base value (zero-cost) and `.raw()` unwraps
    // it — at codegen the wrapper is invisible (i64 base layout).
    let out = run_program(
        r#"
distinct type UserId = i64;
fn main() {
    let u = UserId(42);
    let r: i64 = u.raw();
    println(r);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "42");
    }
}

// ── Combined `distinct type T = Base where pred` ───────────────

#[test]
fn test_e2e_distinct_where_constructor_holds() {
    // `Even(8)` passes the predicate — the constructor compiles to the
    // base value with the runtime assertion satisfied.
    let out = run_program(
        r#"
distinct type Even = i64 where self % 2 == 0;
fn mk(n: i64) -> Even { Even(n) }
fn main() { println(mk(8).raw()); }
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "8");
    }
}

#[test]
fn test_e2e_distinct_where_constructor_aborts() {
    // `Even(7)` violates the predicate at runtime: the constructor emits
    // a contract-violation abort, and code after it does not run.
    let captured = run_program_capturing(
        r#"
distinct type Even = i64 where self % 2 == 0;
fn mk(n: i64) -> Even { Even(n) }
fn main() {
    let e = mk(7);
    println(e.raw());
    println(42);
}
"#,
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("contract violated"),
            "expected a contract-violation abort, got stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
        assert!(
            !c.stdout.contains("42"),
            "code after a violated distinct constructor must not run"
        );
    }
}

#[test]
fn test_e2e_constructor_non_self_return_not_checked() {
    // A static associated function returning some other type (`-> i64`) is
    // not a constructor — its return value is NOT bound as `self` and NOT
    // invariant-checked, even though the type has an invariant. (Also a
    // regression guard: before this slice this aborted codegen with
    // `Undefined variable 'self'`.)
    let out = run_program(
        r#"
struct Counter { n: i64, invariant self.n >= 0 }
impl Counter { pub fn answer() -> i64 { 0 - 9 } }
fn main() { println(Counter.answer()); }
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "-9");
    }
}

/// B-2026-09-01-12 — the two repairs the contradicting-suffix diagnostic
/// offers both agree across every backend.
///
/// The row's own program (`Option[f32] = Option.Some(0.1f64)`) is now a type
/// error, so there is nothing left to run for it — which is the point: the
/// backends disagreed only on programs the checker can reject. What still has
/// to hold is that the fix-it does not trade a silent narrowing for a
/// run-vs-build split, the same obligation
/// `test_float_narrowing_as_cast_rounds_to_the_target_width` states for the
/// `as` the earlier gate recommends.
///
/// So both recommended spellings are pinned — drop the suffix and let the
/// destination type the literal, or keep it and add `as f32` — together with
/// the widening case that stays legal. `16777217` is there because it is not
/// representable in f32: before the fix `let c: f32 = 16777217.0f64` bound it
/// unrounded on all four surfaces, so the value doubles as the witness that
/// the annotation is now honoured rather than merely agreed upon.
/// Verified byte-identical under `karac run --interp`, `karac run`,
/// `karac build`, and `KARAC_AUTO_PAR=0 karac build`.
#[test]
fn test_e2e_contradicting_suffix_repairs_agree_across_backends() {
    assert_eq!(
            run_program(
                r#"fn main() {
    let a: Option[f32] = Option.Some(0.1);
    println(f"{a}");
    let b: Option[f32] = Option.Some(0.1f64 as f32);
    println(f"{b}");
    let c: f32 = 16777217.0;
    println(f"{c}");
    let d: f32 = 16777217.0f64 as f32;
    println(f"{d}");
    let w: f64 = 0.1f32;
    println(f"{w}");
}
"#
            ),
            Some(
                "Some(0.10000000149011612)\nSome(0.10000000149011612)\n16777216\n16777216\n0.10000000149011612\n".to_string()
            )
        );
}

// ── Phase-10: `host fn` native lowering ─────────────────────
// design.md § Host Functions: on native, a `host fn` lowers to the
// same call sequence as an `extern "C"` of equivalent signature.
// The item slice (90f2a101) made this true by construction —
// `host fn` parses into ExternFunction and codegen declares
// externs by signature without reading `.abi` — these tests pin
// it observably.

#[test]
fn host_fn_lowers_to_external_declaration() {
    let ir = ir_for(
        "effect resource Screen;\n\
             host fn host_paint(x: i64) -> i64 with writes(Screen);\n\
             fn main() { let _ = host_paint(1); }\n",
    );
    assert!(
        ir.contains("declare i64 @host_paint(i64"),
        "host fn must lower to a plain external declaration: {ir}",
    );
    assert!(
        !ir.contains("define i64 @host_paint"),
        "host fn must NOT get a body: {ir}",
    );
}

#[test]
fn test_e2e_field_target_param_view_assign_matches_the_identifier_spelling() {
    // B-2026-08-30-54 — `h.f = r` inside a match arm, where `r` is a
    // payload view of an OWNED enum param. The FIELD spelling of
    // B-2026-08-30-53, and it was wrong on BOTH backends in different
    // directions, which is why that row stopped at a bare identifier: the
    // interpreter ran the payload's body twice, and codegen retracted the
    // BASE's whole bodies action all-paths and permanently.
    //
    // The oracle is now the identifier spelling itself, which agrees on all
    // four surfaces since ed0a330b/b2122ccd — `dies`/`refreshed` below are
    // that test's `a`/`b`/`f` rows with `out` replaced by `h.f`, and they
    // must print the same thing.
    //
    // Codegen took the same shape as the identifier fix: a per-path
    // `cond_move_drop_flags` bool on the base, cleared where the store
    // happened, re-armed when the SAME field later receives a fresh value,
    // and consulted by the displaced-field emission. The interpreter took
    // its half through `moved_out_struct_field_bodies`, the per-FIELD mask
    // `drop_user_drop_fields_of_binding` already reads.
    //
    // Five shapes, measured against a stashed `src/`:
    //
    //   a  arm NOT taken       pre-fix `m dE a0`            (`dR0` lost)
    //   b  arm taken           pre-fix interp doubled `dR8`
    //   c  view then FRESH     pre-fix `dR5` lost, interp doubled `dR9`
    //   d  no view at all      unchanged boundary
    //   e  TWO fields, not     pre-fix `s dE e61` (both bodies lost)
    //      taken
    //
    // `e` is the row worth keeping an eye on: the flag guards the base's
    // WHOLE walk, so B-2026-08-01-19's over-suppression trade is unchanged
    // by this fix — a sibling field's body is still silenced while another
    // field holds a view. `e` uses the NOT-taken arm precisely so it pins
    // agreement rather than that residue.
    let out = run_program(
        r#"
struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("dE") } }
struct H { f: R }
struct H2 { f: R, g: R }

#[allow(partial_move_of_drop_enum)]
fn dies(b: E) -> i64 {
    let mut h: H = H { f: R { id: 0, tag: f"t0" } };
    match b { E.A(r) => { h.f = r; } E.B => { } }
    println("m");
    return h.f.id
}

#[allow(partial_move_of_drop_enum)]
#[allow(partial_move_of_drop_enum)]
fn refreshed(b: E) -> i64 {
    let mut h: H = H { f: R { id: 0, tag: f"t0" } };
    match b { E.A(r) => { h.f = r; } E.B => { } }
    println("m1");
    h.f = R { id: 5, tag: f"t5" };
    println("m2");
    return h.f.id
}

fn plain() -> i64 {
    let mut h: H = H { f: R { id: 20, tag: f"t20" } };
    h.f = R { id: 21, tag: f"t21" };
    println("p");
    return h.f.id
}

#[allow(partial_move_of_drop_enum)]
fn sibs(b: E) -> i64 {
    let mut h: H2 = H2 { f: R { id: 30, tag: f"t30" }, g: R { id: 31, tag: f"t31" } };
    match b { E.A(r) => { h.f = r; } E.B => { } }
    println("s");
    return h.f.id + h.g.id
}

fn main() {
    println(f"a{dies(E.B)}");
    println("-");
    println(f"b{dies(E.A(R { id: 8, tag: f"t8" }))}");
    println("-");
    println(f"c{refreshed(E.A(R { id: 9, tag: f"t9" }))}");
    println("-");
    println(f"d{plain()}");
    println("-");
    println(f"e{sibs(E.B)}");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
                out.trim(),
                "m\ndR0\ndE\na0\n-\ndR0\nm\ndE\ndR8\nb8\n-\ndR0\nm1\nm2\ndR5\ndE\ndR9\nc5\n-\ndR20\np\ndR21\nd21\n-\ns\ndR31\ndR30\ndE\ne61"
            );
    }
}

#[test]
fn test_e2e_field_param_view_leaves_sibling_field_bodies_armed() {
    // B-2026-09-02-10 — a param view landing in ONE field must not silence
    // the base's OTHER Drop-bearing fields.
    //
    // B-2026-08-01-19 retracted the base's whole bodies action when a field
    // received a view, and documented the over-suppression in its own code
    // comment; B-2026-08-30-54 replaced the retraction with a per-path bool
    // but kept it keyed by BINDING, so the residue survived and became
    // VISIBLE as a divergence for the first time (that row's `e` shape uses
    // the NOT-taken arm precisely to pin agreement rather than this).
    // `sibs` below is that shape with the arm TAKEN: `g` is never moved,
    // never viewed, and read on the line before, and it printed
    // `dR30 s dE dR8 v39` against `--interp`'s `dR30 s dR31 dE dR8 v39`.
    //
    // The fix moves the flag to the granularity of the REASON: one flag per
    // viewed FIELD, and a death-site branch tree selecting a walker masked
    // to exactly the fields a view landed in ON THAT PATH.
    //
    // Six shapes. `a`/`b` are the row's two, `b` being the same loss one
    // step later (a FRESH value in the sibling after the view, whose body
    // was silenced too).
    //
    // `c` IS THE ROW THE PER-BINDING SHORTCUT CANNOT PASS, and it is why
    // the flags are per field rather than one bool selecting between two
    // walkers. Two fields each take a view on INDEPENDENT paths; on the
    // path where only the first landed, a single bool cannot say that the
    // second still holds its own value. Measured pre-fix as
    // `dR30 s dE dE dR18 w81` against `--interp`'s
    // `dR30 s dR32 dR31 dE dE dR18 w81` — TWO bodies lost, not one.
    //
    // `d` is the all-armed path (no view lands anywhere), which must stay
    // byte-identical to before: the tree calls the REGISTERED walker
    // verbatim at that leaf, never a re-emitted equivalent.
    //
    // `e` pins the RE-ARM, which this fix simplified. B-2026-08-30-54 could
    // only re-arm when the viewed field was the ONLY one recorded, because
    // re-arming a per-binding bool with a sibling still viewed would
    // resurrect that sibling's body. With independent flags that condition
    // is gone, so here `f` is refreshed while `g` still holds a view and
    // both answers must be right at once.
    //
    // `f` is the boundary an over-eager fix would move: a struct with a
    // same-shaped field assignment whose RHS is a FRESH value, never a
    // view, which must be untouched.
    let out = run_program(
        r#"
struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("dE") } }
struct H2 { f: R, g: R }
struct H3 { f: R, g: R, k: R }

#[allow(partial_move_of_drop_enum)]
fn sibs(b: E) -> i64 {
    let mut h: H2 = H2 { f: R { id: 30, tag: f"t30" }, g: R { id: 31, tag: f"t31" } };
    match b { E.A(r) => { h.f = r; } E.B => { } }
    println("s");
    return h.f.id + h.g.id
}

#[allow(partial_move_of_drop_enum)]
fn sib_refreshed(b: E) -> i64 {
    let mut h: H2 = H2 { f: R { id: 30, tag: f"t30" }, g: R { id: 31, tag: f"t31" } };
    match b { E.A(r) => { h.f = r; } E.B => { } }
    h.g = R { id: 9, tag: f"t9" };
    println("s");
    return h.f.id + h.g.id
}

#[allow(partial_move_of_drop_enum)]
fn two_views(b: E, c: E) -> i64 {
    let mut h: H3 = H3 { f: R { id: 30, tag: f"t30" }, g: R { id: 31, tag: f"t31" }, k: R { id: 32, tag: f"t32" } };
    match b { E.A(r) => { h.f = r; } E.B => { } }
    match c { E.A(r2) => { h.g = r2; } E.B => { } }
    println("s");
    return h.f.id + h.g.id + h.k.id
}

#[allow(partial_move_of_drop_enum)]
fn rearm_with_sibling_viewed(b: E, c: E) -> i64 {
    let mut h: H3 = H3 { f: R { id: 40, tag: f"t40" }, g: R { id: 41, tag: f"t41" }, k: R { id: 42, tag: f"t42" } };
    match b { E.A(r) => { h.f = r; } E.B => { } }
    match c { E.A(r2) => { h.g = r2; } E.B => { } }
    h.f = R { id: 7, tag: f"t7" };
    println("s");
    return h.f.id + h.g.id + h.k.id
}

fn fresh_only() -> i64 {
    let mut h: H2 = H2 { f: R { id: 50, tag: f"t50" }, g: R { id: 51, tag: f"t51" } };
    h.f = R { id: 52, tag: f"t52" };
    println("p");
    return h.f.id + h.g.id
}

fn main() {
    println(f"a{sibs(E.A(R { id: 8, tag: f"t8" }))}");
    println("-");
    println(f"b{sib_refreshed(E.A(R { id: 8, tag: f"t8" }))}");
    println("-");
    println(f"c{two_views(E.A(R { id: 18, tag: f"t18" }), E.B)}");
    println("-");
    println(f"d{two_views(E.B, E.B)}");
    println("-");
    println(f"e{rearm_with_sibling_viewed(E.B, E.A(R { id: 6, tag: f"t6" }))}");
    println("-");
    println(f"f{fresh_only()}");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out.trim(),
            concat!(
                "dR30\ns\ndR31\ndE\ndR8\na39\n-",
                "\ndR30\ndR31\ns\ndR9\ndE\ndR8\nb17\n-",
                "\ndR30\ns\ndR32\ndR31\ndE\ndE\ndR18\nc81\n-",
                "\ns\ndR32\ndR31\ndR30\ndE\ndE\nd93\n-",
                "\ndR41\ndR40\ns\ndR42\ndR7\ndE\ndR6\ndE\ne55\n-",
                "\ndR50\np\ndR51\ndR52\nf103",
            )
        );
    }
}

#[test]
fn test_e2e_container_into_literal_field_arg_single_fire() {
    // B-2026-08-02-20 (leg 2) — a container moved into a struct literal
    // that is a CALL ARGUMENT (`v.push(Holder { xs: xs })`) must disarm
    // the source's element-bodies walk, exactly as the let-RHS position
    // already did. The consuming-ARG disarm handled only bare
    // identifiers, so the element body fired TWICE on both backends:
    // once at the source's NLL end, once at the container's death (one
    // logical value, two fires — parity-equal, so no test caught it).
    // The `let-rhs` block below is the always-correct control.
    let out = run_program(
        r#"
struct Res { id: i64, name: String }
impl Drop for Res {
    fn drop(mut ref self) { println(f"drop {self.id} {self.name}") }
}
struct Holder { xs: Vec[Res], tag: i64 }
fn main() {
    println("let-rhs:");
    {
        let mut xs: Vec[Res] = Vec.new();
        xs.push(Res { id: 1, name: f"a{1}" });
        let h = Holder { xs: xs, tag: 3 };
        println(h.tag);
    }
    println("call-arg:");
    {
        let mut v: Vec[Holder] = Vec.new();
        let mut ys: Vec[Res] = Vec.new();
        ys.push(Res { id: 2, name: f"b{2}" });
        v.push(Holder { xs: ys, tag: 4 });
        println(v.len());
    }
    println("end");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out.trim(),
            "let-rhs:\n3\ndrop 1 a1\ncall-arg:\n1\ndrop 2 b2\nend"
        );
    }
}

/// A user fn whose declared RETURN TYPE names a baked stdlib struct got an
/// `i64` LLVM signature while its body returned the real aggregate, so the
/// module failed verification with "Function return type does not match
/// operand type of return inst" (B-2026-08-25-2). Stdlib layouts were
/// declared AFTER user function signatures, so the name resolved against an
/// empty `struct_types` and hit `llvm_type_for_name`'s `i64` fall-through.
/// `-> Command` and `-> Regex` failed identically in modules that had been
/// registered for months, so this was never `std.cli`-specific.
#[test]
fn test_e2e_user_fn_returning_stdlib_struct_compiles() {
    let out = run_program(
        r#"
fn mk() -> Command { return Command.new("echo"); }

fn main() {
    let c = mk();
    println("ok");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "ok");
    }
}

/// The same missing-layout ordering, one position over: a user struct FIELD
/// naming a baked stdlib struct was laid out AS `i64`, so the enclosing
/// struct became `{ i64, i64 }` and the literal's `insertvalue` was rejected
/// against it. This is why the layout pass is hoisted ahead of the user's
/// `build_struct_types`, not merely ahead of its signatures.
#[test]
fn test_e2e_user_struct_field_naming_stdlib_struct_compiles() {
    let out = run_program(
        r#"
struct Holder { c: Command, n: i64 }

fn main() {
    let h = Holder { c: Command.new("echo"), n: 7 };
    println(h.n);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "7");
    }
}

// ── FFI export definitions (`extern "C" fn` / `extern "C-unwind" fn`) ──
// design.md § Panic Semantics at the FFI Boundary, cases 1 & 2.

/// Case 1: an `extern "C"` export compiles under the AOT backend and
/// runs correctly when called from Kāra. Proves the export flows
/// through the whole pipeline and gets a real (External-linkage,
/// un-mangled, C-calling-convention) symbol that links + executes.
#[test]
fn extern_c_export_compiles_and_runs_when_called_from_kara() {
    let out = run_program_capturing(
        "extern \"C\" fn add_one(x: i32) -> i32 { x + 1 }\n\
             fn main() { println(f\"{add_one(41)}\"); }\n",
    );
    if let Some(c) = out {
        assert_eq!(c.stdout.trim(), "42");
    }
}

/// Case 2: an `extern "C-unwind"` export is rejected by the backend
/// with a substrate-gate message (panic-unwind propagation needs the
/// Phase-7 unwind substrate, which this backend lacks). The effect
/// checker independently requires `with panics` here (tested in
/// tests/effectchecker.rs), so the declaration is present and the
/// rejection is purely the codegen gate.
///
/// It is grandfathered past the effect gate (`EFFECT_GATE_GRANDFATHERED`):
/// under the default abort-only profile the effect checker ALSO rejects the
/// export outright (`ExternCUnwindRequiresUnwindProfile`) — a second, later
/// interaction than the `with panics` one this comment describes. That is
/// what makes the program unreachable in production and the test a
/// deliberate negative; gating it would delete the codegen coverage.
#[test]
fn extern_c_unwind_export_rejected_by_backend() {
    use karac::codegen::compile_to_object_with_options;
    let src = "extern \"C-unwind\" fn f() -> i32 with panics { unreachable() }\n";
    let mut parsed = karac::parse(src);
    assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);
    let ownership = karac::ownershipcheck(&parsed.program, &typed);
    super::common::assert_check_clean(&resolved, &typed, src);
    // Effects are the THIRD phase of the same gate, and were simply
    // absent — a test could pin behaviour for a program `karac build`
    // refuses (B-2026-08-19-5). Runs after `lower`, threaded with the
    // typechecker's tables, exactly as `Pipeline::run_all_checks` does.
    super::common::assert_effects_clean_for(&parsed.program, &typed, src);
    super::common::assert_ownership_clean(&ownership, src);
    let obj = format!("/tmp/karac_ffi_cunwind_{}.o", std::process::id());
    let result =
        compile_to_object_with_options(&parsed.program, &obj, Some(&ownership), None, None, None);
    let _ = std::fs::remove_file(&obj);
    let err = result
        .expect_err("expected backend to reject extern \"C-unwind\" export")
        .message;
    assert!(
        err.contains("C-unwind") && err.contains("substrate"),
        "expected the substrate-gate message, got: {err}"
    );
}

#[test]
fn e2e_multi_assign_nested_in_control_flow() {
    // Exercises the desugar walker's recursion: parallel assignments nested
    // inside a while loop, an if branch, and a block-expr must all expand.
    // A bubble-sort pass (swap in a loop) plus a conditional swap.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let mut v: Vec[i64] = Vec.new();\n\
                 v.push(3i64); v.push(1i64); v.push(2i64);\n\
                 let n = v.len();\n\
                 let mut i = 0i64;\n\
                 while i < n {\n\
                     let mut j = 0i64;\n\
                     while j + 1i64 < n {\n\
                         if v[j] > v[j + 1i64] {\n\
                             v[j], v[j + 1i64] = v[j + 1i64], v[j];\n\
                         }\n\
                         j = j + 1i64;\n\
                     }\n\
                     i = i + 1i64;\n\
                 }\n\
                 let extra = { let mut p = 9i64; let mut q = 8i64; p, q = q, p; p - q };\n\
                 println(f\"{v[0]} {v[1]} {v[2]} {extra}\");\n\
             }",
    ) {
        assert_eq!(out, "1 2 3 -1\n");
    }
}

/// B-2026-08-08-10 — a user-declared struct whose name collides with a
/// prelude type must get its OWN layout, not the built-in's.
///
/// Stdlib and user types share one namespace (B-2026-08-02-13), and
/// `llvm_type_for_type_expr` keys its built-in HANDLE lowerings on the NAME
/// alone: `Column` / `DataFrame` / `Tensor` / `Interner` all lower to a bare
/// `ptr`. So `struct Column { data: Vec[i64] }` returned by value from a
/// function declared `-> Column` emitted a body returning the real
/// aggregate against a signature returning `ptr`, and the build died at LLVM
/// module verification — "Function return type does not match operand type
/// of return inst" — with nothing pointing at the source. `karac check`
/// passed and the interpreter ran it, so this was a run-vs-build divergence
/// presented as a raw verifier message.
///
/// Neither generics nor the `Vec` field is the trigger, which is worth
/// pinning because the row was filed as a generic-struct-return gap: the
/// NON-GENERIC case below failed identically pre-fix, and `Wrap[T]` (a
/// generic struct with a heap field and no name collision) always built.
/// The collision is the whole story.
#[test]
fn user_struct_shadowing_a_prelude_type_name_keeps_its_own_layout() {
    // Generic, the shape the row was filed with.
    assert_eq!(
        run_program(
            "struct Column[T] { data: Vec[T] }\n\
                 fn from_vec[T](v: Vec[T]) -> Column[T] { return Column { data: v }; }\n\
                 fn main() {\n\
                     let v: Vec[u32] = [1, 2, 3];\n\
                     let c: Column[u32] = from_vec(v);\n\
                     println(c.data.len().to_string());\n\
                 }",
        )
        .as_deref(),
        Some("3\n")
    );
    // NON-generic — same failure pre-fix, which is what rules out the
    // generic-monomorph return path as the cause.
    assert_eq!(
        run_program(
            "struct Column { data: Vec[i64], name: String }\n\
                 fn make(v: Vec[i64]) -> Column {\n\
                     return Column { data: v, name: \"col\".to_string() };\n\
                 }\n\
                 fn main() {\n\
                     let v: Vec[i64] = [1, 2, 3, 4];\n\
                     let c: Column = make(v);\n\
                     println((c.data.len() + c.name.len()).to_string());\n\
                 }",
        )
        .as_deref(),
        Some("7\n")
    );
    // The other handle-shaped prelude names on the same arm, each of which
    // lowered to `ptr` by name alone.
    for (ty, n) in [("DataFrame", 3), ("Interner", 5), ("Tensor", 2)] {
        let elems = (1..=n)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let src = format!(
            "struct {ty}[T] {{ items: Vec[T] }}\n\
                 fn of[T](v: Vec[T]) -> {ty}[T] {{ return {ty} {{ items: v }}; }}\n\
                 fn main() {{\n\
                     let v: Vec[u32] = [{elems}];\n\
                     let d: {ty}[u32] = of(v);\n\
                     println(d.items.len().to_string());\n\
                 }}"
        );
        assert_eq!(
            run_program(&src).as_deref(),
            Some(format!("{n}\n").as_str()),
            "user `struct {ty}` must keep its own layout"
        );
    }
    // Control: a generic struct with a heap field and NO name collision
    // always worked, and must keep working.
    assert_eq!(
        run_program(
            "struct Holder[T] { data: Vec[T] }\n\
                 fn from_vec[T](v: Vec[T]) -> Holder[T] { return Holder { data: v }; }\n\
                 fn main() {\n\
                     let v: Vec[u32] = [1, 2, 3];\n\
                     let h: Holder[u32] = from_vec(v);\n\
                     println(h.data.len().to_string());\n\
                 }",
        )
        .as_deref(),
        Some("3\n")
    );
}

/// B-2026-08-26-41 — the compiled twin of
/// `a_user_impl_drop_on_a_map_key_fires_like_one_on_a_value`, asserting
/// the same bytes. A user `impl Drop` on a map KEY type never ran:
/// `emit_map_val_user_drop_bodies_fn` shipped for values (B-2026-07-30-11)
/// and keys were left with `emit_map_key_drop_fn_walk`, which reclaims
/// MEMORY and runs no bodies. So an RAII key — a handle, a lock guard, a
/// connection — silently never released, and no sanitizer could see it
/// because nothing leaked.
///
/// The walk needed no new machinery: `at_key_half` already existed for
/// `Set`/`SortedSet`, whose elements live in the key half, so the Map key
/// walk is that configuration with the Map's own `key_size + val_size`
/// stride. What it needed was a distinct SYMBOL and admission to
/// `is_container_elem_bodies_fn`, without which it fired at scope exit
/// while the value walk fired at the binding's NLL end — a run-vs-build
/// divergence, since the interpreter fires both at the NLL point.
///
/// THE SYMBOL NAME CARRIES THE HALF (`__karac_dropkeys_` vs
/// `__karac_dropelems_`) and must keep doing so. Both halves mangle the
/// same map `TypeExpr`, so a shared name makes `get_function` hand back the
/// VALUE walker for a key request — every value body then runs twice and no
/// key body runs at all, which is a double free for a heap-owning `V` and
/// silent under a sanitizer that never sees a second `Drop` as an error.
/// This test's exact expectation is what catches it: the collision shows up
/// as a repeated `dropV` with `dropK` missing.
/// B-2026-08-27-7 — the ESCAPE HATCH, pinned across backends.
///
/// Element destruction order in a `Map`/`Set` is unspecified and a
/// different permutation on each backend (design.md § Map), so no test can
/// assert it and none does — every walker test in the tree asserts COUNTS.
/// That is exactly why an order regression in one of them is invisible, and
/// it is why this test exists: `SortedMap`/`SortedSet` are what design.md
/// points code needing a defined order at, so their destruction order is
/// the one an assertion can pin, and pinning it is what keeps the advice
/// true.
///
/// Every literal here is seed-independent and was measured identical on all
/// three surfaces — `karac run --interp`, the JIT, and this AOT build — at
/// seeds 1 and 7. Interpreter twin:
/// `a_sorted_container_destroys_its_elements_in_key_order`.
///
/// Keys are inserted DESCENDING (`5 - i`, `50 - j`) so key order and
/// insertion order disagree; inserting ascending would let a walk that
/// ignored the ordering pass. The keys all fire before the values because
/// the two halves are separate walks over the table rather than one
/// interleaved pass — and `V4` first because key 1 holds the value inserted
/// last, which is the detail an insertion-ordered value walk would get
/// backwards while still printing the right multiset.
/// `Vec[T] == Vec[T]` compares CONTENTS on the compiled backends
/// (B-2026-08-27-10).
///
/// The whole matrix in one program because the defect was shape-selective
/// in a way a single case hides. `compile_binop`'s struct path dispatched
/// on LLVM FIELD COUNT, and `String` and `Vec` are both
/// `{ ptr, i64, i64 }` — so a `Vec` was compared AS A STRING:
/// `len_eq && memcmp(min(len))` over the element buffer, reading `len`
/// BYTES of what are `len` ELEMENTS.
///
/// That reads as two unrelated bugs unless both legs are present.
/// `[1,2] == [1,9]` answered TRUE (the leading two bytes of both `i64`
/// buffers are `01 00`), while two `Vec[String]` with equal contents
/// answered FALSE (it compared the low bytes of two heap pointers). The
/// differing-LENGTH and empty cases answered correctly throughout, which
/// is why a test built from those alone would have passed against the bug.
///
/// The `ref` leg is here because it was a SECOND miss: the first fix keyed
/// off an owned-`Vec` table, so `ref Vec[T] == ref Vec[T]` stayed wrong
/// (answering `true` for differing contents) until the oracle learned to
/// peel borrows, as the typechecker's own `Eq` arm already does.
/// Every three-field user struct compares correctly (B-2026-08-27-18).
///
/// `compile_binop` decided "is this a String/Vec?" by COUNTING FIELDS, and
/// `{ptr, i64, i64}` has three — so every three-field struct was routed to
/// the byte comparator. Three of the shapes below CRASHED the compiler
/// (`into_pointer_value()` on a non-pointer field 0) and one answered
/// WRONG, which is why they are all in one fixture: a repair that only
/// stops the panic leaves the wrong answer standing, and vice versa.
///
/// `Shared3` is that fourth shape and the reason the obvious narrower fix
/// — checking the three field TYPES instead of the count — does not work.
/// User structs get LITERAL, structurally-uniqued LLVM types, so a struct
/// whose first field is a POINTER lowers to the SAME `{ptr, i64, i64}`
/// type object as a `String`; a `shared` field is the ordinary way to get
/// one. It answered `true` for two values differing in their last field,
/// because the comparator read field 0 as a data pointer, field 1 as a
/// length, and memcmp'd that many bytes of two RC blocks that happened to
/// match. No diagnostic, no crash.
///
/// `Nested` pins the recursion: `compile_struct_eq` walks fields through
/// `compile_binop`, so a three-field struct inside a two-field one reached
/// the same dispatch and crashed even though neither struct is three
/// fields at the top level. The two- and four-field controls are here so a
/// regression that disables the whole struct path fails visibly instead of
/// quietly passing the interesting rows.
#[test]
fn test_e2e_three_field_struct_equality() {
    assert_eq!(
        run_program(
            r#"
#[derive(PartialEq, Eq)]
struct Plain3 { a: i64, b: i64, c: i64 }
#[derive(PartialEq, Eq)]
struct StrFirst { s: String, x: i64, y: i64 }
#[derive(PartialEq, Eq)]
struct StrLast { x: i64, y: i64, s: String }
#[derive(PartialEq, Eq)]
shared struct Node { v: i64 }
#[derive(PartialEq, Eq)]
struct Shared3 { h: Node, a: i64, b: i64 }
#[derive(PartialEq, Eq)]
struct Nested { i: Plain3, z: i64 }
#[derive(PartialEq, Eq)]
struct Two { a: i64, b: i64 }
#[derive(PartialEq, Eq)]
struct Four { a: i64, b: i64, c: i64, d: i64 }

fn main() {
    println(f"{Plain3 { a: 1, b: 2, c: 3 } == Plain3 { a: 1, b: 2, c: 3 }}");
    println(f"{Plain3 { a: 1, b: 2, c: 3 } == Plain3 { a: 1, b: 2, c: 9 }}");
    println(f"{StrFirst { s: "abc", x: 1, y: 0 } == StrFirst { s: "abc", x: 1, y: 0 }}");
    println(f"{StrFirst { s: "abc", x: 1, y: 0 } == StrFirst { s: "axc", x: 1, y: 0 }}");
    println(f"{StrFirst { s: "abc", x: 1, y: 0 } == StrFirst { s: "abc", x: 7, y: 0 }}");
    println(f"{StrLast { x: 1, y: 2, s: "hi" } == StrLast { x: 1, y: 2, s: "hi" }}");
    println(f"{StrLast { x: 1, y: 2, s: "hi" } == StrLast { x: 1, y: 2, s: "no" }}");
    println(f"{Shared3 { h: Node { v: 1 }, a: 5, b: 6 } == Shared3 { h: Node { v: 1 }, a: 5, b: 6 }}");
    println(f"{Shared3 { h: Node { v: 1 }, a: 5, b: 6 } == Shared3 { h: Node { v: 1 }, a: 5, b: 9 }}");
    println(f"{Nested { i: Plain3 { a: 1, b: 2, c: 3 }, z: 7 } == Nested { i: Plain3 { a: 1, b: 2, c: 3 }, z: 7 }}");
    println(f"{Nested { i: Plain3 { a: 1, b: 2, c: 3 }, z: 7 } == Nested { i: Plain3 { a: 1, b: 2, c: 9 }, z: 7 }}");
    println(f"{Two { a: 1, b: 2 } == Two { a: 1, b: 9 }}");
    println(f"{Four { a: 1, b: 2, c: 3, d: 4 } == Four { a: 1, b: 2, c: 3, d: 9 }}");
}
"#,
        ),
        Some(
            "true\nfalse\n\
                 true\nfalse\nfalse\n\
                 true\nfalse\n\
                 true\nfalse\n\
                 true\nfalse\n\
                 false\nfalse\n"
                .to_string()
        )
    );
}

/// Tuple `<` / `<=` / `>` / `>=` and `==` / `!=` on the compiled backend
/// (B-2026-08-27-33).
///
/// `type_supports_ord` / `type_supports_partial_eq` have recursed through
/// `Type::Tuple` since long before this row, so `karac check` accepted
/// every line below — and then nothing ran them: `karac build` refused
/// ordering with "Unsupported struct binary op: Lt" and the interpreter
/// died claiming the typechecker rejects this, which it does not. A
/// run-vs-build hole in a shape the checker calls legal, not a rejection.
///
/// The comparator was already there and already ordering these exact
/// tuples: `emit_cmp_fn_for_type_expr` has had a `TypeKind::Tuple` arm all
/// along, which is why `Vec[(i64, i64)].sort()` (the last row) has always
/// worked while the bare operator did not. What was missing is the
/// operator DISPATCH — it resolves the operand's type by NAME, and a tuple
/// has none, so the span-keyed operand table now carries tuples too.
///
/// The THREE-element rows are a second, independent defect, and the reason
/// arity is varied here rather than fixed at two. A three-scalar tuple
/// lowers to `{i64, i64, i64}` — structurally the same LLVM object as the
/// `{ptr, i64, i64}` String/Vec header — so `compile_binop`'s field-COUNT
/// dispatch sent `(1, 2, 3) == (1, 2, 4)` to the String byte-comparator,
/// which extracted field 0 as a pointer and PANICKED the compiler. Two-
/// and four-element tuples miss the collision entirely, so a fixture built
/// only from pairs passes against it. Exact sibling of B-2026-08-27-18,
/// which is the same collision for a three-field user struct, and the
/// `nest3` row pins the recursion the way that row's `Nested` does: a
/// three-tuple INSIDE a two-tuple walks back into the same dispatch unless
/// the equality is driven by the element `TypeExpr`.
///
/// The float rows are the deliberate NON-fix, pinned so a later widening
/// has to be deliberate. `value_compare` and `karac_cmp_<T>` are both the
/// IEEE 754 TOTAL order (NaN last) because every other caller is a sort
/// key; that is not what `<` means on an `f64`, so a bare-float element
/// declines on BOTH backends rather than being given invented semantics.
/// Equality has no such problem — it is IEEE element-wise — so
/// `(1, 1.5, 2) == (1, 2.5, 2)` answers here while `(1, 1.5) < (1, 2.5)`
/// still refuses to build.
#[test]
fn test_e2e_tuple_comparison_and_equality() {
    assert_eq!(
        run_program(
            r#"
fn main() {
    let a = (1, 2);
    let b = (1, 3);
    println(f"{a < b}");
    println(f"{b < a}");
    println(f"{a <= a}");
    println(f"{a >= a}");
    println(f"{a > b}");
    println(f"{a == b}");
    println(f"{a != b}");
    println(f"{a == a}");

    let sa = ("b", 1);
    let sb = ("b", 2);
    let sc = ("c", 0);
    println(f"{sa < sb}");
    println(f"{sb < sc}");
    println(f"{sc < sa}");
    println(f"{sa == sa}");

    let na = (1, (2, 3));
    let nb = (1, (2, 4));
    println(f"{na < nb}");
    println(f"{na == nb}");

    let t3 = (1, 2, 3);
    let u3 = (1, 2, 4);
    println(f"{t3 < u3}");
    println(f"{t3 == u3}");
    println(f"{t3 == t3}");
    println(f"{t3 != u3}");

    let n3a = ((1, 2, 3), 4);
    let n3b = ((1, 2, 5), 4);
    println(f"{n3a == n3b}");
    println(f"{n3a == n3a}");

    let f3a = (1, 1.5, 2);
    let f3b = (1, 2.5, 2);
    println(f"{f3a == f3b}");

    let mut v: Vec[(i64, i64)] = Vec.new();
    v.push((3, 1));
    v.push((1, 9));
    v.push((2, 5));
    v.sort();
    for e in v {
        println(f"{e.0}:{e.1}");
    }
}
"#,
        ),
        // a<b, b<a, a<=a, a>=a, a>b, a==b, a!=b, a==a,
        // String-first tuples: sa<sb, sb<sc, sc<sa, sa==sa,
        // nested: na<nb, na==nb,
        // three-wide: t3<u3, t3==u3, t3==t3, t3!=u3,
        // three inside two: n3a==n3b, n3a==n3a,
        // float element under `==` (IEEE, element-wise),
        // then the already-working `Vec[(i64, i64)].sort()`.
        Some(
            "true\nfalse\ntrue\ntrue\nfalse\nfalse\ntrue\ntrue\n\
                 true\ntrue\nfalse\ntrue\n\
                 true\nfalse\n\
                 true\nfalse\ntrue\ntrue\n\
                 false\ntrue\n\
                 false\n\
                 1:9\n2:5\n3:1\n"
                .to_string()
        )
    );
}

/// `.cmp()` on a TUPLE, on every surface (B-2026-08-27-41).
///
/// The operator spelling of this comparison has worked since
/// B-2026-08-27-33 — `(1, 2) < (1, 3)` lowers through `karac_cmp_<T>`, and
/// `Vec[(i64, i64)].sort()` has driven that same comparator far longer. The
/// METHOD spelling reached none of it: the typechecker's `cmp` intercept
/// was gated to `Type::Named`, so `(1, 2).cmp((1, 3))` was refused outright
/// with "no method 'cmp' on type '(i64, i64)'", and had it been admitted
/// both backends would have fallen through their method dispatch anyway
/// (the interpreter had no `Value::Tuple` arm; codegen's
/// `compile_user_cmp_to_ordering` was keyed by type NAME, which a tuple
/// does not have). Three arms, one missing dispatch each — every
/// comparator involved already existed.
///
/// Which is why this is not merely about how the comparison is spelled: a
/// `<` written against a bounded type PARAMETER lowers to `T.cmp`, so the
/// method gap — not the operator — is what sits between B-2026-08-27-33's
/// bound fix and `PriorityQueue[(i64, i64)]`.
///
/// Rows 10 and 11 are the NEIGHBOURS, and they are the point of the
/// fixture as much as the tuples are. The typechecker arm this widens is
/// the same one that gates `#[derive(Ord)]` structs behind
/// `!has_user_impl_ord`, and row 11's `Rev` — whose hand-written `cmp`
/// REVERSES the order — is the shape B-2026-08-26-10 filed when the
/// structural comparator answered for a user impl. It must still report
/// `Greater` for `1.cmp(2)`. No derive gate carries over to the tuple arm,
/// deliberately: a tuple is structural, there is no name to hang an `impl
/// Ord` on, so there is no user ordering to overrule.
///
/// Arity is varied for the reason `test_e2e_tuple_comparison_and_equality`
/// varies it — a three-scalar tuple lowers to `{i64, i64, i64}`, the same
/// LLVM object as the `{ptr, i64, i64}` String/Vec header — and row 06
/// nests a three-tuple inside a two-tuple so the recursion has to be
/// driven by the element `TypeExpr` rather than by field count.
///
/// Twinned against the interpreter rather than pinned to a literal, since
/// the whole defect was the two backends disagreeing about whether this
/// program exists at all.
#[test]
fn test_e2e_tuple_cmp_to_ordering() {
    let src = r#"
#[derive(Ord, Eq)]
struct P { a: i64, b: i64 }

struct Rev { v: i64 }
impl PartialEq for Rev { fn eq(ref self, other: ref Rev) -> bool { self.v == other.v } }
impl Eq for Rev {}
impl PartialOrd for Rev { fn partial_cmp(ref self, other: ref Rev) -> Option[Ordering] { Some(other.v.cmp(self.v)) } }
impl Ord for Rev { fn cmp(ref self, other: ref Rev) -> Ordering { other.v.cmp(self.v) } }

fn tag(o: Ordering) -> i64 {
    if o.is_lt() { return 0; }
    if o.is_eq() { return 1; }
    return 2;
}

fn main() {
    println(f"01 {tag((1, 2).cmp((1, 3)))}");
    println(f"02 {tag((1, 2).cmp((1, 2)))}");
    println(f"03 {tag((1, 2).cmp((0, 9)))}");
    let a = ("b", 1);
    let b = ("b", 2);
    let c = ("c", 0);
    println(f"04 {tag(a.cmp(b))} {tag(b.cmp(c))} {tag(c.cmp(a))}");
    println(f"05 {tag((1, 2, 3).cmp((1, 2, 4)))} {tag((1, 2, 3).cmp((1, 2, 3)))}");
    println(f"06 {tag((1, (2, 3)).cmp((1, (2, 4))))} {tag(((1, 2, 3), 4).cmp(((1, 2, 5), 4)))}");
    println(f"07 {tag((true, 'z').cmp((true, 'a')))} {tag((false, 'a').cmp((true, 'a')))}");
    match (5, 1).cmp((5, 1)) {
        Ordering.Less => { println("08 Less"); }
        Ordering.Equal => { println("08 Equal"); }
        Ordering.Greater => { println("08 Greater"); }
    }
    let mut v = [(2, 1), (1, 5), (1, 2), (0, 9)];
    v.sort_by(|x, y| x.cmp(y));
    for e in v { println(f"09 {e.0}:{e.1}"); }
    let p1 = P { a: 1, b: 2 };
    let p2 = P { a: 1, b: 3 };
    println(f"10 {tag(p1.cmp(p2))} {tag("abc".cmp("abd"))} {tag(7.cmp(3))} {tag('a'.cmp('b'))}");
    let r1 = Rev { v: 1 };
    let r2 = Rev { v: 2 };
    println(f"11 {tag(r1.cmp(r2))}");
}
"#;
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(
        interp_errs.is_empty(),
        "interpreter errors: {interp_errs:?}"
    );
    let expected = interp_out.join("");
    // Anti-vacuity. The oracle must show the comparator actually ordering
    // tuples (not answering Equal for everything, the shape a missing
    // comparison arm degrades to) AND the hand-written `impl Ord` still
    // winning — otherwise this asserts agreement on two wrong answers.
    assert!(
        expected.contains("01 0")
            && expected.contains("03 2")
            && expected.contains("09 0:9")
            && expected.contains("11 2"),
        "interpreter oracle is not ordering tuples: {expected:?}"
    );
    let Some(aot) = run_program(src) else { return };
    assert_eq!(
        aot, expected,
        "compiled `.cmp()` on a tuple must match the interpreter",
    );
}

/// A FIELD READ ON A BLOCK-EXPRESSION RECEIVER — `{ let x = make(); x }.n`
/// (B-2026-08-27-49).
///
/// `karac check` accepted this and the interpreter answered it, but
/// `karac build` died on codegen's loud "cannot resolve field 'n' on this
/// receiver" guard, while the same read through a local
/// (`let t = { … }; t.n`) compiled. Codegen compiles the block fine and gets
/// a proper struct value back; what it could not do was NAME the value's
/// type, because `compile_block_with_frame` reverts the block's scope before
/// any consumer asks and `type_name_of_expr` had no block arm.
///
/// The last two lines are why this outranks its hand-written spelling.
/// `lowering.rs`'s `rewrite_enum_literal_method_call` materializes a method
/// call on a unit-variant literal into exactly this block shape, so
/// `Color.Red.to_tag().n` — source containing no block at all — failed to
/// build with a diagnostic naming a receiver the author never wrote.
#[test]
fn test_e2e_field_read_on_a_block_expression_receiver() {
    let src = r#"
struct Inner { m: i64 }
struct Outer { inner: Inner, k: i64 }
struct Tag { n: i64 }

enum Color { Red, Green }

fn make() -> Tag { return Tag { n: 7 }; }
fn make2() -> Outer { return Outer { inner: Inner { m: 3 }, k: 4 }; }

impl Tag {
    fn doubled(self) -> i64 { return self.n * 2; }
}

impl Color {
    fn to_tag(self) -> Tag {
        match self {
            Red => { return Tag { n: 1 }; }
            Green => { return Tag { n: 2 }; }
        }
    }
}

fn main() {
    println({ let x = make(); x }.n);
    println({ let x = make(); x }.n + 1);
    println({ Tag { n: 5 } }.n);
    println({ let x = make2(); x }.inner.m);
    println({ let x = make2(); x }.k);
    println({ let x = make(); x }.doubled());
    println({ let a = make(); { let b = a; b } }.n);
    println(Color.Red.to_tag().n);
    println(Color.Green.to_tag().n);
    let c = Color.Green;
    println(c.to_tag().n);
}
"#;
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(
        interp_errs.is_empty(),
        "interpreter errors: {interp_errs:?}"
    );
    let expected = interp_out.join("");
    // Anti-vacuity. Lines 4 and 5 read BOTH fields of a two-field struct, so
    // a receiver typed as the wrong struct — which is what an off-by-one
    // field index means — cannot pass by coincidence, and the tail pins the
    // lowering-synthesized spelling (8, 9) against the bound one (10).
    assert_eq!(
        expected, "7\n8\n5\n3\n4\n14\n7\n1\n2\n2\n",
        "interpreter oracle is wrong for a block-expression receiver",
    );
    let Some(aot) = run_program(src) else { return };
    assert_eq!(
        aot, expected,
        "a compiled field read on a block receiver must match the interpreter",
    );
}

/// A struct field read THROUGH A TUPLE ELEMENT whose tuple came from a
/// CALL — `make().0.id` and `let q = make(); q.0.id` (B-2026-08-28-3).
///
/// Both failed `karac build` on the loud "cannot resolve field ... its type
/// was not recorded" guard while `karac check` accepted them and the
/// interpreter answered them, and the boundary was where the TUPLE's value
/// came from rather than how the read was spelled: `q.0.n` is identical in
/// the passing and failing lines, and only the initializer of `q` differs.
/// The element type of a tuple resolved through a place chain or a literal;
/// a call matched neither source.
///
/// `Other` and `Tag` both declare an `n`, at DIFFERENT indices, which is
/// what makes this fixture able to fail rather than merely refuse. Trading
/// the loud gap for a silent wrong answer would be strictly worse than the
/// bug — B-2026-08-27-49 measured exactly that outcome from the naive
/// version of its own fix — so the collision is deliberate at every row:
///
///   - the shadowing block reads a call-bound `x` that shadows an outer
///     tuple local of the OTHER struct, and prints 42 afterwards to prove
///     the registry was restored rather than clobbered;
///   - `q.0.a` pins the SECOND field, so an off-by-one index that happens
///     to satisfy `n` still fails;
///   - `deep().0.inner.m` is a nested read, where this arm types the
///     receiver of the second hop rather than the first.
///
/// The method-call and associated-function rows are the other two callee
/// spellings that key `fn_return_type_exprs`; without them the fix could be
/// keyed on a bare `Identifier` callee alone and still look complete.
#[test]
fn test_e2e_struct_field_read_through_a_tuple_element_from_a_call() {
    let src = r#"
struct Other { a: i64, n: i64 }
struct Tag { n: i64, a: i64 }
struct Inner { m: i64 }
struct Outer { inner: Inner, k: i64 }

struct Holder { seed: i64 }
impl Holder {
    fn pair(ref self) -> (Tag, i64) { return (Tag { n: self.seed, a: 51 }, 2); }
    fn origin() -> (Tag, i64) { return (Tag { n: 100, a: 52 }, 3); }
}

fn make() -> (Tag, i64) { return (Tag { n: 7, a: 99 }, 1); }
fn deep() -> (Outer, i64) { return (Outer { inner: Inner { m: 5 }, k: 6 }, 8); }

fn main() {
    let x = (Other { a: 5, n: 42 }, 0);
    println(x.0.n);

    let q = make();
    println(q.0.n);
    println(q.0.a);
    println(make().0.n);

    {
        let x = make();
        println(x.0.n);
    }
    println(x.0.n);

    println(deep().0.inner.m);
    let d = deep();
    println(d.0.inner.m);
    println(d.0.k);

    let h = Holder { seed: 11 };
    let p = h.pair();
    println(p.0.n);
    println(h.pair().0.n);
    println(Holder.origin().0.n);
    let o = Holder.origin();
    println(o.0.n);
}
"#;
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(
        interp_errs.is_empty(),
        "interpreter errors: {interp_errs:?}"
    );
    let expected = interp_out.join("");
    // Row 5 is the shadowed read: 7 is the call-bound `Tag`'s `n`. 99 would
    // mean it typed as the outer `Other` and landed on `Tag.a`; 42 would
    // mean it read the outer binding outright. Row 6 back at 42 is the
    // restore.
    assert_eq!(
        expected, "42\n7\n99\n7\n7\n42\n5\n5\n6\n11\n11\n100\n100\n",
        "interpreter oracle is wrong for a tuple-element field read",
    );
    let Some(aot) = run_program(src) else { return };
    assert_eq!(
        aot, expected,
        "a tuple element's struct type must resolve when the tuple came from a call",
    );
}

/// A TUPLE ELEMENT projected out of a CONTAINER ELEMENT stays a COPY —
/// `v[0].0` over `Vec[(String, i64)]` (B-2026-08-28-24).
///
/// The abort this fixes is a memory error, gated under LSan by
/// `test_tuple_element_of_a_container_element_is_cloned_not_aliased`. What
/// THIS test pins is the semantics the fix had to pick, which no sanitizer
/// can see: the read is a COPY, so the container's element is unchanged
/// afterwards and still readable.
///
/// That is the whole difference between the two available fixes. Cloning
/// the read gives the destination its own buffer and leaves the element
/// alone. Cap-zeroing the SOURCE — spreading the move model the `let` site
/// used — also silences the abort, and B-2026-08-12-27 measured what it
/// costs: `let mut w = ps[0].word; w = w + "X";` then reading `ps[0].word`
/// printed garbage where the interpreter printed the value. So every row
/// here re-reads the container after consuming out of it, and the mutation
/// row asks the question directly.
///
/// The interpreter is the oracle rather than a hand-written expectation
/// because it is the definition of the copy semantics being pinned.
#[test]
fn test_e2e_tuple_element_of_a_container_element_is_a_copy() {
    let src = r#"
struct R { id: i64, name: String }

fn mks() -> Vec[(String, i64)] { return [(f"a{1}", 1), (f"b{2}", 2)]; }
fn mkr() -> Vec[(R, i64)] {
    return [(R { id: 1, name: f"a{1}" }, 1), (R { id: 2, name: f"b{2}" }, 2)];
}
fn ret_r(v: Vec[(R, i64)]) -> R { return v[0].0; }

fn main() {
    let v = mks();
    let mut w = v[0].0;
    w = w + "X";
    // the binding owns a copy; the container's element is untouched
    println(w);
    println(v[0].0);

    let mut o: Vec[String] = Vec.new();
    o.push(v[1].0);
    println(o[0]);
    println(v[1].0);

    let c = true;
    let d = if c { v[0].0 } else { v[1].0 };
    println(d);
    println(v[0].0);

    // a struct member, and the row's own escaping shape
    let r = ret_r(mkr());
    println(r.name);
    let s = mkr();
    let t = s[1].0;
    println(t.name);
}
"#;
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(
        interp_errs.is_empty(),
        "interpreter errors: {interp_errs:?}"
    );
    let expected = interp_out.join("");
    // `a1X` then `a1` is the copy: a move model prints `a1X` then empty or
    // garbage for the element.
    assert_eq!(
        expected, "a1X\na1\nb2\nb2\na1\na1\na1\nb2\n",
        "interpreter oracle is wrong for a tuple-element container read",
    );
    let Some(aot) = run_program(src) else { return };
    assert_eq!(
        aot, expected,
        "a tuple element read out of a container element must be a copy",
    );
}

/// A STRUCT-VALUED field read off a CONTAINER ELEMENT stays a COPY —
/// `q[1].r` over `Vec[Q]` where `Q { r: R, … }` (B-2026-08-28-35).
///
/// The abort is gated under LSan by
/// `test_struct_valued_field_of_a_container_element_is_cloned_not_aliased`.
/// What this pins is the semantics the fix had to pick, which no sanitizer
/// can see: the read is a COPY, so mutating the binding leaves the
/// container's element alone and the element is still readable afterwards.
///
/// That is the difference between cloning the read and cap-zeroing the
/// source — the move model that also silences the abort, and that
/// B-2026-08-12-27 measured printing garbage on the next read of the
/// source. The interpreter is the oracle because it is the definition of
/// the copy semantics being pinned.
#[test]
fn test_e2e_struct_valued_field_of_a_container_element_is_a_copy() {
    let src = r#"
struct R { id: i64, name: String }
struct Q { r: R, n: i64 }

fn mkq() -> Vec[Q] {
    return [Q { r: R { id: 1, name: f"a{1}" }, n: 1 },
            Q { r: R { id: 2, name: f"b{2}" }, n: 2 }];
}
fn ret_r(q: Vec[Q]) -> R { return q[0].r; }

fn main() {
    let q = mkq();
    let mut e = q[1].r;
    e.name = e.name + "X";
    e.id = 99;
    // the binding owns a copy; the container's element is untouched
    println(e.name);
    println(e.id);
    println(q[1].r.name);
    println(q[1].r.id);

    let mut o: Vec[R] = Vec.new();
    o.push(q[0].r);
    println(o[0].name);
    println(q[0].r.name);

    println(ret_r(mkq()).name);
}
"#;
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(
        interp_errs.is_empty(),
        "interpreter errors: {interp_errs:?}"
    );
    let expected = interp_out.join("");
    // `b2X`/99 on the binding and `b2`/2 back on the element is the copy; a
    // move model prints the mutated value (or garbage) for the element too.
    assert_eq!(
        expected, "b2X\n99\nb2\n2\na1\na1\na1\n",
        "interpreter oracle is wrong for a struct-valued container read",
    );
    let Some(aot) = run_program(src) else { return };
    assert_eq!(
        aot, expected,
        "a struct-valued field read off a container element must be a copy",
    );
}

/// A field read through a TUPLE ELEMENT OF A CONTAINER ELEMENT —
/// `v[0].0.id` over `Vec[(R, i64)]` (B-2026-08-28-34).
///
/// `place_chain_tuple_tes` has arms for an identifier, a field and a tuple
/// index but none for an `Index`, so a tuple reached through a container
/// had no source and the read failed `karac build` on the loud "cannot
/// resolve field" guard while `karac check` accepted it and the interpreter
/// answered it. Binding the element first always worked, which is what
/// isolates it to the resolution rather than the read.
///
/// `Other` and `Tag` both declare an `n`, at DIFFERENT indices, so a
/// mis-resolution is a wrong ANSWER here rather than a refusal — the trade
/// B-2026-08-27-49 measured and rejected, and the reason the new source is
/// gated on naming a type codegen has a layout for.
///
/// The `self`-rooted row matters separately: it resolves the container
/// through a FIELD (`self.xs[0]`), a different arm of
/// `vec_index_elem_type_expr` than a bare local.
#[test]
fn test_e2e_field_read_through_a_tuple_element_of_a_container_element() {
    let src = r#"
struct Other { a: i64, n: i64 }
struct Tag { n: i64, a: i64 }
struct R { id: i64, name: String }
struct H { xs: Vec[(R, i64)] }

impl H { fn peek(ref self) -> i64 { return self.xs[0].0.id; } }

fn take(v: Vec[(R, i64)]) -> i64 { return v[0].0.id; }

fn main() {
    let v: Vec[(Tag, i64)] = [(Tag { n: 7, a: 99 }, 1)];
    let w: Vec[(Other, i64)] = [(Other { a: 5, n: 42 }, 2)];
    println(v[0].0.n);
    println(v[0].0.a);
    println(w[0].0.n);
    println(w[0].0.a);

    let r: Vec[(R, i64)] = [(R { id: 41, name: f"n{41}" }, 1)];
    println(r[0].0.id);
    println(r[0].0.name);
    println(take(r));
    let h = H { xs: [(R { id: 8, name: f"m{8}" }, 1)] };
    println(h.peek());
}
"#;
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(
        interp_errs.is_empty(),
        "interpreter errors: {interp_errs:?}"
    );
    let expected = interp_out.join("");
    // 7/99 then 42/5: reading `Tag` at `Other`'s index for `n` would print
    // 99, and the reverse would print 5.
    assert_eq!(
        expected, "7\n99\n42\n5\n41\nn41\n41\n8\n",
        "interpreter oracle is wrong for a container-element tuple field read",
    );
    let Some(aot) = run_program(src) else { return };
    assert_eq!(
        aot, expected,
        "a tuple element of a container element must resolve its field layout",
    );
}

/// A field read on an `if` / `if let` EXPRESSION receiver —
/// `if c { make(1) } else { make(2) }.n` (B-2026-08-28-7 leg 2).
///
/// `ExprKind::If` is a different node kind from `ExprKind::Block` and
/// `type_name_of_expr` had no arm for it, so the read failed `karac build`
/// on the loud "cannot resolve field" gap while `karac check` accepted it
/// and the interpreter answered it — the block-receiver sibling of
/// B-2026-08-27-49, one node kind over.
///
/// Both branches are `Block`s compiled through `compile_block_with_frame`,
/// so the tail-type recording that fix added already covers them and the
/// `if` only has to read the THEN branch's entry (the typechecker has
/// already made the branches agree). An `if let`'s then-arm is the
/// exception: it hand-rolls its frame against a plain `compile_block` and
/// so never reaches that recording site, which is why it needs one of its
/// own — line 07 is what holds that.
#[test]
fn test_e2e_field_read_on_an_if_expression_receiver() {
    let src = r#"
struct Tag { n: i64 }
struct Inner { m: i64 }
struct Outer { inner: Inner, k: i64 }

fn make(k: i64) -> Tag { return Tag { n: k }; }
fn make2() -> Outer { return Outer { inner: Inner { m: 3 }, k: 4 }; }

impl Tag { fn doubled(self) -> i64 { return self.n * 2; } }

fn main() {
    let c = true;
    println(if c { make(1) } else { make(2) }.n);
    println(if not c { make(1) } else { make(2) }.n);
    println(if c { make(1) } else { make(2) }.n + 10);
    println(if c { make2() } else { make2() }.inner.m);
    println(if c { make2() } else { make2() }.k);
    println(if c { make(5) } else { make(6) }.doubled());
    let k = if c { make(7) } else { make(8) }.n;
    println(k);
    println(if c { make(1) } else if not c { make(2) } else { make(3) }.n);
    let o: Option[i64] = Option.Some(9);
    println(if let Option.Some(v) = o { Tag { n: v } } else { Tag { n: 0 } }.n);
}
"#;
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(
        interp_errs.is_empty(),
        "interpreter errors: {interp_errs:?}"
    );
    let expected = interp_out.join("");
    // Anti-vacuity. Rows 1 and 2 take OPPOSITE branches, so a receiver that
    // always resolved through the then-branch's value would show up; rows 4
    // and 5 read both fields of a two-field struct, so an off-by-one field
    // index cannot pass by coincidence.
    assert_eq!(
        expected, "1\n2\n11\n3\n4\n10\n7\n1\n9\n",
        "interpreter oracle is wrong for an if-expression receiver",
    );
    let Some(aot) = run_program(src) else { return };
    assert_eq!(
        aot, expected,
        "a compiled field read on an if-expression receiver must match the interpreter",
    );
}

/// A tuple carrying a TYPE PARAMETER — `(T, i64)` at `T = i64` and
/// `T = String` — orders through `.cmp` in a monomorph (B-2026-08-27-41).
///
/// This is the leg that makes codegen's substitution step load-bearing
/// rather than decorative. The receiver's static type is the tuple
/// `(T, i64)`, so it DOES reach the span-keyed operand table — but as
/// written, with `T` unresolved. Running it through the active monomorph
/// substitution is what turns it into `(i64, i64)` / `(String, i64)`;
/// without that step `te_is_totally_ordered` rejects the bare `T` leaf and
/// the call falls out of dispatch entirely (measured: "no handler for
/// method 'cmp' on variable 'a'"). Fails CLOSED, which is why the failure
/// is a build error rather than a comparison against the wrong layout.
///
/// A receiver typed as a BARE `T` (`fn smaller[T: Ord](a: T, b: T)`) is
/// deliberately not here: it has no tuple in its written type at all, so
/// nothing reaches that table, and the mono channel that would carry the
/// binding drops tuple type arguments outright — B-2026-08-27-40, tracked
/// separately. The interpreter answers it today
/// (`tests/interpreter.rs::tuple_cmp_through_a_bare_type_parameter`);
/// both compiled backends still decline it, loudly.
#[test]
fn test_e2e_tuple_cmp_through_a_tuple_of_type_params() {
    let src = r#"
fn pick[T: Ord](a: (T, i64), b: (T, i64)) -> bool { return a.cmp(b).is_lt(); }

fn main() {
    println(f"{pick((1, 2), (1, 3))}");
    println(f"{pick((5, 2), (1, 3))}");
    println(f"{pick(("a", 2), ("b", 0))}");
    println(f"{pick(("b", 2), ("b", 2))}");
}
"#;
    assert_eq!(
        run_program(src).as_deref(),
        // i64 leg less / greater, String leg less, then equal.
        Some("true\nfalse\ntrue\nfalse\n")
    );
}

/// B-2026-08-31-30 — THE `if let` / `let … else` / NESTED-`match` SPELLINGS
/// OF A STRUCT DESTRUCTURE DOUBLE FREED A MOVED-OUT HEAP FIELD.
///
/// `suppress_destructured_struct_pattern_cleanup` (#16) cap-zeroes each
/// moved-out field in the SOURCE struct so the scrutinee's drop skips the
/// buffer the binding now owns. It had exactly one caller — the `match` arm
/// loop. The `if let` / `while let` / `let … else` legs ran the whole
/// neighbouring battery and never this one, so the source field stayed
/// populated while the binding owned the same buffer and both freed it.
/// The NESTED spelling is a second, independent half: it does reach #16,
/// but the `whole_move` gate correctly declines to mask an outer field
/// whose sub-pattern destructures, and nothing then descended to the inner
/// field. #16 now recurses, and `FieldSkipTree::nested` carries the bodies
/// mask through `struct_moved_nested_field_bodies`.
///
/// EVERY ROW PADS ITS PAYLOAD PAST THE SHORT-STRING THRESHOLD AND
/// ALLOCATES AGAIN AFTERWARDS. Without both, the second free lands on a
/// block nothing reclaims, glibc stays quiet and the printed text is
/// correct — the trap that made an earlier version of the B-2026-08-31-23
/// fixture pass against its own reverted fix. With them, each row aborts
/// with `free(): double free detected in tcache 2` on the pre-fix compiler.
///
/// EVERY ROW PINS ONE STRING FOR BOTH BACKENDS, but note what that depends
/// on: this harness builds with auto-par OFF. Under the DEFAULT build the
/// `let … else` row prints `47 47 end dR47` instead — the binding's body
/// slides to scope exit — which is B-2026-08-31-6 (an auto-par outlined
/// region swallows the NLL drop points of the statements it spans), not
/// anything this row fixed. Measured both ways while deriving these values;
/// recorded here so a future reader who runs the same program from the CLI
/// and sees a different order does not re-diagnose it as a regression.
#[test]
fn e2e_every_struct_destructure_spelling_frees_once() {
    const PRELUDE: &str = "fn pad(t: i64) -> String {\n\
             let mut s: String = String.new();\n\
             s.push_str(\"payload-padded-out-well-past-thirty-six-bytes-\");\n\
             s.push_str(f\"{t}\");\n\
             return s;\n\
         }\n\
         struct R { s: String }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.s.len()}\"); } }\n\
         struct H { r: R, n: i64 }\n\
         struct Q { h: H }\n";
    // (label, statements, interpreter text, compiled text)
    let cases: &[(&str, &str, &str, &str)] = &[
        (
            "`if let` over a bare struct",
            "let h: H = H { r: R { s: pad(1) }, n: 4 };\n\
                 if let H { r, .. } = h { println(r.s.len()) }",
            "47\ndR47\n47\nend\n",
            "47\ndR47\n47\nend\n",
        ),
        (
            "`let … else` over a bare struct",
            "let h: H = H { r: R { s: pad(1) }, n: 4 };\n\
                 let H { r, .. } = h else { return };\n\
                 println(r.s.len());",
            "47\ndR47\n47\nend\n",
            "47\ndR47\n47\nend\n",
        ),
        (
            "NESTED struct sub-pattern under `match`",
            "let q: Q = Q { h: H { r: R { s: pad(1) }, n: 4 } };\n\
                 match q { Q { h: H { r, .. } } => println(r.s.len()) }",
            "47\ndR47\n47\nend\n",
            "47\ndR47\n47\nend\n",
        ),
        (
            "NESTED struct sub-pattern under `if let`",
            "let q: Q = Q { h: H { r: R { s: pad(1) }, n: 4 } };\n\
                 if let Q { h: H { r, .. } } = q { println(r.s.len()) }",
            "47\ndR47\n47\nend\n",
            "47\ndR47\n47\nend\n",
        ),
        (
            "control: the flat `match` spelling, correct throughout",
            "let h: H = H { r: R { s: pad(1) }, n: 4 };\n\
                 match h { H { r, .. } => println(r.s.len()) }",
            "47\ndR47\n47\nend\n",
            "47\ndR47\n47\nend\n",
        ),
    ];
    for (label, stmts, want_interp, want_aot) in cases {
        let src = format!(
            "{PRELUDE}fn main() {{\n    {stmts}\n    let extra: String = pad(2);\n    \
                 println(extra.len());\n    println(\"end\");\n}}\n"
        );
        let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(&src);
        assert!(
            interp_errs.is_empty(),
            "{label}: interpreter errored: {interp_errs:?}"
        );
        assert_eq!(interp_out.join(""), *want_interp, "{label}: interpreter");
        if let Some(aot) = run_program(&src) {
            assert_eq!(
                aot, *want_aot,
                "{label}: the source field and the binding must not both free the buffer"
            );
        }
    }
}

/// The stdlib enums that were NEVER affected, kept as the control that
/// isolates the cause. `Stdio` and `PoolError` get their layouts from
/// `compiled_stdlib_programs`; a regression that removed either module
/// from that list would surface here rather than in a user program.
#[test]
fn stdlib_enums_with_module_supplied_layouts_still_match() {
    assert_eq!(
            run_program(
                "fn main() {\n\
                     let a = Stdio.Inherit;\n\
                     let b = Stdio.Null;\n\
                     let c = Stdio.Piped;\n\
                     match a { Stdio.Inherit => println(\"a=Inherit\"), Stdio.Null => println(\"a=Null\"), Stdio.Piped => println(\"a=Piped\") }\n\
                     match b { Stdio.Inherit => println(\"b=Inherit\"), Stdio.Null => println(\"b=Null\"), Stdio.Piped => println(\"b=Piped\") }\n\
                     match c { Stdio.Inherit => println(\"c=Inherit\"), Stdio.Null => println(\"c=Null\"), Stdio.Piped => println(\"c=Piped\") }\n\
                     let p = PoolError.PoolClosed;\n\
                     match p { PoolError.Timeout => println(\"Timeout\"), PoolError.PoolClosed => println(\"PoolClosed\"), _ => println(\"other\") }\n\
                 }"
            ),
            Some("a=Inherit\nb=Null\nc=Piped\nPoolClosed\n".to_string())
        );
}

/// B-2026-08-21-18 — struct functional update `P { x: 1, ..base }`.
///
/// The parser already stored the base and the interpreter already
/// implemented the copy; codegen ignored it, so the form was gated off
/// with `E_STRUCT_LITERAL_SPREAD_UNSUPPORTED`. It is implemented as a
/// DESUGAR to the explicit field copies it stands for — `y: base.y` —
/// rather than a codegen arm, because the row's own account of the risk
/// was that the hard part is not emitting the copy but OWNERSHIP, and
/// `base.y` is a shape every phase already settles correctly. It is also
/// exactly what the gate's diagnostic told users to write by hand, so the
/// feature and the advice cannot disagree.
///
/// Heap fields are the point of the second case: `s` and `v` are MOVED out
/// of the base, which is the double-free/leak shape. `run_program` agrees
/// the interpreter and the compiled backends; the ASAN corpus carries the
/// allocation-balance half separately.
#[test]
fn test_e2e_struct_functional_update() {
    assert_eq!(
        run_program(
            "struct P { x: i64, y: i64, z: i64 }\n\
                 fn main() {\n\
                 let b = P { x: 1, y: 2, z: 3 };\n\
                 let p = P { x: 10, ..b };\n\
                 println(f\"{p.x} {p.y} {p.z}\");\n\
                 }"
        )
        .as_deref(),
        Some("10 2 3\n"),
        "the explicit field wins and every other field comes from the base"
    );
    assert_eq!(
        run_program(
            "struct P { x: i64, s: String, v: Vec[i64] }\n\
                 fn main() {\n\
                 let b = P { x: 1, s: \"hi\", v: vec![7, 8] };\n\
                 let p = P { x: 9, ..b };\n\
                 println(f\"{p.x} {p.s} {p.v.len()}\");\n\
                 }"
        )
        .as_deref(),
        Some("9 hi 2\n"),
        "HEAP fields move out of the base — the shape the row flagged as the risk"
    );
    assert_eq!(
        run_program(
            "struct I { a: i64, b: i64 }\n\
                 struct O { inner: I }\n\
                 fn main() {\n\
                 let o = O { inner: I { a: 1, b: 2 } };\n\
                 let q = I { a: 5, ..o.inner };\n\
                 println(f\"{q.a} {q.b}\");\n\
                 }"
        )
        .as_deref(),
        Some("5 2\n"),
        "a FIELD-PATH base is a place too, so it is re-evaluable and expands"
    );
}

/// B-2026-08-30-28 — the METHOD spelling, storing into `self`, and a store
/// NESTED two branches deep. The method reaches its stand-down by a
/// different route than the free function (B-2026-08-29-11), so it is a
/// genuinely separate leg rather than a restatement.
#[test]
fn e2e_conditional_store_method_and_nested_run_one_body_each() {
    let src = "struct Res { id: i64 }\n\
             impl Drop for Res {\n\
             \x20   fn drop(mut ref self) { println(f\"drop {self.id}\"); }\n\
             }\n\
             struct Bag { xs: Vec[Res] }\n\
             impl Bag {\n\
             \x20   fn add(mut ref self, r: Res) { if r.id > 100 { self.xs.push(r); } }\n\
             }\n\
             fn nested(sink: mut ref Vec[Res], r: Res) { if r.id > 0 { if r.id > 100 { sink.push(r); } } }\n\
             fn main() {\n\
             \x20   let mut b: Bag = Bag { xs: Vec.new() };\n\
             \x20   b.add(Res { id: 5 });\n\
             \x20   println(\"m-miss\");\n\
             \x20   let mut s: Vec[Res] = Vec.new();\n\
             \x20   nested(mut s, Res { id: 6 });\n\
             \x20   println(\"n-miss\");\n\
             \x20   println(f\"b={b.xs.len()} s={s.len()}\");\n\
             }\n"
            .to_string();
    assert_eq!(
        run_program(&src),
        Some("drop 5\nm-miss\ndrop 6\nn-miss\nb=0 s=0\n".to_string()),
        "the method and nested-branch spellings each run one body on the non-storing path"
    );
}

/// B-2026-09-06-7 — the two-step destructure of a nested tuple field off a
/// LOCAL: `let h = H2 { pe: ((mk(9), 1), 2) }; let (inner, y) = h.pe; let (r, x) =
/// inner; let m: R = r;` ran `dR9` TWICE on every compiled backend, one of them
/// BEFORE the live read (`dR9 l3 9 dR9` against the interpreter's `l3 9 dR9`).
/// The early body was the struct's OWN walk, not the leaf's: the tuple-typed leaf
/// arm of `place_source_tuple_leaf_cleanups` handed `inner` the element bodies but
/// never recorded the element in `took_bodies`, so the B-2026-09-02-43 disarm
/// left `h`'s `NestedTuple` walk descending into `pe.0` and running the inner
/// struct's body at `h`'s NLL death, one statement later. The FIRST destructure
/// alone already doubled (`let (inner, y) = h.pe; println(inner.0.id)`); the
/// second step over the leaf (`let (r, x) = inner`) needs nothing of its own —
/// it moves `inner` whole, and the move-out suppression retires its walk before
/// the leaves take over (a disarm added there was measured inert by ablation).
///
/// THE CONTROLS: `flat` (the struct-typed leaf arm, which did record its index),
/// `plainlocal` / `fromcall` / `wholecopy` (a tuple LOCAL source, whose walk does
/// not descend into a nested element) and `param3` (the owned-param source,
/// B-2026-09-02-41) were one body each before and must stay so.
///
/// Twin of `tests/interpreter.rs`'s `test_two_step_nested_tuple_field_destructure_runs_one_body`, pinned to the same string.
#[test]
fn e2e_two_step_nested_tuple_field_destructure_runs_one_body() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
fn consume(x: R) -> i64 { return x.id }
struct H1 { pe: (R, i64) }
struct H2 { pe: ((R, i64), i64) }
struct H3 { pe: (((R, i64), i64), i64) }
fn mkpair() -> (R, i64) { return (mk(11), 1) }

fn local3() { let h: H2 = H2 { pe: ((mk(9), 1), 2) }; let (inner, y) = h.pe; let (r, x) = inner; let m: R = r; println(f"  l3 {m.id}") }
fn norebind() { let h: H2 = H2 { pe: ((mk(7), 1), 2) }; let (inner, y) = h.pe; let (r, x) = inner; println(f"  nr {r.id}") }
fn viacall() { let h: H2 = H2 { pe: ((mk(4), 1), 2) }; let (inner, y) = h.pe; let (r, x) = inner; let d = consume(r); println(f"  vc {d}") }
fn three() { let h: H3 = H3 { pe: (((mk(3), 1), 2), 3) }; let (mid, z) = h.pe; let (inner, y) = mid; let (r, x) = inner; let m: R = r; println(f"  t3 {m.id}") }
fn leftin() { let h: H2 = H2 { pe: ((mk(2), 1), 2) }; let (inner, y) = h.pe; let (r, x) = inner; println(f"  li {x}") }
fn nestedblock() { let h: H2 = H2 { pe: ((mk(13), 1), 2) }; let (inner, y) = h.pe; { let (r, x) = inner; let m: R = r; println(f"  nb {m.id}") } println("  after") }
fn flat() { let h: H1 = H1 { pe: (mk(8), 1) }; let (r, k) = h.pe; let m: R = r; println(f"  fl {m.id}") }
fn plainlocal() { let t: ((R, i64), i64) = ((mk(6), 1), 2); let (inner, y) = t; let (r, x) = inner; let m: R = r; println(f"  pl {m.id}") }
fn fromcall() { let inner = mkpair(); let (r, x) = inner; let m: R = r; println(f"  fc {m.id}") }
fn wholecopy() { let h: H2 = H2 { pe: ((mk(12), 1), 2) }; let t = h.pe; let (inner, y) = t; let (r, x) = inner; let m: R = r; println(f"  wc {m.id}") }
fn param3(h: H2) { let (inner, y) = h.pe; let (r, x) = inner; let m: R = r; println(f"  p3 {m.id}") }

fn main() {
    println("local3"); local3();
    println("norebind"); norebind();
    println("viacall"); viacall();
    println("three"); three();
    println("leftin"); leftin();
    println("nestedblock"); nestedblock();
    println("flat"); flat();
    println("plainlocal"); plainlocal();
    println("fromcall"); fromcall();
    println("wholecopy"); wholecopy();
    println("param3"); param3(H2 { pe: ((mk(5), 1), 2) });
    println("end");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"local3
  l3 9
  dR9
norebind
  nr 7
  dR7
viacall
  dR4
  vc 4
three
  t3 3
  dR3
leftin
  dR2
  li 1
nestedblock
  nb 13
  dR13
  after
flat
  fl 8
  dR8
plainlocal
  pl 6
  dR6
fromcall
  fc 11
  dR11
wholecopy
  wc 12
  dR12
param3
  p3 5
  dR5
end
"#
    );
}

/// B-2026-09-02-26 — A LOCAL TUPLE SCRUTINEE'S ELEMENT REBIND DOUBLES THE
/// `Drop` BODY, AND THE REPAIR IS THE OPPOSITE OF B-2026-08-31-7's.
///
/// `let t = (R { id: 6 }, 0); match t { (r, k) => { let m = r; … } }` ran
/// `dR6` twice on all four surfaces — agreed, and by one-value-one-body
/// agreed-wrong. One `R` is constructed, so one body is due.
///
/// -31-7 fixed the owned-PARAM spelling by making the element a VIEW: the
/// caller retains the value, so its walk stays the single owner and the rebind
/// inherits view-ness. A LOCAL has no caller to hand the body to, so widening
/// that marking here would have produced a body that runs NOWHERE. The repair
/// is the other direction — RETRACT the tuple's element walk for the moved
/// element and let the rebind own it, which is what the enum family already
/// does for a local scrutinee (`e6` below, correct before and after).
///
/// THE CONTROLS ARE THE POINT, because the fix WITHHOLDS a walk and the failure
/// mode of over-reaching is a body that never runs:
/// - `r6` — the same arm without the rebind. Read-only, so nothing is moved
///   out and the walk must stay the single owner. One body before and after.
/// - `s6` — the arm moves the element into a BY-VALUE CALLEE. One body before
///   and after, and the cell that picked the predicate: a bare-tuple element
///   has no owner of its own to transfer FROM, so treating the argument as
///   consuming (`binding_use::binding_only_read_through`, the enum family's
///   test) masked the walk and ran NO body at all — measured on both backends
///   independently. `consume_class::binding_only_borrowed` reads an
///   entry-copied argument as non-consuming and is what both sides now use.
/// - `t6` — a two-element tuple with only the FIRST rebound. `dR56 dR56 dR57`
///   before, `dR56 dR57` after: the mask is per element, not per tuple.
/// - `g6` — a GUARDED match whose consuming arm is NOT the one taken. The
///   codegen mask is static, so masking on ANY arm ran no body here at all;
///   requiring every binding arm to agree leaves this cell exactly as it was.
///   Its `k > 0` sibling still runs two — this row's bug surviving in the
///   mixed-guard cell, which an agreed-and-wrong answer is the documented
///   trade for against a body that vanishes.
/// - `ol` — the tuple OUTLIVES the match, with a statement after it. The
///   codegen retraction RE-REGISTERS the walk rather than swapping it in place,
///   and it runs inside the arm's region, so the untouched element's body could
///   have been pulled into the arm's cleanup frame. It fires at the tuple's own
///   NLL end (before `after match`), identically on all five surfaces.
/// - `e6` — the ENUM spelling of `p6`, correct before and after. It is what
///   shows the tuple family was behind the enum family rather than that a new
///   rule was invented.
///
/// `h6` and `l4` are the two shapes the fix reaches beyond the row's own:
/// a HEAP-carrying element (`dHx dHx` before — a genuine double free, which is
/// the severity question the row left open and `tests/memory_sanitizer.rs`'s
/// `asan_local_tuple_elem_rebind_frees_once` now pins), and an arm that hands
/// the element OUT of the function (`dR74` twice before). The owned-PARAM
/// escape and the `let (r, k) = t` destructure are NOT fixed here — they are
/// B-2026-09-02-24 and -25, whose machinery is elsewhere.
///
/// Twin of `tests/interpreter.rs`'s `test_local_tuple_elem_rebind_runs_one_body`, pinned to the same string.
#[test]
fn e2e_local_tuple_elem_rebind_runs_one_body() {
    let Some(out) = run_program(
        r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct H { s: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.s}") } }
enum E { A(R), B }

fn sink(r: R) -> i64 { r.id }

fn p6()  { let t = (R { id: 6 }, 0); match t { (r, k) => { let m = r; println(f"  b{m.id}"); } } }
fn e6()  { let t = E.A(R { id: 16 }); match t { E.A(r) => { let m = r; println(f"  b{m.id}"); } E.B => {} } }
fn r6()  { let t = (R { id: 26 }, 0); match t { (r, k) => { println(f"  b{r.id}"); } } }
fn s6()  { let t = (R { id: 36 }, 0); match t { (r, k) => { println(f"  b{sink(r)}"); } } }
fn t6()  { let t = (R { id: 56 }, R { id: 57 }); match t { (r, q) => { let m = r; println(f"  b{m.id}"); } } }
fn h6()  { let t = (H { s: "x" }, 0); match t { (h, k) => { let m = h; println(f"  b{m.s}"); } } }
fn l4() -> R { let t = (R { id: 74 }, 0); match t { (r, k) => { r } } }
fn ol()  {
    let t = (R { id: 91 }, R { id: 92 });
    match t { (r, q) => { let m = r; println(f"  mv{m.id}") } }
    println("  after match");
}
fn g6(n: i64) {
    let t = (R { id: 86 }, n);
    match t {
        (r, k) if k > 0 => { let m = r; println(f"  mv{m.id}"); }
        (r, k) => { println(f"  rd{r.id}"); }
    }
}

fn main() {
    println("p6"); p6(); println("p6 end");
    println("e6"); e6(); println("e6 end");
    println("r6"); r6(); println("r6 end");
    println("s6"); s6(); println("s6 end");
    println("t6"); t6(); println("t6 end");
    println("h6"); h6(); println("h6 end");
    println("l4"); let q = l4(); println(f"  got{q.id}"); println("l4 end");
    println("ol"); ol(); println("ol end");
    println("g6"); g6(0); println("g6 end");
    println("done");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"p6
  b6
dR6
p6 end
e6
  b16
dR16
e6 end
r6
  b26
dR26
r6 end
s6
  b36
dR36
s6 end
t6
  b56
dR56
dR57
t6 end
h6
  bx
dHx
h6 end
l4
  got74
dR74
l4 end
ol
  mv91
dR91
dR92
  after match
ol end
g6
  rd86
dR86
g6 end
done
"#
    );
}

/// B-2026-09-05-36 — a `let`-destructured tuple element or struct field,
/// or a bare by-value parameter, handed to a callee that RETURNS or STORES
/// it has one owner on every surface. Two predicates were intraprocedural
/// where the escape is interprocedural: the part channel
/// (`fn_returns_param_part_paths`) classified only a returned expression
/// that denotes the part, so `let (r, k) = t; wrap(r)` reported nothing
/// and the caller's element walk fired beside the result's owner; the
/// store channel (`fn_moves_param_into_outliving_place`) treated a free
/// function call as opaque, so `fn b_stash(x, v) { stash(x, v) }` kept the
/// caller's temp drop beside the container's drain. Both now have a
/// program-aware sibling (`fn_escaping_param_part_paths`,
/// `fn_moves_param_into_outliving_place_via_call`; one level, argument
/// bare) consulted by the caller-side gates on both backends.
///
/// `one`/`two` forward through `wrap` (tail and explicit `return`),
/// `three` stashes, `four` consumes (must stay at one body inside the
/// call), `five` forwards into a returned struct literal, `six`/`seven`
/// the struct-field destructure, `eight`..`eleven` the bare-param
/// spellings, `thirteen`..`sixteen` named-local arguments. The two-hop
/// chain (`b_stash2` → `b_stash` → `stash`) is deliberately absent: the
/// family's one-level rule leaves it at two bodies, recorded on the row.
#[test]
fn e2e_destructured_part_or_bare_param_handed_to_a_taking_callee_has_one_owner() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct W { r: R, n: i64 }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
fn consume(x: R) -> i64 { return x.id }
fn wrap(x: R) -> R { return x }
fn stash(x: R, v: mut ref Vec[R]) { v.push(x) }
fn wrapw(x: R) -> W { return W { r: x, n: 1 } }
fn t_fwd_let(t: (R, i64)) -> R { let (r, k) = t; wrap(r) }
fn t_fwd_let_ret(t: (R, i64)) -> R { let (r, k) = t; return wrap(r); }
fn t_stash_let(t: (R, i64), v: mut ref Vec[R]) -> i64 { let (r, k) = t; stash(r, v); k }
fn t_consume_let(t: (R, i64)) -> i64 { let (r, k) = t; consume(r) }
fn t_fwdw_let(t: (R, i64)) -> W { let (r, k) = t; wrapw(r) }
fn s_fwd_let(w: W) -> R { let W { r, n } = w; wrap(r) }
fn s_stash_let(w: W, v: mut ref Vec[R]) -> i64 { let W { r, n } = w; stash(r, v); n }
fn b_stash(x: R, v: mut ref Vec[R]) { stash(x, v) }
fn b_stash_ret(x: R, v: mut ref Vec[R]) -> i64 { stash(x, v); 7 }
fn b_fwd(x: R) -> R { wrap(x) }
fn b_consume(x: R) -> i64 { consume(x) }
fn main() {
    { let a: R = t_fwd_let((mk(1), 0)); println(f"r{a.id}"); println("one") }
    { let a: R = t_fwd_let_ret((mk(2), 0)); println(f"r{a.id}"); println("two") }
    { let mut v: Vec[R] = []; let d: i64 = t_stash_let((mk(3), 0), mut v); println(f"r{d} n{v.len()}"); println("three") }
    { let d: i64 = t_consume_let((mk(4), 0)); println(f"r{d}"); println("four") }
    { let w: W = t_fwdw_let((mk(5), 0)); println(f"r{w.r.id}"); println("five") }
    { let a: R = s_fwd_let(W { r: mk(6), n: 1 }); println(f"r{a.id}"); println("six") }
    { let mut v: Vec[R] = []; let d: i64 = s_stash_let(W { r: mk(7), n: 1 }, mut v); println(f"r{d} n{v.len()}"); println("seven") }
    { let mut v: Vec[R] = []; b_stash(mk(8), mut v); println(f"n{v.len()}"); println("eight") }
    { let mut v: Vec[R] = []; let d: i64 = b_stash_ret(mk(9), mut v); println(f"r{d} n{v.len()}"); println("nine") }
    { let a: R = b_fwd(mk(10)); println(f"r{a.id}"); println("ten") }
    { let d: i64 = b_consume(mk(11)); println(f"r{d}"); println("eleven") }
    { let t: (R, i64) = (mk(13), 0); let a: R = t_fwd_let(t); println(f"r{a.id}"); println("thirteen") }
    { let t: (R, i64) = (mk(14), 0); let mut v: Vec[R] = []; let d: i64 = t_stash_let(t, mut v); println(f"r{d} n{v.len()}"); println("fourteen") }
    { let x: R = mk(15); let mut v: Vec[R] = []; b_stash(x, mut v); println(f"n{v.len()}"); println("fifteen") }
    { let w: W = W { r: mk(16), n: 1 }; let a: R = s_fwd_let(w); println(f"r{a.id}"); println("sixteen") }
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "r1\ndR1\none\nr2\ndR2\ntwo\nr0 n1\ndR3\nthree\ndR4\nr4\nfour\nr5\ndR5\nfive\nr6\ndR6\nsix\nr1 n1\ndR7\nseven\nn1\ndR8\neight\nr7 n1\ndR9\nnine\nr10\ndR10\nten\ndR11\nr11\neleven\nr13\ndR13\nthirteen\nr0 n1\ndR14\nfourteen\nn1\ndR15\nfifteen\nr16\ndR16\nsixteen\nend\n");
}

/// B-2026-09-05-17 — a place struct (or tuple) argument whose part is
/// handed back THROUGH a forwarding call runs that part's `Drop` body
/// once. `fn fwd(g: Cd) -> R { return cEsc(g); }` over `fn cEsc(h: Cd) -> R
/// { let Cd { r, z } = h; r }` printed `in dR61 got61 dR61` on every
/// surface: the part channel classified only a return site that DENOTES
/// the part, so a forwarded call reported nothing and `g`'s own field walk
/// fired beside the result's owner. The program-aware part scan
/// (`fn_escaping_param_part_paths`) now composes the callee's own answer
/// under the argument's prefix — the shape `fn_returns_param_via_call`
/// gives the whole-param channel — for a call on the body's TOP LEVEL
/// (tail, `return`, `let`, inside a returned aggregate literal), with a
/// cycle guard for a recursive forward. A CONDITIONAL forward is left
/// unreported on purpose: reporting it would trade this double for a lost
/// body on the path that does not forward.
///
/// `one` is the row's cell, `two` the direct call, `three`/`four` the tail
/// and projection spellings, `five` a two-hop forward, `six` the tuple
/// sibling, `seven` a forward inside a returned struct literal, `eight`
/// the conditional forward's NOT-taken path (one body, unchanged), `nine`/
/// `ten` `let`-bound forwards (`ten` dies inside the frame), `eleven` a
/// fresh-temp argument, `twelve` the METHOD path, which also needed the
/// struct sibling of the place-argument disarm on the method arg loop.
#[test]
fn e2e_forwarded_place_struct_arg_field_handed_back_has_one_owner() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
struct Cd { r: R, z: i64 }
struct W { r: R, n: i64 }
fn cEsc(h: Cd) -> R { let Cd { r, z } = h; println("in"); return r; }
fn cProj(h: Cd) -> R { return h.r; }
fn tEsc(t: (R, i64)) -> R { let (r, k) = t; return r; }
fn fwd(g: Cd) -> R { return cEsc(g); }
fn fwd_tail(g: Cd) -> R { cEsc(g) }
fn fwd_proj(g: Cd) -> R { return cProj(g); }
fn fwd2(g: Cd) -> R { return fwd(g); }
fn fwd_t(t: (R, i64)) -> R { return tEsc(t); }
fn fwd_wrap(g: Cd) -> W { return W { r: cEsc(g), n: 1 }; }
fn fwd_cond(g: Cd, c: bool) -> R { if c { return cEsc(g); } return mk(99); }
fn fwd_let(g: Cd) -> R { let x: R = cEsc(g); return x; }
fn fwd_i(g: Cd) -> i64 { let x: R = cEsc(g); return x.id; }
struct H { n: i64 }
impl H { fn m_fwd(ref self, g: Cd) -> R { return cEsc(g); } }
fn main() {
    let h: H = H { n: 1 };
    { let g: Cd = Cd { r: mk(61), z: 9 }; let out: R = fwd(g); println(f"got{out.id}"); println("one") }
    { let g: Cd = Cd { r: mk(62), z: 9 }; let out: R = cEsc(g); println(f"got{out.id}"); println("two") }
    { let g: Cd = Cd { r: mk(63), z: 9 }; let out: R = fwd_tail(g); println(f"got{out.id}"); println("three") }
    { let g: Cd = Cd { r: mk(64), z: 9 }; let out: R = fwd_proj(g); println(f"got{out.id}"); println("four") }
    { let g: Cd = Cd { r: mk(65), z: 9 }; let out: R = fwd2(g); println(f"got{out.id}"); println("five") }
    { let t: (R, i64) = (mk(66), 0); let out: R = fwd_t(t); println(f"got{out.id}"); println("six") }
    { let g: Cd = Cd { r: mk(67), z: 9 }; let out: W = fwd_wrap(g); println(f"got{out.r.id}"); println("seven") }
    { let g: Cd = Cd { r: mk(69), z: 9 }; let out: R = fwd_cond(g, false); println(f"got{out.id}"); println("nine") }
    { let g: Cd = Cd { r: mk(70), z: 9 }; let out: R = fwd_let(g); println(f"got{out.id}"); println("ten") }
    { let g: Cd = Cd { r: mk(71), z: 9 }; let d: i64 = fwd_i(g); println(f"got{d}"); println("eleven") }
    { let out: R = fwd(Cd { r: mk(72), z: 9 }); println(f"got{out.id}"); println("twelve") }
    { let g: Cd = Cd { r: mk(73), z: 9 }; let out: R = h.m_fwd(g); println(f"got{out.id}"); println("thirteen") }
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "in\ngot61\ndR61\none\nin\ngot62\ndR62\ntwo\nin\ngot63\ndR63\nthree\ngot64\ndR64\nfour\nin\ngot65\ndR65\nfive\ngot66\ndR66\nsix\nin\ngot67\ndR67\nseven\ndR69\ngot99\ndR99\nnine\nin\ngot70\ndR70\nten\nin\ndR71\ngot71\neleven\nin\ngot72\ndR72\ntwelve\nin\ngot73\ndR73\nthirteen\nend\n");
}

/// B-2026-09-03-4 — a destructured element or field returned WRAPPED in
/// an enum constructor (`let (r, k) = t; Option.Some(r)`, `Result.Ok(r)`,
/// a user variant) runs its `Drop` body once, at the caller's binding.
/// The part channel's return-site walk (`yielded`) descended into a
/// struct literal and a tuple literal but not into a constructor CALL, so
/// the `let` spellings reported nothing and the caller's element walk
/// fired beside the result's owner (`dR2 dR2 got`); the `match` spellings
/// were already right through the tuple-arm predicate, whose call rule
/// counts a constructor. A call whose callee is a path (not a plain
/// identifier) now yields its operands' parts.
///
/// The row's expectation that the body fires "at the caller's binding"
/// is met at that binding's NLL death: `g` is not read after its `let`,
/// so its payload body runs before `got` (one body, the design's
/// per-statement liveness), and `sixteen` reads the payload first to show
/// the value is alive until then. `one`/`six` the match spellings,
/// `two`..`five` the `let` spellings over `Option` / explicit `return` /
/// `Result` / a user enum, `seven` the struct-literal wrap (always right),
/// `eight`/`fifteen` a struct source, `nine` a LOCAL source, `ten` the
/// bare param, `eleven` a two-`Drop` tuple (the unreturned sibling still
/// fires), `thirteen`/`fourteen` named-local arguments.
#[test]
fn e2e_destructured_part_returned_in_a_constructor_has_one_owner() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
enum E { A(R), B(i64) }
struct W { r: R, n: i64 }
struct Cd { r: R, z: i64 }
fn f_m(t: (R, i64)) -> Option[R] { match t { (r, k) => { Option.Some(r) } } }
fn f_l(t: (R, i64)) -> Option[R] { let (r, k) = t; Option.Some(r) }
fn f_lr(t: (R, i64)) -> Option[R] { let (r, k) = t; return Option.Some(r); }
fn f_res(t: (R, i64)) -> Result[R, i64] { let (r, k) = t; Result.Ok(r) }
fn f_e(t: (R, i64)) -> E { let (r, k) = t; E.A(r) }
fn f_em(t: (R, i64)) -> E { match t { (r, k) => { E.A(r) } } }
fn f_w(t: (R, i64)) -> W { let (r, k) = t; W { r: r, n: k } }
fn f_s(g: Cd) -> Option[R] { let Cd { r, z } = g; Option.Some(r) }
fn f_sm(g: Cd) -> Option[R] { match g { Cd { r, z } => { Option.Some(r) } } }
fn f_local() -> Option[R] { let t: (R, i64) = (mk(9), 0); let (r, k) = t; Option.Some(r) }
fn f_bare(x: R) -> Option[R] { Option.Some(x) }
fn f_two(t: (R, R)) -> Option[R] { let (a, b) = t; Option.Some(a) }
fn main() {
    { let g: Option[R] = f_m((mk(1), 0)); println("got"); println("one") }
    { let g: Option[R] = f_l((mk(2), 0)); println("got"); println("two") }
    { let g: Option[R] = f_lr((mk(3), 0)); println("got"); println("three") }
    { let g: Result[R, i64] = f_res((mk(4), 0)); println("got"); println("four") }
    { let g: E = f_e((mk(5), 0)); println("got"); println("five") }
    { let g: E = f_em((mk(6), 0)); println("got"); println("six") }
    { let g: W = f_w((mk(7), 0)); println(f"got{g.r.id}"); println("seven") }
    { let g: Option[R] = f_s(Cd { r: mk(8), z: 1 }); println("got"); println("eight") }
    { let g: Option[R] = f_local(); println("got"); println("nine") }
    { let g: Option[R] = f_bare(mk(10)); println("got"); println("ten") }
    { let g: Option[R] = f_two((mk(11), mk(12))); println("got"); println("eleven") }
    { let t: (R, i64) = (mk(13), 0); let g: Option[R] = f_m(t); println("got"); println("thirteen") }
    { let t: (R, i64) = (mk(14), 0); let g: Option[R] = f_l(t); println("got"); println("fourteen") }
    { let g: Option[R] = f_sm(Cd { r: mk(15), z: 1 }); println("got"); println("fifteen") }
    { let g: Option[R] = f_m((mk(16), 0)); match g { Option.Some(x) => { println(f"x{x.id}") }, Option.None => { println("none") } } println("sixteen") }
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "dR1\ngot\none\ndR2\ngot\ntwo\ndR3\ngot\nthree\ndR4\ngot\nfour\ndR5\ngot\nfive\ndR6\ngot\nsix\ngot7\ndR7\nseven\ndR8\ngot\neight\ndR9\ngot\nnine\ndR10\ngot\nten\ndR12\ndR11\ngot\neleven\ndR13\ngot\nthirteen\ndR14\ngot\nfourteen\ndR15\ngot\nfifteen\nx16\ndR16\nsixteen\nend\n");
}

/// B-2026-09-02-41 — the two-step destructure of a nested tuple field
/// reached through an owned struct param (`let (inner, y) = h.pe; let (r,
/// x) = inner; let m = r;`) runs the leaf's `Drop` body once and frees it
/// once. `place_source_tuple_leaf_cleanups`' tuple-typed leaf recorded
/// NOTHING under an owner-runs-bodies source — not the view (so the
/// second destructure treated `inner` as an ordinary local and gave `r` a
/// body of its own beside the caller's walk), not a memory drop, and not
/// the source's cap zeroing (so the source's drop and the value `r` was
/// moved into both freed the same buffers: two invalid frees at -O0, a
/// glibc abort on the JIT). It now mirrors the struct-typed leaf: view
/// marked, memory registered, source zeroed; bodies only when it owns
/// them.
///
/// `one`..`three` the row's shape by rebind / read / unread, `five` the
/// single destructure, `seven`/`eight` the flat sibling (always right),
/// `ten` a three-level nesting, `eleven` the named-local argument. Not
/// here, filed separately: the nested element RETURNED (`pe.0.0` is two
/// tuple levels deep, one more than the skip tree expresses), the rebind
/// of the tuple view (`let z = inner`), and the LOCAL-source two-step,
/// each still double on the compiled backends.
#[test]
fn e2e_two_step_destructure_of_a_nested_tuple_field_has_one_owner() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
struct H2 { pe: ((R, i64), i64) }
struct H1 { pe: (R, i64) }
struct H3 { pe: (((R, i64), i64), i64) }
fn v3(h: H2) { let (inner, y) = h.pe; let (r, x) = inner; let m: R = r; println(f"v3 {m.id}") }
fn v3_read(h: H2) { let (inner, y) = h.pe; let (r, x) = inner; println(f"v3r {r.id}") }
fn v3_unread(h: H2) { let (inner, y) = h.pe; let (r, x) = inner; println("v3u") }
fn v3_ret(h: H2) -> R { let (inner, y) = h.pe; let (r, x) = inner; return r; }
fn v3_one(h: H2) { let (inner, y) = h.pe; println("v3o") }
fn v3_inner_move(h: H2) { let (inner, y) = h.pe; let z: (R, i64) = inner; println(f"v3m {z.0.id}") }
fn flat(h: H1) { let (r, k) = h.pe; let m: R = r; println(f"flat {m.id}") }
fn flat_read(h: H1) { let (r, k) = h.pe; println(f"flatr {r.id}") }
fn deep(h: H3) { let (mid, a) = h.pe; let (inner, b) = mid; let (r, c) = inner; let m: R = r; println(f"deep {m.id}") }
fn local3() { let h: H2 = H2 { pe: ((mk(9), 1), 2) }; let (inner, y) = h.pe; let (r, x) = inner; let m: R = r; println(f"l3 {m.id}") }
fn main() {
    { v3(H2 { pe: ((mk(1), 1), 2) }); println("one") }
    { v3_read(H2 { pe: ((mk(2), 1), 2) }); println("two") }
    { v3_unread(H2 { pe: ((mk(3), 1), 2) }); println("three") }
    { v3_one(H2 { pe: ((mk(5), 1), 2) }); println("five") }
    { flat(H1 { pe: (mk(7), 1) }); println("seven") }
    { flat_read(H1 { pe: (mk(8), 1) }); println("eight") }
    { deep(H3 { pe: (((mk(10), 1), 2), 3) }); println("ten") }
    { let h: H2 = H2 { pe: ((mk(11), 1), 2) }; v3(h); println("eleven") }
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "v3 1\ndR1\none\nv3r 2\ndR2\ntwo\nv3u\ndR3\nthree\nv3o\ndR5\nfive\nflat 7\ndR7\nseven\nflatr 8\ndR8\neight\ndeep 10\ndR10\nten\nv3 11\ndR11\neleven\nend\n");
}

/// B-2026-09-06-6 — a WHOLE rebind of a tuple-typed param VIEW
/// (`let z: (R, i64) = inner` after `let (inner, y) = h.pe`, or
/// `let z = t` over a bare `t: (R, i64)` param) runs the element's user
/// `Drop` body ONCE. The struct spelling (`let h2 = h`) had inherited
/// view-ness at the `let` site since B-2026-08-01-15; the tuple spelling
/// re-armed a full element-bodies walker from the inherited element
/// types, so the body fired at `z`'s death AND in the caller's walk —
/// `v3m 1 dR1 dR1` on jit / aot / AUTO_PAR=0 against the interpreter's
/// `v3m 1 dR1`, and a rebind of the rebind fired it a third time. The
/// rebind is now a view too: memory registered, no bodies, and the mark
/// propagates so a later projection / destructure / handoff of `z`
/// takes the param gates a direct `t` does.
///
/// `one`..`five` the row's shape by read / unread / destructure-of-the-
/// rebind / rebind-of-the-rebind / returned; `six` the flat projection
/// (always right); `seven` and `eleven`..`fourteen` the bare tuple
/// param; `eight` and `fifteen` the named-local argument; `nine` the
/// un-annotated rebind; `ten` / `twelve` the rebind handed to a callee.
/// Not here, filed separately: `let x: R = z.0` leaks `x`'s interior at
/// -O0 in the DIRECT spelling too, and `let w = keep(z)` over a callee
/// that returns its param runs the body twice on all four surfaces.
#[test]
fn e2e_whole_rebind_of_a_tuple_param_view_runs_one_body() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
struct H2 { pe: ((R, i64), i64) }
struct H1 { pe: (R, i64) }
fn take(t: (R, i64)) { println(f"take {t.0.id}") }
fn v3m(h: H2) { let (inner, y) = h.pe; let z: (R, i64) = inner; println(f"v3m {z.0.id}") }
fn v3m_unread(h: H2) { let (inner, y) = h.pe; let z: (R, i64) = inner; println("v3mu") }
fn v3m_destr(h: H2) { let (inner, y) = h.pe; let z: (R, i64) = inner; let (r, x) = z; println(f"v3md {r.id}") }
fn v3m_twice(h: H2) { let (inner, y) = h.pe; let z: (R, i64) = inner; let w: (R, i64) = z; println(f"v3mt {w.0.id}") }
fn v3m_ret(h: H2) -> (R, i64) { let (inner, y) = h.pe; let z: (R, i64) = inner; return z; }
fn v3m_untyped(h: H2) { let (inner, y) = h.pe; let z = inner; println(f"vu {z.0.id}") }
fn v3m_take(h: H2) { let (inner, y) = h.pe; let z: (R, i64) = inner; take(z); println("vt") }
fn flat_m(h: H1) { let p: (R, i64) = h.pe; println(f"fm {p.0.id}") }
fn t_m(t: (R, i64)) { let z: (R, i64) = t; println(f"tm {z.0.id}") }
fn t_destr(t: (R, i64)) { let z: (R, i64) = t; let (r, n) = z; println(f"td {r.id} {n}") }
fn t_take(t: (R, i64)) { let z: (R, i64) = t; take(z); println("tt") }
fn t_ret(t: (R, i64)) -> (R, i64) { let z: (R, i64) = t; return z; }
fn t_untyped(t: (R, i64)) { let z = t; println(f"tu {z.0.id}") }
fn main() {
    { v3m(H2 { pe: ((mk(1), 1), 2) }); println("one") }
    { v3m_unread(H2 { pe: ((mk(2), 1), 2) }); println("two") }
    { v3m_destr(H2 { pe: ((mk(3), 1), 2) }); println("three") }
    { v3m_twice(H2 { pe: ((mk(4), 1), 2) }); println("four") }
    { let a: (R, i64) = v3m_ret(H2 { pe: ((mk(5), 1), 2) }); println(f"got{a.0.id}"); println("five") }
    { flat_m(H1 { pe: (mk(6), 1) }); println("six") }
    { t_m((mk(7), 1)); println("seven") }
    { let h: H2 = H2 { pe: ((mk(8), 1), 2) }; v3m(h); println("eight") }
    { v3m_untyped(H2 { pe: ((mk(9), 1), 2) }); println("nine") }
    { v3m_take(H2 { pe: ((mk(10), 1), 2) }); println("ten") }
    { t_destr((mk(11), 11)); println("eleven") }
    { t_take((mk(12), 12)); println("twelve") }
    { let a: (R, i64) = t_ret((mk(13), 13)); println(f"got{a.0.id}"); println("thirteen") }
    { t_untyped((mk(14), 14)); println("fourteen") }
    { let t: (R, i64) = (mk(15), 15); t_m(t); println("fifteen") }
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "v3m 1\ndR1\none\nv3mu\ndR2\ntwo\nv3md 3\ndR3\nthree\nv3mt 4\ndR4\nfour\ngot5\ndR5\nfive\nfm 6\ndR6\nsix\ntm 7\ndR7\nseven\nv3m 8\ndR8\neight\nvu 9\ndR9\nnine\ntake 10\nvt\ndR10\nten\ntd 11 11\ndR11\neleven\ntake 12\ntt\ndR12\ntwelve\ngot13\ndR13\nthirteen\ntu 14\ndR14\nfourteen\ntm 15\ndR15\nfifteen\nend\n");
}

/// B-2026-09-06-5 — a nested tuple element handed back TWO tuple levels
/// deep (`fn v3_ret(h: H2) -> R { let (inner, y) = h.pe; let (r, x) =
/// inner; return r; }` over `H2 { pe: ((R, i64), i64) }`) runs its user
/// `Drop` body ONCE. The part channel reported the escape correctly
/// (`[Field("pe"), TupleIndex(0), TupleIndex(0)]`); the caller-side skip
/// tree could express a tuple index only ONE level inside a field, so
/// the deeper path was dropped and the caller's field walk fired the
/// body beside the result's owner — `dR1 got1 dR1` on jit / aot /
/// AUTO_PAR=0 against the interpreter's `got1 dR1`. `insert_skip_path`
/// now recurses through `TupleIndex` parts the way it recurses through
/// `Field` parts, and the tuple bodies walker consumes the resulting
/// per-element subtree (`emit_tuple_elem_user_drop_bodies_fn_tree`).
///
/// `one` the row's shape, `two` the direct projection `return
/// h.pe.0.0`, `three` through a rebound inner tuple, `four` three levels
/// deep, `five`/`six` a two-`R` inner tuple handing back either element
/// (the sibling's body stays the callee's), `seven` a struct field
/// below the tuple levels, `eight` the one-level shape that was already
/// right. Not here, filed separately: the NAMED-LOCAL argument
/// (`let h = H2 {..}; v3_ret(h)`) doubles on all four surfaces, at one
/// level too.
#[test]
fn e2e_nested_tuple_element_handed_back_two_levels_deep_runs_one_body() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
struct H2 { pe: ((R, i64), i64) }
struct H3 { pe: (((R, i64), i64), i64) }
struct H2b { pe: ((R, R), i64) }
struct S { r: R, n: i64 }
struct H4 { pe: ((S, i64), i64) }
fn v3_ret(h: H2) -> R { let (inner, y) = h.pe; let (r, x) = inner; return r; }
fn v3_ret_direct(h: H2) -> R { return h.pe.0.0; }
fn v3_ret_z(h: H2) -> R { let z: (R, i64) = h.pe.0; let (r, x) = z; return r; }
fn v4_ret(h: H3) -> R { let (mid, a) = h.pe; let (inner, b) = mid; let (r, c) = inner; return r; }
fn vb_ret0(h: H2b) -> R { let (inner, y) = h.pe; let (r, s) = inner; return r; }
fn vb_ret1(h: H2b) -> R { let (inner, y) = h.pe; let (r, s) = inner; return s; }
fn vs_ret(h: H4) -> R { let (inner, y) = h.pe; let (s, x) = inner; let S { r, n } = s; return r; }
fn v3_ret_tuple(h: H2) -> (R, i64) { let (inner, y) = h.pe; return inner; }
fn main() {
    { let a: R = v3_ret(H2 { pe: ((mk(1), 1), 2) }); println(f"got{a.id}"); println("one") }
    { let a: R = v3_ret_direct(H2 { pe: ((mk(2), 1), 2) }); println(f"got{a.id}"); println("two") }
    { let a: R = v3_ret_z(H2 { pe: ((mk(3), 1), 2) }); println(f"got{a.id}"); println("three") }
    { let a: R = v4_ret(H3 { pe: (((mk(4), 1), 2), 3) }); println(f"got{a.id}"); println("four") }
    { let a: R = vb_ret0(H2b { pe: ((mk(5), mk(6)), 2) }); println(f"got{a.id}"); println("five") }
    { let a: R = vb_ret1(H2b { pe: ((mk(7), mk(8)), 2) }); println(f"got{a.id}"); println("six") }
    { let a: R = vs_ret(H4 { pe: ((S { r: mk(9), n: 1 }, 1), 2) }); println(f"got{a.id}"); println("seven") }
    { let a: (R, i64) = v3_ret_tuple(H2 { pe: ((mk(10), 1), 2) }); println(f"got{a.0.id}"); println("eight") }
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "got1\ndR1\none\ngot2\ndR2\ntwo\ngot3\ndR3\nthree\ngot4\ndR4\nfour\ndR6\ngot5\ndR5\nfive\ndR7\ngot8\ndR8\nsix\ngot9\ndR9\nseven\ngot10\ndR10\neight\nend\n");
}

/// B-2026-09-06-10 — a NAMED local handed to a callee that returns a
/// part nested below one of its fields (`let h = H1 { pe: (mk(1), 1) };
/// let a = flat_ret(h);` over `fn flat_ret(h: H1) -> R { let (r, k) =
/// h.pe; return r; }`) runs that part's `Drop` body ONCE. The named
/// gate (`disarm_escaping_place_struct_field_bodies`) kept only
/// one-field paths, so the local's own walk stayed armed over `pe.0` and
/// fired it at the local's live-range end beside the result's owner —
/// `dR1 got1 dR1` on all four surfaces (the interpreter's identifier arm
/// had the same one-level key), where the fresh-temp spelling of the
/// same call was already right. Deeper paths now resolve to index hops
/// (`param_path_index_hops`) and land in the path-keyed mask that the
/// skip tree reads at any depth.
///
/// `one`..`three` the three shapes (one tuple level, two tuple levels,
/// one struct level) as named locals; `four`/`five` the fresh-temp
/// controls; `six` the local's NLL point right after the call; `seven`
/// the local kept LIVE past the call; `eight` the projection spelling;
/// `nine` a two-step struct destructure; `ten` a two-`R` inner tuple
/// handing back element 1 (the sibling's body stays the callee's). Not
/// here, filed separately: the TUPLE-argument shapes at the same depth
/// (named local and fresh temp alike) still double on all four surfaces.
#[test]
fn e2e_named_local_handing_back_a_nested_part_runs_one_body() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
struct H1 { pe: (R, i64) }
struct H2 { pe: ((R, i64), i64) }
struct S { r: R, n: i64 }
struct G { s: S, n: i64 }
struct H2b { pe: ((R, R), i64) }
fn flat_ret(h: H1) -> R { let (r, k) = h.pe; return r; }
fn flat_proj(h: H1) -> R { return h.pe.0; }
fn v3_ret(h: H2) -> R { let (inner, y) = h.pe; let (r, x) = inner; return r; }
fn g_ret(g: G) -> R { return g.s.r; }
fn g_destr(g: G) -> R { let G { s, n } = g; let S { r, n: m } = s; return r; }
fn vb_ret1(h: H2b) -> R { let (inner, y) = h.pe; let (r, s) = inner; return s; }
fn flat_in(h: H1) -> R { println("in"); let (r, k) = h.pe; println("mid"); return r; }
fn main() {
    { let h: H1 = H1 { pe: (mk(1), 1) }; let a: R = flat_ret(h); println(f"got{a.id}"); println("one") }
    { let h: H2 = H2 { pe: ((mk(2), 1), 2) }; let a: R = v3_ret(h); println(f"got{a.id}"); println("two") }
    { let g: G = G { s: S { r: mk(3), n: 1 }, n: 2 }; let a: R = g_ret(g); println(f"got{a.id}"); println("three") }
    { let a: R = flat_ret(H1 { pe: (mk(4), 1) }); println(f"got{a.id}"); println("four") }
    { let a: R = g_ret(G { s: S { r: mk(5), n: 1 }, n: 2 }); println(f"got{a.id}"); println("five") }
    { let h: H1 = H1 { pe: (mk(6), 1) }; let a: R = flat_in(h); println("out"); println(f"got{a.id}"); println("six") }
    { let h: H1 = H1 { pe: (mk(7), 1) }; let a: R = flat_in(h); println("out"); println(f"got{a.id}"); println(f"h{h.pe.1}"); println("seven") }
    { let h: H1 = H1 { pe: (mk(8), 1) }; let a: R = flat_proj(h); println(f"got{a.id}"); println("eight") }
    { let g: G = G { s: S { r: mk(9), n: 1 }, n: 2 }; let a: R = g_destr(g); println(f"got{a.id}"); println("nine") }
    { let h: H2b = H2b { pe: ((mk(10), mk(11)), 2) }; let a: R = vb_ret1(h); println(f"got{a.id}"); println("ten") }
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "got1\ndR1\none\ngot2\ndR2\ntwo\ngot3\ndR3\nthree\ngot4\ndR4\nfour\ngot5\ndR5\nfive\nin\nmid\nout\ngot6\ndR6\nsix\nin\nmid\nout\ngot7\ndR7\nh1\nseven\ngot8\ndR8\neight\ngot9\ndR9\nnine\ndR10\ngot11\ndR11\nten\nend\n");
}

/// B-2026-09-06-11 — a TUPLE argument whose callee hands back a part
/// BELOW its top-level elements (`fn tv_ret(t: ((R, i64), i64)) -> R {
/// let (inner, y) = t; let (r, x) = inner; return r; }`) runs that
/// part's `Drop` body ONCE, for a named local and a fresh tuple literal
/// alike. Every tuple-argument mask was a flat top-level element index
/// (`tuple_indices_of`, `disarm_tuple_elem_bodies_at`), so the deeper
/// path was dropped and the body fired at the argument's death beside
/// the result's owner — `dR1 got1 dR1` on all four surfaces. The
/// fresh-temp registrar now resolves the callee's whole paths into a
/// skip tree over the literal's element types and the discarded-tuple
/// emitter is tree-driven; the named local gets a path-keyed store
/// (`tuple_moved_nested_elem_bodies`) folded with the flat masks by
/// `tuple_skip_tree_for_var`.
///
/// `one` the named two-level shape, `two` the one-level control, `three`
/// a struct under a tuple, `four`/`five` the fresh-temp spellings of one
/// and three, `six` the projection spelling, `seven` the local kept live
/// past the call, `eight` three levels, `nine`/`ten` a two-`R` inner
/// tuple handing back element 1 (the sibling's body stays the callee's),
/// `eleven`/`twelve` a SCALAR read handed back (`r.id`), which the part
/// channel reports too and which must NOT be masked out of the value.
#[test]
fn e2e_tuple_argument_handing_back_a_nested_part_runs_one_body() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
struct S { r: R, n: i64 }
fn tv_ret(t: ((R, i64), i64)) -> R { let (inner, y) = t; let (r, x) = inner; return r; }
fn tv_proj(t: ((R, i64), i64)) -> R { return t.0.0; }
fn flat_t(t: (R, i64)) -> R { let (r, k) = t; return r; }
fn ts_ret(t: (S, i64)) -> R { let (s, k) = t; return s.r; }
fn t3_ret(t: (((R, i64), i64), i64)) -> R { let (mid, a) = t; let (inner, b) = mid; let (r, c) = inner; return r; }
fn tb_ret1(t: ((R, R), i64)) -> R { let (inner, y) = t; let (a, b) = inner; return b; }
fn tv_read(t: ((R, i64), i64)) -> i64 { let (inner, y) = t; let (r, x) = inner; return r.id; }
fn main() {
    { let t: ((R, i64), i64) = ((mk(1), 1), 2); let a: R = tv_ret(t); println(f"got{a.id}"); println("one") }
    { let t: (R, i64) = (mk(2), 2); let a: R = flat_t(t); println(f"got{a.id}"); println("two") }
    { let t: (S, i64) = (S { r: mk(3), n: 1 }, 2); let a: R = ts_ret(t); println(f"got{a.id}"); println("three") }
    { let a: R = tv_ret(((mk(4), 1), 2)); println(f"got{a.id}"); println("four") }
    { let a: R = ts_ret((S { r: mk(5), n: 1 }, 2)); println(f"got{a.id}"); println("five") }
    { let t: ((R, i64), i64) = ((mk(6), 1), 2); let a: R = tv_proj(t); println(f"got{a.id}"); println("six") }
    { let t: ((R, i64), i64) = ((mk(7), 1), 2); let a: R = tv_ret(t); println(f"got{a.id}"); println(f"k{t.1}"); println("seven") }
    { let t: (((R, i64), i64), i64) = (((mk(8), 1), 2), 3); let a: R = t3_ret(t); println(f"got{a.id}"); println("eight") }
    { let t: ((R, R), i64) = ((mk(9), mk(10)), 2); let a: R = tb_ret1(t); println(f"got{a.id}"); println("nine") }
    { let a: R = tb_ret1(((mk(11), mk(12)), 2)); println(f"got{a.id}"); println("ten") }
    { let d: i64 = tv_read(((mk(13), 1), 2)); println(f"r{d}"); println("eleven") }
    { let t: ((R, i64), i64) = ((mk(14), 1), 2); let d: i64 = tv_read(t); println(f"r{d}"); println("twelve") }
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "got1\ndR1\none\ngot2\ndR2\ntwo\ngot3\ndR3\nthree\ngot4\ndR4\nfour\ngot5\ndR5\nfive\ngot6\ndR6\nsix\ngot7\ndR7\nk2\nseven\ngot8\ndR8\neight\ndR9\ngot10\ndR10\nnine\ndR11\ngot12\ndR12\nten\ndR13\nr13\neleven\ndR14\nr14\ntwelve\nend\n");
}

/// B-2026-09-06-30 — a `let` DESTRUCTURE of a mixed struct literal
/// (`let s = S3 { a: r, b: mk(2) }; let S3 { a, b } = s;`, `r` a by-value
/// param) ran the view field's `Drop` body twice on ALL FOUR surfaces —
/// `dR2 dR1 dR1` against the one body due. The literal's
/// `param_view_struct_fields` record guarded `s`'s own walk, but the
/// destructure leaf `a` was registered as an ordinary owner. The struct
/// destructure now marks such a leaf a view (`param_view_locals`,
/// memory-only) and the tuple helper folds `param_view_tuple_elems` into
/// its per-element `leaf_is_view` / `owner_runs_bodies`, mirroring
/// B-2026-09-06-22's match arm. Interpreter twin:
/// `test_let_destructure_of_a_mixed_struct_literal_runs_one_body`.
///
/// Not here, filed separately: the PARTIAL pattern `let S3 { b, .. } = s`
/// binds the wrong field on codegen and double-frees on the JIT.
#[test]
fn e2e_let_destructure_of_a_mixed_struct_literal_runs_one_body() {
    let Some(out) = run_program(
        r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"n{i}" }; }
struct S3 { a: R, b: R }
fn d_b(r: R) -> i64 { let s: S3 = S3 { a: r, b: mk(2) }; let S3 { a, b } = s; return b.id; }
fn d_a(r: R) -> i64 { let s: S3 = S3 { a: r, b: mk(4) }; let S3 { a, b } = s; return a.id; }
fn d_unread(r: R) -> i64 { let s: S3 = S3 { a: r, b: mk(6) }; let S3 { a, b } = s; return 1; }
fn d_rebind(r: R) -> i64 { let s: S3 = S3 { a: r, b: mk(8) }; let s2: S3 = s; let S3 { a, b } = s2; return b.id; }
fn d_rebind_leaf(r: R) -> i64 { let s: S3 = S3 { a: r, b: mk(10) }; let S3 { a, b } = s; let m: R = a; return m.id; }
fn d_swap(r: R) -> i64 { let s: S3 = S3 { a: mk(12), b: r }; let S3 { a, b } = s; return a.id; }
fn d_fresh(r: R) -> i64 { let s: S3 = S3 { a: mk(14), b: mk(15) }; let S3 { a, b } = s; return a.id; }
fn t_b(r: R) -> i64 { let t: (R, R) = (r, mk(19)); let (a, b) = t; return b.id; }
fn t_rebind_leaf(r: R) -> i64 { let t: (R, R) = (r, mk(21)); let (a, b) = t; let m: R = a; return m.id; }
fn main() {
    { let v: i64 = d_b(mk(1)); println(f"v={v}"); println("one") }
    { let v: i64 = d_a(mk(3)); println(f"v={v}"); println("two") }
    { let v: i64 = d_unread(mk(5)); println(f"v={v}"); println("three") }
    { let v: i64 = d_rebind(mk(7)); println(f"v={v}"); println("four") }
    { let v: i64 = d_rebind_leaf(mk(9)); println(f"v={v}"); println("five") }
    { let v: i64 = d_swap(mk(11)); println(f"v={v}"); println("six") }
    { let v: i64 = d_fresh(mk(13)); println(f"v={v}"); println("seven") }
    { let v: i64 = t_b(mk(18)); println(f"v={v}"); println("eight") }
    { let v: i64 = t_rebind_leaf(mk(20)); println(f"v={v}"); println("nine") }
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "dR2\ndR1\nv=2\none\ndR4\ndR3\nv=3\ntwo\ndR6\ndR5\nv=1\nthree\ndR8\ndR7\nv=8\nfour\ndR10\ndR9\nv=9\nfive\ndR12\ndR11\nv=12\nsix\ndR15\ndR14\ndR13\nv=14\nseven\ndR19\ndR18\nv=19\neight\ndR21\ndR20\nv=20\nnine\nend\n");
}

/// B-2026-09-25-14 — a struct returned by a CALL and passed straight to a
/// param that hands it back: `id(mk(3))` over `fn id(d: P) -> P { d }`,
/// generic or not. The callee entry-copies and returns its copy, and the
/// caller registered nothing for its own temp, so the original's heap fields
/// leaked on every compiled surface; a struct literal and a named argument
/// were clean.
#[test]
fn test_e2e_call_result_struct_handed_back_by_callee_frees_original() {
    let out = run_program(
        r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct P { s: String, n: i64 }
struct Q { o: Option[String], v: Vec[String] }
struct H { s: String, r: R }
struct W[T] { v: T, k: i64 }
fn id2[T](d: T) -> T { d }
fn idp(d: P) -> P { d }
fn sel[T](c: bool, a: T, b: T) -> T { if c { a } else { b } }
fn mk(i: i64) -> P { P { s: f"heap-string-longer-than-sso-{i}", n: i } }
fn mkq(i: i64) -> Q { let mut v: Vec[String] = Vec.new(); v.push(f"heap-string-longer-than-sso-v{i}"); Q { o: Some(f"heap-string-longer-than-sso-o{i}"), v: v } }
fn mkh(i: i64) -> H { H { s: f"heap-string-longer-than-sso-h{i}", r: R { id: i } } }
fn mkw(i: i64) -> W[String] { W { v: f"heap-string-longer-than-sso-w{i}", k: i } }
fn main() {
    for i in 0..2 {
        let r = id2(mk(i));
        println(r.n);
        println(idp(mk(i + 10)).n);
        println(sel(i == 0, mk(20), mk(21)).n);
        let q = id2(mkq(i));
        println(q.v.len());
        let h = id2(mkh(i + 30));
        println(h.r.id);
        println(id2(mkw(i)).k);
        let _ = id2(mk(50));
    }
    println("end")
}
"#,
    );
    assert_eq!(
        out.as_deref(),
        Some("0\n10\n20\n1\n30\nd30\n0\n1\n11\n21\n1\n31\nd31\n1\nend\n"),
        "must match --interp"
    );
}
