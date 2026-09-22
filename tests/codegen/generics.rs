//! generics, monomorphisation, traits, associated items -- fixtures for `tests/codegen.rs`.
//!
//! Split out of `tests/codegen.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test codegen` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test codegen generics::
//!
//! New fixtures about generics, monomorphisation, traits, associated items belong in this file.

use super::*;

/// B-2026-09-05-3 — the exact-output twin of the memory_sanitizer pin: a
/// generic callee that destructures its by-value param and returns the
/// leaf runs each body ONCE, in the interpreter's order, on the compiled
/// build. Pre-fix this program aborted in `free()`.
#[test]
fn test_e2e_generic_callee_destructured_escaping_leaf_once() {
    let out = run_program(
        r#"
struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] }; }
struct Gd[T] { r: T, z: i64 }
fn gEsc[T](h: Gd[T]) -> T { let Gd { r, z } = h; println("in"); return r; }
fn gEsc2[U](h: Gd[U]) -> U { let Gd { r, z } = h; println("in2"); return r; }
fn gZ[T](h: Gd[T]) -> i64 { let Gd { r, z } = h; println("inZ"); return z; }
fn c_esc()  { let out = gEsc(Gd[R] { r: mk(5), z: 9 }); println(f"got{out.id}"); }
fn c_u()    { let out = gEsc2(Gd[R] { r: mk(11), z: 9 }); println(f"got{out.id}"); }
fn c_str()  { let out = gEsc(Gd[String] { r: f"s{8}", z: 9 }); println(f"got{out}"); }
fn c_z()    { let out = gZ(Gd[R] { r: mk(12), z: 9 }); println(f"got{out}"); }
fn main() { c_esc(); c_u(); c_str(); c_z(); println("end"); }
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out, "in\ngot5\ndR5\nin2\ngot11\ndR11\nin\ngots8\ninZ\ndR12\ngot9\nend\n",
            "each destructured-and-returned leaf dies exactly once, at its \
                 owner in the caller; the leaf dropped inside the callee keeps \
                 its place; got {out:?}"
        );
    }
}

/// B-2026-08-06-12 — a generic struct LITERAL in RECEIVER position.
///
/// `Box { v: <String> }.take()` passed `karac check` and ran correctly
/// under the interpreter, then failed `karac build` with an LLVM verifier
/// error (`insertvalue { i64 } undef, { ptr, i64, i64 }`). A run-vs-build
/// divergence, so it belongs on the default codegen leg.
///
/// The parser gives a `MethodCall` its RECEIVER's span, so the literal and
/// the call share one `expr_types` key and the call's type wins it; the
/// literal's instantiation record is dropped entirely. BOTH the literal's
/// layout and the callee's monomorph selection read that one missing
/// record, so fixing either alone only moves the error — a correctly
/// shaped `{ { ptr, i64, i64 } }` receiver handed to a callee still
/// declared `{ i64 }`. Recovery is from the literal's own field
/// initializers, whose spans are uncontested.
///
/// Carries the whole isolation table, because four of these shapes already
/// worked and must keep working — the failure needs a GENERIC struct AND a
/// literal in receiver position AND a wide instantiated field. `peek` is a
/// `ref self` method whose body ignores the field: it failed too, so the
/// bug never depended on the body touching `T`. It is a NON-oracle for
/// layout on its own (it returns a constant), which is why every other arm
/// reads the payload back.
///
/// `Num[i64]` is a SEPARATE struct rather than a second `Box`
/// instantiation on purpose. Two instantiations of one generic struct
/// reached through a mix of literal and named receivers hit a distinct,
/// PRE-EXISTING defect — the unmangled `@Box.take` slot takes whichever
/// signature gets there first — which reproduces identically without this
/// change and is filed separately. Mixing it in here would make this test
/// red for a reason it does not own.
///
/// `env.args().len()` seeds the width so the payload is not a compile-time
/// constant (B-2026-08-04-17).
#[test]
fn e2e_generic_struct_literal_as_method_receiver() {
    let src = r#"
struct Box[T] { v: T }
struct Pair[T] { a: T, b: i64 }
struct Num[T] { n: T }
impl[T] Box[T] { fn take(self) -> T { self.v } }
impl[T] Box[T] { fn peek(ref self) -> i64 { 7 } }
impl[T] Pair[T] { fn first(self) -> T { self.a } }
impl[T] Num[T] { fn get(self) -> T { self.n } }
fn takefn[T](b: Box[T]) -> T { b.v }
fn main() {
    let n: i64 = env.args().len();
    println(Box { v: "payload-past-inline-width-here".repeat(n) }.take().len());
    println(Pair { a: "second-field-shape".repeat(n), b: 7 }.first().len());
    println(Box { v: "ignored-by-the-body".repeat(n) }.peek());
    println(takefn(Box { v: "free-fn-arg-position".repeat(n) }).len());
    let b = Box { v: "named-receiver-still-works".repeat(n) };
    println(b.take().len());
    println(Num { n: n + 41 }.get());
}
"#;
    assert_eq!(run_program(src).as_deref(), Some("30\n18\n7\n20\n26\n42\n"));
}

/// B-2026-08-06-22 — a DOUBLY-nested generic chain, `Box[Box[Wide]]`.
///
/// `outer.take().take().f` passed `karac check`, ran correctly under the
/// interpreter, and failed `karac build` with "cannot resolve field 'f'",
/// while the fully-bound `let m = outer.take(); let q = m.take(); q.f`
/// built and ran. Its parent row B-2026-08-06-19 fixed the SINGLE-level
/// chain and did not reach this one.
///
/// The cause is a SPAN COLLISION, not the value-representation mismatch the
/// original report guessed. A chained receiver is materialized into a synth
/// local by `try_compile_freshtemp_user_method`, and that local was seeded
/// from `enum_inst_type_from_span` — but the parser gives a `MethodCall`
/// its RECEIVER's span, so for `outer.take()` the span lookup answers
/// `Box[Box[Wide]]` (the type called ON) rather than `Box[Wide]` (the type
/// RETURNED). The next link then selected its monomorph one level too high
/// and emitted a `take` over `{ i64 }`, so the field read met an `i64`.
///
/// The measured shape of the wrong answer is what pins this: the failing
/// build emitted a second `take` whose `self` was `{ i64 }` where the
/// working build emitted `{ i64 x6 }`.
#[test]
fn e2e_doubly_nested_generic_chain_builds() {
    let Some(out) = run_program(
            "struct Wide { a: i64, b: i64, c: i64, d: i64, e: i64, f: i64 }\n\
             struct Box[T] { v: T }\n\
             impl[T] Box[T] { fn take(self) -> T { self.v } }\n\
             fn main() {\n\
             \x20   let n: i64 = env.args().len();\n\
             \x20   let inner = Box { v: Wide { a: n, b: 2i64, c: 3i64, d: 4i64, e: 5i64, f: 6i64 } };\n\
             \x20   let outer: Box[Box[Wide]] = Box { v: inner };\n\
             \x20   println(outer.take().take().f);\n\
             \x20   // the fully-bound twin, on its own value — `take(self)`\n\
             \x20   // consumes, so it cannot re-use `outer`.\n\
             \x20   let inner2 = Box { v: Wide { a: n, b: 2i64, c: 3i64, d: 4i64, e: 5i64, f: 6i64 } };\n\
             \x20   let outer2: Box[Box[Wide]] = Box { v: inner2 };\n\
             \x20   let m = outer2.take();\n\
             \x20   let q = m.take();\n\
             \x20   println(q.f);\n\
             }\n",
        ) else {
            return;
        };
    // The chained and the fully-bound spellings must agree — the bound one
    // always worked, so it is the oracle here.
    assert_eq!(out, "6\n6\n");
}

/// B-2026-08-06-19 — a chained FIELD ACCESS on a GENERIC method's return.
///
/// `w.take().f` passed `karac check`, ran correctly under the interpreter,
/// and failed `karac build` with "cannot resolve field 'f' on this
/// receiver", while the one-condition-apart `let x = w.take(); x.f` built
/// and ran. A run-vs-build divergence, so it belongs on the default
/// codegen leg.
///
/// The cause is NOT the span collision its parent row (B-2026-08-06-12)
/// traces to. The declare loop deliberately SKIPS `fn_return_type_names`
/// for a generic return — `-> T` has no static name, which is that map's
/// contract — so the chain's lookup misses entirely rather than returning a
/// wrong answer. The bound form works because `x` gets a concrete
/// `var_type_names` entry that the chain never gets. The fix recovers the
/// erased `-> T` from the method AST the same declare arm keeps in
/// `generic_fns` and substitutes the RECEIVER's instantiation.
///
/// The NON-generic twin (`impl P { fn take(self) -> Wide }`) is carried as
/// a live control: it registers `P.take -> Wide` and its chain always
/// worked, and that asymmetry is what localized the bug. So is the bound
/// form, which must keep working.
///
/// `wrap` covers the row's related note — a method returning `Box[T]`
/// rather than bare `T`, chained through the wrapper's own field
/// (`w.wrap().v.f`). That shape needs the intermediate's generic ARGS, not
/// just its head, which is why the fix resolves a full `TypeExpr` rather
/// than a bare name.
///
/// The enum arm pins that a `-> T` at `T = <enum>` resolves too — the
/// substitution gate accepts `enum_layouts` as well as structs, and a match
/// on the chained result is what would mis-bind if it did not.
///
/// `env.args().len()` seeds every payload so nothing is a compile-time
/// constant (B-2026-08-04-17).
#[test]
fn e2e_chained_field_access_on_generic_method_return() {
    let src = r#"
struct Wide { a: i64, b: i64, c: i64, d: i64, e: i64, f: i64 }
struct Box[T] { v: T }
struct P { v: Wide }
enum Col { Red, Green, Blue }
impl[T] Box[T] {
    fn take(self) -> T { self.v }
    fn wrap(self) -> Box[T] { Box { v: self.v } }
}
impl P { fn take(self) -> Wide { self.v } }
fn main() {
    let n: i64 = env.args().len();
    // (a) the reported shape — chained field on a bare-`T` return.
    let w = Box { v: Wide { a: n, b: 2, c: 3, d: 4, e: 5, f: 6 } };
    println(w.take().f);
    // (b) the one-condition-apart bound form, which always worked.
    let w2 = Box { v: Wide { a: n, b: 2, c: 3, d: 4, e: 5, f: 7 } };
    let x = w2.take();
    println(x.f);
    // (c) NON-generic control: `P.take` registers a concrete return name.
    let p = P { v: Wide { a: n, b: 2, c: 3, d: 4, e: 5, f: 8 } };
    println(p.take().f);
    // (d) the row's related note — a `Box[T]` return, chained through the
    // wrapper's own field, which needs the intermediate's generic ARGS.
    let w3 = Box { v: Wide { a: n, b: 2, c: 3, d: 4, e: 5, f: 9 } };
    println(w3.wrap().v.f);
    // (e) a `-> T` at `T = <enum>`, matched on directly.
    let c: Box[Col] = Box { v: if n > 0 { Col.Green } else { Col.Red } };
    match c.take() {
        Col.Red => println(1),
        Col.Green => println(2),
        Col.Blue => println(3),
    }
}
"#;
    assert_eq!(run_program(src).as_deref(), Some("6\n7\n8\n9\n2\n"));
}

/// B-2026-08-06-20 — TWO instantiations of ONE generic struct reached
/// through a MIX of literal and named receivers.
///
/// The sibling test above deliberately uses a separate `Num[i64]` struct to
/// avoid this shape. Here it is on purpose: `Box[i64]` via a literal
/// receiver and `Box[String]` via a named one, in one program.
///
/// The unmangled `@Box.take` is the fallback slot the monomorphizer emits
/// when it cannot resolve an instantiation — `mangle_mono_name` appends a
/// `$` token only for a param it has a subst for, so an empty subst mangles
/// back to the base name. The i64 literal lands there (its `n + 41` field
/// initializer has no nameable type, and -12's recovery is fail-closed on
/// that), fixing `@Box.take`'s signature at `{ i64 }`. The direct-dispatch
/// arm then found that symbol by name and handed it the String receiver
/// too: "Call parameter type does not match function signature".
///
/// Both receiver forms build ALONE — two named receivers at two
/// instantiations emit `Box.take$i64` and `Box.take$struct` correctly — so
/// it takes the mix, in that order, to reach it. `karac check` passed and
/// `karac run` printed the right answers throughout; only `build` failed.
///
/// Both orders are exercised, since the defect is order-sensitive by
/// construction (whichever instantiation is emitted first wins the slot),
/// and `peek` covers a second method on the same struct so the fix is not
/// specific to one symbol. Verified RED against the pre-fix compiler.
#[test]
fn e2e_generic_method_mixed_literal_and_named_receivers() {
    let src = r#"
struct Box[T] { v: T }
impl[T] Box[T] {
    fn take(self) -> T { self.v }
    fn peek(ref self) -> i64 { 9 }
}
fn main() {
    let n: i64 = env.args().len();
    println(Box { v: n + 41 }.take());
    let b = Box { v: "named-receiver-string".repeat(n) };
    println(b.take().len());

    let c = Box { v: "named-first-this-time".repeat(n) };
    println(c.take().len());
    println(Box { v: n + 6 }.take());

    println(Box { v: n + 1 }.peek());
    let d = Box { v: "peek-receiver".repeat(n) };
    println(d.peek());
}
"#;
    assert_eq!(run_program(src).as_deref(), Some("42\n21\n21\n7\n9\n9\n"));
}

/// B-2026-08-06-23 — a generic struct LITERAL in RECEIVER position whose
/// field initializer is an ARITHMETIC expression.
///
/// `Box { v: f * 2.0 }.take()` passed `karac check`, ran correctly under
/// the interpreter, and failed `karac build` with a module verification
/// error — `insertvalue { i64 } undef, double` — because the literal
/// lowered at the ERASED base layout.
///
/// B-2026-08-06-12's receiver-literal recovery reads each field
/// initializer's type and is deliberately FAIL-CLOSED: an initializer it
/// cannot NAME declines the recovery rather than guessing, since a wrong
/// instantiation silently lowers a struct at another type's layout. Nothing
/// named an arithmetic expression, so `f * 2.0` came back empty.
///
/// `n + 41` is carried as a live control BECAUSE IT ALWAYS "WORKED" — by
/// accident. The erased fallback layout is `{ i64 }`, so an i64 field's
/// wrong answer happened to be the right one, which is exactly why the
/// f64 case was the one that surfaced. It must keep working for the real
/// reason now.
///
/// The arms cover both lowered spellings and the type-vs-bool split: an
/// operator reaches codegen already rewritten by `rewrite_binary` /
/// `rewrite_unary` into `Type.op(..)`, so the name comes off the callee
/// path — two args for a binary op, ONE for unary `neg` (which failed
/// after the binary arm alone was in place). A COMPARISON lowers through
/// the identical shape but yields `bool`, not the operand type; naming it
/// `i32` would be a silently wrong instantiation of the kind the
/// fail-closed design exists to prevent.
///
/// The narrow widths are included because the row predicts them broken for
/// the same reason, and `String` because it is the non-scalar control.
///
/// `env.args().len()` seeds the values so nothing is a compile-time
/// constant (B-2026-08-04-17).
#[test]
fn e2e_generic_receiver_literal_with_arithmetic_field_initializer() {
    let src = r#"
struct Box[T] { v: T }
impl[T] Box[T] { fn take(self) -> T { self.v } }
fn main() {
    let n: i64 = env.args().len();
    let f: f64 = 1.5;
    let a: i32 = 3i32;
    let b: i32 = 4i32;
    let u: u8 = 200u8;
    let s: String = "ab";
    println(Box { v: f * 2.0 }.take());             // the reported shape
    println(Box { v: n + 41 }.take());              // correct-by-accident control
    println(Box { v: a + b }.take());               // narrow signed
    println(Box { v: u / 2u8 }.take());             // narrow unsigned
    println(Box { v: a < b }.take());               // comparison -> bool
    println(Box { v: n as f64 }.take());            // cast
    println(Box { v: (n + 1i64) * 2i64 }.take());   // nested
    println(Box { v: -f }.take());                  // lowered UNARY
    println(Box { v: s + "cd" }.take());            // non-scalar control
    let g = Box { v: f * 3.0 };                     // non-receiver position
    println(g.v);
}
"#;
    assert_eq!(
        run_program(src).as_deref(),
        Some("3\n42\n7\n100\ntrue\n1\n4\n-1.5\nabcd\n4.5\n")
    );
}

/// B-2026-08-06-25 — TWO distinct NESTED generic instantiations in ONE
/// program must reach distinct monomorph symbols.
///
/// The mono name mangled only the type argument's HEAD, so
/// `Box[Box[i64]]`, `Box[Box[String]]` and `Box[Box[Wide]]` all collided on
/// `Box.take$Box`: whichever was emitted first defined the signature and
/// every other call site was type-checked against it, failing module
/// verification with `Call parameter type does not match function
/// signature`. The interpreter was correct throughout.
///
/// IT TAKES TWO DIFFERENTLY-INSTANTIATED NESTED CHAINS IN ONE PROGRAM.
/// Each of these builds and runs correctly ON ITS OWN, which is why every
/// single-chain probe — including the ones that closed B-2026-08-06-22 —
/// missed it. That is the whole reason this fixture is one program rather
/// than several focused ones.
///
/// The three-level chain is here because `Box[Box[Box[i64]]]` must differ
/// from `Box[Box[i64]]` too: both mangle `$Box` at the head, so a fix that
/// only distinguished the INNER head would still collide these two.
///
/// The last two arms are the load-bearing NEGATIVE controls: a
/// single-level `Box[Wide]` / `Box[i64]` must keep its existing symbol
/// untouched. The fix appends a suffix ONLY when the type argument is
/// itself a generic instantiation, so the whole pre-existing mono surface
/// stays byte-identical — verified directly in the emitted symbols, where
/// `Box.take$Wide`, `Box.take$i64` and `Box.take$struct$T_ct_String` are
/// unchanged and only the nested cases gain `$T_gi_…`.
///
/// `env.args().len()` seeds the `Wide` payload (B-2026-08-04-17).
#[test]
fn e2e_two_nested_generic_instantiations_get_distinct_monos() {
    let src = r#"
struct Wide { a: i64, b: i64, c: i64, d: i64, e: i64, f: i64 }
struct Box[T] { v: T }
impl[T] Box[T] { fn take(self) -> T { self.v } }
fn main() {
    let n: i64 = env.args().len();
    let c2: Box[Box[i64]] = Box { v: Box { v: 41i64 } };
    println(c2.take().take());
    let c3: Box[Box[Box[i64]]] = Box { v: Box { v: Box { v: 7i64 } } };
    println(c3.take().take().take());
    let s2: Box[Box[String]] = Box { v: Box { v: "hi" } };
    println(s2.take().take());
    let w2: Box[Box[Wide]] = Box { v: Box { v: Wide { a: n, b: 2, c: 3, d: 4, e: 5, f: 6 } } };
    println(w2.take().take().f);
    // Negative controls — single-level instantiations keep their symbols.
    let p1: Box[Wide] = Box { v: Wide { a: n, b: 2, c: 3, d: 4, e: 5, f: 9 } };
    println(p1.take().f);
    let p2: Box[i64] = Box { v: 5i64 };
    println(p2.take());
}
"#;
    assert_eq!(run_program(src).as_deref(), Some("41\n7\nhi\n6\n9\n5\n"));
}

/// B-2026-09-03-16 — A GENERIC PARENT'S PROJECTION DESTRUCTURE MUST HAND THE
/// ELEMENT'S `Drop` BODY TO THE LEAF, NOT LEAVE IT WITH THE SOURCE.
///
/// `struct G[T] { pe: (T, i64) }; let (r, k) = h.pe;` ran the element's body at
/// `h`'s NLL death on the three compiled surfaces and at the leaf's under
/// `--interp`. ONE body on both sides, on a LIVE value on both sides — a
/// PLACEMENT split with no husk, which is why a body COUNT cannot see it and
/// `marker` below is the cell that identifies the owner: it extends the SOURCE's
/// live range past the read (`z{h.z}`), so a body belonging to `h` prints AFTER
/// `z9` and a body belonging to the leaf prints before it.
///
/// `struct_field_type_exprs` is keyed by the DECLARATION name and stores fields
/// as written, so `G`'s element read back as the bare `T` however `G` was
/// instantiated; `T` names neither an enum nor a struct, so the leaf arm
/// declined and took neither body nor memory. The same name-keyed lookup fed the
/// LLVM layout, where the placeholder width made a load read the element back
/// with its first word right and everything past it zero (`dR7//0`).
/// `nongeneric` is the twin that was correct throughout and is what put the
/// fault on the missing substitution rather than on the projection source.
///
/// Every `Drop` body renders `tag` and `xs.len()`, so a body running on a
/// cap-zeroed husk prints `dRnn//0` and is distinguishable from a correct one —
/// the whole family asserts COUNTS over a payload that cannot render empty, and
/// this class of defect hides in exactly that gap.
///
/// OVER-REACH CONTROLS. `nodrop` instantiates the same generic at a `Drop`-less
/// type and must stay silent; `bothelems` and `secondpos` place the parameter in
/// either position; `nested` goes a level down; `wildcard` discards the leaf;
/// `paramroot` is the non-generic by-value param. `rebind` moves the leaf on —
/// the shape that was a pre-existing DOUBLE FREE before this fix, not merely a
/// misplacement.
///
/// `genericfn` PINNED THE ONE SHAPE THIS FIX DECLINED. A by-value param of a
/// GENERIC function is emitted by `compile_generic_call`, which populated
/// neither `current_fn_param_names` nor `owned_struct_params`, so the ownership
/// gate could not tell it from a local and the leaf would take a body the
/// caller's copy also runs. The substitution made that shape reachable for the
/// first time, so it was guarded rather than answered wrongly. B-2026-09-03-23
/// gave the monomorph body its own param-ownership identity and REMOVED that
/// guard; this cell is the pin that it did not move when it came off, and it
/// still holds at ONE body matching its non-generic twin.
///
/// Twin of `tests/interpreter.rs`'s
/// `test_generic_parent_projection_destructure_hands_the_body_to_the_leaf`,
/// pinned to the same string.
#[test]
fn e2e_generic_parent_projection_destructure_hands_the_body_to_the_leaf() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}/{self.xs.len()}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] }; }
struct G[T]  { pe: (T, i64), z: i64 }
struct Gn    { pe: (R, i64), z: i64 }
struct P2[A, B] { pe: (A, B), z: i64 }
struct Mix[T] { pe: (i64, T), z: i64 }
struct Nest[T] { pe: ((T, i64), i64), z: i64 }
struct Plain { a: i64, b: i64 }

fn n1() { let h = G[R] { pe: (mk(41), 0), z: 9 }; let (r, k) = h.pe; println(f"  b{r.id}/{r.tag}/{r.xs.len()}") }
fn n2() { let h = G[R] { pe: (mk(42), 0), z: 9 }; let (r, k) = h.pe; println(f"  b{r.id}/{r.tag}"); println(f"  z{h.z}") }
fn n3() { let h = Gn   { pe: (mk(43), 0), z: 9 }; let (r, k) = h.pe; println(f"  b{r.id}/{r.tag}") }
fn n4() { let h = G[R] { pe: (mk(44), 0), z: 9 }; let (r, k) = h.pe; let m = r; println(f"  b{m.id}/{m.tag}/{m.xs.len()}") }
fn n5() { let h = P2[R, R] { pe: (mk(45), mk(95)), z: 9 }; let (a, b) = h.pe; println(f"  b{a.id}/{b.id}") }
fn n6() { let h = Mix[R] { pe: (7, mk(46)), z: 9 }; let (a, r) = h.pe; println(f"  b{a}/{r.id}") }
fn n7() { let h = G[Plain] { pe: (Plain { a: 1, b: 2 }, 0), z: 9 }; let (p, k) = h.pe; println(f"  b{p.a}/{p.b}") }
fn n8() { let h = G[R] { pe: (mk(48), 0), z: 9 }; let (_, k) = h.pe; println(f"  b{k}") }
fn n9() { let h = Nest[R] { pe: ((mk(49), 1), 2), z: 9 }; let ((r, a), b) = h.pe; println(f"  b{r.id}/{a}/{b}") }
fn n10[T](h: G[T]) -> i64 { let (r, k) = h.pe; println("  in"); return k; }
fn n11() { let h = G[R] { pe: (mk(51), 5), z: 9 }; let k = n10(h); println(f"  b{k}") }
fn n12(h: Gn) { let (r, k) = h.pe; println(f"  b{r.id}/{r.tag}") }

fn main() {
    println("generic");    n1();                             println("generic end")
    println("marker");     n2();                             println("marker end")
    println("nongeneric"); n3();                             println("nongeneric end")
    println("rebind");     n4();                             println("rebind end")
    println("bothelems");  n5();                             println("bothelems end")
    println("secondpos");  n6();                             println("secondpos end")
    println("nodrop");     n7();                             println("nodrop end")
    println("wildcard");   n8();                             println("wildcard end")
    println("nested");     n9();                             println("nested end")
    println("genericfn");  n11();                            println("genericfn end")
    println("paramroot");  n12(Gn { pe: (mk(52), 0), z: 9 }); println("paramroot end")
    println("done")
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"generic
  b41/t41/1
dR41/t41/1
generic end
marker
  b42/t42
dR42/t42/1
  z9
marker end
nongeneric
  b43/t43
dR43/t43/1
nongeneric end
rebind
  b44/t44/1
dR44/t44/1
rebind end
bothelems
  b45/95
dR95/t95/1
dR45/t45/1
bothelems end
secondpos
  b7/46
dR46/t46/1
secondpos end
nodrop
  b1/2
nodrop end
wildcard
dR48/t48/1
  b0
wildcard end
nested
  b49/1/2
dR49/t49/1
nested end
genericfn
  in
dR51/t51/1
  b5
genericfn end
paramroot
  b52/t52
dR52/t52/1
paramroot end
done
"#
    );
}

