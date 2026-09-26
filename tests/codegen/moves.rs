//! moves, owned values, fresh temporaries, discarded values, clones -- fixtures for `tests/codegen.rs`.
//!
//! Split out of `tests/codegen.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test codegen` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test codegen moves::
//!
//! New fixtures about moves, owned values, fresh temporaries, discarded values, clones belong in this file.

use super::*;

/// B-2026-09-05-6 — a place STRUCT argument whose FIELD the callee hands
/// back runs that field's `Drop` body exactly ONCE.
///
/// One object had two owners: the caller's own field walk fired at `g`'s
/// live-range end on a value the callee had already given away, and the
/// result's binding fired it again — `in dR13 got13 dR13` against a due
/// `in got13 dR13`. Agreed-wrong on all four surfaces, so no A/B gate saw
/// it, and valgrind-clean (33 allocs / 33 frees post-fix), because the
/// entry copy gives each body a buffer of its own.
///
/// Four legs: the destructure spelling (`g1`); a two-field struct (`g2`)
/// where only `a` escapes, so `dR32` is DUE inside the call and pins the
/// mask as per-FIELD rather than a suppression of the binding's whole
/// walk; the PROJECTION spelling (`g3`, `return h.r`), which the row
/// reported clean and is not; and the GENERIC callee (`g4`), which never
/// reaches `compile_call`'s argument loop, so the free-function arm alone
/// left this column at two bodies against the interpreter's one. `g5` pins
/// the discarded-result form.
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
#[test]
fn test_e2e_generic_tuple_param_element_is_owned_once() {
    let out = run_program(
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
"#,
    );
    if let Some(out) = out {
        assert_eq!(
                out, "in\ngot91\ndR91\nin2\ngot92\ndR92\nin3\ndR93\ngotz7\nin\ndR94\nafter\nin5\ngot95\ndR95\nin6\ndR96\nafter6\nin7\ndR97\nafter7\nend\n",
                "one body per object on the AOT column, and no double free at any \
                 optimization level; got {out:?}"
            );
    }
}

/// B-2026-09-05-29 — a discarded GENERIC call whose callee returns its WHOLE
/// by-value param, called on a TEMPORARY: `fn passG[T](x: T) -> T` under
/// `let _ = passG(mk(80));`. No `Drop` body ran on ANY compiled surface —
/// `karac run`, `karac build`, `KARAC_AUTO_PAR=0`, and `-O0` all printed
/// `inP after` against the interpreter's `inP dR80 after` — and valgrind
/// measured 12 allocs / 10 frees, 11 B definitely lost in 2 blocks. The
/// whole-param spelling of B-2026-09-05-18's third defect, filed apart
/// because it reaches no place argument and so needs a different resolver.
///
/// The CAUSE is one resolver short, not a new mechanism.
/// `try_track_discarded_user_drop_temp` names a discarded call's type
/// through `fn_return_type_names`, which only `declare_function` fills; a
/// generic TEMPLATE is never declared (only its monomorphs are), so the
/// table holds no entry and the registrar declined outright. That left the
/// result with NO owner at all: `call_arg_flows_into_return` had already
/// stood the caller-side argument drop down, on the reasoning that the
/// RESULT would carry it, and nothing did.
///
/// The fix resolves the name from the SIGNATURE — `-> T` over `x: T` is a
/// type-level identity, so the result's concrete type is the argument's —
/// and NOT from `fn_returns_param`, the predicate the row nominated. That
/// one is deliberately conservative and answers `true` for a return site
/// that WRAPS the param, whose result is the wrapper's type and not the
/// argument's; cell `e` is that shape and is the pin for it.
///
/// Cells: the row's own shape (`a`); its NON-GENERIC twin (`b`), correct
/// all along through `fn_return_type_names`; the NAMED-LOCAL argument
/// (`c`), which this row left declined — correct on the BODY count it
/// asserts here, and wrong on memory: B-2026-09-05-31 measured the
/// callee's entry copy orphaned in exactly that cell and admits it, so the
/// one body pinned below is now the binding's stand-down plus the copy's
/// registration rather than the binding alone; TWO temporaries where
/// only the second escapes (`d`); the WRAPPING return (`e`) and the SCALAR
/// return (`f`), both of which this arm must decline; the BOUND result
/// (`g`), which never wanted a discard owner; and the LOOP, where the miss
/// was unbounded rather than one-shot — 33 B in 6 blocks over three
/// iterations pre-fix.
///
/// Measured pre-fix on this exact program: five bodies missing (`dR80`,
/// `dR84`, and all three loop cells), and every other cell already right.
#[test]
fn test_e2e_generic_whole_param_discarded_temp_runs_one_body() {
    let out = run_program(
        r#"
struct R { id: i64, tag: String, xs: Vec[i64] }
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
"#,
    );
    if let Some(out) = out {
        assert_eq!(
                out, "inP\ndR80\na\ninN\ndR81\nb\ninP\ndR82\nc\ninB\ndR83\ndR84\nd\ninW\ndR85\ne\ninS\ndR86\nf\ninP\nk87\ndR87\ninP\ndR88\nh\ninP\ndR90\ninP\ndR91\ninP\ndR92\ng\nend\n",
                "the discarded generic result owes exactly one body per object on \
                 the AOT column, at every optimization level; got {out:?}"
            );
    }
}

#[test]
/// B-2026-09-07-2 — a DISCARDED associated-function call registered no owner
/// at all: its returned value's `Drop` body ran on no compiled backend and
/// its heap leaked.
///
/// `try_track_discarded_user_drop_temp` resolves the discarded value's type
/// by matching the tail expression, and its `Call` arm handled only an
/// `Identifier` callee. `Type.fn(args)` parses as a `Call` whose callee is a
/// two-segment PATH, so it fell to the arm's `_ => None` and the whole
/// battery below registered nothing. `Q.make2();` — no argument anywhere in
/// it — printed `dQ62` under `--interp` and nothing on any compiled surface,
/// losing 3 B in 1 block.
///
/// The INSTANCE-METHOD twin `h.make3()` and the FREE-FUNCTION twin `mke2()`
/// are the controls that locate it: both were always correct, because a
/// `MethodCall` reaches the receiver-keyed arm and a bare `Identifier`
/// reaches the free-fn arm. Only the assoc spelling had no arm.
///
/// `Ev.mke()` is the case the fix deliberately EXCLUDES, and it is here so
/// the exclusion cannot be silently dropped: a discarded assoc call
/// returning a user ENUM already has an owner on this leg, and registering
/// for it produced `dQ71 dQ71` at -O2 and a double free under `karac run`
/// and at -O0. `Ev.A(mkq(70))` (a bare variant ctor, the other two-segment
/// path that reaches the same arm) and `Q.count()` (a non-`Drop` return)
/// pin the two other shapes the guard has to keep out. `W.mkw()` is the
/// no-own-`Drop`-but-contains-one case, which routes through the same arm's
/// field-bodies walk.
fn test_e2e_discarded_assoc_fn_call_owns_its_result() {
    let out = run_program(
        r#"
struct Q { id: i64, name: String }
impl Drop for Q { fn drop(mut ref self) { println(f"dQ{self.id}") } }
fn mkq(i: i64) -> Q { return Q { id: i, name: f"q{i}" }; }
struct W { q: Q, n: i64 }
enum Ev { A(Q), B(i64) }
struct Hold { n: i64 }
impl Q {
  fn make2() -> Q { return mkq(62); }
  fn passq(q: Q) -> Q { return q; }
  fn count() -> i64 { return 7; }
}
impl W { fn mkw() -> W { return W { q: mkq(77), n: 1 }; } }
impl Ev { fn mke() -> Ev { return Ev.A(mkq(71)); } }
impl Hold { fn make3(ref self) -> Q { return mkq(63); } }
fn mke2() -> Ev { return Ev.A(mkq(76)); }
fn main() {
  let h = Hold { n: 0 };
  Q.make2();
  Q.passq(mkq(64));
  let _ = Q.make2();
  Q.count();
  W.mkw();
  Ev.mke();
  Ev.A(mkq(70));
  mke2();
  h.make3();
  println("end");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out, "dQ62\ndQ64\ndQ62\ndQ77\ndQ71\ndQ70\ndQ76\ndQ63\nend\n",
            "a discarded `Type.fn(..)` result is the caller's to own, and \
                 exactly once — the enum-return and variant-ctor spellings \
                 already have an owner and must stay out of it; got {out:?}"
        );
    }
}

#[test]
/// B-2026-09-07-5 — a callee that STORES its by-value argument into a place
/// that outlives the call owns it, and the caller must not also.
///
/// The argument registrars ran the STORE route through `escapes_frame`,
/// which picks the registrar's bodies-vs-memory MODE and never declines the
/// registration. For a param the prologue refuses to COPY — a struct with a
/// `shared` field fails `aggregate_param_copy_supported_struct`, so the
/// param FORWARDS the caller's object — the memory-only registration was a
/// second owner of the very buffer the callee had just handed to the
/// vector. `impl Box2 { fn push(mut ref self, r: R) { self.xs.push(r); } }`
/// over `b.push(mk(16))` aborted `free(): double free detected in tcache 2`
/// under `karac run` and at BOTH opt levels while `--interp` printed
/// `a1 dR16` correctly.
///
/// FOUR LEGS, not one. The row was filed as method-only; the free-function
/// twin (`c`), the assoc-fn twin (`d`) and the MONOMORPH leg (`e`,
/// `fn stashg[T](v: mut ref Vec[T], x: T)`) abort identically, so the gate
/// is repeated in `method_call.rs`, `assoc_call.rs`, `call_dispatch.rs` and
/// `mono.rs`. Cells `f`–`j` are the shapes that reach the same registrars by
/// other spellings: a bare `mut ref Vec[R]` param, two stored arguments in
/// one call, a callee that stores AND returns, a struct LITERAL argument,
/// and one level of forwarding (`fn_moves_param_into_outliving_place_via_-
/// call`).
///
/// `k`/`l` ARE THE CONTROL THAT PICKS THE PREDICATE. The store analysis is a
/// MAY-analysis (`any` at every branch) with an ALWAYS sibling, which is the
/// same union-vs-all-paths fork B-2026-09-06-70 had to resolve the other
/// way. Here the MAY reading is the correct one, measured rather than
/// assumed: `fn maybe(mut ref self, r: R, k: bool) { if k { self.xs.push(r);
/// } }` is correct on all five surfaces with 0 valgrind errors at BOTH `k`
/// values, because the callee registers a guarded body drop whenever the
/// ALWAYS predicate is false. Standing the caller down on the non-storing
/// path loses nothing.
///
/// `m`/`n` ARE THE OTHER CONTROL, and the reason the gate cannot be
/// unconditional. B-2026-08-26-9 put the store route into `escapes_frame`
/// for the BODY and deliberately KEPT the memory half registered: a
/// COPY-SUPPORTED element is deep-copied at callee entry, so the caller's
/// original really is orphaned, and "suppressing the whole registration
/// instead orphaned it and traded the double body for a 9-byte leak". That
/// reasoning is exactly right for a copy-supported param and exactly wrong
/// for one the prologue declines to copy — which is why the entry-copy
/// carve-outs the RETURN route already used are shared with this clause
/// rather than re-derived.
///
/// A NAMED-LOCAL argument is deliberately absent: every arm of the registrar
/// matches a PRODUCER shape, so an `Identifier` registers nothing here.
/// `let a = mk(28); b.push(a);` double-frees through the BINDING's own
/// cleanup and runs two `Drop` bodies on the INTERPRETER too — a different
/// mechanism, on its own row.
///
/// Non-vacuous on the parent: 35 valgrind errors from 24 contexts, and an
/// abort on every compiled surface.
fn test_e2e_stored_argument_is_owned_by_its_new_home_not_the_caller() {
    let out = run_program(
        r#"
shared struct Inner { v: i64 }
struct R { id: i64, name: String, inner: Inner }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}", inner: Inner { v: i } }; }

struct S { id: i64, name: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mks(i: i64) -> S { return S { id: i, name: f"s{i}" }; }

struct Box2 { mut xs: Vec[R] }
impl Box2 {
    fn push(mut ref self, r: R) { self.xs.push(r); }
    fn put2(mut ref self, a: R, c: R) { self.xs.push(a); self.xs.push(c); }
    fn puts(mut ref self, r: R) -> i64 { self.xs.push(r); return 5; }
    fn maybe(mut ref self, r: R, k: bool) { if k { self.xs.push(r); } }
    fn stash(b: mut ref Box2, r: R) { b.xs.push(r); }
}
struct Box3 { mut one: R }
impl Box3 { fn set(mut ref self, r: R) { self.one = r; } }
struct BoxS { mut xs: Vec[S] }
impl BoxS { fn add(mut ref self, s: S) { self.xs.push(s); } }

fn take(b: mut ref Box2, r: R) { b.xs.push(r); }
fn inner_push(b: mut ref Box2, r: R) { b.xs.push(r); }
fn outer_push(b: mut ref Box2, r: R) { inner_push(b, r); }
fn pushv(v: mut ref Vec[R], r: R) { v.push(r); }
fn stashg[T](v: mut ref Vec[T], x: T) { v.push(x); }
fn takes(b: mut ref BoxS, s: S) { b.xs.push(s); }

fn c_method() { let mut b = Box2 { xs: Vec.new() }; b.push(mk(16)); println(f"a{b.xs.len()}"); }
fn c_field()  { let mut b = Box3 { one: mk(1) }; b.set(mk(17)); println(f"b{b.one.id}"); }
fn c_free()   { let mut b = Box2 { xs: Vec.new() }; take(mut b, mk(18)); println(f"c{b.xs.len()}"); }
fn c_assoc()  { let mut b = Box2 { xs: Vec.new() }; Box2.stash(mut b, mk(19)); println(f"d{b.xs.len()}"); }
fn c_generic(){ let mut v: Vec[R] = Vec.new(); stashg(mut v, mk(20)); println(f"e{v.len()}"); }
fn c_vecref() { let mut v: Vec[R] = Vec.new(); pushv(mut v, mk(21)); println(f"f{v.len()}"); }
fn c_two()    { let mut b = Box2 { xs: Vec.new() }; b.put2(mk(22), mk(23)); println(f"g{b.xs.len()}"); }
fn c_ret()    { let mut b = Box2 { xs: Vec.new() }; let z = b.puts(mk(24)); println(f"h{z}{b.xs.len()}"); }
fn c_lit()    { let mut b = Box2 { xs: Vec.new() }; b.push(R { id: 25, name: "n", inner: Inner { v: 1 } }); println(f"i{b.xs.len()}"); }
fn c_viacall(){ let mut b = Box2 { xs: Vec.new() }; outer_push(mut b, mk(26)); println(f"j{b.xs.len()}"); }
fn c_cond_no(){ let mut b = Box2 { xs: Vec.new() }; b.maybe(mk(27), false); println(f"k{b.xs.len()}"); }
fn c_cond_yes(){ let mut b = Box2 { xs: Vec.new() }; b.maybe(mk(28), true); println(f"l{b.xs.len()}"); }
fn c_copy_m() { let mut b = BoxS { xs: Vec.new() }; b.add(mks(41)); println(f"m{b.xs.len()}"); }
fn c_copy_f() { let mut b = BoxS { xs: Vec.new() }; takes(mut b, mks(42)); println(f"n{b.xs.len()}"); }

fn main() {
    c_method(); c_field(); c_free(); c_assoc(); c_generic(); c_vecref();
    c_two(); c_ret(); c_lit(); c_viacall(); c_cond_no(); c_cond_yes();
    c_copy_m(); c_copy_f();
    println("end");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
                out, "a1\ndR16\ndR1\nb17\ndR17\nc1\ndR18\nd1\ndR19\ne1\ndR20\nf1\ndR21\ng2\ndR22\ndR23\nh51\ndR24\ni1\ndR25\nj1\ndR26\ndR27\nk0\nl1\ndR28\nm1\ndS41\nn1\ndS42\nend\n",
                "a callee that stores its by-value argument into an outliving \
                 place owns it; the caller registers a second owner only where \
                 the callee's entry copy orphaned the original; got {out:?}"
            );
    }
}

#[test]
/// B-2026-09-06-69 — a MIXED-PATH callee hands its by-value param back on
/// one exit and lets it die on another, and exactly one frame frees it
/// either way.
///
/// The param class here is the one whose prologue REFUSES to own it — a
/// struct with a `shared` field, or a self-referential one, neither
/// copy-supported nor eligible for the transfer bargain — so the callee
/// FORWARDS the caller's object instead of copying it. With the caller
/// owning the buffer statically, the hand-back exit had two owners (its own
/// temp and the result binding) and aborted `free(): double free detected
/// in tcache 2` under `karac run` and at both opt levels; standing the
/// caller down instead left the dies-inside exit with none and stranded
/// 18 B at `-O0`. Neither half is a fix alone, which is why the memory now
/// sits on the callee's per-path registration beside the `Drop` body and the
/// caller retracts.
///
/// `bare`/`d` is the cell that shows the defect was never about the rebind:
/// the no-rebind spelling reached the caller's stand-down through
/// `fn_returns_param`'s union all along, so its dies-inside exit leaked the
/// same 18 B on the PARENT — the row filed that spelling as "clean on every
/// surface" having measured only its hand-back exit.
///
/// Controls, each pinning one gate rather than decorating the fixture:
/// `copyok` is entry-copied, so the caller keeps its own object and nothing
/// here may move; `never` consumes the param without handing it back, so
/// the conditional predicate must decline it; `selfref` is the second
/// declined-copy class; `wrap` reaches the hand-back through an
/// `Option.Some` constructor rather than a bare tail.
fn test_e2e_conditional_handback_of_a_rebound_param_frees_once() {
    let out = run_program(
        r#"
shared struct Inner { v: i64 }
struct R { id: i64, name: String, inner: Inner }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}", inner: Inner { v: i } }; }
struct N { id: i64, name: String, next: Option[N] }
impl Drop for N { fn drop(mut ref self) { println(f"dN{self.id}") } }
fn mkn(i: i64) -> N { return N { id: i, name: f"h{i}", next: Option.None }; }
struct P { id: i64, name: String }
impl Drop for P { fn drop(mut ref self) { println(f"dP{self.id}") } }
fn mkP(i: i64) -> P { return P { id: i, name: f"p{i}" }; }

fn reb(r: R, c: bool) -> R { let m = r; if c { return m; } return mk(99); }
fn bare(r: R, c: bool) -> R { if c { return r; } return mk(98); }
fn arm(r: R, c: bool) -> R { let m = r; match c { true => m, false => mk(97) } }
fn two(r: R, c: bool) -> R { let m = r; let n = m; if c { return n; } return mk(96); }
fn rd(r: R, c: bool) -> R { let m = r; if c { return m; } println(f"in={m.name}"); return mk(95); }
fn wrap(r: R, c: bool) -> Option[R] { let m = r; if c { return Option.Some(m); } return Option.None; }
fn selfref(n: N, c: bool) -> N { let m = n; if c { return m; } return mkn(94); }
fn copyok(p: P, c: bool) -> P { let m = p; if c { return m; } return mkP(93); }
fn never(r: R, c: bool) -> i64 { let m = r; if c { return m.id; } return 0; }

fn main() {
  let a = reb(mk(1), true);  println(f"a={a.inner.v}");
  let b = reb(mk(2), false); println(f"b={b.inner.v}");
  let c = bare(mk(3), true);  println(f"c={c.inner.v}");
  let d = bare(mk(4), false); println(f"d={d.inner.v}");
  let e = arm(mk(5), true);  println(f"e={e.inner.v}");
  let f = arm(mk(6), false); println(f"f={f.inner.v}");
  let g = two(mk(7), true);  println(f"g={g.inner.v}");
  let h = two(mk(8), false); println(f"h={h.inner.v}");
  let i2 = rd(mk(9), true);  println(f"i={i2.inner.v}");
  let j = rd(mk(10), false); println(f"j={j.inner.v}");
  match wrap(mk(11), true) { Option.Some(v) => println(f"k={v.inner.v}"), Option.None => println("kn") }
  match wrap(mk(12), false) { Option.Some(v) => println(f"l={v.inner.v}"), Option.None => println("ln") }
  let m2 = selfref(mkn(13), true);  println(f"m={m2.name}");
  let n2 = selfref(mkn(14), false); println(f"n={n2.name}");
  let o = copyok(mkP(15), true);  println(f"o={o.name}");
  let p2 = copyok(mkP(16), false); println(f"p={p2.name}");
  println(f"q={never(mk(17), true)}");
  println(f"r={never(mk(18), false)}");
  reb(mk(19), true);
  reb(mk(20), false);
  let mut z = 0;
  while z < 2 { let w = reb(mk(21), z == 0); println(f"w={w.inner.v}"); z = z + 1; }
  println("end");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
                out, "a=1\ndR1\ndR2\nb=99\ndR99\nc=3\ndR3\ndR4\nd=98\ndR98\ne=5\ndR5\ndR6\nf=97\ndR97\ng=7\ndR7\ndR8\nh=96\ndR96\ni=9\ndR9\nin=h10\ndR10\nj=95\ndR95\nk=11\ndR11\ndR12\nln\nm=h13\ndN13\ndN14\nn=h94\ndN94\no=p15\ndP15\ndP16\np=p93\ndP93\ndR17\nq=17\ndR18\nr=0\ndR19\ndR20\ndR99\nw=21\ndR21\ndR21\nw=99\ndR99\nend\n",
                "a MIXED-PATH callee over a param its prologue declined to own \
                 must free that buffer itself where the value dies inside and \
                 leave it to the result binding where it is handed back; got {out:?}"
            );
    }
}

#[test]
fn e2e_self_field_move_out_tail_return_no_double_free() {
    // B-2026-07-18-39: a by-value-`self` method returning a HEAP field
    // directly as the tail (`fn get(self) -> String { self.v }`) double-freed
    // under AOT — `self.v` parses as `FieldAccess { object: SelfValue, .. }`,
    // not `Identifier("self")`, so the tail-return field-move-out suppression
    // (which cap-zeroes the moved field so `self`'s callee-owned StructDrop
    // skips it) never fired for `self`. The free-fn `fn get(b: B) { b.v }`
    // form already worked (Identifier arm). Prints the field once, cleanly.
    if let Some(out) = run_program(
        "struct B { v: String, n: i64 }\n\
             impl B { fn get(self) -> String { self.v } }\n\
             fn main() {\n\
                 let b = B { v: \"hi\".to_string(), n: 5 };\n\
                 println(b.get());\n\
             }",
    ) {
        assert_eq!(out, "hi\n");
    }
}

/// B-2026-08-04-16 — moving a named heap value into a TUPLE ELEMENT must
/// disarm the source, the way the struct-field spelling already does.
///
/// `compile_tuple_index_store` drops the old element and MOVES the RHS
/// header into the slot, but the assign arm never got the sibling of
/// B-2026-07-15-25's field-assign move-suppression. The source binding kept
/// its scope-exit cleanup armed while owning nothing, so it and the tuple's
/// element drop freed the same buffer: `free(): double free detected` under
/// AOT and the JIT, while `karac run --interp` printed the right answer.
///
/// The bug did NOT need the move-OUT half the report led with. Arm (b) is
/// the minimal shape — a plain `t.0 = <named source>`, no move-out anywhere
/// — and it aborted identically. Arm (e) is a `Vec[String]` element, which
/// was a TRIPLE free (11 allocations, 13 frees: the element buffer and the
/// Strings inside it).
///
/// The report's own `let mut e = t.0; …; t.0 = e;` spelling is deliberately
/// NOT here: the ownership checker warns on it (`value 't' moved here, used
/// again here` — an over-broad partial move, filed as B-2026-08-04-18), so
/// it trips the harness's ownership gate. Nothing is lost by leaving it out
/// — it reaches this codegen path through exactly the assignment arm (b)
/// covers, and B-2026-08-04-18 carries a probe for the spelling itself.
///
/// The two controls are what localize it: a fresh-temp RHS (f) was always
/// correct because there is no source binding to disarm, and the
/// struct-FIELD spelling (g) has been correct since B-2026-07-15-25 — that
/// asymmetry is what identified the missing arm. Seeded from
/// `env.args().len()` and every arm reads an element or the bytes, so the
/// payloads survive `-O2` instead of folding away (B-2026-08-04-17).
#[test]
fn e2e_named_source_moved_into_a_tuple_element_is_disarmed() {
    let Some(out) = run_program(
            "struct H { items: Vec[i64], n: i64 }\n\
             fn mkvec(k: i64) -> Vec[i64] { let mut v: Vec[i64] = Vec.new(); v.push(k); v.push(k + 1i64); return v; }\n\
             fn mkstr(k: i64) -> String { let mut s: String = String.new(); s.push_str(f\"payload-{k}\"); return s; }\n\
             fn digits(i: i64) -> String { let mut d: String = String.new(); d.push_str(f\"{i}\"); return d; }\n\
             fn mkvs(k: i64) -> Vec[String] { let mut v: Vec[String] = Vec.new(); v.push(mkstr(k)); v.push(mkstr(k + 1i64)); return v; }\n\
             fn main() {\n\
             \x20   let n: i64 = env.args().len();\n\
             \x20   // (b) the minimal shape the report missed: NAMED source, no move-out\n\
             \x20   let mut t2: (Vec[i64], i64) = (mkvec(n), 3i64);\n\
             \x20   let f: Vec[i64] = mkvec(n + 10i64);\n\
             \x20   t2.0 = f;\n\
             \x20   println(f\"b:{t2.0.len()}:{t2.0[0i64]}\");\n\
             \x20   // (c) SECOND element position\n\
             \x20   let mut t3: (i64, Vec[i64]) = (3i64, mkvec(n));\n\
             \x20   let g: Vec[i64] = mkvec(n + 20i64);\n\
             \x20   t3.1 = g;\n\
             \x20   println(f\"c:{t3.1[0i64]}\");\n\
             \x20   // (d) String element\n\
             \x20   let mut t4: (String, i64) = (mkstr(n), 3i64);\n\
             \x20   let s: String = mkstr(n + 30i64);\n\
             \x20   t4.0 = s;\n\
             \x20   if t4.0.contains(digits(n + 30i64)) { println(f\"d:{t4.0.len()}\"); } else { println(\"d:BAD\"); }\n\
             \x20   // (e) Vec[String] element — the TRIPLE-free shape\n\
             \x20   let mut t5: (Vec[String], i64) = (mkvs(n), 3i64);\n\
             \x20   let w: Vec[String] = mkvs(n + 40i64);\n\
             \x20   t5.0 = w;\n\
             \x20   println(f\"e:{t5.0.len()}:{t5.0[0i64].len()}:{t5.0[1i64].len()}\");\n\
             \x20   // CONTROL: a fresh-temp RHS has no source binding and was always correct\n\
             \x20   let mut t6: (Vec[i64], i64) = (mkvec(n), 3i64);\n\
             \x20   t6.0 = mkvec(n + 50i64);\n\
             \x20   println(f\"f:{t6.0[0i64]}\");\n\
             \x20   // CONTROL: the struct-FIELD spelling, correct since B-2026-07-15-25\n\
             \x20   let mut h: H = H { items: mkvec(n), n: 3i64 };\n\
             \x20   let hv: Vec[i64] = mkvec(n + 60i64);\n\
             \x20   h.items = hv;\n\
             \x20   println(f\"g:{h.items[0i64]}\");\n\
             \x20   println(\"end\");\n\
             }\n",
        ) else {
            return;
        };
    assert_eq!(out, "b:2:11\nc:21\nd:10\ne:2:10:10\nf:51\ng:61\nend\n");
}

