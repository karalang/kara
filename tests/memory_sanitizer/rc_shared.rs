//! shared/RC values, boxing, heap payloads -- fixtures for `tests/memory_sanitizer.rs`.
//!
//! Split out of `tests/memory_sanitizer.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test memory_sanitizer` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test memory_sanitizer rc_shared::
//!
//! New fixtures about shared/RC values, boxing, heap payloads belong in this file.

use super::*;

/// B-2026-09-06-62 — the MEMORY half, and the half the row is about: the
/// same program under ASAN + LSan, where the pre-fix build aborted on a
/// double free. One owner and one free per object on the temp receiver, the
/// named receiver, the double rebind, the `ref self` control, a
/// copy-supported struct receiver and the free-function twin.
#[test]
fn asan_owned_self_rebind_of_a_shared_field_struct() {
    assert_clean_asan_run(
        "shared struct Inner { v: i64 }\n\
             struct R { id: i64, name: String, inner: Inner }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"  dR{self.id}\") } }\n\
             struct P { id: i64, name: String, xs: Vec[i64] }\n\
             impl Drop for P { fn drop(mut ref self) { println(f\"  dP{self.id}\") } }\n\
             fn mk(i: i64) -> R { return R { id: i, name: f\"h{i}\", inner: Inner { v: i } }; }\n\
             fn mkp(i: i64) -> P { return P { id: i, name: f\"p{i}\", xs: [i] }; }\n\
             impl R {\n\
             \x20   fn take(self) -> i64 { let m = self; return m.id; }\n\
             \x20   fn twice(self) -> i64 { let m = self; let n = m; return n.id; }\n\
             \x20   fn plain(self) -> i64 { return self.id; }\n\
             \x20   fn borrowed(ref self) -> i64 { return self.id; }\n\
             }\n\
             impl P { fn take(self) -> i64 { let m = self; return m.id; } }\n\
             fn top(r: R) -> i64 { let m = r; return m.id; }\n\
             \n\
             fn main() {\n\
             \x20   println(\"temp_receiver\"); println(f\"  v={mk(1).take()}\");\n\
             \x20   println(\"named_receiver\"); let a = mk(2); println(f\"  v={a.take()}\");\n\
             \x20   println(\"twice\"); println(f\"  v={mk(3).twice()}\");\n\
             \x20   println(\"borrowed\"); let b = mk(5); println(f\"  v={b.borrowed()}\");\n\
             \x20   println(\"copyable_struct\"); println(f\"  v={mkp(6).take()}\");\n\
             \x20   println(\"free_function\"); println(f\"  v={top(mk(7))}\");\n\
             \x20   println(\"end\");\n\
             }\n",
        &[
            "temp_receiver",
            "  dR1",
            "  v=1",
            "named_receiver",
            "  dR2",
            "  v=2",
            "twice",
            "  dR3",
            "  v=3",
            "borrowed",
            "  v=5",
            "  dR5",
            "copyable_struct",
            "  dP6",
            "  v=6",
            "free_function",
            "  dR7",
            "  v=7",
            "end",
        ],
        "owned_self_rebind_shared_field",
    );
}

/// B-2026-09-03-14 — the three arms of B-2026-09-02-43's owner mask under ASAN + LSan.
///
/// The transcript is pinned by `e2e_destructure_owner_mask_reaches_the_remaining_arms`;
/// what this case pins is the MEMORY, which that test cannot see.
///
/// IT CANNOT CATCH THE ORIGINAL DEFECTS, AND SAYING SO IS THE POINT. Two of the three
/// were bodies run against a ZEROED husk and the third was a duplicate body over a live
/// value, so the pre-fix binary is ASAN- and LSan-clean; valgrind agreed at `-O0` and
/// `-O2`. What it guards is the FIX, which deletes bodies from a walk and in the
/// `owndrop` case swaps which walker runs at all: masking the type-level `karac_drop_<T>`
/// wrapper rather than a per-binding action. Getting the memory half of that wrong
/// strands every heap field the element owns, and every `Drop` body here reads both
/// `self.tag` and `self.xs.len()`, so freeing too early is a use-after-free rather than
/// a silent leak.
///
/// `wild` is the arm where body and memory must part company most clearly: the body runs
/// at the destructure while the aggregate keeps the memory, so a mask that also moved
/// ownership would leak it. `wildopt` is the arm that must not be masked at all.
#[test]
fn asan_destructure_owner_mask_remaining_arms_keep_the_heap() {
    assert_clean_asan_run(
        r#"
struct R { id: i64, tag: String, xs: Vec[i64] }
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
        &[
            // The harness trims the run's FIRST line; every later line keeps
            // its indentation.
            "wild",
            "dR1/t1/1",
            "  end",
            "wild end",
            "wildidx",
            "dR2/t2/1",
            "  end",
            "wildidx end",
            "wildopt",
            "dR3/t3/1",
            "  end",
            "wildopt end",
            "nested",
            "  b4/t4",
            "dR4/t4/1",
            "  end",
            "nested end",
            "owndrop",
            "dHd5",
            "  b5/t5",
            "dR5/t5/1",
            "  end",
            "owndrop end",
            "plain",
            "  b6/t6",
            "dR6/t6/1",
            "  end",
            "plain end",
            "param",
            "  b7/t7",
            "  end",
            "dR7/t7/1",
            "param end",
            "done",
        ],
        "b43-14-owner-mask-remaining-arms",
    );
}

#[test]
/// B-2026-09-07-50 — the OWNERSHIP half of
/// `e2e_rc_promoted_param_keeps_its_drop_body_and_heap`.
///
/// One omission produced both of that row's symptoms. The `let` site's copy
/// of the RC-fallback boxing names its box after the boxed type and calls
/// `register_rc_fallback_box_drop`; the param loop's copy built an
/// ANONYMOUS `{i64, T}` and registered no value-drop at all, then
/// `continue`d. So the boxed param's user `Drop` body ran nowhere and its
/// heap was never freed — 20 B in 2 blocks at -O0, 12 allocs / 10 frees,
/// the `String` plus the `shared` field's refcount block.
///
/// The anonymous type could not have carried the drop fn either:
/// `rc_fallback_box_drop_fns` is keyed on the box type, and B-2026-09-07-18
/// records what a shared box type does — two same-shaped values get one
/// drop fn.
///
/// `pf` is the cell that separates the two halves: `P` has no `Drop` of its
/// own but does own a `String`, so it never had a body to lose and leaked
/// anyway. A body-only check would have called it clean.
fn asan_rc_promoted_param_owns_its_body_and_heap() {
    assert_clean_asan_run_min_allocs(
        r#"
shared struct Inner { v: i64 }
struct R { id: i64, name: String, inner: Inner }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}", inner: Inner { v: i } }; }
struct P { id: i64, name: String }
fn mkp(i: i64) -> P { return P { id: i, name: f"p{i}" }; }
struct Box2 { mut xs: Vec[R] }

impl Box2 {
    fn after(mut ref self, r: R, k: bool) { if k { self.xs.push(r); } println(f"s{r.inner.v}"); }
    fn before(mut ref self, r: R, k: bool) { println(f"s{r.inner.v}"); if k { self.xs.push(r); } }
    fn bare(mut ref self, r: R, k: bool) { if k { self.xs.push(r); } }
}
fn ff(v: mut ref Vec[R], r: R, k: bool) { if k { v.push(r); } println(f"s{r.inner.v}"); }
fn pf(v: mut ref Vec[P], p: P, k: bool) { if k { v.push(p); } println(f"s{p.id}"); }

fn main() {
    let mut b = Box2 { xs: Vec.new() };
    b.after(mk(1), false);
    b.before(mk(2), false);
    b.bare(mk(4), false);
    let mut v: Vec[R] = Vec.new();
    ff(mut v, mk(5), false);
    let mut w: Vec[P] = Vec.new();
    pf(mut w, mkp(6), false);
    println("end");
}
"#,
        &["s1", "dR1", "s2", "dR2", "dR4", "s5", "dR5", "s6", "end"],
        "b0907-50-rc-promoted-param-ownership",
        18,
    );
}

#[test]
/// B-2026-09-19-42 — an assignment over a LOCAL binding releases the value
/// it displaces, including that value's `shared` refcount block.
///
/// The THIRD displacement site of B-2026-09-07-20's family. That row
/// repaired the field assign (`h.one = mk(37)`) and the index assign
/// (`v[0] = mk(38)`) by routing them through `displaced_struct_shared_drop`;
/// the plain local reassign (`out = w`) still called
/// `emit_struct_drop_synthesis`, the VALUE drop, which SKIPS `shared`
/// fields by design because a live binding releases through a separate
/// scope-exit channel. A displaced value has no such channel — the
/// binding's scope-exit action reads the slot and finds the NEW occupant —
/// so the overwritten value's block was released by nobody.
///
/// EVERY DISPLACING CELL HERE LEAKED, at `KARAC_OPT_LEVEL=0` under
/// valgrind, on the parent: 90 allocs / 72 frees, `304 bytes in 9 blocks
/// definitely lost` plus 270 indirect, 9 errors from 9 contexts. On the fix
/// the same program is 90 allocs / 90 frees, 0 errors. Nine loss records,
/// one per displacement, in two sizes — seven `62 (32 direct, 30 indirect)`
/// for a `shared struct` box and two `70 (40 direct, 30 indirect)` for the
/// wider `shared enum` one:
///
///   * `a`/`b`/`c` — the same assignment with three RHS PROVENANCES: a call
///     result, a match-arm binding and a named local. All three lost one
///     block, identically, which is what says this is the SITE and not a
///     provenance interaction. B-2026-09-07-20's prose records the plain
///     local rebind as clean; it was clean for a different struct shape and
///     is not a counterexample.
///   * `d` — the `if let` spelling, listed NOT MEASURED on the row.
///   * `e` — an `Option[shared]` field rather than a bare `shared` one,
///     also listed NOT MEASURED.
///   * `f` — TWO successive assignments over a struct whose `shared` field
///     sits among scalars, losing two of the nine blocks. One block per
///     overwrite is what identifies the DISPLACED value rather than the
///     stored one, and it is why this cell carries two assignments.
///   * `i`/`j` — a `shared ENUM` in the field position rather than a
///     `shared struct`, the row's third NOT MEASURED axis, with both RHS
///     spellings. These are the two 40-direct records: a different box
///     layout reached through the same site.
///   * `h` — no `shared` anywhere, two assignments: the guard that the
///     change did not disturb the ordinary struct-reassign path it routes
///     through. Clean before and after.
///   * `k` — a SHADOWING `let` rather than an assignment, the row's fourth
///     NOT MEASURED axis. It is not a displacement at all: the first
///     binding is never overwritten and releases through its own scope
///     exit, so it was already clean on the parent. It is here as the
///     over-reach guard — the fix must not turn a second `let` into a
///     displacement. `h` and `k` together are why the block count is 9
///     rather than 11.
///
/// WHICH VALUE LEAKS IS READ FROM THE INTERIOR, not from the IR: every
/// displaced string is 30 bytes (`out-<n>-` + 24 `o`) and every incoming
/// one is 40 (`pay-<n>-` + 34 `p`), so the `30 indirect` carried by all
/// nine records names the overwritten value. The two lengths are chosen to
/// differ for exactly that reason.
///
/// THE PAYLOAD IS SEEDED AND ITS BYTES ARE READ, and that is load-bearing
/// rather than stylistic. Written with literal strings and `.len()` reads,
/// this same program cleared the harness's vacuity floor at 11 allocations
/// — the optimizer had folded the payloads away, so a green ASAN run over
/// it would have proved nothing. The seed is `env.args().len()`, every
/// string is built from it, and each cell returns `hit(s)`, which reads the
/// buffer's CONTENTS (`s.contains("pppp")`) rather than its length. The
/// floor is 45 against 56 observed at this harness's own opt level and 80
/// at `-O0`; it is set below the lower of the two on purpose, because the
/// two legs compile the same program differently and a floor tuned to the
/// `-O0` count reddens the ordinary leg.
///
/// STDOUT IS BYTE-IDENTICAL ACROSS THE FIX and identical under `--interp`
/// and at `-O2`, so there is no output twin to pair with this: a leak of a
/// value nothing reads again is invisible to every output assertion. At
/// `-O2` the parent is clean too — LLVM folds the payloads away — which is
/// why the measurement above is stated at `-O0` and why this fixture's
/// evidence lives in the ratchet leg rather than the ordinary llvm one.
///
/// AN `impl Drop`-BEARING CELL WAS DELIBERATELY LEFT OUT. It takes a
/// different arm (the user-drop wrapper) and is memory-clean before and
/// after, but its `Drop` body count DIVERGES between backends — `--interp`
/// runs it twice and all three compiled surfaces three times, byte for byte
/// identically before and after this fix, so it is pre-existing and filed
/// separately. Carrying it here would pin that divergence into a fixture
/// that is about something else.
fn asan_local_assignment_releases_the_displaced_shared_field_struct() {
    assert_clean_asan_run_min_allocs(
        r#"
shared struct Sh { s: String }
shared enum Se { V(String), N }
struct Ws { h: Sh }
struct Wo { h: Option[Sh] }
struct Wm { id: i64, h: Sh, n: i64 }
struct Ps { a: String, n: i64 }
struct Wse { h: Se }

enum Es { A(Ws), B }
enum Eo { A(Wo), B }
enum Ese { A(Wse), B }

fn outs(n: i64) -> String { return f"out-{n}-oooooooooooooooooooooooo"; }
fn pays(n: i64) -> String { return f"pay-{n}-pppppppppppppppppppppppppppppppppp"; }

fn mkw(t: String) -> Ws { return Ws { h: Sh { s: t } }; }
fn mkm(i: i64, t: String) -> Wm { return Wm { id: i, h: Sh { s: t }, n: i }; }
fn mkp(n: i64, t: String) -> Ps { return Ps { a: t, n: n }; }

fn hit(s: String) -> i64 { if s.contains(f"pppp") { return 1; } return 0; }

fn c_call(n: i64) -> i64 {
  let mut out: Ws = Ws { h: Sh { s: outs(n) } };
  out = mkw(pays(n));
  let t = out.h;
  return hit(t.s);
}
fn c_arm(b: Es, n: i64) -> i64 {
  let mut out: Ws = Ws { h: Sh { s: outs(n) } };
  match b { Es.A(w) => { out = w; } Es.B => { } }
  let t = out.h;
  return hit(t.s);
}
fn c_local(n: i64) -> i64 {
  let mut out: Ws = Ws { h: Sh { s: outs(n) } };
  let w: Ws = Ws { h: Sh { s: pays(n) } };
  out = w;
  let t = out.h;
  return hit(t.s);
}
fn c_iflet(b: Es, n: i64) -> i64 {
  let mut out: Ws = Ws { h: Sh { s: outs(n) } };
  if let Es.A(w) = b { out = w; }
  let t = out.h;
  return hit(t.s);
}
fn c_opt(b: Eo, n: i64) -> i64 {
  let mut out: Wo = Wo { h: Option.Some(Sh { s: outs(n) }) };
  match b { Eo.A(w) => { out = w; } Eo.B => { } }
  match out.h { Option.Some(x) => { return hit(x.s); } Option.None => { return 0; } }
}
fn c_twice(n: i64) -> i64 {
  let mut r: Wm = mkm(1, outs(n));
  r = mkm(7, outs(n));
  r = mkm(9, pays(n));
  let t = r.h;
  return hit(t.s);
}
fn c_plain(n: i64) -> i64 {
  let mut r: Ps = mkp(1, outs(n));
  r = mkp(7, outs(n));
  r = mkp(9, pays(n));
  let t = r.a;
  return hit(t);
}

fn c_senum_arm(b: Ese, n: i64) -> i64 {
  let mut out: Wse = Wse { h: Se.V(outs(n)) };
  match b { Ese.A(w) => { out = w; } Ese.B => { } }
  match out.h { Se.V(s) => { return hit(s); } Se.N => { return 0; } }
}
fn c_senum_inline(n: i64) -> i64 {
  let mut out: Wse = Wse { h: Se.V(outs(n)) };
  out = Wse { h: Se.V(pays(n)) };
  match out.h { Se.V(s) => { return hit(s); } Se.N => { return 0; } }
}
fn c_shadow(b: Es, n: i64) -> i64 {
  // NOT a displacement: a shadowing `let` leaves the first binding intact, so
  // it must still release through its own scope exit. Deliberately unread --
  // any read would move the field out and remove the action under test.
  let out: Ws = Ws { h: Sh { s: outs(n) } };
  match b { Es.A(w) => { let out2: Ws = w; let t = out2.h; return hit(t.s); } Es.B => { return 0; } }
}

fn main() {
  let n: i64 = env.args().len();
  println(f"a{c_call(n)}");
  println(f"b{c_arm(Es.A(Ws { h: Sh { s: pays(n) } }), n)}");
  println(f"c{c_local(n)}");
  println(f"d{c_iflet(Es.A(Ws { h: Sh { s: pays(n) } }), n)}");
  println(f"e{c_opt(Eo.A(Wo { h: Option.Some(Sh { s: pays(n) }) }), n)}");
  println(f"f{c_twice(n)}");
  println(f"h{c_plain(n)}");
  println(f"i{c_senum_arm(Ese.A(Wse { h: Se.V(pays(n)) }), n)}");
  println(f"j{c_senum_inline(n)}");
  println(f"k{c_shadow(Es.A(Ws { h: Sh { s: pays(n) } }), n)}");
  println("end")
}
"#,
        &[
            "a1", "b1", "c1", "d1", "e1", "f1", "h1", "i1", "j1", "k1", "end",
        ],
        "b0919-42-local-assign-displaced-shared",
        45,
    );
}

#[test]
/// B-2026-09-08-9 — the OWNERSHIP half of
/// `e2e_rc_promoted_deep_chain_field_move_out_in_loop`, and the one leg
/// that actually reported a memory error on the parent.
///
/// A two-hop move-out off an RC-promoted root GEP'd the whole aggregate out
/// of the root's eight-byte box-handle alloca and stored zeros past its
/// end. Under the JIT that landed on a live pointer: `free(): double free
/// detected in tcache 2`. Under AOT it landed on the destination's own
/// slot, which valgrind scored 0 errors at BOTH opt levels — the clobber
/// zeroed the destination's cap, so its buffer was simply never freed
/// rather than freed twice.
///
/// That accident is also what forced B-2026-09-08-6 to gate its
/// destination copy to depth 1: ungated, the copy's buffer was the thing
/// the clobber then orphaned, measuring 15 allocs / 14 frees with 2 bytes
/// lost. With the wild store gone the copy is correct at any depth and the
/// gate is lifted, which is why `c_deep3` (three trips) balances here.
fn asan_rc_promoted_deep_chain_field_move_out_in_loop() {
    assert_clean_asan_run_min_allocs(
        r#"
struct S { id: i64, name: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mks(i: i64) -> S { return S { id: i, name: f"s{i}" }; }

struct In { mut r: S, mut q: S }
struct Ou { mut h: In, mut k: i64 }
struct Bs { mut one: S, mut two: S }

fn c_deep() {
    let mut o = Ou { h: In { r: mks(1), q: mks(2) }, k: 5 };
    let mut i = 0;
    while i < 1 { let x = o.h.r; println(f"t{x.id}"); i = i + 1; }
}
fn c_deep3() {
    let mut o = Ou { h: In { r: mks(3), q: mks(4) }, k: 5 };
    let mut i = 0;
    while i < 3 { let x = o.h.r; i = i + 1; }
    println("u");
}
fn c_flat() {
    let mut g = Bs { one: mks(5), two: mks(6) };
    let mut i = 0;
    while i < 1 { let y = g.one; println(f"v{y.id}"); i = i + 1; }
}
fn c_straight() {
    let mut o = Ou { h: In { r: mks(7), q: mks(8) }, k: 5 };
    let x = o.h.r;
    println(f"w{x.id}");
}
fn main() { c_deep(); c_deep3(); c_flat(); c_straight(); println("end"); }
"#,
        &[
            "t1", "dS1", "dS2", "dS1", "dS3", "dS3", "dS3", "u", "dS4", "dS3", "v5", "dS5", "dS6",
            "dS5", "dS8", "w7", "dS7", "end",
        ],
        "b0908-9-rc-promoted-deep-chain-move-out",
        18,
    );
}

#[test]
/// B-2026-09-08-6 — the OWNERSHIP half of
/// `e2e_rc_promoted_base_field_move_out_in_loop`.
///
/// A field move-out inside a `while` that iterates ONCE makes the consume
/// and the later use dominance-incomparable, so the ownership pass answers
/// with an RC-FALLBACK PROMOTION instead of a `UseAfterMove` — and a
/// promoted binding's alloca holds the `{i64 rc, T}` box HANDLE, not the
/// value. The three destinations already taught to copy off such a root
/// (B-2026-09-07-23 / -29 / -30) all self-gate on the projected value being
/// laid out `{ptr,len,cap}`, so a STRUCT field with heap INTERIOR was
/// handed the box's own buffer and both freed it.
///
/// Parent measurement, `KARAC_OPT_LEVEL=0`, valgrind: `Invalid free() /
/// delete / delete[] / realloc()`, 1 error from 1 context, 24 allocs / 25
/// frees, and `free(): double free detected in tcache 2` with SIGABRT
/// (rc=134) on the JIT and both AOT lanes. After: 24/24 and 20/20 at -O0
/// and -O2, 0 errors.
///
/// `c_both` moves BOTH fields out and `c_trips` runs three iterations —
/// each double-freed on the parent the same way, the second scaling with
/// the trip count. `c_str` keeps the sibling rows' `{ptr,len,cap}` shape
/// passing, and `c_flat` is the straight-line control that never promotes
/// and was correct throughout.
///
/// DELIBERATELY OMITS the two-hop chain (`let x = o.h.r` in a loop). That
/// shape reaches the deep `disarm_struct_field_tuple_elem_bodies_at` route
/// rather than the flat one this fixes, and it is red for its OWN reason
/// both before and after: `t0`/`dS0` at -O0 against `t1`/`dS1` at -O2 —
/// byte-identical on the parent — plus 2 bytes lost in 1 block. Filed
/// separately rather than folded in here, which would make this fixture
/// assert a defect it does not close.
fn asan_rc_promoted_base_field_move_out_in_loop() {
    assert_clean_asan_run_min_allocs(
        r#"
struct S { id: i64, name: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mks(i: i64) -> S { return S { id: i, name: f"s{i}" }; }

struct Bs { mut one: S, mut two: S }
struct Ps { mut a: String, mut b: i64 }

fn c_loop() {
    let mut g = Bs { one: mks(1), two: mks(2) };
    let mut i = 0;
    while i < 1 { let taken = g.one; println(f"t{taken.id}"); i = i + 1; }
}
fn c_trips() {
    let mut g = Bs { one: mks(3), two: mks(4) };
    let mut i = 0;
    while i < 3 { let taken = g.one; i = i + 1; }
    println("u");
}
fn c_both() {
    let mut g = Bs { one: mks(5), two: mks(6) };
    let mut i = 0;
    while i < 1 { let a = g.one; let b = g.two; println(f"v{a.id}{b.id}"); i = i + 1; }
}
fn c_str() {
    let mut p = Ps { a: "payload", b: 1 };
    let mut j = 0;
    while j < 2 { let s = p.a; println(f"L{s.len()}"); j = j + 1; }
}
fn c_flat() {
    let mut h = Bs { one: mks(9), two: mks(10) };
    let straight = h.one;
    println(f"x{straight.id}");
}

fn main() {
    c_loop(); c_trips(); c_both(); c_str(); c_flat();
    println("end");
}
"#,
        &[
            "t1", "dS1", "dS2", "dS1", "dS3", "dS3", "dS3", "u", "dS4", "dS3", "v56", "dS6", "dS5",
            "dS6", "dS5", "L7", "L7", "dS10", "x9", "dS9", "end",
        ],
        "b0908-6-rc-promoted-field-move-out-in-loop",
        20,
    );
}

/// A bare-`shared` field read off a shared TEMPORARY, BOUND to a local,
/// releases the box exactly once (B-2026-08-28-49).
///
/// `load_owned_shared_temp_field` emits a `+1` on the loaded inner pointer
/// so the receiver's recursive drop cannot free it out from under the
/// reader, and documents that `+1` as being taken over by the consumer —
/// "the let-stmt path … takes ownership of the +1". The `Option[shared T]`
/// arm it names does exactly that. The BARE `shared T` arm
/// (B-2026-08-28-14) never taught the let site, so the binding took its OWN
/// ref on top: two increments against one release, and the box was never
/// freed.
///
/// THE ROW THIS CLOSES DESCRIBED TWO SEPARATE RESIDUALS, and they are one
/// defect seen through two different blind spots of the measurement:
///
///   * "the bound spelling leaks at -O2 and is CLEAN at -O0" — the leak is
///     there at both levels. At -O0 the pointer is still sitting in the
///     binding's alloca in a live stack frame at exit, so LSan calls it
///     REACHABLE rather than leaked; -O2 drops the alloca and the same leak
///     becomes visible.
///
///   * "one temp-read binding is clean, a second binding leaks one box, and
///     the count does not scale with the reads" — every temp-read binding
///     leaks its own box. Adding any second shared binding perturbs the
///     frame enough that LSan can see one of them, which is what made the
///     count look flat.
///
/// Proven by re-running the one-binding program under
/// `LSAN_OPTIONS=use_stacks=0`: clean by default, 44 bytes in 2 allocations
/// with stack scanning off. That is why `two-bindings` leads this fixture
/// rather than the row's own one-binding repro — it is the shape the
/// harness's default LSan configuration can actually see. `one-binding` is
/// kept beside it and is honest about asserting less: it fails here only
/// once some later change makes its box unreachable.
///
/// THE THREE CONTROLS ARE WHAT KEEP THE FIX FROM BEING A BLANKET
/// SUPPRESSION, and each was clean before and after:
///   * `plain-bindings` — two ordinary shared locals, no temp read, so no
///     `+1` was ever emitted and the binding inc must stay;
///   * `identifier-receiver` — `let o = make_outer(1); let a = o.inner;`
///     roots at an IDENTIFIER, never takes the read's `+1`, and needs its
///     own inc. Suppressing here instead would be a use-after-free;
///   * `unbound-chain` — B-2026-08-28-20's shape, which consumes the `+1`
///     at the consuming site and must not be double-released.
#[test]
fn asan_bound_shared_temp_field_read_releases_once() {
    const H: &str = "shared struct Node { v: i64, tag: String }\n\
             shared struct Outer { id: i64, inner: Node }\n\
             fn make(k: i64) -> Node { return Node { v: k, tag: f\"tag{k}\" }; }\n\
             fn make_outer(k: i64) -> Outer { return Outer { id: k, inner: make(k) }; }\n";
    for (label, body, want) in [
        // The shape the harness's default LSan can see.
        (
            "two-bindings",
            "fn main() { let a = make_outer(1).inner; println(a.tag);\n\
                 \x20            let c = make(2); println(c.tag); }\n",
            vec!["tag1", "tag2"],
        ),
        // The row's own repro. Asserts less than the rest by construction —
        // see the doc above.
        (
            "one-binding",
            "fn main() { let a = make_outer(1).inner; println(a.tag); }\n",
            vec!["tag1"],
        ),
        // Separate block scopes — one box per binding before the fix, which
        // is what ruled out a scope-exit ordering effect.
        (
            "separate-scopes",
            "fn main() { { let a = make_outer(1).inner; println(a.tag); }\n\
                 \x20            { let b = make_outer(2).inner; println(b.tag); } }\n",
            vec!["tag1", "tag2"],
        ),
        // Three of them, to show the count scales with the reads once the
        // measurement can see them all.
        (
            "three-bindings",
            "fn main() { let a = make_outer(1).inner; let b = make_outer(2).inner;\n\
                 \x20            let c = make_outer(3).inner;\n\
                 \x20            println(a.tag); println(b.tag); println(c.tag); }\n",
            vec!["tag1", "tag2", "tag3"],
        ),
        // Unbounded in a loop, which is why the row was not `wontfix`.
        (
            "loop",
            "fn main() { for i in 0..3 { let a = make_outer(i).inner; println(a.tag); } }\n",
            vec!["tag0", "tag1", "tag2"],
        ),
        // The field READ off the binding is a scalar — the leak is the
        // binding's box, not anything about what was read from it.
        (
            "scalar-field-read",
            "fn main() { let a = make_outer(7).inner; println(f\"{a.v}\");\n\
                 \x20            let c = make(2); println(c.tag); }\n",
            vec!["7", "tag2"],
        ),
        // CONTROL — no temp read anywhere, so no `+1` to take over.
        (
            "plain-bindings",
            "fn main() { let a = make(1); println(a.tag);\n\
                 \x20            let c = make(2); println(c.tag); }\n",
            vec!["tag1", "tag2"],
        ),
        // CONTROL — an IDENTIFIER receiver, which never gets the read's
        // `+1` and must keep its own inc.
        (
            "identifier-receiver",
            "fn main() { let o = make_outer(1); let a = o.inner; println(a.tag);\n\
                 \x20            let c = make(2); println(c.tag); }\n",
            vec!["tag1", "tag2"],
        ),
        // CONTROL — B-2026-08-28-20's unbound chain, which consumes the
        // `+1` at the consuming site.
        (
            "unbound-chain",
            "fn main() { println(make_outer(1).inner.tag);\n\
                 \x20            let c = make(2); println(c.tag); }\n",
            vec!["tag1", "tag2"],
        ),
    ] {
        assert_clean_asan_run(&format!("{H}{body}"), &want, label);
    }
}

/// B-2026-08-21-47 — the `shared struct` column of B-2026-08-21-40's
/// five-shape table, pinned because that row predicted the `-40` fix would
/// NOT reach it and nothing held the answer either way.
///
/// It does reach it. Measured by reverting `e6d0dbe`'s one-line gate
/// (`rhs_is_place_field_move` back to `obj_name.is_none()`) and re-running:
/// both legs below leak 147 bytes (49 x 3) with the gate reverted and are
/// clean with it restored, alongside the plain-struct test above going
/// 392 -> 0 in the same pair of runs. One line, both shapes, both
/// directions.
///
/// So the differing byte counts — 8/call for the plain struct, 3/call here
/// — are two differently-sized buffers going missing through ONE accounting
/// error, not the two separate defects the count difference was read as.
/// The buffer at risk is the one the TARGET (`last`) already held, and the
/// receiver's shape only changes how big it is.
///
/// Leg (b) is the one that names the bug and the reason this is not just
/// leg (a): `mut ref` is INCIDENTAL. Deleting the `mut ref` call entirely
/// leaks the identical 147 bytes, and deleting `last = b.name` instead —
/// keeping the `mut ref` — is CLEAN even pre-fix. A pin built only from the
/// filed `mut ref` repro would therefore have passed against a fix aimed at
/// the wrong path.
///
/// A `shared struct` field must be declared `mut` to be assignable, so the
/// shape differs from the plain-struct twin by more than the keyword.
#[test]
fn asan_shared_struct_field_source_frees_the_targets_old_buffer() {
    // (a) the shape as filed, with `mut ref` — 147 bytes (49 x 3) before.
    assert_clean_asan_run(
        r#"
shared struct Box { mut name: String }
fn free_app(s: mut ref String) { s = s + "!"; }
fn main() {
    let mut i = 0i64;
    let mut last: String = "";
    while i < 50i64 {
        let mut b = Box { name: "n" };
        free_app(mut b.name);
        last = b.name;
        i = i + 1;
    }
    println(last);
}
"#,
        &["n!"],
        "asan_shared_struct_field_source_frees_the_targets_old_buffer/mut_ref",
    );
    // (b) the SAME defect with no `mut ref` at all — also 147 bytes.
    assert_clean_asan_run(
        r#"
shared struct Box { mut name: String }
fn main() {
    let mut i = 0i64;
    let mut last: String = "";
    while i < 50i64 {
        let b = Box { name: "n" + "!" };
        last = b.name;
        i = i + 1;
    }
    println(last);
}
"#,
        &["n!"],
        "asan_shared_struct_field_source_frees_the_targets_old_buffer/plain",
    );
}

/// B-2026-08-21-18 — struct functional update with HEAP fields.
///
/// `P { x: 9, ..base }` expands to `x: 9, s: base.s, v: base.v`, so each
/// heap field is MOVED out of the base. That is the shape the row named as
/// the whole risk of implementing this — "who owns a heap-typed field
/// copied out of the base, whether the base is moved or borrowed and when
/// it drops" — and it is the shape behind a large share of this ledger's
/// double-free and leak rows.
///
/// The desugar inherits its answer from the hand-written `s: base.s`
/// rather than reimplementing it, so this fixture is what proves the two
/// really do agree: the base must not free what it no longer owns, and the
/// new struct must free it exactly once.
///
/// 50 iterations so a per-construction imbalance accumulates rather than
/// hiding in one allocation.
#[test]
fn asan_struct_spread_moves_heap_fields_exactly_once() {
    assert_clean_asan_run(
        r#"
struct P { x: i64, s: String, v: Vec[i64] }
fn main() {
    let mut i = 0i64;
    let mut last: String = "";
    while i < 50i64 {
        let b = P { x: 1, s: "n" + "!", v: vec![7, 8] };
        let p = P { x: 9, ..b };
        last = p.s;
        i = i + 1;
    }
    println(last);
}
"#,
        &["n!"],
        "asan_struct_spread_moves_heap_fields_exactly_once",
    );
}

#[test]
fn asan_nested_scope_heap_shadow_no_leak_or_double_free() {
    // B-2026-07-13-6 cleanup-safety guard: the lexical-scope revert
    // (`restore_var_env`) reverts NAME maps only; heap drops stay keyed by
    // alloca in the cleanup frame, so a heap-typed shadow's inner + outer
    // buffers must each free exactly once. 200 iterations: a nested block
    // and an inner loop each shadow the outer String `s` with a fresh heap
    // buffer; a missed drop leaks and a double-drop aborts — both accumulate
    // well past noise. The outer `s` must survive every inner scope intact.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0i64;
    let mut total: i64 = 0i64;
    while i < 200i64 {
        let s = f"outer{i}";
        {
            let s = f"inner{i}";
            total = total + s.len();
        }
        let mut j: i64 = 0i64;
        while j < 2i64 {
            let s = f"loop{i}";
            total = total + s.len();
            j = j + 1i64;
        }
        total = total + s.len();
        i = i + 1;
    }
    println(total.to_string());
}
"#,
        // Per iter i: len("inner{i}")=5+digits, +2*len("loop{i}")=4+digits,
        // +len("outer{i}")=5+digits. i 0..9 (1 digit): inner=6,loop=5*2=10,
        // outer=6 -> 22; i 10..99 (2): inner=7,loop=6*2=12,outer=7 -> 26;
        // i 100..199 (3): inner=8,loop=7*2=14,outer=8 -> 30.
        // 10*22 + 90*26 + 100*30 = 220+2340+3000 = 5560.
        &["5560"],
        "nested_scope_heap_shadow_no_leak_or_double_free",
    );
}

