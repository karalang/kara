//! fixed-size arrays and SoA element storage -- fixtures for `tests/codegen.rs`.
//!
//! Split out of `tests/codegen.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test codegen` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test codegen arrays::
//!
//! New fixtures about fixed-size arrays and SoA element storage belong in this file.

use super::*;

/// B-2026-09-19-58 — a NAMED `Array` local moved into a seeded-pair
/// constructor used DIRECTLY as a `match` scrutinee double-freed every
/// element buffer: `free(): double free detected in tcache 2` on the JIT
/// and at `-O0`, against a correct `--interp`. Two owners of one buffer
/// set — the local's own `StructDrop` and the box's interior walk — and
/// only the second was ever armed on purpose.
///
/// The cells vary the two things that decide it. `m/arr`, `m/wild`,
/// `m/str`, `m/res` and `m/one` are the shapes that aborted; note that
/// `m/str`'s element runs no user `Drop` at all, so the boundary is heap
/// ownership rather than a `Drop` body. The `b/` cells are the ones that
/// must NOT move: `b/noheap` arms no interior walk, `b/fresh` has no named
/// source to retract, `b/let` was already correct through the `let` site's
/// own retraction, and `b/mono` is a user enum, which this path declines.
///
/// The INLINE-payload width is deliberately absent. `Array[S, 1]` is three
/// words, fits the `Option` payload area, never boxes, and aborts on both
/// the `match` AND the `let` spelling — a different defect on the other
/// side of the same width gate, filed as its own row. Carrying it here
/// would pin a crash as expected output.
#[test]
fn e2e_named_array_local_into_seeded_match_scrutinee_has_one_owner() {
    let Some(out) = run_program(
        r#"struct S { tag: String }
impl Drop for S { fn drop(mut ref self) { println(f"  dS{self.tag}") } }
struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
struct N { id: i64 }
impl Drop for N { fn drop(mut ref self) { println(f"  dN{self.id}") } }
enum W { P(Array[S, 2]), Q }
fn mka() -> Array[S, 2] { return [S { tag: f"gggggggg0" }, S { tag: f"gggggggg1" }] }
fn main() {
    println("m/arr");    { let a: Array[S, 2] = [S { tag: f"aaaaaaaa0" }, S { tag: f"aaaaaaaa1" }]; match Option.Some(a) { Option.Some(v) => { println(f"  r:{v[0].tag}") }, Option.None => { println("  n") } } }
    println("m/wild");   { let a: Array[S, 2] = [S { tag: f"bbbbbbbb0" }, S { tag: f"bbbbbbbb1" }]; match Option.Some(a) { Option.Some(_) => { println("  w") }, Option.None => { println("  n") } } }
    println("m/str");    { let a: Array[String, 2] = [f"cccccccc0", f"cccccccc1"]; match Option.Some(a) { Option.Some(v) => { println(f"  r:{v[0]}") }, Option.None => { println("  n") } } }
    println("m/res");    { let a: Array[S, 2] = [S { tag: f"dddddddd0" }, S { tag: f"dddddddd1" }]; match Result.Ok(a) { Result.Ok(v) => { println("  r") }, Result.Err(e) => { println("  n") } } }
    println("m/one");    { let a: Array[R, 1] = [R { id: 1, s: f"eeeeeeee0" }]; match Option.Some(a) { Option.Some(v) => { println("  r") }, Option.None => { println("  n") } } }
    println("b/noheap"); { let a: Array[N, 2] = [N { id: 2 }, N { id: 3 }]; match Option.Some(a) { Option.Some(v) => { println("  r") }, Option.None => { println("  n") } } }
    println("b/fresh");  { match Option.Some(mka()) { Option.Some(v) => { println(f"  r:{v[0].tag}") }, Option.None => { println("  n") } } }
    println("b/let");    { let a: Array[S, 2] = [S { tag: f"hhhhhhhh0" }, S { tag: f"hhhhhhhh1" }]; let o = Option.Some(a); match o { Option.Some(v) => { println("  r") }, Option.None => { println("  n") } } }
    println("b/mono");   { let a: Array[S, 2] = [S { tag: f"iiiiiiii0" }, S { tag: f"iiiiiiii1" }]; match W.P(a) { W.P(v) => { println("  r") }, W.Q => { println("  n") } } }
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "m/arr\n  r:aaaaaaaa0\n  dSaaaaaaaa0\n  dSaaaaaaaa1\nm/wild\n  w\nm/str\n  r:cccccccc0\nm/res\n  r\n  dSdddddddd0\n  dSdddddddd1\nm/one\n  r\n  dR1\nb/noheap\n  r\n  dN2\n  dN3\nb/fresh\n  r:gggggggg0\n  dSgggggggg0\n  dSgggggggg1\nb/let\n  r\n  dShhhhhhhh0\n  dShhhhhhhh1\nb/mono\n  r\nend\n", "got:\n{out}");
}

/// B-2026-09-19-61 — the BY-VALUE PARAM sibling of
/// `e2e_named_array_local_into_seeded_match_scrutinee_has_one_owner` above,
/// with the two owners on opposite sides of a FUNCTION BOUNDARY rather than
/// inside one frame.
///
/// `fn f(a: Array[S, 2]) { match Option.Some(a) { .. } }` armed the box's
/// interior walk over buffers the CALLER still owned, and the caller keeps
/// its `__karac_drop_array_te_S_2` on purpose: a user-`Drop` element fails
/// `array_param_elem_is_callee_owned`'s second conjunct, so the caller
/// retains (B-2026-09-14-25 / -27 measured what retracting there costs).
/// `free(): double free detected in tcache 2`, exit 134 at `-O0`, on six of
/// the ten cells below.
///
/// B-2026-09-19-58's disarm cannot reach it: that row pairs the arming with
/// a RETRACTION of the source's queued `StructDrop`, and a param the caller
/// retained has no such action in this frame, so the disarm no-ops and the
/// arming stands alone. The third branch is to DECLINE TO ARM —
/// `seeded_array_payload_stays_with_caller`, "a param of this function that
/// `owned_array_params` does NOT hold".
///
/// `p/noheap` carries the BODIES half and nothing else does: `N` owns no
/// heap, so it never aborted — it printed `dN2 dN3 dN2 dN3`, once from this
/// frame's arm binding and once from the caller's channel. Its single pair
/// here is what `pattern_binding_seeded_array_payload_stays_with_caller`
/// buys, and a memory-only fix leaves it doubled.
///
/// `p/str` and `p/gen` were ALREADY CLEAN on the unfixed compiler (measured,
/// exit 0) and pin the two directions the predicate must not widen into: an
/// `Array[String, N]` param IS callee-owned and its disarm works, and a
/// generic callee never armed the walk. `b/local` is B-2026-09-19-58's own
/// shape and pins the non-regression.
///
/// Two neighbours are deliberately absent because they still abort here and
/// aborted on the unfixed compiler too, so neither is this fix's doing: a
/// USER-enum seeded scrutinee over the same param (`match W.P(a)`,
/// B-2026-09-22-6) and the same param moved into a STRUCT-LITERAL field
/// (`Box2 { v: a }`, B-2026-09-22-7).
///
/// `--interp` DIVERGES on every `p/` cell that binds a payload: it still
/// runs each element body twice, unchanged by this fix (which touches
/// codegen only) and filed as B-2026-09-22-8. That is why this fixture has
/// no interpreter twin.
#[test]
fn e2e_array_param_into_seeded_match_scrutinee_stays_with_caller() {
    let Some(out) = run_program(
        r#"struct S { tag: String }
impl Drop for S { fn drop(mut ref self) { println(f"  dS{self.tag}") } }
struct N { id: i64 }
impl Drop for N { fn drop(mut ref self) { println(f"  dN{self.id}") } }
fn p_arr(a: Array[S, 2]) -> i64 { match Option.Some(a) { Option.Some(v) => { println(f"  r:{v[0].tag}"); return 1 }, Option.None => { println("  n"); return 0 } } }
fn p_wild(a: Array[S, 2]) -> i64 { match Option.Some(a) { Option.Some(_) => { println("  w"); return 1 }, Option.None => { println("  n"); return 0 } } }
fn p_str(a: Array[String, 2]) -> i64 { match Option.Some(a) { Option.Some(v) => { println(f"  r:{v[0]}"); return 1 }, Option.None => { println("  n"); return 0 } } }
fn p_res(a: Array[S, 2]) -> i64 { match Result.Ok(a) { Result.Ok(v) => { println("  r"); return 1 }, Result.Err(e) => { println("  n"); return 0 } } }
fn p_gen[T](a: Array[T, 2]) -> i64 { match Option.Some(a) { Option.Some(v) => { println("  r"); return 1 }, Option.None => { println("  n"); return 0 } } }
fn p_three(a: Array[S, 3]) -> i64 { match Option.Some(a) { Option.Some(v) => { println("  r"); return 1 }, Option.None => { println("  n"); return 0 } } }
fn p_noheap(a: Array[N, 2]) -> i64 { match Option.Some(a) { Option.Some(v) => { println("  r"); return 1 }, Option.None => { println("  n"); return 0 } } }
fn p_nonefirst(a: Array[S, 2]) -> i64 { match Option.Some(a) { Option.None => { println("  n"); return 0 }, Option.Some(v) => { println("  r"); return 1 } } }
fn b_local() -> i64 { let a: Array[S, 2] = [S { tag: f"llllllll0" }, S { tag: f"llllllll1" }]; match Option.Some(a) { Option.Some(v) => { println("  r"); return 1 }, Option.None => { println("  n"); return 0 } } }
fn main() {
    println("p/arr");       { let a: Array[S, 2] = [S { tag: f"aaaaaaaa0" }, S { tag: f"aaaaaaaa1" }]; let z = p_arr(a); }
    println("p/wild");      { let a: Array[S, 2] = [S { tag: f"bbbbbbbb0" }, S { tag: f"bbbbbbbb1" }]; let z = p_wild(a); }
    println("p/str");       { let a: Array[String, 2] = [f"cccccccc0", f"cccccccc1"]; let z = p_str(a); }
    println("p/res");       { let a: Array[S, 2] = [S { tag: f"dddddddd0" }, S { tag: f"dddddddd1" }]; let z = p_res(a); }
    println("p/gen");       { let a: Array[S, 2] = [S { tag: f"eeeeeeee0" }, S { tag: f"eeeeeeee1" }]; let z = p_gen(a); }
    println("p/three");     { let a: Array[S, 3] = [S { tag: f"ffffffff0" }, S { tag: f"ffffffff1" }, S { tag: f"ffffffff2" }]; let z = p_three(a); }
    println("p/noheap");    { let a: Array[N, 2] = [N { id: 2 }, N { id: 3 }]; let z = p_noheap(a); }
    println("p/nonefirst"); { let a: Array[S, 2] = [S { tag: f"hhhhhhhh0" }, S { tag: f"hhhhhhhh1" }]; let z = p_nonefirst(a); }
    println("b/local");     { let z = b_local(); }
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "p/arr\n  r:aaaaaaaa0\n  dSaaaaaaaa0\n  dSaaaaaaaa1\np/wild\n  w\n  dSbbbbbbbb0\n  dSbbbbbbbb1\np/str\n  r:cccccccc0\np/res\n  r\n  dSdddddddd0\n  dSdddddddd1\np/gen\n  r\n  dSeeeeeeee0\n  dSeeeeeeee1\np/three\n  r\n  dSffffffff0\n  dSffffffff1\n  dSffffffff2\np/noheap\n  r\n  dN2\n  dN3\np/nonefirst\n  r\n  dShhhhhhhh0\n  dShhhhhhhh1\nb/local\n  r\n  dSllllllll0\n  dSllllllll1\nend\n", "got:\n{out}");
}

/// B-2026-09-22-6 — the USER-ENUM spelling of B-2026-09-19-61, and a
/// different arming path reached by the same disagreement.
///
/// `fn f(a: Array[S, 2]) { match W.P(a) { .. } }` over
/// `enum W { P(Array[S, 2]), Q }`. The caller keeps its
/// `__karac_drop_array_te_S_2` on purpose — a user-`Drop` element fails
/// `array_param_elem_is_callee_owned`'s second conjunct, because its bodies
/// ride a caller-side channel — and the callee's materialized scrutinee temp
/// freed the same element buffers through `__karac_drop_W`. `free(): double
/// free detected in tcache 2`, exit 134 at `-O0`, on six of the nine cells
/// below, against a CORRECT `--interp`.
///
/// B-2026-09-19-61's fix does not reach it: `Option`/`Result` carry their
/// payload in a box whose interior walk is armed by `array_arm_owns_interior`,
/// and a user enum reaches its payload through its own drop switch, which
/// never consults that gate.
///
/// THE BOX IS OURS AND THE INTERIOR IS THE CALLER'S, and
/// `__karac_drop_<E>`'s `BoxedArray` arm does both in one basic block:
/// `karac_drop_Array_S_2(box)` then `free(box)`. Only this frame can do the
/// second — the constructor malloc'd that box here. Declining the whole
/// registration was measured and is the mirror defect: every cell then leaked
/// exactly its box and nothing else (48 B for `Array[S, 2]`, 72 B for
/// `Array[S, 3]`, 16 B for an `Array[N, 2]` whose element owns no heap at
/// all). The arm's own sentinel cannot express the split either — it is the
/// box WORD, and zeroing it skips the free along with the walk. So the split
/// lives in a second function, `__karac_drop_<E>__boxonly`.
///
/// `b/str` IS THE CELL THAT FIXED THE PREDICATE, and it earned its place. The
/// gate was first written as B-2026-09-19-61's — membership of
/// `owned_array_params` — and that admitted this `Array[String, 2]` param,
/// whose element IS callee-owned and whose disarm works, leaking both its
/// element buffers (18 B in 2 blocks). The map is filled by
/// `make_array_param_callee_owned`, and a param it never reaches is absent for
/// reasons unrelated to this question. Asking
/// `array_param_elem_is_callee_owned` over the variant's declared payload
/// element instead is the question itself, and `b/str` is clean either side of
/// the fix.
///
/// `b/local` is B-2026-09-19-58's own shape and pins the non-regression: a
/// LOCAL source keeps the walking drop fn. Its `r` with no element body is a
/// pre-existing agreed gap on all four surfaces (the `b/mono` cell of
/// `..._named_array_local_into_seeded_match_scrutinee_has_one_owner` above
/// pins the same thing) — not this row's, and unchanged by it. `u/noheap` was
/// already clean on the unfixed compiler and pins the other direction: an
/// element owning no heap has nothing to double-free, and giving it the
/// box-only twin must keep it that way.
///
/// TWO NEIGHBOURS ARE DELIBERATELY ABSENT because they still abort here and
/// aborted on the unfixed compiler too, so neither is this fix's doing: a
/// TWO-FIELD variant (`W.P(Array[S, 2], String)`, B-2026-09-22-9) — the switch
/// frees a variant's whole payload in one arm, so standing the walk down there
/// would leak the sibling `String` — and a NAMED local holding the constructor
/// (`let o = W.P(a); match o`, B-2026-09-22-10), which is a different
/// registration altogether.
///
/// `--interp` diverges on `u/letelse` alone, where it runs that cell's element
/// bodies twice — unchanged by this fix, which touches codegen only, and the
/// same remainder B-2026-09-22-8 records for the seeded spelling.
/// That is why this fixture has no interpreter twin.
#[test]
fn e2e_array_param_into_user_enum_match_scrutinee_stays_with_caller() {
    let Some(out) = run_program(
        r#"struct S { tag: String }
impl Drop for S { fn drop(mut ref self) { println(f"  dS{self.tag}") } }
struct N { id: i64 }
impl Drop for N { fn drop(mut ref self) { println(f"  dN{self.id}") } }
enum W { P(Array[S, 2]), Q }
enum V { P(Array[String, 2]), Q }
enum U { P(Array[N, 2]), Q }
enum T3 { P(Array[S, 3]), Q }
fn u_bind(a: Array[S, 2]) -> i64 { match W.P(a) { W.P(v) => { println("  r"); return 1 }, W.Q => { println("  n"); return 0 } } }
fn u_wild(a: Array[S, 2]) -> i64 { match W.P(a) { W.P(_) => { println("  w"); return 1 }, W.Q => { println("  n"); return 0 } } }
fn u_read(a: Array[S, 2]) -> i64 { match W.P(a) { W.P(v) => { println(f"  r:{v[0].tag}"); return 1 }, W.Q => { println("  n"); return 0 } } }
fn u_iflet(a: Array[S, 2]) -> i64 { if let W.P(v) = W.P(a) { println("  r"); return 1 } else { return 0 } }
fn u_letelse(a: Array[S, 2]) -> i64 { let W.P(v) = W.P(a) else { return 0 }; println("  r"); return 1 }
fn u_three(a: Array[S, 3]) -> i64 { match T3.P(a) { T3.P(v) => { println("  r"); return 1 }, T3.Q => { println("  n"); return 0 } } }
fn u_noheap(a: Array[N, 2]) -> i64 { match U.P(a) { U.P(v) => { println("  r"); return 1 }, U.Q => { println("  n"); return 0 } } }
fn b_str(a: Array[String, 2]) -> i64 { match V.P(a) { V.P(v) => { println(f"  r:{v[0]}"); return 1 }, V.Q => { println("  n"); return 0 } } }
fn b_local() -> i64 { let a: Array[S, 2] = [S { tag: f"llllllll0" }, S { tag: f"llllllll1" }]; match W.P(a) { W.P(v) => { println("  r"); return 1 }, W.Q => { println("  n"); return 0 } } }
fn main() {
    println("u/bind");    { let a: Array[S, 2] = [S { tag: f"aaaaaaaa0" }, S { tag: f"aaaaaaaa1" }]; let z = u_bind(a); }
    println("u/wild");    { let a: Array[S, 2] = [S { tag: f"bbbbbbbb0" }, S { tag: f"bbbbbbbb1" }]; let z = u_wild(a); }
    println("u/read");    { let a: Array[S, 2] = [S { tag: f"cccccccc0" }, S { tag: f"cccccccc1" }]; let z = u_read(a); }
    println("u/iflet");   { let a: Array[S, 2] = [S { tag: f"dddddddd0" }, S { tag: f"dddddddd1" }]; let z = u_iflet(a); }
    println("u/letelse"); { let a: Array[S, 2] = [S { tag: f"eeeeeeee0" }, S { tag: f"eeeeeeee1" }]; let z = u_letelse(a); }
    println("u/three");   { let a: Array[S, 3] = [S { tag: f"ffffffff0" }, S { tag: f"ffffffff1" }, S { tag: f"ffffffff2" }]; let z = u_three(a); }
    println("u/noheap");  { let a: Array[N, 2] = [N { id: 2 }, N { id: 3 }]; let z = u_noheap(a); }
    println("b/str");     { let a: Array[String, 2] = [f"gggggggg0", f"gggggggg1"]; let z = b_str(a); }
    println("b/local");   { let z = b_local(); }
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "u/bind\n  r\n  dSaaaaaaaa0\n  dSaaaaaaaa1\nu/wild\n  w\n  dSbbbbbbbb0\n  dSbbbbbbbb1\nu/read\n  r:cccccccc0\n  dScccccccc0\n  dScccccccc1\nu/iflet\n  r\n  dSdddddddd0\n  dSdddddddd1\nu/letelse\n  r\n  dSeeeeeeee0\n  dSeeeeeeee1\nu/three\n  r\n  dSffffffff0\n  dSffffffff1\n  dSffffffff2\nu/noheap\n  r\n  dN2\n  dN3\nb/str\n  r:gggggggg0\nb/local\n  r\nend\n", "got:\n{out}");
}

/// B-2026-09-22-10 — the NAMED-LOCAL spelling of B-2026-09-22-6, which is a
/// different registration and was still aborting after that fix.
///
/// `let o = W.P(a); match o` over `fn f(a: Array[S, 2])` freed the caller's
/// element buffers a second time: `free(): double free detected in tcache 2`,
/// exit 134 at `-O0`, on eight of this fixture's thirteen cells against the
/// unfixed compiler. B-2026-09-22-6 fixes the MATERIALIZED scrutinee temp, and
/// a named scrutinee never reaches `materialize_freshtemp_enum_scrutinee` at
/// all — the constructor's drop is registered by the ordinary `let` path
/// instead, which armed the walking `__karac_drop_<E>` and, beside it, the
/// bodies-only `__karac_dropelems_enum_<E>` over the same buffers.
///
/// TWO WALKS, WHERE THE FIXED SITE HAD ONE, so this needed both halves. The
/// MEMORY half registers the same box-only twin that row emitted, through the
/// same predicate. The BODIES half is the sibling skip: the caller runs those
/// element bodies through its own retained `__karac_drop_array_te_<T>_<N>`, so
/// arming the walker here doubles them. `l/noheap` is the only cell that can
/// observe that half alone — its element owns no heap, so it never aborted,
/// and it printed `dN2 dN3 dN2 dN3` against the control's one pair.
///
/// `l/chain` PINS THE PROPAGATION. `let o2 = o;` retracts `o`'s action and
/// registers `o2`'s, so without inheriting the decision `o2` gets the walking
/// drop fn back and the double free returns; it stayed red when the two halves
/// above were first landed alone. The identical chain over a LOCAL array
/// source is correct on all four surfaces, which is what localises this to the
/// decision's propagation rather than to the move-out retraction.
///
/// `b/stale` IS THE COMPLEMENT, and it is load-bearing rather than decorative:
/// it rebinds the name to a LOCAL-sourced constructor and then chains off it,
/// so a mark left over from the first binding would hand the second value the
/// box-only twin and orphan its element heap. Verified non-vacuous by
/// injection — with the per-binding clear removed it loses `dSrrrrrrrr0/1`
/// entirely and leaks 18 B in 2 blocks. Two earlier spellings of this probe
/// (a plain shadow, and a shadow after a chain) were measured VACUOUS for it
/// and are deliberately not here: the mark is only ever read for a SOURCE
/// name, so a shadow whose RHS is a constructor call never consults it.
///
/// `b/str` pins the widening this must not take: an `Array[String, 2]` param
/// IS callee-owned, its disarm works, and it is clean either side of the fix.
/// `b/local` is the local-source non-regression, and note it keeps its element
/// bodies here where the fresh-temp sibling's `b/local` has none — a
/// pre-existing agreed gap at that other spelling, not this one. `b/ctl` is a
/// by-value callee that does nothing with the array, and is the oracle for
/// what one owner looks like. `b/fresh` is B-2026-09-22-6's own cell, so a
/// change to the shared predicate cannot quietly move that site.
///
/// ONE NEIGHBOUR IS DELIBERATELY ABSENT: a TWO-FIELD variant
/// (`W.P(Array[S, 2], String)`, B-2026-09-22-9), which still aborts here and
/// aborted on the unfixed compiler too. The switch frees a variant's whole
/// payload in one arm, so standing its walk down there would leak the sibling.
///
/// `--interp` runs every caller-retained cell's element bodies TWICE and so
/// diverges from all three compiled surfaces, which agree with each other. It
/// is unchanged by this fix, which touches codegen only, and is the same
/// remainder B-2026-09-22-8 records. That is why this fixture has no
/// interpreter twin.
#[test]
fn e2e_array_param_into_named_enum_local_match_scrutinee_stays_with_caller() {
    let Some(out) = run_program(
        r#"struct S { tag: String }
impl Drop for S { fn drop(mut ref self) { println(f"  dS{self.tag}") } }
struct N { id: i64 }
impl Drop for N { fn drop(mut ref self) { println(f"  dN{self.id}") } }
enum W { P(Array[S, 2]), Q }
enum V { P(Array[String, 2]), Q }
enum U { P(Array[N, 2]), Q }
enum T3 { P(Array[S, 3]), Q }
fn l_bind(a: Array[S, 2]) -> i64 { let o = W.P(a); match o { W.P(v) => { println("  r"); return 1 }, W.Q => { println("  n"); return 0 } } }
fn l_wild(a: Array[S, 2]) -> i64 { let o = W.P(a); match o { W.P(_) => { println("  w"); return 1 }, W.Q => { println("  n"); return 0 } } }
fn l_read(a: Array[S, 2]) -> i64 { let o = W.P(a); match o { W.P(v) => { println(f"  r:{v[0].tag}"); return 1 }, W.Q => { println("  n"); return 0 } } }
fn l_iflet(a: Array[S, 2]) -> i64 { let o = W.P(a); if let W.P(v) = o { println("  r"); return 1 } else { return 0 } }
fn l_letelse(a: Array[S, 2]) -> i64 { let o = W.P(a); let W.P(v) = o else { return 0 }; println("  r"); return 1 }
fn l_three(a: Array[S, 3]) -> i64 { let o = T3.P(a); match o { T3.P(v) => { println("  r"); return 1 }, T3.Q => { println("  n"); return 0 } } }
fn l_noheap(a: Array[N, 2]) -> i64 { let o = U.P(a); match o { U.P(v) => { println("  r"); return 1 }, U.Q => { println("  n"); return 0 } } }
fn l_chain(a: Array[S, 2]) -> i64 { let o = W.P(a); let o2 = o; match o2 { W.P(v) => { println("  r"); return 1 }, W.Q => { println("  n"); return 0 } } }
fn b_stale(a: Array[S, 2]) -> i64 {
    let o = W.P(a);
    let b: Array[S, 2] = [S { tag: f"rrrrrrrr0" }, S { tag: f"rrrrrrrr1" }];
    let o = W.P(b);
    let o2 = o;
    match o2 { W.P(v) => { println("  r"); return 1 }, W.Q => { println("  n"); return 0 } }
}
fn b_str(a: Array[String, 2]) -> i64 { let o = V.P(a); match o { V.P(v) => { println(f"  r:{v[0]}"); return 1 }, V.Q => { println("  n"); return 0 } } }
fn b_local() -> i64 { let a: Array[S, 2] = [S { tag: f"llllllll0" }, S { tag: f"llllllll1" }]; let o = W.P(a); match o { W.P(v) => { println("  r"); return 1 }, W.Q => { println("  n"); return 0 } } }
fn b_ctl(a: Array[S, 2]) -> i64 { println("  r"); return 1 }
fn b_freshtemp(a: Array[S, 2]) -> i64 { match W.P(a) { W.P(v) => { println("  r"); return 1 }, W.Q => { println("  n"); return 0 } } }
fn main() {
    println("l/bind");    { let a: Array[S, 2] = [S { tag: f"aaaaaaaa0" }, S { tag: f"aaaaaaaa1" }]; let z = l_bind(a); }
    println("l/wild");    { let a: Array[S, 2] = [S { tag: f"bbbbbbbb0" }, S { tag: f"bbbbbbbb1" }]; let z = l_wild(a); }
    println("l/read");    { let a: Array[S, 2] = [S { tag: f"cccccccc0" }, S { tag: f"cccccccc1" }]; let z = l_read(a); }
    println("l/iflet");   { let a: Array[S, 2] = [S { tag: f"dddddddd0" }, S { tag: f"dddddddd1" }]; let z = l_iflet(a); }
    println("l/letelse"); { let a: Array[S, 2] = [S { tag: f"eeeeeeee0" }, S { tag: f"eeeeeeee1" }]; let z = l_letelse(a); }
    println("l/three");   { let a: Array[S, 3] = [S { tag: f"ffffffff0" }, S { tag: f"ffffffff1" }, S { tag: f"ffffffff2" }]; let z = l_three(a); }
    println("l/noheap");  { let a: Array[N, 2] = [N { id: 2 }, N { id: 3 }]; let z = l_noheap(a); }
    println("l/chain");   { let a: Array[S, 2] = [S { tag: f"hhhhhhhh0" }, S { tag: f"hhhhhhhh1" }]; let z = l_chain(a); }
    println("b/stale");   { let a: Array[S, 2] = [S { tag: f"ssssssss0" }, S { tag: f"ssssssss1" }]; let z = b_stale(a); }
    println("b/str");     { let a: Array[String, 2] = [f"gggggggg0", f"gggggggg1"]; let z = b_str(a); }
    println("b/local");   { let z = b_local(); }
    println("b/ctl");     { let a: Array[S, 2] = [S { tag: f"jjjjjjjj0" }, S { tag: f"jjjjjjjj1" }]; let z = b_ctl(a); }
    println("b/fresh");   { let a: Array[S, 2] = [S { tag: f"kkkkkkkk0" }, S { tag: f"kkkkkkkk1" }]; let z = b_freshtemp(a); }
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "l/bind\n  r\n  dSaaaaaaaa0\n  dSaaaaaaaa1\nl/wild\n  w\n  dSbbbbbbbb0\n  dSbbbbbbbb1\nl/read\n  r:cccccccc0\n  dScccccccc0\n  dScccccccc1\nl/iflet\n  r\n  dSdddddddd0\n  dSdddddddd1\nl/letelse\n  r\n  dSeeeeeeee0\n  dSeeeeeeee1\nl/three\n  r\n  dSffffffff0\n  dSffffffff1\n  dSffffffff2\nl/noheap\n  r\n  dN2\n  dN3\nl/chain\n  r\n  dShhhhhhhh0\n  dShhhhhhhh1\nb/stale\n  r\n  dSrrrrrrrr0\n  dSrrrrrrrr1\n  dSssssssss0\n  dSssssssss1\nb/str\n  r:gggggggg0\nb/local\n  r\n  dSllllllll0\n  dSllllllll1\nb/ctl\n  r\n  dSjjjjjjjj0\n  dSjjjjjjjj1\nb/fresh\n  r\n  dSkkkkkkkk0\n  dSkkkkkkkk1\nend\n", "got:\n{out}");
}

/// B-2026-09-22-7 — a by-value `Array` param moved into a STRUCT-LITERAL FIELD
/// was freed by both the caller and the aggregate.
///
/// `let b = B1 { v: a }` over `fn f(a: Array[R, 2])` aborted with
/// `free(): double free detected in tcache 2`, exit 134 at `-O0`, on four of
/// this fixture's ten cells. The caller keeps its own
/// `__karac_drop_array_te_R_2` on purpose -- an element that runs a user
/// `Drop` fails `array_param_elem_is_callee_owned`'s second conjunct
/// (B-2026-09-14-25 / -27 measured what retracting there costs) -- and
/// `__karac_drop_struct_B1` walked the same buffers on top of it.
///
/// `suppress_array_binding_move_into_aggregate` is the retraction the family's
/// standing rule reaches for at a hand-off, and it is a NO-OP here: it retracts
/// a queued `StructDrop` for the source, and a by-value param the caller
/// retained has no such action in this frame, because the param-level gate
/// declined to register one. So the arming stands alone. The third branch is to
/// DECLINE TO ARM, per field.
///
/// `s/two` IS THE CELL THAT MATTERS, and it is why this row closed where its
/// enum sibling could not. `emit_struct_drop_synthesis_skipping` already masks
/// individual field indices out of the memory walk and folds the mask into its
/// cache key, so the array field's walk is declined while the sibling
/// `String`'s is kept -- `in:zz` reads that sibling back before the walk, which
/// is what makes the cell load-bearing rather than decorative. The enum
/// spelling (B-2026-09-22-9) was believed to have no such per-field form, on
/// the reading that its switch frees a variant's whole payload in ONE arm. That
/// reading was wrong and the row is now CLOSED: the box-only twin's flag is
/// consumed at one FIELD's interior walk inside the per-field loop, not at the
/// arm, so the sibling keeps its drop. See
/// `e2e_array_param_into_multi_field_enum_variant_stays_with_caller`.
///
/// `n/str` pins the widening this must not take: an `Array[String, 2]` param IS
/// callee-owned, its disarm works, and it is clean either side of the fix.
/// `b/local` is the local-source non-regression -- note its element bodies run
/// BEFORE `in`, unlike every param-sourced cell, which is a pre-existing
/// ordering difference measured identically on both arms and not this fix's.
/// `b/discard` is the discarded-literal shape `in_discarded_aggregate_tail`
/// carves out, clean both arms. `b/ctl` is a by-value callee that does nothing
/// with the array, and is the oracle for what one owner looks like: every fixed
/// cell prints its elements exactly once, as this one does.
///
/// `s/noheap` and `n/tuple` were clean on the unfixed compiler too and pin the
/// two directions the mask must not disturb -- an element owning no heap has
/// nothing to double-free, and a TUPLE destination reaches a different
/// registration that was already correct.
///
/// TWO NEIGHBOURS ARE DELIBERATELY ABSENT because they still abort here and
/// aborted on the unfixed compiler too, so neither is this fix's doing: the
/// struct binding RETURNED from the function, and a `Vec.push` destination.
/// Both were on the row's own NOT MEASURED list and both have their own rows.
///
/// `--interp` runs every caller-retained cell's element bodies TWICE and so
/// diverges from all three compiled surfaces, which agree with each other. It
/// is unchanged by this fix, which touches codegen only, and is the same
/// remainder B-2026-09-22-8 records. That is why this fixture has no
/// interpreter twin.
#[test]
fn e2e_array_param_into_struct_literal_field_stays_with_caller() {
    let Some(out) = run_program(
        r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"  d{self.id}") } }
struct N { id: i64 }
impl Drop for N { fn drop(mut ref self) { println(f"  dN{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i, s: f"aaa" } }
struct B1 { v: Array[R, 2] }
struct B2 { v: Array[R, 2], t: String }
struct B3 { v: Array[R, 3] }
struct B4 { v: Array[N, 2] }
struct B6 { v: Array[String, 2] }
fn s_bind(a: Array[R, 2]) -> i64 { let b = B1 { v: a }; println("  in"); return 7 }
fn s_two(a: Array[R, 2]) -> i64 { let b = B2 { v: a, t: f"zz" }; println(f"  in:{b.t}"); return 7 }
fn s_three(a: Array[R, 3]) -> i64 { let b = B3 { v: a }; println("  in"); return 7 }
fn s_noheap(a: Array[N, 2]) -> i64 { let b = B4 { v: a }; println("  in"); return 7 }
fn s_read(a: Array[R, 2]) -> i64 { let b = B1 { v: a }; println(f"  in:{b.v[0].id}"); return 7 }
fn n_str(a: Array[String, 2]) -> i64 { let b = B6 { v: a }; println(f"  in:{b.v[0]}"); return 7 }
fn n_tuple(a: Array[R, 2]) -> i64 { let b = (a, 1); println("  in"); return 7 }
fn b_local() -> i64 { let a: Array[R, 2] = [mkr(91), mkr(92)]; let b = B1 { v: a }; println("  in"); return 7 }
fn b_discard(a: Array[R, 2]) -> i64 { B1 { v: a }; println("  in"); return 7 }
fn b_ctl(a: Array[R, 2]) -> i64 { println("  in"); return 7 }
fn main() {
    println("s/bind");    { let a: Array[R, 2] = [mkr(1), mkr(2)]; let z = s_bind(a); }
    println("s/two");     { let a: Array[R, 2] = [mkr(11), mkr(12)]; let z = s_two(a); }
    println("s/three");   { let a: Array[R, 3] = [mkr(21), mkr(22), mkr(23)]; let z = s_three(a); }
    println("s/noheap");  { let a: Array[N, 2] = [N { id: 31 }, N { id: 32 }]; let z = s_noheap(a); }
    println("s/read");    { let a: Array[R, 2] = [mkr(41), mkr(42)]; let z = s_read(a); }
    println("n/str");     { let a: Array[String, 2] = [f"s51", f"s52"]; let z = n_str(a); }
    println("n/tuple");   { let a: Array[R, 2] = [mkr(71), mkr(72)]; let z = n_tuple(a); }
    println("b/local");   { let z = b_local(); }
    println("b/discard"); { let a: Array[R, 2] = [mkr(101), mkr(102)]; let z = b_discard(a); }
    println("b/ctl");     { let a: Array[R, 2] = [mkr(111), mkr(112)]; let z = b_ctl(a); }
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "s/bind\n  in\n  d1\n  d2\ns/two\n  in:zz\n  d11\n  d12\ns/three\n  in\n  d21\n  d22\n  d23\ns/noheap\n  in\n  dN31\n  dN32\ns/read\n  in:41\n  d41\n  d42\nn/str\n  in:s51\nn/tuple\n  in\n  d71\n  d72\nb/local\n  d91\n  d92\n  in\nb/discard\n  in\n  d101\n  d102\nb/ctl\n  in\n  d111\n  d112\nend\n", "got:\n{out}");
}

/// B-2026-09-22-9 — an `Array` param moved into a MULTI-FIELD user-enum
/// variant is freed by both the callee's enum drop and the caller.
///
/// B-2026-09-22-6 fixed the ONE-FIELD spelling with a box-only drop twin
/// (`__karac_drop_<E>__boxonly`) and gated it on the variant having exactly
/// one payload field. This lifts that gate to exactly one ARRAY field, which
/// is the question the twin can actually answer.
///
/// THE ROW'S STATED BLOCKER IS REFUTED, and that is the whole of the fix.
/// It reads "`__karac_drop_<E>` releases a variant's whole payload in ONE
/// switch arm", so standing that arm down "would trade the double free for a
/// leak of the sibling `String`". The flag is not consumed at the arm. It is
/// consumed at ONE FIELD's interior walk, inside the per-field loop
/// (`src/codegen/synth_drop.rs`, `inner_drop.filter(|_| !skip_boxed_array_interior)`),
/// and the `free` of the box below it is untouched. So a sibling field keeps
/// its own drop and the array field alone stands down. `m/first` and
/// `m/second` are the cells that prove it: both carry a `String` beside the
/// array, and `m/read` reads that sibling back through `{t}` before the walk,
/// so none of the three can pass while the sibling is being lost.
///
/// EXACTLY ONE array field, not "at least one", because the twin is a
/// per-ENUM function: it stands down every `BoxedArray` interior walk it
/// meets, so a variant holding a caller-retained array BESIDE a callee-owned
/// one would lose the second's elements. That shape still aborts and is
/// deliberately absent — it has its own row. (B-2026-09-22-17 has since
/// widened the gate to "EVERY array field caller-retained and param-rooted";
/// the two-array cells are in the fixture after this one.)
///
/// `m/second` puts the array SECOND and `m/three` puts it in the middle of
/// three fields, because the fix indexes the ctor argument by the array's
/// position rather than assuming it is first.
///
/// `m/noheap` (`Array[N, 2]`, elements owning no heap) and `b/str`
/// (`Array[String, 2]`, a callee-OWNED element type whose ordinary retraction
/// already worked) pin the two directions the relaxed gate must not disturb;
/// both were clean before the fix. `b/single` is B-2026-09-22-6's original
/// one-field cell, kept here as the non-regression on the gate being lifted.
///
/// `b/local` sources its array from a LOCAL rather than a param, so the fix's
/// predicate cannot fire; it prints `r` and NO element bodies, which is an
/// agreed pre-existing gap measured identically on both arms and on every
/// surface including `--interp`. It is pinned as-is rather than corrected,
/// and it is memory-CLEAN — the bodies are skipped, the buffers are not
/// stranded — which is why it can sit in a sanitizer fixture at all.
///
/// EVERY VARIANT NAME IN THIS PROGRAM IS DISTINCT, AND THAT IS LOAD-BEARING
/// RATHER THAN STYLE. The first draft gave every enum the variants `P`/`Q`,
/// which put `W2 { P(Array[S, 2], String) }` beside
/// `W2s { P(String, Array[S, 2]) }` — two enums sharing a variant name and
/// carrying the same heap-bearing types in swapped positions. That is
/// B-2026-09-22-16, a cross-enum field-offset miscompile, and it made this
/// program SIGSEGV before reaching its first line. Renaming the variants
/// apart makes it 6/6 clean and 1 distinct binary over 6 builds. Do not
/// re-collide them.
///
/// `--interp` AGREES WITH ALL THREE COMPILED SURFACES HERE, which is worth
/// stating because the struct sibling B-2026-09-22-7 has no interpreter twin
/// for the opposite reason: there the interpreter runs every caller-retained
/// cell's element bodies twice (B-2026-09-22-8). The enum spelling does not,
/// so this program's expected output is the same on all four.
#[test]
fn e2e_array_param_into_multi_field_enum_variant_stays_with_caller() {
    let Some(out) = run_program(
        r#"struct S { tag: String }
impl Drop for S { fn drop(mut ref self) { println(f"  dS{self.tag}") } }
struct N { id: i64 }
impl Drop for N { fn drop(mut ref self) { println(f"  dN{self.id}") } }
enum W2 { Af(Array[S, 2], String), Aq }
enum W2s { Bs(String, Array[S, 2]), Bq }
enum W3 { Ct(String, Array[S, 2], i64), Cq }
enum Wn { Dn(Array[N, 2], String), Dq }
enum Wv { Ev(Array[String, 2], String), Eq }
enum W1 { Fo(Array[S, 2]), Fq }
fn m_first(a: Array[S, 2]) -> i64 { match W2.Af(a, f"zz") { W2.Af(v, t) => { println("  r"); return 1 }, W2.Aq => { println("  n"); return 0 } } }
fn m_second(a: Array[S, 2]) -> i64 { match W2s.Bs(f"zz", a) { W2s.Bs(t, v) => { println("  r"); return 1 }, W2s.Bq => { println("  n"); return 0 } } }
fn m_read(a: Array[S, 2]) -> i64 { match W2.Af(a, f"kk") { W2.Af(v, t) => { println(f"  r:{t}:{v[0].tag}"); return 1 }, W2.Aq => { println("  n"); return 0 } } }
fn m_three(a: Array[S, 2]) -> i64 { match W3.Ct(f"zz", a, 9) { W3.Ct(t, v, k) => { println("  r"); return 1 }, W3.Cq => { println("  n"); return 0 } } }
fn m_noheap(a: Array[N, 2]) -> i64 { match Wn.Dn(a, f"zz") { Wn.Dn(v, t) => { println("  r"); return 1 }, Wn.Dq => { println("  n"); return 0 } } }
fn b_str(a: Array[String, 2]) -> i64 { match Wv.Ev(a, f"zz") { Wv.Ev(v, t) => { println(f"  r:{v[0]}"); return 1 }, Wv.Eq => { println("  n"); return 0 } } }
fn b_single(a: Array[S, 2]) -> i64 { match W1.Fo(a) { W1.Fo(v) => { println("  r"); return 1 }, W1.Fq => { println("  n"); return 0 } } }
fn b_local() -> i64 { let a: Array[S, 2] = [S { tag: f"llllllll0" }, S { tag: f"llllllll1" }]; match W2.Af(a, f"zz") { W2.Af(v, t) => { println("  r"); return 1 }, W2.Aq => { println("  n"); return 0 } } }
fn main() {
    println("m/first");  { let a: Array[S, 2] = [S { tag: f"aaaaaaaa0" }, S { tag: f"aaaaaaaa1" }]; let z = m_first(a); }
    println("m/second"); { let a: Array[S, 2] = [S { tag: f"bbbbbbbb0" }, S { tag: f"bbbbbbbb1" }]; let z = m_second(a); }
    println("m/read");   { let a: Array[S, 2] = [S { tag: f"cccccccc0" }, S { tag: f"cccccccc1" }]; let z = m_read(a); }
    println("m/three");  { let a: Array[S, 2] = [S { tag: f"dddddddd0" }, S { tag: f"dddddddd1" }]; let z = m_three(a); }
    println("m/noheap"); { let a: Array[N, 2] = [N { id: 2 }, N { id: 3 }]; let z = m_noheap(a); }
    println("b/str");    { let a: Array[String, 2] = [f"gggggggg0", f"gggggggg1"]; let z = b_str(a); }
    println("b/single"); { let a: Array[S, 2] = [S { tag: f"hhhhhhhh0" }, S { tag: f"hhhhhhhh1" }]; let z = b_single(a); }
    println("b/local");  { let z = b_local(); }
    println("end")
}"#,
    ) else {
        return;
    };
    assert_eq!(out, "m/first\n  r\n  dSaaaaaaaa0\n  dSaaaaaaaa1\nm/second\n  r\n  dSbbbbbbbb0\n  dSbbbbbbbb1\nm/read\n  r:kk:cccccccc0\n  dScccccccc0\n  dScccccccc1\nm/three\n  r\n  dSdddddddd0\n  dSdddddddd1\nm/noheap\n  r\n  dN2\n  dN3\nb/str\n  r:gggggggg0\nb/single\n  r\n  dShhhhhhhh0\n  dShhhhhhhh1\nb/local\n  r\nend\n", "got:\n{out}");
}

/// B-2026-09-22-17 — a user-enum variant carrying TWO `Array` params, each
/// caller-retained, was freed by both sides (`free(): double free detected in
/// tcache 2`, exit 134) with B-2026-09-22-9's fix present and without it.
///
/// That fix's gate COUNTED array fields and declined at two. The question it
/// needed was whether EVERY array field is rooted at a by-value param whose
/// element heap the caller keeps; when all are, the box-only twin's answer
/// (stand every `BoxedArray` interior walk down, keep every box `free` and
/// every sibling's drop) is right for all of them. `t/two` is the row's own
/// reproducer, `t/twos` the same element type twice, `t/str` puts a `String`
/// BETWEEN the two arrays and reads it and both arrays back, and `t/let` is
/// the named-`let` spelling, which reaches the gate through `stmts.rs` rather
/// than the match arm. All four aborted 134 with 6 or 12 valgrind errors on
/// the control arm and run clean on the fix.
///
/// `t/callee` is the other side of "every": two `Array[String, 2]` params,
/// whose element heap IS callee-owned, so the gate must decline and the
/// ordinary walk must free them. It was clean before the fix and must stay so.
///
/// DELIBERATELY ABSENT: a caller-retained array BESIDE a callee-owned one or a
/// local. The twin is chosen per ENUM, so it cannot give the two fields two
/// answers; the gate declines, and that shape still aborts 134 identically on
/// both arms. It cannot sit in a clean-run fixture.
///
/// `--interp` is NOT the oracle for `t/let`: the interpreter runs that cell's
/// element bodies TWICE (B-2026-09-22-8). The other four cells agree with it.
/// Variant names are distinct from every other enum here on purpose
/// (B-2026-09-22-16).
#[test]
fn e2e_two_array_params_into_one_enum_variant_stay_with_caller() {
    let Some(out) = run_program(
        r#"struct S { tag: String }
impl Drop for S { fn drop(mut ref self) { println(f"  dS{self.tag}") } }
struct N { id: i64 }
impl Drop for N { fn drop(mut ref self) { println(f"  dN{self.id}") } }
enum X2 { Gp(Array[S, 2], Array[N, 2]), Gq }
enum X2s { Hp(Array[S, 2], Array[S, 2]), Hq }
enum X3 { Ip(Array[S, 2], String, Array[N, 2]), Iq }
enum X4 { Jp(Array[String, 2], Array[String, 2]), Jq }
fn t_two(a: Array[S, 2], b: Array[N, 2]) -> i64 { match X2.Gp(a, b) { X2.Gp(v, w) => { println("  r"); return 1 }, X2.Gq => { println("  n"); return 0 } } }
fn t_twos(a: Array[S, 2], b: Array[S, 2]) -> i64 { match X2s.Hp(a, b) { X2s.Hp(v, w) => { println("  r"); return 1 }, X2s.Hq => { println("  n"); return 0 } } }
fn t_str(a: Array[S, 2], b: Array[N, 2]) -> i64 { match X3.Ip(a, f"kk", b) { X3.Ip(v, t, w) => { println(f"  r:{t}:{v[1].tag}:{w[0].id}"); return 1 }, X3.Iq => { println("  n"); return 0 } } }
fn t_let(a: Array[S, 2], b: Array[N, 2]) -> i64 { let o = X2.Gp(a, b); match o { X2.Gp(v, w) => { println("  r"); return 1 }, X2.Gq => { println("  n"); return 0 } } }
fn t_callee(a: Array[String, 2], b: Array[String, 2]) -> i64 { match X4.Jp(a, b) { X4.Jp(v, w) => { println(f"  r:{v[0]}:{w[1]}"); return 1 }, X4.Jq => { println("  n"); return 0 } } }
fn main() {
    println("t/two");    { let a: Array[S, 2] = [S { tag: f"aaaaaaaa0" }, S { tag: f"aaaaaaaa1" }]; let b: Array[N, 2] = [N { id: 1 }, N { id: 2 }]; let z = t_two(a, b); }
    println("t/twos");   { let a: Array[S, 2] = [S { tag: f"bbbbbbbb0" }, S { tag: f"bbbbbbbb1" }]; let b: Array[S, 2] = [S { tag: f"cccccccc0" }, S { tag: f"cccccccc1" }]; let z = t_twos(a, b); }
    println("t/str");    { let a: Array[S, 2] = [S { tag: f"dddddddd0" }, S { tag: f"dddddddd1" }]; let b: Array[N, 2] = [N { id: 3 }, N { id: 4 }]; let z = t_str(a, b); }
    println("t/let");    { let a: Array[S, 2] = [S { tag: f"eeeeeeee0" }, S { tag: f"eeeeeeee1" }]; let b: Array[N, 2] = [N { id: 5 }, N { id: 6 }]; let z = t_let(a, b); }
    println("t/callee"); { let a: Array[String, 2] = [f"ffffffff0", f"ffffffff1"]; let b: Array[String, 2] = [f"gggggggg0", f"gggggggg1"]; let z = t_callee(a, b); }
    println("end")
}"#,
    ) else {
        return;
    };
    assert_eq!(out, "t/two\n  r\n  dN1\n  dN2\n  dSaaaaaaaa0\n  dSaaaaaaaa1\nt/twos\n  r\n  dScccccccc0\n  dScccccccc1\n  dSbbbbbbbb0\n  dSbbbbbbbb1\nt/str\n  r:kk:dddddddd1:3\n  dN3\n  dN4\n  dSdddddddd0\n  dSdddddddd1\nt/let\n  r\n  dN5\n  dN6\n  dSeeeeeeee0\n  dSeeeeeeee1\nt/callee\n  r:ffffffff0:gggggggg1\nend\n", "got:\n{out}");
}

/// B-2026-08-31-39 — DESTRUCTURING a generic enum's bare-`T` payload
/// renders it at the INSTANTIATION, for every nameless aggregate shape.
///
/// `def3015` closed the whole-`Option` half (`println(f"{x}")`); this is
/// the half it left open, and the three surfaces disagreed with the
/// interpreter in three different ways:
///
/// Measured on a pristine build of the parent commit — EVERY aggregate
/// shape without a name-keyed registration was a SILENT miscompile, and
/// the whole program segfaulted:
///
///     T                interp              build (pre-fix)
///     Array[i64, 2]    [1, 2]              1                 first element
///     Slice[i64]       [1, 2]              140736451076536   the data POINTER
///     (i64, i64)       (5, 6)              5                 first element
///     Vector[i64, 4]   Vector(1, 2, 3, 4)  94380923771616    a raw word
///     Slice[String]    [ab, cd]            93933049563920    the data POINTER
///     Vec[i64]         [1]                 [1]               control — correct
///     Vec[String]      [ab, cd]            [ab, cd]          control — correct
///
/// The two `Vec` controls were already right because a `Vec` has a
/// NAME-keyed registration (`register_var_from_type_expr`) the renderer
/// consults ahead of the span-keyed tables; the five broken shapes have no
/// such twin, which is exactly why they were the ones that broke.
///
/// EVERY LINE IS A DISTINCT CHANNEL, not decoration:
///   * `Array[i64, 2]` then `Array[i64, 3]` — two nameless instantiations
///     of ONE body, whose spans COINCIDE. The span-keyed display tables
///     outlive a function compile, so a seed that neither overwrote nor
///     retracted rendered the second at the first's extent (`[7, 8]`).
///   * `Slice[i64]` — the third failure mode, which this row's opening
///     table never recorded.
///   * `Slice[String]` and `Vec[String]` — heap elements through the same
///     path, where the reconstruction has to reach past word 0.
///   * the tuple and the `Vector` — the two shapes this row never named,
///     which misprinted a first element and a raw word respectively.
///   * `i64` and `String` AFTER the aggregates — the STALE-SEED control. A
///     scalar instantiation shares the same body spans and must not
///     inherit the array entry; without the retraction it printed
///     `[7, <garbage>]`.
///   * a generic METHOD and `Result[T, E]` — the two shapes the row listed
///     as NOT MEASURED, in the destructuring form.
///
/// Twin of `tests/interpreter.rs`'s
/// `test_generic_destructured_payload_renders_at_its_instantiation`,
/// pinned to the same string.
/// B-2026-09-02-37 — an enum payload whose ARRAY elements are themselves
/// multi-word packs at its true width.
///
/// `coerce_to_payload_words`' `ArrayValue` arm pushed exactly ONE word per
/// element, which is right only for a scalar element. An
/// `Array[String, N]`'s elements are `{ptr, len, cap}` triples, so
/// `coerce_to_i64` collapsed each to a single word and the rest of every
/// string was ERASED.
///
/// The truncation is not the damage. `out.len()` then DISAGREED with
/// `llvm_type_word_count`, which every unpack and drop site recomputes, and
/// the boxing decision is `out.len() > num_words` — so the two ends of the
/// same payload made OPPOSITE decisions about whether a box exists. That
/// split the failure in two, measured on a pristine parent build:
///
///     Array[String, 1]   3 words vs an area of 3 — packed 1, no boxing,
///                        zero-padded: `Some([])` for `Some([one])`
///     Array[String, 2]   6 words vs 3 — packed 2, still no boxing, but the
///                        unpack's `want (6) > field_words (3)` DID fire, so
///                        it `inttoptr`'d a word that is not a box pointer:
///                        `[, ]` across a call, SEGFAULT in `main`
///
/// EVERY LINE IS A DISTINCT CHANNEL. Pristine results in parentheses:
///   * a directly-printed `Array[String, 2]` (correct) and an
///     `Option[Array[i64, 2]]` (correct) — the two CONTROLS, so a
///     regression that breaks arrays generally fails here first.
///   * `Array[String, 1]` (`Some([])`) — the under-count leg, which fits
///     the area and therefore never reaches the box at all.
///   * `Array[String, 2]` in `main` (SEGFAULT) and through a CALL (`[, ]`)
///     — the same payload, two positions, two symptoms.
///   * indexing the bound payload, `t[0]` / `t[1]` (two empty strings) —
///     proof the binding really holds erased values, not just a bad render.
///   * `Result[…]` (SEGFAULT) and a user enum (`[, ]`) — the area is not
///     `Option`-specific.
///   * `Array[Vec[i64], 2]` (`[]` `[]`) — a multi-word element that is not
///     a String.
///   * `Array[Array[i64, 2], 2]` (`[0, 0] [0, 0]`) — a nested ARRAY
///     element, where the recursion has to fire twice.
///   * `Array[P, 2]` with a `String` field (` 0` / ` 0`) — eight words, and
///     the only leg whose ints were lost too.
///   * the generic `show[T]` (`[, ]`) — the one leg B-2026-08-31-39's fix
///     left wrong, and the reason this row was filed from it.
///
/// Twin of `tests/interpreter.rs`'s
/// `test_array_of_heap_elements_as_an_enum_payload`, pinned to the same
/// string.
#[test]
fn codegen_array_of_heap_elements_as_an_enum_payload() {
    let Some(out) = run_program(
        r#"struct P { s: String, n: i64 }
enum W { A(Array[String, 2]), B }
fn takeP(p: ref P) -> i64 { println(f"{p.s} {p.n}"); return p.n }
fn viaCall(x: Option[Array[String, 2]]) { match x { Some(t) => { println(f"{t}") } None => { println("n") } } }
fn generic[T: Display](x: Option[T]) { match x { Some(t) => { println(f"g:{t}") } None => { println("n") } } }
fn main() {
    let ctl: Array[String, 2] = ["ab", "cd"];
    println(f"{ctl}");

    let ai: Array[i64, 2] = [1, 2];
    let oi: Option[Array[i64, 2]] = Some(ai);
    match oi { Some(t) => { println(f"{t}") } None => { println("n") } }

    let a1: Array[String, 1] = ["one"];
    let o1: Option[Array[String, 1]] = Some(a1);
    println(f"{o1}");
    match o1 { Some(t) => { println(f"{t}") } None => { println("n") } }

    let a2: Array[String, 2] = ["ab", "cd"];
    let o2: Option[Array[String, 2]] = Some(a2);
    println(f"{o2}");
    match o2 { Some(t) => { println(f"{t}"); println(t[0]); println(t[1]) } None => { println("n") } }

    let a3: Array[String, 2] = ["ef", "gh"];
    viaCall(Some(a3));

    let a4: Array[String, 2] = ["ij", "kl"];
    let r: Result[Array[String, 2], String] = Ok(a4);
    match r { Ok(t) => { println(f"{t}") } Err(e) => { println(e) } }

    let a5: Array[String, 2] = ["mn", "op"];
    let w = W.A(a5);
    match w { W.A(t) => { println(f"{t}") } W.B => { println("b") } }

    let v1: Vec[i64] = [1, 2];
    let v2: Vec[i64] = [3];
    let av: Array[Vec[i64], 2] = [v1, v2];
    let ov: Option[Array[Vec[i64], 2]] = Some(av);
    match ov { Some(t) => { println(f"{t[0]}"); println(f"{t[1]}") } None => { println("n") } }

    let n1: Array[i64, 2] = [5, 6];
    let n2: Array[i64, 2] = [7, 8];
    let m: Array[Array[i64, 2], 2] = [n1, n2];
    let om: Option[Array[Array[i64, 2], 2]] = Some(m);
    match om { Some(t) => { let r0: Array[i64, 2] = t[0]; let r1: Array[i64, 2] = t[1]; println(f"{r0} {r1}") } None => { println("n") } }

    let ap: Array[P, 2] = [P { s: "qr", n: 1 }, P { s: "st", n: 2 }];
    let op: Option[Array[P, 2]] = Some(ap);
    match op { Some(t) => { let x = takeP(t[0]); let y = takeP(t[1]); println(f"{x}{y}") } None => { println("n") } }

    let a6: Array[String, 2] = ["uv", "wx"];
    generic(Some(a6));
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "[ab, cd]\n[1, 2]\nSome([one])\n[one]\nSome([ab, cd])\n[ab, cd]\nab\ncd\n[ef, gh]\n[ij, kl]\n[mn, op]\n[1, 2]\n[3]\n[5, 6] [7, 8]\nqr 1\nst 2\n12\ng:[uv, wx]\n");
}

/// B-2026-08-28-57 — a fixed `Array[T, N]`'s elements run their user
/// `Drop` bodies, at the binding's live-range end, on every backend.
///
/// The row reported `Array[E, 2]` of an own-`Drop` enum as interp
/// `dE dE dR3 mid end` vs compiled `mid end dE dE`: right count, wrong
/// PLACE. Reproducing it turned up something larger. The compiled `dE dE`
/// was coming from the MEMORY channel, which reached the body only because
/// `emit_drop_fn_for_type_expr` handed it the user-drop wrapper — the hole
/// B-2026-08-28-58 closed. With that shut, the compiled side ran NO array
/// element bodies at all, and measuring the surface showed it never had for
/// a STRUCT element either (the wrapper was already excluded for structs by
/// B-2026-07-30-11), which the row did not mention. So `struct-elems` and
/// `heap-struct-elems` are rows here, not just the enum the row names:
/// `Array` simply had no bodies walker, and the enum was the one element
/// type whose body leaked through the memory channel by accident.
///
/// `moved-on-*` is the second half. A bare rebind `let b = a;` carries no
/// annotation, so the destination could not resolve its element type and
/// registered nothing while the move disarmed the source — bodies in the
/// interpreter, nowhere else. Type-agnostic (it failed identically for a
/// struct array) and fixed by the source-record fallback, which is why the
/// annotated spelling is pinned beside it: the two must not diverge.
#[test]
fn e2e_fixed_array_elements_run_their_user_drop_bodies() {
    const H: &str = "enum E { A(R), B }\n\
             impl Drop for E { fn drop(mut ref self) { println(\"drop E\") } }\n\
             enum H { A(R), B }\n\
             enum J { A(i64), B }\n\
             struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop R{self.id}\") } }\n\
             struct S { name: String }\n\
             impl Drop for S { fn drop(mut ref self) { println(f\"drop S{self.name}\") } }\n";
    for (label, body, want) in [
        // The row's shape. Placement is the assertion: the bodies precede
        // `mid`, they do not trail `end`.
        (
            "enum-elems-mixed",
            "fn main() { let a: Array[E, 2] = [E.B, E.A(R { id: 3 })];\n\
                 \x20            println(\"mid\"); println(\"end\"); }\n",
            "drop E\ndrop E\ndrop R3\nmid\nend\n",
        ),
        (
            "enum-elems-unit",
            "fn main() { let a: Array[E, 1] = [E.B]; println(\"mid\"); }\n",
            "drop E\nmid\n",
        ),
        // -54's predicate at this position: no own `Drop`, Drop-bearing
        // payload.
        (
            "payload-only-enum-elem",
            "fn main() { let a: Array[H, 1] = [H.A(R { id: 4 })];\n\
                 \x20            println(\"mid\"); }\n",
            "drop R4\nmid\n",
        ),
        // The gap the row did not report: a STRUCT element never ran its
        // body on the compiled backends either.
        (
            "struct-elems",
            "fn main() { let a: Array[R, 2] = [R { id: 1 }, R { id: 2 }];\n\
                 \x20            println(\"mid\"); }\n",
            "drop R1\ndrop R2\nmid\n",
        ),
        (
            "heap-struct-elems",
            "fn main() { let a: Array[S, 2] = [S { name: f\"x{1}\" }, S { name: f\"y{2}\" }];\n\
                 \x20            println(\"mid\"); }\n",
            "drop Sx1\ndrop Sy2\nmid\n",
        ),
        // MOVED ON, un-annotated destination — the source-record fallback.
        (
            "moved-on-enum",
            "fn main() { let a: Array[E, 2] = [E.B, E.B];\n\
                 \x20            let b = a; println(\"moved\"); }\n",
            "drop E\ndrop E\nmoved\n",
        ),
        (
            "moved-on-struct",
            "fn main() { let a: Array[R, 2] = [R { id: 1 }, R { id: 2 }];\n\
                 \x20            let b = a; println(\"moved\"); }\n",
            "drop R1\ndrop R2\nmoved\n",
        ),
        // The ANNOTATED destination, which already worked, pinned so the
        // two spellings cannot drift apart again.
        (
            "moved-on-enum-annotated",
            "fn main() { let a: Array[E, 2] = [E.B, E.B];\n\
                 \x20            let b: Array[E, 2] = a; println(\"moved\"); }\n",
            "drop E\ndrop E\nmoved\n",
        ),
        // NESTED SCOPE — the bodies belong to the inner frame.
        (
            "nested-scope",
            "fn main() { { let a: Array[E, 1] = [E.B]; println(\"inner\") }\n\
                 \x20            println(\"outer\"); }\n",
            "drop E\ninner\nouter\n",
        ),
        // BOUNDARY — an element type with no `Drop` anywhere runs nothing,
        // so this is not "every array element fires".
        (
            "no-drop-elems",
            "fn main() { let a: Array[J, 2] = [J.A(1), J.B]; println(\"mid\"); }\n",
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

/// B-2026-08-31-29 — `let g = ref arr[i]` over an `Array[T, N]` base reads
/// the ELEMENT, not whatever the array's first two words happen to spell.
///
/// `try_compile_ref_binding`'s named-local arm called
/// `lower_indexed_elem_ptr_vec` unconditionally, so an `Array` — whose slot
/// IS the storage, not a `{ptr, i64, i64}` header — was punned as a Vec:
/// element 0 became the data pointer and element 1 the length. The bounds
/// check was not merely useless, it was DEFEATED, comparing the index
/// against a payload word:
///
///     %arr = alloca [3 x i64]                ; [10, 20, 30]
///     %len  = gep {ptr,i64,i64}, %arr, 0, 1   ; 20
///     %data = gep {ptr,i64,i64}, %arr, 0, 0   ; 10
///     %bounds = icmp uge i64 2, %len          ; 2 < 20, PASSES
///     %e = gep i64, %data, i64 2              ; address 10 + 16
///
/// The `scalar` line is therefore the load-bearing one: `Array[i64, 3]`
/// built CLEANLY and segfaulted. The row was filed against the
/// `Array[Vector[T, N], M]` spelling, which is the LUCKY case — there the
/// wrong element type (the vector's LANE) reaches `compile_vector_method`
/// and panics the compiler before the bad load is ever emitted, so it fails
/// loudly at build time instead of silently at runtime.
///
/// `struct` covers the second half of the fix: an Array's element
/// `TypeExpr` lives in `array_elem_type_exprs`, not the `var_elem_type_exprs`
/// the Vec/Slice arms read, and without it a field read through the borrow
/// failed with "cannot resolve field 'a' on this receiver". `alias` pins
/// the borrow's actual contract — a write through the container is visible
/// through the live borrow — and `vec` is the control: the one class that
/// always worked, which must keep working.
///
/// Twin of `tests/interpreter.rs`'s `test_ref_binding_over_an_array_base`,
/// pinned to the same string. The interpreter was correct throughout, which
/// is what made this a run-vs-build divergence rather than a design gap.
///
/// B-2026-08-31-36 added the four `slice*` lines, and there the backends
/// were the other way round: codegen read the element correctly while the
/// INTERPRETER bound the whole container, so `ref s[2]` printed
/// `[10, 20, 30]` under `--interp` against `30` everywhere else. `slicearith`
/// is the line that makes the binding's TYPE observable rather than just its
/// rendering — it failed with "operator 'Add' is not defined for operands of
/// type 'Slice' and 'Int'" while the compiled backends returned 35.
/// `slicealias` pins the same live-borrow contract the `alias` line pins for
/// an Array: a write through the parent `Vec` is visible through the borrow,
/// which is what a slice element being an element OF THAT VEC means.
#[test]
fn e2e_ref_binding_over_an_array_base_reads_the_element() {
    let Some(out) = run_program(ARRAY_REF_BINDING_SRC) else {
        return;
    };
    assert_eq!(
        out,
        "scalar 30\nvector 10\nstruct 3 4\nalias 99\nvec 8\n\
             slice 30\nslicearith 35\nslicestruct 3 4\nslicealias 77\n",
        "a `ref` binding over an Array base must read the element; got: {out:?}"
    );
}

/// B-2026-09-14-15 — a NESTED fixed `Array` runs its innermost elements'
/// `Drop` bodies on every surface: `Array[Array[D, N], M]` bound, moved,
/// discarded, inside an envelope or an arm, and the mirror shape
/// `Vec[Array[D, N]]`.
///
/// The defect was a ONE-LEVEL HORIZON in two walkers and, on the
/// interpreter, in one value-level gate — three sites, one shape:
///
///   * `elem_te_runs_user_drop` admitted an element by its HEAD NAME, and
///     the head of `Array[D, 1]` is `Array`, no declared struct or enum. So
///     `emit_array_elem_user_drop_bodies_fn` emitted NO walker for a nested
///     array at any depth, and `emit_slot_drop_bodies_at` had no array arm
///     to run once one existed. `--interp` recurses structurally on
///     `Value::Array` and had been printing these bodies all along, so the
///     gap was a run-vs-build divergence rather than an agreed silence.
///   * `emit_nested_vec_elem_bodies_fn` — the `Vec` side of the same
///     nesting — had arms for a tuple, a struct, an `Option`/`Result` and a
///     `Vec` element, and none for an `Array` one, so `Vec[Array[D, 1]]`
///     diverged the same way.
///   * `pattern_binding_owes_drop_body` classified an arm-bound container's
///     elements with `value_runs_user_drop`, which answers only for a
///     `Value::Struct`. A nested element is another `Value::Array`, so no
///     Drop slot was registered for the arm binding and the arm ran nothing
///     — which had already made `match o { Some(t) => .. }` over
///     `Option[Vec[Vec[D]]]` and `Option[Array[Vec[D], 2]]` divergent
///     before this row; both are cells here.
///
/// The two halves move in ONE commit, as every row in this family does. The
/// codegen half alone flipped the envelope cells the other way (compiled
/// printing, `--interp` silent), because B-2026-09-14-2 had deliberately
/// stopped its payload walk and that walk's arming gate short of a fixed
/// array so as not to open a third divergence; lifting that stop is the
/// interpreter half and is only correct once the walker exists.
///
/// NOT moved, and pinned below: an `Array[D, N]` in a STRUCT FIELD is an
/// agreed silence on all four surfaces (B-2026-09-12-21's shape), and a
/// whole-container REASSIGNMENT loses the displaced container's element
/// bodies on every compiled backend — flat `Vec[D]` and `Array[D, N]`
/// alike, so neither is this nesting's business.
#[test]
fn e2e_nested_fixed_array_runs_its_element_drop_bodies() {
    const HDR: &str = "struct D { a: String, b: i64 }\n\
                           impl Drop for D { fn drop(mut ref self) { println(f\"dD{self.b}\") } }\n\
                           fn pay() -> String { return \"heap\"; }\n\
                           fn mkd(n: i64) -> D { return D { a: pay(), b: n }; }\n\
                           fn mka(n: i64) -> Array[D, 1] { return [mkd(n)]; }\n\
                           struct W { z: Array[D, 2] }\n\
                           fn eat(a: Array[Array[D, 1], 2]) { println(\"in-eat\"); }\n";
    for (label, body, want) in [
            (
                "a nested fixed Array local",
                "let v: Array[Array[D, 1], 2] = [[mkd(1)], [mkd(2)]];",
                "dD1\ndD2\nmid\n",
            ),
            (
                "two elements in the inner array",
                "let v: Array[Array[D, 2], 1] = [[mkd(1), mkd(2)]];",
                "dD1\ndD2\nmid\n",
            ),
            (
                "three levels deep",
                "let v: Array[Array[Array[D, 1], 1], 2] = [[[mkd(1)]], [[mkd(2)]]];",
                "dD1\ndD2\nmid\n",
            ),
            (
                "an Array element of a TUPLE, which shares the widened predicate",
                "let t: (Array[D, 2], i64) = ([mkd(1), mkd(2)], 7);",
                "dD1\ndD2\nmid\n",
            ),
            (
                "a nested Array element of a tuple",
                "let t: (Array[Array[D, 1], 2], i64) = ([[mkd(1)], [mkd(2)]], 7);",
                "dD1\ndD2\nmid\n",
            ),
            (
                "bound inside an Option envelope",
                "let o: Option[Array[Array[D, 1], 2]] = Option.Some([[mkd(1)], [mkd(2)]]);",
                "dD1\ndD2\nmid\n",
            ),
            (
                "discarded inside an Option envelope",
                "let o: Option[Array[Array[D, 1], 2]] = Option.Some([[mkd(1)], [mkd(2)]]);\nlet _ = o;",
                "dD1\ndD2\nmid\n",
            ),
            (
                "Result's Ok arm",
                "let r: Result[Array[Array[D, 1], 2], i64] = Result.Ok([mka(1), mka(2)]);",
                "dD1\ndD2\nmid\n",
            ),
            (
                "bound out by a consuming match arm",
                "let o: Option[Array[Array[D, 1], 2]] = Option.Some([[mkd(1)], [mkd(2)]]);\n\
                 match o { Option.Some(t) => { println(\"arm\"); } Option.None => { println(\"none\"); } }",
                "arm\ndD1\ndD2\nmid\n",
            ),
            (
                "the same through if let",
                "let o: Option[Array[Array[D, 1], 2]] = Option.Some([[mkd(1)], [mkd(2)]]);\n\
                 if let Option.Some(t) = o { println(\"arm\"); }",
                "arm\ndD1\ndD2\nmid\n",
            ),
            (
                "moved into a call",
                "let v: Array[Array[D, 1], 2] = [[mkd(1)], [mkd(2)]];\neat(v);",
                "in-eat\ndD1\ndD2\nmid\n",
            ),
            // The Vec side of the same nesting.
            (
                "a Vec of fixed Arrays",
                "let v: Vec[Array[D, 1]] = [mka(1), mka(2)];",
                "dD1\ndD2\nmid\n",
            ),
            (
                "a Vec of fixed Arrays inside an envelope",
                "let o: Option[Vec[Array[D, 1]]] = Option.Some([mka(1), mka(2)]);",
                "dD1\ndD2\nmid\n",
            ),
            (
                "a Vec of fixed Arrays bound out by an arm",
                "let o: Option[Vec[Array[D, 1]]] = Option.Some([mka(1), mka(2)]);\n\
                 match o { Option.Some(t) => { println(\"arm\"); } Option.None => { println(\"none\"); } }",
                "arm\ndD1\ndD2\nmid\n",
            ),
            (
                "Vec of Vec of fixed Array",
                "let v: Vec[Vec[Array[D, 1]]] = [[mka(1)], [mka(2)]];",
                "dD1\ndD2\nmid\n",
            ),
            // The two arm cells that were already divergent before this row and
            // that the widened arm gate closes with it.
            (
                "an arm-bound Vec[Vec[D]] payload",
                "let o: Option[Vec[Vec[D]]] = Option.Some([[mkd(1)], [mkd(2)]]);\n\
                 match o { Option.Some(t) => { println(\"arm\"); } Option.None => { println(\"none\"); } }",
                "arm\ndD1\ndD2\nmid\n",
            ),
            (
                "an arm-bound Array[Vec[D], 2] payload",
                "let o: Option[Array[Vec[D], 2]] = Option.Some([[mkd(1)], [mkd(2)]]);\n\
                 match o { Option.Some(t) => { println(\"arm\"); } Option.None => { println(\"none\"); } }",
                "arm\ndD1\ndD2\nmid\n",
            ),
            // Controls: shapes that were already correct and must not move.
            (
                "control: the flat Array local is unchanged",
                "let v: Array[D, 2] = [mkd(1), mkd(2)];",
                "dD1\ndD2\nmid\n",
            ),
            (
                "control: Array[Vec[D], N] was already correct",
                "let v: Array[Vec[D], 2] = [[mkd(1)], [mkd(2)]];",
                "dD1\ndD2\nmid\n",
            ),
            (
                "control: Vec[Vec[D]] was already correct",
                "let v: Vec[Vec[D]] = [[mkd(1)], [mkd(2)]];",
                "dD1\ndD2\nmid\n",
            ),
            (
                "control: a nested array of non-Drop elements runs nothing",
                "let v: Array[Array[i64, 1], 2] = [[1], [2]];",
                "mid\n",
            ),
            (
                // B-2026-09-12-21's shape. This cell pinned the AGREED SILENCE
                // that B-2026-09-14-15 declined to trade for a divergence, and
                // B-2026-09-15-26 has since closed it in the other direction —
                // both gates learned the array FIELD together, so the cell is
                // now an agreed FIRING and this row's reasoning is intact: it
                // never became a divergence.
                "an Array in a STRUCT FIELD fires on both backends (B-2026-09-15-26)",
                "let w = W { z: [mkd(1), mkd(2)] };",
                "dD1\ndD2\nmid\n",
            ),
        ] {
            let src = format!("{HDR}fn main() {{\n{body}\nprintln(\"mid\");\n}}\n");
            let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(&src);
            assert!(
                interp_errs.is_empty(),
                "[{label}] interp errored: {interp_errs:?}"
            );
            assert_eq!(interp_out.join(""), want, "[{label}] interpreter");
            if let Some(aot) = run_program(&src) {
                assert_eq!(aot, want, "[{label}] AOT");
            }
        }
}

/// B-2026-09-13-23 — the BODIES half of an `Array[T, N]` held in a tuple:
/// each element's user `Drop` runs exactly once, on both backends, in
/// every position the tuple can occupy.
///
/// The row is a LEAK row and measured its own bodies channel as already
/// correct — `(Array[D, 2], i64)` printed `dD1 dD2` while leaking 20 B —
/// so this fixture is not asserting a behaviour change. It is the guard
/// that the MEMORY fix did not buy itself a duplicated or lost body:
/// bodies and memory are separate channels (B-2026-08-28-57), the fix
/// touches four ownership sites including a param entry copy, and a
/// doubled body is invisible to the alloc/free counts the ASAN sibling
/// (`asan_array_inside_a_tuple_frees_its_element_buffers`) reads.
///
/// The `let t2 = t;` cell is the one that matters: a whole-tuple move is
/// where a copy-vs-transfer mistake shows up as two bodies rather than
/// one, and it is also the cell the memory fix was blocked on.
#[test]
fn e2e_array_in_a_tuple_runs_each_element_drop_body_once() {
    const HDR: &str = "struct D { id: i64, s: String }\n\
                           impl Drop for D { fn drop(mut ref self) { println(f\"dD{self.id}\") } }\n\
                           fn mkd(n: i64) -> D { return D { id: n, s: f\"ss{n}\" }; }\n";
    for (label, decls, body, want) in [
            (
                "a plain annotated let",
                "",
                "let t: (Array[D, 2], i64) = ([mkd(1), mkd(2)], 7);\nprintln(f\"n:{t.1}\");",
                "n:7\ndD1\ndD2\nend\n",
            ),
            (
                "a whole-tuple MOVE — the cell the memory fix was blocked on",
                "",
                "let t: (Array[D, 2], i64) = ([mkd(1), mkd(2)], 7);\nlet t2 = t;\nprintln(f\"n:{t2.1}\");",
                "n:7\ndD1\ndD2\nend\n",
            ),
            (
                // PINNED AT AN AGREED SILENCE, measured on all four surfaces
                // and confirmed pre-existing on the fix commit's PARENT tree
                // (parent checkout + marker guard, not a stash — see the
                // destructure note below for why that distinction matters).
                // A tuple held in a STRUCT FIELD runs no element body here,
                // while the identical tuple as a LOCAL (the first two cells)
                // runs both — the nested-container-in-a-struct-field bodies
                // gap, B-2026-09-15-23's family one level further in, and NOT
                // something this row's memory fix changes: the same shape's
                // MEMORY is clean and asserted by
                // `b23-tuple-array-struct-field-move` in the ASAN sibling.
                //
                // Kept as a cell rather than dropped, because it is the
                // position where a future bodies fix has to show up, and
                // because it records that memory and bodies really did come
                // apart here (B-2026-08-28-57).
                "pinned: a struct field holding the tuple runs no body on any backend",
                "struct W { t: (Array[D, 2], i64) }\n",
                "let w: W = W { t: ([mkd(1), mkd(2)], 7) };\nlet w2 = w;\nprintln(f\"n:{w2.t.1}\");",
                "n:7\nend\n",
            ),
            (
                "a by-value param returned — the fourth ownership site",
                "fn thru(p: (Array[D, 2], i64)) -> (Array[D, 2], i64) { return p; }\n",
                "let t: (Array[D, 2], i64) = ([mkd(1), mkd(2)], 7);\nlet u = thru(t);\nprintln(f\"n:{u.1}\");",
                "n:7\ndD1\ndD2\nend\n",
            ),
            (
                "control: a scalar array element runs nothing",
                "",
                "let t: (Array[i64, 2], i64) = ([3, 4], 7);\nlet t2 = t;\nprintln(f\"a0:{t2.0[0]}\");",
                "a0:3\nend\n",
            ),
        ] {
            let src = format!("{HDR}{decls}fn main() {{\n{body}\nprintln(\"end\");\n}}\n");
            let (interp_out, interp_errs, _, _) = karac::run_program_full(&src);
            assert!(
                interp_errs.is_empty(),
                "[{label}] interp errored: {interp_errs:?}"
            );
            assert_eq!(interp_out.join(""), want, "[{label}] interpreter");
            if let Some(aot) = run_program(&src) {
                assert_eq!(aot, want, "[{label}] AOT");
            }
        }
    // A DESTRUCTURE is a RUN-VS-BUILD DIVERGENCE and is pinned as one,
    // with a separate expectation per backend, because a single `want`
    // would force a choice between two behaviours that really do differ.
    //
    // PRE-EXISTING, not this row's doing. Measured on the fix commit's
    // PARENT tree (`git checkout <fix>~1 -- src/`, with a marker count
    // printed as the guard): interp `dD1 dD2 j:7 end`, both compiled
    // backends `j:7 end` — identical to the post-fix reading. Its MEMORY
    // is clean before and after (`b23-tuple-array-return-destructure`
    // asserts that), so this is purely the bodies channel and is filed
    // separately.
    //
    // The first attempt at this check used `git stash push src/` and was
    // WORTHLESS: the fix was already committed, so the stash took nothing
    // and both "before" and "after" measured the same fixed tree. That is
    // CLAUDE.md's documented trap, and the marker guard is what catches it.
    //
    // The interpreter also fires the bodies EARLY, at the destructure
    // rather than at scope end, which is the same shape the array-field
    // move-out shows; that ordering is part of what the new row records.
    {
        let src = format!(
            "{HDR}fn main() {{\n\
                 let t: (Array[D, 2], i64) = ([mkd(1), mkd(2)], 7);\n\
                 let (a, j) = t;\n\
                 println(f\"j:{{j}}\");\n\
                 println(\"end\");\n\
                 }}\n"
        );
        let (interp_out, interp_errs, _, _) = karac::run_program_full(&src);
        assert!(interp_errs.is_empty(), "destructure: {interp_errs:?}");
        assert_eq!(
            interp_out.join(""),
            "dD1\ndD2\nj:7\nend\n",
            "destructure, interpreter (bodies fire, and early)"
        );
        if let Some(aot) = run_program(&src) {
            assert_eq!(
                aot, "j:7\nend\n",
                "destructure, AOT (no bodies — the divergence)"
            );
        }
    }
}

/// B-2026-09-15-26 / B-2026-09-15-27 — an `Array[T, N]`-typed STRUCT
/// FIELD runs its elements' `Drop` bodies, and a `Vec[Array[T, N]]` field
/// frees their heap.
///
/// Both backends were silent on the bodies, so this was an AGREED GAP
/// rather than a divergence, and the two halves move in one commit for
/// that reason — repairing codegen alone would have manufactured the
/// divergence B-2026-09-10-17 declined to create when it fixed the
/// envelope half of this same family.
///
/// THREE sites, one shape. Every one of them is keyed on a HEAD NAME, and
/// the head of `Array[D, 2]` is `Array` — not a declared struct, not a
/// container in any of their tables:
///
///   * codegen's RELEVANCE gate `type_runs_user_drop` has a
///     one-container-level leg per container (`Vec`, `Map`/`Set`, tuple,
///     `Option`/`Result` payload, envelope chain, generic struct
///     instantiation) and had none for a fixed array, so `H { f: Array[D,
///     2] }` classified drop-free and NO bodies action was registered for
///     `h` at all. Widening the field SELECTOR alone (which this row also
///     did, for the same reason) changes nothing while the gate ahead of
///     it answers false — measured.
///   * the interpreter twin `field_te_runs_user_drop` had the identical
///     hole, which is what made the silence agreed.
///   * the interpreter's field WALK then had to learn the position too:
///     `Vec` and `Array` are one runtime value (`Value::Array`), so an
///     array field reached the `Vec` arm, was turned away by its
///     declared-head gate, and fell through that arm's unconditional
///     `continue`.
///
/// The MEMORY half (-15-27) is one further site and the opposite polarity:
/// `vec_element_drain_fn`, the shared struct-field / enum-payload element
/// policy, had no array case, so a `Vec[Array[D, 1]]` FIELD freed its
/// buffer and leaked every element's interior. B-2026-09-10-8/-26 saw that
/// position and deliberately put its recursion in `emit_drop_fn_for_array`
/// instead, on the ground that a `Vec[Array[String, 2]]` built as `let e =
/// [..]; v.push(e)` is already owned by the source local. That holds for a
/// `Vec` LOCAL and not for a FIELD: the push-built spelling leaks as a
/// field exactly as the literal one does, because the push disarms `e` and
/// moving the `Vec` into the field leaves no other owner. Both spellings
/// are cells below.
///
/// The `Vec[Vec[D]]` and `Vec[Array[D, 1]]` BODIES remain an agreed
/// silence — a nested container in a `Vec` field, which is
/// B-2026-09-15-23's subject and not this row's. Pinned as such below so a
/// later change to that gate is visible here.
#[test]
fn e2e_array_typed_struct_field_runs_its_element_drop_bodies() {
    const HDR: &str = "struct D { id: i64, s: String }\n\
                           impl Drop for D { fn drop(mut ref self) { println(f\"dD{self.id}\") } }\n\
                           fn mkd(n: i64) -> D { return D { id: n, s: f\"ss{n}\" }; }\n\
                           struct N { v: i64 }\n";
    for (label, decls, body, want) in [
            (
                "the flat Array field — the row's own cell",
                "struct H { f: Array[D, 2] }\n",
                "let h: H = H { f: [mkd(1), mkd(2)] };",
                "dD1\ndD2\nend\n",
            ),
            (
                "an Array field whose element is itself a container",
                "struct H { f: Array[Vec[D], 1] }\n",
                "let h: H = H { f: [[mkd(1), mkd(2)]] };",
                "dD1\ndD2\nend\n",
            ),
            (
                "an Array field of Arrays",
                "struct H { f: Array[Array[D, 1], 2] }\n",
                "let h: H = H { f: [[mkd(1)], [mkd(2)]] };",
                "dD1\ndD2\nend\n",
            ),
            (
                "the holder declares its own Drop too — own body first, then elements",
                "struct H { f: Array[D, 2] }\n\
                 impl Drop for H { fn drop(mut ref self) { println(\"dH\") } }\n",
                "let h: H = H { f: [mkd(1), mkd(2)] };",
                "dH\ndD1\ndD2\nend\n",
            ),
            (
                "one struct deeper, the control the row quotes",
                "struct H { f: Array[D, 2] }\nstruct G { h: H }\n",
                "let g: G = G { h: H { f: [mkd(1), mkd(2)] } };",
                "dD1\ndD2\nend\n",
            ),
            // Controls that must not move.
            (
                "control: an Array field of non-Drop elements runs nothing",
                "struct H { f: Array[N, 2] }\n",
                "let h: H = H { f: [N { v: 1 }, N { v: 2 }] };",
                "end\n",
            ),
            (
                "control: the flat Vec field, the one shape that always worked",
                "struct H { f: Vec[D] }\n",
                "let h: H = H { f: [mkd(1), mkd(2)] };",
                "dD1\ndD2\nend\n",
            ),
            (
                "control: the Array LOCAL, which localizes the defect to the field",
                "",
                "let a: Array[D, 2] = [mkd(1), mkd(2)];",
                "dD1\ndD2\nend\n",
            ),
            (
                // B-2026-09-12-21's own repro: the array reaches the field by
                // a MOVE out of a named local rather than as a literal. Fires
                // exactly once — the source binding does not keep a second
                // walk.
                "the array is MOVED into the field from a local (B-2026-09-12-21)",
                "struct H { f: Array[D, 2] }\n",
                "let a: Array[D, 2] = [mkd(1), mkd(2)];\nlet h = H { f: a };",
                "dD1\ndD2\nend\n",
            ),
            // B-2026-09-19-3 / B-2026-09-19-7 — the MOVED-FROM-A-NAMED-LOCAL
            // spelling for every container shape beside the flat `Array` above.
            // Each shape already had a cell in this test, and every one of them
            // was written as a LITERAL into the field, which is the spelling
            // that has no source binding to leave a second owner behind. So the
            // whole family passed while the moved-from-local form double-freed
            // (`Array`-outer) or segfaulted (`Array[Vec[D], 1]`), with nothing
            // in the tree exercising it.
            //
            // The `Vec`-outer pair are controls: they were always clean, because
            // `suppress_source_vec_cleanup_for_arg` covers them. Keeping them
            // here is what makes the `Array`-outer rows evidence about the
            // OUTER type rather than about moving in general.
            (
                "an Array-of-Array field MOVED from a local (B-2026-09-19-3)",
                "struct H { f: Array[Array[D, 1], 2] }\n",
                "let a: Array[Array[D, 1], 2] = [[mkd(1)], [mkd(2)]];\nlet h = H { f: a };",
                "dD1\ndD2\nend\n",
            ),
            (
                "an Array-of-Vec field MOVED from a local — SEGV'd at the DEFAULT -O2 (B-2026-09-19-7)",
                "struct H { f: Array[Vec[D], 1] }\n",
                "let a: Array[Vec[D], 1] = [[mkd(1), mkd(2)]];\nlet h = H { f: a };",
                "dD1\ndD2\nend\n",
            ),
            (
                "control: a flat Vec field MOVED from a local — always was clean",
                "struct H { f: Vec[D] }\n",
                "let a: Vec[D] = [mkd(1), mkd(2)];\nlet h = H { f: a };",
                "dD1\ndD2\nend\n",
            ),
            (
                "control: a Vec-of-Array field MOVED from a local — always was clean",
                "struct H { f: Vec[Array[D, 1]] }\n",
                "let a: Vec[Array[D, 1]] = [[mkd(1)], [mkd(2)]];\nlet h = H { f: a };",
                "dD1\ndD2\nend\n",
            ),
            (
                // B-2026-09-12-21, the GENERIC half, and a REGRESSION this row
                // introduced before catching it: the interpreter arm above
                // gates on the DECLARED field type being an array and then
                // fires value-driven, so `Array[T, 2]` admitted it there while
                // codegen's mono selector asked about the bare `T` and
                // declined. Silent on all four surfaces before B-2026-09-15-26,
                // compiled-silent / interp-firing after it, and agreed again
                // once the selector resolves the ELEMENT through the subst.
                "a generic parent whose field is Array[T, N] (B-2026-09-12-21)",
                "struct G[T] { a: Array[T, 2] }\n",
                "let g: G[D] = G { a: [mkd(1), mkd(2)] };",
                "dD1\ndD2\nend\n",
            ),
            (
                "control: the Vec[T] spelling of that generic field, always correct",
                "struct G[T] { a: Vec[T] }\n",
                "let g: G[D] = G { a: [mkd(1), mkd(2)] };",
                "dD1\ndD2\nend\n",
            ),
            (
                "control: a bare generic param bound to a Drop struct (B-2026-08-02-14)",
                "struct G[T] { a: T }\n",
                "let g: G[D] = G { a: mkd(1) };",
                "dD1\nend\n",
            ),
            (
                // B-2026-09-15-35 — these two were PINNED AT SILENCE here,
                // as the reason this row's selector resolved the array's
                // ELEMENT rather than the whole field TypeExpr: the whole-TE
                // substitution reaches them on the codegen side, and the
                // interpreter's field walk gated on a DECLARED container type
                // that `Path("T")` is not, so landing it alone traded the
                // agreed silence for a run-vs-build divergence.
                //
                // Both gates moved together in -15-35 and the pins move with
                // them. Codegen gained a `bare_param_container` leg in
                // `user_drop_field_indices_mono` (only the GATE was missing —
                // the emitter's array and Vec arms already key off the
                // whole-TE-substituted `field_te_resolved`), and the
                // interpreter's `Vec`/`VecDeque` field arm gained the
                // bare-generic-param exception B-2026-08-02-14 established for
                // the plain-struct case.
                //
                // ONE LEVEL, plain named element, on BOTH sides — which is why
                // the nested-container cells below still read `end`. The first
                // interpreter attempt routed through
                // `run_discarded_value_user_drops`, which recurses, and
                // `G[T] { a: T }` at `T = Vec[Vec[D]]` then printed `dD1` on
                // `--interp` alone: B-2026-09-15-23's agreed gap converted into
                // a divergence from the other direction.
                "a bare generic param bound to an Array (B-2026-09-15-35)",
                "struct G[T] { a: T }\n",
                "let g: G[Array[D, 2]] = G { a: [mkd(1), mkd(2)] };",
                "dD1\ndD2\nend\n",
            ),
            (
                "the same with the param bound to a Vec (B-2026-09-15-35)",
                "struct G[T] { a: T }\n",
                "let g: G[Vec[D]] = G { a: [mkd(1), mkd(2)] };",
                "dD1\ndD2\nend\n",
            ),
            (
                // Neither backend can tell a `Vec` from a `VecDeque` from the
                // runtime value alone, and the interpreter arm that gained the
                // exception serves both — so this rides along rather than
                // needing its own gate.
                "a bare generic param bound to a VecDeque (B-2026-09-15-35)",
                "struct G[T] { a: T }\n",
                "let mut d: VecDeque[D] = VecDeque.new();\n                 d.push_back(mkd(1));\n                 d.push_back(mkd(2));\n                 let g: G[VecDeque[D]] = G { a: d };",
                "dD1\ndD2\nend\n",
            ),
            (
                // The MOVED-LOCAL spelling of the field initializer, which the
                // row measured silent beside the literal one: fires exactly
                // once, the source binding keeps no second walk.
                "the container is MOVED into the bare-param field from a local (B-2026-09-15-35)",
                "struct G[T] { a: T }\n",
                "let v: Vec[D] = [mkd(1), mkd(2)];\nlet g: G[Vec[D]] = G { a: v };",
                "dD1\ndD2\nend\n",
            ),
            (
                // A sibling scalar field proves the widened leg admits the
                // FIELD rather than the whole struct, and that reverse
                // declaration order still holds around it.
                "a bare-param container field beside a scalar one (B-2026-09-15-35)",
                "struct G[T] { a: T, n: i64 }\n",
                "let g: G[Vec[D]] = G { a: [mkd(1)], n: 7 };",
                "dD1\nend\n",
            ),
            (
                // Control: the widening is keyed on the element running a user
                // `Drop`, not on the field being a container.
                "control: a bare-param container of non-Drop elements runs nothing",
                "struct G[T] { a: T }\n",
                "let g: G[Vec[N]] = G { a: [N { v: 1 }, N { v: 2 }] };",
                "end\n",
            ),
            (
                // PINNED at the agreed silence: a CONTAINER element in a
                // bare-param container field. Codegen's new leg holds itself to
                // a plain named element for exactly this reason, so the two
                // backends stay on one question. B-2026-09-15-23's subject, one
                // position over, and not this row's.
                "a bare param bound to Vec[Vec[D]] — the two rows' legs composing (B-2026-09-15-23)",
                "struct G[T] { a: T }\n",
                "let g: G[Vec[Vec[D]]] = G { a: [[mkd(1), mkd(2)]] };",
                "dD1\ndD2\nend\n",
            ),
            (
                "likewise a bare param bound to Array[Vec[D], 1] (B-2026-09-15-23)",
                "struct G[T] { a: T }\n",
                "let g: G[Array[Vec[D], 1]] = G { a: [[mkd(1)]] };",
                "dD1\nend\n",
            ),
            (
                "likewise a bare param bound to Vec[(D, i64)] (B-2026-09-15-23)",
                "struct G[T] { a: T }\n",
                "let g: G[Vec[(D, i64)]] = G { a: [(mkd(1), 5)] };",
                "dD1\nend\n",
            ),
            (
                // A hashed container under the bare param is a different
                // runtime value (`Value::Map`), so neither half of -15-35
                // reaches it — pinned so that stays deliberate.
                "pinned: likewise a bare param bound to Map[i64, D]",
                "struct G[T] { a: T }\n",
                "let mut m: Map[i64, D] = Map.new();\n                 m.insert(1, mkd(1));\n                 let g: G[Map[i64, D]] = G { a: m };",
                "end\n",
            ),
            (
                // B-2026-09-15-23's subject: a nested container in a `Vec`
                // field. Silent on BOTH backends, so it is an agreed gap and
                // not a divergence — pinned here to make a change to that gate
                // visible from this row.
                "a Vec[Vec[D]] field runs its innermost bodies (B-2026-09-15-23)",
                "struct H { f: Vec[Vec[D]] }\n",
                "let h: H = H { f: [[mkd(1), mkd(2)]] };",
                "dD1\ndD2\nend\n",
            ),
            (
                "a Vec[Array[D, 1]] field likewise — its HEAP was -15-27's (B-2026-09-15-23)",
                "struct H { f: Vec[Array[D, 1]] }\n",
                "let h: H = H { f: [[mkd(1)], [mkd(2)]] };",
                "dD1\ndD2\nend\n",
            ),
            (
                "the push-built spelling of the same field (B-2026-09-15-23)",
                "struct H { f: Vec[Array[D, 1]] }\n",
                "let mut v: Vec[Array[D, 1]] = [];\n\
                 let e1: Array[D, 1] = [mkd(1)];\n\
                 v.push(e1);\n\
                 let h: H = H { f: v };",
                "dD1\nend\n",
            ),
        ] {
            let src = format!("{HDR}{decls}fn main() {{\n{body}\nprintln(\"end\");\n}}\n");
            let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(&src);
            assert!(
                interp_errs.is_empty(),
                "[{label}] interp errored: {interp_errs:?}"
            );
            assert_eq!(interp_out.join(""), want, "[{label}] interpreter");
            if let Some(aot) = run_program(&src) {
                assert_eq!(aot, want, "[{label}] AOT");
            }
        }
}

/// B-2026-09-15-2 — a named `Array[T, N]` local moved into an enclosing
/// container LITERAL has exactly one owner.
///
/// The local carries a `StructDrop` of its own
/// (`make_array_param_callee_owned`); the literal bit-copies its `N`
/// element descriptors and registers its own walk (B-2026-09-10-8 for the
/// nested-array case); nothing stood the source down, so both freed the
/// same buffers — `free(): double free detected in tcache 2` at exit 134
/// on every compiled backend against a correct `--interp`, and `karac
/// check` printed `All checks passed` with NO diagnostic at all, because
/// nothing in the ownership pass models a container literal as a move.
///
/// STRICT `assert_eq!` ON THE AOT SIDE, deliberately. The pre-fix binary
/// ABORTS, and `run_program` answers `None` for a crash exactly as it does
/// for a missing toolchain — so the tolerant `if let Some(aot)` form this
/// file mostly uses would have passed vacuously on the very defect it was
/// written for. The strict form fails with `left: None`, which is the
/// signal B-2026-07-28-1 documents.
///
/// The controls carry the other half of the rule: `v.push(a)` (disarmed
/// since B-2026-09-13-15, which is what isolated this to the literals), a
/// fresh temp element (no named source to stand down), a `Map` value
/// position, a tuple literal (whose drop walker has no `Array` arm, so the
/// source must KEEP ownership), and a `Copy` element.
#[test]
fn e2e_array_local_moved_into_a_container_literal_has_one_owner() {
    const HDR: &str = "struct D { a: String, b: i64 }\n\
                           impl Drop for D { fn drop(mut ref self) { println(f\"dD{self.b}\") } }\n\
                           fn mkd(n: i64) -> D { return D { a: f\"payload-{n}-aaaaaaaaaaaaaaaa\", b: n }; }\n\
                           fn mka(t: String) -> Array[String, 2] { return [f\"{t}-aaaaaaaaaaaaaaaaaaaa\", f\"{t}-bbbbbbbbbbbbbbbbbbbb\"]; }\n";
    for (label, body, want) in [
            (
                "a named local into an ARRAY literal",
                "let a = mka(\"k1\");\nlet n: Array[Array[String, 2], 1] = [a];\nprintln(f\"{n[0][1]}\");",
                "k1-bbbbbbbbbbbbbbbbbbbb\nend\n",
            ),
            (
                "a named local into a VEC literal",
                "let a = mka(\"k2\");\nlet v: Vec[Array[String, 2]] = [a];\nprintln(f\"{v.len()}\");",
                "1\nend\n",
            ),
            (
                "TWO named locals into one array literal",
                "let a = mka(\"k3a\");\nlet b = mka(\"k3b\");\n\
                 let n: Array[Array[String, 2], 2] = [a, b];\nprintln(f\"{n[0][0]}\");",
                "k3a-aaaaaaaaaaaaaaaaaaaa\nend\n",
            ),
            (
                "a Drop-bearing element type",
                "let a: Array[D, 1] = [mkd(5)];\nlet n: Array[Array[D, 1], 1] = [a];",
                "dD5\nend\n",
            ),
            (
                "the source is READ AGAIN — the copy owns it, not the retraction",
                "let a = mka(\"kA\");\nlet n: Array[Array[String, 2], 1] = [a];\nprintln(f\"{a[0]}\");",
                "kA-aaaaaaaaaaaaaaaaaaaa\nend\n",
            ),
            (
                "control: v.push(a) was already disarmed",
                "let a = mka(\"k4\");\nlet mut v: Vec[Array[String, 2]] = [];\nv.push(a);\nprintln(f\"{v.len()}\");",
                "1\nend\n",
            ),
            (
                "control: a fresh temp element has no named source",
                "let n: Array[Array[String, 2], 1] = [mka(\"k6\")];\nprintln(f\"{n[0][0]}\");",
                "k6-aaaaaaaaaaaaaaaaaaaa\nend\n",
            ),
            (
                "control: a Map value position",
                "let a = mka(\"k7\");\nlet mut m: Map[i64, Array[String, 2]] = Map.new();\n\
                 m.insert(1, a);\nprintln(f\"{m.len()}\");",
                "1\nend\n",
            ),
            (
                "control: a tuple literal keeps the source as sole owner",
                "let a = mka(\"k8\");\nlet t = (a, 5);\nprintln(f\"{t.1}\");",
                "5\nend\n",
            ),
            (
                "control: a Copy element never moves at all",
                "let a: Array[i64, 2] = [1, 2];\nlet n: Array[Array[i64, 2], 1] = [a];\nprintln(f\"{n[0][1]}\");",
                "2\nend\n",
            ),
        ] {
            let src = format!("{HDR}fn main() {{\n{body}\nprintln(\"end\");\n}}\n");
            let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(&src);
            assert!(
                interp_errs.is_empty(),
                "[{label}] interp errored: {interp_errs:?}"
            );
            assert_eq!(interp_out.join(""), want, "[{label}] interpreter");
            assert_eq!(run_program(&src), Some(want.to_string()), "[{label}] AOT");
        }
}

/// B-2026-09-14-27 — an owned `Array[T, N]` READ AFTER it is handed over by
/// value reads its OWN buffers, on every backend.
///
/// `UseAfterMove` is advisory on the compiled surface by design
/// (`kind_blocks_production`), and what makes that safe is the defensive
/// copy: at a flagged move the consumer is handed an independent value and
/// the source keeps its own. `Array` took the CALLEE-OWNS convention
/// (B-2026-09-13-15 / -16) without that copy, so the hand-off retracted the
/// caller's element drop and the later read dangled — `karac check` printed
/// `warning[ownership]` then `All checks passed`, `karac build` produced a
/// binary, and the binary printed two DIFFERENT wrong strings on the two
/// compiled backends at exit 0 against a correct `--interp`.
///
/// The cells are the hand-off shapes, each measured broken before the fix:
/// a free function, a method, an associated function, `Vec.push` and
/// `Map.insert` with the container dying first (in `main` the container
/// outlives the read, which is the timing that hides this whole class), the
/// same binding twice, an enclosing array literal, and a variant
/// constructor. Long f-string elements on purpose — a short literal is
/// stored inline by SSO and never exercises the buffer at all.
///
/// THE CONTROLS ARE THE OTHER HALF OF THE RULE, because the copy must fire
/// exactly where the destination takes ownership. A struct-literal field, a
/// tuple literal and a plain rebind all keep the SOURCE as sole owner — the
/// destination's drop walker has no `Array` arm (B-2026-09-10-8 / -26) or
/// it takes the source's own memory (`rebind_source_keeps_array_memory`) —
/// so copying there strands the copy. Each was clean before this row and
/// stays clean; the tuple cell in particular leaked 44 B at `-O0` on an
/// intermediate version of the fix that copied at every destination.
#[test]
fn e2e_array_read_after_a_by_value_move_reads_its_own_buffers() {
    const HDR: &str = "fn take(a: Array[String, 2]) -> i64 { return a[0].len(); }\n\
                           struct Acc { id: i64 }\n\
                           impl Acc { fn eat(ref self, a: Array[String, 2]) -> i64 { return a[1].len(); } }\n\
                           struct St { }\n\
                           impl St { fn sv(a: Array[String, 2]) -> i64 { return a[1].len(); } }\n\
                           struct Hold { a: Array[String, 2], n: i64 }\n\
                           enum Wrp { Full(Array[String, 2]), Empty }\n\
                           fn wlen(w: Wrp) -> i64 { match w { Wrp.Full(x) => { return x[1].len(); } Wrp.Empty => { return 0; } } }\n\
                           fn takev(a: Array[Vec[i64], 2]) -> i64 { return a[0].len() + a[1].len(); }\n\
                           fn sumi(a: Array[i64, 3]) -> i64 { return a[0] + a[1] + a[2]; }\n\
                           fn takevs(v: Vec[String]) -> i64 { return v[0].len(); }\n\
                           fn gid[T](x: T) -> i64 { return 1; }\n\
                           fn mka(t: String) -> Array[String, 2] { return [f\"{t}-aaaaaaaaaaaaaaaaaaaa\", f\"{t}-bbbbbbbbbbbbbbbbbbbb\"]; }\n";
    for (label, body, want) in [
            (
                "a free function argument",
                "let a = mka(\"c1\");\nlet n = take(a);\nprintln(f\"{a[0]} {n}\");",
                "c1-aaaaaaaaaaaaaaaaaaaa 23\n",
            ),
            (
                "a method argument",
                "let h = Acc { id: 0 };\nlet a = mka(\"c2\");\nlet n = h.eat(a);\nprintln(f\"{a[0]} {n}\");",
                "c2-aaaaaaaaaaaaaaaaaaaa 23\n",
            ),
            (
                "an associated-function argument",
                "let a = mka(\"c3\");\nlet n = St.sv(a);\nprintln(f\"{a[0]} {n}\");",
                "c3-aaaaaaaaaaaaaaaaaaaa 23\n",
            ),
            (
                "Vec.push, with the container dying FIRST",
                "let a = mka(\"c4\");\n\
                 { let mut v: Vec[Array[String, 2]] = []; v.push(a); println(f\"in {v.len()}\"); }\n\
                 println(f\"{a[0]}\");",
                "in 1\nc4-aaaaaaaaaaaaaaaaaaaa\n",
            ),
            (
                "Map.insert, with the container dying FIRST",
                "let a = mka(\"c5\");\n\
                 { let mut m: Map[i64, Array[String, 2]] = Map.new(); m.insert(1, a); println(f\"in {m.len()}\"); }\n\
                 println(f\"{a[0]}\");",
                "in 1\nc5-aaaaaaaaaaaaaaaaaaaa\n",
            ),
            (
                "the same binding handed over TWICE",
                "let a = mka(\"c6\");\nlet x = take(a);\nlet y = take(a);\nprintln(f\"{a[0]} {x} {y}\");",
                "c6-aaaaaaaaaaaaaaaaaaaa 23 23\n",
            ),
            (
                "an element of an enclosing array literal",
                "let a = mka(\"c7\");\nlet n: Array[Array[String, 2], 1] = [a];\nprintln(f\"{a[0]}\");",
                "c7-aaaaaaaaaaaaaaaaaaaa\n",
            ),
            (
                "a user enum variant constructor",
                "let a = mka(\"c8\");\nlet w = Wrp.Full(a);\nprintln(f\"{a[0]} {wlen(w)}\");",
                "c8-aaaaaaaaaaaaaaaaaaaa 23\n",
            ),
            (
                "a GENERIC (monomorph) call argument",
                "let a = mka(\"c9\");\nlet n = gid(a);\nprintln(f\"{a[0]} {n}\");",
                "c9-aaaaaaaaaaaaaaaaaaaa 1\n",
            ),
            (
                "an Array[Vec[i64], N] argument",
                "let a: Array[Vec[i64], 2] = [[1, 2, 3], [4, 5]];\nlet n = takev(a);\nprintln(f\"{a[0].len()} {n}\");",
                "3 5\n",
            ),
            (
                "control: a struct-literal field keeps the source alias",
                "let a = mka(\"d1\");\nlet h = Hold { a: a, n: 1 };\nprintln(f\"{a[0]} {h.a[1].len()}\");",
                "d1-aaaaaaaaaaaaaaaaaaaa 23\n",
            ),
            (
                "control: a tuple literal keeps the source alias",
                "let a = mka(\"d2\");\nlet t = (a, 5);\nprintln(f\"{a[0]} {t.1}\");",
                "d2-aaaaaaaaaaaaaaaaaaaa 5\n",
            ),
            (
                "control: a plain rebind keeps the source alias",
                "let a = mka(\"d3\");\nlet b = a;\nprintln(f\"{a[0]} {b[1].len()}\");",
                "d3-aaaaaaaaaaaaaaaaaaaa 23\n",
            ),
            (
                "control: a Copy Array[i64, N] never moves at all",
                "let a: Array[i64, 3] = [1, 2, 3];\nlet n = sumi(a);\nprintln(f\"{a[0]} {n}\");",
                "1 6\n",
            ),
            (
                "control: Vec[String] is caller-retains and was always correct",
                "let v: Vec[String] = [f\"d5-aaaaaaaaaaaaaaaaaaaa\"];\nlet n = takevs(v);\nprintln(f\"{v[0]} {n}\");",
                "d5-aaaaaaaaaaaaaaaaaaaa 23\n",
            ),
        ] {
            let src = format!("{HDR}fn main() {{\n{body}\n}}\n");
            let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(&src);
            assert!(
                interp_errs.is_empty(),
                "[{label}] interp errored: {interp_errs:?}"
            );
            assert_eq!(interp_out.join(""), want, "[{label}] interpreter");
            if let Some(aot) = run_program(&src) {
                assert_eq!(aot, want, "[{label}] AOT");
            }
        }
}

/// B-2026-09-20-9 — an `Option`/`Result` argument whose payload is an
/// `Array` or a `Vec` loses its elements' user `Drop` bodies on every
/// compiled backend when the arm READS a scalar out of it.
///
/// ```text
/// fn take(o: Option[Array[W, 1]]) -> i64 { match o { Some(x) => { return x[0].v } .. } }
/// ```
///
/// printed `r:40 end` on the JIT and both AOT lanes against the
/// interpreter's correct `dW1_40 r:40 end`.
///
/// THE DISCRIMINATOR IS THE ARM'S EXPRESSION, not the payload, and the row
/// was filed the other way round because a 160-cell sweep varied the
/// payload TYPE while holding the arm fixed. Holding the payload at
/// `Array[W1, 1]` and varying only the arm settles it — the four cells
/// `no-read-control`, `scalar-read`, `read-into-a-local` and
/// `arithmetic-read-control` below are exactly that experiment, and two of
/// them were correct before this fix.
///
/// MECHANISM. `optres_payload_escape_map`'s per-projection copy-read policy
/// (`leaf_is_copy_read`) is what stops a by-copy projection out of a payload
/// being read as the payload ESCAPING. It was filtered to TUPLE payloads, so
/// `x[0].v` off an array payload was called an escape, the caller's
/// fresh-temp bodies walk stood down, and nothing anywhere ran the element
/// bodies. Two things had to move: the filter admits an array or `Vec`
/// payload, and the chain walk gained an INDEX hop
/// (`projection_leaf_te_through_index`) — written beside
/// `te_at_accessor_chain` rather than inside it because `ParamPart` has no
/// index variant and giving it one would reach `FieldSkipTree` and every
/// path-valued consumer built on it.
///
/// The PER-PART sibling (`optres_payload_escape_parts`) is deliberately NOT
/// widened: it answers in top-level TUPLE indices, which the skip tree can
/// express, and an array index is not that question.
#[test]
fn e2e_optres_array_payload_scalar_read_runs_the_element_bodies() {
    const W: &str = "struct W1 { v: i64 }\n\
             impl Drop for W1 { fn drop(mut ref self) { println(f\"dW1_{self.v}\") } }\n";
    // (label, source, AOT expectation, interpreter expectation)
    for (label, prog, want, interp_want) in [
            (
                // The row's own cell.
                "array-payload-scalar-read",
                format!(
                    "{W}fn take(o: Option[Array[W1, 1]]) -> i64 {{ match o {{ Some(x) => {{ return x[0].v }} None => {{ return 0 }} }} }}\n\
                     fn main() {{ let r = take(Some([W1 {{ v: 40 }}])); println(f\"r:{{r}}\"); println(\"end\") }}\n"
                ),
                "dW1_40\nr:40\nend\n",
                "dW1_40\nr:40\nend\n",
            ),
            (
                // The `Result` head, which the row listed and did not measure.
                // Its payload area is 5 words against `Option`'s 3, which is
                // why a fixed-element sweep crosses one envelope's boxing
                // boundary and not the other's — and why the two looked like
                // separate populations when they are one.
                "array-payload-result-head",
                format!(
                    "{W}fn take(o: Result[Array[W1, 1], i64]) -> i64 {{ match o {{ Ok(x) => {{ return x[0].v }} Err(e) => {{ return e }} }} }}\n\
                     fn main() {{ let r = take(Ok([W1 {{ v: 40 }}])); println(f\"r:{{r}}\"); println(\"end\") }}\n"
                ),
                "dW1_40\nr:40\nend\n",
                "dW1_40\nr:40\nend\n",
            ),
            (
                // The read moved into a local first. Same channel, reached
                // through `optres_payload_consumed_paths` instead.
                "array-payload-read-into-a-local",
                format!(
                    "{W}fn take(o: Option[Array[W1, 1]]) -> i64 {{ match o {{ Some(x) => {{ let q = x[0].v; return q }} None => {{ return 0 }} }} }}\n\
                     fn main() {{ let r = take(Some([W1 {{ v: 40 }}])); println(f\"r:{{r}}\"); println(\"end\") }}\n"
                ),
                "dW1_40\nr:40\nend\n",
                "dW1_40\nr:40\nend\n",
            ),
            (
                // CONTROL, correct BEFORE this fix: the same projection wrapped
                // in arithmetic is not a bare projection chain, so it never
                // reached the policy at all. This cell and the one below are
                // what make the discriminator the ARM and not the payload.
                "array-payload-arithmetic-read-control",
                format!(
                    "{W}fn take(o: Option[Array[W1, 1]]) -> i64 {{ match o {{ Some(x) => {{ return x[0].v + 0 }} None => {{ return 0 }} }} }}\n\
                     fn main() {{ let r = take(Some([W1 {{ v: 40 }}])); println(f\"r:{{r}}\"); println(\"end\") }}\n"
                ),
                "dW1_40\nr:40\nend\n",
                "dW1_40\nr:40\nend\n",
            ),
            (
                // CONTROL, correct BEFORE this fix: an arm that reads nothing
                // out of the payload has no projection to misclassify.
                "array-payload-no-read-control",
                format!(
                    "{W}fn take(o: Option[Array[W1, 1]]) -> i64 {{ match o {{ Some(x) => {{ return 7 }} None => {{ return 0 }} }} }}\n\
                     fn main() {{ let r = take(Some([W1 {{ v: 40 }}])); println(f\"r:{{r}}\"); println(\"end\") }}\n"
                ),
                "dW1_40\nr:7\nend\n",
                "dW1_40\nr:7\nend\n",
            ),
            (
                // The `Vec` twin, which the same widening repairs. Two elements,
                // so a fix that ran one body and stopped would show here.
                "vec-payload-index-read",
                format!(
                    "{W}fn take(o: Option[Vec[W1]]) -> i64 {{ match o {{ Some(x) => {{ return x[0].v }} None => {{ return 0 }} }} }}\n\
                     fn main() {{ let r = take(Some(vec![W1 {{ v: 40 }}, W1 {{ v: 41 }}])); println(f\"r:{{r}}\"); println(\"end\") }}\n"
                ),
                "dW1_40\ndW1_41\nr:40\nend\n",
                "dW1_40\ndW1_41\nr:40\nend\n",
            ),
            (
                // CONTROL and the boundary this widening must not cross. For a
                // NAMED struct payload the callee's own param machinery runs the
                // field bodies, so reading the projection as a copy makes the
                // caller a SECOND owner: applying the leaf policy there was
                // measured printing `dIn5 dIn5 r:5 end`. An array and a tuple
                // payload have no name and so no callee-side registration, which
                // is the whole of why they can take the policy and this cannot.
                "named-struct-payload-scalar-read-control",
                format!(
                    "{W}struct In {{ v: i64 }}\n\
                     impl Drop for In {{ fn drop(mut ref self) {{ println(f\"dIn{{self.v}}\") }} }}\n\
                     fn take(o: Option[In]) -> i64 {{ match o {{ Some(t) => {{ return t.v }} None => {{ return 0 }} }} }}\n\
                     fn main() {{ let r = take(Some(In {{ v: 5 }})); println(f\"r:{{r}}\"); println(\"end\") }}\n"
                ),
                "dIn5\nr:5\nend\n",
                "dIn5\nr:5\nend\n",
            ),
            (
                // CONTROL: the arm FORWARDS the payload whole into another call,
                // which really does hand it on. Correct before and after.
                "array-payload-forwarded-whole-control",
                format!(
                    "{W}fn sink(a: Array[W1, 1]) -> i64 {{ return a[0].v }}\n\
                     fn take(o: Option[Array[W1, 1]]) -> i64 {{ match o {{ Some(x) => {{ return sink(x) }} None => {{ return 0 }} }} }}\n\
                     fn main() {{ let r = take(Some([W1 {{ v: 40 }}])); println(f\"r:{{r}}\"); println(\"end\") }}\n"
                ),
                "dW1_40\nr:40\nend\n",
                "dW1_40\nr:40\nend\n",
            ),
            (
                // CONTROL: the arm RETURNS the payload whole, so the caller's
                // consumer owns the bodies and they run after `r:`. Correct
                // before and after — the escape here is real.
                "array-payload-whole-returned-control",
                format!(
                    "{W}fn take(o: Option[Array[W1, 1]]) -> Array[W1, 1] {{ match o {{ Some(x) => {{ return x }} None => {{ return [W1 {{ v: 0 }}] }} }} }}\n\
                     fn main() {{ let g = take(Some([W1 {{ v: 40 }}])); println(f\"r:{{g[0].v}}\"); println(\"end\") }}\n"
                ),
                "r:40\ndW1_40\nend\n",
                "r:40\ndW1_40\nend\n",
            ),
            (
                // CONTROL: a BOXED payload (element wider than `Option`'s 3-word
                // area) is a different channel entirely — the callee owns the
                // bodies there — and is correct before and after. This is the
                // cell that shows the fix did not simply move the boxing
                // boundary.
                "boxed-array-payload-control",
                format!(
                    "{W}struct W4 {{ v: i64, b: i64, c: i64, d: i64 }}\n\
                     impl Drop for W4 {{ fn drop(mut ref self) {{ println(f\"dW4_{{self.v}}\") }} }}\n\
                     fn take(o: Option[Array[W4, 1]]) -> i64 {{ match o {{ Some(x) => {{ return x[0].v }} None => {{ return 0 }} }} }}\n\
                     fn main() {{ let r = take(Some([W4 {{ v: 40, b: 1, c: 2, d: 3 }}])); println(f\"r:{{r}}\"); println(\"end\") }}\n"
                ),
                "dW4_40\nr:40\nend\n",
                "dW4_40\nr:40\nend\n",
            ),
            (
                // DIVERGENT AND PRE-EXISTING, pinned as it measures and NOT this
                // row's — B-2026-09-20-23. A NAMED-LOCAL argument's bodies are
                // retained by the caller's `let` site and drain at ITS scope
                // exit, so the body runs exactly once but AFTER `end`, where the
                // interpreter runs it at the callee's arm.
                //
                // Pre-existing rather than introduced: the same spelling with a
                // heap-bearing element (`struct S1 { tag: String }`) printed the
                // identical late sequence BEFORE this fix, when this cell printed
                // nothing at all. So the fix moves this cell from a LOSS into an
                // existing ORDER divergence, which is strictly closer to the due
                // sequence and valgrind-clean at `-O0` (0 errors, no leak).
                "array-payload-named-local-argument-runs-the-body-late",
                format!(
                    "{W}fn take(o: Option[Array[W1, 1]]) -> i64 {{ match o {{ Some(x) => {{ return x[0].v }} None => {{ return 0 }} }} }}\n\
                     fn main() {{ let a: Array[W1, 1] = [W1 {{ v: 40 }}]; let r = take(Some(a)); println(f\"r:{{r}}\"); println(\"end\") }}\n"
                ),
                "r:40\nend\ndW1_40\n",
                "dW1_40\nr:40\nend\n",
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

/// B-2026-09-15-15 — a MULTI-FIELD enum variant owns its boxed
/// `Array[T, N]` payload, and an arm that hands that payload on is
/// disarmed in both pattern shapes.
///
/// Two narrow gates, one at each end of the same ownership pair.
///
/// The CLASSIFIER (`declarations.rs`) upgraded an oversize array payload to
/// `EnumDropKind::BoxedArray` only for a variant with exactly one field, so
/// an array sharing its variant with any second field kept kind `None` and
/// the drop switch emitted nothing for it — the box was allocated and never
/// freed. Position and the sibling's type are both irrelevant, which is
/// what leaves the field COUNT: measured at `-O0`, `Both(a, b)` lost 96 B +
/// 88 B indirect, `M(a, 5)` / `M(5, a)` / `S { a, n }` / `M(a, "sib")` 48 B
/// + 44 B each, while the single-field `Full(a)` beside them was clean.
///
/// The DISARM (`register_boxed_array_payload_alias`) matched only a
/// `TupleVariant` pattern with exactly one sub-pattern. Once the classifier
/// widens, an arm that binds a multi-field variant's array out and hands it
/// on has two owners — the binding and the switch's interior walk — so
/// `tuplehand` and `out` abort with `free(): double free detected in tcache
/// 2`. Both are cells here.
///
/// WHAT THIS TEST DOES NOT GUARD, stated because the cell list reads as
/// though it does. Neither failure mode is visible from output: the leak
/// by nature, and the double free because `run_program` returns `None` on
/// a non-zero exit, which the tolerant `if let Some(aot)` form then skips.
/// Measured — with the fix reverted this test PASSES. It is kept for
/// interpreter/AOT parity on shapes that had none of it before;
/// `asan_multi_field_variant_owns_its_boxed_array_payload` is the
/// regression guard, and it is non-vacuous.
///
/// `struct1hand` IS A PRE-EXISTING DOUBLE FREE, not a consequence of the
/// widening, and it is the reason the disarm needed the struct arm rather
/// than just the multi-field one: `St1.S { a }` is a SINGLE-field variant
/// that the classifier already admitted, but a struct-shaped pattern has no
/// `TupleVariant` spelling, so nothing was ever recorded for it. It aborts
/// identically on stock `main` and is fixed here.
///
/// `srcread` is the use-after-free this leak was MASKING: with the box
/// unowned nothing freed the interior, so the moved-from source stayed
/// readable by accident. It reads correctly after the fix too — the
/// per-argument defensive copy B-2026-09-15-3 put in place already covers
/// multi-field constructors — and the cell pins that rather than assuming
/// it.
///
/// NOT REPAIRED, deliberately, and each has its own row: a GENERIC
/// multi-field variant (`G2[T] { Y(T, i64), N }` at `T = Array[String, 2]`)
/// still strands 48 B + 44 B, because an erased `T` payload cannot be
/// classified at declaration and the monomorphic path that would catch it
/// declines multi-field variants of its own (B-2026-09-15-18); a `shared
/// enum` is excluded from this machinery entirely (B-2026-09-15-10); and an
/// element running a user `Drop` BODY is declined by the widening on
/// purpose, because admitting it trades an agreed leak for a new
/// run-vs-build divergence (B-2026-09-15-17).
#[test]
fn e2e_multi_field_variant_owns_its_boxed_array_payload() {
    const HDR: &str = "fn mka(t: String) -> Array[String, 2] { return [f\"{t}-aaaaaaaaaaaaaaaaaaaa\", f\"{t}-bbbbbbbbbbbbbbbbbbbb\"]; }\n\
                           fn eat(a: Array[String, 2]) -> i64 { return a[0].len(); }\n\
                           enum Two { Both(Array[String, 2], Array[String, 2]), None2 }\n\
                           fn tlen(t: Two) -> i64 { match t { Two.Both(x, y) => { return x[0].len() + y[1].len(); } Two.None2 => { return 0; } } }\n\
                           enum Mix { M(Array[String, 2], i64), N }\n\
                           fn mlen(m: Mix) -> i64 { match m { Mix.M(x, k) => { return x[0].len() + k; } Mix.N => { return 0; } } }\n\
                           fn mhand(m: Mix) -> i64 { match m { Mix.M(x, k) => { return eat(x) + k; } Mix.N => { return 0; } } }\n\
                           fn mout(m: Mix) -> Array[String, 2] { match m { Mix.M(x, k) => { return x; } Mix.N => { return mka(\"z\"); } } }\n\
                           enum Mix2 { M(i64, Array[String, 2]), N }\n\
                           fn mlen2(m: Mix2) -> i64 { match m { Mix2.M(k, x) => { return x[0].len() + k; } Mix2.N => { return 0; } } }\n\
                           enum Tri { T(Array[String, 2], i64, Array[String, 2]), N }\n\
                           fn trilen(t: Tri) -> i64 { match t { Tri.T(x, k, y) => { return x[0].len() + k + y[1].len(); } Tri.N => { return 0; } } }\n\
                           enum St { S { a: Array[String, 2], n: i64 }, N }\n\
                           fn stlen(s: St) -> i64 { match s { St.S { a, n } => { return a[0].len() + n; } St.N => { return 0; } } }\n\
                           fn sthand(s: St) -> i64 { match s { St.S { a, n } => { return eat(a) + n; } St.N => { return 0; } } }\n\
                           enum St1 { S { a: Array[String, 2] }, N }\n\
                           fn st1hand(s: St1) -> i64 { match s { St1.S { a } => { return eat(a); } St1.N => { return 0; } } }\n\
                           enum Mx { M(Array[String, 2], String), N }\n\
                           fn mxlen(m: Mx) -> i64 { match m { Mx.M(x, s) => { return x[0].len() + s.len(); } Mx.N => { return 0; } } }\n\
                           enum Wrp { Full(Array[String, 2]), Empty }\n\
                           fn wlen(w: Wrp) -> i64 { match w { Wrp.Full(x) => { return x[0].len(); } Wrp.Empty => { return 0; } } }\n";
    for (label, body, want) in [
            (
                "two array payloads",
                "{ let w = Two.Both(mka(\"p\"), mka(\"q\")); println(f\"{tlen(w)}\"); }\nprintln(\"done\");",
                "44\ndone\n",
            ),
            (
                "array then scalar",
                "{ let w = Mix.M(mka(\"a\"), 5); println(f\"{mlen(w)}\"); }\nprintln(\"done\");",
                "27\ndone\n",
            ),
            (
                "scalar then array",
                "{ let w = Mix2.M(5, mka(\"b\")); println(f\"{mlen2(w)}\"); }\nprintln(\"done\");",
                "27\ndone\n",
            ),
            (
                "three fields, two arrays",
                "{ let w = Tri.T(mka(\"c\"), 5, mka(\"d\")); println(f\"{trilen(w)}\"); }\nprintln(\"done\");",
                "49\ndone\n",
            ),
            (
                "a struct-shaped variant",
                "{ let w = St.S { a: mka(\"e\"), n: 5 }; println(f\"{stlen(w)}\"); }\nprintln(\"done\");",
                "27\ndone\n",
            ),
            (
                "an array sibling of a String field",
                "{ let w = Mx.M(mka(\"k\"), f\"sib-aaaaaaaaaaaaaaaaaaaa\"); println(f\"{mxlen(w)}\"); }\nprintln(\"done\");",
                "46\ndone\n",
            ),
            (
                "an arm that HANDS ON a tuple variant's array — aborted before",
                "{ let w = Mix.M(mka(\"h\"), 5); println(f\"{mhand(w)}\"); }\nprintln(\"done\");",
                "27\ndone\n",
            ),
            (
                "an arm that RETURNS a tuple variant's array — aborted before",
                "{ let w = Mix.M(mka(\"j\"), 5); let r = mout(w); println(f\"{r[1].len()}\"); }\nprintln(\"done\");",
                "22\ndone\n",
            ),
            (
                "an arm that HANDS ON a struct-shaped variant's array — aborted before",
                "{ let w = St.S { a: mka(\"f\"), n: 5 }; println(f\"{sthand(w)}\"); }\nprintln(\"done\");",
                "27\ndone\n",
            ),
            (
                "the SINGLE-field struct-shaped spelling — a pre-existing abort",
                "{ let w = St1.S { a: mka(\"g\") }; println(f\"{st1hand(w)}\"); }\nprintln(\"done\");",
                "22\ndone\n",
            ),
            (
                "the moved-from source stays readable — the masked use-after-free",
                "let src = mka(\"m\");\n{ let w = Mix.M(src, 5); println(f\"{mlen(w)}\"); }\nprintln(f\"{src[0]}\");",
                "27\nm-aaaaaaaaaaaaaaaaaaaa\n",
            ),
            (
                "control: the single-field variant was always balanced",
                "{ let w = Wrp.Full(mka(\"n\")); println(f\"{wlen(w)}\"); }\nprintln(\"done\");",
                "22\ndone\n",
            ),
        ] {
            let src = format!("{HDR}fn main() {{\n{body}\n}}\n");
            let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(&src);
            assert!(
                interp_errs.is_empty(),
                "[{label}] interp errored: {interp_errs:?}"
            );
            assert_eq!(interp_out.join(""), want, "[{label}] interpreter");
            if let Some(aot) = run_program(&src) {
                assert_eq!(aot, want, "[{label}] AOT");
            }
        }
}

/// B-2026-09-15-3 — a user ENUM VARIANT CONSTRUCTOR is the hand-off site
/// B-2026-09-14-27's copy did not reach, and this pins it closed on every
/// backend.
///
/// That row's own constructor cell reads the source in the SAME statement
/// that consumes the enum (`println(f"{a[0]} {wlen(w)}")`), so the read
/// happens before anything frees and the cell passed while the site was
/// broken. TIMING IS THE WHOLE TRAP HERE: every cell below therefore lets
/// the enum DIE FIRST — the inner block in `main`, or the callee that takes
/// it by value — and only then reads the source.
///
/// WHY IT SURVIVED THAT FIX, and it is an ORDERING bug rather than a
/// missing copy. `try_compile_enum_variant_at` stood every argument down
/// (`suppress_array_local_move_into_ctor`) in a loop of its own, BEFORE it
/// compiled any payload. The skip inside that helper keys on
/// `uam_copied_sites` — "a copy really happened" — and the copy for this
/// site is emitted by `maybe_defensive_copy_param_arg`, several lines INTO
/// the payload loop. So at skip time the record did not exist yet: the
/// retraction ran unprotected, and the copy it should have protected was
/// made a moment later and stored in a payload whose drop freed it while
/// the source still pointed at the original. Measured as valgrind
/// `Invalid read` and three different wrong strings across JIT / `-O0` /
/// auto-par at exit 0, against a correct `--interp`. The fix moves the
/// retraction to just after that copy; nothing else changed.
///
/// ADDING A SECOND COPY HERE IS THE WRONG SHAPE, measured rather than
/// reasoned: an intermediate version emitted its own copy ahead of the
/// retraction, which left `maybe_defensive_copy_param_arg`'s copy running
/// too, and every repaired cell then leaked 46 B in 2 blocks at `-O0` —
/// one stranded duplicate per array. The existing copy was always in the
/// right place; only the retraction was not.
///
/// The cells are the spellings that differ in WHO frees and WHEN: a
/// block-scoped enum with no callee at all (the smallest repro — the
/// block's own drop is the free), the same handed to an owned callee, the
/// enum at function scope, the constructor in argument position, a
/// GENERIC enum whose payload boxes, an array PARAM as the source, and a
/// struct-element array. Seven, each measured broken before the fix.
///
/// THE CONTROLS PIN THE OTHER HALF, because a copy at a site that does not
/// take ownership strands it. A SEEDED `Option` payload is excluded from
/// the retraction on purpose (B-2026-09-10-6) and was always correct; a
/// `shared` enum is RC-managed and likewise excluded; a scalar
/// `Array[i64, N]` owns no heap; and a fresh temp has no source to
/// protect. Each prints correctly before and after.
///
/// The TWO-array-payload cell is a control of a different kind: its output
/// was correct before this row and stays correct, but a MULTI-field
/// variant strands its boxed array payload (48 B plus its elements, per
/// array) on every backend — B-2026-09-15-15, pre-existing and byte-
/// identical across this change. It is asserted for OUTPUT only here, and
/// deliberately kept out of the memory fixture
/// (`asan_array_moved_into_an_enum_ctor_is_balanced`) for that reason. The
/// `shared` cell carries the same caveat under B-2026-09-15-10.
#[test]
fn e2e_array_moved_into_an_enum_ctor_leaves_the_source_readable() {
    const HDR: &str = "fn mka(t: String) -> Array[String, 2] { return [f\"{t}-aaaaaaaaaaaaaaaaaaaa\", f\"{t}-bbbbbbbbbbbbbbbbbbbb\"]; }\n\
                           enum Wrp { Full(Array[String, 2]), Empty }\n\
                           fn wlen(w: Wrp) -> i64 { match w { Wrp.Full(x) => { return x[0].len(); } Wrp.Empty => { return 0; } } }\n\
                           enum Two { Both(Array[String, 2], Array[String, 2]), None2 }\n\
                           fn tlen(t: Two) -> i64 { match t { Two.Both(x, y) => { return x[0].len() + y[0].len(); } Two.None2 => { return 0; } } }\n\
                           enum G[T] { Y(T), N }\n\
                           fn glen(g: G[Array[String, 2]]) -> i64 { match g { G.Y(x) => { return x[0].len(); } G.N => { return 0; } } }\n\
                           shared enum Sh { Full(Array[String, 2]), Empty }\n\
                           fn slen(s: Sh) -> i64 { match s { Sh.Full(x) => { return x[0].len(); } Sh.Empty => { return 0; } } }\n\
                           enum Wi { Full(Array[i64, 2]), Empty }\n\
                           fn ilen(w: Wi) -> i64 { match w { Wi.Full(x) => { return x[0]; } Wi.Empty => { return 0; } } }\n\
                           struct P { s: String }\n\
                           enum Wp { Full(Array[P, 2]), Empty }\n\
                           fn plen(w: Wp) -> i64 { match w { Wp.Full(x) => { return x[0].s.len(); } Wp.Empty => { return 0; } } }\n\
                           fn takeo(o: Option[Array[String, 2]]) -> i64 { match o { Some(x) => { return x[0].len(); } None => { return 0; } } }\n\
                           fn viaparam(a: Array[String, 2]) { { let w = Wrp.Full(a); println(f\"in {wlen(w)}\"); } println(f\"{a[0]}\"); }\n";
    for (label, body, want) in [
            (
                "the enum dies at the end of an inner block, with NO callee",
                "let a = mka(\"c1\");\n\
                 { let w = Wrp.Full(a); match w { Wrp.Full(x) => { println(f\"in {x[0].len()}\"); } Wrp.Empty => { println(\"e\"); } } }\n\
                 println(f\"{a[0]}\");",
                "in 23\nc1-aaaaaaaaaaaaaaaaaaaa\n",
            ),
            (
                "the enum dies at the end of an inner block, handed to an owned callee",
                "let a = mka(\"c2\");\n{ let w = Wrp.Full(a); println(f\"in {wlen(w)}\"); }\nprintln(f\"{a[0]}\");",
                "in 23\nc2-aaaaaaaaaaaaaaaaaaaa\n",
            ),
            (
                "the enum at FUNCTION scope, consumed by an owned callee",
                "let a = mka(\"c3\");\nlet w = Wrp.Full(a);\nprintln(f\"in {wlen(w)}\");\nprintln(f\"{a[0]}\");",
                "in 23\nc3-aaaaaaaaaaaaaaaaaaaa\n",
            ),
            (
                "the constructor in ARGUMENT position",
                "let a = mka(\"c4\");\nprintln(f\"in {wlen(Wrp.Full(a))}\");\nprintln(f\"{a[0]}\");",
                "in 23\nc4-aaaaaaaaaaaaaaaaaaaa\n",
            ),
            (
                "a GENERIC enum, whose payload boxes",
                "let a = mka(\"c5\");\n{ let g = G.Y(a); println(f\"in {glen(g)}\"); }\nprintln(f\"{a[0]}\");",
                "in 23\nc5-aaaaaaaaaaaaaaaaaaaa\n",
            ),
            (
                "an array PARAM as the source",
                "viaparam(mka(\"c6\"));",
                "in 23\nc6-aaaaaaaaaaaaaaaaaaaa\n",
            ),
            (
                "a struct-element array",
                "let a: Array[P, 2] = [P { s: f\"c7-aaaaaaaaaaaaaaaaaaaa\" }, P { s: f\"c7-bbbbbbbbbbbbbbbbbbbb\" }];\n\
                 { let w = Wp.Full(a); println(f\"in {plen(w)}\"); }\n\
                 println(f\"{a[0].s}\");",
                "in 23\nc7-aaaaaaaaaaaaaaaaaaaa\n",
            ),
            (
                "control: a SEEDED Option payload is excluded from the retraction",
                "let a = mka(\"d1\");\n{ println(f\"in {takeo(Option.Some(a))}\"); }\nprintln(f\"{a[0]}\");",
                "in 23\nd1-aaaaaaaaaaaaaaaaaaaa\n",
            ),
            (
                "control: a shared enum is RC-managed and likewise excluded",
                "let a = mka(\"d2\");\n{ let w = Sh.Full(a); println(f\"in {slen(w)}\"); }\nprintln(f\"{a[0]}\");",
                "in 23\nd2-aaaaaaaaaaaaaaaaaaaa\n",
            ),
            (
                "control: a scalar Array[i64, N] owns no heap",
                "let a: Array[i64, 2] = [7, 8];\n{ let w = Wi.Full(a); println(f\"in {ilen(w)}\"); }\nprintln(f\"{a[0]}\");",
                "in 7\n7\n",
            ),
            (
                "control: a variant carrying TWO array payloads",
                "let a = mka(\"d4\");\nlet b = mka(\"e4\");\n\
                 { let w = Two.Both(a, b); println(f\"in {tlen(w)}\"); }\n\
                 println(f\"{a[0]} {b[1]}\");",
                "in 46\nd4-aaaaaaaaaaaaaaaaaaaa e4-bbbbbbbbbbbbbbbbbbbb\n",
            ),
            (
                "control: a fresh temp has no source to protect",
                "{ let w = Wrp.Full(mka(\"d5\")); println(f\"in {wlen(w)}\"); }\nprintln(\"done\");",
                "in 23\ndone\n",
            ),
        ] {
            let src = format!("{HDR}fn main() {{\n{body}\n}}\n");
            let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(&src);
            assert!(
                interp_errs.is_empty(),
                "[{label}] interp errored: {interp_errs:?}"
            );
            assert_eq!(interp_out.join(""), want, "[{label}] interpreter");
            if let Some(aot) = run_program(&src) {
                assert_eq!(aot, want, "[{label}] AOT");
            }
        }
}

/// B-2026-08-21-53 — the TYPE-QUALIFIED associated call `Type[Args].fn(a)`.
///
/// This is the spelling design.md § Generics settles on for explicit type
/// selection, and it passed `karac check` and then died in BOTH backends: the
/// interpreter with "path 'Box' has no interpreter evaluation rule", and
/// codegen with an LLVM module-verification failure, because the fresh-temp
/// receiver helper compiled the TYPE as a value and passed it as `self`:
///
///   Incorrect number of arguments passed to called function!
///     %call = call { i64 } @"Box.make$i64"(i64 %__urecv_tmp_0, i64 7)
///
/// Every case here is asserted against the value the UNQUALIFIED spelling
/// produces, since the two must agree by construction.
/// B-2026-08-21-43 — a user impl on a NON-SCALAR `Array` head, called on a
/// TEMPORARY. B-2026-08-21-25 admitted only scalar elements and left this
/// loud, because who frees an `Array[String, N]` temporary's element
/// buffers was unestablished. It was in fact already established by the
/// BOUND spelling, which this arm re-dispatches into — the widening only
/// waited on B-2026-08-22-18, the double free that made the bound path
/// unsafe for heap elements in the first place.
///
/// The bound twin is asserted alongside, as the control: it always
/// compiled, so a regression that broke both would still be caught.
#[test]
fn test_e2e_user_impl_on_a_nonscalar_array_temporary() {
    const DECLS: &str = "trait Tag { fn tag(self) -> i64; }\n\
                             impl Tag for Array[String, 2] { fn tag(self) -> i64 { return 42i64; } }\n\
                             fn mk() -> Array[String, 2] { return [\"a\", \"b\"]; }\n";
    assert_eq!(
        run_program(&format!("{DECLS}fn main() {{ println(mk().tag()); }}\n")).as_deref(),
        Some("42\n"),
        "the temporary spelling must build and run"
    );
    assert_eq!(
        run_program(&format!(
            "{DECLS}fn main() {{ let a = mk(); println(a.tag()); }}\n"
        ))
        .as_deref(),
        Some("42\n"),
        "the bound spelling is the control and must keep working"
    );
}

/// The heap half: an element flows OUT of the consumed temporary. Pinned
/// separately from the scalar-returning case above because it is the shape
/// that was memory-unsafe until B-2026-08-22-18 — the value is right here
/// either way, so only the ASAN twin proves the buffers are balanced.
#[test]
fn test_e2e_nonscalar_array_temporary_returning_an_element() {
    assert_eq!(
            run_program(
                "trait Tag { fn take_first(self) -> String; }\n\
                 impl Tag for Array[String, 2] { fn take_first(self) -> String { return self[0]; } }\n\
                 fn mk() -> Array[String, 2] { return [\"abc\", \"de\"]; }\n\
                 fn main() { println(mk().take_first()); }\n"
            )
            .as_deref(),
            Some("abc\n"),
            "an element returned out of an array temporary must survive the call"
        );
}

/// B-2026-08-13-22 — an ANNOTATED ARRAY literal must lay its aggregate out at
/// the DECLARED element width, the third member of the family after the
/// tuple in `let` position (B-2026-08-13-17) and everywhere else
/// (B-2026-08-13-21).
///
/// `let a: Array[i64, 2] = [v, v]` with `v: u8 = 200` built `[2 x i8]` and
/// every read trusted the declared `i64`, so it printed -56 on all three
/// compiled surfaces while the interpreter printed 200.
///
/// NOTHING WAS WRONG WITH THE COERCION. `compile_array_literal` already
/// coerced through `coerce_literal_elem_to_type_from`, which takes the
/// element EXPRESSION and so picks zext over sext from the source's
/// signedness — that is why lines 02 (signed source) and 03 (explicit `as`)
/// were correct all along, and why only unsigned sources diverged. The hint
/// that gates that coercion was simply never set for an `Array[T, N]`
/// binding, which registers in `array_elem_type_exprs` rather than
/// `vec_elem_types`. The fix seeds the existing element-hint channel; no
/// third `pending_*` carrier was needed.
///
/// Lines 04-07 are the sibling POSITIONS, which fail loudly rather than
/// silently (`ret [2 x i8]` against a `[2 x i64]` signature, and so on) —
/// the array half of B-2026-08-13-21, closed here by generalizing that row's
/// staging helper to both aggregate literal forms.
///
/// Line 08 is a fifth site the family sweep does not reach: `Vector[T, N]
/// .from_array` widened each lane with the signedness-BLIND coercion, so two
/// `u8` lanes of 200 reduced to -112 instead of 400. Silent, like the `let`
/// case, and fixed the same way — by handing the coercion the lane's source
/// expression.
#[test]
fn test_e2e_annotated_array_uses_declared_element_width() {
    let src = r#"
struct P { a: Array[i64, 2] }
fn take(a: Array[i64, 2]) -> i64 { return a[0i64]; }
fn give() -> Array[i64, 2] { let v = 200u8; return [v, v]; }
fn givetail() -> Array[i64, 2] { let v = 200u8; [v, v] }

fn main() {
    let v = 200u8;

    let a: Array[i64, 2] = [v, v];
    println(f"01 {a[0i64]}");

    let s = 100i8;
    let b: Array[i64, 2] = [s, s];
    println(f"02 {b[0i64]}");

    let c: Array[i64, 2] = [v as i64, v as i64];
    println(f"03 {c[0i64]}");

    let r1 = give();     println(f"04 {r1[0i64]}");
    let r2 = givetail(); println(f"05 {r2[0i64]}");
    println(f"06 {take([v, v])}");
    let p = P { a: [v, v] };
    println(f"07 {p.a[0i64]}");

    let w: Vector[i64, 2] = Vector[i64, 2].from_array([v, v]);
    println(f"08 {w.reduce_sum()}");
}
"#;
    assert_eq!(
        run_program(src).as_deref(),
        Some(
            "01 200\n\
                 02 100\n\
                 03 200\n\
                 04 200\n\
                 05 200\n\
                 06 200\n\
                 07 200\n\
                 08 400\n"
        ),
    );
}

/// Regression (B-2026-07-30-3, second defect): the CALL ORDER above is
/// load-bearing, so this asserts the reverse.
///
/// Fixing the slice header alone left a second, independent fault. An Array
/// argument bound `T`'s LLVM type (the `Slice` arm of
/// `augment_subst_from_arg_elem_types` reads the array slot via
/// `infer_elem_from_source`) but NOT `T`'s NAME, because an Array local had
/// no registered element `TypeExpr` anywhere. The name is what specializes
/// the per-type clone helper, so two instantiations of ONE generic emitted a
/// single shared `karac_clone_T` sized to whichever was lowered FIRST:
///
///   * `i64` then `String` → the String ran through an 8-byte i64 clone,
///     leaving `len`/`cap` as uninitialized alloca garbage → SIGSEGV;
///   * `String` then `i64` → the i64 ran through a 24-byte clone, reading
///     past an 8-byte alloca and writing 24 bytes into it. That one PRINTED
///     THE RIGHT ANSWER while corrupting the stack, which is why order
///     decided whether the bug looked real.
///
/// Both orders are asserted here for that reason, and the elements are
/// heap-allocated (`"own-" + ...`) rather than literals so a shallow clone
/// of the `{ptr,len,cap}` cannot accidentally survive.
#[test]
fn e2e_array_generic_monos_get_per_type_clone_helpers_either_order() {
    if let Some(out) = run_program(
        "fn first[T](s: ref Slice[T]) -> T { s[0] }\n\
             fn main() {\n\
             \x20   let a1: Array[i64, 2] = [100i64, 7i64];\n\
             \x20   let a2: Array[String, 2] = [\"own-\" + \"alpha\", \"own-\" + \"beta\"];\n\
             \x20   println(f\"[{first(a1)}]\");\n\
             \x20   println(f\"[{first(a2)}]\");\n\
             \x20   let b1: Array[String, 2] = [\"own-\" + \"gamma\", \"own-\" + \"delta\"];\n\
             \x20   let b2: Array[i64, 2] = [42i64, 9i64];\n\
             \x20   println(f\"[{first(b1)}]\");\n\
             \x20   println(f\"[{first(b2)}]\");\n\
             }",
    ) {
        assert_eq!(out, "[100]\n[own-alpha]\n[own-gamma]\n[42]\n");
    }
}

/// B-2026-09-04-28 — an `Array[T, N]` BOUND OUT of a struct field keeps
/// its element type, so a field read through one of its elements lowers.
///
/// The DIRECT spelling `h.a[0].id` has resolved since B-2026-08-14-5 (the
/// test above pins it): `type_name_of_expr`'s indexed-FieldAccess root
/// unwraps the field's declared `Array[T, N]`. The BOUND spelling did not —
///
///     let x = h.a;
///     let e = ref x[0];
///     println(f"{e.id}/{e.tag}");
///
/// — because by the time the element is indexed the root is a plain
/// identifier and the struct it came from is gone from the expression.
/// `array_elem_type_expr_from_rhs`, which is what an un-annotated `Array`
/// binding records its element from, answered only for a CALL and for the
/// two array-valued literals, so a field read fell through and the binding
/// recorded no element type at all. `e.id` then hit codegen's own loud
/// "cannot resolve field 'id' on this receiver (its type was not recorded
/// for codegen)" on `karac build` and `karac run` alike, while `--interp`
/// printed `2/tag2` — a run-vs-build divergence that fails loudly, which is
/// why the row was medium rather than high.
///
/// THE ANNOTATED SPELLING ALREADY WORKED (`let x: Array[R, 2] = h.a;`
/// records the element from the annotation), so the two spellings of one
/// move disagreed; the cells below assert both, since a fix that records
/// only one of them leaves the split in place.
///
/// The `self.ns` cell is the receiver spelling of the same lookup, and
/// `is_sorted` is there because it is the one consumer of the recorded
/// element `TypeExpr` that is not a field read — it needs the element's
/// SIGNEDNESS to pick an ordering, so it exercises the recording rather
/// than just the layout.
///
/// No `shared struct` cell: that spelling is now REJECTED at typecheck
/// (`E_SHARED_FIELD_MOVE` — see `shared_field_move_covers_tuple_and_option…`
/// in tests/typechecker.rs), which is the other half of this row's work.
#[test]
fn test_e2e_array_bound_out_of_a_struct_field_keeps_its_element_type() {
    assert_eq!(
        run_program(
            "struct R { id: i64, tag: String }\n\
                 struct Ha { a: Array[R, 2] }\n\
                 struct Nums { ns: Array[i64, 3] }\n\
                 fn mk(n: i64) -> R { return R { id: n, tag: f\"tag{n}\" }; }\n\
                 impl Nums {\n\
                     fn sorted(ref self) -> bool {\n\
                         let x = self.ns;\n\
                         return x.is_sorted();\n\
                     }\n\
                 }\n\
                 fn main() {\n\
                     let h = Ha { a: [mk(2), mk(3)] };\n\
                     println(f\"direct {h.a[0].id}/{h.a[1].tag}\");\n\
                     let h2 = Ha { a: [mk(4), mk(5)] };\n\
                     let x = h2.a;\n\
                     let e = ref x[0];\n\
                     println(f\"bound {e.id}/{e.tag}\");\n\
                     let h3 = Ha { a: [mk(6), mk(7)] };\n\
                     let y: Array[R, 2] = h3.a;\n\
                     println(f\"annot {y[1].id}/{y[1].tag}\");\n\
                     let n = Nums { ns: [1, 2, 3] };\n\
                     println(f\"sorted {n.sorted()}\");\n\
                 }"
        )
        .as_deref(),
        Some("direct 2/tag3\nbound 4/tag4\nannot 7/tag7\nsorted true\n"),
    );
}

/// B-2026-09-04-34 — a moved-out `Array[E, N]` FIELD disarms the source, so
/// the element buffers are freed once.
///
/// `let x = h.a;` moves the field out of `h`, and every neutralizer that
/// makes that safe for the other field shapes is keyed on the field's LLVM
/// type being a `StructType`. An array field is an `ArrayType`, so it fell
/// out of `suppress_struct_field_move_by_name`'s match entirely and nothing
/// was suppressed: the source struct's drop went on freeing element buffers
/// the binding now owned.
///
/// MEASURED AGAINST ITS FOUR SIBLINGS on one program — a `Vec`, `String`,
/// plain-struct and tuple field each moved out of the identical
/// `let x = h.f;` cleanly, and only `Array[R, 2]` (with
/// `struct R { id: i64, tag: String }`) ended in `free(): double free
/// detected in tcache 2` at scope exit, against a correct `--interp`. In the
/// dead-binding form the source read garbage before that (`h ����` where
/// `--interp` prints `h tag2`) — the use-after-free the double free is the
/// tail of.
///
/// The array itself owns nothing; its ELEMENTS do. So the neutralizer is
/// the per-element rule applied N times, which is why the cells below cover
/// a struct element (heap inside a nested aggregate), a direct `String`
/// element, and — as the over-reach control — a scalar element, whose
/// elements own nothing and must stay untouched.
///
/// Asserts VALUES rather than needing a sanitizer twin for the abort: a
/// double free at scope exit takes the whole program down, so a
/// `run_program` that returns the expected stdout is already the assertion.
/// The `memory_sanitizer` twin (`asan_array_field_move_out_frees_once`)
/// covers the quieter half — that nothing LEAKS once the source is
/// disarmed.
#[test]
fn test_e2e_array_field_move_out_disarms_the_source() {
    assert_eq!(
        run_program(
            "struct R { id: i64, tag: String }\n\
                 struct Ha { a: Array[R, 2] }\n\
                 struct Hs { a: Array[String, 2] }\n\
                 struct Hn { a: Array[i64, 2] }\n\
                 fn mk(n: i64) -> R { return R { id: n, tag: f\"tag{n}\" }; }\n\
                 fn main() {\n\
                     let h = Ha { a: [mk(2), mk(3)] };\n\
                     let x = h.a;\n\
                     let e = ref x[0];\n\
                     println(f\"struct {e.id}/{e.tag}\");\n\
                     let hs = Hs { a: [f\"one\", f\"two\"] };\n\
                     let xs = hs.a;\n\
                     println(f\"string {xs[0]}/{xs[1]}\");\n\
                     let hn = Hn { a: [7, 9] };\n\
                     let xn = hn.a;\n\
                     println(f\"scalar {xn[0]}/{xn[1]}\");\n\
                 }"
        )
        .as_deref(),
        Some("struct 2/tag2\nstring one/two\nscalar 7/9\n"),
    );
}

// ── Array[T, N] fixed-size arrays ────────────────────────────

#[test]
fn test_ir_array_param_type() {
    // Array[T, N] parameter lowers to LLVM `[N x T]`.
    let ir = ir_for("fn take(a: Array[i64, 4]) { }");
    assert!(
        ir.contains("[4 x i64]"),
        "expected [4 x i64] in IR, got:\n{}",
        ir
    );
}

#[test]
fn test_ir_array_different_sizes() {
    let ir = ir_for(
        "fn small(a: Array[i64, 3]) { }
             fn big(a: Array[i64, 16]) { }",
    );
    assert!(ir.contains("[3 x i64]"));
    assert!(ir.contains("[16 x i64]"));
}

#[test]
fn test_ir_array_of_bool() {
    let ir = ir_for("fn flags(a: Array[bool, 8]) { }");
    assert!(
        ir.contains("[8 x i1]"),
        "expected [8 x i1] in IR, got:\n{}",
        ir
    );
}

#[test]
fn test_ir_array_literal_construction() {
    // A bare `[…]` literal is `Vec[T]` by the typechecker's synthesis-mode
    // rule, so it now lowers (via the lowering `ArrayLiteral`→`Vec[…]`
    // canonicalization) to the `{ptr, i64, i64}` Vec struct — NOT a fixed
    // `[3 x i64]` array. (Pre-fix it emitted `[3 x i64]`, which segfaulted
    // on a subsequent `.push` and failed verification on a Vec return.)
    // The fixed-array lowering is exercised by `test_ir_array_literal_let_binding`
    // (`let a: Array[i64, 2] = …`) instead.
    let ir = ir_for("fn main() { let a = [10, 20, 30]; }");
    assert!(
        ir.contains("{ ptr, i64, i64 }"),
        "expected Vec struct for bare array literal, got:\n{}",
        ir
    );
}

#[test]
fn test_ir_array_literal_let_binding() {
    let ir = ir_for("fn main() { let a: Array[i64, 2] = [1, 2]; }");
    // Array should be alloca'd and stored.
    assert!(
        ir.contains("alloca [2 x i64]"),
        "expected alloca [2 x i64], got:\n{}",
        ir
    );
}

/// Reading an element out of an array LITERAL temporary keeps the value
/// the interpreter produces, now that the read clone-then-drops the
/// temporary (B-2026-08-27-35).
///
/// The leak that row filed is asserted by
/// `asan_indexed_array_literal_temp_drops_its_elements`; this is the value
/// half, because the fix DEEP-CLONES the element before freeing the array
/// around it. Get that order wrong and the program still exits 0 while
/// printing freed memory, which no leak count would catch. The rodata and
/// moved-binding legs are here for the same reason they are in the ASAN
/// fixture: they reach the same drop through different element ownership.
#[test]
fn test_e2e_indexed_array_literal_temp_reads_the_right_element() {
    let out = run_program(
        r#"
fn mk(n: i64) -> String {
    return f"item{n}";
}

fn main() {
    let n = 3;
    println(Array[f"aaaa{n}", f"b{n}"][0]);
    println(Array["aa", "bb"][1]);
    println(Array[mk(1), mk(2)][1]);
    println(Array[n, n + 1][1]);
    let s = f"sssss{n}";
    let t = f"t{n}";
    println(Array[s, t][0]);
    for i in 0..3 {
        println(Array[f"x{i}", f"y{i}"][1]);
    }
    println(f"{Array[f"p{n}", f"q{n}"] == Array[f"p{n}", f"q{n}"]}");
    println(f"{Array["aa", "bb"] == Array["aa", "cc"]}");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out,
            "aaaa3\nbb\nitem2\n4\nsssss3\ny0\ny1\ny2\ntrue\nfalse\n"
        );
    }
}

#[test]
fn test_e2e_fixed_array_read_methods() {
    // B-2026-07-17-19: the fixed-array read surface `get`/`first`/`last`/
    // `contains`/`is_empty` now codegens (was "no handler for method 'get'"
    // AOT build-fail), matching the interpreter (which dispatches a fixed
    // array as a Vec). Covers i64, f64 (the payload must reconstruct as a
    // float, not its i64 bit pattern — the typechecker types `get` as
    // `Option[f64]`), bool, and a `ref Array` param.
    let out = run_program(
        r#"
fn sum_ref(a: ref Array[i64, 3]) -> i64 {
    let mut s = 0;
    match a.first() { Some(v) => { s = s + v; }, None => {} }
    match a.last() { Some(v) => { s = s + v; }, None => {} }
    if a.contains(20) { s = s + 100; }
    s
}
fn main() {
    let a: Array[i64, 4] = [10, 20, 30, 40];
    match a.get(2) { Some(v) => println(f"g={v}"), None => println("g=none") }
    match a.get(9) { Some(v) => println(f"g9={v}"), None => println("g9=none") }
    println(f"c20={a.contains(20)} c99={a.contains(99)} len={a.len()} e={a.is_empty()}");
    let fa: Array[f64, 3] = [1.5, 2.5, 3.5];
    match fa.get(1) { Some(v) => println(f"f={v}"), None => println("f=none") }
    let ba: Array[bool, 2] = [true, false];
    println(f"bc={ba.contains(false)}");
    let a3: Array[i64, 3] = [10, 20, 30];
    println(f"ref={sum_ref(a3)}");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out,
            "g=30\ng9=none\nc20=true c99=false len=4 e=false\nf=2.5\nbc=true\nref=140\n"
        );
    }
}

#[test]
fn test_e2e_array_for_loop() {
    let out = run_program(
        r#"
fn main() {
    let a = [10, 20, 30, 40];
    let mut sum = 0;
    for x in a {
        sum = sum + x;
    }
    println(sum);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "100");
    }
}

#[test]
fn test_e2e_array_basics_example() {
    let src = include_str!("../../examples/array_basics.kara");
    let out = run_program(src);
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        // sum([10,40,20,30]) = 100
        // max([10,40,20,30]) = 40
        // scores printed: 100, 85, 92, 78
        // sum(scores) = 355
        assert_eq!(
            lines,
            vec!["100", "40", "100", "85", "92", "78", "355"],
            "array_basics.kara output mismatch"
        );
    }
}

#[test]
fn test_ir_array_len_constant_fold() {
    // `Array[i64, 3]` annotation pins the fixed-array `len()` constant
    // fold (bare `[…]` is now a Vec, whose `len()` loads the len field —
    // see `test_ir_array_literal_construction`).
    let ir = ir_for("fn get_len() -> i64 { let a: Array[i64, 3] = [10, 20, 30]; a.len() }");
    assert!(
        ir.contains("ret i64 3"),
        "expected len() constant fold to 3, got:\n{}",
        ir
    );
}

#[test]
fn test_e2e_array_len() {
    let out = run_program(
        r#"
fn main() {
    let a = [10, 20, 30, 40, 50];
    println(a.len());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "5");
    }
}

#[test]
fn test_e2e_array_literal_as_vec_return_and_consume() {
    // Bare `[a, b, c]` defaults to `Vec[T]` (typechecker synthesis rule),
    // but codegen's `compile_array_literal` emits a fixed `[N x T]`
    // aggregate. The lowering pass now canonicalizes a Vec-typed array
    // literal to the `Vec[…]` prefix form (real heap Vec). Before that,
    // returning `[…]` as a `Vec[T]` failed LLVM verification ("Function
    // return type does not match … ret [3 x i64] … {ptr,i64,i64}") and a
    // typed-let `[…]` followed by `.push` SEGFAULTED (array bytes read as
    // a Vec header). This pins the return, call-arg, push, and bare-let
    // shapes.
    let out = run_program(
        r#"
fn mk() -> Vec[i64] { [1, 2, 3] }
fn take(v: Vec[i64]) -> i64 { v.len() }
fn main() {
    println(mk().len());
    println(take([4, 5]));
    let mut v: Vec[i64] = [7, 8, 9];
    v.push(10);
    println(v.len());
    let mut b = [100, 200];
    b.push(300);
    println(b.len());
    let r = mk();
    let mut s = 0;
    for x in r { s = s + x; }
    println(s);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "3\n2\n4\n3\n6");
    }
}

#[test]
fn test_e2e_array_annotation_stays_fixed_array() {
    // The lowering rewrite must NOT touch an `Array[T, N]`-annotated
    // literal — that stays a fixed-size array (recorded as `Type::Array`,
    // not `Vec`). Guards against the rewrite over-firing.
    let out = run_program(
        r#"
fn main() {
    let a: Array[i64, 3] = [11, 22, 33];
    println(a[0] + a[2]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "44");
    }
}

/// B-2026-09-14-25, the NO-ENUM half — the cell that showed the enum was
/// never the cause.
///
/// A plain `let a: Array[D, 2]` handed to `fn take(a: Array[D, 2])` printed
/// GARBAGE in its elements' `Drop` bodies on both compiled backends at exit
/// 0, while `--interp` was correct: the callee freed the element heap under
/// the callee-owns convention and the CALLER still ran the bodies at its own
/// scope exit, reading buffers that frame had already returned. Measured as
/// `body-7-u<L...` against `body-7-c24-aaaa...`.
///
/// Asserts the OUTPUT half — that the bodies observe LIVE data and run at
/// the same point on every surface. The memory half of this same shape is
/// the `byval:` cell of
/// `asan_drop_bearing_array_element_is_freed_once_through_a_consuming_arm`
/// in `tests/memory_sanitizer.rs`; keeping both is deliberate, because the
/// two ways this shape broke during the fix were a leak with correct output
/// and correct output with a leak, and neither fixture alone catches both.
#[test]
fn test_e2e_by_value_array_param_leaves_element_drop_bodies_readable() {
    assert_eq!(
            run_program(
                "struct D { id: i64, s: String }\n\
                 impl Drop for D { fn drop(mut ref self) { println(f\"body-{self.id}-{self.s}\"); } }\n\
                 fn take(a: Array[D, 2]) -> i64 { return a[0].id; }\n\
                 fn main() {\n\
                 \x20   let a: Array[D, 2] = [D { id: 7, s: f\"left-aaaaaaaaaaaaaaaa\" }, D { id: 107, s: f\"right-bbbbbbbbbbbbbbbb\" }];\n\
                 \x20   println(f\"got={take(a)}\");\n\
                 \x20   println(\"post\");\n\
                 }"
            )
            .as_deref(),
            Some("got=7\nbody-7-left-aaaaaaaaaaaaaaaa\nbody-107-right-bbbbbbbbbbbbbbbb\npost\n")
        );
}

/// B-2026-09-15-12 — the `Array[T, N]` and NAMED-STRUCT inners of a
/// `-> ref` method used in a value position, which B-2026-09-15-4
/// deliberately declined to widen to.
///
/// `h.peek()[0]` failed `karac build` with "Index operator applied to
/// non-array type" and `n.peek().a` with "cannot resolve field 'a' on
/// this receiver"; `--interp` printed the right answer for both, so these
/// were run-vs-build divergences as well as gaps.
///
/// WHY THE FIX IS NOT -15-4's, WIDENED. That row teaches the consumer to
/// LOAD the borrow's pointee, which is right for a tuple and a DOUBLE FREE
/// for an `Array[String, N]`: the loaded array becomes a second owner of
/// the element buffers and nothing retracts the original's drop
/// (`free(): double free detected in tcache 2`, measured on the widened
/// filter before it was reverted). So these consumers bind the borrow to
/// an anonymous ref-local and re-dispatch against it instead — the shape
/// `arrbound:` / `fldbound:` below have always compiled correctly. A view
/// has no ownership story to get wrong.
///
/// `arrorig:` is the cell that proves it stayed a view: it reads the
/// ORIGINAL field after three borrows of it, and a load-based fix that
/// freed through the copy would dangle here.
///
/// The row left four things unmeasured and each has a cell. `arrfree:` /
/// `fldfree:` are the FREE-FUNCTION spellings — correct before the fix
/// (`compile_call` has loaded a borrow-returning free-fn result since
/// B-2026-06-07-5), so this gap was method-only exactly as -15-4's was,
/// and they are the oracle the method arm was made to match. `vec:` is the
/// `-> ref Vec[T]` inner, which the row guessed might already be covered
/// because `try_compile_ref_return_receiver_method` handles a `ref Vec`
/// RECEIVER — it was not: that is `h.peek().len()`, a method ON the
/// borrow, while an INDEX of it took the same fall-through and died on the
/// same message. `num:` is the heap-free `Array[i64, N]` control, which
/// compiled even before the fix and pins the defect to the element
/// buffers rather than to array addressing.
///
/// The two inners were NOT the same defect, which the row also asked. The
/// named-struct half was pure routing — binding the borrow records
/// `var_type_names` for it, which is the record whose absence produced
/// that diagnostic. The `Array` half needed that AND a second fix: with
/// only the routing it compiled and still aborted, because
/// `expr_yields_fresh_owned_temp` classified the borrow-returning ACCESSOR
/// as a fresh owned temp and the argument chokepoint freed the element it
/// read out of a borrowed array. Three comments in the tree had justified
/// that predicate's free-function-only screen by pointing at the upstream
/// `user_ref_method_names` gate in `compile_method_call`; routing these
/// consumers retires that gate, so the screen had to be paid for directly.
#[test]
fn test_e2e_ref_array_and_struct_return_used_in_a_value_position() {
    assert_eq!(
        run_program(
            "struct Pair { a: String, b: String }\n\
                 struct Hold { arr: Array[String, 2] }\n\
                 struct Nums { arr: Array[i64, 2] }\n\
                 struct Named { p: Pair }\n\
                 struct Seq { v: Vec[String] }\n\
                 impl Hold { fn peek(ref self) -> ref Array[String, 2] { return self.arr; } }\n\
                 impl Nums { fn peek(ref self) -> ref Array[i64, 2] { return self.arr; } }\n\
                 impl Named { fn peek(ref self) -> ref Pair { return self.p; } }\n\
                 impl Seq { fn peek(ref self) -> ref Vec[String] { return self.v; } }\n\
                 fn peekh(h: ref Hold) -> ref Array[String, 2] { return h.arr; }\n\
                 fn peekn(n: ref Named) -> ref Pair { return n.p; }\n\
                 fn main() {\n\
                 \x20   let k = 40 + 2;\n\
                 \x20   let h = Hold { arr: [f\"arr-left-{k}\", f\"arr-right-{k}\"] };\n\
                 \x20   println(f\"arr0:{h.peek()[0]}\");\n\
                 \x20   println(f\"arr1:{h.peek()[1]}\");\n\
                 \x20   println(f\"arrfree:{peekh(h)[0]}\");\n\
                 \x20   let ab: ref Array[String, 2] = h.peek();\n\
                 \x20   println(f\"arrbound:{ab[1]}\");\n\
                 \x20   println(f\"arrorig:{h.arr[0]}\");\n\
                 \x20   let s = Nums { arr: [k, k + 1] };\n\
                 \x20   println(f\"num:{s.peek()[1]}\");\n\
                 \x20   let n = Named { p: Pair { a: f\"pair-a-{k}\", b: f\"pair-b-{k}\" } };\n\
                 \x20   println(f\"fld:{n.peek().a}\");\n\
                 \x20   println(f\"fldfree:{peekn(n).b}\");\n\
                 \x20   let pb: ref Pair = n.peek();\n\
                 \x20   println(f\"fldbound:{pb.a}\");\n\
                 \x20   let q = Seq { v: [f\"vec-zero-{k}\", f\"vec-one-{k}\"] };\n\
                 \x20   println(f\"vec:{q.peek()[1]}\");\n\
                 }"
        )
        .as_deref(),
        Some(
            "arr0:arr-left-42\narr1:arr-right-42\narrfree:arr-left-42\n\
                 arrbound:arr-right-42\narrorig:arr-left-42\nnum:43\n\
                 fld:pair-a-42\nfldfree:pair-b-42\nfldbound:pair-a-42\n\
                 vec:vec-one-42\n"
        )
    );
}

#[test]
fn test_state_struct_type_sizes_fixed_array_field_inline() {
    // Regression (coro frame heap overflow): a `let buf: Array[u8, 4096]`
    // local held live across a network-yield park must occupy a
    // `[4096 x i8]` INLINE slot in the coro frame's state struct — NOT
    // the 8-byte i64 default that `llvm_type_for_name("Array")` returns
    // when the size-bearing `TypeExpr` is dropped. With the bug, the
    // typechecker's `pattern_binding_types` recorded only the head name
    // `"Array"` (no element type, no length), the state struct sized the
    // field at i64 (8 bytes), and the post-resume write into the
    // 4096-byte buffer overflowed the frame and clobbered the adjacent
    // heap chunk (`corrupted size vs. prev_size` / `double free or
    // corruption` on glibc; ASAN heap-buffer-overflow on every OS). The
    // buffer is touched (`buf[0]`) after the park so it is live across
    // the suspend and lands in the captured-locals set.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver() {
                 let mut buf: Array[u8, 4096] = [0u8; 4096];
                 fetch();
                 buf[0] = 1u8;
                 let _ = buf[0];
             }",
    );
    let line = ir
        .lines()
        .find(|l| l.starts_with("%kara.state.driver = type {"))
        .unwrap_or_else(|| panic!("no state struct type def in IR:\n{ir}"));
    // The array slot must be the full inline `[4096 x i8]`, not the i64
    // default that under-sizes the coro frame.
    assert!(
        line.contains("[4096 x i8]"),
        "coro state struct must size the Array[u8, 4096] field as an \
             inline [4096 x i8] slot (got the i64 default — frame overflow); \
             line: {line}"
    );
}

#[test]
fn test_e2e_modbind_array_literal() {
    // Fixed-size `[a, b, c]` → `[N x i64]` constant. Indexed read
    // uses the global's pointer as the array-storage pointer
    // (collections.rs fast path).
    let output = run_program(
        "let ARR: Array[i64, 3] = [10, 20, 30];\n\
             fn main() {\n\
                 println(ARR[0]);\n\
                 println(ARR[1]);\n\
                 println(ARR[2]);\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "10\n20\n30\n");
}

#[test]
fn test_ir_modbind_array_emits_constant_array() {
    // `Array[i64, 3]` with literal init lowers to a `[3 x i64]`
    // constant aggregate global.
    let ir = ir_for(
        "let ARR: Array[i64, 3] = [1, 2, 3];\n\
             fn main() { println(ARR[0]); }",
    );
    assert!(
        ir.contains("@ARR = internal constant [3 x i64]"),
        "expected `@ARR = internal constant [3 x i64]` in IR, got:\n{}",
        ir
    );
}

/// `Array[T, N] == Array[T, N]` compares CONTENTS element-wise, and so do
/// an array FIELD and an array MAP KEY (B-2026-08-27-25).
///
/// The row is about the bare operator, which failed LOUDLY — codegen
/// reported "left operand has non-comparable type ArrayType" and blamed
/// "likely a typechecker gap", at an operand pair whose types match
/// exactly. Fixing it turned up two SILENT siblings behind the same
/// missing arm, and they are pinned here because a routing-only repair
/// leaves both standing: `emit_eq_fn_for_type_expr` had no `Array` arm at
/// all, so a struct FIELD and a Map KEY of array type fell to the byte-loop
/// fallback. That is accidentally right for a scalar element and wrong for
/// a heap one — `WrapS { a: Array[String, 2] }` compared two equal values
/// UNEQUAL under `karac build` while the interpreter said equal, and
/// `Map[Array[String, 2], _]` grew a second entry for one key.
///
/// The two `Map` rows are the eq/hash pair: a structural eq without a
/// structural hash puts equal keys in different buckets, where they never
/// meet to be compared, so a fix that adds only the comparator leaves
/// `km.len()` at 3.
///
/// Both NESTED rows are here for the mangler. `display_mangle_te` dropped
/// CONST generic args, so `Array[i64, 2]` and `Array[i64, 3]` produced the
/// same name fragment and the first-emitted comparator served both;
/// `n3 == m3` differs only in the LAST element of a 3-wide inner array, so
/// a body compiled for extent 2 answers it wrong.
#[test]
fn test_e2e_array_equality_compares_contents() {
    assert_eq!(
        run_program(
            r#"
#[derive(PartialEq)]
struct WrapS { a: Array[String, 2] }

fn main() {
    let x: Array[i64, 2] = Array[1, 2];
    let y: Array[i64, 2] = Array[1, 9];
    let z: Array[i64, 2] = Array[1, 2];
    println(f"{x == y}");
    println(f"{x == z}");
    println(f"{x != y}");

    let p: Array[String, 2] = Array["ab", "cd"];
    let q: Array[String, 2] = Array["ab", "cd"];
    let r: Array[String, 2] = Array["ab", "zz"];
    println(f"{p == q}");
    println(f"{p == r}");

    println(f"{WrapS { a: Array["ab", "cd"] } == WrapS { a: Array["ab", "cd"] }}");
    println(f"{WrapS { a: Array["ab", "cd"] } == WrapS { a: Array["ab", "zz"] }}");

    let n2: Array[Array[i64, 2], 2] = Array[Array[1, 2], Array[3, 4]];
    let m2: Array[Array[i64, 2], 2] = Array[Array[1, 2], Array[3, 5]];
    let k2: Array[Array[i64, 2], 2] = Array[Array[1, 2], Array[3, 4]];
    println(f"{n2 == m2}");
    println(f"{n2 == k2}");

    let n3: Array[Array[i64, 3], 2] = Array[Array[1, 2, 3], Array[4, 5, 6]];
    let m3: Array[Array[i64, 3], 2] = Array[Array[1, 2, 3], Array[4, 5, 7]];
    println(f"{n3 == m3}");

    let three: Array[i64, 3] = Array[1, 2, 3];
    let three2: Array[i64, 3] = Array[1, 2, 9];
    println(f"{three == three2}");

    let e1: Array[i64, 0] = Array[];
    let e2: Array[i64, 0] = Array[];
    println(f"{e1 == e2}");

    let mut km: Map[Array[String, 2], i64] = Map.new();
    km.insert(Array["ab", "cd"], 1);
    km.insert(Array["ab", "cd"], 2);
    km.insert(Array["ab", "zz"], 3);
    println(f"{km.len()}");
    let mut ki: Map[Array[i64, 2], i64] = Map.new();
    ki.insert(Array[1, 2], 1);
    ki.insert(Array[1, 2], 2);
    println(f"{ki.len()}");
}
"#,
        ),
        // x==y, x==z, x!=y, p==q, p==r, field equal, field differing,
        // nested differing, nested equal, nested-3 differing (mangler),
        // three-wide differing, two empty arrays, Map[Array[String,2]] len,
        // Map[Array[i64,2]] len.
        Some(
            "false\ntrue\ntrue\ntrue\nfalse\ntrue\nfalse\nfalse\ntrue\n\
                 false\nfalse\ntrue\n2\n1\n"
                .to_string()
        )
    );
}

/// `Array[T, N]` ordering on the compiled backend (B-2026-08-27-42) — the
/// half the tuple sibling above left behind.
///
/// Same run-vs-build shape and same cause one level down:
/// `type_supports_ord` recurses through `Type::Array`, so `karac check`
/// accepted every line here, and then neither backend ran them. The
/// DIFFERENCE from the tuple is what had to be built. A tuple already had
/// its comparator (`emit_cmp_fn_for_type_expr`'s `TypeKind::Tuple` arm)
/// and needed only a dispatch line; an array had none, so this row needed
/// the emitter as well — which is why it outlived B-2026-08-27-33 by a day
/// rather than being folded into it.
///
/// It also fails from a different PLACE, which is why the dispatch is its
/// own block rather than a line inside the struct-operand one: an array
/// lowers to an LLVM `[N x T]`, not a struct, so it never entered that
/// block at all and hit "non-comparable type ArrayType" instead of the
/// tuple's "Unsupported struct binary op: Lt".
///
/// Rows chosen for what each pins:
///
///   * The `String` array is the heap element — the comparator must route
///     through the per-element `karac_cmp_String`, not compare the pointer
///     words. This is the array analogue of the defect B-2026-08-27-25
///     fixed for array EQUALITY.
///   * `[2, 0, 0]` vs `[1, 9, 9]` pins that the FIRST differing element
///     decides and the walk then stops: a comparator that folded all three
///     would answer the opposite way.
///   * The tuple-element array crosses the two comparators, so a
///     regression in either shows up here.
///   * `Vec[Array[i64, 2]].sort()` is the bonus the emitter buys. Unlike
///     the tuple sibling's sort row — which worked BEFORE its fix and is
///     there as a control — this one did NOT: with no comparator for an
///     array there was nothing for `sort()` to route to either, so the
///     operator and the sort were one gap and are fixed by one arm.
///
/// Twinned against the interpreter, whose `value_compare` has had an Array
/// arm since B-2026-06-30-15 — so that side needed only the dispatch, and
/// the two backends order through comparators written to the same rule.
#[test]
fn test_e2e_array_ordering_and_equality() {
    let src = r#"
fn main() {
    let a: Array[i64, 2] = Array[1, 2];
    let b: Array[i64, 2] = Array[1, 3];
    let c: Array[i64, 2] = Array[1, 2];
    println(f"{a < b}");
    println(f"{b < a}");
    println(f"{a < c}");
    println(f"{a <= c}");
    println(f"{a >= c}");
    println(f"{b > a}");
    println(f"{a > b}");
    println(f"{a == c}");
    println(f"{a != b}");

    let s: Array[String, 2] = Array["aa", "bb"];
    let t: Array[String, 2] = Array["aa", "bc"];
    println(f"{s < t}");
    println(f"{t < s}");
    println(f"{s <= s}");

    let p: Array[i64, 3] = Array[2, 0, 0];
    let q: Array[i64, 3] = Array[1, 9, 9];
    println(f"{p < q}");
    println(f"{q < p}");

    let u: Array[char, 2] = Array['a', 'b'];
    let w: Array[char, 2] = Array['a', 'c'];
    println(f"{u < w}");

    let ta: Array[(i64, i64), 2] = Array[(1, 2), (3, 4)];
    let tb: Array[(i64, i64), 2] = Array[(1, 2), (3, 5)];
    println(f"{ta < tb}");
    println(f"{tb < ta}");

    let mut v: Vec[Array[i64, 2]] = Vec.new();
    v.push(Array[2, 1]);
    v.push(Array[1, 9]);
    v.push(Array[1, 2]);
    v.sort();
    let mut i = 0;
    while i < v.len() {
        let e = ref v[i];
        println(f"{e[0]},{e[1]}");
        i = i + 1;
    }
}
"#;
    let expected = "true\nfalse\nfalse\ntrue\ntrue\ntrue\nfalse\ntrue\ntrue\n\
                        true\nfalse\ntrue\n\
                        false\ntrue\n\
                        true\n\
                        true\nfalse\n\
                        1,2\n1,9\n2,1\n";
    assert_eq!(run_program(src), Some(expected.to_string()));
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

/// B-2026-08-18-13 — a method declared by `impl Trait for Array[i64, N]`,
/// end to end. The impl block always checked; the CALL SITE reported "no
/// method 'head_or' on type 'Array'", so the declaration was accepted in
/// full and then unusable on every backend.
///
/// Run rather than merely checked because the INTERPRETER is the leg that
/// nearly shipped broken. A fixed `Array[T, N]` and a `Vec[T]` are both
/// `Value::Array` at runtime and `value_type_name` reports "Vec" for
/// either, so an impl registered under "Array" was unreachable there —
/// "method not found on type 'Vec' (no interpreter dispatch arm)" on a
/// program `karac check` and `karac build` both accepted. That
/// run-vs-build split is what got the `Slice` version of this row reverted
/// twice (B-2026-08-13-7), so `run_program` (which agrees the interpreter,
/// the JIT and the AOT binary) is the assertion that matters.
#[test]
fn a_fixed_array_impl_method_runs_on_every_backend() {
    for (label, self_mode, body, want) in [
        ("owned self", "self", "return self[0];", "1\n"),
        ("borrowed self", "ref self", "return self[1];", "2\n"),
    ] {
        let src = format!(
            "trait Head {{ fn pick({self_mode}) -> i64; }}\n\
                 impl Head for Array[i64, 3] {{ fn pick({self_mode}) -> i64 {{ {body} }} }}\n\
                 fn main() {{\n\
                     let a: Array[i64, 3] = [1, 2, 3];\n\
                     println(a.pick().to_string());\n\
                 }}\n"
        );
        let Some(out) = run_program(&src) else {
            return;
        };
        assert_eq!(out, want, "{label}");
    }
}

/// The Array head alongside a `Vec` head declaring the SAME method name.
/// Both are legal — the two receivers are distinct types — and each must
/// reach its own impl. This is the case the interpreter cannot decide for
/// itself (one `Value::Array` for both), so it is decided at check and
/// recorded per call site; without that recording the interpreter would
/// answer both receivers with whichever head it tried first.
#[test]
fn an_array_impl_and_a_vec_impl_of_one_method_stay_apart() {
    let src = "trait Head { fn tag(self) -> String; }\n\
                   impl Head for Array[i64, 3] { fn tag(self) -> String { return \"ARR\"; } }\n\
                   impl Head for Vec[i64] { fn tag(self) -> String { return \"VEC\"; } }\n\
                   fn main() {\n\
                       let a: Array[i64, 3] = [1, 2, 3];\n\
                       let v: Vec[i64] = [1, 2];\n\
                       println(a.tag());\n\
                       println(v.tag());\n\
                   }\n";
    let Some(out) = run_program(src) else {
        return;
    };
    assert_eq!(out, "ARR\nVEC\n", "each head must answer its own receiver");
}

/// B-2026-08-21-25 — a method call whose receiver is a fixed-array
/// TEMPORARY rather than a named binding.
///
/// The fixed-array dispatch block is identifier-keyed — it reads
/// `variables[name].ty` and fires only when that slot is an LLVM
/// `ArrayType` — so a temporary had no slot to key on and every method of
/// the read surface hit the dispatch-fail error under `karac build` while
/// `--interp`, which runs a fixed array as a Vec, answered.
///
/// Each spelling is pinned BESIDE its bound two-line twin, which is the
/// oracle the fix is written against and the thing that keeps this test
/// honest: the fix routes the temporary onto the binding's own path, so a
/// regression that broke only that path would otherwise still pass with a
/// smaller number of lines in the output.
///
/// Three receiver shapes are swept because the element type behind them is
/// resolved three different ways, and only `is_sorted` consumes it —
/// `to_ne_bytes` (fixed `u8`), a user fn declared `-> Array[T, N]`
/// (signature lookup), and a byte-string literal (fixed `u8`). Get one
/// wrong and `is_sorted` is the single method that notices.
///
/// `hi()` returns `[128, 1]` for exactly that reason: unsigned it is NOT
/// sorted, and read as `i8` (128 → -128) it WOULD be. Asserting `false`
/// is what proves the element type reached the comparator rather than the
/// arm defaulting to a signed compare. It is a declared `Array[u8, 2]`
/// rather than a `to_ne_bytes` result so the assertion does not also
/// depend on the host's byte order.
///
/// `.last()` earns its own line: it is the one method of this surface the
/// dispatch tail could not see, because the iterator-chain terminal arm
/// admits ANY `MethodCall` receiver on the assumption that it is a chain
/// and returned its own error before the tail was reached.
#[test]
fn test_e2e_method_call_on_a_fixed_array_temporary() {
    let src = r#"
trait Head { fn head_or(self, d: i64) -> i64; }
impl Head for Array[i64, 3] {
    fn head_or(self, d: i64) -> i64 { return 111; }
}
fn mk() -> Array[i64, 3] { return [4, 5, 6]; }
fn hi() -> Array[u8, 2] { return [128u8, 1u8]; }

fn main() {
    let e: u16 = 258u16;

    // The whole read surface on a TEMPORARY, each beside its bound twin.
    let b = e.to_ne_bytes();
    println(e.to_ne_bytes().len());
    println(b.len());
    println(e.to_ne_bytes().is_empty());
    println(b.is_empty());
    println(e.to_ne_bytes().first().unwrap());
    println(b.first().unwrap());
    println(e.to_ne_bytes().last().unwrap());
    println(b.last().unwrap());
    println(e.to_ne_bytes().get(0).unwrap());
    println(b.get(0).unwrap());
    println(e.to_ne_bytes().contains(1u8));
    println(b.contains(1u8));

    // A user fn declared `-> Array[T, N]`: the element type comes from the
    // signature, and `[128, 1]` is unsorted only if it is read as `u8`.
    println(mk().len());
    println(mk().last().unwrap());
    println(hi().is_sorted());
    let h = hi();
    println(h.is_sorted());

    // A byte-string literal receiver.
    println(b"abc".len());
    println(b"abc".is_sorted());
    println(b"cba".is_sorted());

    // A method a USER impl declared on the `Array` head, called on a
    // temporary — admitted for the same reason the builtin names are: this
    // routes the call to the path its bound twin already takes.
    println(mk().head_or(-1));
    let m = mk();
    println(m.head_or(-1));
}
"#;
    assert_eq!(
            run_program(src).as_deref(),
            Some(
                "2\n2\nfalse\nfalse\n2\n2\n1\n1\n2\n2\ntrue\ntrue\n3\n6\nfalse\nfalse\n3\ntrue\nfalse\n111\n111\n"
            )
        );
}

/// B-2026-08-21-41 — a fixed-array TEMPORARY as a for-loop source.
///
/// `for_receiver_is_indexable` required the (`.iter()`-peeled) source to be
/// an `Identifier` or a `FieldAccess`, so an array-valued temporary was
/// claimed by no arm, reached the `for` dispatch catch-all, and ran ZERO
/// times — with no diagnostic and exit 0, against the interpreter's N. It
/// is the silent sibling of B-2026-08-21-25, which was the same receiver
/// shape failing LOUDLY at method dispatch.
///
/// Every temporary spelling is pinned beside its bound twin, because the
/// fix routes the temporary onto the binding's own loop — if that path
/// regressed, a test that only covered the temporary would go green while
/// both were wrong.
///
/// Four things here are deliberate:
///
///   * the `enumerate` cases weight by `i`, so a lowering that bound the
///     element but left the index at zero still fails. `compile_for_array_var`
///     carries the storage index as its induction variable and the peel
///     relies on exactly that, so the index is the part worth doubting.
///   * `fs()` has a FLOAT element, so the loop cannot be assuming an
///     integer array — the scalar gate admits `Int` and `Float` alike.
///   * `.iter().count()` is included because it is not a for-loop at all,
///     yet it read 0 pre-fix: the eager terminal desugars through this same
///     lowering, which is what makes this one root cause rather than two.
///   * the `break` case proves the loop frame is intact on an early exit,
///     the shape most likely to break if the materialized slot were ever
///     given a scope-exit action.
#[test]
fn test_e2e_for_loop_over_a_fixed_array_temporary() {
    let src = r#"
fn mk() -> Array[i64, 3] { return [10, 20, 30]; }
fn fs() -> Array[f64, 2] { return [1.5, 2.25]; }

fn main() {
    let mut a = 0;
    for x in mk() { a = a + x; }
    println(a);
    let m = mk();
    let mut b = 0;
    for x in m { b = b + x; }
    println(b);

    let mut c = 0;
    for x in mk().iter() { c = c + x; }
    println(c);
    let mut d = 0;
    for x in mk().into_iter() { d = d + x; }
    println(d);

    let mut e = 0;
    for (i, x) in mk().iter().enumerate() { e = e + i * x; }
    println(e);
    let mut f = 0;
    for (i, x) in m.iter().enumerate() { f = f + i * x; }
    println(f);

    let u: u16 = 258u16;
    let mut g = 0;
    for x in u.to_ne_bytes() { g = g + (x as i64); }
    println(g);
    let mut h = 0;
    for x in b"abc" { h = h + (x as i64); }
    println(h);

    let mut fl = 0.0;
    for x in fs() { fl = fl + x; }
    println(fl);

    println(mk().iter().count());
    println(u.to_ne_bytes().iter().count());

    let mut k = 0;
    for x in mk() { if x > 15 { break; } k = k + x; }
    println(k);

    // An ARRAY `self` inside an impl body, owned and borrowed. The `self`
    // dispatch arm checked the five POINTER-backed container tables; a fixed
    // array is none of them — it is an `[N x T]` slot — so this matched
    // nothing and ran zero times too.
    let s: Array[i64, 3] = [1, 2, 3];
    println(s.total());
    println(s.doubled());
}

trait Sums { fn total(self) -> i64; fn doubled(ref self) -> i64; }
impl Sums for Array[i64, 3] {
    fn total(self) -> i64 { let mut n = 0; for x in self { n = n + x; } return n; }
    fn doubled(ref self) -> i64 { let mut n = 0; for x in self { n = n + x * 2; } return n; }
}
"#;
    assert_eq!(
        run_program(src).as_deref(),
        Some("60\n60\n60\n60\n80\n80\n3\n294\n3.75\n3\n2\n10\n6\n12\n")
    );
}

/// B-2026-09-13-16 — a callee that WRAPS its own by-value `Array` param in
/// an `Option` returned a DANGLING interior, and this pins the OUTPUT
/// because wrong output was the observable.
///
/// `return a;` over an owned by-value `Array[T, N]` param already retracted
/// the callee's scope-exit element drop (B-2026-08-24-5) — which is why
/// `fn passthru(x: Array[String, 2]) -> Array[String, 2]` was always clean.
/// `return Some(a);` is the identical transfer, but the disarm resolved a
/// root of `Identifier` / `SelfValue` only, so a `Call` returned on its
/// first line and nothing was retracted. The callee then freed the `N`
/// buffers while the returned `Option` still carried their `{ptr,len,cap}`
/// triples, and the caller `memmove`d out of freed heap.
///
/// MEASURED before the fix, `KARAC_OPT_LEVEL=0`: garbage on all three
/// compiled surfaces and the right answer under `--interp`, with valgrind
/// reporting `Invalid read of size 2` in `memmove`, 33 errors from 2
/// contexts, on a block freed by the callee's own array drop. The three
/// compiled surfaces printed DIFFERENT garbage from each other, which is
/// the tell that this is freed heap being re-read rather than a consistent
/// miscompile.
///
/// IT LIVES HERE RATHER THAN IN `memory_sanitizer.rs` because the cell is
/// not yet leak-free: retracting the callee's drop hands the buffers to the
/// caller's arm-bound binding, whose owner is the one B-2026-09-13-2 left
/// standing, so 132 B remain — the same residual that row records for its
/// `passthru` sibling. The consuming controls, which CAN be asserted
/// leak-free, are `asan_consuming_array_params_keep_their_callee_drop`.
///
/// The `mk` cell is the in-program oracle: a callee that builds its own
/// interior was correct throughout, so a run where `w:` and `m:` disagree
/// is this defect and not a `Option[Array[..]]` problem in general.
#[test]
fn e2e_array_param_wrapped_into_an_option_return_survives_the_callee() {
    assert_eq!(
            run_program(
                "fn wrap(a: Array[String, 2]) -> Option[Array[String, 2]] { return Some(a); }\n\
                 fn mk(n: i64) -> Option[Array[String, 2]] { return Some([f\"row-aaaaaaaaaaaaaaaa-{n}\", f\"col-bbbbbbbbbbbbbbbb-{n}\"]); }\n\
                 fn main() {\n\
                 \x20\x20\x20\x20let mut j: i64 = 0;\n\
                 \x20\x20\x20\x20while j < 3 {\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20let e: Array[String, 2] = [f\"row-aaaaaaaaaaaaaaaa-{j}\", f\"col-bbbbbbbbbbbbbbbb-{j}\"];\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20let o = wrap(e);\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20match o { Some(a) => { println(f\"w:{a[0]}\"); } None => { println(\"none\"); } }\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20match mk(j) { Some(a) => { println(f\"m:{a[0]}\"); } None => { println(\"none\"); } }\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20j = j + 1;\n\
                 \x20\x20\x20\x20}\n\
                 \x20\x20\x20\x20println(\"end\");\n\
                 }\n"
            ),
            Some(
                "w:row-aaaaaaaaaaaaaaaa-0\n\
                 m:row-aaaaaaaaaaaaaaaa-0\n\
                 w:row-aaaaaaaaaaaaaaaa-1\n\
                 m:row-aaaaaaaaaaaaaaaa-1\n\
                 w:row-aaaaaaaaaaaaaaaa-2\n\
                 m:row-aaaaaaaaaaaaaaaa-2\n\
                 end\n"
                    .to_string()
            )
        );
}

/// B-2026-09-10-27 — a boxed `Array` payload's element `Drop` bodies ran on
/// NO backend, the `Array` peer of the tuple gap a56142bd8 closed.
///
/// `Option[Array[R, 2]]` printed `s:1 / end` under `--interp`, the JIT and
/// both AOT lanes. An AGREED SILENCE, so no A/B rule was broken — which is
/// why both halves had to land in ONE commit, and the pin in
/// `e2e_boxed_array_payload_reads_back_on_every_surface`'s cell 6 is what
/// enforced that. It worked exactly as designed: with only codegen's arm
/// wired, that cell went to `s:1 / dR1 / dR2 / end` compiled against
/// `s:1 / end` interpreted, and the loud failure said the interpreter had to
/// move too.
///
/// THE CODEGEN ARM IS NOT NEW HERE — b55b5e8 (B-2026-09-12-6) wrote it, and
/// then deliberately GATED IT OFF for the seeded `Option`/`Result` head.
/// That row hit this exact wall: the arm lives in a core shared by both
/// heads, so enabling it reached the seeded pair too, where the
/// interpreter's payload walk had no `Value::Array` case —
/// `Option[Array[R, 2]]` went from 0 bodies everywhere to 0 interpreted and
/// 2 compiled, and the two fixtures pinning that silence went red. Rather
/// than trade a both-silent bug for a run-vs-build divergence, it gated the
/// arm to the generic-enum head and left the seeded pair to THIS row, with
/// "its own interpreter half to write". This commit writes that half, so the
/// gate's reason is gone and it is removed; both heads now take the arm.
///
/// Nothing under either arm is new either:
/// `emit_array_elem_user_drop_bodies_fn` has emitted
/// `__karac_dropelems_array_<T>_<N>` since B-2026-08-28-57. The interpreter
/// declined for the mirror reason to codegen's: its registration gate asked
/// `type_expr_runs_user_drop` about the ARRAY rather than its element, and
/// `Array` is not a declared struct or enum.
///
/// THE INTERPRETER NEEDED TWO SEPARATE ARMS, because the two spellings reach
/// bodies by different routes — the asymmetry B-2026-09-09-20 recorded for
/// tuples. A NAMED local goes through the declared-type table
/// (`optres_payload_bodies_tes`); a FRESH-TEMP argument has no binding to key
/// on and goes through the value-driven arg path instead. Wiring only the
/// first left the row's own cell silent.
///
/// THE VALUE-DRIVEN ARM IS SCOPED TO THE OPTRES ENTRY rather than given to
/// `run_discarded_value_user_drops` beside its `Value::Tuple` sibling, and
/// that asymmetry is deliberate: a discarded BARE `Array` local already runs
/// its element bodies through its own registration (measured
/// `dR1 / dR2 / mid / end` on all four surfaces), so widening the general
/// walk would run them a SECOND time for that shape.
///
/// NOT CLOSED HERE, and measured rather than assumed: the QUALIFIED
/// constructor spelling at an argument position
/// (`plainD(Option[Array[R, 2]].Some(a))`) still runs no body compiled. That
/// is not this row's defect and not array-specific — it is pre-existing for
/// TUPLE and STRUCT payloads too, because the qualified form parses as a
/// METHOD CALL (B-2026-08-22-17) and `optres_arg_is_unowned_temp` rejects
/// `MethodCall` in its first arm as "a place rooted at a binding". Filed as
/// its own row; widening that predicate is explicitly unsafe on its own (its
/// doc records three shapes it would turn into double frees).
#[test]
fn e2e_boxed_array_payload_runs_its_element_drop_bodies() {
    const PRE: &str = "struct Ra { id: i64 }\n\
             impl Drop for Ra { fn drop(mut ref self) { println(f\"dRa{self.id}\") } }\n";
    for (label, body, want) in [
            // THE ROW: a named array local wrapped in a bare ctor at an
            // argument position.
            (
                "named-array-local-arg",
                "fn plainD(x: Option[Array[Ra, 2]]) { match x { Some(t) => { println(f\"s:{t[0].id}\") } None => { println(\"n\") } } }\n\
                 fn main() { let a: Array[Ra, 2] = [Ra { id: 1 }, Ra { id: 2 }]; plainD(Some(a)); println(\"end\") }\n",
                "s:1\ndRa1\ndRa2\nend\n",
            ),
            // An INLINE array literal, so the caller has no array local whose
            // own registration could be doing the work instead.
            (
                "inline-array-literal-arg",
                "fn plainD(x: Option[Array[Ra, 2]]) { match x { Some(t) => { println(f\"s:{t[0].id}\") } None => { println(\"n\") } } }\n\
                 fn main() { plainD(Some([Ra { id: 1 }, Ra { id: 2 }])); println(\"end\") }\n",
                "s:1\ndRa1\ndRa2\nend\n",
            ),
            // A NAMED `Option` local rather than a fresh temp — the route
            // asymmetry B-2026-09-09-20 recorded, and one of this row's own
            // NOT MEASURED items.
            (
                "named-option-local",
                "fn plainD(x: Option[Array[Ra, 2]]) { match x { Some(t) => { println(f\"s:{t[0].id}\") } None => { println(\"n\") } } }\n\
                 fn main() { let a: Array[Ra, 2] = [Ra { id: 1 }, Ra { id: 2 }]; let o: Option[Array[Ra, 2]] = Some(a); plainD(o); println(\"end\") }\n",
                "s:1\ndRa1\ndRa2\nend\n",
            ),
            // A DISCARDED `Option` local, never passed anywhere: the bodies are
            // due at its own `let`, which is why they precede `mid`.
            (
                "discarded-option-local",
                "fn main() { let a: Array[Ra, 2] = [Ra { id: 1 }, Ra { id: 2 }]; let o: Option[Array[Ra, 2]] = Some(a); println(\"mid\"); println(\"end\") }\n",
                "dRa1\ndRa2\nmid\nend\n",
            ),
            // The `Result` spelling, another NOT MEASURED item. Built through an
            // annotated local because a bare `[..]` literal inside a qualified
            // `Result` constructor infers as `Vec`, not `Array` — an inference
            // gap unrelated to this row.
            (
                "result-ok-array-payload",
                "fn plainD(x: Result[Array[Ra, 2], i64]) { match x { Ok(t) => { println(f\"s:{t[0].id}\") } Err(e) => { println(\"n\") } } }\n\
                 fn main() { let a: Array[Ra, 2] = [Ra { id: 1 }, Ra { id: 2 }]; let o: Result[Array[Ra, 2], i64] = Result[Array[Ra, 2], i64].Ok(a); plainD(o); println(\"end\") }\n",
                "s:1\ndRa1\ndRa2\nend\n",
            ),
            // N = 3, so a walk that happened to be arity-2 shows up here.
            (
                "arity-three",
                "fn plainD(x: Option[Array[Ra, 3]]) { match x { Some(t) => { println(f\"s:{t[0].id}\") } None => { println(\"n\") } } }\n\
                 fn main() { plainD(Some([Ra { id: 1 }, Ra { id: 2 }, Ra { id: 3 }])); println(\"end\") }\n",
                "s:1\ndRa1\ndRa2\ndRa3\nend\n",
            ),
            // An element that owns HEAP as well as carrying a `Drop`, so the
            // memory and bodies channels are both live at once — the third of
            // this row's NOT MEASURED items. One body each, no double free.
            (
                "element-owns-heap-and-drop",
                "struct Rh { id: i64, name: String }\n\
                 impl Drop for Rh { fn drop(mut ref self) { println(f\"dRh{self.id}\") } }\n\
                 fn plainD(x: Option[Array[Rh, 2]]) { match x { Some(t) => { println(f\"s:{t[0].id}\") } None => { println(\"n\") } } }\n\
                 fn main() { plainD(Some([Rh { id: 1, name: f\"a\" }, Rh { id: 2, name: f\"b\" }])); println(\"end\") }\n",
                "s:1\ndRh1\ndRh2\nend\n",
            ),
            // CONTROL — an element with NO `Drop` at all must stay silent, so a
            // walk that fired on array-ness rather than on the element's own
            // classification shows up here.
            (
                "drop-free-element-control",
                "struct Pa { id: i64 }\n\
                 fn plainD(x: Option[Array[Pa, 2]]) { match x { Some(t) => { println(f\"s:{t[0].id}\") } None => { println(\"n\") } } }\n\
                 fn main() { plainD(Some([Pa { id: 1 }, Pa { id: 2 }])); println(\"end\") }\n",
                "s:1\nend\n",
            ),
            // CONTROL — a discarded BARE array local, which already ran its
            // bodies through its own registration before this change. Exactly
            // ONE pair: this is the cell that fails if the value-driven arm is
            // ever widened into `run_discarded_value_user_drops`.
            (
                "bare-array-local-not-doubled-control",
                "fn main() { let a: Array[Ra, 2] = [Ra { id: 1 }, Ra { id: 2 }]; println(\"mid\"); println(\"end\") }\n",
                "dRa1\ndRa2\nmid\nend\n",
            ),
            // CONTROL — the TUPLE payload a56142bd8 fixed, unchanged by this.
            (
                "tuple-payload-control",
                "fn plainD(x: Option[(Ra, Ra)]) { match x { Some(t) => { println(\"s\") } None => { println(\"n\") } } }\n\
                 fn main() { plainD(Some((Ra { id: 1 }, Ra { id: 2 }))); println(\"end\") }\n",
                "s\ndRa1\ndRa2\nend\n",
            ),
            // BOUNDARY — a BARE array as a fresh-temp argument (no envelope)
            // runs no element body on any backend. An agreed silence, outside
            // this row, and pinned so a later change has to move both backends.
            (
                "boundary-bare-array-arg-stays-silent",
                "fn takeA(x: Array[Ra, 2]) { println(f\"t:{x[0].id}\") }\n\
                 fn main() { takeA([Ra { id: 1 }, Ra { id: 2 }]); println(\"end\") }\n",
                "t:1\nend\n",
            ),
            // B-2026-09-12-24 — the MONOMORPHIC user enum at an `Array`
            // payload. Every cell above wraps the array in `Option` / `Result`
            // or a generic, which is what let this one sit silent: the
            // monomorphic walker (`emit_user_enum_payload_bodies`) admits a
            // variant field only when its head is in `struct_types`, and
            // `Array` is not — so no walker was emitted at all, on either
            // backend. Measured before the fix, this cell printed `end` alone.
            (
                "mono-enum-array-payload",
                "enum Bin { Packed(Array[Ra, 2]), Bare }\n\
                 fn main() { let b: Bin = Bin.Packed([Ra { id: 1 }, Ra { id: 2 }]); println(\"end\") }\n",
                "dRa1\ndRa2\nend\n",
            ),
            // The same cell MATCHED OUT, which is the double-body shape: the
            // arm binding takes the array, so the enum's walk must not fire on
            // top of it. `dRa1 dRa2` once, not twice.
            (
                "mono-enum-array-payload-matched-out",
                "enum Bin { Packed(Array[Ra, 2]), Bare }\n\
                 fn main() { let b: Bin = Packed([Ra { id: 1 }, Ra { id: 2 }]);\n\
                 \x20   match b { Packed(a) => { println(f\"s:{a[0].id}\") } Bare => { println(\"n\") } }\n\
                 \x20   println(\"end\") }\n",
                "s:1\ndRa1\ndRa2\nend\n",
            ),
            // The unit variant of the same enum — the walker\'s switch must fall
            // through to its exit rather than read an absent payload.
            (
                "mono-enum-array-payload-unit-variant",
                "enum Bin { Packed(Array[Ra, 2]), Bare }\n\
                 fn main() { let b: Bin = Bare; println(\"end\") }\n",
                "end\n",
            ),
            // B-2026-09-10-20 — REPINNED, and this cell is why it was pinned.
            // It read `end` alone and its note asked that "whoever writes that
            // arm has to move both backends at once rather than half of it".
            // That arm is written: the name-keyed payload-bodies head now
            // carries a `Vec` field row beside its `Array` one, the interpreter
            // carries the matching DECLARED-head arm, and this program prints
            // `dRa1 dRa2 end` on `--interp`, the JIT, `-O0` and `-O2` auto-par
            // alike (valgrind at `-O0`: 0 errors, nothing lost). The cell keeps
            // its name and its place so the boundary it guards is still
            // legible — what moved is which side of it this shape sits on.
            (
                "boundary-mono-enum-vec-payload-stays-silent",
                "enum Vbin { V(Vec[Ra]), Z }\n\
                 fn main() { let v: Vbin = Vbin.V(Vec[Ra { id: 1 }, Ra { id: 2 }]); println(\"end\") }\n",
                "dRa1\ndRa2\nend\n",
            ),
        ] {
            let Some(out) = run_program(&format!("{PRE}{body}")) else {
                return;
            };
            assert_eq!(out, want, "[{label}]");
        }
}

/// B-2026-09-09-9 — a nested indexed read whose OUTER is an `Array[T, N]`
/// was rejected by codegen on every compiled backend while `--interp` ran
/// the program: `error: codegen: nested indexed read on 'a' — element
/// TypeExpr unknown (outer is not a tracked Vec/Slice/Array variable)`, a
/// message that names Array as tracked while Array was the one hole.
///
/// TWO registrations were missing, and only the first is what the row
/// suspected:
///
///   * `compile_nested_index_read` read only `var_elem_type_exprs`, and an
///     `Array` records its element in `array_elem_type_exprs` instead. The
///     indexed-RECEIVER path took exactly this fallback in B-2026-08-11-1;
///     the nested-read path, which shares the diagnostic text, never did.
///     That alone accounts for cells 4, 5 and 6 — a plain annotated `let`,
///     an `Array` fn param and an array of arrays — which is why the base
///     is NOT the discriminator: nothing about a `match` arm was required
///     to hit this, and `a[i][j]` over an array was unreachable everywhere.
///
///   * a `match`-arm payload binding registered no element type at all,
///     because `bind_pattern_values` had no `"Array"` arm beside its
///     `Vec` / `Slice` ones. Cells 1–3.
///
/// The arm registration then needed `array_inner_type_expr` widened: an
/// array has two `TypeExpr` spellings — the parser's `Path(["Array"], ..)`
/// for a written annotation, and the structural `TypeKind::Array` node for
/// anything the TYPECHECKER inferred — and the resolver knew only the
/// written one. A payload binding's type always comes from the typechecker,
/// so the arm peeled `None` and registered nothing until both spellings
/// answered.
///
/// Cells 7 and 8 are the hazards. A single index on an arm-bound array and
/// a `Vector[T, N]` payload both already worked, and the new arm keys on
/// `"Array"` alone so the vector — whose surface name is recorded by the
/// same typechecker site — must stay out of it.
///
/// NOT covered here, deliberately: the REBIND spelling (`let b = a;` /
/// `Some(t) => { let u = t; u[i][j] }`) still refuses. Carrying the element
/// type across a bare rebind makes it compile, and what it then compiles to
/// is a double free at `-O0` — `Array[Vec[T], N]` rebinding duplicates the
/// element owners, which is live on `main` today for a rebind with no index
/// in it at all. Turning a loud refusal into silent corruption is a worse
/// trade than the refusal, so that spelling waits on the ownership row.
#[test]
fn e2e_nested_indexed_read_reaches_an_array_outer() {
    for (label, src, want) in [
            // 1 — the row's own shape: an `Array[Vec[String], N]` payload bound
            //     out of an `Option` arm, read two levels deep.
            (
                "arm-array-vec-string",
                "fn plainV(x: Option[Array[Vec[String], 2]]) {\n\
                 \x20   match x { Some(t) => { println(f\"s:{t[0][0]}\") } None => { println(\"n\") } }\n\
                 }\n\
                 fn main() {\n\
                 \x20   let a: Array[Vec[String], 2] = [[f\"aaaaaaaa0\", f\"aaaaaaaa1\"], [f\"bbbbbbbb0\"]];\n\
                 \x20   plainV(Some(a));\n\
                 }\n",
                "s:aaaaaaaa0\n",
            ),
            // 2 — the same arm with a SCALAR element, so the gap is not about
            //     the element being heap-bearing.
            (
                "arm-array-vec-i64",
                "fn plainV(x: Option[Array[Vec[i64], 2]]) {\n\
                 \x20   match x { Some(t) => { println(f\"s:{t[0][1]}\") } None => { println(\"n\") } }\n\
                 }\n\
                 fn main() {\n\
                 \x20   let a: Array[Vec[i64], 2] = [[10, 11], [20]];\n\
                 \x20   plainV(Some(a));\n\
                 }\n",
                "s:11\n",
            ),
            // 3 — a USER enum payload rather than `Option`, which reaches the
            //     same binding site by a different variant path.
            (
                "user-enum-array-payload",
                "enum E { A(Array[Vec[String], 2]), B }\n\
                 fn plainE(x: E) {\n\
                 \x20   match x { E.A(t) => { println(f\"s:{t[1][0]}\") } E.B => { println(\"n\") } }\n\
                 }\n\
                 fn main() {\n\
                 \x20   let a: Array[Vec[String], 2] = [[f\"aaaaaaaa0\"], [f\"bbbbbbbb0\", f\"bbbbbbbb1\"]];\n\
                 \x20   plainE(E.A(a));\n\
                 }\n",
                "s:bbbbbbbb0\n",
            ),
            // 4 — no arm anywhere. A plain annotated `let` failed identically,
            //     which is what refutes the row's "the base is the variable".
            (
                "annotated-let-base",
                "fn main() {\n\
                 \x20   let a: Array[Vec[String], 2] = [[f\"aaaaaaaa0\", f\"aaaaaaaa1\"], [f\"bbbbbbbb0\"]];\n\
                 \x20   println(f\"s:{a[0][0]}\");\n\
                 }\n",
                "s:aaaaaaaa0\n",
            ),
            // 5 — an `Array` FN PARAM, the third base with the same miss.
            (
                "fn-param-base",
                "fn takes(a: Array[Vec[String], 2]) { println(f\"s:{a[0][0]}\"); }\n\
                 fn main() {\n\
                 \x20   let a: Array[Vec[String], 2] = [[f\"aaaaaaaa0\", f\"aaaaaaaa1\"], [f\"bbbbbbbb0\"]];\n\
                 \x20   takes(a);\n\
                 }\n",
                "s:aaaaaaaa0\n",
            ),
            // 6 — both levels are arrays, so the synth minted for the inner
            //     element is itself registered from the array table.
            (
                "array-of-array",
                "fn main() {\n\
                 \x20   let a: Array[Array[i64, 2], 2] = [[10, 11], [20, 21]];\n\
                 \x20   println(f\"s:{a[1][0]}\");\n\
                 }\n",
                "s:20\n",
            ),
            // 7 — HAZARD: a SINGLE index on an arm-bound array already worked
            //     (it never reaches the nested-read path), so the new
            //     registration must leave it exactly as it was.
            (
                "single-index-arm-control",
                "fn plainV(x: Option[Array[i64, 3]]) {\n\
                 \x20   match x { Some(t) => { println(f\"s:{t[2]}\") } None => { println(\"n\") } }\n\
                 }\n\
                 fn main() { plainV(Some([7, 8, 9])); }\n",
                "s:9\n",
            ),
            // 8 — HAZARD: `Vector[T, N]` records its surface name at the same
            //     typechecker site as `Array` and shares the width path, but it
            //     is NOT an array and must not enter the array table.
            (
                "vector-payload-control",
                "enum E { V(Vector[i64, 4]), N }\n\
                 fn plainE(x: E) {\n\
                 \x20   match x { E.V(v) => { println(f\"s:{v}\") } E.N => { println(\"n\") } }\n\
                 }\n\
                 fn main() {\n\
                 \x20   let v: Vector[i64, 4] = Vector[i64, 4](1, 2, 3, 4);\n\
                 \x20   plainE(E.V(v));\n\
                 }\n",
                "s:Vector(1, 2, 3, 4)\n",
            ),
        ] {
            let Some(out) = run_program(src) else {
                return;
            };
            assert_eq!(out, want, "[{label}]");
        }
}

/// B-2026-09-09-23 — rebinding an `Array[T, N]` whose element owns heap
/// gave the destination a SECOND memory drop over elements the source
/// still owns: `let a: Array[String, 2] = [..]; let b: Array[String, 2] =
/// a;` with nothing else in the program aborted `free(): double free
/// detected in tcache 2` under the JIT and at `KARAC_OPT_LEVEL=0`, while
/// `--interp` was correct.
///
/// THE ANNOTATION IS THE WHOLE DISCRIMINATOR, and that is what names the
/// bug. B-2026-08-28-57's rule is already written down beside this code —
/// "Bodies follow the move; memory does not" — and it cites this exact
/// double free as the thing it is avoiding. But it was enforced only by
/// accident of resolution: a BARE rebind (`let b = a;`) has no annotation
/// and no `array_elem_type_exprs` entry for the destination, so the
/// element type came back `None` and the memory registration was skipped.
/// An ANNOTATED rebind resolves the element type from the annotation and
/// walked straight past the rule into `make_array_param_callee_owned`.
///
/// So the guard now keys on the RHS SHAPE rather than on where the element
/// type was resolved: a rebind of a live array memory owner
/// (`owned_array_params`) does not take memory ownership, whichever way
/// its element type was found.
///
/// `-O2` was clean throughout, which is the reason this sat unnoticed: the
/// optimizer deletes buffers nothing observes, so the default `karac build`
/// passed and only the JIT — the first thing anyone runs — and an explicit
/// `-O0` aborted.
///
/// Cells 6-9 are the shapes that were already correct: every bare rebind,
/// a scalar element (nothing to double-free), and the `Vec` container,
/// whose move disarms the source's cap and so was never affected.
#[test]
fn e2e_array_rebind_leaves_memory_with_one_owner() {
    for (label, src, want) in [
        // 1 — the minimal reproducer: no index, no read, no call.
        (
            "annotated-rebind-vec-element",
            "fn main() {\n\
                 \x20   let a: Array[Vec[i64], 2] = [[10, 11], [20]];\n\
                 \x20   let b: Array[Vec[i64], 2] = a;\n\
                 \x20   println(\"done\");\n\
                 }\n",
            "done\n",
        ),
        // 2 — a `String` element, so the class is "element owns heap" and
        //     not anything specific to a nested `Vec`.
        (
            "annotated-rebind-string-element",
            "fn main() {\n\
                 \x20   let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb1\"];\n\
                 \x20   let b: Array[String, 2] = a;\n\
                 \x20   println(\"done\");\n\
                 }\n",
            "done\n",
        ),
        // 3 — a user STRUCT element, the spelling B-2026-08-28-57's own
        //     comment measured when it drew the bodies/memory line.
        (
            "annotated-rebind-struct-element",
            "struct S { s: String }\n\
                 fn main() {\n\
                 \x20   let a: Array[S, 2] = [S { s: f\"aaaaaaaa0\" }, S { s: f\"bbbbbbbb1\" }];\n\
                 \x20   let b: Array[S, 2] = a;\n\
                 \x20   println(\"done\");\n\
                 }\n",
            "done\n",
        ),
        // 4 — two levels of heap under the element. This one printed
        //     NOTHING AT ALL on every compiled surface, not even the double
        //     free message.
        (
            "annotated-rebind-vec-string-element",
            "fn main() {\n\
                 \x20   let a: Array[Vec[String], 2] = [[f\"aaaaaaaa0\"], [f\"bbbbbbbb1\"]];\n\
                 \x20   let b: Array[Vec[String], 2] = a;\n\
                 \x20   println(\"done\");\n\
                 }\n",
            "done\n",
        ),
        // 5 — the destination is then READ, so the stand-down has to leave
        //     a live array behind rather than merely a balanced one.
        (
            "annotated-rebind-then-read",
            "fn main() {\n\
                 \x20   let a: Array[Vec[i64], 2] = [[10, 11], [20]];\n\
                 \x20   let b: Array[Vec[i64], 2] = a;\n\
                 \x20   println(f\"s:{b[0][1]}\");\n\
                 }\n",
            "s:11\n",
        ),
        // 6 — B-2026-09-09-9's rebind read, held back at the time because
        //     admitting it while this bug was live turned a loud refusal
        //     into a silent double free. It lands here, with its blocker.
        (
            "bare-rebind-then-read",
            "fn main() {\n\
                 \x20   let a: Array[Vec[i64], 2] = [[10, 11], [20]];\n\
                 \x20   let b = a;\n\
                 \x20   println(f\"s:{b[0][1]}\");\n\
                 }\n",
            "s:11\n",
        ),
        // 7 — CONTROL: the bare rebind was always clean, because the
        //     element type simply did not resolve for it.
        (
            "bare-rebind-struct-control",
            "struct S { s: String }\n\
                 fn main() {\n\
                 \x20   let a: Array[S, 2] = [S { s: f\"aaaaaaaa0\" }, S { s: f\"bbbbbbbb1\" }];\n\
                 \x20   let b = a;\n\
                 \x20   println(\"done\");\n\
                 }\n",
            "done\n",
        ),
        // 8 — CONTROL: a scalar element has no heap for a second owner to
        //     free, so it was correct either way.
        (
            "scalar-element-control",
            "fn main() {\n\
                 \x20   let a: Array[i64, 2] = [10, 11];\n\
                 \x20   let b: Array[i64, 2] = a;\n\
                 \x20   println(\"done\");\n\
                 }\n",
            "done\n",
        ),
        // 9 — CONTROL: the `Vec` container's move zeroes the source's cap,
        //     so its rebind never had two owners to begin with.
        (
            "vec-container-control",
            "fn main() {\n\
                 \x20   let v: Vec[Vec[i64]] = [[10, 11], [20]];\n\
                 \x20   let w: Vec[Vec[i64]] = v;\n\
                 \x20   println(\"done\");\n\
                 }\n",
            "done\n",
        ),
    ] {
        let Some(out) = run_program(src) else {
            return;
        };
        assert_eq!(out, want, "[{label}]");
    }
}

/// B-2026-09-10-4 — the `match`-arm-bound sibling of
/// [`e2e_array_rebind_leaves_memory_with_one_owner`], and the half of that
/// family that was still live on `main`.
///
/// -23 keyed its stand-down on `owned_array_params`, the set the two
/// `make_array_param_callee_owned` sites populate — a by-value array param
/// and a local array `let`. `bind_pattern_values` registers an arm-bound
/// `Array` payload in `array_elem_type_exprs` and DELIBERATELY in no memory
/// table, because the arm frees the payload, so the arm binding satisfied
/// neither half of the guard and its rebind took a second drop.
///
/// The row that filed this described a refusal, not a corruption: the
/// UN-ANNOTATED spelling `Some(t) => { let u = t; u[0][0] }` does not build
/// on `main`, because nothing resolves the destination's element type and
/// the nested read has nothing to index through. That framing missed the
/// spelling that needs no resolution at all — an ANNOTATED rebind takes its
/// element type from the annotation, and on `main` it builds, runs and
/// corrupts. Cells 1-3 are that spelling: 1 and 3 double free under the JIT
/// and at `-O0`, and cell 2 prints NOTHING on any compiled surface against
/// `--interp`'s `held`, which is the one this fixture catches by output
/// alone.
///
/// Cells 4-6 are the reads the fix newly admits, which is the part -9 held
/// back and -23 could only land narrowed; cells 7-9 are shapes that were
/// already right and have to stay right.
///
/// Output-only, so it pins the run/build agreement rather than the memory —
/// `-O2` prints the right answer for most of these cells even when the
/// program is corrupt, which is exactly why the memory half lives in
/// `asan_arm_bound_array_rebind_leaves_memory_with_one_owner`.
/// B-2026-09-06-49 / B-2026-09-10-6 — the OUTPUT twin of
/// `asan_boxed_array_payload_interior_has_exactly_one_owner`.
///
/// The memory fix moves who frees a boxed `Array` payload's interior, and
/// on the shapes with a user `Drop` element it also decides where the
/// BODIES run. Those are separate channels (B-2026-08-28-57: bodies follow
/// the move, memory does not), and a fix that conflates them prints a body
/// twice, or not at all, while every leak count still balances. This is the
/// half of that no sanitizer can see.
///
/// THIS FIXTURE DOES NOT FAIL ON A PRE-FIX TREE, and that is worth saying
/// rather than leaving to be discovered. Both rows are memory defects, and
/// this harness builds at `-O2`, where LLVM deletes the allocations nothing
/// observes — so every cell here prints correctly before the fix as well.
/// The gate is `asan_boxed_array_payload_interior_has_exactly_one_owner`
/// (which does fail, on its first cell), together with the `-O0` ratchet
/// leg that re-runs that same suite where the double frees actually abort.
/// What this one adds is the cross-backend OUTPUT contract, which no
/// sanitizer can see: the Display renderings, the `None` arm, and cell 6's
/// agreed silence.
#[test]
fn e2e_boxed_array_payload_reads_back_on_every_surface() {
    for (label, src, want) in [
            // 1 — the inline-literal payload of -49, read through the arm.
            (
                "inline-literal-payload-read",
                "fn plainA(x: Option[Array[String, 2]]) -> i64 {\n\
                 \x20   match x { Some(t) => { println(f\"s:{t[0]}\"); 1 } None => { println(\"n\"); 0 } }\n\
                 }\n\
                 fn main() {\n\
                 \x20   let n = plainA(Some([f\"aaaaaaaa0\", f\"bbbbbbbb0\"]));\n\
                 \x20   println(f\"n:{n}\");\n\
                 }\n",
                "s:aaaaaaaa0\nn:1\n",
            ),
            // 2 — the named-local spelling of the same call.
            (
                "named-local-payload-read",
                "fn plainA(x: Option[Array[String, 2]]) -> i64 {\n\
                 \x20   match x { Some(t) => { println(f\"s:{t[0]}\"); 1 } None => { println(\"n\"); 0 } }\n\
                 }\n\
                 fn main() {\n\
                 \x20   let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
                 \x20   let n = plainA(Some(a));\n\
                 \x20   println(f\"n:{n}\");\n\
                 }\n",
                "s:aaaaaaaa0\nn:1\n",
            ),
            // 3 — -6: the arm's binding handed by value to a consumer.
            (
                "arm-binding-passed-by-value",
                "fn take(a: Array[String, 2]) { println(f\"t:{a[0]}\") }\n\
                 fn plainP(x: Option[Array[String, 2]]) {\n\
                 \x20   match x { Some(t) => { take(t) } None => { println(\"n\") } }\n\
                 }\n\
                 fn main() {\n\
                 \x20   let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
                 \x20   plainP(Some(a));\n\
                 }\n",
                "t:aaaaaaaa0\n",
            ),
            // 4 — the same consumer one rebind away.
            (
                "arm-binding-rebound-then-passed",
                "fn take(a: Array[String, 2]) { println(f\"t:{a[0]}\") }\n\
                 fn plainM(x: Option[Array[String, 2]]) {\n\
                 \x20   match x { Some(t) => { let u = t; take(u) } None => { println(\"n\") } }\n\
                 }\n\
                 fn main() {\n\
                 \x20   let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
                 \x20   plainM(Some(a));\n\
                 }\n",
                "t:aaaaaaaa0\n",
            ),
            // 5 — the `Result` let site, whose payload the `Option`-shaped
            //     derivation beside it answers `None` for.
            (
                "let-site-result-ok-payload-consumed",
                "fn take(a: Array[String, 2]) { println(f\"t:{a[0]}\") }\n\
                 fn main() {\n\
                 \x20   let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
                 \x20   let o: Result[Array[String, 2], i64] = Ok(a);\n\
                 \x20   match o { Ok(t) => { take(t) } Err(e) => { println(f\"e:{e}\") } }\n\
                 }\n",
                "t:aaaaaaaa0\n",
            ),
            // 6 — THE BODIES CELL. It PINNED AN AGREED SILENCE until
            //     B-2026-09-10-27 closed it, and the pin did its job: that row
            //     wired codegen's array arm first, this cell went from
            //     `s:1 / end` to `s:1 / dR1 / dR2 / end` on the compiled
            //     backends while `--interp` still printed `s:1 / end`, and the
            //     loud failure here is what said the interpreter half had to
            //     land in the same commit. Both did.
            //
            //     Kept as a live assertion of the CORRECTED behaviour rather
            //     than deleted: the bodies channel is separate from the memory
            //     one (B-2026-08-28-57), so a later change to who owns the
            //     interior must not silently take these bodies away again.
            (
                "user-drop-element-bodies",
                "struct R8 { id: i64 }\n\
                 impl Drop for R8 { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
                 fn plainD(x: Option[Array[R8, 2]]) {\n\
                 \x20   match x { Some(t) => { println(f\"s:{t[0].id}\") } None => { println(\"n\") } }\n\
                 }\n\
                 fn main() {\n\
                 \x20   let a: Array[R8, 2] = [R8 { id: 1 }, R8 { id: 2 }];\n\
                 \x20   plainD(Some(a));\n\
                 \x20   println(\"end\");\n\
                 }\n",
                "s:1\ndR1\ndR2\nend\n",
            ),
            // 7 — CONTROL: a generic callee, whose monomorph registers nothing
            //     and whose caller must therefore keep its local.
            (
                "generic-callee-control",
                "fn takesOpt[T: Display](x: Option[T]) -> i64 {\n\
                 \x20   match x { Some(t) => { println(f\"s:{t}\"); 1 } None => { println(\"n\"); 0 } }\n\
                 }\n\
                 fn main() {\n\
                 \x20   let p: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
                 \x20   let n = takesOpt(Some(p));\n\
                 \x20   println(f\"n:{n}\");\n\
                 }\n",
                "s:[aaaaaaaa0, bbbbbbbb0]\nn:1\n",
            ),
            // 8 — CONTROL: a `ref` param takes nothing, so the box keeps its
            //     interior and the read still lands.
            (
                "ref-param-control",
                "fn peek(a: ref Array[String, 2]) { println(f\"p:{a[0]}\") }\n\
                 fn plainB(x: Option[Array[String, 2]]) {\n\
                 \x20   match x { Some(t) => { peek(t) } None => { println(\"n\") } }\n\
                 }\n\
                 fn main() {\n\
                 \x20   let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
                 \x20   plainB(Some(a));\n\
                 }\n",
                "p:aaaaaaaa0\n",
            ),
            // 9 — CONTROL: the `None` arm, where no payload exists at all.
            (
                "none-arm-control",
                "fn plainA(x: Option[Array[String, 2]]) -> i64 {\n\
                 \x20   match x { Some(t) => { println(f\"s:{t[0]}\"); 1 } None => { println(\"n\"); 0 } }\n\
                 }\n\
                 fn main() {\n\
                 \x20   let e: Option[Array[String, 2]] = None;\n\
                 \x20   let n = plainA(e);\n\
                 \x20   println(f\"n:{n}\");\n\
                 }\n",
                "n\nn:0\n",
            ),
            // 10 — CONTROL: a scalar element, outside this family entirely.
            (
                "scalar-element-control",
                "fn plainI(x: Option[Array[i64, 2]]) {\n\
                 \x20   match x { Some(t) => { println(f\"s:{t[0]}\") } None => { println(\"n\") } }\n\
                 }\n\
                 fn main() {\n\
                 \x20   let a: Array[i64, 2] = [7, 9];\n\
                 \x20   plainI(Some(a));\n\
                 }\n",
                "s:7\n",
            ),
        ] {
            let Some(out) = run_program(src) else {
                return;
            };
            assert_eq!(out, want, "[{label}]");
        }
}

/// B-2026-09-12-22 / B-2026-09-12-23 — the `ref`-SCRUTINEE half of the
/// family above, which that test covers only through OWNED matches.
///
/// Every cell of `e2e_boxed_array_payload_reads_back_on_every_surface`
/// matches a by-value scrutinee. Its cell 8 does put a `ref Array[T, N]`
/// on the CALLEE (`fn peek(a: ref Array[String, 2])`), which is what made
/// the gap easy to miss: the `ref` that was never tested is the one on the
/// SCRUTINEE, and it selects a different binder entirely
/// (`bind_pattern_values_via_ptr`, not `bind_pattern_values`).
///
/// That binder aliased the leaf AT the enum's payload word. For a BOXED
/// payload that word holds the box POINTER, so the binding was a `ref` to
/// a pointer typed `i64`:
///
///   - indexing it did not lower at all — `ref_array_index_target` wants an
///     `ArrayType` in `ref_params` and found that `i64` (cells 1, 3, 4, 6
///     are all `codegen failed: Index operator applied to non-array type`
///     on a pre-fix tree);
///   - passing it to a `ref Array[T, N]` callee lowered and read
///     UNINITIALISED memory (cell 2 printed `p:` followed by a NUL byte
///     pre-fix — the box pointer's own bytes read back as the string).
///
/// THE CELLS DO FAIL ON A PRE-FIX TREE, said plainly because the sibling
/// test above has to say the opposite: five of the six are a hard codegen
/// refusal and the sixth is visibly wrong output, at `-O2`, with no
/// sanitizer needed. The `-O2` column of the row is the trap — there the
/// undef folds to the OTHER arm's constant, which reads as a discriminant
/// bug; these cells are written to fail loudly instead.
///
/// Cell 5 is the control that bounds the fix: a `Vec` payload through the
/// same `ref` match is inline, needs no debox, and is correct on both
/// trees. Cell 6 pins the unit arm, the one the folded `-O2` read was
/// mistaken for.
#[test]
fn e2e_boxed_array_payload_reads_back_through_a_ref_match() {
    for (label, src, want) in [
            // 1 — the monomorphic enum, indexed directly in the arm.
            (
                "mono-ref-match-index",
                "enum Bin { Packed(Array[String, 2]), Bare }\n\
                 fn f(b: ref Bin) {\n\
                 \x20   match b { Packed(a) => { println(f\"s:{a[0]}\") } Bare => { println(\"n\") } }\n\
                 }\n\
                 fn main() {\n\
                 \x20   let x: Bin = Bin.Packed([f\"aaaaaaaa0\", f\"bbbbbbbb0\"]);\n\
                 \x20   f(x);\n\
                 }\n",
                "s:aaaaaaaa0\n",
            ),
            // 2 — THE SILENT CELL: the arm binding handed to a `ref Array`
            //     callee. This one LOWERED before the fix and printed a NUL.
            (
                "mono-ref-match-binding-to-ref-array-callee",
                "enum Bin { Packed(Array[String, 2]), Bare }\n\
                 fn peek(a: ref Array[String, 2]) { println(f\"p:{a[1]}\") }\n\
                 fn f(b: ref Bin) {\n\
                 \x20   match b { Packed(a) => { peek(a) } Bare => { println(\"n\") } }\n\
                 }\n\
                 fn main() {\n\
                 \x20   let x: Bin = Bin.Packed([f\"aaaaaaaa0\", f\"bbbbbbbb0\"]);\n\
                 \x20   f(x);\n\
                 }\n",
                "p:bbbbbbbb0\n",
            ),
            // 3 — `Option`, to show this is not about user enums.
            (
                "option-ref-match-index",
                "fn f(o: ref Option[Array[String, 2]]) {\n\
                 \x20   match o { Some(a) => { println(f\"s:{a[0]}\") } None => { println(\"n\") } }\n\
                 }\n\
                 fn main() {\n\
                 \x20   let x: Option[Array[String, 2]] = Some([f\"aaaaaaaa0\", f\"bbbbbbbb0\"]);\n\
                 \x20   f(x);\n\
                 }\n",
                "s:aaaaaaaa0\n",
            ),
            // 4 — a generic user enum at a heap ARRAY type argument, to show it
            //     is not the monomorphic/generic axis B-2026-09-12-12 was about.
            (
                "generic-ref-match-index",
                "enum Slot[T] { Filled(T), Blank }\n\
                 fn f(s: ref Slot[Array[String, 2]]) {\n\
                 \x20   match s { Filled(a) => { println(f\"s:{a[0]}\") } Blank => { println(\"n\") } }\n\
                 }\n\
                 fn main() {\n\
                 \x20   let x: Slot[Array[String, 2]] = Filled([f\"aaaaaaaa0\", f\"bbbbbbbb0\"]);\n\
                 \x20   f(x);\n\
                 }\n",
                "s:aaaaaaaa0\n",
            ),
            // 5 — CONTROL: a `Vec` payload is INLINE, so the same `ref` match
            //     needs no debox and was correct on a pre-fix tree too. This is
            //     what bounds the fix to boxed payloads.
            (
                "vec-payload-ref-match-control",
                "enum Vbin { V(Vec[String]), Z }\n\
                 fn f(b: ref Vbin) {\n\
                 \x20   match b { V(v) => { println(f\"s:{v[0]}\") } Z => { println(\"n\") } }\n\
                 }\n\
                 fn main() {\n\
                 \x20   let x: Vbin = Vbin.V(Vec[f\"aaaaaaaa0\", f\"bbbbbbbb0\"]);\n\
                 \x20   f(x);\n\
                 }\n",
                "s:aaaaaaaa0\n",
            ),
            // 6 — the UNIT arm through the same `ref` match. At `-O2` the
            //     pre-fix undef folded to exactly this arm's value, so a reader
            //     of that column would have called the bug a wrong-arm
            //     dispatch; pin the arm that really is taken here.
            (
                "unit-arm-ref-match",
                "enum Bin { Packed(Array[String, 2]), Bare }\n\
                 fn f(b: ref Bin) {\n\
                 \x20   match b { Packed(a) => { println(f\"s:{a[0]}\") } Bare => { println(\"n\") } }\n\
                 }\n\
                 fn main() {\n\
                 \x20   let x: Bin = Bare;\n\
                 \x20   f(x);\n\
                 }\n",
                "n\n",
            ),
        ] {
            let Some(out) = run_program(src) else {
                return;
            };
            assert_eq!(out, want, "[{label}]");
        }
}

/// B-2026-09-10-8 / B-2026-09-10-26 — the OUTPUT twin of
/// `asan_nested_array_element_interior_has_an_owner`.
///
/// Giving an `Array` element's interior an owner changes WHO frees those
/// buffers, and on the shapes with a user `Drop` element it also decides
/// WHERE the bodies run. Those are separate channels (B-2026-08-28-57:
/// bodies follow the move, memory does not), so a fix that conflates them
/// can balance every allocation while printing a body twice, or not at
/// all. That is the half no sanitizer sees.
///
/// THIS FIXTURE DOES NOT FAIL ON A PRE-FIX TREE, said plainly rather than
/// left to be found: both rows are leaks, this harness builds at `-O2`,
/// and at `-O2` LLVM deletes the buffers nothing observes — so every cell
/// prints correctly before the fix too. The gate is the ASAN twin (which
/// fails on its first cell) plus the `-O0` ratchet leg that re-runs that
/// suite. What this one adds is the cross-backend OUTPUT contract: the
/// reads, the empty arms, the comparator, and cell 8's agreed silence.
#[test]
fn e2e_nested_array_element_reads_back_on_every_surface() {
    for (label, src, want) in [
            // 1 — the plain nested LOCAL, read two levels deep.
            (
                "nested-array-local-read",
                "fn main() {\n\
                 \x20\x20\x20\x20let a: Array[Array[String, 2], 2] =\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20[[f\"aaaaaaaa0\", f\"bbbbbbbb0\"], [f\"cccccccc0\", f\"dddddddd0\"]];\n\
                 \x20\x20\x20\x20println(f\"s:{a[0][0]}\");\n\
                 \x20\x20\x20\x20println(f\"t:{a[1][1]}\");\n\
                 }\n",
                "s:aaaaaaaa0\nt:dddddddd0\n",
            ),
            // 2 — the by-value PARAM. The transfer has to leave the callee
            //     reading intact values, not a frame the caller already freed.
            (
                "nested-array-param-read",
                "fn eat(a: Array[Array[String, 2], 2]) { println(f\"e:{a[0][1]}\") }\n\
                 fn main() {\n\
                 \x20\x20\x20\x20let a: Array[Array[String, 2], 2] =\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20[[f\"aaaaaaaa0\", f\"bbbbbbbb0\"], [f\"cccccccc0\", f\"dddddddd0\"]];\n\
                 \x20\x20\x20\x20eat(a);\n\
                 \x20\x20\x20\x20println(\"end\");\n\
                 }\n",
                "e:bbbbbbbb0\nend\n",
            ),
            // 3 — the `Option` payload, the spelling B-2026-09-10-8 filed.
            (
                "nested-array-option-payload-read",
                "fn plainNN(x: Option[Array[Array[String, 2], 2]]) {\n\
                 \x20\x20\x20\x20match x { Some(t) => { println(f\"s:{t[1][0]}\") } None => { println(\"n\") } }\n\
                 }\n\
                 fn main() {\n\
                 \x20\x20\x20\x20let a: Array[Array[String, 2], 2] =\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20[[f\"aaaaaaaa0\", f\"bbbbbbbb0\"], [f\"cccccccc0\", f\"dddddddd0\"]];\n\
                 \x20\x20\x20\x20plainNN(Some(a));\n\
                 }\n",
                "s:cccccccc0\n",
            ),
            // 4 — the `None` arm of cell 3: the walk must not run over a
            //     payload that was never built.
            (
                "nested-array-option-none-arm",
                "fn plainNN(x: Option[Array[Array[String, 2], 2]]) {\n\
                 \x20\x20\x20\x20match x { Some(t) => { println(f\"s:{t[1][0]}\") } None => { println(\"n\") } }\n\
                 }\n\
                 fn main() { plainNN(None); }\n",
                "n\n",
            ),
            // 5 — the `Result` Err side, the same question for the other
            //     envelope: the Ok payload's array walk must not fire when the
            //     live tag is Err.
            (
                "nested-array-result-err-arm",
                "fn plainR(x: Result[Array[Array[String, 2], 2], i64]) {\n\
                 \x20\x20\x20\x20match x { Ok(t) => { println(f\"s:{t[0][0]}\") } Err(e) => { println(f\"n:{e}\") } }\n\
                 }\n\
                 fn main() { plainR(Err(7)); }\n",
                "n:7\n",
            ),
            // 6 — a REBIND, which must move the single owner rather than
            //     add one; the read after it proves the buffers are still
            //     alive.
            (
                "nested-array-rebind-read",
                "fn main() {\n\
                 \x20\x20\x20\x20let a: Array[Array[String, 2], 2] =\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20[[f\"aaaaaaaa0\", f\"bbbbbbbb0\"], [f\"cccccccc0\", f\"dddddddd0\"]];\n\
                 \x20\x20\x20\x20let u: Array[Array[String, 2], 2] = a;\n\
                 \x20\x20\x20\x20println(f\"s:{u[1][1]}\");\n\
                 }\n",
                "s:dddddddd0\n",
            ),
            // 7 — `==` over two nested arrays. The comparator walks the
            //     same element chain the drop does, so a wrong element
            //     identity shows up as a wrong ANSWER here rather than as a
            //     leak.
            (
                "nested-array-equality-reads",
                "fn mk(n: i64) -> Array[Array[String, 2], 2] {\n\
                 \x20\x20\x20\x20return [[f\"aaaaaaaa{n}\", f\"bbbbbbbb{n}\"], [f\"cccccccc{n}\", f\"dddddddd{n}\"]];\n\
                 }\n\
                 fn main() {\n\
                 \x20\x20\x20\x20let x: Array[Array[String, 2], 2] = mk(0);\n\
                 \x20\x20\x20\x20let y: Array[Array[String, 2], 2] = mk(0);\n\
                 \x20\x20\x20\x20let z: Array[Array[String, 2], 2] = mk(1);\n\
                 \x20\x20\x20\x20println(f\"e:{x == y}\");\n\
                 \x20\x20\x20\x20println(f\"n:{x == z}\");\n\
                 }\n",
                "e:true\nn:false\n",
            ),
            // 8 — B-2026-09-10-35, CLOSED by B-2026-09-14-15 (the two rows
            //     are the same defect, filed four days apart from opposite
            //     directions). This cell used to pin the compiled SILENCE
            //     while `--interp` ran all four bodies; the pin did its job,
            //     failing the moment the walkers learned the nesting, and the
            //     expectation moved deliberately with that fix. All five
            //     surfaces now print the four bodies in this order —
            //     `--interp`, JIT, and `karac build` at both opt levels and
            //     both auto-par settings, measured. The memory half was
            //     already fixed — cell 7 of the ASAN twin.
            (
                "nested-array-drop-bodies-run-on-every-surface",
                "struct R { s: String }\n\
                 impl Drop for R { fn drop(mut ref self) { println(f\"d:{self.s}\") } }\n\
                 fn main() {\n\
                 \x20\x20\x20\x20let a: Array[Array[R, 2], 2] =\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20[[R { s: f\"a0\" }, R { s: f\"a1\" }], [R { s: f\"b0\" }, R { s: f\"b1\" }]];\n\
                 \x20\x20\x20\x20println(\"s:ok\");\n\
                 \x20\x20\x20\x20println(\"end\");\n\
                 }\n",
                "d:a0\nd:a1\nd:b0\nd:b1\ns:ok\nend\n",
            ),
            // 9 — CONTROL for cell 8 one level up: a ONE-level
            //     `Array[R, 2]` runs both bodies on every backend, interpreter
            //     included. So the silence in cell 8 is specific to the
            //     nesting and not to arrays carrying a `Drop` element.
            (
                "one-level-array-drop-bodies-control",
                "struct R { s: String }\n\
                 impl Drop for R { fn drop(mut ref self) { println(f\"d:{self.s}\") } }\n\
                 fn main() {\n\
                 \x20\x20\x20\x20let a: Array[R, 2] = [R { s: f\"a0\" }, R { s: f\"a1\" }];\n\
                 \x20\x20\x20\x20println(\"s:ok\");\n\
                 \x20\x20\x20\x20println(\"end\");\n\
                 }\n",
                "d:a0\nd:a1\ns:ok\nend\n",
            ),
            // 10 — CONTROL: an all-scalar nest, which owns no heap and must
            //      keep emitting no walk at all.
            (
                "nested-array-scalar-control",
                "fn eat(a: Array[Array[i64, 2], 2]) { println(f\"e:{a[1][0]}\") }\n\
                 fn main() {\n\
                 \x20\x20\x20\x20let a: Array[Array[i64, 2], 2] = [[1, 2], [3, 4]];\n\
                 \x20\x20\x20\x20eat(a);\n\
                 }\n",
                "e:3\n",
            ),
        ] {
            let Some(out) = run_program(src) else {
                return;
            };
            assert_eq!(out, want, "[{label}]");
        }
}

#[test]
fn e2e_generic_array_param_reads_back_on_every_surface() {
    // B-2026-09-10-34 -- the cross-surface twin of
    // `asan_generic_callee_array_param_has_exactly_one_owner`.
    //
    // THIS FIXTURE DOES FAIL ON A PRE-FIX TREE, unlike its
    // B-2026-09-10-8 sibling: the defect is a DOUBLE FREE rather than a
    // leak, so the program aborts rather than quietly over-retaining, and
    // the harness builds at `-O2` where a leak would have been optimized
    // away. `--interp` prints correctly throughout, which is what makes
    // each cell a run-vs-build divergence as well as a memory fault.
    for (label, src, want) in [
            // 1 -- generic passthru, result bound.
            (
                "generic-array-bound-read",
                "fn passthru[T](x: T) -> T { return x; }\n\
                 fn main() {\n\
                 \x20\x20\x20\x20let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
                 \x20\x20\x20\x20let b: Array[String, 2] = passthru(a);\n\
                 \x20\x20\x20\x20println(f\"s:{b[0]}\");\n\
                 \x20\x20\x20\x20println(f\"t:{b[1]}\");\n\
                 }\n",
                "s:aaaaaaaa0\nt:bbbbbbbb0\n",
            ),
            // 2 -- the nested element through the same generic.
            (
                "generic-nested-array-bound-read",
                "fn passthru[T](x: T) -> T { return x; }\n\
                 fn main() {\n\
                 \x20\x20\x20\x20let a: Array[Array[String, 2], 2] =\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20[[f\"aaaaaaaa0\", f\"bbbbbbbb0\"], [f\"cccccccc0\", f\"dddddddd0\"]];\n\
                 \x20\x20\x20\x20let b: Array[Array[String, 2], 2] = passthru(a);\n\
                 \x20\x20\x20\x20println(f\"s:{b[0][0]}\");\n\
                 \x20\x20\x20\x20println(f\"t:{b[1][1]}\");\n\
                 }\n",
                "s:aaaaaaaa0\nt:dddddddd0\n",
            ),
            // 3 -- a generic METHOD with an array argument.
            (
                "generic-method-array-arg-read",
                "struct H { k: i64 }\n\
                 impl H {\n\
                 \x20\x20\x20\x20fn pass[T](ref self, x: T) -> T { return x; }\n\
                 }\n\
                 fn main() {\n\
                 \x20\x20\x20\x20let h: H = H { k: 1 };\n\
                 \x20\x20\x20\x20let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
                 \x20\x20\x20\x20let b: Array[String, 2] = h.pass(a);\n\
                 \x20\x20\x20\x20println(f\"s:{b[1]}\");\n\
                 }\n",
                "s:bbbbbbbb0\n",
            ),
            // 4 -- two type params, one array.
            (
                "generic-two-param-one-array-read",
                "fn firstof[T, U](x: T, y: U) -> T { return x; }\n\
                 fn main() {\n\
                 \x20\x20\x20\x20let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
                 \x20\x20\x20\x20let k: i64 = 3;\n\
                 \x20\x20\x20\x20let b: Array[String, 2] = firstof(a, k);\n\
                 \x20\x20\x20\x20println(f\"s:{b[0]}\");\n\
                 }\n",
                "s:aaaaaaaa0\n",
            ),
            // 5 -- forwarded through a generic caller, the shape that rejects
            //      a resolver keyed only on the typechecker's per-call record.
            (
                "generic-forwarded-through-generic-caller-read",
                "fn passthru[T](x: T) -> T { return x; }\n\
                 fn outer[U](y: U) -> U { return passthru(y); }\n\
                 fn main() {\n\
                 \x20\x20\x20\x20let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
                 \x20\x20\x20\x20let b: Array[String, 2] = outer(a);\n\
                 \x20\x20\x20\x20println(f\"s:{b[0]}\");\n\
                 }\n",
                "s:aaaaaaaa0\n",
            ),
            // 6 -- CONTROL: consumed rather than returned. The callee must own
            //      the buffers outright once the caller has stood down.
            (
                "generic-array-consumed-control-read",
                "fn eat[T](x: T) -> i64 { return 7; }\n\
                 fn main() {\n\
                 \x20\x20\x20\x20let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
                 \x20\x20\x20\x20let n: i64 = eat(a);\n\
                 \x20\x20\x20\x20println(f\"s:{n}\");\n\
                 }\n",
                "s:7\n",
            ),
            // 7 -- CONTROL: a `ref` generic param borrows, so the caller's
            //      binding is still readable after the call.
            (
                "generic-ref-array-param-control-read",
                "fn peek[T](x: ref T) -> i64 { return 1; }\n\
                 fn main() {\n\
                 \x20\x20\x20\x20let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
                 \x20\x20\x20\x20let n: i64 = peek(a);\n\
                 \x20\x20\x20\x20println(f\"s:{n}\");\n\
                 \x20\x20\x20\x20println(f\"t:{a[0]}\");\n\
                 }\n",
                "s:1\nt:aaaaaaaa0\n",
            ),
            // 8 -- CONTROL: the concrete twin, clean before this change and
            //      after. It is the oracle the generic path is being brought
            //      into line with.
            (
                "concrete-array-bound-control-read",
                "fn passthru(x: Array[String, 2]) -> Array[String, 2] { return x; }\n\
                 fn main() {\n\
                 \x20\x20\x20\x20let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
                 \x20\x20\x20\x20let b: Array[String, 2] = passthru(a);\n\
                 \x20\x20\x20\x20println(f\"s:{b[0]}\");\n\
                 }\n",
                "s:aaaaaaaa0\n",
            ),
            // 9 -- CONTROL: scalar elements own no heap and must stay
            //      unregistered.
            (
                "generic-scalar-array-control-read",
                "fn passthru[T](x: T) -> T { return x; }\n\
                 fn main() {\n\
                 \x20\x20\x20\x20let a: Array[i64, 3] = [1, 2, 3];\n\
                 \x20\x20\x20\x20let b: Array[i64, 3] = passthru(a);\n\
                 \x20\x20\x20\x20println(f\"s:{b[2]}\");\n\
                 }\n",
                "s:3\n",
            ),
        ] {
            let Some(out) = run_program(src) else {
                return;
            };
            assert_eq!(out, want, "[{label}]");
        }
}

#[test]
fn e2e_discarded_array_result_reads_back_on_every_surface() {
    // B-2026-09-12-2 -- the cross-surface twin of
    // `asan_discarded_array_result_has_exactly_one_owner`.
    //
    // A PRE-FIX TREE PASSES THIS FIXTURE, and that is the point rather
    // than a weakness: the defect is a pure leak, silent on every surface
    // and invisible at `-O2` where the harness builds, so nothing here
    // can observe it. What these cells guard is the OTHER direction --
    // a fix that registers one owner too many aborts the program, and
    // cell 7 is the shape that actually did so before the admission gate
    // went in. Read it together with the ASAN twin, which carries the
    // counts; this one carries the corruption check.
    for (label, src, want) in [
            // 1 -- concrete callee, result discarded.
            (
                "discarded-array-concrete-read",
                "fn passthru(x: Array[String, 2]) -> Array[String, 2] { return x; }\n\
                 fn main() {\n\
                 \x20\x20\x20\x20let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
                 \x20\x20\x20\x20passthru(a);\n\
                 \x20\x20\x20\x20println(\"s:ok\");\n\
                 }\n",
                "s:ok\n",
            ),
            // 2 -- the generic spelling, which resolves its return type
            //      through the per-call substitution rather than the
            //      free-function table.
            (
                "discarded-array-generic-read",
                "fn passthru[T](x: T) -> T { return x; }\n\
                 fn main() {\n\
                 \x20\x20\x20\x20let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
                 \x20\x20\x20\x20passthru(a);\n\
                 \x20\x20\x20\x20println(\"s:ok\");\n\
                 }\n",
                "s:ok\n",
            ),
            // 3 -- the nested element.
            (
                "discarded-array-nested-read",
                "fn passthru(x: Array[Array[String, 2], 2]) -> Array[Array[String, 2], 2] {\n\
                 \x20\x20\x20\x20return x;\n\
                 }\n\
                 fn main() {\n\
                 \x20\x20\x20\x20let a: Array[Array[String, 2], 2] =\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20[[f\"aaaaaaaa0\", f\"bbbbbbbb0\"], [f\"cccccccc0\", f\"dddddddd0\"]];\n\
                 \x20\x20\x20\x20passthru(a);\n\
                 \x20\x20\x20\x20println(\"s:ok\");\n\
                 }\n",
                "s:ok\n",
            ),
            // 4 -- a discarded METHOD result.
            (
                "discarded-array-method-read",
                "struct H { k: i64 }\n\
                 impl H {\n\
                 \x20\x20\x20\x20fn passthru(self, x: Array[String, 2]) -> Array[String, 2] { return x; }\n\
                 }\n\
                 fn main() {\n\
                 \x20\x20\x20\x20let h: H = H { k: 1 };\n\
                 \x20\x20\x20\x20let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
                 \x20\x20\x20\x20h.passthru(a);\n\
                 \x20\x20\x20\x20println(\"s:ok\");\n\
                 }\n",
                "s:ok\n",
            ),
            // 5 -- the `match`-arm return.
            (
                "discarded-array-match-arm-read",
                "fn passthru(x: Array[String, 2], c: bool) -> Array[String, 2] {\n\
                 \x20\x20\x20\x20match c {\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20true => { return x; }\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20false => { return x; }\n\
                 \x20\x20\x20\x20}\n\
                 }\n\
                 fn main() {\n\
                 \x20\x20\x20\x20let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
                 \x20\x20\x20\x20passthru(a, true);\n\
                 \x20\x20\x20\x20println(\"s:ok\");\n\
                 }\n",
                "s:ok\n",
            ),
            // 6 -- a callee that MINTS the array.
            (
                "discarded-array-minted-read",
                "fn mint() -> Array[String, 2] { return [f\"aaaaaaaa0\", f\"bbbbbbbb0\"]; }\n\
                 fn main() {\n\
                 \x20\x20\x20\x20mint();\n\
                 \x20\x20\x20\x20println(\"s:ok\");\n\
                 }\n",
                "s:ok\n",
            ),
            // 7 -- THE CORRUPTION CONTROL: a borrow-projection return. With
            //      the arm ungated this aborts on a double free rather than
            //      printing, on every compiled surface.
            (
                "discarded-borrow-projection-read",
                "struct W { a: Array[String, 2] }\n\
                 fn get(w: ref W) -> Array[String, 2] { return w.a; }\n\
                 fn main() {\n\
                 \x20\x20\x20\x20let w: W = W { a: [f\"aaaaaaaa0\", f\"bbbbbbbb0\"] };\n\
                 \x20\x20\x20\x20get(w);\n\
                 \x20\x20\x20\x20println(\"s:ok\");\n\
                 }\n",
                "s:ok\n",
            ),
            // 8 -- CONTROL: the bound result still reads back.
            (
                "discarded-array-bound-control-read",
                "fn passthru(x: Array[String, 2]) -> Array[String, 2] { return x; }\n\
                 fn main() {\n\
                 \x20\x20\x20\x20let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
                 \x20\x20\x20\x20let b: Array[String, 2] = passthru(a);\n\
                 \x20\x20\x20\x20println(f\"s:{b[0]}\");\n\
                 }\n",
                "s:aaaaaaaa0\n",
            ),
            // 9 -- CONTROL: scalar elements register nothing.
            (
                "discarded-scalar-array-control-read",
                "fn passthru(x: Array[i64, 2]) -> Array[i64, 2] { return x; }\n\
                 fn main() {\n\
                 \x20\x20\x20\x20let a: Array[i64, 2] = [11, 22];\n\
                 \x20\x20\x20\x20passthru(a);\n\
                 \x20\x20\x20\x20println(\"s:ok\");\n\
                 }\n",
                "s:ok\n",
            ),
        ] {
            let Some(out) = run_program(src) else {
                return;
            };
            assert_eq!(out, want, "[{label}]");
        }
}

#[test]
fn e2e_array_binding_moved_into_struct_field_reads_back_on_every_surface() {
    // B-2026-09-12-14 -- the cross-surface twin of
    // `asan_array_binding_moved_into_struct_field_has_one_owner`.
    //
    // A PRE-FIX TREE PASSES THIS FIXTURE. glibc's tcache absorbs the
    // duplicate free rather than aborting, so every backend printed
    // correctly and exited 0 -- which is exactly why the defect survived:
    // no output gate anywhere could see it. The ASAN twin carries the
    // counts and is the one that fails pre-fix (at BOTH opt levels, since
    // a double free is not an allocation the optimizer can delete). What
    // these cells guard is the read-back: the field must still be
    // readable after the source binding's drop is retracted, on every
    // surface, and a retraction that went too far would print garbage or
    // abort here.
    for (label, src, want) in [
            (
                "array-binding-into-struct-field-read",
                "struct W { a: Array[String, 2] }\n\
                 fn main() {\n\
                 \x20\x20\x20\x20let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
                 \x20\x20\x20\x20let w: W = W { a: a };\n\
                 \x20\x20\x20\x20println(f\"s:{w.a[0]}\");\n\
                 \x20\x20\x20\x20println(f\"t:{w.a[1]}\");\n\
                 }\n",
                "s:aaaaaaaa0\nt:bbbbbbbb0\n",
            ),
            (
                "two-array-fields-read",
                "struct W { a: Array[String, 2], b: Array[String, 2] }\n\
                 fn main() {\n\
                 \x20\x20\x20\x20let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
                 \x20\x20\x20\x20let b: Array[String, 2] = [f\"cccccccc0\", f\"dddddddd0\"];\n\
                 \x20\x20\x20\x20let w: W = W { a: a, b: b };\n\
                 \x20\x20\x20\x20println(f\"s:{w.a[0]}\");\n\
                 \x20\x20\x20\x20println(f\"t:{w.b[1]}\");\n\
                 }\n",
                "s:aaaaaaaa0\nt:dddddddd0\n",
            ),
            (
                "escaping-struct-field-read",
                "struct W { a: Array[String, 2] }\n\
                 fn mk() -> W {\n\
                 \x20\x20\x20\x20let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
                 \x20\x20\x20\x20return W { a: a };\n\
                 }\n\
                 fn main() {\n\
                 \x20\x20\x20\x20let w: W = mk();\n\
                 \x20\x20\x20\x20println(f\"s:{w.a[0]}\");\n\
                 }\n",
                "s:aaaaaaaa0\n",
            ),
            (
                "generic-struct-array-field-read",
                "struct Box[T] { v: T }\n\
                 fn main() {\n\
                 \x20\x20\x20\x20let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
                 \x20\x20\x20\x20let w: Box[Array[String, 2]] = Box[Array[String, 2]] { v: a };\n\
                 \x20\x20\x20\x20println(f\"s:{w.v[1]}\");\n\
                 }\n",
                "s:bbbbbbbb0\n",
            ),
            (
                "array-field-move-in-loop-read",
                "struct W { a: Array[String, 2] }\n\
                 fn main() {\n\
                 \x20\x20\x20\x20let mut i: i64 = 0;\n\
                 \x20\x20\x20\x20while i < 3 {\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20let w: W = W { a: a };\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20println(f\"s:{w.a[0]}\");\n\
                 \x20\x20\x20\x20\x20\x20\x20\x20i = i + 1;\n\
                 \x20\x20\x20\x20}\n\
                 }\n",
                "s:aaaaaaaa0\ns:aaaaaaaa0\ns:aaaaaaaa0\n",
            ),
            // CONTROL: the discarded literal, whose source keeps its owner.
            (
                "discarded-struct-literal-read",
                "struct W { a: Array[String, 2] }\n\
                 fn main() {\n\
                 \x20\x20\x20\x20let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
                 \x20\x20\x20\x20W { a: a };\n\
                 \x20\x20\x20\x20println(\"s:ok\");\n\
                 }\n",
                "s:ok\n",
            ),
            // CONTROL: the shared struct, owned through the RC path.
            (
                "shared-struct-array-field-read",
                "shared struct W { a: Array[String, 2] }\n\
                 fn main() {\n\
                 \x20\x20\x20\x20let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
                 \x20\x20\x20\x20let w: W = W { a: a };\n\
                 \x20\x20\x20\x20println(f\"s:{w.a[0]}\");\n\
                 }\n",
                "s:aaaaaaaa0\n",
            ),
            // CONTROL: scalar elements, where the disarm must find nothing.
            (
                "scalar-array-field-read",
                "struct W { a: Array[i64, 2] }\n\
                 fn main() {\n\
                 \x20\x20\x20\x20let a: Array[i64, 2] = [11, 22];\n\
                 \x20\x20\x20\x20let w: W = W { a: a };\n\
                 \x20\x20\x20\x20println(f\"s:{w.a[1]}\");\n\
                 }\n",
                "s:22\n",
            ),
        ] {
            let Some(out) = run_program(src) else {
                return;
            };
            assert_eq!(out, want, "[{label}]");
        }
}

#[test]
fn e2e_arm_bound_array_rebind_reads_back_on_every_surface() {
    for (label, src, want) in [
            // 1 — the annotated rebind, `String` element.
            (
                "annotated-arm-rebind-string-element",
                "fn plainA(x: Option[Array[String, 2]]) {\n\
                 \x20   match x { Some(t) => { let u: Array[String, 2] = t; println(f\"s:{u[0]}\") } None => { println(\"n\") } }\n\
                 }\n\
                 fn main() {\n\
                 \x20   let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
                 \x20   plainA(Some(a));\n\
                 }\n",
                "s:aaaaaaaa0\n",
            ),
            // 2 — the annotated rebind, `Vec[String]` element.
            (
                "annotated-arm-rebind-vec-string-element",
                "fn plainAV(x: Option[Array[Vec[String], 2]]) {\n\
                 \x20   match x { Some(t) => { let u: Array[Vec[String], 2] = t; println(\"held\") } None => { println(\"n\") } }\n\
                 }\n\
                 fn main() {\n\
                 \x20   let a: Array[Vec[String], 2] = [[f\"aaaaaaaa0\", f\"aaaaaaaa1\"], [f\"bbbbbbbb0\"]];\n\
                 \x20   plainAV(Some(a));\n\
                 }\n",
                "held\n",
            ),
            // 3 — the annotated rebind, user struct element.
            (
                "annotated-arm-rebind-struct-element",
                "struct S5 { s: String }\n\
                 fn plainAS(x: Option[Array[S5, 2]]) {\n\
                 \x20   match x { Some(t) => { let u: Array[S5, 2] = t; println(f\"s:{u[0].s}\") } None => { println(\"n\") } }\n\
                 }\n\
                 fn main() {\n\
                 \x20   let a: Array[S5, 2] = [S5 { s: f\"aaaaaaaa0\" }, S5 { s: f\"bbbbbbbb0\" }];\n\
                 \x20   plainAS(Some(a));\n\
                 }\n",
                "s:aaaaaaaa0\n",
            ),
            // 4 — the row's own repro: the bare rebind plus the NESTED read,
            //     which refused to build on `main` with "nested indexed read on
            //     'u' — element TypeExpr unknown".
            (
                "bare-arm-rebind-then-nested-read",
                "fn plainV(x: Option[Array[Vec[String], 2]]) {\n\
                 \x20   match x { Some(t) => { let u = t; println(f\"s:{u[0][0]}\") } None => { println(\"n\") } }\n\
                 }\n\
                 fn main() {\n\
                 \x20   let a: Array[Vec[String], 2] = [[f\"aaaaaaaa0\", f\"aaaaaaaa1\"], [f\"bbbbbbbb0\"]];\n\
                 \x20   plainV(Some(a));\n\
                 }\n",
                "s:aaaaaaaa0\n",
            ),
            // 5 — the `Result` spelling of cell 4.
            (
                "result-arm-rebind-then-nested-read",
                "fn plainR(x: Result[Array[Vec[String], 2], i64]) {\n\
                 \x20   match x { Ok(t) => { let u = t; println(f\"s:{u[0][0]}\") } Err(e) => { println(f\"e:{e}\") } }\n\
                 }\n\
                 fn main() {\n\
                 \x20   let a: Array[Vec[String], 2] = [[f\"aaaaaaaa0\", f\"aaaaaaaa1\"], [f\"bbbbbbbb0\"]];\n\
                 \x20   plainR(Result.Ok(a));\n\
                 }\n",
                "s:aaaaaaaa0\n",
            ),
            // 6 — two rebinds, then the read. The element type has to survive
            //     both hops or the read refuses again at the second one.
            (
                "chained-arm-rebind-then-nested-read",
                "fn plainC(x: Option[Array[Vec[String], 2]]) {\n\
                 \x20   match x { Some(t) => { let u = t; let v = u; println(f\"s:{v[0][0]}\") } None => { println(\"n\") } }\n\
                 }\n\
                 fn main() {\n\
                 \x20   let a: Array[Vec[String], 2] = [[f\"aaaaaaaa0\", f\"aaaaaaaa1\"], [f\"bbbbbbbb0\"]];\n\
                 \x20   plainC(Some(a));\n\
                 }\n",
                "s:aaaaaaaa0\n",
            ),
            // 7 — CONTROL: the arm read with no rebind, which B-2026-09-09-9
            //     already fixed and which must not move.
            (
                "plain-arm-nested-read-control",
                "fn plainP(x: Option[Array[Vec[String], 2]]) {\n\
                 \x20   match x { Some(t) => { println(f\"s:{t[0][0]}\") } None => { println(\"n\") } }\n\
                 }\n\
                 fn main() {\n\
                 \x20   let a: Array[Vec[String], 2] = [[f\"aaaaaaaa0\", f\"aaaaaaaa1\"], [f\"bbbbbbbb0\"]];\n\
                 \x20   plainP(Some(a));\n\
                 }\n",
                "s:aaaaaaaa0\n",
            ),
            // 8 — CONTROL: a scalar element, which owns no heap and so was
            //     never part of this.
            (
                "scalar-element-control",
                "fn plainI(x: Option[Array[i64, 2]]) {\n\
                 \x20   match x { Some(t) => { let u = t; println(f\"s:{u[0]}\") } None => { println(\"n\") } }\n\
                 }\n\
                 fn main() {\n\
                 \x20   let a: Array[i64, 2] = [11, 22];\n\
                 \x20   plainI(Some(a));\n\
                 }\n",
                "s:11\n",
            ),
            // 9 — CONTROL: -23's `let`-bound rebind read, which this fix's
            //     wider guard must leave exactly where -23 put it.
            (
                "let-bound-rebind-read-control",
                "fn main() {\n\
                 \x20   let a: Array[Vec[i64], 2] = [[10, 11], [20]];\n\
                 \x20   let b = a;\n\
                 \x20   println(f\"s:{b[0][1]}\");\n\
                 }\n",
                "s:11\n",
            ),
        ] {
            let Some(out) = run_program(src) else {
                return;
            };
            assert_eq!(out, want, "[{label}]");
        }
}

/// B-2026-09-19-40's output twin — the boxed `Array` payload handed to a
/// callee-owned param aborted the compiled program (`free(): double free
/// detected in tcache 2`, exit 134) while `--interp` printed correctly, so
/// the run-vs-build claim is the one worth pinning here; the memory claim
/// lives in `tests/memory_sanitizer.rs`.
///
/// The mono and read-only rows are controls: the mono spelling retracts
/// through the `field_drop_kinds` alias a generic declaration (spelled `T`)
/// never reaches, and the read-only arm must KEEP its interior drop, which
/// is what an over-broad retraction would break.
#[test]
fn e2e_boxed_array_payload_handed_to_callee_owned_param() {
    let src = r#"
enum G[T] { Y(T), N }
enum M { M(Array[String, 2]), N }

fn eats(a: Array[String, 2]) -> i64 { return a[0].len(); }

fn hand_gen(g: G[Array[String, 2]]) -> i64 {
    match g { G.Y(x) => { return eats(x); } G.N => { return 0; } }
}

fn hand_mono(g: M) -> i64 {
    match g { M.M(x) => { return eats(x); } M.N => { return 0; } }
}

fn read_gen(g: G[Array[String, 2]]) -> i64 {
    match g { G.Y(x) => { return x[0].len(); } G.N => { return 0; } }
}

fn main() {
    let mut n = 0;
    while n < 3 {
        let a: Array[String, 2] = [f"hand-{n}-padpad", f"snd-{n}-padpad"];
        let g: G[Array[String, 2]] = G.Y(a);
        println(f"hg:{hand_gen(g)}");
        let b: Array[String, 2] = [f"mono-{n}-padpad", f"snd-{n}-padpad"];
        let m: M = M.M(b);
        println(f"hm:{hand_mono(m)}");
        let c: Array[String, 2] = [f"read-{n}-padpad", f"snd-{n}-padpad"];
        let r: G[Array[String, 2]] = G.Y(c);
        println(f"rg:{read_gen(r)}");
        n = n + 1;
    }
    println("end");
}
"#;
    assert_eq!(
        run_program(src).as_deref(),
        Some("hg:13\nhm:13\nrg:13\nhg:13\nhm:13\nrg:13\nhg:13\nhm:13\nrg:13\nend\n"),
    );
}

/// B-2026-09-21-8 — a ONE-ELEMENT `Array` enum payload whose element holds a
/// `shared` handle is laid out the same way by the layout pass and by the pack.
///
/// `enum One { A(Array[Sd, 1]), N }` over `struct Sd { h: Inner }` and
/// `shared struct Inner { tag: String }` SEGFAULTED on jit, `-O0` and `-O2`
/// against a correct `--interp`, as a four-line whole program with no call, no
/// match and no `impl Drop` needed anywhere.
///
/// `payload_word_count_for_type_expr` answered a DIFFERENT WIDTH in its two
/// windows. It recognises a `shared` type as one pointer word through
/// `shared_types`, which the STRUCT LLVM build fills — after `declare_enums` has
/// run. So at layout time a shared type reached through a plain struct's FIELD
/// missed that arm, fell through to the struct recursion, and was sized by its own
/// fields: `Sd` measured 3 words at declare time and 1 word at compile time. The
/// `BoxedArray` pass compares `elem_words * n > field_words`, so 3 > 1 classified
/// the payload BOXED while the pack side — reading real LLVM widths — rode it
/// INLINE. `__karac_drop_E` then `inttoptr`'d the RC handle, walked it as an
/// `Array[Sd, 1]` and `free`d it, reading the refcount word as a pointer:
/// `Invalid read of size 8` at address 0x1.
///
/// The fix reads `shared_type_decl_names` beside `shared_types` — the name-only
/// set `register_struct_metadata` fills for exactly this window, which
/// `enum_drop_kind_for_type_expr`'s `SharedRc` arm already consults for the DIRECT
/// payload position (B-2026-09-10-11). The nested position was never wired to it.
///
/// THE CELLS THAT ARE NOT THE FAULT ARE MOST OF THIS FIXTURE. `g` is
/// `Array[Sd, 2]`, two words, which takes the boxed path and was always correct;
/// `h` is a direct `Sd` payload (`NestedStruct`); `i` is a heap-free element;
/// `d` is a `shared` struct that owns no heap, where the same width error sized
/// the payload at 1 and simply left it unclassified. `e` passes the value to a
/// by-value callee and `f` matches on it, the two spellings that carry the most
/// ownership machinery. All nine agree byte-for-byte across `--interp`,
/// `karac run`, `-O0` and `-O2`.
///
/// WHAT THIS FIXTURE DOES NOT PIN, deliberately: the RC box is still STRANDED on
/// the inline shapes (32 B for a heap-owning `Inner`, 16 B for a heap-free one).
/// An inline one-element array payload has no drop kind at all, which is this
/// row's second face and is split out as B-2026-09-21-9 with its three sites
/// named. No memory-sanitizer cell accompanies this fixture for that reason; what
/// it guards is the crash, and the crash is what the output can see.
#[test]
fn test_e2e_one_element_array_enum_payload_with_a_shared_handle_does_not_crash() {
    let src = r#"
shared struct Inner { tag: String }
shared struct Ik { k: i64 }
struct Sd { h: Inner }
impl Drop for Sd { fn drop(mut ref self) { println("dSd") } }
struct Sq { h: Inner }
struct Sk { h: Ik }
struct Sn { k: i64 }

enum One { A(Array[Sd, 1]), N }
enum Bare { A(Array[Sq, 1]), N }
enum Direct { A(Array[Inner, 1]), N }
enum Flat { A(Array[Ik, 1]), N }
enum Wide { A(Array[Sd, 2]), N }
enum Plain { A(Sd), N }
enum Scalar { A(Array[Sn, 1]), N }

fn eat(g: One) -> i64 { match g { One.A(x) => { return 7; } One.N => { return 0; } } }

fn a_bind() { let g: One = One.A([Sd { h: Inner { tag: "q" } }]); println("a"); }
fn b_nobody() { let g: Bare = Bare.A([Sq { h: Inner { tag: "q" } }]); println("b"); }
fn c_shared_elem() { let g: Direct = Direct.A([Inner { tag: "q" }]); println("c"); }
fn d_heapfree() { let g: Flat = Flat.A([Ik { k: 5 }]); println("d"); }
fn e_byvalue() { let g: One = One.A([Sd { h: Inner { tag: "q" } }]); println(f"e:{eat(g)}"); }
fn f_match() { let g: One = One.A([Sd { h: Inner { tag: "q" } }]); match g { One.A(x) => { println("f"); } One.N => { println("fn"); } } }
fn g_wide() { let g: Wide = Wide.A([Sd { h: Inner { tag: "q" } }, Sd { h: Inner { tag: "r" } }]); println("g"); }
fn h_direct() { let g: Plain = Plain.A(Sd { h: Inner { tag: "q" } }); println("h"); }
fn i_scalar() { let g: Scalar = Scalar.A([Sn { k: 5 }]); println("i"); }

fn main() {
    a_bind();
    b_nobody();
    c_shared_elem();
    d_heapfree();
    e_byvalue();
    f_match();
    g_wide();
    h_direct();
    i_scalar();
    println("end");
}
"#;
    assert_eq!(
        run_program(src).as_deref(),
        Some("dSd\na\nb\nc\nd\ne:7\ndSd\nf\ndSd\ndSd\ndSd\ng\ndSd\nh\ni\nend\n")
    );
}

/// B-2026-09-22-11 — the BUILTIN-ENVELOPE spelling of B-2026-09-22-10.
/// A by-value `Array` param moved into a NAMED SEEDED `Option`/`Result` local
/// was freed by both the caller and this frame.
///
/// `let o = Option.Some(a); match o` over `fn f(a: Array[S, 2])` aborted with
/// `free(): double free detected in tcache 2`, exit 134 at `-O0`, on EIGHT of
/// this fixture's fourteen cells against the unfixed compiler, and under
/// valgrind on 17 invalid frees where the fixed arm has none. All three
/// compiled surfaces agreed on the fault and agree on the fix.
///
/// THE PREDICATE ALREADY EXISTED AND THIS SITE NEVER ASKED IT, which is the
/// whole of the change. B-2026-09-19-61 wired
/// `seeded_array_payload_stays_with_caller` into the match-arm gate
/// `array_arm_owns_interior`, whose handle on the constructor is
/// `seeded_variant_arg_payload(scrutinee)` — and a NAMED scrutinee is an
/// `Identifier`, not a call, so that lookup answers `None` and the gate never
/// fires. The constructor is visible at the `let`, which is where the box's
/// interior walk is armed for this spelling, and that is where the question is
/// now asked.
///
/// DECLINE TO ARM, not retract — the third branch that predicate's own doc
/// names. The caller's free is deliberately not retractable: the element `Drop`
/// BODIES ride a caller-side channel and fire while the caller still holds the
/// value, so retracting it would make those bodies read freed memory.
/// Declining leaves the box's own `free` standing, which is this frame's to do,
/// and is byte-for-byte the IR the FRESH TEMP spelling already emits — `b/fresh`
/// is that cell, kept so a change to the shared predicate cannot quietly move
/// the site B-2026-09-19-61 fixed.
///
/// FOUR SPELLINGS WERE ON THE ROW'S OWN NOT-MEASURED LIST AND THREE OF THEM
/// ABORT. `s/iflet`, `s/letelse` and `s/whilelet` each reach the same `let`
/// registration and each went 134 → 0; they are here because a row's
/// not-measured list is where the next spelling hides. `b/chain` is the fourth
/// and is the one that did NOT reproduce: `let o2 = o;` is clean on BOTH arms,
/// so the propagation the user-enum sibling needed for its `l/chain` is not
/// owed here. It stays as the pin for that difference.
///
/// `s/read` reads `v[0].tag` back before the walk, so it cannot pass while the
/// payload is being lost; `s/three` widens to three elements (three invalid
/// frees on the control, against two for each two-element cell, which is what
/// makes 17 add up); `s/res` is the `Result.Ok` sibling, reached through a
/// different variant on the same registration.
///
/// `b/str` DID NOT PIN THE WIDENING, IT CAUGHT IT, which is the one thing in
/// this fixture worth reading before the next row in this family. The first
/// spelling of the fix gated on `seeded_array_payload_stays_with_caller` alone,
/// which answers by MEMBERSHIP of `owned_array_params` -- a map filled at a
/// point this `let` does not see -- so an `Array[String, 2]` param read as
/// caller-retained, the walk was declined, and both element buffers leaked
/// 18 B in 2 blocks. That is the same figure, and the same mistake, that
/// B-2026-09-22-6's own doc records for its spelling of it, written before this
/// one was attempted. The fix is that row's conclusion: ask the QUESTION, not
/// the map -- `array_param_elem_is_callee_owned` over the variant's declared
/// payload element -- so the gate's two conjuncts now name "a param of this
/// function" and "whose element heap the caller keeps" separately.
///
/// AND NOTE WHICH LEG SAW IT. This cell was GREEN under a plain
/// `cargo test --features llvm`, at the default opt level, with the leak
/// present; only the `-O0` ASAN ratchet leg was red, because at `-O2` the
/// allocation nothing observes is deleted. A `valgrind -q` column in the same
/// grid had also called it clean, `-q` having suppressed the leak summary. An
/// invalid-access column and a leak column are different instruments.
///
/// THE OTHER THREE CONTROLS PIN WHAT MUST NOT MOVE, and were clean before the
/// fix as well as after. `b/noheap` owns no element heap
/// and so has nothing to double-free. `b/local` is the local-source
/// non-regression — the predicate is param-rooted precisely so this keeps
/// arming. `b/ctl` is a by-value callee that does nothing with the array, and is
/// the oracle for what ONE owner looks like: every fixed cell prints its
/// elements exactly once, as this one does.
///
/// `--interp` runs every caller-retained cell's element bodies TWICE and so
/// diverges from all three compiled surfaces, which agree with each other. It is
/// unchanged by this fix, which touches codegen only, and is the same remainder
/// B-2026-09-22-8 records — measured here on `b/fresh`, which had that
/// divergence before this change and still has it. That is why this fixture has
/// no interpreter twin.
#[test]
fn e2e_array_param_into_named_seeded_envelope_local_stays_with_caller() {
    let Some(out) = run_program(
        r#"struct S { tag: String }
impl Drop for S { fn drop(mut ref self) { println(f"  dS{self.tag}") } }
struct N { id: i64 }
impl Drop for N { fn drop(mut ref self) { println(f"  dN{self.id}") } }
fn s_bind(a: Array[S, 2]) -> i64 { let o = Option.Some(a); match o { Option.Some(v) => { println("  r"); return 1 }, Option.None => { println("  n"); return 0 } } }
fn s_wild(a: Array[S, 2]) -> i64 { let o = Option.Some(a); match o { Option.Some(_) => { println("  w"); return 1 }, Option.None => { println("  n"); return 0 } } }
fn s_read(a: Array[S, 2]) -> i64 { let o = Option.Some(a); match o { Option.Some(v) => { println(f"  r:{v[0].tag}"); return 1 }, Option.None => { println("  n"); return 0 } } }
fn s_iflet(a: Array[S, 2]) -> i64 { let o = Option.Some(a); if let Option.Some(v) = o { println("  r"); return 1 } else { return 0 } }
fn s_letelse(a: Array[S, 2]) -> i64 { let o = Option.Some(a); let Option.Some(v) = o else { return 0 }; println("  r"); return 1 }
fn s_whilelet(a: Array[S, 2]) -> i64 { let mut o = Option.Some(a); while let Option.Some(v) = o { println("  r"); o = Option.None; } return 1 }
fn s_res(a: Array[S, 2]) -> i64 { let o: Result[Array[S, 2], i64] = Result.Ok(a); match o { Result.Ok(v) => { println("  r"); return 1 }, Result.Err(e) => { println("  n"); return 0 } } }
fn s_three(a: Array[S, 3]) -> i64 { let o = Option.Some(a); match o { Option.Some(v) => { println("  r"); return 1 }, Option.None => { println("  n"); return 0 } } }
fn b_chain(a: Array[S, 2]) -> i64 { let o = Option.Some(a); let o2 = o; match o2 { Option.Some(v) => { println("  r"); return 1 }, Option.None => { println("  n"); return 0 } } }
fn b_noheap(a: Array[N, 2]) -> i64 { let o = Option.Some(a); match o { Option.Some(v) => { println("  r"); return 1 }, Option.None => { println("  n"); return 0 } } }
fn b_str(a: Array[String, 2]) -> i64 { let o = Option.Some(a); match o { Option.Some(v) => { println(f"  r:{v[0]}"); return 1 }, Option.None => { println("  n"); return 0 } } }
fn b_local() -> i64 { let a: Array[S, 2] = [S { tag: f"llllllll0" }, S { tag: f"llllllll1" }]; let o = Option.Some(a); match o { Option.Some(v) => { println("  r"); return 1 }, Option.None => { println("  n"); return 0 } } }
fn b_freshtemp(a: Array[S, 2]) -> i64 { match Option.Some(a) { Option.Some(v) => { println("  r"); return 1 }, Option.None => { println("  n"); return 0 } } }
fn b_ctl(a: Array[S, 2]) -> i64 { println("  r"); return 1 }
fn main() {
    println("s/bind");     { let a: Array[S, 2] = [S { tag: f"aaaaaaaa0" }, S { tag: f"aaaaaaaa1" }]; let z = s_bind(a); }
    println("s/wild");     { let a: Array[S, 2] = [S { tag: f"bbbbbbbb0" }, S { tag: f"bbbbbbbb1" }]; let z = s_wild(a); }
    println("s/read");     { let a: Array[S, 2] = [S { tag: f"cccccccc0" }, S { tag: f"cccccccc1" }]; let z = s_read(a); }
    println("s/iflet");    { let a: Array[S, 2] = [S { tag: f"dddddddd0" }, S { tag: f"dddddddd1" }]; let z = s_iflet(a); }
    println("s/letelse");  { let a: Array[S, 2] = [S { tag: f"eeeeeeee0" }, S { tag: f"eeeeeeee1" }]; let z = s_letelse(a); }
    println("s/whilelet"); { let a: Array[S, 2] = [S { tag: f"mmmmmmmm0" }, S { tag: f"mmmmmmmm1" }]; let z = s_whilelet(a); }
    println("s/res");      { let a: Array[S, 2] = [S { tag: f"pppppppp0" }, S { tag: f"pppppppp1" }]; let z = s_res(a); }
    println("s/three");    { let a: Array[S, 3] = [S { tag: f"ffffffff0" }, S { tag: f"ffffffff1" }, S { tag: f"ffffffff2" }]; let z = s_three(a); }
    println("b/chain");    { let a: Array[S, 2] = [S { tag: f"hhhhhhhh0" }, S { tag: f"hhhhhhhh1" }]; let z = b_chain(a); }
    println("b/noheap");   { let a: Array[N, 2] = [N { id: 2 }, N { id: 3 }]; let z = b_noheap(a); }
    println("b/str");      { let a: Array[String, 2] = [f"gggggggg0", f"gggggggg1"]; let z = b_str(a); }
    println("b/local");    { let z = b_local(); }
    println("b/fresh");    { let a: Array[S, 2] = [S { tag: f"kkkkkkkk0" }, S { tag: f"kkkkkkkk1" }]; let z = b_freshtemp(a); }
    println("b/ctl");      { let a: Array[S, 2] = [S { tag: f"jjjjjjjj0" }, S { tag: f"jjjjjjjj1" }]; let z = b_ctl(a); }
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "s/bind\n  r\n  dSaaaaaaaa0\n  dSaaaaaaaa1\ns/wild\n  w\n  dSbbbbbbbb0\n  dSbbbbbbbb1\ns/read\n  r:cccccccc0\n  dScccccccc0\n  dScccccccc1\ns/iflet\n  r\n  dSdddddddd0\n  dSdddddddd1\ns/letelse\n  r\n  dSeeeeeeee0\n  dSeeeeeeee1\ns/whilelet\n  r\n  dSmmmmmmmm0\n  dSmmmmmmmm1\ns/res\n  r\n  dSpppppppp0\n  dSpppppppp1\ns/three\n  r\n  dSffffffff0\n  dSffffffff1\n  dSffffffff2\nb/chain\n  r\n  dShhhhhhhh0\n  dShhhhhhhh1\nb/noheap\n  r\n  dN2\n  dN3\nb/str\n  r:gggggggg0\nb/local\n  r\n  dSllllllll0\n  dSllllllll1\nb/fresh\n  r\n  dSkkkkkkkk0\n  dSkkkkkkkk1\nb/ctl\n  r\n  dSjjjjjjjj0\n  dSjjjjjjjj1\nend\n", "got:\n{out}");
}
