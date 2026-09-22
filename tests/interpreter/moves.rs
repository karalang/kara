//! moves, owned values, fresh temporaries, discarded values, clones -- fixtures for `tests/interpreter.rs`.
//!
//! Split out of `tests/interpreter.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test interpreter` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test interpreter moves::
//!
//! New fixtures about moves, owned values, fresh temporaries, discarded values, clones belong in this file.

use super::*;

#[test]
fn test_clone_on_primitives() {
    // `clone` on a Copy primitive is identity — used to ICE (no dispatch arm).
    assert_eq!(run("fn main() { println((7i64).clone()); }"), "7\n");
    assert_eq!(run("fn main() { println((2.5f64).clone()); }"), "2.5\n");
    assert_eq!(run("fn main() { println(true.clone()); }"), "true\n");
    assert_eq!(run("fn main() { println('q'.clone()); }"), "q\n");
}

#[test]
fn test_variable_not_leaked_from_block() {
    // After a block, variables defined inside it should not be accessible
    // (the interpreter panics on undefined variable, so we test indirectly)
    assert_eq!(
        run("fn main() {\n\
                 let before = 10;\n\
                 { let inner = 20; }\n\
                 println(before);\n\
             }"),
        "10\n"
    );
}

#[test]
fn test_owned_self_method_is_not_written_back() {
    // A consuming (owned) `self` receiver must NOT trigger write-back — the
    // gate is `MutRef`-only. A `ref self` reader is likewise untouched.
    let out = run_no_errors(
        r#"
struct Counter { n: i64 }
impl Counter {
    fn into_n(self) -> i64 { self.n }
    fn peek(ref self) -> i64 { self.n }
}
fn main() {
    let c = Counter { n: 7 };
    println(c.peek());
    println(c.into_n());
}
"#,
    );
    assert_eq!(out, "7\n7\n");
}

/// A hand-written `fn clone` must WIN over the derive-driven synthesis on both
/// surfaces: the typechecker declines the built-in arm when `env.find_method`
/// resolves one, so the interpreter's arm has to decline too or it would shadow
/// the body the typechecker resolved.
#[test]
fn test_user_clone_impl_wins_over_derive_synthesis() {
    assert_eq!(
        run("struct S { n: i64 }\n\
             impl S { fn clone(ref self) -> S { S { n: self.n + 100 } } }\n\
             let s = S { n: 1 };\n\
             println(f\"{s.clone().n}\");\n"),
        "101\n"
    );
}

/// B-2026-07-31 (container-bodies move disarm) — a WHOLE-VALUE move of a
/// binding that carries a container-bodies walk (enum payload, Vec element,
/// tuple element) fires the payload's `Drop` body exactly ONCE, at the
/// destination. Before the disarm, every move shape here double-fired on the
/// interpreter, and under codegen the second fire read the cap-zeroed
/// moved-from slot (`self.id` printed 0 — a silently wrong value, not a
/// leak). Shapes pinned: rebind, tuple rebind, return-move, Vec rebind,
/// reassign, by-value call arg (the pre-existing balanced case).
///
/// Deliberate residual, asserted by ABSENCE: `f = g;` never runs the body of
/// f's OVERWRITTEN original (no 95 in the expected string) — both backends
/// share that silence today, a leak rather than a divergence.
///
/// Same source and expected string as `tests/codegen.rs`'s
/// `e2e_container_bodies_whole_value_move_single_fire` — the pair is the
/// parity contract.
#[test]
fn test_container_bodies_whole_value_move_single_fire() {
    assert_eq!(
        run("struct Res { id: i64 }\n\
             impl Drop for Res { fn drop(mut ref self) { println(90 + self.id); } }\n\
             enum Slot { Empty, Held(Res) }\n\
             fn make() -> Slot {\n\
                 let m = Slot.Held(Res { id: 3 });\n\
                 return m;\n\
             }\n\
             fn consume(s: Slot) {\n\
                 println(70);\n\
             }\n\
             fn main() {\n\
                 let a = Slot.Held(Res { id: 1 });\n\
                 let a2 = a;\n\
                 println(1);\n\
                 let t = (Res { id: 2 }, 10);\n\
                 let t2 = t;\n\
                 println(2);\n\
                 let c = make();\n\
                 println(3);\n\
                 let v: Vec[Res] = [Res { id: 4 }];\n\
                 let v2 = v;\n\
                 println(4);\n\
                 let mut f = Slot.Held(Res { id: 5 });\n\
                 let g = Slot.Held(Res { id: 6 });\n\
                 f = g;\n\
                 println(5);\n\
                 let b = Slot.Held(Res { id: 7 });\n\
                 consume(b);\n\
                 println(6);\n\
             }\n"),
        // `95` is the reassign target's OVERWRITTEN original firing at the
        // assignment — originally a shared residual silence, closed by the
        // B-2026-07-30-11 enum-assign displacement leg.
        "91\n1\n92\n2\n93\n3\n94\n4\n95\n96\n5\n70\n97\n6\n"
    );
}