/// B-2026-09-20-52 — A CONCRETE HOLDER'S ERASED ENUM BOX HAD NO OWNER, AND
/// A NON-GENERIC CALLEE'S HAND-BACK HAD TWO. Two independent faults, one
/// row, and each is visible in a program the other does not appear in.
///
/// `holder` is the first: `struct Conc { g: G1[String] }` never freed its
/// field's box. `synth_drop.rs` pre-filtered the boxed-field set with
/// `type_expr_mentions_param`, on a comment asserting that a concrete
/// holder "has no erasure to repair ... the existing switch frees the box".
/// It does not: `emit_enum_drop_switch` is keyed by enum NAME and its arms
/// come from the DECLARATION, where `G1[T]`'s payload takes
/// `enum_drop_kind_for_type_expr`'s `_ => None` tail — the same erasure as
/// the generic case, because it is the same one drop function. Measured on
/// a program containing nothing else: 9 allocs / 8 frees, 24 bytes
/// definitely lost with ZERO indirect, fixed at 24 across 8-, 40- and
/// 100-character payloads, so what is lost is the ENVELOPE. `nongen` is the
/// same struct over a non-generic enum and was 8/8 clean throughout, which
/// is what says the fault is the erasure and not the struct.
///
/// `identbind` is the second, and it needs no struct at all:
/// `fn ident(g: G1[String]) -> G1[String] { return g }` over
/// `let g = G1.Y("…"); let k = ident(g);` was 9 allocs / 10 frees with an
/// `Invalid read of size 8` and an `Invalid free()`, BOTH in `main` — the
/// caller's binding and the result binding freeing one box. The emitted IR
/// says it plainly: the caller's disarm store is present before
/// `call @consume`, `call @other` and `call @usz`, and simply absent before
/// `call @ident`. `mono.rs` has carried a runtime-compare disarm for this
/// since B-2026-09-17-7 and `compile_generic_call` was the only site that
/// ever built its `maybe_handed_back_args`, so a NON-GENERIC callee with a
/// concrete parameter type reached no disarm at all.
///
/// THE TWO CANCEL, WHICH IS WHY NEITHER WAS VISIBLE IN THE SHAPE THAT
/// CONTAINS BOTH. `wrapbind` — a named local wrapped into a concrete holder
/// — was BALANCED before either fix: the caller over-freed and the holder
/// under-freed, over one box. Fixing the holder alone turns that cell and
/// four more into double frees, and fixing the caller alone leaves them
/// leaking; a grid of only such cells reports both faults as clean.
///
/// `wrapnone` IS THE DIES-INSIDE LEG AND IT IS THE GUARD THAT REJECTED THE
/// OBVIOUS FIX. `wrapC(g, false)` returns a payload-free variant, so
/// nothing else owns the argument's box. A static disarm gated on the
/// callee's signature took this cell from clean to a 24-byte definite loss
/// while fixing every hand-back cell, and swapping that gate to the
/// ALL-paths predicate changed neither number. Asking the returned VALUE
/// instead gives the two legs of one callee opposite answers from one emit.
///
/// `identdisc` is why the mono twin's discarded-statement guard is NOT
/// ported: there a discarded generic call hands the box to nobody, while
/// here `ident(g);` as a statement is the same 9/10 double free as the
/// bound spelling. The divergence is measured, not assumed.
///
/// `handon`, `identtmp`, `fresh`, `scalar`, `unit` and `twoargs` are the
/// guards in the stranding direction — a field handed onward, a TEMPORARY
/// argument (clean throughout, because it has no binding to be the second
/// owner, which is also why it cannot stand in for `identbind`), a callee
/// returning a FRESH value, a scalar return, a unit return, and a two-
/// argument callee where only the returned one may be disarmed.
///
/// Valgrind on this program, `-O0` / `KARAC_AUTO_PAR=0`: 39 allocs / 39
/// frees, `ERROR SUMMARY: 0 errors`. All four surfaces agree on the output.
///
/// The MEMORY twin is `tests/memory_sanitizer.rs`'s
/// `asan_concrete_holder_and_non_generic_handback_leave_one_owner_per_box`.
#[test]
fn e2e_concrete_holder_and_non_generic_handback_leave_one_owner_per_box() {
    let Some(out) = run_program(
        r#"enum G1[T] { Y(T), N }
enum N1 { Y(String), N }
struct Conc { g: G1[String] }
fn wrapC(g: G1[String], c: bool) -> Conc { if c { return Conc { g: g } } return Conc { g: G1.N } }
fn ident(g: G1[String]) -> G1[String] { return g }
fn other(g: G1[String]) -> G1[String] { return G1.Y("zzzzzzzzzzzzzzzzzzzzzzzz") }
fn pick(a: G1[String], b: G1[String]) -> G1[String] { return a }
fn usz(g: G1[String]) -> i64 { match g { G1.Y(v) => { return v.len() } G1.N => { return 0 } } }
fn sink(g: G1[String]) { match g { G1.Y(v) => { println(f"  s{v.len()}") } G1.N => { println("  s0") } } }
fn shw(g: G1[String]) { match g { G1.Y(v) => { println(f"  mx {v.len()}") } G1.N => { println("  mx 0") } } }
fn identn(g: N1) -> N1 { return g }
fn shwn(g: N1) { match g { N1.Y(v) => { println(f"  nx {v.len()}") } N1.N => { println("  nx 0") } } }

fn main() {
    println("holder");    { let h: Conc = Conc { g: G1.Y("abcdefghijklmnopqrstuvwx") }; println("  x") }
    println("handon");    { let h: Conc = Conc { g: G1.Y("abcdefghijklmnopqrstuvwx") }; shw(h.g) }
    println("wrapbind");  { let g: G1[String] = G1.Y("abcdefghijklmnopqrstuvwx"); let h = wrapC(g, true); shw(h.g) }
    println("wrapnone");  { let g: G1[String] = G1.Y("abcdefghijklmnopqrstuvwx"); let h = wrapC(g, false); shw(h.g) }
    println("wrapdisc");  { let g: G1[String] = G1.Y("abcdefghijklmnopqrstuvwx"); wrapC(g, true); println("  x") }
    println("identbind"); { let g: G1[String] = G1.Y("abcdefghijklmnopqrstuvwx"); let k: G1[String] = ident(g); shw(k) }
    println("identdisc"); { let g: G1[String] = G1.Y("abcdefghijklmnopqrstuvwx"); ident(g); println("  x") }
    println("identtmp");  { let k: G1[String] = ident(G1.Y("abcdefghijklmnopqrstuvwx")); shw(k) }
    println("fresh");     { let g: G1[String] = G1.Y("abcdefghijklmnopqrstuvwx"); let k: G1[String] = other(g); shw(k) }
    println("scalar");    { let g: G1[String] = G1.Y("abcdefghijklmnopqrstuvwx"); let n: i64 = usz(g); println(f"  n{n}") }
    println("unit");      { let g: G1[String] = G1.Y("abcdefghijklmnopqrstuvwx"); sink(g) }
    println("twoargs");   { let x: G1[String] = G1.Y("aaaaaaaaaaaaaaaaaaaaaaaa"); let y: G1[String] = G1.Y("bbbbbbbbbbbbbbbbbbbbbbbbbbbb"); let k: G1[String] = pick(x, y); shw(k) }
    println("nongen");    { let g: N1 = N1.Y("abcdefghijklmnopqrstuvwx"); let k: N1 = identn(g); shwn(k) }
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "holder\n  x\nhandon\n  mx 24\nwrapbind\n  mx 24\nwrapnone\n  mx 0\nwrapdisc\n  x\nidentbind\n  mx 24\nidentdisc\n  x\nidenttmp\n  mx 24\nfresh\n  mx 24\nscalar\n  n24\nunit\n  s24\ntwoargs\n  mx 24\nnongen\n  nx 24\nend\n");
}

/// B-2026-09-20-46 — A GENERIC CALLEE NEVER REACHED B-2026-09-20-13'S
/// ARGUMENT-SITE COPY, so a reused by-value generic enum argument was a
/// use-after-free where its concrete twin was correct.
///
/// `compile_call` reaches that copy through
/// `move_declined_copy_struct_arg_for`, unconditionally for every by-value
/// argument. `compile_generic_call` reached it only at its retraction loop
/// and only when `transfer_ident[i]` held — keyed on
/// `var_types.var_type_names[var]`, a STRUCT name — so an ENUM argument
/// never qualified. Measured on the pre-fix tree: the generic module
/// carried ZERO `b13.*` labels of any kind against fourteen distinct ones
/// in a twin differing only in the callee's signature, and the second call
/// read a null box — `Invalid read of size 8`, `Address 0x0`, SIGSEGV at
/// -O0 and under the JIT, `mx NONE` plus a runtime stack overflow at -O2.
///
/// THE `gen` / `con` PAIR IS THE WHOLE ROW and differs in ONE line, the
/// callee's signature. Keeping the concrete cell beside the generic one is
/// what makes a future regression legible as "the two paths disagree"
/// rather than as a bare wrong answer.
///
/// `one` is the same generic callee with NO reuse, and it guards the other
/// direction: the copy's gate is `source_outlives_move == UseAfterMove`, so
/// a single call must emit no copy at all. `i32` guards it again on a
/// NON-boxing payload, which has nothing to copy — measured clean before
/// the fix as well as after, which is why it is a guard and not a
/// regression cell. `none` takes the payload-less variant on the same call.
///
/// `twodist` — `two(x, y)` over two DISTINCT reused bindings — is the cell
/// that earned its place the hard way. The first version of this fix
/// emitted the copy per argument and produced an INVALID MODULE
/// (`Instruction does not dominate all uses!`), and the cause was not the
/// two copies: `compile_generic_call` emits the monomorph's BODY INLINE, so
/// the callee's own statements reach `compile_stmt`'s restore drain while
/// the CALLER's restore is still queued, and the caller's store was emitted
/// inside the callee. Every one-parameter cell was clean because a
/// single-`match`-expression body never reaches that drain. The queued
/// restores are now keyed to their owning function. A fixture of only
/// one-argument calls cannot see any of this.
///
/// NOT COVERED HERE, deliberately: a generic METHOD receiver
/// (`impl[T] G1[T] { fn shm(self) }` with `g.shm(); g.shm()`), which this
/// fix moves from a double free to a 24 B leak — better, not clean — and a
/// leaking cell would redden the ASAN ratchet. It has its own row. Also not
/// here: one binding passed twice in a single call (`two(g, g)`), which is
/// broken on the concrete path too and is filed separately.
#[test]
fn e2e_generic_callee_reaches_the_argument_site_copy_like_its_concrete_twin() {
    let Some(out) = run_program(
        r#"enum G1[T] { Y(T), N }
enum N1 { Y(String), N }
fn shg[T](g: G1[T]) { match g { G1.Y(v) => { println(f"  mx {v}") } G1.N => { println("  mx NONE") } } }
fn shc(g: G1[String]) { match g { G1.Y(v) => { println(f"  cx {v}") } G1.N => { println("  cx NONE") } } }
fn shn[T](g: G1[T]) -> i64 { match g { G1.Y(v) => { return 1 } G1.N => { return 0 } } }
fn shnn(g: N1) { match g { N1.Y(v) => { println(f"  nx {v}") } N1.N => { println("  nx NONE") } } }
fn two[T](a: G1[T], b: G1[T]) { match a { G1.Y(v) => { println(f"  ax {v}") } G1.N => { println("  ax NONE") } } match b { G1.Y(v) => { println(f"  bx {v}") } G1.N => { println("  bx NONE") } } }
fn main() {
    println("gen");    { let g: G1[String] = G1.Y(f"pa"); shg(g); shg(g) }
    println("con");    { let g: G1[String] = G1.Y(f"pa"); shc(g); shc(g) }
    println("one");    { let g: G1[String] = G1.Y(f"pa"); shg(g) }
    println("thrice"); { let g: G1[String] = G1.Y(f"pa"); shg(g); shg(g); shg(g) }
    println("i32");    { let g: G1[i32] = G1.Y(24); shg(g); shg(g) }
    println("none");   { let g: G1[String] = G1.N; shg(g); shg(g) }
    println("nongen"); { let g: N1 = N1.Y(f"pa"); shnn(g) }
    println("twodist");{ let x: G1[String] = G1.Y(f"xa"); let y: G1[String] = G1.Y(f"yb"); two(x, y); shg(x); shg(y) }
    println("scalar"); { let g: G1[String] = G1.Y(f"pa"); let n: i64 = shn(g); shg(g); println(f"  n{n}") }
    println("long");   { let g: G1[String] = G1.Y(f"abcdefghijklmnopqrstuvwx"); shg(g); shg(g) }
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "gen\n  mx pa\n  mx pa\ncon\n  cx pa\n  cx pa\none\n  mx pa\nthrice\n  mx pa\n  mx pa\n  mx pa\ni32\n  mx 24\n  mx 24\nnone\n  mx NONE\n  mx NONE\nnongen\n  nx pa\ntwodist\n  ax xa\n  bx yb\n  mx xa\n  mx yb\nscalar\n  mx pa\n  n1\nlong\n  mx abcdefghijklmnopqrstuvwx\n  mx abcdefghijklmnopqrstuvwx\nend\n");
}

/// B-2026-08-30-41 — A `T: PartialOrd` BOUND MUST BE RUNNABLE, FOR EVERY TYPE.
///
/// The bound was SATISFIABLE by every scalar (a structural rule in `env.rs`,
/// with no impl behind it) and runnable by none: lowering turns `a < b` under
/// it into `T.partial_cmp(a, b).is_lt()`, and no primitive had `partial_cmp`.
/// The tree-walk aborted with "method 'partial_cmp' not found on type 'i64'"
/// and `karac build` failed on the FOLLOW-ON predicate with "no handler for
/// method 'is_lt'". The identical body under a `T: Ord` bound worked on both,
/// which is the contrast that makes this a bound defect rather than a
/// comparison one.
///
/// `cmp` IS NOT AN ALTERNATIVE, and that is forced rather than preferred:
/// supertrait methods do not re-export through the requiring trait, so a
/// `T: PartialOrd` bound cannot call `cmp` at all — and routing to it would
/// fail for exactly the population the trait exists to serve, the bare floats,
/// which are `PartialOrd` and deliberately not `Ord`.
///
/// EVERY ROW IS A DIFFERENT DISPATCH PATH, not a restatement:
/// - `ord` — the `T: Ord` control, which worked before and must keep working.
/// - `i64` — all four operators, so a fix that wired only `is_lt` is caught.
/// - `u64` — an unsigned value above `i64::MAX` riding a signed carrier.
///   `partial_cmp` recovers signedness through the same span-hint path `cmp`
///   uses (B-2026-08-28-5); without it `u64::MAX < 1` answers `true`.
/// - `f64` — the motivating case: `PartialOrd` and NOT `Ord`, so `T: Ord`
///   rejects it outright and this is the only bound that admits it.
/// - `str` — a heap receiver, through `karac_string_cmp`.
/// - `user` — a struct deriving `PartialOrd`, through the aggregate comparator.
/// - `nan` — the reason the trait returns an `Option` at all. All four
///   predicates are `false` for an incomparable pair, per design.md
///   § Comparison Traits and IEEE-754.
///
/// Twin of `tests/interpreter.rs`'s `test_partial_ord_bound_is_runnable`,
/// pinned to the same string. Both sides are asserted because the fix has a
/// half in each backend plus a shared typechecker registration — comparing
/// only one would miss a half.
#[test]
fn e2e_partial_ord_bound_is_runnable() {
    let Some(out) = run_program(
        r#"#[derive(PartialEq, PartialOrd)]
struct Po { v: i64 }

fn lt_ord[T: Ord](a: T, b: T) -> bool { return a < b }
fn lt_po[T: PartialOrd](a: T, b: T) -> bool { return a < b }
fn le_po[T: PartialOrd](a: T, b: T) -> bool { return a <= b }
fn gt_po[T: PartialOrd](a: T, b: T) -> bool { return a > b }
fn ge_po[T: PartialOrd](a: T, b: T) -> bool { return a >= b }

fn main() {
    let n = env.args().len() as i64;
    println(f"ord  {lt_ord(n, n + 1)} {lt_ord(f"a", f"b")}");
    println(f"i64  {lt_po(n, n + 1)} {le_po(n, n)} {gt_po(n + 1, n)} {ge_po(n, n + 1)}");
    let u: u64 = 18446744073709551615u64;
    println(f"u64  {lt_po(u, 1u64)} {gt_po(u, 1u64)}");
    let f: f64 = (n as f64) + 1.0;
    println(f"f64  {lt_po(f, f + 1.0)} {ge_po(f, f)}");
    println(f"str  {lt_po(f"a", f"b")} {gt_po(f"a", f"b")}");
    println(f"user {lt_po(Po { v: n }, Po { v: n + 1 })} {ge_po(Po { v: n }, Po { v: n })}");
    let z: f64 = (n as f64) - 1.0;
    let nan: f64 = z / z;
    println(f"nan  {lt_po(nan, f)} {le_po(nan, nan)} {gt_po(f, nan)} {ge_po(nan, nan)}");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"ord  true true
i64  true true true false
u64  false true
f64  true true
str  true false
user true true
nan  false false false false
"#
    );
}

/// A method call whose receiver is an unnamed TEMPORARY inside a generic
/// impl (B-2026-08-25-28). `enum_inst_var_types` is NAME-keyed, so a
/// temporary has no key and no binding-site arm can reach it — this is the
/// case the `let`-site family's six arms structurally could not close.
/// Resolved on the receiver side instead: a struct literal reconstructs its
/// args from the struct's declared params, and a call resolves its declared
/// (generic) return type through the active monomorph's substitution.
///
/// The named-binding control was correct before this fix, so it pins the
/// diagnosis: the defect was the ABSENCE of a name, not the value.
#[test]
fn e2e_generic_method_call_on_temporary_receiver() {
    let receivers: &[(&str, &str)] = &[
        ("struct literal", "Bag { xs: v }.inner()"),
        ("free-fn call", "mkbag(v).inner()"),
        ("assoc-fn call", "Bag.make(v).inner()"),
        ("chained temporaries", "Bag { xs: v }.dup().inner()"),
        (
            "named binding (control)",
            "{ let q = Bag { xs: v }; q.inner() }",
        ),
    ];
    for (name, recv) in receivers {
        let src = format!(
                "struct Bag[=T] {{ xs: Vec[T] }}\n\
                 impl[T: Ord] Bag[T] {{\n    \
                     fn swap2(mut ref self, i: i64, j: i64) {{ self.xs.swap(i, j); }}\n    \
                     fn arrange(mut ref self) {{ let n = self.xs.len(); if n > 1 {{ self.swap2(0, n - 1); }} }}\n    \
                     fn dup(ref self) -> Bag[T] {{ Bag {{ xs: self.xs }} }}\n    \
                     fn make(v: Vec[T]) -> Bag[T] {{ Bag {{ xs: v }} }}\n    \
                     fn inner(self) -> Vec[T] {{ let mut b = self; b.arrange(); b.xs }}\n    \
                     fn mk(v: Vec[T]) -> Vec[T] {{ {recv} }}\n\
                 }}\n\
                 fn mkbag[T: Ord](v: Vec[T]) -> Bag[T] {{ Bag {{ xs: v }} }}\n\
                 fn main() {{\n    \
                     let a = Bag.mk([\"x\", \"y\", \"z\"]); println(a[0]);\n    \
                     let b = Bag.mk([1, 2, 3]); println(b[0]);\n\
                 }}\n"
            );
        assert_eq!(
            run_program(&src).as_deref(),
            Some("z\n3\n"),
            "temporary-receiver form `{name}` did not round-trip at both T = String and T = i64",
        );
    }
}

/// The `let`-site instantiation family, swept across every RHS form that
/// can bind a generic-struct value inside a monomorph. Each of these
/// SEGFAULTED at a heap-carrying `T` before the span-resolution arm landed
/// (audit of B-2026-08-25-25): the span record is pre-monomorphization, so
/// it holds the GENERIC form (`Bag[T]`), which is worse than holding
/// nothing — it satisfies the chain and stops the search, then the sibling
/// call site cannot bind concrete params from it.
///
/// Every case runs at BOTH `i64` and `String`. The scalar leg is not
/// decoration: at `T = i64` all of these were silently CORRECT, because the
/// base prototype's layout happens to match, so a scalar-only test reports
/// green on every one of them.
#[test]
fn e2e_generic_let_binding_instantiation_across_rhs_forms() {
    // (form-name, impl-extra, free-fn-extra)
    let forms: &[(&str, &str, &str)] = &[
            ("identifier copy",
             "fn mk(v: Vec[T]) -> Vec[T] { let a = Bag { xs: v }; let mut c = a; c.arrange(); c.xs }", ""),
            ("free-fn call",
             "fn mk(v: Vec[T]) -> Vec[T] { let mut b = mkbag(v); b.arrange(); b.xs }",
             "fn mkbag[T: Ord](v: Vec[T]) -> Bag[T] { Bag { xs: v } }"),
            ("assoc-fn returning Self",
             "fn make(v: Vec[T]) -> Bag[T] { Bag { xs: v } }\n    \
              fn mk(v: Vec[T]) -> Vec[T] { let mut b = Bag.make(v); b.arrange(); b.xs }", ""),
            ("method returning Self",
             "fn dup(ref self) -> Bag[T] { Bag { xs: self.xs } }\n    \
              fn mk(v: Vec[T]) -> Vec[T] { let q = Bag { xs: v }; let mut b = q.dup(); b.arrange(); b.xs }", ""),
            ("block expression",
             "fn mk(v: Vec[T]) -> Vec[T] { let mut b = { Bag { xs: v } }; b.arrange(); b.xs }", ""),
            // `bs[0]` reads a non-`Copy` element out of a container, so it is
            // spelled `.clone()` now (B-2026-08-26-21) and `Bag` derives
            // `Clone` above for it. Kept rather than dropped: an index RHS
            // resolves its span differently from the other six forms, which is
            // the entire point of sweeping them.
            ("index into Vec[Bag[T]]",
             "fn mk(v: Vec[T]) -> Vec[T] { let bs = [Bag { xs: v }]; let mut b = bs[0].clone(); b.arrange(); b.xs }", ""),
            ("named binding, then consuming method",
             "fn inner(self) -> Vec[T] { let mut b = self; b.arrange(); b.xs }\n    \
              fn mk(v: Vec[T]) -> Vec[T] { let q = Bag { xs: v }; q.inner() }", ""),
        ];
    for (name, impl_extra, free_extra) in forms {
        let src = format!(
                "#[derive(Clone)]\n\
                 struct Bag[=T] {{ xs: Vec[T] }}\n\
                 impl[T: Ord] Bag[T] {{\n    \
                     fn swap2(mut ref self, i: i64, j: i64) {{ self.xs.swap(i, j); }}\n    \
                     fn arrange(mut ref self) {{ let n = self.xs.len(); if n > 1 {{ self.swap2(0, n - 1); }} }}\n    \
                     {impl_extra}\n\
                 }}\n\
                 {free_extra}\n\
                 fn main() {{\n    \
                     let a = Bag.mk([\"x\", \"y\", \"z\"]); println(a[0]);\n    \
                     let b = Bag.mk([1, 2, 3]); println(b[0]);\n\
                 }}\n"
            );
        assert_eq!(
            run_program(&src).as_deref(),
            Some("z\n3\n"),
            "let-binding RHS form `{name}` did not round-trip at both T = String and T = i64",
        );
    }
}

/// The NON-`let` binding sites, swept the same way the `let`-RHS forms were
/// above. B-2026-08-25-27/-28 cleared the `let` sites and the temporary
/// receiver but left these three unverified, and the probe that was
/// supposed to clear them could not detect the defect even on a
/// known-broken control — so "ok" meant nothing (B-2026-08-27-36).
///
/// The MATCH arm was genuinely broken. `pattern_binding_inner_types` is a
/// PRE-monomorphization record, so a payload bound inside a generic impl
/// held the GENERIC form (`Bag[T]`), and the payload-binding site inserted
/// that straight into `enum_inst_var_types` — the FIRST and authoritative
/// arm of `enum_inst_type_of_expr`. A generic record there is worse than no
/// record: it satisfies the chain and stops the search before the span
/// fallback, which is the one arm that resolves through the substitution.
/// So `Some(b) => b.inner()` dispatched to the UNMANGLED base prototype and
/// SEGFAULTED at `T = String`, while the for-loop binding — which falls
/// through to that span fallback — was already correct.
///
/// Both legs run at BOTH `i64` and `String`. The scalar leg is not
/// decoration: at `T = i64` the match arm was silently CORRECT, because the
/// base prototype's layout happens to match.
#[test]
fn e2e_generic_non_let_binding_instantiation_across_sites() {
    let sites: &[(&str, &str)] = &[
        (
            "for-loop binding",
            "fn mk(v: Vec[T]) -> Vec[T] { let bags = [Bag { xs: v }]; \
              let mut out: Vec[T] = Vec.new(); for b in bags { out = b.inner(); } out }",
        ),
        (
            "match pattern binding",
            "fn mk(v: Vec[T]) -> Vec[T] { let o = Some(Bag { xs: v }); \
              match o { Some(b) => { b.inner() } None => { Vec.new() } } }",
        ),
    ];
    for (name, mk) in sites {
        let src = format!(
                "struct Bag[=T] {{ xs: Vec[T] }}\n\
                 impl[T: Ord] Bag[T] {{\n    \
                     fn swap2(mut ref self, i: i64, j: i64) {{ self.xs.swap(i, j); }}\n    \
                     fn arrange(mut ref self) {{ let n = self.xs.len(); if n > 1 {{ self.swap2(0, n - 1); }} }}\n    \
                     fn inner(self) -> Vec[T] {{ let mut b = self; b.arrange(); b.xs }}\n    \
                     {mk}\n\
                 }}\n\
                 fn main() {{\n    \
                     let a = Bag.mk([\"x\", \"y\", \"z\"]); println(a[0]);\n    \
                     let b = Bag.mk([1, 2, 3]); println(b[0]);\n\
                 }}\n"
            );
        assert_eq!(
            run_program(&src).as_deref(),
            Some("z\n3\n"),
            "non-`let` binding site `{name}` did not round-trip at both T = String and T = i64",
        );
    }
}

/// B-2026-08-27-37 — the run-vs-build half of that double free. A by-value
/// TUPLE param of a MONOMORPH got no entry-copy and no scope-exit drop
/// (`compile_mono_function`'s owned-param arm was gated `TypeKind::Path(_)`),
/// while the caller registered its tuple temp's drop regardless. Both sides
/// aliased one buffer, so moving the struct's heap field out handed the same
/// `Vec` to the result and still left the caller's temp to free it.
///
/// Runs at BOTH element types: this fired at `T = i64` too, which is what
/// separates it from the wrong-monomorph family where the scalar leg is
/// silently correct. `tests/memory_sanitizer.rs` carries the ASAN twin.
#[test]
fn e2e_generic_struct_destructured_from_a_tuple_param_frees_once() {
    let src = "struct Bag[T] { xs: Vec[T] }\n\
                   fn take[T](p: (Bag[T], i64)) -> Vec[T] { let (b, _n) = p; b.xs }\n\
                   fn main() {\n    \
                       let a = take((Bag { xs: [\"x\", \"y\"] }, 0)); println(f\"{a.len()} {a[0]}\");\n    \
                       let b = take((Bag { xs: [10, 20] }, 0)); println(f\"{b.len()} {b[0]}\");\n\
                   }\n";
    assert_eq!(run_program(src).as_deref(), Some("2 x\n2 10\n"));
}

/// A GENERIC struct leaf destructured out of a tuple runs its Drop-bearing
/// field's body (B-2026-08-28-11).
///
/// `let (b, n) = (Box2[R] { v: R { id: 41 } }, 1);` ran the body under the
/// interpreter and on NEITHER compiled backend, uniformly across all three
/// sources — local, call result and tuple literal — which is what pointed at
/// the SUBSTITUTION rather than at reachability. The non-generic control in
/// the same position was always correct.
///
/// THE GENERIC ARGUMENTS WERE ERASED THREE TIMES OVER, each fix necessary
/// and none sufficient:
///  * `infer_arg_elem_te` rebuilt every element's type from its NAME, so the
///    literal's own `Box2[R]` became a bare `Box2` before anything recorded
///    it;
///  * `tuple_var_elem_tes` then preferred that name spelling over the
///    recorded `TypeExpr`, because a name is "informative" and the erasure
///    is invisible — `Box2` meaning `Box2[R]` and `Box2` meaning nothing in
///    particular are the same string;
///  * and the destructure leaf recorded no instantiation at all, so even a
///    correct element type never reached the leaf's own lookup.
///
/// `two-params` and `used-leaf` guard the substitution's shape rather than
/// its presence. `non-drop-param` is the control that must stay SILENT: a
/// fix that recorded any instantiation at all, rather than the right one,
/// would run a body here where the interpreter runs none.
///
/// The GENERIC-CALLEE spelling — `fn src[T](x: T) -> (Box2[T], i64)` — is
/// deliberately absent and filed separately: the element type there is a
/// bare `Box2[T]` read from the callee's declaration, and binding `T` needs
/// the call site's substitution, which is the B-2026-08-28-28 machinery
/// rather than this one. Recording `Box2[T]` instead is not a partial
/// answer but a harmful one — see the commit for the over-free it caused.
#[test]
fn e2e_generic_tuple_destructure_leaf_runs_its_field_body() {
    const H: &str = "struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id}\") } }\n\
             struct Box2[T] { v: T }\n";
    for (label, body, want) in [
        // The row's three sources, which failed identically.
        (
            "local-source",
            "fn main() { let p = (Box2[R] { v: R { id: 41 } }, 1); let (b, n) = p;\n\
                 \x20            println(f\"{n}\"); }\n",
            "drop 41\n1\n",
        ),
        (
            "call-source",
            "fn make() -> (Box2[R], i64) { return (Box2[R] { v: R { id: 42 } }, 1); }\n\
                 fn main() { let (b, n) = make(); println(f\"{n}\"); }\n",
            "drop 42\n1\n",
        ),
        (
            "literal-source",
            "fn main() { let (b, n) = (Box2[R] { v: R { id: 43 } }, 1); println(f\"{n}\"); }\n",
            "drop 43\n1\n",
        ),
        // The leaf is READ, so the body lands after the read.
        (
            "used-leaf",
            "fn main() { let (b, n) = (Box2[R] { v: R { id: 44 } }, 1);\n\
                 \x20            println(f\"{b.v.id}\"); println(f\"{n}\"); }\n",
            "44\ndrop 44\n1\n",
        ),
        // TWO parameters, only one of them Drop-bearing.
        (
            "two-params",
            "struct Pair2[A, B] { a: A, b: B }\n\
                 fn main() { let (b, n) = (Pair2[R, i64] { a: R { id: 45 }, b: 7 }, 1);\n\
                 \x20            println(f\"{n}\"); }\n",
            "drop 45\n1\n",
        ),
        // CONTROL — the parameter carries no `Drop`, so nothing may run. A
        // fix that records ANY instantiation rather than the right one
        // fails here.
        (
            "non-drop-param",
            "fn main() { let (b, n) = (Box2[i64] { v: 9 }, 1); println(f\"{n}\"); }\n",
            "1\n",
        ),
        // CONTROL — the non-generic sibling, correct throughout.
        (
            "non-generic-control",
            "struct W { r: R }\n\
                 fn main() { let (w, n) = (W { r: R { id: 48 } }, 1); println(f\"{n}\"); }\n",
            "drop 48\n1\n",
        ),
    ] {
        let prog = format!("{H}{body}");
        assert_eq!(run_program(&prog).as_deref(), Some(want), "{label}");
    }
}

/// A GENERIC CALLEE's tuple element runs its Drop-bearing field's body at a
/// call-site destructure (B-2026-08-28-61).
///
/// B-2026-08-28-11 fixed the three sources that spell a CONCRETE `Box2[R]`.
/// A generic wrapper declares `-> (Box2[T], i64)`, so the element type read
/// from that declaration is a bare `Box2[T]` and `T` is bound only at the
/// call site — the leaf recorded nothing and the body was silent on both
/// compiled backends.
///
/// Recording `Box2[T]` instead is NOT a partial answer, which is why -11
/// filed this rather than reaching for it: such a record satisfies the
/// instantiation lookup and stops it before the arm that would resolve `T`,
/// and while landing -11 that turned a missing body into an OVER-FREE of the
/// caller's buffer. The fix substitutes at the call site instead, through the
/// same `call_type_subs` table B-2026-08-28-28 used to name a generic
/// callee's element — one level deeper, inside the element's own arguments.
///
/// `scalar-param` and `other-drop-type` are what make that substitution
/// per-call-site rather than per-callee: one `src[T]` is called at three
/// instantiations in one program, and each leaf must answer for its own.
///
/// The NESTED-generic spelling (`fn outer[U](y: U) -> (Box2[U], i64) {
/// src(y) }`) is deliberately absent. It runs the body TWICE — but so does
/// its fully non-generic twin, on both backends and before this fix, because
/// a param escaping through a CALL in return position is a route
/// `fn_returns_param` does not model. That is B-2026-08-28-62, an older and
/// separate defect; this fix simply brings the generic spelling into line
/// with the non-generic one rather than inheriting an accidental silence.
#[test]
fn e2e_generic_callee_tuple_leaf_runs_its_field_body() {
    const H: &str = "struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id}\") } }\n\
             struct S { sid: i64 }\n\
             impl Drop for S { fn drop(mut ref self) { println(f\"drop S{self.sid}\") } }\n\
             struct Box2[T] { v: T }\n\
             fn src[T](x: T) -> (Box2[T], i64) { return (Box2[T] { v: x }, 1); }\n";
    for (label, body, want) in [
        // The row's own repro.
        (
            "generic-callee",
            "fn main() { let (a, n) = src(R { id: 47 }); println(f\"{n}\"); }\n",
            "drop 47\n1\n",
        ),
        // CONTROL — the SAME callee at a scalar instantiation must stay
        // silent, which is what makes the substitution per-call-site.
        (
            "scalar-param",
            "fn main() { let (b, n) = src(9); println(f\"{n}\"); }\n",
            "1\n",
        ),
        // The same callee at a DIFFERENT Drop type, in the same program as
        // the first — neither leaf may be handed the other's answer.
        (
            "other-drop-type",
            "fn main() { let (a, n) = src(R { id: 47 }); println(f\"{n}\");\n\
                 \x20            let (c, m) = src(S { sid: 3 }); println(f\"{m}\"); }\n",
            "drop 47\n1\ndrop S3\n1\n",
        ),
        // The leaf is READ, so the body lands after the read.
        (
            "used-leaf",
            "fn main() { let (e, n) = src(R { id: 50 });\n\
                 \x20            println(f\"{e.v.id}\"); println(f\"{n}\"); }\n",
            "50\ndrop 50\n1\n",
        ),
    ] {
        let prog = format!("{H}{body}");
        assert_eq!(run_program(&prog).as_deref(), Some(want), "{label}");
    }
}

/// B-2026-08-27-50 — a generic callee's CONTAINER param bound to a
/// STRUCT-FIELD argument (`swap01(mut self.xs)`, `vlen(b.items)`).
///
/// Every codegen resolver that recovers a container argument's element
/// keys on the argument's BINDING NAME, and a field access has none, so
/// all of them declined and `T` fell to the `i64` unknown-name default:
/// an 8-byte swap over a 16-byte tuple element (a SILENT wrong answer —
/// `10:1 2:20` for `2:20 1:10`) and a SEGFAULT at a String element.
///
/// The precondition is narrower than "a field access", which is why the
/// last two cases are here: the TYPECHECKER's `call_type_subs` frame also
/// binds `T`, and when it does, a field argument was always fine. It comes
/// back empty in exactly two situations, and each one alone is enough —
/// the element type is NAMELESS (a tuple, the B-2026-08-27-40 channel
/// limit), or the call sits inside a GENERIC IMPL, where the binding is
/// self-referential and deliberately dropped. Hence `Bag[String]` in an
/// impl method breaks while the same `Bag[String]` from `main` does not.
///
/// Verified RED: every `mut ref` case below returns the pre-fix wrong
/// answer on an unmodified tree, and `02_string`'s AOT binary exits 139.
#[test]
fn e2e_container_param_bound_to_a_struct_field_argument() {
    // (a) tuple element, generic impl — the row's own repro. An 8-byte
    // swap over a 16-byte element, silent on both compiled backends.
    let tuple_in_impl = "struct Bag[=T] { xs: Vec[T] }\n\
             fn swap01[T](v: mut ref Vec[T]) { v.swap(0, 1); }\n\
             impl[T] Bag[T] {\n\
                 fn go(mut ref self) { swap01(mut self.xs); }\n\
                 fn at(ref self, i: i64) -> T { return self.xs[i]; }\n\
             }\n\
             fn main() {\n\
                 let mut a: Bag[(i64, i64)] = Bag { xs: Vec.new() };\n\
                 a.xs.push((1, 10)); a.xs.push((2, 20)); a.go();\n\
                 let z0 = a.at(0); let z1 = a.at(1);\n\
                 println(f\"{z0.0}:{z0.1} {z1.0}:{z1.1}\");\n\
             }\n";
    assert_eq!(run_program(tuple_in_impl).as_deref(), Some("2:20 1:10\n"));
    // (b) a NAMED element in the same impl — pre-fix this SEGFAULTED, so
    // a nameless element is not what makes the shape fail.
    let string_in_impl = "struct Bag[=T] { xs: Vec[T] }\n\
             fn swap01[T](v: mut ref Vec[T]) { v.swap(0, 1); }\n\
             impl[T] Bag[T] {\n\
                 fn go(mut ref self) { swap01(mut self.xs); }\n\
                 fn at(ref self, i: i64) -> T { return self.xs[i]; }\n\
             }\n\
             fn main() {\n\
                 let mut a: Bag[String] = Bag { xs: Vec.new() };\n\
                 a.xs.push(\"aa\"); a.xs.push(\"bb\"); a.go();\n\
                 println(f\"{a.at(0)} {a.at(1)}\");\n\
             }\n";
    assert_eq!(run_program(string_in_impl).as_deref(), Some("bb aa\n"));
    // (c) NO impl and a NON-generic struct — the receiver's genericity is
    // not the trigger either; the nameless tuple element empties the frame
    // on its own.
    let tuple_plain_struct = "struct Bag { xs: Vec[(i64, i64)] }\n\
             fn swap01[T](v: mut ref Vec[T]) { v.swap(0, 1); }\n\
             fn main() {\n\
                 let mut a: Bag = Bag { xs: Vec.new() };\n\
                 a.xs.push((1, 10)); a.xs.push((2, 20));\n\
                 swap01(mut a.xs);\n\
                 let z0 = a.xs[0]; let z1 = a.xs[1];\n\
                 println(f\"{z0.0}:{z0.1} {z1.0}:{z1.1}\");\n\
             }\n";
    assert_eq!(
        run_program(tuple_plain_struct).as_deref(),
        Some("2:20 1:10\n")
    );
    // (d) a generic struct, tuple element, called from `main` rather than
    // from an impl method.
    let tuple_generic_from_main = "struct Bag[=T] { xs: Vec[T] }\n\
             fn swap01[T](v: mut ref Vec[T]) { v.swap(0, 1); }\n\
             fn main() {\n\
                 let mut a: Bag[(i64, i64)] = Bag { xs: Vec.new() };\n\
                 a.xs.push((1, 10)); a.xs.push((2, 20));\n\
                 swap01(mut a.xs);\n\
                 let z0 = a.xs[0]; let z1 = a.xs[1];\n\
                 println(f\"{z0.0}:{z0.1} {z1.0}:{z1.1}\");\n\
             }\n";
    assert_eq!(
        run_program(tuple_generic_from_main).as_deref(),
        Some("2:20 1:10\n")
    );
    // (e) the LOUD half: a callee that RETURNS the element. With `T`
    // unbound the mono returned `i64` against a `{i64,i64}` operand and
    // module verification hard-failed, so this program did not build at
    // all. Where the callee returns nothing the LLVM signature is a bare
    // `ptr` and there is no mismatch to catch — which is why the swap
    // above was silent and this was not.
    let returning = "struct Bag[=T] { xs: Vec[T] }\n\
             fn first[T](v: ref Vec[T]) -> T { return v[0]; }\n\
             impl[T] Bag[T] {\n\
                 fn head(ref self) -> T { return first(self.xs); }\n\
             }\n\
             fn main() {\n\
                 let mut a: Bag[(i64, i64)] = Bag { xs: Vec.new() };\n\
                 a.xs.push((1, 10)); a.xs.push((2, 20));\n\
                 let h = a.head();\n\
                 println(f\"{h.0}:{h.1}\");\n\
             }\n";
    assert_eq!(run_program(returning).as_deref(), Some("1:10\n"));
    // (f) the IDENTIFIER-argument control: correct before the fix and
    // after, so it pins the diagnosis to the argument's shape rather than
    // to the generic callee or the tuple element.
    let ident_control = "fn swap01[T](v: mut ref Vec[T]) { v.swap(0, 1); }\n\
             fn main() {\n\
                 let mut t: Vec[(i64, i64)] = Vec.new();\n\
                 t.push((1, 10)); t.push((2, 20));\n\
                 swap01(mut t);\n\
                 let z0 = t[0]; let z1 = t[1];\n\
                 println(f\"{z0.0}:{z0.1} {z1.0}:{z1.1}\");\n\
             }\n";
    assert_eq!(run_program(ident_control).as_deref(), Some("2:20 1:10\n"));
}

/// B-2026-08-29-3 — a GENERIC method that hands an owned argument back
/// stands its caller down, exactly as the non-generic one does.
///
/// A method reaches the monomorph argument loop under a plain `Type.method`
/// key, which `call_arg_flows_into_return`'s `Item::Function`-only scan
/// answers `false` for, so the caller kept registering a body the RESULT
/// binding also owned: two bodies for one object on all three compiled
/// backends, against one in the interpreter and one for both the
/// non-generic and free-function twins, which are the oracles here.
///
/// The CONDITIONAL leg is admitted too, since B-2026-08-28-71 taught
/// `compile_mono_function` to install the callee-side per-path flip for a
/// monomorph. Before that it could not be stood down — the callee had no
/// way to own the body — which is what B-2026-08-29-16 tracked, and the two
/// `generic-method-conditional-*` cases below are that row's program.
#[test]
fn e2e_generic_method_returned_param_body_runs_once() {
    const DROPPER: &str = "struct R { id: i64, tag: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id}\"); } }\n\
             struct G1 { n: i64 }\n";
    for (label, body, want) in [
        // The row's program: a generic method that ALWAYS returns its owned
        // param. Pre-fix `drop 32` / `32` / `drop 32` on every compiled
        // backend — two bodies for one object.
        (
            "generic-method-always-returns",
            "impl G1 { fn gm_ret[T](ref self, r: R, t: T) -> R { r } }\n\
                 fn main() { let h = G1 { n: 1 }; \
                 let b = h.gm_ret(R { id: 32, tag: f\"h\" }, 5); println(f\"{b.id}\"); }\n",
            "32\ndrop 32\n",
        ),
        // The generic DYING direction, fixed by B-2026-08-28-70 and kept as
        // a control: the caller must still fire where nothing else owns it.
        (
            "generic-method-param-dies",
            "impl G1 { fn gm_dies[T](ref self, r: R, t: T) -> i64 { 3 } }\n\
                 fn main() { let g = G1 { n: 1 }; \
                 let a = g.gm_dies(R { id: 31, tag: f\"g\" }, 5); println(f\"{a}\"); }\n",
            "drop 31\n3\n",
        ),
        // B-2026-08-29-16 — the generic CONDITIONAL pair, which used to be
        // wrong in BOTH directions and in OPPOSITE senses: measured at
        // 907b378^, the param-dies leg was interp 0 bodies / compiled 1, and
        // the param-escapes leg interp 1 / compiled 2. Neither backend could
        // be corrected alone, because the static answer cannot be precise —
        // which path runs is a runtime fact — so the resolution had to be the
        // callee-side per-path flag, which `compile_mono_function` did not
        // install for a monomorph until B-2026-08-28-71.
        //
        // CONTROL on this backend: the dying leg was already the right count
        // here, and it is the interpreter twin that was RED for it.
        (
            "generic-method-conditional-param-dies",
            "impl G1 { fn gm_pick[T](ref self, r: R, k: bool, t: T) -> R \
                 { if k { r } else { R { id: 99, tag: f\"z\" } } } }\n\
                 fn main() { let g = G1 { n: 1 }; \
                 let a = g.gm_pick(R { id: 41, tag: f\"a\" }, false, 5); \
                 println(f\"{a.id}\"); }\n",
            "drop 41\n99\ndrop 99\n",
        ),
        // RED on this backend: `drop 41` / `41` / `drop 41` before the fix —
        // the caller registered a body the result binding also owned. The
        // row's free-function contrast (0 bodies on BOTH backends before -71,
        // the documented B-2026-08-28-22 trade) is pinned by
        // `generic-callee-dying-path` in the -22 suite, so it is not repeated here.
        (
            "generic-method-conditional-param-escapes",
            "impl G1 { fn gm_pick[T](ref self, r: R, k: bool, t: T) -> R \
                 { if k { r } else { R { id: 99, tag: f\"z\" } } } }\n\
                 fn main() { let g = G1 { n: 1 }; \
                 let a = g.gm_pick(R { id: 41, tag: f\"a\" }, true, 5); \
                 println(f\"{a.id}\"); }\n",
            "41\ndrop 41\n",
        ),
        // CONTROL — the NON-generic twin, correct since B-2026-08-28-70.
        (
            "non-generic-twin",
            "impl G1 { fn id(ref self, r: R) -> R { r } }\n\
                 fn main() { let g = G1 { n: 1 }; \
                 let b = g.id(R { id: 32, tag: f\"h\" }); println(f\"{b.id}\"); }\n",
            "32\ndrop 32\n",
        ),
        // CONTROL — the free-function oracle, unanimous before and after.
        (
            "free-fn-oracle",
            "fn idf(r: R) -> R { r }\n\
                 fn main() { let b = idf(R { id: 32, tag: f\"h\" }); println(f\"{b.id}\"); }\n",
            "32\ndrop 32\n",
        ),
    ] {
        assert_eq!(
            run_program(&format!("{DROPPER}{body}")).as_deref(),
            Some(want),
            "{label}"
        );
    }
}

/// An ASSOCIATED fn in a generic impl that binds a struct LITERAL to a
/// local and calls a mutating sibling through it — the third shape of the
/// wrong-monomorph family, after B-2026-08-25-7's `let mut h = self` and
/// B-2026-07-15-24's moved-out field.
///
/// The `let` site records a binding's concrete instantiation so the sibling
/// call can bind the impl's type params from it. A struct literal has no
/// entry in the span table at all, so the binding got nothing, the subst
/// came back empty, and `mangle_mono_name` maps an empty subst to the base
/// name — the call went to the UNMANGLED prototype built at the all-`i64`
/// base layout. At a heap-carrying `T` that reads a 24-byte String control
/// block as one i64 and SEGFAULTS.
///
/// Case (b) is a scalar-`T` control and (c) a non-generic one; both were
/// correct before the fix, so they pin the diagnosis rather than merely
/// passing. Verified RED: pre-fix this program segfaults, and `nm` shows a
/// bare `Bag.arrange` / `Bag.swap2` with no `$i64` / `$struct` sibling —
/// one prototype serving both instantiations. Post-fix all four exist.
#[test]
fn e2e_generic_assoc_fn_struct_literal_local_calls_mutating_sibling() {
    let src = r#"
struct Bag[T] { xs: Vec[T] }
impl[T: Ord] Bag[T] {
    fn swap2(mut ref self, i: i64, j: i64) {
        self.xs.swap(i, j);
    }
    fn arrange(mut ref self) { let n = self.xs.len(); if n > 1 { self.swap2(0, n - 1); } }
    fn build(v: Vec[T]) -> Bag[T] { let mut b = Bag { xs: v }; b.arrange(); b }
    fn take(mut ref self) -> Option[T] { self.xs.pop() }
}
struct PlainBag { xs: Vec[String] }
impl PlainBag {
    fn swap2(mut ref self, i: i64, j: i64) {
        self.xs.swap(i, j);
    }
    fn arrange(mut ref self) { let n = self.xs.len(); if n > 1 { self.swap2(0, n - 1); } }
    fn build(v: Vec[String]) -> PlainBag { let mut b = PlainBag { xs: v }; b.arrange(); b }
}
fn main() {
    // (a) heap-carrying T — segfaulted pre-fix. Assert CONTENTS, not the count.
    let mut a = Bag.build(["x", "y", "z"]);
    match a.take() { Some(v) => { println(v); } None => {} }
    println(a.xs.len());
    println(a.xs[0]);
    // (b) scalar control: correct pre-fix, because the base layout IS i64.
    let mut b = Bag.build([1, 2, 3]);
    match b.take() { Some(v) => { println(v); } None => {} }
    // (c) NON-generic control at the same heap element type.
    let c = PlainBag.build(["p", "q"]);
    println(c.xs[0]);
}
"#;
    assert_eq!(run_program(src).as_deref(), Some("x\n2\nz\n1\nq\n"));
}

/// B-2026-08-25-35 — a `#[derive(Ord)]` user type satisfies a `T: Ord` bound on
/// a GENERIC IMPL's method, not only on a free generic fn.
///
/// Two gates discharge bounds and they had different powers. `type_satisfies_bound`
/// (typechecker layer) recognizes `#[derive]` on a named type; `Env::bound_satisfied`
/// (the gate method resolution uses for a bound on a generic impl) did not, and its
/// own comment asserted the derive tables were unreachable from there — they are not,
/// `TypeEnv` owns `structs`/`enums`. So the SAME type against the SAME bound got two
/// answers depending on which side of the call the bound was written on: `fn
/// free[T: Ord](..)` accepted it, `impl[T: Ord] Holder[T] { fn tag(..) }` rejected it
/// with "`Item` does not implement `Ord`; trait `Ord` is implemented by: <primitives>".
///
/// `PriorityQueue[T]`'s whole surface is the second kind, which is why no derived
/// user type could be put in one — the point of the sibling test below.
///
/// Twin of `tests/interpreter.rs`'s
/// `test_derived_ord_satisfies_a_generic_impl_bound`.
#[test]
fn e2e_derived_ord_satisfies_a_generic_impl_bound() {
    let src = r#"
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Item { id: i64 }
struct Holder[=T] { v: T }
impl[T: Ord] Holder[T] {
    fn tag(ref self) -> i64 { 7 }
}
fn free[T: Ord](a: T) -> i64 { 9 }
fn main() {
    let h = Holder { v: Item { id: 1 } };
    println(h.tag());
    println(free(Item { id: 1 }));
}
"#;
    assert_eq!(run_program(src).as_deref(), Some("7\n9\n"));
}

/// B-2026-08-31-48 — two instantiations of one generic fn at different
/// `Array` / `Slice` / `Vector` / tuple type arguments get DISTINCT
/// monomorph symbols.
///
/// They did not. `mangle_mono_name` appends a disambiguating token only for
/// a param that clears a collision-class gate, and that gate read the
/// concrete HEAD NAME:
///
///     let head = match subst_names.get(&param.name) {
///         Some(h) => h.as_str(),
///         None => continue,        // every nameless type argument exits here
///     };
///
/// `type_to_concrete_or_param_name` cannot spell an `Array` / `Slice` /
/// `Vector` / tuple, so `subst_names` had no entry, the param was skipped,
/// and every such instantiation shared ONE symbol — one body, element- and
/// length-erased. The element-aware channel that exists precisely to
/// disambiguate these (`type_to_mono_mangle_token`) had no arm for the
/// three aggregates either, so both routes failed for the same shapes.
///
/// The smallest generic function in the language reproduced it — no
/// Display, no traits, no nesting:
///
///     fn ident[T](x: T) -> T { return x }
///
/// called at `Array[i64, 2]` and `Array[i64, 3]` built
/// `call [2 x i64] @"ident$opaque"([3 x i64] %a3)` and failed module
/// verification, while `--interp` printed both arrays correctly.
///
/// Each line pins a different axis of the identity, because a token that
/// collapses any one of them reintroduces the bug: `len` two lengths at one
/// element type, `elem` two element types at one length, `slice` and
/// `vector` the other two nameless aggregates, `tuple` the shape that has
/// no head name for a different reason, and `method` the same collision
/// through a generic METHOD rather than a free fn.
///
/// `prior` is the regression guard, not decoration: `String` / `Vec[i64]` /
/// `Vec[String]` are the collision class the gate ALREADY handled
/// (B-2026-07-11-35), and widening it must not disturb them.
///
/// Twin of `tests/interpreter.rs`'s
/// `test_generic_fn_at_nameless_aggregate_args`, pinned to the same string.
#[test]
fn e2e_generic_fn_at_nameless_aggregate_args_gets_distinct_monos() {
    let Some(out) = run_program(NAMELESS_MONO_ARG_SRC) else {
        return;
    };
    assert_eq!(
        out, "len 4 9\nelem 1 b\nslice 6 z\nvector 10 36\ntuple 1 3\nmethod 3 7\nprior hi 8 k\n",
        "each nameless-aggregate instantiation must get its own mono; got: {out:?}"
    );
}

/// B-2026-09-06-9 — a by-value `Drop` param handed to a callee that
/// returns it on EVERY exit, with the result bound to a local that dies
/// there (`let w: R = keeps(r); println(..)`), runs the body ONCE: the
/// let-site marks `w` a param VIEW (the caller runs the body on its own
/// temp / named binding after the call), through the four let-site gates
/// (struct, tuple, enum, `Option`/`Result`) and the shared
/// `fn_whole_param_aliases` set — the param, its whole rebinds, and such a
/// call's own result (so `keeps(keeps(r))` chains). Cells: the row's four
/// (tuple/struct x direct/rebind), unread, the two-hop chain, a generic
/// and an associated callee, nested in a branch (both paths), an arm-tail
/// spelling (both paths), an enum, a method frame (fresh and named),
/// named struct/tuple arguments, and two controls that must stay at one
/// body from the RESULT binding: a local source and a destructured LEAF
/// through the same callee (the part channel's case, excluded from the
/// view mark on purpose — marking it too ran zero bodies). Interpreter
/// twin: `test_param_through_returning_callee_bound_locally_runs_one_body`.
#[test]
fn e2e_param_through_returning_callee_bound_locally_runs_one_body() {
    assert_eq!(
            run_program(
                r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct H2 { pe: ((R, i64), i64) }
enum E { A(R), B }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] }; }
fn keep(t: (R, i64)) -> (R, i64) { return t; }
fn keeps(r: R) -> R { return r; }
fn keepg[T](x: T) -> T { return x; }
fn keepe(e: E) -> E { return e; }
fn t_keep_direct(t: (R, i64)) { let w: (R, i64) = keep(t); println(f"tkd {w.0.id}") }
fn t_keep_rebind(t: (R, i64)) { let z: (R, i64) = t; let w: (R, i64) = keep(z); println(f"tkr {w.0.id}") }
fn s_keep_direct(r: R) { let w: R = keeps(r); println(f"skd {w.id}") }
fn s_keep_rebind(r: R) { let z: R = r; let w: R = keeps(z); println(f"skr {w.id}") }
fn s_keep_unread(r: R) { let w: R = keeps(r); println("sku") }
fn s_keep_twice(r: R) { let w: R = keeps(r); let v: R = keeps(w); println(f"skt {v.id}") }
fn s_keep_generic(r: R) { let w: R = keepg(r); println(f"skg {w.id}") }
fn s_keep_assoc(r: R) { let w: R = K.id(r); println(f"ska {w.id}") }
fn s_keep_nested(r: R, k: bool) { if k { let w: R = keeps(r); println(f"skn {w.id}"); } println("skn-out") }
fn s_keep_arm(r: R, k: bool) -> i64 { let w: R = keeps(r); if k { return 1; } println(f"skm {w.id}"); return 0 }
fn e_keep(e: E) { let w: E = keepe(e); match w { E.A(x) => println(f"ek {x.id}"), E.B => println("ekB") } }
fn s_local() { let l = mk(30); let w: R = keeps(l); println(f"sl {w.id}") }
fn v_keep(h: H2) { let (inner, y) = h.pe; let z: (R, i64) = inner; let w: (R, i64) = keep(z); println(f"vk {w.0.id}") }
struct K { n: i64 }
impl K {
    fn id(r: R) -> R { return r; }
    fn m_keep(ref self, r: R) { let w: R = keeps(r); println(f"mk {w.id}") }
}
fn main() {
    let k = K { n: 0 };
    println("one"); t_keep_direct((mk(1), 1));
    println("two"); t_keep_rebind((mk(2), 2));
    println("three"); s_keep_direct(mk(3));
    println("four"); s_keep_rebind(mk(4));
    println("five"); s_keep_unread(mk(5));
    println("six"); s_keep_twice(mk(6));
    println("seven"); s_keep_generic(mk(7));
    println("eight"); s_keep_assoc(mk(8));
    println("nine-t"); s_keep_nested(mk(9), true);
    println("ten-f"); s_keep_nested(mk(10), false);
    println("eleven-t"); let _ = s_keep_arm(mk(11), true);
    println("twelve-f"); let _ = s_keep_arm(mk(12), false);
    println("thirteen"); e_keep(E.A(mk(13)));
    println("fourteen"); k.m_keep(mk(14));
    println("fifteen-named"); let a = mk(15); s_keep_direct(a);
    println("sixteen-mnamed"); let b = mk(16); k.m_keep(b);
    println("seventeen-tnamed"); let t: (R, i64) = (mk(17), 1); t_keep_direct(t);
    println("eighteen-local"); s_local();
    println("nineteen-leaf"); v_keep(H2 { pe: ((mk(19), 1), 2) });
    println("end");
}"#
            ),
            Some("one\ntkd 1\ndR1\ntwo\ntkr 2\ndR2\nthree\nskd 3\ndR3\nfour\nskr 4\ndR4\nfive\nsku\ndR5\nsix\nskt 6\ndR6\nseven\nskg 7\ndR7\neight\nska 8\ndR8\nnine-t\nskn 9\nskn-out\ndR9\nten-f\nskn-out\ndR10\neleven-t\ndR11\ntwelve-f\nskm 12\ndR12\nthirteen\nek 13\ndR13\nfourteen\nmk 14\ndR14\nfifteen-named\nskd 15\ndR15\nsixteen-mnamed\nmk 16\ndR16\nseventeen-tnamed\ntkd 17\ndR17\neighteen-local\nsl 30\ndR30\nnineteen-leaf\nvk 19\ndR19\nend\n".to_string()),
            "a by-value param rebound through an always-returning callee has one owner"
        );
}

