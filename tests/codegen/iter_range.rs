//! iterators, ranges, collect -- fixtures for `tests/codegen.rs`.
//!
//! Split out of `tests/codegen.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test codegen` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test codegen iter_range::
//!
//! New fixtures about iterators, ranges, collect belong in this file.

use super::*;

#[test]
fn e2e_iter_over_tuple_element_vec() {
    // Regression: iterating a `Vec` that lives in a TUPLE element
    // (`t.0.iter()`) silently iterated ZERO times under codegen while the
    // interpreter iterated the real elements — a wrong-answer miscompile.
    // The for-loop dispatch had a `FieldAccess` receiver arm but no
    // `TupleIndex` sibling, so `t.0.iter()` fell through to the silent
    // `_ =>` default; the same hole zeroed every fused terminal that
    // desugars to that for-loop shape (`t.0.iter().fold(..)`). Covers a
    // for-loop AND a fold, over both an inferred and an annotated tuple
    // binding, for scalar and heap (`String`) elements.
    if let Some(out) = run_program(
        "fn make() -> (Vec[i64], Vec[i64]) {\n\
                 let mut a: Vec[i64] = Vec.new();\n\
                 a.push(10); a.push(20); a.push(30);\n\
                 let mut b: Vec[i64] = Vec.new();\n\
                 b.push(1);\n\
                 (a, b)\n\
             }\n\
             fn strs() -> (Vec[String], i64) {\n\
                 let mut a: Vec[String] = Vec.new();\n\
                 a.push(\"alpha\"); a.push(\"beta\");\n\
                 (a, 0)\n\
             }\n\
             fn main() {\n\
                 let t = make();\n\
                 let mut c = 0;\n\
                 for x in t.0.iter() { c = c + 1; }\n\
                 let s = t.0.iter().fold(0, |acc, x| acc + x);\n\
                 println(f\"c={c} sum={s} len={t.0.len()}\");\n\
                 let ta: (Vec[i64], Vec[i64]) = make();\n\
                 let s2 = ta.1.iter().fold(0, |acc, x| acc + x);\n\
                 println(f\"s2={s2}\");\n\
                 let h = strs();\n\
                 let mut chars = 0;\n\
                 for w in h.0.iter() { chars = chars + w.len(); }\n\
                 println(f\"chars={chars}\");\n\
             }",
    ) {
        assert_eq!(out, "c=3 sum=60 len=3\ns2=1\nchars=9\n");
    }
}

#[test]
fn test_ir_for_range() {
    let ir = ir_for(
        r#"
fn sum_range(n: i64) -> i64 {
    let mut acc = 0;
    for i in 0..n {
        acc = acc + i;
    }
    acc
}
"#,
    );
    assert!(ir.contains("for.cond"));
    assert!(ir.contains("for.body"));
    assert!(ir.contains("for.incr"));
    assert!(ir.contains("for.exit"));
}

#[test]
fn test_ir_for_range_inclusive() {
    let ir = ir_for(
        r#"
fn sum_inclusive(n: i64) -> i64 {
    let mut acc = 0;
    for i in 1..=n {
        acc = acc + i;
    }
    acc
}
"#,
    );
    assert!(ir.contains("for.cond"));
    // Inclusive range uses SLE
    assert!(ir.contains("icmp sle") || ir.contains("for.incr"));
}

/// The COMPILED half of B-2026-08-20-32's parity pair. Codegen was always
/// right here — this is the leg the interpreter had to be brought back
/// into line with, so pinning it stops a future codegen change from
/// "fixing" the divergence in the wrong direction.
///
/// The twin is `test_clone_of_a_nested_collection_is_deep_at_every_level`
/// in `tests/interpreter.rs`, and it asserts THE SAME STRING. Keep them
/// equal: the defect was that they disagreed while both looked reasonable
/// in isolation.
///
/// Two levels rather than three because a three-level index ASSIGNMENT is
/// a separate open codegen gap (B-2026-08-20-33) — the interpreter twin
/// covers depth 3 on its own.
#[test]
fn e2e_clone_of_a_nested_collection_is_deep() {
    let Some(out) = run_program(
        "fn main() {\n\
             \x20   let orig: Vec[Vec[i64]] = [[1, 1], [1, 1]];\n\
             \x20   let mut copy = orig.clone();\n\
             \x20   copy[0][0] = 99i64;\n\
             \x20   println(orig[0][0]);\n\
             \x20   println(copy[0][0]);\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "1\n99\n");
}

/// B-2026-09-14-7 — a payload part MOVED INTO A LOCAL that dies inside the
/// callee's own frame drops at that local's live-range end, not after the
/// call returns.
///
/// ```text
/// fn eat(o: Option[(R, i64)]) { match o { Some(t) => { let x = t.0; println("mid"); } .. } }
/// ```
///
/// The row was filed with its direction reversed and corrected later: the
/// three COMPILED surfaces print `dR5 mid end` and are right, the
/// interpreter printed `mid dR5 end`. design.md § 866 settles it -- "a
/// value whose last use is mid-scope is dropped at that use and does not
/// appear in the end-of-scope stack at all" -- and `x` is never read after
/// its `let`, so the body is owed BEFORE `mid`. The count agreed
/// everywhere, so this was a pure ORDERING divergence that no count-based
/// assertion could have caught.
///
/// MECHANISM: `let x = t.0` off an arm binding of a by-value
/// `Option`/`Result` param reads identically to `let x = h.r` off the param
/// itself, so the interpreter's `let_reads_param_view_field` classified `x`
/// a VIEW of the caller's value and registered no Drop slot for it; the
/// caller's fresh-temp walk ran the body after the call. The repair is an
/// ownership transfer across the call boundary, not a placement tweak --
/// `fn_consumed_param_payload_part_paths` is consulted from BOTH ends of
/// the same call, so the callee's new slot and the caller's stand-down
/// cannot disagree.
///
/// THE METHOD SPELLING IS A CELL BECAUSE IT MOVED THE OTHER WAY. A method
/// frame already registered the slot (its `caller_retains_args` bail), so
/// it printed `dR5 mid dR5` -- a DOUBLED body, not a late one -- and the
/// caller half of this fix is what brings it to one. The two spellings
/// were wrong in opposite directions through one missing channel.
///
/// TWO CELLS WERE PINNED DIVERGENT AND NEITHER WAS THIS ROW'S. The named-
/// local argument doubled on every COMPILED surface (`dR5 mid dR5 end`)
/// and the Drop-bearing SIBLING part is lost on every compiled surface --
/// both measured at this commit, both pre-existing, both filed separately.
/// They are cells here because this row's fix moves the interpreter side
/// of each, and pinning both halves is what keeps a later compiled fix
/// from landing silently. That is exactly what it did: B-2026-09-17-37
/// closed the named-local half, and this fixture is where it showed --
/// that cell AND the deeper-frame control both moved to agreement in the
/// same commit, the second of them a cell nobody had noticed was carrying
/// the same double. The SIBLING cell was closed in turn by
/// B-2026-09-17-38, whose own fixture is
/// `e2e_optres_payload_sibling_part_keeps_its_body_when_its_peer_is_consumed`;
/// both cells here now read the same on both halves, so this fixture pins
/// nothing divergent except the two `return`-escape controls it names
/// above, which belong to other rows.
///
/// THE ESCAPE CONTROL IS THE ONE THAT KEEPS THE RULE HONEST: `let x = t.0;
/// return x;` hands the part OUT of the frame, so the consumed channel
/// must decline it and leave the shape exactly as it behaves today. Its
/// interpreter double is B-2026-09-13-5's alias spelling, untouched here.
///
/// BODY-ONLY, so no sanitizer leg can see any of it: the row records `-O0`
/// valgrind at `0 bytes in 0 blocks` / `0 errors` on its own cell.
#[test]
fn e2e_optres_payload_part_consumed_in_frame_drops_at_its_own_live_range_end() {
    const R: &str = "struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n";
    // (label, source, AOT expectation, interpreter expectation)
    for (label, prog, want, interp_want) in [
            (
                "the row's cell: Option head, fresh-temp argument",
                format!(
                    "{R}fn eat(o: Option[(R, i64)]) {{ match o {{ Some(t) => {{ let x = t.0; println(\"mid\"); }} None => {{ println(\"n\"); }} }} }}\n\
                     fn main() {{ eat(Some((R {{ id: 5 }}, 9i64))); println(\"end\") }}\n"
                ),
                "dR5\nmid\nend\n",
                "dR5\nmid\nend\n",
            ),
            (
                "Result head",
                format!(
                    "{R}fn eat(o: Result[(R, i64), i64]) {{ match o {{ Ok(t) => {{ let x = t.0; println(\"mid\"); }} Err(e) => {{ println(\"n\"); }} }} }}\n\
                     fn main() {{ eat(Result.Ok((R {{ id: 5 }}, 9i64))); println(\"end\") }}\n"
                ),
                "dR5\nmid\nend\n",
                "dR5\nmid\nend\n",
            ),
            (
                "method spelling -- was a DOUBLED body in the interpreter",
                format!(
                    "{R}struct H {{ n: i64 }}\n\
                     impl H {{ fn eat(ref self, o: Option[(R, i64)]) {{ match o {{ Some(t) => {{ let x = t.0; println(\"mid\"); }} None => {{ println(\"n\"); }} }} }} }}\n\
                     fn main() {{ let h = H {{ n: 1 }}; h.eat(Some((R {{ id: 5 }}, 9i64))); println(\"end\") }}\n"
                ),
                "dR5\nmid\nend\n",
                "dR5\nmid\nend\n",
            ),
            (
                "if let spelling",
                format!(
                    "{R}fn eat(o: Option[(R, i64)]) {{ if let Some(t) = o {{ let x = t.0; println(\"mid\"); }} }}\n\
                     fn main() {{ eat(Some((R {{ id: 5 }}, 9i64))); println(\"end\") }}\n"
                ),
                "dR5\nmid\nend\n",
                "dR5\nmid\nend\n",
            ),
            (
                "control: the local IS read later, so its body is owed later",
                format!(
                    "{R}fn eat(o: Option[(R, i64)]) {{ match o {{ Some(t) => {{ let x = t.0; println(\"mid\"); println(f\"v:{{x.id}}\"); }} None => {{ println(\"n\"); }} }} }}\n\
                     fn main() {{ eat(Some((R {{ id: 5 }}, 9i64))); println(\"end\") }}\n"
                ),
                "mid\nv:5\ndR5\nend\n",
                "mid\nv:5\ndR5\nend\n",
            ),
            (
                "control: `return x` ESCAPES, so the consumed channel declines",
                format!(
                    "{R}fn eat(o: Option[(R, i64)]) -> R {{ match o {{ Some(t) => {{ let x = t.0; println(\"mid\"); return x; }} None => {{ return R {{ id: 0 }}; }} }} }}\n\
                     fn main() {{ let g = eat(Some((R {{ id: 5 }}, 9i64))); println(f\"got:{{g.id}}\"); println(\"end\") }}\n"
                ),
                "mid\ngot:5\ndR5\nend\n",
                // B-2026-09-13-5's ALIAS spelling: the escape predicate reads
                // `return <local>`, not `return t.0`, so the interpreter still
                // doubles here. Pinned, not repaired -- this row's channel must
                // decline the shape, and that it does is what this cell proves.
                "mid\ndR5\ngot:5\ndR5\nend\n",
            ),
            (
                "control: the name-keyed mask does not leak into a deeper frame",
                format!(
                    "{R}fn eat(o: Option[(R, i64)]) {{ match o {{ Some(t) => {{ let x = t.0; println(\"mid\"); }} None => {{ println(\"n\"); }} }} }}\n\
                     fn inner() {{ let a: Option[(R, i64)] = Some((R {{ id: 7 }}, 1i64)); println(\"in\") }}\n\
                     fn main() {{ let a = Some((R {{ id: 5 }}, 9i64)); eat(a); inner(); println(\"end\") }}\n"
                ),
                // B-2026-09-17-37 — the second `dR5` here was the named-local
                // double, not a frame-leak. Both halves of this cell now read
                // the same, which is what the control was asking.
                "dR5\nmid\ndR7\nin\nend\n",
                "dR5\nmid\ndR7\nin\nend\n",
            ),
            (
                "B-2026-09-17-37: a NAMED-local argument no longer doubles",
                format!(
                    "{R}fn eat(o: Option[(R, i64)]) {{ match o {{ Some(t) => {{ let x = t.0; println(\"mid\"); }} None => {{ println(\"n\"); }} }} }}\n\
                     fn main() {{ let a = Some((R {{ id: 5 }}, 9i64)); eat(a); println(\"end\") }}\n"
                ),
                "dR5\nmid\nend\n",
                "dR5\nmid\nend\n",
            ),
            (
                // B-2026-09-17-38 — this cell was pinned DIVERGENT here, the
                // compiled surfaces printing `dR5 mid end` for element 1's
                // body owed to nobody. Both halves now read the same.
                "B-2026-09-17-38: the Drop-bearing SIBLING part keeps its body",
                format!(
                    "{R}fn eat(o: Option[(R, R)]) {{ match o {{ Some(t) => {{ let x = t.0; println(\"mid\"); }} None => {{ println(\"n\"); }} }} }}\n\
                     fn main() {{ eat(Some((R {{ id: 5 }}, R {{ id: 6 }}))); println(\"end\") }}\n"
                ),
                "dR5\nmid\ndR6\nend\n",
                "dR5\nmid\ndR6\nend\n",
            ),
        ] {
            let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(&prog);
            assert!(
                interp_errs.is_empty(),
                "[{label}] interp errored: {interp_errs:?}"
            );
            assert_eq!(interp_out.join(""), interp_want, "[{label}] interpreter");
            if let Some(aot) = run_program(&prog) {
                assert_eq!(aot, want, "[{label}] AOT");
            }
        }
}

/// B-2026-09-19-41 — a `Drop`-bearing NAMED FIELD moved out of an
/// `Option` payload runs its body at the binding's live-range end, on
/// every compiled surface.
///
/// `Some(t) => { let x = t.r; println("mid") }` over `Option[P]` for
/// `struct P { r: R, n: i64 }`. `x` is never read after its `let`, so
/// design.md § 866 puts the body BEFORE `mid`: "Destructors fire at each
/// binding's live-range end, not at lexical scope end". The TUPLE spelling
/// (`t.0`) has printed it there since B-2026-09-14-7; the field spelling
/// came out three different ways instead, all wrong and all from one cause.
///
/// WHAT THE PARENT TREE PRINTS on these same cells, measured:
///
/// ```text
///   named   mid dR5            LATE  (the caller's walk ran it)
///   temp    mid                LOST  (nobody ran it)
///   after   mid after dR5      LATE  (and at the callee's FRAME exit,
///                                     not the arm's close)
///   method  mid dR5            LATE
///   assoc   mid dR5            LATE
/// ```
///
/// Five of the eleven cells; the other six are byte-identical before and
/// after, which is what makes this a fix rather than a re-balance.
///
/// THE CAUSE IS ONE GATE, and the three faces are the caller's doing. The
/// callee's move-out disarms the field from the arm binding's own walk
/// (statically, or through the `fvflag.t.r` bit
/// `conditional_field_move_takes_runtime_flag` emits) and then
/// B-2026-08-29-47's param-view suppression ALSO withheld a body from the
/// destination, so the field had no owner in the callee at all. What the
/// program printed then depended on what the caller happened to hold: a
/// named local's walk ran it late, a fresh temp's — minted already masked —
/// ran it nowhere. Both ends move in this commit; masking one and not the
/// other is what produced two of the three.
///
/// TWO CELLS WERE PINNED DIVERGENT HERE AND ARE NOW CONVERGED, which is
/// the whole of what B-2026-09-19-48 changed in this fixture:
///
/// ```text
///   nomove  peek9 dR5 dR5  ->  peek9 dR5   (the arm moves NOTHING)
///   two     dR5 mid dR6 dR6 -> dR5 mid dR6 (the UNMOVED sibling)
/// ```
///
/// One question, not two: for a STRUCT payload both the callee's arm
/// binding and the caller's named local believed they owned the payload's
/// bodies, where the tuple spelling has exactly one owner. That is
/// B-2026-09-17-38's subject — the sibling part's owner — reached from the
/// doubling side rather than the losing side. It was deliberately left open
/// here, pinned so it could not move silently, and this is the deliberate
/// move: `bind_pattern_values` no longer registers a second walk on a
/// caller-retained payload binding, so the assertion above is now
/// byte-identical to the interpreter twin's, which is the point.
///
/// The `heap` cell is the discriminator worth keeping: give the payload's
/// field a `String` and every symptom vanishes on the parent tree too, so a
/// payload struct that is plain data apart from its own `Drop` is what
/// takes the broken path.
///
/// BODY-ONLY, so no sanitizer leg sees it: `-O0` valgrind reads
/// `22 allocs, 22 frees`, `0 bytes in 0 blocks` in use at exit and
/// `0 errors`, before and after. `karac run`, `karac build` and `karac
/// build` at `KARAC_AUTO_PAR=0` are byte-identical on every cell.
///
/// The INTERPRETER twin is `tests/interpreter.rs`'s
/// `test_named_struct_optres_payload_part_drops_at_its_own_live_range_end`,
/// byte-identical source, its expectation differing in exactly the two
/// pinned cells above.
#[test]
fn e2e_named_struct_optres_payload_part_drops_at_its_own_live_range_end() {
    let Some(out) = run_program(
        r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
struct P { r: R, n: i64 }
struct Q { r: R, s: R }
struct H { name: String, id: i64 }
impl Drop for H { fn drop(mut ref self) { println(f"  dH{self.id}") } }
struct Ph { h: H, n: i64 }

fn eat(o: Option[P]) { match o { Option.Some(t) => { let x = t.r; println("  mid") } Option.None => { println("  n") } } }
fn eat_after(o: Option[P]) { match o { Option.Some(t) => { let x = t.r; println("  mid") } Option.None => { println("  n") } } println("  after") }
fn eat_two(o: Option[Q]) { match o { Option.Some(t) => { let x = t.r; println("  mid") } Option.None => { println("  n") } } }
fn eat_heap(o: Option[Ph]) { match o { Option.Some(t) => { let x = t.h; println("  mid") } Option.None => { println("  n") } } }
fn eat_tuple(o: Option[(R, i64)]) { match o { Option.Some(t) => { let x = t.0; println("  mid") } Option.None => { println("  n") } } }
fn eat_nomove(o: Option[P]) { match o { Option.Some(t) => { println(f"  peek{t.n}") } Option.None => { println("  n") } } }

struct Sink { tag: i64 }
impl Sink {
  fn take(ref self, o: Option[P]) { match o { Option.Some(t) => { let x = t.r; println("  mid") } Option.None => { println("  n") } } }
  fn grab(o: Option[P]) { match o { Option.Some(t) => { let x = t.r; println("  mid") } Option.None => { println("  n") } } }
}

fn main() {
  println("named")
  { let a = Option.Some(P { r: R { id: 5 }, n: 9 }); eat(a) }
  println("  out")

  println("temp")
  eat(Option.Some(P { r: R { id: 5 }, n: 9 }))
  println("  out")

  println("after")
  { let a = Option.Some(P { r: R { id: 5 }, n: 9 }); eat_after(a) }
  println("  out")

  println("heap")
  { let a = Option.Some(Ph { h: H { name: "n5", id: 5 }, n: 9 }); eat_heap(a) }
  println("  out")

  println("tuple")
  { let a = Option.Some((R { id: 5 }, 9)); eat_tuple(a) }
  println("  out")

  println("nomove")
  { let a = Option.Some(P { r: R { id: 5 }, n: 9 }); eat_nomove(a) }
  println("  out")

  println("method")
  { let s = Sink { tag: 1 }; let a = Option.Some(P { r: R { id: 5 }, n: 9 }); s.take(a) }
  println("  out")

  println("assoc")
  { let a = Option.Some(P { r: R { id: 5 }, n: 9 }); Sink.grab(a) }
  println("  out")

  println("two")
  { let a = Option.Some(Q { r: R { id: 5 }, s: R { id: 6 } }); eat_two(a) }
  println("  out")

  println("none")
  { let a: Option[P] = Option.None; eat(a) }
  println("  out")

  println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "named\n  dR5\n  mid\n  out\ntemp\n  dR5\n  mid\n  out\nafter\n  dR5\n  mid\n  after\n  out\nheap\n  dH5\n  mid\n  out\ntuple\n  dR5\n  mid\n  out\nnomove\n  peek9\n  dR5\n  out\nmethod\n  dR5\n  mid\n  out\nassoc\n  dR5\n  mid\n  out\ntwo\n  dR5\n  mid\n  dR6\n  out\nnone\n  n\n  out\nend\n", "got:\n{out}");
}

#[test]
fn test_e2e_iter_max_min_over_total_order_wrapper() {
    // B-2026-08-11-15 — `max()`/`min()` over a `Vec[F64]`, the remedy the
    // `f64` gate now prescribes. This is the half that made the gate
    // landable: -7 withdrew it because admitting the wrapper at typecheck
    // only moved the failure into codegen, which would have traded a
    // silent wrong answer for a run-vs-build gap.
    //
    // The NaN cases are the point. Under raw IEEE comparison `max`/`min`
    // are order-DEPENDENT — `[3.0, NaN, 1.0]` skips the NaN and answers 3,
    // `[NaN, 3.0, 1.0]` makes BOTH answer NaN. Under the wrapper's total
    // order the answer depends only on the SET: NaN sorts last
    // (design.md § Float semantics), so max is NaN and min is 1 wherever
    // the NaN sits. Both orderings are asserted, and they must agree.
    //
    // The result is bound with `let` rather than chained as
    // `.max().unwrap().value`: chained field access on an Option-unwrap
    // TEMPORARY is a separate, pre-existing codegen gap that reproduces
    // with a plain struct and no iterator involved, so spelling it that
    // way here would fail for a reason unrelated to this fix.
    let out = run_program(
        "fn main() {\n\
                 let z: f64 = 0.0;\n\
                 let nan: f64 = z / z;\n\
                 let clean: Vec[f64] = [3.0, 1.0, 2.0];\n\
                 let cw: Vec[F64] = clean.iter().map(F64.from).collect();\n\
                 let cmax: F64 = cw.iter().max().unwrap();\n\
                 let cmin: F64 = cw.iter().min().unwrap();\n\
                 println(cmax.value);\n\
                 println(cmin.value);\n\
                 let first: Vec[f64] = [nan, 3.0, 1.0];\n\
                 let fw: Vec[F64] = first.iter().map(F64.from).collect();\n\
                 let fmax: F64 = fw.iter().max().unwrap();\n\
                 let fmin: F64 = fw.iter().min().unwrap();\n\
                 println(fmax.value);\n\
                 println(fmin.value);\n\
                 let mid: Vec[f64] = [3.0, nan, 1.0];\n\
                 let mw: Vec[F64] = mid.iter().map(F64.from).collect();\n\
                 let mmax: F64 = mw.iter().max().unwrap();\n\
                 let mmin: F64 = mw.iter().min().unwrap();\n\
                 println(mmax.value);\n\
                 println(mmin.value);\n\
             }",
    );
    if let Some(out) = out {
        // clean max/min, then NaN-first and NaN-middle: both give NaN/1,
        // i.e. position-independent, which is the whole property the
        // wrapper buys and the raw `f64` path could not provide.
        assert_eq!(out, "3\n1\nNaN\n1\nNaN\n1\n");
    }
}

/// The METHOD-CALL half of B-2026-08-21-24 (found while building
/// B-2026-08-22-6). That row spilled the synthesized header for a `ref
/// Slice[T]` slot on the free-function path and never on this one — the
/// same "landed on two of the three call paths" shape as B-2026-06-19-1,
/// B-2026-08-05-41 and B-2026-08-21-39 before it.
///
/// Found through design.md § `Hash` and `Hasher`, whose `Hasher.write_u8`
/// default body is literally `self.write([n])`. Baking that trait therefore
/// made EVERY user `impl Hasher` fail module verification with "Call
/// parameter type does not match function signature", while `karac check`
/// accepted the program and `--interp` ran it — so the spec's own text was
/// the reproducer.
///
/// Both receiver shapes, because they are separate arms: a trait DEFAULT
/// body monomorphized into the impl, and an ordinary inherent method. The
/// by-value `Slice[T]` control sits beside each, since that spelling always
/// worked and a regression confined to the `ref` slot would otherwise hide
/// behind it.
#[test]
fn e2e_collection_literal_into_a_ref_slice_param_of_a_method() {
    let src = "trait Sink {\n\
                   fn write(mut ref self, b: ref Slice[u8]);\n\
                   fn write_val(mut ref self, b: Slice[u8]);\n\
                   fn total(ref self) -> u64;\n\
                   fn put(mut ref self, n: u8) { self.write([n]) }\n\
                   fn put_val(mut ref self, n: u8) { self.write_val([n]) }\n\
                   }\n\
                   struct Acc { n: u64 }\n\
                   impl Sink for Acc {\n\
                   fn write(mut ref self, b: ref Slice[u8]) {\n\
                   for x in b { self.n = self.n + (x as u64); }\n\
                   }\n\
                   fn write_val(mut ref self, b: Slice[u8]) {\n\
                   for x in b { self.n = self.n + (x as u64); }\n\
                   }\n\
                   fn total(ref self) -> u64 { self.n }\n\
                   }\n\
                   struct Own { n: u64 }\n\
                   impl Own {\n\
                   fn write(mut ref self, b: ref Slice[u8]) {\n\
                   for x in b { self.n = self.n + (x as u64); }\n\
                   }\n\
                   fn put(mut ref self, n: u8) { self.write([n]) }\n\
                   fn total(ref self) -> u64 { self.n }\n\
                   }\n\
                   fn main() {\n\
                   let mut a = Acc { n: 0u64 };\n\
                   a.put(3u8);\n\
                   a.put_val(4u8);\n\
                   println(a.total());\n\
                   let mut o = Own { n: 0u64 };\n\
                   o.put(5u8);\n\
                   println(o.total());\n\
                   }";
    assert_eq!(run_program(src).as_deref(), Some("7\n5\n"));
}

// ── Refinement types + contracts + struct-variant heap payloads (codegen) ──
//
// Regression cluster surfaced by the `examples/weave` dogfood when first
// built (not just `karac run`) via codegen. Each test pins one fix.

#[test]
fn e2e_refinement_alias_collection_method_and_iteration() {
    // A refinement alias of a collection (`type NonEmpty[T] = Vec[T] where
    // self.len() > 0`) used as a parameter type must dispatch collection
    // methods (`.len()`) and `for` iteration on the binding — codegen has to
    // resolve the alias to its *instantiated* base `Vec[EnrichedRow]` (with
    // the generic arg substituted) so `vec_elem_types` is registered.
    // Pre-fix: "no handler for method 'len' on variable 'rows'".
    if let Some(out) = run_program(
        "pub type NonEmpty[T] = Vec[T] where self.len() > 0;\n\
             pub struct Row { v: i64 }\n\
             pub fn total(rows: NonEmpty[Row]) -> i64 {\n\
                 let mut n = 0;\n\
                 for r in rows { n = n + r.v; }\n\
                 n\n\
             }\n\
             fn main() {\n\
                 let mut xs: Vec[Row] = Vec.new();\n\
                 xs.push(Row { v: 10 });\n\
                 xs.push(Row { v: 32 });\n\
                 println(f\"len={xs.len()} total={total(xs as NonEmpty[Row])}\");\n\
             }",
    ) {
        assert_eq!(out, "len=2 total=42\n");
    }
}

#[test]
fn test_e2e_for_iter_mut_scalar() {
    // B-2026-07-14-10 (codegen leg): `for x in xs.iter_mut()` over a named
    // Vec with a SCALAR element. The loop binds `x` as a mut-ref slot
    // pointer into the Vec's storage (`entry_slot_ref_vars` deref
    // machinery), so `*x = …` and `*x += …` write back in place. Covers
    // i64 and f64 elements and two sequential mutation passes; must match
    // the interpreter oracle. Heap elements / destructure patterns bail
    // loud to `--interp`.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let mut v: Vec[i64] = [1, 2, 3, 4];\n\
                 for x in v.iter_mut() { *x = *x * 2; }\n\
                 for x in v.iter_mut() { *x += 1; }\n\
                 let mut s = 0;\n\
                 for y in v { s = s + y; }\n\
                 println(s);\n\
                 let mut f: Vec[f64] = [1.5, 2.5];\n\
                 for z in f.iter_mut() { *z = *z + 0.5; }\n\
                 let mut fs = 0.0;\n\
                 for w in f { fs = fs + w; }\n\
                 println(fs);\n\
             }",
    ) {
        assert_eq!(out, "24\n5\n");
    }
}