#[test]
fn asan_return_owned_heap_struct_param_no_leak() {
    // B-2026-07-08-6 (non-generic leg, FIXED) — a fn that returns an owned
    // heap-owning STRUCT param used to leak the arg buffer: the caller's
    // return-passthrough guard suppressed the arg-temp drop assuming the
    // callee FORWARDS the buffer, but a copy-supported heap struct param is
    // ENTRY-COPIED at the callee, so the callee returns an INDEPENDENT copy
    // and the original moved-in buffer was orphaned. `id`/`pick`/`choose`
    // all leaked; a String param (no entry-copy) was already clean.
    // Exercises single-return, two-param-keep-one, and conditional-return.
    assert_clean_asan_run(
        r#"
struct Name { s: String }
fn id(a: Name) -> Name { a }
fn pick(a: Name, b: Name) -> Name { a }
fn choose(a: Name, b: Name, t: bool) -> Name { if t { a } else { b } }
fn main() {
    let mut i: i64 = 0i64;
    while i < 2i64 {
        let x = id(Name { s: f"id-{i}-padding-padding" });
        let y = pick(Name { s: f"pa-{i}-padding-padding" }, Name { s: f"pb-{i}-padding-padding" });
        let z = choose(Name { s: f"ca-{i}-padding-padding" }, Name { s: f"cb-{i}-padding-padding" }, true);
        println(x.s);
        println(y.s);
        println(z.s);
        i = i + 1;
    }
}
"#,
        &[
            "id-0-padding-padding",
            "pa-0-padding-padding",
            "ca-0-padding-padding",
            "id-1-padding-padding",
            "pa-1-padding-padding",
            "ca-1-padding-padding",
        ],
        "asan_return_owned_heap_struct_param_no_leak",
    );
}

#[test]
fn asan_heap_example_no_leak() {
    // examples/heap.kara under ASAN — the generic `Heap[i64]` churns its
    // backing `Vec[i64]` hard (push sift-up, pop sift-down + `Vec.pop`,
    // per-swap index read/assign) across heapsort + a PQ drain, plus a fresh
    // `Heap.new()` per phase and `String`s built for the printed lines. `i64`
    // is POD, so this is a buffer-lifecycle check: every heap's backing Vec,
    // the heapsort output Vec, and the print Strings are freed exactly once —
    // no leak, no double-free — across construct -> churn -> drain -> drop.
    assert_clean_asan_run(
        include_str!("../../examples/heap.kara"),
        &[
            "0 1 2 3 4 5 6 7 8 9",
            "size=6",
            "4 17 23 42 58 99",
            "empty-ok",
        ],
        "asan_heap_example_no_leak",
    );
}

#[test]
fn asan_std_cmp_min_max_clamp_heap_ord_no_leak() {
    // roadmap Phase 8 § std.cmp — `min`/`max`/`clamp` are generic stdlib
    // free fns monomorphized on demand from `ordering.kara`. Over a
    // HEAP-OWNING `Ord` type (a `String`-field struct) the un-returned
    // argument must drop exactly once. B-2026-07-08-6 (generic/mono leg,
    // FIXED): the monomorph now ENTRY-COPIES its owned struct params
    // (`compile_mono_function` → `make_aggregate_param_callee_owned`) and
    // the caller registers the original arg-temp's drop
    // (`compile_generic_call` → `track_inline_owned_aggregate_arg`),
    // bringing the mono path to ownership parity with the non-generic
    // `compile_function`/`compile_call` path. Before the fix `min`'s body
    // (`match a.cmp(b) { Greater => b, _ => a }`) returned one owned param
    // and the OTHER (plus every caller original) leaked. Ground-truthed
    // balanced via a malloc interposer (4 mallocs / 4 frees for
    // `min(Name, Name)`).
    assert_clean_asan_run(
        r#"
struct Name { s: String }
impl PartialEq for Name { fn eq(ref self, other: ref Name) -> bool { self.s == other.s } }
impl Eq for Name {}
impl PartialOrd for Name { fn partial_cmp(ref self, other: ref Name) -> Option[Ordering] { Some(self.s.cmp(other.s)) } }
impl Ord for Name { fn cmp(ref self, other: ref Name) -> Ordering { self.s.cmp(other.s) } }
fn main() {
    let mut i: i64 = 0i64;
    while i < 2i64 {
        let lo = min(Name { s: f"alpha-{i}-padding-padding-padding" }, Name { s: f"beta-{i}-padding-padding-padding" });
        let hi = max(Name { s: f"gamma-{i}-padding-padding-padding" }, Name { s: f"delta-{i}-padding-padding-padding" });
        let cl = clamp(Name { s: f"mid-{i}-padding-padding-padding" }, Name { s: f"aaa-{i}" }, Name { s: f"zzz-{i}" });
        println(lo.s);
        println(hi.s);
        println(cl.s);
        i = i + 1;
    }
}
"#,
        &[
            "alpha-0-padding-padding-padding",
            "gamma-0-padding-padding-padding",
            "mid-0-padding-padding-padding",
            "alpha-1-padding-padding-padding",
            "gamma-1-padding-padding-padding",
            "mid-1-padding-padding-padding",
        ],
        "asan_std_cmp_min_max_clamp_heap_ord_no_leak",
    );
}

#[test]
fn asan_std_mem_swap_replace_heap_no_leak_no_double_free() {
    // roadmap Phase 8 § std.mem — `swap` / `replace` move String buffers
    // through `mut ref` places via raw load/store (no destructor on the
    // value that leaves the place). The memory contract: every buffer is
    // dropped EXACTLY once. `swap(s, t)` relocates two buffers (no alloc,
    // no free); `replace(s, v)` moves `v` in and returns the OLD `s`
    // (moved out, not freed — the caller's `old` binding owns and drops
    // it). LSan flags a leak if the old value's drop is dropped; ASan flags
    // a double-free if the store also freed the overwritten slot. Looped so
    // any per-iteration imbalance accumulates.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0i64;
    while i < 2i64 {
        let mut s = f"aaa-{i}-padding-padding";
        let mut t = f"bbb-{i}-padding-padding";
        swap(mut s, mut t);
        let old = replace(mut s, f"ccc-{i}-padding-padding");
        println(s);
        println(t);
        println(old);
        i = i + 1;
    }
}
"#,
        &[
            "ccc-0-padding-padding",
            "aaa-0-padding-padding",
            "bbb-0-padding-padding",
            "ccc-1-padding-padding",
            "aaa-1-padding-padding",
            "bbb-1-padding-padding",
        ],
        "asan_std_mem_swap_replace_heap_no_leak_no_double_free",
    );
}

#[test]
fn asan_boxed_opt_heap_tuple_consume_no_leak() {
    // B-2026-07-18-15: consuming a BOXED `Option[(String,String)]` payload (a
    // >3-word heap-owning tuple that `coerce_to_payload_words` heap-boxes)
    // must free the reconstructed tuple's inner String buffers at the
    // binding's scope exit. `Vec.pop()` on a `Vec[(String,String)]` is the
    // canonical shared producer (it transfers the element out without
    // cloning), so it stands in for any `-> Option[wide-heap-tuple]` (the
    // SortedMap min/max/floor/ceiling among them). Two consuming shapes:
    //  - whole-tuple binding `Some(kv)` then `kv.0`/`kv.1` — the leak shape
    //    the fix targets (the box drop now runs the tuple's per-element inner
    //    drop; before, box-only-free stranded 2 String buffers per iter);
    //  - per-element destructure `Some((a, b))` — already clean (each leaf
    //    binding owns its field), pinned here so the fix doesn't regress it.
    // Looped so any per-iteration imbalance accumulates for LSan; ASan would
    // flag a double-free if the box drop and a binding drop both fired.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut n = 0;
    while n < 3 {
        let mut v: Vec[(String, String)] = Vec.new();
        v.push((f"key-{n}-padpad", f"val-{n}-padpad"));
        match v.pop() { Some(kv) => println(f"{kv.0}={kv.1}"), None => println("n") }

        let mut w: Vec[(String, String)] = Vec.new();
        w.push((f"k2-{n}-padpad", f"v2-{n}-padpad"));
        match w.pop() { Some((a, b)) => println(f"{a}={b}"), None => println("n") }
        n = n + 1;
    }
}
"#,
        &[
            "key-0-padpad=val-0-padpad",
            "k2-0-padpad=v2-0-padpad",
            "key-1-padpad=val-1-padpad",
            "k2-1-padpad=v2-1-padpad",
            "key-2-padpad=val-2-padpad",
            "k2-2-padpad=v2-2-padpad",
        ],
        "asan_boxed_opt_heap_tuple_consume_no_leak",
    );
}

#[test]
fn asan_std_mem_take_heap_no_leak_no_double_free() {
    // roadmap Phase 8 § std.mem — `take[T: Default](dest: mut ref T) -> T`
    // over a heap-owning type. `take` monomorphizes `replace(dest,
    // T.default())`: the old heap buffer is moved OUT (returned, the
    // caller's `old` binding owns and drops it exactly once) and a fresh
    // `T.default()` (empty String buffer, later dropped when `dest` goes out
    // of scope) is moved IN. The memory contract: no buffer leaks (LSan) and
    // none is freed twice (ASan) — in particular the `T.default()`
    // freshly-allocated empty String and the moved-out old value each drop
    // once. Both the named-struct field String and the bare-String cases are
    // looped so any per-iteration imbalance accumulates.
    assert_clean_asan_run(
        r#"
#[derive(Default)]
struct S { x: i64, name: String }
fn main() {
    let mut i: i64 = 0i64;
    while i < 2i64 {
        let mut a = S { x: i, name: f"held-{i}-padding-padding" };
        let old = take(mut a);
        println(old.name);
        println(f"[{a.name}]");
        let mut s = f"bare-{i}-padding-padding";
        let got = take(mut s);
        println(got);
        println(f"[{s}]");
        i = i + 1;
    }
}
"#,
        &[
            "held-0-padding-padding",
            "[]",
            "bare-0-padding-padding",
            "[]",
            "held-1-padding-padding",
            "[]",
            "bare-1-padding-padding",
            "[]",
        ],
        "asan_std_mem_take_heap_no_leak_no_double_free",
    );
}

#[test]
fn asan_generic_fn_returns_owned_heap_struct_param_no_leak() {
    // B-2026-07-08-6 (generic/mono leg, FIXED) — the non-stdlib peer of
    // the std.cmp test: a USER generic fn that returns an owned heap-owning
    // struct param. Pins the mono-path ownership parity directly (entry-
    // copy in `compile_mono_function` + caller arg-temp drop in
    // `compile_generic_call`) independent of the baked stdlib. `gid`
    // (single param) and `gpick` (keep one of two) both leaked pre-fix: the
    // mono registered no owned-aggregate param drop and the generic call
    // path registered no caller arg-temp cleanup. (A `cmp`-based generic
    // body can't be user-written — the ownership checker treats a generic
    // trait-method value arg as a move — so this uses plain returns.)
    assert_clean_asan_run(
        r#"
struct Name { s: String }
fn gid[T](a: T) -> T { a }
fn gpick[T](a: T, b: T) -> T { a }
fn main() {
    let mut i: i64 = 0i64;
    while i < 2i64 {
        let x = gid(Name { s: f"gid-{i}-padding-padding" });
        let y = gpick(Name { s: f"gpa-{i}-padding-padding" }, Name { s: f"gpb-{i}-padding-padding" });
        println(x.s);
        println(y.s);
        i = i + 1;
    }
}
"#,
        &[
            "gid-0-padding-padding",
            "gpa-0-padding-padding",
            "gid-1-padding-padding",
            "gpa-1-padding-padding",
        ],
        "asan_generic_fn_returns_owned_heap_struct_param_no_leak",
    );
}

#[test]
fn asan_generic_assoc_type_projection_heap_return_no_leak() {
    // A generic fn with an associated-type PROJECTION return
    // (`fn get[C: Container](c: C) -> C.Item`) whose concrete associated
    // type is a HEAP `Vec[i64]`. The projection now lowers to the concrete
    // `{ptr,i64,i64}` (previously it hit the i64 default and failed the LLVM
    // verifier), so the returned Vec's buffer must be owned by the caller
    // and freed exactly once — no leak (the mono must not drop it at its own
    // scope exit) and no double-free. Looped to accumulate any imbalance.
    assert_clean_asan_run(
        r#"
trait Container { type Item; fn make(ref self) -> Self.Item; }
struct VecMaker { base: i64 }
impl Container for VecMaker {
    type Item = Vec[i64];
    fn make(ref self) -> Vec[i64] { [self.base, self.base + 1i64, self.base + 2i64] }
}
fn build[C: Container](c: C) -> C.Item { c.make() }
fn main() {
    let mut i: i64 = 0i64;
    while i < 3i64 {
        let v = build(VecMaker { base: i });
        println(f"{v.len()}");
        i = i + 1;
    }
}
"#,
        &["3", "3", "3"],
        "generic_assoc_type_projection_heap_return_no_leak",
    );
}

#[test]
fn asan_b04_2_chain_adaptor_side_heap_no_leak() {
    // B-2026-07-04-2 sub-part 1 (chain adaptor-carrying side): a `chain`
    // whose side carries its own adaptor (`a.iter().filter(g).chain(b.iter())
    // .collect()`) recursively collects each side and merges into a shared
    // accumulator. Both `Vec[String]` sources survive (freed once) and each
    // merged element is a clone owned once by the result. 30x >=40-byte
    // payloads; both sources re-read.
    assert_clean_asan_run(
            r#"
fn main() {
    let mut round: i64 = 0i64;
    while round < 30i64 {
        let mut a: Vec[String] = Vec[
            "chain-adp-left-alpha-aaaaaaaaaaaaaaaaaaaaaa".to_string(),
            "chain-adp-left-bravo-bbbbbbbbbbbbbbbbbbbbbb".to_string()
        ];
        let mut b: Vec[String] = Vec[
            "chain-adp-right-charlie-cccccccccccccccccccc".to_string(),
            "chain-adp-right-delta-dddddddddddddddddddddd".to_string()
        ];
        let r: Vec[String] = a.iter().filter(|s| s.len() > 0i64).chain(b.iter()).collect();
        println(f"{r.len()} {r[0i64]} {r[3i64]} {a.len()} {b.len()}");
        round = round + 1i64;
    }
}
"#,
            [
                "4 chain-adp-left-alpha-aaaaaaaaaaaaaaaaaaaaaa chain-adp-right-delta-dddddddddddddddddddddd 2 2",
            ]
            .repeat(30)
            .as_slice(),
            "asan_b04_2_chain_adaptor_side_heap_no_leak",
        );
}

#[test]
fn asan_b04_2_zip_adaptor_side_heap_no_leak() {
    // B-2026-07-04-2 sub-part 1 (zip adaptor-carrying side): a `zip` whose
    // side carries its own adaptor (`a.iter().filter(g).zip(b.iter())
    // .collect()`) pre-collects each side to a typed temp and reuses the
    // identity zip. Both `Vec[String]` sources survive; each paired element
    // is a clone owned once by the result; the two side temps are dropped at
    // block exit. 30x >=40-byte payloads; both sources re-read.
    assert_clean_asan_run(
            r#"
fn main() {
    let mut round: i64 = 0i64;
    while round < 30i64 {
        let mut a: Vec[String] = Vec[
            "zip-adp-left-alpha-aaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            "zip-adp-left-bravo-bbbbbbbbbbbbbbbbbbbbbbbb".to_string(),
            "zip-adp-left-charlie-cccccccccccccccccccccc".to_string()
        ];
        let mut b: Vec[String] = Vec[
            "zip-adp-right-xray-xxxxxxxxxxxxxxxxxxxxxxxx".to_string(),
            "zip-adp-right-yankee-yyyyyyyyyyyyyyyyyyyyyy".to_string()
        ];
        let r: Vec[(String, String)] = a.iter().filter(|s| s.len() > 0i64).zip(b.iter()).collect();
        println(f"{r.len()} {r[0i64].0} {r[1i64].1} {a.len()} {b.len()}");
        round = round + 1i64;
    }
}
"#,
            [
                "2 zip-adp-left-alpha-aaaaaaaaaaaaaaaaaaaaaaaa zip-adp-right-yankee-yyyyyyyyyyyyyyyyyyyyyy 3 2",
            ]
            .repeat(30)
            .as_slice(),
            "asan_b04_2_zip_adaptor_side_heap_no_leak",
        );
}

#[test]
fn asan_b04_2_cycle_take_heap_no_leak() {
    // B-2026-07-04-2 sub-part 1 (cycle+take): `v.iter().cycle().take(n)
    // .collect()` repeats the source until n elements. Each element is
    // cloned on push (the source may be read multiple times), so the
    // borrowed source survives and every clone is owned once. 40x heap
    // payloads; source re-read each round.
    assert_clean_asan_run(
            r#"
fn main() {
    let mut round: i64 = 0i64;
    while round < 40i64 {
        let v: Vec[String] = Vec[
            "cycle-take-alpha-aaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            "cycle-take-bravo-bbbbbbbbbbbbbbbbbbbbbbbbbb".to_string()
        ];
        let r: Vec[String] = v.iter().cycle().take(5i64).collect();
        println(f"{r.len()} {r[0i64]} {r[4i64]} {v.len()}");
        round = round + 1i64;
    }
}
"#,
            [
                "5 cycle-take-alpha-aaaaaaaaaaaaaaaaaaaaaaaaaa cycle-take-alpha-aaaaaaaaaaaaaaaaaaaaaaaaaa 2",
            ]
            .repeat(40)
            .as_slice(),
            "asan_b04_2_cycle_take_heap_no_leak",
        );
}

#[test]
fn asan_b04_2_scan_heap_no_leak() {
    // B-2026-07-04-2 sub-part 1 (scan): `v.iter().scan(init, |acc, x|
    // Some((new, out))).collect()` threads a running accumulator and
    // collects each output. Here the output is a heap f-string. The pushed
    // outputs are owned once by the result; the source survives. 40x heap
    // payloads.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut round: i64 = 0i64;
    while round < 40i64 {
        let v: Vec[i64] = Vec[1i64, 2i64, 3i64];
        let r: Vec[String] = v.iter().scan(0i64, |acc, x| Some((acc + x, f"running-sum-payload-{acc}-plus-{x}"))).collect();
        println(f"{r.len()} {r[0i64]} {r[2i64]} {v.len()}");
        round = round + 1i64;
    }
}
"#,
        ["3 running-sum-payload-0-plus-1 running-sum-payload-3-plus-3 3"]
            .repeat(40)
            .as_slice(),
        "asan_b04_2_scan_heap_no_leak",
    );
}

/// B-2026-08-05-32: a struct with a DIRECT `shared` field, bound to a LOCAL
/// and passed BY VALUE, leaked its RC box — one per call.
///
/// `move_declined_copy_struct_arg` (B-2026-07-28-4) retracts the caller's
/// `StructDrop` whenever copy-support declines, so the argument is a true
/// move. That is right for the shape it was written for — a self-referential
/// `struct N { edges: Vec[N] }`, where the callee receives an ALIAS it may
/// store into an owning container and both would free the same buffers.
///
/// Copy-support also declines for a direct `shared` field, and there the
/// move reasoning does not follow: the callee is caller-retains, so it never
/// entry-copies, never rc-INCs and never rc-DECs. The binding's drop is the
/// box's ONLY rc-dec, and retracting it stranded the box.
///
/// The three controls are kept in the program because a too-broad fix
/// double-decs exactly there, and a double rc-dec ABORTS rather than leaks:
/// an `Option[shared]` field (copy-supported, so the callee's entry-copy
/// balances it), a fresh-TEMP arg (B-2026-07-04-9(b) registers its own
/// drop), and a local moved to another local without ever being passed.
#[test]
fn asan_direct_shared_field_struct_local_passed_by_value_rc_balanced() {
    assert_clean_asan_run_min_allocs(
        r#"
shared enum Val { Nothing, Ident(String), Num(i64) }
struct DirH { value: Val }
struct OptH { value: Option[Val] }
fn pay(i: i64) -> String { f"payload-{i}-long-enough-aaaa" }
fn use_dir(h: DirH) -> i64 {
    match h.value {
        Val.Ident(s) => { if s.starts_with("payload") { s.len() + 1i64 } else { s.len() } }
        Val.Num(n) => n,
        Val.Nothing => 0i64,
    }
}
fn use_opt(h: OptH) -> i64 {
    match h.value { Some(_) => 1i64, None => 0i64 }
}
fn main() {
    let base: i64 = env.args().len();
    let mut t = 0i64;
    let mut i = 0i64;
    while i < base + 39i64 {
        // The leak: local, then passed by value.
        let d = DirH { value: Val.Ident(pay(i)) };
        t = t + use_dir(d);
        // Control 1 — Option[shared] field, same local-then-pass shape.
        let o = OptH { value: Some(Val.Ident(pay(i))) };
        t = t + use_opt(o);
        // Control 2 — fresh-temp arg.
        t = t + use_dir(DirH { value: Val.Ident(pay(i)) });
        // Control 3 — local moved to another local, never passed.
        let m = DirH { value: Val.Ident(pay(i)) };
        let _m2 = m;
        i = i + 1;
    }
    println(t);
}
"#,
        &["2260"],
        "direct_shared_field_struct_local_passed_by_value_rc_balanced",
        // 88 allocations at -O2 (the optimizer folds some of the four
        // per-iteration boxes); 328 at -O0. The floor only has to sit well
        // above the 3 an allocation-free run reports.
        50,
    );
}

/// B-2026-07-04-9(b) (FIXED): a struct with a DIRECT `shared` field
/// (`DirH { value: Val }`, `Val` a shared enum) passed as an INLINE
/// fresh-temp arg (`borrow_dir(DirH { value: Val.Ident(..) })`) leaked its
/// RC box. `DirH` is NOT copy-supported (`field_copy_supported` bails on a
/// direct shared field), so the fresh-temp struct-arg cleanup gate — which
/// required `aggregate_param_copy_supported_struct` — registered no
/// caller-temp drop, and the caller-retains param doesn't drop it either. A
/// LOCAL arg (`let d = DirH { .. }; f(d)`) was already covered by
/// `track_struct_var` at the binding site. Fixed by registering the combined
/// drop (`track_struct_var`, a pure rc-dec of the shared field — no buffer
/// copy) for any shared-owning fresh-temp struct, copy-supported or not;
/// such a struct is caller-retains, so the caller temp is its sole owner.
/// Payload ≥36 bytes so LSan sees the leaked box; a double rc-dec would abort
/// under ASAN. Exercises the fresh-temp direct-shared arg across a loop.
/// Run: `scripts/lsan-local.sh "b04_9b_direct_shared_freshtemp"`.
#[test]
fn asan_b04_9b_direct_shared_freshtemp_struct_arg_no_leak() {
    assert_clean_asan_run(
        r#"
shared enum Val { Nothing, Ident(String), Num(i64) }
struct DirH { value: Val }

fn borrow_dir(h: DirH) -> i64 {
    let mut r = 0;
    match h.value { Val.Ident(_) => { r = 1; } _ => {} }
    r
}

fn main() {
    let mut total = 0;
    let mut i = 0;
    while i < 5 {
        // INLINE fresh-temp arg — the leaking shape (no `let` binding).
        total = total + borrow_dir(DirH {
            value: Val.Ident("b049b_direct_shared_freshtemp_payload_omega_ffff".to_string()),
        });
        i = i + 1;
    }
    println(total);
}
"#,
        &["5"],
        "b04_9b_direct_shared_freshtemp",
    );
}

/// B-2026-08-29-30 — a bare LITERAL statement discard owns its heap.
///
/// The rest of the discard-gate family is missing `Drop` BODIES, which no
/// sanitizer can see. This cell is different: the bare-statement arm
/// chained no `discarded_owned_literal_tail` leg, so `H { s: payload() };`
/// reached no gate at all and its field buffer had no owner — while
/// `let _ = H { s: payload() };`, one spelling over, was clean through the
/// wildcard-let arm's own literal leg. The registrar the fix routes it
/// through carries memory as well as bodies, so both halves land together.
///
/// The tuple rows are the hazard: an element that MOVES A PLACE has its
/// source retracted so this walk becomes the single owner, which is a
/// double free if the retraction is missed and a leak if the walk is. The
/// `let _ =` spelling has carried that pairing since B-2026-08-01-8; the
/// bare statement needed the retraction widened to reach it.
///
/// DELIBERATELY NOT HERE, and measured rather than assumed: a discarded
/// branch or match whose arms are literals of a struct with heap fields
/// but NO `impl Drop` of its own still leaks them — 38 B, unchanged by
/// this fix, for both `if c { P { a: payload() } } else { .. };` and
/// `let _ = match .. { P { a: payload() } .. };`. The bodies half of those
/// is fixed; the memory half returns early in
/// `try_track_discarded_user_drop_temp` because `type_runs_user_drop` is
/// false for such a type. That is B-2026-08-29-32 — a row here would only
/// make this test red.
#[test]
fn asan_bare_literal_statement_discard_owns_its_heap() {
    const H: &str = "struct H { s: String }\n\
             fn seed() -> i64 { env.args().len() }\n\
             fn payload() -> String { f\"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa\" }\n\
             fn slen(s: String) -> i64 { if s.contains(\"payload\") { s.len() } else { 0 } }\n\
             fn main() { println(go()); }\n";
    for (label, body, want) in [
        (
            "bare-struct-literal-statement",
            "fn go() -> i64 { H { s: payload() };\n\
                 \x20  1 }\n",
            "1",
        ),
        (
            "block-wrapped-struct-literal-statement",
            "fn go() -> i64 { { H { s: payload() } };\n\
                 \x20  1 }\n",
            "1",
        ),
        (
            "bare-tuple-literal-statement",
            "fn go() -> i64 { (payload(), 20);\n\
                 \x20  1 }\n",
            "1",
        ),
        (
            "bare-tuple-statement-moving-a-place",
            "fn go() -> i64 { let h = H { s: payload() };\n\
                 \x20  (h, 20);\n\
                 \x20  1 }\n",
            "1",
        ),
        (
            "wildcard-let-tuple-moving-a-place",
            "fn go() -> i64 { let h = H { s: payload() };\n\
                 \x20  let _ = (h, 20);\n\
                 \x20  1 }\n",
            "1",
        ),
        (
            "wildcard-let-struct-literal",
            "fn go() -> i64 { let _ = H { s: payload() };\n\
                 \x20  1 }\n",
            "1",
        ),
        (
            "bound-literal-still-owns-itself",
            "fn go() -> i64 { let h = H { s: payload() };\n\
                 \x20  slen(h.s) }\n",
            "38",
        ),
    ] {
        assert_clean_asan_run(&format!("{H}{body}"), &[want], label);
    }
}

// ── Block expression used AS A VALUE returns a live (not freed) buffer ──
//
// B-2026-06-11-2: a block in value position (`let s = { …; tail }`, an
// `if`/`match` arm, a function-return block) whose tail is a
// scope-registered heap value — an f-string accumulator or a block-local
// `let`-bound String — was freed by the block frame's `drain_top_frame_
// with_emit` between the tail-value load and the value escaping. That left
// the consumer holding a dangling buffer (use-after-free) and, against the
// consumer's own owner cleanup, a double-free. Fix: suppress the tail
// value's cleanup before the block-frame drain so the consumer's binding is
// the sole owner. The loop builds a FRESH heap String each iteration in
// every consumer position with non-foldable (concat / f-string) sources, so
// a stale free of any trips ASAN's heap-use-after-free / double-free.

#[test]
fn asan_block_expr_value_heap_return_no_stale_free() {
    assert_clean_asan_run(
        r#"
enum E { A(String), B }
fn mk(n: i64) -> String { { f"r{n}" } }
fn main() {
    let mut i: i64 = 0;
    while i < 3 {
        let a = { f"a{i}" };
        println(a);
        let b = { let p = "x" + "y"; p };
        println(b);
        let c = if i < 5 { f"c{i}" } else { f"d{i}" };
        println(c);
        let e1 = E.A("m" + "m");
        let d = match e1 { E.A(n) => { f"<{n}>" }, E.B => "z" };
        println(d);
        let e2 = E.A("k" + "k");
        let g = match e2 { E.A(n) => { let p = f"[{n}]"; p }, E.B => "z" };
        println(g);
        println(mk(i));
        i = i + 1;
    }
}
"#,
        &[
            "a0", "xy", "c0", "<mm>", "[kk]", "r0", "a1", "xy", "c1", "<mm>", "[kk]", "r1", "a2",
            "xy", "c2", "<mm>", "[kk]", "r2",
        ],
        "block_expr_value_heap_return",
    );
}

#[test]
fn asan_freshtemp_shared_struct_method_no_double_free() {
    // Slice 3k: a user method on a fresh-temp SHARED-STRUCT receiver
    // (`make().count()`), looped. `make()` returns an RC box at rc==1 owning a
    // `Vec[String]` field; the temp materializes into `__urecv_tmp` and is
    // drop-tracked as ONE scope-exit `RcDec` (`track_rc_var`) — the method
    // borrows / shallow-copies `self`, net-zero on the count, so this single
    // dec drives rc→0 and `__karac_rc_drop_Bag` frees the box + both field
    // Strings. A spurious second dec would free-at-rc==0 twice (macOS ASAN);
    // no dec leaks the whole box (Linux LSan). ≥36-byte field strings + the
    // loop expose either fault.
    assert_clean_asan_run(
        r#"
shared struct Bag { items: Vec[String] }
impl Bag { fn count(self) -> i64 { self.items.len() } }
fn make() -> Bag {
    let mut v: Vec[String] = Vec.new();
    v.push("first field string padded beyond thirty-six bytes ok");
    v.push("second field string padded beyond thirty-six byte");
    Bag { items: v }
}
fn main() {
    let mut p = 0;
    while p < 3 {
        println(make().count());
        p = p + 1;
    };
}
"#,
        &["2", "2", "2"],
        "freshtemp_shared_struct_method_no_double_free",
    );
}

/// B-2026-08-05-33 predicate (a), offset leg — the same by-value generic
/// param with the bare-`T` heap field in the MIDDLE of the struct.
///
/// Separate from the single-field sibling because it fails differently. The
/// declared layout erases `v: T` to one i64 word while the instance is the
/// widened `{ptr,len,cap}`, so a drop synthesized against the declaration
/// GEPs the trailing `Vec[i64]` at the wrong offset — a wild free or a
/// SIGSEGV, not a leak. Resolving the param's declared instantiation is what
/// makes both the layout and the classification mono-correct (the same
/// widening B-2026-07-15-24 established for monomorph bodies), so a
/// regression that only restored the erased layout would still pass the
/// single-field test and abort here.
///
/// Floored per B-2026-08-04-17: both heap fields are read through `len()` on
/// the callee side, so neither entry is a dead allocation at -O2.
#[test]
fn asan_generic_wrapper_mid_heap_field_by_value_param_no_leak() {
    assert_clean_asan_run_min_allocs(
        r#"
struct Multi[T] { a: i64, v: T, w: Vec[i64] }

fn sink(m: Multi[String]) -> i64 { m.a + m.v.len() + m.w.len() }

fn main() {
    let n = env.args().len() as i64;
    let mut acc: i64 = 0;
    let mut i: i64 = 0;
    while i < 40 {
        let mut w: Vec[i64] = Vec.new();
        w.push(i);
        w.push(i + 1);
        let mut s: String = String.new();
        s.push_str("payload-");
        s.push_str(n.to_string());
        s.push_str("-padded-out-to-force-heap");
        let m = Multi { a: 1, v: s, w: w };
        acc = acc + sink(m);
        i = i + 1;
    }
    println(acc);
}
"#,
        // (1 + 34 + 2) x 40 iterations.
        &["1480"],
        "generic_wrapper_mid_heap_field_by_value_param",
        100,
    );
}

