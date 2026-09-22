//! files, processes, CLI, environment, sockets, time -- fixtures for `tests/codegen.rs`.
//!
//! Split out of `tests/codegen.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test codegen` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test codegen io_runtime::
//!
//! New fixtures about files, processes, CLI, environment, sockets, time belong in this file.

use super::*;

/// B-2026-08-25-2 — E2E: a real `std.cli` program, compiled and run.
///
/// The row's repro was three lines that died at method dispatch. This is
/// the shape the row actually cares about — the builder chain, `parse()`'s
/// argv loop, and the generated help text — because that is what a shipped
/// CLI tool runs.
///
/// It ROUND-TRIPS STRING VALUES BACK OUT (`help_text` embeds the program
/// name, the about text, and each arg's name and help). That is deliberate
/// and is the lesson of this row's two false-negative probes: a cli program
/// that never reads a field back printed the right answer even against the
/// broken compiler, because every reference was consistently `i64`-wide and
/// the collapse was unobservable. Same trap as B-2026-08-25-15's string
/// literals.
///
/// No argv is passed (the harness runs the binary bare), so the required
/// arg is missing and `parse()` takes its `Err` path — deterministic, and
/// still a full traversal of the `Vec[ArgEntry]` / `Vec[FlagEntry]`
/// machinery.
#[test]
fn e2e_stdlib_cli_parses_and_reports_end_to_end() {
    let src = r#"
fn main() {
    let p = Parser.new("greet")
        .about("greets someone")
        .version("2.1")
        .arg("--name", Arg.string().required().help("who to greet"))
        .flag("--loud", 'l', "shout it");
    match p.parse() {
        Ok(args) => { println("parsed"); }
        Err(e) => { println(f"err {e.message}"); }
    }
    println(p.version_line());
    let h = p.help_text();
    if h.contains("greets someone") { println("about-ok"); } else { println("about-BAD"); }
    if h.contains("who to greet") { println("help-ok"); } else { println("help-BAD"); }
}
"#;
    let out = run_program(src);
    if let Some(out) = out {
        assert_eq!(
            out, "err missing required argument\ngreet 2.1\nabout-ok\nhelp-ok\n",
            "compiled std.cli program diverged"
        );
    }
}

/// B-2026-08-25-2 — the body half: CALLING a `std.cli` method compiles.
///
/// This is the row's headline, and it replaces an earlier pin that
/// asserted the opposite (that the call failed loudly, naming the gap).
/// That assertion was correct for the layout-only state and is now false
/// by construction: `program_uses_cli` sees `Arg` in the program's
/// `referenced_type_names` and compiles cli's bodies, so both the
/// associated fn and the instance method resolve to real definitions.
///
/// Compiling to IR is the whole assertion. `Arg.string()` used to take the
/// associated-call dispatcher's `i64 0` last resort, and `a.required()`
/// used to die at method dispatch with `no handler for method 'required'
/// on variable 'a'` — the error the row was filed on.
#[test]
fn stdlib_cli_method_call_compiles() {
    let mut parsed = karac::parse(
        "fn main() { let a = Arg.string(); let b = a.required(); println(\"ok\"); }\n",
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
    compile_to_ir(&parsed.program, None, None)
        .expect("a std.cli associated fn + instance method must compile");
}

/// B-2026-08-25-2 — the gate is a GATE: a program that never mentions a
/// `std.cli` type must not drag cli's bodies into its module.
///
/// The IR-shape tests (`test_ir_return_struct_field_vec_element_emits_clone`,
/// `wrapping_arith_lowers_without_overflow_trap`) already fail if cli is
/// compiled unconditionally — both were measured failing that way — but
/// they assert it indirectly, through the absence of a clone call and an
/// overflow trap. This asserts the gate's own decision directly, so a
/// change that widens `program_uses_cli` is caught here by name rather
/// than surfacing as two unrelated-looking IR assertions.
#[test]
fn stdlib_cli_bodies_stay_out_of_a_cli_free_program() {
    let mut parsed = karac::parse("fn main() { let x = 1 + 2; println(f\"{x}\"); }\n");
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    karac::prepare_for_resolve(&mut parsed.program);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);
    let ir = compile_to_ir(&parsed.program, None, None).expect("cli-free program must compile");
    for m in ["Parser_parse", "Parser_help_text", "Arg_required"] {
        assert!(
            !ir.contains(m),
            "cli body `{m}` leaked into a program that never mentions a cli type"
        );
    }
}

/// B-2026-09-02-4 — a by-value `Drop` param moved into a returned
/// AGGREGATE LITERAL on some exits and absent from the others
/// (`fn fmake(r: R, k: bool) -> Box2 { if k { return Box2 { r: mr(77) };
/// } return Box2 { r: r }; }`) has one owner per path on every surface:
/// the callee's per-path flip when the value dies inside, the caller's
/// result binding when it comes back wrapped. `fn_conditionally_returns_
/// param_bare` now admits the wrap as a hand-over (`yields_wrapped`), the
/// same shape `fn_always_returns_param` and both tail-source walkers
/// already recognised. Before: the free fresh-temp spelling LOST the
/// dies-inside body on all four surfaces, the named and the associated /
/// method spellings ran the hand-back body TWICE compiled, and the
/// interpreter lost it for the associated spelling — the row's
/// "both directions at once". Cells: free / associated / method spellings
/// of a `Drop`-bearing wrapper and a plain one, a tuple wrap, the tail
/// spelling, a nested return on all three path combinations, a two-level
/// wrap on its dies-inside path, a unit-variant vs dying param, and named
/// arguments, each on both `k` values. Interpreter twin:
/// `test_param_wrapped_in_returned_aggregate_on_some_paths_has_one_owner`.
#[test]
fn e2e_param_wrapped_in_returned_aggregate_on_some_paths_has_one_owner() {
    assert_eq!(
            run_program(
                r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct Box2 { r: R }
impl Drop for Box2 { fn drop(mut ref self) { println(f"B{self.r.id}") } }
struct P2 { r: R, n: i64 }
enum Slot { Held(R), Empty }
struct H { n: i64 }
fn mk(i: i64) -> String { return f"pay-{i}-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"; }
fn mr(i: i64) -> R { return R { id: i, s: mk(i) }; }
fn fmake(r: R, k: bool) -> Box2 { if k { return Box2 { r: mr(77) }; } return Box2 { r: r }; }
fn fplain(r: R, k: bool) -> P2 { if k { return P2 { r: mr(78), n: 1 }; } return P2 { r: r, n: 2 }; }
fn ftup(r: R, k: bool) -> (R, i64) { if k { return (mr(79), 1); } return (r, 2); }
fn fslot(r: R, k: bool) -> Slot { if k { return Slot.Empty; } return Slot.Held(r); }
fn ftail(r: R, k: bool) -> P2 { if k { P2 { r: mr(80), n: 1 } } else { P2 { r: r, n: 2 } } }
fn fnest(r: R, k: bool, j: bool) -> P2 { if k { if j { return P2 { r: r, n: 3 }; } } return P2 { r: mr(81), n: 1 }; }
fn ftwo(r: R, k: bool) -> Box2 { if k { return Box2 { r: mr(82) }; } let p = P2 { r: r, n: 1 }; return Box2 { r: p.r }; }
impl H {
    fn amake(r: R, k: bool) -> Box2 { if k { return Box2 { r: mr(87) }; } return Box2 { r: r }; }
    fn aplain(r: R, k: bool) -> P2 { if k { return P2 { r: mr(88), n: 1 }; } return P2 { r: r, n: 2 }; }
    fn mmake(ref self, r: R, k: bool) -> Box2 { if k { return Box2 { r: mr(89) }; } return Box2 { r: r }; }
}
fn main() {
    let h = H { n: 0 };
    println("ft"); { let x = fmake(mr(55), true); println(f"C{x.r.id}"); }
    println("ff"); { let x = fmake(mr(53), false); println(f"C{x.r.id}"); }
    println("at"); { let x = H.amake(mr(56), true); println(f"C{x.r.id}"); }
    println("af"); { let x = H.amake(mr(57), false); println(f"C{x.r.id}"); }
    println("pt"); { let x = fplain(mr(58), true); println(f"C{x.r.id}"); }
    println("pf"); { let x = fplain(mr(59), false); println(f"C{x.r.id}"); }
    println("apt"); { let x = H.aplain(mr(60), true); println(f"C{x.r.id}"); }
    println("apf"); { let x = H.aplain(mr(61), false); println(f"C{x.r.id}"); }
    println("tt"); { let x = ftup(mr(62), true); println(f"C{x.0.id}"); }
    println("tf"); { let x = ftup(mr(63), false); println(f"C{x.0.id}"); }
    println("st"); { let x = fslot(mr(64), true); match x { Slot.Held(v) => println(f"C{v.id}"), Slot.Empty => println("CE") } }
    println("tlt"); { let x = ftail(mr(66), true); println(f"C{x.r.id}"); }
    println("tlf"); { let x = ftail(mr(67), false); println(f"C{x.r.id}"); }
    println("ntt"); { let x = fnest(mr(68), true, true); println(f"C{x.r.id}"); }
    println("ntf"); { let x = fnest(mr(69), true, false); println(f"C{x.r.id}"); }
    println("nff"); { let x = fnest(mr(70), false, false); println(f"C{x.r.id}"); }
    println("twt"); { let x = ftwo(mr(71), true); println(f"C{x.r.id}"); }
    println("mt"); { let x = h.mmake(mr(73), true); println(f"C{x.r.id}"); }
    println("mf"); { let x = h.mmake(mr(74), false); println(f"C{x.r.id}"); }
    println("nt"); { let a = mr(75); let x = fmake(a, true); println(f"C{x.r.id}"); }
    println("nf"); { let b = mr(76); let x = fmake(b, false); println(f"C{x.r.id}"); }
    println("end");
}"#
            ),
            Some("ft\nd55\nC77\nB77\nd77\nff\nC53\nB53\nd53\nat\nd56\nC87\nB87\nd87\naf\nC57\nB57\nd57\npt\nd58\nC78\nd78\npf\nC59\nd59\napt\nd60\nC88\nd88\napf\nC61\nd61\ntt\nd62\nC79\nd79\ntf\nC63\nd63\nst\nd64\nCE\ntlt\nd66\nC80\nd80\ntlf\nC67\nd67\nntt\nC68\nd68\nntf\nd69\nC81\nd81\nnff\nd70\nC81\nd81\ntwt\nd71\nC82\nB82\nd82\nmt\nd73\nC89\nB89\nd89\nmf\nC74\nB74\nd74\nnt\nd75\nC77\nB77\nd77\nnf\nC76\nB76\nd76\nend\n".to_string()),
            "a param wrapped in a returned aggregate on some paths has one owner per path"
        );
}

