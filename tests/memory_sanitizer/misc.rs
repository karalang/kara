//! everything the area rules do not claim -- fixtures for `tests/memory_sanitizer.rs`.
//!
//! Split out of `tests/memory_sanitizer.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test memory_sanitizer` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test memory_sanitizer misc::
//!
//! New fixtures about everything the area rules do not claim belong in this file.

use super::*;

/// B-2026-09-02-1 — the element `Drop` walk must not read a buffer the same
/// drain already freed.
///
/// The `let` path registered the `StructFieldBodies` walk BEFORE the
/// struct's memory drop, and the scope-exit drain runs a frame LIFO, so the
/// free drained one step ahead of the walk that reads through it. The body
/// then printed a different garbage id on every run.
///
/// WHAT ACTUALLY CATCHES IT HERE IS THE TRANSCRIPT, NOT THE SANITIZER, and
/// that is worth stating because the opposite is the natural assumption for
/// a use-after-free. Measured on the pre-fix compiler: VALGRIND reports it
/// (`Invalid read of size 8`, 1 error from 1 context, the `free` being the
/// preceding instruction in the same drain), but this ASAN harness does NOT
/// flag it -- the pre-fix run fails on `stdout mismatch` with a garbage
/// `dR64`, having exited ASAN-clean. So for THIS defect the expected-id
/// assertion is the oracle and the sanitizer is not.
///
/// It still earns its place: it is the only case exercising this shape in
/// the auto-par column under a sanitizer, so if the free and the read ever
/// land in a form ASAN does see, it fails here too. Do not read a green run
/// of this case alone as evidence that the ordering is sound -- the
/// transcript is what makes it evidence.
///
/// TWO CASES, because B-2026-08-31-6's fix changed which one reaches the
/// drain. This used to say "`_auto_par` is load-bearing: the sequential
/// harness cannot reach the shape at all. Without outlining the bodies
/// action fires at its NLL point and never enters the drain, so the two
/// halves never meet." That was true, and it stopped being true: a group no
/// longer swallows the firing point of a statement it covers, so under
/// auto-par the walk ALSO fires at the NLL point now, and case 1 --
/// unchanged except for its transcript, which moved from `a10 b1 done dR10`
/// to the interpreter's `a10 dR10 b1 done` -- no longer exercises the
/// drain at all.
///
/// Case 1 is kept anyway (it is the historical fixture, and a regression
/// that re-defers the walk fails it on the transcript), but on its own it
/// would leave B-2026-09-02-1 UNGUARDED. Case 2 restores the guard by
/// reaching the drain the one way that survives the fix: reading `a` in the
/// block's FINAL EXPRESSION pins its endpoint to `scope_exit`, which
/// `fire_due_user_drops` never matches (it is only ever called with an
/// index below the statement count), so the walk and the memory drop meet
/// in the LIFO drain exactly as before. Verified to reach it, on all four
/// surfaces: `a10 b1 done10 dR10` -- the body after the final expression,
/// with the correct id rather than a freed-block read.
/// B-2026-09-07-40 — the instrumentation's OWN positive control.
///
/// This suite links `-fsanitize=address` but, until this row, never
/// INSTRUMENTED the karac-emitted object. ASAN's memory-ACCESS checking is
/// a compiler pass; the link flag alone buys only the allocator
/// interposition (LeakSanitizer, double/invalid free, allocator-side
/// overflow). So every fixture here was blind to an invalid read or write,
/// and `memory_sanitizer` reported clean on a program that wrote three zero
/// words into a block it had just freed (B-2026-09-07-33).
///
/// `KARAC_SANITIZE_ADDRESS=1` turns the instrumentation on
/// (`codegen::driver::apply_address_sanitizer`), and this test is what keeps
/// that knob honest IN BOTH DIRECTIONS: with the knob off the object must
/// carry no `__asan_report*` reference, with it on it must. Without such a
/// check a knob that silently stopped working — an LLVM upgrade renaming the
/// `asan` pass, or the `sanitize_address` attribute failing to land, either
/// of which makes the pass return every function untouched — would leave the
/// instrumented leg reporting green over an uninstrumented suite. That is
/// the exact vacuous pass this row is about, one level up.
///
/// Deliberately NOT gated on `asan_available()`: that probes for a `cc` that
/// can LINK the sanitizer runtime, and instrumentation is a compile-side
/// property that holds on a host with no such `cc` at all.
#[test]
fn asan_instrumentation_tracks_the_sanitize_address_knob() {
    let src = "fn main() { let v = [1, 2, 3]; println(f\"n={v.len()}\") }";
    let mut parsed = karac::parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    karac::prepare_for_resolve(&mut parsed.program);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);
    let ownership = karac::ownershipcheck(&parsed.program, &typed);
    super::common::assert_check_clean(&resolved, &typed, src);

    let obj_path = format!("/tmp/karac_asan_selfcheck_{}.o", std::process::id());
    compile_to_object(&parsed.program, &obj_path, Some(&ownership), None)
        .expect("the self-check program must compile");
    let bytes = std::fs::read(&obj_path).expect("emitted object must be readable");
    let _ = std::fs::remove_file(&obj_path);

    let needle = b"__asan_report";
    let instrumented = bytes.windows(needle.len()).any(|w| w == needle);
    let requested = !matches!(
        std::env::var("KARAC_SANITIZE_ADDRESS").as_deref(),
        Err(_) | Ok("0") | Ok("")
    );
    assert_eq!(
        instrumented,
        requested,
        "KARAC_SANITIZE_ADDRESS requested={requested} but the emitted object \
             {} an `__asan_report*` reference. With the knob ON and no reference, \
             every access-checking fixture in this suite is vacuous — the `asan` \
             pass ran over functions that carry no `sanitize_address` attribute, \
             or the pass name changed under this LLVM. With the knob OFF and a \
             reference present, the default leg pays for instrumentation it never \
             asked for and will fail to link.",
        if instrumented {
            "carries"
        } else {
            "carries no"
        },
    );
}

/// B-2026-09-06-19 — the wrap-through-local hand-back and the
/// projected-field return, under ASAN + LSan: standing the caller down
/// and retracting the local's field walk leaves exactly one owner of the
/// `String` / `Vec` buffers on every path, frees nothing twice and leaks
/// nothing on the dies-inside paths.
#[test]
fn asan_param_wrapped_through_local_has_one_owner() {
    assert_clean_asan_run(
            "struct R { id: i64, tag: String, xs: Vec[i64] }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"  d{self.id}\") } }\n\
             fn mr(i: i64) -> R { return R { id: i, tag: f\"t{i}\", xs: [i] } }\n\
             struct P2 { r: R, n: i64 }\n\
             struct Box2 { r: R }\n\
             struct W { p: P2 }\n\
             struct K { n: i64 }\n\
             \n\
             fn ftwo(r: R, k: bool) -> Box2 { if k { return Box2 { r: mr(82) }; } let p = P2 { r: r, n: 1 }; return Box2 { r: p.r }; }\n\
             fn fone(r: R, k: bool) -> Box2 { if k { return Box2 { r: mr(83) }; } let p = Box2 { r: r }; return p; }\n\
             fn fproj(r: R, k: bool) -> R { if k { return mr(84); } let p = P2 { r: r, n: 1 }; return p.r; }\n\
             fn flet(r: R, k: bool) -> Box2 { if k { return Box2 { r: mr(85) }; } let p = P2 { r: r, n: 1 }; let q = p.r; return Box2 { r: q }; }\n\
             fn fnest(r: R, k: bool) -> W { if k { return W { p: P2 { r: mr(86), n: 0 } }; } let p = P2 { r: r, n: 1 }; let w = W { p: p }; return w; }\n\
             fn ftup(r: R, k: bool) -> Box2 { if k { return Box2 { r: mr(87) }; } let t = (r, 1); return Box2 { r: t.0 }; }\n\
             fn frebind(r: R, k: bool) -> P2 { if k { return P2 { r: mr(88), n: 0 }; } let p = P2 { r: r, n: 1 }; let q = p; return q; }\n\
             fn fearly(r: R, k: bool) -> Box2 { let p = P2 { r: r, n: 1 }; if k { return Box2 { r: mr(89) }; } return Box2 { r: p.r }; }\n\
             fn funcond(r: R) -> Box2 { let p = P2 { r: r, n: 1 }; return Box2 { r: p.r }; }\n\
             fn fpart(r: R, k: bool) -> i64 { let p = P2 { r: r, n: 1 }; if k { return 0; } return p.r.id; }\n\
             fn lproj() -> Box2 { let p = P2 { r: mr(19), n: 1 }; return Box2 { r: p.r }; }\n\
             fn lbare() -> R { let p = P2 { r: mr(20), n: 1 }; return p.r; }\n\
             fn ltail() -> Box2 { let p = P2 { r: mr(21), n: 1 }; Box2 { r: p.r } }\n\
             fn lopt() -> Option[R] { let p = P2 { r: mr(22), n: 1 }; return Option.Some(p.r); }\n\
             fn lnest() -> R { let w = W { p: P2 { r: mr(23), n: 1 } }; return w.p.r; }\n\
             impl K {\n\
             \x20   fn mlocal(self) -> Box2 { let p = P2 { r: mr(24), n: self.n }; return Box2 { r: p.r }; }\n\
             \x20   fn m(self, r: R, k: bool) -> Box2 { if k { return Box2 { r: mr(90) }; } let p = P2 { r: r, n: self.n }; return Box2 { r: p.r }; }\n\
             \x20   fn mref(ref self, r: R, k: bool) -> Box2 { if k { return Box2 { r: mr(91) }; } let p = P2 { r: r, n: self.n }; return Box2 { r: p.r }; }\n\
             \x20   fn muncond(self, r: R) -> Box2 { let p = P2 { r: r, n: self.n }; return Box2 { r: p.r }; }\n\
             }\n\
             \n\
             fn main() {\n\
             \x20   println(\"ftwo/t\"); let a1 = ftwo(mr(1), true); println(f\"  C{a1.r.id}\");\n\
             \x20   println(\"ftwo/f\"); let a2 = ftwo(mr(2), false); println(f\"  C{a2.r.id}\");\n\
             \x20   println(\"fone/f\"); let a3 = fone(mr(3), false); println(f\"  C{a3.r.id}\");\n\
             \x20   println(\"fproj/f\"); let a4 = fproj(mr(4), false); println(f\"  C{a4.id}\");\n\
             \x20   println(\"flet/f\"); let a5 = flet(mr(5), false); println(f\"  C{a5.r.id}\");\n\
             \x20   println(\"fnest/f\"); let a6 = fnest(mr(6), false); println(f\"  C{a6.p.r.id}\");\n\
             \x20   println(\"ftup/f\"); let a7 = ftup(mr(7), false); println(f\"  C{a7.r.id}\");\n\
             \x20   println(\"frebind/f\"); let a8 = frebind(mr(8), false); println(f\"  C{a8.r.id}\");\n\
             \x20   println(\"fearly/t\"); let a9 = fearly(mr(9), true); println(f\"  C{a9.r.id}\");\n\
             \x20   println(\"fearly/f\"); let a10 = fearly(mr(10), false); println(f\"  C{a10.r.id}\");\n\
             \x20   println(\"funcond\"); let a11 = funcond(mr(11)); println(f\"  C{a11.r.id}\");\n\
             \x20   println(\"funcond/named\"); let x12 = mr(12); let a12 = funcond(x12); println(f\"  C{a12.r.id}\");\n\
             \x20   println(\"fpart/t\"); let a13 = fpart(mr(13), true); println(f\"  C{a13}\");\n\
             \x20   println(\"fpart/f\"); let a14 = fpart(mr(14), false); println(f\"  C{a14}\");\n\
             \x20   println(\"m/t\"); let a15 = K { n: 1 }.m(mr(15), true); println(f\"  C{a15.r.id}\");\n\
             \x20   println(\"m/f\"); let a16 = K { n: 1 }.m(mr(16), false); println(f\"  C{a16.r.id}\");\n\
             \x20   println(\"mref/f\"); let kk = K { n: 2 }; let a17 = kk.mref(mr(17), false); println(f\"  C{a17.r.id}\");\n\
             \x20   println(\"muncond\"); let a18 = K { n: 3 }.muncond(mr(18)); println(f\"  C{a18.r.id}\");\n\
             \x20   println(\"lproj\"); let a19 = lproj(); println(f\"  C{a19.r.id}\");\n\
             \x20   println(\"lbare\"); let a20 = lbare(); println(f\"  C{a20.id}\");\n\
             \x20   println(\"ltail\"); let a21 = ltail(); println(f\"  C{a21.r.id}\");\n\
             \x20   println(\"lopt\"); if let Some(a22) = lopt() { println(f\"  C{a22.id}\"); }\n\
             \x20   println(\"lnest\"); let a23 = lnest(); println(f\"  C{a23.id}\");\n\
             \x20   println(\"mlocal\"); let a24 = K { n: 4 }.mlocal(); println(f\"  C{a24.r.id}\");\n\
             \x20   println(\"end\");\n\
             }\n",
            &[
                "ftwo/t",
                "  d1",
                "  C82",
                "  d82",
                "ftwo/f",
                "  C2",
                "  d2",
                "fone/f",
                "  C3",
                "  d3",
                "fproj/f",
                "  C4",
                "  d4",
                "flet/f",
                "  C5",
                "  d5",
                "fnest/f",
                "  C6",
                "  d6",
                "ftup/f",
                "  C7",
                "  d7",
                "frebind/f",
                "  C8",
                "  d8",
                "fearly/t",
                "  C89",
                "  d89",
                "fearly/f",
                "  C10",
                "  d10",
                "funcond",
                "  C11",
                "  d11",
                "funcond/named",
                "  C12",
                "  d12",
                "fpart/t",
                "  d13",
                "  C0",
                "fpart/f",
                "  d14",
                "  C14",
                "m/t",
                "  d15",
                "  C90",
                "  d90",
                "m/f",
                "  C16",
                "  d16",
                "mref/f",
                "  C17",
                "  d17",
                "muncond",
                "  C18",
                "  d18",
                "lproj",
                "  C19",
                "  d19",
                "lbare",
                "  C20",
                "  d20",
                "ltail",
                "  C21",
                "  d21",
                "lopt",
                "  C22",
                "  d22",
                "lnest",
                "  C23",
                "  d23",
                "mlocal",
                "  C24",
                "  d24",
                "end"
            ],
            "param_wrapped_through_local",
        );
}