// ── Shared struct: rc_inc/rc_dec + final free ─────────────────
// `shared struct Counter` heap-allocates with an RC header.
// Scope-exit runs `emit_rc_dec`; when refcount hits zero, the free
// branch inside `emit_rc_dec` must release the heap block.

#[test]
fn asan_shared_struct_single_owner() {
    assert_clean_asan_run(
        r#"
shared struct Counter { val: i64 }
fn main() {
    let c = Counter { val: 42 };
    println(c.val);
}
"#,
        &["42"],
        "shared_struct_single_owner",
    );
}

// ── Shared struct structural `==` (C1, B-2026-06-19-9) ────────
// The field-walk comparator reads through the RC pointers (and a
// String field's heap buffer) but allocates nothing; this pins that
// it neither leaks nor double-frees the compared structs or their
// String fields when they drop. A ≥36-byte String field defeats LSan's
// short-string reachability blind spot (memory: lsan-reachability).
#[test]
fn asan_shared_struct_structural_eq_no_leak_no_double_free() {
    assert_clean_asan_run(
        r#"
#[derive(Eq, PartialEq)]
shared struct Tag { id: i64, name: String }
fn main() {
    let a = Tag { id: 1, name: "shared-struct-eq-asan-payload-0001" };
    let b = Tag { id: 1, name: "shared-struct-eq-asan-payload-0001" };
    let c = Tag { id: 2, name: "shared-struct-eq-asan-payload-XXXX" };
    if a == b { println("eq"); }
    if a != c { println("ne"); }
}
"#,
        &["eq", "ne"],
        "shared_struct_structural_eq",
    );
}

// ── Shared struct alias: refcount goes to 2, then 0 ───────────
// Binding `b = a` triggers `rc_inc`. Scope-exit runs `rc_dec` twice
// (once per binding); only the last one should free. Catches bugs
// where the alias path double-frees or leaks.

#[test]
fn asan_shared_struct_alias_refcount_balance() {
    assert_clean_asan_run(
        r#"
shared struct Data { x: i64 }
fn main() {
    let a = Data { x: 100 };
    let b = a;
    println(a.x);
    println(b.x);
}
"#,
        &["100", "100"],
        "shared_struct_alias_refcount_balance",
    );
}

// ── Shared struct passed to a function ────────────────────────
// The parameter binding inside the callee adds its own refcount
// lifetime. Both caller- and callee-side rc_dec must balance.

#[test]
fn asan_shared_struct_passed_to_fn() {
    assert_clean_asan_run(
        r#"
shared struct Wrapper { val: i64 }
fn read_val(w: Wrapper) -> i64 { w.val }
fn main() {
    let w = Wrapper { val: 7 };
    println(read_val(w));
}
"#,
        &["7"],
        "shared_struct_passed_to_fn",
    );
}

/// B-2026-08-06-15 — a `shared` handle escaping a VALUE-POSITION BLOCK
/// (`let x = { let b = mk(); b.v };`) leaked one ref per evaluation.
///
/// An ASAN fixture rather than an E2E one, the OPPOSITE of its sibling
/// B-2026-08-06-14: the output is correct before and after, so only a
/// leak-detecting gate can see it. It is also -O0-only — at the default -O2
/// the escaping box is provably dead and LLVM deletes the allocation along
/// with the evidence — so the fixture that matters runs on the
/// `memory-sanitizer-o0` leg. At -O2 it still has to clear the vacuity
/// floor, which is what the byte-level `contains` reads are for.
///
/// The defect was an ownership-protocol disagreement across the block
/// boundary. `suppress_block_tail_cleanup`'s null-store disarms the owner's
/// rc-dec, which TRANSFERS the box's single ref to the escaping value — but
/// the consumer's let-site receive-inc still fired, stranding the count at 1
/// forever. Reference arithmetic, from the emission trace:
/// `1 (alloc) -> 1 (owner dec skipped) -> 2 (receive-inc) -> 1 (consumer
/// dec)`. Dropping the receive-inc for a transferred tail closes it at 0.
///
/// Both directions are covered: under-releasing shows up as the original
/// leak, and over-releasing — the trap here, since the neighbouring repair
/// that removes the null-store instead produces a double free — shows up as
/// an ASAN invalid-free on the same fixture.
#[test]
fn asan_shared_field_escaping_a_value_block_transfers_exactly_one_ref() {
    assert_clean_asan_run_min_allocs(
        r#"shared struct Node { s: String }
struct Box[T] { v: T }
struct Holder { v: Node }

fn mk(i: i64, n: i64) -> Node {
    return Node { s: f"blk-{i}-padded-out-to-force-a-real-heap-buffer-{n}" };
}

fn score(x: Node) -> i64 {
    let mut r: i64 = x.s.len();
    if x.s.contains("padded") { r = r + 1i64; }
    return r;
}

fn main() {
    let n: i64 = env.args().len();
    let mut acc: i64 = 0;
    let mut i: i64 = 0;
    while i < 40i64 {
        // (a) the reported shape — GENERIC wrapper, field escaping a block
        let x1 = { let b = Box { v: mk(i, n) }; b.v };
        acc = acc + score(x1);
        // (b) the CONCRETE spelling, which leaked identically
        let x2 = { let h = Holder { v: mk(i, n) }; h.v };
        acc = acc + score(x2);
        // (c) a NESTED value block — the recursion arm of the same suppressor
        let x3 = { { let b = Box { v: mk(i, n) }; b.v } };
        acc = acc + score(x3);
        // CONTROL, correct before and after: the non-block move-out, whose
        // owner's dec DOES run and whose receive-inc must therefore stay.
        let b4 = Box { v: mk(i, n) };
        let x4 = b4.v;
        acc = acc + score(x4);
        // CONTROL, a String field through the same block shape — the buffer
        // types were never affected and must not start being.
        let s5 = { let b = Box { v: f"str-{i}-padded-out-to-force-a-real-heap-{n}" }; b.v };
        acc = acc + s5.len();
        i = i + 1;
    }
    println(acc);
}
"#,
        &["9230"],
        "shared_field_escaping_a_value_block_transfers_exactly_one_ref",
        // A real -O2 run measures 166 allocations; 120 is what this program
        // can honestly guarantee there. The -O0 leg — where this defect is
        // actually visible, the -O2 build having deleted the escaping box —
        // runs far above it.
        120,
    );
}

/// B-2026-08-06-8 — the third head the bare-generic-param rescue could not
/// see, after String/Vec (B-2026-07-15-11) and Map/Set (B-2026-08-06-1): a
/// field bound to a `shared struct`.
///
/// The gate this time is not the drop classifier but
/// `track_struct_var_inst`, which asks the NAME-ONLY
/// `struct_owns_shared_field` whether a local needs the COMBINED drop
/// (value drop + shared-field rc-dec walker) or the value drop alone.
/// `Box[T] { v: T }` at `T = Node` answered `false` — declared field type
/// `T` is not a shared type — so nothing ever rc-dec'd the box and it
/// leaked. The concrete `Holder { v: Node }` answered `true` and was always
/// clean; that asymmetry IS the bug, and it is why this test carries the
/// concrete spelling alongside the generic one as a live control.
///
/// Making the gate see through the bare param is only half of it. Once the
/// owner's drop rc-dec's the field, moving the field OUT leaves two owners
/// of one +1 and the box hits zero while the moved handle is live — a
/// USE-AFTER-FREE, which is what shapes (d)/(e) cover. The neutralizer is a
/// null-store into the source slot, which the rc-dec walker's existing
/// `build_is_null` guard then skips.
///
/// Catches both directions: under-dec'ing as the original leak, over-dec'ing
/// or a missed neutralize as a use-after-free on the moved handle.
///
/// NOT VACUOUS (B-2026-08-04-17): opaque `env.args().len()` seed, every
/// String built from it at runtime and read back through `.len()`, and 40
/// rounds x seven boxes so nothing folds away.
#[test]
fn asan_bare_generic_param_shared_field_is_rc_dec_and_neutralized() {
    assert_clean_asan_run_min_allocs(
        r#"shared struct Node { s: String }
struct Box[T] { v: T }
struct Holder { v: Node }
struct Trip[T] { a: String, v: T, n: i64 }

fn mk(i: i64, n: i64) -> Node {
    return Node { s: f"shrv-{i}-padded-out-to-force-a-real-heap-buffer-{n}" };
}

// Reads the payload BYTES, not just the length, so the buffer cannot be
// folded away as dead at the default -O2 the ASAN harness builds at.
fn score(x: Node) -> i64 {
    let mut r: i64 = x.s.len();
    if x.s.contains("padded") { r = r + 1i64; }
    if x.s.contains("zzz") { r = r + 1000i64; }
    return r;
}

fn consume(b: Box[Node]) -> i64 { let x = b.v; return score(x); }
fn peek(b: ref Box[Node]) -> i64 { let x = b.v; return score(x); }
fn mid(t: Trip[Node]) -> i64 {
    let x = t.v;
    let mut r: i64 = score(x) + t.n;
    if t.a.contains("padding") { r = r + 1i64; }
    return r;
}

fn main() {
    let n: i64 = env.args().len();
    let mut acc: i64 = 0;
    let mut i: i64 = 0;
    while i < 40i64 {
        // (a) the reported shape — field moved out of a plain LOCAL
        let b1 = Box { v: mk(i, n) };
        let x1 = b1.v;
        acc = acc + score(x1);
        // (b) a WHOLE-struct move, then consumed by value
        let b2 = Box { v: mk(i, n) };
        let b3 = b2;
        acc = acc + consume(b3);
        // (c) a `ref` param read — the source keeps ownership
        let b4 = Box { v: mk(i, n) };
        acc = acc + peek(b4);
        // (d) the MID field of a multi-field wrapper (offset-sensitive)
        let t1 = Trip { a: f"lead-{i}-padding-{n}", v: mk(i, n), n: 3i64 };
        acc = acc + mid(t1);
        // (e) moved out and handed straight to a consuming callee — the
        // use-after-free direction. If the owner's drop dec's a field it no
        // longer owns, the callee reads a freed box.
        let b5 = Box { v: mk(i, n) };
        acc = acc + consume(Box { v: b5.v });
        // CONTROL, correct before and after: never moved, only dropped whole.
        let b6 = Box { v: mk(i, n) };
        acc = acc + 1i64;
        // CONTROL, the CONCRETE spelling that was always clean — it must stay
        // clean, since the fix routes the generic owner onto its exact path.
        let h1 = Holder { v: mk(i, n) };
        let x3 = h1.v;
        acc = acc + score(x3);
        i = i + 1;
    }
    println(acc);
}
"#,
        &["11900"],
        "bare_generic_param_shared_field_is_rc_dec_and_neutralized",
        // 40 rounds x seven rc boxes, each with its own String payload; a
        // real -O2 run measures 526 allocations, so this floor sits well
        // above anything a folded-away run could reach. The byte-level
        // `contains` reads in `score` are what keep it there — with
        // length-only reads the same fixture folded to 48 and tripped the
        // vacuity guard.
        400,
    );
}

/// B-2026-07-28-9: moving a plain struct that holds a `shared` HANDLE
/// field must null that handle in the moved-out source.
///
/// `zero_struct_move_caps` neuters every heap-bearing field of a moved-out
/// struct so the source's drop is a no-op — `cap`/`len` for Vec/String, the
/// tag for Option, the handle for Map/Set. A `shared struct` / `shared enum`
/// handle field had no arm: a shared enum has an `enum_layouts` entry that
/// the loop skips for `is_shared`, and a shared struct is explicitly
/// excluded from the nested-struct recursion, so both fell through to
/// nothing. The source stayed live and rc-dec'd a second time for one owned
/// reference — the second dec reading the refcount word of a block the
/// first already freed.
///
/// Alloc/free counts still BALANCE (the garbage refcount the second dec
/// reads is rarely 1, so no second `free` fires), so only a sanitizer sees
/// it — which is why `examples/tangle/src/undo_redo.kara` printed correct
/// output while corrupting the heap.
#[test]
fn asan_struct_move_nulls_shared_handle_field() {
    // The undo_redo shape: a command struct captures a shared cell, is
    // let-bound, then moved into a Vec. Inline construction was always
    // fine — the `let` is what gives the source a cleanup slot to fire.
    assert_clean_asan_run(
        r#"
shared struct Cell { mut value: i64 }
struct Cmd { cell: Cell }
fn main() {
    let c = Cell { value: 1 };
    let mut v: Vec[Cmd] = Vec.new();
    let cmd = Cmd { cell: c };
    v.push(cmd);
    c.value = 9;
    println(c.value);
    println(v[0].cell.value);
}
"#,
        &["9", "9"],
        "struct_move_nulls_shared_handle_field",
    );
}

#[test]
fn asan_struct_move_nulls_shared_handle_field_through_owned_param() {
    // Same move reached through an owned `shared` param and a `mut ref
    // self` method — the undo_redo call shape exactly.
    assert_clean_asan_run(
        r#"
shared struct Cell { mut value: i64 }
struct Cmd { cell: Cell }
struct Ed { stack: Vec[Cmd] }
impl Ed {
    fn record(mut ref self, cell: Cell) {
        let cmd = Cmd { cell: cell };
        self.stack.push(cmd);
    }
}
fn main() {
    let mut ed = Ed { stack: Vec.new() };
    let c = Cell { value: 5 };
    ed.record(c);
    ed.record(c);
    println(c.value);
}
"#,
        &["5"],
        "struct_move_nulls_shared_handle_field_through_owned_param",
    );
}

/// B-2026-07-28-11: CLONING a value that carries a `shared` handle must
/// RETAIN it — the clone-path mirror of `asan_struct_move_nulls_*` above.
///
/// A handle is one `ptr`, so the shallow primitive clone copied it
/// correctly; what it did not do is retain. A copy of a handle is a new
/// OWNER, so the source's drop and the clone's drop then released for one
/// owned reference — the first freeing, the second reading the refcount
/// word of freed memory. Alloc/free counts still balanced, so this printed
/// the right answer everywhere except under a sanitizer, until enough
/// instances accumulated to corrupt the allocator outright (which is how it
/// surfaced: the self-host emitter cloning a `Vec[MatchArm]`).
///
/// All four shapes below were confirmed RED with the fix disabled.
#[test]
fn asan_clone_retains_shared_handle() {
    // Vec of a struct that CARRIES a handle — the shape that surfaced it.
    assert_clean_asan_run(
        r#"
shared enum E { V(i64) }
struct Row { e: E, n: i64 }
fn main() {
    let mut v: Vec[Row] = Vec.new();
    v.push(Row { e: E.V(1), n: 10 });
    v.push(Row { e: E.V(2), n: 20 });
    let mut t = 0;
    for r in v.clone() { t = t + r.n; }
    println(t);
}
"#,
        &["30"],
        "clone_retains_shared_handle_vec_of_struct",
    );
    // Vec whose ELEMENT IS the handle — reaches the same dispatcher arm
    // directly rather than through a struct field.
    assert_clean_asan_run(
        r#"
shared enum E { V(i64) }
fn main() {
    let mut v: Vec[E] = Vec.new();
    v.push(E.V(1));
    v.push(E.V(2));
    let w = v.clone();
    println(w.len());
}
"#,
        &["2"],
        "clone_retains_shared_handle_vec_of_handle",
    );
    // Map VALUE carrying a handle — the map clone delegates per-value to
    // the same dispatcher.
    assert_clean_asan_run(
        r#"
shared enum E { V(i64) }
struct Row { e: E, n: i64 }
fn main() {
    let mut m: Map[String, Row] = Map.new();
    m.insert("a", Row { e: E.V(1), n: 5 });
    let m2 = m.clone();
    println(m2.len());
}
"#,
        &["1"],
        "clone_retains_shared_handle_map_value",
    );
    // A `shared struct` handle, not just a `shared enum` — both live in
    // `shared_types`, and the arm keys off that map rather than on which
    // kind of shared type it is.
    assert_clean_asan_run(
        r#"
shared struct Cell { mut v: i64 }
struct Row { c: Cell, n: i64 }
fn main() {
    let mut v: Vec[Row] = Vec.new();
    v.push(Row { c: Cell { v: 1 }, n: 7 });
    let w = v.clone();
    println(w.len());
}
"#,
        &["1"],
        "clone_retains_shared_struct_handle",
    );
}

#[test]
fn asan_for_loop_shared_bearing_struct_elem_move_out_no_double_free() {
    // B-2026-07-18-2: a for-loop over `Vec[S]` where S carries a DIRECT
    // `shared` handle field alongside a String. The bare-shared field made
    // `field_copy_supported` bail, so the element was never registered in
    // `for_loop_owned_agg_vars` — a destructured String leaf pushed into an
    // outer Vec and a whole-move (`let x = lf`) both aliased the element's
    // String buffer and double-freed against the container's per-element
    // drain. The registration now runs copy-support in allow-bare-shared
    // mode; move-out copies rc-INC the handle (drain rc-DECs — balanced,
    // so LSan must also see no leak). ≥36-byte String so the fault is loud.
    assert_clean_asan_run(
        r#"
shared enum T2 { Num(i64) }
struct Slf { name: String, value: T2 }
fn main() {
    let mut fs: Vec[Slf] = Vec.new();
    let mut i = 0;
    while i < 6 {
        fs.push(Slf { name: "shared-bearing-elem-move-out-guard-xxxx".to_string(), value: T2.Num(3) });
        i = i + 1;
    }
    let mut lit_names: Vec[String] = Vec.new();
    let mut n = 0;
    for lf in fs {
        let Slf { name: fname, value } = lf;
        match value { T2.Num(k) => { n = n + k; } _ => {} }
        lit_names.push(fname);
    }
    for lf2 in fs {
        let x = lf2;
        n = n + x.name.len();
    }
    println(lit_names.len());
    println(n);
}
"#,
        &["6", "252"],
        "for_loop_shared_bearing_struct_elem_move_out",
    );
}

#[test]
fn asan_generic_struct_heap_field_move_out_no_double_free() {
    // B-2026-07-18-44: a generic struct's owned-by-value param/self whose
    // heap String field is returned (moved out). The monomorph analogue of
    // B-2026-07-18-37: the cap-zero GEP'd the erased generic base layout and
    // the mono-`str`-spelled field wasn't recognized as copy-supported, so
    // `self` stayed a caller-retains alias and the returned field aliased a
    // buffer both the caller and the return binding freed. Covers a free fn
    // and a method, single- and two-field generic structs.
    assert_clean_asan_run(
        r#"
struct Box[T] { v: T }
struct Box2[T] { v: T, n: i64 }
fn take[T](b: Box[T]) -> T { b.v }
impl[T] Box[T] { fn get(self) -> T { self.v } }
impl[T] Box2[T] { fn get(self) -> T { self.v } }
fn main() {
    println(take(Box { v: "a".to_string() }));
    let b = Box { v: "b".to_string() };
    println(b.get());
    let b2 = Box2 { v: "c".to_string(), n: 1 };
    println(b2.get());
}
"#,
        &["a", "b", "c"],
        "generic_struct_heap_field_move_out",
    );
}

/// B-2026-09-04-13 — a `shared struct`'s plain-struct FIELD must have its
/// nested heap freed. The field fell to `SharedFieldKind::None`, a no-op,
/// so the walk neither ran its `Drop` body nor freed its buffer: 134 B
/// over two `String`s, measured, and identical with the `impl Drop`
/// deleted — so this half was never about drop bodies at all.
///
/// The tags are long on purpose. Short ones are inline, and with the field
/// never observed LLVM elides the construction entirely, which is exactly
/// why the original row recorded "memory is not the casualty" and measured
/// a balanced heap. Reading both fields forces materialization.
#[test]
fn asan_shared_holder_frees_its_plain_struct_fields_heap() {
    assert_clean_asan_run(
            r#"
struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(n: i64) -> R { return R { id: n, tag: f"tag-{n}-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" }; }
shared struct Sh { a: R, b: R }
fn main() {
    let seed: Vec[i64] = [9, 109];
    let h = Sh { a: mk(seed[0]), b: mk(seed[1]) };
    println(f"read {h.a.tag} | {h.b.tag}");
    println("mid")
}
"#,
            &[
                "read tag-9-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa | tag-109-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                // Reverse declaration order at the holder's NLL endpoint,
                // then `mid` — the same placement the non-shared control has.
                "dR109",
                "dR9",
                "mid",
            ],
            "shared holder frees its plain-struct field's heap (B-2026-09-04-13)",
        );
}

/// B-2026-08-13-6 — a heap field read off a `shared struct` is deep-cloned,
/// so it no longer double-frees.
///
/// The read handed back a `{ptr,len,cap}` ALIAS of the buffer inside the RC
/// BOX, and the binding's own cleanup then freed what the box's drop also
/// frees. NO CONTAINER IS INVOLVED — `let i = Inner { .. }; let w = i.word;`
/// aborts by itself; the Vec element, the nested hop and the second handle
/// are just other ways of reaching the same read, and all four are pinned.
///
/// CLONING IS RIGHT HERE FOR A REASON THE NON-SHARED CASE DOES NOT SHARE. A
/// non-shared local's field move-out is reconciled by cap-zeroing the
/// SOURCE — a move — which is sound because that local is the only owner.
/// An RC box has no such guarantee: the `two` leg takes a second handle
/// (`let j = i`) and reads the field through the first, so a move model
/// would empty the box under `j`. The interpreter reads the field again
/// afterwards in every leg, which is the copy semantics being restored.
///
/// THE BORROW LEGS ARE LEAK CONTROLS, not double-free ones. A `ref`
/// receiver's frame does not own the box, and the borrowed-receiver arm of
/// `maybe_defensive_copy_param_arg` already clones such a read at the
/// consuming position; cloning again at the READ makes this one the first
/// of two, and the consumer takes it over and clones again, leaving 3 B per
/// call with no owner. All three spellings (`ref self` method, `ref` param
/// bound, `ref` param returned) are here so that regressing the gate shows
/// up as an LSan report rather than as nothing.
///
/// The `loop` leg is the other half of that: 200 iterations of the bound
/// read, which reports 200 blocks if the clone's own cleanup stops firing.
#[test]
fn asan_shared_struct_heap_field_read_cloned_not_aliased() {
    assert_clean_asan_run(
        r#"
shared struct Inner { word: String }
shared struct Leaf { word: String }
shared struct Node { leaf: Leaf }
struct Holder { inner: Inner, tag: i64 }
impl Inner { fn get(ref self) -> String { self.word } }
fn peek(i: ref Inner) -> i64 { let w = i.word; w.len() }
fn give(i: ref Inner) -> String { let w = i.word; w }
fn main() {
    let k = env.args().len() as i64;
    let i = Inner { word: f"a{k}" };
    let bound = i.word;
    let again = i.word;
    let mut acc = bound.len() + again.len();
    let j = i;
    let via_second = j.word;
    acc = acc + via_second.len();
    let mut is: Vec[Inner] = Vec.new();
    is.push(Inner { word: f"b{k}" });
    let elem = is[0].word;
    acc = acc + elem.len();
    let mut hs: Vec[Holder] = Vec.new();
    hs.push(Holder { inner: Inner { word: f"c{k}" }, tag: 2 });
    let hop = hs[0].inner.word;
    acc = acc + hop.len();
    let n = Node { leaf: Leaf { word: f"d{k}" } };
    let deep = n.leaf.word;
    acc = acc + deep.len();
    let o: Option[Inner] = Option.Some(Inner { word: f"e{k}" });
    match o {
        Some(inner) => { let w = inner.word; acc = acc + w.len(); }
        None => { acc = acc + 1; }
    }
    acc = acc + i.get().len() + peek(i) + give(i).len();
    let mut c = 0i64;
    while c < 200 {
        let w = i.word;
        acc = acc + w.len();
        c = c + 1;
    }
    println(acc > 0);
}
"#,
        &["true"],
        "shared_struct_heap_field_read_cloned_not_aliased",
    );
}

/// B-2026-08-02-19 — a GENERIC parent's tuple field: the NestedTuple
/// classification and emit now resolve `(T, i64)` through the mono
/// subst, so the tuple-held Res's String buffer is freed at owner death.
/// The leak was LATENT pre- B-2026-08-02-18 (LLVM elided the dead
/// allocation while nothing read the field); the -18 bodies walk makes
/// the allocation live, so this pin holds the pair together: bodies
/// fire AND the mono tuple memory walk frees the element heap.
#[test]
fn asan_generic_parent_tuple_field_heap_freed() {
    assert_clean_asan_run(
        r#"
struct Res { id: i64, name: String }
impl Drop for Res {
    fn drop(mut ref self) { println(f"drop {self.id} {self.name}") }
}
struct Duo[T] { pair: (T, i64), tag: i64 }
fn main() {
    println("a");
    {
        let d: Duo[Res] = Duo { pair: (Res { id: 5, name: f"tt{5}" }, 9), tag: 1 };
        println(f"tag {d.tag} snd {d.pair.1}");
    }
    println("end");
}
"#,
        &["a", "tag 1 snd 9", "drop 5 tt5", "end"],
        "generic_parent_tuple_field_heap_freed",
    );
}

#[test]
fn asan_tuple_binding_container_element_heap_freed() {
    // B-2026-08-02-26 — a tuple binding whose element is a `Vec[Res]`.
    // The LLVM-type aggregate drop freed the element Vec's BUFFER but not
    // the live elements' String leaves, so this leaked 3 bytes per element
    // under LSan while printing nothing (the bodies walker had declined on
    // the same erased element type). Named-binding, fresh-call and
    // annotated element sources all covered.
    assert_clean_asan_run(
        r#"
struct Res { id: i64, name: String }
impl Drop for Res {
    fn drop(mut ref self) { println(f"drop {self.id} {self.name}") }
}
fn mkv() -> Vec[Res] {
    let mut v: Vec[Res] = Vec.new();
    v.push(Res { id: 2, name: f"bb{2}" });
    v
}
fn main() {
    println("named:");
    {
        let mut xs: Vec[Res] = Vec.new();
        xs.push(Res { id: 1, name: f"aa{1}" });
        let t = (xs, 9);
        println(t.1);
    }
    println("call:");
    {
        let u = (mkv(), 8);
        println(u.1);
    }
    println("annot:");
    {
        let mut ys: Vec[Res] = Vec.new();
        ys.push(Res { id: 3, name: f"cc{3}" });
        let w: (Vec[Res], i64) = (ys, 7);
        println(w.1);
    }
    println("end");
}
"#,
        &[
            "named:",
            "9",
            "drop 1 aa1",
            "call:",
            "8",
            "drop 2 bb2",
            "annot:",
            "7",
            "drop 3 cc3",
            "end",
        ],
        "tuple_binding_container_element_heap_freed",
    );
}

/// B-2026-08-01-10 + B-2026-08-01-11 — bare-statement discard gaps:
/// a bare USER-ENUM ctor statement (`Box2.Full(Res { .. });`) had no
/// codegen ctor channel, so the payload body never fired AND the
/// payload heap leaked (-10); and a discarded no-own-Drop struct temp
/// with a Drop-bearing heap field (`mk_h();` / `let _ = mk_h();`) got
/// a bodies-only registration — the field body fired but the String
/// leaked on both discard arms (-11, the struct sibling of the enum
/// path's `track_enum_var` memory call). LSan (Linux CI) gates both.
#[test]
fn asan_bare_discard_ctor_and_struct_temp_heap_freed() {
    assert_clean_asan_run(
        r#"
struct Res { id: i64, name: String }
impl Drop for Res {
    fn drop(mut ref self) { println(f"drop {self.id} {self.name}") }
}
struct Holder { r: Res }
enum Box2 { Full(Res), Empty }
fn mk_h(n: i64) -> Holder {
    return Holder { r: Res { id: n, name: f"f{n}" } };
}
fn main() {
    println("a");
    Box2.Full(Res { id: 24, name: f"h{24}" });
    println("b");
    mk_h(64);
    println("c");
    let _ = mk_h(63);
    println("end");
}
"#,
        &[
            "a",
            "drop 24 h24",
            "b",
            "drop 64 f64",
            "c",
            "drop 63 f63",
            "end",
        ],
        "bare_discard_ctor_and_struct_temp_heap_freed",
    );
}

#[test]
fn asan_discarded_rc_temp_freed() {
    // A discarded fresh shared-struct (RC box): the producing call returns
    // one owned reference, so `materialize_owned_temp` queues a single
    // `rc_dec` at the `;` (refcount → 0 frees via the recursive drop fn).
    // Faults on macOS if the box is double-freed (e.g. the return-move-out
    // also decs); leaks on Linux if the discard goes untracked.
    assert_clean_asan_run(
        r#"
shared struct Counter { val: i64 }

fn make_counter() -> Counter {
    return Counter { val: 7 };
}

fn main() {
    let mut i = 0;
    while i < 8 {
        make_counter();
        i = i + 1;
    }
    println("done");
}
"#,
        &["done"],
        "discarded_rc_temp_freed",
    );
}

// ── first-class fn values (B-2026-06-20-1) ───────────────────

#[test]
fn asan_named_fn_value_heap_arg_no_leak() {
    // B-2026-06-20-1: a bare named `fn` passed in `Fn(...)` position is
    // reified into a `{trampoline, null env}` fat pointer. The trampoline
    // is a transparent env-ignoring forwarder, so a heap-carrying
    // (String) arg moved through the higher-order call must be owned and
    // freed exactly once — no leak, no double-free. A ≥36-byte payload
    // keeps the buffer off the short-String reachable path so LSan sees a
    // genuine leak if one is introduced.
    assert_clean_asan_run(
        r#"
fn shout(s: String) -> String { f"{s}!" }
fn apply(f: Fn(String) -> String, x: String) -> String { f(x) }
fn main() {
    let r = apply(shout, "hello-this-is-a-fairly-long-payload-string");
    println(r);
}
"#,
        &["hello-this-is-a-fairly-long-payload-string!"],
        "named_fn_value_heap_arg_no_leak",
    );
}

#[test]
fn asan_let_bound_fn_value_heap_arg_no_leak() {
    // B-2026-06-21-1: the same transparent-trampoline guarantee through a
    // fn value bound to a LOCAL first (`let g = shout`) and then passed to a
    // `Fn(...)` parameter. The reified fat pointer's env is null and the
    // trampoline is a module global (no heap), so the only heap is the
    // String arg — it must move through and free exactly once. ≥36-byte
    // payload to defeat the short-String reachable-leak blind spot.
    assert_clean_asan_run(
        r#"
fn shout(s: String) -> String { f"{s}!" }
fn apply(f: Fn(String) -> String, x: String) -> String { f(x) }
fn main() {
    let g = shout;
    let r = apply(g, "hello-this-is-a-fairly-long-payload-string");
    println(r);
}
"#,
        &["hello-this-is-a-fairly-long-payload-string!"],
        "let_bound_fn_value_heap_arg_no_leak",
    );
}

#[test]
fn asan_returned_fn_value_heap_arg_no_leak() {
    // B-2026-06-21-2: a fn value flowed through a `-> Fn(...)` return, then
    // invoked on a heap (String) arg. The returned value is a heap-free fat
    // pointer (null env, module-global trampoline); the only heap is the
    // String arg, which must move through the transparent trampoline and
    // free exactly once. ≥36-byte payload to defeat the short-String
    // reachable-leak blind spot.
    assert_clean_asan_run(
        r#"
fn shout(s: String) -> String { f"{s}!" }
fn pick() -> Fn(String) -> String { shout }
fn main() {
    let f = pick();
    let r = f("hello-this-is-a-fairly-long-payload-string");
    println(r);
}
"#,
        &["hello-this-is-a-fairly-long-payload-string!"],
        "returned_fn_value_heap_arg_no_leak",
    );
}

#[test]
fn asan_let_else_binding_and_else_heap_clean() {
    // let-else: a heap String bound on the match edge drops at scope
    // exit; a heap String built in the diverging else path drops on
    // the `return`. Exercises both edges of `compile_let_else`.
    assert_clean_asan_run(
        r#"
fn make(empty: bool) -> Option[String] {
    if empty {
        return Option.None;
    }
    let s = "hello";
    return Option.Some(s + "!");
}

fn run(empty: bool) {
    let Some(s) = make(empty) else {
        let msg = "was ";
        let full = msg + "empty";
        println(full);
        return
    }
    println(s);
}

fn main() {
    run(false);
    run(true);
    println("done");
}
"#,
        &["hello!", "was empty", "done"],
        "let_else_binding_and_else_heap_clean",
    );
}

#[test]
fn asan_soa_by_value_param_caller_retains_no_leak_or_double_free() {
    // B-2026-06-19-14 slice 1: a SoA `Vec[Entity]` passed BY VALUE to a
    // reader fn whose param (`entities`) matches `layout entities`. The
    // param's signature is the 4-field SoA struct; the callee borrows it
    // (CALLER-RETAINS — no callee-side FreeSoaGroups), so the caller's
    // per-iteration binding frees both group buffers exactly once. Looped
    // 20× so a per-call leak (callee never frees AND caller suppressed) or
    // a double-free (both free) would surface under LSan/ASAN. 600/iter ×
    // 20 = 12000.
    assert_clean_asan_run(
        r#"
struct Entity { x: f64, y: f64, hp: i64 }
layout entities: Vec[Entity] {
    group physics { x, y }
    group combat { hp }
}
fn total(entities: Vec[Entity]) -> i64 {
    let mut t = 0;
    let mut i = 0;
    while i < entities.len() {
        let e = entities[i];
        t = t + e.hp;
        i = i + 1;
    }
    t
}
fn main() {
    let mut sum = 0;
    let mut k = 0;
    while k < 20 {
        let mut entities: Vec[Entity] = Vec.new();
        entities.push(Entity { x: 1.0, y: 2.0, hp: 100 });
        entities.push(Entity { x: 3.0, y: 4.0, hp: 200 });
        entities.push(Entity { x: 5.0, y: 6.0, hp: 300 });
        sum = sum + total(entities);
        k = k + 1;
    }
    println(sum);
}
"#,
        &["12000"],
        "soa_by_value_param_caller_retains",
    );
}

