//! generics, monomorphisation, traits, associated items -- fixtures for `tests/memory_sanitizer.rs`.
//!
//! Split out of `tests/memory_sanitizer.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test memory_sanitizer` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test memory_sanitizer generics::
//!
//! New fixtures about generics, monomorphisation, traits, associated items belong in this file.

use super::*;

/// B-2026-09-07-4's MIXED-PATH half — the MEMORY question, under ASAN + LSan,
/// where the pre-fix build double-freed the object a mixed-path method or
/// assoc fn handed back on its escaping leg.
///
/// Carries the WHOLE cell set including the DIES-INSIDE legs (`c`/`d`/`j`/
/// `l`/`n`), which matter more here than in the output pin: this fix's own
/// near-miss retracted their only memory owner and cost `d` 19 bytes in 2
/// blocks under valgrind. LSan is what catches that on Linux CI, so the leg
/// belongs in this file and not only in the A/B string.
///
/// Floored at 60 allocations (76 measured at -O2) because this class hides
/// under DCE: an allocation whose only consumer is a callee that hands it
/// straight back is exactly what LLVM removes at -O2, and a collapsed
/// program reads clean with nothing left to free.
#[test]
fn asan_method_and_assoc_mixed_path_hand_back_owns_its_argument() {
    assert_clean_asan_run_min_allocs(
            "shared struct Inner { v: i64 }\n\
             struct R { id: i64, name: String, inner: Inner }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             fn mk(i: i64) -> R { return R { id: i, name: f\"h{i}\", inner: Inner { v: i } }; }\n\
             fn fwd(r: R) -> R { return r; }\n\
             \n\
             struct S { id: i64, name: String }\n\
             impl Drop for S { fn drop(mut ref self) { println(f\"dS{self.id}\") } }\n\
             fn mks(i: i64) -> S { return S { id: i, name: f\"s{i}\" }; }\n\
             \n\
             struct Hold { n: i64 }\n\
             impl Hold {\n\
                 fn pick(ref self, r: R, k: bool) -> R { if k { return mk(98); } return r; }\n\
                 fn passmv(ref self, r: R) -> R { return fwd(r); }\n\
                 fn passmb(ref self, r: R) -> R { return r; }\n\
                 fn picks(ref self, s: S, k: bool) -> S { if k { return mks(96); } return s; }\n\
             }\n\
             impl R {\n\
                 fn passb(r: R) -> R { return fwd(r); }\n\
                 fn picka(r: R, k: bool) -> R { if k { return mk(92); } return r; }\n\
             }\n\
             fn pickf(r: R, k: bool) -> R { if k { return mk(97); } return r; }\n\
             \n\
             // method, MIXED-PATH bare, escaping leg, fresh temp\n\
             fn a1() { let h = Hold { n: 1 }; let z = h.pick(mk(21), false); println(f\"a{z.id}\"); }\n\
             // method, MIXED-PATH bare, escaping leg, NAMED LOCAL\n\
             fn a2() { let h = Hold { n: 1 }; let a = mk(37); let z = h.pick(a, false); println(f\"b{z.id}\"); }\n\
             // method, MIXED-PATH bare, DIES-INSIDE leg -- the leak control, both spellings\n\
             fn a3() { let h = Hold { n: 1 }; let z = h.pick(mk(22), true); println(f\"c{z.id}\"); }\n\
             fn a4() { let h = Hold { n: 1 }; let a = mk(38); let z = h.pick(a, true); println(f\"d{z.id}\"); }\n\
             // method, ALL-PATHS via-call, both spellings\n\
             fn a5() { let h = Hold { n: 1 }; let z = h.passmv(mk(26)); println(f\"e{z.id}\"); }\n\
             fn a6() { let h = Hold { n: 1 }; let a = mk(36); let z = h.passmv(a); println(f\"f{z.id}\"); }\n\
             // method, ALL-PATHS bare -- B-2026-09-06-70's fix, must stay clean\n\
             fn a7() { let h = Hold { n: 1 }; let z = h.passmb(mk(25)); println(f\"g{z.id}\"); }\n\
             // assoc, ALL-PATHS via-call\n\
             fn a8() { let z = R.passb(mk(19)); println(f\"h{z.id}\"); }\n\
             // assoc, MIXED-PATH bare, both legs\n\
             fn a9() { let z = R.picka(mk(32), false); println(f\"i{z.id}\"); }\n\
             fn a10() { let z = R.picka(mk(33), true); println(f\"j{z.id}\"); }\n\
             // free-fn mixed-path -- B-2026-09-06-69's own shape, must stay clean\n\
             fn a11() { let z = pickf(mk(23), false); println(f\"k{z.id}\"); }\n\
             fn a12() { let z = pickf(mk(24), true); println(f\"l{z.id}\"); }\n\
             // COPY-SUPPORTED class -- the -08-26-9 carve-out, must keep its memory\n\
             fn a13() { let h = Hold { n: 1 }; let z = h.picks(mks(41), false); println(f\"m{z.id}\"); }\n\
             fn a14() { let h = Hold { n: 1 }; let z = h.picks(mks(42), true); println(f\"n{z.id}\"); }\n\
             \n\
             fn main() {\n\
                 a1(); a2(); a3(); a4(); a5(); a6(); a7(); a8(); a9(); a10(); a11(); a12(); a13(); a14();\n\
                 println(\"end\");\n\
             }\n",
            &[
                "a21",
                "dR21",
                "b37",
                "dR37",
                "dR22",
                "c98",
                "dR98",
                "dR38",
                "d98",
                "dR98",
                "e26",
                "dR26",
                "f36",
                "dR36",
                "g25",
                "dR25",
                "h19",
                "dR19",
                "i32",
                "dR32",
                "dR33",
                "j92",
                "dR92",
                "k23",
                "dR23",
                "dR24",
                "l97",
                "dR97",
                "m41",
                "dS41",
                "dS42",
                "n96",
                "dS96",
                "end"
            ],
            "method_and_assoc_mixed_path_hand_back",
            60,
        );
}

/// B-2026-09-01-40 — the heap-carrying form of the doubled sibling body,
/// under ASAN + LSan.
///
/// WHAT THIS PINS, and what it does NOT. The defect was a body COUNT, not a
/// memory error: the extra `__karac_dropbodies_*` walk ran user `Drop`
/// bodies a second time but freed nothing twice, so ASAN was GREEN on this
/// program BEFORE the fix and this case would not have caught it. The row
/// records that measurement, and it is the same blind spot B-2026-09-01-38
/// names for its own family.
///
/// What it pins is the OTHER direction, which is the risk this particular
/// fix carries: the fix removes a registered walker, and removing a walker
/// is how bodies -- and, for a walker that also frees, memory -- go missing.
/// On Linux this runs LeakSanitizer too, so a leak introduced by dropping
/// that registration fails here. The transcript is asserted alongside, so a
/// "fix" that silenced the double by losing the surviving field's body
/// entirely fails on the output rather than passing quietly.
#[test]
fn asan_let_bound_scalar_field_read_sibling_body_runs_once() {
    assert_clean_asan_run(
            "struct R { s: String, xs: Vec[i64] }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.xs.len()}:{self.s}\") } }\n\
             struct H { r: R, n: i64 }\n\
             impl Drop for H { fn drop(mut ref self) { println(\"dH\") } }\n\
             fn main() {\n\
             \x20   let h = H { r: R { s: \"payload\", xs: [1, 2, 3] }, n: 4 };\n\
             \x20   let q = h.n;\n\
             \x20   println(f\"{q}\");\n\
             \x20   println(\"end\");\n\
             }\n",
            &["dH", "dR3:payload", "4", "end"],
            "b40-let-bound-scalar-field-read",
        );
}