#[test]
fn test_e2e_direct_vec_iterator_terminals_survive_being_chained() {
    // B-2026-08-11-19 — `v.max()` / `.min()` / `.sum()` / `.product()`
    // without an `.iter()` hop are desugared to the `.iter().<terminal>()`
    // chain the backends implement. The desugar's gate used to read a
    // side-table keyed by the call's own span — which the parser sets equal
    // to the RECEIVER's span, so every call in a chain collapses to one key
    // and the last write wins. One extra chained call was enough to
    // overwrite `Vec.max` with `i64.to_string`; the gate then saw a non-Vec
    // head, skipped the rewrite, and both backends reported the raw `max`
    // as an unsupported method — blaming `max`, which was fine.
    //
    // Each line below chains at least one further call onto the terminal,
    // which is the shape that used to fail. The bare form is included as
    // the control: it always worked, so a regression there means the
    // desugar broke rather than the key.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let xs: Vec[i64] = [1, 5, 3];\n\
                     println(xs.max().unwrap());\n\
                     println(\"max=\" + xs.max().unwrap().to_string());\n\
                     let s = xs.min().unwrap().to_string();\n\
                     println(s);\n\
                     println(\"sum=\" + xs.sum().to_string());\n\
                     println(\"prod=\" + xs.product().to_string());\n\
                     let mut d: VecDeque[i64] = VecDeque.new();\n\
                     d.push_back(4);\n\
                     d.push_back(9);\n\
                     println(\"dmax=\" + d.max().unwrap().to_string());\n\
                 }\n"
        )
        .as_deref(),
        Some("5\nmax=5\n1\nsum=9\nprod=15\ndmax=9\n"),
    );
}

#[test]
fn test_e2e_iter_chain_fold_terminal() {
    // B-2026-07-11-17 — `fold(init, |acc, x| body)` on a fused iterator chain.
    // Before the fix it fell through to the loud "no handler for method 'fold'
    // on non-identifier receiver" dispatch error (the interpreter ran it), so
    // any chain ending in `fold` failed `karac build`. Covers: a bare
    // `iter().fold` sum, a `map().fold`, a `filter().fold`, a two-stage
    // `filter().map().fold`, a `fold` whose body branches (max), a nonzero
    // init, an empty source (folds to init), and a `range` fold.
    if let Some(out) = run_program(
            "fn main() {\n\
                 let v: Vec[i64] = [1, 2, 3, 4, 5, 6];\n\
                 println(f\"{v.iter().fold(0, |a, x| a + x)}\");\n\
                 println(f\"{v.iter().map(|x| x * x).fold(0, |a, x| a + x)}\");\n\
                 println(f\"{v.iter().filter(|x| x % 2 == 0).fold(0, |a, x| a + x)}\");\n\
                 println(f\"{v.iter().filter(|x| x > 2).map(|x| x * 10).fold(0, |a, x| a + x)}\");\n\
                 println(f\"{v.iter().fold(0, |a, x| if x > a { x } else { a })}\");\n\
                 println(f\"{v.iter().fold(100, |a, x| a - x)}\");\n\
                 let e: Vec[i64] = [];\n\
                 println(f\"{e.iter().fold(42, |a, x| a + x)}\");\n\
                 println(f\"{(0..5).fold(0, |a, x| a + x)}\");\n\
             }",
        ) {
            assert_eq!(out, "21\n91\n12\n180\n6\n79\n42\n10\n");
        }
}

#[test]
fn test_e2e_iter_chain_sum_terminal() {
    // B-2026-07-11-19 — the numeric `sum()` terminal on a fused iterator
    // chain. Before the fix it was rejected at TYPECHECK ("no method 'sum'
    // on type 'Iterator'"). Now it desugars to `fold((0 as elem), |a, x| a +
    // x)`, seeding the accumulator with a width-correct zero read from the
    // typechecker-recorded element type. Covers: a bare `iter().sum`, a
    // `map().sum`, a `filter().sum`, a two-stage `filter().map().sum`, an
    // f64 sum (the zero must be `0.0`, not an i64 `0`), an empty source
    // (sums to 0), and a `range` sum.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let v: Vec[i64] = [1, 2, 3, 4, 5, 6];\n\
                 println(f\"{v.iter().sum()}\");\n\
                 println(f\"{v.iter().map(|x| x * x).sum()}\");\n\
                 println(f\"{v.iter().filter(|x| x % 2 == 0).sum()}\");\n\
                 println(f\"{v.iter().filter(|x| x > 2).map(|x| x * 10).sum()}\");\n\
                 let f: Vec[f64] = [1.5, 2.5, 3.0];\n\
                 println(f\"{f.iter().sum()}\");\n\
                 let e: Vec[i64] = [];\n\
                 println(f\"{e.iter().sum()}\");\n\
                 println(f\"{(1..5).sum()}\");\n\
             }",
    ) {
        // 21, 91 (1+4+9+16+25+36), 12 (2+4+6), 180 ((3+4+5+6)*10),
        // 7 (1.5+2.5+3.0), 0, 10 (1+2+3+4)
        assert_eq!(out, "21\n91\n12\n180\n7\n0\n10\n");
    }
}

#[test]
fn test_e2e_iter_chain_product_terminal() {
    // The `product()` numeric terminal — the multiplicative sibling of
    // `sum()`. Desugars to `fold((1 as elem), |a, x| a * x)`, seeding the
    // accumulator with a width-correct ONE. Covers: a bare `iter().product`,
    // a `map().product`, a `filter().product`, an f64 product (the seed must
    // be `1.0`), an empty source (products to 1), and a `range` product.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let v: Vec[i64] = [1, 2, 3, 4];\n\
                 println(f\"{v.iter().product()}\");\n\
                 println(f\"{v.iter().map(|x| x + 1).product()}\");\n\
                 println(f\"{v.iter().filter(|x| x % 2 == 0).product()}\");\n\
                 let f: Vec[f64] = [1.5, 2.0, 4.0];\n\
                 println(f\"{f.iter().product()}\");\n\
                 let e: Vec[i64] = [];\n\
                 println(f\"{e.iter().product()}\");\n\
                 println(f\"{(1..5).product()}\");\n\
             }",
    ) {
        // 24 (1*2*3*4), 120 (2*3*4*5), 8 (2*4), 12 (1.5*2*4), 1, 24 (1*2*3*4)
        assert_eq!(out, "24\n120\n8\n12\n1\n24\n");
    }
}

#[test]
fn test_e2e_iter_chain_count_terminal() {
    // B-2026-07-11-19 — the `count()` terminal on a fused iterator chain.
    // Before the fix it was caught by the materialized-collection `len`/
    // `count` intercept, which tried to compile the chain receiver as a Vec
    // and failed on the `map`/`filter` adaptor ("no handler for method
    // 'filter'"). Now an iterator-chain `count` desugars to `fold(0, |a, _|
    // a + 1)`. Covers a bare `iter().count`, `filter().count`, `map().count`
    // (map doesn't change the count), a two-stage `filter().map().count`, an
    // empty source (0), and a `range().count`. A materialized
    // `s.chars().count()` still falls through to the intercept.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let v: Vec[i64] = [1, 2, 3, 4, 5];\n\
                 println(f\"{v.iter().count()}\");\n\
                 println(f\"{v.iter().filter(|x| x > 2).count()}\");\n\
                 println(f\"{v.iter().map(|x| x * 2).count()}\");\n\
                 println(f\"{v.iter().filter(|x| x > 1).map(|x| x + 1).count()}\");\n\
                 let e: Vec[i64] = [];\n\
                 println(f\"{e.iter().count()}\");\n\
                 println(f\"{(1..5).count()}\");\n\
                 let s: String = \"hello\";\n\
                 println(f\"{s.chars().count()}\");\n\
             }",
    ) {
        // 5, 3 (>2), 5, 4 (>1), 0, 4 (1..5), 5 (chars)
        assert_eq!(out, "5\n3\n5\n4\n0\n4\n5\n");
    }
}

#[test]
fn test_e2e_iter_chain_for_each_terminal() {
    // B-2026-07-11-19 / -23 — the side-effecting `for_each` terminal on a
    // fused iterator chain. Desugars to a `for` loop over the peeled base
    // with the closure body inlined, so a capture-mutating body propagates.
    // Covers a bare for_each, a map().for_each, a filter().for_each, and a
    // range for_each. Interp == codegen.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let v: Vec[i64] = [1, 2, 3, 4];\n\
                 let mut total = 0;\n\
                 v.iter().for_each(|x| { total = total + x; });\n\
                 println(f\"{total}\");\n\
                 v.iter().map(|x| x * 2).for_each(|x| { total = total + x; });\n\
                 println(f\"{total}\");\n\
                 let mut ev = 0;\n\
                 v.iter().filter(|x| x % 2 == 0).for_each(|x| { ev = ev + x; });\n\
                 println(f\"{ev}\");\n\
                 let mut rng = 0;\n\
                 (1..5).for_each(|x| { rng = rng + x; });\n\
                 println(f\"{rng}\");\n\
             }",
    ) {
        // 10, 30 (10+20), 6 (2+4), 10 (1+2+3+4)
        assert_eq!(out, "10\n30\n6\n10\n");
    }
}

#[test]
fn test_e2e_iter_chain_over_temporary_vec_source() {
    // B-2026-07-18-39 — an iterator chain whose SOURCE is a TEMPORARY Vec (a
    // `vec![…]` literal or a call result, NOT a `let`-bound variable)
    // silently miscompiled to 0/empty: the typechecker only recorded the
    // fresh-temp element type for Call/MethodCall `.iter()` receivers, so a
    // `PrefixCollectionLiteral` source left `temp_recv_elem_types` empty and
    // codegen's `try_compile_for_vec_value` skipped the loop body. The `sum`/
    // `min`/`max`/`prod` terminals additionally got mis-stolen by the tensor
    // full-reduce path once the temp entry existed (both key on the collided
    // chain span); an iterator terminal is now distinguished by its
    // `iter_terminal_elem_types` entry so the tensor path declines. Every
    // reduce terminal + the bare `for` loop must match the interpreter.
    if let Some(out) = run_program(
        "fn main() {\n\
                 println(vec![1, 2, 3].iter().sum());\n\
                 println(vec![1, 2, 3, 4].iter().product());\n\
                 println(vec![1, 2, 3].iter().fold(0, |a, x| a + x));\n\
                 println(vec![1, 2, 3, 4].iter().count());\n\
                 match vec![1, 5, 3].iter().max() { Some(m) => println(m), None => println(-1) }\n\
                 match vec![4, 1, 3].iter().min() { Some(m) => println(m), None => println(-1) }\n\
                 println(vec![1, 2, 3].iter().map(|x| x * 2).sum());\n\
                 println(vec![1, 2, 3, 4].iter().filter(|x| x % 2 == 0).sum());\n\
                 println(vec![1, 2, 3].into_iter().sum());\n\
                 let mut n = 0;\n\
                 for x in vec![10, 20, 30].iter() { n = n + x; }\n\
                 println(n);\n\
             }",
    ) {
        // sum=6, product=24, fold=6, count=4, max=5, min=1,
        // map(*2).sum=12, filter(even).sum=6, into_iter.sum=6, for-loop=60
        assert_eq!(out, "6\n24\n6\n4\n5\n1\n12\n6\n6\n60\n");
    }
}

#[test]
fn test_e2e_iter_chain_reduce_scalar_terminal() {
    // B-2026-07-11-19 — the `reduce(|a, x| ..) -> Option[A]` terminal for a
    // SCALAR element. Desugars to an `Option[i64]` accumulator folded via a
    // synthetic `match` (None => Some(x), Some(a) => Some(body)). Covers a
    // sum reduce, a max reduce, an empty source (None), a map().reduce, and a
    // filter().reduce. (Heap-element reduce stays interp-only — gated out.)
    if let Some(out) = run_program(
            "fn main() {\n\
                 let v: Vec[i64] = [1, 2, 3, 4];\n\
                 match v.iter().reduce(|a, x| a + x) { Some(s) => println(s), None => println(-1) }\n\
                 match v.iter().reduce(|a, x| if a > x { a } else { x }) { Some(s) => println(s), None => println(-1) }\n\
                 let e: Vec[i64] = [];\n\
                 match e.iter().reduce(|a, x| a + x) { Some(s) => println(s), None => println(-1) }\n\
                 match v.iter().map(|x| x * 2).reduce(|a, x| a + x) { Some(s) => println(s), None => println(-1) }\n\
                 match v.iter().filter(|x| x > 1).reduce(|a, x| a + x) { Some(s) => println(s), None => println(-1) }\n\
             }",
        ) {
            // 10, 4, -1 (empty), 20, 9 (2+3+4)
            assert_eq!(out, "10\n4\n-1\n20\n9\n");
        }
}

#[test]
fn test_e2e_materialized_iterator_binding() {
    // B-2026-07-11-19 — a `let it = v.iter()` iterator binding used at a
    // terminal / adaptor / for-loop. Codegen has no runtime iterator value,
    // so the binding's chain is recorded and inlined at each use (gated on
    // the RHS being typechecker-typed `Iterator`, so a collection-returning
    // `.iter()` like `Column.iter()` / `bytes()` is never mis-intercepted).
    // Covers: bare fold, adaptor-in-let sum, for_each, a chained
    // `let it5 = it4.filter(..)`, and a for-loop over the binding.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let v: Vec[i64] = [1, 2, 3, 4];\n\
                 let it = v.iter();\n\
                 println(it.fold(0, |a, x| a + x));\n\
                 let it2 = v.iter().map(|x| x * 2);\n\
                 println(it2.sum());\n\
                 let it3 = v.iter();\n\
                 let mut tot = 0;\n\
                 it3.for_each(|x| { tot = tot + x; });\n\
                 println(tot);\n\
                 let it4 = v.iter();\n\
                 let it5 = it4.filter(|x| x > 2);\n\
                 println(it5.sum());\n\
                 let it6 = v.iter();\n\
                 let mut s = 0;\n\
                 for x in it6 { s = s + x; }\n\
                 println(s);\n\
             }",
    ) {
        // 10, 20, 10, 7 (3+4), 10
        assert_eq!(out, "10\n20\n10\n7\n10\n");
    }
}