/// B-2026-09-06-44 — the MEMORY half of `tests/codegen.rs`'s
/// `e2e_projection_off_a_param_runs_the_sibling_body_once` under ASAN +
/// LSan: with the `$keep` walk no longer minted for a caller-retained
/// param root, the sibling field's `String` / `Vec` buffers have exactly
/// one owner on every projection spelling, nothing freed twice, nothing
/// leaked.
#[test]
fn asan_projection_off_a_param_keeps_one_owner() {
    assert_clean_asan_run(
            "struct R { id: i64, name: String, xs: Vec[i64] }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"  dR{self.id}\") } }\n\
             fn mk(i: i64) -> R { return R { id: i, name: f\"n{i}\", xs: [i] }; }\n\
             struct S3 { a: R, b: R }\n\
             struct T { r: R, v: Vec[i64] }\n\
             \n\
             fn proj_none(s: S3) -> i64 { let a = s.a; println(\"  mid\"); return 1; }\n\
             fn proj_ret(s: S3) -> i64 { let a = s.a; println(\"  mid\"); return a.id; }\n\
             fn proj_b(s: S3) -> i64 { let b = s.b; println(\"  mid\"); return b.id; }\n\
             fn proj_both(s: S3) -> i64 { let a = s.a; let b = s.b; println(\"  mid\"); return a.id + b.id; }\n\
             fn proj_reads(s: S3) -> i64 { let a = s.a; println(f\"  m{a.id}\"); let b = s.b; println(f\"  m{b.id}\"); return 1; }\n\
             fn proj_whole(s: S3) -> R { let a = s.a; println(\"  mid\"); return a; }\n\
             fn proj_heap(t: T) -> i64 { let r = t.r; println(\"  mid\"); return r.id + t.v.len(); }\n\
             fn destr(s: S3) -> i64 { let S3 { a, b } = s; println(\"  mid\"); return a.id; }\n\
             impl S3 { fn m(self) -> i64 { let a = self.a; println(\"  mid\"); return a.id; } }\n\
             \n\
             fn main() {\n\
             \x20   println(\"proj_none\"); let v1 = proj_none(S3 { a: mk(1), b: mk(2) }); println(f\"  v={v1}\");\n\
             \x20   println(\"proj_ret\"); let v2 = proj_ret(S3 { a: mk(3), b: mk(4) }); println(f\"  v={v2}\");\n\
             \x20   println(\"proj_b\"); let v3 = proj_b(S3 { a: mk(5), b: mk(6) }); println(f\"  v={v3}\");\n\
             \x20   println(\"proj_both\"); let v4 = proj_both(S3 { a: mk(7), b: mk(8) }); println(f\"  v={v4}\");\n\
             \x20   println(\"proj_reads\"); let v5 = proj_reads(S3 { a: mk(9), b: mk(10) }); println(f\"  v={v5}\");\n\
             \x20   println(\"proj_whole\"); let r6 = proj_whole(S3 { a: mk(11), b: mk(12) }); println(f\"  v={r6.id}\");\n\
             \x20   println(\"proj_heap\"); let v7 = proj_heap(T { r: mk(13), v: [1, 2] }); println(f\"  v={v7}\");\n\
             \x20   println(\"self_root\"); let v8 = S3 { a: mk(14), b: mk(15) }.m(); println(f\"  v={v8}\");\n\
             \x20   println(\"named\"); let s9 = S3 { a: mk(16), b: mk(17) }; let v9 = proj_none(s9); println(f\"  v={v9}\");\n\
             \x20   println(\"destr\"); let v10 = destr(S3 { a: mk(18), b: mk(19) }); println(f\"  v={v10}\");\n\
             \x20   println(\"end\");\n\
             }\n",
            &[
                "proj_none",
                "  mid",
                "  dR2",
                "  dR1",
                "  v=1",
                "proj_ret",
                "  mid",
                "  dR4",
                "  dR3",
                "  v=3",
                "proj_b",
                "  mid",
                "  dR6",
                "  dR5",
                "  v=6",
                "proj_both",
                "  mid",
                "  dR8",
                "  dR7",
                "  v=15",
                "proj_reads",
                "  m9",
                "  m10",
                "  dR10",
                "  dR9",
                "  v=1",
                "proj_whole",
                "  mid",
                "  dR12",
                "  v=11",
                "  dR11",
                "proj_heap",
                "  mid",
                "  dR13",
                "  v=15",
                "self_root",
                "  mid",
                "  dR15",
                "  dR14",
                "  v=14",
                "named",
                "  mid",
                "  dR17",
                "  dR16",
                "  v=1",
                "destr",
                "  mid",
                "  dR19",
                "  dR18",
                "  v=18",
                "end"
            ],
            "projection_off_param_sibling_once",
        );
}

#[test]
fn asan_self_referential_by_value_param_keeps_todays_behaviour() {
    // CONTROL, and the boundary of B-2026-09-06-66's fix rather than a case
    // it closes. `self_referential_struct_sole_field_owner` carries the same
    // never-a-bare-by-value-param scope condition the shared-owning arm
    // does, so a type that reaches a call boundary at all keeps its leak
    // instead of gaining a double free (B-2026-08-07-20 measured that
    // direction at 26 invalid frees).
    //
    // This fixture therefore asserts only that the shape does not CRASH or
    // double-free; its 67-byte leak is expected and is why the program is
    // built with a `Plain` struct that LSan sees as clean -- the leaking
    // spelling cannot be an `assert_clean_asan_run` case while the row that
    // owns it is open. That row is the `Drop`-body divergence recorded
    // beside this one: a let-bound local of a self-referential struct passed
    // by value loses its `Drop` body on every compiled backend, which is
    // exactly the shape whose ownership is too unsettled to arm a free on.
    assert_clean_asan_run(
        r#"
struct Plain { id: i64, next: Option[Plain], tag: String }

fn mkp(i: i64) -> Plain { return Plain { id: i, next: Option.None, tag: f"p{i}" }; }
fn read(p: Plain) -> i64 { return p.id; }

fn main() {
    println(read(mkp(4)));
}
"#,
        &["4"],
        "self_referential_by_value_param_control",
    );
}