/// B-2026-09-05-5 — the memory gate on the nested-generic-field fix.
///
/// The fix widens the Drop-field GATE, so the risk is not a missed free
/// but a walker now emitted for a parent that never had one — running a
/// nested body over a field the callee also owns, or over one already
/// freed by the parent's memory drop (the frame is LIFO and memory is
/// pushed first, so bodies read before frees). Both parents, both argument
/// forms, three iterations, so any imbalance accumulates.
#[test]
fn asan_nested_generic_struct_field_bodies_are_balanced() {
    let mut expected: Vec<&str> = Vec::new();
    for _ in 0..3 {
        expected.extend(["dR6", "dR7", "dR8", "dR4"]);
    }
    expected.push("done");
    assert_clean_asan_run_min_allocs(
        r#"
struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] }; }
struct Gd[T]  { r: T, z: i64 }
struct Gn3    { inner: Gd[R], z: i64 }
struct Gn2[T] { inner: Gd[T], z: i64 }
fn take3(h: Gn3) -> i64 { return h.z; }
fn gNest[T](h: Gn2[T]) -> i64 { return h.z; }
fn c_conc_temp()  { let _ = take3(Gn3 { inner: Gd[R] { r: mk(6), z: 1 }, z: 9 }); }
fn c_conc_local() { let h = Gn3 { inner: Gd[R] { r: mk(7), z: 1 }, z: 9 }; let _ = take3(h); }
fn c_gen_local()  { let h = Gn2[R] { inner: Gd[R] { r: mk(8), z: 1 }, z: 9 }; let _ = gNest(h); }
fn c_gen_temp()   { let _ = gNest(Gn2[R] { inner: Gd[R] { r: mk(4), z: 1 }, z: 9 }); }
fn main() {
    let mut i = 0;
    while i < 3 {
        c_conc_temp(); c_conc_local(); c_gen_local(); c_gen_temp();
        i = i + 1;
    }
    println("done");
}
"#,
        &expected,
        "b0905-5-nested-generic-struct-field",
        24,
    );
}

/// B-2026-09-04-24 — the memory gate on the generic-call temp-arg fix.
///
/// This one is specifically a DOUBLE-FREE watch rather than a leak watch,
/// because the pre-fix state was already leak-free: the memory half of this
/// registration has resolved the temp's instantiation since B-2026-08-06-2,
/// so `__karac_drop_struct_G$R` ran and valgrind reported no leaks while the
/// body was silent. The fix adds a BODIES walk beside that existing free, on
/// the same cleanup frame — so the risk it introduces is the mirror image:
/// a walk that frees something the memory half also frees, or one that reads
/// a field after it. The frame drains LIFO and the memory action is pushed
/// first, which is what keeps the body ahead of the free it reads through.
///
/// Both argument forms and both genericities, three iterations, so an
/// imbalance accumulates rather than cancelling.
#[test]
fn asan_generic_call_struct_temp_arg_bodies_are_balanced() {
    assert_clean_asan_run_min_allocs(
        r#"
struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}/{self.xs.len()}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] }; }
struct G[T] { pe: (T, i64), z: i64 }
struct Gn { pe: (R, i64), z: i64 }
fn gfn[T](h: G[T]) -> i64 { let (r, k) = h.pe; return k; }
fn nfn(h: Gn) -> i64 { let (r, k) = h.pe; return k; }
fn gLocal() { let h = G[R] { pe: (mk(80), 5), z: 9 }; let _ = gfn(h); }
fn gTemp()  { let _ = gfn(G[R] { pe: (mk(81), 5), z: 9 }); }
fn nLocal() { let h = Gn   { pe: (mk(82), 5), z: 9 }; let _ = nfn(h); }
fn nTemp()  { let _ = nfn(Gn   { pe: (mk(83), 5), z: 9 }); }
fn main() {
    let mut i = 0;
    while i < 3 {
        gLocal(); gTemp(); nLocal(); nTemp();
        i = i + 1;
    }
    println("done");
}
"#,
        &[
            "dR80/t80/1",
            "dR81/t81/1",
            "dR82/t82/1",
            "dR83/t83/1",
            "dR80/t80/1",
            "dR81/t81/1",
            "dR82/t82/1",
            "dR83/t83/1",
            "dR80/t80/1",
            "dR81/t81/1",
            "dR82/t82/1",
            "dR83/t83/1",
            "done",
        ],
        "b0904-24-generic-call-struct-temp-arg",
        24,
    );
}

/// B-2026-09-05-3 — A GENERIC CALLEE THAT RETURNS A DESTRUCTURED FIELD
/// DOUBLE-FREES WHEN THE ARGUMENT IS A TEMP LITERAL.
///
/// `fn gEsc[T](h: Gd[T]) -> T { let Gd { r, z } = h; return r }` over a
/// `Gd[R] { r: mk(5), z: 9 }` temp aborted with `free(): double free
/// detected in tcache 2` on all three compiled surfaces (valgrind: 2x
/// invalid free) against a clean interpreter. The row's discriminator was
/// wrong in an instructive way: neither the temp literal nor `Drop` selects
/// it — `Gd[String]` double-freed too, and `return h.r` without a
/// destructure was clean. The trigger is the destructuring `let` of a
/// by-value param inside a MONOMORPH with the leaf handed out: the leaf
/// predicates in `finish_owned_struct_destructure` read the declaration's
/// `r: T`, found nothing transferable, registered no leaf cleanup and never
/// zeroed the source's `r` cap, so `h`'s scope-exit drain freed the buffers
/// the returned `r` still owned. The fix resolves the field types under the
/// source's instantiation first.
///
/// Four shapes, three rounds: the row's temp, the differently-named fn
/// param (`gEsc2[U]`, whose AOT was clean only because the transfer path
/// happened to cancel the miss, and whose JIT double-freed), a `T` with no
/// `Drop` anywhere, and the leaf dropped inside the callee (the control
/// whose order must not move). Every body once, in the interpreter's order.
#[test]
fn asan_generic_callee_destructured_escaping_leaf_frees_once() {
    assert_clean_asan_run_min_allocs(
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
fn main() {
    let mut i = 0;
    while i < 3 {
        c_esc(); c_u(); c_str(); c_z();
        i = i + 1;
    }
    println("done");
}
"#,
        &[
            "in", "got5", "dR5", "in2", "got11", "dR11", "in", "gots8", "inZ", "dR12", "got9",
            "in", "got5", "dR5", "in2", "got11", "dR11", "in", "gots8", "inZ", "dR12", "got9",
            "in", "got5", "dR5", "in2", "got11", "dR11", "in", "gots8", "inZ", "dR12", "got9",
            "done",
        ],
        "b0905-3-generic-callee-destructured-escaping-leaf",
        24,
    );
}