#[test]
fn test_e2e_collection_returning_iter_not_mis_materialized() {
    // Guard the SOUND gate: `Column.iter()` returns `Vec[Option[T]]` (a real
    // collection, NOT an Iterator), so a `let all = c.iter()` binding must be
    // compiled as the Vec value it is (indexed / for-looped), never inlined
    // as an iterator chain. Was a regression in the syntactic-only draft.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let mut c: Column[i64] = Column.new();\n\
                 c.push(10);\n\
                 c.push(30);\n\
                 let all: Vec[Option[i64]] = c.iter();\n\
                 println(all.len());\n\
                 let b = \"ab\".bytes();\n\
                 println(b[0] + b[1]);\n\
             }",
    ) {
        assert_eq!(out, "2\n195\n");
    }
}

#[test]
fn test_e2e_for_over_iter_chain() {
    // B-2026-07-11-18 — `for x in <src>.iter().{map|filter}+ { .. }`. Before
    // the fix a map/filter adaptor iterable had no `compile_for` arm and fell
    // through to the silent `_ =>`, running the body ZERO times (a silent
    // wrong answer; the interpreter iterated correctly). Now the loop routes
    // through the shared map/filter fusion with the user body as the sink.
    // Covers map, filter, filter+map, `break`, and `continue`; each sum was 0
    // pre-fix.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let v: Vec[i64] = [1, 2, 3, 4, 5, 6];\n\
                 let mut a = 0;\n\
                 for x in v.iter().map(|x| x * 10) { a = a + x; }\n\
                 println(f\"{a}\");\n\
                 let mut b = 0;\n\
                 for x in v.iter().filter(|x| x % 2 == 0) { b = b + x; }\n\
                 println(f\"{b}\");\n\
                 let mut c = 0;\n\
                 for x in v.iter().filter(|x| x > 2).map(|x| x * x) { c = c + x; }\n\
                 println(f\"{c}\");\n\
                 let mut d = 0;\n\
                 for x in v.iter().map(|x| x * 2) { if x > 8 { break; } d = d + x; }\n\
                 println(f\"{d}\");\n\
                 let mut e = 0;\n\
                 for x in v.iter().map(|x| x * 2) { if x == 4 { continue; } e = e + x; }\n\
                 println(f\"{e}\");\n\
             }",
    ) {
        // a=210, b=12, c=86 (9+16+25+36), d=2+4+6+8=20, e=42-4=38
        assert_eq!(out, "210\n12\n86\n20\n38\n");
    }
}

#[test]
fn test_e2e_iter_chain_any_all_terminal() {
    // B-2026-07-11-19 — the short-circuit `any`/`all` boolean terminals on a
    // fused iterator chain. The typechecker and interpreter already accepted
    // them; codegen had no terminal, so any chain ending in `any`/`all` hit
    // the loud "no handler" dispatch error under `karac build`. Covers
    // any-true / any-false / all-true / all-false, the empty-source edges
    // (any -> false, all -> true), and adapters before the terminal
    // (filter+any, map+all). All boolean results must match the interpreter.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let v: Vec[i64] = [1, 2, 3, 4];\n\
                 let e: Vec[i64] = [];\n\
                 println(f\"{v.iter().any(|x| x > 3)}\");\n\
                 println(f\"{v.iter().any(|x| x > 9)}\");\n\
                 println(f\"{v.iter().all(|x| x > 0)}\");\n\
                 println(f\"{v.iter().all(|x| x > 2)}\");\n\
                 println(f\"{e.iter().any(|x| x > 0)}\");\n\
                 println(f\"{e.iter().all(|x| x > 0)}\");\n\
                 println(f\"{v.iter().filter(|x| x % 2 == 0).any(|x| x > 4)}\");\n\
                 println(f\"{v.iter().map(|x| x * x).all(|x| x < 20)}\");\n\
             }",
    ) {
        assert_eq!(out, "true\nfalse\ntrue\nfalse\nfalse\ntrue\nfalse\ntrue\n");
    }
}

#[test]
fn test_e2e_iter_chain_position_terminal() {
    // `<iter-chain>.position(|x| pred) -> Option[i64]` — the 0-based index of
    // the first YIELDED element the predicate holds for, or None. Desugars to
    // a fused for-loop with a running index and a short-circuit break; the
    // index counts POST-adaptor elements. Covers a hit, a miss (None), the
    // first-element hit (index 0), filter/map adaptors before the terminal
    // (index is over the ADAPTED sequence), a String source, and a range.
    if let Some(out) = run_program(
            "fn main() {\n\
                 let v: Vec[i64] = [10, 20, 30, 40];\n\
                 match v.iter().position(|x| x == 30) { Some(i) => println(i), None => println(-1) }\n\
                 match v.iter().position(|x| x == 99) { Some(i) => println(i), None => println(-1) }\n\
                 match v.iter().position(|x| x == 10) { Some(i) => println(i), None => println(-1) }\n\
                 match v.iter().filter(|x| x % 20 == 0).position(|x| x == 40) { Some(i) => println(i), None => println(-1) }\n\
                 match v.iter().map(|x| x / 10).position(|x| x == 3) { Some(i) => println(i), None => println(-1) }\n\
                 let s: Vec[String] = [\"a\", \"bb\", \"ccc\"];\n\
                 match s.iter().position(|w| w.len() == 3) { Some(i) => println(i), None => println(-1) }\n\
                 match (0..10).position(|x| x == 7) { Some(i) => println(i), None => println(-1) }\n\
             }",
        ) {
            // 30@2; miss=-1; 10@0; filter[20,40] 40@1; map[1,2,3] 3@2; \"ccc\"@2; range 7@7
            assert_eq!(out, "2\n-1\n0\n1\n2\n2\n7\n");
        }
}

#[test]
fn test_e2e_iter_chain_find_terminal_scalar() {
    // `<iter-chain>.find(|x| pred) -> Option[T]` — the first YIELDED element
    // the predicate holds for, or None. Desugars like `position` but stores
    // the ELEMENT. SCALAR payloads only (a heap `Some(elem)` would alias the
    // borrowed source; deferred loud — see below). Covers hit / miss / first
    // / filter+find / map+find / range.
    if let Some(out) = run_program(
            "fn main() {\n\
                 let v: Vec[i64] = [10, 20, 30, 40];\n\
                 match v.iter().find(|x| x > 15) { Some(e) => println(e), None => println(-1) }\n\
                 match v.iter().find(|x| x > 99) { Some(e) => println(e), None => println(-1) }\n\
                 match v.iter().find(|x| x > 0) { Some(e) => println(e), None => println(-1) }\n\
                 match v.iter().filter(|x| x % 20 == 0).find(|x| x > 25) { Some(e) => println(e), None => println(-1) }\n\
                 match v.iter().map(|x| x / 10).find(|x| x > 2) { Some(e) => println(e), None => println(-1) }\n\
                 match (0..10).find(|x| x > 5) { Some(e) => println(e), None => println(-1) }\n\
             }",
        ) {
            // 20 (>15); -1 (miss); 10 (first>0); filter[20,40] first>25=40; map[1,2,3,4] first>2=3; range 6
            assert_eq!(out, "20\n-1\n10\n40\n3\n6\n");
        }
}

#[test]
fn test_e2e_iter_chain_next_first_yield_terminal() {
    // B-2026-07-21-2: `<iter-chain>.next() -> Option[T]` — the single-pull
    // FIRST-YIELD read on a chain receiver. A fresh chain expression is its
    // own iterator, so `next()` lowers as `find(|_| true)` (same peel,
    // scalar gate, Option[T] annotation). Covers the idiomatic first-char
    // read `s.chars().next()` (glyph-rendered inline AND let-bound — the
    // unwrap-of-Option[char] arm in `expr_is_char`), a filter hit, a filter
    // miss (None), a map chain, and a range. Stateful multi-pull on a
    // materialized binding and heap elements stay loud interp-only bails.
    if let Some(out) = run_program(
            "fn main() {\n\
                 let s = \"Zebra\".to_string();\n\
                 println(s.chars().next().unwrap());\n\
                 let c = s.chars().next().unwrap();\n\
                 println(c);\n\
                 let v: Vec[i64] = [4, 9, 1, 7];\n\
                 match v.iter().filter(|x| x > 5).next() { Some(n) => println(n), None => println(-1) }\n\
                 match v.iter().filter(|x| x > 100).next() { Some(n) => println(n), None => println(-1) }\n\
                 match v.iter().map(|x| x * 3).next() { Some(n) => println(n), None => println(-1) }\n\
                 match (3..8).next() { Some(n) => println(n), None => println(-1) }\n\
             }",
        ) {
            assert_eq!(out, "Z\nZ\n9\n-1\n12\n3\n");
        }
}

#[test]
fn test_e2e_iter_chain_next_stateful_and_heap_deferred_loud() {
    // The two out-of-scope shapes must bail LOUD (never silent-wrong):
    // stateful `next()` on a MATERIALIZED binding (would re-yield element 0
    // on every pull if inlined), and a heap element (Some(elem) would alias
    // the borrowed source — find's gate).
    let err = ir_result(
        "fn main() {\n\
                 let v: Vec[i64] = [1, 2, 3];\n\
                 let it = v.iter();\n\
                 match it.next() { Some(n) => println(n), None => println(-1) }\n\
             }\n",
    )
    .expect_err("stateful materialized next must bail loud");
    assert!(
        err.contains("materialized iterator") && err.contains("--interp"),
        "expected stateful-next deferral message, got: {err}"
    );
    let err2 = ir_result(
        "fn main() {\n\
                 let v: Vec[String] = [\"aa\", \"bb\"];\n\
                 match v.iter().next() { Some(s) => println(s), None => println(\"none\") }\n\
             }\n",
    )
    .expect_err("heap-element next must bail loud");
    assert!(
        err2.contains("Iterator.next()") && err2.contains("--interp"),
        "expected heap-next deferral message, got: {err2}"
    );
}

#[test]
fn test_e2e_iter_chain_find_heap_element_deferred_loud() {
    // A HEAP-element `find` (`Some(elem)` would alias the borrowed source
    // buffer — the reduce/max deferral) must bail LOUD naming find + pointing
    // at `--interp`, never a generic "no handler" or a silent skip.
    let err = ir_result(
            "fn main() {\n\
                 let v: Vec[String] = [\"a\", \"bb\", \"ccc\"];\n\
                 match v.iter().find(|s| s.len() == 2) { Some(e) => println(e), None => println(\"none\") }\n\
             }\n",
        )
        .expect_err("heap-element find must bail loud");
    assert!(
        err.contains("Iterator.find()") && err.contains("--interp"),
        "expected find heap-deferral message, got: {err}"
    );
}

#[test]
fn test_e2e_iter_chain_last_nth_terminals() {
    // `<iter-chain>.last() -> Option[T]` (drain, keep last) and
    // `.nth(n) -> Option[T]` (the n-th yielded element). Scalar desugar
    // mirroring `find`. Covers last (basic / empty-None / filter / map) and
    // nth (basic / out-of-bounds-None / index 0 / filter / range).
    if let Some(out) = run_program(
            "fn main() {\n\
                 let v: Vec[i64] = [1, 2, 3];\n\
                 match v.iter().last() { Some(x) => println(x), None => println(-1) }\n\
                 let e: Vec[i64] = [];\n\
                 match e.iter().last() { Some(x) => println(x), None => println(-1) }\n\
                 match v.iter().filter(|x| x % 2 == 1).last() { Some(x) => println(x), None => println(-1) }\n\
                 match v.iter().map(|x| x * 10).last() { Some(x) => println(x), None => println(-1) }\n\
                 let w: Vec[i64] = [10, 20, 30, 40];\n\
                 match w.iter().nth(2) { Some(x) => println(x), None => println(-1) }\n\
                 match w.iter().nth(9) { Some(x) => println(x), None => println(-1) }\n\
                 match w.iter().nth(0) { Some(x) => println(x), None => println(-1) }\n\
                 match w.iter().filter(|x| x % 20 == 0).nth(1) { Some(x) => println(x), None => println(-1) }\n\
                 match (0..10).nth(5) { Some(x) => println(x), None => println(-1) }\n\
             }",
        ) {
            // last: 3; empty -1; odd[1,3] last=3; map[10,20,30] last=30;
            // nth: w[2]=30; oob -1; w[0]=10; filter[20,40] nth1=40; range nth5=5
            assert_eq!(out, "3\n-1\n3\n30\n30\n-1\n10\n40\n5\n");
        }
}

#[test]
fn e2e_collect_accumulator_is_not_presized_codegen() {
    // Spike collection-capacity-presizing (S2/S3), decision recorded
    // 2026-07-09: the iterator-adaptor `.collect()` accumulator is a plain
    // `Vec.new()` and is DELIBERATELY NOT rewritten to
    // `Vec.with_capacity(<src>.len())`. Pre-sizing it was prototyped and
    // measured net-harmful under glibc — a modest ~1.16× on POD-element
    // sources but a 20–30% REGRESSION on heap-element sources (a fresh
    // full-size malloc per iteration on cold pages vs. the grow path reusing
    // the previous iteration's hot buffer; `Vec[String].filter().collect()`
    // measured 0.72×). The desugared grow loop is already within ~15% of
    // hand-tuned `with_capacity` because glibc reallocs in place and the
    // loop carries no per-element bounds check. This test locks that decision
    // in: re-introducing an unconditional accumulator pre-size would flip
    // `with_cap` into the emitted IR and must be re-measured against the heap
    // regression first (see the spike doc). `Vec.with_capacity` itself stays
    // the documented manual idiom for hand-written counted push loops, where
    // it IS a reliable ~2× win.
    let ir = ir_for(
        r#"
fn main() {
    let s: Vec[i64] = Vec[1i64, 2i64, 3i64, 4i64, 5i64];
    let doubled: Vec[i64] = s.iter().map(|n| n * 2i64).collect();
    let big: Vec[i64] = s.iter().filter(|x| x > 2i64).collect();
    println(f"{doubled[4]} {big.len()}");
}
"#,
    );
    assert!(
        !ir.contains("with_cap"),
        "the collect accumulator must stay Vec.new() (not pre-sized) — \
             pre-sizing it regresses heap-element sources; see the \
             collection-capacity-presizing spike"
    );
}

#[test]
fn e2e_iter_adaptor_collect_stateful_passthrough_codegen() {
    // B-2026-07-03-29: `<iter>....collect()` under `karac build` rejected
    // every adaptor beyond the `map`/`filter` subset landed in
    // B-2026-07-03-25. This extends the desugar to the stateful
    // element-type-preserving passthrough family — `take`, `skip`,
    // `step_by`, `take_while`, `skip_while`, `inspect` — each lowered to its
    // for-loop equivalent with a pre-loop state var (counter / bound count)
    // and, for the short-circuiting `take`/`take_while`, an in-body `break`
    // at the adaptor's position. The break sits AFTER the upstream stages,
    // matching the interpreter's lazy step order (iter_eval.rs): upstream
    // side effects (a preceding `inspect`) fire on the element that trips
    // exhaustion too — hence `inspect(...).take(2)` prints `see1 see2 see3`
    // on BOTH surfaces, not `see1 see2`. Exercises each adaptor, mixed
    // chains (`skip().take().map()`, `filter().take()`), the interpreter-
    // matching side-effect count, and a heap `Vec[String]` source whose
    // element method (`.len()`) is called after a leading count adaptor
    // (the loop var must inherit the source element type — see
    // `elem_name`).
    if let Some(out) = run_program(
        r#"
fn main() {
    let s: Vec[i64] = Vec[1i64, 2i64, 3i64, 4i64, 5i64, 6i64];
    let tk: Vec[i64] = s.iter().take(2i64).collect();
    println(f"{tk.len()} {tk[0]} {tk[1]}");
    let sk: Vec[i64] = s.iter().skip(4i64).collect();
    println(f"{sk.len()} {sk[0]} {sk[1]}");
    let sb: Vec[i64] = s.iter().step_by(2i64).collect();
    println(f"{sb.len()} {sb[0]} {sb[1]} {sb[2]}");
    let tw: Vec[i64] = s.iter().take_while(|x| x < 4i64).collect();
    println(f"{tw.len()} {tw[0]} {tw[2]}");
    let sw: Vec[i64] = s.iter().skip_while(|x| x < 4i64).collect();
    println(f"{sw.len()} {sw[0]} {sw[2]}");
    let mx: Vec[i64] = s.iter().skip(1i64).take(2i64).map(|n| n * 10i64).collect();
    println(f"{mx.len()} {mx[0]} {mx[1]}");
    let ft: Vec[i64] = s.iter().filter(|x| x > 1i64).take(2i64).collect();
    println(f"{ft.len()} {ft[0]} {ft[1]}");
    let insp: Vec[i64] = s.iter().inspect(|x| println(f"see{x}")).take(2i64).collect();
    println(f"{insp.len()}");
    let words: Vec[String] = Vec["apple".to_string(), "berry".to_string(), "fig".to_string(), "kiwi".to_string()];
    let lens: Vec[i64] = words.iter().skip(1i64).map(|w| w.len()).collect();
    println(f"{lens.len()} {lens[0]} {lens[2]}");
}
"#,
    ) {
        assert_eq!(
            out,
            "2 1 2\n2 5 6\n3 1 3 5\n3 1 3\n3 4 6\n2 20 30\n2 2 3\nsee1\nsee2\nsee3\n2\n3 5 4\n"
        );
    }
}

#[test]
fn e2e_iter_adaptor_identity_collect_to_vec_codegen() {
    // B-2026-07-04-2 sub-part 4: a PLAIN `<src>.iter().collect()` with NO
    // `map`/`filter`/... adaptor (an identity collect) fell through to the
    // loud dispatch-fail under `karac build` ("no handler for method
    // 'collect' on non-identifier receiver"). The fix injects a synthetic
    // identity `map(|x| x)` when the adaptor chain is empty over an `.iter()`
    // source, so the shared pipeline lowers it exactly like the verified
    // `<src>.iter().map(|x| x).collect()` shape — a fresh Vec of element
    // CLONES. `.iter()` borrows, so the source Vec SURVIVES (asserted via
    // `a.len()`/`s.len()` after the collect), and the two Vecs own
    // independent buffers. Exercises POD + heap elements over a named-local
    // source AND a fresh-temp source (`build().iter()`, whose heap must be
    // freed after cloning — leak-checked by the memory_sanitizer sibling
    // `asan_b04_2_identity_collect_no_leak`).
    if let Some(out) = run_program(
        r#"
fn build() -> Vec[String] {
    let mut v: Vec[String] = Vec.new();
    v.push("alpha".to_string());
    v.push("beta".to_string());
    v.push("gamma".to_string());
    v
}
fn main() {
    let a: Vec[i64] = Vec[1i64, 2i64, 3i64, 4i64];
    let b: Vec[i64] = a.iter().collect();
    println(f"{b.len()} {a.len()} {b[0]} {b[3]}");
    let s: Vec[String] = Vec["one".to_string(), "two".to_string()];
    let t: Vec[String] = s.iter().collect();
    println(f"{t.len()} {s.len()} {t[0]}{t[1]}");
    let ft: Vec[String] = build().iter().collect();
    println(f"{ft.len()} {ft[0]}");
}
"#,
    ) {
        assert_eq!(out, "4 4 1 4\n2 2 onetwo\n3 alpha\n");
    }
}

#[test]
fn e2e_iter_adaptor_range_identity_collect_codegen() {
    // B-2026-07-04-2 sub-part 4 (range half): a BOUNDED integer range
    // collected with NO adaptor — `(a..b).collect()` / `(a..=b).collect()`
    // — fell through to the loud dispatch-fail under `karac build`, though
    // `karac run` handled it. The fix accepts a `Range { start: Some, end:
    // Some }` as an identity-collect source (alongside `.iter()`): `for x in
    // a..b` yields owned POD integers, so the synthetic `map(|x| x)` is a
    // plain copy with no source to alias. Covers exclusive, inclusive,
    // variable-bound, and empty ranges; an unbounded range is a typecheck
    // error (no `collect`) and never reaches here.
    if let Some(out) = run_program(
        r#"
fn main() {
    let a: Vec[i64] = (0i64..5i64).collect();
    println(f"{a.len()} {a[0]} {a[4]}");
    let b: Vec[i64] = (1i64..=3i64).collect();
    println(f"{b.len()} {b[0]} {b[2]}");
    let n: i64 = 4i64;
    let c: Vec[i64] = (0i64..n).collect();
    println(f"{c.len()} {c[0]} {c[3]}");
    let e: Vec[i64] = (5i64..5i64).collect();
    println(f"{e.len()}");
}
"#,
    ) {
        assert_eq!(out, "5 0 4\n3 1 3\n4 0 3\n0\n");
    }
}