/// B-2026-09-07-4 — the MEMORY half of the via-call legs: the same program
/// under ASAN + LSan, where the pre-fix build double-freed the object each
/// hop handed on. One owner and one free per object on every leg.
#[test]
fn asan_argument_handed_back_through_a_hop_by_a_method() {
    assert_clean_asan_run(
            "shared struct Inner { v: i64 }\n\
             struct R { id: i64, name: String, inner: Inner }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"  dR{self.id}\") } }\n\
             struct P { id: i64, name: String, xs: Vec[i64] }\n\
             impl Drop for P { fn drop(mut ref self) { println(f\"  dP{self.id}\") } }\n\
             fn mk(i: i64) -> R { return R { id: i, name: f\"h{i}\", inner: Inner { v: i } }; }\n\
             fn mkp(i: i64) -> P { return P { id: i, name: f\"p{i}\", xs: [i] }; }\n\
             fn fwd(r: R) -> R { return r; }\n\
             fn pfwd(p: P) -> P { return p; }\n\
             fn dies(r: R) -> i64 { return r.id; }\n\
             struct Hold { n: i64 }\n\
             impl R { fn passb(r: R) -> R { return fwd(r); } }\n\
             impl R { fn passa(r: R) -> R { return r; } }\n\
             impl P { fn ppassb(p: P) -> P { return pfwd(p); } }\n\
             impl Hold { fn thruv(ref self, r: R) -> R { return fwd(r); } }\n\
             impl Hold { fn eats(ref self, r: R) -> i64 { return dies(r); } }\n\
             fn main() {\n\
               let h = Hold { n: 0 };\n\
               println(\"assoc_hop\"); let z = R.passb(mk(1)); println(f\"  v={z.inner.v}\");\n\
               println(\"assoc_direct\"); let y = R.passa(mk(2)); println(f\"  v={y.inner.v}\");\n\
               println(\"method_hop\"); let w = h.thruv(mk(3)); println(f\"  v={w.inner.v}\");\n\
               println(\"assoc_hop_copyable\"); let c = P.ppassb(mkp(4)); println(f\"  v={c.id}\");\n\
               println(\"method_hop_dies\"); println(f\"  v={h.eats(mk(5))}\");\n\
               println(\"named_into_assoc_hop\"); let a = mk(6); let b = R.passb(a); println(f\"  v={b.inner.v}\");\n\
               println(\"end\");\n\
             }\n",
            &[
                "assoc_hop",
                "  v=1",
                "  dR1",
                "assoc_direct",
                "  v=2",
                "  dR2",
                "method_hop",
                "  v=3",
                "  dR3",
                "assoc_hop_copyable",
                "  v=4",
                "  dP4",
                "method_hop_dies",
                "  dR5",
                "  v=5",
                "named_into_assoc_hop",
                "  v=6",
                "  dR6",
                "end"
            ],
            "argument_handed_back_through_a_hop_by_a_method",
        );
}

/// B-2026-09-07-11 — the MEMORY half: the same program under ASAN + LSan,
/// where the pre-fix build double-freed the object the method stored. One
/// owner and one free per object in both copy classes, and the free-function
/// control still frees its own.
#[test]
fn asan_named_local_into_a_storing_method() {
    assert_clean_asan_run(
            "shared struct Inner { v: i64 }\n\
             struct R { id: i64, name: String, inner: Inner }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"  dR{self.id}\") } }\n\
             struct S { id: i64, name: String }\n\
             impl Drop for S { fn drop(mut ref self) { println(f\"  dS{self.id}\") } }\n\
             fn mk(i: i64) -> R { return R { id: i, name: f\"h{i}\", inner: Inner { v: i } }; }\n\
             fn mks(i: i64) -> S { return S { id: i, name: f\"s{i}\" }; }\n\
             struct Box2 { mut xs: Vec[R] }\n\
             impl Box2 { fn push(mut ref self, r: R) { self.xs.push(r); } }\n\
             struct BoxS { mut ys: Vec[S] }\n\
             impl BoxS { fn add(mut ref self, s: S) { self.ys.push(s); } }\n\
             fn take(b: mut ref Box2, r: R) { b.xs.push(r); }\n\
             fn main() {\n\
               println(\"named_method\");\n\
               let mut b = Box2 { xs: Vec.new() };\n\
               let a = mk(1); b.push(a); println(f\"  len={b.xs.len()}\");\n\
               println(\"fresh_method\");\n\
               let mut c = Box2 { xs: Vec.new() };\n\
               c.push(mk(2)); println(f\"  len={c.xs.len()}\");\n\
               println(\"named_free_fn\");\n\
               let mut d = Box2 { xs: Vec.new() };\n\
               let e = mk(3); take(mut d, e); println(f\"  len={d.xs.len()}\");\n\
               println(\"copy_supported_method\");\n\
               let mut g = BoxS { ys: Vec.new() };\n\
               let h = mks(4); g.add(h); println(f\"  len={g.ys.len()}\");\n\
               println(\"two_named\");\n\
               let mut i = Box2 { xs: Vec.new() };\n\
               let j = mk(5); i.push(j); let k = mk(6); i.push(k); println(f\"  len={i.xs.len()}\");\n\
               println(\"end\");\n\
             }\n",
            &[
                "named_method",
                "  len=1",
                "  dR1",
                "fresh_method",
                "  len=1",
                "  dR2",
                "named_free_fn",
                "  len=1",
                "  dR3",
                "copy_supported_method",
                "  len=1",
                "  dS4",
                "two_named",
                "  len=2",
                "  dR5",
                "  dR6",
                "end"
            ],
            "named_local_into_a_storing_method",
        );
}

/// B-2026-09-07-10 — the MEMORY half: the same program under ASAN + LSan,
/// where the pre-fix build double-freed the object handed back through the
/// hop. One owner and one free per object on every spelling, including the
/// copy-supported control, whose caller slot keeps its own copy.
#[test]
fn asan_named_local_through_a_forwarding_hop() {
    assert_clean_asan_run(
            "shared struct Inner { v: i64 }\n\
             struct R { id: i64, name: String, inner: Inner }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"  dR{self.id}\") } }\n\
             struct P { id: i64, name: String, xs: Vec[i64] }\n\
             impl Drop for P { fn drop(mut ref self) { println(f\"  dP{self.id}\") } }\n\
             fn mk(i: i64) -> R { return R { id: i, name: f\"h{i}\", inner: Inner { v: i } }; }\n\
             fn mkp(i: i64) -> P { return P { id: i, name: f\"p{i}\", xs: [i] }; }\n\
             fn f(r: R) -> R { return r; }\n\
             fn ppass(p: P) -> P { return p; }\n\
             fn dies(r: R) -> i64 { return r.id; }\n\
             fn via(r: R) -> R { return f(r); }\n\
             fn via2(r: R) -> R { let m = r; return f(m); }\n\
             fn pvia(p: P) -> P { return ppass(p); }\n\
             fn dvia(r: R) -> i64 { return dies(r); }\n\
             fn mvia(r: R, c: bool) -> R { if c { return f(r); } return mk(99); }\n\
             fn main() {\n\
               println(\"named_hop\"); let a = mk(1); let z = via(a); println(f\"  v={z.inner.v}\");\n\
               println(\"fresh_hop\"); let w = via(mk(2)); println(f\"  v={w.inner.v}\");\n\
               println(\"rebind_hop\"); let b = mk(3); let y = via2(b); println(f\"  v={y.inner.v}\");\n\
               println(\"copyable_hop\"); let c = mkp(4); let d = pvia(c); println(f\"  v={d.id}\");\n\
               println(\"dies_in_hop\"); let e = mk(5); println(f\"  v={dvia(e)}\");\n\
               println(\"mixed_dies_inside\"); let g = mk(6); let h = mvia(g, false); println(f\"  v={h.id}\");\n\
               println(\"end\");\n\
             }\n",
            &[
                "named_hop",
                "  v=1",
                "  dR1",
                "fresh_hop",
                "  v=2",
                "  dR2",
                "rebind_hop",
                "  v=3",
                "  dR3",
                "copyable_hop",
                "  v=4",
                "  dP4",
                "dies_in_hop",
                "  v=5",
                "  dR5",
                "mixed_dies_inside",
                "  dR6",
                "  v=99",
                "  dR99",
                "end"
            ],
            "named_local_through_a_forwarding_hop",
        );
}

/// B-2026-09-06-71 — the MEMORY half, which is the row: the same program
/// under ASAN + LSan, where the pre-fix build double-freed the object the
/// callee handed back. One owner and one free per object on every argument
/// spelling, and the copy-supported cell still frees the caller's own copy.
#[test]
fn asan_named_local_argument_to_a_passthrough_callee() {
    assert_clean_asan_run(
            "shared struct Inner { v: i64 }\n\
             struct R { id: i64, name: String, inner: Inner }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"  dR{self.id}\") } }\n\
             struct P { id: i64, name: String, xs: Vec[i64] }\n\
             impl Drop for P { fn drop(mut ref self) { println(f\"  dP{self.id}\") } }\n\
             fn mk(i: i64) -> R { return R { id: i, name: f\"h{i}\", inner: Inner { v: i } }; }\n\
             fn mkp(i: i64) -> P { return P { id: i, name: f\"p{i}\", xs: [i] }; }\n\
             fn pass(r: R) -> R { return r; }\n\
             fn rebpass(r: R) -> R { let m = r; return m; }\n\
             fn ppass(p: P) -> P { return p; }\n\
             fn dies(r: R) -> i64 { return r.id; }\n\
             fn main() {\n\
               println(\"named\"); let a = mk(1); let z = pass(a); println(f\"  v={z.inner.v}\");\n\
               println(\"named_rebind\"); let b = mk(2); let y = rebpass(b); println(f\"  v={y.inner.v}\");\n\
               println(\"fresh_temp\"); let w = pass(mk(3)); println(f\"  v={w.inner.v}\");\n\
               println(\"copyable\"); let c = mkp(4); let d = ppass(c); println(f\"  v={d.id}\");\n\
               println(\"dies_inside\"); let e = mk(5); println(f\"  v={dies(e)}\");\n\
               println(\"two_in_a_row\"); let g = mk(6); let h = pass(g); let n = mk(7); let q = rebpass(n); println(f\"  v={h.inner.v}{q.inner.v}\");\n\
               println(\"end\");\n\
             }\n",
            &[
                "named",
                "  v=1",
                "  dR1",
                "named_rebind",
                "  v=2",
                "  dR2",
                "fresh_temp",
                "  v=3",
                "  dR3",
                "copyable",
                "  v=4",
                "  dP4",
                "dies_inside",
                "  v=5",
                "  dR5",
                "two_in_a_row",
                "  v=67",
                "  dR7",
                "  dR6",
                "end"
            ],
            "named_local_argument_to_a_passthrough_callee",
        );
}