/// B-2026-09-05-6 — the memory gate on removing a duplicate `Drop` body.
///
/// The defect was a COUNT: a place struct argument whose field the callee
/// hands back ran that field's body twice, once from the caller's own field
/// walk at `g`'s live-range end and once from the result's binding. It was
/// valgrind-clean throughout, because the callee's entry copy gives each
/// body a buffer of its own — which is exactly why this pin matters. The
/// fix masks one of the two fires, and the failure mode of masking a fire
/// is the opposite of the original: a leak, or a body that no longer runs
/// over a live object.
///
/// Four shapes over three rounds, mirroring the E2E twin: the destructure
/// spelling, a two-field struct whose non-escaping sibling `b` must KEEP
/// its body inside the call (the leg that would leak if the mask were
/// per-binding instead of per-field), the projection spelling, and a
/// generic callee. Measured on this fixture post-fix: exit 0, no
/// LeakSanitizer report; the standalone build is 33 allocs / 33 frees under
/// valgrind with 0 errors. (LSan is Linux-only; see CLAUDE.md.)
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
/// The memory gate is the point of this one: the E2E twin sees a body
/// count, and defect 1 is a genuine DOUBLE FREE — the callee frees the
/// caller's `String` and `Vec` buffers, then the caller frees them again.
/// It aborts before stdout is flushed under `karac run` and under any
/// `-O0` build, and hides at the default `-O2`, so an output assertion at
/// the default level is not on its own enough. Three rounds so a
/// per-round imbalance accumulates.
#[test]
fn asan_generic_tuple_param_element_frees_once() {
    assert_clean_asan_run_min_allocs(
        r#"
struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] }; }
struct Gd[T] { r: T, z: i64 }
fn kEsc[T](p: (T, i64)) -> T { let (r, z) = p; println("in"); return r; }
fn nEsc(p: (R, i64)) -> R { let (r, z) = p; println("in2"); return r; }
fn zEsc[T](p: (T, i64)) -> i64 { let (r, z) = p; println("in3"); return z; }
fn kEsc1[T](p: (i64, T)) -> T { let (z, r) = p; println("in5"); return r; }
fn tOnly[T](p: (T, i64)) { let (r, z) = p; println("in6"); }
fn gEsc[T](h: Gd[T]) -> T { let Gd { r, z } = h; println("in7"); return r; }
fn round() {
  let a = (mk(91), 9);  let o1 = kEsc(a);   println(f"got{o1.id}");
  let b = (mk(92), 9);  let o2 = nEsc(b);   println(f"got{o2.id}");
  let c = (mk(93), 7);  let o3 = zEsc(c);   println(f"gotz{o3}");
  let d = (mk(94), 9);  let _  = kEsc(d);   println("after");
  let e = (5, mk(95));  let o5 = kEsc1(e);  println(f"got{o5.id}");
  let f = (mk(96), 9);  tOnly(f);           println("after6");
  let g = Gd[R] { r: mk(97), z: 9 }; let _ = gEsc(g); println("after7");
}
fn main() {
    let mut i = 0;
    while i < 3 { round(); i = i + 1; }
    println("done");
}
"#,
        &[
            "in", "got91", "dR91", "in2", "got92", "dR92", "in3", "dR93", "gotz7", "in", "dR94",
            "after", "in5", "got95", "dR95", "in6", "dR96", "after6", "in7", "dR97", "after7",
            "in", "got91", "dR91", "in2", "got92", "dR92", "in3", "dR93", "gotz7", "in", "dR94",
            "after", "in5", "got95", "dR95", "in6", "dR96", "after6", "in7", "dR97", "after7",
            "in", "got91", "dR91", "in2", "got92", "dR92", "in3", "dR93", "gotz7", "in", "dR94",
            "after", "in5", "got95", "dR95", "in6", "dR96", "after6", "in7", "dR97", "after7",
            "done",
        ],
        "b0905-18-generic-tuple-param-element",
        36,
    );
}

#[test]
/// B-2026-09-07-51 — the OWNERSHIP half of
/// `e2e_generic_conditional_store_runs_one_body_on_the_missed_path`.
///
/// The mono param loop carried no conditional-store registration, so on the
/// path where the store does not happen the value died with no owner in any
/// frame — the caller had already stood down. That is a lost `Drop` body
/// AND a leak: 16 B in 1 block at -O0 (9 allocs / 8 frees), the argument's
/// `shared` field refcount block. The non-generic twin of the identical
/// callee measured 10 / 10 clean on the same tree.
///
/// Balanced at both opt levels after the fix, which matters because the
/// leak is small and `-O2` can delete a dead allocation outright — a check
/// at one level only would not have separated "owned" from "optimised
/// away".
fn asan_generic_conditional_store_owns_the_missed_path() {
    assert_clean_asan_run_min_allocs(
        r#"
shared struct Inner { v: i64 }
struct Ri { id: i64, inner: Inner }
impl Drop for Ri { fn drop(mut ref self) { println(f"dI{self.id}") } }
fn mki(i: i64) -> Ri { return Ri { id: i, inner: Inner { v: i } }; }
struct Sp { id: i64, name: String }
impl Drop for Sp { fn drop(mut ref self) { println(f"dP{self.id}") } }
fn mkp(i: i64) -> Sp { return Sp { id: i, name: f"p{i}" }; }
struct Vi { mut xs: Vec[Ri] }

fn gcond[T](v: mut ref Vec[T], x: T, k: bool) { if k { v.push(x); } }
fn guncond[T](v: mut ref Vec[T], x: T) { v.push(x); }
fn fcond(b: mut ref Vi, r: Ri, k: bool) { if k { b.xs.push(r); } }

fn main() {
    let mut a: Vec[Ri] = Vec.new();
    gcond(mut a, mki(1), false);
    gcond(mut a, mki(2), true);
    println(f"n{a.len()}");
    let mut b: Vec[Sp] = Vec.new();
    gcond(mut b, mkp(3), false);
    let mut c: Vec[Ri] = Vec.new();
    guncond(mut c, mki(4));
    println(f"m{c.len()}");
    let mut d = Vi { xs: Vec.new() };
    fcond(mut d, mki(5), false);
    let mut i = 0;
    while i < 3 { gcond(mut a, mki(i), false); i = i + 1; }
    println("end");
}
"#,
        &[
            "dI1", "n1", "dP3", "m1", "dI4", "dI5", "dI0", "dI1", "dI2", "dI2", "end",
        ],
        "b0907-51-generic-conditional-store",
        18,
    );
}

#[test]
fn asan_generic_struct_destructured_from_a_tuple_param_frees_once() {
    for (label, decl, arg) in [
        (
            "struct-first",
            "(Bag[T], i64)",
            "(Bag { xs: [\"x\", \"y\"] }, 0)",
        ),
        (
            "struct-second",
            "(i64, Bag[T])",
            "(0, Bag { xs: [\"x\", \"y\"] })",
        ),
        (
            "both-structs",
            "(Bag[T], Bag[T])",
            "(Bag { xs: [\"x\", \"y\"] }, Bag { xs: [\"q\"] })",
        ),
    ] {
        let pat = if label == "struct-second" {
            "(_n, b)"
        } else {
            "(b, _c)"
        };
        let src = format!(
            "struct Bag[T] {{ xs: Vec[T] }}\n\
                 fn take[T](p: {decl}) -> Vec[T] {{ let {pat} = p; b.xs }}\n\
                 fn main() {{ let r = take({arg}); println(f\"len={{r.len()}} [{{r[0]}}]\"); }}\n"
        );
        assert_clean_asan_run(&src, &["len=2 [x]"], label);
    }
    // The scalar-element leg: `Vec[i64]` double-freed under the JIT even
    // though AOT happened to survive it, so the element type is not what
    // makes this a memory bug.
    assert_clean_asan_run(
        r#"
struct Bag[T] { xs: Vec[T] }
fn take[T](p: (Bag[T], i64)) -> Vec[T] { let (b, _n) = p; b.xs }
fn main() { let r = take((Bag { xs: [10, 20] }, 0)); println(f"len={r.len()} [{r[0]}]"); }
"#,
        &["len=2 [10]"],
        "scalar-element",
    );
    // The fix ADDS a callee-side entry-copy + scope-exit drop to the mono's
    // tuple param, so these three are the shapes where that could leak or
    // over-free rather than balance: the param RETURNED whole (its drop must
    // be move-suppressed, else the copy the caller now owns is freed twice),
    // a NAMED binding as the argument (the caller's binding owns the
    // original), and a CALL TEMPORARY as the argument.
    // The two ESCAPE shapes, guarded here as B-2026-08-27-44 asked when it
    // was filed. Both leaked 48 bytes under LSan — the caller entry-copies
    // and the callee returns the COPY, so the caller's ORIGINAL is orphaned
    // — and both are covered in the GENERIC and NON-generic spellings,
    // because the non-generic one leaked on an unmodified tree while the
    // generic one was clean only by aliasing the buffer it should have
    // copied. Fixing -37 unmasked the generic half; -44 fixed both.
    for (label, prog, want) in [
        (
            "escape-passthru-generic",
            r#"
struct Bag[T] { xs: Vec[T] }
fn passthru[T](p: (Bag[T], i64)) -> (Bag[T], i64) { p }
fn main() { let (b, n) = passthru((Bag { xs: ["x", "y"] }, 7)); println(f"{n} {b.xs.len()} {b.xs[0]}"); }
"#,
            "7 2 x",
        ),
        (
            "escape-passthru-plain",
            r#"
struct Bag { xs: Vec[String] }
fn passthru(p: (Bag, i64)) -> (Bag, i64) { p }
fn main() { let (b, n) = passthru((Bag { xs: ["x", "y"] }, 7)); println(f"{n} {b.xs.len()} {b.xs[0]}"); }
"#,
            "7 2 x",
        ),
        (
            "escape-call-temp-generic",
            r#"
struct Bag[T] { xs: Vec[T] }
fn mk[T](v: Vec[T]) -> (Bag[T], i64) { (Bag { xs: v }, 3) }
fn use2[T](p: (Bag[T], i64)) -> i64 { let (b, n) = p; b.xs.len() + n }
fn main() { println(f"{use2(mk(["x", "y"]))}"); }
"#,
            "5",
        ),
        (
            "escape-call-temp-plain",
            r#"
struct Bag { xs: Vec[String] }
fn mk(v: Vec[String]) -> (Bag, i64) { (Bag { xs: v }, 3) }
fn use2(p: (Bag, i64)) -> i64 { let (b, n) = p; b.xs.len() + n }
fn main() { println(f"{use2(mk(["x", "y"]))}"); }
"#,
            "5",
        ),
        // A bare `String` element rather than a struct: the predicate keys
        // on drop-bearing heap, not on a NAMED type, and this spelling
        // leaked 3 bytes on the unfixed tree.
        (
            "escape-passthru-string-elem",
            r#"
fn passthru(p: (String, i64)) -> (String, i64) { p }
fn main() { let (s, n) = passthru((f"abc", 7)); println(f"{n} {s}"); }
"#,
            "7 abc",
        ),
        // TWO call temporaries in one expression — 72 bytes over 2 blocks
        // unfixed, so the registration is per-argument and not once-per-fn.
        (
            "escape-two-call-temps",
            r#"
struct Bag { xs: Vec[String] }
fn mk(v: Vec[String]) -> (Bag, i64) { (Bag { xs: v }, 3) }
fn use2(p: (Bag, i64)) -> i64 { let (b, n) = p; b.xs.len() + n }
fn main() { println(f"{use2(mk(["x"])) + use2(mk(["y", "z"]))}"); }
"#,
            "9",
        ),
    ] {
        assert_clean_asan_run(prog, &[want], label);
    }
    assert_clean_asan_run(
        r#"
struct Bag[T] { xs: Vec[T] }
fn takes[T](p: (Bag[T], i64)) -> i64 { let (b, n) = p; b.xs.len() + n }
fn main() { let t = (Bag { xs: ["x", "y"] }, 7); println(f"{takes(t)}"); }
"#,
        &["9"],
        "named-binding-arg",
    );
}