/// B-2026-08-31-7 — A BARE-TUPLE ELEMENT BOUND OUT OF AN OWNED PARAM IS A VIEW,
/// AND A REBIND OF IT MUST INHERIT THAT.
///
/// `match t { (r, k) => { let m = r; … } }` over `fn s1(t: (R, i64))` printed
/// `b1 dR1 dR1` on all three compiled surfaces against this backend's correct
/// `b1 dR1`; the PROJECTED spelling `match s.t { … }` over `fn s1c(s: W)`
/// printed `b3 dR3 dR3` on ALL FOUR, agreed and — by one-value-one-body —
/// agreed-wrong. One `R` is constructed and passed by value, so one body is due
/// in each.
///
/// The two halves are one mechanism seen from two sides. Codegen wrote
/// `param_view_locals` only from the VARIANT-payload site, so a bare-tuple
/// element never became a view and `let m = r` minted a second owner; this
/// backend's projection branch admitted only `TupleVariant`/`Struct` patterns,
/// so it minted one too for the `s.t` spelling. Its bare-identifier branch has
/// always had the wider reach, which is why `param` alone diverged.
///
/// THE CONTROLS ARE THE POINT, because the fix WITHHOLDS a body and the failure
/// mode of over-reaching is a body that never runs:
/// - `norebind` / `norebind_proj` — the same arms without the rebind, correct
///   before and after. They put the axis on the rebind rather than on the
///   pattern, and prove the element WALK still owns the single body.
/// - `consumed` — the arm moves the element into a by-value callee. Already at
///   one body, and marking `r` a view must not disturb the transfer.
/// - `local` — a LOCAL tuple, not a param. Nobody else owns it, so
///   caller-retains does not apply and this shape was deliberately NOT fixed
///   here. It was pinned at TWO bodies so the marking could not be quietly
///   widened to locals without a measurement. B-2026-09-02-26 supplied that
///   measurement and the cell now runs ONE — but the marking was still not
///   widened: that row RETRACTS the tuple's element walk for the moved element
///   and lets the rebind own the body, the OPPOSITE direction of this row's
///   repair. The cell stays pinned here as the guard that the two do not
///   overlap into a body that runs nowhere.
///
/// The ENUM twins of the two fixed shapes were already correct on all four
/// surfaces before this row, which is what shows the tuple family was simply
/// behind the enum family rather than that a new rule was invented.
///
/// Twin of `tests/codegen.rs`'s
/// `e2e_bare_tuple_element_of_owned_param_is_a_view`, pinned to the same string.
#[test]
fn test_bare_tuple_element_of_owned_param_is_a_view() {
    assert_eq!(
        run(r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct W { t: (R, i64) }

fn sink(x: R) { println(f"  sank{x.id}") }

fn s1(t: (R, i64))       { match t { (r, k) => { let m = r; println(f"  b{m.id}"); } } }
fn s1b(t: (R, i64))      { match t { (r, k) => { println(f"  b{r.id}"); } } }
fn s1c(s: W)             { match s.t { (r, k) => { let m = r; println(f"  b{m.id}"); } } }
fn s1d(s: W)             { match s.t { (r, k) => { println(f"  b{r.id}"); } } }
fn s1e(t: (R, i64))      { match t { (r, k) => { sink(r); } } }
fn s1f() { let t = (R { id: 6 }, 0); match t { (r, k) => { let m = r; println(f"  b{m.id}"); } } }

fn main() {
    println("param");          s1((R { id: 1 }, 0));           println("param end");
    println("norebind");       s1b((R { id: 2 }, 0));          println("norebind end");
    println("proj");           s1c(W { t: (R { id: 3 }, 0) }); println("proj end");
    println("norebind_proj");  s1d(W { t: (R { id: 4 }, 0) }); println("norebind_proj end");
    println("consumed");       s1e((R { id: 5 }, 0));          println("consumed end");
    println("local");          s1f();                          println("local end");
    println("done");
}
"#),
        r#"param
  b1
dR1
param end
norebind
  b2
dR2
norebind end
proj
  b3
dR3
proj end
norebind_proj
  b4
dR4
norebind_proj end
consumed
  sank5
dR5
consumed end
local
  b6
dR6
local end
done
"#
    );
}

/// B-2026-09-02-25 — THE `let` SPELLING OF B-2026-08-31-7's RULE, which the
/// `match` spelling got and this one did not.
///
/// `let (r, k) = t;` over an owned TUPLE PARAM binds views of the callee's entry
/// copy exactly as `match t { (r, k) => … }` does, so a later whole rebind
/// (`let m = r;`) must inherit the withholding. It did not: `param` below printed
/// `b1 dR1 dR1` where one body is due — agreed on all four surfaces — while
/// `arm`, the identical signature written as a match, was already correct at one.
///
/// TWO HALVES, LANDED TOGETHER because either alone is a divergence:
/// - THIS BACKEND: `let_destructures_owned_param` returned `true` for a `Tuple`
///   pattern over an owned-param RHS — correctly retracting the destructure's OWN
///   slots — without inserting the bound names into `owned_param_names_stack` the
///   way every sibling branch does. So `r` was not a known view and the rebind
///   minted a second owner.
/// - CODEGEN: `place_source_tuple_leaf_cleanups` already registers such a leaf for
///   MEMORY only, but never recorded it in `param_view_locals`, so the let-site's
///   `rhs_is_param_view` said no.
///
/// `letelse` is the cell that shows the two halves are one rule: codegen was
/// ALREADY at one body there and this backend was the lone doubler, so it
/// converges from this side alone.
///
/// THE PINNED-AT-TWO CELLS ARE THE POINT, and each is held back by a measurement
/// rather than by taste. All four surfaces agree at two bodies on every one of
/// them, and marking a view on ONE side would turn that agreement into a
/// run-vs-build split:
/// - `structpat` — `let S { r, k } = s;`. FIXED SINCE, by B-2026-09-02-38, and
///   the reason it was pinned turned out to hold for a different source than
///   this one. `finish_owned_struct_destructure` does TRANSFER the body to the
///   leaf rather than leaving it with the source — for a LOCAL source. For the
///   PARAM source this cell uses it does not: the transfer is gated on
///   `var_owns_struct_field_bodies`, i.e. on the source having a
///   `StructFieldBodies` action, and a by-value param has none. Measured with an
///   END-OF-CALLEE marker, the body here fires AFTER that marker on both
///   backends (the source's owner) where the local spelling fires it BEFORE
///   (the leaf's live-range end) — so a view mark is exactly right, and -38
///   lifted both sides together. One body here now, and
///   `test_struct_pattern_destructure_of_owned_param_is_a_view` pins the widened
///   shape in full.
/// - `nested` — `let ((r, a), b) = t;`. FIXED SINCE, by B-2026-09-02-39: a tuple
///   PARAM used to register no `tuple_var_elem_type_exprs`, so its nested
///   element resolved to an EMPTY `TypeExpr` and the compiled recursion never
///   reached the leaf. Registering the param's declared element types made it
///   reachable, and the two backends were lifted together in that commit — one
///   body here now. Kept in this list because the cell is what proved the
///   restriction was a REACHABILITY limit rather than an ownership judgement.
/// - `viewsrc` — `let t2 = t; let (r, k) = t2;`. FIXED SINCE, by
///   B-2026-09-02-44, which supplied the missing half rather than lifting a
///   judgement: codegen's `owner_runs_bodies` tested `current_fn_param_names`
///   alone, so a root that had INHERITED view-ness (`t2`) answered no and its
///   leaf took the body. Widening that test to consult `param_view_locals` —
///   the mark `let t2 = t;` was already writing — let the interpreter's
///   propagation read the full `owned_param_names_stack` instead of the seeded
///   subset, and the two moved together. THE REASON RECORDED HERE HAD GONE
///   STALE: it said codegen could not see through a tuple whole-rebind at all,
///   citing `t2.0.id`, and that spelling measures one body on all four surfaces
///   today. The restriction outlived its cause, which is the argument for
///   re-measuring a pin's stated reason rather than only the cell it guards.
/// - `proj` — `let (r, k) = h.pe;`. FIXED SINCE, by B-2026-09-02-40. A
///   projection source already satisfied codegen's `owner_runs_bodies` (its root
///   is a param); only this backend's gate wanted a bare identifier, and
///   codegen's marking was narrowed to identifiers purely to match it. -40
///   taught this side the field-chain shape and lifted both together — one body
///   here now. Kept in this list because it is the cell that shows a pin can be a
///   MATCHING restriction rather than an ownership judgement, and
///   `test_projection_source_tuple_destructure_is_a_view` is where the widened
///   shape is pinned in full.
///
/// `norebind` and `local` are the over-reach controls: withholding a body fails
/// by running none, and both must stay at exactly one.
///
/// Twin of `tests/codegen.rs`'s
/// `e2e_let_tuple_destructure_of_owned_param_is_a_view`, pinned to the same
/// string.
#[test]
fn test_let_tuple_destructure_of_owned_param_is_a_view() {
    assert_eq!(
        run(r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct S { r: R, k: i64 }
struct H { pe: (R, i64) }
enum W { A(R), B }

fn s1(t: (R, i64))  { let (r, k) = t; let m = r; println(f"  b{m.id}") }
fn s2(t: (R, i64))  { let (r, k) = t; let m = r; let m2 = m; println(f"  b{m2.id}") }
fn s3(t: (R, R))    { let (r, q) = t; let m = r; println(f"  b{m.id}_{q.id}") }
fn s4(w: W)         { let W.A(r) = w else { return }; let m = r; println(f"  b{m.id}") }
fn s5(t: (R, i64))  { let (r, k) = t; println(f"  b{r.id}") }
fn s6(t: (R, i64))  { match t { (r, k) => { let m = r; println(f"  b{m.id}") } } }
fn s7()             { let t = (R { id: 7 }, 0); let (r, k) = t; let m = r; println(f"  b{m.id}") }
fn s8(s: S)         { let S { r, k } = s; let m = r; println(f"  b{m.id}") }
fn s9(t: ((R, i64), i64)) { let ((r, a), b) = t; let m = r; println(f"  b{m.id}") }
fn s10(t: (R, i64)) { let t2 = t; let (r, k) = t2; let m = r; println(f"  b{m.id}") }
fn s11(h: H)        { let (r, k) = h.pe; let m = r; println(f"  b{m.id}") }

fn main() {
    println("param");      s1((R { id: 1 }, 0));            println("param end")
    println("chained");    s2((R { id: 2 }, 0));            println("chained end")
    println("two");        s3((R { id: 3 }, R { id: 4 }));  println("two end")
    println("letelse");    s4(W.A(R { id: 5 }));            println("letelse end")
    println("norebind");   s5((R { id: 6 }, 0));            println("norebind end")
    println("arm");        s6((R { id: 8 }, 0));            println("arm end")
    println("local");      s7();                            println("local end")
    println("structpat");  s8(S { r: R { id: 9 }, k: 0 });  println("structpat end")
    println("nested");     s9(((R { id: 10 }, 0), 0));      println("nested end")
    println("viewsrc");    s10((R { id: 11 }, 0));          println("viewsrc end")
    println("proj");       s11(H { pe: (R { id: 12 }, 0) });println("proj end")
    println("done")
}
"#),
        r#"param
  b1
dR1
param end
chained
  b2
dR2
chained end
two
  b3_4
dR3
dR4
two end
letelse
  b5
dR5
letelse end
norebind
  b6
dR6
norebind end
arm
  b8
dR8
arm end
local
  b7
dR7
local end
structpat
  b9
dR9
structpat end
nested
  b10
dR10
nested end
viewsrc
  b11
dR11
viewsrc end
proj
  b12
dR12
proj end
done
"#
    );
}

/// B-2026-08-29-11 — a method frame's moved-out marks stay INSIDE it, and the
/// caller stands down on every by-value argument it hands over.
///
/// Both halves are one change. The interpreter's `moved_out_*` sets are keyed by
/// BINDING NAME with no frame qualifier, and the method path did not isolate a
/// callee's copies the way `eval_call` does for a free fn. Two frames that
/// happened to reuse a name therefore shared marks, in BOTH directions:
///
///   - a mark one method made outlived its frame and silenced an unrelated
///     LATER method's identically-named param (`missed-body-*` below), and
///   - the caller's walk over an argument it had already handed away was
///     silenced only by the callee's mark leaking back OUT under that same
///     shared name (`escaping-*`).
///
/// The second is why the isolation could not simply be added: 277621a landed it
/// alone and turned the escaping shape into a DOUBLE body, so it was reverted
/// and the leak left in place as load-bearing.
///
/// It was never as load-bearing as it looked. It held only while the caller
/// SPELLED its binding the same as the callee's param — rename the caller's
/// binding and the double body was already there, on all three backends'
/// oracle, before any of this landed. `escaping-caller-renamed` is that case,
/// and it is why the fix is not "restore the leak".
///
/// What replaces the leak is a rule the method path was missing outright:
/// `owned_param_frame_is_method` already declares that a method frame OWNS its
/// arguments (B-2026-08-29-10), so the CALLER must stop walking every by-value
/// arg it passes — not merely the passthrough subset `eval_call` marks for free
/// fns, which follow the opposite caller-retains convention. Codegen has had
/// the method-side twin since B-2026-08-09-15.
///
/// Every case is pinned to the three compiled backends, which agree with each
/// other throughout and were the oracle for all of it.
#[test]
fn test_method_frame_marks_do_not_leak_across_frames() {
    const DROPPER: &str = "struct R { id: i64 }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id}\") } }\n";
    for (label, body, want) in [
        // THE ROW'S REPRO. `add`'s `r` is LEGITIMATELY marked — it moves into
        // `self.xs` — and that mark used to outlive the frame and silence
        // `eat`'s unrelated `r`, which dies inside `eat` and needs its body.
        (
            "missed-body-shared-param-name",
            "struct Box3 { xs: Vec[R] }\n\
             impl Box3 { fn add(mut ref self, r: R) { self.xs.push(r); } }\n\
             struct G2 { n: i64 }\n\
             impl G2 { fn eat(ref self, r: R) -> i64 { 3 } }\n\
             fn main() { let mut bx = Box3 { xs: Vec.new() }; \
             bx.add(R { id: 11 }); println(\"added\"); \
             let g = G2 { n: 1 }; let v = g.eat(R { id: 12 }); println(f\"{v}\") }\n",
            "drop 11\nadded\ndrop 12\n3\n",
        ),
        // The same program with the SECOND param renamed. Renaming it was how
        // the name-keying was identified rather than inferred, and it passed
        // before the fix — so it is the control that the fix closed the gap
        // instead of moving it: both spellings must now print the same thing.
        (
            "missed-body-distinct-param-name",
            "struct Box3 { xs: Vec[R] }\n\
             impl Box3 { fn add(mut ref self, r: R) { self.xs.push(r); } }\n\
             struct G2 { n: i64 }\n\
             impl G2 { fn eat(ref self, q: R) -> i64 { 3 } }\n\
             fn main() { let mut bx = Box3 { xs: Vec.new() }; \
             bx.add(R { id: 11 }); println(\"added\"); \
             let g = G2 { n: 1 }; let v = g.eat(R { id: 12 }); println(f\"{v}\") }\n",
            "drop 11\nadded\ndrop 12\n3\n",
        ),
    ] {
        assert_eq!(run(&format!("{DROPPER}{body}")), want, "{label}");
    }

    // The ESCAPING half — a method that hands back a payload bound out of its
    // owned enum param. One value is constructed and one reaches the caller's
    // binding, so ONE body is correct.
    const RES: &str = "struct Res { id: i64, name: String }\n\
         impl Drop for Res { fn drop(mut ref self) { println(f\"drop {self.id} {self.name}\") } }\n";
    for (label, body, want) in [
        // The caller's binding SPELLED like the param — the one spelling the
        // old leak happened to cover, kept as a fixed point.
        (
            "escaping-caller-same-name",
            "enum Box2 { Full(Res), Empty }\n\
             struct T { n: i64 }\n\
             impl T { fn take(ref self, b: Box2) -> Res \
             { match b { Box2.Full(r) => { return r; } \
             Box2.Empty => { return Res { id: 0, name: f\"z\" }; } } } }\n\
             fn main() { let t = T { n: 1 }; \
             let b: Box2 = Box2.Full(Res { id: 7, name: f\"e7\" }); \
             let r: Res = t.take(b); println(f\"got {r.id}\") }\n",
            "got 7\ndrop 7 e7\n",
        ),
        // THE CASE THAT SHOWS THE LEAK WAS NOT PROTECTING THE SHAPE. Identical
        // but for the caller's binding name, and it ran the body TWICE against
        // once on every compiled backend, before and after 277621a alike.
        (
            "escaping-caller-renamed",
            "enum Box2 { Full(Res), Empty }\n\
             struct T { n: i64 }\n\
             impl T { fn take(ref self, b: Box2) -> Res \
             { match b { Box2.Full(r) => { return r; } \
             Box2.Empty => { return Res { id: 0, name: f\"z\" }; } } } }\n\
             fn main() { let t = T { n: 1 }; \
             let qq: Box2 = Box2.Full(Res { id: 7, name: f\"e7\" }); \
             let r: Res = t.take(qq); println(f\"got {r.id}\") }\n",
            "got 7\ndrop 7 e7\n",
        ),
        // The TAIL spelling of the same escape, renamed caller binding. The two
        // exits must not disagree — the split `return` / tail is exactly the
        // shape B-2026-08-29-21 had to close once already on this channel.
        (
            "escaping-tail-spelling",
            "enum Box2 { Full(Res), Empty }\n\
             struct T { n: i64 }\n\
             impl T { fn take(ref self, b: Box2) -> Res \
             { match b { Box2.Full(r) => { r } \
             Box2.Empty => { Res { id: 0, name: f\"z\" } } } } }\n\
             fn main() { let t = T { n: 1 }; \
             let qq: Box2 = Box2.Full(Res { id: 7, name: f\"e7\" }); \
             let r: Res = t.take(qq); println(f\"got {r.id}\") }\n",
            "got 7\ndrop 7 e7\n",
        ),
        // BOUNDARY, and the one the caller-side stand-down could most easily
        // break: the payload DIES inside the method (`r.id` is a field read, so
        // nothing escapes). The frame's arm stash owns the body and the caller
        // must stay silent — one body, not zero. This is B-2026-08-29-10's
        // shape, and disarming the caller for ALL by-value args rather than the
        // passthrough subset is what keeps it at one.
        (
            "payload-dies-inside-method",
            "enum Box2 { Full(Res), Empty }\n\
             struct T { n: i64 }\n\
             impl T { fn take(ref self, b: Box2) -> i64 \
             { match b { Box2.Full(r) => { return r.id; } Box2.Empty => { return 0; } } } }\n\
             fn main() { let t = T { n: 1 }; \
             let b: Box2 = Box2.Full(Res { id: 7, name: f\"e7\" }); \
             let v: i64 = t.take(b); println(f\"v={v}\") }\n",
            "drop 7 e7\nv=7\n",
        ),
        // FREE-FN ORACLES — correct before and after, on all four surfaces.
        // They follow the opposite convention (the CALLER retains and fires),
        // so a change that "fixed" methods by moving free functions onto the
        // method rule would fail here. Both spellings, because the free-fn path
        // has been name-independent all along and must stay so.
        (
            "free-fn-oracle-same-name",
            "enum Box2 { Full(Res), Empty }\n\
             fn takef(b: Box2) -> Res \
             { match b { Box2.Full(r) => { return r; } \
             Box2.Empty => { return Res { id: 0, name: f\"z\" }; } } }\n\
             fn main() { let b: Box2 = Box2.Full(Res { id: 7, name: f\"e7\" }); \
             let r: Res = takef(b); println(f\"got {r.id}\") }\n",
            "got 7\ndrop 7 e7\n",
        ),
        (
            "free-fn-oracle-renamed",
            "enum Box2 { Full(Res), Empty }\n\
             fn takef(b: Box2) -> Res \
             { match b { Box2.Full(r) => { return r; } \
             Box2.Empty => { return Res { id: 0, name: f\"z\" }; } } }\n\
             fn main() { let qq: Box2 = Box2.Full(Res { id: 7, name: f\"e7\" }); \
             let r: Res = takef(qq); println(f\"got {r.id}\") }\n",
            "got 7\ndrop 7 e7\n",
        ),
    ] {
        assert_eq!(run(&format!("{RES}{body}")), want, "{label}");
    }
}

/// B-2026-08-29-28 — a FRESH-TEMP scrutinee's own `impl Drop` body runs at
/// the CONSTRUCT's exit, not at the end of the enclosing block.
///
/// design.md § Temporary Lifetime Rules gives this its own table row:
/// "Match-expression scrutinee | Through every arm body (the scrutinee is
/// live across all arms; drops at match exit)". Codegen registered the body
/// on the enclosing SCOPE frame instead, so `match mk() { … }` followed by
/// three statements printed the body after all three. That is the direction
/// the same section's composition-with-NLL paragraph explicitly forbids —
/// "NLL never EXTENDS a temporary's live range past the position-specific
/// end — that direction would invalidate the lock-eagerness guarantee" —
/// so `match pool.acquire() { … }` held the lease to the end of the
/// function. The interpreter already implemented the table.
///
/// `nested-in-expression` and `two-in-one-statement` are the two rows that
/// distinguish MATCH-EXIT firing from mere statement-end firing, and they
/// are why the fix has a second half: an earlier cut that only admitted the
/// temp to the statement-end drain printed `dR2 c2 dE` and
/// `dR3 dR4 dE dE a7` — right block, wrong point inside it.
///
/// `place-binding-control` and `no-own-drop-enum-boundary` are the
/// unchanged boundaries: a NAMED scrutinee is owned elsewhere and keeps its
/// own placement, and an enum with no `Drop` of its own has no body to
/// place. Both printed identically before this fix.
///
/// The parity twin of `codegen::e2e_freshtemp_scrutinee_body_fires_at_construct_exit`.
/// This half was already GREEN before the fix and must stay so: it is the
/// oracle the compiled half was moved onto, so a future change that "reconciles"
/// the two by moving the interpreter has to break this first.
#[test]
fn freshtemp_scrutinee_body_fires_at_construct_exit() {
    const H: &str = "struct R { id: i64 }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
         enum E { A(R), B }\n\
         impl Drop for E { fn drop(mut ref self) { println(\"dE\") } }\n\
         enum H { A(R), B }\n\
         fn mk(n: i64) -> E { return E.A(R { id: n }) }\n\
         fn mkH(n: i64) -> H { return H.A(R { id: n }) }\n\
         fn sink(n: i64) -> i64 { return n }\n";
    for (label, body, want) in [
                (
                "statement-match",
                "match mk(1) { E.A(r) => { println(f\"v{r.id}\") } E.B => {} }\n",
                "v1\ndR1\ndE\npost\n",
                ),
                (
                "nested-in-expression",
                "println(f\"c{sink(match mk(2) { E.A(r) => { r.id } E.B => { 0 } })}\")\n",
                "dR2\ndE\nc2\npost\n",
                ),
                (
                "two-in-one-statement",
                "let a = match mk(3) { E.A(r) => { r.id } E.B => { 0 } }\n\
                 \x20       + match mk(4) { E.A(r) => { r.id } E.B => { 0 } };\n\
                 println(f\"a{a}\")\n",
                "dR3\ndE\ndR4\ndE\na7\npost\n",
                ),
                (
                "nested-match",
                "match mk(5) { E.A(r) => { match mk(6) { E.A(q) => { println(f\"i{q.id}\") } E.B => {} }\n\
                 \x20   println(f\"o{r.id}\") } E.B => {} }\n",
                "i6\ndR6\ndE\no5\ndR5\ndE\npost\n",
                ),
                (
                "if-let",
                "if let E.A(r) = mk(7) { println(f\"f{r.id}\") }\n",
                "f7\ndR7\ndE\npost\n",
                ),
                (
                "let-bound-value",
                "let x = match mk(8) { E.A(r) => { r.id } E.B => { 0 } };\n\
                 println(f\"x{x}\")\n",
                "dR8\ndE\nx8\npost\n",
                ),
                (
                "in-loop-body",
                "let mut i = 0;\n\
                 while i < 2 { match mk(9) { E.A(r) => { println(f\"w{r.id}\") } E.B => {} }\n\
                 \x20   println(\"it\"); i = i + 1; }\n",
                "w9\ndR9\ndE\nit\nw9\ndR9\ndE\nit\npost\n",
                ),
                (
                "no-own-drop-enum-boundary",
                "match mkH(11) { H.A(r) => { println(f\"v{r.id}\") } H.B => {} }\n",
                "v11\ndR11\npost\n",
                ),
                (
                "place-binding-control",
                "let e = mk(12);\n\
                 match e { E.A(r) => { println(f\"v{r.id}\") } E.B => {} }\n",
                "v12\ndE\ndR12\npost\n",
                ),
    ] {
        let src = format!("{H}fn main() {{ {body} println(\"post\") }}\n");
        assert_eq!(run(&src), want, "{label}");
    }
}

/// B-2026-08-01-27 — a `let`-move of a Vec binding no longer leaves the new
/// binding aliasing the moved-from source's storage: `let mut v = w;
/// v.push(2)` used to make `w.len()` read 2 under the interpreter
/// (`Value::Array` is Arc-shared) while both compiled backends read 1 (move
/// bit-copies the header, source frozen at pre-move state). The same alias
/// made a closure body's `let mut v = outer; v.push(x)` accumulate across
/// calls (3) where the compiled backends re-read the untouched env copy per
/// call (2). The Let arm now binds a deep clone for an identifier-RHS
/// Vec move. Codegen twin: `e2e_let_move_source_frozen` pins the same
/// programs under the compiled backends (their behavior is unchanged — the
/// interpreter moved to match them).
#[test]
fn test_let_move_source_frozen() {
    assert_eq!(
        run("fn main() {\n\
                 let mut w: Vec[i64] = Vec.new();\n\
                 w.push(1);\n\
                 let mut v = w;\n\
                 v.push(2);\n\
                 println(w.len());\n\
                 println(v.len());\n\
                 let outer: Vec[String] = Vec.new();\n\
                 let mut grab = |x: String| {\n\
                     let mut v2 = outer;\n\
                     v2.push(x);\n\
                     v2.len()\n\
                 };\n\
                 let a = grab(String.from(\"a\"));\n\
                 let b = grab(String.from(\"b\"));\n\
                 println(a + b);\n\
             }\n"),
        "1\n2\n2\n"
    );
}

/// B-2026-08-13-3 — interpreter twin of `tests/codegen.rs`'s
/// `test_e2e_nested_field_move_out_of_owned_param_values_survive`, same source
/// and expected string.
///
/// The interpreter never had the bug — it copies values and GCs memory, so a
/// nested field move-out is just a read. That is exactly what makes it the
/// oracle: the compiled leg's cap-zeroing has to reproduce these bytes, and
/// pinning them here means a future change to the suppression cannot redefine
/// "correct" to match itself.
#[test]
fn test_nested_field_move_out_of_owned_param_values() {
    assert_eq!(
        run("             struct Pair { word: String, n: i64 }\n\
             struct Deep { inner: Pair, tag: i64 }\n\
             struct Outer { mid: Deep, label: String }\n\
             fn ret(d: Deep) -> String { d.inner.word }\n\
             fn lit(d: Deep) -> Deep {\n\
                 Deep { inner: Pair { word: d.inner.word, n: d.inner.n + 1 }, tag: d.tag }\n\
             }\n\
             fn bound(d: Deep) -> String { let s = d.inner.word; s }\n\
             fn deeper(o: Outer) -> String { o.mid.inner.word }\n\
             fn main() {\n\
                 let k = 1;\n\
                 println(ret(Deep { inner: Pair { word: f\"a{k}\", n: 1 }, tag: 2 }));\n\
                 let grown = lit(Deep { inner: Pair { word: f\"b{k}\", n: 3 }, tag: 4 });\n\
                 println(grown.inner.word);\n\
                 println(grown.inner.n);\n\
                 println(bound(Deep { inner: Pair { word: f\"d{k}\", n: 7 }, tag: 8 }));\n\
                 let o = Outer {\n\
                     mid: Deep { inner: Pair { word: f\"e{k}\", n: 9 }, tag: 10 },\n\
                     label: f\"L{k}\",\n\
                 };\n\
                 println(deeper(o));\n\
             }"),
        "a1\nb1\n4\nd1\ne1\n"
    );
}

/// B-2026-08-02-21 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_struct_field_set_and_sortedmap_bodies` (parity pin: the interp's
/// field walk fires Set-element / SortedMap-value bodies via the
/// B-2026-08-02-18 table arm, sorted values in key order like the b170
/// sorted walker; the memory half is AOT-only and pinned by the asan
/// twin).
#[test]
fn test_nested_literal_move_source_disarm() {
    // B-2026-08-02-23 leg 1 — interpreter twin of `tests/codegen.rs`'s
    // `e2e_nested_literal_move_source_disarm`, same source and expected
    // string. Both backends double-fired pre-fix (their walks were
    // depth-1 in the same way), so parity diffing was blind to it.
    assert_eq!(
        run("struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) { println(f\"drop {self.id} {self.name}\") }\n\
             }\n\
             struct Inner { xs: Vec[Res] }\n\
             struct Outer { inner: Inner, tag: i64 }\n\
             fn main() {\n\
                 println(\"a\");\n\
                 {\n\
                     let mut v: Vec[Outer] = Vec.new();\n\
                     let mut xs: Vec[Res] = Vec.new();\n\
                     xs.push(Res { id: 1, name: f\"n{1}\" });\n\
                     v.push(Outer { inner: Inner { xs: xs }, tag: 5 });\n\
                     println(v.len());\n\
                 }\n\
                 println(\"end\");\n\
             }\n"),
        "a\n1\ndrop 1 n1\nend\n"
    );
}

/// B-2026-08-01-31 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_deep_chain_field_move_then_reassign`, same source and expected
/// string. The deep-chain move records the ROOT in
/// `moved_out_drop_field_bindings` (root-coarse, the depth-1
/// B-2026-07-29-39 rule), so the displaced fire at the reassign and the
/// root's walk at o's death both stay silent — x's own fire is the single
/// body, matching codegen's disarm.
#[test]
fn test_deep_chain_field_move_then_reassign() {
    assert_eq!(
        run("struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"drop {self.id} {self.name}\")\n\
                 }\n\
             }\n\
             struct H7 { r: Res }\n\
             struct O7 { h: H7 }\n\
             fn main() {\n\
                 println(\"a\");\n\
                 let mut o = O7 { h: H7 { r: Res { id: 9, name: f\"z{9}\" } } };\n\
                 let x = o.h.r;\n\
                 o.h.r = Res { id: 5, name: f\"y{5}\" };\n\
                 println(f\"x {x.name} new {o.h.r.name}\");\n\
                 println(\"end\");\n\
             }\n"),
        "a\nx z9 new y5\ndrop 9 z9\nend\n"
    );
}

/// B-2026-09-06-55 — the MULTI-FIELD spelling of
/// `test_deep_chain_field_move_then_reassign`, and its interpreter/codegen
/// agreement is the whole point of the row.
///
/// `H7` above has ONE field, so masking the moved leaf empties the hop's
/// walker and codegen fell back to deleting the root's action outright —
/// which is what made its displacement gate decline and the two backends
/// agree by accident. Give the hop a SIBLING (`q`) and a top-level sibling
/// (`k`) and the masked walker survives, so the accident stops: the compiled
/// backends ran the displaced value's body over the husk `x` already owns,
/// printing `drop 9 ` with an EMPTY name against this backend's silence.
/// Measured on `main` before the fix, all three compiled surfaces.
///
/// Both siblings' bodies are the recovery this row is named for (`drop 8 z8`
/// and `drop 7 z7`, one each). The re-filled slot's own body is still absent,
/// exactly as in the one-field pin above — a move-out mask is permanent for
/// the binding, which is that pin's documented behaviour and not this row's
/// subject.
#[test]
fn test_deep_chain_field_move_then_reassign_with_siblings() {
    assert_eq!(
        run("struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"drop {self.id} {self.name}\")\n\
                 }\n\
             }\n\
             struct H8 { r: Res, q: Res }\n\
             struct O8 { h: H8, k: Res }\n\
             fn main() {\n\
                 println(\"a\");\n\
                 let mut o = O8 { h: H8 { r: Res { id: 9, name: f\"z{9}\" }, q: Res { id: 8, name: f\"z{8}\" } }, k: Res { id: 7, name: f\"z{7}\" } };\n\
                 let x = o.h.r;\n\
                 o.h.r = Res { id: 5, name: f\"y{5}\" };\n\
                 println(f\"x {x.name} new {o.h.r.name}\");\n\
                 println(\"end\");\n\
             }\n"),
        "a\nx z9 new y5\ndrop 9 z9\ndrop 7 z7\ndrop 8 z8\nend\n"
    );
}

/// B-2026-09-07-63 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_depth1_field_move_then_reassign_rearms_new_value`, same three sources
/// and expected strings.
///
/// This backend was already right on all three cells and is the reference the
/// row names: its depth-1 gate reads `moved_out_struct_field_bodies` directly
/// rather than asking whether an action is still armed, so the reassign
/// displaced nothing and the newly stored value was still the base's to drop.
/// The pin exists to keep that reference fixed while codegen's gate is taught
/// the same question — a later change here would turn the fix into a
/// run-vs-build divergence pointing the other way.
#[test]
fn test_depth1_field_move_then_reassign_rearms_new_value() {
    assert_eq!(
        run("struct Rs { id: i64, name: String }\n\
             impl Drop for Rs {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"dS{self.id}\")\n\
                 }\n\
             }\n\
             fn mks(i: i64) -> Rs {\n\
                 return Rs { id: i, name: f\"h{i}\" };\n\
             }\n\
             struct Bs { mut one: Rs, mut two: Rs }\n\
             fn main() {\n\
                 let mut g = Bs { one: mks(1), two: mks(2) };\n\
                 let taken = g.one;\n\
                 g.one = mks(7);\n\
                 println(f\"t{taken.id}\");\n\
             }\n"),
        "dS2\ndS7\nt1\ndS1\n"
    );
    assert_eq!(
        run("struct Rs { id: i64, name: String }\n\
             impl Drop for Rs {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"dS{self.id}\")\n\
                 }\n\
             }\n\
             fn mks(i: i64) -> Rs {\n\
                 return Rs { id: i, name: f\"h{i}\" };\n\
             }\n\
             struct Bs { mut one: Rs, mut two: Rs }\n\
             fn main() {\n\
                 let f = false;\n\
                 let mut g = Bs { one: mks(1), two: mks(2) };\n\
                 let taken = g.one;\n\
                 if f { g.one = mks(7); }\n\
                 println(f\"t{taken.id}\");\n\
             }\n"),
        "dS2\nt1\ndS1\n"
    );
    assert_eq!(
        run("struct Rs { id: i64, name: String }\n\
             impl Drop for Rs {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"dS{self.id}\")\n\
                 }\n\
             }\n\
             fn mks(i: i64) -> Rs {\n\
                 return Rs { id: i, name: f\"h{i}\" };\n\
             }\n\
             struct Bs { mut one: Rs, mut two: Rs }\n\
             fn main() {\n\
                 let mut g = Bs { one: mks(1), two: mks(2) };\n\
                 let taken = g.one;\n\
                 println(\"m2\");\n\
                 g.two = mks(8);\n\
                 println(\"m3\");\n\
                 println(f\"t{taken.id}\");\n\
             }\n"),
        "m2\ndS2\ndS8\nm3\nt1\ndS1\n"
    );
}

/// B-2026-09-08-4 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_cond_field_move_walk_stays_in_the_owning_frame`, same two sources.
///
/// This backend was already right on both paths and is the reference the row
/// names: it has no per-frame cleanup list for a branch to drain early, so a
/// field move-out inside an `if` leaves the base's remaining bodies to fire at
/// the base's own live-range end on either path. The pin keeps that reference
/// fixed while codegen's frame placement is corrected.
///
/// Both cells assert the full correct string, and the codegen twin now asserts
/// the same two strings — the asymmetry that stood while only the frame half
/// was fixed is gone.
#[test]
fn test_cond_field_move_walk_stays_in_the_owning_frame() {
    assert_eq!(
        run("struct Rs { id: i64, name: String }\n\
             impl Drop for Rs {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"dS{self.id}\")\n\
                 }\n\
             }\n\
             fn mks(i: i64) -> Rs {\n\
                 return Rs { id: i, name: f\"h{i}\" };\n\
             }\n\
             struct Bs { mut one: Rs, mut two: Rs }\n\
             fn main() {\n\
                 let f = true;\n\
                 let mut g = Bs { one: mks(1), two: mks(2) };\n\
                 if f { let taken = g.one; println(f\"t{taken.id}\"); }\n\
                 println(\"m3\");\n\
             }\n"),
        "t1\ndS1\ndS2\nm3\n"
    );
    assert_eq!(
        run("struct Rs { id: i64, name: String }\n\
             impl Drop for Rs {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"dS{self.id}\")\n\
                 }\n\
             }\n\
             fn mks(i: i64) -> Rs {\n\
                 return Rs { id: i, name: f\"h{i}\" };\n\
             }\n\
             struct Bs { mut one: Rs, mut two: Rs }\n\
             fn main() {\n\
                 let f = false;\n\
                 let mut g = Bs { one: mks(1), two: mks(2) };\n\
                 if f { let taken = g.one; println(f\"t{taken.id}\"); }\n\
                 println(\"m3\");\n\
             }\n"),
        "dS2\ndS1\nm3\n"
    );
}

/// B-2026-09-08-14 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_cond_move_then_reassign_displaces_on_the_untaken_path`, same two
/// sources and expected strings. This backend was already right on both paths:
/// it asks whether the move actually happened rather than whether a
/// compile-time record says it might have, so the untaken path still displaces.
#[test]
fn test_cond_move_then_reassign_displaces_on_the_untaken_path() {
    assert_eq!(
        run("struct Rs { id: i64, name: String }\n\
             impl Drop for Rs {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"dS{self.id}\")\n\
                 }\n\
             }\n\
             fn mks(i: i64) -> Rs {\n\
                 return Rs { id: i, name: f\"h{i}\" };\n\
             }\n\
             struct Bs { mut one: Rs, mut two: Rs }\n\
             fn main() {\n\
                 let f = false;\n\
                 let mut g = Bs { one: mks(1), two: mks(2) };\n\
                 if f { let taken = g.one; println(f\"t{taken.id}\"); }\n\
                 g.one = mks(7);\n\
                 println(\"m3\");\n\
             }\n"),
        "dS1\ndS2\ndS7\nm3\n"
    );
    assert_eq!(
        run("struct Rs { id: i64, name: String }\n\
             impl Drop for Rs {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"dS{self.id}\")\n\
                 }\n\
             }\n\
             fn mks(i: i64) -> Rs {\n\
                 return Rs { id: i, name: f\"h{i}\" };\n\
             }\n\
             struct Bs { mut one: Rs, mut two: Rs }\n\
             fn main() {\n\
                 let f = true;\n\
                 let mut g = Bs { one: mks(1), two: mks(2) };\n\
                 if f { let taken = g.one; println(f\"t{taken.id}\"); }\n\
                 g.one = mks(7);\n\
                 println(\"m3\");\n\
             }\n"),
        "t1\ndS1\ndS2\ndS7\nm3\n"
    );
}

#[test]
fn test_tuple_elem_move_out_single_body_fire() {
    // B-2026-08-03-3 (bodies half) interp twin. `let x = t.N` moved one element
    // out, but the source tuple's element walk still ran EVERY element, so the
    // moved one's body printed a full duplicate. Codegen's twin re-emits the
    // walker with index N masked; here a per-`(binding, index)` record does the
    // same job. Element 1 is the control that must still fire at scope exit.
    assert_eq!(
        run("struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) { println(f\"drop {self.id} {self.name}\") }\n\
             }\n\
             fn main() {\n\
                 println(\"struct-elem:\");\n\
                 {\n\
                     let t = (Res { id: 1, name: f\"a{1}\" }, Res { id: 2, name: f\"bb{2}\" });\n\
                     let x = t.0;\n\
                     println(t.1.id);\n\
                 }\n\
                 println(\"option-elem:\");\n\
                 {\n\
                     let t = (Option.Some(Res { id: 3, name: f\"ccc{3}\" }), 30);\n\
                     let x = t.0;\n\
                     println(t.1);\n\
                 }\n\
                 println(\"end\");\n\
             }\n"),
        "struct-elem:\ndrop 1 a1\n2\ndrop 2 bb2\noption-elem:\ndrop 3 ccc3\n30\nend\n"
    );
}

#[test]
fn test_named_source_moved_into_a_tuple_element_is_disarmed() {
    // B-2026-08-04-16 — the ORACLE half. The interpreter has always given the
    // tuple element sole ownership of a moved-in value; codegen left the source
    // binding's cleanup armed as well, so both freed the same buffer and the
    // program aborted with `free(): double free detected` (a TRIPLE free for a
    // `Vec[String]` element). The struct-FIELD spelling was correct on both
    // backends, which is the asymmetry that located the missing arm.
    //
    // Keep in step with the codegen twin
    // `e2e_named_source_moved_into_a_tuple_element_is_disarmed`. The seed is
    // spelled as the literal 1 here rather than `env.args().len()`: the codegen
    // fixture needs an opaque seed to survive -O2 folding and 1 is what that
    // yields under its harness, while `env.args()` inside an in-process
    // interpreter test would report the TEST binary's argv. Every printed value
    // is byte-identical to the codegen twin's.
    assert_eq!(
        run("struct H { items: Vec[i64], n: i64 }\n\
             fn mkvec(k: i64) -> Vec[i64] { let mut v: Vec[i64] = Vec.new(); v.push(k); v.push(k + 1i64); return v; }\n\
             fn mkstr(k: i64) -> String { let mut s: String = String.new(); s.push_str(f\"payload-{k}\"); return s; }\n\
             fn digits(i: i64) -> String { let mut d: String = String.new(); d.push_str(f\"{i}\"); return d; }\n\
             fn mkvs(k: i64) -> Vec[String] { let mut v: Vec[String] = Vec.new(); v.push(mkstr(k)); v.push(mkstr(k + 1i64)); return v; }\n\
             fn main() {\n\
             let n: i64 = 1i64;\n\
             let mut t2: (Vec[i64], i64) = (mkvec(n), 3i64);\n\
             let f: Vec[i64] = mkvec(n + 10i64);\n\
             t2.0 = f;\n\
             println(f\"b:{t2.0.len()}:{t2.0[0i64]}\");\n\
             let mut t3: (i64, Vec[i64]) = (3i64, mkvec(n));\n\
             let g: Vec[i64] = mkvec(n + 20i64);\n\
             t3.1 = g;\n\
             println(f\"c:{t3.1[0i64]}\");\n\
             let mut t4: (String, i64) = (mkstr(n), 3i64);\n\
             let s: String = mkstr(n + 30i64);\n\
             t4.0 = s;\n\
             if t4.0.contains(digits(n + 30i64)) { println(f\"d:{t4.0.len()}\"); } else { println(\"d:BAD\"); }\n\
             let mut t5: (Vec[String], i64) = (mkvs(n), 3i64);\n\
             let w: Vec[String] = mkvs(n + 40i64);\n\
             t5.0 = w;\n\
             println(f\"e:{t5.0.len()}:{t5.0[0i64].len()}:{t5.0[1i64].len()}\");\n\
             let mut t6: (Vec[i64], i64) = (mkvec(n), 3i64);\n\
             t6.0 = mkvec(n + 50i64);\n\
             println(f\"f:{t6.0[0i64]}\");\n\
             let mut h: H = H { items: mkvec(n), n: 3i64 };\n\
             let hv: Vec[i64] = mkvec(n + 60i64);\n\
             h.items = hv;\n\
             println(f\"g:{h.items[0i64]}\");\n\
             println(\"end\");\n\
             }\n"),
        "b:2:11\nc:21\nd:10\ne:2:10:10\nf:51\ng:61\nend\n"
    );
}

#[test]
fn test_struct_field_move_out_single_body_fire() {
    // B-2026-08-03-8 (bodies half) interp twin. `let x = h.f` left the source
    // struct's field walk running over every field, so the moved one's body
    // printed a full duplicate. The binding-level walk now drops moved-out
    // fields from the value before walking it — the whole mask, since the walk
    // already skips a field the value does not carry. Codegen's twin re-emits
    // the walker with the same field index masked.
    assert_eq!(
        run("struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
             fn drop(mut ref self) { println(f\"drop {self.id} {self.name}\") }\n\
             }\n\
             struct Ho { o: Option[Res], t: i64 }\n\
             struct Hv { v: Vec[Res], t: i64 }\n\
             struct Hs { r: Res, t: i64 }\n\
             fn main() {\n\
             println(\"option-field:\");\n\
             { let h = Ho { o: Option.Some(Res { id: 1, name: f\"a{1}\" }), t: 10 }; let x = h.o; println(h.t); }\n\
             println(\"vec-field:\");\n\
             {\n\
             let mut vv: Vec[Res] = Vec.new();\n\
             vv.push(Res { id: 2, name: f\"bb{2}\" });\n\
             let h = Hv { v: vv, t: 20 };\n\
             let x = h.v;\n\
             println(h.t);\n\
             }\n\
             println(\"struct-field:\");\n\
             { let h = Hs { r: Res { id: 3, name: f\"ccc{3}\" }, t: 30 }; let x = h.r; println(h.t); }\n\
             println(\"sibling-survives:\");\n\
             {\n\
             let mut w: Vec[Res] = Vec.new();\n\
             w.push(Res { id: 4, name: f\"dddd{4}\" });\n\
             let h = Hv { v: w, t: 40 };\n\
             println(h.t);\n\
             }\n\
             println(\"end\");\n\
             }\n"),
        "option-field:\ndrop 1 a1\n10\nvec-field:\ndrop 2 bb2\n20\nstruct-field:\ndrop 3 ccc3\n30\nsibling-survives:\n40\ndrop 4 dddd4\nend\n"
    );
}

#[test]
fn test_second_field_move_of_one_binding_oracle() {
    // Oracle twin of `tests/codegen.rs`'s
    // `test_e2e_second_field_move_of_one_binding_still_defensively_copies`
    // (B-2026-08-16-7). The interpreter's value semantics were always right
    // here — only the compiled defensive-copy planner deduped per binding —
    // so this pins the values the build side must match.
    let out = run("\n\
         enum Cmd { Clear(Vec[String]) }\n\
         struct Doc { lines: Vec[String] }\n\
         struct Ed { doc: Doc }\n\
         fn apply(d: mut ref Doc, c: Cmd) -> Cmd {\n\
             match c {\n\
                 Clear(old) => {\n\
                     let mut snap: Vec[String] = Vec.new();\n\
                     for i in 0..d.lines.len() { snap.push(d.lines[i]); }\n\
                     d.lines.clear();\n\
                     for i in 0..old.len() { d.lines.push(old[i]); }\n\
                     Cmd.Clear(snap)\n\
                 }\n\
             }\n\
         }\n\
         fn render(d: ref Doc) -> String {\n\
             let mut s = String.new();\n\
             for i in 0..d.lines.len() { s.push_str(d.lines[i]); }\n\
             s\n\
         }\n\
         fn main() {\n\
             let mut l: Vec[String] = Vec.new();\n\
             let mut a = String.new(); a.push_str(\"ALPHA\");\n\
             l.push(a);\n\
             let mut e = Ed { doc: Doc { lines: l } };\n\
             let mut snapshot: Vec[String] = Vec.new();\n\
             let cur = e.doc;\n\
             for i in 0..cur.lines.len() { snapshot.push(cur.lines[i]); }\n\
             let _inv = apply(mut e.doc, Cmd.Clear(snapshot));\n\
             let after = e.doc;\n\
             println(f\"[{render(e.doc)}] lines={after.lines.len()}\")\n\
         }\n");
    assert_eq!(out, "[ALPHA] lines=1\n");
}

#[test]
fn test_method_owned_arg_reassigned_in_body_is_not_written_back() {
    // The gate is the DECLARED parameter mode, and this is the control that
    // makes that load-bearing: an OWNED parameter reassigned in the body is
    // the callee's own copy, and the caller's binding must not move. The
    // free-function spelling below is the in-program oracle — it prints
    // `9 5`, and the method spelling has to match. Dropping the mode gate to
    // a blanket "copy every parameter back" flips the method half to `9 9`,
    // measured.
    let out = run("struct H { acc: i64 }\n\
        impl H { fn owned(ref self, x: i64) -> i64 { x = 9; x } }\n\
        fn free_owned(x: i64) -> i64 { x = 9; x }\n\
        fn main() {\n\
            let h = H { acc: 0 };\n\
            let mut n = 5;\n\
            println(h.owned(n));\n\
            println(n);\n\
            let mut m = 5;\n\
            println(free_owned(m));\n\
            println(m);\n\
        }");
    assert_eq!(out, "9\n5\n9\n5\n");
}

/// B-2026-08-28-15's ORACLE. The interpreter has always treated `p.0` as a
/// move that hands the caller a live element, and it is what the compiled
/// backends were measured against: they aborted with `free(): double free
/// detected in tcache 2` on each of these while `--interp` printed the values
/// below. This test therefore PASSES against the unfixed compiler BY DESIGN —
/// its job is to pin the semantics the codegen twin
/// (`e2e_tuple_elem_moved_out_at_an_escaping_position_is_not_double_freed`) is
/// asserted against, so a future change cannot quietly move the oracle to meet
/// a broken backend.
#[test]
fn tuple_elem_moved_out_at_an_escaping_position_yields_a_live_value() {
    const DECL: &str = "struct R { id: i64, name: String }\n";
    let cases: &[(&str, &str, &str)] = &[
        (
            "tail",
            "fn take(p: (R, i64)) -> R { p.0 }\n\
             fn main() { let x = take((R { id: 41, name: f\"n{41}\" }, 1)); println(f\"{x.id} {x.name}\") }",
            "41 n41\n",
        ),
        (
            "explicit-return",
            "fn take(p: (R, i64)) -> R { return p.0; }\n\
             fn main() { let x = take((R { id: 41, name: f\"n{41}\" }, 1)); println(f\"{x.id} {x.name}\") }",
            "41 n41\n",
        ),
        (
            "struct-literal-field",
            "struct H { r: R }\n\
             fn take(p: (R, i64)) -> H { H { r: p.0 } }\n\
             fn main() { let x = take((R { id: 41, name: f\"n{41}\" }, 1)); println(f\"{x.r.id} {x.r.name}\") }",
            "41 n41\n",
        ),
        (
            "nested-field",
            "fn take(p: (R, i64)) -> String { p.0.name }\n\
             fn main() { let a = take((R { id: 41, name: f\"n{41}\" }, 1)); println(f\"{a}\") }",
            "n41\n",
        ),
        (
            "array-elem-literal",
            "struct H { r: R }\n\
             fn take(a: Array[R, 2]) -> H { H { r: a[0] } }\n\
             fn main() { let x = take([R { id: 41, name: f\"n{41}\" }, R { id: 9, name: f\"m{9}\" }]); println(f\"{x.r.id} {x.r.name}\") }",
            "41 n41\n",
        ),
        // The source stays readable after an element moves out — the property
        // that makes the compiled cap-zeroing a suppression of the SOURCE's
        // free rather than a retraction of the source itself.
        (
            "reuse-after-move",
            "fn main() { let p = (R { id: 41, name: f\"n{41}\" }, 7); let a = p.0; println(f\"{a.name}\"); println(f\"{p.1}\") }",
            "n41\n7\n",
        ),
    ];
    for (label, body, want) in cases {
        assert_eq!(run(&format!("{DECL}{body}\n")), *want, "[{label}]");
    }
}

/// B-2026-08-29-30 — the INTERPRETER twin of
/// `e2e_discarded_literal_statement_and_no_else_if`, landed in the same commit.
///
/// The LEAK half: the bare-statement dispatch had no LITERAL arm, so `R { .. };` ran
/// nothing while `let _ = R { .. };` ran one body. B-2026-08-01-8's
/// moved-place retraction is widened to the bare spelling with it, since
/// admitting the tuple without retracting doubles a body rather than supplying
/// one — which is what the two controls hold.
///
/// The no-`else` half: row 3 was a REGRESSION GUARD, not a wish. fd4e80f's statement-site `If`
/// arm gated on liveness alone, and a no-`else` `if` hands out no live binding,
/// so this backend fired a body both compiled surfaces cannot emit — the
/// divergence that commit's own message warns against, one arm over. It was
/// declined here until codegen could own the value from inside the arm; that
/// owner landed with B-2026-08-29-30's remaining half, so row 3 now pins the
/// BODY, fired by all three backends together. The liveness gate it kept is
/// what still declines a tail NAMING an enclosing local — the population whose
/// body belongs to its own binding.
#[test]
fn test_discarded_literal_statement_and_no_else_if() {
    let hdr = "struct R { id: i64 }\n\
               impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n";
    let rows: [(&str, &str, &str); 3] = [
        (
            "R { id: 7 };\nprintln(\"end\");",
            "dR7\nend",
            "FIXED: bare struct-literal statement",
        ),
        (
            "{ R { id: 7 } };\nprintln(\"end\");",
            "dR7\nend",
            "FIXED: block-wrapped struct literal, bare statement",
        ),
        (
            "let n = 1;\nif n == 1 { R { id: 7 } };\nprintln(\"end\");",
            "dR7\nend",
            "FIXED: a no-`else` `if`, owned from inside the arm",
        ),
    ];
    for (body, expected, label) in rows {
        let src = format!("{hdr}fn main() {{\n{body}\n}}\n");
        assert_eq!(run(&src).trim(), expected, "[{label}]");
    }
    for (body, label) in [
        (
            "let r = R { id: 1 };\n(r, 20);\nprintln(\"end\");",
            "control: bare tuple statement moving a place element",
        ),
        (
            "let r = R { id: 1 };\nlet _ = (r, 20);\nprintln(\"end\");",
            "control: wildcard-let tuple moving a place element",
        ),
    ] {
        let src = format!("{hdr}fn main() {{\n{body}\n}}\n");
        assert_eq!(run(&src).trim(), "dR1\nend", "[{label}]");
    }
}

/// B-2026-09-05-13 — the INTERPRETER twin of
/// `e2e_rebound_param_returned_in_ctor_runs_one_body`, same program and same
/// string, landed in the same commit so the two backends cannot be fixed to
/// different answers. On this backend the rebind site stops marking `m` a
/// param VIEW when the frame owns `r`'s body per path (`cond_store_param_names`)
/// and lets `m` take an ordinary slot instead, and the named-argument gate
/// gains `fn_always_returns_param` — the `named-top` cell is the pre-existing
/// interpreter-only double that exposed.
#[test]
fn test_rebound_param_returned_in_ctor_runs_one_body() {
    assert_eq!(
        run(r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"drop {self.id} {self.name}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}" }; }
fn uncond_rebind(r: R) -> Option[R] { let m = r; return Option.Some(m); }
fn rebind(r: R, keep: bool) -> Option[R] { let m = r; if keep { return Option.Some(m); } return Option.None; }
fn rebind_ok(r: R, keep: bool) -> Result[R, i64] { let m = r; if keep { return Result.Ok(m); } return Result.Err(0); }
fn bare_rebind(r: R) -> R { let m = r; return m; }
fn top(r: R) -> Option[R] { return Option.Some(r); }
fn chain(r: R, keep: bool) -> Option[R] { let m = r; let n = m; if keep { return Option.Some(n); } return Option.None; }
fn inbranch(r: R, keep: bool) -> Option[R] { if keep { let m = r; return Option.Some(m); } println("after"); return Option.None; }
fn tail(r: R, keep: bool) -> Option[R] { let m = r; if keep { Option.Some(m) } else { Option.None } }
fn shadow(r: R, keep: bool) -> Option[R] { let m = r; if keep { return Option.Some(m); } let m = mk(99); println(f"sh {m.id}"); return Option.None; }
struct K { n: i64 }
impl K {
    fn mrebind(ref self, r: R, keep: bool) -> Option[R] { let m = r; if keep { return Option.Some(m); } return Option.None; }
    fn muncond(ref self, r: R) -> Option[R] { let m = r; return Option.Some(m); }
}
fn main() {
    let k = K { n: 0 };
    println("u-rebind"); let _ = uncond_rebind(mk(2));
    println("rb-f");     let _ = rebind(mk(3), false);
    println("rb-t");     let _ = rebind(mk(4), true);
    println("ok-f");     let _ = rebind_ok(mk(5), false);
    println("ok-t");     let _ = rebind_ok(mk(6), true);
    println("bare");     let _ = bare_rebind(mk(7));
    println("named-u");  let a = mk(8); let _ = uncond_rebind(a);
    println("named-top"); let b = mk(9); let _ = top(b);
    println("named-rb-f"); let c = mk(10); let _ = rebind(c, false);
    println("named-rb-t"); let d = mk(11); let _ = rebind(d, true);
    println("chain-f");  let _ = chain(mk(12), false);
    println("chain-t");  let _ = chain(mk(13), true);
    println("inbr-f");   let _ = inbranch(mk(14), false);
    println("inbr-t");   let _ = inbranch(mk(15), true);
    println("tail-f");   let _ = tail(mk(16), false);
    println("tail-t");   let _ = tail(mk(17), true);
    println("shadow-f"); let _ = shadow(mk(18), false);
    println("m-rb-f");   let _ = k.mrebind(mk(20), false);
    println("m-rb-t");   let _ = k.mrebind(mk(21), true);
    println("m-u");      let _ = k.muncond(mk(22));
    println("done")
}"#),
        "u-rebind\ndrop 2 h2\nrb-f\ndrop 3 h3\nrb-t\ndrop 4 h4\nok-f\ndrop 5 h5\nok-t\ndrop 6 h6\nbare\ndrop 7 h7\nnamed-u\ndrop 8 h8\nnamed-top\ndrop 9 h9\nnamed-rb-f\ndrop 10 h10\nnamed-rb-t\ndrop 11 h11\nchain-f\ndrop 12 h12\nchain-t\ndrop 13 h13\ninbr-f\nafter\ndrop 14 h14\ninbr-t\ndrop 15 h15\ntail-f\ndrop 16 h16\ntail-t\ndrop 17 h17\nshadow-f\nsh 99\ndrop 99 h99\ndrop 18 h18\nm-rb-f\ndrop 20 h20\nm-rb-t\ndrop 21 h21\nm-u\ndrop 22 h22\ndone\n",
        "a rebound by-value param handed back through a ctor runs one body"
    );
}

/// B-2026-09-06-12 — the INTERPRETER twin of
/// `e2e_param_rebound_through_returning_callee_then_returned_runs_one_body`,
/// same program and the same expected string. The unconditional cells were
/// agreed-wrong on all four surfaces; the conditional ones were already right
/// here (the frame's per-path slot for `w` plus the hand-over disarm) and
/// wrong compiled, so the pin holds both backends to one answer.
#[test]
fn test_param_rebound_through_returning_callee_then_returned_runs_one_body() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct W { r: R, n: i64 }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] }; }
fn keeps(r: R) -> R { return r; }
fn keep(t: (R, i64)) -> (R, i64) { return t; }
fn keepg[T](x: T) -> T { return x; }
fn s_ret(r: R) -> R { let w: R = keeps(r); return w; }
fn s_ret_tail(r: R) -> R { let w: R = keeps(r); w }
fn s_ret_direct(r: R) -> R { return keeps(r); }
fn s_ret_rebind(r: R) -> R { let z: R = r; let w: R = keeps(z); return w; }
fn s_ret_twice(r: R) -> R { let w: R = keeps(r); let v: R = keeps(w); return v; }
fn s_ret_generic(r: R) -> R { let w: R = keepg(r); return w; }
fn s_ret_cond(r: R, k: bool) -> R { let w: R = keeps(r); if k { return w; } return mk(99); }
fn s_ret_wrap(r: R) -> W { let w: R = keeps(r); return W { r: w, n: 1 }; }
fn s_ret_opt(r: R) -> Option[R] { let w: R = keeps(r); return Option.Some(w); }
fn t_ret(t: (R, i64)) -> (R, i64) { let w: (R, i64) = keep(t); return w; }
fn s_ret_after(r: R) -> R { let w: R = keeps(r); println(f"mid {w.id}"); return w; }
fn s_ret_nested(r: R, k: bool) -> R { if k { let w: R = keeps(r); return w; } return mk(98); }
fn s_ret_chain(r: R) -> R { let w: R = keeps(r); let v: R = w; return v; }
fn s_ret_unused_path(r: R, k: bool) -> R { let w: R = keeps(r); if k { return w; } println("np"); return w; }
struct K { n: i64 }
impl K {
    fn m_ret(ref self, r: R) -> R { let w: R = keeps(r); return w; }
}
fn main() {
    let k = K { n: 0 };
    println("one"); let a = s_ret(mk(1)); println(f"got {a.id}");
    println("two"); let b = s_ret_tail(mk(2)); println(f"got {b.id}");
    println("three"); let c = s_ret_direct(mk(3)); println(f"got {c.id}");
    println("four"); let d = s_ret_rebind(mk(4)); println(f"got {d.id}");
    println("five"); let e = s_ret_twice(mk(5)); println(f"got {e.id}");
    println("six"); let f = s_ret_generic(mk(6)); println(f"got {f.id}");
    println("seven-t"); let g = s_ret_cond(mk(7), true); println(f"got {g.id}");
    println("eight-f"); let h = s_ret_cond(mk(8), false); println(f"got {h.id}");
    println("nine"); let i = s_ret_wrap(mk(9)); println(f"got {i.r.id}");
    println("ten"); let j = s_ret_opt(mk(10)); match j { Option.Some(x) => println(f"got {x.id}"), Option.None => println("none") }
    println("eleven"); let l = t_ret((mk(11), 1)); println(f"got {l.0.id}");
    println("twelve"); let m = s_ret_after(mk(12)); println(f"got {m.id}");
    println("thirteen"); let n = k.m_ret(mk(13)); println(f"got {n.id}");
    println("fourteen-named"); let src = mk(14); let o = s_ret(src); println(f"got {o.id}");
    println("fifteen-discard"); let _ = s_ret(mk(15)); println("disc");
    println("sixteen-stmt"); s_ret(mk(16)); println("stmt");
    println("seventeen-t"); let p = s_ret_nested(mk(17), true); println(f"got {p.id}");
    println("eighteen-f"); let q = s_ret_nested(mk(18), false); println(f"got {q.id}");
    println("nineteen"); let s2 = s_ret_chain(mk(19)); println(f"got {s2.id}");
    println("twenty-f"); let u = s_ret_unused_path(mk(20), false); println(f"got {u.id}");
    println("end");
}"#),
        "one\ngot 1\ndR1\ntwo\ngot 2\ndR2\nthree\ngot 3\ndR3\nfour\ngot 4\ndR4\nfive\ngot 5\ndR5\nsix\ngot 6\ndR6\nseven-t\ngot 7\ndR7\neight-f\ndR8\ngot 99\ndR99\nnine\ngot 9\ndR9\nten\ngot 10\ndR10\neleven\ngot 11\ndR11\ntwelve\nmid 12\ngot 12\ndR12\nthirteen\ngot 13\ndR13\nfourteen-named\ngot 14\ndR14\nfifteen-discard\ndR15\ndisc\nsixteen-stmt\ndR16\nstmt\nseventeen-t\ngot 17\ndR17\neighteen-f\ndR18\ngot 98\ndR98\nnineteen\ngot 19\ndR19\ntwenty-f\nnp\ngot 20\ndR20\nend\n",
        "a param rebound through an always-returning callee and returned has one owner"
    );
}

/// B-2026-08-29-31 — the INTERPRETER twin of
/// `e2e_wildcard_let_discard_owns_what_its_arm_hands_out`, landed in the same
/// commit with the SAME shapes in the same order, so the two backends cannot
/// be fixed to different answers.
///
/// The `let _ =` spelling of a discarded branch ran no `Drop` body on any
/// backend and leaked one allocation per evaluation, while the BARE-STATEMENT
/// spelling of the identical program was correct — which is what identified
/// the statement kind rather than the branch as the unit.
///
/// This backend needed two changes. A wildcard `let` no longer marks its RHS
/// as an ESCAPING position, so an arm tail naming an enclosing local is no
/// longer recorded moved-out and keeps its own scope-exit body. And the
/// discard gate stopped excluding a bare `Identifier` arm tail outright: it
/// now asks whether the name still RESOLVES, which separates a live enclosing
/// local (already owned — do not fire) from an arm's own pattern binding
/// (already out of scope — nothing else can).
///
/// The `mixed-branch` row is why `taken_branch_tail` exists. One arm hands out
/// a live local and the other mints; judging every arm and taking the
/// conservative answer is wrong in BOTH directions, so this backend records
/// which arm actually ran — the runtime bit compiled gets for free from
/// per-path basic blocks.
/// B-2026-08-29-32 — the interpreter twin of
/// `codegen::e2e_discarded_branch_of_body_less_heap_literals_keeps_one_owner`,
/// carrying the SAME eight shapes in the same order.
///
/// The row itself is codegen-only: the leak it fixes is a compiled-backend
/// memory registration, and this backend was already clean. The twin exists
/// for the other half of the contract — the shapes the fix touches must keep
/// AGREEING across backends, and a body count is the only cross-backend
/// observable a body-less struct's memory bug has. If the compiled fix ever
/// starts double-owning, this file is where the divergence shows up as a
/// doubled `dD`.
/// B-2026-08-29-36 — interpreter twin of
/// `codegen::e2e_deep_projection_scrutinee_runs_one_payload_body`, carrying the
/// SAME 14 shapes in the same order.
///
/// A projection scrutinee DEEPER than one hop
/// (`match w.s.e { .. }`) whose arm materializes the payload.
///
/// B-2026-08-29-33 taught the one-hop form to stop the owner re-running a
/// payload body the arm already ran, and keyed its mask on a single field
/// index. A deeper chain resolved to nothing, so the owner's walk stayed
/// unmasked and fired a SECOND time — on the interpreter against the live
/// object (`dR8`), on both compiled backends against the slot the move-out
/// cap-zeroed (`dR0`). One defect, two symptoms: an extra body everywhere
/// and a run-vs-build divergence in what that body printed.
///
/// The mask is now a PATH. `FieldSkipTree::nested` has been consumed by
/// the emitter since B-2026-08-28-23; what was missing was anything that
/// built a non-empty one.
///
/// Every row here must read exactly as its one-hop analogue does.
/// B-2026-08-29-38 — interpreter twin of
/// `codegen::e2e_method_fresh_temp_arg_handed_back_runs_one_body`, same shapes
/// in the same order. This backend was already CORRECT on the row's first
/// spelling; the twin pins that it stays so while codegen converges onto it,
/// and carries the same two known defects so a one-sided fix fails loudly.
///
/// A METHOD's FRESH-TEMP argument whose value the callee
/// hands back out.
///
/// The passthrough guard that exists for exactly this computes the right
/// answer and then acts only `if let ExprKind::Identifier(var_name)`, which
/// a temp never is — so the caller-side temp drop fired AND the returned
/// value's own binding fired, two bodies for one object, against one in the
/// interpreter. The fix feeds the same predicate into `escapes_frame`,
/// which the registrar reads off the VALUE rather than off a name.
///
/// MEMORY: the row left open whether the extra body came with a second
/// free. It does not — measured under valgrind with a `String`-carrying
/// payload, 0 errors and no leak before or after. Bodies only.
#[test]
fn test_method_fresh_temp_arg_handed_back_runs_one_body() {
    let hdr = "struct R { id: i64, name: String }\n\
               impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id} {self.name}\") } }\n\
               fn mk(i: i64) -> R { return R { id: i, name: f\"h{i}\" }; }\n\
               enum Box2 { Full(R), Empty }\n\
               struct T { n: i64 }\n\
               impl T {\n\
               \x20   fn take(ref self, b: Box2) -> R {\n\
               \x20       match b { Box2.Full(r) => { return r; } Box2.Empty => { return mk(0); } }\n\
               \x20   }\n\
               \x20   fn keep(ref self, b: Box2) -> i64 {\n\
               \x20       match b { Box2.Full(r) => { return r.id; } Box2.Empty => { return 0; } }\n\
               \x20   }\n\
               }\n\
               struct K { n: i64 }\n\
               impl K { fn g(ref self, r: R) -> i64 { return r.id; } }\n";
    for (label, body, want) in [
        (
            "the row: enum temp whose payload is handed back",
            "let t = T { n: 1 };\n\
             let r = t.take(Box2.Full(mk(7)));\n\
             println(f\"got {r.id}\");",
            "got 7\ndrop 7 h7\n",
        ),
        (
            "same, result read twice so its own body is unmistakably the survivor",
            "let t = T { n: 1 };\n\
             let r = t.take(Box2.Full(mk(7)));\n\
             println(f\"got {r.id}\");\n\
             println(f\"again {r.id}\");",
            "got 7\nagain 7\ndrop 7 h7\n",
        ),
        // The NAMED spelling was always correct — it is what the guard
        // reaches — and is the in-tree proof that this row is about the
        // argument's SYNTACTIC FORM, not about passthrough analysis.
        (
            "control: the same call with a NAMED binding argument",
            "let t = T { n: 1 };\n\
             let b = Box2.Full(mk(7));\n\
             let r = t.take(b);\n\
             println(f\"got {r.id}\");",
            "got 7\ndrop 7 h7\n",
        ),
        // Controls the widened `escapes_frame` must keep DECLINING: the
        // callee consumes the value instead of handing it back, so the
        // caller-side temp drop is the only owner and must still fire.
        (
            "control: method consumes a struct temp and returns a scalar",
            "let k = K { n: 1 };\n\
             let n = k.g(mk(7));\n\
             println(f\"n{n}\");",
            "drop 7 h7\nn7\n",
        ),
    ] {
        let src = format!("{hdr}fn main() {{\n{body}\n}}\n");
        assert_eq!(run(&src), want, "[{label}]");
    }
    // PINNED AT KNOWN DEFECTS, not asserted as correct.
    //
    // (a) The METHOD leg's OTHER spelling — a struct temp escaping inside a
    // returned `Option.Some(r)` rather than as the bare param — still runs
    // two bodies compiled against one interpreted. It is NOT reachable by
    // widening a predicate: these are documented conservative-true, and the
    // non-escaping calls in this very program (`false`, `false`) correctly
    // print their body today. A conservative-true answer would skip the
    // caller-side drop on ALL THREE calls, trading one doubled body for two
    // LOST ones and a leak. It needs the per-path callee-side ownership
    // flip that `fn_conditionally_returns_param_bare` has for the bare
    // form. Filed separately.
    //
    // (b) WAS a pinned divergence — `t.keep(..)`, whose arm binds the payload
    // but returns a SCALAR, ran no body at all here against one compiled. Fixed
    // by B-2026-08-31-47 and now asserted as CORRECT below, agreeing with both
    // compiled backends. The disarm it tripped over is a hand-off, and a method
    // frame reached with a fresh temp has nobody to hand to.
    let hdr2 = "struct R { id: i64, name: String }\n\
                impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id} {self.name}\") } }\n\
                fn mk(i: i64) -> R { return R { id: i, name: f\"h{i}\" }; }\n\
                enum Box2 { Full(R), Empty }\n\
                struct T { n: i64 }\n\
                impl T { fn keep(ref self, b: Box2) -> i64 {\n\
                \x20   match b { Box2.Full(r) => { return r.id; } Box2.Empty => { return 0; } } } }\n\
                struct K { n: i64 }\n\
                impl K { fn f(ref self, r: R, keep: bool) -> Option[R] {\n\
                \x20   if keep { return Option.Some(r); }\n\
                \x20   return Option.None; } }\n";
    assert_eq!(
        run(&format!(
            "{hdr2}fn main() {{\n\
             let k = K {{ n: 1 }};\n\
             let _ = k.f(mk(3), false); println(\"a\");\n\
             let _ = k.f(mk(4), true);  println(\"b\");\n\
             let _ = k.f(mk(5), false); println(\"c\");\n}}\n"
        )),
        // One body per call. This backend was always right here; codegen
        // doubled the escaping call until B-2026-08-31-46, and the twin now
        // pins the SAME string on both sides.
        "drop 3 h3\na\ndrop 4 h4\nb\ndrop 5 h5\nc\n",
        "[(a) temp escaping inside a returned Option ctor — agreed since B-2026-08-31-46]"
    );
    assert_eq!(
        run(&format!(
            "{hdr2}fn main() {{\n\
             let t = T {{ n: 1 }};\n\
             let n = t.keep(Box2.Full(mk(7)));\n\
             println(f\"n{{n}}\");\n}}\n"
        )),
        // B-2026-08-31-47 — now correct, and identical to both compiled
        // backends. The arm binds the payload out of an owned param and
        // returns a SCALAR, so nothing escapes and this frame is the only
        // owner; the payload walk therefore stays armed.
        "drop 7 h7\nn7\n",
        "[B-2026-08-31-47: enum temp whose arm binds a payload it does not return]"
    );
}

#[test]
fn test_param_view_field_moved_back_out_runs_one_body() {
    let hdr = "struct R { id: i64, name: String }\n\
               impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
               fn mk(i: i64) -> R { return R { id: i, name: f\"heap-{i}\" }; }\n\
               struct S1 { r: R }\n\
               struct S3 { a: R, b: R }\n\
               struct D1 { r: R }\n\
               impl Drop for D1 { fn drop(mut ref self) { println(\"dD\") } }\n\
               struct W1 { s: S1 }\n\
               struct O { r: R }\n\
               struct O3 { o: O }\n\
               struct H { n: i64 }\n";
    for (label, fns, main, want) in [
        // THE ROW. `let s = S1 { r: r }` wraps a param view; `let x = s.r`
        // reads it straight back out. The caller runs that body, so `x`
        // must not register one — this printed `dR1 dR1` on all four
        // surfaces, agreed, which is why no A/B parity gate could see it.
        (
            "all-views wrap, then move the view back out",
            "fn take(r: R) -> i64 { let s = S1 { r: r }; let x = s.r; return 7; }",
            "let v = take(mk(1)); println(f\"v={v}\");",
            "dR1\nv=7\n",
        ),
        // NO WRAP AT ALL. The same read straight off a by-value param is
        // the same caller-owned move, and doubled the same way — which is
        // what shows the subject is view-ness, not the wrap.
        (
            "raw owned param, direct field read",
            "fn take(o: O) -> i64 { let x = o.r; return 7; }",
            "let v = take(O { r: mk(1) }); println(f\"v={v}\");",
            "dR1\nv=7\n",
        ),
        (
            "raw owned param, two-hop field read",
            "fn take(w: O3) -> i64 { let x = w.o.r; return 7; }",
            "let v = take(O3 { o: O { r: mk(1) } }); println(f\"v={v}\");",
            "dR1\nv=7\n",
        ),
        // DEPTH. One test at the chain ROOT settles any depth, because
        // everything reachable through a view is a view.
        (
            "two-hop chain through two wraps",
            "fn take(r: R) -> i64 { let s = S1 { r: r }; let w = W1 { s: s }; let x = w.s.r; return 7; }",
            "let v = take(mk(1)); println(f\"v={v}\");",
            "dR1\nv=7\n",
        ),
        // The move-out destination inherits view-ness, so a later whole-
        // value rebind of it does not re-arm what this withheld.
        (
            "move out, then rebind the destination",
            "fn take(r: R) -> i64 { let s = S1 { r: r }; let x = s.r; let y = x; return 7; }",
            "let v = take(mk(1)); println(f\"v={v}\");",
            "dR1\nv=7\n",
        ),
        // READING `x` does not change who owns it.
        (
            "move out, then read the destination",
            "fn take(r: R) -> i64 { let s = S1 { r: r }; let x = s.r; return x.id; }",
            "let v = take(mk(1)); println(f\"v={v}\");",
            "dR1\nv=1\n",
        ),
        // BOTH fields views: the binding is a view whole, and moving one
        // out leaves both bodies to the caller.
        (
            "both fields views, move one out",
            "fn take(p: R, q: R) -> i64 { let s = S3 { a: p, b: q }; let x = s.a; return 7; }",
            "let v = take(mk(1), mk(2)); println(f\"v={v}\");",
            "dR2\ndR1\nv=7\n",
        ),
        // OWN-`Drop` WRAPPER. This wrap never becomes a view — `D1`'s body
        // is the binding's own — so the per-field record is the only thing
        // that can answer for `d.r`. `dD` still fires; only the doubled
        // `dR1` goes.
        (
            "own-Drop wrapper, move the view out",
            "fn take(r: R) -> i64 { let d = D1 { r: r }; let x = d.r; return 7; }",
            "let v = take(mk(1)); println(f\"v={v}\");",
            "dD\ndR1\nv=7\n",
        ),
        // METHOD FRAME. Codegen doubled here where the interpreter did not
        // (its caller-side fire is wired into `eval_call` alone), so this
        // shape was a run-vs-build DIVERGENCE before the fix — the one
        // place in the family where the two backends disagreed. Fixing
        // codegen converges it; the interpreter's method-frame bail is what
        // keeps its single body from going to zero.
        (
            "method frame, all-views wrap",
            "impl H { fn take(ref self, r: R) -> i64 { let s = S1 { r: r }; let x = s.r; return 7; } }",
            "let h = H { n: 0 }; let v = h.take(mk(1)); println(f\"v={v}\");",
            "dR1\nv=7\n",
        ),
        // A REBIND on the way in is still a view.
        (
            "param rebound, then wrapped, then moved out",
            "fn take(r: R) -> i64 { let m = r; let s = S1 { r: m }; let x = s.r; return 7; }",
            "let v = take(mk(1)); println(f\"v={v}\");",
            "dR1\nv=7\n",
        ),
        // CONTROLS. Each is the same statement over a LOCAL source, and
        // each was already correct — which is what isolates view-ness as
        // the missing half rather than the move-out machinery, and what
        // this fix must not disturb.
        (
            "control: local source, move out",
            "fn take() -> i64 { let s = S1 { r: mk(1) }; let x = s.r; return 7; }",
            "let v = take(); println(f\"v={v}\");",
            "dR1\nv=7\n",
        ),
        (
            "control: all-views wrap, NO move out",
            "fn take(r: R) -> i64 { let s = S1 { r: r }; return 7; }",
            "let v = take(mk(1)); println(f\"v={v}\");",
            "dR1\nv=7\n",
        ),
        (
            "control: own-Drop wrapper, local source",
            "fn take() -> i64 { let d = D1 { r: mk(1) }; let x = d.r; return 7; }",
            "let v = take(); println(f\"v={v}\");",
            "dR1\ndD\nv=7\n",
        ),
        (
            "control: method frame, local source",
            "impl H { fn take(ref self) -> i64 { let s = S1 { r: mk(1) }; let x = s.r; return 7; } }",
            "let h = H { n: 0 }; let v = h.take(); println(f\"v={v}\");",
            "dR1\nv=7\n",
        ),
        // CONTROL, and the one that keeps the fix from being a blunt
        // instrument: moving the FRESH field out of a mixed wrap must
        // still register a body, because nobody else runs it.
        (
            "control: mixed wrap, move the FRESH field out",
            "fn take(r: R) -> i64 { let s = S3 { a: r, b: mk(2) }; let x = s.b; return 7; }",
            "let v = take(mk(1)); println(f\"v={v}\");",
            "dR2\ndR1\nv=7\n",
        ),
    ] {
        let src = format!("{hdr}{fns}\nfn main() {{ {main} }}\n");
        assert_eq!(run(&src), want, "[{label}]");
    }
    // FIXED by B-2026-09-01-3, which supplied the tuple record this comment
    // asked for. `param_view_tuple_elems` is written where the literal's view
    // elements are already computed, and read from a `TupleIndex` arm of the
    // same chain walk that answers for a field -- so `t.0` and `s.r` reach one
    // decision by one route, each through its own store. A third store rather
    // than a wider predicate, because a tuple literal reaches NEITHER answer
    // the struct path has: it never becomes a `param_view_locals` mark (its
    // mask is per SLOT, so the all-views propagation does not fire) and it has
    // no field NAME to key the field record by.
    //
    // The cell stays as the guard that the two hops do not overlap -- a struct
    // field must not be answered by the tuple record, nor an element by the
    // field one.
    let tuple_moveout = format!(
        "{hdr}fn take(r: R) -> i64 {{ let t = (r, 5); let x = t.0; return 7; }}\n\
         fn main() {{ let v = take(mk(1)); println(f\"v={{v}}\"); }}\n"
    );
    assert_eq!(
        run(&tuple_moveout),
        "dR1\nv=7\n",
        "[tuple wrap then element move-out]"
    );
    // FIXED by B-2026-09-03-8, and the repair was neither thing this comment
    // predicted. It is not a wider record and not the all-views propagation: the
    // per-slot record was already right, it simply did not TRAVEL. A whole-value
    // rebind runs `transfer_move_masks_on_rebind`, which carried every MASK to the
    // destination and neither param-view RECORD -- so `t2`'s walk stayed correctly
    // masked and then handed the element to a binding that minted a second owner. Two
    // lookups added beside the mask transfers, on both backends, and the record
    // arrives with the mask it explains.
    //
    // WHICH ALSO FIXED A SPELLING THE ROW DID NOT MENTION. A MIXED STRUCT literal
    // rebound and then projected (`let s = S3 { a: r, b: mk(2) }; let s2 = s;
    // let x = s2.a;`) doubled identically on all four surfaces. The row scoped itself
    // to tuples because the ALL-VIEWS struct case is correct -- it rides
    // `param_view_locals`, the propagation this comment pointed at -- and a mixed
    // struct literal is not a view whole, so it depends on the same per-field record
    // and lost it at the same rebind. Chasing the proposed propagation would have
    // fixed the tuple and left this one standing.
    //
    // The cells below keep both hops honest: a struct field must not be answered by
    // the tuple record nor an element by the field one, and the FRESH half of a mixed
    // literal must keep the body nobody else runs.
    let rebind_cells: [(&str, &str, &str, &str); 5] = [
        (
            "tuple ALL-VIEWS rebound, then element move-out",
            "fn take(r: R) -> i64 { let t = (r, 5); let t2 = t; let x = t2.0; return 7; }",
            "take(mk(1))",
            "dR1\nv=7\n",
        ),
        (
            "tuple MIXED rebound, then the VIEW element out",
            "fn take(r: R) -> i64 { let t = (r, mk(2)); let t2 = t; let x = t2.0; return 7; }",
            "take(mk(1))",
            "dR2\ndR1\nv=7\n",
        ),
        (
            "control: tuple MIXED rebound, then the FRESH element out",
            "fn take(r: R) -> i64 { let t = (r, mk(2)); let t2 = t; let x = t2.1; return 7; }",
            "take(mk(1))",
            "dR2\ndR1\nv=7\n",
        ),
        (
            "struct MIXED rebound, then the VIEW field out",
            "fn take(r: R) -> i64 { let s = S3 { a: r, b: mk(2) }; let s2 = s; let x = s2.a; return 7; }",
            "take(mk(1))",
            "dR2\ndR1\nv=7\n",
        ),
        (
            "control: local source, rebound, then out",
            "fn take() -> i64 { let t = (mk(1), 5); let t2 = t; let x = t2.0; return 7; }",
            "take()",
            "dR1\nv=7\n",
        ),
    ];
    for (label, fns, call, want) in rebind_cells {
        let src = format!("{hdr}{fns}\nfn main() {{ let v = {call}; println(f\"v={{v}}\"); }}\n");
        assert_eq!(run(&src), want, "[{label}]");
    }
}

/// B-2026-09-01-3 -- the TUPLE spelling of the move B-2026-08-29-47 fixed for
/// a struct FIELD and pinned here at the defect.
///
/// `let t = (r, 5); let x = t.0;` moves a param VIEW into a tuple element and
/// reads it straight back out. The caller runs that body, so `x` must not
/// register one; it printed `dR1 dR1` where one was due.
///
/// THE REPAIR IS THE TUPLE PEER OF -47's RECORD, NOT A WIDENING OF IT. A tuple
/// literal reaches neither answer the struct path has: it never becomes a
/// `param_view_locals` mark, because its mask is per SLOT and the all-views
/// propagation does not fire, and it has no field NAME for the field record to
/// be keyed by. So `param_view_tuple_elems` is a third store, written where
/// the literal's view elements are already computed and read from a
/// `TupleIndex` arm of the same chain walk that answers for a field.
///
/// MEASURED before and after, every cell on all four surfaces:
///
/// | cell                                  | before          | after     |
/// |---------------------------------------|-----------------|-----------|
/// | the row                               | `dR1 dR1`       | `dR1`     |
/// | mixed, move the VIEW out              | `dR1 dR2 dR1`   | `dR2 dR1` |
/// | both views, move ONE out              | `dR1 dR2 dR1`   | `dR2 dR1` |
/// | both views, move BOTH out             | `dR1 dR2 dR2 dR1` | `dR2 dR1` |
/// | move out, then rebind the destination | `dR1 dR1`       | `dR1`     |
/// | move out, then read the destination   | `dR1 dR1`       | `dR1`     |
/// | `o.t.0` -- root is the by-value param | `dR1 dR1`       | `dR1`     |
/// | method frame                          | SPLIT (below)   | `dR1`     |
///
/// THE METHOD FRAME WAS NOT AN AGREED GAP. Measured pre-fix, the interpreter
/// printed ONE body there and all three compiled surfaces printed two, so that
/// spelling was a run-vs-build DIVERGENCE while the headline shape was an
/// agreed one. Same split -47 found at its own method-frame cell, and the same
/// resolution: fixing codegen converges it, and the interpreter's
/// method-frame bail is what keeps its single body from going to zero.
///
/// The four CONTROLS are unchanged in both columns, which is what shows the
/// fix is not a blunt instrument -- moving the FRESH element out of a mixed
/// literal still registers a body, because nobody else runs it; a LOCAL source
/// was already correct and must stay so; and the STRUCT rebind is correct by a
/// route the tuple has no peer for, which is the shape the sentinel in
/// `test_param_view_field_moved_back_out_runs_one_body` still pins.
#[test]
fn test_param_view_tuple_elem_moved_back_out_runs_one_body() {
    let hdr = "struct R { id: i64, name: String }\n\
               impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
               fn mk(i: i64) -> R { return R { id: i, name: f\"heap-{i}\" }; }\n\
               struct S1 { r: R }\n\
               struct Ot { t: (R, i64) }\n\
               struct H { n: i64 }\n";
    for (label, fns, main, want) in [
        // THE ROW.
        (
            "tuple wrap, then move the element back out",
            "fn take(r: R) -> i64 { let t = (r, 5); let x = t.0; return 7; }",
            "let v = take(mk(1)); println(f\"v={v}\");",
            "dR1\nv=7\n",
        ),
        // MIXED literal: the view's element is masked, the fresh one is not,
        // so only the view's body moves to the caller.
        (
            "mixed literal, move the VIEW element out",
            "fn take(r: R) -> i64 { let t = (r, mk(2)); let x = t.0; return 7; }",
            "let v = take(mk(1)); println(f\"v={v}\");",
            "dR2\ndR1\nv=7\n",
        ),
        // BOTH elements views: the record is per index, so one entry per
        // element and moving either out leaves both bodies to the caller.
        (
            "both elements views, move ONE out",
            "fn take(p: R, q: R) -> i64 { let t = (p, q); let x = t.0; return 7; }",
            "let v = take(mk(1), mk(2)); println(f\"v={v}\");",
            "dR2\ndR1\nv=7\n",
        ),
        (
            "both elements views, move BOTH out",
            "fn take(p: R, q: R) -> i64 { let t = (p, q); let x = t.0; let y = t.1; return 7; }",
            "let v = take(mk(1), mk(2)); println(f\"v={v}\");",
            "dR2\ndR1\nv=7\n",
        ),
        // The destination inherits view-ness, so a later whole-value rebind
        // of it does not re-arm what this withheld.
        (
            "move out, then rebind the destination",
            "fn take(r: R) -> i64 { let t = (r, 5); let x = t.0; let y = x; return 7; }",
            "let v = take(mk(1)); println(f\"v={v}\");",
            "dR1\nv=7\n",
        ),
        // READING the destination does not change who owns it.
        (
            "move out, then read the destination",
            "fn take(r: R) -> i64 { let t = (r, 5); let x = t.0; return x.id; }",
            "let v = take(mk(1)); println(f\"v={v}\");",
            "dR1\nv=1\n",
        ),
        // MIXED HOPS. The chain walk is one loop over both hop kinds, so a
        // tuple index under a struct field settles at the ROOT test and needs
        // no record at all -- the half that was unreachable before only
        // because the walk refused to start on a `TupleIndex`.
        (
            "root is a by-value param: struct field then tuple index",
            "fn take(o: Ot) -> i64 { let x = o.t.0; return 7; }",
            "let v = take(Ot { t: (mk(1), 5) }); println(f\"v={v}\");",
            "dR1\nv=7\n",
        ),
        // METHOD FRAME -- the cell that was a run-vs-build SPLIT before this
        // fix, not an agreed gap. See the doc comment.
        (
            "method frame, tuple wrap then move out",
            "impl H { fn take(ref self, r: R) -> i64 { let t = (r, 5); let x = t.0; return 7; } }",
            "let h = H { n: 0 }; let v = h.take(mk(1)); println(f\"v={v}\");",
            "dR1\nv=7\n",
        ),
        // CONTROLS. Each was already correct and must stay so; together they
        // are what keeps the record from becoming a blanket suppression.
        (
            "control: mixed literal, move the FRESH element out",
            "fn take(r: R) -> i64 { let t = (mk(2), r); let x = t.0; return 7; }",
            "let v = take(mk(1)); println(f\"v={v}\");",
            "dR2\ndR1\nv=7\n",
        ),
        (
            "control: local source, move out",
            "fn take() -> i64 { let t = (mk(1), 5); let x = t.0; return 7; }",
            "let v = take(); println(f\"v={v}\");",
            "dR1\nv=7\n",
        ),
        (
            "control: tuple wrap, NO move out",
            "fn take(r: R) -> i64 { let t = (r, 5); return 7; }",
            "let v = take(mk(1)); println(f\"v={v}\");",
            "dR1\nv=7\n",
        ),
        // CONTROL, and the contrast that explains the sentinel: the STRUCT
        // rebind is correct, by the all-views propagation the tuple lacks.
        (
            "control: the STRUCT rebind spelling, which is correct",
            "fn take(r: R) -> i64 { let s = S1 { r: r }; let s2 = s; let x = s2.r; return 7; }",
            "let v = take(mk(1)); println(f\"v={v}\");",
            "dR1\nv=7\n",
        ),
    ] {
        let src = format!("{hdr}{fns}\nfn main() {{ {main} }}\n");
        assert_eq!(run(&src), want, "[{label}]");
    }
}

/// B-2026-09-06-47 — the let-destructure discard collector
/// (`run_wildcard_destructure_leaf_user_drops`) re-ran the `Drop` body of a
/// field already MOVED OUT of the source (`let x = s.a; let S3 { b, .. } =
/// s;`): the value the pattern consumes still carries a copy of `a`, so
/// its body ran at the destructure AND at `x`'s death (`dR2 mid dR3 dR2`).
/// A field in `moved_out_struct_field_bodies` is now skipped exactly as a
/// param-view field is. Interpreter-only pin: the compiled backends lose
/// `b`'s body on this exact shape (B-2026-09-06-46, open), so there is no
/// agreed four-surface string yet.
///
/// `one` the rest spelling, `two` the explicit `a: _` spelling, `three`
/// the rest spelling with the leaf unread.
#[test]
fn test_let_destructure_discard_skips_a_moved_out_field() {
    assert_eq!(
        run(r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"n{i}" }; }
struct S3 { a: R, b: R }
fn p_moved_rest(r: R) -> i64 { let s: S3 = S3 { a: mk(2), b: mk(3) }; let x: R = s.a; let S3 { b, .. } = s; println("mid"); return b.id + x.id; }
fn p_moved_wild(r: R) -> i64 { let s: S3 = S3 { a: mk(5), b: mk(6) }; let x: R = s.a; let S3 { b, a: _ } = s; println("mid"); return b.id + x.id; }
fn p_moved_unread(r: R) -> i64 { let s: S3 = S3 { a: mk(8), b: mk(9) }; let x: R = s.a; let S3 { b, .. } = s; println("mid"); return 1; }
fn main() {
    { let v: i64 = p_moved_rest(mk(1)); println(f"v={v}"); println("one") }
    { let v: i64 = p_moved_wild(mk(4)); println(f"v={v}"); println("two") }
    { let v: i64 = p_moved_unread(mk(7)); println(f"v={v}"); println("three") }
    println("end")
}
"#),
        "mid\ndR3\ndR2\ndR1\nv=5\none\nmid\ndR6\ndR5\ndR4\nv=11\ntwo\ndR8\ndR9\nmid\ndR7\nv=1\nthree\nend\n"
    );
}

/// B-2026-09-07-8 — the interpreter ran the doubled body too, on the same cell,
/// which is what made this row invisible to the A/B rule: both backends agreed on
/// the wrong answer. A fix pin on both sides.
///
/// Twin of `tests/codegen.rs`'s `e2e_rebound_param_into_a_mixed_path_callee`, pinned to the same string.
#[test]
fn test_rebound_param_into_a_mixed_path_callee() {
    assert_eq!(
        run(r#"shared struct Inner { v: i64 }
struct R { id: i64, name: String, inner: Inner }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}", inner: Inner { v: i } }; }
fn f(r: R, c: bool) -> R { let m = r; if c { return m; } return mk(99); }
fn direct(a: R, c: bool) { f(a, c); }
fn rebound(a: R, c: bool) { let q = a; f(q, c); println("  in"); }
fn rebound_bound(a: R, c: bool) -> i64 { let q = a; let w = f(q, c); return w.id; }
fn rebound_only(a: R) { let q = a; println("  only"); }
fn main() {
  println("direct_handback"); direct(mk(1), true);
  println("rebound_handback"); rebound(mk(2), true);
  println("rebound_bound"); println(f"  v={rebound_bound(mk(3), true)}");
  println("rebound_no_call"); rebound_only(mk(4));
  println("end");
}
"#),
        r#"direct_handback
  dR1
rebound_handback
  dR2
  in
rebound_bound
  dR3
  v=3
rebound_no_call
  only
  dR4
end
"#
    );
}

/// B-2026-09-06-16 — `let e = self.e` inside an OWNED receiver ran both the
/// field's and its payload's `Drop` bodies twice for a named-local receiver
/// (`dR51 dE dE dR51`) on every surface, while `let e = h.e` off a by-value
/// PARAMETER (`p_fieldlet`) was one body each — the control. Both backends mark a
/// `let` from a projection off a by-value parameter as a VIEW of the
/// caller-retained value, and both walks stop at `ExprKind::Identifier`; `self`
/// is `ExprKind::SelfValue`. Codegen's is the let epilogue's
/// `field_move_out_source_is_param_view` (which cancels the bodies the enum-let
/// gate registers), the interpreter's is `let_reads_param_view_field`; each gained
/// the owned-`self` root, projections only.
///
/// `justlet` (the field read and never used), `readlet` (a read-only arm), `deep`
/// (two hops), `mid` (a struct-typed field, then a match off it) are the other
/// spellings that doubled; `*/temp` are the fresh-temp receivers, whose bodies
/// the B-2026-09-04-30 gate now retains caller-side for a `let` from a projection
/// too; `borrowedlet` is `mut ref self`, where the second body is the documented
/// copy (design.md "A projection off a borrow is an implicit copy") and must stay
/// at two.
///
/// Twin of `tests/codegen.rs`'s `e2e_owned_self_field_let_runs_one_body`, pinned to the same string.
#[test]
fn test_owned_self_field_let_runs_one_body() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("  dE") } }
struct S { e: E }
struct H1 { e: E }
struct H2 { s: S }

impl H1 {
    fn fieldlet(self) -> i64 { let e = self.e; match e { E.A(r) => { let m = r; return m.id; } E.B => { return 0; } } }
    fn readlet(self) -> i64 { let e = self.e; match e { E.A(r) => { return r.id; } E.B => { return 0; } } }
    fn justlet(self) -> i64 { let e = self.e; return 7; }
    fn borrowedlet(mut ref self) -> i64 { let e = self.e; match e { E.A(r) => { return r.id; } E.B => { return 0; } } }
}
impl H2 {
    fn deep(self) -> i64 { let e = self.s.e; match e { E.A(r) => { let m = r; return m.id; } E.B => { return 0; } } }
    fn mid(self) -> i64 { let s = self.s; match s.e { E.A(r) => { let m = r; return m.id; } E.B => { return 0; } } }
}
fn p_fieldlet(h: H1) -> i64 { let e = h.e; match e { E.A(r) => { let m = r; return m.id; } E.B => { return 0; } } }

fn main() {
    println("fieldlet/local"); let a1 = H1 { e: E.A(mk(1)) }; let x1 = a1.fieldlet(); println(f"  r{x1}");
    println("fieldlet/temp"); let x2 = H1 { e: E.A(mk(2)) }.fieldlet(); println(f"  r{x2}");
    println("readlet/local"); let a3 = H1 { e: E.A(mk(3)) }; let x3 = a3.readlet(); println(f"  r{x3}");
    println("justlet/local"); let a4 = H1 { e: E.A(mk(4)) }; let x4 = a4.justlet(); println(f"  r{x4}");
    println("deep/local"); let a5 = H2 { s: S { e: E.A(mk(5)) } }; let x5 = a5.deep(); println(f"  r{x5}");
    println("mid/local"); let a6 = H2 { s: S { e: E.A(mk(6)) } }; let x6 = a6.mid(); println(f"  r{x6}");
    println("deep/temp"); let x7 = H2 { s: S { e: E.A(mk(7)) } }.deep(); println(f"  r{x7}");
    println("borrowedlet/local"); let mut a8 = H1 { e: E.A(mk(8)) }; let x8 = a8.borrowedlet(); println(f"  r{x8}");
    println("p_fieldlet/local"); let a9 = H1 { e: E.A(mk(9)) }; let x9 = p_fieldlet(a9); println(f"  r{x9}");
    println("p_fieldlet/temp"); let x10 = p_fieldlet(H1 { e: E.A(mk(10)) }); println(f"  r{x10}");
    println("end");
}
"#),
        r#"fieldlet/local
  dE
  dR1
  r1
fieldlet/temp
  dE
  dR2
  r2
readlet/local
  dE
  dR3
  r3
justlet/local
  dE
  dR4
  r7
deep/local
  dE
  dR5
  r5
mid/local
  dE
  dR6
  r6
deep/temp
  dE
  dR7
  r7
borrowedlet/local
  dE
  dR8
  dE
  dR8
  r8
p_fieldlet/local
  dE
  dR9
  r9
p_fieldlet/temp
  dE
  dR10
  r10
end
"#
    );
}