/// B-2026-07-31 (container-bodies move disarm) — a WHOLE-VALUE move of a
/// binding carrying a container-bodies walk fires the payload body exactly
/// ONCE, at the destination. Before the disarm, `let a2 = a;` on an enum
/// with a Drop-bearing payload printed the body twice under the
/// interpreter and — worse — the second codegen fire read the cap-zeroed
/// moved-from slot, printing `90` (`self.id` == 0): a silently WRONG
/// value, invisible to every sanitizer. Shapes: rebind (enum/tuple/Vec),
/// return-move, reassign, by-value call arg. The tuple rebind
/// additionally pins the `tuple_var_elem_tes` propagation — without it
/// the destination cannot re-register and codegen goes silent where the
/// interpreter fires.
///
/// Twin of `tests/interpreter.rs`'s
/// `test_container_bodies_whole_value_move_single_fire`, same source and
/// expected string — the pair is the parity contract.
#[test]
fn e2e_container_bodies_whole_value_move_single_fire() {
    let Some(out) = run_program(
        "struct Res { id: i64 }\n\
             impl Drop for Res { fn drop(mut ref self) { println(90 + self.id); } }\n\
             enum Slot { Empty, Held(Res) }\n\
             fn make() -> Slot {\n\
             \x20   let m = Slot.Held(Res { id: 3 });\n\
             \x20   return m;\n\
             }\n\
             fn consume(s: Slot) {\n\
             \x20   println(70);\n\
             }\n\
             fn main() {\n\
             \x20   let a = Slot.Held(Res { id: 1 });\n\
             \x20   let a2 = a;\n\
             \x20   println(1);\n\
             \x20   let t = (Res { id: 2 }, 10);\n\
             \x20   let t2 = t;\n\
             \x20   println(2);\n\
             \x20   let c = make();\n\
             \x20   println(3);\n\
             \x20   let v: Vec[Res] = [Res { id: 4 }];\n\
             \x20   let v2 = v;\n\
             \x20   println(4);\n\
             \x20   let mut f = Slot.Held(Res { id: 5 });\n\
             \x20   let g = Slot.Held(Res { id: 6 });\n\
             \x20   f = g;\n\
             \x20   println(5);\n\
             \x20   let b = Slot.Held(Res { id: 7 });\n\
             \x20   consume(b);\n\
             \x20   println(6);\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(
        out,
        // One fire per moved value, at its destination's live-range end.
        // No `90` anywhere (no body over zeroed data). `95` is the
        // reassign target's OVERWRITTEN original firing at the
        // assignment — originally a shared residual silence on both
        // backends, closed by the B-2026-07-30-11 enum-assign
        // displacement leg (both backends fire it identically).
        "91\n1\n92\n2\n93\n3\n94\n4\n95\n96\n5\n70\n97\n6\n"
    );
}