#[test]
fn test_converging_two_pointer_bce_skip_stays_in_bounds() {
    // B-2026-08-04-8: the converging skip drops BOTH halves of the bounds
    // check on `v[base + lo]` / `v[base + hi]`, so nothing at runtime
    // stops an over-wide index from walking off the buffer. ASAN is the
    // backstop: it reads AND writes every cell of the last row (the tight
    // case the linear identity has to get exactly right — `base + hi_init`
    // must land on the final element, not one past it).
    //
    // Rows are deliberately ODD-width so `lo` and `hi` meet on a middle
    // cell, and the corpus is exactly `n * len` so there is no slack to
    // absorb an off-by-one.
    assert_clean_asan_run(
        r#"
fn main() {
    let n = 7i64;
    let len = 5i64;
    let mut v: Vec[i64] = Vec.filled(n * len, 0i64);
    let mut i = 0i64;
    while i < n {
        let base = i * len;
        let mut lo = 0i64;
        let mut hi = len - 1i64;
        while lo <= hi {
            v[base + lo] = v[base + lo] + 1i64;
            v[base + hi] = v[base + hi] + 1i64;
            lo = lo + 1i64;
            hi = hi - 1i64;
        }
        i = i + 1;
    }
    let mut total = 0i64;
    let mut k = 0i64;
    while k < n * len {
        total = total + v[k];
        k = k + 1i64;
    }
    println(f"{total}");
}
"#,
        // Per row the pairs are (0,4), (1,3), (2,2): every cell gets +1,
        // and the middle cell gets +1 again because `lo` and `hi` land on
        // it together on the last iteration. So 5 + 1 = 6 per row, and
        // 6 * 7 = 42 overall.
        &["42"],
        "converging_two_pointer_bce_skip",
    );
}

#[test]
fn asan_diverging_type_param_name_generic_struct_param() {
    // B-2026-08-07-18. The whole trigger is a COSMETIC choice: naming a
    // generic fn's type param differently from the struct's. `fn t[U](x:
    // Mix[U])` corrupted where the character-for-character identical `fn
    // t[T](x: Mix[T])` was clean, at the DEFAULT -O2.
    //
    // `mono_struct_type_from_active_subst` resolves the STRUCT's fields
    // through the FN's substitution, so the two agree only by name
    // coincidence. On the diverging spelling the entry-copy arm declines,
    // own-by-transfer takes it, and its drop was keyed by BARE NAME inside
    // the monomorph — synthesized against the ERASED base layout. For
    // `Mix[T] { v: T, s: String }` field 1 sits at byte 8 erased and byte
    // 24 mono, so the drop read field 0's LENGTH WORD and freed it (the
    // reported bad address was `0x4` for a 4-char payload — the length, not
    // a pointer).
    //
    // Both halves are pinned here because the fix needed both:
    //
    //   * the FRESH-TEMP arg, where the callee is the only owner. Every
    //     shape below leaked or corrupted before; a struct with no concrete
    //     heap sibling (`Pair[T] { a: T, b: T }`) leaked instead of
    //     corrupting, because there was no concrete field for the mis-GEP'd
    //     drop to reach.
    //   * the NAMED binding, which is the regression direction. Correcting
    //     the drop's layout made it REAL, and the generic call path never
    //     retracted the caller's drop for an identifier argument the way
    //     `compile_call` does — so the fix's first cut turned this row's
    //     leak into a fresh double free in shapes that had been clean.
    //     `C`/`D` below are exactly those: clean before, double-freeing
    //     mid-fix, clean now.
    //
    // The matching-name spellings ride along as controls: they take the
    // entry-copy arm instead and their IR must not move.
    assert_clean_asan_run(
        r#"
struct Mix[T] { v: T, s: String }
struct MixC[T] { v: T, s: String }
struct Scal[T] { v: T, n: i64 }
struct Lone[T] { v: T }
struct Pr[T] { a: T, b: T }
struct PrC[T] { a: T, b: T }

fn diverge[U](x: Mix[U]) -> i64 { 1 }
fn same[T](x: MixC[T]) -> i64 { 1 }
fn scal[U](x: Scal[U]) -> i64 { 1 }
fn lone[U](x: Lone[U]) -> i64 { 1 }
fn pair[U](x: Pr[U]) -> i64 { 1 }
fn pair_same[T](x: PrC[T]) -> i64 { 1 }

fn main() {
    // Runtime-derived payload: a constant-folded one is a dead allocation the
    // optimizer deletes, and the fixture then passes vacuously against the
    // unfixed compiler (B-2026-08-04-17).
    let n: i64 = env.args().len();
    let mut i: i64 = 0;
    while i < 3 {
        // Fresh temps — the callee is the sole owner.
        println(diverge(Mix { v: "diverging-payload-past-inline".repeat(n), s: "sibling-payload-past-inline".repeat(n) }));
        println(same(MixC { v: "control-payload-past-inline".repeat(n), s: "sibling-payload-past-inline".repeat(n) }));
        println(pair(Pr { a: "bare-a-payload-past-inline".repeat(n), b: "bare-b-payload-past-inline".repeat(n) }));
        println(pair_same(PrC { a: "bare-a-payload-past-inline".repeat(n), b: "bare-b-payload-past-inline".repeat(n) }));
        // Named bindings — the caller owns until it retracts.
        let m = Mix { v: "named-payload-past-inline".repeat(n), s: "named-sibling-past-inline".repeat(n) };
        println(diverge(m));
        let sc = Scal { v: "scalar-sibling-payload-past-inline".repeat(n), n: i };
        println(scal(sc));
        let lo = Lone { v: "lone-bare-payload-past-inline".repeat(n) };
        println(lone(lo));
        let pr = Pr { a: "named-a-payload-past-inline".repeat(n), b: "named-b-payload-past-inline".repeat(n) };
        println(pair(pr));
        i = i + 1;
    }
}
"#,
        &[
            "1", "1", "1", "1", "1", "1", "1", "1", "1", "1", "1", "1", "1", "1", "1", "1", "1",
            "1", "1", "1", "1", "1", "1", "1",
        ],
        "diverging_type_param_name_struct_param",
    );
}