/// B-2026-09-05-37 — the LEAK gate for a whole rebind of a by-value `Drop`
/// param nested in a branch.
///
/// Three rounds of fifteen calls; 443 allocations, 443 frees under valgrind
/// after the fix. Before it, every call whose branch did NOT rebind lost
/// one `String` — `brF`, `faF`, `rdF`, `arF`, `tw00`, `tw10` and `opt`'s
/// `None` leg — because the source's memory action was retracted from the
/// cleanup frame at COMPILE time, on every path, from inside the branch.
///
/// THE `top` CELL IS THE CONTROL AND MUST STAY. A TOP-LEVEL `let m = r;`
/// keeps that static removal (B-2026-08-09-16 put it there to stop a double
/// free), so the guard has to decline for it; if a future change arms the
/// bit unconditionally this cell double-frees rather than leaks, which is
/// the failure worth catching first.
#[test]
/// B-2026-09-06-52 — the freeing half of
/// `test_e2e_declined_copy_param_rebind_keeps_the_callers_ownership`.
///
/// Every cell rebinds a by-value param whose struct the prologue declined to
/// own, so nothing was entry-copied and the caller's buffers must be freed
/// exactly once, by the caller. Before the fix this aborted with a double
/// free under the JIT and at `-O0`; ASAN sees the `shared` handle's refcount
/// block read and written after its free even where the abort does not land.
///
/// THE `rd*` CELLS ARE B-2026-09-06-59's, added when that row closed. They
/// read the source AFTER the rebind, which makes the ownership pass take a
/// `UseAfterMove` defensive copy — an INDEPENDENT buffer that this row's
/// fix had taught the destination not to own, so each lost its `String`.
///
/// THEY ARE `-O0`-ONLY, and that is the whole reason they are here rather
/// than in the E2E twin. At `-O2` LLVM deletes the dead copy before it can
/// be lost, so this fixture is CLEAN ON THE UNFIXED COMPILER at the default
/// opt level: measured on stock `main`, `-O0` loses 15 B in 5 blocks (one
/// per cell, 98 allocs / 93 frees) while `-O2` reports 0 errors. A
/// non-vacuity check run at the default level would therefore "pass" on a
/// tree without the fix and prove nothing. `scripts/asan-o0-leg.sh` is what
/// makes these cells a ratchet; `cargo test --features llvm` alone does
/// not.
fn asan_declined_copy_param_rebind_keeps_the_callers_ownership() {
    assert_clean_asan_run_min_allocs(
        r#"
shared struct Inner { v: i64 }
struct R { id: i64, name: String, inner: Inner }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}", inner: Inner { v: i } }; }

shared struct Deep { v: i64 }
struct Mid { d: Deep }
struct S { id: i64, name: String, mid: Mid }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mks(i: i64) -> S { return S { id: i, name: f"h{i}", mid: Mid { d: Deep { v: i } } }; }

struct N { id: i64, name: String, inner: Inner }
fn mkn(i: i64) -> N { return N { id: i, name: f"h{i}", inner: Inner { v: i } }; }

struct Node { id: i64, name: String, kids: Vec[Node] }
impl Drop for Node { fn drop(mut ref self) { println(f"dNd{self.id}") } }
fn mknode(i: i64) -> Node { return Node { id: i, name: f"h{i}", kids: Vec[Node].new() }; }

struct P { id: i64, name: String }
impl Drop for P { fn drop(mut ref self) { println(f"dP{self.id}") } }
fn mkp(i: i64) -> P { return P { id: i, name: f"h{i}" }; }

fn top(r: R) -> i64 { let m = r; return m.inner.v; }
fn br(r: R, keep: bool) -> i64 { if keep { let m = r; return m.inner.v; } return 0; }
fn two(r: R) -> i64 { let m = r; let n = m; return n.inner.v; }
fn keeps(x: R) -> R { return x; }
fn call(r: R) -> i64 { let w = keeps(r); return w.inner.v; }
fn pair(a: R, b: R) -> i64 { let m = b; return m.inner.v + a.id; }
fn loopreb(r: R, n: i64) -> i64 {
    let mut t = 0;
    for i in 0..n { if i == 0 { let m = r; t = t + m.inner.v; } }
    return t;
}
fn deep(s: S) -> i64 { let m = s; return m.mid.d.v; }
fn nodrop(n: N) -> i64 { let m = n; return m.inner.v; }
fn selfref(nd: Node) -> i64 { let m = nd; return m.id; }
fn ctl(p: P) -> i64 { let m = p; return m.id; }
fn rd(r: R) -> String { let m = r; return f"{r.name}"; }
fn rdi(r: R) -> i64 { let m = r; return r.inner.v; }
fn rdb(r: R) -> i64 { let m = r; return m.id + r.id; }
fn rdc(r: R) -> String { let m = r; let n = m; return f"{m.name}"; }
impl R { fn takerd(self) -> String { let m = self; let n = m; return f"{m.name}"; } }

fn main() {
    println(f"top={top(mk(21))}");
    println(f"brT={br(mk(22), true)}");
    println(f"brF={br(mk(23), false)}");
    println(f"two={two(mk(24))}");
    println(f"call={call(mk(25))}");
    println(f"pair={pair(mk(1), mk(26))}");
    println(f"loop={loopreb(mk(28), 3)}");
    println(f"deep={deep(mks(29))}");
    println(f"nod={nodrop(mkn(30))}");
    println(f"self={selfref(mknode(31))}");
    println(f"ctl={ctl(mkp(32))}");
    println(f"rd={rd(mk(33))}");
    println(f"rdi={rdi(mk(34))}");
    println(f"rdb={rdb(mk(35))}");
    println(f"rdc={rdc(mk(36))}");
    println(f"rds={mk(37).takerd()}");
    println("end");
}
"#,
        &[
            "dR21", "top=21", "dR22", "brT=22", "dR23", "brF=0", "dR24", "two=24", "dR25",
            "call=25", "dR26", "dR1", "pair=27", "dR28", "loop=28", "dS29", "deep=29", "nod=30",
            "dNd31", "self=31", "dP32", "ctl=32", "dR33", "rd=h33", "dR34", "rdi=34", "dR35",
            "rdb=70", "dR36", "rdc=h36", "dR37", "rds=h37", "end",
        ],
        "b0906-52-declined-copy-param-rebind",
        50,
    );
}

#[test]
/// B-2026-09-06-61 — the FREEING half of
/// `test_e2e_param_handed_back_through_a_rebind_leaves_one_owner`.
///
/// That test asserts the output, which on the parent was already correct at
/// `-O2` while the program held a use-after-free; only a sanitizer
/// separates "prints the right lines" from "owns its memory once". Measured
/// here: 52 allocs / 52 frees and 0 valgrind errors after the fix, against
/// `free(): double free detected in tcache 2` and 26/27 errors on the
/// parent.
///
/// DELIBERATELY OMITS the `tp` and `op` cells the E2E fixture carries. Both
/// go from a double free to a 16-byte leak of the `shared` handle's
/// refcount block at `-O0` — strictly less severe, and not this fix's to
/// close: their no-rebind twin (`fn f(r: R) -> (R, i64) { return (r, 9); }`)
/// leaks the same 16 bytes on the PARENT, so the residual is a pre-existing
/// defect in the aggregate-return path that this change merely lands them
/// on. Filed separately; including them here would make this fixture red for
/// someone else's bug.
fn asan_param_handed_back_through_a_rebind_leaves_one_owner() {
    assert_clean_asan_run_min_allocs(
        r#"
shared struct Inner { v: i64 }
struct R { id: i64, name: String, inner: Inner }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}", inner: Inner { v: i } }; }
struct Box2 { r: R }
struct P { id: i64, name: String }
impl Drop for P { fn drop(mut ref self) { println(f"dP{self.id}") } }
fn mkP(i: i64) -> P { return P { id: i, name: f"p{i}" }; }

fn top(r: R) -> R { let m = r; return m; }
fn two(r: R) -> R { let m = r; let n = m; return n; }
fn tail(r: R) -> R { let m = r; m }
fn pair(a: i64, r: R) -> R { let m = r; return m; }
fn bx(r: R) -> Box2 { let m = r; return Box2 { r: m }; }
fn rd(r: R) -> R { let m = r; println(f"in={m.name}"); return m; }
fn ctl(p: P) -> P { let m = p; return m; }
fn ret(r: R) -> R { return r; }

fn main() {
  let a = top(mk(41)); println(f"top={a.inner.v}");
  let b = two(mk(42)); println(f"two={b.inner.v}");
  let c = tail(mk(43)); println(f"tail={c.inner.v}");
  let d = pair(7, mk(44)); println(f"pair={d.inner.v}");
  let e = bx(mk(45)); println(f"bx={e.r.inner.v}");
  let h = rd(mk(48)); println(f"rd={h.name}");
  top(mk(49));
  let mut i = 0;
  while i < 2 { let z = top(mk(50)); println(f"lp={z.inner.v}"); i = i + 1; }
  let p = ctl(mkP(52)); println(f"ctl={p.name}");
  let r = ret(mk(53)); println(f"ret={r.inner.v}");
  println("end");
}
"#,
        &[
            "top=41", "dR41", "two=42", "dR42", "tail=43", "dR43", "pair=44", "dR44", "bx=45",
            "dR45", "in=h48", "rd=h48", "dR48", "dR49", "lp=50", "dR50", "lp=50", "dR50",
            "ctl=p52", "dP52", "ret=53", "dR53", "end",
        ],
        "b0906-61-rebind-handback",
        40,
    );
}