/// B-2026-09-06-15 — a BARE `match self { H1 { e } => .. }` on an OWNED struct
/// receiver ran the payload's `Drop` body twice for a named-local receiver (`dR31 dE
/// dR31`) and lost the enum shell's body for a fresh temp (`dR32`, no `dE`), on all
/// four surfaces. Both backends kept a bare `self` scrutinee on the TRANSFER path
/// (arms own what they bind) on the premise that no caller walk existed for it; a
/// struct receiver's caller walk does exist — the named local's own binding, or the
/// receiver-temp registrar (B-2026-09-04-30) — so the arm's body was a second one,
/// and for a temp the `fn_binds_self_part_out` gate declined the registrar, so the
/// shell's body ran nowhere. Bare owned struct `self` now takes the owned-param VIEW
/// walks on both backends (codegen `bare_self_is_owned_struct_receiver`, the
/// interpreter twin) and a `match self` scrutinee is no longer a bind-out; an owned
/// ENUM receiver keeps the transfer (`enum_recv/*` guard cells; since
/// B-2026-09-06-38 the temp cell also carries the shell's `dE`).
///
/// The free-function twin was the oracle and had a compiled-only defect of its own
/// in the same shape: codegen never marked a plain-STRUCT pattern's leaves as param
/// views, so the NESTED `match e { .. }` inside `match h { H1 { e } => .. }` gave
/// `r` a body beside the caller's walk — `dR51 dE dR51` on jit / -O0 / -O2 against
/// `--interp`'s `dE dR51`, with or without a rebind (`p_whole`, `p_plain`,
/// `p_rebind`). `stage_bare_tuple_bindings_for_bind` now marks them
/// (`collect_plain_struct_pattern_binding_names`). `strleaf` pins a `String` leaf
/// rebound inside the arm (memory unchanged by the mark); `two` pins a scalar leaf
/// beside the enum one; `shell` pins a leaf bound and never consumed.
///
/// Twin of `tests/codegen.rs`'s `e2e_bare_owned_struct_self_scrutinee_binds_views`, pinned to the same string.
#[test]
fn test_bare_owned_struct_self_scrutinee_binds_views() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("  dE") } }
struct H1 { e: E }
struct H2 { e: E, n: i64 }
struct Hs { e: E, s: String }
fn consume(x: R) -> i64 { return x.id }