/// B-2026-08-13-14 — a heap field BOUND OUT of a local that is read again
/// (`let t = a.lines; … a.lines[0]`) gets an independent buffer, so the
/// source's own `{ptr,len}` no longer dangles.
///
/// The ownership pass already reports this as `UseAfterMove`, and that
/// diagnostic is advisory on the documented promise that "codegen
/// defensive-copies the reuse, so the binary is memory-safe"
/// (B-2026-08-10-21). The promise was false at a FIELD-ACCESS consume site:
/// `uam_defensive_copy` matched only `ExprKind::Identifier`, so the bind
/// copied nothing while the source disarm ran anyway.
///
/// The realloc is what makes it a use-after-free rather than a stale read.
/// A depth-1 Vec field bind zeroes only the source's `cap`, so the source
/// keeps a live `{ptr,len}` into the buffer the new binding owns; growing
/// that binding past its capacity hands the allocation to
/// `karac_realloc_or_panic`, which frees it. Under the unfixed compiler
/// this reports `heap-use-after-free` on `a.lines[0]` — valgrind names it
/// `Invalid read of size 8 … free'd by realloc`. The loop must run enough
/// iterations to force a MOVING realloc: at one push the allocator grew the
/// block in place and the program was accidentally clean, which is exactly
/// how this stayed invisible.
///
/// The nested-struct sibling in the same program covers the other half. It
/// routes through `zero_struct_move_caps_mono`, which zeroes `len` as well
/// as `cap` (it must — B-2026-07-10-1), so its failure mode was silent data
/// LOSS rather than a dangling read: `b.a.lines` read back empty. Both are
/// one missing copy.
#[test]
fn asan_field_bound_out_of_local_then_reread_is_copied() {
    assert_clean_asan_run(
        r#"
struct A { lines: Vec[String] }
struct B { a: A }
struct S { name: String }
fn main() {
    let k = env.args().len() as i64;
    let mut s = String.new(); s.push_str("alpha"); s.push_str(k.to_string());
    let mut v: Vec[String] = Vec.new(); v.push(s);
    let a = A { lines: v };
    let mut t = a.lines;
    let mut i = 0i64;
    while i < 2000 {
        let mut q = String.new(); q.push_str("y"); q.push_str(k.to_string());
        t.push(q);
        i = i + 1;
    }
    let mut acc = t.len() as i64;
    acc = acc + a.lines.len() as i64;
    acc = acc + a.lines[0].len();

    let mut s2 = String.new(); s2.push_str("beta"); s2.push_str(k.to_string());
    let mut v2: Vec[String] = Vec.new(); v2.push(s2);
    let b = B { a: A { lines: v2 } };
    let u = b.a;
    acc = acc + u.lines.len() as i64;
    let w = b.a;
    acc = acc + w.lines.len() as i64;
    acc = acc + w.lines[0].len();

    let mut s3 = String.new(); s3.push_str("gamma"); s3.push_str(k.to_string());
    let st = S { name: s3 };
    let n1 = st.name;
    let n2 = st.name;
    acc = acc + n1.len() + n2.len();
    println(acc);
}
"#,
        // 2001 (`t`) + 1 + 6 (`a.lines`, which the unfixed build read
        // through a dangling pointer) + 1 + 1 + 5 (`b.a` twice, which the
        // unfixed build read back EMPTY) + 12 (`st.name` twice, likewise).
        // `k` is a stable 1 — the binary runs with no args — so the string
        // lengths are fixed; it exists only to defeat literal folding,
        // since a string LITERAL is static with `cap == 0` and hides every
        // move-out failure in this family.
        &["2027"],
        "field_bound_out_of_local_then_reread_is_copied",
    );
}

/// B-2026-08-13-19 — the TUPLE-ELEMENT sibling of the fixture above.
///
/// Same advisory-`UseAfterMove` promise, same reach gap, one place-
/// expression spelling over: `uam_defensive_copy` grew a `FieldAccess` arm
/// and still had none for `ExprKind::TupleIndex`, so `let r = t.0` copied
/// nothing while `suppress_tuple_index_move_source` zeroed the source's
/// element anyway.
///
/// TWO HALVES had to land together, which is why this fixture exists rather
/// than only an output test. The copy alone left the source zeroed and the
/// output still wrong; the disarm-skip alone would have left two owners of
/// one buffer. Both key on `uam_copied_sites` — the set of sites where a
/// copy REALLY happened — so they cannot drift apart. A leak or a double
/// free is exactly what a mistake in that pairing looks like, and only a
/// sanitizer run sees it: the value assertion below passes either way as
/// long as the copy fires.
///
/// The realloc loop is here for the same reason as the sibling's — a Vec
/// element whose source keeps a live `{ptr,len}` only dangles once the
/// buffer actually MOVES, and one push often grows in place.
#[test]
fn asan_tuple_elem_bound_out_of_local_then_reread_is_copied() {
    assert_clean_asan_run(
        r#"
struct A { mut lines: Vec[String] }
struct H { mut pe: (A, i64) }
fn main() {
    let k = env.args().len() as i64;
    let mut s = String.new(); s.push_str("alpha"); s.push_str(k.to_string());
    let mut v: Vec[String] = Vec.new(); v.push(s);
    let seed = A { lines: v };
    let t: (A, i64) = (seed, 1);
    let mut r = t.0;
    let mut i = 0i64;
    while i < 2000 {
        let mut q = String.new(); q.push_str("y"); q.push_str(k.to_string());
        r.lines.push(q);
        i = i + 1;
    }
    let mut acc = r.lines.len() as i64;
    let chk = t.0;
    acc = acc + chk.lines.len() as i64;
    acc = acc + chk.lines[0].len();

    let mut v2: Vec[i64] = Vec.new(); v2.push(k); v2.push(k + 1);
    let t2: (Vec[i64], i64) = (v2, 2);
    let mut r2 = t2.0;
    r2.push(9);
    acc = acc + r2.len() as i64;
    let chk2 = t2.0;
    acc = acc + chk2.len() as i64;

    let mut s3 = String.new(); s3.push_str("gamma"); s3.push_str(k.to_string());
    let mut v3: Vec[String] = Vec.new(); v3.push(s3);
    let h = H { pe: (A { lines: v3 }, 3) };
    let mut r3 = h.pe.0;
    r3.lines.push(f"w");
    acc = acc + r3.lines.len() as i64;
    let chk3 = h.pe.0;
    acc = acc + chk3.lines.len() as i64;
    acc = acc + chk3.lines[0].len();
    println(acc);
}
"#,
        // 2001 + 1 + 6 ("alpha1")  = 2008   struct element, Vec[String] field
        //  + 3 + 2                  = 2013   direct Vec element
        //  + 2 + 1 + 6 ("gamma1")   = 2022   struct element under a field
        // `k` is a stable 1 (the binary runs with no args); it exists only
        // to defeat literal folding, since a string LITERAL is static with
        // `cap == 0` and hides every move-out failure in this family.
        //
        // The middle leg is `Vec[i64]` rather than `Vec[String]` on
        // purpose: moving a HEAP-ELEMENT Vec out of a tuple SEGFAULTS on
        // its own, with no reuse and no diagnostic — filed separately, and
        // pre-existing (it crashes identically on the pre-fix compiler).
        // Building this fixture on top of that crash would make it assert
        // nothing about the copy it exists to pin.
        &["2022"],
        "tuple_elem_bound_out_of_local_then_reread_is_copied",
    );
}