#[test]
/// B-2026-09-07-4, the MIXED-PATH half — the part `0164085` deliberately left
/// open. `impl Hold { fn pick(ref self, r: R, k: bool) -> R { if k { return
/// mk(98); } return r; } }` at `k = false` aborted `free(): double free
/// detected in tcache 2` under `karac run` and at -O0 (correct at -O2, where
/// the abort inlines away), and the assoc twin `R.picka(mk(32), false)` with
/// it, while the identical FREE FUNCTION was clean on every surface.
///
/// The asymmetry was B-2026-09-06-69's conditional hand-back flip — a
/// mixed-path callee owning the MEMORY of a declined-copy param under the
/// per-path flag that already guards the body — being free-function-only BY
/// CONSTRUCTION: `conditional_handback_memory_moves_to_callee` resolved
/// through an `Item::Function` scan, and its whole-program gate
/// `compute_handback_safe_params` seeded `live` from `Item::Function` and
/// collected only `ExprKind::Call`. A mixed-path method had no mechanism at
/// all. Both halves move together, which is the safety argument and not an
/// implementation detail: the callee registers a memory owner only where the
/// caller provably stood down, and that coupling is what the predicate's own
/// doc said kept methods out.
///
/// Cells `a`/`b`/`i` are that route (fresh-temp, named-local, assoc).
/// `e`/`h` are the via-call route `0164085` closed, carried as controls.
///
/// `c`/`d`/`j`/`l`/`n` are the DIES-INSIDE controls and they are
/// load-bearing. Conditioning the memory retraction on
/// `callee_takes_over_arg_drop_body` — whose union includes the mixed-path
/// predicate — takes away the only memory owner those legs have and cost
/// `d` 19 bytes definitely lost in 2 blocks, B-2026-09-07-3's signature
/// reproduced on this leg. `m`/`n` are the copy-supported class, whose
/// caller slot keeps its own entry copy (B-2026-08-26-9's carve-out), and
/// `k`/`l` are the free-fn shape that was already correct.
///
/// The mixed-path VIA-CALL spelling (`if k { return mk(95); } return
/// fwd(r);`) is NOT covered here — it needed the per-path flag clearers to
/// see a via-call tail before its callee may own the memory. That is
/// B-2026-09-07-3, closed by `99bd72d`, and pinned by
/// `test_e2e_free_fn_mixed_path_hand_back_through_a_hop_owns_its_argument`
/// below. (This paragraph cited B-2026-09-07-16 for it, which is an
/// unrelated enum-payload double free — an id allocated by two sessions in
/// the same window, the hazard CLAUDE.md's late-allocation rule warns about.)
fn test_e2e_method_and_assoc_mixed_path_hand_back_owns_its_argument() {
    let Some(out) = run_program(
        r#"shared struct Inner { v: i64 }
struct R { id: i64, name: String, inner: Inner }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}", inner: Inner { v: i } }; }
fn fwd(r: R) -> R { return r; }

struct S { id: i64, name: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mks(i: i64) -> S { return S { id: i, name: f"s{i}" }; }

struct Hold { n: i64 }
impl Hold {
    fn pick(ref self, r: R, k: bool) -> R { if k { return mk(98); } return r; }
    fn passmv(ref self, r: R) -> R { return fwd(r); }
    fn passmb(ref self, r: R) -> R { return r; }
    fn picks(ref self, s: S, k: bool) -> S { if k { return mks(96); } return s; }
}
impl R {
    fn passb(r: R) -> R { return fwd(r); }
    fn picka(r: R, k: bool) -> R { if k { return mk(92); } return r; }
}
fn pickf(r: R, k: bool) -> R { if k { return mk(97); } return r; }

// method, MIXED-PATH bare, escaping leg, fresh temp
fn a1() { let h = Hold { n: 1 }; let z = h.pick(mk(21), false); println(f"a{z.id}"); }
// method, MIXED-PATH bare, escaping leg, NAMED LOCAL
fn a2() { let h = Hold { n: 1 }; let a = mk(37); let z = h.pick(a, false); println(f"b{z.id}"); }
// method, MIXED-PATH bare, DIES-INSIDE leg -- the leak control, both spellings
fn a3() { let h = Hold { n: 1 }; let z = h.pick(mk(22), true); println(f"c{z.id}"); }
fn a4() { let h = Hold { n: 1 }; let a = mk(38); let z = h.pick(a, true); println(f"d{z.id}"); }
// method, ALL-PATHS via-call, both spellings
fn a5() { let h = Hold { n: 1 }; let z = h.passmv(mk(26)); println(f"e{z.id}"); }
// method, ALL-PATHS bare -- B-2026-09-06-70's fix, must stay clean
fn a7() { let h = Hold { n: 1 }; let z = h.passmb(mk(25)); println(f"g{z.id}"); }
// assoc, ALL-PATHS via-call
fn a8() { let z = R.passb(mk(19)); println(f"h{z.id}"); }
// assoc, MIXED-PATH bare, both legs
fn a9() { let z = R.picka(mk(32), false); println(f"i{z.id}"); }
fn a10() { let z = R.picka(mk(33), true); println(f"j{z.id}"); }
// free-fn mixed-path -- B-2026-09-06-69's own shape, must stay clean
fn a11() { let z = pickf(mk(23), false); println(f"k{z.id}"); }
fn a12() { let z = pickf(mk(24), true); println(f"l{z.id}"); }
// COPY-SUPPORTED class -- the -08-26-9 carve-out, must keep its memory
fn a13() { let h = Hold { n: 1 }; let z = h.picks(mks(41), false); println(f"m{z.id}"); }
fn a14() { let h = Hold { n: 1 }; let z = h.picks(mks(42), true); println(f"n{z.id}"); }

fn main() {
    a1(); a2(); a3(); a4(); a5(); a7(); a8(); a9(); a10(); a11(); a12(); a13(); a14();
    println("end");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"a21
dR21
b37
dR37
dR22
c98
dR98
dR38
d98
dR98
e26
dR26
g25
dR25
h19
dR19
i32
dR32
dR33
j92
dR92
k23
dR23
dR24
l97
dR97
m41
dS41
dS42
n96
dS96
end
"#
    );
}

#[test]
fn test_e2e_named_binding_receiver_in_generic_impl_still_correct() {
    // Control: the SAME value bound to a name first was always correct
    // (the `let` site seeds the instantiation). It must stay correct — the
    // fix tightens the temporary path only.
    let src = format!(
        "{B28_BAG}    fn mk(v: Vec[T]) -> Vec[T] {{ let q = Bag {{ xs: v }}; q.inner() }}\n\
             }}\n\
             fn main() {{ let a = Bag.mk([\"x\",\"y\",\"z\"]); println(a[0]); \
             let b = Bag.mk([1,2,3]); println(b[0]); }}"
    );
    assert_eq!(run_program(&src).as_deref(), Some("z\n3\n"));
}

/// B-2026-08-22-17 — the same qualified spelling over a BUILT-IN head.
///
/// B-2026-08-21-53 made `Type[Args].fn(..)` the documented way to pin a type
/// explicitly, and wired it for USER types. It stayed broken for the
/// builtins — `Vec[i64].new()`, `Map[String, i64].new()`,
/// `Channel[i64].new()` all reported "no method 'new' on `Vec[…]`" — i.e.
/// the spec named a form that failed on exactly the types whose element
/// cannot always be inferred from context.
///
/// WHY IT SPLIT ALONG USER-vs-BUILTIN. `Vec.new()` parses as a CALL over a
/// two-segment path and is answered by `infer_call`'s constructor arm;
/// `Vec[i64].new()` parses as a METHOD CALL whose receiver is a one-segment
/// path with generic args, which resolved against `env.impls` — where a
/// builtin has no entry at all. The fix routes the second form to the
/// first and then pins the args, so there is still ONE implementation of
/// each constructor.
///
/// Each case is asserted against the value the UNQUALIFIED spelling
/// produces, because the two must agree by construction.
mod builtin_type_qualified_assoc_call {
    use super::run_program;

    #[test]
    fn qualified_builtin_constructors_match_their_unqualified_twins() {
        // Vec / Map / Set, plus `with_capacity` to show it is not just `new`.
        let src = "fn main() {\n\
                \x20   let mut v = Vec[i64].new(); v.push(3); v.push(4); println(v.len());\n\
                \x20   let mut m = Map[String, i64].new(); m.insert(\"a\", 1); println(m.len());\n\
                \x20   let mut s = Set[i64].new(); let _ = s.insert(7); println(s.len());\n\
                \x20   let mut w = Vec[i64].with_capacity(4); w.push(9); println(w.len());\n\
                }\n";
        assert_eq!(run_program(src).as_deref(), Some("2\n1\n1\n1\n"));

        // The unqualified twins, as the control: these always worked, so a
        // difference here would mean the delegation changed the meaning of
        // the spelling it delegates to.
        let twin = "fn main() {\n\
                \x20   let mut v: Vec[i64] = Vec.new(); v.push(3); v.push(4); println(v.len());\n\
                \x20   let mut m: Map[String, i64] = Map.new(); m.insert(\"a\", 1); println(m.len());\n\
                \x20   let mut s: Set[i64] = Set.new(); let _ = s.insert(7); println(s.len());\n\
                \x20   let mut w: Vec[i64] = Vec.with_capacity(4); w.push(9); println(w.len());\n\
                }\n";
        assert_eq!(run_program(twin).as_deref(), Some("2\n1\n1\n1\n"));
    }

    /// `Channel` is the case that broke the obvious implementation, so it
    /// gets its own test rather than a line in the one above.
    ///
    /// Every other builtin constructor returns a value NAMED after the head
    /// (`Vec.new() -> Vec[?T]`), so pinning is a unification against
    /// `Head[args]`. `Channel.new()` returns `(Sender[?T], Receiver[?T])` —
    /// the head name appears nowhere in the result and the pinned element
    /// sits inside a tuple, so that unification reports a mismatch on a
    /// perfectly well-formed call. The fix pairs the explicit args with the
    /// result's own free metavars instead, which also means the expression's
    /// type must stay the TUPLE: handing back `Channel[i64]` would break the
    /// destructuring on the very next token.
    #[test]
    fn a_qualified_channel_pins_through_a_tuple_return() {
        let src = "fn main() {\n\
                \x20   let (tx, rx) = Channel[i64].new();\n\
                \x20   tx.send(5);\n\
                \x20   println(rx.recv());\n\
                }\n";
        assert_eq!(run_program(src).as_deref(), Some("5\n"));
    }

    /// The qualified form has to work where the ANNOTATION workaround
    /// cannot: an argument position with no binding to annotate. This is
    /// the case that makes the row worth fixing rather than documenting.
    #[test]
    fn a_qualified_builtin_works_in_argument_position() {
        let src = "fn take(v: Vec[String]) -> i64 { return v.len(); }\n\
                fn main() { println(take(Vec[String].new())); }\n";
        assert_eq!(run_program(src).as_deref(), Some("0\n"));
    }
}

/// B-2026-08-22-15 — a `-> Self` trait method on an EXISTENTIAL receiver
/// must RUN, not just typecheck. The row predicted this: B-2026-08-22-12's
/// substitution already hands codegen a concrete witness for the
/// existential, and the impl method is ordinary, so recovering the type is
/// the whole job. Asserted rather than assumed.
mod self_returning_existential {
    use super::run_program;

    const DECLS: &str = "trait Counter { fn bumped(self) -> Self; fn value(ref self) -> i64; }\n\
                             struct Ctr { n: i64 }\n\
                             impl Counter for Ctr {\n\
                             \x20   fn bumped(self) -> Ctr { Ctr { n: self.n + 1 } }\n\
                             \x20   fn value(ref self) -> i64 { self.n }\n\
                             }\n\
                             fn make(n: i64) -> impl Counter { Ctr { n: n } }\n";

    #[test]
    fn a_bound_self_returning_existential_runs() {
        assert_eq!(
                run_program(&format!(
                    "{DECLS}fn main() {{ let c = make(1); let d = c.bumped(); println(f\"{{d.value()}}\"); }}\n"
                ))
                .as_deref(),
                Some("2\n"),
                "the builder shape must build and run"
            );
    }

    /// Chained — each link returns the existential again, so this fails
    /// differently from the single-step case if `Self` is only recovered
    /// once.
    #[test]
    fn a_chained_self_returning_existential_runs() {
        assert_eq!(
                run_program(&format!(
                    "{DECLS}fn main() {{ println(f\"{{make(1).bumped().bumped().bumped().value()}}\"); }}\n"
                ))
                .as_deref(),
                Some("4\n"),
                "three chained `-> Self` calls must each keep the existential"
            );
    }
}

mod type_qualified_assoc_call {
    use super::run_program;

    const DECLS: &str = "struct Box[T] { value: T }\n\
                             impl[T] Box[T] {\n\
                             \x20   fn make(v: T) -> Box[T] { return Box { value: v }; }\n\
                             \x20   fn zero() -> i64 { return 0i64; }\n\
                             }\n";

    #[test]
    fn a_qualified_assoc_call_binds_and_reads_its_field() {
        assert_eq!(
            run_program(&format!(
                "{DECLS}fn main() {{ let b = Box[i64].make(7i64); println(f\"{{b.value}}\"); }}\n"
            ))
            .as_deref(),
            Some("7\n"),
            "the qualified spelling must build and run"
        );
        // The unqualified twin, as the control: it always worked, so a
        // regression that broke BOTH would still be caught here.
        assert_eq!(
            run_program(&format!(
                "{DECLS}fn main() {{ let b = Box.make(7i64); println(f\"{{b.value}}\"); }}\n"
            ))
            .as_deref(),
            Some("7\n"),
            "the unqualified spelling is the control and must keep working"
        );
    }

    /// A DIRECT chain on the call result, with no binding for the codegen
    /// side-tables to answer from. This is the case a generic impl makes hard:
    /// its methods are monomorphized on demand and never registered under the
    /// bare `Type.method` key, so the return type has to come from the call's
    /// own recorded instantiation.
    #[test]
    fn a_qualified_assoc_call_types_a_direct_field_chain() {
        assert_eq!(
            run_program(&format!(
                "{DECLS}fn main() {{ let d = Box[String].make(\"hi\").value; println(d); }}\n"
            ))
            .as_deref(),
            Some("hi\n"),
            "a field read directly off the call result must resolve"
        );
    }

    /// An associated fn whose return type is NOT the generic type. This one
    /// survived the first (wrong) fix — `compile_assoc_call` by name alone —
    /// which is exactly why it is pinned separately from the generic case.
    #[test]
    fn a_qualified_assoc_call_returning_a_scalar_runs() {
        assert_eq!(
            run_program(&format!(
                "{DECLS}fn main() {{ println(f\"{{Box[String].zero()}}\"); }}\n"
            ))
            .as_deref(),
            Some("0\n"),
            "a scalar-returning associated fn must run under the qualified spelling"
        );
    }

    #[test]
    fn a_qualified_assoc_call_works_on_a_generic_enum() {
        assert_eq!(
                run_program(
                    "enum Opt2[T] { Nothing, Just(T) }\n\
                     impl[T] Opt2[T] { fn none() -> Opt2[T] { return Opt2.Nothing; } }\n\
                     fn main() {\n\
                     \x20   let o = Opt2[i64].none();\n\
                     \x20   match o { Opt2.Nothing => println(\"nothing\"), Opt2.Just(v) => println(f\"just {v}\") }\n\
                     }\n"
                )
                .as_deref(),
                Some("nothing\n"),
                "an enum receiver must dispatch the same way a struct one does"
            );
    }

    /// Nesting — a qualified call supplying the argument of another. Pins that
    /// the re-formed call node is compiled through the ordinary expression
    /// path rather than intercepted only at statement level.
    #[test]
    fn a_qualified_assoc_call_nests_as_an_argument() {
        assert_eq!(
                run_program(&format!(
                    "{DECLS}fn main() {{ let c = Box[i64].make(Box[i64].zero()); println(f\"{{c.value}}\"); }}\n"
                ))
                .as_deref(),
                Some("0\n"),
                "a qualified call must work in argument position"
            );
    }

    /// A local binding must still shadow the type name — the guard both
    /// backends apply before treating a receiver as a type.
    #[test]
    fn a_binding_still_shadows_the_type_name() {
        assert_eq!(
            run_program(
                "struct Holder { value: i64 }\n\
                     fn main() {\n\
                     \x20   let fs: Vec[i64] = [10i64, 20i64];\n\
                     \x20   println(f\"{fs[1]}\");\n\
                     }\n"
            )
            .as_deref(),
            Some("20\n"),
            "indexing a value binding must not be read as type application"
        );
    }
}

#[test]
fn e2e_generic_at_both_128_bit_signednesses_keeps_each_ones_value() {
    // B-2026-08-30-45 — behavioural half of the symbol test above. One body
    // served both widths and whichever was emitted FIRST decided the
    // signedness for both, so the defect is order-dependent and symmetric.
    // Both directions are here because they fail differently, and only one
    // of them was in the row:
    //
    //   i128-then-u128   `u128::MAX` printed -1
    //   u128-then-i128   `i128 -7`   printed 340282366920938463463374607431768211449
    //
    // that second number being 2^128 - 7 — the i128 rendered through the
    // u128 monomorph. Measuring only the first direction would leave the
    // impression that u128 is the broken width; it is not, the SHARED
    // SYMBOL is, and either width can be the loser.
    //
    // The single-width programs are the controls that localize it, and they
    // are why the row's conclusion ("nothing about 128-bit codegen is
    // broken, only the symbol was shared") holds: each is correct pre-fix.
    // `control-i64-u64-only` and `control-f16-bf16` guard the two families
    // the name channel already listed.
    //
    // Compiled output only, no interpreter twin: `--interp` prints -1 for
    // every u128 here regardless, which is B-2026-08-30-44 and still open.
    let big = "340282366920938463463374607431768211455";
    let show = "fn show[T](x: T) -> String { f\"{x}\" }\n";
    let cases: &[(&str, String, String)] = &[
            (
                "i128-then-u128",
                format!(
                    "{show}fn main() {{ let a: i128 = -7i128; let b: u128 = {big}u128; println(show(a)); println(show(b)); }}"
                ),
                format!("-7\n{big}\n"),
            ),
            (
                "u128-then-i128",
                format!(
                    "{show}fn main() {{ let b: u128 = {big}u128; let a: i128 = -7i128; println(show(b)); println(show(a)); }}"
                ),
                format!("{big}\n-7\n"),
            ),
            (
                "two-param-generic",
                format!(
                    "fn pair[T](x: T, y: T) -> String {{ f\"{{x}},{{y}}\" }}\n\
                     fn main() {{ let a: i128 = -3i128; let b: i128 = 4i128; let c: u128 = {big}u128; let d: u128 = 1u128; println(pair(a, b)); println(pair(c, d)); }}"
                ),
                format!("-3,4\n{big},1\n"),
            ),
            (
                "both-plus-i64-u64",
                format!(
                    "{show}fn main() {{ let a: i128 = -7i128; let b: u128 = {big}u128; let c: i64 = -5; let d: u64 = 18446744073709551615u64; println(show(a)); println(show(b)); println(show(c)); println(show(d)); }}"
                ),
                format!("-7\n{big}\n-5\n18446744073709551615\n"),
            ),
            (
                "control-u128-only",
                format!("{show}fn main() {{ let b: u128 = {big}u128; println(show(b)); }}"),
                format!("{big}\n"),
            ),
            (
                "control-i128-only",
                format!("{show}fn main() {{ let a: i128 = -7i128; println(show(a)); }}"),
                "-7\n".to_string(),
            ),
            (
                "control-i64-u64-only",
                format!(
                    "{show}fn main() {{ let c: i64 = -5; let d: u64 = 18446744073709551615u64; println(show(c)); println(show(d)); }}"
                ),
                "-5\n18446744073709551615\n".to_string(),
            ),
            (
                "control-f16-bf16",
                format!(
                    "{show}fn main() {{ let a: f16 = 1.5f16; let b: bf16 = 2.5bf16; println(show(a)); println(show(b)); }}"
                ),
                "1.5\n2.5\n".to_string(),
            ),
        ];
    for (label, src, want) in cases {
        assert_eq!(run_program(src).as_deref(), Some(want.as_str()), "{label}");
    }
}

#[test]
fn test_ir_transitive_generic_keeps_the_callee_at_the_declared_signedness() {
    // B-2026-08-31-11. `fn wrap[T](x: T) { show(x) }` calling
    // `fn show[T](x: T)` resolves show's `T` to the CALLER's `T`, and
    // `infer_call` drops that as self-referential (the guard exists so the
    // interpreter's substitution stack never sees a `T -> T` entry
    // shadowing the outer frame that does know `T`). `call_type_subs` is
    // then empty for the inner call, `subst_names` stays unfilled, and the
    // mangle falls back to `llvm_type_to_mangle_str` — which carries the
    // WIDTH but not the SIGNEDNESS.
    //
    // So `wrap$u32` called `show$i32`, and the body compiled at that
    // signedness printed `u32::MAX` as -1. The symbol is the assertion
    // because it is the mechanism: `wrap` keeps its own declared name and
    // only the CALLEE loses it, which is what separates this from
    // B-2026-08-30-45 (one symbol serving two widths, needing two
    // instantiations to show; this needs one).
    let ir = ir_for(
        "fn show[T](x: T) -> String { f\"{x}\" }\n\
             fn wrap[T](x: T) -> String { show(x) }\n\
             fn main() {\n\
                 let v: u32 = 4294967295u32;\n\
                 println(wrap(v));\n\
             }",
    );
    for sym in ["wrap$u32", "show$u32"] {
        assert!(
            ir.contains(&format!("@\"{sym}\"(")),
            "expected `{sym}`; the transitive callee must keep the declared \
                 signedness, not fall back to the LLVM token. IR:\n{ir}"
        );
    }
    assert!(
        !ir.contains("@\"show$i32\"("),
        "`show` was instantiated at the SIGNED sibling of the same width; \
             IR:\n{ir}"
    );
}

/// B-2026-08-31-11 — the behavioural half, and the interpreter's oracle.
///
/// Every expectation below is what `--interp` prints, and the interpreter
/// is right here for a structural reason: it carries the declared type at
/// runtime and recovers signedness from it (B-2026-08-30-44), so it never
/// had a symbol to collide. The compiled backends monomorphize, and the
/// transitive instantiation lost the name.
///
/// Measured on the pre-fix compiler: 15 of these 22 cases diverged, at
/// EVERY unsigned width (u8/u16/u32/u64/u128/usize), at any nesting depth,
/// through a `ref` param, and in ARITHMETIC as well as display —
/// `u64::MAX / 2` gave 0 and `u64::MAX < 2` gave true. The row that filed
/// this listed only u64 and u128 display and left the arithmetic half as an
/// open question; both are wider than it recorded.
///
/// `control-renamed-param` is the localizer: the identical program with the
/// callee's parameter spelled `U` instead of `T` was ALWAYS correct, which
/// is what pins this to the name collision rather than to nesting.
/// `control-tuple-arg` is the guard on the fix's own fallback — a callee
/// whose type argument is a TUPLE has no spellable name, must keep its
/// structural `$struct$…` token, and must not be dragged onto the
/// enclosing scalar's `$u64`.
#[test]
fn e2e_transitive_generic_keeps_the_declared_unsignedness() {
    let hdr = "fn show[T](x: T) -> String { f\"{x}\" }\n\
                   fn showU[U](x: U) -> String { f\"{x}\" }\n\
                   fn wrap[T](x: T) -> String { show(x) }\n\
                   fn wrapU[T](x: T) -> String { showU(x) }\n\
                   fn wrap2[T](x: T) -> String { wrap(x) }\n\
                   fn wrapRef[T](x: ref T) -> String { show(x) }\n\
                   fn dv[T: Div](a: T, b: T) -> T { a / b }\n\
                   fn wdv[T: Div](a: T, b: T) -> T { dv(a, b) }\n\
                   fn lt[T: PartialOrd](a: T, b: T) -> bool { a < b }\n\
                   fn wlt[T: PartialOrd](a: T, b: T) -> bool { lt(a, b) }\n\
                   fn add1[T: Add](x: T, o: T) -> T { x + o }\n\
                   fn wInline[T: Add](x: T, o: T) -> String { show(add1(x, o)) }\n\
                   fn wArithInline[T: Add](x: T, o: T) -> String { show(x + o) }\n\
                   fn wLet[T: Add](x: T, o: T) -> String { let y = add1(x, o); show(y) }\n\
                   fn mkPair[T](x: T) -> (T, T) { (x, x) }\n\
                   fn showP[T](p: T) -> String { f\"pair\" }\n\
                   fn wPair[T](x: T) -> String { showP(mkPair(x)) }\n";
    let big = "let v: u64 = 18446744073709551614u64; let o: u64 = 1u64;";
    for (label, body, want) in [
        // Display, every unsigned width.
        (
            "u8",
            "let v: u8 = 255u8; println(wrap(v));".to_string(),
            "255\n",
        ),
        (
            "u16",
            "let v: u16 = 65535u16; println(wrap(v));".to_string(),
            "65535\n",
        ),
        (
            "u32",
            "let v: u32 = 4294967295u32; println(wrap(v));".to_string(),
            "4294967295\n",
        ),
        (
            "u64",
            "let v: u64 = 18446744073709551615u64; println(wrap(v));".to_string(),
            "18446744073709551615\n",
        ),
        (
            "u128",
            "let v: u128 = 340282366920938463463374607431768211455u128;\n\
                 println(wrap(v));"
                .to_string(),
            "340282366920938463463374607431768211455\n",
        ),
        (
            "usize",
            "let v: usize = 18446744073709551615u64 as usize; println(wrap(v));".to_string(),
            "18446744073709551615\n",
        ),
        // Depth and parameter mode.
        (
            "depth-3",
            "let v: u64 = 18446744073709551615u64; println(wrap2(v));".to_string(),
            "18446744073709551615\n",
        ),
        (
            "ref-param",
            "let v: u32 = 4294967295u32; println(wrapRef(v));".to_string(),
            "4294967295\n",
        ),
        // ARITHMETIC — the half the row left as an open question. A signed
        // reading of u64::MAX is -1, so the division gave 0 and the
        // comparison gave true.
        (
            "arith-div",
            "let b: u64 = 18446744073709551615u64; let t: u64 = 2u64;\n\
                 println(wdv(b, t));"
                .to_string(),
            "9223372036854775807\n",
        ),
        (
            "arith-cmp",
            "let b: u64 = 18446744073709551615u64; let t: u64 = 2u64;\n\
                 println(wlt(b, t));"
                .to_string(),
            "false\n",
        ),
        // Argument shapes. A binding-rooted argument resolves from the
        // enclosing mono's own registration; an INLINE expression names no
        // binding and resolves from the enclosing type-param binding.
        (
            "inline-call-arg",
            format!("{big} println(wInline(v, o));"),
            "18446744073709551615\n",
        ),
        (
            "inline-arith-arg",
            format!("{big} println(wArithInline(v, o));"),
            "18446744073709551615\n",
        ),
        (
            "let-bound-arg",
            format!("{big} println(wLet(v, o));"),
            "18446744073709551615\n",
        ),
        // Both signednesses in ONE program: two distinct instantiation
        // chains that previously collapsed onto the signed one.
        (
            "both-signs-one-program",
            "let u: u32 = 4294967295u32; let s: i32 = -1i32;\n\
                 println(wrap(u)); println(wrap(s));"
                .to_string(),
            "4294967295\n-1\n",
        ),
        // Controls.
        (
            "control-renamed-param",
            "let v: u32 = 4294967295u32; println(wrapU(v));".to_string(),
            "4294967295\n",
        ),
        (
            "control-direct",
            "let v: u64 = 18446744073709551615u64; println(show(v));".to_string(),
            "18446744073709551615\n",
        ),
        (
            "control-i64",
            "let s: i64 = -7; println(wrap(s));".to_string(),
            "-7\n",
        ),
        (
            "control-i32",
            "let s: i32 = -7i32; println(wrap(s));".to_string(),
            "-7\n",
        ),
        (
            "control-f64",
            "let f: f64 = 1.5; println(wrap(f));".to_string(),
            "1.5\n",
        ),
        (
            "control-string",
            "let s: String = \"hi\"; println(wrap(s));".to_string(),
            "hi\n",
        ),
        (
            "control-bool",
            "let b: bool = true; println(wrap(b));".to_string(),
            "true\n",
        ),
        (
            "control-tuple-arg",
            format!("{big} println(wPair(v)); println(wrap(v));"),
            "pair\n18446744073709551614\n",
        ),
    ] {
        let src = format!("{hdr}fn main() {{\n    {body}\n}}");
        assert_eq!(run_program(&src).as_deref(), Some(want), "case {label}");
    }
}

/// B-2026-08-31-12 — a `u64` FIELD read inside a generic `impl` printed as
/// -1 on both compiled backends.
///
/// `expr_is_unsigned_int` decides signedness for the print / f-string path
/// from the field's DECLARED type name. For `struct Box[T] { v: T }` that
/// name is `T`, which is not a uint name, so the field fell through to
/// signed and every widening sign-extended. The enclosing monomorph knows
/// what `T` is; the three arms that read a declared name (the field itself,
/// a `Vec[T]` element reached through a local, and one reached through a
/// field) now resolve through `type_subst_names` first — a no-op outside a
/// mono and for an already-concrete name.
///
/// The interpreter is the oracle: B-2026-08-30-43 gave a generic impl body
/// its substitution frame, which fixed that half, and this row's own
/// correction records that the remaining divergence was compiled-only.
/// `interp_generic_impl_field_is_the_codegen_unsignedness_oracle` in
/// tests/interpreter.rs pins the same values on that side.
///
/// NARROWER THAN "A GENERIC IMPL", which is why the controls outnumber the
/// cases. Measured pre-fix, 8 of these 19 shapes diverged and 11 agreed:
/// ARITHMETIC on the same field (`self.v / d`, `self.v < o`) was already
/// correct — answering the row's open scope note in the negative — as were
/// a method PARAM of type `T` and the field copied to a local before use.
/// Only a read that reaches `expr_is_unsigned_int` with the field's own
/// declared name was affected.
#[test]
fn e2e_generic_impl_field_keeps_the_declared_unsignedness() {
    let hdr = "struct Box[T] { v: T }\n\
                   impl[T] Box[T] {\n\
                   \x20   fn get(ref self) -> String { f\"{self.v}\" }\n\
                   \x20   fn viaParam(ref self, x: T) -> String { f\"{x}\" }\n\
                   \x20   fn viaLocal(ref self) -> String { let y = self.v; f\"{y}\" }\n\
                   \x20   fn pr(ref self) { println(self.v); }\n\
                   }\n\
                   impl[T: Div] Box[T] { fn half(ref self, d: T) -> T { self.v / d } }\n\
                   impl[T: PartialOrd] Box[T] {\n\
                   \x20   fn under(ref self, o: T) -> bool { self.v < o }\n\
                   }\n\
                   struct Bag[T] { xs: Vec[T] }\n\
                   impl[T] Bag[T] {\n\
                   \x20   fn first(ref self) -> String { f\"{self.xs[0]}\" }\n\
                   \x20   fn localFirst(ref self) -> String { let ys = self.xs; f\"{ys[0]}\" }\n\
                   }\n\
                   struct BoxU { v: u64 }\n\
                   impl BoxU { fn get(ref self) -> String { f\"{self.v}\" } }\n\
                   fn show[T](x: T) -> String { f\"{x}\" }\n";
    let big = "let big: u64 = 18446744073709551615u64; let b = Box[u64] { v: big };";
    for (label, body, want) in [
        // The field read, at every unsigned width.
        (
            "field-u8",
            "let s: u8 = 255u8; let c = Box[u8] { v: s }; println(c.get());".to_string(),
            "255\n",
        ),
        (
            "field-u16",
            "let s: u16 = 65535u16; let c = Box[u16] { v: s }; println(c.get());".to_string(),
            "65535\n",
        ),
        (
            "field-u32",
            "let s: u32 = 4294967295u32; let c = Box[u32] { v: s }; println(c.get());".to_string(),
            "4294967295\n",
        ),
        (
            "field-u64",
            format!("{big} println(b.get());"),
            "18446744073709551615\n",
        ),
        (
            "field-u128",
            "let s: u128 = 340282366920938463463374607431768211455u128;\n\
                 let c = Box[u128] { v: s }; println(c.get());"
                .to_string(),
            "340282366920938463463374607431768211455\n",
        ),
        // `println(self.v)` reaches the same predicate by a different route.
        (
            "field-println",
            format!("{big} b.pr();"),
            "18446744073709551615\n",
        ),
        // A `Vec[T]` ELEMENT, reached through the field and through a local
        // — the two Index arms that read a declared element name.
        (
            "vec-field-index",
            "let big: u64 = 18446744073709551615u64;\n\
                 let g = Bag[u64] { xs: [big] }; println(g.first());"
                .to_string(),
            "18446744073709551615\n",
        ),
        (
            "vec-local-index",
            "let big: u64 = 18446744073709551615u64;\n\
                 let g = Bag[u64] { xs: [big] }; println(g.localFirst());"
                .to_string(),
            "18446744073709551615\n",
        ),
        // Controls that were ALREADY correct pre-fix. The first two answer
        // the row's scope note: arithmetic on the field was never affected.
        (
            "control-arith-div",
            format!("{big} let d: u64 = 2u64; println(b.half(d));"),
            "9223372036854775807\n",
        ),
        (
            "control-arith-cmp",
            format!("{big} let d: u64 = 2u64; println(b.under(d));"),
            "false\n",
        ),
        (
            "control-method-param",
            format!("{big} println(b.viaParam(big));"),
            "18446744073709551615\n",
        ),
        (
            "control-field-via-local",
            format!("{big} println(b.viaLocal());"),
            "18446744073709551615\n",
        ),
        (
            "control-free-fn",
            "let big: u64 = 18446744073709551615u64; println(show(big));".to_string(),
            "18446744073709551615\n",
        ),
        (
            "control-nongeneric-struct",
            "let big: u64 = 18446744073709551615u64;\n\
                 let n = BoxU { v: big }; println(n.get());"
                .to_string(),
            "18446744073709551615\n",
        ),
        // Signed / non-integer instantiations must not move.
        (
            "control-i64-field",
            "let s: i64 = -7; let c = Box[i64] { v: s }; println(c.get());".to_string(),
            "-7\n",
        ),
        (
            "control-i32-field",
            "let s: i32 = -7i32; let c = Box[i32] { v: s }; println(c.get());".to_string(),
            "-7\n",
        ),
        (
            "control-f64-field",
            "let f: f64 = 1.5; let c = Box[f64] { v: f }; println(c.get());".to_string(),
            "1.5\n",
        ),
        (
            "control-string-field",
            "let s: String = \"hi\"; let c = Box[String] { v: s }; println(c.get());".to_string(),
            "hi\n",
        ),
        (
            "control-i64-vec-field",
            "let s: i64 = -9; let g = Bag[i64] { xs: [s] }; println(g.first());".to_string(),
            "-9\n",
        ),
    ] {
        let src = format!("{hdr}fn main() {{\n    {body}\n}}");
        assert_eq!(run_program(&src).as_deref(), Some(want), "case {label}");
    }
}

#[test]
fn test_e2e_generic_arithmetic_at_reduced_precision() {
    // The compiled twin of `tests/interpreter.rs`'s
    // `test_generic_arithmetic_computes_at_the_instantiated_float_width`
    // — same program, same expected output, so the tree-walk's
    // generic-body rounding and codegen's monomorphization stay pinned to
    // each other (B-2026-08-30-36).
    //
    // Receivers are derived from `env.args().len()` so the optimizer cannot
    // constant-fold the arithmetic away and assert nothing (B-2026-08-29-61);
    // the interpreter twin uses a literal `1` because an in-process test
    // reads the test binary's own argv.
    let out = run_program(
            "fn g_add[T: Add](a: T, b: T) -> T { a + b }\n\
             fn g_sub[T: Sub](a: T, b: T) -> T { a - b }\n\
             fn g_mul[T: Mul](a: T, b: T) -> T { a * b }\n\
             fn g_div[T: Div](a: T, b: T) -> T { a / b }\n\
             fn g_rem[T: Rem](a: T, b: T) -> T { a % b }\n\
             fn g_neg[T: Neg](a: T) -> T { -a }\n\
             fn g_poly[T: Add + Mul](a: T, b: T) -> T { g_add(g_mul(a, b), b) }\n\
             fn g_sum[T: Add](xs: Vec[T], zero: T) -> T {\n\
                 let mut acc = zero;\n\
                 for x in xs { acc = acc + x; }\n\
                 acc\n\
             }\n\
             fn main() {\n\
                 let n: i64 = env.args().len() as i64;\n\
                 let one: f32 = n as f32;\n\
                 let p: f16 = (one * 0.1f32) as f16;\n\
                 let q: f16 = (one * 0.3f32) as f16;\n\
                 let bp: bf16 = (one * 0.1f32) as bf16;\n\
                 let bq: bf16 = (one * 0.3f32) as bf16;\n\
                 let sp: f32 = one * 0.1f32;\n\
                 let sq: f32 = one * 0.3f32;\n\
                 println(f\"h {g_add(p,q)} {g_sub(p,q)} {g_mul(p,q)} {g_div(p,q)} {g_rem(p,q)} {g_neg(p)}\");\n\
                 println(f\"b {g_add(bp,bq)} {g_sub(bp,bq)} {g_mul(bp,bq)} {g_div(bp,bq)} {g_rem(bp,bq)} {g_neg(bp)}\");\n\
                 println(f\"s {g_add(sp,sq)} {g_sub(sp,sq)} {g_mul(sp,sq)} {g_div(sp,sq)} {g_rem(sp,sq)} {g_neg(sp)}\");\n\
                 let big: f16 = (one * 65504.0f32) as f16;\n\
                 let three: f16 = (one * 3.0f32) as f16;\n\
                 let tiny: f16 = (one * 0.00001f32) as f16;\n\
                 println(f\"o {g_mul(big,three)} {g_mul(tiny,tiny)}\");\n\
                 println(f\"y {g_poly(p,q)} {g_poly(bp,bq)} {g_poly(sp,sq)}\");\n\
                 let mut hx: Vec[f16] = vec![];\n\
                 let mut bx: Vec[bf16] = vec![];\n\
                 let mut i: i64 = 0;\n\
                 while i < 10 { hx.push(p); bx.push(bp); i = i + 1; }\n\
                 println(f\"a {g_sum(hx, (one * 0.0f32) as f16)} {g_sum(bx, (one * 0.0f32) as bf16)}\");\n\
             }",
        );
    if let Some(out) = out {
        assert_eq!(
                out.trim(),
                "h 0.39990234375 -0.2000732421875 0.029998779296875 0.333251953125 0.0999755859375 -0.0999755859375\n\
                 b 0.400390625 -0.201171875 0.0301513671875 0.33203125 0.10009765625 -0.10009765625\n\
                 s 0.4000000059604645 -0.20000001788139343 0.030000001192092896 0.3333333134651184 0.10000000149011612 -0.10000000149011612\n\
                 o inf 0\n\
                 y 0.330078125 0.330078125 0.33000001311302185\n\
                 a 1 1.0078125"
            );
    }
}

/// Regression (B-2026-07-03-15): a GENERIC impl/trait method (`m[A](..)`
/// with its OWN generic param) on a CONCRETE receiver monomorphizes under
/// `karac build`. Before the fix the declaration pass skipped generic
/// methods entirely, so the call fell through to "no handler for method"
/// though `karac run` executed it correctly. The method is now registered
/// in `generic_fns` keyed `Type.method` (self prepended as param 0) and the
/// call routes through `compile_generic_call` with the receiver prepended.
/// Covers: an inherent scalar generic method at TWO distinct element types
/// (`wrap(7)` i64 vs `wrap(2.5)` f64 — distinct monos, no width collision),
/// a generic method taking a closure param (`apply` — the `fold[A]` shape),
/// and an explicitly-implemented trait generic method (`dup`).
#[test]
fn e2e_generic_impl_method_monomorphizes_on_concrete_receiver() {
    if let Some(out) = run_program(
        "struct One { v: i64 }\n\
             impl One {\n\
             \x20   fn wrap[A](ref self, x: A) -> A { x }\n\
             \x20   fn apply[A](ref self, init: A, f: Fn(A, i64) -> A) -> A { f(init, self.v) }\n\
             }\n\
             trait Wrapper { fn dup[A](ref self, x: A) -> A; }\n\
             struct C { v: i64 }\n\
             impl Wrapper for C { fn dup[A](ref self, x: A) -> A { x } }\n\
             fn main() {\n\
             \x20   let o = One { v: 40 };\n\
             \x20   println(f\"{o.wrap(7)}\");\n\
             \x20   println(f\"{o.wrap(2.5)}\");\n\
             \x20   println(f\"{o.apply(2, |a, x| a + x)}\");\n\
             \x20   let c = C { v: 0 };\n\
             \x20   println(f\"{c.dup(99)}\");\n\
             }",
    ) {
        // wrap$i64=7, wrap$f64=2.5 (distinct monos), apply closure=2+40=42,
        // trait-impl dup$i64=99.
        assert_eq!(out, "7\n2.5\n42\n99\n");
    }
}

/// B-2026-07-11-25: a GENERIC struct's ASSOCIATED function (`W.make(7)` for
/// `impl[T] W[T]`) returning a struct was miscompiled — the call fell through
/// `compile_assoc_call` (which only knows concrete `module.get_function`
/// names) to its `Ok(const 0)` tail, silently returning a ZEROED struct
/// (native printed 0 / OOM'd on a Vec field; the interpreter was correct).
/// The generic impl method lives only in `generic_fns`, so the 2-segment
/// `Path` dispatch now routes it through `compile_generic_call` like a generic
/// free fn. Covers: arg-inferred `T` (`make(7)`), return-annotation `T`
/// (`S.new()` with no args), a `Vec[T]` field driven through push/len (the
/// original OOM shape), a `String` instantiation, a method on the returned
/// value, a two-type-param generic, and two distinct `i64` monomorphs.
#[test]
fn e2e_generic_assoc_fn_returns_struct() {
    if let Some(out) = run_program(
            "struct W[T] { v: T }\n\
             impl[T] W[T] { fn make(x: T) -> W[T] { W { v: x } } fn get(ref self) -> T { self.v } }\n\
             struct S[T] { items: Vec[T] }\n\
             impl[T] S[T] {\n\
             \x20   fn new() -> S[T] { S { items: Vec.new() } }\n\
             \x20   fn push(mut ref self, x: T) { self.items.push(x); }\n\
             \x20   fn len(ref self) -> i64 { self.items.len() }\n\
             }\n\
             struct P[A, B] { a: A, b: B }\n\
             impl[A, B] P[A, B] { fn of(x: A, y: B) -> P[A, B] { P { a: x, b: y } } }\n\
             fn main() {\n\
             \x20   let w: W[i64] = W.make(7);\n\
             \x20   println(f\"{w.get()}\");\n\
             \x20   let ws: W[String] = W.make(\"hi\");\n\
             \x20   println(ws.v);\n\
             \x20   let mut s: S[i64] = S.new();\n\
             \x20   s.push(10); s.push(20); s.push(30);\n\
             \x20   println(f\"{s.len()}\");\n\
             \x20   let p: P[i64, String] = P.of(5, \"z\");\n\
             \x20   println(f\"{p.a}\"); println(p.b);\n\
             \x20   let a: W[i64] = W.make(1); let b: W[i64] = W.make(2);\n\
             \x20   println(f\"{a.v + b.v}\");\n\
             }",
        ) {
            assert_eq!(out, "7\nhi\n3\n5\nz\n3\n");
        }
}

/// B-2026-07-11-28: a GENERIC monomorph with a VOID return whose body TAIL is
/// a statement-position `if` (or `while`) emitted `ret i64 0` into a void LLVM
/// function — module verification failed ("non-void return in Function of
/// void return type"). A statement-position `if` yields a default `i64 0` from
/// `compile_block`, and the mono return-emission ret-ed it without the
/// `fn_returns_void` guard the non-generic `compile_function` path has (the
/// uncovered sibling of the same-site narrow-width return fix). The generic
/// comparison / `Ord` bound was a red herring — `if true { }` triggers it.
/// Covers: bare `if`, `if/else`, `if { return; }`, and the `T: Ord`
/// compare-and-swap shape; plus value-returning and narrow-`u8` monos to lock
/// the non-void arm against regression.
#[test]
fn e2e_generic_void_fn_tail_if_returns_void() {
    if let Some(out) = run_program(
        "fn noop[T](x: T) { if true { } }\n\
             fn choose[T: Ord](x: T, y: T) { if x > y { } else { } }\n\
             fn early[T: Ord](x: T, y: T) { if x > y { return; } }\n\
             fn cmp_swap[T: Ord](xs: mut ref Vec[T], i: i64, j: i64) {\n\
             \x20   if xs[i] > xs[j] { xs.swap(i, j); }\n\
             }\n\
             fn bigger[T: Ord](x: T, y: T) -> i64 { if x > y { 1 } else { 0 } }\n\
             fn to_u8[T](x: T) -> u8 { 255 }\n\
             fn main() {\n\
             \x20   noop(7); choose(3, 1); early(3, 1);\n\
             \x20   let mut v: Vec[i64] = [5, 2, 9, 1];\n\
             \x20   cmp_swap(mut v, 0, 1);\n\
             \x20   println(f\"{v[0]}\");\n\
             \x20   println(f\"{bigger(9, 4)}\");\n\
             \x20   println(f\"{to_u8(0)}\");\n\
             }",
    ) {
        // cmp_swap(v,0,1): 5>2 → swap → v[0]=2; bigger(9,4)=1; to_u8=255.
        assert_eq!(out, "2\n1\n255\n");
    }
}

/// B-2026-07-11-31: a generic struct instance method mis-inferred its type
/// param `T` (mangled `$i64`, defaulting) when `T` appeared ONLY nested inside
/// a container field (`xs: Vec[T]`) — the typechecker can't solve `T` from
/// `Vec.new()`, so the literal froze as the bare `H[T]`, and codegen's method
/// dispatch synthesized a bare-`T` explicit binding that lowered to `i64` and
/// OVERRODE the correct `T` from the arg. `add(x: T)` mangled `add$i64` (loud
/// verifier error, String arg to i64 param); `get()->T` / `pop()->Option[T]`
/// (no `T` arg) returned i64-shaped garbage (silent). Fixed two ways:
/// (1) drop bare/unsolved impl params from the receiver `explicit` prefix so
/// arg inference wins (method_call.rs); (2) seed the receiver var's
/// instantiation from the concrete `let` ANNOTATION (`H[String]`) rather than
/// the frozen RHS (stmts.rs), which also fixes the no-`T`-arg return/index
/// methods. Covers a String and an f64 heap-like struct through
/// add / get(->T) / pop(->Option[T]); i64 baseline unchanged.
#[test]
fn e2e_generic_method_container_nested_type_param() {
    if let Some(out) = run_program(
        "struct H[T] { xs: Vec[T] }\n\
             impl[T] H[T] {\n\
             \x20   fn new() -> H[T] { H { xs: Vec.new() } }\n\
             \x20   fn add(mut ref self, x: T) { self.xs.push(x); }\n\
             \x20   fn get(ref self, i: i64) -> T { self.xs[i] }\n\
             \x20   fn take(mut ref self) -> Option[T] { self.xs.pop() }\n\
             \x20   fn n(ref self) -> i64 { self.xs.len() }\n\
             }\n\
             fn main() {\n\
             \x20   let mut hs: H[String] = H.new();\n\
             \x20   hs.add(\"a\"); hs.add(\"b\");\n\
             \x20   println(hs.get(0));\n\
             \x20   println(f\"{hs.n()}\");\n\
             \x20   match hs.take() { Some(v) => println(v), None => println(\"none\") }\n\
             \x20   let mut hf: H[f64] = H.new();\n\
             \x20   hf.add(1.5); hf.add(2.5);\n\
             \x20   println(f\"{hf.get(1)}\");\n\
             \x20   let mut hi: H[i64] = H.new();\n\
             \x20   hi.add(7);\n\
             \x20   println(f\"{hi.get(0)}\");\n\
             }",
    ) {
        // hs.get(0)=a, n=2, take=b (last pushed); hf.get(1)=2.5; hi.get(0)=7.
        assert_eq!(out, "a\n2\nb\n2.5\n7\n");
    }
}

/// B-2026-07-03-11 (heap + default-method + `-> Self` facets): a default
/// trait method calling another trait method dispatches through the bound
/// on a String-carrying receiver; and a `-> Self`-returning method bound
/// through the same bound (`let b = w.bump()`) registers `b` under the
/// concrete impl type. The `let b` half exercises the reverse-lookup fix
/// in `stmts.rs` — `w.bump()`'s struct result used to fall to the
/// HashMap-order LLVM-shape reverse-lookup (a `{i64}` `Ctr` aliased to
/// the first same-shape struct, e.g. `TcpStream`), so `b.val()` dispatched
/// against the wrong type.
#[test]
fn e2e_generic_bound_default_and_self_return() {
    if let Some(out) = run_program(
        "trait Greeter {\n\
             \x20   fn name(self) -> String;\n\
             \x20   fn greeting(self) -> String { \"hi \" + self.name() }\n\
             }\n\
             struct Person { id: i64 }\n\
             impl Greeter for Person {\n\
             \x20   fn name(self) -> String { \"person-with-a-long-enough-name\" }\n\
             }\n\
             fn describe[G: Greeter](g: G) -> String { g.greeting() }\n\
             trait Bumpable {\n\
             \x20   fn bump(self) -> Self;\n\
             \x20   fn val(self) -> i64;\n\
             }\n\
             struct Ctr { n: i64 }\n\
             impl Bumpable for Ctr {\n\
             \x20   fn bump(self) -> Self { Ctr { n: self.n + 1 } }\n\
             \x20   fn val(self) -> i64 { self.n }\n\
             }\n\
             fn run_it[W: Bumpable](w: W) -> i64 { let b = w.bump(); b.val() }\n\
             fn main() {\n\
             \x20   println(f\"{describe(Person { id: 1 })}\");\n\
             \x20   println(f\"{run_it(Ctr { n: 41 })}\");\n\
             }",
    ) {
        assert_eq!(out, "hi person-with-a-long-enough-name\n42\n");
    }
}

/// Associated-type PROJECTION in a generic fn's signature under codegen —
/// `fn get[C: Container](c: C) -> C.Item { c.first() }`. The mono lowered
/// `C.Item` to the i64/`{}` default (only `segments.first()` was read),
/// mismatching the body's real return value at the LLVM verifier. Codegen
/// now resolves the projection: inside the mono `C` → its concrete type name
/// (`type_subst_names`), then the concrete impl's `type Item = <ty>` binding
/// (`assoc_type_bindings`). Covers an i64 associated type, a `Vec[i64]`
/// associated type, and a fresh-String one (a returned heap FIELD is a
/// separate ownership follow-on and not exercised here).
#[test]
fn e2e_generic_assoc_type_projection_return_codegen() {
    if let Some(out) = run_program(
            "trait Container { type Item; fn first(ref self) -> Self.Item; }\n\
             struct IntBox { v: i64 }\n\
             impl Container for IntBox { type Item = i64; fn first(ref self) -> i64 { self.v } }\n\
             struct VecMaker { }\n\
             impl Container for VecMaker { type Item = Vec[i64]; fn first(ref self) -> Vec[i64] { [10i64, 20i64, 30i64] } }\n\
             struct Greeter { }\n\
             impl Container for Greeter { type Item = String; fn first(ref self) -> String { \"hello\".to_string() } }\n\
             fn get_first[C: Container](c: C) -> C.Item { c.first() }\n\
             fn main() {\n\
                 let a = get_first(IntBox { v: 7i64 });\n\
                 println(f\"{a}\");\n\
                 let v = get_first(VecMaker {});\n\
                 println(f\"{v.len()}\");\n\
                 let s = get_first(Greeter {});\n\
                 println(s);\n\
             }",
        ) {
            assert_eq!(out, "7\n3\nhello\n");
        }
}

/// B-2026-07-03-5, `-> Self` shape: a primitive trait impl whose method
/// returns `Self` (`impl Dbl for u8 { fn dbl(self) -> Self { self + self } }`)
/// — the body's `self` must be recognized as numeric (pre-fix the hand-built
/// `Named { "u8" }` self type errored "arithmetic operator requires numeric
/// type, found 'u8'"), and the `-> Self` result resolves to the primitive.
#[test]
fn e2e_primitive_trait_impl_self_return() {
    if let Some(out) = run_program(
        "trait Dbl { fn dbl(self) -> Self; }\n\
             impl Dbl for u8  { fn dbl(self) -> Self { self + self } }\n\
             impl Dbl for i64 { fn dbl(self) -> Self { self + self } }\n\
             fn main() {\n\
             \x20   let a: u8 = 100;\n\
             \x20   let b: i64 = 21;\n\
             \x20   println(a.dbl());\n\
             \x20   println(b.dbl());\n\
             }",
    ) {
        assert_eq!(out, "200\n42\n");
    }
}

/// B-2026-07-03-24, `-> Self` + arithmetic: a generic bound whose method
/// returns `Self` and does width-sensitive arithmetic must run each width's
/// impl (pre-fix `dbl_it(i32=70000)` reused the i8 mono → overflow panic).
#[test]
fn e2e_generic_bound_primitive_self_return() {
    if let Some(out) = run_program(
        "trait Dbl { fn dbl(self) -> Self; }\n\
             impl Dbl for i8  { fn dbl(self) -> Self { self + self } }\n\
             impl Dbl for i32 { fn dbl(self) -> Self { self + self } }\n\
             impl Dbl for f64 { fn dbl(self) -> Self { self + self } }\n\
             fn dbl_it[T: Dbl](x: T) -> T { x.dbl() }\n\
             fn main() {\n\
             \x20   let a: i8 = 5;\n\
             \x20   let c: i32 = 70000;\n\
             \x20   let d: f64 = 1.5;\n\
             \x20   println(dbl_it(a));\n\
             \x20   println(dbl_it(c));\n\
             \x20   println(dbl_it(d));\n\
             }",
    ) {
        assert_eq!(out, "10\n140000\n3\n");
    }
}

/// S6b-4a (B-2026-07-03-18): an arithmetic operator on a type parameter
/// bounded by that operator's stdlib trait (`+`→Add, `-`→Sub, `*`→Mul,
/// `/`→Div, `%`→Rem, unary `-`→Neg) monomorphizes and runs. Before the fix
/// the typechecker hard-errored under `build` ("arithmetic operator
/// requires numeric type, found 'T'"), so codegen never ran — the run/build
/// divergence that blocked the stdlib `Reduce` fold-based defaults. Since
/// user operator-trait impls are forbidden (stdlib-only), each concrete
/// instantiation is a primitive numeric / String (Add) that codegen already
/// lowers post-mono; the fix is a pure typecheck admission (verified the
/// existing `T: Numeric` arm already built+ran). Covers a FREE FUNCTION
/// bound (i64 + f64 distinct monos, String concat via `Add`, unary `Neg`)
/// and a GENERIC-TRAIT default body using `+`/`*` on `-> T` method results /
/// a `let x: T` local — the `Named { "T" }`-spelled operand path.
#[test]
fn e2e_operator_on_operator_trait_bounded_type_param() {
    if let Some(out) = run_program(
        "fn add_gen[T: Add](a: T, b: T) -> T { a + b }\n\
             fn neg_gen[T: Neg](a: T) -> T { -a }\n\
             trait Foldy[T: Add + Mul] {\n\
             \x20   fn a(ref self) -> T;\n\
             \x20   fn b(ref self) -> T;\n\
             \x20   fn sum2(ref self) -> T { self.a() + self.b() }\n\
             \x20   fn prod2(ref self) -> T {\n\
             \x20       let x: T = self.a();\n\
             \x20       x * self.b()\n\
             \x20   }\n\
             }\n\
             struct IPair { x: i64, y: i64 }\n\
             impl Foldy[i64] for IPair {\n\
             \x20   fn a(ref self) -> i64 { self.x }\n\
             \x20   fn b(ref self) -> i64 { self.y }\n\
             }\n\
             struct FPair { x: f64, y: f64 }\n\
             impl Foldy[f64] for FPair {\n\
             \x20   fn a(ref self) -> f64 { self.x }\n\
             \x20   fn b(ref self) -> f64 { self.y }\n\
             }\n\
             fn main() {\n\
             \x20   println(f\"{add_gen(10, 20)}\");\n\
             \x20   println(f\"{add_gen(1.5, 2.0)}\");\n\
             \x20   println(f\"{neg_gen(5)}\");\n\
             \x20   let s = add_gen(\"op_bound_prefix_aaaa\", \"_op_bound_suffix_bbbb\");\n\
             \x20   println(f\"{s}\");\n\
             \x20   let p = IPair { x: 6, y: 7 };\n\
             \x20   println(f\"{p.sum2()}\");\n\
             \x20   println(f\"{p.prod2()}\");\n\
             \x20   let q = FPair { x: 1.5, y: 4.0 };\n\
             \x20   println(f\"{q.sum2()}\");\n\
             \x20   println(f\"{q.prod2()}\");\n\
             }",
    ) {
        // add_gen$i64=30, add_gen$f64=3.5, neg_gen$i64=-5, add_gen$String
        // concat, IPair sum2=6+7=13 / prod2=6*7=42, FPair sum2=1.5+4=5.5 /
        // prod2=1.5*4=6.
        assert_eq!(
            out,
            "30\n3.5\n-5\nop_bound_prefix_aaaa_op_bound_suffix_bbbb\n13\n42\n5.5\n6\n"
        );
    }
}

#[test]
fn e2e_generic_provider_trait_bound() {
    // `effect resource RequestCh: Channel[i64];` — a GENERIC provider trait
    // bound (B-2026-08-18-41, design.md:6071). The declaration did not
    // parse at all ("Expected Semicolon, found LeftBracket"), so the only
    // available spelling dropped the argument — and a generic provider
    // trait named without its argument is unusable, not merely imprecise:
    // every `RequestCh.send(v)` failed with "expected 'T', found 'i64'",
    // naming a type parameter the user never wrote.
    //
    // The end-to-end path is what makes this more than a parser test: the
    // trait's `T` has to be bound at typecheck AND the vtable has to
    // dispatch to `impl Channel[i64] for Echo`, so a fix that satisfied the
    // typechecker while leaving codegen keyed on the wrong thing fails
    // here.
    //
    // `reads`, not `sends`: `push` is `pub`, so its declared effects are
    // VERIFIED rather than inferred, and a `ref self` receiver seeds
    // `reads(RequestCh)` at the dispatch. This harness does not run the
    // effect checker, so `sends` passes here and `karac build` rejects it —
    // the program is spelled the way the CLI would accept.
    if let Some(out) = run_program(
        "pub trait Channel[T] { fn send(ref self, v: T) -> T; }\n\
             pub effect resource RequestCh: Channel[i64];\n\
             pub struct Echo { bump: i64 }\n\
             impl Channel[i64] for Echo { fn send(ref self, v: i64) -> i64 { v + self.bump } }\n\
             pub fn push(v: i64) -> i64 with reads(RequestCh) { RequestCh.send(v) }\n\
             fn main() {\n\
                 with_provider[RequestCh](Echo { bump: 1 }, || {\n\
                     println(f\"{push(41)}\");\n\
                 });\n\
             }",
    ) {
        assert_eq!(out, "42\n");
    }
}

#[test]
fn e2e_multi_bound_provider_resource() {
    // `effect resource UserDB: DatabaseProvider + HealthCheckable;` — the
    // MULTI-BOUND form (B-2026-08-19-3, design.md:7216, prose at :7213).
    // The parser stopped at the `+` with "Expected Semicolon, found Plus".
    //
    // End-to-end is the whole point: dispatch is ONE vtable pointer per
    // `ProviderFrame` and `R.method(..)` indexes it by `position()`, so the
    // two bounds' methods have to be laid out end-to-end in ONE vtable
    // keyed by the RESOURCE (`@VT_InMemoryDB_UserDB`) — a fix that widened
    // the declaration but left dispatch keyed on a single trait would find
    // no vtable, or index the wrong slot and call `lookup` for `healthy`.
    // Both methods are called, and from DIFFERENT bounds, so a wrong slot
    // index shows up as a wrong answer rather than a crash.
    if let Some(out) = run_program(
        "trait DatabaseProvider { fn lookup(ref self, id: i64) -> i64; }\n\
             trait HealthCheckable { fn healthy(ref self) -> bool; }\n\
             effect resource UserDB: DatabaseProvider + HealthCheckable;\n\
             struct InMemoryDB { offset: i64 }\n\
             impl DatabaseProvider for InMemoryDB {\n\
                 fn lookup(ref self, id: i64) -> i64 { id + self.offset }\n\
             }\n\
             impl HealthCheckable for InMemoryDB {\n\
                 fn healthy(ref self) -> bool { self.offset > 0 }\n\
             }\n\
             fn fetch(id: i64) -> i64 with reads(UserDB) { UserDB.lookup(id) }\n\
             fn check() -> bool with reads(UserDB) { UserDB.healthy() }\n\
             fn main() {\n\
                 with_provider[UserDB](InMemoryDB { offset: 100 }, || {\n\
                     println(f\"{fetch(7)} {check()}\");\n\
                 });\n\
             }",
    ) {
        assert_eq!(out, "107 true\n");
    }
}

/// A SECOND resource bound to the same single trait still shares the
/// trait-keyed `@VT_<U>_<T>` vtable — the multi-bound pass must not have
/// turned every resource into its own vtable. design.md's own "two
/// resources can share one trait" note (§ Why two declarations) is the
/// program shape here.
#[test]
fn e2e_two_resources_share_one_single_bound_vtable() {
    if let Some(out) = run_program(
        "trait Counter { fn get(ref self) -> i64; }\n\
             effect resource Ctr: Counter;\n\
             effect resource Audit: Counter;\n\
             struct InMem { n: i64 }\n\
             impl Counter for InMem { fn get(ref self) -> i64 { self.n } }\n\
             fn a() -> i64 with reads(Ctr) { Ctr.get() }\n\
             fn b() -> i64 with reads(Audit) { Audit.get() }\n\
             fn main() {\n\
                 with_provider[Ctr](InMem { n: 40 }, || {\n\
                     with_provider[Audit](InMem { n: 2 }, || {\n\
                         println(f\"{a() + b()}\");\n\
                     });\n\
                 });\n\
             }",
    ) {
        assert_eq!(out, "42\n");
    }
}

#[test]
fn e2e_providers_block_trait_less_ambient_resource() {
    // A TRAIT-LESS `effect resource R;` overridden only through the block
    // form. Its `(U, R)` vtable and method order are minted by the eager
    // pre-pass (`emit_ambient_provider_vtables`), which walked
    // `with_provider` CALLS only — so this failed with "no method order for
    // resource" even after the block body started compiling.
    if let Some(out) = run_program(
        "effect resource Ambient;\n\
             struct FakeAmb { n: i64 }\n\
             impl FakeAmb { fn now(ref self) -> i64 { self.n } }\n\
             fn readamb() -> i64 with reads(Ambient) { Ambient.now() }\n\
             fn main() {\n\
                 providers { Ambient => FakeAmb { n: 77 } } in {\n\
                     println(f\"amb {readamb()}\");\n\
                 }\n\
             }",
    ) {
        assert_eq!(out, "amb 77\n");
    }
}

/// B-2026-08-13-19 — the TUPLE-ELEMENT sibling of
/// `test_e2e_field_bound_out_of_local_is_a_copy`, and the interpreter is
/// the oracle here too: it read `2 1` on every line while both compiled
/// backends read `2 0`, the source EMPTIED rather than merely stale.
///
/// `uam_defensive_copy` had grown a `FieldAccess` arm and still had none
/// for `ExprKind::TupleIndex`, so the bind copied nothing while
/// `suppress_tuple_index_move_source` zeroed the source's element anyway —
/// the same advisory-`UseAfterMove` promise failing at one more place-
/// expression spelling.
///
/// Three roots, because the copy and the disarm have to be defined over the
/// SAME shapes or the pairing is not one: a bare local, a direct
/// `{ptr,len,cap}` element, and a tuple reached through a struct FIELD
/// (`h.pe.0`) — the disarm resolves that chain via `place_chain_tuple_tes`,
/// so the copy has to resolve it too.
///
/// Lines 1 and 3 are the guard: they read `2 0` against the pre-fix
/// compiler. Line 2 already passed, because a direct Vec element's move-out
/// zeroes only `cap` and a `len()` read survives that — it is here as
/// coverage that the `{ptr,len,cap}` branch of the new arm did not break,
/// not as a regression witness. The memory error that branch DID have is
/// pinned in `tests/memory_sanitizer.rs`.
#[test]
fn test_e2e_tuple_elem_bound_out_of_local_is_a_copy() {
    assert_eq!(
        run_program(
            "struct A { mut lines: Vec[String] }\n\
                 struct H { mut pe: (A, i64) }\n\
                 fn main() {\n\
                     let mut seed = A { lines: Vec.new() };\n\
                     seed.lines.push(f\"x\");\n\
                     let t: (A, i64) = (seed, 1);\n\
                     let mut r = t.0;\n\
                     r.lines.push(f\"y\");\n\
                     let chk = t.0;\n\
                     println(f\"{r.lines.len()} {chk.lines.len()}\");\n\
                     let mut v: Vec[i64] = Vec.new();\n\
                     v.push(1);\n\
                     let t2: (Vec[i64], i64) = (v, 2);\n\
                     let mut r2 = t2.0;\n\
                     r2.push(2);\n\
                     let chk2 = t2.0;\n\
                     println(f\"{r2.len()} {chk2.len()}\");\n\
                     let mut v3: Vec[String] = Vec.new();\n\
                     v3.push(f\"x\");\n\
                     let h = H { pe: (A { lines: v3 }, 3) };\n\
                     let mut r3 = h.pe.0;\n\
                     r3.lines.push(f\"y\");\n\
                     let chk3 = h.pe.0;\n\
                     println(f\"{r3.lines.len()} {chk3.lines.len()}\");\n\
                 }"
        )
        .as_deref(),
        Some("2 1\n2 1\n2 1\n"),
    );
}

#[test]
fn test_e2e_generic_at_unsigned_width_zero_extends() {
    assert_eq!(
            run_program(
                "struct Boxg[T] { v: T }\n\
                 impl[T] Boxg[T] { fn get(ref self) -> T { return self.v; } }\n\
                 fn idg[T](x: T) -> T { return x; }\n\
                 fn main() {\n\
                     println(idg(200u8));\n\
                     println(idg(60000u16));\n\
                     println(idg(4000000000u32));\n\
                     println(idg(18446744073709551615u64));\n\
                     println(idg(-56i8));\n\
                     let g: Boxg[u8] = Boxg { v: 200u8 };\n\
                     println(g.v);\n\
                     println(g.get());\n\
                     let k: Boxg[i8] = Boxg { v: -56i8 };\n\
                     println(k.v);\n\
                     let mut vg: Vec[Boxg[u8]] = Vec.new();\n\
                     vg.push(Boxg { v: 200u8 });\n\
                     println(vg[0i64].v);\n\
                     println(idg(200u8) as i64);\n\
                     println(idg(-56i8) as i64);\n\
                     let direct = 200u8;\n\
                     println(direct);\n\
                 }"
            )
            .as_deref(),
            Some("200\n60000\n4000000000\n18446744073709551615\n-56\n200\n200\n-56\n200\n200\n-56\n200\n"),
        );
}

#[test]
fn test_e2e_field_bound_out_of_local_is_a_copy() {
    // B-2026-08-13-14's VALUE side, and the oracle is the interpreter twin
    // (`tests/interpreter.rs::test_field_bound_out_of_local_is_a_copy`),
    // which reads the source's ORIGINAL contents on every line here.
    //
    // Three depths, because the unfixed compiler failed each differently:
    //   * `s.name` — a direct String field: the source kept a live
    //     `{ptr,len}` into a buffer the binding owned (cap-only zeroing), so
    //     the second read was a stale alias, and a realloc made it a
    //     use-after-free (pinned in `tests/memory_sanitizer.rs`);
    //   * `b.a` — a nested STRUCT field: `zero_struct_move_caps_mono` zeroes
    //     `len` too, so the source read back EMPTY — `0` and `""`, plausible
    //     values, no crash;
    //   * `d.lines` — a direct Vec field, the depth-1 sibling of the second.
    //
    // The scalar `b.n` read last is the control that must NOT change: the
    // disarm skip is per-field, so a sibling field of a moved-from struct
    // stays exactly as it was.
    assert_eq!(
        run_program(
            "struct S { name: String }\n\
                 struct A { lines: Vec[String] }\n\
                 struct B { a: A, n: i64 }\n\
                 fn main() {\n\
                     let k = 1;\n\
                     let s = S { name: f\"hi{k}\" };\n\
                     let n1 = s.name;\n\
                     println(n1);\n\
                     println(s.name);\n\
                     let mut v: Vec[String] = Vec.new();\n\
                     v.push(f\"x{k}\");\n\
                     let d = A { lines: v };\n\
                     let t = d.lines;\n\
                     println(t.len());\n\
                     println(d.lines.len());\n\
                     println(d.lines[0]);\n\
                     let mut v2: Vec[String] = Vec.new();\n\
                     v2.push(f\"y{k}\");\n\
                     let b = B { a: A { lines: v2 }, n: 7 };\n\
                     let u = b.a;\n\
                     println(u.lines.len());\n\
                     let w = b.a;\n\
                     println(w.lines.len());\n\
                     println(w.lines[0]);\n\
                     println(b.n);\n\
                 }"
        )
        .as_deref(),
        Some("hi1\nhi1\n1\n1\nx1\n1\n1\ny1\n7\n"),
    );
}

/// B-2026-08-29-54 — a STATIC (associated) function's by-value arguments had
/// NO caller-side owner on the compiled backends. The free-function arm
/// (`compile_call`) and the instance-method arm (`compile_method_call`) both
/// registered one; `Type.method(args)` with no receiver was the third
/// dispatch arm and registered nothing, so a fresh-temp argument's user
/// `Drop` body never ran and its buffer was orphaned.
///
/// Each case states the FREE-FUNCTION spelling's answer, because that is the
/// oracle the row is measured against: every one of these was already
/// correct for `s2(...)` and wrong only for `H.s2(...)`. Asserting absolute
/// expected output, not counts — the interpreter ran every one of these
/// bodies before the fix, so no count-based or A/B-parity check could see
/// the defect from this side.
#[test]
fn e2e_static_assoc_fn_arg_temps_own_their_values() {
    let hdr = "struct R { id: i64 }\n\
                   impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n";
    // A heap-carrying twin, for the two cases that read a field off an
    // associated call's RESULT — see the note on `static-passthrough-single-body`.
    let hdr_heap = "struct R { id: i64, s: String }\n\
                        impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
                        fn mk(i: i64) -> String { f\"pay-{i}-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\" }\n";
    for (label, src, want) in [
            // One argument is enough — not an arity or an ordering effect.
            (
                "static-one-fresh-temp",
                format!(
                    "{hdr}struct H {{ n: i64 }}\n\
                     impl H {{ fn s1(a: R) -> i64 {{ 7 }} }}\n\
                     fn main() {{ let v = H.s1(R {{ id: 1 }}); println(f\"v={{v}}\"); }}"
                ),
                "dR1\nv=7\n",
            ),
            (
                "static-three-fresh-temps",
                format!(
                    "{hdr}struct H {{ n: i64 }}\n\
                     impl H {{ fn s3(a: R, b: R, c: R) -> i64 {{ 7 }} }}\n\
                     fn main() {{\n\
                     \x20   let v = H.s3(R {{ id: 1 }}, R {{ id: 2 }}, R {{ id: 3 }});\n\
                     \x20   println(f\"v={{v}}\");\n\
                     }}"
                ),
                "dR3\ndR2\ndR1\nv=7\n",
            ),
            // MOVED LOCALS already worked before the fix — the arg has its own
            // binding to own it. Kept so a future edit cannot fix the temp case
            // by breaking this one.
            (
                "static-moved-locals",
                format!(
                    "{hdr}struct H {{ n: i64 }}\n\
                     impl H {{ fn s2(a: R, b: R) -> i64 {{ 7 }} }}\n\
                     fn main() {{\n\
                     \x20   let x = R {{ id: 1 }};\n\
                     \x20   let y = R {{ id: 2 }};\n\
                     \x20   let v = H.s2(x, y);\n\
                     \x20   println(f\"v={{v}}\");\n\
                     }}"
                ),
                "dR2\ndR1\nv=7\n",
            ),
            // A mixed call is sequenced by the two owners' relative PROGRAM
            // ORDER, not by argument position (B-2026-08-29-46's rule): `a` is
            // introduced first, so it pops last. Before the fix the temp's body
            // was simply absent and only `dR1` printed.
            (
                "static-mixed-local-and-temp",
                format!(
                    "{hdr}struct H {{ n: i64 }}\n\
                     impl H {{ fn s2(a: R, b: R) -> i64 {{ 7 }} }}\n\
                     fn main() {{\n\
                     \x20   let a = R {{ id: 1 }};\n\
                     \x20   let v = H.s2(a, R {{ id: 2 }});\n\
                     \x20   println(f\"v={{v}}\");\n\
                     }}"
                ),
                "dR2\ndR1\nv=7\n",
            ),
            // A `ref` parameter alongside an owned one: the borrow keeps its own
            // owner and only the by-value temp gains one here.
            (
                "static-ref-and-owned-params",
                format!(
                    "{hdr}struct H {{ n: i64 }}\n\
                     impl H {{ fn f(a: ref R, b: R) -> i64 {{ a.id }} }}\n\
                     fn main() {{\n\
                     \x20   let x = R {{ id: 9 }};\n\
                     \x20   let v = H.f(x, R {{ id: 2 }});\n\
                     \x20   println(f\"v={{v}}\");\n\
                     }}"
                ),
                "dR2\ndR9\nv=9\n",
            ),
            // The impl block's target may be an ENUM — same dispatch arm.
            (
                "static-on-enum-type",
                format!(
                    "{hdr}enum E {{ A, B }}\n\
                     impl E {{ fn f(a: R) -> i64 {{ 7 }} }}\n\
                     fn main() {{ let v = E.f(R {{ id: 1 }}); println(f\"v={{v}}\"); }}"
                ),
                "dR1\nv=7\n",
            ),
            // A fn-RETURNED Drop temp, the shape B-2026-07-01-7 fixed for free
            // functions.
            (
                "static-fn-returned-temp",
                format!(
                    "{hdr}fn mkr(i: i64) -> R {{ R {{ id: i }} }}\n\
                     struct H {{ n: i64 }}\n\
                     impl H {{ fn s1(a: R) -> i64 {{ 7 }} }}\n\
                     fn main() {{ let v = H.s1(mkr(1)); println(f\"v={{v}}\"); }}"
                ),
                "dR1\nv=7\n",
            ),
            // PASSTHROUGH — the callee hands the argument back, so the caller's
            // RESULT binding owns it and the caller must NOT register a second
            // body. Guards the direction this fix could most easily overshoot
            // in, and matches the free-function spelling exactly.
            //
            // `hdr_heap`, not `hdr`: reading a field off an
            // associated-call RESULT (`x.id` where `x = H.id(..)`) fails to
            // codegen for an ALL-SCALAR struct — "cannot resolve field 'id' on
            // this receiver", a pre-existing gap unrelated to this row and
            // measured identical before and after the fix. A heap-carrying
            // field records the binding's type and compiles, so these two cases
            // use it rather than working around a defect this test is not about.
            (
                "static-passthrough-single-body",
                format!(
                    "{hdr_heap}struct H {{ n: i64 }}\n\
                     impl H {{ fn id(a: R) -> R {{ a }} }}\n\
                     fn main() {{ let x = H.id(R {{ id: 1, s: mk(1) }}); println(f\"x={{x.id}}\"); }}"
                ),
                "x=1\ndR1\n",
            ),
            // CONDITIONAL return: the callee returns a DIFFERENT value on this
            // path, so the argument dies inside the call and its body must run.
            // Standing the caller down on the union-over-return-sites predicate
            // would lose it — which is why the escape test is the two-predicate
            // form B-2026-08-28-70 settled on.
            //
            // The FREE-FUNCTION spelling of this shape prints no `dR1` on either
            // backend — it frees the buffer and loses the body — so this is one
            // case where the associated form is correct and its twin is not.
            // Asserting the correct answer here deliberately; the free-function
            // half is filed separately rather than copied.
            (
                "static-conditional-return-arg-dies",
                format!(
                    "{hdr_heap}struct H {{ n: i64 }}\n\
                     impl H {{ fn pick(a: R, k: bool) -> R {{ if k {{ return R {{ id: 98, s: mk(98) }}; }} a }} }}\n\
                     fn main() {{ let x = H.pick(R {{ id: 1, s: mk(1) }}, true); println(f\"x={{x.id}}\"); }}"
                ),
                "dR1\nx=98\ndR98\n",
            ),
            // A GENERIC associated fn already worked — it lowers through the
            // monomorph path, which calls the registrar. Pinned so the two arms
            // cannot drift apart again.
            (
                "static-generic-already-worked",
                format!(
                    "{hdr}struct H {{ n: i64 }}\n\
                     impl H {{ fn g[T](a: T, k: i64) -> i64 {{ k }} }}\n\
                     fn main() {{ let v = H.g(R {{ id: 1 }}, 7); println(f\"v={{v}}\"); }}"
                ),
                "dR1\nv=7\n",
            ),
        ] {
            assert_eq!(run_program(&src).as_deref(), Some(want), "case {label}");
        }
}

/// B-2026-08-30-20 — an argument PRODUCED BY an associated call has an
/// owner, so its `Drop` body runs and its buffer is freed.
///
/// `track_inline_owned_aggregate_arg` matched a bare `ExprKind::Identifier`
/// callee only, so `s1(H.mkr(1))` — a two-segment `Path` callee — was
/// classified as carrying no user `Drop`. The interpreter's classifier had
/// the same hole one arm over, recognising a qualified UNIT VARIANT but not
/// an associated fn, so ALL FOUR surfaces agreed on the omission. That is
/// why this is an absolute expectation against the free-function producer
/// rather than an A/B parity check: with every backend wrong in the same
/// way, no cross-backend comparison could see it, and the leak is
/// `-O0`-only (at `-O2` the callee ignores its parameter and LLVM deletes
/// the allocation, the same masking that mis-graded B-2026-08-29-54).
#[test]
fn e2e_assoc_call_produced_argument_owns_its_value() {
    let hdr = "struct R { id: i64, s: String }\n\
                   impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
                   fn mk(i: i64) -> String { return f\"pay-{i}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa\" }\n\
                   fn s1(a: R) -> i64 { return 7 }\n";
    // Associated producer — the row's shape.
    let Some(assoc) = run_program(&format!(
        "{hdr}struct H {{ n: i64 }}\n\
             impl H {{ fn mkr(i: i64) -> R {{ return R {{ id: i, s: mk(i) }} }} }}\n\
             fn main() {{ let v = s1(H.mkr(1)); println(f\"v={{v}}\") }}"
    )) else {
        return;
    };
    // Free producer — the oracle, correct throughout.
    let Some(free) = run_program(&format!(
        "{hdr}fn mkr2(i: i64) -> R {{ return R {{ id: i, s: mk(i) }} }}\n\
             fn main() {{ let v = s1(mkr2(1)); println(f\"v={{v}}\") }}"
    )) else {
        return;
    };
    assert_eq!(free, "dR1\nv=7\n", "the free-function oracle itself moved");
    assert_eq!(
        assoc, free,
        "an associated-call producer must own its argument exactly as a free one does"
    );
}

#[test]
fn test_e2e_generic_method_some_over_field_pop() {
    // B-2026-07-12-6 (typecheck) — `Some(self.items.pop())` inside a generic
    // method `impl[T] Stack[T]` over-nested by one Option layer and was
    // rejected at typecheck. This E2E confirms the once-rejected shape now
    // both compiles AND runs correctly (interp == JIT == AOT): the value
    // flows through the double `Option[Option[T]]` unchanged. The early-
    // return `None` path and the `Some(field-pop)` path are both exercised.
    if let Some(out) = run_program(
        "struct Stack[T] { items: Vec[T] }\n\
             impl[T] Stack[T] {\n\
                 fn new() -> Stack[T] { Stack { items: Vec.new() } }\n\
                 fn push(mut ref self, v: T) { self.items.push(v); }\n\
                 fn pop_wrapped(mut ref self) -> Option[Option[T]] {\n\
                     if self.items.len() == 0 { return None; }\n\
                     Some(self.items.pop())\n\
                 }\n\
             }\n\
             fn main() {\n\
                 let mut s: Stack[i64] = Stack.new();\n\
                 s.push(10);\n\
                 s.push(20);\n\
                 match s.pop_wrapped() {\n\
                     Some(inner) => {\n\
                         match inner {\n\
                             Some(v) => { println(f\"got {v}\"); }\n\
                             None => { println(\"inner none\"); }\n\
                         }\n\
                     }\n\
                     None => { println(\"outer none\"); }\n\
                 }\n\
                 let _ = s.pop_wrapped();\n\
                 match s.pop_wrapped() {\n\
                     Some(_) => { println(\"unexpected some\"); }\n\
                     None => { println(\"empty -> outer none\"); }\n\
                 }\n\
             }",
    ) {
        assert_eq!(out, "got 20\nempty -> outer none\n");
    }
}

#[test]
fn test_e2e_generic_fn_divergent_loop_tail() {
    // B-2026-07-12-8 — a GENERIC (monomorphized) function `fn f[T](..) -> i64`
    // whose body TAIL is a divergent `loop { .. return <value>; .. }` (exits
    // only via `return`) failed module verification: the mono emitted a
    // spurious `ret void` at the loop's unreachable fall-through in an
    // i64-returning fn ("Function return type does not match operand type of
    // return inst"). The non-generic sibling, and a generic `while`+tail or
    // `loop{..break;}`+tail, all compiled fine — the bare divergent `loop`
    // tail was the trigger. Fix: emit `unreachable` (not `ret void`) at the
    // no-value tail of a non-void mono, mirroring `compile_function`. Covers
    // a bare `loop{return}`, a conditional-return loop, and confirms a VOID
    // generic `loop{return;}` still returns cleanly.
    if let Some(out) = run_program(
        "fn f[T](x: T) -> i64 { loop { return 5; } }\n\
             fn g[T](x: T) -> i64 { let mut i = 0; loop { if i >= 3 { return i; } i = i + 1; } }\n\
             fn h[T](x: T) { loop { return; } }\n\
             fn main() {\n\
                 println(f\"{f(99)}\");\n\
                 println(f\"{g(99)}\");\n\
                 h(99);\n\
                 println(\"ok\");\n\
             }",
    ) {
        assert_eq!(out, "5\n3\nok\n");
    }
}

#[test]
fn test_e2e_bf16_arithmetic_compare_neg_through_fn_boundary() {
    // Runtime sibling of test_ir_bf16_widen_to_f64_routes_through_f32:
    // bf16 add/mul/div, compare, and unary negation on values that cross
    // a fn boundary (so the mid-end can't fold them away). This is the
    // shape that killed the macOS LLJIT parity lane (B-2026-07-22-1) —
    // on that lane this test exercises runtime ISel of the promoted
    // f32-compute emission. All values are exact in bf16.
    if let Some(run) = run_program_capturing(
        "fn step(x: bf16) -> bf16 {\n\
                 x + 0.5bf16\n\
             }\n\
             fn main() {\n\
                 let y = step(1.25bf16);\n\
                 println(y);\n\
                 let n = -y;\n\
                 println(n);\n\
                 if n < 0.0bf16 { println(\"neg\") }\n\
                 println(y * 2.0bf16);\n\
                 println(y / 0.5bf16);\n\
             }",
    ) {
        assert_eq!(
            run.stdout, "1.75\n-1.75\nneg\n3.5\n3.5\n",
            "bf16 arithmetic program diverged (status: {}, stderr: {:?})",
            run.status, run.stderr
        );
    }
}

#[test]
fn test_e2e_with_provider_trait_less_user_resource() {
    // Gap (c): a *trait-less* user resource (`effect resource R;` with no
    // `: T`) overridden by a statically-typed provider. Unlike a prelude
    // ambient resource (`Clock`), a trait-less user resource has no
    // canonical method order and no FFI default — its method order is
    // derived from the override type's inherent impl, and every call
    // dispatches through the active override. Two methods exercise the
    // vtable-index path, and the call lives in a *separate function*
    // (`tally`) so dispatch is cross-boundary (the runtime provider
    // stack, not lexical scope). Before the fix this errored at codegen
    // with "resource 'AuditLog' has no provider trait". `karac run` of
    // the same source prints 12.
    let out = run_program(
        r#"
effect resource AuditLog;
struct FakeLog { n: i64 }
impl FakeLog {
    fn count(self) -> i64 { self.n }
    fn bump(self) -> i64 { self.n + 7 }
}
fn tally() -> i64 reads(AuditLog) {
    AuditLog.count() + AuditLog.bump()
}
fn main() reads(AuditLog) {
    with_provider[AuditLog](FakeLog { n: 1 }, || {
        println(tally());
    });
}
"#,
    );
    if let Some(out) = out {
        // count() = n = 1; bump() = n + 7 = 8; sum = 9.
        assert_eq!(out.trim(), "9");
    }
}

#[test]
fn test_e2e_with_provider_trait_less_call_ctor() {
    // Sibling to the gap-(c) test above, but the provider is bound to a
    // *constructor call* (`let p = make_log();`) rather than an inline
    // struct literal. The eager ambient-vtable pre-pass must resolve `p`'s
    // type from `make_log`'s declared return type — it used to recognize
    // only struct-literal providers, so this errored at codegen with "no
    // method order for resource". `karac run` prints 9.
    let out = run_program(
        r#"
effect resource AuditLog;
struct FakeLog { n: i64 }
impl FakeLog { fn count(self) -> i64 { self.n } }
fn make_log() -> FakeLog { FakeLog { n: 9 } }
fn tally() -> i64 reads(AuditLog) { AuditLog.count() }
fn main() reads(AuditLog) {
    let p = make_log();
    with_provider[AuditLog](p, || { println(tally()); });
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "9");
    }
}

/// B-2026-08-13-10 — soundness twin of the IR gate above. The hint is
/// advisory, but an unrolled loop must still compute exactly what the
/// rolled one did, so this asserts the VALUE rather than the shape: a
/// name-bounded reduction over a runtime-opaque array, whose result would
/// change if the unroll dropped or duplicated an iteration.
///
/// Shaped after kata #265's kernel — a three-way carried reduction
/// (min, its index, second-min), which is the loop that motivated the
/// row and the one whose branch LLVM only if-converts once fully
/// unrolled.
#[test]
fn test_e2e_const_local_bound_unroll_is_sound() {
    let Some(out) = run_program(
        "fn main() {\n\
             \x20   let k = 32i64;\n\
             \x20   let mut v: Vec[i64] = Vec.new();\n\
             \x20   let mut state = 7i64;\n\
             \x20   let mut z = 0i64;\n\
             \x20   while z < k {\n\
             \x20       state = (state * 1103515245i64 + 12345i64) & 2147483647i64;\n\
             \x20       v.push((state / 65536i64) % 40i64 + 1i64);\n\
             \x20       z = z + 1i64;\n\
             \x20   }\n\
             \x20   let mut min1 = 1000000000i64;\n\
             \x20   let mut idx1 = -1i64;\n\
             \x20   let mut min2 = 1000000000i64;\n\
             \x20   let mut j = 0i64;\n\
             \x20   while j < k {\n\
             \x20       if v[j] < min1 { min2 = min1; min1 = v[j]; idx1 = j; }\n\
             \x20       else { if v[j] < min2 { min2 = v[j]; } }\n\
             \x20       j = j + 1i64;\n\
             \x20   }\n\
             \x20   println(f\"{min1} {idx1} {min2}\");\n\
             }",
    ) else {
        return;
    };
    // Matches `karac run --interp` on the identical source.
    assert_eq!(out, "3 15 4\n");
}

// ── Generic monomorphization ─────────────────────────────────────

#[test]
fn test_ir_generic_identity_function() {
    let ir = ir_for(
        r#"
fn identity[T](x: T) -> T { x }
fn main() {
    let a = identity(42);
    println(a);
}
"#,
    );
    // A specialization for i64 should be generated.
    assert!(
        ir.contains("identity$i64"),
        "should contain mangled i64 specialization"
    );
    assert!(ir.contains("define"), "should define at least one function");
}

#[test]
fn test_ir_generic_two_params() {
    let ir = ir_for(
        r#"
fn add_generic[T](a: T, b: T) -> T { a + b }
fn main() {
    let x = add_generic(3, 4);
    println(x);
}
"#,
    );
    assert!(
        ir.contains("add_generic$i64"),
        "should contain i64 specialization"
    );
}

#[test]
fn test_ir_generic_two_type_params() {
    let ir = ir_for(
        r#"
fn first[A, B](a: A, b: B) -> A { a }
fn main() {
    let x = first(10, 3.14);
    println(x);
}
"#,
    );
    // Should generate first$i64$f64
    assert!(
        ir.contains("first$i64$f64"),
        "should contain dual-param specialization"
    );
}

#[test]
fn test_ir_generic_multiple_uses_same_type() {
    // Calling a generic function twice with the same type should only
    // generate one specialization (deduplicated by mangle name).
    let ir = ir_for(
        r#"
fn negate[T](x: T) -> T { 0 - x }
fn main() {
    let a = negate(5);
    let b = negate(3);
    println(a + b);
}
"#,
    );
    // Count how many times the definition appears (not calls).
    let define_count = ir
        .lines()
        .filter(|l| l.contains("define") && l.contains("negate$i64"))
        .count();
    assert_eq!(
        define_count, 1,
        "should only generate one i64 specialization"
    );
}

#[test]
fn test_ir_generic_different_types_two_specializations() {
    let ir = ir_for(
        r#"
fn square[T](x: T) -> T { x * x }
fn main() {
    let a = square(3);
    let b = square(2.0);
    println(a);
}
"#,
    );
    // Both i64 and f64 specializations should be present.
    assert!(ir.contains("square$i64"), "should have i64 specialization");
    assert!(ir.contains("square$f64"), "should have f64 specialization");
    let define_count = ir
        .lines()
        .filter(|l| l.contains("define") && l.contains("square$"))
        .count();
    assert_eq!(
        define_count, 2,
        "should generate exactly two specializations"
    );
}

#[test]
fn test_e2e_const_generic_param_in_body() {
    // Const generics slice 4: const-param identifier reference
    // resolves at codegen body lowering. `fn f[const N: i64](x: i64) -> i64 { x + N }`
    // called with `f[3](10)` returns 13; `f[7](10)` returns 17.
    // The compile_expr Identifier branch consults `const_subst`
    // and emits the matching LLVM constant via
    // `compile_primitive_const`.
    let out = run_program(
        r#"
fn f[const N: i64](x: i64) -> i64 { x + N }
fn main() {
    println(f[3](10));
    println(f[7](10));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "13\n17");
    }
}

#[test]
fn test_e2e_const_generic_param_in_larger_expression() {
    // Const generics slice 4: const-param embedded in a larger
    // expression. `fn g[const N: i64]() -> i64 { N * 2 + 1 }`
    // called with `g[7]()` returns 15.
    let out = run_program(
        r#"
fn g[const N: i64]() -> i64 { N * 2 + 1 }
fn main() {
    println(g[7]());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "15");
    }
}

#[test]
fn test_ir_const_generic_param_distinct_monos_in_body() {
    // Slice 4 + slice 1b: each distinct const-arg produces a
    // distinct compiled mono symbol AND the body of each mono
    // emits a different LLVM constant for the const-param. The
    // IR for `f[3]` should contain the literal 3 in its body;
    // the IR for `f[7]` should contain 7.
    let ir = ir_for(
        r#"
fn f[const N: i64](x: i64) -> i64 { x + N }
fn main() {
    let _ = f[3](10);
    let _ = f[7](10);
}
"#,
    );
    // `f` has only the const-param `N` as a generic (no T), so
    // the mangled symbol is `f$<const-N-value>` — no type-arg
    // token in the middle.
    assert!(
        ir.contains("f$3i64"),
        "expected `f$3i64` mono symbol, IR:\n{}",
        ir
    );
    assert!(
        ir.contains("f$7i64"),
        "expected `f$7i64` mono symbol, IR:\n{}",
        ir
    );
}

#[test]
fn test_ir_const_generic_mono_key_disambiguation() {
    // Const generics slice 1b (2026-05-11). Two calls to the same
    // generic function with the same type-arg but distinct
    // const-args (`make_arr[i64, 4]()` vs `make_arr[i64, 8]()`)
    // produce two distinct compiled symbols in the LLVM module:
    // the mango-key walks const params alongside type params and
    // appends each const value's mangled token. Without slice 1b,
    // both calls collapse to a single `make_arr$i64` symbol — the
    // latent bug `mangle_mono_name` had pre-slice-1b.
    let ir = ir_for(
        r#"
fn make_arr[T, const N: i64]() -> i64 { 42 }
fn main() {
    let _ = make_arr[i64, 4]();
    let _ = make_arr[i64, 8]();
}
"#,
    );
    assert!(
        ir.contains("make_arr$i64$4i64"),
        "expected `make_arr$i64$4i64` specialization in IR, got:\n{}",
        ir
    );
    assert!(
        ir.contains("make_arr$i64$8i64"),
        "expected `make_arr$i64$8i64` specialization in IR, got:\n{}",
        ir
    );
    let define_count = ir
        .lines()
        .filter(|l| l.contains("define") && l.contains("make_arr$"))
        .count();
    assert_eq!(
        define_count, 2,
        "should generate two distinct specializations for N=4 and N=8"
    );
}

#[test]
fn test_e2e_generic_identity() {
    let out = run_program(
        r#"
fn identity[T](x: T) -> T { x }
fn main() {
    println(identity(99));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "99");
    }
}

#[test]
fn test_e2e_generic_max() {
    let out = run_program(
        r#"
fn max_val[T](a: T, b: T) -> T {
    if a > b { a } else { b }
}
fn main() {
    println(max_val(3, 7));
    println(max_val(10, 2));
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["7", "10"]);
    }
}

#[test]
fn test_e2e_generic_swap_via_tuple() {
    let out = run_program(
        r#"
fn swap[T](a: T, b: T) -> (T, T) { (b, a) }
fn main() {
    let result = swap(1, 2);
    println(result.0);
    println(result.1);
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["2", "1"]);
    }
}

#[test]
fn test_e2e_generic_higher_order_chain() {
    // Generic function calling another generic function.
    let out = run_program(
        r#"
fn double_val[T: Add](x: T) -> T { x + x }
fn quad[T: Add](x: T) -> T { double_val(double_val(x)) }
fn main() {
    println(quad(3));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "12");
    }
}

/// B-2026-06-21-1 (sibling of B-2026-06-20-1): a bare named `fn` bound to a
/// local first — `let g = doubler;` — then (a) passed to a `Fn(...)`-typed
/// parameter and (b) called directly through the local. Both forms, and the
/// `Fn(...)`-annotated binding, must reify the fn name into the closure
/// fat-pointer ABI + register the local in `closure_fn_types`. Before this
/// the local held a raw `ptr`: the pass-to-param form failed LLVM
/// verification and the direct call silently returned 0.
#[test]
fn fn_value_let_bound_named_fn_passed_and_called() {
    let out = run_program(
        "fn doubler(n: i64) -> i64 { n * 2i64 }\n\
             fn apply(f: Fn(i64) -> i64, x: i64) -> i64 { f(x) }\n\
             fn main() {\n\
                 let g = doubler;\n\
                 let h: Fn(i64) -> i64 = doubler;\n\
                 println(f\"{apply(g, 10i64)}\");\n\
                 println(f\"{h(11i64)}\");\n\
             }\n",
    );
    assert_eq!(out.as_deref(), Some("20\n22\n"));
}

#[test]
fn test_e2e_bug8_call_chain_field_assoc_call() {
    // Sibling shape: associated-function call (`Node.make()`)
    // returning a shared struct, then bare `.val` access. The
    // `Path { segments: [Type, fn] }` callee shape flows through
    // the same `fn_return_type_names` registration as the free-fn
    // path, so the call-like recognizer picks it up identically.
    let out = run_program(
        r#"
shared struct Node { val: i64 }
impl Node {
    fn make() -> Node {
        let n = Node { val: 7 };
        n
    }
}
fn main() {
    println(Node.make().val);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "7");
    }
}

#[test]
fn test_e2e_soa_layout_named_param_base_is_aos_mono_is_soa() {
    // Per-layout monomorphization slice 5 (origin-only `soa_layouts`): a
    // by-value `Vec[E]` param whose NAME coincides with a `layout` block
    // (`es`) no longer lowers SoA by name. The physical layout is the value
    // carrier of the ARGUMENT's binding site, not the param name:
    //   - `sumall(es)`    — `es` is the SoA local (matches `layout es`), so
    //                       the call routes to a per-layout monomorph whose
    //                       `es` param is the 4-field SoA struct.
    //   - `sumall(plain)` — `plain` is an ordinary AoS `Vec[E]`, so the call
    //                       routes to the BASE symbol, whose `es` param is
    //                       AoS `{ptr,len,cap}`.
    // Before slice 5 the name-keyed `soa_value_param_layout` lowered the
    // base `sumall`'s `es` param SoA on the name match alone, so the AoS
    // `sumall(plain)` call marshalled a 3-field AoS Vec into a 4-field SoA
    // slot — an LLVM "Call parameter type does not match function signature"
    // verification failure (the footgun this slice retires). Each path reads
    // its own grouping correctly: es → (1+2)+(3+4)=10, plain → 100+200=300.
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
    let mut plain: Vec[E] = Vec.new();
    plain.push(E { x: 100.0, y: 0.0 });
    plain.push(E { x: 200.0, y: 0.0 });
    println(sumall(es));
    println(sumall(plain));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
                out.trim(),
                "10\n300",
                "layout-named by-value param: SoA arg routes to a SoA mono, AoS arg to the AoS base symbol"
            );
    }
}

#[test]
fn test_ir_user_impl_method_emitted() {
    // User impl methods land in the module as LLVM functions named
    // `Type.method`. Regression guard for the impl-block codegen pass.
    // CR-202 slice 5b: companion `impl PartialEq for Point` keeps the
    // typecheck pass clean now that `Eq: PartialEq`.
    let ir = ir_for(
        r#"
struct Point { x: i64, y: i64 }
impl PartialEq for Point {
    fn eq(ref self, other: ref Point) -> bool { self.x == other.x and self.y == other.y }
}
impl Eq for Point {
    fn eq(self, other: Point) -> bool { self.x == other.x and self.y == other.y }
}
fn main() {}
"#,
    );
    assert!(
        ir.contains("@\"Point.eq\"") || ir.contains("@Point.eq"),
        "expected Point.eq function definition in IR, got:\n{}",
        ir
    );
}

#[test]
fn test_ir_trait_impl_assoc_fn_emitted() {
    // `impl Default for Foo { fn default() -> Foo { ... } }` —
    // associated function (no `self` receiver). Same convention as
    // method impls: emitted as `Foo.default` LLVM symbol so
    // `Foo.default()` and lowered bare `let w: Foo = default()` both
    // dispatch through `Path([Foo, default])`.
    let ir = ir_for(
        r#"
trait Default {
    fn default() -> Self;
}
struct Foo { value: i64 }
impl Default for Foo {
    fn default() -> Foo { Foo { value: 42 } }
}
fn main() {}
"#,
    );
    assert!(
        ir.contains("@\"Foo.default\"") || ir.contains("@Foo.default"),
        "expected Foo.default function definition in IR, got:\n{}",
        ir
    );
}

#[test]
fn test_e2e_concrete_type_prefix_assoc_fn() {
    // `Foo.default()` (UFCS / type-prefixed) dispatches directly to the
    // `Foo.default` LLVM function emitted by the impl-block pass.
    let out = run_program(
        r#"
trait Default {
    fn default() -> Self;
}
struct Foo { value: i64 }
impl Default for Foo {
    fn default() -> Foo { Foo { value: 7 } }
}
fn main() {
    let f = Foo.default();
    println(f.value);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "7");
    }
}

#[test]
fn test_e2e_bare_assoc_fn_lowering_concrete() {
    // `let w: Foo = default()` — typechecker resolves the bare call via
    // expected-type inference; lowering rewrites it to `Foo.default()`,
    // which codegen dispatches through the existing impl path.
    let out = run_program(
        r#"
trait Default {
    fn default() -> Self;
}
struct Foo { value: i64 }
impl Default for Foo {
    fn default() -> Foo { Foo { value: 99 } }
}
fn main() {
    let f: Foo = default();
    println(f.value);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "99");
    }
}

#[test]
fn test_ir_derive_default_assoc_fn_emitted() {
    // `#[derive(Default)]` synthesizes an inherent `Config.default`
    // impl in desugar; codegen emits it as the `Config.default`
    // symbol, same as a hand-written impl.
    let ir = ir_for_desugared(
        r#"
#[derive(Default)]
struct Config { timeout_ms: i64, verbose: bool }
fn main() {
    let c = Config.default();
    println(c.timeout_ms);
}
"#,
    );
    assert!(
        ir.contains("@\"Config.default\"") || ir.contains("@Config.default"),
        "expected synthesized Config.default in IR, got:\n{}",
        ir
    );
}

#[test]
fn test_e2e_bare_assoc_fn_with_arg_lowering() {
    // Trait method with a non-Self parameter.
    let out = run_program(
        r#"
trait FromI64 {
    fn from_i64(n: i64) -> Self;
}
struct Wrap { v: i64 }
impl FromI64 for Wrap {
    fn from_i64(n: i64) -> Wrap { Wrap { v: n + 1 } }
}
fn main() {
    let w: Wrap = from_i64(41);
    println(w.v);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "42");
    }
}

#[test]
fn test_e2e_user_impl_eq_drives_equality() {
    // End-to-end: `a == b` on user type lowers to `Point.eq(a, b)`,
    // which routes through the codegen-compiled impl method.
    // CR-202 slice 5b: companion `impl PartialEq for Point` satisfies
    // the new `Eq: PartialEq` supertrait edge.
    let out = run_program(
        r#"
struct Point { x: i64, y: i64 }
impl PartialEq for Point {
    fn eq(ref self, other: ref Point) -> bool {
        self.x == other.x and self.y == other.y
    }
}
impl Eq for Point {
    fn eq(self, other: Point) -> bool {
        self.x == other.x and self.y == other.y
    }
}
fn main() {
    let a = Point { x: 1, y: 2 };
    let b = Point { x: 1, y: 2 };
    let c = Point { x: 9, y: 9 };
    println(a == b);
    println(a != c);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "true\ntrue");
    }
}

// ── Generic associated types (GATs) codegen ───────────────────
//
// The GAT typechecker/resolver slices (design.md § Generic
// associated types) are pinned by 46 tests in `tests/typechecker.rs`,
// but the runtime lowering of a projected GAT return had NO codegen
// coverage. These two E2E tests exercise it: a call whose signature
// return is `Self.Mapped[..]` must lower to the impl binding's
// concrete right-hand side and run correctly. The first is a concrete
// impl (`Mapped[U] = Vec[U]`); the second is the harder generic impl
// where the RHS mixes the impl's own param `T` with the GAT param `U`
// (`Mapped[U] = Pair[T, U]`), so codegen must apply the two-sided
// substitution the resolver computes, not just a single map.

#[test]
fn test_e2e_gat_concrete_impl_projection_returns_vec() {
    // `Doubler` binds `Mapped[U] = Vec[U]`; `map_to_i64(ref self) ->
    // Self.Mapped[i64]` resolves to `Vec[i64]`. The returned vector is
    // indexed at the call site, proving the projected type lowered to a
    // real `Vec[i64]` (not an opaque/aborted shape).
    let out = run_program(
        r#"
trait Functor {
    type Mapped[U];
    fn map_to_i64(ref self) -> Self.Mapped[i64];
}
struct Doubler {}
impl Functor for Doubler {
    type Mapped[U] = Vec[U];
    fn map_to_i64(ref self) -> Vec[i64] {
        let mut v: Vec[i64] = Vec.new();
        v.push(21i64);
        v.push(42i64);
        v
    }
}
fn caller(d: ref Doubler) -> Vec[i64] {
    d.map_to_i64()
}
fn main() {
    let dbl = Doubler {};
    let result = caller(dbl);
    println(f"{result[0]}");
    println(f"{result[1]}");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "21\n42");
    }
}

#[test]
fn test_e2e_gat_generic_impl_two_sided_substitution() {
    // `impl[T] Functor for Wrapper[T]` with `Mapped[U] = Pair[T, U]`:
    // resolving `Self.Mapped[i64]` for `Wrapper[bool]` must substitute
    // BOTH the impl param `T = bool` and the GAT param `U = i64`,
    // yielding `Pair[bool, i64]`. Reads back both fields to prove each
    // side of the substitution reached codegen intact.
    let out = run_program(
        r#"
trait Functor {
    type Mapped[U];
    fn rewrap(ref self) -> Self.Mapped[i64];
}
struct Pair[A, B] { a: A, b: B }
struct Wrapper[T] { x: T }
impl[T] Functor for Wrapper[T] {
    type Mapped[U] = Pair[T, U];
    fn rewrap(ref self) -> Pair[T, i64] {
        Pair { a: self.x, b: 99i64 }
    }
}
fn main() {
    let w = Wrapper { x: true };
    let p = w.rewrap();
    let msg = if p.a { "yes" } else { "no" };
    println(msg);
    println(f"{p.b}");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "yes\n99");
    }
}

#[test]
fn test_ir_struct_destructure_bound_field_freed() {
    // A bound heap field is freed via its binding's scope-exit cleanup.
    let src = format!(
            "{STRUCT_DESTRUCTURE_PRELUDE}fn main() {{\n    let Point {{ items, count }} = make();\n    println(items.len() + count);\n}}\n"
        );
    let ir = ir_for_with_ownership(&src);
    assert!(
        main_free_count(&ir) >= 1,
        "bound Vec field must be freed once; got:\n{ir}"
    );
}

#[test]
fn test_ir_struct_destructure_unbound_field_freed() {
    // An explicitly-discarded heap field (`items: _`) is stashed in a
    // synthetic discard slot and freed there.
    let src = format!(
            "{STRUCT_DESTRUCTURE_PRELUDE}fn main() {{\n    let Point {{ items: _, count }} = make();\n    println(count);\n}}\n"
        );
    let ir = ir_for_with_ownership(&src);
    assert!(
        ir.contains("__destructure_discard_") && main_free_count(&ir) >= 1,
        "unbound Vec field must be discard-freed; got:\n{ir}"
    );
}

// ── Theme 6: provider vtable emission (sub-step 2) ────────────────────
//
// Structural tests pinning that codegen emits a static `@VT_<U>_<T>`
// global per `impl T for U` where `T` is bound to some `effect resource
// R: T`. The fully-wired dispatch (sub-steps 3+4 — `with_provider[R]`
// lowering + `R.method(...)` indirect call) is out of scope for this
// commit; these tests verify the foundation only.

#[test]
fn test_provider_vtable_emitted_for_provider_trait_impl() {
    let ir = ir_for(
        "pub trait Recorder { fn record(value: i64); }\n\
             pub struct Counter { n: i64 }\n\
             impl Recorder for Counter { fn record(value: i64) { } }\n\
             pub effect resource Metric: Recorder;\n\
             fn main() { }",
    );
    assert!(
        ir.contains("@VT_Counter_Recorder"),
        "expected vtable global @VT_Counter_Recorder; IR: {}",
        ir
    );
}

#[test]
fn test_provider_vtable_skipped_for_non_provider_trait_impl() {
    // No `effect resource` declaration → the trait isn't a provider
    // trait → no vtable emitted, even though `impl Foo for Bar`
    // exists.
    let ir = ir_for(
        "pub trait Foo { fn f(value: i64); }\n\
             pub struct Bar { n: i64 }\n\
             impl Foo for Bar { fn f(value: i64) { } }\n\
             fn main() { }",
    );
    assert!(
        !ir.contains("@VT_Bar_Foo"),
        "expected no vtable global for non-provider trait; IR: {}",
        ir
    );
}

#[test]
fn test_provider_vtable_one_per_impl_target() {
    // Two impls of the same provider trait on different target types
    // produce two distinct vtables.
    let ir = ir_for(
        "pub trait Recorder { fn record(value: i64); }\n\
             pub struct CounterA { n: i64 }\n\
             pub struct CounterB { n: i64 }\n\
             impl Recorder for CounterA { fn record(value: i64) { } }\n\
             impl Recorder for CounterB { fn record(value: i64) { } }\n\
             pub effect resource Metric: Recorder;\n\
             fn main() { }",
    );
    assert!(
        ir.contains("@VT_CounterA_Recorder"),
        "expected @VT_CounterA_Recorder; IR: {}",
        ir
    );
    assert!(
        ir.contains("@VT_CounterB_Recorder"),
        "expected @VT_CounterB_Recorder; IR: {}",
        ir
    );
}

#[test]
fn test_state_struct_type_emitted_for_network_boundary_function() {
    // A function that calls into a `sends(Network)` callee gets a
    // `%kara.state.driver` LLVM struct type emitted at module setup.
    // First field is the i32 yield-point tag.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver() { fetch(); }",
    );
    assert!(
        ir.contains("%kara.state.driver"),
        "expected state struct type %kara.state.driver to appear in IR:\n{ir}"
    );
    // The emitted struct definition line carries the tag + fields list.
    // Match `%kara.state.driver = type { i32` to pin the tag at field 0.
    assert!(
        ir.contains("%kara.state.driver = type { i32"),
        "state struct's first field must be i32 yield-point tag:\n{ir}"
    );
}