impl H1 {
    fn whole(self) -> i64 { match self { H1 { e } => { match e { E.A(r) => { let m = r; return m.id; } E.B => { return 0; } } } } }
    fn plain(self) -> i64 { match self { H1 { e } => { match e { E.A(r) => { return r.id; } E.B => { return 0; } } } } }
    fn viacall(self) -> i64 { match self { H1 { e } => { match e { E.A(r) => { return consume(r); } E.B => { return 0; } } } } }
    fn rebind(self) -> i64 { match self { H1 { e } => { let k = e; match k { E.A(r) => { return r.id; } E.B => { return 0; } } } } }
    fn shell(self) -> i64 { match self { H1 { e } => { return 9; } } }
}
impl H2 {
    fn two(self) -> i64 { match self { H2 { e, n } => { match e { E.A(r) => { return r.id + n; } E.B => { return n; } } } } }
}
impl Hs {
    fn strleaf(self) -> i64 { match self { Hs { e, s } => { let m = s; println(f"  s{m}"); match e { E.A(r) => { return r.id; } E.B => { return 0; } } } } }
}
impl E {
    fn m_r(self) -> R { match self { E.A(r) => { return r; } E.B => { return mk(0); } } }
}
fn p_whole(h: H1) -> i64 { match h { H1 { e } => { match e { E.A(r) => { let m = r; return m.id; } E.B => { return 0; } } } } }
fn p_plain(h: H1) -> i64 { match h { H1 { e } => { match e { E.A(r) => { return r.id; } E.B => { return 0; } } } } }
fn p_rebind(h: H1) -> i64 { match h { H1 { e } => { let k = e; match k { E.A(r) => { return r.id; } E.B => { return 0; } } } } }
fn p_strleaf(h: Hs) -> i64 { match h { Hs { e, s } => { let m = s; println(f"  s{m}"); match e { E.A(r) => { return r.id; } E.B => { return 0; } } } } }