/// B-2026-09-07-22 — the METHOD and ASSOC-FN spelling of B-2026-09-07-15's
/// shape: `h.pick2(mk(1), false)` over
/// `impl Hold { fn pick2(ref self, r: R, k: bool) -> R { if k { return mk(90); }
/// return fwd(r); } }` aborted `free(): double free detected in tcache 2` on all
/// four compiled surfaces while `--interp` was correct.
///
/// Closed by the SAME commit, without a line of method-specific code: the two
/// halves that fix it are the shared predicate
/// (`fn_conditionally_returns_param_bare` recognising the one-hop leaf, which is
/// what admits the callee to the per-path flip on every call leg at once) and
/// the per-path disarm at the hand-off INSIDE that callee — where `fwd(r)` is a
/// free-function call whatever spelling reached `pick2`. B-2026-09-07-4 had
/// already given the method and assoc legs the flip itself.
///
/// That is worth a pin of its own rather than a note, because the row predicted
/// the opposite — that the flag-clearing keys on `ExprKind::Identifier` and so
/// could not see a `return fwd(r)` tail. It does not have to: the clearing
/// happens at the ARGUMENT hand-off, one frame down.
///
/// Cells: both legs of the method hop, both legs of the assoc hop, the no-hop
/// method control, and a named local into the method hop.
///
/// Twin of `tests/interpreter.rs`'s `test_mixed_path_hop_on_a_method`, pinned to the same string.
#[test]
fn e2e_mixed_path_hop_on_a_method() {
    let Some(out) = run_program(
        r#"shared struct Inner { v: i64 }
struct R { id: i64, name: String, inner: Inner }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}", inner: Inner { v: i } }; }
fn fwd(r: R) -> R { return r; }
struct Hold { n: i64 }
impl Hold { fn pick2(ref self, r: R, k: bool) -> R { if k { return mk(90); } return fwd(r); } }
impl R { fn picka2(r: R, k: bool) -> R { if k { return mk(91); } return fwd(r); } }
impl Hold { fn pick(ref self, r: R, k: bool) -> R { if k { return mk(92); } return r; } }
fn main() {
  let h = Hold { n: 0 };
  println("method_hop_handback"); let a = h.pick2(mk(1), false); println(f"  v={a.inner.v}");
  println("method_hop_dies"); let b = h.pick2(mk(2), true); println(f"  v={b.id}");
  println("assoc_hop_handback"); let c = R.picka2(mk(3), false); println(f"  v={c.inner.v}");
  println("assoc_hop_dies"); let d = R.picka2(mk(4), true); println(f"  v={d.id}");
  println("method_nohop_handback"); let e = h.pick(mk(5), false); println(f"  v={e.inner.v}");
  println("named_into_method_hop"); let g = mk(6); let n = h.pick2(g, false); println(f"  v={n.inner.v}");
  println("end");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"method_hop_handback
  v=1
  dR1
method_hop_dies
  dR2
  v=90
  dR90
assoc_hop_handback
  v=3
  dR3
assoc_hop_dies
  dR4
  v=91
  dR91
method_nohop_handback
  v=5
  dR5
named_into_method_hop
  v=6
  dR6
end
"#
    );
}