// ── Phase 6 line 26 slice 8c: state-struct constructor helper ──────
//
// For each network-boundary function, codegen emits a no-arg helper
// `define internal ptr @__kara_state_new_<fn_key>()` that mallocs a
// fresh state struct, initializes the i32 yield-point tag at field
// 0 to 0, and returns the heap pointer. Caller-side wiring (slice
// 8d+) replaces each direct call to a network-boundary fn with a
// constructor call + initial poll-fn invocation.

#[test]
fn test_state_constructor_emitted_for_network_boundary_function() {
    // A free fn `driver` that calls a network-effect callee gets a
    // `define internal ptr @__kara_state_new_driver()` constructor.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver() { fetch(); }",
    );
    let define_line = ir
        .lines()
        .find(|l| l.contains("@__kara_state_new_driver"))
        .unwrap_or_else(|| panic!("expected @__kara_state_new_driver in IR:\n{ir}"));
    assert!(
        define_line.contains("define"),
        "constructor should be defined: {define_line}"
    );
    assert!(
        define_line.contains("internal"),
        "constructor should have internal linkage: {define_line}"
    );
    // No arguments, returns ptr.
    assert!(
        define_line.contains("ptr @__kara_state_new_driver()"),
        "constructor signature must be `ptr @__kara_state_new_driver()`: {define_line}"
    );
}