#[test]
fn asan_let_bound_container_element_field_box_has_one_owner() {
    // B-2026-08-18-15 — `let c = v[i].opt;` over a BOXED payload
    // double-freed the 32-byte envelope: an element read is a copy, the
    // container still owns the box, and this binding registered a second
    // `BoxedEnumDrop` over the same pointer. It aborted with `free():
    // double free detected in tcache 2` on a DEFAULT `karac build` — no
    // sanitizer required — so this fixture pins a crash, not a leak.
    //
    // THE PAYLOAD IS FOUR PLAIN `i64`s AND NO STRING, deliberately. Four
    // words is one past the 3-word inline area, so the payload boxes while
    // owning no interior heap of its own — which isolates the envelope as
    // the thing freed twice. The same program with a 1-word or a
    // String-only (3-word, fitting) payload was always clean, and that
    // contrast is what identified boxing as the variable.
    //
    // The SECOND read of the same element is the other half: it proves the
    // element survives the binding, i.e. the semantics are a copy and the
    // container is the rightful sole owner. A fix that suppressed the
    // container instead of narrowing this registration would pass the
    // double-free check and corrupt this line.
    assert_clean_asan_run(
        r#"
struct City { a: i64, b: i64, c: i64, d: i64 }
struct Holder { city: Option[City] }

fn main() {
    let mut v: Vec[Holder] = Vec.new();
    let mut i = 0;
    while i < 5 {
        v.push(Holder { city: Some(City { a: i, b: i, c: i, d: i }) });
        i = i + 1;
    }
    let mut total = 0;
    let mut k = 0;
    while k < 5 {
        let c2 = v[k].city;
        match c2 { Some(c) => { total = total + c.a; } None => { total = total + 100; } }
        match v[k].city { Some(c) => { total = total + c.a; } None => { total = total + 1000; } }
        k = k + 1;
    }
    println(total);
}
"#,
        &["20"],
        "let_bound_container_element_field_box_has_one_owner",
    );
}

#[test]
fn asan_let_bound_element_field_box_through_a_branch_and_a_field_root() {
    // B-2026-08-18-15's two reaching shapes, which the direct spelling
    // above does not cover.
    //
    // `b.items[k].city` is FIELD-ROOTED — the self-hosted parser's
    // `self.tokens[self.pos].token`, and the reason this row is not an
    // exotic-shape row.
    //
    // The `match` binding is the branching one: the RHS is not a place at
    // all, so the element projection sits in an ARM TAIL. Exactly one arm
    // runs but the binding registers once for both, so one projecting tail
    // is enough to make the registration a second owner on that path. This
    // spelling SEGV'd where the direct one aborted, which is the same
    // defect landing on a different pointer.
    assert_clean_asan_run(
        r#"
struct City { a: i64, b: i64, c: i64, d: i64 }
struct Holder { city: Option[City] }
struct Bag { items: Vec[Holder] }

fn main() {
    let mut b = Bag { items: Vec.new() };
    let mut i = 0;
    while i < 6 {
        b.items.push(Holder { city: Some(City { a: i, b: i, c: i, d: i }) });
        i = i + 1;
    }
    let mut total = 0;
    let mut k = 0;
    while k < 6 {
        let viaroot = b.items[k].city;
        match viaroot { Some(c) => { total = total + c.a; } None => { total = total + 100; } }
        let viabranch = match k % 2 { 0 => { b.items[k].city } _ => { None } };
        match viabranch { Some(c) => { total = total + c.b; } None => { total = total + 1; } }
        k = k + 1;
    }
    println(total);
}
"#,
        &["24"],
        "let_bound_element_field_box_through_a_branch_and_a_field_root",
    );
}

#[test]
fn asan_descending_loop_bce_skip_in_bounds() {
    // B-2026-07-17-1: the descending-loop bounds-check skip
    // (bce_length_pin.rs `compute_descending_skips`) elides the upper-half
    // check on `row[k]` / `row[k-1]` inside `while k >= 1 { .. k = k - 1 }`,
    // where `k` inits to `i - 1` under an enclosing `while i <= n` and `row`
    // is pinned to `n + 1` by an inclusive fill. If the transitive
    // `k <= i-1 <= n-1 < n+1 == row.len()` proof were wrong, an eliminated
    // check would be an out-of-bounds read/write ASAN catches here. The
    // canonical LeetCode #119 in-place rolling Pascal row, run to `n = 30`.
    assert_clean_asan_run(
        r#"
fn get_row(row_index: i64) -> Vec[i64] {
    let mut row: Vec[i64] = Vec.new();
    let mut j = 0i64;
    while j <= row_index { row.push(1i64); j = j + 1i64; }
    let mut i = 2i64;
    while i <= row_index {
        let mut k = i - 1i64;
        while k >= 1i64 { row[k] = row[k] + row[k - 1i64]; k = k - 1i64; }
        i = i + 1;
    }
    row
}
fn main() {
    let row = get_row(30i64);
    let mut sum = 0i64;
    let mut j = 0i64;
    while j < row.len() { sum = sum + row[j]; j = j + 1i64; }
    println(sum);
}
"#,
        // Row 30 of Pascal's triangle sums to 2^30 = 1073741824.
        &["1073741824"],
        "descending_loop_bce_skip_in_bounds",
    );
}

/// B-2026-08-27-50 — the element-type half of the same family, under ASAN.
///
/// A generic callee's container param bound to a struct-field argument left
/// `T` at the `i64` unknown-name default, so a `Vec[String]` element was
/// swapped 8 bytes at a time through a 24-byte control block. The
/// user-visible symptom was a SEGFAULT, but the underlying operation is a
/// partial overwrite of a live heap pointer, so it belongs here as well as
/// in the E2E suite — a shape that merely corrupted without crashing would
/// pass there and fail here.
#[test]
fn asan_container_param_bound_to_a_struct_field_argument() {
    for (label, prog, want) in [
        (
            "field-arg-string-element",
            r#"
struct Bag[=T] { xs: Vec[T] }
fn swap01[T](v: mut ref Vec[T]) { v.swap(0, 1); }
impl[T] Bag[T] {
    fn go(mut ref self) { swap01(mut self.xs); }
    fn at(ref self, i: i64) -> T { return self.xs[i]; }
}
fn main() {
    let mut a: Bag[String] = Bag { xs: Vec.new() };
    a.xs.push("aa"); a.xs.push("bb"); a.go();
    println(f"[{a.at(0)} {a.at(1)}]");
}
"#,
            "[bb aa]",
        ),
        (
            "field-arg-heap-bearing-tuple-element",
            r#"
struct Bag[=T] { xs: Vec[T] }
fn swap01[T](v: mut ref Vec[T]) { v.swap(0, 1); }
impl[T] Bag[T] {
    fn go(mut ref self) { swap01(mut self.xs); }
    fn at(ref self, i: i64) -> T { return self.xs[i]; }
}
fn main() {
    let mut a: Bag[(String, i64)] = Bag { xs: Vec.new() };
    a.xs.push(("aa", 1)); a.xs.push(("bb", 2)); a.go();
    let z0 = a.at(0); let z1 = a.at(1);
    println(f"[{z0.0}:{z0.1} {z1.0}:{z1.1}]");
}
"#,
            "[bb:2 aa:1]",
        ),
        (
            "field-arg-returning-callee",
            r#"
struct Bag[=T] { xs: Vec[T] }
fn first[T](v: ref Vec[T]) -> T { return v[0]; }
impl[T] Bag[T] {
    fn head(ref self) -> T { return first(self.xs); }
}
fn main() {
    let mut a: Bag[String] = Bag { xs: Vec.new() };
    a.xs.push("aa"); a.xs.push("bb");
    println(f"[{a.head()}]");
}
"#,
            "[aa]",
        ),
    ] {
        assert_clean_asan_run(prog, &[want], label);
    }
}