#[test]
fn asan_soa_by_value_param_caller_different_name_caller_retains() {
    // Per-layout monomorphization slice 2: same caller-retains ownership as
    // the sibling above, but the callee param (`rows`) does NOT match the
    // `layout entities` block — the call is served by the on-demand layout
    // monomorph `total$soa_entities` (forward layout-flow inference), not
    // the name-keyed by-value path. The mono's SoA param prologue must keep
    // CALLER-RETAINS (no callee-side FreeSoaGroups), so the caller's
    // per-iteration `entities` frees both group buffers exactly once.
    // Looped 20× so a per-call leak or double-free surfaces under LSan/ASAN.
    assert_clean_asan_run(
        r#"
struct Entity { x: f64, y: f64, hp: i64 }
layout entities: Vec[Entity] {
    group physics { x, y }
    group combat { hp }
}
fn total(rows: Vec[Entity]) -> i64 {
    let mut t = 0;
    let mut i = 0;
    while i < rows.len() {
        let e = rows[i];
        t = t + e.hp;
        i = i + 1;
    }
    t
}
fn main() {
    let mut sum = 0;
    let mut k = 0;
    while k < 20 {
        let mut entities: Vec[Entity] = Vec.new();
        entities.push(Entity { x: 1.0, y: 2.0, hp: 100 });
        entities.push(Entity { x: 3.0, y: 4.0, hp: 200 });
        entities.push(Entity { x: 5.0, y: 6.0, hp: 300 });
        sum = sum + total(entities);
        k = k + 1;
    }
    println(sum);
}
"#,
        &["12000"],
        "soa_by_value_param_caller_different_name",
    );
}

#[test]
fn asan_rc_elision_scratch_loop_repeat() {
    // RC elision phase A: per-iteration elided scratch objects.
    // The elided cleanup is an unconditional free — ASAN catches
    // a free of a still-referenced object (analysis unsound) and
    // LeakSanitizer (linux CI) catches a skipped free. Includes a
    // conditional-branch let (null-guard path) and the read-only
    // declared-owned callee (the inferred-Ref would-be-mode gate).
    assert_clean_asan_run(
        r#"
shared struct Stats { mut count: i64, mut total: i64 }
fn read_only(s: Stats) -> i64 {
    s.count
}
impl Stats {
    fn bump(mut ref self, n: i64) {
        self.count = self.count + 1;
        self.total = self.total + n;
    }
}
fn main() {
    let mut grand = 0;
    let mut iter = 0;
    while iter < 100 {
        let s = Stats { count: 0, total: 0 };
        s.bump(iter);
        grand = grand + s.total + read_only(s);
        if iter > 50 {
            let extra = Stats { count: 1, total: iter };
            grand = grand + extra.total;
        }
        iter = iter + 1;
    }
    println(grand);
}
"#,
        &["8725"],
        "rc_elision_scratch_loop_repeat",
    );
}

// B-2026-06-10-1: `Vec.contains` / `String.contains` codegen lowering.
// `contains` is read-only — it loads each element (or memcmp's a window)
// but never moves out of, frees, or aliases the receiver's buffer. This
// exercises both over genuinely heap-allocated sources (a Vec[String]
// whose elements are f-string heap buffers, and a heap String built via
// push_str) so a stray free / double-free / over-read in the scan would
// trip ASAN. The needle is also a heap f-string for the String case.
#[test]
fn asan_contains_heap_sources_no_uaf() {
    assert_clean_asan_run(
        r#"
fn main() {
    let mut names: Vec[String] = Vec.new();
    let mut i = 0i64;
    while i < 4 { names.push(f"name:{i}"); i = i + 1; }
    println(names.contains(f"name:2"));
    println(names.contains(f"name:9"));

    let mut s: String = "";
    s.push_str("hello ");
    s.push_str("world");
    println(s.contains(f"o w"));
    println(s.contains(f"zzz"));
}
"#,
        &["true", "false", "true", "false"],
        "contains_heap_sources_no_uaf",
    );
}

#[test]
fn asan_rc_fallback_tuple_moved_no_double_free() {
    // B-2026-06-10-8 move-out safety: the new box value-drop recursion
    // must fire exactly once for the binding's last owner. A returned
    // boxed tuple (moved out of the producer), a whole-binding move
    // (`let u = t`), and a partial field read (`let s = t.1`) each go
    // through the refcounted box; the rc gates the field-free to rc==0,
    // so none double-frees the String. macOS ASAN is the double-free
    // oracle here (Linux additionally checks no leak).
    assert_clean_asan_run(
        r#"
fn make(i: i64) -> (i64, String) { (i, f"made-{i}") }
fn main() {
    let r = make(7i64);
    println(r.1);
    let t = (1i64, f"x-{1}");
    let u = t;
    println(u.1);
    let p = (2i64, f"y-{2}");
    let s = p.1;
    println(s);
}
"#,
        &["made-7", "x-1", "y-2"],
        "rc_fallback_tuple_moved",
    );
}

#[test]
fn asan_struct_wrapped_move_out_and_rc_share_no_double_free() {
    // `let t2 = t1` (move-out of a tree), a subtree moved into a parent
    // (RC fan-in), and a builder returning a tree by move. Each node is
    // owned by exactly one live path at a time and freed once.
    assert_clean_asan_run(
        r#"
shared enum Expr { Num(i64), Add(BinOp), Neg(Unary) }
struct BinOp { left: Expr, right: Expr }
struct Unary { operand: Expr }
fn eval(e: Expr) -> i64 {
    match e {
        Num(n) => n,
        Add(b) => eval(b.left) + eval(b.right),
        Neg(u) => 0 - eval(u.operand),
    }
}
fn main() {
    let t1 = Add(BinOp { left: Num(3), right: Num(4) });
    let t2 = t1;
    let sub = Add(BinOp { left: Num(2), right: Num(3) });
    let p1 = Add(BinOp { left: sub, right: Num(10) });
    let p2 = Neg(Unary { operand: Num(7) });
    println(eval(t2));
    println(eval(p1));
    println(eval(p2));
}
"#,
        &["7", "15", "-7"],
        "struct_wrapped_move_out_and_rc_share",
    );
}

#[test]
fn asan_derive_eq_struct_heap_field_vs_ref_param() {
    // B-2026-08-11-33, split out of B-2026-08-11-24 and fixed separately.
    //
    // Same operand pairing as that row — an unbound temp against a `ref`
    // param — but a `#[derive(Eq)]` STRUCT whose String field is heap
    // allocated. The temp struct is never dropped, so its field's buffer
    // leaks: 4 bytes in 1 allocation.
    //
    // B-2026-08-11-24's filing measured this shape as CLEAN and concluded
    // the defect was narrow to String equality. That control used a
    // LITERAL field (see `derive_eq_struct_literal_field` above), which
    // allocates nothing and so could not leak — the pairing was right and
    // the payload was not. Swapping the literal for a `substring` makes it
    // reproduce every time.
    //
    // Fixed separately from -24 because it is a different lowering:
    // `compile_struct_eq`, not the surface-`Binary` String path. The leak
    // is a heap FIELD inside the operand rather than the operand's own
    // buffer, so the fix gives the temp an OWNER — materialize into a slot
    // and register its struct drop, the
    // `track_freshtemp_field_access_object` pattern from B-2026-07-22-2 —
    // instead of freeing a buffer. Ordering is what makes that safe: the
    // comparison has already read both operands when the tracking runs,
    // and the registered drop fires at scope exit.
    assert_clean_asan_run(
        r#"
#[derive(Eq)]
struct P { name: String }
fn mk(s: ref String) -> P { P { name: s.substring(0, 4) } }
fn f(hay: ref String, other: ref P) -> bool { mk(hay) == other }
fn main() {
    let h = "abcdefgh";
    let p = P { name: "abcd" };
    println(f"{f(h, p)}");
}
"#,
        &["true"],
        "derive_eq_struct_heap_field_vs_ref_param",
    );
}

#[test]
fn asan_chained_call_struct_heap_field_no_leak() {
    // B-2026-07-03-3: a chained `n.relabel().name` reads a HEAP (String)
    // field off a method-call temporary that was never exercised before
    // the fix (it returned 0). Loop it with a >=36-byte payload so any
    // leak of the temp struct's String buffer (or a double-free of the
    // extracted field) trips Linux LSan / macOS ASan.
    assert_clean_asan_run(
        r#"
struct N { name: String, id: i64 }
impl N {
    fn relabel(self) -> N {
        N { name: "a sufficiently long heap payload for lsan", id: self.id + 1 }
    }
}
fn main() {
    let mut i = 0i64;
    let mut acc = 0i64;
    while i < 50i64 {
        let n = N { name: "seed string that is also quite long", id: i };
        acc = acc + n.relabel().name.len() + n.relabel().id;
        i = i + 1;
    }
    println(f"{acc}");
}
"#,
        &["3325"],
        "chained_call_struct_heap_field_no_leak",
    );
}

#[test]
fn asan_fresh_some_shared_reused_across_consuming_calls_no_double_free() {
    // B-2026-07-11-21: a fresh `let orig = Some(Node { .. })` (an untyped
    // `Some(<shared struct literal>)` binding) passed BY VALUE to a
    // recursive consumer that clones the matched subtree, TWICE. Before the
    // fix the binding was never registered as `Option[shared]` (no call-site
    // retain, no scope-exit dec), so each consuming call's param drop
    // decremented `orig`'s refcount — the first call freed the tree, the
    // second double-freed it (glibc "malloc(): unaligned tcache chunk").
    // The interpreter was correct throughout; codegen (JIT+AOT) corrupted
    // the heap. Registering the fresh-Some binding into the caller-retains
    // model (one arg-site inc per pass, one scope-exit dec) balances it.
    assert_clean_asan_run(
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
    let orig = Some(Node { val: 1, left: Some(Node { val: 2, left: None, right: None }), right: None });
    let c1 = clone_offset(orig, 10);
    let c2 = clone_offset(orig, 20);
    println(count_nodes(c1) + count_nodes(c2));
}
"#,
        &["4"], // two 2-node clones
        "fresh_some_shared_reused_across_consuming_calls_no_double_free",
    );
}

#[test]
fn asan_shared_scrutinee_shadowed_by_local() {
    // B-2026-07-12-31: a `match e { … }` arm over a by-value shared-enum
    // param `e` that declares a same-named local (`let mut e = 0`) shadows
    // the scrutinee's pointer slot. The param's scope-exit RC-dec reloaded
    // its pointer BY NAME from `variables["e"]`, which the shadow had
    // repointed at an `i64` alloca — so the dec walked an integer-as-pointer
    // and corrupted the heap (segfault at O2, hang at O0). The fix gates the
    // reload on the slot being pointer-typed, falling back to the pointer
    // captured at registration. Looped so each call allocates + frees the
    // shared node; a garbage-pointer RC-dec surfaces as ASAN
    // use-after-free / heap corruption and a wrong-slot drop as an LSan
    // leak. Kept a NON-recursive enum with i64 payloads so this isolates
    // the shadow RC-dec — a recursive `Node(E)` shape would also exercise
    // the separate shared-enum recursive-payload-drop path.
    assert_clean_asan_run(
        r#"
shared enum E { A(i64), B(i64) }
fn chk(e: E) -> i64 {
    match e {
        A(n) => n,
        B(m) => {
            let mut e = 0;
            let mut i = 0;
            loop {
                if i >= 3 { break; }
                e = e + i;
                i = i + 1;
            }
            m + e
        }
    }
}
fn main() {
    let mut i: i64 = 0;
    let mut t: i64 = 0;
    while i < 200 {
        t = t + chk(B(10));
        i = i + 1;
    }
    println(f"{t}");
}
"#,
        // (10 payload + 3 from 0+1+2) * 200 = 13 * 200 = 2600.
        &["2600"],
        "shared_scrutinee_shadowed_by_local",
    );
}

// ── RC-elision payload-escape guard (KARAC_RC_ELIDE_REF_PARAMS) ───────
// Condition 4 (src/rc_elide.rs) closes the "known residual" from
// docs/spikes/rc-elide-ref-params.md: a match-binding of the candidate
// param passed BY VALUE to a consuming callee (`match p { Some(n) =>
// consume(n) }`). The guard makes elision sound BY CONSTRUCTION — it
// DECLINES to elide any param whose payload is moved out as a bare value, so
// `probe`/`probe2`/`probe3` below run on the normal balanced-RC path (the
// elidable set is empty for them, verified in src/rc_elide.rs unit tests).
// The positive control (is_mirror/is_symmetric — payloads used only via
// projections) IS elided. Run the whole suite with
// `KARAC_RC_ELIDE_REF_PARAMS=1`: all must be byte-for-byte as clean as
// flag-off (Linux LSan). Together with the unit tests these pin both halves:
// the guard declines the escaping shapes, and the elided walkers stay
// leak-free. (Runtime was already balanced even pre-guard — the guard
// removes the reliance on codegen's payload re-share, not a live leak.)

#[test]
fn asan_rc_elide_mutation_through_alias_projection_guarded() {
    // B-2026-07-16-7 regression pin — the condition-5 miscompile shape.
    // `walk` satisfies conditions 1-4 (scrutinee-only param, projection
    // args at its call site, scalar return, projection-only payload), yet
    // its arm passes the payload's BACK-pointer (`n.back`) to `detach`,
    // which field-assigns through the alias (`m.left = None`) — releasing
    // the only count keeping the borrowed `n` alive. Pre-condition-5 this
    // ELIDED `walk`, and the default build read freed memory: a SILENT
    // WRONG ANSWER (garbage instead of 8) plus a valgrind invalid-read.
    // Condition 5 (5c: a lineage projection may flow only to an elided
    // position — detach is dropped by 5a's store ban) reverts `walk` to
    // the owned protocol: n holds its own +1 across the arm, the read is
    // valid, and the mutation's release balances at arm exit. The `make`
    // helper keeps the child's only extra count out of main's frame so
    // the field really is the last count. Must print 8 and be
    // LSan/ASAN-clean under the default AND `=0` paths.
    assert_clean_asan_run(
        r#"
shared struct Node {
    val: i64,
    mut left: Option[Node],
    mut back: Option[Node],
}
fn detach(q: Option[Node]) -> i64 {
    match q {
        None => 0,
        Some(m) => {
            m.left = None;
            1
        },
    }
}
fn walk(p: Option[Node]) -> i64 {
    match p {
        None => 0,
        Some(n) => detach(n.back) + n.val,
    }
}
fn make() -> Node {
    let parent = Node { val: 10, left: None, back: None };
    let child = Node { val: 7, left: None, back: Some(parent) };
    parent.left = Some(child);
    parent
}
fn main() {
    let parent = make();
    let total = walk(parent.left);
    println(total);
}
"#,
        &["8"],
        "rc_elide_mutation_through_alias_projection_guarded",
    );
}

#[test]
fn asan_rc_elide_recursive_tree_sum_pool_no_leak() {
    // B-2026-07-15-21 second-shape pin: an 8-tree POOL summed via
    // `sum(pool[rep % 8])` — the call site is an Index PROJECTION into a
    // caller-held Vec (condition 1's `v[i]` leg, where the sibling pin
    // `asan_rc_elide_some_binding_or_recursion_walk_no_leak` exercises a
    // 2-node pool + or-short-circuit walk). `sum` is a COMBINE-BOTH
    // recursion (`n.val + sum(n.left) + sum(n.right)` — no TCO), so this
    // pins the pure Part A + Part B rc-elision with both child calls
    // live: zero header RMWs per node on the default path, pool stays
    // sole owner, every node drops exactly once at scope exit. Must be
    // clean under the default (elided) AND `=0` (owned-protocol) paths —
    // LSan catches an over-elide as a leak, an under-balance as a
    // UAF/double-free. 31-node trees x 200 reps amplify any per-visit
    // imbalance.
    assert_clean_asan_run(
        r#"
shared struct TreeNode {
    val: i64,
    left: Option[TreeNode],
    right: Option[TreeNode],
}
fn build(depth: i64, counter: i64) -> Option[TreeNode] {
    if depth == 0 {
        return None;
    }
    let left = build(depth - 1, counter * 2);
    let right = build(depth - 1, counter * 2 + 1);
    return Some(TreeNode { val: counter, left: left, right: right });
}
fn sum(node: Option[TreeNode]) -> i64 {
    match node {
        None => 0,
        Some(n) => n.val + sum(n.left) + sum(n.right),
    }
}
fn main() {
    let mut pool: Vec[Option[TreeNode]] = Vec.new();
    let mut i = 0;
    while i < 8 {
        pool.push(build(5, 1));
        i = i + 1;
    }
    let mut total = 0;
    let mut rep = 0;
    while rep < 200 {
        total = total + sum(pool[rep % 8].clone());
        rep = rep + 1;
    }
    println(total);
}
"#,
        &["99200"],
        "rc_elide_recursive_tree_sum_pool_no_leak",
    );
}

#[test]
fn asan_rc_elide_some_binding_or_recursion_walk_no_leak() {
    // B-2026-07-15-21 Part B — the Some(n)-binding acquire/RcDec is ALSO
    // elided (not just the param + child-arg retains) when the scrutinee is
    // an elidable param, so a read-only OR-recursion walk carries ZERO rc
    // ops per node and the tail recursion loop-ifies. `has_path_sum` is the
    // canonical shape: bool-returning, `node` used only as a `match`
    // scrutinee, payload read only via projections (`n.val`, `n.left`,
    // `n.right`) into `ref`/borrowed positions. Must be leak- AND
    // UAF-clean: a 5-node tree, has_path_sum 200x (100 achievable target 7
    // → true, 100 unachievable target 6 → false) → prints 100. If the
    // Some-binding elision were unsound the shared nodes would double-free
    // (release without acquire) or leak (acquire without release) here.
    assert_clean_asan_run(
        r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn has_path_sum(node: Option[Node], target: i64) -> bool {
    match node {
        None => false,
        Some(n) => {
            let rem = target - n.val;
            let ln = match n.left { None => true, Some(_) => false };
            let rn = match n.right { None => true, Some(_) => false };
            if ln and rn { rem == 0i64 }
            else { has_path_sum(n.left, rem) or has_path_sum(n.right, rem) }
        }
    }
}
fn main() {
    let l = Some(Node { val: 2i64, left: Some(Node { val: 4i64, left: None, right: None }), right: Some(Node { val: 5i64, left: None, right: None }) });
    let r = Some(Node { val: 3i64, left: None, right: None });
    let root = Some(Node { val: 1i64, left: l, right: r });
    let mut pool: Vec[Option[Node]] = Vec.new();
    pool.push(root);
    let mut t = 0i64;
    let mut rep = 0i64;
    while rep < 200i64 {
        let tgt = if (rep % 2i64) == 0i64 { 7i64 } else { 6i64 };
        let hit = has_path_sum(pool[0i64].clone(), tgt);
        t = t + (if hit { 1i64 } else { 0i64 });
        rep = rep + 1i64;
    }
    println(f"{t}")
}
"#,
        &["100"],
        "rc_elide_some_binding_or_recursion_walk_no_leak",
    );
}

#[test]
fn asan_rc_elide_borrow_forward_ref_param_no_leak() {
    // B-2026-07-15-21 Part C — the condition-1 borrow-forward relaxation: a
    // thin wrapper `is_balanced(root)` forwards its `Ref` param `root` by
    // bare identifier to the recursive helper `check`. That forward is a
    // borrow (a `Ref` param's referent is kept alive by the enclosing frame
    // for the whole call), so `check`'s `node` elides its per-node RC. Must
    // be leak- AND UAF-clean: eliding `check`'s retain/release while the
    // wrapper's `root` (and up-chain `pool`) keep the tree alive. Balanced
    // tree → true, right-chain → unbalanced → false; 200 reps → 100.
    assert_clean_asan_run(
        r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn check(node: Option[Node]) -> i64 {
    match node {
        None => 0i64,
        Some(n) => {
            let lh = check(n.left);
            if lh == -1i64 { return -1i64; }
            let rh = check(n.right);
            if rh == -1i64 { return -1i64; }
            let d = lh - rh;
            let ad = if d < 0i64 { -d } else { d };
            if ad > 1i64 { return -1i64; }
            if lh > rh { 1i64 + lh } else { 1i64 + rh }
        }
    }
}
fn is_balanced(root: Option[Node]) -> bool { check(root) != -1i64 }
fn main() {
    let bal = Some(Node { val: 4i64, left: Some(Node { val: 2i64, left: Some(Node{val:1i64,left:None,right:None}), right: Some(Node{val:3i64,left:None,right:None}) }), right: Some(Node { val: 6i64, left: Some(Node{val:5i64,left:None,right:None}), right: Some(Node{val:7i64,left:None,right:None}) }) });
    let unb = Some(Node { val: 1i64, left: None, right: Some(Node { val: 2i64, left: None, right: Some(Node{val:3i64,left:None,right:None}) }) });
    let mut pool: Vec[Option[Node]] = Vec.new();
    pool.push(bal);
    pool.push(unb);
    let mut t = 0i64;
    let mut rep = 0i64;
    while rep < 200i64 { let idx = rep % 2i64; if is_balanced(pool[idx].clone()) { t = t + 1i64; } rep = rep + 1i64; }
    println(f"{t}")
}
"#,
        &["100"],
        "rc_elide_borrow_forward_ref_param_no_leak",
    );
}

/// B-2026-08-12-18 — the INTERIOR of the envelope the fixture above owns.
/// `struct S { o: Option[Option[String]] }` inside a `Result[S, i64]`: the
/// box is freed, and the `String` inside it was freed by nobody.
///
/// THE ROW SCOPED THIS TO THE FRESH-TEMP ARGUMENT and that is too narrow,
/// which is why this fixture is a matrix. Measured per position against
/// the pre-fix compiler, at the DEFAULT `-O2`:
///
///   * `nomatch` — let-bound, never matched, no call in the program: LEAKED
///   * `wild`    — let-bound, `Ok(_)`: LEAKED
///   * `bound`   — let-bound, passed BY VALUE to a callee: LEAKED
///   * `temp`    — the row's own shape, `cls(Result.Ok(S { … }))`: LEAKED
///   * `sbind`   — let-bound, arm binds the STRUCT (`Ok(s) => 1`): clean
///   * `deep`    — let-bound, arm binds the String out: clean
///
/// The two clean ones are the reason this cannot be an unconditional free,
/// and they are in the fixture as the DOUBLE-FREE guard: an arm that binds
/// the struct out runs `__karac_drop_struct_S`, which frees the box and the
/// interior, so a second owner here aborts rather than leaks. They stay
/// green because the handoff disarms this action wholesale — the arm-bind
/// suppressor zeroes the box word and the null guard skips everything,
/// interior included — so no separate retraction was needed.
///
/// THE INTERIOR IS ALLOCATED EXACTLY ONCE in every position: all six
/// spellings report 48 allocations for 40 iterations, so nothing anywhere
/// deep-copies it and exactly one owner is correct. That measurement is
/// what rules out "give the callee one too" as the fix.
///
/// A `String` payload makes this non-vacuous at `-O2` for the reason the
/// sibling fixture's `sbox` leg documents: an all-scalar envelope folds
/// away entirely, and a clean run over an allocation that never happened
/// proves nothing. Here the interior IS the heap, so both levels are real
/// — unlike the sibling, this fixture pins at `-O2` as well as `-O0`.
#[test]
fn asan_struct_field_boxed_interior_owned_without_arm() {
    assert_clean_asan_run_min_allocs(
        r#"
struct S { o: Option[Option[String]] }
fn cls(r: Result[S, i64]) -> i64 {
    match r { Result.Ok(s) => match s.o { Option.Some(Option.Some(t)) => t.len() as i64, _ => -1 }, Result.Err(e) => e }
}
fn main() {
    let n = env.args().len() as i64;
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 40 {
        let nomatch: Result[S, i64] = Result.Ok(S { o: Option.Some(Option.Some(f"a{n + i}")) });
        let wild: Result[S, i64] = Result.Ok(S { o: Option.Some(Option.Some(f"b{n + i}")) });
        acc = acc + match wild { Result.Ok(_) => 1, Result.Err(e) => e };
        let sbind: Result[S, i64] = Result.Ok(S { o: Option.Some(Option.Some(f"c{n + i}")) });
        acc = acc + match sbind { Result.Ok(s) => 2, Result.Err(e) => e };
        let deep: Result[S, i64] = Result.Ok(S { o: Option.Some(Option.Some(f"d{n + i}")) });
        acc = acc + match deep {
            Result.Ok(s) => match s.o { Option.Some(Option.Some(t)) => t.len() as i64, _ => -1 },
            Result.Err(e) => e,
        };
        let bound: Result[S, i64] = Result.Ok(S { o: Option.Some(Option.Some(f"e{n + i}")) });
        acc = acc + cls(bound);
        acc = acc + cls(Result.Ok(S { o: Option.Some(Option.Some(f"f{n + i}")) }));
        i = i + 1;
    }
    println(acc);
}
"#,
        // Per iteration: 1 (wild) + 2 (sbind) + len(deep) + len(bound) + len(temp).
        // The three measured strings are "d1".."d40" etc — 2 chars for the
        // first nine, 3 thereafter — so 3 + 3*(9*2 + 31*3) = 3*111 + 3 = 336
        // over the whole loop, plus 3*40 = 120 for the two constants.
        &["453"],
        "struct_field_boxed_interior_owned_without_arm",
        40,
    );
}

/// B-2026-08-12-19 — the CALLEE's ENTRY COPY of the same shape. The row
/// above owns the caller's value; this owns the copy the callee makes of
/// it, which is a genuinely separate allocation.
///
/// THE COUNT IS THE ARGUMENT, and it is what rules out "the caller already
/// owns it, so this is a double free": the program reports 168 allocations
/// for 40 iterations — FOUR per call, two boxes and two Strings. The entry
/// copy deep-copies through the box, so each side needs its own owner and
/// freeing here cannot reach the caller's.
///
/// A MATRIX OVER THE CALLEE'S BODY, because that is the axis that decides
/// ownership — not the argument form, which is what the row was filed
/// against. Measured per callee at `-O0` before the fix:
///
///   * `wild`    — `Result.Ok(_)`, binds nothing:        LEAKED 1,391 B
///   * `nomatch` — never matches the param at all:       LEAKED 1,391 B
///   * `sbind`   — `Result.Ok(s) => 2`, binds the struct: clean
///   * `deep`    — binds the struct and reads the String: clean
///
/// The two clean ones are the DOUBLE-FREE GUARD and the reason this could
/// not be an unconditional registration. `functions.rs` declined this
/// population precisely because they abort — an arm that binds gets its own
/// `StructDrop`, so a second owner is a glibc `double free detected in
/// tcache 2`. They stay green because the param now joins
/// `struct_field_boxed_payload_vars`, which is what lets the existing
/// arm-bind handoff zero the box word and disarm this action; registering
/// WITHOUT joining that set is the earlier attempt the old comment records
/// failing.
///
/// `-O0` IS WHERE THIS PINS. At `-O2` the copy is dead and LLVM deletes it,
/// so all four legs pass there whether the fix is present or not — the
/// `scripts/asan-o0-leg.sh` run is the gate, exactly as for the fixtures
/// this file's `-O0` list was built for.
#[test]
fn asan_struct_field_boxed_interior_nonbinding_callee() {
    assert_clean_asan_run_min_allocs(
        r#"
struct S { o: Option[Option[String]] }
fn wild(r: Result[S, i64]) -> i64 { match r { Result.Ok(_) => 1, Result.Err(e) => e } }
fn nomatch(r: Result[S, i64]) -> i64 { return 3; }
fn sbind(r: Result[S, i64]) -> i64 { match r { Result.Ok(s) => 2, Result.Err(e) => e } }
fn deep(r: Result[S, i64]) -> i64 {
    match r { Result.Ok(s) => match s.o { Option.Some(Option.Some(t)) => t.len() as i64, _ => -1 }, Result.Err(e) => e }
}
fn main() {
    let n = env.args().len() as i64;
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 40 {
        acc = acc + wild(Result.Ok(S { o: Option.Some(Option.Some(f"g{n + i}")) }));
        acc = acc + nomatch(Result.Ok(S { o: Option.Some(Option.Some(f"h{n + i}")) }));
        acc = acc + sbind(Result.Ok(S { o: Option.Some(Option.Some(f"i{n + i}")) }));
        acc = acc + deep(Result.Ok(S { o: Option.Some(Option.Some(f"j{n + i}")) }));
        i = i + 1;
    }
    println(acc);
}
"#,
        // Per iteration: 1 + 3 + 2 + len("j1".."j40"). Over 40 iterations
        // that is 40*6 = 240 constant plus 9*2 + 31*3 = 111 for the lengths.
        &["351"],
        "struct_field_boxed_interior_nonbinding_callee",
        40,
    );
}

#[test]
fn asan_reduction_over_shared_pool_no_race_uaf() {
    // B-2026-07-16-6: the auto-par reduction lowering ran this loop's
    // body on multiple worker threads while it carried NON-atomic
    // rc-inc/rc-dec traffic on the same 8 pooled shared trees (element
    // retain in the worker, callee-drop release inside `sum`). Racing
    // workers lost refcount updates, freeing live nodes: ASAN reports
    // heap-use-after-free / attempting-double-free within a few hundred
    // iterations pre-fix. Post-fix the reduction recognizer declines the
    // shared-capturing body (cross-task-safe gate) so the loop runs
    // sequentially: clean ASAN, exact deterministic sum, zero leaks at
    // exit (pool + trees drop through the normal sequential path).
    assert_clean_asan_run(
        r#"
shared struct TreeNode {
    val: i64,
    left: Option[TreeNode],
    right: Option[TreeNode],
}
fn build(depth: i64, counter: i64) -> Option[TreeNode] {
    if depth == 0 {
        return None;
    }
    let left = build(depth - 1, counter * 2);
    let right = build(depth - 1, counter * 2 + 1);
    return Some(TreeNode { val: counter, left: left, right: right });
}
fn sum(node: Option[TreeNode]) -> i64 {
    match node {
        None => 0,
        Some(n) => n.val + sum(n.left) + sum(n.right),
    }
}
fn main() {
    let mut pool: Vec[Option[TreeNode]] = Vec.new();
    let mut i = 0;
    while i < 8 {
        pool.push(build(5, 1));
        i = i + 1;
    }
    let mut total = 0;
    let mut rep = 0;
    while rep < 1000 {
        total = total + sum(pool[rep % 8].clone());
        rep = rep + 1;
    }
    println(total);
}
"#,
        &["496000"],
        "reduction_over_shared_pool_no_race_uaf",
    );
}

#[test]
fn asan_nil_coalesce_heap_fallbacks_clean() {
    // B-2026-08-17-27 — `??` is lowered to `unwrap_or`, so it inherits that
    // path's ownership machinery: the moved-binding cap-zeroing (leg 1), the
    // present-path free of a collection literal (leg 2), and the f-string
    // `acc` cleanup suppression (leg 3) of B-2026-07-16-23. Inheriting is
    // the POINT of the lowering — a second implementation of `??` would have
    // had to re-derive all three, and the hand-rolled interpreter version it
    // replaced got the plain semantics wrong on three of four legs, never
    // mind the memory.
    //
    // This is the `??` twin of the two fixtures below it: same three default
    // shapes, spelled with `??`, plus a `Result` receiver that the old
    // typechecker never even admitted. If the desugar introduced an extra
    // copy or lost a suppression, the double-free aborts here and any
    // per-iteration leak accumulates over 200 iterations for LSan.
    assert_clean_asan_run(
        r#"
fn opt_s(i: i64) -> Option[String] {
    if i % 2 == 0 { Some("even-payload".to_string()) } else { None }
}
fn opt_v(i: i64) -> Option[Vec[i64]] {
    if i % 3 == 0 { Some([i, i + 1]) } else { None }
}
fn res_s(i: i64) -> Result[String, String] {
    if i % 2 == 0 { Ok("ok-payload".to_string()) } else { Err("bad") }
}
fn main() {
    let mut total: i64 = 0;
    let mut i: i64 = 0;
    while i < 200 {
        let ds = "string-fallback".to_string();
        let s = opt_s(i) ?? ds;
        total = total + (s.len() as i64);
        let dv: Vec[i64] = [7, 7, 7];
        let v = opt_v(i) ?? dv;
        total = total + (v.len() as i64);
        let f: String = opt_s(i) ?? f"def-{i}";
        total = total + (f.len() as i64);
        let r: String = res_s(i) ?? "err-fallback";
        total = total + (r.len() as i64);
        i = i + 1;
    }
    println(total);
}
"#,
        &["7278"],
        "nil_coalesce_heap_fallbacks_clean",
    );
}