/// B-2026-09-07-15 — a MIXED-PATH callee that hands its argument back THROUGH
/// one further call (`fn mvia(r: R, c: bool) -> R { if c { return f(r); }
/// return mk(90); }`) double-freed on its hand-back leg: `free(): double free
/// detected in tcache 2` on every compiled surface, with `--interp` running the
/// body twice for one object. The same callee with NO hop and the same hop with
/// no branch were both already correct — the intersection of B-2026-09-06-69
/// and B-2026-09-07-10 was claimed by neither.
///
/// Two halves, and they have to move together.
/// `fn_conditionally_returns_param_bare`'s leaf test now recognises the one-hop
/// hand-back (asking the INNER callee the all-paths question, so a chain cannot
/// launder a mixed-path callee through a passthrough), which is what admits the
/// function to the per-path mechanism at all. And the hand-off INSIDE that
/// callee now disarms per path instead of statically: `f(r)` sits in a branch,
/// so retracting `r`'s registration outright took the body away from the exit
/// where the value never reached the call. Clearing the per-path flag in the
/// branch's own block is the disarm `arm_conditional_store_flag` performs for a
/// conditional store.
///
/// The per-path disarm sits ABOVE the copy-class split for the same reason in
/// both classes: measured, a declined-copy param lost its dies-inside body and
/// leaked 2 blocks at -O0, and a COPY-SUPPORTED one ran the callee's guarded
/// body on top of the result binding's on the hand-back leg (`dP7 v=7 dP7`).
///
/// Cells: both legs of the hop, named and fresh-temp, both legs of the no-hop
/// control, and both legs of a copy-supported hop.
///
/// Twin of `tests/interpreter.rs`'s `test_mixed_path_hand_back_through_a_hop`, pinned to the same string.
#[test]
fn e2e_mixed_path_hand_back_through_a_hop() {
    let Some(out) = run_program(
        r#"shared struct Inner { v: i64 }
struct R { id: i64, name: String, inner: Inner }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
struct P { id: i64, name: String, xs: Vec[i64] }
impl Drop for P { fn drop(mut ref self) { println(f"  dP{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}", inner: Inner { v: i } }; }
fn mkp(i: i64) -> P { return P { id: i, name: f"p{i}", xs: [i] }; }
fn f(r: R) -> R { return r; }
fn pf(p: P) -> P { return p; }
fn mvia(r: R, c: bool) -> R { if c { return f(r); } return mk(90); }
fn cvia(r: R, c: bool) -> R { if c { return r; } return mk(91); }
fn pvia(p: P, c: bool) -> P { if c { return pf(p); } return mkp(92); }
fn main() {
  println("hop_handback"); let a = mk(1); let z = mvia(a, true); println(f"  v={z.inner.v}");
  println("hop_dies_inside"); let b = mk(2); let y = mvia(b, false); println(f"  v={y.id}");
  println("hop_handback_fresh"); let w = mvia(mk(3), true); println(f"  v={w.inner.v}");
  println("hop_dies_fresh"); let x = mvia(mk(4), false); println(f"  v={x.id}");
  println("nohop_handback"); let c = mk(5); let u = cvia(c, true); println(f"  v={u.inner.v}");
  println("nohop_dies_inside"); let d = mk(6); let t = cvia(d, false); println(f"  v={t.id}");
  println("copyable_hop_handback"); let e = mkp(7); let s = pvia(e, true); println(f"  v={s.id}");
  println("copyable_hop_dies"); let g = mkp(8); let r = pvia(g, false); println(f"  v={r.id}");
  println("end");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"hop_handback
  v=1
  dR1
hop_dies_inside
  dR2
  v=90
  dR90
hop_handback_fresh
  v=3
  dR3
hop_dies_fresh
  dR4
  v=90
  dR90
nohop_handback
  v=5
  dR5
nohop_dies_inside
  dR6
  v=91
  dR91
copyable_hop_handback
  v=7
  dP7
copyable_hop_dies
  dP8
  v=92
  dP92
end
"#
    );
}

#[test]
/// B-2026-09-07-3, the VIA-CALL half — the spelling
/// `test_e2e_method_and_assoc_mixed_path_hand_back_owns_its_argument`'s own
/// doc named as NOT covered. A MIXED-PATH callee that hands its by-value
/// param back through ONE HOP (`if k { return mk(92); } return fwd(r); }`)
/// leaked 19 bytes in 2 blocks on the DIES-INSIDE leg and lost the argument's
/// `Drop` body outright — `dR40` never fired, on every compiled surface,
/// while `--interp` printed it. Body AND bytes, so no output assertion and no
/// leak gate caught it alone.
///
/// It took two independent halves to close, which is why it is pinned here
/// rather than with either one. `6ef13bb` (B-2026-09-06-69) moved the memory
/// onto the callee's per-path registration for the class whose prologue
/// declines to own it, and stood the caller all the way down. That fixed the
/// BARE spelling and left this one still silent in both frames:
/// `fn_conditionally_returns_param_bare`'s condition 3 declined a leaf that
/// reaches the param through a call, because `is_bare` did not recognise a
/// plain call as a hand-out — so the callee registered nothing while the
/// caller had already stood down through `fn_returns_param_via_call`.
/// `99bd72d` (B-2026-09-07-15/-22) taught that leaf test the one-hop
/// hand-back and made the hand-off inside the callee disarm PER PATH.
///
/// Both legs and both argument spellings are covered because the two failures
/// are opposite and each masks the other: the dies-inside leg is where the
/// memory strands, the escaping leg is where an over-eager owner would double
/// free. The method and assoc twins ride along — the predicate is resolved by
/// name through `Item::Function`, so a method reaches it by a different route
/// and has regressed independently before (B-2026-09-07-4).
///
/// Note the ORIENTATION: the hand-back is on the FALLTHROUGH and the fresh
/// value in the branch, the mirror of `e2e_mixed_path_hand_back_through_a_hop`.
/// The per-path flag is armed in a different basic block each way round.
fn test_e2e_free_fn_mixed_path_hand_back_through_a_hop_owns_its_argument() {
    let Some(out) = run_program(
        r#"shared struct Inner { v: i64 }
struct R { id: i64, name: String, inner: Inner }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}", inner: Inner { v: i } }; }
fn fwd(r: R) -> R { return r; }
struct Hold { n: i64 }
impl Hold { fn pv(ref self, r: R, k: bool) -> R { if k { return mk(90); } return fwd(r); } }
impl R { fn av(r: R, k: bool) -> R { if k { return mk(91); } return fwd(r); } }
fn fv(r: R, k: bool) -> R { if k { return mk(92); } return fwd(r); }
// free fn via-call, ESCAPING leg then DIES-INSIDE leg, fresh temp
fn c1() { let z = fv(mk(11), false); println(f"c1={z.id}"); }
fn c2() { let z = fv(mk(12), true);  println(f"c2={z.id}"); }
// the same two legs with a NAMED LOCAL argument -- the caller-side retraction
fn c3() { let a = mk(13); let z = fv(a, false); println(f"c3={z.id}"); }
fn c4() { let a = mk(14); let z = fv(a, true);  println(f"c4={z.id}"); }
// method twin, both legs
fn c5() { let h = Hold { n: 1 }; let z = h.pv(mk(15), false); println(f"c5={z.id}"); }
fn c6() { let h = Hold { n: 1 }; let z = h.pv(mk(16), true);  println(f"c6={z.id}"); }
// assoc twin, both legs
fn c7() { let z = R.av(mk(17), false); println(f"c7={z.id}"); }
fn c8() { let z = R.av(mk(18), true);  println(f"c8={z.id}"); }
fn main() { c1(); c2(); c3(); c4(); c5(); c6(); c7(); c8(); println("end"); }
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"c1=11
dR11
dR12
c2=92
dR92
c3=13
dR13
dR14
c4=92
dR92
c5=15
dR15
dR16
c6=90
dR90
c7=17
dR17
dR18
c8=91
dR91
end
"#
    );
}

/// B-2026-07-30-5 (ineligible leg) — deques OUTSIDE the head-index
/// eligibility rule (returned from a fn, passed by value, indexed) keep
/// the memmove lowering and stay correct. A returned deque's header must
/// keep `len` meaning COUNT (the caller and every generic Vec path read
/// it that way), so eligibility refusing these shapes is load-bearing —
/// this pins the behavior side of that refusal.
///
/// Twin of `tests/interpreter.rs`'s `test_deque_ineligible_shapes_slow_path`.
#[test]
fn e2e_deque_ineligible_shapes_slow_path() {
    let Some(out) = run_program(
        "fn make() -> VecDeque[i64] {\n\
             \x20   let mut q: VecDeque[i64] = VecDeque.new();\n\
             \x20   q.push_back(5);\n\
             \x20   q.push_back(6);\n\
             \x20   q\n\
             }\n\
             fn total(d: VecDeque[i64]) -> i64 {\n\
             \x20   let mut acc = 0;\n\
             \x20   let mut d2 = d;\n\
             \x20   while not d2.is_empty() {\n\
             \x20       match d2.pop_front() { Some(x) => { acc = acc + x; } None => {} }\n\
             \x20   }\n\
             \x20   acc\n\
             }\n\
             fn main() {\n\
             \x20   let got = make();\n\
             \x20   println(total(got));\n\
             \x20   let mut r: VecDeque[i64] = VecDeque.new();\n\
             \x20   r.push_back(7);\n\
             \x20   r.push_back(8);\n\
             \x20   println(r[0]);\n\
             \x20   match r.pop_front() { Some(x) => { println(x); } None => {} }\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "11\n7\n7\n");
}

/// B-2026-08-21-6 — codegen actually SELECTS on the `Map[K, V, H]` hasher.
///
/// One process, two maps, identical `String` keys inserted in identical
/// order, differing only in the declared hasher. The per-key-type hash
/// function is synthesized once and CACHED under a symbol name, and stored
/// in the map's control block at construction — so if the name did not
/// carry the hasher, the second map would silently reuse the first's
/// function and the two walks would come out identical. That is the exact
/// failure this pins, and it is checkable in a single process, unlike the
/// across-process properties (which `tests/cli.rs` covers by subprocess).
///
/// Ten keys: two hash functions agreeing on the whole permutation by
/// coincidence is 1 in 10!, not a flake.
#[test]
fn the_two_hashers_order_the_same_keys_differently_in_one_process() {
    let out = run_program(
        "fn main() {\n\
                 let mut fx: Map[String, i64, FxBuildHasher] = Map.new();\n\
                 let mut sip: Map[String, i64, SipHash13BuildHasher] = Map.new();\n\
                 let names = [\"zulu\", \"alpha\", \"mike\", \"bravo\", \"yankee\",\n\
                              \"charlie\", \"xray\", \"delta\", \"whiskey\", \"echo\"];\n\
                 for n in names {\n\
                     fx.insert(n.to_string(), 1);\n\
                     sip.insert(n.to_string(), 1);\n\
                 }\n\
                 let mut a = \"\";\n\
                 for k in fx.keys() { a = a + k + \" \"; }\n\
                 println(a);\n\
                 let mut b = \"\";\n\
                 for k in sip.keys() { b = b + k + \" \"; }\n\
                 println(b);\n\
             }",
    );
    if let Some(out) = out {
        let mut lines = out.lines();
        let fx = lines.next().unwrap_or_default();
        let sip = lines.next().unwrap_or_default();
        assert_eq!(
            fx.split_whitespace().count(),
            10,
            "the Fx map lost keys: {out}"
        );
        assert_ne!(
            fx, sip,
            "both maps walked in the same order, so the FxBuildHasher one \
                 is using the default's hash function — the synthesized \
                 symbol name is not keyed on the hasher"
        );
        let mut fx_sorted: Vec<&str> = fx.split_whitespace().collect();
        let mut sip_sorted: Vec<&str> = sip.split_whitespace().collect();
        fx_sorted.sort_unstable();
        sip_sorted.sort_unstable();
        assert_eq!(
            fx_sorted, sip_sorted,
            "the two maps hold different keys, so the order comparison \
                 above proves nothing: {out}"
        );
    }
}

/// Two user builders, one process, same ten keys in the same order.
///
/// `hash_fn` is synthesized once per (key type, hasher) and CACHED under a
/// symbol name, so if `HasherKind::mangle_suffix` did not carry the
/// builder's name the second map would silently reuse the first's function
/// and the walks would match. This is also the test that shows the user's
/// permutation runs at all: a degraded constant digest would put every key
/// in one bucket and make both walks insertion-ordered, hence equal.
#[test]
fn two_user_hashers_order_the_same_keys_differently_in_one_process() {
    let out = run_program(&format!(
        "{USER_HASHERS}\
             fn main() {{\n\
                 let mut a: Map[String, i64, FnvBuild] = Map.new();\n\
                 let mut b: Map[String, i64, SumBuild] = Map.new();\n\
                 let names = [\"zulu\", \"alpha\", \"mike\", \"bravo\", \"yankee\",\n\
                              \"charlie\", \"xray\", \"delta\", \"whiskey\", \"echo\"];\n\
                 for n in names {{\n\
                     a.insert(n.to_string(), 1);\n\
                     b.insert(n.to_string(), 1);\n\
                 }}\n\
                 let mut x = \"\";\n\
                 for k in a.keys() {{ x = x + k + \" \"; }}\n\
                 println(x);\n\
                 let mut y = \"\";\n\
                 for k in b.keys() {{ y = y + k + \" \"; }}\n\
                 println(y);\n\
             }}"
    ));
    if let Some(out) = out {
        let mut lines = out.lines();
        let fnv = lines.next().unwrap_or_default();
        let sum = lines.next().unwrap_or_default();
        assert_eq!(
            fnv.split_whitespace().count(),
            10,
            "the FnvBuild map lost keys: {out}"
        );
        assert_ne!(
            fnv, sum,
            "both maps walked in the same order, so either the two builders \
                 share one cached hash_fn or the digest degraded to a constant"
        );
        let mut fnv_sorted: Vec<&str> = fnv.split_whitespace().collect();
        let mut sum_sorted: Vec<&str> = sum.split_whitespace().collect();
        fnv_sorted.sort_unstable();
        sum_sorted.sort_unstable();
        assert_eq!(
            fnv_sorted, sum_sorted,
            "the two maps hold different keys, so the order comparison above \
                 proves nothing: {out}"
        );
    }
}

#[test]
fn test_ir_non_const_and_multibyte_chars_loops_keep_the_decode_path() {
    // B-2026-07-27-7 fail-closed guard — the half that keeps the fix
    // sound. Each of these must keep a loop that really DECODES, because
    // the proof-gated branch-free walk binds one `char` per BYTE and would
    // silently split a multibyte scalar into its UTF-8 bytes.
    //
    // "Decodes" is the invariant, not any one lowering: since
    // B-2026-07-28-2 the `param` case takes the dual-region ASCII bailout,
    // whose multibyte region calls the same decoder. What must never
    // happen is the byte walk, and the surviving call is what proves it
    // didn't. The cases:
    //   - a String PARAMETER (contents unknown at compile time),
    //   - a multibyte literal,
    //   - an ASCII literal binding that is later MUTATED (`push`), so the
    //     literal is no longer what the loop walks.
    for (name, src) in [
        (
            "param",
            "fn walk(s: String) -> i64 {\n\
                 \x20   let mut n = 0i64;\n\
                 \x20   for ch in s.chars() { n = n + 1i64; }\n\
                 \x20   return n;\n\
                 }\n\
                 fn main() { println(walk(\"ab\".to_string())); }",
        ),
        (
            "multibyte-literal",
            "fn walk() -> i64 {\n\
                 \x20   let s: String = \"h\u{e9}llo\";\n\
                 \x20   let mut n = 0i64;\n\
                 \x20   for ch in s.chars() { n = n + 1i64; }\n\
                 \x20   return n;\n\
                 }\n\
                 fn main() { println(walk()); }",
        ),
        (
            "mutated-after-let",
            "fn walk() -> i64 {\n\
                 \x20   let s: String = \"ab\";\n\
                 \x20   s.push('c');\n\
                 \x20   let mut n = 0i64;\n\
                 \x20   for ch in s.chars() { n = n + 1i64; }\n\
                 \x20   return n;\n\
                 }\n\
                 fn main() { println(walk()); }",
        ),
    ] {
        let ir = ir_for(src);
        let w = ir
            .split("define")
            .find(|f| f.contains("@walk("))
            .unwrap_or_else(|| panic!("@walk not found in IR for {}", name));
        assert!(
            w.contains("@karac_string_decode_char"),
            "{}: must fail CLOSED to a lowering that really decodes UTF-8 \
                 — the branch-free walk would mis-iterate multibyte text; \
                 got:\n{}",
            name,
            w
        );
    }
}

#[test]
fn test_e2e_ascii_const_chars_loop_matches_the_decode_path() {
    // B-2026-07-27-7 behavioural companion to the two IR guards. The fast
    // path is a different loop body, so pin the observable results across
    // every shape it can and cannot take: a let-bound ASCII constant, a
    // multibyte let-bound constant, bare ASCII / multibyte literal
    // receivers, the no-`.chars()` spelling, a binding mutated after its
    // `let`, a reassigned binding, `break` / `continue` through the fast
    // path, the empty string, glyph (not codepoint) rendering, and a
    // SHADOWING inner binding of the same name holding runtime multibyte
    // text — the case the loop-site alloca identity check exists for.
    if let Some(out) = run_program(
        "fn pick() -> String { return \"\u{e9}\u{2192}\"; }\n\
             fn spoil(s: mut ref String) { s.push('\u{e9}'); }\n\
             fn main() {\n\
             \x20   let a: String = \"abcXYZ\";\n\
             \x20   let mut o = 0i64;\n\
             \x20   for c in a.chars() { o = o + (c as i64); }\n\
             \x20   println(o);\n\
             \x20   let m: String = \"h\u{e9}llo\u{2192}\";\n\
             \x20   let mut p = 0i64;\n\
             \x20   for c in m.chars() { p = p + (c as i64); }\n\
             \x20   println(p);\n\
             \x20   let mut q = 0i64;\n\
             \x20   for c in \"hey\".chars() { q = q + (c as i64); }\n\
             \x20   println(q);\n\
             \x20   let mut r = 0i64;\n\
             \x20   for c in \"\u{e9}\u{2192}\".chars() { r = r + (c as i64); }\n\
             \x20   println(r);\n\
             \x20   let e: String = \"pq\";\n\
             \x20   let mut t = 0i64;\n\
             \x20   for c in e { t = t + (c as i64); }\n\
             \x20   println(t);\n\
             \x20   let mut g: String = \"ab\";\n\
             \x20   g.push('\u{e9}');\n\
             \x20   let mut u = 0i64;\n\
             \x20   for c in g.chars() { u = u + (c as i64); }\n\
             \x20   println(u);\n\
             \x20   let mut w: String = \"ab\";\n\
             \x20   w = \"\u{e9}\";\n\
             \x20   let mut x = 0i64;\n\
             \x20   for c in w.chars() { x = x + (c as i64); }\n\
             \x20   println(x);\n\
             \x20   let k: String = \"abcdef\";\n\
             \x20   let mut n = 0i64;\n\
             \x20   let mut s = 0i64;\n\
             \x20   for c in k.chars() {\n\
             \x20       if n == 1i64 { n = n + 1i64; continue; }\n\
             \x20       if n == 4i64 { break; }\n\
             \x20       s = s + (c as i64);\n\
             \x20       n = n + 1i64;\n\
             \x20   }\n\
             \x20   println(s);\n\
             \x20   let mut z = 0i64;\n\
             \x20   for c in \"\".chars() { z = z + 1i64; }\n\
             \x20   println(z);\n\
             \x20   let v: String = \"xyz\";\n\
             \x20   let mut gl: String = \"\";\n\
             \x20   for c in v.chars() { gl = gl + f\"{c}\"; }\n\
             \x20   println(gl);\n\
             \x20   let alphabet: String = \"ab\";\n\
             \x20   let mut y = 0i64;\n\
             \x20   for c in alphabet.chars() { y = y + (c as i64); }\n\
             \x20   println(y);\n\
             \x20   {\n\
             \x20       let alphabet: String = pick();\n\
             \x20       let mut sh = 0i64;\n\
             \x20       for c in alphabet.chars() { sh = sh + (c as i64); }\n\
             \x20       println(sh);\n\
             \x20   }\n\
             \x20   let mut ma: String = \"ab\";\n\
             \x20   spoil(mut ma);\n\
             \x20   let mut mc = 0i64;\n\
             \x20   for c in ma.chars() { mc = mc + (c as i64); }\n\
             \x20   println(mc);\n\
             }",
    ) {
        assert_eq!(
            out,
            // a=97+98+99+88+89+90=561; héllo→=104+233+108+108+111+8594=9258;
            // hey=104+101+121=326; é→=233+8594=8827; pq=112+113=225;
            // "ab"+push('é')=97+98+233=428; reassigned to "é"=233;
            // break/continue over "abcdef"=97+99+100=296; empty=0;
            // glyphs=xyz; ab=195; shadowed é→=8827; `spoil(mut ma)` pushes é onto
            // "ab" so rule 4 must disqualify it: 97+98+233=428 (a byte-wise walk
            // would give 4 chars summing 559).
            "561\n9258\n326\n8827\n225\n428\n233\n296\n0\nxyz\n195\n8827\n428\n"
        );
    }
}

#[test]
fn e2e_for_chars_decode_ascii_fastpath_and_multibyte() {
    // `for c in s.chars()` ASCII fast-path: a leading byte < 0x80 is its own
    // scalar, decoded inline (peek byte, offset += 1) without the per-char
    // `karac_string_decode_char` call — the read-side counterpart of the
    // push fast-path (B-2026-06-18-6). SOUNDNESS: the multibyte slow path
    // must still decode the *correct codepoints*, so this sums `c as i64`
    // over a string interleaving 1/2/3/4-byte scalars with ASCII and checks
    // both the count and the codepoint sum against the interpreter.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let s: String = \"aé€🦀z\";\n\
                 let mut n = 0i64;\n\
                 let mut sum = 0i64;\n\
                 for c in s.chars() { n = n + 1i64; sum = sum + (c as i64); }\n\
                 println(f\"{n} {sum}\");\n\
             }",
    ) {
        // 'a'97 + 'é'233 + '€'8364 + '🦀'129408 + 'z'122 = 138224; 5 scalars
        assert_eq!(out, "5 138224\n");
    }
}

#[test]
fn test_stdin_read_line_resolves_and_typechecks_clean() {
    // Full-pipeline check (NOT the `run_program` path, which tolerates
    // resolve/typecheck errors — see the
    // codegen-run_program-bypasses-typecheck hazard). `Stdin.read_line()`
    // must resolve and typecheck cleanly: it is a real stdlib
    // `#[compiler_builtin]` method whose `Result[String, IoError]` return
    // flows to the `Ok(s)` binding so `s.len()` dispatches. Guards against
    // regressing the capitalized surface into a codegen-only "fake pass".
    let src = r#"
fn main() reads(Stdin) {
    match Stdin.read_line() {
        Ok(s) => { let _n = s.len(); }
        Err(_) => {}
    }
}
"#;
    let parsed = karac::parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let resolved = karac::resolve(&parsed.program);
    assert!(
        resolved.errors.is_empty(),
        "resolve errors for Stdin.read_line: {:?}",
        resolved.errors
    );
    let typed = karac::typecheck(&parsed.program, &resolved);
    assert!(
        typed.errors.is_empty(),
        "typecheck errors for Stdin.read_line: {:?}",
        typed.errors
    );
}

#[test]
fn test_stdout_stderr_methods_resolve_and_typecheck_clean() {
    // Full-pipeline check (NOT `run_program`, which tolerates
    // resolve/typecheck errors). The capitalized `Stdout.*` / `Stderr.*`
    // forms are real stdlib `#[compiler_builtin]` methods and must
    // resolve + typecheck cleanly.
    let src = r#"
fn main() {
    Stdout.print("a");
    Stdout.println("b");
    Stdout.flush();
    Stderr.print("c");
    Stderr.println("d");
    Stderr.flush();
}
"#;
    let parsed = karac::parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let resolved = karac::resolve(&parsed.program);
    assert!(
        resolved.errors.is_empty(),
        "resolve errors for Stdout/Stderr methods: {:?}",
        resolved.errors
    );
    let typed = karac::typecheck(&parsed.program, &resolved);
    assert!(
        typed.errors.is_empty(),
        "typecheck errors for Stdout/Stderr methods: {:?}",
        typed.errors
    );
}

#[test]
fn test_e2e_with_provider_override_env_var_result() {
    // `with_provider[Env]` override of `var` — `Result[String, VarError]`
    // return, exercising the dispatch-branch phi at an *enum struct* type
    // (the generalization beyond the old hardcoded-i64 phi). The override
    // returns `Ok("mocked")` for any key; after the scope pops, the real
    // FFI default returns `Err(VarError.NotPresent)` for an unset key.
    // The call lives in a separate fn (`rv`) so dispatch is cross-boundary.
    let out = run_program(
        r#"
struct FakeEnv {}
impl FakeEnv { fn var(self, name: String) -> Result[String, VarError] { Ok("mocked") } }
fn rv() -> String reads(Env) {
    match env.var("ANYTHING") { Ok(s) => s, Err(e) => "missing" }
}
fn main() reads(Env) {
    with_provider[Env](FakeEnv {}, || { println(rv()); });
    match env.var("KARA_DEFINITELY_UNSET_XYZ") {
        Ok(s) => println(s),
        Err(e) => println("unset-default"),
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "mocked\nunset-default");
    }
}

#[test]
fn test_e2e_with_provider_override_env_args_vec() {
    // `with_provider[Env]` override of `args` — `Vec[String]` return, the
    // dispatch-branch phi at a *Vec struct* type. This is the exact case
    // the old `test_ambient_override_of_nonvtable_method_errors_loudly`
    // pinned as a LOUD error; the slice lifts that to a working override.
    // (The provider body builds the Vec via an explicitly-typed
    // `Vec.new()` + push — an array literal as a `Vec[String]` return is a
    // separate, pre-existing codegen-coercion gap, tracked in
    // phase-7-codegen.md, orthogonal to override dispatch.) Cross-boundary
    // call in `count()`.
    let out = run_program(
        r#"
struct FakeEnv {}
impl FakeEnv {
    fn args(self) -> Vec[String] {
        let mut v: Vec[String] = Vec.new();
        v.push("alpha");
        v.push("beta");
        v.push("gamma");
        v
    }
}
fn count() -> i64 reads(Env) { env.args().len() }
fn main() reads(Env) {
    with_provider[Env](FakeEnv {}, || { println(count()); });
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "3");
    }
}

#[test]
fn test_e2e_split_at_mut_halves_write_through_to_the_source() {
    // Vec receiver.
    assert_eq!(
        run_program(
            "fn fill(s: mut Slice[u8], v: u8) {\n\
                     let mut i: i64 = 0i64;\n\
                     while i < s.len() { s[i] = v; i = i + 1i64; }\n\
                 }\n\
                 fn main() {\n\
                     let mut buf: Vec[u8] = [0u8, 0u8, 0u8, 0u8, 0u8, 0u8];\n\
                     let mut parts: (mut Slice[u8], mut Slice[u8]) = buf.split_at_mut(2i64);\n\
                     fill(parts.0, 7u8);\n\
                     fill(parts.1, 9u8);\n\
                     println(f\"{buf[0]} {buf[1]} {buf[2]} {buf[5]}\");\n\
                 }"
        )
        .as_deref(),
        Some("7 7 9 9\n")
    );
    // `mut Slice` receiver, over an Array — the second spec'd receiver.
    assert_eq!(
        run_program(
            "fn fill(s: mut Slice[i64], v: i64) {\n\
                     let mut i: i64 = 0i64;\n\
                     while i < s.len() { s[i] = v; i = i + 1i64; }\n\
                 }\n\
                 fn main() {\n\
                     let mut a: Array[i64, 4] = [1i64, 2i64, 3i64, 4i64];\n\
                     let mut s: mut Slice[i64] = a.as_slice_mut();\n\
                     let mut p: (mut Slice[i64], mut Slice[i64]) = s.split_at_mut(1i64);\n\
                     fill(p.0, 100i64);\n\
                     fill(p.1, 200i64);\n\
                     println(f\"{a[0]} {a[1]} {a[2]} {a[3]}\");\n\
                 }"
        )
        .as_deref(),
        Some("100 200 200 200\n")
    );
    // Boundary: mid == 0 and mid == len are legal; only `mid > len` panics.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let mut v: Vec[i64] = [1i64, 2i64, 3i64];\n\
                     let mut a: (mut Slice[i64], mut Slice[i64]) = v.split_at_mut(0i64);\n\
                     let mut b: (mut Slice[i64], mut Slice[i64]) = v.split_at_mut(3i64);\n\
                     println(f\"{a.0.len()} {a.1.len()} {b.0.len()} {b.1.len()}\");\n\
                 }"
        )
        .as_deref(),
        Some("0 3 3 0\n")
    );
}

/// B-2026-08-20-26 — NO TEST IN THIS BINARY MAY MUTATE THE PROCESS ENV.
///
/// Several codegen gates are read afresh at every `Codegen::new`
/// (`KARAC_RUNTIME_DEBUG_METADATA`, `KARAC_AUTO_PAR`,
/// `KARAC_STRIP_CONTRACTS`, `KARAC_DEBUG_INFO`, …). The environment is
/// process-global and cargo runs these ~3,000 tests on parallel threads,
/// so one test setting a gate changes what EVERY concurrently-compiling
/// peer emits. That is not a theoretical hazard: it made the determinism
/// gate for B-2026-08-05-16 fail ~6 of 8 full-suite runs, and the failure
/// was filed as that miscompile returning.
///
/// The remedy is per-compile or per-thread overrides —
/// `compile_to_ir_with_contracts_stripped`, `compile_to_ir_with_debug_info`,
/// `pin_runtime_debug_metadata` — and this test is what keeps the next
/// env-mutating test from being written. It scans this file's own source.
///
/// Setting env on a CHILD process (`Command::env`) is unaffected and
/// stays fine: it cannot be observed by a compile in this process.
#[test]
fn no_test_in_this_binary_mutates_the_process_env() {
    // Built at runtime so this test's own source does not match itself.
    let set = format!("std::env::{}_var(", "set");
    let remove = format!("std::env::{}_var(", "remove");
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/codegen.rs"),
    )
    .expect("read tests/codegen.rs");
    let mut hits: Vec<(usize, &str)> = Vec::new();
    for (i, line) in src.lines().enumerate() {
        if line.contains(&set) || line.contains(&remove) {
            hits.push((i + 1, line.trim()));
        }
    }
    assert!(
        hits.is_empty(),
        "tests/codegen.rs mutates the process environment, which every \
             concurrently-compiling test in this binary observes — use a \
             per-compile or per-thread override instead (see this test's doc). \
             Offending lines: {hits:?}"
    );
}

#[test]
fn test_e2e_modbind_container_for_loop_matches_other_access_paths() {
    // The discriminator that localized B-2026-07-31-30: indexing and
    // `.clone()` were always right, so any of the three disagreeing with the
    // others means the for-loop lowering regressed again rather than the
    // module-binding storage itself breaking (which would move all three).
    let output = run_program(
        "let mut MV: Vec[i64] = Vec.new();\n\
             fn main() {\n\
                 MV.push(10); MV.push(20);\n\
                 let mut direct = 0;\n\
                 for x in MV { direct = direct + x; }\n\
                 let mut indexed = 0;\n\
                 for i in 0..MV.len() { indexed = indexed + MV[i]; }\n\
                 let copy = MV.clone();\n\
                 let mut cloned = 0;\n\
                 for x in copy { cloned = cloned + x; }\n\
                 println(direct);\n\
                 println(indexed);\n\
                 println(cloned);\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "30\n30\n30\n");
}

// ── Phase 8 File handle slice F5 — ?-propagation ──────────────────
//
// Validates that `?` on a `Result[File, IoError]` (or
// `Result[T, IoError]` for any T) early-returns the IoError to the
// caller's Result. The mechanism reuses the existing 4-word Result
// payload path widened at phase-8 line 435 slice 2 (2026-05-21);
// Json.parse exercised the Ok-with-4-word-payload case, F5
// validates the Err-with-multi-word-payload case (IoError variant
// tag + optional String message for the Other variant). No
// codegen changes at F5 — the existing `compile_question` path
// handles this shape correctly.
//
// The tests run a compiled binary that calls a helper returning
// `Result[i64, IoError]` and uses `?` to propagate `File.open`'s
// Err arm; the caller matches the propagated IoError variant.

/// B-2026-08-10-3 — `File.seek` through the compiled backends.
///
/// The runtime entry point `karac_runtime_file_seek` has existed since the
/// File slice shipped, exported early and kept on the
/// `__preserve_no_mangle_symbols` list precisely so adding the surface
/// would need no runtime rebuild. This pins that the promise held: the only
/// codegen work was declaring the extern, truncating the `SeekFrom` tag to
/// the ABI's `u8`, and unpacking the io-result as a byte count.
///
/// Reads the byte AT the sought position rather than only checking the
/// returned offset — a position check alone passes even if the cursor never
/// moved. The file is "ABCD", so seeking to 2 must yield 'C' (67).
#[test]
fn test_e2e_file_seek_positions_and_reads() {
    let tmp = std::env::temp_dir().join("karac_e2e_file_seek.bin");
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    let _ = std::fs::remove_file(&tmp);
    let out = run_program(&format!(
        r#"
fn main() {{
    match File.create("{path}") {{
        Ok(f) => {{
            let data = [65u8, 66u8, 67u8, 68u8];
            match f.write(data[0..4]) {{ Ok(_) => {{}} Err(_) => println("write err") }}
            match f.flush() {{ Ok(_) => {{}} Err(_) => println("flush err") }}
        }}
        Err(_) => println("create err"),
    }}
    match File.open("{path}") {{
        Ok(g) => {{
            match g.seek(SeekFrom.Start, 2i64) {{
                Ok(p) => println("pos " + p.to_string()),
                Err(_) => println("seek err"),
            }}
            let mut buf = [0u8, 0u8];
            match g.read(mut buf) {{
                Ok(n) => println("read " + n.to_string() + " b0 " + buf[0].to_string()),
                Err(_) => println("read err"),
            }}
            match g.seek(SeekFrom.End, -1i64) {{
                Ok(e) => println("end " + e.to_string()),
                Err(_) => println("seek err"),
            }}
            match g.seek(SeekFrom.Current, 0i64) {{
                Ok(c) => println("cur " + c.to_string()),
                Err(_) => println("seek err"),
            }}
            match g.seek(SeekFrom.Start, -5i64) {{
                Ok(_) => println("unexpected ok"),
                Err(_) => println("neg rejected"),
            }}
        }}
        Err(_) => println("open err"),
    }}
}}
"#
    ));
    let _ = std::fs::remove_file(&tmp);
    assert_eq!(
        out.as_deref(),
        Some("pos 2\nread 2 b0 67\nend 3\ncur 3\nneg rejected\n")
    );
}

#[test]
fn test_e2e_fs_write_err_on_unwritable_path() {
    // Writing under a nonexistent directory fails; codegen builds
    // `Result.Err(IoError.<kind>)` via the shared `lower_kara_io_result`
    // error arm. Pins the Err half of the Unit-result branch (the Ok
    // half is covered by the round-trip test).
    let out = run_program(
        r#"
fn main() with writes(FileSystem) {
    match FileSystem.write("/no_such_dir_karac_l646_s4/x.txt", "data") {
        Ok(_) => println("unexpected-ok"),
        Err(_) => println("write-err"),
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "write-err");
    }
}

#[test]
fn test_fs_write_resolves_and_typechecks_clean() {
    // Full-pipeline check (NOT `run_program`, which tolerates
    // resolve/typecheck errors). `FileSystem.write(path, contents)` must
    // resolve + typecheck cleanly as a real stdlib `#[compiler_builtin]`
    // method returning `Result[Unit, IoError]`.
    let src = r#"
fn main() with writes(FileSystem) {
    let _r = FileSystem.write("/tmp/karac_fs_write_tc.txt", "x");
}
"#;
    let parsed = karac::parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let resolved = karac::resolve(&parsed.program);
    assert!(
        resolved.errors.is_empty(),
        "resolve errors for FileSystem.write: {:?}",
        resolved.errors
    );
    let typed = karac::typecheck(&parsed.program, &resolved);
    assert!(
        typed.errors.is_empty(),
        "typecheck errors for FileSystem.write: {:?}",
        typed.errors
    );
}

#[test]
fn test_e2e_fs_read_lines_splits_file_into_vec() {
    // `FileSystem.read_lines(path) -> Result[Vec[String], IoError]`
    // (B-2026-07-11-38): slurp a multi-line file, split on newlines,
    // and iterate the resulting `Vec[String]`. Witnesses the whole
    // vertical — the `karac_runtime_fs_read_lines` two-out-param FFI,
    // the `Result.Ok(<Vec[String]>)` aggregate construction, and the
    // per-element String drop (no leak). The middle blank line pins
    // that empty lines survive as empty `String`s (not dropped).
    let tmp = std::env::temp_dir().join("karac_e2e_fs_read_lines.txt");
    let _ = std::fs::remove_file(&tmp);
    std::fs::write(&tmp, b"alpha\nbeta\n\ndelta\n").expect("temp write");
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    let src = format!(
        r#"
fn main() with reads(FileSystem) {{
    match FileSystem.read_lines("{path}") {{
        Ok(lines) => {{
            println(lines.len().to_string());
            for line in lines {{
                println("[" + line + "]");
            }}
        }},
        Err(_) => println("read-err"),
    }}
}}
"#
    );
    let out = run_program(&src);
    let _ = std::fs::remove_file(&tmp);
    if let Some(out) = out {
        assert_eq!(out.trim(), "4\n[alpha]\n[beta]\n[]\n[delta]");
    }
}

#[test]
fn test_e2e_fs_read_lines_missing_file_returns_err() {
    // A read of a nonexistent path must land on the `Err(IoError…)`
    // arm — the KaracIoResult error path builds `Result.Err` with the
    // IoError variant tag (`error_kind - 1`), leaving the vec empty.
    let src = r#"
fn main() with reads(FileSystem) {
    match FileSystem.read_lines("/nonexistent/karac-b38/xyz.txt") {
        Ok(lines) => println(lines.len().to_string()),
        Err(_) => println("io-err"),
    }
}
"#;
    let out = run_program(src);
    if let Some(out) = out {
        assert_eq!(out.trim(), "io-err");
    }
}

#[test]
fn test_e2e_lowercase_stdout_stderr_split() {
    // Lowercase `stdout.println` / `stderr.println` must land on the same
    // streams as their capitalized forms: stdout line on fd 1, stderr line
    // on fd 2. Sharp witness that the lowercase alias reaches the same
    // `emit_console_str_write` lowering (printf for stdout, dprintf(2) for
    // stderr) as `Stdout`/`Stderr`.
    let cap = run_program_capturing(
        r#"
fn main() {
    stderr.println("lc-to-stderr");
    stdout.println("lc-to-stdout");
}
"#,
    );
    if let Some(cap) = cap {
        assert_eq!(cap.stdout.trim(), "lc-to-stdout");
        assert!(
            cap.stderr.contains("lc-to-stderr"),
            "stderr missing line: {:?}",
            cap.stderr
        );
    }
}

#[test]
fn test_e2e_file_sync_all_and_sync_data_persist_contents() {
    // B-2026-07-30-16 — the durability pair. Both were accepted by
    // the typechecker but unimplemented in codegen AND the
    // interpreter, so `karac check` went green on a program neither
    // backend could run. They are NOT `flush` with a different name:
    // `Write::flush` on a `std::fs::File` is a documented no-op that
    // pushes userspace buffers and never the page cache, while these
    // issue a real fsync/fdatasync. The syscall-level proof lives in
    // the ledger entry (strace shows `fsync`/`fdatasync` for these
    // and zero for a flush-only control); what this test pins is the
    // surface: both return `Ok` and the bytes land on disk.
    let tmp = std::env::temp_dir().join("karac_e2e_file_sync_pair.txt");
    let _ = std::fs::remove_file(&tmp);
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    let src = format!(
        r#"
fn main() with writes(FileSystem) {{
    match File.create("{path}") {{
        Ok(f) => {{
            let mut data: Vec[u8] = Vec.new();
            data.push(79u8); data.push(75u8);
            match f.write(data) {{
                Ok(_) => println("wrote"),
                Err(_) => println("write-err"),
            }}
            match f.sync_all() {{
                Ok(_) => println("sync_all"),
                Err(_) => println("sync_all-err"),
            }}
            match f.sync_data() {{
                Ok(_) => println("sync_data"),
                Err(_) => println("sync_data-err"),
            }}
        }}
        Err(_) => println("create-err"),
    }}
}}
"#
    );
    let out = run_program(&src);
    if let Some(out) = out {
        assert_eq!(out.trim(), "wrote\nsync_all\nsync_data");
        let contents = std::fs::read(&tmp).expect("read tempfile");
        assert_eq!(contents, b"OK");
    }
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_e2e_unsigned_refinement_runtime_value_above_signed_midpoint() {
    // The positive twin of the guard above, also through a parameter so
    // it exercises the RUNTIME compare: 40000 is in range for `u16` but
    // reads as negative in `i16`, so the signed predicate rejected it.
    let out = run_program(
        r#"
distinct type Port = u16 where self >= 1 and self <= 65535;
fn make(x: u16) -> Port { Port(x) }
fn main() {
    let p = make(40000);
    println("built");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "built");
    }
}

#[test]
fn test_ir_repl_cell_keeps_runtime_prefix() {
    // REPL cell modules always keep the runtime read: a cell can call a
    // contracted function JIT'd from an EARLIER cell, which this
    // module's item scan can't see (size/perf are irrelevant under JIT).
    let mut parsed = karac::parse(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1);
    println(v[0]);
}
"#,
    );
    assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);
    let ir = karac::codegen::compile_to_ir_for_repl_cell(
        &parsed.program,
        &std::collections::HashSet::new(),
        "__karac_repl_cell_main",
    )
    .expect("repl-cell codegen failed");
    assert!(
        ir.contains("call ptr @karac_runtime_panic_prefix"),
        "a REPL cell module must keep the runtime prefix read (cross-cell contracts)"
    );
}

#[test]
fn test_e2e_conditional_param_view_assign_keeps_target_body_per_path() {
    // B-2026-08-30-53 — `out = r` inside a match arm, where `r` is a
    // payload view of an OWNED param. The assignment hands the caller's
    // value to `out`, so `out`'s own body must not fire on the path that
    // stored; the site enforced that with an ALL-PATHS retraction
    // (`suppress_user_drop_for_var`), which also deleted the body on the
    // path where the arm never ran and `out` still held its own
    // initializer. Nobody else owned that value — the scrutinee is `E.B`
    // and has no payload to walk — so both compiled backends printed
    // `mid dE a0` against the interpreter's `mid dR0 dE a0`.
    //
    // The fix arms B-2026-08-28-51's `cond_move_drop_flags` on the
    // assignment TARGET instead: entry-block `true`, a `false` store in the
    // block the assignment compiles into, and
    // `emit_user_drop_call_guarded` reading it at both fire sites. The
    // taken path is byte-identical to the retraction it replaces.
    //
    // Seven shapes. `a` is the row's repro (arm not taken) and `e` is its
    // in-loop sibling, both of which lost the body entirely before. `b` is
    // the taken path, unchanged and the reason the retraction exists — the
    // displacement fire still prints `dR0` at the assignment, ahead of
    // `mid`. `c` (a FRESH value assigned in the arm) and `d` (an
    // unconditional param-view assign) were already correct on all four
    // surfaces and are the boundaries an over-eager fix would move.
    //
    // `f` is the RE-ARM row, and it is here because the first cut of this
    // fix broke it: the retraction it replaced had a counterpart the flag
    // did not, since `rearm_container_bodies_for_name` re-registers the
    // action on every assignment to a name, so a target that held a view
    // and is then given a FRESH value got its body back for free. With the
    // action never removed it is never re-registered either, and the stale
    // `false` silenced the fresh value's body — `dR5` lost. The flag is now
    // re-armed on any assignment whose RHS is not a param view. Nothing in
    // the suite covered this shape, which is why the first cut shipped.
    let out = run_program(
        r#"
struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("dE") } }

#[allow(partial_move_of_drop_enum)]
fn dies(b: E) -> i64 {
    let mut out: R = R { id: 0, tag: f"t0" };
    match b { E.A(r) => { out = r; } E.B => { } }
    println("mid");
    return out.id
}

fn fresh(b: E) -> i64 {
    let mut out: R = R { id: 1, tag: f"t1" };
    match b { E.A(r) => { out = R { id: 7, tag: f"t7" }; } E.B => { } }
    return out.id
}

fn uncond(p: R) -> i64 {
    let mut out: R = R { id: 2, tag: f"t2" };
    out = p;
    return out.id
}

#[allow(partial_move_of_drop_enum)]
fn looped(b: E) -> i64 {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 2 {
        let mut out: R = R { id: 90 + i, tag: f"t" };
        match b { E.A(r) => { out = r; } E.B => { } }
        acc = acc + out.id;
        i = i + 1;
    }
    return acc
}

#[allow(partial_move_of_drop_enum)]
fn refreshed(b: E) -> i64 {
    let mut out: R = R { id: 0, tag: f"t0" };
    match b { E.A(r) => { out = r; } E.B => { } }
    println("m1");
    out = R { id: 5, tag: f"t5" };
    println("m2");
    return out.id
}

fn main() {
    println(f"a{dies(E.B)}");
    println("-");
    println(f"b{dies(E.A(R { id: 5, tag: f"t5" }))}");
    println("-");
    println(f"c{fresh(E.B)}");
    println("-");
    println(f"d{uncond(R { id: 9, tag: f"t9" })}");
    println("-");
    println(f"e{looped(E.B)}");
    println("-");
    println(f"f{refreshed(E.A(R { id: 8, tag: f"t8" }))}");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
                out.trim(),
                "mid\ndR0\ndE\na0\n-\ndR0\nmid\ndE\ndR5\nb5\n-\ndR1\ndE\nc1\n-\ndR2\ndR9\nd9\n-\ndR90\ndR91\ndE\ne181\n-\ndR0\nm1\nm2\ndR5\ndE\ndR8\nf5"
            );
    }
}