/// B-2026-08-28-71 — the MEMORY half of the GENERIC leg of the same fix.
///
/// The monomorph is compiled through `compile_mono_function`, which has its
/// own param loop, so a generic callee kept B-2026-08-28-22's original
/// defect (a MISSED body) until that loop gained the same registration. The
/// registration is BODIES-ONLY there for exactly the reason the non-generic
/// twin above states, and the mono adds one hazard of its own:
/// `make_aggregate_param_callee_owned_inst` gives a mono's owned aggregate
/// param an ENTRY DEEP-COPY, so callee and caller hold independent buffers
/// and a wrapper that freed fields here would be freeing the copy while the
/// body ran on it. Every struct carries a `String` so that any such
/// mismatch is a real double free rather than a silent no-op.
///
/// Both directions of every branch, on all three callee shapes the mono
/// path can take: a generic free function (`gcond`), a generic function
/// whose branch is a `match` (`gmatch`, additionally instantiated at
/// `U = String` so the type argument itself is heap-carrying), and a
/// GENERIC METHOD (`T.pick`) — the one whose caller stands down through
/// `compile_generic_call` rather than `call_arg_flows_into_return`, since
/// that union scan is `Item::Function`-only and answers `false` for a
/// `Type.method` key.
///
/// Param names are DISTINCT across the three callees (`ra`/`rb`/`rc`) for
/// the B-2026-08-29-11 reason the `return`-spelling fixture below records:
/// the interpreter shares one moved-out name space across frames, so a
/// reused name lets an earlier callee's move-out suppress a later one's
/// body. That is a different row's defect and naming around it keeps this
/// fixture measuring this one.
#[test]
fn asan_generic_conditionally_returned_param_bodies_are_memory_balanced() {
    assert_clean_asan_run(
        r#"
struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"drop {self.name}"); } }
struct T { n: i64 }
impl T {
    fn pick[U](ref self, ra: R, k: bool, u: U) -> R { if k { ra } else { R { id: 99, name: f"n99" } } }
}
fn gcond[U](rb: R, k: bool, u: U) -> R { if k { rb } else { R { id: 98, name: f"n98" } } }
fn gmatch[U](rc: R, k: i64, u: U) -> R { match k { 1 => { rc } _ => { R { id: 97, name: f"n97" } } } }
fn main() {
    let base: i64 = env.args().len();
    let a = gcond(R { id: base, name: f"a{base}" }, false, 7);
    println(f"{a.id}");
    let b = gcond(R { id: base, name: f"b{base}" }, true, 7);
    println(f"{b.id}");
    let c = gmatch(R { id: base, name: f"c{base}" }, 2, f"s");
    println(f"{c.id}");
    let d = gmatch(R { id: base, name: f"d{base}" }, 1, f"s");
    println(f"{d.id}");
    let t = T { n: 1 };
    let e = t.pick(R { id: base, name: f"e{base}" }, false, 7);
    println(f"{e.id}");
    let g = t.pick(R { id: base, name: f"g{base}" }, true, 7);
    println(f"{g.id}");
    println("end");
}
"#,
        &[
            "drop a1", "98", "drop n98", "1", "drop b1", "drop c1", "97", "drop n97", "1",
            "drop d1", "drop e1", "99", "drop n99", "1", "drop g1", "end",
        ],
        "generic-conditionally-returned-param-bodies",
    );
}

/// B-2026-09-20-52 — the MEMORY twin of `tests/codegen.rs`'s
/// `e2e_concrete_holder_and_non_generic_handback_leave_one_owner_per_box`,
/// which asserts stdout only. Read that fixture's note for the two faults
/// and why each is invisible in the shape that contains both.
///
/// SPLIT INTO FOUR PROGRAMS RATHER THAN ONE, because the faults cancel. A
/// single program carrying every cell was BALANCED overall while two of its
/// cells were a leak and a double free, so a whole-program alloc/free delta
/// is the one instrument that cannot see this row. Each block below is its
/// own binary and its own verdict.
///
/// The `wrap` block is the pair that pulls in opposite directions:
/// `wrapbind` double-frees if the holder is fixed alone, and `wrapnone`
/// strands its box if the caller is disarmed on the callee's signature
/// instead of on the value that came back. Both spellings of the obvious
/// fix were measured and rejected by exactly these two cells.
#[test]
fn asan_concrete_holder_and_non_generic_handback_leave_one_owner_per_box() {
    const DECLS: &str = "enum G1[T] { Y(T), N }\n\
             enum N1 { Y(String), N }\n\
             struct Conc { g: G1[String] }\n\
             fn wrapC(g: G1[String], c: bool) -> Conc { if c { return Conc { g: g } } return Conc { g: G1.N } }\n\
             fn ident(g: G1[String]) -> G1[String] { return g }\n\
             fn other(g: G1[String]) -> G1[String] { return G1.Y(\"zzzzzzzzzzzzzzzzzzzzzzzz\") }\n\
             fn pick(a: G1[String], b: G1[String]) -> G1[String] { return a }\n\
             fn usz(g: G1[String]) -> i64 { match g { G1.Y(v) => { return v.len() } G1.N => { return 0 } } }\n\
             fn sink(g: G1[String]) { match g { G1.Y(v) => { println(f\"  s{v.len()}\") } G1.N => { println(\"  s0\") } } }\n\
             fn shw(g: G1[String]) { match g { G1.Y(v) => { println(f\"  mx {v.len()}\") } G1.N => { println(\"  mx 0\") } } }\n\
             fn identn(g: N1) -> N1 { return g }\n\
             fn shwn(g: N1) { match g { N1.Y(v) => { println(f\"  nx {v.len()}\") } N1.N => { println(\"  nx 0\") } } }\n";

    // FAULT L on its own: a concrete holder whose field is never
    // handed onward. 9 allocs / 8 frees, 24 B definitely lost.
    assert_clean_asan_run(
            &format!(
                "{DECLS}\
                 fn main() {{\n\
                 \x20   {{ let h: Conc = Conc {{ g: G1.Y(\"abcdefghijklmnopqrstuvwx\") }}; println(\"  x\") }}\n\
                 \x20   println(\"end\");\n\
                 }}\n"
            ),
            &["x", "end"],
            "b92052-holder",
        );

    // FAULT D on its own, with no struct anywhere: a non-generic callee
    // that returns its own by-value parameter. Both spellings were
    // 9 allocs / 10 frees with an Invalid read and an Invalid free.
    assert_clean_asan_run(
            &format!(
                "{DECLS}\
                 fn main() {{\n\
                 \x20   {{ let g: G1[String] = G1.Y(\"abcdefghijklmnopqrstuvwx\"); let k: G1[String] = ident(g); shw(k) }}\n\
                 \x20   {{ let g: G1[String] = G1.Y(\"abcdefghijklmnopqrstuvwx\"); ident(g); println(\"  x\") }}\n\
                 \x20   println(\"end\");\n\
                 }}\n"
            ),
            &["mx 24", "  x", "end"],
            "b92052-handback",
        );

    // THE SHAPE THAT CONTAINS BOTH, and the dies-inside leg beside it.
    // `wrapbind` was BALANCED before either fix and double-frees if the
    // holder is fixed alone; `wrapnone` strands its box if the caller is
    // disarmed on the callee's signature rather than on the returned
    // value. The two cells pull in opposite directions on purpose.
    assert_clean_asan_run(
            &format!(
                "{DECLS}\
                 fn main() {{\n\
                 \x20   {{ let g: G1[String] = G1.Y(\"abcdefghijklmnopqrstuvwx\"); let h = wrapC(g, true); shw(h.g) }}\n\
                 \x20   {{ let g: G1[String] = G1.Y(\"abcdefghijklmnopqrstuvwx\"); let h = wrapC(g, false); shw(h.g) }}\n\
                 \x20   {{ let g: G1[String] = G1.Y(\"abcdefghijklmnopqrstuvwx\"); wrapC(g, true); println(\"  x\") }}\n\
                 \x20   println(\"end\");\n\
                 }}\n"
            ),
            &["mx 24", "  mx 0", "  x", "end"],
            "b92052-wrap",
        );

    // THE STRANDING-DIRECTION GUARDS, every one of them clean before this
    // row and required to stay so: a field handed onward, a TEMPORARY
    // argument, a callee returning a fresh value, a scalar return, a unit
    // return, a two-argument callee where only the returned one may be
    // disarmed, and the non-generic enum twin.
    assert_clean_asan_run(
            &format!(
                "{DECLS}\
                 fn main() {{\n\
                 \x20   {{ let h: Conc = Conc {{ g: G1.Y(\"abcdefghijklmnopqrstuvwx\") }}; shw(h.g) }}\n\
                 \x20   {{ let k: G1[String] = ident(G1.Y(\"abcdefghijklmnopqrstuvwx\")); shw(k) }}\n\
                 \x20   {{ let g: G1[String] = G1.Y(\"abcdefghijklmnopqrstuvwx\"); let k: G1[String] = other(g); shw(k) }}\n\
                 \x20   {{ let g: G1[String] = G1.Y(\"abcdefghijklmnopqrstuvwx\"); let n: i64 = usz(g); println(f\"  n{{n}}\") }}\n\
                 \x20   {{ let g: G1[String] = G1.Y(\"abcdefghijklmnopqrstuvwx\"); sink(g) }}\n\
                 \x20   {{ let x: G1[String] = G1.Y(\"aaaaaaaaaaaaaaaaaaaaaaaa\"); let y: G1[String] = G1.Y(\"bbbbbbbbbbbbbbbbbbbbbbbbbbbb\"); let k: G1[String] = pick(x, y); shw(k) }}\n\
                 \x20   {{ let g: N1 = N1.Y(\"abcdefghijklmnopqrstuvwx\"); let k: N1 = identn(g); shwn(k) }}\n\
                 \x20   println(\"end\");\n\
                 }}\n"
            ),
            &["mx 24", "  mx 24", "  mx 24", "  n24", "  s24", "  mx 24", "  nx 24", "end"],
            "b92052-guards",
        );
}