#[test]
fn test_state_machine_still_emits_for_non_generic_concrete_function() {
    // Sanity guard: slice 8v Phase 1's filter must NOT affect
    // non-generic yielding fns — `caller()` here has no generic
    // params and is itself transitively network-boundary
    // (through `driver(42)`'s call to `fetch()`), so all four
    // helpers must still emit under its base name.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver[T](item: T) { fetch(); }
             fn caller() { driver(42i64); }",
    );
    assert!(
        ir.contains("%kara.state.caller = type"),
        "non-generic caller must still emit its state struct type:\n{ir}"
    );
    assert!(
        ir.contains("@__kara_poll_caller"),
        "non-generic caller must still emit its poll-fn:\n{ir}"
    );
    assert!(
        ir.contains("@__kara_state_new_caller"),
        "non-generic caller must still emit its state constructor:\n{ir}"
    );
}

#[test]
fn test_state_machine_skips_polymorphic_impl_method() {
    // Polymorphic methods on a non-generic impl block also skip
    // base-name emission — `is_generic_fn_key` resolves
    // `"Hub.run"` to the impl method's AST and checks
    // `generic_params.is_some()`.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             shared struct Hub { count: i64 }
             impl Hub {
                 fn run[T](self, item: T) { fetch(); }
             }",
    );
    assert!(
        !ir.contains("%kara.state.Hub.run = type"),
        "polymorphic Hub.run must not emit a base-name state struct type:\n{ir}"
    );
    assert!(
        !ir.contains("@__kara_poll_Hub.run("),
        "polymorphic Hub.run must not emit a base-name poll-fn:\n{ir}"
    );
}