/// B-2026-07-31-17: a `let` whose init TERMINATES (`let x = { return
/// s.len(); }`) with a heap local live in scope. The return edge's
/// scope-exit cleanup must free the f-string String on every iteration —
/// pre-fix this program did not even compile ("Terminator found in the
/// middle of a basic block"), so this gate cannot pass vacuously.
#[test]
fn asan_terminated_let_init_frees_live_heap_local() {
    assert_clean_asan_run(
        r#"
fn t(k: i64) -> i64 {
    let s = f"payload-{k}";
    let x = { return s.len(); };
    0
}
fn main() {
    let mut n = 0i64;
    let mut i = 0i64;
    while i < 200i64 {
        n = n + t(i);
        i = i + 1;
    }
    println(n);
}
"#,
        // "payload-0".len() = 9 for i 0..9 (10 iters), 10 for i 10..99
        // (90 iters), 11 for i 100..199 (100 iters): 90 + 900 + 1100 = 2090.
        &["2090"],
        "terminated_let_init_frees_live_heap_local",
    );
}

/// B-2026-08-29-32 — a discarded BRANCH whose arms are literals of a
/// struct that carries heap but declares no `Drop` ANYWHERE in it.
///
/// This is a pure MEMORY row and only this file can see it: `P` has no
/// body to run, so every body-count fixture in the tree reads identically
/// before and after. The discard battery answered the bodies question
/// correctly — there is none — and then RETURNED, and the return sat above
/// the one memory registration that path makes, so the `String` was
/// stranded. The directly-discarded literal was always clean, because it
/// routes through `track_inline_owned_aggregate_arg`, which carries memory
/// AND bodies rather than gating the first on the second; that asymmetry
/// between two spellings of one discard is the whole bug.
///
/// The loop makes the leak a multiple rather than a single block so a
/// partial fix cannot hide inside allocator noise. Measured on the pinned
/// program below: 810 B in 20 blocks definitely lost before the fix and
/// clean after, at `KARAC_OPT_LEVEL=0` AND at the default `-O2` this
/// harness actually builds at. Both levels are stated because the sibling
/// fixture for B-2026-08-29-30 gates at `-O0` only — there the discarded
/// value was a pure temporary LLVM deletes under optimization, and reading
/// that caveat across to this row would understate what this one proves.
#[test]
fn asan_discarded_branch_of_body_less_heap_literals_frees_once() {
    assert_clean_asan_run(
        r#"
struct P { a: String, b: i64 }
fn seed() -> i64 { env.args().len() }
fn payload(i: i64) -> String { f"payload-{seed()}-{i}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa" }
fn go() -> i64 {
    let mut i = 0i64;
    while i < 20i64 {
        if seed() > 0 { P { a: payload(i), b: 1 } } else { P { a: payload(i), b: 2 } };
        let _ = match seed() { 1 => { P { a: payload(i), b: 3 } } _ => { P { a: payload(i), b: 4 } } };
        i = i + 1;
    }
    return 1;
}
fn main() { println(go()); }
"#,
        &["1"],
        "discarded_branch_body_less_heap_literal_loop",
    );
    // Every spelling of the same discard, one evaluation each: bare `if`,
    // `let _ = if`, bare `match`, `let _ = match`, an `else if` chain, a
    // `match` NESTED in an `if` arm, and arms that are CALLS rather than
    // literals. All seven stranded 38 B apiece before the fix.
    assert_clean_asan_run(
        r#"
struct P { a: String, b: i64 }
fn seed() -> i64 { env.args().len() }
fn payload() -> String { f"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa" }
fn mkp(n: i64) -> P { return P { a: payload(), b: n }; }
fn go() -> i64 {
    if seed() > 0 { P { a: payload(), b: 1 } } else { P { a: payload(), b: 2 } };
    let _ = if seed() > 0 { P { a: payload(), b: 1 } } else { P { a: payload(), b: 2 } };
    match seed() { 1 => { P { a: payload(), b: 1 } } _ => { P { a: payload(), b: 2 } } };
    let _ = match seed() { 1 => { P { a: payload(), b: 1 } } _ => { P { a: payload(), b: 2 } } };
    let _ = if seed() > 9 { P { a: payload(), b: 1 } } else if seed() > 0 { P { a: payload(), b: 2 } } else { P { a: payload(), b: 3 } };
    let _ = if seed() > 0 { match seed() { 1 => { P { a: payload(), b: 1 } } _ => { P { a: payload(), b: 2 } } } } else { P { a: payload(), b: 3 } };
    let _ = if seed() > 0 { mkp(1) } else { mkp(2) };
    return 1;
}
fn main() { println(go()); }
"#,
        &["1"],
        "discarded_branch_body_less_heap_literal_spellings",
    );
    // THE GUARD, and the reason this fix is not simply "register memory
    // here". A field initializer that is a PROJECTION OFF A FRESH TEMP
    // (`mkv(1).v`) ALIASES that temp's buffer, and the temp keeps its own
    // cleanup — so registering the literal too frees one pointer twice.
    // Registering unconditionally turned this program from clean into
    // `free(): double free detected in tcache 2`, which is a strictly
    // worse trade than the 38 B leak it was fixing.
    //
    // The aliasing itself is PRE-EXISTING and is filed separately: the
    // `let`-BOUND spelling of the identical literal double-frees on a tree
    // without this fix, with no discard anywhere in it. This fixture pins
    // only that the DISCARD path declines to walk into it.
    assert_clean_asan_run(
        r#"
struct V { v: Vec[i64], b: i64 }
struct P { a: String, b: i64 }
fn seed() -> i64 { env.args().len() }
fn payload() -> String { f"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa" }
fn mkv(n: i64) -> V { let mut q: Vec[i64] = Vec.new(); q.push(n); return V { v: q, b: n }; }
fn mkp(n: i64) -> P { return P { a: payload(), b: n }; }
fn go() -> i64 {
    if seed() > 0 { V { v: mkv(1).v, b: 1 } } else { V { v: mkv(2).v, b: 2 } };
    let _ = if seed() > 0 { P { a: mkp(1).a, b: 1 } } else { P { a: mkp(2).a, b: 2 } };
    return 1;
}
fn main() { println(go()); }
"#,
        &["1"],
        "discarded_branch_literal_aliasing_a_temp_declines",
    );
    // The initializer kinds the guard keeps ADMITTING — values MINTED at
    // the literal: a `.clone()`, a string literal, an interpolated
    // string, and a nested call. Each is registered and freed once.
    //
    // The statements are deliberately STACKED rather than measured one
    // program at a time, and that is the methodological point of this
    // fixture. An earlier cut of the guard admitted `P { a: t.a, .. }`
    // over a local on the strength of single-statement runs that were
    // clean — but a local dies at its NLL last use when nothing follows
    // it, so the alias and the local's own cleanup never coexisted. Add
    // one more discard after it and they do: that program double-freed
    // under a guard tuned on one-statement measurements, where without
    // any registration it merely leaked 38 B. A one-statement fixture
    // cannot see the difference, so this one does not use one.
    assert_clean_asan_run(
        r#"
struct P { a: String, b: i64 }
fn seed() -> i64 { env.args().len() }
fn payload() -> String { f"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa" }
fn mkp(n: i64) -> P { return P { a: payload(), b: n }; }
fn go() -> i64 {
    let u = mkp(8);
    let _ = if seed() > 0 { P { a: u.a.clone(), b: 1 } } else { P { a: payload(), b: 2 } };
    let _ = if seed() > 0 { P { a: "lit-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", b: 1 } } else { P { a: payload(), b: 2 } };
    let _ = if seed() > 0 { P { a: f"x{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", b: 1 } } else { P { a: payload(), b: 2 } };
    let _ = if seed() > 0 { mkp(1) } else { mkp(2) };
    return 1;
}
fn main() { println(go()); }
"#,
        &["1"],
        "discarded_branch_literal_safe_initializer_kinds_still_owned",
    );
    // THE DECLINED KINDS, pinned for what they are. As of B-2026-08-31-44
    // this is the FIELD PROJECTION (`t.a`) alone: it is NOT registered, so
    // it still strands 38 B exactly as it did before that row, and that
    // remains a deliberate trade — registering it double-frees when the
    // local is declared outside a loop and projected on each iteration
    // (see `asan_discarded_branch_literal_field_over_a_loop_outer_local_declines`),
    // and a leak is the better half of that pair.
    //
    // The WHOLE-VALUE move (`s`) is no longer declined: re-measured, it no
    // longer aliases, and declining it was itself the leak. It is kept in
    // this program because the property asserted here is that neither
    // spelling is CORRUPTED, which holds under either decision.
    //
    // ASAN is not the gate for the declined cell — a leak would fail this
    // fixture — so both are exercised through a program whose values ARE
    // freed, to pin that the decline is a decline and not a corruption.
    // The surviving field-projection leak is filed separately.
    assert_clean_asan_run(
        r#"
struct P { a: String, b: i64 }
fn seed() -> i64 { env.args().len() }
fn payload() -> String { f"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa" }
fn mkp(n: i64) -> P { return P { a: payload(), b: n }; }
fn slen(s: String) -> i64 { if s.contains("payload") { s.len() } else { 0 } }
fn go() -> i64 {
    let t = mkp(9);
    let s = payload();
    return slen(t.a) - slen(s) + 1;
}
fn main() { println(go()); }
"#,
        &["1"],
        "discarded_branch_literal_declined_kinds_are_not_corrupted",
    );
}

/// B-2026-09-07-29 — a WHOLE consume inside a loop that actually RUNS
/// double-frees an RC-fallback-promoted local, with no projection anywhere
/// in the program.
///
///     fn takep(p: P) -> i64 { return p.b; }
///     let t = mkp(9); while i < 3i64 { takep(t); i = i + 1; }
///
/// `takep(t)` consumes the WHOLE binding — there is no `t.a` here, which is
/// what separates this from the projection family (B-2026-09-07-19 /
/// -23 / B-2026-09-01-5) and from every disarm those rows touch.
///
/// THE DEFECT IS IN THE TRANSFER GATE, not in the box. The whole-program
/// prepass (`codegen::param_transfer`) admits a by-value struct param as
/// owned by TRANSFER — the callee takes the caller's buffers and the caller
/// retracts its own drop in lockstep — only when EVERY call site passes it
/// in a shape whose caller-side owner can actually be retracted. Its own
/// statement of that shape is "an `Identifier` naming a binding the frame
/// owns, that the ownership pass did not report as read again after this
/// move", and it enforced the second clause by subtracting
/// `use_after_move_consume_sites`.
///
/// But that pass has TWO answers for a consume the source outlives, and
/// `UseAfterMove` is only one of them: for a consume inside a LOOP it
/// RC-FALLBACK PROMOTES the binding instead (`perf[rc-fallback]: RC
/// fallback inserted for 't' (direct re-use after consume)`). Those sites
/// were never subtracted, so the param was admitted and the caller could
/// not pay its half — a promoted binding's alloca holds a `{i64 rc, T}` box
/// HANDLE and its cleanup is an `RcDec`, so `move_transferred_struct_arg`'s
/// retraction finds no `StructDrop` keyed on the binding and silently
/// no-ops, exactly as its own doc says it will. Both frames then free.
///
/// THE TRIP COUNT IS THE AXIS, and it is why five of these cells differ
/// only in it. B-2026-09-07-17 fixed the same `takep(t)`-in-a-loop shape
/// and its regression cells all either never ENTER the loop or carry a user
/// `Drop` — so the executing loop was an untested axis of that fix in
/// exactly the way it was of B-2026-09-07-19's. Measured on the parent at
/// -O0: 25 allocs against 26 / 27 / 28 / 30 frees at 1 / 2 / 3 / 5 trips,
/// with `free(): double free detected in tcache 2` killing the program
/// before it prints, on the JIT and at both optimization levels.
///
/// THE FIX DECLINES THE TRANSFER; it does not move an owner. The box has to
/// go on owning the value — the binding is promoted precisely because the
/// next iteration reads it — which is the same conclusion B-2026-09-07-23
/// reached one shape over. Declining puts the param back on the entry copy,
/// so the callee frees a buffer of its own: every extra free above becomes
/// a matching alloc (26/26, 27/27, 28/28, 30/30).
///
/// CELL 8 IS WHY THAT IS THE RIGHT SHAPE OF FIX rather than a guess. Give
/// `P` an `impl Drop` and the identical program was ALWAYS clean, because
/// `struct_param_transfer_eligible` already declines a type that reaches a
/// user `Drop` — so the entry-copy path was demonstrably handling this
/// shape correctly all along, and only the transfer path was not.
#[test]
fn asan_whole_consume_in_a_running_loop_keeps_the_rc_box_the_only_owner() {
    const OWN: &str = "struct P { a: String, b: i64 }\n\
             fn seed() -> i64 { env.args().len() }\n\
             fn payload() -> String { f\"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa\" }\n\
             fn mkp(n: i64) -> P { return P { a: payload(), b: n }; }\n\
             fn takep(p: P) -> i64 { return p.b; }\n\
             fn main() { println(esc()); }\n";
    // 1 — the row's own spelling. Three trips, three extra frees on the
    // parent, and the program aborts before `println` can run.
    assert_clean_asan_run_min_allocs(
        &format!(
            "{OWN}fn esc() -> String {{ let t = mkp(9); let mut i = 0i64; let mut r = payload();\n\
                 \x20 while i < 3i64 {{ takep(t); i = i + 1; }}\n\
                 \x20 return r; }}\n"
        ),
        &["payload-1-aaaaaaaaaaaaaaaaaaaaaaaaaaaa"],
        "rc_promoted_whole_consume_three_trips",
        10,
    );
    // 2 — ONE trip: the smallest cell that is dirty on the parent, and the
    // one that shows the damage is per-ITERATION rather than per-loop.
    assert_clean_asan_run_min_allocs(
        &format!(
            "{OWN}fn esc() -> String {{ let t = mkp(9); let mut i = 0i64; let mut r = payload();\n\
                 \x20 while i < 1i64 {{ takep(t); i = i + 1; }}\n\
                 \x20 return r; }}\n"
        ),
        &["payload-1-aaaaaaaaaaaaaaaaaaaaaaaaaaaa"],
        "rc_promoted_whole_consume_one_trip",
        10,
    );
    // 3 — FIVE trips: five extra frees on the parent, the other end of the
    // scaling.
    assert_clean_asan_run_min_allocs(
        &format!(
            "{OWN}fn esc() -> String {{ let t = mkp(9); let mut i = 0i64; let mut r = payload();\n\
                 \x20 while i < 5i64 {{ takep(t); i = i + 1; }}\n\
                 \x20 return r; }}\n"
        ),
        &["payload-1-aaaaaaaaaaaaaaaaaaaaaaaaaaaa"],
        "rc_promoted_whole_consume_five_trips",
        10,
    );
    // 4 — the `for` spelling. The promotion is about the consume being
    // inside a LOOP, not about which loop keyword spells it.
    assert_clean_asan_run_min_allocs(
        &format!(
            "{OWN}fn esc() -> String {{ let t = mkp(9); let mut r = payload();\n\
                 \x20 for _k in 0i64..3i64 {{ takep(t); }}\n\
                 \x20 return r; }}\n"
        ),
        &["payload-1-aaaaaaaaaaaaaaaaaaaaaaaaaaaa"],
        "rc_promoted_whole_consume_for_loop",
        10,
    );
    // 5 — NO surviving local. The returned `r` in the cells above is
    // incidental; a freshly-minted return measures identically.
    assert_clean_asan_run_min_allocs(
        &format!(
            "{OWN}fn esc() -> String {{ let t = mkp(9); let mut i = 0i64;\n\
                 \x20 while i < 3i64 {{ takep(t); i = i + 1; }}\n\
                 \x20 return payload(); }}\n"
        ),
        &["payload-1-aaaaaaaaaaaaaaaaaaaaaaaaaaaa"],
        "rc_promoted_whole_consume_fresh_return",
        10,
    );
    // 6 — CONTROL: no loop, so no promotion. `t` is consumed once and the
    // caller's retraction really can fire, which is the population the
    // transfer gate exists to serve. It must keep its transfer.
    assert_clean_asan_run_min_allocs(
        &format!(
            "{OWN}fn esc() -> String {{ let t = mkp(9); let mut r = payload();\n\
                 \x20 takep(t);\n\
                 \x20 return r; }}\n"
        ),
        &["payload-1-aaaaaaaaaaaaaaaaaaaaaaaaaaaa"],
        "rc_promoted_whole_consume_control_no_loop",
        10,
    );
    // 7 — CONTROL: the loop is NEVER ENTERED. Promotion is static, so `t`
    // is boxed here too — but no call happens, so nothing is transferred
    // and this cell was clean on the parent. It is B-2026-09-07-17's own
    // shape, kept so a regression there fails here as well.
    assert_clean_asan_run_min_allocs(
        &format!(
            "{OWN}fn esc() -> String {{ let t = mkp(9); let mut i = 0i64; let mut r = payload();\n\
                 \x20 while i < 0i64 {{ takep(t); i = i + 1; }}\n\
                 \x20 return r; }}\n"
        ),
        &["payload-1-aaaaaaaaaaaaaaaaaaaaaaaaaaaa"],
        "rc_promoted_whole_consume_control_never_entered",
        10,
    );
    // 8 — CONTROL, and the cell that pinned the fix's shape: the same
    // program with an `impl Drop for P`. Transfer eligibility already
    // declines a type that reaches a user `Drop`, so this was clean on the
    // parent at every trip count — the entry-copy path handling the shape
    // correctly while the transfer path did not. The body fires ONCE, from
    // the box, not once per trip.
    assert_clean_asan_run_min_allocs(
        &format!(
            "{OWN}impl Drop for P {{ fn drop(mut ref self) {{ println(f\"dP{{self.b}}\"); }} }}\n\
                 fn esc() -> String {{ let t = mkp(9); let mut i = 0i64; let mut r = payload();\n\
                 \x20 while i < 3i64 {{ takep(t); i = i + 1; }}\n\
                 \x20 return r; }}\n"
        ),
        &["dP9", "payload-1-aaaaaaaaaaaaaaaaaaaaaaaaaaaa"],
        "rc_promoted_whole_consume_control_user_drop",
        10,
    );
    // 9 — CONTROL for the PER-FUNCTION keying. The promotion set is read by
    // the ownership pass's own fn key, so a promoted `t` in one frame must
    // not disqualify a DIFFERENT callee consumed from an unpromoted `t` in
    // another. `takeq` keeps its transfer; both frames stay clean.
    assert_clean_asan_run_min_allocs(
        &format!(
            "{OWN}fn takeq(p: P) -> i64 {{ return p.b; }}\n\
                 fn plain() -> i64 {{ let t = mkp(4); return takeq(t); }}\n\
                 fn esc() -> String {{ let t = mkp(9); let mut i = 0i64; let mut r = payload();\n\
                 \x20 while i < 3i64 {{ takep(t); i = i + 1; }}\n\
                 \x20 println(plain());\n\
                 \x20 return r; }}\n"
        ),
        &["4", "payload-1-aaaaaaaaaaaaaaaaaaaaaaaaaaaa"],
        "rc_promoted_whole_consume_control_other_frame_keeps_transfer",
        10,
    );
    // 10 — the promoted binding in the SECOND parameter slot, beside a
    // fresh temp that disqualifies the first on its own. The gate is keyed
    // by `(callee, index)`, so this asserts the decline lands on the index
    // the promoted argument actually occupies.
    assert_clean_asan_run_min_allocs(
        &format!(
            "{OWN}fn two(x: P, y: P) -> i64 {{ return x.b + y.b; }}\n\
                 fn esc() -> String {{ let t = mkp(9); let mut i = 0i64; let mut r = payload();\n\
                 \x20 while i < 3i64 {{ two(mkp(1), t); i = i + 1; }}\n\
                 \x20 return r; }}\n"
        ),
        &["payload-1-aaaaaaaaaaaaaaaaaaaaaaaaaaaa"],
        "rc_promoted_whole_consume_second_param_slot",
        10,
    );
    // 11 — the promoted binding lives in an IMPL METHOD's frame, not a free
    // function's. The promotion set is keyed the way the ownership pass
    // keys it -- bare name for a free fn, `Type.method` for a method -- and
    // this cell is what makes that arm load-bearing rather than assumed.
    // Verified by neutering just that key and rebuilding: this program goes
    // to 25 allocs / 28 frees with 3 errors at three trips and 25/30 with 5
    // at five, aborting before it prints, exactly as the free-function
    // spelling does. With the key intact it is flat 24/24 at every trip
    // count -- note this frame lands on caller-retains rather than on the
    // entry copy the free-function cells take, which is a different
    // non-transfer path and equally correct.
    assert_clean_asan_run_min_allocs(
            &format!(
                "{OWN}struct Runner {{ id: i64 }}\n\
                 impl Runner {{\n\
                 \x20 fn drive(ref self) -> String {{ let t = mkp(9); let mut i = 0i64; let mut r = payload();\n\
                 \x20   while i < 3i64 {{ takep(t); i = i + 1; }}\n\
                 \x20   return r; }}\n\
                 }}\n\
                 fn esc() -> String {{ let q = Runner {{ id: 1 }}; return q.drive(); }}\n"
            ),
            &["payload-1-aaaaaaaaaaaaaaaaaaaaaaaaaaaa"],
            "rc_promoted_whole_consume_inside_an_impl_method",
            10,
        );
}

/// B-2026-09-07-23 — a heap field PROJECTED out of an RC-FALLBACK-PROMOTED
/// local was freed by the destination AND by the box, once per loop
/// iteration.
///
/// Every disarm reaches the source field by GEP-ing the binding's slot
/// (`suppress_struct_field_move_by_name` and its place-shaped peers). A
/// promoted binding's slot holds a `{i64 rc, T}` box HANDLE rather than the
/// struct, so each one bails on its shape test and the cap is never zeroed;
/// the destination then takes ownership the box has not given up. Inside a
/// loop that is once per ITERATION, so the damage scales with the trip
/// count — measured 1 / 3 / 5 extra frees at 1 / 3 / 5 trips, and
/// `free(): double free detected in tcache 2` on every compiled backend.
///
/// THE TRIP COUNT IS THE AXIS, which is why five of these cells differ only
/// in it. The parent row B-2026-09-07-19 was filed against a NEVER-ENTERED
/// loop and reported 17 allocations against 22 frees; the never-entered
/// program is in fact 17/17 clean (at `9a50182` and at `b7626d6` alike, so
/// nothing fixed it in between) and the 17/22 is the FIVE-trip program,
/// reproduced exactly by the fifth cell below. That row closed as fixed in
/// passing; this one is its executing-loop sibling, and the never-entered
/// cell is kept here as a CONTROL rather than as the defect.
///
/// THE STRUCT LITERAL IN BOTH ROWS' TITLES IS INCIDENTAL. The bare
/// projection `let s = t.a`, with no literal anywhere in the program,
/// measures identically, and is the second cell here for that reason.
///
/// THE FIX COPIES; IT DOES NOT RETRACT THE DESTINATION'S OWNERSHIP, and
/// both rejected alternatives are worth keeping because each looks right:
///
///   * Zeroing the box's own field through the handle would neutralize the
///     source, but the box is the surviving owner precisely because the
///     binding is re-used after the consume — the SECOND iteration would
///     read a zeroed String. The interpreter is the oracle and reads the
///     field's full 38 bytes on every iteration (cell 6 asserts it).
///   * Declining the destination's registration — which B-2026-09-07-23's
///     own prose prescribes as "the field-view direction" — leaks as soon
///     as the binding is MUTATED: `push_str` reallocs into a fresh buffer
///     that then has no owner at all. Measured on the way through: 228 B
///     definitely lost in 3 blocks, with the 2 invalid frees still in
///     place. Cell 7 is that shape and is why the copy is the answer.
#[test]
fn asan_rc_boxed_projection_copies_instead_of_sharing_an_owner() {
    const OWN: &str = "struct P { a: String, b: i64 }\n\
             fn seed() -> i64 { env.args().len() }\n\
             fn payload() -> String { f\"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa\" }\n\
             fn mkp(n: i64) -> P { return P { a: payload(), b: n }; }\n\
             fn takep(p: P) -> i64 { return p.b; }\n\
             fn main() { println(go()); }\n";
    // 1 — the row's own spelling, with the loop ENTERED. Three trips, three
    // extra frees on the parent.
    assert_clean_asan_run_min_allocs(
        &format!(
            "{OWN}fn go() -> i64 {{ let t = mkp(9); let mut i = 0i64;\n\
                 \x20 while i < 3i64 {{ let p = P {{ a: t.a, b: 1 }}; i = i + p.b; }}\n\
                 \x20 return 1; }}\n"
        ),
        &["1"],
        "rc_boxed_proj_literal_loop_entered",
        // 8, measured on both hosts; the 10 was an estimate. See
        // B-2026-09-07-26 and [`asan_alloc_floor`].
        8,
    );
    // 2 — NO LITERAL. The defect is the projection, not the literal.
    assert_clean_asan_run_min_allocs(
        &format!(
            "{OWN}fn go() -> i64 {{ let t = mkp(9); let mut i = 0i64;\n\
                 \x20 while i < 3i64 {{ let s = t.a; i = i + 1; }}\n\
                 \x20 return 1; }}\n"
        ),
        &["1"],
        "rc_boxed_proj_bare_no_literal",
        // 8, measured on both hosts; the 10 was an estimate. See
        // B-2026-09-07-26 and [`asan_alloc_floor`].
        8,
    );
    // 3 — ONE trip. The smallest cell that is dirty on the parent, and the
    // one that shows the damage is per-iteration rather than per-loop.
    assert_clean_asan_run_min_allocs(
        &format!(
            "{OWN}fn go() -> i64 {{ let t = mkp(9); let mut i = 0i64;\n\
                 \x20 while i < 1i64 {{ let p = P {{ a: t.a, b: 1 }}; i = i + p.b; }}\n\
                 \x20 return 1; }}\n"
        ),
        &["1"],
        "rc_boxed_proj_one_trip",
        // AUDITED, per cell (B-2026-09-07-26). Every floor in this family is
        // now the count `KARAC_ASAN_ALLOC_AUDIT=1` reports for that exact
        // cell, not a family-wide estimate: most sit at 8, the `Drop`-body
        // cells at 10, `rc_boxed_proj_mutated_destination` at 15 and
        // `rc_fb_twin_shape_both_boxed` at 183. Until the predicate became
        // floor-relative none of them could be checked — the comparison was
        // against ASAN's raw process-wide count, whose host start-up floor
        // (10 arm64 Linux, 199 macOS) exceeds most of these numbers on its
        // own. See [`asan_alloc_floor`].
        8,
    );
    // 4 — the `for` spelling, a different loop lowering onto the same
    // promotion.
    assert_clean_asan_run_min_allocs(
        &format!(
            "{OWN}fn go() -> i64 {{ let t = mkp(9); let mut n = 0i64;\n\
                 \x20 for k in 0..3 {{ let p = P {{ a: t.a, b: 1 }}; n = n + p.b; }}\n\
                 \x20 return n; }}\n"
        ),
        &["3"],
        "rc_boxed_proj_for_loop",
        // AUDITED, per cell (B-2026-09-07-26). Every floor in this family is
        // now the count `KARAC_ASAN_ALLOC_AUDIT=1` reports for that exact
        // cell, not a family-wide estimate: most sit at 8, the `Drop`-body
        // cells at 10, `rc_boxed_proj_mutated_destination` at 15 and
        // `rc_fb_twin_shape_both_boxed` at 183. Until the predicate became
        // floor-relative none of them could be checked — the comparison was
        // against ASAN's raw process-wide count, whose host start-up floor
        // (10 arm64 Linux, 199 macOS) exceeds most of these numbers on its
        // own. See [`asan_alloc_floor`].
        8,
    );
    // 5 — FIVE trips: the 17-allocs-against-22-frees B-2026-09-07-19 was
    // filed with, which is how those numbers were traced to a running loop
    // rather than the never-entered one its prose quotes.
    assert_clean_asan_run_min_allocs(
        &format!(
            "{OWN}fn go() -> i64 {{ let t = mkp(9); let mut i = 0i64;\n\
                 \x20 while i < 5i64 {{ let p = P {{ a: t.a, b: 1 }}; i = i + p.b; }}\n\
                 \x20 return 1; }}\n"
        ),
        &["1"],
        "rc_boxed_proj_five_trips",
        // AUDITED, per cell (B-2026-09-07-26). Every floor in this family is
        // now the count `KARAC_ASAN_ALLOC_AUDIT=1` reports for that exact
        // cell, not a family-wide estimate: most sit at 8, the `Drop`-body
        // cells at 10, `rc_boxed_proj_mutated_destination` at 15 and
        // `rc_fb_twin_shape_both_boxed` at 183. Until the predicate became
        // floor-relative none of them could be checked — the comparison was
        // against ASAN's raw process-wide count, whose host start-up floor
        // (10 arm64 Linux, 199 macOS) exceeds most of these numbers on its
        // own. See [`asan_alloc_floor`].
        8,
    );
    // 6 — READS the projected field on every iteration. This is the cell
    // that rules out disarming the box's own field: the length has to be 38
    // every trip, so `i` reaches 3 and the program prints 1. Zero the box's
    // field on trip one and this prints something else.
    assert_clean_asan_run_min_allocs(
        &format!(
            "{OWN}fn go() -> i64 {{ let t = mkp(9); let mut i = 0i64;\n\
                 \x20 while i < 3i64 {{ let p = P {{ a: t.a, b: 1 }}; i = i + p.a.len() - 37; }}\n\
                 \x20 return 1; }}\n"
        ),
        &["1"],
        "rc_boxed_proj_field_read_each_trip",
        // AUDITED, per cell (B-2026-09-07-26). Every floor in this family is
        // now the count `KARAC_ASAN_ALLOC_AUDIT=1` reports for that exact
        // cell, not a family-wide estimate: most sit at 8, the `Drop`-body
        // cells at 10, `rc_boxed_proj_mutated_destination` at 15 and
        // `rc_fb_twin_shape_both_boxed` at 183. Until the predicate became
        // floor-relative none of them could be checked — the comparison was
        // against ASAN's raw process-wide count, whose host start-up floor
        // (10 arm64 Linux, 199 macOS) exceeds most of these numbers on its
        // own. See [`asan_alloc_floor`].
        8,
    );
    // 7 — the projected binding is MUTATED. The copy must be independent:
    // `push_str` reallocs, and with the destination's registration declined
    // instead of copied that fresh buffer leaked (228 B in 3 blocks).
    assert_clean_asan_run_min_allocs(
            &format!(
                "{OWN}fn go() -> i64 {{ let t = mkp(9); let mut i = 0i64; let mut n = 0i64;\n\
                 \x20 while i < 3i64 {{ let mut s = t.a; s.push_str(\"XY\"); n = s.len(); i = i + 1; }}\n\
                 \x20 return n; }}\n"
            ),
            &["40"],
            "rc_boxed_proj_mutated_destination",
            // AUDITED, per cell (B-2026-09-07-26). Every floor in this family is
            // now the count `KARAC_ASAN_ALLOC_AUDIT=1` reports for that exact
            // cell, not a family-wide estimate: most sit at 8, the `Drop`-body
            // cells at 10, `rc_boxed_proj_mutated_destination` at 15 and
            // `rc_fb_twin_shape_both_boxed` at 183. Until the predicate became
            // floor-relative none of them could be checked — the comparison was
            // against ASAN's raw process-wide count, whose host start-up floor
            // (10 arm64 Linux, 199 macOS) exceeds most of these numbers on its
            // own. See [`asan_alloc_floor`].
            15,
        );
    // 8 — CONTROL: the loop is never entered (B-2026-09-07-19's own cell).
    // Clean on the parent and must stay clean.
    assert_clean_asan_run_min_allocs(
        &format!(
            "{OWN}fn go() -> i64 {{ let t = mkp(9); let mut i = 0i64;\n\
                 \x20 while i < 0i64 {{ let p = P {{ a: t.a, b: 1 }}; i = i + p.b; }}\n\
                 \x20 return 1; }}\n"
        ),
        &["1"],
        "rc_boxed_proj_never_entered_control",
        // AUDITED, per cell (B-2026-09-07-26). Every floor in this family is
        // now the count `KARAC_ASAN_ALLOC_AUDIT=1` reports for that exact
        // cell, not a family-wide estimate: most sit at 8, the `Drop`-body
        // cells at 10, `rc_boxed_proj_mutated_destination` at 15 and
        // `rc_fb_twin_shape_both_boxed` at 183. Until the predicate became
        // floor-relative none of them could be checked — the comparison was
        // against ASAN's raw process-wide count, whose host start-up floor
        // (10 arm64 Linux, 199 macOS) exceeds most of these numbers on its
        // own. See [`asan_alloc_floor`].
        8,
    );
    // 9 — CONTROL: no loop, so no promotion. The disarm works and the
    // destination legitimately OWNS; the copy must not fire here.
    assert_clean_asan_run_min_allocs(
        &format!(
            "{OWN}fn go() -> i64 {{ let t = mkp(9); let p = P {{ a: t.a, b: 1 }};\n\
                 \x20 return p.b; }}\n"
        ),
        &["1"],
        "rc_boxed_proj_no_promotion_control",
        // AUDITED, per cell (B-2026-09-07-26). Every floor in this family is
        // now the count `KARAC_ASAN_ALLOC_AUDIT=1` reports for that exact
        // cell, not a family-wide estimate: most sit at 8, the `Drop`-body
        // cells at 10, `rc_boxed_proj_mutated_destination` at 15 and
        // `rc_fb_twin_shape_both_boxed` at 183. Until the predicate became
        // floor-relative none of them could be checked — the comparison was
        // against ASAN's raw process-wide count, whose host start-up floor
        // (10 arm64 Linux, 199 macOS) exceeds most of these numbers on its
        // own. See [`asan_alloc_floor`].
        8,
    );
    // 10 — CONTROL: the projection as a CALL ARGUMENT was already clean
    // before this fix and must stay so.
    assert_clean_asan_run_min_allocs(
        &format!(
            "{OWN}fn takes(s: String) -> i64 {{ return s.len(); }}\n\
                 fn go() -> i64 {{ let t = mkp(9); let mut i = 0i64;\n\
                 \x20 while i < 3i64 {{ takes(t.a); i = i + 1; }}\n\
                 \x20 return 1; }}\n"
        ),
        &["1"],
        "rc_boxed_proj_call_argument_control",
        // AUDITED, per cell (B-2026-09-07-26). Every floor in this family is
        // now the count `KARAC_ASAN_ALLOC_AUDIT=1` reports for that exact
        // cell, not a family-wide estimate: most sit at 8, the `Drop`-body
        // cells at 10, `rc_boxed_proj_mutated_destination` at 15 and
        // `rc_fb_twin_shape_both_boxed` at 183. Until the predicate became
        // floor-relative none of them could be checked — the comparison was
        // against ASAN's raw process-wide count, whose host start-up floor
        // (10 arm64 Linux, 199 macOS) exceeds most of these numbers on its
        // own. See [`asan_alloc_floor`].
        8,
    );
}