#[test]
fn e2e_iter_adaptor_into_iter_identity_collect_codegen() {
    // B-2026-07-04-2 sub-part 4 (into_iter half): `<local>.into_iter()
    // .collect()` with no adaptor fell through to the loud dispatch-fail
    // under `karac build`, though `karac run` handled it. Kāra's
    // `into_iter().collect()` is NON-consuming here (the ownership checker
    // leaves the source valid, and `for x in <src>.into_iter()` already
    // lowers like `.iter()`), so it collects a fresh Vec of element clones
    // and the source SURVIVES (asserted via `v.len()`). Covers POD and heap;
    // leak-checked by `asan_b04_2_into_iter_identity_collect_no_leak`.
    if let Some(out) = run_program(
        r#"
fn main() {
    let v: Vec[i64] = Vec[1i64, 2i64, 3i64];
    let r: Vec[i64] = v.into_iter().collect();
    println(f"{r.len()} {r[0]} {r[2]} {v.len()}");
    let s: Vec[String] = Vec["aa".to_string(), "bb".to_string()];
    let t: Vec[String] = s.into_iter().collect();
    println(f"{t.len()} {t[0]}{t[1]} {s.len()}");
}
"#,
    ) {
        assert_eq!(out, "3 1 3 3\n2 aabb 2\n");
    }
}

#[test]
fn e2e_iter_adaptor_chain_identity_collect_codegen() {
    // B-2026-07-04-2 sub-part 1 (chain half): `A.chain(B).collect()` over two
    // plain identity sources (`.iter()` / bounded range) fell through to the
    // loud dispatch-fail under `karac build`, though `karac run` handled it.
    // The fix emits the identity-collect loop once per source into a shared
    // accumulator — the same clone semantics as a single identity collect,
    // so both borrowed sources SURVIVE (asserted via `a.len()`/`b.len()`).
    // Exercises POD, heap (`Vec[String]`), and a range+iter mix. A chain side
    // carrying its OWN adaptor still bails (loud dispatch-fail, no
    // miscompile).
    if let Some(out) = run_program(
        r#"
fn main() {
    let a: Vec[i64] = Vec[1i64, 2i64];
    let b: Vec[i64] = Vec[3i64, 4i64, 5i64];
    let r: Vec[i64] = a.iter().chain(b.iter()).collect();
    println(f"{r.len()} {r[0]} {r[4]} {a.len()} {b.len()}");
    let s: Vec[String] = Vec["aa".to_string(), "bb".to_string()];
    let t: Vec[String] = Vec["cc".to_string()];
    let u: Vec[String] = s.iter().chain(t.iter()).collect();
    println(f"{u.len()} {u[0]}{u[2]} {s.len()} {t.len()}");
    let c: Vec[i64] = (0i64..3i64).chain(b.iter()).collect();
    println(f"{c.len()} {c[0]} {c[2]} {c[3]} {c[4]}");
}
"#,
    ) {
        assert_eq!(out, "5 1 5 2 3\n3 aacc 2 1\n6 0 2 3 4\n");
    }
}

#[test]
fn e2e_iter_adaptor_zip_identity_collect_codegen() {
    // B-2026-07-04-2 sub-part 1 (zip half): `A.iter().zip(B.iter()).collect()`
    // pairs the two sources element-wise into a `Vec[(EA, EB)]`, stopping at
    // the shorter — it fell through to the loud dispatch-fail under `karac
    // build`, though `karac run` handled it. The fix emits an index loop
    // `while i < min { acc.push((A[i], B[i])); i += 1 }`; `A[i]`/`B[i]` copy,
    // so both POD sources SURVIVE (asserted via `a.len()`/`b.len()`).
    // Exercises unequal lengths and a min-length cutoff. Heap-bearing pairs
    // are now supported too (see `e2e_iter_adaptor_zip_heap_collect_codegen`
    // — the pushed tuple deep-clones each named-Vec heap index-read); a
    // downstream adaptor after `zip` (`zip().map(…)`) still bails (loud
    // dispatch-fail, no miscompile).
    if let Some(out) = run_program(
        r#"
fn main() {
    let a: Vec[i64] = Vec[1i64, 2i64, 3i64];
    let b: Vec[i64] = Vec[10i64, 20i64];
    let r: Vec[(i64, i64)] = a.iter().zip(b.iter()).collect();
    println(f"{r.len()} {r[0].0} {r[0].1} {r[1].0} {r[1].1} {a.len()} {b.len()}");
    let c: Vec[i64] = Vec[7i64, 8i64];
    let d: Vec[i64] = Vec[70i64, 80i64, 90i64];
    let e: Vec[(i64, i64)] = c.iter().zip(d.iter()).collect();
    println(f"{e.len()} {e[1].0} {e[1].1}");
}
"#,
    ) {
        assert_eq!(out, "2 1 10 2 20 3 2\n2 8 80\n");
    }
}

/// B-2026-07-04-2 sub-part 1 (heap-zip leg): `A.iter().zip(B.iter())
/// .collect()` over `Vec[String]` sources → `Vec[(String, String)]`. The
/// pushed tuple `(A[i], B[i])` now deep-clones each named-Vec heap
/// index-read (`compile_tuple` → `maybe_defensive_copy_param_arg` →
/// `clone_owned_vec_index_element`), so the borrowed sources survive and the
/// result owns independent buffers — previously POD-gated because the raw
/// index-read aliased the source buffer (double-free). Leak/double-free
/// coverage is the ASAN twin `asan_b04_2_zip_heap_collect_no_leak`.
#[test]
fn e2e_iter_adaptor_zip_heap_collect_codegen() {
    if let Some(out) = run_program(
        r#"
fn main() {
    let a: Vec[String] = Vec["alpha".to_string(), "bravo".to_string(), "charlie".to_string()];
    let b: Vec[String] = Vec["one".to_string(), "two".to_string()];
    let z: Vec[(String, String)] = a.iter().zip(b.iter()).collect();
    println(f"{z.len()} {z[0].0} {z[0].1} {z[1].0} {z[1].1} {a.len()} {b.len()}");
}
"#,
    ) {
        // min(3,2)=2 pairs; both sources survive (len 3 and 2).
        assert_eq!(out, "2 alpha one bravo two 3 2\n");
    }
}

/// B-2026-07-14-7: for-loops over unlowered iterator adaptors silently
/// SKIPPED the loop body in codegen (ran zero times) — a silent wrong
/// answer. They now bail LOUD (matching the codebase policy for unsupported
/// zip chains). Covers `enumerate` (single-var binding), `zip`, and the
/// `skip`/`take`/`chain` family; the handled forms (`map`/`filter` fusion,
/// `enumerate` 2-tuple destructure, range `step_by`) are unaffected
/// (asserted by the positive tests below).
#[test]
fn e2e_for_unlowered_iter_adaptors_bail_loud() {
    let cases: &[(&str, &str)] = &[
        (
            // The scalar single-var shape is now LOWERED
            // (test_e2e_for_enumerate_single_var), so the bail contract
            // covers a still-unsupported enumerate shape: a HEAP (String)
            // element, which the tuple materialization would double-own.
            "enumerate",
            "let a: Vec[String] = Vec[f\"x\", f\"y\"];\n\
                 let mut s = 0i64;\n\
                 for p in a.iter().enumerate() { s = s + p.0; }\n\
                 println(f\"{s}\");",
        ),
        (
            // Scalar single-var zip is now LOWERED
            // (test_e2e_for_zip_single_var, B-2026-07-15-10), so the bail
            // contract covers a still-unsupported single-var zip shape: HEAP
            // (String) elements, whose tuple materialization would copy the
            // element headers into the pair with no borrow-marking (the
            // two-sub-pattern destructure path handles heap; a single-var
            // heap binding still bails). The interpreter handles it.
            "zip (single-var binding, heap elements)",
            "let a: Vec[String] = Vec[f\"x\", f\"y\"];\n\
                 let b: Vec[i64] = Vec[3i64, 4i64];\n\
                 let mut s = 0i64;\n\
                 for p in a.iter().zip(b.iter()) { s = s + p.1; }\n\
                 println(f\"{s}\");",
        ),
        (
            // `flat_map` for-loops are now LOWERED via the nested-loop
            // desugar (test_e2e_for_flat_map) — INCLUDING labeled loops
            // (the label lands on the outer loop; labeled continues are
            // renamed to the inner loop). The bail contract covers a shape
            // the desugar still declines: a DESTRUCTURING closure param.
            // The interpreter handles it.
            "flat_map (destructuring closure param)",
            "let ps: Vec[(i64, i64)] = Vec[(1i64, 2i64), (3i64, 4i64)];\n\
                 let vv: Vec[Vec[i64]] = Vec[Vec[1i64]];\n\
                 let mut s = 0i64;\n\
                 for y in ps.iter().flat_map(|(a, b)| vv[0].iter()) { s = s + y; }\n\
                 println(f\"{s}\");",
        ),
        (
            // B-2026-07-14-21: `map`/`filter` ARE lowered, but a chain the
            // fused peel REJECTS (here: a destructuring closure param over
            // tuple elements) used to fall through to the silent
            // zero-iteration skip — interp 10, JIT 0. Must bail loud like
            // every other adaptor shape.
            "map (peel-rejected shape)",
            "let ps: Vec[(i64, i64)] = Vec[(1i64, 2i64), (3i64, 4i64)];\n\
                 let mut s = 0i64;\n\
                 for x in ps.iter().map(|(a, b)| a + b) { s = s + x; }\n\
                 println(f\"{s}\");",
        ),
        (
            // Heap-element chain is now LOWERED (loop-borrow marking,
            // test_e2e_for_adaptor_tail_complete); the bail contract
            // covers a chain whose side is NOT a named Vec (a range).
            "chain (range side)",
            "let v: Vec[i64] = Vec[1i64];\n\
                 let mut s = 0i64;\n\
                 for x in v.iter().chain(0..3) { s = s + x; }\n\
                 println(f\"{s}\");",
        ),
    ];
    for (method, body) in cases {
        let src = format!("fn main() {{\n{body}\n}}\n");
        // `ir_result` returns Err on a loud bail, Ok if it compiled. A
        // successful compile here is the silent-skip regression.
        assert!(
            ir_result(&src).is_err(),
            "for-loop over `.{method}()` must bail loud (Err), not compile+silently-skip"
        );
    }
}

/// B-2026-07-14-9: `for x in xs.iter_mut()` (mutable iteration) is not yet
/// lowered in codegen and was SILENTLY skipping the loop body — `for x in
/// v.iter_mut() { *x = *x * 10 }` left `v` unmutated with no error (a
/// silent wrong answer; `karac run`/interp fails loudly). Now bails loud in
/// codegen too, pointing at the index-loop workaround.
#[test]
fn e2e_for_iter_mut_bails_loud() {
    // B-2026-07-14-10: the SCALAR-element named-Vec shape is now LOWERED
    // (see `test_e2e_for_iter_mut_scalar`), so the loud-bail contract only
    // covers the still-unsupported shapes — here a HEAP (String) element,
    // which a mut-ref write would need to drop-old-payload for. Must bail
    // LOUD (never the silent zero-iteration skip of B-2026-07-14-9) and
    // point at `--interp`, which handles every shape.
    let err = ir_result(
        "fn main() {\n\
                 let mut v: Vec[String] = Vec[f\"a\", f\"b\"];\n\
                 for x in v.iter_mut() { *x = f\"z\"; }\n\
                 println(f\"{v.len()}\");\n\
             }\n",
    )
    .expect_err("heap-element iter_mut for-loop must bail loud, not silently skip");
    assert!(
        err.contains("iter_mut") && err.contains("not yet"),
        "expected iter_mut loud-bail message, got: {err}"
    );
}

/// Companion to the above using `expect_err` so the actual bail message is
/// asserted (names the adaptor + points at `--interp`). Uses `cycle` — a
/// genuinely-unlowered adaptor (`chain` over scalar Vecs is now lowered,
/// test_e2e_for_chain_two_vecs).
#[test]
fn e2e_for_unlowered_iter_adaptor_message() {
    // Every ADAPTOR now has a lowering (`peekable` peels as identity;
    // `cycle`/`scan`/… have desugars) — the message contract now rides on
    // a still-declined SHAPE: `scan`'s conditional-`None` early-stop body
    // (the desugar only handles direct `Some((new, out))` bodies; the
    // Option-typed early-stop needs typed match reconstruction on
    // synthetic AST — deferred with rationale in the ledger).
    let err = ir_result(
            "fn main() {\n\
                 let v: Vec[i64] = Vec[1i64, 2i64, 3i64, 4i64];\n\
                 let mut s = 0i64;\n\
                 for x in v.iter().scan(0i64, |acc, q| if q > 2i64 { None } else { Some((acc + q, acc + q)) }) { s = s + x; }\n\
                 println(f\"{s}\");\n\
             }\n",
        )
        .expect_err("early-stop scan for-loop must bail loud, not silently skip");
    assert!(
        err.contains("`.scan()`") && err.contains("not yet lowered"),
        "expected scan loud-bail message, got: {err}"
    );
}

/// B-2026-07-18-41 — `Iterator.rev()` is interpreter-only; codegen must bail
/// LOUD (naming rev + pointing at `--interp`) for every chain shape that
/// includes it — the terminal-over-rev (`v.iter().rev().sum()`), the
/// adaptor-over-rev (`v.iter().rev().collect()`), rev-over-adaptor
/// (`v.iter().map(f).rev()`), and the `for x in v.iter().rev()` loop — never
/// a silent empty iteration (the for-loop's `_ =>` fall-through hazard).
#[test]
fn e2e_iter_rev_bound_vec_reverse_iterate() {
    // B-2026-07-18-41 codegen leg — the reverse-iterate lowering for a
    // `.rev()` chain over a BOUND `Vec` identifier. Strips `.rev()`, sets a
    // one-shot signal, and re-dispatches; the base `compile_for_vec_var` then
    // walks `len-1-i` (no allocation — leak-free). Composes with order-
    // independent adaptors on BOTH sides and every terminal / the for-loop.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let v: Vec[i64] = [1, 2, 3, 4];\n\
                 for x in v.iter().rev() { println(x); }\n\
                 let a: Vec[i64] = v.iter().rev().collect();\n\
                 println(a.get(0));\n\
                 println(v.iter().rev().fold(0, |acc, x| acc * 10 + x));\n\
                 let s: i64 = v.iter().rev().sum();\n\
                 println(s);\n\
                 println(v.iter().rev().count());\n\
                 let b: Vec[i64] = v.iter().map(|x| x * 10).rev().collect();\n\
                 println(b.get(0));\n\
                 let c: Vec[i64] = v.iter().rev().filter(|x| x % 2 == 0).collect();\n\
                 println(c.get(0));\n\
                 let sv: Vec[String] = [\"a\", \"b\", \"c\"];\n\
                 for w in sv.iter().rev() { println(w); }\n\
             }",
    ) {
        // for: 4 3 2 1; a.get(0)=Some(4); fold=4321; sum=10; count=4;
        // b (map*10 then rev).get(0)=Some(40);
        // c (rev [4,3,2,1] then even).get(0)=Some(4); string for: c b a
        assert_eq!(
            out,
            "4\n3\n2\n1\nSome(4)\n4321\n10\n4\nSome(40)\nSome(4)\nc\nb\na\n"
        );
    }
}

/// B-2026-07-18-41 range leg — the reverse-iterate lowering extended to a
/// BARE range base: `for i in (a..b).rev()` / `(a..=b).rev()` and the
/// terminals that lower to `compile_for_range` (`collect`/`fold`/`sum`/
/// `count`). The descending loop (`init = end-1`/`end`, `i >= start`,
/// decrement) visits the SAME value set as forward, so bounds-check elision
/// on `v[i]` stays valid; the one-shot `pending_reverse_iter` signal is
/// nesting-safe. Byte-identical to the interpreter
/// (`test_iter_rev_range_interpreter`); no allocation, so leak-free.
#[test]
fn e2e_iter_rev_range_reverse_iterate() {
    if let Some(out) = run_program(
        "fn main() {\n\
                 for i in (0..5).rev() { print(i); }\n\
                 println(\"\");\n\
                 for i in (1..=4).rev() { print(i); }\n\
                 println(\"\");\n\
                 for i in (5..5).rev() { print(i); }\n\
                 println(\"E\");\n\
                 let v: Vec[i64] = [10, 20, 30, 40];\n\
                 let mut s: i64 = 0;\n\
                 for i in (0..4).rev() { s = s + v[i]; }\n\
                 println(s);\n\
                 let w: Vec[i64] = (0..3).rev().collect();\n\
                 println(w.get(0));\n\
                 println((0..4).rev().fold(0, |acc, x| acc * 10 + x));\n\
                 println((0..4).rev().sum());\n\
                 println((1..=3).rev().count());\n\
                 for a in (0..2).rev() { for b in (0..2).rev() { print(f\"{a}{b} \"); } }\n\
                 println(\"\");\n\
             }",
    ) {
        // (0..5).rev)=43210; (1..=4).rev=4321; empty→E; vec-sum(all)=100;
        // collect.get(0)=Some(2); fold rev[3,2,1,0]=3210; sum=6; count=3;
        // nested rev: 11 10 01 00
        assert_eq!(
            out,
            "43210\n4321\nE\n100\nSome(2)\n3210\n6\n3\n11 10 01 00 \n"
        );
    }
}

#[test]
fn e2e_iter_rev_deferred_loud_message() {
    // The reverse-iterate lowering covers a bound-Vec base with order-
    // independent steps (above) and a BARE range base (for-loop + collect /
    // fold / sum / count terminals, `e2e_iter_rev_range_reverse_iterate`).
    // Shapes OUTSIDE those — a TEMP/literal source, or a POSITIONAL adaptor
    // (`enumerate`/`take`/`step_by`) or `map` combined with `rev` over a
    // range (semantically NOT a plain reverse, and routed through the fused
    // chain rather than `compile_for_range`) — must still bail LOUD naming
    // rev + `--interp`, never silently skip or forward-iterate.
    for src in [
            "fn main() { let n: i64 = vec![1,2,3].iter().rev().sum(); println(n); }\n",
            "fn main() { let v = vec![1,2,3]; for (i, x) in v.iter().enumerate().rev() { println(x); } }\n",
            "fn main() { let v = vec![1,2,3,4]; let w: Vec[i64] = v.iter().take(2).rev().collect(); println(w.get(0)); }\n",
            "fn main() { for i in (0..10).step_by(2).rev() { println(i); } }\n",
            "fn main() { for i in (0..5).map(|x: i64| x * 2).rev() { println(i); } }\n",
        ] {
            let err = ir_result(src).expect_err("unsupported rev chain must bail loud");
            assert!(
                err.contains("Iterator.rev()") && err.contains("--interp"),
                "expected rev loud-bail message, got: {err}"
            );
        }
}

/// B-2026-07-19-12 slice 2 — the `for x in <recv>.flatten()` nested-loop
/// codegen (outer loop binds each inner iterable, inner loop yields its
/// elements; ≡ `flat_map(|x| x)`). Break/continue thread through correctly
/// (unlabeled break exits the whole flat sequence). Byte-identical to the
/// interpreter (`test_iter_flatten_interpreter`); leak-freedom for heap
/// elements is gated in `tests/memory_sanitizer.rs`.
#[test]
fn e2e_iter_flatten_for_loop() {
    if let Some(out) = run_program(
        "fn main() {\n\
                 let n: Vec[Vec[i64]] = [[1, 2, 3], [4, 5], [6]];\n\
                 for x in n.iter().flatten() { print(x); }\n\
                 println(\"\");\n\
                 let words: Vec[Vec[String]] = [[\"a\", \"b\"], [\"c\"], [\"d\", \"e\"]];\n\
                 for w in words.iter().flatten() { print(w); }\n\
                 println(\"\");\n\
                 let e: Vec[Vec[i64]] = [[], [1], [], [2, 3], []];\n\
                 let mut sum: i64 = 0;\n\
                 for x in e.iter().flatten() { sum = sum + x; }\n\
                 println(sum);\n\
                 let mut first_big: i64 = -1;\n\
                 for x in n.iter().flatten() { if x >= 4 { first_big = x; break; } }\n\
                 println(first_big);\n\
                 let mut odds: i64 = 0;\n\
                 for x in n.iter().flatten() { if x % 2 == 0 { continue; } odds = odds + x; }\n\
                 println(odds);\n\
             }",
    ) {
        // flat 123456; strings abcde; empty-inners sum 1+2+3=6; break at 4;
        // odds 1+3+5=9
        assert_eq!(out, "123456\nabcde\n6\n4\n9\n");
    }
}

/// B-2026-07-19-12 slice 3 — the fused TERMINALS over flatten
/// (`collect`/`sum`/`count`/`fold`, plus adaptor-then-terminal like
/// `flatten().map(f).collect()` / `flatten().filter(p).sum()`) now lower via
/// the flatten-aware structural base peel (`peel_base_is_structural_adaptor`)
/// and the dedicated `try_compile_flatten_collect`. Byte-identical to the
/// interpreter (`test_iter_flatten_interpreter`).
#[test]
fn e2e_iter_flatten_terminals() {
    if let Some(out) = run_program(
        "fn main() {\n\
                 let n: Vec[Vec[i64]] = [[1, 2], [3, 4], [5]];\n\
                 let flat: Vec[i64] = n.iter().flatten().collect();\n\
                 println(flat.get(0));\n\
                 println(flat.get(4));\n\
                 println(n.iter().flatten().sum());\n\
                 println(n.iter().flatten().count());\n\
                 println(n.iter().flatten().fold(0, |acc, x| acc * 10 + x));\n\
                 let d: Vec[i64] = n.iter().flatten().map(|x| x * 2).collect();\n\
                 println(d.get(2));\n\
                 println(n.iter().flatten().filter(|x| x % 2 == 1).sum());\n\
             }",
    ) {
        // collect .get(0)=Some(1) .get(4)=Some(5); sum 15; count 5;
        // fold 12345; map*2 .get(2)=Some(6); odd-filter sum 1+3+5=9
        assert_eq!(out, "Some(1)\nSome(5)\n15\n5\n12345\nSome(6)\n9\n");
    }
}