fn main() {
    println("whole/local"); let a1 = H1 { e: E.A(mk(31)) }; let x1 = a1.whole(); println(f"  r{x1}");
    println("whole/temp"); let x2 = H1 { e: E.A(mk(32)) }.whole(); println(f"  r{x2}");
    println("plain/local"); let a3 = H1 { e: E.A(mk(33)) }; let x3 = a3.plain(); println(f"  r{x3}");
    println("plain/temp"); let x4 = H1 { e: E.A(mk(34)) }.plain(); println(f"  r{x4}");
    println("viacall/local"); let a5 = H1 { e: E.A(mk(35)) }; let x5 = a5.viacall(); println(f"  r{x5}");
    println("viacall/temp"); let x6 = H1 { e: E.A(mk(36)) }.viacall(); println(f"  r{x6}");
    println("rebind/local"); let a7 = H1 { e: E.A(mk(37)) }; let x7 = a7.rebind(); println(f"  r{x7}");
    println("rebind/temp"); let x8 = H1 { e: E.A(mk(38)) }.rebind(); println(f"  r{x8}");
    println("shell/local"); let a9 = H1 { e: E.A(mk(39)) }; let x9 = a9.shell(); println(f"  r{x9}");
    println("shell/temp"); let x10 = H1 { e: E.A(mk(40)) }.shell(); println(f"  r{x10}");
    println("two/local"); let a11 = H2 { e: E.A(mk(41)), n: 100 }; let x11 = a11.two(); println(f"  r{x11}");
    println("two/temp"); let x12 = H2 { e: E.A(mk(42)), n: 100 }.two(); println(f"  r{x12}");
    println("strleaf/local"); let a13 = Hs { e: E.A(mk(43)), s: "sa".to_string() }; let x13 = a13.strleaf(); println(f"  r{x13}");
    println("strleaf/temp"); let x14 = Hs { e: E.A(mk(44)), s: "sb".to_string() }.strleaf(); println(f"  r{x14}");
    println("p_whole/local"); let b1 = H1 { e: E.A(mk(51)) }; let y1 = p_whole(b1); println(f"  r{y1}");
    println("p_whole/temp"); let y2 = p_whole(H1 { e: E.A(mk(52)) }); println(f"  r{y2}");
    println("p_plain/local"); let b3 = H1 { e: E.A(mk(53)) }; let y3 = p_plain(b3); println(f"  r{y3}");
    println("p_rebind/local"); let b4 = H1 { e: E.A(mk(54)) }; let y4 = p_rebind(b4); println(f"  r{y4}");
    println("p_strleaf/local"); let b6 = Hs { e: E.A(mk(56)), s: "sc".to_string() }; let y6 = p_strleaf(b6); println(f"  r{y6}");
    println("enum_recv/local"); let c1 = E.A(mk(61)); let r1 = c1.m_r(); println(f"  r{r1.id}");
    println("enum_recv/temp"); let r2 = E.A(mk(62)).m_r(); println(f"  r{r2.id}");
    println("end");
}
"#),
        r#"whole/local
  dE
  dR31
  r31