#[test]
fn asan_shared_struct_container_field_reassign_no_leak() {
    // B-2026-08-14-18: reassigning a `mut` CONTAINER field of a
    // `shared struct` never freed the container it displaced. The plain
    // `build_store` on that path was deliberate — "a shared parent keeps its
    // raw store (its fields ride the RC node teardown)" — and that reasoning
    // holds for the field's FINAL occupant while saying nothing about one an
    // assignment threw away: the teardown never sees that value.
    //
    // LOOPED 20 times because the loss is PER ASSIGNMENT, not a fixed
    // header. Pre-fix a single `Vec[Node]` reassign stranded 88 bytes in 2
    // allocations and a one-entry `Map[String, i64]` 600 in 3; the loop
    // multiplies that per container, so a partial fix cannot hide in the
    // noise. The containers are re-filled each iteration so every displaced
    // one owns element heap, which is what makes the loss scale with
    // CONTENTS rather than stopping at the control block.
    //
    // The `label: String` reassign is an OVER-REACH GUARD, not a witness: a
    // String field on this path was already released correctly (measured
    // against the pre-fix compiler), so a fix that released it a second time
    // shows up here as a double free rather than a pass. It is declared
    // FIRST deliberately — the four-field order `Vec, Map, Set, String` has
    // a separate, pre-existing, layout-dependent String leak that is not
    // this row's and would make the fixture assert two things at once.
    assert_clean_asan_run(
        r#"
shared struct Node { name: String, mut size: i64, mut children: Vec[Node] }
shared struct Reg { mut label: String, mut tags: Vec[String], mut m: Map[String, i64], mut s: Set[String] }
fn main() {
    let root = Node { name: "rootrootrootroot", size: 1, children: Vec.new() };
    let kid = Node { name: "kidkidkidkidkidk", size: 2, children: Vec.new() };
    root.children.push(kid);
    let r = Reg { label: "seedseedseedseedseed", tags: Vec.new(), m: Map.new(), s: Set.new() };
    let mut k = 0;
    while k < 20 {
        r.tags.push("alphabetalphabetalphabet");
        let _ = r.m.insert("gammagammagammagammagam", k);
        let _ = r.s.insert("deltadeltadeltadeltadel");

        let fresh_t: Vec[String] = Vec.new();
        let fresh_m: Map[String, i64] = Map.new();
        let fresh_s: Set[String] = Set.new();

        r.tags = fresh_t;
        r.m = fresh_m;
        r.s = fresh_s;
        k = k + 1;
    }
    let kept: Vec[Node] = Vec.new();
    root.children = kept;
    println(f"{r.tags.len()} {r.m.len()} {r.s.len()} {root.children.len()}");
}
"#,
        &["0 0 0 0"],
        "asan_shared_struct_container_field_reassign_no_leak",
    );
}

#[test]
fn asan_shared_struct_container_field_reassign_aliasing() {
    // B-2026-08-14-18, the other direction: freeing the displaced container
    // must not free one that is still reachable. Three shapes, each of which
    // an over-eager release turns into a double free or a use-after-free
    // rather than a leak — and each printing a value read AFTER the
    // assignment, so a freed-but-stored buffer is caught by the read and not
    // only by LSan.
    //
    // The SELF-ASSIGN is the one that overturned the obvious reasoning.
    // Releasing before the store looks unsafe for `b.v = b.v` — free the
    // buffer, then store a pointer to it back — which is the hazard the
    // `Option[shared]` field store's retain-then-store-then-release order
    // exists to dodge. It does not arise for a container, because reading
    // one out of a shared node yields an independent copy. A guard that
    // declined when the RHS mentioned the base was written first and LEAKED
    // 96 bytes here; the unconditional release is clean and leaves the
    // element readable, which is why the guard was removed.
    //
    // The last shape is the tree this bug came from: `shared struct Node`
    // with a `weak parent` back-edge, pruned by replacing `children`
    // wholesale — the ordinary way to filter a container field, and the one
    // statement that leaked the entire discarded subtree.
    assert_clean_asan_run(
        r#"
shared struct B { mut v: Vec[String] }
shared struct Node { name: String, mut size: i64, mut children: Vec[Node], mut parent: weak Node }
fn main() {
    let b = B { v: Vec.new() };
    b.v.push("alphabetalphabetalphabet");
    b.v = b.v;
    println(f"{b.v.len()} {b.v[0].len()}");
    let x = B { v: Vec.new() };
    let y = B { v: Vec.new() };
    x.v.push("gammagammagammagammagam");
    y.v.push("deltadeltadeltadeltadel");
    x.v = y.v;
    println(f"{x.v.len()} {x.v[0].len()}");
    let root = Node { name: "rootrootrootroot", size: 10, children: Vec.new(), parent: None };
    let a = Node { name: "aaaaaaaaaaaaaaaaaaa", size: 20, children: Vec.new(), parent: None };
    let c = Node { name: "cccccccccccccccccc", size: 30, children: Vec.new(), parent: None };
    a.parent = root;
    c.parent = root;
    root.children.push(a);
    root.children.push(c);
    let mut kept: Vec[Node] = Vec.new();
    kept.push(c);
    root.children = kept;
    println(f"{root.children.len()} {root.children[0].size}");
}
"#,
        &["1 24", "1 23", "1 30"],
        "asan_shared_struct_container_field_reassign_aliasing",
    );
}

/// B-2026-08-14-26, memory half — the container a CHAINED shared field
/// assignment displaces.
///
/// B-2026-08-14-18 gave the DEPTH-1 shared store this release. The nested
/// one could not have needed it before: that branch never reached its store
/// for a shared parent, so nothing was ever displaced. Making the write land
/// is what makes the old value's fate observable — and without the release a
/// two-element `Vec[String]` reassigned through the chain strands 96 bytes,
/// once per assignment.
///
/// Output comparison cannot see this. Every surface prints `0` for the
/// re-read length either way; only LeakSanitizer separates a store that
/// lands and frees from one that lands and strands. The loop is here so the
/// per-assignment cost compounds into something no allocator coincidence
/// hides.
#[test]
fn asan_chained_shared_field_assignment_releases_the_displaced_container() {
    assert_clean_asan_run(
        r#"
shared struct Inner { mut n: i64, mut v: Vec[String] }
shared struct Outer { mut inner: Inner }

fn main() {
    let o = Outer { inner: Inner { n: 0, v: Vec.new() } };
    let mut i = 0i64;
    while i < 12i64 {
        let a = o.inner;
        a.v.push("one");
        a.v.push("two");
        let fresh: Vec[String] = Vec.new();
        o.inner.v = fresh;
        o.inner.n = i;
        i = i + 1;
    }
    let b = o.inner;
    println(f"{b.v.len()} {b.n}");
    println("end");
}
"#,
        &["0 11", "end"],
        "chained_shared_field_assignment_displaced_container",
    );
}

/// B-2026-08-17-23 — a HEAP-CARRYING struct destructured in parameter
/// position. The fix binds the pattern's leaves through the same
/// `bind_pattern` choke point `let (a, b) = p;` uses, which means the
/// leaves take ownership of the parameter's heap fields exactly as a
/// body-level destructure would; this pins that the resulting
/// retain/release balance is clean rather than merely that the program
/// prints the right thing. Loops so a per-call imbalance accumulates
/// into something LSan cannot miss.
#[test]
fn asan_destructured_heap_struct_param() {
    assert_clean_asan_run(
        r#"struct H { s: String, n: i64 }
struct P { a: String, b: String }
fn take(H { s, n }: H) -> String { s + n.to_string() }
fn both(P { a, b }: P) -> String { a + b }
fn pair((l, r): (String, String)) -> String { l + r }
fn main() {
    let mut i = 0i64;
    let mut last: String = "";
    while i < 40i64 {
        last = take(H { s: "v", n: i });
        last = both(P { a: "x", b: "y" });
        last = pair(("p", "q"));
        i = i + 1;
    }
    println(last);
}
"#,
        &["pq"],
        "asan_destructured_heap_struct_param",
    );
}

/// B-2026-08-21-38 (codegen half) — a `mut ref` METHOD parameter fed a
/// heap-bearing struct FIELD. Before the fix the callee received a pointer
/// to a temporary COPY of the field, so the `Vec[String]` had two owners:
/// the copy's cleanup freed the buffer the caller's `Box` still owned and
/// freed again at scope exit. Measured on this fixture as
/// `AddressSanitizer: double-free` — the write-back miscompile and a
/// memory-safety defect were the same bug.
///
/// With the place pointer the callee mutates the caller's field directly,
/// which is a BORROW: the caller keeps sole ownership and frees once. The
/// free-function spelling in the same program is the control — it has
/// taken this path since B-2026-08-05-41 — so a report naming only the
/// method half points at this transplant. The loop makes a per-call
/// imbalance accumulate rather than hide in one iteration, and `String`
/// elements put the element bodies on the same hook as the buffer.
#[test]
fn asan_mut_ref_heap_field_into_a_method_is_a_borrow() {
    assert_clean_asan_run(
        r#"struct Box { rows: Vec[String] }
struct H { n: i64 }
impl H {
    fn grow(ref self, v: mut ref Vec[String]) { v.push("row"); }
}
fn free_grow(v: mut ref Vec[String]) { v.push("row"); }
fn main() {
    let mut i = 0i64;
    let mut total = 0i64;
    while i < 50i64 {
        let mut b = Box { rows: ["seed"] };
        free_grow(mut b.rows);
        let h = H { n: 0 };
        h.grow(mut b.rows);
        total = total + b.rows.len();
        i = i + 1;
    }
    println(total);
}
"#,
        &["150"],
        "asan_mut_ref_heap_field_into_a_method_is_a_borrow",
    );
}

/// B-2026-08-22-12 — a return-position `impl Trait` whose witness is a
/// `shared struct` (RC), driven in a loop.
///
/// The fix routes the existential's value onto the WITNESS's ordinary
/// codegen path, which for a `shared struct` means the retain/release
/// discipline. That is exactly where a substitution can be subtly wrong in
/// a way no output comparison catches: hand the backend a concrete type it
/// half-recognises and the balancing release can go missing, leaking one
/// node per call while every printed line stays correct.
///
/// Both call spellings are in the loop, because they reach codegen
/// differently: the bound one through `var_type_names`, the direct one as
/// a fresh temporary whose owner is the statement. The loop is what turns
/// a per-call imbalance into an accumulating leak rather than a single
/// block LSan might not distinguish from startup noise.
#[test]
fn asan_impl_trait_witness_shared_struct_is_balanced() {
    assert_clean_asan_run(
        r#"trait Named { fn label(ref self) -> String; }

shared struct Node { name: String }
impl Named for Node {
    fn label(ref self) -> String { self.name }
}

fn a_node(i: i64) -> impl Named { Node { name: f"n{i}" } }

fn main() {
    let mut i = 0i64;
    let mut total = 0i64;
    let mut last: String = "";
    while i < 50i64 {
        let held = a_node(i);
        total = total + held.label().len();
        last = a_node(i).label();
        i = i + 1;
    }
    println(last);
    println(total);
}
"#,
        &["n49", "140"],
        "asan_impl_trait_witness_shared_struct_is_balanced",
    );
}

/// B-2026-08-23-18 — `dbg(x)` on a PLACE expression of every heap-owning
/// shape, under ASAN/LSan.
///
/// `dbg` does not consume its argument (it is classified `Ref` alongside the
/// print family, because a construct stripped from release builds must not
/// change what a program means by being present), so the value it hands back
/// cannot be the argument's own descriptor — the binding still frees the
/// buffer at scope exit while the returned temporary gets a cleanup of its
/// own. Before the owned-copy fix, `dbg(vs)` on a `Vec` and `dbg(hs)` on a
/// heap `String` aborted with "double free detected in tcache 2", `dbg(mp)`
/// on a `Map` SEGFAULTED, and `Option[String]` aborted the same way. Each
/// shape is exercised both discarded and bound, since the two reach
/// different cleanup paths.
#[test]
fn test_dbg_of_place_expression_double_frees_no_heap_shape() {
    assert_clean_asan_run(
        r#"
struct Pair { a: i64, b: String }
fn main() {
    let mut vs: Vec[i64] = Vec.new();
    vs.push(1_i64);
    dbg(vs);
    let vs2 = dbg(vs);
    println(f"{vs2.len()}");

    let hs = "x".to_uppercase();
    dbg(hs);
    let hs2 = dbg(hs);
    println(hs2);

    let mut mp: Map[String, i64] = Map.new();
    mp.insert("k", 7_i64);
    dbg(mp);
    let mp2 = dbg(mp);
    println(f"{mp2.len()}");

    let op: Option[String] = Some("s".to_uppercase());
    dbg(op);
    let op2 = dbg(op);
    match op2 {
        Some(v) => println(v),
        None => println("none"),
    }

    let pr = Pair { a: 1_i64, b: "t".to_uppercase() };
    dbg(pr);
    println(f"{pr.a}");
}
"#,
        &["1", "X", "1", "S", "1"],
        "dbg_place_expression_heap_shapes",
    );
}

/// B-2026-08-24-2 — ASAN/LSan COVERAGE for `dbg` of a `shared struct`
/// handle, a shape that could not be compiled at all until the renderer
/// landed, so nothing in this suite had ever exercised it.
///
/// READ THE NEXT PARAGRAPH BEFORE TRUSTING THIS TEST. It is coverage, NOT
/// the regression guard for the refcount bug that landed alongside it:
/// MEASURED to pass both with and without that fix, at `-O2` and at
/// `KARAC_OPT_LEVEL=0`. The AOT leg this harness builds is balanced either
/// way; the premature free was observable only under the JIT, as
/// `malloc(): unaligned tcache chunk detected`. The guard that actually
/// fails is `test_dbg_of_a_shared_struct_renders_the_same_on_all_three_backends`
/// in `tests/cli.rs`, which compares JIT stderr byte for byte and so sees
/// the allocator's complaint as a diff. Do not "strengthen" this one by
/// asserting on the bug; strengthen the JIT comparison instead.
///
/// What it does earn: a shared handle owns a REFCOUNT rather than a buffer,
/// so `dbg`'s owned copy has to RETAIN rather than memcpy, and this pins
/// that the retaining path is free of the ordinary leak/double-free faults
/// on the AOT surface — including the niche `Option[shared]` layout, where
/// `None` is a null handle and the retain must be null-guarded.
#[test]
fn test_dbg_of_a_shared_struct_place_expression_is_refcount_balanced() {
    assert_clean_asan_run(
        r#"
shared struct Flat { a: i64, b: String }
shared struct Link { v: i64, mut next: Option[Link] }
fn main() {
    let f = Flat { a: 1_i64, b: "t".to_uppercase() };
    dbg(f);
    let f2 = dbg(f);
    println(f"{f2.a}");

    let tail = Link { v: 2_i64, next: None };
    let head = Link { v: 1_i64, next: Some(tail) };
    dbg(head);
    let head2 = dbg(head);
    println(f"{head2.v}");
}
"#,
        &["1", "1"],
        "dbg_shared_struct_place_expression",
    );
}

/// B-2026-08-24-21 — a `shared` value allocated on EVERY iteration and
/// broken out on a later one. The breaking path hands its ref to the
/// receiver at net +1; the iterations that fall through must release
/// theirs.
///
/// This is an RC fixture, so BOTH directions matter and only ASAN sees
/// them: too few decs leaks the unbroken iterations' nodes, too many frees
/// the one that escaped. The shape is also the one that bus-errored under
/// the wrong mechanism (disarm instead of retain), so a regression to that
/// mechanism fails here rather than silently.
#[test]
fn asan_loop_break_shared_struct_releases_unbroken_iterations() {
    assert_clean_asan_run_min_allocs(
        "shared struct Node { v: i64 }\n\
             fn pick() -> Node {\n\
             \x20   let mut i: i64 = env.args().len() - 1;\n\
             \x20   loop {\n\
             \x20       i = i + 1;\n\
             \x20       let n = Node { v: i * 11 };\n\
             \x20       if i == 3 { break n }\n\
             \x20   }\n\
             }\n\
             fn main() { println(pick().v); }\n",
        &["33"],
        "loop-break-shared-struct",
        5,
    );
}

/// B-2026-08-25-1 — the RVALUE carrier: a `shared` struct literal built
/// directly in the `break`, with no binding to key any mechanism on.
///
/// The transfer emits NO ownership action, which is the whole claim of the
/// fix, so this fixture exists to prove that is not merely a leak in
/// disguise. Every iteration allocates a `scratch` node the drain must
/// still free, and one more node escapes through the break: too few decs
/// leaks the three scratches, too many frees the one that got out. The
/// escaping node is also READ after `pick` returns, so a premature free is
/// a use-after-free ASAN reports rather than a silent wrong number.
#[test]
fn asan_loop_break_shared_struct_rvalue_single_owner() {
    assert_clean_asan_run_min_allocs(
        "shared struct Node { v: i64 }\n\
             fn pick() -> Node {\n\
             \x20   let mut i: i64 = env.args().len() - 1;\n\
             \x20   loop {\n\
             \x20       i = i + 1;\n\
             \x20       let scratch = Node { v: i };\n\
             \x20       if i == 3 { break Node { v: scratch.v * 11 } }\n\
             \x20   }\n\
             }\n\
             fn main() { println(pick().v); }\n",
        &["33"],
        "loop-break-shared-struct-rvalue",
        4,
    );
}

/// B-2026-08-29-13 — an arm/branch tail that hands out a bound `shared`
/// value TRANSFERS its ref, and the consuming `let` was taking a second.
///
/// The transfer is deliberate and correct: `suppress_source_vec_cleanup_
/// for_arg_ex`'s shared arm balances the source's queued dec with a fresh
/// inc, so "the consumer becomes the new owner of one ref". What was
/// missing is the other half — the `block_tail_shared_transfer` record the
/// let site reads to SKIP its own receive-inc. It existed for a direct
/// `shared` FIELD tail (B-2026-08-06-15) and for nothing else, so a bound
/// shared identifier handed out of a match arm or an if-let branch was
/// counted twice and the box never reached zero.
///
/// Traced, `let keep = match t { .. Bin(l, r) => l };` over a recursive
/// `shared enum`: three incs (arm binding, transfer, let) against three
/// decs (arm, let, the parent's drop walk) over a birth of 1 — final
/// refcount 1, one box lost. The RETURN spelling was already clean because
/// a `Call` RHS is fresh, so the caller's binding never took the extra inc;
/// that asymmetry is what made this look like a freshness bug when it is
/// not.
///
/// TWO CAUSES, and the rows below separate them:
///  * a CONSUMED result (`let keep = …`) needed the transfer RECORDED, at
///    three leaf sites — the match arm tail, the value-position block tail,
///    and an if-let's THEN branch, which is compiled in `compile_if_let`
///    and reaches neither of the other two (the same "third leaf hook" gap
///    B-2026-08-27-34 records for `Option[shared T]`).
///  * a DISCARDED construct has no consumer at all, so the transfer inc is
///    never spent. Those rows gate the inc on `branch_value_is_owned`, the
///    same predicate the boxed-payload neutralizer nearby already uses for
///    "a DISCARDED match result has no destination".
///
/// Unlike B-2026-08-28-74 and B-2026-08-29-12, this leak SURVIVES -O2 on
/// the recursive shape (measured pre-fix: 11 allocs / 10 frees, 32 B lost
/// at the level this suite compiles at), so these rows assert here without
/// needing a container to defeat the optimizer.
#[test]
fn asan_branch_tail_shared_transfer_is_not_double_counted() {
    let rows: [(&str, &str, &str); 8] = [
            // CONSUMED — the row's minimal repro, and its variants.
            (
                "let t = mk(); let keep = match t { Num(a) => E.Num(a), Bin(l, r) => l }; println(9);",
                "9",
                "shared-child-match-bound",
            ),
            (
                "let t = mk(); let keep = match t { Num(a) => E.Num(a), Bin(l, r) => l };                  match keep { Num(a) => println(a), Bin(l, r) => println(9) }",
                "1",
                "shared-child-match-bound-read",
            ),
            (
                "let keep = match mk() { Num(a) => E.Num(a), Bin(l, r) => l }; println(9);",
                "9",
                "shared-child-match-freshtemp",
            ),
            (
                "let t = mk(); let keep = if let Bin(l, r) = t { l } else { E.Num(0) }; println(9);",
                "9",
                "shared-child-iflet-bound",
            ),
            (
                "let t = mk(); let keep = { match t { Num(a) => E.Num(a), Bin(l, r) => l } }; println(9);",
                "9",
                "shared-child-block-wrapped",
            ),
            // DISCARDED — no consumer, so the transfer inc must not be emitted.
            (
                "let t = mk(); match t { Num(a) => E.Num(a), Bin(l, r) => l }; println(9);",
                "9",
                "shared-child-match-discarded",
            ),
            (
                "let t = mk(); if let Bin(l, r) = t { l } else { E.Num(0) }; println(9);",
                "9",
                "shared-child-iflet-discarded",
            ),
            // The RETURN control: already clean, and the row that would fail if
            // the record over-reached and suppressed a genuinely needed inc.
            (
                "let k = pick(); match k { Num(a) => println(a), Bin(l, r) => println(9) }",
                "1",
                "shared-child-returned-control",
            ),
        ];
    for (body, expected, label) in rows {
        let src = format!(
                "shared enum E {{ Num(i64), Bin(E, E) }}\n\
                 fn mk() -> E {{ return E.Bin(E.Num(1), E.Num(2)); }}\n\
                 fn pick() -> E {{ let t = mk(); return match t {{ Num(a) => E.Num(a), Bin(l, r) => l }}; }}\n\
                 fn main() {{ {body} }}\n"
            );
        // 3 — `mk()`'s three boxes. Every row here measures exactly that,
        // audited, on macOS and arm64 Linux alike: the recursive
        // `E.Bin(E.Num(1), E.Num(2))` is the whole of this program's heap
        // and no spelling of the consume adds to it. What the floor is for
        // is unchanged — at 0 the boxes have been folded away and the
        // refcount claim has nothing to stand on.
        //
        // It read 8 until B-2026-09-07-26, when the predicate became
        // floor-relative. Nothing could have caught that: the old comparison
        // was against ASAN's raw process-wide count, which carries a host
        // start-up floor of 10 (arm64 Linux) or 199 (macOS) — larger, by
        // itself, than the threshold it was being checked against. See
        // [`asan_alloc_floor`].
        assert_clean_asan_run_min_allocs(&src, &[expected], label, 3);
    }
}

/// B-2026-08-25-32 — `PriorityQueue.peek` over a HEAP-CARRYING `T`.
///
/// `peek` returns `Option[T]`, not `Option[ref T]`: a Kāra body cannot
/// construct a borrow-carrying `Option`, so the root is COPIED out of the
/// backing `Vec` while `self` keeps its own. That copy is exactly the shape
/// that has produced this repo's ownership defects before — read an element
/// out of a container behind a `ref` receiver and either the copy is shallow
/// (double free when both are dropped) or the original is orphaned (leak).
///
/// LSan is the oracle that can see both, and only on Linux. The `Drop` body
/// count is the second oracle: `peek` must fire NO drop of its own beyond
/// the peeked copy's, and must leave the queue's element count intact — a
/// `peek` that moved the root out would drop one element early and print a
/// short queue at the end.
#[test]
fn asan_priority_queue_peek_copies_a_heap_root_without_disturbing_it() {
    assert_clean_asan_run(
        r#"
fn main() {
    let mut q: PriorityQueue[String] = PriorityQueue.new();
    q.push(f"pear{1}");
    q.push(f"apple{2}");
    q.push(f"fig{3}");

    // Peek twice: the root is copied, never moved out, so the second read
    // sees the same element and the queue still holds all three.
    match q.peek() { Some(v) => { println(v); } None => { println("none"); } }
    match q.peek() { Some(v) => { println(v); } None => { println("none"); } }
    println(q.len());

    // Drain: every element must still be there, in order.
    while q.len() > 0 {
        match q.pop() { Some(v) => { println(v); } None => {} }
    }
    match q.peek() { Some(v) => { println(v); } None => { println("none"); } }
    println("end");
}
"#,
        &[
            "apple2", "apple2", "3", "apple2", "fig3", "pear1", "none", "end",
        ],
        "priority-queue-peek-heap-root",
    );
}

/// B-2026-08-26-18 — `PriorityQueue[T]` where `T` is a struct carrying a
/// heap field, the shape the row was filed on. `push`'s `sift_up` calls
/// `swap`, whose three self-rooted element stores orphaned a buffer each.
/// Three pushes trigger exactly one swap, and pre-fix this leaked exactly
/// the two strings that swap touched (6 bytes in 2 objects).
#[test]
fn asan_priority_queue_of_heap_bearing_struct_sift_no_leak() {
    assert_clean_asan_run(
        r#"
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Item { id: i64, name: String }
fn main() {
    let mut q: PriorityQueue[Item] = PriorityQueue.new();
    q.push(Item { id: 3, name: f"ccc{3}" });
    q.push(Item { id: 1, name: f"a{1}" });
    q.push(Item { id: 2, name: f"bb{2}" });
    println(f"{q.len()}");
}
"#,
        &["3"],
        "pq-heap-struct-sift",
    );
}

/// B-2026-08-26-31, the swap-rotation shape — the one at the heart of any
/// hand-written sort, over elements that genuinely own heap.
///
/// Like its sibling above this was ASAN-clean on both sides of the bodies
/// fix: `zero_struct_move_caps` had the memory right, and only the Drop
/// body count was wrong. Kept as a regression floor for the memory half
/// while the bodies half is edited.
///
/// Every `tag` is an f-STRING on purpose. A string LITERAL has `cap` 0, so
/// a second free would be a guarded no-op and the shape would look clean
/// even if it were not — three of B-2026-08-12-22's "safe" boundary rows
/// were literal-valued and aborted once the field was genuinely allocated.
#[test]
fn asan_swap_rotation_over_heap_bearing_elements_is_clean() {
    assert_clean_asan_run(
        r#"
#[derive(Clone)]
struct Item { id: i64, tag: String }
struct Bag { xs: Vec[Item] }
fn main() {
    let mut b = Bag { xs: Vec.new() };
    b.xs.push(Item { id: 1i64, tag: f"payload_one_{1}" });
    b.xs.push(Item { id: 2i64, tag: f"payload_two_{2}" });
    b.xs.swap(0, 1);
    println(f"{b.xs[0].id}{b.xs[1].id}");
}
"#,
        &["21"],
        "swap-rotation-heap-bearing-elements",
    );
}

/// PROBE (B-2026-08-27-5): is the niche-field `==` an OUT-OF-BOUNDS READ,
/// not merely a wrong answer? A niche `Option[shared T]` slot is one
/// pointer (8 bytes); the comparator built for `Option[T]` expects the
/// conventional `{tag, w0, w1, w2}` (32 bytes). `Tiny`'s whole heap box is
/// `{i64 rc, ptr next}` = 16 bytes, so a 32-byte read from offset 8 runs
/// 24 bytes past the end of the allocation.
/// B-2026-08-27-40 — the heap-carrying row of
/// `test_e2e_tuple_type_arg_survives_a_nested_generic_call`, which is the
/// one that SEGFAULTED rather than merely answering wrong. A
/// `(String, i64)` element is 32 bytes; with `T` fallen back to the `i64`
/// default the nested `self.sw(..)` swapped 8 of them, splicing a String
/// header across two elements and leaving a pointer field holding a
/// length. The crash is what that read produced, so a green run here is
/// the claim that the element stride is right — and LSan additionally
/// covers the ownership half, since every element carries a genuinely
/// heap-owned String (built by `+`, not a static literal, which would let
/// a shallow alias stay readable by luck).
///
/// The loop runs the whole build-swap-read cycle repeatedly so a per-call
/// imbalance accumulates past the noise floor rather than showing up once.
#[test]
fn asan_tuple_type_arg_with_a_heap_element_through_a_nested_generic_call() {
    assert_clean_asan_run(
        r#"
struct Bag[=T] { xs: Vec[T] }

impl[T] Bag[T] {
    fn sw(mut ref self, i: i64, j: i64) { self.xs.swap(i, j); }
    fn mid(mut ref self) { self.sw(0, 1); }
    fn outer(mut ref self) { self.mid(); }
    fn at(ref self, i: i64) -> T { return self.xs[i]; }
}

fn main() {
    let mut n = 0i64;
    let mut i = 0i64;
    while i < 40i64 {
        let mut s: Bag[(String, i64)] = Bag { xs: Vec.new() };
        s.xs.push(("x" + "y", 1i64));
        s.xs.push(("p" + "q", 2i64));
        s.outer();
        let s0 = s.at(0);
        n = n + s0.1;
        i = i + 1;
    }
    println(f"{n}");
}
"#,
        &["80"],
        "tuple-type-arg-heap-element-nested-generic-call",
    );
}

/// A field read on a BLOCK-EXPRESSION receiver whose struct owns heap
/// fields (B-2026-08-27-49).
///
/// The block-receiver fix made this shape COMPILE for the first time, so
/// its ownership had never been exercised. It needs its own gate because
/// `expr_yields_fresh_owned_temp` — which arms the fresh-temp struct drop
/// behind `track_freshtemp_field_access_object` — admits `Call` and
/// `MethodCall` and NOT `Block`, so a block temp is not a fresh owned temp
/// by that predicate. The read below hands out one field and never names
/// the other, which is precisely the B-2026-07-22-2 leak shape: an
/// unnamed heap field of a temporary aggregate that nothing owns.
#[test]
fn test_block_receiver_heap_field_read_is_leak_free() {
    let src = r#"
struct Person { name: String, note: String, age: i64 }

fn make() -> Person {
    return Person { name: "ada".to_string(), note: "first".to_string(), age: 36 };
}

fn main() {
    // `name` escapes to the print; `note` is never read by anyone.
    println({ let p = make(); p }.name);
    println({ let p = make(); p }.age);
}
"#;
    assert_clean_asan_run(src, &["ada", "36"], "block-receiver-heap-field-read");
}

/// A field read on an `if`-EXPRESSION receiver whose struct owns heap
/// fields (B-2026-08-28-7 leg 2).
///
/// Sibling of the block-receiver gate above and for the same reason: making
/// the shape compile made its ownership reachable for the first time.
/// Whichever branch runs, `compile_block_with_frame` neutralized that
/// branch's tail cleanup, so the consumer owns the aggregate — and with
/// nothing registered to drop it, its heap fields leak, including the
/// `note` this read never names.
#[test]
fn test_if_expression_receiver_heap_field_read_is_leak_free() {
    let src = r#"
struct Person { name: String, note: String, age: i64 }

fn make(k: i64) -> Person {
    return Person { name: "ada".to_string(), note: "unread".to_string(), age: k };
}

fn main() {
    let c = true;
    println(if c { make(1) } else { make(2) }.name);
    println(if not c { make(3) } else { make(4) }.age);
}
"#;
    assert_clean_asan_run(src, &["ada", "4"], "if-receiver-heap-field-read");
}

