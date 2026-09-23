//! enum definitions, variants, discriminants, payloads -- fixtures for `tests/codegen.rs`.
//!
//! Split out of `tests/codegen.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test codegen` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test codegen enums::
//!
//! New fixtures about enum definitions, variants, discriminants, payloads belong in this file.

use super::*;

#[test]
/// B-2026-09-07-13 — the ENUM twin of
/// `test_e2e_stored_argument_is_owned_by_its_new_home_not_the_caller`, and
/// the row that twin's own fix note deferred.
///
/// A stored enum argument ran its `Drop` body TWICE on every compiled
/// backend in the FN-CALL spelling only: `b.put(mke(60))` over
/// `fn put(mut ref self, e: Ev) { self.xs.push(e); }` printed
/// `dEv a1 dEv` under `karac run` and at both opt levels against
/// `--interp`'s `a1 dEv`, while `b.put(Ev.A(77, ..))` — the same type into
/// the same callee — was correct everywhere.
///
/// THE SPELLING IS THE DISCRIMINATOR BECAUSE IT PICKS THE REGISTRAR ARM.
/// `track_inline_owned_aggregate_arg_inst`'s CTOR arm has stood down on the
/// escape path since B-2026-08-01-14 (memory alone via `track_enum_var`,
/// then return); its FN-CALL-RETURNED arm gated the same carve-out on
/// `is_struct`, with a note claiming "the enum leg's dual registration
/// below has no memory-half-alone form". The ctor arm was that form. So an
/// enum temp produced by a CALL fell through to the dual registration and
/// the caller became a second owner of a body the value's new home already
/// runs.
///
/// FOUR LEGS FROM ONE ARM. The method (`a`), free-function (`d`), assoc-fn
/// (`e`) and MONOMORPH (`f`) legs all doubled, and all four are fixed by
/// the single carve-out, because `method_call.rs`, `assoc_call.rs`,
/// `call_dispatch.rs` and `mono.rs` share this registrar. That is what
/// separates this row from B-2026-09-06-70's and B-2026-09-07-5's per-leg
/// gaps.
///
/// `b` AND `g`/`h` ARE THE CONTROLS, and they are the reason the fix is a
/// carve-out rather than a wider predicate. `b` is the ctor spelling, which
/// reaches the arm that was already right. `g` (`let z = passe(mkes(71))`)
/// is the RETURN route, whose registration the free-leg gate declines
/// outright — so this arm never runs for it, and its behaviour is unchanged
/// here by construction. `h` is a plain `let`, owned by its binding.
///
/// MEMORY IS UNTOUCHED, deliberately: valgrind reports the same 3
/// errors / 48 B at `-O2` and 4 / 51 B at `-O0` before and after, all of it
/// the `shared` handle's refcount block (B-2026-09-06-72's class, carried by
/// the CORRECT ctor cell too) plus the return route's own `-O0` residual.
/// No A/B leak gate could ever see this defect — it was one body too many,
/// not one byte.
///
/// Non-vacuous on the parent: five compiled surfaces print six doubled
/// bodies against a correct `--interp`.
fn test_e2e_stored_enum_argument_is_owned_by_its_new_home_not_the_caller() {
    let out = run_program(
        r#"
shared struct In2 { v: i64 }
enum Ev { A(i64, In2), B }
impl Drop for Ev { fn drop(mut ref self) { println("dEv") } }
fn mke(i: i64) -> Ev { return Ev.A(i, In2 { v: i }); }

enum Es { A(String), B }
impl Drop for Es { fn drop(mut ref self) { println("dEs") } }
fn mkes(i: i64) -> Es { return Es.A(f"e{i}"); }
impl Es { fn is_a(ref self) -> i64 { match self { Es.A(s) => { return 1; } Es.B => { return 0; } } } }

struct BoxE { mut xs: Vec[Ev] }
impl BoxE {
    fn put(mut ref self, e: Ev) { self.xs.push(e); }
    fn stash(b: mut ref BoxE, e: Ev) { b.xs.push(e); }
}
struct BoxS { mut ys: Vec[Es] }
impl BoxS { fn puts(mut ref self, e: Es) { self.ys.push(e); } }

fn pute(b: mut ref BoxS, e: Es) { b.ys.push(e); }
fn stashg[T](v: mut ref Vec[T], x: T) { v.push(x); }
fn passe(e: Es) -> Es { return e; }

fn c_meth()   { let mut b = BoxE { xs: Vec.new() }; b.put(mke(60)); println(f"a{b.xs.len()}"); }
fn c_ctor()   { let mut b = BoxE { xs: Vec.new() }; b.put(Ev.A(77, In2 { v: 1 })); println(f"b{b.xs.len()}"); }
fn c_meths()  { let mut d = BoxS { ys: Vec.new() }; d.puts(mkes(61)); println(f"c{d.ys.len()}"); }
fn c_free()   { let mut d = BoxS { ys: Vec.new() }; pute(mut d, mkes(76)); println(f"d{d.ys.len()}"); }
fn c_assoc()  { let mut b = BoxE { xs: Vec.new() }; BoxE.stash(mut b, mke(78)); println(f"e{b.xs.len()}"); }
fn c_generic(){ let mut v: Vec[Es] = Vec.new(); stashg(mut v, mkes(75)); println(f"f{v.len()}"); }
fn c_ret()    { let z = passe(mkes(71)); println(f"g{z.is_a()}"); }
fn c_plain()  { let e = mkes(79); println(f"h{e.is_a()}"); }

fn main() {
    c_meth(); c_ctor(); c_meths(); c_free(); c_assoc(); c_generic(); c_ret(); c_plain();
    println("end");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out, "a1\ndEv\nb1\ndEv\nc1\ndEs\nd1\ndEs\ne1\ndEv\nf1\ndEs\ng1\ndEs\nh1\ndEs\nend\n",
            "a callee that stores a CALL-produced enum argument owns its \
                 body; the caller keeps only the orphaned memory, exactly as \
                 the ctor spelling already did; got {out:?}"
        );
    }
}

#[test]
fn e2e_enum_method_owned_self_payload_no_double_free() {
    // B-2026-07-18-47: an enum METHOD with owned `self` matching its heap
    // payload double-freed under AOT/JIT (interp correct) — `self` parses as
    // SelfValue, which `suppress_destructured_enum_payload_cleanup` matched
    // only as Identifier, so it never cap-zeroed the consumed payload in the
    // source and `self`'s callee-owned enum-drop freed it AND the moved-out
    // binding freed it again. The enum-payload analogue of B-2026-07-18-37;
    // the free-fn `match e { … }` form already worked. Note it double-freed
    // even when the arm returns a SCALAR derived from the payload (`s.len()`)
    // — the payload binding itself is the second owner. Covers payload
    // returned, payload consumed-to-scalar, and a Vec payload.
    // Method names avoid the builtin Vec/String method namespace
    // (`get`/`take`/`unwrap`/…), which a separate dispatch bug misroutes
    // (B-2026-07-18-48).
    if let Some(out) = run_program(
        "enum E { V(String) }\n\
             enum W { X(Vec[i64]) }\n\
             impl E { fn extract(self) -> String { match self { E.V(s) => s } } }\n\
             impl E { fn length(self) -> i64 { match self { E.V(s) => s.len() } } }\n\
             impl W { fn firstval(self) -> i64 { match self { W.X(v) => v[0] } } }\n\
             fn main() {\n\
                 let e = E.V(\"hi\".to_string());\n\
                 println(e.extract());\n\
                 let e2 = E.V(\"abcd\".to_string());\n\
                 println(e2.length());\n\
                 let w = W.X([7, 8]);\n\
                 println(w.firstval());\n\
             }",
    ) {
        assert_eq!(out, "hi\n4\n7\n");
    }
}

// ── Enums ────────────────────────────────────────────────────

#[test]
fn test_ir_enum_unit_variants() {
    let ir = ir_for(
        r#"
enum Direction { North, South, East, West }
fn is_north(d: Direction) -> bool {
    match d {
        Direction.North => true,
        _ => false,
    }
}
"#,
    );
    // Enum should produce insertvalue for tag
    assert!(ir.contains("define"));
}

#[test]
fn test_ir_enum_tuple_variant() {
    let ir = ir_for(
        r#"
enum Maybe { Nothing, Just(i64) }
fn unwrap_or(m: Maybe, default: i64) -> i64 {
    match m {
        Maybe.Just(v) => v,
        Maybe.Nothing => default,
    }
}
"#,
    );
    assert!(ir.contains("define"));
    // Tag check in match
    assert!(ir.contains("icmp eq"));
}

/// B-2026-08-03-5 — a user type whose name collides with a generic enum's
/// PARAMETER name must not gain a phantom Drop walker.
///
/// A generic enum's variant payload `TypeExpr` is the parameter itself, so
/// `Option`'s payload reads as the type named `T`. Resolving that against
/// `struct_types` found the USER's `struct T` in any program that declared
/// one, and `Option` then looked like it carried a Drop-bearing payload:
/// `__karac_dropelems_enum_Option` was emitted alongside the real
/// `__karac_dropelems_opt_T` and ran a SECOND body over the erased layout's
/// words. Here that printed a garbage tag (an address, so it varied run to
/// run); with a heap field it would have freed through a garbage pointer.
///
/// ASan and LSan are both silent on this shape — the extra fire touches no
/// freed pointer — so only an output oracle can see it. Renaming the struct
/// to anything but `T` was the bisect.
#[test]
fn e2e_struct_named_like_enum_generic_param_gets_no_phantom_drop_walker() {
    let Some(out) = run_program(
        "struct T { t: i64, name: String }\n\
             impl Drop for T { fn drop(mut ref self) { println(f\"d{self.t}\"); } }\n\
             fn mk(t: i64) -> T { return T { t: t, name: \"payload-string-data\" }; }\n\
             fn main() {\n\
             \x20   { let p: Option[T] = Some(mk(2)); println(\"a\"); }\n\
             \x20   { let mut q: Option[T] = Some(mk(3)); q = Some(mk(4)); println(\"b\"); }\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(
        out,
        // Exactly one body per constructed value, and no phantom third
        // fire. Before the fix each block emitted an extra `d<garbage>`.
        "d2\na\nd3\nd4\nb\nend\n"
    );
}

/// B-2026-07-31-45 — `let…else` with a moved Drop-bearing enum payload
/// runs the payload's body exactly ONCE, at the escaped binding's NLL
/// end (after its last use). Pre-fix the interpreter ran it twice (the
/// source's un-disarmed walk before the binding use + the binding's
/// slot) and AOT ran it once via the source's walk while the binding
/// was still live. The miss edge diverges without a spurious fire.
/// Twin of `tests/interpreter.rs`'s
/// `test_let_else_moved_payload_single_drop`.
#[test]
fn e2e_let_else_moved_payload_single_drop() {
    let Some(out) = run_program(
        "struct Res { id: i64 }\n\
             impl Drop for Res {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"drop {self.id}\")\n\
             \x20   }\n\
             }\n\
             enum Box2 { Full(Res), Empty }\n\
             fn check(w: Box2) {\n\
             \x20   let Full(r2) = w else {\n\
             \x20       println(\"nope\")\n\
             \x20       return\n\
             \x20   }\n\
             \x20   println(f\"bound {r2.id}\")\n\
             }\n\
             fn main() {\n\
             \x20   let w = Box2.Full(Res { id: 52 });\n\
             \x20   println(\"a\");\n\
             \x20   let Full(r2) = w else {\n\
             \x20       println(\"nope\")\n\
             \x20       return\n\
             \x20   }\n\
             \x20   println(f\"bound {r2.id}\");\n\
             \x20   println(\"b\");\n\
             \x20   check(Box2.Full(Res { id: 53 }));\n\
             \x20   println(\"c\");\n\
             \x20   check(Box2.Empty);\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(
        out,
        "a\nbound 52\ndrop 52\nb\nbound 53\ndrop 53\nc\nnope\nend\n"
    );
}

/// B-2026-07-30-11 (enum-assign displacement) — overwriting an enum
/// binding runs the OLD variant's Drop-bearing payload body before the
/// store (`__karac_dropelems_enum_<E>` on the old slot), matching the
/// struct leg. Pre-fix `b = Box2.Empty;` over `Full(Res{5})` was
/// silent in both backends. Twin of `tests/interpreter.rs`'s
/// `test_enum_assign_displacement_runs_payload_body`, same source and
/// expected string.
#[test]
fn e2e_enum_assign_displacement_runs_payload_body() {
    let Some(out) = run_program(
        "struct Res { id: i64 }\n\
             impl Drop for Res {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"drop {self.id}\")\n\
             \x20   }\n\
             }\n\
             enum Box2 { Full(Res), Empty }\n\
             fn main() {\n\
             \x20   println(\"a: full -> empty\");\n\
             \x20   let mut b = Box2.Full(Res { id: 5 });\n\
             \x20   b = Box2.Empty;\n\
             \x20   println(\"b: full -> full\");\n\
             \x20   let mut c = Box2.Full(Res { id: 6 });\n\
             \x20   c = Box2.Full(Res { id: 7 });\n\
             \x20   println(\"c: empty -> full (no old body)\");\n\
             \x20   let mut d = Box2.Empty;\n\
             \x20   d = Box2.Full(Res { id: 8 });\n\
             \x20   println(\"d: moved-out then reassign (no double)\");\n\
             \x20   let mut e = Box2.Full(Res { id: 9 });\n\
             \x20   match e {\n\
             \x20       Box2.Full(r) => { println(f\"took {r.id}\"); }\n\
             \x20       Box2.Empty => {}\n\
             \x20   }\n\
             \x20   e = Box2.Empty;\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(
        out,
        "a: full -> empty\ndrop 5\nb: full -> full\ndrop 6\ndrop 7\n\
             c: empty -> full (no old body)\ndrop 8\n\
             d: moved-out then reassign (no double)\ntook 9\ndrop 9\nend\n"
    );
}

/// B-2026-08-01-2 — a discarded user-ENUM return's Drop-bearing payload
/// body fires at the `;` (`let _ = mk_enum();`, `mk_enum();`, and the
/// user-method siblings), where it used to be silent while `karac run`
/// fired — a run-vs-build divergence. The discard registrar now hangs
/// `__karac_dropelems_enum_<E>` on the one-shot frame, with the enum
/// drop switch pushed FIRST so the LIFO drain runs the body before the
/// payload's heap is freed (the `String` in each payload pins that
/// ordering — the reverse printed garbage). An erased-generic payload
/// stays silent on both — the declared-type rule.
///
/// B-2026-09-02-13 — the `Loud` rows read `loud drop` ALONE until that
/// row. "Own-`impl Drop` enums run their OWN body only" was this
/// fixture's stated rule and it was wrong against the bound-local
/// oracle: `let x = mk_loud(5);` prints `loud drop` then `drop 5 l5`,
/// and the wrapper and the payload walker are complementary
/// registrations for an enum rather than alternatives. Twin of `tests/interpreter.rs`'s
/// `test_enum_return_discard_runs_payload_body`, same source and
/// expected string.
#[test]
fn e2e_enum_return_discard_runs_payload_body() {
    // B-2026-09-10-2 — the `e` quadrant's ORACLE CHANGED. The note above
    // recorded that an erased-generic payload is one "codegen structurally
    // cannot see", so both backends were made silent there and this string
    // omitted `drop 7 g7` / `drop 8 g8`. Codegen can see it now: the
    // instantiation-keyed walker resolves `MyBox[Res]` through the enum's own
    // substitution, so a discarded generic return runs its payload body on all
    // three backends and no longer strands 32 B per call. Same move
    // B-2026-08-02-14 made one container over for a generic STRUCT FIELD —
    // parity holds in the FIRING direction now, not the silent-leak one.
    let Some(out) = run_program(
        "struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"drop {self.id} {self.name}\")\n\
             \x20   }\n\
             }\n\
             enum Box2 { Full(Res), Empty }\n\
             enum MyBox[T] { Wrap(T), Nil }\n\
             enum Loud { Hold(Res), Quiet }\n\
             impl Drop for Loud {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(\"loud drop\")\n\
             \x20   }\n\
             }\n\
             struct Fac { tag: i64 }\n\
             impl Fac {\n\
             \x20   fn make(ref self, n: i64) -> Box2 {\n\
             \x20       return Box2.Full(Res { id: n, name: f\"m{n}\" });\n\
             \x20   }\n\
             }\n\
             fn mk_enum(n: i64) -> Box2 {\n\
             \x20   return Box2.Full(Res { id: n, name: f\"h{n}\" });\n\
             }\n\
             fn mk_gen(n: i64) -> MyBox[Res] {\n\
             \x20   return MyBox.Wrap(Res { id: n, name: f\"g{n}\" });\n\
             }\n\
             fn mk_loud(n: i64) -> Loud {\n\
             \x20   return Loud.Hold(Res { id: n, name: f\"l{n}\" });\n\
             }\n\
             fn mk_empty() -> Box2 {\n\
             \x20   return Box2.Empty;\n\
             }\n\
             fn main() {\n\
             \x20   println(\"a\");\n\
             \x20   let _ = mk_enum(1);\n\
             \x20   println(\"b\");\n\
             \x20   mk_enum(2);\n\
             \x20   println(\"c\");\n\
             \x20   let f = Fac { tag: 0 };\n\
             \x20   let _ = f.make(3);\n\
             \x20   f.make(4);\n\
             \x20   println(\"d\");\n\
             \x20   let _ = mk_loud(5);\n\
             \x20   mk_loud(6);\n\
             \x20   println(\"e\");\n\
             \x20   let _ = mk_gen(7);\n\
             \x20   mk_gen(8);\n\
             \x20   println(\"f\");\n\
             \x20   let _ = mk_empty();\n\
             \x20   mk_empty();\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(
            out,
            "a\ndrop 1 h1\nb\ndrop 2 h2\nc\ndrop 3 m3\ndrop 4 m4\nd\nloud drop\ndrop 5 l5\nloud drop\ndrop 6 l6\ne\ndrop 7 g7\ndrop 8 g8\nf\nend\n"
        );
}

/// B-2026-07-30-11 (own-Drop enum reassign leg) + B-2026-08-01-3 — the
/// displaced old value of an own-`impl Drop` enum reassign fires its own
/// body then its payload bodies at the assignment (the struct twin's
/// order), and the displaced payload's heap is freed (the eager-free
/// ladder gained its enum leg). The matrix also pins: whole-value move +
/// reassign stays silent for the new value (m2 — the interpreter's Drop
/// action was name-retracted at the move), payload move-out + reassign
/// fires nothing at the assign site but re-arms the walker so the NEW
/// value's payload body fires at exit AFTER the own body (m3 — the
/// insert-before-own placement), a self-mention RHS is skipped (m4), and
/// Quiet transitions fire exactly once per held payload (m5). Twin of
/// `tests/interpreter.rs`'s `test_own_drop_enum_reassign_sequencing`.
#[test]
fn e2e_own_drop_enum_reassign_sequencing() {
    let Some(out) = run_program(
        "struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"drop {self.id} {self.name}\")\n\
             \x20   }\n\
             }\n\
             enum Loud { Hold(Res), Quiet }\n\
             impl Drop for Loud {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(\"loud drop\")\n\
             \x20   }\n\
             }\n\
             fn mk_loud(n: i64) -> Loud {\n\
             \x20   return Loud.Hold(Res { id: n, name: f\"l{n}\" });\n\
             }\n\
             fn use_res(r: Res) {\n\
             \x20   println(f\"took {r.id}\");\n\
             }\n\
             fn pass(b: Loud) -> Loud {\n\
             \x20   return b;\n\
             }\n\
             fn main() {\n\
             \x20   println(\"m1: plain reassign\");\n\
             \x20   let mut a = mk_loud(1);\n\
             \x20   a = mk_loud(2);\n\
             \x20   println(\"m1 end\");\n\
             \x20   println(\"m2: reassign after whole-value move\");\n\
             \x20   let mut b = mk_loud(3);\n\
             \x20   let c = b;\n\
             \x20   b = mk_loud(4);\n\
             \x20   println(\"m2 end\");\n\
             \x20   println(\"m3: reassign after payload move-out\");\n\
             \x20   let mut d = mk_loud(5);\n\
             \x20   match d {\n\
             \x20       Loud.Hold(r) => { use_res(r); }\n\
             \x20       Loud.Quiet => {}\n\
             \x20   }\n\
             \x20   d = mk_loud(6);\n\
             \x20   println(\"m3 end\");\n\
             \x20   println(\"m4: self-mention\");\n\
             \x20   let mut e = mk_loud(7);\n\
             \x20   e = pass(e);\n\
             \x20   println(\"m4 end\");\n\
             \x20   println(\"m5: quiet-to-full and full-to-quiet\");\n\
             \x20   let mut g = mk_loud(8);\n\
             \x20   g = Loud.Quiet;\n\
             \x20   g = mk_loud(9);\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(
            out,
            "m1: plain reassign\nloud drop\ndrop 1 l1\nloud drop\ndrop 2 l2\nm1 end\n\
             m2: reassign after whole-value move\nloud drop\ndrop 3 l3\nm2 end\n\
             m3: reassign after payload move-out\ntook 5\ndrop 5 l5\nloud drop\ndrop 6 l6\nm3 end\n\
             m4: self-mention\nloud drop\ndrop 7 l7\nm4 end\n\
             m5: quiet-to-full and full-to-quiet\nloud drop\ndrop 8 l8\nloud drop\nloud drop\ndrop 9 l9\nend\n"
        );
}

/// B-2026-08-29-37 — A DEFENSIVE COPY OF THE MATCH SCRUTINEE MUST NOT RUN
/// THE ENUM'S OWN `Drop` BODY. Twin of `tests/interpreter.rs`'s
/// `test_scrutinee_clone_does_not_rerun_the_enum_own_drop_body`, pinned to
/// the same string.
///
/// `materialize_freshtemp_enum_scrutinee` stages the scrutinee into
/// `__freshtemp_enum_scrut` and used to register BOTH halves of the enum's
/// drop on it — the memory glue AND `karac_drop_<E>`, the user body. That is
/// right for the one population the registration was written for (a genuine
/// fresh temp, B-2026-07-11-26: nobody else owns it, so its body runs here
/// or nowhere). It is wrong for the four DEFENSIVE-COPY legs that reach the
/// same code through `force`/`heap_index`, where the source still owns an
/// equal value and runs the body itself — so the body fired twice, and the
/// second one was an artifact of a copy the source program never asked for.
///
/// Pre-fix, measured on `f29da14`: `refchain` printed `dR1 dE got 2` — the
/// `dE` INSIDE the callee, on a struct it only borrows — and `loop` / `index`
/// each printed a doubled `dE dE`. The interpreter, which never copies,
/// printed one `dE` per leg on every one of them.
///
/// FOUR LEGS PLUS THE CONTROL, because the fix keys on the one distinction
/// that separates them (`expr_yields_fresh_owned_temp`) rather than on the
/// reported shape:
///   - `refchain` — `match <refparam>.field` (B-2026-07-21-5/-6's clone),
///     the shape the row reported, here through a `ref self` method's
///     free-function twin.
///   - `loop` — a `for`-loop element (B-2026-07-14-1's clone).
///   - `index` — `match v[0]` over a heap element (`heap_index`, the leg
///     that reaches materialization without `force` at all).
///   - `fresh` — THE CONTROL. `match mk(4)` is a real owned temp with no
///     other owner, so its `dE` MUST still fire; if the fix over-reached and
///     silenced this one, B-2026-07-11-26 regresses and this leg catches it.
///
/// The arms use `let m = r` rather than a consuming call on purpose: a
/// consuming `eat(r)` makes the INTERPRETER skip the payload's own body,
/// which is a separate divergence this fixture must not be sensitive to.
///
/// The `index` leg is the ONE exception, and B-2026-08-31-3 is why: that
/// divergence turned out to be a program design.md does not sanction —
/// `v[i]` is a borrow of an element the container still owns, so no arm
/// binding may be consumed out of it — and `let m = r` is a consume just as
/// much as `eat(r)` is. So that leg reads THROUGH the binding instead. The
/// other three legs are untouched — a `ref`-chain field, a `for`-loop
/// element and a fresh temp are all owned or borrowed places the rule has
/// nothing to say about.
///
/// B-2026-09-02-11 then CHANGED the `index` leg's expectation, and the
/// reason is worth keeping: `got 4 dR3 dE dR3` was never the interpreter's
/// answer for the read-only spelling, only for the `let m = r` one this
/// fixture used to carry. The migration above left the compiled string
/// pinned to the old program's count, so the leg silently asserted a
/// two-body answer against a one-body source. It is now `got 4 dE dR3` on
/// every surface — the container destroys its element once, at its own NLL
/// death, and the defensive clone's payload binding no longer mourns a copy
/// the source never named.
#[test]
fn e2e_defensive_scrutinee_copy_does_not_rerun_the_enum_own_drop_body() {
    let Some(out) = run_program(SCRUTINEE_CLONE_DROP_BODY_SRC) else {
        return;
    };
    assert_eq!(out, SCRUTINEE_CLONE_DROP_BODY_EXPECTED);
}

/// B-2026-09-02-17 — THE `let … else` SPELLING OVER AN INDEXED ELEMENT RUNS
/// EXACTLY ONE PAYLOAD `Drop` BODY, AND IT IS THE CONTAINER'S.
///
/// The last member of the `v[i]` family to get a fixture, and the only one that
/// diverged. `v[i]` evaluates to `ref T`, so a binding taken out of one is a
/// view of a defensive clone (B-2026-09-02-11) and the container runs the body
/// at its own NLL death. `match` / `if let` / `while let` all reach that answer
/// through `scrutinee_expr_is_consuming`, which has no `Index` arm and so
/// answers false. `let … else` never asked: it binds through `bind_pattern`
/// directly and `push_drops_for_stmt` registered a real slot per name, so the
/// body ran TWICE — once via the container's still-armed walk, once via the
/// binding's own slot.
///
/// WHICH COUNT IS RIGHT WAS THE ROW'S OPEN QUESTION, and `liveafter` is the leg
/// that answers it. The row's second reading was that `let … else` binds into
/// the ENCLOSING scope, so `r` outlives the container's death and "a value the
/// program observes arguably earns its own body". `liveafter` reads `v` AFTER
/// `r`, so the container outlives the binding and no such escape exists — and
/// the interpreter doubled there too, pre-fix. The extra body was never the
/// escape's consequence, which also retires the row's third reading (reject the
/// form, per B-2026-08-31-3): there is nothing to reject in a program whose
/// borrow never dangles.
///
/// `temp` IS THE CONTROL THAT SHAPED THE GATE. Over a TEMPORARY container
/// (`mkv(8)[0]`) nothing else owns the element, every surface already ran one
/// body, and that body is the BINDING's — `got 9` then `dR8`. Suppressing there
/// would lose it entirely rather than merely mistime it, so the gate reuses
/// `place_walk_is_retractable`, the same "is there an owner to hand back to"
/// walk the disarm family uses: an identifier root or a one-hop field chain,
/// and nothing else.
///
/// THE DEFECT IS THE BINDING SLOT, NOT THE ENUM PAYLOAD. `struct` — a bare
/// `let W { r, k } = v[0] else { … }` — carries it with no enum anywhere, and
/// `opt` carries it on the seeded pair. `field` (`h.xs[0]`) and `two` (`v[1]`
/// of two elements, where the container walks BOTH) were not named in the row.
///
/// Pre-fix, measured per leg and on `--interp` ONLY: `bare` `dE dR1 got 2 dR1`,
/// `liveafter` `got 3 dR2 len 1 dE dR2`, `field` `dE dR3 got 4 dR3`, `two`
/// `dE dR4 dE dR5 got 6 dR5`, `opt` `dR6 got 7 dR6`, `struct` `dR7 got 14 dR7`
/// — one extra body each. `temp`, `iflet` and `elsetaken` were identical before
/// and after, and all three compiled surfaces were correct throughout.
///
/// Pinned to the same program and the same string as `tests/interpreter.rs`'s
/// `test_let_else_over_an_indexed_element_runs_one_payload_body`. The
/// compiled backends were CORRECT throughout; this is the oracle half.
#[test]
fn e2e_let_else_over_an_indexed_element_runs_one_payload_body() {
    let Some(out) = run_program(
        r#"struct R { id: i64, v: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("dE") } }
struct H { xs: Vec[E] }
struct W { r: R, k: i64 }

fn mkr(n: i64) -> R { let mut v: Vec[i64] = Vec.new(); v.push(n); return R { id: n, v: v } }
fn mk(n: i64) -> E { return E.A(mkr(n)) }
fn mkw(n: i64) -> W { return W { r: mkr(n), k: n } }
fn mkv(n: i64) -> Vec[E] { let mut v: Vec[E] = Vec.new(); v.push(mk(n)); return v }

#[allow(partial_move_of_drop_enum)]
fn leg_bare() {
    println("bare");
    let mut v: Vec[E] = Vec.new();
    v.push(mk(1));
    let E.A(r) = v[0] else { println("no"); return }
    println(f"got {r.id + r.v.len()}");
    println("bare end");
}

#[allow(partial_move_of_drop_enum)]
fn leg_live_after() {
    println("liveafter");
    let mut v: Vec[E] = Vec.new();
    v.push(mk(2));
    let E.A(r) = v[0] else { println("no"); return }
    println(f"got {r.id + r.v.len()}");
    println(f"len {v.len()}");
    println("liveafter end");
}

#[allow(partial_move_of_drop_enum)]
fn leg_field() {
    println("field");
    let mut xs: Vec[E] = Vec.new();
    xs.push(mk(3));
    let h = H { xs: xs };
    let E.A(r) = h.xs[0] else { println("no"); return }
    println(f"got {r.id + r.v.len()}");
    println("field end");
}

#[allow(partial_move_of_drop_enum)]
fn leg_two() {
    println("two");
    let mut v: Vec[E] = Vec.new();
    v.push(mk(4));
    v.push(mk(5));
    let E.A(r) = v[1] else { println("no"); return }
    println(f"got {r.id + r.v.len()}");
    println("two end");
}

fn leg_opt() {
    println("opt");
    let mut v: Vec[Option[R]] = Vec.new();
    v.push(Option.Some(mkr(6)));
    let Option.Some(r) = v[0] else { println("no"); return }
    println(f"got {r.id + r.v.len()}");
    println("opt end");
}

fn leg_struct() {
    println("struct");
    let mut v: Vec[W] = Vec.new();
    v.push(mkw(7));
    let W { r, k } = v[0] else { println("no"); return }
    println(f"got {r.id + k}");
    println("struct end");
}

#[allow(partial_move_of_drop_enum)]
fn leg_temp() {
    println("temp");
    let E.A(r) = mkv(8)[0] else { println("no"); return }
    println(f"got {r.id + r.v.len()}");
    println("temp end");
}

fn leg_iflet() {
    println("iflet");
    let mut v: Vec[E] = Vec.new();
    v.push(mk(9));
    if let E.A(r) = v[0] { println(f"got {r.id + r.v.len()}") }
    println("iflet end");
}

#[allow(partial_move_of_drop_enum)]
fn leg_elsetaken() {
    println("elsetaken");
    let mut v: Vec[E] = Vec.new();
    v.push(E.B);
    let E.A(r) = v[0] else { println("no"); return }
    println(f"got {r.id}");
}

fn main() {
    leg_bare();
    leg_live_after();
    leg_field();
    leg_two();
    leg_opt();
    leg_struct();
    leg_temp();
    leg_iflet();
    leg_elsetaken();
    println("end");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"bare
dE
dR1
got 2
bare end
liveafter
got 3
len 1
dE
dR2
liveafter end
field
dE
dR3
got 4
field end
two
dE
dR4
dE
dR5
got 6
two end
opt
dR6
got 7
opt end
struct
dR7
got 14
struct end
temp
got 9
dR8
temp end
iflet
got 10
dE
dR9
iflet end
elsetaken
no
dE
end
"#
    );
}

/// B-2026-08-31-1 — A PAYLOAD BOUND OUT OF A *PROJECTION* OF AN OWNED PARAM IS
/// A VIEW, AND THE VIEW-NESS MUST PROPAGATE THROUGH A REBIND.
///
/// Under caller-retains (B-2026-08-01-13), a payload destructured from an owned
/// by-value param belongs to the CALLER's fire, so the arm binding registers no
/// Drop slot of its own. B-2026-08-29-17 made that view-ness propagate through
/// `let m = r`, but keyed the propagation on the scrutinee being a bare
/// `Identifier` — so `match s.e { E.A(r) => { let m = r; ... } }` inside
/// `fn take(s: S)` left `r` out of the view set, `m` took a slot, and this
/// backend ran the body the caller was already running.
///
/// Codegen never had the hole: its twin predicate walks field and tuple-index
/// hops to the root (B-2026-08-03-3 leg B), so the two sides now ask the same
/// question. Pre-fix this program printed FIVE extra `dR` lines against both
/// compiled backends — one each for `enum`, `opt`, `res`, `two` and `sv`.
///
/// THE FIVE CONTROLS ARE THE POINT, because the fix withholds a body and the
/// failure mode of over-reaching is a body that never runs at all:
/// - `tup` — a plain TUPLE pattern over an owned param's tuple field. NO LONGER A
///   CONTROL. It ran two bodies on every surface when this test was written, and
///   this bullet recorded that -31-1 deliberately left it alone: codegen
///   view-marked only VARIANT payload bindings, so withholding on the interpreter
///   alone would have turned an agreed answer into a new divergence.
///   B-2026-08-31-7 repaired the codegen side instead —
///   `collect_bare_tuple_binding_names` marks a bare-tuple element bound out of an
///   owned-param scrutinee as a param VIEW too, without routing it to the arm
///   channel a variant payload takes — so both columns moved to ONE body together
///   and the expectation below now carries `dR6` once.
/// - `ref` — a `ref` param, not owned, so no caller-retains: the arm keeps its slot.
/// - `local` — an owned LOCAL projection, where the arm binding really is the owner.
/// - `fresh` — a fresh-temp scrutinee, likewise.
/// - `norebind` — the same owned-param projection WITHOUT the rebind, which was
///   already correct and must stay so.
///
/// Twin of `tests/interpreter.rs`'s
/// `test_owned_param_projection_payload_rebind_is_a_view`, pinned to the same
/// string. The compiled backends were already correct here; this side exists
/// so a future codegen change cannot drift away from the oracle unnoticed.
#[test]
fn e2e_owned_param_projection_payload_rebind_is_a_view() {
    let Some(out) = run_program(
        r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("dE") } }
enum Sv { Hold { inner: R }, Nil }
impl Drop for Sv { fn drop(mut ref self) { println("dSv") } }

struct S { e: E }
struct Ob { o: Option[R] }
struct Rb { r: Result[R, i64] }
struct Tb { t: (R, i64) }
struct W2 { s: S }
struct Hs { v: Sv }

fn mk(n: i64) -> E { return E.A(R { id: n }) }

#[allow(partial_move_of_drop_enum)]
fn f_enum(s: S)  { match s.e { E.A(r) => { let m = r; println(f"  b{m.id}"); } E.B => { } } }
fn f_opt(s: Ob)  { match s.o { Option.Some(r) => { let m = r; println(f"  b{m.id}"); } Option.None => { } } }
fn f_res(s: Rb)  { match s.r { Result.Ok(r) => { let m = r; println(f"  b{m.id}"); } Result.Err(e) => { } } }
#[allow(partial_move_of_drop_enum)]
fn f_two(w: W2)  { match w.s.e { E.A(r) => { let m = r; println(f"  b{m.id}"); } E.B => { } } }
fn f_sv(h: Hs)   { match h.v { Sv.Hold { inner } => { let m = inner; println(f"  b{m.id}"); } Sv.Nil => { } } }
fn f_tup(s: Tb)  { match s.t { (r, k) => { let m = r; println(f"  b{m.id}"); } } }
#[allow(partial_move_of_drop_enum)]
fn f_ref(s: ref S) { match s.e { E.A(r) => { let m = r; println(f"  b{m.id}"); } E.B => { } } }
fn f_norebind(s: S) { match s.e { E.A(r) => { println(f"  b{r.id}"); } E.B => { } } }
#[allow(partial_move_of_drop_enum)]
fn f_local() { let s = S { e: mk(8) }; match s.e { E.A(r) => { let m = r; println(f"  b{m.id}"); } E.B => { } } }
#[allow(partial_move_of_drop_enum)]
fn f_fresh() { match mk(9) { E.A(r) => { let m = r; println(f"  b{m.id}"); } E.B => { } } }

fn main() {
    println("enum");     f_enum(S { e: mk(1) });                              println("enum end");
    println("opt");      f_opt(Ob { o: Option.Some(R { id: 2 }) });           println("opt end");
    println("res");      f_res(Rb { r: Result.Ok(R { id: 3 }) });             println("res end");
    println("two");      f_two(W2 { s: S { e: mk(4) } });                     println("two end");
    println("sv");       f_sv(Hs { v: Sv.Hold { inner: R { id: 5 } } });      println("sv end");
    println("tup");      f_tup(Tb { t: (R { id: 6 }, 0) });                   println("tup end");
    println("ref");      let a = S { e: mk(7) }; f_ref(a);                    println("ref end");
    println("local");    f_local();                                           println("local end");
    println("fresh");    f_fresh();                                           println("fresh end");
    println("norebind"); f_norebind(S { e: mk(10) });                         println("norebind end");
    println("done");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"enum
  b1
dE
dR1
enum end
opt
  b2
dR2
opt end
res
  b3
dR3
res end
two
  b4
dE
dR4
two end
sv
  b5
dSv
dR5
sv end
tup
  b6
dR6
tup end
ref
  b7
dR7
dE
dR7
ref end
local
  b8
dR8
dE
local end
fresh
  b9
dR9
dE
fresh end
norebind
  b10
dE
dR10
norebind end
done
"#
    );
}

/// B-2026-09-10-25 — A **BARE** ENUM-VARIANT CONSTRUCTOR AS A TUPLE ELEMENT
/// MUST RUN ITS `Drop` WORK, EXACTLY AS THE QUALIFIED SPELLING ABOVE DOES.
///
/// `optArg((Some(mk(11)), 7))` printed nothing on `--interp`, the JIT and
/// both builds, where one `dR11` is owed — an AGREED gap, so no parity rule
/// saw it. `qualtemp`, one line down, is the same call with
/// `Option.Some(..)` and was correct on all four throughout: the
/// discriminator is the CONSTRUCTOR SPELLING, not the argument position the
/// filing row blamed.
///
/// TWO SITES, and neither is sufficient alone. The freshness gate
/// (`discard_tuple_elem_is_fresh_expr`, and its interpreter twin) admitted a
/// `Path` callee and, for an `Identifier` callee, only a user FUNCTION name
/// — `Some` is neither, so the element read as non-fresh, and ONE non-fresh
/// element disqualifies the whole literal, which is why `mixed` lost its
/// plain-struct element's body too. Widening only that gate left codegen
/// registering the walk and still skipping the `Option` element, because
/// `infer_arg_elem_te`'s B-2026-09-03-21 arm recovers the payload type from
/// a 2-segment PATH callee only and a bare ctor fell through to the bare
/// head `Option` with no generic args, which every optres walker declines.
/// Measured in that intermediate state: `mixed` printed `dR19` and not
/// `dR18` compiled while the interpreter printed both — a run-vs-build
/// divergence manufactured by half a fix, which is why both sites and both
/// backends land in one commit.
///
/// `discard` is the second POSITION the one gate governs (`let _ = (..)`,
/// and the bare-statement spelling with it), broken identically and fixed
/// by the same change — the filing row reports only the argument one.
/// `movedplace` pins exactly ONE body where the payload is a moved binding,
/// `barenone` that a payload-free variant runs nothing, `bareuv` / `qualuv`
/// that a user enum behaves the same both ways, and `bareok` the `Result`
/// head of the bare spelling.
///
/// SHARED / `par` enums are deliberately EXCLUDED from the widened gate and
/// are pinned separately by `e2e_bare_shared_enum_tuple_elem_stays_silent`.
/// Their QUALIFIED spelling is already a run-vs-build divergence
/// (B-2026-09-17-19), so admitting the bare one would have opened a second
/// divergence rather than closed a gap.
///
/// Memory is UNMOVED by this change — every cell reports byte-identical
/// valgrind numbers before and after, which is the whole of its memory
/// claim. It is NOT a claim that these shapes are clean: a tuple whose only
/// heap-bearing element is the `Option` (`(Option[R], i64)`) already leaked
/// 32 B + 1 B at `-O0` on the parent tree, in the bare and qualified
/// spellings alike, and still does. That leak is orthogonal, pre-existing
/// and filed separately; the sibling fixture above is clean because its
/// tuple carries a plain `R` element as well.
///
/// Twin of `tests/interpreter.rs`'s
/// `test_bare_variant_ctor_tuple_elem_runs_its_drop_body`, pinned to the
/// same string.
#[test]
fn e2e_bare_variant_ctor_tuple_elem_runs_its_drop_body() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}") } }
enum W { A(R), N }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}" }; }

fn optArg(t: (Option[R], i64)) -> i64 { println(f"  in{t.1}"); return 0; }
fn twoArg(t: (Option[R], Option[R])) -> i64 { println("  in2"); return 0; }
fn sndArg(t: (i64, Option[R])) -> i64 { println(f"  in{t.0}"); return 0; }
fn uvArg(t: (W, i64)) -> i64 { println(f"  in{t.1}"); return 0; }
fn mixArg(t: (Option[R], R)) -> i64 { println(f"  in{t.1.id}"); return 0; }
fn nestArg(t: ((Option[R], i64), i64)) -> i64 { println(f"  in{t.1}"); return 0; }
fn resArg(t: (Result[R, i64], i64)) -> i64 { println(f"  in{t.1}"); return 0; }

fn main() {
    println("baretemp");   let _ = optArg((Some(mk(11)), 7));                println("baretemp end")
    println("qualtemp");   let _ = optArg((Option.Some(mk(12)), 7));         println("qualtemp end")
    println("twobare");    let _ = twoArg((Some(mk(13)), Some(mk(14))));     println("twobare end")
    println("secondpos");  let _ = sndArg((7, Some(mk(15))));                println("secondpos end")
    println("bareuv");     let _ = uvArg((A(mk(16)), 7));                    println("bareuv end")
    println("qualuv");     let _ = uvArg((W.A(mk(17)), 7));                  println("qualuv end")
    println("mixed");      let _ = mixArg((Some(mk(18)), mk(19)));           println("mixed end")
    println("nested");     let _ = nestArg(((Some(mk(20)), 7), 9));          println("nested end")
    println("bareok");     let _ = resArg((Ok(mk(21)), 7));                  println("bareok end")
    println("barenone");   let _ = optArg((None, 7));                        println("barenone end")
    println("discard");    let _ = (Some(mk(22)), 7);                        println("discard end")
    println("movedplace"); let r = mk(23); let _ = optArg((Some(r), 7));     println("movedplace end")
    println("done")
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"baretemp
  in7
dR11/t11
baretemp end
qualtemp
  in7
dR12/t12
qualtemp end
twobare
  in2
dR13/t13
dR14/t14
twobare end
secondpos
  in7
dR15/t15
secondpos end
bareuv
  in7
dR16/t16
bareuv end
qualuv
  in7
dR17/t17
qualuv end
mixed
  in19
dR18/t18
dR19/t19
mixed end
nested
  in9
dR20/t20
nested end
bareok
  in7
dR21/t21
bareok end
barenone
  in7
barenone end
discard
dR22/t22
discard end
movedplace
  in7
dR23/t23
movedplace end
done
"#
    );
}

/// B-2026-09-10-25's CARVE-OUT, pinned so it cannot be widened by accident.
///
/// A `shared` / `par` enum's drop is refcount-driven, and the two backends
/// do not agree about this shape: the QUALIFIED `(Sh.S(mk(1)), 7)` runs the
/// payload body under `--interp` and on NO compiled surface, in the
/// argument and `let _ =` positions alike. That divergence predates this
/// row and is B-2026-09-17-19, which owns the same lost body one wrapping
/// in (a BARE `shared enum` local); these tuple-element cells are a second
/// repro of it rather than a separate defect. What matters here is that the
/// BARE spelling
/// is AGREED-silent, so the gate widened by B-2026-09-10-25 excludes shared
/// and `par` heads. Admitting them would have converted an agreed gap into
/// a second divergence — strictly worse.
///
/// The BARE cell below agrees on all four surfaces (silent), which is why
/// it is a genuine twin of
/// `tests/interpreter.rs`'s
/// `test_bare_shared_enum_tuple_elem_stays_silent` and pinned to the same
/// string. It is pinning a KNOWN GAP rather than correct behaviour: when
/// the qualified divergence is fixed, both halves move together and this
/// expectation is what should change.
#[test]
fn e2e_bare_shared_enum_tuple_elem_stays_silent() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}") } }
shared enum Sh { S(R), Z }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}" }; }
fn shArg(t: (Sh, i64)) -> i64 { println(f"  in{t.1}"); return 0; }
fn main() {
    println("bare"); let _ = shArg((S(mk(1)), 7)); println("bare end")
    println("done")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "bare\n  in7\nbare end\ndone\n");
}

/// B-2026-09-16-17 + B-2026-09-06-21 — the COMPILED twin of
/// `test_enum_payload_drops_reverse_and_readonly_arm_bindings_are_borrows`,
/// byte-identical source and byte-identical expectation. That identity is
/// the point: these two defects were an ordering question, and an ordering
/// question is only settled when both backends print the same bytes AND
/// those bytes are the ones design.md names.
///
/// THE ENUM HALF WAS AN AGREED GAP, so no A/B check could have found it.
/// design.md § `Drop` Field drop order pins reverse declaration order for a
/// struct "or enum variant" alike; the struct walker complied and the enum
/// one ran declaration order, on all four surfaces at once —
/// `struct P2 { a: R, b: R }` printed `dR2 dR1` while
/// `enum E2 { T(R, R) }` printed `dR1 dR2`. Both backends being wrong
/// together is exactly the class the A/B rule is blind to, which is why the
/// `structs()` line is kept here beside `enums()`: it is the control that
/// makes the asymmetry visible in one transcript.
///
/// THE ARM half is codegen's control rather than its fix — the compiled
/// backends already treated a read-only arm binding as a borrow of the
/// husk, which design.md § Match Arm Binding Modes says is correct
/// ("bindings that are only read BORROW from the already-owned value").
/// B-2026-09-06-21 concluded the opposite, that codegen was the side to
/// move; it was the interpreter. This test pins the compiled side so a
/// future attempt to "fix" it here fails loudly.
#[test]
fn e2e_enum_payload_fields_drop_in_reverse_declaration_order() {
    let Some(out) = run_program(
            "struct R { id: i64, name: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             fn mk(i: i64) -> R { return R { id: i, name: f\"nnnnnnnn{i}\" } }\n\
             struct P2 { a: R, b: R }\n\
             enum E2 { T(R, R), N }\n\
             enum E1 { O(R), N1 }\n\
             fn take(r: R) -> i64 { return r.id }\n\
             fn structs() { let p: P2 = P2 { a: mk(1), b: mk(2) }; println(\"-structs\") }\n\
             fn enums() { let w: E2 = E2.T(mk(1), mk(2)); println(\"-enums\") }\n\
             fn two_locals() -> i64 {\n\
             \x20   let w: E2 = E2.T(mk(1), mk(2));\n\
             \x20   let z: R = mk(3);\n\
             \x20   match w { E2.T(a, b) => { return a.id + z.id; } E2.N => { return 0; } }\n\
             }\n\
             fn single() -> i64 {\n\
             \x20   let w: E1 = E1.O(mk(4));\n\
             \x20   let z: R = mk(5);\n\
             \x20   match w { E1.O(a) => { return a.id + z.id; } E1.N1 => { return 0; } }\n\
             }\n\
             fn wildcard() -> i64 {\n\
             \x20   let w: E2 = E2.T(mk(6), mk(7));\n\
             \x20   let z: R = mk(8);\n\
             \x20   match w { E2.T(a, _) => { return a.id + z.id; } E2.N => { return 0; } }\n\
             }\n\
             fn consuming() -> i64 {\n\
             \x20   let w: E2 = E2.T(mk(9), mk(10));\n\
             \x20   let z: R = mk(11);\n\
             \x20   match w { E2.T(a, b) => { return take(a) + b.id + z.id; } E2.N => { return 0; } }\n\
             }\n\
             fn fresh_temp() -> i64 {\n\
             \x20   let z: R = mk(12);\n\
             \x20   match E2.T(mk(13), mk(14)) { E2.T(a, b) => { return a.id + z.id; } E2.N => { return 0; } }\n\
             }\n\
             fn main() {\n\
             \x20   structs(); enums();\n\
             \x20   println(f\"a={two_locals()}\");\n\
             \x20   println(f\"b={single()}\");\n\
             \x20   println(f\"c={wildcard()}\");\n\
             \x20   println(f\"d={consuming()}\");\n\
             \x20   println(f\"e={fresh_temp()}\");\n\
             }\n\
             ",
        ) else {
            return;
        };
    assert_eq!(
        out,
        "dR2\ndR1\n-structs\n\
             dR2\ndR1\n-enums\n\
             dR3\ndR2\ndR1\na=4\n\
             dR5\ndR4\nb=9\n\
             dR8\ndR7\ndR6\nc=14\n\
             dR10\ndR9\ndR11\nd=30\n\
             dR14\ndR13\ndR12\ne=25\n"
    );
}

/// B-2026-09-16-31 — A GENERIC ENUM WITH AN `impl[T] Drop` NO LONGER
/// SEGFAULTS WHEN AN OWNED-`self` METHOD IS CALLED ON IT, and its own `Drop`
/// body runs on the compiled backends at all.
///
/// The row's program — `enum G[T] { X(T), Y }` + `impl[T] Drop for G[T]` +
/// `impl[T] G[T] { fn read(self) { match self { .. } } }` over a
/// `let g: G[R]` — exited 139 under `karac build` and
/// `KARAC_AUTO_PAR=0 karac build` alike with no output, where `--interp` ran
/// it correctly. Four independent defects sat under that one crash, each
/// measured apart before it was fixed:
///
///  * the MONOMORPH's own param prologue registered the owned `self`
///    receiver's box free and payload-`Drop` walk while the caller kept
///    both. That arm is half of a callee-owns pair whose other half is
///    `compile_generic_call`'s ARGUMENT retraction, and a method call's
///    receiver never passes through it (`gnone`, `repro`);
///  * the caller-side temp registrar knew only the NAME-keyed payload
///    walker, which declines a generic-param payload by contract, so a
///    `G[R]` TEMP receiver registered no body and no box memory at all
///    (`htemp`, `ptemp`, `pcall` — the last two are a CONCRETE impl block
///    and were broken before this row too);
///  * the owned-`self` receiver disarm B-2026-08-01-7 wrote sits inside
///    `module.get_function(..).filter(|_| !generic_fns…)`, so a method from
///    an `impl[T]` block never reached it and the arm channel and the named
///    receiver both owned the payload (`hread`);
///  * `struct_drop_mono_suffix` reads the STRUCT-only generic-param table,
///    so a generic enum's own `impl[T] Drop` resolved to the bare `G.drop`
///    symbol a generic impl never emits and the shell body ran on no
///    compiled surface, memory balanced throughout (`glocal`, `gnarrow`).
///
/// Plus two hazards that only a NESTED compile can have, both introduced by
/// instantiating `G.drop$R` from the middle of a `let`: `compile_function`
/// clears sixteen `payload_vars` tables on entry (which deleted the `let`'s
/// own box-tracking entry and turned the next by-value call into a
/// use-after-free), and `compile_mono_function` never set
/// `self_arms_bind_views`, so a mono read its CALLER's answer.
///
/// EVERY CELL IS VALGRIND-CLEAN AT `-O0` (0 errors, 0 bytes lost), measured
/// one cell per binary as well as all fifteen together.
///
/// FOUR RESIDUALS ARE NOT IN THIS FIXTURE, each with a CONCRETE-`Drop` twin
/// that behaves identically before and after, which is what says they are
/// not this row's: a by-value free-fn param and a local-scrutinee `match`
/// over a generic enum both order the payload body against the shell's
/// differently from `--interp`, and a CONCRETE `impl G[R]` method that
/// matches on `self` loses the payload body outright. Filed separately
/// rather than pinned, because pinning a divergence would cost this
/// fixture its byte-identical interpreter twin.
///
/// Twin of `tests/interpreter.rs`'s
/// `test_generic_enum_with_generic_drop_impl_survives_an_owned_self_method`,
/// byte-identical source and expectation — the only fixture shape that can
/// hold an agreed gap closed.
#[test]
fn e2e_generic_enum_with_generic_drop_impl_survives_an_owned_self_method() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
    impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
    fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }

    enum G[T] { X(T), Y }
    impl[T] Drop for G[T] { fn drop(mut ref self) { println("  dG") } }
    impl[T] G[T] {
        fn gread(self) -> i64 { match self { G.X(t) => { return 1; } G.Y => { return 0; } } }
        fn gnone(self) -> i64 { return 5 }
    }

    enum K[T] { X(T), Y }
    impl Drop for K[R] { fn drop(mut ref self) { println("  dK") } }
    impl[T] K[T] { fn kread(self) -> i64 { match self { K.X(t) => { return 1; } K.Y => { return 0; } } } }

    enum H[T] { X(T), Y }
    impl[T] H[T] {
        fn hnone(self) -> i64 { return 5 }
        fn hread(self) -> i64 { match self { H.X(t) => { return 1; } H.Y => { return 0; } } }
    }

    enum P[T] { X(T), Y }
    impl P[R] { fn pnone(self) -> i64 { return 5 } }
    fn mkp(i: i64) -> P[R] { return P.X(mk(i)) }

    enum E { A(R), B }
    impl Drop for E { fn drop(mut ref self) { println("  dE") } }
    impl E {
        fn eread(self) -> i64 { match self { E.A(t) => { return 1; } E.B => { return 0; } } }
        fn enone(self) -> i64 { return 5 }
    }

    struct S[T] { v: T }
    impl[T] Drop for S[T] { fn drop(mut ref self) { println("  dS") } }
    impl[T] S[T] { fn snone(self) -> i64 { return 5 } }

    fn main() {
        println("repro");  { let g: G[R] = G.X(mk(20)); println(f"  x{g.gread()}") }
        println("gnone");  { let g: G[R] = G.X(mk(21)); println(f"  x{g.gnone()}") }
        println("glocal"); { let g: G[i64] = G.X(7); println("  x1") }
        println("gnarrow");{ let g: G[i64] = G.X(7); println(f"  x{g.gnone()}") }
        println("kread");  { let k: K[R] = K.X(mk(22)); println(f"  x{k.kread()}") }
        println("hnone");  { let h: H[R] = H.X(mk(23)); println(f"  x{h.hnone()}") }
        println("hread");  { let h: H[R] = H.X(mk(24)); println(f"  x{h.hread()}") }
        println("htemp");  { println(f"  x{H.X(mk(25)).hnone()}") }
        println("pnone");  { let p: P[R] = P.X(mk(26)); println(f"  x{p.pnone()}") }
        println("ptemp");  { println(f"  x{P.X(mk(27)).pnone()}") }
        println("pcall");  { println(f"  x{mkp(28).pnone()}") }
        println("twomono");{ let a: G[R] = G.X(mk(29)); println(f"  x{a.gnone()}"); let b: G[i64] = G.X(7); println(f"  y{b.gnone()}") }
        println("enone");  { let e: E = E.A(mk(30)); println(f"  x{e.enone()}") }
        println("eread");  { let e: E = E.A(mk(31)); println(f"  x{e.eread()}") }
        println("snone");  { let s: S[R] = S { v: mk(32) }; println(f"  x{s.snone()}") }
        println("end")
    }
"#,
    ) else {
        return;
    };
    assert_eq!(out, "repro\n  x1\n  dG\n  dR20\ngnone\n  x5\n  dG\n  dR21\nglocal\n  dG\n  x1\ngnarrow\n  x5\n  dG\nkread\n  x1\n  dK\n  dR22\nhnone\n  x5\n  dR23\nhread\n  dR24\n  x1\nhtemp\n  dR25\n  x5\npnone\n  x5\n  dR26\nptemp\n  dR27\n  x5\npcall\n  dR28\n  x5\ntwomono\n  x5\n  dG\n  dR29\n  y5\n  dG\nenone\n  x5\n  dE\n  dR30\neread\n  x1\n  dE\n  dR31\nsnone\n  x5\n  dS\n  dR32\nend\n");
}

/// B-2026-09-19-12 — an ELEMENT moved out of a boxed tuple payload must not
/// have its `Drop` body run again by the envelope's walk.
///
/// `Some(t) => { let x = t.0 }` reads THROUGH `t` — a projection is a read of
/// the binding — so `optres_arm_takes_whole_payload` answers false and the
/// scrutinee keeps its element walk. That is right about `t` and wrong about
/// element 0, which `x` now owns: both ran its body, the second time over a
/// husk. The codegen twin masks per element at the move site
/// (B-2026-09-19-9); this is the interpreter's copy of that mask, asked of the
/// SAME shared predicate one hop deeper, so the two cannot drift.
///
/// The cells, and what each one is for:
///
///   one        the row's own spelling, one `Drop` element moved.
///   two        a SIBLING the arm never touched, which must still run — the
///              cell that forbids retracting the walk outright.
///   both       every element moved, where the walk owes nothing.
///   second     only element 1 moved, so the mask must be per element and not
///              a high-water mark. Element 0's body is due AFTER `mid`.
///   readonly   an arm that reads and moves nothing: the walk is the sole
///              owner of both elements.
///   inline     an INLINE scrutinee (`match mko()`), which was already correct
///              and stays so — the row's narrowing said the defect was the
///              LOCAL spelling only.
///   struct     a struct payload, the shape B-2026-09-17-34 already covered.
///   none       the `None` arm, where nothing is bound at all.
///
/// Twin of `tests/interpreter.rs`'s
/// `test_boxed_tuple_payload_elem_move_runs_each_body_once`, byte-identical
/// source and expectation. This side was already correct; it is here to hold
/// the agreement, since a one-sided fixture cannot see an A/B divergence
/// reopen.
#[test]
fn e2e_boxed_tuple_payload_elem_move_runs_each_body_once() {
    let Some(out) = run_program(
        r#"struct R { id: i64 }
    impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
    struct W { r: R, n: i64, p: i64, q: i64 }
    fn mk(i: i64) -> R { return R { id: i } }
    fn mko() -> Option[(R, i64, i64, i64)] { return Some((mk(40), 1, 2, 3)) }

    fn main() {
        println("one");      { let o: Option[(R, i64, i64, i64)] = Some((mk(1), 1, 2, 3)); match o { Some(t) => { let x = t.0; println("  mid") } None => { println("  n") } } println("  end") }
        println("two");      { let o: Option[(R, R, i64, i64)] = Some((mk(2), mk(3), 2, 3)); match o { Some(t) => { let x = t.0; println("  mid") } None => { println("  n") } } println("  end") }
        println("both");     { let o: Option[(R, R, i64, i64)] = Some((mk(4), mk(5), 2, 3)); match o { Some(t) => { let x = t.0; let y = t.1; println("  mid") } None => { println("  n") } } println("  end") }
        println("second");   { let o: Option[(R, R, i64, i64)] = Some((mk(6), mk(7), 2, 3)); match o { Some(t) => { let y = t.1; println("  mid") } None => { println("  n") } } println("  end") }
        println("readonly"); { let o: Option[(R, R, i64, i64)] = Some((mk(9), mk(10), 2, 3)); match o { Some(t) => { println(f"  mid{t.0.id}") } None => { println("  n") } } println("  end") }
        println("inline");   { match mko() { Some(t) => { let x = t.0; println("  mid") } None => { println("  n") } } println("  end") }
        println("struct");   { let o: Option[W] = Some(W { r: mk(12), n: 1, p: 2, q: 3 }); match o { Some(t) => { let x = t.r; println("  mid") } None => { println("  n") } } println("  end") }
        println("none");     { let o: Option[(R, i64, i64, i64)] = None; match o { Some(t) => { let x = t.0; println("  mid") } None => { println("  n") } } println("  end") }
        println("end")
    }
"#,
    ) else {
        return;
    };
    assert_eq!(out, "one\n  dR1\n  mid\n  end\ntwo\n  dR2\n  mid\n  dR3\n  end\nboth\n  dR4\n  dR5\n  mid\n  end\nsecond\n  dR7\n  mid\n  dR6\n  end\nreadonly\n  mid9\n  dR9\n  dR10\n  end\ninline\n  dR40\n  mid\n  end\nstruct\n  dR12\n  mid\n  end\nnone\n  n\n  end\nend\n");
}

/// B-2026-09-10-20 — AN ENUM VARIANT DECLARING A `Vec[E]` PAYLOAD NOW RUNS
/// ITS ELEMENTS' `Drop` BODIES.
///
/// `enum H4 { P(Vec[Mono]), Q }` printed `x` where `d2:9 x` is due, on
/// `--interp`, the JIT, `-O0` and `-O2` auto-par alike — an AGREED gap, so
/// no A/B rule saw it, with memory balanced so no sanitizer leg did either.
///
/// THE CAUSE IS ONE MISSING FIELD ROW. The name-keyed payload-bodies head
/// gained an `Array[E, N]` arm in B-2026-09-12-24; `array_elem_and_len`
/// needs a compile-time length, so a `Vec[E]` payload fell past it to the
/// struct gate, whose `struct_types` lookup answers `None` for the head
/// `Vec`. No field row, no walker, no bodies. The predicate was already
/// written (`payload_vec_bodies_parts`, B-2026-09-13-29) and only the
/// shared seeded-pair core was using it.
///
/// `EnumPayloadBodyField` gains a FIFTH slot rather than reusing the array
/// one, because the two shapes' boxing arithmetic differs: an `Array[E, N]`
/// payload is an `[N x E]` aggregate `N` elements wide, a `Vec[E]` payload
/// is the three-word `{ptr, len, cap}` handle however wide its elements
/// are. `vecmixed` is the cell that proves the field INDEX is right — a
/// two-field variant `P(Vec[S1], i64)` walks the handle at its own word.
///
/// BOTH BACKENDS IN ONE COMMIT, which B-2026-09-12-6 and B-2026-09-12-24
/// both state as a rule: wiring only codegen turns a both-silent bug into a
/// run-vs-build divergence, which is strictly worse under the A/B rule. The
/// interpreter arm is keyed on the same DECLARED head, and has to be — the
/// interpreter represents `Array[T, N]` and `Vec[T]` with one
/// `Value::Array`, so no value-shaped test can tell them apart.
///
/// THE INTERPRETER ARM IS WIDER THAN THE ARRAY ONE AND HAS TO BE: that arm
/// handles a `Value::Struct` element only, and the shape the row reports is
/// `Vec[Mono]` over a user ENUM whose payload owns the body. Codegen's
/// `emit_vec_elem_user_drop_bodies_fn_mono` takes a struct OR a non-shared
/// enum layout, so admitting both element kinds is what keeps the two walks
/// equal rather than widening past them (`vecenum` vs `vecstruct`).
///
/// THREE CELLS WERE PINNED AS-IS, all three silent on every surface when
/// this fixture was written and none of them this half's: `sharedec`
/// (`enum H3 { P(SMono) }`) is a `shared` enum payload, which the `targets`
/// filter excludes for a DIFFERENT reason — it admits "a user ENUM running
/// a user drop" and a shared enum's drop is refcount-driven — so it needs
/// its own answer. `genvec` and `gensh` are the GENERIC spellings, whose
/// declared payload head is the type parameter; telling `G[Vec[R]]` from
/// `G[Array[R, N]]` needs the binding's instantiation, which is
/// B-2026-09-17-15's subject.
///
/// `genvec` HAS SINCE BEEN FLIPPED BY B-2026-09-20-62, which is the row
/// that made the DECLARED head the wrong question on both backends at
/// once. `emit_generic_enum_payload_user_drop_bodies_fn` stopped refusing
/// the `Vec` arm by the spelling of the declaration, and the interpreter
/// resolves a bare-parameter payload against the binding's recorded
/// instantiation for `Vec` as well as `Array`, so `G[Vec[Mono]]` now
/// prints `d2:9` — the element's own `R2` payload body, one level in — on
/// `--interp`, the JIT and AOT at both opt levels (measured, four
/// surfaces, 2026-09-21). `gensh` and `sharedec` are UNCHANGED and still
/// pinned: `gensh` staying silent on all four is what says that row
/// widened the container arm and not B-2026-09-10-2's own-generic-param
/// exception.
///
/// Twin of `tests/interpreter.rs`'s
/// `test_declared_vec_enum_payload_runs_element_drop_bodies`, byte-identical
/// source and expectation — the only fixture shape that can hold an agreed
/// gap closed.
/// B-2026-09-17-25 — a NAMED local moved into a `shared enum` tuple-variant
/// constructor no longer runs its `Drop` body over the moved-from husk.
///
/// `suppress_source_vec_cleanup_for_arg` disarms the MEMORY half of that
/// move by zeroing the source struct's String/Vec `cap` and `len`, so its
/// buffer frees (gated on `cap > 0`) no-op and the payload box is sole
/// owner. It left the source's user `Drop` BODY registered, so the body ran
/// anyway — over the husk the zeroing had just made. `self.s.len()` is 9 for
/// a live `R2` and 0 for a moved-from one, which is what makes the defect
/// legible in the output rather than merely suspected:
///
///     let r = mkr(1);
///     { let s = SMono.P(r); println("B") }   --interp   d2:9 B C
///     println("C")                           compiled   B d2:0 C
///
/// Wrong CONTENT, not a missing line — and invisible to every sanitizer
/// leg, because the zeroing is exactly what keeps the memory balanced
/// (measured: 0 definitely-lost, 0 invalid accesses, before and after).
///
/// THE ASSERTION IS THE ABSENCE OF `d2:0`, and the fresh-temp cell beside
/// it is what keeps that honest: it was silent before this fix and is
/// silent after, so a regression that re-armed the body everywhere would
/// still redden the first cell. The `--interp` side prints `d2:9` and is
/// NOT asserted here: it runs the moved-from source's body too, which is
/// the same defect on the other backend and is its own row — this fixture
/// pins the compiled surfaces only.
///
/// SCOPE IS THE TUPLE VARIANT. The struct-variant spelling
/// (`Sv.P { f: r }`) reaches a different path that is silent on the
/// compiled backends before and after, and whose interpreter side runs the
/// body TWICE; it is recorded on the row as unexplained rather than
/// changed here.
#[test]
fn e2e_shared_enum_ctor_named_source_runs_no_husk_drop_body() {
    let Some(out) = run_program(
        r#"struct R2 { s: String, t: String, u: String }
    impl Drop for R2 { fn drop(mut ref self) { println(f"d2:{self.s.len()}") } }
    fn mkr(i: i64) -> R2 { return R2 { s: f"ssssssss{i}", t: f"tttttttt{i}", u: f"uuuuuuuu{i}" } }
    shared enum SMono { P(R2), Q }

    fn main() {
        println("named");
        let r = mkr(1);
        { let s = SMono.P(r); println("  B") }
        println("  C");
        println("temp");
        { let s2 = SMono.P(mkr(2)); println("  B2") }
        println("  C2");
        println("end")
    }
"#,
    ) else {
        return;
    };
    // B-2026-09-17-19 — BOTH BLOCKS NOW PRINT `d2:9`, and this fixture's
    // warning about that reading needs the correction rather than the
    // assertion being weakened. It said `d2:9` "would mean it ran on live
    // memory the payload box also owns", which was the right inference when
    // the ONLY thing that could produce a body here was the husk action
    // firing off the moved-from staging slot. There is now a second, correct
    // producer: `__karac_rc_drop_SMono` runs the payload's body when the
    // refcount hits zero, before the memory walk, so it reads its own live
    // buffers and the box is being torn down around it. `len 9` rather than
    // `len 0` is exactly what distinguishes the two — a husk reads the
    // zeroed slot. The ASAN twin
    // (`asan_shared_enum_payload_body_runs_once_before_the_memory_walk`,
    // which carries this same named-source cell) is what proves there is no
    // use-after-free or double free behind it, and `--interp` prints the
    // identical line, which it did NOT before: this expectation was
    // codegen-only and encoded the missing body.
    assert_eq!(
        out, "named\nd2:9\n  B\n  C\ntemp\nd2:9\n  B2\n  C2\nend\n",
        "a moved-from source must run no HUSK body: `d2:0` is the husk body \
             B-2026-09-17-25 removed and must never come back, while `d2:9` is \
             the payload's own body at the refcount's 0-transition"
    );
}

/// B-2026-09-19-17, the COMPILED half — this side was already right, and
/// the fixture exists to keep it that way while the interpreter caught up.
///
/// B-2026-09-17-19 made a `shared enum`'s payload body run here and filed
/// the interpreter half as its remainder, on the reading that that backend
/// needed a refcounted representation it does not have. It did not: the
/// interpreter's enum-payload walk simply stopped at an enum payload,
/// where its struct / tuple / `Vec` / `Option` siblings already ran the
/// body. So every cell below agrees with the twin ON THE COUNT.
///
/// WHAT THIS PINS THAT THE TWIN CANNOT is the placement it still
/// disagrees about, written out rather than left implicit. The four holder
/// cells (`nested`, `par`, `owndrop`, `noheap`) print their body AFTER
/// `mid` here and before it there — lexical scope exit against the
/// binding's live-range end. That is B-2026-09-19-18, open, and design.md
/// `:866` ("Destructor calls ... fire at each binding's LIVE-RANGE END")
/// says the twin's placement is the correct one, so it is THIS
/// expectation that should move when that row lands. `reclist`, `control`,
/// `qvar` and `unitvar` are byte-identical to the twin and are not part of
/// that trade.
///
/// `par` is not decoration: `E_ENUM_NESTED_ENUM_PAYLOAD` names `shared`
/// and `par` as the only two ways to write an enum inside an enum
/// variant's payload at all, so the pair is the whole population of the
/// shape and both halves have to be measured.
///
/// The INTERPRETER twin is `tests/interpreter.rs`'s
/// `test_shared_enum_in_plain_enum_payload_runs_its_drop_body`,
/// byte-identical SOURCE and a deliberately different expectation.
#[test]
fn e2e_shared_enum_in_plain_enum_payload_runs_its_drop_body() {
    let Some(out) = run_program(
        r#"struct R2 { s: String, t: String, u: String }
impl Drop for R2 { fn drop(mut ref self) { println(f"  d2:{self.s.len()}") } }
fn mkr(i: i64) -> R2 { return R2 { s: f"aaaaaaaaa", t: f"b", u: f"c" } }
struct Z { n: i64 }
impl Drop for Z { fn drop(mut ref self) { println(f"  dZ{self.n}") } }
shared enum SMono { P(R2), Q }
shared enum Sdd { P(R2), Q }
impl Drop for Sdd { fn drop(mut ref self) { println("  dSdd") } }
shared enum Sz { P(Z), Q }
par enum PMono { P(R2), Q }
enum H3 { P(SMono), Q }
enum Hd { P(Sdd), Q }
enum Hz { P(Sz), Q }
enum Hp { P(PMono), Q }
enum Hn { P(SMono), Q }
shared enum Lst { Cons(R2, Lst), Nil }
enum Plain { P(R2), Q }

fn main() {
    println("nested");   { let h: H3 = H3.P(SMono.P(mkr(1))); println("  mid") } println("  out")
    println("par");      { let h: Hp = Hp.P(PMono.P(mkr(2))); println("  mid") } println("  out")
    println("owndrop");  { let h: Hd = Hd.P(Sdd.P(mkr(3))); println("  mid") } println("  out")
    println("noheap");   { let h: Hz = Hz.P(Sz.P(Z { n: 4 })); println("  mid") } println("  out")
    println("qvar");     { let h: Hn = Hn.P(SMono.Q); println("  mid") } println("  out")
    println("unitvar");  { let h: Hn = Hn.Q; println("  mid") } println("  out")
    println("reclist");  { let a: Lst = Lst.Cons(mkr(5), Lst.Nil); let b: Lst = Lst.Cons(mkr(6), a); println("  mid") } println("  out")
    println("control");  { let p: Plain = Plain.P(mkr(7)); println("  mid") } println("  out")
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "nested\n  mid\n  d2:9\n  out\npar\n  mid\n  d2:9\n  out\nowndrop\n  mid\n  dSdd\n  d2:9\n  out\nnoheap\n  mid\n  dZ4\n  out\nqvar\n  mid\n  out\nunitvar\n  mid\n  out\nreclist\n  d2:9\n  d2:9\n  mid\n  out\ncontrol\n  d2:9\n  mid\n  out\nend\n");
}

/// B-2026-09-17-19 — A `shared enum`'s VARIANT PAYLOAD RUNS ITS `Drop` BODY,
/// AND THE RELEASE LANDS AT THE BINDING'S LIVE-RANGE END.
///
/// `let s: SMono = SMono.P(mkr(1));` over `shared enum SMono {{ P(R2), Q }}` with
/// `impl Drop for R2` printed `d2:9` under `--interp` and NOTHING on jit /
/// `karac build` / `KARAC_AUTO_PAR=0 build`. `emit_enum_payload_user_drop_bodies_fn_skipping`
/// defers a shared enum to "the RC machinery", and the RC machinery ran the
/// enum's own body and the memory walk but never the payload's — so no pass
/// owned it.
///
/// `live` and `uselater` are the second half and do not follow from the first:
/// once the bodies ran, they ran at the CLOSING BRACE while `--interp` ran them
/// at `s`'s last use. `uselater` is the cell that PINS that — with a use after
/// the `let`, the body lands after the use, which separates live-range-end
/// firing from firing at the construction statement (the two coincide in every
/// other cell here, and the plain-enum convention this tree already had is the
/// latter). B-2026-09-04-13 made the same admission for a shared
/// STRUCT's plain fields and wrote down this exact consequence — a holder
/// "invisible while its field bodies never ran" that "became a run/build
/// divergence the moment they did". `noheap` is what forces the whole-fn gate
/// to widen rather than just the per-variant one: a payload that owns a body
/// but no heap is not walkable, so the drop fn used to decline outright.
///
/// `psh` is a separate defect the same work uncovered, on the INTERPRETER
/// side: the shared-release walk covered struct, tuple and array holders and
/// stopped at an enum, so a `shared struct` in a plain enum's payload released
/// nothing here while every compiled surface ran its body. `control`, `qvar`
/// and `unitvar` are the negative cells — a plain enum, a payload-free variant
/// of a payload-carrying enum, and an enum with no payload anywhere.
///
/// The INTERPRETER twin is `tests/interpreter.rs`'s
/// `test_shared_enum_payload_runs_its_drop_body`, byte-identical source and
/// expectation.
#[test]
fn e2e_shared_enum_payload_runs_its_drop_body() {
    let Some(out) = run_program(
        r#"struct R2 { s: String, t: String, u: String }
impl Drop for R2 { fn drop(mut ref self) { println(f"  d2:{self.s.len()}") } }
fn mkr(i: i64) -> R2 { return R2 { s: f"ssssssss{i}", t: f"tttttttt{i}", u: f"uuuuuuuu{i}" } }
struct Z { n: i64 }
impl Drop for Z { fn drop(mut ref self) { println(f"  dZ{self.n}") } }
shared struct Sr { s: String }
impl Drop for Sr { fn drop(mut ref self) { println(f"  dS{self.s.len()}") } }
enum Mono { P(R2), Q }
shared enum SMono { P(R2), Q }
shared enum Sz { P(Z), Q }
shared enum Unit { P, Q }
enum HoldSr { P(Sr), Q }
fn tag(e: ref SMono) -> i64 { match e { SMono.P(_) => { return 1 } SMono.Q => { return 0 } } }

fn main() {
    println("temp");    { let s: SMono = SMono.P(mkr(1)); } println("  out")
    println("named");   { let r = mkr(2); let s: SMono = SMono.P(r); } println("  out")
    println("two");     { let a: SMono = SMono.P(mkr(3)); let b = a; } println("  out")
    println("live");    { let s: SMono = SMono.P(mkr(4)); println("  mid") } println("  out")
    println("noheap");  { let s: Sz = Sz.P(Z { n: 5 }); println("  mid") } println("  out")
    println("unitvar"); { let s: Unit = Unit.Q; println("  mid") } println("  out")
    println("qvar");    { let s: SMono = SMono.Q; println("  mid") } println("  out")
    println("uselater"); { let s: SMono = SMono.P(mkr(9)); println("  mid"); println(f"  t{tag(s)}") } println("  out")
    println("psh");     { let h: HoldSr = HoldSr.P(Sr { s: "sssss" }); println("  mid") } println("  out")
    println("control"); { let m: Mono = Mono.P(mkr(8)); println("  mid") } println("  out")
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "temp\n  d2:9\n  out\nnamed\n  d2:9\n  out\ntwo\n  d2:9\n  out\nlive\n  d2:9\n  mid\n  out\nnoheap\n  dZ5\n  mid\n  out\nunitvar\n  mid\n  out\nqvar\n  mid\n  out\nuselater\n  mid\n  t1\n  d2:9\n  out\npsh\n  mid\n  dS5\n  out\ncontrol\n  d2:9\n  mid\n  out\nend\n");
}

/// B-2026-09-19-21 — a generic callee that hands its boxed payload back
/// INSIDE AN AGGREGATE freed the box twice.
///
/// `fn wrap[T](g: G1[T], c: bool) -> H[T] { if c { return H { g: g } } return
/// H { g: G1.N } }` over `struct H[T] { g: G1[T] }` is the mixed-path callee
/// B-2026-09-17-7 fixed, one wrapping out: the same box comes back, but inside
/// a struct rather than as the return value. That row's runtime compare looks
/// at word 1 of the RETURN, so with a return type of `H[T]` the two shapes did
/// not even agree in type, the compare declined by construction, and the
/// argument kept a box drop the wrapper's field now also owned. Measured
/// `free(): double free detected in tcache 2` against a correct `--interp`, and
/// under valgrind `Invalid read of size 8` then `Invalid free()`.
///
/// `tuple` and `nested` are the same defect through the other two aggregate
/// shapes — a `(G1[T], i64)` return and a struct inside a struct — and both
/// died the same way. They were this row's own NOT MEASURED list and are cells
/// rather than a follow-up because one scan covers all three.
///
/// THE SCAN IS TYPE-IDENTITY-BOUNDED, and `allpaths` is what says the bound is
/// not merely cautious. The disarm now compares against every position inside
/// the returned aggregate whose LLVM type is the argument's own enum type,
/// which is a strictly wider `%same` — and a wider disarm is the direction that
/// STRANDS boxes. `allpaths` (the static all-paths spelling), `structF`,
/// `tupleF` (the dies-inside legs, which return a payload-free variant whose
/// box word is zero), `bare` / `bareF` (the sibling row's own cells, which must
/// not move) and `discard` (the result consumed by nobody, where a disarm would
/// strand the box outright) are all here for that direction. A cell that starts
/// leaking fails the memory twin rather than this one.
///
/// Valgrind on this program, `-O0` / `KARAC_AUTO_PAR=0`: 12 errors from 12
/// contexts before, `no leaks are possible` after.
///
/// The INTERPRETER twin is `tests/interpreter.rs`'s
/// `test_generic_callee_hands_its_boxed_payload_back_inside_an_aggregate`,
/// byte-identical source and expectation, and the MEMORY twin is
/// `tests/memory_sanitizer.rs`'s
/// `asan_generic_aggregate_handback_leaves_exactly_one_owner_on_the_payload_box`.
#[test]
fn e2e_generic_callee_hands_its_boxed_payload_back_inside_an_aggregate() {
    let Some(out) = run_program(
        r#"enum G1[T] { Y(T), N }
struct H[T] { g: G1[T] }
struct H2[T] { h: H[T] }
fn wrap[T](g: G1[T], c: bool) -> H[T] { if c { return H { g: g } } return H { g: G1.N } }
fn wrapAll[T](g: G1[T]) -> H[T] { return H { g: g } }
fn wrapTup[T](g: G1[T], c: bool) -> (G1[T], i64) { if c { return (g, 7) } return (G1.N, 7) }
fn wrapNest[T](g: G1[T], c: bool) -> H2[T] { if c { return H2 { h: H { g: g } } } return H2 { h: H { g: G1.N } } }
fn bare[T](g: G1[T], c: bool) -> G1[T] { if c { return g } return G1.N }
fn shw(g: G1[String]) { match g { G1.Y(v) => { println(f"  mx {v.len()}") } G1.N => { println("  mx 0") } } }

fn main() {
    println("struct");  { let g: G1[String] = G1.Y(f"aaaaaaaa-1"); let h = wrap(g, true); shw(h.g) }
    println("structF"); { let g: G1[String] = G1.Y(f"aaaaaaaa-2"); let h = wrap(g, false); shw(h.g) }
    println("allpaths");{ let g: G1[String] = G1.Y(f"aaaaaaaa-3"); let h = wrapAll(g); shw(h.g) }
    println("tuple");   { let g: G1[String] = G1.Y(f"aaaaaaaa-4"); let t = wrapTup(g, true); shw(t.0) }
    println("tupleF");  { let g: G1[String] = G1.Y(f"aaaaaaaa-5"); let t = wrapTup(g, false); shw(t.0) }
    println("nested");  { let g: G1[String] = G1.Y(f"aaaaaaaa-6"); let h = wrapNest(g, true); shw(h.h.g) }
    println("bare");    { let g: G1[String] = G1.Y(f"aaaaaaaa-7"); let b = bare(g, true); shw(b) }
    println("bareF");   { let g: G1[String] = G1.Y(f"aaaaaaaa-8"); let b = bare(g, false); shw(b) }
    println("discard"); { let g: G1[String] = G1.Y(f"aaaaaaaa-9"); wrap(g, true); println("  x") }
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "struct\n  mx 10\nstructF\n  mx 0\nallpaths\n  mx 10\ntuple\n  mx 10\ntupleF\n  mx 0\nnested\n  mx 10\nbare\n  mx 10\nbareF\n  mx 0\ndiscard\n  x\nend\n");
}

/// B-2026-09-19-35 — A BLOCK'S TAIL EXPRESSION IS NOT A STATEMENT, so the
/// move-out neutralizer for a boxed erased enum payload was queued and
/// never drained, and a CHAINED place could not be queued at all.
///
/// Three spellings of one handoff, and each covers a different emission
/// site. `stmt` is `shw(h.g);`, which `compile_stmt` drains. `tail` is the
/// same call written as a braced block's final expression, which
/// `compile_block` emits and the statement drain structurally cannot
/// reach. `fnTail` is the same call as a FUNCTION BODY's final expression,
/// `compile_function_body`'s own site — a third place, not a restatement
/// of the second.
///
/// THE DISCRIMINATOR IS ONE CHARACTER, which is what makes the pair worth
/// keeping rather than collapsing. Measured on the minimal cell before the
/// fix: `shw(h.g)` gives zero `b35.eboxzero` stores and
/// `free(): double free detected in tcache 2`, `shw(h.g);` gives two
/// stores and `ERROR SUMMARY: 0 errors`. Nothing else about the two
/// programs differs. A family whose cells are all spelled one way cannot
/// see the other one at all, and every cell of
/// `e2e_generic_callee_hands_its_boxed_payload_back_inside_an_aggregate`
/// is spelled as a tail, deliberately, for its own row's reason.
///
/// `chain` / `chainF` are `shw(h.h.g)` — a place one hop deeper than the
/// neutralizer used to resolve. That exclusion was harmless while nothing
/// freed the box; once the holder's drop learned to, the chain became the
/// one path with two owners and no neutralizer, and the nine-cell fixture
/// above aborted at its `nested` cell having printed the six before it.
///
/// The `F` cells take the variant with no payload on the same call, so a
/// neutralizer that fired unconditionally on a non-boxing variant would
/// show here rather than in a shape nobody wrote.
///
/// The MEMORY twin is `tests/memory_sanitizer.rs`'s
/// `asan_boxed_erased_payload_survives_every_handoff_spelling`.
#[test]
fn e2e_boxed_erased_payload_survives_every_handoff_spelling() {
    let Some(out) = run_program(
        r#"enum G1[T] { Y(T), N }
struct H[T] { g: G1[T] }
struct H2[T] { h: H[T] }
fn wrap[T](g: G1[T], c: bool) -> H[T] { if c { return H { g: g } } return H { g: G1.N } }
fn wrapNest[T](g: G1[T], c: bool) -> H2[T] { if c { return H2 { h: H { g: g } } } return H2 { h: H { g: G1.N } } }
fn shw(g: G1[String]) { match g { G1.Y(v) => { println(f"  mx {v.len()}") } G1.N => { println("  mx 0") } } }
fn fnTail() { let g: G1[String] = G1.Y(f"aaaaaaaa-6"); let h = wrap(g, true); shw(h.g) }

fn main() {
    println("tail");   { let g: G1[String] = G1.Y(f"aaaaaaaa-1"); let h = wrap(g, true); shw(h.g) }
    println("stmt");   { let g: G1[String] = G1.Y(f"aaaaaaaa-2"); let h = wrap(g, true); shw(h.g); }
    println("tailF");  { let g: G1[String] = G1.Y(f"aaaaaaaa-3"); let h = wrap(g, false); shw(h.g) }
    println("chain");  { let g: G1[String] = G1.Y(f"aaaaaaaa-4"); let h = wrapNest(g, true); shw(h.h.g) }
    println("chainF"); { let g: G1[String] = G1.Y(f"aaaaaaaa-5"); let h = wrapNest(g, false); shw(h.h.g) }
    println("fnTail"); fnTail()
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "tail\n  mx 10\nstmt\n  mx 10\ntailF\n  mx 0\nchain\n  mx 10\nchainF\n  mx 0\nfnTail\n  mx 10\nend\n");
}

/// B-2026-09-19-35 — the chain walk steps its HOPS and its FIELD
/// separately, and this is the cell that says so.
///
/// `struct Out1[T] { g: In1[T] }` over `struct In1[T] { g: G1[T] }` spells
/// the hop and the field with the SAME NAME, so `o.g.g` breaks any walk
/// that folds the field onto the end of the hop list and stops at the
/// first name that matches: it would answer with `In1`, a struct, where
/// the field's own type `G1[String]` was wanted, and the neutralizer would
/// then GEP one level short. Caught by reading the walker rather than by a
/// failing cell, which is why the cell exists — a bug found by reading is
/// one nothing re-checks.
#[test]
fn e2e_boxed_erased_payload_chain_hop_and_field_may_share_a_name() {
    let Some(out) = run_program(
        r#"enum G1[T] { Y(T), N }
struct In1[T] { g: G1[T] }
struct Out1[T] { g: In1[T] }
fn wrapSame[T](g: G1[T], c: bool) -> Out1[T] { if c { return Out1 { g: In1 { g: g } } } return Out1 { g: In1 { g: G1.N } } }
fn shw(g: G1[String]) { match g { G1.Y(v) => { println(f"  mx {v.len()}") } G1.N => { println("  mx 0") } } }
fn main() {
    println("same");  { let g: G1[String] = G1.Y(f"aaaaaaaa-1"); let o = wrapSame(g, true); shw(o.g.g) }
    println("sameF"); { let g: G1[String] = G1.Y(f"aaaaaaaa-2"); let o = wrapSame(g, false); shw(o.g.g) }
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "same\n  mx 10\nsameF\n  mx 0\nend\n");
}

/// B-2026-09-17-7 — a generic callee that MAY hand its boxed payload back
/// freed the box twice, and the check that stops it was blind inside braces.
///
/// `fn mid[T](g: G[T], c: bool) -> G[T] { if c { return g } return G.N }`
/// returns its own parameter on one leg and lets it die inside on the other.
/// Both legs belong to ONE call site, so no static answer there is right for
/// both: disarming the argument strands the box when the callee kept it, and
/// leaving it armed frees the box twice when the callee handed it back. The
/// `handback` cell measured the second — no stdout at all, `Invalid read of
/// size 8`, the caller freeing `a`'s box after `b`'s box, one pointer.
///
/// The answer is a runtime compare, the cheapest dynamic form: after the call
/// the caller zeroes the argument's slot only when the returned box word IS
/// the word that went in. It cannot be wrong in the suppressing direction,
/// which is what makes the UNION predicate `fn_returns_param` usable here
/// where the all-paths one was needed before — a return that merely moves the
/// parameter into a new value carries a different word, the compare fails, and
/// the argument keeps the drop it has today. `keptinside`, `diesinside` and
/// `mono` are the negative cells.
///
/// The second half is why every cell here is BRACED. The window that tells
/// that disarm "nothing consumes this result" was armed over a discarded
/// statement's whole expression, so a braced block written as a statement
/// covered every call inside it — including calls whose result a `let` binding
/// owns outright. The flat twin of `handback` was clean while the braced one
/// died at the block's exit. The window now follows a block to its tail, the
/// only part of it whose value the statement throws away; `discarded` and
/// `blocktail` are the cells that keep it armed where it does belong, and each
/// leaks one box if it stops firing.
///
/// B-2026-09-17-22 — A `shared enum`'s UNIT VARIANT STRANDS ITS RC SHELL,
/// AND WITH IT THE `Drop` BODY THAT SHELL WAS SUPPOSED TO RUN.
///
/// `{ let s = U.Ua; }` over `shared enum U { Ua, Ub }` lost the whole
/// `{ i64 rc, i64 tag, .. }` allocation on every compiled surface — 16 B
/// here, 24 B and 64 B in the two shapes the row was filed on, because the
/// leaked block is the shell and the shell is sized to the enum's widest
/// variant. The payload-carrying variant of the same enum was always clean,
/// which is what localizes this to the UNIT spelling rather than to `shared
/// enum` as such.
///
/// The leak is the invisible half. The visible half is this fixture: the
/// shell's free is what runs the enum's own `Drop` body, so a body that
/// never reached refcount 0 never ran at all. Every `dU` / `dW` line below
/// is a line `--interp` printed and no compiled surface did — six cells of
/// run/build divergence hiding behind a leak nobody had to look at.
///
/// The cause is a receive-inc the constructor had already paid for.
/// `emit_rc_alloc` stores `rc = 1`, and `rhs_yields_fresh_ref` — the
/// predicate that tells the `let` / assign site not to retain a value that
/// arrives fresh — matched `Call` / `MethodCall` / `StructLiteral` and
/// nothing else. A unit variant is the one fresh-ref source spelled as
/// something else: `U.Ua` parses as a two-segment `Path` and a bare `Ub` as
/// an `Identifier`, so both fell to that predicate's `_ => false` arm. The
/// count went to 2, the single scope-exit dec left it at 1, and the
/// `rc_free` block LLVM had faithfully emitted was unreachable at runtime.
///
/// `bare` is the unqualified spelling and `reassign` the assign site, since
/// both read the same predicate. `uselater` pins that the body lands at the
/// binding's live-range end rather than at the construction statement — the
/// distinction B-2026-09-17-19's fixture of the same name exists for, and
/// the two coincide in every other cell. `par` is the Arc path, which
/// leaked identically. `nodrop` and `plain` are the negative cells: a
/// shared enum with no body prints nothing either way, and a plain enum has
/// no shell to strand, so its body always ran.
///
/// The INTERPRETER twin is `tests/interpreter.rs`'s
/// `test_shared_enum_unit_variant_runs_its_drop_body`, byte-identical
/// source and expectation.
#[test]
fn e2e_shared_enum_unit_variant_runs_its_drop_body() {
    let Some(out) = run_program(
        r#"shared enum U { Ua, Ub }
impl Drop for U { fn drop(mut ref self) { println(f"  dU") } }
shared enum V { Va, Vb }
par enum W { Wa, Wb }
impl Drop for W { fn drop(mut ref self) { println(f"  dW") } }
enum Plain { Pa, Pb }
impl Drop for Plain { fn drop(mut ref self) { println(f"  dP") } }
fn tag(u: ref U) -> i64 { match u { U.Ua => { return 1 } U.Ub => { return 0 } } }
fn main() {
    println("qual");     { let s = U.Ua; println("  mid") } println("  out")
    println("bare");     { let s = Ub; println("  mid") } println("  out")
    println("two");      { let s = U.Ua; let t = U.Ub; println("  mid") } println("  out")
    println("alias");    { let s = U.Ua; let t = s; println("  mid") } println("  out")
    println("uselater"); { let s = U.Ua; println("  mid"); println(f"  t{tag(s)}") } println("  out")
    println("reassign"); { let mut s = U.Ua; s = U.Ub; println("  mid") } println("  out")
    println("nodrop");   { let s = V.Va; println("  mid") } println("  out")
    println("par");      { let s = W.Wa; println("  mid") } println("  out")
    println("plain");    { let s = Plain.Pa; println("  mid") } println("  out")
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "qual\n  dU\n  mid\n  out\nbare\n  dU\n  mid\n  out\ntwo\n  dU\n  dU\n  mid\n  out\nalias\n  dU\n  mid\n  out\nuselater\n  mid\n  t1\n  dU\n  out\nreassign\n  dU\n  dU\n  mid\n  out\nnodrop\n  mid\n  out\npar\n  dW\n  mid\n  out\nplain\n  dP\n  mid\n  out\nend\n");
}

/// The INTERPRETER twin is `tests/interpreter.rs`'s
/// `test_generic_callee_may_hand_its_boxed_payload_back`, byte-identical source and expectation.
#[test]
fn e2e_generic_callee_may_hand_its_boxed_payload_back() {
    let Some(out) = run_program(
        r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
enum G[T] { Y(T), N }
enum M { Y(String), N }
fn mid[T](g: G[T], c: bool) -> G[T] { if c { return g } return G.N }
fn allpaths[T](g: G[T]) -> G[T] { return g }
fn diesinside[T](g: G[T]) -> G[T] { return G.N }
fn tailmatch[T](g: G[T], c: i64) -> G[T] { match c { 1 => { return g } _ => { return G.N } } }
fn pick[T](a: G[T], b: G[T], c: bool) -> G[T] { if c { return a } return G.N }
fn midmono(g: M, c: bool) -> M { if c { return g } return M.N }
fn show[T](g: G[String]) { match g { G.Y(v) => { println(f"  {v.len()}") } G.N => { println("  none") } } }

fn main() {
    println("handback");  { let a: G[String] = G.Y(f"aaaaaaaa-1"); let b = mid(a, true); show(b) }
    println("keptinside");{ let a: G[String] = G.Y(f"bbbbbbbb-2"); let b = mid(a, false); show(b) }
    println("allpaths");  { let a: G[String] = G.Y(f"cccccccc-3"); let b = allpaths(a); show(b) }
    println("diesinside");{ let a: G[String] = G.Y(f"dddddddd-4"); let b = diesinside(a); show(b) }
    println("discarded"); { let a: G[String] = G.Y(f"eeeeeeee-5"); mid(a, true); println("  out") }
    println("unread");    { let a: G[String] = G.Y(f"ffffffff-6"); let b = mid(a, true); println("  out") }
    println("userdrop");  { let a: G[R] = G.Y(R { id: 7, s: f"ssssssss" }); let b = mid(a, true); println("  out") }
    println("i64");       { let a: G[i64] = G.Y(5); let b = mid(a, true); match b { G.Y(v) => { println(f"  {v}") } G.N => { println("  none") } } }
    println("twoargs");   { let x: G[String] = G.Y(f"iiiiiiii-9"); let y: G[String] = G.Y(f"jjjjjjjj-10"); let b = pick(x, y, true); show(b) }
    println("vecpayload");{ let a: G[Vec[String]] = G.Y([f"kkkkkkkk-11", f"llllllll-12"]); let b = mid(a, true); match b { G.Y(v) => { println(f"  {v.len()}") } G.N => { println("  none") } } }
    println("arrpayload");{ let a: G[Array[String, 2]] = G.Y([f"mmmmmmmm-13", f"nnnnnnnn-14"]); let b = mid(a, true); match b { G.Y(v) => { println("  arr") } G.N => { println("  none") } } }
    println("matchtail"); { let a: G[String] = G.Y(f"oooooooo-15"); let b = tailmatch(a, 1); show(b) }
    println("chain");     { let a: G[String] = G.Y(f"pppppppp-16"); let b = mid(mid(a, true), true); show(b) }
    println("blocktail"); { let a: G[String] = G.Y(f"rrrrrrrr-18"); mid(a, true) }; println("  out")
    println("mono");      { let a: M = M.Y(f"qqqqqqqq-17"); let b = midmono(a, true); match b { M.Y(v) => { println(f"  {v.len()}") } M.N => { println("  none") } } }
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "handback\n  10\nkeptinside\n  none\nallpaths\n  10\ndiesinside\n  none\ndiscarded\n  out\nunread\n  out\nuserdrop\n  dR7\n  out\ni64\n  5\ntwoargs\n  10\nvecpayload\n  2\narrpayload\n  arr\nmatchtail\n  11\nchain\n  11\nblocktail\n  out\nmono\n  11\nend\n");
}

/// B-2026-09-17-8 — compiling a monomorph mid-caller wiped the CALLER's
/// payload-ownership records, and the caller then gave the same payload two
/// owners.
///
/// Nine registries are cleared at the mono body entry and none of them was
/// swapped out around the nested compile, so the clear was one-way.
/// `let b = idOpt(a);` over `fn idOpt[T](g: Option[T]) -> Option[T] { return
/// g }` records `passthrough_owner_alias[b] = a` and registers no owner for
/// `b`, on B-2026-08-06-27's rule that the source stays sole owner — but only
/// while `a` is still in `inline_option_payload_vars` when that `let`
/// compiles. The monomorph compiled for that very call had emptied the set, so
/// the alias was never recorded and `b` took a second registration over `a`'s
/// payload: `free(): double free detected in tcache 2`, 11 allocs / 12 frees.
/// `monoinline`, `monobody` and `nocall` are the cells that were always clean,
/// and the first two are clean for one reason — nothing clears the set.
///
/// The second half is the BODIES channel, which the memory fix exposed rather
/// than caused. `compile_call` has retracted a handed-back argument's payload
/// walk since B-2026-08-09-15; `compile_generic_call` never did, so with the
/// source correctly registered again both `a` and `b` fired it — `dD / got 6 /
/// dD` against the interpreter's `got 6 / dD`, on balanced memory. `body`,
/// `bodyboxed` and `bodynoarm` are the three cells that double without it, and
/// they double across both payload classes: `D` is one word and rides the
/// inline channel, `W` is four and boxes.
///
/// `twocalls` is the cell that refutes the argument-side disarm this row first
/// reached for: standing the argument down fixes the first call to a monomorph
/// and leaks every later one, because the inline channel's contract is the
/// opposite of the boxed one — there the source is the sole owner, not the
/// second.
///
/// The INTERPRETER twin is `tests/interpreter.rs`'s
/// `test_generic_call_keeps_the_callers_payload_ownership`, byte-identical source and expectation.
#[test]
fn e2e_generic_call_keeps_the_callers_payload_ownership() {
    let Some(out) = run_program(
        r#"struct D { id: i64 }
impl Drop for D { fn drop(mut ref self) { println(f"  dD{self.id}") } }
struct W { id: i64, s: String }
impl Drop for W { fn drop(mut ref self) { println(f"  dW{self.id}") } }
fn idOpt[T](g: Option[T]) -> Option[T] { return g }
fn idRes[T](g: Result[T, i64]) -> Result[T, i64] { return g }
fn idOptMono(g: Option[String]) -> Option[String] { return g }
fn idDMono(g: Option[D]) -> Option[D] { return g }

fn main() {
    println("inline");   { let a: Option[String] = Option.Some(f"aaaaaaaa-1"); let b = idOpt(a); match b { Option.Some(v) => { println(f"  {v.len()}") } Option.None => { println("  none") } } }
    println("result");   { let a: Result[String, i64] = Result.Ok(f"bbbbbbbb-2"); let b = idRes(a); match b { Result.Ok(v) => { println(f"  {v.len()}") } Result.Err(e) => { println("  err") } } }
    println("noarm");    { let a: Option[String] = Option.Some(f"cccccccc-3"); let b = idOpt(a); println("  out") }
    println("twocalls"); { let a: Option[String] = Option.Some(f"dddddddd-4"); let x = idOpt(a); let c: Option[String] = Option.Some(f"eeeeeeee-5"); let y = idOpt(c); match x { Option.Some(v) => { println(f"  {v.len()}") } Option.None => { println("  none") } } match y { Option.Some(v) => { println(f"  {v.len()}") } Option.None => { println("  none") } } }
    println("body");     { let a: Option[D] = Option.Some(D { id: 6 }); let b = idOpt(a); match b { Option.Some(v) => { println(f"  got {v.id}") } Option.None => { println("  none") } } }
    println("bodyboxed");{ let a: Option[W] = Option.Some(W { id: 7, s: f"ssssssss" }); let b = idOpt(a); match b { Option.Some(v) => { println(f"  got {v.id}") } Option.None => { println("  none") } } }
    println("bodynoarm");{ let a: Option[W] = Option.Some(W { id: 8, s: f"tttttttt" }); let b = idOpt(a); println("  out") }
    println("monoinline"); { let a: Option[String] = Option.Some(f"ffffffff-9"); let b = idOptMono(a); match b { Option.Some(v) => { println(f"  {v.len()}") } Option.None => { println("  none") } } }
    println("monobody");   { let a: Option[D] = Option.Some(D { id: 10 }); let b = idDMono(a); match b { Option.Some(v) => { println(f"  got {v.id}") } Option.None => { println("  none") } } }
    println("nocall");     { let a: Option[D] = Option.Some(D { id: 11 }); match a { Option.Some(v) => { println(f"  got {v.id}") } Option.None => { println("  none") } } }
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "inline\n  10\nresult\n  10\nnoarm\n  out\ntwocalls\n  10\n  10\nbody\n  got 6\n  dD6\nbodyboxed\n  got 7\n  dW7\nbodynoarm\n  dW8\n  out\nmonoinline\n  10\nmonobody\n  got 10\n  dD10\nnocall\n  got 11\n  dD11\nend\n");
}

/// B-2026-09-10-20 — an enum's CONTAINER payload held in a STRUCT FIELD.
///
/// The sibling of `tests/interpreter.rs`'s
/// `test_enum_container_payload_in_struct_field_runs_element_drop_bodies`, on the
/// axis that row's earlier sessions never varied: POSITION. Both of its previous
/// fixes were measured across payload TYPES — `Vec` vs `Array` vs tuple, declared
/// vs generic — with every cell written at the same `let`-bound position, and both
/// were reverted. A cell matrix that varies only the type cannot see a defect
/// whose whole shape is which registration site fires.
///
/// `f-arr` / `f-vec` / `f-venum` are the fixed cells, and THIS is the side that
/// was wrong: an `Array[R, 2]`, `Vec[R]` or `Vec[Mono]` payload whose enum sits in
/// a struct field ran its element bodies under `--interp` and on no compiled
/// surface — three run-vs-build divergences from ONE gate.
/// `type_runs_user_drop`'s enum leg reads the payload's HEAD NAME, and the head of
/// `Array[R, 2]` is `Array`, so `user_drop_field_indices_mono` never admitted the
/// field, `emit_user_drop_field_bodies_fn_skipping` declined for an empty set, and
/// the field walker's enum arm — which already calls
/// `emit_enum_payload_user_drop_bodies_fn` and walks the container correctly — was
/// never reached. Only the gate was missing, the shape B-2026-09-15-35 records for
/// the bare-param field.
///
/// `f-bind` varies how the field is INITIALIZED (a named local rather than a fresh
/// temp) and `f-second` puts the enum at field index 1 behind a scalar, which pins
/// the GEP rather than the admission.
///
/// `l-arr` / `l-vec` are the non-regression controls: the same payloads at the
/// `let`-bound position, correct since B-2026-09-12-6 / B-2026-09-13-29 and
/// unmoved by this commit.
///
/// THE THREE `b-` CELLS ARE THE DELIBERATE BOUNDARY AND MUST STAY SILENT.
/// `b-arrenum` (`Array[Mono, 1]`, a user ENUM element) and `b-tuple` are AGREED
/// silences today, and the gate is narrowed to mirror
/// `run_enum_payload_user_drops_value`'s element dispatch arm for arm so they stay
/// that way: its declared-`Array` arm takes a `Value::Struct` element only, while
/// its declared-`Vec` arm takes a struct OR a non-shared user enum. Asking the
/// emitter's wider `elem_te_runs_user_drop` here instead was measured to make
/// `b-arrenum` print on all three compiled surfaces and nowhere under `--interp`
/// — one fresh divergence bought for three closed, which is what 0eba4d1's revert
/// was about. `b-arrenum`'s family is the row's remainder; `b-tuple` is
/// B-2026-09-19-46.
#[test]
fn e2e_enum_container_payload_in_struct_field_runs_element_drop_bodies() {
    let Some(out) = run_program(
        r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i, s: f"aaa" } }
enum Mono { P(R), Q }
enum Ea { P(Array[R, 2]), Q }
enum Ev { P(Vec[R]), Q }
enum Em { P(Vec[Mono]), Q }
enum En { P(Array[Mono, 1]), Q }
enum Et { P((R, R)), Q }
struct Ha { h: Ea }
struct Hv { h: Ev }
struct Hm { h: Em }
struct Hn { h: En }
struct Ht { h: Et }
struct Hw { lead: i64, h: Ea }

fn main() {
    println("f-arr");   { let a: Array[R, 2] = [mkr(1), mkr(2)]; let g = Ha { h: Ea.P(a) }; println("m") }
    println("f-vec");   { let mut w: Vec[R] = []; w.push(mkr(3)); let g = Hv { h: Ev.P(w) }; println("m") }
    println("f-venum"); { let mut w: Vec[Mono] = []; w.push(Mono.P(mkr(4))); let g = Hm { h: Em.P(w) }; println("m") }
    println("f-bind");  { let a: Array[R, 2] = [mkr(5), mkr(6)]; let h = Ea.P(a); let g = Ha { h: h }; println("m") }
    println("f-second"); { let a: Array[R, 2] = [mkr(7), mkr(8)]; let g = Hw { lead: 9, h: Ea.P(a) }; println("m") }
    println("l-arr");   { let a: Array[R, 2] = [mkr(10), mkr(11)]; let h = Ea.P(a); println("m") }
    println("l-vec");   { let mut w: Vec[R] = []; w.push(mkr(12)); let h = Ev.P(w); println("m") }
    println("b-arrenum"); { let a: Array[Mono, 1] = [Mono.P(mkr(13))]; let g = Hn { h: En.P(a) }; println("m") }
    println("b-tuple");  { let g = Ht { h: Et.P((mkr(14), mkr(15))) }; println("m") }
    println("b-unit");   { let g = Ha { h: Ea.Q }; println("m") }
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "f-arr\nd1\nd2\nm\nf-vec\nd3\nm\nf-venum\nd4\nm\nf-bind\nd5\nd6\nm\nf-second\nd7\nd8\nm\nl-arr\nd10\nd11\nm\nl-vec\nd12\nm\nb-arrenum\nm\nb-tuple\nm\nb-unit\nm\nend\n", "got:\n{out}");
}

/// B-2026-09-17-15 — A GENERIC ENUM'S `shared` PAYLOAD IS NOW RC-RELEASED,
/// AND EVERY CELL IS PINNED BESIDE ITS CONCRETE TWIN, which is the whole
/// design of this fixture rather than decoration.
///
/// `enum Box2[T] { V(T), N }` at `T = shared struct Sh` stranded one 16 B
/// RC control block per value at `-O0` and LOST the payload's `Drop` body
/// on all three compiled surfaces, where `--interp` ran it — a run-vs-build
/// split, not the leak the row was filed as. The cause: `field_drop_kinds`
/// is written once per enum NAME in `declare_enums`, so the payload
/// classified is the bare `T` and takes `enum_drop_kind_for_type_expr`'s
/// `_ => None` tail, which all four of B-2026-09-10-11's gates then read.
///
/// WHY EVERY `G-` CELL HAS A `C-` TWIN. The row's own control is the
/// CONCRETE enum `Et { A(Sh), B }`, correct since that parent fix. Pinning
/// the two spellings in ONE program makes the test assert the property that
/// actually matters — the generic spelling behaves as the concrete one —
/// rather than a transcript somebody re-measured after the fact. Five of the
/// six pairs are byte-identical here and on `--interp`.
///
/// THE TWO PLACES `--interp` DIFFERS ARE PRE-EXISTING AND SHARED BY BOTH
/// SPELLINGS, which is why they are pinned rather than fixed: `G-temp` /
/// `C-temp` both lose the body under `--interp` (a fresh-temp argument to a
/// by-value param), and `G-named` / `C-named` both place it at the end of
/// `main` there instead of at the callee's exit. Measured on the CONCRETE
/// cells alone before this fix, so neither was opened by it. The twin in
/// `tests/interpreter.rs` carries the other half of that pin.
///
/// `G-alias` IS THE ONE CELL WHERE THE TWO SPELLINGS STILL DISAGREE, and it
/// is the fix's measured remainder. `let gf = ge` registers nothing: the
/// let-site gate wants a FRESH-owned RHS (`rhs_is_fresh_inline_enum`) and a
/// bound identifier is a move, while `ge` itself is excluded for escaping
/// into `gf`. Registering either without defusing the other is a double
/// free, and the suppression channel that would do it
/// (`var_option_shared_heap`-keyed) is `Option`-only — which is why the
/// `Result[shared]` sibling carries the identical residual. `C-alias` prints
/// `dSh6` because the concrete path is a TYPE-keyed drop fn rather than a
/// per-binding cleanup, so it fires wherever the value lives.
///
/// `sharedenum` IS A DELIBERATELY-HELD AGREED GAP, NOT AN OVERSIGHT.
/// B-2026-09-19-17 withheld the interpreter's own-generic-param exception
/// from an ENUM payload precisely so this cell stays silent on both sides;
/// admitting it in codegen would close a 24 B leak by opening a divergence,
/// and the dec cannot be taken without the body, since releasing the last
/// ref is what reaches `emit_shared_enum_rc_drop_fn`. The
/// `generic_enum_shared_payload_arms` query excludes it for that reason and
/// says so at the site.
///
/// The remaining cells are controls that place the fault: `string` (3 words,
/// so the BOXED path already owned it), `scalar` (fits the area, owns
/// nothing), `unit`, `otherarm` (the non-shared arm of the same two-param
/// enum, which the per-arm tag guard must leave untouched), `plain` (a
/// non-shared struct payload, which rides the bodies walker) and `par`
/// (`shared_types` records `is_par` too, and a fix keyed on `shared` alone
/// would leave it leaking).
#[test]
fn e2e_generic_enum_shared_payload_is_rc_released() {
    let Some(out) = run_program(
        r#"shared struct Sh { n: i64 }
impl Drop for Sh { fn drop(mut ref self) { println(f"  dSh{self.n}") } }
par struct Pa { n: i64 }
impl Drop for Pa { fn drop(mut ref self) { println(f"  dPa{self.n}") } }
struct Pl { n: i64 }
impl Drop for Pl { fn drop(mut ref self) { println(f"  dPl{self.n}") } }
shared enum Sen { P(Pl), Q }
enum Box2[T] { V(T), N }
enum Pair[A, B] { L(A), R(B) }
enum Et { A(Sh), B }

fn pg(b: Box2[Sh]) -> Box2[Sh] { return b }
fn pc(b: Et) -> Et { return b }
fn eg(b: Box2[Sh]) { println("  eaten") }
fn ec(b: Et) { println("  eaten") }

fn main() {
    println("G-bare"); { let ga: Box2[Sh] = Box2.V(Sh { n: 1 }); println("  x") }
    println("C-bare"); { let ca: Et = Et.A(Sh { n: 1 }); println("  x") }
    println("G-round"); { let gb = pg(Box2.V(Sh { n: 2 })); println("  x") }
    println("C-round"); { let cb = pc(Et.A(Sh { n: 2 })); println("  x") }
    println("G-arm"); { let gc: Box2[Sh] = Box2.V(Sh { n: 3 }); match gc { Box2.V(s) => { println(f"  got{s.n}") } Box2.N => { println("  no") } } }
    println("C-arm"); { let cc: Et = Et.A(Sh { n: 3 }); match cc { Et.A(s) => { println(f"  got{s.n}") } Et.B => { println("  no") } } }
    println("G-temp"); { eg(Box2.V(Sh { n: 4 })); println("  x") }
    println("C-temp"); { ec(Et.A(Sh { n: 4 })); println("  x") }
    println("G-named"); { let gd: Box2[Sh] = Box2.V(Sh { n: 5 }); eg(gd); println("  x") }
    println("C-named"); { let cd: Et = Et.A(Sh { n: 5 }); ec(cd); println("  x") }
    println("G-alias"); { let ge: Box2[Sh] = Box2.V(Sh { n: 6 }); let gf = ge; println("  x") }
    println("C-alias"); { let ci: Et = Et.A(Sh { n: 6 }); let cj = ci; println("  x") }
    println("par"); { let pz: Box2[Pa] = Box2.V(Pa { n: 7 }); println("  x") }
    println("twoparam"); { let tp: Pair[Sh, i64] = Pair.L(Sh { n: 8 }); println("  x") }
    println("otherarm"); { let oa: Pair[Sh, i64] = Pair.R(9); println("  x") }
    println("unit"); { let uz: Box2[Sh] = Box2.N; println("  x") }
    println("string"); { let sz: Box2[String] = Box2.V("aaaaaaaaa"); println("  x") }
    println("scalar"); { let iz: Box2[i64] = Box2.V(11); println("  x") }
    println("plain"); { let lz: Box2[Pl] = Box2.V(Pl { n: 13 }); println("  x") }
    println("sharedenum"); { let ez: Box2[Sen] = Box2.V(Sen.P(Pl { n: 14 })); println("  x") }
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "G-bare\n  x\n  dSh1\nC-bare\n  x\n  dSh1\nG-round\n  x\n  dSh2\nC-round\n  x\n  dSh2\nG-arm\n  got3\n  dSh3\nC-arm\n  got3\n  dSh3\nG-temp\n  eaten\n  dSh4\n  x\nC-temp\n  eaten\n  dSh4\n  x\nG-named\n  eaten\n  dSh5\n  x\nC-named\n  eaten\n  dSh5\n  x\nG-alias\n  x\nC-alias\n  x\n  dSh6\npar\n  x\n  dPa7\ntwoparam\n  x\n  dSh8\notherarm\n  x\nunit\n  x\nstring\n  x\nscalar\n  x\nplain\n  dPl13\n  x\nsharedenum\n  x\nend\n");
}

/// B-2026-09-19-43 — a match arm's payload bound out of a TRANSFER-OWNED
/// enum param, then assigned to an outer local, still runs its `Drop`
/// body.
///
/// TWO GATES EACH STOOD DOWN BECAUSE IT BELIEVED THE OTHER WOULD FIRE, and
/// neither looks defective read on its own:
///
///  * the CALLER's inline-constructor argument arm returns early on
///    `enum_param_owned_by_transfer` before it registers the
///    `__karac_dropelems_enum_<E>` payload-bodies walker, so a heap-bearing
///    payload gets no caller-side fire at all;
///  * the CALLEE then read the arm binding's membership in
///    `param_view_locals` as a param-view rebind at `o = w;` and stored
///    `false` into the target's `cond_move_drop_flags` bit, on the premise
///    that the caller fires instead. The assignment's own retraction of the
///    SOURCE (B-2026-07-30-11's displaced-value leg) carries the opposite
///    premise — that the LHS fires — so between them the body ran in no
///    frame.
///
/// `one` is the cell: `dR11 one:12` on the JIT, `karac build` -O0 and -O2
/// against `--interp`'s `dR11 dR12 one:12`.
///
/// `two` IS A CONTROL THAT USED TO PASS BY ACCIDENT, which is why the row
/// was filed against the wrong axis twice. An all-scalar payload makes
/// `enum_param_owned_by_transfer` false, so the caller KEEPS its walker and
/// fires the body there — nothing arranges for it, no suppression was
/// needed. Payload WIDTH therefore looked like the discriminator and runs
/// the wrong way: 2w/3w/4w/5w all-scalar carriers are all correct, and a
/// 4w carrier with a `String` loses it.
///
/// `three` and `four` are the other two disarm mechanisms — Vec/String
/// zero a `cap` sentinel, Map/Set edit a cleanup queue — so the cell is not
/// one suppressor's quirk. `five` and `six` are the named-local sources,
/// correct throughout. `seven` (the arm only READS the payload) and
/// `eight` (nothing binds it) are the no-double guards: there the caller
/// stands down and the callee's own `__karac_dropelems_enum_<E>(%b)` fires,
/// and a fix that re-homed the body unconditionally would print it twice.
///
/// `nine` is the ROW'S OWN reduction and varies the PROVENANCE of the heap:
/// `Hold { r: Rh, n: i64 }` puts the `String` INSIDE the `Drop`-bearing
/// field rather than beside it, so the carrier's own field list is
/// `Drop`-bearing + scalar. It printed `dRh91 nine:9` against `--interp`'s
/// `dRh91 dRh92 nine:9`.
///
/// `ten` pins the WIDTH boundary, because the fix sits inside a block
/// guarded by a word-count test against the scrutinee's payload area and a
/// narrower placement would have covered `one` and missed this: `Wide` is
/// six words where `Ch` is four. Measured broken at 4w, 5w and 6w alike
/// before the fix and correct at all three after, which is the second
/// reason width was never the axis.
#[test]
fn e2e_transfer_owned_enum_payload_assigned_out_runs_its_body() {
    let Some(out) = run_program(
            "struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             struct Rh { id: i64, tag: String }\n\
             impl Drop for Rh { fn drop(mut ref self) { println(f\"dRh{self.id}\") } }\n\
             struct Ch { r: R, s: String }\n\
             struct Cv { r: R, v: Vec[i64] }\n\
             struct Cm { r: R, m: Map[String, i64] }\n\
             struct Cn { r: R, n: i64 }\n\
             struct Hold { r: Rh, n: i64 }\n\
             struct Wide { r: R, s: String, k: i64, j: i64 }\n\
             enum Ech { A(Ch), B }\n\
             enum Ecv { A(Cv), B }\n\
             enum Ecm { A(Cm), B }\n\
             enum Ecn { A(Cn), B }\n\
             enum Eh { A(Hold), B }\n\
             enum Ew { A(Wide), B }\n\
             fn one(b: Ech) -> i64 { let mut o: Ch = Ch { r: R { id: 11 }, s: f\"OUT\" }; match b { Ech.A(w) => { o = w; } Ech.B => { } } return o.r.id }\n\
             fn two(b: Ecn) -> i64 { let mut o: Cn = Cn { r: R { id: 21 }, n: 0 }; match b { Ecn.A(w) => { o = w; } Ecn.B => { } } return o.r.id }\n\
             fn three(b: Ecv) -> i64 { let mut o: Cv = Cv { r: R { id: 31 }, v: Vec.new() }; match b { Ecv.A(w) => { o = w; } Ecv.B => { } } return o.r.id }\n\
             fn four(b: Ecm) -> i64 { let mut o: Cm = Cm { r: R { id: 41 }, m: Map.new() }; match b { Ecm.A(w) => { o = w; } Ecm.B => { } } return o.r.id }\n\
             fn five() -> i64 { let mut o: Ch = Ch { r: R { id: 51 }, s: f\"OUT\" }; let w: Ch = Ch { r: R { id: 52 }, s: f\"PAY\" }; o = w; return o.r.id }\n\
             fn six() -> i64 { let mut o: Cn = Cn { r: R { id: 61 }, n: 0 }; let w: Cn = Cn { r: R { id: 62 }, n: 1 }; o = w; return o.r.id }\n\
             fn seven(b: Ech) -> i64 { match b { Ech.A(w) => { return w.r.id } Ech.B => { } } return 0 }\n\
             fn eight(b: Ech) -> i64 { return 7 }\n\
             fn nine(b: Eh) -> i64 { let mut o: Hold = Hold { r: Rh { id: 91, tag: f\"OUTOUT\" }, n: 1 }; match b { Eh.A(w) => { o = w; } Eh.B => { } } return o.n }\n\
             fn ten(b: Ew) -> i64 { let mut o: Wide = Wide { r: R { id: 101 }, s: f\"OUT\", k: 0, j: 0 }; match b { Ew.A(w) => { o = w; } Ew.B => { } } return o.r.id }\n\
             fn main() {\n\
             \x20   println(f\"one:{one(Ech.A(Ch { r: R { id: 12 }, s: f\"PAY\" }))}\");\n\
             \x20   println(f\"two:{two(Ecn.A(Cn { r: R { id: 22 }, n: 1 }))}\");\n\
             \x20   println(f\"three:{three(Ecv.A(Cv { r: R { id: 32 }, v: Vec.new() }))}\");\n\
             \x20   println(f\"four:{four(Ecm.A(Cm { r: R { id: 42 }, m: Map.new() }))}\");\n\
             \x20   println(f\"five:{five()}\");\n\
             \x20   println(f\"six:{six()}\");\n\
             \x20   println(f\"seven:{seven(Ech.A(Ch { r: R { id: 72 }, s: f\"PAY\" }))}\");\n\
             \x20   println(f\"eight:{eight(Ech.A(Ch { r: R { id: 82 }, s: f\"PAY\" }))}\");\n\
             \x20   println(f\"nine:{nine(Eh.A(Hold { r: Rh { id: 92, tag: f\"PAYPAYPAYPAY\" }, n: 9 }))}\");\n\
             \x20   println(f\"ten:{ten(Ew.A(Wide { r: R { id: 102 }, s: f\"PAY\", k: 1, j: 2 }))}\");\n\
             \x20   println(\"end\")\n\
             }\n\
             ",
        ) else {
            return;
        };
    assert_eq!(
        out,
        "dR11\ndR12\none:12\n\
             dR21\ndR22\ntwo:22\n\
             dR31\ndR32\nthree:32\n\
             dR41\ndR42\nfour:42\n\
             dR51\ndR52\nfive:52\n\
             dR61\ndR62\nsix:62\n\
             dR72\nseven:72\n\
             dR82\neight:7\n\
             dRh91\ndRh92\nnine:9\n\
             dR101\ndR102\nten:102\n\
             end\n"
    );
}

/// B-2026-09-05-26 — a user enum's STRUCT payload owns its heap, inline or
/// boxed, on every path: unbound (`one`, `four`), through a `_` arm
/// (`two`, `five`), bound and unread (`three`, `six`, `eight`), and
/// destructured (`seven`).
///
/// `Two { a: R, b: R }` and `Ho2 { a: R, b: Option[R] }` are not
/// copy-supported (a `Drop`-bearing field), so the drop-kind classifier
/// answered `None` and the enum synthesized no memory drop: the bodies
/// walk ran and every `String`/`Vec` inside leaked. The new
/// `NestedOwnedStruct` kind frees them through the struct's own drop
/// synthesis and is never deep-copied. `Ho2` is wider than its allotted
/// words and is heap-BOXED at construction; the bodies walker and the
/// drop switch read the box pointer as the struct, which is why the
/// unbound `Ho2` cells printed a garbage `id` before — both now walk and
/// drop through the box and free the envelope. `seven`'s envelope was
/// then hidden from the enum's drop by the destructure-consume zeroing,
/// which now frees it first. `nine` (a bare `R` payload) and `ten` (no
/// payload) are controls. The ASAN twin runs the same program under LSan.
#[test]
fn e2e_user_enum_struct_payload_owns_its_heap() {
    let Some(out) = run_program(
            "struct R { id: i64, tag: String, xs: Vec[i64] }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             struct Two { a: R, b: R }\n\
             struct Ho2 { a: R, b: Option[R] }\n\
             enum Wrap { W(Ho2), T(Two), N }\n\
             enum E { V(R), N }\n\
             fn mk(k: i64) -> R { return R { id: k, tag: f\"t{k}\", xs: [k] } }\n\
             fn main() {\n\
             \x20   { let w: Wrap = Wrap.T(Two { a: mk(1), b: mk(101) }); println(\"one\") }\n\
             \x20   { let w: Wrap = Wrap.T(Two { a: mk(2), b: mk(102) }); match w { _ => println(\"n\") } println(\"two\") }\n\
             \x20   { let w: Wrap = Wrap.T(Two { a: mk(3), b: mk(103) }); match w { Wrap.T(h) => { println(\"in\") }, _ => println(\"n\") } println(\"three\") }\n\
             \x20   { let w: Wrap = Wrap.W(Ho2 { a: mk(4), b: Option.Some(mk(104)) }); println(\"four\") }\n\
             \x20   { let w: Wrap = Wrap.W(Ho2 { a: mk(5), b: Option.Some(mk(105)) }); match w { _ => println(\"n\") } println(\"five\") }\n\
             \x20   { let w: Wrap = Wrap.W(Ho2 { a: mk(6), b: Option.Some(mk(106)) }); match w { Wrap.W(h) => { println(\"in\") }, _ => println(\"n\") } println(\"six\") }\n\
             \x20   { let w: Wrap = Wrap.W(Ho2 { a: mk(7), b: Option.Some(mk(107)) }); match w { Wrap.W(h) => { let Ho2 { a, b } = h; println(\"in\") }, _ => println(\"n\") } println(\"seven\") }\n\
             \x20   { let w: Wrap = Wrap.W(Ho2 { a: mk(8), b: Option.None }); match w { Wrap.W(h) => { println(\"in\") }, _ => println(\"n\") } println(\"eight\") }\n\
             \x20   { let e: E = E.V(mk(9)); println(\"nine\") }\n\
             \x20   { let w: Wrap = Wrap.N; match w { Wrap.W(h) => { println(\"in\") }, _ => println(\"n\") } println(\"ten\") }\n\
             \x20   println(\"end\")\n\
             }\n\
             ",
        ) else {
            return;
        };
    assert_eq!(out, "dR101\ndR1\none\nn\ndR102\ndR2\ntwo\nin\ndR103\ndR3\nthree\ndR104\ndR4\nfour\nn\ndR105\ndR5\nfive\nin\ndR106\ndR6\nsix\ndR107\ndR7\nin\nseven\nin\ndR8\neight\ndR9\nnine\nn\nten\nend\n");
}

/// B-2026-09-07-16 — a by-value ENUM param whose payload struct owns heap
/// the entry copy CANNOT duplicate is owned by TRANSFER, not by copy.
///
/// `EnumDropKind::NestedOwnedStruct` exists (B-2026-09-05-26) for a payload
/// struct the two `NestedStruct` admissions decline — not copy-supported (a
/// `Drop`-bearing or `Map` field, an `Option` field whose payload is not
/// itself copyable) — yet reachable by the drop switch. When such a payload
/// owns heap BELOW it, `deep_copy_enum_heap_payload_in_place` duplicates
/// nothing for it, and that refusal is deliberate: a PARTIAL copy would
/// alias exactly the fields the walk skipped, which the drop then frees
/// twice. So the callee's slot and the caller's temp were ONE buffer and
/// both frames registered a drop over it.
///
/// B-2026-09-06-4 fixed the neighbouring class — a `NestedOwnedStruct`
/// owning NO heap, where the box IS the whole cleanup and duplicating the
/// envelope alone is a complete copy. This is the half that fix
/// deliberately did not reach, and extending the copy is not the fix it
/// looks like. The callee takes the caller's buffers instead
/// (`enum_param_owned_by_transfer`, the enum sibling of B-2026-08-05-33's
/// struct bargain) and the caller retracts at all three of its sites.
///
/// Every payload class in the row's table is here, because the failure
/// MODE differed across them and a fixture carrying only the abort would
/// not have covered the rest: `X1 { Option[i64], String }` and
/// `Xd { Option[i64], R2 }` (a `Drop` field that owns heap) aborted
/// `free(): double free detected in tcache 2` at both opt levels;
/// `X3 { Option[i64], Map }` aborted at `-O2` and SEGV'd at `-O0`; the
/// INLINE `M { Map, i64 }` SEGV'd at both. `--interp` was right on every
/// one of them.
///
/// Ten call shapes, because the caller-side registration differs across
/// them and each stands down through a different site: a fresh ctor temp
/// (`c1`), a NAMED LOCAL (`c2` — the binding's own cleanup, which no
/// existing retraction reached), a MATCH-CONSUMING callee over both
/// spellings (`c3`/`c4`), a HAND-BACK (`c5`), a two-hop pass-through
/// (`c6`), and the METHOD and ASSOC twins over both spellings
/// (`c7`–`c10`). `c10` is the one that needed a second fix: `Type.f(a)`
/// never called the declined-copy retraction at all.
///
/// `dR215`/`dR216` pin that a `Drop` body fires exactly ONCE and inside
/// the callee, where the memory now lives — the payload-bodies walker
/// moves with the memory, or it would read a buffer the callee has freed.
/// `dC17`/`dC18` are the CONTROL: `Ctl { String }` is copy-supported, so it
/// classifies `NestedStruct`, stays entry-copied, and must be untouched.
#[test]
fn test_e2e_by_value_enum_param_with_owning_struct_payload_transfers() {
    let Some(out) = run_program(
        r#"struct X1 { a: Option[i64], s: String }
struct X3 { a: Option[i64], m: Map[i64, String] }
struct M  { m: Map[i64, String], n: i64 }
struct R2 { id: i64, s: String }
impl Drop for R2 { fn drop(mut ref self) { println(f"dR2{self.id}") } }
struct Xd { a: Option[i64], r: R2 }
struct Ctl { s: String }
impl Drop for Ctl { fn drop(mut ref self) { println(f"dC{self.s}") } }
enum W { T(X1), U(i64) }
enum V { T(X3), U(i64) }
enum Y { T(M), U(i64) }
enum D { T(Xd), U(i64) }
enum C { T(Ctl), U(i64) }
struct H { n: i64 }
fn sink(w: W) {}
fn eat(w: W) -> i64 { return match w { W.T(x) => x.a.unwrap_or(0), W.U(n) => n } }
fn hand(w: W) -> W { return w; }
fn hop(w: W) { sink(w); }
fn sinkv(v: V) {}
fn sinky(y: Y) {}
fn sinkd(d: D) {}
fn sinkc(c: C) {}
impl H { fn pv(ref self, w: W) {} }
impl W { fn av(w: W) {} }
fn mkx(i: i64) -> X1 { return X1 { a: Option.Some(i), s: f"s{i}" }; }
fn mkm(i: i64) -> Map[i64, String] { let mut m: Map[i64, String] = Map.new(); m.insert(i, f"v{i}"); return m; }
fn main() {
  sink(W.T(mkx(1)));                       println("c1")
  let a = W.T(mkx(2)); sink(a);            println("c2")
  println(f"c3={eat(W.T(mkx(3)))}")
  let b = W.T(mkx(4)); println(f"c4={eat(b)}")
  let z = hand(W.T(mkx(5))); println(f"c5={eat(z)}")
  hop(W.T(mkx(6)));                        println("c6")
  let h = H { n: 1 };
  h.pv(W.T(mkx(7)));                       println("c7")
  let c = W.T(mkx(8)); h.pv(c);            println("c8")
  W.av(W.T(mkx(9)));                       println("c9")
  let d = W.T(mkx(10)); W.av(d);           println("c10")
  sinkv(V.T(X3 { a: Option.Some(11), m: mkm(11) })); println("c11")
  let e = V.T(X3 { a: Option.Some(12), m: mkm(12) }); sinkv(e); println("c12")
  sinky(Y.T(M { m: mkm(13), n: 13 }));     println("c13")
  let f = Y.T(M { m: mkm(14), n: 14 }); sinky(f); println("c14")
  sinkd(D.T(Xd { a: Option.Some(15), r: R2 { id: 15, s: "h15" } })); println("c15")
  let g = D.T(Xd { a: Option.Some(16), r: R2 { id: 16, s: "h16" } }); sinkd(g); println("c16")
  sinkc(C.T(Ctl { s: "17" }));             println("c17")
  let i = C.T(Ctl { s: "18" }); sinkc(i);  println("c18")
  println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(
            out,
            "c1\nc2\nc3=3\nc4=4\nc5=5\nc6\nc7\nc8\nc9\nc10\nc11\nc12\nc13\nc14\ndR215\nc15\ndR216\nc16\ndC17\nc17\ndC18\nc18\nend\n"
        );
}

/// B-2026-09-02-45 — a function parameter's `Option`/`Result` PAYLOAD type
/// does not outlive the function that declared it.
///
/// `var_option_payload_te` / `var_result_payload_te` are keyed by BINDING
/// NAME, and they were the only name-keyed type side-tables in codegen that
/// were never cleared at a function boundary — `var_type_names`,
/// `enum_inst_var_types`, `vec_elem_types`, `array_var_elem_te` and the rest
/// all are, for the reason `enum_inst_var_types`' comment spells out.
///
/// So an UNUSED declaration published a payload type that any same-named
/// binding in a later function inherited, including a scalar: the `let x:
/// i64 = 7` below rendered through the `Option` path. B-2026-08-31-49 and
/// 78e1389 both patched this class by retracting on RE-REGISTRATION of the
/// name, which only helps when the later binding goes through
/// `register_var_from_type_expr` — and a scalar `let` does not, which is
/// why the hole survived both.
///
/// The symptom was heap-state dependent and got WORSE, not better, as
/// B-2026-09-02-37's fix landed: with the payload correctly boxed, the
/// inherited registration `inttoptr`s a scalar, so a program that printed
/// `Some([0, 48])` for `1` started segfaulting instead.
#[test]
fn codegen_param_payload_type_does_not_leak_into_a_later_function() {
    let Some(out) = run_program(
        r#"fn other(x: Option[Vec[String]]) -> i64 { return 1 }
fn otherR(y: Result[Vec[String], String]) -> i64 { return 2 }
fn main() {
    let x: i64 = 7;
    println(f"{x}");
    let y: i64 = 8;
    println(f"{y}");
    let n: Option[Vec[String]] = None;
    let e: Result[Vec[String], String] = Err("e");
    println(f"{other(n)}{otherR(e)}");
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "7\n8\n12\n");
}

/// B-2026-09-03-2 — a generic body FORWARDING its `Option[T]` param to a
/// SECOND generic function, where the type argument is NAMELESS.
///
/// The solver binds `sink`'s `T` to `fwd`'s `T` — a `Type::TypeParam` —
/// and `record_call_type_subs`'s caller dropped any solution whose
/// resolved name equalled the param's own, meaning to skip an UNSOLVED
/// metavar. That name test cannot tell an unsolved metavar apart from a
/// genuine propagation when both functions spell the param `T`, so the
/// inner call recorded NO frame in any of the three channels and `sink`
/// lowered the payload at the erased one-word width. For a boxed payload
/// that word is the box POINTER: `s:94482719980304`, a fresh number every
/// run, on all three compiled surfaces where `--interp` printed
/// `[uv, wx]`. Renaming the outer param to `U` made the same program
/// correct on all four, which is what pinned the diagnosis.
///
/// EVERY LINE IS A DISTINCT CHANNEL:
///   * `fwd` at `Array[String, 2]` — the reported shape, a boxed payload
///     whose one erased word is a pointer.
///   * `fwd` at `i64` — the scalar CONTROL, correct before and after, so a
///     regression that breaks propagation generally fails here too.
///   * `outer` → `mid` → `sink` — THREE hops, where the resolution has to
///     chain rather than fire once.
///   * `h.m` — the same forward with a METHOD as the inner callee.
///   * `fwdU`, whose type param is spelled `U` — the control that
///     ISOLATED the name collision, and the one leg that passed pre-fix.
///   * two different NAMELESS instantiations through ONE `fwd`
///     (`Array[String, 2]` then `(i64, i64)`) — pre-fix these mangled
///     their inner monomorphs identically and shared one body, which
///     SEGFAULTED once the propagated binding started resolving. The
///     structural mangle axis reading `subst_call_te` is what separates
///     them.
///   * `fwd` at `Array[i64, 3]` and then at `Slice[i64]` — the last two
///     lines, and the ones that show the mangle axis had to stop asking
///     `type_expr_is_structural_type_arg` on the propagated arm. That
///     predicate is TUPLES ONLY, so an `Array` fell past it to a probe
///     that only answers for `StructType` and appended NOTHING — exactly
///     as a scalar did. Two arrays therefore still collided after the
///     tuple case was fixed, and measured EMPTY OUTPUT (a crash before
///     the first print); a `Slice[i64]` reached a preceding
///     `Array[i64, 3]`'s body and rendered as `[<ptr>, 2, 0]`. Both are
///     silent-wrong-value shapes, so they need pinning rather than
///     trusting that the earlier lines cover the axis.
///
/// Twin of `tests/interpreter.rs`'s
/// `test_generic_forward_to_a_second_generic_resolves_the_payload`, pinned
/// to the same string — the interpreter was correct throughout, so it is
/// the oracle here rather than a second subject.
#[test]
fn codegen_generic_forward_to_a_second_generic_resolves_the_payload() {
    let Some(out) = run_program(
        r#"fn sink[T: Display](x: Option[T]) { match x { Some(t) => { println(f"s:{t}") } None => { println("n") } } }
fn fwd[T: Display](x: Option[T]) { sink(x) }
fn mid[T: Display](x: Option[T]) { sink(x) }
fn outer[T: Display](x: Option[T]) { mid(x) }
fn fwdU[U: Display](x: Option[U]) { sink(x) }
struct H { }
impl H {
    fn m[T: Display](ref self, x: Option[T]) { match x { Some(t) => { println(f"m:{t}") } None => { println("n") } } }
}
fn fwdm[T: Display](h: ref H, x: Option[T]) { h.m(x) }
fn main() {
    let a: Array[String, 2] = ["uv", "wx"];
    fwd(Some(a));
    fwd(Some(7));
    let b: Array[String, 2] = ["yz", "ab"];
    outer(Some(b));
    let c: Array[String, 2] = ["cd", "ef"];
    fwdU(Some(c));
    let h = H { };
    let d: Array[String, 2] = ["gh", "ij"];
    fwdm(h, Some(d));
    let e: Array[String, 2] = ["kl", "mn"];
    fwd(Some(e));
    let t: (i64, i64) = (5, 6);
    fwd(Some(t));
    let g: Array[i64, 3] = [1, 2, 3];
    fwd(Some(g));
    let sl: Slice[i64] = g[0..2];
    fwd(Some(sl));
}
"#,
    ) else {
        return;
    };
    assert_eq!(
            out,
            "s:[uv, wx]\ns:7\ns:[yz, ab]\ns:[cd, ef]\nm:[gh, ij]\ns:[kl, mn]\ns:(5, 6)\ns:[1, 2, 3]\ns:[1, 2]\n"
        );
}

/// B-2026-08-01-10 — a bare USER-ENUM constructor statement
/// (`Box2.Full(Res { .. });`) discards its value: the payload's Drop
/// body must fire at the `;` and the payload heap must be freed, same
/// as the wildcard-let twin. Pre-fix the bare arm had no ctor channel
/// (the battery's registrar resolves fn/method return types only), so
/// `karac build` was body-silent while `karac run` fired — and a heap
/// payload leaked. A discarded unit variant stays silent. Twin of
/// `tests/interpreter.rs`'s `test_bare_user_enum_ctor_discard`.
#[test]
fn e2e_bare_user_enum_ctor_discard() {
    let Some(out) = run_program(
        "struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"drop {self.id} {self.name}\")\n\
             \x20   }\n\
             }\n\
             enum Box2 { Full(Res), Empty }\n\
             fn main() {\n\
             \x20   println(\"a\");\n\
             \x20   Box2.Full(Res { id: 24, name: f\"h{24}\" });\n\
             \x20   println(\"b\");\n\
             \x20   Box2.Empty;\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "a\ndrop 24 h24\nb\nend\n");
}

/// B-2026-08-25-15 — a heap FIELD read out of a BORROW-accessor match
/// payload and consumed DIRECTLY, with no intervening `let`.
///
/// `match self.values.get(i) { Some(pv) => … }` on a `ref self` receiver
/// binds `pv` as a shallow bit-copy of the container's element, so the
/// container's per-element drain owns and frees its `String` fields.
/// `register_borrowed_agg_payload_struct_bindings` already recorded such
/// bindings for the LET-site copier, and `let s = pv.value; return s;`
/// was correct throughout (`via_let` below is that control). But the
/// let-site copier only fires at a `let`, so the two DIRECT consume
/// spellings got no copy from anywhere and handed the sink an alias:
/// `return pv.value` and `out.push(pv.value)` both aborted with
/// `free(): double free detected in tcache 2` under JIT/AOT while
/// `--interp` printed the right answer. The arg/return-site copier now
/// admits the same root class its two siblings already carry.
///
/// The trailing second `h.direct()` is load-bearing: it proves the
/// container survived all three escapes rather than merely not crashing
/// on the way out. Every payload is built with an f-string — a string
/// LITERAL is static with `cap == 0`, so every free over it is a no-op
/// and the double free is unobservable (two earlier reductions of this
/// bug were false negatives for exactly that reason). ASAN/LSan twin in
/// `tests/memory_sanitizer.rs`.
#[test]
fn e2e_borrow_payload_field_direct_consume_no_double_free() {
    let Some(out) = run_program(
        "struct Pv { name: String, value: String }\n\
             struct Holder { values: Vec[Pv] }\n\
             impl Holder {\n\
             \x20   fn direct(ref self) -> String {\n\
             \x20       match self.values.get(0) {\n\
             \x20           Some(pv) => { return pv.value; }\n\
             \x20           None => { return \"none\"; }\n\
             \x20       }\n\
             \x20   }\n\
             \x20   fn via_let(ref self) -> String {\n\
             \x20       match self.values.get(1) {\n\
             \x20           Some(pv) => { let s = pv.value; return s; }\n\
             \x20           None => { return \"none\"; }\n\
             \x20       }\n\
             \x20   }\n\
             \x20   fn collect(ref self) -> Vec[String] {\n\
             \x20       let mut out: Vec[String] = Vec.new();\n\
             \x20       let mut i = 0;\n\
             \x20       while i < self.values.len() {\n\
             \x20           match self.values.get(i) {\n\
             \x20               Some(pv) => { out.push(pv.value); }\n\
             \x20               None => {}\n\
             \x20           }\n\
             \x20           i = i + 1;\n\
             \x20       }\n\
             \x20       return out;\n\
             \x20   }\n\
             }\n\
             fn main() {\n\
             \x20   let mut v: Vec[Pv] = Vec.new();\n\
             \x20   v.push(Pv { name: f\"k{1}\", value: f\"a{1}\" });\n\
             \x20   v.push(Pv { name: f\"k{2}\", value: f\"b{2}\" });\n\
             \x20   let h = Holder { values: v };\n\
             \x20   println(h.direct());\n\
             \x20   println(h.via_let());\n\
             \x20   let got = h.collect();\n\
             \x20   for g in got { println(g); }\n\
             \x20   println(h.direct());\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "a1\nb2\na1\nb2\na1\n");
}

/// B-2026-09-13-25 — a qualified FIELD-LESS enum-variant constant with
/// pinned type arguments (`Ho[i64].Empty`) lowers and runs.
///
/// The defect was a front-end reject, so the interesting part downstream is
/// that nothing downstream had to change: the new parser shape is the same
/// two-segment `Path` the unqualified `Ho.Empty` already produced, with the
/// args attached, and every codegen arm matches `Path { segments, .. }`.
/// This asserts that claim rather than trusting it — a lowering that read
/// the args as a subscript, or lost the monomorph, would fail here and
/// nowhere in `tests/typechecker.rs`.
#[test]
fn e2e_qualified_field_less_variant_with_pinned_type_args() {
    for (label, src, want) in [
            (
                "user-generic-enum",
                "enum Ho[T] { Full(T), Empty }\n\
                 fn takeit(x: Ho[i64]) { match x { Full(r) => { println(f\"f:{r}\") } Empty => { println(\"e\") } } }\n\
                 fn main() { takeit(Ho[i64].Empty); println(\"end\"); }\n",
                "e\nend\n",
            ),
            (
                "seeded-option",
                "fn takeit(x: Option[i64]) { match x { Some(r) => { println(f\"f:{r}\") } None => { println(\"e\") } } }\n\
                 fn main() { takeit(Option[i64].None); println(\"end\"); }\n",
                "e\nend\n",
            ),
            (
                "annotated-binding",
                "enum Ho[T] { Full(T), Empty }\n\
                 fn main() { let v: Ho[i64] = Ho[i64].Empty;\n\
                 match v { Full(r) => { println(f\"f:{r}\") } Empty => { println(\"e\") } } }\n",
                "e\n",
            ),
            (
                "two-parameters",
                "enum Pair[A, B] { L(A), R(B), N }\n\
                 fn takeit(x: Pair[i64, String]) { match x { L(a) => { println(f\"l:{a}\") } R(b) => { println(f\"r:{b}\") } N => { println(\"n\") } } }\n\
                 fn main() { takeit(Pair[i64, String].N); }\n",
                "n\n",
            ),
            (
                "payload-carrying-sibling-still-runs",
                "enum Ho[T] { Full(T), Empty }\n\
                 fn takeit(x: Ho[i64]) { match x { Full(r) => { println(f\"f:{r}\") } Empty => { println(\"e\") } } }\n\
                 fn main() { takeit(Ho[i64].Full(7)); println(\"end\"); }\n",
                "f:7\nend\n",
            ),
        ] {
            assert_eq!(run_program(src).as_deref(), Some(want), "{label}");
        }
}

/// B-2026-09-14-4 — a qualified STRUCT-SHAPED enum-variant literal with
/// pinned type arguments (`Sh[i64].S { v: 3 }`) lowers and runs.
///
/// The defect was a parse error, so what this checks downstream is that the
/// new shape reaches codegen as the two-segment `StructLiteral` the
/// unqualified `Enum.Variant { .. }` form already produced — nothing in the
/// backend had to learn the spelling, and this asserts that rather than
/// trusting it.
#[test]
fn e2e_qualified_struct_shaped_variant_with_pinned_type_args() {
    for (label, src, want) in [
            (
                "annotated-binding",
                "enum Sh[T] { S { v: T }, E }\n\
                 fn main() { let x: Sh[i64] = Sh[i64].S { v: 3 };\n\
                 match x { S { v } => { println(f\"{v}\") } E => { println(\"e\") } } }\n",
                "3\n",
            ),
            (
                "argument-position",
                "enum Sh[T] { S { v: T }, E }\n\
                 fn takeit(x: Sh[i64]) { match x { S { v } => { println(f\"{v}\") } E => { println(\"e\") } } }\n\
                 fn main() { takeit(Sh[i64].S { v: 3 }); }\n",
                "3\n",
            ),
            (
                "phantom-parameter",
                "enum Sh[T] { S { n: i64 }, E }\n\
                 fn main() { let x = Sh[i64].S { n: 3 };\n\
                 match x { S { n } => { println(f\"{n}\") } E => { println(\"e\") } } }\n",
                "3\n",
            ),
            (
                "two-parameters",
                "enum Pr[A, B] { L { a: A }, R { b: B }, N }\n\
                 fn takeit(x: Pr[i64, String]) { match x { L { a } => { println(f\"l:{a}\") } R { b } => { println(f\"r:{b}\") } N => { println(\"n\") } } }\n\
                 fn main() { takeit(Pr[i64, String].L { a: 7 }); }\n",
                "l:7\n",
            ),
        ] {
            assert_eq!(run_program(src).as_deref(), Some(want), "{label}");
        }
}

/// B-2026-08-29-4 — a METHOD that hands an inline `Option`/`Result`
/// argument back ALIASES the source binding's payload, so the result must be
/// recorded as an alias rather than tracked as a fresh owner.
///
/// Pre-fix both bindings freed the same buffer once the result was
/// consumed, aborting the process (`free(): double free detected in tcache
/// 2`) on all three compiled backends while the interpreter and the
/// free-function twin were correct — so `run_program` returns `None` here
/// rather than wrong bytes. The memory twin is
/// `asan_method_passthrough_arg_aliases_the_source_payload`.
///
/// Every case binds the result with `let` first: the INLINE-scrutinee
/// spelling double-frees for a FREE FUNCTION too and is a different, wider
/// defect tracked on its own row.
#[test]
fn e2e_method_passthrough_arg_aliases_the_source_payload() {
    const DECLS: &str = "struct Bx { n: i64 }\n\
             impl Bx {\n\
             fn take(ref self, o: Option[String]) -> Option[String] { o }\n\
             fn second(ref self, a: Option[String], b: Option[String]) -> Option[String] { b }\n\
             fn first(ref self, a: Option[String], b: Option[String]) -> Option[String] { a }\n\
             fn fresh(ref self, o: Option[String]) -> Option[String] { Option.Some(f\"fresh{self.n}\") }\n\
             }\n";
    for (label, body, want) in [
        // The headline shape: one argument, handed straight back.
        (
            "single-arg-passthrough",
            "fn main() { let b = Bx { n: 1 }; let s = Option.Some(f\"a{b.n}\"); \
                 let o = b.take(s); \
                 match o { Option.Some(v) => { println(f\"got {v}\"); } \
                 Option.None => { println(\"none\"); } } }\n",
            "got a1\n",
        ),
        // TWO arguments, the SECOND handed back — the first must keep its own
        // owner and the second must alias.
        (
            "two-args-second-returned",
            "fn main() { let b = Bx { n: 1 }; let p = Option.Some(f\"d{b.n}\"); \
                 let q = Option.Some(f\"e{b.n}\"); let o = b.second(p, q); \
                 match o { Option.Some(v) => { println(f\"got {v}\"); } \
                 Option.None => { println(\"none\"); } } }\n",
            "got e1\n",
        ),
        // TWO arguments, the FIRST handed back. Together with the case above
        // this pins that the alias is recorded for the argument the callee
        // actually returns rather than for a fixed position.
        (
            "two-args-first-returned",
            "fn main() { let b = Bx { n: 1 }; let p = Option.Some(f\"f{b.n}\"); \
                 let q = Option.Some(f\"g{b.n}\"); let o = b.first(p, q); \
                 match o { Option.Some(v) => { println(f\"got {v}\"); } \
                 Option.None => { println(\"none\"); } } }\n",
            "got f1\n",
        ),
        // CONTROL — the method does NOT hand the argument back, so the
        // result owns a freshly produced payload. No alias may be recorded,
        // or the fresh payload is left with no owner at all.
        (
            "no-passthrough-keeps-fresh-owner",
            "fn main() { let b = Bx { n: 1 }; let s = Option.Some(f\"h{b.n}\"); \
                 let o = b.fresh(s); \
                 match o { Option.Some(v) => { println(f\"got {v}\"); } \
                 Option.None => { println(\"none\"); } } }\n",
            "got fresh1\n",
        ),
        // CONTROL — the free-function twin, correct before and after. It is
        // the oracle that showed one allocation with two frees was wrong
        // rather than merely different.
        (
            "free-fn-oracle-passthrough",
            "fn takef(o: Option[String]) -> Option[String] { o }\n\
                 fn main() { let n = 1; let s = Option.Some(f\"a{n}\"); let o = takef(s); \
                 match o { Option.Some(v) => { println(f\"got {v}\"); } \
                 Option.None => { println(\"none\"); } } }\n",
            "got a1\n",
        ),
    ] {
        assert_eq!(
            run_program(&format!("{DECLS}{body}")).as_deref(),
            Some(want),
            "{label}"
        );
    }
}

#[test]
fn e2e_hash_container_enum_element_runs_its_body() {
    const H: &str = "#[derive(Hash, Eq, PartialEq)]\n\
             enum Tg { Named { s: String }, Num { n: i64 } }\n\
             impl Drop for Tg { fn drop(mut ref self) { println(\"dD\") } }\n\
             fn mkd(n: i64) -> Tg { return Tg.Named { s: f\"aaaaaaaaaaaaaaaa-{n}\" }; }\n";
    for (label, body, want) in [
        // The row's own cell: a Set element, insert only, no lookup.
        (
            "set-element",
            "fn main() {\n\
                 \x20   let mut s: Set[Tg] = Set.new();\n\
                 \x20   s.insert(mkd(0i64));\n\
                 \x20   println(f\"len:{s.len()}\");\n\
                 }\n",
            "len:1\ndD\n",
        ),
        (
            "map-key",
            "fn main() {\n\
                 \x20   let mut m: Map[Tg, i64] = Map.new();\n\
                 \x20   m.insert(mkd(0i64), 7i64);\n\
                 \x20   println(f\"len:{m.len()}\");\n\
                 }\n",
            "len:1\ndD\n",
        ),
        // NOT in the row, measured on this fix: the enum as the map's
        // VALUE. The row listed it as the open question that would say
        // whether the defect is about KEYS or about hash-container
        // elements generally. It is the latter — this was silent too.
        (
            "map-value",
            "fn main() {\n\
                 \x20   let mut m: Map[i64, Tg] = Map.new();\n\
                 \x20   m.insert(3i64, mkd(0i64));\n\
                 \x20   println(f\"len:{m.len()}\");\n\
                 }\n",
            "len:1\ndD\n",
        ),
        // Also not in the row: the container held as a struct FIELD rather
        // than a local binding. Codegen reaches both spellings through one
        // walker, but the interpreter has two — patching only the binding
        // one turned this cell into a run-vs-build divergence mid-fix
        // (`interp=0 build=1`), which is what put the second arm in scope.
        (
            "set-in-struct-field",
            "struct Holder { mut s: Set[Tg] }\n\
                 fn main() {\n\
                 \x20   let mut h = Holder { s: Set.new() };\n\
                 \x20   h.s.insert(mkd(0i64));\n\
                 \x20   println(f\"len:{h.s.len()}\");\n\
                 }\n",
            "len:1\ndD\n",
        ),
    ] {
        let prog = format!("{H}{body}");
        assert_eq!(run_program(&prog).as_deref(), Some(want), "{label}");
    }
    // The SORTED sibling, the row's other open question: it destroys in KEY
    // order through a different walk (the `karac_map_sorted_keys` leg), and
    // was silent in the same way. Its own prelude because `SortedSet`
    // requires `#[derive(Ord)]` on the element.
    assert_eq!(
        run_program(
            "#[derive(Hash, Eq, PartialEq, Ord)]\n\
                 enum Tg { Named { s: String }, Num { n: i64 } }\n\
                 impl Drop for Tg { fn drop(mut ref self) { println(\"dD\") } }\n\
                 fn mkd(n: i64) -> Tg { return Tg.Named { s: f\"aaaaaaaaaaaaaaaa-{n}\" }; }\n\
                 fn main() {\n\
                 \x20   let mut s: SortedSet[Tg] = SortedSet.new();\n\
                 \x20   s.insert(mkd(0i64));\n\
                 \x20   println(f\"len:{s.len()}\");\n\
                 }\n"
        )
        .as_deref(),
        Some("len:1\ndD\n"),
        "sorted-set element — the sorted walk had the same hole"
    );
    // CONTROLS — both already correct before the fix, and both are what
    // isolate the axis. A widening that fired per-container rather than
    // per-element shows up here as a doubled body.
    const CH: &str = "#[derive(Hash, Eq, PartialEq)]\n\
             struct Dk { s: String }\n\
             impl Drop for Dk { fn drop(mut ref self) { println(\"dD\") } }\n\
             fn mks(n: i64) -> Dk { return Dk { s: f\"aaaaaaaaaaaaaaaa-{n}\" }; }\n";
    assert_eq!(
        run_program(&format!(
            "{CH}fn main() {{\n\
                 \x20   let mut m: Map[Dk, i64] = Map.new();\n\
                 \x20   m.insert(mks(0i64), 7i64);\n\
                 \x20   println(f\"len:{{m.len()}}\");\n\
                 }}\n"
        ))
        .as_deref(),
        Some("len:1\ndD\n"),
        "struct element in the same Map — the control that says the axis is the ENUM"
    );
    assert_eq!(
        run_program(&format!(
            "{H}fn main() {{\n\
                 \x20   let mut v: Vec[Tg] = Vec.new();\n\
                 \x20   v.push(mkd(0i64));\n\
                 \x20   println(f\"len:{{v.len()}}\");\n\
                 }}\n"
        ))
        .as_deref(),
        Some("len:1\ndD\n"),
        "Vec of the same enum — the control that says the axis is the HASH CONTAINER"
    );
}

/// B-2026-08-30-16 — the SHARED-enum flavour of B-2026-08-29-28's
/// scrutinee-lifetime fix, which that row could not reach.
///
/// The two enum flavours release through different channels: a VALUE enum's
/// `Drop` body is a standalone `UserDrop` action that -28 moved to the
/// construct's exit, while a SHARED enum's runs from inside
/// `__karac_rc_drop_<E>` when the count reaches zero — so the only way to
/// move the body is to move the DECREMENT. Pre-fix, all three compiled
/// surfaces printed `v1 s1 s2 dSe` against the interpreter's
/// `v1 dSe s1 s2`: the box outlived the match to the enclosing scope's exit,
/// the extended lease design.md § Temporary Lifetime Rules forbids.
///
/// `escaping-payload` IS THE ROW'S OWN OBJECTION, TURNED INTO A TEST. Moving
/// a decrement is not the same as moving a body — a dec that reaches zero
/// frees the box, so the row argued an arm binding aliasing into it would be
/// a use-after-free rather than a reordering. This row is that shape: the
/// arm MOVES the `String` payload out and the match's value is read AFTER
/// the dec. It is clean because `suppress_shared_enum_payload_move_out`
/// already zeroed the consumed field's words in the box (B-2026-08-28-74),
/// so the binding owns the payload outright and the box no longer references
/// it. Valgrind agrees at `KARAC_OPT_LEVEL=0` — no invalid read, no leak.
///
/// `nested` and `in-loop-body` pin that the release is PER CONSTRUCT rather
/// than per function: an inner match must drop its own box before the outer
/// arm continues, and a loop must drop each iteration's box at that
/// iteration's match exit — the shape where scope-exit placement would
/// otherwise pile every iteration's box up until the function returned.
#[test]
fn e2e_freshtemp_shared_enum_scrutinee_releases_at_construct_exit() {
    const H: &str = "shared enum Se { A(i64), B }\n\
             impl Drop for Se { fn drop(mut ref self) { println(\"dSe\") } }\n\
             shared enum Sh { A(String), B }\n\
             impl Drop for Sh { fn drop(mut ref self) { println(\"dSh\") } }\n\
             fn mkSe(n: i64) -> Se { return Se.A(n) }\n\
             fn mkSh(n: i64) -> Sh { return Sh.A(f\"pay{n}\") }\n";
    for (label, body, want) in [
            (
                "statement-match",
                "match mkSe(1) { Se.A(n) => { println(f\"v{n}\") } Se.B => {} }\n\
                 println(\"s1\")\n",
                "v1\ndSe\ns1\npost\n",
            ),
            (
                "heap-payload",
                "match mkSh(1) { Sh.A(s) => { println(f\"v[{s}]\") } Sh.B => {} }\n\
                 println(\"s1\")\n",
                "v[pay1]\ndSh\ns1\npost\n",
            ),
            (
                "escaping-payload",
                "let out = match mkSh(2) { Sh.A(s) => { s } Sh.B => { \"none\".to_string() } };\n\
                 println(f\"out[{out}]\")\n",
                "dSh\nout[pay2]\npost\n",
            ),
            (
                "nested",
                "match mkSe(1) { Se.A(n) => { match mkSe(2) { Se.A(m) => { println(f\"i{m}\") } Se.B => {} }\n\
                 \x20   println(f\"o{n}\") } Se.B => {} }\n",
                "i2\ndSe\no1\ndSe\npost\n",
            ),
            (
                "in-loop-body",
                "let mut i = 0;\n\
                 while i < 2 { match mkSe(i) { Se.A(n) => { println(f\"w{n}\") } Se.B => {} }\n\
                 \x20   println(\"it\"); i = i + 1; }\n",
                "w0\ndSe\nit\nw1\ndSe\nit\npost\n",
            ),
        ] {
            let src = format!("{H}#[allow(partial_move_of_drop_enum)]\nfn main() {{ {body} println(\"post\") }}\n");
            assert_eq!(run_program(&src).as_deref(), Some(want), "{label}");
        }
}

/// B-2026-08-28-59 — a NAMED tuple-destructure leaf of ENUM type runs the
/// enum's OWN `Drop` body, not just its payload's.
///
/// `track_destructure_leaf_cleanup`'s enum arm registered the memory
/// (`track_enum_var`) and the live variant's PAYLOAD walk, then returned —
/// before reaching the struct arm below it, which has carried the own-body
/// cascade since B-2026-08-28-1. So the leaf WAS reached and walked, and
/// only `<E>.drop` was missing.
///
/// THE PAYLOAD/UNIT SPLIT IS THE DIAGNOSTIC, and both spellings are rows
/// here because they show the same loss at different contrast: with a
/// payload the compiled side still printed `dR5` and lost `dE` alone, which
/// is what proves the registration existed; the unit variant has no payload
/// to hide behind and showed the loss bare.
///
/// The own body is registered LAST so the LIFO drain fires it FIRST — own
/// body, then payload, then memory. That is the interpreter's order and
/// what the parity gate compares; registering it earlier would print `dR5`
/// before `dE`.
///
/// `param-source-control` is the row that holds the `register_user_bodies`
/// gate: a by-value tuple PARAM already owns its elements' bodies
/// caller-side, so the leaf must not register a second one.
#[test]
fn e2e_named_enum_tuple_leaf_runs_the_enums_own_drop_body() {
    const H: &str = "struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             enum E { A(R), B }\n\
             impl Drop for E { fn drop(mut ref self) { println(\"dE\") } }\n";
    for (label, body, want) in [
        // The row's own repro, fresh tuple-literal source.
        (
            "fresh-payload-variant",
            "fn main() { let (gv, gn) = (E.A(R { id: 5 }), 6); println(f\"{gn}\") }\n",
            "dE\ndR5\n6\n",
        ),
        // The unit variant, where the loss showed bare.
        (
            "fresh-unit-variant",
            "fn main() { let (gv, gn) = (E.B, 6); println(f\"{gn}\") }\n",
            "dE\n6\n",
        ),
        // PLACE source, both variants.
        (
            "place-payload-variant",
            "fn main() { let g = (E.A(R { id: 5 }), 6); let (gv, gn) = g;\n\
                 \x20            println(f\"{gn}\") }\n",
            "dE\ndR5\n6\n",
        ),
        (
            "place-unit-variant",
            "fn main() { let g = (E.B, 6); let (gv, gn) = g; println(f\"{gn}\") }\n",
            "dE\n6\n",
        ),
        // The leaf CONSUMED by a call rather than left to die.
        (
            "consumed-leaf",
            "fn take(e: E) -> i64 { 7 }\n\
                 fn main() { let (gv, gn) = (E.A(R { id: 5 }), 6);\n\
                 \x20            println(f\"{take(gv) + gn}\") }\n",
            "13\ndE\ndR5\n",
        ),
        // CONTROL — the same enum BOUND directly, no destructure. Correct
        // before this and the yardstick for the order.
        (
            "bound-control",
            "fn main() { let gv = E.A(R { id: 5 }); println(\"mid\") }\n",
            "dE\ndR5\nmid\n",
        ),
        // CONTROL — the tuple never destructured.
        (
            "no-destructure-control",
            "fn main() { let g = (E.A(R { id: 5 }), 6); println(f\"{g.1}\") }\n",
            "6\ndE\ndR5\n",
        ),
        // CONTROL — a by-value tuple PARAM source. Its elements' bodies are
        // owned caller-side, so the leaf must register nothing; this is the
        // row that fails if the `register_user_bodies` gate is dropped.
        (
            "param-source-control",
            "fn take(p: (E, i64)) -> i64 { let (gv, gn) = p; gn }\n\
                 fn main() { let arg = (E.A(R { id: 5 }), 6); println(f\"{take(arg)}\") }\n",
            "6\ndE\ndR5\n",
        ),
    ] {
        let prog = format!("{H}{body}");
        assert_eq!(run_program(&prog).as_deref(), Some(want), "{label}");
    }
    // BOUNDARY — an enum with NO `Drop` of its own. Its payload body was
    // already correct and must stay at ONE: the new registration is gated
    // on the enum declaring `Drop`, not on it being an enum.
    assert_eq!(
        run_program(
            "struct R { id: i64 }\n\
                 impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
                 enum E { A(R), B }\n\
                 fn main() { let (gv, gn) = (E.A(R { id: 5 }), 6); println(f\"{gn}\") }\n"
        )
        .as_deref(),
        Some("dR5\n6\n"),
        "payload-only-enum-control"
    );
}

/// B-2026-08-28-43 — the BARE spelling of a unit variant (`B`, not `E.B`)
/// runs its enum's `Drop` body at every FRESH-TEMP position, and the bare
/// STATEMENT spellings run it at all.
///
/// B-2026-08-28-41 fixed `let _ = E.B` and left the bare `let _ = B` at
/// compiled ZERO, on the reading that the aggregate registrar declines
/// every `Identifier` by design. That reading was right about the
/// registrar and wrong about the scope: measured across the positions
/// below, `B` lost its body at EVERY fresh-temp position while `E.B` was
/// correct at all of them — argument 0/0/0, tuple wildcard leaf 1/0/0,
/// `let _ =` 1/0/0. One defect wearing three shapes, not two spellings.
///
/// The registrar's invariant survives intact, and that is the care here.
/// It still declines a let-bound `Identifier`, whose drop belongs to its
/// binding and would be double-freed by a second caller-side owner. What it
/// gained is the ability to tell that apart from a bare VARIANT:
/// `fresh_bare_unit_variant_enum` answers only for a name no local, const
/// or module binding shadows, and never through `enum_name_of_expr`, whose
/// `Identifier` arm resolves through `var_type_names` and so means exactly
/// the local case.
///
/// EVERY ROW CARRIES ITS QUALIFIED SIBLING, because spelling-dependence is
/// the finding: a regression that takes both spellings back to zero
/// together would otherwise read as a consistent gap rather than a
/// reintroduction of this one. `bound-*` and `passthrough` are the
/// double-free direction — each must stay at ONE.
#[test]
fn e2e_bare_unit_variant_of_own_drop_enum_runs_its_body() {
    const H: &str = "enum E { A(R), B }\n\
             impl Drop for E { fn drop(mut ref self) { println(\"drop E\") } }\n\
             struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop R{self.id}\") } }\n";
    for (label, body, want) in [
        // The row's first named spelling.
        (
            "let-discard-bare",
            "fn main() { let _ = B; println(\"end\") }\n",
            "drop E\nend\n",
        ),
        (
            "let-discard-qualified",
            "fn main() { let _ = E.B; println(\"end\") }\n",
            "drop E\nend\n",
        ),
        // The row's second named spelling — a bare STATEMENT, which reached
        // no discard arm at all in EITHER spelling and so ran zero bodies
        // on every backend.
        (
            "statement-bare",
            "fn main() { B; println(\"end\") }\n",
            "drop E\nend\n",
        ),
        (
            "statement-qualified",
            "fn main() { E.B; println(\"end\") }\n",
            "drop E\nend\n",
        ),
        // A fresh ARGUMENT — 0/0/0 before this, against the qualified
        // spelling's 1/1/1. Not in the row as filed; found by widening its
        // repro across positions.
        (
            "argument-bare",
            "fn take(e: E) -> i64 { 7 }\n\
                 fn main() { let x = take(B); println(f\"{x}\") }\n",
            "drop E\n7\n",
        ),
        (
            "argument-qualified",
            "fn take(e: E) -> i64 { 7 }\n\
                 fn main() { let x = take(E.B); println(f\"{x}\") }\n",
            "drop E\n7\n",
        ),
        // A tuple wildcard LEAF, both sources. Also not in the row: the
        // leaf's element type came back EMPTY for the bare spelling,
        // because `enum_name_of_expr` answers `None` for an `Identifier`
        // that is not a local.
        (
            "tuple-leaf-place-bare",
            "fn main() { let p = (B, 1); let (_, n) = p; println(f\"{n}\") }\n",
            "drop E\n1\n",
        ),
        (
            "tuple-leaf-place-qualified",
            "fn main() { let p = (E.B, 1); let (_, n) = p; println(f\"{n}\") }\n",
            "drop E\n1\n",
        ),
        (
            "tuple-leaf-fresh-bare",
            "fn main() { let (_, n) = (B, 1); println(f\"{n}\") }\n",
            "drop E\n1\n",
        ),
        (
            "tuple-leaf-fresh-qualified",
            "fn main() { let (_, n) = (E.B, 1); println(f\"{n}\") }\n",
            "drop E\n1\n",
        ),
        // The STRUCT-field wildcard, which was already correct in both
        // spellings — the one fresh-temp position the bare form reached.
        // Pinned so the fix cannot regress it while moving its siblings.
        (
            "struct-field-bare",
            "struct W { e: E, n: i64 }\n\
                 fn main() { let w = W { e: B, n: 1 };\n\
                 \x20            let W { e: _, n } = w; println(f\"{n}\") }\n",
            "drop E\n1\n",
        ),
        // CONTROL — BOUND. Correct throughout, and the yardstick every row
        // above is measured against.
        (
            "bound-control",
            "fn main() { let e = B; println(\"mid\") }\n",
            "drop E\nmid\n",
        ),
        // CONTROL, the DOUBLE-FREE direction — a bound local passed on. If
        // the registrar ever admitted a plain `Identifier` this goes to
        // two: the binding's drop plus a caller-side one over the same
        // value.
        (
            "bound-then-passed-control",
            "fn take(e: E) -> i64 { 7 }\n\
                 fn main() { let e = B; println(f\"{take(e)}\") }\n",
            "7\ndrop E\n",
        ),
        // CONTROL — a PASSTHROUGH callee. The result's consumer owns the
        // drop; firing caller-side as well would double it.
        (
            "passthrough-control",
            "fn pass(e: E) -> E { e }\n\
                 fn main() { let y = pass(B); println(\"mid\") }\n",
            "drop E\nmid\n",
        ),
        // CONTROL — a LOOP. One body per iteration, not an accumulating
        // registration.
        (
            "loop-control",
            "fn take(e: E) -> i64 { 7 }\n\
                 fn main() {\n\
                 \x20  let mut i = 0;\n\
                 \x20  while i < 3 { let _ = take(B); i = i + 1; }\n\
                 \x20  println(\"end\");\n\
                 \x20}\n",
            "drop E\ndrop E\ndrop E\nend\n",
        ),
    ] {
        let prog = format!("{H}{body}");
        assert_eq!(run_program(&prog).as_deref(), Some(want), "{label}");
    }
    // BOUNDARY — an enum with NO user `Drop` stays silent. The admission is
    // about reaching the body, not about inventing one.
    assert_eq!(
        run_program(
            "enum E { A(i64), B }\n\
                 fn take(e: E) -> i64 { 7 }\n\
                 fn main() { let x = take(B); println(f\"{x}\") }\n"
        )
        .as_deref(),
        Some("7\n"),
        "no-user-drop-control"
    );
    // BOUNDARY — a bare `None`. The seeded enums are excluded from the
    // admission on both backends (codegen filters `seeded_enum_names`; the
    // interpreter's scan reads `program.items`, which carries no baked
    // stdlib), so `Option`'s payload cleanup stays with its own machinery.
    assert_eq!(
        run_program(
            "fn take(o: Option[String]) -> i64 { 7 }\n\
                 fn main() { let x = take(None); println(f\"{x}\") }\n"
        )
        .as_deref(),
        Some("7\n"),
        "bare-None-control"
    );
    // Two enums, two bare names: each resolves to ITS OWN enum. The bare
    // spelling has no qualifier to disambiguate with, so this is where a
    // wrong owner would show up as the wrong body.
    assert_eq!(
        run_program(
            "enum E { A(i64), Z }\n\
                 impl Drop for E { fn drop(mut ref self) { println(\"drop E\") } }\n\
                 enum F { Y(i64), W }\n\
                 impl Drop for F { fn drop(mut ref self) { println(\"drop F\") } }\n\
                 fn take(e: E) -> i64 { 7 }\n\
                 fn main() { let x = take(Z); println(f\"{x}\"); let _ = W; println(\"end\") }\n"
        )
        .as_deref(),
        Some("drop E\n7\ndrop F\nend\n"),
        "two-enums-distinct-owners"
    );
}

/// B-2026-08-28-41 — `let _ = E.B`, a PAYLOADLESS variant of an
/// own-`impl Drop` enum, runs that enum's body.
///
/// Pre-fix it ran ZERO bodies on all three backends, which is why this is a
/// soundness row rather than a parity one — every backend agreed on the
/// wrong number, the failure mode no A/B gate can report.
///
/// The discard arm is gated on `discarded_owned_temp_tail`, a static,
/// purely syntactic predicate matching `Call` / `MethodCall` / a block tail
/// of one. A unit variant is a bare `Path`, so it entered the arm at all —
/// and thus the whole cleanup battery — never. Deciding that needs the enum
/// tables, hence a `&self` sibling rather than a widening of the static
/// predicate.
///
/// THE THREE CONTROLS ARE WHAT LOCALIZE IT, and they were the reason to
/// look at the discard site rather than at unit variants: the same value
/// drops exactly once when BOUND, as a fresh ARGUMENT, and through a tuple
/// wildcard LEAF, on every backend, before and after.
///
/// The BARE spelling (`let _ = B`) is deliberately absent and still runs
/// zero compiled. The aggregate registrar this arm feeds never matches an
/// `Identifier` argument — a let-bound enum's drop belongs to its binding,
/// and registering a second caller-side drop over it is a double free, not
/// a missing body — so admitting it here would buy nothing and imply
/// support that is not present. Filed as its own row.
#[test]
fn e2e_discarded_unit_variant_of_own_drop_enum_runs_its_body() {
    const H: &str = "enum E { A(R), B }\n\
             impl Drop for E { fn drop(mut ref self) { println(\"drop E\") } }\n\
             struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop R{self.id}\") } }\n";
    for (label, body, want) in [
        // The row's own repro.
        (
            "qualified-unit-variant",
            "fn main() { let _ = E.B; println(\"end\") }\n",
            "drop E\nend\n",
        ),
        // CONTROL — the same value BOUND. Correct before and after, and the
        // reason one body is the right answer.
        (
            "bound-control",
            "fn main() { let e = E.B; println(\"mid\") }\n",
            "drop E\nmid\n",
        ),
        // CONTROL — a fresh ARGUMENT.
        (
            // ORDER CORRECTED by B-2026-08-29-55 -- the argument temp dies AS THE
            // CALL RETURNS (design.md's "Function/method call argument | After the
            // call returns"), which is BEFORE the enclosing `println` runs. The old
            // expectation held it to the statement's `;`; `--interp` printed the
            // body first all along, so that line recorded a run-vs-build divergence
            // rather than a decision.
            "argument-control",
            "fn take(e: E) -> i64 { 7 }\n\
                 fn main() { println(f\"{take(E.B)}\") }\n",
            "drop E\n7\n",
        ),
        // CONTROL — a tuple wildcard LEAF (B-2026-08-28-31's shape).
        (
            "wildcard-leaf-control",
            "fn main() { let p = (E.B, 1); let (_, n) = p; println(f\"{n}\") }\n",
            "drop E\n1\n",
        ),
        // CONTROL — the payload-BEARING ctor at the same site, which
        // B-2026-08-28-39 settled at two bodies. The new arm must not
        // disturb it.
        (
            "payload-ctor-control",
            "fn main() { let _ = E.A(R { id: 41 }); println(\"end\") }\n",
            "drop E\ndrop R41\nend\n",
        ),
        // B-2026-08-28-48 — the compiled side of the BARE STATEMENT
        // position, both spellings. Compiled was already right for all
        // four of these rows; they are the parity anchor the interpreter
        // twin is measured against, and pinning them here is what makes a
        // future change to the compiled side visible as a failure rather
        // than as a silent re-divergence.
        (
            "qualified-ctor-stmt",
            "fn main() { E.A(R { id: 41 }); println(\"end\") }\n",
            "drop E\ndrop R41\nend\n",
        ),
        (
            "bare-ctor-stmt",
            "fn main() { A(R { id: 41 }); println(\"end\") }\n",
            "drop E\ndrop R41\nend\n",
        ),
        (
            "call-source-stmt-control",
            "fn mk() -> E { E.A(R { id: 41 }) }\n\
                 fn main() { mk(); println(\"end\") }\n",
            "drop E\ndrop R41\nend\n",
        ),
        (
            "bare-unit-variant-stmt",
            "fn main() { B; println(\"end\") }\n",
            "drop E\nend\n",
        ),
    ] {
        let prog = format!("{H}{body}");
        assert_eq!(run_program(&prog).as_deref(), Some(want), "{label}");
    }
    // An enum whose variants are ALL payloadless — the same fix, with no
    // payload machinery anywhere in the type.
    assert_eq!(
        run_program(
            "enum S { X, Y }\n\
                 impl Drop for S { fn drop(mut ref self) { println(\"drop S\") } }\n\
                 fn main() { let _ = S.X; println(\"end\") }\n"
        )
        .as_deref(),
        Some("drop S\nend\n"),
        "unit-only-enum"
    );
}

/// B-2026-08-01-13 — owned enum ARG body ownership, the coherent
/// caller-retains rule: a fresh enum-ctor arg's payload body fires
/// exactly once, CALLER-side, at the arg's statement end — whether the
/// callee drops the enum whole (was: silent on both backends), matches
/// it (was: single by accident of two bugs cancelling — output
/// unchanged, channel flipped to the caller), or if-lets an identifier
/// arg (was: DOUBLE on both backends — the callee's arm channel fired
/// on top of the caller's NLL fire). An own-Drop enum fires its own
/// body then the payload walk. Twin of `tests/interpreter.rs`'s
/// `test_owned_enum_arg_payload_body_single_caller_fire`.
#[test]
fn e2e_owned_enum_arg_payload_body_single_caller_fire() {
    let Some(out) = run_program(
        "struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"drop {self.id} {self.name}\")\n\
             \x20   }\n\
             }\n\
             enum Loud { Hold(Res), Quiet }\n\
             impl Drop for Loud {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(\"loud drop\")\n\
             \x20   }\n\
             }\n\
             enum E2 { B(Res), Empty }\n\
             fn check(w: E2) { println(\"checked\"); }\n\
             fn check_match(w: E2) {\n\
             \x20   match w {\n\
             \x20       E2.B(r2) => { println(f\"got {r2.id}\"); }\n\
             \x20       E2.Empty => { println(\"none\"); }\n\
             \x20   }\n\
             \x20   println(\"match done\");\n\
             }\n\
             fn check_iflet(w: E2) {\n\
             \x20   if let E2.B(r2) = w {\n\
             \x20       println(f\"if {r2.id}\");\n\
             \x20   }\n\
             \x20   println(\"iflet done\");\n\
             }\n\
             fn check_loud(w: Loud) { println(\"loudcheck\"); }\n\
             fn mk(n: i64) -> E2 { return E2.B(Res { id: n, name: f\"x{n}\" }); }\n\
             fn main() {\n\
             \x20   println(\"a\");\n\
             \x20   check(E2.B(Res { id: 55, name: f\"x{55}\" }));\n\
             \x20   println(\"b\");\n\
             \x20   check_loud(Loud.Hold(Res { id: 9, name: f\"l{9}\" }));\n\
             \x20   println(\"c\");\n\
             \x20   let e = mk(53);\n\
             \x20   check_match(e);\n\
             \x20   println(\"d\");\n\
             \x20   check_iflet(E2.B(Res { id: 6, name: f\"x{6}\" }));\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(
        out,
        "a\nchecked\ndrop 55 x55\nb\nloudcheck\nloud drop\ndrop 9 l9\nc\ngot 53\n\
             match done\ndrop 53 x53\nd\nif 6\niflet done\ndrop 6 x6\nend\n"
    );
}

/// B-2026-09-20-10 — the INTERPRETER, not the compiled backends, lost a
/// discarded seeded-envelope value's payload `Drop` body.
///
/// ```text
/// let _ = Some(W1 { v: 41 });   // --interp printed NOTHING; all three compiled surfaces ran the body
/// ```
///
/// Filed as an `Array`-payload question and measured here as EVERY payload
/// shape, because the miss is at the GATE and not in the walk:
/// `discard_rhs_produces_owned_value`'s bare-constructor arm asks
/// `find_enum_for_variant`, which cannot see `Some` / `Ok` / `Err` —
/// `Option` and `Result` have no source `EnumDef`. The gate answered false
/// and `run_discarded_value_user_drops` was never called, whatever the
/// payload was.
///
/// WHY THE INTERPRETER IS THE WRONG SIDE HERE, which matters because every
/// fixture in this family is written as an A/B against it: the identical
/// discard over a USER-DECLARED enum is correct on all four surfaces, and it
/// is correct precisely because it reaches that arm through
/// `find_enum_for_variant`. The declared spelling is the control, and it is
/// in the same program as the seeded ones below so a tree difference cannot
/// explain the split.
#[test]
fn e2e_discarded_seeded_envelope_runs_its_payload_bodies_on_every_surface() {
    const W: &str = "struct W1 { v: i64 }\n\
             impl Drop for W1 { fn drop(mut ref self) { println(f\"dW1_{self.v}\") } }\n";
    // Five payload shapes and the bare-array control, in ONE program: a
    // per-cell grid cannot show that the shapes behave alike, and the
    // interpreter lost all five for one reason.
    let prog = format!(
        "{W}fn main() {{\n\
               let _ = [W1 {{ v: 40 }}];\n\
               println(\"m1\");\n\
               let _ = Some(W1 {{ v: 41 }});\n\
               println(\"m2\");\n\
               let _ = Some([W1 {{ v: 42 }}]);\n\
               println(\"m3\");\n\
               let _ = Some(vec![W1 {{ v: 43 }}]);\n\
               println(\"m4\");\n\
               let _ = Some((W1 {{ v: 44 }}, 1));\n\
               println(\"end\")\n\
             }}\n"
    );
    let want = "dW1_40\nm1\ndW1_41\nm2\ndW1_42\nm3\ndW1_43\nm4\ndW1_44\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(&prog);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    if let Some(aot) = run_program(&prog) {
        assert_eq!(aot, want, "AOT");
    }

    // The user-declared CONTROL beside the seeded pair, both provenances.
    // Correct on all four surfaces before and after, which is what says the
    // gate and not the walk was missing.
    let ctl = format!(
        "{W}enum E {{ Y(Array[W1, 1]), N }}\n\
             fn main() {{\n\
               let _ = Some([W1 {{ v: 40 }}]);\n\
               println(\"m1\");\n\
               let a: Array[W1, 1] = [W1 {{ v: 41 }}];\n\
               let _ = Some(a);\n\
               println(\"m2\");\n\
               let _ = E.Y([W1 {{ v: 42 }}]);\n\
               println(\"m3\");\n\
               let b: Array[W1, 1] = [W1 {{ v: 43 }}];\n\
               let _ = E.Y(b);\n\
               println(\"end\")\n\
             }}\n"
    );
    let ctl_want = "dW1_40\nm1\ndW1_41\nm2\ndW1_42\nm3\ndW1_43\nend\n";
    let (ctl_out, ctl_errs, _, _) = karac::run_program_full_checked(&ctl);
    assert!(ctl_errs.is_empty(), "control interp errored: {ctl_errs:?}");
    assert_eq!(ctl_out.join(""), ctl_want, "control interpreter");
    if let Some(aot) = run_program(&ctl) {
        assert_eq!(aot, ctl_want, "control AOT");
    }
}

/// B-2026-09-20-15 — a DISCARDED generic enum whose monomorph's payload is
/// heap-BOXED had no owner at all: the box leaked and the payload's `Drop`
/// body never ran.
///
/// ```text
/// let _ = Gen.Y(R { id: 47, s: f"payload-a" });   // interp: dR47   compiled: (nothing)
/// ```
///
/// TWO independent omissions at one site, which is why the fixture asserts
/// output here and a `memory_sanitizer` twin asserts the free. A generic
/// enum declares its payload as `T`, so `payload_word_count_for_type_expr`
/// sizes the payload area at ONE word and any wider monomorph is heap-boxed.
/// At the discard site that made BOTH owners decline: `heap_payload` is
/// name-keyed off the erased `T` and so answered false (nothing freed the
/// box), and the bodies fallback stood itself down on any variant that
/// boxes (nothing ran `R`'s body). Each half was measured alone — the
/// memory half made all four discard cells valgrind-clean with the body
/// still missing — so neither is redundant.
///
/// PAYLOAD WIDTH, NOT HEAP, IS THE DISCRIMINATOR, and the `dW49` cell is
/// what says so: `W` is two `i64`s with no heap anywhere in the program and
/// it boxed and leaked exactly like the `String`-bearing `R`. `dN46` is the
/// one-word monomorph that FITS the erased area, so it never boxed and was
/// correct before the fix — the control that keeps the repair from being
/// read as "generic enums now walk their payloads".
///
/// The last two blocks are the PROTECTED positions: `Mix[R].N(63)` is the
/// non-payload variant of a boxing enum (nothing to free, and a walker that
/// ran would double-free), and `eat(Gen.Y(…))` is the ARGUMENT position,
/// which has an owner already — the callee's. Both printed correctly at
/// base and must still print exactly once, which is the half of this fix
/// that a leak-only measurement cannot see.
#[test]
fn e2e_discarded_boxed_generic_enum_payload_runs_its_drop_body() {
    // One program, seven scoped blocks: the repair and both protected
    // positions have to be seen in a single frame, because the defect is a
    // decision made per call site.
    let prog = "struct R { id: i64, s: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             struct W { id: i64, n2: i64 }\n\
             impl Drop for W { fn drop(mut ref self) { println(f\"dW{self.id}\") } }\n\
             struct N { id: i64 }\n\
             impl Drop for N { fn drop(mut ref self) { println(f\"dN{self.id}\") } }\n\
             enum Gen[T] { Y(T), Z }\n\
             enum Mix[T] { W(T), N(i64) }\n\
             fn mkR(i: i64) -> R { return R { id: i, s: f\"p{i}\" }; }\n\
             fn eat(g: Gen[R]) { println(\"ate\") }\n\
             fn main() {\n\
                 { let _ = Gen.Y(R { id: 47, s: f\"payload-a\" }); }\n\
                 { let _ = Gen.Y(mkR(48)); }\n\
                 { let _ = Gen.Y(W { id: 49, n2: 1 }); }\n\
                 { let _ = Gen.Y(N { id: 46 }); }\n\
                 { let _ = Mix.W(R { id: 62, s: f\"payload-m\" }); }\n\
                 { let _ = Mix[R].N(63); }\n\
                 { eat(Gen.Y(R { id: 64, s: f\"payload-e\" })); }\n\
                 println(\"end\")\n\
             }\n";
    let want = "dR47\ndR48\ndW49\ndN46\ndR62\nate\ndR64\nend\n";

    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(prog);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    if let Some(aot) = run_program(prog) {
        assert_eq!(aot, want, "AOT");
    }
}

/// B-2026-09-17-31 — A METHOD CALL KEEPS AN UNMOVED PAYLOAD PART'S `Drop`
/// BODY, WHERE THE FREE-FUNCTION SPELLING OF THE SAME BODY ALWAYS DID.
///
/// `h.eat(Some((R { id: 5 }, R { id: 6 })))` over
/// `fn eat(ref self, o: Option[(R, R)]) -> R { match o { Some(t) => { return t.0; } .. } }`
/// printed `got:5 dR5 end` under `--interp` against the three compiled
/// surfaces' correct `dR6 got:5 dR5 end`. Element 1 is never moved out and
/// nothing else can own it, so exactly one body is owed; the interpreter
/// ran zero. The identical FREE function was correct on all four.
///
/// THE ROW'S DIRECTION HAD FLIPPED BY THE TIME IT WAS FIXED. It was filed
/// as an AGREED loss on all four surfaces -- invisible to the kata A/B
/// rule, which is the only automatic check on this class -- and
/// B-2026-09-17-30's fix moved the three compiled surfaces to correct
/// without moving the interpreter, turning it into an ordinary run-vs-build
/// divergence. Re-rated at close; the fix site moved from codegen to the
/// interpreter with it.
///
/// MECHANISM: `run_fresh_temp_arg_drops` stands a whole argument down when
/// `callee_owns_arg_beyond_call` says the callee keeps it past the call,
/// and B-2026-09-03-7 gave the METHOD path an extra, TYPE-LEVEL disjunct:
/// a return type that could carry the argument out licenses the
/// stand-down, because the structural walks miss a constructor wrap
/// (`return Option.Some(r)`). `-> R` is such a type, so the whole argument
/// stood down and the `continue` it triggers sits AHEAD of the part-precise
/// walk, leaving nothing to mask. B-2026-09-05-33 had already carved a bare
/// TUPLE parameter out of that test for the identical reason one level out;
/// the repair carves out an `Option`/`Result` parameter too. The free path
/// is the existence proof that nothing is lost by it: that path never
/// consults the type-level test, reaches its answer structurally plus
/// `mask_optres_payload_escaping_parts`, and is correct for both payload
/// shapes.
///
/// THE CELLS ARE THE ROW'S OWN "NOT MEASURED" LIST and every one was
/// wrong: the OWNED-`self` receiver, the `Result` head, and the
/// struct-payload spelling the row named as its boundary. `second` and
/// `destr` are two more arm spellings through the same gate.
///
/// THE THREE CONTROLS ARE BYTE-IDENTICAL BEFORE AND AFTER. `nomove` moves
/// nothing, so the gate must never be reached. `whole` returns the
/// argument ITSELF, which is the shape the stand-down exists for -- the
/// structural `fn_returns_param` still catches it, and both bodies stay
/// with the result. `assoc` is an associated function, whose caller-side
/// walk was already correct.
///
/// BODY-ONLY, so no sanitizer leg sees it.
///
/// The INTERPRETER twin is `tests/interpreter.rs`'s
/// `test_method_call_keeps_an_unmoved_payload_parts_drop_body`, the half
/// that actually moved.
#[test]
fn e2e_method_call_keeps_an_unmoved_payload_parts_drop_body() {
    const R: &str = "struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n";
    const ARG: &str = "Some((R { id: 5 }, R { id: 6 }))";
    // (label, source, expectation -- both backends, all four surfaces)
    for (label, prog, want) in [
            (
                "the row's cell: ref self, Option head",
                format!(
                    "{R}struct H {{ n: i64 }}\n\
                     impl H {{ fn eat(ref self, o: Option[(R, R)]) -> R {{ match o {{ Some(t) => {{ return t.0; }} None => {{ return R {{ id: 0 }}; }} }} }} }}\n\
                     fn main() {{ let h = H {{ n: 1 }}; let g = h.eat({ARG}); println(f\"got:{{g.id}}\"); println(\"end\") }}\n"
                ),
                "dR6\ngot:5\ndR5\nend\n",
            ),
            (
                "owned self receiver",
                format!(
                    "{R}struct H {{ n: i64 }}\n\
                     impl H {{ fn eat(self, o: Option[(R, R)]) -> R {{ match o {{ Some(t) => {{ return t.0; }} None => {{ return R {{ id: 0 }}; }} }} }} }}\n\
                     fn main() {{ let h = H {{ n: 1 }}; let g = h.eat({ARG}); println(f\"got:{{g.id}}\"); println(\"end\") }}\n"
                ),
                "dR6\ngot:5\ndR5\nend\n",
            ),
            (
                "Result head",
                format!(
                    "{R}struct H {{ n: i64 }}\n\
                     impl H {{ fn eat(ref self, o: Result[(R, R), i64]) -> R {{ match o {{ Ok(t) => {{ return t.0; }} Err(e) => {{ return R {{ id: 0 }}; }} }} }} }}\n\
                     fn main() {{ let h = H {{ n: 1 }}; let g = h.eat(Result.Ok((R {{ id: 5 }}, R {{ id: 6 }}))); println(f\"got:{{g.id}}\"); println(\"end\") }}\n"
                ),
                "dR6\ngot:5\ndR5\nend\n",
            ),
            (
                "the handed-out part is index 1, so the lost one was index 0",
                format!(
                    "{R}struct H {{ n: i64 }}\n\
                     impl H {{ fn eat(ref self, o: Option[(R, R)]) -> R {{ match o {{ Some(t) => {{ return t.1; }} None => {{ return R {{ id: 0 }}; }} }} }} }}\n\
                     fn main() {{ let h = H {{ n: 1 }}; let g = h.eat({ARG}); println(f\"got:{{g.id}}\"); println(\"end\") }}\n"
                ),
                "dR5\ngot:6\ndR6\nend\n",
            ),
            (
                "named-STRUCT payload -- the row's own boundary case",
                format!(
                    "{R}struct Q {{ r: R, s: R }}\n\
                     struct H {{ n: i64 }}\n\
                     impl H {{ fn eat(ref self, o: Option[Q]) -> R {{ match o {{ Some(t) => {{ return t.r; }} None => {{ return R {{ id: 0 }}; }} }} }} }}\n\
                     fn main() {{ let h = H {{ n: 1 }}; let g = h.eat(Some(Q {{ r: R {{ id: 5 }}, s: R {{ id: 6 }} }})); println(f\"got:{{g.id}}\"); println(\"end\") }}\n"
                ),
                "dR6\ngot:5\ndR5\nend\n",
            ),
            (
                "the DESTRUCTURING arm spelling of the same hand-out",
                format!(
                    "{R}struct H {{ n: i64 }}\n\
                     impl H {{ fn eat(ref self, o: Option[(R, R)]) -> R {{ match o {{ Some((a, b)) => {{ return a; }} None => {{ return R {{ id: 0 }}; }} }} }} }}\n\
                     fn main() {{ let h = H {{ n: 1 }}; let g = h.eat({ARG}); println(f\"got:{{g.id}}\"); println(\"end\") }}\n"
                ),
                "dR6\ngot:5\ndR5\nend\n",
            ),
            (
                "control: the arm moves NOTHING, so the gate is never reached",
                format!(
                    "{R}struct H {{ n: i64 }}\n\
                     impl H {{ fn eat(ref self, o: Option[(R, R)]) {{ match o {{ Some(t) => {{ println(\"mid\"); }} None => {{ println(\"n\"); }} }} }} }}\n\
                     fn main() {{ let h = H {{ n: 1 }}; h.eat({ARG}); println(\"end\") }}\n"
                ),
                "mid\ndR5\ndR6\nend\n",
            ),
            (
                "control: the callee returns the ARGUMENT, the shape the stand-down is for",
                format!(
                    "{R}struct H {{ n: i64 }}\n\
                     impl H {{ fn eat(ref self, o: Option[(R, R)]) -> Option[(R, R)] {{ return o; }} }}\n\
                     fn main() {{ let h = H {{ n: 1 }}; let g = h.eat({ARG}); println(\"kept\"); println(\"end\") }}\n"
                ),
                "dR5\ndR6\nkept\nend\n",
            ),
            (
                "control: an ASSOCIATED function was already correct",
                format!(
                    "{R}struct A {{}}\n\
                     impl A {{ fn eat(o: Option[(R, R)]) -> R {{ match o {{ Some(t) => {{ return t.0; }} None => {{ return R {{ id: 0 }}; }} }} }} }}\n\
                     fn main() {{ let g = A.eat({ARG}); println(f\"got:{{g.id}}\"); println(\"end\") }}\n"
                ),
                "dR6\ngot:5\ndR5\nend\n",
            ),
            (
                "use site: the result is DISCARDED",
                format!(
                    "{R}struct H {{ n: i64 }}\n\
                     impl H {{ fn eat(ref self, o: Option[(R, R)]) -> R {{ match o {{ Some(t) => {{ return t.0; }} None => {{ return R {{ id: 0 }}; }} }} }} }}\n\
                     fn main() {{ let h = H {{ n: 1 }}; h.eat({ARG}); println(\"after\"); println(\"end\") }}\n"
                ),
                "dR6\ndR5\nafter\nend\n",
            ),
            (
                "use site: the result is stored in a STRUCT FIELD",
                format!(
                    "{R}struct B {{ v: R }}\n\
                     struct H {{ n: i64 }}\n\
                     impl H {{ fn eat(ref self, o: Option[(R, R)]) -> R {{ match o {{ Some(t) => {{ return t.0; }} None => {{ return R {{ id: 0 }}; }} }} }} }}\n\
                     fn main() {{ let h = H {{ n: 1 }}; let b = B {{ v: h.eat({ARG}) }}; println(f\"in:{{b.v.id}}\"); println(\"end\") }}\n"
                ),
                "dR6\nin:5\ndR5\nend\n",
            ),
            (
                "use site: the call is in a LOOP, so one sibling body per iteration",
                format!(
                    "{R}struct H {{ n: i64 }}\n\
                     impl H {{ fn eat(ref self, o: Option[(R, R)]) -> R {{ match o {{ Some(t) => {{ return t.0; }} None => {{ return R {{ id: 0 }}; }} }} }} }}\n\
                     fn main() {{ let h = H {{ n: 1 }}; let mut i = 0i64; while i < 2 {{ let g = h.eat({ARG}); println(f\"it:{{g.id}}\"); i = i + 1; }} println(\"end\") }}\n"
                ),
                "dR6\nit:5\ndR5\ndR6\nit:5\ndR5\nend\n",
            ),
            (
                // WAS PINNED at the wrong answer (`dR6 sank:5 end`) as
                // B-2026-09-19-56: the method result handed straight to a
                // consuming free function ran `sink`'s own by-value param body
                // NOWHERE, on all four surfaces, while `sink(R { .. })`,
                // `sink(mk(2))` and `sink(<named local>)` all ran it.
                //
                // FIXED, and the trigger was wider than the pin implied —
                // nothing about the payload was involved, only the fact that
                // an instance method was not one of the producer shapes either
                // backend enumerates. See
                // `e2e_method_call_result_argument_runs_the_callees_param_drop_body`,
                // which carries the cell that settles it (a method minting a
                // fresh value with no `Option` anywhere) and the mechanism.
                "a method result consumed by a free fn runs THAT fn's param body",
                format!(
                    "{R}struct H {{ n: i64 }}\n\
                     impl H {{ fn eat(ref self, o: Option[(R, R)]) -> R {{ match o {{ Some(t) => {{ return t.0; }} None => {{ return R {{ id: 0 }}; }} }} }} }}\n\
                     fn sink(r: R) {{ println(f\"sank:{{r.id}}\") }}\n\
                     fn main() {{ let h = H {{ n: 1 }}; sink(h.eat({ARG})); println(\"end\") }}\n"
                ),
                "dR6\nsank:5\ndR5\nend\n",
            ),
            (
                "control: the FREE function, correct throughout",
                format!(
                    "{R}fn eat(o: Option[(R, R)]) -> R {{ match o {{ Some(t) => {{ return t.0; }} None => {{ return R {{ id: 0 }}; }} }} }}\n\
                     fn main() {{ let g = eat({ARG}); println(f\"got:{{g.id}}\"); println(\"end\") }}\n"
                ),
                "dR6\ngot:5\ndR5\nend\n",
            ),
        ] {
            let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(&prog);
            assert!(
                interp_errs.is_empty(),
                "[{label}] interp errored: {interp_errs:?}"
            );
            assert_eq!(interp_out.join(""), want, "[{label}] interpreter");
            if let Some(aot) = run_program(&prog) {
                assert_eq!(aot, want, "[{label}] AOT");
            }
        }
}

/// B-2026-09-19-55 — TWO INHERENT IMPLS DEFINING AN ASSOCIATED FUNCTION OF
/// ONE NAME MADE THE INTERPRETER RUN A HANDED-OUT PAYLOAD PART'S `Drop`
/// BODY TWICE.
///
/// `A.dup(Some(Q { r: R { id: 5 }, s: R { id: 6 } }))` over
/// `fn dup(o: Option[Q]) -> R { match o { Some(t) => { return t.r; } .. } }`
/// printed `dR6 dR6 got:5 dR5` on `--interp` against `dR6 got:5 dR5` on
/// jit / `karac build` / `KARAC_AUTO_PAR=0`. Renaming EITHER function made
/// it correct, and `h.dup(..)` — the instance-method spelling of the
/// identical body — was correct throughout.
///
/// MECHANISM, and it is a resolution failure rather than a walk defect:
/// `register_impl_methods` binds an impl method under the qualified env key
/// `A.dup` but stores the BARE name in the `Value::Function` it binds, so
/// the dispatch arm has only `dup` to resolve with. Both bare-name callee
/// resolvers fail CLOSED on two candidates (`callee_fn_by_bare_name`
/// returns `None` the moment it finds a second), and a `None` there does
/// not read to its consumers as "unknown" — it reads as "this callee owns
/// nothing". The one that bites is frame-entry seeding
/// (`owned_param_names_of_fn`): with an empty owned-param set the callee's
/// arm-bound payload is never marked a view of the entry copy, so the
/// CALLEE ran the surviving field's body on top of the caller's own walk.
/// B-2026-09-12-15 recorded the same mechanism for the empty set an
/// associated callee used to get unconditionally.
///
/// The fix carries the impl target through the dispatch arm
/// (`CalleeOwner::Assoc`), so both resolvers reach `impl_method_ast` and
/// answer exactly. `CalleeOwner` distinguishes the assoc and instance
/// spellings because B-2026-09-03-7's type-level return stand-down is a
/// question about the RECEIVER, which an associated function has none of.
///
/// THE CELLS VARY THE USE SITE, not only the payload: `discard`, `field`,
/// `sink` and `loop` are the shapes a caller-side walk is most easily
/// wrong at, and all four moved. `peer` proves the resolution is EXACT
/// rather than merely unblocked — `B.dup` hands out the OTHER field, and
/// its own sibling is the one that must survive.
///
/// THE CONTROLS ARE BYTE-IDENTICAL BEFORE AND AFTER: a unique assoc name,
/// the free function, a free function that SHADOWS the duplicated name
/// (the resolvers try free functions first, so it must still win), the
/// callee returning the whole argument, and the tuple-payload `nomove`.
///
/// BODY-ONLY, so no sanitizer leg sees it.
///
/// The INTERPRETER twin is `tests/interpreter.rs`'s
/// `test_duplicate_assoc_fn_name_keeps_one_payload_part_drop_body`, the
/// half that actually moved.
#[test]
fn e2e_duplicate_assoc_fn_name_keeps_one_payload_part_drop_body() {
    const R: &str = "struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n";
    const Q: &str = "struct Q { r: R, s: R }\n";
    const ARG: &str = "Some(Q { r: R { id: 5 }, s: R { id: 6 } })";
    // Two inherent impls, one method name -- the collision itself.
    const DUP2: &str = "struct A {}\nstruct B {}\n\
             impl A { fn dup(o: Option[Q]) -> R { match o { Some(t) => { return t.r; } None => { return R { id: 0 }; } } } }\n\
             impl B { fn dup(o: Option[Q]) -> R { match o { Some(t) => { return t.r; } None => { return R { id: 0 }; } } } }\n";
    // (label, source, interpreter expectation, compiled expectation)
    for (label, prog, want_interp, want_aot) in [
            (
                "the row's cell: two impls define `dup`, struct payload",
                format!("{R}{Q}{DUP2}fn main() {{ let g = A.dup({ARG}); println(f\"got:{{g.id}}\"); println(\"end\") }}\n"),
                "dR6\ngot:5\ndR5\nend\n",
                "dR6\ngot:5\ndR5\nend\n",
            ),
            (
                "THREE impls share the name",
                format!(
                    "{R}{Q}struct A {{}}\nstruct B {{}}\nstruct C {{}}\n\
                     impl A {{ fn dup(o: Option[Q]) -> R {{ match o {{ Some(t) => {{ return t.r; }} None => {{ return R {{ id: 0 }}; }} }} }} }}\n\
                     impl B {{ fn dup(o: Option[Q]) -> R {{ match o {{ Some(t) => {{ return t.r; }} None => {{ return R {{ id: 0 }}; }} }} }} }}\n\
                     impl C {{ fn dup(o: Option[Q]) -> R {{ match o {{ Some(t) => {{ return t.r; }} None => {{ return R {{ id: 0 }}; }} }} }} }}\n\
                     fn main() {{ let g = C.dup({ARG}); println(f\"got:{{g.id}}\"); println(\"end\") }}\n"
                ),
                "dR6\ngot:5\ndR5\nend\n",
                "dR6\ngot:5\ndR5\nend\n",
            ),
            (
                // Resolution is EXACT, not merely unblocked: the two bodies
                // hand out DIFFERENT fields, so picking the wrong one would
                // keep the wrong sibling alive and be visible here.
                "the PEER impl hands out the other field",
                format!(
                    "{R}{Q}struct A {{}}\nstruct B {{}}\n\
                     impl A {{ fn dup(o: Option[Q]) -> R {{ match o {{ Some(t) => {{ return t.r; }} None => {{ return R {{ id: 0 }}; }} }} }} }}\n\
                     impl B {{ fn dup(o: Option[Q]) -> R {{ match o {{ Some(t) => {{ return t.s; }} None => {{ return R {{ id: 0 }}; }} }} }} }}\n\
                     fn main() {{ let g = B.dup({ARG}); println(f\"got:{{g.id}}\"); println(\"end\") }}\n"
                ),
                "dR5\ngot:6\ndR6\nend\n",
                "dR5\ngot:6\ndR6\nend\n",
            ),
            (
                "TUPLE payload, the other part-classified shape",
                format!(
                    "{R}struct A {{}}\nstruct B {{}}\n\
                     impl A {{ fn dup(o: Option[(R, R)]) -> R {{ match o {{ Some(t) => {{ return t.0; }} None => {{ return R {{ id: 0 }}; }} }} }} }}\n\
                     impl B {{ fn dup(o: Option[(R, R)]) -> R {{ match o {{ Some(t) => {{ return t.0; }} None => {{ return R {{ id: 0 }}; }} }} }} }}\n\
                     fn main() {{ let g = A.dup(Some((R {{ id: 5 }}, R {{ id: 6 }}))); println(f\"got:{{g.id}}\"); println(\"end\") }}\n"
                ),
                "dR6\ngot:5\ndR5\nend\n",
                "dR6\ngot:5\ndR5\nend\n",
            ),
            (
                "`Result` head",
                format!(
                    "{R}{Q}struct A {{}}\nstruct B {{}}\n\
                     impl A {{ fn dup(o: Result[Q, i64]) -> R {{ match o {{ Ok(t) => {{ return t.r; }} Err(e) => {{ return R {{ id: 0 }}; }} }} }} }}\n\
                     impl B {{ fn dup(o: Result[Q, i64]) -> R {{ match o {{ Ok(t) => {{ return t.r; }} Err(e) => {{ return R {{ id: 0 }}; }} }} }} }}\n\
                     fn main() {{ let g = A.dup(Result.Ok(Q {{ r: R {{ id: 5 }}, s: R {{ id: 6 }} }})); println(f\"got:{{g.id}}\"); println(\"end\") }}\n"
                ),
                "dR6\ngot:5\ndR5\nend\n",
                "dR6\ngot:5\ndR5\nend\n",
            ),
            (
                "the DESTRUCTURING arm spelling",
                format!(
                    "{R}struct A {{}}\nstruct B {{}}\n\
                     impl A {{ fn dup(o: Option[(R, R)]) -> R {{ match o {{ Some((a, b)) => {{ return a; }} None => {{ return R {{ id: 0 }}; }} }} }} }}\n\
                     impl B {{ fn dup(o: Option[(R, R)]) -> R {{ match o {{ Some((a, b)) => {{ return a; }} None => {{ return R {{ id: 0 }}; }} }} }} }}\n\
                     fn main() {{ let g = A.dup(Some((R {{ id: 5 }}, R {{ id: 6 }}))); println(f\"got:{{g.id}}\"); println(\"end\") }}\n"
                ),
                "dR6\ngot:5\ndR5\nend\n",
                "dR6\ngot:5\ndR5\nend\n",
            ),
            (
                "use site: the result is DISCARDED",
                format!("{R}{Q}{DUP2}fn main() {{ A.dup({ARG}); println(\"after\"); println(\"end\") }}\n"),
                "dR6\ndR5\nafter\nend\n",
                "dR6\ndR5\nafter\nend\n",
            ),
            (
                "use site: the result is stored in a STRUCT FIELD",
                format!(
                    "{R}{Q}struct Bx {{ v: R }}\n{DUP2}\
                     fn main() {{ let b = Bx {{ v: A.dup({ARG}) }}; println(f\"in:{{b.v.id}}\"); println(\"end\") }}\n"
                ),
                "dR6\nin:5\ndR5\nend\n",
                "dR6\nin:5\ndR5\nend\n",
            ),
            (
                "use site: the result is handed to a BY-VALUE callee",
                format!(
                    "{R}{Q}{DUP2}fn sink(r: R) {{ println(f\"sank:{{r.id}}\") }}\n\
                     fn main() {{ sink(A.dup({ARG})); println(\"end\") }}\n"
                ),
                "dR6\nsank:5\ndR5\nend\n",
                "dR6\nsank:5\ndR5\nend\n",
            ),
            (
                "use site: the call is in a LOOP, so one sibling body per iteration",
                format!(
                    "{R}{Q}{DUP2}\
                     fn main() {{ let mut i = 0; while i < 2 {{ let g = A.dup({ARG}); println(f\"it:{{g.id}}\"); i = i + 1; }} println(\"end\") }}\n"
                ),
                "dR6\nit:5\ndR5\ndR6\nit:5\ndR5\nend\n",
                "dR6\nit:5\ndR5\ndR6\nit:5\ndR5\nend\n",
            ),
            (
                // The reduction's original shape: the collision is between an
                // associated function and an INSTANCE method. The bare-name
                // guard scan admits methods, so this defeated it too -- but
                // the ownership scan drops them, which is why this spelling
                // needed BOTH resolvers carrying the owner, not just one.
                "the name is shared with an INSTANCE method",
                format!(
                    "{R}{Q}struct A {{}}\nstruct H {{ n: i64 }}\n\
                     impl H {{ fn dup(ref self, o: Option[Q]) -> R {{ match o {{ Some(t) => {{ return t.r; }} None => {{ return R {{ id: 0 }}; }} }} }} }}\n\
                     impl A {{ fn dup(o: Option[Q]) -> R {{ match o {{ Some(t) => {{ return t.r; }} None => {{ return R {{ id: 0 }}; }} }} }} }}\n\
                     fn main() {{ let g = A.dup({ARG}); println(f\"got:{{g.id}}\"); println(\"end\") }}\n"
                ),
                "dR6\ngot:5\ndR5\nend\n",
                "dR6\ngot:5\ndR5\nend\n",
            ),
            (
                "the METHOD spelling of that same pair, correct throughout",
                format!(
                    "{R}{Q}struct A {{}}\nstruct H {{ n: i64 }}\n\
                     impl H {{ fn dup(ref self, o: Option[Q]) -> R {{ match o {{ Some(t) => {{ return t.r; }} None => {{ return R {{ id: 0 }}; }} }} }} }}\n\
                     impl A {{ fn dup(o: Option[Q]) -> R {{ match o {{ Some(t) => {{ return t.r; }} None => {{ return R {{ id: 0 }}; }} }} }} }}\n\
                     fn main() {{ let h = H {{ n: 1 }}; let g = h.dup({ARG}); println(f\"got:{{g.id}}\"); println(\"end\") }}\n"
                ),
                "dR6\ngot:5\ndR5\nend\n",
                "dR6\ngot:5\ndR5\nend\n",
            ),
            (
                "control: a UNIQUE associated name was already correct",
                format!(
                    "{R}{Q}struct Z {{}}\n\
                     impl Z {{ fn zdup(o: Option[Q]) -> R {{ match o {{ Some(t) => {{ return t.r; }} None => {{ return R {{ id: 0 }}; }} }} }} }}\n\
                     fn main() {{ let g = Z.zdup({ARG}); println(f\"got:{{g.id}}\"); println(\"end\") }}\n"
                ),
                "dR6\ngot:5\ndR5\nend\n",
                "dR6\ngot:5\ndR5\nend\n",
            ),
            (
                "control: the FREE function, correct throughout",
                format!(
                    "{R}{Q}fn dup(o: Option[Q]) -> R {{ match o {{ Some(t) => {{ return t.r; }} None => {{ return R {{ id: 0 }}; }} }} }}\n\
                     fn main() {{ let g = dup({ARG}); println(f\"got:{{g.id}}\"); println(\"end\") }}\n"
                ),
                "dR6\ngot:5\ndR5\nend\n",
                "dR6\ngot:5\ndR5\nend\n",
            ),
            (
                // Both resolvers try free functions FIRST. A free `dup`
                // alongside the two impls must still be the one a bare call
                // resolves to, which the owner channel must not disturb.
                "control: a FREE function shadowing the duplicated name still wins",
                format!(
                    "{R}{Q}fn dup(o: Option[Q]) -> R {{ match o {{ Some(t) => {{ return t.r; }} None => {{ return R {{ id: 0 }}; }} }} }}\n\
                     {DUP2}fn main() {{ let g = dup({ARG}); println(f\"got:{{g.id}}\"); println(\"end\") }}\n"
                ),
                "dR6\ngot:5\ndR5\nend\n",
                "dR6\ngot:5\ndR5\nend\n",
            ),
            (
                "control: the callee returns the WHOLE argument",
                format!(
                    "{R}{Q}struct A {{}}\nstruct B {{}}\n\
                     impl A {{ fn dup(o: Option[Q]) -> Option[Q] {{ return o; }} }}\n\
                     impl B {{ fn dup(o: Option[Q]) -> Option[Q] {{ return o; }} }}\n\
                     fn main() {{ let g = A.dup({ARG}); println(\"kept\"); println(\"end\") }}\n"
                ),
                "dR6\ndR5\nkept\nend\n",
                "dR6\ndR5\nkept\nend\n",
            ),
            (
                "control: the arm moves NOTHING (tuple payload)",
                format!(
                    "{R}struct A {{}}\nstruct B {{}}\n\
                     impl A {{ fn dup(o: Option[(R, R)]) {{ match o {{ Some(t) => {{ println(\"mid\"); }} None => {{ println(\"n\"); }} }} }} }}\n\
                     impl B {{ fn dup(o: Option[(R, R)]) {{ match o {{ Some(t) => {{ println(\"mid\"); }} None => {{ println(\"n\"); }} }} }} }}\n\
                     fn main() {{ A.dup(Some((R {{ id: 5 }}, R {{ id: 6 }}))); println(\"end\") }}\n"
                ),
                "mid\ndR5\ndR6\nend\n",
                "mid\ndR5\ndR6\nend\n",
            ),
            (
                // WAS A PINNED DIVERGENCE, and it was not this row's.
                // B-2026-09-19-48: an `Option[<named struct>]` argument whose
                // arm moves NOTHING ran BOTH fields' bodies twice on every
                // compiled backend. The cell below it is the same program with
                // a UNIQUE name and diverged identically -- so the name
                // collision was never what made it happen, which is why the
                // two were pinned side by side. What this row's fix changed is
                // that the interpreter used to be wrong here in the SAME
                // direction, so the duplicate-name spelling agreed by both
                // halves being wrong; it became correct, the compiled half's
                // own defect was exposed, and that defect is now fixed. The
                // note said "both cells fail together the day -48 lands", and
                // they did; the two expectations are now one string.
                "converged (B-2026-09-19-48): struct payload, arm moves nothing",
                format!(
                    "{R}{Q}struct A {{}}\nstruct B {{}}\n\
                     impl A {{ fn dup(o: Option[Q]) {{ match o {{ Some(t) => {{ println(\"mid\"); }} None => {{ println(\"n\"); }} }} }} }}\n\
                     impl B {{ fn dup(o: Option[Q]) {{ match o {{ Some(t) => {{ println(\"mid\"); }} None => {{ println(\"n\"); }} }} }} }}\n\
                     fn main() {{ A.dup({ARG}); println(\"end\") }}\n"
                ),
                "mid\ndR6\ndR5\nend\n",
                "mid\ndR6\ndR5\nend\n",
            ),
            (
                "converged (B-2026-09-19-48): the UNIQUE-name twin, same answer",
                format!(
                    "{R}{Q}struct Z {{}}\n\
                     impl Z {{ fn zdup(o: Option[Q]) {{ match o {{ Some(t) => {{ println(\"mid\"); }} None => {{ println(\"n\"); }} }} }} }}\n\
                     fn main() {{ Z.zdup({ARG}); println(\"end\") }}\n"
                ),
                "mid\ndR6\ndR5\nend\n",
                "mid\ndR6\ndR5\nend\n",
            ),
        ] {
            let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(&prog);
            assert!(
                interp_errs.is_empty(),
                "[{label}] interp errored: {interp_errs:?}"
            );
            assert_eq!(interp_out.join(""), want_interp, "[{label}] interpreter");
            if let Some(aot) = run_program(&prog) {
                assert_eq!(aot, want_aot, "[{label}] AOT");
            }
        }
}

/// B-2026-09-06-18 — a by-value `Drop` param wrapped in a USER enum
/// variant on some exits (`fn fslot(r: R, k: bool) -> Slot { if k {
/// return Slot.Empty; } return Slot.Held(r); }`) has one owner per path on
/// every surface: the callee's per-path flip when the value dies inside,
/// the caller's result binding when it comes back inside the variant.
/// `fn_conditionally_returns_param_bare` now recognises the constructor
/// through the program's enum declarations (`is_user_variant_ctor`), which
/// is what tells `Slot.Held(r)` apart from an associated function of the
/// same spelling; the constructor lowering's per-path payload retraction
/// (B-2026-08-31-46) already cleared the flag on the hand-back path.
/// Cells: a one-payload and a two-payload tuple variant, the struct
/// variant, a fresh payload on the other exit, the tail spelling, a
/// nested return on all three path combinations, an enum with its own
/// `impl Drop`, associated / method / named-argument spellings, each on
/// both `k` values. Interpreter twin:
/// `test_param_wrapped_in_returned_enum_variant_on_some_paths_has_one_owner`.
#[test]
fn e2e_param_wrapped_in_returned_enum_variant_on_some_paths_has_one_owner() {
    assert_eq!(
            run_program(
                r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
enum Slot { Held(R), Pair(R, i64), Boxed { r: R, n: i64 }, Empty }
enum Tagged { Held(R), Empty }
impl Drop for Tagged { fn drop(mut ref self) { println("dT") } }
struct H { n: i64 }
fn mk(i: i64) -> String { return f"pay-{i}-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"; }
fn mr(i: i64) -> R { return R { id: i, s: mk(i) }; }
fn show(x: Slot) { match x { Slot.Held(v) => println(f"C{v.id}"), Slot.Pair(v, n) => println(f"P{v.id}"), Slot.Boxed { r, n } => println(f"X{r.id}"), Slot.Empty => println("CE") } }
fn fslot(r: R, k: bool) -> Slot { if k { return Slot.Empty; } return Slot.Held(r); }
fn fpair(r: R, k: bool) -> Slot { if k { return Slot.Empty; } return Slot.Pair(r, 1); }
fn fboxed(r: R, k: bool) -> Slot { if k { return Slot.Empty; } return Slot.Boxed { r: r, n: 1 }; }
fn ffresh(r: R, k: bool) -> Slot { if k { return Slot.Held(mr(90)); } return Slot.Held(r); }
fn ftail(r: R, k: bool) -> Slot { if k { Slot.Empty } else { Slot.Held(r) } }
fn fnest(r: R, k: bool, j: bool) -> Slot { if k { if j { return Slot.Held(r); } } return Slot.Empty; }
fn ftag(r: R, k: bool) -> Tagged { if k { return Tagged.Empty; } return Tagged.Held(r); }
impl H {
    fn aslot(r: R, k: bool) -> Slot { if k { return Slot.Empty; } return Slot.Held(r); }
    fn mslot(ref self, r: R, k: bool) -> Slot { if k { return Slot.Empty; } return Slot.Held(r); }
}
fn main() {
    let h = H { n: 0 };
    println("st"); { let x = fslot(mr(1), true); show(x); }
    println("sf"); { let x = fslot(mr(2), false); show(x); }
    println("pt"); { let x = fpair(mr(3), true); show(x); }
    println("pf"); { let x = fpair(mr(4), false); show(x); }
    println("bt"); { let x = fboxed(mr(5), true); show(x); }
    println("bf"); { let x = fboxed(mr(6), false); show(x); }
    println("frt"); { let x = ffresh(mr(7), true); show(x); }
    println("frf"); { let x = ffresh(mr(8), false); show(x); }
    println("tlt"); { let x = ftail(mr(9), true); show(x); }
    println("tlf"); { let x = ftail(mr(10), false); show(x); }
    println("ntt"); { let x = fnest(mr(11), true, true); show(x); }
    println("ntf"); { let x = fnest(mr(12), true, false); show(x); }
    println("nff"); { let x = fnest(mr(13), false, false); show(x); }
    println("tgt"); { let x = ftag(mr(14), true); match x { Tagged.Held(v) => println(f"C{v.id}"), Tagged.Empty => println("CE") } }
    println("tgf"); { let x = ftag(mr(15), false); match x { Tagged.Held(v) => println(f"C{v.id}"), Tagged.Empty => println("CE") } }
    println("at"); { let x = H.aslot(mr(16), true); show(x); }
    println("af"); { let x = H.aslot(mr(17), false); show(x); }
    println("mt"); { let x = h.mslot(mr(18), true); show(x); }
    println("mf"); { let x = h.mslot(mr(19), false); show(x); }
    println("nt"); { let a = mr(20); let x = fslot(a, true); show(x); }
    println("nf"); { let b = mr(21); let x = fslot(b, false); show(x); }
    println("end");
}"#
            ),
            Some("st\nd1\nCE\nsf\nC2\nd2\npt\nd3\nCE\npf\nP4\nd4\nbt\nd5\nCE\nbf\nX6\nd6\nfrt\nd7\nC90\nd90\nfrf\nC8\nd8\ntlt\nd9\nCE\ntlf\nC10\nd10\nntt\nC11\nd11\nntf\nd12\nCE\nnff\nd13\nCE\ntgt\nd14\nCE\ndT\ntgf\nC15\ndT\nd15\nat\nd16\nCE\naf\nC17\nd17\nmt\nd18\nCE\nmf\nC19\nd19\nnt\nd20\nCE\nnf\nC21\nd21\nend\n".to_string()),
            "a param wrapped in a returned enum variant on some paths has one owner per path"
        );
}

/// B-2026-09-01-39 — a LIVE local handed out of a discarded branch, or
/// discarded directly (`let _ = e;`), runs its payload's `Drop` body
/// exactly once on every surface, reading the bound-local oracle
/// (`dE dR1`). Two halves: the interpreter's discard site now OWNS a taken
/// tail that names a live local (the `if` arm is a block whose tail record
/// had already masked the local's payload walk; the `match` arm is not, so
/// the two disagreed) and silences the local whole; codegen's general
/// `let` path no longer retracts the source's element-bodies walker for a
/// wildcard target, which has no destination to register it anew. Cells:
/// `if` and `match`, `let _` and bare-statement, both branch directions,
/// direct `let _ = e` and `e;`, an own-`Drop` enum, an enum without one, a
/// `Drop`-bearing struct, a plain struct, a bare `R`, a unit variant, and
/// a branch nested two deep. Interpreter twin:
/// `test_live_local_handed_out_of_a_discarded_branch_runs_payload_body_once`.
#[test]
fn e2e_live_local_handed_out_of_a_discarded_branch_runs_payload_body_once() {
    assert_eq!(
            run_program(
                r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("dE") } }
struct W { r: R, n: i64 }
impl Drop for W { fn drop(mut ref self) { println("dW") } }
struct P { r: R, n: i64 }
enum E2 { A(R), B }
fn mk(n: i64) -> R { return R { id: n }; }
fn if_let_e2(c: bool) { let e = E2.A(mk(21)); let _ = if c { E2.A(mk(8)) } else { e }; println("mid") }
fn direct_let_e2() { let e = E2.A(mk(22)); let _ = e; println("mid") }
fn direct_bare_e2() { let e = E2.A(mk(23)); e; println("mid") }
fn direct_let_read(c: bool) { let e = E.A(mk(24)); let _ = e; println("mid") }
fn direct_let_e_unit() { let e = E.B; let _ = e; println("mid") }
fn if_let(c: bool) { let e = E.A(mk(1)); let _ = if c { E.A(mk(8)) } else { e }; println("mid") }
fn if_bare(c: bool) { let e = E.A(mk(2)); if c { E.A(mk(8)) } else { e }; println("mid") }
fn match_let(c: bool) { let e = E.A(mk(3)); let _ = match c { true => E.A(mk(8)), _ => e }; println("mid") }
fn match_bare(c: bool) { let e = E.A(mk(4)); match c { true => E.A(mk(8)), _ => e }; println("mid") }
fn direct_let() { let e = E.A(mk(5)); let _ = e; println("mid") }
fn direct_bare() { let e = E.A(mk(6)); e; println("mid") }
fn if_let_w(c: bool) { let w = W { r: mk(11), n: 1 }; let _ = if c { W { r: mk(8), n: 2 } } else { w }; println("mid") }
fn direct_let_w() { let w = W { r: mk(12), n: 1 }; let _ = w; println("mid") }
fn if_let_p(c: bool) { let p = P { r: mk(13), n: 1 }; let _ = if c { P { r: mk(8), n: 2 } } else { p }; println("mid") }
fn direct_let_p() { let p = P { r: mk(14), n: 1 }; let _ = p; println("mid") }
fn if_let_r(c: bool) { let r = mk(15); let _ = if c { mk(8) } else { r }; println("mid") }
fn direct_let_r() { let r = mk(16); let _ = r; println("mid") }
fn if_let_nested(c: bool) { let e = E.A(mk(17)); let _ = if c { E.A(mk(8)) } else { if c { E.B } else { e } }; println("mid") }
fn main() {
    println("if_let-f"); if_let(false);
    println("if_let-t"); if_let(true);
    println("if_bare-f"); if_bare(false);
    println("if_bare-t"); if_bare(true);
    println("match_let-f"); match_let(false);
    println("match_let-t"); match_let(true);
    println("match_bare-f"); match_bare(false);
    println("direct_let"); direct_let();
    println("direct_bare"); direct_bare();
    println("if_let_w-f"); if_let_w(false);
    println("if_let_w-t"); if_let_w(true);
    println("direct_let_w"); direct_let_w();
    println("if_let_p-f"); if_let_p(false);
    println("if_let_p-t"); if_let_p(true);
    println("direct_let_p"); direct_let_p();
    println("if_let_r-f"); if_let_r(false);
    println("direct_let_r"); direct_let_r();
    println("if_let_nested-f"); if_let_nested(false);
    println("if_let_e2-f"); if_let_e2(false);
    println("direct_let_e2"); direct_let_e2();
    println("direct_bare_e2"); direct_bare_e2();
    println("direct_let_e_unit"); direct_let_e_unit();
    println("end");
}"#
            ),
            Some("if_let-f\ndE\ndR1\nmid\nif_let-t\ndE\ndR8\ndE\ndR1\nmid\nif_bare-f\ndE\ndR2\nmid\nif_bare-t\ndE\ndR8\ndE\ndR2\nmid\nmatch_let-f\ndE\ndR3\nmid\nmatch_let-t\ndE\ndR8\ndE\ndR3\nmid\nmatch_bare-f\ndE\ndR4\nmid\ndirect_let\ndE\ndR5\nmid\ndirect_bare\ndE\ndR6\nmid\nif_let_w-f\ndW\ndR11\nmid\nif_let_w-t\ndW\ndR8\ndW\ndR11\nmid\ndirect_let_w\ndW\ndR12\nmid\nif_let_p-f\ndR13\nmid\nif_let_p-t\ndR8\ndR13\nmid\ndirect_let_p\ndR14\nmid\nif_let_r-f\ndR15\nmid\ndirect_let_r\ndR16\nmid\nif_let_nested-f\ndE\ndR17\nmid\nif_let_e2-f\ndR21\nmid\ndirect_let_e2\ndR22\nmid\ndirect_bare_e2\ndR23\nmid\ndirect_let_e_unit\ndE\nmid\nend\n".to_string()),
            "a live local handed out of a discarded branch runs its payload body once"
        );
}

/// B-2026-09-06-17 — a payload bound out of a PROJECTED enum and handed back
/// (`fn out(self) -> R { match self.e { E.A(r) => return r, .. } }`, and the
/// free-function twin `fn p_out(h: H1) -> R { match h.e { .. } }`) ran its `Drop`
/// body in the caller's walk over the argument as well as at the result's own
/// death, on a named local and a fresh temp alike, agreed on every surface. The
/// whole-param scanner keys on the bare parameter (`e_out`, one body throughout)
/// and the part-path scanner denotes returned PLACES; neither covered a projected
/// enum's payload. `fn_escaping_param_field_payload_paths` (and its owned-`self`
/// form) now reports the field path, and the callers mask that field's payload
/// bodies — in place on a named binding, payload-only in a fresh temp's walker.
///
/// `read` / `p_read` are the read-only arms (unchanged, one body); `out_some/not`
/// is the conservative-any-variant trade, where the un-taken hand-out path still
/// runs the payload's body once at the caller (`dR6` before `got1`); `out/temp`
/// is the fresh-temp RECEIVER, whose payload is one body but whose enum shell's
/// `dE` is B-2026-09-04-30's pre-existing loss, pinned as it stands.
///
/// Twin of `tests/interpreter.rs`'s `test_projected_enum_payload_handed_out_runs_one_body`, pinned to the same string.
#[test]
fn e2e_projected_enum_payload_handed_out_runs_one_body() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("  dE") } }
struct S { e: E }
struct H1 { e: E }
struct H2 { s: S }

impl H1 {
    #[allow(partial_move_of_drop_enum)]
    fn out(self) -> R { match self.e { E.A(r) => { return r; } E.B => { return mk(0); } } }
    #[allow(partial_move_of_drop_enum)]
    fn out_iflet(self) -> R { if let E.A(r) = self.e { return r; } else { return mk(0); } }
    #[allow(partial_move_of_drop_enum)]
    fn out_tail(self) -> R { match self.e { E.A(r) => r, E.B => mk(0) } }
    #[allow(partial_move_of_drop_enum)]
    fn out_some(self, k: bool) -> R { match self.e { E.A(r) => { if k { return r; } return mk(1); } E.B => { return mk(0); } } }
    #[allow(partial_move_of_drop_enum)]
    fn read(self) -> i64 { match self.e { E.A(r) => { return r.id; } E.B => { return 0; } } }
}
impl H2 { #[allow(partial_move_of_drop_enum)] fn out2(self) -> R { match self.s.e { E.A(r) => { return r; } E.B => { return mk(0); } } } }
#[allow(partial_move_of_drop_enum)]
fn p_out(h: H1) -> R { match h.e { E.A(r) => { return r; } E.B => { return mk(0); } } }
#[allow(partial_move_of_drop_enum)]
fn p_out2(h: H2) -> R { match h.s.e { E.A(r) => { return r; } E.B => { return mk(0); } } }
fn p_read(h: H1) -> i64 { match h.e { E.A(r) => { return r.id; } E.B => { return 0; } } }
#[allow(partial_move_of_drop_enum)]
fn e_out(b: E) -> R { match b { E.A(r) => { return r; } E.B => { return mk(0); } } }

fn main() {
    println("out/local"); let a1 = H1 { e: E.A(mk(1)) }; let r1 = a1.out(); println(f"  got{r1.id}");
    println("out/temp"); let r2 = H1 { e: E.A(mk(2)) }.out(); println(f"  got{r2.id}");
    println("out_iflet/local"); let a3 = H1 { e: E.A(mk(3)) }; let r3 = a3.out_iflet(); println(f"  got{r3.id}");
    println("out_tail/local"); let a4 = H1 { e: E.A(mk(4)) }; let r4 = a4.out_tail(); println(f"  got{r4.id}");
    println("out_some/taken"); let a5 = H1 { e: E.A(mk(5)) }; let r5 = a5.out_some(true); println(f"  got{r5.id}");
    println("out_some/not"); let a6 = H1 { e: E.A(mk(6)) }; let r6 = a6.out_some(false); println(f"  got{r6.id}");
    println("read/local"); let a7 = H1 { e: E.A(mk(7)) }; let x7 = a7.read(); println(f"  r{x7}");
    println("out2/local"); let a8 = H2 { s: S { e: E.A(mk(8)) } }; let r8 = a8.out2(); println(f"  got{r8.id}");
    println("p_out/local"); let a9 = H1 { e: E.A(mk(9)) }; let r9 = p_out(a9); println(f"  got{r9.id}");
    println("p_out/temp"); let r10 = p_out(H1 { e: E.A(mk(10)) }); println(f"  got{r10.id}");
    println("p_out2/local"); let a11 = H2 { s: S { e: E.A(mk(11)) } }; let r11 = p_out2(a11); println(f"  got{r11.id}");
    println("p_read/local"); let a12 = H1 { e: E.A(mk(12)) }; let x12 = p_read(a12); println(f"  r{x12}");
    println("e_out/local"); let a13 = E.A(mk(13)); let r13 = e_out(a13); println(f"  got{r13.id}");
    println("end");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"out/local
  dE
  got1
  dR1
out/temp
  got2
  dR2
out_iflet/local
  dE
  got3
  dR3
out_tail/local
  dE
  got4
  dR4
out_some/taken
  dE
  got5
  dR5
out_some/not
  dE
  got1
  dR1
read/local
  dE
  dR7
  r7
out2/local
  dE
  got8
  dR8
p_out/local
  dE
  got9
  dR9
p_out/temp
  dE
  got10
  dR10
p_out2/local
  dE
  got11
  dR11
p_read/local
  dE
  dR12
  r12
e_out/local
  dE
  got13
  dR13
end
"#
    );
}

/// B-2026-08-31-43 — a `match` / `if let` / `let … else` / `while let` over a
/// PROJECTION off an OWNED `self` receiver (`match self.e { E.A(r) => { let m = r;
/// .. } }`, one and two hops) ran the payload's `Drop` body twice on every surface:
/// the owned-param-root walk in each backend (codegen's
/// `scrutinee_is_owned_param_binding`, the interpreter's `place_root_is_owned_param`)
/// stopped at `ExprKind::Identifier`, `self` is `ExprKind::SelfValue`, so the arm's
/// binding was never a view of the caller-retained value and took a body beside the
/// caller's walk. A named by-value param in the same position (`p_take`, `p_iflet`)
/// was one body throughout — the control.
///
/// The fresh-temp receiver (`take/temp`, `viacall/temp`, `iflet/temp`) had been
/// losing the enum shell's `dE` all along, because the B-2026-09-04-30 gate
/// declined to retain a temp receiver's bodies caller-side for any method that
/// binds a part of `self` out; a projection scrutinee no longer counts as one. The
/// `read` cells are the read-only arms (`read2/local` was a compiled-only double at
/// two hops); `borrowed/local` is `mut ref self`, where the second body is the
/// documented copy (design.md "A projection off a borrow is an implicit copy") and
/// must stay at two.
///
/// Twin of `tests/interpreter.rs`'s `test_owned_self_projection_scrutinee_runs_one_payload_body`, pinned to the same string.
#[test]
fn e2e_owned_self_projection_scrutinee_runs_one_payload_body() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("  dE") } }
struct S { e: E }
struct H1 { e: E }
struct H2 { s: S }
fn consume(x: R) -> i64 { return x.id }

impl H1 {
    #[allow(partial_move_of_drop_enum)]
    fn take(self) -> i64 { match self.e { E.A(r) => { let m = r; return m.id; } E.B => { return 0; } } }
    fn read(self) -> i64 { match self.e { E.A(r) => { return r.id; } E.B => { return 0; } } }
    fn viacall(self) -> i64 { match self.e { E.A(r) => { return consume(r); } E.B => { return 0; } } }
    #[allow(partial_move_of_drop_enum)]
    fn iflet(self) -> i64 { if let E.A(r) = self.e { let m = r; return m.id; } else { return 0; } }
    #[allow(partial_move_of_drop_enum)]
    fn letelse(self) -> i64 { let E.A(r) = self.e else { return 0; }; let m = r; return m.id; }
    #[allow(partial_move_of_drop_enum)]
    fn whilelet(self) -> i64 { while let E.A(r) = self.e { let m = r; return m.id; } return 0; }
    #[allow(partial_move_of_drop_enum)]
    fn borrowed(mut ref self) -> i64 { match self.e { E.A(r) => { let m = r; return m.id; } E.B => { return 0; } } }
}
impl H2 {
    #[allow(partial_move_of_drop_enum)]
    fn take2(self) -> i64 { match self.s.e { E.A(r) => { let m = r; return m.id; } E.B => { return 0; } } }
    fn read2(self) -> i64 { match self.s.e { E.A(r) => { return r.id; } E.B => { return 0; } } }
}
#[allow(partial_move_of_drop_enum)]
fn p_take(h: H1) -> i64 { match h.e { E.A(r) => { let m = r; return m.id; } E.B => { return 0; } } }
#[allow(partial_move_of_drop_enum)]
fn p_iflet(h: H1) -> i64 { if let E.A(r) = h.e { let m = r; return m.id; } else { return 0; } }

fn main() {
    println("take/local"); let a1 = H1 { e: E.A(mk(1)) }; let x1 = a1.take(); println(f"  r{x1}");
    println("take/temp"); let x2 = H1 { e: E.A(mk(2)) }.take(); println(f"  r{x2}");
    println("take2/local"); let a3 = H2 { s: S { e: E.A(mk(3)) } }; let x3 = a3.take2(); println(f"  r{x3}");
    println("read/local"); let a4 = H1 { e: E.A(mk(4)) }; let x4 = a4.read(); println(f"  r{x4}");
    println("read2/local"); let a5 = H2 { s: S { e: E.A(mk(5)) } }; let x5 = a5.read2(); println(f"  r{x5}");
    println("viacall/local"); let a6 = H1 { e: E.A(mk(6)) }; let x6 = a6.viacall(); println(f"  r{x6}");
    println("viacall/temp"); let x7 = H1 { e: E.A(mk(7)) }.viacall(); println(f"  r{x7}");
    println("iflet/local"); let a8 = H1 { e: E.A(mk(8)) }; let x8 = a8.iflet(); println(f"  r{x8}");
    println("iflet/temp"); let x9 = H1 { e: E.A(mk(9)) }.iflet(); println(f"  r{x9}");
    println("letelse/local"); let a10 = H1 { e: E.A(mk(10)) }; let x10 = a10.letelse(); println(f"  r{x10}");
    println("whilelet/local"); let a11 = H1 { e: E.A(mk(11)) }; let x11 = a11.whilelet(); println(f"  r{x11}");
    println("borrowed/local"); let mut a12 = H1 { e: E.A(mk(12)) }; let x12 = a12.borrowed(); println(f"  r{x12}");
    println("p_take/local"); let a13 = H1 { e: E.A(mk(13)) }; let x13 = p_take(a13); println(f"  r{x13}");
    println("p_iflet/local"); let a14 = H1 { e: E.A(mk(14)) }; let x14 = p_iflet(a14); println(f"  r{x14}");
    println("end");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"take/local
  dE
  dR1
  r1
take/temp
  dE
  dR2
  r2
take2/local
  dE
  dR3
  r3
read/local
  dE
  dR4
  r4
read2/local
  dE
  dR5
  r5
viacall/local
  dE
  dR6
  r6
viacall/temp
  dE
  dR7
  r7
iflet/local
  dE
  dR8
  r8
iflet/temp
  dE
  dR9
  r9
letelse/local
  dE
  dR10
  r10
whilelet/local
  dE
  dR11
  r11
borrowed/local
  dR12
  dE
  dR12
  r12
p_take/local
  dE
  dR13
  r13
p_iflet/local
  dE
  dR14
  r14
end
"#
    );
}

/// B-2026-09-06-36 — a `match` / `if let` over a LOCAL struct scrutinee whose
/// ENUM leaf the arm never consumes lost that leaf's `Drop` body on every
/// compiled backend: `let c = H1 { e: E.A(mk(1)) }; match c { H1 { e } => { .. } }`
/// with `e` untouched printed `m` alone under `karac run` / `karac build` /
/// `KARAC_AUTO_PAR=0`, against `m dE dR1` on `--interp`. Memory was balanced
/// throughout — a lost BODY, not a leak.
///
/// `disarm_arm_destructured_struct_field_bodies` masked a field's bodies
/// whenever its sub-pattern was a bare binding, without asking whether the arm
/// used it. That mask is a HANDOVER — its own doc says it exists so a moved-out
/// field is not walked twice — and with nothing moved out there is nobody to
/// hand to.
///
/// The repair registers a bodies-only walker on the BINDING rather than
/// declining the mask, because the MEMORY half beside it has already given the
/// binding the field's heap ("the binding owns the field's entire heap
/// subtree"). Leaving the body with the source ran it over the husk the
/// cap-zeroing left: measured `dR0` where `dR34` was due — the exact symptom
/// the disarm's own doc records. Body and memory stay with one owner.
///
/// TWO EXCLUSIONS, each established by a measured double rather than by
/// argument, and each pinned by a cell here:
///   * an owned-PARAM scrutinee (`by_value_param`, and the `self`-receiver) —
///     the leaf is a view of the callee's entry copy whose body the CALLER runs
///     (caller-retains). Registering here too gave `dE dR7 dE dR7`.
///   * a STRUCT leaf (`struct_leaf`) — already covered by the source's own
///     field-bodies walker; registering gave `s dR8 dR8`. The row scoped itself
///     to an ENUM leaf and listed the struct leaf as NOT MEASURED. This is that
///     measurement, and it says leave it alone.
///
/// `let … else` is deliberately untouched: its binding escapes into the
/// enclosing block, so there is no scope to classify it against, and it keeps
/// today's mask — the same `scope: None` convention the interpreter's twin
/// states.
///
/// CELLS: `unread` (the row's own shape), `bound_result` (match result bound
/// rather than discarded — the row notes the discard/bound distinction is not
/// the discriminator), `iflet` (the `if let` spelling the row lists as NOT
/// MEASURED, which diverged identically), `read_only` (leaf bound beside a
/// scalar the arm returns), `consumed` (the arm hands the leaf to a call, which under
/// caller-retains is NOT a transfer, so the binding still owes the body —
/// the cell is named for its shape, not for a handover), `by_value_param` and
/// `struct_leaf` (the two
/// exclusions), `wildcard` (`e: _`, which never masked and was always right).
///
/// All four surfaces — `--interp`, `karac run`, `KARAC_AUTO_PAR=0` and the
/// default auto-par build — now print this string byte-identically, so the
/// twinned pair is pinned to ONE expected output rather than two.
///
/// Twin of `tests/interpreter.rs`'s
/// `test_unconsumed_enum_leaf_of_a_local_struct_scrutinee_runs_its_body`.
#[test]
fn e2e_unconsumed_enum_leaf_of_a_local_struct_scrutinee_runs_its_body() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("  dE") } }
struct H1 { e: E }
struct H2 { r: R }
struct H3 { e: E, k: i64 }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
fn eat(e: E) -> i64 { match e { E.A(r) => { return r.id; } E.B => { return 0; } } }

fn unread() { let c: H1 = H1 { e: E.A(mk(1)) }; match c { H1 { e } => { println("  m"); } } }
fn bound_result() -> i64 { let c: H1 = H1 { e: E.A(mk(2)) }; let k: i64 = match c { H1 { e } => { 9 } }; return k; }
fn iflet() { let c: H1 = H1 { e: E.A(mk(3)) }; if let H1 { e } = c { println("  i"); } }
fn read_only() -> i64 { let c: H3 = H3 { e: E.A(mk(4)), k: 5 }; match c { H3 { e, k } => { return k; } } }
fn consumed() -> i64 { let c: H1 = H1 { e: E.A(mk(6)) }; match c { H1 { e } => { return eat(e); } } }
fn by_value_param(h: H1) -> i64 { match h { H1 { e } => { return 9; } } }
fn struct_leaf() { let c: H2 = H2 { r: mk(8) }; match c { H2 { r } => { println("  s"); } } }
fn wildcard() { let c: H1 = H1 { e: E.A(mk(9)) }; match c { H1 { e: _ } => { println("  w"); } } }

fn main() {
    println("unread"); unread();
    println("bound_result"); let a: i64 = bound_result(); println(f"  ={a}");
    println("iflet"); iflet();
    println("read_only"); let b: i64 = read_only(); println(f"  ={b}");
    println("consumed"); let c2: i64 = consumed(); println(f"  ={c2}");
    println("by_value_param"); let d: i64 = by_value_param(H1 { e: E.A(mk(7)) }); println(f"  ={d}");
    println("struct_leaf"); struct_leaf();
    println("wildcard"); wildcard();
    println("end");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"unread
  m
  dE
  dR1
bound_result
  dE
  dR2
  =9
iflet
  i
  dE
  dR3
read_only
  dE
  dR4
  =5
consumed
  dE
  dR6
  =6
by_value_param
  dE
  dR7
  =9
struct_leaf
  s
  dR8
wildcard
  w
  dE
  dR9
end
"#
    );
}

/// B-2026-09-06-38 — a FRESH-TEMP owned ENUM receiver lost the enum SHELL's own
/// `Drop` body on every surface: `E.A(mk(2)).m_read()` printed `dR2 x2` and never
/// `dE`, where the named local `let a = E.A(mk(1)); a.m_read()` printed `dR1 dE x1`.
/// B-2026-09-04-30's receiver-temp registrar kept the value-enum arm memory-only
/// (B-2026-08-01-5's reasoning: a ref-self method binding the payload fired the
/// interpreter's arm channel, so a walk here would double it). That covered the
/// PAYLOAD; the shell's own body has no arm to fire from and had no owner for a
/// temp. Measured wider, a `ref self` temp (`E.A(mk(6)).m_ref()`) fired NOTHING —
/// neither payload nor shell — on all four surfaces, the arm channel having stood
/// down on a borrowed receiver since B-2026-08-28-67's read-through gate.
///
/// Both registrars now give an enum receiver temp its bodies at the statement's
/// end, in two shapes: a `ref self` / `mut ref self` method BORROWED the temp, so
/// the caller owns the whole value — shell body, then the payload walk (`ref/temp`
/// `dE dR6 x6`, the order `ref/local` prints; `refnoshell/temp` `dR13`); an owned
/// `self` CONSUMED it and the arm channel runs the payload (B-2026-09-06-27, -37),
/// so the caller registers the shell's body ALONE (`read/temp` `dR2 dE x2`,
/// `print/temp` `p5 dR5 dE`, `iflet/temp` `dR7 dE x7`). The owned gate is
/// `owned_self_return_cannot_carry_receiver`, the enum-specific form of the struct
/// arm's opacity gate: it declines only a return that can carry the WHOLE receiver
/// (`-> E`, `-> Self`, `-> W { e: E }`, `-> Option[E]`), the one shape whose result
/// binding would run the shell body a second time — `me/temp`, `wrap/temp`,
/// `optself/temp` keep their single `dE` — and admits a payload hand-back (`-> R`,
/// `-> Option[R]`), which doubles nothing: `r/temp` `dE y4 dR4` now matches
/// `r/local` `dE y3 dR3`. Memory is untouched (the new registrations are
/// bodies-only fns behind the unchanged `track_enum_var` free). The chain link
/// (`E.A(mk(11)).me().m_read()`, `chain/temp`) stays shell-less, the struct side's
/// recorded residual; `unit/temp` (`E.B.m_read()`) was already right.
///
/// B-2026-09-06-39 — REPINNED. `read/*`, `print/temp` and `iflet/temp` now print
/// the shell's body before the payload's (`dE dR1` for `dR1 dE`), the design.md
/// § Part 8 order: a read-only bare-`self` arm over an enum with its own `Drop`
/// binds VIEWS now and the caller owns the payload's body. `r/*` (the arm hands
/// the payload back) and `chain/temp` are unchanged — the first because the arm
/// really does take it, the second because the fresh-temp registrar was widened
/// to admit a chain-link receiver for that walk rather than lose it.
///
/// Twin of `tests/interpreter.rs`'s `test_fresh_temp_owned_enum_receiver_runs_the_shell_body`, pinned to the same string.
#[test]
fn e2e_fresh_temp_owned_enum_receiver_runs_the_shell_body() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("  dE") } }
struct W { e: E }
enum F { A(R), B }
impl E {
    fn m_read(self) -> i64 { match self { E.A(r) => { return r.id; } E.B => { return 0; } } }
    #[allow(partial_move_of_drop_enum)]
    fn m_r(self) -> R { match self { E.A(r) => { return r; } E.B => { return mk(0); } } }
    fn m_print(self) { match self { E.A(r) => { println(f"  p{r.id}"); } E.B => { println("  pB"); } } }
    fn m_ref(ref self) -> i64 { match self { E.A(r) => { return r.id; } E.B => { return 0; } } }
    fn m_iflet(self) -> i64 { if let E.A(r) = self { return r.id; } else { return 0; } }
    fn me(self) -> E { return self; }
    fn wrap(self) -> W { return W { e: self }; }
    fn m_opt(self) -> Option[R] { match self { E.A(r) => { return Some(r); } E.B => { return None; } } }
    fn m_optself(self) -> Option[E] { return Some(self); }
    fn m_mut(mut ref self) -> i64 { match self { E.A(r) => { return r.id; } E.B => { return 0; } } }
}
impl F {
    fn m_read(self) -> i64 { match self { F.A(r) => { return r.id; } F.B => { return 0; } } }
    fn m_ref(ref self) -> i64 { match self { F.A(r) => { return r.id; } F.B => { return 0; } } }
}
fn main() {
    println("read/local"); let a = E.A(mk(1)); let x = a.m_read(); println(f"  x{x}");
    println("read/temp"); let x2 = E.A(mk(2)).m_read(); println(f"  x{x2}");
    println("r/local"); let b = E.A(mk(3)); let y = b.m_r(); println(f"  y{y.id}");
    println("r/temp"); let y2 = E.A(mk(4)).m_r(); println(f"  y{y2.id}");
    println("print/temp"); E.A(mk(5)).m_print(); println("  after");
    println("ref/temp"); let x6 = E.A(mk(6)).m_ref(); println(f"  x{x6}");
    println("iflet/temp"); let x7 = E.A(mk(7)).m_iflet(); println(f"  x{x7}");
    println("unit/temp"); let x8 = E.B.m_read(); println(f"  x{x8}");
    println("noshell/temp"); let x9 = F.A(mk(8)).m_read(); println(f"  x{x9}");
    println("me/temp"); let e = E.A(mk(9)).me(); println("  held");
    println("wrap/temp"); let w = E.A(mk(10)).wrap(); println("  held");
    println("chain/temp"); let x11 = E.A(mk(11)).me().m_read(); println(f"  x{x11}");
    println("ref/local"); let c = E.A(mk(12)); let x12 = c.m_ref(); println(f"  x{x12}");
    println("refnoshell/temp"); let x13 = F.A(mk(13)).m_ref(); println(f"  x{x13}");
    println("refnoshell/local"); let d = F.A(mk(14)); let x14 = d.m_ref(); println(f"  x{x14}");
    println("opt/temp"); let o16 = E.A(mk(16)).m_opt(); println("  held");
    println("optself/temp"); let o17 = E.A(mk(17)).m_optself(); println("  held");
    println("mut/temp"); let x18 = E.A(mk(18)).m_mut(); println(f"  x{x18}");
    println("end");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"read/local
  dE
  dR1
  x1
read/temp
  dE
  dR2
  x2
r/local
  dE
  y3
  dR3
r/temp
  dE
  y4
  dR4
print/temp
  p5
  dE
  dR5
  after
ref/temp
  dE
  dR6
  x6
iflet/temp
  dE
  dR7
  x7
unit/temp
  dE
  x0
noshell/temp
  dR8
  x8
me/temp
  dE
  dR9
  held
wrap/temp
  dE
  dR10
  held
chain/temp
  dR11
  x11
ref/local
  dE
  dR12
  x12
refnoshell/temp
  dR13
  x13
refnoshell/local
  dR14
  x14
opt/temp
  dE
  dR16
  held
optself/temp
  dE
  dR17
  held
mut/temp
  dE
  dR18
  x18
end
"#
    );
}

#[test]
fn e2e_deep_projection_scrutinee_runs_one_payload_body() {
    let hdr = "struct R { id: i64 }\n\
                   impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
                   enum E { A(R), B }\n\
                   impl Drop for E { fn drop(mut ref self) { println(\"dE\") } }\n\
                   struct S { e: E }\n\
                   struct W { s: S }\n\
                   struct X { w: W }\n\
                   struct Sd { e: E }\n\
                   impl Drop for Sd { fn drop(mut ref self) { println(\"dSd\") } }\n\
                   struct Wd { s: Sd }\n\
                   struct S2 { n: i64, e: E }\n\
                   struct W2 { k: R, s: S2 }\n\
                   struct Coll { e: E, s: S }\n\
                   struct Two { a: S, b: S }\n";
    for (label, body, want) in [
            (
                "control: one hop",
                "let s = S { e: E.A(R { id: 8 }) };\n\
                 match s.e { E.A(r) => { let m = r; println(f\"m{m.id}\") } E.B => {} }",
                "m8\ndR8\ndE\n",
            ),
            (
                "two hops — the row",
                "let w = W { s: S { e: E.A(R { id: 8 }) } };\n\
                 match w.s.e { E.A(r) => { let m = r; println(f\"m{m.id}\") } E.B => {} }",
                "m8\ndR8\ndE\n",
            ),
            (
                "three hops",
                "let x = X { w: W { s: S { e: E.A(R { id: 8 }) } } };\n\
                 match x.w.s.e { E.A(r) => { let m = r; println(f\"m{m.id}\") } E.B => {} }",
                "m8\ndR8\ndE\n",
            ),
            (
                "two hops, intermediate struct has its OWN Drop",
                "let w = Wd { s: Sd { e: E.A(R { id: 8 }) } };\n\
                 match w.s.e { E.A(r) => { let m = r; println(f\"m{m.id}\") } E.B => {} }",
                "m8\ndR8\ndSd\ndE\n",
            ),
            (
                "two hops, sibling fields still walked",
                "let w = W2 { k: R { id: 3 }, s: S2 { n: 1, e: E.A(R { id: 8 }) } };\n\
                 match w.s.e { E.A(r) => { let m = r; println(f\"m{m.id}\") } E.B => {} }",
                "m8\ndR8\ndE\ndR3\n",
            ),
            (
                "two hops, `if let` spelling",
                "let w = W { s: S { e: E.A(R { id: 8 }) } };\n\
                 if let E.A(r) = w.s.e { let m = r; println(f\"m{m.id}\") }",
                "m8\ndR8\ndE\n",
            ),
            (
                "two hops, a statement follows",
                "let w = W { s: S { e: E.A(R { id: 8 }) } };\n\
                 match w.s.e { E.A(r) => { let m = r; println(f\"m{m.id}\") } E.B => {} }\n\
                 println(\"after\");",
                "m8\ndR8\ndE\nafter\n",
            ),
            // CONTROLS the mask must keep DECLINING. A non-consuming arm did
            // not take the payload, so the owner still owes that body; the
            // `B` arm holds no payload at all.
            (
                "control: two hops, arm does NOT consume",
                "let w = W { s: S { e: E.A(R { id: 8 }) } };\n\
                 match w.s.e { E.A(r) => { println(f\"n{r.id}\") } E.B => {} }",
                "n8\ndE\ndR8\n",
            ),
            (
                "control: two hops, payload-less arm taken",
                "let w = W { s: S { e: E.B } };\n\
                 match w.s.e { E.A(r) => { let m = r; println(f\"m{m.id}\") } E.B => { println(\"bee\") } }",
                "bee\ndE\n",
            ),
            // COMPOSITION. A path mask must land on the level that owns the
            // enum and nowhere else — these are the rows that fail if the
            // nesting is built or consumed at the wrong depth.
            (
                "name collision: masking the INNER `e` leaves the outer's payload",
                "let c = Coll { e: E.A(R { id: 1 }), s: S { e: E.A(R { id: 8 }) } };\n\
                 match c.s.e { E.A(r) => { let m = r; println(f\"m{m.id}\") } E.B => {} }",
                "m8\ndR8\ndE\ndE\ndR1\n",
            ),
            (
                "name collision: masking the OUTER `e` leaves the inner's payload",
                "let c = Coll { e: E.A(R { id: 1 }), s: S { e: E.A(R { id: 8 }) } };\n\
                 match c.e { E.A(r) => { let m = r; println(f\"m{m.id}\") } E.B => {} }",
                "m1\ndR1\ndE\ndR8\ndE\n",
            ),
            (
                "compose: a one-hop and a two-hop mask on the same variable",
                "let c = Coll { e: E.A(R { id: 1 }), s: S { e: E.A(R { id: 8 }) } };\n\
                 match c.e { E.A(r) => { let m = r; println(f\"o{m.id}\") } E.B => {} }\n\
                 match c.s.e { E.A(r) => { let m = r; println(f\"i{m.id}\") } E.B => {} }",
                "o1\ndR1\ni8\ndR8\ndE\ndE\n",
            ),
            (
                "compose: both sibling subtrees masked",
                "let t = Two { a: S { e: E.A(R { id: 1 }) }, b: S { e: E.A(R { id: 2 }) } };\n\
                 match t.a.e { E.A(r) => { let m = r; println(f\"a{m.id}\") } E.B => {} }\n\
                 match t.b.e { E.A(r) => { let m = r; println(f\"b{m.id}\") } E.B => {} }",
                "a1\ndR1\nb2\ndR2\ndE\ndE\n",
            ),
            (
                "compose: only ONE sibling subtree masked",
                "let t = Two { a: S { e: E.A(R { id: 1 }) }, b: S { e: E.A(R { id: 2 }) } };\n\
                 match t.a.e { E.A(r) => { let m = r; println(f\"a{m.id}\") } E.B => {} }",
                "a1\ndR1\ndE\ndR2\ndE\n",
            ),
        ] {
            let src = format!("{hdr}#[allow(partial_move_of_drop_enum)]\nfn main() {{\n{body}\n}}\n");
            assert_eq!(run_program(&src).as_deref(), Some(want), "[{label}]");
        }
    // A `self`-ROOTED projection scrutinee through a BORROWED receiver
    // (`mut ref self`) runs the payload's body twice, and that is the
    // documented copy, not a gap: design.md "A projection off a borrow is
    // an implicit copy, and that is a stopgap" -- the match copies
    // `E.A(R)` out of the borrow, `m` owns the copy and runs its body, and
    // the caller's receiver still owns the original and runs its own. The
    // explicit spelling `let e = h.e` through `ref h` prints `dE dR dE dR`
    // on every surface (valgrind-clean with a heap-carrying payload) and
    // carries W0299 `borrow_projection_copy` saying so. B-2026-08-31-43
    // first pinned this as a KNOWN GAP; its fix covers the OWNED receiver
    // (`#[allow(partial_move_of_drop_enum)]\nfn take(self)`), whose payload was a genuine double -- see
    // `e2e_owned_self_projection_scrutinee_runs_one_payload_body`. Kept at
    // the copy's transcript so a change in that stopgap fails loudly here.
    let selfrooted = "struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             enum E { A(R), B }\n\
             impl Drop for E { fn drop(mut ref self) { println(\"dE\") } }\n\
             struct S { e: E }\n\
             struct H1 { e: E }\n\
             impl H1 { #[allow(partial_move_of_drop_enum)]\nfn take(mut ref self) -> i64 { match self.e { E.A(r) => { let m = r; return m.id; } E.B => { return 0; } } } }\n\
             struct H2 { s: S }\n\
             impl H2 { #[allow(partial_move_of_drop_enum)]\nfn take(mut ref self) -> i64 { match self.s.e { E.A(r) => { let m = r; return m.id; } E.B => { return 0; } } } }\n\
             #[allow(partial_move_of_drop_enum)]\nfn main() {\n\
             \x20   let mut a = H1 { e: E.A(R { id: 1 }) };\n\
             \x20   println(a.take());\n\
             \x20   let mut b = H2 { s: S { e: E.A(R { id: 2 }) } };\n\
             \x20   println(b.take());\n\
             }\n";
    assert_eq!(
        run_program(selfrooted).as_deref(),
        Some("dR1\n1\ndE\ndR1\ndR2\n2\ndE\ndR2\n"),
        "[borrowed receiver: the projection copies, one hop and two]"
    );
}

/// B-2026-09-12-17 / B-2026-09-13-24 — a GENERIC user enum's payload `Drop` body
/// runs for a fresh-temp constructor argument, and the axis is GENERICITY rather
/// than "user enum vs the seeded pair".
///
/// The name-keyed walker `emit_enum_payload_user_drop_bodies_fn` skips a payload
/// declared as one of the enum's own generic params (B-2026-08-03-5's guard), so
/// for `enum Ho[T] { Full(T) }` every slot was skipped, it returned `None`, and a
/// fresh temp — which has no `let` site to fall back on — had no owner for its
/// payload body at all. The instantiation-keyed walker
/// `emit_generic_enum_payload_user_drop_bodies_fn` (B-2026-09-10-2) is now tried
/// when the name-keyed one declines.
///
/// CELLS 4-9 ARE THE CONTROLS THAT MAKE THE FIX A PARTITION RATHER THAN A
/// WIDENING. The monomorphic enum reaches the name-keyed walker and the seeded
/// `Option` reaches its own instantiation-keyed gate; both were already correct,
/// and neither may now DOUBLE. The generic named-local cell is correct through its
/// `let` site, which is the asymmetry that localised this in the first place, and
/// the payload-less variant pins that the tag switch still selects nothing to run.
///
/// INLINE PAYLOADS ONLY, and cells 10-12 are why. When the instantiated payload
/// outgrows the erased one-word payload area it is heap-BOXED, and the box's own
/// interior drop already runs the body (B-2026-09-10-2) -- so registering here as
/// well gives it TWO owners. The first pass at this fix did exactly that and
/// doubled the body for a three-`String` payload, caught by
/// `asan_generic_enum_payload_runs_its_drop_and_frees_its_interior` rather than by
/// anything here: every cell in this table used a ONE-WORD payload, so the table
/// could not see the boxing axis at all. Cell 10 is that shape, pinned.
///
/// ASAN STAYED CLEAN THROUGH THAT REGRESSION -- a duplicated body is not a double
/// free -- so only an output comparison catches this class. That is the argument
/// for pinning cell 10 here rather than relying on the sanitizer suite.
///
/// CELL 3 WAS THE PINNED GAP AND IS NOW FIXED (B-2026-09-13-24). The spelling
/// qualified WITH generic args (`Ho[R].Full(..)`) parses as a `MethodCall` whose
/// receiver is a one-segment path carrying generic args, and neither fresh-temp
/// registrar had an arm for that node: `enum_name_of_expr` has none, and the
/// interpreter's `fresh_temp_arg_type_name` had none either. Both gained one, in
/// the same commit, because the cell was AGREED-silent and repairing either
/// backend alone would have converted an agreed gap into a fresh divergence —
/// which is precisely what the pin was here to catch, and it did: the codegen
/// half alone turned this cell red.
///
/// CELL 13 IS THE BOXED TWIN OF CELL 3, and measuring it corrected the row. The
/// "runs on NO surface" framing is true only for the INLINE payload: with a
/// three-`String` payload the box's own interior drop already carried the body on
/// the compiled backends, so `holdw(Ho[W].Full(mkw()))` was `hw / end` on the
/// interpreter against `hw / dW4 / end` compiled — a live run-vs-build divergence,
/// not an agreed gap. The interpreter arm repairs it, and the cell pins that the
/// codegen arm did NOT add a second owner on top of the box route (the doubling
/// that cells 10-12 exist for).
///
/// CELL 14 IS THE OVER-CLAIM CONTROL. `Ho[R].mk(R { id: 7 })` — an associated
/// function on the same generic enum — has the IDENTICAL node shape to cell 3 and
/// must not be claimed by either registrar, since its result is owned by whatever
/// the callee returns. Both predicates ask `qualified_enum_variant_is_unit`, which
/// answers `None` for a name that is not a variant, and this cell is what holds
/// that distinction in place.
///
/// Twin in the other backend's suite under the same name, same table.
#[test]
fn e2e_generic_enum_ctor_temp_arg_runs_its_payload_drop_body() {
    let hdr = "struct R { id: i64 }\n\
                   impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
                   struct W { a: String, b: String, c: String }\n\
                   impl Drop for W { fn drop(mut ref self) { println(f\"dW{self.a.len()}\") } }\n\
                   fn mkw() -> W { return W { a: f\"aaa{1}\", b: f\"bbb{1}\", c: f\"ccc{1}\" }; }\n\
                   enum Ho[T] { Full(T), Empty }\n\
                       impl[T] Ho[T] { fn mk(v: T) -> Ho[T] { return Ho.Full(v); } }\n\
                   enum Mo { Whole(R), Nil }\n\
                   enum Bo { Wide(W), Nil }\n\
                   fn takeit(x: Ho[R]) { match x { Full(r) => { println(f\"f:{r.id}\") } Empty => { println(\"e\") } } }\n\
                   fn takemo(x: Mo) { match x { Whole(r) => { println(f\"f:{r.id}\") } Nil => { println(\"e\") } } }\n\
                   fn taken(o: Option[R]) { match o { Some(r) => { println(f\"f:{r.id}\") } None => { println(\"e\") } } }\n\
                   fn takebo(x: Bo) { match x { Wide(r) => { println(f\"w:{r.a.len()}\") } Nil => { println(\"e\") } } }\n\
                   fn holdw(x: Ho[W]) { println(\"hw\"); }\n\
                   fn holdbo(x: Bo) { println(\"hb\"); }\n";
    for (label, stmts, want) in [
        (
            "generic enum, bare ctor temp",
            "takeit(Full(R { id: 5 }));",
            "f:5\ndR5\nend\n",
        ),
        (
            "generic enum, qualified-without-args ctor temp",
            "takeit(Ho.Full(R { id: 5 }));",
            "f:5\ndR5\nend\n",
        ),
        (
            "generic enum, ctor qualified WITH generic args",
            "takeit(Ho[R].Full(R { id: 5 }));",
            "f:5\ndR5\nend\n",
        ),
        (
            "control: monomorphic enum, bare ctor temp",
            "takemo(Whole(R { id: 5 }));",
            "f:5\ndR5\nend\n",
        ),
        (
            "control: monomorphic enum, qualified ctor temp",
            "takemo(Mo.Whole(R { id: 5 }));",
            "f:5\ndR5\nend\n",
        ),
        (
            "control: the seeded Option oracle",
            "taken(Some(R { id: 5 }));",
            "f:5\ndR5\nend\n",
        ),
        (
            "control: the seeded Option oracle, qualified",
            "taken(Option[R].Some(R { id: 5 }));",
            "f:5\ndR5\nend\n",
        ),
        (
            "control: generic enum through a NAMED LOCAL",
            "let h: Ho[R] = Full(R { id: 5 });\n\
                 takeit(h);",
            "f:5\ndR5\nend\n",
        ),
        (
            "control: generic enum, payload-less variant",
            "takeit(Empty);",
            "e\nend\n",
        ),
        (
            "REGRESSION GUARD: boxed generic payload, callee does not bind it",
            "holdw(Ho.Full(mkw()));",
            "hw\ndW4\nend\n",
        ),
        (
            "control: boxed payload in a MONOMORPHIC enum, callee does not bind",
            "holdbo(Bo.Wide(mkw()));",
            "hb\ndW4\nend\n",
        ),
        (
            "control: boxed payload in a MONOMORPHIC enum, arm binds it",
            "takebo(Bo.Wide(mkw()));",
            "w:4\ndW4\nend\n",
        ),
        (
            "the BOXED twin of cell 3 — was interp-vs-compiled divergent",
            "holdw(Ho[W].Full(mkw()));",
            "hw\ndW4\nend\n",
        ),
        (
            "control: an ASSOC FN with the same node shape is not a ctor",
            "takeit(Ho[R].mk(R { id: 7 }));",
            "dR7\nf:7\nend\n",
        ),
    ] {
        let src = format!("{hdr}fn main() {{\n{stmts}\nprintln(\"end\");\n}}\n");
        assert_eq!(run_program(&src).as_deref(), Some(want), "[{label}]");
    }
}

/// B-2026-09-14-22 — a GENERIC enum's BOXED payload keeps its `Drop` body when the
/// consuming arm BINDS that payload read-only.
///
/// `suppress_destructured_enum_payload_cleanup` masks the scrutinee's payload-BODIES
/// walk for every position the arm consumes, on the premise that the arm's binding
/// now owns the body. For a GENERIC payload behind a BY-VALUE PARAM scrutinee that
/// premise fails on both halves at once: the instantiation-keyed walker
/// `__karac_dropelems_genum_<te>` is the only bodies channel a generic payload has
/// (the name-keyed one skips a payload declared as the enum's own parameter —
/// B-2026-08-03-5's guard), and a param scrutinee's arm binds a VIEW whose drop is
/// the memory-only `__karac_drop_struct_<T>`. So the mask retracted the one channel
/// and handed the body to a binding that runs none: `w:4 / end` compiled against
/// `w:4 / dW4 / end` on the interpreter, a live run-vs-build divergence.
///
/// The gate is THREE predicates and every one of them earns its place, each pinned
/// by a cell that went wrong when it was missing:
///
/// * read-only arm (`binding_only_borrowed`) — cell 10 is the consuming arm, still a
///   gap and pinned as one below.
/// * generic payload (`arm_consumes_only_generic_payload`) — cells 4-5 and 8 are the
///   MONOMORPHIC twin, which has a SECOND channel in the enum's own
///   `__karac_drop_<E>` switch; skipping the mask there DOUBLES the body.
/// * param scrutinee (`scrutinee_is_owned_param_binding`) — cells 6-7 match a NAMED
///   LOCAL, whose arm binding gets its own body-running `karac_drop_<T>`; skipping
///   the mask there printed `w:4 / dW4 / dW4 / end`, measured.
///
/// AND THE MEMORY HALF OF THE SAME BLOCK IS NOT GATED. `clear_boxed_enum_inner_drop`
/// is what stops the box drop and the binding from both freeing a boxed payload; the
/// first cut of this fix skipped the whole block and turned cells 6-7 into
/// `free(): double free detected in tcache 2`, exit 134. Only the BODIES mask is
/// gated — hence the inner `if` rather than a condition on the outer one.
///
/// INLINE PAYLOADS ARE A DIFFERENT CELL ALREADY GREEN, which is why this row is
/// separate from `(test|e2e)_generic_enum_ctor_temp_arg_runs_its_payload_drop_body`:
/// that table's one-word `R` payload rides the inline route and was never masked.
/// Three `String`s (9 words) outgrow the erased one-word payload area and heap-box,
/// which is the whole of this row.
///
/// CELL 9 IS THE TWO-PARAMETER DECLARATION (`enum Ro[T, E]`), which the row listed as
/// NOT MEASURED: divergent before the fix, correct on all four surfaces after, so the
/// gate keys on the payload rather than on the enum's arity.
///
/// CELL 10 IS THE REMAINING GAP, split out as its own open row rather than buried
/// here: an arm that CONSUMES its binding (`let z = r`) still loses the body on every
/// compiled surface. It is pinned at the divergent value in this suite and at the
/// correct one in the interpreter's twin, deliberately, so the split is visible in
/// both tables rather than quietly absent from one.
///
/// Twin in the other backend's suite under the same name, same table.
#[test]
fn e2e_generic_boxed_enum_payload_body_survives_a_read_only_binding_arm() {
    let hdr = "struct W { a: String, b: String, c: String }\n\
                  impl Drop for W { fn drop(mut ref self) { println(f\"dW{self.a.len()}\") } }\n\
                  fn mkw() -> W { return W { a: f\"aaa{1}\", b: f\"bbb{1}\", c: f\"ccc{1}\" }; }\n\
                  enum Ho[T] { Full(T), Empty }\n\
                  enum Mo { Full(W), Empty }\n\
                  enum Ro[T, E] { Good(T), Bad(E) }\n\
                  fn taker(x: Ro[W, i64]) { match x { Good(r) => { println(f\"w:{r.a.len()}\") } Bad(n) => { println(f\"b{n}\") } } }\n\
                  fn takew(x: Ho[W]) { match x { Full(r) => { println(f\"w:{r.a.len()}\") } Empty => { println(\"e\") } } }\n\
                  fn holdw(x: Ho[W]) { match x { Full(_) => { println(\"w\") } Empty => { println(\"e\") } } }\n\
                  fn takem(x: Mo) { match x { Full(r) => { println(f\"w:{r.a.len()}\") } Empty => { println(\"e\") } } }\n\
                  fn holdm(x: Mo) { match x { Full(_) => { println(\"w\") } Empty => { println(\"e\") } } }\n\
                  fn consumew(x: Ho[W]) { match x { Full(r) => { let z = r; println(f\"w:{z.a.len()}\") } Empty => { println(\"e\") } } }\n\
                  fn mkho() -> Ho[W] { return Ho.Full(mkw()); }\n\
                  fn mkmo() -> Mo { return Mo.Full(mkw()); }\n";
    for (label, stmts, want) in [
            (
                "generic, boxed payload, read-only binding arm, fresh ctor temp",
                "takew(Ho.Full(mkw()));",
                "w:4\ndW4\nend\n",
            ),
            (
                "generic, same arm, a named local moved into the callee",
                "let g: Ho[W] = Ho.Full(mkw());\ntakew(g);",
                "w:4\ndW4\nend\n",
            ),
            (
                "control: the arm does NOT bind (wildcard) — never masked, never lost",
                "holdw(Ho.Full(mkw()));",
                "w\ndW4\nend\n",
            ),
            (
                "control: MONOMORPHIC twin, binding arm — has a second channel, must not double",
                "takem(Mo.Full(mkw()));",
                "w:4\ndW4\nend\n",
            ),
            (
                "control: MONOMORPHIC twin, non-binding arm",
                "holdm(Mo.Full(mkw()));",
                "w\ndW4\nend\n",
            ),
            (
                "control: generic, NAMED LOCAL scrutinee from a call — arm owns, must not double",
                "let g = mkho();\nmatch g { Full(r) => { println(f\"w:{r.a.len()}\") } Empty => { println(\"e\") } }",
                "w:4\ndW4\nend\n",
            ),
            (
                "control: generic, NAMED LOCAL scrutinee from a ctor",
                "let g: Ho[W] = Ho.Full(mkw());\nmatch g { Full(r) => { println(f\"w:{r.a.len()}\") } Empty => { println(\"e\") } }",
                "w:4\ndW4\nend\n",
            ),
            (
                "control: MONOMORPHIC named local scrutinee from a call",
                "let g = mkmo();\nmatch g { Full(r) => { println(f\"w:{r.a.len()}\") } Empty => { println(\"e\") } }",
                "w:4\ndW4\nend\n",
            ),
            (
                "generic with TWO parameters — the `Result`-shaped declaration",
                "taker(Ro.Good(mkw()));",
                "w:4\ndW4\nend\n",
            ),
            (
                "PINNED GAP: the arm CONSUMES its binding (`let z = r`) — body lost when compiled",
                "consumew(Ho.Full(mkw()));",
                "w:4\nend\n",
            ),
        ] {
            let src = format!("{hdr}fn main() {{\n{stmts}\nprintln(\"end\");\n}}\n");
            assert_eq!(run_program(&src).as_deref(), Some(want), "[{label}]");
        }
}

/// B-2026-09-20-41 — a generic enum whose payload instantiates to a
/// CONTAINER runs its ELEMENTS' `Drop` bodies at a read-only match arm.
///
/// The generic-payload bodies walker is the SOLE channel that can run
/// `R::drop` for a `Slot[Array[R, 2]]`: there is no `karac_drop_Array`
/// carrying it, and the arm binds `v` read-only so the binding never
/// becomes an owner. The bodies mask used to stand that walker down at
/// every scrutinee except a by-value param, which is the right answer only
/// when the payload has a SECOND channel — a payload struct with its own
/// `impl Drop`, the `Ho[W]` cell below, where standing the mask down
/// prints the body twice. So the gate asks whether the payload's bodies
/// are ELEMENT-ONLY, and asks it of the INSTANTIATION: the variant
/// declares its payload as `T`, one word, from which no container head is
/// readable.
///
/// The two PINNED GAPS were the remainder, and they were DIFFERENT faults
/// rather than two spellings of one. ONE IS NOW FIXED: a `Vec` payload was
/// silent on ALL FOUR surfaces — `--interp` included, and silent with no
/// `match` in the program at all — because its walker was discarded before
/// emission (`define 0 / call 0`; the `Array` twin, which this fixture's
/// first cell covers, measured `define 1 / call 0` BEFORE this fix); that
/// half was B-2026-09-20-62 and its cell below now asserts the bodies. The
/// OTHER STANDS: a fresh ctor temp never reaches this site at all and loses
/// the bodies only on the three COMPILED surfaces, where `--interp` is
/// correct; that half is B-2026-09-20-63, which additionally leaks the
/// container buffer, and B-2026-09-20-62 left it exactly as it found it.
#[test]
fn e2e_generic_enum_container_payload_runs_element_bodies_at_a_read_only_arm() {
    let hdr = "struct R { id: i64 }\n\
                   impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
                   fn mkr(i: i64) -> R { return R { id: i }; }\n\
                   struct W { a: String }\n\
                   impl Drop for W { fn drop(mut ref self) { println(f\"dW{self.a.len()}\") } }\n\
                   fn mkw() -> W { return W { a: f\"aaa{1}\" }; }\n\
                   enum Slot[T] { S(T), N }\n\
                   fn seenarr(x: Slot[Array[R, 2]]) { match x { Slot.S(v) => { println(f\"x{v[0].id}\") } Slot.N => { println(\"no\") } } }\n";
    for (label, stmts, want) in [
            (
                "THE FIX: Array payload, NAMED LOCAL scrutinee, read-only arm",
                "let a: Array[R, 2] = [mkr(1), mkr(2)];\n\
                 let s: Slot[Array[R, 2]] = Slot.S(a);\n\
                 match s { Slot.S(v) => { println(f\"x{v[0].id}\") } Slot.N => { println(\"no\") } }",
                "x1\ndR1\ndR2\nend\n",
            ),
            (
                "control: same shape, the arm CONSUMES its binding — unchanged",
                "let a: Array[R, 2] = [mkr(1), mkr(2)];\n\
                 let s: Slot[Array[R, 2]] = Slot.S(a);\n\
                 match s { Slot.S(v) => { let z = v; println(f\"x{z[0].id}\") } Slot.N => { println(\"no\") } }",
                "x1\ndR1\ndR2\nend\n",
            ),
            (
                "control: BY-VALUE PARAM scrutinee — the arm the old gate allowed",
                "let a: Array[R, 2] = [mkr(1), mkr(2)];\nseenarr(Slot.S(a));",
                "x1\ndR1\ndR2\nend\n",
            ),
            (
                "control: no match at all — the husk's own walk, never masked",
                "let a: Array[R, 2] = [mkr(1), mkr(2)];\n\
                 let s: Slot[Array[R, 2]] = Slot.S(a);\n\
                 println(\"mid\");",
                "dR1\ndR2\nmid\nend\n",
            ),
            (
                "control: payload struct with its OWN body — a second channel, must NOT double",
                "let g: Slot[W] = Slot.S(mkw());\n\
                 match g { Slot.S(r) => { println(f\"w:{r.a.len()}\") } Slot.N => { println(\"no\") } }",
                "w:4\ndW4\nend\n",
            ),
            (
                "B-2026-09-20-62: a Vec payload — its walker is emitted now, and \
                 runs on the ARM'S BINDING because the binding holds the buffer",
                "let a: Vec[R] = [mkr(1), mkr(2)];\n\
                 let s: Slot[Vec[R]] = Slot.S(a);\n\
                 match s { Slot.S(v) => { println(f\"x{v[0].id}\") } Slot.N => { println(\"no\") } }",
                "x1\ndR1\ndR2\nend\n",
            ),
            (
                "RETIRED PIN: a FRESH CTOR TEMP scrutinee. This cell asserted the \
                 wrong-but-current `\"x1\\nend\\n\"` while B-2026-09-20-63 was open \
                 and B-2026-09-21-11 after it closed the leak; both have landed, \
                 so the two elements' bodies now run. The payload here infers to \
                 Vec[R], and the bodies come from the ARM'S BINDING rather than \
                 from the scrutinee husk, which stands down because that binding \
                 owns the buffer",
                "match Slot.S([mkr(1), mkr(2)]) { Slot.S(v) => { println(f\"x{v[0].id}\") } Slot.N => { println(\"no\") } }",
                "x1\ndR1\ndR2\nend\n",
            ),
        ] {
            let src = format!("{hdr}fn main() {{\n{stmts}\nprintln(\"end\");\n}}\n");
            assert_eq!(run_program(&src).as_deref(), Some(want), "[{label}]");
        }
}

#[test]
fn e2e_freshtemp_generic_enum_scrutinee_owns_its_instantiated_payload() {
    let hdr = "struct R { id: i64 }\n\
                   impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
                   fn mkr(i: i64) -> R { return R { id: i }; }\n\
                   enum Slot[T] { S(T), N }\n\
                   enum Ev { V(Vec[R]), N }\n";
    for (label, stmts, want) in [
            (
                "THE FIX: Array payload, FRESH CTOR TEMP scrutinee, binding arm",
                "let a: Array[R, 2] = [mkr(1), mkr(2)];\n\
                 match Slot.S(a) { Slot.S(v) => { println(f\"x{v[0].id}\") } Slot.N => { println(\"no\") } }",
                "x1\ndR1\ndR2\nend\n",
            ),
            (
                "THE FIX, several arms with a GUARD: the walker fires once at the \
                 merge block, so every arm of a match whose arms all bind must \
                 agree — this is the two-arm cell that a single-arm grid cannot see",
                "let a: Array[R, 2] = [mkr(1), mkr(2)];\n\
                 match Slot.S(a) {\n\
                   Slot.S(v) if v[0].id > 99 => { println(f\"big{v[0].id}\") }\n\
                   Slot.S(w) => { println(f\"x{w[1].id}\") }\n\
                   Slot.N => { println(\"no\") }\n\
                 }",
                "x2\ndR1\ndR2\nend\n",
            ),
            (
                "AGREED GAP, PINNED: the same temp with an arm that DISCARDS its \
                 payload. Silent on all four surfaces and on the declared spelling \
                 too, so it stays silent here; only its leak is closed",
                "let a: Array[R, 2] = [mkr(1), mkr(2)];\n\
                 match Slot.S(a) { Slot.S(_) => { println(\"x\") } Slot.N => { println(\"no\") } }",
                "x\nend\n",
            ),
            (
                "RETIRED PIN: the Vec spelling of the fix's own cell. B-2026-09-20-63 \
                 closed its LEAK and pinned the silence at `\"x1\\nend\\n\"`; \
                 B-2026-09-21-11 then gave the bodies to the ARM'S BINDING, where \
                 they fire ahead of its own buffer free. The husk still stands \
                 down here, which is why that row's fix is at a different site",
                "let a: Vec[R] = [mkr(1), mkr(2)];\n\
                 match Slot.S(a) { Slot.S(v) => { println(f\"x{v[0].id}\") } Slot.N => { println(\"no\") } }",
                "x1\ndR1\ndR2\nend\n",
            ),
            (
                "control: the ctor BOUND FIRST — B-2026-09-20-62's cell, unchanged",
                "let a: Vec[R] = [mkr(1), mkr(2)];\n\
                 let s = Slot.S(a);\n\
                 match s { Slot.S(v) => { println(f\"x{v[0].id}\") } Slot.N => { println(\"no\") } }",
                "x1\ndR1\ndR2\nend\n",
            ),
            (
                "control: the DECLARED-container spelling at the same position. Its \
                 own agreed gap on all four surfaces, and this fix must not move it \
                 — arming one backend of an agreed gap is strictly worse than the gap",
                "let a: Vec[R] = [mkr(1), mkr(2)];\n\
                 match Ev.V(a) { Ev.V(v) => { println(f\"x{v[0].id}\") } Ev.N => { println(\"no\") } }",
                "x1\nend\n",
            ),
            (
                "control: a whole-value DISCARD of the same ctor — already correct \
                 through B-2026-09-20-15's discard registrar, must not double",
                "let a: Array[R, 2] = [mkr(1), mkr(2)];\n\
                 let _ = Slot.S(a);",
                "dR1\ndR2\nend\n",
            ),
        ] {
            let src = format!("{hdr}fn main() {{\n{stmts}\nprintln(\"end\");\n}}\n");
            assert_eq!(run_program(&src).as_deref(), Some(want), "[{label}]");
        }
}

/// B-2026-09-06-39 — CELL `b` WAS PINNING A LOST BODY, and its own label
/// said so: `just_three(self)` never destructures `self`, and this fixture
/// recorded `w=3` with no `drop 8 e8` under the words "consumed silently".
/// The disarm cell `a` needs is a HAND-OFF to the arm channel, and cell `b`
/// has no arm — so it reached nobody and the payload's body was lost on
/// every surface. Both cells now fire exactly once. Label corrected.
#[test]
fn e2e_owned_self_enum_receiver_single_fire() {
    let Some(out) = run_program(
        "struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"drop {self.id} {self.name}\")\n\
             \x20   }\n\
             }\n\
             enum Box2 { Full(Res), Empty }\n\
             impl Box2 {\n\
             \x20   fn into_id(self) -> i64 {\n\
             \x20       match self {\n\
             \x20           Box2.Full(r) => { return r.id; }\n\
             \x20           Box2.Empty => { return 0; }\n\
             \x20       }\n\
             \x20   }\n\
             \x20   fn just_three(self) -> i64 {\n\
             \x20       return 3;\n\
             \x20   }\n\
             }\n\
             fn mk_e(n: i64) -> Box2 {\n\
             \x20   return Box2.Full(Res { id: n, name: f\"e{n}\" });\n\
             }\n\
             fn main() {\n\
             \x20   println(\"a: owned-self match-consume fires once via the arm\");\n\
             \x20   let b = mk_e(7);\n\
             \x20   let v = b.into_id();\n\
             \x20   println(f\"v={v}\");\n\
             \x20   println(\"b: owned-self non-consuming — runs its payload body\");\n\
             \x20   let c = mk_e(8);\n\
             \x20   let w = c.just_three();\n\
             \x20   println(f\"w={w}\");\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(
        out,
        "a: owned-self match-consume fires once via the arm\ndrop 7 e7\nv=7\n\
             b: owned-self non-consuming — runs its payload body\ndrop 8 e8\nw=3\nend\n"
    );
}

/// B-2026-07-30-11 (enum leg) — a value enum's live-variant PAYLOAD runs its
/// user `impl Drop` body when the enum binding dies.
///
/// Every backend was silent here before: `let s = Slot.Full(Res { .. })`
/// freed the payload's memory (`__karac_drop_<E>`'s `NestedStruct` arm) but
/// ran no body, so a resource held in an enum payload was never released.
///
/// Pins, in order: a tuple variant; a struct variant (payload addressed by
/// declared position, not by name order); two payloads in forward order; a
/// payload whose own FIELD is the Drop-bearing one; and an all-scalar enum
/// that must emit nothing. `Res { id: i64 }` owns NO heap deliberately —
/// the body must run for a payload the memory-side machinery ignores.
///
/// Twinned with `tests/interpreter.rs`'s
/// `test_enum_payload_runs_user_drop_bodies`.
#[test]
fn e2e_enum_payload_runs_user_drop_bodies() {
    let Some(out) = run_program(
        "struct Res { id: i64 }\n\
             impl Drop for Res { fn drop(mut ref self) { println(self.id); } }\n\
             struct W { r: Res }\n\
             enum Slot { Empty, Full(Res) }\n\
             enum Named { None0, One { r: Res, tag: i64 } }\n\
             enum Pair { Zero, Two(Res, Res) }\n\
             enum Nest { Nil, Wrap(W) }\n\
             enum Plain { A, B(i64) }\n\
             fn main() {\n\
             \x20   { let s = Slot.Full(Res { id: 21 }); println(1); }\n\
             \x20   { let n = Named.One { r: Res { id: 22 }, tag: 5 }; println(2); }\n\
             \x20   { let p = Pair.Two(Res { id: 23 }, Res { id: 24 }); println(3); }\n\
             \x20   { let w = Nest.Wrap(W { r: Res { id: 25 } }); println(4); }\n\
             \x20   { let q = Plain.B(7); println(5); }\n\
             \x20   println(999);\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "21\n1\n22\n2\n24\n23\n3\n25\n4\n5\n999\n");
}

/// B-2026-07-30-11 (enum leg) — a `match` / `if let` arm that BINDS the
/// payload out disarms the source's payload-body walk, so the body does not
/// fire against a payload the source no longer owns. A `_` sub-pattern
/// claims no ownership, so there the source still fires.
///
/// Without the disarm the body ran on the cap-zeroed source and printed
/// garbage read out of a wiped payload.
///
/// Twinned with `tests/interpreter.rs`'s
/// `test_enum_payload_move_out_disarms_source_drop`.
#[test]
fn e2e_enum_payload_move_out_disarms_source_drop() {
    let Some(out) = run_program(
        "struct Res { id: i64 }\n\
             impl Drop for Res { fn drop(mut ref self) { println(self.id); } }\n\
             enum Slot { Empty, Full(Res) }\n\
             fn main() {\n\
             \x20   { let s = Slot.Full(Res { id: 31 });\n\
             \x20     match s { Slot.Full(r) => println(r.id), Slot.Empty => println(0) } }\n\
             \x20   { let t = Slot.Full(Res { id: 32 });\n\
             \x20     match t { Slot.Full(_) => println(100), Slot.Empty => println(0) } }\n\
             \x20   { let u = Slot.Full(Res { id: 33 });\n\
             \x20     if let Slot.Full(q) = u { println(q.id) } }\n\
             \x20   println(999);\n\
             }\n",
    ) else {
        return;
    };
    // 31: bound out — the source runs nothing, and since the match-arm
    // leg (B-2026-07-30-11) the ARM BINDING runs the body at its NLL end
    // (the second 31; it was a residual leak before). 32: `_` binds
    // nothing, so the source still fires. 33: since the if-let leg the
    // `if let` payload binding rides the same arm-drop channel, so the
    // second 33 fires exactly like the match arm's second 31 (it was
    // the remaining silent-by-parity residual before).
    assert_eq!(out, "31\n31\n100\n32\n33\n33\n999\n");
}

/// B-2026-07-31-5 — a value enum's OWN `impl Drop` body fires at the
/// binding's NLL live-range end, not at scope exit.
///
/// `karac_drop_<E>` is body-only for an enum (the wrapper's field-bodies and
/// struct-memory steps both decline for an enum name; payload memory is the
/// separate scope-exit `EnumDrop`), but it was excluded from the NLL channel
/// because the filter tested `struct_types`. Measured `mid|DS` under AOT vs
/// `DS|mid` under `karac run`.
///
/// The second block pins the interleave when the enum ALSO has a
/// Drop-bearing payload: own body first, then the payload's — the order
/// `run_user_drop_body` + `drop_user_drop_fields_of_binding` produce.
#[test]
fn e2e_enum_own_drop_body_fires_at_nll_end() {
    let Some(out) = run_program(
        "struct Res { id: i64 }\n\
             impl Drop for Res { fn drop(mut ref self) { println(self.id); } }\n\
             enum Bare { Empty, Full(i64) }\n\
             impl Drop for Bare { fn drop(mut ref self) { println(1); } }\n\
             enum Both { Nil, Held(Res) }\n\
             impl Drop for Both { fn drop(mut ref self) { println(2); } }\n\
             fn main() {\n\
             \x20   { let b = Bare.Full(7); println(50); }\n\
             \x20   { let h = Both.Held(Res { id: 41 }); println(51); }\n\
             \x20   println(999);\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "1\n50\n2\n41\n51\n999\n");
}

#[test]
fn test_e2e_recursive_shared_enum_arg_correct() {
    // B-2026-07-12-25 correctness pin (the ASAN/leak gate lives in
    // tests/memory_sanitizer.rs::asan_recursive_shared_enum_arg_no_leak).
    // A freshly-constructed `shared enum` value passed by value to a
    // recursive self-call registered a caller-side RC-dec only at EVEN
    // constructor-nesting depth (`fresh_arg_bare_shared_heap_type`'s
    // passthrough self-exclusion recursed and flipped Some/None per level),
    // so odd-depth `Node(Node(...))` args leaked the whole chain. The fix
    // skips the passthrough guard for a variant constructor (which owns its
    // payload via its recursive drop). This pins the value at odd nesting.
    let out = run_program(
        r#"
shared enum E { Leaf(i64), Node(E) }
fn chk(e: E) -> i64 {
    match e {
        Leaf(n) => n,
        Node(x) => chk(x)
    }
}
fn main() {
    println(chk(Node(Node(Node(Leaf(42))))));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "42");
    }
}

/// B-2026-07-05-2 (fix 81ad98c4): moving a for-loop `Vec[<user enum>]` element
/// whole into a new owner (`let x = a`) deep-copies the live variant's payload,
/// so the new binding's `EnumDrop` and the container's per-element drain free
/// independent heap. Build-side output must match `karac run` (288). The
/// `memory_sanitizer` ASAN cases cover the double-free; this pins the numeric
/// result on the plain codegen surface.
#[test]
fn for_loop_enum_element_whole_move_runs() {
    let src = "enum Tok { Empty, Word(String) }\n\
                   fn build() -> Vec[Tok] {\n\
                   \x20   let mut v: Vec[Tok] = Vec.new();\n\
                   \x20   let mut i = 0;\n\
                   \x20   while i < 6 { v.push(Tok.Word(\"forloop_enum_element_whole_move_payload_theta_xx\".to_string())); i = i + 1; }\n\
                   \x20   v\n\
                   }\n\
                   fn main() {\n\
                   \x20   let items = build();\n\
                   \x20   let mut n: i64 = 0;\n\
                   \x20   for a in items {\n\
                   \x20       let x = a;\n\
                   \x20       match x { Tok.Word(s) => { n = n + s.len(); } Tok.Empty => {} }\n\
                   \x20   }\n\
                   \x20   println(n);\n\
                   }\n";
    assert_eq!(run_program(src).as_deref(), Some("288\n"));
}

/// B-2026-07-05-2, NestedStruct-payload variant: the deep-copy recurses into
/// the inline struct's own heap fields (copy-depth == drop-depth), matching
/// `karac run` (240).
#[test]
fn for_loop_enum_element_nested_struct_payload_runs() {
    let src = "struct Inner { s: String }\n\
                   enum Node { Leaf, Wrap(Inner) }\n\
                   fn build() -> Vec[Node] {\n\
                   \x20   let mut v: Vec[Node] = Vec.new();\n\
                   \x20   let mut i = 0;\n\
                   \x20   while i < 5 { v.push(Node.Wrap(Inner { s: \"forloop_enum_nested_struct_payload_iota_field_yy\".to_string() })); i = i + 1; }\n\
                   \x20   v\n\
                   }\n\
                   fn main() {\n\
                   \x20   let items = build();\n\
                   \x20   let mut n: i64 = 0;\n\
                   \x20   for a in items {\n\
                   \x20       let x = a;\n\
                   \x20       match x { Node.Wrap(inner) => { n = n + inner.s.len(); } Node.Leaf => {} }\n\
                   \x20   }\n\
                   \x20   println(n);\n\
                   }\n";
    assert_eq!(run_program(src).as_deref(), Some("240\n"));
}

/// B-2026-07-03-7 (codegen side): `Vec[Enum].sort()` — variant discriminant
/// (declaration order) then payload, including tuple, struct-shaped, and
/// unit variants. See `e2e_struct_sort_declaration_order` for the class.
#[test]
fn e2e_enum_sort_declaration_order() {
    if let Some(out) = run_program(
        "#[derive(Eq, Ord)]\n\
             enum Shape { Circle(i64), Rect(i64, i64), Unit }\n\
             fn stag(s: ref Shape) -> i64 {\n\
             \x20   match s {\n\
             \x20       Shape.Circle(r) => 100 + r,\n\
             \x20       Shape.Rect(w, h) => 200 + w * 10 + h,\n\
             \x20       Shape.Unit => 300,\n\
             \x20   }\n\
             }\n\
             fn main() {\n\
             \x20   let mut v: Vec[Shape] = Vec.new();\n\
             \x20   v.push(Shape.Rect(2, 1));\n\
             \x20   v.push(Shape.Circle(9));\n\
             \x20   v.push(Shape.Unit);\n\
             \x20   v.push(Shape.Rect(1, 5));\n\
             \x20   v.push(Shape.Circle(3));\n\
             \x20   v.sort();\n\
             \x20   let mut i = 0;\n\
             \x20   while i < v.len() { let s = ref v[i]; println(f\"{stag(s)}\"); i = i + 1; };\n\
             }",
    ) {
        // Circle(0) < Rect(1) < Unit(2); within: Circle 3<9, Rect (1,5)<(2,1).
        assert_eq!(out, "103\n109\n215\n221\n300\n");
    }
}

/// Direct recursive `shared enum` — `Add(Expr, Expr)` (two recursive
/// fields) built as an RC tree and folded by a recursive `eval`. Before the
/// fix the value was laid out by value (`{tag, w0}`) instead of an RC heap
/// handle, so the match scrutinee/payload extraction ICE'd in
/// `pattern_binding`. The fix routes shared-typed pattern bindings through
/// the pointer path (`pattern_payload_{llvm_type,word_count}` shared arms)
/// and the ownership checker accepts the breakable shared cycle.
#[test]
fn e2e_direct_recursive_shared_enum_two_field() {
    if let Some(out) = run_program(
        "shared enum Expr { Num(i64), Add(Expr, Expr) }\n\
             fn eval(e: Expr) -> i64 {\n\
                 match e {\n\
                     Num(n) => n,\n\
                     Add(a, b) => eval(a) + eval(b),\n\
                 }\n\
             }\n\
             fn main() {\n\
                 let e = Add(Num(3), Add(Num(4), Num(5)));\n\
                 println(eval(e));\n\
             }",
    ) {
        assert_eq!(out, "12\n");
    }
}

#[test]
fn e2e_recursive_shared_enum_recursive_variant_declared_first() {
    // B-2026-06-13-10: a recursive `shared enum` whose RECURSIVE variant is
    // declared before the base case must typecheck + run. The match
    // exhaustiveness check (`src/exhaustive.rs`) overflowed the compiler
    // stack here: `unreachable_arms` specializing the `Add` arm emptied the
    // pattern matrix, and — missing the Maranget `U(∅, q)` base case — the
    // wildcard columns over the recursive `Expr` enumerated constructors and
    // descended into `Add` forever. Order-dependent only by accident
    // (base-case-first short-circuited on the terminating constructor); the
    // `…_two_field` test above puts `Num` first, which masked it.
    if let Some(out) = run_program(
        "shared enum Expr { Add(Expr, Expr), Num(i64) }\n\
             fn eval(e: Expr) -> i64 {\n\
                 match e {\n\
                     Num(n) => n,\n\
                     Add(a, b) => eval(a) + eval(b),\n\
                 }\n\
             }\n\
             fn main() {\n\
                 let e = Add(Num(3), Add(Num(4), Num(5)));\n\
                 println(eval(e));\n\
             }",
    ) {
        assert_eq!(out, "12\n");
    }
}

/// Single recursive field (`Box(Wrap)`) — the minimal direct-recursion
/// shape; the recursive payload binding `inner` must extract as a pointer.
#[test]
fn e2e_direct_recursive_shared_enum_single_field() {
    if let Some(out) = run_program(
        "shared enum Wrap { Leaf(i64), Box(Wrap) }\n\
             fn depth(w: Wrap) -> i64 {\n\
                 match w {\n\
                     Leaf(n) => 0,\n\
                     Box(inner) => 1 + depth(inner),\n\
                 }\n\
             }\n\
             fn main() {\n\
                 let w = Box(Box(Leaf(7)));\n\
                 println(depth(w));\n\
             }",
    ) {
        assert_eq!(out, "2\n");
    }
}

#[test]
fn e2e_shared_enum_struct_variant_construct_and_match() {
    // B-2026-06-13-8: a SHARED enum struct-variant (`shared enum T { Node {
    // v: i64 } }`) must (a) construct as an RC heap box `{rc, tag, words}`
    // reached by pointer — not the inline `{tag, words}` aggregate, which
    // mismatched the by-pointer shared ABI — and (b) bind its named fields
    // in a match (the via_ptr binder deferred enum struct-variants and the
    // value-path Struct arm required a StructValue, so a pointer scrutinee
    // missed both → "Undefined variable"). Distinct from B-13-7 (which was
    // a PLAIN enum, unqualified-only). Covers qualified + unqualified +
    // multi-field-with-String, all matching the interpreter.
    if let Some(out) = run_program(
        "shared enum T { Node { v: i64 }, Leaf }\n\
             shared enum P { Pair { a: i64, s: String }, None }\n\
             fn f(t: T) -> i64 { match t { T.Node { v } => v, Leaf => -1 } }\n\
             fn g(t: T) -> i64 { match t { Node { v } => v, Leaf => -1 } }\n\
             fn h(p: P) -> i64 { match p { Pair { a, s } => a + s.len(), None => 0 } }\n\
             fn main() {\n\
                 println(f(T.Node { v: 7 }));\n\
                 println(f(T.Leaf));\n\
                 println(g(T.Node { v: 9 }));\n\
                 println(h(P.Pair { a: 3, s: \"hello\" }));\n\
             }",
    ) {
        assert_eq!(out, "7\n-1\n9\n8\n");
    }
}

#[test]
fn e2e_unqualified_struct_variant_construction() {
    // B-2026-06-13-12 (codegen twin): an UNQUALIFIED struct-variant
    // *construction* `Variant { .. }` must build, not just typecheck.
    // Pre-fix the codegen `StructLiteral` dispatch only recognized the
    // qualified `Enum.Variant { .. }` form (path.len() >= 2); an unqualified
    // `Circle { r: 2 }` fell through to `compile_struct_init`, treating the
    // variant name as a struct — the constructed value was a bare field
    // scalar, so passing it to `area(Shape)` failed module verification
    // ("Call parameter type does not match function signature"). The
    // dispatch now finds the parent enum from `enum_layouts`. Plain enum.
    if let Some(out) = run_program(
        "enum Shape { Circle { r: i64 }, Rect { w: i64, h: i64 } }\n\
             fn area(s: Shape) -> i64 {\n\
                 match s {\n\
                     Circle { r } => r * r * 3,\n\
                     Rect { w, h } => w * h,\n\
                 }\n\
             }\n\
             fn main() {\n\
                 let a: Shape = Circle { r: 2 };\n\
                 let b: Shape = Rect { w: 3, h: 4 };\n\
                 println(f\"{area(a)}\");\n\
                 println(f\"{area(b)}\");\n\
             }",
    ) {
        assert_eq!(out, "12\n12\n");
    }
}

#[test]
fn e2e_shared_unqualified_struct_variant_construction() {
    // B-2026-06-13-12 (codegen twin, shared): unqualified construction of a
    // `shared enum` struct-variant, including the nested recursive-AST shape
    // (`Bin { l: Num { .. }, r: Num { .. } }`) that surfaced the bug. The
    // RC-heap-box construction path must fire for the unqualified form too
    // (pre-fix it `compile_struct_init`'d and segfaulted at run).
    if let Some(out) = run_program(
        "shared enum Expr { Num { n: i64 }, Bin { l: Expr, r: Expr } }\n\
             fn eval(e: Expr) -> i64 {\n\
                 match e {\n\
                     Num { n } => n,\n\
                     Bin { l, r } => eval(l) + eval(r),\n\
                 }\n\
             }\n\
             fn main() {\n\
                 let t: Expr = Bin { l: Num { n: 3 }, r: Num { n: 4 } };\n\
                 println(f\"{eval(t)}\");\n\
             }",
    ) {
        assert_eq!(out, "7\n");
    }
}

// ── A struct field whose type is a USER enum (self-hosting blocker) ──

#[test]
fn test_e2e_user_enum_field_in_struct() {
    // Regression for the self-hosting `enum-in-struct-field` codegen
    // blocker: `declare_structs` ran before `declare_enums`, so a struct
    // field naming a user enum wasn't in `enum_layouts` yet and
    // `llvm_type_for_name` collapsed it to the `i64` fall-through —
    // losing the payload word. The match arm then bound nothing
    // (`Undefined variable 'name'` at codegen). Fixed by a two-pass
    // struct declaration (metadata → declare_enums → struct LLVM types).
    //
    // Exercises the load-bearing details: a WIDE payload variant
    // (`Pair(i64, String)` — multi-word, so a mis-sized enum slot would
    // corrupt layout), and a scalar field AFTER the enum field (`end`)
    // whose correct read proves the enum field is sized exactly right
    // (neither collapsed to i64 nor over-wide).
    if let Some(out) = run_program(
            "enum Token { Ident(String), Num(i64), Pair(i64, String), Eof }\n\
             struct Spanned { start: i64, tok: Token, end: i64 }\n\
             fn show(sp: Spanned) {\n\
                 print(f\"{sp.start}:\");\n\
                 match sp.tok {\n\
                     Ident(name) => { print(\"Id(\"); print(name); print(\")\"); }\n\
                     Num(n) => { print(f\"Num({n})\"); }\n\
                     Pair(a, s) => { print(\"Pair(\"); print(f\"{a},\"); print(s); print(\")\"); }\n\
                     Eof => { print(\"Eof\"); }\n\
                 }\n\
                 println(f\":{sp.end}\");\n\
             }\n\
             fn main() {\n\
                 show(Spanned { start: 0, tok: Token.Ident(\"foo\"), end: 3 });\n\
                 show(Spanned { start: 3, tok: Token.Num(42), end: 5 });\n\
                 show(Spanned { start: 5, tok: Token.Pair(7, \"bar\"), end: 9 });\n\
                 show(Spanned { start: 9, tok: Token.Eof, end: 9 });\n\
             }",
        ) {
            assert_eq!(
                out,
                "0:Id(foo):3\n3:Num(42):5\n5:Pair(7,bar):9\n9:Eof:9\n"
            );
        }
}

#[test]
fn test_e2e_ref_enum_tuple_payload() {
    // Regression for B-2026-09-12-4: a TUPLE payload bound through a `ref`
    // scrutinee read as garbage. Third instance of the same via-ptr trap as
    // B-2026-07-09-6 (struct payload) and B-2026-07-23-3 (Map/Set payload):
    // the typechecker records the binding's surface type as the bare name
    // "Tuple", `llvm_type_for_name` has no entry for it and falls back to
    // i64, so `declared_mismatches_word` compared i64 against the i64
    // payload word, saw no mismatch, and bound the leaf as a ref-to-i64 AT
    // THE PAYLOAD WORD. Every read through the binding then reinterpreted
    // that single word as the whole tuple.
    //
    // The three cells below are the three ways the enum can hold it, and
    // all three were broken -- so this was never confined to the generic
    // enum family it was found in. Compiled vs `--interp`, pre-fix:
    //
    //     enum M { P((i64, i64)), Q }   a=0 b=0          vs a=42 b=7
    //     Option[(i64, i64)]            a=0 b=0          vs a=42 b=7
    //     enum Slot[T] at T=(i64, i64)  a=0 b=0          vs a=42 b=7
    //
    // `mono` is a plain MONOMORPHIC enum whose 2-word tuple payload was
    // admitted by `ok_padded_primitive` (a tuple of scalars really does
    // have an i64 first word); `boxd` is the generic case, where the
    // payload is heap-boxed and `num_words` is 1, so `ok_single_word`
    // admitted it and the binding aliased the BOX POINTER itself. Neither
    // existing guard could see either one.
    //
    // The scalar spelling is deliberate: it isolates the read from any
    // memory question, since nothing here allocates. At `(String, i64)`
    // the same shape read a null pointer (`Invalid read of size 1 ...
    // Address 0x0` under valgrind) and silently took the wrong branch.
    if let Some(out) = run_program(
        "enum M { P((i64, i64)), Q }\n\
             enum Slot[T] { Filled(T), Blank }\n\
             fn mono(s: ref M) -> i64 {\n\
                 match s { P(x) => { return x.0 + x.1; } Q => { return -1; } }\n\
             }\n\
             fn opt(s: ref Option[(i64, i64)]) -> i64 {\n\
                 match s { Some(x) => { return x.0 + x.1; } None => { return -1; } }\n\
             }\n\
             fn boxd(s: ref Slot[(i64, i64)]) -> i64 {\n\
                 match s { Filled(x) => { return x.0 + x.1; } Blank => { return -1; } }\n\
             }\n\
             fn main() {\n\
                 let n = env.args().len() as i64;\n\
                 let a: M = M.P((41 + n, 7));\n\
                 let b: Option[(i64, i64)] = Some((41 + n, 7));\n\
                 let c: Slot[(i64, i64)] = Filled((41 + n, 7));\n\
                 println(f\"{mono(a)}:{opt(b)}:{boxd(c)}\");\n\
             }",
    ) {
        assert_eq!(out, "49:49:49\n");
    }
}

/// B-2026-08-14-3 — a generic instantiated at an UNSIGNED width must
/// zero-extend, not sign-extend.
///
/// Codegen answers "is this value unsigned?" with a syntactic walk
/// (`expr_is_unsigned_int`): a suffixed literal, a variable's declared type
/// name, a callee's declared return-type name. That walk is exact wherever
/// the type is spelled concretely and BLIND through a generic —
/// `fn idg[T](x: T) -> T` registers its return type as `T`, which is not a
/// uint name, so the print path fell through to signed. Every generic shape
/// here printed a plausible negative number on both compiled backends while
/// the interpreter printed the right value, with `karac check` silent.
///
/// The `u64` line is not redundant with the narrow ones. At 64 bits there
/// is no extension to get wrong; only the print FORMATTING is, and
/// `idg(u64::MAX)` rendered -1. That is the evidence the fault was the
/// signedness question rather than the widening, which is why the fix is a
/// type hint rather than a change to any extend site.
///
/// The two `as i64` lines are the one shape the span-keyed hint cannot
/// serve on its own: the parser gives a `Cast` its OPERAND's span, so the
/// cast's target type is the last write at that key and the operand's
/// signedness is unrecoverable from it. They fail without the dedicated
/// `cast_source_unsigned` table even with the main hint in place.
///
/// Controls, all of which were already correct and must stay so: every
/// signed instantiation (`i8`/`i16`), and the non-generic `direct` binding
/// whose concrete `u8` the syntactic walk resolves. A fix that flipped
/// signedness blindly would break these.
/// B-2026-08-14-6 — the implicit int-to-float widening reaches a CONTAINER
/// element, its lookup PROBE, and an assignment store, on both surfaces.
///
/// The row's own control is the first three lines, and it is what shows the
/// bug is two bugs. `vc` is pushed a `u8`; `vd` is pushed a genuine `200.0`
/// with no coercion anywhere. Before the fix:
///
/// ```text
///                      interp   compiled
///   vc.contains(200.0)  false    true      <- interp stored an Int
///   vc.contains(v)      true     false
///   vd.contains(v)      false    false     <- NEITHER converts the probe
/// ```
///
/// The `vd` line is the finding: with a real float in the Vec, both
/// surfaces answered false to `contains(some_u8)`. Converting only the
/// interpreter's store would have made them agree on that false — agreement
/// on the wrong answer, since a `u8` 200 is 200.0 and IS in the container.
///
/// The remaining lines are sites found by probing past the row, each
/// measured wrong before: an `Array[f64, N]` index-assign, which wrote the
/// integer's BYTES into a double slot and read back a denormal (LLVM types
/// a store by its value, so nothing complained); and a plain `x = some_u8`
/// on an `f64` local, which took `sitofp` blind to the source's signedness
/// and produced -56.0.
///
/// The `-5i8` lines are the signedness control — a fix that zero-extended
/// blindly would turn them into 251.0. `vf`/`h` are the no-coercion
/// controls: a genuine float element and a genuine float field must be
/// untouched by any of this.
/// B-2026-08-14-8 — `Slice[T]`'s read accessors have codegen.
///
/// `contains` / `first` / `last` / `get` typechecked and interpreted
/// correctly but had no arm in codegen's slice dispatch, so every program
/// using one checked clean and died at `karac build` with "no handler for
/// slice method". `binary_search` / `len` / `is_empty` / `iter` on the same
/// receiver built fine, which is what showed this was missing arms rather
/// than a slice-wide deferral.
///
/// They are ROUTED to `compile_vec_method` over a borrowed `{ptr, len,
/// cap: 0}` view rather than reimplemented. A slice header and a Vec header
/// share their first two fields, and `cap == 0` is the codebase's existing
/// borrowed-view convention, so any method that only reads `ptr`/`len` is
/// already correct against it — including the Option-payload word splitting
/// `first`/`last`/`get` need for a multi-word element, which four
/// hand-written arms would each have had to repeat.
///
/// The `Vec[String]` half is the reason the element type is published under
/// the slice's name for the duration of the call: `compile_vec_method`
/// resolves the element type by VARIABLE NAME, and a slice binding is
/// absent from `vec_elem_types` — it would have silently defaulted to i64
/// and misread the stride for any wider element.
///
/// The trailing `v.len()` / `w.len()` are the aliasing controls: the view
/// borrows the Vec's buffer, so the source containers must be intact and
/// unfreed afterwards. Verified under valgrind as well as here.
///
/// NOT covered, and deliberately so: the mutators and view-producers the
/// typechecker also lists for `Slice` (`fill`/`reverse`/`sort`/
/// `sort_by_key`/`swap`, `chunks`/`windows`/`split_at`). A `cap == 0`
/// header is a lie to anything that would grow or reallocate. They keep the
/// loud error and are filed.
/// B-2026-08-14-10 — a user enum may declare `None`/`Some`/`Ok`/`Err`
/// without making resolution a coin flip.
///
/// `Option` and `Result` are ordinary prelude enums living in the same
/// `HashMap` as the user's, and both the typechecker's bare-name scan and
/// codegen's returned the FIRST match — so with a colliding variant name,
/// `karac check` disagreed with itself run to run (measured 15/30, 10/20,
/// 7/16 in the row), and on the runs it accepted, the module failed the
/// LLVM verifier because the two phases had picked DIFFERENT enums:
/// `ret i64 0` against `{i64, i64, i64, i64}`.
///
/// Every line here is a resolution the two phases must agree on:
///
/// * `Some`/`None` in a fn returning `Option[i64]`, with `Sink.None`
///   declared — the built-in wins, so the program means what it reads as;
/// * `Ok`/`Err` in a fn returning `Result[i64, i64]`, with `Slot.Ok` and
///   `Slot.Err` declared. This leg is here because the row's own `Err`
///   probe reported 40 stable runs and could not explain it: that fixture
///   DECLARED the colliding variant but never used a bare `Err`, so it
///   never exercised the collision. Measured on the stashed compiler, a
///   fixture that does use one is nondeterministic like the rest — 16/30
///   and 11/30 for `Err` and `Ok` — against 30/30 for the declare-only
///   shape;
/// * `Sink.None` qualified — still the user's variant, which is what makes
///   the built-in-wins rule survivable;
/// * `Other(7)` with a user `MyIoErr.Other` against the seeded
///   `IoError.Other(String)` — the USER's wins here, the opposite way, and
///   deliberately: that preference is one codegen already documents, and
///   inverting it would let a prelude type hijack a user's constructor.
///   This line was a coin flip too (6/20 before the fix).
///
/// The last two are why the rule is a fixed four-name table rather than
/// "prelude beats user": both directions are needed, and only the
/// `Option`/`Result` constructors go the built-in way. Determinism alone
/// would not have been enough — sorting the scan makes the answer stable,
/// and stably WRONG for `Other` unless the user/prelude tiering is there
/// too.
#[test]
fn test_e2e_user_enum_shadowing_builtin_variant_names() {
    assert_eq!(
        run_program(
            "enum Sink { None, Open(i64) }\n\
                enum Slot { Ok, Err, Eof }\n\
                enum MyIoErr { Other(i64), Eof }\n\
                fn make(x: i64) -> Option[i64] { if x > 0 { Some(x) } else { None } }\n\
                fn div(x: i64) -> Result[i64, i64] { if x > 0 { Ok(x) } else { Err(0 - 1) } }\n\
                fn main() {\n\
                    match make(1) { Some(v) => println(v), None => println(-1) }\n\
                    match make(0) { Some(v) => println(v), None => println(-1) }\n\
                    match div(2) { Ok(v) => println(v), Err(e) => println(e) }\n\
                    match div(0) { Ok(v) => println(v), Err(e) => println(e) }\n\
                    let s: Sink = Sink.None;\n\
                    match s { None => println(-2), Open(v) => println(v) }\n\
                    let e = Other(7);\n\
                    match e { Other(v) => println(v), Eof => println(-3) }\n\
                }"
        )
        .as_deref(),
        Some("1\n-1\n2\n-1\n-2\n7\n"),
    );
}

#[test]
fn test_e2e_struct_payload_boxed_field_envelope_reads_correctly() {
    // B-2026-08-12-15's offset arithmetic, as a VALUE assertion rather than
    // a leak one. The envelope free walks a FLATTENED enum payload and finds
    // the inner tag at `1 + <words of the fields ahead of the box>`; get
    // that index wrong and the cleanup reads a scalar field as a tag and the
    // tag as a pointer. `V` puts a scalar AHEAD of the box (index 2) and `U`
    // puts one behind it (index 1, the unshifted case the sibling predicate
    // is hardcoded to) — so a drifted index lands on a live value in one
    // direction or the other, and reading the scalars and the payload back
    // proves the walk touched neither.
    //
    // ONE scalar each, which is forced rather than chosen: an `Option` is 4
    // LLVM words against `Result`'s 5-word area, so a second field would push
    // the payload over and box it WHOLE — a different path
    // (`boxed_enum_payload_variants`), and the shape this test is about would
    // not be exercised at all. Measured: the 2-scalar version allocates a
    // 56-byte payload box and never reaches this code.
    assert_eq!(
        run_program(
            "struct V { a: i64, o: Option[Option[i64]] }\n\
                 struct U { o: Option[Option[i64]], z: i64 }\n\
                 fn takev(r: Result[V, i64]) -> i64 {\n\
                     match r {\n\
                         Result.Ok(v) => match v.o {\n\
                             Option.Some(Option.Some(n)) => v.a + n,\n\
                             _ => -1,\n\
                         },\n\
                         Result.Err(e) => e,\n\
                     }\n\
                 }\n\
                 fn takeu(r: Result[U, i64]) -> i64 {\n\
                     match r {\n\
                         Result.Ok(u) => match u.o {\n\
                             Option.Some(Option.Some(n)) => u.z + n,\n\
                             _ => -1,\n\
                         },\n\
                         Result.Err(e) => e,\n\
                     }\n\
                 }\n\
                 fn main() {\n\
                     let bound: Result[V, i64] = \
                         Result.Ok(V { a: 1, o: Option.Some(Option.Some(30)) });\n\
                     println(takev(bound));\n\
                     println(takev(Result.Ok(V { a: 5, o: Option.Some(Option.Some(70)) })));\n\
                     println(takeu(Result.Ok(U { o: Option.Some(Option.Some(400)), z: 9 })));\n\
                     let m: Result[V, i64] = \
                         Result.Ok(V { a: 100, o: Option.Some(Option.Some(11)) });\n\
                     println(match m {\n\
                         Result.Ok(v) => match v.o {\n\
                             Option.Some(Option.Some(n)) => v.a + n,\n\
                             _ => -1,\n\
                         },\n\
                         Result.Err(e) => e,\n\
                     });\n\
                     let absent: Result[V, i64] = Result.Ok(V { a: 7, o: Option.None });\n\
                     println(match absent { Result.Ok(v) => v.a, Result.Err(e) => e });\n\
                     let errside: Result[V, i64] = Result.Err(42);\n\
                     println(match errside { Result.Ok(v) => v.a, Result.Err(e) => e });\n\
                 }"
        )
        .as_deref(),
        Some("31\n75\n409\n111\n7\n42\n"),
    );
}

/// B-2026-08-09-15 — a match arm that RETURNS a Drop-carrying payload out
/// of an owned enum param runs the body TWICE: once at the callee's frame
/// exit and again at the caller's binding. The payload escapes by `return`,
/// so the caller's binding is its sole owner after the call and exactly one
/// body is correct — the expectation below.
///
/// Pinned rather than left to the run-vs-build parity oracle ON PURPOSE.
/// `--interp` used to print the single correct fire here, but only because
/// B-2026-08-09-10's cross-frame name-collision leak happened to suppress
/// the callee's spurious one. Fixing that (frame-scoping the moved-out
/// sets) makes the interpreter double-fire too, so the two backends now
/// AGREE — and a parity gate cannot see a defect both sides share. This
/// test is what keeps it visible; without it, closing -10 would have
/// silently retired the only signal that -15 exists.
///
/// FIXED by extending the caller-side retraction the family already runs on:
/// `fn_returns_param_payload` asks the callee whether a `match` arm hands
/// back something bound OUT of the arg, which `fn_returns_param` cannot see
/// because the param itself reaches no return site. Kept un-ignored as the
/// regression pin — the parity oracle still cannot see this class.
#[test]
fn test_e2e_returned_enum_param_payload_drop_fires_once_still_open() {
    // The `Drop` body READS `name` on purpose. Printing the id alone leaves
    // the payload buffer unobserved, so LLVM deletes the allocation together
    // with its frees and a program that double-frees underneath still passes
    // an output pin — which is exactly how B-2026-08-09-16 hid behind the
    // alias case below. Reading the string keeps the memory live, so this pin
    // and the ASAN fixture describe the same program.
    let hdr = "struct Res { id: i64, name: String }\n\
                   impl Drop for Res {\n\
                   \x20   fn drop(mut ref self) { println(f\"drop {self.id} {self.name}\") }\n\
                   }\n\
                   enum Box2 { Full(Res), Empty }\n";
    assert_eq!(
        run_program(&format!(
            "{hdr}\
                 fn take(b: Box2) -> Res {{\n\
                 \x20   match b {{\n\
                 \x20       Box2.Full(r) => {{ return r; }}\n\
                 \x20       Box2.Empty => {{ return Res {{ id: 0, name: f\"z\" }}; }}\n\
                 \x20   }}\n\
                 }}\n\
                 fn main() {{\n\
                 \x20   let b: Box2 = Box2.Full(Res {{ id: 7, name: f\"e7\" }});\n\
                 \x20   let r: Res = take(b);\n\
                 \x20   println(f\"got {{r.id}}\");\n\
                 }}"
        ))
        .as_deref(),
        Some("got 7\ndrop 7 e7\n")
    );
    // The same escape routed through a `let`. The alias has to be followed:
    // the arm returns `k`, not the binding the pattern introduced, and a
    // predicate that only looks at pattern names answers "does not escape"
    // for the spelling most code actually writes.
    assert_eq!(
        run_program(&format!(
            "{hdr}\
                 fn take(b: Box2) -> Res {{\n\
                 \x20   match b {{\n\
                 \x20       Box2.Full(r) => {{ let k: Res = r; return k; }}\n\
                 \x20       Box2.Empty => {{ return Res {{ id: 0, name: f\"z\" }}; }}\n\
                 \x20   }}\n\
                 }}\n\
                 fn main() {{\n\
                 \x20   let b: Box2 = Box2.Full(Res {{ id: 7, name: f\"e7\" }});\n\
                 \x20   let r: Res = take(b);\n\
                 \x20   println(f\"got {{r.id}}\");\n\
                 }}"
        ))
        .as_deref(),
        Some("got 7\ndrop 7 e7\n")
    );
}

/// B-2026-08-09-15 siblings — the shapes whose payload does NOT escape the
/// callee, and must keep firing caller-side exactly once.
///
/// These are the guard rail on the retraction. The fix keys on "does the
/// callee return something bound out of this arg", and every case here
/// answers no: a payload read through a field projection stays behind, and
/// an unmatched param is never bound out at all. Widening that predicate —
/// or replacing it with an unconditional retraction, which an earlier
/// attempt at this row did — silences all three, and a lost `Drop` body
/// reads as a passing test unless something pins the output.
#[test]
fn test_e2e_nonescaping_enum_arg_payload_still_fires_once() {
    let hdr = "struct Res { id: i64, name: String }\n\
                   impl Drop for Res {\n\
                   \x20   fn drop(mut ref self) { println(f\"drop {self.id}\") }\n\
                   }\n\
                   enum Box2 { Full(Res), Empty }\n";
    // Payload bound out but NOT returned — the callee is the sole owner.
    assert_eq!(
        run_program(&format!(
            "{hdr}\
                 fn take(b: Box2) -> i64 {{\n\
                 \x20   match b {{\n\
                 \x20       Box2.Full(r) => {{ return r.id; }}\n\
                 \x20       Box2.Empty => {{ return 0; }}\n\
                 \x20   }}\n\
                 }}\n\
                 fn main() {{\n\
                 \x20   let b: Box2 = Box2.Full(Res {{ id: 7, name: f\"e7\" }});\n\
                 \x20   let v: i64 = take(b);\n\
                 \x20   println(f\"v={{v}}\");\n\
                 }}"
        ))
        .as_deref(),
        Some("drop 7\nv=7\n")
    );
    // Param NEVER matched — nothing binds the payload out, so the param's
    // own walk is the only fire, at the callee's frame exit.
    assert_eq!(
        run_program(&format!(
            "{hdr}\
                 fn take(b: Box2) -> i64 {{ println(f\"in\"); 1 }}\n\
                 fn main() {{\n\
                 \x20   let b: Box2 = Box2.Full(Res {{ id: 7, name: f\"e7\" }});\n\
                 \x20   let v: i64 = take(b);\n\
                 \x20   println(f\"end {{v}}\");\n\
                 }}"
        ))
        .as_deref(),
        Some("in\ndrop 7\nend 1\n")
    );
    // NO-HEAP payload returned out of a param: the entry copy declines, so
    // codegen's callee-side registration never engages — yet one body, in
    // the caller, is still the answer both backends must print.
    assert_eq!(
        run_program(
            "struct Res2 { id: i64 }\n\
                 impl Drop for Res2 {\n\
                 \x20   fn drop(mut ref self) { println(f\"drop {self.id}\") }\n\
                 }\n\
                 enum Box3 { Full(Res2), Empty }\n\
                 fn take(b: Box3) -> Res2 {\n\
                 \x20   match b {\n\
                 \x20       Box3.Full(r) => { return r; }\n\
                 \x20       Box3.Empty => { return Res2 { id: 0 }; }\n\
                 \x20   }\n\
                 }\n\
                 fn main() {\n\
                 \x20   let b: Box3 = Box3.Full(Res2 { id: 7 });\n\
                 \x20   let r: Res2 = take(b);\n\
                 \x20   println(f\"got {r.id}\");\n\
                 }"
        )
        .as_deref(),
        Some("got 7\ndrop 7\n")
    );
}

/// B-2026-08-29-17 — a `match` / `if let` / `while let` arm that REBINDS a
/// payload bound out of an OWNED-PARAM scrutinee ran that payload's `Drop`
/// body twice, on ALL THREE backends.
///
/// The bind itself was already right: under caller-retains a payload bound
/// out of an owned param is a VIEW of the callee's entry copy, so it
/// registers memory-only and the CALLER fires the body. What was missing is
/// that the view-ness did not PROPAGATE — `let m = r;` inside the arm minted
/// a genuinely owned local, which registered its own body and ran the one
/// the caller was already going to run.
///
/// B-2026-08-01-15 established exactly this rule one level up (`let h2 = h;`
/// over the PARAM itself propagates, and is correct); this is the same rule
/// for a payload bound out of the param. Both backends carry it now —
/// `param_view_locals` in codegen, `owned_param_names_stack` in the
/// interpreter — and they had the identical hole.
///
/// BOTH BACKENDS AGREED ON THE WRONG ANSWER, which is why no A/B gate could
/// see it and why this test asserts an absolute expectation rather than
/// parity. Fixing one side alone would have converted an agreed defect into
/// a divergence — measured mid-fix, when the codegen half landed first.
#[test]
fn test_e2e_rebound_owned_param_payload_drops_once() {
    let hdr = "struct R { id: i64 }\n\
                   impl Drop for R {\n\
                   \x20   fn drop(mut ref self) { println(f\"dR{self.id}\") }\n\
                   }\n\
                   enum Box2 { Full(R), Empty }\n";
    // The defect: owned-param scrutinee, rebind, payload does NOT escape.
    assert_eq!(
        run_program(&format!(
            "{hdr}\
                 fn take(o: Box2) -> i64 {{\n\
                 \x20   match o {{ Box2.Full(r) => {{ let m = r; m.id }} Box2.Empty => {{ 0 }} }}\n\
                 }}\n\
                 fn main() {{\n\
                 \x20   let o: Box2 = Box2.Full(R {{ id: 1 }});\n\
                 \x20   let v = take(o);\n\
                 \x20   println(f\"v={{v}}\");\n\
                 }}"
        ))
        .as_deref(),
        Some("dR1\nv=1\n")
    );
    // `Option` carries it through the same channel.
    assert_eq!(
        run_program(
            "struct R { id: i64 }\n\
                 impl Drop for R {\n\
                 \x20   fn drop(mut ref self) { println(f\"dR{self.id}\") }\n\
                 }\n\
                 fn take(o: Option[R]) -> i64 {\n\
                 \x20   match o { Some(r) => { let m = r; m.id } None => { 0 } }\n\
                 }\n\
                 fn main() {\n\
                 \x20   let o: Option[R] = Some(R { id: 1 });\n\
                 \x20   let v = take(o);\n\
                 \x20   println(f\"v={v}\");\n\
                 }"
        )
        .as_deref(),
        Some("dR1\nv=1\n")
    );
    // `if let` — a separate binding path in the interpreter, and it needed
    // its own leg. B-2026-08-28-63 already had to close a spelling-dependent
    // split in this family once.
    assert_eq!(
        run_program(&format!(
            "{hdr}\
                 fn take(o: Box2) -> i64 {{\n\
                 \x20   if let Box2.Full(r) = o {{ let m = r; m.id }} else {{ 0 }}\n\
                 }}\n\
                 fn main() {{\n\
                 \x20   let o: Box2 = Box2.Full(R {{ id: 1 }});\n\
                 \x20   let v = take(o);\n\
                 \x20   println(f\"v={{v}}\");\n\
                 }}"
        ))
        .as_deref(),
        Some("dR1\nv=1\n")
    );
    // TRANSITIVE — two levels of rebind.
    assert_eq!(
        run_program(&format!(
            "{hdr}\
                 fn take(o: Box2) -> i64 {{\n\
                 \x20   match o {{\n\
                 \x20       Box2.Full(r) => {{ let m = r; let n = m; n.id }}\n\
                 \x20       Box2.Empty => {{ 0 }}\n\
                 \x20   }}\n\
                 }}\n\
                 fn main() {{\n\
                 \x20   let o: Box2 = Box2.Full(R {{ id: 1 }});\n\
                 \x20   let v = take(o);\n\
                 \x20   println(f\"v={{v}}\");\n\
                 }}"
        ))
        .as_deref(),
        Some("dR1\nv=1\n")
    );
}

/// B-2026-08-29-19 — WRAPPING a param view into a fresh enum runs the
/// payload's `Drop` body TWICE, on all three backends.
///
///     fn take(r: R) -> i64 { let w = W.One(r); 7 }   // dR1 dR1
///
/// Under caller-retains a value moved out of an owned param is a VIEW: the
/// caller runs its body. B-2026-08-01-15 propagates that across a plain
/// rebind and B-2026-08-29-17 across a match-arm rebind, but a CONSTRUCTOR
/// — the same move one level up — was never covered, so the fresh binding
/// armed a `__karac_dropelems_enum_<E>` walk on top of the caller's fire.
///
/// THE ROW THAT FILED THIS DESCRIBED A LARGER SHAPE than the defect needs.
/// It was found as `let m = r; let w = W.One(m); match w { .. }` and framed
/// as being about the rebind and the re-match; both are red herrings. The
/// third case below is the whole bug with neither of them — no rebind, no
/// second match, no enum payload, no `match` at all — and the row's own
/// shape is a consequence. It also guessed the move-out retraction fails to
/// run for a view; it runs, and correctly finds nothing to retract, because
/// a view never had a registration. The surplus is the WRAPPER's walk.
///
/// Cases 5-8 are the guard rails, and they are the reason the fix is a
/// conditional rather than a blanket retraction:
///
///  * 5 wraps a genuine LOCAL. Its body is the callee's to run and stays.
///  * 6 constructs a FRESH payload inside the wrap — no view anywhere.
///  * 7 is MIXED (`W2.Two(r, R { id: 2 })`), one view and one fresh sharing
///    a single walker. Pinned at the DEFECT's three bodies deliberately:
///    the walk cannot be dropped per-slot, so suppressing it wholesale
///    would trade this double for a MISSING body on the fresh payload —
///    the same severity, the other direction. Tracked as B-2026-08-29-24;
///    when a masked walker lands, this line becomes `dR2\ndR1\nv=7`.
///  * 8 ESCAPES the wrap by returning it, where the callee is not the owner
///    and nothing may be withheld.
///
/// ASAN/LSan are clean on all of these INCLUDING a heap-carrying payload
/// (measured — the row left it open): the frees were balanced before and
/// after, so this was only ever a body-count defect.
///
/// Every backend agreed on the wrong answer, so no A/B parity gate could
/// see it and the expectation has to be absolute. Case 4 is the exception
/// that proves it — `let w2 = w;` over an enum PARAM was additionally a
/// run-vs-build divergence, interp at one body and both compiled backends
/// at two, with the interpreter already right.
#[test]
fn test_e2e_wrapped_param_view_payload_drops_once() {
    let hdr = "struct R { id: i64 }\n\
                   impl Drop for R {\n\
                   \x20   fn drop(mut ref self) { println(f\"dR{self.id}\") }\n\
                   }\n\
                   enum Box2 { Full(R), Empty }\n\
                   enum W { One(R), None2 }\n";
    // 1. The row's exact shape: rebind, re-wrap, re-match.
    assert_eq!(
        run_program(&format!(
            "{hdr}\
                 fn take(o: Box2) -> i64 {{\n\
                 \x20   match o {{\n\
                 \x20       Box2.Full(r) => {{\n\
                 \x20           let m = r;\n\
                 \x20           let w = W.One(m);\n\
                 \x20           match w {{ W.One(z) => {{ z.id }} W.None2 => {{ 0 }} }}\n\
                 \x20       }}\n\
                 \x20       Box2.Empty => {{ 0 }}\n\
                 \x20   }}\n\
                 }}\n\
                 fn main() {{\n\
                 \x20   let o: Box2 = Box2.Full(R {{ id: 1 }});\n\
                 \x20   let v = take(o);\n\
                 \x20   println(f\"v={{v}}\");\n\
                 }}"
        ))
        .as_deref(),
        Some("dR1\nv=1\n")
    );
    // 2. Neither the rebind nor the second match is load-bearing.
    assert_eq!(
            run_program(&format!(
                "{hdr}\
                 fn take(o: Box2) -> i64 {{\n\
                 \x20   match o {{ Box2.Full(r) => {{ let w = W.One(r); 7 }} Box2.Empty => {{ 0 }} }}\n\
                 }}\n\
                 fn main() {{\n\
                 \x20   let o: Box2 = Box2.Full(R {{ id: 1 }});\n\
                 \x20   let v = take(o);\n\
                 \x20   println(f\"v={{v}}\");\n\
                 }}"
            ))
            .as_deref(),
            Some("dR1\nv=7\n")
        );
    // 3. The whole defect with no enum payload and no `match` at all —
    //    the minimal shape, and the one that shows the row's framing was
    //    incidental.
    assert_eq!(
        run_program(&format!(
            "{hdr}\
                 fn take(r: R) -> i64 {{ let w = W.One(r); 7 }}\n\
                 fn main() {{\n\
                 \x20   let v = take(R {{ id: 1 }});\n\
                 \x20   println(f\"v={{v}}\");\n\
                 }}"
        ))
        .as_deref(),
        Some("dR1\nv=7\n")
    );
    // 4. The bare rebind of an enum PARAM: the divergent leg (interp was
    //    already at one body, both compiled backends at two).
    assert_eq!(
        run_program(&format!(
            "{hdr}\
                 fn take(w: W) -> i64 {{ let w2 = w; 7 }}\n\
                 fn main() {{\n\
                 \x20   let v = take(W.One(R {{ id: 1 }}));\n\
                 \x20   println(f\"v={{v}}\");\n\
                 }}"
        ))
        .as_deref(),
        Some("dR1\nv=7\n")
    );
    // 4b. View-ness must propagate THROUGH the wrap, or the chained rebind
    //     re-arms a walker the wrap just withheld.
    assert_eq!(
        run_program(&format!(
            "{hdr}\
                 fn take(r: R) -> i64 {{ let w = W.One(r); let w2 = w; 7 }}\n\
                 fn main() {{\n\
                 \x20   let v = take(R {{ id: 1 }});\n\
                 \x20   println(f\"v={{v}}\");\n\
                 }}"
        ))
        .as_deref(),
        Some("dR1\nv=7\n")
    );
    // 5. GUARD — a genuine local's body is the callee's to run.
    assert_eq!(
        run_program(&format!(
            "{hdr}\
                 fn take() -> i64 {{\n\
                 \x20   let m = R {{ id: 1 }};\n\
                 \x20   let w = W.One(m);\n\
                 \x20   match w {{ W.One(z) => {{ z.id }} W.None2 => {{ 0 }} }}\n\
                 }}\n\
                 fn main() {{ let v = take(); println(f\"v={{v}}\"); }}"
        ))
        .as_deref(),
        Some("dR1\nv=1\n")
    );
    // 6. GUARD — a payload constructed FRESH inside the wrap.
    assert_eq!(
        run_program(&format!(
            "{hdr}\
                 fn take(k: i64) -> i64 {{ let w = W.One(R {{ id: k }}); 7 }}\n\
                 fn main() {{ let v = take(1); println(f\"v={{v}}\"); }}"
        ))
        .as_deref(),
        Some("dR1\nv=7\n")
    );
    // 7. MIXED payloads. One walker covered both slots, so this fix could
    //    only arm or withhold it whole and left the row at three bodies;
    //    B-2026-08-29-24's per-slot mask
    //    (`emit_enum_payload_user_drop_bodies_fn_skipping`) masks the view
    //    slot alone, so the fresh payload keeps its body (`dR2`) and the
    //    view's returns to the caller (`dR1`). Was `dR1 dR2 dR1`.
    assert_eq!(
        run_program(
            "struct R { id: i64 }\n\
                 impl Drop for R {\n\
                 \x20   fn drop(mut ref self) { println(f\"dR{self.id}\") }\n\
                 }\n\
                 enum W2 { Two(R, R), None3 }\n\
                 fn take(r: R) -> i64 { let w = W2.Two(r, R { id: 2 }); 7 }\n\
                 fn main() { let v = take(R { id: 1 }); println(f\"v={v}\"); }"
        )
        .as_deref(),
        Some("dR2\ndR1\nv=7\n")
    );
    // 8. GUARD — the wrap ESCAPES, so the callee owns nothing to withhold.
    assert_eq!(
        run_program(&format!(
            "{hdr}\
                 fn take(o: Box2) -> W {{\n\
                 \x20   match o {{ Box2.Full(r) => {{ W.One(r) }} Box2.Empty => {{ W.None2 }} }}\n\
                 }}\n\
                 fn main() {{\n\
                 \x20   let o: Box2 = Box2.Full(R {{ id: 1 }});\n\
                 \x20   let w = take(o);\n\
                 \x20   println(\"mid\");\n\
                 }}"
        ))
        .as_deref(),
        Some("dR1\nmid\n")
    );
}

#[test]
fn e2e_wrapped_param_view_nonenum_and_mixed_wraps_drop_once() {
    let hdr = "struct R { id: i64 }\n\
                   impl Drop for R {\n\
                   \x20   fn drop(mut ref self) { println(f\"dR{self.id}\") }\n\
                   }\n\
                   enum W2 { Two(R, R), None3 }\n\
                   struct S { r: R }\n\
                   struct S2 { r: R, k: i64 }\n\
                   struct S3 { a: R, b: R }\n\
                   struct Sd { r: R }\n\
                   impl Drop for Sd {\n\
                   \x20   fn drop(mut ref self) { println(\"dSd\") }\n\
                   }\n";
    // (label, body of `take`, expected output)
    for (label, body, want) in [
        // ── the wrap kinds B-2026-08-29-19 left doubling ───────────────
        ("struct-literal", "let s = S { r: r };", "dR1\nv=7\n"),
        // An inert sibling field must not change the answer: the mask is
        // keyed on the walker's visited indices, and `k` is not one.
        (
            "struct-literal-inert-field",
            "let s = S2 { r: r, k: 9 };",
            "dR1\nv=7\n",
        ),
        ("tuple-literal", "let t = (r, 5);", "dR1\nv=7\n"),
        ("option-wrap", "let q = Some(r);", "dR1\nv=7\n"),
        // A struct with its OWN `impl Drop` has no per-binding field
        // walker — the bodies run inside the type-level `karac_drop_<T>`
        // wrapper — so this goes through the wholesale wrapper swap
        // instead. Its own body still fires; only the view's is withheld.
        (
            "owndrop-struct-all-views",
            "let s = Sd { r: r };",
            "dSd\ndR1\nv=7\n",
        ),
        // ── MIXED wraps: one view, one fresh, one walker ───────────────
        // The fresh payload keeps its body, the view's returns to the
        // caller. Getting this wrong in the other direction — dropping the
        // walker wholesale — is the reason B-2026-08-29-19 declined these.
        (
            "mixed-enum",
            "let w = W2.Two(r, R { id: 2 });",
            "dR2\ndR1\nv=7\n",
        ),
        (
            "mixed-struct",
            "let s = S3 { a: r, b: R { id: 2 } };",
            "dR2\ndR1\nv=7\n",
        ),
        (
            "mixed-tuple",
            "let t = (r, R { id: 2 });",
            "dR2\ndR1\nv=7\n",
        ),
        // ── view-ness propagates THROUGH the wrap ──────────────────────
        // Or the rebind re-arms a full walk over the fields just masked —
        // the same propagation B-2026-08-01-15 does for a plain rebind,
        // reached through a literal.
        (
            "struct-then-rebind",
            "let s = S { r: r }; let s2 = s;",
            "dR1\nv=7\n",
        ),
    ] {
        assert_eq!(
            run_program(&format!(
                "{hdr}\
                     fn take(r: R) -> i64 {{ {body} 7 }}\n\
                     fn main() {{ let v = take(R {{ id: 1 }}); println(f\"v={{v}}\"); }}"
            ))
            .as_deref(),
            Some(want),
            "case {label}"
        );
    }
    // GUARD — a payload constructed FRESH in each wrap kind: no caller owns
    // it, so the callee's walk is the ONLY fire and must stay armed. An
    // over-broad mask silences all of these, and a lost body reads as a
    // passing test unless something pins the output.
    for (label, body, want) in [
        (
            "guard-fresh-struct",
            "let s = S { r: R { id: 3 } };",
            "dR3\nv=7\n",
        ),
        (
            "guard-fresh-tuple",
            "let t = (R { id: 3 }, 5);",
            "dR3\nv=7\n",
        ),
        (
            "guard-fresh-option",
            "let q = Some(R { id: 3 });",
            "dR3\nv=7\n",
        ),
        (
            "guard-fresh-owndrop",
            "let s = Sd { r: R { id: 3 } };",
            "dSd\ndR3\nv=7\n",
        ),
    ] {
        assert_eq!(
            run_program(&format!(
                "{hdr}\
                     fn take(k: i64) -> i64 {{ {body} 7 }}\n\
                     fn main() {{ let v = take(3); println(f\"v={{v}}\"); }}"
            ))
            .as_deref(),
            Some(want),
            "case {label}"
        );
    }
    // GUARD — a genuine LOCAL wrapped. The rule keys on the source being an
    // owned param, which is exactly when some caller fires instead; a local
    // fails that condition in every wrap kind and keeps its body here.
    for (label, body, want) in [
        ("guard-local-struct", "let s = S { r: m };", "dR4\nv=7\n"),
        ("guard-local-tuple", "let t = (m, 5);", "dR4\nv=7\n"),
        ("guard-local-option", "let q = Some(m);", "dR4\nv=7\n"),
    ] {
        assert_eq!(
            run_program(&format!(
                "{hdr}\
                     fn take() -> i64 {{ let m = R {{ id: 4 }}; {body} 7 }}\n\
                     fn main() {{ let v = take(); println(f\"v={{v}}\"); }}"
            ))
            .as_deref(),
            Some(want),
            "case {label}"
        );
    }
    // GUARD — the wrap ESCAPES, so the callee owns nothing to withhold and
    // the caller's own fire is the single body.
    assert_eq!(
        run_program(&format!(
            "{hdr}\
                 fn take(r: R) -> S {{ S {{ r: r }} }}\n\
                 fn main() {{ let s = take(R {{ id: 5 }}); println(\"mid\"); }}"
        ))
        .as_deref(),
        Some("dR5\nmid\n")
    );
    // FIXED (B-2026-08-29-43). This was PINNED AT THE DEFECT here —
    // `dSd3 dR2 dR1 dR1`, agreed across all three backends — on the
    // reasoning that an own-`Drop` struct's field bodies run inside the
    // type-level `karac_drop_<T>` wrapper, whose only surgery was the
    // all-or-nothing `karac_dropnf_<T>` swap; masking one slot with that
    // would have cost the FRESH field's body, the same severity in the
    // other direction. `emit_user_drop_wrapper_skipping` (B-2026-08-28-21)
    // is the per-field variant that did not exist when this was declined —
    // it masks only the BODY step and leaves every field's memory — so the
    // binding now gets a wrapper with just the view's body withheld. The
    // interpreter's matching bail came out in the same commit, which is
    // what keeps the two agreed rather than trading a shared defect for a
    // divergence.
    assert_eq!(
        run_program(&format!(
            "{hdr}\
                 struct Sd3 {{ a: R, b: R }}\n\
                 impl Drop for Sd3 {{\n\
                 \x20   fn drop(mut ref self) {{ println(\"dSd3\") }}\n\
                 }}\n\
                 fn take(r: R) -> i64 {{ let s = Sd3 {{ a: r, b: R {{ id: 2 }} }}; 7 }}\n\
                 fn main() {{ let v = take(R {{ id: 1 }}); println(f\"v={{v}}\"); }}"
        ))
        .as_deref(),
        Some("dSd3\ndR2\ndR1\nv=7\n")
    );
    // A whole-value REBIND after a MIXED wrap used to re-arm the full
    // walk: the per-slot mask was keyed on the binding and derived from
    // the constructor expression, so the destination got a fresh
    // registration with no mask. FIXED by B-2026-08-31-50: the slots are
    // stored per binding (`enum_ctor_moved_payload_slots`) and a rebind
    // inherits them, on both backends.
    assert_eq!(
        run_program(&format!(
            "{hdr}\
                 fn take(r: R) -> i64 {{ let w = W2.Two(r, R {{ id: 2 }}); let w2 = w; 7 }}\n\
                 fn main() {{ let v = take(R {{ id: 1 }}); println(f\"v={{v}}\"); }}"
        ))
        .as_deref(),
        Some("dR2\ndR1\nv=7\n")
    );
    // A `Vec` literal doubled as well, and the row that prompted THIS fix
    // listed it beside the others — but it was NOT the same defect: a plain
    // LOCAL moved into one doubled identically, with no param anywhere, so
    // the cause was a move-suppression hole rather than caller-retains.
    // Both spellings stay pinned, because the pair is what showed which bug
    // it was.
    //
    // NOW FIXED under B-2026-08-29-45, which needed BOTH halves — the
    // move-suppression retraction for the local source and this file's own
    // caller-retains treatment (`container_literal_elems_are_all_param_-
    // views`) for the param one. The expectations below were the
    // known-bad output this fixture deliberately recorded; they are now the
    // correct single body. The MIXED literal (`[r, R { id: 2 }]`) stays
    // unfixed by design — see that row's close note for why a `Vec` cannot
    // carry the per-slot mask a tuple can.
    assert_eq!(
        run_program(&format!(
            "{hdr}\
                 fn take(r: R) -> i64 {{ let v2 = [r]; 7 }}\n\
                 fn main() {{ let v = take(R {{ id: 1 }}); println(f\"v={{v}}\"); }}"
        ))
        .as_deref(),
        Some("dR1\nv=7\n")
    );
    assert_eq!(
        run_program(&format!(
            "{hdr}\
                 fn take() -> i64 {{ let m = R {{ id: 4 }}; let v2 = [m]; 7 }}\n\
                 fn main() {{ let v = take(); println(f\"v={{v}}\"); }}"
        ))
        .as_deref(),
        Some("dR4\nv=7\n")
    );
    // ...and the same Vec literal with a FRESH element is correct, which is
    // what makes the two above a move-suppression hole and not a walker
    // that fires twice unconditionally.
    assert_eq!(
        run_program(&format!(
            "{hdr}\
                 fn take() -> i64 {{ let v2 = [R {{ id: 4 }}]; 7 }}\n\
                 fn main() {{ let v = take(); println(f\"v={{v}}\"); }}"
        ))
        .as_deref(),
        Some("dR4\nv=7\n")
    );
    // Moving a param view back OUT of the struct it was just wrapped into.
    // The move-out hands the field's body to `x` correctly; what `x` used
    // to inherit nothing of is the field having been a VIEW, so it
    // registered a body the caller also ran. FIXED (B-2026-08-29-47) — this
    // expectation was the recorded known-bad `dR1 dR1` and is now the due
    // single body. The LOCAL-source twin below never moved, which is what
    // isolated view-ness rather than the move-out machinery as the cause.
    assert_eq!(
        run_program(&format!(
            "{hdr}\
                 fn take(r: R) -> i64 {{ let s = S {{ r: r }}; let x = s.r; 7 }}\n\
                 fn main() {{ let v = take(R {{ id: 1 }}); println(f\"v={{v}}\"); }}"
        ))
        .as_deref(),
        Some("dR1\nv=7\n")
    );
    assert_eq!(
        run_program(&format!(
            "{hdr}\
                 fn take() -> i64 {{ let s = S {{ r: R {{ id: 1 }} }}; let x = s.r; 7 }}\n\
                 fn main() {{ let v = take(); println(f\"v={{v}}\"); }}"
        ))
        .as_deref(),
        Some("dR1\nv=7\n")
    );
    // TWO owned params, no wrap at all: both backends drop the caller's
    // fresh temps in REVERSE argument order. This side was always right —
    // the temps ride a cleanup frame that drains LIFO — and it pinned the
    // compiled half of what was B-2026-08-29-46 while `--interp` walked the
    // argument list forward. Kept here as the wrap-free neighbour of the
    // cases above; the full surface lives in
    // `e2e_owned_param_temps_drop_in_reverse_argument_order`.
    assert_eq!(
        run_program(&format!(
            "{hdr}\
                 fn take(r: R, q: R) -> i64 {{ 7 }}\n\
                 fn main() {{\n\
                 \x20   let v = take(R {{ id: 1 }}, R {{ id: 2 }});\n\
                 \x20   println(f\"v={{v}}\");\n\
                 }}"
        ))
        .as_deref(),
        Some("dR2\ndR1\nv=7\n")
    );
}

/// B-2026-08-29-17's guard rails — the shapes where the arm binding really
/// IS the only owner, and whose body the view propagation must not silence.
///
/// The fix keys on the scrutinee being an OWNED PARAM, which is exactly the
/// condition under which some caller fires instead. Every case here fails
/// that condition, so every one must keep its body. An over-broad
/// propagation silences all of them, and a lost `Drop` body reads as a
/// passing test unless something pins the output.
#[test]
fn test_e2e_rebound_non_param_payload_still_fires_once() {
    let hdr = "struct R { id: i64 }\n\
                   impl Drop for R {\n\
                   \x20   fn drop(mut ref self) { println(f\"dR{self.id}\") }\n\
                   }\n\
                   enum Box2 { Full(R), Empty }\n";
    // LOCAL scrutinee — no caller owns it, so the rebind must keep the body.
    assert_eq!(
            run_program(&format!(
                "{hdr}\
                 fn main() {{\n\
                 \x20   let o: Box2 = Box2.Full(R {{ id: 1 }});\n\
                 \x20   let v = match o {{ Box2.Full(r) => {{ let m = r; m.id }} Box2.Empty => {{ 0 }} }};\n\
                 \x20   println(f\"v={{v}}\");\n\
                 \x20   println(\"end\");\n\
                 }}"
            ))
            .as_deref(),
            Some("dR1\nv=1\nend\n")
        );
    // FRESH-TEMP scrutinee — the match owns the value outright.
    assert_eq!(
            run_program(&format!(
                "{hdr}\
                 fn mk() -> Box2 {{ Box2.Full(R {{ id: 1 }}) }}\n\
                 fn main() {{\n\
                 \x20   let v = match mk() {{ Box2.Full(r) => {{ let m = r; m.id }} Box2.Empty => {{ 0 }} }};\n\
                 \x20   println(f\"v={{v}}\");\n\
                 }}"
            ))
            .as_deref(),
            Some("dR1\nv=1\n")
        );
    // A SECOND owned param that is never matched keeps its own caller-side
    // fire — the propagation must be per-binding, not per-frame. Two values
    // are constructed here, so two bodies are correct.
    assert_eq!(
            run_program(&format!(
                "{hdr}\
                 fn take(o: Box2, p: R) -> i64 {{\n\
                 \x20   let s = match o {{ Box2.Full(r) => {{ let m = r; m.id }} Box2.Empty => {{ 0 }} }};\n\
                 \x20   s + p.id\n\
                 }}\n\
                 fn main() {{\n\
                 \x20   let o: Box2 = Box2.Full(R {{ id: 1 }});\n\
                 \x20   let v = take(o, R {{ id: 2 }});\n\
                 \x20   println(f\"v={{v}}\");\n\
                 }}"
            ))
            .as_deref(),
            Some("dR2\ndR1\nv=3\n")
        );
    // The rebind ESCAPES as the function's return value — the caller's
    // binding is then the sole owner and fires exactly once, there.
    assert_eq!(
            run_program(&format!(
                "{hdr}\
                 fn take(o: Box2) -> R {{\n\
                 \x20   match o {{ Box2.Full(r) => {{ let m = r; m }} Box2.Empty => {{ R {{ id: 0 }} }} }}\n\
                 }}\n\
                 fn main() {{\n\
                 \x20   let o: Box2 = Box2.Full(R {{ id: 1 }});\n\
                 \x20   let k = take(o);\n\
                 \x20   println(f\"k={{k.id}}\");\n\
                 }}"
            ))
            .as_deref(),
            Some("k=1\ndR1\n")
        );
}

/// B-2026-08-09-15, METHOD spelling — the same doubling one call shape over.
///
/// `t.take(b)` reaches a different arg loop than `take(b)`, so the retraction
/// has to be wired there too, and it is worth its own test because of the
/// index. The method path counts the receiver as declared slot 0 while the
/// AST keeps `self` out of `params` entirely — an off-by-one reads the wrong
/// param, or past the end of a one-arg method, and quietly answers "does not
/// escape" for every method in the program without failing anything. That is
/// exactly what happened on the first wiring, and only instrumentation
/// caught it; these expectations are what make it fail out loud instead.
#[test]
fn test_e2e_returned_enum_arg_payload_method_spelling_fires_once() {
    let hdr = "struct Res { id: i64, name: String }\n\
                   impl Drop for Res {\n\
                   \x20   fn drop(mut ref self) { println(f\"drop {self.id} {self.name}\") }\n\
                   }\n\
                   enum Box2 { Full(Res), Empty }\n\
                   struct Taker { tag: i64 }\n";
    // Arm returns the payload out of the method.
    assert_eq!(
        run_program(&format!(
            "{hdr}\
                 impl Taker {{\n\
                 \x20   fn take(ref self, b: Box2) -> Res {{\n\
                 \x20       match b {{\n\
                 \x20           Box2.Full(r) => {{ return r; }}\n\
                 \x20           Box2.Empty => {{ return Res {{ id: 0, name: f\"z\" }}; }}\n\
                 \x20       }}\n\
                 \x20   }}\n\
                 }}\n\
                 fn main() {{\n\
                 \x20   let t: Taker = Taker {{ tag: 1 }};\n\
                 \x20   let b: Box2 = Box2.Full(Res {{ id: 7, name: f\"e7\" }});\n\
                 \x20   let r: Res = t.take(b);\n\
                 \x20   println(f\"got {{r.id}}\");\n\
                 }}"
        ))
        .as_deref(),
        Some("got 7\ndrop 7 e7\n")
    );
    // The `let`-alias spelling of the same method. It used to abort with
    // `free(): double free detected in tcache 2` and was carved out of this
    // test as B-2026-08-09-16; that row is now fixed (the `let` move retracts
    // the source's memory action, not only its body), so the shape belongs
    // back here where the retraction's index arithmetic is under test.
    assert_eq!(
        run_program(&format!(
            "{hdr}\
                 impl Taker {{\n\
                 \x20   fn take(ref self, b: Box2) -> Res {{\n\
                 \x20       match b {{\n\
                 \x20           Box2.Full(r) => {{ let k: Res = r; return k; }}\n\
                 \x20           Box2.Empty => {{ return Res {{ id: 0, name: f\"z\" }}; }}\n\
                 \x20       }}\n\
                 \x20   }}\n\
                 }}\n\
                 fn main() {{\n\
                 \x20   let t: Taker = Taker {{ tag: 1 }};\n\
                 \x20   let b: Box2 = Box2.Full(Res {{ id: 7, name: f\"e7\" }});\n\
                 \x20   let r: Res = t.take(b);\n\
                 \x20   println(f\"got {{r.id}}\");\n\
                 }}"
        ))
        .as_deref(),
        Some("got 7\ndrop 7 e7\n")
    );
    // Method never matches the param — its own walk is the only fire.
    assert_eq!(
        run_program(&format!(
            "{hdr}\
                 impl Taker {{ fn take(ref self, b: Box2) -> i64 {{ println(f\"in\"); 1 }} }}\n\
                 fn main() {{\n\
                 \x20   let t: Taker = Taker {{ tag: 1 }};\n\
                 \x20   let b: Box2 = Box2.Full(Res {{ id: 7, name: f\"e7\" }});\n\
                 \x20   let v: i64 = t.take(b);\n\
                 \x20   println(f\"end {{v}}\");\n\
                 }}"
        ))
        .as_deref(),
        Some("in\ndrop 7 e7\nend 1\n")
    );
}

#[test]
fn test_e2e_f16_bf16_enum_payload_pack_unpack() {
    // B-2026-07-20-12 — an f16/bf16 enum payload bound out of a match
    // stayed a RAW i64 word (its bit pattern), so arithmetic on it read
    // the pattern as an integer VALUE: `Some((1.5f16, 2.5f16))` printed
    // 32512 (0x3E00 + 0x4100, an int add of bit patterns) and the single
    // form printed 15874.5 (sitofp 0x3E00 + 2.5). Two typechecker
    // omissions (float_surface_name and type_to_type_expr skipped
    // F16/BF16) plus the f32-only width split at the codegen float
    // pack/unpack sites (now exact-width via `float_bits_int_type`:
    // f16/bf16 ↔ i16). Covers: compound tuple binding (p.0 + p.1),
    // tuple destructure `Some((a, b))`, runtime-value pack through a fn
    // (non-const path), single payload, bf16, and Result[f16, String].
    // Captured (not just stdout) so a divergence reports the child's
    // exit status + stderr: an empty stdout alone can't distinguish a
    // runtime crash (signal, block-buffered output lost) from a clean
    // exit that printed nothing — the exact ambiguity that stalled the
    // macOS-arm64 investigation of B-2026-07-22-1.
    if let Some(run) = run_program_capturing(
            "fn mk(x: f16, y: f16) -> Option[(f16, f16)] {\n\
                 Some((x, y))\n\
             }\n\
             fn mk1(x: f16) -> Option[f16] {\n\
                 if x > 0.0f16 { Some(x) } else { None }\n\
             }\n\
             fn mkb(x: bf16) -> Option[bf16] {\n\
                 Some(x)\n\
             }\n\
             fn main() {\n\
                 let o: Option[(f16, f16)] = Some((1.5f16, 2.5f16));\n\
                 match o { Some(p) => println(p.0 + p.1), None => println(0.0f16) }\n\
                 match mk(0.25f16, 0.75f16) { Some((a, b)) => println(a + b), None => println(0.0f16) }\n\
                 match mk1(3.5f16) { Some(v) => println(v), None => println(0.0f16) }\n\
                 match mkb(1.25bf16) { Some(v) => println(v + 0.5bf16), None => println(0.0bf16) }\n\
                 let r: Result[f16, String] = Ok(2.0f16);\n\
                 match r { Ok(v) => println(v * 3.0f16), Err(e) => println(e) }\n\
             }",
        ) {
            assert_eq!(
                run.stdout,
                "4\n1\n3.5\n1.75\n6\n",
                "f16/bf16 payload program diverged (status: {}, stderr: {:?})",
                run.status,
                run.stderr
            );
        }
}

#[test]
fn test_e2e_shared_enum_struct_payload_with_aggregate_fields() {
    // #37 (phase-12 self-hosting, parser stage): constructing/matching a
    // shared-enum variant whose payload struct has SUB-AGGREGATE fields — a
    // unit enum stored as `{i64}` (here `BinOp`) and a nested `{4×i64}`
    // struct (`Span`) — failed LLVM verification (`Invalid InsertValueInst`):
    // the payload reconstruction inserted a bare `i64` into a `{i64}` /
    // `{4×i64}` slot. `reconstruct_payload_value` (`control_flow_match.rs`)
    // now routes a single-WORD struct field through the struct branch
    // (wrapping the word) instead of the scalar branch. This is the AST's
    // `Expr.Binary(BinaryExpr { op: BinOp, left, right, span: Span })` shape.
    if let Some(out) = run_program(
            "struct Sp { line: i64, column: i64, offset: i64, length: i64 }\n\
             enum Bop { Add, Sub }\n\
             struct IntLit { value: i64, span: Sp }\n\
             struct BinE { op: Bop, left: Expr, right: Expr, span: Sp }\n\
             shared enum Expr { Int(IntLit), Bin(BinE) }\n\
             fn eval(e: Expr) -> i64 {\n\
                 match e {\n\
                     Int(n) => n.value,\n\
                     Bin(b) => b.span.length,\n\
                 }\n\
             }\n\
             fn main() {\n\
                 let lf = Expr.Int(IntLit { value: 1, span: Sp { line: 1, column: 1, offset: 0, length: 1 } });\n\
                 let rt = Expr.Int(IntLit { value: 2, span: Sp { line: 1, column: 1, offset: 2, length: 1 } });\n\
                 let b = Expr.Bin(BinE { op: Bop.Add, left: lf, right: rt, span: Sp { line: 1, column: 1, offset: 0, length: 3 } });\n\
                 println(eval(b).to_string());\n\
                 let a = Expr.Int(IntLit { value: 5, span: Sp { line: 1, column: 1, offset: 0, length: 1 } });\n\
                 println(eval(a).to_string());\n\
             }",
        ) {
            assert_eq!(out, "3\n5\n");
        }
}

#[test]
fn e2e_unit_payload_ok_some_codegen() {
    // The empty-tuple literal `()` now types as `Type::Unit`, so
    // `fn f() -> Result[(), i64] { Ok(()) }` typechecks and reaches codegen
    // (it failed at typecheck before the unit-payload fix). Verify the
    // newly-enabled path compiles + runs: construct, `?`-propagate, and
    // match on `Ok(())` / `Some(())`.
    if let Some(out) = run_program(
        "fn unit_ok() -> Result[(), i64] { Ok(()) }\n\
             fn unit_some() -> Option[()] { Some(()) }\n\
             fn build() -> Result[(), i64] {\n\
                 unit_ok()?;\n\
                 Ok(())\n\
             }\n\
             fn main() {\n\
                 match build() {\n\
                     Ok(_) => println(\"ok\"),\n\
                     Err(_) => println(\"err\"),\n\
                 }\n\
                 match unit_some() {\n\
                     Some(_) => println(\"some\"),\n\
                     None => println(\"none\"),\n\
                 }\n\
             }",
    ) {
        assert_eq!(out, "ok\nsome\n");
    }
}

#[test]
fn e2e_freshtemp_enum_scrutinee_runs_user_drop() {
    // B-2026-07-11-26: a fresh-temp enum scrutinee whose type has a user
    // `impl Drop` must RUN that Drop in if-let / while-let / let-else /
    // match — pre-fix `materialize_freshtemp_enum_scrutinee` gated on a
    // heap payload, so an all-scalar user-Drop enum was never materialized
    // and its Drop was silently skipped (a plain `let s = next(0)` binding
    // ran it).
    //
    // B-2026-08-29-28 — the expectation moved from `Y 0 / AFTER / DROP` to
    // `Y 0 / DROP / AFTER`. This test's own comment used to end "the user
    // Drop fires at enclosing-scope exit here (the slice-3 arm-boundary
    // timing polish stays deferred)", so it was pinning a placement it
    // already recorded as provisional; that polish has now landed and the
    // assertion follows it. design.md § `if let` and `let...else`,
    // Scrutinee temporary scope: "In the then arm of `if let`: scrutinee
    // temporaries ... drop at the arm's exit, BEFORE any cleanup in the
    // surrounding scope." Three independent things agree on the new order —
    // that sentence, the interpreter (which was already emitting it, and is
    // the parity oracle), and the sibling
    // `e2e_freshtemp_enum_scrutinee_while_let_runs_user_drop_per_iter`,
    // which has always expected the per-iteration placement and passed
    // through this change unmodified.
    if let Some(out) = run_program(
            "enum Step { Yield(i64), Stop }\n\
             impl Drop for Step { fn drop(mut ref self) { println(\"DROP\"); } }\n\
             fn next(i: i64) -> Step { if i < 3 { Step.Yield(i) } else { Step.Stop } }\n\
             fn main() {\n\
                 if let Step.Yield(v) = next(0) { println(f\"Y {v}\"); } else { println(\"MISS\"); }\n\
                 println(\"AFTER\");\n\
             }",
        ) {
            // if-let hit: the drop runs (once) at the ARM's exit, before AFTER.
            assert_eq!(out, "Y 0\nDROP\nAFTER\n");
        }
}

#[test]
fn e2e_enum_heap_payload_eq_compares_by_content() {
    // A variant with a concrete `String` payload compares by *content*, not
    // by pointer word — the variant-aware `compile_enum_eq` rebuilds the
    // payload at its declared type and recurses (`Text("a"+"b")` is a
    // distinct allocation from `Text("ab")` but must compare equal). Scalar
    // and unit variants stay correct.
    if let Some(out) = run_program(
        "#[derive(Eq)]\n\
             enum Msg { Text(String), Code(i64), Empty }\n\
             fn main() {\n\
                 let a = Msg.Text(\"a\" + \"b\");\n\
                 let b = Msg.Text(\"ab\");\n\
                 let c = Msg.Text(\"xy\");\n\
                 let d = Msg.Code(5);\n\
                 let e = Msg.Code(5);\n\
                 let f = Msg.Empty;\n\
                 println(f\"{a == b}\");\n\
                 println(f\"{a == c}\");\n\
                 println(f\"{d == e}\");\n\
                 println(f\"{a == d}\");\n\
                 println(f\"{f == f}\");\n\
                 println(f\"{a == f}\");\n\
             }",
    ) {
        assert_eq!(out, "true\nfalse\ntrue\nfalse\ntrue\nfalse\n");
    }
}

#[test]
fn e2e_generic_enum_heap_payload_eq_compares_by_content() {
    // Generic heap-payload enums (`Option[String]`, `Result[_, String]`)
    // compare by *content*, not by pointer word. The bare seeded layout is
    // monomorphization-blind (one `Option`/`Result` shape, payload-as-
    // words), so routing keys off the lowering pass's recorded
    // instantiation (`enum_inst_type_exprs`): `compile_enum_eq` substitutes
    // the `[String]` arg into the `Some`/`Err` payload type and rebuilds it
    // as a `String`. Distinct allocations with equal content must compare
    // equal; scalar instantiations (`Option[i64]`) stay word-wise.
    // Comparisons are written *inline inside f-string interpolations* on
    // purpose: every interp expr is re-parsed under a fixed-length
    // `fn __interp__() { … }` wrapper, so same-position operands across
    // different f-strings share a span. Routing therefore resolves an
    // identifier operand's instantiation by *name* (`enum_inst_var_types`),
    // not by the colliding span — without that, `g == h` (`Option[i64]`)
    // and `a == b` (`Option[String]`) would alias and mis-route. The
    // bound-then-formatted variant is covered by the assertions below too,
    // but the inline form is the regression guard for the span collision.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let a: Option[String] = Some(\"a\" + \"b\");\n\
                 let b: Option[String] = Some(\"ab\");\n\
                 let c: Option[String] = Some(\"xy\");\n\
                 let n: Option[String] = None;\n\
                 println(f\"{a == b}\");\n\
                 println(f\"{a == c}\");\n\
                 println(f\"{a == n}\");\n\
                 println(f\"{n == n}\");\n\
                 let r: Result[String, i64] = Ok(\"a\" + \"b\");\n\
                 let s: Result[String, i64] = Ok(\"ab\");\n\
                 println(f\"{r == s}\");\n\
                 let e1: Result[i64, String] = Err(\"x\" + \"y\");\n\
                 let e2: Result[i64, String] = Err(\"xy\");\n\
                 let e3: Result[i64, String] = Err(\"zz\");\n\
                 println(f\"{e1 == e2}\");\n\
                 println(f\"{e1 == e3}\");\n\
                 let g: Option[i64] = Some(7);\n\
                 let h: Option[i64] = Some(7);\n\
                 println(f\"{g == h}\");\n\
             }",
    ) {
        assert_eq!(out, "true\nfalse\nfalse\ntrue\ntrue\ntrue\nfalse\ntrue\n");
    }
}

#[test]
fn e2e_generic_enum_heap_eq_through_params() {
    // The instantiated-enum type of a *parameter* (`opt: Option[String]`)
    // is registered by name at function entry, so a heap-payload `==`
    // inside the function body compares by content — exercises the
    // parameter-binding leg of `enum_inst_var_types` (distinct from the
    // let-binding leg above).
    if let Some(out) = run_program(
        "fn same_opt(x: Option[String], y: Option[String]) -> bool { x == y }\n\
             fn same_res(x: Result[String, i64], y: Result[String, i64]) -> bool { x == y }\n\
             fn main() {\n\
                 let a: Option[String] = Some(\"a\" + \"b\");\n\
                 let a2: Option[String] = Some(\"a\" + \"b\");\n\
                 let b: Option[String] = Some(\"ab\");\n\
                 let c: Option[String] = Some(\"zz\");\n\
                 println(f\"{same_opt(a, b)}\");\n\
                 println(f\"{same_opt(a2, c)}\");\n\
                 let r: Result[String, i64] = Ok(\"x\" + \"y\");\n\
                 let s: Result[String, i64] = Ok(\"xy\");\n\
                 println(f\"{same_res(r, s)}\");\n\
             }",
    ) {
        assert_eq!(out, "true\nfalse\ntrue\n");
    }
}

#[test]
fn test_e2e_generic_struct_payload_recovery_from_if_scrutinee() {
    // B-2026-07-12-28: the sibling of B-2026-07-12-27, triggered by an
    // INLINE `if`/`match` scrutinee (vs a function whose return annotation
    // pins the type). `match (if c { Ok(x) } else { Err(Wrap{..}) }) { ... }`
    // joined its branches as `Result[i64, TypeParam("E")]` — the `Ok` side
    // left the `Err` slot the abstract enum param and the branch join took
    // the first branch verbatim, freezing it — so the payload binding lost
    // its concrete `Wrap[String]` and codegen truncated the 3-word String to
    // the all-`i64` base (LLVM `ret i64` vs the mono aggregate, a module-
    // verification error). Fixed by MERGING compatible branch types
    // (`join_branch_types`) so the concrete side wins each slot. Pins the
    // whole-binding RETURN (`Err(e) => e`), the field read (`e.val`), and a
    // bind-first move; interp == JIT == AOT.
    let out = run_program(
        r#"
struct Wrap[T] { val: T }
fn ret_binding(x: i64) -> Wrap[String] {
    match (if x > 0i64 { Ok(x) } else { Err(Wrap { val: "kept".to_string() }) }) {
        Ok(_) => { Wrap { val: "ok".to_string() } }
        Err(e) => { e }
    }
}
fn read_field(x: i64) -> i64 {
    match (if x > 0i64 { Ok(x) } else { Err(Wrap { val: "hello".to_string() }) }) {
        Ok(n) => { n }
        Err(e) => { e.val.len() }
    }
}
fn main() {
    println(ret_binding(-1i64).val);
    println(read_field(-1i64).to_string());
}
"#,
    );
    if let Some(out) = out {
        // ret_binding returns the recovered Wrap.val = "kept";
        // read_field returns "hello".len() = 5.
        assert_eq!(out.trim(), "kept\n5");
    }
}

// ── Shared enum (RC) tests ─────────────────────────────────

#[test]
fn test_ir_shared_enum_malloc() {
    let ir = ir_for(
        r#"
shared enum Shape { Circle(i64), Square(i64) }
fn make() -> Shape { Circle(5) }
"#,
    );
    assert!(ir.contains("@malloc"), "shared enum should call malloc");
    assert!(
        ir.contains("store i64 1"),
        "should store initial refcount of 1"
    );
}

#[test]
fn test_e2e_shared_enum_construct_and_match() {
    // NOTE: Unit variant pattern matching (`Color::Red =>`) is a known pre-existing
    // parser limitation (parsed as Binding, not variant pattern). Use tuple variants
    // or wildcard to test shared enum matching.
    let out = run_program(
        r#"
shared enum Action { Add(i64), Mul(i64) }
fn apply(a: Action, base: i64) -> i64 {
    match a {
        Add(n) => base + n,
        Mul(n) => base * n,
    }
}
fn main() {
    let a = Add(5);
    println(apply(a, 10));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "15");
    }
}

#[test]
fn test_e2e_shared_enum_tuple_variant() {
    let out = run_program(
        r#"
shared enum Value { Num(i64), Nothing }
fn extract(v: Value) -> i64 {
    match v {
        Num(n) => n,
        Nothing => 0,
    }
}
fn main() {
    let v = Num(42);
    println(extract(v));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "42");
    }
}

#[test]
fn test_e2e_user_drop_and_payload_free_for_heap_enum_temp_arg() {
    // Heap-payload sibling: for an enum with BOTH a String payload and
    // a user Drop, the inline temp registers the `karac_drop_<E>`
    // wrapper (user body) AND the payload-walking `__karac_drop_<E>`
    // — complementary for enums, unlike structs where the wrapper
    // subsumes field cleanup. Exactly-once on the body; the payload
    // free is asserted leak-side by the Linux LSan CI gate (the
    // ≥36-byte payload defeats short-string reachability masking).
    let out = run_program(
        r#"
enum Msg { Text(String), Nil }
impl Drop for Msg {
    fn drop(mut ref self) { println("msg dropped"); }
}
fn consume(m: Msg) {}
fn main() {
    consume(Msg.Text("this is a long heap string payload over 36 bytes"));
    consume(Msg.Nil);
    println("after");
}
"#,
    )
    .expect("program should compile and run");
    assert_eq!(
        out.matches("msg dropped").count(),
        2,
        "both inline heap-enum temps must run the user drop exactly \
             once; got:\n{out}"
    );
}

#[test]
fn test_ir_enum_temp_arg_registers_user_drop_wrapper() {
    // IR-level pin for the enum carry-forward: the inline unit-variant
    // temp `consume(Sig.B)` must route through the `karac_drop_Sig`
    // user-drop wrapper in `main`. A heap-free enum synthesizes no
    // `__karac_drop_Sig` payload walker (that gate is unchanged).
    let ir = ir_for(
        r#"
enum Sig { A(i64), B }
impl Drop for Sig {
    fn drop(mut ref self) { }
}
fn consume(s: Sig) {}
fn main() {
    consume(Sig.B);
}
"#,
    );
    let main_body = function_body(&ir, "main").unwrap_or_else(|| {
        panic!("main body not found in IR:\n{}", ir);
    });
    assert!(
        main_body.contains("call void @karac_drop_Sig("),
        "expected `main` to call `@karac_drop_Sig(...)` for the \
             inline unit-variant temp; body was:\n{}",
        main_body
    );
}

// ── Unit enum variant matching ──────────────────────────────

#[test]
fn test_e2e_unit_enum_match() {
    let out = run_program(
        r#"
enum Color { Red, Green, Blue }
fn describe(c: Color) -> i64 {
    match c {
        Color.Red => 1,
        Color.Green => 2,
        Color.Blue => 3,
    }
}
fn main() {
    println(describe(Green));
    println(describe(Red));
    println(describe(Blue));
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["2", "1", "3"]);
    }
}

#[test]
fn test_e2e_shared_enum_unit_variant_match() {
    let out = run_program(
        r#"
shared enum Dir { North, South, East, West }
fn to_num(d: Dir) -> i64 {
    match d {
        Dir.North => 0,
        Dir.South => 1,
        Dir.East => 2,
        Dir.West => 3,
    }
}
fn main() {
    println(to_num(East));
    println(to_num(North));
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["2", "0"]);
    }
}

/// B-2026-08-05-16 — an unqualified variant pattern must resolve against
/// the match scrutinee's OWN enum, deterministically.
///
/// `Tok.Str(String, Sp)` and `shared enum Expr { .. Str(SLit) }` share the
/// bare name `Str`. Resolution pinned on `llvm_type` equality for a VALUE
/// scrutinee, which is not a unique key — two enums with structurally
/// identical layouts share one LLVM struct type — so the enum came from
/// whichever the unordered `enum_layouts` map yielded first. The wrong
/// enum's `field_word_offsets` then drove the binding: `Sp.length` (3) was
/// loaded and dereferenced as a buffer pointer, SEGV on address 0x3.
///
/// ASSERTED BY REPETITION, deliberately. The trigger is the compiler's own
/// per-HashMap seed, so ONE green compile proves nothing — the pre-fix
/// compiler emitted correct IR on roughly a third of runs. Compiling the
/// same source repeatedly and demanding byte-identical IR tests the
/// determinism directly, and fails as soon as any run picks the other enum.
///
/// The failure message deliberately does NOT name a cause. It used to
/// assert one — "an unqualified variant name ... is resolving against
/// whichever the unordered enum_layouts map yields first" — and when this
/// test started failing ~6 of 8 full-suite runs, that sentence was read as
/// the diagnosis and filed as the miscompile returning (B-2026-08-20-26).
/// It had not: a sibling test was flipping the runtime-debug-metadata gate
/// on the PROCESS env while this one compiled, so the single differing
/// line was `@KARAC_SPAWN_SITES_ENABLED`. A repetition test can observe
/// that two compiles differ; it cannot observe why, so it should not say.
#[test]
fn test_ir_shared_variant_name_resolves_to_scrutinee_enum_deterministically() {
    const SRC: &str = r#"
struct Sp { line: i64, column: i64, offset: i64, length: i64 }
struct SLit { value: String, span: Sp }
enum Tok { Str(String, Sp), Int(i64) }
shared enum Expr { Int(i64), Boolish(i64), Str(SLit) }
fn text_of(e: Expr) -> String {
    match e {
        Int(v) => v.to_string(),
        Boolish(v) => v.to_string(),
        Str(n) => n.value,
    }
}
fn tok_kind(t: Tok) -> i64 {
    match t { Str(s, sp) => 1, Int(v) => 2 }
}
fn main() {
    let t = Tok.Str("tok".to_string(), Sp { line: 1, column: 1, offset: 0, length: 3 });
    let n = SLit { value: "hello world".to_string(), span: Sp { line: 1, column: 1, offset: 7, length: 11 } };
    println(tok_kind(t) + text_of(Expr.Str(n)).len());
}
"#;
    let first = ir_for(SRC);
    for i in 1..24 {
        let again = ir_for(SRC);
        let differing: Vec<String> = first
            .lines()
            .zip(again.lines())
            .filter(|(a, b)| a != b)
            .take(8)
            .map(|(a, b)| format!("\n  first: {a}\n  again: {b}"))
            .collect();
        assert_eq!(
            first,
            again,
            "IR for the same source differed on compile {i}. Two causes \
                 produce this, and the differing lines below tell them apart: \
                 an unqualified variant name shared across two enums resolving \
                 against whichever enum the unordered `enum_layouts` map yields \
                 first (B-2026-08-05-16 — expect tag constants / payload word \
                 offsets to move), or another test in this binary mutating a \
                 codegen env gate on the PROCESS env mid-compile \
                 (B-2026-08-20-26 — expect a single global's initializer to \
                 flip). Differing lines:{}",
            differing.join("")
        );
    }
}

/// B-2026-09-16-10 — a by-value GENERIC-enum argument to a GENERIC function
/// segfaulted on every compiled backend: exit 139 with NO OUTPUT AT ALL,
/// five `Invalid read of size 8` contexts, against a correct `--interp`.
///
/// THE ROW'S SHAPE IS NARROWER THAN ITS TITLE AND ITS CAUSE IS WIDER. It is
/// filed as `T = Array[String, N]` with a `match` in the callee; neither is
/// required. Measured: `T = String` and `T = Vec[String]` crash identically,
/// `T = Array[i64, N]` aborts with `free(): double free` instead, and a
/// callee whose whole body is `return 7` crashes just as well. What IS
/// required is all four of: a generic ENUM (a generic STRUCT is clean), a
/// variant with exactly ONE field (adding a second is clean), `T` bound to
/// a heap-bearing type (`i64` is clean), and passing it BY VALUE to a
/// GENERIC function (the monomorphic `fn glen(g: G1[String])` is clean, and
/// so are `ref` and a direct `match` in `main`).
///
/// THE DEFECT IS ONE MISSING STORE. The monomorph's prologue registers a
/// `BoxedEnumDrop` for such a param (`user_enum_boxed_payload_variants`), so
/// the callee frees the box at its scope exit; on the NON-generic path the
/// caller zeroes its own slot at the argument and the two balance. IR from
/// the two spellings, side by side -- the monomorphic caller emits
/// `store {i64, i64} zeroinitializer` into its slot before the call and the
/// generic caller emits nothing at all.
///
/// `compile_generic_call` never reaches `compile_call`'s argument loop -- its
/// own doc says it "runs neither half" -- so the disarm was never emitted and
/// the caller's let-site box drop then read through memory the callee had
/// freed. The repair emits it there, gated on the PROLOGUE'S OWN predicate
/// asked of the same instantiated type, so the two halves cannot drift.
///
/// FOUR DISTINCT SYMPTOMS FROM ONE SHAPE, which is what says the defect is a
/// mishandled pointer rather than a drop-ownership slip: `T = String` /
/// `Vec[String]` / `Array[String, N]` / a struct / a tuple all SEGV,
/// `Array[i64, N]` aborts with `free(): double free`, `Vec[i64]` HANGS (a
/// sibling session measured it still running at 1708 s), and `i64` alone is
/// correct. All ten cells below are pinned, the hang included, because a
/// regression that reappears as a hang is the one a crash-only assertion
/// would sit through until the harness timed out.
///
/// THIS FIXTURE PINS THE CRASH ONLY, deliberately. Two shapes in the same
/// family still leak and are filed rather than folded in: the box INTERIOR
/// of an `Array[String, N]` payload (its element buffers), and a payload
/// matched out into an unused arm binding. Both are exit-0 leaks here, so
/// the memory fixture next door carries only the subset that is fully clean
/// and this one asserts the output that used to not exist at all.
#[test]
fn e2e_by_value_generic_enum_arg_to_a_generic_fn_does_not_crash() {
    assert_eq!(
            run_program(
                "             struct P { a: String, b: i64 }\n\
             enum G1[T] { Y(T), N }\n\
             enum G2[T] { Y(T) }\n\
             \n\
             fn glen[T](g: G1[T]) -> i64 {\n\
                 match g { G1.Y(v) => { return 1; } G1.N => { return 0; } }\n\
             }\n\
             fn gplain[T](g: G1[T]) -> i64 { return 7; }\n\
             fn g2len[T](g: G2[T]) -> i64 { return 7; }\n\
             \n\
             fn main() {\n\
                 let a: Array[String, 2] = [f\"b1610-arr-aaaaaaaaaaaa\", f\"b1610-arr-bbbbbbbbbbbb\"];\n\
                 let ga: G1[Array[String, 2]] = G1.Y(a);\n\
                 println(f\"arr {glen(ga)}\");\n\
             \n\
                 let a1: Array[String, 1] = [f\"b1610-one-cccccccccccc\"];\n\
                 let g1: G1[Array[String, 1]] = G1.Y(a1);\n\
                 println(f\"one {glen(g1)}\");\n\
             \n\
                 let gi: G1[Array[i64, 2]] = G1.Y([3, 4]);\n\
                 println(f\"ints {glen(gi)}\");\n\
             \n\
                 let gs: G1[String] = G1.Y(f\"b1610-str-dddddddddddd\");\n\
                 println(f\"str {glen(gs)}\");\n\
             \n\
                 let gv: G1[Vec[String]] = G1.Y([f\"b1610-vec-eeeeeeeeeeee\"]);\n\
                 println(f\"vec {gplain(gv)}\");\n\
             \n\
                 let gn: G1[String] = G1.Y(f\"b1610-nomatch-ffffffffffff\");\n\
                 println(f\"nomatch {gplain(gn)}\");\n\
             \n\
                 let gu: G2[String] = G2.Y(f\"b1610-unit-gggggggggggg\");\n\
                 println(f\"unit {g2len(gu)}\");\n\
             \n\
                 let gp: G1[P] = G1.Y(P { a: f\"b1610-struct-hhhhhhhhhhhh\", b: 2 });\n\
                 println(f\"struct {glen(gp)}\");\n\
             \n\
                 let gt: G1[(String, i64)] = G1.Y((f\"b1610-tuple-iiiiiiiiiiii\", 3));\n\
                 println(f\"tuple {glen(gt)}\");\n\
             \n\
                 let gvi: G1[Vec[i64]] = G1.Y([1, 2]);\n\
                 println(f\"veci {glen(gvi)}\");\n\
             }"
            )
            .as_deref(),
            Some("arr 1\none 1\nints 1\nstr 1\nvec 7\nnomatch 7\nunit 7\nstruct 1\ntuple 1\nveci 1\n")
        );
}

#[test]
fn test_e2e_let_bound_enum_heap_payload_moved_out() {
    // #9 (phase-12 self-hosting): a bare `let`-bound enum with a heap
    // payload, moved out by `return`, must reach the consumer intact (the
    // source's `EnumDrop` is suppressed so the payload isn't freed before
    // use). Value-correctness twin of the ASAN regression — `make()`
    // returns `let e = E.Id(...); e` and the caller reads the String back.
    if let Some(out) = run_program(
        r#"
enum E { Id(String), N(i64) }
fn make() -> E {
    let e = E.Id("hello".to_string());
    e
}
fn main() {
    let r = make();
    match r {
        Id(s) => println(s),
        N(n) => println(n.to_string()),
    }
}
"#,
    ) {
        assert_eq!(out, "hello\n");
    }
}

#[test]
fn test_e2e_struct_with_direct_enum_field_heap_payload() {
    // #15 (phase-12 self-hosting): a struct whose field is a heap-bearing
    // user enum (`Span { tok: Tok, .. }`, the bootstrap's `SpannedToken`
    // shape). The enum payload (`Tok.Id(String)`) must survive being built,
    // transferred out of a by-value param (`wrap`), and destructured — and
    // the struct's synthesized drop must free the enum field exactly once
    // (no double-free / corruption). Correctness is observed via the
    // round-tripped String payloads; the leak itself is covered by the
    // ASAN test on Linux.
    //
    // #19 FIXED 2026-06-12 — transfer-out of an enum-field struct followed by
    // a destructure that *uses* the bound payload is now sound. The fix is
    // entry-copy for enum-field structs (`field_copy_supported` user-enum arm
    // → true; `deep_copy_one_aggregate_field` deep-copies the enum payload via
    // `deep_copy_enum_heap_payload_in_place`): the callee owns a copy
    // independent of the caller's retained original, so `let b = wrap(a)` no
    // longer aliases `a`'s enum buffer and the two struct drops free distinct
    // buffers. `e1`/`e2` below exercise the previously-excised shape — a
    // transfer-out then a BORROW arm (`match b.tok { Id(s) => println(s) }`)
    // and a CONSUME arm — both guardmalloc-clean at O0 and O2. Earlier this
    // double-freed (the source and the transferred result aliased one enum
    // buffer); it passed on normal malloc only by allocation luck, which #20's
    // layout shift removed.
    if let Some(out) = run_program(
        r#"
enum Tok { Id(String), Int(i64) }
struct Span { tok: Tok, off: i64 }
fn wrap(s: Span) -> Span { s }
fn sink(x: String) { if x.len() > 9000 { println(x) } }
fn main() {
    let a = Span { tok: Tok.Id("hello".to_string()), off: 1 };
    let b = wrap(a);
    println(b.off.to_string());
    let c = Span { tok: Tok.Int(42_i64), off: 2 };
    match c.tok { Id(s) => println(s), Int(n) => println(n.to_string()) }
    let d = Span { tok: Tok.Id("world".to_string()), off: 3 };
    match d.tok { Id(s) => println(s), Int(n) => println(n.to_string()) }
    // #19: transfer-out then destructure-and-USE the bound payload.
    let e1 = Span { tok: Tok.Id("xfer".to_string()), off: 4 };
    let f1 = wrap(e1);
    match f1.tok { Id(s) => println(s), Int(n) => println(n.to_string()) }
    let e2 = Span { tok: Tok.Id("cons".to_string()), off: 5 };
    let f2 = wrap(e2);
    match f2.tok { Id(s) => sink(s), Int(n) => println(n.to_string()) }
    println("ok");
}
"#,
    ) {
        assert_eq!(out, "1\n42\nworld\nxfer\nok\n");
    }
}

#[test]
fn test_e2e_struct_nested_enum_leaf_heap_payload() {
    // #18 (phase-12 self-hosting): a struct whose only heap is TRANSITIVELY
    // inside an enum nested under ANOTHER struct field — `Wrap { sp: Span }`
    // with `Span { tok: Tok }` and `Tok` heap-bearing. #15 freed only a
    // DIRECT enum field; `emit_struct_drop_synthesis` now routes a NAMED
    // nested struct field through its own `__karac_drop_struct_<S>` (which
    // post-#15 frees its enum fields). Correctness here is the round-tripped
    // payloads through a struct-literal move into a Wrap then a two-level
    // (`w.sp.tok`) match-BORROW, a three-level (`Deep -> Wrap -> Span ->
    // Tok`) undestructured drop, and a two-level Int-variant match. The leak
    // is covered by `asan_struct_nested_enum_leaf_no_leak_no_double_free`.
    //
    // #19 FIXED 2026-06-12 — the nested-struct TRANSFER-out then two-level
    // match-BORROW (`let b = fwd(a); match b.sp.tok { Id(s) => println(s) }`)
    // is now sound (entry-copy for enum-field structs recurses through the
    // nested `Wrap { sp: Span { tok } }`, so `fwd`'s result owns a copy
    // independent of the source). Exercised by `t1` below; guardmalloc-clean
    // at O0 and O2. Previously double-freed on the caller-retains path.
    if let Some(out) = run_program(
        r#"
enum Tok { Id(String), Int(i64) }
struct Span { tok: Tok, off: i64 }
struct Wrap { sp: Span, hi: i64 }
struct Deep { w: Wrap, tag: i64 }
fn mk(n: i64, s: String) -> Wrap { Wrap { sp: Span { tok: Tok.Id(s), off: n }, hi: n } }
fn fwd(w: Wrap) -> Wrap { w }
fn main() {
    let span = Span { tok: Tok.Id("beta".to_string()), off: 2 };
    let w = Wrap { sp: span, hi: 2 };
    match w.sp.tok { Id(s) => println(s), Int(n) => println(n.to_string()) }

    let deep = Deep { w: mk(3, "gamma".to_string()), tag: 7 };
    println(deep.tag.to_string());

    let c = Wrap { sp: Span { tok: Tok.Int(42_i64), off: 4 }, hi: 4 };
    match c.sp.tok { Id(s) => println(s), Int(n) => println(n.to_string()) }
    // #19: nested transfer-out then two-level match-BORROW of the payload.
    let a1 = Wrap { sp: Span { tok: Tok.Id("xfer".to_string()), off: 5 }, hi: 5 };
    let t1 = fwd(a1);
    match t1.sp.tok { Id(s) => println(s), Int(n) => println(n.to_string()) }
    println("ok");
}
"#,
    ) {
        assert_eq!(out, "beta\n7\n42\nxfer\nok\n");
    }
}

#[test]
fn test_e2e_struct_tuple_enum_leaf_heap_payload() {
    // #21 (phase-12 self-hosting): a struct field that is an anonymous TUPLE
    // whose only heap is inside an enum leaf — `struct H { pe: (Tok, i64) }`
    // with heap enum `Tok`. The struct drop's `NestedTuple` path frees the
    // enum leaf; paired cap-zero suppression at the destructure / tuple-index
    // match / tuple-index let sites and entry-copy of heap-bearing tuple
    // params keep every CONSUME shape single-free. Correctness here is the
    // payloads round-tripping through: a full-tuple destructure + match
    // (`let (t, n) = h.pe; match t`), a direct tuple-index match
    // (`match h.pe.0`), a tuple-index let-move (`let x = h.pe.0`), and a
    // whole-tuple by-value arg whose callee matches an element internally
    // (`sinkt(h.pe)` — the cross-boundary case the param entry-copy closes).
    // The leak + double-free are covered by
    // `asan_struct_tuple_enum_leaf_no_leak_no_double_free`.
    if let Some(out) = run_program(
        r#"
enum Tok { Id(String), Int(i64) }
struct H { pe: (Tok, i64), tag: i64 }
fn sinkt(p: (Tok, i64)) -> String { match p.0 { Id(s) => s, Int(n) => n.to_string() } }
fn main() {
    let a = H { pe: (Tok.Id("alpha".to_string()), 1), tag: 1 };
    let (t, n) = a.pe;
    match t { Id(s) => println(s), Int(x) => println(x.to_string()) }
    println(n.to_string());

    let b = H { pe: (Tok.Id("beta".to_string()), 2), tag: 2 };
    match b.pe.0 { Id(s) => println(s), Int(x) => println(x.to_string()) }

    let c = H { pe: (Tok.Id("gamma".to_string()), 3), tag: 3 };
    let x = c.pe.0;
    match x { Id(s) => println(s), Int(z) => println(z.to_string()) }

    let d = H { pe: (Tok.Id("delta".to_string()), 4), tag: 4 };
    println(sinkt(d.pe));

    let e = H { pe: (Tok.Int(5_i64), 5), tag: 5 };
    match e.pe.0 { Id(s) => println(s), Int(z) => println(z.to_string()) }
    println("ok");
}
"#,
    ) {
        assert_eq!(out, "alpha\n1\nbeta\ngamma\ndelta\n5\nok\n");
    }
}

#[test]
fn test_e2e_enum_field_struct_field_move_out_loop() {
    // #19 FIXED 2026-06-12 — the bootstrap lexer's `render()` shape: iterate a
    // `Vec[SpannedToken]` and pass each element BY VALUE to a fn that moves the
    // enum field OUT of its (now entry-copied) param into a local
    // (`let tk = t.token; match tk { … }`). Entry-copy makes the param
    // callee-owned, and the enum-field move-out cap-zeros the source field in
    // the owning struct's slot (`suppress_struct_field_move_into_literal`'s
    // enum arm, wired from the enum let-binding site) so the param's struct
    // drop and the moved-out local's drop free distinct buffers. Without the
    // cap-zero this double-freed (exit 133); the leak/double-free is covered by
    // `asan_enum_field_struct_field_move_out_no_double_free`.
    if let Some(out) = run_program(
        r#"
enum Tok { Id(String), Eof }
struct Span2 { offset: i64, length: i64 }
struct Span { token: Tok, span: Span2 }
fn render(t: Span) -> String {
    let off = t.span.offset;
    let mut line = f"{off} ";
    let tk = t.token;
    match tk {
        Id(s) => line.push_str(s),
        Eof => line.push_str("eof"),
    }
    line
}
fn build() -> Vec[Span] {
    let mut out: Vec[Span] = Vec.new();
    out.push(Span { token: Tok.Id("alpha".to_string()), span: Span2 { offset: 1, length: 5 } });
    out.push(Span { token: Tok.Id("beta".to_string()), span: Span2 { offset: 7, length: 4 } });
    out.push(Span { token: Tok.Eof, span: Span2 { offset: 11, length: 0 } });
    out
}
fn main() {
    let toks = build();
    for t in toks {
        println(render(t));
    }
}
"#,
    ) {
        assert_eq!(out, "1 alpha\n7 beta\n11 eof\n");
    }
}

#[test]
fn test_ir_freshtemp_enum_method_materializes_and_drops() {
    // Slice 3k: a user impl-block method on a fresh-temp VALUE-ENUM receiver
    // (`make().size()`) where the enum has a heap-bearing variant
    // (`Text(String)`). The identifier-keyed user-impl dispatch resolves only
    // Identifier/self receivers, so a call-result receiver hard-errored ("no
    // handler for method ... on non-identifier receiver"). The fresh-temp path
    // materializes the receiver into a `__urecv_tmp` synth local and — because
    // the enum owns heap — drop-tracks it via `track_enum_var`, whose
    // scope-exit `EnumDrop` runs the synthesized `__karac_drop_Msg` switch to
    // free the live variant's String. Without the materialize the call fails to
    // compile; without the drop the `Text` payload String leaks (Linux LSan).
    let src = r#"
enum Msg { Text(String), Empty }
impl Msg {
    fn size(self) -> i64 {
        match self {
            Msg.Text(s) => s.len(),
            Msg.Empty => 0_i64,
        }
    }
}
fn make() -> Msg { Msg.Text("a message payload string padded beyond thirty-six bytes") }
fn main() {
    println(make().size());
}
"#;
    let ir = ir_for(src);
    assert!(
        ir.contains("__urecv_tmp"),
        "expected the fresh-temp enum receiver materialized into __urecv_tmp; got:\n{}",
        ir
    );
    assert!(
        ir.contains("__karac_drop_Msg"),
        "expected the value-enum temp receiver drop-tracked via the \
             __karac_drop_Msg drop-switch (frees the Text payload String); got:\n{}",
        ir
    );
}

#[test]
fn test_ir_freshtemp_shared_enum_method_materializes_and_rc_drops() {
    // Slice 3k: the shared-ENUM sibling. A `shared enum` receiver is also
    // `Shared(name)` and RC-managed, so it rides the same `track_rc_var` path
    // as the shared struct (`shared_types` carries both, keyed by name with a
    // shared heap type). The fresh-temp path materializes into `__urecv_tmp`
    // and queues one `RcDec` → `__karac_rc_drop_Expr` frees the box (and any
    // heap payload). `track_enum_var` no-ops for a shared enum (DP3), so the
    // shared branch is what carries the drop — verified here so a future change
    // that routes shared enums through the value-enum branch (which would
    // double-count / mis-drop) is caught.
    let src = r#"
shared enum Expr { Lit(i64), Name(String) }
impl Expr {
    fn weight(self) -> i64 {
        match self {
            Expr.Lit(n) => n,
            Expr.Name(s) => s.len(),
        }
    }
}
fn make() -> Expr { Expr.Name("an expr name payload padded beyond thirty-six bytes") }
fn main() {
    println(make().weight());
}
"#;
    let ir = ir_for(src);
    assert!(
        ir.contains("__urecv_tmp"),
        "expected the fresh-temp shared-enum receiver materialized into __urecv_tmp; got:\n{}",
        ir
    );
    assert!(
        ir.contains("__karac_rc_drop_Expr"),
        "expected the shared-enum temp receiver drop-tracked via a scope-exit \
             RcDec running __karac_rc_drop_Expr; got:\n{}",
        ir
    );
}

#[test]
fn test_ir_letelse_freshtemp_enum_unbound_field_freed() {
    // `let Full(_, n) = make() else { return };` — let-else surface. The
    // unbound Vec is freed by the materialized temp's EnumDrop (on the
    // match edge at enclosing-scope exit; on the miss edge the divergent
    // else's cleanup walk frees it wholesale).
    let src = format!(
            "{B_ENUM_PRELUDE}\nfn main() {{\n    let Holder.Full(_, n) = make() else {{ return }}\n    println(n);\n}}\n"
        );
    let ir = ir_for_with_ownership(&src);
    assert!(
        ir.contains("__freshtemp_enum_scrut") && ir.contains("@__karac_drop_Holder("),
        "expected fresh-temp let-else scrutinee materialized + enum-dropped; got:\n{ir}"
    );
}

#[test]
fn test_e2e_derive_default_enum_marked_variant() {
    // Derived enum default selects the `#[default]`-marked variant,
    // regardless of declaration order.
    let out = run_program(
        r#"
#[derive(Default)]
enum Mode { Running(i64), #[default] Idle }
fn main() {
    let m = Mode.default();
    match m {
        Mode.Idle => { println("idle"); }
        Mode.Running(n) => { println(n); }
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "idle");
    }
}

#[test]
fn test_e2e_generic_enum_scalar_payload_unchanged() {
    // B-2026-07-13-3 regression guard: a SCALAR `T` (1 word) fits the erased
    // payload area inline (no boxing), so the mono-payload path must NOT
    // engage — the arm value stays a bare i64, exactly as before the fix.
    let out = run_program(
        "enum Opt[T] { Yes(T), No }\n\
             fn get[T](o: Opt[T], d: T) -> T { match o { Opt.Yes(v) => v, Opt.No => d } }\n\
             fn main() {\n\
             \x20   let a: i64 = get(Opt.Yes(41i64), 7i64);\n\
             \x20   println(a.to_string());\n\
             \x20   let b: i64 = get(Opt.No, 99i64);\n\
             \x20   println(b.to_string());\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "41\n99");
    }
}

#[test]
fn test_e2e_method_on_enum_unit_variant_literal() {
    // B-2026-07-13-4: a method called directly on an enum UNIT-VARIANT
    // LITERAL — `Dir.North.code()` — parses as `Call(Path([Dir, North,
    // code]))`. The typechecker left it untyped and neither backend
    // dispatched a method on a bare enum-literal receiver (codegen keys on
    // an identifier receiver). The typechecker now types it via
    // `infer_method_call` and lowering materializes the receiver into a
    // fresh local (`{ let __recv: Dir = Dir.North; __recv.code() }`).
    // Covers: a chained `.to_string()` (the block-value receiver), a method
    // with an argument, a heap (String) return, and use of the result in
    // arithmetic — each previously failed codegen or the interpreter.
    let out = run_program(
        r#"
enum Color { Red, Green, Blue }
impl Color {
    fn code(self) -> i64 { match self { Color.Red => 0, Color.Green => 1, Color.Blue => 2 } }
    fn name(self) -> String { match self { Color.Red => f"red", Color.Green => f"green", Color.Blue => f"blue" } }
    fn plus(self, n: i64) -> i64 { self.code() + n }
}
fn main() {
    println(Color.Green.code().to_string());
    println(Color.Blue.name());
    println(Color.Red.plus(10).to_string());
    let z = Color.Blue.code() + 5;
    println(z.to_string());
}
"#,
    );
    let out = out.expect("method on enum unit-variant literal should compile");
    assert_eq!(out.trim(), "1\nblue\n10\n7");
}

#[test]
fn test_e2e_enum_explicit_discriminants_payload() {
    // Explicit discriminants are declarations, not layout commitments
    // (design.md § Explicit Discriminants on Payload Variants): codegen
    // lowers the payload enum exactly as it does without them, so construct
    // + match on a `#[repr(u8)]` enum carrying explicit values runs the same
    // as the interpreter (the `run_program` harness pins build == run).
    let out = run_program(
        r#"
#[repr(u8)] enum Op { Reset = 1, Ping(u32) = 5, Stop = 255 }

fn code(o: Op) -> i64 {
    match o {
        Op.Reset => 1_i64,
        Op.Ping(n) => n as i64,
        Op.Stop => 255_i64,
    }
}

fn main() {
    println(code(Op.Reset));
    println(code(Op.Ping(42_u32)));
    println(code(Op.Stop));
}
"#,
    );
    let out = out.expect("payload enum with explicit discriminants should codegen + run");
    assert_eq!(out.trim(), "1\n42\n255");
}

#[test]
fn test_compound_enum_mixed_width_variants_v1_uses_one_word() {
    let out = run_program(
        r#"
enum E { V1(i64), V2(String) }
fn main() {
    let e = V1(42);
    match e {
        V1(x) => println(x),
        V2(_s) => println(99),
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "42");
    }
}

#[test]
fn test_compound_enum_mixed_width_variants_v2_uses_three_words() {
    let out = run_program(
        r#"
enum E { V1(i64), V2(String) }
fn main() {
    let e = V2("hello");
    match e {
        V1(_x) => println(0),
        V2(s) => println(s),
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "hello");
    }
}

#[test]
fn test_enum_variant_name_collision_with_seeded_other_is_deterministic() {
    // This test reads the runtime-debug-metadata gate (via `compile_to_ir`'s
    // `Codegen::new`) once per iteration. It used to hold
    // `SPAWN_SITE_ENV_LOCK` across the loop, because the
    // `test_spawn_site_metadata_*` tests flipped the gate's env var
    // process-wide and a concurrent flip made `KARAC_SPAWN_SITES_ENABLED`
    // read `true` on some iters and `false` on others — diverging the IR
    // for identical source. That diagnosis was right, and the defence was
    // the wrong shape: it protected the tests that knew to ask, and the
    // determinism test added later (B-2026-08-05-16's regression gate) did
    // not, so it inherited the race and its failures read as the
    // enum-order miscompile returning (B-2026-08-20-26). Those tests now
    // pin the gate per-thread instead, so no lock is needed here.
    // Regression gate for the 2026-05-25 codegen-suite intermittent
    // hang. Variant-name → enum-name lookups at four codegen sites
    // (constructor in `try_compile_enum_variant` + `try_unit_enum_variant`,
    // match-dispatch tag in `enum_tag_for_variant`, RHS-typing in
    // `infer_enum_from_value`) previously iterated `self.enum_layouts`
    // (HashMap) with only a hard-coded `Option`/`Result` carve-out
    // for the user-vs-seed preference. When a user enum's variant
    // name collided with a *seeded* built-in (e.g., `MyIoErr.Other`
    // colliding with `TcpError.Other`), HashMap iteration order
    // sometimes picked the seeded layout — producing a wrong-shape
    // enum value at construction and the wrong tag at match
    // dispatch. The downstream symptom was a binary whose `main`
    // ended in LLVM's `unreachable` (lowered to `brk #0x1` on macOS
    // arm64); under `cargo test`'s parallel spawn pattern, the
    // brk-trapped child looped forever instead of terminating (the
    // libtest parent intercepts `EXC_BREAKPOINT` Mach exceptions).
    // The fix replaces the hard-coded name list with a
    // `seeded_enum_names: HashSet<String>` populated by
    // `seed_builtin_enum_layouts`.
    //
    // This test compiles the same `Other`-named-variant program 20
    // times in-process and asserts every IR is byte-identical. With
    // the fix the variant-to-enum lookup is deterministic, so the
    // IR is too. Pre-fix, two distinct IR shapes appeared
    // intermittently (hash 03264… vs ea8b…). Catches any future
    // regression at the IR level without needing to disassemble
    // binaries or depend on cargo-test scheduling to surface a
    // race.
    use karac::codegen::compile_to_ir_with_options;
    const SRC: &str = r#"
enum MyIoErr {
    NotFound,
    PermissionDenied,
    Other(String),
}
fn main() {
    let e = MyIoErr.Other("disk full");
    match e {
        Other(msg) => println(msg),
        _ => println("wrong variant"),
    }
}
"#;
    let mut irs: Vec<String> = Vec::with_capacity(20);
    for _ in 0..20 {
        let mut parsed = karac::parse(SRC);
        assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
        karac::prepare_for_resolve(&mut parsed.program);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let ir =
            compile_to_ir_with_options(&parsed.program, None, None, None, None).expect("ir gen");
        irs.push(ir);
    }
    let first = &irs[0];
    for (i, ir) in irs.iter().enumerate().skip(1) {
        assert_eq!(
            ir, first,
            "IR diverged at iter {i} — variant-name disambiguation \
                 regressed; the user-vs-seed preference in \
                 `seeded_enum_names` is leaking through."
        );
    }
    // Sanity: the IR should print "disk full" through the proper
    // String-payload destructure, not via an integer mis-read. The String
    // path lowers to the NUL-safe console chokepoint `@karac_runtime_write_console`
    // (the auto-par ordered-output primitive, B-2026-06-14-20; it replaced
    // the direct `@fwrite` of the pre-ordered-output path — the runtime fn
    // is what now funnels to libc `fwrite`); the wrong path (mis-typing the
    // payload as i64) would lower `println(msg)` to a `printf("%lld\n")`.
    // The discriminator is the `%lld\0A` format (with newline) — the module
    // also carries `%lld\00` (no newline) f-string integer-format globals
    // unconditionally, so a bare `%lld` check would false-positive; the
    // `\0A`-terminated form is the `println`-of-integer fingerprint.
    assert!(
        first.contains("@karac_runtime_write_console") && !first.contains("%lld\\0A"),
        "expected IR to emit the length-prefixed string console write \
             (`@karac_runtime_write_console`) for `Other(msg) => println(msg)` \
             with no `println`-of-integer format string; got the `%lld\\n` \
             integer path (the wrong-enum-layout symptom)"
    );
}

// ── Compound-payload enum drop-path: non-ASAN regressions ─────────
//
// DP slice (Phase 7.2 — 2026-05-09) lights up scope-exit cleanup for
// value-type enum bindings whose payload includes `String` / `Vec[T]`.
// The ASAN tests in `tests/memory_sanitizer.rs` are the load-bearing
// gates for the heap-buffer-free correctness; the two tests below
// pin the IR-level shape choices the slice locks down — move
// suppression on function-arg consume paths (DP4) and the
// `is_shared` carve-out (DP3) — without depending on ASAN being
// available on the host.

#[test]
fn test_compound_enum_drop_suppressed_when_moved() {
    // Regression gate for DP4. Constructing `e = V(s)` where `s` is
    // a tracked String binding zeros the source's `cap` field as a
    // move-suppression marker (the existing `FreeVecBuffer` cleanup
    // is gated on `cap > 0`). Then `consume(e)` takes the enum by
    // value — function parameters don't register `track_enum_var`,
    // so the param's local alloca becomes a stranded view of the
    // payload words; only the caller's `e`-bound alloca owns
    // cleanup. Verifies no double-free SIGABRT at scope exit.
    let out = run_program(
        r#"
enum E { V(String) }
fn consume(_e: E) -> i64 { 7 }
fn main() {
    let mut s = String.new();
    s.push_str("hello");
    let e = V(s);
    let n = consume(e);
    println(n);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "7");
    }
}

#[test]
fn test_compound_enum_drop_skipped_for_shared_enum() {
    // Regression gate for DP3. `shared enum Sel { V(String) }` is
    // RC-allocated; cleanup goes through `track_rc_var` →
    // `emit_rc_dec`, NOT through the new `track_enum_var` /
    // `__karac_drop_Sel` machinery. Asserts the negative path —
    // no `__karac_drop_Sel` symbol is emitted. (The IR introspection
    // is via `compile_to_ir_string`; we assert the program runs
    // and produces the expected output, with the symbol-absence
    // check as a side-comment.)
    let out = run_program(
        r#"
shared enum Sel { V(String) }
fn main() {
    let mut s = String.new();
    s.push_str("rc payload");
    let _e = Sel.V(s);
    println(1);
}
"#,
    );
    if let Some(out) = out {
        // Program runs; shared-enum cleanup is RC-driven. The
        // `is_shared` carve-out at `track_enum_var` ensures we
        // never registered an EnumDrop action for `_e`.
        assert_eq!(out.trim(), "1");
    }
}

#[test]
fn test_compound_tuple_payload_nested() {
    // TP5 — recursive tuple destructure works through one nesting
    // layer. `((i64, i64), String)` decomposes to inner-tuple +
    // string element via the recursive `reconstruct_payload_value`
    // / `bind_pattern_values` Tuple branches.
    let out = run_program(
        r#"
enum E { V(((i64, i64), String)) }
fn main() {
    let e = V(((10, 20), "nested"));
    match e {
        V(((a, b), s)) => {
            println(a + b);
            println(s);
        }
    }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["30", "nested"]);
    }
}

#[test]
fn test_compound_tuple_payload_three_elements() {
    // Three-element tuple with mixed heap-bearing + primitive
    // elements. Verifies the per-element offset walk handles N≥3
    // correctly (the field_words slice cursor advances past each
    // element's word count without overrunning).
    let out = run_program(
        r#"
enum E { V((Vec[i64], String, i64)) }
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(11);
    v.push(22);
    let e = V((v, "tag", 99));
    match e {
        V((xs, s, n)) => {
            println(xs.len());
            println(s);
            println(n);
        }
    }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["2", "tag", "99"]);
    }
}

#[test]
fn test_e2e_ref_param_enum_field_payload_consume() {
    // B-2026-07-21-5/-6: `match <refparam>.field { Ident(name) => <consume
    // name> … }` — an enum field read through a `ref` param, with an arm
    // that consumes the String payload (string `+` desugars to
    // `String.add(..)`, so the binding counts as escaping). The owned-path
    // move-out suppression GEP'd the ref param's 8-byte pointer SLOT as if
    // it were the struct, storing three zero words out of bounds into
    // adjacent stack slots — clobbering the payload binding (empty-string
    // output) or corrupting locals into a double-free, flipping with
    // opt-level/layout. Now `field_chain_place_ptr` bails on a borrowed
    // root and an ESCAPING ref-chain match deep-clones the scrutinee
    // (`clone_escaping_borrowed_ref_chain_enum`), so bindings own
    // independent buffers and the caller's value stays intact — matching
    // the interpreter. Covers: concat-consume via free fn (b6), identity
    // return (strongest escape), `ref self` receiver, a no-bind arm hit at
    // runtime, a deep two-hop chain, a read-only arm (stays on the
    // zero-cost borrow path), and caller reuse after each call. Sibling
    // LSan test guards the leak/double-free halves.
    let output = run_program(
        "enum Tok { Plus, Ident(String) }\n\
             struct SpTok { tok: Tok, n: i64 }\n\
             struct Outer { sp: SpTok }\n\
             impl SpTok {\n\
                 fn label(ref self) -> String {\n\
                     match self.tok {\n\
                         Ident(name) => { return \"m:\".to_string() + name; }\n\
                         Plus => { return \"m:+\".to_string(); }\n\
                     }\n\
                     return \"?\".to_string();\n\
                 }\n\
             }\n\
             fn render(st: ref SpTok) -> String {\n\
                 match st.tok {\n\
                     Ident(name) => { return \"id:\".to_string() + name; }\n\
                     Plus => { return \"+\".to_string(); }\n\
                 }\n\
                 return \"?\".to_string();\n\
             }\n\
             fn take_name(st: ref SpTok) -> String {\n\
                 match st.tok {\n\
                     Ident(name) => { return name; }\n\
                     Plus => { return \"+\".to_string(); }\n\
                 }\n\
                 return \"?\".to_string();\n\
             }\n\
             fn deep(o: ref Outer) -> String {\n\
                 match o.sp.tok {\n\
                     Ident(name) => { return \"d:\".to_string() + name; }\n\
                     Plus => { return \"d:+\".to_string(); }\n\
                 }\n\
                 return \"?\".to_string();\n\
             }\n\
             fn name_len(st: ref SpTok) -> i64 {\n\
                 match st.tok {\n\
                     Ident(name) => { return name.len(); }\n\
                     Plus => { return 0; }\n\
                 }\n\
                 return -1;\n\
             }\n\
             fn main() {\n\
                 let a = SpTok { tok: Tok.Ident(\"foo\".to_string()), n: 1 };\n\
                 println(render(a));\n\
                 println(render(a));\n\
                 println(take_name(a));\n\
                 println(a.label());\n\
                 println(name_len(a));\n\
                 let p = SpTok { tok: Tok.Plus, n: 2 };\n\
                 println(render(p));\n\
                 let o = Outer { sp: SpTok { tok: Tok.Ident(\"deep\".to_string()), n: 3 } };\n\
                 println(deep(o));\n\
                 let mut v: Vec[SpTok] = Vec.new();\n\
                 v.push(SpTok { tok: Tok.Ident(\"vec\".to_string()), n: 4 });\n\
                 let t = ref v[0];\n\
                 println(render(t));\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "id:foo\nid:foo\nfoo\nm:foo\n3\n+\nd:deep\nid:vec\n");
}

/// B-2026-09-01-33 — an enum temp returned FROM A CALL and passed as an
/// argument runs its payload's `Drop` body.
///
/// `karac_drop_<E>` runs an enum's OWN body alone; the payload walk lives in
/// the separate `__karac_dropelems_enum_<E>`. (A struct differs -- its
/// wrapper carries the field bodies from inside.) The fn-call producer arm
/// registered the wrapper and the memory free and no walker, so
/// `eat(mk(5))` ran `e dSv` here against the interpreter's `e dSv dR5`.
///
/// NOT struct-variant specific: the tuple spelling loses it identically,
/// which is what puts the axis on the producer-call shape rather than on
/// the variant form.
///
/// The three controls are what localize it, and all three were already
/// correct: a NAMED LOCAL is registered by the `let` path, an INLINE
/// CONSTRUCTOR argument by the ctor arm, and an ASSOCIATED producer
/// (`Mk.s(10)`, a 2-segment `Path` callee) reaches the same arm as the free
/// one. Only the temp returned from a call had half the pair.
#[test]
fn test_e2e_producer_call_arg_runs_the_enum_payload_body() {
    const H: &str = "struct R { id: i64 }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
         enum Sv { Hold { inner: R }, Nil }\n\
         impl Drop for Sv { fn drop(mut ref self) { println(\"dSv\") } }\n\
         enum Tv { A(R), Nil }\n\
         impl Drop for Tv { fn drop(mut ref self) { println(\"dTv\") } }\n\
         struct Mk { z: i64 }\n\
         impl Mk { fn s(i: i64) -> Sv { return Sv.Hold { inner: R { id: i } }; } }\n\
         fn eatS(v: Sv) { println(\"e\") }\n\
         fn eatT(v: Tv) { println(\"e\") }\n\
         fn mkS(i: i64) -> Sv { return Sv.Hold { inner: R { id: i } }; }\n\
         fn mkT(i: i64) -> Tv { return Tv.A(R { id: i }); }\n";

    for (label, body, want) in [
        (
            "producer fn, struct variant",
            "eatS(mkS(5))",
            "e\ndSv\ndR5\n",
        ),
        (
            "producer fn, tuple variant",
            "eatT(mkT(6))",
            "e\ndTv\ndR6\n",
        ),
        (
            "via named local, struct variant (control)",
            "let x = mkS(7); eatS(x)",
            "e\ndSv\ndR7\n",
        ),
        (
            "via named local, tuple variant (control)",
            "let y = mkT(8); eatT(y)",
            "e\ndTv\ndR8\n",
        ),
        (
            "inline constructor argument (control)",
            "eatS(Sv.Hold { inner: R { id: 9 } })",
            "e\ndSv\ndR9\n",
        ),
        ("associated producer fn", "eatS(Mk.s(10))", "e\ndSv\ndR10\n"),
    ] {
        assert_eq!(
            run_program(&format!("{H}fn main() {{\n{body}\n}}\n")),
            Some(want.to_string()),
            "{label}"
        );
    }

    // Heap-carrying payload: the body reads a live `String` and `Vec`, which
    // is what proves the memory free now drains AFTER the bodies. Before the
    // reorder this arm pushed the free after the wrapper, so a walker added
    // without moving it would have read through a freed payload.
    assert_eq!(
            run_program(
                "struct R { id: i64, s: String, xs: Vec[i64] }\n\
                 impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}:{self.s}:{self.xs.len()}\") } }\n\
                 enum Sv { Hold { inner: R }, Nil }\n\
                 impl Drop for Sv { fn drop(mut ref self) { println(\"dSv\") } }\n\
                 fn eatS(v: Sv) { println(\"e\") }\n\
                 fn mkS(i: i64) -> Sv { return Sv.Hold { inner: R { id: i, s: \"pay\", xs: [1, 2, 3] } }; }\n\
                 fn main() { eatS(mkS(5)) }\n"
            ),
            Some("e\ndSv\ndR5:pay:3\n".to_string())
        );
}

/// B-2026-09-01-31 — compiled twin of `tests/interpreter.rs`'s
/// `discarded_enum_struct_variant_literal_runs_its_payload_body`, same
/// programs and expectations.
///
/// This backend was correct on every row; the interpreter ran the enum's
/// own body alone for the struct-variant spellings. The twin is what makes
/// "the two spellings and the two backends all agree" a property a test
/// holds rather than one only the fixed side asserts.
///
/// `producer-call` matters most here: it is the row that pins what this
/// backend does NOT do, and the interpreter had to be left matching it.
///
/// `two-tail-if` and `match` are the rows B-2026-09-01-34 added. This
/// backend used to emit no body at all for a struct-variant literal in
/// those positions -- two declines at once, since neither the
/// representative-tail redirect nor the type-name battery admitted the
/// spelling -- so there was no agreed expectation to share and the
/// interpreter carried an exclusion to avoid deepening the gap. Both are
/// asserted on both sides now.
#[test]
fn test_e2e_discarded_enum_struct_variant_literal_runs_its_payload_body() {
    const H: &str = "struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             enum Sv { Hold { inner: R }, Nil }\n\
             impl Drop for Sv { fn drop(mut ref self) { println(\"dSv\") } }\n\
             enum Tv { A(R), Nil }\n\
             impl Drop for Tv { fn drop(mut ref self) { println(\"dTv\") } }\n\
             enum Nd { H { inner: R }, Nil }\n\
             fn mk(i: i64) -> Sv { return Sv.Hold { inner: R { id: i } }; }\n";
    for (label, body, want) in [
            (
                "struct-variant literal",
                "let _ = Sv.Hold { inner: R { id: 1 } }; println(\"d\")",
                "dSv\ndR1\nd\n",
            ),
            (
                "tuple-variant literal (control)",
                "let _ = Tv.A(R { id: 2 }); println(\"d\")",
                "dTv\ndR2\nd\n",
            ),
            (
                "no-own-drop-enum (control)",
                "let _ = Nd.H { inner: R { id: 3 } }; println(\"d\")",
                "dR3\nd\n",
            ),
            (
                "block-peel",
                "let _ = { Sv.Hold { inner: R { id: 4 } } }; println(\"d\")",
                "dSv\ndR4\nd\n",
            ),
            (
                "no-else-if",
                "let c = true; let _ = if c { Sv.Hold { inner: R { id: 5 } } }; println(\"d\")",
                "dSv\ndR5\nd\n",
            ),
            (
                // B-2026-09-02-13 — no longer "own body alone": a call
                // producer runs the payload body too, matching the bound
                // local. Kept as the producer-call control.
                "producer-call (control: a call producer, own body + payload)",
                "let _ = mk(6); println(\"d\")",
                "dSv\ndR6\nd\n",
            ),
            (
                "two-tail-if",
                "let c = true; let _ = if c { Sv.Hold { inner: R { id: 7 } } } else { Sv.Nil }; println(\"d\")",
                "dSv\ndR7\nd\n",
            ),
            (
                "match",
                "let n = 1; let _ = match n { 1 => { Sv.Hold { inner: R { id: 8 } } } _ => { Sv.Nil } }; println(\"d\")",
                "dSv\ndR8\nd\n",
            ),
            (
                "two-tail-if, tuple variant (control)",
                "let c = true; let _ = if c { Tv.A(R { id: 9 }) } else { Tv.Nil }; println(\"d\")",
                "dTv\ndR9\nd\n",
            ),
        ] {
            assert_eq!(
                run_program(&format!("{H}fn main() {{\n{body}\n}}\n")),
                Some(want.to_string()),
                "{label}"
            );
        }

    assert_eq!(
            run_program(
                "struct R { id: i64, xs: Vec[i64] }\n\
                 impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}:{self.xs.len()}\") } }\n\
                 enum Sv { Hold { inner: R }, Nil }\n\
                 impl Drop for Sv { fn drop(mut ref self) { println(\"dSv\") } }\n\
                 fn main() {\n\
                 \x20   let _ = Sv.Hold { inner: R { id: 1, xs: [1, 2, 3] } };\n\
                 \x20   println(\"d\")\n\
                 }\n"
            ),
            Some("dSv\ndR1:3\nd\n".to_string())
        );
}

/// B-2026-09-01-32 — the UNQUALIFIED enum struct-variant literal
/// `Hold { .. }` runs its `Drop` bodies.
///
/// The sibling of B-2026-08-31-8, and the spelling that row's fix deliberately
/// declined. It was wrong on ALL FOUR surfaces at once, so no A/B parity gate
/// could see it: every backend agreed, and every backend ran neither the enum's
/// own body nor its payload's. Fixing the interpreter alone would have turned
/// that agreed-but-wrong answer into a fresh run-vs-build divergence — the
/// worse of the two — which is why both halves landed together.
///
/// The two CONTROLS are what localize it to the brace spelling. The qualified
/// form `Sv.Hold { .. }` was correct (B-2026-08-31-8 for the interpreter;
/// codegen always), and the unqualified TUPLE form `A(r)` was correct on every
/// surface throughout — it parses as an `ExprKind::Call` and reaches
/// `enum_name_for_variant_ctor`, the very helper the brace arm now shares.
///
/// `struct wins over a same-named variant` pins the PRECEDENCE, which is the
/// one way this fix could have broken working programs: both the interpreter
/// and codegen check for a real struct of that name before reading the segment
/// as a variant, so an ordinary literal keeps its own drop in a program that
/// also declares a variant of the same name. Without this row a fix that
/// dropped the struct check would still pass every other row here.
///
/// The two order rows pin the SEQUENCE, not just the count: argument temps are
/// introduced left to right and pop right to left (B-2026-08-29-46), so a fix
/// that fired them forward would agree on every count and differ only here.
/// The mixed row additionally proves the two spellings share one owner
/// mechanism rather than two that happen to agree.
///
/// Compiled twin of `tests/interpreter.rs`'s
/// `fresh_temp_unqualified_struct_variant_arg_runs_its_drop_bodies` — same
/// programs, same expectations. Unlike B-2026-08-31-8's twin, this backend
/// was NOT already correct: it needed its own half of the fix, in
/// `enum_name_of_expr`, whose struct-literal arm was guarded `path.len() >=
/// 2` and so answered `None` for the one-segment spelling.
#[test]
fn test_e2e_fresh_temp_unqualified_struct_variant_arg_runs_its_drop_bodies() {
    const H: &str = "struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             enum Sv { Hold { inner: R }, Nil }\n\
             impl Drop for Sv { fn drop(mut ref self) { println(\"dSv\") } }\n\
             enum Tv { A(R), Nil }\n\
             impl Drop for Tv { fn drop(mut ref self) { println(\"dTv\") } }\n\
             struct Dup { id: i64 }\n\
             impl Drop for Dup { fn drop(mut ref self) { println(f\"dDup{self.id}\") } }\n\
             enum Ev { Dup { inner: R }, Nil }\n\
             impl Drop for Ev { fn drop(mut ref self) { println(\"dEv\") } }\n\
             fn eat(v: Sv) { println(\"e\") }\n\
             fn eatt(v: Tv) { println(\"e\") }\n\
             fn eatd(v: Dup) { println(\"e\") }\n\
             fn eat2(v: Sv, w: Sv) { println(\"e2\") }\n\
             struct Rh { id: i64, tag: String, buf: Vec[i64] }\n\
             impl Drop for Rh { fn drop(mut ref self) { println(f\"dRh{self.id}:{self.buf.len()}\") } }\n\
             enum Svh { HoldH { inner: Rh }, Nil }\n\
             impl Drop for Svh { fn drop(mut ref self) { println(\"dSvh\") } }\n\
             fn eath(v: Svh) { println(\"e\") }\n";
    for (label, body, want) in [
        (
            "unqualified struct-variant fresh temp",
            "eat(Hold { inner: R { id: 1 } })",
            "e\ndSv\ndR1\n",
        ),
        (
            "qualified struct-variant (control, B-2026-08-31-8)",
            "eat(Sv.Hold { inner: R { id: 2 } })",
            "e\ndSv\ndR2\n",
        ),
        (
            "unqualified tuple-variant (control)",
            "eatt(A(R { id: 3 }))",
            "e\ndTv\ndR3\n",
        ),
        (
            "named-local unqualified (control)",
            "let a = Hold { inner: R { id: 4 } }; eat(a)",
            "e\ndSv\ndR4\n",
        ),
        (
            "struct wins over a same-named variant",
            "eatd(Dup { id: 7 })",
            "e\ndDup7\n",
        ),
        (
            "unqualified, HEAP-carrying payload",
            "eath(HoldH { inner: Rh { id: 8, tag: \"ab\", buf: [1, 2, 3] } })",
            "e\ndSvh\ndRh8:3\n",
        ),
        (
            "two-fresh unqualified, reverse order",
            "eat2(Hold { inner: R { id: 5 } }, Hold { inner: R { id: 6 } })",
            "e2\ndSv\ndR6\ndSv\ndR5\n",
        ),
        (
            "mixed qualified + unqualified, reverse order",
            "eat2(Sv.Hold { inner: R { id: 8 } }, Hold { inner: R { id: 9 } })",
            "e2\ndSv\ndR9\ndSv\ndR8\n",
        ),
    ] {
        assert_eq!(
            run_program(&format!("{H}fn main() {{\n{body}\n}}\n")),
            Some(want.to_string()),
            "{label}"
        );
    }
}

/// B-2026-08-31-8 — compiled twin of `tests/interpreter.rs`'s
/// `fresh_temp_struct_variant_arg_runs_its_drop_bodies`, same programs and
/// expectations.
///
/// This backend was correct on every row; the interpreter ran NO body for
/// the struct-variant fresh temp. The twin is what makes "the two spellings
/// and the two backends all agree" a property a test holds, rather than one
/// only the fixed side asserts — and `two-fresh` pins the argument-temp
/// order, which is this backend's cleanup-frame LIFO and was the reference
/// the interpreter's walk had to match.
#[test]
fn test_e2e_fresh_temp_struct_variant_arg_runs_its_drop_bodies() {
    const H: &str = "struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             enum Sv { Hold { inner: R }, Nil }\n\
             impl Drop for Sv { fn drop(mut ref self) { println(\"dSv\") } }\n\
             enum Tv { A(R), Nil }\n\
             impl Drop for Tv { fn drop(mut ref self) { println(\"dTv\") } }\n\
             fn eat(v: Sv) { println(\"e\") }\n\
             fn eatt(v: Tv) { println(\"e\") }\n\
             fn eat2(v: Sv, w: Sv) { println(\"e2\") }\n\
             fn mk(i: i64) -> Sv { return Sv.Hold { inner: R { id: i } }; }\n";
    for (label, body, want) in [
        (
            "struct-variant fresh temp",
            "eat(Sv.Hold { inner: R { id: 1 } })",
            "e\ndSv\ndR1\n",
        ),
        (
            "tuple-variant fresh temp (control)",
            "eatt(Tv.A(R { id: 2 }))",
            "e\ndTv\ndR2\n",
        ),
        (
            "named-local (control)",
            "let a = Sv.Hold { inner: R { id: 3 } }; eat(a)",
            "e\ndSv\ndR3\n",
        ),
        (
            "two-fresh, reverse order",
            "eat2(Sv.Hold { inner: R { id: 5 } }, Sv.Hold { inner: R { id: 6 } })",
            "e2\ndSv\ndR6\ndSv\ndR5\n",
        ),
    ] {
        assert_eq!(
            run_program(&format!("{H}fn main() {{\n{body}\n}}\n")),
            Some(want.to_string()),
            "{label}"
        );
    }

    const M: &str = "struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             enum Sv { Hold { inner: R }, Nil }\n\
             impl Drop for Sv { fn drop(mut ref self) { println(\"dSv\") } }\n\
             fn s2(v: Sv) { match v { Sv.Hold { inner } => { let m = inner; println(f\"b{m.id}\") } Sv.Nil => { } } }\n\
             fn s2b(v: Sv) { match v { Sv.Hold { inner } => { println(f\"b{inner.id}\") } Sv.Nil => { } } }\n";
    for (label, body, want) in [
        (
            "match, with rebind",
            "s2(Sv.Hold { inner: R { id: 7 } })",
            "b7\ndSv\ndR7\n",
        ),
        (
            "match, no rebind",
            "s2b(Sv.Hold { inner: R { id: 8 } })",
            "b8\ndSv\ndR8\n",
        ),
    ] {
        assert_eq!(
            run_program(&format!("{M}fn main() {{\n{body}\n}}\n")),
            Some(want.to_string()),
            "{label}"
        );
    }
}

/// B-2026-08-30-55 — the compiled twin of `tests/interpreter.rs`'s
/// `method_frame_owns_its_by_value_enum_argument`, sharing its expectations
/// verbatim.
///
/// This backend was already correct on every row; the interpreter ran ZERO
/// bodies for the fresh-temp enum argument and TWO for the named struct
/// one. The twin exists because the property being restored is that the two
/// AGREE, and a fixture on one side alone cannot pin that — it would keep
/// passing while the other drifted.
#[test]
fn test_e2e_method_frame_owns_its_by_value_enum_argument() {
    const H: &str = "struct R { id: i64, tag: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             enum E { A(R), B }\n\
             impl Drop for E { fn drop(mut ref self) { println(\"dE\") } }\n\
             struct T { n: i64 }\n\
             impl T { fn eat(ref self, b: E) -> i64 { return 3; } }\n\
             impl T { fn eats(ref self, r: R) -> i64 { return 3; } }\n\
             fn eatf(b: E) -> i64 { return 3; }\n";
    for (label, body, want) in [
        (
            "temp",
            "fn main() { let t: T = T { n: 1 };\n\
                 let v: i64 = t.eat(E.A(R { id: 8, tag: f\"t8\" })); println(f\"v{v}\") }\n",
            "dE\ndR8\nv3\n",
        ),
        (
            "named",
            "fn main() { let t: T = T { n: 1 }; let c: E = E.A(R { id: 8, tag: f\"t8\" });\n\
                 let v: i64 = t.eat(c); println(f\"v{v}\") }\n",
            "dE\ndR8\nv3\n",
        ),
        (
            "named-struct",
            "fn main() { let t: T = T { n: 1 }; let c: R = R { id: 8, tag: f\"t8\" };\n\
                 let v: i64 = t.eats(c); println(f\"v{v}\") }\n",
            "dR8\nv3\n",
        ),
        (
            "free-fn",
            "fn main() { let v: i64 = eatf(E.A(R { id: 8, tag: f\"t8\" })); println(f\"v{v}\") }\n",
            "dE\ndR8\nv3\n",
        ),
    ] {
        assert_eq!(
            run_program(&format!("{H}{body}")),
            Some(want.to_string()),
            "{label}"
        );
    }
}

/// phase-7 — nested enum-payload destructure through `Result` / `Option`.
/// `match r { Result.Err(E.A(c)) => … }` must (a) check the *inner*
/// variant's tag (so `Err(E.B)` does NOT take the `Err(E.A(c))` arm)
/// and (b) bind the inner payload `c`. Before the fix, the binding
/// dropped (`Undefined variable 'c'` — didn't compile); once binding
/// worked, the outer-tag-only condition wrongly matched any `Err(...)`.
/// Both halves are covered here, plus the fieldless inner variant
/// (`E.B`), a multi-payload inner variant (`E.A(c, d)`), and the
/// `Option.Some(E…)` / `Result.Ok(E…)` carriers.
#[test]
fn test_e2e_nested_enum_payload_bind_and_discriminate() {
    let output = run_program(
            "enum E { A(i64), B }\n\
             enum P { Two(i64, i64), N }\n\
             fn err_a() -> Result[i64, E] { Result.Err(E.A(42)) }\n\
             fn err_b() -> Result[i64, E] { Result.Err(E.B) }\n\
             fn opt(w: i64) -> Option[E] { if w == 0 { Option.Some(E.A(7)) } else { Option.Some(E.B) } }\n\
             fn ok_p() -> Result[P, i64] { Result.Ok(P.Two(3, 4)) }\n\
             fn show_e(r: Result[i64, E]) {\n\
                 match r {\n\
                     Result.Ok(_) => { println(0 - 1); }\n\
                     Result.Err(E.A(c)) => { println(c); }\n\
                     Result.Err(E.B) => { println(0 - 2); }\n\
                 }\n\
             }\n\
             fn main() {\n\
                 show_e(err_a());\n\
                 show_e(err_b());\n\
                 match opt(0) { Option.Some(E.A(c)) => { println(c); } Option.Some(E.B) => { println(0 - 3); } Option.None => { println(0 - 4); } }\n\
                 match opt(1) { Option.Some(E.A(c)) => { println(c); } Option.Some(E.B) => { println(0 - 3); } Option.None => { println(0 - 4); } }\n\
                 match ok_p() { Result.Ok(P.Two(a, b)) => { println(a + b); } Result.Ok(P.N) => { println(0 - 5); } Result.Err(_) => { println(0 - 6); } }\n\
             }",
        )
        .expect("compile + run failed");
    // err_a → 42 ; err_b → -2 (the E.B arm, NOT the E.A(c) arm) ;
    // opt(0)=Some(E.A(7)) → 7 ; opt(1)=Some(E.B) → -3 ; ok_p → 3+4=7.
    assert_eq!(output, "42\n-2\n7\n-3\n7\n");
}

#[test]
fn test_e2e_modbind_enum_unit_variant() {
    // `EnumName.UnitVariant` path → enum layout's struct constant
    // with the tag in field 0 and zero-initialised payload words.
    // Match-arm dispatches against the loaded tag.
    let output = run_program(
        "enum Mode { Off, On, Auto }\n\
             let DEFAULT_MODE: Mode = Mode.Off;\n\
             let LIVE_MODE: Mode = Mode.Auto;\n\
             fn main() {\n\
                 let m: Mode = DEFAULT_MODE;\n\
                 let a: Mode = LIVE_MODE;\n\
                 match m {\n\
                     Mode.Off => println(0),\n\
                     Mode.On => println(1),\n\
                     Mode.Auto => println(2),\n\
                 }\n\
                 match a {\n\
                     Mode.Off => println(0),\n\
                     Mode.On => println(1),\n\
                     Mode.Auto => println(2),\n\
                 }\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "0\n2\n");
}

/// B-2026-08-10-3 — the pin that actually caught the bug in this slice, and
/// the reason it is a separate test from `seek` itself.
///
/// `SeekFrom` is a PRELUDE enum, and prelude enums reach the typechecker
/// through `STDLIB_PROGRAMS` but never codegen's `declare_enums`, which
/// walks only the user's `program.items`. Each one therefore needs an
/// explicit layout seed (`Ordering`, `VarError`, `IoError` all carry one).
/// Without it every variant construction falls through to the `i64 0`
/// placeholder.
///
/// That failure is silent and DIRECTIONAL, which is what makes it worth its
/// own pin: `Start` is tag 0, so seeking from the start kept working while
/// `Current` and `End` both silently became `Start` — a wrong seek, not an
/// error. Measured pre-seed, `f.seek(SeekFrom.End, -1)` returned `Err`
/// (negative absolute position) and `f.seek(SeekFrom.Current, 0)` reported
/// position 0. A seek-only test would have caught it here by luck; this one
/// catches the CAUSE, and would catch it for any future prelude enum whose
/// seed is forgotten.
#[test]
fn test_e2e_seek_from_prelude_enum_discriminates() {
    assert_eq!(
        run_program(
            "fn name(w: SeekFrom) -> String {\n\
                 \x20   match w {\n\
                 \x20       SeekFrom.Start => { return \"start\"; }\n\
                 \x20       SeekFrom.Current => { return \"current\"; }\n\
                 \x20       SeekFrom.End => { return \"end\"; }\n\
                 \x20   }\n\
                 }\n\
                 fn main() {\n\
                     println(name(SeekFrom.Start));\n\
                     println(name(SeekFrom.Current));\n\
                     println(name(SeekFrom.End));\n\
                 }"
        )
        .as_deref(),
        Some("start\ncurrent\nend\n")
    );
}

// ── Contracts — struct/impl invariants at method exits (codegen) ──
//
// design.md § Contracts rule 3: `impl invariant` fires at every method
// exit (pub and private); plain `invariant` only at `pub` method exits.
// These exercise the pub/private × plain/impl matrix in an AOT binary,
// mirroring the interpreter coverage in tests/interpreter.rs.

#[test]
fn test_e2e_contract_invariant_holds() {
    // A pub method that keeps `self.n >= 0` true must not fault; the
    // value prints normally.
    let out = run_program(
        r#"
struct Counter { n: i64, invariant self.n >= 0 }
impl Counter { pub fn inc(mut ref self) -> i64 { self.n = self.n + 1; self.n } }
fn main() { let mut c = Counter { n: 0 }; println(c.inc()); }
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "1");
    }
}

#[test]
fn test_e2e_contract_invariant_violation_aborts() {
    // `dec` drives `self.n` to -1, violating `self.n >= 0` at the pub
    // method exit — the binary aborts with `contract violated`.
    let captured = run_program_capturing(
        r#"
struct Counter { n: i64, invariant self.n >= 0 }
impl Counter { pub fn dec(mut ref self) { self.n = self.n - 1; } }
fn main() { let mut c = Counter { n: 0 }; c.dec(); println(42); }
"#,
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("contract violated"),
            "expected an invariant abort, got stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
        assert!(
            !c.stdout.contains("42"),
            "code after the abort must not run"
        );
    }
}

#[test]
fn test_e2e_contract_impl_invariant_fires_on_nonpub_exit() {
    // `impl invariant` fires at *every* method exit, including non-pub
    // methods. `dec` is non-pub and breaks `self.n >= 0` at its exit;
    // the binary aborts. This is what distinguishes `impl invariant`
    // from a plain `invariant` (next test). Calls the non-pub method
    // directly from `main` (same module, identifier receiver) — the
    // `self.method()` self-dispatch path is a separate codegen gap.
    let captured = run_program_capturing(
        r#"
struct Counter { n: i64, impl invariant self.n >= 0 }
impl Counter { fn dec(mut ref self) { self.n = self.n - 1; } }
fn main() { let mut c = Counter { n: 0 }; c.dec(); println(42); }
"#,
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("contract violated"),
            "expected impl-invariant abort at the non-pub exit, got stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
        assert!(
            !c.stdout.contains("42"),
            "code after the abort must not run"
        );
    }
}

#[test]
fn test_e2e_contract_plain_invariant_not_at_nonpub_exit() {
    // A plain `invariant` is checked only at `pub` method exits. `dec`
    // is non-pub and drives `self.n` to -1, violating `self.n >= 0` —
    // but because the method isn't `pub`, no check fires and the
    // program completes normally (prints 7). Same plain-invariant
    // struct as the abort test above; the *only* difference is the
    // `pub` keyword, isolating the pub-gating logic.
    let out = run_program(
        r#"
struct Counter { n: i64, invariant self.n >= 0 }
impl Counter { fn dec(mut ref self) { self.n = self.n - 1; } }
fn main() { let mut c = Counter { n: 0 }; c.dec(); println(7); }
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "7");
    }
}

// ── Contracts — constructor invariants (pub assoc fn returning Self) ─
//
// design.md § Contracts: "Constructors (pub associated functions that
// return `Self`) also check the invariant at their return point." In
// codegen the constructor has no `self` parameter, so the RETURN value is
// bound as `self` before the invariant check (mirroring how `ensures`
// binds `result`). Before this slice, `method_invariants_for` resolved the
// invariants by the `Type.method` name but `emit_invariant_checks` aborted
// codegen with `Undefined variable 'self'` — so these tests are
// load-bearing (the harness panics on codegen failure). Owned structs
// only; shared (RC) constructors are a tracked follow-on.

#[test]
fn test_e2e_constructor_invariant_holds() {
    // A constructor that produces a valid instance prints normally.
    let out = run_program(
        r#"
pub struct Counter { n: i64, invariant self.n >= 0 }
impl Counter { pub fn make() -> Counter { Counter { n: 7 } } }
fn main() { let c = Counter.make(); println(c.n); }
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "7");
    }
}

#[test]
fn test_e2e_constructor_invariant_violation_aborts() {
    // The constructor builds `n = -5`, violating `self.n >= 0` at its
    // return point — the AOT binary aborts with `contract violated` even
    // though no method ran.
    let captured = run_program_capturing(
        r#"
pub struct Counter { n: i64, invariant self.n >= 0 }
impl Counter { pub fn bad() -> Counter { Counter { n: 0 - 5 } } }
fn main() { let c = Counter.bad(); println(c.n); println(42); }
"#,
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("contract violated"),
            "constructor invariant must abort, got stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
        assert!(
            !c.stdout.contains("42"),
            "code after the abort must not run"
        );
    }
}

#[test]
fn test_e2e_constructor_impl_invariant_aborts() {
    // `impl invariant` fires at the constructor return too. (The constructor
    // returns the explicit type name `-> Counter`; a `-> Self` return with a
    // named struct literal is rejected by the typechecker before codegen, so
    // `-> Type` is the buildable constructor form — `returns_self_or_type`
    // also accepts a literal `Self` defensively.)
    let captured = run_program_capturing(
        r#"
pub struct Counter { n: i64, impl invariant self.n >= 0 }
impl Counter { pub fn bad() -> Counter { Counter { n: 0 - 1 } } }
fn main() { let c = Counter.bad(); println(c.n); }
"#,
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("contract violated"),
            "an `impl invariant` must fire at the constructor return, got stdout={:?}",
            c.stdout
        );
    }
}

#[test]
fn test_e2e_shared_constructor_invariant_holds() {
    // Shared (RC) struct constructor invariants are now enforced in codegen
    // too: a valid instance runs and prints. The shared-receiver `self.field`
    // resolves through the heap-GEP path because `shared_type_for_expr`
    // accepts the constructor's `SelfValue` binding (gated to constructor
    // emission via `constructor_invariant_self_type`).
    let out = run_program(
        r#"
pub shared struct Scell { n: i64, invariant self.n >= 0 }
impl Scell { pub fn make() -> Scell { Scell { n: 3 } } }
fn main() { let c = Scell.make(); println(c.n); }
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "3");
    }
}

#[test]
fn test_e2e_shared_constructor_invariant_violation_aborts() {
    // A shared-struct constructor that builds `n = -1` violates
    // `self.n >= 0` at its return point and aborts — the shared heap-GEP
    // field read sees the real `-1` (the +1 refcount offset is handled by
    // the shared path). Before this slice shared constructors were skipped
    // (unenforced); now they enforce like owned ones.
    let captured = run_program_capturing(
        r#"
pub shared struct Scell { n: i64, invariant self.n >= 0 }
impl Scell { pub fn bad() -> Scell { Scell { n: 0 - 1 } } }
fn main() { let c = Scell.bad(); println(c.n); println(42); }
"#,
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("contract violated"),
            "a shared-struct constructor invariant must abort, got stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
        assert!(
            !c.stdout.contains("42"),
            "code after the abort must not run"
        );
    }
}

#[test]
fn test_ir_strip_contracts_invariant_only() {
    // An `invariant`-only struct method: the invariant assert is present
    // by default and gone when stripped.
    let src = r#"
struct Counter { n: i64, invariant self.n >= 0 }
impl Counter { pub fn inc(mut ref self) -> i64 { self.n = self.n + 1; self.n } }
fn main() { let mut c = Counter { n: 0 }; println(c.inc()); }
"#;
    assert!(ir_for(src).contains("contract violated"));
    assert!(!ir_for_contracts_stripped(src).contains("contract violated"));
}

#[test]
fn test_ir_invariant_keeps_runtime_prefix() {
    // The scan must see struct `invariant`s too, not just fn contracts.
    let ir = ir_for(
        r#"
struct Counter { n: i64, invariant self.n >= 0 }
impl Counter { pub fn inc(mut ref self) -> i64 { self.n = self.n + 1; self.n } }
fn main() { let mut c = Counter { n: 0 }; println(c.inc()); }
"#,
    );
    assert!(
        ir.contains("call ptr @karac_runtime_panic_prefix"),
        "a program with a struct invariant must keep the runtime prefix read"
    );
}

#[test]
fn test_e2e_struct_wrapped_recursive_shared_enum_tree() {
    // B-2026-06-14-28: the self-hosting AST-port wrapping convention —
    // `shared enum Expr` for recursive edges (the `Box<Expr>` analog),
    // plain `struct` for tagged-union operands, `Vec[Expr]` for sequence
    // children. v1 forbids a direct nested-enum payload, so the recursive
    // edge passes through a struct operand wrapper (`Add(BinOp)` +
    // `struct BinOp { left: Expr, right: Expr }`) rather than the direct
    // `Add(Expr, Expr)`. This pins value-correctness of build / recursive
    // match-destructure / by-value transform-returning-a-new-tree (the
    // parser-rewrite shape) / move-out of Vec[Expr] elements on that
    // shape — the operations the self-hosting parser hammers while
    // building the AST at scale. (Memory-safety on the same shape is
    // pinned by the `asan_struct_wrapped_*` cases in
    // tests/memory_sanitizer.rs.)
    if let Some(out) = run_program(
        "shared enum Expr { Num(i64), Add(BinOp), Neg(Unary), Call(CallExpr) }\n\
             struct BinOp { left: Expr, right: Expr }\n\
             struct Unary { operand: Expr }\n\
             struct CallExpr { callee: Expr, args: Vec[Expr] }\n\
             fn eval(e: Expr) -> i64 {\n\
                 match e {\n\
                     Num(n) => n,\n\
                     Add(b) => eval(b.left) + eval(b.right),\n\
                     Neg(u) => 0 - eval(u.operand),\n\
                     Call(c) => {\n\
                         let mut acc = eval(c.callee);\n\
                         for a in c.args { acc = acc + eval(a); }\n\
                         acc\n\
                     }\n\
                 }\n\
             }\n\
             fn fold(e: Expr) -> Expr {\n\
                 match e {\n\
                     Num(n) => Num(n),\n\
                     Neg(u) => Neg(Unary { operand: fold(u.operand) }),\n\
                     Add(b) => Add(BinOp { left: fold(b.left), right: fold(b.right) }),\n\
                     Call(c) => Call(c),\n\
                 }\n\
             }\n\
             fn build(n: i64) -> Expr {\n\
                 if n <= 0 { Num(1) }\n\
                 else { Add(BinOp { left: Num(n), right: build(n - 1) }) }\n\
             }\n\
             fn main() {\n\
                 let inner = Add(BinOp { left: Num(2), right: Num(3) });\n\
                 let t = Neg(Unary { operand: Add(BinOp { left: Num(1), right: inner }) });\n\
                 println(eval(t).to_string());            // -(1+(2+3)) = -6\n\
                 let mut args: Vec[Expr] = Vec.new();\n\
                 let mut i: i64 = 0;\n\
                 while i < 5 { args.push(Num(i)); i = i + 1; }\n\
                 let call = Call(CallExpr { callee: Num(100), args: args });\n\
                 println(eval(call).to_string());         // 100+0+1+2+3+4 = 110\n\
                 let folded = fold(build(10));            // by-value transform\n\
                 println(eval(folded).to_string());       // 10+9+..+1+1 = 56\n\
                 let mut v: Vec[Expr] = Vec.new();\n\
                 let mut j: i64 = 0;\n\
                 while j < 4 { v.push(build(j)); j = j + 1; }\n\
                 let mut total: i64 = 0;\n\
                 for e in v { total = total + eval(fold(e)); }\n\
                 println(total.to_string());              // 1+2+4+7 = 14\n\
             }",
    ) {
        assert_eq!(out.trim(), "-6\n110\n56\n14");
    }
}

#[test]
fn test_e2e_shared_enum_payload_with_nested_heap_struct_field() {
    // #44 (phase-12 parser slice 2a): a shared-enum variant whose payload
    // node struct embeds ANOTHER heap-bearing struct BY VALUE — the parser's
    // `IfExpr.then_block: Block` where `Block { stmts: Vec, tail: Option,
    // span: Span }`. The nested `Block` is a multi-word struct (Vec=3w +
    // Option=4w + Span=4w = 11 words) sitting inside the `IfNode` payload.
    // Two stacked bugs, both fixed: (1) the PACK
    // (`coerce_to_payload_words`) recursed with `count_fields()` (3) as the
    // sub-struct's `num_words`, so `out.len()(11) > 3` fired the oversize
    // BOXING path — the nested Block got heap-boxed (pointer in word 0) while
    // the unpack expected 11 inline words; (2) the UNPACK
    // (`reconstruct_payload_value`) rebuilt the nested struct assuming each
    // sub-field was a single word and `insertvalue`d a bare `i64` into a
    // multi-word struct sub-field → \"Invalid InsertValueInst operands\". Pins
    // the build + match round-trip of both a DIRECT block payload
    // (`E.Blk(Block)`) and a NESTED one (`E.Iff(IfNode { then_block: Block })`).
    if let Some(out) = run_program(
            "struct Span { a: i64, b: i64, c: i64, d: i64 }\n\
             shared enum E { Lit(i64), Iff(IfNode), Blk(Block) }\n\
             struct Block { stmts: Vec[i64], tail: Option[E], span: Span }\n\
             struct IfNode { cond: E, then_block: Block, span: Span }\n\
             fn mk_block(first: i64, sp: i64) -> Block {\n\
                 let mut s: Vec[i64] = Vec.new();\n\
                 s.push(first); s.push(first + 1);\n\
                 Block { stmts: s, tail: Some(E.Lit(99)), span: Span { a: sp, b: 0, c: 0, d: 0 } }\n\
             }\n\
             fn main() {\n\
                 let be = E.Blk(mk_block(10, 1));\n\
                 match be {\n\
                     Lit(n) => println(n),\n\
                     Iff(nd) => println(nd.span.a),\n\
                     Blk(b) => {\n\
                         println(b.span.a);\n\
                         println(b.stmts[0]);\n\
                         println(b.stmts[1]);\n\
                         match b.tail { Some(t) => match t { Lit(v) => println(v), Iff(_) => println(-1), Blk(_) => println(-2) }, None => println(-3) }\n\
                     }\n\
                 }\n\
                 let ife = E.Iff(IfNode { cond: E.Lit(7), then_block: mk_block(20, 2), span: Span { a: 5, b: 0, c: 0, d: 0 } });\n\
                 match ife {\n\
                     Lit(n) => println(n),\n\
                     Iff(nd) => {\n\
                         println(nd.span.a);\n\
                         let tb = nd.then_block;\n\
                         println(tb.span.a);\n\
                         println(tb.stmts[0]);\n\
                     }\n\
                     Blk(_) => println(-9)\n\
                 }\n\
             }",
        ) {
            assert_eq!(out.trim(), "1\n10\n11\n99\n5\n2\n20");
        }
}

/// A shared-enum variant whose struct payload is heap-BOXED (wider than its
/// allotted payload words — `struct Block { tail: Option[Expr] }` used as
/// `Expr.Blk(Block)`, the enum-in-enum carve-out undersizing the area) must,
/// in `__karac_rc_drop_<E>`, DEREF word 0 to the box, walk it, then free the
/// box. Before the fix the struct arm always read the payload inline, so it
/// walked the box-POINTER word as the struct's first field (skip) and never
/// freed the box — leaking the box and its heap children. Regression for the
/// boxed-struct arm of `emit_shared_enum_field_drop` (B-2026-06-20). E2E:
/// `memory_sanitizer::asan_single_field_struct_option_payload_sizing_no_bad_access`.
#[test]
fn shared_enum_boxed_struct_payload_rc_drop_unboxes_and_frees() {
    let ir = ir_for(
            "shared enum Expr { Str(String), Blk(Block), Error }\n\
             struct Block { tail: Option[Expr] }\n\
             fn render_block(b: Block) -> String {\n\
             \x20   let Block { tail } = b;\n\
             \x20   match tail { Some(e) => render_expr(e), None => \"n\".to_string() }\n\
             }\n\
             fn render_expr(e: Expr) -> String {\n\
             \x20   match e { Str(s) => s, Blk(b) => render_block(b), Error => \"e\".to_string() }\n\
             }\n\
             fn main() {\n\
             \x20   let blk = Block { tail: Some(Expr.Str(\"payload-string\".to_string())) };\n\
             \x20   println(render_expr(Expr.Blk(blk)));\n\
             }\n",
        );
    let drop_body =
        function_body(&ir, "__karac_rc_drop_Expr").expect("__karac_rc_drop_Expr must be emitted");
    assert!(
        drop_body.contains("nstr.box.p") && drop_body.contains("nstr.box.do"),
        "the boxed-struct variant must unbox (inttoptr) its payload word and \
             walk + free the heap box in the rc-drop\n--- drop body ---\n{drop_body}"
    );
}

/// B-2026-08-17-7 — a user enum variant whose bare name collides with a
/// prelude type constructs BARE in value position, the same meaning
/// pattern position always gave it. `Request`/`Response` are std.http
/// prelude names and `File` is a scope-0 stdlib type; before the fix
/// every bare use below was "'Request' is a type, not a function" while
/// `match e { Request(p) => … }` happily bound the user's variant.
/// Paired with `test_prelude_colliding_variant_ctor_oracle` in
/// `tests/interpreter.rs`.
#[test]
fn test_e2e_prelude_colliding_variant_constructs_bare() {
    assert_eq!(
        run_program(
            r#"
enum Ev { Request(String), Response(i64), Idle }
enum Mode { Fast(i64), File }
fn describe(e: Ev) -> String {
    match e {
        Request(p) => f"req {p}",
        Response(c) => f"resp {c}",
        Idle => "idle",
    }
}
fn main() {
    let a = Request("/users");
    let b = Response(200);
    let c = Idle;
    println(describe(a));
    println(describe(b));
    println(describe(c));
    let m = File;
    match m { Mode.File => println("file"), _ => println("fast") }
}
"#
        ),
        Some("req /users\nresp 200\nidle\nfile\n".to_string())
    );
}

/// B-2026-08-17-41 — a qualified unit-variant match compiles and selects
/// the right arm. The bug that row fixed was in the exhaustiveness
/// ANALYSIS (`Dir.North` lowered to a wildcard, so a non-exhaustive match
/// was accepted and then failed differently on every backend); lowering
/// was always correct, and this pins that it stayed correct. Paired with
/// `test_qualified_unit_variant_match_oracle` in `tests/interpreter.rs`.
#[test]
fn test_e2e_qualified_unit_variant_match() {
    assert_eq!(
        run_program(
            r#"
enum Dir { North, South, East, West }

fn f(d: Dir) -> i64 {
    match d {
        Dir.North => 0,
        Dir.South => 1,
        Dir.East  => 2,
        Dir.West  => 3,
    }
}

fn main() {
    println(f(Dir.North));
    println(f(Dir.South));
    println(f(Dir.East));
    println(f(Dir.West));
}
"#
        ),
        Some("0\n1\n2\n3\n".to_string())
    );
}

/// B-2026-08-27-46 — a method on a bare enum-variant LITERAL agrees with
/// the compiled backends.
///
/// The interpreter alone was wrong here, so the compiled side of this twin
/// was green before the fix and is not the regression signal — the ORACLE
/// is. `run_program_full` runs the interpreter, and it is that half which
/// returned a constant `Ordering` for every operand pair (measured:
/// `2 2 2`) while the AOT binary returned `0 2 1`. Twinning is still the
/// right shape: the defect was a run-vs-build divergence, so the assertion
/// that closes it is the two backends agreeing, not either one in
/// isolation.
///
/// The literal receiver is the whole subject, so every row uses one; the
/// bound-receiver row is carried only as the control that was always
/// correct. `to_fruit` and `to_tag` are here because `cmp` was never
/// special — the trigger is any method on a variant literal whose return
/// type is `Named`, and those two are the enum-valued and struct-valued
/// shapes of it.
#[test]
fn test_e2e_method_on_enum_variant_literal_receiver() {
    let src = r#"
#[derive(Ord, Eq)]
enum E { A, B }

enum Color { Red, Green }
enum Fruit { Red, Apple }

impl Color {
    fn to_fruit(self) -> Fruit {
        match self { Red => return Fruit.Apple, Green => return Fruit.Red, }
    }
}

fn fruit_tag(f: Fruit) -> i64 {
    match f { Red => return 0, Apple => return 1, }
}

fn tag(o: Ordering) -> i64 {
    if o.is_lt() { return 0; }
    if o.is_eq() { return 1; }
    return 2;
}

fn main() {
    println(f"01 {tag(E.A.cmp(E.B))} {tag(E.B.cmp(E.A))} {tag(E.A.cmp(E.A))}");
    let a = E.A;
    let b = E.B;
    println(f"02 {tag(a.cmp(b))} {tag(b.cmp(a))} {tag(a.cmp(a))}");
    println(f"03 {tag(a.cmp(E.B))} {tag(b.cmp(E.A))}");
    println(f"04 {fruit_tag(Color.Red.to_fruit())} {fruit_tag(Color.Green.to_fruit())}");
}
"#;
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(
        interp_errs.is_empty(),
        "interpreter errors: {interp_errs:?}"
    );
    let expected = interp_out.join("");
    // Anti-vacuity, and it is doing real work on this fixture: the failure
    // mode was a CONSTANT ordering, so "all three answers equal" is exactly
    // the wrong output. Pinning row 01 in full is what distinguishes a
    // working comparator from one that returns the same `Ordering` three
    // times, and row 04 pins that the enum-valued method dispatched on the
    // receiver's own type rather than on the retagged one.
    assert!(
        expected.contains("01 0 2 1") && expected.contains("04 1 0"),
        "interpreter oracle is wrong for a variant-literal receiver: {expected:?}"
    );
    let Some(aot) = run_program(src) else { return };
    assert_eq!(
        aot, expected,
        "compiled method call on an enum-variant literal must match the interpreter",
    );
}

/// B-2026-08-31-22 — A DISCARDED BRANCH OF OWN-`Drop` ENUM CONSTRUCTORS
/// dropped its merged value with NO OWNER on either compiled backend: no
/// own body, no payload walk, and the payload's heap stranded.
///
/// THE ROW'S COMPILED MEASUREMENT WAS STALE and the correction is what
/// located the real gate. It recorded `let _ = if c { E.A(mk(8)) } else
/// { E.B };` as running NOTHING on jit/aot; that spelling had since become
/// correct, and it was correct BY ACCIDENT. `discarded_unit_variant_tail`
/// is tried before `discarded_match_value_tail` at both discard sites and
/// peels an `if` to a unit arm's `Path`, so a construct with ONE unit arm
/// was admitted through the unit-variant gate and registered — keyed on the
/// merged value and dispatched on its tag, so right for either arm. Give
/// both arms a payload and the accident is gone:
///
///     let _ = if c { E.A(mk(8)) } else { E.B };        dE dR8   (worked)
///     let _ = if c { E.A(mk(8)) } else { E.A(mk(9)) }; (nothing)
///     let _ = match n { 0 => E.A(mk(8)) _ => E.B };    (nothing)
///
/// TWO GATES, both fixed here. The enum leg at each discard site asked
/// `enum_name_of_expr` about the CONSTRUCT, which is neither a `Call` nor a
/// `Path`, so a two-tail branch registered nothing; it now asks a
/// representative arm tail, the same redirect the arg-position registrar
/// performs at its head. And `discard_arm_tail_qualifies` had no arm for a
/// fresh UNIT variant, so one `_ => E.B` declined the whole construct —
/// which is why the mixed `match`, with no unit-variant peel to fall back
/// on, ran nothing at all.
///
/// THE INTERPRETER PEEL IS THE OTHER HALF, and it could not have landed
/// first. `discard_producer_expr` deliberately refused to peel a two-tail
/// branch, because compiled registered no payload walk there and peeling
/// alone would have traded agreement at ONE body for disagreement at TWO.
/// Fixing the compiled side is what makes the peel correct, so both land
/// together.
///
/// ALL ARMS MUST QUALIFY, on both backends, and that is measured rather
/// than cautious: the walk is VALUE-driven, so it runs over whichever arm's
/// value arrived. An arm handing out a LIVE LOCAL, or producing its enum
/// from a CALL, is declined — a call-produced enum runs its own body alone
/// in the direct spelling (`let _ = mke(8);`), and a branch must not start
/// walking a payload just because a sibling arm is a constructor.
#[test]
fn e2e_discarded_branch_of_enum_ctors_runs_own_body_and_payload() {
    const PRELUDE: &str = "struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\"); } }\n\
             enum E { A(R), B }\n\
             impl Drop for E { fn drop(mut ref self) { println(\"dE\"); } }\n\
             enum G { A(R), B }\n\
             fn mk(n: i64) -> R { return R { id: n }; }\n\
             fn mke(n: i64) -> E { return E.A(mk(n)); }\n";
    let cases: &[(&str, &str, &str)] = &[
            (
                "the row's repro: one ctor arm, one unit arm",
                "fn go(n: i64) -> i64 { let _ = if n == 0 { E.A(mk(8)) } else { E.B }; return 7; }\n\
                 fn take() -> i64 { return go(0); }",
                "dE\ndR8\nv=7\n",
            ),
            (
                "BOTH arms carry a payload",
                "fn go(n: i64) -> i64 { let _ = if n == 0 { E.A(mk(8)) } else { E.A(mk(9)) }; return 7; }\n\
                 fn take() -> i64 { return go(0); }",
                "dE\ndR8\nv=7\n",
            ),
            (
                "the else arm runs",
                "fn go(n: i64) -> i64 { let _ = if n == 0 { E.A(mk(8)) } else { E.A(mk(9)) }; return 7; }\n\
                 fn take() -> i64 { return go(3); }",
                "dE\ndR9\nv=7\n",
            ),
            (
                "the `match` spelling, mixed arms",
                "fn go(n: i64) -> i64 { let _ = match n { 0 => { E.A(mk(8)) } _ => { E.B } }; return 7; }\n\
                 fn take() -> i64 { return go(0); }",
                "dE\ndR8\nv=7\n",
            ),
            (
                "the `match` spelling, both arms payload",
                "fn go(n: i64) -> i64 { let _ = match n { 0 => { E.A(mk(8)) } _ => { E.A(mk(9)) } }; return 7; }\n\
                 fn take() -> i64 { return go(0); }",
                "dE\ndR8\nv=7\n",
            ),
            // The BARE-STATEMENT spelling of each. It reaches a different
            // dispatch on both backends, and the two statement kinds have
            // drifted apart on this trio before (B-2026-08-29-20).
            (
                "bare statement, both arms payload",
                "fn go(n: i64) -> i64 { if n == 0 { E.A(mk(8)) } else { E.A(mk(9)) }; return 7; }\n\
                 fn take() -> i64 { return go(0); }",
                "dE\ndR8\nv=7\n",
            ),
            (
                "bare statement, `match`, mixed arms",
                "fn go(n: i64) -> i64 { match n { 0 => { E.A(mk(8)) } _ => { E.B } }; return 7; }\n\
                 fn take() -> i64 { return go(0); }",
                "dE\ndR8\nv=7\n",
            ),
            (
                "a NESTED branch at the arm tail",
                "fn go(n: i64) -> i64 { let _ = if n == 0 { if n == 0 { E.A(mk(8)) } else { E.B } } else { E.B }; return 7; }\n\
                 fn take() -> i64 { return go(0); }",
                "dE\ndR8\nv=7\n",
            ),
            // CONTROLS. Each was already correct, and together they are what
            // says the branch — not enums, not discards — was the gap.
            (
                "control: the DIRECT spelling",
                "fn go() -> i64 { let _ = E.A(mk(8)); return 7; }\n\
                 fn take() -> i64 { return go(); }",
                "dE\ndR8\nv=7\n",
            ),
            (
                "control: the no-`else` `if`",
                "fn go(n: i64) -> i64 { let _ = if n == 0 { E.A(mk(8)) }; return 7; }\n\
                 fn take() -> i64 { return go(0); }",
                "dE\ndR8\nv=7\n",
            ),
            (
                "control: unit variants in BOTH arms — no payload to walk",
                "fn go(n: i64) -> i64 { let _ = if n == 0 { E.B } else { E.B }; return 7; }\n\
                 fn take() -> i64 { return go(0); }",
                "dE\nv=7\n",
            ),
            // The two arm kinds the all-arms gate must keep DECLINING. A
            // call-produced enum runs its own body alone in the direct
            // spelling, so a branch containing one must not walk its payload.
            (
                // B-2026-09-02-13 — the direct spelling's answer is now
                // `dE dR8`, not `dE`. The invariant this row states is
                // unchanged (a branch containing a call arm agrees with
                // `let _ = mke(8);`); the answer both sides give moved.
                "control: a CALL arm keeps the direct spelling's answer",
                "fn go() -> i64 { let _ = mke(8); return 7; }\n\
                 fn take() -> i64 { return go(); }",
                "dE\ndR8\nv=7\n",
            ),
            (
                "control: an enum with NO own `Drop`, payload only",
                "fn go(n: i64) -> i64 { let _ = if n == 0 { G.A(mk(8)) } else { G.B }; return 7; }\n\
                 fn take() -> i64 { return go(0); }",
                "dR8\nv=7\n",
            ),
        ];
    for (label, decls, want) in cases {
        let src = format!("{PRELUDE}{decls}\nfn main() {{ println(f\"v={{take()}}\"); }}\n");
        let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(&src);
        assert!(
            interp_errs.is_empty(),
            "{label}: interpreter errored: {interp_errs:?}"
        );
        assert_eq!(interp_out.join(""), *want, "{label}: interpreter");
        if let Some(aot) = run_program(&src) {
            assert_eq!(aot, *want, "{label}: own body then payload, once each");
        }
    }
}

/// B-2026-09-02-13 — A DISCARDED ENUM STATEMENT RAN THE ENUM'S OWN `Drop`
/// BODY BUT NOT ITS PAYLOAD'S, on all four surfaces.
///
/// `mk(1);` printed `dB` where `let x = mk(1);` — the identical value, one
/// spelling away — prints `dB dW7`. One `Bx` and one `W` are constructed in
/// each and the two differ in nothing but whether the value gets a name, so
/// the bound local is the oracle and the discard was short a body.
///
/// THE FIX WAS TO RELAX A GATE, NOT TO ADD A WALK, and that distinction is
/// the whole of it. For an enum the own-body wrapper (`karac_drop_<E>`,
/// whose field-cleanup half is a no-op for an enum name) and the payload
/// walker are COMPLEMENTARY registrations. Codegen's discard registrar
/// reached the walker only inside its `field_bodies_only` arm, so an enum
/// that declares its own `Drop` got the wrapper alone. The interpreter's
/// discard gates were then written to MATCH that — each one admits the
/// producers codegen walked, and a call-shaped producer was not among them.
/// So the codegen registration comes first and the interpreter gates WIDEN
/// to follow it; adding a second interpreter walk instead double-fires, as
/// the row records twice.
///
/// AGREED-AND-WRONG, so no A/B gate could see it, and the storage is
/// reclaimed either way — a lost side effect, not a leak.
///
/// THE `match`-WITH-LIVE-LOCAL HALF OF B-2026-09-01-39 CLOSES HERE as a
/// consequence: that spelling was interp `dE dR5` / compiled `dE`, and both
/// now read `dB dW7`. Two rows below pin it. Its `if` sibling is a
/// DIVERGENCE and so cannot be a row of this fixture, which asserts one
/// transcript against both backends — it is unchanged in direction and
/// magnitude by this fix and stays on B-2026-09-01-39, whose own repro
/// carries it.
///
/// The row's third NOT-MEASURED item — whether the payload's own nested
/// Drop-bearing fields are reached once the walker is registered — is
/// answered by `test_a_discarded_enum_statement_runs_its_payloads_drop_body`
/// on the default leg, which carries the three-level `Bn -> Mid -> Leaf`
/// case that does not fit this fixture's single-prelude shape.
#[test]
fn e2e_a_discarded_enum_statement_runs_its_payloads_drop_body() {
    const PRELUDE: &str = "struct W { id: i64 }\n\
             impl Drop for W { fn drop(mut ref self) { println(f\"dW{self.id}\"); } }\n\
             enum Bx { Full(W), Empty(W) }\n\
             impl Drop for Bx { fn drop(mut ref self) { println(\"dB\"); } }\n\
             fn mk(n: i64) -> Bx {\n\
             if n < 1 { return Bx.Full(W { id: n }); }\n\
             return Bx.Empty(W { id: 7 });\n\
             }\n\
             enum NoOwn { Full(W), Empty(W) }\n\
             fn mk2(n: i64) -> NoOwn {\n\
             if n < 1 { return NoOwn.Full(W { id: n }); }\n\
             return NoOwn.Empty(W { id: 7 });\n\
             }\n\
             enum OwnOnly { A(i64), B(i64) }\n\
             impl Drop for OwnOnly { fn drop(mut ref self) { println(\"dO\"); } }\n\
             fn mk3(n: i64) -> OwnOnly {\n\
             if n < 1 { return OwnOnly.A(n); }\n\
             return OwnOnly.B(7);\n\
             }\n\
             struct Fac { k: i64 }\n\
             impl Fac { fn make(ref self) -> Bx { return Bx.Empty(W { id: 7 }); } }\n";
    let cases: &[(&str, &str, &str)] = &[
        (
            "THE ORACLE: the same value, BOUND",
            "let x = mk(1);",
            "dB\ndW7\nmid\n",
        ),
        ("the row: the bare statement", "mk(1);", "dB\ndW7\nmid\n"),
        (
            // The row listed this as NOT MEASURED and it behaves exactly
            // like the bare statement, before and after.
            "the wildcard `let` spelling [row: NOT MEASURED]",
            "let _ = mk(1);",
            "dB\ndW7\nmid\n",
        ),
        (
            // The METHOD producer. Codegen's registrar reaches it through
            // its own MethodCall arm, so it moved with the call spelling
            // and both interpreter sites had to follow.
            "the bare-statement METHOD producer",
            "let f = Fac { k: 1 };\n\
                 f.make();",
            "dB\ndW7\nmid\n",
        ),
        (
            "the wildcard-`let` METHOD producer",
            "let f = Fac { k: 1 };\n\
                 let _ = f.make();",
            "dB\ndW7\nmid\n",
        ),
        (
            // The row listed a HEAP-carrying payload as NOT MEASURED. It
            // behaves as the all-scalar one does; the body reads the buffer
            // (`len()`), so a walk over freed storage would show here.
            "a HEAP-carrying payload [row: NOT MEASURED]",
            "mk(1);",
            "dB\ndW7\nmid\n",
        ),
        (
            "a branch of CALLS, bare",
            "let c: bool = true;\n\
                 if c { mk(1) } else { mk(0) };",
            "dB\ndW7\nmid\n",
        ),
        (
            "a branch of CALLS, wildcard `let`",
            "let c: bool = true;\n\
                 let _ = if c { mk(1) } else { mk(0) };",
            "dB\ndW7\nmid\n",
        ),
        (
            // A branch mixing an inline ctor with a call: the ctor arm was
            // already correct and the call arm is what this row admits, so
            // the mixed shape is the one that proves the two agree.
            "a branch mixing a ctor arm with a call arm",
            "let c: bool = true;\n\
                 let _ = if c { Bx.Empty(W { id: 7 }) } else { mk(0) };",
            "dB\ndW7\nmid\n",
        ),
        (
            "the `match` spelling of a call branch",
            "let c: bool = true;\n\
                 match c { true => mk(1), false => mk(0) };",
            "dB\ndW7\nmid\n",
        ),
        (
            // B-2026-09-01-39's `match` row, which closes here: it was
            // interp `dE dR5` against compiled `dE`, and the arm taken is
            // the one naming the LIVE local.
            "the `match` live-local arm [closes half of B-2026-09-01-39]",
            "let c: bool = false;\n\
                 let e = mk(1);\n\
                 let _ = match c { true => Bx.Empty(W { id: 9 }), _ => e };",
            "dB\ndW7\nmid\n",
        ),
        (
            "…and its bare-statement spelling",
            "let c: bool = false;\n\
                 let e = mk(1);\n\
                 match c { true => Bx.Empty(W { id: 9 }), _ => e };",
            "dB\ndW7\nmid\n",
        ),
        (
            // CONTROL, unchanged by the fix: no own `Drop`, so the discard
            // took the field-bodies leg all along and walked the payload.
            "control: an enum with NO own `Drop` but a Drop-bearing payload",
            "mk2(1);",
            "dW7\nmid\n",
        ),
        (
            // CONTROL, the other direction: an own `Drop` and a payload
            // that has none, so the own body alone is the whole answer.
            "control: an own-`Drop` enum whose payload has no `Drop`",
            "mk3(1);",
            "dO\nmid\n",
        ),
        (
            // CONTROL for the ordering: the wrapper is pushed AFTER the
            // walker so the LIFO discard frame drains own-body-then-payload,
            // which is the order the bound local produces. An inverted push
            // reads `dW7 dB` here.
            "control: an inline ctor discard was correct throughout",
            "let _ = Bx.Empty(W { id: 7 });",
            "dB\ndW7\nmid\n",
        ),
    ];
    for (label, body, want) in cases {
        // The heap row swaps the prelude's payload for a `String`-carrying
        // one; every other row uses the scalar `W`.
        let src = if label.starts_with("a HEAP") {
            "struct H { tag: String }\n\
                 impl Drop for H { fn drop(mut ref self) { println(f\"dW{self.tag.len()}\"); } }\n\
                 enum Bx { Full(H), Empty(H) }\n\
                 impl Drop for Bx { fn drop(mut ref self) { println(\"dB\"); } }\n\
                 fn mk(n: i64) -> Bx {\n\
                 if n < 1 { return Bx.Full(H { tag: \"x\" }); }\n\
                 return Bx.Empty(H { tag: \"1234567\" });\n\
                 }\n\
                 fn main() {\n    mk(1);\n    println(\"mid\");\n}\n"
                .to_string()
        } else {
            format!("{PRELUDE}fn main() {{\n    {body}\n    println(\"mid\");\n}}\n")
        };
        let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(&src);
        assert!(
            interp_errs.is_empty(),
            "{label}: interpreter errored: {interp_errs:?}"
        );
        assert_eq!(
            interp_out.join(""),
            *want,
            "{label}: interpreter transcript"
        );
        if let Some(aot) = run_program(&src) {
            assert_eq!(
                aot, *want,
                "{label}: the compiled backends must agree with the interpreter"
            );
        }
    }
}

/// B-2026-08-27-19 — `==` on an enum was gated on a DROP classification, so
/// an enum the drop path deliberately declines to own compared its payload
/// WORDS.
///
/// `enum_has_heap_payload` folds over `field_drop_kinds`, and
/// `enum_drop_kind_for_type_expr` refuses to classify a payload struct
/// `NestedStruct` unless it is word-ALIGNED — because the drop path and the
/// deep-copy-on-entry must stay symmetric, and classifying one the entry-copy
/// cannot duplicate turns a status-quo leak into a DOUBLE FREE. That `None` is
/// correct for ownership and wrong as an answer to "can these bytes be
/// compared as words". `subword` is the shape: `{ a: i32, b: i32, s: String }`
/// spends one word per field where LLVM packs the two `i32`s into eight bytes,
/// so it is not word-aligned, and `==` compared the `String`'s heap POINTER.
///
/// `zeros` and `nan` are the SECOND symptom of the same gate, and the reason
/// the new predicate is not simply "owns heap". A float payload has no heap at
/// all, so no drop classification would ever have routed it — but bit equality
/// is not IEEE equality in exactly the two places the standard defines
/// specially: `0.0` and `-0.0` are equal with different bits, NaN is unequal
/// to itself with identical bits. Both answered backwards. The operands come
/// from opaque functions so LLVM cannot constant-fold the comparison and
/// answer correctly without running the emitted code.
///
/// `scalarstruct` and `ints` are the controls: an all-scalar payload, struct
/// or not, is wholly inline and keeps the cheaper word compare. `AllScalar`
/// is deliberately three `i32`s — also not word-aligned, so it proves the
/// predicate keys on what the bytes MEAN rather than on alignment.
///
/// The `-tag` lines guard the same dereference hazard B-2026-08-27-16 closed:
/// the payload walk must not run when the tags differ, since a unit variant's
/// uninitialised words become a wild pointer once a field is read through.
#[test]
fn test_e2e_enum_eq_is_not_gated_on_a_drop_classification() {
    assert_eq!(
        run_program(
            r#"
#[derive(Hash, Eq, PartialEq)]
struct TwoNarrow { a: i32, b: i32, s: String }
#[derive(Hash, Eq, PartialEq)]
struct AllScalar { a: i32, b: i32, c: i32 }
#[derive(Hash, Eq, PartialEq)]
enum Holder { W(TwoNarrow), S(AllScalar), N }
#[derive(PartialEq)]
enum Num { F(f64), I(i64), N }

fn mk(n: i64) -> String { return f"v{n}"; }
fn nan64(z: f64) -> f64 { return z / z; }
fn negzero(z: f64) -> f64 { return -z; }

fn main() {
    let x: Holder = Holder.W(TwoNarrow { a: 1, b: 2, s: mk(1) });
    let y: Holder = Holder.W(TwoNarrow { a: 1, b: 2, s: mk(1) });
    println(f"subword={x == y}");
    println(f"subword-ne-s={x == Holder.W(TwoNarrow { a: 1, b: 2, s: mk(2) })}");
    println(f"subword-ne-a={x == Holder.W(TwoNarrow { a: 9, b: 2, s: mk(1) })}");
    println(f"subword-ne-b={x == Holder.W(TwoNarrow { a: 1, b: 9, s: mk(1) })}");
    println(f"subword-tag={x == Holder.N}");

    let p: Holder = Holder.S(AllScalar { a: 1, b: 2, c: 3 });
    println(f"scalarstruct={p == Holder.S(AllScalar { a: 1, b: 2, c: 3 })}");
    println(f"scalarstruct-ne={p == Holder.S(AllScalar { a: 1, b: 2, c: 4 })}");

    let z = 0.0;
    let neg = negzero(z);
    let nan = nan64(z);
    println(f"zeros={Num.F(0.0) == Num.F(neg)}");
    println(f"nan={Num.F(nan) == Num.F(nan)}");
    println(f"floats={Num.F(1.5) == Num.F(1.5)}");
    println(f"floats-ne={Num.F(1.5) == Num.F(2.5)}");
    println(f"ints={Num.I(7) == Num.I(7)}");
    println(f"ints-ne={Num.I(7) == Num.I(8)}");
    println(f"num-tag={Num.F(1.5) == Num.N}");
}
"#
        ),
        Some(
            "subword=true\nsubword-ne-s=false\nsubword-ne-a=false\n\
                 subword-ne-b=false\nsubword-tag=false\n\
                 scalarstruct=true\nscalarstruct-ne=false\nzeros=true\n\
                 nan=false\nfloats=true\nfloats-ne=false\nints=true\n\
                 ints-ne=false\nnum-tag=false\n"
                .to_string()
        )
    );
}

/// B-2026-08-27-17 — `assert_eq` / `assert_ne` compared an ENUM by its
/// payload WORDS, so an assertion over two structurally-equal enums PASSED on
/// the interpreter and FAILED compiled. A test asserting enum equality got the
/// wrong verdict, which is worse than a wrong value: it makes a correct
/// program look broken, and for `assert_ne` a broken one look correct.
///
/// `test_assert.rs` compiled its two operands and called `compile_binop`
/// directly. That dispatches an aggregate by shape and sent the enum to the
/// word-wise `compile_struct_eq` — a heap-pointer compare for any payload
/// owning heap. The `==` OPERATOR site had a structural path all along
/// (`compile_enum_eq`), selected by running `enum_name_of_expr` over the
/// operand EXPRESSIONS; the assertion helper simply never consulted it.
///
/// The fix lifts that decision into one helper both sites call, so "how a
/// compiled `==` on an enum is decided" has a single definition. Unlike
/// `Vec.contains` (B-2026-08-27-13), this site HAS the operand expressions, so
/// it can reach the operator's own path rather than needing the pointer
/// comparator.
///
/// EVERY PAYLOAD IS BUILT BY A FUNCTION CALL, never a literal: two identical
/// string literals can fold to one global, and a pointer compare would then
/// answer `true` and hide the defect. The struct, string and int lines are
/// controls that were already correct — `compile_struct_eq` recurses per
/// FIELD, and only an enum's fields are raw payload words.
#[test]
fn test_e2e_assert_eq_compares_an_enum_by_content() {
    assert_eq!(
        run_program(
            r#"
#[derive(Hash, Eq, PartialEq)]
enum E { A(String), Pair(i64, String), B }
#[derive(Hash, Eq, PartialEq)]
struct S { a: i64, s: String }

fn mk(n: i64) -> String { return f"v{n}"; }

fn main() {
    assert_eq(E.A(mk(1)), E.A(mk(1)));
    println(f"eq-payload ok");
    assert_ne(E.A(mk(1)), E.A(mk(2)));
    println(f"ne-payload ok");
    assert_eq(E.Pair(7, mk(3)), E.Pair(7, mk(3)));
    println(f"eq-tuple ok");
    assert_ne(E.Pair(7, mk(3)), E.Pair(7, mk(4)));
    println(f"ne-tuple ok");
    assert_eq(E.B, E.B);
    println(f"eq-unit ok");
    assert_ne(E.A(mk(1)), E.B);
    println(f"ne-tag ok");

    let a: Option[String] = Some(mk(5));
    let b: Option[String] = Some(mk(5));
    let c: Option[String] = Some(mk(6));
    assert_eq(a, b);
    println(f"eq-generic ok");
    assert_ne(a, c);
    println(f"ne-generic ok");

    assert_eq(S { a: 1, s: mk(7) }, S { a: 1, s: mk(7) });
    println(f"eq-struct ok");
    assert_eq(mk(8), mk(8));
    println(f"eq-string ok");
    assert_eq(1, 1);
    println(f"eq-int ok");
}
"#
        ),
        Some(
            "eq-payload ok\nne-payload ok\neq-tuple ok\nne-tuple ok\n\
                 eq-unit ok\nne-tag ok\neq-generic ok\nne-generic ok\n\
                 eq-struct ok\neq-string ok\neq-int ok\n"
                .to_string()
        )
    );
}

/// B-2026-08-27-16 — `==` on a generic enum whose argument OVERFLOWS its
/// erased payload allotment PANICKED THE COMPILER, and a wide inline payload
/// was compared by its words.
///
/// `compile_enum_eq` packed offsets sequentially from the RESOLVED field
/// widths. That is right only while every field still fits the allotment the
/// erased layout reserved; when it does not,
/// `coerce_to_payload_words` heap-BOXED the field into one word at
/// construction, so the sequential sum indexed past an area that never held
/// those words and `build_extract_value` returned an unwrapped `Err`.
/// `boxed` and `boxed2` are that shape — `enum Box1[T] { One(T) }` allots `T`
/// ONE word, so a `String` does not fit. Before the fix this fixture did not
/// produce a wrong answer; `karac build` aborted.
///
/// `wide` is the second defect: a field over three words fell to a
/// best-effort word-wise compare, so `struct Big { a: i64, b: i64, s: String }`
/// (five words) compared the `String`'s heap POINTER and `x == y` answered
/// `false` for equal contents.
///
/// THE `-tag` LINES ARE THE SOUNDNESS CASE, and they are why the payload walk
/// is now gated on tag equality. The switch dispatches on the LEFT tag, so
/// with mismatched tags it still entered that variant's block and read the
/// RIGHT operand's words as that variant's payload. That was harmless while
/// those reads were integer extracts feeding an `icmp` — the tag conjunction
/// discarded the answer — but the fix DEREFERENCES a field (a boxed payload
/// is `inttoptr` + load, a rebuilt one is loaded through by its comparator),
/// and an uninitialised word from a unit variant is a wild pointer.
/// `Box1.One(s) == Box1.Nil` segfaulted under the JIT. One line per read
/// mode, since each dereferences differently.
///
/// `opt-scalar` is the control: a payload that fits its allotment and is
/// wholly inline was correct before and must stay so.
#[test]
fn test_e2e_enum_eq_handles_a_boxed_or_wide_generic_payload() {
    assert_eq!(
        run_program(
            r#"
#[derive(Hash, Eq, PartialEq)]
enum Box1[T] { One(T), Nil }
#[derive(Hash, Eq, PartialEq)]
enum Pair2[T] { P(T, i64), Nil }
#[derive(Hash, Eq, PartialEq)]
struct Big { a: i64, b: i64, s: String }
#[derive(Hash, Eq, PartialEq)]
enum H { W(Big), N }

fn mk(n: i64) -> String { return f"v{n}"; }

fn main() {
    let a: Box1[String] = Box1.One(mk(1));
    let b: Box1[String] = Box1.One(mk(1));
    let c: Box1[String] = Box1.One(mk(2));
    println(f"boxed={a == b}");
    println(f"boxed-ne={a == c}");
    println(f"boxed-tag={a == Box1.Nil}");

    let d: Pair2[String] = Pair2.P(mk(3), 7);
    let e: Pair2[String] = Pair2.P(mk(3), 7);
    let g: Pair2[String] = Pair2.P(mk(4), 7);
    let h: Pair2[String] = Pair2.P(mk(3), 8);
    println(f"boxed2={d == e}");
    println(f"boxed2-ne-s={d == g}");
    println(f"boxed2-ne-n={d == h}");
    println(f"boxed2-tag={d == Pair2.Nil}");

    let i: H = H.W(Big { a: 1, b: 2, s: mk(5) });
    let j: H = H.W(Big { a: 1, b: 2, s: mk(5) });
    let k: H = H.W(Big { a: 1, b: 2, s: mk(6) });
    let l: H = H.W(Big { a: 1, b: 9, s: mk(5) });
    println(f"wide={i == j}");
    println(f"wide-ne-s={i == k}");
    println(f"wide-ne-b={i == l}");
    println(f"wide-tag={i == H.N}");

    let m: Option[String] = Some(mk(7));
    let n: Option[String] = Some(mk(7));
    println(f"opt={m == n}");
    let q: Option[String] = None;
    println(f"opt-tag={m == q}");
    let o: Option[i64] = Some(5);
    let p: Option[i64] = Some(5);
    println(f"opt-scalar={o == p}");
}
"#
        ),
        Some(
            "boxed=true\nboxed-ne=false\nboxed-tag=false\n\
                 boxed2=true\nboxed2-ne-s=false\nboxed2-ne-n=false\n\
                 boxed2-tag=false\nwide=true\nwide-ne-s=false\n\
                 wide-ne-b=false\nwide-tag=false\nopt=true\n\
                 opt-tag=false\nopt-scalar=true\n"
                .to_string()
        )
    );
}

/// B-2026-08-27-12 — the two payload shapes B-2026-08-27-6's structural enum
/// key walk DECLINED, so they kept the byte compare it replaced and stayed
/// wrong: a GENERIC enum key, and a payload struct whose word image is not its
/// LLVM layout.
///
/// BOTH ARE ONE CAUSE — reading a payload by pointer-casting into its words
/// only works where the words ARE the value. Three ways they are not, and the
/// three read modes that answer them:
///
///   * `subword` — `struct { a: i32, b: i32, s: String }` spends three word
///     runs where LLVM packs the two `i32`s into eight bytes, so `s` sits at
///     byte 8 by the word stream and byte 8 by LLVM only by luck; field `b`
///     does not. Rebuilt from its words through the same helper a match arm
///     binds a payload with. `subword-ne-b` is the line that catches a walk
///     reading `b` out of `a`'s padding.
///   * `opt` / `res` — the payload type is the enum's PARAMETER, resolved by
///     substituting the instantiation, exactly as `compile_enum_eq` does.
///   * `boxed` / `boxed2` — `enum Box1[T] { One(T) }` allots `T` ONE word from
///     its erased layout, so a `String` argument does not fit and construction
///     heap-BOXES it. The word holds a box pointer, not the value.
///
/// `inst-a` and `inst-b` are the collision guard. `mangled_type_name` drops
/// generic arguments, so `Option[String]` and `Option[i64]` would share one
/// `karac_eq_Option`; once the comparator resolves payloads per instantiation,
/// whichever was emitted first would answer for both. Two live maps of
/// different instantiations in one program is what makes that observable.
///
/// The `-ne` lines are not padding: a comparator that answered `true`
/// unconditionally passes every positive assertion above them. `dedup` and
/// `removed` check that hash moved with eq — equal keys that hash differently
/// never meet in the same bucket.
#[test]
fn test_e2e_a_generic_or_sub_word_enum_key_is_matched_by_content() {
    assert_eq!(
        run_program(
            r#"
#[derive(Hash, Eq, PartialEq)]
struct TwoNarrow { a: i32, b: i32, s: String }
#[derive(Hash, Eq, PartialEq)]
enum Holder { W(TwoNarrow), N }
#[derive(Hash, Eq, PartialEq)]
enum Box1[T] { One(T), Nil }
#[derive(Hash, Eq, PartialEq)]
enum Pair2[T] { P(T, i64), Nil }

fn main() {
    let mut m: Map[Holder, i64] = Map.new();
    m.insert(Holder.W(TwoNarrow { a: 1, b: 2, s: f"zz" }), 7);
    println(f"subword={m.contains_key(Holder.W(TwoNarrow { a: 1, b: 2, s: f"zz" }))}");
    println(f"subword-ne-s={m.contains_key(Holder.W(TwoNarrow { a: 1, b: 2, s: f"zy" }))}");
    println(f"subword-ne-b={m.contains_key(Holder.W(TwoNarrow { a: 1, b: 3, s: f"zz" }))}");

    let mut o: Map[Option[String], i64] = Map.new();
    o.insert(Some(f"kk"), 1);
    o.insert(None, 2);
    println(f"opt={o.contains_key(Some(f"kk"))}");
    println(f"opt-ne={o.contains_key(Some(f"kj"))}");
    println(f"opt-none={o.contains_key(None)}");

    let mut r: Map[Result[String, i64], i64] = Map.new();
    r.insert(Ok(f"rr"), 1);
    r.insert(Err(9), 2);
    println(f"res-ok={r.contains_key(Ok(f"rr"))}");
    println(f"res-ok-ne={r.contains_key(Ok(f"rq"))}");
    println(f"res-err={r.contains_key(Err(9))}");

    let mut b: Map[Box1[String], i64] = Map.new();
    b.insert(Box1.One(f"bb"), 1);
    println(f"boxed={b.contains_key(Box1.One(f"bb"))}");
    println(f"boxed-ne={b.contains_key(Box1.One(f"bc"))}");

    let mut p: Map[Pair2[String], i64] = Map.new();
    p.insert(Pair2.P(f"pp", 3), 1);
    println(f"boxed2={p.contains_key(Pair2.P(f"pp", 3))}");
    println(f"boxed2-ne-s={p.contains_key(Pair2.P(f"pq", 3))}");
    println(f"boxed2-ne-n={p.contains_key(Pair2.P(f"pp", 4))}");

    let mut i: Map[Option[i64], i64] = Map.new();
    i.insert(Some(5), 1);
    println(f"scalar={i.contains_key(Some(5))}");
    println(f"scalar-ne={i.contains_key(Some(6))}");

    let mut two: Map[Option[String], i64] = Map.new();
    two.insert(Some(f"aa"), 1);
    let mut three: Map[Option[i64], i64] = Map.new();
    three.insert(Some(7), 1);
    println(f"inst-a={two.contains_key(Some(f"aa"))}");
    println(f"inst-b={three.contains_key(Some(7))}");

    let mut s: Set[Box1[String]] = Set.new();
    s.insert(Box1.One(f"dup"));
    s.insert(Box1.One(f"dup"));
    println(f"dedup={s.len()}");
    b.remove(Box1.One(f"bb"));
    println(f"removed={b.len()}");
}
"#
        ),
        Some(
            "subword=true\nsubword-ne-s=false\nsubword-ne-b=false\n\
                 opt=true\nopt-ne=false\nopt-none=true\nres-ok=true\n\
                 res-ok-ne=false\nres-err=true\nboxed=true\n\
                 boxed-ne=false\nboxed2=true\nboxed2-ne-s=false\n\
                 boxed2-ne-n=false\nscalar=true\nscalar-ne=false\n\
                 inst-a=true\ninst-b=true\ndedup=1\nremoved=0\n"
                .to_string()
        )
    );
}

/// B-2026-08-27-6 — a plain (non-shared) ENUM key with a heap-bearing
/// payload was compared by its payload WORDS instead of recursing, so the
/// compiled backends missed a structurally-equal key the interpreter found.
/// `emit_eq_fn_for_type_expr` had arms for tuples, `Vec` and STRUCTS but
/// none for enums, and the byte-compare fallback it fell to reads a
/// `String` payload's two distinct heap POINTERS.
///
/// THE SCALAR-PAYLOAD LEG IS THE CONTROL, and the reason this sat unseen:
/// a byte compare IS correct when the whole value is inline, so the common
/// enum key always worked.
///
/// `Scalar(5)` and `Alt(5)` are the tag leg — identical payload words under
/// different discriminants. They must not collide, which is what a walk
/// that compared payloads without first gating on the tag would do.
///
/// EQ AND HASH HAD TO MOVE TOGETHER; `dedup` and `removed` are what check
/// it. Equal keys that hash differently never land in the same bucket, so
/// an equality-only fix leaves every lookup missing exactly as before.
///
/// THE LAST TWO LINES PIN THE TWO PATHS TOGETHER. `==` on an enum VALUE
/// does not use this comparator at all — the operator site routes a
/// heap-payload enum to `compile_enum_eq`, which has walked tags and
/// rebuilt payloads structurally all along. That asymmetry IS this bug:
/// `x == y` and `m.contains_key(y)` disagreed on the same two values,
/// because only one of the two enum comparators in this compiler knew how
/// to look past the payload words. They must keep agreeing.
///
/// The `-ne` lines are not padding: a comparator that answered `true`
/// unconditionally passes every positive assertion above them.
///
/// `shared` closes the loop with B-2026-08-27-4, whose shared-enum leg was
/// defined as "behaves as its non-shared twin does" and so inherited this
/// defect. Fixing the plain enum without it would have broken that parity
/// in the other direction.
#[test]
fn test_e2e_an_enum_key_with_a_heap_payload_is_matched_by_content() {
    assert_eq!(
        run_program(
            r#"
#[derive(Hash, Eq, PartialEq)]
struct Inner { id: i64, tag: String }
#[derive(Hash, Eq, PartialEq)]
enum Shape {
    Unit,
    Scalar(i64),
    Alt(i64),
    Text { s: String },
    Pair(i64, String),
    Items(Vec[i64]),
    Nested(Inner),
    Narrow(u8, i64),
}
#[derive(Hash, Eq, PartialEq)]
shared enum Node { Named { s: String }, Anon }
fn main() {
    let mut m: Map[Shape, i64] = Map.new();
    m.insert(Shape.Unit, 1);
    m.insert(Shape.Scalar(5), 2);
    m.insert(Shape.Alt(5), 3);
    m.insert(Shape.Text { s: f"hello" }, 4);
    m.insert(Shape.Pair(9, f"pp"), 5);
    m.insert(Shape.Items(vec![1, 2, 3]), 6);
    m.insert(Shape.Nested(Inner { id: 7, tag: f"nn" }), 7);
    m.insert(Shape.Narrow(200, 11), 8);
    println(f"len={m.len()}");
    println(f"unit={m.contains_key(Shape.Unit)}");
    println(f"text={m.contains_key(Shape.Text { s: f"hello" })}");
    println(f"text-ne={m.contains_key(Shape.Text { s: f"hellp" })}");
    println(f"pair={m.contains_key(Shape.Pair(9, f"pp"))}");
    println(f"pair-ne={m.contains_key(Shape.Pair(9, f"pq"))}");
    println(f"items={m.contains_key(Shape.Items(vec![1, 2, 3]))}");
    println(f"items-ne={m.contains_key(Shape.Items(vec![1, 2, 4]))}");
    println(f"nested={m.contains_key(Shape.Nested(Inner { id: 7, tag: f"nn" }))}");
    println(f"nested-ne={m.contains_key(Shape.Nested(Inner { id: 7, tag: f"nm" }))}");
    println(f"narrow={m.contains_key(Shape.Narrow(200, 11))}");
    println(f"narrow-ne={m.contains_key(Shape.Narrow(201, 11))}");
    match m.get(Shape.Scalar(5)) { Some(v) => { println(f"tag-a={v}") } None => { println(f"tag-a=miss") } }
    match m.get(Shape.Alt(5)) { Some(v) => { println(f"tag-b={v}") } None => { println(f"tag-b=miss") } }

    let mut s: Set[Shape] = Set.new();
    s.insert(Shape.Text { s: f"dup" });
    s.insert(Shape.Text { s: f"dup" });
    println(f"dedup={s.len()}");
    m.remove(Shape.Text { s: f"hello" });
    println(f"removed={m.len()}");

    let mut n: Map[Node, i64] = Map.new();
    n.insert(Node.Named { s: f"aa" }, 1);
    println(f"shared={n.contains_key(Node.Named { s: f"aa" })}");
    println(f"shared-ne={n.contains_key(Node.Named { s: f"ab" })}");

    let p = Shape.Text { s: f"eq" };
    let q = Shape.Text { s: f"eq" };
    let r = Shape.Text { s: f"ne" };
    println(f"eq={p == q}");
    println(f"eq-ne={p == r}");
}
"#
        ),
        Some(
            "len=8\nunit=true\ntext=true\ntext-ne=false\npair=true\n\
                 pair-ne=false\nitems=true\nitems-ne=false\nnested=true\n\
                 nested-ne=false\nnarrow=true\nnarrow-ne=false\ntag-a=2\n\
                 tag-b=3\ndedup=1\nremoved=7\nshared=true\n\
                 shared-ne=false\neq=true\neq-ne=false\n"
                .to_string()
        )
    );
}

/// B-2026-08-15-12 — a user enum may shadow a prelude handle type; codegen
/// must lower it as the enum the user wrote.
///
/// The shadow registry (`user_shadowed_prelude_types`, B-2026-08-08-10) was
/// populated only by the STRUCT declaration pass, so a user ENUM never
/// entered it: `builtin_opaque_ptr_handle` won on the `TypeExpr` lowering
/// path, a `ref Request` parameter became an opaque `ptr` rather than the
/// tagged union, and the payload binding was never registered — surfacing
/// as `codegen failed: Undefined variable 'x'` on a program `karac check`
/// accepts and the interpreter runs correctly.
///
/// IR, not E2E, is the right pin: the failure was a COMPILE ERROR, so a
/// successful `compile_to_ir` is itself the assertion, and it holds without
/// the runtime archives an E2E test soft-skips without. The whole list is
/// swept because the defect was never about `Request` — that name is simply
/// what the router dogfood happened to use.
#[test]
fn test_user_enum_shadowing_a_prelude_handle_type_lowers_as_the_users_enum() {
    for name in SHADOWABLE_HANDLE_NAMES {
        let ir = ir_for(&format!(
            "enum {name} {{ A(i64), B }}\n\
                 fn f(e: ref {name}) -> i64 {{ match e {{ A(x) => x, B => 0 }} }}\n\
                 fn g(e: {name}) -> i64 {{ match e {{ A(x) => x + 1, B => 0 }} }}\n\
                 fn main() {{ println(f({name}.A(4))); println(g({name}.A(4))); }}\n"
        ));
        // The user's enum is a tagged union, never the built-in's bare
        // `ptr` — if the shadow were still unrecorded this call would have
        // panicked in `ir_for` long before here.
        assert!(
            ir.contains("define"),
            "{name}: shadowing enum produced no IR",
        );
    }
}

/// The E2E half of B-2026-08-15-12: shadowing must produce the RIGHT
/// ANSWER, not merely compile.
///
/// A fix that registered the shadow but resolved it to the wrong layout
/// would satisfy the IR pin above and silently read the payload from the
/// wrong offset. `karac run --interp` printed `4` and `5` for this program
/// throughout the bug's life, so the interpreter is the oracle and these
/// are the values codegen has to match.
#[test]
fn test_e2e_user_enum_shadowing_a_prelude_handle_type_reads_its_payload() {
    for name in SHADOWABLE_HANDLE_NAMES {
        let Some(out) = run_program(&format!(
            "enum {name} {{ A(i64), B }}\n\
                 fn f(e: ref {name}) -> i64 {{ match e {{ A(x) => x, B => 0 }} }}\n\
                 fn g(e: {name}) -> i64 {{ match e {{ A(x) => x + 1, B => 0 }} }}\n\
                 fn main() {{ println(f({name}.A(4))); println(g({name}.A(4))); \
                     println(f({name}.B)); }}\n"
        )) else {
            return;
        };
        assert_eq!(out, "4\n5\n0\n", "{name}: wrong payload read");
    }
}

/// The row's own repro, whole — a request router with a `shared enum`
/// payload passed to a `ref`-taking helper.
///
/// Kept alongside the swept list because it is the shape that FOUND the
/// defect, and because it exercises the parts the reduction dropped: a
/// nested enum payload, a `shared` inner enum, and the binding forwarded to
/// another function rather than read in place.
#[test]
fn test_e2e_shadowed_enum_router_with_shared_payload() {
    let Some(out) = run_program(
        "shared enum Method { Get, Post, Delete }\n\
             enum Request { Simple(Method, String), WithBody(Method, String) }\n\
             fn method_name(m: ref Method) -> String {\n\
                 match m { Get => \"GET\", Post => \"POST\", Delete => \"DELETE\" }\n\
             }\n\
             fn handle(r: ref Request) -> String {\n\
                 match r {\n\
                     Simple(m, _path) => method_name(m),\n\
                     WithBody(_m, _path) => \"withbody\",\n\
                 }\n\
             }\n\
             fn main() {\n\
                 let mut reqs: Vec[Request] = Vec.new();\n\
                 reqs.push(Request.Simple(Method.Post, \"/\"));\n\
                 reqs.push(Request.WithBody(Method.Get, \"/x\"));\n\
                 for i in 0..reqs.len() { println(handle(reqs[i])); }\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "POST\nwithbody\n");
}

/// A struct `invariant` reached through a GENERIC method — the third
/// contract kind, and it rides the same frame.
#[test]
fn test_e2e_generic_method_checks_the_struct_invariant() {
    let src = r#"
struct Holder {
    items: Vec[i64],

    invariant self.items.len() < 4
}
impl Holder {
    pub fn add[T](mut ref self, x: T) -> i64 where T: Ord {
        self.items.push(1);
        self.items.len()
    }
}
fn main() {
    let mut h = Holder { items: [1, 2] };
    println(h.add(3));
    println(h.add(4));
}
"#;
    let cap = run_program_capturing(src).expect("build the invariant program");
    assert!(
        cap.stdout.starts_with("3\n"),
        "the first call satisfies the invariant and must print; got stdout={:?}",
        cap.stdout
    );
    assert!(
        cap.stderr.contains("contract violated: invariant"),
        "the second call breaks it and must abort; got stdout={:?}",
        cap.stdout
    );
}

/// B-2026-08-21-10 — C-like enum `.discriminant()`, end to end.
///
/// The value is NOT the tag. design.md is explicit that declared
/// discriminants are not layout commitments at v1, so codegen lays these
/// enums out at declaration positions 0/1/2 and maps them to the DECLARED
/// values with a select chain. A lowering that just returned the tag would
/// answer 0/1/2 here and pass any test whose declared values happened to
/// be 0, 1, 2 — hence the deliberately non-positional `0x01 / 0x03 / 0x08`
/// and the constant-expression variant.
#[test]
fn test_e2e_discriminant_reads_the_declared_value() {
    let src = r#"
const BASE: i64 = 16;

#[repr(u8)]
enum UsbClass { Audio = 0x01, Hid = 0x03, MassStorage = 0x08 }

enum Plain { A, B, C }

#[repr(i8)]
enum Neg { Down = -128, Up = 127 }

#[repr(u16)]
enum Wide { A = 1000, B = 65535 }

#[repr(u8)]
enum Op { Add = BASE + 1, Sub = BASE + 2 }

struct Holder { kind: UsbClass }

impl Op {
    fn code(ref self) -> u8 { self.discriminant() }
}

fn as_u8(k: UsbClass) -> u8 { k.discriminant() }

fn main() {
    // The spec's example.
    let c = UsbClass.Hid;
    let byte: u8 = c.discriminant();
    println(byte);
    println(UsbClass.Audio.discriminant());
    println(UsbClass.MassStorage.discriminant());

    // No declared values: declaration position, as in C.
    println(Plain.A.discriminant());
    println(Plain.C.discriminant());

    // Signed and wide reprs at their boundaries.
    println(Neg.Down.discriminant());
    println(Neg.Up.discriminant());
    println(Wide.A.discriminant());
    println(Wide.B.discriminant());

    // A folded constant expression — the fold happens once, in the
    // typechecker, so both backends read the same number.
    println(Op.Add.discriminant());

    // Every receiver shape: struct field, by-value parameter, `self`.
    let h = Holder { kind: UsbClass.MassStorage };
    println(h.kind.discriminant());
    println(as_u8(UsbClass.Audio));
    let o = Op.Sub;
    println(o.code());
}
"#;
    assert_eq!(
        run_program(src).as_deref(),
        Some("3\n1\n8\n0\n2\n-128\n127\n1000\n65535\n17\n8\n1\n18\n")
    );
}

/// B-2026-08-22-1 — `MemoryOrdering` reached codegen with NO enum layout:
/// it is absent from `compiled_stdlib_programs` (which is what gives
/// `Stdio` and `PoolError` theirs) and had no seed. Every match arm,
/// qualified and bare alike, then compiled to a BINDING pattern instead of
/// a tag test, so the first arm always matched and all five orderings
/// printed `Relaxed` — silently, in a concurrency type. Measured before
/// the seed: `a=Relaxed b=Relaxed c=Relaxed` against the interpreter's
/// `a=Relaxed b=Acquire c=SeqCst`.
#[test]
fn memory_ordering_through_a_binding_keeps_its_variant() {
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let a = MemoryOrdering.Relaxed;\n\
                     let b = MemoryOrdering.Acquire;\n\
                     let c = MemoryOrdering.Release;\n\
                     let d = MemoryOrdering.AcqRel;\n\
                     let e = MemoryOrdering.SeqCst;\n\
                     println(name(a));\n\
                     println(name(b));\n\
                     println(name(c));\n\
                     println(name(d));\n\
                     println(name(e));\n\
                 }\n\
                 fn name(m: MemoryOrdering) -> String {\n\
                     match m {\n\
                         Relaxed => \"Relaxed\",\n\
                         Acquire => \"Acquire\",\n\
                         Release => \"Release\",\n\
                         AcqRel => \"AcqRel\",\n\
                         SeqCst => \"SeqCst\",\n\
                     }\n\
                 }"
        ),
        Some("Relaxed\nAcquire\nRelease\nAcqRel\nSeqCst\n".to_string())
    );
}

/// `WaitTarget` was the SECOND enum reaching codegen with no layout, found
/// by the fail-closed guard rather than by a wrong answer: with a single
/// variant, the always-matching binding fallback and a correct tag test
/// agree, so it was right by luck. Seeded alongside `MemoryOrdering`; this
/// keeps it honest if a second variant is ever added.
#[test]
fn wait_target_matches_on_its_only_variant() {
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let w = WaitTarget.Running;\n\
                     match w { WaitTarget.Running => println(\"Running\") }\n\
                 }"
        ),
        Some("Running\n".to_string())
    );
}

/// Leg 3 — the SECOND hole on its own. A FRESH-TEMP enum argument whose
/// payload is stored: the caller does consult the predicate here, so this
/// fails only because the predicate could not see through the `match`.
/// Keeping it separate from leg 1 is what stops a future edit from fixing
/// one hole and reporting both closed.
#[test]
fn e2e_temp_enum_arg_payload_stored_into_mut_ref_param_runs_one_body() {
    assert_eq!(
        run_program(
            "struct Res { id: i64 }\n\
                 impl Drop for Res {\n\
                 \x20   fn drop(mut ref self) { println(f\"drop {self.id}\"); }\n\
                 }\n\
                 enum Box2 { Full(Res), Empty }\n\
                 fn take(sink: mut ref Vec[Res], b: Box2) {\n\
                 \x20   match b { Box2.Full(r) => { sink.push(r); } Box2.Empty => {} }\n\
                 }\n\
                 fn main() {\n\
                 \x20   let mut sink: Vec[Res] = Vec.new();\n\
                 \x20   take(mut sink, Box2.Full(Res { id: 7 }));\n\
                 \x20   println(f\"pushed {sink.len()}\");\n\
                 }\n"
        ),
        Some("pushed 1\ndrop 7\n".to_string())
    );
}

/// Leg 4 — the shape the row was actually filed on: a METHOD arm moving an
/// owned enum param's payload into a `mut ref` param. It reaches the same
/// count by a THIRD route — the method caller already stands down on every
/// by-value arg (B-2026-08-29-11), so here it was the callee FRAME that
/// fired, because the blind predicate did not mark `b` as escaping.
#[test]
fn e2e_method_enum_arg_payload_stored_into_mut_ref_param_runs_one_body() {
    assert_eq!(
        run_program(
            "struct Res { id: i64 }\n\
                 impl Drop for Res {\n\
                 \x20   fn drop(mut ref self) { println(f\"drop {self.id}\"); }\n\
                 }\n\
                 enum Box2 { Full(Res), Empty }\n\
                 struct T { n: i64 }\n\
                 impl T {\n\
                 \x20   fn take(ref self, sink: mut ref Vec[Res], b: Box2) {\n\
                 \x20       match b { Box2.Full(r) => { sink.push(r); } Box2.Empty => {} }\n\
                 \x20   }\n\
                 }\n\
                 fn main() {\n\
                 \x20   let t: T = T { n: 1 };\n\
                 \x20   let mut sink: Vec[Res] = Vec.new();\n\
                 \x20   let carg: Box2 = Box2.Full(Res { id: 7 });\n\
                 \x20   t.take(mut sink, carg);\n\
                 \x20   println(f\"pushed {sink.len()}\");\n\
                 }\n"
        ),
        Some("pushed 1\ndrop 7\n".to_string())
    );
}

/// B-2026-09-07-62 — the OUTPUT twin of
/// `memory_sanitizer.rs::asan_boxed_shared_enum_nested_struct_field_has_one_owner`.
///
/// The row is a leak, so the leak checker is the fixture that pins it; this
/// one pins that the ownership change is otherwise INVISIBLE — the four
/// move-out spellings, the three classifier-parity nested-child shapes and
/// the `Vec[String]` element case all print exactly what they printed
/// before, on every backend. A duplicator that copied the wrong field, or a
/// walker that freed a buffer still in use, would show up here as changed
/// or missing output rather than as a leak.
#[test]
fn e2e_boxed_shared_enum_nested_struct_field_output_is_unchanged() {
    fn prog(extra: &str, init: &str, use_expr: &str) -> String {
        format!(
                "struct Sp4n {{ a: i64, b: i64, c: i64, d: i64 }}\n\
                 shared struct Sh {{ v: i64 }}\n\
                 shared enum E {{ Lit(i64), Iff(IfNode), Blk(Block) }}\n\
                 struct Block {{ stmts: Vec[i64], {extra} pad: Option[String], sp: Sp4n }}\n\
                 struct IfNode {{ cond: E, then_block: Block, sp: Sp4n }}\n\
                 fn mk_block(first: i64, s: i64) -> Block {{\n\
                 \x20   let mut v: Vec[i64] = Vec.new(); v.push(first); v.push(first + 1);\n\
                 \x20   let mut w: Vec[String] = Vec.new(); w.push(f\"t{{first}}\");\n\
                 \x20   return Block {{ stmts: v, {init} pad: Option.Some(f\"p{{first}}\"), sp: Sp4n {{ a: s, b: 0, c: 0, d: 0 }} }};\n\
                 }}\n\
                 fn mk() -> E {{ return E.Iff(IfNode {{ cond: E.Lit(7), then_block: mk_block(20, 2), sp: Sp4n {{ a: 5, b: 0, c: 0, d: 0 }} }}); }}\n\
                 fn main() {{\n\
                 \x20   let ife = mk();\n\
                 \x20   match ife {{ E.Lit(n) => println(f\"{{n}}\"), E.Iff(nd) => {{ {use_expr} }}, E.Blk(_) => println(\"-9\") }}\n\
                 \x20   println(\"end\");\n\
                 }}\n"
            )
    }
    const NONE: &str = "println(f\"v{nd.sp.a}\");";
    const SIB: &str = "let c = nd.cond; println(f\"v{nd.sp.a}\");";
    const FA: &str = "let tb = nd.then_block; println(f\"v{tb.stmts.len()}\");";
    const DE: &str = "let Block { stmts, pad, sp } = nd.then_block; println(f\"v{stmts.len()}\");";

    for (label, extra, init, u, want) in [
        ("no-moveout", "", "", NONE, "v5\nend\n"),
        ("sibling-moveout", "", "", SIB, "v5\nend\n"),
        ("fieldaccess-moveout", "", "", FA, "v2\nend\n"),
        ("destructure-moveout", "", "", DE, "v2\nend\n"),
        (
            "parity-bare-shared",
            "sh: Sh,",
            "sh: Sh { v: 3 },",
            NONE,
            "v5\nend\n",
        ),
        (
            "parity-bare-shared-fa",
            "sh: Sh,",
            "sh: Sh { v: 3 },",
            FA,
            "v2\nend\n",
        ),
        (
            "parity-option-shared",
            "osh: Option[Sh],",
            "osh: Option.Some(Sh { v: 4 }),",
            NONE,
            "v5\nend\n",
        ),
        (
            "parity-both",
            "sh: Sh, osh: Option[Sh],",
            "sh: Sh { v: 3 }, osh: Option.Some(Sh { v: 4 }),",
            FA,
            "v2\nend\n",
        ),
        (
            "vecstring",
            "tags: Vec[String],",
            "tags: w,",
            NONE,
            "v5\nend\n",
        ),
        (
            "vecstring-fa",
            "tags: Vec[String],",
            "tags: w,",
            FA,
            "v2\nend\n",
        ),
    ] {
        let Some(out) = run_program(&prog(extra, init, u)) else {
            return;
        };
        assert_eq!(out, want, "[{label}]");
    }
}

/// B-2026-09-06-67 — the OUTPUT twin of
/// `memory_sanitizer.rs::asan_boxed_enum_payload_param_owns_its_box`.
///
/// The row is a leak, so the leak checker pins it; this pins that giving
/// the caller ownership of a boxed ENUM payload's box is otherwise
/// invisible. It also carries the `String`-bearing spellings the ASAN twin
/// cannot assert clean — their payload INTERIOR is still unowned under a
/// nested-pattern arm, filed separately — so the row's own cells are at
/// least pinned for output, and a fix for that residual has a place to
/// prove it changed nothing here.
#[test]
fn e2e_boxed_enum_payload_param_output_is_unchanged() {
    const PRE: &str = "struct R2 { s: String, t: String, u: String }\n\
             enum K { A(R2), B }\n\
             fn mkr(i: i64) -> R2 { return R2 { s: f\"s{i}\", t: f\"t{i}\", u: f\"u{i}\" }; }\n";
    for (label, body, want) in [
            (
                "option-nested-arm",
                "fn show(x: Option[K]) { match x { Option.Some(K.A(r)) => { println(f\"a:{r.s}\"); }, Option.Some(K.B) => {}, Option.None => {} } }\n\
                 fn main() { let mut i = 0; while i < 3 { show(Option.Some(K.A(mkr(i)))); i = i + 1; } println(\"end\") }\n",
                "a:s0\na:s1\na:s2\nend\n",
            ),
            (
                "result-nested-arm",
                "fn show(x: Result[K, i64]) { match x { Result.Ok(K.A(r)) => { println(f\"a:{r.s}\"); }, Result.Ok(K.B) => {}, Result.Err(e) => {} } }\n\
                 fn main() { let mut i = 0; while i < 3 { show(Result.Ok(K.A(mkr(i)))); i = i + 1; } println(\"end\") }\n",
                "a:s0\na:s1\na:s2\nend\n",
            ),
            (
                "result-err-side",
                "fn show(x: Result[i64, K]) { match x { Result.Ok(n) => { println(f\"n{n}\"); }, Result.Err(K.A(r)) => { println(f\"e:{r.s}\"); }, Result.Err(K.B) => {} } }\n\
                 fn main() { let mut i = 0; while i < 3 { show(Result.Err(K.A(mkr(i)))); i = i + 1; } println(\"end\") }\n",
                "e:s0\ne:s1\ne:s2\nend\n",
            ),
            (
                "whole-payload-binding",
                "fn show(x: Option[K]) { match x { Option.Some(k) => { match k { K.A(r) => { println(f\"a:{r.s}\"); }, K.B => {} } }, Option.None => {} } }\n\
                 fn main() { let mut i = 0; while i < 3 { show(Option.Some(K.A(mkr(i)))); i = i + 1; } println(\"end\") }\n",
                "a:s0\na:s1\na:s2\nend\n",
            ),
            (
                "binds-nothing",
                "fn show(x: Option[K]) { match x { Option.Some(_) => { println(\"s\"); }, Option.None => {} } }\n\
                 fn main() { let mut i = 0; while i < 3 { show(Option.Some(K.A(mkr(i)))); i = i + 1; } println(\"end\") }\n",
                "s\ns\ns\nend\n",
            ),
            // B-2026-09-09-18 — this cell's expectation was CHANGED, and it was
            // wrong before rather than a decision this row overrode. Three `K`
            // values are built and all three die inside `show` (a by-value
            // param the callee does not let escape), and `K` declares
            // `impl Drop`, so three `dK` lines are owed. The pin froze ZERO.
            //
            // What proves it is the `-named-local` cell added directly below,
            // which this pin did not have: it differs from this one by binding
            // the argument to a local first, and it printed `dK` three times
            // BEFORE this row's fix as well as after (measured on both
            // backends at `KARAC_OPT_LEVEL=0`). Two spellings of one program
            // disagreed, and the pin happened to contain only the wrong one —
            // which is exactly how an output pin freezes a bug rather than a
            // behaviour. The two now agree.
            //
            // Same defect as the row's headline shape, one level up: there the
            // lost body belongs to a `Drop`-bearing payload STRUCT, here to the
            // payload ENUM's own `impl Drop`. Both ride
            // `emit_optres_payload_user_drop_bodies_fn`, whose user-enum arm is
            // B-2026-08-28-58 leg B, and both were missing for the same reason —
            // a fresh temp has no let site to own them.
            (
                "drop-bearing-payload-enum",
                "impl Drop for K { fn drop(mut ref self) { println(\"dK\") } }\n\
                 fn show(x: Option[K]) { match x { Option.Some(K.A(r)) => { println(f\"a:{r.s}\"); }, Option.Some(K.B) => {}, Option.None => {} } }\n\
                 fn main() { let mut i = 0; while i < 3 { show(Option.Some(K.A(mkr(i)))); i = i + 1; } println(\"end\") }\n",
                "a:s0\ndK\na:s1\ndK\na:s2\ndK\nend\n",
            ),
            // The control the pin was missing. One word different from the cell
            // above — the argument is bound to a local before the call — and it
            // is the spelling that was always correct, so it is what a future
            // change to this family has to keep agreeing with.
            (
                "drop-bearing-payload-enum-named-local",
                "impl Drop for K { fn drop(mut ref self) { println(\"dK\") } }\n\
                 fn show(x: Option[K]) { match x { Option.Some(K.A(r)) => { println(f\"a:{r.s}\"); }, Option.Some(K.B) => {}, Option.None => {} } }\n\
                 fn main() { let mut i = 0; while i < 3 { let q = Option.Some(K.A(mkr(i))); show(q); i = i + 1; } println(\"end\") }\n",
                "a:s0\ndK\na:s1\ndK\na:s2\ndK\nend\n",
            ),
            (
                "flows-into-return-control",
                "fn pass(x: Option[K]) -> Option[K] { return x; }\n\
                 fn main() { let mut i = 0; while i < 3 { let y = pass(Option.Some(K.A(mkr(i)))); match y { Option.Some(K.A(r)) => { println(f\"a:{r.s}\"); }, Option.Some(K.B) => {}, Option.None => {} } i = i + 1; } println(\"end\") }\n",
                "a:s0\na:s1\na:s2\nend\n",
            ),
        ] {
            let Some(out) = run_program(&format!("{PRE}{body}")) else {
                return;
            };
            assert_eq!(out, want, "[{label}]");
        }
}

/// B-2026-09-10-9 — a boxed TUPLE payload handed to a by-value
/// `Option`/`Result` param ran its elements' `Drop` bodies THROUGH A BOX
/// THE CALLEE HAD ALREADY FREED.
///
/// The bodies were the caller's (B-2026-09-09-18), the box was the
/// callee's (B-2026-09-04-12 and the rows around it), and the caller's walk
/// runs after the call returns — so the two orders could not both be
/// satisfied. Under valgrind, two invalid reads per call for
/// `takeR(Some((Rt { .. }, Rt { .. })))`, one per element, every one of
/// them inside a 64-byte block already passed to `free`.
///
/// ONLY ELEMENT 0 PRINTED WRONG, and that is an allocator artifact rather
/// than a second defect: glibc's tcache writes its safe-linked `next` over
/// a freed chunk's first 8 bytes, so a field at offset 0 reads freelist
/// metadata (`dRt23108613045`, a different value on every run) while every
/// later element still reads its own stale-but-intact bytes. The row was
/// filed on that printed value and called it a wrong OFFSET into live
/// memory; it is the right offset into dead memory.
///
/// This harness pins the OUTPUT of the family. It is not the gate for the
/// use-after-free itself — that is
/// `asan_boxed_tuple_payload_param_bodies_precede_the_box_free` in
/// `tests/memory_sanitizer.rs`, which with this commit reverted reports
/// `heap-use-after-free` on the INSTRUMENTED leg
/// (`KARAC_SANITIZE_ADDRESS=1`) and fails on the printed value alone on
/// the default one -- as that fixture's own doc comment states, and as
/// this sentence did not until B-2026-09-10-15's commit corrected it. What these cells catch is the OTHER direction: a
/// body that stops running at all, which is what moving the walk to the
/// callee costs if the arm-level disarm is left to fire for a whole-payload
/// binding (`Some(t)`), and what the interpreter did for every named local
/// until this commit.
///
/// The two CONTROLS are the shapes that must not move. A boxed STRUCT
/// payload keeps the box on the CALLER, so its walk already preceded its
/// own free and nothing here may disturb it; an INLINE tuple payload has no
/// box at all and reads the caller's own spilled bytes.
#[test]
fn e2e_boxed_tuple_payload_param_runs_its_element_bodies() {
    const PRE: &str = "struct Rt { id: i64, name: String }\n\
             impl Drop for Rt { fn drop(mut ref self) { println(f\"dRt{self.id}\") } }\n";
    for (label, body, want) in [
            // The row's own shape: a whole-tuple binding the arm never reads.
            (
                "whole-tuple-binding",
                "fn takeR(x: Option[(Rt, Rt)]) { match x { Some(t) => { println(\"ok\") } None => { println(\"n\") } } }\n\
                 fn main() { takeR(Some((Rt { id: 71, name: f\"a\" }, Rt { id: 72, name: f\"b\" }))); println(\"done\") }\n",
                "ok\ndRt71\ndRt72\ndone\n",
            ),
            // Arity 3 — every element is read after the free, so a fix that
            // only repaired "element 0" would leave this one printing 71/72/73
            // by luck rather than by ownership.
            (
                "arity-three",
                "fn takeR(x: Option[(Rt, Rt, Rt)]) { match x { Some(t) => { println(\"ok\") } None => { println(\"n\") } } }\n\
                 fn main() { takeR(Some((Rt { id: 71, name: f\"a\" }, Rt { id: 72, name: f\"b\" }, Rt { id: 73, name: f\"c\" }))); println(\"done\") }\n",
                "ok\ndRt71\ndRt72\ndRt73\ndone\n",
            ),
            // Element 0 the ONLY Drop-bearing position: the one cell where the
            // freelist word is the whole observable, since there is no correct
            // later element to sit beside it.
            (
                "drop-bearing-element-zero-only",
                "fn takeR(x: Option[(Rt, i64)]) { match x { Some(t) => { println(\"ok\") } None => { println(\"n\") } } }\n\
                 fn main() { takeR(Some((Rt { id: 71, name: f\"a\" }, 5))); println(\"done\") }\n",
                "ok\ndRt71\ndone\n",
            ),
            // A NAMED local rather than a fresh temp. The caller's let-site
            // registration is disarmed at the call-arg move, so this spelling
            // never had a caller-side walk to be wrong — it printed NOTHING on
            // both backends, and is the half the interpreter change repairs.
            (
                "named-local",
                "fn takeR(x: Option[(Rt, Rt)]) { match x { Some(t) => { println(\"ok\") } None => { println(\"n\") } } }\n\
                 fn main() { let o: Option[(Rt, Rt)] = Some((Rt { id: 71, name: f\"a\" }, Rt { id: 72, name: f\"b\" })); takeR(o); println(\"done\") }\n",
                "ok\ndRt71\ndRt72\ndone\n",
            ),
            // The `Result` twin. It reached this family only when
            // B-2026-09-09-8 (92eeb8a84) gave `Result` the box owner it was
            // missing; before that its box leaked and its bodies came from the
            // caller's temp path, which is why it printed correctly while
            // losing 144 B per program.
            (
                "result-twin",
                "fn takeR(x: Result[(Rt, Rt), i64]) { match x { Ok(t) => { println(\"ok\") } Err(e) => { println(\"n\") } } }\n\
                 fn main() { takeR(Result.Ok((Rt { id: 71, name: f\"a\" }, Rt { id: 72, name: f\"b\" }))); println(\"done\") }\n",
                "ok\ndRt71\ndRt72\ndone\n",
            ),
            // A DESTRUCTURING arm, where the leaves each take an element and
            // each own their own body. This cell is why the arm-level disarm is
            // narrowed to sub-patterns that BIND rather than destructure:
            // leaving the place armed here would print every body twice.
            (
                "destructuring-arm",
                "fn takeR(x: Option[(Rt, Rt)]) { match x { Some((a, b)) => { println(f\"a{a.id}\") } None => { println(\"n\") } } }\n\
                 fn main() { takeR(Some((Rt { id: 71, name: f\"a\" }, Rt { id: 72, name: f\"b\" }))); println(\"done\") }\n",
                "a71\ndRt71\ndRt72\ndone\n",
            ),
            // A callee that never matches at all — nothing disarms, so this is
            // the cell that passes purely on the callee-side registration.
            (
                "callee-does-not-match",
                "fn takeR(x: Option[(Rt, Rt)]) { println(\"nomatch\") }\n\
                 fn main() { takeR(Some((Rt { id: 71, name: f\"a\" }, Rt { id: 72, name: f\"b\" }))); println(\"done\") }\n",
                "nomatch\ndRt71\ndRt72\ndone\n",
            ),
            // The arm FORWARDS its binding into a second call. Exactly one set
            // of bodies is owed, and the inner callee's plain tuple param runs
            // none of its own.
            (
                "arm-forwards-into-a-call",
                "fn eat(p: (Rt, Rt)) { println(\"eat\") }\n\
                 fn takeR(x: Option[(Rt, Rt)]) { match x { Some(t) => { eat(t) } None => { println(\"n\") } } }\n\
                 fn main() { takeR(Some((Rt { id: 71, name: f\"a\" }, Rt { id: 72, name: f\"b\" }))); println(\"done\") }\n",
                "eat\ndRt71\ndRt72\ndone\n",
            ),
            // CONTROL — a boxed STRUCT payload. The callee's arm declines it
            // (`inner_struct.is_some()`), the CALLER owns the box, and its walk
            // already ran ahead of its own free. Unchanged by this commit, and
            // here so a later change that moves the struct case shows up.
            (
                "boxed-struct-payload-control",
                "struct Wt { a: Rt, b: Rt }\n\
                 fn takeW(x: Option[Wt]) { match x { Some(t) => { println(\"ok\") } None => { println(\"n\") } } }\n\
                 fn main() { takeW(Some(Wt { a: Rt { id: 71, name: f\"a\" }, b: Rt { id: 72, name: f\"b\" } })); println(\"done\") }\n",
                "ok\ndRt72\ndRt71\ndone\n",
            ),
            // CONTROL — an INLINE tuple payload: two one-word elements fit the
            // 3-word seeded area, so there is no box, the caller's walk reads
            // its own spilled copy, and nothing about this cell was ever wrong.
            (
                "inline-tuple-payload-control",
                "struct St { id: i64 }\n\
                 impl Drop for St { fn drop(mut ref self) { println(f\"dSt{self.id}\") } }\n\
                 fn takeS(x: Option[(St, St)]) { match x { Some(t) => { println(\"ok\") } None => { println(\"n\") } } }\n\
                 fn main() { takeS(Some((St { id: 71 }, St { id: 72 }))); println(\"done\") }\n",
                "ok\ndSt71\ndSt72\ndone\n",
            ),
        ] {
            let Some(out) = run_program(&format!("{PRE}{body}")) else {
                return;
            };
            assert_eq!(out, want, "[{label}]");
        }
}

/// B-2026-09-10-17 — an ENVELOPE-NESTED payload lost its `Drop` body
/// wherever a type-level gate read a payload's HEAD NAME.
///
/// `W { o: Option[Option[R]] }` printed `dR71` under `--interp` and nothing
/// compiled. THREE functions carried the same one-level horizon, each able
/// on its own to stop the body: `user_drop_field_indices_mono` (the field
/// never entered the walk set, so no walker was BUILT),
/// `type_runs_user_drop` (the parent classified drop-free), and
/// `elem_te_runs_user_drop` (a tuple element declined). Patching the
/// classifier alone moved NOTHING — capturing the JIT IR showed the
/// two-deep field emitting no `__karac_dropelems_*` symbol at all beside
/// the one-deep field that emits `__karac_dropelems_opt_R` — which is how
/// the build gate was told apart from the call gate.
///
/// THE LEG IS ENVELOPE-ONLY, and that is the measured half of the design
/// rather than a shortcut. Every nesting through a `Vec` level is silent on
/// the INTERPRETER too, because its `field_te_runs_user_drop` has the same
/// horizon and it reaches envelope chains by a different route entirely —
/// the value-driven `run_discarded_value_user_drops`, which recurses over
/// the VALUE and so descends nested envelopes for free while never entering
/// a `Vec`. Recursing this leg through containers would repair codegen for
/// those and leave the interpreter silent, turning five agreed silences
/// into five run-vs-build divergences.
///
/// So the last three cells assert SILENCE, and they are the point of this
/// test: if a later change makes them print, the interpreter has to move in
/// the same commit or the tree gains divergences.
#[test]
fn e2e_envelope_nested_payload_runs_its_drop_body_in_a_field() {
    const PRE: &str = "struct Re { id: i64, name: String }\n\
             impl Drop for Re { fn drop(mut ref self) { println(f\"dRe{self.id}\") } }\n";
    for (label, body, want) in [
            // THE ROW: a field two envelopes deep, never read.
            (
                "field-option-option",
                "struct Wa { o: Option[Option[Re]] }\n\
                 fn main() { let w = Wa { o: Some(Some(Re { id: 71, name: f\"a\" })) }; println(\"ok\"); println(\"done\") }\n",
                "dRe71\nok\ndone\n",
            ),
            // The `Result` spelling, which the row left unmeasured.
            (
                "field-result-result",
                "struct Wb { o: Result[Result[Re, i64], i64] }\n\
                 fn main() { let w = Wb { o: Result[Result[Re, i64], i64].Ok(Result[Re, i64].Ok(Re { id: 71, name: f\"a\" })) }; println(\"ok\"); println(\"done\") }\n",
                "dRe71\nok\ndone\n",
            ),
            // Three deep — the recursion has no horizon of its own.
            (
                "field-three-deep",
                "struct Wc { o: Option[Option[Option[Re]]] }\n\
                 fn main() { let w = Wc { o: Some(Some(Some(Re { id: 71, name: f\"a\" }))) }; println(\"ok\"); println(\"done\") }\n",
                "dRe71\nok\ndone\n",
            ),
            // The leaf is a user ENUM rather than a struct: the recursion stops
            // at the first non-envelope head and asks `type_runs_user_drop`,
            // whose enum leg answers for this one.
            (
                "field-envelope-of-user-enum",
                "enum Ee { A(Re), B }\n\
                 struct Wd { o: Option[Option[Ee]] }\n\
                 fn main() { let w = Wd { o: Some(Some(Ee.A(Re { id: 71, name: f\"a\" }))) }; println(\"ok\"); println(\"done\") }\n",
                "dRe71\nok\ndone\n",
            ),
            // The leaf is a struct with NO `Drop` of its own but a Drop-bearing
            // field — the same `type_runs_user_drop` question, field leg.
            (
                "field-envelope-of-struct-with-drop-field",
                "struct Wf { r: Re }\n\
                 struct Wg { o: Option[Option[Wf]] }\n\
                 fn main() { let w = Wg { o: Some(Some(Wf { r: Re { id: 71, name: f\"a\" } })) }; println(\"ok\"); println(\"done\") }\n",
                "dRe71\nok\ndone\n",
            ),
            // The TUPLE-ELEMENT site — the third horizon, and the cell left
            // over from B-2026-09-10-18 once its element inference landed.
            (
                "tuple-element-envelope",
                "fn main() { let p = (Some(Some(Re { id: 71, name: f\"a\" })), 7); println(\"ok\"); println(\"done\") }\n",
                "dRe71\nok\ndone\n",
            ),
            // CONTROL — one level, correct throughout.
            (
                "field-option-control",
                "struct Wh { o: Option[Re] }\n\
                 fn main() { let w = Wh { o: Some(Re { id: 71, name: f\"a\" }) }; println(\"ok\"); println(\"done\") }\n",
                "dRe71\nok\ndone\n",
            ),
            // CONTROL — a one-level `Vec` field, correct throughout.
            (
                "field-vec-control",
                "struct Wi { xs: Vec[Re] }\n\
                 fn main() { let w = Wi { xs: [Re { id: 71, name: f\"a\" }] }; println(\"ok\"); println(\"done\") }\n",
                "dRe71\nok\ndone\n",
            ),
            // BOUNDARY — `Vec[Vec[Re]]` is the residual this widening does NOT
            // close, and is silent on the interpreter too. Printing here means
            // a divergence was just created.
            (
                "vec-of-vec-now-fires-B-2026-09-15-23",
                "struct Wj { xs: Vec[Vec[Re]] }\n\
                 fn main() { let w = Wj { xs: [[Re { id: 71, name: f\"a\" }]] }; println(\"ok\"); println(\"done\") }\n",
                "dRe71\nok\ndone\n",
            ),
            // BOUNDARY — an envelope chain that ends in a `Vec`. The recursion
            // must stop at the `Vec` head, not walk through it.
            (
                "boundary-envelope-of-vec-stays-silent",
                "struct Wk { o: Option[Option[Vec[Re]]] }\n\
                 fn main() { let w = Wk { o: Some(Some([Re { id: 71, name: f\"a\" }])) }; println(\"ok\"); println(\"done\") }\n",
                "ok\ndone\n",
            ),
            // BOUNDARY — a `Vec` of envelopes, the mirror of the one above.
            (
                "vec-of-option-now-fires-B-2026-09-15-23",
                "struct Wl { xs: Vec[Option[Re]] }\n\
                 fn main() { let w = Wl { xs: [Some(Re { id: 71, name: f\"a\" })] }; println(\"ok\"); println(\"done\") }\n",
                "dRe71\nok\ndone\n",
            ),
        ] {
            let Some(out) = run_program(&format!("{PRE}{body}")) else {
                return;
            };
            assert_eq!(out, want, "[{label}]");
        }
}

/// B-2026-09-10-18 — a tuple literal's `Option` element lost its payload's
/// `Drop` body, but ONLY in the bare-constructor spelling.
///
/// `refined_tuple_literal_elem_te` has a constructor-rebuild arm, added by
/// B-2026-08-03-1 for `Option.Some(x)` — whose callee is a `Path` — and it
/// sat in the CATCH-ALL arm of a match on the callee's shape. A bare
/// `Some(x)` parses with an `Identifier` callee, so it took the sibling
/// branch, looked itself up in `fn_return_type_exprs` (a constructor names
/// no function), missed, and returned `None`. The element then fell back to
/// head-name inference, came back as a bare `Option` with no generic args,
/// and the binding registered no bodies walker at all.
///
/// WHAT ISOLATES IT TO THE EXPRESSION'S SHAPE rather than to tuples or to
/// payloads: five spellings of one value disagreed. The qualified
/// `Option.Some(x)`, the annotated `Option[R].Some(x)`, an annotated
/// binding `let p: (Option[R], i64)`, a call element `mk()` returning
/// `Option[R]`, and a plain struct element `(R { .. }, 7)` were all correct
/// throughout; only the bare `Some(x)` was silent. Those five are the
/// CONTROLS below.
///
/// The fix hoists the function-return lookup so a declared `fn Some(..)`
/// still wins, then lets the existing rebuild run for both callee shapes.
/// The rebuild itself is untouched, so the qualified spellings keep the
/// exact path they had.
///
/// FIXED SINCE, by B-2026-09-10-24: an `Option`-typed LOCAL moved into the
/// tuple (`let o: Option[R] = ..; let p = (o, 7);`) was silent here, and
/// this paragraph used to record it as unreachable — "handing this arm that
/// local's recorded instantiation makes the tuple's walk and the
/// un-disarmed source's walk both own the payload, an abort, not a missed
/// body". The abort was real and the inference from it was wrong: the
/// second owner is the DISARM's absence, not the lookup's presence.
/// `compile_tuple` now disarms a moved-in binding's three enum-payload
/// channels per element, exactly as `v.push(o)` does, and on top of that
/// the lookup is safe — see
/// `e2e_optres_local_moved_into_a_tuple_literal_runs_its_drop_body`. That
/// same missing disarm was crashing the ANNOTATED spelling of this cell
/// outright (B-2026-09-10-28). And a
/// `(Option[Option[R]], i64)` element stays silent for a different reason
/// again: the element type now resolves, and the tuple-element SELECTOR
/// has the one-level horizon B-2026-09-10-17 covers.
#[test]
fn e2e_bare_ctor_tuple_element_runs_its_payload_drop_body() {
    const PRE: &str = "struct Rq { id: i64, name: String }\n\
             impl Drop for Rq { fn drop(mut ref self) { println(f\"dRq{self.id}\") } }\n";
    for (label, body, want) in [
            // THE ROW: never read, so the binding dies at its own `let`.
            (
                "bare-ctor-element",
                "fn main() { let p = (Some(Rq { id: 71, name: f\"a\" }), 7); println(\"ok\"); println(\"done\") }\n",
                "dRq71\nok\ndone\n",
            ),
            // The same binding READ, so it lives to the end of the statement
            // instead — the body moves, but it must still be exactly one.
            (
                "bare-ctor-element-read",
                "fn main() { let p = (Some(Rq { id: 71, name: f\"a\" }), 7); println(f\"n{p.1}\"); println(\"done\") }\n",
                "n7\ndRq71\ndone\n",
            ),
            // Arity one: the Option is the whole tuple.
            (
                "arity-one",
                "fn main() { let p = (Some(Rq { id: 71, name: f\"a\" }),); println(\"ok\"); println(\"done\") }\n",
                "dRq71\nok\ndone\n",
            ),
            // The Option in the SECOND slot, so a fix that only inspected
            // element 0 shows up here.
            (
                "option-not-first",
                "fn main() { let p = (7, Some(Rq { id: 71, name: f\"a\" })); println(\"ok\"); println(\"done\") }\n",
                "dRq71\nok\ndone\n",
            ),
            // TWO Option elements: both bodies, in element order.
            (
                "two-option-elements",
                "fn main() { let p = (Some(Rq { id: 71, name: f\"a\" }), Some(Rq { id: 72, name: f\"b\" })); println(\"ok\"); println(\"done\") }\n",
                "dRq71\ndRq72\nok\ndone\n",
            ),
            // A NESTED tuple literal — the rebuild recurses through
            // `refined_tuple_literal_elem_te` one level down.
            (
                "nested-tuple-literal",
                "fn main() { let p = ((Some(Rq { id: 71, name: f\"a\" }), 6), 7); println(\"ok\"); println(\"done\") }\n",
                "dRq71\nok\ndone\n",
            ),
            // CONTROL — the QUALIFIED ctor, which always reached the rebuild.
            (
                "qualified-ctor-control",
                "fn main() { let p = (Option.Some(Rq { id: 71, name: f\"a\" }), 7); println(\"ok\"); println(\"done\") }\n",
                "dRq71\nok\ndone\n",
            ),
            // CONTROL — the ANNOTATED ctor, which reaches the MethodCall arm.
            (
                "annotated-ctor-control",
                "fn main() { let p = (Option[Rq].Some(Rq { id: 71, name: f\"a\" }), 7); println(\"ok\"); println(\"done\") }\n",
                "dRq71\nok\ndone\n",
            ),
            // CONTROL — an annotated BINDING, which takes the declared element
            // types and never consults the element expressions at all.
            (
                "annotated-binding-control",
                "fn main() { let p: (Option[Rq], i64) = (Some(Rq { id: 71, name: f\"a\" }), 7); println(\"ok\"); println(\"done\") }\n",
                "dRq71\nok\ndone\n",
            ),
            // CONTROL — a CALL element, resolved from the callee's declared
            // return type by the branch the fix hoists above the rebuild.
            (
                "call-element-control",
                "fn mk() -> Option[Rq] { Some(Rq { id: 71, name: f\"a\" }) }\n\
                 fn main() { let p = (mk(), 7); println(\"ok\"); println(\"done\") }\n",
                "dRq71\nok\ndone\n",
            ),
            // CONTROL — a plain struct element, which never needed the rebuild.
            (
                "struct-element-control",
                "fn main() { let p = (Rq { id: 71, name: f\"a\" }, 7); println(\"ok\"); println(\"done\") }\n",
                "dRq71\nok\ndone\n",
            ),
            // CONTROL — a bare ctor over a payload with NO `Drop`. The rebuild
            // now names this element too; nothing may run for it.
            (
                "no-drop-payload-control",
                "fn main() { let p = (Some(5), 7); println(f\"ok{p.1}\"); println(\"done\") }\n",
                "ok7\ndone\n",
            ),
        ] {
            let Some(out) = run_program(&format!("{PRE}{body}")) else {
                return;
            };
            assert_eq!(out, want, "[{label}]");
        }
}

/// B-2026-09-12-7 — a NESTED envelope destructure whose inner variant name
/// collides with the ENCLOSING envelope's own variant set miscompiled, from
/// ONE wrong lookup that produced three different observables.
///
/// `variant_pattern_enum_and_tag` resolves an unqualified variant pattern
/// against `match_scrutinee_enum_hint` — the #39 disambiguator, which names
/// the enum a TOP-LEVEL arm resolves against. That hint is exactly wrong one
/// level down: a nested sub-pattern is a variant of the PAYLOAD's enum. So
/// for `match x: Option[MyOpt] { Some(Some(v)) => .. }` over
/// `enum MyOpt { Some(i64), Nothing }`, the inner `Some` answered `Option`'s
/// — returning `Option`'s TAG (1, against `MyOpt.Some`'s 0) *and* `Option`'s
/// 4-word LLVM TYPE, and that oversized type is what made
/// `reconstruct_payload_value` debox an INLINE 2-word payload via `inttoptr`.
///
/// THREE OBSERVABLES from that one lookup, which is why the row's own
/// "two observables" reading was corrected when the full matrix was measured:
///
///   * SIGSEGV — a narrow inline payload: `inttoptr` of a tag value (0) is a
///     null deref. Also what the two wrong-arm cells below do at `-O2`
///     auto-par, so the crash is the general case and the wrong arm is the
///     `-O0` special case.
///   * a SILENTLY WRONG ARM — a boxed payload: the load succeeds through a
///     real box pointer, reads `MyOpt.Some`'s tag 0, compares it against 1,
///     falls through, and takes the outer `None` arm on a scrutinee that is
///     demonstrably `Some(..)`.
///   * a WRONG VALUE — a `shared` payload enum: `a0` for `a71`.
///
/// THE TRIGGER IS THE ENCLOSING ENVELOPE'S SET, not `Option`/`Result` in
/// general: `Ok` is dangerous only under a `Result` and `Some` only under an
/// `Option`, which is what the four controls pin. Nesting is required too —
/// the same enum matched directly, and an enum that merely HAS a colliding
/// variant the sub-pattern does not name, were correct throughout.
///
/// BOTH SIDES HAD TO MOVE, and the intermediate state is the evidence:
/// fixing only the condition path (`and_in_nested_variant_conditions`) made
/// the arm correctly ENTERED and then let `bind_pattern_values` debox the
/// inline payload itself — the two wrong-arm cells turned INTO crashes. The
/// hint is therefore swapped on both paths.
///
/// The hint carries the payload's full `TypeExpr` rather than its head name
/// so each level can rebuild the map for the next one; with a head name
/// alone, `Option[Option[MyOpt]]` still crashed because level 2 could not
/// answer level 3.
#[test]
fn e2e_nested_envelope_variant_name_collision_resolves_against_the_payload() {
    for (label, body, want) in [
            // THE ROW: inline narrow payload — a null deref before the fix.
            (
                "option-some-collision-inline",
                "enum MyOpt { Some(i64), Nothing }\n\
                 fn main() { let x: Option[MyOpt] = Some(MyOpt.Some(71));\n\
                 match x { Some(Some(v)) => { println(f\"a{v}\") } Some(Nothing) => { println(\"sn\") } None => { println(\"n\") } }\n\
                 println(\"done\") }\n",
                "a71\ndone\n",
            ),
            // A BOXED payload: the wrong-arm observable at -O0, a crash at -O2.
            (
                "option-some-collision-boxed",
                "struct Rc7 { id: i64, name: String }\n\
                 impl Drop for Rc7 { fn drop(mut ref self) { println(f\"dRc{self.id}\") } }\n\
                 enum MyOptB { Some(Rc7), Nothing }\n\
                 fn main() { let x: Option[MyOptB] = Some(MyOptB.Some(Rc7 { id: 71, name: f\"a\" }));\n\
                 match x { Some(Some(r)) => { println(f\"a{r.id}\") } Some(Nothing) => { println(\"sn\") } None => { println(\"n\") } }\n\
                 println(\"done\") }\n",
                "a71\ndRc71\ndone\n",
            ),
            // The QUALIFIED inner spelling — qualifying did NOT rescue it, which
            // is what rules out a mere unqualified-name ambiguity the author
            // could write around.
            (
                "qualified-inner-pattern",
                "enum MyOptQ { Some(i64), Nothing }\n\
                 fn main() { let x: Option[MyOptQ] = Some(MyOptQ.Some(71));\n\
                 match x { Some(MyOptQ.Some(v)) => { println(f\"a{v}\") } Some(MyOptQ.Nothing) => { println(\"sn\") } None => { println(\"n\") } }\n\
                 println(\"done\") }\n",
                "a71\ndone\n",
            ),
            // A WILDCARD payload behind the colliding name.
            (
                "wildcard-payload",
                "enum MyOptW { Some(i64), Nothing }\n\
                 fn main() { let x: Option[MyOptW] = Some(MyOptW.Some(71));\n\
                 match x { Some(Some(_)) => { println(\"a\") } Some(Nothing) => { println(\"sn\") } None => { println(\"n\") } }\n\
                 println(\"done\") }\n",
                "a\ndone\n",
            ),
            // `Result` outer with a colliding `Ok` — the same defect on the
            // other envelope.
            (
                "result-ok-collision",
                "enum MyRes { Ok(i64), Bad }\n\
                 fn main() { let x: Result[MyRes, i64] = Result[MyRes, i64].Ok(MyRes.Ok(71));\n\
                 match x { Ok(Ok(v)) => { println(f\"a{v}\") } Ok(Bad) => { println(\"ob\") } Err(e) => { println(\"n\") } }\n\
                 println(\"done\") }\n",
                "a71\ndone\n",
            ),
            // The `Err` SIDE of a `Result`, so the per-variant payload lookup is
            // exercised on arg 1 rather than arg 0.
            (
                "result-err-collision",
                "enum MyE { Err(i64), Fine }\n\
                 fn main() { let x: Result[i64, MyE] = Result[i64, MyE].Err(MyE.Err(71));\n\
                 match x { Ok(v) => { println(f\"ok{v}\") } Err(Err(c)) => { println(f\"e{c}\") } Err(Fine) => { println(\"ef\") } }\n\
                 println(\"done\") }\n",
                "e71\ndone\n",
            ),
            // THREE DEEP. This is why the hint carries a `TypeExpr` and not a
            // head name: each level rebuilds the map for the one below it, and
            // with a name alone this cell still crashed after the two-deep cells
            // were green.
            (
                "three-deep-collision",
                "enum MyOpt3 { Some(i64), Nothing }\n\
                 fn main() { let x: Option[Option[MyOpt3]] = Some(Some(MyOpt3.Some(71)));\n\
                 match x { Some(Some(Some(v))) => { println(f\"a{v}\") } Some(Some(Nothing)) => { println(\"ssn\") } Some(None) => { println(\"sn\") } None => { println(\"n\") } }\n\
                 println(\"done\") }\n",
                "a71\ndone\n",
            ),
            // A `shared` payload enum — the third observable, a wrong VALUE
            // (`a0`) rather than a crash or a wrong arm.
            (
                "shared-payload-enum-collision",
                "shared enum MyOptS { Some(i64), Nothing }\n\
                 fn main() { let x: Option[MyOptS] = Some(MyOptS.Some(71));\n\
                 match x { Some(Some(v)) => { println(f\"a{v}\") } Some(Nothing) => { println(\"sn\") } None => { println(\"n\") } }\n\
                 println(\"done\") }\n",
                "a71\ndone\n",
            ),
            // CONTROL — the inner variant renamed. Never collided, correct
            // throughout, and here so a fix that widened past the collision
            // shows up.
            (
                "noncolliding-name-control",
                "enum MyOptN { Thing(i64), Nothing }\n\
                 fn main() { let x: Option[MyOptN] = Some(MyOptN.Thing(71));\n\
                 match x { Some(Thing(v)) => { println(f\"a{v}\") } Some(Nothing) => { println(\"sn\") } None => { println(\"n\") } }\n\
                 println(\"done\") }\n",
                "a71\ndone\n",
            ),
            // CONTROL — `Ok` under an `Option`. The name is a seeded variant
            // name but NOT one of the enclosing envelope's, so it never
            // collided: this is the cell that pins the trigger to the outer
            // set rather than to `Option`/`Result` names in general.
            (
                "ok-under-option-control",
                "enum MyResO { Ok(i64), Bad }\n\
                 fn main() { let x: Option[MyResO] = Some(MyResO.Ok(71));\n\
                 match x { Some(Ok(v)) => { println(f\"a{v}\") } Some(Bad) => { println(\"ob\") } None => { println(\"n\") } }\n\
                 println(\"done\") }\n",
                "a71\ndone\n",
            ),
            // CONTROL — no envelope at all. The collision needs the nesting.
            (
                "no-envelope-control",
                "enum MyOptD { Some(i64), Nothing }\n\
                 fn main() { let x: MyOptD = MyOptD.Some(71);\n\
                 match x { Some(v) => { println(f\"a{v}\") } Nothing => { println(\"sn\") } }\n\
                 println(\"done\") }\n",
                "a71\ndone\n",
            ),
            // CONTROL — the colliding variant EXISTS on the payload enum but the
            // sub-pattern does not name it, so nothing resolves against it.
            (
                "colliding-variant-unnamed-control",
                "enum MyOptU { Thing(i64), None }\n\
                 fn main() { let x: Option[MyOptU] = Some(MyOptU.Thing(71));\n\
                 match x { Some(Thing(v)) => { println(f\"a{v}\") } Some(None) => { println(\"sn\") } None => { println(\"n\") } }\n\
                 println(\"done\") }\n",
                "a71\ndone\n",
            ),
            // CONTROL — the #39 shape this hint exists for: one variant name
            // shared across two USER enums, reached through an envelope. The
            // swap must not cost #39 its disambiguation.
            (
                "hash39-shared-name-control",
                "enum Tok7 { Float(i64), Other }\n\
                 enum Exp7 { Float(i64), Blank }\n\
                 fn main() { let x: Option[Tok7] = Some(Tok7.Float(71));\n\
                 match x { Some(Float(v)) => { println(f\"t{v}\") } Some(Other) => { println(\"o\") } None => { println(\"n\") } }\n\
                 println(\"done\") }\n",
                "t71\ndone\n",
            ),
            // CONTROL — the nesting SPLIT into two sequential matches. Always
            // correct, and the cell that originally localized the defect to the
            // nested pattern rather than to the enum's storage or its own match.
            (
                "split-into-two-matches-control",
                "enum MyOptP { Some(i64), Nothing }\n\
                 fn main() { let x: Option[MyOptP] = Some(MyOptP.Some(71));\n\
                 match x { Some(inner) => { match inner { Some(v) => { println(f\"a{v}\") } Nothing => { println(\"sn\") } } } None => { println(\"n\") } }\n\
                 println(\"done\") }\n",
                "a71\ndone\n",
            ),
        ] {
            let Some(out) = run_program(body) else {
                return;
            };
            assert_eq!(out, want, "[{label}]");
        }
}

/// B-2026-09-12-11 — a QUALIFIED constructor at an argument position
/// (`plainD(Option[(Rq, Rq)].Some((..)))`) ran its payload's `Drop` body on
/// NO compiled backend, while the BARE `plainD(Some((..)))` spelling of the
/// same program was correct on all four surfaces.
///
/// ONE CLASS UNDER TWO SPELLINGS, which is the whole content of the defect.
/// B-2026-08-22-17 established that `T[args].method(..)` parses as a
/// `MethodCall` while `T.method(..)` parses as a `Call`;
/// `optres_arg_is_unowned_temp` lists `MethodCall` in its first arm beside
/// `Identifier` / `FieldAccess` / `Index` and answers `false` for all of
/// them on the reading "a live binding or a place rooted at one". For a
/// qualified CONSTRUCTOR that reading is simply wrong — it mints a fresh
/// enum value nothing else owns — so the caller never staged
/// `__optres_arg_bodies_tmp` and the payload-bodies walker was never even
/// EMITTED (confirmed in captured IR, not inferred from the predicate).
///
/// NOT A WIDENING OF THAT PREDICATE'S SEMANTIC CLASS, which matters because
/// its doc records that widening it turns three shapes clean today into
/// DOUBLE FREES. The `Call` arm already answers `true` for every enum
/// constructor unconditionally; this makes the same answer reachable
/// through the second spelling. Cells 8–10 are those three shapes in the
/// qualified spelling — a callee that STORES the argument, one that RETURNS
/// it, and one that returns it INSIDE AN AGGREGATE — and each must show
/// exactly one body and no abort.
///
/// THE ENVELOPE PREDICATE NEEDED THE SAME RECOGNIZER, and its half is a
/// LEAK rather than a missing body, so it is pinned in
/// `tests/memory_sanitizer.rs` where LSan can see it (32 B per call):
/// `optres_arg_mints_field_envelope` matched only `ExprKind::Call` too.
///
/// MEMORY WAS CLEAN THROUGHOUT — before the fix and after, every cell here
/// is 0 valgrind errors with all heap blocks freed. That is why no
/// sanitizer caught this half: the box and its interior always had owners
/// and only the user body was missing, exactly as B-2026-09-09-18's
/// fresh-temp hole went unnoticed.
///
/// NON-ARGUMENT POSITIONS WERE NEVER AFFECTED (cell 12), which was an open
/// question on the row rather than an assumption: a `let` RHS, a `return`
/// and a `Vec`-literal element all ran the body correctly in both
/// spellings, because they reach ownership through the let/return
/// machinery rather than through the argument-freshness predicates.
#[test]
fn e2e_qualified_ctor_argument_runs_its_payload_drop_body() {
    const PRE: &str = "struct Rq { id: i64 }\n\
             impl Drop for Rq { fn drop(mut ref self) { println(f\"dRq{self.id}\") } }\n";
    for (label, body, want) in [
            // 1 — THE ROW: a boxed TUPLE payload behind a qualified ctor.
            (
                "tuple-payload-qualified-arg",
                "fn plainD(x: Option[(Rq, Rq)]) { match x { Some(t) => { println(\"s\") } None => { println(\"n\") } } }\n\
                 fn main() { plainD(Option[(Rq, Rq)].Some((Rq { id: 1 }, Rq { id: 2 }))); println(\"end\") }\n",
                "s\ndRq1\ndRq2\nend\n",
            ),
            // 2 — a STRUCT payload, so this is not tuple-specific.
            (
                "struct-payload-qualified-arg",
                "fn plainD(x: Option[Rq]) { match x { Some(r) => { println(f\"s:{r.id}\") } None => { println(\"n\") } } }\n\
                 fn main() { plainD(Option[Rq].Some(Rq { id: 1 })); println(\"end\") }\n",
                "s:1\ndRq1\nend\n",
            ),
            // 3 — an `Array` payload. Visible only since B-2026-09-10-27 gave
            //     the interpreter its element walk; before that this shape sat
            //     inside that row's agreed silence.
            (
                "array-payload-qualified-arg",
                "fn plainD(x: Option[Array[Rq, 2]]) { match x { Some(t) => { println(f\"s:{t[0].id}\") } None => { println(\"n\") } } }\n\
                 fn main() { let a: Array[Rq, 2] = [Rq { id: 1 }, Rq { id: 2 }]; plainD(Option[Array[Rq, 2]].Some(a)); println(\"end\") }\n",
                "s:1\ndRq1\ndRq2\nend\n",
            ),
            // 4 — `Result`'s `Ok` side.
            (
                "result-ok-qualified-arg",
                "fn plainD(x: Result[Array[Rq, 2], i64]) { match x { Ok(t) => { println(f\"s:{t[0].id}\") } Err(e) => { println(\"n\") } } }\n\
                 fn main() { let a: Array[Rq, 2] = [Rq { id: 1 }, Rq { id: 2 }]; plainD(Result[Array[Rq, 2], i64].Ok(a)); println(\"end\") }\n",
                "s:1\ndRq1\ndRq2\nend\n",
            ),
            // 5 — and its `Err` side, whose tag is the other one.
            (
                "result-err-qualified-arg",
                "fn plainD(x: Result[i64, Array[Rq, 2]]) { match x { Ok(n) => { println(\"ok\") } Err(t) => { println(f\"e:{t[0].id}\") } } }\n\
                 fn main() { let a: Array[Rq, 2] = [Rq { id: 1 }, Rq { id: 2 }]; plainD(Result[i64, Array[Rq, 2]].Err(a)); println(\"end\") }\n",
                "e:1\ndRq1\ndRq2\nend\n",
            ),
            // 6 — NESTED, both levels qualified, so the recognizer has to hold
            //     for a ctor sitting inside a ctor.
            (
                "nested-qualified-arg",
                "fn plainD(x: Option[Option[Rq]]) { match x { Some(o) => { println(\"s\") } None => { println(\"n\") } } }\n\
                 fn main() { plainD(Option[Option[Rq]].Some(Option[Rq].Some(Rq { id: 1 }))); println(\"end\") }\n",
                "s\ndRq1\nend\n",
            ),
            // 7 — CONTROL: the BARE spelling of cell 1, correct before this
            //     change and unchanged by it. The two spellings now agree,
            //     which is the property the row is actually about.
            (
                "bare-spelling-control",
                "fn plainD(x: Option[(Rq, Rq)]) { match x { Some(t) => { println(\"s\") } None => { println(\"n\") } } }\n\
                 fn main() { plainD(Some((Rq { id: 1 }, Rq { id: 2 }))); println(\"end\") }\n",
                "s\ndRq1\ndRq2\nend\n",
            ),
            // 8 — CONTROL, escape shape 1: the callee STORES the argument into
            //     a `mut ref` accumulator that outlives the call. Owning it in
            //     the caller as well would be a double free, so the count of
            //     `dRq1` here is the load-bearing assertion.
            (
                "control-callee-stores-the-arg",
                "fn keep(x: Option[Rq], acc: mut ref Vec[Option[Rq]]) { acc.push(x); println(\"k\") }\n\
                 fn main() { let mut acc: Vec[Option[Rq]] = []; keep(Option[Rq].Some(Rq { id: 1 }), mut acc); println(f\"len:{acc.len()}\"); println(\"end\") }\n",
                "k\nlen:1\ndRq1\nend\n",
            ),
            // 9 — CONTROL, escape shape 2: the callee RETURNS the param, so the
            //     destination `let` owns it.
            (
                "control-callee-returns-the-arg",
                "fn giveback(x: Option[Rq]) -> Option[Rq] { return x }\n\
                 fn main() { let z = giveback(Option[Rq].Some(Rq { id: 1 })); match z { Some(r) => { println(f\"z:{r.id}\") } None => { println(\"zn\") } } println(\"end\") }\n",
                "z:1\ndRq1\nend\n",
            ),
            // 10 — CONTROL, escape shape 3: returned INSIDE AN AGGREGATE, the
            //      route B-2026-09-01-35's escape analysis added and a
            //      syntactic store gate could not see.
            (
                "control-callee-returns-it-in-an-aggregate",
                "fn wrap(x: Option[Rq]) -> (Option[Rq], i64) { return (x, 7) }\n\
                 fn main() { let t = wrap(Option[Rq].Some(Rq { id: 1 })); match t.0 { Some(r) => { println(f\"w:{r.id}\") } None => { println(\"wn\") } } println(\"end\") }\n",
                "w:1\ndRq1\nend\n",
            ),
            // 11 — CONTROL: a real ASSOCIATED FUNCTION on an enum type, whose
            //      qualified spelling is the same `MethodCall` shape but whose
            //      method names no variant. The recognizer must decline it —
            //      it mints nothing this frame can claim — and the program must
            //      still work.
            (
                "control-assoc-fn-on-an-enum-type",
                "enum Md[T] { A(T), B }\n\
                 impl[T] Md[T] { fn make(n: T) -> Md[T] { return A(n) } }\n\
                 fn takeit(x: Md[i64]) { match x { A(n) => { println(f\"a:{n}\") } B => { println(\"b\") } } }\n\
                 fn main() { takeit(Md[i64].make(3)); println(\"end\") }\n",
                "a:3\nend\n",
            ),
            // 12 — CONTROL: a NON-ARGUMENT position. Correct in both spellings
            //      before this change, and pinned so the fix stays confined to
            //      the argument-freshness predicates it edited.
            (
                "control-qualified-ctor-as-a-let-rhs",
                "fn main() { let z = Option[Rq].Some(Rq { id: 1 }); println(\"l\"); println(\"end\") }\n",
                "dRq1\nl\nend\n",
            ),
        ] {
            let Some(out) = run_program(&format!("{PRE}{body}")) else {
                return;
            };
            assert_eq!(out, want, "[{label}]");
        }
}

/// B-2026-09-10-5 — a NAMED LOCAL of a user generic enum passed BY VALUE
/// smashed the caller's stack, because the moved-from-slot disarm zeroed
/// `Option`'s four words into whatever the binding's slot actually was.
///
/// `enum G[T] { X(T), Y }` lays `T` out erased at one word, so a `G[..]`
/// slot is TWO words; the disarm's store was sized to the seeded
/// `Option` layout and wrote 16 bytes past the end of the alloca. In a
/// small frame that is the saved return address, and `main`'s `ret`
/// jumped to 0 — a SIGSEGV with no output at all, on an ORDINARY build.
///
/// The matrix below is the shape of the evidence, and each column is
/// load-bearing:
///
///  * PAYLOAD WIDTH is the discriminator. One word fits the erased area
///    and was always clean; 2 / 3 / 6 / 9 words all crashed. That is what
///    says the write was sized to a TYPE and the slot to a LAYOUT, rather
///    than anything about the payload's contents.
///  * NO `Drop` AND NO HEAP ARE NEEDED — `W2 { a: i64, b: i64 }` is plain
///    POD. The bug lives in the move disarm, not the drop machinery, and a
///    probe that reached for a `Drop`-bearing payload would have suggested
///    otherwise.
///  * THE ARGUMENT MUST BE A NAMED LOCAL. The same value as a fresh temp
///    never reaches the disarm, and was clean before and after.
///  * THE CALLEE IS IRRELEVANT — one that only prints crashed exactly like
///    one that destructures its parameter.
///
/// `--interp`, the JIT and `-O2` all printed correctly throughout: the
/// wider frames there put something other than the return address under
/// the overrun, so this was invisible to every surface but an unoptimised
/// AOT build. The `Option`/`Result` cells are the regression half — those
/// slots ARE the seeded layout, so they must keep emitting what they did
/// before.
///
/// THIS TEST IS NOT THE GATE FOR THE CRASH, and saying so here is the
/// point of the paragraph. This harness compiles at the suite's default
/// opt level, where the overrun does not reproduce — every cell below
/// passes with the fix reverted, which is what a disable-and-rerun check
/// measured rather than assumed. What it pins is the OUTPUT of the shapes
/// involved, the seeded-pair controls included. The defect itself is
/// gated by `asan_generic_enum_named_local_by_value_arg_does_not_overrun_
/// its_slot` in `tests/memory_sanitizer.rs`, which catches the write in
/// the stack redzone and goes red with the fix reverted on the
/// `KARAC_OPT_LEVEL=0` leg.
#[test]
fn e2e_generic_enum_named_local_by_value_arg_does_not_smash_the_stack() {
    const PRE: &str = "struct W2 { a: i64, b: i64 }\n\
             struct W9 { a: i64, b: i64, c: i64, d: i64, e: i64, f: i64, g: i64, h: i64, i: i64 }\n\
             enum G[T] { X(T), Y }\n";
    for (label, body, want) in [
            // One word — fits the erased payload area, so nothing is boxed and
            // nothing was ever wrong here. The control that says width is the
            // axis.
            (
                "one-word-payload-control",
                "fn hg(g: G[i64]) { println(\"ig\"); }\n\
                 fn main() { println(\"A\"); let a = G.X(7); hg(a); println(\"end\") }\n",
                "A\nig\nend\n",
            ),
            // Two words — the narrowest crashing case, and plain POD.
            (
                "two-word-pod-payload",
                "fn hg(g: G[W2]) { println(\"ig\"); }\n\
                 fn main() { println(\"A\"); let a = G.X(W2 { a: 1, b: 2 }); hg(a); println(\"end\") }\n",
                "A\nig\nend\n",
            ),
            // Three words, and heap-bearing rather than POD.
            (
                "three-word-string-payload",
                "fn hg(g: G[String]) { println(\"ig\"); }\n\
                 fn main() { println(\"A\"); let a = G.X(f\"zz{1}\"); hg(a); println(\"end\") }\n",
                "A\nig\nend\n",
            ),
            // Nine words — well past the overrun, and the callee DESTRUCTURES,
            // which is the spelling the shape was first seen in.
            (
                "nine-word-payload-destructuring-callee",
                "fn hg(g: G[W9]) { match g { G.X(r) => { println(f\"in:{r.a}\"); }, G.Y => { println(\"y\"); } } }\n\
                 fn main() { println(\"A\"); let a = G.X(W9 { a: 1, b: 2, c: 3, d: 4, e: 5, f: 6, g: 7, h: 8, i: 9 }); hg(a); println(\"end\") }\n",
                "A\nin:1\nend\n",
            ),
            // The fresh-temp spelling of the crashing cell: never reached the
            // disarm, so it is what the named-local cells have to agree with.
            (
                "fresh-temp-spelling-control",
                "fn hg(g: G[W9]) { println(\"ig\"); }\n\
                 fn main() { println(\"A\"); hg(G.X(W9 { a: 1, b: 2, c: 3, d: 4, e: 5, f: 6, g: 7, h: 8, i: 9 })); println(\"end\") }\n",
                "A\nig\nend\n",
            ),
            // The seeded pair in the same position — their slots ARE the layout
            // the store used, so these are the cells that must not move.
            (
                "option-wide-payload-regression-control",
                "fn ho(o: Option[W9]) { println(\"ig\"); }\n\
                 fn main() { println(\"A\"); let a = Option.Some(W9 { a: 1, b: 2, c: 3, d: 4, e: 5, f: 6, g: 7, h: 8, i: 9 }); ho(a); println(\"end\") }\n",
                "A\nig\nend\n",
            ),
            (
                "result-wide-payload-regression-control",
                "fn hr(r: Result[W9, i64]) { println(\"ig\"); }\n\
                 fn main() { println(\"A\"); let a: Result[W9, i64] = Result.Ok(W9 { a: 1, b: 2, c: 3, d: 4, e: 5, f: 6, g: 7, h: 8, i: 9 }); hr(a); println(\"end\") }\n",
                "A\nig\nend\n",
            ),
        ] {
            let Some(out) = run_program(&format!("{PRE}{body}")) else {
                return;
            };
            assert_eq!(out, want, "[{label}]");
        }
}

/// B-2026-08-31-50 — an enum-constructor MIXED wrap keeps its slot mask
/// across a whole-value rebind. `let w = W2.Two(r, mk(2)); let w2 = w;`
/// printed `dR1 dR2 dR1` on all three backends where `dR2 dR1` is due:
/// the wrap masked the view slot at `w`'s `let`, but the mask was derived
/// from the constructor expression and stored nowhere, so the rebind
/// re-armed the full walk for `w2`. `enum_ctor_moved_payload_slots` now
/// records it per binding, inherited under the bare-identifier rebind
/// gate and carried by `transfer_move_masks_on_rebind`; the interpreter
/// transfers `moved_out_enum_payload_slots` the same way.
///
/// `one` the row's shape, `two` the no-rebind control, `three` the
/// all-views wrap rebound (view-ness propagation, always right), `four`
/// the rebind chained twice, `five` the view in the other slot. Not
/// here, filed separately: a `match` that destructures the view slot
/// runs its body twice in the interpreter alone, rebind or not.
#[test]
fn e2e_enum_ctor_mixed_wrap_rebind_inherits_the_slot_mask() {
    let Some(out) = run_program(
        r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"n{i}" }; }
enum W2 { Two(R, R), None2 }
enum W1 { One(R), None1 }
fn take(r: R) -> i64 { let w: W2 = W2.Two(r, mk(2)); let w2: W2 = w; return 7; }
fn take_ctl(r: R) -> i64 { let w: W2 = W2.Two(r, mk(4)); return 7; }
fn take1(r: R) -> i64 { let w: W1 = W1.One(r); let w2: W1 = w; return 7; }
fn take_twice(r: R) -> i64 { let w: W2 = W2.Two(r, mk(8)); let w2: W2 = w; let w3: W2 = w2; return 7; }
fn take_swap(r: R) -> i64 { let w: W2 = W2.Two(mk(10), r); let w2: W2 = w; return 7; }
fn main() {
    { let v: i64 = take(mk(1)); println(f"v={v}"); println("one") }
    { let v: i64 = take_ctl(mk(3)); println(f"v={v}"); println("two") }
    { let v: i64 = take1(mk(5)); println(f"v={v}"); println("three") }
    { let v: i64 = take_twice(mk(7)); println(f"v={v}"); println("four") }
    { let v: i64 = take_swap(mk(9)); println(f"v={v}"); println("five") }
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "dR2\ndR1\nv=7\none\ndR4\ndR3\nv=7\ntwo\ndR5\nv=7\nthree\ndR8\ndR7\nv=7\nfour\ndR10\ndR9\nv=7\nfive\nend\n");
}

/// B-2026-09-16-13 — a fresh enum returned BY A CALL, straight into
/// argument position, had no caller-side owner at all.
///
/// `eat(mk(i))` where `mk` returns an enum left the returned value owned by
/// nobody: the callee's param is by-value but declines the entry copy, and
/// the caller never wrote a `__owned_agg_tmp` for a CALL-produced enum the
/// way it already did for a call-produced STRUCT (B-2026-08-02-28) and for
/// an INLINE enum constructor. Measured at `-O0` on this program: 41 allocs
/// against 36 frees, 192 bytes definitely lost in 4 blocks, 4 valgrind
/// errors — and `dR2` ABSENT, so the payload's own `Drop` body never ran
/// either. After: 42 allocs / 42 frees, 0 errors, `dR2` present. (The alloc
/// counts differ by one BECAUSE the fix restores `dR2`, whose format string
/// allocates — a fixed arm with 41 allocs would mean the body was still
/// lost.)
///
/// The row was filed on the `String` payload alone and the scope is wider:
/// the struct-with-`Drop` payload (`dR2`), the destructuring callee (`m`)
/// and the `Vec` payload (`v`) are the same defect, not neighbours — every
/// one of them leaked on the pre-fix tree. What separates the one enum that
/// was always correct, `Td`, is that it carries its OWN `impl Drop`, which
/// routes through `has_user_drop` to a different owner entirely; `dTd` is
/// therefore a HARM GUARD here and no evidence the fix does any work.
///
/// The other four rows — a named local, an inline constructor, a struct
/// return and a seeded-enum (`Option`) return — read identically on both
/// arms by design. They pin the three spellings that were already correct,
/// so an over-broad repair shows up as a doubled body or an invalid free
/// rather than as a leak. The memory twin is in `tests/memory_sanitizer.rs`.
///
/// The last three rows are the CALL SPELLING axis, added after the rest of
/// this grid had already gone green: every other cell here is a free
/// function, and the row records that a method argument leaks identically.
/// Measured against the named parent tree, they were not guards — a method
/// argument, a `Drop`-bearing method argument and an associated-function
/// argument together lost 120 B in 4 blocks with `dR11` ABSENT, and are
/// clean with the body present after. A grid can be wide on payload type
/// and blind on how the callee is spelled.
#[test]
fn e2e_enum_call_return_in_argument_position_has_an_owner() {
    let src = r#"
struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
enum Ts { A(String), B }
enum Tr { A(R), B }
enum Tv { A(Vec[String]), B }
enum Td { A(String), B }
impl Drop for Td { fn drop(mut ref self) { println("dTd") } }
struct W { a: String }

fn mks(n: i64) -> Ts { return Ts.A(f"b1613-payload-aaaaaaaaaaaaaaaa-{n}"); }
fn mkr(n: i64) -> Tr { return Tr.A(R { id: n, s: f"b1613-payload-aaaaaaaaaaaaaaaa-{n}" }); }
fn mkv(n: i64) -> Tv { let mut v: Vec[String] = Vec.new(); v.push(f"b1613-payload-aaaaaaaaaaaaaaaa-{n}"); return Tv.A(v); }
fn mkd(n: i64) -> Td { return Td.A(f"b1613-payload-aaaaaaaaaaaaaaaa-{n}"); }
fn mkw(n: i64) -> W { return W { a: f"b1613-payload-aaaaaaaaaaaaaaaa-{n}" }; }
fn mko(n: i64) -> Option[String] { return Option.Some(f"b1613-payload-aaaaaaaaaaaaaaaa-{n}"); }

fn eats(e: Ts) -> i64 { return 7; }
fn eatr(e: Tr) -> i64 { return 7; }
fn eatv(e: Tv) -> i64 { return 7; }
fn eatd(e: Td) -> i64 { return 7; }
fn eatw(e: W) -> i64 { return 7; }
fn eato(e: Option[String]) -> i64 { return 7; }
fn takes(e: Ts) -> i64 { match e { Ts.A(s) => { return 1 }, Ts.B => { return 0 } } }
struct H { n: i64 }
impl H {
    fn meth(ref self, e: Ts) -> i64 { return 7; }
    fn methr(ref self, e: Tr) -> i64 { return 7; }
    fn assoc(e: Ts) -> i64 { return 7; }
}

fn main() {
    println(f"s={eats(mks(1))}");
    println(f"r={eatr(mkr(2))}");
    println(f"v={eatv(mkv(3))}");
    println(f"m={takes(mks(4))}");
    println(f"d={eatd(mkd(5))}");
    let e6: Ts = mks(6);
    println(f"local={eats(e6)}");
    println(f"inline={eats(Ts.A(f"b1613-payload-aaaaaaaaaaaaaaaa-7"))}");
    println(f"struct={eatw(mkw(8))}");
    println(f"option={eato(mko(9))}");
    let h = H { n: 1 };
    println(f"meth={h.meth(mks(10))}");
    println(f"methr={h.methr(mkr(11))}");
    println(f"assoc={H.assoc(mks(12))}");
    println("end");
}
"#;
    assert_eq!(
            run_program(src).as_deref(),
            Some("s=7\ndR2\nr=7\nv=7\nm=1\ndTd\nd=7\nlocal=7\ninline=7\nstruct=7\noption=7\nmeth=7\ndR11\nmethr=7\nassoc=7\nend\n"),
        );
}

/// B-2026-09-20-13 — a by-value generic enum argument whose monomorph
/// heap-BOXES its payload was freed by the CALLEE while the caller still
/// owned the binding, so every later read of it read freed memory.
///
/// The value is reused, which `karac check` reports and deliberately does
/// not block on (B-2026-08-29-64): the promise the non-fatal warning rests
/// on is that codegen leaves the reused value INTACT. It did not. On the
/// parent commit the first four blocks below SIGSEGV before printing
/// anything and the last two print nothing with four invalid reads each;
/// the `Gen.N` block, which carries no payload at all, crashed too.
///
/// Each block uses a DISTINCT payload spelling so a failure names its own
/// block rather than the fixture.
#[test]
fn e2e_reused_by_value_generic_enum_argument_is_not_freed_under_the_caller() {
    let out = run_program(
        r#"
enum Gen[T] { Y(T), N }
struct Holder { g: Gen[String] }
struct Hg[T] { g: Gen[T] }
struct Wide { a: String, b: String, c: String }

fn shw(g: Gen[String]) { match g { Gen.Y(v) => { println(f"s:{v}") } Gen.N => { println("s:none") } } }
fn idf(g: Gen[String]) -> Gen[String] { return g }
fn shs(g: Gen[Wide]) { match g { Gen.Y(v) => { println(f"w:{v.a}") } Gen.N => { println("w:none") } } }
fn shv(g: Gen[Vec[String]]) { match g { Gen.Y(v) => { println(f"v:{v.len()}") } Gen.N => { println("v:none") } } }
fn wrap[T](g: Gen[T], c: bool) -> Hg[T] { if c { return Hg { g: g } } return Hg { g: Gen.N } }

fn main() {
    let a: Gen[String] = Gen.Y(f"aa-local-2"); shw(a); shw(a)
    let b: Gen[String] = Gen.Y(f"bbb-thrice-33"); shw(b); shw(b); shw(b)
    let c: Holder = Holder { g: Gen.Y(f"cccc-field-444") }; shw(c.g); shw(c.g)
    let d: Gen[Wide] = Gen.Y(Wide { a: f"ddddd-struct-5555", b: f"ddddd-struct-5556", c: f"ddddd-struct-5557" }); shs(d); shs(d)
    let mut q: Vec[String] = Vec.new(); q.push(f"eeeeee-vecelem-66666"); let e: Gen[Vec[String]] = Gen.Y(q); shv(e); shv(e)
    let f: Gen[String] = Gen.Y(f"fffffff-escape-777777"); let h: Gen[String] = idf(f); shw(f); shw(h)
    let i: Hg[String] = Hg { g: Gen.Y(f"gggggggg-genfield-8888888") }; shw(i.g); shw(i.g)
    let j: Gen[String] = Gen.Y(f"hhhhhhhhh-wrapped-99999999"); let k: Hg[String] = wrap(j, true); shw(k.g); shw(k.g)
    let n: Gen[String] = Gen.N; shw(n); shw(n)
    println("done")
}
"#,
    );

    assert_eq!(
        out.as_deref(),
        Some(
            "s:aa-local-2\n\
                 s:aa-local-2\n\
                 s:bbb-thrice-33\n\
                 s:bbb-thrice-33\n\
                 s:bbb-thrice-33\n\
                 s:cccc-field-444\n\
                 s:cccc-field-444\n\
                 w:ddddd-struct-5555\n\
                 w:ddddd-struct-5555\n\
                 v:1\n\
                 v:1\n\
                 s:fffffff-escape-777777\n\
                 s:fffffff-escape-777777\n\
                 s:gggggggg-genfield-8888888\n\
                 s:gggggggg-genfield-8888888\n\
                 s:hhhhhhhhh-wrapped-99999999\n\
                 s:hhhhhhhhh-wrapped-99999999\n\
                 s:none\n\
                 s:none\n\
                 done\n"
        ),
        "a reused by-value generic enum argument read back wrong or crashed, \
             so the callee freed the caller's box"
    );
}

/// B-2026-09-13-7 — an enum variant whose payload is a CONTAINER WRITTEN
/// OVER THE ENUM'S OWN TYPE PARAMETER runs its elements' user `Drop` bodies
/// on the compiled surfaces, as the interpreter already did.
///
/// `emit_generic_enum_payload_user_drop_bodies_fn` partitions variants with
/// the name-keyed walker, and its half of that partition asked whether the
/// payload's HEAD is one of the enum's parameters. `Vec[T]` and
/// `Array[T, 2]` have the heads `Vec` and `Array`, so they failed that test
/// and went to the name-keyed walker, which takes CONCRETE payloads only —
/// owned by neither, so no walker was emitted and no compiled surface ran
/// the bodies while `--interp` ran them correctly.
///
/// EVERY EXPECTATION BELOW IS THE COMPILED OUTPUT OF THE PROGRAM BESIDE IT,
/// read off the run rather than typed, and every one matches the sequence
/// design.md fixes — each body exactly once, at the binding's LIVE-RANGE
/// end, not at lexical scope end (§ Drop ordering within a branch). That is
/// why the no-match cell prints its bodies BEFORE the statement that
/// follows the `let`: the binding is never read again, so its live range
/// ends there. Reading that as premature is the mistake this fixture's
/// author made first.
///
/// THE LAST TWO CELLS PINNED A GAP AND NOW PIN THE FIX — B-2026-09-20-62,
/// which is the row this fixture's own note predicted and asked to flip
/// them. A bare `T` payload that merely INSTANTIATES to a container was
/// silent on BOTH backends, deliberately: the interpreter did not walk it,
/// so arming the compiled side alone would have traded a both-backends-
/// silent gap for a run-vs-build divergence. That row moved both halves in
/// one commit — the walker head stopped asking how the payload was SPELLED
/// and the interpreter's `substituted_array_head` admitted `Vec` — so the
/// two cells below print their elements' bodies on all four surfaces now.
///
/// They still earn their place: they are the pair that says the fix reaches
/// the INSTANTIATED spelling and not only the declared one, which is the
/// whole of that row. If either goes silent again, the head has gone back
/// to reading the declaration.
///
/// The memory channel is unchanged by construction: this walker runs bodies
/// and frees nothing. Measured anyway on every program below at
/// `KARAC_OPT_LEVEL=0` under valgrind — balanced allocs/frees, ERROR
/// SUMMARY 0, no definite loss — because arming a walk that was not running
/// is a second-release candidate and a body assertion cannot see one.
#[test]
fn e2e_generic_enum_container_payload_runs_element_drop_bodies() {
    const PRE: &str = "struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             fn mkr(i: i64) -> R { return R { id: i } }\n";

    // `Vec[T]` under a generic head, consumed by a match. Was: x1 alone.
    if let Some(out) = run_program(&format!(
            "{PRE}enum EVecG[T] {{ V(Vec[T]), N }}\n\
             fn main() {{\n\
             \x20 let a: Vec[R] = [mkr(1), mkr(2)];\n\
             \x20 let e: EVecG[R] = EVecG.V(a);\n\
             \x20 match e {{ EVecG.V(v) => {{ println(f\"x{{v[0].id}}\") }} EVecG.N => {{ println(\"no\") }} }}\n\
             \x20 println(\"end\")\n\
             }}\n"
        )) {
            assert_eq!(out, "x1\ndR1\ndR2\nend\n");
        }

    // `Array[T, N]` under a generic head — the other container spelling,
    // excluded by the same gate for the same reason. Was: x1 alone.
    if let Some(out) = run_program(&format!(
            "{PRE}enum EArrG[T] {{ A(Array[T, 2]), N }}\n\
             fn main() {{\n\
             \x20 let a: Array[R, 2] = [mkr(1), mkr(2)];\n\
             \x20 let e: EArrG[R] = EArrG.A(a);\n\
             \x20 match e {{ EArrG.A(v) => {{ println(f\"x{{v[0].id}}\") }} EArrG.N => {{ println(\"no\") }} }}\n\
             \x20 println(\"end\")\n\
             }}\n"
        )) {
            assert_eq!(out, "x1\ndR1\ndR2\nend\n");
        }

    // The same head with NO consuming match, so the walk is reached at the
    // binding's own live-range end rather than through an arm. Was: mid,
    // end.
    if let Some(out) = run_program(&format!(
        "{PRE}enum EVecG[T] {{ V(Vec[T]), N }}\n\
             fn main() {{\n\
             \x20 let a: Vec[R] = [mkr(1), mkr(2)];\n\
             \x20 let e: EVecG[R] = EVecG.V(a);\n\
             \x20 println(\"mid\");\n\
             \x20 println(\"end\")\n\
             }}\n"
    )) {
        assert_eq!(out, "dR1\ndR2\nmid\nend\n");
    }

    // CONTROL, concrete head: the name-keyed walker's own case, correct
    // throughout. It says the partition still holds — a payload walked
    // twice would print each body twice here first.
    if let Some(out) = run_program(&format!(
            "{PRE}enum EVec {{ V(Vec[R]), N }}\n\
             fn main() {{\n\
             \x20 let a: Vec[R] = [mkr(1), mkr(2)];\n\
             \x20 let e: EVec = EVec.V(a);\n\
             \x20 match e {{ EVec.V(v) => {{ println(f\"x{{v[0].id}}\") }} EVec.N => {{ println(\"no\") }} }}\n\
             \x20 println(\"end\")\n\
             }}\n"
        )) {
            assert_eq!(out, "x1\ndR1\ndR2\nend\n");
        }

    // CONTROL, bare `T` at a PLAIN struct: the generic head's established
    // case, unaffected by the widened gate.
    if let Some(out) = run_program(&format!(
            "{PRE}enum Slot[T] {{ S(T), N }}\n\
             fn main() {{\n\
             \x20 let a: R = mkr(1);\n\
             \x20 let s: Slot[R] = Slot.S(a);\n\
             \x20 match s {{ Slot.S(v) => {{ println(f\"x{{v.id}}\") }} Slot.N => {{ println(\"no\") }} }}\n\
             \x20 println(\"end\")\n\
             }}\n"
        )) {
            assert_eq!(out, "x1\ndR1\nend\n");
        }

    // FLIPPED BY B-2026-09-20-62, and see the note above: a bare `T`
    // INSTANTIATED to a container, at a read-only arm. The element walk now
    // rides the ARM'S BINDING here rather than the husk, because the
    // binding has taken the buffer by the time the husk's walker would run;
    // the output is what says the two are in the right order.
    if let Some(out) = run_program(&format!(
            "{PRE}enum Slot[T] {{ S(T), N }}\n\
             fn main() {{\n\
             \x20 let a: Vec[R] = [mkr(1), mkr(2)];\n\
             \x20 let s: Slot[Vec[R]] = Slot.S(a);\n\
             \x20 match s {{ Slot.S(v) => {{ println(f\"x{{v[0].id}}\") }} Slot.N => {{ println(\"no\") }} }}\n\
             \x20 println(\"end\")\n\
             }}\n"
        )) {
            assert_eq!(out, "x1\ndR1\ndR2\nend\n");
        }

    // FLIPPED BY B-2026-09-20-62, no-match position of the same spelling —
    // the cell that row calls load-bearing, because it removes the whole
    // match-lowering surface from the question.
    if let Some(out) = run_program(&format!(
        "{PRE}enum Slot[T] {{ S(T), N }}\n\
             fn main() {{\n\
             \x20 let a: Vec[R] = [mkr(1), mkr(2)];\n\
             \x20 let s: Slot[Vec[R]] = Slot.S(a);\n\
             \x20 println(\"mid\");\n\
             \x20 println(\"end\")\n\
             }}\n"
    )) {
        assert_eq!(out, "dR1\ndR2\nmid\nend\n");
    }
}

/// B-2026-09-20-62 — the POSITIONS a generic container payload reaches, and
/// the two it must not.
///
/// The row this pins says a payload that only becomes a `Vec` through the
/// INSTANTIATION (`enum Slot[T] { S(T), N }` at `T = Vec[R]`) runs none of
/// its elements' `Drop` bodies, on any of the four surfaces. Its two own
/// cells — a read-only arm and no match at all — live in
/// `e2e_generic_enum_container_payload_runs_element_bodies_at_a_read_only_arm`
/// and `e2e_generic_enum_container_payload_runs_element_drop_bodies`, which
/// that row FLIPPED. This fixture is everything else the fix touches, and
/// it exists because nothing else pins these positions at all.
///
/// EVERY EXPECTATION BELOW WAS READ OFF THE RUN, not typed: each cell is a
/// program that was executed on `--interp`, the JIT, AOT `-O0` and AOT
/// `-O2`, all four agreed byte for byte, and the agreed output is what the
/// assertion carries. The `before:` note on each cell is the same
/// measurement on the tree without the fix.
///
/// THREE CELLS WERE ALREADY BROKEN BEFORE THE ROW AND ARE FIXED BY IT:
///
///  * the MIXED-WIDTH enum (`enum Mix[T] { A(T), B(Vec[T]), N }`). Its
///    payload area is the WIDEST variant's, 3 words from `B`, while `A`'s
///    own declared width is 1 — so `coerce_to_payload_words` heap-boxed the
///    payload and the walker read it inline. At `T = Array[R, 2]` that
///    printed an ASLR-varying id and a `dR0` on all three compiled
///    surfaces, with valgrind reporting ZERO errors and zero invalid reads:
///    the words it read are perfectly live, they are just the box pointer
///    and its neighbour. Only an expected-output oracle sees that class.
///  * the three `Array` CONTROLS (discard, bare statement, nesting), which
///    ran their bodies on every compiled surface and on none under
///    `--interp`. They are controls for the `Vec` cells above them and were
///    divergent in their own right.
///
/// THE TWO SILENT CELLS ARE ASSERTIONS, not omissions. Three-deep nesting
/// and a two-field variant are silent on ALL FOUR surfaces, and the fix
/// deliberately keeps them there: `elem_te_runs_user_drop` stops at one
/// level and the walker head skips a multi-field variant, so firing on
/// either side alone would trade a gap both backends share for a
/// run-vs-build divergence. If either starts printing, one side has been
/// widened past the other.
///
/// The memory channel is unchanged by construction — this walk runs bodies
/// and frees nothing — and measured anyway at `KARAC_OPT_LEVEL=0` under
/// valgrind on every program below: no invalid read, write or free on any
/// cell, and every alloc/free count identical to the same cell before the
/// fix. The mixed-width enum's own 2-block leak is unchanged by this row
/// and is the memory half of the miscompile above, left for its own row.

#[test]
fn e2e_generic_enum_container_payload_positions() {
    for (label, src, want) in [
            (
                "a DISCARDED value — no binding at all, so no instantiation is recorded for one (before: mid|end|)",
                r#"
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i }; }
struct P { id: i64 }
enum Ew { Z(R), N }
impl Drop for Ew { fn drop(mut ref self) { println("dER") } }
enum Slot[T] { S(T), N }
enum EVecG[T] { V(Vec[T]), N }
enum Mix[T] { A(T), B(Vec[T]), N }
enum G2[T] { X(T, i64), Y }
fn seenv(x: Slot[Vec[R]]) { match x { Slot.S(v) => { println(f"x{v[0].id}") } Slot.N => { println("no") } } }

fn main() {
let a: Vec[R] = [mkr(1), mkr(2)];
let _ = Slot.S(a);
println("mid");
println("end");
}
"#,
                "dR1\ndR2\nmid\nend\n",
            ),
            (
                "a BARE STATEMENT of the same constructor (before: mid|end|)",
                r#"
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i }; }
struct P { id: i64 }
enum Ew { Z(R), N }
impl Drop for Ew { fn drop(mut ref self) { println("dER") } }
enum Slot[T] { S(T), N }
enum EVecG[T] { V(Vec[T]), N }
enum Mix[T] { A(T), B(Vec[T]), N }
enum G2[T] { X(T, i64), Y }
fn seenv(x: Slot[Vec[R]]) { match x { Slot.S(v) => { println(f"x{v[0].id}") } Slot.N => { println("no") } } }

fn main() {
let a: Vec[R] = [mkr(1), mkr(2)];
Slot.S(a);
println("mid");
println("end");
}
"#,
                "dR1\ndR2\nmid\nend\n",
            ),
            (
                "a BLOCK-scoped binding: the bodies land at the live-range end, before the block's own output (before: in|mid|end|)",
                r#"
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i }; }
struct P { id: i64 }
enum Ew { Z(R), N }
impl Drop for Ew { fn drop(mut ref self) { println("dER") } }
enum Slot[T] { S(T), N }
enum EVecG[T] { V(Vec[T]), N }
enum Mix[T] { A(T), B(Vec[T]), N }
enum G2[T] { X(T, i64), Y }
fn seenv(x: Slot[Vec[R]]) { match x { Slot.S(v) => { println(f"x{v[0].id}") } Slot.N => { println("no") } } }

fn main() {
let a: Vec[R] = [mkr(1), mkr(2)];
{ let s: Slot[Vec[R]] = Slot.S(a); println("in"); }
println("mid");
println("end");
}
"#,
                "dR1\ndR2\nin\nmid\nend\n",
            ),
            (
                "a BY-VALUE PARAM scrutinee at a read-only arm — the binding holds the buffer here too (before: x1|end|)",
                r#"
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i }; }
struct P { id: i64 }
enum Ew { Z(R), N }
impl Drop for Ew { fn drop(mut ref self) { println("dER") } }
enum Slot[T] { S(T), N }
enum EVecG[T] { V(Vec[T]), N }
enum Mix[T] { A(T), B(Vec[T]), N }
enum G2[T] { X(T, i64), Y }
fn seenv(x: Slot[Vec[R]]) { match x { Slot.S(v) => { println(f"x{v[0].id}") } Slot.N => { println("no") } } }

fn main() {
let a: Vec[R] = [mkr(1), mkr(2)];
seenv(Slot.S(a));
println("end");
}
"#,
                "x1\ndR1\ndR2\nend\n",
            ),
            (
                "ENUM elements: own body first, then the element's payload (before: mid|end|)",
                r#"
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i }; }
struct P { id: i64 }
enum Ew { Z(R), N }
impl Drop for Ew { fn drop(mut ref self) { println("dER") } }
enum Slot[T] { S(T), N }
enum EVecG[T] { V(Vec[T]), N }
enum Mix[T] { A(T), B(Vec[T]), N }
enum G2[T] { X(T, i64), Y }
fn seenv(x: Slot[Vec[R]]) { match x { Slot.S(v) => { println(f"x{v[0].id}") } Slot.N => { println("no") } } }

fn main() {
let a: Vec[Ew] = [Ew.Z(mkr(1)), Ew.Z(mkr(2))];
let s: Slot[Vec[Ew]] = Slot.S(a);
println("mid");
println("end");
}
"#,
                "dER\ndR1\ndER\ndR2\nmid\nend\n",
            ),
            (
                "one container NESTED inside another (before: mid|end|)",
                r#"
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i }; }
struct P { id: i64 }
enum Ew { Z(R), N }
impl Drop for Ew { fn drop(mut ref self) { println("dER") } }
enum Slot[T] { S(T), N }
enum EVecG[T] { V(Vec[T]), N }
enum Mix[T] { A(T), B(Vec[T]), N }
enum G2[T] { X(T, i64), Y }
fn seenv(x: Slot[Vec[R]]) { match x { Slot.S(v) => { println(f"x{v[0].id}") } Slot.N => { println("no") } } }

fn main() {
let i1: Vec[R] = [mkr(1)];
let i2: Vec[R] = [mkr(2)];
let a: Vec[Vec[R]] = [i1, i2];
let s: Slot[Vec[Vec[R]]] = Slot.S(a);
println("mid");
println("end");
}
"#,
                "dR1\ndR2\nmid\nend\n",
            ),
            (
                "MIXED-WIDTH enum, Vec instantiation: the variant's own width decides boxing, not the enum's area (before: mid|end|)",
                r#"
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i }; }
enum Mix[T] { A(T), B(Vec[T]), N }

fn main() {
let a: Vec[R] = [mkr(1), mkr(2)];
let s: Mix[Vec[R]] = Mix.A(a);
println("mid");
println("end");
}
"#,
                "dR1\ndR2\nmid\nend\n",
            ),
            (
                "MIXED-WIDTH enum, Array instantiation — printed an ASLR-varying id before this fix (before: interp dR1|dR2|mid|end| vs compiled dR<addr>|dR0|mid|end|)",
                r#"
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i }; }
enum Slot[T] { S(T), N }
enum Mix[T] { A(T), B(Vec[T]), N }

fn main() {
let a: Array[R, 2] = [mkr(1), mkr(2)];
let s: Mix[Array[R, 2]] = Mix.A(a);
println("mid");
println("end");
}
"#,
                "dR1\ndR2\nmid\nend\n",
            ),
            (
                "CONTROL, the Array twin of the discard cell: compiled-correct before, interpreted-silent (before: interp mid|end| vs compiled dR1|dR2|mid|end|)",
                r#"
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i }; }
enum Slot[T] { S(T), N }
enum Mix[T] { A(T), B(Vec[T]), N }

fn main() {
let a: Array[R, 2] = [mkr(1), mkr(2)];
let _ = Slot.S(a);
println("mid");
println("end");
}
"#,
                "dR1\ndR2\nmid\nend\n",
            ),
            (
                "CONTROL, the Array twin of the bare statement (before: interp mid|end| vs compiled dR1|dR2|mid|end|)",
                r#"
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i }; }
enum Slot[T] { S(T), N }
enum Mix[T] { A(T), B(Vec[T]), N }

fn main() {
let a: Array[R, 2] = [mkr(1), mkr(2)];
Slot.S(a);
println("mid");
println("end");
}
"#,
                "dR1\ndR2\nmid\nend\n",
            ),
            (
                "CONTROL, the Array twin of the nesting cell (before: interp mid|end| vs compiled dR1|dR2|mid|end|)",
                r#"
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i }; }
enum Slot[T] { S(T), N }
enum Mix[T] { A(T), B(Vec[T]), N }

fn main() {
let i1: Array[R, 1] = [mkr(1)];
let i2: Array[R, 1] = [mkr(2)];
let a: Array[Array[R, 1], 2] = [i1, i2];
let s: Slot[Array[Array[R, 1], 2]] = Slot.S(a);
println("mid");
println("end");
}
"#,
                "dR1\ndR2\nmid\nend\n",
            ),
            (
                "AGREED SILENCE, three deep: `elem_te_runs_user_drop` stops at one level and so does the walk (before: mid|end|)",
                r#"
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i }; }
enum Slot[T] { S(T), N }

fn main() {
let a1: Vec[R] = [mkr(1)];
let b1: Vec[Vec[R]] = [a1];
let c1: Vec[Vec[Vec[R]]] = [b1];
let s: Slot[Vec[Vec[Vec[R]]]] = Slot.S(c1);
println("mid");
println("end");
}
"#,
                "mid\nend\n",
            ),
            (
                "AGREED SILENCE, a TWO-FIELD variant: the walker head skips it, so neither side may fire (before: mid|end|)",
                r#"
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i }; }
struct P { id: i64 }
enum Ew { Z(R), N }
impl Drop for Ew { fn drop(mut ref self) { println("dER") } }
enum Slot[T] { S(T), N }
enum EVecG[T] { V(Vec[T]), N }
enum Mix[T] { A(T), B(Vec[T]), N }
enum G2[T] { X(T, i64), Y }
fn seenv(x: Slot[Vec[R]]) { match x { Slot.S(v) => { println(f"x{v[0].id}") } Slot.N => { println("no") } } }

fn main() {
let a: Vec[R] = [mkr(1), mkr(2)];
let s: G2[Vec[R]] = G2.X(a, 7);
println("mid");
println("end");
}
"#,
                "mid\nend\n",
            ),
            (
                "CONTROL, elements with no body: nothing is due and nothing runs (before: mid|end|)",
                r#"
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i }; }
struct P { id: i64 }
enum Ew { Z(R), N }
impl Drop for Ew { fn drop(mut ref self) { println("dER") } }
enum Slot[T] { S(T), N }
enum EVecG[T] { V(Vec[T]), N }
enum Mix[T] { A(T), B(Vec[T]), N }
enum G2[T] { X(T, i64), Y }
fn seenv(x: Slot[Vec[R]]) { match x { Slot.S(v) => { println(f"x{v[0].id}") } Slot.N => { println("no") } } }

fn main() {
let a: Vec[P] = [P { id: 1 }, P { id: 2 }];
let s: Slot[Vec[P]] = Slot.S(a);
println("mid");
println("end");
}
"#,
                "mid\nend\n",
            ),
        ] {
            assert_eq!(run_program(src).as_deref(), Some(want), "[{label}]");
        }
}

/// B-2026-09-20-49 — a `shared` enum's box is NOT EMPTIED by an arm that
/// moves its inline tuple payload on.
///
/// THE ONE CELL THAT SEPARATES THE TWO WAYS OF BALANCING THAT OWNERSHIP,
/// and it is an OUTPUT test rather than a memory one deliberately: both
/// ways are memory-clean, and only this assertion tells them apart.
///
/// When an arm hands a shared enum's tuple payload to a new owner, the box
/// must stop owning the same buffers or the two double-free it. The obvious
/// way is to ZERO the box's payload words — it is what the non-shared path
/// does (`suppress_destructured_enum_payload_cleanup_at_limited`) and what
/// the `BoxedArray` channel's alias disarm does. It is wrong here for a
/// reason with no analogue on the non-shared side: a non-shared enum value
/// has exactly one owner, and a SHARED BOX IS REACHED BY EVERY HANDLE, so
/// zeroing retracts the payload out from under all of them.
///
/// Measured on the first cut of that fix: this program printed
/// `L-again:0/` — an emptied box read back through a surviving handle —
/// against `L-again:7/L-t-aaaaaaaaaaa` from `--interp` and from the tree
/// before the fix. Valgrind reported zero lost bytes, zero invalid reads
/// and zero invalid frees on both, because THE PROGRAM IS MEMORY-BALANCED
/// EITHER WAY. No leak or invalid-access instrument in this project can see
/// a silently wrong VALUE; the interpreter was the only oracle.
///
/// So the box RE-OWNS a deep copy instead
/// (`reown_shared_enum_inline_tuple_payload_on_move`): the arm's binding
/// keeps the ORIGINAL buffers, the box gets fresh ones, and every handle
/// reads the same characters.
///
/// THIS PROGRAM DRAWS AN OWNERSHIP WARNING, and that is load-bearing rather
/// than sloppy. Matching a `shared` enum by value CONSUMES the handle, so a
/// second read after the arm is a move-then-use and the checker says so —
/// in every spelling tried (`let s2 = s1`, a second `match`, and `.clone()`,
/// which a shared enum does not have). There is therefore NO warning-free
/// program in which a shared box survives an arm that moved its payload,
/// which means the re-own keeps a WARNED-but-compiled program agreeing with
/// `--interp` rather than fixing a supported one. It is kept anyway,
/// because it is the only one of the two that agrees with the interpreter
/// on every program the compiler ACCEPTS, and the cheaper version is the one
/// that is silently wrong. Stated so a future reader can retire it
/// deliberately rather than by accident.
#[test]
fn test_e2e_shared_enum_tuple_payload_is_not_emptied_by_a_moving_arm() {
    let src = r#"
shared enum Sh { S((String, i64)), N }
fn mkL(t: String) -> (String, i64) { return (f"L-{t}-aaaaaaaaaaa", 7); }
fn main() {
    let keep = Sh.S(mkL("t"));
    match keep { Sh.S(x) => { let u = x; println(f"L:{u.0}"); } Sh.N => { println("e"); } }
    match keep { Sh.S(y) => { println(f"L-again:{y.1}/{y.0}"); } Sh.N => { println("e2"); } }
    println("done");
}
"#;
    assert_eq!(
        run_program(src),
        Some("L:L-t-aaaaaaaaaaa\nL-again:7/L-t-aaaaaaaaaaa\ndone\n".to_string()),
        "the shared box was emptied by the arm rather than re-owning a copy"
    );
}

/// B-2026-09-21-5 — a generic `ref` parameter over a generic enum READS
/// THE BOXED PAYLOAD AS ITS POINTER.
///
/// `fn shr[T](g: ref G1[T])` over `enum G1[T] { Y(T), N }` with
/// `T = String` printed an ASLR-varying integer where `--interp` printed
/// the string — rc=0, nothing on stderr, and memory BALANCED, so no
/// sanitizer, no leak column and no output-comparison fixture could see
/// it. Only an A/B against the interpreter does, and only if the fixture
/// prints the payload, which is why this cell prints every payload it
/// binds. The by-value spelling (`c`) was correct throughout and is a
/// control that must not move.
///
/// THE ROOT IS A TABLE THAT ONE OF TWO BINDING PATHS NEVER FILLED IN.
/// `record_mono_generic_enum_payload_types` (B-2026-07-13-3) resolves a
/// generic enum's bare-type-param payload through the active monomorph
/// substitution and stashes it span-keyed, because the typechecker records
/// nothing for a `Type::TypeParam` binding. It was called from
/// `bind_pattern_values` only — the VALUE-source path — while a
/// `ref`-matched scrutinee is bound by `bind_pattern_values_via_ptr`,
/// which never called it. Every guard on that path therefore ran against
/// an empty table, and all four of them default to "fine" on an absent
/// type: `pattern_payload_word_count` returned the ERASED width (1)
/// rather than `String`'s 3, so `payload_is_boxed` read `1 > 1` and said
/// no, and `declared_mismatches_word` had no name to compare. The leaf was
/// aliased AT the payload word, which in the erased layout holds the box
/// pointer.
///
/// The boxed-ness guard could not have caught it even in principle, which
/// is worth stating because it was written for this class: it compares a
/// payload's width against its area, and both are one word here, the value
/// being boxed for UNKNOWN size rather than for oversize.
///
/// Cells: the row's own program (`a`), the `mut ref` spelling (`b`) and a
/// generic `impl[T]` `ref self` receiver (`e`), both of which the row
/// listed as NOT MEASURED and the first of which was also broken; the
/// by-value control (`c`); a non-boxing `G1[i64]` payload through the same
/// `ref` parameter (`d`); a struct payload reached through a trait bound
/// (`f`), the shape B-2026-07-09-6's struct guard now sees through the
/// same table; and the payload-less variant (`g`). All four surfaces
/// byte-identical, 28 allocs / 28 frees, 0 valgrind errors.
#[test]
fn test_e2e_generic_ref_param_over_generic_enum_reads_its_payload() {
    let src = r#"
trait Num {
    fn get(ref self) -> i64;
}
struct W { n: i64 }
impl Num for W {
    fn get(ref self) -> i64 { self.n }
}

enum G1[T] { Y(T), N }

fn shr[T](g: ref G1[T]) { match g { G1.Y(v) => { println(f"  rx {v}") } G1.N => { println("  rx NONE") } } }
fn shm[T](g: mut ref G1[T]) { match g { G1.Y(v) => { println(f"  mx {v}") } G1.N => { println("  mx NONE") } } }
fn shg[T](g: G1[T]) { match g { G1.Y(v) => { println(f"  vx {v}") } G1.N => { println("  vx NONE") } } }
fn shw[T: Num](g: ref G1[T]) { match g { G1.Y(v) => { println(f"  wx {v.get()}") } G1.N => { println("  wx NONE") } } }

impl[T] G1[T] {
    fn shs(ref self) -> i64 { match self { G1.Y(v) => { println(f"  sx {v}"); 1 } G1.N => { println("  sx NONE"); 0 } } }
}

fn a_ref_string() { println("a"); let g: G1[String] = G1.Y(f"pa"); shr(g); shr(g) }
fn b_mutref_string() { println("b"); let mut g: G1[String] = G1.Y(f"pb"); shm(mut g) }
fn c_value_string() { println("c"); let g: G1[String] = G1.Y(f"pc"); shg(g) }
fn d_ref_i64() { println("d"); let g: G1[i64] = G1.Y(77); shr(g); shr(g) }
fn e_refself_string() { println("e"); let g: G1[String] = G1.Y(f"pe"); let r = g.shs(); println(f"  r={r}") }
fn f_ref_struct() { println("f"); let g: G1[W] = G1.Y(W { n: 5 }); shw(g); shw(g) }
fn g_ref_none() { println("g"); let g: G1[String] = G1.N; shr(g) }

fn main() {
    a_ref_string();
    b_mutref_string();
    c_value_string();
    d_ref_i64();
    e_refself_string();
    f_ref_struct();
    g_ref_none();
    println("end");
}
"#;
    assert_eq!(
            run_program(src).as_deref(),
            Some(
                "a\n  rx pa\n  rx pa\nb\n  mx pb\nc\n  vx pc\nd\n  rx 77\n  rx 77\ne\n  sx pe\n  r=1\nf\n  wx 5\n  wx 5\ng\n  rx NONE\nend\n"
            )
        );
}

/// B-2026-09-20-12 — a by-value enum argument spelled as a FIELD
/// PROJECTION runs its payload's `Drop` body ONCE.
///
/// `eat(b.w)` over an enum the callee owns BY TRANSFER fired the payload's
/// body twice: once in the callee, which owns it, and once again in the
/// caller when the holder died. The discriminator was a SIBLING VARIANT
/// that is never constructed, never matched and never passed — `Wpr`'s
/// `S(Inner)` classifies `SharedRc`, which makes
/// `enum_param_owned_by_transfer` answer TRUE for the whole TYPE, so a
/// value whose live variant is the inline `A(Array[Sp, 1])` is declared
/// callee-owned. `Wps`, identical but for that variant, is correct and
/// stays correct here (`b:7 dSp41`).
///
/// TWO CHANNELS, AND ONLY ONE WAS STOOD DOWN. B-2026-09-19-51 already
/// zeroes the handed-over field's payload words, which neutralizes
/// `__karac_drop_struct_<S>`'s FREE of it. The holder's field BODIES live
/// in a separate per-binding `__karac_dropbodies_<S>` walker fired from its
/// own `CleanupAction`, which the zero cannot reach — so the second body
/// still ran, over the zeroed words, printing `dSp0`. On an element one
/// word wide that OWNS heap it would read a live pointer instead, which is
/// why the row is filed as a use-after-free rather than as noise.
/// `zero_transfer_owned_enum_field_arg` now masks the field out of that
/// walker as well, through the same `disarm_struct_field_bodies_at` the
/// `let x = h.o` move-out sites use.
///
/// THE MASK IS PER FIELD, and four cells here exist to pin that rather
/// than to restate the fault: a SIBLING enum field (`e`), a plain
/// `Drop`-bearing field (`h`), a NON-transfer enum field beside a transfer
/// one (`i`), and both fields handed over in turn (`f`/`g`). Each
/// survivor's body still runs exactly once. Before the fix every one of
/// these carried a trailing `dSp0`; `j`, where nothing is transfer-owned,
/// is byte-identical on both arms and is the control.
///
/// ORDER IS NOT THIS ROW. The three compiled surfaces agree exactly, and
/// `--interp` prints the same 25 lines in a different order: a
/// transfer-owned argument is dropped by the CALLEE on the compiled
/// backends and at the caller's statement end under `--interp`. That split
/// predates this fix — it is `named-local` (`c`) and `temporary` (`d`)
/// here, both of which this row calls correct — and it is B-2026-09-15-17,
/// whose own prose names the same lever. The multiset of lines is equal on
/// all four surfaces; only the sequence differs, so the twin in
/// `tests/interpreter.rs` pins the interpreter's order deliberately.
#[test]
fn test_e2e_transfer_owned_enum_field_arg_runs_its_payload_body_once() {
    let src = r#"
struct Sp { v: i64 }
impl Drop for Sp { fn drop(mut ref self) { println(f"dSp{self.v}") } }
shared struct Inner { tag: String }

enum Wpr { A(Array[Sp, 1]), S(Inner), N }
enum Wps { A(Array[Sp, 1]), N }

struct Br { w: Wpr, n: i64 }
struct Bs { w: Wps, n: i64 }
struct Two { w: Wpr, u: Wpr, n: i64 }
struct Mix { w: Wpr, s: Sp, n: i64 }
struct MixNT { w: Wpr, v: Wps, n: i64 }

fn eat_r(g: Wpr) -> i64 { match g { Wpr.A(x) => { return 7; } Wpr.S(i) => { return 1; } Wpr.N => { return 0; } } }
fn eat_s(g: Wps) -> i64 { match g { Wps.A(x) => { return 7; } Wps.N => { return 0; } } }

fn proj_sib() { let b: Br = Br { w: Wpr.A([Sp { v: 42 }]), n: 1 }; println(f"a:{eat_r(b.w)}"); }
fn proj_nosib() { let b: Bs = Bs { w: Wps.A([Sp { v: 41 }]), n: 1 }; println(f"b:{eat_s(b.w)}"); }
fn named_local() { let b: Br = Br { w: Wpr.A([Sp { v: 43 }]), n: 1 }; let w2: Wpr = b.w; println(f"c:{eat_r(w2)}"); }
fn temporary() { println(f"d:{eat_r(Wpr.A([Sp { v: 44 }]))}"); }
fn sibling_field() { let t: Two = Two { w: Wpr.A([Sp { v: 51 }]), u: Wpr.A([Sp { v: 52 }]), n: 1 }; println(f"e:{eat_r(t.w)}"); }
fn both_handed() { let t: Two = Two { w: Wpr.A([Sp { v: 53 }]), u: Wpr.A([Sp { v: 54 }]), n: 1 }; println(f"f:{eat_r(t.w)}"); println(f"g:{eat_r(t.u)}"); }
fn plain_field() { let m: Mix = Mix { w: Wpr.A([Sp { v: 55 }]), s: Sp { v: 56 }, n: 1 }; println(f"h:{eat_r(m.w)}"); }
fn nt_sibling() { let m: MixNT = MixNT { w: Wpr.A([Sp { v: 57 }]), v: Wps.A([Sp { v: 58 }]), n: 1 }; println(f"i:{eat_r(m.w)}"); }
fn nt_only() { let m: MixNT = MixNT { w: Wpr.A([Sp { v: 60 }]), v: Wps.A([Sp { v: 61 }]), n: 1 }; println(f"j:{eat_s(m.v)}"); }

fn main() {
    proj_sib();
    proj_nosib();
    named_local();
    temporary();
    sibling_field();
    both_handed();
    plain_field();
    nt_sibling();
    nt_only();
    println("end");
}
"#;
    assert_eq!(
            run_program(src).as_deref(),
            Some(
                "dSp42\na:7\nb:7\ndSp41\ndSp43\nc:7\ndSp44\nd:7\ndSp51\ne:7\ndSp52\ndSp53\nf:7\ndSp54\ng:7\ndSp55\nh:7\ndSp56\ndSp57\ni:7\ndSp58\nj:7\ndSp61\ndSp60\nend\n"
            )
        );
}

/// B-2026-09-23-26 — a by-value `Option[R]` / `Result[R, i64]` param whose
/// payload runs a user `Drop`, returned on some exits only: a tail `if`, a
/// `let`-bound `if`, a tail `match` with bare arms, and a `let`-bound `match`.
/// Before the fix the `let`-bound spellings kept the caller's argument armed
/// while the local handed it back (a segfault on every compiled surface, the
/// body twice under `--interp`), and the tail spellings ran no body at all on
/// the exit where the value died inside the callee, on every surface.
#[test]
fn e2e_conditional_optres_param_handback_runs_one_body() {
    let Some(out) = run_program(
        r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"  d{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn tl(a: Option[R], c: bool) -> Option[R] { if c { a } else { None } }
fn lt(a: Option[R], c: bool) -> Option[R] { let r: Option[R] = if c { a } else { None }; println("  mid"); r }
fn mt(a: Option[R], c: bool) -> Option[R] { match c { true => a, false => None } }
fn ml(a: Option[R], c: bool) -> Option[R] { let r: Option[R] = match c { true => a, false => None }; println("  mid"); r }
fn rs(a: Result[R, i64], c: bool) -> Result[R, i64] { let r: Result[R, i64] = if c { a } else { Err(5) }; r }
fn show(o: Option[R]) { match o { Some(x) => println(f"  y{x.id}"), None => println("  none") } }
fn main() {
    for c in [true, false] {
        println(f"c={c}");
        { let a = Some(mkr(1)); let b = tl(a, c); show(b); }
        { let a = Some(mkr(2)); let b = lt(a, c); show(b); }
        { let a = Some(mkr(3)); let b = mt(a, c); show(b); }
        { let a = Some(mkr(4)); let b = ml(a, c); show(b); }
        { let a: Result[R, i64] = Ok(mkr(5)); let b = rs(a, c); match b { Ok(x) => println(f"  y{x.id}"), Err(e) => println(f"  e{e}") } }
    }
    { let b = lt(Some(mkr(9)), true); show(b); }
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "c=true\n  y1\n  d1\n  mid\n  y2\n  d2\n  y3\n  d3\n  mid\n  y4\n  d4\n  y5\n  d5\nc=false\n  d1\n  none\n  mid\n  d2\n  none\n  d3\n  none\n  mid\n  d4\n  none\n  d5\n  e5\n  mid\n  y9\n  d9\nend\n", "got:\n{out}");
}

/// B-2026-09-23-42 — the ASSOCIATED (`H.sf(a, c)`) and METHOD (`h.mf(a, c)`)
/// spellings of B-2026-09-23-26's conditional `Option` / `Result` hand-back.
/// The let site's passthrough skip matched a bare-identifier callee only, so
/// the result binding registered its own drop of the box the argument's
/// binding still owned: `double free` on every compiled surface at `c = true`,
/// and at `c = false` the method spelling lost the body the free spelling ran.
/// A no-`Drop` payload (`h.nf`) double freed the same way.
#[test]
fn e2e_conditional_optres_param_handback_assoc_and_method() {
    let Some(out) = run_program(
        r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct N { id: i64, s: String }
fn mkr(i: i64) -> R { return R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn mkn(i: i64) -> N { return N { id: i, s: f"heap-string-longer-than-sso-{i}" } }
struct H { k: i64 }
impl H {
    fn sf(a: Option[R], c: bool) -> Option[R] { if c { a } else { None } }
    fn mf(ref self, a: Option[R], c: bool) -> Option[R] { if c { a } else { None } }
    fn rf(a: Result[R, String], c: bool) -> Result[R, String] { if c { a } else { Err("no") } }
    fn nf(ref self, a: Option[N], c: bool) -> Option[N] { if c { a } else { None } }
}
fn show(o: Option[R]) { match o { Some(x) => println(f"y{x.id}"), None => println("none") } }
fn run(c: bool, base: i64) {
    let h = H { k: 1 };
    let a = Some(mkr(base + 1));
    let b = H.sf(a, c);
    show(b);
    let a2 = Some(mkr(base + 2));
    let b2 = h.mf(a2, c);
    show(b2);
    let a3: Result[R, String] = Ok(mkr(base + 3));
    let b3 = H.rf(a3, c);
    match b3 { Ok(x) => println(f"y{x.id}"), Err(e) => println(e) }
    let a4 = Some(mkn(base + 4));
    let b4 = h.nf(a4, c);
    match b4 { Some(x) => println(f"n{x.id} {x.s}"), None => println("none") }
}
fn main() { run(true, 0); run(false, 10); println("end") }
"#,
    ) else {
        return;
    };
    assert_eq!(out, "y1\nd1\ny2\nd2\ny3\nd3\nn4 heap-string-longer-than-sso-4\nd11\nnone\nd12\nnone\nd13\nno\nnone\nend\n", "got:\n{out}");
}