/// B-2026-08-31-7 — A BARE-TUPLE ELEMENT BOUND OUT OF AN OWNED PARAM IS A
/// VIEW, AND A REBIND OF IT MUST INHERIT THAT.
///
/// `match t { (r, k) => { let m = r; … } }` over `fn s1(t: (R, i64))` printed
/// `b1 dR1 dR1` on all three compiled surfaces against `--interp`'s correct
/// `b1 dR1`; the PROJECTED spelling `match s.t { … }` over `fn s1c(s: W)`
/// printed `b3 dR3 dR3` on ALL FOUR, agreed and — by one-value-one-body —
/// agreed-wrong. One `R` is constructed and passed by value, so one body is
/// due in each.
///
/// The two halves are one mechanism seen from two sides. Codegen wrote
/// `param_view_locals` only from the VARIANT-payload site, so a bare-tuple
/// element never became a view and `let m = r` minted a second owner; the
/// interpreter's projection branch admitted only `TupleVariant`/`Struct`
/// patterns, so it minted one too for the `s.t` spelling. Its bare-identifier
/// branch has always had the wider reach, which is why `s1` alone diverged.
///
/// THE CONTROLS ARE THE POINT, because the fix WITHHOLDS a body and the
/// failure mode of over-reaching is a body that never runs:
/// - `norebind` / `norebind_proj` — the same arms without the rebind, correct
///   before and after. They are what put the axis on the rebind rather than on
///   the pattern, and they prove the element WALK still owns the single body.
/// - `consumed` — the arm moves the element into a by-value callee. Already at
///   one body, and marking `r` a view must not disturb the transfer.
/// - `local` — a LOCAL tuple, not a param. Nobody else owns it, so caller-
///   retains does not apply and this shape was deliberately NOT fixed here. It
///   was pinned at TWO bodies so the marking could not be quietly widened to
///   locals without a measurement. B-2026-09-02-26 supplied that measurement
///   and the cell now runs ONE — but the marking was still not widened: that
///   row RETRACTS the tuple's element walk for the moved element and lets the
///   rebind own the body, the OPPOSITE direction of this row's repair. The
///   cell stays pinned here as the guard that the two do not overlap into a
///   body that runs nowhere.
///
/// The ENUM twins of the two fixed shapes were already correct on all four
/// surfaces before this row, which is what shows the tuple family was simply
/// behind the enum family rather than that a new rule was invented.
///
/// Twin of `tests/interpreter.rs`'s
/// `test_bare_tuple_element_of_owned_param_is_a_view`, pinned to the same
/// string.
#[test]
fn e2e_bare_tuple_element_of_owned_param_is_a_view() {
    let Some(out) = run_program(
        r#"struct R { id: i64 }
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
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
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
/// `let (r, k) = t;` over an owned TUPLE PARAM binds views of the callee's
/// entry copy exactly as `match t { (r, k) => … }` does, so a later whole
/// rebind (`let m = r;`) must inherit the withholding. It did not: `param`
/// below printed `b1 dR1 dR1` where one body is due — agreed on all four
/// surfaces — while `arm`, the identical signature written as a match, was
/// already correct at one.
///
/// TWO HALVES, LANDED TOGETHER because either alone is a divergence:
/// - INTERPRETER: `let_destructures_owned_param` returned `true` for a
///   `Tuple` pattern over an owned-param RHS — correctly retracting the
///   destructure's OWN slots — without inserting the bound names into
///   `owned_param_names_stack` the way every sibling branch does. So `r` was
///   not a known view and the rebind minted a second owner.
/// - CODEGEN: `place_source_tuple_leaf_cleanups` already passes
///   `!owner_runs_bodies` (i.e. `false`) when the source is a by-value param,
///   registering the leaf for MEMORY only — but never recorded the leaf in
///   `param_view_locals`, so the let-site's `rhs_is_param_view` said no.
///
/// `letelse` is the cell that shows the two halves are one rule: codegen was
/// ALREADY at one body there and the interpreter was the lone doubler, so it
/// converges from the interpreter side alone.
///
/// THE PINNED-AT-TWO CELLS ARE THE POINT, and each is held back by a
/// measurement rather than by taste. All four surfaces agree at two bodies on
/// every one of them, and marking a view on ONE side would turn that agreement
/// into a run-vs-build split:
/// - `structpat` — `let S { r, k } = s;`. FIXED SINCE, by B-2026-09-02-38,
///   and the reason it was pinned turned out to hold for a different source
///   than this one. `finish_owned_struct_destructure` does TRANSFER the body
///   to the leaf rather than leaving it with the source — for a LOCAL
///   source. For the PARAM source this cell uses it does not: the transfer
///   is gated on `var_owns_struct_field_bodies`, i.e. on the source having a
///   `StructFieldBodies` action, and a by-value param has none. Measured
///   with an END-OF-CALLEE marker, the body here fires AFTER that marker on
///   both backends (the source's owner) where the local spelling fires it
///   BEFORE (the leaf's live-range end) — so a view mark is exactly right,
///   and -38 lifted both sides together. One body here now, and
///   `e2e_struct_pattern_destructure_of_owned_param_is_a_view` pins the
///   widened shape in full.
/// - `nested` — `let ((r, a), b) = t;`. FIXED SINCE, by B-2026-09-02-39: a
///   tuple PARAM used to register no `tuple_var_elem_type_exprs`, so its
///   nested element resolved to an EMPTY `TypeExpr` and the compiled
///   recursion never reached the leaf. Registering the param's declared
///   element types made it reachable, and the two backends were lifted
///   together in that commit — one body here now. Kept in this list because
///   the cell is what proved the restriction was a REACHABILITY limit rather
///   than an ownership judgement.
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
///   projection source already satisfied codegen's `owner_runs_bodies` (its
///   root is a param); only the interpreter's gate wanted a bare identifier,
///   and codegen's marking was narrowed to identifiers purely to match it.
///   -40 taught the interpreter the field-chain shape and lifted both sides
///   together — one body here now. Kept in this list because it is the cell
///   that shows a pin can be a MATCHING restriction rather than an ownership
///   judgement, and `e2e_projection_source_tuple_destructure_is_a_view` is
///   where the widened shape is pinned in full.
///
/// `norebind` and `local` are the over-reach controls: withholding a body
/// fails by running none, and both must stay at exactly one.
///
/// Twin of `tests/interpreter.rs`'s
/// `test_let_tuple_destructure_of_owned_param_is_a_view`, pinned to the same
/// string.
#[test]
fn e2e_let_tuple_destructure_of_owned_param_is_a_view() {
    let Some(out) = run_program(
        r#"struct R { id: i64 }
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
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
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

/// B-2026-09-05-20 / B-2026-09-05-21 — the two defects on the cells next to
/// B-2026-09-05-7's: a nested by-value param destructured in the callee,
/// with the leaf `inner` READ or MOVED INTO A LOCAL.
///
/// `one`/`two` (-20): the NON-generic `Hn { inner: Hd { r: R } }` is
/// entry-copied, so its field bodies are the CALLER's (its walk runs after
/// the call returns). Reading `inner.z` went through the field-move
/// disarm, which re-arms a binding's walk under a wider mask — and MINTED
/// one for a param view that had none: `r`'s body twice on every compiled
/// backend. A view that owns no walk now has nothing to disarm.
///
/// `three`/`four` (-21): the generic `Gn2[T] { inner: Gd[T] }` reaches the
/// callee by transfer; the destructure's memory transfer tracked the leaf
/// as the erased `Gd` (nothing synthesized) and zeroed the source field
/// with the erased walker (nothing zeroed), so `inner` was a memory-less
/// alias and `let g: Gd[T] = inner` freed buffers `h` freed again:
/// `free(): double free detected in tcache 2`. Three pieces: the leaf is
/// tracked under its instantiation, the source field is zeroed under the
/// field's subst, and the instantiation a binding records inside a
/// monomorph is the SUBSTITUTED one — a declared `Gd[T]` is `Gd[R]` there,
/// which is what lets the rebind take `inner`'s walk over to `g` (`moved`
/// before `dR3`, the interpreter's order) instead of firing it at the move.
///
/// `five` and `six` are the controls that were always right: the generic
/// leaf read (the callee owns the bodies under transfer), and the
/// non-generic leaf moved (the rebind off a param view mints nothing).
#[test]
fn e2e_nested_param_destructure_leaf_read_and_moved_one_body_each() {
    let Some(out) = run_program(
            "struct R { id: i64, tag: String, xs: Vec[i64] }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             struct Gd[T] { r: T, z: i64 }\n\
             struct Gn2[T] { inner: Gd[T], z: i64 }\n\
             struct Hd { r: R, z: i64 }\n\
             struct Hn { inner: Hd, z: i64 }\n\
             fn mk(k: i64) -> R { return R { id: k, tag: f\"t{k}\", xs: [k] } }\n\
             fn hLive(h: Hn) -> i64 { let Hn { inner, z } = h; println(\"in\"); let q: i64 = inner.z; return z + q }\n\
             fn hLiveLocal(h: Hn) -> i64 { let Hn { inner, z } = h; println(\"in\"); let g: Hd = inner; println(\"moved\"); return z + g.z }\n\
             fn gLive[T](h: Gn2[T]) -> i64 { let Gn2 { inner, z } = h; println(\"in\"); let q: i64 = inner.z; return z + q }\n\
             fn gLiveLocal[T](h: Gn2[T]) -> i64 { let Gn2 { inner, z } = h; println(\"in\"); let g: Gd[T] = inner; println(\"moved\"); return z + g.z }\n\
             fn main() {\n\
             \x20   { let a: i64 = hLive(Hn { inner: Hd { r: mk(1), z: 1 }, z: 9 }); println(f\"one{a}\") }\n\
             \x20   { let n: Hn = Hn { inner: Hd { r: mk(2), z: 1 }, z: 9 }; let a: i64 = hLive(n); println(f\"two{a}\") }\n\
             \x20   { let a: i64 = gLiveLocal(Gn2[R] { inner: Gd[R] { r: mk(3), z: 1 }, z: 9 }); println(f\"three{a}\") }\n\
             \x20   { let n: Gn2[R] = Gn2[R] { inner: Gd[R] { r: mk(4), z: 1 }, z: 9 }; let a: i64 = gLiveLocal(n); println(f\"four{a}\") }\n\
             \x20   { let a: i64 = gLive(Gn2[R] { inner: Gd[R] { r: mk(5), z: 1 }, z: 9 }); println(f\"five{a}\") }\n\
             \x20   { let a: i64 = hLiveLocal(Hn { inner: Hd { r: mk(6), z: 1 }, z: 9 }); println(f\"six{a}\") }\n\
             \x20   println(\"end\")\n\
             }\n\
             ",
        ) else {
            return;
        };
    assert_eq!(out, "in\ndR1\none10\nin\ndR2\ntwo10\nin\nmoved\ndR3\nthree10\nin\nmoved\ndR4\nfour10\nin\ndR5\nfive10\nin\nmoved\ndR6\nsix10\nend\n");
}

/// B-2026-09-15-30 — the OUTPUT half of a leak, which is a deliberate
/// choice rather than an oversight.
///
/// `compile_mono_function` never installed `discarded_branch_spans`, so
/// every branch inside a generic instantiation read as NON-discarded and
/// its arm-tail clone was emitted with no owner. That loses 216 B over the
/// shapes below and changes NO output, so the gate that actually catches
/// it is `tests/memory_sanitizer.rs`'s
/// `asan_discarded_branch_in_a_generic_body_clones_nothing`.
///
/// What this fixture holds is the other direction. Per
/// `compute_discarded_branch_spans`' own doc, "missing a position here
/// costs a leak; wrongly INCLUDING one costs a double free" — so
/// installing the set is a switch that can go wrong loudly, and `kept`,
/// `callerkept` and `twoinst` are the cells that would say so. They print
/// a length read back out of the container AFTER the branch, which is
/// exactly what a suppressed-but-needed clone destroys.
///
/// The INTERPRETER twin is `tests/interpreter.rs`'s
/// `test_discarded_branch_in_a_generic_body_clones_nothing`, byte-identical
/// source and expectation.
#[test]
fn e2e_discarded_branch_in_a_generic_body_clones_nothing() {
    let Some(out) = run_program(
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
    ) else {
        return;
    };
    assert_eq!(out, "stmt\n  1\nloop\n  2\nblock\n  3\nmatch\n  4\nkept\n  kept 26\n  5\nnested\n  6\ncallerdiscard\n  26\ncallerkept\n  26\n  26\ntwoinst\n  7\n  8\nend\n");
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
/// Twin of `tests/interpreter.rs`'s
/// `test_deep_chain_move_out_then_bound_hop_runs_each_body_once`, byte-identical source and expectation.
#[test]
fn e2e_deep_chain_move_out_then_bound_hop_runs_each_body_once() {
    let Some(out) = run_program(
        r#"struct R { id: i64, name: String }
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
    "#,
    ) else {
        return;
    };
    assert_eq!(out, "bound\n  dR1/n1\n  dR2/n2\n  mid\n  v=3\n  dR3/n3\nrenamed\n  dR4/n4\n  dR5/n5\n  mid\n  v=6\n  dR6/n6\ntwo_outs\n  dR7/n7\n  dR8/n8\n  mid\n  v=9\n  dR9/n9\ndeep\n  dR10/n10\n  dR11/n11\n  mid\n  v=12\n  dR12/n12\nwild\n  dR13/n13\n  dR14/n14\n  mid\n  v=15\n  dR15/n15\nnosplit\n  dR17/n17\n  dR16/n16\n  mid\n  v=18\n  dR18/n18\nnestedpat\n  dR20/n20\n  dR19/n19\n  mid\n  v=21\n  dR21/n21\nend\n");
}

/// B-2026-09-10-37 — `.clone()` on an `Array[T, N]` lowers.
///
/// It bailed with codegen's own "this is a codegen bug" fall-through at
/// every depth. The row read that as a missing DISPATCHER arm over existing
/// machinery, believing `emit_clone_fn_for_type_expr` already had an
/// `Array` route. It did not — the array-shaped emitters were the DROP
/// walker and the EQ walker, and every `emit_clone_fn_for_type_expr` call
/// in `collections.rs` passes an ELEMENT type. So the capability was
/// genuinely absent, and `emit_clone_fn_for_array` is it.
///
/// `independence` is the cell that makes this a CLONE test rather than a
/// compiles-without-erroring test: it mutates the source afterwards and
/// asserts the copy did not move with it. A shallow `memcpy` of the
/// `[N x T]` would pass every other cell here and fail this one.
///
/// `derive-clone-with-array-field` is the row's own unmeasured cell, and it
/// named it "the spelling most likely to be hit by real code".
/// `indexed-element` is the spelling `E_INDEX_MOVE_NON_COPY` tells users to
/// write when they try to move an array element out — it goes through the
/// synth binding an indexed receiver mints, which has no let-site entry in
/// `array_var_elem_te` and so needed the `array_elem_type_exprs` fallback.
#[test]
fn e2e_clone_on_a_fixed_array() {
    for (label, src, want) in [
            (
                "string-elems",
                "fn main() { let n = env.args().len() as i64;\n\
                 let a: Array[String, 2] = [f\"aa-{n}\", f\"bb-{n}\"];\n\
                 let b: Array[String, 2] = a.clone(); println(f\"{b[0]}:{b[1]}\"); }\n",
                "aa-1:bb-1\n",
            ),
            (
                "nested-array",
                "fn main() { let n = env.args().len() as i64;\n\
                 let a: Array[Array[String, 2], 2] = [[f\"p-{n}\", f\"q-{n}\"], [f\"r-{n}\", f\"s-{n}\"]];\n\
                 let b: Array[Array[String, 2], 2] = a.clone();\n\
                 let i2: Array[String, 2] = b[1].clone(); println(f\"{i2[0]}\"); }\n",
                "r-1\n",
            ),
            (
                "indexed-element",
                "fn main() { let n = env.args().len() as i64;\n\
                 let a: Array[Array[String, 2], 2] = [[f\"p-{n}\", f\"q-{n}\"], [f\"r-{n}\", f\"s-{n}\"]];\n\
                 let e: Array[String, 2] = a[0].clone(); println(f\"{e[1]}\"); }\n",
                "q-1\n",
            ),
            (
                "scalar-elems",
                "fn main() { let a: Array[i64, 3] = [1, 2, 3];\n\
                 let b: Array[i64, 3] = a.clone(); println(f\"{b[2]}\"); }\n",
                "3\n",
            ),
            (
                "derive-clone-with-array-field",
                "#[derive(Clone)]\n\
                 struct S { a: Array[String, 2], n: i64 }\n\
                 fn main() { let m = env.args().len() as i64;\n\
                 let s = S { a: [f\"x-{m}\", f\"y-{m}\"], n: 7 };\n\
                 let t = s.clone(); println(f\"{t.a[0]}:{t.n}\"); }\n",
                "x-1:7\n",
            ),
            (
                "independence",
                "fn main() { let n = env.args().len() as i64;\n\
                 let mut a: Array[String, 2] = [f\"aa-{n}\", f\"bb-{n}\"];\n\
                 let b: Array[String, 2] = a.clone();\n\
                 a[0] = f\"MUT-{n}\"; println(f\"{a[0]}|{b[0]}\"); }\n",
                "MUT-1|aa-1\n",
            ),
        ] {
            assert_eq!(run_program(src).as_deref(), Some(want), "{label}");
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
/// The terminator path (`return` inside the arm) is pinned SEPARATELY
/// below. It was written codegen-only because the interpreter lost the
/// `return` itself on this shape, so asserting it on all four surfaces
/// would have written that bug into the contract as if it were the
/// contract. B-2026-08-30-14 has since FIXED that, and the shape now
/// measures `v10 dR10 dE` on all four surfaces; the row stays here because
/// what it pins is codegen-specific (the arm's cleanup is emitted on the
/// arm's OWN edge, never reaching the merge block), and the four-surface
/// coverage lives in
/// `e2e_diverging_arm_over_a_freshtemp_scrutinee_keeps_its_control_flow`
/// and its interpreter twin.
#[test]
fn e2e_freshtemp_scrutinee_body_fires_at_construct_exit() {
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
            assert_eq!(run_program(&src).as_deref(), Some(want), "{label}");
        }
    // Terminator path: the arm `return`s, so it never reaches the merge
    // block and fires the body on its OWN edge instead — the reason firing
    // exactly once per execution path is structural rather than a thing to
    // check. Compiled-only expectation; see the doc comment above.
    let src = format!(
            "{H}fn main() {{ match mk(10) {{ E.A(r) => {{ println(f\"v{{r.id}}\"); return }} E.B => {{}} }}\n\
             \x20 println(\"post\") }}\n"
        );
    assert_eq!(
        run_program(&src).as_deref(),
        Some("v10\ndR10\ndE\n"),
        "arm-returns-fires-on-that-edge"
    );
}

#[test]
fn e2e_let_move_source_frozen() {
    let Some(out) = run_program(
        "fn main() {\n\
             \x20   let mut w: Vec[i64] = Vec.new();\n\
             \x20   w.push(1);\n\
             \x20   let mut v = w;\n\
             \x20   v.push(2);\n\
             \x20   println(w.len());\n\
             \x20   println(v.len());\n\
             \x20   let outer: Vec[String] = Vec.new();\n\
             \x20   let mut grab = |x: String| {\n\
             \x20       let mut v2 = outer;\n\
             \x20       v2.push(x);\n\
             \x20       v2.len()\n\
             \x20   };\n\
             \x20   let a = grab(String.from(\"a\"));\n\
             \x20   let b = grab(String.from(\"b\"));\n\
             \x20   println(a + b);\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "1\n2\n2\n");
}

/// B-2026-08-01-31 — a deep-chain field MOVE-OUT followed by a reassign
/// (`let x = o.h.r; o.h.r = <new>`). The move zeroes the source field
/// and disarms the root's bodies walk (root-coarse, exactly the depth-1
/// B-2026-07-29-39 trade), so: x fires the moved value's body at ITS
/// death, the displaced-fire and the old-drop at the reassign stay
/// silent on the moved-out (cap-zeroed) bits, and the new value's body
/// goes silent at o's death — on BOTH backends identically, which is
/// what this twin pins. Pre-fix karac build double-freed z9 (x's drop +
/// o's StructDrop). Deliberate reuse-after-move (UAM-warned), so the
/// program is pinned in `OWNERSHIP_GATE_GRANDFATHERED`. Twin of
/// `tests/interpreter.rs`'s `test_deep_chain_field_move_then_reassign`.
#[test]
fn e2e_deep_chain_field_move_then_reassign() {
    let Some(out) = run_program(
        "struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"drop {self.id} {self.name}\")\n\
             \x20   }\n\
             }\n\
             struct H7 { r: Res }\n\
             struct O7 { h: H7 }\n\
             fn main() {\n\
             \x20   println(\"a\");\n\
             \x20   let mut o = O7 { h: H7 { r: Res { id: 9, name: f\"z{9}\" } } };\n\
             \x20   let x = o.h.r;\n\
             \x20   o.h.r = Res { id: 5, name: f\"y{5}\" };\n\
             \x20   println(f\"x {x.name} new {o.h.r.name}\");\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "a\nx z9 new y5\ndrop 9 z9\nend\n");
}

/// B-2026-09-06-55 — the MULTI-FIELD spelling of the pin above, and the
/// one that says the two backends agree on purpose rather than by
/// accident.
///
/// `H7` has ONE field, so masking the moved leaf empties the hop's walker
/// and this backend fell back to deleting the root's UserDrop action
/// outright — which is what made `emit_displaced_field_bodies`' armed-action
/// gate decline. Give the hop a SIBLING (`q`) and a top-level sibling (`k`)
/// and the masked walker survives, the action stays armed, and the gate
/// stopped discriminating: the displaced fire ran over the husk `x` owns,
/// printing `drop 9 ` with an EMPTY name where `--interp` printed nothing.
/// Measured on `main` before the fix on all three compiled surfaces.
/// `emit_displaced_field_bodies` now asks whether the assigned PLACE (or a
/// prefix of it) is masked, which is the question the interpreter's gate
/// has always asked.
///
/// Both siblings' bodies are the recovery the row is named for. The
/// re-filled slot's own body is still absent, exactly as in the one-field
/// pin — that is its documented behaviour, not this row's subject.
#[test]
fn e2e_deep_chain_field_move_then_reassign_with_siblings() {
    let Some(out) = run_program(
            "struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"drop {self.id} {self.name}\")\n\
             \x20   }\n\
             }\n\
             struct H8 { r: Res, q: Res }\n\
             struct O8 { h: H8, k: Res }\n\
             fn main() {\n\
             \x20   println(\"a\");\n\
             \x20   let mut o = O8 { h: H8 { r: Res { id: 9, name: f\"z{9}\" }, q: Res { id: 8, name: f\"z{8}\" } }, k: Res { id: 7, name: f\"z{7}\" } };\n\
             \x20   let x = o.h.r;\n\
             \x20   o.h.r = Res { id: 5, name: f\"y{5}\" };\n\
             \x20   println(f\"x {x.name} new {o.h.r.name}\");\n\
             \x20   println(\"end\");\n\
             }\n",
        ) else {
            return;
        };
    assert_eq!(
        out,
        "a\nx z9 new y5\ndrop 9 z9\ndrop 7 z7\ndrop 8 z8\nend\n"
    );
}

/// B-2026-09-07-63 — a DEPTH-1 field move-out followed by a reassign of
/// that same field (`let taken = g.one; g.one = mks(7);`), which was BOTH
/// a body too many and a body too few on every compiled surface:
/// `dS1 dS2 t1 dS1` against `--interp`'s `dS2 dS7 t1 dS1`.
///
/// The `dS1` too many is the displacement firing over the husk `taken`
/// already owns — `emit_displaced_field_bodies` rested on the `full_action`
/// gate at depth 1, and since B-2026-09-06-46 a move-out REPLACES that
/// action with a masked walker rather than deleting it, so the gate stayed
/// armed and stopped discriminating. It kept answering only for a ONE-FIELD
/// base, whose masked walker comes out empty and is deleted outright; the
/// sibling `two` here is what makes the walker survive. Exactly the
/// accident B-2026-09-06-55 removed one level down, removed one level up.
///
/// The `dS7` too few is the mask never being lifted: the field is masked
/// for the value `taken` owns and stayed masked for the REPLACEMENT stored
/// on the next line, so the new value's body ran nowhere. `dS2` survives
/// throughout, which is what says the walk is masked per FIELD.
///
/// Cell (b) is the guard on the restriction, not a second symptom. The mask
/// is compile-time state while the store may be runtime-conditional, so an
/// unconditional re-arm would run a body over the husk on the path the
/// assignment never took; the re-arm asks that the base's walk live in the
/// INNERMOST frame, which an `if` body is not. Both backends agree here
/// before and after the fix, and this cell fails if the re-arm is ever
/// loosened to fire inside a branch.
///
/// Cell (c) is the per-field guard: assigning the UN-moved sibling must
/// give `two` its new body (`dS8`) without resurrecting the masked `one`.
///
/// Bodies only — `valgrind --leak-check=full` on cell (a) reports
/// `All heap blocks were freed` and `0 errors` at KARAC_OPT_LEVEL 0 and 2,
/// before and after. Twin of `tests/interpreter.rs`'s
/// `test_depth1_field_move_then_reassign_rearms_new_value`.
#[test]
fn e2e_depth1_field_move_then_reassign_rearms_new_value() {
    // (a) the row's cell: the reassign displaces nothing and the new
    // value is the base's to drop.
    let Some(out) = run_program(
        "struct Rs { id: i64, name: String }\n\
             impl Drop for Rs {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"dS{self.id}\")\n\
             \x20   }\n\
             }\n\
             fn mks(i: i64) -> Rs {\n\
             \x20   return Rs { id: i, name: f\"h{i}\" };\n\
             }\n\
             struct Bs { mut one: Rs, mut two: Rs }\n\
             fn main() {\n\
             \x20   let mut g = Bs { one: mks(1), two: mks(2) };\n\
             \x20   let taken = g.one;\n\
             \x20   g.one = mks(7);\n\
             \x20   println(f\"t{taken.id}\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "dS2\ndS7\nt1\ndS1\n");
    // (b) CONDITIONAL store — the re-arm must decline, or the untaken
    // path runs a body over the moved-out husk.
    let Some(out) = run_program(
        "struct Rs { id: i64, name: String }\n\
             impl Drop for Rs {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"dS{self.id}\")\n\
             \x20   }\n\
             }\n\
             fn mks(i: i64) -> Rs {\n\
             \x20   return Rs { id: i, name: f\"h{i}\" };\n\
             }\n\
             struct Bs { mut one: Rs, mut two: Rs }\n\
             fn main() {\n\
             \x20   let f = false;\n\
             \x20   let mut g = Bs { one: mks(1), two: mks(2) };\n\
             \x20   let taken = g.one;\n\
             \x20   if f { g.one = mks(7); }\n\
             \x20   println(f\"t{taken.id}\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "dS2\nt1\ndS1\n");
    // (c) the UN-moved sibling: `two` re-fills normally while `one`
    // stays masked.
    let Some(out) = run_program(
        "struct Rs { id: i64, name: String }\n\
             impl Drop for Rs {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"dS{self.id}\")\n\
             \x20   }\n\
             }\n\
             fn mks(i: i64) -> Rs {\n\
             \x20   return Rs { id: i, name: f\"h{i}\" };\n\
             }\n\
             struct Bs { mut one: Rs, mut two: Rs }\n\
             fn main() {\n\
             \x20   let mut g = Bs { one: mks(1), two: mks(2) };\n\
             \x20   let taken = g.one;\n\
             \x20   println(\"m2\");\n\
             \x20   g.two = mks(8);\n\
             \x20   println(\"m3\");\n\
             \x20   println(f\"t{taken.id}\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "m2\ndS2\ndS8\nm3\nt1\ndS1\n");
}

/// B-2026-09-08-4 — a field MOVE-OUT compiled inside an `if` BODY left the
/// base's field-bodies walk registered in the BRANCH's frame, so it drained
/// when that frame popped rather than at the base's own death.
///
/// `disarm_struct_field_bodies_at` retracted the walk and re-registered it
/// masked, a pair that is correct only when it runs in the same frame the
/// action lives in — `replace_user_drop_fn_for_var`'s stated rule
/// (B-2026-08-29-33), and false for any move-out inside an `if`, a `match`
/// arm or a loop body. It swaps in place now. The defect hid outside a
/// nested construct because there the innermost frame IS the owning one,
/// which is why every straight-line move-out pin passed either way; a frame
/// dump showed the action in frame0 before the disarm and frame1 after.
///
/// TWO DEFECTS, fixed in that order and both pinned here. The FRAME one
/// cost the TAKEN path its un-moved sibling (`t1 dS1 m3`, no `dS2`). The
/// second is that the mask is COMPILE-TIME state applied on every path
/// while the move it records may not run, so the UNTAKEN path skipped
/// `one`'s body although nothing had moved it — `dS2 m3` against
/// `--interp`'s `dS2 dS1 m3`. A conditional SIMPLE field move-out now takes
/// a runtime per-field flag that the death-site tree selects on.
///
/// Cell (a) fails if the walk goes back to draining in the branch's frame;
/// cell (b) fails if the runtime route regresses to a static mask. Both
/// read byte-identically to `--interp` on all five surfaces.
///
/// The runtime route is deliberately NOT taken for a partial DESTRUCTURE:
/// `asan_match_arm_struct_payload_binding_field_bodies_clean` loses the
/// discarded field's body without the masked walker on the source's action,
/// and kept losing it when the move map was written too — so that path
/// needs its transfer worked out first, and B-2026-09-08-5 (the conditional
/// ASSIGN, a different site) is untouched by this.
///
/// Twin of `tests/interpreter.rs`'s
/// `test_cond_field_move_walk_stays_in_the_owning_frame`.
#[test]
fn e2e_cond_field_move_walk_stays_in_the_owning_frame() {
    // (a) TAKEN path — correct on every surface after the fix.
    let Some(out) = run_program(
        "struct Rs { id: i64, name: String }\n\
             impl Drop for Rs {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"dS{self.id}\")\n\
             \x20   }\n\
             }\n\
             fn mks(i: i64) -> Rs {\n\
             \x20   return Rs { id: i, name: f\"h{i}\" };\n\
             }\n\
             struct Bs { mut one: Rs, mut two: Rs }\n\
             fn main() {\n\
             \x20   let f = true;\n\
             \x20   let mut g = Bs { one: mks(1), two: mks(2) };\n\
             \x20   if f { let taken = g.one; println(f\"t{taken.id}\"); }\n\
             \x20   println(\"m3\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "t1\ndS1\ndS2\nm3\n");
    // (b) UNTAKEN path — nothing moved, so BOTH fields' bodies are due.
    // This is the cell the runtime flag buys: under the static mask it
    // read `dS2 m3`.
    let Some(out) = run_program(
        "struct Rs { id: i64, name: String }\n\
             impl Drop for Rs {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"dS{self.id}\")\n\
             \x20   }\n\
             }\n\
             fn mks(i: i64) -> Rs {\n\
             \x20   return Rs { id: i, name: f\"h{i}\" };\n\
             }\n\
             struct Bs { mut one: Rs, mut two: Rs }\n\
             fn main() {\n\
             \x20   let f = false;\n\
             \x20   let mut g = Bs { one: mks(1), two: mks(2) };\n\
             \x20   if f { let taken = g.one; println(f\"t{taken.id}\"); }\n\
             \x20   println(\"m3\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "dS2\ndS1\nm3\n");
}

/// B-2026-09-08-14 — a CONDITIONAL move-out followed by an UNCONDITIONAL
/// reassign lost the DISPLACED value's body on the path the move never
/// took: `dS2 dS7 m3` against `--interp`'s `dS1 dS2 dS7 m3`.
///
/// `emit_displaced_field_bodies` decides in two stages — a STATIC decline
/// when the assigned field is in `struct_moved_field_bodies`, then a
/// RUNTIME guard on the field's `field_view_flags` bit. The static decline
/// returned first, so the guard never got to speak. Since B-2026-09-08-4 a
/// conditional move-out records the move in the map (its other readers need
/// that answer) AND mints the flag, so the map alone no longer says whether
/// the move actually RAN on the path being compiled; the decline now defers
/// to the guard whenever the flag exists.
///
/// Cell (b) is the one that keeps the deferral honest in the other
/// direction: on the TAKEN path `taken` owns the old value and the
/// displaced body must NOT fire, which is what the flag being false buys.
/// A deferral that simply stopped declining would print `dS1` twice here.
///
/// Twin of `tests/interpreter.rs`'s
/// `test_cond_move_then_reassign_displaces_on_the_untaken_path`.
#[test]
fn e2e_cond_move_then_reassign_displaces_on_the_untaken_path() {
    // (a) UNTAKEN — nothing moved `one`, so the reassign displaces it.
    let Some(out) = run_program(
        "struct Rs { id: i64, name: String }\n\
             impl Drop for Rs {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"dS{self.id}\")\n\
             \x20   }\n\
             }\n\
             fn mks(i: i64) -> Rs {\n\
             \x20   return Rs { id: i, name: f\"h{i}\" };\n\
             }\n\
             struct Bs { mut one: Rs, mut two: Rs }\n\
             fn main() {\n\
             \x20   let f = false;\n\
             \x20   let mut g = Bs { one: mks(1), two: mks(2) };\n\
             \x20   if f { let taken = g.one; println(f\"t{taken.id}\"); }\n\
             \x20   g.one = mks(7);\n\
             \x20   println(\"m3\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "dS1\ndS2\ndS7\nm3\n");
    // (b) TAKEN — `taken` owns the old value, so NO displaced body.
    let Some(out) = run_program(
        "struct Rs { id: i64, name: String }\n\
             impl Drop for Rs {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"dS{self.id}\")\n\
             \x20   }\n\
             }\n\
             fn mks(i: i64) -> Rs {\n\
             \x20   return Rs { id: i, name: f\"h{i}\" };\n\
             }\n\
             struct Bs { mut one: Rs, mut two: Rs }\n\
             fn main() {\n\
             \x20   let f = true;\n\
             \x20   let mut g = Bs { one: mks(1), two: mks(2) };\n\
             \x20   if f { let taken = g.one; println(f\"t{taken.id}\"); }\n\
             \x20   g.one = mks(7);\n\
             \x20   println(\"m3\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "t1\ndS1\ndS2\ndS7\nm3\n");
}

/// B-2026-08-29-30 — the two halves of the row fd4e80f filed against itself,
/// left, found by filling in its matrix in both directions.
///
///   * the LEAK half: a bare LITERAL statement reached NO gate. That commit chained
///     `discarded_match_value_tail` at both discard sites, but the
///     bare-statement arm still chained no `discarded_owned_literal_tail`
///     leg — the mirror of what B-2026-08-29-20 fixed one site over, and
///     refuted by the same argument its comment makes. The only cell of
///     this family that is also a LEAK: `H { s: payload() };` stranded
///     36 B where `let _ = H { s: payload() };` was clean
///     (`asan_bare_literal_statement_discard_owns_its_heap`).
///
///   * the no-`else` half: a no-`else` `if` was a live run-vs-build DIVERGENCE — the
///     interpreter's statement-site arm gated on liveness alone, which such
///     an `if` passes, so it fired a body both compiled backends cannot.
///     `compile_if`'s merge yields a placeholder with no `else`, so the
///     value never reaches this site. Row 3 pinned the agreed silence that
///     restored; it now pins the BODY, because B-2026-08-29-30's remaining
///     half landed the arm-level owner that lets all three fire together.
#[test]
fn e2e_discarded_literal_statement_and_no_else_if() {
    let hdr = "struct R { id: i64 }\n\
                   impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n";
    let rows: [(&str, &str, &str); 3] = [
        (
            "R { id: 7 };\nprintln(\"end\");",
            "dR7\nend\n",
            "FIXED: bare struct-literal statement",
        ),
        (
            "{ R { id: 7 } };\nprintln(\"end\");",
            "dR7\nend\n",
            "FIXED: block-wrapped struct literal, bare statement",
        ),
        (
            "let n = 1;\nif n == 1 { R { id: 7 } };\nprintln(\"end\");",
            "dR7\nend\n",
            "FIXED: a no-`else` `if`, owned from inside the arm",
        ),
    ];
    for (body, expected, label) in rows {
        let src = format!("{hdr}fn main() {{\n{body}\n}}\n");
        assert_eq!(run_program(&src).as_deref(), Some(expected), "[{label}]");
    }
    // A place-moving tuple is the hazard the literal leg brings with it:
    // the source's own cleanup is retracted so this walk is the single
    // owner, which is a DOUBLED body if the retraction is missed. Both
    // spellings must sit at exactly one.
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
        assert_eq!(
            run_program(&src).as_deref(),
            Some("dR1\nend\n"),
            "[{label}]"
        );
    }
}

/// B-2026-09-14-16 — projecting one field off a FRESH TEMP runs the temp's
/// OTHER `Drop`-bearing fields' bodies, at the projection.
///
/// `let w = (mkw(7).r, 1);` over `struct W { r: D, s: D, b: i64 }` printed
/// `idx1 dD7 end` on all four surfaces: the moved leaf's body ran at the
/// consumer and `s`'s — which nothing moved and nothing else owns — ran
/// NOWHERE. Agreed, so no A/B gate saw it. The control is a bare discarded
/// `mkw(7);`, which reaches the discard route, takes the value whole and
/// runs both.
///
/// THE BODY RUNS AT THE PROJECTION, not at the consumer's drop, and the
/// NAMED-source spelling is why: `let t = mkw(7); let w = (t.r, 1);` is
/// correct today and prints `dD107` at `t`'s own last use — which is the
/// projection — because design.md § 866 fires a destructor at the
/// live-range end. A fresh temp's last use is the same projection, so the
/// due sequence is the named spelling's.
///
/// BOTH BACKENDS MOVE TOGETHER, which they had to: the loss was agreed, so
/// fixing one alone would have manufactured a run-vs-build divergence out
/// of it. Codegen registers a masked field-bodies walk beside the cap zero
/// `consume_freshtemp_field_move` already emitted; the interpreter stages
/// the temp's VALUE at the read (it cannot re-evaluate the producer) and
/// consumes it at the same statement.
///
/// THE PROJECTED FIELD IS EXCLUDED BY REMOVING IT FROM THE VALUE the
/// interpreter's walk sees, not by `pending_payload_masked_fields`: that
/// channel masks a field's PAYLOAD bodies rather than the field itself, and
/// using it left the projected field's own body running here beside the
/// consumer's — measured `dD107 dD7 idx1 dD7` against the due
/// `dD107 idx1 dD7`.
///
/// CELLS 6-8 ARE THE READ-THROUGH POSITIONS, which this fix left alone and
/// B-2026-09-17-36 later took up. A scalar read through the projection
/// (`println(f"v{mkw(7).r.id}")`) and a scalar FIELD read (`mkw(7).b`) now run
/// both bodies at the end of the statement, as the named spelling does. The
/// projection passed straight to a discarding callee (`eat(mkw(7).r)`) is
/// still PINNED AS MEASURED, running no body at all on both surfaces: the
/// callee takes a field that has a body of its own, and neither backend yet
/// knows whether it moved.
///
/// MEMORY IS CLEAN AND THE STRINGS ARE INTACT with a heap-carrying `D`:
/// `-O0` valgrind reports 12-13 allocs with equal frees, `0 bytes in 0
/// blocks` at exit, `0 errors` and no invalid access on every cell, and
/// each body prints its own name.
#[test]
fn e2e_projecting_a_field_off_a_fresh_temp_runs_the_siblings_bodies() {
    const H: &str = "struct D { id: i64 }\n\
             impl Drop for D { fn drop(mut ref self) { println(f\"dD{self.id}\") } }\n\
             fn mkd(n: i64) -> D { return D { id: n }; }\n\
             struct W { r: D, s: D, b: i64 }\n\
             fn mkw(n: i64) -> W { return W { r: mkd(n), s: mkd(n + 100), b: n }; }\n";
    for (label, prog, want) in [
            (
                // 1 — the row's headline cell: a tuple literal built from the
                //     projection.
                "tuple literal of a projected field",
                format!(
                    "{H}fn main() {{ let w = (mkw(7).r, 1i64); println(f\"idx{{w.1}}\"); println(\"end\") }}\n"
                ),
                "dD107\nidx1\ndD7\nend\n",
            ),
            (
                // 2 — CONTROL: a bare discarded temp, which was always correct
                //     and is the proof the sibling's body is owed at all.
                "control: bare discarded temp runs both",
                format!("{H}fn main() {{ mkw(7); println(\"mid\"); println(\"end\") }}\n"),
                "dD107\ndD7\nmid\nend\n",
            ),
            (
                // 3 — the STRUCT-literal spelling, a different consuming site
                //     with the identical loss.
                "struct literal of a projected field",
                format!(
                    "{H}struct V {{ r: D, b: i64 }}\n\
                     fn main() {{ let w = V {{ r: mkw(7).r, b: 1i64 }}; println(f\"idx{{w.b}}\"); println(\"end\") }}\n"
                ),
                "dD107\nidx1\ndD7\nend\n",
            ),
            (
                // 4 — CONTROL: the NAMED source, correct before this change and
                //     the oracle the due sequence comes from.
                "control: named source",
                format!(
                    "{H}fn main() {{ let t = mkw(7); let w = (t.r, 1i64); println(f\"idx{{w.1}}\"); println(\"end\") }}\n"
                ),
                "dD107\nidx1\ndD7\nend\n",
            ),
            (
                // 5 — the plainest spelling: the projection bound directly.
                "bare let of a projected field",
                format!(
                    "{H}fn main() {{ let x = mkw(7).r; println(f\"idx{{x.id}}\"); println(\"end\") }}\n"
                ),
                "dD107\nidx7\ndD7\nend\n",
            ),
            (
                // 6-7 — B-2026-09-17-36: both bodies at the statement's end.
                "scalar read through the projection",
                format!("{H}fn main() {{ println(f\"v{{mkw(7).r.id}}\"); println(\"end\") }}\n"),
                "v7\ndD107\ndD7\nend\n",
            ),
            (
                "scalar field read off the temp",
                format!("{H}fn main() {{ println(f\"v{{mkw(7).b}}\"); println(\"end\") }}\n"),
                "v7\ndD107\ndD7\nend\n",
            ),
            (
                // 8 — PINNED AS MEASURED, not fixed. See the note above.
                "pinned: projection into a discarding callee",
                format!(
                    "{H}fn eat(d: D) -> i64 {{ return d.id; }}\n\
                     fn main() {{ println(f\"v{{eat(mkw(7).r)}}\"); println(\"end\") }}\n"
                ),
                "v7\nend\n",
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

/// B-2026-09-17-36 / B-2026-09-25-26 — a FRESH TEMP read through a projection
/// runs its `Drop` bodies at the end of the statement that read it.
///
/// `println(f"v{mkw(7).b}")` built a `W { r: D, s: D, b: i64 }` and ran neither
/// `D` body, and `println(o.unwrap().s)` never ran `R`'s own, on all four
/// surfaces alike; the named spelling (`let t = mkw(7); println(f"v{t.b}")`)
/// runs them at the end of the statement that last uses `t`. A temp's last use
/// is its projection, so both backends now keep a per-statement list of temps
/// read that way (`freshtemp_read_levels`) and run the bodies when the
/// statement ends; codegen guards each behind a runtime flag, so an untaken
/// branch runs nothing and an early exit runs it from the frame instead.
///
/// Several cells were run-vs-build DIVERGENCES before, not agreed losses:
/// codegen already ran the bodies for an assignment, a `return` and a block
/// tail, and the interpreter did for a taken `if` arm inside a `let`. Each now
/// prints one sequence on every surface.
///
/// The PINNED cells are the positions both backends still decline, together:
/// a projected field that has a body of its own handed to a callee (which may
/// have moved it), a generic struct (a method-call temp has no instantiation
/// on the compiled side), and a temp read in a loop condition, a `match`
/// scrutinee or a closure body, where the enclosing statement runs the read
/// more than once or not at all.
#[test]
fn e2e_fresh_temp_read_through_a_projection_runs_its_bodies() {
    const H: &str = "struct D { id: i64, name: String }\n\
             impl Drop for D { fn drop(mut ref self) { println(f\"dD{self.id}{self.name}\") } }\n\
             fn mkd(n: i64) -> D { return D { id: n, name: f\"n{n}\" }; }\n\
             struct W { r: D, s: D, b: i64 }\n\
             fn mkw(n: i64) -> W { return W { r: mkd(n), s: mkd(n + 100), b: n }; }\n\
             fn eat(d: D) -> i64 { return d.id; }\n\
             struct R { s: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.s}\") } }\n\
             fn mkr(t: String) -> R { return R { s: t }; }\n\
             struct Ho { o: Option[R], p: R }\n\
             fn take(n: i64) -> i64 { return n + 1; }\n\
             fn g(fail: bool) -> Result[i64, String] { if fail { return Err(f\"e\"); } return Ok(1); }\n\
             fn ex(fail: bool) -> Result[i64, String] { let x = mkw(7).b + g(fail)?; println(f\"x{x}\"); return Ok(x); }\n\
             fn ret() -> i64 { return mkw(8).b; }\n\
             struct G[T] { v: T, k: i64 }\n\
             fn mkg(d: D) -> G[D] { return G { v: d, k: 5 }; }\n\
             struct Q { d: D, k: i64 }\n\
             impl Drop for Q { fn drop(mut ref self) { println(f\"dQ{self.k}\") } }\n\
             fn mkq() -> Q { return Q { d: mkd(3), k: 4 }; }\n\
             struct Ow { o: Option[D], k: i64 }\n\
             fn mkow(b: bool) -> Ow { if b { return Ow { o: Some(mkd(1)), k: 2 }; } return Ow { o: None, k: 3 }; }\n";
    for (label, body, want) in [
        (
            "scalar read through a projection",
            "println(f\"v{mkw(7).r.id}\");",
            "v7\ndD107n107\ndD7n7\nend\n",
        ),
        (
            "scalar field of the temp",
            "println(f\"v{mkw(7).b}\");",
            "v7\ndD107n107\ndD7n7\nend\n",
        ),
        (
            "B-2026-09-25-26: field of an unwrap result",
            "let o = Some(mkr(f\"rrr\")); println(o.unwrap().s);",
            "rrr\ndrop rrr\nend\n",
        ),
        (
            "field of a call result with its own Drop",
            "println(mkr(f\"rrr\").s);",
            "rrr\ndrop rrr\nend\n",
        ),
        (
            "field of an unwrapped struct field",
            "let h = Ho { o: Some(mkr(f\"rrr\")), p: mkr(f\"ppp\") }; println(h.o.unwrap().s);",
            "rrr\ndrop rrr\ndrop ppp\nend\n",
        ),
        (
            "taken branch of a let",
            "let c = true; let x = if c { mkw(7).b } else { 0 }; println(f\"x{x}\");",
            "dD107n107\ndD7n7\nx7\nend\n",
        ),
        (
            "branch not taken",
            "let c = false; let x = if c { mkw(7).b } else { 0 }; println(f\"x{x}\");",
            "x0\nend\n",
        ),
        (
            "once per loop iteration",
            "for i in 0..3 { println(f\"i{mkw(i).b}\"); }",
            "i0\ndD100n100\ndD0n0\ni1\ndD101n101\ndD1n1\ni2\ndD102n102\ndD2n2\nend\n",
        ),
        (
            "beside a ? that does not exit",
            "match ex(false) { Ok(v) => println(f\"ok{v}\"), Err(e) => println(f\"err{e}\") }",
            "dD107n107\ndD7n7\nx8\nok8\nend\n",
        ),
        (
            "return statement",
            "println(f\"r{ret()}\");",
            "dD108n108\ndD8n8\nr8\nend\n",
        ),
        (
            "method on the projected field",
            "println(f\"l{mkr(f\"abc\").s.len()}\");",
            "l3\ndrop abc\nend\n",
        ),
        (
            "projection as a call argument",
            "println(f\"t{take(mkw(7).b)}\");",
            "t8\ndD107n107\ndD7n7\nend\n",
        ),
        (
            "two temps, last read first",
            "println(f\"{mkw(1).b} {mkw(2).b}\");",
            "1 2\ndD102n102\ndD2n2\ndD101n101\ndD1n1\nend\n",
        ),
        (
            "assignment",
            "let mut y = 0; y = mkw(7).b; println(f\"y{y}\");",
            "dD107n107\ndD7n7\ny7\nend\n",
        ),
        (
            "compound assignment",
            "let mut y = 0; y += mkw(7).b; println(f\"y{y}\");",
            "dD107n107\ndD7n7\ny7\nend\n",
        ),
        (
            "block tail inside a let",
            "let x = { let y = 2; mkw(y).b }; println(f\"x{x}\");",
            "dD102n102\ndD2n2\nx2\nend\n",
        ),
        (
            "own-Drop parent, scalar field",
            "println(f\"q{mkq().k}\");",
            "q4\ndQ4\ndD3n3\nend\n",
        ),
        (
            "own-Drop parent, read through its Drop field",
            "println(f\"q{mkq().d.id}\");",
            "q3\ndQ4\ndD3n3\nend\n",
        ),
        (
            "if condition inside a let",
            "let x = if mkw(3).b > 1 { 1 } else { 2 }; println(f\"x{x}\");",
            "dD103n103\ndD3n3\nx1\nend\n",
        ),
        (
            "String read through a Drop field",
            "println(mkw(7).r.name);",
            "n7\ndD107n107\ndD7n7\nend\n",
        ),
        (
            "Option field, Some then None",
            "println(f\"o{mkow(true).k}\"); println(f\"o{mkow(false).k}\");",
            "o2\ndD1n1\no3\nend\n",
        ),
        (
            "pinned: Drop field handed to a callee",
            "println(f\"v{eat(mkw(7).r)}\");",
            "v7\nend\n",
        ),
        (
            "pinned: generic struct",
            "println(f\"g{mkg(mkd(3)).k}\");",
            "g5\nend\n",
        ),
        (
            "pinned: match scrutinee",
            "match mkw(3).b { 3 => println(\"three\"), _ => println(\"other\") }",
            "three\nend\n",
        ),
        (
            "pinned: while condition",
            "let mut i = 0; while mkw(i).b < 2 { i = i + 1; } println(f\"i{i}\");",
            "i2\nend\n",
        ),
        (
            "pinned: closure body",
            "let f = |n: i64| mkw(n).b; println(f\"f{f(3)}\");",
            "f3\nend\n",
        ),
    ] {
        let prog = format!("{H}fn main() {{\n    {body}\n    println(\"end\")\n}}\n");
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

/// B-2026-09-25-42 — a FUNCTION's tail hands its value to the caller, so a
/// projection off a fresh temp there (`fn tail() -> i64 { mkw(9).b }`) ends
/// the temp and its remaining fields' `Drop` bodies are owed at the return.
///
/// Codegen always did this (`suppress_cleanup_for_tail_return` consumes the
/// body's tail, or its last statement when there is no tail) and printed
/// `dD109 dD9 t9`; the interpreter printed `t9`, a run-vs-build divergence in
/// every cell below but the pinned ones. It now consumes the same expression
/// at the same point, including a block-bodied closure's tail. Codegen's walk
/// additionally resolves a GENERIC temp's fields from its instantiation, which
/// the interpreter already did (`let x = mkg(mkd(3)).k;` diverged the other
/// way).
///
/// The PINNED cells are tails both backends still leave alone, together: a
/// tail of an inner block and a projection read through (`mkw(9).r.id`). A
/// tail nested in an `if` arm and one inside arithmetic were pinned here too
/// until B-2026-09-26-6 gave a body's tail its own read level.
#[test]
fn e2e_fresh_temp_projected_in_a_function_tail_runs_its_bodies() {
    const H: &str = "struct D { id: i64, name: String }\n\
             impl Drop for D { fn drop(mut ref self) { println(f\"dD{self.id}{self.name}\") } }\n\
             fn mkd(n: i64) -> D { return D { id: n, name: f\"n{n}\" }; }\n\
             struct W { r: D, s: D, b: i64 }\n\
             fn mkw(n: i64) -> W { return W { r: mkd(n), s: mkd(n + 100), b: n }; }\n\
             struct G[T] { v: T, k: i64 }\n\
             fn mkg(d: D) -> G[D] { return G { v: d, k: 5 }; }\n\
             fn tail() -> i64 { mkw(9).b }\n\
             fn tailg() -> i64 { mkg(mkd(3)).k }\n\
             fn lastst() { let a = 1; mkw(9).b; }\n\
             fn taild() -> D { mkw(9).r }\n\
             impl W { fn m(ref self) -> i64 { mkw(9).b } }\n\
             fn tailif(c: bool) -> i64 { if c { mkw(9).b } else { 0 } }\n\
             fn tailr() -> i64 { mkw(9).r.id }\n\
             fn tailadd() -> i64 { mkw(9).b + 1 }\n\
             fn tailnest() -> i64 { let a = 1; { mkw(9).b } }\n";
    for (label, body, want) in [
        (
            "function tail",
            "println(f\"t{tail()}\");",
            "dD109n109\ndD9n9\nt9\nend\n",
        ),
        (
            "method tail",
            "let w = mkw(1); println(f\"t{w.m()}\");",
            "dD109n109\ndD9n9\nt9\ndD101n101\ndD1n1\nend\n",
        ),
        (
            "Drop-bearing field moved out of the tail",
            "let d = taild(); println(f\"t{d.id}\");",
            "dD109n109\nt9\ndD9n9\nend\n",
        ),
        (
            "last statement of a unit function",
            "lastst(); println(\"x\");",
            "dD109n109\ndD9n9\nx\nend\n",
        ),
        (
            "block-bodied closure tail",
            "let f = |n: i64| { mkw(n).b }; println(f\"f{f(9)}\");",
            "dD109n109\ndD9n9\nf9\nend\n",
        ),
        (
            "block-bodied closure, last statement",
            "let f = |n: i64| { mkw(n).b; }; f(9); println(\"x\");",
            "dD109n109\ndD9n9\nx\nend\n",
        ),
        (
            "generic struct in a function tail",
            "println(f\"t{tailg()}\");",
            "dD3n3\nt5\nend\n",
        ),
        (
            "generic struct taken by a let",
            "let x = mkg(mkd(3)).k; println(f\"x{x}\");",
            "dD3n3\nx5\nend\n",
        ),
        (
            "tail nested in an if arm (B-2026-09-26-6)",
            "println(f\"t{tailif(true)}\");",
            "dD109n109\ndD9n9\nt9\nend\n",
        ),
        (
            "pinned: tail read through a projection",
            "println(f\"t{tailr()}\");",
            "t9\nend\n",
        ),
        (
            "tail inside arithmetic (B-2026-09-26-6)",
            "println(f\"t{tailadd()}\");",
            "dD109n109\ndD9n9\nt10\nend\n",
        ),
        (
            "pinned: tail of an inner block",
            "println(f\"t{tailnest()}\");",
            "t9\nend\n",
        ),
    ] {
        let prog = format!("{H}fn main() {{\n    {body}\n    println(\"end\")\n}}\n");
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

/// B-2026-09-26-1 — a heap field moved out THROUGH a projection of a fresh
/// temp (`let s = mkw2().p.name;`) has one owner.
///
/// The consumer matched only a ONE-hop projection of the staged temp, so for
/// `mkw2().p.name` nothing cap-zeroed `p.name` in the temp's slot: the temp's
/// memory drop freed it and so did `s`, `free(): double free detected in tcache
/// 2` on every compiled surface for a program with no `Drop` anywhere. Both
/// backends now follow the chain to the staged root, zero the leaf there, and
/// run the temp's remaining bodies once (`--interp` also ran a moved-out
/// `Drop`-bearing leaf's body twice before, `n06`).
///
/// Not covered and not pinned here because they still abort: a GENERIC root
/// (`mkg2().v.name` over `G[P]`), and a chain through a struct with a `Drop` of
/// its own (`mkw(9).r.name`), whose legality on a temp is the typechecker's
/// question.
#[test]
fn e2e_field_moved_through_a_fresh_temp_projection_has_one_owner() {
    const H: &str = "struct D { id: i64, name: String }\n\
             impl Drop for D { fn drop(mut ref self) { println(f\"dD{self.id}{self.name}\") } }\n\
             fn mkd(n: i64) -> D { return D { id: n, name: f\"n{n}\" }; }\n\
             struct P { name: String }\n\
             struct W2 { p: P, b: i64 }\n\
             fn mkw2() -> W2 { return W2 { p: P { name: f\"pp\" }, b: 1 }; }\n\
             struct P3 { name: String, d: D }\n\
             struct W3 { p: P3, q: D, b: i64 }\n\
             fn mkw3() -> W3 { return W3 { p: P3 { name: f\"p3\", d: mkd(1) }, q: mkd(2), b: 3 }; }\n\
             struct A4 { w: W3, k: i64 }\n\
             fn mk4() -> A4 { return A4 { w: mkw3(), k: 4 }; }\n\
             fn f5() -> String { mkw2().p.name }\n\
             fn f6() -> String { mkw3().p.name }\n\
             fn f7() -> D { mkw3().p.d }\n";
    for (label, body, want) in [
        (
            "n01 let, no Drop anywhere",
            "let s = mkw2().p.name; println(s);",
            "pp\nend\n",
        ),
        (
            "n02 function tail, no Drop anywhere",
            "println(f\"t{f5()}\");",
            "tpp\nend\n",
        ),
        (
            "n03 let, Drop siblings at both levels",
            "let s = mkw3().p.name; println(s);",
            "dD2n2\ndD1n1\np3\nend\n",
        ),
        (
            "n04 function tail, Drop siblings",
            "println(f\"t{f6()}\");",
            "dD2n2\ndD1n1\ntp3\nend\n",
        ),
        (
            "n05 three hops",
            "let s = mk4().w.p.name; println(s);",
            "dD2n2\ndD1n1\np3\nend\n",
        ),
        (
            "n06 Drop-bearing leaf moved out",
            "let d = mkw3().p.d; println(f\"d{d.id}\");",
            "dD2n2\nd1\ndD1n1\nend\n",
        ),
        (
            "n07 Drop-bearing leaf out of a function tail",
            "let d = f7(); println(f\"d{d.id}\");",
            "dD2n2\nd1\ndD1n1\nend\n",
        ),
        (
            "n09 tuple element",
            "let t = (mkw3().p.name, 1); println(t.0);",
            "dD2n2\ndD1n1\np3\nend\n",
        ),
        (
            "n10 read only, not a move",
            "println(mkw3().p.name);",
            "p3\ndD2n2\ndD1n1\nend\n",
        ),
        (
            "n11 assignment",
            "let mut s = f\"x\"; s = mkw3().p.name; println(s);",
            "dD2n2\ndD1n1\np3\nend\n",
        ),
    ] {
        let prog = format!("{H}fn main() {{\n    {body}\n    println(\"end\")\n}}\n");
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

/// B-2026-09-26-3 — a SCALAR taken off a fresh temp whose type has a `Drop` of
/// its own runs that body, as the named spelling does.
///
/// `let y = mkq().k;` over `struct Q { d: D, k: i64 }` with `impl Drop for Q`
/// printed `dD3n3 y4` on every surface, where `let q = mkq(); let x = q.k;`
/// prints `dQ4 dD3n3 x4`: the consumer's masked walk ran the remaining FIELDS'
/// bodies and never the type's own. A scalar leaves the value whole, so both
/// backends now run the whole value's bodies at the consumer, in every
/// consuming position (`let`, assignment, tuple element, constructor argument,
/// `return`, function tail).
///
/// The PINNED cell is a non-scalar field moved off such a temp
/// (`mkqs().s`), which runs no body on either backend; whether that move is
/// legal on a temp at all is the typechecker's question.
#[test]
fn e2e_scalar_taken_off_a_fresh_temp_runs_its_types_own_drop() {
    const H: &str = "struct D { id: i64, name: String }\n\
             impl Drop for D { fn drop(mut ref self) { println(f\"dD{self.id}{self.name}\") } }\n\
             fn mkd(n: i64) -> D { return D { id: n, name: f\"n{n}\" }; }\n\
             struct Q { d: D, k: i64 }\n\
             impl Drop for Q { fn drop(mut ref self) { println(f\"dQ{self.k}\") } }\n\
             fn mkq() -> Q { return Q { d: mkd(3), k: 4 }; }\n\
             struct Qs { s: String, k: i64 }\n\
             impl Drop for Qs { fn drop(mut ref self) { println(f\"dQs{self.k}{self.s}\") } }\n\
             fn mkqs() -> Qs { return Qs { s: f\"ss\", k: 6 }; }\n\
             fn tailq() -> i64 { mkq().k }\n\
             fn retq() -> i64 { return mkq().k; }\n";
    for (label, body, want) in [
        (
            "let",
            "let y = mkq().k; println(f\"y{y}\");",
            "dQ4\ndD3n3\ny4\nend\n",
        ),
        (
            "function tail",
            "println(f\"t{tailq()}\");",
            "dQ4\ndD3n3\nt4\nend\n",
        ),
        (
            "return",
            "println(f\"r{retq()}\");",
            "dQ4\ndD3n3\nr4\nend\n",
        ),
        (
            "tuple element",
            "let t = (mkq().k, 1); println(f\"t{t.0}\");",
            "dQ4\ndD3n3\nt4\nend\n",
        ),
        (
            "assignment",
            "let mut y = 0; y = mkq().k; println(f\"y{y}\");",
            "dQ4\ndD3n3\ny4\nend\n",
        ),
        (
            "own body reads a sibling heap field",
            "let y = mkqs().k; println(f\"y{y}\");",
            "dQs6ss\ny6\nend\n",
        ),
        (
            "constructor argument",
            "let o = Some(mkq().k); println(f\"o{o.unwrap()}\");",
            "dQ4\ndD3n3\no4\nend\n",
        ),
        (
            "control: the named spelling",
            "let q = mkq(); let x = q.k; println(f\"x{x}\");",
            "dQ4\ndD3n3\nx4\nend\n",
        ),
        (
            "pinned: a non-scalar moved off the temp",
            "let s = mkqs().s; println(s);",
            "ss\nend\n",
        ),
    ] {
        let prog = format!("{H}fn main() {{\n    {body}\n    println(\"end\")\n}}\n");
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

/// B-2026-09-26-2 — a projection off a fresh temp placed in a variant
/// constructor, tuple, struct literal or array literal is consumed AT THAT
/// SITE on both backends, so the temp's remaining `Drop` bodies run wherever
/// the aggregate sits. The interpreter consumed only through a `let`
/// initializer and a statement's end, so a function TAIL (`Ok(mkw(9).b)`)
/// ran none, a non-scalar constructor argument (`Some(mkw(9).r)`) ran none
/// in any position, and a multi-argument constructor or tuple consumed only
/// its last temp, after later arguments' side effects. Codegen has consumed
/// at the site since B-2026-08-31-34.
///
/// The last two cells are a tail that READS through the temp without
/// consuming it, which ran no body on any surface until B-2026-09-26-6.
#[test]
fn e2e_fresh_temp_projection_consumed_at_its_aggregate_site() {
    const H: &str = "struct D { id: i64, name: String }\n\
             impl Drop for D { fn drop(mut ref self) { println(f\"dD{self.id}\") } }\n\
             fn mkd(n: i64) -> D { return D { id: n, name: f\"n{n}\" }; }\n\
             struct W { r: D, s: D, b: i64 }\n\
             fn mkw(n: i64) -> W { return W { r: mkd(n), s: mkd(n + 100), b: n }; }\n\
             struct B { v: i64 }\n\
             enum E { A(i64, i64), B(D) }\n\
             fn say(n: i64) -> i64 { println(f\"say{n}\"); n }\n\
             fn f24(c: bool) -> Result[i64, String] { if c { return Err(f\"e\"); } Ok(mkw(9).b) }\n\
             fn g3(c: bool) -> Option[i64] { if c { Some(mkw(9).b) } else { None } }\n\
             fn g4() -> (i64, i64) { (mkw(9).b, 1) }\n\
             fn g9() -> B { B { v: mkw(9).b } }\n\
             fn g11() -> Option[D] { Some(mkw(9).r) }\n\
             fn h1() -> E { E.A(mkw(1).b, mkw(2).b) }\n\
             fn h3() -> Option[Option[i64]] { Some(Some(mkw(9).b)) }\n\
             fn h4() -> E { E.B(mkw(9).r) }\n\
             fn h5() -> Array[i64, 2] { [mkw(9).b, 1] }\n\
             fn h6() -> Vec[i64] { Vec[mkw(9).b, 1] }\n\
             fn id(x: i64) -> i64 { x }\n\
             fn p1() -> i64 { mkw(9).b + 0 }\n\
             fn p2() -> i64 { id(mkw(9).b) }\n";
    for (label, body, want) in [
        (
            "Ok in a function tail",
            "println(f\"t{f24(false).unwrap()}\");",
            "dD109\ndD9\nt9\nend\n",
        ),
        (
            "Some in an if-branch tail",
            "println(f\"t{g3(true).unwrap()}\");",
            "dD109\ndD9\nt9\nend\n",
        ),
        (
            "tuple tail",
            "println(f\"t{g4().0}\");",
            "dD109\ndD9\nt9\nend\n",
        ),
        (
            "struct literal tail",
            "println(f\"t{g9().v}\");",
            "dD109\ndD9\nt9\nend\n",
        ),
        (
            "non-scalar in Some, tail",
            "let o = g11(); println(f\"t{o.unwrap().id}\");",
            "dD109\nt9\ndD9\nend\n",
        ),
        (
            "non-scalar in Some, let",
            "let x = Some(mkw(9).r); println(f\"x{x.unwrap().id}\");",
            "dD109\nx9\ndD9\nend\n",
        ),
        (
            "user variant, two temps",
            "match h1() { E.A(a, b) => println(f\"a{a}{b}\"), E.B(_) => println(\"b\") }",
            "dD101\ndD1\ndD102\ndD2\na12\nend\n",
        ),
        (
            "user variant, before a later argument",
            "let e = E.A(mkw(1).b, say(5)); match e { E.A(a, _) => println(f\"a{a}\"), _ => println(\"x\") }",
            "dD101\ndD1\nsay5\na1\nend\n",
        ),
        (
            "nested Some",
            "println(f\"t{h3().unwrap().unwrap()}\");",
            "dD109\ndD9\nt9\nend\n",
        ),
        (
            "user variant, non-scalar",
            "match h4() { E.B(d) => println(f\"b{d.id}\"), _ => println(\"x\") }",
            "dD109\nb9\ndD9\nend\n",
        ),
        (
            "array literal tail",
            "println(f\"t{h5()[0]}\");",
            "dD109\ndD9\nt9\nend\n",
        ),
        (
            "Vec literal tail",
            "println(f\"t{h6()[0]}\");",
            "dD109\ndD9\nt9\nend\n",
        ),
        (
            "tuple, before a later element",
            "let t = (mkw(1).r, say(5)); println(f\"t{t.0.id}\");",
            "dD101\nsay5\nt1\ndD1\nend\n",
        ),
        (
            "array, before a later element",
            "let v = [mkw(9).b, say(5)]; println(f\"v{v[0]}\");",
            "dD109\ndD9\nsay5\nv9\nend\n",
        ),
        (
            "tuple, two temps in order",
            "let t = (mkw(1).b, mkw(2).b); println(f\"t{t.0}{t.1}\");",
            "dD101\ndD1\ndD102\ndD2\nt12\nend\n",
        ),
        (
            "a tail that reads through the temp (B-2026-09-26-6)",
            "println(f\"t{p1()}\");",
            "dD109\ndD9\nt9\nend\n",
        ),
        (
            "a tail that hands the read to a call (B-2026-09-26-6)",
            "println(f\"t{p2()}\");",
            "dD109\ndD9\nt9\nend\n",
        ),
    ] {
        let prog = format!("{H}fn main() {{\n    {body}\n    println(\"end\")\n}}\n");
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

/// B-2026-09-25-43 — a field read off a FRESH `shared struct` temporary runs
/// the temporary's `Drop` body at the read on both backends, where the
/// interpreter ran it nowhere: once the field is out nothing holds the
/// temporary, so on its last reference the body runs, as codegen's release
/// of the temporary does. The two controls are spellings that already agreed.
#[test]
fn e2e_field_read_off_a_fresh_shared_temp_runs_its_drop() {
    const H: &str = "struct D { id: i64 }\n\
             impl Drop for D { fn drop(mut ref self) { println(f\"dD{self.id}\") } }\n\
             shared struct Sh { k: i64, name: String, d: D }\n\
             impl Drop for Sh { fn drop(mut ref self) { println(f\"dSh{self.k}{self.name}\") } }\n\
             fn mksh(n: i64) -> Sh { return Sh { k: n, name: f\"nm{n}\", d: D { id: n } }; }\n\
             fn tl() -> i64 { mksh(8).k }\n\
             fn cl(n: i64) -> i64 { let f = |x: i64| mksh(x).k; f(n) }\n";
    for (label, body, want) in [
        (
            "read in a call argument",
            "println(f\"s{mksh(1).k}\");",
            "dSh1nm1\ndD1\ns1\nend\n",
        ),
        (
            "let initializer",
            "let k = mksh(2).k; println(f\"k{k}\");",
            "dSh2nm2\ndD2\nk2\nend\n",
        ),
        (
            "two temps in one expression",
            "let k = mksh(3).k + mksh(4).k; println(f\"k{k}\");",
            "dSh3nm3\ndD3\ndSh4nm4\ndD4\nk7\nend\n",
        ),
        (
            "heap field",
            "println(mksh(5).name);",
            "dSh5nm5\ndD5\nnm5\nend\n",
        ),
        (
            "read through a nested field",
            "println(f\"d{mksh(6).d.id}\");",
            "dSh6nm6\ndD6\nd6\nend\n",
        ),
        (
            "while condition",
            "let mut i = 0; while mksh(7).k > i { i = i + 7; } println(f\"i{i}\");",
            "dSh7nm7\ndD7\ndSh7nm7\ndD7\ni7\nend\n",
        ),
        (
            "function tail",
            "println(f\"t{tl()}\");",
            "dSh8nm8\ndD8\nt8\nend\n",
        ),
        (
            "closure body",
            "println(f\"c{cl(9)}\");",
            "dSh9nm9\ndD9\nc9\nend\n",
        ),
        (
            "control: a discarded temp",
            "mksh(10);",
            "dSh10nm10\ndD10\nend\n",
        ),
        (
            "control: the named spelling",
            "let s = mksh(11); println(f\"s{s.k}\");",
            "s11\ndSh11nm11\ndD11\nend\n",
        ),
    ] {
        let prog = format!("{H}fn main() {{\n    {body}\n    println(\"end\")\n}}\n");
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

/// B-2026-09-26-6 — a function body's TAIL expression is a statement end for
/// the fresh temps read inside it, on both backends: `fn p1() -> i64 {
/// mkw(1).b + 0 }` ran none of the temp's bodies on any surface, where
/// `let k = mkw(1).b + 0;` runs them at the `let`'s end. Both backends now open
/// one read level around a body's tail (the shapes
/// `ast::tail_ends_freshtemp_reads` admits, branch tails included) and close
/// it once the value exists, before the body's scope exit. The control is the
/// bare projection tail, which is consumed instead (B-2026-09-25-42).
#[test]
fn e2e_fresh_temp_read_in_a_function_tail_runs_its_bodies() {
    const H: &str = "struct D { id: i64, name: String }\n\
             impl Drop for D { fn drop(mut ref self) { println(f\"dD{self.id}\") } }\n\
             fn mkd(n: i64) -> D { return D { id: n, name: f\"n{n}\" }; }\n\
             struct W { r: D, s: D, b: i64 }\n\
             fn mkw(n: i64) -> W { return W { r: mkd(n), s: mkd(n + 100), b: n }; }\n\
             fn id(x: i64) -> i64 { x }\n\
             fn say(n: i64) -> i64 { println(f\"say{n}\"); n }\n\
             fn p1() -> i64 { mkw(1).b + 0 }\n\
             fn p2() -> i64 { id(mkw(2).b) }\n\
             fn p3(k: i64) -> i64 { match k { 0 => mkw(3).b, _ => 1 } }\n\
             fn p4() -> i64 { let d = mkd(40); mkw(4).b + d.id }\n\
             fn p5() -> i64 { -mkw(5).b }\n\
             fn p6() -> i64 { mkw(6).r.id * 2 }\n\
             fn p7() -> i64 { mkw(7).r.id + say(70) }\n\
             fn p8(c: bool) -> Result[i64, String] { let x: Result[i64, String] = if c { Err(f\"e\") } else { Ok(1) }; Ok(x? + mkw(8).b) }\n\
             fn p9() -> i64 { return mkw(9).b + 0 }\n\
             fn p10(n: i64) -> i64 { if n == 0 { 0 } else { mkw(n).b + p10(n - 1) } }\n\
             struct M { k: i64 }\n\
             impl M { fn m1(ref self) -> i64 { mkw(11).b + self.k } }\n\
             fn g1[T](t: T) -> i64 { mkw(12).b + 0 }\n\
             fn p13() -> i64 { mkw(13).b }\n\
             fn q3(c: bool) -> i64 { if c { mkw(23).b } else { 1 } }\n\
             fn q5(c: bool) -> i64 { if c { let z = mkw(25).b; z + 1 } else { 0 } }\n\
             fn q6(o: Option[i64]) -> i64 { if let Some(v) = o { mkw(26).b + v } else { 0 } }\n\
             fn q7(c: bool) -> i64 { if c { return mkw(27).b; } else { mkw(28).b + 0 } }\n";
    for (label, body, want) in [
        (
            "binary operator",
            "println(f\"t{p1()}\");",
            "dD101\ndD1\nt1\nend\n",
        ),
        (
            "call argument",
            "println(f\"t{p2()}\");",
            "dD102\ndD2\nt2\nend\n",
        ),
        (
            "match arm",
            "println(f\"t{p3(0)}\");",
            "dD103\ndD3\nt3\nend\n",
        ),
        (
            "beside a local with its own body",
            "println(f\"t{p4()}\");",
            "dD104\ndD4\ndD40\nt44\nend\n",
        ),
        (
            "unary operator",
            "println(f\"t{p5()}\");",
            "dD105\ndD5\nt-5\nend\n",
        ),
        (
            "read through a Drop-bearing field",
            "println(f\"t{p6()}\");",
            "dD106\ndD6\nt12\nend\n",
        ),
        (
            "before a later operand's side effect",
            "println(f\"t{p7()}\");",
            "say70\ndD107\ndD7\nt77\nend\n",
        ),
        (
            "after a `?` that did not leave",
            "println(f\"t{p8(false).unwrap()}\");",
            "dD108\ndD8\nt9\nend\n",
        ),
        (
            "a tail `return`",
            "println(f\"t{p9()}\");",
            "dD109\ndD9\nt9\nend\n",
        ),
        (
            "recursion through an if arm",
            "println(f\"t{p10(2)}\");",
            "dD101\ndD1\ndD102\ndD2\nt3\nend\n",
        ),
        (
            "method body",
            "let m = M { k: 1 }; println(f\"t{m.m1()}\");",
            "dD111\ndD11\nt12\nend\n",
        ),
        (
            "generic function body",
            "println(f\"t{g1(5)}\");",
            "dD112\ndD12\nt12\nend\n",
        ),
        (
            "if arm, bare projection",
            "println(f\"t{q3(true)}\");",
            "dD123\ndD23\nt23\nend\n",
        ),
        (
            "if arm with a statement first",
            "println(f\"t{q5(true)}\");",
            "dD125\ndD25\nt26\nend\n",
        ),
        (
            "if-let arm",
            "println(f\"t{q6(Some(1))}\");",
            "dD126\ndD26\nt27\nend\n",
        ),
        (
            "both exits of one function",
            "println(f\"t{q7(true)}{q7(false)}\");",
            "dD127\ndD27\ndD128\ndD28\nt2728\nend\n",
        ),
        (
            "control: a bare projection tail",
            "println(f\"t{p13()}\");",
            "dD113\ndD13\nt13\nend\n",
        ),
    ] {
        let prog = format!("{H}fn main() {{\n    {body}\n    println(\"end\")\n}}\n");
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

/// B-2026-09-05-13 — a by-value param REBOUND whole (`let m = r;`) and then
/// handed back through an `Option`/`Result` constructor runs the `Drop` body
/// ONCE, on every surface, unconditionally (`u-rebind`) and conditionally
/// (`rb-t`) alike. Before this both ran it TWICE — the caller's fresh-temp walk
/// and the discarded result binding — while the non-escaping call (`rb-f`)
/// was correct; all four surfaces agreed, so no A/B gate saw it.
///
/// The passthrough predicates are name-keyed; `param_rebind_aliases` now lets
/// `fn_always_returns_param` and `fn_conditionally_returns_param_bare` follow
/// the rebind. That alone was measured to LOSE `rb-f`'s body (the callee flip
/// dropped `r` by name while the value lived in `m`), which is why the
/// rebind site also hands the flip's bodies-only registration from `r` to `m`
/// on both backends, and why a rebind NESTED in a branch (`inbr-f`) disarms
/// `r` per path rather than statically.
///
/// Cells: `Result.Ok` (`ok-*`), the bare `return m` (`bare`), the NAMED
/// argument spellings (`named-*`, whose direct `top(b)` form was an
/// interpreter-only double before this), a two-hop chain (`chain-*`), the
/// arm-tail spelling (`tail-*`), the method spellings (`m-*`), and `shadow-f`,
/// where `m` is bound twice: the alias is DECLINED (today's behaviour keeps
/// the caller firing) and the dies-inside body must still run once. One
/// string across all four surfaces; the interpreter twin pins the same one.
#[test]
fn e2e_rebound_param_returned_in_ctor_runs_one_body() {
    assert_eq!(
            run_program(
                r#"struct R { id: i64, name: String }
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
}"#
            ),
            Some("u-rebind\ndrop 2 h2\nrb-f\ndrop 3 h3\nrb-t\ndrop 4 h4\nok-f\ndrop 5 h5\nok-t\ndrop 6 h6\nbare\ndrop 7 h7\nnamed-u\ndrop 8 h8\nnamed-top\ndrop 9 h9\nnamed-rb-f\ndrop 10 h10\nnamed-rb-t\ndrop 11 h11\nchain-f\ndrop 12 h12\nchain-t\ndrop 13 h13\ninbr-f\nafter\ndrop 14 h14\ninbr-t\ndrop 15 h15\ntail-f\ndrop 16 h16\ntail-t\ndrop 17 h17\nshadow-f\nsh 99\ndrop 99 h99\ndrop 18 h18\nm-rb-f\ndrop 20 h20\nm-rb-t\ndrop 21 h21\nm-u\ndrop 22 h22\ndone\n".to_string()),
            "a rebound by-value param handed back through a ctor runs one body"
        );
}

/// B-2026-09-06-12 — a by-value `Drop` param rebound THROUGH an
/// always-returning callee and then handed out (`let w: R = keeps(r);
/// return w;`) runs its body ONCE, from the caller's result binding. The
/// two alias-aware passthrough predicates (`fn_always_returns_param`,
/// `fn_conditionally_returns_param_bare`) now read the program-aware
/// alias set (`param_whole_aliases`), so `w` reads as `r` at every exit:
/// the outer caller stands down, and for the CONDITIONAL spelling the
/// callee's per-path flip moves from `r` to `w` at the `let`, exactly as
/// for `let m = r;`. Cells: `return w` / tail `w` / the direct
/// `return keeps(r)` control / rebind before / two-hop after / generic
/// callee / conditional return of `w` on both paths / wrapped in a struct
/// literal and an `Option` ctor / tuple param / a use before the return /
/// a method frame / a named argument / the discarded and statement
/// spellings / nested in a branch on both paths / a plain rebind of the
/// call result / `w` returned on every path with a print between.
/// Interpreter twin:
/// `test_param_rebound_through_returning_callee_then_returned_runs_one_body`.
#[test]
fn e2e_param_rebound_through_returning_callee_then_returned_runs_one_body() {
    assert_eq!(
            run_program(
                r#"struct R { id: i64, tag: String, xs: Vec[i64] }
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
}"#
            ),
            Some("one\ngot 1\ndR1\ntwo\ngot 2\ndR2\nthree\ngot 3\ndR3\nfour\ngot 4\ndR4\nfive\ngot 5\ndR5\nsix\ngot 6\ndR6\nseven-t\ngot 7\ndR7\neight-f\ndR8\ngot 99\ndR99\nnine\ngot 9\ndR9\nten\ngot 10\ndR10\neleven\ngot 11\ndR11\ntwelve\nmid 12\ngot 12\ndR12\nthirteen\ngot 13\ndR13\nfourteen-named\ngot 14\ndR14\nfifteen-discard\ndR15\ndisc\nsixteen-stmt\ndR16\nstmt\nseventeen-t\ngot 17\ndR17\neighteen-f\ndR18\ngot 98\ndR98\nnineteen\ngot 19\ndR19\ntwenty-f\nnp\ngot 20\ndR20\nend\n".to_string()),
            "a param rebound through an always-returning callee and returned has one owner"
        );
}

/// B-2026-08-29-31 — the `let _ =` spelling of a discarded branch now owns
/// whatever its arm hands out, on all three backends.
///
/// B-2026-08-29-5 fixed the BARE-STATEMENT form. The wildcard-`let` form of
/// the same `if` / `match` / block ran NO `Drop` body anywhere and leaked
/// one allocation per evaluation, because nothing told the arm its value
/// had no consumer: a wildcard `let` was not a recorded discarding
/// position, and — separately — `note_escaping_stmt_sites` marked its RHS
/// as ESCAPING, which cleared the conditional-move drop flag of whatever
/// local the taken arm named. Both halves had to go.
///
/// TWO POPULATIONS, and they are fixed by opposite means, which is why the
/// row could not be closed with one change:
///
///   * the tail names an ENCLOSING LOCAL — leave the source armed and its
///     own scope-exit body is the single fire (`*-local` rows);
///   * the tail names the arm's own PATTERN BINDING, which has already left
///     scope — nothing is left to arm, so the discard site must own it
///     (`payload-*` rows).
///
/// Twin: `test_wildcard_let_discard_owns_what_its_arm_hands_out`, same
/// shapes in the same order. Leak leg:
/// `asan_wildcard_let_discard_owns_what_its_arm_hands_out`.
///
/// The DOUBLE-OWN rows are the ones that cost the most to get right. A
/// bare value-block has no `discard_stmt_owns_value` to stand down
/// against, so making it a discarding position made it register a second
/// owner beside the statement frame's and the body ran TWICE — first for
/// `{ match … }`, and then, after a too-narrow first guard, for
/// `{ mk(7) }`. Both are pinned here at ONE.
/// B-2026-08-29-32 — the BODY-COUNT side of teaching the discard battery
/// to register memory for a body-less aggregate.
///
/// The row itself is invisible here: `P` has no `Drop`, so its leak
/// changes no transcript and `memory_sanitizer` is the fixture that sees
/// it. What THIS file is for is the double-own risk the fix runs. The
/// registration added below `try_track_discarded_user_drop_temp`'s early
/// return is reached by falling THROUGH a decision that used to return, so
/// the way it goes wrong is a type that already had an owner acquiring a
/// second one — which shows up as a doubled `Drop` body long before it
/// shows up as a double free. Each row here must print its body exactly
/// once.
/// B-2026-08-29-36 — a projection scrutinee DEEPER than one hop
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
/// B-2026-08-29-38 — a METHOD's FRESH-TEMP argument whose value the callee
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
fn e2e_method_fresh_temp_arg_handed_back_runs_one_body() {
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
        assert_eq!(run_program(&src).as_deref(), Some(want), "[{label}]");
    }
    // (a) FIXED by B-2026-08-31-46 and now asserted as correct: a struct
    // temp escaping inside a returned `Option.Some(r)` runs ONE body on
    // every surface, byte-identical to the interpreter twin. It was fixed
    // exactly as this comment used to say it had to be — not by widening a
    // conservative-true predicate (that would have skipped the caller-side
    // drop on the two `false` calls too, trading one doubled body for two
    // lost ones), but by teaching the per-path flip
    // (`fn_conditionally_returns_param_bare`) to see the constructor wrap,
    // and by making the variant-ctor payload retraction flag-aware when it
    // is nested in a branch. The two `false` cells are the in-program proof
    // that the non-escaping path still fires.
    //
    // (b) `t.keep(..)`, whose arm binds the payload but returns a SCALAR,
    // runs no body at all on the interpreter against one compiled — the
    // opposite direction, and a different defect. Filed separately.
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
        run_program(&format!(
            "{hdr2}fn main() {{\n\
                 let k = K {{ n: 1 }};\n\
                 let _ = k.f(mk(3), false); println(\"a\");\n\
                 let _ = k.f(mk(4), true);  println(\"b\");\n\
                 let _ = k.f(mk(5), false); println(\"c\");\n}}\n"
        ))
        .as_deref(),
        Some("drop 3 h3\na\ndrop 4 h4\nb\ndrop 5 h5\nc\n"),
        "[pinned DEFECT (a): temp escaping inside a returned Option ctor]"
    );
    assert_eq!(
        run_program(&format!(
            "{hdr2}fn main() {{\n\
                 let t = T {{ n: 1 }};\n\
                 let n = t.keep(Box2.Full(mk(7)));\n\
                 println(f\"n{{n}}\");\n}}\n"
        ))
        .as_deref(),
        Some("drop 7 h7\nn7\n"),
        "[pinned DEFECT (b): compiled runs the body the interpreter loses]"
    );
}

#[test]
fn e2e_param_view_field_moved_back_out_runs_one_body() {
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
                "#[allow(partial_move_of_drop_struct)]\n\
                 fn take(r: R) -> i64 { let d = D1 { r: r }; let x = d.r; return 7; }",
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
                "#[allow(partial_move_of_drop_struct)]\n\
                 fn take() -> i64 { let d = D1 { r: mk(1) }; let x = d.r; return 7; }",
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
            assert_eq!(run_program(&src).as_deref(), Some(want), "[{label}]");
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
        run_program(&tuple_moveout).as_deref(),
        Some("dR1\nv=7\n"),
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
        assert_eq!(run_program(&src).as_deref(), Some(want), "[{label}]");
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
/// `e2e_param_view_field_moved_back_out_runs_one_body` still pins.
#[test]
fn e2e_param_view_tuple_elem_moved_back_out_runs_one_body() {
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
        assert_eq!(run_program(&src).as_deref(), Some(want), "[{label}]");
    }
}

/// B-2026-09-07-8 — a mixed-path callee reached with the ENCLOSING frame's own
/// declined-copy param, through a rebind. `fn g(a: R, c: bool) { let q = a;
/// f(q, c); }` over `fn f(r: R, c: bool) -> R { let m = r; if c { return m; }
/// return mk(99); }` aborted `free(): double free detected in tcache 2` on
/// every compiled surface on the hand-back path, and ran the `Drop` body TWICE
/// for one object on the interpreter AND the compiled backends alike — a
/// uniform wrong answer, which is why no A/B gate and no sanitizer reported it.
/// The same call WITHOUT the rebind (`f(a, c)`) was correct throughout.
///
/// One predicate decided both. `fn_conditionally_hands_param_to_flip_callee` is
/// what tells the outer caller that some other frame takes the body per path
/// (B-2026-09-06-13); it matched the parameter's own name only, so the rebound
/// spelling answered false and the caller registered a full owner for its temp
/// alongside the callee's per-path one. It now follows the param's whole
/// aliases — and the rebind that CREATES an alias no longer counts as an
/// "other move" of the param, which is what had disqualified the very function
/// whose alias set it seeded.
///
/// Cells: the direct hand-back control, the rebound spelling, the rebound
/// spelling whose result is `let`-bound, and a rebind with no call at all.
/// The DIES-INSIDE legs are deliberately not here: they inherit the -O0 leak
/// B-2026-09-07-3 owns, which the un-rebound spelling has on `main` today and
/// which this fix neither causes nor cures.
///
/// Twin of `tests/interpreter.rs`'s `test_rebound_param_into_a_mixed_path_callee`, pinned to the same string.
#[test]
fn e2e_rebound_param_into_a_mixed_path_callee() {
    let Some(out) = run_program(
        r#"shared struct Inner { v: i64 }
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
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
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
/// Twin of `tests/interpreter.rs`'s `test_owned_self_field_let_runs_one_body`, pinned to the same string.
#[test]
fn e2e_owned_self_field_let_runs_one_body() {
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
    fn fieldlet(self) -> i64 { let e = self.e; match e { E.A(r) => { let m = r; return m.id; } E.B => { return 0; } } }
    fn readlet(self) -> i64 { let e = self.e; match e { E.A(r) => { return r.id; } E.B => { return 0; } } }
    fn justlet(self) -> i64 { let e = self.e; return 7; }
    fn borrowedlet(mut ref self) -> i64 { let e = self.e; match e { E.A(r) => { return r.id; } E.B => { return 0; } } }
}
impl H2 {
    #[allow(partial_move_of_drop_enum)]
    fn deep(self) -> i64 { let e = self.s.e; match e { E.A(r) => { let m = r; return m.id; } E.B => { return 0; } } }
    #[allow(partial_move_of_drop_enum)]
    fn mid(self) -> i64 { let s = self.s; match s.e { E.A(r) => { let m = r; return m.id; } E.B => { return 0; } } }
}
#[allow(partial_move_of_drop_enum)]
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
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
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
/// Twin of `tests/interpreter.rs`'s `test_bare_owned_struct_self_scrutinee_binds_views`, pinned to the same string.
#[test]
fn e2e_bare_owned_struct_self_scrutinee_binds_views() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("  dE") } }
struct H1 { e: E }
struct H2 { e: E, n: i64 }
struct Hs { e: E, s: String }
fn consume(x: R) -> i64 { return x.id }

impl H1 {
    #[allow(partial_move_of_drop_enum)]
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
    #[allow(partial_move_of_drop_enum)]
    fn m_r(self) -> R { match self { E.A(r) => { return r; } E.B => { return mk(0); } } }
}
#[allow(partial_move_of_drop_enum)]
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
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
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
/// Twin of `tests/interpreter.rs`'s `test_whole_self_rebind_in_owned_method_runs_each_body_once`, pinned to the same string.
#[test]
fn e2e_whole_self_rebind_in_owned_method_runs_each_body_once() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
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
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
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

/// The NEIGHBOURS of B-2026-09-07-59, pinned so the niche unpack cannot be
/// widened into the shapes that were already correct. A Vec / String /
/// `Option[i64]` field, and an `Option[shared T]` field on a PLAIN struct
/// outer, are all conventionally laid out — only a `shared` outer's
/// `Option[shared T]` field is niche-encoded, which is the discriminator
/// the fix keys on.
#[test]
fn test_e2e_clone_of_non_niche_fields_is_unchanged() {
    let src = r#"
shared struct Node { val: i64 }
struct Holder { o: Option[Node] }
struct S { v: Vec[i64], t: String, oi: Option[i64] }
fn main() {
    let s = S { v: [1, 2, 3], t: "hello", oi: Some(7) };
    println(f"{s.v.clone().len()}");
    println(f"{s.t.clone().len()}");
    println(f"{match s.oi.clone() { None => -1, Some(x) => x }}");
    let h = Holder { o: Some(Node { val: 9 }) };
    println(f"{match h.o.clone() { None => -1, Some(n) => n.val }}");
}
"#;
    assert_eq!(run_program(src).as_deref(), Some("3\n5\n7\n9\n"));
}

#[test]
fn test_e2e_interner_fresh_temp_intern_arg() {
    // A fresh-owned temp argument (`intern(p + "pha")` — a runtime concat)
    // must dedup against the equal literal AND get its buffer materialized
    // for scope-exit free (the runtime copies the bytes; nothing else owns
    // the temp — the ASAN twin pins the no-leak half).
    let out = run_program(
        r#"
fn main() {
    let mut tab: Interner = Interner.new();
    let a = tab.intern("alpha");
    let p = "al";
    let b = tab.intern(p + "pha");
    println(a == b);
    println(tab.len());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "true\n1");
    }
}

#[test]
fn test_e2e_nested_field_move_out_of_owned_param_values_survive() {
    // B-2026-08-13-3's VALUE side. Moving a nested heap field out of an
    // owned by-value param now zeroes that field's `cap` in the source, so
    // the param's callee-owned struct drop skips it — the memory claim is
    // the asan twin's. What this pins is that the value SURVIVES the
    // zeroing: `cap` is the ownership bit, `ptr`/`len` are untouched, so
    // the string the caller receives must still read correctly, and the
    // source struct's OTHER fields must still be droppable.
    //
    // `grown.inner.n` is printed right after `grown.inner.word` for that
    // second half: the scalar sibling of the moved field has to survive
    // intact, which a cap-zero at the wrong offset would corrupt.
    //
    // `let k = 1` rather than the asan twin's `env.args()`: this test is
    // the ORACLE, so it wants a fixed expected string, and defeating
    // constant-folding is the sanitizer fixture's job.
    assert_eq!(
        run_program(
            "                 struct Pair { word: String, n: i64 }\n\
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
                 }"
        )
        .as_deref(),
        Some("a1\nb1\n4\nd1\ne1\n"),
    );
}

#[test]
fn test_e2e_repeated_place_field_move_assign_reads_correctly() {
    // B-2026-08-12-4's alias guard. Freeing the assignment target's
    // displaced buffer is only safe when it is a DIFFERENT buffer from the
    // incoming one, which every other arm of `trigger_eager_free` arranges
    // structurally. The place-field-move arm cannot: the first
    // `cur = box[0].s` hands `cur` the element's buffer AND cap-zeroes the
    // source, so running the same assignment again reads a place that now
    // aliases `cur` itself — and the free would reclaim the buffer about to
    // be stored back.
    //
    // A CONTENT read, not `.len()`: the length is carried in the header, so
    // a `.len()`-only pin reads correctly off a dangling pointer and sees
    // nothing. Without the guard this printed `region` then garbage, with
    // valgrind reporting two invalid reads — the fix for the leak, applied
    // unguarded, would have traded it for a use-after-free.
    assert_eq!(
        run_program(
            "struct S { s: String }\n\
                 fn main() {\n\
                     let mut box_: Vec[S] = Vec.new();\n\
                     let mut nm = String.new();\n\
                     nm.push_str(\"region\");\n\
                     box_.push(S { s: nm });\n\
                     let mut cur = String.new();\n\
                     cur = box_[0].s;\n\
                     println(cur);\n\
                     cur = box_[0].s;\n\
                     println(cur);\n\
                 }"
        )
        .as_deref(),
        Some("region\nregion\n"),
    );

    // B-2026-08-12-13 restores the target's `cap` through the same guard so
    // it stays the buffer's sole owner. A `Vec[i64]` field re-read three
    // times must still read its ELEMENTS back, which is what would break if
    // the restore handed back a wrong capacity.
    assert_eq!(
        run_program(
            "struct S { xs: Vec[i64] }\n\
                 fn main() {\n\
                     let mut box_: Vec[S] = Vec.new();\n\
                     let mut inner: Vec[i64] = Vec.new();\n\
                     inner.push(4); inner.push(5); inner.push(6);\n\
                     box_.push(S { xs: inner });\n\
                     let mut got: Vec[i64] = Vec.new();\n\
                     got = box_[0].xs;\n\
                     got = box_[0].xs;\n\
                     got = box_[0].xs;\n\
                     println(f\"{got.len()} {got[0]} {got[2]}\");\n\
                 }"
        )
        .as_deref(),
        Some("3 4 6\n"),
    );

    // Interleaved DISTINCT and REPEATED sources through one target. The
    // distinct steps must still free the displaced buffer (the
    // B-2026-08-12-4 arm) while the repeated step must not (the guard), so
    // this is the case where getting either half wrong shows up: a missed
    // free leaks, an unguarded free reads back garbage.
    assert_eq!(
        run_program(
            "struct S { s: String }\n\
                 fn main() {\n\
                     let mut box_: Vec[S] = Vec.new();\n\
                     let mut i: i64 = 0;\n\
                     while i < 3 {\n\
                         let mut nm = String.new();\n\
                         nm.push_str(\"elem\");\n\
                         box_.push(S { s: nm });\n\
                         i = i + 1;\n\
                     }\n\
                     let mut cur = String.new();\n\
                     cur = box_[0].s;\n\
                     cur = box_[1].s;\n\
                     cur = box_[1].s;\n\
                     cur = box_[2].s;\n\
                     println(cur);\n\
                 }"
        )
        .as_deref(),
        Some("elem\n"),
    );
}

#[test]
fn test_e2e_clone_in_a_chain_resolves_to_its_receiver_type() {
    // B-2026-08-11-22, second leg. `x.clone()` as a receiver inside a chain
    // had no resolvable type, so the same span-shadowed dispatch key that
    // broke `n.to_string().to_string()` also broke anything routed through
    // a clone — `s.clone().to_string().len()` and
    // `n.clone().to_string().len()` both died as "no handler for method
    // 'to_string' on non-identifier receiver", on both a String and a
    // scalar receiver.
    //
    // `clone` preserves its receiver's type, so resolving it by recursion
    // is right for every receiver — and the Vec case is the control that it
    // did not become "string-like" wholesale, which is the mistake the
    // neighbouring name list exists to avoid.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let s: String = \"hello\".to_string();\n\
                     println(s.clone().to_string().len());\n\
                     let n: i64 = 12345;\n\
                     println(n.clone().to_string().len());\n\
                     let v: Vec[i64] = [1, 2, 3];\n\
                     println(v.clone().len());\n\
                 }\n"
        )
        .as_deref(),
        Some("5\n5\n3\n"),
    );
}

#[test]
fn test_e2e_nested_struct_field_move_out_no_double_free() {
    // B-2026-07-15-22: `let bound = o.inner` — moving a STRUCT-typed field out
    // of an owned struct where that field's type carries heap (a `Vec`). Before
    // the fix, `bound`'s scope-exit StructDrop AND `o`'s StructDrop both freed
    // the same inner `Vec` buffer → `free(): double free detected` (exit 134).
    // The struct-var tracking path suppressed only `Identifier` (`let g = f`)
    // and `TupleIndex` (`let inr = h.ps.0`) RHS move-outs, never a `FieldAccess`
    // struct-typed field — so no cap-zero disarmed the source's drop. Fixed by
    // calling `suppress_struct_field_move_into_literal` (its nested-aggregate arm
    // recurses into the moved-out struct's Vec/String leaves) for the FieldAccess
    // RHS. Both generic and non-generic reproduced; both are `karac check`-clean.
    // Covers: non-generic, generic, deep (two-level) nesting, multi-heap-field
    // moved-out struct, `self.field` move-out in a consuming method, and a
    // sibling heap field of the outer struct that must STILL free (not
    // over-suppressed). This is the OUTPUT-correctness guard (the cap-zero must
    // not corrupt the `.len()` reads); the double-free/leak itself is caught by
    // the ASAN sibling `asan_nested_struct_field_move_out_no_double_free` — the
    // abort fires at scope exit AFTER these prints, so `run_program`'s captured
    // stdout can't witness it. Each Vec is built via a tracked local
    // (`Vec.new()+push`) rather than an inline array literal, because only that
    // construction takes the move path that reproduced.
    if let Some(out) = run_program(
        "struct Inner { data: Vec[i64] }\n\
             struct Outer { inner: Inner, extra: Vec[i64] }\n\
             struct GInner[T] { data: Vec[T] }\n\
             struct GOuter[T] { inner: GInner[T] }\n\
             struct Mid { inner: Inner }\n\
             struct Deep { mid: Mid }\n\
             struct Multi { a: Vec[i64], b: String }\n\
             struct MOuter { m: Multi }\n\
             struct Con { inner: Inner, tag: i64 }\n\
             impl Con {\n\
                 fn take(self) -> Inner {\n\
                     let x: Inner = self.inner;\n\
                     x\n\
                 }\n\
             }\n\
             fn main() {\n\
                 let mut d0: Vec[i64] = Vec.new();\n\
                 d0.push(1); d0.push(2); d0.push(3);\n\
                 let mut e0: Vec[i64] = Vec.new();\n\
                 e0.push(100); e0.push(200);\n\
                 let o: Outer = Outer { inner: Inner { data: d0 }, extra: e0 };\n\
                 let bound: Inner = o.inner;\n\
                 println(bound.data.len());\n\
                 println(o.extra.len());\n\
                 let mut gd: Vec[i64] = Vec.new();\n\
                 gd.push(7); gd.push(8); gd.push(9); gd.push(10);\n\
                 let g: GOuter[i64] = GOuter { inner: GInner { data: gd } };\n\
                 let gb: GInner[i64] = g.inner;\n\
                 println(gb.data.len());\n\
                 let mut dd: Vec[i64] = Vec.new();\n\
                 dd.push(5); dd.push(6);\n\
                 let d: Deep = Deep { mid: Mid { inner: Inner { data: dd } } };\n\
                 let m: Mid = d.mid;\n\
                 let mi: Inner = m.inner;\n\
                 println(mi.data.len());\n\
                 let mut ma: Vec[i64] = Vec.new();\n\
                 ma.push(1); ma.push(2); ma.push(3); ma.push(4); ma.push(5);\n\
                 let mo: MOuter = MOuter { m: Multi { a: ma, b: \"tag\" } };\n\
                 let mm: Multi = mo.m;\n\
                 println(mm.a.len());\n\
                 println(mm.b.len());\n\
                 let mut cd: Vec[i64] = Vec.new();\n\
                 cd.push(11); cd.push(22); cd.push(33);\n\
                 let c: Con = Con { inner: Inner { data: cd }, tag: 9 };\n\
                 let got: Inner = c.take();\n\
                 println(got.data.len());\n\
             }",
    ) {
        assert_eq!(out, "3\n2\n4\n2\n5\n3\n3\n");
    }
}

/// B-2026-08-09-11 — the CONSUMING half of the block spellings, the
/// sibling of cases 10 and 11 above (which pin the READ-ONLY half).
///
/// Leg 3 gave the live-local clone (`clone_escaping_live_local_enum`) to
/// the `match` site only, so the block sites kept the transfer path and
/// emptied the source: each of these printed the payload once and then an
/// EMPTY line, while `--interp` printed it twice.
///
/// `let…else` is NOT here, and the leg is not wired at that site. A probe
/// reproduced the same signature there, but `karac check` rejects it: the
/// `let…else` MOVES the scrutinee, so the later read the leg keys on is a
/// `UseAfterMove` that never reaches codegen. See the comment at
/// `compile_let_else` for why the checker-clean spelling cannot observe
/// the bug either.
#[test]
fn test_e2e_consuming_block_spellings_keep_the_live_source_for_user_enums() {
    // 1. `if let` — the row's own reproduction.
    assert_eq!(
        run_program(
            "enum E { A(String), B }\n\
                 fn main() {\n\
                     let e: E = E.A(f\"hi\");\n\
                     if let E.A(v) = e { let k: String = v; println(k); }\n\
                     if let E.A(v) = e { println(v); }\n\
                 }"
        )
        .as_deref(),
        Some("hi\nhi\n")
    );
    // 2. `while let`, whose clone emits per evaluation in the header.
    //    The body reassigns the scrutinee so the loop terminates; the
    //    source is read after it, which is what arms the leg.
    assert_eq!(
        run_program(
            "enum E { A(String), B }\n\
                 fn main() {\n\
                     let mut e: E = E.A(f\"hi\");\n\
                     while let E.A(v) = e { let k: String = v; println(k); e = E.B; }\n\
                     if let E.A(w) = e { println(w); } else { println(f\"empty-ok\"); }\n\
                 }"
        )
        .as_deref(),
        Some("hi\nempty-ok\n")
    );
    // 3. DEAD source, `if let` — the liveness gate must still decline, or
    //    every consuming if-let pays for a clone it does not need. Same
    //    output before and after the fix; it is here to pin that.
    assert_eq!(
        run_program(
            "enum E { A(String), B }\n\
                 fn main() {\n\
                     let e: E = E.A(f\"hi\");\n\
                     if let E.A(v) = e { let k: String = v; println(k); }\n\
                     println(f\"done\");\n\
                 }"
        )
        .as_deref(),
        Some("hi\ndone\n")
    );
}

#[test]
fn test_e2e_bug8_if_tail_call_no_leak() {
    // E2E guard for the branch-shape fix — the value side was
    // correct before the fix too (rc=2 vs rc=1 doesn't change
    // the pointee bytes), but locking the program behavior here
    // documents the intended semantics and pairs with the
    // IR-level gates above for a layered regression net.
    let out = run_program(
        r#"
shared struct S { val: i64 }
fn make_a() -> S { let s = S { val: 10 }; s }
fn make_b() -> S { let s = S { val: 20 }; s }
fn main() {
    let x = if true { make_a() } else { make_b() };
    println(x.val);
    let y = if false { make_a() } else { make_b() };
    println(y.val);
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["10", "20"]);
    }
}

#[test]
fn test_e2e_descending_skip_keeps_the_check_when_a_nested_block_moves_the_goalposts() {
    // B-2026-08-04-13. The descending-loop BCE skip freezes four facts by
    // scanning a region: the fill counter starts at 0, the enclosing
    // counter still satisfies its guard at the inner loop, and the index's
    // init is the value written before it. `stmt_writes_ident` answered all
    // of them looking only at TOP-LEVEL assignment targets, so a write one
    // block deep left every fact reading "unchanged" while the program
    // changed it — and the skip then dropped a check that was carrying
    // real weight.
    //
    // This is an E2E and not an IR assertion on purpose: the failure is a
    // *store* past the end of the buffer, and what that looks like is
    // allocator-dependent (the three cases below produced a silent exit 0,
    // `munmap_chunk(): invalid pointer`, and a glibc malloc assertion).
    // Asserting the clean panic is the only stable oracle; any of those
    // corruption modes fails it.
    //
    // `k` starts at `i + 9` and walks down, on a Vec filled to 10 — in
    // range for the control, and pushed out of it by each lever: the first
    // two raise `k` itself, the third shrinks the buffer under it.
    let case_src = |pre_fill: &str, enc_head: &str, post_init: &str| {
        format!(
            r#"
fn main() {{
    let mut v: Vec[i64] = Vec.new();
    let mut j = 0i64;
    let flag = 1i64;
    {pre_fill}
    while j < 10i64 {{ v.push(0i64); j = j + 1i64; }}
    let mut i = 0i64;
    while i <= 0i64 {{
        {enc_head}
        let mut k = i + 9i64;
        {post_init}
        while k >= 0i64 {{ v[k] = 7i64; k = k - 1i64; }}
        i = i + 1i64;
    }}
    println(f"len={{v.len()}}");
}}
"#
        )
    };

    // Control first: with no nested write every index is in range, so the
    // program completes. This is what makes the three assertions below
    // about the nested write and not about a program that was always OOB.
    if let Some(c) = run_program_capturing(&case_src("", "", "")) {
        assert!(
            c.stdout.contains("len=10"),
            "control must run clean, got stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
    }

    let cases = [
        (
            "enclosing counter rewritten in a nested block",
            "",
            "if flag == 1i64 { i = 50i64; }",
            "",
        ),
        (
            "index init rewritten in a nested block",
            "",
            "",
            "if flag == 1i64 { k = 50i64; }",
        ),
        // Here the index arithmetic is sound and the PREMISE is not: the
        // fill runs 5 iterations instead of 10, so the length pin claims a
        // buffer twice the size of the real one.
        (
            "fill counter preset in a nested block",
            "if flag == 1i64 { j = 5i64; }",
            "",
            "",
        ),
    ];
    for (label, pre_fill, enc_head, post_init) in cases {
        if let Some(c) = run_program_capturing(&case_src(pre_fill, enc_head, post_init)) {
            assert!(
                c.stderr.contains("vec index out of bounds"),
                "[{label}] an out-of-range store must panic, not be elided; \
                     got stdout={:?} stderr={:?}",
                c.stdout,
                c.stderr
            );
        }
    }
}

#[test]
fn test_e2e_struct_field_move_no_double_free() {
    // Move-aware suppression at struct-construction sites. When
    // a struct field's initializer is an Identifier naming a
    // tracked Vec / String, the field captures the binding's
    // data pointer — but the source's let-site `track_vec_var`
    // unconditionally schedules a scope-exit free that would
    // free the buffer the caller now reads through the struct.
    // This is the shape Parallax/HTTP hits via
    // `Response { body: my_string }` — without suppression,
    // the caller reads NUL bytes (or SIGSEGVs) downstream of
    // FFI consumption.
    let out = run_program(
        r#"
struct Holder {
    tag: i64,
    body: String,
}
fn build() -> Holder {
    let mut s: String = String.new();
    s.push_str("hello, world");
    Holder { tag: 7, body: s }
}
fn main() {
    let h: Holder = build();
    println(h.tag);
    println(h.body);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "7\nhello, world");
    }
}

#[test]
fn test_e2e_struct_param_field_move_out() {
    // B-2026-06-10-2: moving a Vec field OUT of a by-value struct PARAM
    // (`let inner = h.v`) deep-copies the field buffer so the moved-out
    // local is independent of the caller's (the caller's struct-drop frees
    // the original). Pre-fix this double-freed (the buffer was shallow-
    // shared; ASAN coverage in `tests/memory_sanitizer.rs`). Output
    // correctness on the codegen lane is the non-ASAN guard: reuse + the
    // original both stay valid.
    let out = run_program(
        "struct Holder { v: Vec[i64] }\n\
             fn build() -> Holder {\n\
             \x20   let mut inner: Vec[i64] = Vec.new();\n\
             \x20   inner.push(10i64); inner.push(20i64);\n\
             \x20   Holder { v: inner }\n\
             }\n\
             fn first_elem(h: Holder) -> i64 { let inner = h.v; inner[0] }\n\
             fn main() {\n\
             \x20   let h = build();\n\
             \x20   let a = first_elem(h);\n\
             \x20   let b = first_elem(h);\n\
             \x20   println(a + b);\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "20");
    }
}

#[test]
fn test_e2e_let_rebind_move_no_double_free() {
    // `let outer = inner;` where `inner` is a tracked Vec /
    // String is a move — both slots end up holding the same
    // {ptr, len, cap}. Without source-cap suppression at the
    // let-rebind site, both `track_vec_var`-queued cleanups
    // fire and double-free the heap buffer. The LHS's track
    // becomes the unique cleanup owner.
    let out = run_program(
        r#"
fn build() -> String {
    let mut inner: String = String.new();
    inner.push_str("relayed");
    let outer: String = inner;
    outer
}
fn main() {
    let s: String = build();
    println(s);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "relayed");
    }
}

#[test]
fn test_e2e_assign_rebind_move_no_double_free() {
    // `acc = extra;` where both `acc` and `extra` are tracked
    // Vec / String bindings is an assign-rebind. The old
    // `acc` buffer leaks (no RAII drop in v1), but without
    // source-cap suppression on `extra`, both queued cleanups
    // fire against the same post-assign buffer → double-free.
    let out = run_program(
        r#"
fn build() -> String {
    let mut acc: String = String.new();
    acc.push_str("first");
    let mut extra: String = String.new();
    extra.push_str("second");
    acc = extra;
    acc
}
fn main() {
    let s: String = build();
    println(s);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "second");
    }
}

#[test]
fn test_e2e_clone_on_primitives() {
    // Both identifier and literal/expr receivers — the type-based gate
    // in `compile_method_call` handles any receiver form.
    if let Some(out) = run_program(
        r#"
fn main() {
    let i = 7i64;
    println(i.clone());
    let f = 2.5f64;
    println(f.clone());
    println((-3i64).clone());
    println(true.clone());
    println('q'.clone());
    let u = 99u64;
    println(u.clone());
}
"#,
    ) {
        assert_eq!(out, "7\n2.5\n-3\ntrue\nq\n99\n");
    }
}

#[test]
fn test_e2e_tuple_elem_bind_move_out() {
    // #27 (phase-12 self-hosting, B-2026-06-14-8) — binding a heap-bearing
    // value OUT of a tuple element. `let inr = h.ps.0` (a heap-bearing struct
    // moved out of a tuple element) and `let tk = h.ps.0.tok` (an enum field
    // moved out of a tuple-element struct) each registered the binding's drop
    // but did NOT suppress the SOURCE — so both the binding's drop and the
    // owning `h`'s `NestedTuple` tuple drop freed the same buffer
    // (double-free). Fix: call `suppress_tuple_index_move_source` in the
    // struct-binding path (cap-zeros the struct element via
    // `zero_struct_move_caps`), and add `suppress_place_field_enum_move_source`
    // for the `<tupleindex>.field` enum form (place-chain GEP +
    // `zero_enum_payload_caps`). Correctness here is the moved-out value
    // surviving + reading right (consume + read forms); the double-free is in
    // `asan_tuple_elem_bind_move_out_no_double_free`. A heapless tuple element
    // (`Plain`) is the regression guard.
    if let Some(out) = run_program(
        r#"
enum Tok { Id(String), Num(i64) }
struct Inner { tok: Tok, n: i64 }
struct Hs { ps: (Inner, i64) }
struct Plain { a: i64, b: i64 }
struct Hp { ps: (Plain, i64) }
fn main() {
    // Struct element moved out, then read a field.
    let h1 = Hs { ps: (Inner { tok: Tok.Id("alpha".to_string()), n: 42 }, 7) };
    let inr = h1.ps.0;
    println(inr.n.to_string());                 // 42
    // Struct element moved out, then CONSUME its enum field via match.
    let h2 = Hs { ps: (Inner { tok: Tok.Id("beta".to_string()), n: 1 }, 2) };
    let inr2 = h2.ps.0;
    match inr2.tok { Id(s) => { println(s); } Num(n) => { println(n.to_string()); } }  // beta
    // Enum field moved out THROUGH the tuple element, then consumed.
    let h3 = Hs { ps: (Inner { tok: Tok.Id("gamma".to_string()), n: 9 }, 3) };
    let tk = h3.ps.0.tok;
    match tk { Id(s) => { println(s); } Num(n) => { println(n.to_string()); } }        // gamma
    // Heapless tuple element — regression guard (reads the right field).
    let hp = Hp { ps: (Plain { a: 5, b: 6 }, 7) };
    let p = hp.ps.0;
    println(p.a.to_string());                   // 5
}
"#,
    ) {
        assert_eq!(out, "42\nbeta\ngamma\n5\n");
    }
}

#[test]
fn test_ir_discarded_unit_call_no_owned_temp() {
    // Negative: a discarded Call/MethodCall that does NOT yield a
    // Vec/String (here a unit-returning `println`) must not spuriously
    // allocate an `__owned_tmp` slot — `materialize_owned_temp` gates on
    // the `{ptr,len,cap}` LLVM value type, so non-heap discards are
    // untouched. Guards against the chokepoint over-reaching.
    let src = r#"
fn main() {
    println("x");
}
"#;
    let ir = ir_for(src);
    assert!(
        !ir.contains("__owned_tmp"),
        "unit-returning discard must not materialize an owned temp; got:\n{}",
        ir
    );
}

#[test]
fn test_ir_method_chain_field_receiver_no_owned_temp() {
    // Negative / double-free guard: a *place*-expression receiver
    // (`h.items.len()`, a field access) reloads a buffer the `h` binding
    // owns. `expr_yields_fresh_owned_temp` excludes it, so the receiver
    // path must NOT materialize an `__owned_tmp` — freeing it would
    // double-free against `h`'s own scope-exit cleanup. (Construction
    // uses no other fresh-temp method chain, so any `__owned_tmp` here
    // could only come from the field receiver.)
    let src = r#"
struct Holder { items: Vec[i64] }

fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1_i64);
    let h = Holder { items: v };
    let n = h.items.len();
    println(n);
}
"#;
    let ir = ir_for(src);
    assert!(
        !ir.contains("__owned_tmp"),
        "a field-access receiver must not materialize an owned temp \
             (would double-free against the binding's cleanup); got:\n{}",
        ir
    );
}

#[test]
fn test_ir_discarded_block_tail_temp_freed() {
    // Slice 5 (tail-expr temp drop): a fresh `Vec` produced in the tail
    // of a statement-position block (`{ make_vec() }`) is the block's
    // return value — its frame drops only block-local lets, so the tail
    // Vec escaped uncleaned and leaked. `discarded_owned_temp_tail` now
    // peels the block to its `make_vec()` tail, and the discard arm
    // routes it through `materialize_owned_temp` (keyed on the *tail*
    // expr's span). Archive-independent (macOS ASAN has no LeakSanitizer).
    let src = r#"
fn make_vec() -> Vec[i64] {
    let mut v: Vec[i64] = Vec.new();
    v.push(1_i64);
    return v;
}

fn main() {
    { make_vec() }
    println(0);
}
"#;
    let ir = ir_for(src);
    assert!(
        ir.contains("__owned_tmp"),
        "expected the block-tail Vec temp materialized into __owned_tmp; got:\n{}",
        ir
    );
    assert!(
        ir.contains("cleanup.free"),
        "expected a FreeVecBuffer drain for the discarded block-tail temp; got:\n{}",
        ir
    );
}

#[test]
fn test_ir_discarded_branching_tail_temp_is_tracked() {
    // B-2026-08-29-5 lifts slice 5's deferral. This test asserted the
    // opposite until then — "a safe leak, deferred" — because slice 5
    // peeled only SINGLE-TAIL block wrappers and a branching tail was
    // routed nowhere. The deferral rested entirely on the risk it names:
    // an ALIASING place-expr branch (`if c { v } else { w }`) would be
    // double-freed against the sources' own cleanups.
    //
    // The owner is now registered inside the ARM, not at the statement,
    // and only when the arm's tail MINTS its value
    // (`expr_yields_fresh_owned_temp`) — which is exactly what excludes
    // the aliasing branch the deferral was protecting. Its sibling below
    // pins that half, so the guard the old assertion stood for is still
    // gated, by a predicate rather than by declining the whole shape.
    //
    // B-2026-08-29-25 then gave the STATEMENT site an `if` leg of its own,
    // so two mechanisms can claim this shape and freeing it twice is a
    // real double free (measured, on the `[if-fresh-in-both-arms]` ASAN
    // fixture). They partition it instead: `compile_if` asks
    // `discarded_if_parts_qualify` and stands the arm-level owner down
    // when the statement site will take ownership, which leaves the arm
    // path exactly the two cases the statement gate declines — a
    // no-`else` branch and an arm tail naming an existing binding. Either
    // mechanism satisfies this assertion; the ASAN suite is what says
    // only one of them fires.
    let src = r#"
fn make_vec() -> Vec[i64] {
    let mut v: Vec[i64] = Vec.new();
    v.push(1_i64);
    return v;
}

fn main() {
    let cond = true;
    { if cond { make_vec() } else { make_vec() } }
    println(0);
}
"#;
    let ir = ir_for(src);
    assert!(
        ir.contains("__owned_tmp"),
        "expected each discarded branch arm's fresh tail temp to be \
             materialized and freed with the arm; got:\n{}",
        ir
    );
}

#[test]
fn test_ir_discarded_branching_place_tail_not_tracked() {
    // The other half of B-2026-08-29-5, and the guard the assertion above
    // used to stand for: a discarded branch whose arms hand out EXISTING
    // BINDINGS must register no owner of its own. Those buffers already
    // have owners — the bindings themselves, whose cleanups this fix stops
    // suppressing — so materializing here would free them twice.
    let src = r#"
fn make_vec() -> Vec[i64] {
    let mut v: Vec[i64] = Vec.new();
    v.push(1_i64);
    return v;
}

fn main() {
    let cond = true;
    let a = make_vec();
    let b = make_vec();
    { if cond { a } else { b } }
    println(0);
}
"#;
    let ir = ir_for(src);
    assert!(
        !ir.contains("__owned_tmp"),
        "an aliasing place-expr branch tail must register no owner of its \
             own — the bindings keep theirs; got:\n{}",
        ir
    );
}

/// B-2026-07-11-35 (return-owned-`T`-param leg): returning an owned heap
/// (String / Vec) PARAM from a GENERIC fn (`fn echo[T](x: T) -> T { x }`)
/// double-freed under codegen (JIT + native) while the non-generic
/// `fn f(x: String) -> String { x }` and the interpreter oracle were clean.
/// Root: `compile_mono_function`'s tail return only did the Identifier-tail
/// MOVE suppression, skipping the owned-vecstr retaining-consume DEEP-COPY
/// that `compile_function` applies — so it handed back the caller's moved-in
/// buffer, which the caller then freed a second time. The mono tail now
/// deep-copies, mirroring the non-generic path. Also pins the mono-mangle
/// COLLISION the copy exposed: `String`, `Vec[i64]`, and `Vec[String]` all
/// lower to `{ptr,i64,i64}` and had collapsed onto one `echo$struct` body,
/// so the copy ran the first instantiation's element stride over the others
/// (a 3-byte under-copy of a Vec[i64] under String's i8 stride — UB). Each
/// builtin-collection whole-param instantiation now gets a distinct symbol
/// (`echo$struct$T_ct_String` / `$T_ct_Vec_i64` / `$T_ct_Vec_String`) with
/// its own correctly-strided body. `i64` (POD) is unchanged. Memory safety
/// is pinned in `tests/memory_sanitizer.rs::asan_return_owned_generic_param_*`.
#[test]
fn e2e_return_owned_generic_param_no_double_free() {
    if let Some(out) = run_program(
        "fn echo[T](x: T) -> T { x }\n\
             fn main() {\n\
             \x20   let a: String = echo(f\"aaa-fresh\");\n\
             \x20   println(a);\n\
             \x20   let s: String = f\"bbb-local\";\n\
             \x20   let b: String = echo(s);\n\
             \x20   println(b);\n\
             \x20   let c: i64 = echo(777);\n\
             \x20   println(f\"{c}\");\n\
             \x20   let vi: Vec[i64] = echo([10, 20, 30]);\n\
             \x20   println(f\"{vi[1]}\");\n\
             \x20   let vs: Vec[String] = echo([f\"pp\", f\"qq\"]);\n\
             \x20   println(vs[0]);\n\
             \x20   let d: String = echo(echo(f\"ccc-chain\"));\n\
             \x20   println(d);\n\
             }",
    ) {
        assert_eq!(out, "aaa-fresh\nbbb-local\n777\n20\npp\nccc-chain\n");
    }
}

#[test]
fn test_ir_a_reduction_over_a_struct_owned_buffer_does_not_free_it() {
    // The place-vs-temporary rule, at the point where making the type
    // storable changed the answer. `sim.grid` is a PLACE — the struct owns
    // that buffer and the reduction only reads it — so the reduction must
    // emit no free of its own; the struct's drop is the single owner.
    //
    // Under the older "a bare identifier is a binding, anything else is a
    // temporary" rule this freed the field at the first reduction, and a
    // second read of `sim.grid` hit the runtime's already-freed guard.
    // Two reductions plus one struct here: exactly one free (plus its
    // declare) is correct.
    let src = r#"
struct Body { mass: f32, speed: f32 }
struct Sim  { grid: GpuBuffer[Body], step: i32 }

fn main() {
    let bodies: Vec[Body] = [Body { mass: 1.0, speed: 2.0 }];
    let sim = Sim { grid: gpu.upload(bodies), step: 0 };
    println(f"{gpu.sum(sim.grid.mass)}");
    println(f"{gpu.sum(sim.grid.speed)}");
}
"#;
    let ir = ir_for_with_ownership(src);
    let frees = ir.matches("karac_runtime_gpu_free_soa").count();
    assert_eq!(
        frees, 2,
        "a struct-owned buffer must be freed once by the struct's drop and \
             never by a reduction over it (one declare + one call), got {frees}:\n{ir}"
    );
}

#[test]
fn test_with_provider_e2e_owned_self_with_extra_args() {
    // Owned-self dispatch with additional method args. The fix's
    // self-arg construction sits ahead of the user-args loop; this
    // pins that the trailing args still thread through correctly.
    let src = "pub trait Adder { fn add(self, x: i64) -> i64; }\n\
            pub struct H { base: i64 }\n\
            impl Adder for H { fn add(self, x: i64) -> i64 { self.base + x } }\n\
            pub effect resource A: Adder;\n\
            fn main() {\n\
              let p = H { base: 10 };\n\
              with_provider[A](p, || { println(A.add(5)); });\n\
            }";
    let Some(out) = run_program(src) else {
        eprintln!("skipping with_provider owned-self+args e2e: runtime/linker unavailable");
        return;
    };
    assert_eq!(out.trim(), "15");
}

#[test]
fn test_e2e_freshtemp_field_access_all_consumers() {
    // B-2026-07-22-2: a FRESH call-result struct temp whose field is
    // read in expression position never dropped its aggregate — every
    // heap field leaked, even unread ones (x86 -O2 DCE'd the dead
    // allocations; arm64 -O2 did not, going red on the
    // memory-sanitizer-arm64 leg via the closure-capture test whose
    // consumers hit exactly this shape). The temp is now materialized
    // and drop-tracked at the access; move consumers (let / assign /
    // return / fn tail / consuming match arms) zero the accessed field
    // in the slot. This E2E pins output correctness across the full
    // consumer matrix; the LSan sibling guards the memory halves.
    let output = run_program(
        "struct W { s: String }\n\
             struct W2 { s: String, t: String }\n\
             struct Wv { v: Vec[i64] }\n\
             struct H { opt: Option[String], n: i64 }\n\
             fn mk() -> W { return W { s: \"one\".to_string() }; }\n\
             fn mk2() -> W2 { return W2 { s: \"aa\".to_string(), t: \"bb\".to_string() }; }\n\
             fn mkv() -> Wv { return Wv { v: [1, 2, 3] }; }\n\
             fn mko() -> H { return H { opt: Some(\"op\".to_string()), n: 5 }; }\n\
             fn take(x: String) -> i64 { return x.len(); }\n\
             fn get() -> String { return mk().s; }\n\
             fn get2() -> String { mk().s }\n\
             fn main() {\n\
                 println(mk().s);\n\
                 println(take(mk().s).to_string());\n\
                 println(mkv().v.len().to_string());\n\
                 println(mko().n.to_string());\n\
                 let s = mk2().s;\n\
                 println(s);\n\
                 let mut acc = \"seed\".to_string();\n\
                 acc = mk().s;\n\
                 println(acc);\n\
                 println(get());\n\
                 println(get2());\n\
                 match mko().opt {\n\
                     Some(p) => { println(\"m:\".to_string() + p); }\n\
                     None => { }\n\
                 }\n\
                 if let Some(q) = mko().opt {\n\
                     println(\"i:\".to_string() + q);\n\
                 }\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "one\n3\n3\n5\naa\none\none\none\nm:op\ni:op\n");
}

#[test]
fn test_e2e_file_moved_into_a_struct_survives_a_function_return() {
    // The same B-2026-08-09-17 defect with no `Vec` and no match arm in the
    // consuming function: the handle is moved into a struct literal and the
    // struct is RETURNED. The origin binding's scope-exit close fired as the
    // helper returned, so the caller received a struct whose `File` field
    // pointed at freed memory.
    //
    // Worth pinning separately from the Vec case because the first diagnosis
    // of this bug was "Vec-element-specific" — a struct field appeared to
    // work, but only because that test read the handle while the origin
    // binding was still in scope. Containers were never the discriminator;
    // outliving the origin binding is.
    let tmp = std::env::temp_dir().join("karac_e2e_file_moved_into_struct.txt");
    let _ = std::fs::remove_file(&tmp);
    std::fs::write(&tmp, b"ABCDEFGH").expect("temp write");
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    let src = format!(
        r#"
struct Holder {{ f: File }}
fn make(path: String) -> Holder with reads(FileSystem) panics {{
    match File.open(path) {{
        Ok(fh) => Holder {{ f: fh }},
        Err(_) => panic("open-failed"),
    }}
}}
fn main() with reads(FileSystem) writes(FileSystem) panics {{
    let mut buf: Array[u8, 4] = [0u8; 4];
    let h = make("{path}");
    match h.f.read(mut buf) {{
        Ok(_) => println(f"struct {{buf[0]}}"),
        Err(_) => println("struct-failed"),
    }}
}}
"#
    );
    let out = run_program(&src);
    if let Some(out) = out {
        assert_eq!(out.trim(), "struct 65");
    }
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_e2e_nested_literal_move_source_disarm() {
    // B-2026-08-02-23 leg 1 — the aggregate-literal source disarm was
    // depth-1: `v.push(Outer { inner: Inner { xs: xs } })` inspected only
    // the OUTER literal's immediate fields, and `inner` is a
    // StructLiteral rather than an Identifier, so `xs` was never
    // disarmed and its element body fired twice (once at xs's death,
    // once at the container's) identically on both backends — a
    // parity-equal double that only a fire-COUNT oracle catches.
    // Exactly one fire, at the container's death, is correct.
    let out = run_program(
        r#"
struct Res { id: i64, name: String }
impl Drop for Res {
    fn drop(mut ref self) { println(f"drop {self.id} {self.name}") }
}
struct Inner { xs: Vec[Res] }
struct Outer { inner: Inner, tag: i64 }
fn main() {
    println("a");
    {
        let mut v: Vec[Outer] = Vec.new();
        let mut xs: Vec[Res] = Vec.new();
        xs.push(Res { id: 1, name: f"n{1}" });
        v.push(Outer { inner: Inner { xs: xs }, tag: 5 });
        println(v.len());
    }
    println("end");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "a\n1\ndrop 1 n1\nend");
    }
}

#[test]
fn test_e2e_owned_generic_param_if_branch_return_no_double_free() {
    // B-2026-07-13-1, generic sibling: `fn pick[T: Ord](a: T, b: T) -> T {
    // if a > b { a } else { b } }` monomorphized at `String` must deep-copy
    // per branch in the String monomorph; the `i64` monomorph is a POD
    // no-op (no buffer to copy).
    let out = run_program(
        r#"
fn pick[T: Ord](a: T, b: T) -> T {
    if a > b { a } else { b }
}

fn main() {
    println(pick(f"apple", f"banana"));
    println(pick(3, 7));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "banana\n7");
    }
}

#[test]
fn test_e2e_struct_field_move_out_single_body_fire() {
    // B-2026-08-03-8 (bodies half) — `let x = h.f` moves ONE field out, but
    // the struct's `__karac_dropbodies_*` walk stayed fully armed and fired
    // that field's body a SECOND time. The memory half (this row's first
    // slice) had already stopped the Option case from SEGVing; this stops
    // the duplicate print for all three container field kinds. `struct-field`
    // is the direct-struct control that was correct throughout, and
    // `sibling-survives` checks the mask is per-FIELD: a struct whose field
    // was NOT moved still fires at scope exit.
    //
    // The retraction is the interesting part: the tuple-element fix's
    // `suppress_container_elem_bodies_for_var` matches a
    // `__karac_dropelems_` prefix, which a struct's field-bodies action does
    // not carry — so masking here needed its own prefix-keyed retraction,
    // and a naive reuse silently ADDED an action instead of replacing one.
    let out = run_program(
        r#"
struct Res { id: i64, name: String }
impl Drop for Res {
    fn drop(mut ref self) { println(f"drop {self.id} {self.name}") }
}
struct Ho { o: Option[Res], t: i64 }
struct Hv { v: Vec[Res], t: i64 }
struct Hs { r: Res, t: i64 }
fn main() {
    println("option-field:");
    { let h = Ho { o: Option.Some(Res { id: 1, name: f"a{1}" }), t: 10 }; let x = h.o; println(h.t); }
    println("vec-field:");
    {
        let mut vv: Vec[Res] = Vec.new();
        vv.push(Res { id: 2, name: f"bb{2}" });
        let h = Hv { v: vv, t: 20 };
        let x = h.v;
        println(h.t);
    }
    println("struct-field:");
    { let h = Hs { r: Res { id: 3, name: f"ccc{3}" }, t: 30 }; let x = h.r; println(h.t); }
    println("sibling-survives:");
    {
        let mut w: Vec[Res] = Vec.new();
        w.push(Res { id: 4, name: f"dddd{4}" });
        let h = Hv { v: w, t: 40 };
        println(h.t);
    }
    println("end");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "option-field:\ndrop 1 a1\n10\nvec-field:\ndrop 2 bb2\n20\nstruct-field:\ndrop 3 ccc3\n30\nsibling-survives:\n40\ndrop 4 dddd4\nend");
    }
}

#[test]
fn test_e2e_tuple_elem_move_out_single_body_fire() {
    // B-2026-08-03-3 (bodies half) — `let x = t.N` moves ONE tuple element
    // out. Cap-zeroing already neutralized the source's MEMORY drop, but its
    // `__karac_dropelems_tuple_*` walk stayed fully armed and fired element
    // N's body a SECOND time over the just-zeroed slot: `drop 1 ` with an
    // empty name (the interpreter printed a full duplicate instead — a
    // divergence on top of the double fire). The whole-binding disarm is too
    // coarse here, so the walker is re-emitted with only index N masked —
    // which is why the second element still fires at scope exit.
    let out = run_program(
        r#"
struct Res { id: i64, name: String }
impl Drop for Res {
    fn drop(mut ref self) { println(f"drop {self.id} {self.name}") }
}
fn main() {
    println("struct-elem:");
    {
        let t = (Res { id: 1, name: f"a{1}" }, Res { id: 2, name: f"bb{2}" });
        let x = t.0;
        println(t.1.id);
    }
    println("option-elem:");
    {
        let t = (Option.Some(Res { id: 3, name: f"ccc{3}" }), 30);
        let x = t.0;
        println(t.1);
    }
    println("end");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out.trim(),
            "struct-elem:\ndrop 1 a1\n2\ndrop 2 bb2\n\
                 option-elem:\ndrop 3 ccc3\n30\nend"
        );
    }
}

/// B-2026-08-15-10 — the OUTPUT half of the call-argument defensive copy,
/// for hosts where the ASAN gate skips.
///
/// The memory error and the wrong answer are one defect seen twice: the
/// disarm zeroes the moved field's `cap`, so the reuse becomes a borrow of
/// the buffer the consumer now owns. While the consumer is alive that
/// borrow reads correctly — which is why the `main`-shaped control below
/// passed throughout. It is the CALLEE's scope exit that frees the map and
/// turns the escaping reuse into mojibake. `tests/memory_sanitizer.rs`
/// names the use-after-free; this names the wrong bytes, and it runs
/// everywhere.
#[test]
fn e2e_uam_moved_field_reused_as_call_argument_in_a_callee() {
    let src = r#"
struct Stat { service: String }
struct Entry { service: String }

fn agg(entries: Vec[Entry]) -> Vec[Stat] {
    let mut index: Map[String, usize] = Map.new();
    let mut stats: Vec[Stat] = Vec.new();
    let mut i = 0;
    while i < entries.len() {
        let e = ref entries[i];
        let _ = index.insert(e.service, 0 as usize);
        stats.push(Stat { service: e.service });
        i = i + 1;
    }
    return stats;
}

fn main() {
    let mut es: Vec[Entry] = Vec.new();
    es.push(Entry { service: "alphabetical" });
    es.push(Entry { service: "betamaximum" });
    let out = agg(es);
    println(f"{out[0].service} {out[1].service}");
}
"#;
    assert_eq!(
        run_program(src).as_deref(),
        Some("alphabetical betamaximum\n"),
        "a field moved as a call argument and reused must keep its own bytes"
    );
}

/// B-2026-08-16-7 — a struct FIELD moved TWICE, with a reuse after each
/// move, lost its heap contents on both compiled backends: the second
/// move ran its source-zeroing suppression with NO defensive copy, so
/// the reuse after it read a zeroed Vec — length intact, Strings gone.
///
/// The `UseAfterMove` diagnostic dedups to one witness per binding (the
/// first in source order), and the defensive-copy planner harvested the
/// DIAGNOSTICS, inheriting the dedup. The copy set now carries every
/// reused consume; the warning is unchanged.
///
/// The row's 31-line repro, verbatim — its reduction record shows every
/// ingredient is needed (dropping the `Ed` wrapper, the snapshot build,
/// or the `mut ref` call each makes the bug vanish), so it is kept whole
/// rather than re-reduced. Paired with an interpreter oracle in
/// `tests/interpreter.rs` and a leak/double-free fixture in
/// `tests/memory_sanitizer.rs` — the copy-at-every-move fix moves
/// ownership balance, so the value assertions here are necessary but
/// not sufficient.
#[test]
fn test_e2e_second_field_move_of_one_binding_still_defensively_copies() {
    assert_eq!(
        run_program(
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
fn main() {
    let mut l: Vec[String] = Vec.new();
    let mut a = String.new(); a.push_str("ALPHA");
    l.push(a);
    let mut e = Ed { doc: Doc { lines: l } };
    let mut snapshot: Vec[String] = Vec.new();
    let cur = e.doc;
    for i in 0..cur.lines.len() { snapshot.push(cur.lines[i]); }
    let _inv = apply(mut e.doc, Cmd.Clear(snapshot));
    let after = e.doc;
    println(f"[{render(e.doc)}] lines={after.lines.len()}")
}
"#
        ),
        Some("[ALPHA] lines=1\n".to_string())
    );
}

/// B-2026-09-01-24 — A SCALAR FIELD READ of a live local INSIDE a discarded
/// literal (`k: t.id`) declined the all-fresh gate, so the whole literal
/// registered no owner and every MINTED sibling was leaked.
///
/// `discard_tuple_elem_is_fresh_expr`'s fallback is meant to admit exactly
/// this — "a place / unknown shape: safe only when its type is a scalar
/// primitive" — but reached its slot-type test only through an
/// `Identifier` guard, and none of `infer_arg_elem_te`'s three resolvers
/// handles a `FieldAccess`, so the type came back as the EMPTY path and
/// the scalar test failed. `place_projection_is_scalar` answers from the
/// DECLARED field type instead.
///
/// A scalar read is a COPY, so unlike B-2026-09-01-21's movable-place
/// admission it needs no source retraction and is safe for every consumer
/// of the predicate.
#[test]
fn e2e_a_scalar_projection_field_does_not_decline_a_discarded_literal() {
    const PRELUDE: &str = "struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\"); } }\n\
             struct Inner { n: i64 }\n\
             struct Outer { inner: Inner, m: i64 }\n\
             struct S2 { r: R, s: R, k: i64 }\n";
    let cases: &[(&str, &str, &str)] = &[
        (
            "scalar field read of a live Drop-bearing local, bare statement",
            "fn go() -> i64 { let t = R { id: 7 };\n\
                 S2 { r: R { id: 1 }, s: R { id: 9 }, k: t.id };\n\
                 return 7; }\n\
                 fn take() -> i64 { return go(); }",
            "dR9\ndR1\ndR7\nv=7\n",
        ),
        (
            "same, wildcard `let`",
            "fn go() -> i64 { let t = R { id: 7 };\n\
                 let _ = S2 { r: R { id: 1 }, s: R { id: 9 }, k: t.id };\n\
                 return 7; }\n\
                 fn take() -> i64 { return go(); }",
            "dR9\ndR1\ndR7\nv=7\n",
        ),
        (
            "same, behind a block wrapper",
            "fn go() -> i64 { let t = R { id: 7 };\n\
                 { S2 { r: R { id: 1 }, s: R { id: 9 }, k: t.id } };\n\
                 return 7; }\n\
                 fn take() -> i64 { return go(); }",
            "dR9\ndR1\ndR7\nv=7\n",
        ),
        (
            "DEPTH-1 projection of a non-`Drop` struct — lost BOTH objects before",
            "fn go() -> i64 { let o = Outer { inner: Inner { n: 3 }, m: 4 };\n\
                 S2 { r: R { id: 1 }, s: R { id: 9 }, k: o.m };\n\
                 return 7; }\n\
                 fn take() -> i64 { return go(); }",
            "dR9\ndR1\nv=7\n",
        ),
        (
            "NESTED projection `o.inner.n` — the recursion through the object",
            "fn go() -> i64 { let o = Outer { inner: Inner { n: 3 }, m: 4 };\n\
                 S2 { r: R { id: 1 }, s: R { id: 9 }, k: o.inner.n };\n\
                 return 7; }\n\
                 fn take() -> i64 { return go(); }",
            "dR9\ndR1\nv=7\n",
        ),
        (
            "control: an INDEX of a scalar element, which agreed throughout",
            "fn go() -> i64 { let v = [5, 6, 7];\n\
                 S2 { r: R { id: 1 }, s: R { id: 9 }, k: v[0] };\n\
                 return 7; }\n\
                 fn take() -> i64 { return go(); }",
            "dR9\ndR1\nv=7\n",
        ),
        (
            "control: the same read HOISTED out, correct before and after",
            "fn go() -> i64 { let t = R { id: 7 }; let n = t.id;\n\
                 S2 { r: R { id: 1 }, s: R { id: 9 }, k: n };\n\
                 return 7; }\n\
                 fn take() -> i64 { return go(); }",
            "dR7\ndR9\ndR1\nv=7\n",
        ),
        (
            "control: the local is read AFTER the statement, not inside it",
            "fn go() -> i64 { let t = R { id: 7 };\n\
                 S2 { r: R { id: 1 }, s: R { id: 9 }, k: 5 };\n\
                 return t.id; }\n\
                 fn take() -> i64 { return go(); }",
            // `t` is still live at the statement — the `return` reads it —
            // so the literal's fields die at the discard and `t` at scope
            // exit, in that order. The hoisted control above has the
            // opposite order for the opposite reason: there `t`'s last use
            // is the `let`, so NLL kills it first.
            "dR9\ndR1\ndR7\nv=7\n",
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
            assert_eq!(aot, *want, "{label}: every object dropped, each once");
        }
    }
}

/// B-2026-09-01-18 — A DISCARDED STRUCT LITERAL BEHIND A BLOCK WRAPPER, OR
/// WRITTEN BARE IN STATEMENT POSITION, ran its consumed local's `Drop` body
/// TWICE under `--interp` and once on both compiled backends.
///
/// `suppress_let_rebind_user_drop` retracts the source's cleanup action by
/// matching the literal SYNTACTICALLY at a `let`'s RHS, so it reached
/// `let _ = S { r: t, k: 1 };` — the one spelling that agreed — and nothing
/// else. A wrapper (`let _ = { S { .. } };`, `{ S { .. } };`) or a bare
/// statement (`S { .. };`) all fell out of its `_ => return`.
///
/// The ORACLE is the BOUND `let` (`let w = S { r: t, k: 1 };`), which was
/// correct on all four surfaces throughout and fixes what "once" means
/// here: the literal owns `t`'s value, so `t` must not run its own body.
#[test]
fn e2e_a_discarded_literal_behind_a_wrapper_owns_what_it_consumed() {
    const PRELUDE: &str = "struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\"); } }\n\
             struct S { r: R, k: i64 }\n\
             struct S2 { r: R, s: R, k: i64 }\n";
    let cases: &[(&str, &str, &str)] = &[
        (
            "bare struct literal in statement position",
            "fn go() -> i64 { let t = R { id: 7 };\n\
                 S { r: t, k: 1 };\n\
                 return 7; }\n\
                 fn take() -> i64 { return go(); }",
            "dR7\nv=7\n",
        ),
        (
            "BLOCK wrapper in statement position",
            "fn go() -> i64 { let t = R { id: 7 };\n\
                 { S { r: t, k: 1 } };\n\
                 return 7; }\n\
                 fn take() -> i64 { return go(); }",
            "dR7\nv=7\n",
        ),
        (
            "wildcard `let` over a BLOCK wrapper",
            "fn go() -> i64 { let t = R { id: 7 };\n\
                 let _ = { S { r: t, k: 1 } };\n\
                 return 7; }\n\
                 fn take() -> i64 { return go(); }",
            "dR7\nv=7\n",
        ),
        (
            "TWO sources, bare literal — both bodies, each once",
            "fn go() -> i64 { let t = R { id: 7 }; let u = R { id: 8 };\n\
                 S2 { r: t, s: u, k: 1 };\n\
                 return 7; }\n\
                 fn take() -> i64 { return go(); }",
            "dR8\ndR7\nv=7\n",
        ),
        (
            "TWO sources behind a wrapper",
            "fn go() -> i64 { let t = R { id: 7 }; let u = R { id: 8 };\n\
                 { S2 { r: t, s: u, k: 1 } };\n\
                 return 7; }\n\
                 fn take() -> i64 { return go(); }",
            "dR8\ndR7\nv=7\n",
        ),
        (
            "control: the wildcard `let` DIRECT spelling, which always agreed",
            "fn go() -> i64 { let t = R { id: 7 };\n\
                 let _ = S { r: t, k: 1 };\n\
                 return 7; }\n\
                 fn take() -> i64 { return go(); }",
            "dR7\nv=7\n",
        ),
        (
            "ORACLE: the BOUND `let`, correct on every surface throughout",
            "fn go() -> i64 { let t = R { id: 7 };\n\
                 let w = S { r: t, k: 1 };\n\
                 return w.k + 6; }\n\
                 fn take() -> i64 { return go(); }",
            "dR7\nv=7\n",
        ),
        (
            "control: an ALL-MINTED discarded literal keeps its own body",
            "fn go() -> i64 {\n\
                 S { r: R { id: 7 }, k: 1 };\n\
                 return 7; }\n\
                 fn take() -> i64 { return go(); }",
            "dR7\nv=7\n",
        ),
        (
            "control: all-minted behind a wrapper, both fields still fire",
            "fn go() -> i64 {\n\
                 { S2 { r: R { id: 7 }, s: R { id: 9 }, k: 1 } };\n\
                 return 7; }\n\
                 fn take() -> i64 { return go(); }",
            "dR9\ndR7\nv=7\n",
        ),
        (
            "control: a NON-`Drop` field source is untouched by the retraction",
            "fn go() -> i64 { let t = R { id: 7 }; let n = 5;\n\
                 S { r: t, k: n };\n\
                 return 7; }\n\
                 fn take() -> i64 { return go(); }",
            "dR7\nv=7\n",
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
            assert_eq!(aot, *want, "{label}: one value, one body");
        }
    }
}

/// B-2026-09-01-22 — A DISCARDED STRUCT LITERAL BEHIND **TWO OR MORE**
/// BLOCK WRAPPERS ran its consumed local's `Drop` body ONCE PER WRAPPER on
/// both compiled backends, against one under `--interp`.
///
/// The body count tracked NESTING DEPTH exactly — two wrappers printed
/// `dR7 dR7`, three printed `dR7 dR7 dR7` — which is the shape of the bug
/// and the thing that identifies its site. `compile_block_with_frame` asks
/// `stmt_owns_block_tail` whether the enclosing STATEMENT already owns this
/// block's tail value, and stands down when it does. That guard asked
/// `discarded_owned_literal_tail`; B-2026-09-01-21 had widened the two
/// statement-discard sites to `discarded_movable_literal_tail`, which also
/// admits a field that MOVES a live Drop-bearing local (`S { r: t, k: 1 }`)
/// — and left the guard behind. So the guard answered "no owner" about a
/// value the statement had just taken, and every enclosing block registered
/// one of its own through the arm-discard leg.
///
/// The ONE-wrapper spelling was correct by coincidence, which is why the
/// family's earlier rows never saw this: its tail is the literal itself,
/// and the arm-discard leg declines a bare literal with a non-fresh field
/// for that same freshness reason, so nothing extra registered. Only a tail
/// that is ITSELF a block reaches the registration.
///
/// Both predicates peel block wrappers, so the fix — ask the wider one —
/// makes the guard agree with the statement site at every depth. The
/// ORACLE is the bound `let`, correct on all four surfaces throughout.
#[test]
fn e2e_a_deeply_wrapped_discarded_literal_runs_one_body_per_value() {
    const PRELUDE: &str = "struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\"); } }\n\
             struct S { r: R, k: i64 }\n\
             struct T2 { r: R, s: R }\n\
             fn mk(n: i64) -> R { return R { id: n }; }\n";
    let cases: &[(&str, &str, &str)] = &[
        (
            "TWO wrappers, wildcard `let` — the row's spelling",
            "fn go() -> i64 { let t = R { id: 7 };\n\
                 let _ = { { S { r: t, k: 1 } } };\n\
                 return 7; }\n\
                 fn take() -> i64 { return go(); }",
            "dR7\nv=7\n",
        ),
        (
            "TWO wrappers, bare statement",
            "fn go() -> i64 { let t = R { id: 7 };\n\
                 { { S { r: t, k: 1 } } };\n\
                 return 7; }\n\
                 fn take() -> i64 { return go(); }",
            "dR7\nv=7\n",
        ),
        (
            "THREE wrappers — the count tracked depth, so depth is pinned",
            "fn go() -> i64 { let t = R { id: 7 };\n\
                 let _ = { { { S { r: t, k: 1 } } } };\n\
                 return 7; }\n\
                 fn take() -> i64 { return go(); }",
            "dR7\nv=7\n",
        ),
        (
            "TWO wrappers over the TUPLE leg, which shares the predicate",
            "fn go() -> i64 { let t = R { id: 7 };\n\
                 let _ = { { (t, 20) } };\n\
                 return 7; }\n\
                 fn take() -> i64 { return go(); }",
            "dR7\nv=7\n",
        ),
        (
            "TWO wrappers, a moved source beside a MINTED sibling",
            "fn go() -> i64 { let t = R { id: 7 };\n\
                 let _ = { { T2 { r: t, s: mk(9) } } };\n\
                 return 7; }\n\
                 fn take() -> i64 { return go(); }",
            "dR9\ndR7\nv=7\n",
        ),
        // ── controls: shapes the OLD guard already admitted, which must
        //    not have moved. Each of these was correct at every depth
        //    before the fix, because `discarded_owned_literal_tail` admits
        //    an all-fresh literal and the guard therefore stood down.
        (
            "control: an ALL-MINTED literal, two wrappers",
            "fn go() -> i64 {\n\
                 let _ = { { S { r: R { id: 7 }, k: 1 } } };\n\
                 return 7; }\n\
                 fn take() -> i64 { return go(); }",
            "dR7\nv=7\n",
        ),
        (
            "control: a CALL tail, two wrappers",
            "fn go() -> i64 {\n\
                 let _ = { { mk(9) } };\n\
                 return 7; }\n\
                 fn take() -> i64 { return go(); }",
            "dR9\nv=7\n",
        ),
        (
            "control: the ONE-wrapper spelling, correct by coincidence before",
            "fn go() -> i64 { let t = R { id: 7 };\n\
                 let _ = { S { r: t, k: 1 } };\n\
                 return 7; }\n\
                 fn take() -> i64 { return go(); }",
            "dR7\nv=7\n",
        ),
        (
            "ORACLE: the BOUND `let` behind two wrappers",
            "fn go() -> i64 { let t = R { id: 7 };\n\
                 let w = { { S { r: t, k: 1 } } };\n\
                 return w.k + 6; }\n\
                 fn take() -> i64 { return go(); }",
            "dR7\nv=7\n",
        ),
        (
            "guard: the source is still live under a bound `let`, deeper",
            "fn go() -> i64 { let t = R { id: 7 };\n\
                 let w = { { { T2 { r: t, s: mk(9) } } } };\n\
                 return w.s.id - 2; }\n\
                 fn take() -> i64 { return go(); }",
            "dR9\ndR7\nv=7\n",
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
            assert_eq!(aot, *want, "{label}: one body per value, not per wrapper");
        }
    }
}

/// B-2026-08-31-34 — A HEAP FIELD READ OFF A FRESH TEMP AND CONSUMED BY AN
/// OWNING AGGREGATE DOUBLE FREED ON BOTH COMPILED BACKENDS.
///
/// `let a = mkp(1).a;` has transferred the field out of the temp since
/// B-2026-07-22-2: `consume_freshtemp_field_move` zeroes the accessed
/// field's heap in the staged temp slot, so the temp's struct drop frees
/// only the UNREAD remainder. That call was hooked at the let / assign /
/// return / fn-tail sites and at the ordinary call-argument path — and at
/// none of the AGGREGATE-LITERAL consume sites. So the same read consumed
/// by a struct literal, an array/`Vec` literal, a tuple, or a variant
/// constructor left the temp's cleanup armed while the aggregate took the
/// same pointer, and both freed it.
///
/// SIX SITES, which is the whole fix: the three struct-literal initializer
/// loops, the enum struct-variant initializer loops, the array and
/// vec-prefix literal element loops, the tuple element loop, and the
/// variant-constructor argument loops.
///
/// THE ROW UNDER-DESCRIBED THE AXIS. It recorded a struct literal; probing
/// found `Vec` literals, tuples, `Some(...)`, and a user enum's
/// tuple-variant constructor fail identically, while the ORDINARY call
/// argument `takes(mkp(1).a)` was already correct. The axis is the
/// destination's kind, not the struct literal.
///
/// The interpreter is the oracle throughout — it gets every one of these
/// right, which is what makes the compiled column the wrong side.
#[test]
fn e2e_a_fresh_temp_field_read_consumed_by_an_aggregate_frees_once() {
    const PRELUDE: &str = "fn payload(t: i64) -> String {\n\
             let mut s: String = String.new();\n\
             s.push_str(\"payload-padded-out-well-past-thirty-six-bytes-\");\n\
             s.push_str(f\"{t}\");\n\
             return s;\n\
         }\n\
         struct P { a: String, b: i64 }\n\
         struct V { v: Vec[i64], b: i64 }\n\
         struct W { a: String }\n\
         struct TwoHeap { a: String, c: String }\n\
         enum E { Ea { a: String }, En }\n\
         enum T { Ta(String), Tn }\n\
         fn mkp(n: i64) -> P { return P { a: payload(n), b: n }; }\n\
         fn mk2(n: i64) -> TwoHeap { return TwoHeap { a: payload(n), c: payload(n + 10) }; }\n\
         fn mkv(n: i64) -> V { let mut q: Vec[i64] = Vec.new(); q.push(n); return V { v: q, b: n }; }\n\
         fn takes(s: String) -> i64 { return s.len(); }\n";
    let cases: &[(&str, &str, &str)] = &[
        (
            "struct literal, `String` field",
            "let x: P = P { a: mkp(1).a, b: 1 };\n println(x.a.len());",
            "47\n47\nend\n",
        ),
        (
            "struct literal, `Vec` field",
            "let w: V = V { v: mkv(1).v, b: 1 };\n println(w.v.len());",
            "1\n47\nend\n",
        ),
        (
            "single-field struct literal",
            "let x: W = W { a: mkp(1).a };\n println(x.a.len());",
            "47\n47\nend\n",
        ),
        (
            "`Vec` literal element",
            "let v2: Vec[String] = [mkp(1).a];\n println(v2[0].len());",
            "47\n47\nend\n",
        ),
        (
            "tuple element",
            "let tp: (String, i64) = (mkp(1).a, 1);\n println(tp.0.len());",
            "47\n47\nend\n",
        ),
        (
            "enum STRUCT-variant initializer",
            "let e: E = E.Ea { a: mkp(1).a };\n\
                 match e { E.Ea { a } => println(a.len()), E.En => {} }",
            "47\n47\nend\n",
        ),
        (
            "enum TUPLE-variant constructor argument",
            "let e: T = T.Ta(mkp(1).a);\n\
                 match e { T.Ta(s) => println(s.len()), T.Tn => {} }",
            "47\n47\nend\n",
        ),
        (
            "`Some(...)` constructor argument",
            "let o: Option[String] = Some(mkp(1).a);\n\
                 match o { Some(s) => println(s.len()), None => {} }",
            "47\n47\nend\n",
        ),
        (
            "LEAK-direction control: the temp's OTHER heap field is untouched",
            "let x: W = W { a: mk2(1).a };\n println(x.a.len());",
            "47\n47\nend\n",
        ),
        // CONTROLS — each was already correct and must stay so. Together
        // they are what localizes the defect to the DESTINATION's kind.
        (
            "control: the read bound to a plain local first",
            "let t: P = mkp(1);\n let x: P = P { a: t.a, b: 1 };\n println(x.a.len());",
            "47\n47\nend\n",
        ),
        (
            "control: a DIRECT call as the field initializer",
            "let x: P = P { a: payload(1), b: 1 };\n println(x.a.len());",
            "47\n47\nend\n",
        ),
        (
            "control: the same read at a plain `let`",
            "let a: String = mkp(1).a;\n println(a.len());",
            "47\n47\nend\n",
        ),
        (
            "control: the same read as an ORDINARY call argument",
            "println(takes(mkp(1).a));",
            "47\n47\nend\n",
        ),
    ];
    for (label, stmts, want) in cases {
        let src = format!(
            "{PRELUDE}fn main() {{\n    {stmts}\n    let extra: String = payload(9);\n    \
                 println(extra.len());\n    println(\"end\");\n}}\n"
        );
        let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(&src);
        assert!(
            interp_errs.is_empty(),
            "{label}: interpreter errored: {interp_errs:?}"
        );
        assert_eq!(interp_out.join(""), *want, "{label}: interpreter");
        if let Some(aot) = run_program(&src) {
            assert_eq!(
                aot, *want,
                "{label}: the temp and the aggregate must not both free the field"
            );
        }
    }
}

/// B-2026-09-02-34 (B-2026-09-02-27 REACH) — TWO SCRUTINEE SHAPES THE FIRST FIX LEFT
/// DOUBLE-FREEING.
///
/// `e49aa9e` established the mechanism: a bare-tuple arm binding is a
/// bit-copy VIEW of the tuple's element (B-2026-09-02-23 made the tuple's
/// own `__karac_drop_tuple_*` the single owner), so `let n = r.name`
/// cap-zeroes storage no drop reads while the tuple's walk still frees the
/// buffer `n` now owns. `bare_tuple_elem_slots` records the element's real
/// home and the move-out site zeroes there too.
///
/// It recorded that home only for a FLAT tuple pattern over an IDENTIFIER
/// scrutinee. Two shapes therefore stayed broken, and this fixture is what
/// pins them — each measured aborting on `e49aa9e` itself:
///
///   `match w.t { (r, k) => … }`      a PROJECTION scrutinee — aborts on
///                                    jit, `-O0`, `-O2` and no-auto-par
///   `match t { ((r, j), k) => … }`   a NESTED bare tuple — aborts on jit
///                                    and `-O0`; `-O2` folds it away
///
/// Both are addressable by the same mechanism, so the widening is to the
/// RECORDER, not to the model: `record_bare_tuple_elem_sources` resolves
/// the scrutinee through `field_chain_place_ptr` +
/// `place_chain_aggregate_llvm_type` — the pair every other place-rooted
/// suppression already uses — and recurses through bare-tuple nesting.
/// That also picks up an owned `self`, a tuple index and a `vec[i]`
/// element for free, since those are arms of the same resolver.
///
/// `field_chain_place_ptr` declines a `ref` root, which is the one
/// deliberate exclusion and is checked below: a borrowed source's owner is
/// the caller, so the callee must not write into it.
///
/// The `let (r, k) = t;` spelling was correct before either fix and stays
/// so: `finish_place_source_tuple_destructure` transfers the element
/// outright there, so the binding IS the owner and its own cap-zero is the
/// whole story. That control is what puts the axis on the match-arm
/// binding rather than on tuple destructuring.
///
/// EVERY FIELD IS A REAL HEAP BUFFER (`pad` → 45 bytes), and `tag` is the
/// sibling the move does NOT take, read through at every cell: an
/// over-broad suppression that neutralized the whole element instead of the
/// one field would leak `tag` rather than abort, and the ASAN twin
/// (`asan_bare_tuple_elem_field_move_out_frees_exactly_once`) is what
/// catches that direction.
#[test]
fn e2e_bare_tuple_elem_field_move_out_frees_once() {
    const PRELUDE: &str = "fn pad(t: i64) -> String {\n\
             let mut s: String = String.new();\n\
             s.push_str(\"payload-padded-out-well-past-thirty-six-byte\");\n\
             s.push_str(f\"{t}\");\n\
             return s;\n\
         }\n\
         struct H { id: i64, xs: Vec[i64], tag: String, name: String }\n\
         fn mk(id: i64) -> H {\n\
             let mut v: Vec[i64] = Vec.new();\n\
             v.push(id);\n\
             return H { id: id, xs: v, tag: pad(id), name: pad(id) };\n\
         }\n\
         struct W { t: (H, i64) }\n\
         fn take(t: (H, i64)) {\n\
             match t { (r, k) => { let n = r.name; println(f\"p{n.len()}:{r.tag.len()}:{r.xs.len()}:{k}\"); } }\n\
         }\n\
         fn nested(t: ((H, i64), i64)) {\n\
             match t { ((r, j), k) => { let n = r.name; println(f\"q{n.len()}:{r.tag.len()}:{j}:{k}\"); } }\n\
         }\n\
         fn refscrut(t: ref (H, i64)) {\n\
             match t { (r, k) => { println(f\"s{r.name.len()}:{k}\"); } }\n\
         }\n";
    let cases: &[(&str, &str, &str)] = &[
            (
                "control: a tuple PARAM's element (covered by `e49aa9e`)",
                "take((mk(1), 0));",
                "p45:45:1:0\nafter\n",
            ),
            (
                "control: a tuple LOCAL's element (covered by `e49aa9e`)",
                "let t = (mk(2), 5);\n\
                 match t { (r, k) => { let n = r.name; println(f\"l{n.len()}:{r.tag.len()}:{k}\"); } }",
                "l45:45:5\nafter\n",
            ),
            (
                "THE GAP: a PROJECTION scrutinee (`match w.t`)",
                "let w = W { t: (mk(3), 9) };\n\
                 match w.t { (r, k) => { let n = r.name; println(f\"w{n.len()}:{r.tag.len()}:{k}\"); } }",
                "w45:45:9\nafter\n",
            ),
            (
                // `-O2` folds this one away, so it is the shape most able to
                // look fixed on a default build while still aborting under the
                // JIT and at `-O0`.
                "THE GAP: a NESTED bare tuple (`((r, j), k)`)",
                "nested(((mk(4), 7), 8));",
                "q45:45:7:8\nafter\n",
            ),
            (
                // BOTH heap fields moved out of one element: the mirror has to
                // be per-field and cumulative, not a one-shot.
                "both heap fields moved out of the same element",
                "let t = (mk(5), 1);\n\
                 match t { (r, k) => { let n = r.name; let g = r.tag; println(f\"b{n.len()}:{g.len()}:{k}\"); } }",
                "b45:45:1\nafter\n",
            ),
            (
                // The control that puts the axis on the MATCH arm: this
                // spelling transfers the element to the binding outright and
                // was correct on all four surfaces before the fix.
                "control: the `let (r, k) = t;` destructure spelling",
                "let t = (mk(6), 2);\n\
                 let (r, k) = t;\n\
                 let n = r.name;\n\
                 println(f\"d{n.len()}:{r.tag.len()}:{k}\");",
                "d45:45:2\nafter\n",
            ),
            (
                // No move at all: the tuple owns and frees every field, which
                // is the state B-2026-09-02-23 established. Pins that the
                // mirror fires only for a field actually moved out.
                "control: an arm that only READS the element's fields",
                "let t = (mk(7), 3);\n\
                 match t { (r, k) => { println(f\"n{r.name.len()}:{r.tag.len()}:{k}\"); } }",
                "n45:45:3\nafter\n",
            ),
            (
                // A `ref` tuple param: `field_chain_place_ptr` bails on a
                // borrowed root, so nothing is recorded and the callee never
                // writes into the caller's storage — the caller reads the
                // field back afterwards to prove it.
                "control: a `ref` tuple scrutinee is never written into",
                "let t = (mk(8), 4);\n\
                 refscrut(t);\n\
                 println(f\"after8:{t.0.name.len()}\");",
                "s45:4\nafter8:45\nafter\n",
            ),
            (
                // Two struct elements, only one moved from: the sibling
                // element keeps both of its buffers and the tuple frees them.
                "control: a sibling element the move never touched",
                "let t = (mk(9), mk(10));\n\
                 match t { (a, b) => { let n = a.name; println(f\"e{n.len()}:{b.name.len()}:{b.tag.len()}\"); } }",
                "e45:46:46\nafter\n",
            ),
            (
                // Looped, so a mirror recorded once and gone stale on the
                // second iteration shows up.
                "the same arm inside a loop",
                "let mut i = 0;\n\
                 while i < 3 {\n\
                     let t = (mk(i), i);\n\
                     match t { (r, k) => { let n = r.name; println(f\"c{n.len()}:{r.tag.len()}:{k}\"); } }\n\
                     i = i + 1;\n\
                 }",
                "c45:45:0\nc45:45:1\nc45:45:2\nafter\n",
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

/// The owned-locals path through that same `return <unit call>;` arm. The
/// new arm sits after `emit_scope_cleanup` has already run, so it must NOT
/// clean up again — a second walk would double free every owned local.
/// Drop order and the `Drop` body firing exactly once are what pin that;
/// the LSan half is `tests/memory_sanitizer.rs`'s business, but a
/// double free shows up here as a crash or a repeated `D5`.
#[test]
fn test_e2e_return_of_unit_call_with_owned_locals() {
    let src = "struct T { tag: i64, name: String }\n\
             impl Drop for T { fn drop(mut ref self) { println(f\"D{self.tag}\"); } }\n\
             fn sink() -> () { println(\"sink\"); }\n\
             fn g() -> () {\n\
                 let s: String = \"owned_payload_long_enough_to_heap_allocate\".to_string();\n\
                 let t: T = T { tag: 5i64, name: \"tracked_payload_long_enough_here\".to_string() };\n\
                 println(s.len() + t.name.len());\n\
                 return sink();\n\
             }\n\
             fn main() { println(\"before\"); g(); println(\"after\"); }\n";
    assert_eq!(
        run_program(src).expect("owned locals + `return <unit call>;` should build and run"),
        // 42 + 32 — the two literals' lengths, cross-checked against the
        // interpreter rather than transcribed from codegen's own output.
        "before\n74\nD5\nsink\nafter\n"
    );
}

/// B-2026-08-28-15 — a heap-carrying element moved out of an owned tuple
/// by `.N` at an ESCAPING position (fn tail, explicit `return`, aggregate
/// literal field) left the frame while the tuple's own scope-exit drop
/// still freed it: `free(): double free detected in tcache 2`, rc 134, on
/// both compiled backends, from a `karac check`-clean program the
/// interpreter runs correctly.
///
/// The move-suppression machinery all existed — `zero_tuple_elem_cap_at`
/// even documents itself as "used at a single-element move-out
/// `let x = t.0`" — but `suppress_tuple_index_move_source` was hooked at
/// the two `let`-statement positions in `stmts.rs` and NOWHERE else. So
/// `let r = p.0; r` was clean while `p.0` was a double free, which is why
/// the row's own repro used the tail spelling and its "the destructure
/// spelling is clean" control read as though the EXTRACTION FORM mattered.
/// It does not: the CONSUMING POSITION does.
///
/// Three peers shared the hole, and all are asserted here because each is
/// reached by a different suppressor:
///   * `suppress_tuple_index_move_source` — no escaping hook at all.
///   * `suppress_place_field_struct_move_source` (`p.0.name`, a field
///     INSIDE the element) — likewise `let`-only, all four of its hooks.
///   * `suppress_array_elem_move_source` (`a[0]`) — wired at both return
///     positions but NOT at the aggregate-literal field, so
///     `H { r: a[0] }` double-freed while `return a[0]` was clean.
///
/// Every non-control row aborted (rc 134, so `run_program` returns `None`)
/// before the fix. The `control-*` rows were already correct and guard the
/// REVERSE failure: a suppression that fires where the consumer does not
/// take ownership converts this double free into a leak, which no exit
/// code would reveal.
#[test]
fn e2e_tuple_elem_moved_out_at_an_escaping_position_is_not_double_freed() {
    const DECL: &str = "struct R { id: i64, name: String }\n";
    let cases: &[(&str, &str, &str)] = &[
            // The row's own repro: fn tail `p.0` off an owned tuple PARAM.
            (
                "tail-param",
                "fn take(p: (R, i64)) -> R { p.0 }\n\
                 fn main() { let x = take((R { id: 41, name: f\"n{41}\" }, 1)); println(f\"{x.id} {x.name}\") }",
                "41 n41\n",
            ),
            // Same tail, tuple LOCAL rather than a param — so the defect is
            // not about parameter ownership at all.
            (
                "tail-local",
                "fn mk() -> R { let p = (R { id: 41, name: f\"n{41}\" }, 1); p.0 }\n\
                 fn main() { let x = mk(); println(f\"{x.id} {x.name}\") }",
                "41 n41\n",
            ),
            (
                "explicit-return",
                "fn take(p: (R, i64)) -> R { return p.0; }\n\
                 fn main() { let x = take((R { id: 41, name: f\"n{41}\" }, 1)); println(f\"{x.id} {x.name}\") }",
                "41 n41\n",
            ),
            // Early `return` inside a branch — a different lowering path from
            // the last-statement return above.
            (
                "early-return",
                "fn take(p: (R, i64), f: bool) -> R { if f { return p.0; } p.0 }\n\
                 fn main() { let x = take((R { id: 41, name: f\"n{41}\" }, 1), true); println(f\"{x.id} {x.name}\") }",
                "41 n41\n",
            ),
            (
                "struct-literal-field",
                "struct H { r: R }\n\
                 fn take(p: (R, i64)) -> H { H { r: p.0 } }\n\
                 fn main() { let x = take((R { id: 41, name: f\"n{41}\" }, 1)); println(f\"{x.r.id} {x.r.name}\") }",
                "41 n41\n",
            ),
            // The tuple lives as a struct FIELD and the move is off `self`.
            (
                "method-self",
                "struct B { p: (R, i64) }\n\
                 impl B { fn take(self) -> R { self.p.0 } }\n\
                 fn main() { let b = B { p: (R { id: 41, name: f\"n{41}\" }, 1) }; let x = b.take(); println(f\"{x.id} {x.name}\") }",
                "41 n41\n",
            ),
            // Element ONE, so the fix is not index-0 special-casing.
            (
                "second-element",
                "fn take(p: (i64, R)) -> R { p.1 }\n\
                 fn main() { let x = take((1, R { id: 41, name: f\"n{41}\" })); println(f\"{x.id} {x.name}\") }",
                "41 n41\n",
            ),
            // A `Vec` payload rather than a `String`. This row is also the
            // run-vs-build witness: pre-fix it aborted under LLJIT while the
            // -O2 AOT build happened to survive, so a `karac build`-only check
            // would have called this shape clean.
            (
                "vec-payload",
                "struct V { id: i64, xs: Vec[i64] }\n\
                 fn take(p: (V, i64)) -> V { p.0 }\n\
                 fn main() { let x = take((V { id: 41, xs: [1, 2] }, 1)); println(f\"{x.id} {x.xs.len()}\") }",
                "41 2\n",
            ),
            // A field INSIDE the element — the deeper-place peer, reached by
            // `suppress_place_field_struct_move_source`.
            (
                "nested-field-return",
                "fn take(p: (R, i64)) -> String { p.0.name }\n\
                 fn main() { let a = take((R { id: 41, name: f\"n{41}\" }, 1)); println(f\"{a}\") }",
                "n41\n",
            ),
            (
                "nested-field-literal",
                "struct H { s: String }\n\
                 fn take(p: (R, i64)) -> H { H { s: p.0.name } }\n\
                 fn main() { let x = take((R { id: 41, name: f\"n{41}\" }, 1)); println(f\"{x.s}\") }",
                "n41\n",
            ),
            // The ARRAY peer at the literal position: `return a[0]` was
            // already suppressed, `H { r: a[0] }` was not.
            (
                "array-elem-literal",
                "struct H { r: R }\n\
                 fn take(a: Array[R, 2]) -> H { H { r: a[0] } }\n\
                 fn main() { let x = take([R { id: 41, name: f\"n{41}\" }, R { id: 9, name: f\"m{9}\" }]); println(f\"{x.r.id} {x.r.name}\") }",
                "41 n41\n",
            ),
            // ---- controls: already correct, and must STAY correct ----
            // The row's own "the destructure spelling is clean" control.
            (
                "control-destructure",
                "fn take(p: (R, i64)) -> R { let (r, n) = p; r }\n\
                 fn main() { let x = take((R { id: 41, name: f\"n{41}\" }, 1)); println(f\"{x.id} {x.name}\") }",
                "41 n41\n",
            ),
            // A `ref` tuple param: the callee does NOT own the storage, so
            // zeroing here would strand the CALLER's buffer. The second line
            // reads that field back through the caller's binding — which is
            // what an over-firing suppression would print empty.
            (
                "control-ref-param",
                "fn peek(p: ref (R, i64)) -> i64 { p.0.id }\n\
                 fn main() { let p = (R { id: 41, name: f\"n{41}\" }, 1); println(f\"{peek(p)}\"); println(f\"{p.0.name}\") }",
                "41\nn41\n",
            ),
            // The source tuple is READ AGAIN after the element moved out.
            (
                "control-reuse-after-move",
                "fn main() { let p = (R { id: 41, name: f\"n{41}\" }, 1); let a = p.0; println(f\"{a.name}\"); println(f\"{p.1}\") }",
                "n41\n1\n",
            ),
            // The `let` spelling the pre-fix compiler already handled — the
            // one position that WAS covered, kept as a fixed point.
            (
                "control-let-binding",
                "fn take(p: (R, i64)) -> i64 { let r = p.0; r.id }\n\
                 fn main() { let a = take((R { id: 41, name: f\"n{41}\" }, 1)); println(f\"{a}\") }",
                "41\n",
            ),
            // A struct param's field projection — the row's PARAM SHAPE
            // control, isolating this to tuple/array element sources.
            (
                "control-struct-param",
                "struct W { r: R, n: i64 }\n\
                 fn take(w: W) -> R { w.r }\n\
                 fn main() { let x = take(W { r: R { id: 41, name: f\"n{41}\" }, n: 1 }); println(f\"{x.id} {x.name}\") }",
                "41 n41\n",
            ),
            // A scalar-only element: no heap, so nothing to double free.
            (
                "control-scalar-element",
                "struct S { id: i64 }\n\
                 fn take(p: (S, i64)) -> S { p.0 }\n\
                 fn main() { let x = take((S { id: 41 }, 1)); println(f\"{x.id}\") }",
                "41\n",
            ),
            // A BARE heap element rather than a struct that carries heap.
            (
                "control-bare-string-element",
                "fn take(p: (String, i64)) -> String { p.0 }\n\
                 fn main() { let x = take((f\"n{41}\", 1)); println(f\"{x}\") }",
                "n41\n",
            ),
        ];
    for (label, body, want) in cases {
        let src = format!("{DECL}{body}\n");
        assert_eq!(
            run_program(&src).as_deref(),
            Some(*want),
            "[{label}] wrong output — or `None`, which for this fixture means \
                 the binary ABORTED, the pre-fix double free"
        );
    }
}

/// B-2026-08-28-42 — a heap read off an element of a container held in a
/// struct FIELD (`h.xs[0].name`, `self.xs[0].name`) was never cloned, so it
/// handed back a shallow alias of the element's buffer and every owning
/// destination freed it alongside the container: `free(): double free
/// detected in tcache 2`, rc 134, on both compiled backends, from a
/// `karac check`-clean program the interpreter answers correctly.
///
/// ONE GATE, TWO CLONERS. `clone_vec_elem_heap_field_read` and its tuple-hop
/// sibling `clone_vec_elem_tuple_index_read` both asked "does this container
/// own or lend its elements" of three per-VARIABLE registries, which a
/// struct field is absent from by construction. Resolution was never the
/// gap — `vec_index_elem_type_expr` has had a FieldAccess arm (`self`,
/// generics, `Array`) for a while, and the bound container name was used
/// nowhere else in either function — so only the container-SHAPE test
/// assumed a bare name. Both now ask the same question of the FIELD's
/// declared type, which is why the tuple-hop rows are here beside the
/// field-hop ones.
///
/// EVERY ASSERTION READS THE SOURCE BACK. The second line of each program
/// re-reads the container's element after the first read consumed one, so a
/// row fails loudly in BOTH directions: pre-fix the process aborted
/// (`run_program` → `None`), and a fix that made the read a MOVE instead of
/// a copy would print an empty field here rather than the value. That
/// distinction is the whole reason this is a clone and not a cap-zero —
/// B-2026-08-12-27 measured the move model turning these double frees into
/// silent use-after-frees.
///
/// The `control-*` rows were already correct. `control-non-consuming` is
/// the leak direction: the fix ADDS a clone, and a clone no destination
/// takes over leaks rather than aborting, which no exit code would show —
/// the ASAN twin (`asan_field_rooted_container_element_read_is_cloned`)
/// carries that half under LSan.
/// B-2026-08-29-11, compiled twin — the ORACLE half of the interpreter
/// fixture of the same name.
///
/// Every case here was already correct on all three compiled backends and
/// is pinned so it stays that way: the whole defect was interpreter-side
/// (frame-shared moved-out marks), so this file's job is to keep the target
/// from moving while that is repaired. `escaping-caller-renamed` is the one
/// worth reading — the compiled answer is one body regardless of what the
/// caller spells its binding, which is what made the interpreter's
/// name-dependent answer a divergence rather than a convention.
#[test]
fn e2e_method_frame_marks_do_not_leak_across_frames() {
    const DROPPER: &str = "struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id}\") } }\n";
    let cases: &[(&str, &str, &str)] = &[
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
    ];
    for (label, body, want) in cases {
        assert_eq!(
            run_program(&format!("{DROPPER}{body}")).as_deref(),
            Some(*want),
            "[{label}]"
        );
    }

    const RES: &str = "struct Res { id: i64, name: String }\n\
             impl Drop for Res { fn drop(mut ref self) { println(f\"drop {self.id} {self.name}\") } }\n";
    let res_cases: &[(&str, &str, &str)] = &[
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
    ];
    for (label, body, want) in res_cases {
        assert_eq!(
            run_program(&format!("{RES}{body}")).as_deref(),
            Some(*want),
            "[{label}]"
        );
    }
}

/// B-2026-09-06-1 — the compiled half of the A/B pin for a DISCARDED
/// generic call's moved-in argument (`let g = mk(1); passG(g);` over
/// `fn passG[T](x: T) -> T`, and the generic-method spellings). This
/// side was right throughout — the row is an interpreter-only loss —
/// and the pin exists so the two backends are asserted from both sides.
/// Interpreter twin:
/// `test_discarded_generic_call_runs_the_moved_in_argument_body`.
#[test]
fn e2e_discarded_generic_call_runs_the_moved_in_argument_body() {
    let Some(out) = run_program(
        r#"struct R { id: i64, names: Vec[String] }
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
"#,
    ) else {
        return;
    };
    assert_eq!(out, "dR1\none\ndR2\ntwo\ndR3\nthree\ndR4\nfour\ndR5\nfive\ndR6\nsix\ndR7\nseven\ndR8\neight\ndR9\nnine\ndR10\nten\ndR11\neleven\nw12\ndR12\ntwelve\nend\n");
}

/// B-2026-09-06-46 — the same partial destructure, over a source ONE of
/// whose fields was moved out first (`let x: R = s.a;`). Every compiled
/// backend lost the BOUND leaf's body outright: `mid dR2 dR1` for the
/// rest spelling where `mid dR3 dR2 dR1` is due, `b`'s body running
/// nowhere, on jit / aot / `KARAC_AUTO_PAR=0` alike.
///
/// The move-out disarm was a whole-walker DELETE for a struct with no
/// `impl Drop` of its own, so `s` stopped running every field's body and
/// not just the moved one; the destructure then asked
/// `var_owns_struct_field_bodies` whether the source still held the walk
/// before handing `b`'s body to the leaf and got no. It masks the moved
/// field instead now.
///
/// Only the BODY was lost — the memory half was balanced before the fix
/// and after it (valgrind: 20 allocs / 20 frees, 0 errors), which is why
/// no sanitizer caught this and the assert has to count bodies.
///
/// Exact twin of `tests/interpreter.rs`'s
/// `test_let_destructure_discard_skips_a_moved_out_field` — same program,
/// same string, so the two backends are pinned to each other. ASAN twin:
/// `asan_partial_destructure_over_a_moved_out_source_runs_each_body_once`.
#[test]
fn e2e_partial_destructure_over_a_moved_out_source_runs_each_body_once() {
    let Some(out) = run_program(
        r#"struct R { id: i64, name: String }
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
"#,
    ) else {
        return;
    };
    assert_eq!(
            out,
            "mid\ndR3\ndR2\ndR1\nv=5\none\nmid\ndR6\ndR5\ndR4\nv=11\ntwo\ndR8\ndR9\nmid\ndR7\nv=1\nthree\nend\n"
        );
}