/// A BARE `Iterator.flatten()` VALUE — materialized into a `let` binding that
/// codegen compiles eagerly rather than inlining — has no codegen
/// representation and must bail LOUD (naming flatten + `--interp`), never a
/// silent skip. The driven shapes (for-loop, terminals) are covered above.
#[test]
fn e2e_iter_flatten_bare_value_deferred_loud() {
    let src = "fn main() { let v: Vec[Vec[i64]] = [[1,2],[3]]; \
                   let it = v.iter().flatten(); for x in it { println(x); } }\n";
    let err = ir_result(src).expect_err("bare flatten value must bail loud");
    assert!(
        err.contains("Iterator.flatten()") && err.contains("--interp"),
        "expected flatten loud-bail message, got: {err}"
    );
}

/// B-2026-07-04-2 sub-part 1 (chunks/windows): `<base>.iter().chunks(n)
/// .collect()` groups the source into consecutive `Vec[E]` slices of length
/// `n` (last chunk short); `.windows(n)` yields every overlapping length-`n`
/// slice. Both were gated to the loud dispatch-fail. The fix builds each
/// chunk as a FRESH temp via an inline block tail-return
/// (`acc.push({ let mut c = Vec.new(); ...; c })`) — the `mk()`-fresh-temp
/// pattern inlined — so there is no consume-then-reuse loop binding (which
/// would need the ownership RC fallback the post-ownership synthetic AST
/// can't emit) and no in-place fill of a growing accumulator (which
/// double-freed on realloc). The borrowed base survives; heap coverage is
/// the ASAN twin `asan_b04_2_chunks_heap_collect_no_leak`.

#[test]
fn e2e_iter_adaptor_chunks_windows_collect_codegen() {
    if let Some(out) = run_program(
        r#"
fn main() {
    let v: Vec[i64] = Vec[1i64, 2i64, 3i64, 4i64, 5i64];
    let cs: Vec[Vec[i64]] = v.iter().chunks(2i64).collect();
    // 3 chunks: [1,2] [3,4] [5]
    println(f"{cs.len()} {cs[0i64].len()} {cs[0i64][0i64]} {cs[0i64][1i64]} {cs[2i64].len()} {cs[2i64][0i64]} {v.len()}");
    let w: Vec[i64] = Vec[1i64, 2i64, 3i64, 4i64];
    let ws: Vec[Vec[i64]] = w.iter().windows(2i64).collect();
    // 3 windows: [1,2] [2,3] [3,4]
    println(f"{ws.len()} {ws[0i64][0i64]} {ws[0i64][1i64]} {ws[2i64][0i64]} {ws[2i64][1i64]} {w.len()}");
}
"#,
    ) {
        assert_eq!(out, "3 2 1 2 1 5 5\n3 1 2 3 4 4\n");
    }
}

/// B-2026-07-05-1 (resolved): a heap `enumerate` tuple `(i64, String)`
/// threaded through downstream stages that whole-COPY it — a differing-param
/// stage (`enumerate().filter(|p| …).inspect(|q| …)`), a non-terminal
/// identity `map` (`enumerate().map(|p| p).filter(|q| …)`), or a re-tuple
/// `map` (`.map(|p| (p.0, p.1))`) — used to bail to the loud dispatch-fail
/// (the whole-tuple copy aliased the heap buffer). The copies are now sound:
/// the desugar threads the tuple as a bare identifier / a re-tuple of its
/// fields, so each `let q = <id>` is an ordinary MOVE the source-suppression
/// retires. `karac run` and `karac build` now agree. ASAN twins:
/// asan_b05_1_*.
#[test]
fn e2e_iter_adaptor_enumerate_heap_tuple_downstream_copy_codegen() {
    if let Some(out) = run_program(
        r#"
fn main() {
    let v: Vec[String] = Vec["alpha".to_string(), "bravo".to_string()];
    // non-terminal identity map, then filter with a DIFFERENT param name
    let a: Vec[(i64, String)] = v.iter().enumerate().map(|p| p).filter(|q| q.0 >= 0i64).collect();
    println(f"{a.len()} {a[0i64].0} {a[1i64].0}");
    // filter then inspect (differing param), terminal collect
    let b: Vec[(i64, String)] = v.iter().enumerate().filter(|p| p.0 >= 0i64).collect();
    println(f"{b.len()} {b[1i64].0}");
    // re-tuple map body
    let c: Vec[(i64, String)] = v.iter().enumerate().map(|p| (p.0, p.1)).filter(|q| q.0 < 9i64).collect();
    println(f"{c.len()} {c[0i64].0} {c[1i64].0}");
}
"#,
    ) {
        assert_eq!(out, "2 0 1\n2 1\n2 0 1\n");
    }
}

/// B-2026-07-04-2 sub-part 1 (chain adaptor-carrying side): a `chain` whose
/// side carries its own adaptor (`a.iter().map(f).chain(b.iter())`, or the
/// arg side `a.chain(b.iter().filter(g))`) recursively collects each side
/// through the full pipeline and merges into a shared accumulator — it used
/// to bail to the loud dispatch-fail (only identity+identity chains lowered).
/// Both sources survive; an unsupported adaptor on a side still bails via the
/// recursive compile. ASAN twin: asan_b04_2_chain_adaptor_side_heap_no_leak.
#[test]
fn e2e_iter_adaptor_chain_pipeline_collect_codegen() {
    if let Some(out) = run_program(
        r#"
fn main() {
    let a: Vec[i64] = Vec[1i64, 2i64, 3i64];
    let b: Vec[i64] = Vec[4i64, 5i64];
    // receiver side carries a map
    let r: Vec[i64] = a.iter().map(|x| x * 2i64).chain(b.iter()).collect();
    println(f"{r.len()} {r[0i64]} {r[2i64]} {r[3i64]} {a.len()} {b.len()}");
    // arg side carries a filter
    let s: Vec[i64] = a.iter().chain(b.iter().filter(|y| y > 4i64)).collect();
    println(f"{s.len()} {s[3i64]}");
}
"#,
    ) {
        // r = [2,4,6, 4,5]; s = [1,2,3, 5]
        assert_eq!(out, "5 2 6 4 3 2\n4 5\n");
    }
}

/// B-2026-07-04-2 sub-part 1 (zip adaptor-carrying side): a `zip` whose side
/// carries its own adaptor (`a.iter().map(f).zip(b.iter())`, or the arg side
/// `a.iter().zip(b.iter().filter(g))`) pre-collects each side to a typed temp
/// and reuses the identity zip on the temps — it used to bail (only
/// iter+iter zips lowered). Both sources survive. ASAN twin:
/// asan_b04_2_zip_adaptor_side_heap_no_leak.
#[test]
fn e2e_iter_adaptor_zip_pipeline_collect_codegen() {
    if let Some(out) = run_program(
        r#"
fn main() {
    let a: Vec[i64] = Vec[1i64, 2i64, 3i64];
    let b: Vec[i64] = Vec[10i64, 20i64];
    // receiver side carries a map; min(3,2)=2 pairs
    let r: Vec[(i64, i64)] = a.iter().map(|x| x * 2i64).zip(b.iter()).collect();
    println(f"{r.len()} {r[0i64].0} {r[0i64].1} {r[1i64].0} {r[1i64].1} {a.len()} {b.len()}");
    // arg side carries a filter
    let s: Vec[(i64, i64)] = a.iter().zip(b.iter().filter(|y| y > 5i64)).collect();
    println(f"{s.len()} {s[0i64].0} {s[0i64].1} {s[1i64].1}");
}
"#,
    ) {
        // r=[(2,10),(4,20)]; b.filter(>5)=[10,20], a=[1,2,3] -> s=[(1,10),(2,20)]
        assert_eq!(out, "2 2 10 4 20 3 2\n2 1 10 20\n");
    }
}

/// B-2026-07-04-2 sub-part 1 (cycle+take): `<src>.cycle().take(n).collect()`
/// repeats the identity source until `n` elements are collected — it used to
/// bail. Lowered as a repeat-until-`n` loop with an empty-source guard. A
/// BARE `cycle()` (no `take`) is unbounded and stays a loud dispatch-fail.
/// ASAN twin: asan_b04_2_cycle_take_heap_no_leak.
#[test]
fn e2e_iter_adaptor_cycle_take_collect_codegen() {
    if let Some(out) = run_program(
        r#"
fn main() {
    let v: Vec[i64] = Vec[1i64, 2i64];
    let r: Vec[i64] = v.iter().cycle().take(5i64).collect();
    println(f"{r.len()} {r[0i64]} {r[1i64]} {r[2i64]} {r[3i64]} {r[4i64]} {v.len()}");
    // range source
    let s: Vec[i64] = (0i64..3i64).cycle().take(7i64).collect();
    println(f"{s.len()} {s[0i64]} {s[3i64]} {s[6i64]}");
}
"#,
    ) {
        // r=[1,2,1,2,1]; s=[0,1,2,0,1,2,0]
        assert_eq!(out, "5 1 2 1 2 1 2\n7 0 0 0\n");
    }
}

/// B-2026-07-04-2 sub-part 1 (scan): `v.iter().scan(init, |acc, x|
/// Some((new_acc, output))).collect()` threads a running accumulator and
/// collects each output — it used to bail. Lowered by extracting the inner
/// tuple of the `Some(..)` body directly (no Option pattern-match / is_none
/// dispatch, which synthetic post-typecheck AST can't resolve); a
/// conditionally-`None` body bails. ASAN twin: asan_b04_2_scan_heap_no_leak.
#[test]
fn e2e_iter_adaptor_scan_collect_codegen() {
    if let Some(out) = run_program(
        r#"
fn main() {
    let v: Vec[i64] = Vec[1i64, 2i64, 3i64, 4i64];
    // running sum
    let r: Vec[i64] = v.iter().scan(0i64, |acc, x| Some((acc + x, acc + x))).collect();
    println(f"{r.len()} {r[0i64]} {r[1i64]} {r[2i64]} {r[3i64]}");
    // running product, output distinct from accumulator
    let p: Vec[i64] = v.iter().scan(1i64, |acc, x| Some((acc * x, acc * x * 10i64))).collect();
    println(f"{p.len()} {p[0i64]} {p[3i64]}");
}
"#,
    ) {
        // r = [1,3,6,10]; acc product = 1,2,6,24 -> p = [10,20,60,240]
        assert_eq!(out, "4 1 3 6 10\n4 10 240\n");
    }
}

#[test]
fn e2e_iter_adaptor_collect_enumerate_codegen() {
    // B-2026-07-04-2 sub-part 1: `<iter>.enumerate().collect()` — the
    // element-retyping adaptor `T` → `(i64, T)`. Lowered like the other
    // adaptors, threading a pre-loop index counter: at the `enumerate` stage
    // the current element is wrapped `(idx, current)` and the counter
    // advanced (matching the interpreter's `Enumerate` step). Exercises a
    // terminal enumerate (tuple push + `.0`/`.1` read), `enumerate().map`
    // over the tuple via a single-`Binding` param, a `map` BEFORE enumerate,
    // `enumerate().take` (index counted per enumerated output),
    // `skip().enumerate` (enumerate re-indexes from 0 after the skip), and
    // `enumerate().filter` on a tuple field. HEAP element sources
    // (`Vec[String]`): a TERMINAL enumerate (`Vec[(i64, String)]` pushed
    // straight in — B-2026-07-04-3) works, and `enumerate().map(|p| …)` works
    // (the tuple is bound DIRECTLY to the map's param — single owning binding
    // — and `map` pushes a TRANSFORMED value — B-2026-07-04-4). A heap
    // `enumerate` followed by `filter`/`take_while` (a conditional WHOLE-
    // tuple move) or a DOWNSTREAM `map` past a `take`/`filter` also works now
    // (B-2026-07-04-4): the tuple binds directly to the first downstream
    // param (searching past `take`/`skip`/`step_by`), keeping it a single
    // owning binding -- no `let p = __ietup` whole-tuple bit-copy. (A
    // residual whole-tuple copy -- a later param stage with a DIFFERENT name,
    // or a non-terminal whole-tuple `map` -- still gates to the loud
    // dispatch-fail rather than miscompiling.)
    if let Some(out) = run_program(
        r#"
fn main() {
    let s: Vec[i64] = Vec[10i64, 20i64, 30i64, 40i64];
    let e: Vec[(i64, i64)] = s.iter().enumerate().collect();
    let e0 = e[0];
    let e3 = e[3];
    println(f"{e.len()} {e0.0} {e0.1} {e3.0} {e3.1}");
    let m: Vec[i64] = s.iter().enumerate().map(|p| p.0 * 100i64 + p.1).collect();
    println(f"{m[0]} {m[1]} {m[3]}");
    let me: Vec[i64] = s.iter().map(|x| x + 1i64).enumerate().map(|p| p.0 + p.1).collect();
    println(f"{me[0]} {me[3]}");
    let et: Vec[(i64, i64)] = s.iter().enumerate().take(2i64).collect();
    let t1 = et[1];
    println(f"{et.len()} {t1.0} {t1.1}");
    let se: Vec[(i64, i64)] = s.iter().skip(1i64).enumerate().collect();
    let se0 = se[0];
    println(f"{se.len()} {se0.0} {se0.1}");
    let ef: Vec[(i64, i64)] = s.iter().enumerate().filter(|p| p.1 > 20i64).collect();
    let f0 = ef[0];
    println(f"{ef.len()} {f0.0} {f0.1}");
    let hw: Vec[String] = Vec["red".to_string(), "green".to_string(), "blue".to_string()];
    let he: Vec[(i64, String)] = hw.iter().enumerate().collect();
    let h0 = ref he[0];
    let h2 = ref he[2];
    println(f"{he.len()} {h0.0} {h0.1} {h2.0} {h2.1}");
    let hf: Vec[(i64, String)] = hw.iter().filter(|s| s.len() > 3i64).enumerate().collect();
    let hf0 = ref hf[0];
    println(f"{hf.len()} {hf0.0} {hf0.1}");
    let hm: Vec[String] = hw.iter().enumerate().map(|p| p.1).collect();
    println(f"{hm.len()} {hm[0]} {hm[2]}");
    let hl: Vec[i64] = hw.iter().enumerate().map(|p| p.0 + p.1.len()).collect();
    println(f"{hl[0]} {hl[1]} {hl[2]}");
    let hs: Vec[String] = hw.iter().skip(1i64).enumerate().map(|p| p.1).collect();
    println(f"{hs.len()} {hs[0]}");
    let hef: Vec[(i64, String)] = hw.iter().enumerate().filter(|p| p.0 > 0i64).collect();
    let hef0 = ref hef[0];
    println(f"{hef.len()} {hef0.0} {hef0.1}");
    let hetw: Vec[(i64, String)] = hw.iter().enumerate().take_while(|p| p.0 < 2i64).collect();
    let hetw1 = ref hetw[1];
    println(f"{hetw.len()} {hetw1.0} {hetw1.1}");
    let htm: Vec[String] = hw.iter().enumerate().take(2i64).map(|p| p.1).collect();
    println(f"{htm.len()} {htm[0]} {htm[1]}");
    let hfm: Vec[String] = hw.iter().enumerate().filter(|p| p.0 > 0i64).map(|p| p.1).collect();
    println(f"{hfm.len()} {hfm[0]} {hfm[1]}");
}
"#,
    ) {
        assert_eq!(
                out,
                "4 0 10 3 40\n10 120 340\n11 44\n2 1 20\n3 0 20\n2 2 30\n3 0 red 2 blue\n2 0 green\n3 red blue\n3 6 6\n2 green\n2 1 green\n2 1 green\n2 red green\n2 green blue\n"
            );
    }
}

/// B-2026-07-04-5: a collect-adaptor chain whose SOURCE is a fresh-temp
/// call result (`mk().iter()…`) — not a named local — mis-typed the
/// synthetic for-loop element. `mk().iter().enumerate().collect()` reduces
/// (in `try_compile_for_vec_value`) to iterating the materialized `mk()`
/// temp; the element type was resolved from `owned_temp_drops` at the
/// iterable span, but the whole method-call chain (`mk`, `mk()`,
/// `.iter()`, `.enumerate()`, `.collect()`) shares the base callee's span,
/// so that table held the OUTERMOST result `Vec[(i64, String)]` instead of
/// the source `Vec[String]`. The loop then read/dropped the `Vec[String]`
/// buffer at the wider `(i64, String)` element stride → `pointer being
/// freed was not allocated` (SIGABRT / SIGTRAP under -O2). The fix prefers
/// the collision-immune `temp_recv_elem_types` element table. Covers a
/// heap (`String`) terminal enumerate over a fresh-temp source (the double-
/// free shape — reads `.0`/`.1` and relies on the drop of both the temp
/// and the collected Vec), the `enumerate().map(|p| p.1)` transform, a POD
/// fresh-temp enumerate, and a plain `.map` over a fresh-temp source.
#[test]
fn e2e_iter_adaptor_collect_fresh_temp_source_codegen() {
    if let Some(out) = run_program(
        r#"
fn mk() -> Vec[String] {
    return Vec["redxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx".to_string(), "grnyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyy".to_string()];
}
fn nums() -> Vec[i64] {
    return Vec[10i64, 20i64, 30i64];
}
fn main() {
    let he: Vec[(i64, String)] = mk().iter().enumerate().collect();
    let e0 = ref he[0];
    let e1 = ref he[1];
    println(f"{he.len()} {e0.0} {e0.1} {e1.0} {e1.1}");
    let hm: Vec[String] = mk().iter().enumerate().map(|p| p.1).collect();
    println(f"{hm.len()} {hm[0]} {hm[1]}");
    let hl: Vec[i64] = mk().iter().map(|s| s.len()).collect();
    println(f"{hl[0]} {hl[1]}");
    let pe: Vec[(i64, i64)] = nums().iter().enumerate().collect();
    let p1 = pe[1];
    println(f"{pe.len()} {p1.0} {p1.1}");
}
"#,
    ) {
        assert_eq!(
                out,
                "2 0 redxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx 1 grnyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyy\n2 redxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx grnyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyy\n41 41\n3 1 20\n"
            );
    }
}

#[test]
fn e2e_chars_iterator_bound_to_variable_codegen() {
    // B-2026-06-18-5: `s.chars()` bound to a NAME (`let it = s.chars();`)
    // failed codegen ("Vec/String method 'chars' is not yet supported")
    // because codegen has no first-class iterator value — only the direct
    // `s.chars().collect()` chain was pattern-matched (B-2026-06-18-1). The
    // fix materializes a standalone `chars()` as the eager `Vec[char]`
    // snapshot, registers the binding as `Vec[char]`, and treats
    // `it.collect()` (collect only typechecks on an iterator) as a clone of
    // that snapshot. Exercises: collect from a bound iterator, a `for c in
    // it` loop directly over the bound iterator, an empty string, Unicode
    // (é is a 2-byte char counted as one scalar), and collecting the SAME
    // bound iterator twice (independent copies — no aliasing/double-free).
    if let Some(out) = run_program(
        r#"
fn main() {
    let s: String = "héllo";
    let it = s.chars();
    let v: Vec[char] = it.collect();
    let mut joined: String = "";
    for c in v { joined.push(c); }
    println(f"{joined} {joined.char_count()}");

    let it2 = s.chars();
    let mut dashed: String = "";
    for c in it2 { dashed.push(c); dashed.push('-'); }
    println(dashed);

    let e: String = "";
    let ie = e.chars();
    let ve: Vec[char] = ie.collect();
    println(f"{ve.len()}");

    let it3 = s.chars();
    let a: Vec[char] = it3.collect();
    let b: Vec[char] = it3.collect();
    println(f"{a.len()} {b.len()} {a[0]} {b[4]}");
}
"#,
    ) {
        assert_eq!(out, "héllo 5\nh-é-l-l-o-\n0\n5 5 h o\n");
    }
}

#[test]
fn e2e_from_slice_range_of_array_codegen() {
    // Relay dogfood slice 4: `Vec.from_slice(arr[a..b])` — a RANGE-slice of
    // an array (or vec/slice) as the source. Previously mis-routed to the
    // scalar nested-index `Vec[Vec[T]]` path ("nested-index source
    // `arr[i]` requires outer to be Vec[Vec[T]]") because
    // `recover_from_slice_src` treated every `Index` expr as a scalar
    // nested index. Now a `Range` index is recognized as a slice and
    // lowered via `coerce_to_slice` (the same `compile_range_slice` path
    // a bare `f(arr[a..b])` call argument uses). Copies a window of bytes
    // into an owned Vec — the canonical "read N bytes, hand them to a
    // parser" shape (`examples/relay`'s request-line peek).
    if let Some(out) = run_program(
        "fn main() {\n\
                 let arr: Array[i64, 5] = [10, 20, 30, 40, 50];\n\
                 let v: Vec[i64] = Vec.from_slice(arr[1..4]);\n\
                 println(v.len());\n\
                 println(v[0]);\n\
                 println(v[1]);\n\
                 println(v[2]);\n\
             }",
    ) {
        assert_eq!(out, "3\n20\n30\n40\n");
    }
}