/// B-2026-09-19-20 — the MEMORY half of an indexed-receiver method whose
/// CONTAINER is a call.
///
/// The OWNED spelling (`own:`, `ownclone:`, `nested:`) is why this cell
/// exists. Every container hoist that existed before this row pointed its
/// synth at storage somebody else owns — a struct field, a tuple element,
/// a map's live bucket, a borrow's referent — so each one's teardown is
/// registry bookkeeping that emits no IR. A call's result is a fresh owned
/// temp, so the fix had to emit a real `free` for the first time on this
/// path: the index is lowered through machinery that already drops the
/// container (B-2026-07-15-27), and the standalone element it hands back
/// becomes this arm's own temp, dropped after the method returns.
///
/// One `free` too few leaks per evaluation and one too many double-frees,
/// and neither shows in the output half
/// (`test_e2e_indexed_receiver_method_on_a_call_container` in
/// `tests/codegen.rs`), which is what splits the two tests. The loop is
/// load-bearing for the same reason: a per-evaluation imbalance
/// accumulates here instead of hiding in a single teardown.
///
/// The BORROW cells (`borrarr:`, `borrvec:`, `borrfree:`, `bound:`) are
/// the control, and they are the half that must NOT free: their owner is
/// the receiver, still live. `orig:` reads those borrowed fields after
/// three borrows of them in the same iteration, so an over-eager teardown
/// dangles rather than merely leaking.
#[test]
fn asan_indexed_receiver_method_on_a_call_container_is_balanced() {
    assert_clean_asan_run(
        r#"
struct Hold { arr: Array[String, 2], v: Vec[String] }
impl Hold {
    fn pa(ref self) -> ref Array[String, 2] { return self.arr; }
    fn pv(ref self) -> ref Vec[String] { return self.v; }
}
fn pav(h: ref Hold) -> ref Vec[String] { return h.v; }
fn mkv(n: i64) -> Vec[String] { return [f"b191920-own-aaaaaaaaaaaaaaaa-{n}", f"b191920-own-bbbb-{n}"]; }
fn mkn(n: i64) -> Vec[Vec[i64]] { return [[n, n + 1, n + 2], [n]]; }
fn main() {
    let mut i: i64 = 0;
    while i < 3 {
        let h = Hold {
            arr: [f"b191920-arr-cccccccccccccccc-{i}", f"b191920-arr-dddd-{i}"],
            v: [f"b191920-vec-eeeeeeeeeeeeeeee-{i}", f"b191920-vec-ffff-{i}"],
        };
        println(f"own:{mkv(i)[0].len()}");
        println(f"ownclone:{mkv(i)[1].clone()}");
        println(f"borrarr:{h.pa()[0].len()}");
        println(f"borrvec:{h.pv()[1].len()}");
        println(f"borrfree:{pav(h)[0].len()}");
        let a: ref Array[String, 2] = h.pa();
        println(f"bound:{a[1].len()}");
        println(f"nested:{mkn(i)[0].len()}");
        println(f"orig:{h.arr[0].len()}");
        i = i + 1;
    }
}
"#,
        &[
            "own:30",
            "ownclone:b191920-own-bbbb-0",
            "borrarr:30",
            "borrvec:18",
            "borrfree:30",
            "bound:18",
            "nested:3",
            "orig:30",
            "own:30",
            "ownclone:b191920-own-bbbb-1",
            "borrarr:30",
            "borrvec:18",
            "borrfree:30",
            "bound:18",
            "nested:3",
            "orig:30",
            "own:30",
            "ownclone:b191920-own-bbbb-2",
            "borrarr:30",
            "borrvec:18",
            "borrfree:30",
            "bound:18",
            "nested:3",
            "orig:30",
        ],
        "asan_indexed_receiver_method_on_a_call_container_is_balanced",
    );
}

/// Ad-hoc reduction probe — runs an ARBITRARY `.kara` file through the same
/// ASAN path the fixtures use, with the allocation count printed. Ignored by
/// default; it asserts nothing about output because the program under
/// reduction changes every iteration.
///
/// This exists because minimizing a leak on B-2026-08-05-7 means varying the
/// PROGRAM, not the compiler, and rebuilding this ~200 MB test binary per
/// iteration to edit a string literal dominates the loop. Reading the source
/// from the environment rebuilds it once. B-2026-08-05-7's METHOD NOTE
/// recommends exactly this shape.
///
///   KARAC_PROBE_SRC=/path/to/reduce.kara KARAC_OPT_LEVEL=0 \
///     cargo test --features llvm --test memory_sanitizer -- \
///     --ignored --exact memory_sanitizer_tests::asan_probe_from_env --nocapture
///
/// Prints `PROBE <allocs> allocations, exit <code>` plus the raw ASAN stderr,
/// and fails only when ASAN itself reports an error — so a clean run at one
/// `-O` level and a dirty run at the other is a single flag flip apart.
#[test]
#[ignore = "reduction aid: needs KARAC_PROBE_SRC"]
fn asan_probe_from_env() {
    if !asan_available() {
        eprintln!("PROBE: ASAN unavailable — skipping");
        return;
    }
    let path =
        std::env::var("KARAC_PROBE_SRC").expect("set KARAC_PROBE_SRC to the .kara file to reduce");
    let src = std::fs::read_to_string(&path).expect("KARAC_PROBE_SRC unreadable");
    let Some((stdout, stderr, status)) = run_under_asan_counting(&src, "probe") else {
        eprintln!("PROBE: setup failed (parse error / missing runtime archive)");
        return;
    };
    let allocs = asan_malloc_calls(&stderr).map_or(-1, |n| n as i64);
    eprintln!("PROBE {allocs} allocations, exit {:?}", status.code());
    eprintln!("PROBE stdout: {}", stdout.trim());
    if !status.success() {
        eprintln!("{stderr}");
    }
    assert!(
        status.success(),
        "PROBE: ASAN reported an error (see above)"
    );
}

// ── Baseline: no heap allocations ─────────────────────────────
// Sanity-checks the harness itself — should trivially pass on any host
// with a working `cc + ASAN`. If this fails, the infrastructure is
// broken, not the codegen.

#[test]
fn asan_baseline_no_allocations() {
    assert_clean_asan_run(
        r#"
fn main() {
    println(42);
}
"#,
        &["42"],
        "baseline_no_allocations",
    );
}

#[test]
fn asan_operand_temp_chained_concat_freed() {
    // Slice 3c chained case: `make_s() + " mid " + <tail>` parses as
    // `(make_s() + " mid ") + <tail>`. The fresh `make_s()` operand of the
    // INNER `+` is freed there; the inner `+` RESULT is itself a fresh temp
    // consumed as the outer `+`'s left operand and must also be freed (it is
    // a `Binary{Add}` operand, recognized as a fresh String concat). Three
    // distinct buffers — `make_s()`, the inner concat, the outer concat (the
    // last bound to `r`) — each freed exactly once: no leak, no double-free.
    assert_clean_asan_run(
            r#"
fn make_s() -> String {
    let s: String = "a freshly allocated heap operand string over thirty-six bytes";
    return s;
}

fn main() {
    let mut i = 0;
    while i < 3 {
        let r = make_s() + " mid " + "tail padded out beyond thirty-six bytes here";
        println(r);
        i = i + 1;
    };
}
"#,
            &[
                "a freshly allocated heap operand string over thirty-six bytes mid tail padded out beyond thirty-six bytes here",
                "a freshly allocated heap operand string over thirty-six bytes mid tail padded out beyond thirty-six bytes here",
                "a freshly allocated heap operand string over thirty-six bytes mid tail padded out beyond thirty-six bytes here",
            ],
            "operand_temp_chained_concat_freed",
        );
}

#[test]
fn asan_user_method_builtin_name_on_literal_receiver_clean() {
    // B-2026-07-18-48: dispatching a user method (builtin-colliding name) on
    // a struct/enum LITERAL receiver materializes the receiver into a temp
    // and re-dispatches. The owned-`self` method consumes that temp (its
    // drop frees the payload), so the materialization must NOT also drop the
    // caller's copy — verify no double-free / leak across a struct literal, an
    // enum literal, and a call-result receiver.
    // DE-VACUUMED (B-2026-08-04-17). As originally written this allocated
    // NOTHING at -O2: literal payloads, printed once, so every buffer was
    // foldable or dead and LLVM deleted them. It passed while a real leak
    // existed -- the struct-literal receiver's field String was never
    // freed, which the -O0 leg caught and which is fixed alongside this.
    //
    // Rewritten to the row's three-legged recipe so it is a live gate at
    // the DEFAULT level rather than only at -O0: an opaque seed
    // (`env.args().len()` is 1 under this harness but the optimizer cannot
    // see that), runtime-derived payload CONTENT, a BYTE-level read via
    // `contains`, and enough iterations to survive unrolling. Floored so it
    // cannot silently drift back to zero.
    assert_clean_asan_run_min_allocs(
        r#"
struct R { v: String }
impl R { fn get(self) -> String { self.v } }
enum E { A(String) }
impl E { fn take(self) -> String { match self { E.A(s) => s } } }
fn mk(n: i64) -> R { R { v: f"chained-{n}" } }
fn main() {
    let seed = env.args().len();
    let mut hits = 0i64;
    let mut i = 0i64;
    while i < 60i64 {
        let n = seed + i;
        // The shape under test: an inline STRUCT-LITERAL receiver for an
        // owned-`self` method. Its materialized temp was never drop-tracked.
        let a = R { v: f"structlit-{n}" }.get();
        // Enum-literal and call-result receivers, the fixture's other two arms.
        let b = E.A(f"enumlit-{n}").take();
        let c = mk(n).get();
        // Byte-level reads against a runtime-derived needle, so the buffers
        // are live and cannot be deleted.
        if a.contains(f"structlit-{n}") { hits = hits + 1i64; }
        if b.contains(f"enumlit-{n}") { hits = hits + 1i64; }
        if c.contains(f"chained-{n}") { hits = hits + 1i64; }
        i = i + 1;
    }
    println(f"hits={hits}");
}
"#,
        &["hits=180"],
        "user_method_builtin_name_on_literal_receiver",
        100,
    );
}

// ── kara-katas leetcode #8 (atoi) end-to-end ─────────────────
//
// The kata that surfaced the interpreter Cast no-op (commit
// 6a79ae2) and motivated `String.bytes()` (commit 517aa1d).
// Locks in: the shipped kata source compiles, runs, prints the
// 20 expected integers, and exits ASAN-clean. Source kept
// in-sync with `kara-katas/.../atoi.kara` (~80 lines verbatim);
// if the kara-katas file drifts, this test stays a fixed
// regression target. Output matches what `python3 atoi.py`
// emits — see kara-katas/leetcode/1-100/8-string-to-integer-atoi.

#[test]
fn asan_kata_8_atoi_bytes_one_pass() {
    assert_clean_asan_run(
        r#"
fn my_atoi(s: ref String) -> i32 {
    let bytes = s.bytes();
    let n = bytes.len();

    let space: u8 = ' ' as u32 as u8;
    let plus:  u8 = '+' as u32 as u8;
    let minus: u8 = '-' as u32 as u8;
    let zero:  u8 = '0' as u32 as u8;
    let nine:  u8 = '9' as u32 as u8;

    let mut i = 0i64;
    while i < n and bytes[i] == space {
        i = i + 1;
    }

    let mut sign: i32 = 1i32;
    if i < n and bytes[i] == plus {
        i = i + 1;
    } else if i < n and bytes[i] == minus {
        sign = -1i32;
        i = i + 1;
    }

    let int_max: i32 = 2147483647i32;
    let int_min: i32 = -2147483648i32;
    let max_div: i32 = int_max / 10i32;

    let mut result: i32 = 0i32;
    while i < n {
        let b = bytes[i];
        if b < zero or b > nine {
            break;
        }
        let digit: i32 = (b as i32) - (zero as i32);
        if result > max_div or (result == max_div and digit > 7i32) {
            if sign == 1i32 {
                return int_max;
            }
            return int_min;
        }
        result = result * 10i32 + digit;
        i = i + 1;
    }

    sign * result
}

fn report(s: ref String) {
    println(my_atoi(s));
}

fn main() {
    report("42");
    report("   -42");
    report("4193 with words");
    report("words and 987");
    report("-91283472332");
    report("91283472332");
    report("+1");
    report("");
    report("   ");
    report("+-12");
    report("-+12");
    report("  0000000000012345678");
    report("2147483647");
    report("-2147483648");
    report("2147483648");
    report("-2147483649");
    report("  +0 123");
    report("00000-42a1234");
    report("  -0012a42");
    report("+");
}
"#,
        &[
            "42",
            "-42",
            "4193",
            "0",
            "-2147483648",
            "2147483647",
            "1",
            "0",
            "0",
            "0",
            "0",
            "12345678",
            "2147483647",
            "-2147483648",
            "2147483647",
            "-2147483648",
            "0",
            "0",
            "-12",
            "0",
        ],
        "kata_8_atoi_bytes_one_pass",
    );
}