whole/temp
  dE
  dR32
  r32
plain/local
  dE
  dR33
  r33
plain/temp
  dE
  dR34
  r34
viacall/local
  dE
  dR35
  r35
viacall/temp
  dE
  dR36
  r36
rebind/local
  dE
  dR37
  r37
rebind/temp
  dE
  dR38
  r38
shell/local
  dE
  dR39
  r9
shell/temp
  dE
  dR40
  r9
two/local
  dE
  dR41
  r141
two/temp
  dE
  dR42
  r142
strleaf/local
  ssa
  dE
  dR43
  r43
strleaf/temp
  ssb
  dE
  dR44
  r44
p_whole/local
  dE
  dR51
  r51
p_whole/temp
  dE
  dR52
  r52
p_plain/local
  dE
  dR53
  r53
p_rebind/local
  dE
  dR54
  r54
p_strleaf/local
  ssc
  dE
  dR56
  r56
enum_recv/local
  dE
  r61
  dR61
enum_recv/temp
  dE
  r62
  dR62
end
"#
    );
}

/// B-2026-09-06-42 — `let e = self;` inside an OWNED-`self` method on a value enum
/// with its own `Drop` DOUBLE-FREED the payload at -O0 and under the JIT, for a
/// named-local and a fresh-temp receiver alike (clean at -O2 only by accident of
/// the optimizer). The enum whole-rebind arm of let-lowering cap-zeroes the SOURCE
/// of a `let g = f;` move so `g`'s freshly tracked `EnumDrop` is the only owner —
/// but it admitted an `Identifier` source only, and `self` parses as `SelfValue`,
/// so the entry-copied receiver's payload was freed by `self`'s `EnumDrop` and by
/// `e`'s. The gate now admits a bare `self` (the suppressor already resolves it).
///
/// The same rebind also ran every BODY twice on every surface for a NAMED
/// receiver — `dE dR1 dE` (own-`Drop` enum), `dS dR7 dS dR7` (own-`Drop` struct),
/// `dR5 dR5` (struct with only Drop-bearing fields): the local `e` owns the
/// receiver and runs the bodies at its death, and the caller's retained walk over
/// the binding ran them again (a TEMP was already right — the receiver-temp
/// registrar declines via `fn_binds_self_part_out`). `fn_rebinds_self_whole` (a
/// top-level `let <name> = self;`) now stands the named receiver's bodies down at
/// the call site on both backends — own body, struct field walk, enum payload
/// walk — keeping its memory action, which frees the caller's own copy.
///
/// Cells: `enum` / `noshell` / `struct` / `structdrop` (local and temp), the
/// by-value-param twin `free` (unchanged), `mut` (`let mut e = self; e = E.B;`,
/// three bodies: the reassigned value's, the new value's, none from the caller).
/// `cond-true` was the documented residual and is now fixed by B-2026-09-06-45:
/// a rebind NESTED in a branch is still not a whole rebind, but the caller stands
/// down for it too, because the callee frame took the receiver's body back under a
/// per-path guard the rebind clears. That cell dropped its second `dE`; `cond-false`
/// (the non-rebinding path, which the guard leaves armed) is unchanged.
///
/// Twin of `tests/codegen.rs`'s `e2e_whole_self_rebind_in_owned_method_runs_each_body_once`, pinned to the same string.
#[test]
fn test_whole_self_rebind_in_owned_method_runs_each_body_once() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("  dE") } }
enum F { A(R), B }
struct S { r: R }
struct Sd { r: R }
impl Drop for Sd { fn drop(mut ref self) { println("  dS") } }
impl E {
    fn m_let(self) -> i64 { let e = self; match e { E.A(r) => { return r.id; } E.B => { return 0; } } }
    fn m_cond(self, c: bool) -> i64 { if c { let e = self; match e { E.A(r) => { return r.id; } E.B => { return 0; } } } else { match self { E.A(r) => { return r.id + 100; } E.B => { return 100; } } } }
    fn m_mut(self) -> i64 { let mut e = self; e = E.B; match e { E.A(r) => { return r.id; } E.B => { return 0; } } }
}
impl F { fn m_let(self) -> i64 { let e = self; match e { F.A(r) => { return r.id; } F.B => { return 0; } } } }
impl S { fn m_let(self) -> i64 { let e = self; return e.r.id; } }
impl Sd { fn m_let(self) -> i64 { let e = self; return e.r.id; } }
fn f_let(x: E) -> i64 { let e = x; match e { E.A(r) => { return r.id; } E.B => { return 0; } } }
fn main() {
    println("enum/local"); let a = E.A(mk(1)); let x1 = a.m_let(); println(f"  x{x1}");
    println("enum/temp"); let x2 = E.A(mk(2)).m_let(); println(f"  x{x2}");
    println("noshell/local"); let b = F.A(mk(3)); let x3 = b.m_let(); println(f"  x{x3}");
    println("noshell/temp"); let x4 = F.A(mk(4)).m_let(); println(f"  x{x4}");
    println("struct/local"); let c = S { r: mk(5) }; let x5 = c.m_let(); println(f"  x{x5}");
    println("struct/temp"); let x6 = S { r: mk(6) }.m_let(); println(f"  x{x6}");
    println("structdrop/local"); let d = Sd { r: mk(7) }; let x7 = d.m_let(); println(f"  x{x7}");
    println("structdrop/temp"); let x8 = Sd { r: mk(8) }.m_let(); println(f"  x{x8}");
    println("free/local"); let g = E.A(mk(9)); let x9 = f_let(g); println(f"  x{x9}");
    println("free/temp"); let x10 = f_let(E.A(mk(10))); println(f"  x{x10}");
    println("cond-true/local"); let h = E.A(mk(11)); let x11 = h.m_cond(true); println(f"  x{x11}");
    println("cond-false/local"); let i = E.A(mk(12)); let x12 = i.m_cond(false); println(f"  x{x12}");
    println("mut/local"); let j = E.A(mk(13)); let x13 = j.m_mut(); println(f"  x{x13}");
    println("end");
}
"#),
        r#"enum/local
  dE
  dR1
  x1