#[test]
fn e2e_collect_into_every_documented_from_iterator_target() {
    // Codegen twin of the interpreter oracle for B-2026-08-17-36
    // (tests/interpreter.rs). `collect()` produced `Vec[T]` and nothing
    // else, so every non-`Vec` target design.md promises was rejected at
    // typecheck.
    //
    // This runs as an E2E rather than a typecheck test because the whole
    // point of lowering it as a pre-typecheck desugar is that `--interp`
    // and both compiled backends execute the SAME rewritten code -- so the
    // check that matters is that a real binary prints what the interpreter
    // prints. Same program and same expected output as the oracle.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let s = \"hi there\";\n\
                 let up: String = s.chars().map(|c| c.to_uppercase()).collect();\n\
                 println(up);\n\
                 let mut words: Vec[String] = Vec.new();\n\
                 words.push(\"ab\");\n\
                 words.push(\"cd\");\n\
                 let joined: String = words.iter().map(|w| w.to_uppercase()).collect();\n\
                 println(joined);\n\
                 let mut raw: Vec[i64] = Vec.new();\n\
                 raw.push(1); raw.push(2); raw.push(2); raw.push(3);\n\
                 let plain: Vec[i64] = raw.iter().map(|x| x * 2).collect();\n\
                 println(plain.len());\n\
                 let st: Set[i64] = raw.iter().map(|x| x).collect();\n\
                 println(st.len());\n\
                 let dq: VecDeque[i64] = raw.iter().map(|x| x * 10).collect();\n\
                 println(dq.len());\n\
                 println(dq[0]);\n\
                 let mp: Map[i64, i64] = raw.iter().map(|x| (x, x * 100)).collect();\n\
                 println(mp.len());\n\
                 println(mp[3]);\n\
                 let empty: Vec[i64] = Vec.new();\n\
                 let es: Set[i64] = empty.iter().map(|x| x).collect();\n\
                 println(es.len());\n\
             }",
    ) {
        assert_eq!(out, "HI THERE\nABCD\n4\n3\n4\n10\n3\n300\n0\n");
    }
}

#[test]
fn test_e2e_sum_for_range() {
    let out = run_program(
        r#"
fn main() {
    let mut sum = 0;
    for i in 1..=100 {
        sum = sum + i;
    }
    println(sum);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "5050");
    }
}

#[test]
fn test_e2e_match_range_inclusive() {
    // Regression: range patterns had no codegen arm and fell through
    // to the catch-all `_ => true`, so every value matched the first
    // range arm. The interpreter was correct; codegen was not.
    let out = run_program(
        r#"
fn classify(n: i64) -> i64 {
    match n {
        10..=20 => 111,
        _ => 999,
    }
}
fn main() {
    println(classify(5));    // below → 999
    println(classify(10));   // lower bound → 111
    println(classify(15));   // inside → 111
    println(classify(20));   // upper bound (inclusive) → 111
    println(classify(21));   // above → 999
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["999", "111", "111", "111", "999"]);
    }
}

#[test]
fn test_e2e_match_range_exclusive() {
    // Exclusive upper bound: `10..20` excludes 20.
    let out = run_program(
        r#"
fn classify(n: i64) -> i64 {
    match n {
        10..20 => 111,
        _ => 999,
    }
}
fn main() {
    println(classify(10));   // lower bound → 111
    println(classify(19));   // inside → 111
    println(classify(20));   // upper bound (exclusive) → 999
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["111", "111", "999"]);
    }
}

#[test]
fn test_e2e_match_range_char() {
    let out = run_program(
        r#"
fn kind(c: char) -> i64 {
    match c {
        '0'..='9' => 1,
        'a'..='z' => 2,
        _ => 0,
    }
}
fn main() {
    println(kind('5'));   // digit → 1
    println(kind('q'));   // lower → 2
    println(kind('M'));   // neither → 0
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["1", "2", "0"]);
    }
}

#[test]
fn test_e2e_match_range_const_bounds() {
    // Const-named range bounds (design.md § Range Patterns) lower by
    // re-compiling the named const's initializer at the bound site.
    let out = run_program(
        r#"
const LO: i64 = 10;
const HI: i64 = 20;
fn classify(n: i64) -> i64 {
    match n {
        ..LO => 1,
        LO..=HI => 2,
        _ => 3,
    }
}
fn main() {
    println(classify(5));    // below LO → 1
    println(classify(10));   // lower bound → 2
    println(classify(20));   // upper bound (inclusive) → 2
    println(classify(25));   // above → 3
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["1", "2", "2", "3"]);
    }
}

#[test]
fn test_e2e_match_at_binding_range() {
    // `@` bindings had no codegen arm: `bind_pattern_values` fell
    // through to `_ => Ok(())` (alias never stored) and
    // `compile_pattern_condition` fell through to `_ => true` (every
    // `@` matched unconditionally). The interpreter was correct.
    // This exercises both halves: the range condition must select the
    // right arm AND the alias `c` must hold the real scrutinee value.
    let out = run_program(
        r#"
fn classify(code: i64) -> i64 {
    match code {
        c @ 200..=299 => c + 1000,
        c @ 400..=499 => c + 2000,
        other => other,
    }
}
fn main() {
    println(classify(204));  // in 200..=299 → 204 + 1000 = 1204
    println(classify(404));  // in 400..=499 → 404 + 2000 = 2404
    println(classify(500));  // no range → catch-all alias → 500
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["1204", "2404", "500"]);
    }
}

#[test]
fn test_e2e_match_byte_range() {
    // Combines both fixes: byte-literal range bounds (parser) lowered
    // to unsigned comparisons (codegen).
    let out = run_program(
        r#"
fn kind(b: u8) -> i64 {
    match b {
        b'0'..=b'9' => 1,
        b'a'..=b'z' => 2,
        _ => 0,
    }
}
fn main() {
    println(kind(b'5'));   // digit → 1
    println(kind(b'q'));   // lower → 2
    println(kind(b'M'));   // neither → 0
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["1", "2", "0"]);
    }
}

/// B-2026-08-24-15 — `Vec.insert` / `Vec.remove` / `Vec.swap_remove` emit
/// no bounds check in codegen, so an out-of-range index was memory-unsafe
/// instead of fatal.
///
/// `insert`'s shift count is `len - idx`, so `idx > len` went negative and
/// reached `memmove` as a huge unsigned byte count (glibc `free(): invalid
/// pointer`, exit 134). `remove` did the same. `swap_remove` did not crash
/// at all — it read past the end, returned the garbage (a live heap
/// pointer, under the JIT) and decremented `len`, dropping a live element
/// with a clean exit 0.
///
/// Plain indexing `v[i]` was always checked, which is what made this an
/// inconsistency inside one type rather than a no-checks policy. All three
/// must now panic exactly as `--interp` and design.md say they do.
///
/// B-2026-08-26-21 follow-up adds `Vec.swap` to the family, and it was the
/// worst of the four: codegen had NO `swap` arm at all (`karac build`
/// hard-errored "not yet supported"), while the interpreter accepted an
/// out-of-range swap and SILENTLY DID NOTHING — `v.swap(0, 99)` on a
/// two-element Vec left the vector untouched and exited 0. Negative indices
/// were swallowed the same way, by an `as usize` cast that wrapped them out
/// of range. Both halves are fixed; the negative cases below are what pin
/// the wrap.
#[test]
fn vec_mutation_methods_bounds_check_out_of_range_index() {
    const PRELUDE: &str = "fn main() {\n\
             let mut v: Vec[i64] = Vec.new();\n\
             v.push(11i64);\n\
             v.push(22i64);\n";

    // (call, panic-message fragment) — each must trap, not corrupt.
    let cases = [
        ("v.insert(7i64, 9i64);", "Vec.insert index out of bounds"),
        ("v.insert(-1i64, 9i64);", "Vec.insert index out of bounds"),
        (
            "let x = v.remove(7i64); println(x);",
            "Vec.remove index out of bounds",
        ),
        (
            "let x = v.remove(-1i64); println(x);",
            "Vec.remove index out of bounds",
        ),
        (
            "let x = v.swap_remove(7i64); println(x);",
            "Vec.swap_remove index out of bounds",
        ),
        (
            "let x = v.swap_remove(-1i64); println(x);",
            "Vec.swap_remove index out of bounds",
        ),
        ("v.swap(7i64, 0i64);", "Vec.swap index out of bounds"),
        ("v.swap(0i64, 7i64);", "Vec.swap index out of bounds"),
        ("v.swap(-1i64, 0i64);", "Vec.swap index out of bounds"),
        ("v.swap(0i64, -1i64);", "Vec.swap index out of bounds"),
    ];

    for (call, want) in cases {
        let src = format!("{PRELUDE}    {call}\n    println(\"survived\");\n}}");
        let run = match run_program_capturing(&src) {
            Some(r) => r,
            None => return, // no runtime archive / linker — harness skip
        };
        assert!(
            run.stderr.contains(want),
            "{call} must panic with {want:?}; got stdout={:?} stderr={:?}",
            run.stdout,
            run.stderr
        );
        assert!(
            !run.stdout.contains("survived"),
            "{call} must not fall through to the next statement; got stdout={:?}",
            run.stdout
        );
        // Exit 101 is the panic path. The bug produced 134 (abort from
        // glibc's heap check) for insert/remove and 0 for swap_remove, so
        // pinning the code is what separates "panics" from "dies somehow".
        assert_eq!(
            run.status.code(),
            Some(101),
            "{call} must exit via panic (101), not abort or success; stderr={:?}",
            run.stderr
        );
    }
}

/// The other half of B-2026-08-24-15: the new bounds checks must not
/// reject anything legal. `insert(len, v)` is an APPEND and is valid —
/// `insert` alone accepts `idx == len`, which is why it needs `UGT` where
/// `remove` / `swap_remove` need `UGE`. Getting that predicate wrong turns
/// a memory-safety fix into a broken `push`.
#[test]
fn vec_mutation_methods_accept_every_in_range_index() {
    let src = "fn main() {\n\
             let mut v: Vec[i64] = Vec.new();\n\
             v.push(11i64);\n\
             v.push(22i64);\n\
             v.insert(2i64, 33i64);\n\
             v.insert(0i64, 0i64);\n\
             println(v.len());\n\
             let a = v.remove(0i64);\n\
             let b = v.remove(v.len() - 1i64);\n\
             let c = v.swap_remove(0i64);\n\
             println(a);\n\
             println(b);\n\
             println(c);\n\
             println(v.len());\n\
             let mut e: Vec[i64] = Vec.new();\n\
             e.insert(0i64, 7i64);\n\
             println(e[0]);\n\
         }";
    // insert(2)=append -> 11 22 33; insert(0) -> 0 11 22 33
    // remove(0)=0; remove(last)=33; swap_remove(0)=11, leaving [22]
    assert_eq!(
        run_program(src),
        Some("4\n0\n33\n11\n1\n7\n".to_string()),
        "in-range insert/remove/swap_remove must be unaffected, including \
             insert-at-len (append) and insert(0) into an EMPTY vec"
    );
}

/// B-2026-08-26-27 — `Vec.try_from_iter`, twinned against the interpreter.
///
/// The two backends do NOT do the same work here, and that is the point of
/// twinning them: the interpreter always answers `Ok` (its host allocator
/// does not OOM), while codegen genuinely grows the accumulator through
/// `karac_alloc_fallible` and can return
/// `Err(AllocError.OutOfMemory { requested_bytes })`. On the success path —
/// the only one reachable without an allocator that fails on demand — they
/// must agree exactly, which is what this asserts.
///
/// The shapes are the same three `e2e_vec_from_iter_and_collect_are_the_same_program`
/// uses, deliberately: `try_from_iter` routes through the SAME collect
/// engine as its panicking base, so any shape one accepts the other must.
#[test]
fn e2e_vec_try_from_iter_agrees_with_the_interpreter() {
    let src = "fn build() -> Result[i64, AllocError] {\n\
                       let a: Vec[i64] = Vec.try_from_iter(0..4)?;\n\
                       println(f\"{a.len()} {a[0]} {a[3]}\");\n\
                       let b: Vec[i64] = Vec.try_from_iter((0..6).map(|x| x * 2))?;\n\
                       println(f\"{b.len()} {b[5]}\");\n\
                       let s: Vec[i64] = [5, 6, 7];\n\
                       let c: Vec[i64] = Vec.try_from_iter(s.iter().map(|x| x + 1))?;\n\
                       println(f\"{c.len()} {c[2]}\");\n\
                       let h: Vec[String] = [\"a\", \"bb\"];\n\
                       let hu: Vec[String] = Vec.try_from_iter(h.iter().map(|t| t.to_uppercase()))?;\n\
                       println(f\"{hu.len()} {hu[0]} {hu[1]}\");\n\
                       return Ok(a.len() + b.len() + c.len() + hu.len());\n\
                   }\n\
                   fn main() {\n\
                       match build() {\n\
                           Ok(n) => { println(f\"ok {n}\"); }\n\
                           Err(e) => { println(\"oom\"); }\n\
                       }\n\
                   }";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(
        interp_errs.is_empty(),
        "interpreter errored: {interp_errs:?}"
    );
    let expected = interp_out.join("");
    assert_eq!(expected, "4 0 3\n6 10\n3 8\n2 A BB\nok 15\n");
    if let Some(aot) = run_program(src) {
        assert_eq!(
            aot, expected,
            "the fallible collect must produce the same Vec as the interpreter's"
        );
    }
}

/// ANTI-DRIFT: `try_from_iter` and `from_iter` must accept the same shapes
/// and produce the same contents. They share one engine — the fallible
/// switch changes the leaf append and the block's tail, nothing else — and
/// this is what would catch someone giving the companion its own lowering.
#[test]
fn e2e_vec_try_from_iter_matches_its_panicking_base() {
    let fallible = "fn build() -> Result[i64, AllocError] {\n\
                            let a: Vec[i64] = Vec.try_from_iter((0..9).map(|x| x * 3))?;\n\
                            println(f\"{a.len()} {a[0]} {a[8]}\");\n\
                            return Ok(a.len());\n\
                        }\n\
                        fn main() { match build() { Ok(n) => { println(f\"n {n}\"); } Err(e) => { println(\"oom\"); } } }";
    let panicking = "fn main() {\n\
                             let a: Vec[i64] = Vec.from_iter((0..9).map(|x| x * 3));\n\
                             println(f\"{a.len()} {a[0]} {a[8]}\");\n\
                             println(f\"n {a.len()}\");\n\
                         }";
    let (f_out, f_errs, _, _) = karac::run_program_full_checked(fallible);
    let (p_out, p_errs, _, _) = karac::run_program_full_checked(panicking);
    assert!(
        f_errs.is_empty() && p_errs.is_empty(),
        "{f_errs:?} {p_errs:?}"
    );
    assert_eq!(
        f_out.join(""),
        p_out.join(""),
        "try_from_iter and from_iter must collect identically"
    );
    if let (Some(fa), Some(pa)) = (run_program(fallible), run_program(panicking)) {
        assert_eq!(fa, pa, "and identically when compiled");
        assert_eq!(fa, f_out.join(""), "and with the interpreter");
    }
}

/// `Vec.from_iter(it)` and `it.collect()` must produce the SAME program,
/// not merely similar answers: the typechecker types the former by
/// inferring the latter, and lowering rewrites to it. Comparing the two
/// spellings directly is what keeps a future "optimisation" from giving
/// `from_iter` its own divergent path.
#[test]
fn e2e_vec_from_iter_and_collect_are_the_same_program() {
    let via_from_iter = "fn main() {\n\
             let a: Vec[i64] = Vec.from_iter((0..8).map(|x| x * 3));\n\
             println(f\"{a.len()} {a[0]} {a[7]}\");\n\
         }\n";
    let via_collect = "fn main() {\n\
             let a: Vec[i64] = (0..8).map(|x| x * 3).collect();\n\
             println(f\"{a.len()} {a[0]} {a[7]}\");\n\
         }\n";
    let (interp_a, errs_a, _, _) = karac::run_program_full_checked(via_from_iter);
    let (interp_b, errs_b, _, _) = karac::run_program_full_checked(via_collect);
    assert!(
        errs_a.is_empty() && errs_b.is_empty(),
        "{errs_a:?} {errs_b:?}"
    );
    assert_eq!(
        interp_a.join(""),
        interp_b.join(""),
        "from_iter and collect must agree in the interpreter"
    );
    if let (Some(aot_a), Some(aot_b)) = (run_program(via_from_iter), run_program(via_collect)) {
        assert_eq!(
            aot_a, aot_b,
            "from_iter and collect must agree when compiled"
        );
        assert_eq!(aot_a, interp_a.join(""), "and with the interpreter");
    }
}

#[test]
fn test_e2e_array_literal_range_slice_arg() {
    let out = run_program(
        r#"
fn sum(xs: Slice[u8]) -> i64 {
    let mut s = 0_i64;
    for x in xs { s = s + (x as i64); }
    s
}
fn main() {
    // [1..3] selects 20 + 30.
    println(sum([10u8, 20u8, 30u8, 40u8][1..3]));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "50");
    }
}

#[test]
fn test_standalone_array_literal_range_slice_fails_loud() {
    // `let x = [1,2,3][a..b]` (standalone, not a call arg) routes through
    // compile_index, where the literal's element type isn't recoverable.
    // Rather than fall through to a confusing downstream error (or risk a
    // Vec mis-stride silent miscompile), codegen fails loud with an
    // actionable "bind it to a variable first" message. (The call-arg
    // form `f([1,2,3][a..b])` works — see the tests above.)
    let mut parsed = karac::parse(
        "fn main() {\n\
                 let x = [10u8, 20u8, 30u8, 40u8][1..3];\n\
                 println(x.len());\n\
             }",
    );
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    karac::prepare_for_resolve(&mut parsed.program);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);
    let err = compile_to_ir(&parsed.program, None, None)
        .expect_err("standalone array-literal range-slice must fail loud")
        .message;
    assert!(
        err.contains("anonymous array/Vec literal") && err.contains("bind it to a variable"),
        "expected the actionable bind-first message, got: {err}"
    );
}

#[test]
fn test_e2e_for_range_step_by_codegen() {
    // `for j in (start..=end).step_by(n)` — the iterator-adaptor
    // chain previously fell through `compile_for`'s match to the
    // silent `_ =>` arm, skipping the body entirely. Now lowers
    // to a Range loop with the step expr evaluated once before
    // the loop and used as the increment. Pins the sieve / strided
    // iteration pattern that the LeetCode 3629 kata uses.
    let out = run_program(
        r#"
fn main() {
    let mut sum = 0i64;
    for j in (2..=10).step_by(2) {
        sum = sum + j;
    }
    println(sum);
    let mut count = 0i64;
    for _ in (0..20).step_by(5) {
        count = count + 1;
    }
    println(count);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "30\n4");
    }
}

#[test]
fn test_e2e_for_range_step_by_with_runtime_step() {
    // The kata's actual usage: the step expression refers to an
    // outer variable. The step is evaluated once per loop entry
    // and captured into the increment block.
    let out = run_program(
        r#"
fn main() {
    let cap = 12i64;
    for i in 2..=cap {
        let mut count = 0i64;
        for _j in (i..=cap).step_by(i) {
            count = count + 1;
        }
        println(count);
    }
}
"#,
    );
    // For each i in 2..=12, count multiples of i up to cap=12:
    //   i=2 → 2,4,6,8,10,12 → 6
    //   i=3 → 3,6,9,12 → 4
    //   i=4 → 4,8,12 → 3
    //   i=5 → 5,10 → 2
    //   i=6 → 6,12 → 2
    //   i=7 → 7 → 1
    //   i=8 → 8 → 1
    //   i=9 → 9 → 1
    //   i=10 → 10 → 1
    //   i=11 → 11 → 1
    //   i=12 → 12 → 1
    if let Some(out) = out {
        assert_eq!(out.trim(), "6\n4\n3\n2\n2\n1\n1\n1\n1\n1\n1");
    }
}

/// The two directions the fix above could get wrong, which matter more
/// than the returning cases themselves.
///
/// Case 1: every fused pipeline WITHOUT a `return` must still compile and
/// run — these are the hot paths the fusion exists for, and a guard that
/// tripped on them would be far worse than the bug.
///
/// Case 2: a `return` inside a NESTED closure belongs to THAT closure,
/// which is lowered as a real closure body where `return` already worked.
/// An earlier iteration of this work walked into nested closures and
/// mishandled it, breaking a program that built correctly — measured, not
/// hypothetical, and the reason the body walker stops at a nested closure.
#[test]
fn test_e2e_fused_iter_pipelines_without_return_are_untouched() {
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let v: Vec[i64] = [1i64, 2i64, 3i64, 4i64];\n\
                     let d: Vec[i64] = v.iter().map(|n| n * 2i64).collect();\n\
                     let e: Vec[i64] = v.iter().filter(|n| n > 2i64).collect();\n\
                     let s: i64 = v.iter().fold(0i64, |acc, n| acc + n);\n\
                     let a: bool = v.iter().any(|n| n > 3i64);\n\
                     let b: bool = v.iter().all(|n| n > 0i64);\n\
                     let mut w: Vec[i64] = v;\n\
                     w.retain(|n| n > 2i64);\n\
                     println(f\"{d[3]} {e.len()} {s} {a} {b} {w.len()}\");\n\
                 }"
        )
        .as_deref(),
        Some("8 2 10 true true 2\n")
    );
    assert_eq!(
        run_program(
            "fn apply(f: Fn(i64) -> i64, x: i64) -> i64 { f(x) }\n\
                 fn main() {\n\
                     let v: Vec[i64] = [1i64, 2i64];\n\
                     let d: Vec[i64] = v.iter()\n\
                         .map(|n| apply(|m| { if m > 1i64 { return m * 10i64; } return m; }, n))\n\
                         .collect();\n\
                     println(f\"{d[0]} {d[1]}\");\n\
                 }"
        )
        .as_deref(),
        Some("1 20\n")
    );
}