// ── Phase 6 line 26 slice 8v Phase 2: per-mono state-machine emission + caller intercept ──
//
// For each monomorphization of a polymorphic network-yielding fn
// (e.g. `fn driver[T](item: T) { fetch(); }` called with `T = i64`,
// `T = Vec[i64]`, ...), codegen emits the four state-machine
// helpers — state-struct LLVM type, poll-fn, constructor,
// destructor — under the mangled key (`driver$i64`, etc.),
// with `type_subst` active so `T`-typed captured locals resolve
// to the per-mono concrete LLVM type. The caller-side intercept
// in `compile_generic_call` routes the call through
// `__kara_state_new_<mangled>` + poll loop + `free` instead of
// a direct `call @<mangled>(args)`, mirroring the slice 8d
// caller intercept for non-generic yielding fns.

#[test]
fn test_per_mono_state_struct_type_emitted_with_concrete_field_type() {
    // `driver[T](item: T)` instantiated with `T = i64` emits
    // `%"kara.state.driver$i64" = type { i32, i64 }` — the
    // `T`-typed `item` field resolves to `i64` via the active
    // `type_subst` at per-mono emission time, matching the
    // slot type that `compile_mono_function` allocated for the
    // parameter binding.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver[T](item: T) { fetch(); }
             fn caller() { driver(42i64); }",
    );
    // LLVM quotes named types containing `$`; the IR contains
    // `%"kara.state.driver$i64" = type { i32, i64 }`.
    let line = ir
        .lines()
        .find(|l| l.contains("%\"kara.state.driver$i64\" = type"))
        .unwrap_or_else(|| {
            panic!("expected per-mono state struct %\"kara.state.driver$i64\":\n{ir}")
        });
    assert!(
        line.contains("{ i32, i64 }"),
        "per-mono state struct must have {{ i32, i64 }} layout (tag + i64 item): {line}"
    );
}

#[test]
fn test_per_mono_caller_intercept_routes_through_state_machine() {
    // When the polymorphic source is network-yielding, the
    // caller's body must invoke the per-mono helpers instead
    // of direct-calling the mono fn:
    //   - constructor call to allocate the state struct
    //   - poll-loop with `kara.poll_loop` BB
    //   - cooperative `sched_yield` on Pending
    //   - terminal `free` on done
    //   - NO direct `call void @"driver$i64"(args)` in caller body
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver[T](item: T) { fetch(); }
             fn caller() { driver(42i64); }",
    );
    let caller_body = extract_fn_ir(&ir, "caller");
    assert!(
        caller_body.contains("call ptr @\"__kara_state_new_driver$i64\"()"),
        "caller must invoke per-mono state constructor:\n{caller_body}"
    );
    assert!(
        caller_body.contains("kara.poll_loop:"),
        "caller must emit per-mono poll-loop block:\n{caller_body}"
    );
    assert!(
        caller_body.contains("call i8 @\"__kara_poll_driver$i64\"(ptr %kara.state, ptr null)"),
        "caller must invoke per-mono poll-fn with state ptr + null cancel:\n{caller_body}"
    );
    assert!(
        caller_body.contains("call void @free(ptr %kara.state)"),
        "caller must free the per-mono state struct after done block:\n{caller_body}"
    );
    // CRITICAL: caller must NOT direct-call the mono fn. Slice
    // 8v Phase 2's intercept replaces the direct call with the
    // state-machine invocation. A surviving direct call would
    // mean the intercept fired without replacing the trailing
    // emit, or the mono path's gating predicate missed.
    assert!(
        !caller_body.contains("call void @\"driver$i64\"("),
        "caller must NOT direct-call the mono fn (intercept replaces it):\n{caller_body}"
    );
}

#[test]
fn test_per_mono_caller_intercept_stores_arg_into_state_struct_field() {
    // The caller-side intercept stores the call arg into the
    // state struct's captured-local field at position 1 (after
    // the tag at 0). Mirrors slice 8f's discipline keyed on the
    // mangled state-struct type.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver[T](item: T) { fetch(); }
             fn caller() { driver(42i64); }",
    );
    let caller_body = extract_fn_ir(&ir, "caller");
    assert!(
        caller_body.contains("%kara.arg0.field_ptr"),
        "intercept must GEP state struct field for arg0:\n{caller_body}"
    );
    // Arg value 42 stored into the field. With opaque pointers,
    // the store shape is `store i64 42, ptr %kara.arg0.field_ptr`.
    assert!(
        caller_body.contains("store i64 42, ptr %kara.arg0.field_ptr"),
        "intercept must store arg value 42 into state struct field:\n{caller_body}"
    );
}

#[test]
fn test_non_yielding_generic_fn_keeps_direct_call() {
    // A polymorphic fn that's NOT network-yielding (no `fetch()`
    // or similar) gets no state-struct entry under its base
    // name, so the slice 8v Phase 2 orchestrator no-ops for its
    // monos. The caller-side intercept's gating predicate (
    // `state_machine_state_constructors.get(&mangled)`) misses,
    // so the call falls through to the direct-call path. This
    // is the common case for ordinary user generics.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             fn identity[T](x: T) -> T { x }
             fn caller() {
                 let v = identity(42i64);
             }",
    );
    // No per-mono state struct.
    assert!(
        !ir.contains("%\"kara.state.identity$i64\""),
        "non-yielding generic mono must not emit a state struct:\n{ir}"
    );
    // No per-mono poll-fn.
    assert!(
        !ir.contains("@\"__kara_poll_identity$i64\""),
        "non-yielding generic mono must not emit a poll-fn:\n{ir}"
    );
    // Caller body direct-calls the mono.
    let caller_body = extract_fn_ir(&ir, "caller");
    assert!(
        caller_body.contains("call i64 @\"identity$i64\""),
        "caller must direct-call the non-yielding mono:\n{caller_body}"
    );
}

#[test]
fn test_e2e_generic_struct_field_receiver_method_param_and_indexed() {
    // B-2026-07-15-20: a field-receiver method on a GENERIC struct whose
    // field is the bare type param loud-bailed in codegen ("no handler for
    // method 'len' on variable '__field_elem_0'") for three receiver shapes
    // that the direct-local B-2026-07-15-17/-18 fix didn't reach — the synth
    // field-element binding was registered with the bare param type instead
    // of the concrete instantiation because the receiver's instantiation was
    // never recovered:
    //   (1) a `ref` generic-struct PARAM (`p: ref Pair[Vec, Vec]` →
    //       `p.second.len()`);
    //   (2) an OWNED generic-struct param (`b: Box[String]` → `b.v.len()`);
    //   (3) an INDEXED receiver over a Vec of generic structs
    //       (`v[0].second.len()`).
    // Fix: record a concretely-instantiated generic-struct param into
    // `enum_inst_var_types` at bind time (covers 1+2, ref peeled), and add an
    // indexed-container fallback (`receiver_struct_inst`) sourcing the
    // element instantiation from `var_elem_type_exprs` (covers 3). Both the
    // dispatch resolution and the mono-struct field GEP consult the same
    // instantiation, so the field's `.len()` now resolves to the concrete
    // `Vec`/`String`. The interpreter was already correct — this closes the
    // AOT loud-bail (a build failure, not a miscompile).
    let output = run_program(
        "struct Pair[A, B] { first: A, second: B }\n\
             struct Box[T] { v: T }\n\
             fn show(p: ref Pair[Vec[i64], Vec[i64]]) -> i64 {\n\
                 p.first.len() + p.second.len()\n\
             }\n\
             fn sink(b: Box[String]) -> i64 {\n\
                 b.v.len()\n\
             }\n\
             fn main() {\n\
                 let mut a: Vec[i64] = Vec.new();\n\
                 a.push(1); a.push(2);\n\
                 let mut b: Vec[i64] = Vec.new();\n\
                 b.push(10); b.push(20); b.push(30);\n\
                 let p = Pair { first: a, second: b };\n\
                 println(show(p));\n\
                 let bx = Box { v: (12345).to_string() };\n\
                 println(sink(bx));\n\
                 let mut a2: Vec[i64] = Vec.new();\n\
                 a2.push(7);\n\
                 let mut b2: Vec[i64] = Vec.new();\n\
                 b2.push(8); b2.push(9);\n\
                 let p2 = Pair { first: a2, second: b2 };\n\
                 let mut v: Vec[Pair[Vec[i64], Vec[i64]]] = Vec.new();\n\
                 v.push(p2);\n\
                 println(v[0].second.len());\n\
             }",
    )
    .expect("compile + run failed");
    // show: 2 + 3 = 5; sink: len(\"12345\") = 5; v[0].second.len() = 2.
    assert_eq!(output, "5\n5\n2\n");
}

/// B-2026-09-01-40 — a `let`-bound SCALAR field read off a struct with its
/// own `impl Drop` must not run a sibling field's body twice.
///
/// `disarm_struct_field_bodies_at` REPLACES a per-binding
/// `__karac_dropbodies_*` walker, which is right for a struct with no
/// `Drop` of its own. A struct that HAS one runs its field bodies from
/// inside `karac_drop_<T>` and has no per-binding walker, so the retraction
/// found nothing and the registration ADDED a second walk -- measured
/// `dR3 dH dR3` here against the interpreter's `dH dR3`, with the field
/// fully live in both bodies.
///
/// Nothing is moved and the read is of an `i64`, so the heap is freed
/// exactly once and ASAN was green on it before the fix. Only the body
/// COUNT is wrong, which is why these rows assert the transcript.
///
/// The five discriminators are the row's own and each is required:
/// no field read at all, the same read spelled INLINE, and a wrapper
/// without its own `Drop` were all already correct; two reads produce
/// exactly ONE extra body, so it is keyed on the binding rather than the
/// read; and reading the DROP-BEARING field is correct too, because that
/// path takes the whole-wrapper swap instead.
#[test]
fn test_e2e_let_bound_scalar_field_read_runs_sibling_body_once() {
    const H: &str = "struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             struct H { r: R, n: i64 }\n\
             impl Drop for H { fn drop(mut ref self) { println(\"dH\") } }\n\
             struct P { r: R, n: i64 }\n\
             struct H2 { r: R, r2: R, n: i64 }\n\
             impl Drop for H2 { fn drop(mut ref self) { println(\"dH2\") } }\n";
    for (label, body, want) in [
        (
            "let-bound scalar read",
            "let h = H { r: R { id: 3 }, n: 4 }; let q = h.n; println(f\"{q}\")",
            "dH\ndR3\n4\n",
        ),
        (
            "no field read (control)",
            "let h = H { r: R { id: 3 }, n: 4 }; println(\"mid\")",
            "dH\ndR3\nmid\n",
        ),
        (
            "inline read (control)",
            "let h = H { r: R { id: 3 }, n: 4 }; println(f\"{h.n}\")",
            "4\ndH\ndR3\n",
        ),
        (
            "two reads run one set of bodies",
            "let h = H { r: R { id: 3 }, n: 4 }; let q = h.n; let w = h.n; println(f\"{q}{w}\")",
            "dH\ndR3\n44\n",
        ),
        (
            "wrapper without its own Drop (control)",
            "let p = P { r: R { id: 3 }, n: 4 }; let q = p.n; println(f\"{q}\")",
            "dR3\n4\n",
        ),
        (
            "reading the Drop-bearing field (control)",
            "let h = H { r: R { id: 5 }, n: 4 }; let q = h.r; println(\"z\")",
            "dR5\ndH\nz\n",
        ),
        (
            "two Drop siblings each run once",
            "let h = H2 { r: R { id: 1 }, r2: R { id: 2 }, n: 4 }; let q = h.n; println(f\"{q}\")",
            "dH2\ndR2\ndR1\n4\n",
        ),
        (
            "field read inside an expression (control)",
            "let h = H { r: R { id: 4 }, n: 4 }; let q = h.n + 1; println(f\"{q}\")",
            "dH\ndR4\n5\n",
        ),
    ] {
        // Every other cell reads the `Copy` field `n`, which is a READ and
        // stays legal. "reading the Drop-bearing field" reads `h.r` off an
        // own-`Drop` `H`, which `partial_move_of_drop_struct` denies
        // (B-2026-09-01-43) — it is the control that makes the scalar cells
        // mean something, so it is kept under the designed opt-out rather
        // than dropped. Per cell, so the other seven keep the check-gate.
        let attr = match label {
            "reading the Drop-bearing field (control)" => "#[allow(partial_move_of_drop_struct)]\n",
            _ => "",
        };
        assert_eq!(
            run_program(&format!("{H}{attr}fn main() {{\n{body}\n}}\n")),
            Some(want.to_string()),
            "{label}"
        );
    }

    // The row's own repro, with a heap-carrying payload: `xs.len()` read 3
    // in BOTH bodies before the fix, which is what made it a live-object
    // double release rather than a residue read.
    assert_eq!(
        run_program(
            "struct R { s: String, xs: Vec[i64] }\n\
                 impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.xs.len()}\") } }\n\
                 struct H { r: R, n: i64 }\n\
                 impl Drop for H { fn drop(mut ref self) { println(\"dH\") } }\n\
                 fn main() {\n\
                 \x20   let h = H { r: R { s: \"d\", xs: [1, 2, 3] }, n: 4 };\n\
                 \x20   let q = h.n;\n\
                 \x20   println(f\"{q}\");\n\
                 \x20   println(\"end\");\n\
                 }\n"
        ),
        Some("dH\ndR3\n4\nend\n".to_string())
    );
}