/// A HEAP field read on a struct PROJECTED OUT OF A FRESH TUPLE TEMP —
/// `make().0.name` where `make -> (Person, i64)` (B-2026-08-28-3).
///
/// Third in the line the block- and `if`-receiver gates above started, and
/// for the same reason: making the shape RESOLVE made it compile for the
/// first time, which made its ownership reachable for the first time — and
/// nothing owned it. Measured before the fix, this program emitted no drop
/// at all, against the sibling `println(plain(1).name)`'s
/// `__freshtemp_fldobj` slot plus `__karac_drop_struct_Person`.
///
/// `expr_yields_fresh_owned_temp` asks what KIND of expression produced a
/// value, and a `TupleIndex` is a projection rather than a producer, so the
/// fresh-temp registration had to learn to look one hop down at the base.
///
/// Every row reads a DIFFERENT field, because the leak was exactly the read
/// field's buffer and nothing else: a fixture that always read `name` would
/// pass against a fix that only rescued the first field. `age` is the
/// control that allocates nothing of its own, `move-let` is the consuming
/// position — where the read field's cap is zeroed and the binding becomes
/// its sole owner, so a fix that dropped unconditionally would double-free
/// here rather than leak — and the last two rows are the already-working
/// bound and non-tuple spellings, which must stay clean.
#[test]
fn test_heap_field_read_on_a_fresh_tuple_temp_is_leak_free() {
    let src = r#"
struct Person { name: String, note: String, age: i64 }

fn make(k: i64) -> (Person, i64) {
    return (
        Person { name: "ada".to_string(), note: "unread".to_string(), age: k },
        k + 1,
    );
}

fn plain(k: i64) -> Person {
    return Person { name: "ada".to_string(), note: "unread".to_string(), age: k };
}

fn main() {
    println(make(1).0.name);
    println(make(2).0.note);
    println(make(3).0.age);
    let w = make(4).0.name;
    println(w);
    let q = make(5);
    println(q.0.name);
    println(plain(6).name);
}
"#;
    assert_clean_asan_run(
        src,
        &["ada", "unread", "3", "ada", "ada", "ada"],
        "fresh-tuple-temp-heap-field-read",
    );
}

/// A HEAP FIELD read through a DEEPER PLACE rooted at a `ref` binding and
/// consumed with no intervening `let` — `fn peek(w: ref W) -> String {
/// w.r.name }` (B-2026-08-28-25).
///
/// A `ref` binding does not own the caller's storage, so the loaded
/// `{ptr,len,cap}` was an ALIAS of the caller's buffer: the caller's drop
/// freed it and so did whatever the callee handed it to. `free(): double
/// free detected in tcache 2`, rc 134, on both compiled backends, from a
/// `karac check`-clean program.
///
/// THE THREE CONTROLS ARE THE POINT, because each one alone would misplace
/// the cause. DEPTH ONE (`r.name` on a `ref R`) was already clean — so the
/// `ref` mode is not the variable. The depth-two `let` spelling
/// (`let s = w.r.name; s`) was already clean — so the depth is not the
/// variable either; `clone_ref_chain_field_move_rhs` covers that position.
/// And a SCALAR field through the same deeper place is clean, having no
/// buffer to free twice. What was broken is the intersection: two or more
/// hops, a heap leaf, and a consuming position with no binding.
///
/// A CLONE, NOT A SUPPRESSION, and this fixture is shaped to catch the
/// wrong direction: every row reads the caller's field back AFTER the call.
/// Cap-zeroing the source would silence the abort and strand the caller's
/// buffer with no owner — a leak here, and a use-after-free for the reader.
///
/// The argument and `Vec.push` rows are the other failure direction: this
/// helper also runs at ARGUMENT positions, so a clone nothing takes over
/// would show up as a leak rather than an abort.
#[test]
fn test_ref_rooted_deep_place_heap_read_is_cloned_not_aliased() {
    let src = r#"
struct R { id: i64, name: String }
struct W { r: R, n: i64 }
struct W3 { w: W, k: i64 }

fn sink(s: String) -> i64 { return s.len(); }

fn depth1(r: ref R) -> String { return r.name; }
fn depth2_let(w: ref W) -> String { let s = w.r.name; return s; }
fn depth2_ret(w: ref W) -> String { return w.r.name; }
fn depth2_tail(w: ref W) -> String { w.r.name }
fn depth3(x: ref W3) -> String { return x.w.r.name; }
fn depth2_scalar(w: ref W) -> i64 { return w.r.id; }
fn depth2_mut(w: mut ref W) -> String { return w.r.name; }
fn depth2_struct(x: ref W3) -> R { return x.w.r; }
fn tuple_hop(p: ref (R, i64)) -> String { return p.0.name; }
fn arg_pos(w: ref W) -> i64 { return sink(w.r.name); }
fn push_pos(w: ref W) -> i64 {
    let mut o: Vec[String] = Vec.new();
    o.push(w.r.name);
    return o[0].len();
}

impl W3 { fn peek(ref self) -> String { return self.w.r.name; } }

fn main() {
    let a = R { id: 41, name: f"n{41}" };
    println(depth1(a));
    println(a.name);

    let mut b = W { r: R { id: 41, name: f"n{41}" }, n: 1 };
    println(depth2_let(b));
    println(depth2_ret(b));
    println(depth2_tail(b));
    println(depth2_scalar(b));
    println(arg_pos(b));
    println(push_pos(b));
    println(depth2_mut(mut b));
    // the caller's field survives every one of those
    println(b.r.name);

    let c = W3 { w: W { r: R { id: 41, name: f"n{41}" }, n: 1 }, k: 2 };
    println(depth3(c));
    println(c.peek());
    println(depth2_struct(c).name);
    println(c.w.r.name);

    let d = (R { id: 41, name: f"n{41}" }, 1);
    println(tuple_hop(d));
    println(d.0.name);
}
"#;
    assert_clean_asan_run(
        src,
        &[
            "n41", "n41", "n41", "n41", "n41", "41", "3", "3", "n41", "n41", "n41", "n41", "n41",
            "n41", "n41", "n41",
        ],
        "ref-rooted-deep-place-heap-read",
    );
}

/// A heap field read off a FIELD-ROOTED container is cloned —
/// `h.xs[0].name` and its `ref self` twin `self.xs[0].name`
/// (B-2026-08-28-42).
///
/// `clone_vec_elem_heap_field_read` gated its container on
/// `ExprKind::Identifier`, so a field-rooted one declined the clone and the
/// read handed out a shallow alias of the element's buffer: `free(): double
/// free detected in tcache 2`, rc 134, on both compiled backends, from a
/// `karac check`-clean program the interpreter answered correctly.
///
/// THE IDENTIFIER ROW IS THE CONTROL THAT LOCATES IT. The identical read
/// off a bare local `Vec[R]` was always clean, so neither the read nor the
/// element type is the variable — only the container's ROOT is. Keeping
/// both spellings here is what would catch a future change that fixes one
/// and regresses the other.
///
/// The TUPLE spelling (`h.ps[0].0.name`) is here because the same gate is
/// duplicated in the tuple-index cloner, and the two were widened in
/// lockstep: a fix applied to only one leaves the pair disagreeing about
/// which containers own their elements.
///
/// The argument and non-consuming rows are the other direction — this clone
/// carries its own cleanup, so a read nothing takes over must free it. They
/// were clean BEFORE the fix (an owned aggregate param is callee-owned by
/// entry copy, and a borrow frees nothing), which makes them the rows that
/// fail if the clone is registered without a takeover.
#[test]
fn test_field_rooted_container_heap_field_read_is_cloned() {
    let src = r#"
struct R { id: i64, name: String }
struct H { xs: Vec[R], k: i64 }
struct H2 { ps: Vec[(R, i64)], k: i64 }
struct Box2 { w: String }

fn sink(s: String) -> i64 { return s.len(); }

impl H { fn peek(ref self) -> String { return self.xs[0].name; } }
impl H2 { fn peek(ref self) -> String { return self.ps[0].0.name; } }

fn main() {
    // the identifier-rooted control, always clean
    let v: Vec[R] = [R { id: 8, name: f"m{8}" }];
    let c = v[0].name;
    println(c);
    println(v[0].name);

    let h = H { xs: [R { id: 8, name: f"m{8}" }], k: 1 };
    // consuming positions
    let s = h.xs[0].name;
    println(s);
    let b = Box2 { w: h.xs[0].name };
    println(b.w);
    let mut o: Vec[String] = Vec.new();
    o.push(h.xs[0].name);
    println(o[0]);
    println(h.peek());
    // non-consuming reads: the clone must free itself
    println(sink(h.xs[0].name));
    println(h.xs[0].name + "!");
    println(h.xs[0].id);
    // the container's element survives all of it
    println(h.xs[0].name);

    // the tuple spelling shares the gate
    let h2 = H2 { ps: [(R { id: 9, name: f"p{9}" }, 1)], k: 1 };
    let t = h2.ps[0].0.name;
    println(t);
    println(h2.peek());
    println(h2.ps[0].0.name);
}
"#;
    assert_clean_asan_run(
        src,
        &[
            "m8", "m8", "m8", "m8", "m8", "m8", "2", "m8!", "8", "m8", "p9", "p9", "p9",
        ],
        "field-rooted-container-heap-field-read",
    );
}

/// A HEAP field read through a GENERIC callee's tuple element —
/// `firstof(R { … }).0.name` where `fn firstof[T](x: T) -> (T, i64)`
/// (B-2026-08-28-28).
///
/// That fix made the shape COMPILE for the first time, which is the point
/// at which this family has produced a fresh ownership defect four times
/// running (B-2026-08-27-49, -3, -34, and the tuple temp of -27). It does
/// NOT here, and this test is what says so rather than an assumption: the
/// fresh-tuple-temp registration from B-2026-08-28-27 and the projected-
/// element handover from B-2026-08-28-3 already cover the generic callee,
/// because both key on the SHAPE of the projection and neither cares
/// whether the callee's return type was written generically.
///
/// So this is a guard on that coverage rather than a fix's gate: it fails
/// if a future change to either registration starts distinguishing generic
/// callees. Bound, unbound and consumed reads are all here for that reason,
/// and the container is re-read at the end.
#[test]
fn test_generic_callee_tuple_element_heap_read_is_owned() {
    let src = r#"
struct R { id: i64, name: String }

fn firstof[T](x: T) -> (T, i64) { return (x, 1); }

fn main() {
    let q = firstof(R { id: 1, name: f"n{41}" });
    println(q.0.name);
    println(firstof(R { id: 2, name: f"m{2}" }).0.name);
    let w = firstof(R { id: 3, name: f"p{3}" }).0.name;
    println(w);
    println(q.0.id);
    println(q.1);
    println(q.0.name);
}
"#;
    assert_clean_asan_run(
        src,
        &["n41", "m2", "p3", "1", "1", "n41"],
        "generic-callee-tuple-element-heap-read",
    );
}

/// A BARE-`shared` field read off a shared TEMPORARY receiver releases the
/// ref it took — `make_outer(12).inner.tag` (B-2026-08-28-20).
///
/// `load_owned_shared_temp_field` bumps the loaded inner handle's refcount
/// so the receiver's recursive drop cannot free it out from under the
/// reader, and documents that `+1` as being taken over by "the caller … or
/// the next FieldAccess hop in a chain". The next hop could not take it:
/// `shared_type_for_call_like` had no FieldAccess arm — its own comment
/// defers one as needing "recursive receiver-type recovery" — so the outer
/// read never reached the release path and the whole inner box leaked, 43
/// bytes in 2 allocations at -O0.
///
/// BOTH a heap and a SCALAR leaf are here because what leaked is the box,
/// not the value read out of it: `.inner.v` lost exactly as much as
/// `.inner.tag`, which rules out the loaded `String` as the cause.
///
/// THE IDENTIFIER-ROOTED ROWS ARE THE CONTROL, and the reason the fix
/// recurses rather than merely admitting a FieldAccess. `let o =
/// make_outer(12); o.inner.tag` roots at a binding, never receives a `+1`,
/// and releasing there would be one dec too many — a use-after-free rather
/// than a leak. It stays clean only because the recursion declines it.
///
/// EACH SHAPE IS ITS OWN PROGRAM, deliberately, and two shapes are absent
/// for the same reason: a program with more than one shared-temp field read
/// still leaks one box, and the BOUND spelling (`let h = ….inner`) still
/// leaks its payload at -O2 while being clean at -O0. Both are
/// pre-existing, both are unchanged by this fix (identical byte counts
/// before and after), and both are B-2026-08-28-49. Combining these rows
/// into one `main` would hide every one of them behind that residual.
#[test]
fn test_bare_shared_field_read_off_a_shared_temp_releases_its_ref() {
    let hdr = "\
shared struct Node { v: i64, tag: String }\n\
shared struct Outer { id: i64, inner: Node }\n\
shared struct Deep { o: Outer, z: i64 }\n\
fn make(k: i64) -> Node { return Node { v: k, tag: f\"t{k}\" }; }\n\
fn make_outer(k: i64) -> Outer { return Outer { id: k, inner: make(k) }; }\n\
fn make_deep(k: i64) -> Deep { return Deep { o: make_outer(k), z: k }; }\n";
    for (label, body, want) in [
        // the temporary chain: a heap leaf and a scalar leaf leaked alike
        ("heap-leaf", "println(make_outer(12).inner.tag);", "t12"),
        ("scalar-leaf", "println(make_outer(13).inner.v);", "13"),
        // two hops down, so the recursion is exercised past one level
        ("two-hop", "println(make_deep(16).o.inner.tag);", "t16"),
        // identifier-rooted: never gets a +1, must not be released
        (
            "ident-root-heap",
            "let o = make_outer(15); println(o.inner.tag);",
            "t15",
        ),
        (
            "ident-root-scalar",
            "let o = make_outer(15); println(o.inner.v);",
            "15",
        ),
        // a scalar off the temp itself, and a non-nested shared temp
        ("temp-own-scalar", "println(make_outer(17).id);", "17"),
        ("plain-shared-temp", "println(make(18).tag);", "t18"),
    ] {
        let src = format!("{hdr}\nfn main() {{\n    {body}\n}}\n");
        assert_clean_asan_run(&src, &[want], label);
    }
}

/// A scalar field read on a `shared struct` block / `if` receiver is
/// REFCOUNT-BALANCED (B-2026-08-28-7 leg 1).
///
/// The path added for this reads through the temporary's RC pointer and
/// then releases the one ref the block handed out, reusing the call-result
/// receiver's helper so the two cannot drift. Balance is the whole question
/// here: one dec too few leaks the box, one too many frees it under a live
/// alias — and the bound receiver in the same program shares the type, so a
/// dec aimed at the wrong handle shows up as well.
///
/// The struct carries a `String` field that this program never reads
/// through a temporary, on purpose: it makes the box's drop non-trivial, so
/// a missing release leaks visibly rather than costing nothing.
#[test]
fn test_shared_block_receiver_scalar_field_read_is_refcount_balanced() {
    let src = r#"
shared struct Node { v: i64, tag: String }

fn make(k: i64) -> Node { return Node { v: k, tag: "held".to_string() }; }

fn main() {
    println({ let n = make(1); n }.v);
    let c = true;
    println(if c { make(2) } else { make(3) }.v);
    let b = make(4);
    println(b.v);
    println(b.tag);
}
"#;
    assert_clean_asan_run(
        src,
        &["1", "2", "4", "held"],
        "shared-block-receiver-scalar-field",
    );
}

/// A `String` / `Vec` field read off a shared-struct TEMPORARY receiver
/// owns its copy (B-2026-08-28-14).
///
/// The read is served by `load_owned_shared_temp_field`, which releases the
/// temporary's last ref as soon as the field is in a register — true for a
/// SCALAR field and false for a heap one, whose `{ptr,len,cap}` still points
/// into the box that release is about to drop. The deep copy that gives the
/// read an independent buffer runs one step later, in `exprs.rs`'s
/// FieldAccess arm, so before this fix the temp spelling below printed the
/// right bytes and leaked them.
///
/// All three temporary shapes are here because they release through the same
/// helper and differ only in how the ref arrives: a CALL result, a value
/// BLOCK that neutralized its tail's cleanup, and an `if` whose taken branch
/// did. The bound receiver rides along as the control that was always clean
/// — a fix that over-corrects by cloning for every receiver double-frees it.
#[test]
fn test_shared_temp_receiver_heap_field_read_owns_its_copy() {
    let src = r#"
shared struct Node { v: i64, tag: String, xs: Vec[i64] }

fn make(k: i64) -> Node {
    return Node { v: k, tag: "hello".to_string(), xs: [k, k + 1] };
}

fn main() {
    println(make(1).tag);
    println({ let n = make(2); n }.tag);
    let c = true;
    println(if c { make(3) } else { make(4) }.tag);
    println(make(5).xs.len());
    // The copy has to OUTLIVE the temporary it was read from, which is the
    // half a clone emitted after the release cannot deliver.
    let kept = make(6).tag;
    println(kept);
    // The bound receiver was never broken and must not become a double free.
    let b = make(7);
    println(b.tag);
}
"#;
    assert_clean_asan_run(
        src,
        &["hello", "hello", "hello", "2", "hello", "hello"],
        "shared-temp-receiver-heap-field-read",
    );
}

/// B-2026-08-31-21 — the LEAK half. A discarded `shared` struct literal got
/// no owner at any discard site, so its RC box and the `String` inside it
/// were stranded: 41 B in 2 allocations per evaluation, on every backend.
///
/// Measured at `KARAC_OPT_LEVEL=0` in the row, because at the default `-O2`
/// the allocation is dead and LLVM removes it — which is why this ran clean
/// in ordinary testing and why the loop below matters more than the count:
/// it makes the stranded box reachable work the optimizer cannot fold away.
///
/// The allocation floor keeps the cell honest — a build that stopped
/// allocating would otherwise pass vacuously.
#[test]
fn asan_discarded_shared_literal_frees_its_rc_box() {
    let src = "shared struct S { id: i64, name: String }\n\
             impl Drop for S { fn drop(mut ref self) { println(f\"dS{self.id}\"); } }\n\
             fn mks(n: i64) -> S { return S { id: n, name: f\"n{n}\" }; }\n\
             fn main() {\n\
                 let mut i = 0i64;\n\
                 while i < 20i64 {\n\
                     let _ = S { id: i, name: f\"a{i}\" };\n\
                     S { id: i + 100i64, name: f\"b{i}\" };\n\
                     let _ = if i % 2i64 == 0i64 { S { id: i + 200i64, name: f\"c{i}\" } } else { S { id: i + 300i64, name: f\"d{i}\" } };\n\
                     let _ = mks(i + 400i64);\n\
                     i = i + 1i64;\n\
                 }\n\
                 println(\"done\");\n\
             }\n";
    // 20 iterations x 4 discards, each printing its own body, then `done`.
    let mut expect: Vec<String> = Vec::new();
    for i in 0..20i64 {
        expect.push(format!("dS{i}"));
        expect.push(format!("dS{}", i + 100));
        expect.push(format!("dS{}", if i % 2 == 0 { i + 200 } else { i + 300 }));
        expect.push(format!("dS{}", i + 400));
    }
    expect.push("done".to_string());
    let expect_refs: Vec<&str> = expect.iter().map(|s| s.as_str()).collect();
    assert_clean_asan_run_min_allocs(src, &expect_refs, "b21-discarded-shared-literal", 80);
}

/// B-2026-09-02-5 — `h2 = h` over a `Drop`-bearing struct freed the
/// moved-in value's heap NOWHERE, because the two suppressions that meet on
/// that statement each assumed the other still would.
///
/// An owned by-value struct param is ENTRY-COPIED by the callee
/// (`make_aggregate_param_callee_owned`), so `h`'s buffers inside `f` are
/// the callee's and only the `Drop` BODY is the caller's. B-2026-08-01-16
/// withheld that body — now per-path, via B-2026-08-30-53's flag — and for
/// a type with an `impl Drop` the body and the field walk are ONE
/// registered action (`karac_drop_<T>` calls `__karac_drop_struct_<T>`
/// from inside itself, and is mutually exclusive with `StructDrop`), so
/// withholding the body withheld the free. B-2026-07-16-18's struct-move
/// suppression then zeroed the SOURCE's field caps on the stated premise
/// that "the LHS `a`'s own StructDrop stays the unique owner of the buffers
/// now in its slot" — which for a `Drop`-bearing type does not exist. The
/// fix gives the flag-`false` edge the field walk alone.
///
/// THE ROW'S OWN REPRO COULD NOT HAVE GATED THIS, and that is why the
/// fixture looks nothing like it. It carries a one-character `String` tag,
/// which leaks ONE byte at `-O0` and is clean at the default `-O2` — the
/// masking the row itself records, and the level CI's default ASAN leg
/// runs at, so a fixture written to it would have passed on the pre-fix
/// compiler. The `Vec[String]` payload here defeats that folding: measured
/// against the pre-fix compiler, 3,195 B definitely + 2,280 B indirectly
/// lost at `KARAC_OPT_LEVEL=0` AND 2,880 + 2,280 at the default `-O2`. Both
/// legs go to zero with the fix.
///
/// FIVE CELLS, looped so a flag that fails to reset between calls shows up:
///
///   * `basic` — the row's shape.
///   * `rearm` — the target is later given a FRESH value. The DISPLACEMENT
///     fire reads the same flag, so it is the same else edge that owes the
///     view's memory here; without it the entry copy is orphaned by the
///     overwrite rather than at scope exit.
///   * `chain` / `viewlet` — the view reaches a SECOND target, through an
///     assignment and through a `let` rebind. Both make the source a
///     `param_view_locals` entry whose own action is already withheld, so
///     the second target is the only owner left.
///   * `cond` — exercised on BOTH legs. The not-taken leg is what fails if
///     the memory drop is made unconditional instead of an else edge: there
///     the target still holds its own initializer and the full wrapper
///     frees it, so a second free would be a double free rather than a leak.
///
/// Output is byte-identical pre- and post-fix on the compiled backends —
/// this is memory only. The `rearm` cell is the one place `--interp`
/// disagrees (it loses the fresh value's `dR5` after a param-view
/// assignment); that divergence predates this fix, is unchanged by it, and
/// is filed as B-2026-09-03-30. The compiled side is the correct one, so
/// pinning it here is right either way.
#[test]
fn asan_param_view_assignment_frees_the_moved_in_heap_exactly_once() {
    assert_clean_asan_run(
        r#"
struct R { id: i64, tag: String, xs: Vec[String] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}-{self.xs.len()}") } }

fn mk(n: i64) -> R {
    let mut v: Vec[String] = Vec.new();
    let mut i: i64 = 0;
    while i < 8 { v.push(f"payloadpayload-{n}-{i}"); i = i + 1; }
    return R { id: n, tag: f"tag-payloadpayload-{n}", xs: v }
}

fn basic(h: R) -> i64 { let mut a: R = mk(1); a = h; return a.id }
fn rearm(h: R) -> i64 { let mut a: R = mk(2); a = h; a = mk(5); return a.id }
fn chain(h: R) -> i64 { let mut a: R = mk(3); a = h; let mut b: R = mk(4); b = a; return b.id }
fn viewlet(h: R) -> i64 { let a = h; let mut b: R = mk(6); b = a; return b.id }
fn cond(h: R, c: bool) -> i64 { let mut a: R = mk(7); if c { a = h; } return a.id }

fn main() {
    let mut i: i64 = 0;
    while i < 3 {
        println(f"A{basic(mk(40))}");
        println(f"B{rearm(mk(41))}");
        println(f"C{chain(mk(42))}");
        println(f"D{viewlet(mk(43))}");
        println(f"E{cond(mk(44), true)}");
        println(f"F{cond(mk(45), false)}");
        i = i + 1;
    }
    println("done");
}
"#,
        &[
            "dR1-8", "dR40-8", "A40", "dR2-8", "dR5-8", "dR41-8", "B5", "dR3-8", "dR4-8", "dR42-8",
            "C42", "dR6-8", "dR43-8", "D43", "dR7-8", "dR44-8", "E44", "dR7-8", "dR45-8", "F7",
            "dR1-8", "dR40-8", "A40", "dR2-8", "dR5-8", "dR41-8", "B5", "dR3-8", "dR4-8", "dR42-8",
            "C42", "dR6-8", "dR43-8", "D43", "dR7-8", "dR44-8", "E44", "dR7-8", "dR45-8", "F7",
            "dR1-8", "dR40-8", "A40", "dR2-8", "dR5-8", "dR41-8", "B5", "dR3-8", "dR4-8", "dR42-8",
            "C42", "dR6-8", "dR43-8", "D43", "dR7-8", "dR44-8", "E44", "dR7-8", "dR45-8", "F7",
            "done",
        ],
        "b0902-5-param-view-assign-heap",
    );
}

/// B-2026-09-03-36 — the ASSIGNMENT sibling of the row above, and the one
/// case both of `emit_drop_fn_for_type_expr`'s existing guards exclude.
///
/// Reassigning a `shared` field of a plain struct — `n.s = Sd { m: 9 }` —
/// must release the DISPLACED handle. The memory-side resolver every such
/// channel funnels through carries two guards for a `Drop`-bearing type,
/// each opening with `!shared_types.contains_key(..)`, both written because
/// the user-drop WRAPPER is named exactly `karac_drop_<T>` too. A `shared`
/// type falls through both and reaches that name — so the displacing
/// release ran the wrapper: the `Drop` body PRINTED, which is why the
/// defect reads as correct on output alone, while the wrapper never touched
/// the refcount and the 16-byte box was stranded. One per assignment,
/// unbounded, and independent of whether the owner has `impl Drop`.
///
/// The fix routes a shared slot to an rc-dec instead — a shared field holds
/// an 8-byte HANDLE, not a value to walk — which is already the answer
/// `vec_elem_agg_drop_for_type_expr` reaches for on a shared Vec ELEMENT.
///
/// THE FIXTURE IS BUILT TO BITE AT `-O2` for the reason the row above
/// records: a constant-seeded payload nobody reads folds away, and the CI
/// ASAN leg runs at the default level. Seeding from `env.args().len()` and
/// reading `note` from inside each `Drop` body keeps the boxes alive.
/// Measured against the pre-fix compiler: 480 B definitely + 348 B
/// indirectly lost at BOTH `KARAC_OPT_LEVEL=0` and the default `-O2`; both
/// go to zero.
///
/// `aliased` IS THE CELL THAT FAILS AN OVER-EAGER FIX, and it also pins the
/// half of the defect that output alone does show. One handle is held by
/// both `g` and `n.s`, so the displacing release must dec to 1 and free
/// NOTHING. Pre-fix the wrapper ran `dSd40` right there, on a box `g` still
/// owns and still reads afterwards; post-fix it moves to `g`'s scope exit,
/// so the expected order is `dSd41` then `dSd40`. A fix that released
/// unconditionally rather than through the rc-dec double-frees here.
///
/// The other four cells cover the shapes the displacing release reaches
/// through: `plain` (owner has no `impl Drop`), `owner_drop` (owner has
/// one, so the wrapper is in play), `nested` (`o.i.s = ..`, two hops), and
/// `twice` (two consecutive assignments, so a fix that disarms the field
/// after the first shows up as a leak).
///
/// `--interp` prints none of these `dSd` lines — that is B-2026-09-03-9,
/// the interpreter losing a shared struct's body whenever it is held in an
/// aggregate. This harness asserts the compiled side only.
#[test]
fn asan_reassigning_a_shared_field_releases_the_displaced_rc_box() {
    assert_clean_asan_run(
        r#"
shared struct Sd { m: i64, note: String }
impl Drop for Sd { fn drop(mut ref self) { println(f"dSd{self.m}-{self.note.len()}") } }

struct N { id: i64, s: Sd, tag: String }

struct H { id: i64, s: Sd, tag: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}-{self.tag.len()}") } }

struct Inner { s: Sd }
struct Outer { i: Inner, tag: String }

fn mk(n: i64) -> Sd { return Sd { m: n, note: f"note-payloadpayloadpayload-{n}" } }
fn tg(n: i64) -> String { return f"tag-payloadpayloadpayload-{n}" }

fn plain(k: i64) -> i64 { let mut n: N = N { id: k, s: mk(10 * k), tag: tg(1) }; n.s = mk(11 * k); return n.tag.len() }
fn owner_drop(k: i64) -> i64 { let mut h: H = H { id: 2 * k, s: mk(20 * k), tag: tg(2) }; h.s = mk(21 * k); return h.tag.len() }
fn nested(k: i64) -> i64 { let mut o: Outer = Outer { i: Inner { s: mk(30 * k) }, tag: tg(3) }; o.i.s = mk(31 * k); return o.tag.len() }
fn aliased(k: i64) -> i64 { let g: Sd = mk(40 * k); let mut n: N = N { id: 4 * k, s: g, tag: tg(4) }; n.s = mk(41 * k); return n.tag.len() + g.note.len() }
fn twice(k: i64) -> i64 { let mut n: N = N { id: 5 * k, s: mk(50 * k), tag: tg(5) }; n.s = mk(51 * k); n.s = mk(52 * k); return n.tag.len() }

fn main() {
    let k: i64 = env.args().len();
    let mut acc: i64 = 0;
    let mut i: i64 = 0;
    while i < 2 {
        acc = acc + plain(k);
        acc = acc + owner_drop(k);
        acc = acc + nested(k);
        acc = acc + aliased(k);
        acc = acc + twice(k);
        i = i + 1;
    }
    println(f"acc{acc}");
}
"#,
        &[
            "dSd10-29", "dSd11-29", "dSd20-29", "dH2-27", "dSd21-29", "dSd30-29", "dSd31-29",
            "dSd41-29", "dSd40-29", "dSd50-29", "dSd51-29", "dSd52-29", "dSd10-29", "dSd11-29",
            "dSd20-29", "dH2-27", "dSd21-29", "dSd30-29", "dSd31-29", "dSd41-29", "dSd40-29",
            "dSd50-29", "dSd51-29", "dSd52-29", "acc328",
        ],
        "b0903-36-shared-field-reassign-rc",
    );
}

