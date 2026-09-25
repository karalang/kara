//! Option, Result, `?`, try -- fixtures for `tests/codegen.rs`.
//!
//! Split out of `tests/codegen.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test codegen` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test codegen option_result::
//!
//! New fixtures about Option, Result, `?`, try belong in this file.

use super::*;

/// B-2026-09-05-5 — a field that is a GENERIC STRUCT INSTANTIATION
/// (`inner: Gd[R]`) was invisible to the Drop-field gate, so the parent's
/// bodies walker was never emitted and the inner field's body never ran.
///
/// Filed as a temp-argument bug, because that is where the widening probe
/// for B-2026-09-04-24 found it. It is not one: the four cells here — temp
/// literal and NAMED LOCAL, generic and NON-GENERIC parent — were all
/// silent on every compiled backend against a clean `--interp`, which is
/// what moved the defect from a registration path into the one gate every
/// path consults, `user_drop_field_indices_mono`. Its `direct` leg resolved
/// the field's head through the subst and then asked the NAME-KEYED
/// `type_runs_user_drop("Gd")`, which reads `Gd[T] { r: T }` unresolved and
/// answers false. The walker's own recursion resolves `Gd[R]` correctly
/// (`nested_struct_field_subst`) — it was simply never reached, because the
/// module never contained `__karac_dropbodies_Gn2$R` at all.
///
/// Memory was balanced throughout (valgrind: every block freed), so this is
/// bodies-only. The one-level control (`Gd[R]` held directly) is what
/// localized it to the nesting.
/// B-2026-09-09-18 — a FRESH-TEMP `Option`/`Result` argument at a by-value
/// param runs its payload's `Drop` body exactly once.
///
/// The row was filed as "a boxed payload behind an `Option`/`Result` PARAM
/// never runs its body", and the param is not the axis: the NAMED-LOCAL
/// spelling of the identical call was always correct, because the caller's
/// let site owns a by-value optres argument's payload bodies (the rule
/// `stmts.rs`'s optres let arm states as its own reason for marking the
/// callee's leaves param views). A fresh temp has no let site, so the body
/// it owed ran in no frame at all.
///
/// The six cells are the whole point and the three passing ones are not
/// padding: A/B/C are the named-local controls that were already correct
/// and must not double, and F is the escape control — a callee that hands
/// the argument back is owned by whatever binds the result, so registering
/// in the caller as well would run two bodies. Only D and E were missing,
/// and a fixture with just those could not tell the fix from a double
/// fire.
///
/// Memory was balanced before the fix (valgrind: 0 bytes at exit, 0
/// errors) — the callee's own prologue owns the box and its interior — so
/// no sanitizer could see this and the missing line is the only witness.
/// B is what shows the callee's match is irrelevant: it binds the payload
/// out and still owes exactly one body, the caller's.
#[test]
fn e2e_freshtemp_optres_argument_runs_its_payload_drop_body_once() {
    let Some(out) = run_program(
        r#"struct R2 { s: String, t: String, u: String }
impl Drop for R2 { fn drop(mut ref self) { println(f"d:{self.s.len()}") } }
fn mkr(i: i64) -> R2 { return R2 { s: f"ssssssss{i}", t: f"tttttttt{i}", u: f"uuuuuuuu{i}" }; }
fn ignore(x: Option[R2]) { println("  ig"); }
fn matchit(x: Option[R2]) { match x { Option.Some(r) => { println(f"  m:{r.s}"); } Option.None => { println("  mn"); } } }
fn giveback(x: Option[R2]) -> Option[R2] { println("  gb"); return x; }
fn resig(x: Result[R2, i64]) { println("  rig"); }
fn main() {
    println("A named->ignore");   { let a = Option.Some(mkr(1)); ignore(a); }
    println("B named->matchit");  { let b = Option.Some(mkr(2)); matchit(b); }
    println("C named->giveback"); { let c = Option.Some(mkr(3)); let r = giveback(c); }
    println("D temp->ignore");    ignore(Option.Some(mkr(4)));
    println("E temp->matchit");   matchit(Option.Some(mkr(5)));
    println("F temp->giveback");  { let r = giveback(Option.Some(mkr(6))); }
    println("G temp->resig");     resig(Result.Ok(mkr(7)));
    println("end");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"A named->ignore
  ig
d:9
B named->matchit
  m:ssssssss2
d:9
C named->giveback
  gb
d:9
D temp->ignore
  ig
d:9
E temp->matchit
  m:ssssssss5
d:9
F temp->giveback
  gb
d:9
G temp->resig
  rig
d:9
end
"#
    );
}

#[test]
/// B-2026-09-06-70 — the METHOD and ASSOC-FN argument registrars had no
/// ADMISSION gate at all, so a fresh-temp argument a passthrough callee
/// hands straight back had two owners, with no rebind anywhere.
///
/// Both legs computed `escapes_frame` and both fed it to the registrar, but
/// that flag only picks the bodies-vs-memory MODE — nothing declined the
/// registration. For a param the prologue declines to COPY (`R` owns a
/// `shared` field, so `aggregate_param_copy_supported_struct` fails and the
/// param FORWARDS the caller's object) the memory-only registration was a
/// second owner of the buffer the result binding already owns. The free leg
/// has had this gate since B-2026-07-01-7; `fn f(r: R) -> R { return r; }`
/// was clean throughout while its method and assoc twins aborted.
///
/// The cells are the spellings that were red: the assoc passthrough `a`,
/// the method passthrough `b`, the method REBIND `c`, the struct-LITERAL
/// argument `d` (whose parent failure was `malloc(): unaligned tcache chunk
/// detected` — heap corruption, not a detected double free), the two
/// DISCARDED-result calls with no result binding at all, and the loop.
/// `R.mka()` is the discarded assoc call with no argument, which is
/// B-2026-09-07-2's own root and is pinned by
/// `test_e2e_discarded_assoc_fn_call_owns_its_result` — carried here too
/// because the gate is what removed its cover.
///
/// `e`/`g` are the load-bearing controls: `P` is copy-supported, so the
/// callee entry-copies and hands back an INDEPENDENT object whose original
/// the caller must still drop. That case is re-admitted by
/// `arg_is_entry_copied_heap_struct`, and it is the reason the gate is safe
/// at all — without the carve-out this fix would trade every double free
/// for a leak. `k`/`l` are the second control: an argument the callee
/// CONSUMES keeps its body and its memory on both legs.
///
/// -O2 MASKS most of this. LLVM inlines the callee and the double free
/// becomes a surviving use-after-free that prints every line correctly, so
/// on the parent this fixture's output was already right at `-O2` while the
/// program was corrupt. `asan_method_and_assoc_arg_registrars_admit_only_-
/// when_the_result_owns_it` is the half that sees it.
fn test_e2e_method_and_assoc_arg_registrars_admit_only_when_the_result_owns_it() {
    let out = run_program(
        r#"
shared struct Inner { v: i64 }
struct R { id: i64, name: String, inner: Inner }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}", inner: Inner { v: i } }; }
struct P { id: i64, name: String }
impl Drop for P { fn drop(mut ref self) { println(f"dP{self.id}") } }
fn mkP(i: i64) -> P { return P { id: i, name: f"p{i}" }; }
struct Hold { n: i64 }
impl R {
  fn passa(r: R) -> R { return r; }
  fn eata(r: R) -> i64 { return r.id; }
  fn mka() -> R { return mk(31); }
}
impl P { fn passp(p: P) -> P { return p; } }
impl Hold {
  fn thru(ref self, r: R) -> R { return r; }
  fn reb(ref self, r: R) -> R { let m = r; return m; }
  fn thrup(ref self, p: P) -> P { return p; }
  fn eat(ref self, r: R) -> i64 { return r.id; }
}
fn main() {
  let h = Hold { n: 0 };
  let a = R.passa(mk(16)); println(f"a={a.inner.v}");
  let b = h.thru(mk(17)); println(f"b={b.inner.v}");
  let c = h.reb(mk(18)); println(f"c={c.inner.v}");
  let d = h.thru(R { id: 19, name: "h19", inner: Inner { v: 19 } }); println(f"d={d.inner.v}");
  h.thru(mk(20));
  R.passa(mk(21));
  R.mka();
  let mut i = 0;
  while i < 2 { let z = R.passa(mk(22)); println(f"lp={z.inner.v}"); i = i + 1; }
  let e = h.thrup(mkP(23)); println(f"e={e.name}");
  let g = P.passp(mkP(24)); println(f"g={g.name}");
  let k = h.eat(mk(25)); println(f"k={k}");
  let l = R.eata(mk(26)); println(f"l={l}");
  println("end");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
                out,
                "a=16\ndR16\nb=17\ndR17\nc=18\ndR18\nd=19\ndR19\ndR20\ndR21\ndR31\nlp=22\ndR22\nlp=22\ndR22\ne=p23\ndP23\ng=p24\ndP24\ndR25\nk=25\ndR26\nl=26\nend\n",
                "a method or assoc callee that hands its by-value argument back \
                 leaves the result binding the only owner, so the argument temp \
                 must not be registered beside it; got {out:?}"
            );
    }
}

#[test]
fn e2e_freshtemp_result_struct_field_moved_to_free_fn() {
    // B-2026-07-23-4: matching a FRESH-TEMP `Result[W, _]` (a direct `f()`
    // call, not a bound local) whose `Ok(w)` struct payload has a heap field,
    // then passing that field BY VALUE to a free fn (`println(w.s)`), MOVES
    // the field's buffer into the callee — under `karac build` the source
    // `FreeInlineResultPayload` then double-freed it (interp was clean). The
    // wrapper gate now suppresses the source drop on a heap-field-to-free-fn
    // move. Verifies interp == JIT == AOT byte-identical output.
    if let Some(out) = run_program(
        "struct W { s: String }\n\
             fn use_s(x: String) -> i64 { x.len() }\n\
             fn f() -> Result[W, i64] { Ok(W { s: \"boom\".to_string() }) }\n\
             fn main() {\n\
                 match f() {\n\
                     Ok(w) => println(w.s),\n\
                     Err(e) => println(e.to_string()),\n\
                 }\n\
                 match f() {\n\
                     Ok(w) => println(use_s(w.s).to_string()),\n\
                     Err(e) => println(e.to_string()),\n\
                 }\n\
             }",
    ) {
        assert_eq!(out, "boom\n4\n");
    }
}

/// The `Option[T]` half of B-2026-07-29-31. `Option[String]` was rejected
/// at typecheck alongside the user types; `Option[i64]` too, so the gap was
/// never payload-heap-specific. Codegen routes the heap payload through the
/// existing tag-guarded `emit_option_value_clone_fn` and the scalar payload
/// through the shallow whole-value copy (exact for a `{tag, w…}` of
/// scalars). `None` must survive both.
#[test]
fn e2e_option_clone_deep_copies_payload() {
    let Some(out) = run_program(
        "fn main() {\n\
             \x20   let o: Option[String] = Option.Some(\"inner\");\n\
             \x20   let p = o.clone();\n\
             \x20   println(p.unwrap());\n\
             \x20   let q: Option[i64] = Option.Some(5);\n\
             \x20   let r = q.clone();\n\
             \x20   println(f\"{r.unwrap()}\");\n\
             \x20   let n: Option[String] = Option.None;\n\
             \x20   let m = n.clone();\n\
             \x20   println(f\"{m.is_none()}\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "inner\n5\ntrue\n");
}

/// B-2026-07-30-10 — `Result[T, E].clone()`, the sibling gap
/// B-2026-07-29-31 left open. Admitted for the DIRECT String/Vec/scalar
/// halves class codegen can deep-copy in place (`emit_result_value_clone_fn`
/// → `deep_copy_result_inline_heap_halves_in_place`). Exercises a live-Ok
/// String half, a live-Err String half, an all-scalar Result (shallow whole-
/// value copy), and a Vec half — reusing the SOURCE after each clone so the
/// two independent drops both fire. A shallow-alias regression would double-
/// free under the memory-sanitizer suite; here we pin the deep-copy output.
#[test]
fn e2e_result_clone_deep_copies_live_half() {
    let Some(out) = run_program(
            "fn main() {\n\
             \x20   let a: Result[String, i64] = Result.Ok(\"okstr\");\n\
             \x20   let ca = a.clone();\n\
             \x20   match ca { Result.Ok(s) => println(s), Result.Err(e) => println(f\"{e}\"), }\n\
             \x20   match a  { Result.Ok(s) => println(s), Result.Err(e) => println(f\"{e}\"), }\n\
             \x20   let b: Result[i64, String] = Result.Err(\"errstr\");\n\
             \x20   let cb = b.clone();\n\
             \x20   match cb { Result.Ok(n) => println(f\"{n}\"), Result.Err(e) => println(e), }\n\
             \x20   let c: Result[i64, i64] = Result.Ok(9);\n\
             \x20   let cc = c.clone();\n\
             \x20   match cc { Result.Ok(n) => println(f\"{n}\"), Result.Err(e) => println(f\"{e}\"), }\n\
             \x20   let d: Result[Vec[String], i64] = Result.Ok(vec![\"x\", \"y\"]);\n\
             \x20   let cd = d.clone();\n\
             \x20   match cd { Result.Ok(v) => println(f\"{v.len()}\"), Result.Err(e) => println(f\"{e}\"), }\n\
             \x20   match d  { Result.Ok(v) => println(f\"{v.len()}\"), Result.Err(e) => println(f\"{e}\"), }\n\
             }\n",
        ) else {
            return;
        };
    assert_eq!(out, "okstr\nokstr\nerrstr\n9\n2\n2\n");
}

/// B-2026-08-02-25 — displacing an `Option[T]` binding runs the DISPLACED
/// payload's user `impl Drop` body.
///
/// `o = None;` / `o = Some(fresh)` over a live `Some(payload)` dropped the
/// old payload's body on the floor: the binding's scope-exit bodies action
/// reads the slot AFTER the store, so it only ever saw the new value. The
/// value-enum sibling of this leg excludes `Option`/`Result` by name (they
/// never go through `enum_layouts`-driven `__karac_drop_<E>` synthesis, and
/// their payload is a generic param at the layout level), so nothing
/// covered them.
///
/// No sanitizer could witness this and none did: the displaced payload's
/// MEMORY was always freed correctly by the untouched
/// `FreeInlineOptionPayload` / `BoxedEnumDrop` action, so ASan and LSan
/// stayed silent while the destructor simply never ran. The interpreter ran
/// it, making this a run-vs-build divergence on the shipping side. Found by
/// `drop_fuzz`'s drop-log oracle.
///
/// The fix calls `__karac_dropelems_opt_*`, which frees NOTHING by
/// construction, so it cannot double-free beside the existing frees —
/// `asan_optres_payload_user_drop_bodies_fire_once` is the over-fire guard.
#[test]
fn e2e_option_displacement_runs_displaced_payload_user_drop() {
    // Verbatim the two shapes verified by hand against the interpreter.
    // Kept narrow on purpose: several near-variants of this program also
    // emit a SPURIOUS extra body over a stale slot (a garbage tag), which
    // is a separate pre-existing defect (B-2026-08-03-5) present with or
    // without this leg — widening the corpus here would assert that bug's
    // behaviour rather than this fix's.
    const PRE: &str = "struct Tracked { tag: i64, name: String }\n\
             impl Drop for Tracked { fn drop(mut ref self) { println(f\"D{self.tag}\"); } }\n\
             fn new_tracked(tag: i64, name: String) -> Tracked {\n\
             \x20   println(f\"N{tag}\");\n\
             \x20   return Tracked { tag: tag, name: name };\n\
             }\n";
    const PAY: &str = "\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\".to_string()";

    // Displaced to `None` — the old payload's body fires at the assignment.
    if let Some(out) = run_program(&format!(
        "{PRE}fn main() {{\n\
             \x20   let mut o: Option[Tracked] = Some(new_tracked(1i64, {PAY}));\n\
             \x20   o = None;\n\
             \x20   println(999);\n\
             }}\n"
    )) {
        assert_eq!(out, "N1\nD1\n999\n");
    }

    // Displaced to a fresh `Some` — D1 at the assignment, D2 at the
    // binding's NLL end. Exactly one of each: the walk frees nothing, so a
    // double here would show up as a second D1.
    if let Some(out) = run_program(&format!(
        "{PRE}fn main() {{\n\
             \x20   let mut o: Option[Tracked] = Some(new_tracked(1i64, {PAY}));\n\
             \x20   o = Some(new_tracked(2i64, {PAY}));\n\
             \x20   println(999);\n\
             }}\n"
    )) {
        assert_eq!(out, "N1\nN2\nD1\nD2\n999\n");
    }
}

/// B-2026-08-04-2 — a boxed `Option`/`Result` payload bound whole and then
/// MOVED hands the box's interior to the destination.
///
/// The binding is an unboxed COPY of the box's `{ptr,len,cap}` words and
/// registers no memory drop — the box drop's inner walk owns the interior,
/// which is what keeps `if let Some(r) = v.pop()` from double-freeing. But
/// once the binding moves on, the destination registers its own drop over
/// the same buffers and both free them. All four destinations below aborted
/// under glibc; the fix clears the source `BoxedEnumDrop`'s `inner_drop_fn`
/// at each move site, leaving a box-only free.
///
/// A `Drop` impl is NOT what makes this fire — the earlier reading of this
/// class as Drop-dependent was a dead-allocation artifact. Without a live
/// read of the moved payload's heap field the buffer is elided and every
/// case looks clean; `println(w.r.name)` and friends are load-bearing here,
/// and the same program with no `impl Drop` but the same reads aborts
/// identically. The bodies are here to pin the fire COUNT alongside.
#[test]
fn e2e_boxed_optres_payload_view_move_transfers_box_interior() {
    const PRE: &str = "struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
             \x20   fn drop(mut ref self) { println(f\"D{self.id}[{self.name}]\"); }\n\
             }\n\
             struct W { r: Res }\n\
             fn mko(i: i64) -> Option[Res] {\n\
             \x20   return Option.Some(Res { id: i, name: f\"pay{i}\" });\n\
             }\n\
             fn eat(r: Res) -> i64 { return r.name.len(); }\n";

    // The four move destinations, plus the fresh-temp spelling of the
    // first — a temp's box is staged in `__freshtemp_boxed_scrut` rather
    // than a named slot, and the neutralizer has to find it either way.
    if let Some(out) = run_program(&format!(
        "{PRE}fn main() {{\n\
             \x20   {{\n\
             \x20       let o: Option[Res] = Option.Some(Res {{ id: 1, name: f\"pay{{1}}\" }});\n\
             \x20       match o {{\n\
             \x20           Option.Some(r) => {{ let w = W {{ r: r }}; println(w.r.name); }}\n\
             \x20           Option.None => {{ println(\"none\"); }}\n\
             \x20       }}\n\
             \x20       println(\"a-end\");\n\
             \x20   }}\n\
             \x20   {{\n\
             \x20       let o: Option[Res] = Option.Some(Res {{ id: 2, name: f\"pay{{2}}\" }});\n\
             \x20       let r2 = match o {{\n\
             \x20           Option.Some(r) => r,\n\
             \x20           Option.None => Res {{ id: 0, name: f\"z{{0}}\" }},\n\
             \x20       }};\n\
             \x20       println(r2.name);\n\
             \x20       println(\"b-end\");\n\
             \x20   }}\n\
             \x20   {{\n\
             \x20       let o: Option[Res] = Option.Some(Res {{ id: 3, name: f\"pay{{3}}\" }});\n\
             \x20       match o {{\n\
             \x20           Option.Some(r) => {{ let x = r; println(x.name); }}\n\
             \x20           Option.None => {{ println(\"none\"); }}\n\
             \x20       }}\n\
             \x20       println(\"c-end\");\n\
             \x20   }}\n\
             \x20   {{\n\
             \x20       let o: Option[Res] = Option.Some(Res {{ id: 4, name: f\"pay{{4}}\" }});\n\
             \x20       let mut v: Vec[Res] = Vec.new();\n\
             \x20       match o {{\n\
             \x20           Option.Some(r) => {{ v.push(r); }}\n\
             \x20           Option.None => {{ println(\"none\"); }}\n\
             \x20       }}\n\
             \x20       println(v[0].name);\n\
             \x20       println(\"d-end\");\n\
             \x20   }}\n\
             \x20   {{\n\
             \x20       match mko(7i64) {{\n\
             \x20           Option.Some(r) => {{ let w = W {{ r: r }}; println(w.r.name); }}\n\
             \x20           Option.None => {{ println(\"none\"); }}\n\
             \x20       }}\n\
             \x20       println(\"e-end\");\n\
             \x20   }}\n\
             \x20   println(\"end\");\n\
             }}\n"
    )) {
        assert_eq!(
            out,
            "pay1\nD1[pay1]\na-end\n\
                 pay2\nD2[pay2]\nb-end\n\
                 pay3\nD3[pay3]\nc-end\n\
                 pay4\nD4[pay4]\nd-end\n\
                 pay7\nD7[pay7]\ne-end\n\
                 end\n"
        );
    }

    // CONTROL: a by-value fn arg is an entry COPY, not a move. The callee
    // deep-copies and frees its own; the box keeps the interior and the
    // caller's fire keeps the body. Neutralizing here instead LEAKED the
    // box's copy (9 bytes under valgrind) — which is how this control
    // earned its place: the first cut dispatched the neutralizer from
    // `suppress_inline_option_result_binding_move`, whose roster includes
    // call args.
    if let Some(out) = run_program(&format!(
        "{PRE}fn main() {{\n\
             \x20   let o: Option[Res] = Option.Some(Res {{ id: 5, name: f\"pay{{5}}\" }});\n\
             \x20   match o {{\n\
             \x20       Option.Some(r) => {{ println(eat(r)); }}\n\
             \x20       Option.None => {{ println(\"none\"); }}\n\
             \x20   }}\n\
             \x20   println(\"e-end\");\n\
             }}\n"
    )) {
        assert_eq!(out, "4\nD5[pay5]\ne-end\n");
    }

    // CONTROL: not moved at all — the box owns the interior and frees it,
    // exactly as before this leg.
    if let Some(out) = run_program(&format!(
        "{PRE}fn main() {{\n\
             \x20   let o: Option[Res] = Option.Some(Res {{ id: 6, name: f\"pay{{6}}\" }});\n\
             \x20   match o {{\n\
             \x20       Option.Some(r) => {{ println(r.name); }}\n\
             \x20       Option.None => {{ println(\"none\"); }}\n\
             \x20   }}\n\
             \x20   println(\"f-end\");\n\
             }}\n"
    )) {
        assert_eq!(out, "pay6\nD6[pay6]\nf-end\n");
    }
}

/// B-2026-08-11-18 — a chained FIELD ACCESS on an UNWRAPPING sibling's
/// return: `get().unwrap().x` where `get() -> Option[P]`.
///
/// Same run-vs-build shape as B-2026-08-06-19 above, one method-name set
/// over: `karac check` passed, the interpreter printed the right answer,
/// and `karac build` failed with "cannot resolve field 'x' on this
/// receiver", while `let p: P = get().unwrap(); p.x` built and ran.
///
/// The cause is a deliberate exclusion. B-2026-08-09-7 taught
/// `type_name_of_expr` the wrapper-PRESERVING combinators (`map`,
/// `and_then`, …), which return the receiver's own type, and its comment
/// explicitly left out the unwrapping siblings because they return the
/// PAYLOAD — for which "Option" is the wrong answer. That exclusion was
/// right; what was missing was the payload's own name.
///
/// The row characterized this as "the un-bound temporary receiver", and
/// that is measurably too broad: `plain().x` on a struct-returning fn and
/// `P { x: 11 }.x` on a literal are both un-bound temporaries and both
/// always worked. Those two are carried here as live controls, since they
/// are what localize the bug to the unwrap family rather than to
/// temporaries. So is `get().unwrap().double()` — a METHOD call on the
/// same receiver, which resolves through a different path and never
/// failed.
///
/// `unwrap_err` is the load-bearing case for the payload INDEX: it yields
/// `E`, not `T`, so answering arg 0 for it would read `P`'s field layout
/// out of a `Q` value — a miscompile, not a failed lookup. Before the
/// index was made method-dependent this arm failed loudly, which is the
/// safe direction and is why it was caught rather than shipped.
///
/// The nested `Option[Option[P]]` chain pins the recursion: the
/// intermediate payload is itself a wrapper, so the resolver must carry
/// full generic args (`Option[P]`) rather than a head name, or the outer
/// link loses the `[P]` it needs.
///
/// `env.args().len()` seeds every payload so nothing is a compile-time
/// constant (B-2026-08-04-17).
#[test]
fn e2e_chained_field_access_on_option_result_unwrap() {
    let src = r#"
struct P { x: i64 }
struct Q { y: i64 }

impl P { fn double(self) -> i64 { self.x * 2 } }

fn get(n: i64) -> Option[P] { Some(P { x: 5 + n }) }
fn getres(n: i64) -> Result[P, Q] { Ok(P { x: 9 + n }) }
fn geterr(n: i64) -> Result[P, Q] { Err(Q { y: 99 + n }) }
fn nested(n: i64) -> Option[Option[P]] { Some(Some(P { x: 42 + n })) }
fn plain(n: i64) -> P { P { x: 7 + n } }

fn main() {
    let n: i64 = env.args().len();
    println(get(n).unwrap().x);
    println(get(n).expect("missing").x);
    println(get(n).unwrap_or(P { x: 0 }).x);
    println(getres(n).unwrap().x);
    println(geterr(n).unwrap_err().y);
    println(nested(n).unwrap().unwrap().x);
    let o: Option[P] = Some(P { x: 3 + n });
    println(o.unwrap().x);
    println(plain(n).x);
    println(P { x: 11 + n }.x);
    println(get(n).unwrap().double());
    let b: P = get(n).unwrap();
    println(b.x);
}
"#;
    // n = 1. unwrap/expect/unwrap_or on Option[P] → 6; Result Ok → 10;
    // unwrap_err → Q's field, 100 (NOT P's layout); nested → 43;
    // identifier receiver → 4; then the three controls (7+1, 11+1,
    // double of 6) and the bound form.
    assert_eq!(
        run_program(src).as_deref(),
        Some("6\n6\n6\n10\n100\n43\n4\n8\n12\n12\n6\n")
    );
}

/// B-2026-08-05-3 — an `Option`/`Result` payload that is a TUPLE with a
/// heap element must be owned EXACTLY ONCE, on both carriers.
///
/// Three gaps, and the Option one took two attempts:
///
/// (1) RESULT: a consuming arm ran `suppress_inline_result_payload_cleanup`
///     unconditionally, disarming the source for a binding that has NO
///     owner of its own. A struct payload is `track_struct_var`'d and a
///     direct `String`/`Vec` payload is `track_vec_var`'d, but
///     `bind_pattern_values` never `track_tuple_var`s a match binding.
///     `Option` had been consumption-gated since B-2026-07-03-31;
///     `Result` never was.
///
/// (2) GUARD: a guarded arm leaked on both carriers, and only when the
///     guard mentioned the binding — `Ok(x) if x.1 == 5i64` leaked while
///     `Ok(x) if n == 1i64` was clean. `a == b` on scalars does not stay a
///     surface `Binary`; the lowering pass rewrites it to
///     `Call { callee: Path(["i64", "eq"]) }`, which the consumption
///     classifier read as a path-callee CONSTRUCTION.
///
/// (3) OPTION: the box drop was box-ONLY. `track_boxed_enum_var` derives
///     `inner_drop_fn` from a struct NAME, which is `None` for a tuple, so
///     the tuple's elements were never freed. The fix hands the tuple's own
///     recursive drop to that SAME `BoxedEnumDrop`, and a consuming arm
///     retracts it back to box-only.
///
/// THE FIRST ATTEMPT AT (3) IS WHY THE ARMS BELOW ARE PINNED. It registered
/// a SECOND, independent scope-exit drop at the let-site. Every hand probe
/// passed; it double-freed in the wild (drop_fuzz seed 4272, one extra free
/// per loop round), because a droppable tuple payload is at least four
/// words and is therefore ALWAYS already boxed with an owner. Moving the
/// drop onto the existing action fixes that, but is still not sufficient on
/// its own — arms O9 and O10 double-free unless the box's `inner_drop_fn`
/// is retracted:
///
///   * O9  `match o { Some(x) => x.0 }` — the element escapes the match.
///   * O10 `match o { Some((v, k)) => … }` — a per-element destructure,
///     whose leaf bindings each get their own `track_vec_var`.
///
/// Those two are the whole reason this fixture exists in its current form;
/// deleting either re-opens a double free, not a leak.
///
/// Seeded from `env.args().len()`, looped 100x, with every payload read
/// through an element or its bytes so nothing folds or dead-strips at `-O2`
/// (B-2026-08-04-17). ~3,800 allocations.
///
/// This test is the VALUE half — the bug was a pure leak and the printed
/// answer was always correct, so it passes against the unfixed compiler.
/// `asan_optres_tuple_payload_is_owned_exactly_once` is the leak-detecting
/// twin and is the one that goes red; keep the two fixtures in step, and
/// re-verify any change here against `scripts/drop-fuzz.sh`, not only
/// against hand probes — that is the lesson the first attempt paid for.
/// B-2026-08-05-3, RESULT leg — a `Result` payload that is a TUPLE with a
/// heap element must be owned exactly once.
///
/// Two gaps, both on the `Result` carrier:
///
/// (1) A consuming arm ran `suppress_inline_result_payload_cleanup`
///     unconditionally, disarming the source for a binding that has NO
///     owner of its own. A struct payload is `track_struct_var`'d and a
///     direct `String`/`Vec` payload is `track_vec_var`'d, but
///     `bind_pattern_values` never `track_tuple_var`s a match binding — so
///     the buffer went to nobody. `Option` has been consumption-gated since
///     B-2026-07-03-31; `Result` never was.
///
/// (2) A GUARDED arm leaked, and only when the guard mentioned the binding:
///     `Ok(x) if x.1 == 5i64` leaked while `Ok(x) if n == 1i64` was clean.
///     `a == b` on scalars does not stay a surface `Binary` — the lowering
///     pass rewrites it to `Call { callee: Path(["i64", "eq"]) }`, which the
///     consumption classifier read as a path-callee CONSTRUCTION and scored
///     as a transfer. That correction is carrier-independent.
///
/// The OPTION carrier's own leak is NOT fixed here and the row stays open —
/// see its ledger entry. The first attempt registered a second, independent
/// scope-exit drop at the let-site; it passed every hand probe and
/// DOUBLE-FREED in the wild (drop_fuzz seed 4272), because a droppable tuple
/// payload is always >= 4 words and therefore already heap-boxed with a
/// `BoxedEnumDrop` owning it. That is where the Option fix belongs, and it
/// additionally needs the consuming-arm suppressors to clear the box's
/// `inner_drop_fn` — without which the move-out and tuple-destructure arms
/// double-free instead.
///
/// The controls are load-bearing: the tuple PATTERN destructure
/// (`Ok((v, k))`) binds elements that DO own themselves, and the struct and
/// all-scalar payloads must keep their existing behavior.
///
/// Seeded from `env.args().len()`, looped 100x, with every payload read
/// through an element or its bytes so nothing folds or dead-strips at `-O2`
/// (B-2026-08-04-17).
///
/// This test is the VALUE half — the bug was a pure leak and the printed
/// answer was already correct, so it passes against the unfixed compiler.
/// `asan_optres_tuple_payload_is_owned_exactly_once` is the leak-detecting
/// twin and is the one that goes red; keep the two fixtures in step.
/// B-2026-08-05-3 — an `Option`/`Result` payload that is a TUPLE with a
/// heap element must be owned exactly once.
///
/// Three independent gaps, all found by the same fixture:
///
/// (1) `option_payload_struct_or_enum_drop_ok` bailed at its
///     `TypeKind::Path` bind, so a TUPLE payload registered no scope-exit
///     drop at all — the element buffer leaked whether or not the carrier
///     was ever matched. An `Option` bound and never touched leaked
///     identically, which is what places it on the let-site walk rather
///     than on anything about arm binding.
///
/// (2) On the `Result` carrier the leak survived (1): a consuming arm ran
///     `suppress_inline_result_payload_cleanup` unconditionally, disarming
///     the source for a binding that has NO owner of its own. A struct
///     payload is `track_struct_var`'d and a direct `String`/`Vec` payload
///     is `track_vec_var`'d, but a match binding is never
///     `track_tuple_var`'d — so the buffer went to nobody. `Option` has
///     been consumption-gated since B-2026-07-03-31; `Result` never was.
///
/// (3) A GUARDED arm leaked on BOTH carriers, and only when the guard
///     mentioned the binding: `Some(x) if x.1 == 5i64` leaked while
///     `Some(x) if n == 1i64` was clean. `a == b` on scalars does not stay
///     a surface `Binary` — the lowering pass rewrites it to
///     `Call { callee: Path(["i64", "eq"]) }`, which the consumption
///     classifier read as a path-callee CONSTRUCTION and scored as a
///     transfer.
///
/// The controls are what keep the fix honest, and each one caught a real
/// regression during development: the tuple PATTERN destructure
/// (`Some((v, k))`) binds elements that DO own themselves, and admitting it
/// to (2)'s gate turned the leak into a double free; the struct payloads are
/// already registered on another channel, and registering them again at the
/// let-site took `Option[H]` from 8 allocations / 8 frees to 11 / 12 and
/// aborted.
///
/// Seeded from `env.args().len()`, looped 100x, with every payload read
/// through an element or its bytes so nothing folds or dead-strips at `-O2`
/// (B-2026-08-04-17). Pre-fix: 1,807 allocations, 1,007 frees.
///
/// This test is the VALUE half — the bug was a pure leak and the printed
/// answer was already correct, so this one passes against the unfixed
/// compiler. `asan_optres_tuple_payload_is_owned_exactly_once` is the
/// leak-detecting twin and is the one that goes red; keep the two fixtures
/// in step.
#[test]
fn e2e_optres_tuple_payload_is_owned_exactly_once() {
    let Some(out) = run_program(
            "struct H { a: Vec[i64], b: i64 }\n\
             fn mkv(k: i64) -> Vec[i64] { let mut v: Vec[i64] = Vec.new(); v.push(k); v.push(k + 1i64); return v; }\n\
             fn mks(k: i64) -> String { let mut s: String = String.new(); s.push_str(f\"pay-{k}\"); return s; }\n\
             fn dig(i: i64) -> String { let mut d: String = String.new(); d.push_str(f\"{i}\"); return d; }\n\
             fn sinkt(t: (Vec[i64], i64)) -> i64 { return t.0[0i64]; }\n\
             fn main() {\n\
             \x20   let base: i64 = env.args().len();\n\
             \x20   let mut acc = 0i64;\n\
             \x20   let mut i = base;\n\
             \x20   while i < base + 100i64 {\n\
             \x20       // 1. Result[tuple] bound and READ, never moved — the leg the ungated\n\
             \x20       //    arm suppression broke.\n\
             \x20       let r1: Result[(Vec[i64], i64), i64] = Result.Ok((mkv(i), 5i64));\n\
             \x20       match r1 {\n\
             \x20           Result.Ok(x) => { acc = acc + x.0[0i64] + x.1; }\n\
             \x20           Result.Err(e) => { acc = acc + e; }\n\
             \x20       }\n\
             \x20       // 2. Result[tuple] never matched at all — the let-site walk alone.\n\
             \x20       let r2: Result[(Vec[i64], i64), i64] = Result.Ok((mkv(i), 5i64));\n\
             \x20       acc = acc + 1i64;\n\
             \x20       // 3. GUARDED arm — the `x.1 == 5` operator desugar.\n\
             \x20       let r3: Result[(Vec[i64], i64), i64] = Result.Ok((mkv(i), 5i64));\n\
             \x20       match r3 {\n\
             \x20           Result.Ok(x) if x.1 == 5i64 => { acc = acc + x.0[0i64]; }\n\
             \x20           _ => { acc = acc - 1i64; }\n\
             \x20       }\n\
             \x20       // 4. String element, read through its BYTES.\n\
             \x20       let r4: Result[(String, i64), i64] = Result.Ok((mks(i), 5i64));\n\
             \x20       match r4 {\n\
             \x20           Result.Ok(x) => { if x.0.contains(dig(i)) { acc = acc + x.0.len(); } }\n\
             \x20           Result.Err(e) => { acc = acc + e; }\n\
             \x20       }\n\
             \x20       // 5. Vec[String] element.\n\
             \x20       let mut vs: Vec[String] = Vec.new();\n\
             \x20       vs.push(mks(i));\n\
             \x20       vs.push(mks(i + 1i64));\n\
             \x20       let r5: Result[(Vec[String], i64), i64] = Result.Ok((vs, 5i64));\n\
             \x20       match r5 {\n\
             \x20           Result.Ok(x) => { acc = acc + x.0.len() + x.0[0i64].len(); }\n\
             \x20           Result.Err(e) => { acc = acc + e; }\n\
             \x20       }\n\
             \x20       // 6. Heap on the Err half.\n\
             \x20       let r6: Result[i64, (Vec[i64], i64)] = Result.Err((mkv(i), 5i64));\n\
             \x20       match r6 {\n\
             \x20           Result.Ok(v) => { acc = acc + v; }\n\
             \x20           Result.Err(e) => { acc = acc + e.0[0i64]; }\n\
             \x20       }\n\
             \x20       // 7. if-let, borrow-only and moving.\n\
             \x20       let r7: Result[(Vec[i64], i64), i64] = Result.Ok((mkv(i), 5i64));\n\
             \x20       if let Result.Ok(x) = r7 { acc = acc + x.0[0i64]; } else { acc = acc - 1i64; }\n\
             \x20       let r8: Result[(Vec[i64], i64), i64] = Result.Ok((mkv(i), 5i64));\n\
             \x20       let mut g: Vec[i64] = Vec.new();\n\
             \x20       if let Result.Ok(x) = r8 { g = x.0; } else { acc = acc - 1i64; }\n\
             \x20       acc = acc + g[0i64] + g.len();\n\
             \x20       // 8. Moved out of the match into an owned-param callee.\n\
             \x20       let r9: Result[(Vec[i64], i64), i64] = Result.Ok((mkv(i), 5i64));\n\
             \x20       match r9 {\n\
             \x20           Result.Ok(x) => { acc = acc + sinkt(x); }\n\
             \x20           Result.Err(e) => { acc = acc + e; }\n\
             \x20       }\n\
             \x20       // 9. Element MOVED out through the match value.\n\
             \x20       let r10: Result[(Vec[i64], i64), i64] = Result.Ok((mkv(i), 5i64));\n\
             \x20       let g2: Vec[i64] = match r10 { Result.Ok(x) => x.0, Result.Err(e) => Vec.new() };\n\
             \x20       acc = acc + g2[1i64];\n\
             \x20       // --- CONTROLS: shapes that must NOT gain a second owner ---\n\
             \x20       // C1. Tuple PATTERN destructure — the elements own themselves.\n\
             \x20       let r11: Result[(Vec[i64], i64), i64] = Result.Ok((mkv(i), 5i64));\n\
             \x20       match r11 {\n\
             \x20           Result.Ok((v, k)) => { acc = acc + v[0i64] + k; }\n\
             \x20           Result.Err(e) => { acc = acc + e; }\n\
             \x20       }\n\
             \x20       // C2. STRUCT payload — must keep its unconditional arm suppression.\n\
             \x20       let r12: Result[H, i64] = Result.Ok(H { a: mkv(i), b: 5i64 });\n\
             \x20       match r12 {\n\
             \x20           Result.Ok(x) => { acc = acc + x.a[0i64] + x.b; }\n\
             \x20           Result.Err(e) => { acc = acc + e; }\n\
             \x20       }\n\
             \x20       let r13: Result[H, i64] = Result.Ok(H { a: mkv(i), b: 5i64 });\n\
             \x20       match r13 {\n\
             \x20           Result.Ok(x) if x.b == 5i64 => { acc = acc + x.a[0i64]; }\n\
             \x20           _ => { acc = acc - 1i64; }\n\
             \x20       }\n\
             \x20       // C4. All-scalar tuple payload — must get no drop at all.\n\
             \x20       let r15: Result[(i64, i64), i64] = Result.Ok((i, 5i64));\n\
             \x20       match r15 {\n\
             \x20           Result.Ok(x) => { acc = acc + x.0 + x.1; }\n\
             \x20           Result.Err(e) => { acc = acc + e; }\n\
             \x20       }\n\
             \x20       // --- OPTION carrier (B-2026-08-05-3's own shape; second attempt) ---\n\
             \x20       // O1. Bound and READ, never moved.\n\
             \x20       let o1: Option[(Vec[i64], i64)] = Option.Some((mkv(i), 5i64));\n\
             \x20       match o1 {\n\
             \x20           Option.Some(x) => { acc = acc + x.0[0i64] + x.1; }\n\
             \x20           Option.None => { acc = acc - 1i64; }\n\
             \x20       }\n\
             \x20       // O2. NEVER matched at all — the let-site walk alone. This is what\n\
             \x20       //     shows the bug is scope-exit, not arm binding.\n\
             \x20       let o2: Option[(Vec[i64], i64)] = Option.Some((mkv(i), 5i64));\n\
             \x20       acc = acc + 1i64;\n\
             \x20       // O3. GUARDED arm.\n\
             \x20       let o3: Option[(Vec[i64], i64)] = Option.Some((mkv(i), 5i64));\n\
             \x20       match o3 {\n\
             \x20           Option.Some(x) if x.1 == 5i64 => { acc = acc + x.0[0i64]; }\n\
             \x20           _ => { acc = acc - 1i64; }\n\
             \x20       }\n\
             \x20       // O4. String element, read through its BYTES.\n\
             \x20       let o4: Option[(String, i64)] = Option.Some((mks(i), 5i64));\n\
             \x20       match o4 {\n\
             \x20           Option.Some(x) => { if x.0.contains(dig(i)) { acc = acc + x.0.len(); } }\n\
             \x20           Option.None => { acc = acc - 1i64; }\n\
             \x20       }\n\
             \x20       // O5. Vec[String] element.\n\
             \x20       let mut ovs: Vec[String] = Vec.new();\n\
             \x20       ovs.push(mks(i));\n\
             \x20       ovs.push(mks(i + 1i64));\n\
             \x20       let o5: Option[(Vec[String], i64)] = Option.Some((ovs, 5i64));\n\
             \x20       match o5 {\n\
             \x20           Option.Some(x) => { acc = acc + x.0.len() + x.0[0i64].len(); }\n\
             \x20           Option.None => { acc = acc - 1i64; }\n\
             \x20       }\n\
             \x20       // O6. if-let, borrow-only and moving.\n\
             \x20       let o6: Option[(Vec[i64], i64)] = Option.Some((mkv(i), 5i64));\n\
             \x20       if let Option.Some(x) = o6 { acc = acc + x.0[0i64]; } else { acc = acc - 1i64; }\n\
             \x20       let o7: Option[(Vec[i64], i64)] = Option.Some((mkv(i), 5i64));\n\
             \x20       let mut og: Vec[i64] = Vec.new();\n\
             \x20       if let Option.Some(x) = o7 { og = x.0; } else { acc = acc - 1i64; }\n\
             \x20       acc = acc + og[0i64] + og.len();\n\
             \x20       // O8. Moved into an owned-param callee.\n\
             \x20       let o8: Option[(Vec[i64], i64)] = Option.Some((mkv(i), 5i64));\n\
             \x20       match o8 {\n\
             \x20           Option.Some(x) => { acc = acc + sinkt(x); }\n\
             \x20           Option.None => { acc = acc - 1i64; }\n\
             \x20       }\n\
             \x20       // --- The two shapes that DEFEATED the first attempt at this leg. ---\n\
             \x20       // O9. Element MOVED out through the match value. The arm takes the\n\
             \x20       //     box's interior, so the box drop must retract to box-only.\n\
             \x20       let o9: Option[(Vec[i64], i64)] = Option.Some((mkv(i), 5i64));\n\
             \x20       let og2: Vec[i64] = match o9 { Option.Some(x) => x.0, Option.None => Vec.new() };\n\
             \x20       acc = acc + og2[1i64];\n\
             \x20       // O10. Tuple PATTERN destructure — the leaf bindings own their\n\
             \x20       //      elements, so the box drop must retract here too.\n\
             \x20       let o10: Option[(Vec[i64], i64)] = Option.Some((mkv(i), 5i64));\n\
             \x20       match o10 {\n\
             \x20           Option.Some((v, k)) => { acc = acc + v[0i64] + k; }\n\
             \x20           Option.None => { acc = acc - 1i64; }\n\
             \x20       }\n\
             \x20       // O11. STRUCT payload control — a different channel owns it.\n\
             \x20       let o11: Option[H] = Option.Some(H { a: mkv(i), b: 5i64 });\n\
             \x20       match o11 {\n\
             \x20           Option.Some(x) => { acc = acc + x.a[0i64] + x.b; }\n\
             \x20           Option.None => { acc = acc - 1i64; }\n\
             \x20       }\n\
             \x20       // O12. All-scalar tuple payload — must get no drop at all.\n\
             \x20       let o12: Option[(i64, i64)] = Option.Some((i, 5i64));\n\
             \x20       match o12 {\n\
             \x20           Option.Some(x) => { acc = acc + x.0 + x.1; }\n\
             \x20           Option.None => { acc = acc - 1i64; }\n\
             \x20       }\n\
             \x20       i = i + 1i64;\n\
             \x20   }\n\
             \x20   println(f\"acc={acc}\");\n\
             }\n",
        ) else {
            return;
        };
    assert_eq!(out, "acc=108568\n");
}

/// B-2026-08-04-9 — `?` must reconstruct its unwrapped payload at the
/// payload's real width, deboxing when the carrier boxed it.
///
/// `reconstruct_question_ok_payload` read words straight out of the
/// `Option`/`Result` aggregate and rebuilt from three of them. Two
/// independent defects fell out of that, and the fixture separates them
/// because they have different widths and different symptoms:
///
/// (1) BOXED — `Full` is 6 words, so it exceeds both payload areas and w0
/// is the box POINTER, not the payload's first word. The rebuild produced
/// `{box_ptr, undef, undef}`: an empty String under AOT while `karac run`
/// printed the real one, and both the box and its interior leaked.
///
/// (2) WIDE BUT INLINE — `Mid` is 4 words, which FITS `Result`'s 5-word
/// area, so it never boxes and never reaches the debox. It still broke,
/// because the 3-word rebuild helper dropped every word past the third:
/// the String came back right and `pad` came back garbage (1 where the
/// program stored 3). Silent, and again correct under `karac run`.
///
/// The two carriers are load-bearing on `Mid`: through `Result` it is
/// case (2), through `Option` (3-word area) the SAME struct is case (1).
/// One type taking both paths is what proves boxing — not the carrier and
/// not the type — is the discriminator.
///
/// `.unwrap()` on the identical values was always correct, so nothing here
/// pins the shared payload machinery; it pins `?` specifically.
#[test]
fn e2e_question_reconstructs_wide_and_boxed_ok_payloads() {
    let Some(out) = run_program(
        "struct Full { name: String, buf: Vec[i64] }\n\
             struct Mid { name: String, pad: i64 }\n\
             fn mkf(i: i64) -> Full {\n\
             \x20   let mut b: Vec[i64] = Vec.new();\n\
             \x20   b.push(i);\n\
             \x20   let mut s: String = String.new();\n\
             \x20   s.push_str(\"wide-\");\n\
             \x20   s.push_str(f\"{i}\");\n\
             \x20   return Full { name: s, buf: b };\n\
             }\n\
             fn mkm(i: i64) -> Mid {\n\
             \x20   let mut s: String = String.new();\n\
             \x20   s.push_str(\"mid-\");\n\
             \x20   s.push_str(f\"{i}\");\n\
             \x20   return Mid { name: s, pad: i };\n\
             }\n\
             fn resf(i: i64) -> Result[Full, String] { return Result.Ok(mkf(i)); }\n\
             fn optf(i: i64) -> Option[Full] { return Option.Some(mkf(i)); }\n\
             fn optm(i: i64) -> Option[Mid] { return Option.Some(mkm(i)); }\n\
             fn resm(i: i64) -> Result[Mid, String] { return Result.Ok(mkm(i)); }\n\
             fn run_res(i: i64) -> Result[i64, String] {\n\
             \x20   let a = resf(i)?;\n\
             \x20   println(f\"a:{a.name}:{a.buf.len()}\");\n\
             \x20   let b = resm(i)?;\n\
             \x20   println(f\"b:{b.name}:{b.pad}\");\n\
             \x20   let c = resf(i)?;\n\
             \x20   let Full { name, buf: _ } = c;\n\
             \x20   println(f\"c:{name}\");\n\
             \x20   return Result.Ok(1i64);\n\
             }\n\
             fn run_opt(i: i64) -> Option[i64] {\n\
             \x20   let d = optf(i)?;\n\
             \x20   println(f\"d:{d.name}:{d.buf.len()}\");\n\
             \x20   let e = optm(i)?;\n\
             \x20   println(f\"e:{e.name}:{e.pad}\");\n\
             \x20   return Option.Some(2i64);\n\
             }\n\
             fn main() {\n\
             \x20   match run_res(3i64) {\n\
             \x20       Result.Ok(v) => { println(f\"res:{v}\"); }\n\
             \x20       Result.Err(er) => { println(f\"err:{er}\"); }\n\
             \x20   }\n\
             \x20   match run_opt(4i64) {\n\
             \x20       Option.Some(v) => { println(f\"opt:{v}\"); }\n\
             \x20       Option.None => { println(\"opt:none\"); }\n\
             \x20   }\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(
        out,
        "a:wide-3:1\nb:mid-3:3\nc:wide-3\nres:1\n\
             d:wide-4:1\ne:mid-4:4\nopt:2\n"
    );
}

/// B-2026-07-30-11 (Option/Result leg) — a let-bound `Option[P]` /
/// `Result[O, E]` runs its live payload's user `impl Drop` body at the
/// binding's NLL point, on both backends, resolved through ONE shared
/// static chain (annotation -> span-keyed instantiation -> callee return
/// -> source-var record). `Option` and `Result` never had the enum leg's
/// walk: they are built-in (no `EnumDef`) and their declared payload is
/// the bare generic param, so the payload gate here is instantiation-
/// driven — which is what makes it mirrorable, since a payload head that
/// names no user struct fails on both backends identically.
///
/// Shapes: inline ctor let, `Err` side, BOXED payload from an
/// unannotated call (`mk(3)` — the >3-word spill; the body reads the
/// box, not the box pointer as struct bytes), `None` (nothing), match
/// move-out (source disarmed; since the match-arm leg the arm binding
/// fires the body — the 95), a NON-consuming `Some(_)` arm (source stays
/// armed — 96 fires), `unwrap` (consuming-combinator disarm; the result
/// binding's own drop fires instead), and a ctor-arg move
/// (`Option.Some(h)` — h's own drop is silenced, s's walk owns the body).
///
/// Twin of `tests/interpreter.rs`'s
/// `test_optres_payload_runs_user_drop_bodies` on identical source and
/// expected output — the pair is the parity contract.
#[test]
fn e2e_optres_payload_runs_user_drop_bodies() {
    let Some(out) = run_program(
        "struct Res { id: i64 }\n\
             impl Drop for Res { fn drop(mut ref self) { println(90 + self.id); } }\n\
             struct HeapRes { name: String, id: i64 }\n\
             impl Drop for HeapRes { fn drop(mut ref self) { println(80 + self.id); } }\n\
             fn mk(i: i64) -> Option[HeapRes] {\n\
             \x20   return Option.Some(HeapRes { name: \"payload-string-data\", id: i });\n\
             }\n\
             fn main() {\n\
             \x20   let a = Option.Some(Res { id: 1 });\n\
             \x20   println(1);\n\
             \x20   let b: Result[i64, Res] = Err(Res { id: 2 });\n\
             \x20   println(2);\n\
             \x20   let c = mk(3);\n\
             \x20   println(3);\n\
             \x20   let d: Option[Res] = Option.None;\n\
             \x20   println(4);\n\
             \x20   let e = Option.Some(Res { id: 5 });\n\
             \x20   match e {\n\
             \x20       Some(r) => { println(20 + r.id); }\n\
             \x20       None => { println(0); }\n\
             \x20   }\n\
             \x20   println(5);\n\
             \x20   let f = Option.Some(Res { id: 6 });\n\
             \x20   match f {\n\
             \x20       Some(_) => { println(40); }\n\
             \x20       None => { println(0); }\n\
             \x20   }\n\
             \x20   println(6);\n\
             \x20   let g = Option.Some(Res { id: 7 });\n\
             \x20   let r7 = g.unwrap();\n\
             \x20   println(10 + r7.id);\n\
             \x20   println(7);\n\
             \x20   let h = Res { id: 8 };\n\
             \x20   let s = Option.Some(h);\n\
             \x20   println(8);\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(
        out,
        "91\n1\n92\n2\n83\n3\n4\n25\n95\n5\n40\n96\n6\n17\n97\n7\n98\n8\n"
    );
}

/// B-2026-09-04-1 — a STRUCT-FIELD destructure leaf typed `Result[<struct with
/// a Drop body>, _]` runs the payload's body exactly once, on every surface.
///
/// The row's cell (`loc`): the arm only borrows `r`, so nothing at the arm owned
/// the body, and the leaf's memory action was a body-less struct drop — `dR101`
/// ran under `--interp` and on none of jit/aot/AUTO_PAR=0. `iflet`, `errs`,
/// `noread`, `solo` and `rename` are the same defect in five spellings the row
/// did not record; `unused` is the UNCONSUMED leaf, which lost the body on every
/// backend (the B-2026-09-03-33 deferral, closed here). `fcall` / `flit` /
/// `fcallu` are FRESH sources, whose `Result` field had no owner at all (a leak
/// the optimizer's dead-chain mask hid until a body walk made the words live;
/// `fstru` is the unmasked direct-`String` twin, valgrind: 4 bytes). The `w*`
/// cells are the heap-BOXED payload (seven words), which the inline tracker
/// declined and nothing else owned. `param` is the by-value-param source, whose
/// own field walk already ran the body — unchanged.
///
/// The interpreter twin runs the same string; the ASAN twin is
/// `asan_struct_field_result_leaf_is_balanced`.
#[test]
fn e2e_struct_field_result_leaf_owns_its_payload_body() {
    let src = r#"struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}") } }
fn mk(n: i64) -> R { return R { id: n, tag: f"t{n}" }; }
struct W { id: i64, x: String, y: String }
impl Drop for W { fn drop(mut ref self) { println(f"dW{self.id}/{self.x}{self.y}") } }
fn mkw(n: i64) -> W { return W { id: n, x: f"x{n}", y: f"y{n}" }; }
struct HoRes { a: R, b: Result[R, String] }
struct HoErr { a: R, b: Result[String, R] }
struct SoloRes { b: Result[R, String] }
struct HoStr { a: R, b: Result[String, String] }
struct HoW { a: R, b: Result[W, String] }
fn mkho(n: i64) -> HoRes { return HoRes { a: mk(n), b: Result.Ok(mk(n + 100)) }; }
fn mkhs(n: i64) -> HoStr { return HoStr { a: mk(n), b: Result.Ok(f"s{n + 100}") }; }
fn mkhw(n: i64) -> HoW { return HoW { a: mk(n), b: Result.Ok(mkw(n + 100)) }; }

fn loc()    { let h = HoRes { a: mk(1), b: Result.Ok(mk(101)) }; let HoRes { a, b } = h; println(f"  rd{a.id}")
              match b { Result.Ok(r) => println(f"  ok{r.id}"), Result.Err(e) => println(f"  er{e}") } }
fn iflet()  { let h = HoRes { a: mk(2), b: Result.Ok(mk(102)) }; let HoRes { a, b } = h; println(f"  rd{a.id}")
              if let Result.Ok(r) = b { println(f"  ok{r.id}") } }
fn errs()   { let h = HoErr { a: mk(3), b: Result.Err(mk(103)) }; let HoErr { a, b } = h; println(f"  rd{a.id}")
              match b { Result.Ok(s) => println(f"  ok{s}"), Result.Err(r) => println(f"  er{r.id}") } }
fn noread() { let h = HoRes { a: mk(4), b: Result.Ok(mk(104)) }; let HoRes { a, b } = h; println(f"  rd{a.id}")
              match b { Result.Ok(r) => println("  ok"), Result.Err(e) => println(f"  er{e}") } }
fn solo()   { let h = SoloRes { b: Result.Ok(mk(105)) }; let SoloRes { b } = h;
              match b { Result.Ok(r) => println(f"  ok{r.id}"), Result.Err(e) => println(f"  er{e}") } }
fn rename() { let h = HoRes { a: mk(6), b: Result.Ok(mk(106)) }; let HoRes { a: aa, b: bb } = h; println(f"  rd{aa.id}")
              match bb { Result.Ok(r) => println(f"  ok{r.id}"), Result.Err(e) => println(f"  er{e}") } }
fn unused() { let h = HoRes { a: mk(7), b: Result.Ok(mk(107)) }; let HoRes { a, b } = h; println(f"  rd{a.id}") }
fn fcall()  { let HoRes { a, b } = mkho(8); println(f"  rd{a.id}")
              match b { Result.Ok(r) => println(f"  ok{r.id}"), Result.Err(e) => println(f"  er{e}") } }
fn flit()   { let HoRes { a, b } = HoRes { a: mk(9), b: Result.Ok(mk(109)) }; println(f"  rd{a.id}")
              match b { Result.Ok(r) => println(f"  ok{r.id}"), Result.Err(e) => println(f"  er{e}") } }
fn fcallu() { let HoRes { a, b } = mkho(10); println(f"  rd{a.id}") }
fn fstru()  { let HoStr { a, b } = mkhs(11); println(f"  rd{a.id}") }
fn fstr()   { let HoStr { a, b } = mkhs(12); println(f"  rd{a.id}")
              match b { Result.Ok(s) => println(f"  ok{s}"), Result.Err(e) => println(f"  er{e}") } }
fn wloc()   { let h = HoW { a: mk(13), b: Result.Ok(mkw(113)) }; let HoW { a, b } = h; println(f"  rd{a.id}")
              match b { Result.Ok(w) => println(f"  ok{w.id}"), Result.Err(e) => println(f"  er{e}") } }
fn wunused(){ let h = HoW { a: mk(14), b: Result.Ok(mkw(114)) }; let HoW { a, b } = h; println(f"  rd{a.id}") }
fn wfcall() { let HoW { a, b } = mkhw(15); println(f"  rd{a.id}")
              match b { Result.Ok(w) => println(f"  ok{w.id}"), Result.Err(e) => println(f"  er{e}") } }
fn wfcallu(){ let HoW { a, b } = mkhw(16); println(f"  rd{a.id}") }
fn param()  { take(HoRes { a: mk(17), b: Result.Ok(mk(117)) }) }
fn take(h: HoRes) { let HoRes { a, b } = h; println(f"  rd{a.id}")
              match b { Result.Ok(r) => println(f"  ok{r.id}"), Result.Err(e) => println(f"  er{e}") } }

fn main() {
  println("loc");     loc()
  println("iflet");   iflet()
  println("errs");    errs()
  println("noread");  noread()
  println("solo");    solo()
  println("rename");  rename()
  println("unused");  unused()
  println("fcall");   fcall()
  println("flit");    flit()
  println("fcallu");  fcallu()
  println("fstru");   fstru()
  println("fstr");    fstr()
  println("wloc");    wloc()
  println("wunused"); wunused()
  println("wfcall");  wfcall()
  println("wfcallu"); wfcallu()
  println("param");   param()
  println("done")
}
"#;
    assert_eq!(
        run_program(src).as_deref(),
        Some(
            r#"loc
  rd1
dR1/t1
  ok101
dR101/t101
iflet
  rd2
dR2/t2
  ok102
dR102/t102
errs
  rd3
dR3/t3
  er103
dR103/t103
noread
  rd4
dR4/t4
  ok
dR104/t104
solo
  ok105
dR105/t105
rename
  rd6
dR6/t6
  ok106
dR106/t106
unused
dR107/t107
  rd7
dR7/t7
fcall
  rd8
dR8/t8
  ok108
dR108/t108
flit
  rd9
dR9/t9
  ok109
dR109/t109
fcallu
dR110/t110
  rd10
dR10/t10
fstru
  rd11
dR11/t11
fstr
  rd12
dR12/t12
  oks112
wloc
  rd13
dR13/t13
  ok113
dW113/x113y113
wunused
dW114/x114y114
  rd14
dR14/t14
wfcall
  rd15
dR15/t15
  ok115
dW115/x115y115
wfcallu
dW116/x116y116
  rd16
dR16/t16
param
  rd17
  ok117
dR117/t117
dR17/t17
done
"#
        )
    );
}

/// B-2026-09-04-21 — a struct destructured out of a PROJECTION of an owned local
/// (`let HoRes { a, b } = w.inner;`) hands its `Option` / `Result` leaf the
/// field, on every surface.
///
/// On the parent the leaf was a bit-copy VIEW registered in no set while the
/// root kept the memory, so `rmatch` (a consuming arm on a `Result[R, String]`
/// leaf) freed the payload's String from the arm binding and again from the
/// root's drop — glibc's `free(): double free detected in tcache 2` on an
/// ordinary build — and `rcall` / `resc` / `rrebind` / `two` / `awild` aborted
/// the same way; `unused` lost the body on every backend; `omatch` / `ocall` /
/// `oesc` ran the `Option` payload's body at the ROOT's last use (before the
/// leaf was read, or a second time) and `orebind` crashed silently. The leaf
/// now TRANSFERS the field out of the root (cap-zero in place, the same move the
/// by-value-param destructure makes) when the read is a move, and owns its own
/// defensive copy when the root is read again (`rlive`, `olive`, `livecall`);
/// the root's walk is masked for the field either way, so the body fires at
/// the leaf's last use. `wild` / `errs` are the no-payload controls; `nest`
/// moves the destructure into a block. A by-value-PARAM root is deliberately
/// not on this path (its own walk runs the bodies) and keeps its own row.
///
/// Interpreter twin `test_projection_source_optres_leaf_owns_its_field`; ASAN twin `asan_projection_source_optres_leaf_is_balanced`.
#[test]
fn e2e_projection_source_optres_leaf_owns_its_field() {
    let src = r#"struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}") } }
fn mk(n: i64) -> R { return R { id: n, tag: f"t{n}" }; }
struct HoRes { a: R, b: Result[R, String] }
struct HoOpt { a: R, b: Option[R] }
struct WrapR { inner: HoRes }
struct WrapO { inner: HoOpt }
struct Outer { h: WrapR }
fn eat(x: R) { println(f"  eat{x.id}") }

fn unused()  { let w = WrapR { inner: HoRes { a: mk(1), b: Result.Ok(mk(101)) } }; let HoRes { a, b } = w.inner; println(f"  rd{a.id}") }
fn rmatch()  { let w = WrapR { inner: HoRes { a: mk(2), b: Result.Ok(mk(102)) } }; let HoRes { a, b } = w.inner; println(f"  rd{a.id}")
               match b { Result.Ok(r) => println(f"  ok{r.tag}"), Result.Err(e) => println(f"  er{e}") } }
fn rlive()   { let w = WrapR { inner: HoRes { a: mk(3), b: Result.Ok(mk(103)) } }; let HoRes { a, b } = w.inner; println(f"  rd{a.id}")
               match b { Result.Ok(r) => println(f"  ok{r.tag}"), Result.Err(e) => println(f"  er{e}") }
               println(f"  w{w.inner.a.id}") }
fn ounused() { let w = WrapO { inner: HoOpt { a: mk(4), b: Option.Some(mk(104)) } }; let HoOpt { a, b } = w.inner; println(f"  rd{a.id}") }
fn omatch()  { let w = WrapO { inner: HoOpt { a: mk(5), b: Option.Some(mk(105)) } }; let HoOpt { a, b } = w.inner; println(f"  rd{a.id}")
               match b { Option.Some(r) => println(f"  ok{r.tag}"), Option.None => println("  none") } }
fn olive()   { let w = WrapO { inner: HoOpt { a: mk(6), b: Option.Some(mk(106)) } }; let HoOpt { a, b } = w.inner; println(f"  rd{a.id}")
               match b { Option.Some(r) => println(f"  ok{r.tag}"), Option.None => println("  none") }
               println(f"  w{w.inner.a.id}") }
fn two()     { let g = Outer { h: WrapR { inner: HoRes { a: mk(7), b: Result.Ok(mk(107)) } } }; let HoRes { a, b } = g.h.inner; println(f"  rd{a.id}")
               match b { Result.Ok(r) => println(f"  ok{r.tag}"), Result.Err(e) => println(f"  er{e}") } }
fn rcall()   { let w = WrapR { inner: HoRes { a: mk(8), b: Result.Ok(mk(108)) } }; let HoRes { a, b } = w.inner;
               match b { Result.Ok(r) => eat(r), Result.Err(e) => println(f"  er{e}") } }
fn resc()    { let w = WrapR { inner: HoRes { a: mk(9), b: Result.Ok(mk(109)) } }; let HoRes { a, b } = w.inner;
               let g = match b { Result.Ok(r) => r, Result.Err(e) => mk(0) }; println(f"  got{g.id}") }
fn rrebind() { let w = WrapR { inner: HoRes { a: mk(10), b: Result.Ok(mk(110)) } }; let HoRes { a, b } = w.inner; let c = b;
               match c { Result.Ok(r) => println(f"  ok{r.tag}"), Result.Err(e) => println(f"  er{e}") } }
fn ocall()   { let w = WrapO { inner: HoOpt { a: mk(11), b: Option.Some(mk(111)) } }; let HoOpt { a, b } = w.inner;
               match b { Option.Some(r) => eat(r), Option.None => println("  none") } }
fn oesc()    { let w = WrapO { inner: HoOpt { a: mk(12), b: Option.Some(mk(112)) } }; let HoOpt { a, b } = w.inner;
               let g = match b { Option.Some(r) => r, Option.None => mk(0) }; println(f"  got{g.id}") }
fn orebind() { let w = WrapO { inner: HoOpt { a: mk(13), b: Option.Some(mk(113)) } }; let HoOpt { a, b } = w.inner; let c = b;
               match c { Option.Some(r) => println(f"  ok{r.tag}"), Option.None => println("  none") } }
fn wild()    { let w = WrapR { inner: HoRes { a: mk(14), b: Result.Ok(mk(114)) } }; let HoRes { a, b: _ } = w.inner; println(f"  rd{a.id}") }
fn awild()   { let w = WrapR { inner: HoRes { a: mk(15), b: Result.Ok(mk(115)) } }; let HoRes { a: _, b } = w.inner;
               match b { Result.Ok(r) => println(f"  ok{r.tag}"), Result.Err(e) => println(f"  er{e}") } }
fn nest()    { let w = WrapR { inner: HoRes { a: mk(16), b: Result.Ok(mk(116)) } };
               { let HoRes { a, b } = w.inner; println(f"  rd{a.id}") }
               println("  outer") }
fn errs()    { let w = WrapR { inner: HoRes { a: mk(17), b: Result.Err("e17") } }; let HoRes { a, b } = w.inner;
               match b { Result.Ok(r) => println(f"  ok{r.tag}"), Result.Err(e) => println(f"  er{e}") } }
fn livecall(){ let w = WrapR { inner: HoRes { a: mk(18), b: Result.Ok(mk(118)) } }; let HoRes { a, b } = w.inner;
               match b { Result.Ok(r) => eat(r), Result.Err(e) => println(f"  er{e}") }
               println(f"  w{w.inner.a.id}") }

fn main() {
  println("unused");   unused()
  println("rmatch");   rmatch()
  println("rlive");    rlive()
  println("ounused");  ounused()
  println("omatch");   omatch()
  println("olive");    olive()
  println("two");      two()
  println("rcall");    rcall()
  println("resc");     resc()
  println("rrebind");  rrebind()
  println("ocall");    ocall()
  println("oesc");     oesc()
  println("orebind");  orebind()
  println("wild");     wild()
  println("awild");    awild()
  println("nest");     nest()
  println("errs");     errs()
  println("livecall"); livecall()
  println("done")
}
"#;
    assert_eq!(
        run_program(src).as_deref(),
        Some(
            r#"unused
dR101/t101
  rd1
dR1/t1
rmatch
  rd2
dR2/t2
  okt102
dR102/t102
rlive
  rd3
dR3/t3
  okt103
dR103/t103
  w3
ounused
dR104/t104
  rd4
dR4/t4
omatch
  rd5
dR5/t5
  okt105
dR105/t105
olive
  rd6
dR6/t6
  okt106
dR106/t106
  w6
two
  rd7
dR7/t7
  okt107
dR107/t107
rcall
dR8/t8
  eat108
dR108/t108
resc
dR9/t9
  got109
dR109/t109
rrebind
dR10/t10
  okt110
dR110/t110
ocall
dR11/t11
  eat111
dR111/t111
oesc
dR12/t12
  got112
dR112/t112
orebind
dR13/t13
  okt113
dR113/t113
wild
dR114/t114
  rd14
dR14/t14
awild
dR15/t15
  okt115
dR115/t115
nest
dR116/t116
  rd16
dR16/t16
  outer
errs
dR17/t17
  ere17
livecall
dR18/t18
  eat118
dR118/t118
  w18
done
"#
        )
    );
}

/// B-2026-09-04-25 — a struct destructured out of a PROJECTION of a by-value
/// PARAM (`fn f(w: WrapR) { let HoRes { a, b } = w.inner; .. }`) hands each
/// `Option` / `Result` leaf the field, on every surface.
///
/// On the parent the leaf was a view of the param's storage registered in no
/// set, while the param's own `StructDrop` still owned the field: `rmatch` ran
/// the payload's body twice on aot and aborted with glibc's `free(): double free
/// detected in tcache 2` on jit; `rlive` ran it twice around the later read;
/// `rcall` ran it from the arm binding and again from `eat`'s callee copy; and
/// under `--interp`, `wild` / `awild` ran a discarded field's body at the
/// destructure and again at the param's exit. The projection now takes the
/// identifier source's transfer (`sfld.move` into the param in place, the leaf
/// owning the field) and its leaves are param views (`mark_views`), so a
/// consuming arm's binding takes the memory-only struct drop its identifier
/// twin takes; the interpreter's discard walk defers a projected wildcard to
/// the param's drop as it already did for `let HoRes { a, b: _ } = h`. `two`
/// is the two-hop root, `errs` the `Err` side, `runused` / `ounused` the unread
/// leaf, `livecall` a moving arm with the param read again. A rebind of the
/// leaf and a `self` receiver split identically for the identifier source and
/// keep their own row.
///
/// Interpreter twin `test_param_projection_optres_leaf_owns_its_field`; ASAN twin `asan_param_projection_optres_leaf_is_balanced`.
#[test]
fn e2e_param_projection_optres_leaf_owns_its_field() {
    let src = r#"struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}") } }
fn mk(n: i64) -> R { return R { id: n, tag: f"t{n}" }; }
struct HoRes { a: R, b: Result[R, String] }
struct HoErr { a: R, b: Result[String, R] }
struct HoOpt { a: R, b: Option[R] }
struct WrapR { inner: HoRes }
struct WrapE { inner: HoErr }
struct WrapO { inner: HoOpt }
struct Outer { h: WrapR }
fn eat(x: R) { println(f"  eat{x.id}") }

fn rmatch(w: WrapR)  { let HoRes { a, b } = w.inner; println(f"  rd{a.id}")
                       match b { Result.Ok(r) => println(f"  ok{r.tag}"), Result.Err(e) => println(f"  er{e}") } }
fn rlive(w: WrapR)   { let HoRes { a, b } = w.inner; println(f"  rd{a.id}")
                       match b { Result.Ok(r) => println(f"  ok{r.tag}"), Result.Err(e) => println(f"  er{e}") }
                       println(f"  w{w.inner.a.id}") }
fn two(o: Outer)     { let HoRes { a, b } = o.h.inner; println(f"  rd{a.id}")
                       match b { Result.Ok(r) => println(f"  ok{r.tag}"), Result.Err(e) => println(f"  er{e}") } }
fn rcall(w: WrapR)   { let HoRes { a, b } = w.inner;
                       match b { Result.Ok(r) => eat(r), Result.Err(e) => println(f"  er{e}") } }
fn runused(w: WrapR) { let HoRes { a, b } = w.inner; println(f"  rd{a.id}") }
fn errs(w: WrapE)    { let HoErr { a, b } = w.inner;
                       match b { Result.Ok(s) => println(f"  ok{s}"), Result.Err(r) => println(f"  er{r.tag}") } }
fn wild(w: WrapR)    { let HoRes { a, b: _ } = w.inner; println(f"  rd{a.id}") }
fn awild(w: WrapR)   { let HoRes { a: _, b } = w.inner;
                       match b { Result.Ok(r) => println(f"  ok{r.tag}"), Result.Err(e) => println(f"  er{e}") } }
fn livecall(w: WrapR){ let HoRes { a, b } = w.inner;
                       match b { Result.Ok(r) => eat(r), Result.Err(e) => println(f"  er{e}") }
                       println(f"  w{w.inner.a.id}") }
fn omatch(w: WrapO)  { let HoOpt { a, b } = w.inner; println(f"  rd{a.id}")
                       match b { Option.Some(r) => println(f"  ok{r.tag}"), Option.None => println("  none") } }
fn olive(w: WrapO)   { let HoOpt { a, b } = w.inner; println(f"  rd{a.id}")
                       match b { Option.Some(r) => println(f"  ok{r.tag}"), Option.None => println("  none") }
                       println(f"  w{w.inner.a.id}") }
fn ocall(w: WrapO)   { let HoOpt { a, b } = w.inner;
                       match b { Option.Some(r) => eat(r), Option.None => println("  none") } }
fn ounused(w: WrapO) { let HoOpt { a, b } = w.inner; println(f"  rd{a.id}") }

fn main() {
  println("rmatch");   rmatch(WrapR { inner: HoRes { a: mk(1), b: Result.Ok(mk(101)) } })
  println("rlive");    rlive(WrapR { inner: HoRes { a: mk(2), b: Result.Ok(mk(102)) } })
  println("two");      two(Outer { h: WrapR { inner: HoRes { a: mk(3), b: Result.Ok(mk(103)) } } })
  println("rcall");    rcall(WrapR { inner: HoRes { a: mk(4), b: Result.Ok(mk(104)) } })
  println("runused");  runused(WrapR { inner: HoRes { a: mk(5), b: Result.Ok(mk(105)) } })
  println("errs");     errs(WrapE { inner: HoErr { a: mk(6), b: Result.Err(mk(106)) } })
  println("wild");     wild(WrapR { inner: HoRes { a: mk(7), b: Result.Ok(mk(107)) } })
  println("awild");    awild(WrapR { inner: HoRes { a: mk(8), b: Result.Ok(mk(108)) } })
  println("livecall"); livecall(WrapR { inner: HoRes { a: mk(9), b: Result.Ok(mk(109)) } })
  println("omatch");   omatch(WrapO { inner: HoOpt { a: mk(10), b: Option.Some(mk(110)) } })
  println("olive");    olive(WrapO { inner: HoOpt { a: mk(11), b: Option.Some(mk(111)) } })
  println("ocall");    ocall(WrapO { inner: HoOpt { a: mk(12), b: Option.Some(mk(112)) } })
  println("ounused");  ounused(WrapO { inner: HoOpt { a: mk(13), b: Option.Some(mk(113)) } })
  println("done")
}
"#;
    assert_eq!(
        run_program(src).as_deref(),
        Some(
            r#"rmatch
  rd1
  okt101
dR101/t101
dR1/t1
rlive
  rd2
  okt102
  w2
dR102/t102
dR2/t2
two
  rd3
  okt103
dR103/t103
dR3/t3
rcall
  eat104
dR104/t104
dR4/t4
runused
  rd5
dR105/t105
dR5/t5
errs
  ert106
dR106/t106
dR6/t6
wild
  rd7
dR107/t107
dR7/t7
awild
  okt108
dR108/t108
dR8/t8
livecall
  eat109
  w9
dR109/t109
dR9/t9
omatch
  rd10
  okt110
dR110/t110
dR10/t10
olive
  rd11
  okt111
  w11
dR111/t111
dR11/t11
ocall
  eat112
dR112/t112
dR12/t12
ounused
  rd13
dR113/t113
dR13/t13
done
"#
        )
    );
}

/// B-2026-09-04-9 — A FRESH TUPLE SOURCE'S `Option` BINDING LEAF OWNS ITS
/// PAYLOAD'S `Drop` BODY.
///
/// B-2026-09-03-39 gave this leaf its element TYPE, which is what makes the
/// payload walker buildable; what it still lacked was an owner to hang the
/// walker on. The wildcard sibling took the element whole
/// (`free_memory: true`) and was fixed there; the binding leaf keeps its
/// memory where `track_destructure_leaf_cleanup` put it, so it needs the
/// `track_inline_option_agg_payload_var` owner as well — bodies alone leak
/// 76 bytes per occurrence, because arming a `ContainerElemBodies` action
/// stops whoever was freeing the boxed payload.
///
/// IT COULD NOT LAND UNTIL B-2026-09-04-10 DID. Registering that owner
/// balanced every leaf that stays put and DOUBLE-FREED the one that moves
/// whole, because the move-disarm did not know this tracker's set. -10
/// taught it, and this fix then also had to widen that disarm to the
/// struct-literal and enum-struct-variant FIELD inits — a field takes
/// ownership, and `let wm = W { p: om };` aborted with `free(): double free
/// detected in tcache 2` until it did. A call ARGUMENT is the counter-case
/// and stays on the old entry point: it does not transfer, so disarming
/// there loses the body instead (`arg` is the cell that proves it).
///
/// EIGHT CELLS WERE RED ON THE COMPILED BACKENDS and the row recorded two.
/// `bind` and `bindsib` are the row's own; `slot0` puts the `Option` in
/// element 0, `nested` reaches the leaf through a nested pattern, `loopb`
/// runs it twice, `consume` hands the payload to a `match` arm, and `arg`
/// passes it to a function. Pre-fix, this exact program loses `dR132`,
/// `dR148`, `dR72`, `dR102`, both `dR164`s, `dR152` and `dR158`.
///
/// TWO CELLS WERE RED ON THE INTERPRETER, which is why its twin is not a
/// no-op here as it usually is: `nested` and `nestpl` lost `dR102` and
/// `dR106` under `--interp` while all three compiled surfaces ran them
/// once the codegen half landed. `record_destructure_optres_payload_tes`
/// walked only the TOP level of a tuple pattern, and the element types it
/// reads flattened a nested tuple to `None`. Both halves are fixed, so the
/// twins share one string again.
///
/// THE CONTROLS COVER THE OPPOSITE FAILURE — a body running twice, or one
/// starting where none is due. `moved`, `ret` and `field` were already
/// correct because the DESTINATION owns the payload afterwards and runs the
/// body; `instr` is an inline `Option[String]` with no user `Drop`;
/// `plainel` is a struct element; `none` has no payload; `wildcd` is the
/// wildcard leaf B-2026-09-03-39 fixed.
///
/// Twin of `tests/interpreter.rs`'s
/// `test_fresh_tuple_option_binding_leaf_owns_its_payload_body`, pinned to
/// the same string.
#[test]
fn e2e_fresh_tuple_option_binding_leaf_owns_its_payload_body() {
    let src = r#"struct R { id: i64, s: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}:{self.s}:{self.xs.len()}") } }
struct W { p: Option[R] }
fn mk(n: i64) -> R { return R { id: n, s: f"s{n}", xs: [n, n] }; }
fn eat(xa: Option[R]) -> i64 { match xa { Option.Some(ra) => { ra.id }, Option.None => { 0 } } }
fn give() -> Option[R] { let (_, ob) = (mk(56), Option.Some(mk(156))); return ob }

fn bind()   { let (_, oc) = (mk(32), Option.Some(mk(132))); println("  b") }
fn bindsib(){ let (ad, bd) = (mk(48), Option.Some(mk(148))); println(f"  rd{ad.id}") }
fn slot0()  { let (oe, ke) = (Option.Some(mk(72)), 5); println(f"  k{ke}") }
fn nested() { let ((_, of), nf) = ((mk(2), Option.Some(mk(102))), 3); println(f"  n{nf}") }
fn nestpl() { let tg = ((mk(6), Option.Some(mk(106))), 7); let ((_, og), ng) = tg; println(f"  p{ng}") }
fn loopb()  { let mut i = 0; while i < 2 { let (_, oh) = (mk(64), Option.Some(mk(164))); i = i + 1; } println("  l") }
fn consume(){ let (_, oi) = (mk(52), Option.Some(mk(152))); match oi { Option.Some(ri) => { println(f"  c{ri.id}") }, Option.None => { println("  z") } } }
fn moved()  { let (_, oj) = (mk(54), Option.Some(mk(154))); let qj = oj; println(f"  m{qj.is_some()}") }
fn ret()    { let zk = give(); println(f"  r{zk.is_some()}") }
fn arg()    { let (_, ol) = (mk(58), Option.Some(mk(158))); println(f"  a{eat(ol)}") }
fn field()  { let (_, om) = (mk(76), Option.Some(mk(176))); let wm = W { p: om }; println(f"  f{wm.p.is_some()}") }
fn instr()  { let (_, on) = (mk(60), Option.Some(f"q60")); println("  s") }
fn none()   { let np: Option[R] = Option.None; let (_, op) = (mk(62), np); println("  o") }
fn plainel(){ let (_, oq) = (mk(9), mk(109)); println("  q") }
fn wildcd() { let (_, _) = (mk(31), Option.Some(mk(131))); println("  w") }

fn main() {
  println("bind");    bind()
  println("bindsib"); bindsib()
  println("slot0");   slot0()
  println("nested");  nested()
  println("nestpl");  nestpl()
  println("loopb");   loopb()
  println("consume"); consume()
  println("moved");   moved()
  println("ret");     ret()
  println("arg");     arg()
  println("field");   field()
  println("instr");   instr()
  println("none");    none()
  println("plainel"); plainel()
  println("wildcd");  wildcd()
  println("done")
}
"#;
    assert_eq!(
        run_program(src).as_deref(),
        Some(
            r#"bind
dR32:s32:2
dR132:s132:2
  b
bindsib
dR148:s148:2
  rd48
dR48:s48:2
slot0
dR72:s72:2
  k5
nested
dR2:s2:2
dR102:s102:2
  n3
nestpl
dR6:s6:2
dR106:s106:2
  p7
loopb
dR64:s64:2
dR164:s164:2
dR64:s64:2
dR164:s164:2
  l
consume
dR52:s52:2
  c152
dR152:s152:2
moved
dR54:s54:2
  mtrue
dR154:s154:2
ret
dR56:s56:2
  rtrue
dR156:s156:2
arg
dR58:s58:2
  a158
dR158:s158:2
field
dR76:s76:2
  ftrue
dR176:s176:2
instr
dR60:s60:2
  s
none
dR62:s62:2
  o
plainel
dR9:s9:2
dR109:s109:2
  q
wildcd
dR31:s31:2
dR131:s131:2
  w
done
"#
        )
    );
}

/// B-2026-09-03-21 — AN `Option` ELEMENT OF A TUPLE **TEMP-LITERAL ARGUMENT**
/// MUST NOT LOSE ITS PAYLOAD'S `Drop` BODY (OR ITS HEAP).
///
/// `takes((mk(1), Option.Some(mk(11))))` ran `dR11` under `--interp` and nothing
/// on jit / build / AUTO_PAR=0, and leaked the payload's heap besides — 22 B over
/// the filing row's probe, which reported only the lost body.
///
/// THE CAUSE IS AN ERASED ELEMENT TYPE, not the by-value tuple param the row
/// blamed. `infer_arg_elem_te` resolved a variant-ctor element through the
/// namers, which yield the bare head `Option` with `generic_args: None`, and
/// every optres consumer — `emit_optres_payload_user_drop_bodies_fn` for the
/// body, `tuple_elem_optres_drop_ok` for the memory — reads `generic_args` and
/// declines outright without them. `enumtemp` is the control that isolates it:
/// a USER-ENUM element in the identical position was always correct, because its
/// walkers key on the NAME, which survives the erasure.
///
/// `localarg` and `localonly` are the controls that put the fault on the
/// ARGUMENT FORM rather than on the param: the same callee reached from a NAMED
/// LOCAL, and the same tuple never passed at all, were both correct throughout.
/// That is why the two fixes the filing row records — one in the param prologue,
/// one in the shared `emit_tuple_elem_drops` optres leg — could not work: the
/// owner is the caller's temp registration.
///
/// `nonetemp` pins `Option.None` (nothing to run), `destr` the destructured
/// spelling of the same call, `errtemp` a `Result` whose payload is a scalar.
///
/// A `Result.Ok` element carrying a Drop payload is covered by
/// `e2e_result_ctor_tuple_temp_arg_keeps_its_payload_drop_body` below.
/// It was split out (B-2026-09-04-26) on the reading that enriching its
/// type traded a lost body for a 56-byte leak; that reading was an
/// artifact. The leak was already there at `KARAC_OPT_LEVEL=0` — pre-fix
/// the cell reports 17 allocs / 15 frees — and `-O2` hid it by deleting a
/// `malloc` nothing reads, the same dead-allocation elimination that made
/// B-2026-09-04-3 look "balanced". Filling `E` from the callee's declared
/// parameter type closes the body and the leak together. Every cell in
/// this fixture is valgrind-clean ("All heap blocks were freed").
///
/// Twin of `tests/interpreter.rs`'s
/// `test_optres_ctor_tuple_temp_arg_keeps_its_payload_drop_body`, pinned to the
/// same string.
#[test]
fn e2e_optres_ctor_tuple_temp_arg_keeps_its_payload_drop_body() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}/{self.xs.len()}") } }
enum W { A(R), N }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] }; }

fn optArg(t: (R, Option[R]))       -> i64 { println(f"  rd{t.0.id}"); return 0; }
fn resArg(t: (R, Result[R, i64]))  -> i64 { println(f"  rd{t.0.id}"); return 0; }
fn enumArg(t: (R, W))              -> i64 { println(f"  rd{t.0.id}"); return 0; }
fn destr(t: (R, Option[R]))        -> i64 { let (r, o) = t; println(f"  rd{r.id}"); return 0; }

fn main() {
    println("opttemp");  let _ = optArg((mk(1), Option.Some(mk(11))));  println("opttemp end")
    println("errtemp");  let _ = resArg((mk(3), Result.Err(7)));        println("errtemp end")
    println("enumtemp"); let _ = enumArg((mk(4), W.A(mk(44))));         println("enumtemp end")
    println("destr");    let _ = destr((mk(5), Option.Some(mk(55))));   println("destr end")
    println("nonetemp"); let _ = optArg((mk(6), Option.None));          println("nonetemp end")
    println("localarg"); let a = (mk(7), Option.Some(mk(77))); let _ = optArg(a); println("localarg end")
    println("localonly"); let b = (mk(8), Option.Some(mk(88))); println(f"  rd{b.0.id}"); println("localonly end")
    println("done")
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"opttemp
  rd1
dR1/t1/1
dR11/t11/1
opttemp end
errtemp
  rd3
dR3/t3/1
errtemp end
enumtemp
  rd4
dR4/t4/1
dR44/t44/1
enumtemp end
destr
  rd5
dR5/t5/1
dR55/t55/1
destr end
nonetemp
  rd6
nonetemp end
localarg
  rd7
dR7/t7/1
dR77/t77/1
localarg end
localonly
  rd8
dR8/t8/1
dR88/t88/1
localonly end
done
"#
    );
}

/// B-2026-09-04-26 — the `Result` head of the same erasure, which
/// B-2026-09-03-21 measured and deliberately declined.
///
/// `Result.Ok(mk(22))` as a tuple-temp argument named its element the bare
/// `Result`, so `emit_optres_payload_user_drop_bodies_fn` (which needs
/// generic args) declined and the payload's body was lost on all three
/// compiled surfaces. Filling only the OK type is not enough and is the
/// trap the sibling row hit: `Result[R, <empty>]` satisfies the bodies
/// walker but not `tuple_elem_optres_drop_ok`, which tests BOTH arms, so
/// the body runs and the 56-byte envelope leaks. `E` is unknowable from
/// the expression, so it comes from the callee's declared parameter type.
///
/// Cells, in order: the row's own repro; two calls (the leak scaled per
/// call, 112 B in 2 blocks); `Result` and `Option` in ONE call, which the
/// sibling row recorded as a separate cross-cell leak and which this closes
/// too; a heap `E` (`Result[R, String]`), where a wrong `E` would be a
/// double free rather than a leak; `Result.Err`, correct before and after;
/// and the annotated `let`, which always worked and is what fixes the
/// argument form to the same answer.
#[test]
fn e2e_result_ctor_tuple_temp_arg_keeps_its_payload_drop_body() {
    let Some(out) = run_program(
            "struct R { id: i64, tag: String, xs: Vec[i64] }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}/{self.xs.len()}\"); } }\n\
             fn mk(k: i64) -> R { return R { id: k, tag: \"t\", xs: [1, 2, 3] } }\n\
             fn resArg(t: (R, Result[R, i64])) { println(f\"rd{t.0.id}\") }\n\
             fn resStr(t: (R, Result[R, String])) { println(f\"rs{t.0.id}\") }\n\
             fn both(a: (R, Result[R, i64]), b: (R, Option[R])) { println(f\"bo{a.0.id}{b.0.id}\") }\n\
             fn main() {\n\
             \x20   resArg((mk(2), Result.Ok(mk(22))));\n\
             \x20   resArg((mk(3), Result.Ok(mk(33))));\n\
             \x20   both((mk(4), Result.Ok(mk(44))), (mk(5), Option.Some(mk(55))));\n\
             \x20   resStr((mk(6), Result.Ok(mk(66))));\n\
             \x20   resArg((mk(7), Result.Err(9)));\n\
             \x20   { let t: (R, Result[R, i64]) = (mk(8), Result.Ok(mk(88))); resArg(t); }\n\
             \x20   println(\"end\");\n\
             }\n",
        ) else {
            return;
        };
    assert_eq!(
            out,
            "rd2\ndR2/3\ndR22/3\nrd3\ndR3/3\ndR33/3\nbo45\ndR5/3\ndR55/3\ndR4/3\ndR44/3\nrs6\ndR6/3\ndR66/3\nrd7\ndR7/3\nrd8\ndR8/3\ndR88/3\nend\n"
        );
}

/// B-2026-09-04-22 — a heap-BOXED `Result` payload bound out of an agg
/// destructure leaf (tuple element or struct field) and handed to a
/// by-value call keeps its `Drop` body, which runs AFTER the call — the
/// interpreter's order, and the plain-local twin's (`six`).
///
/// The borrow gate both agg suppressors sit behind consulted only the
/// `Option` membership set, so a `Result` leaf never read as borrow-only
/// and its suppressor zeroed the payload under `Ok(w) => eatw(w)`: no
/// body on any compiled surface and, for a boxed payload, a 56-byte
/// envelope leak at -O0 that -O2 hid (the row called it balanced).
/// `five` is the `Option` leaf that was always right; `four` the `Err`
/// side; `three` the `if let` spelling of the same borrow.
///
/// `seven` is a different defect fixed at the same time: the `if let`
/// sites never called the `Result` suppressor at all, so a MOVING
/// `if let Ok(w) = b { let g = w }` left the leaf armed while `g` owned
/// the contents — `free(): double free detected in tcache 2`. It now
/// prints once. Its envelope leaked until B-2026-09-05-9, whose fixture
/// (`e2e_agg_leaf_boxed_payload_moving_arm_frees_envelope`) covers every
/// moving arm over a boxed agg payload.
#[test]
fn e2e_result_agg_leaf_boxed_payload_by_value_call_keeps_body() {
    let Some(out) = run_program(
            "struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             struct W { id: i64, x: String, y: String }\n\
             impl Drop for W { fn drop(mut ref self) { println(f\"dW{self.id}/{self.x}{self.y}\") } }\n\
             struct HoW { a: R, b: Result[W, String] }\n\
             fn eatw(w: W) { println(f\"eat{w.id}\") }\n\
             fn mkw(k: i64) -> W { return W { id: k, x: f\"x{k}\", y: f\"y{k}\" } }\n\
             fn main() {\n\
                 { let t: (R, Result[W, String]) = (R { id: 1 }, Result.Ok(mkw(11))); let (a, b) = t; match b { Result.Ok(w) => eatw(w), Result.Err(e) => println(\"err\") } println(\"one\") }\n\
                 { let h: HoW = HoW { a: R { id: 2 }, b: Result.Ok(mkw(22)) }; let HoW { a, b } = h; match b { Result.Ok(w) => eatw(w), Result.Err(e) => println(\"err\") } println(\"two\") }\n\
                 { let t: (R, Result[W, String]) = (R { id: 3 }, Result.Ok(mkw(33))); let (a, b) = t; if let Result.Ok(w) = b { eatw(w) } println(\"three\") }\n\
                 { let t: (R, Result[W, String]) = (R { id: 4 }, Result.Err(\"e4\")); let (a, b) = t; match b { Result.Ok(w) => eatw(w), Result.Err(e) => println(f\"err{e}\") } println(\"four\") }\n\
                 { let t: (R, Option[W]) = (R { id: 5 }, Option.Some(mkw(55))); let (a, b) = t; match b { Option.Some(w) => eatw(w), Option.None => println(\"none\") } println(\"five\") }\n\
                 { let r: Result[W, String] = Result.Ok(mkw(66)); match r { Result.Ok(w) => eatw(w), Result.Err(e) => println(\"err\") } println(\"six\") }\n\
                 { let t: (R, Result[W, String]) = (R { id: 7 }, Result.Ok(mkw(77))); let (a, b) = t; if let Result.Ok(w) = b { let g: W = w; println(f\"g{g.id}\") } println(\"seven\") }\n\
                 println(\"end\")\n\
             }\n\
             ",
        ) else {
            return;
        };
    assert_eq!(out, "dR1\neat11\ndW11/x11y11\none\ndR2\neat22\ndW22/x22y22\ntwo\ndR3\neat33\ndW33/x33y33\nthree\ndR4\nerre4\nfour\ndR5\neat55\ndW55/x55y55\nfive\neat66\ndW66/x66y66\nsix\ndR7\ng77\ndW77/x77y77\nseven\nend\n");
}

/// B-2026-09-19-36 — the remainder of B-2026-09-19-21, one `Option` layer
/// deeper. A generic callee that wraps its boxed payload parameter in
/// `Option.Some` before placing it in the returned aggregate
/// (`return Ho { g: Option.Some(g) }`) left two owners on one box:
/// `free(): double free detected in tcache 2` where `--interp` printed `mx 10`.
///
/// TWO gates declined it, not one, which is why the earlier fix did not reach
/// it. `fn_returns_param` decides whether the argument is even a hand-back
/// CANDIDATE, and its `expr_is_ident` recognized a bare identifier, a struct
/// literal and a tuple — but not a variant constructor, so `Option.Some(g)`
/// answered `false` and the disarm was never attempted. Past that,
/// `collect_handback_box_words` looked only for a leaf whose TYPE equals the
/// argument slot's; `coerce_to_payload_words` decomposes the argument's
/// envelope into the OUTER envelope's payload words, so the box survives as a
/// plain `i64` at some index and no leaf carries that type any more.
///
/// Naming `Option` and `Result` in the predicate is COMPLETE rather than a
/// shortcut: `E_ENUM_NESTED_ENUM_PAYLOAD` rejects a user enum with a plain
/// enum payload outright, so those two are the only wrappers a plain generic
/// enum can reach.
///
/// `optF` / `resF` / `bareF` are the dies-inside legs and carry the weight
/// here, because a WIDER disarm is the direction that strands boxes: the
/// callee keeps the argument and returns a payload-free variant, so the
/// caller must still free its own. `discard` hands the result to nobody.
/// `diesin` never wraps at all. `optAll` is the all-paths spelling, whose
/// argument was already disarmed statically and which this must not disturb.
/// `bareT` answers the row's own open question — a bare `Option[G1[T]]`
/// return with no surrounding struct had the same double free, and the same
/// fix reaches it.
///
/// Measured per cell against the parent tree: `optT`, `resT` and `bareT`
/// aborted at exit 134 with one `Invalid free()` each and now exit 0; no
/// negative cell moved. As with B-2026-09-19-21, the three repaired cells
/// trade the double free for a 24-byte leak, and `optAll` is the proof that
/// leak pre-dates the change — it is the one cell already disarmed on the
/// parent tree and the one cell that already leaked there. B-2026-09-19-35
/// owns that missing drop.
///
/// Byte-identical to the interpreter twin, which is the assertion.
#[test]
fn e2e_option_wrapped_handback_leaves_one_owner_on_the_payload_box() {
    let Some(out) = run_program(
        r#"enum G1[T] { Y(T), N }
struct Ho[T] { g: Option[G1[T]] }
struct Hr[T] { g: Result[G1[T], i64] }

fn optW[T](g: G1[T], c: bool) -> Ho[T] { if c { return Ho { g: Option.Some(g) } } return Ho { g: Option.None }; }
fn optAll[T](g: G1[T]) -> Ho[T] { return Ho { g: Option.Some(g) }; }
fn resW[T](g: G1[T], c: bool) -> Hr[T] { if c { return Hr { g: Result.Ok(g) } } return Hr { g: Result.Err(7) }; }
fn optBare[T](g: G1[T], c: bool) -> Option[G1[T]] { if c { return Option.Some(g) } return Option.None; }
fn eats[T](g: G1[T], c: bool) -> i64 { match g { G1.Y(v) => { return 1; } G1.N => { return 0; } } }

fn shwO(o: Option[G1[String]]) { match o { Option.Some(i) => { match i { G1.Y(v) => { println(f"  mx {v.len()}") } G1.N => { println("  mx 0") } } } Option.None => { println("  none") } } }
fn shwR(r: Result[G1[String], i64]) { match r { Result.Ok(i) => { match i { G1.Y(v) => { println(f"  mx {v.len()}") } G1.N => { println("  mx 0") } } } Result.Err(e) => { println("  err") } } }

fn main() {
    println("optT");     { let g: G1[String] = G1.Y(f"aaaaaaaa-1"); let h = optW(g, true); shwO(h.g) }
    println("optF");     { let g: G1[String] = G1.Y(f"aaaaaaaa-2"); let h = optW(g, false); shwO(h.g) }
    println("optAll");   { let g: G1[String] = G1.Y(f"aaaaaaaa-3"); let h = optAll(g); shwO(h.g) }
    println("resT");     { let g: G1[String] = G1.Y(f"aaaaaaaa-4"); let h = resW(g, true); shwR(h.g) }
    println("resF");     { let g: G1[String] = G1.Y(f"aaaaaaaa-5"); let h = resW(g, false); shwR(h.g) }
    println("bareT");    { let g: G1[String] = G1.Y(f"aaaaaaaa-6"); let o = optBare(g, true); shwO(o) }
    println("bareF");    { let g: G1[String] = G1.Y(f"aaaaaaaa-7"); let o = optBare(g, false); shwO(o) }
    println("diesin");   { let g: G1[String] = G1.Y(f"aaaaaaaa-8"); let n = eats(g, true); println(f"  e{n}") }
    println("discard");  { let g: G1[String] = G1.Y(f"aaaaaaaa-9"); optW(g, true); println("  x") }
    println("end");
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "optT\n  mx 10\noptF\n  none\noptAll\n  mx 10\nresT\n  mx 10\nresF\n  err\nbareT\n  mx 10\nbareF\n  none\ndiesin\n  e1\ndiscard\n  x\nend\n", "got:\n{out}");
}

/// B-2026-09-10-22 — a whole-payload arm binding over a GENERIC by-value
/// `Option[(T, i64)]` param runs the payload element's `Drop` body.
///
/// `fn take[T](o: Option[(T, i64)]) { match o { Some(t) => t.1 } }` printed `g9`
/// on the JIT, `-O0` and `-O2` auto-par alike against `--interp`'s `dW3 g9` — the
/// body ran on NO compiled surface — while the CONCRETE twin of the same function
/// was correct throughout. A run-vs-build divergence, so the kata A/B rule catches
/// it where a body count cannot.
///
/// THE CAUSE IS WHICH ESCAPE MAP THE TWO CALL PATHS ASK. `compile_call` resolves a
/// per-projection policy from the param type (B-2026-09-14-5: a projection is a
/// READ exactly when its leaf carries no `Drop` body), while
/// `compile_generic_call` asked `optres_payload_escaping_param_variants` — the
/// fixed, fully projection-INTOLERANT end of that axis. So on the monomorph leg
/// `t.1` read as an escape, the caller stood down for a taker that does not exist,
/// and nobody ran the body. Both paths now call one helper,
/// `optres_payload_escape_map`, which is what keeps them from drifting again.
///
/// `g3` answers the row's first unmeasured axis: `Result` behaves identically.
/// `g6` is the `let x = t.1; return x;` spelling, which loses it the same way.
///
/// THE CONTROLS ARE WHAT LOCATE THE AXIS, and three of them were already correct
/// before the fix: `g2` (the concrete twin), `g4` (`Some(_)`, which never names the
/// payload), `g5` (`println(f"{t.1}")`, a copy read that does not `return`) and
/// `g7` (a BARE `T` payload). `g7` is the row's second unmeasured axis and it
/// answers no: the monomorph alone is not the trigger — it takes a generic
/// callee AND a projection of a tuple payload flowing into `return`.
///
/// `g8` IS THE OVER-REACH CONTROL, in the opposite direction: `return t.0` really
/// does move the `Drop`-bearing element out, so the returned value owns the body
/// and exactly ONE must run. Applying the copy-read policy to it would register a
/// second one in the caller — the `dR1 / len:1 / dR1` double B-2026-09-12-15
/// measured. It stays at one here, which is what says the policy is per-projection
/// rather than tolerant.
///
/// TWO THINGS IN THIS TRANSCRIPT ARE OTHER ROWS. `g8`'s interpreter line runs
/// TWICE where the compiled backends run it once — B-2026-09-13-5, open, and this
/// fixture adds that the GENERIC spelling behaves exactly like the concrete one it
/// was filed on. And `g8`'s `d` param (`W { id: 108 }`) runs its body on no
/// surface because the `None` arm returns it — the conditionally-returned-param
/// family, B-2026-09-13-13. Neither moved with this fix.
///
/// THE HEAP-BEARING PAYLOAD IS A DIFFERENT LEG AND IS STILL BROKEN: give `W` a
/// `String` field and the payload BOXES, the caller correctly stands down because
/// the box is the callee's, and the monomorph prologue never runs the by-value
/// `Option`/`Result` param arms at all — so the body has no owner anywhere. Filed
/// as B-2026-09-17-23 with the probe evidence; deliberately not fixed here,
/// because it needs the mono prologue to gain those arms rather than a policy
/// change. This fixture's `W` is scalar on purpose, which is the shape the row was
/// filed on.
///
/// Memory at `KARAC_OPT_LEVEL=0`: 25 allocs / 25 frees, `ERROR SUMMARY: 0 errors`,
/// "All heap blocks were freed".
///
/// Twin: `tests/interpreter.rs`'s
/// `test_generic_by_value_optres_param_payload_body_runs`, pinned to the
/// INTERPRETER's transcript, which differs only in `g8`'s doubled line.
#[test]
/// `dW108` — the unused `d` argument's body on the `Some` path — is absent
/// from this transcript and from its interpreter twin's, and always was.
/// `g8[T](o: Option[(T, i64)], d: T) -> T` returns `d` only on the `None`
/// arm; this call takes `Some`, so `d` dies inside `g8` and exactly one
/// body is owed. No surface runs it. That is B-2026-09-17-32 —
/// B-2026-08-28-22's per-path conditional-escape flag recognises an
/// `if`/`else` tail and not a `match` arm tail — and it is agreed on all
/// four surfaces, so the A/B rule cannot see it. Whoever fixes it adds a
/// `dW108` line here and to the twin.
fn e2e_generic_by_value_optres_param_payload_body_runs() {
    assert_eq!(
        run_program(
            r#"struct W { id: i64 }
impl Drop for W { fn drop(mut ref self) { println(f"dW{self.id}") } }

fn g1[T](o: Option[(T, i64)]) -> i64 { match o { Some(t) => { return t.1; } None => { return 0; } } }
fn g2(o: Option[(W, i64)]) -> i64 { match o { Some(t) => { return t.1; } None => { return 0; } } }
fn g3[T](o: Result[(T, i64), i64]) -> i64 { match o { Ok(t) => { return t.1; } Err(e) => { return e; } } }
fn g4[T](o: Option[(T, i64)]) -> i64 { match o { Some(_) => { return 5; } None => { return 0; } } }
fn g5[T](o: Option[(T, i64)]) -> i64 { match o { Some(t) => { println(f"    p{t.1}"); return 0; } None => { return 0; } } }
fn g6[T](o: Option[(T, i64)]) -> i64 { match o { Some(t) => { let x = t.1; return x; } None => { return 0; } } }
fn g7[T](o: Option[T]) -> i64 { match o { Some(t) => { return 1; } None => { return 0; } } }
fn g8[T](o: Option[(T, i64)], d: T) -> T { match o { Some(t) => { return t.0; } None => { return d; } } }

fn main() {
  println("g1"); println(f"  {g1(Some((W { id: 1 }, 9)))}");
  println("g2"); println(f"  {g2(Some((W { id: 2 }, 9)))}");
  println("g3"); println(f"  {g3(Ok((W { id: 3 }, 9)))}");
  println("g4"); println(f"  {g4(Some((W { id: 4 }, 9)))}");
  println("g5"); println(f"  {g5(Some((W { id: 5 }, 9)))}");
  println("g6"); println(f"  {g6(Some((W { id: 6 }, 9)))}");
  println("g7"); println(f"  {g7(Some(W { id: 7 }))}");
  println("g8"); let r = g8(Some((W { id: 8 }, 9)), W { id: 108 }); println(f"  {r.id}");
  println("end");
}
"#
        ),
        Some(
            r#"g1
dW1
  9
g2
dW2
  9
g3
dW3
  9
g4
dW4
  5
g5
    p9
dW5
  0
g6
dW6
  9
g7
dW7
  1
g8
  8
dW8
end
"#
            .to_string()
        )
    );
}

/// B-2026-08-31-10 (diagnostic half) — the payload shapes that STAY
/// declined say which type and which payload, and stop prescribing a `let`
/// that is already there.
///
/// One string served two arrivals: an f-string interpolating a struct
/// LITERAL or call result, where "bind it to a `let` first" is the fix, and
/// an Option/Result PLACE EXPRESSION whose payload the synthesizer declines
/// — already `let`-bound, so the advice was false as well as unhelpful.
///
/// THE SUBJECT HAS RUN OUT, and the declined-payload assertion is GONE.
/// It had been retired three times — `Option[Vector[i64, 4]]`, then
/// `Option[Array[i64, 3]]` (B-2026-08-31-18 / -19), then
/// `Option[Slice[i64]]` (B-2026-08-31-25 / -41) — leaving only an
/// UNSUBSTITUTED GENERIC `T` (`Option[T]` inside
/// `fn f[T: Display](x: Option[T])`), which this fixture pinned
/// deliberately and under protest, because that was a DEFECT rather than
/// an inherently unrenderable type. Its standing instruction was to DELETE
/// the assertion when B-2026-08-31-39 was fixed rather than hunt for a
/// fourth victim, and that is what happened: the generic instantiations
/// render now, so the program that used to be the declined subject is
/// asserted below to PRINT, alongside the three retired shapes.
///
/// Nothing pins the declined-payload arm of `deferred_display_error` any
/// more, and that is the honest state rather than an oversight — every `E`
/// the payload gate would decline is rejected EARLIER by the typechecker
/// (`does not implement Display`), measured across `VecDeque`, `Fn(..)`,
/// `()` and `Option` wrappers of each. The arm is kept for a shape that has
/// not been invented yet; the message it would produce is not asserted.
///
/// The message prescribes no rewrite, and for every shape that has actually
/// reached the declined arm the rewrite would have been worse than the
/// error — see `deferred_display_error`'s doc for all three.
///
/// The declined shape was `Option[Vector[i64, 4]]` when this was written;
/// B-2026-08-31-18 and -19 taught codegen both vectors and arrays, so the
/// fixture moves to `Slice` and KEEPS the retired shapes as a second
/// assertion that they now build and match the interpreter — the same
/// pattern the `main`-error fixture uses. Back then the advice would have
/// been worse than useless: destructuring them bound `Some(0)` / `Some(1)`
/// against the interpreter's real values.
///
/// The literal/call-result case must KEEP the old text — the fix adds a
/// branch rather than replacing the message.
#[test]
fn codegen_declined_option_payload_names_its_shape() {
    // The former declined subject. Asserted as OUTPUT rather than deleted,
    // so a regression that re-declines it fails on the fixture that
    // documents why it was ever declined (B-2026-08-31-39).
    if let Some(out) = run_program(
        r#"fn show[T: Display](x: Option[T]) { println(f"{x}"); }
fn main() {
    let a: Array[i64, 2] = [1, 2];
    show(Some(a));
}
"#,
    ) {
        assert_eq!(out, "Some([1, 2])\n");
    }

    let lit_err = codegen_error(
        r#"#[derive(Display)]
struct P { x: i64 }
fn mk() -> P { return P { x: 1 } }
fn main() { println(f"{mk()}"); }
"#,
    );
    assert!(
        lit_err.contains("bind a struct literal or call result to a `let` first"),
        "the literal/call-result case keeps its own advice; got: {lit_err}"
    );

    // The retired shapes: both compile now, and both render. Asserted here
    // rather than dropped, so a regression that re-declines either fails on
    // the row that documents why it was ever declined.
    let Some(out) = run_program(
        r#"fn main() {
    let n = env.args().len() as i64;
    let vv: Vector[i64, 4] = Vector[i64, 4](n, n + 1, n + 2, n + 3);
    let ovv: Option[Vector[i64, 4]] = Some(vv);
    println(f"{ovv}");
    let a: Array[i64, 3] = [n, n + 1, n + 2];
    let oa: Option[Array[i64, 3]] = Some(a);
    println(f"{oa}");
    let mut v: Vec[i64] = Vec.new();
    v.push(n);
    v.push(n + 1);
    let os: Option[Slice[i64]] = Some(v.as_slice());
    println(f"{os}");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        "Some(Vector(1, 2, 3, 4))\nSome([1, 2, 3])\nSome([1, 2])\n"
    );
}

#[test]
fn codegen_generic_option_payload_renders_at_its_instantiation() {
    let Some(out) = run_program(
        r#"struct H { n: i64 }
impl H {
    fn show[T: Display](ref self, x: Option[T]) { println(f"m {self.n} {x}"); }
}

fn show[T: Display](x: Option[T]) { println(f"{x}"); }
fn showr[T: Display](x: Result[T, String]) { println(f"{x}"); }

fn main() {
    show(Some(7));
    show(Some("hi"));

    let v: Vec[i64] = vec![1];
    show(Some(v));
    let vs: Vec[String] = vec!["p" + "q"];
    show(Some(vs));
    let vv: Vec[Vec[i64]] = vec![vec![1, 2]];
    show(Some(vv));

    let a2: Array[i64, 2] = [1, 2];
    show(Some(a2));
    let a3: Array[i64, 3] = [3, 4, 5];
    show(Some(a3));

    let c: Array[i64, 2] = [6, 7];
    let sl: Slice[i64] = c[0..2];
    show(Some(sl));

    let t: (i64, i64) = (8, 9);
    show(Some(t));

    let rv: Vec[i64] = vec![1, 2];
    showr(Ok(rv));

    let h = H { n: 3 };
    h.show(Some(7));
    let mv: Vec[i64] = vec![4];
    h.show(Some(mv));
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        "Some(7)\n\
             Some(hi)\n\
             Some([1])\n\
             Some([pq])\n\
             Some([[1, 2]])\n\
             Some([1, 2])\n\
             Some([3, 4, 5])\n\
             Some([6, 7])\n\
             Some((8, 9))\n\
             Ok([1, 2])\n\
             m 3 Some(7)\n\
             m 3 Some([4])\n"
    );
}

/// B-2026-08-28-58 — an own-`Drop` enum held as an `Option`/`Result`
/// PAYLOAD runs its body when the binding dies. The last position in the
/// -46/-47/-54/-55 family, and the one that failed worst: silent on all
/// three backends for `Option`, so no A/B gate could report it.
///
/// TWO defects had to be fixed together, and this fixture pins both.
///
/// The payload-bodies walker demanded `struct_types`, so a user-enum
/// payload got no walker and no `UserDrop` registration at all — the
/// missing-body half, `Option` and `Result` alike.
///
/// `emit_drop_fn_for_type_expr`'s B-2026-07-30-11 guard covered structs
/// only, so for a Drop-bearing ENUM the MEMORY channel's
/// `module.get_function("karac_drop_<E>")` lookup returned the user-drop
/// WRAPPER (`emit_user_drop_wrappers` mints one for every type in
/// `drop_method_keys`, enums included) and ran the body at scope exit.
/// That is why `Result` printed `drop E` AFTER the last statement on both
/// compiled backends while the interpreter printed nothing — a live
/// run-vs-build divergence hiding inside the missing-body row. Fixing only
/// the walker would have made `Result` print the body TWICE, which is the
/// reason the two legs are one commit and one test.
///
/// `result-*` therefore carries the ordering, not just the count: the body
/// belongs at the binding's live-range end (before `mid`), never at scope
/// exit.
///
/// `payload-only-*` is the -54 predicate reaching this position: an enum
/// with no own `Drop` but a Drop-bearing payload. `no-drop-*` is the
/// boundary that keeps the widening honest — an enum with neither runs
/// nothing.
#[test]
fn e2e_own_drop_enum_as_optres_payload_runs_its_body() {
    const H: &str = "enum E { A(R), B }\n\
             impl Drop for E { fn drop(mut ref self) { println(\"drop E\") } }\n\
             enum H { A(R), B }\n\
             enum J { A(i64), B }\n\
             struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop R{self.id}\") } }\n";
    for (label, body, want) in [
        // The agree-on-zero half: `Option`, silent on all three backends.
        (
            "option-unit",
            "fn main() { let o: Option[E] = Some(E.B); println(\"mid\"); }\n",
            "drop E\nmid\n",
        ),
        (
            "option-payload",
            "fn main() { let o: Option[E] = Some(E.A(R { id: 4 }));\n\
                 \x20            println(\"mid\"); }\n",
            "drop E\ndrop R4\nmid\n",
        ),
        // The run-vs-build half: `Result` ran the body at SCOPE EXIT on
        // the compiled backends (after `mid`) and not at all in the
        // interpreter. Both the count and the position are pinned.
        (
            "result-unit",
            "fn main() { let r: Result[E, i64] = Ok(E.B); println(\"mid\"); }\n",
            "drop E\nmid\n",
        ),
        (
            "result-payload",
            "fn main() { let r: Result[E, i64] = Ok(E.A(R { id: 5 }));\n\
                 \x20            println(\"mid\"); }\n",
            "drop E\ndrop R5\nmid\n",
        ),
        (
            "result-err-position",
            "fn main() { let r: Result[i64, E] = Err(E.B); println(\"mid\"); }\n",
            "drop E\nmid\n",
        ),
        // -54's predicate at this position: no own `Drop`, Drop-bearing
        // payload. Only the payload's body runs.
        (
            "payload-only-option",
            "fn main() { let o: Option[H] = Some(H.A(R { id: 6 }));\n\
                 \x20            println(\"mid\"); }\n",
            "drop R6\nmid\n",
        ),
        (
            "payload-only-result",
            "fn main() { let r: Result[H, i64] = Ok(H.A(R { id: 7 }));\n\
                 \x20            println(\"mid\"); }\n",
            "drop R7\nmid\n",
        ),
        // BOUNDARY — an enum with neither an own body nor a Drop-bearing
        // payload runs nothing, so this is not "any enum payload fires".
        (
            "no-drop-option",
            "fn main() { let o: Option[J] = Some(J.A(3)); println(\"mid\"); }\n",
            "mid\n",
        ),
        // BOUNDARY — `None` has no live payload; the tag guard must hold.
        (
            "none-runs-nothing",
            "fn main() { let o: Option[E] = None; println(\"mid\"); }\n",
            "mid\n",
        ),
    ] {
        assert_eq!(
            run_program(&format!("{H}{body}")).as_deref(),
            Some(want),
            "{label}"
        );
    }
}

/// B-2026-08-29-10 — a METHOD's owned `Option[T]` / value-enum argument
/// runs its payload's `Drop` body exactly as its FREE-FUNCTION twin does:
/// once, in the caller, after the callee's scope exit.
///
/// The row reported the `Option` spelling missing the body under codegen
/// (`v=7` where `drop 7` / `v=7` is due) and attributed it to the caller
/// arming the payload-bodies walk only for a tracked VALUE enum. That is
/// not where the gap was — the walk IS armed for an `Option` binding, and a
/// frame trace shows it armed and due at the call statement. What killed it
/// is that the method path zeroed the argument's source slot in between, so
/// the walker read an already-cleared tag. The free-fn path does not,
/// because B-2026-08-06-31 carved out a binding whose box carries a user
/// STRUCT interior — and that carve-out landed on `call_dispatch.rs` only.
/// One missing guard, the same one-path-of-two shape this family keeps
/// producing.
///
/// `*-shadowed-name` is the row that matters most here, and it is not a
/// stylistic variant. The interpreter half of this fix removes a leak of
/// the callee frame's moved-out marks, which until now disarmed a CALLER
/// binding that happened to share the callee's parameter name. Two defects
/// cancelled whenever the two names matched, so every probe that spelled
/// both `b` — including this row's own original repro — measured correct
/// behaviour. Each shape therefore appears twice, once with the caller's
/// binding named `carg` and once named `b`, and the two must agree: a
/// rename is not a semantic change.
///
/// `*-returns-payload` pins B-2026-08-29-9's boundary from the other side.
/// When the arm hands the payload back, the caller's result binding owns it
/// and exactly one body is due. That was correct at HEAD only in the
/// same-name spelling (by accident, via the leak); with the caller's
/// binding renamed it ran the body TWICE. It is now name-independent,
/// through the method peer of `record_passthrough_arg_moves`.
///
/// `mid9` / `dR9` is a callee LOCAL, present so the assertions pin the
/// PLACE and not just the count: a body fired at the arm lands before
/// `mid9`, one fired at callee scope exit between `mid9` and `dR9`, and the
/// caller-side fire this rule wants lands after `dR9`. Without it the arm
/// and caller placements are indistinguishable, which is exactly how
/// 57bfb26 came to move the interpreter's fire into the callee without
/// noticing.
#[test]
fn e2e_method_owned_optres_arg_runs_its_payload_body_like_a_free_fn() {
    const H: &str = "struct R { id: i64, tag: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             enum E { A(R), B }\n\
             impl Drop for E { fn drop(mut ref self) { println(\"dE\") } }\n\
             struct T { n: i64 }\n\
             impl T {\n\
             \x20   fn opt_bind(ref self, b: Option[R]) -> i64 {\n\
             \x20       let mut out: i64 = 0;\n\
             \x20       match b { Some(r) => { out = r.id; } None => { out = 0; } }\n\
             \x20       let loc = R { id: 9, tag: f\"t9\" }; println(f\"mid{loc.id}\"); return out; }\n\
             \x20   fn enum_bind(ref self, b: E) -> i64 {\n\
             \x20       let mut out: i64 = 0;\n\
             \x20       match b { E.A(r) => { out = r.id; } E.B => { out = 0; } }\n\
             \x20       let loc = R { id: 9, tag: f\"t9\" }; println(f\"mid{loc.id}\"); return out; }\n\
             \x20   fn opt_none(ref self, b: Option[R]) -> i64 { println(\"mid\"); return 1i64 }\n\
             \x20   fn enum_none(ref self, b: E) -> i64 { println(\"mid\"); return 1i64 }\n\
             \x20   fn opt_ret(ref self, b: Option[R]) -> R {\n\
             \x20       match b { Some(r) => { return r } None => { return R { id: 0, tag: f\"t0\" } } } }\n\
             \x20   #[allow(partial_move_of_drop_enum)]\nfn enum_ret(ref self, b: E) -> R {\n\
             \x20       match b { E.A(r) => { return r } E.B => { return R { id: 0, tag: f\"t0\" } } } } }\n\
             fn f_opt_bind(b: Option[R]) -> i64 {\n\
             \x20   let mut out: i64 = 0;\n\
             \x20   match b { Some(r) => { out = r.id; } None => { out = 0; } }\n\
             \x20   let loc = R { id: 9, tag: f\"t9\" }; println(f\"mid{loc.id}\"); return out; }\n\
             fn f_enum_bind(b: E) -> i64 {\n\
             \x20   let mut out: i64 = 0;\n\
             \x20   match b { E.A(r) => { out = r.id; } E.B => { out = 0; } }\n\
             \x20   let loc = R { id: 9, tag: f\"t9\" }; println(f\"mid{loc.id}\"); return out; }\n\
             fn f_opt_ret(b: Option[R]) -> R {\n\
             \x20   match b { Some(r) => { return r } None => { return R { id: 0, tag: f\"t0\" } } } }\n";
    for (label, body, want) in [
        // THE ROW. `dR8` after `dR9` is the caller-side placement; before
        // this fix codegen printed no `dR8` at all.
        (
            "method-option-bindout",
            "let t = T { n: 1 }; let carg: Option[R] = Some(R { id: 8, tag: f\"t8\" });\n\
                 \x20 let v: i64 = t.opt_bind(carg); println(f\"v{v}\");\n",
            "mid9\ndR9\ndR8\nv8\npost\n",
        ),
        // The ORACLE for the row above — identical output, free-fn spelling.
        (
            "free-option-bindout",
            "let carg: Option[R] = Some(R { id: 8, tag: f\"t8\" });\n\
                 \x20 let v: i64 = f_opt_bind(carg); println(f\"v{v}\");\n",
            "mid9\ndR9\ndR8\nv8\npost\n",
        ),
        (
            "method-enum-bindout",
            "let t = T { n: 1 }; let carg: E = E.A(R { id: 8, tag: f\"t8\" });\n\
                 \x20 let v: i64 = t.enum_bind(carg); println(f\"v{v}\");\n",
            "mid9\ndR9\ndE\ndR8\nv8\npost\n",
        ),
        (
            "free-enum-bindout",
            "let carg: E = E.A(R { id: 8, tag: f\"t8\" });\n\
                 \x20 let v: i64 = f_enum_bind(carg); println(f\"v{v}\");\n",
            "mid9\ndR9\ndE\ndR8\nv8\npost\n",
        ),
        // Never matched at all: the argument simply dies. Codegen missed
        // the `Option` body here too, which the row did not record.
        (
            "method-option-never-matched",
            "let t = T { n: 1 }; let carg: Option[R] = Some(R { id: 8, tag: f\"t8\" });\n\
                 \x20 let v: i64 = t.opt_none(carg); println(f\"v{v}\");\n",
            "mid\ndR8\nv1\npost\n",
        ),
        (
            "method-enum-never-matched",
            "let t = T { n: 1 }; let carg: E = E.A(R { id: 8, tag: f\"t8\" });\n\
                 \x20 let v: i64 = t.enum_none(carg); println(f\"v{v}\");\n",
            "mid\ndE\ndR8\nv1\npost\n",
        ),
        // Same shapes, caller's binding spelled as the callee's parameter.
        // Must be byte-identical to the `carg` rows above.
        (
            "method-option-bindout-shadowed-name",
            "let t = T { n: 1 }; let b: Option[R] = Some(R { id: 8, tag: f\"t8\" });\n\
                 \x20 let v: i64 = t.opt_bind(b); println(f\"v{v}\");\n",
            "mid9\ndR9\ndR8\nv8\npost\n",
        ),
        (
            "method-enum-bindout-shadowed-name",
            "let t = T { n: 1 }; let b: E = E.A(R { id: 8, tag: f\"t8\" });\n\
                 \x20 let v: i64 = t.enum_bind(b); println(f\"v{v}\");\n",
            "mid9\ndR9\ndE\ndR8\nv8\npost\n",
        ),
        // B-2026-08-29-9's boundary: the arm hands the payload back, so the
        // caller's RESULT binding owns it and one body is due.
        (
            "method-option-returns-payload",
            "let t = T { n: 1 }; let carg: Option[R] = Some(R { id: 8, tag: f\"t8\" });\n\
                 \x20 let v: R = t.opt_ret(carg); println(f\"v{v.id}\");\n",
            "v8\ndR8\npost\n",
        ),
        (
            "free-option-returns-payload",
            "let carg: Option[R] = Some(R { id: 8, tag: f\"t8\" });\n\
                 \x20 let v: R = f_opt_ret(carg); println(f\"v{v.id}\");\n",
            "v8\ndR8\npost\n",
        ),
        (
            "method-enum-returns-payload",
            "let t = T { n: 1 }; let carg: E = E.A(R { id: 8, tag: f\"t8\" });\n\
                 \x20 let v: R = t.enum_ret(carg); println(f\"v{v.id}\");\n",
            "dE\nv8\ndR8\npost\n",
        ),
        (
            "method-option-returns-payload-shadowed-name",
            "let t = T { n: 1 }; let b: Option[R] = Some(R { id: 8, tag: f\"t8\" });\n\
                 \x20 let v: R = t.opt_ret(b); println(f\"v{v.id}\");\n",
            "v8\ndR8\npost\n",
        ),
    ] {
        let src = format!("{H}fn main() {{ {body} println(\"post\") }}\n");
        assert_eq!(run_program(&src).as_deref(), Some(want), "{label}");
    }
}

/// B-2026-09-03-6, OUTPUT half — a by-value `Result[Option[H], E]` param
/// prints the same thing on every surface. The memory half is
/// `memory_sanitizer::asan_nested_optres_by_value_param_owns_its_own_payload`,
/// which is where the double free itself is pinned; this asserts the
/// rendering the AOT binary produced all along, so a fix that stops the
/// double free by dropping the payload instead of copying it would be
/// caught here rather than looking clean under ASAN.
///
/// `option-of-result` is the OTHER NESTING ORDER, and it is here rather than
/// in the ASAN fixture on purpose: its output was and is correct, but it
/// leaks 45 B in 3 allocations for B-2026-09-02-22's reason (an `Option`
/// param's payload is gated at 3 words and a `Result` is 6, so the param is
/// never admitted to the entry-copy convention). Pinning the leak there
/// would fail that fixture for an unrelated open defect; pinning the OUTPUT
/// here costs nothing and still guards the shape.
#[test]
fn e2e_nested_optres_by_value_param_renders_on_every_surface() {
    for (label, src, want) in [
        (
            "result-of-option-string",
            r#"
fn show(x: Result[Option[String], String]) { println(f"{x}"); }
fn main() {
    let s = f"payloadpayload0";
    show(Ok(Some(s)));
    println("done");
}
"#,
            "Ok(Some(payloadpayload0))\ndone\n",
        ),
        (
            "result-of-option-vec",
            r#"
fn show(x: Result[Option[Vec[i64]], String]) { println(f"{x}"); }
fn main() {
    let v: Vec[i64] = [1, 2, 3];
    show(Ok(Some(v)));
    println("done");
}
"#,
            "Ok(Some([1, 2, 3]))\ndone\n",
        ),
        (
            "option-of-result",
            r#"
fn show(x: Option[Result[String, String]]) { println(f"{x}"); }
fn main() {
    let s = f"payloadpayload0";
    show(Some(Ok(s)));
    println("done");
}
"#,
            "Some(Ok(payloadpayload0))\ndone\n",
        ),
        (
            "err-half-live",
            r#"
fn show(x: Result[Option[String], String]) { println(f"{x}"); }
fn main() {
    let e = f"errerrerrerrerr0";
    show(Err(e));
    println("done");
}
"#,
            "Err(errerrerrerrerr0)\ndone\n",
        ),
        (
            "none-payload",
            r#"
fn show(x: Result[Option[String], String]) { println(f"{x}"); }
fn main() {
    show(Ok(None));
    println("done");
}
"#,
            "Ok(None)\ndone\n",
        ),
    ] {
        assert_eq!(run_program(src).as_deref(), Some(want), "{label}");
    }
}

/// B-2026-08-01-27 — compiled-backend pin for the let-move alias fix:
/// the interpreter moved to MATCH these (already-correct) behaviors, so
/// this twin guards the target semantics from drifting. Same program as
/// `tests/interpreter.rs`'s `test_let_move_source_frozen`.
/// B-2026-08-11-11 — `m.get(k).unwrap_or(d)` over a HEAP value.
///
/// A borrow-returning accessor hands back an `Option` whose payload words
/// ALIAS the container's stored value. `unwrap_or` returned that as an
/// owned result, so the caller's free and the container's stored-value drop
/// hit one buffer: `free(): double free detected in tcache 2`, with no move
/// anywhere in the program.
///
/// The `match` spelling of the same read was correct throughout (case 4),
/// because the arm path already classifies this receiver as a borrow and
/// deep-clones an escaping payload. The fix reuses that path's own shape
/// gate, so the two agree on which receivers alias and how deep to clone.
///
/// Cases 5 and 6 are the controls that keep the clone off everything else:
/// a scalar value has nothing to alias, and an ABSENT key returns the
/// caller's own default, which must not be touched.
#[test]
fn test_e2e_borrow_accessor_unwrap_or_clones_heap_payload() {
    // 1. The filed reproduction — `Map[String, String]`.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let mut m: Map[String, String] = Map.new();\n\
                     m.insert(f\"k\", f\"alpha\");\n\
                     println(m.get(f\"k\").unwrap_or(f\"-\"));\n\
                 }"
        )
        .as_deref(),
        Some("alpha\n")
    );
    // 2. `Vec[String].get(i)` — the same accessor family, and broken the
    // same way, so the fix must not be Map-specific.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let mut vv: Vec[String] = Vec.new();\n\
                     vv.push(f\"alpha\");\n\
                     println(vv.get(0).unwrap_or(f\"-\"));\n\
                 }"
        )
        .as_deref(),
        Some("alpha\n")
    );
    // 3. A nested heap value — the clone must go element-deep, not just
    // duplicate the outer buffer.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let mut m: Map[String, Vec[String]] = Map.new();\n\
                     let mut v: Vec[String] = Vec.new();\n\
                     v.push(f\"alpha\");\n\
                     m.insert(f\"k\", v);\n\
                     let got = m.get(f\"k\").unwrap_or(Vec.new());\n\
                     println(got[0]);\n\
                 }"
        )
        .as_deref(),
        Some("alpha\n")
    );
    // 4. CONTROL — the `match` spelling, correct before and after.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let mut m: Map[String, String] = Map.new();\n\
                     m.insert(f\"k\", f\"alpha\");\n\
                     match m.get(f\"k\") { Some(v) => println(v), None => println(f\"-\") }\n\
                 }"
        )
        .as_deref(),
        Some("alpha\n")
    );
    // 5. CONTROL — a scalar value has no buffer to alias.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let mut m: Map[String, i64] = Map.new();\n\
                     m.insert(f\"k\", 7i64);\n\
                     println(m.get(f\"k\").unwrap_or(0i64));\n\
                 }"
        )
        .as_deref(),
        Some("7\n")
    );
    // 6. CONTROL — ABSENT key, so the caller's own default is returned and
    // must be handed back untouched.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let mut m: Map[String, String] = Map.new();\n\
                     m.insert(f\"k\", f\"alpha\");\n\
                     println(m.get(f\"zz\").unwrap_or(f\"-\"));\n\
                 }"
        )
        .as_deref(),
        Some("-\n")
    );
}

/// B-2026-08-27-23 — `.clone()` on a TUPLE and on `Option[shared T]`, the
/// two element shapes B-2026-08-26-21's rejection had no remedy for.
///
/// Both read the element THROUGH AN INDEX, which is the shape that matters:
/// the same tuple in a local already cloned correctly, and it was only the
/// index-read receiver that had no dispatch. Values are asserted, not just
/// compilation — the tuple emitter was reached before this by a path that
/// double-freed a `Vec` component and silently miscompiled a nested tuple,
/// so "it built" proves nothing here.
#[test]
fn test_e2e_clone_tuple_and_option_shared_elements() {
    let Some(out) = run_program(
        r#"
shared struct N { mut v: i64 }
fn main() {
    let mut ts: Vec[(i64, String)] = Vec.new();
    ts.push((7, "payload_long_enough_to_be_heap_allocated".to_string()));
    let t = ts[0].clone();
    println(f"{t.0} {t.1.len()} {ts[0].1.len()}");

    let mut os: Vec[Option[N]] = Vec.new();
    os.push(Some(N { v: 41 }));
    let o = os[0].clone();
    match o { Some(n) => { println(n.v); } None => { println("none"); } }
    match os[0] { Some(n) => { println(n.v + 1); } None => { println("none"); } }
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "7 40 40\n41\n42\n", "got: {out:?}");
}

/// B-2026-09-14-5 (defect 1) — a COPY projection off an owned
/// `Option`/`Result` payload binding does not stand the caller down.
///
/// `optres_payload_escaping_param_variants` called every projection off the
/// payload binding an escape, so `Some(t) => return t.1` over
/// `Option[(R, i64)]` read as a partial move of the payload: the gate
/// declined, the callee has no binding to own a fresh temp, and the body
/// ran NOWHERE. `got:9 end` on all three compiled surfaces against
/// `--interp`'s `dR5 got:9 end`.
///
/// THE TWO FIXED POLICIES ARE THE ENDS OF ONE AXIS and both are wrong here.
/// `t.0` really does move `R` out and hand its body to the receiver, while
/// `t.1` and `t.0.id` carry nothing — so calling all three escapes loses two
/// bodies and calling all three reads doubles one. The policy is now asked
/// per projection with the payload's type in hand: a projection is a READ
/// exactly when its LEAF carries no `Drop` body.
///
/// CELL 3 IS THE ONE THAT KEPT THE RULE HONEST, and it has now been paid
/// out in full. `return t.0` is a real part move and must STILL decline on
/// the copy-read axis this row is about; what it must NOT do is stand the
/// whole payload down, which is what cost its sibling a body. It was
/// pinned at its measured value precisely so a later part-precision fix
/// would have to move it deliberately rather than silently, and both
/// halves of that duly happened, in two separate commits:
/// B-2026-09-13-5's fix made the INTERPRETER part-precise and moved this
/// cell's interpreter expectation to the due sequence with the AOT pin
/// untouched, and B-2026-09-17-30's fix did the same for the COMPILED
/// side. The cell now asserts ONE string for both surfaces, which is the
/// shape a fully closed defect takes here.
///
/// CELLS 6-7 ARE THE TUPLE BOUNDARY, measured rather than assumed. The
/// first cut applied the leaf policy to every payload shape and DOUBLED a
/// body for a NAMED payload — there the callee's own param machinery
/// already runs the field bodies, so the gate declining is what keeps the
/// caller from becoming a second owner. A tuple payload has no such
/// callee-side owner, which is why its cells lose the body instead. Same
/// question, opposite correct answers; cell 7 is the named-payload control
/// that caught it.
#[test]
fn e2e_copy_projection_off_an_optres_payload_keeps_the_owed_body() {
    const HDR: &str = "struct R { id: i64 }\n\
                           impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
                           struct In { id: i64 }\n\
                           impl Drop for In { fn drop(mut ref self) { println(f\"dIn{self.id}\") } }\n\
                           struct Plain { inner: In }\n";
    // Each cell carries its own interpreter expectation, because ONE of
    // them legitimately differs: the real-part-move control is the
    // interpreter's part-precise (and due) `dR6 got:5 dR5` against
    // codegen's under-run, a divergence this row does not touch. Pinning
    // both sides separately keeps that cell honest instead of dropping the
    // interpreter assertion for the whole table — and it is what let
    // B-2026-09-13-5 move one side without touching the other.
    for (label, fns, body, want, interp_want) in [
            (
                "a Copy leaf: t.1",
                "fn eat(o: Option[(R, i64)]) -> i64 { match o { Some(t) => { return t.1; } None => { return 0; } } }\n",
                "let got: i64 = eat(Option.Some((R { id: 5 }, 9)));\nprintln(f\"got:{got}\");\nprintln(\"end\");",
                "dR5\ngot:9\nend\n",
                "dR5\ngot:9\nend\n",
            ),
            (
                "a Copy leaf reached THROUGH the Drop-bearing element: t.0.id",
                "fn eat(o: Option[(R, i64)]) -> i64 { match o { Some(t) => { return t.0.id; } None => { return 0; } } }\n",
                "let got: i64 = eat(Option.Some((R { id: 5 }, 9)));\nprintln(f\"got:{got}\");\nprintln(\"end\");",
                "dR5\ngot:5\nend\n",
                "dR5\ngot:5\nend\n",
            ),
            (
                "a REAL part move — both backends now at the due sequence",
                "fn eat(o: Option[(R, R)]) -> R { match o { Some(t) => { return t.0; } None => { return R { id: 0 }; } } }\n",
                "let got: R = eat(Option.Some((R { id: 5 }, R { id: 6 })));\nprintln(f\"got:{got.id}\");\nprintln(\"end\");",
                // B-2026-09-17-30, FIXED — this AOT pin read `got:5 dR5 end`,
                // the all-or-nothing decline that ran element 1's body
                // nowhere, and B-2026-09-14-18's row named this exact cell as
                // the control a part-precision fix would have to move: "a
                // part-precision fix must move both together and the pin will
                // say so." It said so, and this is the deliberate move. The
                // two expectations are now the same string, which is the
                // point.
                "dR6\ngot:5\ndR5\nend\n",
                // B-2026-09-13-5, FIXED — the interpreter reaches the due
                // `dR6 got:5 dR5 end` here now. It used to print
                // `dR5 dR6 got:5 dR5 end`: the moved-out part's body ran at
                // the payload's death AND again at the caller's binding, one
                // more than codegen. Its fresh-temp argument walk is now
                // part-precise (it masks exactly the projected-out part and
                // keeps the siblings), so element 1's body still runs at the
                // payload's death and element 0's only at the receiver.
                //
                // THE CELL IS STILL A DIVERGENCE, which is why the two
                // expectations stay separate: codegen runs NO part's body
                // when one escapes, so it loses `dR6`. That is defect 2's
                // compiled half, now the only half left — tracked by
                // B-2026-09-17-30 for this projection spelling and by
                // B-2026-09-14-18 for the destructure one. This cell was
                // divergent BEFORE the interpreter fix too (the two strings
                // above differed then as well), so making one side correct
                // traded nothing away.
                "dR6\ngot:5\ndR5\nend\n",
            ),
            (
                "control: no projection at all was always correct",
                "fn eat(o: Option[(R, i64)]) -> i64 { match o { Some(t) => { return 0; } None => { return 0; } } }\n",
                "let got: i64 = eat(Option.Some((R { id: 5 }, 9)));\nprintln(f\"got:{got}\");\nprintln(\"end\");",
                "dR5\ngot:0\nend\n",
                "dR5\ngot:0\nend\n",
            ),
            (
                "control: a bare payload binding read",
                "fn eat(o: Option[R]) -> i64 { match o { Some(t) => { return t.id; } None => { return 0; } } }\n",
                "let got: i64 = eat(Option.Some(R { id: 5 }));\nprintln(f\"got:{got}\");\nprintln(\"end\");",
                "dR5\ngot:5\nend\n",
                "dR5\ngot:5\nend\n",
            ),
            (
                "control: a NAMED payload's scalar read — the doubling boundary",
                "fn eat(o: Option[Plain]) -> i64 { match o { Some(t) => { return t.inner.id; } None => { return 0; } } }\n",
                "let got: i64 = eat(Option.Some(Plain { inner: In { id: 5 } }));\nprintln(f\"got:{got}\");\nprintln(\"end\");",
                "dIn5\ngot:5\nend\n",
                "dIn5\ngot:5\nend\n",
            ),
        ] {
            let src = format!("{HDR}{fns}fn main() {{\n{body}\n}}\n");
            let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(&src);
            assert!(
                interp_errs.is_empty(),
                "[{label}] interp errored: {interp_errs:?}"
            );
            assert_eq!(interp_out.join(""), interp_want, "[{label}] interpreter");
            if let Some(aot) = run_program(&src) {
                assert_eq!(aot, want, "[{label}] AOT");
            }
        }
}

/// B-2026-09-13-5 — the cross-backend twin of
/// `tests/interpreter.rs`'s
/// `test_optres_arg_payload_projection_runs_each_part_body_once`, and the
/// place the ROW's own A/B claim is asserted: byte-identical source, both
/// surfaces measured, the expectations derived from the ownership rule
/// rather than from either backend.
///
/// The row: the interpreter ran a payload part's `Drop` body TWICE when an
/// arm bound a by-value `Option`/`Result` payload WHOLE and returned only
/// a PROJECTION of it (`Some(t) => return t.0`), while the three compiled
/// surfaces were correct. Its fresh-temp argument walk is now
/// part-precise: it masks exactly the projected-out parts and keeps the
/// siblings.
///
/// B-2026-09-17-30 CLOSED THE COMPILED HALF, and this fixture is where its
/// two headline cells changed. `tuple-two-droppers-first-returned` and
/// `tuple-two-droppers-second-returned` carried a second, DIVERGENT AOT
/// expectation recording that codegen ran NO part's body; both now match
/// the interpreter at the due sequence, and the pins were moved
/// deliberately rather than regenerated, which is what that row asked for.
///
/// B-2026-09-19-33 CLOSED THE NESTED HALF, and it is where three more
/// cells joined. `nested-projection`'s divergent AOT pin now matches the
/// interpreter, and `nested-projection-tuple-then-field`,
/// `-result-head` and `-consumed-in-frame` pin the rest of that channel:
/// a mixed tuple-then-struct path, the `Result` head, and the arm where
/// the part is moved into a local that dies inside the callee. The last
/// had the same defect from the other path channel and is fixed by the
/// same tree, both channels being unioned into one mask.
///
/// The row proposed BUILDING a tree-shaped tuple mask. None was needed:
/// `FieldSkipTree` is index-keyed at every level, B-2026-09-06-5 already
/// gave the tuple walker a tree-driven entry point and a nested dispatcher
/// that takes a struct OR a tuple at each hop, and `insert_tuple_skip_path`
/// already resolved a mixed path through declared types. Only the mask arm
/// and this channel's answer were still flat.
///
/// THE BOXED CHANNEL JOINED THIS FIXTURE WITH B-2026-09-19-34, which is
/// why half the cells below now carry a `boxed-projection-` label. That
/// row was the one cell this fixture could not reach: a payload too wide
/// to ride inline boxes, and the caller-side walk the cells above exercise
/// STANDS DOWN there by construction (`track_optres_arg_temp_bodies`
/// returns early for a box the callee owns), so the bodies move to the
/// CALLEE and the arm-scoped suppressor decides. It kept its walk armed
/// over a part the caller had already been handed, and the part's body ran
/// twice; the repair narrows that walk with the same `FieldSkipTree` the
/// inline channel got here, driven by a path-valued answer
/// (`binding_use::optres_arm_moved_tuple_paths`).
///
/// SEVEN OF THOSE CELLS ARE THE REPAIR — one hop, nested, tuple-then-field,
/// three hops, a `Result` head, a whole inner tuple, consumed-into-frame —
/// and TWO ARE THE BOUNDARY: a borrow-only arm and a forwarded whole
/// binding, both of which must KEEP the walk, because there the callee is
/// the only holder. THREE MORE ARE PINNED WRONG on purpose: a whole
/// binding RETURNED (B-2026-09-20-16), a rebind-then-project that doubles on
/// all four surfaces at once (B-2026-09-20-17), and two guarded `Some` arms
/// taking different elements, where the narrowing declines and every
/// surface is wrong in a different direction (B-2026-09-20-18).
///
/// AND THE NESTED SHAPE IS STILL BROKEN ON TWO CHANNELS THIS FIXTURE DOES
/// NOT COVER, measured while fixing the one above and filed rather than
/// pinned here, because each belongs to a different walk: a STRUCT-rooted
/// path whose inner hop is a tuple (`w.p.1`) runs the escaping element's
/// body twice (B-2026-09-20-6), and the NAMED-LOCAL argument spelling is
/// wrong on BOTH backends at once, differently. The third — "the BOXED
/// counterparts of every cell here" — is what B-2026-09-19-34 closed.
///
/// ONE CELL AGREES AT A WRONG ANSWER, deliberately:
/// `conditional-projection-branch-taken`. The escape scan records a
/// hand-back only at the arm's own statement level, so a `return t.0`
/// inside an `if` is not masked and element 0's body runs twice. This fix
/// brings codegen onto the interpreter's long-standing answer there,
/// trading a divergence for an agreed double; being right on both branches
/// needs a path-sensitive escape answer neither backend has
/// (B-2026-09-19-31).
///
/// THE STRUCT PAYLOAD IS THE SHARP BOUNDARY, measured rather than assumed:
/// `struct-field-returned-dropping-sibling` is the same shape with a NAMED
/// payload instead of a tuple and it is correct on all four surfaces, so
/// the compiled loss is specific to a tuple payload — for a named one the
/// callee's own param machinery owns the surviving field's body. That is
/// the same boundary B-2026-09-14-5's fix had to draw for the copy-read
/// policy, arrived at from the other side.
///
/// BODY-ONLY, so no sanitizer or ASAN ratchet leg can see any of it: a
/// `Drop` body frees nothing, and the row records `0 errors` / `0 bytes`
/// at `-O0` on every cell.
#[test]
fn e2e_optres_arg_payload_projection_runs_each_part_body_once() {
    const R: &str = "struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n";
    // (label, source tail, AOT expectation, interpreter expectation)
    for (label, prog, want, interp_want) in [
            (
                "tuple-elem-returned",
                format!(
                    "{R}fn eat(o: Option[(R, i64)]) -> R {{ match o {{ Some(t) => {{ return t.0; }} None => {{ return R {{ id: 0 }}; }} }} }}\n\
                     fn main() {{ let got = eat(Some((R {{ id: 5 }}, 9i64))); println(f\"got:{{got.id}}\"); println(\"end\") }}\n"
                ),
                "got:5\ndR5\nend\n",
                "got:5\ndR5\nend\n",
            ),
            (
                // FIXED by B-2026-09-17-30: the AOT pin below recorded the
                // compiled loss of element 1's body and now matches the
                // interpreter at the due sequence. Moved deliberately, which
                // is what that row asked of whoever fixed it.
                "tuple-two-droppers-first-returned",
                format!(
                    "{R}fn eat(o: Option[(R, R)]) -> R {{ match o {{ Some(t) => {{ return t.0; }} None => {{ return R {{ id: 0 }}; }} }} }}\n\
                     fn main() {{ let got = eat(Some((R {{ id: 5 }}, R {{ id: 6 }}))); println(f\"got:{{got.id}}\"); println(\"end\") }}\n"
                ),
                "dR6\ngot:5\ndR5\nend\n",
                "dR6\ngot:5\ndR5\nend\n",
            ),
            (
                // FIXED by B-2026-09-17-30, the mirror of the cell above — the
                // element that leaves is 1, so the mask must name 1 and not a
                // prefix. Its AOT pin moved with it.
                "tuple-two-droppers-second-returned",
                format!(
                    "{R}fn eat(o: Option[(R, R)]) -> R {{ match o {{ Some(t) => {{ return t.1; }} None => {{ return R {{ id: 0 }}; }} }} }}\n\
                     fn main() {{ let got = eat(Some((R {{ id: 5 }}, R {{ id: 6 }}))); println(f\"got:{{got.id}}\"); println(\"end\") }}\n"
                ),
                "dR5\ngot:6\ndR6\nend\n",
                "dR5\ngot:6\ndR6\nend\n",
            ),
            (
                "struct-field-returned",
                format!(
                    "{R}struct Hd {{ r: R, n: i64 }}\n\
                     fn eat(o: Option[Hd]) -> R {{ match o {{ Some(t) => {{ return t.r; }} None => {{ return R {{ id: 0 }}; }} }} }}\n\
                     fn main() {{ let got = eat(Some(Hd {{ r: R {{ id: 5 }}, n: 9i64 }})); println(f\"got:{{got.id}}\"); println(\"end\") }}\n"
                ),
                "got:5\ndR5\nend\n",
                "got:5\ndR5\nend\n",
            ),
            (
                // THE BOUNDARY CELL: a NAMED payload with a surviving
                // `Drop`-bearing sibling is correct on all four surfaces, where
                // the tuple spelling above is not.
                "struct-field-returned-dropping-sibling",
                format!(
                    "{R}struct Hd2 {{ r: R, q: R }}\n\
                     fn eat(o: Option[Hd2]) -> R {{ match o {{ Some(t) => {{ return t.r; }} None => {{ return R {{ id: 0 }}; }} }} }}\n\
                     fn main() {{ let got = eat(Some(Hd2 {{ r: R {{ id: 5 }}, q: R {{ id: 6 }} }})); println(f\"got:{{got.id}}\"); println(\"end\") }}\n"
                ),
                "dR6\ngot:5\ndR5\nend\n",
                "dR6\ngot:5\ndR5\nend\n",
            ),
            (
                "result-payload-elem-returned",
                format!(
                    "{R}fn eat(o: Result[(R, i64), i64]) -> R {{ match o {{ Ok(t) => {{ return t.0; }} Err(e) => {{ return R {{ id: 0 }}; }} }} }}\n\
                     fn main() {{ let got = eat(Ok((R {{ id: 5 }}, 9i64))); println(f\"got:{{got.id}}\"); println(\"end\") }}\n"
                ),
                "got:5\ndR5\nend\n",
                "got:5\ndR5\nend\n",
            ),
            (
                "if-let-spelling",
                format!(
                    "{R}fn eat(o: Option[(R, i64)]) -> R {{ if let Some(t) = o {{ return t.0; }} return R {{ id: 0 }}; }}\n\
                     fn main() {{ let got = eat(Some((R {{ id: 5 }}, 9i64))); println(f\"got:{{got.id}}\"); println(\"end\") }}\n"
                ),
                "got:5\ndR5\nend\n",
                "got:5\ndR5\nend\n",
            ),
            (
                // The arm TAIL, no `return`: a yield site only because the
                // `match` sits in the function's tail position.
                "arm-tail-no-return",
                format!(
                    "{R}fn eat(o: Option[(R, i64)]) -> R {{ match o {{ Some(t) => {{ t.0 }} None => {{ R {{ id: 0 }} }} }} }}\n\
                     fn main() {{ let got = eat(Some((R {{ id: 5 }}, 9i64))); println(f\"got:{{got.id}}\"); println(\"end\") }}\n"
                ),
                "got:5\ndR5\nend\n",
                "got:5\ndR5\nend\n",
            ),
            (
                // FIXED by B-2026-09-19-33: the AOT pin below recorded the
                // compiled loss of element 0's sibling and now matches the
                // interpreter at the due sequence. Moved deliberately, which
                // is what the row asked of whoever fixed it.
                //
                // The depth filter that produced the loss is gone rather than
                // widened. It declined any path longer than one hop because
                // `PayloadBodiesMask::TupleElems` is flat and reporting the
                // first hop alone would be a FALSE escape losing element 0's
                // sibling — a real trade, correctly made. What the row did not
                // know is that the tree the fix needs already existed
                // (B-2026-09-06-5's `emit_tuple_elem_user_drop_bodies_fn_tree`
                // plus `insert_tuple_skip_path`), so the mask gained a
                // `TupleTree` arm and the paths are now resolved at full depth
                // instead of being truncated or declined.
                "nested-projection",
                format!(
                    "{R}fn eat(o: Option[((R, R), i64)]) -> R {{ match o {{ Some(t) => {{ return t.0.1; }} None => {{ return R {{ id: 0 }}; }} }} }}\n\
                     fn main() {{ let got = eat(Some(((R {{ id: 5 }}, R {{ id: 6 }}), 9i64))); println(f\"got:{{got.id}}\"); println(\"end\") }}\n"
                ),
                "dR5\ngot:6\ndR6\nend\n",
                "dR5\ngot:6\ndR6\nend\n",
            ),
            (
                // B-2026-09-19-33 — the MIXED-HOP sibling of the cell above,
                // and the one that proves the fix resolves a path rather than
                // special-casing a tuple-of-tuples. The inner hop is a STRUCT
                // FIELD, so `insert_tuple_skip_path` has to cross from the
                // tuple channel into `insert_skip_path` and mask field `s` of
                // `P` one level down. It carried the identical compiled loss
                // (`got:22 dR22`) and was not in the row.
                "nested-projection-tuple-then-field",
                format!(
                    "{R}struct P {{ r: R, s: R }}\n\
                     fn eat(o: Option[(P, i64)]) -> R {{ match o {{ Some(t) => {{ return t.0.s; }} None => {{ return R {{ id: 0 }}; }} }} }}\n\
                     fn main() {{ let got = eat(Some((P {{ r: R {{ id: 21 }}, s: R {{ id: 22 }} }}, 9i64))); println(f\"got:{{got.id}}\"); println(\"end\") }}\n"
                ),
                "dR21\ngot:22\ndR22\nend\n",
                "dR21\ngot:22\ndR22\nend\n",
            ),
            (
                // B-2026-09-19-33 — the `Result` head, which shares the whole
                // mechanism and is pinned because the mask is keyed on the
                // MANGLED payload type precisely so a two-tuple-arm `Result`
                // cannot have one arm's indices applied to the other.
                "nested-projection-result-head",
                format!(
                    "{R}fn eat(o: Result[((R, R), i64), i64]) -> R {{ match o {{ Ok(t) => {{ return t.0.1; }} Err(e) => {{ return R {{ id: 0 }}; }} }} }}\n\
                     fn main() {{ let got = eat(Ok(((R {{ id: 61 }}, R {{ id: 62 }}), 9i64))); println(f\"got:{{got.id}}\"); println(\"end\") }}\n"
                ),
                "dR61\ngot:62\ndR62\nend\n",
                "dR61\ngot:62\ndR62\nend\n",
            ),
            (
                // B-2026-09-19-33 — the CONSUMED-IN-FRAME arm, which had the
                // identical defect and is fixed by the same tree because both
                // path channels are unioned into one. The part never leaves
                // the callee (`let x = t.0.1` dies at the arm's end), so the
                // caller owes element 0's sibling and owed it to nobody:
                // `mid dR32 got:32` compiled against the interpreter's
                // `mid dR32 dR31 got:32`.
                "nested-projection-consumed-in-frame",
                format!(
                    "{R}fn eat(o: Option[((R, R), i64)]) -> i64 {{ match o {{ Some(t) => {{ let x = t.0.1; println(\"mid\"); return x.id; }} None => {{ return 0i64; }} }} }}\n\
                     fn main() {{ println(f\"got:{{eat(Some(((R {{ id: 31 }}, R {{ id: 32 }}), 9i64)))}}\"); println(\"end\") }}\n"
                ),
                "mid\ndR32\ndR31\ngot:32\nend\n",
                "mid\ndR32\ndR31\ngot:32\nend\n",
            ),
            (
                "guard-whole-binding-returned",
                format!(
                    "{R}fn eat(o: Option[(R, R)]) -> (R, R) {{ match o {{ Some(t) => {{ return t; }} None => {{ return (R {{ id: 0 }}, R {{ id: 1 }}); }} }} }}\n\
                     fn main() {{ let got = eat(Some((R {{ id: 5 }}, R {{ id: 6 }}))); println(f\"got:{{got.0.id}}\"); println(\"end\") }}\n"
                ),
                "got:5\ndR5\ndR6\nend\n",
                "got:5\ndR5\ndR6\nend\n",
            ),
            (
                "guard-scalar-leaf-through-dropper",
                format!(
                    "{R}fn eat(o: Option[(R, i64)]) -> i64 {{ match o {{ Some(t) => {{ return t.0.id; }} None => {{ return 0i64; }} }} }}\n\
                     fn main() {{ let got = eat(Some((R {{ id: 5 }}, 9i64))); println(f\"got:{{got}}\"); println(\"end\") }}\n"
                ),
                "dR5\ngot:5\nend\n",
                "dR5\ngot:5\nend\n",
            ),
            (
                "guard-scalar-sibling-read",
                format!(
                    "{R}fn eat(o: Option[(R, i64)]) -> i64 {{ match o {{ Some(t) => {{ return t.1; }} None => {{ return 0i64; }} }} }}\n\
                     fn main() {{ let got = eat(Some((R {{ id: 5 }}, 9i64))); println(f\"got:{{got}}\"); println(\"end\") }}\n"
                ),
                "dR5\ngot:9\nend\n",
                "dR5\ngot:9\nend\n",
            ),
            (
                "guard-nested-destructure",
                format!(
                    "{R}fn eat(o: Option[(R, i64)]) -> R {{ match o {{ Some((a, b)) => {{ return a; }} None => {{ return R {{ id: 0 }}; }} }} }}\n\
                     fn main() {{ let got = eat(Some((R {{ id: 5 }}, 9i64))); println(f\"got:{{got.id}}\"); println(\"end\") }}\n"
                ),
                "got:5\ndR5\nend\n",
                "got:5\ndR5\nend\n",
            ),
            (
                // The one that made the scan record only at the arm's own
                // statement level: a union over branches would mask `t.r` on
                // this `k = false` run and lose `dR5` on BOTH backends.
                "guard-conditional-escape-not-taken",
                format!(
                    "{R}struct Hd3 {{ r: R, n: i64 }}\n\
                     fn eat(o: Option[Hd3], k: bool) -> R {{ match o {{ Some(t) => {{ if k {{ return t.r; }} return R {{ id: 1 }}; }} None => {{ return R {{ id: 0 }}; }} }} }}\n\
                     fn main() {{ let got = eat(Some(Hd3 {{ r: R {{ id: 5 }}, n: 9i64 }}), false); println(f\"got:{{got.id}}\"); println(\"end\") }}\n"
                ),
                "dR5\ngot:1\ndR1\nend\n",
                "dR5\ngot:1\ndR1\nend\n",
            ),
            (
                // B-2026-09-17-30 — the TUPLE spelling of the cell above, on
                // the branch that is NOT taken. Element 1 leaves at the arm's
                // own statement level, so the scan records it, element 0's
                // body stays here, and both backends now say so. Divergent
                // before this fix (the compiled side ran neither body).
                "conditional-projection-branch-not-taken",
                format!(
                    "{R}fn eat(o: Option[(R, R)], k: bool) -> R {{ match o {{ Some(t) => {{ if k {{ return t.0; }} return t.1; }} None => {{ return R {{ id: 0 }}; }} }} }}\n\
                     fn main() {{ let got = eat(Some((R {{ id: 5 }}, R {{ id: 6 }})), false); println(f\"got:{{got.id}}\"); println(\"end\") }}\n"
                ),
                "dR5\ngot:6\ndR6\nend\n",
                "dR5\ngot:6\ndR6\nend\n",
            ),
            (
                // AGREED AND WRONG, pinned as it stands rather than blessed.
                // The SAME program on the branch that IS taken: `t.0` leaves
                // through the `if`, which the statement-level rule does not
                // record, so element 0 is not masked and its body runs BOTH
                // here and at the caller's `got` -- `dR5 got:5 dR5` against the
                // due `dR6 got:5 dR5`. The interpreter has answered this way
                // since B-2026-09-13-5; this fix brings codegen onto the same
                // answer, which trades a DIVERGENCE (the compiled side ran
                // neither body) for an agreed double. Being right on both
                // branches needs a PATH-SENSITIVE escape answer, which neither
                // backend has -- B-2026-09-19-31.
                "conditional-projection-branch-taken",
                format!(
                    "{R}fn eat(o: Option[(R, R)], k: bool) -> R {{ match o {{ Some(t) => {{ if k {{ return t.0; }} return t.1; }} None => {{ return R {{ id: 0 }}; }} }} }}\n\
                     fn main() {{ let got = eat(Some((R {{ id: 5 }}, R {{ id: 6 }})), true); println(f\"got:{{got.id}}\"); println(\"end\") }}\n"
                ),
                "dR5\ngot:5\ndR5\nend\n",
                "dR5\ngot:5\ndR5\nend\n",
            ),
            (
                // FIXED by B-2026-09-19-34; the AOT pin below recorded the double and
                // now matches the interpreter at the due sequence. Moved deliberately,
                // which is what that row asked of whoever fixed it.
                //
                // Labelled `boxed-payload-projection-doubles-the-escaping-part` until
                // that fix, which is the name the row and B-2026-09-19-33's prose cite.
                // Renamed rather than kept, because a label asserting a double beside a
                // correct expectation reads as a stale pin to the next person.
                //
                // A payload too wide to ride inline heap-BOXES and takes the
                // callee-owned bodies channel, where the arm's walk stayed armed over a
                // part the caller had already been handed.
                "boxed-projection-runs-the-escaping-part-once",
                format!(
                    "{R}struct H {{ id: i64, s: String }}\n\
                     impl Drop for H {{ fn drop(mut ref self) {{ println(f\"dH{{self.id}}\") }} }}\n\
                     fn eat(o: Option[(H, H)]) -> H {{ match o {{ Some(t) => {{ return t.0; }} None => {{ return H {{ id: 0, s: \"zzzzzzzzzzzz\" }}; }} }} }}\n\
                     fn main() {{ let got = eat(Some((H {{ id: 5, s: \"aaaaaaaaaaaa\" }}, H {{ id: 6, s: \"bbbbbbbbbbbb\" }}))); println(f\"got:{{got.id}}\"); println(\"end\") }}\n"
                ),
                "dH6\ngot:5\ndH5\nend\n",
                "dH6\ngot:5\ndH5\nend\n",
            ),
            (
                // B-2026-09-19-34, NESTED path on the boxed channel. `t.1.0` is the
                // cell that rules out the index-valued remedy the row proposed: masking
                // its FIRST HOP would suppress the callee's walk of the whole of `t.1`
                // while `t.1.1` (dH7) is still owed here, turning the double into a
                // loss. Pre-fix AOT was `dH5 dH6 dH7 got:6 dH6 end`.
                "boxed-projection-nested-inner",
                format!(
                    "{R}struct H {{ id: i64, s: String }}\n\
                     impl Drop for H {{ fn drop(mut ref self) {{ println(f\"dH{{self.id}}\") }} }}\n\
                     fn eat(o: Option[(H, (H, H))]) -> H {{ match o {{ Some(t) => {{ return t.1.0; }} None => {{ return H {{ id: 0, s: \"zzzzzzzzzzzz\" }}; }} }} }}\n\
                     fn main() {{ let got = eat(Some((H {{ id: 5, s: \"aaaaaaaaaaaa\" }}, (H {{ id: 6, s: \"bbbbbbbbbbbb\" }}, H {{ id: 7, s: \"cccccccccccc\" }})))); println(f\"got:{{got.id}}\"); println(\"end\") }}\n"
                ),
                "dH5\ndH7\ngot:6\ndH6\nend\n",
                "dH5\ndH7\ngot:6\ndH6\nend\n",
            ),
            (
                // B-2026-09-19-34, a MIXED path whose second hop is a struct FIELD.
                // Here because the two hops go through different resolvers -- the tuple
                // level through `insert_tuple_skip_path`, the field level through
                // `insert_skip_path` -- so a repair handling only tuple-to-tuple would
                // pass the cell above and fail this one. Pre-fix AOT was
                // `dH5 dH7 dH6 got:6 dH6 end`, and its extra body lands in a DIFFERENT
                // position than the nested cell's, so the two are not one shape with a
                // rename.
                "boxed-projection-tuple-then-field",
                format!(
                    "{R}struct H {{ id: i64, s: String }}\n\
                     impl Drop for H {{ fn drop(mut ref self) {{ println(f\"dH{{self.id}}\") }} }}\n\
                     struct G {{ a: H, b: H }}\n\
                     fn eat(o: Option[(H, G)]) -> H {{ match o {{ Some(t) => {{ return t.1.a; }} None => {{ return H {{ id: 0, s: \"zzzzzzzzzzzz\" }}; }} }} }}\n\
                     fn main() {{ let got = eat(Some((H {{ id: 5, s: \"aaaaaaaaaaaa\" }}, G {{ a: H {{ id: 6, s: \"bbbbbbbbbbbb\" }}, b: H {{ id: 7, s: \"cccccccccccc\" }} }}))); println(f\"got:{{got.id}}\"); println(\"end\") }}\n"
                ),
                "dH5\ndH7\ngot:6\ndH6\nend\n",
                "dH5\ndH7\ngot:6\ndH6\nend\n",
            ),
            (
                // B-2026-09-19-34, THREE hops, and its elements are one-word `R`s: the
                // payload boxes on WIDTH (four words against the seeded `Option`'s
                // three-word inline area), not on any element owning heap. Depth is not
                // the discriminator here, width is -- which is why this cell doubled
                // while the three-word `tuple-two-droppers-first-returned` sibling is
                // correct. Pre-fix AOT was `dR5 dR6 got:6 dR6 end`.
                "boxed-projection-three-hops",
                format!(
                    "{R}fn eat(o: Option[(((R, R), i64), i64)]) -> R {{ match o {{ Some(t) => {{ return t.0.0.1; }} None => {{ return R {{ id: 0 }}; }} }} }}\n\
                     fn main() {{ let got = eat(Some((((R {{ id: 5 }}, R {{ id: 6 }}), 7), 8))); println(f\"got:{{got.id}}\"); println(\"end\") }}\n"
                ),
                "dR5\ngot:6\ndR6\nend\n",
                "dR5\ngot:6\ndR6\nend\n",
            ),
            (
                // B-2026-09-19-34, a `Result` HEAD, which the row listed as NOT
                // MEASURED. Not a formality: the first draft of the fix repaired every
                // `Option` cell and left this one doubled, because it read the `Err(e)`
                // arm as an arm that binds a whole payload and agrees about nothing. An
                // `Err` binding constrains nothing about the `Ok` payload's parts, so
                // the narrowing is variant-aware. Pre-fix AOT was
                // `dH5 dH6 got:5 dH5 end`.
                "boxed-projection-result-head",
                format!(
                    "{R}struct H {{ id: i64, s: String }}\n\
                     impl Drop for H {{ fn drop(mut ref self) {{ println(f\"dH{{self.id}}\") }} }}\n\
                     fn eat(o: Result[(H, H), i64]) -> H {{ match o {{ Ok(t) => {{ return t.0; }} Err(e) => {{ return H {{ id: 0, s: \"zzzzzzzzzzzz\" }}; }} }} }}\n\
                     fn main() {{ let got = eat(Ok((H {{ id: 5, s: \"aaaaaaaaaaaa\" }}, H {{ id: 6, s: \"bbbbbbbbbbbb\" }}))); println(f\"got:{{got.id}}\"); println(\"end\") }}\n"
                ),
                "dH6\ngot:5\ndH5\nend\n",
                "dH6\ngot:5\ndH5\nend\n",
            ),
            (
                // B-2026-09-19-34, a whole INNER TUPLE taken out: two bodies leave in
                // one move, and pre-fix both doubled
                // (`dH5 dH6 dH7 got:6 dH6 dH7 end`). The mask lands on element 1 WHOLE
                // here, where the nested cell needs it one level deeper on the same
                // element -- the two directions a flat index answer cannot tell apart.
                "boxed-projection-whole-inner-tuple",
                format!(
                    "{R}struct H {{ id: i64, s: String }}\n\
                     impl Drop for H {{ fn drop(mut ref self) {{ println(f\"dH{{self.id}}\") }} }}\n\
                     fn eat(o: Option[(H, (H, H))]) -> (H, H) {{ match o {{ Some(t) => {{ return t.1; }} None => {{ return (H {{ id: 0, s: \"zzzzzzzzzzzz\" }}, H {{ id: 1, s: \"zzzzzzzzzzzz\" }}); }} }} }}\n\
                     fn main() {{ let got = eat(Some((H {{ id: 5, s: \"aaaaaaaaaaaa\" }}, (H {{ id: 6, s: \"bbbbbbbbbbbb\" }}, H {{ id: 7, s: \"cccccccccccc\" }})))); println(f\"got:{{got.0.id}}\"); println(\"end\") }}\n"
                ),
                "dH5\ngot:6\ndH6\ndH7\nend\n",
                "dH5\ngot:6\ndH6\ndH7\nend\n",
            ),
            (
                // B-2026-09-19-34, the CONSUMED-INTO-FRAME spelling at the boxed
                // width: the arm moves two nested parts into locals of its own and
                // returns one. Repaired by the same narrowing -- pre-fix AOT was
                // `n:7 dH7 dH5 dH6 dH7 got:6 dH6 end`, doubling BOTH inner bodies.
                //
                // THE INTERPRETER IS STILL WRONG HERE and is pinned as such
                // (B-2026-09-20-19): it prints an extra `dH6` for the part moved into
                // the local. The two expectations differ on purpose and the AOT one is
                // the correct sequence, which is the reverse of the usual reading in
                // this test.
                "boxed-projection-consumed-into-frame-nested",
                format!(
                    "{R}struct H {{ id: i64, s: String }}\n\
                     impl Drop for H {{ fn drop(mut ref self) {{ println(f\"dH{{self.id}}\") }} }}\n\
                     fn eat(o: Option[(H, (H, H))]) -> H {{ match o {{ Some(t) => {{ let a = t.1.0; let b = t.1.1; println(f\"n:{{b.id}}\"); return a; }} None => {{ return H {{ id: 0, s: \"zzzzzzzzzzzz\" }}; }} }} }}\n\
                     fn main() {{ let got = eat(Some((H {{ id: 5, s: \"aaaaaaaaaaaa\" }}, (H {{ id: 6, s: \"bbbbbbbbbbbb\" }}, H {{ id: 7, s: \"cccccccccccc\" }})))); println(f\"got:{{got.id}}\"); println(\"end\") }}\n"
                ),
                "n:7\ndH7\ndH5\ngot:6\ndH6\nend\n",
                "n:7\ndH7\ndH5\ndH6\ngot:6\ndH6\nend\n",
            ),
            (
                // CONTROL for B-2026-09-19-34, pinning the boundary the narrowing must
                // not cross. A BORROW-ONLY arm takes no part out, so the callee-owned
                // walk is the only holder of both bodies and standing it down would run
                // them nowhere -- the measurement B-2026-09-10-9 made when it added the
                // early return this fix narrows. Correct on all four surfaces before and
                // after.
                "boxed-projection-borrow-only-keeps-the-walk",
                format!(
                    "{R}struct H {{ id: i64, s: String }}\n\
                     impl Drop for H {{ fn drop(mut ref self) {{ println(f\"dH{{self.id}}\") }} }}\n\
                     fn eat(o: Option[(H, H)]) -> i64 {{ match o {{ Some(t) => {{ return t.0.id + t.1.id; }} None => {{ return 0; }} }} }}\n\
                     fn main() {{ let got = eat(Some((H {{ id: 5, s: \"aaaaaaaaaaaa\" }}, H {{ id: 6, s: \"bbbbbbbbbbbb\" }}))); println(f\"got:{{got}}\"); println(\"end\") }}\n"
                ),
                "dH5\ndH6\ngot:11\nend\n",
                "dH5\ndH6\ngot:11\nend\n",
            ),
            (
                // CONTROL, and the second half of that boundary: the same whole
                // binding FORWARDED into another call is correct on all four surfaces.
                // The callee-owned walk is the only holder of both bodies here, so the
                // disarm that would repair `whole-binding-returned` below loses both in
                // this one -- the two spellings are one `takes_payload` answer apart.
                "boxed-projection-forwarded-whole",
                format!(
                    "{R}struct H {{ id: i64, s: String }}\n\
                     impl Drop for H {{ fn drop(mut ref self) {{ println(f\"dH{{self.id}}\") }} }}\n\
                     fn sink(t: (H, H)) -> i64 {{ return t.0.id + t.1.id }}\n\
                     fn eat(o: Option[(H, H)]) -> i64 {{ match o {{ Some(t) => {{ return sink(t); }} None => {{ return 0; }} }} }}\n\
                     fn main() {{ let got = eat(Some((H {{ id: 5, s: \"aaaaaaaaaaaa\" }}, H {{ id: 6, s: \"bbbbbbbbbbbb\" }}))); println(f\"got:{{got}}\"); println(\"end\") }}\n"
                ),
                "dH5\ndH6\ngot:11\nend\n",
                "dH5\ndH6\ngot:11\nend\n",
            ),
            (
                // DIVERGENT AND PRE-EXISTING, not this row's -- B-2026-09-20-16. The
                // arm returns the payload WHOLE, so no projection is involved and
                // B-2026-09-19-34's narrowing declines by construction; the callee-owned
                // walk stays armed over both parts the caller has just been handed and
                // runs both bodies a second time. Measured identical before and after
                // that fix.
                //
                // THE OBVIOUS GATE IS THE WRONG ONE, which is why this is a row rather
                // than a line in that fix. `optres_arm_takes_whole_payload` is already
                // computed at the call site and is true here -- and equally true of the
                // `forwarded-whole` cell above, which B-2026-09-10-9 measured LOSING
                // both bodies when the walk is stood down.
                "boxed-projection-whole-binding-returned-doubles",
                format!(
                    "{R}struct H {{ id: i64, s: String }}\n\
                     impl Drop for H {{ fn drop(mut ref self) {{ println(f\"dH{{self.id}}\") }} }}\n\
                     fn eat(o: Option[(H, H)]) -> (H, H) {{ match o {{ Some(t) => {{ return t; }} None => {{ return (H {{ id: 0, s: \"zzzzzzzzzzzz\" }}, H {{ id: 1, s: \"zzzzzzzzzzzz\" }}); }} }} }}\n\
                     fn main() {{ let got = eat(Some((H {{ id: 5, s: \"aaaaaaaaaaaa\" }}, H {{ id: 6, s: \"bbbbbbbbbbbb\" }}))); println(f\"got:{{got.0.id}}\"); println(\"end\") }}\n"
                ),
                "dH5\ndH6\ngot:5\ndH5\ndH6\nend\n",
                "got:5\ndH5\ndH6\nend\n",
            ),
            (
                // AGREED AND WRONG, pinned as it stands -- B-2026-09-20-17. The arm
                // REBINDS the payload whole (`let u = t`) and projects out of the
                // rebinding, and element 0's body runs twice on every surface, the
                // interpreter included. Hand-derived: `u.0` is moved out to the caller
                // and `u.1` dies here, so the due sequence is `dH6 got:5 dH5 end` and
                // all four print `dH5 dH6 got:5 dH5 end`.
                //
                // NO A/B CAN SEE THIS CELL, which is why it is pinned rather than left
                // to a sweep: the two backends agree, and a `Drop` body frees nothing,
                // so no sanitizer leg sees it either. Found by computing the due
                // sequence from the ownership rule and comparing against that instead of
                // against the interpreter.
                "boxed-projection-rebind-then-project-doubles",
                format!(
                    "{R}struct H {{ id: i64, s: String }}\n\
                     impl Drop for H {{ fn drop(mut ref self) {{ println(f\"dH{{self.id}}\") }} }}\n\
                     fn eat(o: Option[(H, H)]) -> H {{ match o {{ Some(t) => {{ let u = t; return u.0; }} None => {{ return H {{ id: 0, s: \"zzzzzzzzzzzz\" }}; }} }} }}\n\
                     fn main() {{ let got = eat(Some((H {{ id: 5, s: \"aaaaaaaaaaaa\" }}, H {{ id: 6, s: \"bbbbbbbbbbbb\" }}))); println(f\"got:{{got.id}}\"); println(\"end\") }}\n"
                ),
                "dH5\ndH6\ngot:5\ndH5\nend\n",
                "dH5\ndH6\ngot:5\ndH5\nend\n",
            ),
            (
                // DIVERGENT AND PRE-EXISTING -- B-2026-09-20-18. TWO `Some` arms over
                // one scrutinee, separated by a guard, each taking a DIFFERENT element.
                // B-2026-09-19-34's narrowing intersects over the arms and declines here
                // deliberately: masking element 0 is right on the guarded arm and loses
                // element 1's body on the other, which is the same false escape one arm
                // further out.
                //
                // The interpreter is wrong too and in the OPPOSITE direction -- it
                // prints `got:5 dH5 end`, losing element 1's body entirely, where the
                // compiled surfaces double element 0's. All four are wrong, differently,
                // so neither side is an oracle for the other here.
                "boxed-projection-two-some-arms-declines",
                format!(
                    "{R}struct H {{ id: i64, s: String }}\n\
                     impl Drop for H {{ fn drop(mut ref self) {{ println(f\"dH{{self.id}}\") }} }}\n\
                     fn eat(o: Option[(H, H)], k: bool) -> H {{ match o {{ Some(t) if k => {{ return t.0; }} Some(t) => {{ return t.1; }} None => {{ return H {{ id: 0, s: \"zzzzzzzzzzzz\" }}; }} }} }}\n\
                     fn main() {{ let got = eat(Some((H {{ id: 5, s: \"aaaaaaaaaaaa\" }}, H {{ id: 6, s: \"bbbbbbbbbbbb\" }})), true); println(f\"got:{{got.id}}\"); println(\"end\") }}\n"
                ),
                "dH5\ndH6\ngot:5\ndH5\nend\n",
                "got:5\ndH5\nend\n",
            ),
        ] {
            let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(&prog);
            assert!(
                interp_errs.is_empty(),
                "[{label}] interp errored: {interp_errs:?}"
            );
            assert_eq!(interp_out.join(""), interp_want, "[{label}] interpreter");
            let Some(aot) = run_program(&prog) else {
                continue;
            };
            assert_eq!(aot, want, "[{label}] AOT");
        }
}

/// B-2026-09-19-56 — A METHOD-CALL RESULT PASSED AS A BY-VALUE ARGUMENT RAN
/// THE CALLEE'S OWN PARAM `Drop` BODY ON NO SURFACE.
///
/// `sink(h.eat(Some((S { id: 11 }, S { id: 12 }))))` over `fn sink(r: S)`
/// printed `dS12 sank11` for a due `dS12 sank11 dS11` — agreed on all four
/// surfaces, which is the class no A/B against the interpreter can see.
///
/// THE ROW'S TRIGGER WAS TOO NARROW, and that is why it sat open. It was
/// filed around a method that hands out an `Option` payload part and
/// concluded "it is the METHOD-call result specifically". The `decisive`
/// cell below has no `Option`, no payload, no `match` and no container
/// anywhere — `fn mk(ref self, k: i64) -> S { return S { id: k } }` — and
/// lost the body just the same. Nothing about the payload was involved.
///
/// MECHANISM, one gap wearing two hats. Both backends answer "does this
/// argument mint a fresh owned temp whose body the caller owes" by
/// enumerating PRODUCER SHAPES — `track_inline_owned_aggregate_arg_inst`
/// (src/codegen/call_dispatch.rs) and `fresh_temp_arg_type_name`
/// (src/interpreter/eval_call.rs). Each had two of the three: a free call
/// (`ExprKind::Call` with an `Identifier` callee) and an associated call
/// (a 2-segment `Path` callee). An instance method is an
/// `ExprKind::MethodCall` and matched neither, so nothing claimed the temp
/// and the callee's own by-value param body was owed to nobody.
///
/// The associated spelling is present because B-2026-08-30-20 added it for
/// this exact symptom — no body on any backend, 76 bytes lost at `-O0` —
/// and B-2026-07-01-7 added the free one before that. This is the third
/// leg of a repair made twice already, each time one spelling short.
///
/// BOTH HALVES LAND TOGETHER. The cell was AGREED-silent, so moving either
/// backend alone would convert an agreed gap into a fresh run-vs-build
/// divergence — the arithmetic the interpreter's own arm already states.
///
/// `dup` IS A SECOND SPELLING NO ROW NAMED: an inherent
/// `fn dup(ref self) -> S` used as an argument ran ONE body for TWO
/// constructions before this. Measured against the unpatched tree rather
/// than assumed, because a new owner is exactly how this family produces a
/// double free; the count is right here in a scalar cell and in the heap
/// cell under `asan_method_call_result_argument_no_double_free`.
///
/// SEVEN CONTROLS, byte-identical before and after: the free and
/// associated producers, a literal, a named local, the result DISCARDED,
/// the result BOUND, and the `self` receiver reached from inside another
/// method. The last is the one that exercises the `SelfValue` arm of the
/// receiver lookup rather than the `Identifier` arm.
///
/// KNOWN EDGE, not closed by this: the receiver must be a plain binding or
/// `self`, because that is what either backend's type resolver can name. A
/// CHAINED or PROJECTED receiver still loses the body. That is a remaining
/// gap rather than a safe default — but naming the wrong receiver type
/// would claim a temp whose body another owner already runs, and this
/// family punishes a double harder than a loss.
#[test]
fn e2e_method_call_result_argument_runs_the_callees_param_drop_body() {
    const S: &str = "struct S { id: i64 }\n\
             impl Drop for S { fn drop(mut ref self) { println(f\"dS{self.id}\") } }\n";
    const SINK: &str = "fn sink(r: S) { println(f\"sank{r.id}\") }\n";
    const H: &str = "struct H { n: i64 }\n\
             impl H { fn mk(ref self, k: i64) -> S { return S { id: k } }\n\
                      fn viaself(ref self, k: i64) -> S { return self.mk(k) }\n\
                      fn eat(ref self, o: Option[(S, S)]) -> S { match o { Some(t) => { return t.0; } None => { return S { id: 0 }; } } } }\n";
    // (label, source, expectation -- both backends, all four surfaces)
    for (label, prog, want) in [
            (
                "the row's own cell: a method handing out an Option payload part",
                format!(
                    "{S}{H}{SINK}fn main() {{ let h = H {{ n: 1 }}; sink(h.eat(Some((S {{ id: 11 }}, S {{ id: 12 }})))); }}\n"
                ),
                "dS12\nsank11\ndS11\n",
            ),
            (
                "decisive: a method minting a fresh value -- no Option, payload or container",
                format!("{S}{H}{SINK}fn main() {{ let h = H {{ n: 1 }}; sink(h.mk(2)); }}\n"),
                "sank2\ndS2\n",
            ),
            (
                "an OWNED-self receiver",
                format!(
                    "{S}struct H {{ n: i64 }}\n\
                     impl H {{ fn omk(self, k: i64) -> S {{ return S {{ id: k }} }} }}\n\
                     {SINK}fn main() {{ let h = H {{ n: 1 }}; sink(h.omk(3)); }}\n"
                ),
                "sank3\ndS3\n",
            ),
            (
                "the receiver is `self`, reached from inside another method",
                format!("{S}{H}{SINK}fn main() {{ let h = H {{ n: 1 }}; sink(h.viaself(4)); }}\n"),
                "sank4\ndS4\n",
            ),
            (
                "dup: an inherent `fn dup(ref self) -> S` -- two constructions, two bodies",
                format!(
                    "{S}impl S {{ fn dup(ref self) -> S {{ return S {{ id: self.id }} }} }}\n\
                     {SINK}fn main() {{ let s = S {{ id: 11 }}; sink(s.dup()); println(\"mid\") }}\n"
                ),
                "sank11\ndS11\ndS11\nmid\n",
            ),
            (
                "the call is in a LOOP, so one body per iteration",
                format!(
                    "{S}{H}{SINK}fn main() {{ let h = H {{ n: 1 }}; let mut i = 0; while i < 2 {{ sink(h.mk(i + 12)); i = i + 1; }} }}\n"
                ),
                "sank12\ndS12\nsank13\ndS13\n",
            ),
            (
                "control: the ASSOCIATED producer, correct since B-2026-08-30-20",
                format!(
                    "{S}struct A {{}}\n\
                     impl A {{ fn amk(k: i64) -> S {{ return S {{ id: k }} }} }}\n\
                     {SINK}fn main() {{ sink(A.amk(5)); }}\n"
                ),
                "sank5\ndS5\n",
            ),
            (
                "control: the FREE producer, correct since B-2026-07-01-7",
                format!(
                    "{S}fn mk(k: i64) -> S {{ return S {{ id: k }} }}\n\
                     {SINK}fn main() {{ sink(mk(6)); }}\n"
                ),
                "sank6\ndS6\n",
            ),
            (
                "control: a literal argument",
                format!("{S}{SINK}fn main() {{ sink(S {{ id: 7 }}); }}\n"),
                "sank7\ndS7\n",
            ),
            (
                "control: a NAMED LOCAL, whose binding owns its own body",
                format!(
                    "{S}{H}{SINK}fn main() {{ let h = H {{ n: 1 }}; let t = h.mk(8); sink(t); }}\n"
                ),
                "sank8\ndS8\n",
            ),
            (
                "control: the method result DISCARDED rather than passed",
                format!(
                    "{S}{H}fn main() {{ let h = H {{ n: 1 }}; h.mk(9); println(\"after\") }}\n"
                ),
                "dS9\nafter\n",
            ),
            (
                "control: the method result BOUND rather than passed",
                format!(
                    "{S}{H}fn main() {{ let h = H {{ n: 1 }}; let g = h.mk(10); println(f\"got{{g.id}}\") }}\n"
                ),
                "got10\ndS10\n",
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

/// B-2026-09-19-48 — AN `Option` ARGUMENT WHOSE PAYLOAD IS A NAMED STRUCT
/// RUNS A FIELD'S `Drop` BODY TWICE, AT EVERY PROVENANCE AND AT BOTH
/// PAYLOAD WIDTHS.
///
/// `peek(a)` with `let a = Option.Some(Q { r: R { id: 5 }, s: R { id: 6 } })`
/// over `fn peek(o: Option[Q]) { match o { Some(t) => { println("mid"); } .. } }`
/// printed `mid dR6 dR5 dR6 dR5 end` on jit / `karac build` /
/// `KARAC_AUTO_PAR=0` against the interpreter's correct `mid dR6 dR5 end`.
/// Two `R`s are constructed and two bodies are owed; the compiled backends
/// ran four.
///
/// THE ROW'S OWN FRAMING IS WRONG IN TWO WAYS, both corrected by
/// measurement. The named local is not the trigger — the fresh-temp
/// spelling diverges identically — and the associated-function spelling
/// behaves the same as the method one once the callee's name is unique
/// (the duplicate-name interaction is B-2026-09-19-55, fixed separately).
/// What discriminates is the PAYLOAD SHAPE: a named struct is wrong, a
/// TUPLE payload is correct on all four surfaces, and so are the
/// destructuring and wildcard spellings. Those three are the controls
/// here.
///
/// MECHANISM — TWO OWNERS, and the question is decided in two different
/// places depending on the payload's WIDTH.
///
/// A payload that rides INLINE reaches `bind_pattern_values`, which
/// registered a field-bodies walk beside the arm binding's memory. That
/// registration exists because a consuming arm disarms the enum's own
/// payload walk in the binding's favour, leaving the binding the only
/// owner — but on a by-value PARAM scrutinee the premise is false: the
/// caller retains the payload's bodies and its walk is still armed. The
/// tuple payload is the existence proof that one owner suffices, since it
/// registers no arm-side walk at all and is correct everywhere.
///
/// A payload too WIDE to ride inline never reaches that site. It is boxed,
/// and `disarm_struct_field_bodies_at`'s `$keep` mint is what arms the
/// second owner instead — the same decision, made in the other of the two
/// places that make it. That mint's own guard (B-2026-09-19-14) explicitly
/// ruled this case out, and was correct when written: the caller used to
/// DECLINE for a named-struct payload, so the mint really was the sole
/// owner. The caller-side half of this fix is what falsifies that, which
/// is why the two edits are one change.
///
/// THE CALLER-SIDE HALF: `callee_by_value_optres_param_bodies_te` now
/// narrows for a named-struct payload instead of declining.
/// `optres_payload_consumed_elems` answers only for a tuple, so `taken`
/// came back empty and the empty-set branch declined; with the arm-side
/// walk gone that decline would leave the payload with NO owner, measured
/// as `read5` alone where `read5 dR6 dR5` is owed. An empty set is the
/// FULL walk for a struct payload, and a full-arity set is the decline.
///
/// SCORED BY SINGLE-BACKEND CONSERVATION, not by backend agreement: every
/// `R` a cell constructs owes exactly one body carrying the value it was
/// built with, counted per surface. An A/B cannot see an agreed double,
/// and this family has them.
///
/// The BOXED cells carry a `String` and a `Vec[i64]`, so they move real
/// memory; the inline cells are body-only and no sanitizer leg sees them.
#[test]
fn e2e_named_struct_optres_payload_field_body_runs_once() {
    for (label, prog, want) in [
            (
                "inline payload, NAMED LOCAL, whole binding (the row's headline)",
                "struct R { id: i64 }\n\
                 impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
                 struct Q { r: R, s: R }\n\
                 fn peek(o: Option[Q]) { match o { Some(t) => { println(\"mid\"); } None => { println(\"n\"); } } }\n\
                 fn main() { let a = Option.Some(Q { r: R { id: 5 }, s: R { id: 6 } }); peek(a); println(\"end\") }\n",
                "mid\ndR6\ndR5\nend\n",
            ),
            (
                "inline payload, FRESH TEMP, whole binding",
                "struct R { id: i64 }\n\
                 impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
                 struct Q { r: R, s: R }\n\
                 fn peek(o: Option[Q]) { match o { Some(t) => { println(\"mid\"); } None => { println(\"n\"); } } }\n\
                 fn main() { peek(Option.Some(Q { r: R { id: 5 }, s: R { id: 6 } })); println(\"end\") }\n",
                "mid\ndR6\ndR5\nend\n",
            ),
            (
                "inline payload, NAMED LOCAL, a field moved into an in-frame local",
                "struct R { id: i64 }\n\
                 impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
                 struct Q { r: R, s: R }\n\
                 fn take(o: Option[Q]) { match o { Some(t) => { let x = t.r; println(\"mid\"); } None => { println(\"n\"); } } }\n\
                 fn main() { let a = Option.Some(Q { r: R { id: 5 }, s: R { id: 6 } }); take(a); println(\"end\") }\n",
                "dR5\nmid\ndR6\nend\n",
            ),
            (
                "inline payload, FRESH TEMP, a field moved into an in-frame local",
                "struct R { id: i64 }\n\
                 impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
                 struct Q { r: R, s: R }\n\
                 fn take(o: Option[Q]) { match o { Some(t) => { let x = t.r; println(\"mid\"); } None => { println(\"n\"); } } }\n\
                 fn main() { take(Option.Some(Q { r: R { id: 5 }, s: R { id: 6 } })); println(\"end\") }\n",
                "dR5\nmid\ndR6\nend\n",
            ),
            (
                "inline payload, the SECOND field moved out",
                "struct R { id: i64 }\n\
                 impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
                 struct Q { r: R, s: R }\n\
                 fn take2(o: Option[Q]) { match o { Some(t) => { let y = t.s; println(\"mid\"); } None => { println(\"n\"); } } }\n\
                 fn main() { let a = Option.Some(Q { r: R { id: 5 }, s: R { id: 6 } }); take2(a); println(\"end\") }\n",
                "dR6\nmid\ndR5\nend\n",
            ),
            (
                "inline payload, BOTH fields moved out",
                "struct R { id: i64 }\n\
                 impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
                 struct Q { r: R, s: R }\n\
                 fn both(o: Option[Q]) { match o { Some(t) => { let x = t.r; let y = t.s; println(\"mid\"); } None => { println(\"n\"); } } }\n\
                 fn main() { let a = Option.Some(Q { r: R { id: 5 }, s: R { id: 6 } }); both(a); println(\"end\") }\n",
                "dR5\ndR6\nmid\nend\n",
            ),
            (
                "BOXED payload (String + Vec fields), FRESH TEMP, whole binding",
                "struct R { id: i64, tag: String, xs: Vec[i64] }\n\
                 impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
                 fn mkr(k: i64) -> R { return R { id: k, tag: f\"tag-number-{k}-padded-out-well-past-any-inline-capacity\", xs: [k, k, k] } }\n\
                 struct Q { r: R, s: R }\n\
                 fn peek(o: Option[Q]) { match o { Some(t) => { println(\"mid\"); } None => { println(\"n\"); } } }\n\
                 fn main() { peek(Option.Some(Q { r: mkr(5), s: mkr(6) })); println(\"end\") }\n",
                "mid\ndR6\ndR5\nend\n",
            ),
            (
                "BOXED payload, FRESH TEMP, a field moved into an in-frame local",
                "struct R { id: i64, tag: String, xs: Vec[i64] }\n\
                 impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
                 fn mkr(k: i64) -> R { return R { id: k, tag: f\"tag-number-{k}-padded-out-well-past-any-inline-capacity\", xs: [k, k, k] } }\n\
                 struct Q { r: R, s: R }\n\
                 fn take(o: Option[Q]) { match o { Some(t) => { let x = t.r; println(\"mid\"); } None => { println(\"n\"); } } }\n\
                 fn main() { take(Option.Some(Q { r: mkr(5), s: mkr(6) })); println(\"end\") }\n",
                "dR5\nmid\ndR6\nend\n",
            ),
            (
                "BOXED payload, NAMED LOCAL, a field moved into an in-frame local",
                "struct R { id: i64, tag: String, xs: Vec[i64] }\n\
                 impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
                 fn mkr(k: i64) -> R { return R { id: k, tag: f\"tag-number-{k}-padded-out-well-past-any-inline-capacity\", xs: [k, k, k] } }\n\
                 struct Q { r: R, s: R }\n\
                 fn take(o: Option[Q]) { match o { Some(t) => { let x = t.r; println(\"mid\"); } None => { println(\"n\"); } } }\n\
                 fn main() { let a = Option.Some(Q { r: mkr(5), s: mkr(6) }); take(a); println(\"end\") }\n",
                "dR5\nmid\ndR6\nend\n",
            ),
            (
                "control: the DESTRUCTURING spelling, correct throughout",
                "struct R { id: i64 }\n\
                 impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
                 struct Q { r: R, s: R }\n\
                 fn destr(o: Option[Q]) { match o { Some(Q { r, s }) => { println(\"mid\"); } None => { println(\"n\"); } } }\n\
                 fn main() { let a = Option.Some(Q { r: R { id: 5 }, s: R { id: 6 } }); destr(a); println(\"end\") }\n",
                "mid\ndR6\ndR5\nend\n",
            ),
            (
                "control: a WILDCARD payload, correct throughout",
                "struct R { id: i64 }\n\
                 impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
                 struct Q { r: R, s: R }\n\
                 fn wild(o: Option[Q]) { match o { Some(_) => { println(\"mid\"); } None => { println(\"n\"); } } }\n\
                 fn main() { let a = Option.Some(Q { r: R { id: 5 }, s: R { id: 6 } }); wild(a); println(\"end\") }\n",
                "mid\ndR6\ndR5\nend\n",
            ),
            (
                "control: the TUPLE payload, correct throughout",
                "struct R { id: i64 }\n\
                 impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
                 fn tpeek(o: Option[(R, R)]) { match o { Some(t) => { println(\"mid\"); } None => { println(\"n\"); } } }\n\
                 fn main() { let a = Option.Some((R { id: 5 }, R { id: 6 })); tpeek(a); println(\"end\") }\n",
                "mid\ndR5\ndR6\nend\n",
            ),
            (
                "control: the None arm, which owns nothing",
                "struct R { id: i64 }\n\
                 impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
                 struct Q { r: R, s: R }\n\
                 fn peek(o: Option[Q]) { match o { Some(t) => { println(\"mid\"); } None => { println(\"n\"); } } }\n\
                 fn main() { let a: Option[Q] = Option.None; peek(a); println(\"end\") }\n",
                "n\nend\n",
            ),
        ] {
            let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(prog);
            assert!(
                interp_errs.is_empty(),
                "[{label}] interp errored: {interp_errs:?}"
            );
            assert_eq!(interp_out.join(""), want, "[{label}] interpreter");
            if let Some(aot) = run_program(prog) {
                assert_eq!(aot, want, "[{label}] AOT");
            }
        }
}

/// B-2026-09-17-38 — A `Drop`-BEARING SIBLING PART KEEPS ITS BODY WHEN ITS
/// PEER IS CONSUMED BY AN IN-FRAME LOCAL.
///
/// `eat(Some((R { id: 5 }, R { id: 6 })))` over
/// `fn eat(o: Option[(R, R)]) { match o { Some(t) => { let x = t.0; println("mid"); } .. } }`
/// printed `dR5 mid end` on jit / `karac build` / `KARAC_AUTO_PAR=0`
/// against the interpreter's `dR5 mid dR6 end`. Element 1 is never moved
/// out and nothing else can own it, so exactly one body is owed at the
/// payload's death; the compiled backends ran zero.
///
/// MECHANISM: the caller's fresh-temp payload-bodies walk is gated by
/// `callee_by_value_optres_param_bodies_te`, which narrows an all-or-
/// nothing "the callee takes this payload" bit to a set of PARTS before
/// standing the walk down. Both of its part channels answer ESCAPE —
/// `optres_payload_escape_parts` and the projection sibling B-2026-09-17-30
/// added — and `let x = t.0` escapes nothing, so both returned the empty
/// set, the gate read that as "cannot narrow", and declined outright. The
/// coarse map above it had already flagged the variant as taken (a
/// projection whose leaf runs a user `Drop` is not a copy read), so the
/// decline was total: no walk at all in the IR, not a walk with element 1
/// masked out. Element 1's body was then owed to nobody, the callee's arm
/// binding being a param VIEW that owns none of it.
///
/// THE REPAIR is to ask the question the caller actually has: which parts
/// does the callee OWN, not which ones outlive it. Where the body runs is
/// the difference between the two channels and is no business of this
/// walk. `fn_consumed_param_payload_part_paths` is the predicate
/// B-2026-09-14-7 wrote for the CALLEE end of this exact call — it is what
/// stands `x`'s own slot up — so reading it here unions one answer into
/// both ends, which is what keeps them from drifting into a lost body
/// (both stand down) or a doubled one (neither does).
///
/// THE CELLS ARE THE ROW'S OWN "NOT MEASURED" LIST, and every one of them
/// was wrong: the `Result` head, the method spelling, three elements, and
/// `second` (the reversed cell — the loss is not "the last one"). `iflet`
/// and `call-arg` are two more argument spellings that reach the same
/// gate. `later-read` is the cell nobody predicted: moving the `let`'s
/// read after `mid` pushes element 0's body late by design.md § 866 and
/// STILL lost element 1, so the loss does not depend on the consumed
/// part's placement.
///
/// THE SIX CONTROLS ARE BYTE-IDENTICAL BEFORE AND AFTER, which is what
/// says the fix narrows rather than widens. `named` was closed by
/// B-2026-09-17-37 and must stay single; `nomove` moves nothing, so the
/// gate must never be reached; `scalar-sibling` has no second body to owe;
/// `both` consumes the whole payload, the full-arity case the gate already
/// declined and must keep declining; `escape` hands element 0 OUT, which
/// is a different owner's business; `struct` is the named-payload shape
/// whose callee-side machinery already owns the surviving field — the
/// reason B-2026-09-17-30 and this row both read correct there.
///
/// BODY-ONLY, so no sanitizer leg sees it: the row records `-O0` valgrind
/// at `0 bytes in 0 blocks` / `0 errors` on its own cell, before and
/// after.
///
/// The INTERPRETER twin is `tests/interpreter.rs`'s
/// `test_optres_payload_sibling_part_keeps_its_body_when_its_peer_is_consumed`,
/// asserting the same programs against the same expectation.
#[test]
fn e2e_optres_payload_sibling_part_keeps_its_body_when_its_peer_is_consumed() {
    const R: &str = "struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n";
    // (label, source, expectation -- both backends, all four surfaces)
    for (label, prog, want) in [
            (
                "the row's cell: Option head, fresh-temp argument",
                format!(
                    "{R}fn eat(o: Option[(R, R)]) {{ match o {{ Some(t) => {{ let x = t.0; println(\"mid\"); }} None => {{ println(\"n\"); }} }} }}\n\
                     fn main() {{ eat(Some((R {{ id: 5 }}, R {{ id: 6 }}))); println(\"end\") }}\n"
                ),
                "dR5\nmid\ndR6\nend\n",
            ),
            (
                "second: the consumed part is index 1, so the lost one was index 0",
                format!(
                    "{R}fn eat(o: Option[(R, R)]) {{ match o {{ Some(t) => {{ let x = t.1; println(\"mid\"); }} None => {{ println(\"n\"); }} }} }}\n\
                     fn main() {{ eat(Some((R {{ id: 5 }}, R {{ id: 6 }}))); println(\"end\") }}\n"
                ),
                "dR6\nmid\ndR5\nend\n",
            ),
            (
                "Result head",
                format!(
                    "{R}fn eat(o: Result[(R, R), i64]) {{ match o {{ Ok(t) => {{ let x = t.0; println(\"mid\"); }} Err(e) => {{ println(\"n\"); }} }} }}\n\
                     fn main() {{ eat(Result.Ok((R {{ id: 5 }}, R {{ id: 6 }}))); println(\"end\") }}\n"
                ),
                "dR5\nmid\ndR6\nend\n",
            ),
            (
                "method spelling",
                format!(
                    "{R}struct H {{ n: i64 }}\n\
                     impl H {{ fn eat(ref self, o: Option[(R, R)]) {{ match o {{ Some(t) => {{ let x = t.0; println(\"mid\"); }} None => {{ println(\"n\"); }} }} }} }}\n\
                     fn main() {{ let h = H {{ n: 1 }}; h.eat(Some((R {{ id: 5 }}, R {{ id: 6 }}))); println(\"end\") }}\n"
                ),
                "dR5\nmid\ndR6\nend\n",
            ),
            (
                "three elements: both survivors keep their bodies",
                format!(
                    "{R}fn eat(o: Option[(R, R, R)]) {{ match o {{ Some(t) => {{ let x = t.0; println(\"mid\"); }} None => {{ println(\"n\"); }} }} }}\n\
                     fn main() {{ eat(Some((R {{ id: 5 }}, R {{ id: 6 }}, R {{ id: 7 }}))); println(\"end\") }}\n"
                ),
                "dR5\nmid\ndR6\ndR7\nend\n",
            ),
            (
                "if let spelling",
                format!(
                    "{R}fn eat(o: Option[(R, R)]) {{ if let Some(t) = o {{ let x = t.0; println(\"mid\"); }} }}\n\
                     fn main() {{ eat(Some((R {{ id: 5 }}, R {{ id: 6 }}))); println(\"end\") }}\n"
                ),
                "dR5\nmid\ndR6\nend\n",
            ),
            (
                "the payload comes from a CALL rather than a literal tuple",
                format!(
                    "{R}fn mk() -> (R, R) {{ (R {{ id: 5 }}, R {{ id: 6 }}) }}\n\
                     fn eat(o: Option[(R, R)]) {{ match o {{ Some(t) => {{ let x = t.0; println(\"mid\"); }} None => {{ println(\"n\"); }} }} }}\n\
                     fn main() {{ eat(Some(mk())); println(\"end\") }}\n"
                ),
                "dR5\nmid\ndR6\nend\n",
            ),
            (
                "later-read: element 0 is owed LATE and element 1 was still lost",
                format!(
                    "{R}fn eat(o: Option[(R, R)]) {{ match o {{ Some(t) => {{ let x = t.0; println(\"mid\"); println(f\"v:{{x.id}}\"); }} None => {{ println(\"n\"); }} }} }}\n\
                     fn main() {{ eat(Some((R {{ id: 5 }}, R {{ id: 6 }}))); println(\"end\") }}\n"
                ),
                "mid\nv:5\ndR5\ndR6\nend\n",
            ),
            (
                "control: a NAMED-local argument stays single (B-2026-09-17-37)",
                format!(
                    "{R}fn eat(o: Option[(R, R)]) {{ match o {{ Some(t) => {{ let x = t.0; println(\"mid\"); }} None => {{ println(\"n\"); }} }} }}\n\
                     fn main() {{ let a = Some((R {{ id: 5 }}, R {{ id: 6 }})); eat(a); println(\"end\") }}\n"
                ),
                "dR5\nmid\ndR6\nend\n",
            ),
            (
                "control: the arm moves NOTHING, so the gate is never reached",
                format!(
                    "{R}fn eat(o: Option[(R, R)]) {{ match o {{ Some(t) => {{ println(\"mid\"); }} None => {{ println(\"n\"); }} }} }}\n\
                     fn main() {{ eat(Some((R {{ id: 5 }}, R {{ id: 6 }}))); println(\"end\") }}\n"
                ),
                "mid\ndR5\ndR6\nend\n",
            ),
            (
                "control: a SCALAR sibling owes no second body",
                format!(
                    "{R}fn eat(o: Option[(R, i64)]) {{ match o {{ Some(t) => {{ let x = t.0; println(\"mid\"); }} None => {{ println(\"n\"); }} }} }}\n\
                     fn main() {{ eat(Some((R {{ id: 5 }}, 9i64))); println(\"end\") }}\n"
                ),
                "dR5\nmid\nend\n",
            ),
            (
                "control: BOTH parts consumed is the full-arity decline",
                format!(
                    "{R}fn eat(o: Option[(R, R)]) {{ match o {{ Some(t) => {{ let x = t.0; let y = t.1; println(\"mid\"); }} None => {{ println(\"n\"); }} }} }}\n\
                     fn main() {{ eat(Some((R {{ id: 5 }}, R {{ id: 6 }}))); println(\"end\") }}\n"
                ),
                "dR5\ndR6\nmid\nend\n",
            ),
            (
                "control: `return t.0` ESCAPES, a different owner's business",
                format!(
                    "{R}fn eat(o: Option[(R, R)]) -> R {{ match o {{ Some(t) => {{ return t.0; }} None => {{ return R {{ id: 0 }}; }} }} }}\n\
                     fn main() {{ let g = eat(Some((R {{ id: 5 }}, R {{ id: 6 }}))); println(f\"got:{{g.id}}\"); println(\"end\") }}\n"
                ),
                "dR6\ngot:5\ndR5\nend\n",
            ),
            (
                "control: a NAMED-struct payload was correct throughout",
                format!(
                    "{R}struct P {{ r: R, q: R }}\n\
                     fn eat(o: Option[P]) {{ match o {{ Some(t) => {{ let x = t.r; println(\"mid\"); }} None => {{ println(\"n\"); }} }} }}\n\
                     fn main() {{ eat(Some(P {{ r: R {{ id: 5 }}, q: R {{ id: 6 }} }})); println(\"end\") }}\n"
                ),
                "dR5\nmid\ndR6\nend\n",
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

/// B-2026-09-17-37 — A NAMED-LOCAL `Option`/`Result` ARGUMENT NO LONGER
/// DOUBLES THE `Drop` BODY OF A PART THE CALLEE CONSUMES.
///
/// `let a = Some((R { id: 5 }, 9)); eat(a);` over
/// `fn eat(o: Option[(R, i64)]) { match o { Some(t) => { let x = t.0; .. } .. } }`
/// printed `dR5 mid dR5 end` on jit / `karac build` / `KARAC_AUTO_PAR=0`
/// against the interpreter's `dR5 mid end`. The callee's in-frame local
/// already runs the moved part's body at its own live-range end — the
/// FIRST `dR5`, before `mid` — and the caller's let-site payload-bodies
/// walk ran it again after the call.
///
/// `temp` is what isolated it: the FRESH-TEMP spelling of the identical
/// callee was always correct, because a temp has no let site and its walk
/// is minted at the call already masked. So the second owner is the named
/// local's registration, not the callee's transfer.
///
/// `method` and `assoc` are cells because the fix is three wirings of one
/// helper, one per argument loop — the shape B-2026-09-12-15 records for
/// this same channel, where the bodies half was wired at the free-function
/// loop only and the two method spellings stayed wrong for a week.
/// `result`, `second` and `sibling` answer three of the four questions the
/// row listed as NOT MEASURED: the `Result` head doubles identically, so
/// does a part at tuple index 1, and with two `Drop`-bearing elements it is
/// the CONSUMED one that doubled while the untouched sibling was always
/// right.
///
/// `nomove` and `mixed` are the controls with teeth. `nomove` reads the
/// payload without moving anything, so nothing may be masked; `mixed`
/// consumes one element and RETURNS the other, which is the shape that
/// rules out reusing `callee_by_value_optres_param_bodies_te` — that gate
/// declines an escaping param outright and would have left the consumed
/// element unmasked. Its `dR5 got5 dR5` is an agreed double on BOTH
/// backends (B-2026-09-13-5's alias spelling) and must stay exactly as it
/// is: masking it here would close one divergence by opening another.
///
/// NO NAMED-STRUCT CELL, deliberately. `Option[P]` for
/// `struct P { r: R, n: i64 }` does not double — it runs the body LATE
/// (`mid dR5` against `--interp`'s `dR5 mid`), an ordering divergence on a
/// different channel that is byte-identical before and after this commit.
/// That is B-2026-09-19-41, now fixed and pinned by
/// `e2e_named_struct_optres_payload_part_drops_at_its_own_live_range_end`.
///
/// ID CORRECTION: this paragraph shipped citing B-2026-09-19-39, an id
/// another session had taken minutes earlier for an unrelated generic
/// `BoxedEnumDrop` row. The id was read before the final `git fetch`,
/// which is the one thing CLAUDE.md's allocation rule says not to do; the
/// ROW itself was allocated late and came out as -41, so only this comment
/// was ever wrong.
///
/// BODY-ONLY, so no sanitizer leg sees it: every cell is `0 errors` and
/// `0 bytes definitely lost` under `-O0` valgrind, before and after.
///
/// The INTERPRETER twin is `tests/interpreter.rs`'s
/// `test_named_optres_arg_does_not_double_a_consumed_part_body`,
/// byte-identical source and expectation.
#[test]
fn e2e_named_optres_arg_does_not_double_a_consumed_part_body() {
    let Some(out) = run_program(
        r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
struct Hd { n: i64 }
impl Hd { fn eat(ref self, o: Option[(R, i64)]) { match o { Option.Some(t) => { let x = t.0; println("  mid"); } Option.None => { println("  n"); } } } }
struct Snk { n: i64 }
impl Snk { fn eat(o: Option[(R, i64)]) { match o { Option.Some(t) => { let x = t.0; println("  mid"); } Option.None => { println("  n"); } } } }
fn eat(o: Option[(R, i64)]) { match o { Option.Some(t) => { let x = t.0; println("  mid"); } Option.None => { println("  n"); } } }
fn eatr(o: Result[(R, i64), i64]) { match o { Result.Ok(t) => { let x = t.0; println("  mid"); } Result.Err(e) => { println("  n"); } } }
fn eat1(o: Option[(i64, R)]) { match o { Option.Some(t) => { let x = t.1; println("  mid"); } Option.None => { println("  n"); } } }
fn eat2(o: Option[(R, R)]) { match o { Option.Some(t) => { let x = t.0; println("  mid"); } Option.None => { println("  n"); } } }
fn peek(o: Option[(R, i64)]) { match o { Option.Some(t) => { println(f"  mid{t.1}"); } Option.None => { println("  n"); } } }
fn hands(o: Option[(R, R)]) -> R { match o { Option.Some(t) => { let y = t.1; println("  mid"); return t.0; } Option.None => { return R { id: 0 }; } } }
fn main() {
    println("named");    { let a = Option.Some((R { id: 5 }, 9)); eat(a); } println("  out")
    println("temp");     { eat(Option.Some((R { id: 5 }, 9))); } println("  out")
    println("result");   { let a: Result[(R, i64), i64] = Result.Ok((R { id: 5 }, 9)); eatr(a); } println("  out")
    println("method");   { let h = Hd { n: 1 }; let a = Option.Some((R { id: 5 }, 9)); h.eat(a); } println("  out")
    println("assoc");    { let a = Option.Some((R { id: 5 }, 9)); Snk.eat(a); } println("  out")
    println("second");   { let a = Option.Some((9, R { id: 5 })); eat1(a); } println("  out")
    println("sibling");  { let a = Option.Some((R { id: 5 }, R { id: 6 })); eat2(a); } println("  out")
    println("nomove");   { let a = Option.Some((R { id: 5 }, 9)); peek(a); } println("  out")
    println("mixed");    { let a = Option.Some((R { id: 5 }, R { id: 6 })); let r = hands(a); println(f"  got{r.id}"); } println("  out")
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "named\n  dR5\n  mid\n  out\ntemp\n  dR5\n  mid\n  out\nresult\n  dR5\n  mid\n  out\nmethod\n  dR5\n  mid\n  out\nassoc\n  dR5\n  mid\n  out\nsecond\n  dR5\n  mid\n  out\nsibling\n  dR5\n  mid\n  dR6\n  out\nnomove\n  mid9\n  dR5\n  out\nmixed\n  dR6\n  mid\n  dR5\n  got5\n  dR5\n  out\nend\n");
}

#[test]
fn e2e_discarded_optres_tuple_payload_runs_one_body() {
    assert_eq!(
        run_program(
            r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"drop {self.id}") } }
fn mk(i: i64) -> R { return R { id: i }; }
fn topagg(r: R) -> Option[(R, i64)] { return Option.Some((r, 9)); }
fn topres(r: R) -> Result[(R, i64), i64] { return Result.Ok((r, 7)); }
fn main() {
    println("o-fn");   let _ = topagg(mk(1));
    println("r-fn");   let _ = topres(mk(2));
    println("o-ctor"); let _ = Option.Some((mk(3), 9));
    println("done")
}"#
        ),
        Some("o-fn\ndrop 1\nr-fn\ndrop 2\no-ctor\ndrop 3\ndone\n".to_string()),
        "a discarded Option/Result temp with an inline tuple payload runs one body per element"
    );
}

/// B-2026-09-05-11 — a NESTED `let o = Option.Some(r)` over a by-value
/// param runs `r`'s body ONCE, after the call, on every surface. The
/// QUALIFIED spelling was the compiled half: `optres_ctor_payloads_are_all_
/// param_views` matched a bare `Some` callee only, so `o` armed a payload
/// walker over a value the caller still fires (`drop 4 / held / drop 4`)
/// while `let o = Some(r)` beside it was already right. The free and method
/// spellings, the `Result` ctor, the top-level (non-nested) `let`, a later
/// rebind of `o`, and a scalar sibling param are all cells, because the
/// interpreter half (`method_frame_caller_retains_args` flipping on a
/// scalar argument) only shows on the method rows with a literal or a
/// returned scalar. Interpreter twin:
/// `test_nested_optres_ctor_let_over_param_runs_one_body`.
#[test]
fn e2e_nested_optres_ctor_let_over_param_runs_one_body() {
    assert_eq!(
            run_program(
                r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"drop {self.id} {self.name}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}" }; }
fn fl(r: R, keep: bool) -> i64 { if keep { let o = Option.Some(r); println("held"); } return 7; }
fn flb(r: R, keep: bool) -> i64 { if keep { let o = Some(r); println("held"); } return 7; }
fn flt(r: R) -> i64 { let o = Option.Some(r); println("held"); return 7; }
fn flr(r: R, keep: bool) -> i64 { if keep { let o: Result[R, i64] = Result.Ok(r); println("held"); } return 7; }
fn flu(r: R, keep: bool) -> i64 { if keep { let o = Option.Some(r); println("held"); let p = o; println("used"); } return 7; }
fn fln(r: R, n: i64) -> i64 { let o = Option.Some(r); println("held"); return n; }
struct K { z: i64 }
impl K {
    fn l(ref self, r: R, keep: bool) -> i64 { if keep { let o = Option.Some(r); println("held"); } return 7; }
    fn lb(ref self, r: R, keep: bool) -> i64 { if keep { let o = Some(r); println("held"); } return 7; }
    fn lt(ref self, r: R) -> i64 { let o = Option.Some(r); println("held"); return 7; }
    fn lr(ref self, r: R, keep: bool) -> i64 { if keep { let o: Result[R, i64] = Result.Ok(r); println("held"); } return 7; }
    fn ln(ref self, r: R, n: i64) -> i64 { let o = Option.Some(r); println("held"); return n; }
}
fn main() {
    let k = K { z: 0 };
    println("fl-f"); let _ = fl(mk(1), false);
    println("fl-t"); let _ = fl(mk(2), true);
    println("flb-t"); let _ = flb(mk(3), true);
    println("flt"); let _ = flt(mk(4));
    println("flr-t"); let _ = flr(mk(5), true);
    println("flu-t"); let _ = flu(mk(6), true);
    println("fln"); let _ = fln(mk(7), 9);
    println("fl-n"); let a = mk(8); let _ = fl(a, true);
    println("ml-f"); let _ = k.l(mk(11), false);
    println("ml-t"); let _ = k.l(mk(12), true);
    println("mlb-t"); let _ = k.lb(mk(13), true);
    println("mlt"); let _ = k.lt(mk(14));
    println("mlr-t"); let _ = k.lr(mk(15), true);
    println("mln"); let _ = k.ln(mk(16), 9);
    println("ml-n"); let b = mk(17); let _ = k.l(b, true);
    println("ml-var"); let t = true; let _ = k.l(mk(18), t);
    println("end");
}"#
            ),
            Some("fl-f\ndrop 1 h1\nfl-t\nheld\ndrop 2 h2\nflb-t\nheld\ndrop 3 h3\nflt\nheld\ndrop 4 h4\nflr-t\nheld\ndrop 5 h5\nflu-t\nheld\nused\ndrop 6 h6\nfln\nheld\ndrop 7 h7\nfl-n\nheld\ndrop 8 h8\nml-f\ndrop 11 h11\nml-t\nheld\ndrop 12 h12\nmlb-t\nheld\ndrop 13 h13\nmlt\nheld\ndrop 14 h14\nmlr-t\nheld\ndrop 15 h15\nmln\nheld\ndrop 16 h16\nml-n\nheld\ndrop 17 h17\nml-var\nheld\ndrop 18 h18\nend\n".to_string()),
            "a nested Option/Result ctor let over a by-value param runs one body"
        );
}

/// B-2026-09-06-53 — `let x = mkUses(i)` inside a function that took `i`
/// as a parameter ran the returned value's `Drop` body NOWHERE, on every
/// backend at both opt levels, with valgrind clean (the memory side was
/// never in doubt). The call-result view classifier concluded that the
/// result was a VIEW of the argument, because the callee does store the
/// parameter into the aggregate it returns — but an `i64` owns nothing and
/// runs no body, so the body was deferred to an owner that does not exist.
/// The scalar test now guards both the classifier and the whole-alias
/// closure that carries the same conclusion one hand-off further
/// (`let x = mkUses(i); let y = keep(x)`, which the interpreter alone lost).
/// Cells: the bare scalar argument, a rebound scalar, the chained hand-off,
/// an `Option` return, `f64` and `char` parameters, an associated function,
/// and — as controls that must keep their existing single body — a callee
/// that ignores the parameter, one that consumes it through an f-string, a
/// constant argument, an arithmetic argument, a genuine owned hand-back,
/// an owned wrap, a mixed scalar-and-owned signature and a scalar local.
///
/// Twin of `tests/interpreter.rs`'s `test_scalar_argument_does_not_make_the_result_a_view`, pinned to the same string.
#[test]
fn e2e_scalar_argument_does_not_make_the_result_a_view() {
    let Some(out) = run_program(
        r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
struct H { r: R, n: i64 }
fn mkUses(i: i64) -> R { return R { id: i, name: f"h{i}" }; }
fn mkIgnores(i: i64) -> R { return R { id: 0, name: "z" }; }
fn mkName(i: i64) -> R { return R { id: 7, name: f"n{i}" }; }
fn mkOpt(i: i64) -> Option[R] { return Option.Some(R { id: i, name: f"o{i}" }); }
fn mkFlt(x: f64) -> R { return R { id: 20, name: f"f{x}" }; }
fn mkChr(c: char) -> R { return R { id: 21, name: f"c{c}" }; }
fn keep(r: R) -> R { return r; }
fn wrap(r: R) -> H { return H { r: r, n: 1 }; }
fn both(i: i64, r: R) -> R { return r; }
impl H { fn make(i: i64) -> R { return R { id: i, name: f"a{i}" }; } }

fn scalar_arg(i: i64) { let x = mkUses(i); println(f"  v={x.id}"); }
fn scalar_rebound(i: i64) { let j = i; let x = mkUses(j); println(f"  v={x.id}"); }
fn scalar_chained(i: i64) { let x = mkUses(i); let y = keep(x); println(f"  v={y.id}"); }
fn scalar_unused(i: i64) { let x = mkIgnores(i); println(f"  v={x.id}"); }
fn scalar_interpolated(i: i64) { let x = mkName(i); println(f"  v={x.id}"); }
fn scalar_constant(i: i64) { let x = mkUses(9); println(f"  v={x.id}"); }
fn scalar_arith(i: i64) { let x = mkUses(i + 0); println(f"  v={x.id}"); }
fn scalar_option(i: i64) { let o = mkOpt(i); match o { Option.Some(r) => { println(f"  v={r.id}"); } Option.None => { println("  v=none"); } } }
fn scalar_float(x: f64) { let r = mkFlt(x); println(f"  v={r.id}"); }
fn scalar_char(c: char) { let r = mkChr(c); println(f"  v={r.id}"); }
fn scalar_assoc(i: i64) { let x = H.make(i); println(f"  v={x.id}"); }
fn owned_handback(r: R) { let x = keep(r); println(f"  v={x.id}"); }
fn owned_wrapped(r: R) { let x = wrap(r); println(f"  v={x.r.id}"); }
fn mixed_args(i: i64, r: R) { let x = both(i, r); println(f"  v={x.id}"); }
fn local_scalar() { let n = 31; let x = mkUses(n); println(f"  v={x.id}"); }

fn main() {
    println("scalar_arg"); scalar_arg(1);
    println("scalar_rebound"); scalar_rebound(2);
    println("scalar_chained"); scalar_chained(3);
    println("scalar_unused"); scalar_unused(4);
    println("scalar_interpolated"); scalar_interpolated(5);
    println("scalar_constant"); scalar_constant(6);
    println("scalar_arith"); scalar_arith(8);
    println("scalar_option"); scalar_option(10);
    println("scalar_float"); scalar_float(1.5);
    println("scalar_char"); scalar_char('q');
    println("scalar_assoc"); scalar_assoc(11);
    println("owned_handback"); owned_handback(mkUses(12));
    println("owned_wrapped"); owned_wrapped(mkUses(13));
    println("mixed_args"); mixed_args(14, mkUses(15));
    println("local_scalar"); local_scalar();
    println("end");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"scalar_arg
  v=1
  dR1
scalar_rebound
  v=2
  dR2
scalar_chained
  v=3
  dR3
scalar_unused
  v=0
  dR0
scalar_interpolated
  v=7
  dR7
scalar_constant
  v=9
  dR9
scalar_arith
  v=8
  dR8
scalar_option
  v=10
  dR10
scalar_float
  v=20
  dR20
scalar_char
  v=21
  dR21
scalar_assoc
  v=11
  dR11
owned_handback
  v=12
  dR12
owned_wrapped
  v=13
  dR13
mixed_args
  v=15
  dR15
local_scalar
  v=31
  dR31
end
"#
    );
}

/// B-2026-09-06-58 — the sibling of B-2026-09-06-53 one TYPE over: a call
/// whose result WRAPS a `Drop`-free argument in a `Drop`-bearing type ran
/// the result's own body nowhere. `fn from_string(i: i64, s: String) -> R`
/// hands the parameter back inside the `R` it returns, so the classifier
/// called the result a view of `s` — right about the memory, since the `R`
/// carries that buffer, and wrong about the body, since a `String` runs no
/// user `Drop` for the `R`'s to defer to. Five cells recovered a body that
/// ran on no surface before, with the pre-fix output byte-identical across
/// backends, so no A/B or memory gate saw it.
///
/// The gate is narrow on purpose: it fires only when the parameter's type
/// carries no user `Drop` ANYWHERE inside it. A parameter that does carry
/// one keeps the view, because declining there gives the result binding a
/// full walk that runs the wrapped value's body beside the argument owner's
/// (measured `dH3 dR9 dR9` on `fn wrap_bodied(r: R) -> H`).
///
/// B-2026-09-06-63 built the own-body-without-fields split that shape
/// needed, so its `wrap_bodied_control` cell now shows `dH3` before `dR9` —
/// the wrapper's own body added WITHOUT the doubling above. The view is
/// still kept for that parameter class; what changed is that the result
/// binding additionally owns its own body. Every other cell here is
/// unchanged, which is what says the split is scoped to the one shape.
///
/// Twin of `tests/interpreter.rs`'s `test_drop_free_argument_does_not_make_the_result_a_view`, pinned to the same string.
#[test]
fn e2e_drop_free_argument_does_not_make_the_result_a_view() {
    let Some(out) = run_program(
        r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
struct H { r: R, n: i64 }
impl Drop for H { fn drop(mut ref self) { println(f"  dH{self.n}") } }
struct Q { s: String, n: i64 }
struct W { r: R, n: i64 }
struct N { r: R, n: i64 }

fn from_string(i: i64, s: String) -> R { return R { id: i, name: s }; }
fn from_vec(i: i64, v: Vec[String]) -> R { return R { id: i, name: v[0] }; }
fn from_struct(q: Q) -> R { return R { id: q.n, name: q.s }; }
fn from_two(a: String, b: String) -> R { return R { id: 30, name: a }; }
fn into_option(s: String) -> Option[R] { return Option.Some(R { id: 40, name: s }); }
fn into_bodyless(s: String) -> N { return N { r: R { id: 50, name: s }, n: 1 }; }
fn hand_back(r: R) -> R { return r; }
fn wrap_bodyless(r: R) -> W { return W { r: r, n: 2 }; }
fn wrap_bodied(r: R) -> H { return H { r: r, n: 3 }; }
fn from_scalar(i: i64) -> R { return R { id: i, name: f"s{i}" }; }
fn no_store(i: i64, v: Vec[i64]) -> R { return R { id: i, name: f"n{v.len()}" }; }

fn string_arg(i: i64, nm: String) { let x = from_string(i, nm); println(f"  v={x.id}"); }
fn vec_arg(i: i64, v: Vec[String]) { let x = from_vec(i, v); println(f"  v={x.id}"); }
fn struct_arg(q: Q) { let x = from_struct(q); println(f"  v={x.id}"); }
fn two_string_args(a: String, b: String) { let x = from_two(a, b); println(f"  v={x.id}"); }
fn option_return(s: String) { let o = into_option(s); match o { Option.Some(r) => { println(f"  v={r.id}"); } Option.None => { println("  v=none"); } } }
fn bodyless_return(s: String) { let n = into_bodyless(s); println(f"  v={n.r.id}"); }
fn chained(s: String) { let x = from_string(60, s); let y = hand_back(x); println(f"  v={y.id}"); }
fn same_type(r: R) { let y = hand_back(r); println(f"  v={y.id}"); }
fn wrap_control(r: R) { let w = wrap_bodyless(r); println(f"  v={w.r.id}"); }
fn wrap_bodied_control(r: R) { let h = wrap_bodied(r); println(f"  v={h.r.id}"); }
fn scalar_control(i: i64) { let x = from_scalar(i); println(f"  v={x.id}"); }
fn read_only_arg(i: i64, v: Vec[i64]) { let x = no_store(i, v); println(f"  v={x.id}"); }

fn main() {
    println("string_arg"); string_arg(1, "a");
    println("vec_arg"); vec_arg(2, ["b"]);
    println("struct_arg"); struct_arg(Q { s: "c", n: 3 });
    println("two_string_args"); two_string_args("d", "e");
    println("option_return"); option_return("f");
    println("bodyless_return"); bodyless_return("g");
    println("chained"); chained("h");
    println("same_type"); same_type(R { id: 7, name: "i" });
    println("wrap_control"); wrap_control(R { id: 8, name: "j" });
    println("wrap_bodied_control"); wrap_bodied_control(R { id: 9, name: "k" });
    println("scalar_control"); scalar_control(10);
    println("read_only_arg"); read_only_arg(11, [1, 2]);
    println("end");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"string_arg
  v=1
  dR1
vec_arg
  v=2
  dR2
struct_arg
  v=3
  dR3
two_string_args
  v=30
  dR30
option_return
  v=40
  dR40
bodyless_return
  v=50
  dR50
chained
  v=60
  dR60
same_type
  v=7
  dR7
wrap_control
  v=8
  dR8
wrap_bodied_control
  v=9
  dH3
  dR9
scalar_control
  v=10
  dR10
read_only_arg
  v=11
  dR11
end
"#
    );
}

/// B-2026-07-30-11 (optres bare-statement leg) — a BARE discarded
/// `Option` temp fires its payload's Drop body exactly like the
/// wildcard-let shape: only the `let _ =` arm called the optres
/// payload-bodies registrar, so `mkopt(2);` and `Option.Some(x);` were
/// silent on both backends while `let _ = mkopt(1);` fired. Twin of
/// `tests/interpreter.rs`'s `test_optres_bare_discard_payload_body`.
#[test]
fn e2e_optres_bare_discard_payload_body() {
    let Some(out) = run_program(
        "struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"drop {self.id} {self.name}\")\n\
             \x20   }\n\
             }\n\
             fn mkopt(n: i64) -> Option[Res] {\n\
             \x20   return Option.Some(Res { id: n, name: f\"o{n}\" });\n\
             }\n\
             fn main() {\n\
             \x20   println(\"a\");\n\
             \x20   let _ = mkopt(1);\n\
             \x20   println(\"b\");\n\
             \x20   mkopt(2);\n\
             \x20   println(\"c\");\n\
             \x20   Option.Some(Res { id: 3, name: f\"o3\" });\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "a\ndrop 1 o1\nb\ndrop 2 o2\nc\ndrop 3 o3\nend\n");
}

/// B-2026-07-30-11 (boxed-payload bodies) — a heap-BOXED Option payload
/// (a >3-word struct popped from a Vec) fires its Drop body exactly
/// ONCE: the box drop owns the memory, the bodies-only walker runs the
/// body at the binding's NLL end. The naive UserDrop-wrapper routing
/// double-freed the payload's String buffer at all three bind sites
/// (caught by valgrind mid-development — `free(): double free`), and
/// before that the body never ran at all under codegen. Twin of
/// `tests/interpreter.rs`'s `test_boxed_option_payload_body_once`.
#[test]
fn e2e_boxed_option_payload_body_once() {
    let Some(out) = run_program(
        "struct Res { id: i64, s: String }\n\
             impl Drop for Res {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"drop {self.id}\")\n\
             \x20   }\n\
             }\n\
             fn main() {\n\
             \x20   let mut v: Vec[Res] = Vec.new();\n\
             \x20   v.push(Res { id: 1, s: f\"one-{1}\" });\n\
             \x20   v.push(Res { id: 2, s: f\"two-{2}\" });\n\
             \x20   println(\"a\");\n\
             \x20   while let Option.Some(r) = v.pop() {\n\
             \x20       println(f\"got {r.id} len {r.s.len()}\");\n\
             \x20   }\n\
             \x20   println(\"b\");\n\
             \x20   let mut w: Vec[Res] = Vec.new();\n\
             \x20   w.push(Res { id: 3, s: f\"three-{3}\" });\n\
             \x20   if let Option.Some(r) = w.pop() {\n\
             \x20       println(f\"iflet {r.id} len {r.s.len()}\");\n\
             \x20   }\n\
             \x20   println(\"c\");\n\
             \x20   let mut u: Vec[Res] = Vec.new();\n\
             \x20   u.push(Res { id: 4, s: f\"four-{4}\" });\n\
             \x20   match u.pop() {\n\
             \x20       Option.Some(r) => { println(f\"match {r.id} len {r.s.len()}\"); }\n\
             \x20       Option.None => { println(\"none\"); }\n\
             \x20   }\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(
        out,
        "a\ngot 2 len 5\ndrop 2\ngot 1 len 5\ndrop 1\nb\niflet 3 len 7\ndrop 3\nc\n\
             match 4 len 6\ndrop 4\nend\n"
    );
}

/// B-2026-09-07-59 — the VALUE half. A `.clone()` whose receiver is a
/// niche-encoded `Option[shared T]` field returned an empty chain on every
/// compiled backend while `--interp` returned the real one.
///
/// The field stores ONE nullable pointer (null = None); the declared type's
/// value shape is the seeded 4-i64 `{tag,w0,w1,w2}`. The method-receiver
/// hoist bound its synth straight to the field pointer, so
/// `karac_clone_Option_*` read the pointer as the tag and three words past
/// the field — a garbage tag, hence `None`, hence `0`. The plain-struct
/// outer never diverged (its field is not niche-encoded), which is what
/// made this look like a `shared`-payload bug rather than a layout one.
///
/// `let s = n.left;` was correct throughout: the VALUE path already unpacks
/// the niche, and only the method-receiver hoist skipped it — asserted
/// alongside so a fix that broke the working path cannot pass this test.
#[test]
fn test_e2e_clone_of_niche_option_shared_field_keeps_the_chain() {
    let src = r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn clone_offset(node: Option[Node], delta: i64) -> Option[Node] {
    match node {
        None => None,
        Some(n) => Some(Node { val: n.val + delta, left: clone_offset(n.left, delta), right: clone_offset(n.right, delta) }),
    }
}
fn count_nodes(node: Option[Node]) -> i64 {
    match node { None => 0, Some(n) => 1 + count_nodes(n.left) + count_nodes(n.right) }
}
fn main() {
    let n: Node = Node { val: 5, left: Some(Node { val: 6, left: None, right: None }), right: None };
    let s = n.left.clone();
    let l0 = clone_offset(s, 10);
    let l1 = clone_offset(s, 20);
    println(f"{count_nodes(l0) + count_nodes(l1)}");
    let direct = n.left;
    println(f"{count_nodes(direct)}");
}
"#;
    assert_eq!(run_program(src).as_deref(), Some("2\n1\n"));
}

/// B-2026-09-07-60 — `mk().clone()` over a call returning
/// `Option[shared T]` used to fail codegen outright ("no handler for method
/// 'clone' on non-identifier receiver") while `--interp` answered `4`.
///
/// It lowers to the call itself: `karac_clone_Option_*` is a SHALLOW clone
/// (copy the value, rc-inc the inner handle) and the receiver is a fresh
/// temporary holding the only reference, so the clone's `+1` and the
/// discarded temporary's `-1` cancel exactly. Identity here is an equality,
/// not an approximation.
#[test]
fn test_e2e_clone_of_call_receiver_returning_option_shared() {
    let src = r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn clone_offset(node: Option[Node], delta: i64) -> Option[Node] {
    match node {
        None => None,
        Some(n) => Some(Node { val: n.val + delta, left: clone_offset(n.left, delta), right: clone_offset(n.right, delta) }),
    }
}
fn count_nodes(node: Option[Node]) -> i64 {
    match node { None => 0, Some(n) => 1 + count_nodes(n.left) + count_nodes(n.right) }
}
fn mk() -> Option[Node] { Some(Node { val: 3, left: Some(Node { val: 4, left: None, right: None }), right: None }) }
fn main() {
    let s = mk().clone();
    let l0 = clone_offset(s, 10);
    let l1 = clone_offset(s, 20);
    println(f"{count_nodes(l0) + count_nodes(l1)}");
}
"#;
    assert_eq!(run_program(src).as_deref(), Some("4\n"));
}

/// B-2026-09-07-54 — the ESCALATION half of the cloned-`Option[shared]`
/// reuse defect, gated where it is actually visible: under the NATIVE
/// allocator.
///
/// An untyped `let s = src[0].clone()` was never registered into the
/// caller-retains model, so each by-value pass decremented the payload
/// without a matching arg-site inc. The refcount after N passes is 2 - N, so
/// the third pass hands an already-freed block to the allocator a second
/// time. Pre-fix, under glibc, the three-pass program aborts:
///
///     one=2
///     two=4
///     malloc(): unaligned tcache chunk detected
///
/// — `two=4` being printed is the point: two passes is a silent
/// use-after-free READ that answers correctly, so nothing before the third
/// pass gives the defect away.
///
/// WHY THIS TEST EXISTS SEPARATELY FROM THE ASAN FIXTURE. The obvious home
/// for a double free is `tests/memory_sanitizer.rs`, and it does not work
/// there: ASAN replaces the allocator, and its quarantine means the second
/// free is not the glibc abort. Measured on the parent, a three-pass cell
/// PASSES the ASAN suite's default leg outright while failing here. So the ASAN
/// fixture (`asan_cloned_option_shared_binding_is_owned_on_every_reuse`)
/// gates the two-pass READ under `KARAC_SANITIZE_ADDRESS=1`, and this test
/// gates the three-pass FREE on the plain `--features llvm` leg that CI
/// already runs. Neither one covers the other.
///
/// Each pass count builds its own `src`, so the three cells cannot
/// contaminate one another and the failure names which count broke.
#[test]
fn e2e_cloned_option_shared_binding_survives_repeated_reuse() {
    let Some(one) = run_program(
        r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn clone_offset(node: Option[Node], delta: i64) -> Option[Node] {
    match node {
        None => None,
        Some(n) => Some(Node { val: n.val + delta, left: clone_offset(n.left, delta), right: clone_offset(n.right, delta) }),
    }
}
fn count_nodes(node: Option[Node]) -> i64 {
    match node { None => 0, Some(n) => 1 + count_nodes(n.left) + count_nodes(n.right) }
}
fn main() {
    let mut src: Vec[Option[Node]] = Vec.new();
    src.push(Some(Node { val: 1, left: Some(Node { val: 2, left: None, right: None }), right: None }));
    let s = src[0].clone();
    let l0 = clone_offset(s, 10);
    println(f"one={count_nodes(l0)}");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        one, "one=2\n",
        "one pass is balanced by luck and must stay so"
    );

    let Some(two) = run_program(
        r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn clone_offset(node: Option[Node], delta: i64) -> Option[Node] {
    match node {
        None => None,
        Some(n) => Some(Node { val: n.val + delta, left: clone_offset(n.left, delta), right: clone_offset(n.right, delta) }),
    }
}
fn count_nodes(node: Option[Node]) -> i64 {
    match node { None => 0, Some(n) => 1 + count_nodes(n.left) + count_nodes(n.right) }
}
fn main() {
    let mut src: Vec[Option[Node]] = Vec.new();
    src.push(Some(Node { val: 1, left: Some(Node { val: 2, left: None, right: None }), right: None }));
    let s = src[0].clone();
    let l0 = clone_offset(s, 10);
    let l1 = clone_offset(s, 20);
    println(f"two={count_nodes(l0) + count_nodes(l1)}");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        two, "two=4\n",
        "two passes free the payload while `src` still holds it — the ANSWER \
             stays right, which is why this half needs the instrumented ASAN leg \
             to see anything"
    );

    let Some(three) = run_program(
        r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn clone_offset(node: Option[Node], delta: i64) -> Option[Node] {
    match node {
        None => None,
        Some(n) => Some(Node { val: n.val + delta, left: clone_offset(n.left, delta), right: clone_offset(n.right, delta) }),
    }
}
fn count_nodes(node: Option[Node]) -> i64 {
    match node { None => 0, Some(n) => 1 + count_nodes(n.left) + count_nodes(n.right) }
}
fn main() {
    let mut src: Vec[Option[Node]] = Vec.new();
    src.push(Some(Node { val: 1, left: Some(Node { val: 2, left: None, right: None }), right: None }));
    let s = src[0].clone();
    let l0 = clone_offset(s, 10);
    let l1 = clone_offset(s, 20);
    let l2 = clone_offset(s, 30);
    println(f"three={count_nodes(l0) + count_nodes(l1) + count_nodes(l2)}");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        three, "three=6\n",
        "three passes drive the refcount to -1 and free the block twice; \
             pre-fix this aborts under glibc with `malloc(): unaligned tcache \
             chunk detected` and prints nothing at all"
    );
}

/// B-2026-08-12-1. Passing a by-value `Option`/`Result` argument twice: the
/// SECOND call reads the wrong variant. The caller's pass-as-arg move
/// cap-zeros the source binding
/// (`suppress_inline_result_payload_cleanup_for_moved_arg`) on the
/// assumption that the destination takes ownership — which holds for
/// `v.push(r)`, the shape it was written for, but not for a plain call:
/// `make_aggregate_param_callee_owned_inst` bails on `Option`/`Result`, so
/// the callee never takes it and the zeroing is pure corruption of a
/// binding the caller still owns.
///
/// Kāra's move-checker deliberately does NOT reject double-consume
/// (`param_own.rs` § "Why not move-by-default" — `take(x); take(x)` is
/// specified to work), so this is a silent wrong answer in an accepted
/// program, not a diagnosable misuse. The interpreter prints the right
/// answer on both, which is what makes it a backend divergence rather than
/// a language question.
///
/// Same root cause as B-2026-08-11-30's leak: caller retracts, callee
/// bails. Both halves have to move together.
///
/// FIXED: the callee now entry-COPIES such a param and the caller keeps its
/// original, so nothing is zeroed and every pass reads the true value.
#[test]
fn e2e_repeated_by_value_optres_arg_reads_wrong_variant() {
    assert_eq!(
        run_program(
            r#"
enum E { Missing(String) }
fn mkv() -> Vec[String] { let mut v: Vec[String] = Vec.new(); v.push("x"); v.push("y"); return v; }
fn mkr() -> Result[Vec[String], E] { return Ok(mkv()); }
fn takr(r: Result[Vec[String], E]) -> i64 { match r { Ok(v) => v.len(), Err(_e) => -1 } }
fn tako(o: Option[Vec[String]]) -> i64 { match o { Some(v) => v.len(), None => -1 } }
fn main() {
    let r = mkr();
    println(takr(r));
    println(takr(r));
    let o = Some(mkv());
    println(tako(o));
    println(tako(o));
}
"#
        ),
        // Codegen prints 2 / -1 / 2 / -1; the interpreter prints all 2s. A
        // user ENUM in this exact shape is already correct on both.
        Some("2\n2\n2\n2\n".to_string())
    );
}

/// B-2026-07-28-8: storing into a PLAIN (value-type) struct's
/// `Option[shared T]` field must retain the new inner BEFORE releasing the
/// old one.
///
/// The shared-struct destinations already did; the plain-struct
/// fall-through instead dropped the old field value and then raw-stored the
/// new one. That is fine for a fresh RHS and fatal for an ALIASING one: in
/// `h.head = n.next` the RHS is loaded un-retained, dropping the old head
/// releases the old head's own `next` — the very node just loaded — and the
/// store then commits a dangling pointer. The next read is a
/// use-after-free and the scope-exit drop of the field is a double free.
///
/// This is the list-head shape (`struct DList { mut head: Option[Node] }`)
/// that `examples/tangle/doubly_linked.kara` is built from; it printed an
/// uninitialized value where the README documents `3`.
#[test]
fn test_e2e_plain_struct_option_shared_field_store_retains_aliasing_rhs() {
    // `h.head = n.next` where the old `h.head` transitively owns the RHS.
    // Reading the field afterwards must see node 2, not freed memory.
    assert_eq!(
        run_program(
            r#"
shared struct Node { mut val: i64, mut next: Option[Node] }
struct H { mut head: Option[Node] }
fn main() {
    let mut h = H { head: None };
    let a = Node { val: 1, next: None };
    let b = Node { val: 2, next: None };
    a.next = Some(b);
    h.head = Some(a);
    match h.head { None => {} Some(n) => { h.head = n.next; } }
    match h.head { None => { println("none") } Some(m) => { println(m.val) } }
}
"#
        ),
        Some("2\n".to_string())
    );
    // Same store reached through a `mut ref self` method receiver — the
    // sibling branch in `compile_field_store`, hooked alongside it.
    assert_eq!(
        run_program(
            r#"
shared struct Node { mut val: i64, mut next: Option[Node] }
struct H { mut head: Option[Node] }
impl H {
    fn advance(mut ref self) {
        match self.head { None => {} Some(n) => { self.head = n.next; } }
    }
}
fn main() {
    let mut h = H { head: None };
    let a = Node { val: 1, next: None };
    let b = Node { val: 2, next: None };
    a.next = Some(b);
    h.head = Some(a);
    h.advance();
    match h.head { None => { println("none") } Some(m) => { println(m.val) } }
}
"#
        ),
        Some("2\n".to_string())
    );
    // Self-assignment must not free the node: retain-then-release keeps the
    // count above zero across the store.
    assert_eq!(
        run_program(
            r#"
shared struct Node { mut val: i64, mut next: Option[Node] }
struct H { mut head: Option[Node] }
fn main() {
    let mut h = H { head: None };
    let a = Node { val: 7, next: None };
    h.head = Some(a);
    h.head = h.head;
    match h.head { None => { println("none") } Some(m) => { println(m.val) } }
}
"#
        ),
        Some("7\n".to_string())
    );
    // Overwriting with an unrelated node still releases the old one — the
    // retain must not turn the old-side release into a leak. Two writes to
    // the same field, then a read of the survivor.
    assert_eq!(
        run_program(
            r#"
shared struct Node { mut val: i64, mut next: Option[Node] }
struct H { mut head: Option[Node] }
fn main() {
    let mut h = H { head: None };
    let a = Node { val: 1, next: None };
    let b = Node { val: 2, next: None };
    h.head = Some(a);
    h.head = Some(b);
    h.head = None;
    match h.head { None => { println("none") } Some(m) => { println(m.val) } }
}
"#
        ),
        Some("none\n".to_string())
    );
}

#[test]
fn test_e2e_process_try_wait_kill_reap() {
    // `Child.try_wait` (bit-packed Option[ExitStatus] Ok payload) on a
    // long sleeper: Ok(None) while running, then kill + wait reaps with
    // success=false (signal-killed → code -1). Second wait after the
    // reap is Err(NotFound) — the pid entry is removed up front,
    // matching the interpreter.
    if let Some(out) = run_program(
        r#"
fn main() {
    let sleeper = Command.new("sleep").arg("30");
    match sleeper.spawn() {
        Ok(child) => {
            match child.try_wait() {
                Ok(maybe) => {
                    match maybe {
                        Some(st) => println("exited early?!"),
                        None => println("running"),
                    }
                }
                Err(e) => println("try_wait failed"),
            }
            match child.kill() {
                Ok(u) => println("killed"),
                Err(e) => println("kill failed"),
            }
            match child.wait() {
                Ok(st) => println(f"success={st.success} code={st.code}"),
                Err(e) => println("wait failed"),
            }
            match child.wait() {
                Ok(st) => println("double reap?!"),
                Err(e) => {
                    match e {
                        IoError.NotFound => println("reaped"),
                        _ => println("unexpected err"),
                    }
                }
            }
        }
        Err(e) => println("spawn failed"),
    }
}
"#,
    ) {
        assert_eq!(out, "running\nkilled\nsuccess=false code=-1\nreaped\n");
    }
}

#[test]
fn test_e2e_result_chained_generic_mono_struct_field_read() {
    // B-2026-07-12-2 gap 3 (general, NOT once-specific): a chained field
    // READ through a generic-instantiated struct payload binding
    // (`Err(e) => e.inner.a` where `e: Wrap[Pair]`) used to silently read the
    // `i64 0` placeholder — `type_name_of_expr` returned the generic PARAM
    // name (`T`) for `e.inner` instead of resolving the concrete `Pair`
    // through the mono instantiation. Now it substitutes the concrete args.
    if let Some(out) = run_program(
            "struct Pair { a: i64, b: i64 }\n\
             struct Wrap[T] { inner: T }\n\
             fn boom() -> Result[i64, Wrap[Pair]] { Err(Wrap { inner: Pair { a: 3i64, b: 4i64 } }) }\n\
             fn main() {\n\
                 match boom() {\n\
                     Ok(_) => { println(\"ok\"); },\n\
                     Err(e) => { println((e.inner.a + e.inner.b).to_string()); },\n\
                 }\n\
             }",
        ) {
            assert_eq!(out, "7\n");
        }
}

/// B-2026-07-09-6: matching a BORROWED `Option[struct]` (`ref` / `mut ref`)
/// and reading a field of the `Some(n)` payload returned 0 — a silent
/// wrong-answer miscompile. A single-field struct flattens to one i64
/// payload word, so the via-ptr `TupleVariant` fast path bound `n` as a
/// ref-to-i64 leaf (the wrong type); the arm body's `n.field` access then
/// collapsed to 0. Independent of `shared`/RC — plain structs and shared
/// structs both hit it. The fix defers struct-typed payload bindings to the
/// value-source path, which reconstructs the struct aggregate at the right
/// type. Owned `Option[struct]` always worked (the control below).
#[test]
fn borrowed_option_struct_payload_field_read_is_correct() {
    // Plain struct, `ref` param.
    let plain_ref = "struct P { val: i64 }\n\
            fn getval(o: ref Option[P]) -> i64 { match o { None => 999i64, Some(n) => n.val } }\n\
            fn main() { println(getval(Some(P { val: -3i64 }))); println(getval(None)); }\n";
    if let Some(out) = run_program(plain_ref) {
        assert_eq!(
            out.trim(),
            "-3\n999",
            "ref Option[struct] field read; got:\n{out}"
        );
    }
    // Plain struct, `mut ref` param — same projection path.
    let plain_mut = "struct P { val: i64 }\n\
            fn getval(o: mut ref Option[P]) -> i64 { match o { None => 999i64, Some(n) => n.val } }\n\
            fn main() { let mut x = Some(P { val: 7i64 }); println(getval(mut x)); }\n";
    if let Some(out) = run_program(plain_mut) {
        assert_eq!(
            out.trim(),
            "7",
            "mut ref Option[struct] field read; got:\n{out}"
        );
    }
    // Shared struct, `ref` param, recursive walk (the #98 validate-BST shape):
    // a list [5, -3]; `any_negative` must find the -3 through the borrow.
    let shared_rec = "shared struct Node { val: i64, mut next: Option[Node] }\n\
            fn any_negative(node: ref Option[Node]) -> bool {\n\
                match node {\n\
                    None => false,\n\
                    Some(n) => { if n.val < 0i64 { return true; } any_negative(n.next) }\n\
                }\n\
            }\n\
            fn main() {\n\
                let list = Some(Node { val: 5i64, next: Some(Node { val: -3i64, next: None }) });\n\
                println(any_negative(list));\n\
            }\n";
    if let Some(out) = run_program(shared_rec) {
        assert_eq!(
            out.trim(),
            "true",
            "ref Option[shared struct] recursive read; got:\n{out}"
        );
    }
    // Owned control — must remain correct (guards against a regression the
    // other way if the deferral logic ever changes).
    let owned = "struct P { val: i64 }\n\
            fn getval(o: Option[P]) -> i64 { match o { None => 999i64, Some(n) => n.val } }\n\
            fn main() { println(getval(Some(P { val: -3i64 }))); }\n";
    if let Some(out) = run_program(owned) {
        assert_eq!(
            out.trim(),
            "-3",
            "owned Option[struct] control; got:\n{out}"
        );
    }
}

/// Regression (B-2026-07-03-16, duplicate of B-2026-07-03-3): immediate
/// field access on a struct-returning call — `f().field` — read `0` under
/// `karac build` while `karac run` was correct, because `type_name_of_expr`
/// had no `Call`/`MethodCall` arm so `field_index_for` returned `None` and
/// `compile_field_access`'s generic tail emitted the `i64 0` placeholder
/// instead of an `extractvalue`. Fixed by 839beaea's Call/MethodCall arms.
/// This locks in the shapes B-16 characterized that the sibling test
/// (`e2e_chained_call_struct_field_access`) does NOT cover: a static
/// associated fn (`Cnt.zero()` — the two-segment `Call/Path` arm), a
/// `ref self` method (`g.goref()`), the multi-field offset-independence
/// (`makeP().a`/`.b`), and the plain non-f-string `print(make().n)` path
/// (B-16 confirmed the bug was not f-string specific).
#[test]
fn e2e_call_result_field_access_assoc_and_ref_self() {
    if let Some(out) = run_program(
        "struct Cnt { n: i64 }\n\
             struct P { a: i64, b: i64 }\n\
             fn make() -> Cnt { Cnt { n: 7 } }\n\
             fn make_p() -> P { P { a: 3, b: 9 } }\n\
             struct Gen { seed: i64 }\n\
             impl Gen {\n\
             \x20   fn goref(ref self) -> Cnt { Cnt { n: self.seed + 2 } }\n\
             }\n\
             impl Cnt {\n\
             \x20   fn zero() -> Cnt { Cnt { n: 0 } }\n\
             \x20   fn forty() -> Cnt { Cnt { n: 40 } }\n\
             }\n\
             fn main() {\n\
             \x20   print(make().n);\n\
             \x20   println(f\"{make().n + 100}\");\n\
             \x20   println(f\"{make_p().a}\");\n\
             \x20   println(f\"{make_p().b}\");\n\
             \x20   println(f\"{Cnt.zero().n}\");\n\
             \x20   println(f\"{Cnt.forty().n}\");\n\
             \x20   let g = Gen { seed: 5 };\n\
             \x20   println(f\"{g.goref().n}\");\n\
             }",
    ) {
        // print(make().n)=7 (no newline) then make().n+100=107 → "7107\n";
        // make_p().a=3, .b=9; Cnt.zero().n=0, Cnt.forty().n=40; g.goref().n=7
        assert_eq!(out, "7107\n3\n9\n0\n40\n7\n");
    }
}

#[test]
fn e2e_contract_ensures_result_field_access() {
    // `ensures(result) result.field == ...` must read the right field of the
    // returned struct. The `result` binding now records its static type
    // name so field access resolves the struct field index; pre-fix it read
    // the wrong slot and the contract spuriously failed at runtime.
    if let Some(out) = run_program(
        "pub struct In { q: i64 }\n\
             pub struct Out { q: i64, doubled: i64 }\n\
             pub fn f(x: In) -> Out\n\
                 ensures(result) result.q == old(x.q)\n\
             {\n\
                 Out { q: x.q, doubled: x.q * 2 }\n\
             }\n\
             fn main() {\n\
                 let o = f(In { q: 21 });\n\
                 println(f\"{o.q} {o.doubled}\");\n\
             }",
    ) {
        assert_eq!(out, "21 42\n");
    }
}

#[test]
fn test_e2e_question_on_result_concrete_enum() {
    // B-2026-07-11-7: `?` on `Result[<concrete user enum>, E]` unwraps the Ok
    // payload. The reconstruction (a) needs the enum's TYPE — which
    // `enum_inst_type_exprs` (generic-only) dropped, truncating to `w0` and
    // tripping module verification (`insertvalue`/`br` type mismatch) — and
    // (b) needs the enum's FULL word span (a heap-bearing enum flattens to >3
    // words, exceeding the old `rebuild_value_from_payload_words` cap → a
    // dropped `cap` word → invalid free). Both fixed. Exercises a unit variant,
    // a String payload moved out, and a wide `(i64, String)` tuple variant.
    if let Some(out) = run_program(
            "enum J { N, S(String), Pair(i64, String) }\n\
             fn get(k: i64) -> Result[J, String] {\n\
                 if k == 0 { Result.Ok(J.N) }\n\
                 else { if k == 1 { Result.Ok(J.S(\"hi\")) } else { Result.Ok(J.Pair(7, \"x\")) } }\n\
             }\n\
             fn show(k: i64) -> Result[String, String] {\n\
                 let v = get(k)?;\n\
                 match v {\n\
                     N => { Result.Ok(\"N\") }\n\
                     S(s) => { Result.Ok(s) }\n\
                     Pair(a, s) => { Result.Ok(f\"{a}:{s}\") }\n\
                 }\n\
             }\n\
             fn main() {\n\
                 match show(0) { Ok(r) => println(r), Err(_) => println(\"e\") }\n\
                 match show(1) { Ok(r) => println(r), Err(_) => println(\"e\") }\n\
                 match show(2) { Ok(r) => println(r), Err(_) => println(\"e\") }\n\
             }",
        ) {
            assert_eq!(out, "N\nhi\n7:x\n");
        }
}

#[test]
fn test_e2e_result_struct_wrapper_moved_to_callee_and_its_controls() {
    // B-2026-08-11-29 — the value half. `match mk() { Ok(s) => take(s) }`
    // where `S` has a `Map`/`Set` field SEGV'd on a default build (exit
    // 139), with `karac check` clean and the interpreter correct, so the
    // compiled binary produced no output at all rather than a diagnostic.
    //
    // One small program per shape rather than one combined one: a single
    // program holding all of them trips an unrelated module-verification
    // failure that reproduces ONLY through this harness — `karac build` and
    // `karac run --interp` both compile and run the combined program
    // correctly, with and without this fix. Kept split so this pin tests
    // the row rather than that.
    //
    // The two crashing shapes:
    let map_field = "enum E { Missing(String) }\n\
             struct Sm { a: Map[String, i64] }\n\
             fn mk() -> Result[Sm, E] { let mut m: Map[String, i64] = Map.new(); m.insert(\"k\", 1); Ok(Sm { a: m }) }\n\
             fn take(s: Sm) { println(f\"{s.a.len()}\"); }\n\
             fn main() { match mk() { Ok(s) => take(s), Err(_e) => println(\"e\") } }\n";
    assert_eq!(run_program(map_field).as_deref(), Some("1\n"));

    let set_field = "enum E { Missing(String) }\n\
             struct Ss { a: Set[String] }\n\
             fn mk() -> Result[Ss, E] { let mut s: Set[String] = Set.new(); s.insert(\"k\"); Ok(Ss { a: s }) }\n\
             fn take(s: Ss) { println(f\"{s.a.len()}\"); }\n\
             fn main() { match mk() { Ok(s) => take(s), Err(_e) => println(\"e\") } }\n";
    assert_eq!(run_program(set_field).as_deref(), Some("1\n"));

    // The same move out of the ERR arm — the detector keys on the arm's
    // pattern, not on `Ok` specifically.
    let err_arm = "struct Er { m: Map[String, i64] }\n\
             fn mk() -> Result[i64, Er] { let mut m: Map[String, i64] = Map.new(); m.insert(\"k\", 1); return Err(Er { m: m }); }\n\
             fn take(e: Er) { println(f\"{e.m.len()}\"); }\n\
             fn main() { match mk() { Ok(v) => println(f\"{v}\"), Err(e) => take(e) } }\n";
    assert_eq!(run_program(err_arm).as_deref(), Some("1\n"));

    // The over-fire controls, each already clean and each a shape a broader
    // fix would plausibly regress into a LEAK by suppressing a source drop
    // that is the only owner.
    //
    // `Option` rather than `Result` — the sharpest, since the two share most
    // of this machinery.
    let option_wrapper = "struct Sm { a: Map[String, i64] }\n\
             fn mk() -> Option[Sm] { let mut m: Map[String, i64] = Map.new(); m.insert(\"k\", 1); return Some(Sm { a: m }); }\n\
             fn take(s: Sm) { println(f\"{s.a.len()}\"); }\n\
             fn main() { match mk() { Some(s) => take(s), None => println(\"n\") } }\n";
    assert_eq!(run_program(option_wrapper).as_deref(), Some("1\n"));

    // No wrapper at all.
    let no_wrapper = "struct Sm { a: Map[String, i64] }\n\
             fn mk() -> Sm { let mut m: Map[String, i64] = Map.new(); m.insert(\"k\", 1); return Sm { a: m }; }\n\
             fn take(s: Sm) { println(f\"{s.a.len()}\"); }\n\
             fn main() { take(mk()); }\n";
    assert_eq!(run_program(no_wrapper).as_deref(), Some("1\n"));

    // A bare Map payload — no struct to wrap it.
    let bare_payload = "enum E { Missing(String) }\n\
             fn mk() -> Result[Map[String, i64], E] { let mut m: Map[String, i64] = Map.new(); m.insert(\"k\", 1); Ok(m) }\n\
             fn take(m: Map[String, i64]) { println(f\"{m.len()}\"); }\n\
             fn main() { match mk() { Ok(m) => take(m), Err(_e) => println(\"e\") } }\n";
    assert_eq!(run_program(bare_payload).as_deref(), Some("1\n"));

    // Consumed INLINE in the arm rather than passed by value.
    let inline_use = "enum E { Missing(String) }\n\
             struct Sm { a: Map[String, i64] }\n\
             fn mk() -> Result[Sm, E] { let mut m: Map[String, i64] = Map.new(); m.insert(\"k\", 1); Ok(Sm { a: m }) }\n\
             fn main() { match mk() { Ok(s) => println(f\"{s.a.len()}\"), Err(_e) => println(\"e\") } }\n";
    assert_eq!(run_program(inline_use).as_deref(), Some("1\n"));

    // A `Vec` field, which was ALREADY suppressed through a different
    // predicate (the struct is entry-copy supported, so
    // `inline_result_payload_binding_registers_own_drop` is true). It must
    // stay exactly as clean — this is the control that the new condition is
    // additive rather than a behaviour change.
    let vec_field = "enum E { Missing(String) }\n\
             struct Sv { a: Vec[String] }\n\
             fn mk() -> Result[Sv, E] { let mut v: Vec[String] = Vec.new(); v.push(\"k\"); Ok(Sv { a: v }) }\n\
             fn take(s: Sv) { println(f\"{s.a.len()}\"); }\n\
             fn main() { match mk() { Ok(s) => take(s), Err(_e) => println(\"e\") } }\n";
    assert_eq!(run_program(vec_field).as_deref(), Some("1\n"));
}

#[test]
fn test_e2e_option_ok_or_builds_result_layout() {
    // B-2026-07-15-15: `Option.ok_or(e)` built the result value in the
    // OPTION layout `{tag, w0..w2}` (4 fields), but the value is typed
    // `Result` whose layout is `{tag, w0..w4}` (6 fields) with Ok/Err at
    // offset `(0, 5)`. A downstream `match r { Ok(v)/Err(e) }` extracted 5
    // payload words from the 4-field value → `build_extract_value(sv, 5)`
    // = ExtractOutOfRange, a compiler ICE — for EVERY payload type (i64 and
    // String alike), so `ok_or` never worked under `karac build`. Fixed by
    // building the result in the Result layout (copy the Some payload into
    // the Ok slots, pack `e` into the Err slots).
    if let Some(out) = run_program(
        "fn main() {\n\
                 let a: Option[i64] = Some(42);\n\
                 match a.ok_or(\"no\") {\n\
                     Ok(v) => println(f\"ok {v}\"),\n\
                     Err(e) => println(f\"err {e}\"),\n\
                 }\n\
                 let b: Option[i64] = None;\n\
                 match b.ok_or(\"missing value\") {\n\
                     Ok(v) => println(f\"ok {v}\"),\n\
                     Err(e) => println(f\"err {e}\"),\n\
                 }\n\
                 let c: Option[i64] = None;\n\
                 match c.ok_or(0 - 99) {\n\
                     Ok(v) => println(f\"ok {v}\"),\n\
                     Err(e) => println(f\"errcode {e}\"),\n\
                 }\n\
                 // unwrap_or on the Ok/Err result\n\
                 let d: Option[i64] = Some(7);\n\
                 println(d.ok_or(\"x\").unwrap_or(0));\n\
             }",
    ) {
        assert_eq!(out, "ok 42\nerr missing value\nerrcode -99\n7\n");
    }
}

#[test]
fn test_e2e_question_on_result_nested_option_payload() {
    // B-2026-07-13-19: `?` on `Result[Option[T], E]` — the Ok payload is
    // itself an `Option[T]`. `reconstruct_question_ok_payload`'s wrapper
    // guard (B-2026-07-11-7) matched any `Option`/`Result`-headed recorded
    // type at the `?` span and truncated it to `w0`, so the extracted
    // `Option[T]` lost its payload words and the subsequent `match` could not
    // type the `Some` binding (`Undefined variable 's'` on the value form;
    // `no handler for method 'len'` on the method form). The typechecker now
    // records the unwrapped payload type under a dedicated key, so codegen
    // rebuilds the genuine multi-word `Option[T]`. Exercises a String payload
    // returned from the arm AND consumed by a method.
    if let Some(out) = run_program(
        "enum E { X }\n\
             fn inner(n: i64) -> Result[Option[String], E] {\n\
                 if n < 0 { return Err(E.X); }\n\
                 Ok(Some(f\"got-{n}\"))\n\
             }\n\
             fn outer(n: i64) -> Result[String, E] {\n\
                 let opt = inner(n)?;\n\
                 match opt { Some(s) => Ok(s), None => Ok(f\"empty\") }\n\
             }\n\
             fn olen(n: i64) -> Result[i64, E] {\n\
                 let opt = inner(n)?;\n\
                 match opt { Some(s) => Ok(s.len()), None => Ok(0) }\n\
             }\n\
             fn main() {\n\
                 match outer(7) { Ok(v) => println(v), Err(_) => println(\"e\") }\n\
                 match olen(7) { Ok(v) => println(v), Err(_) => println(\"e\") }\n\
             }",
    ) {
        assert_eq!(out, "got-7\n5\n");
    }
}

#[test]
fn test_e2e_option_result_map() {
    // B-2026-07-12-11 — `Option[T].map(f)` / `Result[T, E].map(f)` were
    // unimplemented in codegen (no dispatch arm) despite typechecking. Now
    // the map lowering branches on the tag, reconstructs the payload, binds
    // it to a synthetic local, and compiles `Some(f(x))` / `Ok(f(x))` —
    // reusing the call + ctor codegen — with the absent receiver passed
    // through. Covers a fn-reference and an annotated closure, a Result Ok
    // (mapped) and Err (passthrough), a type-changing map (i64 -> bool),
    // and a chain. interp == JIT == AOT (heap payloads now supported too —
    // see test_e2e_option_result_map_heap_payload).
    if let Some(out) = run_program(
        "fn dbl(n: i64) -> i64 { n * 2 }\n\
             fn main() {\n\
                 let a: Option[i64] = Some(5);\n\
                 println(f\"{a.map(dbl).unwrap_or(-1)}\");\n\
                 let n: Option[i64] = None;\n\
                 println(f\"{n.map(dbl).unwrap_or(-1)}\");\n\
                 let ok: Result[i64, String] = Ok(21);\n\
                 println(f\"{ok.map(dbl).unwrap_or(-1)}\");\n\
                 let er: Result[i64, String] = Err(f\"boom\");\n\
                 println(f\"{er.map(dbl).unwrap_or(-99)}\");\n\
                 println(f\"{a.map(dbl).map(|x: i64| x + 1).unwrap_or(-1)}\");\n\
                 let m = a.map(|x: i64| x > 3);\n\
                 match m { Some(b) => { println(f\"{b}\"); } None => { println(\"none\"); } }\n\
             }",
    ) {
        assert_eq!(out, "10\n-1\n42\n-99\n11\ntrue\n");
    }
}

/// B-2026-08-09-8 — the shape above, with one `let p = o;` in front of it.
///
/// A bare rebind is a whole-value MOVE, and the let-site's registration gate
/// could not see it: `rhs_is_fresh_inline_enum` rejects an identifier naming
/// an existing binding (correctly — not fresh), and neither passthrough
/// detector matches a bare identifier, so `p` got no cleanup and `o` kept
/// its. That much is balanced on its own; what broke is that `p` was then
/// invisible to `scrutinee_is_inline_optres_local`, the membership test the
/// caller-retains classifier consults. Every `match p` therefore took the
/// OWNED path and its arm binding freed the buffer at arm exit, while `o`'s
/// untouched scope-exit action freed it again.
///
/// ONE match already aborted, which the row filing this got wrong — its
/// narrowing recorded a single read as clean. Measured on the filing commit
/// and on its parent: one match is `1 errors from 1 contexts` and a
/// `free(): double free detected in tcache 2` abort, two is 3 errors and
/// adds an `Invalid read of size 2` (the second match reading the freed
/// buffer — a use-after-free, not only a double free), three is 5. So the
/// count is `2n-1`, and the very first read is already wrong.
///
/// The control is the neighbouring test above: the identical double read
/// WITHOUT the rebind is clean, because there the scrutinee is the
/// registered binding and the classifier fires.
#[test]
fn test_e2e_rebound_option_local_is_read_repeatedly() {
    // One read — the shape the row recorded as clean.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let o: Option[String] = Some(f\"hi\");\n\
                     let p: Option[String] = o;\n\
                     match p { Some(v) => { println(v); } None => {} }\n\
                 }"
        )
        .as_deref(),
        Some("hi\n")
    );
    // Two and three reads.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let o: Option[String] = Some(f\"hi\");\n\
                     let p: Option[String] = o;\n\
                     match p { Some(v) => { println(v); } None => {} }\n\
                     match p { Some(v) => { println(v); } None => {} }\n\
                     match p { Some(v) => { println(v); } None => {} }\n\
                 }"
        )
        .as_deref(),
        Some("hi\nhi\nhi\n")
    );
    // A CHAINED rebind: the transfer has to keep moving, not stop at `p`.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let o: Option[String] = Some(f\"hi\");\n\
                     let p: Option[String] = o;\n\
                     match p { Some(v) => { println(v); } None => {} }\n\
                     let q: Option[String] = p;\n\
                     match q { Some(v) => { println(v); } None => {} }\n\
                 }"
        )
        .as_deref(),
        Some("hi\nhi\n")
    );
    // `Result`, the sibling registry, and a `Vec` payload (an ELEMENT read,
    // which actually touches the buffer — `.len()` would pass unfixed).
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let o: Result[String, i64] = Ok(f\"hi\");\n\
                     let p: Result[String, i64] = o;\n\
                     match p { Ok(v) => { println(v); } Err(_) => {} }\n\
                     match p { Ok(v) => { println(v); } Err(_) => {} }\n\
                 }"
        )
        .as_deref(),
        Some("hi\nhi\n")
    );
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let mut v: Vec[i64] = Vec.new(); v.push(111); v.push(222);\n\
                     let o: Option[Vec[i64]] = Some(v);\n\
                     let p: Option[Vec[i64]] = o;\n\
                     match p { Some(x) => { println(x[0]); } None => {} }\n\
                     match p { Some(x) => { println(x[1]); } None => {} }\n\
                 }"
        )
        .as_deref(),
        Some("111\n222\n")
    );
    // The shape that chose TRANSFER over alias: reassigning the SOURCE after
    // the move must not strand the payload. Under an alias fix this is a
    // 2-byte leak; the destination has to be the owner for it to be clean.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let mut o: Option[String] = Some(f\"hi\");\n\
                     let p: Option[String] = o;\n\
                     match p { Some(v) => { println(v); } None => {} }\n\
                     o = None;\n\
                     match o { Some(v) => { println(v); } None => { println(\"none\"); } }\n\
                 }"
        )
        .as_deref(),
        Some("hi\nnone\n")
    );
}

/// B-2026-08-09-7 — chaining two `Result`-returning combinators without an
/// intervening `let`. TWO independent defects, and the first one masked the
/// second:
///
/// 1. `type_name_of_expr` had no answer for a chained combinator —
///    `Option`/`Result` builtins never reach `fn_return_type_names`, whose
///    contract is user fns. So the chain resolved to `None`, every caller
///    asking "is this a Result?" silently got `false`, and the outer `map`
///    built a `Some(..)` for a `Result`: `PHI node operands are not the same
///    type as the result` under `unwrap_or`/`is_ok`, and a raw panic in
///    `bind_pattern_values` (`ExtractOutOfRange`) under a `match`.
///
/// 2. Underneath it, a DOUBLE EVALUATION of the receiver.
///    `try_compile_option_result_method` compiled the receiver eagerly, then
///    `compile_map_via_match_synthesis` compiled it again as the synthesized
///    match's scrutinee. For an identifier receiver that is a dead reload —
///    invisible. For a chained one it re-runs the whole inner combinator,
///    and the first run has already moved the payload out and zeroed the
///    source's words, so the second reads zeros: case 5 printed `2`
///    (`f(0)=1`, `g(1)=2`) instead of `42`, and the heap cases came back as
///    the empty string.
///
/// Fixing only (1) turns a loud panic into a silent wrong answer, which is
/// why both land together. `Option` was immune to (1) by luck — guessing
/// "not a Result" is right for it — so cases 7-8 are the controls that must
/// stay green, and case 9 pins the `let` workaround that always worked.
#[test]
fn test_e2e_chained_result_combinators_evaluate_the_receiver_once() {
    // 1. All-scalar, hand-rolled lowering both times — was an ICE.
    assert_eq!(
            run_program(
                "fn main() {\n\
                     let r: Result[i64, i64] = Ok(20i64);\n\
                     match r.map(|x| x + 1i64).map(|x| x * 2i64) { Ok(v) => { println(v); } Err(e) => { println(e); } }\n\
                 }"
            )
            .as_deref(),
            Some("42\n")
        );
    // 2. Same chain into a combinator — was the PHI verification failure.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let r: Result[i64, i64] = Ok(20i64);\n\
                     println(r.map(|x| x + 1i64).map(|x| x * 2i64).unwrap_or(0i64));\n\
                 }"
        )
        .as_deref(),
        Some("42\n")
    );
    // 3/4. Heap on both sides — synthesis lowering, both branches.
    assert_eq!(
            run_program(
                "fn main() {\n\
                     let r: Result[String, String] = Ok(f\"hi\");\n\
                     match r.map(|x| x.to_uppercase()).map(|x| x.to_uppercase()) { Ok(v) => { println(v); } Err(e) => { println(e); } }\n\
                 }"
            )
            .as_deref(),
            Some("HI\n")
        );
    assert_eq!(
            run_program(
                "fn main() {\n\
                     let r: Result[String, String] = Err(f\"boom\");\n\
                     match r.map(|x| x.to_uppercase()).map(|x| x.to_uppercase()) { Ok(v) => { println(v); } Err(e) => { println(e); } }\n\
                 }"
            )
            .as_deref(),
            Some("boom\n")
        );
    // 5. The double-evaluation witness: printed `2` before the fix.
    assert_eq!(
            run_program(
                "fn main() {\n\
                     let r: Result[i64, String] = Ok(20i64);\n\
                     match r.map(|x| x + 1i64).map(|x| x * 2i64) { Ok(v) => { println(v); } Err(e) => { println(e); } }\n\
                 }"
            )
            .as_deref(),
            Some("42\n")
        );
    // 6. Not just `map` — the Result-returning combinator FAMILY.
    assert_eq!(
            run_program(
                "fn main() {\n\
                     let r: Result[i64, i64] = Ok(20i64);\n\
                     match r.map_err(|e| e + 1i64).map(|x| x * 2i64) { Ok(v) => { println(v); } Err(e) => { println(e); } }\n\
                 }"
            )
            .as_deref(),
            Some("40\n")
        );
    // 7/8. Option controls — immune to defect (1), must stay green.
    assert_eq!(
            run_program(
                "fn main() {\n\
                     let o: Option[String] = Some(f\"hi\");\n\
                     match o.map(|x| x.to_uppercase()).map(|x| x.to_uppercase()) { Some(v) => { println(v); } None => { println(\"-\"); } }\n\
                 }"
            )
            .as_deref(),
            Some("HI\n")
        );
    assert_eq!(
            run_program(
                "fn main() {\n\
                     let o: Option[i64] = Some(20i64);\n\
                     match o.map(|x| x + 1i64).map(|x| x * 2i64).map(|x| x + 0i64) { Some(v) => { println(v); } None => { println(\"-\"); } }\n\
                 }"
            )
            .as_deref(),
            Some("42\n")
        );
    // 9. The `let` workaround, which always worked — it must keep working,
    //    since it is the shape that proved the receiver was evaluated twice.
    assert_eq!(
            run_program(
                "fn main() {\n\
                     let r: Result[String, String] = Ok(f\"hi\");\n\
                     let m = r.map(|x| x.to_uppercase());\n\
                     match m.map(|x| x.to_uppercase()) { Ok(v) => { println(v); } Err(e) => { println(e); } }\n\
                 }"
            )
            .as_deref(),
            Some("HI\n")
        );
}

/// B-2026-08-28-72 — the `Option` / `Result` leg of B-2026-08-09-15, whose
/// retraction this reuses verbatim.
///
/// That row taught the caller to stop running an owned arg's payload bodies
/// when the callee hands the payload back out of a `match` arm. Its gate
/// admitted only a bare user enum, excluding `Option` and `Result` by name
/// on the stated ground that they "have their own" retraction. They do not.
/// The SAME program spelled with a user enum was correct on every backend
/// while the `Option` spelling ran the payload's `Drop` body TWICE on both
/// compiled ones — the caller's `__karac_dropelems_opt_*` walk at the arg's
/// live-range end, plus the result binding's own wrapper at scope exit.
///
/// Not reachable by the run-vs-build parity oracle in the other direction
/// either: memory stays balanced (the bodies walk frees nothing by
/// construction), so an ASAN/LSan corpus cannot see this class at all. The
/// output pin is the only signal.
///
/// The `Drop` body READS `name` for the reason B-2026-08-09-15's pin gives:
/// printing the id alone leaves the payload buffer unobserved and LLVM
/// deletes the allocation with its frees, so a memory defect underneath
/// could still pass an output-only assertion.
#[test]
fn test_e2e_returned_option_arg_payload_drop_fires_once() {
    let hdr = "struct Res { id: i64, name: String }\n\
                   impl Drop for Res {\n\
                   \x20   fn drop(mut ref self) { println(f\"drop {self.id} {self.name}\") }\n\
                   }\n";
    // `return` out of the arm — the spelling B-2026-08-09-15 pins for enums.
    assert_eq!(
        run_program(&format!(
            "{hdr}\
                 fn take(b: Option[Res]) -> Res {{\n\
                 \x20   match b {{\n\
                 \x20       Some(r) => {{ return r; }}\n\
                 \x20       None => {{ return Res {{ id: 0, name: f\"z\" }}; }}\n\
                 \x20   }}\n\
                 }}\n\
                 fn main() {{\n\
                 \x20   let b: Option[Res] = Some(Res {{ id: 7, name: f\"e7\" }});\n\
                 \x20   let r: Res = take(b);\n\
                 \x20   println(f\"got {{r.id}}\");\n\
                 }}"
        ))
        .as_deref(),
        Some("got 7\ndrop 7 e7\n")
    );
    // BLOCK-TAIL spelling of the same escape — the arm yields the binding
    // rather than returning it. This is the shape the row reduced to, and
    // it is a different code path from `return` (an arm tail is a Block,
    // which is why B-2026-08-28-66's neutralizer family keys on it).
    assert_eq!(
            run_program(&format!(
                "{hdr}\
                 fn take(b: Option[Res]) -> Res {{\n\
                 \x20   match b {{ Some(r) => {{ r }} None => {{ Res {{ id: 0, name: f\"z\" }} }} }}\n\
                 }}\n\
                 fn main() {{\n\
                 \x20   let b: Option[Res] = Some(Res {{ id: 7, name: f\"e7\" }});\n\
                 \x20   let r: Res = take(b);\n\
                 \x20   println(f\"got {{r.id}}\");\n\
                 }}"
            ))
            .as_deref(),
            Some("got 7\ndrop 7 e7\n")
        );
    // `Result` carries the identical defect through its own
    // `__karac_dropelems_res_*` walker, and the widened gate admits it by
    // the same name test — pinned so a future narrowing cannot drop the
    // `Result` half while keeping `Option`.
    assert_eq!(
        run_program(&format!(
            "{hdr}\
                 fn take(b: Result[Res, i64]) -> Res {{\n\
                 \x20   match b {{\n\
                 \x20       Ok(r) => {{ return r; }}\n\
                 \x20       Err(_) => {{ return Res {{ id: 0, name: f\"z\" }}; }}\n\
                 \x20   }}\n\
                 }}\n\
                 fn main() {{\n\
                 \x20   let b: Result[Res, i64] = Result.Ok(Res {{ id: 7, name: f\"e7\" }});\n\
                 \x20   let r: Res = take(b);\n\
                 \x20   println(f\"got {{r.id}}\");\n\
                 }}"
        ))
        .as_deref(),
        Some("got 7\ndrop 7 e7\n")
    );
}

/// B-2026-08-30-21 — a `let` bound to an ASSOCIATED call binds the callee's
/// RETURN type, not the type the callee is declared on.
///
/// The UFCS arm of the let-binding type hint read `segments[0]` — the
/// HOLDER — and accepted it whenever the holder's LLVM shape matched the
/// value's. That is a CONSTRUCTOR assumption (`Type.new()` does return
/// `Type`); for `impl H { fn id(a: R) -> R }` the two are unrelated, so the
/// shape test was a coincidence filter. When it passed, the binding was
/// recorded as `H` and a field read died on the loud "cannot resolve field
/// 'id' on this receiver"; when it failed, nothing was recorded and the
/// binding's own `Drop` body was silently lost.
///
/// THE ROW CALLED THE GATE "ALL-SCALAR", AND IT IS NOT. Measured: the gate
/// is an LLVM-SHAPE COLLISION between the returned struct and any other
/// registered struct. `R { id: i64 }` fails beside `H { n: i64 }` and
/// SUCCEEDS beside `H { n: f64 }` — same `R`, same all-scalar-ness. A
/// two-scalar `R` fails just as hard when `H` matches its shape, and an
/// `f64` pair fails too, so it is neither about scalars nor about field
/// count. The row's "give `R` a heap field and it vanishes" worked only
/// because that changed `R`'s shape away from `H`'s. The `collide-*` cases
/// below are the ones that pin this; `distinct-shape` is why they are not
/// simply "assoc calls are broken".
///
/// Compiled expectations only, stated as the FREE-FUNCTION oracle's answer.
/// When this landed, `--interp` ran an EXTRA body for the shape
/// (B-2026-08-30-22), so twinning would have pinned that defect's output;
/// -22 is fixed now and all four surfaces agree, but the expectations stay
/// written as the compiled oracle's because that is what this row is about.
/// `tests/interpreter.rs`'s `test_assoc_fn_passthrough_arg_runs_one_drop_body`
/// is where the interpreter half is pinned.
#[test]
fn e2e_assoc_call_result_binds_its_return_type() {
    let drop_impl = "impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n";
    let cases: [(&str, String, &str); 5] = [
        // Symptom 1: a HARD build failure before the fix.
        (
            "collide-field-read",
            format!(
                "struct R {{ id: i64 }}\n{drop_impl}\
                     struct H {{ n: i64 }}\n\
                     impl H {{ fn id(a: R) -> R {{ return a }} }}\n\
                     fn main() {{ let x = H.id(R {{ id: 1 }}); println(f\"x={{x.id}}\") }}"
            ),
            "x=1\ndR1\n",
        ),
        // Symptom 2: SILENT — no body at all before the fix.
        (
            "collide-lost-body",
            format!(
                "struct R {{ id: i64 }}\n{drop_impl}\
                     struct H {{ n: i64 }}\n\
                     impl H {{ fn id(a: R) -> R {{ return a }} }}\n\
                     fn main() {{ let x = H.id(R {{ id: 1 }}); println(\"mid\") }}"
            ),
            "dR1\nmid\n",
        ),
        // Same `R`, holder shape changed: this ALWAYS worked, and is what
        // proves the gate is the collision rather than anything about `R`.
        (
            "distinct-shape",
            format!(
                "struct R {{ id: i64 }}\n{drop_impl}\
                     struct H {{ n: f64 }}\n\
                     impl H {{ fn id(a: R) -> R {{ return a }} }}\n\
                     fn main() {{ let x = H.id(R {{ id: 1 }}); println(f\"x={{x.id}}\") }}"
            ),
            "x=1\ndR1\n",
        ),
        // Two scalars, colliding: kills the "all-scalar"/"single-field"
        // readings in one line.
        (
            "collide-two-scalars",
            format!(
                "struct R {{ id: i64, k: i64 }}\n{drop_impl}\
                     struct H {{ p: i64, q: i64 }}\n\
                     impl H {{ fn id(a: R) -> R {{ return a }} }}\n\
                     fn main() {{ let x = H.id(R {{ id: 1, k: 2 }}); println(f\"x={{x.id}}\") }}"
            ),
            "x=1\ndR1\n",
        ),
        // The oracle, and the CONSTRUCTOR case the old holder heuristic
        // existed for — `P.new() -> P` must keep resolving.
        (
            "constructor-still-resolves",
            format!(
                "struct R {{ id: i64 }}\n{drop_impl}\
                     struct P {{ v: i64 }}\n\
                     impl P {{ fn new(v: i64) -> P {{ return P {{ v: v }} }} }}\n\
                     fn main() {{ let p = P.new(5); println(f\"v={{p.v}}\") }}"
            ),
            "v=5\n",
        ),
    ];
    for (label, src, want) in cases {
        let Some(out) = run_program(&src) else {
            return;
        };
        assert_eq!(
            out, want,
            "[{label}] assoc-call result bound the wrong type"
        );
    }
}

/// B-2026-08-29-7 — a `match` arm over an owned `Option[T]` param that
/// REBINDS its payload to a local and lets that local escape is a DOUBLE
/// FREE on both compiled backends, on a program `--interp` runs correctly.
///
/// The payload here is 4 words against `Option`'s 3-word area, so it is
/// heap-BOXED, and the box's owner is the CALLER. B-2026-08-06-10's mirror
/// zeroes the box's interior alongside the escaping value's own copy, which
/// is what stops the caller's `__karac_drop_struct_Res(box)` from freeing a
/// buffer the return value carried away. That mirror is keyed by SLOT, and
/// `let k = r;` gives the escaping value a NEW slot the map had never heard
/// of — so the box kept a live `{ptr,len,cap}` and was freed twice.
///
/// The ASAN fixture
/// `asan_rebound_boxed_option_payload_returned_is_owned_once` is the memory
/// half; this is the OUTPUT half, and it is not redundant with it. A
/// double free aborts the process, so `run_program` returns `None` and a
/// `.as_deref()` comparison against `Some(..)` fails loudly — but the
/// BODY-COUNT question (exactly one `drop 7 e7`) is only visible here.
#[test]
fn test_e2e_rebound_boxed_option_payload_returned_drops_once() {
    let hdr = "struct Res { id: i64, name: String }\n\
                   impl Drop for Res {\n\
                   \x20   fn drop(mut ref self) { println(f\"drop {self.id} {self.name}\") }\n\
                   }\n";
    // `return` out of the rebind.
    assert_eq!(
        run_program(&format!(
            "{hdr}\
                 fn take(b: Option[Res]) -> Res {{\n\
                 \x20   match b {{\n\
                 \x20       Some(r) => {{ let k: Res = r; return k; }}\n\
                 \x20       None => {{ return Res {{ id: 0, name: f\"z\" }}; }}\n\
                 \x20   }}\n\
                 }}\n\
                 fn main() {{\n\
                 \x20   let b: Option[Res] = Some(Res {{ id: 7, name: f\"e7\" }});\n\
                 \x20   let r: Res = take(b);\n\
                 \x20   println(f\"got {{r.id}}\");\n\
                 }}"
        ))
        .as_deref(),
        Some("got 7\ndrop 7 e7\n")
    );
    // TWO levels of rebind — the propagation has to survive a chain, since
    // each `let` re-keys the box pointer onto the next slot.
    assert_eq!(
        run_program(&format!(
            "{hdr}\
                 fn take(b: Option[Res]) -> Res {{\n\
                 \x20   match b {{\n\
                 \x20       Some(r) => {{ let k: Res = r; let m: Res = k; return m; }}\n\
                 \x20       None => {{ return Res {{ id: 0, name: f\"z\" }}; }}\n\
                 \x20   }}\n\
                 }}\n\
                 fn main() {{\n\
                 \x20   let b: Option[Res] = Some(Res {{ id: 7, name: f\"e7\" }});\n\
                 \x20   let r: Res = take(b);\n\
                 \x20   println(f\"got {{r.id}}\");\n\
                 }}"
        ))
        .as_deref(),
        Some("got 7\ndrop 7 e7\n")
    );
    // GUARD RAIL — the same arm WITHOUT the rebind was always correct, and
    // has to stay that way: the propagation must not disturb the shape
    // whose escaping value already IS the registered slot.
    assert_eq!(
        run_program(&format!(
            "{hdr}\
                 fn take(b: Option[Res]) -> Res {{\n\
                 \x20   match b {{\n\
                 \x20       Some(r) => {{ return r; }}\n\
                 \x20       None => {{ return Res {{ id: 0, name: f\"z\" }}; }}\n\
                 \x20   }}\n\
                 }}\n\
                 fn main() {{\n\
                 \x20   let b: Option[Res] = Some(Res {{ id: 7, name: f\"e7\" }});\n\
                 \x20   let r: Res = take(b);\n\
                 \x20   println(f\"got {{r.id}}\");\n\
                 }}"
        ))
        .as_deref(),
        Some("got 7\ndrop 7 e7\n")
    );
}

/// B-2026-08-28-72's guard rail — the `Option` shapes whose payload does
/// NOT escape the callee must keep firing caller-side exactly once.
///
/// The twin of `test_e2e_nonescaping_enum_arg_payload_still_fires_once`,
/// and it exists for the same reason: the widened gate keys on "does the
/// callee return something bound out of this arg", and an unconditional
/// retraction would silence every case here. A lost `Drop` body reads as a
/// passing test unless something pins the output.
#[test]
fn test_e2e_nonescaping_option_arg_payload_still_fires_once() {
    let hdr = "struct Res { id: i64, name: String }\n\
                   impl Drop for Res {\n\
                   \x20   fn drop(mut ref self) { println(f\"drop {self.id}\") }\n\
                   }\n";
    // Payload bound out but NOT returned — the caller-retains fire is the
    // only one, and it must survive the widened gate.
    assert_eq!(
        run_program(&format!(
            "{hdr}\
                 fn take(b: Option[Res]) -> i64 {{\n\
                 \x20   match b {{ Some(r) => {{ return r.id; }} None => {{ return 0; }} }}\n\
                 }}\n\
                 fn main() {{\n\
                 \x20   let b: Option[Res] = Some(Res {{ id: 7, name: f\"e7\" }});\n\
                 \x20   let v: i64 = take(b);\n\
                 \x20   println(f\"v={{v}}\");\n\
                 }}"
        ))
        .as_deref(),
        Some("drop 7\nv=7\n")
    );
    // Param NEVER matched — nothing binds the payload out at all.
    assert_eq!(
        run_program(&format!(
            "{hdr}\
                 fn take(b: Option[Res]) -> i64 {{ println(f\"in\"); 1 }}\n\
                 fn main() {{\n\
                 \x20   let b: Option[Res] = Some(Res {{ id: 7, name: f\"e7\" }});\n\
                 \x20   let v: i64 = take(b);\n\
                 \x20   println(f\"end {{v}}\");\n\
                 }}"
        ))
        .as_deref(),
        Some("in\ndrop 7\nend 1\n")
    );
}

#[test]
fn test_e2e_mut_ref_option_shared_writeback() {
    // B-2026-07-12-3 — a reassignment through a `mut ref Option[shared]`
    // parameter (`slot = Some(n)`) did not propagate back to the caller
    // under codegen: the store landed in the param's local pointer-slot
    // alloca instead of through the borrow pointer, so the caller kept the
    // pre-call value (a silent stale `None`, SIGSEGV downstream when it was
    // later unwrapped). Now the Assign arm routes the ARC retain/release
    // store through `get_data_ptr` for `mut ref Option[shared]` params.
    // Covers: a single-level write into a `None` slot, and a second write
    // OVER an existing `Some` (exercising the old-inner release). Both
    // must equal the interpreter and be leak-clean (verified separately
    // under valgrind: all heap blocks freed).
    if let Some(out) = run_program(
        "shared struct Node { mut val: i64, mut next: Option[Node] }\n\
             fn setit(prev: mut ref Option[Node], n: Node) { prev = Some(n); }\n\
             fn main() {\n\
                 let mut cur: Option[Node] = Some(Node { val: 1, next: None });\n\
                 let a = Node { val: 2, next: None };\n\
                 setit(mut cur, a);\n\
                 let b = Node { val: 3, next: None };\n\
                 setit(mut cur, b);\n\
                 match cur {\n\
                     None => { println(\"none\"); }\n\
                     Some(p) => { println(f\"val: {p.val}\"); }\n\
                 }\n\
             }",
    ) {
        assert_eq!(out, "val: 3\n");
    }
}

#[test]
fn test_e2e_f32_option_payload_pack_unpack() {
    // B-2026-07-20-11 — `coerce_to_i64` packed an f32 into an enum
    // payload word via a DIRECT f32→i64 bitcast (invalid IR: a float↔int
    // bitcast requires equal widths), and three unpack sites
    // (`or.pl.fc`, `pl.fc`, `pl.sub.fc`) did the i64→f32 inverse. Any
    // f32-carrying enum payload failed module verification at build; the
    // original trigger was a fused f32 `row.zip_with(row, f).sum()` over
    // an `iter_axis` row-view (whose element is Option-wrapped). f32 now
    // routes through its 32-bit pattern (bitcast↔i32 + zext/trunc), the
    // packing the f64-vs-f32 unpack arms (`pat.f32.*`, `col.f32bits`)
    // always expected. Covers: Option[f32], Option[struct{f32,f32}]
    // (the pl.fc/pl.sub.fc field paths), Result[f32, String], an f32
    // iter reduce, and the original fused row-view repro.
    if let Some(out) = run_program(
            "struct P { x: f32, y: f32 }\n\
             fn pick(flag: bool) -> Option[f32] {\n\
                 if flag { Some(2.5f32) } else { None }\n\
             }\n\
             fn wrap(flag: bool) -> Option[P] {\n\
                 if flag { Some(P { x: 1.25f32, y: -3.5f32 }) } else { None }\n\
             }\n\
             fn halve(v: f32) -> Result[f32, String] {\n\
                 if v > 0.0f32 { Ok(v / 2.0f32) } else { Err(\"neg\".to_string()) }\n\
             }\n\
             fn scan[N, D](corpus: ref Tensor[f32, [N, D]]) -> Vec[f32] {\n\
                 let mut out: Vec[f32] = Vec.new();\n\
                 for row in corpus.iter_axis(0) {\n\
                     let d = row.zip_with(row, |x, y| x * y).sum();\n\
                     out.push(d);\n\
                 }\n\
                 out\n\
             }\n\
             fn main() {\n\
                 match pick(true) { Some(v) => println(v), None => println(-1.0f32) }\n\
                 match wrap(true) { Some(p) => { println(p.x); println(p.y) }, None => println(-1.0f32) }\n\
                 match halve(5.0f32) { Ok(v) => println(v), Err(e) => println(e) }\n\
                 let vs: Vec[f32] = Vec[0.5f32, 1.5f32, 2.5f32];\n\
                 match vs.iter().reduce(|a, x| a + x) { Some(s) => println(s), None => println(-1.0f32) }\n\
                 let c: Tensor[f32, [2, 2]] = Tensor.from([[1.0f32, 2.0f32], [3.0f32, 4.0f32]]);\n\
                 let r = scan(c);\n\
                 match r.get(0) { Some(v) => println(v), None => println(-1.0f32) }\n\
                 match r.get(1) { Some(v) => println(v), None => println(-1.0f32) }\n\
             }",
        ) {
            assert_eq!(out, "2.5\n1.25\n-3.5\n2.5\n4.5\n5\n25\n");
        }
}

#[test]
fn test_e2e_single_field_struct_option_payload_sizing() {
    // #49 (phase-12 self-hosting, found while minimizing #48): a struct
    // whose ONLY field is an `Option[T]` (`struct Block { tail: Option[Expr] }`),
    // used as a shared-enum variant payload (`Expr.Blk(Block)`), SIGSEGV'd.
    // `payload_word_count_for_type_expr` routes the `Option` field through
    // the enum-in-enum carve-out (returns 1, not Option's real 4-word LLVM
    // width), so the variant's payload AREA is 1 word — which is fine on its
    // own (multi-field `Block`s with the same undercount still heap-box,
    // because their real width still exceeds the area). The actual bug was
    // in `coerce_to_payload_words`: with `num_words == 1` it took the scalar
    // fast path and called `coerce_to_i64` on the 4-word `Block` value, which
    // recursed into field 0 (the multi-field Option sub-struct) and collapsed
    // to `0` — silently dropping the payload. The unpack/drop sites
    // independently compute `llvm_type_word_count(T) > area` and treat the
    // payload as BOXED, so they `inttoptr` that `0` → null deref → SIGSEGV.
    // The fix guards the fast path on the value's real width: a wide-but-
    // undersized payload falls through to the decompose-and-box path, which
    // is exactly what unpack and drop expect — all three sites coherent. The
    // multi-field `Block` (`Vec[Stmt]` + `Option[Expr]` + `Span`) already
    // boxed and is the regression peer (`test_e2e_owned_struct_option_shared_field_captured_from_builder`).
    if let Some(out) = run_program(
        "shared enum Expr { Num(i64), Blk(Block), Error }\n\
             struct Block { tail: Option[Expr] }\n\
             fn render_block(b: Block) -> String {\n\
                 let Block { tail } = b;\n\
                 match tail { Some(e) => render_expr(e), None => \"no-tail\".to_string() }\n\
             }\n\
             fn render_expr(e: Expr) -> String {\n\
                 match e {\n\
                     Num(n) => n.to_string(),\n\
                     Blk(b) => render_block(b),\n\
                     Error => \"error\".to_string(),\n\
                 }\n\
             }\n\
             fn main() {\n\
                 let blk = Block { tail: Some(Expr.Num(7)) };\n\
                 println(render_expr(Expr.Blk(blk)));\n\
             }",
    ) {
        assert_eq!(out, "7\n");
    }
}

#[test]
fn e2e_try_extend_alias_codegen() {
    // `Vec.try_extend` — the spelling design.md § Fallible Allocation's
    // method table names (`extend(iter)` / `try_extend(iter)`), which did
    // not exist until B-2026-08-25-20. It shares the
    // `try_extend_from_slice` lowering, so the assertion is EQUIVALENCE:
    // both spellings run in one program over identical inputs and every
    // observable has to match. A drift in either arm breaks this.
    //
    // Deliberately spelled with `match`, not `?` in `main`: the latter
    // segfaults under `karac run` for any error enum with an inline
    // scalar payload, `AllocError` included (B-2026-08-25-33) — a
    // pre-existing JIT defect this method merely inherits.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let src: Vec[i64] = Vec.filled(4_i64, 5_i64);\n\
                 let mut a: Vec[i64] = Vec.with_capacity(2);\n\
                 a.push(1_i64);\n\
                 let mut b: Vec[i64] = Vec.with_capacity(2);\n\
                 b.push(1_i64);\n\
                 match a.try_extend(src) {\n\
                     Ok(_) => println(\"ok\"),\n\
                     Err(_) => println(\"err\"),\n\
                 }\n\
                 match b.try_extend_from_slice(src) {\n\
                     Ok(_) => println(\"ok\"),\n\
                     Err(_) => println(\"err\"),\n\
                 }\n\
                 println(a.len());\n\
                 println(b.len());\n\
                 println(a[0]);\n\
                 println(a[4]);\n\
                 println(b[4]);\n\
             }",
    ) {
        assert_eq!(out, "ok\nok\n5\n5\n1\n5\n5\n");
    }
}

#[test]
fn e2e_question_multiword_ok_payload_codegen() {
    // The `?` operator must reconstruct a multi-word Ok payload from ALL its
    // words, not just the first. Previously `?` returned only `w0`, so a
    // 3-word `Vec`/`String` lost its `len`/`cap` and crashed on use. Covers
    // `Result[String, _]?` and `Result[Vec[T], _]?` (via try_from_slice).
    if let Some(out) = run_program(
        "fn s() -> Result[i64, AllocError] {\n\
                 let r: Result[String, AllocError] = Ok(\"hello\");\n\
                 let st: String = r?;\n\
                 Ok(st.len())\n\
             }\n\
             fn v() -> Result[i64, AllocError] {\n\
                 let src: Vec[i64] = Vec.filled(2_i64, 5_i64);\n\
                 let vv: Vec[i64] = Vec.try_from_slice(src)?;\n\
                 Ok(vv.len())\n\
             }\n\
             fn main() {\n\
                 match s() { Ok(n) => println(n), Err(_) => println(\"e\") }\n\
                 match v() { Ok(n) => println(n), Err(_) => println(\"e\") }\n\
             }",
    ) {
        assert_eq!(out, "5\n2\n");
    }
}

#[test]
fn e2e_question_struct_payload_survives_a_span_of_its_own() {
    // B-2026-08-18-9. `question_ok_payload_types` is WRITTEN by the
    // typechecker at the `?` node's span and READ by
    // `reconstruct_question_ok_payload`, which used to read the OPERAND's
    // span instead. Those were the same key only because the parser handed
    // `ExprKind::Question` a verbatim copy of its operand's span; giving
    // `?` a span of its own (so nested sites stop colliding) moved the
    // producer off the consumer's key.
    //
    // This is the shape that caught it: a multi-word struct payload. The
    // miss fell through to the `w0`-truncating fallback and the binding's
    // fields stopped resolving under codegen ("cannot resolve field
    // 'name'") while `--interp` stayed correct — so the pin has to be an
    // E2E, not a typechecker test. Both legs run: `Some` through the
    // success path, `None` through the propagation path.
    if let Some(out) = run_program(
            "struct Big { name: String, n: i64 }\n             fn mk(n: i64) -> Option[Big] {\n                 if n < 0 { return None; }\n                 return Some(Big { name: \"widget\", n: n });\n             }\n             fn use_it(n: i64) -> Option[String] {\n                 let b = mk(n)?;\n                 return Some(b.name + \"-\" + b.n.to_string());\n             }\n             fn main() {\n                 match use_it(7) { Some(s) => println(s), None => println(\"none\") }\n                 match use_it(-1) { Some(s) => println(s), None => println(\"none\") }\n             }",
        ) {
            assert_eq!(out, "widget-7\nnone\n");
        }
}

#[test]
fn e2e_nested_question_sites_do_not_share_a_span_key() {
    // The reason `?` needed a span of its own (B-2026-08-18-9): every node
    // copied `lhs.span`, so an inner `?` and the outer `?` enclosing it
    // landed on ONE SpanKey and the outer's recorded payload type
    // overwrote the inner's. Here the two differ — inner unwraps
    // `Option[Big]` (multi-word struct), outer unwraps `Option[String]`
    // (3-word) — so a collision rebuilds one of them at the other's
    // layout instead of merely being untidy.
    if let Some(out) = run_program(
            "struct Big { name: String, n: i64 }\n             fn mk(n: i64) -> Option[Big] {\n                 if n < 0 { return None; }\n                 return Some(Big { name: \"w\", n: n });\n             }\n             fn label(b: Big) -> Option[String] {\n                 if b.n == 0 { return None; }\n                 return Some(b.name + b.n.to_string());\n             }\n             fn go(n: i64) -> Option[String] {\n                 let s = label(mk(n)?)?;\n                 return Some(s + \"!\");\n             }\n             fn main() {\n                 match go(3) { Some(s) => println(s), None => println(\"none\") }\n                 match go(0) { Some(s) => println(s), None => println(\"none\") }\n                 match go(-1) { Some(s) => println(s), None => println(\"none\") }\n             }",
        ) {
            assert_eq!(out, "w3!\nnone\nnone\n");
        }
}

#[test]
fn e2e_try_clone_question_codegen() {
    // `?`-form: `let c: Vec[i64] = v.try_clone()?` unwraps the cloned Vec
    // out of the `Result`. Exercises the multi-word `?` Ok-payload
    // reconstruction (the 3-word Vec unwrapped by `?`) on top of try_clone.
    if let Some(out) = run_program(
        "fn dup() -> Result[i64, AllocError] {\n\
                 let mut v: Vec[i64] = Vec.new();\n\
                 v.push(3_i64);\n\
                 v.push(4_i64);\n\
                 let c: Vec[i64] = v.try_clone()?;\n\
                 Ok(c[0] + c[1])\n\
             }\n\
             fn main() {\n\
                 match dup() { Ok(n) => println(n), Err(_) => println(\"err\") }\n\
             }",
    ) {
        assert_eq!(out, "7\n");
    }
}

#[test]
fn e2e_try_clone_vecdeque_codegen() {
    // `VecDeque[i64].try_clone()` rides the Vec fallible-clone arm (shared
    // `{ptr,len,cap}` storage). The VecDeque payload round-trips through
    // Result.Ok match-extraction (VecDeque-in-enum reconstruction).
    if let Some(out) = run_program(
        "fn main() {\n\
                 let mut q: VecDeque[i64] = VecDeque.new();\n\
                 q.push_back(1_i64);\n\
                 q.push_back(2_i64);\n\
                 match q.try_clone() {\n\
                     Ok(c) => { println(c.len()); println(c[0]); println(c[1]); }\n\
                     Err(_) => println(\"err\"),\n\
                 }\n\
             }",
    ) {
        assert_eq!(out, "2\n1\n2\n");
    }
}

#[test]
fn e2e_builtin_enum_eq_option_result() {
    // Built-in enum `==` is sound in codegen too (None/Ok unit + payload
    // words). Regression guard for the zero-init enum construction.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let a: Option[i64] = Some(1);\n\
                 let b: Option[i64] = Some(1);\n\
                 let c: Option[i64] = None;\n\
                 let d: Option[i64] = None;\n\
                 println(f\"{a == b}\");\n\
                 println(f\"{a == c}\");\n\
                 println(f\"{c == d}\");\n\
                 let r: Result[i64, i64] = Ok(5);\n\
                 let s: Result[i64, i64] = Ok(5);\n\
                 println(f\"{r == s}\");\n\
             }",
    ) {
        assert_eq!(out, "true\nfalse\ntrue\ntrue\n");
    }
}

#[test]
fn test_e2e_option_result_unwrap_or() {
    // B-2026-06-11-10: `unwrap_or(default)` on Option/Result. The bug
    // report mis-scoped this as a non-identifier-receiver dispatch gap; it
    // was actually unimplemented across typecheck + interp + codegen. This
    // pins the receiver-shape-agnostic codegen path: a chained call-result
    // receiver (`m.get(k).unwrap_or(d)` — the original repro), a plain
    // Option identifier, a width-mismatched default (i64 literal into an
    // `i32` payload, exercising the codegen width-coerce), a `Result`, and
    // a heap `String` payload (3-word reconstitution + default).
    let output = run_program(
        "fn rstr(o: Result[String, i64]) -> String { o.unwrap_or(\"fb\") }\n\
             fn main() {\n\
                 let mut m: Map[i64, i64] = Map.new();\n\
                 m.insert(1, 2);\n\
                 println(m.get(1).unwrap_or(0));\n\
                 println(m.get(9).unwrap_or(-1));\n\
                 let a: Option[i64] = Some(5);\n\
                 let b: Option[i64] = None;\n\
                 println(a.unwrap_or(0));\n\
                 println(b.unwrap_or(7));\n\
                 let mut n: Map[i64, i32] = Map.new();\n\
                 n.insert(3, 30);\n\
                 println(n.get(8).unwrap_or(-9));\n\
                 println(rstr(Result.Ok(\"got\")));\n\
                 println(rstr(Result.Err(404)));\n\
                 let s: Option[String] = None;\n\
                 println(s.unwrap_or(\"def\"));\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "2\n-1\n5\n7\n-9\ngot\nfb\ndef\n");
}

#[test]
fn test_e2e_result_unwrap_err_expect_err() {
    // B-2026-07-09-10: `Result::unwrap_err()` / `expect_err()` — the
    // Err-extracting, Ok-panicking mirrors of `unwrap`/`expect`. Codegen
    // panics on the Ok tag (tag == 1) and reconstitutes the Err payload
    // (inner type E, from `method_unwrap_inner_types`), sharing the
    // extract-or-panic shape with `unwrap`. Covers a scalar `Err(i64)`, a
    // heap `Err(String)` on a fresh-temp receiver (the common `f().unwrap_err()`
    // shape), and `expect_err`. `unwrap` on the Ok side stays correct.
    let output = run_program(
            "fn parse(ok: bool) -> Result[i64, i64] {\n\
             \x20   if ok { Result.Ok(42) } else { Result.Err(99) }\n\
             }\n\
             fn emsg(ok: bool) -> Result[i64, String] {\n\
             \x20   if ok { Result.Ok(1) } else { Result.Err(\"the error message here\".to_string()) }\n\
             }\n\
             fn main() {\n\
             \x20   println(parse(true).unwrap());\n\
             \x20   println(parse(false).unwrap_err());\n\
             \x20   println(emsg(false).unwrap_err());\n\
             \x20   println(emsg(false).expect_err(\"wanted an error\"));\n\
             }",
        )
        .expect("compile + run failed");
    assert_eq!(
        output,
        "42\n99\nthe error message here\nthe error message here\n"
    );
}

#[test]
fn test_e2e_ambient_env_var_question_mark_propagates_as_io_error() {
    // `env.var(x)?` in a `Result[_, IoError]`-returning fn desugars to a
    // match whose Err arm calls `IoError.from(varErr)` via the stdlib
    // `impl From for IoError { fn from(VarError) }` (runtime/stdlib/io.kara)
    // — that conversion impl resolves on the codegen path, so the missing
    // key propagates out as an `IoError` and the caller's match takes the
    // Err arm. Pins that the slice-3a Result construction composes with
    // `?`-propagation + cross-error-type `From` conversion (the idiomatic
    // usage), not just the direct-match form.
    let out = run_program(
        r#"
fn read_it() -> Result[String, IoError] reads(Env) {
    let s: String = env.var("__KARAC_E2E_QMARK_NO_SUCH__")?;
    Ok(s)
}
fn main() reads(Env) {
    match read_it() {
        Ok(v) => { println(v); }
        Err(_) => { println("io-err"); }
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "io-err");
    }
}

#[test]
fn test_e2e_main_result_question_and_returns() {
    // B-2026-06-12-9: `main() -> Result[(), E]` used to fail module
    // verification — a `?` early-return (and explicit `return Ok/Err`)
    // emitted the `{tag, …}` Result aggregate against `main`'s `i32` C
    // signature. The fix adapts every Result-returning site in `main` to a
    // process exit code per design.md § Entry Point: `Ok` exits 0, `Err(e)`
    // prints `Error: {e}\n` to stderr (via E's Display) and exits 1.

    // (a) `?` success path — body continues, `Ok(())` tail exits 0.
    let ok = run_program_capturing(
        r#"
#[derive(Display)]
enum MyErr { Bad }
fn helper(x: i64) -> Result[i64, MyErr] {
    if x < 0 { return Err(MyErr.Bad); }
    Ok(x + 1)
}
fn main() -> Result[(), MyErr] {
    let v = helper(5)?;
    println(v);
    Ok(())
}
"#,
    );
    if let Some(cap) = ok {
        assert_eq!(cap.stdout.trim(), "6");
        assert_eq!(
            cap.status.code(),
            Some(0),
            "Ok path must exit 0; stderr={:?}",
            cap.stderr
        );
    }

    // (b) `?` error-propagation path — Err exits 1 with `Error: <Display>`
    // on stderr (a unit enum renders as its bare variant name).
    let q_err = run_program_capturing(
        r#"
#[derive(Display)]
enum MyErr { Bad }
fn helper(x: i64) -> Result[i64, MyErr] {
    if x < 0 { return Err(MyErr.Bad); }
    Ok(x + 1)
}
fn main() -> Result[(), MyErr] {
    let v = helper(0 - 5)?;
    println(v);
    Ok(())
}
"#,
    );
    if let Some(cap) = q_err {
        assert_eq!(
            cap.status.code(),
            Some(1),
            "Err path must exit 1; stdout={:?} stderr={:?}",
            cap.stdout,
            cap.stderr
        );
        assert!(
            cap.stderr.contains("Error: Bad"),
            "expected 'Error: Bad' on stderr, got: {:?}",
            cap.stderr
        );
        assert!(
            cap.stdout.trim().is_empty(),
            "Err path must not print the unwrapped value to stdout: {:?}",
            cap.stdout
        );
    }

    // (c) explicit `return Err(struct)` — struct Display, stderr/stdout
    // split, exit 1. Prior stdout work (`println` before the return) is
    // still observable.
    let ret_err = run_program_capturing(
        r#"
#[derive(Display)]
struct IoError { code: i64 }
fn main() -> Result[(), IoError] {
    println("before");
    return Err(IoError { code: 42 });
}
"#,
    );
    if let Some(cap) = ret_err {
        assert_eq!(cap.status.code(), Some(1), "stderr={:?}", cap.stderr);
        assert_eq!(cap.stdout.trim(), "before");
        assert!(
            cap.stderr.contains("Error: IoError { code: 42 }"),
            "expected struct Display on stderr, got: {:?}",
            cap.stderr
        );
        assert!(
            !cap.stdout.contains("Error:"),
            "error line leaked onto stdout: {:?}",
            cap.stdout
        );
    }

    // (d) tail `Err` with a scalar error type — exits 1, `Error: 99`.
    let tail_err = run_program_capturing(
        r#"
fn pick(x: i64) -> Result[(), i64] {
    if x > 0 { return Ok(()); }
    Err(99)
}
fn main() -> Result[(), i64] {
    pick(0 - 1)
}
"#,
    );
    if let Some(cap) = tail_err {
        assert_eq!(cap.status.code(), Some(1), "stderr={:?}", cap.stderr);
        assert!(
            cap.stderr.contains("Error: 99"),
            "expected 'Error: 99' on stderr, got: {:?}",
            cap.stderr
        );
    }
}

#[test]
fn test_e2e_generic_struct_result_payload_recovery() {
    // B-2026-07-12-2 heap-recovery gap (the true blocker for the OnceLock
    // heap-`T` ungate, also a live silent miscompile for ordinary user
    // code): a concretely-instantiated GENERIC user-struct payload moved
    // out of a `Result`/`Option` used to silently miscompile — the mono
    // field layout (e.g. a 3-word `String`) collapsed to the all-`i64`
    // generic base's single word, so `Err(e) => e.field` read GARBAGE
    // (a raw pointer word), and `e.field.method()` hit the loud
    // `__field_elem_0` method-dispatch gap. Fixed by recording the concrete
    // instantiation `TypeExpr` at the payload binding (typechecker) and
    // recovering the mono width / field GEP at codegen. Pins field-move,
    // field-method, bind-first, multi-param, and the `Option` payload
    // shape. The borrow `Option[ref T]` path (Vec.first) must stay correct
    // — covered by `test_e2e_vec_get_first_option_ref_t_reread`.
    let out = run_program(
        r#"
struct Wrap[T] { val: T }
struct Pair[A, B] { a: A, b: B }
fn boom(x: i64) -> Result[i64, Wrap[String]] {
    if x > 0i64 { Ok(x) } else { Err(Wrap { val: "badness".to_string() }) }
}
fn opt(x: i64) -> Option[Wrap[String]] {
    if x > 0i64 { Some(Wrap { val: "opt".to_string() }) } else { None }
}
fn pair(x: i64) -> Result[i64, Pair[i64, String]] {
    if x > 0i64 { Ok(x) } else { Err(Pair { a: 7i64, b: "hi".to_string() }) }
}
fn main() {
    match boom(-1i64) { Ok(_) => { println("ok"); } Err(e) => { println(e.val); } }
    match boom(-1i64) { Ok(_) => { println("ok"); } Err(e) => { println(e.val.len().to_string()); } }
    match boom(-1i64) { Ok(_) => { println("ok"); } Err(e) => { let s = e.val; println(s.len().to_string()); } }
    match opt(1i64) { Some(w) => { println(w.val); } None => { println("none"); } }
    match pair(-1i64) { Ok(_) => { println("ok"); } Err(e) => { println(e.a.to_string()); println(e.b); } }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["badness", "7", "7", "opt", "7", "hi"]);
    }
}

#[test]
fn test_e2e_option_shared_walk_unwrap_cursor() {
    // Regression for TWO coupled walk-cursor refcount bugs
    // (2026-06-05, from the wip-shared-struct-codegen-followups
    // bug-#2 re-verification):
    //
    // 1. `Option[shared T]` variable-assign ordering: `cur =
    //    node.next` released the old inner BEFORE retaining the new
    //    one. When the new value is reachable through the old (the
    //    canonical list walk), the old head's drop freed the next
    //    node out from under the store — UAF / trap. Variable-assign
    //    sibling of the field-store fix (25442e73); same ARC setter
    //    rule (retain new → store → release old).
    // 2. `let node = cur.unwrap()` classified the MethodCall RHS as
    //    a fresh +1 source (`rhs_yields_fresh_ref`), skipping the
    //    receive-inc — but unwrap's lowering only re-extracts the
    //    payload words (a borrowing alias), while `track_rc_var`
    //    still queued the scope-exit dec. One over-dec per
    //    iteration.
    //
    // Pre-fix: trap before printing (the chain frees itself from
    // under the cursor on the first `cur = node.next`).
    let out = run_program(
        r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
fn make() -> Option[ListNode] {
    let mut head = ListNode { val: 1, next: None };
    let second = ListNode { val: 2, next: None };
    head.next = Some(second);
    Some(head)
}
fn main() {
    let mut cur = make();
    let mut sum = 0;
    while cur.is_some() {
        let node = cur.unwrap();
        sum = sum + node.val;
        cur = node.next;
    }
    println(sum);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "3");
    }
}

#[test]
fn test_e2e_option_shared_chain_through_call_result_arg() {
    // The wip-doc "bug #2" repro shape: a call-result
    // `Option[shared T]` temporary passed directly as an owned arg
    // (`ident(make())`), where the callee returns a chain aliasing
    // that param. The arg-side carries the callee's +1 directly into
    // the param slot; the Identifier-tail return retain (426b8dc3)
    // balances the callee's scope-exit dec. Pre-walk-fix this
    // printed garbage (the walk bugs corrupted the returned chain);
    // the let-bound workaround form is covered implicitly by the
    // suite's other walks.
    let out = run_program(
        r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
fn make() -> Option[ListNode] {
    let mut head = ListNode { val: 1, next: None };
    let second = ListNode { val: 2, next: None };
    head.next = Some(second);
    Some(head)
}
fn ident(head: Option[ListNode]) -> Option[ListNode] {
    head
}
fn main() {
    let mut cur = ident(make());
    let mut sum = 0;
    while cur.is_some() {
        let node = cur.unwrap();
        sum = sum + node.val;
        cur = node.next;
    }
    println(sum);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "3");
    }
}

#[test]
fn test_e2e_option_shared_prepend_builder_not_rc_boxed() {
    // Regression for the RC-fallback boxing / `Option[shared T]`
    // collision (2026-06-05). The ownership checker flags the
    // prepend-builder's `head` (captured into `ListNode { next:
    // head }`, then reassigned) for RC fallback; the let-site boxing
    // then redirected the binding's slot to a `{rc, Option}` heap
    // ptr — but every `var_option_shared_heap` path (Option-assign,
    // arg-share inc, scope-exit RcDecOption) addresses the slot as a
    // raw 4-word Option struct. The Option-assign arm smashed the
    // 8-byte slot with a 32-byte store (segfault); the tag reads
    // decoded a heap address as the discriminant. Option[shared]
    // bindings are now excluded from boxing (the inner node already
    // has its own RC discipline).
    //
    // MUST run through `run_program_with_ownership` — the plain
    // `run_program` passes no ownership result, so the RC-fallback
    // set is empty and the boxing path never fires.
    let out = run_program_with_ownership(
        r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
fn make(n: i64) -> Option[ListNode] {
    let mut head: Option[ListNode] = None;
    let mut i = 0;
    while i < n {
        let node = ListNode { val: i, next: head };
        head = Some(node);
        i = i + 1;
    }
    head
}
fn walk(head: Option[ListNode]) -> i64 {
    let mut cur = head;
    let mut sum = 0;
    while cur.is_some() {
        let node = cur.unwrap();
        sum = sum + node.val;
        cur = node.next;
    }
    sum
}
fn main() {
    let chain = make(100);
    println(walk(chain));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "4950");
    }
}

// ── Niche call ABI for `Option[shared T]` signatures ───────────
//
// wip-shared-struct-codegen-followups Slice 1 (2026-06-05): free user
// fns with `Option[shared T]` in return/param position are declared
// with a single nullable `ptr` at those positions (null = None)
// instead of the conventional 4-i64 Option enum struct — closing the
// field-niche/call-ABI asymmetry and skipping the sret round-trip per
// call. Bodies stay on the conventional shape: entry unpacks params,
// return sites pack, `compile_call` packs args / unpacks results.
// The method extension widened eligibility to impl methods — the
// `usercall`/`usermethod`/provider-vtable call paths pack/unpack via
// the shared helpers. Closures, generic monos, and coroutine ramps
// keep the conventional ABI (no `fn_niche_abi` entry).

#[test]
fn test_ir_option_shared_niche_abi_signature_shapes() {
    let ir = ir_for(
        r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
impl ListNode {
    fn step(ref self) -> Option[ListNode] { self.next }
}
fn make() -> Option[ListNode] {
    Some(ListNode { val: 1, next: None })
}
fn ident(head: Option[ListNode]) -> Option[ListNode] { head }
fn opt_i64(x: Option[i64]) -> Option[i64] { x }
fn main() {
    let a = ident(make());
    let b = opt_i64(Some(3));
    let n = ListNode { val: 2, next: None };
    let c = n.step();
    if a.is_some() { println(1); }
    if b.is_some() { println(2); }
    if c.is_none() { println(3); }
}
"#,
    );
    // Niche positions: ptr return + ptr param on the free fns.
    assert!(
        ir.contains("define internal ptr @ident(ptr"),
        "ident should be niche-shaped (ptr -> ptr); IR:\n{ir}"
    );
    assert!(
        ir.contains("define internal ptr @make()"),
        "make should be niche-shaped (-> ptr); IR:\n{ir}"
    );
    // Call sites use the niche shape.
    assert!(
        ir.contains("call ptr @make()"),
        "call to make should return ptr; IR:\n{ir}"
    );
    assert!(
        ir.contains("call ptr @ident(ptr"),
        "call to ident should pass+return ptr; IR:\n{ir}"
    );
    // Non-shared Option keeps the conventional 4-i64 struct ABI.
    assert!(
        ir.contains("@opt_i64({ i64, i64, i64, i64 }"),
        "Option[i64] param must stay conventional; IR:\n{ir}"
    );
    // Impl methods are niche-shaped too (method extension): `ref
    // self` lowers to ptr, the Option return to a nullable ptr.
    assert!(
        ir.contains("define internal ptr @ListNode.step(ptr"),
        "impl method should be niche-shaped (ptr self -> ptr ret); IR:\n{ir}"
    );
    assert!(
        !ir.contains("define internal { i64, i64, i64, i64 } @ListNode.step"),
        "impl method must not keep the conventional Option return; IR:\n{ir}"
    );
}

#[test]
fn test_e2e_option_shared_niche_abi_explicit_return_alias() {
    // Explicit-`return` compensation for aliased `Option[shared T]`
    // values (pre-existing soundness gap surfaced by this slice's
    // convergence tests; tail-position siblings were fixed by
    // 426b8dc3 / fca1e3ea). Three shapes, all of which under-counted
    // and returned freed memory before the fix:
    //   - `return head;`      (param identifier — trap on main)
    //   - `return node.next;` (field access — garbage sums)
    //   - `node.next` tail    (control: already worked, stays green)
    // The Return arm now routes through `compile_tail_final_expr`
    // (bare-binding inc, control-flow re-arm) plus the FieldAccess
    // companion `share_option_shared_field_ref_for_arg`.
    let out = run_program(
        r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
fn make(n: i64) -> Option[ListNode] {
    let mut head: Option[ListNode] = None;
    let mut i = n;
    while i > 0 {
        let node = ListNode { val: i, next: head };
        head = Some(node);
        i = i - 1;
    }
    head
}
fn ret_ident(head: Option[ListNode], flag: i64) -> Option[ListNode] {
    if flag == 1 {
        return head;
    }
    None
}
fn ret_field(head: Option[ListNode]) -> Option[ListNode] {
    if head.is_some() {
        let node = head.unwrap();
        return node.next;
    }
    return None;
}
fn tail_field(head: Option[ListNode]) -> Option[ListNode] {
    let node = head.unwrap();
    node.next
}
fn sum(head: Option[ListNode]) -> i64 {
    let mut total = 0;
    let mut cur = head;
    while cur.is_some() {
        let node = cur.unwrap();
        total = total + node.val;
        cur = node.next;
    }
    total
}
fn main() {
    println(sum(ret_ident(make(3), 1)));
    println(sum(ret_field(make(3))));
    println(sum(tail_field(make(3))));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "6\n5\n5");
    }
}

#[test]
fn test_e2e_option_shared_niche_abi_recursion() {
    // Recursion through the niche ABI: every recursive call packs
    // the field-loaded `node.next` arg to a ptr and unpacks the ptr
    // result; the explicit `return head;` / `return None;` exits
    // exercise the Return-arm compensation + niche packing together.
    let out = run_program(
        r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
fn make(n: i64) -> Option[ListNode] {
    let mut head: Option[ListNode] = None;
    let mut i = n;
    while i > 0 {
        let node = ListNode { val: i, next: head };
        head = Some(node);
        i = i - 1;
    }
    head
}
fn nth(head: Option[ListNode], k: i64) -> Option[ListNode] {
    if k == 0 {
        return head;
    }
    if head.is_none() {
        return None;
    }
    let node = head.unwrap();
    nth(node.next, k - 1)
}
fn sum(head: Option[ListNode]) -> i64 {
    let mut total = 0;
    let mut cur = head;
    while cur.is_some() {
        let node = cur.unwrap();
        total = total + node.val;
        cur = node.next;
    }
    total
}
fn main() {
    println(sum(nth(make(5), 3)));
    println(sum(nth(make(5), 0)));
    if nth(make(2), 5).is_none() { println(1); }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "9\n15\n1");
    }
}

#[test]
fn test_e2e_option_shared_question_operator_shared_let() {
    // `let first = head?;` — the `?` success path yields the payload
    // as the raw i64 word `q_w0`; a shared binding's slot must hold
    // the heap pointer. Pre-existing panic ("expected PointerValue")
    // at the let-site `into_pointer_value` on every karac build since
    // the `?` lowering landed; the let handler now int_to_ptr's the
    // word back when the binding is shared-typed. Also covers the
    // niche `?` early-return (None propagates as a null ptr from a
    // niche-shaped fn) and the success-path chain into `Some(...)`.
    let out = run_program(
        r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
fn make(n: i64) -> Option[ListNode] {
    let mut head: Option[ListNode] = None;
    let mut i = n;
    while i > 0 {
        let node = ListNode { val: i, next: head };
        head = Some(node);
        i = i - 1;
    }
    head
}
fn second(head: Option[ListNode]) -> Option[ListNode] {
    let first = head?;
    let rest = first.next?;
    Some(rest)
}
fn sum(head: Option[ListNode]) -> i64 {
    let mut total = 0;
    let mut cur = head;
    while cur.is_some() {
        let node = cur.unwrap();
        sum_step(node.val);
        total = total + node.val;
        cur = node.next;
    }
    total
}
fn sum_step(v: i64) {
    let _ = v;
}
fn main() {
    println(sum(second(make(3))));
    if second(make(1)).is_none() { println(7); }
    if second(None).is_none() { println(8); }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "5\n7\n8");
    }
}

#[test]
fn test_e2e_option_shared_method_return_composes_with_niche_fn() {
    // A method-returned `Option[shared T]` flows into a niche-shaped
    // free fn and out to a conventional walk — convergence of all
    // the ABI boundaries on one value.
    //
    // OWNED `self` receiver deliberately: this exact shape segfaulted
    // on the receiver-move bug (the usermethod dispatch passed the
    // stack-slot address where an owned-shared `self` expects the
    // heap pointer; the callee's receive-inc then incremented a stack
    // word) — fixed alongside the tail-zeroing retirement; the test
    // was pinned to `ref self` until then.
    let out = run_program(
        r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
impl ListNode {
    fn step(self) -> Option[ListNode] { self.next }
}
fn make(n: i64) -> Option[ListNode] {
    let mut head: Option[ListNode] = None;
    let mut i = n;
    while i > 0 {
        let node = ListNode { val: i, next: head };
        head = Some(node);
        i = i - 1;
    }
    head
}
fn ident(head: Option[ListNode]) -> Option[ListNode] { head }
fn sum(head: Option[ListNode]) -> i64 {
    let mut total = 0;
    let mut cur = head;
    while cur.is_some() {
        let node = cur.unwrap();
        total = total + node.val;
        cur = node.next;
    }
    total
}
fn main() {
    let chain = make(4);
    let node = chain.unwrap();
    let rest = node.step();
    println(sum(ident(rest)));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "9");
    }
}

#[test]
fn test_e2e_option_shared_niche_method_args_and_reuse() {
    // Method niche-ABI extension: `Option[shared T]` params/returns
    // on instance methods (`usermethod` path). Also pins the
    // arg-share discipline this slice ADDED to the method arg loops
    // — passing the same tracked binding twice (`m.total(chain)`
    // twice) read freed memory before, because the method paths
    // lacked `compile_call`'s `share_option_shared_ref_for_arg`
    // (pre-existing on the conventional ABI; surfaced by this
    // slice's probes, 2026-06-05).
    let out = run_program(
        r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
shared struct Merger { count: i64 }
impl Merger {
    fn total(ref self, head: Option[ListNode]) -> i64 {
        let mut t = 0;
        let mut cur = head;
        while cur.is_some() {
            let n = cur.unwrap();
            t = t + n.val;
            cur = n.next;
        }
        t
    }
    fn first_or_make(ref self, head: Option[ListNode]) -> Option[ListNode] {
        if head.is_some() {
            return head;
        }
        Some(ListNode { val: 99, next: None })
    }
}
fn make(n: i64) -> Option[ListNode] {
    let mut head: Option[ListNode] = None;
    let mut i = n;
    while i > 0 {
        let node = ListNode { val: i, next: head };
        head = Some(node);
        i = i - 1;
    }
    head
}
fn sum(head: Option[ListNode]) -> i64 {
    let mut t = 0;
    let mut cur = head;
    while cur.is_some() {
        let n = cur.unwrap();
        t = t + n.val;
        cur = n.next;
    }
    t
}
fn main() {
    let m = Merger { count: 0 };
    let chain = make(3);
    println(m.total(chain));
    println(m.total(chain));
    println(m.total(make(4)));
    let got = m.first_or_make(make(2));
    println(sum(got));
    let fresh = m.first_or_make(None);
    println(sum(fresh));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "6\n6\n10\n3\n99");
    }
}

#[test]
fn test_e2e_option_shared_niche_assoc_static() {
    // Static associated fns (`Type.method(...)`, the `usercall`
    // path) with `Option[shared T]` params/returns — binding reuse,
    // explicit-return alias, and a fully chained
    // `total(tail_of(build(...)))` through three niche boundaries.
    let out = run_program(
        r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
impl ListNode {
    fn build(n: i64) -> Option[ListNode] {
        let mut head: Option[ListNode] = None;
        let mut i = n;
        while i > 0 {
            let node = ListNode { val: i, next: head };
            head = Some(node);
            i = i - 1;
        }
        head
    }
    fn total(head: Option[ListNode]) -> i64 {
        let mut t = 0;
        let mut cur = head;
        while cur.is_some() {
            let n = cur.unwrap();
            t = t + n.val;
            cur = n.next;
        }
        t
    }
    fn tail_of(head: Option[ListNode]) -> Option[ListNode] {
        if head.is_some() {
            let n = head.unwrap();
            return n.next;
        }
        None
    }
}
fn main() {
    let chain = ListNode.build(4);
    println(ListNode.total(chain));
    println(ListNode.total(chain));
    let rest = ListNode.tail_of(chain);
    println(ListNode.total(rest));
    println(ListNode.total(ListNode.tail_of(ListNode.build(3))));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "10\n10\n9\n5");
    }
}

#[test]
fn test_e2e_option_shared_ref_self_tail_field_not_zeroed() {
    // `fn step(ref self) -> Option[ListNode] { self.next }` — a tail
    // field return rooted at a BORROWED receiver. Two fixes pinned:
    //   1. The move-out tail zeroing (`suppress_tail_field_option_dec`,
    //      the kata-#2 `dummy.next` mechanism) must NOT run for ref
    //      roots — it both severed the caller's list semantically
    //      AND miscompiled the address (wrote null through the
    //      un-deref'd ref-param slot into the caller's stack frame).
    //   2. The returned alias gets its +1 from the new ref-rooted
    //      FieldAccess arm in `compile_tail_final_expr` instead.
    // `chain` is summed AFTER the step() call to prove the caller's
    // list is intact (pre-fix: stack corruption / severed chain).
    let out = run_program(
        r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
impl ListNode {
    fn step(ref self) -> Option[ListNode] { self.next }
}
fn make(n: i64) -> Option[ListNode] {
    let mut head: Option[ListNode] = None;
    let mut i = n;
    while i > 0 {
        let node = ListNode { val: i, next: head };
        head = Some(node);
        i = i - 1;
    }
    head
}
fn sum(head: Option[ListNode]) -> i64 {
    let mut t = 0;
    let mut cur = head;
    while cur.is_some() {
        let n = cur.unwrap();
        t = t + n.val;
        cur = n.next;
    }
    t
}
fn main() {
    let mut total = 0;
    let mut iter = 0;
    while iter < 64 {
        let chain = make(8);
        let node = chain.unwrap();
        let stepped = node.step();
        total = total + sum(stepped);
        total = total + sum(chain);
        iter = iter + 1;
    }
    println(total);
}
"#,
    );
    if let Some(out) = out {
        // per iter: sum(stepped)=2..8=35, sum(chain)=1..8=36 → 71×64
        assert_eq!(out.trim(), "4544");
    }
}

#[test]
fn test_e2e_option_shared_owned_self_receiver() {
    // Owned `self` on a SHARED receiver (the bugs.md receiver-move
    // segfault, fixed 2026-06-05). Two coupled fixes pinned:
    //   1. Receiver passing — owned-shared `self` is ptr-typed at
    //      the LLVM level (shared types lower to the heap pointer),
    //      indistinguishable from `ref self` by LLVM type alone; the
    //      `usermethod` dispatch passed the STACK SLOT address for
    //      both. The callee's entry receive-inc then incremented a
    //      stack word as a refcount and every `self` field GEP was
    //      one indirection off. Now discriminated via the
    //      source-level ref flag (`fn_param_ref`): owned-shared self
    //      receives the loaded heap pointer by value.
    //   2. Tail `self.next` from owned `self` must not zero the
    //      field — the heap object is still referenced by the
    //      caller's binding (the receive-inc/scope-dec keep the
    //      callee's frame balanced, they don't make it exclusive).
    //      The retired move-out zeroing severed the caller's list;
    //      the unified loaded-inner inc in `compile_tail_final_expr`
    //      keeps it intact.
    // `sum(chain)` AFTER the step() call proves non-destructive
    // reads (matches the interpreter); the loop catches refcount
    // drift in either direction.
    let out = run_program(
        r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
impl ListNode {
    fn step(self) -> Option[ListNode] { self.next }
    fn value(self) -> i64 { self.val }
}
fn make(n: i64) -> Option[ListNode] {
    let mut head: Option[ListNode] = None;
    let mut i = n;
    while i > 0 {
        let node = ListNode { val: i, next: head };
        head = Some(node);
        i = i - 1;
    }
    head
}
fn sum(head: Option[ListNode]) -> i64 {
    let mut t = 0;
    let mut cur = head;
    while cur.is_some() {
        let n = cur.unwrap();
        t = t + n.val;
        cur = n.next;
    }
    t
}
fn main() {
    let mut total = 0;
    let mut iter = 0;
    while iter < 64 {
        let chain = make(8);
        let node = chain.unwrap();
        total = total + node.value();
        let rest = node.step();
        total = total + sum(rest);
        total = total + sum(chain);
        iter = iter + 1;
    }
    println(total);
}
"#,
    );
    if let Some(out) = out {
        // per iter: value=1, sum(rest)=2..8=35, sum(chain)=1..8=36 → 72×64
        assert_eq!(out.trim(), "4608");
    }
}

#[test]
fn test_e2e_option_shared_dummy_tail_inc_not_zeroing() {
    // The kata-#2 builder shape (`fn f() -> Option[T] { ...
    // dummy.next }` — tail field return from a DYING owned local)
    // under the unified loaded-inner inc that replaced the move-out
    // field zeroing. The inc (+1) and the dying owner's
    // recursive-drop dec (-1) net to a wholesale transfer of the
    // field's ref — same caller-visible contract as the zeroing,
    // without mutating heap state any other ref could observe.
    // Looped so a drift in either direction (UAF or leak) surfaces.
    let out = run_program(
        r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
fn build_appended(n: i64) -> Option[ListNode] {
    let mut dummy = ListNode { val: 0, next: None };
    let mut tail = dummy;
    let mut i = 1;
    while i <= n {
        let node = ListNode { val: i, next: None };
        tail.next = Some(node);
        tail = node;
        i = i + 1;
    }
    dummy.next
}
fn sum(head: Option[ListNode]) -> i64 {
    let mut t = 0;
    let mut cur = head;
    while cur.is_some() {
        let n = cur.unwrap();
        t = t + n.val;
        cur = n.next;
    }
    t
}
fn main() {
    let mut total = 0;
    let mut iter = 0;
    while iter < 64 {
        total = total + sum(build_appended(8));
        iter = iter + 1;
    }
    println(total);
}
"#,
    );
    if let Some(out) = out {
        // per iter: 1..8 = 36 → 36×64
        assert_eq!(out.trim(), "2304");
    }
}

#[test]
fn test_e2e_option_shared_field_let_alias_acquires_ref() {
    // `let stepped = node.next;` — an Identifier-object
    // `Option[shared T]` field read bound by an untyped let. The
    // case-(c) registration queued the binding's scope-exit
    // `RcDecOption` but nothing inc'd the loaded inner — stepped's
    // dec freed the sub-chain the field still owned and the owner's
    // drop walked freed memory. LATENT on main since case (c)
    // landed (the freed chunk's garbage rc-word usually stops the
    // walk silently); the niche-ABI allocation-pattern shift made
    // it trap deterministically (v0c repro, 2026-06-05). The
    // binding now takes the same aliasing-acquire +1 as case (d).
    // Free fns only — pins the fix independent of the method
    // extension.
    let out = run_program(
        r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
fn build(n: i64) -> Option[ListNode] {
    let mut head: Option[ListNode] = None;
    let mut i = n;
    while i > 0 {
        let node = ListNode { val: i, next: head };
        head = Some(node);
        i = i - 1;
    }
    head
}
fn sum(head: Option[ListNode]) -> i64 {
    let mut t = 0;
    let mut cur = head;
    while cur.is_some() {
        let n = cur.unwrap();
        t = t + n.val;
        cur = n.next;
    }
    t
}
fn main() {
    let mut total = 0;
    let mut iter = 0;
    while iter < 64 {
        let chain = build(8);
        let node = chain.unwrap();
        let stepped = node.next;
        total = total + sum(stepped);
        total = total + sum(chain);
        iter = iter + 1;
    }
    println(total);
}
"#,
    );
    if let Some(out) = out {
        // per iter: sum(stepped)=2..8=35, sum(chain)=1..8=36 → 71×64
        assert_eq!(out.trim(), "4544");
    }
}

#[test]
fn test_e2e_with_provider_option_shared_resource_methods() {
    // Provider-vtable dispatch (`with_provider`) with
    // `Option[shared T]` in the resource methods' signatures. The
    // indirect-call FunctionType comes from the registered impl fn
    // (niche-shaped under the method extension), and the dispatch
    // site packs args / unpacks results keyed by the same impl
    // (`provider_method_fn_type`'s returned qualified name). Lets
    // are deliberately UNannotated: `resolve_path_type`'s
    // effect-resource dispatch arm types `let got = Store.lookup(1)`
    // from the override impl's signature (the former typechecker
    // inference gap tracked in bugs.md), which is what populates the
    // `method_unwrap_inner_types` table the `is_some`/`unwrap`
    // lowering gates on.
    let out = run_program(
        r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
effect resource Store;
struct FakeStore { n: i64 }
impl FakeStore {
    fn lookup(self, k: i64) -> Option[ListNode] {
        if k == 1 {
            return Some(ListNode { val: self.n, next: None });
        }
        None
    }
    fn passthru(self, h: Option[ListNode]) -> Option[ListNode] { h }
}
fn probe() -> i64 reads(Store) {
    let got = Store.lookup(1);
    let missed = Store.lookup(0);
    let mut t = 0;
    if got.is_some() {
        let node = got.unwrap();
        t = t + node.val;
    }
    if missed.is_none() {
        t = t + 100;
    }
    let chain = Store.passthru(got);
    if chain.is_some() {
        let node = chain.unwrap();
        t = t + node.val;
    }
    t
}
fn main() reads(Store) {
    with_provider[Store](FakeStore { n: 5 }, || {
        println(probe());
    });
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "110");
    }
}

/// Phase-8 line 74 — `build_fd_construct_result`'s Err arm decodes the
/// runtime's stable negative code into a named `TcpError` variant via
/// a `select` chain (`.err.is_conn_refused` / `.err.variant_tag.*`),
/// rather than packing a fixed `Other(-1)`. Pins the classification.
#[test]
fn test_ir_fd_construct_result_classifies_named_causes() {
    let ir = ir_for(
        r#"
fn main() {
    let s = TcpStream.connect("127.0.0.1:8080").unwrap();
    println(s.fd);
}
"#,
    );
    let body = function_body(&ir, "main").expect("main body");
    assert!(
        body.contains("tcp.connect.err.is_conn_refused")
            && body.contains("tcp.connect.err.is_addr_in_use"),
        "Err arm should emit the cause-classification comparisons; body was:\n{}",
        body
    );
    assert!(
        body.contains("tcp.connect.err.variant_tag.conn_refused"),
        "Err arm should select the variant tag per cause; body was:\n{}",
        body
    );
}

/// B-2026-08-26-12 — an `if`-EXPRESSION whose arms yield an
/// `Option[shared]` binding, passed BY VALUE to a function, inside a loop
/// that rebinds it, corrupts the heap on both compiled backends.
///
/// The control below is the same program with the `if` removed
/// (`show(a)` instead of `show(if s == 0 { a } else { a })`) and is green,
/// so the whole delta is the if-expression. `--interp` prints `v 0` /
/// `v 1` correctly; `karac run` (JIT) and `karac build` both died with
/// glibc's `malloc(): unaligned tcache chunk detected` (SIGABRT).
///
/// FIXED: a branch leaf handing out an `Option[shared]` binding now takes
/// the same per-arm retain the plain-`shared` leaf already took. The
/// if-expression turned out to be one member of a family — see
/// `option_shared_through_every_branch_leaf_form_survives_a_rebinding_loop`
/// below, which covers the `match` arm, the bare block, the nested and
/// else-if chains, and the mixed-arm shape that pins the fix per-arm.
///
/// Deliberately an E2E test and NOT an ASAN fixture: the
/// `-fsanitize=address` link does not reproduce it at all — under ASAN's
/// allocator this program runs clean AND prints the right answer, which is
/// why the ~1200-fixture `tests/memory_sanitizer.rs` corpus never caught
/// the class. Measured, not assumed — and structural, not luck: the
/// sanitizer is linked in, never compiled in (`link_executable_with_sanitizer`
/// passes `-fsanitize=address` to `cc` at LINK time only), so ASAN sees
/// allocator-level faults — double free, invalid free, leaks — and cannot
/// see a use-after-free ACCESS from the uninstrumented Kāra object. This
/// bug's fault is a stray refcount decrement THROUGH a freed box, which
/// writes `-1` where glibc keeps the tcache `fd` word and never calls
/// `free` twice, so there is nothing at the allocator boundary to catch.
/// B-2026-08-27-34 — the THIRD consumption of an `Option[shared]` selected
/// by a value-position branch read through a freed box.
///
/// The mechanism is one step earlier than "the retain is emitted once per
/// binding rather than once per use", which is how the symptom reads from
/// the outside. MEASURED by instrumenting `share_option_shared_ref_for_arg`:
/// the consuming binding `t` was never registered in
/// `var_option_shared_heap` AT ALL, because `control_flow_owned_option_shared`
/// — case (g) of the `let`-RHS cascade — only counted a leaf as owning a
/// `+1` if it was a `Some(..)`/call/`None` PRODUCER, and a bare binding
/// handed out of an arm is none of those. So `t` got neither a scope-exit
/// `RcDecOption` nor the per-USE call-site retains that a registered
/// binding gets, and the single leaf retain B-2026-08-26-12 added was
/// standing in for the whole of `t`'s ownership. One `+1` covers exactly
/// TWO consumptions, which is why the boundary is exact and why two reads
/// look green.
///
/// The fix teaches case (g) that a bare `Option[shared]` binding IS an
/// owning leaf now — which is precisely what B-2026-08-26-12 made true when
/// it taught the leaf to retain. Registering `t` restores the invariant the
/// other cases already rely on: each use incs and the callee decs, so the
/// count stops depending on how many times the binding is consumed.
///
/// This has no loop and no rebinding at all — it is straight-line code:
///
/// ```kara
/// let t = if true { a } else { b };
/// println(f"{show(t)} {show(t)} {show(t)}");
/// ```
///
/// `--interp` printed `1 1 1`. `karac build` printed `1 1 <garbage>` and
/// exited **0** — a silent wrong answer, not a crash, and the garbage word
/// differed run to run. `karac run` (JIT) printed the same garbage and then
/// aborted inside the allocator. The control below is the identical program
/// with the if-expression removed and was green throughout, so the delta is
/// exactly the branch leaf.
#[test]
fn option_shared_from_a_branch_leaf_survives_a_third_consumption() {
    let src = "shared struct Node { val: i64 }\n\
                   fn make(n: i64) -> Option[Node] { return Some(Node { val: n }); }\n\
                   fn show(t: Option[Node]) -> i64 {\n\
                       match t { None => { return 0; } Some(n) => { return n.val; } }\n\
                   }\n\
                   fn main() {\n\
                       let a = make(1);\n\
                       let b = make(2);\n\
                       let t = if true { a } else { b };\n\
                       println(f\"{show(t)} {show(t)} {show(t)}\");\n\
                   }";
    if let Some(c) = run_program_capturing(src) {
        assert_eq!(
            c.stdout, "1 1 1\n",
            "every read of a branch-leaf `Option[shared]` must see the same value"
        );
        assert!(
            c.status.success(),
            "process died ({:?}) — stderr: {}",
            c.status,
            c.stderr
        );
    }
}

/// B-2026-08-27-34, the whole branch-leaf family at THREE-PLUS consumptions
/// — the axis every fixture that landed with B-2026-08-26-12 missed, since
/// each of those consumes the selected value exactly once (twice over a
/// two-iteration loop, which is a different axis: per-iteration accounting
/// is correct, per-use accounting is not).
///
/// Twelve legs, each binding the branch's value and then consuming that
/// ONE binding three times (five on leg 11), so the count can no longer be
/// balanced by the single leaf retain standing in for the binding's
/// ownership. Two legs are load-bearing beyond the reported `if` shape:
///
///   * Leg 3, the `if let` THEN arm. B-2026-08-26-12's message says its
///     `compile_block_with_frame` hook covers "if/if let arms". It covers
///     `if`, and an `if let`'s ELSE branch (leg 4) which routes through
///     that same helper — but NOT an `if let`'s then arm, which hand-rolls
///     its frame drain against a plain `compile_block`. That gap was
///     invisible while the consuming binding went unregistered (no retain
///     and no dec is balanced by accident for one use) and became a live
///     use-after-free the moment registration made the missing `+1`
///     load-bearing, so the two halves of this fix are not separable.
///   * Legs 9 and 10, the MIXED arms, which pin the retain per-arm rather
///     than at the phi: `{ a }` hands out a borrowed binding and needs the
///     retain, `{ make(19) }` carries its producer's own `+1` and must not
///     take a second. Omit the retain and the borrowed arm double-frees;
///     add it at the phi and the fresh arm leaks. Both directions are
///     exercised, and `asan_option_shared_branch_leaf_repeated_consumption_is_clean`
///     in `tests/memory_sanitizer.rs` is the LSan half that catches the
///     over-retaining wrong fix.
///
/// Twinned against the interpreter, which was correct throughout.
#[test]
fn option_shared_from_every_branch_leaf_form_survives_repeated_consumption() {
    let src = "shared struct Node { val: i64 }\n\
                   fn make(n: i64) -> Option[Node] { return Some(Node { val: n }); }\n\
                   fn show(t: Option[Node]) -> i64 {\n\
                       match t { None => { return 0; } Some(n) => { return n.val; } }\n\
                   }\n\
                   fn main() {\n\
                       let a1 = make(1); let b1 = make(2);\n\
                       let t1 = if true { a1 } else { b1 };\n\
                       println(f\"{show(t1)} {show(t1)} {show(t1)}\");\n\
                       let a2 = make(3); let b2 = make(4);\n\
                       let t2 = if false { a2 } else { b2 };\n\
                       println(f\"{show(t2)} {show(t2)} {show(t2)}\");\n\
                       let a3 = make(5); let b3 = make(6); let o3 = Some(1);\n\
                       let t3 = if let Some(x) = o3 { a3 } else { b3 };\n\
                       println(f\"{show(t3)} {show(t3)} {show(t3)}\");\n\
                       let a4 = make(7); let b4 = make(8); let o4: Option[i64] = None;\n\
                       let t4 = if let Some(y) = o4 { a4 } else { b4 };\n\
                       println(f\"{show(t4)} {show(t4)} {show(t4)}\");\n\
                       let a5 = make(9); let b5 = make(10); let k5 = 0;\n\
                       let t5 = match k5 { 0 => a5, _ => b5 };\n\
                       println(f\"{show(t5)} {show(t5)} {show(t5)}\");\n\
                       let a6 = make(11);\n\
                       let t6 = { a6 };\n\
                       println(f\"{show(t6)} {show(t6)} {show(t6)}\");\n\
                       let a7 = make(12); let b7 = make(13); let c7 = make(14); let k7 = 1;\n\
                       let t7 = if k7 == 0 { a7 } else if k7 == 1 { b7 } else { c7 };\n\
                       println(f\"{show(t7)} {show(t7)} {show(t7)}\");\n\
                       let a8 = make(15); let b8 = make(16);\n\
                       let t8 = if true { if true { a8 } else { b8 } } else { make(99) };\n\
                       println(f\"{show(t8)} {show(t8)} {show(t8)}\");\n\
                       let a9 = make(17);\n\
                       let t9 = if true { a9 } else { make(99) };\n\
                       println(f\"{show(t9)} {show(t9)} {show(t9)}\");\n\
                       let a10 = make(18);\n\
                       let t10 = if false { a10 } else { make(19) };\n\
                       println(f\"{show(t10)} {show(t10)} {show(t10)}\");\n\
                       let a11 = make(20); let b11 = make(21);\n\
                       let t11 = if true { a11 } else { b11 };\n\
                       println(f\"{show(t11)} {show(t11)} {show(t11)} {show(t11)} {show(t11)}\");\n\
                       let a12 = make(22);\n\
                       let t12 = if false { a12 } else { None };\n\
                       println(f\"{show(t12)} {show(t12)} {show(t12)}\");\n\
                   }";
    let expected = "1 1 1\n4 4 4\n5 5 5\n8 8 8\n9 9 9\n11 11 11\n13 13 13\n15 15 15\n17 17 17\n19 19 19\n20 20 20 20 20\n0 0 0\n";
    if let Some(c) = run_program_capturing(src) {
        assert_eq!(
            c.stdout, expected,
            "every branch-leaf form must survive repeated consumption of \
                 the binding it was selected into"
        );
        assert!(
            c.status.success(),
            "process died ({:?}) — stderr: {}",
            c.status,
            c.stderr
        );
    }
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(
        interp_errs.is_empty(),
        "interpreter errored on the twin: {interp_errs:?}"
    );
    assert_eq!(
        interp_out.join(""),
        expected,
        "interpreter twin must agree with the compiled backend"
    );
}

/// The control for `option_shared_from_a_branch_leaf_survives_a_third_consumption`:
/// the same three reads with no branch leaf between the binding and the
/// uses. Green today, and it is what keeps that test's failure attributable
/// to the branch leaf rather than to reading an `Option[shared]` three times.
#[test]
fn option_shared_read_three_times_without_a_branch_leaf() {
    let src = "shared struct Node { val: i64 }\n\
                   fn make(n: i64) -> Option[Node] { return Some(Node { val: n }); }\n\
                   fn show(t: Option[Node]) -> i64 {\n\
                       match t { None => { return 0; } Some(n) => { return n.val; } }\n\
                   }\n\
                   fn main() {\n\
                       let a = make(1);\n\
                       println(f\"{show(a)} {show(a)} {show(a)}\");\n\
                   }";
    if let Some(c) = run_program_capturing(src) {
        assert_eq!(c.stdout, "1 1 1\n");
        assert!(c.status.success(), "stderr: {}", c.stderr);
    }
}

#[test]
fn option_shared_through_if_expression_survives_a_rebinding_loop() {
    let src = "shared struct Node { val: i64 }\n\
                   fn make(n: i64) -> Option[Node] { return Some(Node { val: n }); }\n\
                   fn show(t: Option[Node]) {\n\
                       match t { None => { } Some(n) => { println(f\"v {n.val}\"); } }\n\
                   }\n\
                   fn main() {\n\
                       let mut s = 0;\n\
                       while s < 2 {\n\
                           let a = make(s);\n\
                           show(if s == 0 { a } else { a });\n\
                           s = s + 1;\n\
                       }\n\
                   }";
    if let Some(c) = run_program_capturing(src) {
        assert_eq!(
            c.stdout, "v 0\nv 1\n",
            "if-expression over an Option[shared] must not change the value"
        );
        assert!(
            c.status.success(),
            "process died ({:?}) — stderr: {}",
            c.status,
            c.stderr
        );
    }
}

/// The control for `option_shared_through_if_expression_survives_a_rebinding_loop`:
/// identical except the if-expression is gone. Green today, and it is what
/// makes that test's failure attributable to the if-expression rather than
/// to `Option[shared]` in a loop generally.
#[test]
fn option_shared_without_an_if_expression_survives_a_rebinding_loop() {
    let src = "shared struct Node { val: i64 }\n\
                   fn make(n: i64) -> Option[Node] { return Some(Node { val: n }); }\n\
                   fn show(t: Option[Node]) {\n\
                       match t { None => { } Some(n) => { println(f\"v {n.val}\"); } }\n\
                   }\n\
                   fn main() {\n\
                       let mut s = 0;\n\
                       while s < 2 {\n\
                           let a = make(s);\n\
                           show(a);\n\
                           s = s + 1;\n\
                       }\n\
                   }";
    if let Some(c) = run_program_capturing(src) {
        assert_eq!(c.stdout, "v 0\nv 1\n");
        assert!(c.status.success(), "stderr: {}", c.stderr);
    }
}

/// B-2026-08-26-12, the FAMILY. The reported shape was an if-expression,
/// but the defect was never about `if`: any value-position branch leaf that
/// hands an `Option[shared]` binding to an owned parameter handed out an
/// UNCOUNTED alias, so the callee's exit dec took the box to rc 0 and freed
/// it while the source's still-armed `RcDecOption` decremented through the
/// freed memory. Every leg below aborted before the fix; only the direct
/// `show(a)` control did not.
///
/// The `mixed` leg is the one with teeth. Its arms are not alike —
/// `{ d }` hands out a borrowed binding and needs the retain, `{ make(9) }`
/// carries `make`'s own `+1` and must not take a second — so it fails in
/// BOTH directions: a compiler that retains neither double frees, and one
/// that retains at the merge instead of per-arm leaks the fresh arm. That
/// is what forces the retain into each arm's own basic block.
///
/// Twinned against `--interp` rather than a hardcoded string, so the two
/// backends are asserted equal as well as correct.
#[test]
fn option_shared_through_every_branch_leaf_form_survives_a_rebinding_loop() {
    let src = "shared struct Node { val: i64 }\n\
                   fn make(n: i64) -> Option[Node] { return Some(Node { val: n }); }\n\
                   fn take(t: Option[Node]) -> i64 {\n\
                       match t { None => { return 0; } Some(n) => { return n.val; } }\n\
                   }\n\
                   fn main() {\n\
                       let mut s = 0;\n\
                       let mut total = 0;\n\
                       while s < 4 {\n\
                           let a = make(s);\n\
                           total = total + take(if s == 0 { a } else { a });\n\
                           let b = make(s);\n\
                           total = total + take(match s { 0 => b, _ => b });\n\
                           let c = make(s);\n\
                           total = total + take({ c });\n\
                           let d = make(s);\n\
                           total = total + take(if s < 2 { d } else { make(9) });\n\
                           let e = make(s);\n\
                           total = total + take(if s == 0 { if s == 0 { e } else { e } } else { e });\n\
                           let g = make(s);\n\
                           total = total + take(if s == 0 { g } else if s == 1 { g } else { g });\n\
                           let h = make(s);\n\
                           let bound = if s == 0 { h } else { h };\n\
                           total = total + take(bound);\n\
                           s = s + 1;\n\
                       }\n\
                       println(total);\n\
                   }";
    // 6 legs yield s (0+1+2+3 = 6 each = 36); the mixed leg yields s for
    // s<2 (0+1 = 1) and 9 twice (18) = 19. 36 + 19 = 55.
    let expected = "55\n";
    if let Some(c) = run_program_capturing(src) {
        assert_eq!(
            c.stdout, expected,
            "every branch-leaf form handing out an Option[shared] must \
                 keep the value intact"
        );
        assert!(
            c.status.success(),
            "process died ({:?}) — stderr: {}",
            c.status,
            c.stderr
        );
    }
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(
        interp_errs.is_empty(),
        "interpreter errored on the twin: {interp_errs:?}"
    );
    assert_eq!(
        interp_out.join(""),
        expected,
        "interpreter twin must agree with the compiled backend"
    );
}

#[test]
fn test_try_companion_instance_codegen_rejected_cleanly() {
    // Fallible-allocation `try_*` companions whose codegen lowering is still
    // blocked are interpreter-only; `karac build` must fail loud with the
    // actionable item-8 message rather than mis-lower. `Map`/`Set`
    // `try_insert` now compiles (B-2026-07-09-15, via the fallible
    // `karac_map_try_insert` runtime path), but `try_clone` on a Map/Set is
    // still blocked (its fallible clone needs the whole `karac_map_*` clone
    // surface made fallible). `try_clone` on a Vec/String/VecDeque compiles,
    // so we exercise the remaining guard through a `Map` receiver's
    // `try_clone`.
    let mut parsed = karac::parse(
        "fn main() {\n\
                 let mut m: Map[String, i64] = Map.new();\n\
                 m.insert(\"k\", 1_i64);\n\
                 let _ = m.try_clone();\n\
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
    assert!(
        typed.errors.is_empty(),
        "Map.try_clone should typecheck: {:?}",
        typed.errors
    );
    karac::lower(&mut parsed.program, &typed);
    let err = compile_to_ir(&parsed.program, None, None)
        .expect_err("Map.try_clone codegen must fail loud (interpreter-only)")
        .message;
    assert!(
        err.contains("interpreter-only") && err.contains("item 8") && err.contains("try_clone"),
        "expected the actionable item-8 message, got: {err}"
    );
}

#[test]
fn test_try_companion_constructors_codegen_all_implemented() {
    // Every recognized static-constructor `try_*` companion now codegens
    // (Vec.try_from_slice + Vec/VecDeque/String.try_with_capacity), so none
    // hits the `compile_assoc_call` interpreter-only reject guard anymore.
    // Regression guard: these must compile to IR cleanly (the guard stays as
    // a defensive net for any future companion, exercised by the parallel
    // instance test `test_try_companion_instance_codegen_rejected_cleanly`).
    let mut parsed = karac::parse(
        "fn vd() -> Result[i64, AllocError] {\n\
                 let r: Result[VecDeque[i64], AllocError] = VecDeque.try_with_capacity(8);\n\
                 match r { Ok(v) => Ok(v.len()), Err(_) => Ok(0_i64) }\n\
             }\n\
             fn st() -> Result[i64, AllocError] {\n\
                 let r: Result[String, AllocError] = String.try_with_capacity(8);\n\
                 match r { Ok(s) => Ok(s.len()), Err(_) => Ok(0_i64) }\n\
             }\n\
             fn main() { let _ = vd(); let _ = st(); }",
    );
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    karac::prepare_for_resolve(&mut parsed.program);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    assert!(
        typed.errors.is_empty(),
        "VecDeque/String.try_with_capacity should typecheck: {:?}",
        typed.errors
    );
    karac::lower(&mut parsed.program, &typed);
    assert!(
        compile_to_ir(&parsed.program, None, None).is_ok(),
        "VecDeque/String.try_with_capacity must now codegen (no interpreter-only reject)"
    );
}

#[test]
fn test_e2e_call_result_tuple_var_drop() {
    // #24 (phase-12 self-hosting) — a let-bound tuple VAR sourced from a CALL
    // (`let p = ret_tuple(i)`, RHS a `Call`, not a tuple literal, no annotation)
    // whose only heap is an enum / Map leaf. `track_tuple_var` is enum/Map-blind
    // (the leaf is all-i64 words), and the call-result source missed the
    // annotation/literal arms of `tuple_binding_elem_tes`, so NO drop was
    // registered and the leaf leaked. The fix recovers the element `TypeExpr`s
    // from the callee's recorded return type (`fn_return_type_exprs`) so the
    // `TypeExpr`-driven tuple drop runs. Coupled (`B-2026-06-14-1`): the drop-fn
    // memoization key (`type_expr_sig`) now folds in generic args, so
    // `Map[i64,i64]` ((0,0) flags) and `Map[String,i64]` ((1,0) flags) get
    // distinct drop fns rather than aliasing (a scalar-first program had leaked
    // a later `Map[String,_]`'s keys). Correctness is the call-sourced tuple
    // still round-tripping when later consumed, across enum / scalar-map /
    // String-key-map leaves; the leak fix itself is in
    // `asan_call_result_tuple_var_no_leak`.
    if let Some(out) = run_program(
        r#"
enum Tok { Id(String), Num(i64) }
fn ret_tuple(i: i64) -> (Tok, i64) { return (Tok.Id(f"id{i}"), i); }
fn ret_imap(i: i64) -> (Map[i64, i64], i64) {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(i, i * 10);
    return (m, i);
}
fn ret_smap(i: i64) -> (Map[String, i64], i64) {
    let mut m: Map[String, i64] = Map.new();
    m.insert(f"k{i}", i);
    return (m, i);
}
fn use_tok(t: Tok) -> String {
    match t { Id(s) => s, Num(n) => n.to_string() }
}
fn main() {
    // Call-sourced enum-leaf tuple var, then destructure + consume.
    let p = ret_tuple(2);
    let (t, n) = p;
    println(f"{use_tok(t)}-{n}");
    // Call-sourced scalar-map tuple var, consumed by-value.
    let q = ret_imap(4);
    let (m1, a) = q;
    println(f"{m1.len()}-{a}");
    // Call-sourced String-key-map tuple var (distinct memo key from scalar map).
    let r = ret_smap(6);
    let (m2, b) = r;
    println(f"{m2.len()}-{b}");
    println("ok");
}
"#,
    ) {
        assert_eq!(out, "id2-2\n1-4\n1-6\nok\n");
    }
}

#[test]
fn test_e2e_option_unwrap_shared_struct() {
    // Slice OR companion: `Option[SharedStruct].unwrap()` reconstitutes
    // the RC heap-pointer from the i64 payload word via `inttoptr`,
    // and downstream field access dispatches correctly through the
    // shared-struct heap GEP. This is the kata-133 unwrap shape
    // (`visited.get(curr.val).unwrap()` where `Node` is a `shared
    // struct`) — the second half of the OR slice's coverage area
    // (primitive payloads via the integer arm, shared-struct
    // payloads via the pointer arm).
    let out = run_program(
        r#"
shared struct Node { val: i64 }
fn main() {
    let n = Node { val: 99 };
    let mut m: Map[i64, Node] = Map.new();
    let _ = m.insert(7, n);
    let got = m.get(7).unwrap();
    println(got.val);
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["99"]);
    }
}

#[test]
fn test_e2e_option_is_some_is_none() {
    // Slice OR companion: `is_some` / `is_none` are the tag-only
    // arms of the OR dispatch (no payload reconstitution, no panic
    // BB).  Verifies both polarities and that the surface return
    // type is `bool` so the result composes with normal boolean
    // arithmetic at the use site.
    let out = run_program(
        r#"
fn main() {
    let mut m: Map[i64, i64] = Map.new();
    let _ = m.insert(1, 42);
    println(m.get(1).is_some());
    println(m.get(2).is_some());
    println(m.get(1).is_none());
    println(m.get(2).is_none());
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["true", "false", "false", "true"]);
    }
}

// ── Option/Result-like enums ────────────────────────────────

#[test]
fn test_e2e_option_like_enum() {
    let out = run_program(
        r#"
enum MyOption {
    None,
    Some(i64),
}
fn get_value(opt: MyOption) -> i64 {
    match opt {
        None => 0,
        Some(x) => x,
    }
}
fn main() {
    println(get_value(MyOption.Some(42)));
    println(get_value(MyOption.None));
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["42", "0"]);
    }
}

#[test]
fn test_ir_adopted_root_takes_option_free_walk() {
    // Phase C1c: a fresh-return builder call result bound to a
    // non-escaping local drops via the Option-tag-guarded
    // free-walk (acw_*) instead of the RcDecOption dec-walk — and
    // the caller's whole body is count-free: the call move-out
    // needs no inc, the sanctioned match binds a borrowed alias,
    // and family cursors are non-owning.
    let ir = ir_for_with_ownership(
        r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
fn build(n: i64) -> Option[ListNode] {
    let head = ListNode { val: 1, next: None };
    let mut tail = head;
    let mut i = 2;
    while i <= n {
        let node = ListNode { val: i, next: None };
        tail.next = Some(node);
        tail = node;
        i = i + 1;
    }
    Some(head)
}
fn main() {
    let mut total = 0;
    let mut iter = 0;
    while iter < 4 {
        let out = build(5);
        match out {
            Some(node) => { total = total + node.val; }
            None => {}
        }
        iter = iter + 1;
    }
    println(total);
}
"#,
    );
    let body = function_body(&ir, "main").expect("fn body");
    assert!(
        body.contains("acw_tag") && body.contains("acw_loop"),
        "adopted root must take the option-guarded free-walk; body:\n{body}"
    );
    assert!(
        !body.contains("opt_rc_cleanup"),
        "no RcDecOption dec-walk for the adopted root; body:\n{body}"
    );
    assert!(
        !body.contains("rc_inc") && !body.contains("rc_dec"),
        "adopting caller is count-free; body:\n{body}"
    );
}

// ── ? operator codegen ───────────────────────────────────────────────────

#[test]
fn test_e2e_question_option_some_propagates() {
    // When the inner expression is Some, ? unwraps the value and continues.
    let out = run_program(
        r#"
fn maybe(flag: bool) -> Option[i64] {
    if flag { Some(5_i64) } else { None }
}
fn add_ten(flag: bool) -> Option[i64] {
    let x = maybe(flag)?;
    Some(x + 10)
}
fn main() {
    match add_ten(true) {
        Some(n) => println(n),
        None => println(0),
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "15");
    }
}

#[test]
fn test_e2e_question_option_none_propagates() {
    // When the inner expression is None, ? early-returns None from the caller.
    let out = run_program(
        r#"
fn maybe(flag: bool) -> Option[i64] {
    if flag { Some(5_i64) } else { None }
}
fn add_ten(flag: bool) -> Option[i64] {
    let x = maybe(flag)?;
    Some(x + 10)
}
fn main() {
    match add_ten(false) {
        Some(n) => println(n),
        None => println(0),
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "0");
    }
}

#[test]
fn test_e2e_question_result_ok_propagates() {
    // When the inner expression is Ok, ? unwraps the value and continues.
    let out = run_program(
        r#"
fn parse_int(flag: bool) -> Result[i64, i64] {
    if flag { Ok(42_i64) } else { Err(99_i64) }
}
fn add_ten(flag: bool) -> Result[i64, i64] {
    let x = parse_int(flag)?;
    Ok(x + 10)
}
fn main() {
    match add_ten(true) {
        Ok(n) => println(n),
        Err(_) => println(0),
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "52");
    }
}

#[test]
fn test_e2e_question_result_err_propagates() {
    // When the inner expression is Err, ? early-returns Err from the caller.
    let out = run_program(
        r#"
fn parse_int(flag: bool) -> Result[i64, i64] {
    if flag { Ok(42_i64) } else { Err(99_i64) }
}
fn add_ten(flag: bool) -> Result[i64, i64] {
    let x = parse_int(flag)?;
    Ok(x + 10)
}
fn main() {
    match add_ten(false) {
        Ok(_) => println(0),
        Err(e) => println(e),
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "99");
    }
}

#[test]
fn test_e2e_question_in_loop() {
    // ? inside a loop body must propagate the failure out of the enclosing function,
    // not just out of the loop iteration.
    let out = run_program(
        r#"
fn step(n: i64) -> Result[i64, i64] {
    if n < 3_i64 { Ok(n) } else { Err(n) }
}
fn run() -> Result[i64, i64] {
    let mut total: i64 = 0;
    let mut i: i64 = 0;
    while i < 5_i64 {
        let v = step(i)?;
        total = total + v;
        i = i + 1;
    }
    Ok(total)
}
fn main() {
    match run() {
        Ok(v) => println(v),
        Err(e) => println(e),
    }
}
"#,
    );
    if let Some(out) = out {
        // step succeeds for i=0,1,2 (sums to 3); fails at i=3 with Err(3)
        assert_eq!(out.trim(), "3");
    }
}

#[test]
fn test_e2e_question_cross_error_from_conversion() {
    // ? converts the inner error type via the user-impl `From` when
    // typechecker records a question_conversion at this site.
    // raw err `7_i64` flows through MyError.from(_) which doubles it.
    let out = run_program(
        r#"
struct RawError { code: i64 }
struct MyError { code: i64 }
impl From for MyError {
    fn from(e: RawError) -> MyError { MyError { code: e.code * 2_i64 } }
}
fn lookup() -> Result[i64, RawError] { Err(RawError { code: 7_i64 }) }
fn process() -> Result[i64, MyError] {
    let _ = lookup()?;
    Ok(0_i64)
}
fn main() {
    match process() {
        Ok(_) => println(0_i64),
        Err(e) => println(e.code),
    }
}
"#,
    );
    let out = out.expect("? cross-error codegen should not bail");
    // Without conversion: 7. With From doubling: 14.
    assert_eq!(out.trim(), "14");
}

#[test]
fn test_e2e_question_cross_error_from_conversion_reaches_err_arm() {
    // Smoke test for the LLVM-verification half of the `?` cross-error
    // path: codegen reconstitutes the source-error struct from the i64
    // payload word before calling `Target.from`, and coerces the
    // returned struct back to an i64 word for the outer Result aggregate.
    // The full `e.code` assertion is gated on a separate codegen fix
    // for struct-payload match-arm binding (see the `#[ignore]` note on
    // `test_e2e_question_cross_error_from_conversion`); this case
    // matches the Err arm without touching the binding's fields, so it
    // exercises the verification fix in isolation.
    let out = run_program(
        r#"
struct RawError { code: i64 }
struct MyError { code: i64 }
impl From for MyError {
    fn from(e: RawError) -> MyError { MyError { code: e.code * 2_i64 } }
}
fn lookup() -> Result[i64, RawError] { Err(RawError { code: 7_i64 }) }
fn process() -> Result[i64, MyError] {
    let _ = lookup()?;
    Ok(0_i64)
}
fn main() {
    match process() {
        Ok(_) => println(0_i64),
        Err(_) => println(99_i64),
    }
}
"#,
    );
    let out = out.expect("? cross-error codegen should not bail");
    assert_eq!(out.trim(), "99");
}

// ── ? error_return_trace push from compiled binaries ─────────────────────
//
// The runtime maintains a thread-local depth-64 ring buffer; codegen
// emits a `karac_error_trace_push` at each `?` failure block before the
// early return. An atexit handler in the runtime prints the buffer to
// stderr at process exit when non-empty. These tests exercise the full
// compile → link → run → stderr-capture path.

#[test]
fn test_e2e_question_trace_single_frame_on_err() {
    // A single `?` site that propagates `Err` should produce one frame
    // in the stderr trace, matching the interpreter's text format.
    let captured = run_program_capturing(
        r#"
fn boom() -> Result[i64, i64] { Err(7_i64) }
fn caller() -> Result[i64, i64] {
    let _ = boom()?;
    Ok(0_i64)
}
fn main() {
    match caller() {
        Ok(_) => println(0_i64),
        Err(e) => println(e),
    }
}
"#,
    );
    if let Some(c) = captured {
        assert_eq!(c.stdout.trim(), "7");
        assert!(
            c.stderr.contains("Error return trace:"),
            "expected trace header on stderr; got {:?}",
            c.stderr
        );
        // One ? site → one frame line in the trace.
        // Frame lines have shape `  <line>:<col>` or `  <file>:<line>:<col>`
        // — indented, contain at least one `:`, and aren't the
        // truncation suffix or the header.
        let frame_lines = c
            .stderr
            .lines()
            .filter(|l| l.starts_with("  ") && l.contains(':') && !l.contains("truncated"))
            .count();
        assert_eq!(
            frame_lines, 1,
            "expected exactly 1 frame, got {} ({:?})",
            frame_lines, c.stderr
        );
    }
}

#[test]
fn test_e2e_question_trace_two_deep_chain() {
    // A two-deep chain of `?` sites should produce two frames, in
    // call-order (innermost frame first since it's pushed first).
    let captured = run_program_capturing(
        r#"
fn level_a() -> Result[i64, i64] { Err(3_i64) }
fn level_b() -> Result[i64, i64] {
    let _ = level_a()?;
    Ok(0_i64)
}
fn level_c() -> Result[i64, i64] {
    let _ = level_b()?;
    Ok(0_i64)
}
fn main() {
    match level_c() {
        Ok(_) => println(0_i64),
        Err(e) => println(e),
    }
}
"#,
    );
    if let Some(c) = captured {
        assert_eq!(c.stdout.trim(), "3");
        assert!(c.stderr.contains("Error return trace:"));
        // Frame lines have shape `  <line>:<col>` or `  <file>:<line>:<col>`
        // — indented, contain at least one `:`, and aren't the
        // truncation suffix or the header.
        let frame_lines = c
            .stderr
            .lines()
            .filter(|l| l.starts_with("  ") && l.contains(':') && !l.contains("truncated"))
            .count();
        assert_eq!(
            frame_lines, 2,
            "expected exactly 2 frames; got {} ({:?})",
            frame_lines, c.stderr
        );
    }
}

#[test]
fn test_e2e_question_trace_cleared_on_recovery() {
    // When a `?` succeeds (Ok-extract), the runtime clears any frames
    // a prior `?` had pushed. A subsequent failure should produce a
    // trace with only the new frames — not stale ones from the
    // recovered earlier propagation.
    let captured = run_program_capturing(
        r#"
fn maybe(flag: bool) -> Result[i64, i64] {
    if flag { Ok(1_i64) } else { Err(9_i64) }
}
fn after_recovery() -> Result[i64, i64] {
    let _ = maybe(true)?;     // success — should clear any pushed frames
    let _ = maybe(false)?;    // fresh failure — pushes one frame
    Ok(0_i64)
}
fn main() {
    match after_recovery() {
        Ok(_) => println(0_i64),
        Err(e) => println(e),
    }
}
"#,
    );
    if let Some(c) = captured {
        assert_eq!(c.stdout.trim(), "9");
        // The trace should have exactly one frame — the second `?`'s,
        // not both. (The first `?`'s frame would have been cleared by
        // the success path.) NOTE: the v1 implementation pushes a frame
        // on the failure block ONLY, so the first `?` (which succeeds)
        // never pushed a frame in the first place. The clear is a
        // safety net for the case where a prior propagation reached
        // this function and was caught higher in the chain.
        // Frame lines have shape `  <line>:<col>` or `  <file>:<line>:<col>`
        // — indented, contain at least one `:`, and aren't the
        // truncation suffix or the header.
        let frame_lines = c
            .stderr
            .lines()
            .filter(|l| l.starts_with("  ") && l.contains(':') && !l.contains("truncated"))
            .count();
        assert_eq!(
            frame_lines, 1,
            "expected exactly 1 frame; got {:?}",
            c.stderr
        );
    }
}

// ── Oversized enum payload — native boxing ──────────────────────
//
// `Option`'s payload area is 3 i64 words, `Result`'s is 5. A struct /
// tuple `T` wider than the area cannot be inlined; it used to silently
// truncate (the spike's `Entity{x,y,hp,label}.pop()` read `label` as
// garbage `8271692032`), then briefly errored (E_ENUM_PAYLOAD_OVERSIZED),
// and is now heap-boxed: malloc `T`, box pointer in word 0. Pack
// (`coerce_to_payload_words`), unpack (`reconstruct_payload_value`,
// the unwrap helper in calls.rs), and drop recompute the same
// `llvm_type_word_count(T) > area` predicate. See
// docs/spikes/oversized-enum-payload.md.

/// The spike's exact repro: a 4-word `Entity` round-trips through
/// `Vec.pop() -> Option[Entity]` and a `Some(e) =>` arm. The field
/// that used to read back as garbage (`label`) must now be correct.
#[test]
fn test_e2e_boxed_option_field_reads_correct_value() {
    let src = r#"
struct Entity { x: i64, y: i64, hp: i64, label: i64 }
fn main() {
    let mut v: Vec[Entity] = Vec.new();
    v.push(Entity { x: 1, y: 2, hp: 3, label: 5000 });
    match v.pop() {
        Some(e) => println(e.label),
        None => println(-1),
    }
}
"#;
    if let Some(out) = run_program(src) {
        assert_eq!(out.trim(), "5000");
    }
}

/// IR shape: an oversized `Option[Entity]` payload emits a `malloc`
/// box at construction (the `enumbox` markers) instead of erroring.
#[test]
fn test_ir_boxes_oversized_option_payload() {
    let src = r#"
struct Entity { x: i64, y: i64, hp: i64, label: i64 }
fn main() {
    let mut v: Vec[Entity] = Vec.new();
    v.push(Entity { x: 1, y: 2, hp: 3, label: 5000 });
    match v.pop() {
        Some(e) => println(e.label),
        None => println(-1),
    }
}
"#;
    let ir = ir_for_with_ownership(src);
    assert!(
        ir.contains("enumbox") && ir.contains("@malloc"),
        "oversized Option payload must box via malloc; got:\n{ir}"
    );
}

/// `Some(big)` constructed directly (no collection op) and read via
/// `.unwrap()` — exercises the calls.rs unbox path with the true area
/// (Option = 3). A 5-word struct (> 3) must box and round-trip.
#[test]
fn test_e2e_boxed_option_unwrap_reads_correct_value() {
    let src = r#"
struct Wide { a: i64, b: i64, c: i64, d: i64, e: i64 }
fn main() {
    let o: Option[Wide] = Some(Wide { a: 1, b: 2, c: 3, d: 4, e: 99 });
    let w = o.unwrap();
    println(w.e);
}
"#;
    if let Some(out) = run_program(src) {
        assert_eq!(out.trim(), "99");
    }
}

/// `Result[Wide, i64]` with a 6-word `Ok` payload (> Result's 5-word
/// area) boxes and round-trips through a `match Ok(w) =>` arm.
#[test]
fn test_e2e_boxed_result_ok_reads_correct_value() {
    let src = r#"
struct Wide6 { a: i64, b: i64, c: i64, d: i64, e: i64, f: i64 }
fn make() -> Result[Wide6, i64] {
    return Ok(Wide6 { a: 1, b: 2, c: 3, d: 4, e: 5, f: 600 });
}
fn main() {
    match make() {
        Ok(w) => println(w.f),
        Err(n) => println(n),
    }
}
"#;
    if let Some(out) = run_program(src) {
        assert_eq!(out.trim(), "600");
    }
}

/// Drop: an annotated `let o: Option[Wide]` frees its heap box at
/// scope exit (the `boxdrop` cleanup). Without it the box leaks
/// (invisible to macOS ASAN, so this IR free-count is the gate).
#[test]
fn test_ir_boxed_option_let_frees_box() {
    let src = r#"
struct Wide { a: i64, b: i64, c: i64, d: i64 }
fn main() {
    let o: Option[Wide] = Some(Wide { a: 1, b: 2, c: 3, d: 4 });
    println(0);
}
"#;
    let ir = ir_for_with_ownership(src);
    assert!(
        ir.contains("boxdrop") && main_free_count(&ir) >= 1,
        "boxed Option let must free its box at scope exit; got:\n{ir}"
    );
}

/// Drop with inner heap: `Option[H]` where `H` owns a `Vec` (5 words,
/// boxed). Scope exit must free BOTH the inner Vec buffer (via the
/// inner struct drop) AND the box — two frees.
#[test]
fn test_ir_boxed_option_let_frees_inner_heap_and_box() {
    let src = r#"
struct H { v: Vec[i64], a: i64, b: i64 }
fn main() {
    let mut vv: Vec[i64] = Vec.new();
    vv.push(7_i64);
    let o: Option[H] = Some(H { v: vv, a: 1, b: 2 });
    println(0);
}
"#;
    let ir = ir_for_with_ownership(src);
    assert!(
        main_free_count(&ir) >= 2,
        "boxed Option[H] must free inner Vec + box (>=2 frees); got:\n{ir}"
    );
}

/// Oversized-enum-payload §3 (untyped-let inference): `let o = make()`
/// with NO annotation, where `make() -> Option[Wide]` (boxed). The box
/// drop is recovered from the callee's return type
/// (`fn_return_type_exprs`) instead of the missing annotation, so the box
/// is freed at scope exit. (Leak gate.)
#[test]
fn test_ir_untyped_let_boxed_option_frees_box() {
    let src = r#"
struct Wide { a: i64, b: i64, c: i64, d: i64 }
fn make() -> Option[Wide] {
    return Some(Wide { a: 1, b: 2, c: 3, d: 4 });
}
fn main() {
    let o = make();
    println(0);
}
"#;
    let ir = ir_for_with_ownership(src);
    assert!(
        ir.contains("boxdrop"),
        "untyped let over a boxed Option-returning call must free its box; got:\n{ir}"
    );
}

/// Oversized-enum-payload §3/§4 (untyped Result.Err boxing): `let r =
/// make()` with no annotation, where `make() -> Result[i64, WideErr]` and
/// `WideErr` is 6 words (> Result's 5-word area), so the `Err` payload is
/// boxed. The box drop for the `Err` variant must be queued from the
/// inferred return type. (Leak gate; the runtime value is `Ok` so the
/// drop's tag guard skips, but the cleanup block is still emitted.)
#[test]
fn test_ir_untyped_let_boxed_result_err_frees_box() {
    let src = r#"
struct WideErr { a: i64, b: i64, c: i64, d: i64, e: i64, f: i64 }
fn make() -> Result[i64, WideErr] {
    return Ok(7);
}
fn main() {
    let r = make();
    println(0);
}
"#;
    let ir = ir_for_with_ownership(src);
    assert!(
        ir.contains("boxdrop"),
        "untyped let over a boxed Result.Err-returning call must free its box; got:\n{ir}"
    );
}

/// Oversized-enum-payload §3 round-trip: an untyped `let o = make()`
/// (boxed `Option[Wide]`) then `match o { … }` reads the 4th word back
/// through the box correctly.
#[test]
fn test_e2e_untyped_let_boxed_option_reads_correct_value() {
    let src = r#"
struct Wide { a: i64, b: i64, c: i64, d: i64 }
fn make() -> Option[Wide] {
    return Some(Wide { a: 1, b: 2, c: 3, d: 400 });
}
fn main() {
    let o = make();
    match o {
        Some(e) => println(e.d),
        None => println(-1),
    }
}
"#;
    if let Some(out) = run_program(src) {
        assert_eq!(out.trim(), "400");
    }
}

#[test]
fn test_e2e_errdefer_fires_on_question_propagation() {
    // The `?` operator's Err-propagation branch is the canonical
    // error-exit path. An `errdefer { ... }` registered upstream
    // of the `?` site fires in the `fail_bb` before the early
    // `ret` — `emit_scope_cleanup_for_error_path` drains in phase
    // order. Compares against the existing
    // `test_e2e_question_triggers_scope_cleanup` test, which only
    // pins that compiler-internal cleanup runs at the `?` site.
    let out = run_program(
        r#"
fn boom() -> Result[i64, i64] { Err(7_i64) }
fn caller() -> Result[i64, i64] {
    errdefer { println("err-cleanup"); }
    let _ = boom()?;
    Ok(0_i64)
}
fn main() {
    match caller() {
        Ok(_) => println("ok"),
        Err(_) => println("caller-err"),
    }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["err-cleanup", "caller-err"]);
    }
}

#[test]
fn test_e2e_errdefer_with_binding_on_question_propagation() {
    // Pin the `?` site's payload staging: when `boom()?`
    // propagates an Err, the `errdefer(e) { ... }` in the caller's
    // scope binds `e` to the i64 payload word (`w0`) extracted
    // from the propagated result struct. Distinguishes the `?`-
    // failure path from the explicit-return path covered above —
    // they share the same `emit_scope_cleanup_for_error_path`
    // drain but stage the payload from different sources
    // (`compile_question`'s `w0` vs `compile_expr`'s recompiled
    // Err arg).
    let out = run_program(
        r#"
fn boom() -> Result[i64, i64] { Err(99_i64) }
fn caller() -> Result[i64, i64] {
    errdefer(e) { println(e); }
    let _ = boom()?;
    Ok(0_i64)
}
fn main() {
    match caller() {
        Ok(_) => println(0_i64),
        Err(_) => println(2_i64),
    }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        // errdefer(e) prints the bound 99 (the propagated payload),
        // then the caller's Err arm prints 2.
        assert_eq!(lines, vec!["99", "2"]);
    }
}

#[test]
fn test_e2e_errdefer_with_binding_sees_wider_e_at_question_propagation() {
    // Slice 4 follow-up (a) — wider-E payload reconstruction at the
    // `?` site (2026-05-26). Pre-(a), `compile_question`'s `fail_bb`
    // staged bare `w0` (the i64-coerced first payload word) for the
    // binding-form errdefer's payload — so a `Result[T, String]`
    // caller's `errdefer(e) { println(e); }` at a `boom()?` site
    // saw `e` as the i64 reinterpretation of String's `data` ptr
    // (garbage from the binding's perspective; would crash on
    // println if the load happened to dereference a non-string-like
    // value).
    //
    // The fix records the current function's source-level Err arm
    // LLVM type at `compile_function` entry (in
    // `current_fn_err_payload_ty`, lowered via
    // `llvm_type_for_type_expr` from the `Result[T, E]`
    // annotation). `compile_question`'s `fail_bb` extracts every
    // available payload word from the result struct (w0/w1/w2 at
    // fields 1/2/3) and calls `rebuild_value_from_payload_words`
    // to reconstruct the source-typed value. For `Result[T, String]`
    // this gives the `{ptr, i64 len, i64 cap}` struct directly —
    // the binding-form errdefer sees the live String value and
    // `println(e)` prints the message.
    //
    // This test pins the canonical wider-E shape (String — 3
    // payload words). Adjacent shapes (Vec, user struct) ride on
    // the same `rebuild_value_from_payload_words` dispatch path
    // and are out of scope for this single test.
    let out = run_program(
        r#"
fn boom() -> Result[i64, String] { Err("kaboom") }
fn caller() -> Result[i64, String] {
    errdefer(e) { println(e); }
    let _ = boom()?;
    Ok(0_i64)
}
fn main() {
    match caller() {
        Ok(_) => println("ok"),
        Err(_) => println("caller-err"),
    }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        // errdefer(e) prints the bound String payload "kaboom"
        // (rebuilt from the result struct's 3 payload words via
        // `rebuild_value_from_payload_words`), then the caller's
        // Err arm prints "caller-err".
        assert_eq!(lines, vec!["kaboom", "caller-err"]);
    }
}

#[test]
fn test_e2e_local_scrutinee_option_field_move_out_runs() {
    // B-2026-08-17-45 — the VALUE half of the move-out fix; the memory half
    // is `asan_boxed_option_local_scrutinee_field_move_out_no_leak_no_double_free`
    // in tests/memory_sanitizer.rs. Both are needed: the defect aborted the
    // process (`free(): double free detected in tcache 2`) rather than
    // printing a wrong answer, so a value assertion alone would have caught
    // it only by the run dying — and a neutralizer that fires too widely
    // frees a live buffer and prints EMPTY here while ASAN stays quiet.
    //
    // The moved-out `C` must survive the arm that handed it out, and the
    // struct it came from carries a second owning field (`other`) that the
    // move did not take, so a too-wide zero shows up as a blank second line
    // rather than as a crash.
    let output = run_program(
        "struct C { name: String, zip: i64 }\n\
             struct A { c: Option[C], other: String }\n\
             fn main() {\n\
                 let a = Option.Some(A {\n\
                     c: Option.Some(C { name: \"moved out payload\", zip: 7 }),\n\
                     other: \"untouched sibling\",\n\
                 });\n\
                 let f = match a {\n\
                     Option.Some(x) => { x.c }\n\
                     Option.None => { Option.None }\n\
                 };\n\
                 match f {\n\
                     Option.Some(cc) => { println(cc.name); println(cc.zip); }\n\
                     Option.None => { println(\"none\"); }\n\
                 }\n\
                 let b = Option.Some(A {\n\
                     c: Option.Some(C { name: \"kept payload\", zip: 3 }),\n\
                     other: \"kept sibling\",\n\
                 });\n\
                 let n = match b {\n\
                     Option.Some(y) => { y.other.len() }\n\
                     Option.None => { 0 }\n\
                 };\n\
                 println(n);\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "moved out payload\n7\n12\n");
}

#[test]
fn test_e2e_user_drop_scrutinee_option_field_move_out_runs() {
    // B-2026-08-18-4 — the user-`Drop` spelling of the row above, which
    // -45's fix deliberately excluded: with an `impl Drop` on the payload a
    // BODIES walk still has to read the box, so -45's move-site zero would
    // have corrupted what that body reads. The zero is queued and emitted
    // between the two readers instead.
    //
    // THE ASSERTION THAT MATTERS IS `other=`. The double free aborted the
    // process, so the run merely surviving proves the memory half; what a
    // value test uniquely catches is the OPPOSITE failure — the zero
    // landing before the Drop body, which prints an EMPTY `other=` while
    // ASAN stays perfectly quiet. That exact trade (a double free for a
    // silently wrong value) is what B-2026-08-06-10's comment recorded
    // having made once, and it is why the body reads a sibling field here
    // rather than just announcing itself.
    //
    // The memory half is
    // `asan_boxed_option_user_drop_field_move_out_no_leak_no_double_free`
    // in tests/memory_sanitizer.rs.
    let output = run_program(
        "struct C { name: String, zip: i64 }\n\
             struct A { c: Option[C], other: String }\n\
             impl Drop for A {\n\
                 fn drop(mut ref self) { println(f\"dropA other={self.other}\"); }\n\
             }\n\
             #[allow(partial_move_of_drop_struct)]\n\
             fn main() {\n\
                 let a = Option.Some(A {\n\
                     c: Option.Some(C { name: \"moved out payload\", zip: 7 }),\n\
                     other: \"untouched sibling\",\n\
                 });\n\
                 let f = match a {\n\
                     Option.Some(x) => { x.c }\n\
                     Option.None => { Option.None }\n\
                 };\n\
                 match f {\n\
                     Option.Some(cc) => { println(cc.name); println(cc.zip); }\n\
                     Option.None => { println(\"none\"); }\n\
                 }\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(
        output,
        "dropA other=untouched sibling\nmoved out payload\n7\n"
    );
}

#[test]
fn test_e2e_ref_param_option_field_consume() {
    // B-2026-07-21-9: the Option-LEAF sibling of the ref-chain family —
    // `match <refparam>.opt { Some(s) => <consume s> … }` over an
    // `Option[String]` / `Option[Vec[U]]` field. The enum clone leg gates
    // seeded Option out (all-None drop kinds, no freshtemp channel), so
    // the inline payload binding aliased the caller's buffer and both
    // freed it (double-free abort on all compiled backends; interp
    // correct). Now `clone_escaping_borrowed_ref_chain_option`
    // deep-clones the Option value (the dispatcher's tag-guarded clone),
    // registers a FreeInlineOptionPayload on the clone slot, and a
    // CONSUMING Some arm zeroes the clone's tag so the binding owns the
    // payload; None / `Some(_)` arms leave the cleanup armed. Covers
    // match / if-let / let-else routes, a String and a Vec payload, the
    // None paths, a non-consuming `Some(_)` arm, and caller reuse.
    let output = run_program(
        "struct Holder { opt: Option[String], vs: Option[Vec[i64]], n: i64 }\n\
             fn render(h: ref Holder) -> String {\n\
                 match h.opt {\n\
                     Some(s) => { return \"o:\".to_string() + s; }\n\
                     None => { return \"none\".to_string(); }\n\
                 }\n\
                 return \"?\".to_string();\n\
             }\n\
             fn has(h: ref Holder) -> i64 {\n\
                 match h.opt {\n\
                     Some(_) => { return 1; }\n\
                     None => { return 0; }\n\
                 }\n\
                 return -1;\n\
             }\n\
             fn total(h: ref Holder) -> i64 {\n\
                 match h.vs {\n\
                     Some(v) => { return v.len() + v[0]; }\n\
                     None => { return 0; }\n\
                 }\n\
                 return -1;\n\
             }\n\
             fn ifl(h: ref Holder) -> String {\n\
                 if let Some(s) = h.opt {\n\
                     return \"if:\".to_string() + s;\n\
                 }\n\
                 return \"none\".to_string();\n\
             }\n\
             fn lel(h: ref Holder) -> String {\n\
                 let Some(s) = h.opt else {\n\
                     return \"none\".to_string();\n\
                 }\n\
                 return \"le:\".to_string() + s;\n\
             }\n\
             fn main() {\n\
                 let a = Holder { opt: Some(\"op\".to_string()), vs: Some([10, 20]), n: 1 };\n\
                 println(render(a));\n\
                 println(render(a));\n\
                 println(has(a));\n\
                 println(total(a));\n\
                 println(total(a));\n\
                 println(ifl(a));\n\
                 println(lel(a));\n\
                 let e = Holder { opt: None, vs: None, n: 2 };\n\
                 println(render(e));\n\
                 println(ifl(e));\n\
                 println(lel(e));\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(
        output,
        "o:op\no:op\n1\n12\n12\nif:op\nle:op\nnone\nnone\nnone\n"
    );
}

#[test]
fn test_e2e_struct_result_field_drop_and_moves() {
    // B-2026-07-21-15: a struct's `Result[String, i64]`-class field
    // payload was NEVER freed at the owning struct's scope-exit drop —
    // `field_copy_supported` deliberately kept every Result field
    // caller-retains (no entry copy existed), so the OptionInline
    // classifier never armed a free and the payload leaked wholesale.
    // Now the DIRECT String/Vec-halves class completes the copy == drop
    // pair: `field_copy_supported` admits it, the by-value entry copy
    // duplicates the live half (`deep_copy_result_inline_heap_halves_
    // in_place`), the struct drop frees it (the classifier's Result
    // extension → `karac_drop_Result_<ok>_<err>`), and every move site
    // zeroes the source payload area (whole-struct move, field move-out,
    // #16 destructure, plus the B-2026-07-21-16 pattern/let/assign
    // legs). Covers: unconsumed drop (the leak), by-value param passing
    // (one holder per call — the harness ownership gate rejects
    // double-consume reuse; the caller-retains reuse angle is covered
    // by the ledger battery) + a consuming match on the callee's copy, a
    // function-returned holder, a struct move + consume, a Vec element
    // holder, a struct-pattern destructure with the leaf consumed, an
    // Err-half scalar, and a both-halves-heap `Result[String, String]`.
    let output = run_program(
        "struct H3 { res: Result[String, i64], n: i64 }\n\
             struct P2 { r: Result[String, String], n: i64 }\n\
             fn take_n(h: H3) -> i64 { return h.n; }\n\
             fn take_s(h: H3) -> String {\n\
                 match h.res {\n\
                     Ok(s) => { return s; }\n\
                     Err(e) => { return e.to_string(); }\n\
                 }\n\
                 return \"?\".to_string();\n\
             }\n\
             fn mk(v: String) -> H3 { return H3 { res: Ok(v), n: 9 }; }\n\
             fn main() {\n\
                 let a1 = H3 { res: Ok(\"aa\".to_string()), n: 1 };\n\
                 println(take_n(a1).to_string());\n\
                 let a2 = H3 { res: Ok(\"aa\".to_string()), n: 1 };\n\
                 println(take_s(a2));\n\
                 let a3 = H3 { res: Ok(\"aa\".to_string()), n: 1 };\n\
                 println(take_s(a3));\n\
                 let b = mk(\"bb\".to_string());\n\
                 println(b.n.to_string());\n\
                 let c = H3 { res: Ok(\"cc\".to_string()), n: 3 };\n\
                 let c2 = c;\n\
                 match c2.res {\n\
                     Ok(s) => { println(\"c:\".to_string() + s); }\n\
                     Err(e) => { println(e.to_string()); }\n\
                 }\n\
                 let mut vs: Vec[H3] = vec![];\n\
                 vs.push(H3 { res: Ok(\"vv\".to_string()), n: 4 });\n\
                 println(vs[0].n.to_string());\n\
                 let d = H3 { res: Ok(\"dd\".to_string()), n: 5 };\n\
                 let H3 { res: r, n: k } = d;\n\
                 println(k.to_string());\n\
                 if let Ok(s) = r { println(\"r:\".to_string() + s); }\n\
                 let e = H3 { res: Err(7), n: 6 };\n\
                 println(take_s(e));\n\
                 let g = P2 { r: Err(\"bad\".to_string()), n: 8 };\n\
                 println(g.n.to_string());\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "1\naa\naa\n9\nc:cc\n4\n5\nr:dd\n7\n8\n");
}

/// B-2026-07-28-16 — the call-argument sibling of the test below, on the
/// OUTPUT side. `consume(nd.opt)` moves an owned struct's `Option`/`Result`
/// field into a parameter that owns it.
///
/// The move-suppression wired at the call site matched an `Identifier`
/// only, so a `FieldAccess` argument was never treated as a move: the
/// callee freed the payload and the owning struct's drop freed it again.
/// Under AOT that aborts; the interpreter was correct throughout, which is
/// why the differential oracle never saw it. The memory verdict lives in
/// `asan_owned_struct_optres_field_call_arg_move_no_leak_no_double_free` —
/// what this pins is the SEMANTICS the source zero implies: consuming the
/// field is a MOVE, so a later read sees `None`, matching the established
/// behaviour for every other field class (B-2026-07-21-16).
#[test]
fn test_e2e_owned_struct_optres_field_call_arg_move() {
    let src = r#"
struct Inner { label: String, k: i64 }
struct Hs { opt: Option[String], n: i64 }
struct Ht { opt: Option[Inner], n: i64 }
struct Hr { res: Result[String, i64], n: i64 }
fn take_str(o: Option[String]) -> String {
    match o {
        Some(s) => s,
        None => "none".to_string(),
    }
}
fn take_struct(o: Option[Inner]) -> String {
    match o {
        Some(x) => x.label,
        None => "none".to_string(),
    }
}
fn take_res(r: Result[String, i64]) -> String {
    match r {
        Ok(s) => s,
        Err(e) => e.to_string(),
    }
}
fn main() {
    let a = Hs { opt: Some("payload".to_string()), n: 1 };
    println(take_str(a.opt));

    let b = Ht { opt: Some(Inner { label: "deep".to_string(), k: 2 }), n: 2 };
    println(take_struct(b.opt));

    let r = Hr { res: Ok("okstr".to_string()), n: 3 };
    println(take_res(r.res));

    let c = Hs { opt: None, n: 4 };
    println(take_str(c.opt));

    let d = Ht { opt: Some(Inner { label: "kept".to_string(), k: 5 }), n: 5 };
    println(d.n.to_string());
    match d.opt {
        Some(x) => println(x.label),
        None => println("none"),
    }
}
"#;
    let out = run_program(src).expect("optres field call-arg move program should run");
    let got: Vec<&str> = out.lines().collect();
    assert_eq!(got[0], "payload", "String payload did not reach the callee");
    assert_eq!(got[1], "deep", "struct payload did not reach the callee");
    assert_eq!(got[2], "okstr", "Result payload did not reach the callee");
    assert_eq!(got[3], "none", "a None field must stay None");
    assert_eq!(
        got[4], "5",
        "an unconsumed field must leave its struct intact"
    );
    assert_eq!(
        got[5], "kept",
        "an unconsumed payload must still be readable"
    );
}

#[test]
fn test_e2e_owned_struct_optres_field_consume_and_move() {
    // B-2026-07-21-16: consuming (or even print-only-binding) match /
    // if-let / let-else DIRECTLY over an OWNED struct's Option field,
    // and let/assign moves of such a field, double-freed: the binding's
    // own free plus the struct drop's OptionInline arm hit the same
    // buffer (the Facet A move-site zeroing never covered these sites).
    // Now the consuming arm / let / assign zero the SOURCE field (Option
    // tag to None; Result payload area), making the move real. Result
    // shapes ride the same legs (load-bearing once B-2026-07-21-15 arms
    // the struct-drop Result free; the drained `let dr = h.res` route
    // also gives dr a real cleanup registration today). Non-binding arms
    // leave the source to the struct drop.
    let output = run_program(
        "struct H2 { opt: Option[String], n: i64 }\n\
             struct H3 { res: Result[String, i64], n: i64 }\n\
             fn main() {\n\
                 let a = H2 { opt: Some(\"ma\".to_string()), n: 1 };\n\
                 match a.opt {\n\
                     Some(s) => { println(\"m:\".to_string() + s); }\n\
                     None => { println(\"none\".to_string()); }\n\
                 }\n\
                 let b = H2 { opt: Some(\"if\".to_string()), n: 2 };\n\
                 if let Some(s) = b.opt {\n\
                     println(\"i:\".to_string() + s);\n\
                 }\n\
                 let c = H2 { opt: Some(\"le\".to_string()), n: 3 };\n\
                 let Some(t) = c.opt else {\n\
                     return;\n\
                 }\n\
                 println(\"l:\".to_string() + t);\n\
                 let d = H2 { opt: Some(\"mv\".to_string()), n: 4 };\n\
                 let x = d.opt;\n\
                 if let Some(s) = x {\n\
                     println(\"x:\".to_string() + s);\n\
                 }\n\
                 let e = H2 { opt: Some(\"as\".to_string()), n: 5 };\n\
                 let mut y: Option[String] = None;\n\
                 y = e.opt;\n\
                 if let Some(s) = y {\n\
                     println(\"y:\".to_string() + s);\n\
                 }\n\
                 let g = H3 { res: Ok(\"rr\".to_string()), n: 6 };\n\
                 match g.res {\n\
                     Ok(s) => { println(\"g:\".to_string() + s); }\n\
                     Err(e2) => { println(e2.to_string()); }\n\
                 }\n\
                 let h = H2 { opt: Some(\"wc\".to_string()), n: 7 };\n\
                 match h.opt {\n\
                     Some(_) => { println(\"has\".to_string()); }\n\
                     None => { println(\"no\".to_string()); }\n\
                 }\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "m:ma\ni:if\nl:le\nx:mv\ny:as\ng:rr\nhas\n");
}

#[test]
fn test_e2e_ref_param_result_field_consume() {
    // B-2026-07-21-14: the RESULT-leaf sibling of the ref-chain family —
    // `match <refparam>.res { Ok(s) => <consume s> … }` over a
    // `Result[String, i64]` / `Result[Vec[i64], i64]` field. The Option
    // clone leg keys on the `Option` head, so a Result leaf's payload
    // binding aliased the caller's buffer and both freed it (double-free
    // abort on all compiled backends; interp correct). Now
    // `clone_escaping_borrowed_ref_chain_result` deep-clones the live
    // half in place (`deep_copy_result_inline_heap_halves_in_place`),
    // registers the clone's FreeInlineResultPayload, and a CONSUMING
    // Ok/Err arm zeroes the clone's payload area so the binding owns the
    // buffer; a no-bind arm leaves the cleanup armed. Covers match /
    // if-let / let-else routes, a String and a Vec Ok half, a scalar Err
    // half (both live tags), a non-consuming `Ok(_)` arm, and caller
    // reuse. The drain epilogue moves each holder's field out at the end
    // because a struct's scope-exit drop does not free a `Result` field
    // payload (the deliberate caller-retains exclusion in
    // `field_copy_supported` — tracked on the ledger as its own bug).
    let output = run_program(
        "struct Holder { res: Result[String, i64], vres: Result[Vec[i64], i64], n: i64 }\n\
             fn render(h: ref Holder) -> String {\n\
                 match h.res {\n\
                     Ok(s) => { return \"ok:\".to_string() + s; }\n\
                     Err(e) => { return \"err:\".to_string() + e.to_string(); }\n\
                 }\n\
                 return \"?\".to_string();\n\
             }\n\
             fn code(h: ref Holder) -> i64 {\n\
                 match h.res {\n\
                     Ok(_) => { return 1; }\n\
                     Err(e) => { return e; }\n\
                 }\n\
                 return -1;\n\
             }\n\
             fn total(h: ref Holder) -> i64 {\n\
                 match h.vres {\n\
                     Ok(v) => { return v.len() + v[0]; }\n\
                     Err(e) => { return e; }\n\
                 }\n\
                 return -1;\n\
             }\n\
             fn ifl(h: ref Holder) -> String {\n\
                 if let Ok(s) = h.res {\n\
                     return \"if:\".to_string() + s;\n\
                 }\n\
                 return \"no\".to_string();\n\
             }\n\
             fn lel(h: ref Holder) -> String {\n\
                 let Ok(s) = h.res else {\n\
                     return \"no\".to_string();\n\
                 }\n\
                 return \"le:\".to_string() + s;\n\
             }\n\
             fn main() {\n\
                 let a = Holder { res: Ok(\"rr\".to_string()), vres: Ok([10, 20]), n: 1 };\n\
                 println(render(a));\n\
                 println(render(a));\n\
                 println(code(a));\n\
                 println(total(a));\n\
                 println(total(a));\n\
                 println(ifl(a));\n\
                 println(lel(a));\n\
                 let b = Holder { res: Err(7), vres: Err(9), n: 2 };\n\
                 println(render(b));\n\
                 println(code(b));\n\
                 println(total(b));\n\
                 println(ifl(b));\n\
                 println(lel(b));\n\
                 let dra = a.res;\n\
                 if let Ok(s) = dra { println(\"d:\".to_string() + s); }\n\
                 let drv = a.vres;\n\
                 if let Ok(v) = drv { println(v[1]); }\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(
        output,
        "ok:rr\nok:rr\n1\n12\n12\nif:rr\nle:rr\nerr:7\n7\n9\nno\nno\nd:rr\n20\n"
    );
}

#[test]
fn test_e2e_file_open_question_propagates_not_found() {
    // `File.open` on a nonexistent path returns
    // `Err(IoError.NotFound)`; `?` propagates that Err verbatim to
    // the helper's caller. The match arm in `main` then matches
    // the propagated variant — pins the IoError tag pass-through.
    let out = run_program(
        r#"
fn try_open(path: String) -> Result[i64, IoError] with reads(FileSystem) {
    let _f = File.open(path)?;
    Ok(0_i64)
}
fn main() with reads(FileSystem) {
    match try_open("/nonexistent_karac_f5_test.txt") {
        Ok(_) => println("unexpected-ok"),
        Err(e) => match e {
            IoError.NotFound => println("propagated-NotFound"),
            _ => println("propagated-other"),
        },
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "propagated-NotFound");
    }
}

#[test]
fn test_e2e_lowercase_fs_read_lines_question_unwrap() {
    // Lowercase `fs.read_lines(path)?` — the ambient-alias path
    // (`ambient_resource_for_alias("fs")` → `compile_ambient_ffi`'s
    // `("FileSystem", "read_lines")` arm → `compile_fs_read_lines_val`).
    // The `?`-unwrap binds a `Vec[String]` local whose per-element
    // String buffers must be freed at scope exit (B-38 leak caveat);
    // summing the line lengths exercises a full drain + drop.
    let tmp = std::env::temp_dir().join("karac_e2e_lc_fs_read_lines.txt");
    let _ = std::fs::remove_file(&tmp);
    std::fs::write(&tmp, b"first line\nsecond\n\nfourth line\n").expect("temp write");
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    let src = format!(
        r#"
fn total_len(path: String) -> Result[i64, IoError] with reads(FileSystem) {{
    let lines = fs.read_lines(path)?;
    let mut total = 0;
    for line in lines {{
        total = total + line.len();
    }}
    Ok(total)
}}
fn main() with reads(FileSystem) {{
    match total_len("{path}") {{
        Ok(n) => println(n.to_string()),
        Err(_) => println("read-err"),
    }}
}}
"#
    );
    let out = run_program(&src);
    let _ = std::fs::remove_file(&tmp);
    if let Some(out) = out {
        // "first line"(10) + "second"(6) + ""(0) + "fourth line"(11) = 27
        assert_eq!(out.trim(), "27");
    }
}

#[test]
fn test_e2e_file_open_question_passes_through_ok() {
    // Successful File.open returns `Ok(File)`; `?` extracts the
    // File and the helper's terminal `Ok(0_i64)` propagates as the
    // caller's Result. Pins that the Ok path doesn't accidentally
    // wrap the success as Err. Uses the host OS to create the
    // tempfile (test-harness pre-condition), then exercises the
    // compiled Kāra path.
    let tmp = std::env::temp_dir().join("karac_e2e_file_f5_open_ok.txt");
    let _ = std::fs::remove_file(&tmp);
    std::fs::write(&tmp, b"seed").expect("temp write");
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    let src = format!(
        r#"
fn try_open(path: String) -> Result[i64, IoError] with reads(FileSystem) {{
    let _f = File.open(path)?;
    Ok(0_i64)
}}
fn main() with reads(FileSystem) {{
    match try_open("{path}") {{
        Ok(_) => println("ok-passthrough"),
        Err(_) => println("unexpected-err"),
    }}
}}
"#
    );
    let out = run_program(&src);
    if let Some(out) = out {
        assert_eq!(out.trim(), "ok-passthrough");
    }
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_e2e_unsigned_refinement_as_and_try_from_accept() {
    // The other two refinement enforcement sites share the predicate
    // path, so they shared the bug.
    let out = run_program(
        r#"
type P = u16 where self >= 1 and self <= 65535;
fn main() {
    let z: u16 = 80;
    let a = z as P;
    println("as-ok");
    match P.try_from(80) { Ok(v) => println("tryfrom-ok"), Err(e) => println("tryfrom-err") }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "as-ok\ntryfrom-ok");
    }
}

#[test]
fn test_e2e_refinement_try_from_ok_branch() {
    // `Even.try_from(4)` builds `Ok(4)` at runtime; the match prints it.
    let out = run_program(
        r#"
type Even = i64 where self % 2 == 0;
fn main() {
    match Even.try_from(4) {
        Ok(v) => println(v),
        Err(_) => println(-1),
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "4");
    }
}

#[test]
fn test_e2e_refinement_try_from_err_branch() {
    // `Even.try_from(3)` builds `Err(...)`; the match takes the Err arm
    // (no abort — try_from is the recoverable construction form).
    let out = run_program(
        r#"
type Even = i64 where self % 2 == 0;
fn main() {
    match Even.try_from(3) {
        Ok(v) => println(v),
        Err(_) => println(-1),
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "-1");
    }
}

#[test]
fn test_e2e_distinct_where_try_from_branches() {
    // `Even.try_from` builds `Ok`/`Err` with no abort (recoverable form).
    let ok = run_program(
        r#"
distinct type Even = i64 where self % 2 == 0;
fn main() {
    match Even.try_from(8) { Ok(e) => println(e.raw()), Err(_) => println(-1) }
}
"#,
    );
    if let Some(out) = ok {
        assert_eq!(out.trim(), "8");
    }
    let err = run_program(
        r#"
distinct type Even = i64 where self % 2 == 0;
fn main() {
    match Even.try_from(7) { Ok(e) => println(e.raw()), Err(_) => println(-1) }
}
"#,
    );
    if let Some(out) = err {
        assert_eq!(out.trim(), "-1");
    }
}

#[test]
fn test_e2e_optres_payload_bodies_in_nested_positions() {
    // B-2026-08-03-1 — an `Option[P]` payload ran its Drop body only when
    // bound DIRECTLY; in every NESTED position it was silent on BOTH
    // backends, so parity diffing saw nothing and (memory being clean) no
    // sanitizer did either. Option/Result were simply missing from the
    // one-container-level widening Vec/Map/Set/tuple already had, in all
    // three layers: the reachability gates, the field-index selector, and
    // the per-position dispatchers. Four positions plus a `None` control
    // that must stay silent.
    let out = run_program(
        r#"
struct Res { id: i64, name: String }
impl Drop for Res {
    fn drop(mut ref self) { println(f"drop {self.id} {self.name}") }
}
struct H { o: Option[Res], t: i64 }
fn main() {
    println("field:");
    {
        let h = H { o: Option.Some(Res { id: 1, name: f"a{1}" }), t: 2 };
        println(h.t);
    }
    println("vecelem:");
    {
        let mut v: Vec[Option[Res]] = Vec.new();
        v.push(Option.Some(Res { id: 2, name: f"b{2}" }));
        println(v.len());
    }
    println("mapval:");
    {
        let mut m: Map[i64, Option[Res]] = Map.new();
        m.insert(5, Option.Some(Res { id: 3, name: f"c{3}" }));
        println(m.len());
    }
    println("tupelem:");
    {
        let t = (Option.Some(Res { id: 4, name: f"d{4}" }), 7);
        println(t.1);
    }
    println("none:");
    {
        let h2 = H { o: Option.None, t: 9 };
        println(h2.t);
    }
    println("end");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out.trim(),
            "field:\n2\ndrop 1 a1\nvecelem:\n1\ndrop 2 b2\nmapval:\n1\n\
                 drop 3 c3\ntupelem:\n7\ndrop 4 d4\nnone:\n9\nend"
        );
    }
}

// ── Static branch hints from effect analysis (`llvm.expect`) ─────────
//
// The `?` operator's failure arm is the cold path — an ordinary early
// return (not a panic, so no already-`cold` callee to signal it) — so its
// tag-is-failure condition is wrapped in `llvm.expect.i1(cond, false)` to
// lay out the Ok continuation as the hot fall-through. Advisory only.
#[test]
fn question_operator_emits_expect_hint() {
    let ir = ir_for(
        r#"
fn maybe() -> Result[i64, String] { return Ok(5); }
fn use_it() -> Result[i64, String] {
    let n = maybe()?;
    return Ok(n + 1);
}
fn main() { print(0); }
"#,
    );
    let body = fn_body(&ir, "@use_it(");
    // The `?` branch condition is the `llvm.expect.i1(..., false)` result.
    assert!(
        body.contains("call i1 @llvm.expect.i1(") && body.contains("i1 false"),
        "`?` failure arm should be hinted unlikely via llvm.expect:\n{body}"
    );
    assert!(
        body.contains("br i1 %expect,"),
        "the `?` conditional branch should use the expect result:\n{body}"
    );
}

/// slice-3c-iv boxed-`Option`-binding move-into-literal fix: a wide
/// (heap-boxed) `Option[Wide]` LOCAL moved whole into a struct-literal
/// field must have its source slot zeroed so the binding's
/// `BoxedEnumDrop` no-ops — the returned struct now solely owns the box.
/// Without it the builder frees the box at scope exit while the returned
/// value still references it (UAF; selfhost slice 3c-iv's
/// `TraitMethodNode { body, .. }` for `let mut body = Some(parse_block())`).
/// E2E coverage: `tests/memory_sanitizer.rs::asan_boxed_option_moved_into_struct_literal_no_uaf`.
#[test]
fn boxed_option_binding_moved_into_literal_zeroes_source() {
    // A `let mut body = None; if … { body = Some(mk()) }` binding registers
    // a scope-exit `BoxedEnumDrop` (the box may or may not exist at the end
    // — a definite `let body = Some(mk())` is instead a proven move-out with
    // no drop). Moving that binding into `Holder { body }` must therefore
    // ZERO the source slot so the still-emitted drop no-ops — mirrors
    // selfhost `parse_trait_method`'s `let mut body … TraitMethodNode{body}`.
    let ir = ir_for(
        "struct Wide { a: i64, b: i64, c: i64, d: i64, e: String }\n\
             struct Holder { tag: i64, body: Option[Wide] }\n\
             fn mk() -> Wide { Wide { a: 1, b: 2, c: 3, d: 4, e: \"x\".to_string() } }\n\
             fn build(flag: bool) -> Holder {\n\
             \x20   let mut body: Option[Wide] = None;\n\
             \x20   if flag { body = Some(mk()); }\n\
             \x20   Holder { tag: 0, body: body }\n\
             }\n\
             fn main() { let h = build(true); println(h.tag); }\n",
    );
    let body = function_body(&ir, "build").expect("fn build must be emitted");
    // The box is constructed (Option[Wide] is wide → heap-boxed) and a
    // scope-exit BoxedEnumDrop guards on the tag (`boxdrop` / inner
    // `__karac_drop_struct_Wide`).
    let inner_drop = body
        .find("@__karac_drop_struct_Wide")
        .or_else(|| body.find("boxdrop"));
    assert!(
        inner_drop.is_some(),
        "Option[Wide] must heap-box and register a BoxedEnumDrop\n--- body ---\n{body}"
    );
    // The move-suppression zeroes the source Option slot so that drop reads
    // `tag == None` and skips the free the returned Holder now owns.
    let src_zero = body.find("store { i64, i64, i64, i64 } zeroinitializer, ptr %body");
    assert!(
        src_zero.is_some(),
        "moving the boxed `body` into `Holder {{ .. }}` must zero the source \
             Option slot so its BoxedEnumDrop no-ops\n--- body ---\n{body}"
    );
    assert!(
        src_zero < inner_drop,
        "source-zero must precede the boxed payload's drop\n--- body ---\n{body}"
    );
}

#[test]
fn test_e2e_result_struct_payload_field_freed_and_single_body() {
    // B-2026-08-03-3 leg B — a `Result[<Drop struct>, E]` STRUCT FIELD was
    // never freed in ANY position (local binding, by-value param, returned
    // from a fn, `let x = h.r` move-out), because the `OptionInline`
    // classifier's Result arm admitted only the DIRECT String/Vec-halves
    // class. Arming the free needed its entry-copy peer, and attempt 1 —
    // which factored the payload copy out of the Option twin — double-freed:
    // the `Result` payload AREA is 5 words (`declarations.rs`'s
    // `result_payload_words`, the threshold `coerce_to_payload_words` packs
    // at and `emit_result_drop_fn` frees at) while `Option`'s is 3, so the
    // canonical 4-word `Res` is INLINE in a `Result` and BOXED in an
    // `Option`. Copying it at Option's `> 3` read `id` as a box pointer.
    //
    // Two neutralizers had to land with the free. `zero_result_payload_area`
    // is not the dual of `zero_option_field_tag_at`: it leaves the tag
    // selecting the live arm, which every cap-guarded memory free tolerates
    // but the BODIES walk does not — a consuming `match h.r` printed a
    // spurious `drop 0 ` over the zeroed slot. And a `match p.field` inside
    // a by-value param binds out of the callee's entry copy exactly as a
    // bare `match p` does, so its body belongs to the caller's fire; the
    // owned-param test only looked at a bare Identifier.
    //
    // `err-struct-side` pins that the admit is per-half (both sides
    // structs), and `mixed-halves` that a String half keeps the shape OUT of
    // this class entirely — neither gate admits it, so it stays status quo.
    let out = run_program(
        r#"
struct Res { id: i64, name: String }
impl Drop for Res { fn drop(mut ref self) { println(f"drop {self.id} {self.name}") } }
struct H { r: Result[Res, i64], t: i64 }
struct Hm { r: Result[Res, String], t: i64 }
fn take(h: H) -> i64 { h.t }
fn mk() -> H { H { r: Result.Ok(Res { id: 2, name: f"bb{2}" }), t: 20 } }
fn consume(h: H) -> i64 { match h.r { Result.Ok(x) => x.id, Result.Err(e) => e } }
fn main() {
    println("binding:");
    { let h = H { r: Result.Ok(Res { id: 1, name: f"a{1}" }), t: 10 }; println(h.t); }
    println("returned:");
    { let h = mk(); println(h.t); }
    println("byvalue:");
    { let h = H { r: Result.Ok(Res { id: 3, name: f"ccc{3}" }), t: 30 }; println(take(h)); }
    println("moveout:");
    { let h = H { r: Result.Ok(Res { id: 4, name: f"dddd{4}" }), t: 40 }; let x = h.r; println(h.t); }
    println("local-match:");
    { let h = H { r: Result.Ok(Res { id: 5, name: f"eeeee{5}" }), t: 50 };
      let v = match h.r { Result.Ok(x) => x.id, Result.Err(e) => e }; println(v); }
    println("param-match:");
    { let h = H { r: Result.Ok(Res { id: 6, name: f"ffffff{6}" }), t: 60 }; println(consume(h)); }
    println("mixed-halves:");
    { let h = Hm { r: Result.Err(f"ggggggg{7}"), t: 70 }; println(h.t); }
    println("end");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out.trim(),
            "binding:\n10\ndrop 1 a1\n\
                 returned:\n20\ndrop 2 bb2\n\
                 byvalue:\n30\ndrop 3 ccc3\n\
                 moveout:\ndrop 4 dddd4\n40\n\
                 local-match:\ndrop 5 eeeee5\n5\n\
                 param-match:\n6\ndrop 6 ffffff6\n\
                 mixed-halves:\n70\nend"
        );
    }
}

#[test]
fn test_e2e_mixed_halves_result_struct_field_freed() {
    // B-2026-08-03-11 — the hole BETWEEN the two Result field-drop gates.
    // Leg B of B-2026-08-03-3 armed the free for a Result whose heap-owning
    // halves are all structs/enums, and kept that gate structurally
    // disjoint from the older direct-String/Vec-halves gate so each class
    // binds to exactly one entry-copy helper. A field typed
    // `Result[Res, String]` has one heap half of EACH kind, so both gates
    // rejected it and the struct payload's buffer was orphaned.
    //
    // Both free sides already dispatched per half (`emit_result_drop_fn`
    // branches on Ok and Err independently; `track_inline_result_payload_var`
    // pairs the overlay elems with the struct drops), so the fix is the
    // admit gate plus a per-half entry copy: the struct/enum half keeps the
    // boxed-or-inline dance at the Result area's `> 5`, and the direct half
    // routes to the same `{ptr,len,cap}` overlay copy the all-direct class
    // uses, now factored into `emit_result_half_overlay_copy` so the two
    // cannot drift.
    //
    // `swapped-sides` is the half-order control — the same mix with the Vec
    // on Ok and the struct on Err — because the admit is per half, not per
    // position.
    //
    // This is a COMPANION guard, not the row's oracle: it is green before
    // the fix too, because a pure leak changes no output. Its job is the
    // other direction — arming a free is what turns every position that
    // already consumes the value into a double-free or double-fire
    // candidate (the lesson B-2026-08-03-3 records three times over), so
    // these six positions pin that arming this class did not do that.
    // `asan_mixed_halves_result_struct_field_freed` is the leak oracle and
    // is stash-proven RED (123 bytes in 6 allocations under LSan).
    let out = run_program(
        r#"
struct Res { id: i64, name: String }
impl Drop for Res { fn drop(mut ref self) { println(f"drop {self.id} {self.name}") } }
struct Hm { r: Result[Res, String], t: i64 }
struct Hv { r: Result[Vec[String], Res], t: i64 }
fn take(h: Hm) -> i64 { h.t }
fn main() {
    println("ok-struct:");
    { let h = Hm { r: Result.Ok(Res { id: 1, name: f"aaaa{1}" }), t: 10 }; println(h.t); }
    println("err-string:");
    { let h = Hm { r: Result.Err(f"bbbbbb{2}"), t: 20 }; println(h.t); }
    println("byvalue-ok-struct:");
    { let h = Hm { r: Result.Ok(Res { id: 3, name: f"ccc{3}" }), t: 30 }; println(take(h)); }
    println("moveout-ok-struct:");
    { let h = Hm { r: Result.Ok(Res { id: 4, name: f"dddd{4}" }), t: 40 }; let x = h.r; println(h.t); }
    println("swapped-sides:");
    {
      let mut v: Vec[String] = Vec.new();
      v.push(f"eeeee{5}");
      let h = Hv { r: Result.Ok(v), t: 50 };
      println(h.t);
    }
    println("swapped-sides-err:");
    { let h = Hv { r: Result.Err(Res { id: 6, name: f"ffffff{6}" }), t: 60 }; println(h.t); }
    println("end");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out.trim(),
            "ok-struct:\n10\ndrop 1 aaaa1\n\
                 err-string:\n20\n\
                 byvalue-ok-struct:\n30\ndrop 3 ccc3\n\
                 moveout-ok-struct:\ndrop 4 dddd4\n40\n\
                 swapped-sides:\n50\n\
                 swapped-sides-err:\n60\ndrop 6 ffffff6\nend"
        );
    }
}

#[test]
fn test_e2e_tuple_held_optres_payload_freed_and_dropped() {
    // B-2026-08-03-3 — an `Option[P]` / `Result[O, E]` held INSIDE A TUPLE
    // never freed its payload's heap: `emit_tuple_elem_drops` had a hard
    // `"Option" | "Result" => {}` no-op, and every admit gate above it went
    // through `type_expr_has_drop_heap`, which reads Option/Result as
    // heapless by design. The direct `Vec[Option[Res]]` control was clean
    // all along, which is what made the tuple hole invisible. All six
    // positions leaked 2+ bytes each under valgrind before the fix; the last
    // two (a Vec of such tuples, and a NESTED tuple) were also SILENT on one
    // backend, because the walker's selector read only inner head names.
    let out = run_program(
        r#"
struct Res { id: i64, name: String }
impl Drop for Res {
    fn drop(mut ref self) { println(f"drop {self.id} {self.name}") }
}
fn take(t: (Option[Res], i64)) -> i64 { t.1 }
fn mk() -> (Option[Res], i64) { (Option.Some(Res { id: 3, name: f"c{3}" }), 30) }
fn main() {
    println("binding:");
    { let t = (Option.Some(Res { id: 1, name: f"a{1}" }), 10); println(t.1); }
    println("param:");
    { let t = (Option.Some(Res { id: 2, name: f"bb{2}" }), 20); println(take(t)); }
    println("returned:");
    { let t = mk(); println(t.1); }
    println("result:");
    { let t: (Result[Res, i64], i64) = (Result.Ok(Res { id: 4, name: f"dddd{4}" }), 40); println(t.1); }
    println("vec-of-tuple:");
    {
        let mut v: Vec[(Option[Res], i64)] = Vec.new();
        v.push((Option.Some(Res { id: 5, name: f"eeeee{5}" }), 50));
        println(v.len());
    }
    println("nested-tuple:");
    { let t = ((Option.Some(Res { id: 6, name: f"ffffff{6}" }), 60), 600); println(t.1); }
    println("end");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out.trim(),
            "binding:\n10\ndrop 1 a1\nparam:\n20\ndrop 2 bb2\n\
                 returned:\n30\ndrop 3 c3\nresult:\n40\ndrop 4 dddd4\n\
                 vec-of-tuple:\n1\ndrop 5 eeeee5\n\
                 nested-tuple:\n600\ndrop 6 ffffff6\nend"
        );
    }
}

/// B-2026-09-02-12 — AN `Option`/`Result` TEMPORARY THE PATTERN DECLINED
/// RAN ITS PAYLOAD'S `Drop` BODY ON NO SURFACE.
///
/// `if let Ok(w) = mkerr()` builds a `Result[W, W]` holding `Err(W { .. })`,
/// the arm does not take it, and the temporary dies right there — so `W`'s
/// body is due, exactly as it is for `mkerr();`, the discard spelling of
/// the same value one line away, which has always run it. It ran nowhere,
/// on all four surfaces, through all three `let`-family spellings.
///
/// AGREED-AND-WRONG, so no A/B gate could see it: `karac run`, `karac build`
/// and `--interp` printed the same missing body, and the storage IS
/// reclaimed, so it is a lost side effect rather than a leak. Only an
/// absolute expectation catches this class, which is what this fixture is.
///
/// THE CARVE-OUT WAS RIGHT ABOUT THE WALKER AND WRONG ABOUT THE OUTCOME.
/// Both backends routed `Option`/`Result` past the payload-bodies walk with
/// a note that the two "keep their own payload machinery". The walker they
/// were being kept away from is declared-type driven and genuinely cannot
/// see through `Ok(T)` / `Err(E)` — those payloads are the enum's own
/// generic parameters — so sending them there would have been a no-op. But
/// sending them NOWHERE lost the body. Both sides now resolve the payload
/// through the INSTANTIATION the type-checker recorded
/// (`emit_optres_payload_user_drop_bodies_fn` / the value-driven
/// `Option`/`Result` arm of `run_discarded_value_user_drops`), which is the
/// same resolution the discard spelling already used — so the two spellings
/// agree by construction rather than by convention.
///
/// `let ... else` WAS ONE OF THE ROW'S UNMEASURED SHAPES. It is affected
/// and closes with the other two.
///
/// FRESH TEMPORARIES ONLY, and the last two rows here are why. A NAMED
/// local reaching a miss edge still owns its value and has its own walk to
/// run the body at its own death — firing here as well would double it. The
/// gate is unchanged; what changed is that the local's walk now survives to
/// the miss edge at all. It used to be retracted statically before the match
/// test, so the local lost its body too, for an unrelated reason — split out
/// as B-2026-09-02-14 and closed by making that retraction edge-sensitive
/// (a per-path flag cleared in the arm's own block). The two rows below pin
/// the local's body and the walk that produces it, so a regression in either
/// direction — a lost body, or this fixture's fire doubling it — fails here.
#[test]
fn e2e_a_declined_optres_temporary_runs_its_payload_drop_body() {
    const PRELUDE: &str = "struct W { id: i64 }\n\
             impl Drop for W { fn drop(mut ref self) { println(f\"dW{self.id}\"); } }\n\
             fn mkr(n: i64) -> Result[W, W] {\n\
             if n < 1 { return Ok(W { id: n }); }\n\
             return Err(W { id: 7 });\n\
             }\n\
             fn mkerr() -> Result[W, W] { return Err(W { id: 7 }); }\n\
             fn mksome() -> Option[W] { return Some(W { id: 3 }); }\n\
             fn mknone() -> Option[W] { return None; }\n";
    let cases: &[(&str, &str, &str)] = &[
        (
            "if let, the temporary the arm declined",
            "if let Ok(w) = mkerr() { println(f\"v{w.id}\"); }",
            "dW7\nafter\n",
        ),
        (
            // TWO `W`s are constructed: the one the last pass bound, and
            // the `Err` that ended the loop. Both bodies are due.
            "while let, the temporary that ends the loop",
            "let mut i: i64 = 0;\n\
                 while let Ok(w) = mkr(i) {\n\
                 println(f\"v{w.id}\");\n\
                 i = i + 1;\n\
                 }",
            "v0\ndW0\ndW7\nafter\n",
        ),
        (
            // The row left this spelling unmeasured. The body lands BEFORE
            // the else block, per design.md § "Scrutinee temporary scope":
            // scrutinee temporaries are dropped before the divergent block
            // runs.
            "let ... else, the temporary the pattern declined",
            "let Ok(w) = mkerr() else { println(\"miss\"); return };\n\
                 println(f\"v{w.id}\");",
            "dW7\nmiss\n",
        ),
        (
            // The `Option` half of the shape. A `None` carries nothing, so
            // the payload has to sit in the variant the pattern REJECTS —
            // here by matching `None` against a `Some`.
            "an `Option` payload declined by a `None` pattern",
            "if let None = mksome() { println(\"none\"); } else { println(\"some\"); }",
            "dW3\nsome\nafter\n",
        ),
        (
            // THE GATE on the hit edge: the arm's binding owns the payload
            // and runs the body itself, so the miss-edge call must not be
            // emitted there or every matching `if let` doubles.
            "control: the same temporary when the arm DOES match",
            "if let Ok(w) = mkr(0) { println(f\"v{w.id}\"); }",
            "v0\ndW0\nafter\n",
        ),
        (
            "control: the `let ... else` hit edge",
            "let Ok(w) = mkr(0) else { println(\"miss\"); return };\n\
                 println(f\"v{w.id}\");",
            "v0\ndW0\nafter\n",
        ),
        (
            "control: `match` binding both arms, correct throughout",
            "match mkerr() { Ok(w) => println(f\"v{w.id}\"), Err(e) => println(f\"e{e.id}\") }",
            "e7\ndW7\nafter\n",
        ),
        (
            "control: a declined `None` carries nothing, so nothing is due",
            "if let Some(w) = mknone() { println(f\"v{w.id}\"); }",
            "after\n",
        ),
        (
            // A borrow accessor's `Option` payload ALIASES a container
            // element the container still owns and still walks, so the
            // miss-edge call must decline — this row is that exclusion.
            "control: a borrow accessor (`v.pop()`) on an empty container",
            "let mut v: Vec[W] = Vec.new();\n\
                 if let Some(w) = v.pop() { println(f\"v{w.id}\"); }",
            "after\n",
        ),
        (
            // B-2026-09-02-14 — a NAMED local is not a temporary, so the
            // miss-edge fire above declines and the local's OWN walk runs
            // the body, at the local's real death (the `if let`, its last
            // use). It printed nothing until that row made the arm
            // retraction edge-sensitive.
            "a BOUND local, not a temporary [B-2026-09-02-14]",
            "let r: Result[W, W] = mkerr();\n\
                 if let Ok(w) = r { println(f\"v{w.id}\"); }",
            "dW7\nafter\n",
        ),
        (
            // …and the shape that proves the bound local's walk exists at
            // all, which is what makes the row above a retraction bug
            // rather than a missing registration.
            "control: the same bound local with no `if let` at all",
            "let r: Result[W, W] = mkerr();\n\
                 println(\"x\");",
            "dW7\nx\nafter\n",
        ),
    ];
    for (label, body, want) in cases {
        let src = format!("{PRELUDE}fn main() {{\n    {body}\n    println(\"after\");\n}}\n");
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

/// B-2026-08-27-5 — the compiled twin of
/// `eq_on_a_shared_struct_reads_a_niche_option_field_as_a_niche`.
///
/// `==` on a shared struct with a niche-encoded `Option[shared T]` field
/// answered `false` for structurally equal values. The slot is ONE pointer
/// (null = None); the comparator `emit_eq_fn_for_type_expr` builds for
/// `Option[T]` is for the conventional `{tag, w0, w1, w2}`, FOUR words —
/// so its byte loop ran past the end of the field.
///
/// THE `padded` LINES ARE THE ONES THAT PIN THE MECHANISM. With trailing
/// fields the surplus 24 bytes land on p1/p2 and the compiled answer came
/// out RIGHT purely because those were equal; only with the niche field
/// LAST does it read past the object and go wrong. A test written around
/// the last-field shape alone can be satisfied by a "fix" that merely
/// moves the over-read somewhere harmless.
///
/// `deep` exercises the recursion this cannot terminate by luck: a 5-link
/// chain compares link by link, and each level re-enters the same
/// comparator through the niche slot. Its operands are BOUND rather than
/// inline `chain(4) == chain(4)` — comparing two call results is a
/// separate, already-diagnosed codegen gap ("structural `==` on this
/// reference type is not yet supported under `karac build`"), and letting
/// it fire here would mean this test never reached the comparator.
#[test]
fn test_e2e_eq_on_a_shared_struct_niche_option_field() {
    assert_eq!(
        run_program(
            r#"
#[derive(Hash, Eq, PartialEq)]
shared struct Node { v: i64, next: Option[Node] }
#[derive(Hash, Eq, PartialEq)]
shared struct Padded { v: i64, next: Option[Padded], p1: i64, p2: i64 }
fn chain(n: i64) -> Node {
    if n == 0 { return Node { v: 0, next: None }; }
    return Node { v: n, next: Some(chain(n - 1)) };
}
fn main() {
    let a = Node { v: 1, next: None };
    let b = Node { v: 1, next: None };
    println(f"none-none={a == b}");

    let leaf = Node { v: 2, next: None };
    let c = Node { v: 1, next: Some(leaf) };
    let leaf2 = Node { v: 2, next: None };
    let d = Node { v: 1, next: Some(leaf2) };
    println(f"some-some={c == d}");
    println(f"some-none={c == a}");

    let x = chain(4);
    let y = chain(4);
    let z = chain(3);
    println(f"deep={x == y}");
    println(f"deep-ne={x == z}");

    let p = Padded { v: 1, next: None, p1: 7, p2: 8 };
    let q = Padded { v: 1, next: None, p1: 7, p2: 8 };
    let r = Padded { v: 1, next: None, p1: 7, p2: 9 };
    println(f"padded={p == q}");
    println(f"padded-ne={p == r}");
}
"#
        ),
        Some(
            "none-none=true\nsome-some=true\nsome-none=false\n\
                 deep=true\ndeep-ne=false\n\
                 padded=true\npadded-ne=false\n"
                .to_string()
        )
    );
}

/// B-2026-08-17-28 leg (3) — `?.` had no arm in `compile_expr`, so every
/// optional chain failed the build. It is lowered to the `match` design.md
/// line 782 describes it as, which is also the shape whose ownership
/// machinery already exists.
///
/// Pinned against `--interp`, which was fixed first: the point is that all
/// three backends now agree on the same program, including design.md's own
/// three-level example at every level of absence.
#[test]
fn optional_chain_compiles_and_short_circuits() {
    let decls = "struct City { name: String, zip: i64 }\n\
                     struct Address { city: Option[City], zip: i64 }\n\
                     struct User { address: Option[Address] }\n";
    let full = "Some(Address { city: Some(City { name: \"Paris\", zip: 7 }), zip: 3 })";
    for (addr, body, want) in [
            // Single level, struct member — the shape that ICE'd the
            // interpreter before this row's first commit.
            (
                full,
                "match u.address?.city { Some(c) => { println(c.name); } None => { println(\"none\"); } }",
                "Paris\n",
            ),
            // design.md's own chain, all present.
            (
                full,
                "match u.address?.city?.name { Some(n) => { println(n); } None => { println(\"none\"); } }",
                "Paris\n",
            ),
            // …absent at the inner level, and at the outer: both short-circuit.
            (
                "Some(Address { city: None, zip: 3 })",
                "match u.address?.city?.name { Some(n) => { println(n); } None => { println(\"none\"); } }",
                "none\n",
            ),
            (
                "None",
                "match u.address?.city?.name { Some(n) => { println(n); } None => { println(\"none\"); } }",
                "none\n",
            ),
            // A NON-Option member wraps rather than flattening — the other
            // half of the arm choice.
            (
                "Some(Address { city: None, zip: 9 })",
                "match u.address?.zip { Some(z) => { println(z); } None => { println(\"none\"); } }",
                "9\n",
            ),
        ] {
            let src = format!(
                "{decls}fn main() {{ let u = User {{ address: {addr} }};\n{body} }}\n"
            );
            let Some(got) = run_program(&src) else {
                return;
            };
            assert_eq!(got, want, "optional chain: {body}");
        }
}

/// The METHOD form, which the row did not name and which used to discard
/// its arguments entirely.
#[test]
fn optional_chain_method_form_compiles() {
    let src = "struct City { name: String, zip: i64 }\n\
                   impl City { fn label(ref self) -> String { return self.name; } }\n\
                   fn mk(present: bool) -> Option[City] {\n\
                       if present { return Some(City { name: \"Paris\", zip: 7 }); }\n\
                       return None;\n\
                   }\n";
    for (arg, want) in [("true", "Paris\n"), ("false", "none\n")] {
        let Some(got) = run_program(&format!(
                "{src}fn main() {{ match mk({arg})?.label() {{ Some(s) => {{ println(s); }} None => {{ println(\"none\"); }} }} }}\n"
            )) else {
                return;
            };
        assert_eq!(got, want, "`mk({arg})?.label()`");
    }
}

/// The `Option` head of the same fix, end to end. `impl Trait for
/// Option[i64]` typed `self` as an args-less `Option`, so the body's
/// `self.unwrap_or(0)` failed to resolve at all ("no method 'unwrap_or'
/// on type 'Option'") — this is the head that goes from REJECTED to
/// correct on all three backends with no codegen change of its own.
#[test]
fn an_option_impl_head_can_unwrap_its_own_payload() {
    let src = "trait Or { fn or_zero(self) -> i64; }\n\
                   impl Or for Option[i64] {\n\
                       fn or_zero(self) -> i64 { return self.unwrap_or(0); }\n\
                   }\n\
                   fn main() {\n\
                       let some: Option[i64] = Option.Some(9);\n\
                       let none: Option[i64] = None;\n\
                       println(some.or_zero().to_string());\n\
                       println(none.or_zero().to_string());\n\
                   }\n";
    let Some(out) = run_program(src) else {
        return;
    };
    assert_eq!(out, "9\n0\n", "the Option head must keep its payload type");
}

/// B-2026-08-21-26 — auto-generated `TryFrom[intN]`, end to end.
///
/// The inbound twin of `.discriminant()` above, reading the SAME folded
/// table backwards, so the declared values are deliberately non-positional
/// again: a lowering that compared against layout TAGS would answer `Ok`
/// for 0/1/2 and `Err` for 0x08.
///
/// Two shapes are deliberately avoided, and neither is a `try_from` gap:
///
///   * the converted variant is read back through `.discriminant()` rather
///     than bound and re-matched, because matching a C-like enum bound out
///     of `Ok(...)` MISCOMPILES today (B-2026-08-21-50) — `Ok(UsbClass.Hid)`
///     selects the first variant under codegen and the right one under the
///     interpreter, with no `try_from` anywhere;
///   * the `Err` payload was printed rather than cast while
///     B-2026-08-21-51 was open (the `value` binding came back as the
///     uninstantiated `D`). That is FIXED, so the arm now casts, which is
///     the stronger assertion: it proves the payload arrives at the enum's
///     repr width and not merely that some number was stored.
///
/// Reading the discriminant back is exact — it separates all three
/// variants — so nothing here is weaker for the detour.
#[test]
fn test_e2e_enum_try_from_maps_declared_values_back_to_variants() {
    let src = r#"
const BASE: i64 = 16;

#[repr(u8)]
enum UsbClass { Audio = 0x01, Hid = 0x03, MassStorage = 0x08 }

#[repr(i8)]
enum Neg { Down = -128, Up = 127 }

#[repr(u16)]
enum Wide { A = 1000, B = 65535 }

#[repr(u8)]
enum Op { Add = BASE + 1, Sub = BASE + 2 }

fn probe(raw: u8) {
    match UsbClass.try_from(raw) {
        Ok(c) => println(f"ok{c.discriminant()}"),
        Err(e) => match e {
            DiscriminantError.OutOfRange { value } => println(f"no{0 - (value as i64)}"),
        },
    }
}

fn main() {
    // Every declared value round-trips; every gap reports the value it saw.
    probe(0u8);
    probe(1u8);
    probe(2u8);
    probe(3u8);
    probe(8u8);
    probe(255u8);

    // Signed repr at both boundaries, including a NEGATIVE declared value.
    let lo: i8 = (0 - 128) as i8;
    match Neg.try_from(lo) { Ok(n) => println(n.discriminant()), Err(_) => println(999) }
    let hi: i8 = 127 as i8;
    match Neg.try_from(hi) { Ok(n) => println(n.discriminant()), Err(_) => println(999) }
    let mid: i8 = 0 as i8;
    match Neg.try_from(mid) { Ok(n) => println(n.discriminant()), Err(_) => println(999) }

    // Wide repr at the top of its range, and a near miss.
    let top: u16 = 65535 as u16;
    match Wide.try_from(top) { Ok(w) => println(w.discriminant()), Err(_) => println(999) }
    let near: u16 = 1001 as u16;
    match Wide.try_from(near) { Ok(w) => println(w.discriminant()), Err(_) => println(999) }

    // A folded constant expression — the same fold both backends read.
    match Op.try_from(17u8) { Ok(o) => println(o.discriminant()), Err(_) => println(999) }

    // Round-trip against the outbound direction.
    match UsbClass.try_from(UsbClass.MassStorage.discriminant()) {
        Ok(c) => println(c.discriminant()),
        Err(_) => println(999),
    }
}
"#;
    assert_eq!(
        run_program(src).as_deref(),
        Some("no0\nok1\nno-2\nok3\nok8\nno-255\n-128\n127\n999\n65535\n999\n17\n8\n")
    );
}

/// B-2026-08-21-50 — a C-like enum bound out of an `Ok(...)` / `Some(...)`
/// / `Err(...)` payload.
///
/// `bind_pattern_values` re-wraps a single-i64-field payload word back into
/// its aggregate, but consulted only `struct_types`. A C-like enum's layout
/// is `{ i64 }` — structurally the same shape — so nothing re-wrapped it:
/// the binding's slot became a bare `i64`, the inner match compared that raw
/// word against variant tags and selected the FIRST variant, and a call
/// boundary rejected `i64` against the `{ i64 }` parameter outright. The
/// interpreter was right throughout, so the match half was a SILENT
/// run-vs-build divergence on a program with no unsafe, FFI or concurrency.
///
/// THE FIRST-VARIANT CASE IS THE TRAP THIS TEST IS BUILT AROUND. Pre-fix
/// every shape below printed the first variant, so `Ok(UsbClass.Audio)` —
/// whose correct answer IS the first variant — passed against the broken
/// compiler. A pin that happened to pick the first variant would have been
/// green on the bug. `ok_audio` is still here, but as a control that the fix
/// did not invert the selection rather than as evidence it works; the
/// load-bearing cases are `ok_hid` and `ok_mass`.
///
/// `name_of(c)` covers the second, louder symptom — the call boundary that
/// failed module verification — and `tag(c)` reads the discriminant through
/// a different arm shape, so a fix that satisfied the verifier while still
/// binding the wrong word would fail here.
///
/// The last two cases are the paths this must NOT disturb, and both were
/// already correct pre-fix: a DATA-CARRYING enum (layout `{ tag, words… }`,
/// excluded by the same `count_fields() == 1` gate the struct path uses,
/// because one word cannot rebuild it) and the single-field struct payload
/// the block was originally written for.
#[test]
fn test_e2e_c_like_enum_bound_out_of_a_result_payload() {
    let src = r#"
#[repr(u8)]
enum UsbClass { Audio = 0x01, Hid = 0x03, MassStorage = 0x08 }
enum Plain { P, Q, R }
enum Data { N(i64), S(i64) }
struct W { v: i64 }

fn name_of(c: UsbClass) -> String {
    match c {
        UsbClass.Audio => return "audio",
        UsbClass.Hid => return "hid",
        UsbClass.MassStorage => return "mass",
    }
}
fn ok_hid() -> Result[UsbClass, String] { Ok(UsbClass.Hid) }
fn ok_mass() -> Result[UsbClass, String] { Ok(UsbClass.MassStorage) }
fn ok_audio() -> Result[UsbClass, String] { Ok(UsbClass.Audio) }
fn some_r() -> Option[Plain] { Some(Plain.R) }
fn err_q() -> Result[i64, Plain] { Err(Plain.Q) }
fn ok_data() -> Result[Data, String] { Ok(Data.S(7)) }
fn ok_struct() -> Result[W, String] { Ok(W { v: 9 }) }

fn tag(c: UsbClass) -> i64 {
    match c {
        UsbClass.Audio => 1,
        UsbClass.Hid => 3,
        UsbClass.MassStorage => 8,
    }
}

fn main() {
    match ok_hid() { Ok(c) => println(name_of(c)), Err(_) => println("err") }
    match ok_mass() { Ok(c) => println(name_of(c)), Err(_) => println("err") }
    match ok_audio() { Ok(c) => println(name_of(c)), Err(_) => println("err") }
    match ok_hid() { Ok(c) => println(tag(c)), Err(_) => println(-1) }
    match some_r() {
        Some(p) => match p { Plain.P => println("p"), Plain.Q => println("q"), Plain.R => println("r") },
        None => println("none"),
    }
    match err_q() {
        Ok(_) => println("ok"),
        Err(e) => match e { Plain.P => println("ep"), Plain.Q => println("eq"), Plain.R => println("er") },
    }
    if let Ok(c) = ok_mass() { println(name_of(c)); }
    match ok_data() { Ok(d) => match d { Data.N(v) => println(v), Data.S(v) => println(v * 2) }, Err(_) => println("e") }
    match ok_struct() { Ok(w) => println(w.v), Err(_) => println("e") }
}
"#;
    assert_eq!(
        run_program(src).as_deref(),
        Some("hid\nmass\naudio\n3\nr\neq\nmass\n14\n9\n")
    );
}

/// B-2026-09-10-24 + B-2026-09-10-28 — an `Option`/`Result` BINDING moved
/// into a tuple literal. Two rows, one cause, and they sit in one fixture
/// because separating them hides what makes the fix ordered.
///
/// `compile_tuple` disarms a moved-in element's Vec/String cap, its Map
/// handle and its f-string accumulator, but never its ENUM PAYLOAD — that
/// lives behind `FreeInlineOptionPayload` / `FreeInlineResultPayload` /
/// `BoxedEnumDrop`, three guards `suppress_source_vec_cleanup_for_arg`
/// cannot reach. So the source stayed armed over bits the tuple had taken,
/// and WHAT THAT COST DEPENDED ENTIRELY ON WHETHER THE ELEMENT'S TYPE WAS
/// KNOWN:
///
///   * KNOWN (an annotated binding, -28): the tuple armed its own walks,
///     the payload had two owners, and the 32-byte box was freed twice and
///     read after — SIGSEGV at `-O0`, `free(): double free detected in
///     tcache 2` at `-O2`, against the interpreter's correct body.
///   * UNKNOWN (an unannotated binding, -24): the tuple armed nothing, the
///     payload had exactly one owner, and the only loss was the body —
///     `dR71` interpreted, nothing compiled.
///
/// THE ORDER IS THE POINT. -24's own row proposed naming the element from
/// `optres_var_payload_tes` and MEASURED that three-line version aborting;
/// it concluded the lookup was wrong. The lookup is right and the disarm
/// was missing — typing the element merely converts the silent cell into
/// the crashing one. So the disarm lands first and the lookup rides on it;
/// a future edit that removes the disarm turns the `annotated-*` cells
/// below into aborts rather than into failures.
///
/// The DISCARDED-LOCAL cells print the body BEFORE `ok`: `p` is never read,
/// so it dies at its own `let` under this tree's NLL model. That is the
/// model, not an artifact of the move — the `read` cells show the same
/// single body landing later instead.
#[test]
fn e2e_optres_local_moved_into_a_tuple_literal_runs_its_drop_body() {
    const PRE: &str = "struct Rt { id: i64, name: String }\n\
             impl Drop for Rt { fn drop(mut ref self) { println(f\"dRt{self.id}\") } }\n";
    for (label, body, want) in [
            // B-2026-09-10-24, THE ROW: an `Option` local, unannotated tuple.
            (
                "option-local",
                "fn main() { let o: Option[Rt] = Some(Rt { id: 71, name: f\"a\" }); let p = (o, 7); println(\"ok\"); println(\"done\") }\n",
                "dRt71\nok\ndone\n",
            ),
            // The `Result` twin the row measured behaving identically.
            (
                "result-local",
                "fn main() { let r: Result[Rt, i64] = Result[Rt, i64].Ok(Rt { id: 71, name: f\"a\" }); let p = (r, 7); println(\"ok\"); println(\"done\") }\n",
                "dRt71\nok\ndone\n",
            ),
            // B-2026-09-10-28: the ANNOTATED spelling — the crashing one. The
            // annotation supplies the element types with no help from
            // `refined_tuple_literal_elem_te`, so this cell is a pure test of
            // the DISARM. Before it: SIGSEGV at -O0, double-free abort at -O2.
            (
                "annotated-option-local",
                "fn main() { let o: Option[Rt] = Some(Rt { id: 71, name: f\"a\" }); let p: (Option[Rt], i64) = (o, 7); println(\"ok\"); println(\"done\") }\n",
                "dRt71\nok\ndone\n",
            ),
            (
                "annotated-result-local",
                "fn main() { let r: Result[Rt, i64] = Result[Rt, i64].Ok(Rt { id: 71, name: f\"a\" }); let p: (Result[Rt, i64], i64) = (r, 7); println(\"ok\"); println(\"done\") }\n",
                "dRt71\nok\ndone\n",
            ),
            // The binding READ, so it lives to the statement's end: the body
            // moves after `n7`, and must still be exactly one.
            (
                "option-local-read",
                "fn main() { let o: Option[Rt] = Some(Rt { id: 71, name: f\"a\" }); let p = (o, 7); println(f\"n{p.1}\"); println(\"done\") }\n",
                "n7\ndRt71\ndone\n",
            ),
            // The SOURCE read before the move — one of the row's three
            // NOT MEASURED items. A read does not change who owns the payload.
            (
                "source-read-before-move",
                "fn main() { let o: Option[Rt] = Some(Rt { id: 71, name: f\"a\" }); println(f\"s{o.is_some()}\"); let p = (o, 7); println(\"ok\"); println(\"done\") }\n",
                "strue\ndRt71\nok\ndone\n",
            ),
            // TWO such locals in one literal — the row's second NOT MEASURED
            // item. Both bodies, in element order.
            (
                "two-locals-one-literal",
                "fn main() { let a: Option[Rt] = Some(Rt { id: 71, name: f\"a\" }); let b: Option[Rt] = Some(Rt { id: 72, name: f\"b\" }); let p = (a, b); println(\"ok\"); println(\"done\") }\n",
                "dRt71\ndRt72\nok\ndone\n",
            ),
            // The local in the SECOND slot, so a fix that only inspected
            // element 0 shows up here.
            (
                "local-not-first",
                "fn main() { let o: Option[Rt] = Some(Rt { id: 71, name: f\"a\" }); let p = (7, o); println(\"ok\"); println(\"done\") }\n",
                "dRt71\nok\ndone\n",
            ),
            // A NESTED tuple literal over the same local: the disarm runs per
            // element at every level, and the lookup recurses with it.
            (
                "nested-tuple-literal",
                "fn main() { let o: Option[Rt] = Some(Rt { id: 71, name: f\"a\" }); let p = ((o, 6), 7); println(\"ok\"); println(\"done\") }\n",
                "dRt71\nok\ndone\n",
            ),
            // A DESTRUCTURE of the same literal — the other consumer of the
            // arm this fix teaches, reached through `finish_owned_tuple_*`
            // rather than the binding path.
            (
                "destructured-literal",
                "fn main() { let o: Option[Rt] = Some(Rt { id: 71, name: f\"a\" }); let (x, y) = (o, 7); println(f\"n{y}\"); println(\"done\") }\n",
                // `x` binds the payload and is never read, so it dies at its
                // own `let` — the body lands BEFORE `n7`, and both backends
                // agree on that. Contrast `option-local-read` above, where the
                // tuple itself is read and the single body lands after.
                "dRt71\nn7\ndone\n",
            ),
            // CONTROL — a `None` local. The element types now resolve, so the
            // walk is armed; it must find no payload and print nothing.
            (
                "none-local-control",
                "fn main() { let o: Option[Rt] = None; let p = (o, 7); println(\"ok\"); println(\"done\") }\n",
                "ok\ndone\n",
            ),
            // CONTROL — a payload with NO user `Drop`. The lookup names this
            // element too; nothing may run for it, and its buffer must still
            // be freed exactly once (the asan fixture asserts that half).
            (
                "no-drop-payload-control",
                "fn main() { let o: Option[String] = Some(f\"abc\"); let p = (o, 7); println(f\"ok{p.1}\"); println(\"done\") }\n",
                "ok7\ndone\n",
            ),
            // CONTROL — the BARE REBIND (`let o2 = o;`), the move that already
            // disarmed correctly and whose `optres_var_payload_tes` read this
            // fix reuses. Unchanged by either half.
            (
                "bare-rebind-control",
                "fn main() { let o: Option[Rt] = Some(Rt { id: 71, name: f\"a\" }); let o2 = o; println(\"ok\"); println(\"done\") }\n",
                "dRt71\nok\ndone\n",
            ),
            // CONTROL — the same local handed to a CALL, the other move site
            // that has disarmed this trio since slice 3q.
            (
                "call-arg-control",
                "fn eat(o: Option[Rt]) { println(\"in\") }\n\
                 fn main() { let o: Option[Rt] = Some(Rt { id: 71, name: f\"a\" }); eat(o); println(\"done\") }\n",
                "in\ndRt71\ndone\n",
            ),
            // ---- THE LITERAL SIBLINGS (B-2026-09-10-29) ----
            // A `Vec[..]` PREFIX literal over the same local. On `main` this
            // SIGSEGVed at `-O0` with 4 valgrind errors.
            (
                "vec-prefix-local",
                "fn main() { let o: Option[Rt] = Some(Rt { id: 71, name: f\"a\" }); let v = Vec[o]; println(\"ok\"); println(\"done\") }\n",
                "dRt71\nok\ndone\n",
            ),
            // Its ANNOTATED form. Note this is the OPPOSITE of the `Array`
            // control below: an annotated `Vec` destination DOES take the
            // payload, an annotated `Array` does not, and only measurement
            // separates them.
            (
                "vec-prefix-annotated",
                "fn main() { let o: Option[Rt] = Some(Rt { id: 71, name: f\"a\" }); let v: Vec[Option[Rt]] = Vec[o]; println(\"ok\"); println(\"done\") }\n",
                "dRt71\nok\ndone\n",
            ),
            // A bare `[..]` ARRAY literal — a different builder again
            // (`compile_array_literal`). SIGSEGVed on `main`; fixed by the
            // element-type lookup alone, with NO disarm added to that builder.
            (
                "array-literal-local",
                "fn main() { let o: Option[Rt] = Some(Rt { id: 71, name: f\"a\" }); let v = [o]; println(\"ok\"); println(\"done\") }\n",
                "dRt71\nok\ndone\n",
            ),
            (
                "array-literal-two-locals",
                "fn main() { let a: Option[Rt] = Some(Rt { id: 71, name: f\"a\" }); let b: Option[Rt] = Some(Rt { id: 72, name: f\"b\" }); let v = [a, b]; println(\"ok\"); println(\"done\") }\n",
                "dRt71\ndRt72\nok\ndone\n",
            ),
            // The NO-USER-`Drop` twin of the same array cell. It printed
            // nothing to lose and still aborted with `free(): double free
            // detected in tcache 2` on `main` — this class is reachable in a
            // program containing no `Drop` impl at all, which is why it is not
            // merely a missing-body row.
            (
                "array-literal-no-drop-payload",
                "fn main() { let o: Option[String] = Some(f\"abc\"); let v = [o]; println(\"ok\"); println(\"done\") }\n",
                "ok\ndone\n",
            ),
            // CONTROL — an annotated fixed `Array` destination. This one is
            // CLEAN on `main` and must STAY clean: its element drop does not
            // take the payload, so the source binding is the only owner and
            // disarming it here leaks (measured: 32 B). It is why
            // `compile_array_literal` deliberately carries no disarm.
            (
                "array-annotated-control",
                "fn main() { let o: Option[Rt] = Some(Rt { id: 71, name: f\"a\" }); let v: Array[Option[Rt], 1] = [o]; println(\"ok\"); println(\"done\") }\n",
                "dRt71\nok\ndone\n",
            ),
            // CONTROL — an INLINE heap payload in a TUPLE. The tuple's
            // synthesized drop does not free one, so the source keeps it and
            // nothing may be disarmed: with the inline suppressors added here
            // this leaked 17 B (`String`) and 24 B (`Vec[i64]`). Silent by
            // design — the payload has no user `Drop` — so the fixture asserts
            // the OUTPUT and the asan leg asserts the balance.
            (
                "tuple-inline-string-control",
                "fn main() { let o: Option[String] = Some(f\"hello-long-string\"); let p = (o, 7); println(f\"ok{p.1}\"); println(\"done\") }\n",
                "ok7\ndone\n",
            ),
            (
                "tuple-inline-vec-control",
                "fn main() { let o: Option[Vec[i64]] = Some([1, 2, 3]); let p = (o, 7); println(f\"ok{p.1}\"); println(\"done\") }\n",
                "ok7\ndone\n",
            ),
        ] {
            let Some(out) = run_program(&format!("{PRE}{body}")) else {
                return;
            };
            assert_eq!(out, want, "[{label}]");
        }
}

/// B-2026-09-10-15 — an `Option`/`Result` payload that is ITSELF an
/// `Option`/`Result` ran its inner `Drop` body on the interpreter and
/// nowhere on the compiled backends.
///
/// `emit_optres_payload_user_drop_bodies_fn`'s shared core had arms for a
/// payload that is a user struct, a user enum or a tuple, and its filter
/// excluded `Option`/`Result` outright with the note that "a nested
/// built-in payload rides its own walker, not this arm". There is no such
/// walker: a walker for the OUTER `Option[Option[R]]` is precisely what
/// that fn emits, and it declined the shape, so nothing anywhere ran the
/// inner `R`'s body. Memory was balanced throughout — the box and its
/// interior always had owners — so the missing output was the whole
/// observable.
///
/// The fix recurses: an envelope payload's arm calls the same emitter for
/// the inner envelope. THE ADMISSION TEST IS THAT CALL, not a predicate
/// beside it, and the three-deep cell is why. `elem_te_runs_user_drop`
/// reads a nested envelope's payload HEAD NAME only, so
/// `Option[Option[R]]` asks `type_runs_user_drop("Option")` and gets
/// `false`; gated on it, the two-deep cells here passed and
/// `Option[Option[Option[R]]]` stayed silent. The emitter has no horizon.
///
/// The three DISCARDED-LOCAL cells print the body BEFORE `ok`, which is
/// this tree's NLL model for a binding that is never read (`exec.rs`'s
/// `note_unread`), not an ordering quirk of the nesting.
#[test]
fn e2e_nested_optres_payload_runs_its_inner_drop_body() {
    const PRE: &str = "struct Rn { id: i64, name: String }\n\
             impl Drop for Rn { fn drop(mut ref self) { println(f\"dRn{self.id}\") } }\n";
    for (label, body, want) in [
            // THE ROW: a fresh-temp argument to a by-value param.
            (
                "fresh-temp-param",
                "fn takeR(x: Option[Option[Rn]]) { match x { Some(t) => { println(\"ok\") } None => { println(\"n\") } } }\n\
                 fn main() { takeR(Some(Some(Rn { id: 71, name: f\"a\" }))); println(\"done\") }\n",
                "ok\ndRn71\ndone\n",
            ),
            // A NAMED local handed to the same param. The caller's let-site
            // registration is disarmed at the move, so the callee owns it.
            (
                "named-local-param",
                "fn takeR(x: Option[Option[Rn]]) { match x { Some(t) => { println(\"ok\") } None => { println(\"n\") } } }\n\
                 fn main() { let o = Some(Some(Rn { id: 71, name: f\"a\" })); takeR(o); println(\"done\") }\n",
                "ok\ndRn71\ndone\n",
            ),
            // A DISCARDED local: never read, so it dies at its own `let`.
            (
                "discarded-local",
                "fn main() { let o = Some(Some(Rn { id: 71, name: f\"a\" })); println(\"ok\"); println(\"done\") }\n",
                "dRn71\nok\ndone\n",
            ),
            // The same, with the value arriving from a call rather than a
            // literal — a different construction route to the same slot.
            (
                "returned-then-bound",
                "fn mk() -> Option[Option[Rn]] { Some(Some(Rn { id: 71, name: f\"a\" })) }\n\
                 fn main() { let o = mk(); println(\"ok\"); println(\"done\") }\n",
                "dRn71\nok\ndone\n",
            ),
            // A CONTAINER element rather than a binding: the per-element
            // walker reaches the same emitter.
            (
                "vec-element",
                "fn main() { let v: Vec[Option[Option[Rn]]] = [Some(Some(Rn { id: 71, name: f\"a\" }))]; println(\"ok\"); println(\"done\") }\n",
                "dRn71\nok\ndone\n",
            ),
            // The `Result` spelling on the `Ok` side, which the row left
            // unmeasured and which diverged identically.
            (
                "result-of-result",
                "fn takeR(x: Result[Result[Rn, i64], i64]) { match x { Ok(t) => { println(\"ok\") } Err(e) => { println(\"n\") } } }\n\
                 fn main() { takeR(Ok(Ok(Rn { id: 71, name: f\"a\" }))); println(\"done\") }\n",
                "ok\ndRn71\ndone\n",
            ),
            // THREE deep — the cell that fails on any one-level predicate and
            // the reason the admission test is the recursive emitter itself.
            (
                "three-deep",
                "fn takeR(x: Option[Option[Option[Rn]]]) { match x { Some(t) => { println(\"ok\") } None => { println(\"n\") } } }\n\
                 fn main() { takeR(Some(Some(Some(Rn { id: 71, name: f\"a\" })))); println(\"done\") }\n",
                "ok\ndRn71\ndone\n",
            ),
            // CONTROL — the OUTER envelope is `None`, so the tag switch must
            // fall through and the recursion never run.
            (
                "outer-none",
                "fn takeR(x: Option[Option[Rn]]) { match x { Some(t) => { println(\"ok\") } None => { println(\"n\") } } }\n\
                 fn main() { let e: Option[Option[Rn]] = None; takeR(e); println(\"done\") }\n",
                "n\ndone\n",
            ),
            // CONTROL — the INNER envelope is `None`: the outer arm is taken,
            // the recursion runs, and its own tag switch finds nothing. This
            // is the cell a recursion that walked a level too far would fail.
            (
                "inner-none",
                "fn takeR(x: Option[Option[Rn]]) { match x { Some(t) => { println(\"ok\") } None => { println(\"n\") } } }\n\
                 fn main() { let e: Option[Rn] = None; takeR(Some(e)); println(\"done\") }\n",
                "ok\ndone\n",
            ),
            // CONTROL — one level only. Correct before this commit; here so a
            // later change that double-fires the recursion prints `dRn71`
            // twice and is caught.
            (
                "single-level-control",
                "fn takeR(x: Option[Rn]) { match x { Some(t) => { println(\"ok\") } None => { println(\"n\") } } }\n\
                 fn main() { takeR(Some(Rn { id: 71, name: f\"a\" })); println(\"done\") }\n",
                "ok\ndRn71\ndone\n",
            ),
            // CONTROL — a TUPLE payload (B-2026-09-10-9's shape), the sibling
            // arm of the same filter, unchanged by this commit.
            (
                "tuple-payload-control",
                "fn takeR(x: Option[(Rn, Rn)]) { match x { Some(t) => { println(\"ok\") } None => { println(\"n\") } } }\n\
                 fn main() { takeR(Some((Rn { id: 71, name: f\"a\" }, Rn { id: 72, name: f\"b\" }))); println(\"done\") }\n",
                "ok\ndRn71\ndRn72\ndone\n",
            ),
        ] {
            let Some(out) = run_program(&format!("{PRE}{body}")) else {
                return;
            };
            assert_eq!(out, want, "[{label}]");
        }
}

/// B-2026-09-12-15 — a by-value `Option`/`Result` argument's payload `Drop`
/// body ran on NO compiled backend at three of the FOUR argument positions,
/// and ran TWICE at the fourth when the callee consumed the payload.
///
/// FOUR ARGUMENT LOOPS, ONE WIRED. The memory halves of this question were
/// wired at all four when B-2026-08-12-15 split it (payload buffer vs field
/// envelope); B-2026-09-09-18 then added the BODIES half at the
/// free-function loop only. So `s.take(Some(R { .. }))` (method),
/// `Sink.eat(Some(R { .. }))` (associated fn) and `genf(Some(R { .. }))`
/// (monomorphized generic) each printed one body on `--interp` and none
/// compiled. The generic path could not reuse the other three's name-keyed
/// gate: `compile_generic_call`'s own doc says it "runs neither half" of
/// `compile_call`, so it asks the two questions of the `generic_fn` AST it
/// already holds.
///
/// THE CONSUMING GATE IS WHAT MAKES THE TRANSPLANT SAFE, and it was missing
/// from the site that already had the registration. A param can be
/// NON-ESCAPING while its PAYLOAD escapes: `acc.push(r)` hands `r` to
/// something outliving the call, `return r` hands it to the caller's own
/// binding. Both make the receiver run the body, so owning it here as well
/// printed TWO bodies for one value — cells 7 and 8, measured that way at
/// the free-function position before this change. Copying the old
/// registration to three more loops without this gate would have spread that
/// double run rather than fixed the silence; cell 9 is the shape that would
/// have regressed (correct today only BECAUSE its position registered
/// nothing).
///
/// The gate is per-VARIANT where the argument is a constructor and so says
/// which variant it builds — a callee may take `Ok`'s payload and only read
/// `Err`'s — and falls back to declining whenever any variant is consumed,
/// which is the status quo rather than the double run.
///
/// MEMORY WAS CLEAN THROUGHOUT, before and after, on every cell: 0 valgrind
/// errors with all heap blocks freed. A body frees nothing, so neither the
/// silence nor the double run was ever visible to a sanitizer — the same
/// reason B-2026-09-09-18's own hole and B-2026-09-12-11 went unnoticed.
#[test]
fn e2e_optres_arg_payload_body_runs_once_at_every_call_position() {
    const PRE: &str = "struct Rp { id: i64 }\n\
             impl Drop for Rp { fn drop(mut ref self) { println(f\"dRp{self.id}\") } }\n";
    for (label, body, want) in [
            // 1 — METHOD position, the row's first cell.
            (
                "method-arg",
                "struct Sk { n: i64 }\n\
                 impl Sk { fn take(mut ref self, x: Option[Rp]) { match x { Some(r) => { println(f\"m:{r.id}\") } None => { println(\"n\") } } } }\n\
                 fn main() { let mut s = Sk { n: 0 }; s.take(Some(Rp { id: 1 })); println(\"end\") }\n",
                "m:1\ndRp1\nend\n",
            ),
            // 2 — the same with a `ref self` receiver, so the fix does not
            //     depend on the receiver's mode.
            (
                "method-arg-ref-self",
                "struct Sk { n: i64 }\n\
                 impl Sk { fn look(ref self, x: Option[Rp]) { match x { Some(r) => { println(f\"l:{r.id}\") } None => { println(\"n\") } } } }\n\
                 fn main() { let s = Sk { n: 0 }; s.look(Some(Rp { id: 1 })); println(\"end\") }\n",
                "l:1\ndRp1\nend\n",
            ),
            // 3 — ASSOCIATED-FUNCTION position. This cell needed the
            //     INTERPRETER half as well: it printed `dRp1` twice there,
            //     because `owned_param_names_of_fn` scanned `Item::Function`
            //     only and an assoc fn lives in an `ImplBlock`, so the
            //     arm-bound payload was never marked a view of the entry copy.
            (
                "assoc-fn-arg",
                "struct Sk { n: i64 }\n\
                 impl Sk { fn eat(x: Option[Rp]) { match x { Some(r) => { println(f\"a:{r.id}\") } None => { println(\"n\") } } } }\n\
                 fn main() { Sk.eat(Some(Rp { id: 1 })); println(\"end\") }\n",
                "a:1\ndRp1\nend\n",
            ),
            // 4 — MONOMORPHIZED GENERIC position.
            (
                "generic-fn-arg",
                "fn genf[T](x: Option[T]) { match x { Some(v) => { println(\"g\") } None => { println(\"n\") } } }\n\
                 fn main() { genf(Some(Rp { id: 1 })); println(\"end\") }\n",
                "g\ndRp1\nend\n",
            ),
            // 5 — the `Err` side at the associated position, so the tag that is
            //     0 is covered too.
            (
                "assoc-fn-result-err",
                "struct Sk { n: i64 }\n\
                 impl Sk { fn eat(x: Result[i64, Rp]) { match x { Ok(n) => { println(\"ok\") } Err(r) => { println(f\"e:{r.id}\") } } } }\n\
                 fn main() { Sk.eat(Err(Rp { id: 1 })); println(\"end\") }\n",
                "e:1\ndRp1\nend\n",
            ),
            // 6 — the QUALIFIED spelling at the method position. B-2026-09-12-11
            //     made the two spellings agree; this holds them together now
            //     that the position itself is fixed.
            (
                "method-arg-qualified-spelling",
                "struct Sk { n: i64 }\n\
                 impl Sk { fn take(mut ref self, x: Option[Rp]) { match x { Some(r) => { println(f\"m:{r.id}\") } None => { println(\"n\") } } } }\n\
                 fn main() { let mut s = Sk { n: 0 }; s.take(Option[Rp].Some(Rp { id: 1 })); println(\"end\") }\n",
                "m:1\ndRp1\nend\n",
            ),
            // 7 — THE DOUBLE RUN, free-function position: the callee pushes the
            //     payload into an accumulator that outlives the call. Printed
            //     `dRp1 / len:1 / dRp1` compiled before the consuming gate.
            (
                "consuming-callee-pushes-payload",
                "fn eat(x: Option[Rp], acc: mut ref Vec[Rp]) { match x { Some(r) => { acc.push(r) } None => { println(\"n\") } } }\n\
                 fn main() { let mut acc: Vec[Rp] = []; eat(Some(Rp { id: 1 }), mut acc); println(f\"len:{acc.len()}\"); println(\"end\") }\n",
                "len:1\ndRp1\nend\n",
            ),
            // 8 — the other consuming shape: the callee RETURNS the payload, so
            //     the caller's own destination binding owns it. Printed
            //     `k:1 / dRp1 / end / dRp1` before.
            (
                "consuming-callee-returns-payload",
                "fn give(x: Option[Rp]) -> Rp { match x { Some(r) => { return r } None => { return Rp { id: 0 } } } }\n\
                 fn main() { let k = give(Some(Rp { id: 1 })); println(f\"k:{k.id}\"); println(\"end\") }\n",
                "k:1\ndRp1\nend\n",
            ),
            // 9 — CONTROL, and the cell that says why the gate had to come
            //     first: a consuming callee at the METHOD position, correct
            //     before this change only because that position registered
            //     nothing. It must still print one body now that it does.
            (
                "consuming-callee-at-method-position",
                "struct Sk { n: i64 }\n\
                 impl Sk { fn take(mut ref self, x: Option[Rp], acc: mut ref Vec[Rp]) { match x { Some(r) => { acc.push(r) } None => { println(\"n\") } } } }\n\
                 fn main() { let mut s = Sk { n: 0 }; let mut acc: Vec[Rp] = []; s.take(Some(Rp { id: 1 }), mut acc); println(f\"len:{acc.len()}\"); println(\"end\") }\n",
                "len:1\ndRp1\nend\n",
            ),
            // 10 — CONTROL: a `let` destructure of a non-`Option` by-value
            //      param inside an ASSOCIATED function. The interpreter half
            //      repairs this too — it printed two bodies there — and it is
            //      the shape `owned_param_names_stack` exists for
            //      (B-2026-08-01-12), so it pins the widening to what it was
            //      meant to cover.
            (
                "assoc-fn-let-destructure-of-owned-param",
                "struct Wp { r: Rp }\n\
                 struct Sk { n: i64 }\n\
                 impl Sk { fn eat(w: Wp) { let m = w.r; println(f\"m:{m.id}\") } }\n\
                 fn main() { Sk.eat(Wp { r: Rp { id: 1 } }); println(\"end\") }\n",
                "m:1\ndRp1\nend\n",
            ),
            // 11 — CONTROL: the FREE-function twin of cell 10, which was always
            //      correct. Pinned so the widening cannot be read as having
            //      moved it.
            (
                "free-fn-let-destructure-of-owned-param-control",
                "struct Wp { r: Rp }\n\
                 fn eat(w: Wp) { let m = w.r; println(f\"m:{m.id}\") }\n\
                 fn main() { eat(Wp { r: Rp { id: 1 } }); println(\"end\") }\n",
                "m:1\ndRp1\nend\n",
            ),
            // 12 — CONTROL: a plain non-`Option` owned param at the associated
            //      position, which has no payload channel at all.
            (
                "assoc-fn-plain-owned-param-control",
                "struct Sk { n: i64 }\n\
                 impl Sk { fn eat(r: Rp) { println(f\"a:{r.id}\") } }\n\
                 fn main() { Sk.eat(Rp { id: 1 }); println(\"end\") }\n",
                "a:1\ndRp1\nend\n",
            ),
            // 14 — A NESTED DESTRUCTURE whose leaf is only READ, at the
            //      associated position. This is the shape that caught the first
            //      version of this fix: it gated on
            //      `optres_payload_consuming_param_variants`, which reports a
            //      nested pattern as TAKING the payload unconditionally — right
            //      for the memory question it was written for, wrong here, since
            //      `r.s` is merely read and the payload ENUM's own body is still
            //      the caller's to run. That version silenced three `dK` lines
            //      pinned by `e2e_boxed_enum_payload_param_output_is_unchanged`,
            //      and `optres_payload_escaping_param_variants` exists because of
            //      it. Divergent before the fix (`a:z` compiled against
            //      `a:z / dK` interpreted).
            (
                "nested-destructure-read-only-leaf",
                "struct R2q { s: String }\n                 enum Kq { A(R2q), B }\n                 impl Drop for Kq { fn drop(mut ref self) { println(\"dKq\") } }\n                 struct Sk2 { n: i64 }\n                 impl Sk2 { fn show(x: Option[Kq]) { match x { Option.Some(Kq.A(r)) => { println(f\"a:{r.s}\") } Option.Some(Kq.B) => {} Option.None => {} } } }\n                 fn main() { Sk2.show(Option.Some(Kq.A(R2q { s: f\"z\" }))); println(\"end\") }\n",
                "a:z\ndKq\nend\n",
            ),
            // 13 — CONTROL: the free-function position the row started from,
            //      correct before and after.
            (
                "free-fn-arg-control",
                "fn plainD(x: Option[Rp]) { match x { Some(r) => { println(f\"s:{r.id}\") } None => { println(\"n\") } } }\n\
                 fn main() { plainD(Some(Rp { id: 1 })); println(\"end\") }\n",
                "s:1\ndRp1\nend\n",
            ),
        ] {
            let Some(out) = run_program(&format!("{PRE}{body}")) else {
                return;
            };
            assert_eq!(out, want, "[{label}]");
        }
}

/// B-2026-09-13-3 — the same caller-side bodies channel, silenced one gate
/// further in: a fresh-temp `Option` argument whose ARM lets a
/// payload-DERIVED value flow out ran the payload's `Drop` body NOWHERE.
///
/// `binding_only_borrowed`'s syntactic walk calls every projection rooted
/// at the arm binding a partial move, so `Some(t) => { t.tag }` read as
/// "the payload escapes", `callee_by_value_optres_param_bodies_te`
/// declined, and nothing compensated: a fresh temp has no let site in the
/// callee to own it. Measured on all four surfaces before this change —
/// `eat(Some(Tracked { tag: 7 }))` printed `7 / 1000` against `--interp`'s
/// `99 / 7 / 1000`, and `return t.tag;` / `let z: i64 = t.tag; z` lost it
/// too while `t.tag + 0i64` and `println(t.tag); 7i64` kept it (an operator
/// or a call between the read and the result is enough to change the
/// syntactic answer, which is what made the trigger look arbitrary).
///
/// WHAT MAKES THE NARROW READING SOUND IS A TYPECHECKER RULE, not a codegen
/// convention. The projection-tolerant escape map is consulted only when
/// the payload type declares its own `impl Drop` — and for such a struct a
/// partial move is a hard error (`partial_move_of_drop_struct`, design.md
/// § Part 8, measured: `return t.inner;` off a `Drop`-bearing `Outer` does
/// not compile at all). So every projection that reaches codegen there is
/// provably a copy and carries no body away.
///
/// CELLS 5-8 ARE THE GUARDS, and they are why the rule is not wider. A
/// payload WITHOUT its own `Drop` may legally move a `Drop`-bearing field
/// out, and a TUPLE payload may move an element out; both hand the body to
/// the receiver and both are correct today. Reading those projections as
/// copies would register a second body in the caller — the double run
/// B-2026-09-12-15 measured as `dRp1 / len:1 / dRp1`.
///
/// MEMORY WAS CLEAN THROUGHOUT at `-O0`: 0 valgrind errors, 0 bytes in use
/// at exit, on every cell before and after. A body frees nothing, so no
/// sanitizer and neither ASAN ratchet leg could ever see this.
///
/// FOUR CELLS WERE RE-MEASURED FOR B-2026-09-14-19, which moved the body
/// from the caller's scope exit to the call's return. `tail-field-read`,
/// `return-field-read`, `let-then-tail` and `nested-drop-field` all called
/// `eat` INSIDE a `println`, so the payload dies in the callee before the
/// caller has a value to print and the body belongs first — which is what
/// `--interp` printed all along. This table asserts AOT only
/// (`run_program`), so the divergence was invisible in it and their
/// expectations had the compiled side's lateness baked in, exactly the
/// class B-2026-08-29-55's note records finding in seven other control
/// cases. The table was already internally inconsistent about it:
/// `guard-plain-struct-scalar-read` is the same shape and expected the
/// body FIRST. Each of the four was re-derived against the interpreter
/// rather than flipped to match the new build.
///
/// THE TWO `guard-*-moves-*` CELLS DID NOT MOVE, and that is the rule
/// rather than an exemption: there the body is handed to the RECEIVER, so
/// it is owed at the caller's binding and legitimately runs after the read.
/// (The interpreter used to DOUBLE on both — `dIn5 g:5 dIn5` — which was
/// the separate B-2026-09-13-5 defect and why these assert the compiled
/// order. That row is fixed: the interpreter now prints `g:5 dIn5` on both,
/// the same sequence asserted here, so the two backends agree on these two
/// cells and no longer only happen to.)
#[test]
fn e2e_optres_arg_payload_body_survives_a_projecting_arm() {
    const PRE: &str = "struct Tr { tag: i64 }\n\
             impl Drop for Tr { fn drop(mut ref self) { println(f\"dTr{self.tag}\") } }\n";
    for (label, body, want) in [
            // 1 — the row's headline cell: the arm's tail is a bare field read.
            (
                "tail-field-read",
                "fn eat(o: Option[Tr]) -> i64 { match o { Some(t) => { t.tag } None => { 0 } } }\n\
                 fn main() { println(f\"r:{eat(Some(Tr { tag: 7 }))}\"); println(\"end\") }\n",
                // B-2026-09-14-19 — was `r:7\ndTr7\nend\n`, which baked in the
                // compiled side's own lateness. The call sits INSIDE the
                // `println`, so the payload dies in `eat` before the caller has
                // a value to print: the body belongs first. `--interp` said
                // `dTr7 r:7 end` all along and this table asserts AOT only
                // (`run_program`), so the divergence was invisible here and got
                // recorded as intent — the eighth instance of exactly what
                // B-2026-08-29-55's own note describes finding in seven other
                // control cases. Both backends now print this.
                "dTr7\nr:7\nend\n",
            ),
            // 2 — the explicit `return` spelling of the same read.
            (
                "return-field-read",
                "fn eat(o: Option[Tr]) -> i64 { match o { Some(t) => { return t.tag; } None => { return 0; } } }\n\
                 fn main() { println(f\"r:{eat(Some(Tr { tag: 7 }))}\"); println(\"end\") }\n",
                // B-2026-09-14-19 — was the same string with the body LAST.
                // See the `tail-field-read` cell above: the call sits inside
                // the `println`, the payload dies in the callee, and
                // `--interp` printed the body first all along. Verified
                // against the interpreter cell by cell rather than flipped
                // to match the new build.
                "dTr7\nr:7\nend\n",
            ),
            // 3 — bound to a local first, which the walk also read as a move.
            (
                "let-then-tail",
                "fn eat(o: Option[Tr]) -> i64 { match o { Some(t) => { let z: i64 = t.tag; z } None => { 0 } } }\n\
                 fn main() { println(f\"r:{eat(Some(Tr { tag: 7 }))}\"); println(\"end\") }\n",
                // B-2026-09-14-19 — was the same string with the body LAST.
                // See the `tail-field-read` cell above: the call sits inside
                // the `println`, the payload dies in the callee, and
                // `--interp` printed the body first all along. Verified
                // against the interpreter cell by cell rather than flipped
                // to match the new build.
                "dTr7\nr:7\nend\n",
            ),
            // 4 — a NESTED body under the same shape: the payload's own body and
            //     its field's were both lost, so both have to come back.
            (
                "nested-drop-field",
                "struct In2 { n: i64 }\n\
                 impl Drop for In2 { fn drop(mut ref self) { println(f\"dIn{self.n}\") } }\n\
                 struct Ou2 { inner: In2, tag: i64 }\n\
                 impl Drop for Ou2 { fn drop(mut ref self) { println(f\"dOu{self.tag}\") } }\n\
                 fn eat(o: Option[Ou2]) -> i64 { match o { Some(t) => { t.tag } None => { 0 } } }\n\
                 fn main() { println(f\"r:{eat(Some(Ou2 { inner: In2 { n: 5 }, tag: 7 }))}\"); println(\"end\") }\n",
                // B-2026-09-14-19 — was the same string with the body LAST.
                // See the `tail-field-read` cell above: the call sits inside
                // the `println`, the payload dies in the callee, and
                // `--interp` printed the body first all along. Verified
                // against the interpreter cell by cell rather than flipped
                // to match the new build.
                "dOu7\ndIn5\nr:7\nend\n",
            ),
            // 5 — GUARD: payload has NO `Drop` of its own, so moving a
            //     `Drop`-bearing field out is legal and the RECEIVER owns that
            //     body. Correct today; must not gain a second one.
            (
                "guard-plain-struct-moves-drop-field",
                "struct In3 { n: i64 }\n\
                 impl Drop for In3 { fn drop(mut ref self) { println(f\"dIn{self.n}\") } }\n\
                 struct Hd3 { inner: In3, tag: i64 }\n\
                 fn eat(o: Option[Hd3]) -> In3 { match o { Some(t) => { return t.inner; } None => { return In3 { n: 0 }; } } }\n\
                 fn main() { let g: In3 = eat(Some(Hd3 { inner: In3 { n: 5 }, tag: 7 })); println(f\"g:{g.n}\"); println(\"end\") }\n",
                "g:5\ndIn5\nend\n",
            ),
            // 6 — GUARD: a TUPLE payload moving an element out, same hazard by a
            //     different route.
            (
                "guard-tuple-payload-moves-element",
                "fn eat(o: Option[(Tr, i64)]) -> Tr { match o { Some(t) => { return t.0; } None => { return Tr { tag: 0 }; } } }\n\
                 fn main() { let g: Tr = eat(Some((Tr { tag: 5 }, 9))); println(f\"g:{g.tag}\"); println(\"end\") }\n",
                "g:5\ndTr5\nend\n",
            ),
            // 7 — GUARD: payload with no own `Drop`, scalar read. Already
            //     correct before the change (a different channel supplies the
            //     field's body), so it pins that this did not disturb it.
            (
                "guard-plain-struct-scalar-read",
                "struct In4 { n: i64 }\n\
                 impl Drop for In4 { fn drop(mut ref self) { println(f\"dIn{self.n}\") } }\n\
                 struct Hd4 { inner: In4, tag: i64 }\n\
                 fn eat(o: Option[Hd4]) -> i64 { match o { Some(t) => { t.tag } None => { 0 } } }\n\
                 fn main() { println(f\"r:{eat(Some(Hd4 { inner: In4 { n: 5 }, tag: 7 }))}\"); println(\"end\") }\n",
                "dIn5\nr:7\nend\n",
            ),
            // 8 — GUARD: the whole payload handed to an accumulator that
            //     outlives the call. The bare identifier is NOT a projection, so
            //     it still stands the caller down — this is the exact cell
            //     B-2026-09-12-15's gate was built for.
            (
                "guard-consuming-callee-pushes-payload",
                "fn eat(o: Option[Tr], acc: mut ref Vec[Tr]) { match o { Some(t) => { acc.push(t) } None => { println(\"n\") } } }\n\
                 fn main() { let mut acc: Vec[Tr] = []; eat(Some(Tr { tag: 1 }), mut acc); println(f\"len:{acc.len()}\"); println(\"end\") }\n",
                "len:1\ndTr1\nend\n",
            ),
        ] {
            let Some(out) = run_program(&format!("{PRE}{body}")) else {
                return;
            };
            assert_eq!(out, want, "[{label}]");
        }
}

/// B-2026-09-06-63 — a callee that WRAPS a `Drop`-bearing argument in
/// another `Drop`-bearing type lost the WRAPPER's own body.
///
/// `let h = wrap_bodied(r)` makes `h` a param VIEW of `r`, which is right
/// about the FIELDS — they are the argument owner's and the caller fires
/// them — and wrong about the wrapper: `dH3` was nobody's, so the cell
/// printed `v=9 dR9` where a locally built `H` prints `v=9 dH3 dR9`.
///
/// The obvious repair is to decline the view, and B-2026-09-06-58 measured
/// that: the binding then takes a FULL ownership walk and prints
/// `dH3 dR9 dR9` — the wrapper recovered by doubling the argument. So the
/// fix adds the OWN body only (`__karac_dropselfbody_<T>` /
/// `run_user_drop_body_only`), keeping the fields with the argument owner.
///
/// Cells 4-6 are the hazards, and cell 4 is the one that caught a real
/// mistake while this was built: an IDENTITY callee (`fn keeps(r: R) -> R`)
/// returns a `Drop`-bearing type too, and a first version keyed only on
/// that printed `v=9 dR9 dR9` on both backends. The admitting predicate
/// therefore asks whether the callee wraps in a DIFFERENT type, not merely
/// whether the result has a body.
///
/// Cell 7 pins the NLL endpoint, which is where the slot actually fires:
/// `h`'s last use is the statement after its `let`, so a narrowing applied
/// only at scope exit would leave the full walk running here — measured, it
/// gave the same `dH3 dR9 dR9`.
#[test]
fn e2e_wrapped_argument_result_runs_the_wrappers_own_body() {
    const PRE: &str = "struct R { id: i64, name: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             struct H { r: R, n: i64 }\n\
             impl Drop for H { fn drop(mut ref self) { println(f\"dH{self.n}\") } }\n\
             fn mkr(i: i64) -> R { return R { id: i, name: f\"n{i}\" }; }\n\
             fn wrap_bodied(r: R) -> H { return H { r: r, n: 3 }; }\n";
    for (label, body, want) in [
            // 1 — the row's cell.
            (
                "wrapped",
                "fn f(r: R) { let h = wrap_bodied(r); println(f\"v={h.r.id}\"); }\n\
                 fn main() { f(mkr(9)); println(\"end\") }\n",
                "v=9\ndH3\ndR9\nend\n",
            ),
            // 2 — the ORACLE: the same `H` built locally, no callee. This is
            //     what says `dH3 dR9` is the target rather than a guess.
            (
                "built-locally-oracle",
                "fn f(r: R) { let h = H { r: r, n: 3 }; println(f\"v={h.r.id}\"); }\n\
                 fn main() { f(mkr(9)); println(\"end\") }\n",
                "v=9\ndH3\ndR9\nend\n",
            ),
            // 3 — the wrapper handed onward: the caller's binding owns it, and
            //     the body must still fire exactly once.
            (
                "returned-onward",
                "fn f(r: R) -> H { let h = wrap_bodied(r); println(f\"v={h.r.id}\"); return h; }\n\
                 fn main() { let g = f(mkr(9)); println(f\"g={g.n}\"); println(\"end\") }\n",
                "v=9\ng=3\ndH3\ndR9\nend\n",
            ),
            // 4 — HAZARD: an IDENTITY callee. Same object back, so its body is
            //     the parameter's own and the caller already fires it.
            (
                "identity-callee-hazard",
                "fn keeps(r: R) -> R { return r; }\n\
                 fn f(r: R) { let w = keeps(r); println(f\"v={w.id}\"); }\n\
                 fn main() { f(mkr(9)); println(\"end\") }\n",
                "v=9\ndR9\nend\n",
            ),
            // 5 — HAZARD: a wrapper with NO body of its own. Nothing to add,
            //     and its `Drop` field stays the argument owner's.
            (
                "bodyless-wrapper-hazard",
                "struct H3 { r: R }\n\
                 fn wrap_free(r: R) -> H3 { return H3 { r: r }; }\n\
                 fn f(r: R) { let h = wrap_free(r); println(f\"v={h.r.id}\"); }\n\
                 fn main() { f(mkr(9)); println(\"end\") }\n",
                "v=9\ndR9\nend\n",
            ),
            // 6 — HAZARD: a SCALAR argument (B-2026-09-06-53's class), which is
            //     not a view at all and must keep its ordinary full drop.
            (
                "scalar-argument-hazard",
                "fn mkUses(i: i64) -> R { return R { id: i, name: f\"u{i}\" }; }\n\
                 fn f() { let x = mkUses(4); println(f\"v={x.id}\"); }\n\
                 fn main() { f(); println(\"end\") }\n",
                "v=4\ndR4\nend\n",
            ),
            // 7 — the NLL endpoint: `h` is never read, so its slot fires at the
            //     `let` itself, before the `println`.
            (
                "nll-endpoint",
                "fn f(r: R) { let h = wrap_bodied(r); println(\"x\"); }\n\
                 fn main() { f(mkr(9)); println(\"end\") }\n",
                "dH3\nx\ndR9\nend\n",
            ),
            // 9 — HAZARD, and the one the hand probes missed: a callee taking
            //     BOTH the viewed argument and an unrelated second parameter.
            //     A first version scanned every argument index, so the SCALAR
            //     answered for the owned one — its type differs from the return
            //     type, which is all the type-level predicate asks — and the
            //     result gained a body it does not own. Caught by the existing
            //     `scalar_argument_does_not_make_the_result_a_view` pins on
            //     both backends (`dR15 dR15`), not by this file.
            (
                "mixed-args-hazard",
                "fn wrap_mixed(r: R, k: i64) -> H { return H { r: r, n: k }; }\n                 fn f(r: R) { let h = wrap_mixed(r, 5); println(f\"v={h.r.id}\"); }\n                 fn main() { f(mkr(9)); println(\"end\") }\n",
                "v=9\ndH5\ndR9\nend\n",
            ),
            // 10 — the same callee shape where the SCALAR is the viewed
            //      argument's neighbour on the other side, so an index-order
            //      accident cannot pass both.
            (
                "mixed-args-scalar-first",
                "fn wrap_first(k: i64, r: R) -> H { return H { r: r, n: k }; }\n                 fn f(r: R) { let h = wrap_first(6, r); println(f\"v={h.r.id}\"); }\n                 fn main() { f(mkr(9)); println(\"end\") }\n",
                "v=9\ndH6\ndR9\nend\n",
            ),
            // 8 — two wrappers in one frame, so a single shared registration
            //     keyed on the type rather than the binding would show up.
            (
                "two-wrappers",
                "fn f(a: R, b: R) { let h = wrap_bodied(a); let k = wrap_bodied(b); println(f\"v={h.r.id}{k.r.id}\"); }\n\
                 fn main() { f(mkr(1), mkr(2)); println(\"end\") }\n",
                "v=12\ndH3\ndH3\ndR2\ndR1\nend\n",
            ),
        ] {
            let Some(out) = run_program(&format!("{PRE}{body}")) else {
                return;
            };
            assert_eq!(out, want, "[{label}]");
        }
}

/// B-2026-09-24-21 — a tuple literal that moves an inline `Option`/`Result`
/// local now disarms it, and every owner of the tuple frees the payload: a
/// returned tuple (the row's own cell, a `let t = (..); t` return, a by-value
/// param handed back inside it, a conditional `return`), an annotated or
/// unannotated `let`, a destructure, a `Vec` push, a struct field, a `Some(..)`
/// wrap, a `match (a, b)` scrutinee, a temp argument, and a discarded
/// `let _ = (..)`, which leaves the local its owner. On `main` 21 of these
/// spellings double freed and the temp argument leaked.
#[test]
fn e2e_tuple_literal_moves_inline_optres_local() {
    let Some(out) = run_program(
        r#"struct S { t: (Option[String], i64) }
fn mk(k: i64) -> (Option[String], i64) { let label = Some(f"heap-string-longer-than-sso-{k}"); (label, k) }
fn mkt(k: i64) -> (Option[String], i64) { let label = Some(f"heap-string-longer-than-sso-{k}"); let t = (label, k); t }
fn mkp(label: Option[String], k: i64) -> (Option[String], i64) { (label, k) }
fn mkv(k: i64) -> (Option[Vec[i64]], i64) { let v: Vec[i64] = [k, 2, 3]; let o = Some(v); (o, k) }
fn mkr(k: i64) -> (Result[String, i64], i64) { let r: Result[String, i64] = Ok(f"heap-string-longer-than-sso-{k}"); (r, k) }
fn mkc(k: i64) -> (Option[String], i64) { let label = Some(f"heap-string-longer-than-sso-{k}"); if k > 5 { return (label, k); } (None, 0) }
fn eat(t: (Option[String], i64)) -> i64 { t.1 }
fn txt(o: ref Option[String]) -> String { match o { Some(s) => s.clone(), None => "none" } }
fn main() {
    let a = mk(1); println(f"{txt(a.0)} {a.1}");
    let b = mkt(2); println(f"{txt(b.0)} {b.1}");
    let l = Some(f"heap-string-longer-than-sso-3"); let c = mkp(l, 3); println(f"{txt(c.0)} {c.1}");
    let v = mkv(4); println(v.1);
    let r = mkr(5); println(r.1);
    let (d0, d1) = mk(6); println(f"{txt(d0)} {d1}");
    let e = mkc(7); let f = mkc(1); println(f"{txt(e.0)} {e.1} {f.1}");
    let mut n = 0; for i in 0..3 { let q = mk(i); n = n + q.1; } println(n);
    let x = Some(f"heap-string-longer-than-sso-8"); let tx: (Option[String], i64) = (x, 8); println(f"{txt(tx.0)} {tx.1}");
    let y = Some(f"heap-string-longer-than-sso-9"); let ty = (y, 9); let (y0, y1) = ty; println(f"{txt(y0)} {y1}");
    let z = Some(f"heap-string-longer-than-sso-10"); let mut vs: Vec[(Option[String], i64)] = []; vs.push((z, 10)); println(vs.len());
    let w = Some(f"heap-string-longer-than-sso-11"); let s = S { t: (w, 11) }; println(s.t.1);
    let u = Some(f"heap-string-longer-than-sso-12"); let o = Some((u, 12)); match o { Some(p) => println(p.1), None => println("n") }
    let g = Some(f"heap-string-longer-than-sso-13"); let h = Some(f"heap-string-longer-than-sso-14"); match (g, h) { (Some(p), Some(q)) => println(f"{p} {q}"), _ => println("n") }
    let m = Some(f"heap-string-longer-than-sso-15"); println(eat((m, 15)));
    let dd = Some(f"heap-string-longer-than-sso-16"); let _ = (dd, 16);
    let lt = Some(f"heap-string-longer-than-sso-17"); let tt = (lt, 17); println(eat(tt));
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "heap-string-longer-than-sso-1 1\nheap-string-longer-than-sso-2 2\nheap-string-longer-than-sso-3 3\n4\n5\nheap-string-longer-than-sso-6 6\nheap-string-longer-than-sso-7 7 0\n3\nheap-string-longer-than-sso-8 8\nheap-string-longer-than-sso-9 9\n1\n11\n12\nheap-string-longer-than-sso-13 heap-string-longer-than-sso-14\n15\n17\nend\n", "got:\n{out}");
}

/// B-2026-09-24-23 — a local that ownership promotes to an RC box (it is
/// consumed in one arm and read after the `match`) keeps its `Option` /
/// `Result` payload in the box, and the box now frees it: the box is named by
/// the full type and given that type's value drop, the slot registrars stand
/// down for the handle slot, and the pattern bindings of a later `match` /
/// `if let` / `while let` / `let … else` are views of the box, cloned when
/// they escape. On `main` every one of these spellings leaked the payload
/// (518 B over the program), and the `Result` ones printed wrong values at
/// `-O0` and under the JIT. This harness builds at the default opt level,
/// where `main` already printed these values, so this cell is a value pin on
/// the new path; the ASAN twin
/// (`asan_rc_fallback_optres_local_frees_its_payload`) is the one that fails
/// on `main`.
#[test]
fn e2e_rc_fallback_optres_local_frees_its_payload() {
    let Some(out) = run_program(
        r#"struct P { pos: i64 }
struct S { s: String, k: i64 }
impl P {
    fn take(mut ref self, doc: Option[String]) -> i64 { self.pos = self.pos + 1; match doc { Some(s) => s.len(), None => 0 } }
    fn item(mut ref self, t: i64) -> i64 { let doc = Some(f"heap-string-longer-than-sso-{t}"); let a = match t { 0 => self.take(doc), _ => 5 }; let b = match doc { Some(s) => s.len(), None => 0 }; a + b }
}
fn take(doc: Option[String]) -> i64 { match doc { Some(s) => s.len(), None => 0 } }
fn takev(doc: Option[Vec[i64]]) -> i64 { match doc { Some(s) => s.len(), None => 0 } }
fn takes(doc: Option[S]) -> i64 { match doc { Some(s) => s.s.len(), None => 0 } }
fn taker(doc: Result[String, i64]) -> i64 { match doc { Ok(s) => s.len(), Err(e) => e } }
fn takee(doc: Result[i64, String]) -> i64 { match doc { Ok(v) => v, Err(e) => e.len() } }
fn back(doc: Option[String]) -> Option[String] { doc }
fn read(t: i64) -> i64 { let doc = Some(f"heap-string-longer-than-sso-{t}"); let a = match t { 0 => take(doc), _ => 5 }; let b = match doc { Some(s) => s.len(), None => 0 }; a + b }
fn armmove(t: i64) -> String { let doc = Some(f"heap-string-longer-than-sso-{t}"); let a = match t { 0 => take(doc), _ => 5 }; let b = match doc { Some(s) => s, None => f"none" }; f"{a} {b}" }
fn handback(t: i64) -> i64 { let doc = Some(f"heap-string-longer-than-sso-{t}"); let a = match t { 0 => match back(doc) { Some(s) => s.len(), None => 0 }, _ => 5 }; let b = match doc { Some(s) => s.len(), None => 0 }; a + b }
fn whilelet(t: i64) -> i64 { let doc = Some(f"heap-string-longer-than-sso-{t}"); let a = match t { 0 => take(doc), _ => 5 }; let mut k = 0; while let Some(s) = doc { k = k + s.len(); break }; a + k }
fn letelse(t: i64) -> i64 { let doc = Some(f"heap-string-longer-than-sso-{t}"); let a = match t { 0 => take(doc), _ => 5 }; let Some(s) = doc else { return 0 }; a + s.len() }
fn optvec(t: i64) -> i64 { let doc = Some([1, 2, t]); let a = match t { 0 => takev(doc), _ => 5 }; let b = match doc { Some(s) => s.len(), None => 0 }; a + b }
fn optstruct(t: i64) -> i64 { let doc = Some(S { s: f"heap-string-longer-than-sso-{t}", k: t }); let a = match t { 0 => takes(doc), _ => 5 }; let b = match doc { Some(s) => s.k, None => 0 }; a + b }
fn resok(t: i64) -> i64 { let doc: Result[String, i64] = Ok(f"heap-string-longer-than-sso-{t}"); let a = match t { 0 => taker(doc), _ => 5 }; let b = match doc { Ok(s) => s.len(), Err(e) => e }; a + b }
fn resmove(t: i64) -> String { let doc: Result[String, i64] = Ok(f"heap-string-longer-than-sso-{t}"); let a = match t { 0 => taker(doc), _ => 5 }; let b = match doc { Ok(s) => s, Err(e) => f"e{e}" }; f"{a} {b}" }
fn reserr(t: i64) -> i64 { let doc: Result[i64, String] = Err(f"heap-string-longer-than-sso-{t}"); let a = match t { 0 => takee(doc), _ => 5 }; let b = match doc { Ok(v) => v, Err(e) => e.len() }; a + b }
fn main() {
    let mut p = P { pos: 0 };
    println(f"{p.item(0)} {p.item(1)}");
    println(f"{read(0)} {read(1)}");
    println(f"{armmove(0)} / {armmove(1)}");
    println(f"{handback(0)} {handback(1)}");
    println(f"{whilelet(0)} {whilelet(1)}");
    println(f"{letelse(0)} {letelse(1)}");
    println(f"{optvec(0)} {optvec(1)}");
    println(f"{optstruct(0)} {optstruct(1)}");
    println(f"{resok(0)} {resok(1)}");
    println(f"{resmove(0)} / {resmove(1)}");
    println(f"{reserr(0)} {reserr(1)}");
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "58 34\n58 34\n29 heap-string-longer-than-sso-0 / 5 heap-string-longer-than-sso-1\n58 34\n58 34\n58 34\n6 8\n29 6\n58 34\n29 heap-string-longer-than-sso-0 / 5 heap-string-longer-than-sso-1\n58 34\nend\n", "got:\n{out}");
}

/// B-2026-09-24-28 — the payload shapes B-2026-09-24-23 did not reach: an
/// RC-promoted local (consumed in one arm, read after the `match`) whose
/// payload is a tuple, a three-tuple or a nested `Option`. The box now takes
/// its value drop for these types, the boxed-enum chain registrar stands down
/// for the handle slot as the inline ones already did, an arm binding of the
/// box is not treated as a move out of it, and the argument handed to a
/// callee (a free function or a method) is a deep copy, since the callee
/// frees what it is given and the box still owns its own. On `main` the
/// tuple and nested-`Option` spellings segfaulted on every compiled surface
/// (invalid read, 40 B lost).
/// This harness builds at the default opt level; the ASAN twin
/// (`asan_rc_fallback_boxed_optres_payload_handed_a_copy`) is the cell that
/// sees the memory fault.
#[test]
fn e2e_rc_fallback_boxed_optres_payload_handed_a_copy() {
    let Some(out) = run_program(
        r#"struct P { pos: i64 }
impl P {
    fn take(mut ref self, doc: Option[(String, i64)]) -> i64 { self.pos = self.pos + 1; match doc { Some((s, k)) => s.len() + k, None => 0 } }
    fn item(mut ref self, t: i64) -> i64 { let doc = Some((f"heap-string-longer-than-sso-{t}", t)); let a = match t { 0 => self.take(doc), _ => 5 }; let b = match doc { Some((s, k)) => s.len() + k, None => 0 }; a + b }
}
fn take(doc: Option[(String, i64)]) -> i64 { match doc { Some((s, k)) => s.len() + k, None => 0 } }
fn take3(doc: Option[(String, String, i64)]) -> i64 { match doc { Some((s, u, k)) => s.len() + u.len() + k, None => 0 } }
fn takeo(doc: Option[Option[String]]) -> i64 { match doc { Some(Some(s)) => s.len(), _ => 0 } }
fn back(doc: Option[(String, i64)]) -> Option[(String, i64)] { doc }
fn tuple(t: i64) -> i64 { let doc = Some((f"heap-string-longer-than-sso-{t}", t)); let a = match t { 0 => take(doc), _ => 5 }; let b = match doc { Some((s, k)) => s.len() + k, None => 0 }; a + b }
fn tuple3(t: i64) -> i64 { let doc = Some((f"heap-string-longer-than-sso-{t}", f"second-heap-string-longer-than-sso", t)); let a = match t { 0 => take3(doc), _ => 5 }; let b = match doc { Some((s, u, k)) => s.len() + u.len() + k, None => 0 }; a + b }
fn nested(t: i64) -> i64 { let doc = Some(Some(f"heap-string-longer-than-sso-{t}")); let a = match t { 0 => takeo(doc), _ => 5 }; let b = match doc { Some(Some(s)) => s.len(), _ => 0 }; a + b }
fn handback(t: i64) -> i64 { let doc = Some((f"heap-string-longer-than-sso-{t}", t)); let a = match t { 0 => match back(doc) { Some((s, k)) => s.len() + k, None => 0 }, _ => 5 }; let b = match doc { Some((s, k)) => s.len() + k, None => 0 }; a + b }
fn armmove(t: i64) -> String { let doc = Some((f"heap-string-longer-than-sso-{t}", t)); let a = match t { 0 => take(doc), _ => 5 }; let b = match doc { Some((s, k)) => s, None => f"none" }; f"{a} {b}" }
fn iflet(t: i64) -> i64 { let doc = Some((f"heap-string-longer-than-sso-{t}", t)); let a = match t { 0 => take(doc), _ => 5 }; let b = if let Some((s, k)) = doc { s.len() + k } else { 0 }; a + b }
fn main() {
    let mut p = P { pos: 0 };
    println(f"{p.item(0)} {p.item(1)}");
    println(f"{tuple(0)} {tuple(1)}");
    println(f"{tuple3(0)} {tuple3(1)}");
    println(f"{nested(0)} {nested(1)}");
    println(f"{handback(0)} {handback(1)}");
    println(f"{armmove(0)} / {armmove(1)}");
    println(f"{iflet(0)} {iflet(1)}");
    let doc = Some((f"heap-string-longer-than-sso-1", 1));
    let mut n = 0;
    for i in 0..3 { if i == 2 { n = n + take(doc); } else { n = n + match doc { Some((s, k)) => s.len(), None => 0 }; } }
    println(n);
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "58 35\n58 35\n126 69\n58 34\n58 35\n29 heap-string-longer-than-sso-0 / 5 heap-string-longer-than-sso-1\n58 35\n88\nend\n", "got:\n{out}");
}

/// B-2026-09-24-31 — an `Option[Map]` / `Option[Set]` local moved on by
/// value is freed once. The `Map`/`Set` handle channel
/// (`inline_option_map_payload_vars`) was missing from the move disarm, the
/// hand-back alias, the discarded hand-back temp and the arm suppressor's
/// alias lookup. So every spelling here, including a call, a `let`, a `return`,
/// a method, a field, a conditional, a loop and a hand-back kept both source and
/// destination armed. On `main` the compiled program printed nothing and
/// valgrind counted 668 errors.
#[test]
fn e2e_option_map_local_moved_on_is_freed_once() {
    let Some(out) = run_program(
        r#"struct H { d: Option[Map[i64, String]] }
impl H { fn eat(self, doc: Option[Map[i64, String]]) -> i64 { match doc { Some(m) => m.len(), None => 0 } } }
fn mk(t: i64) -> Map[i64, String] { let mut m: Map[i64, String] = Map.new(); m.insert(t, f"heap-string-longer-than-sso-{t}"); m }
fn take(doc: Option[Map[i64, String]]) -> i64 { match doc { Some(m) => m.len(), None => 0 } }
fn takes(doc: Option[Set[i64]]) -> i64 { match doc { Some(s) => s.len(), None => 0 } }
fn keep(doc: Option[Map[i64, String]]) -> Option[Map[i64, String]] { doc }
fn ret(t: i64) -> Option[Map[i64, String]] { let d = Some(mk(t)); d }
fn cond(t: i64) -> i64 { let d = Some(mk(t)); if t == 0 { take(d) } else { 7 } }
fn main() {
    let m = mk(1); let d = Some(m); println(take(d));
    let d = Some(mk(2)); let q = d; println(take(q));
    let d = ret(3); println(take(d));
    let h = H { d: None }; let d = Some(mk(4)); println(h.eat(d));
    let d = Some(mk(5)); let g = H { d: d }; println(take(g.d));
    let mut s: Set[i64] = Set.new(); s.insert(6); let d = Some(s); println(takes(d));
    println(f"{cond(0)} {cond(1)}");
    let mut n = 0; for i in 0..3 { let d = Some(mk(i)); n = n + take(d); } println(n);
    let d = Some(mk(7)); let e = keep(d); println(take(e));
    let d = Some(mk(8)); keep(d);
    let d = Some(mk(9)); let e = keep(d); let k = match e { Some(x) => { let mut v: Vec[Map[i64, String]] = Vec.new(); v.push(x); v.len() }, None => 0 }; println(k);
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "1\n1\n1\n1\n1\n1\n1 7\n3\n1\n1\nend\n", "got:\n{out}");
}

/// B-2026-09-24-30 — the three shapes B-2026-09-24-28 left: an RC-promoted
/// local (moved into a call on one path, read again after) whose value is a
/// `Result` with a tuple / array / wide-tuple half, an `Option[Map]` /
/// `Option[Set]` / `Result[Map, E]` / `Result[E, Map]`, or an
/// `Option[Option[String]]` moved out through a nested `Some(Some(s))` arm.
/// The `Result` halves were handed to the callee without a deep copy, the
/// `Map` boxes had no value drop at all, and the nested arm aliased the box's
/// String; on `main` the compiled program printed nothing (a double free
/// aborted it) and the `Map` cells leaked the table under valgrind.
#[test]
fn e2e_rc_promoted_result_and_map_boxes_are_freed_once() {
    let Some(out) = run_program(
        r#"fn take_c0(doc: Result[(String, i64), i64]) -> i64 { match doc { Ok((s, k)) => s.len() + k, Err(e) => e } }
fn item_c0(t: i64) -> i64 { let doc: Result[(String, i64), i64] = Ok((f"heap-string-longer-than-sso-{t}", t)); let a = match t { 0 => take_c0(doc), _ => 5 }; let b = match doc { Ok((s, k)) => s.len() + k, Err(e) => e }; a + b }
fn take_c1(doc: Result[i64, (String, i64)]) -> i64 { match doc { Ok(k) => k, Err((s, k)) => s.len() + k } }
fn item_c1(t: i64) -> i64 { let doc: Result[i64, (String, i64)] = Err((f"heap-string-longer-than-sso-{t}", t)); let a = match t { 0 => take_c1(doc), _ => 5 }; let b = match doc { Ok(k) => k, Err((s, k)) => s.len() + k }; a + b }
fn take_c2(doc: Result[Array[String, 2], i64]) -> i64 { match doc { Ok(a) => a[0].len() + a[1].len(), Err(e) => e } }
fn item_c2(t: i64) -> i64 { let doc: Result[Array[String, 2], i64] = Ok([f"heap-string-longer-than-sso-{t}", f"x{t}"]); let a = match t { 0 => take_c2(doc), _ => 5 }; let b = match doc { Ok(a) => a[0].len(), Err(e) => e }; a + b }
fn take_c3(doc: Result[(String, String, i64), i64]) -> i64 { match doc { Ok((s, u, k)) => s.len() + u.len() + k, Err(e) => e } }
fn item_c3(t: i64) -> i64 { let doc: Result[(String, String, i64), i64] = Ok((f"heap-string-longer-than-sso-{t}", f"second-heap-string-longer-than-sso", t)); let a = match t { 0 => take_c3(doc), _ => 5 }; let b = match doc { Ok((s, u, k)) => s.len() + k, Err(e) => e }; a + b }
fn take_c4(doc: Option[Map[i64, String]]) -> i64 { match doc { Some(m) => m.len(), None => 0 } }
fn item_c4(t: i64) -> i64 { let mut m: Map[i64, String] = Map.new(); m.insert(t, f"heap-string-longer-than-sso-{t}"); let doc = Some(m); let a = match t { 0 => take_c4(doc), _ => 5 }; let b = match doc { Some(m) => m.len(), None => 0 }; a + b }
fn take_c5(doc: Option[Set[i64]]) -> i64 { match doc { Some(m) => m.len(), None => 0 } }
fn item_c5(t: i64) -> i64 { let mut m: Set[i64] = Set.new(); m.insert(t); m.insert(t + 1); let doc = Some(m); let a = match t { 0 => take_c5(doc), _ => 5 }; let b = match doc { Some(m) => m.len(), None => 0 }; a + b }
fn back_c6(doc: Option[Map[i64, String]]) -> Option[Map[i64, String]] { doc }
fn item_c6(t: i64) -> i64 { let mut m: Map[i64, String] = Map.new(); m.insert(t, f"heap-string-longer-than-sso-{t}"); let doc = Some(m); let a = match t { 0 => match back_c6(doc) { Some(x) => x.len(), None => 0 }, _ => 5 }; let b = match doc { Some(m) => m.len(), None => 0 }; a + b }
fn take_c7(doc: Option[Map[i64, String]]) -> i64 { match doc { Some(m) => m.len(), None => 0 } }
fn item_c7(t: i64) -> i64 { let mut m: Map[i64, String] = Map.new(); m.insert(t, f"heap-string-longer-than-sso-{t}"); let doc = Some(m); let a = match t { 0 => take_c7(doc), _ => 5 }; let b = match doc { Some(m) => { let mut v: Vec[Map[i64, String]] = Vec.new(); v.push(m); v.len() }, None => 0 }; a + b }
fn take_c8(doc: Result[Map[i64, String], i64]) -> i64 { match doc { Ok(m) => m.len(), Err(e) => e } }
fn item_c8(t: i64) -> i64 { let mut m: Map[i64, String] = Map.new(); m.insert(t, f"heap-string-longer-than-sso-{t}"); let doc: Result[Map[i64, String], i64] = Ok(m); let a = match t { 0 => take_c8(doc), _ => 5 }; let b = match doc { Ok(m) => m.len(), Err(e) => e }; a + b }
fn take_c9(doc: Result[i64, Map[i64, String]]) -> i64 { match doc { Ok(e) => e, Err(m) => m.len() } }
fn item_c9(t: i64) -> i64 { let mut m: Map[i64, String] = Map.new(); m.insert(t, f"heap-string-longer-than-sso-{t}"); let doc: Result[i64, Map[i64, String]] = Err(m); let a = match t { 0 => take_c9(doc), _ => 5 }; let b = match doc { Ok(e) => e, Err(m) => m.len() }; a + b }
fn take_c10(doc: Result[Map[i64, String], i64]) -> i64 { match doc { Ok(m) => m.len(), Err(e) => e } }
fn item_c10(t: i64) -> i64 { let mut m: Map[i64, String] = Map.new(); m.insert(t, f"heap-string-longer-than-sso-{t}"); let doc: Result[Map[i64, String], i64] = Ok(m); let a = match t { 0 => take_c10(doc), _ => 5 }; let b = match doc { Ok(m) => { let mut v: Vec[Map[i64, String]] = Vec.new(); v.push(m); v.len() }, Err(e) => e }; a + b }
fn keep_c11(doc: Result[Map[i64, String], i64]) -> Result[Map[i64, String], i64] { doc }
fn item_c11(t: i64) -> i64 { let mut m: Map[i64, String] = Map.new(); m.insert(t, f"heap-string-longer-than-sso-{t}"); let doc: Result[Map[i64, String], i64] = Ok(m); let a = match t { 0 => { let d = keep_c11(doc); match d { Ok(m) => m.len(), Err(e) => e } }, _ => 5 }; let b = match doc { Ok(m) => m.len(), Err(e) => e }; a + b }
fn take_c12(doc: Option[Option[String]]) -> i64 { match doc { Some(Some(s)) => s.len(), _ => 0 } }
fn item_c12(t: i64) -> String { let doc = Some(Some(f"heap-string-longer-than-sso-{t}")); let a = match t { 0 => take_c12(doc), _ => 5 }; let b = match doc { Some(Some(s)) => s, _ => f"none" }; f"{a} {b}" }
fn main() {
    println(f"{item_c0(0)} {item_c0(1)}");
    println(f"{item_c1(0)} {item_c1(1)}");
    println(f"{item_c2(0)} {item_c2(1)}");
    println(f"{item_c3(0)} {item_c3(1)}");
    println(f"{item_c4(0)} {item_c4(1)}");
    println(f"{item_c5(0)} {item_c5(1)}");
    println(f"{item_c6(0)} {item_c6(1)}");
    println(f"{item_c7(0)} {item_c7(1)}");
    println(f"{item_c8(0)} {item_c8(1)}");
    println(f"{item_c9(0)} {item_c9(1)}");
    println(f"{item_c10(0)} {item_c10(1)}");
    println(f"{item_c11(0)} {item_c11(1)}");
    println(f"{item_c12(0)} {item_c12(1)}");
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "58 35\n58 35\n60 34\n92 35\n2 6\n4 7\n2 6\n2 6\n2 6\n2 6\n2 6\n2 6\n29 heap-string-longer-than-sso-0 5 heap-string-longer-than-sso-1\nend\n", "got:\n{out}");
}

/// B-2026-09-25-1 — an `if let` / `while let` pattern binding that reuses
/// the name of a local moved earlier (`let doc = Some(m); if let Some(m) = doc
/// { m.len() }`). The CFG gave the pattern binding no rename frame, so the
/// inner `m`'s read paired with the outer `m`'s move: a false `UseAfterMove`
/// warning, and codegen's defensive copy then cloned the outer `Map` into
/// `doc` while the original, counted as moved, was never freed (629 B
/// definitely lost per call under valgrind, every backend).
#[test]
fn e2e_if_let_binding_shadowing_a_moved_local_frees_it() {
    let Some(out) = run_program(
        r#"fn take_c0(doc: Option[Map[i64, String]]) -> i64 { match doc { Some(m) => m.len(), None => 0 } }
fn item_c0(t: i64) -> i64 { let mut m: Map[i64, String] = Map.new(); m.insert(t, f"heap-string-longer-than-sso-{t}"); let doc = Some(m); let b = if let Some(m) = doc { m.len() } else { 0 }; b }
fn take_c1(doc: Option[Map[i64, String]]) -> i64 { match doc { Some(m) => m.len(), None => 0 } }
fn item_c1(t: i64) -> i64 { let mut m: Map[i64, String] = Map.new(); m.insert(t, f"heap-string-longer-than-sso-{t}"); let doc = Some(m); let a = match t { 0 => take_c1(doc), _ => 5 }; let b = if let Some(m) = doc { m.len() } else { 0 }; a + b }
fn item_c2(t: i64) -> i64 { let mut v: Vec[String] = Vec.new(); let s = f"heap-string-longer-than-sso-{t}"; v.push(s); let mut n = 0; while let Some(s) = v.pop() { n = n + s.len(); }; n }
fn item_c3(t: i64) -> i64 { let s = f"heap-string-longer-than-sso-{t}"; let o = Some(s); let b = if let Some(s) = o { s.len() } else { 0 }; b }
fn main() {
    println(f"{item_c0(0)} {item_c0(1)}");
    println(f"{item_c1(0)} {item_c1(1)}");
    println(f"{item_c2(0)} {item_c2(1)}");
    println(f"{item_c3(0)} {item_c3(1)}");
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "1 1\n2 6\n29 29\n29 29\nend\n", "got:\n{out}");
}

/// B-2026-09-25-2 — a `Map`/`Set` local moved into an owner and then read
/// again (`let doc = Some(m); m.len()`, `v.push(m); m.len()`, a struct field,
/// a user-enum payload, `Ok(m)`). `uam_defensive_copy` hands the owner a clone
/// so the source keeps the original, but every one of those sinks still
/// retracted the source's `FreeMapHandle`, so the original had no freer and
/// leaked whole on every backend.
#[test]
fn e2e_map_read_after_move_into_an_owner_keeps_its_free() {
    let Some(out) = run_program(
        r#"fn item_c0(t: i64) -> i64 { let mut m: Map[i64, String] = Map.new(); m.insert(t, f"heap-string-longer-than-sso-{t}"); let doc = Some(m); let b = m.len(); match doc { Some(x) => x.len() + b, None => b } }
fn item_c1(t: i64) -> i64 { let mut m: Map[i64, String] = Map.new(); m.insert(t, f"heap-string-longer-than-sso-{t}"); let mut v: Vec[Map[i64, String]] = Vec.new(); v.push(m); let b = m.len(); v.len() + b }
struct H { m: Map[i64, String] }
fn item_c2(t: i64) -> i64 { let mut m: Map[i64, String] = Map.new(); m.insert(t, f"heap-string-longer-than-sso-{t}"); let h = H { m: m }; let b = m.len(); h.m.len() + b }
enum E { A(Map[i64, String]), B }
fn item_c3(t: i64) -> i64 { let mut m: Map[i64, String] = Map.new(); m.insert(t, f"heap-string-longer-than-sso-{t}"); let e = E.A(m); let b = m.len(); match e { E.A(x) => x.len() + b, E.B => b } }
fn item_c4(t: i64) -> i64 { let mut m: Map[i64, String] = Map.new(); m.insert(t, f"heap-string-longer-than-sso-{t}"); let r: Result[Map[i64, String], i64] = Ok(m); let b = m.len(); match r { Ok(x) => x.len() + b, Err(_) => b } }
fn item_c5(t: i64) -> i64 { let mut m: Set[String] = Set.new(); m.insert(f"heap-string-longer-than-sso-{t}"); let doc = Some(m); let b = m.len(); match doc { Some(x) => x.len() + b, None => b } }
fn item_c6(t: i64) -> i64 { let mut m: Set[String] = Set.new(); m.insert(f"heap-string-longer-than-sso-{t}"); let mut v: Vec[Set[String]] = Vec.new(); v.push(m); let b = m.len(); v.len() + b }
fn main() {
    println(f"{item_c0(0)} {item_c0(1)}");
    println(f"{item_c1(0)} {item_c1(1)}");
    println(f"{item_c2(0)} {item_c2(1)}");
    println(f"{item_c3(0)} {item_c3(1)}");
    println(f"{item_c4(0)} {item_c4(1)}");
    println(f"{item_c5(0)} {item_c5(1)}");
    println(f"{item_c6(0)} {item_c6(1)}");
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out, "2 2\n2 2\n2 2\n2 2\n2 2\n2 2\n2 2\nend\n",
        "got:\n{out}"
    );
}

/// B-2026-09-25-3 — a `Map`/`Set` local moved into a sink that takes no
/// defensive copy (a tuple or array literal element, `Vec.insert`), or moved on
/// ONE PATH / in a loop and read after (`if c { v.push(m) }; m.len()`), which
/// the ownership pass RC-promotes rather than flags and codegen does not box
/// for a `Map`. The disarm then nulled the source slot, so the later
/// `m.len()` read a null handle: SIGSEGV at -O0/-O2, a `null pointer
/// dereference` panic under the JIT, on every one of these spellings. The
/// disarm now clones the table over the source's own slot first.
#[test]
fn e2e_map_moved_then_read_again_keeps_its_own_table() {
    let Some(out) = run_program(
        r#"fn item_c0(t: i64) -> i64 { let mut m: Map[i64, String] = Map.new(); m.insert(t, f"heap-string-longer-than-sso-{t}"); let p: (Map[i64, String], i64) = (m, 1); let b = m.len(); p.0.len() + b }
fn item_c1(t: i64) -> i64 { let mut m: Map[i64, String] = Map.new(); m.insert(t, f"heap-string-longer-than-sso-{t}"); let mut v: Vec[Map[i64, String]] = Vec.new(); v.insert(0, m); let b = m.len(); v.len() + b }
fn item_c2(t: i64) -> i64 { let mut m: Set[String] = Set.new(); m.insert(f"heap-string-longer-than-sso-{t}"); let p: (Set[String], i64) = (m, 1); let b = m.len(); p.0.len() + b }
fn item_c3(t: i64) -> i64 { let mut m: Map[i64, String] = Map.new(); m.insert(t, f"heap-string-longer-than-sso-{t}"); let p = [m]; let b = m.len(); p[0].len() + b }
fn item_c4(t: i64) -> i64 { let mut m: Map[i64, String] = Map.new(); m.insert(t, f"heap-string-longer-than-sso-{t}"); let mut n = 0; if t == 0 { let p: (Map[i64, String], i64) = (m, 1); n = p.0.len(); } n + m.len() }
fn item_c5(t: i64) -> i64 { let mut m: Map[i64, String] = Map.new(); m.insert(t, f"heap-string-longer-than-sso-{t}"); let mut n = 0; if t == 0 { let mut v: Vec[Map[i64, String]] = Vec.new(); v.insert(0, m); n = v.len(); } n + m.len() }
fn item_c6(t: i64) -> i64 { let mut m: Map[i64, String] = Map.new(); m.insert(t, f"heap-string-longer-than-sso-{t}"); let mut n = 0; if t == 0 { let mut v: Vec[Map[i64, String]] = Vec.new(); v.push(m); n = v.len(); } n + m.len() }
fn item_c7(t: i64) -> i64 { let mut m: Map[i64, String] = Map.new(); m.insert(t, f"heap-string-longer-than-sso-{t}"); let mut n = 0; if t == 0 { let d = Some(m); n = match d { Some(x) => x.len(), None => 0 }; } n + m.len() }
fn item_c8(t: i64) -> i64 { let mut m: Map[i64, String] = Map.new(); m.insert(t, f"heap-string-longer-than-sso-{t}"); let mut v: Vec[Map[i64, String]] = Vec.new(); for i in 0..3 { v.push(m); } v.len() + m.len() }
struct H { m: Map[i64, String] }
fn item_c9(t: i64) -> i64 { let mut m: Map[i64, String] = Map.new(); m.insert(t, f"heap-string-longer-than-sso-{t}"); let mut n = 0; if t == 0 { let h = H { m: m }; n = h.m.len(); } n + m.len() }
fn item_c10(t: i64) -> i64 { let mut m: Map[i64, String] = Map.new(); m.insert(t, f"heap-string-longer-than-sso-{t}"); let mut n = 0; if t == 0 { let q = m; n = q.len(); } n + m.len() }
fn main() {
    println(f"{item_c0(0)} {item_c0(1)}");
    println(f"{item_c1(0)} {item_c1(1)}");
    println(f"{item_c2(0)} {item_c2(1)}");
    println(f"{item_c3(0)} {item_c3(1)}");
    println(f"{item_c4(0)} {item_c4(1)}");
    println(f"{item_c5(0)} {item_c5(1)}");
    println(f"{item_c6(0)} {item_c6(1)}");
    println(f"{item_c7(0)} {item_c7(1)}");
    println(f"{item_c8(0)} {item_c8(1)}");
    println(f"{item_c9(0)} {item_c9(1)}");
    println(f"{item_c10(0)} {item_c10(1)}");
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out, "2 2\n2 2\n2 2\n2 2\n2 1\n2 1\n2 1\n2 1\n4 4\n2 1\n2 1\nend\n",
        "got:\n{out}"
    );
}

/// B-2026-09-25-13 — a local `Option` / `Result` with a heap payload handed
/// out as a BRANCH LEAF: `let s = Some(f".."); if c { s } else { None }`, the
/// `match`-arm spelling, and `let r = if c { s } else { None }`. The branch
/// value left with the buffer while the local kept its scope-exit free, so
/// every compiled surface aborted `free(): double free detected in tcache 2`;
/// `--interp` was right.
#[test]
fn test_e2e_option_local_handed_out_as_branch_leaf() {
    let out = run_program(
        r#"fn tail_if(c: bool, n: i64) -> Option[String] {
    let s = Some(f"heap-string-longer-than-sso-{n}");
    if c { s } else { None }
}

fn tail_two(c: bool, n: i64) -> Option[String] {
    let s = Some(f"heap-string-longer-than-sso-s{n}");
    let d = Some(f"heap-string-longer-than-sso-d{n}");
    if c { s } else { d }
}

fn let_bound(c: bool, n: i64) -> Option[String] {
    let s = Some(f"heap-string-longer-than-sso-l{n}");
    let r = if c { s } else { None };
    r
}

fn arm_tail(c: bool, n: i64) -> Option[String] {
    let s = Some(f"heap-string-longer-than-sso-m{n}");
    match c {
        true => s,
        false => None,
    }
}

fn res_tail(c: bool, n: i64) -> Result[String, i64] {
    let s: Result[String, i64] = Ok(f"heap-string-longer-than-sso-r{n}");
    if c { s } else { Err(n) }
}

fn main() {
    for i in 0..2 {
        let c = i == 0;
        println(tail_if(c, i).unwrap_or(f"none"));
        println(tail_two(c, i).unwrap());
        println(let_bound(c, i).unwrap_or(f"none"));
        println(arm_tail(c, i).unwrap_or(f"none"));
        match res_tail(c, i) {
            Ok(s) => println(s),
            Err(e) => println(f"err {e}"),
        }
        let s = Some(f"heap-string-longer-than-sso-main{i}");
        let r = if c { s } else { None };
        println(r.is_some());
    }
    println("end")
}
"#,
    );
    assert_eq!(
        out.as_deref(),
        Some("heap-string-longer-than-sso-0\nheap-string-longer-than-sso-s0\nheap-string-longer-than-sso-l0\nheap-string-longer-than-sso-m0\nheap-string-longer-than-sso-r0\ntrue\nnone\nheap-string-longer-than-sso-d1\nnone\nnone\nerr 1\nfalse\nend\n"),
        "must match --interp"
    );
}