#[test]
fn asan_reshaper_headerless_dummy_free_repeat() {
    // Headerless "reshaper" elision (KARAC_HEADERLESS_RESHAPER, default-OFF):
    // an in-place link-permuting transform (reverse a sublist, LeetCode #92)
    // that owns its input list, permutes links via head-insertion splices,
    // and returns `dummy.next`. Under the flag the whole ListNode goes
    // headerless (16 B, no rc word); the sentinel `dummy` (a fresh node NOT
    // in the returned chain) must get a single-node free at scope exit. A
    // prior bug leaked that dummy once per reversal when the walk reassigned
    // `prev` off it (left > 1). Build + reverse + fold repeated 40× with a
    // shifting left>1 window, so a per-iteration dummy leak trips
    // LeakSanitizer (Linux) and any double-free trips ASAN. Runs clean under
    // BOTH layouts: headered by default, headerless when the env flag is set
    // (the flag-on leak gate is the point — run this test under
    // `KARAC_HEADERLESS_RESHAPER=1` in the Linux-LSan harness).
    assert_clean_asan_run(
        r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
fn build(m: i64, seed: i64) -> Option[ListNode] {
    let dummy = ListNode { val: -1, next: None };
    let mut tail = dummy; let mut j = 0i64;
    while j < m { let node = ListNode { val: (j + seed) % 97i64, next: None }; tail.next = Some(node); tail = node; j = j + 1i64; }
    dummy.next
}
fn reverse_between(head: Option[ListNode], left: i64, right: i64) -> Option[ListNode] {
    let dummy = ListNode { val: 0, next: head };
    let mut prev = dummy; let mut i = 1i64;
    while i < left { match prev.next { Some(n) => { prev = n; } None => {} } i = i + 1i64; }
    match prev.next { Some(cur) => { let mut j = left;
        while j < right { match cur.next { Some(nxt) => { cur.next = nxt.next; nxt.next = prev.next; prev.next = Some(nxt); } None => {} } j = j + 1i64; } } None => {} }
    dummy.next
}
fn fold(list: Option[ListNode], seed: i64) -> i64 { let mut a = seed; let mut c = list;
    loop { match c { Some(n) => { a = (a * 131i64 + (n.val + 1i64)) % 1000000007i64; c = n.next; } None => break, } } a }
fn main() {
    let mut sum = 0i64; let mut k = 0i64;
    while k < 40i64 {
        let list = build(30i64, k);
        let r = reverse_between(list, 2i64 + (k % 5i64), 12i64);
        sum = (sum * 131i64 + fold(r, k)) % 1000000007i64;
        k = k + 1i64;
    }
    println(sum);
}
"#,
        &["530882893"],
        "reshaper_headerless_dummy_free_repeat",
    );
}

#[test]
fn asan_cluster_append_builder_repeat() {
    // Phase B1 cluster free-walk under ASAN: the root's cleanup
    // frees every chain node WITHOUT consulting refcounts — a
    // wrong analysis (any node with a second owner) is an
    // immediate ASAN double-free; a missed node is a leak (linux
    // CI LeakSanitizer). Covers the canonical append builder +
    // inline walk + a link-displacement orphan (freed through
    // normal RC mid-build, unreachable from the walk).
    assert_clean_asan_run(
        r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
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
    let mut sum = 0;
    let mut cur = dummy.next;
    while cur.is_some() {
        let x = cur.unwrap();
        sum = sum + x.val;
        cur = x.next;
    }
    sum
}
fn displaced() -> i64 {
    let dummy = ListNode { val: 0, next: None };
    let a = ListNode { val: 10, next: None };
    let b = ListNode { val: 20, next: None };
    dummy.next = Some(a);
    dummy.next = Some(b);
    let mut sum = 0;
    let mut cur = dummy.next;
    while cur.is_some() {
        let x = cur.unwrap();
        sum = sum + x.val;
        cur = x.next;
    }
    sum
}
fn main() {
    let mut total = 0;
    let mut iter = 0;
    while iter < 50 {
        total = total + build_and_sum(50);
        total = total + displaced();
        iter = iter + 1;
    }
    println(total);
}
"#,
        &["64750"],
        "cluster_append_builder_repeat",
    );
}

#[test]
fn asan_headerless_cluster_repeat() {
    // Phase D headerless members under ASAN: the type-pure
    // canonical builder allocates 16-byte nodes (no rc word) and
    // the root free-walk geps the SHIFTED link slot — a missed
    // layout conversion at any consumer site reads/writes 8 bytes
    // off and trips ASAN heap-buffer-overflow immediately; a
    // free-walk against the wrong slot is a wild-pointer free.
    // 100 iterations x 100 nodes; sum(1..=100) = 5050 per call.
    // Mixed-layout half: `lone()` uses the same type headered
    // (free literal, no cluster) in the same binary.
    assert_clean_asan_run(
        r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
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
    let mut sum = 0;
    let mut cur = dummy.next;
    while cur.is_some() {
        let x = cur.unwrap();
        sum = sum + x.val;
        cur = x.next;
    }
    sum
}
fn lone() -> i64 {
    let a = ListNode { val: 3, next: None };
    a.val
}
fn main() {
    let mut total = 0;
    let mut iter = 0;
    while iter < 100 {
        total = total + build_and_sum(100);
        total = total + lone();
        iter = iter + 1;
    }
    println(total);
}
"#,
        &["505300"],
        "headerless_cluster_repeat",
    );
}