/// The mono merge sort must be STABLE — design.md requires an in-place
/// stable sort (which is also why heapsort/introsort are not options),
/// and the runtime path it replaces takes the left run on a tie. Sorts
/// `(key, original_index)` pairs by KEY ONLY; a stable sort leaves the
/// original indices ascending inside every equal-key group.
///
/// Sizes straddle the insertion-sort base run (32) and the old dispatch
/// threshold (64), since those are the boundaries the merge passes and
/// the removed length check turned on.
#[test]
fn test_e2e_mono_sort_by_is_stable_across_the_run_boundaries() {
    let out = run_program(
        r#"
fn stable_violations(n: i64) -> i64 {
    let mut v: Vec[(i64, i64)] = Vec.new();
    let mut i: i64 = 0;
    while i < n {
        v.push((i % 8, i));
        i = i + 1;
    }
    v.sort_by(|a, b| a.0.cmp(b.0));
    let mut bad: i64 = 0;
    let mut j: i64 = 1;
    while j < v.len() {
        if v[j - 1].0 == v[j].0 {
            if v[j - 1].1 > v[j].1 { bad = bad + 1; }
        }
        j = j + 1;
    }
    bad
}

fn main() {
    println(stable_violations(31));
    println(stable_violations(32));
    println(stable_violations(33));
    println(stable_violations(64));
    println(stable_violations(65));
    println(stable_violations(1000));
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["0", "0", "0", "0", "0", "0"]);
    }
}