/// B-2026-09-04-31 — a `shared struct` in a TUPLE ELEMENT, every shape
/// that was a use-after-free or a leak, under ASAN + LSan.
///
/// The read itself was the UAF: `a.0.id` fell through to the fresh-
/// temporary receiver and released the element after the load, so a
/// second read touched freed memory (valgrind `Invalid read of size 8`,
/// garbage on stdout). The move-outs were UAFs in the first draft of the
/// fix, once the tuple started releasing its element: `return a.0` and
/// the tail spelling off a by-value param, `H { s: a.0 }`, and `let p =
/// w.p`. The never-read tuple was the LEAK half — 40 B at `-O0`, hidden
/// at `-O2` by dead-allocation elimination, which is why the row it came
/// from called it "balanced". One program, every shape, clean exit.
#[test]
fn asan_tuple_held_shared_struct_reads_and_move_outs_clean() {
    let label = "tuple_held_shared_struct_reads_and_move_outs";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan(
        r#"
shared struct S { id: i64, tag: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
struct H { s: S }
struct W { p: (S, i64) }
fn pick(a: (S, i64)) -> S { return a.0 }
fn pick2(a: (S, i64)) -> S { a.0 }
fn build(k: i64) -> (S, i64) { let s: S = S { id: k, tag: "b" }; return (s, 3) }
fn main() {
    { let a: (S, i64) = (S { id: 1, tag: "a" }, 3); println(f"r1={a.0.id}"); println(f"r2={a.0.id}"); println(f"r3={a.0.tag}"); }
    { let b: (S, i64) = (S { id: 2, tag: "a" }, 3); }
    { let a: (S, i64) = (S { id: 3, tag: "a" }, 3); let s: S = a.0; let u: S = a.0; println(f"v{s.id}{u.id}{a.0.id}"); }
    { let a: (S, i64) = (S { id: 4, tag: "a" }, 3); let (s, n) = a; println(f"v{s.id}{n}"); }
    { let a: (S, i64) = (S { id: 5, tag: "a" }, 3); let s: S = pick(a); println(f"v{s.id}"); }
    { let a: (S, i64) = (S { id: 6, tag: "a" }, 3); let s: S = pick2(a); println(f"v{s.id}"); }
    { let a: (S, i64) = (S { id: 7, tag: "a" }, 3); let h: H = H { s: a.0 }; println(f"v{h.s.id}{a.0.id}"); }
    { let w: W = W { p: (S { id: 8, tag: "a" }, 3) }; let p: (S, i64) = w.p; println(f"v{p.0.id}"); }
    { let a: (S, i64) = build(9); println(f"v{a.0.id}"); }
    { let s: S = S { id: 10, tag: "a" }; let a: (S, i64) = (s, 1); let b: (S, i64) = (s, 2); println(f"v{a.0.id}{b.0.id}"); }
    { let v: Vec[(S, i64)] = [(S { id: 11, tag: "a" }, 1), (S { id: 12, tag: "a" }, 2)]; let v2: Vec[(S, i64)] = v; println(f"v{v2[1].0.id}"); }
    println("end");
}
"#,
        label,
    ) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported an error (exit {:?}); stdout:\n{stdout}",
        status.code()
    );
    assert_eq!(
        stdout.trim().lines().collect::<Vec<_>>(),
        vec![
            "r1=1", "r2=1", "r3=a", "dS1", "dS2", "v333", "dS3", "v43", "dS4", "v5", "dS5", "v6",
            "dS6", "v77", "dS7", "v8", "dS8", "v9", "dS9", "v1010", "dS10", "v12", "dS11", "dS12",
            "end",
        ],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// B-2026-09-07-49 — a DESTRUCTURE whose source PROJECTS off an
/// RC-fallback-promoted local, pinned CLEAN.
///
/// This shape reaches both consults in `finish_owned_struct_destructure`
/// (`moved_projection_src`'s gate and `src_read_is_copy`), each of which
/// answers "not a consume site" for a promoted root and so takes the
/// TRANSFER rather than treating the read as a copy.
///
/// It is pinned because the corpus does not otherwise reach it: the
/// instrumented census found ZERO divergences at either `stmts.rs` consult
/// across every `memory_sanitizer`, `codegen` and `par_codegen` program,
/// while this ten-line fixture hits one. An untested divergence is exactly
/// how B-2026-09-07-23 / -29 / -30 each stayed hidden until a double free
/// in a small program found it.
///
/// Clean on all four surfaces at 1 and 3 trips — the transfer is correct
/// here because the leaves take the box's field and the box's own `RcDec`
/// is what releases it; registering a leaf owner as well would be the
/// second one.
#[test]
fn rc_promoted_projection_destructure_stays_clean() {
    const OWN: &str = "struct Inner { a: String, b: String }\n\
             struct W { inner: Inner, n: i64 }\n\
             fn seed() -> i64 { env.args().len() }\n\
             fn payload() -> String { f\"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa\" }\n\
             fn mkw(n: i64) -> W { return W { inner: Inner { a: payload(), b: payload() }, n: n }; }\n\
             fn main() { println(go()); }\n";
    // 3 trips over a destructure of `w.inner`, `w` promoted by the loop.
    assert_clean_asan_run_no_auto_par(
            &format!(
                "{OWN}fn go() -> i64 {{ let w = mkw(9); let mut n = 0i64; let mut i = 0i64;\n\
                 \x20 while i < 3i64 {{ let Inner {{ a, b }} = w.inner; n = n + a.len() + b.len(); i = i + 1; }}\n\
                 \x20 return n; }}\n"
            ),
            &["228"],
            "b49_destructure_proj_three_trips",
        );
    // 1 trip — the smallest cell.
    assert_clean_asan_run_no_auto_par(
            &format!(
                "{OWN}fn go() -> i64 {{ let w = mkw(9); let mut n = 0i64; let mut i = 0i64;\n\
                 \x20 while i < 1i64 {{ let Inner {{ a, b }} = w.inner; n = n + a.len() + b.len(); i = i + 1; }}\n\
                 \x20 return n; }}\n"
            ),
            &["76"],
            "b49_destructure_proj_one_trip",
        );
}

/// B-2026-09-06-72 — a `shared` FIELD's 16-byte refcount block when the
/// owning struct travels out of a function inside an AGGREGATE.
///
/// `__karac_drop_struct_<T>` skips a direct `shared` / `Option[shared]`
/// scalar field by design (B-2026-06-14-28 #3): those are refcount
/// machinery, and the contract is that the value's OWN cleanup rc-decs
/// them. A struct `let` honours it (`track_struct_var_inst` registers the
/// combined drop), which is why `return r;` was always clean. Three
/// channels that are likewise a value's only cleanup did not, and each
/// resolved the memory drop with the bare value synthesis:
///
///   * a TUPLE element — `emit_tuple_elem_drops`;
///   * a BOXED enum payload — `track_boxed_enum_var` (the `Option` cell);
///   * the memory dispatcher's Drop-bearing-struct guard, which the inline
///     `Result`/`Option` payload drop funnels through (the `Result` cell).
///
/// All three now ask `sole_owner_struct_memory_drop`.
///
/// The `Drop` BODY was correct on every surface at both opt levels
/// throughout — this is memory only, so a body-count assertion would not
/// have caught it and does not guard it. The leak is what these assert.
///
/// `-O2` hid every cell (the block is dead-store-eliminated once nothing
/// reads it), which is the B-2026-09-07-36 rule: an `-O2`-only zero is
/// evidence of nothing for a leak-class cell.
///
/// SO THIS FIXTURE'S GUARD IS THE `-O0` LEG, not the default one, and that
/// is stated rather than assumed: measured against the pre-fix compiler it
/// PASSES on the default `-O2` run (LSan sees nothing, because the block
/// was never allocated) and FAILS under `scripts/asan-o0-leg.sh` with
/// `ERROR: LeakSanitizer` and exit 23. `assert_clean_asan_run` takes no
/// opt-level argument, so a reader checking this fixture on the default leg
/// alone would conclude it guards nothing — it is the -O0 leg that makes it
/// bite, exactly the case that leg exists for.
#[test]
fn asan_aggregate_returned_struct_releases_its_shared_field() {
    // `RET` is the return type, `WRAP` the returned expression, `READ` a
    // use of the payload — a never-read aggregate is dead-code-eliminated
    // at `-O2` and would assert nothing there.
    fn prog(ret: &str, wrap: &str, read: &str) -> String {
        format!(
                "shared struct Inner {{ v: i64 }}\n\
                 struct R {{ id: i64, name: String, inner: Inner }}\n\
                 impl Drop for R {{ fn drop(mut ref self) {{ println(f\"dR{{self.id}}\") }} }}\n\
                 fn mk(i: i64) -> R {{ return R {{ id: i, name: f\"h{{i}}\", inner: Inner {{ v: i }} }}; }}\n\
                 fn f(r: R) -> {ret} {{ return {wrap}; }}\n\
                 fn main() {{ let z = f(mk(20)); {read} }}\n"
            )
    }

    // 1-3 — the row's two cells plus the `Result` spelling found with them.
    //       Each lost 16 B in 1 block at -O0 (12 allocs / 11 frees).
    for (label, ret, wrap, read) in [
            (
                "b72-tuple",
                "(R, i64)",
                "(r, 9)",
                "println(f\"{z.0.inner.v}\");",
            ),
            (
                "b72-option",
                "Option[R]",
                "Option.Some(r)",
                "match z { Option.Some(x) => println(f\"{x.inner.v}\"), Option.None => println(\"n\") }",
            ),
            (
                "b72-result",
                "Result[R, i64]",
                "Result.Ok(r)",
                "match z { Result.Ok(x) => println(f\"{x.inner.v}\"), Result.Err(e) => println(\"e\") }",
            ),
        ] {
            assert_clean_asan_run(&prog(ret, wrap, read), &["20", "dR20"], label);
        }

    // 4 — the CONTROL that localized it: the same function returning the
    //     struct BARE was clean before this change and must stay clean. A
    //     regression here means the combined drop reached a value whose
    //     `let` cleanup already rc-decs it, i.e. a double release.
    assert_clean_asan_run(
        &prog("R", "r", "println(f\"{z.inner.v}\");"),
        &["20", "dR20"],
        "b72-bare-return-control",
    );

    // 5 — UNBOUNDED, which is what makes a 16-byte leak worth fixing: the
    //     block is lost once per evaluation, not once per program.
    assert_clean_asan_run(
            "shared struct Inner { v: i64 }\n\
             struct R { id: i64, name: String, inner: Inner }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             fn mk(i: i64) -> R { return R { id: i, name: f\"h{i}\", inner: Inner { v: i } }; }\n\
             fn f(r: R) -> (R, i64) { return (r, 9); }\n\
             fn main() { let mut i = 0; while i < 3 { let z = f(mk(i)); i = i + 1; } println(\"done\"); }\n",
            &["dR0", "dR1", "dR2", "done"],
            "b72-loop-unbounded",
        );

    // 6 — a DESTRUCTURE of the returned tuple. The element is moved out, so
    //     `zero_tuple_elem_cap_at` must have disarmed the tuple's own walk
    //     over the same slot; a mismatch between that dual and the drop is
    //     a double free rather than a leak, and the tuple's route changed
    //     here (it now takes the TypeExpr path, not the LLVM-type one).
    assert_clean_asan_run(
        &prog("(R, i64)", "(r, 9)", "").replace(
            "let z = f(mk(20)); ",
            "let (a, b) = f(mk(20)); println(f\"{a.inner.v}{b}\");",
        ),
        &["209", "dR20"],
        "b72-destructure",
    );

    // 7 — a single-element move-out of the returned tuple into a by-value
    //     callee, the other half of that dual.
    assert_clean_asan_run(
        "shared struct Inner { v: i64 }\n\
             struct R { id: i64, name: String, inner: Inner }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             fn mk(i: i64) -> R { return R { id: i, name: f\"h{i}\", inner: Inner { v: i } }; }\n\
             fn f(r: R) -> (R, i64) { return (r, 9); }\n\
             fn g(r: R) -> i64 { return r.inner.v; }\n\
             fn main() { let z = f(mk(20)); let n = g(z.0); println(f\"{n}\"); }\n",
        &["dR20", "20"],
        "b72-elem-moveout",
    );

    // 8 — the aggregate return over a struct with NO shared field, and 9 —
    //     the shared field spelled `Option[shared]`. The first is the
    //     control that isolated the shared field as the cause (it was
    //     clean throughout); the second is the sibling classification the
    //     combined drop's pass 2 also covers, so a walker that handled one
    //     and not the other would show up here.
    assert_clean_asan_run(
        "struct R2 { id: i64, name: String }\n\
             impl Drop for R2 { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             fn mk2(i: i64) -> R2 { return R2 { id: i, name: f\"h{i}\" }; }\n\
             fn f(r: R2) -> (R2, i64) { return (r, 9); }\n\
             fn main() { let z = f(mk2(20)); println(f\"{z.0.id}\"); }\n",
        &["20", "dR20"],
        "b72-no-shared-field-control",
    );
    assert_clean_asan_run(
            "shared struct Inner { v: i64 }\n\
             struct R { id: i64, name: String, inner: Option[Inner] }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             fn mk(i: i64) -> R { return R { id: i, name: f\"h{i}\", inner: Option.Some(Inner { v: i }) }; }\n\
             fn f(r: R) -> (R, i64) { return (r, 9); }\n\
             fn main() { let z = f(mk(20)); println(\"ok\"); }\n",
            &["dR20", "ok"],
            "b72-option-shared-field",
        );

    // 10 — the same shape with NO `impl Drop` at all. It reaches a
    //      different resolver (the dispatcher's guard is keyed on
    //      `drop_method_keys`), and was already clean; it is here so a
    //      later narrowing of that key cannot silently drop it.
    assert_clean_asan_run(
        "shared struct Inner { v: i64 }\n\
             struct R { id: i64, name: String, inner: Inner }\n\
             fn mk(i: i64) -> R { return R { id: i, name: f\"h{i}\", inner: Inner { v: i } }; }\n\
             fn f(r: R) -> (R, i64) { return (r, 9); }\n\
             fn main() { let z = f(mk(20)); println(f\"{z.0.inner.v}\"); }\n",
        &["20"],
        "b72-no-user-drop",
    );
}

/// B-2026-09-25-31 — a struct with a `shared` field declines copy support, so
/// a by-value callee FORWARDS it, and a hand-back returns the caller's own
/// object. The caller kept its cleanup beside the result's: `let t = idg(s)`
/// read and wrote a freed block at -O0 and aborted in `malloc` under `karac
/// run`, generic or concrete, with or without a `Drop` of its own. Body and
/// memory now move to the result together wherever the callee hands the value
/// back whole (or inside a struct) on every path, or pushes it into a
/// container of its own on every path; the discarded spellings (`h_`, `i_`,
/// `j_`) are the result's only owner.
#[test]
fn asan_struct_with_shared_field_handed_back_has_one_owner() {
    assert_clean_asan_run(
        r#"shared struct Sh { k: i64 }
struct S2 { h: Sh, id: i64 }
impl Drop for S2 { fn drop(mut ref self) { println(f"dS{self.id}") } }
struct S3 { h: Sh, id: i64 }
struct Bx[T] { v: T }
struct K { n: i64 }
impl K { fn keep[T](ref self, x: T) -> T { return x } }
fn idg[T](v: T) -> T { return v }
fn idS3(v: S3) -> S3 { return v }
fn wrap[T](v: T) -> Bx[T] { return Bx { v: v } }
fn in2[T](v: T) -> T { let t = v; return t }
fn in5[T](v: T) -> T { let t = idg(v); t }
fn st[T](a: T) -> Vec[T] { let mut v: Vec[T] = Vec.new(); v.push(a); return v }
fn stS3(a: S3) -> Vec[S3] { let mut v: Vec[S3] = Vec.new(); v.push(a); return v }
fn a_gen_drop() { let s = S2 { h: Sh { k: 1 }, id: 1 }; let t = idg(s); println(f"t{t.id}") }
fn b_gen_plain() { let s = S3 { h: Sh { k: 1 }, id: 2 }; let t = idg(s); println(f"t{t.id}") }
fn c_conc_plain() { let s = S3 { h: Sh { k: 1 }, id: 3 }; let t = idS3(s); println(f"t{t.id}") }
fn d_wrap_drop() { let s = S2 { h: Sh { k: 1 }, id: 4 }; let b = wrap(s); println(f"b{b.v.id}") }
fn e_wrap_plain() { let s = S3 { h: Sh { k: 1 }, id: 5 }; let b = wrap(s); println(f"b{b.v.id}") }
fn f_method() { let k = K { n: 0 }; let s = S2 { h: Sh { k: 1 }, id: 6 }; let t = k.keep(s); println(f"t{t.id}") }
fn g_shared_alive() { let sh = Sh { k: 7 }; let s = S2 { h: sh, id: 7 }; let t = idg(s); println(f"t{t.id} {sh.k}") }
fn h_conc_disc() { let s = S3 { h: Sh { k: 1 }, id: 8 }; idS3(s); println("h") }
fn i_conc_letdisc() { let s = S3 { h: Sh { k: 1 }, id: 9 }; let _ = idS3(s); println("i") }
fn j_gen_disc() { let s = S3 { h: Sh { k: 1 }, id: 10 }; idg(s); let r = S2 { h: Sh { k: 1 }, id: 11 }; let _ = idg(r); println("j") }
fn k_rebind() { let s = S3 { h: Sh { k: 1 }, id: 12 }; let t = in2(s); let u = S2 { h: Sh { k: 1 }, id: 13 }; let w = in5(u); println(f"k{t.id} {w.id}") }
fn l_push() { let s = S3 { h: Sh { k: 1 }, id: 14 }; let v = st(s); let c = S3 { h: Sh { k: 1 }, id: 15 }; let w = stS3(c); println(f"l{v.len()} {w.len()}") }
fn m_loop() { for i in 0..3 { let s = S3 { h: Sh { k: i }, id: 16 }; let t = idg(s); println(f"m{t.h.k}") } }

fn main() {
    a_gen_drop()
    b_gen_plain()
    c_conc_plain()
    d_wrap_drop()
    e_wrap_plain()
    f_method()
    g_shared_alive()
    h_conc_disc()
    i_conc_letdisc()
    j_gen_disc()
    k_rebind()
    l_push()
    m_loop()
    println("end")
}
"#,
        &[
            "t1", "dS1", "t2", "t3", "b4", "dS4", "b5", "t6", "dS6", "t7 7", "dS7", "h", "i",
            "dS11", "j", "k12 13", "dS13", "l1 1", "m0", "m1", "m2", "end",
        ],
        "asan_struct_with_shared_field_handed_back_has_one_owner",
    );
}

/// B-2026-09-25-37 — a struct with a `shared` field and NO `Drop` of its
/// own is forwarded by a by-value callee, which never frees it, so its only
/// owner is the caller's memory-only `StructDrop`. B-2026-09-25-31 moved that
/// to the result on the free-fn every-path shapes; the METHOD and ASSOC legs
/// (`a_`, `b_`) and the MIXED-PATH shapes (`c_`..`g_`: a conditional
/// hand-back or store, where the callee now owns the memory per path under the
/// conditional-move flag exactly as it already did for a `Drop`-bearing struct)
/// read and wrote a freed block. `h_` pins an `Option` result, which stays with
/// the caller because its payload cannot release the `shared` field.
#[test]
fn asan_dropless_shared_field_struct_on_method_and_mixed_paths_has_one_owner() {
    assert_clean_asan_run(
        r#"shared struct Sh { k: i64 }
struct S3 { h: Sh, id: i64 }
struct K { n: i64 }
impl K {
    fn keep3(ref self, v: S3) -> S3 { return v }
    fn akeep3(v: S3) -> S3 { return v }
    fn pk(ref self, v: S3, c: bool, w: S3) -> S3 { if c { return v } return w }
}
struct Hold { xs: Vec[S3] }
impl Hold { fn maybe(mut ref self, r: S3, k: bool) { if k { self.xs.push(r); } } }
fn mk(i: i64) -> S3 { return S3 { h: Sh { k: i }, id: i } }
fn pickS3(v: S3, c: bool, w: S3) -> S3 { if c { return v } return w }
fn keep1(v: S3, c: bool) -> S3 { if c { return v } return mk(99) }
fn stcS3(a: S3, c: bool) -> Vec[S3] { let mut v: Vec[S3] = Vec.new(); if c { v.push(a) } return v }
fn midS3(v: S3, c: bool) -> Option[S3] { if c { return Some(v) } return None }
fn a_method() { let k = K { n: 0 }; let s = mk(1); let t = k.keep3(s); let u = mk(2); let w = K.akeep3(u); println(f"a{t.id} {w.id}") }
fn b_method_disc() { let k = K { n: 0 }; let s = mk(3); k.keep3(s); let u = mk(4); let _ = K.akeep3(u); println("b") }
fn c_pick() { let s = mk(5); let w = mk(6); let t = pickS3(s, true, w); let s2 = mk(7); let w2 = mk(8); let t2 = pickS3(s2, false, w2); println(f"c{t.id} {t2.id}") }
fn d_keep1() { let s = mk(9); let t = keep1(s, true); let s2 = mk(10); let t2 = keep1(s2, false); let t3 = keep1(mk(11), false); println(f"d{t.id} {t2.id} {t3.id}") }
fn e_method_pick() { let k = K { n: 0 }; let s = mk(12); let w = mk(13); let t = k.pk(s, false, w); println(f"e{t.id}") }
fn f_store() { let s = mk(14); let v = stcS3(s, true); let s2 = mk(15); let v2 = stcS3(s2, false); println(f"f{v.len()} {v2.len()}") }
fn g_self_store() { let mut h = Hold { xs: Vec.new() }; let s = mk(16); h.maybe(s, true); let s2 = mk(17); h.maybe(s2, false); println(f"g{h.xs.len()}") }
fn h_option() { let s = mk(18); let o = midS3(s, true); let s2 = mk(19); let o2 = midS3(s2, false); println("h") }
fn i_loop() { for i in 0..3 { let s = mk(i); let t = keep1(s, i == 1); println(f"i{t.id}") } }

fn main() {
    a_method()
    b_method_disc()
    c_pick()
    d_keep1()
    e_method_pick()
    f_store()
    g_self_store()
    h_option()
    i_loop()
    println("end")
}
"#,
        &[
            "a1 2", "b", "c5 8", "d9 99 99", "e13", "f1 0", "g1", "h", "i99", "i1", "i99", "end",
        ],
        "asan_dropless_shared_field_struct_on_method_and_mixed_paths_has_one_owner",
    );
}

/// B-2026-09-25-41 — a by-value param handed on to a callee that returns it
/// on only some paths (`fn passp(a: S3, c: bool) -> S3 { .. return pickS3(a,
/// c, w) }`). Two faults: the auto-par body path compiles a function's LAST
/// expression without the conditional-store disarm `compile_block`'s tail has,
/// so `passp`'s per-path flag stayed armed across the hand-over (`a_` and `b_`
/// true: a use after free once the result freed the memory, and a second `dS3`
/// body; `b_` false: `dS4` twice); and the hand-back gate declined every
/// enclosing-frame param, so `passp` switched `pickS3`'s per-path owner off for
/// EVERY caller -- `h_`'s direct call read a freed block in a program merely
/// containing `passp`, and the false paths of `c_`, `d_` and `f_` leaked the
/// value nobody freed. `e_` is a `Drop` struct with a `String` field, clean
/// before and after; `g_` a fresh temp and a loop.
#[test]
fn asan_param_handed_on_to_mixed_path_callee_has_one_owner() {
    assert_clean_asan_run(
        r#"shared struct Sh { k: i64 }
struct S2 { h: Sh, id: i64 }
impl Drop for S2 { fn drop(mut ref self) { println(f"dS{self.id}") } }
struct S3 { h: Sh, id: i64 }
struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn pickS3(v: S3, c: bool, w: S3) -> S3 { if c { return v } return w }
fn pickS2(v: S2, c: bool, w: S2) -> S2 { if c { return v } return w }
fn keepR(r: R, c: bool) -> R { if c { return r } return R { id: 99, tag: "z" } }
fn keep3(v: S3, c: bool) -> S3 { if c { return v } return S3 { h: Sh { k: 9 }, id: 99 } }
fn keep2(v: S2, c: bool) -> S2 { if c { return v } return S2 { h: Sh { k: 9 }, id: 99 } }
fn mk(i: i64) -> S3 { return S3 { h: Sh { k: i }, id: i } }
fn mk2(i: i64) -> S2 { return S2 { h: Sh { k: i }, id: i } }
fn passp(a: S3, c: bool) -> S3 { let w = mk(98); return pickS3(a, c, w) }
fn passp2(a: S2, c: bool) -> S2 { let w = mk2(98); return pickS2(a, c, w) }
fn letret(a: S3, c: bool) -> S3 { let w = mk(98); let t = pickS3(a, c, w); return t }
fn stmt3(a: S3, c: bool) { keep3(a, c); println("s3") }
fn stmt2(a: S2, c: bool) { keep2(a, c); println("s2") }
fn stmtR(a: R, c: bool) { keepR(a, c); println("sR") }
fn letR(a: R, c: bool) -> i64 { let t = keepR(a, c); return t.id }
fn nest3(a: S3, c: bool, j: bool) { if j { let t = keep3(a, c); println(f"n{t.id}") } println("nx") }
fn nest2(a: S2, c: bool, j: bool) { if j { let t = keep2(a, c); println(f"n{t.id}") } println("nx") }
fn a_pass() { let s = mk(1); let t = passp(s, true); let s2 = mk(2); let t2 = passp(s2, false); println(f"a{t.id} {t2.id}") }
fn b_pass2() { let s = mk2(3); let t = passp2(s, true); println(f"b{t.id}"); let s2 = mk2(4); let t2 = passp2(s2, false); println(f"b{t2.id}") }
fn c_letret() { let s = mk(5); let t = letret(s, true); let s2 = mk(6); let t2 = letret(s2, false); println(f"c{t.id} {t2.id}") }
fn d_stmt() { let s = mk(7); stmt3(s, true); let s2 = mk(8); stmt3(s2, false); let s3 = mk2(9); stmt2(s3, true); let s4 = mk2(10); stmt2(s4, false) }
fn e_stmtR() { let s = R { id: 11, tag: "a" }; stmtR(s, true); let s2 = R { id: 12, tag: "b" }; stmtR(s2, false); let s3 = R { id: 13, tag: "c" }; println(f"e{letR(s3, true)}") }
fn f_nest() { let s = mk(14); nest3(s, true, true); let s2 = mk(15); nest3(s2, false, true); let s3 = mk(16); nest3(s3, true, false); let s4 = mk2(17); nest2(s4, true, true); let s5 = mk2(18); nest2(s5, true, false) }
fn g_temp_loop() { let t = passp(mk(21), true); println(f"g{t.id}"); for i in 0..3 { let s = mk(i); let u = passp(s, i == 1); println(f"g{u.id}") } }
fn h_direct() { let s = mk(22); let w = mk(23); let t = pickS3(s, true, w); let s2 = mk2(24); let w2 = mk2(25); let t2 = pickS2(s2, false, w2); println(f"h{t.id} {t2.id}") }
fn main() {
    a_pass()
    b_pass2()
    c_letret()
    d_stmt()
    e_stmtR()
    f_nest()
    g_temp_loop()
    h_direct()
    println("end")
}
"#,
        &[
            "a1 98", "dS98", "b3", "dS3", "dS4", "b98", "dS98", "c5 98", "s3", "s3", "dS9", "s2",
            "dS10", "dS99", "s2", "dR11", "sR", "dR12", "dR99", "sR", "dR13", "e13", "n14", "nx",
            "n99", "nx", "nx", "n17", "dS17", "nx", "nx", "dS18", "g21", "g98", "g1", "g98",
            "dS24", "h22 25", "dS25", "end",
        ],
        "asan_param_handed_on_to_mixed_path_callee_has_one_owner",
    );
}

/// B-2026-09-25-39 — an `Option` or generic-enum payload that is a struct
/// with a `shared` field. `let o = Some(S3 { h: Sh { .. }, id })` had no memory
/// owner for an INLINE struct payload: the let path ran only the overlay
/// registrars, the move into `Some` had already stood the source down, so the
/// `Sh` box was never released (16 B per value at -O0, `a_`..`c_`, `e_`,
/// `j_`, `k_`). The new owner is the tag-guarded `karac_drop_Option_<S>`
/// `EnumDrop`, which every hand-over has to disarm: `unwrap`/`expect`
/// (`f_`), a container store or tuple (`g_`, `l_`), a rebind (`h_`), a
/// `return` (`m`), and a reassignment has to free the displaced value first
/// (`i_`). `d_` is the generic enum box, whose interior drop was the plain
/// value drop that skips a `shared` field by contract. `n_` is the other
/// direction: a payload that is still a caller-retained param's memory
/// (`Some(a)` / `Ho.Full(a)` over `a: S3`, or `wrap3(s)` handing `s` back) must
/// NOT get the new owner, or it is freed twice.
#[test]
fn asan_option_or_generic_enum_payload_with_shared_field_is_released() {
    assert_clean_asan_run(
        r#"shared struct Sh { k: i64 }
struct S2 { h: Sh, id: i64 }
impl Drop for S2 { fn drop(mut ref self) { println(f"dS{self.id}") } }
struct S3 { h: Sh, id: i64 }
struct P6 { e: En, id: i64 }
enum En { A(Sh), B }
enum Ho[T] { Full(T), Empty }
struct G2[T] { v: T, id: i64 }
struct W { p: Option[S2] }
fn mk(i: i64) -> S3 { return S3 { h: Sh { k: i }, id: i } }
fn mk2(i: i64) -> S2 { return S2 { h: Sh { k: i }, id: i } }
fn a_some() { let s = mk(1); let o = Some(s); println(f"a{o.is_some()}") }
fn b_drop() { let o = Some(mk2(2)); println(f"b{o.is_some()}") }
fn c_temp() { let o = Some(S3 { h: Sh { k: 3 }, id: 3 }); println(f"c{o.is_some()}") }
fn d_generic() { let s = mk(4); let o = Ho.Full(s); let g = Ho.Full(G2 { v: Sh { k: 4 }, id: 4 }); println("d") }
fn e_match() { let o = Some(mk(5)); match o { Some(x) => println(f"e{x.id}"), None => println("eN") } }
fn f_unwrap() { let o = Some(mk(6)); let x = o.unwrap(); let p = Some(mk2(6)); let y = p.expect("no"); println(f"f{x.id}{y.id}") }
fn g_push() { let mut v: Vec[Option[S2]] = Vec.new(); let o = Some(mk2(7)); v.push(o); let t = (Some(mk2(8)), 1); println(f"g{v.len()}{t.1}") }
fn h_rebind() { let o = Some(mk2(9)); let p = o; println(f"h{p.is_some()}") }
fn i_reassign() { let mut o = Some(mk2(10)); o = None; let mut q = Some(mk2(11)); q = Some(mk2(12)); println("i") }
fn j_loop() { for i in 0..3 { let o = Some(mk(i)); println(f"j{o.is_some()}") } }
fn k_enum() { let o = Some(En.A(Sh { k: 1 })); let p = Some(P6 { e: En.A(Sh { k: 2 }), id: 2 }); println(f"k{o.is_some()}{p.is_some()}") }
fn l_moves() { let mut m: Map[i64, Option[S2]] = Map.new(); let o = Some(mk2(13)); m.insert(1, o); let p = Some(mk2(14)); let w = W { p: p }; println(f"l{m.len()}") }
fn ret() -> Option[S2] { let o = Some(mk2(15)); return o }
fn midS3(v: S3, c: bool) -> Option[S3] { if c { return Some(v) } return None }
fn wrap3(v: S3) -> Option[S3] { return Some(v) }
fn inner3(a: S3) { let o = Some(a); println(f"n{o.is_some()}") }
fn inner2(a: S2) { let o = Some(a); println(f"n{o.is_some()}") }
fn innerho(a: S3) { let o = Ho.Full(a); println("nho") }
fn n_params() { let s = mk(18); let o = midS3(s, true); let s2 = mk(19); let o2 = midS3(s2, false); let s3 = mk(20); let o3 = wrap3(s3); let s4 = mk(21); inner3(s4); let s5 = mk2(22); inner2(s5); let s6 = mk(23); innerho(s6); println("n") }
fn main() {
    a_some()
    b_drop()
    c_temp()
    d_generic()
    e_match()
    f_unwrap()
    g_push()
    h_rebind()
    i_reassign()
    j_loop()
    k_enum()
    l_moves()
    n_params()
    let r = ret();
    println(f"m{r.is_some()}")
    println("end")
}
"#,
        &[
            "atrue",
            "btrue",
            "dS2",
            "ctrue",
            "d",
            "e5",
            "f66",
            "dS6",
            "g11",
            "dS8",
            "dS7",
            "htrue",
            "dS9",
            "dS10",
            "dS11",
            "dS12",
            "i",
            "jtrue",
            "jtrue",
            "jtrue",
            "ktruetrue",
            "dS14",
            "l1",
            "dS13",
            "ntrue",
            "ntrue",
            "dS22",
            "nho",
            "n",
            "mtrue",
            "dS15",
            "end",
        ],
        "asan_option_or_generic_enum_payload_with_shared_field_is_released",
    );
}

/// B-2026-09-25-38 — a `Drop` struct with a `shared` field (so the callee
/// FORWARDS it rather than entry-copying) handed to a generic fn that returns
/// it inside an enum. `let h = mkh(s)` over `fn mkh[T](v: T) -> Ho[T]` and
/// `mid(s, c)` over `fn mid[T](v: T, c: bool) -> Option[T]` ran the body twice
/// on every compiled surface (`a_`..`c_`, `i_`, `j_`): the result's payload
/// walk (or the callee's per-path flag, on `None`) plus the caller's own
/// wrapper. The caller now gives up the BODY and keeps the memory, which the
/// result aliases. That exposed three places the body then ran nowhere, all
/// covered here: a discarded result (`d_`, `e_`, which also leaked the box),
/// and a match arm whose binding only borrows the payload (`f_`..`h_`, `k_`,
/// B-2026-09-26-10's cells), where the arm-binding site registers no drop for
/// a struct that declines copy support, so the scrutinee keeps its walk.
#[test]
fn asan_generic_enum_handback_of_shared_field_drop_struct_runs_body_once() {
    assert_clean_asan_run(
        r#"shared struct Sh { k: i64 }
struct S2 { h: Sh, id: i64 }
impl Drop for S2 { fn drop(mut ref self) { println(f"dS{self.id}") } }
enum Ho[T] { Full(T), Empty }
struct H { n: i64 }
impl H { fn wrap[T](ref self, v: T) -> Option[T] { return Some(v) } }
fn mk2(i: i64) -> S2 { return S2 { h: Sh { k: i }, id: i } }
fn mkh[T](v: T) -> Ho[T] { return Ho.Full(v); }
fn mid[T](v: T, c: bool) -> Option[T] { if c { return Some(v) } return None }
fn rd(x: ref S2) { println(f"rd{x.id}") }
fn eat2(s: S2) { println(f"x{s.id}") }
fn a_bound() { let s = mk2(1); let h = mkh(s); println("a") }
fn b_mid_some() { let s = mk2(2); let o = mid(s, true); println("b") }
fn c_mid_none() { let s = mk2(3); let o = mid(s, false); println("c") }
fn d_discard() { let s = mk2(4); mkh(s); println("d") }
fn e_underscore() { let s = mk2(5); let _ = mkh(s); println("e") }
fn f_match_ho() {
    let s = mk2(6); let h = mkh(s); println("f")
    match h { Ho.Full(x) => println(f"got{x.id}"), Ho.Empty => println("none") }
    println("f after")
}
fn g_match_opt() {
    let s = mk2(7); let o = mid(s, true); println("g")
    match o { Some(x) => println(f"got{x.id}"), None => println("none") }
    println("g after")
}
fn h_ref_arm() {
    let s = mk2(8); let o = mid(s, true);
    match o { Some(x) => rd(x), None => println("none") }
    let t = mk2(9); let k = mkh(t);
    match k { Ho.Full(x) => rd(x), Ho.Empty => println("none") }
    println("h after")
}
fn i_loop() { for i in 10..12 { let s = mk2(i); let h = mkh(s); println(f"i{i}") } }
fn j_method() { let hh = H { n: 1 }; let s = mk2(13); let o = hh.wrap(s); println("j") }
fn k_local() {
    let o = Some(mk2(14)); if let Some(x) = o { println(f"k{x.id}") }
    let p = Some(mk2(15)); match p { Some(x) => eat2(x), None => println("none") }
    let q = Some(mk2(16)); match q { Some(x) => rd(x), None => println("none") }
}
fn main() {
    a_bound()
    b_mid_some()
    c_mid_none()
    d_discard()
    e_underscore()
    f_match_ho()
    g_match_opt()
    h_ref_arm()
    i_loop()
    j_method()
    k_local()
    println("end")
}
"#,
        &[
            "dS1", "a", "dS2", "b", "dS3", "c", "dS4", "d", "dS5", "e", "f", "got6", "dS6",
            "f after", "g", "got7", "dS7", "g after", "rd8", "dS8", "rd9", "dS9", "h after",
            "dS10", "i10", "dS11", "i11", "dS13", "j", "k14", "dS14", "x15", "dS15", "rd16",
            "dS16", "end",
        ],
        "asan_generic_enum_handback_of_shared_field_drop_struct_runs_body_once",
    );
}