#[test]
fn asan_adopted_builders_repeat() {
    // Phase C1c under ASAN: both adopted-family shapes — the
    // sanctioned match head-read and the non-owning cursor walk —
    // dropping per iteration via the option-guarded free-walk. An
    // adoption miscount has both signatures: an over-eager walk
    // double-frees against a still-counted ref (immediate ASAN
    // UAF); a missed adoption / suppressed-cleanup mismatch leaks
    // a 100-node chain per iteration (LeakSanitizer where
    // available, RSS blowup otherwise). 100 iterations, exact
    // total: (1 + 5050) * 100.
    assert_clean_asan_run(
        r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
fn build_someroot(n: i64) -> Option[ListNode] {
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
fn build_rootlink(n: i64) -> Option[ListNode] {
    let dummy = ListNode { val: 0, next: None };
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
fn main() {
    let mut total = 0;
    let mut iter = 0;
    while iter < 100 {
        let a = build_someroot(100);
        match a {
            Some(node) => { total = total + node.val; }
            None => {}
        }
        let b = build_rootlink(100);
        let mut cur = b;
        while cur.is_some() {
            let x = cur.unwrap();
            total = total + x.val;
            cur = x.next;
        }
        iter = iter + 1;
    }
    println(total);
}
"#,
        &["505100"],
        "adopted_builders_repeat",
    );
}

#[test]
fn asan_param_coexisting_builders_repeat() {
    // Phase C1a under ASAN: kata #2's exact pipeline — C1b
    // builders feed a param-walking adder whose own cluster
    // transfers out (member-type params coexist with the cluster,
    // keeping full RC). A wall failure has both signatures: a
    // param node entering the cluster double-frees against its RC
    // drop; a fresh node leaking under a param chain over-frees on
    // the param's dec-walk. 200 iterations, exact total pins the
    // arithmetic (342+465=807 → digit sum 15 → 3000).
    assert_clean_asan_run(
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
    let mut total = 0;
    let mut iter = 0;
    while iter < 200 {
        let l1 = from_three(2, 4, 3);
        let l2 = from_three(5, 6, 4);
        let r = add_two_numbers(l1, l2);
        total = total + sum_chain(r);
        iter = iter + 1;
    }
    println(total);
}
"#,
        &["3000"],
        "param_coexisting_builders_repeat",
    );
}

#[test]
fn asan_a2b2_method_distinct_receivers_fanout_clean() {
    // A2b-2 Phase 2 Slice 2: two `mut ref self` network method calls on
    // DISTINCT non-shared local receivers (`s1.fetch(); s2.fetch()`) fan out.
    // Memory-safety proof for the method-receiver path: each receiver is
    // BORROWED (mut ref self — not moved into the coroutine, so no
    // receiver double-drop), the mutation is written back through the
    // captured-mutation machinery, and each returned owned `String` flows
    // through its own return slot with the parent as sole drop owner. The
    // `Stream` locals drop exactly once at `main` scope exit. A double-free
    // or leak surfaces under LSan/ASan.
    assert_clean_asan_run(
        r#"
struct Stream { n: i64 }
impl Stream {
    fn fetch(mut ref self) -> String with sends(Network) receives(Network) {
        self.n = self.n + 1;
        return "aaaaaaaaaaaaaaaaaaaa";
    }
}
fn main() {
    let mut s1 = Stream { n: 0 };
    let mut s2 = Stream { n: 0 };
    let a = s1.fetch();
    let b = s2.fetch();
    println(a);
    println(b);
}
"#,
        &["aaaaaaaaaaaaaaaaaaaa", "aaaaaaaaaaaaaaaaaaaa"],
        "asan_a2b2_method_distinct_receivers_fanout_clean",
    );
}

#[test]
fn asan_self_referential_by_value_param_touching_nothing_frees_its_box() {
    // B-2026-09-09-3 — B-2026-09-06-66's by-value remainder, closed for the
    // callee class that can be proved safe.
    //
    // That row's gate refused on the whole-type question "is this ever a
    // bare by-value param", inherited from the shared-owning arm. It is now
    // the two hazards that question stood in for: does any callee STORE
    // such a param (asked with the same predicates
    // `declined_copy_arg_stays_with_caller` uses per call site, so the two
    // cannot drift), and does any callee MOVE the promoted field out
    // (B-2026-08-07-20, unchanged). Here the callee provably does neither.
    //
    // 67 bytes: the boxed `Node` payload plus its `tag`. The `Drop` bodies
    // were already correct before this fix and the values already agreed,
    // so no A/B gate could see this — only a leak checker.
    assert_clean_asan_run(
        r#"
struct Node { id: i64, next: Option[Node], tag: String }
impl Drop for Node { fn drop(mut ref self) { println(f"  dN{self.id}") } }

fn mkn(i: i64) -> Node { return Node { id: i, next: Option.None, tag: f"t{i}" }; }
fn nothing(n: Node) -> i64 { return 1; }

fn main() {
    let c: Node = Node { id: 9, next: Option.Some(mkn(10)), tag: "n" };
    println(nothing(c));
}
"#,
        &["1", "  dN9", "  dN10"],
        "self_referential_by_value_param_touching_nothing",
    );
}

#[test]
fn asan_swap_pairs_pair_relink_loop() {
    // Full kata-#24 iterative pair-swap over a fresh 6-node chain:
    // per-pair three-store re-link with `break` exits from inside
    // `if let` arms holding live bindings. Catches both halves of
    // the fix under ASAN — the binding acquire (UAF on `second`)
    // and the break-drain (whose absence leaks; whose
    // over-aggressive form would double-free on the fall-through
    // path).
    assert_clean_asan_run(
        r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
fn build(n: i64) -> Option[ListNode] {
    let head = ListNode { val: 1, next: None };
    let mut tail = head;
    for i in 2..n + 1 {
        let node = ListNode { val: i, next: None };
        tail.next = Some(node);
        tail = node;
    }
    Some(head)
}
fn swap_pairs(head: Option[ListNode]) -> Option[ListNode] {
    let dummy = ListNode { val: 0, next: head };
    let mut prev = dummy;
    loop {
        if let Some(first) = prev.next {
            if let Some(second) = first.next {
                first.next = second.next;
                second.next = Some(first);
                prev.next = Some(second);
                prev = first;
            } else {
                break;
            }
        } else {
            break;
        }
    }
    dummy.next
}
fn main() {
    let mut cur = swap_pairs(build(6));
    let mut sum = 0;
    loop {
        match cur {
            Some(node) => {
                sum = sum + node.val;
                cur = node.next;
            }
            None => break,
        }
    }
    println(sum);
}
"#,
        &["21"],
        "swap_pairs_pair_relink_loop",
    );
}

// B-2026-07-30-5 — the VecDeque head-index lowering frees the malloc
// base, exactly once, on every exit shape. The header's data pointer
// never moves (only `head` advances, in a frame-local alloca), so the
// scope-exit free is correct by construction — this pins that invariant
// against a future formulation change: a lowering that advanced the data
// pointer and freed it raw with head > 0 is an instant ASAN
// invalid-free, and one that leaked the buffer is LSan-visible through
// the loop (each iteration's buffer is unreachable once the allocas are
// overwritten). Shapes per iteration: a full drain past growth and
// amortized compaction slides, a PARTIAL drain that dies with head > 0,
// and pop-to-empty-then-push.
#[test]
fn asan_deque_head_frees_base_once() {
    assert_clean_asan_run(
        r#"
fn main() {
    let mut n = 0i64;
    let mut it = 0i64;
    while it < 200i64 {
        let mut q: VecDeque[i64] = VecDeque.new();
        let mut i = 0i64;
        while i < 64i64 {
            q.push_back(i);
            i = i + 1i64;
        }
        while not q.is_empty() {
            match q.pop_front() { Some(_) => {} None => {} }
        }
        n = n + 1i64;

        let mut p: VecDeque[i64] = VecDeque.new();
        let mut j = 0i64;
        while j < 32i64 {
            p.push_back(j);
            j = j + 1i64;
        }
        match p.pop_front() { Some(_) => {} None => {} }
        match p.pop_front() { Some(_) => {} None => {} }
        n = n + 1i64;

        let mut d: VecDeque[i64] = VecDeque.new();
        d.push_back(1i64);
        d.push_back(2i64);
        while not d.is_empty() {
            match d.pop_front() { Some(_) => {} None => {} }
        }
        d.push_back(3i64);
        n = n + 1i64;

        it = it + 1i64;
    }
    println(n);
}
"#,
        &["600"],
        "deque_head_frees_base_once",
    );
}

/// B-2026-08-16-7, memory half. The fix makes EVERY reused consume of a
/// binding defensively copy, not just the first — which multiplies
/// ownership: after `let cur = e.doc; … let after = e.doc;` there are
/// THREE live owners of that heap shape (the source and two copies),
/// where the value assertions alone cannot tell a correct three-owner
/// balance from a copy that aliased (double free at the three scope
/// exits) or a source whose cleanup was disarmed one time too many
/// (leak). Every ingredient of the row's repro is kept — the `Ed`
/// wrapper, the `mut ref` field call between the two moves — because its
/// reduction record shows each is needed, plus the minimal
/// three-consumes shape and a loop that re-runs a reused-move pair so a
/// per-iteration imbalance accumulates.
#[test]
fn asan_every_reused_consume_copy_is_balanced() {
    assert_clean_asan_run(
        r#"
enum Cmd { Clear(Vec[String]) }
struct Doc { lines: Vec[String] }
struct Ed { doc: Doc }
fn apply(d: mut ref Doc, c: Cmd) -> Cmd {
    match c {
        Clear(old) => {
            let mut snap: Vec[String] = Vec.new();
            for i in 0..d.lines.len() { snap.push(d.lines[i]); }
            d.lines.clear();
            for i in 0..old.len() { d.lines.push(old[i]); }
            Cmd.Clear(snap)
        }
    }
}
fn render(d: ref Doc) -> String {
    let mut s = String.new();
    for i in 0..d.lines.len() { s.push_str(d.lines[i]); }
    s
}
fn eat(s: String) -> i64 { return s.len(); }
fn main() {
    let mut l: Vec[String] = Vec.new();
    let mut a = String.new(); a.push_str("ALPHAALPHAALPHA");
    l.push(a);
    let mut e = Ed { doc: Doc { lines: l } };
    let mut snapshot: Vec[String] = Vec.new();
    let cur = e.doc;
    for i in 0..cur.lines.len() { snapshot.push(cur.lines[i]); }
    let _inv = apply(mut e.doc, Cmd.Clear(snapshot));
    let after = e.doc;
    println(f"[{render(e.doc)}] lines={after.lines.len()}");

    let b = "betabetabetabeta";
    let x = eat(b);
    let y = eat(b);
    let z = eat(b);
    println(f"{x} {y} {z} {b.len()}");

    let mut k = 0;
    let mut t = 0;
    while k < 10 {
        let mut v: Vec[String] = Vec.new();
        v.push("gammagammagamma");
        let first = v;
        let second = v;
        t = t + first.len() + second.len();
        k = k + 1;
    }
    println(f"{t}");
    println("end");
}
"#,
        &["[ALPHAALPHAALPHA] lines=1", "16 16 16 16", "20", "end"],
        "asan_every_reused_consume_copy_is_balanced",
    );
}

/// The `--release` half of the same property. Stripping removes the
/// diagnostic, NOT the expression, so the ownership handling has to run in
/// the stripped build too — an early return that skipped it made
/// `karac build --release` abort on a program whose debug build was clean.
/// Exercised here through the same ASAN harness with `KARAC_STRIP_DBG=1`,
/// which is what `karac build --release` sets.
#[test]
fn test_dbg_stripped_build_keeps_ownership_handling() {
    struct StripGuard;
    impl Drop for StripGuard {
        fn drop(&mut self) {
            std::env::remove_var("KARAC_STRIP_DBG");
        }
    }
    std::env::set_var("KARAC_STRIP_DBG", "1");
    let _g = StripGuard;
    assert_clean_asan_run(
        r#"
fn main() {
    let mut vs: Vec[i64] = Vec.new();
    vs.push(1_i64);
    dbg(vs);
    let vs2 = dbg(vs);
    println(f"{vs2.len()}");
    let hs = "y".to_uppercase();
    let hs2 = dbg(hs);
    println(hs2);
}
"#,
        &["1", "Y"],
        "dbg_stripped_ownership",
    );
}

/// B-2026-09-08-13 — a LET-BOUND LOCAL of a SELF-REFERENTIAL struct passed
/// BY VALUE into a callee that never hands it on lost its `Drop` body and
/// leaked its heap, because `move_declined_copy_struct_arg` retracted the
/// caller's cleanup on a MAY-analysis it never checked against the actual
/// callee.
///
/// WHAT THIS FIXTURE PINS. On Linux CI this harness runs LeakSanitizer, so
/// cell 1 pins the LEAK half directly — the 3-byte `tag` buffer the parent
/// stranded (measured by hand at `-O0` and `-O2`: 11 allocs / 10 frees,
/// `3 bytes in 1 blocks` definitely lost, against 11 / 11 clean after). On
/// macOS `-fsanitize=address` runs no LSan and the leak is invisible here,
/// which is why the E2E twin in `tests/codegen.rs` asserts the missing
/// `dN10` line as well — that half is architecture-independent.
///
/// Cells 2-4 are the hazard controls for the NARROWING, and they are the
/// half this harness catches everywhere: a gate that restored the caller's
/// drop for a callee that DOES take the value over would abort as a double
/// free at any opt level, on any platform. They were clean before this
/// change and must stay clean.
#[test]
fn asan_declined_copy_arg_keeps_exactly_one_owner() {
    const PRE: &str = "struct Node { id: i64, next: Option[Node], tag: String }\n\
             impl Drop for Node { fn drop(mut ref self) { println(f\"dN{self.id}\") } }\n\
             fn mkn(i: i64) -> Node { return Node { id: i, next: Option.None, tag: f\"t{i}\" }; }\n";
    // 1 — the row's cell: the callee reads and drops, so the CALLER owns it.
    assert_clean_asan_run_no_auto_par(
        &format!(
            "{PRE}fn read(n: Node) -> i64 {{ return n.id; }}\n\
                 fn main() {{ let c = mkn(10); println(f\"v{{read(c)}}\"); println(\"end\") }}\n"
        ),
        &["v10", "dN10", "end"],
        "b13-letbound",
    );
    // 2 — HAZARD: the callee always returns it. The caller must still stand
    // down, or the result binding and the argument binding both own it.
    assert_clean_asan_run_no_auto_par(
            &format!(
                "{PRE}fn pass(n: Node) -> Node {{ return n; }}\n\
                 fn main() {{ let c = mkn(10); let d = pass(c); println(f\"v{{d.id}}\"); println(\"end\") }}\n"
            ),
            &["v10", "dN10", "end"],
            "b13-returns-hazard",
        );
    // 3 — HAZARD: the callee stores it into a container the caller holds.
    assert_clean_asan_run_no_auto_par(
            &format!(
                "{PRE}fn stash(n: Node, v: mut ref Vec[Node]) {{ v.push(n); }}\n\
                 fn main() {{ let mut v: Vec[Node] = Vec.new(); let c = mkn(10); stash(c, mut v); println(f\"n{{v.len()}}\"); println(\"end\") }}\n"
            ),
            &["n1", "dN10", "end"],
            "b13-stores-hazard",
        );
    // 4 — HAZARD: the callee stores it on SOME paths. The miss path is the
    // one taken here, and it is the path with no second owner to balance a
    // restored caller drop against.
    assert_clean_asan_run_no_auto_par(
            &format!(
                "{PRE}fn cs(n: Node, k: bool, v: mut ref Vec[Node]) {{ if k {{ v.push(n); }} }}\n\
                 fn main() {{ let mut v: Vec[Node] = Vec.new(); let c = mkn(10); cs(c, false, mut v); println(f\"n{{v.len()}}\"); println(\"end\") }}\n"
            ),
            &["dN10", "n0", "end"],
            "b13-cond-store-hazard",
        );
}

/// B-2026-09-12-11's LEAK half — the QUALIFIED constructor spelling at an
/// argument position was invisible to BOTH argument-freshness predicates,
/// and the envelope one loses memory rather than a `Drop` body.
///
/// `optres_arg_mints_field_envelope` matched only `ExprKind::Call`, so a
/// qualified `cls(Result[W, i64].Ok(W { o: Option.Some(Option.Some(i)) }))`
/// minted a 32-byte `coerce_to_payload_words` envelope per call that no
/// frame owned: measured 128 B definitely lost in 4 blocks over four calls
/// at `KARAC_OPT_LEVEL=0`, against 0 bytes and all blocks freed for the
/// BARE `cls(Ok(W { .. }))` spelling of the same program. The output is
/// identical either way, which is why this half needs a sanitizer cell and
/// the body half in `tests/codegen.rs` does not.
///
/// CELL 1 IS AN `-O0` CELL, and that was measured rather than assumed: on a
/// deliberately reverted tree this test failed on cell 3, not cell 1, so at
/// this harness's default opt level LLVM deletes an envelope nothing
/// observes and LSan sees nothing to report. The cell earns its keep on
/// `scripts/asan-o0-leg.sh`, which re-runs this whole suite at
/// `KARAC_OPT_LEVEL=0` — the same reason CLAUDE.md calls that leg the place
/// a leak-class cell is actually measured. Cell 3's buffer is large enough
/// to survive the optimizer, which is why the pre-fix failure surfaced
/// there: 276 B in 9 allocations under LSan, 144 B direct in 3 blocks under
/// valgrind at `-O0`.
///
/// Cells 3 and 4 are the DOUBLE-FREE direction of the same admission,
/// because `optres_arg_is_unowned_temp` now answers `true` for this
/// spelling and that hands the caller ownership of a real heap buffer. A
/// payload the callee copies in must be freed exactly once (cell 3); one the
/// callee STORES somewhere that outlives the call must be freed by the
/// store's owner and not here as well (cell 4). Both abort under ASAN if
/// the admission reaches a shape the escape gate should have excluded.
#[test]
fn asan_qualified_ctor_argument_envelope_has_an_owner() {
    // 1 — the row's leak: 128 B in 4 blocks before the fix.
    assert_clean_asan_run(
        "struct W { o: Option[Option[i64]] }\n\
             fn cls(x: Result[W, i64]) {\n\
             \x20   match x { Ok(w) => { println(\"ok\") } Err(e) => { println(\"er\") } }\n\
             }\n\
             fn main() {\n\
             \x20   for i in 0..4 {\n\
             \x20       cls(Result[W, i64].Ok(W { o: Option.Some(Option.Some(i)) }));\n\
             \x20   }\n\
             \x20   println(\"end\");\n\
             }\n",
        &["ok", "ok", "ok", "ok", "end"],
        "b11-qualified-ctor-field-envelope",
    );
    // 2 — CONTROL: the BARE spelling of cell 1, clean before and after.
    assert_clean_asan_run(
        "struct W { o: Option[Option[i64]] }\n\
             fn cls(x: Result[W, i64]) {\n\
             \x20   match x { Ok(w) => { println(\"ok\") } Err(e) => { println(\"er\") } }\n\
             }\n\
             fn main() {\n\
             \x20   for i in 0..4 {\n\
             \x20       cls(Ok(W { o: Option.Some(Option.Some(i)) }));\n\
             \x20   }\n\
             \x20   println(\"end\");\n\
             }\n",
        &["ok", "ok", "ok", "ok", "end"],
        "b11-bare-ctor-field-envelope-control",
    );
    // 3 — a real heap payload behind the qualified spelling. The caller now
    //     owns this `{ptr,len,cap}` buffer, so a second owner shows up here
    //     as a double free rather than as a leak.
    assert_clean_asan_run(
            "fn show(x: Option[Vec[String]]) {\n\
             \x20   match x { Some(v) => { println(f\"n:{v.len()}\") } None => { println(\"n\") } }\n\
             }\n\
             fn main() {\n\
             \x20   for i in 0..3 {\n\
             \x20       let vs: Vec[String] = [f\"aaaaaaaaaaaaaaaaaaaa-{i}\", f\"bbbbbbbbbbbbbbbbbbbb-{i}\"];\n\
             \x20       show(Option[Vec[String]].Some(vs));\n\
             \x20   }\n\
             \x20   println(\"end\");\n\
             }\n",
            &["n:2", "n:2", "n:2", "end"],
            "b11-qualified-ctor-heap-payload",
        );
    // 4 — the ESCAPE shape: the callee pushes the argument into an
    //     accumulator that outlives the call, so the caller must NOT own it.
    assert_clean_asan_run(
            "fn keep(x: Option[Vec[String]], acc: mut ref Vec[Option[Vec[String]]]) {\n\
             \x20   acc.push(x);\n\
             \x20   println(\"k\");\n\
             }\n\
             fn main() {\n\
             \x20   let mut acc: Vec[Option[Vec[String]]] = [];\n\
             \x20   for i in 0..3 {\n\
             \x20       let vs: Vec[String] = [f\"cccccccccccccccccccc-{i}\", f\"dddddddddddddddddddd-{i}\"];\n\
             \x20       keep(Option[Vec[String]].Some(vs), mut acc);\n\
             \x20   }\n\
             \x20   println(f\"len:{acc.len()}\");\n\
             }\n",
            &["k", "k", "k", "len:3"],
            "b11-qualified-ctor-escaping-arg",
        );
}

/// B-2026-09-21-4 — one binding passed twice by value in a single call,
/// under ASAN.
///
/// This is the memory half of
/// `test_e2e_one_binding_passed_twice_by_value_in_one_call`. The defect
/// was a `free(): double free detected in tcache 2` on the generic
/// spelling and a SIGSEGV on the concrete one, so a sanitiser cell is the
/// instrument that speaks to its class directly rather than through
/// stdout: an output comparison alone cannot tell a correct program from
/// one that happens to print correctly before corrupting the heap.
///
/// The cells include the two broken shapes, three aliased arguments, an
/// alias with a GAP between its occurrences (`three(g, h, g)`), and three
/// controls that were already correct — distinct bindings, a single
/// argument, and two sequential calls. Measured outside the harness at 53
/// allocs / 53 frees with 0 valgrind errors, which is why no minimum-alloc
/// floor had to be relaxed for it.
#[test]
fn asan_one_binding_passed_twice_by_value_in_one_call() {
    assert_clean_asan_run(
        r#"
enum G1[T] { Y(T), N }

fn one[T](a: G1[T]) {
    match a { G1.Y(v) => { println(f"  1x {v}") } G1.N => { println("  1x NONE") } }
}
fn two[T](a: G1[T], b: G1[T]) {
    match a { G1.Y(v) => { println(f"  ax {v}") } G1.N => { println("  ax NONE") } }
    match b { G1.Y(v) => { println(f"  bx {v}") } G1.N => { println("  bx NONE") } }
}
fn three[T](a: G1[T], b: G1[T], c: G1[T]) {
    match a { G1.Y(v) => { println(f"  ax {v}") } G1.N => { println("  ax NONE") } }
    match b { G1.Y(v) => { println(f"  bx {v}") } G1.N => { println("  bx NONE") } }
    match c { G1.Y(v) => { println(f"  cx {v}") } G1.N => { println("  cx NONE") } }
}
fn twoc(a: G1[String], b: G1[String]) {
    match a { G1.Y(v) => { println(f"  ax {v}") } G1.N => { println("  ax NONE") } }
    match b { G1.Y(v) => { println(f"  bx {v}") } G1.N => { println("  bx NONE") } }
}

fn a_gen_dup() { println("a"); let g: G1[String] = G1.Y(f"pa"); two(g, g) }
fn b_con_dup() { println("b"); let g: G1[String] = G1.Y(f"pb"); twoc(g, g) }
fn c_triple() { println("c"); let g: G1[String] = G1.Y(f"pc"); three(g, g, g) }
fn d_distinct() { println("d"); let g: G1[String] = G1.Y(f"pd"); let h: G1[String] = G1.Y(f"qd"); two(g, h) }
fn e_single() { println("e"); let g: G1[String] = G1.Y(f"pe"); one(g) }
fn f_twocalls() { println("f"); let g: G1[String] = G1.Y(f"pf"); one(g); one(g) }
fn g_middle() { println("g"); let g: G1[String] = G1.Y(f"pg"); let h: G1[String] = G1.Y(f"qg"); three(g, h, g) }

fn main() {
    a_gen_dup();
    b_con_dup();
    c_triple();
    d_distinct();
    e_single();
    f_twocalls();
    g_middle();
    println("end");
}
"#,
        &[
            "a", "  ax pa", "  bx pa", "b", "  ax pb", "  bx pb", "c", "  ax pc", "  bx pc",
            "  cx pc", "d", "  ax pd", "  bx qd", "e", "  1x pe", "f", "  1x pf", "  1x pf", "g",
            "  ax pg", "  bx qg", "  cx pg", "end",
        ],
        "b2021-4-alias-args",
    );
}

/// B-2026-09-23-33 — a program under test that does not PARSE fails the
/// fixture. It used to return `None` from the harness, which every
/// `assert_clean_asan_run*` helper reports as missing setup and skips, so a
/// fixture with a typo passed while asserting nothing. The parse runs before
/// any toolchain check, so this pin holds on a host without ASAN too.
#[test]
#[should_panic(expected = "PARSE FAILED")]
fn asan_harness_parse_error_fails_the_fixture() {
    let _ = run_under_asan(
        "fn main() { let x = 1 && 2; println(f\"{x}\"); }",
        "asan_harness_parse_error_fails_the_fixture",
    );
}