enum/temp
  dE
  dR2
  x2
noshell/local
  dR3
  x3
noshell/temp
  dR4
  x4
struct/local
  dR5
  x5
struct/temp
  dR6
  x6
structdrop/local
  dS
  dR7
  x7
structdrop/temp
  dS
  dR8
  x8
free/local
  dE
  dR9
  x9
free/temp
  dE
  dR10
  x10
cond-true/local
  dE
  dR11
  x11
cond-false/local
  dR12
  dE
  x112
mut/local
  dE
  dR13
  dE
  x0
end
"#
    );
}

/// B-2026-09-05-20 / B-2026-09-05-21 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_nested_param_destructure_leaf_read_and_moved_one_body_each`, same
/// program and string. The interpreter was the correct reference throughout.
#[test]
fn test_nested_param_destructure_leaf_read_and_moved_one_body_each() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct Gd[T] { r: T, z: i64 }
struct Gn2[T] { inner: Gd[T], z: i64 }
struct Hd { r: R, z: i64 }
struct Hn { inner: Hd, z: i64 }
fn mk(k: i64) -> R { return R { id: k, tag: f"t{k}", xs: [k] } }
fn hLive(h: Hn) -> i64 { let Hn { inner, z } = h; println("in"); let q: i64 = inner.z; return z + q }
fn hLiveLocal(h: Hn) -> i64 { let Hn { inner, z } = h; println("in"); let g: Hd = inner; println("moved"); return z + g.z }
fn gLive[T](h: Gn2[T]) -> i64 { let Gn2 { inner, z } = h; println("in"); let q: i64 = inner.z; return z + q }
fn gLiveLocal[T](h: Gn2[T]) -> i64 { let Gn2 { inner, z } = h; println("in"); let g: Gd[T] = inner; println("moved"); return z + g.z }
fn main() {
    { let a: i64 = hLive(Hn { inner: Hd { r: mk(1), z: 1 }, z: 9 }); println(f"one{a}") }
    { let n: Hn = Hn { inner: Hd { r: mk(2), z: 1 }, z: 9 }; let a: i64 = hLive(n); println(f"two{a}") }
    { let a: i64 = gLiveLocal(Gn2[R] { inner: Gd[R] { r: mk(3), z: 1 }, z: 9 }); println(f"three{a}") }
    { let n: Gn2[R] = Gn2[R] { inner: Gd[R] { r: mk(4), z: 1 }, z: 9 }; let a: i64 = gLiveLocal(n); println(f"four{a}") }
    { let a: i64 = gLive(Gn2[R] { inner: Gd[R] { r: mk(5), z: 1 }, z: 9 }); println(f"five{a}") }
    { let a: i64 = hLiveLocal(Hn { inner: Hd { r: mk(6), z: 1 }, z: 9 }); println(f"six{a}") }
    println("end")
}
"#),
        "in\ndR1\none10\nin\ndR2\ntwo10\nin\nmoved\ndR3\nthree10\nin\nmoved\ndR4\nfour10\nin\ndR5\nfive10\nin\nmoved\ndR6\nsix10\nend\n"
    );
}