#[test]
fn test_e2e_for_vec_iter_propagates_outer_mutable_writes() {
    // `for x in v.iter()` codegen previously fell through to the
    // silent `_ =>` arm in `compile_for` — the body never ran,
    // so writes to outer-scope mutables (e.g. `m = x`) had no
    // effect and the loop appeared to be a no-op. Regression
    // test: maxv on [1,2,4,6] must return 6, not the initial 0.
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1); v.push(2); v.push(4); v.push(6);
    let mut m = 0i64;
    for x in v.iter() {
        if x > m { m = x; }
    }
    println(m);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "6");
    }
}

#[test]
fn test_e2e_for_slice_iter_propagates_outer_mutable_writes() {
    // Same shape as the Vec case but for `Slice[T].iter()` —
    // sibling of the previous test; `compile_for`'s `_ =>` arm
    // ate both shapes before the iter/into_iter peel-off landed.
    let out = run_program(
        r#"
fn maxv(nums: Slice[i64]) -> i64 {
    let mut m = 0i64;
    for v in nums.iter() {
        if v > m { m = v; }
    }
    m
}
fn main() {
    let a: Array[i64, 4] = [1, 2, 4, 6];
    println(maxv(a));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "6");
    }
}

#[test]
fn test_e2e_trunc_to_in_range_and_to_float() {
    let out = run_program(
        r#"
fn main() {
    println((42.9f64).trunc_to_i32());
    println((42i64).to_f64());
    println((200u8).to_f32());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "42\n42\n200");
    }
}

#[test]
fn test_e2e_trunc_to_out_of_range_traps() {
    // `trunc_to_iN` is the trapping form — out-of-range panics with the
    // `#[track_caller]` "float-to-int out of range" message (carries the
    // `panics` effect; slice 2 wired the effect, slice 4 the trap).
    let captured = run_program_capturing(
        r#"
fn main() {
    let big: f64 = 1e30;
    println(big.trunc_to_i32());
}
"#,
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("float-to-int out of range"),
            "expected out-of-range trap, got stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
        assert!(
            !c.stdout.contains("2147483647"),
            "the saturated value must not print on the trapping path"
        );
    }
}

#[test]
fn test_e2e_for_self_field_vec_iter_in_impl_method() {
    // Silent-miscompile gate: `for s in self.items.iter()` inside an
    // impl method iterated ZERO times in codegen while the interpreter
    // iterated correctly (`self` parses as `SelfValue`, which
    // `try_compile_for_field_iter`'s inner-receiver match didn't handle
    // — it fell through to the dispatcher's silent `_ =>` skip). The
    // `ref self` receiver also needs ref-param-aware pointer resolution
    // (`get_data_ptr`), since the alloca holds a pointer-TO-struct.
    // Also exercises the bare form `for s in self.items` (no `.iter()`),
    // which routes through the same helper. Expected sum: base 100 +
    // len(52) + len(49) == 201. String payloads are >36 bytes so the
    // LSan gate exercises real heap element frees.
    let out = run_program(
        r#"
struct Counter { items: Vec[String], base: i64 }
impl Counter {
    fn total_iter(ref self) -> i64 {
        let mut t = self.base;
        for s in self.items.iter() { t = t + s.len(); };
        return t;
    }
    fn total_bare(ref self) -> i64 {
        let mut t = self.base;
        for s in self.items { t = t + s.len(); };
        return t;
    }
    fn count(ref self) -> i64 { return self.items.len(); }
}
fn make_counter() -> Counter {
    let mut c = Counter { items: Vec.new(), base: 100_i64 };
    c.items.push("first field string padded beyond thirty-six bytes ok");
    c.items.push("second field string padded beyond thirty-six byte");
    return c;
}
fn main() {
    let c = make_counter();
    println(c.total_iter());
    println(c.total_bare());
    println(c.count());
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["201", "201", "2"]);
    }
}

#[test]
fn test_e2e_for_self_field_vec_iter_shared_struct() {
    // Shared-struct sibling of the plain-struct self-field for-loop fix.
    // A `shared struct` receiver reached the loop via the same
    // `try_compile_for_field_iter` helper but the hand-rolled resolver
    // single-loaded `slot.ptr` — one indirection short for a `ref self`
    // shared handle (the alloca holds a pointer-TO-handle) — so the field
    // GEP read a garbage `{ptr,len,cap}` → 0 iterations. Delegating to
    // `lower_field_access_ptr` (which walks `compile_expr(self)`'s full
    // ref-param deref chain) resolves the heap struct pointer correctly on
    // both owned and `ref self` shared receivers. Expected: base 10 +
    // len(55) + len(54) == 119.
    let out = run_program(
        r#"
shared struct SBag { mut items: Vec[String], base: i64 }
impl SBag {
    fn total(ref self) -> i64 {
        let mut t = self.base;
        for s in self.items.iter() { t = t + s.len(); };
        return t;
    }
}
fn main() {
    let b = SBag { items: Vec.new(), base: 10_i64 };
    b.items.push("shared field string padded beyond thirty-six bytes okay");
    b.items.push("another shared field string beyond thirty-six bytes ok");
    println(b.total());
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["119"]);
    }
}

#[test]
fn test_ir_for_self_field_vec_iter_emits_loop_body() {
    // Structural guard for the silent-skip regression: the compiled
    // `total_iter` must contain a real for-vec loop over the field —
    // the `for.cond` / `for.body` blocks and the element load. When the
    // SelfValue arm was missing, the body was skipped entirely and none
    // of these appeared (the loop lowered to nothing).
    let ir = ir_for(
        r#"
struct Counter { items: Vec[String], base: i64 }
impl Counter {
    fn total_iter(ref self) -> i64 {
        let mut t = self.base;
        for s in self.items.iter() { t = t + s.len(); };
        return t;
    }
}
fn main() {
    let mut c = Counter { items: Vec.new(), base: 0_i64 };
    c.items.push("x");
    println(c.total_iter());
}
"#,
    );
    assert!(
        ir.contains("for.cond") && ir.contains("for.body"),
        "for-loop over `self.items.iter()` must emit a real loop body \
             (for.cond/for.body), not the silent 0-iteration skip\n--- ir ---\n{ir}"
    );
    // The field GEP for a plain (non-shared) struct hangs off `fr_pl_items`
    // (emitted by the shared `lower_field_access_ptr` resolver).
    assert!(
        ir.contains("fr_pl_items"),
        "the field pointer GEP (`fr_pl_<field>`) must be emitted for the \
             self-field vec source\n--- ir ---\n{ir}"
    );
}

#[test]
fn test_ir_freshtemp_vec_iter_emits_materialize_and_drop() {
    // Slice 3h: `for s in names().iter()` on a fresh-temp `Vec[String]`. The
    // for-loop peels `.iter()` and recurses on the receiver `names()`, whose
    // span collides with the `.iter()` MethodCall — so `expr_types` holds
    // `Iterator[String]` and `owned_temp_drops` has NO Vec entry. Pre-fix the
    // loop fell through to the silent skip (body never ran, output 0). The
    // fresh-temp gate now records the element type span-keyed in
    // `temp_recv_elem_types`; codegen reconstructs `Vec[String]`, materializes
    // the temp into a `__for_vec_` synth local, iterates, and frees each
    // element String in the per-element `cleanup.drop.inner.free` loop before
    // the outer buffer. Without the materialize the body is skipped; without
    // the per-element drop the element Strings leak (Linux LSan).
    let src = r#"
fn names() -> Vec[String] {
    let mut v: Vec[String] = Vec.new();
    v.push("a heap string element padded beyond thirty-six bytes ok");
    return v;
}

fn main() {
    let mut total = 0_i64;
    for s in names().iter() {
        total = total + s.len();
    };
    println(total);
}
"#;
    let ir = ir_for(src);
    assert!(
        ir.contains("__for_vec_"),
        "expected the fresh-temp Vec[String] iterable materialized into a \
             __for_vec_ synth local (not the silent body-skip); got:\n{}",
        ir
    );
    assert!(
        ir.contains("cleanup.drop.inner.free"),
        "expected the vec-struct per-element drop loop (cleanup.drop.inner.free) \
             freeing each element String buffer of the materialized iter temp; got:\n{}",
        ir
    );
}

#[test]
fn test_e2e_slice_range_from_array() {
    let out = run_program(
        r#"
fn sum(xs: Slice[i64]) -> i64 {
    let mut acc = 0;
    for x in xs { acc = acc + x; }
    acc
}
fn main() {
    let a: Array[i64, 5] = [10, 20, 30, 40, 50];
    println(sum(a[1..4]));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "90");
    }
}

#[test]
fn test_e2e_slice_range_bound_to_let() {
    // Range-indexing into a let binding — the variable should be
    // inferred as a Slice[i64] at codegen time so subsequent uses
    // work (indexing, iteration, call coercion).
    let out = run_program(
        r#"
fn sum(xs: Slice[i64]) -> i64 {
    let mut acc = 0;
    for x in xs { acc = acc + x; }
    acc
}
fn main() {
    let a: Array[i64, 5] = [1, 2, 3, 4, 5];
    let middle = a[1..4];
    println(sum(middle));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "9");
    }
}

#[test]
fn test_e2e_bounds_elision_for_range_zero_to_len() {
    // `for i in 0..v.len()` proves both bounds on i: lower from the
    // 0 literal start, upper from v.len() as the end. Inside the
    // body, v[i] should skip both halves of the runtime bounds check.
    let out = run_program(
        r#"
fn sum_all(v: ref Vec[i64]) -> i64 {
    let mut acc = 0i64;
    for i in 0..v.len() {
        acc = acc + v[i];
    }
    acc
}
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1);
    v.push(2);
    v.push(3);
    v.push(4);
    v.push(5);
    println(sum_all(v));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "15");
    }
}

#[test]
fn test_e2e_bounds_elision_for_range_nonzero_start() {
    // `for i in 1..n` where n aliases v.len(). Lower bound from the
    // non-negative literal 1; upper bound via the alias. Both elide.
    let out = run_program(
        r#"
fn sum_skip_first(v: ref Vec[i64]) -> i64 {
    let n = v.len();
    let mut acc = 0i64;
    for i in 1..n {
        acc = acc + v[i];
    }
    acc
}
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(100);
    v.push(1);
    v.push(2);
    v.push(3);
    println(sum_skip_first(v));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "6");
    }
}

#[test]
fn test_e2e_bounds_elision_for_range_inclusive_keeps_upper_check() {
    // Inclusive range `0..=n` includes i = n, which would be OOB on
    // v[i]. The pass MUST NOT elide the upper-bound check here. We
    // exercise the pattern with a safe `n - 1` end so the test
    // passes correctness-wise; the gate is that the compiled code
    // doesn't silently miscompile through elision.
    let out = run_program(
        r#"
fn sum_inclusive(v: ref Vec[i64]) -> i64 {
    let n = v.len();
    let last = n - 1;
    let mut acc = 0i64;
    for i in 0..=last {
        acc = acc + v[i];
    }
    acc
}
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(10);
    v.push(20);
    v.push(30);
    println(sum_inclusive(v));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "60");
    }
}

#[test]
fn test_e2e_bounds_elision_length_pin_for_range_fill() {
    // Follow-up (1): a `for i in 0..n { dp.push(..) }` fill establishes the
    // same `dp.len() == n` pin, so the rolling scan under `while c < n`
    // elides. `unique_paths`-style count for 3x7 → 28.
    let out = run_program(
        r#"
fn build(rows: i64, cols: i64) -> i64 {
    let mut dp: Vec[i64] = Vec.new();
    for i in 0i64..cols { dp.push(1i64); }
    let mut r = 1i64;
    while r < rows {
        let mut c = 1i64;
        while c < cols { dp[c] = dp[c] + dp[c - 1i64]; c = c + 1i64; }
        r = r + 1i64;
    }
    dp[cols - 1i64]
}
fn main() { println(build(7i64, 3i64)); }
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "28");
    }
}

#[test]
fn test_e2e_bounds_elision_length_pin_for_range_nonzero_start_still_checks() {
    // Follow-up (1) negative: `for i in 1..n` pushes only n-1 elements, so
    // `dp[c]` with `c < n` is out of bounds at `c == n-1`. The pin must NOT
    // fire (non-zero start), the check survives, and the program panics.
    if let Some(c) = run_program_capturing(
        r#"
fn go(n: i64) -> i64 {
    let mut dp: Vec[i64] = Vec.new();
    for i in 1i64..n { dp.push(1i64); }
    let mut acc = 0i64;
    let mut c = 0i64;
    while c < n { acc = acc + dp[c]; c = c + 1i64; }
    acc
}
fn main() { println(go(5i64)); }
"#,
    ) {
        assert!(
            !c.status.success(),
            "for 1..n fill must keep the bounds check (panic), got stdout={:?}",
            c.stdout
        );
    }
}

// ── Half-open range indexing ──────────────────────────────────────────────

#[test]
fn test_e2e_range_from_array_tail() {
    // v[a..] — open end: slice from index 2 to end of array
    let out = run_program(
        r#"
fn sum(xs: Slice[i64]) -> i64 {
    let mut acc = 0;
    for x in xs { acc = acc + x; }
    acc
}
fn main() {
    let a: Array[i64, 5] = [10, 20, 30, 40, 50];
    println(sum(a[2..]));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "120"); // 30+40+50
    }
}

#[test]
fn test_e2e_range_full_array() {
    // v[..] — full slice of the array
    let out = run_program(
        r#"
fn sum(xs: Slice[i64]) -> i64 {
    let mut acc = 0;
    for x in xs { acc = acc + x; }
    acc
}
fn main() {
    let a: Array[i64, 4] = [1, 2, 3, 4];
    println(sum(a[..]));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "10"); // 1+2+3+4
    }
}

#[test]
fn test_e2e_range_to_exclusive_array() {
    // v[..b] — from start up to (not including) b
    let out = run_program(
        r#"
fn sum(xs: Slice[i64]) -> i64 {
    let mut acc = 0;
    for x in xs { acc = acc + x; }
    acc
}
fn main() {
    let a: Array[i64, 5] = [10, 20, 30, 40, 50];
    println(sum(a[..3]));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "60"); // 10+20+30
    }
}

#[test]
fn test_e2e_range_to_inclusive_array() {
    // v[..=b] — from start up to and including b
    let out = run_program(
        r#"
fn sum(xs: Slice[i64]) -> i64 {
    let mut acc = 0;
    for x in xs { acc = acc + x; }
    acc
}
fn main() {
    let a: Array[i64, 5] = [10, 20, 30, 40, 50];
    println(sum(a[..=2]));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "60"); // 10+20+30
    }
}

#[test]
fn test_e2e_range_inclusive_array() {
    // v[a..=b] — closed range: both ends inclusive
    let out = run_program(
        r#"
fn sum(xs: Slice[i64]) -> i64 {
    let mut acc = 0;
    for x in xs { acc = acc + x; }
    acc
}
fn main() {
    let a: Array[i64, 5] = [10, 20, 30, 40, 50];
    println(sum(a[1..=3]));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "90"); // 20+30+40
    }
}

#[test]
fn test_e2e_range_from_vec_tail() {
    // vec[a..] — open-end slice of a Vec
    let out = run_program(
        r#"
fn sum(xs: Slice[i64]) -> i64 {
    let mut acc = 0;
    for x in xs { acc = acc + x; }
    acc
}
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1);
    v.push(2);
    v.push(3);
    v.push(4);
    println(sum(v[1..]));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "9"); // 2+3+4
    }
}

/// B-2026-08-14-20, sibling half — a RANGE index as a method receiver.
///
/// `v[a..b]` is a `Slice[T]` VIEW, but the indexed-receiver path treated
/// every `Index` receiver as an ELEMENT access: it registered the synth
/// binding from the container's element type (so a `Vec[i64]` window
/// looked like a scalar `i64`) and GEP'd to one element rather than
/// building a `{ptr, len}` header. Every method on such a receiver then
/// hit the loud "no handler on variable '__indexed_elem_N'" fall-through
/// while `--interp` ran the program — method-agnostic, which is why
/// `len()` is pinned here alongside the method this row adds.
#[test]
fn test_e2e_range_index_receiver_is_a_slice_view() {
    let src = r#"
fn main() {
    let nums: Vec[i64] = [10i64, 20i64, 30i64, 40i64, 50i64];
    println(f"01 {nums[1..4].len()}");
    println(f"02 {nums[1..4].is_empty()} {nums[2..2].is_empty()}");
    let w = nums[1..4].to_vec();
    println(f"03 {w.len()} {w[0i64]} {w[2i64]}");
    match nums[3..5].first() { Some(x) => println(f"04 {x}"), None => println("04 none") }
    println(f"05 {nums[0..3].contains(20i64)}");
    let words: Vec[String] = ["alpha", "beta", "gamma"];
    let ws = words[1..3].to_vec();
    println(f"06 {words[1..3].len()} {ws[1i64]}");
}
"#;
    assert_eq!(
        run_program(src).as_deref(),
        Some(
            "01 3\n\
                 02 false true\n\
                 03 3 20 40\n\
                 04 40\n\
                 05 true\n\
                 06 2 gamma\n"
        ),
    );
}

/// For-loop binding over `Vec[Struct]` + struct-field access regression
/// (cond_simple bug, 2026-05-16).
///
/// `for x in xs.iter() { ... x.val ... }` where `xs: Vec[N]` (plain
/// struct) used to silently produce `i64 0` for every `x.val` read:
/// the for-loop's `register_for_loop_bindings` populated
/// `vec_elem_types[x]` from the source's element TypeExpr but did
/// not populate `var_type_names[x]`, so `field_index_for(x, "val")`
/// — which keys off `var_type_names` — returned `None`, and the
/// generic `compile_field_access` tail fell through to the
/// `Ok(const_int(0))` default.
///
/// The same gap caused the surrounding-shape repros (cond_simple /
/// with_for / loop_shape) to print only the seed-count: the
/// in-for-loop `if x.val > 0 { q.push_back(x) }` compiled to
/// `br i1 false, label %then, label %else` so the conditional push
/// never ran, and the outer `loop { q.pop_front() }` drained on
/// the second iteration. Fix wires `var_type_names` through
/// `register_var_from_type_expr` for any bare user-type-named
/// TypeExpr path (struct / shared struct / enum).
#[test]
fn test_e2e_for_iter_struct_field_access_resolves() {
    let src = r#"
struct N { val: i64 }
fn main() {
    let mut xs: Vec[N] = Vec.new();
    xs.push(N { val: 10 });
    xs.push(N { val: 20 });
    let mut sum: i64 = 0;
    for x in xs.iter() {
        sum = sum + x.val;
    }
    println(sum);
}
"#;
    if let Some(out) = run_program(src) {
        assert_eq!(
            out, "30\n",
            "for x in Vec[Struct].iter() {{ x.field }} — field access must resolve, \
                 not silently fold to 0"
        );
    }
}

/// `for x in obj.field.iter()` where `obj` is a known shared struct
/// and `field` is a `Vec[T]`. Pre-fix the `FieldAccess` receiver
/// fell through `compile_for`'s dispatch match to the `_ =>` arm
/// (no recognised iterable shape), so the for-body was silently
/// elided — outer mutations of `count` / `q` looked unchanged
/// and the outer-loop drained on iteration 2. Closes the
/// `for nb in curr.neighbors.iter()` surface used by the
/// clone-graph kata (kata-133).
#[test]
fn test_e2e_for_iter_shared_struct_field_vec() {
    let src = r#"
shared struct Node {
    val: i64,
    mut neighbors: Vec[Node],
}
fn main() {
    let n1 = Node { val: 1, neighbors: Vec.new() };
    let n2 = Node { val: 2, neighbors: Vec.new() };
    let n3 = Node { val: 3, neighbors: Vec.new() };
    n1.neighbors.push(n2);
    n1.neighbors.push(n3);
    let mut sum: i64 = 0;
    for nb in n1.neighbors.iter() {
        sum = sum + nb.val;
    }
    println(sum);
}
"#;
    if let Some(out) = run_program(src) {
        assert_eq!(
            out, "5\n",
            "for x in shared_struct.field.iter() must iterate the embedded Vec, \
                 not skip the loop body"
        );
    }
}