/// B-2026-09-20-46 — the memory twin of
/// `e2e_generic_callee_reaches_the_argument_site_copy_like_its_concrete_twin`,
/// which asserts stdout only. Read that fixture's note for the gap and why
/// the generic and concrete spellings disagreed.
///
/// SPLIT INTO THREE PROGRAMS rather than one, following
/// `asan_concrete_holder_and_non_generic_handback_leave_one_owner_per_box`:
/// a whole-program alloc/free delta is exactly the instrument that cannot
/// see a pair of faults pulling in opposite directions, so each block is
/// its own binary and its own verdict.
///
/// The first block is the row itself — before the fix it was an `Invalid
/// read of size 8` at `Address 0x0` with `definitely lost` at ZERO, which
/// is why a leak column alone reads it clean and the invalid-access column
/// is what catches it.
///
/// The second is the multi-argument cell. It is here because the first
/// attempt at this fix emitted an invalid module for it, and because a
/// per-argument copy that over-fires would strand a box that no
/// single-argument cell can expose.
///
/// The third is the two guards for the copy NOT firing: no reuse, and a
/// non-boxing payload. Both were clean before the fix as well as after, so
/// they guard against over-firing rather than pinning a repair.
///
/// The generic METHOD receiver is deliberately absent: this fix moves it
/// from a double free to a 24 B leak, which is an improvement and not a
/// clean cell, and it has its own row.
#[test]
fn asan_generic_callee_reaches_the_argument_site_copy_like_its_concrete_twin() {
    const DECLS: &str = "enum G1[T] { Y(T), N }\n\
             fn shg[T](g: G1[T]) { match g { G1.Y(v) => { println(f\"  mx {v}\") } G1.N => { println(\"  mx NONE\") } } }\n\
             fn shc(g: G1[String]) { match g { G1.Y(v) => { println(f\"  cx {v}\") } G1.N => { println(\"  cx NONE\") } } }\n\
             fn two[T](a: G1[T], b: G1[T]) { match a { G1.Y(v) => { println(f\"  ax {v}\") } G1.N => { println(\"  ax NONE\") } } match b { G1.Y(v) => { println(f\"  bx {v}\") } G1.N => { println(\"  bx NONE\") } } }\n";

    // THE ROW: a reused by-value generic enum argument, beside the
    // concrete twin it must now agree with. Pre-fix the generic half was
    // an Invalid read of a null box with `definitely lost` at 0 B.
    assert_clean_asan_run(
            &format!(
                "{DECLS}\
                 fn main() {{\n\
                 \x20   {{ let g: G1[String] = G1.Y(\"abcdefghijklmnopqrstuvwx\"); shg(g); shg(g) }}\n\
                 \x20   {{ let g: G1[String] = G1.Y(\"abcdefghijklmnopqrstuvwx\"); shc(g); shc(g) }}\n\
                 \x20   println(\"end\");\n\
                 }}\n"
            ),
            &[
                "mx abcdefghijklmnopqrstuvwx",
                "  mx abcdefghijklmnopqrstuvwx",
                "  cx abcdefghijklmnopqrstuvwx",
                "  cx abcdefghijklmnopqrstuvwx",
                "end",
            ],
            "b92046-gen-vs-con",
        );

    // MULTI-ARGUMENT, two DISTINCT reused bindings. The callee's body is
    // two `match` STATEMENTS, which is what reaches the restore drain that
    // a single-`match`-expression body never does.
    assert_clean_asan_run(
            &format!(
                "{DECLS}\
                 fn main() {{\n\
                 \x20   {{ let x: G1[String] = G1.Y(\"aaaaaaaaaaaaaaaaaaaaaaaa\"); let y: G1[String] = G1.Y(\"bbbbbbbbbbbbbbbbbbbbbbbb\"); two(x, y); shg(x); shg(y) }}\n\
                 \x20   println(\"end\");\n\
                 }}\n"
            ),
            &[
                "ax aaaaaaaaaaaaaaaaaaaaaaaa",
                "  bx bbbbbbbbbbbbbbbbbbbbbbbb",
                "  mx aaaaaaaaaaaaaaaaaaaaaaaa",
                "  mx bbbbbbbbbbbbbbbbbbbbbbbb",
                "end",
            ],
            "b92046-twodist",
        );

    // THE COPY MUST NOT FIRE: no reuse, and a non-boxing payload.
    assert_clean_asan_run(
        &format!(
            "{DECLS}\
                 fn main() {{\n\
                 \x20   {{ let g: G1[String] = G1.Y(\"abcdefghijklmnopqrstuvwx\"); shg(g) }}\n\
                 \x20   {{ let g: G1[i32] = G1.Y(24); shg(g); shg(g) }}\n\
                 \x20   {{ let g: G1[String] = G1.N; shg(g); shg(g) }}\n\
                 \x20   println(\"end\");\n\
                 }}\n"
        ),
        &[
            "mx abcdefghijklmnopqrstuvwx",
            "  mx 24",
            "  mx 24",
            "  mx NONE",
            "  mx NONE",
            "end",
        ],
        "b92046-no-copy",
    );
}

/// B-2026-09-25-15 — a field read IN PLACE on a call returning a generic
/// struct: `wrap(f"..").k` over `fn wrap[T](x: T) -> W[T]`. The fresh temp's
/// drop was registered by the bare name `W`, which frees no `T`-typed field,
/// so the payload leaked on every compiled surface; `let w = wrap(..)` was
/// clean.
#[test]
fn asan_field_read_on_generic_struct_call_result_frees_payload() {
    assert_clean_asan_run_min_allocs(
        r#"struct P { s: String, n: i64 }
struct W[T] { v: T, k: i64 }
struct Two[A, B] { a: A, b: B, k: i64 }
fn wrap[T](x: T) -> W[T] { W { v: x, k: 3 } }
fn two[A, B](a: A, b: B) -> Two[A, B] { Two { a: a, b: b, k: 4 } }
fn mkw(i: i64) -> W[String] { W { v: f"heap-string-longer-than-sso-w{i}", k: i } }
fn main() {
    for i in 0..2 {
        println(wrap(f"heap-string-longer-than-sso-{i}").k);
        println(wrap(f"heap-string-longer-than-sso-v{i}").v);
        let s = f"heap-string-longer-than-sso-n{i}";
        println(wrap(s).v.len());
        println(wrap(P { s: f"heap-string-longer-than-sso-p{i}", n: 5 }).v.n);
        let mut xs: Vec[String] = Vec.new();
        xs.push(f"heap-string-longer-than-sso-x{i}");
        println(wrap(xs).v.len());
        println(two(f"heap-string-longer-than-sso-a{i}", f"heap-string-longer-than-sso-b{i}").k);
        println(mkw(i).k);
        println(wrap(7).v);
    }
    println("end")
}
"#,
        &[
            "3",
            "heap-string-longer-than-sso-v0",
            "30",
            "5",
            "1",
            "4",
            "0",
            "7",
            "3",
            "heap-string-longer-than-sso-v1",
            "30",
            "5",
            "1",
            "4",
            "1",
            "7",
            "end",
        ],
        "asan_field_read_on_generic_struct_call_result_frees_payload",
        20,
    );
}