#[test]
fn test_e2e_process_exit_sets_status_code() {
    // phase-12 Cluster-2: `process.exit(code)` codegen lowering. Under
    // `karac build`, `process.exit` previously aborted codegen with
    // "no handler for method 'exit' on variable 'process'" (it works under
    // the interpreter as a dotted path-call). The new dispatcher arm lowers
    // it to libc `exit(i32)` + an `unreachable` terminator. The exit code is
    // the unforgeable proof: output before the call must flush, and the
    // process must exit with exactly the requested code.
    let out = run_program_capturing(
        r#"
fn main() {
    println("before exit");
    process.exit(42);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.stdout.trim(), "before exit");
        assert_eq!(
            out.status.code(),
            Some(42),
            "process.exit(42) must terminate with status 42; got {:?}",
            out.status
        );
    }
}

/// B-2026-08-30-33 — a parameter with TWO exits: conditionally STORED into
/// a `mut ref` place on one path, and RETURNED (inside a wrapper) on
/// another. Every path must run the body exactly once.
///
/// This began as B-2026-08-30-28's escape-route guard, which DECLINED the
/// shape because only the store disarmed the per-path flag: with the
/// registration ungated and the return left armed, the callee's body and
/// the caller's result binding both fired for one object — `drop 301 ` with
/// the String already moved into the wrapper, then the real one, on all four
/// surfaces. -33 added the missing disarms (a `return`/`let`/assign that
/// hands the value over, on BOTH backends) and the shape is admitted.
///
/// All three paths are asserted together on purpose. They fail in opposite
/// directions — the escaping path doubles, the dying path loses — so a test
/// covering one would pass a fix that broke the other.
#[test]
fn e2e_a_param_with_two_exits_runs_one_body_on_every_path() {
    let src = "struct Res { id: i64, tag: String }\n\
             impl Drop for Res {\n\
             \x20   fn drop(mut ref self) { println(f\"drop {self.id} {self.tag}\"); }\n\
             }\n\
             struct Wrap { r: Res }\n\
             fn via_let(sink: mut ref Vec[Res], r: Res) -> Option[Wrap] {\n\
             \x20   if r.id > 200 { let w: Wrap = Wrap { r: r }; return Some(w); }\n\
             \x20   if r.id > 100 { sink.push(r); }\n\
             \x20   return None;\n\
             }\n\
             fn main() {\n\
             \x20   let mut s: Vec[Res] = Vec.new();\n\
             \x20   let a: Option[Wrap] = via_let(mut s, Res { id: 301, tag: \"esc\" });\n\
             \x20   println(\"1\");\n\
             \x20   let b: Option[Wrap] = via_let(mut s, Res { id: 151, tag: \"store\" });\n\
             \x20   println(\"2\");\n\
             \x20   let c: Option[Wrap] = via_let(mut s, Res { id: 6, tag: \"die\" });\n\
             \x20   println(\"3\");\n\
             \x20   println(f\"len {s.len()}\");\n\
             }\n"
    .to_string();
    assert_eq!(
        run_program(&src),
        Some("drop 301 esc\n1\n2\ndrop 6 die\n3\nlen 1\ndrop 151 store\n".to_string()),
        "each of the three paths runs the body exactly once: escaped at the \
             caller's binding, died in the callee, stored at the container's drain"
    );
}