#[test]
fn test_e2e_let_bound_range_for_loop_iterates() {
    // B-2026-08-17-29 — the range-valued sibling of the module-binding bug
    // above, and it failed at the SAME `compile_for` Identifier arm: a name
    // bound to a range matched no container table and fell through. All
    // three iterable range forms from design.md § Loops' range-literal
    // table are covered, each against the equivalent INLINE range in the
    // same program — the inline control is what proves the loop shape
    // itself is fine and only the binding was dropped.
    let output = run_program(
        "fn main() {\n\
                 let r1 = 0..5;\n\
                 let mut a = 0;\n\
                 for i in r1 { a = a + i; }\n\
                 let mut ac = 0;\n\
                 for i in 0..5 { ac = ac + i; }\n\
                 let r2 = 0..=4;\n\
                 let mut b = 0;\n\
                 for i in r2 { b = b + i; }\n\
                 let lo = 2;\n\
                 let hi = 6;\n\
                 let r3 = lo..hi;\n\
                 let mut c = 0;\n\
                 for i in r3 { c = c + i; }\n\
                 println(a);\n\
                 println(ac);\n\
                 println(b);\n\
                 println(c);\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "10\n10\n10\n14\n");
}

#[test]
fn test_e2e_let_bound_range_empty_and_reused() {
    // Degenerate bounds and multi-use. An empty range must run zero times
    // for the RIGHT reason (start == end), which is indistinguishable from
    // the old bug's output on its own — hence pairing it with the
    // single-element inclusive range and the nested reuse, which the bug
    // could not have produced. Reuse also pins that the binding is not
    // consumed by the first loop: the captured bounds are re-read, so
    // `for i in r { for j in r { } }` is 3 x 3.
    let output = run_program(
        "fn main() {\n\
                 let e = 5..5;\n\
                 let mut n = 0;\n\
                 for i in e { n = n + 1; }\n\
                 let one = 3..=3;\n\
                 let mut m = 0;\n\
                 for i in one { m = m + 1; }\n\
                 let r = 0..3;\n\
                 let mut k = 0;\n\
                 for i in r { for j in r { k = k + 1; } }\n\
                 println(n);\n\
                 println(m);\n\
                 println(k);\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "0\n1\n9\n");
}

#[test]
fn test_e2e_let_bound_range_control_flow_and_labels() {
    // The bound range reaches the loop through a different entry point
    // than an inline one, so pin that everything hanging off the loop
    // frame still works: `break`, `continue`, a labeled `continue` from a
    // nested loop over a SECOND bound range, and a bound range inside a
    // function other than `main`. All four share one emitted loop shape
    // with the inline path (`compile_for_range_values`), so a divergence
    // here would mean the label or the frame was dropped on the way in.
    let output = run_program(
        "fn sum_to(n: i64) -> i64 {\n\
                 let r = 0..n;\n\
                 let mut t = 0;\n\
                 for i in r { t = t + i; }\n\
                 return t;\n\
             }\n\
             fn main() {\n\
                 println(sum_to(5));\n\
                 let r = 0..10;\n\
                 let mut b = 0;\n\
                 for i in r {\n\
                     if i == 4 { break; }\n\
                     if i % 2 == 0 { continue; }\n\
                     b = b + i;\n\
                 }\n\
                 println(b);\n\
                 let outer = 0..3;\n\
                 let inner = 0..3;\n\
                 let mut c = 0;\n\
                 o: for i in outer {\n\
                     for j in inner {\n\
                         if j == 2 { continue o; }\n\
                         c = c + 1;\n\
                     }\n\
                 }\n\
                 println(c);\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "10\n4\n6\n");
}

#[test]
fn test_e2e_shadow_collection_to_scalar() {
    // Vec→scalar shadow with RHS referencing the old Vec. The old
    // `vec_elem_types` tag must be purged so `println(v)` formats an i64.
    if let Some(out) = run_program(
        "fn main() {\n\
             let mut v: Vec[i64] = Vec.new();\n\
             v.push(10i64);\n\
             v.push(20i64);\n\
             v.push(30i64);\n\
             let v = v.len();\n\
             println(v);\n\
             }",
    ) {
        assert_eq!(out, "3\n");
    }
}

#[test]
fn test_e2e_shadow_in_for_loop_over_collection_var() {
    // A for-loop binding that shadows an in-scope String with a scalar
    // element. The purge lives in `bind_pattern` (the for-loop's choke
    // point), so the loop var `s` dispatches as i64 inside the body even
    // though an outer `s` was a String — `sum + s` and `println(s)` both
    // treat `s` as i64. (This program did not compile before the fix: the
    // old `bind_pattern` guard rejected the scalar-over-String rebind.
    // Codegen's function-flat `variables` map does not restore the outer
    // `s` after the loop, an orthogonal pre-existing scoping limitation,
    // so the test asserts only the in-loop dispatch.)
    if let Some(out) = run_program(
        "fn main() {\n\
             let s = \"outer\";\n\
             let mut sum = 0i64;\n\
             for s in [1i64, 2i64, 3i64] {\n\
                 println(s);\n\
                 sum = sum + s;\n\
             }\n\
             println(sum);\n\
             }",
    ) {
        assert_eq!(out, "1\n2\n3\n6\n");
    }
}

#[test]
fn test_e2e_iter_axis_row_view_bind() {
    // B-2026-07-13-7: `let r = rows[i]` binding a sub-tensor out of a
    // `Vec[Tensor]` (`iter_axis` result) deep-clones the whole tensor block
    // — a shallow pointer copy aliased the container's element and both
    // freed the same block (`free(): double free detected in tcache 2`
    // under JIT/native; interpreter correct). Covers axis 0 and 1, a
    // rank-3→rank-2 reduction, and multiple bound row views.
    let src = r#"
fn main() {
    let m: Tensor[f32, [2, 3]] = Tensor.from([[1.0f32, 2.0f32, 3.0f32], [4.0f32, 5.0f32, 6.0f32]]);
    let rows = m.iter_axis(0);
    let r0 = ref rows[0];
    let r1 = ref rows[1];
    println(r0.sum());
    println(r1.sum());
    let cols = m.iter_axis(1);
    let c0 = ref cols[0];
    println(c0.sum());
    let t: Tensor[i64, [2, 2, 2]] = Tensor.from([[[1, 2], [3, 4]], [[5, 6], [7, 8]]]);
    let planes = t.iter_axis(0);
    let p1 = ref planes[1];
    println(p1.sum().to_string());
}
"#;
    let out = run_program(src).expect("program should compile and run");
    assert_eq!(out, "6\n15\n5\n26\n");
}

#[test]
fn test_e2e_unsigned_refinement_still_rejects_out_of_range() {
    // THE ANTI-VACUITY GUARD: the fix must select the right predicate,
    // NOT weaken the check.
    //
    // The value must reach the constructor through a PARAMETER. A literal
    // (`Small(200)`) is folded by the const-evaluable arm and rejected at
    // compile time with E_REFINEMENT_PREDICATE_VIOLATION, so it never
    // reaches the runtime predicate this bug lived in — a literal here
    // would assert nothing about the emitted compare. (The check-gate in
    // tests/common caught exactly that when this test was first written.)
    let out = run_program(
        r#"
distinct type Small = u8 where self <= 100;
fn make(x: u8) -> Small { Small(x) }
fn main() {
    let s = make(200);
    println("built");
}
"#,
    );
    if let Some(out) = out {
        assert!(
            !out.contains("built"),
            "out-of-range value was accepted — the predicate is now vacuous: {out}"
        );
    }
}

/// B-2026-09-01-19 — the shapes that stay ACCEPTED around the new range check
/// agree on every backend.
///
/// The rejected shapes have no runtime left to compare, so what needs pinning
/// is the boundary the check draws: an in-range suffixed literal in a seeded
/// payload slot still compiles and still means what it says, and the `as u8`
/// an author writes to mean the truncation gives the same 44 everywhere rather
/// than the `300`/`44` split the unchecked literal used to.
/// `255i64` sits on the boundary itself, which is where an off-by-one in the
/// range comparison would show. Verified byte-identical under
/// `karac run --interp`, `karac run`, `karac build`, and
/// `KARAC_AUTO_PAR=0 karac build`.
#[test]
fn test_e2e_seeded_slot_range_check_boundary_agrees_across_backends() {
    assert_eq!(
        run_program(
            r#"fn main() {
    let e: Option[u8] = Option.Some(5i64);
    println(f"{e}");
    let g: Option[u8] = Option.Some(255i64);
    println(f"{g}");
    let h: Option[u8] = Option.Some(300i64 as u8);
    println(f"{h}");
}
"#
        ),
        Some("Some(5)\nSome(255)\nSome(44)\n".to_string())
    );
}

// ── Arithmetic flags (`nsw`) on the for-range induction variable ─────
//
// For an ascending exclusive range with the default step 1 (`for i in a..b`),
// the counter increment `i + 1` is reached only when `i < b`, so it provably
// never signed-overflows → `add nsw` (unlocks IV canonicalization /
// vectorization). An inclusive range or an explicit step cannot be proven
// wrap-free here, so those stay on a plain `add`.
#[test]
fn for_range_induction_variable_nsw() {
    let ir = ir_for(
        r#"
fn excl(n: i64) -> i64 { let mut s = 0; for i in 0..n { s = s + i; } return s; }
fn incl(n: i64) -> i64 { let mut s = 0; for i in 0..=n { s = s + i; } return s; }
fn stepped(n: i64) -> i64 { let mut s = 0; for i in (0..n).step_by(2) { s = s + i; } return s; }
fn main() { print(0); }
"#,
    );
    // Exclusive step-1 range → the counter increment is `add nsw`.
    assert!(
        fn_body(&ir, "@excl(").contains("%incr = add nsw i64"),
        "exclusive step-1 for-range counter should be `add nsw`:\n{}",
        fn_body(&ir, "@excl(")
    );
    // Inclusive range → plain add (the final `+1` can reach `end + 1`).
    let incl = fn_body(&ir, "@incl(");
    assert!(
        incl.contains("%incr = add i64") && !incl.contains("%incr = add nsw"),
        "inclusive for-range counter must stay a plain add:\n{incl}"
    );
    // Explicit step → plain add (cannot prove wrap-free here).
    let stepped = fn_body(&ir, "@stepped(");
    assert!(
        stepped.contains("%incr = add i64") && !stepped.contains("%incr = add nsw"),
        "stepped for-range counter must stay a plain add:\n{stepped}"
    );
}

#[test]
fn for_collection_iteration_counter_nsw() {
    // arith-flags P3: a `for x in <collection>` loop's synthetic index
    // counter runs only under `cur < len` (len ≤ isize::MAX), so `cur + 1`
    // never signed-overflows → `add nsw` (the same wrap-free-affine-IV win
    // as the for-range induction variable). Covers the Vec and Slice
    // element-iteration forms.
    let ir = ir_for(
        r#"
fn sum_vec(v: Vec[i64]) -> i64 { let mut s = 0; for x in v { s = s + x; } return s; }
fn sum_slice(v: mut Slice[i64]) -> i64 { let mut s = 0; for x in v { s = s + x; } return s; }
fn main() { print(0); }
"#,
    );
    let vec_body = fn_body(&ir, "@sum_vec(");
    assert!(
        vec_body.contains("%incr = add nsw i64"),
        "Vec for-loop counter should be `add nsw`:\n{vec_body}"
    );
    let slice_body = fn_body(&ir, "@sum_slice(");
    assert!(
        slice_body.contains("%incr = add nsw i64"),
        "Slice for-loop counter should be `add nsw`:\n{slice_body}"
    );
}

/// B-2026-08-17-25 fallout — a RANGE subscript passed by `ref` to a
/// trait-bound generic used to reach `ref_arg_index_borrow_ptr`, which
/// borrows ONE ELEMENT. It compiled the range as the subscript, got the
/// catch-all's 0, and produced `&v[0]`: the correct address for a slice
/// starting at 0 and the wrong one for any other. The existing fixture
/// only ever passed `v[0..2]` to an impl that returns a literal, so
/// neither half of that was observable.
///
/// Pinned on the header's LENGTH, which is the part a bare element
/// pointer cannot carry: two ranges of DIFFERENT length over the same Vec
/// must report their own, and the `--interp` oracle agrees.
#[test]
fn a_range_subscript_by_ref_passes_a_slice_not_an_element_pointer() {
    let src = "trait Sized2 { fn size(ref self) -> i64; }\n\
                   impl Sized2 for Slice[i64] { fn size(ref self) -> i64 { return self.len(); } }\n\
                   fn show[T: Sized2](x: ref T) -> i64 { return x.size(); }\n\
                   fn main() {\n\
                       let mut v: Vec[i64] = Vec.new();\n\
                       v.push(10); v.push(20); v.push(30); v.push(40);\n\
                       println(show(v[0..2]));\n\
                       println(show(v[1..4]));\n\
                   }\n";
    let Some(out) = run_program(src) else {
        return;
    };
    assert_eq!(
        out, "2\n3\n",
        "each range must carry its own length — a one-element borrow has none"
    );
}

/// B-2026-08-18-18 — `collect()` into a non-`Vec` target in RETURN position
/// and in a function body's TAIL, end to end.
///
/// B-2026-08-17-36 delivered design.md's "infers the target type from
/// context" for an annotated `let` only; these two positions still fixed the
/// chain to `Vec` and failed with "expected 'Set[i64]', found 'Vec[i64]'".
/// The rewrite is the SAME pre-typecheck desugar that row introduced, so
/// interpreter/JIT/AOT parity holds by construction — the only new input is
/// which type to aim at.
///
/// THE CLOSURE FUNCTION IS THE GUARD, not filler. A `return` inside a
/// closure returns from the CLOSURE, so the enclosing fn's declared return
/// type must not reach it; aiming at `Set[i64]` there would rewrite a
/// closure that genuinely yields `Vec[i64]`, and it would look correct
/// until the closure's own type mattered. The target is dropped on entry to
/// a closure body, and this pins that.
///
/// Receivers are BOUND before the `.len()` call on purpose: calling a read
/// method directly on a returned `Set`/`Map` temp is a separate, unrelated
/// defect, and threading it through here would make this test fail for a
/// reason it is not about.
#[test]
fn collect_reaches_a_non_vec_target_in_return_and_tail_position() {
    let src = "fn build_ret(v: Vec[i64]) -> Set[i64] {\n\
                       return v.iter().map(|x| x * 2).collect();\n\
                   }\n\
                   fn build_tail(v: Vec[i64]) -> VecDeque[i64] {\n\
                       v.iter().map(|x| x + 1).collect()\n\
                   }\n\
                   fn apply(f: Fn(i64) -> Vec[i64], n: i64) -> Vec[i64] { return f(n); }\n\
                   fn closure_return_is_the_closures(v: Vec[i64]) -> Set[i64] {\n\
                       let f = |x: i64| { return v.iter().map(|y| y + x).collect(); };\n\
                       let inner = apply(f, 10);\n\
                       let mut out: Set[i64] = Set.new();\n\
                       for e in inner { out.insert(e); }\n\
                       return out;\n\
                   }\n\
                   fn main() {\n\
                       let v: Vec[i64] = [1, 2, 3];\n\
                       let a = build_ret(v);\n\
                       println(a.len().to_string());\n\
                       let b = build_tail(v);\n\
                       println(b.len().to_string());\n\
                       let c = closure_return_is_the_closures(v);\n\
                       println(c.len().to_string());\n\
                   }\n";
    let Some(out) = run_program(src) else {
        return;
    };
    assert_eq!(
        out, "3\n3\n3\n",
        "a `collect()` in return or tail position must build the declared target, \
             and a closure's own `return` must not be aimed at the fn's type"
    );
}

/// B-2026-08-18-27 — `collect()` into a non-`Vec` target in ARGUMENT
/// position, the last of design.md's three "infers the target type from
/// context" positions. `f(<chain>.collect())` reported
/// "expected 'Set[i64]', found 'Vec[i64]'" while the annotated `let`
/// (B-2026-08-17-36) and return/tail (B-2026-08-18-18) both worked.
///
/// THE `ambush` CALL IS THE TEST, not the first four lines. A local
/// `let ambush = |x: Vec[i64]| ...` shadows the top-level `fn ambush(s:
/// Set[i64])` and is callable identically, so a rewrite that trusts the
/// name would aim the argument at `Set[i64]` and turn a program that
/// compiles today into a typecheck error — a regression traded for a
/// feature. The desugar collects every name the body binds before it
/// rewrites anything, and stands down on a hit. `6` here is the shadowed
/// closure's answer (`Vec` length, dups kept); `3` would mean the guard
/// is gone. The last line prints the VEC length 4, not the set size 3.
///
/// THE TWO `take_str` CALLS pin the other half. Both aim at one `String`
/// parameter, so both rewrites derive from the SAME declared type — but
/// their elements differ (`String` from `w.iter()`, `char` from
/// `s.chars()`). The synthesized nodes take their spans from the ARGUMENT
/// rather than the parameter for exactly this reason: shared spans would
/// record `char` and `String` under one key and let the last write win.
#[test]
fn collect_reaches_a_non_vec_target_in_argument_position() {
    let src = "fn take_set(s: Set[i64]) -> i64 { return s.len() as i64; }\n\
                   fn take_deque(d: VecDeque[i64]) -> i64 { return d.len() as i64; }\n\
                   fn take_map(m: Map[i64, i64]) -> i64 { return m.len() as i64; }\n\
                   fn take_str(s: String) -> i64 { return s.len() as i64; }\n\
                   fn ambush(s: Set[i64]) -> i64 { return s.len() as i64; }\n\
                   fn main() {\n\
                       let v: Vec[i64] = [3, 1, 3, 2];\n\
                       println(take_set(v.iter().collect()).to_string());\n\
                       println(take_deque(v.iter().map(|x| x + 1).collect()).to_string());\n\
                       println(take_map(v.iter().map(|x| (x, x * 2)).collect()).to_string());\n\
                       let w: Vec[String] = [\"ab\", \"cd\"];\n\
                       println(take_str(w.iter().collect()).to_string());\n\
                       let s: String = \"xyz\";\n\
                       println(take_str(s.chars().collect()).to_string());\n\
                       let ambush = |x: Vec[i64]| x.len() as i64;\n\
                       println(ambush(v.iter().collect()).to_string());\n\
                   }\n";
    let Some(out) = run_program(src) else {
        return;
    };
    assert_eq!(
        out, "3\n4\n3\n4\n3\n4\n",
        "a `collect()` argument must build the callee's declared parameter type, \
             and must stand down when a local binding shadows the callee's name"
    );
}

/// B-2026-08-20-38 — design.md § Slices' second example, end to end. A
/// sub-range handed to a `mut Slice[T]` parameter produced a READ-ONLY
/// header, so `sort_in_place(mut v[1..4])` failed `karac check` with
/// "expected 'mut Slice[i64]', found 'Slice[i64]'" while the line above it
/// in the same spec block (`sort_in_place(mut v)`) worked on every
/// backend. There was no other spelling: `let s = mut v[1..4]` is not
/// syntax.
///
/// The typechecker half is pinned in tests/typechecker.rs. This is the
/// half that matters for correctness: the write has to land in the
/// ORIGINAL buffer, at the right OFFSET, and touch nothing outside the
/// range. `[3,1,4,1,5]` sorted over `1..4` is `3 [1,1,4] 5` — the
/// untouched endpoints are load-bearing, since a header built at the wrong
/// offset or over a copy would still print five sorted-looking numbers.
#[test]
fn test_e2e_mut_sub_range_slice_writes_through_to_the_backing_buffer() {
    let src = r#"
fn sort_in_place(xs: mut Slice[i64]) {
    let n = xs.len();
    let mut i = 0i64;
    while i < n {
        let mut j = i + 1i64;
        while j < n {
            if xs[j] < xs[i] {
                let t = xs[i];
                xs[i] = xs[j];
                xs[j] = t;
            }
            j = j + 1i64;
        }
        i = i + 1i64;
    }
}

fn poke(xs: mut Slice[i64]) { xs[0] = 99; }

fn main() {
    let mut v: Vec[i64] = [3, 1, 4, 1, 5];
    sort_in_place(mut v[1..4]);
    println(f"{v[0]} {v[1]} {v[2]} {v[3]} {v[4]}");

    // The offset is the point: writing index 0 of `v[2..4]` must hit v[2].
    let mut w: Vec[i64] = [1, 2, 3, 4];
    poke(mut w[2..4]);
    println(f"{w[0]} {w[1]} {w[2]} {w[3]}");

    // Same for an `Array[T, N]` base — the other source the coercion table
    // lists.
    let mut a: Array[i64, 4] = [1, 2, 3, 4];
    poke(mut a[1..3]);
    println(f"{a[0]} {a[1]} {a[2]} {a[3]}");

    // A read-only sub-range is untouched by the change.
    let r: Vec[i64] = [1, 2, 3, 4];
    println(sum(r[1..3]));
}

fn sum(xs: Slice[i64]) -> i64 {
    let mut acc = 0i64;
    for x in xs { acc = acc + x; }
    acc
}
"#;
    assert_eq!(
        run_program(src).as_deref(),
        Some("3 1 1 4 5\n1 2 99 4\n1 99 3 4\n5\n")
    );
}

/// A sub-range of an already-mutable view forwards through a second call
/// without a marker, and the write still reaches the original `Vec`'s
/// buffer two frames down. Pins the arm of `is_arg_forwarded` that must
/// keep seeing through a range index to the place root.
#[test]
fn test_e2e_mut_sub_range_of_a_mut_slice_forwards() {
    let src = r#"
fn poke(xs: mut Slice[i64]) { xs[0] = 77; }
fn middle(s: mut Slice[i64]) { poke(s[1..3]); }

fn main() {
    let mut v: Vec[i64] = [1, 2, 3, 4, 5];
    middle(mut v[1..5]);
    println(f"{v[0]} {v[1]} {v[2]} {v[3]} {v[4]}");
}
"#;
    // `v[1..5]` views [2,3,4,5]; its `[1..3]` views [3,4]; writing index 0
    // of that lands on v[2].
    assert_eq!(run_program(src).as_deref(), Some("1 2 77 4 5\n"));
}