/// B-2026-09-05-6 — a place STRUCT argument whose FIELD the callee hands back
/// runs that field's `Drop` body exactly ONCE.
///
/// `cEsc(g)` over `fn cEsc(h: Cd) -> R { let Cd { r, z } = h; r }` gave one
/// object two owners: the caller's own field walk fired at `g`'s live-range end
/// on a value the callee had already given away, and the result's binding fired
/// it again — `in dR13 got13 dR13` against a due `in got13 dR13`. Agreed-wrong
/// on all four surfaces, so no A/B gate saw it, and valgrind-clean, because the
/// entry copy gives each body a buffer of its own; the defect is purely the
/// count.
///
/// Four shapes, each a distinct leg of the fix, and the third and fourth are
/// what make this more than a one-liner:
///
///  * the destructure spelling (`g1`), the row's own;
///  * a two-field struct (`g2`) where only `a` escapes — `dR32` is DUE inside
///    the call and must survive, which is why the mask is per-field rather
///    than a suppression of the binding's whole walk;
///  * the PROJECTION spelling (`g3`, `return h.r`, no destructure), which the
///    row reported clean and is not — the destructure is not the discriminator,
///    the named-local argument is;
///  * the GENERIC callee (`g4`), which never reaches `compile_call`'s argument
///    loop at all, so the free-function arm alone left the interpreter at one
///    body and the three compiled surfaces at two — a divergence, not the
///    agreed-wrong count the row opened on.
///
/// `g5` pins the discarded-result form, which was also two and is not named in
/// the row.
///
/// `g6` is the guard's own pin, and it is what makes the fix more than the
/// masking arm: `fn zEsc(h: Cd) -> i64 { return h.z; }` hands back a SCALAR
/// field, which carries no body to mask, but `disarm_struct_field_bodies_at`
/// retracts and re-registers a binding's walker — so disarming a field the
/// walker never ran ADDED a second walk and doubled the SIBLING `r`'s body.
/// That broke B-2026-09-05-5's pin (`dR7 dR7`) on a shape this row does not
/// touch, which is why the arm is gated on `user_drop_field_indices_mono`.
/// B-2026-09-05-18 — a GENERIC callee's by-value TUPLE param whose element
/// carries a `Drop` type. TWO defects, and the row allowed they might be one:
///
///  1. NO ENTRY COPY. `make_tuple_param_callee_owned` gates on the element
///     `TypeExpr`s, and the monomorph path resolved those with
///     `concrete_generic_struct_inst` — a resolver for a generic struct PATH
///     (`Bag[T]` -> `Bag[String]`) that answers `None` for a BARE type param.
///     `(T, i64)` therefore stayed `(T, i64)`, read as heapless, and the
///     callee got neither the entry copy nor the scope-exit drop its
///     non-generic twin has. The CALLER had already decided otherwise — its
///     `arg_is_entry_copied_heap_tuple` gate reads the caller's inferred
///     `(R, i64)` — so `let (r, z) = p` handed the caller's own buffers to a
///     callee-scope binding and both freed them. That pairing rule is the one
///     B-2026-08-27-37 states at this very site.
///  2. NO ESCAPING-ELEMENT MASK. B-2026-08-28-16 put the place-tuple disarm
///     in `compile_call`'s argument loop, which a generic call never reaches.
///
/// The row measured `karac run` ABORTING while the AOT build merely ran two
/// bodies and was valgrind-clean, and warned against assuming one cause. The
/// discriminator is neither the backend nor the shape: `karac build` defaults
/// to `-O2`, and at `-O0` the AOT binary aborts identically on every cell.
/// `karac run` is simply the unoptimized column. One defect — "AOT is clean"
/// was an artifact of the default opt level, which is worth remembering the
/// next time a JIT-only abort looks like a JIT problem.
///
/// Cells: the escaping element (`a`); its non-generic twin (`b`), the control
/// the row reports as always right; the SCALAR escape (`c`), the mask guard's
/// pin — masking an element that carries no body re-registers the walker and
/// doubles element 0's, the shape that broke a sibling row's pin when the
/// struct arm shipped without the guard; the DISCARDED result (`d`); element
/// 1 escaping while element 0 keeps its body (`e`); a callee that destructures
/// and returns NOTHING (`f`), which is defect 1 with no escape at all; and the
/// generic STRUCT discard (`g`).
///
/// `d` and `g` are why the fix has a third part. A discarded GENERIC result
/// had no owner on the compiled backends at all — `fn_return_type_names` has
/// no entry for a template, which is never `declare_function`'d — and the
/// caller's UNMASKED walk was covering for it. Masking correctly, as the
/// concrete path always has, removes the cover, so the two had to land
/// together. The whole-param spelling of that same miss
/// (`fn passG[T](x: T) -> T`) reaches no place argument and is filed apart.
///
/// This pin PASSES on the pre-fix tree, unlike its three compiled siblings, and
/// that is the point rather than a gap: the interpreter was correct on every
/// cell here, so it is the oracle the other three are measured against. It
/// guards the oracle itself against a later regression — the shape B-2026-09-05-6
/// needed an interpreter-side arm for, one channel over.
/// B-2026-09-05-29 — a discarded GENERIC call whose callee returns its WHOLE
/// by-value param, called on a TEMPORARY: `fn passG[T](x: T) -> T` under
/// `let _ = passG(mk(80));`. Every compiled surface ran no `Drop` body for the
/// result and lost 11 B in 2 blocks; the interpreter was right on every cell.
///
/// This pin PASSES on the pre-fix tree, and that is the point rather than a
/// gap — it is the ORACLE the three compiled siblings are measured against, so
/// it guards the oracle itself against a later regression. Same role, and same
/// reason, as the interpreter twin of B-2026-09-05-18 above.
///
/// Cells: the row's own shape (`a`); its NON-GENERIC twin (`b`); the
/// NAMED-LOCAL argument (`c`); two temporaries where only the second escapes
/// (`d`); the WRAPPING return (`e`) and the SCALAR return (`f`); the BOUND
/// result (`g`); the LABELLED spelling (`h`); and the LOOP.
#[test]
fn generic_whole_param_discarded_temp_runs_one_body() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] }; }
struct H { r: R, z: i64 }
fn passG[T](x: T) -> T { println("inP"); return x; }
fn passN(x: R) -> R { println("inN"); return x; }
fn pickB[T](a: T, b: T) -> T { println("inB"); return b; }
fn wrapG[T](x: T) -> H { println("inW"); return H { r: x, z: 1 }; }
fn scalarG[T](x: T) -> i64 { println("inS"); return 3; }
fn main() {
  let _ = passG(mk(80)); println("a");
  let _ = passN(mk(81)); println("b");
  let g = mk(82); let _ = passG(g); println("c");
  let _ = pickB(mk(83), mk(84)); println("d");
  let _ = wrapG(mk(85)); println("e");
  let _ = scalarG(mk(86)); println("f");
  let k = passG(mk(87)); println(f"k{k.id}");
  let _ = passG(x: mk(88)); println("h");
  let mut i = 0; while i < 3 { let _ = passG(mk(90 + i)); i = i + 1; } println("g");
  println("end");
}
"#),
        "inP\ndR80\na\ninN\ndR81\nb\ninP\ndR82\nc\ninB\ndR83\ndR84\nd\ninW\ndR85\ne\ninS\ndR86\nf\ninP\nk87\ndR87\ninP\ndR88\nh\ninP\ndR90\ninP\ndR91\ninP\ndR92\ng\nend\n",
        "the interpreter is the oracle here: one body per object on every cell"
    );
}

#[test]
fn generic_tuple_param_element_is_owned_once() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] }; }
struct Gd[T] { r: T, z: i64 }
fn kEsc[T](p: (T, i64)) -> T { let (r, z) = p; println("in"); return r; }
fn nEsc(p: (R, i64)) -> R { let (r, z) = p; println("in2"); return r; }
fn zEsc[T](p: (T, i64)) -> i64 { let (r, z) = p; println("in3"); return z; }
fn kEsc1[T](p: (i64, T)) -> T { let (z, r) = p; println("in5"); return r; }
fn tOnly[T](p: (T, i64)) { let (r, z) = p; println("in6"); }
fn gEsc[T](h: Gd[T]) -> T { let Gd { r, z } = h; println("in7"); return r; }
fn main() {
  let a = (mk(91), 9);  let o1 = kEsc(a);   println(f"got{o1.id}");
  let b = (mk(92), 9);  let o2 = nEsc(b);   println(f"got{o2.id}");
  let c = (mk(93), 7);  let o3 = zEsc(c);   println(f"gotz{o3}");
  let d = (mk(94), 9);  let _  = kEsc(d);   println("after");
  let e = (5, mk(95));  let o5 = kEsc1(e);  println(f"got{o5.id}");
  let f = (mk(96), 9);  tOnly(f);           println("after6");
  let g = Gd[R] { r: mk(97), z: 9 }; let _ = gEsc(g); println("after7");
  println("end");
}
"#),
        "in\ngot91\ndR91\nin2\ngot92\ndR92\nin3\ndR93\ngotz7\nin\ndR94\nafter\nin5\ngot95\ndR95\nin6\ndR96\nafter6\nin7\ndR97\nafter7\nend\n",
        "the interpreter is the oracle here and was correct throughout: one body \
         per object, at the owner that outlives the call"
    );
}

/// B-2026-09-06-1 — a DISCARDED generic call's moved-in argument runs its
/// `Drop` body on this backend. `let g = mk(1); passG(g);` over
/// `fn passG[T](x: T) -> T` printed nothing here against `dR1` on all three
/// compiled surfaces: the caller had stood down (the callee hands the
/// argument back) and the discard site, reading the declared return `T`,
/// found no type to run a body for. Same for a generic METHOD under both
/// discard spellings. The discard arm now resolves the type from the VALUE
/// when the callee returns its own generic parameter
/// (`discard_return_type_from_value`), and the method gate admits such a
/// method (`user_method_returns_owned_type`). Twin of `tests/codegen.rs`'s
/// `e2e_discarded_generic_call_runs_the_moved_in_argument_body`, same
/// program and string; that side was right throughout.
///
/// `one`/`four` the free-fn bare statement (named / temp), `two`/`five` its
/// `let _ =` spelling (always right), `three` the concrete twin, `six`..`nine`
/// the generic method under `let _ =` and bare, named and temp, `ten`/
/// `eleven` its concrete twin, `twelve` the result bound (one body, at the
/// binding's death).
#[test]
fn test_discarded_generic_call_runs_the_moved_in_argument_body() {
    assert_eq!(
        run(r#"struct R { id: i64, names: Vec[String] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, names: [f"a{i}", f"b{i}"] }; }
fn passG[T](x: T) -> T { return x; }
fn passN(x: R) -> R { return x; }
struct H { n: i64 }
impl H { fn keep[T](ref self, x: T) -> T { return x; } fn keepN(ref self, x: R) -> R { return x; } }
fn main() {
    { let g: R = mk(1); passG(g); println("one") }
    { let g: R = mk(2); let _ = passG(g); println("two") }
    { let g: R = mk(3); passN(g); println("three") }
    { passG(mk(4)); println("four") }
    { let _ = passG(mk(5)); println("five") }
    { let h: H = H { n: 1 }; let g: R = mk(6); let _ = h.keep(g); println("six") }
    { let h: H = H { n: 1 }; let g: R = mk(7); h.keep(g); println("seven") }
    { let h: H = H { n: 1 }; let _ = h.keep(mk(8)); println("eight") }
    { let h: H = H { n: 1 }; h.keep(mk(9)); println("nine") }
    { let h: H = H { n: 1 }; let g: R = mk(10); let _ = h.keepN(g); println("ten") }
    { let h: H = H { n: 1 }; h.keepN(mk(11)); println("eleven") }
    { let g: R = mk(12); let w: R = passG(g); println(f"w{w.id}"); println("twelve") }
    println("end")
}
"#),
        "dR1\none\ndR2\ntwo\ndR3\nthree\ndR4\nfour\ndR5\nfive\ndR6\nsix\ndR7\nseven\ndR8\neight\ndR9\nnine\ndR10\nten\ndR11\neleven\nw12\ndR12\ntwelve\nend\n"
    );
}

/// B-2026-09-07-1 — A DEEP-CHAIN MOVE-OUT WHOSE HOP IS THEN BOUND OUT RAN THE
/// MOVED LEAF'S `Drop` BODY TWICE, and the compiled second fire read a HUSK.
///
/// `let x = o.h.r;` records the move as a PATH on the source (`(o, [h, r])`),
/// and the source's own walk honours it. But `let Outer { h, k } = o;` gives
/// the bound leaf `h` a walker of its OWN, keyed on `h`, and nothing rewrote
/// the record onto that key — so `h` ran `r`'s body a second time over `o`'s
/// copy. The WILDCARD spelling was already correct, which is what put the axis
/// on the bound leaf rather than on the destructure.
///
/// THE `String` FIELD IS LOAD-BEARING and the row that filed this said so: with
/// a plain `i64` payload both backends print `dR1 dR2 dR1` and the agreement
/// reads as "both wrong the same way". Add a heap field and they split — the
/// compiled copy's `name` was cap-zeroed by the move-out, so it printed
/// `dR1/` where `--interp` printed `dR1/n1`. So this is a run-vs-build
/// divergence as well as a doubled body, and only the `String` spelling shows
/// it. MEMORY IS BALANCED throughout (83 allocs, 83 frees, 0 errors) — a
/// doubled body over an intact free set, which no sanitizer can see and only a
/// body-COUNT assertion catches.
///
/// `bound` is the row's cell; `renamed` (`h: hh`) and `two_outs` (two moves out
/// of one hop, which doubled TWICE) are the same axis through other spellings;
/// `deep` is the three-hop chain, and it is the cell that forced the two
/// backends to move together — the interpreter applies its map at WALK time and
/// was fixed by the record alone, while codegen emits the leaf's walker eagerly
/// and needed an explicit re-emit, so fixing only the obvious half would have
/// left a fresh divergence behind. `wild`, `nosplit` and `nestedpat` are the
/// controls that must not move.
///
/// Twin of `tests/codegen.rs`'s
/// `e2e_deep_chain_move_out_then_bound_hop_runs_each_body_once`, byte-identical source and expectation.
#[test]
fn test_deep_chain_move_out_then_bound_hop_runs_each_body_once() {
    let out = run(r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}/{self.name}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"n{i}" } }
struct Inner { r: R, q: R }
struct Deep { g: Inner, z: i64 }
struct Outer { h: Inner, k: R }
struct OuterD { h: Deep, k: R }
fn main() {
    println("bound");     { let o: Outer = Outer { h: Inner { r: mk(1), q: mk(2) }, k: mk(3) }; let x: R = o.h.r; let Outer { h, k } = o; println("  mid"); println(f"  v={k.id}") }
    println("renamed");   { let o: Outer = Outer { h: Inner { r: mk(4), q: mk(5) }, k: mk(6) }; let x: R = o.h.r; let Outer { h: hh, k } = o; println("  mid"); println(f"  v={k.id}") }
    println("two_outs");  { let o: Outer = Outer { h: Inner { r: mk(7), q: mk(8) }, k: mk(9) }; let x: R = o.h.r; let y: R = o.h.q; let Outer { h, k } = o; println("  mid"); println(f"  v={k.id}") }
    println("deep");      { let o: OuterD = OuterD { h: Deep { g: Inner { r: mk(10), q: mk(11) }, z: 1 }, k: mk(12) }; let x: R = o.h.g.r; let OuterD { h, k } = o; println("  mid"); println(f"  v={k.id}") }
    println("wild");      { let o: Outer = Outer { h: Inner { r: mk(13), q: mk(14) }, k: mk(15) }; let x: R = o.h.r; let Outer { h: _, k } = o; println("  mid"); println(f"  v={k.id}") }
    println("nosplit");   { let o: Outer = Outer { h: Inner { r: mk(16), q: mk(17) }, k: mk(18) }; let Outer { h, k } = o; println("  mid"); println(f"  v={k.id}") }
    println("nestedpat"); { let o: Outer = Outer { h: Inner { r: mk(19), q: mk(20) }, k: mk(21) }; let Outer { h: Inner { r, q }, k } = o; println("  mid"); println(f"  v={k.id}") }
    println("end");
}
"#);
    assert_eq!(out, "bound\n  dR1/n1\n  dR2/n2\n  mid\n  v=3\n  dR3/n3\nrenamed\n  dR4/n4\n  dR5/n5\n  mid\n  v=6\n  dR6/n6\ntwo_outs\n  dR7/n7\n  dR8/n8\n  mid\n  v=9\n  dR9/n9\ndeep\n  dR10/n10\n  dR11/n11\n  mid\n  v=12\n  dR12/n12\nwild\n  dR13/n13\n  dR14/n14\n  mid\n  v=15\n  dR15/n15\nnosplit\n  dR17/n17\n  dR16/n16\n  mid\n  v=18\n  dR18/n18\nnestedpat\n  dR20/n20\n  dR19/n19\n  mid\n  v=21\n  dR21/n21\nend\n", "got:\n{out}");
}

/// B-2026-09-15-30 — the ORACLE for the codegen twin, and the reason the
/// compiled half can be trusted to be measuring the right program.
///
/// The defect is compiled-only: `compile_mono_function` never installed
/// `discarded_branch_spans`, so a discarded branch inside a generic body
/// cloned a container element with no owner to free it. The interpreter has
/// never had that gap, so this fixture's job is to pin the expectation rather
/// than to reproduce anything — it printed exactly this before the fix and
/// exactly this after.
///
/// The CODEGEN twin is `tests/codegen.rs`'s
/// `e2e_discarded_branch_in_a_generic_body_clones_nothing`, byte-identical
/// source and expectation; the leak itself is gated in
/// `tests/memory_sanitizer.rs`.
#[test]
fn test_discarded_branch_in_a_generic_body_clones_nothing() {
    let out = run(
        r#"fn stmtDiscard[T](v: Vec[String], c: bool, t: T) -> T { if c { v[0] } else { v[1] }; return t; }
fn loopDiscard[T](v: Vec[String], t: T) -> T { for i in 0..2 { if i == 0 { v[0] } else { v[1] } } return t; }
fn blockDiscard[T](v: Vec[String], c: bool, t: T) -> T { { if c { v[0] } else { v[1] } }; return t; }
fn matchDiscard[T](v: Vec[String], c: i64, t: T) -> T { match c { 0 => { v[0] } _ => { v[1] } }; return t; }
fn keptValue[T](v: Vec[String], c: bool, t: T) -> T { let s = if c { v[0] } else { v[1] }; println(f"  kept {s.len()}"); return t; }
fn innerDiscard[T](v: Vec[String], c: bool, t: T) -> T { if c { v[0] } else { v[1] }; return t; }
fn outerCalls[T](v: Vec[String], c: bool, t: T) -> T { let r = innerDiscard(v, c, t); return r; }
fn ident[T](t: T) -> T { return t; }
fn mkVec() -> Vec[String] {
    let mut v: Vec[String] = Vec.new();
    v.push(f"aaaaaaaaaaaaaaaaaaaaaaaa-0");
    v.push(f"bbbbbbbbbbbbbbbbbbbbbbbb-1");
    return v;
}

fn main() {
    println("stmt");   println(f"  {stmtDiscard(mkVec(), true, 1)}");
    println("loop");   println(f"  {loopDiscard(mkVec(), 2)}");
    println("block");  println(f"  {blockDiscard(mkVec(), true, 3)}");
    println("match");  println(f"  {matchDiscard(mkVec(), 0, 4)}");
    println("kept");   println(f"  {keptValue(mkVec(), true, 5)}");
    println("nested"); println(f"  {outerCalls(mkVec(), true, 6)}");
    { let vc = mkVec(); let c = ident(true); if c { vc[0] } else { vc[1] }; println("callerdiscard"); println(f"  {vc[0].len()}") }
    { let vk = mkVec(); let ck = ident(true); let s = if ck { vk[0] } else { vk[1] }; println("callerkept"); println(f"  {s.len()}"); println(f"  {vk[0].len()}") }
    println("twoinst"); println(f"  {stmtDiscard(mkVec(), true, 7)}"); println(f"  {stmtDiscard(mkVec(), false, 8)}");
    println("end")
}
"#,
    );
    assert_eq!(out, "stmt\n  1\nloop\n  2\nblock\n  3\nmatch\n  4\nkept\n  kept 26\n  5\nnested\n  6\ncallerdiscard\n  26\ncallerkept\n  26\n  26\ntwoinst\n  7\n  8\nend\n", "got:\n{out}");
}