#[test]
fn test_e2e_large_n_sort_by_generic_elem_size() {
    // 24-byte element (3-tuple) exercises the generic (non-8/16) element
    // path. Sort 80 records by their first field; assert sorted.
    // (Named-struct field comparators on the runtime path are covered
    // separately by `test_e2e_large_n_sort_by_named_struct_field`.)
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[(i64, i64, i64)] = Vec.new();
    let mut i: i64 = 0;
    while i < 80 {
        v.push(((i * 29 + 3) % 80, i, i * 2));
        i = i + 1;
    }
    v.sort_by(|x, y| x.0.cmp(y.0));
    let mut bad: i64 = 0;
    let mut prev: i64 = -1;
    let mut j: i64 = 0;
    while j < 80 {
        let t = v.get(j).unwrap();
        if t.0 < prev { bad = bad + 1; }
        prev = t.0;
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
fn e2e_a_user_hash_impl_composes_with_a_user_build_hasher() {
    // design.md § `Hash` and `Hasher` splits the two questions, and the
    // implementation has to keep them split: `Hash` decides WHICH BYTES a key
    // contributes, `BuildHasher` decides how those bytes become a digest. A
    // key type with its own `impl Hash` inside a container with its own
    // hasher exercises both at once — the impl must not override the builder,
    // and the builder must not go back to hashing the key's memory image.
    //
    // The impl keys on `id` alone, so the two `id: 1` inserts collapse
    // whatever the builder does; the builder is a real FNV-1a, so the digest
    // is its own rather than the default's.
    let out = run_program(
        r#"
struct Item { id: i64, tag: i64 }
impl PartialEq for Item { fn eq(ref self, other: ref Item) -> bool { self.id == other.id } }
impl Eq for Item {}
impl Hash for Item { fn hash[H: Hasher](ref self, hasher: mut ref H) { hasher.write_i64(self.id) } }

struct Fnv { mut h: u64 }
impl Hasher for Fnv {
    fn write(mut ref self, bytes: ref Slice[u8]) {
        let mut i = 0;
        while i < bytes.len() {
            self.h = (self.h ^ (bytes[i] as u64)).wrapping_mul(16777619u64);
            i = i + 1;
        }
    }
    fn finish(ref self) -> u64 { self.h }
}
struct FnvBuild { }
impl BuildHasher for FnvBuild {
    type Hasher = Fnv;
    fn build(ref self) -> Fnv { Fnv { h: 2166136261u64 } }
}
fn main() {
    let mut m: Map[Item, i64, FnvBuild] = Map.new();
    m.insert(Item { id: 1, tag: 100 }, 10);
    m.insert(Item { id: 1, tag: 999 }, 11);
    m.insert(Item { id: 2, tag: 5 }, 20);
    println(m.len());
    match m.get(Item { id: 1, tag: 0 }) { Some(v) => println(v), None => println(-1) }
}
"#,
    );
    assert_eq!(
        out.expect("a user Hash impl under a user BuildHasher must build")
            .trim(),
        "2\n11"
    );
}

#[test]
fn e2e_a_generic_body_comparison_reaches_the_element_impl() {
    // The user-written half of B-2026-08-26-24: `a < b` on a `T: Ord`
    // inside a generic function. Lowering runs BEFORE monomorphization, so
    // there is no concrete type to resolve an impl against — the fix emits
    // a METHOD call, which dispatches on the receiver and so needs no `T`
    // substitution in either backend.
    //
    // The comparator reverses, so `smaller(1, 5)` is FALSE. A fallback to
    // declaration order or to field values would print true.
    let out = run_program(
        r#"
struct Item { id: i64 }
impl PartialEq for Item { fn eq(ref self, other: ref Item) -> bool { self.id == other.id } }
impl Eq for Item {}
impl PartialOrd for Item { fn partial_cmp(ref self, other: ref Item) -> Option[Ordering] { Some(other.id.cmp(self.id)) } }
impl Ord for Item { fn cmp(ref self, other: ref Item) -> Ordering { other.id.cmp(self.id) } }
fn smaller[T: Ord](a: T, b: T) -> bool { a < b }
fn main() {
    println(smaller(Item { id: 1 }, Item { id: 5 }));
}
"#,
    );
    assert_eq!(
        out.expect("a generic-body comparison must build").trim(),
        "false"
    );
}

#[test]
fn test_e2e_user_impl_ord_cmp_returns_ordering() {
    // Regression: `impl Ord for T { fn cmp(self, other: T) -> Ordering }`
    // used to emit the function with `i64` return type (the int tag of
    // the unit-only Ordering enum) instead of the `{ i64 }` struct shape
    // its body builds via the `.cmp` lowering in method_call.rs:670 —
    // module verification rejected the program with
    // `Function return type does not match operand type of return inst!
    //  ret { i64 } %ord  i64`. Root cause: `Ordering` wasn't seeded in
    // `seed_builtin_enum_layouts`, so `llvm_type_for_name("Ordering")`
    // fell through to the i64 default. The seed lands the layout
    // consistently across declaration and value sites. The bug had been
    // latent forever because no consumer made user-Ord-impl
    // typechecker-reachable; the user-impl-Ord investigation surfaced
    // it. See docs/implementation_checklist/phase-7-codegen.md.
    let out = run_program(
        r#"
struct Score { v: i64 }
impl PartialEq for Score { fn eq(self, other: Score) -> bool { self.v == other.v } }
impl Eq for Score {}
impl PartialOrd for Score { fn partial_cmp(self, other: Score) -> Option[Ordering] { Some(self.v.cmp(other.v)) } }
impl Ord for Score { fn cmp(self, other: Score) -> Ordering { self.v.cmp(other.v) } }
fn main() {
    let s1 = Score { v: 1i64 };
    let s2 = Score { v: 2i64 };
    let _ = s1.cmp(s2);
    println(10i64);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "10");
    }
}

#[test]
fn test_e2e_reduce_trait_bound_prod() {
    // S6c-11: `prod` on the `Reduce` trait — a required method (like
    // `sum`), so bound-generic `c.prod()` monomorphizes to the shared fold
    // kernel on both `Column` and `Tensor`, i64 and f64. `run` == `build`.
    let src = r#"
fn totalprod[C: Reduce[i64]](c: ref C) -> i64 { c.prod() }
fn fprod[C: Reduce[f64]](c: ref C) -> f64 { c.prod() }
fn sumprod[C: Reduce[i64]](c: ref C) -> i64 { c.sum() + c.prod() }
fn main() {
    let ci: Column[i64] = Column.from_vec([2, 3, 4]);
    let ti: Tensor[i64, [4]] = Tensor.from([1, 2, 3, 5]);
    let cf: Column[f64] = Column.from_vec([1.5, 2.0, 4.0]);
    println(f"{totalprod(ci)} {totalprod(ti)}");
    println(f"{fprod(cf)}");
    println(f"{sumprod(ci)}");
}
"#;
    let out = run_program(src).expect("program should compile and run");
    assert_eq!(out, "24 30\n12\n33\n");
}

#[test]
fn test_e2e_user_trait_impl_over_column() {
    // S6c-12: a user-defined `impl Trait for Column[i64]` (and f64) whose
    // body calls the builtin reductions on `self`, plus another user
    // method on `self` (`quad` → `twice`). Codegen declares the method as
    // `Column.<m>`; the fix threads the concrete element args onto the
    // synthesized `self` param so `self.sum()` (a `SelfValue` receiver)
    // registers in `column_var_infos` and dispatches to the i64/f64 kernel.
    // `run` == `build`.
    let src = r#"
trait Combo[T] { fn twice(ref self) -> T; fn quad(ref self) -> T; }
impl Combo[i64] for Column[i64] {
    fn twice(ref self) -> i64 { self.sum() + self.sum() }
    fn quad(ref self) -> i64 { self.twice() + self.twice() }
}
trait Spread[T] { fn spread(ref self) -> T; fn scaled(ref self, k: T) -> T; }
impl Spread[f64] for Column[f64] {
    fn spread(ref self) -> f64 { self.max() - self.min() }
    fn scaled(ref self, k: f64) -> f64 { self.sum() * k }
}
fn main() {
    let ci: Column[i64] = Column.from_vec([1, 2, 3, 4]);
    let cf: Column[f64] = Column.from_vec([1.5, 4.0, 2.5]);
    println(f"{ci.quad()}");
    println(f"{cf.spread()} {cf.scaled(2.0)}");
}
"#;
    let out = run_program(src).expect("program should compile and run");
    // quad = 4*sum = 40; spread = 4.0-1.5 = 2.5; scaled = 8.0*2 = 16.
    assert_eq!(out, "40\n2.5 16\n");
}

#[test]
fn test_e2e_user_trait_default_method_over_container() {
    // S6c-12 slice 3: a user trait's DEFAULT method is inherited by a
    // container impl and dispatches correctly — the desugar splice pass
    // (`synthesize_trait_default_methods`) copies the default body into the
    // `impl ... for Column`/`Tensor` block, and slices 1/2's `self`-arg +
    // `SelfValue` machinery carry it. Covers a no-arith default (`total_or`,
    // returns `total()`, ignores the fallback) and a `T: Add` arithmetic
    // default (`twice_total`), on a Column and a Tensor. `run` == `build`.
    let src = r#"
trait Stat[T: Add] {
    fn total(ref self) -> T;
    fn total_or(ref self, fallback: T) -> T { self.total() }
    fn twice_total(ref self) -> T { self.total() + self.total() }
}
impl Stat[i64] for Column[i64] {
    fn total(ref self) -> i64 { self.sum() }
}
impl Stat[i64] for Tensor[i64, [3]] {
    fn total(ref self) -> i64 { self.sum() }
}
fn main() {
    let c: Column[i64] = Column.from_vec([4, 5, 6]);
    let t: Tensor[i64, [3]] = Tensor.from([1, 2, 3]);
    println(f"{c.total_or(0)} {c.twice_total()}");
    println(f"{t.total_or(0)} {t.twice_total()}");
}
"#;
    let out = run_program(src).expect("program should compile and run");
    // c: total=15 → total_or=15, twice=30. t: total=6 → 6, 12.
    assert_eq!(out, "15 30\n6 12\n");
}

#[test]
fn test_e2e_inherent_impl_over_container_adds_methods() {
    // S6c-12 final: user *inherent* impls (`impl Column[i64] { .. }`, no
    // trait) that ADD new method names to a builtin container — admitted
    // by method-granular overlap admission alongside the baked
    // generic-on-name `impl[T] Column[T]`. Covers a Column with two
    // DISJOINT inherent impls, a self-calls-another-inherent-method chain,
    // and a Tensor. Once the typechecker admits the impl, codegen dispatch
    // already works (disjoint names → no collision). `run` == `build`.
    let src = r#"
impl Column[i64] {
    fn doubled_sum(ref self) -> i64 { self.sum() + self.sum() }
    fn quad_sum(ref self) -> i64 { self.doubled_sum() + self.doubled_sum() }
}
impl Column[i64] {
    fn spread(ref self) -> i64 { self.max() - self.min() }
}
impl Tensor[i64, [3]] {
    fn twice_sum(ref self) -> i64 { self.sum() + self.sum() }
}
fn main() {
    let c: Column[i64] = Column.from_vec([1, 2, 3, 4]);
    let t: Tensor[i64, [3]] = Tensor.from([1, 2, 3]);
    println(f"{c.doubled_sum()} {c.quad_sum()} {c.spread()}");
    println(f"{t.twice_sum()}");
}
"#;
    let out = run_program(src).expect("program should compile and run");
    // sum=10 → doubled=20, quad=40; spread = 4-1 = 3. tensor sum=6 → 12.
    assert_eq!(out, "20 40 3\n12\n");
}

#[test]
fn test_e2e_user_generic_trait_impl_over_container() {
    // S6c-12 slice 4: a GENERIC container impl `impl[T: Add] Trait[T] for
    // Column[T]` / `Tensor[T, S]` with an explicit (required) method. Routes
    // through `make_generic_impl_method_function` (types `self` as the target
    // expr) + the S6a mono handle plumbing. Column across two element monos
    // (i64 + f64) and a Tensor at i64. `run` == `build`.
    let src = r#"
trait Doubler[T: Add] { fn doubled_sum(ref self) -> T; }
impl[T: Add] Doubler[T] for Column[T] {
    fn doubled_sum(ref self) -> T { self.sum() + self.sum() }
}
impl[T: Add] Doubler[T] for Tensor[T, [3]] {
    fn doubled_sum(ref self) -> T { self.sum() + self.sum() }
}
fn main() {
    let ci: Column[i64] = Column.from_vec([1, 2, 3]);
    let cf: Column[f64] = Column.from_vec([1.5, 2.5, 3.0]);
    let ti: Tensor[i64, [3]] = Tensor.from([1, 2, 3]);
    println(f"{ci.doubled_sum()} {cf.doubled_sum()} {ti.doubled_sum()}");
}
"#;
    let out = run_program(src).expect("program should compile and run");
    // Column sum(1,2,3)=6 → 12; sum(1.5,2.5,3.0)=7.0 → 14; Tensor i64 → 12.
    assert_eq!(out, "12 14 12\n");
}

#[test]
fn test_e2e_reduce_trait_bound_fold() {
    // S6c: `fold` on the `Reduce` trait surface, dispatched through a
    // bound-generic `fn f[C: Reduce[i64]]`. The typechecker intercept types
    // the closure `(A, T)` from `init` + the bound's element; codegen needs
    // NO new routing — the mono handle param registers as a Column/Tensor,
    // so `c.fold(0, |a, x| a + x)` (an inline closure) reaches the same
    // `compile_column_fold` / `compile_tensor_fold` inline-closure kernel
    // the concrete surface uses. Covers a sum fold on both containers, a
    // non-sum body (count > 2), and null-skipping (nulls dropped, in order).
    let src = r#"
fn accumulate[C: Reduce[i64]](c: ref C) -> i64 {
    c.fold(0, |a, x| a + x)
}
fn count_gt2[C: Reduce[i64]](c: ref C) -> i64 {
    c.fold(0, |a, x| if x > 2 { a + 1 } else { a })
}
fn main() {
    let col: Column[i64] = Column.from_vec([3, 1, 4, 1, 5]);
    let t: Tensor[i64, [3]] = Tensor.from([10, 20, 5]);
    let mut nulled: Column[i64] = Column.new();
    nulled.push(10);
    nulled.push_null();
    nulled.push(30);
    println(f"{accumulate(col)} {accumulate(t)}");
    println(f"{count_gt2(col)}");
    println(f"{accumulate(nulled)}");
}
"#;
    // col sum = 14, t sum = 35; count>2 over [3,1,4,1,5] = 3; nulled = 10+30.
    let out = run_program(src).expect("program should compile and run");
    assert_eq!(out, "14 35\n3\n40\n");
}

#[test]
fn test_e2e_bounded_generic_impl_method_call() {
    // B-2026-07-03-20: a method on a bounded generic impl (`impl[T: Sub]
    // Pair[T]`) resolves and runs end-to-end. Before the fix
    // `impl_bounds_discharge` dropped the impl whenever it could not prove
    // `T: Sub` — which is exactly the case for the impl's own `self` (bare
    // target type, no args) — so `gap`'s `self.hi() - self.lo()` failed
    // "no method 'hi' on type 'Pair'" under `build` (ran fine, as `karac
    // run` executes past the warning). The fix makes discharge permissive
    // for an undecidable (missing / type-variable) substitution. `gap` =
    // `b - a`. Two i64 instantiations exercise the shared-layout mono; a
    // NON-i64 (f64) element is still blocked on the separate generic
    // struct-literal arg-inference miscompile (B-2026-07-03-23), so this
    // stays on i64.
    let src = r#"
struct Pair[T] { a: T, b: T }
impl[T: Sub] Pair[T] {
    fn lo(ref self) -> T { self.a }
    fn hi(ref self) -> T { self.b }
    fn gap(ref self) -> T { self.hi() - self.lo() }
}
fn main() {
    let p = Pair { a: 3, b: 10 };
    println(f"{p.gap()}");
    let q = Pair { a: 100, b: 7 };
    println(f"{q.gap()}");
}
"#;
    // gap = b - a: p → 10 - 3 = 7; q → 7 - 100 = -93.
    let out = run_program(src).expect("program should compile and run");
    assert_eq!(out, "7\n-93\n");
}

#[test]
fn test_e2e_generic_struct_field_monomorphizes_by_element() {
    // B-2026-07-03-23: a generic struct with an inline type-param field
    // (`Box[T] { v: T }`) is now laid out / accessed per its concrete
    // instantiation. Before the fix `let b = Box { v: 2.5 }` typed as the
    // bare `Box` (args discarded) and codegen defaulted the field to i64,
    // so `b.v` read 2.5's f64 bits as an integer (silent garbage under
    // `build`, correct under `run`). Fixed in three layers: the typechecker
    // infers the literal's args (`Box[f64]`) and substitutes them into field
    // access, and codegen builds a per-instantiation LLVM struct type
    // (`Box[f64]` -> `{double}`) at construction / field access / store / the
    // function ABI. Covers f64 + i64 + String elements, a field STORE, a
    // two-field struct's arithmetic, and passing `Box[f64]` across a function
    // boundary. (A METHOD reading a generic-struct-instance's non-i64 field
    // is layer 4, covered by
    // `test_e2e_generic_struct_method_monomorphizes_by_receiver`.)
    let src = r#"
struct Box[T] { v: T }
struct Pair[T] { a: T, b: T }
fn getv(b: ref Box[f64]) -> f64 { b.v }
fn main() {
    let bf = Box { v: 2.5 };
    println(f"{bf.v}");
    let bi = Box { v: 42 };
    println(f"{bi.v}");
    let bs = Box { v: "gen_struct_string_payload_abcdef" };
    println(f"{bs.v}");
    let mut m = Box { v: 1.0 };
    m.v = 9.5;
    println(f"{m.v}");
    let p = Pair { a: 1.5, b: 4.0 };
    println(f"{p.a - p.b}");
    println(f"{getv(bf)}");
}
"#;
    let out = run_program(src).expect("program should compile and run");
    assert_eq!(
        out,
        "2.5\n42\ngen_struct_string_payload_abcdef\n9.5\n-2.5\n2.5\n"
    );
}

#[test]
fn test_e2e_generic_struct_method_monomorphizes_by_receiver() {
    // B-2026-07-03-23 layer 4: a METHOD on a generic struct is compiled per
    // the RECEIVER's instantiation, so `Box.get` on a `Box[f64]` uses a
    // `{double}` self and returns `double`. Before the fix a `ref self`
    // method read the field as i64 (silent garbage) and a by-value `self`
    // method HARD-crashed the build (`{double}` value vs `{i64}` self param)
    // — both because the method's self was lowered as the bare all-i64
    // `Box`. The generic-struct-impl methods now register into `generic_fns`
    // and dispatch through `compile_generic_call`, binding the impl's `T`
    // explicitly from the receiver's recorded instantiation. Covers ref-self
    // + by-value self (`Box.get_ref`/`get_val`), i64 (regression), and a
    // BOUNDED impl whose method calls other self methods (`Pair[T: Sub].gap`
    // = `hi - lo`), for f64 and i64.
    let src = r#"
struct Box[T] { v: T }
impl[T] Box[T] {
    fn get_ref(ref self) -> T { self.v }
    fn get_val(self) -> T { self.v }
}
struct Pair[T] { a: T, b: T }
impl[T: Sub] Pair[T] {
    fn lo(ref self) -> T { self.a }
    fn hi(ref self) -> T { self.b }
    fn gap(ref self) -> T { self.hi() - self.lo() }
}
fn main() {
    let bf = Box { v: 2.5 };
    println(f"{bf.get_ref()}");
    println(f"{bf.get_val()}");
    let bi = Box { v: 42 };
    println(f"{bi.get_ref()}");
    let pf = Pair { a: 1.5, b: 9.0 };
    println(f"{pf.gap()}");
    let pi = Pair { a: 100, b: 7 };
    println(f"{pi.gap()}");
}
"#;
    // bf f64 (ref + by-value) = 2.5; bi i64 = 42; pf gap = 9-1.5 = 7.5;
    // pi gap = 7-100 = -93.
    let out = run_program(src).expect("program should compile and run");
    assert_eq!(out, "2.5\n2.5\n42\n7.5\n-93\n");
}

#[test]
fn test_e2e_unsigned_refinement_bound_at_base_max_accepts() {
    // The idiomatic spelling — bound the range by the base type's own
    // maximum — was the always-failing case.
    let out = run_program(
        r#"
distinct type Port = u16 where self >= 1 and self <= 65535;
fn main() {
    let p = Port(80);
    println("built");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "built");
    }
}

#[test]
fn e2e_tracing_user_exporter_impl_reads_event_and_span_fields() {
    // A user `impl Exporter` whose methods take `LogEvent` / `Span` by
    // value compiles and reads their fields correctly. Regression for a
    // pre-existing gap surfaced by the line-156 work: the baked tracing
    // value structs weren't in `struct_types` when user functions were
    // declared, so `fn export_event(ref self, event: LogEvent)` got an
    // `i64` param (the `llvm_type_for_name` fall-through) and the call
    // `t.export_event(LogEvent.info(...))` failed module verification.
    // `seed_builtin_struct_types` now seeds `SpanField`/`Span`/`LogEvent`
    // so the by-value param layout matches. Direct (non-registered) calls.
    let out = run_program(
        r#"struct Collector { }
            impl Exporter for Collector {
                fn export_event(ref self, event: LogEvent) {
                    println(f"E {event.level}/{event.message}/{event.span_id}");
                }
                fn export_span(ref self, span: Span) {
                    println(f"S {span.name}/{span.span_id}/{span.parent_id}");
                }
            }
            fn main() {
                let c = Collector {};
                c.export_event(LogEvent.warn("disk").in_span(3));
                c.export_span(Span.root("req", 7).child("inner", 9));
            }"#,
    );
    assert_eq!(out.as_deref(), Some("E warn/disk/3\nS inner/9/7\n"),);
}

#[test]
fn test_e2e_sub64_widths_across_boundaries() {
    // One program per boundary the coercion covers: explicit + tail
    // returns (i32/i16/f32), an annotated-let u8 (i64-backed slot,
    // trunc at ret — 200 pins zero-extension on the print path),
    // literal call args landing in i8 params (free fn AND method),
    // narrow-param arithmetic against a default-width literal (int
    // and float lanes), and a negative i8 round trip (sign
    // preserved through trunc + print sext).
    let out = run_program(
        "struct P { v: i64 }\n\
             impl P {\n\
                 fn add_small(self, k: i8) -> i8 {\n        return k + 1;\n    }\n\
             }\n\
             fn ret_i32() -> i32 {\n    return 0;\n}\n\
             fn tail_i32() -> i32 {\n    7\n}\n\
             fn ret_i16() -> i16 {\n    return 3;\n}\n\
             fn u8_var() -> u8 {\n    let x: u8 = 200;\n    return x;\n}\n\
             fn i8_inc(x: i8) -> i8 {\n    return x + 1;\n}\n\
             fn f32_op(x: f32) -> f32 {\n    return x + 1.5;\n}\n\
             fn main() {\n\
                 println(ret_i32());\n\
                 println(tail_i32());\n\
                 println(ret_i16());\n\
                 println(u8_var());\n\
                 println(i8_inc(5));\n\
                 println(i8_inc((0 - 2) as i8));\n\
                 let p = P { v: 0 };\n\
                 println(p.add_small(4));\n\
                 println(f32_op(2.0));\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(
            out, "0\n7\n3\n200\n6\n-1\n5\n3.5\n",
            "sub-64-bit widths must round-trip correct values across \
                 ret, call-arg, method-arg, and binop boundaries",
        );
    }
}

#[test]
fn test_ir_letbound_zip_reduce_does_not_fuse() {
    // The let-bound form (`let p = a.zip_with(b, f); p.sum()`) is an
    // IDENTIFIER receiver — it materializes then reduces (no `fmr.*`).
    // Guards that fusion is scoped to the chained temp-receiver form and
    // never fires on a bound intermediate (the shape the batched embedding
    // kernels rely on, B-2026-07-20-6).
    let ir = ir_for(
        "fn d(a: ref Tensor[f32, [64]], b: ref Tensor[f32, [64]]) -> f32 \
             { let p = a.zip_with(b, |x, y| x * y); p.sum() }\n\
             fn main() {\n\
                 let a: Tensor[f32, [64]] = Tensor.ones([64]);\n\
                 let b: Tensor[f32, [64]] = Tensor.ones([64]);\n\
                 println(d(a, b));\n\
             }\n",
    );
    assert!(
        !ir.contains("fmr."),
        "the let-bound form must NOT fuse (materialize path); IR:\n{ir}"
    );
}

// ── `getelementptr inbounds` on bounds-checked element accesses ──────
//
// Element GEPs (Vec / Slice / Array indexing) are reached only with a valid
// in-range index (bounds-check-dominated, BCE-proven, or the get_unchecked
// unsafe contract), so `data + i` provably stays within the allocation →
// `getelementptr inbounds`. Unlocks the aliasing/stride facts the vectorizer
// needs; composes with the `nsw` IV and slice `noalias`.
#[test]
fn element_access_uses_inbounds_gep() {
    let ir = ir_for(
        r#"
fn vsum(v: Vec[i64]) -> i64 {
    let mut s = 0; let n = v.len(); let mut i = 0;
    while i < n { s = s + v[i]; i = i + 1; }
    return s;
}
fn sset(xs: mut Slice[i64]) {
    let n = xs.len(); let mut i = 0;
    while i < n { xs[i] = i; i = i + 1; }
}
fn aget(a: Array[i64, 4], i: i64) -> i64 { return a[i]; }
fn main() { print(0); }
"#,
    );
    assert!(
        fn_body(&ir, "@vsum(").contains("%v.elem.ptr = getelementptr inbounds"),
        "Vec element read GEP should be inbounds:\n{}",
        fn_body(&ir, "@vsum(")
    );
    assert!(
        fn_body(&ir, "@sset(").contains("%s.st.elem.ptr = getelementptr inbounds"),
        "Slice element store GEP should be inbounds:\n{}",
        fn_body(&ir, "@sset(")
    );
    assert!(
        fn_body(&ir, "@aget(").contains("%arr.elem.ptr = getelementptr inbounds"),
        "Array element read GEP should be inbounds:\n{}",
        fn_body(&ir, "@aget(")
    );
}

#[test]
fn test_e2e_fn_value_param_through_generic_mono() {
    // B-2026-07-02-11: `fn apply[T](x: T, f: Fn(T) -> T) -> T { f(x) }`
    // called with a closure silently returned 0 under `karac build`
    // (the mono prologue never registered the `Fn`-typed param in
    // `closure_fn_types`, so `f(x)` fell through to the unknown-callee
    // const-0 placeholder). `karac run` was always correct.
    let out = run_program(
        "fn apply[T](x: T, f: Fn(T) -> T) -> T {\n\
                 return f(x);\n\
             }\n\
             fn main() {\n\
                 println(apply(20, |v| v * 2 + 2));\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(out, "42\n");
    }
}

/// The `init == 0` half of the pin above: same loop, accumulator started
/// near `i64::MAX`, so the add genuinely overflows on the second
/// iteration. The analysis must NOT have elided this check.
#[test]
fn bounded_accumulator_nonzero_init_still_traps() {
    if let Some(cap) = run_program_capturing(
        "fn main() {\n\
                 let mut acc = 9223372036854775806i64;\n\
                 let mut i = 0i64;\n\
                 while i < 10i64 {\n\
                     acc = acc + 1i64;\n\
                     i = i + 1i64;\n\
                 }\n\
                 println(f\"{acc}\");\n\
             }\n",
    ) {
        assert_eq!(cap.status.code(), Some(101), "stderr={:?}", cap.stderr);
        assert!(
            cap.stderr.contains("integer overflow"),
            "a non-zero-init accumulator must still trap; stdout={:?}",
            cap.stdout
        );
    }
}

#[test]
fn test_e2e_seeded_generic_free_fn_adopts_and_runs() {
    // B-2026-08-08-8 — the newly-accepted shapes must LOWER, not merely
    // typecheck. Expected-return seeding now reaches a bare-identifier
    // callee, so `wrap([30, 10, 20])` adopts `u32` from the annotation
    // instead of minting `Vec[i64]`; and pass 2 resolves the argument's own
    // type, so `Vec.new()` reaches a slot a sibling argument fixed. Both
    // previously failed typecheck outright, which is exactly the situation
    // where a fix can typecheck a program codegen cannot emit
    // (B-2026-08-08-5 / -7 both did on their first cut).
    let out = run_program(
        r#"
struct Holder[T] { items: Vec[T] }
fn wrap[T](v: Vec[T]) -> Holder[T] { Holder { items: v } }
fn pair[T](a: Vec[T], b: Vec[T]) -> i64 { a.len() + b.len() }
fn main() {
    let h: Holder[u32] = wrap([30, 10, 20]);
    println(h.items.len());
    println(h.items[1]);
    let sv: Vec[String] = ["a"];
    println(pair(Vec.new(), sv));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "3\n10\n1");
    }
}

/// B-2026-08-14-15 — the VALUE side of the two leaks. Both spellings of
/// each program must agree with the interpreter twin
/// (`tests/interpreter.rs::test_bound_nested_container_reads`); the leaks
/// themselves are pinned under LSan in `tests/memory_sanitizer.rs`.
///
/// Leg A binds a `Map` element out of a `Vec` and probes it with a key;
/// leg B binds an `Option[Vec[P]]` and matches it. Each is paired with the
/// inline form that was already clean, so a fix that changed the binding
/// form's OUTPUT would fail here rather than pass quietly.
#[test]
fn test_e2e_bound_nested_container_reads() {
    assert_eq!(
        run_program(
            "struct P { tag: String }\n\
                 fn mk(k: i64) -> Option[Vec[P]] {\n\
                     let mut c: Vec[P] = Vec.new();\n\
                     c.push(P { tag: f\"alpha{k}\" });\n\
                     Some(c)\n\
                 }\n\
                 fn mkr(k: i64) -> Result[Vec[P], Vec[P]] {\n\
                     let mut c: Vec[P] = Vec.new();\n\
                     c.push(P { tag: f\"beta{k}\" });\n\
                     if k > 100 { return Err(c); }\n\
                     Ok(c)\n\
                 }\n\
                 fn main() {\n\
                     let k = 1;\n\
                     let mut vms: Vec[Map[String, i64]] = Vec.new();\n\
                     let mut inner: Map[String, i64] = Map.new();\n\
                     let _ = inner.insert(\"n\", k);\n\
                     vms.push(inner);\n\
                     let cur = vms[0].clone();\n\
                     match cur.get(\"n\") { Some(x) => println(x), None => println(-1) }\n\
                     println(cur.contains_key(\"n\"));\n\
                     println(cur.len());\n\
                     match vms[0].get(\"n\") { Some(x) => println(x), None => println(-1) }\n\
                     let held = mk(k);\n\
                     match held { Some(v) => println(v[0].tag), None => println(\"none\") }\n\
                     match mk(k) { Some(v) => println(v[0].tag), None => println(\"none\") }\n\
                     let okr = mkr(k);\n\
                     match okr { Ok(v) => println(v[0].tag), Err(e) => println(e[0].tag) }\n\
                     let err = mkr(200);\n\
                     match err { Ok(v) => println(v[0].tag), Err(e) => println(e[0].tag) }\n\
                 }\n"
        ),
        Some("1\ntrue\n1\n1\nalpha1\nalpha1\nbeta1\nbeta200\n".to_string())
    );
}

/// B-2026-08-15-9 — bare-`T` generic params across the return relationships
/// that decide who frees the argument.
///
/// A regression guard, not the detector: the leak is what
/// `asan_generic_unused_bare_type_param_temp_arg_no_leak` catches, and every
/// value here was already correct. What it pins is the direction this fix
/// could go wrong. `takeout` returns its `match` scrutinee and `pick` /
/// `nest` reach the return through a forwarding call, so all three are
/// shapes where a caller-side drop would collide with the result consumer's
/// — a collision ASAN reports as a double free shows up here as a corrupted
/// read. `x` and `ns` are re-read at the end, after being cloned into eight
/// calls, to pin that nothing freed a buffer their bindings still own.
#[test]
fn test_e2e_generic_bare_type_param_temp_arg() {
    assert_eq!(
        run_program(
            "fn id[T](v: T) -> T { return v; }\n\
                 fn pick[T](a: T, b: T) -> T { return id(a); }\n\
                 fn keep[T](a: T, b: T) -> T { return a; }\n\
                 fn echo[T](x: T) -> T { return x; }\n\
                 fn nest[T](x: T) -> T { return id(id(x)); }\n\
                 fn takeout[T](b: T) -> T { match b { v => { return v; } } }\n\
                 fn three[T](a: T, b: T, c: T) -> T { return b; }\n\
                 fn mixed[T](a: T, n: i64) -> i64 { return n; }\n\
                 fn mk() -> Vec[i64] { let v: Vec[i64] = [10, 20, 30]; return v; }\n\
                 fn main() {\n\
                     let x: Vec[i64] = [1, 2, 3, 4, 5];\n\
                     let y: Vec[i64] = [6, 7, 8];\n\
                     let ns: Vec[String] = [\"alpha\", \"beta\"];\n\
                     let r1 = pick(x.clone(), y.clone()); println(r1[0]);\n\
                     let r2 = keep(x.clone(), y.clone()); println(r2[4]);\n\
                     let r3 = echo(x.clone()); println(r3[1]);\n\
                     let r4 = nest(x.clone()); println(r4[2]);\n\
                     let r5 = takeout(x.clone()); println(r5[3]);\n\
                     let r6 = three(x.clone(), y.clone(), mk()); println(r6[1]);\n\
                     println(mixed(x.clone(), 42));\n\
                     let r7 = pick(ns.clone(), ns.clone()); println(r7[1]);\n\
                     let r8 = keep(ns.clone(), ns.clone()); println(r8[0]);\n\
                     let r9 = keep(x.clone(), [90, 91, 92]); println(r9[0]);\n\
                     println(x.len());\n\
                     println(y.len());\n\
                     println(ns[1]);\n\
                 }\n"
        ),
        Some("1\n5\n2\n3\n4\n7\n42\nbeta\nalpha\n1\n5\n3\nbeta\n".to_string())
    );
}

/// B-2026-09-01-20 — the same call-site default fill, reached through the
/// QUALIFIED `Type.assoc_fn(..)` spelling.
///
/// B-2026-08-17-19 shipped the fill for a bare identifier only and said so:
/// "Method / associated-function calls and module-qualified `Path` callees are
/// out of scope for this slice." design.md scopes the feature to no function
/// kind, though — "Parameters may have default values, allowing callers to omit
/// them" — so two of the three call forms lacked the feature the document
/// describes. (The row's `runtime/stdlib/column.kara` evidence does not hold:
/// `Column.fillna` is `#[compiler_builtin]` and always dispatched through the
/// builtin surface, so `c.fillna(0)` type-checked and ran before the fix, and
/// it is the only defaulted method signature in `runtime/stdlib`.)
///
/// The four shapes are the free-function fixture's, verbatim, so the two read as
/// one oracle: omit every default, omit the tail, skip one by label, and mix a
/// positional override with two labels. Same values as
/// `test_e2e_default_parameter_call_site_fill`, because a fill that produced
/// anything else for the same signature would mean the two spellings disagree —
/// which is the whole complaint.
///
/// This fixture covers the ASSOCIATED half, which the pre-resolve pass fills
/// because `Type.assoc_fn` names its callee syntactically. The instance-method
/// half is the twin below.
#[test]
fn test_e2e_default_parameter_fill_through_an_associated_call() {
    assert_eq!(
        run_program(
            r#"
struct Server { id: i64 }

impl Server {
    fn create(host: i64, port: i64 = 8080, max_connections: i64 = 1000, timeout_ms: i64 = 5000) -> i64 {
        host + port + max_connections + timeout_ms
    }
}

fn main() {
    println(Server.create(1));
    println(Server.create(1, 9090));
    println(Server.create(1, max_connections: 100));
    println(Server.create(1, 9090, max_connections: 100, timeout_ms: 250));
}
"#
        ),
        Some("14081\n15091\n13181\n9441\n".to_string())
    );
}

/// B-2026-08-27-40 — A TUPLE TYPE ARGUMENT MUST SURVIVE A NESTED GENERIC
/// CALL. `Bag[(i64, i64)]`'s outer call bound `T`'s LLVM type correctly,
/// but the binding was recorded ONLY in the name channel
/// (`type_subst_names`), and a tuple has no name — so the inner
/// `self.sw(..)` re-derived the receiver's instantiation, found nothing,
/// and emitted the callee with an EMPTY substitution. `T` then fell to the
/// `i64` unknown-name default and the 16-byte element was swapped 8 bytes
/// at a time.
///
/// ONE METHOD CALLING ANOTHER ON `self` is the trigger, which is why this
/// hid for so long: a direct `b.sw(0, 1)` from `main` was always correct,
/// and every generic-impl fixture in this file is single-level. `mid` sits
/// between `outer` and `sw` so the fixture also covers depth > 2.
///
/// Four shapes, because the pre-fix failure mode differs by shape and no
/// single one of them would have caught the others — all measured against
/// the pre-fix compiler rather than argued:
///
/// - `(i64, i64)` — wrong answer (`10:1 2:20` for `2:20 1:10`).
/// - `(i64, i64, i64)` — wrong answer, and arity-selective: three scalars
///   are the `{ptr, i64, i64}` String-header shape.
/// - `(String, i64)` — SEGFAULT at run time, not a wrong answer.
/// - `((i64, i64), i64)` — wrong answer through a nested tuple.
///
/// and with all four in one file the pre-fix compiler does not get that
/// far at all: three tuple instantiations of `Bag` collide on one mono
/// symbol and it panics with `ExtractOutOfRange`. The `Bag[i64]` control
/// is the shape that always worked, and it must keep working — the name
/// channel still serves it.
///
/// Oracle: `tuple_type_arg_through_a_nested_generic_call_in_the_interpreter`
/// runs this program verbatim, and the ASAN twin
/// `asan_tuple_type_arg_with_a_heap_element_through_a_nested_generic_call`
/// covers the `(String, i64)` row that used to crash.
#[test]
fn test_e2e_tuple_type_arg_survives_a_nested_generic_call() {
    assert_eq!(
        run_program(
            r#"
struct Bag[=T] { xs: Vec[T] }

impl[T] Bag[T] {
    fn sw(mut ref self, i: i64, j: i64) { self.xs.swap(i, j); }
    fn mid(mut ref self) { self.sw(0, 1); }
    fn outer(mut ref self) { self.mid(); }
    fn at(ref self, i: i64) -> T { return self.xs[i]; }
}

fn main() {
    let mut p: Bag[(i64, i64)] = Bag { xs: Vec.new() };
    p.xs.push((1, 10));
    p.xs.push((2, 20));
    p.outer();
    let p0 = p.at(0);
    let p1 = p.at(1);
    println(f"{p0.0}:{p0.1} {p1.0}:{p1.1}");

    let mut t: Bag[(i64, i64, i64)] = Bag { xs: Vec.new() };
    t.xs.push((1, 2, 3));
    t.xs.push((4, 5, 6));
    t.outer();
    let t0 = t.at(0);
    let t1 = t.at(1);
    println(f"{t0.0},{t0.1},{t0.2} {t1.0},{t1.1},{t1.2}");

    let mut s: Bag[(String, i64)] = Bag { xs: Vec.new() };
    s.xs.push(("x" + "y", 1));
    s.xs.push(("p" + "q", 2));
    s.outer();
    let s0 = s.at(0);
    let s1 = s.at(1);
    println(f"{s0.0}:{s0.1} {s1.0}:{s1.1}");

    let mut n: Bag[((i64, i64), i64)] = Bag { xs: Vec.new() };
    n.xs.push(((1, 2), 3));
    n.xs.push(((4, 5), 6));
    n.outer();
    let n0 = n.at(0);
    println(f"{n0.0.0},{n0.0.1},{n0.1}");

    let mut c: Bag[i64] = Bag { xs: Vec.new() };
    c.xs.push(1);
    c.xs.push(2);
    c.outer();
    println(f"{c.at(0)} {c.at(1)}");
}
"#,
        ),
        Some("2:20 1:10\n4,5,6 1,2,3\npq:2 xy:1\n4,5,6\n2 1\n".to_string())
    );
}

#[test]
fn test_e2e_ord_bound_on_generic_impl_admits_a_tuple() {
    assert_eq!(
        run_program(
            r#"
struct W[=T] { v: Vec[T] }

impl[T: Ord] W[T] {
    fn add(mut ref self, x: T) { self.v.push(x); }
    fn n(ref self) -> i64 { return self.v.len(); }
}

fn main() {
    let mut w: W[(i64, i64)] = W { v: Vec.new() };
    w.add((1, 2));
    w.add((3, 4));
    println(f"{w.n()}");

    let mut x: W[(String, i64)] = W { v: Vec.new() };
    x.add(("a", 1));
    println(f"{x.n()}");

    let mut u: W[i64] = W { v: Vec.new() };
    u.add(7);
    println(f"{u.n()}");
}
"#,
        ),
        // two distinct tuple instantiations, then the scalar control that
        // always worked.
        Some("2\n1\n1\n".to_string())
    );
}

/// A field read through a GENERIC callee's tuple element —
/// `let q = firstof(Tag { n: 9 }); q.0.n` where
/// `fn firstof[T](x: T) -> (T, i64)` (B-2026-08-28-28).
///
/// B-2026-08-28-3 read the callee's DECLARED return type, which for a
/// generic callee is `(T, i64)` — the type PARAMETER — and deliberately let
/// that refuse loudly rather than record `"T"`, because a consumer taking
/// it at face value resolves the read against whatever unrelated struct
/// also declares the field. This binds `T` for the call instead of guessing.
///
/// TWO INSTANTIATIONS OF THE SAME GENERIC FUNCTION ARE THE POINT. `Tag` and
/// `Other` both declare an `n`, at DIFFERENT indices, and both are passed
/// to the same `firstof`. A resolution that bound `T` once — last writer
/// wins, the failure mode `emit_clone_fn_for_type_expr` actually had under
/// B-2026-07-12-16 — prints one of them wrong rather than refusing, and
/// nothing else in the fixture would notice.
///
/// The remaining rows cover the sources the binding has to come through: an
/// UNBOUND call result (no binding to key on), a generic callee nested
/// inside another monomorph (the outer parameter must flatten), and a
/// generic METHOD (a different callee key).
#[test]
fn test_e2e_field_read_through_a_generic_callees_tuple_element() {
    let src = r#"
struct Tag { n: i64, a: i64 }
struct Other { a: i64, n: i64 }
struct Box3 { k: i64 }

fn firstof[T](x: T) -> (T, i64) { return (x, 1); }
fn outer[U](y: U) -> i64 { let q = firstof(y); return q.1; }

impl Box3 { fn pair[T](ref self, x: T) -> (T, i64) { return (x, self.k); } }

fn main() {
    let a = firstof(Tag { n: 9, a: 99 });
    let b = firstof(Other { a: 5, n: 42 });
    println(a.0.n);
    println(a.0.a);
    println(b.0.n);
    println(b.0.a);
    println(firstof(Tag { n: 7, a: 77 }).0.n);
    println(outer(Tag { n: 1, a: 2 }));
    let c = Box3 { k: 5 };
    let d = c.pair(Tag { n: 3, a: 4 });
    println(d.0.n);
    println(d.1);
}
"#;
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(
        interp_errs.is_empty(),
        "interpreter errors: {interp_errs:?}"
    );
    let expected = interp_out.join("");
    // 9/99 then 42/5: one binding shared across both instantiations reads
    // `Other` at `Tag`'s index for `n` (or the reverse) and prints 5 or 99.
    assert_eq!(
        expected, "9\n99\n42\n5\n7\n1\n3\n5\n",
        "interpreter oracle is wrong for a generic callee's tuple element",
    );
    let Some(aot) = run_program(src) else { return };
    assert_eq!(
        aot, expected,
        "a generic callee's tuple element must bind its type parameter per call",
    );
}

/// B-2026-08-18-24 / -31 — a generic-struct FIELD reached through an
/// INDEXED receiver: `v[0].second.len()` over `Vec[Pair[Vec, Vec]]`.
///
/// A VALUE assertion, which is the point. The postfix-span rows are pinned
/// by parser tests asserting that each node carries its own SpanKey; those
/// cannot see this shape, because the failure mode here is not a lost span
/// but a field GEP that lands in the wrong place. `v[0]` and `v[0].second`
/// resolve their instantiation through span-keyed records, and when that
/// resolution picks up the wrong entry the receiver falls back to the
/// ERASED base struct — whose generic fields are one word each — so the GEP
/// reads mid-`{ptr,len,cap}`. Observed during that work: `.len()` on a
/// two-element field returning 4. The program builds and exits 0, so only
/// checking the ANSWER catches it.
///
/// Both fields are read, since a wrong GEP that happens to land on the
/// other field would still satisfy a single-field check.
#[test]
fn indexed_generic_struct_field_receiver_reads_the_right_field() {
    let src = "struct Pair[A, B] { first: A, second: B }\n\
                   fn main() {\n\
                       let mut a: Vec[i64] = Vec.new();\n\
                       a.push(1);\n\
                       let mut b: Vec[i64] = Vec.new();\n\
                       b.push(10); b.push(20);\n\
                       let mut v: Vec[Pair[Vec[i64], Vec[i64]]] = Vec.new();\n\
                       v.push(Pair { first: a, second: b });\n\
                       println(v[0].first.len().to_string());\n\
                       println(v[0].second.len().to_string());\n\
                   }\n";
    let Some(out) = run_program(src) else {
        return;
    };
    assert_eq!(
        out, "1\n2\n",
        "each field of an indexed generic struct must GEP to its own storage"
    );
}

/// B-2026-08-21-21 — `requires` / `ensures` on a GENERIC function are
/// enforced by the compiled backends.
///
/// The monomorphized body path had NO contract handling at all, so every
/// contract on a generic function was silently dropped by JIT and AOT
/// while the interpreter enforced it. design.md § Contracts presents a
/// generic `binary_search[T: Ord]` as the feature's worked example, so the
/// one shape the documentation leads with was the one shape neither
/// compiled backend checked.
///
/// A passing contract must stay silent, which is what separates "the check
/// is emitted" from "the check is emitted and always fires".
#[test]
fn test_e2e_generic_fn_requires_is_enforced() {
    let violated = r#"
fn gen_req[T](v: ref Vec[T]) -> i64
    requires v.len() > 5
where T: Ord
{ v.len() }
fn main() {
    let a: Vec[i64] = [1, 2, 3];
    println(gen_req(a));
}
"#;
    let cap = run_program_capturing(violated).expect("build the violating program");
    assert!(
        cap.stderr.contains("contract violated: requires clause"),
        "a generic `requires` must abort; got stdout={:?}",
        cap.stdout
    );
    assert_ne!(
        cap.status.code(),
        Some(0),
        "the abort must exit nonzero, got {:?}",
        cap.status
    );

    // The same function, satisfied: no abort, no noise.
    let satisfied = r#"
fn gen_req[T](v: ref Vec[T]) -> i64
    requires v.len() > 1
where T: Ord
{ v.len() }
fn main() {
    let a: Vec[i64] = [1, 2, 3];
    let s: Vec[String] = ["x", "y"];
    println(gen_req(a));
    println(gen_req(s));
}
"#;
    // Two instantiations, so each mono has to carry its OWN frame rather
    // than inheriting one.
    assert_eq!(run_program(satisfied).as_deref(), Some("3\n2\n"));
}

/// The `ensures` half, including `old(...)` capture and an explicit
/// `return` (which exits through a different site than the tail).
#[test]
fn test_e2e_generic_fn_ensures_and_old_are_enforced() {
    // Tail return.
    let tail = r#"
fn gen_ens[T](v: ref Vec[T]) -> i64
    ensures(result) result > 100
where T: Ord
{ v.len() }
fn main() {
    let a: Vec[i64] = [1, 2, 3];
    println(gen_ens(a));
}
"#;
    let cap = run_program_capturing(tail).expect("build the tail-return program");
    assert!(
        cap.stderr.contains("contract violated: ensures clause"),
        "a generic tail-return `ensures` must abort; got stdout={:?}",
        cap.stdout
    );

    // Explicit `return` — the shared Return arm fires the checks off the
    // frame installed for this mono.
    let early = r#"
fn gen_ret[T](v: ref Vec[T]) -> i64
    ensures(result) result > 100
where T: Ord
{
    if v.len() > 0 { return v.len(); }
    999
}
fn main() {
    let a: Vec[i64] = [1, 2, 3];
    println(gen_ret(a));
}
"#;
    let cap = run_program_capturing(early).expect("build the early-return program");
    assert!(
        cap.stderr.contains("contract violated: ensures clause"),
        "a generic early-return `ensures` must abort; got stdout={:?}",
        cap.stdout
    );

    // `old(...)` pre-state inside a generic `ensures`: the correct
    // postcondition passes, the wrong one fires — so the snapshot is
    // actually captured rather than defaulting to something that happens
    // to satisfy the predicate.
    let old_ok = r#"
fn grow[T](v: mut ref Vec[T], x: T) -> i64
    ensures(result) result == old(v.len()) + 1
where T: Ord
{
    v.push(x);
    v.len()
}
fn main() {
    let mut a: Vec[i64] = [1, 2];
    println(grow(mut a, 3));
}
"#;
    assert_eq!(run_program(old_ok).as_deref(), Some("3\n"));

    let old_bad = r#"
fn bad[T](v: mut ref Vec[T], x: T) -> i64
    ensures(result) result == old(v.len())
where T: Ord
{
    v.push(x);
    v.len()
}
fn main() {
    let mut b: Vec[i64] = [1, 2];
    println(bad(mut b, 3));
}
"#;
    let cap = run_program_capturing(old_bad).expect("build the old() program");
    assert!(
        cap.stderr.contains("contract violated: ensures clause"),
        "`old(...)` in a generic `ensures` must be captured and compared; got stderr={:?}",
        cap.stderr
    );
}

/// The contract frame is SAVED and RESTORED around a monomorphized body,
/// because a mono is compiled INLINE inside its caller.
///
/// Both directions are pinned, and both were measured by disabling the
/// restore: a monomorphic caller's own `ensures` was silently lost (the
/// program printed its value instead of aborting), and a generic caller's
/// `ensures` failed codegen outright with "Undefined variable 'result'" as
/// the inner frame leaked outward. Installing the frame without the
/// save/restore would have traded one silent drop for another.
#[test]
fn test_e2e_generic_contract_frame_is_saved_across_nested_bodies() {
    // A MONOMORPHIC caller whose `ensures` must survive an inner generic
    // call compiled inside its body.
    let mono_outer = r#"
fn helper[T](v: ref Vec[T]) -> i64 where T: Ord { v.len() }
fn mono_outer(v: ref Vec[i64]) -> i64
    ensures(result) result > 100
{
    helper(v)
}
fn main() {
    let a: Vec[i64] = [1, 2, 3];
    println(mono_outer(a));
}
"#;
    let cap = run_program_capturing(mono_outer).expect("build the mono-outer program");
    assert!(
        cap.stderr.contains("contract violated: ensures clause"),
        "a monomorphic caller's `ensures` must survive an inner generic call; got stdout={:?}",
        cap.stdout
    );

    // A GENERIC caller whose own `ensures` must survive two inner generic
    // calls.
    let gen_outer = r#"
fn inner[T](v: ref Vec[T]) -> i64
    requires v.len() > 0
    ensures(result) result > 0
where T: Ord
{ v.len() }

fn outer[T](v: ref Vec[T]) -> i64
    requires v.len() > 1
    ensures(result) result > 100
where T: Ord
{
    inner(v) + inner(v)
}

fn main() {
    let a: Vec[i64] = [1, 2, 3];
    println(outer(a));
}
"#;
    let cap = run_program_capturing(gen_outer).expect("build the generic-outer program");
    assert!(
        cap.stderr.contains("contract violated: ensures clause"),
        "a generic caller's `ensures` must survive its inner monos; got stdout={:?}",
        cap.stdout
    );
}

/// design.md § Contracts' own worked example, generic as the spec writes
/// it: `requires haystack.is_sorted()` on `binary_search[T: Ord]`.
#[test]
fn test_e2e_generic_binary_search_contract_from_the_spec() {
    let src = r#"
fn binary_search[T](haystack: ref Vec[T], needle: ref T) -> Option[i64]
    requires haystack.is_sorted()
where T: Ord
{
    haystack.binary_search(needle)
}
fn main() {
    let sorted: Vec[i64] = [1, 3, 5, 7];
    match binary_search(sorted, 5) { Some(i) => { println(i); } None => { println("none"); } }
    let unsorted: Vec[i64] = [7, 3, 5, 1];
    match binary_search(unsorted, 5) { Some(i) => { println(i); } None => { println("none"); } }
}
"#;
    let cap = run_program_capturing(src).expect("build the spec example");
    assert!(
        cap.stdout.starts_with("2\n"),
        "the sorted haystack satisfies the precondition; got stdout={:?}",
        cap.stdout
    );
    assert!(
        cap.stderr.contains("contract violated: requires clause"),
        "the unsorted haystack must abort; got stdout={:?}",
        cap.stdout
    );
}

/// B-2026-08-22-12 — a method call through a return-position `impl Trait`.
///
/// The row's own repro (`let s = make(7); s.get()`) had NO codegen
/// dispatcher at all: check-green, `--interp`-green, red under both
/// `karac run` and `karac build`. The fix is a substitution in
/// `lowering.rs` rather than a dispatcher arm — the existential is a
/// caller-side abstraction the typechecker has finished enforcing by then,
/// and design.md guarantees one concrete witness per monomorphization, so
/// the backend can simply be handed the concrete type. `tests/lowering.rs`
/// pins the substitution; this pins that programs written against it RUN,
/// and run the same everywhere.
///
/// The witness matrix is the point. Each row is a different LLVM story,
/// and a substitution that only worked for a plain `{i64}` struct would
/// pass a thinner test while leaving the interesting cases red:
///
///   - a struct with a HEAP field (`Vec[i64]`) — the shape the
///     LLVM-identity reverse-lookup aliases against every other
///     `{ptr,len,cap}` type;
///   - an ENUM witness — tag+payload, not a struct at all;
///   - a `shared struct` witness — RC, a pointer with refcount discipline;
///   - the value returned from an IMPL METHOD rather than a free fn;
///   - the existential passed INTO an argument-position `impl Trait`,
///     which monomorphizes on a type param the call site could not name;
///   - two branches agreeing on one witness, still substitutable;
///   - an existential declaring CONCRETE method-use effects, whose verbs
///     are re-derived from the witness's own impl method.
///
/// Both spellings of the call are exercised throughout — bound to a `let`
/// first, and called directly on the return value — because they reach
/// codegen through different paths (`var_type_names` vs. a fresh temp).
#[test]
fn test_e2e_return_position_impl_trait_witness() {
    let src = r#"
effect resource Log;

trait Named {
    type Label;
    fn label(ref self) -> Self.Label;
}

struct Holder { items: Vec[i64] }
impl Named for Holder {
    type Label = String;
    fn label(ref self) -> String { f"holder:{self.items.len()}" }
}

enum Colour { Red, Blue }
impl Named for Colour {
    type Label = String;
    fn label(ref self) -> String {
        match self { Colour.Red => "red", Colour.Blue => "blue" }
    }
}

shared struct Node { name: String }
impl Named for Node {
    type Label = String;
    fn label(ref self) -> String { self.name }
}

struct Emitter { n: i64 }
impl Named for Emitter {
    type Label = String;
    fn label(ref self) -> String with writes(Log) { f"emit:{self.n}" }
}

fn a_holder(n: i64) -> impl Named[Label = String] { Holder { items: [n, n] } }
fn a_colour() -> impl Named[Label = String] { Colour.Blue }
fn a_node() -> impl Named[Label = String] { Node { name: "n1" } }
fn an_emitter() -> impl Named[Label = String] with writes(Log) { Emitter { n: 5 } }

// No inline binding — the caller annotates instead. Same substitution.
fn bare(n: i64) -> impl Named { Holder { items: [n] } }

// Both branches return the same concrete witness.
fn pick(hi: bool) -> impl Named[Label = String] {
    if hi { Holder { items: [1, 2, 3] } } else { Holder { items: [7] } }
}

// The existential flows INTO a generic (argument-position `impl Trait`),
// which must monomorphize on a type the call site cannot name.
fn shout(x: impl Named[Label = String]) -> String { x.label() }

struct Factory { seed: i64 }
impl Factory {
    fn build(ref self) -> impl Named[Label = String] { Holder { items: [self.seed] } }
}

fn main() {
    // Bound to a `let`, then called.
    let h = a_holder(4);
    println(h.label());
    // Called directly on the return value.
    println(a_holder(4).label());

    println(a_colour().label());
    println(a_node().label());
    println(an_emitter().label());

    let b = bare(9);
    let bl: String = b.label();
    println(bl);

    println(pick(true).label());
    println(pick(false).label());

    println(shout(a_holder(1)));

    let f = Factory { seed: 6 };
    println(f.build().label());
    let g = f.build();
    println(g.label());
}
"#;
    let expected =
            "holder:2\nholder:2\nblue\nn1\nemit:5\nholder:1\nholder:3\nholder:1\nholder:2\nholder:1\nholder:1\n";
    assert_eq!(run_program(src).as_deref(), Some(expected));
    // The A/B half, pinned rather than left to a manual check: this bug
    // WAS a run-vs-build divergence, so the interpreter answering
    // identically is the property under test, not a side note.
    assert_eq!(
        karac::run_program(src).join(""),
        expected,
        "interpreter and AOT must agree"
    );
}

/// B-2026-08-22-14 — the one return-position `impl Trait` shape that had
/// no build: an existential declaring polymorphic effect VARIABLES.
///
/// `substitute_impl_trait_returns` excluded it, so it stayed check-green
/// and `--interp`-green while both `karac run` and `karac build` failed.
/// The exclusion guarded a consumer that does not exist: the effect-var
/// harvester's only caller walks PARAMETER types and documents that
/// return-position `with E` slots are deliberately not tracked.
///
/// Pinned as an interp/AOT agreement test because the defect WAS a
/// run-vs-build divergence — the two backends answering identically is the
/// property under test. `an_effect_variable_existential_still_reports_its_effects`
/// in tests/lowering.rs is the paired control proving the substitution
/// does not hide an effect from the public-boundary check.
#[test]
fn test_e2e_return_position_impl_trait_with_effect_variables() {
    let src = r#"
effect resource Log;

trait Emit { fn emit(ref self) -> i64; }

struct S { v: i64 }
impl Emit for S { fn emit(ref self) -> i64 { self.v } }

struct L { v: i64 }
impl Emit for L { fn emit(ref self) -> i64 with writes(Log) { self.v } }

// The inert shape: `F` appears only in return position.
fn make[with F]() -> impl Emit with F { S { v: 7 } }

// The param-bound shape: `F` is bound by a parameter, so it is not inert.
fn wrap[with F](f: Fn() -> i64 with F) -> impl Emit with F { S { v: f() } }
fn src_pure() -> i64 { 5 }

// An effect variable alongside a concrete verb on the same clause.
fn mixed[with F]() -> impl Emit with writes(Log) F { L { v: 9 } }

fn main() {
    let e = make();
    println(e.emit());
    println(make().emit());
    println(wrap(src_pure).emit());
    println(mixed().emit());
}
"#;
    let expected = "7\n7\n5\n9\n";
    assert_eq!(run_program(src).as_deref(), Some(expected));
    assert_eq!(
        karac::run_program(src).join(""),
        expected,
        "interpreter and AOT must agree"
    );
}

/// B-2026-09-07-51 — the GENERIC leg of this family. `compile_function`
/// gates B-2026-08-30-28's conditional-store registration on
/// `func.generic_params.is_none()`, and a generic callee is compiled by
/// `compile_mono_function` instead, whose param loop carried the
/// conditional-RETURN sibling (B-2026-08-28-71) but not this one. So the
/// mono was one registration short and the non-storing path had no owner
/// anywhere: the caller stands down identically for both spellings, because
/// `fn_moves_param_into_outliving_place` is answered off the AST and knows
/// nothing about monomorphisation.
///
/// Pre-fix `c1` printed no `dI1` on the JIT and both AOT lanes against
/// `--interp`'s `dI1`, and lost the argument's `shared` refcount block
/// (16 B in 1 at -O0, 9 allocs / 8 frees). `c5` is the NON-generic twin of
/// the identical callee, correct throughout, which is what isolates
/// genericity as the whole difference.
///
/// `c2` is the storing path (the per-path flag must still suppress the
/// callee body so the container's drain is the only one), `c3` is a SECOND
/// monomorphisation in the same program (the flags are allocas in the
/// mono's own function, so they must not leak between specialisations), and
/// `c4` is an unconditional store, which keeps today's no-registration path.
#[test]
fn e2e_generic_conditional_store_runs_one_body_on_the_missed_path() {
    let Some(out) = run_program(
        "shared struct Inner { v: i64 }\n\
             struct Ri { id: i64, inner: Inner }\n\
             impl Drop for Ri {\n\
             \x20   fn drop(mut ref self) { println(f\"dI{self.id}\") }\n\
             }\n\
             fn mki(i: i64) -> Ri { return Ri { id: i, inner: Inner { v: i } }; }\n\
             struct Sp { id: i64, name: String }\n\
             impl Drop for Sp {\n\
             \x20   fn drop(mut ref self) { println(f\"dP{self.id}\") }\n\
             }\n\
             fn mkp(i: i64) -> Sp { return Sp { id: i, name: f\"p{i}\" }; }\n\
             struct Vi { mut xs: Vec[Ri] }\n\
             fn gcond[T](v: mut ref Vec[T], x: T, k: bool) { if k { v.push(x); } }\n\
             fn guncond[T](v: mut ref Vec[T], x: T) { v.push(x); }\n\
             fn fcond(b: mut ref Vi, r: Ri, k: bool) { if k { b.xs.push(r); } }\n\
             fn main() {\n\
             \x20   println(\"c1\");\n\
             \x20   let mut a: Vec[Ri] = Vec.new();\n\
             \x20   gcond(mut a, mki(1), false);\n\
             \x20   println(\"c2\");\n\
             \x20   gcond(mut a, mki(2), true);\n\
             \x20   println(f\"n{a.len()}\");\n\
             \x20   println(\"c3\");\n\
             \x20   let mut b: Vec[Sp] = Vec.new();\n\
             \x20   gcond(mut b, mkp(3), false);\n\
             \x20   println(\"c4\");\n\
             \x20   let mut c: Vec[Ri] = Vec.new();\n\
             \x20   guncond(mut c, mki(4));\n\
             \x20   println(f\"m{c.len()}\");\n\
             \x20   println(\"c5\");\n\
             \x20   let mut d = Vi { xs: Vec.new() };\n\
             \x20   fcond(mut d, mki(5), false);\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(
        out,
        "c1\ndI1\nc2\nn1\ndI2\nc3\ndP3\nc4\nm1\ndI4\nc5\ndI5\nend\n"
    );
}

// ── B-2026-09-03-17: assoc fn on a PRIMITIVE, compiled backends ────
//
// The row's defining property is that all THREE executors rejected what
// `karac check` accepted, so the interpreter tests alone would not have
// covered it: `karac run` (JIT) and `karac build` both failed with
// "no handler for method 'zero' on variable 'i64'". The cause sat in the
// parser (a lowercase primitive name never satisfied the `starts_upper`
// type test), which is why one upstream fix cleared all three at once —
// and why this pins the compiled half explicitly rather than trusting
// that shared root to stay shared.
#[test]
fn test_assoc_fn_on_primitive_type_compiles() {
    // Values are deliberately NON-ZERO. `compile_assoc_call` ends in a
    // silent `Ok(const 0)` fallback for an unrecognized `Type.method`, so
    // a `zero()` returning 0 would assert equally well against a real
    // dispatch and against that fallback — the test would pass while the
    // bug it guards was fully present.
    assert_eq!(
        run_program(
            "trait Zero { fn zero() -> Self; }\n\
                 impl Zero for i64 { fn zero() -> i64 { return 41; } }\n\
                 fn main() { let x: i64 = i64.zero(); println(x.to_string()); }\n"
        )
        .as_deref(),
        Some("41\n"),
        "a trait-impl associated function on a primitive must compile and run"
    );

    // Inherent impl — the row's scoping (c), not trait-specific.
    assert_eq!(
        run_program(
            "impl i64 { fn two() -> i64 { return 42; } }\n\
                 fn main() { let x: i64 = i64.two(); println(x.to_string()); }\n"
        )
        .as_deref(),
        Some("42\n"),
        "an inherent associated function on a primitive must compile and run"
    );

    // Non-integer primitives: the widths the `const 0` tail would get
    // wrong rather than merely returning a wrong integer for.
    assert_eq!(
        run_program(
            "trait Two { fn two() -> Self; }\n\
                 impl Two for bool { fn two() -> bool { return true; } }\n\
                 impl Two for f64 { fn two() -> f64 { return 45.5; } }\n\
                 fn main() { println(bool.two().to_string()); println(f64.two().to_string()); }\n"
        )
        .as_deref(),
        Some("true\n45.5\n"),
        "bool / f64 associated functions on primitives must compile at the right width"
    );

    // NEGATIVE CONTROL: associated CONSTANT access has no call parens and
    // must still lower as a field access, per the parser comment the fix
    // sits under.
    assert_eq!(
        run_program("fn main() { println(i64.MAX.to_string()); }\n").as_deref(),
        Some("9223372036854775807\n"),
        "`i64.MAX` must keep lowering as an associated constant, not a path call"
    );
}
