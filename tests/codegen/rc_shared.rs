//! shared/RC values, boxing, heap payloads -- fixtures for `tests/codegen.rs`.
//!
//! Split out of `tests/codegen.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test codegen` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test codegen rc_shared::
//!
//! New fixtures about shared/RC values, boxing, heap payloads belong in this file.

use super::*;

#[test]
fn e2e_generic_struct_heap_field_move_out_no_double_free() {
    // B-2026-07-18-44: a GENERIC struct's owned-by-value param/self whose
    // heap field is returned (moved out) double-freed under AOT/JIT — the
    // monomorph analogue of the non-generic B-2026-07-18-37. Two root gaps:
    // (1) the field move-out cap-zero GEP'd the generic base struct type
    // (erased placeholder fields) instead of the concrete mono layout, and
    // the receiver-type guard rejected the mono slot type; (2) a generic
    // method monomorph records `type_subst_names["T"] = "str"` (vs the
    // free-fn's "String"), which `field_copy_supported` didn't recognize, so
    // `self` was never entry-copied and stayed a caller-retains alias.
    // Fixed by making the cap-zero mono-aware and treating "str" as "String".
    // Covers a free fn (`b.v`) and a method (`self.v`), single- and
    // two-field generic structs, at T=String.
    if let Some(out) = run_program(
        "struct Box[T] { v: T }\n\
             struct Box2[T] { v: T, n: i64 }\n\
             fn take[T](b: Box[T]) -> T { b.v }\n\
             impl[T] Box[T] { fn get(self) -> T { self.v } }\n\
             impl[T] Box2[T] { fn get(self) -> T { self.v } }\n\
             fn main() {\n\
                 println(take(Box { v: \"a\".to_string() }));\n\
                 let b = Box { v: \"b\".to_string() };\n\
                 println(b.get());\n\
                 let b2 = Box2 { v: \"c\".to_string(), n: 1 };\n\
                 println(b2.get());\n\
             }",
    ) {
        assert_eq!(out, "a\nb\nc\n");
    }
}

#[test]
fn test_e2e_ref_eq_shared_identity() {
    // `ref_eq` (design.md § Equality Semantics): reference identity of two
    // `shared` handles → `icmp eq` on the heap pointers. `b = a` aliases the
    // same allocation (true); a separately-built `c` is a distinct alloc
    // (false). Parity with interpreter::test_ref_eq_shared_struct_identity.
    let out = run_program(
        "shared struct N { v: i64 }\n\
             fn main() {\n\
                 let a = N { v: 1 };\n\
                 let b = a;\n\
                 let c = N { v: 1 };\n\
                 println(ref_eq(a, b));\n\
                 println(ref_eq(a, c));\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "true\nfalse");
    }
}

/// B-2026-08-07-8 — a `shared struct` receiver handed to a free function.
///
/// Codegen had never seen these programs: the typechecker rejected every
/// receiver/parameter combination, because the impl's `self_type` rebuilt
/// the correctly-lowered `Type::Shared(S)` as the `Named` form and the two
/// are deliberately distinct types (they merely RENDER the same, which is
/// why the error read "expected 'Inner', found 'Inner'"). So this is the
/// first coverage of the lowering, not a regression guard on it.
///
/// Each callee scales by a different factor so a wrong argument or a
/// mis-taken borrow shows up as a wrong SUM rather than a crash, and the
/// seed is `env.args().len()` so nothing folds to a constant. `ref` and
/// by-value both appear, and the `frozen` callee is reached from both a
/// `ref self` and a `frozen self` receiver.
#[test]
fn e2e_shared_receiver_passed_to_free_function() {
    if let Some(out) = run_program(
            "shared struct Inner { v: i64 }\n             fn byref(i: ref Inner) -> i64 { i.v * 2 }\n             fn byval(i: Inner) -> i64 { i.v * 3 }\n             fn frz(i: frozen Inner) -> i64 { i.v * 5 }\n             impl Inner {\n             \x20   fn a(ref self) -> i64 { byref(self) }\n             \x20   fn b(self) -> i64 { byval(self) }\n             \x20   fn c(ref self) -> i64 { frz(self) }\n             \x20   fn d(frozen self) -> i64 { frz(self) }\n             \x20   fn me(ref self) -> Inner { self }\n             }\n             fn main() {\n             \x20   let n = env.args().len() as i64;\n             \x20   let x = Inner { v: n };\n             \x20   println(x.a() + x.b() + x.c() + x.d());\n             \x20   println(x.me().v);\n             }\n",
        ) {
            // n = 1: 1*2 + 1*3 + 1*5 + 1*5 = 15, then the returned self's v.
            assert_eq!(out, "15\n1\n");
        }
}

/// B-2026-09-03-9 — the compiled half of the field-held `shared struct`
/// release, which is what the interpreter fix had to match.
///
/// Three things are pinned here, and only the first is about the body
/// running at all:
///
///   1. A `shared struct` in a struct FIELD runs its body exactly once.
///      (The interpreter ran it ZERO times; this side always ran it.)
///   2. WHERE it runs. `Mx { r: R, s: S }` used to SPLIT — the plain
///      field's body at the binding's live-range end, the shared field's
///      release at SCOPE EXIT — and B-2026-09-04-32 collapsed the two onto
///      the live-range end, on BOTH backends in one commit. The split was
///      never a backend divergence (all four surfaces agreed) but a joint
///      departure from design.md § Drop ordering within a branch, which
///      names RC decrements explicitly — "Destructor calls — including
///      `Rc` and `Arc` reference-count decrements — ... fire at each
///      binding's live-range end, not lexical scope end" — and excludes
///      the scope-exit stack outright for a mid-branch last use. ONE
///      BINDING CANNOT HAVE TWO LIVE-RANGE ENDS, and a BARE `shared`
///      binding was already correct, which made the aggregate the outlier
///      rather than the rule. `dR1` then `dS2`, both before `two`.
///
///      Scoped to PLAIN STRUCT holders, which is exactly what codegen can
///      pair (the holder's `UserDrop` with its `StructDrop`, same alloca).
///      A `Vec[S]` / tuple holder releases through a different action that
///      still drains at scope exit on both backends, so those shapes are
///      unchanged and still agree — moving one backend alone would trade a
///      shared spec deviation for a run-vs-build divergence.
///   3. Two holders of ONE shared value release it once, at the last one.
///
/// Kept in one program so the three answers are read off a single output
/// and cannot drift apart across separate fixtures.
#[test]
fn e2e_field_held_shared_struct_release_placement() {
    let Some(out) = run_program(
            "shared struct S { id: i64 }\n\
             impl Drop for S { fn drop(mut ref self) { println(f\"dS{self.id}\"); } }\n\
             struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\"); } }\n\
             struct Sw { s: S }\n\
             struct Mx { r: R, s: S }\n\
             fn build(k: i64) -> Sw {\n\
             \x20   let s: S = S { id: k };\n\
             \x20   return Sw { s: s }\n\
             }\n\
             fn main() {\n\
             \x20   { let a: Sw = build(14); println(f\"v{a.s.id}\"); println(\"one\"); }\n\
             \x20   { let m: Mx = Mx { r: R { id: 1 }, s: S { id: 2 } }; println(f\"v{m.s.id}\"); println(\"two\"); }\n\
             \x20   { let s: S = S { id: 3 }; let w1: Sw = Sw { s: s }; let w2: Sw = Sw { s: s }; println(f\"v{w1.s.id}{w2.s.id}\"); println(\"three\"); }\n\
             \x20   println(\"end\");\n\
             }\n",
        ) else {
            return;
        };
    assert_eq!(
        out,
        // Block 1: release at scope exit, after `one` — NOT at `a`'s last
        // use. Block 2: `dR1` at the NLL endpoint, `dS2` at scope exit,
        // with `two` between them. Block 3: exactly one `dS3`, at the last
        // holder's release.
        "v14\none\ndS14\nv2\ndR1\ndS2\ntwo\nv33\nthree\ndS3\nend\n"
    );
}

/// B-2026-09-04-31 — a `shared struct` in a TUPLE ELEMENT is released at
/// the tuple binding's SCOPE EXIT, once, and every read before that sees
/// the live value.
///
/// Before: the read `a.0.id` fell through `compile_field_access` to the
/// B-2026-08-28-7 fresh-temporary receiver, which released the element
/// after loading the field — the first read printed `14` out of a box
/// whose `Drop` body had already run, and the second read printed
/// garbage (valgrind: `Invalid read of size 8`). Nothing released the
/// element otherwise: `tuple_elem_needs_deep_drop` did not admit a shared
/// element, so a never-read `(S, i64)` leaked 40 B at `-O0` with no body.
/// Two reads and a trailing `post` pin both halves.
#[test]
fn e2e_tuple_held_shared_struct_read_then_release_at_scope_exit() {
    let Some(out) = run_program(
            "shared struct S { id: i64, tag: String }\n\
             impl Drop for S { fn drop(mut ref self) { println(f\"dS{self.id}/{self.tag}\"); } }\n\
             fn main() {\n\
             \x20   { let a: (S, i64) = (S { id: 14, tag: \"alpha\" }, 3); println(f\"r1={a.0.id}\"); println(f\"r2={a.0.tag}\"); println(\"one\"); }\n\
             \x20   { let b: (S, i64) = (S { id: 15, tag: \"beta\" }, 3); println(\"two\"); }\n\
             \x20   println(\"end\");\n\
             }\n",
        ) else {
            return;
        };
    assert_eq!(
        out,
        "r1=14\nr2=alpha\none\ndS14/alpha\ntwo\ndS15/beta\nend\n"
    );
}

/// B-2026-09-04-31 — the move-out matrix for a tuple's shared element. The
/// element's scope-exit release is new, so every owning destination must
/// hold a ref of its own or the two decs land on one:
///
///   1. `let s = a.0` — an alias (+1), the tuple keeps its slot.
///   2. `let (s, n) = a` — a destructure; the bit-copy owns nothing and
///      the tuple stays the single owner.
///   3. `return a.0` / tail `a.0` off a by-value tuple PARAM — the param
///      is caller-retains (`make_tuple_param_callee_owned` bails on a
///      shared leaf), so the returned handle is an alias and incs.
///   4. `H { s: a.0 }` — an owned struct literal takes its own ref.
///   5. `let p = w.p` — a whole tuple moved out of a struct field nulls
///      the source's slot.
///
/// 3, 4 and 5 were each a use-after-free in the first draft of the fix
/// (measured `Invalid read of size 8`); the sanitizer twin asserts the
/// memory side, this one pins the observable order: every body exactly
/// once, after the block's last print.
#[test]
fn e2e_tuple_held_shared_struct_move_outs_fire_once() {
    let Some(out) = run_program(
            "shared struct S { id: i64 }\n\
             impl Drop for S { fn drop(mut ref self) { println(f\"dS{self.id}\"); } }\n\
             struct H { s: S }\n\
             struct W { p: (S, i64) }\n\
             fn pick(a: (S, i64)) -> S { return a.0 }\n\
             fn pick2(a: (S, i64)) -> S { a.0 }\n\
             fn main() {\n\
             \x20   { let a: (S, i64) = (S { id: 1 }, 3); let s: S = a.0; println(f\"v{s.id}{a.0.id}\"); println(\"one\"); }\n\
             \x20   { let a: (S, i64) = (S { id: 2 }, 3); let (s, n) = a; println(f\"v{s.id}{n}\"); println(\"two\"); }\n\
             \x20   { let a: (S, i64) = (S { id: 3 }, 3); let s: S = pick(a); println(f\"v{s.id}\"); println(\"three\"); }\n\
             \x20   { let a: (S, i64) = (S { id: 4 }, 3); let s: S = pick2(a); println(f\"v{s.id}\"); println(\"four\"); }\n\
             \x20   { let a: (S, i64) = (S { id: 5 }, 3); let h: H = H { s: a.0 }; println(f\"v{h.s.id}{a.0.id}\"); println(\"five\"); }\n\
             \x20   { let w: W = W { p: (S { id: 6 }, 3) }; let p: (S, i64) = w.p; println(f\"v{p.0.id}\"); println(\"six\"); }\n\
             \x20   println(\"end\");\n\
             }\n",
        ) else {
            return;
        };
    assert_eq!(
            out,
            "v11\none\ndS1\nv23\ntwo\ndS2\nv3\nthree\ndS3\nv4\nfour\ndS4\nv55\nfive\ndS5\nv6\nsix\ndS6\nend\n"
        );
}

/// B-2026-08-05-41, codegen half — the SHARED-receiver case B-2026-08-05-37
/// left behind.
///
/// That fix taught the `mut ref` argument path to pass a pointer to the
/// PLACE, but bailed explicitly on a `shared` / `par` struct receiver: its
/// slot holds an RC HANDLE, not the aggregate, so GEPing it as the struct
/// would have written past the 8-byte slot. The bail was safe and wrong —
/// the argument fell back to the rvalue path, the callee's write landed on
/// a shallow COPY, and every arm below printed the PRE-call value under
/// both AOT and JIT while the INTERPRETER printed the post-call one. A
/// run/build divergence with no diagnostic at any phase.
///
/// The place is one load in, at the header-shifted offset, so the arm now
/// resolves it through `shared_gep_layout` — the same funnel every other
/// shared field site uses, which is what keeps the headed / headerless /
/// weak-headed layouts from mixing on one object.
///
/// Arms (a)–(d) are the witnesses, each a receiver shape that reaches the
/// arm differently: a bare shared local, a shared `ref` PARAMETER (whose
/// slot needs the extra deref), `self` inside a shared method, and a
/// NESTED shared chain (`o.inner.v` — two handle loads). All four printed
/// 7 against the unfixed compiler, measured.
///
/// Arm (e) is a CONTROL, not a witness: a String field was already correct
/// before this fix (it prints `ab:2` either way), because an AGGREGATE
/// `mut ref` reassignment reaches the caller's storage through
/// B-2026-08-05-39's path rather than through the argument pointer. It is
/// kept because the fix newly routes it through the place pointer instead,
/// and a heap payload must keep reclaiming exactly once on that path —
/// the leak leg is `asan_shared_field_mut_ref_arg_string_no_leak`.
///
/// The mutability gate is the typechecker's half of the same row and is
/// covered in `tests/typechecker.rs`: every field written here is declared
/// `mut`, which is what makes these programs legal at all.
///
/// Seeded from `env.args().len()` so no arm folds away at `-O2`
/// (B-2026-08-04-17).
#[test]
fn e2e_mut_ref_place_argument_shared_receiver_writes_back() {
    let Some(out) = run_program(
        "shared struct N { mut val: i64 }\n\
             shared struct T { mut s: String }\n\
             shared struct Inner { mut v: i64 }\n\
             shared struct Outer { mut inner: Inner }\n\
             impl N {\n\
             \x20   fn go(ref self) { bump(mut self.val); }\n\
             }\n\
             fn bump(x: mut ref i64) { x = x + 1i64; }\n\
             fn app(x: mut ref String) { x = x + \"b\"; }\n\
             fn viaref(n: ref N) -> i64 { bump(mut n.val); return n.val; }\n\
             fn main() {\n\
             \x20   let n: i64 = env.args().len();\n\
             \x20   // (a) bare shared local receiver\n\
             \x20   let a = N { val: n + 6i64 };\n\
             \x20   bump(mut a.val);\n\
             \x20   println(f\"a:{a.val}\");\n\
             \x20   // (b) shared `ref` PARAMETER receiver — the slot holds the\n\
             \x20   //     borrow, so the handle is one extra load in\n\
             \x20   let b = N { val: n + 6i64 };\n\
             \x20   println(f\"b:{viaref(b)}\");\n\
             \x20   // (c) `self` inside a shared method\n\
             \x20   let c = N { val: n + 6i64 };\n\
             \x20   c.go();\n\
             \x20   println(f\"c:{c.val}\");\n\
             \x20   // (d) NESTED shared chain — two handle loads\n\
             \x20   let di = Inner { v: n + 6i64 };\n\
             \x20   let d = Outer { inner: di };\n\
             \x20   bump(mut d.inner.v);\n\
             \x20   println(f\"d:{d.inner.v}\");\n\
             \x20   // (e) HEAP payload — the displaced old value reclaims\n\
             \x20   //     through a different path than a scalar\n\
             \x20   let e = T { s: \"a\" };\n\
             \x20   app(mut e.s);\n\
             \x20   println(f\"e:{e.s}:{e.s.len()}\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "a:8\nb:8\nc:8\nd:8\ne:ab:2\n");
}

/// B-2026-08-25-7 — a generic impl method that REBINDS its owned receiver
/// (`let mut h = self`) and then calls a sibling through it.
///
/// `self` parses as `ExprKind::SelfValue`, not `Identifier("self")`, so the
/// `let` site recorded no generic instantiation for `h`. The sibling call
/// then could not recover the impl's type args and was lowered against the
/// UNMANGLED default prototype (`Heap.pop_one`, built at the all-`i64` base
/// layout) instead of the `T = String` monomorph — reading a 24-byte String
/// control block as an i64. The Vec length still decremented, so the drain
/// loop terminated with the right COUNT and every element came back empty.
///
/// The two controls are what make the assertion non-vacuous, because both
/// were already correct pre-fix and pin the two halves of the diagnosis:
/// (c) a SCALAR `T`, correct only because the default layout coincides with
/// `i64`, and (d) the identical shape on a NON-generic impl at the same
/// heap element type, which never went through the monomorphizer at all.
/// Case (a) is the minimal single-call form — the filed repro's drain loop
/// in (b) is an amplifier, not a trigger.
///
/// Verified RED against the pre-fix compiler: (a) and (b) printed empty
/// lines while (c) and (d) printed correctly.
///
/// The element type is deliberately `String` and not `Vec[i64]`: a
/// nested-heap element reaches a SEPARATE, pre-existing double-free in the
/// same rebinding shape (present identically before this fix, so not a
/// regression from it; B-2026-08-25-10), which would make this test fail
/// for an unrelated reason.
#[test]
fn e2e_generic_impl_rebinding_owned_self_drains_through_sibling_at_heap_t() {
    let src = r#"
struct Heap[T] { xs: Vec[T] }
impl[T] Heap[T] {
    fn pop_one(mut ref self) -> Option[T] { self.xs.pop() }
    fn take_one(self) -> Option[T] {
        let mut h = self;
        h.pop_one()
    }
    fn into_sorted(self) -> Vec[T] {
        let mut h = self;
        let mut out: Vec[T] = Vec.new();
        while h.xs.len() > 0 {
            match h.pop_one() { Some(v) => { out.push(v); } None => {} }
        }
        out
    }
}
struct PlainHeap { xs: Vec[String] }
impl PlainHeap {
    fn pop_one(mut ref self) -> Option[String] { self.xs.pop() }
    fn take_one(self) -> Option[String] { let mut h = self; h.pop_one() }
}
fn main() {
    // (a) minimal: ONE sibling call through the rebound owned receiver.
    let a = Heap { xs: ["a", "bb", "ccc"] };
    match a.take_one() { Some(v) => { println(v); } None => {} }
    // (b) the filed shape — assert CONTENTS, not just the count: the count was
    // right even while every element was empty.
    let b = Heap { xs: ["a", "bb", "ccc"] };
    let o = b.into_sorted();
    println(o.len());
    let mut i = 0;
    while i < o.len() { println(o[i]); i = i + 1; }
    // (c) scalar control: correct pre-fix, because the base layout is i64.
    let c = Heap { xs: [1, 2, 3] };
    match c.take_one() { Some(v) => { println(v); } None => {} }
    // (d) NON-generic control at the same heap element type.
    let d = PlainHeap { xs: ["p", "qq"] };
    match d.take_one() { Some(v) => { println(v); } None => {} }
}
"#;
    assert_eq!(
        run_program(src).as_deref(),
        Some("ccc\n3\nccc\nbb\na\n3\nqq\n")
    );
}

/// B-2026-08-06-1 — a generic wrapper's bare-`T` field bound to a `Map` /
/// `Set` leaked its whole handle tree, because the drop classifier and the
/// move neutralizer disagreed about what the field IS.
///
/// `emit_struct_drop_synthesis_impl`'s subst-driven rescue loop promoted a
/// bare generic-param field only when it resolved to a String / Vec /
/// VecDeque head, so a `Map` head stayed `FieldDrop::None` and nothing ever
/// freed it: `fn sink(b: Box[Map[i64, String]])` lost 25,830 bytes over 40
/// rounds on a default -O2 build, identically at -O0. The concrete twin
/// `Gmap[T] { m: Map[i64, T] }` was always clean — its DECLARED field type
/// is spelled `Map`, so the name-based classifier never needed the subst.
///
/// Adding the Map/Set arm alone is what the row warned would trade a leak
/// for a double free, and it did. Both neutralizers classify a field by its
/// declared type name, which for a bare param is the erased `T`:
/// `zero_struct_field_move_cap` matched no arm (so a moved-out field left a
/// live handle) and `zero_struct_move_caps_mono` rescued only the
/// Vec/String heads (so `let c = b;` left one too). Both now resolve the
/// bare param the same way the drop synthesizer does — through the source
/// binding's recorded instantiation — which is the property that actually
/// matters: the free list and the neutralize list must agree.
///
/// Eleven shapes in one program, because they reach those two helpers by
/// different routes: read via a by-value param, read via a plain local
/// (so this is NOT param-specific), read as the MID field of a multi-field
/// wrapper, the field RETURNED out of a by-value param, moved to a local,
/// passed to a consuming callee, a WHOLE-struct move, a `ref`-param read, a
/// `Set` instantiation, a struct-pattern destructure, and a control that
/// only reads the fields of a struct it never moves — that last one turns
/// red if a fix over-nulls and the owner stops freeing.
///
/// Expected total is DERIVED, not read off a run: `mk` builds a 2-entry map
/// and `mkset` a 1-entry set, so a round is
/// 2+2+(2+3+1)+2+2+2+2+2+1+2+(1+2) = 26, and 26 x 40 = 1040.
/// B-2026-08-06-14 — a direct `shared` field handed out of a CALLER-RETAINS
/// struct param, which was a use-after-free on a DEFAULT -O2 build.
///
/// AN E2E TEST ON PURPOSE, and the reason is worth recording: the ASAN
/// harness CANNOT see this defect. It links a karac-emitted object that
/// carries no ASAN instrumentation, so the sanitizer catches allocator-level
/// faults (double free, invalid free, leaks) but not a use-after-free READ
/// out of generated code. An ASAN fixture over this program passes against
/// the broken compiler — written, measured green at HEAD, and discarded.
/// At scale the premature free corrupts the glibc heap instead, which the
/// plain run does catch: HEAD aborts with `malloc(): unaligned tcache chunk
/// detected` on every run, deterministically.
///
/// The regime is what the fix turns on. A by-value struct param that
/// transitively owns a `shared` field stays CALLER-RETAINS
/// (B-2026-08-05-32) — the callee deliberately registers no drop — so the
/// caller's +1 is still live and a field handed back out is an ALIAS that
/// needs its own ref. A struct owning no shared field is owned BY TRANSFER
/// instead, and there the source null-store is correct and an inc would
/// leak. Both regimes are exercised so a fix that incs in the wrong one
/// shows up.
///
/// Covers both return spellings deliberately: `return b.v;` and the tail
/// `{ b.v }` reach the return value through paths that diverge at
/// `compile_tail_final_expr`'s `tail_inner` gate, and the tail form stayed
/// broken after the return form was fixed.
///
/// Expected total is DERIVED: `mk` builds a 43-byte payload, `score` adds 1
/// for the `contains` hit, so legs (a)-(d) contribute 44 each; (e) adds 1
/// and (f) adds its 38-byte plain string. 40 x (44 x 4 + 1 + 38) = 9350.
#[test]
fn e2e_shared_field_returned_from_a_caller_retains_param_keeps_its_ref() {
    let Some(out) = run_program(
        r#"shared struct Node { s: String }
struct Holder { v: Node }
struct Plain { t: String }
struct Box[T] { v: T }

fn mk(i: i64, n: i64) -> Node {
    return Node { s: f"crp-{i}-padded-out-to-force-a-real-heap-buffer-{n}" };
}

fn score(x: Node) -> i64 {
    let mut r: i64 = x.s.len();
    if x.s.contains("padded") { r = r + 1i64; }
    return r;
}

// CALLER-RETAINS regime: `Holder` transitively owns a shared field, so the
// callee registers no drop and the caller keeps its ref.
fn ret_byval(b: Holder) -> Node { return b.v; }
fn tail_byval(b: Holder) -> Node { b.v }
fn ret_ref(b: ref Holder) -> Node { return b.v; }

// OWNED-BY-TRANSFER regime, the control an over-inc would turn into a leak:
// `Box[Node]`'s gate is name-only, so it is owned by transfer and neutralized
// via the source null-store instead.
fn ret_generic(b: Box[Node]) -> Node { return b.v; }

// A no-shared struct on the transfer path, unrelated to rc accounting.
fn ret_plain(p: Plain) -> String { return p.t; }

fn main() {
    let n: i64 = env.args().len();
    let mut acc: i64 = 0;
    let mut i: i64 = 0;
    while i < 40i64 {
        // (a) by-value param, explicit return — the reported shape
        let h1 = Holder { v: mk(i, n) };
        let x1 = ret_byval(h1);
        acc = acc + score(x1);
        // (b) by-value param, TAIL spelling — a separate code path
        let h2 = Holder { v: mk(i, n) };
        let x2 = tail_byval(h2);
        acc = acc + score(x2);
        // (c) `ref` param — caller-retains by definition, and it reproduced too
        let h3 = Holder { v: mk(i, n) };
        let x3 = ret_ref(h3);
        acc = acc + score(x3);
        // (d) CONTROL, owned-by-transfer: an inc here would LEAK
        let b4 = Box { v: mk(i, n) };
        let x4 = ret_generic(b4);
        acc = acc + score(x4);
        // (e) CONTROL, a caller-retains struct whose field is NEVER handed out
        let h5 = Holder { v: mk(i, n) };
        acc = acc + 1i64;
        // (f) CONTROL, a plain heap field on the transfer path
        let p6 = Plain { t: f"plain-{i}-padded-out-to-force-a-real-heap-{n}" };
        let s6 = ret_plain(p6);
        acc = acc + s6.len();
        i = i + 1i64;
    }
    println(acc);
}
"#,
    ) else {
        return;
    };
    assert_eq!(out.trim(), "9350");
}

/// B-2026-08-06-8 — the CORRECTNESS half of the bare-`T` shared-field fix.
/// The defect itself is a leak, invisible here; what this pins is the
/// use-after-free that fixing it exposes, which prints a WRONG ANSWER.
///
/// Once a struct local's drop rc-dec's its `shared` fields, a field moved
/// out of a value-position BLOCK (`let x = { let b = mk(); b.v };`) is
/// dec'd by the block frame on the way out — the consumer receives a
/// freed box. `suppress_block_tail_cleanup` neutralizes an Identifier tail
/// but fell through on a FIELD tail, while its function-body sibling
/// `suppress_cleanup_for_tail_return` handles any shape.
///
/// The CONCRETE spelling is the load-bearing case and the reason this test
/// is worth having: `Holder { v: Node }` has always taken the combined
/// drop, so it reproduced at HEAD with no generics involved — printing 0
/// instead of 38 on a default -O2 build, with 160 valgrind errors. The
/// generic spelling is included beside it because the fix routes it onto
/// exactly that path.
///
/// Expected total is DERIVED, not read off a run: the payload string is 38
/// bytes, each of the four legs contributes one length, so 38 x 4 = 152.
#[test]
fn e2e_shared_field_moved_out_of_a_value_block_survives_the_frame_drain() {
    let Some(out) = run_program(
        r#"shared struct Node { s: String }
struct Box[T] { v: T }
struct Holder { v: Node }

fn main() {
    let mut acc: i64 = 0;
    // (a) GENERIC wrapper, field escaping a value-position block.
    let x1 = { let b = Box { v: Node { s: "padded-out-to-force-a-real-heap-buffer" } }; b.v };
    acc = acc + x1.s.len();
    // (b) CONCRETE wrapper, same shape — broken at HEAD independently of
    // generics, which is what makes this more than a companion case.
    let x2 = { let h = Holder { v: Node { s: "padded-out-to-force-a-real-heap-buffer" } }; h.v };
    acc = acc + x2.s.len();
    // (c) CONTROL, the non-block move-out that was already correct.
    let b3 = Box { v: Node { s: "padded-out-to-force-a-real-heap-buffer" } };
    let x3 = b3.v;
    acc = acc + x3.s.len();
    // (d) CONTROL, the concrete non-block move-out.
    let h4 = Holder { v: Node { s: "padded-out-to-force-a-real-heap-buffer" } };
    let x4 = h4.v;
    acc = acc + x4.s.len();
    println(acc);
}
"#,
    ) else {
        return;
    };
    assert_eq!(out.trim(), "152");
}

/// B-2026-07-31-37 (sibling filing; heap face) — `a = b` on a struct
/// whose Drop body reads a heap field must fire that body ONCE with the
/// real length. Pre-fix the moved source's UserDrop action stayed armed
/// after `zero_struct_move_caps` zeroed its caps, so the body ran twice
/// — and the first firing read a ZEROED length under AOT (`D0 D7`) while
/// the interpreter printed `D7 D7`: a run-vs-build divergence on top of
/// the double body. Now: `D3` (the displaced old value, the
/// B-2026-07-30-11 leg) then `D7`, once, everywhere.
///
/// Twin of `tests/interpreter.rs`'s `test_struct_assign_move_heap_body_once`.
#[test]
fn e2e_struct_assign_move_heap_body_once() {
    let Some(out) = run_program(
        "struct Res { data: Vec[i64] }\n\
             impl Drop for Res {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"D{self.data.len()}\")\n\
             \x20   }\n\
             }\n\
             fn mk(n: i64) -> Res {\n\
             \x20   let mut v: Vec[i64] = Vec.new();\n\
             \x20   let mut i = 0;\n\
             \x20   while i < n {\n\
             \x20       v.push(i);\n\
             \x20       i = i + 1;\n\
             \x20   }\n\
             \x20   Res { data: v }\n\
             }\n\
             fn main() {\n\
             \x20   let mut a = mk(3);\n\
             \x20   let b = mk(7);\n\
             \x20   a = b;\n\
             \x20   println(\"x\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "D3\nD7\nx\n");
}

/// B-2026-08-27-44 — a heap-bearing TUPLE argument whose value ESCAPES the
/// caller's frame. The callee entry-copies the tuple param and the COPY is
/// what flows onward, so the caller's ORIGINAL is orphaned; unfixed, each
/// of these leaked (48 bytes for the `Bag` shapes, 3 for the bare `String`,
/// 72 over two blocks for the double-call).
///
/// Two independent gaps, which is why the shapes come in pairs. The
/// PASSTHRU shapes never reached the caller-side registrar at all: the
/// admission test had entry-copy siblings for a struct and an enum argument
/// and none for a tuple. The CALL-TEMPORARY shapes did reach it and then
/// matched no arm, because the tuple arm tested `ExprKind::Tuple` — a
/// literal — while a call that RETURNS a tuple is an `ExprKind::Call` whose
/// return type has no name for the named-struct arm to key on.
///
/// Output correctness is the weaker half of this test; `tests/memory_sanitizer.rs`
/// carries the LSan twin that actually sees the leak. What this pins is that
/// adding the caller-side free did not disturb the VALUE — an over-free
/// would corrupt these strings rather than merely leak.
#[test]
fn e2e_heap_tuple_arg_escaping_the_frame_is_freed_once() {
    // The param returned whole, generic and non-generic.
    let generic_passthru = "struct Bag[T] { xs: Vec[T] }\n\
             fn passthru[T](p: (Bag[T], i64)) -> (Bag[T], i64) { p }\n\
             fn main() { let (b, n) = passthru((Bag { xs: [\"x\", \"y\"] }, 7)); println(f\"{n} {b.xs.len()} {b.xs[0]}\"); }\n";
    assert_eq!(run_program(generic_passthru).as_deref(), Some("7 2 x\n"));
    let plain_passthru = "struct Bag { xs: Vec[String] }\n\
             fn passthru(p: (Bag, i64)) -> (Bag, i64) { p }\n\
             fn main() { let (b, n) = passthru((Bag { xs: [\"x\", \"y\"] }, 7)); println(f\"{n} {b.xs.len()} {b.xs[0]}\"); }\n";
    assert_eq!(run_program(plain_passthru).as_deref(), Some("7 2 x\n"));
    // A call temporary as the argument, generic and non-generic.
    let generic_temp = "struct Bag[T] { xs: Vec[T] }\n\
             fn mk[T](v: Vec[T]) -> (Bag[T], i64) { (Bag { xs: v }, 3) }\n\
             fn use2[T](p: (Bag[T], i64)) -> i64 { let (b, n) = p; b.xs.len() + n }\n\
             fn main() { println(f\"{use2(mk([\"x\", \"y\"]))}\"); }\n";
    assert_eq!(run_program(generic_temp).as_deref(), Some("5\n"));
    let plain_temp = "struct Bag { xs: Vec[String] }\n\
             fn mk(v: Vec[String]) -> (Bag, i64) { (Bag { xs: v }, 3) }\n\
             fn use2(p: (Bag, i64)) -> i64 { let (b, n) = p; b.xs.len() + n }\n\
             fn main() { println(f\"{use2(mk([\"x\"])) + use2(mk([\"y\", \"z\"]))}\"); }\n";
    assert_eq!(run_program(plain_temp).as_deref(), Some("9\n"));
    // A bare `String` element — the predicate keys on drop-bearing heap,
    // not on a named struct type.
    let string_elem = "fn passthru(p: (String, i64)) -> (String, i64) { p }\n\
             fn main() { let (s, n) = passthru((f\"abc\", 7)); println(f\"{n} {s}\"); }\n";
    assert_eq!(run_program(string_elem).as_deref(), Some("7 abc\n"));
}

/// B-2026-09-15-22 — a `VecDeque` built as a NESTED SEQUENCE-LITERAL
/// element must lower to a real `{ptr, len, cap}` heap handle, not to the
/// fixed `[N x T]` aggregate.
///
/// `lowering.rs` canonicalizes a bare `[..]` whose recorded type is a
/// growable sequence into the `Vec[..]` PREFIX form, because codegen's
/// `compile_array_literal` always emits the fixed aggregate and that is
/// the wrong shape for a heap handle. The gate read `name == "Vec"`, so a
/// `VecDeque`-recorded literal was left alone and codegen wrote an 8-byte
/// `[1 x i64]` into a 24-byte `[1 x {ptr,len,cap}]` slot — `len` and `cap`
/// left as uninitialized stack. The scope-exit drop then walked a garbage
/// length (HUNG at N = 1) or freed a garbage pointer (SIGSEGV at N = 2),
/// in both cases AFTER printing the program's correct output. The gate's
/// own comment had already predicted it: "a segfault on the array bytes
/// read as a Vec header".
///
/// Only a NESTED literal was affected — an annotated `let` builds the
/// handle in its own arm, which is why `let v: VecDeque[i64] = [1, 2, 3];`
/// was always correct and why this survived undetected.
///
/// Every cell here is a program that HUNG or CRASHED before the fix, so
/// this test asserting anything at all is the regression guard; the
/// `Vec`-element and `VecDeque.new()`-element cells are the controls that
/// were already correct (the latter passed only because a null handle
/// makes a spurious free a no-op).
#[test]
fn e2e_nested_vecdeque_literal_lowers_to_a_heap_handle() {
    for (label, body, want) in [
            // Hung before the fix: garbage `len` walked at scope exit.
            (
                "array-of-deque-n1",
                "fn main() { let v: Array[VecDeque[i64], 1] = [[1]];\n                 \x20            println(f\"x:{v[0].len()}\"); }\n",
                "x:1\n",
            ),
            // SIGSEGV before the fix: garbage pointer freed at scope exit.
            (
                "array-of-deque-n2",
                "fn main() { let v: Array[VecDeque[i64], 2] = [[1], [2]];\n                 \x20            println(f\"a:{v[0].len()} b:{v[1].len()}\"); }\n",
                "a:1 b:1\n",
            ),
            // No read at all still faulted — the `let` alone was enough.
            (
                "array-of-deque-no-read",
                "fn main() { let v: Array[VecDeque[i64], 1] = [[1]]; println(\"ok\"); }\n",
                "ok\n",
            ),
            // The `Vec`-outer nests, which the typechecker declined until this
            // fix landed (B-2026-09-14-24's decline, since lifted).
            (
                "vec-of-deque",
                "fn main() { let v: Vec[VecDeque[i64]] = [[1], [2]];\n                 \x20            println(f\"n:{v.len()} x:{v[0].len()}\"); }\n",
                "n:2 x:1\n",
            ),
            (
                "deque-of-vec",
                "fn main() { let v: VecDeque[Vec[i64]] = [[1], [2]];\n                 \x20            println(f\"n:{v.len()} x:{v[0].len()}\"); }\n",
                "n:2 x:1\n",
            ),
            (
                "deque-outer-over-array",
                "fn main() { let v: VecDeque[Array[i64, 2]] = [[1, 2]];\n                 \x20            println(f\"n:{v.len()}\"); }\n",
                "n:1\n",
            ),
            // Controls that were already correct.
            (
                "control-vec-element",
                "fn main() { let v: Array[Vec[i64], 1] = [[1]];\n                 \x20            println(f\"x:{v[0].len()}\"); }\n",
                "x:1\n",
            ),
            (
                "control-deque-new-element",
                "fn main() { let v: Array[VecDeque[i64], 1] = [VecDeque.new()];\n                 \x20            println(f\"x:{v[0].len()}\"); }\n",
                "x:0\n",
            ),
            (
                "control-flat-deque",
                "fn main() { let v: VecDeque[i64] = [1, 2, 3];\n                 \x20            println(f\"n:{v.len()}\"); }\n",
                "n:3\n",
            ),
        ] {
            assert_eq!(run_program(body).as_deref(), Some(want), "{label}");
        }
}

/// Phase-11 `PriorityQueue[T: Ord]` — the first stdlib collection whose
/// bodies are real Kāra over a generic, trait-bounded impl.
///
/// Exercises both directions and both element classes in one program,
/// because the two axes have historically failed independently: a scalar
/// `T` rides the default all-`i64` base layout (which is why B-2026-08-25-7
/// looked correct at `T = i64` while every `String` came back empty), and
/// the direction flag is the one branch `outranks` takes.
///
/// `into_sorted_vec` is deliberately the drain used throughout — it is the
/// `let mut h = self` + sibling-`mut ref self` shape that B-2026-08-25-5
/// hung on and B-2026-08-25-7 emptied. A regression in either resurfaces
/// here as a hang or as blank lines rather than as a subtle ordering bug.
///
/// Runs on a 16 MB worker, matching what the compiler gives itself in
/// production: `src/main.rs` spawns the whole CLI on a
/// `.stack_size(16 * 1024 * 1024)` thread and `src/lib.rs` does the same,
/// because on-demand monomorphization recurses through the callee chain —
/// here `into_sorted_vec` → `pop` → `sift_down` → `outranks` / `swap`, at
/// two instantiations. libtest's default thread is smaller than that, so
/// without the worker this test overflows the stack while `karac build` on
/// the identical source succeeds — a harness artifact, not a codegen bug,
/// and the same reason `tests/interpreter.rs` and `tests/parser.rs` already
/// spawn sized workers for their deep cases.
#[test]
fn e2e_stdlib_priority_queue_min_and_max_at_scalar_and_heap_t() {
    let src = r#"
fn main() {
    // (a) smallest-first, scalar T: push order is deliberately not sorted.
    let mut q: PriorityQueue[i64] = PriorityQueue.new();
    q.push(5); q.push(1); q.push(4); q.push(2); q.push(3);
    println(q.len());
    println(q.is_empty());
    let a = q.into_sorted_vec();
    let mut i = 0;
    while i < a.len() { println(a[i]); i = i + 1; }
    // (b) largest-first: the other arm of `outranks`.
    let mut m: PriorityQueue[i64] = PriorityQueue.max_first();
    m.push(5); m.push(1); m.push(4);
    let b = m.into_sorted_vec();
    let mut j = 0;
    while j < b.len() { println(b[j]); j = j + 1; }
    // (c) O(n) Floyd heapify rather than n pushes.
    let v: Vec[i64] = [9, 7, 8, 1, 3];
    let c = PriorityQueue.from(v).into_sorted_vec();
    let mut k = 0;
    while k < c.len() { println(c[k]); k = k + 1; }
    // (d) heap-carrying T: asserts CONTENTS, since the count stayed right
    //     while every element was empty under B-2026-08-25-7.
    let mut s: PriorityQueue[String] = PriorityQueue.new();
    s.push("pear"); s.push("apple"); s.push("fig");
    let d = s.into_sorted_vec();
    let mut z = 0;
    while z < d.len() { println(d[z]); z = z + 1; }
    // (e) largest-first at a heap T — both axes crossed.
    let e = PriorityQueue.max_first_from(["pear", "apple", "fig"]).into_sorted_vec();
    let mut w = 0;
    while w < e.len() { println(e[w]); w = w + 1; }
    // (f) empty queue: pop is None, and heapify's `n / 2 - 1` start index
    //     must not run the sift loop at n == 0.
    let mut f: PriorityQueue[i64] = PriorityQueue.new();
    println(f.is_empty());
    match f.pop() { Some(x) => { println(x); } None => { println("none"); } }
    let empty: Vec[i64] = [];
    let g = PriorityQueue.from(empty).into_sorted_vec();
    println(g.len());
}
"#;
    let out = std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(move || run_program(src))
        .expect("failed to spawn sized worker")
        .join()
        .expect("compile worker panicked");
    assert_eq!(
        out.as_deref(),
        Some(
            "5\nfalse\n1\n2\n3\n4\n5\n5\n4\n1\n1\n3\n7\n8\n9\n\
                 apple\nfig\npear\npear\nfig\napple\ntrue\nnone\n0\n"
        )
    );
}

/// B-2026-09-08-9 — the DEEP sibling of
/// `e2e_rc_promoted_base_field_move_out_in_loop`. A two-hop chain
/// (`let x = o.h.r`) off an RC-fallback-promoted root reaches the move-out
/// suppression through `field_chain_place_ptr`, whose arms GEP with a type
/// resolved from the DECLARED struct name and never consult `slot.ty`. A
/// promoted root's slot is an eight-byte `alloca ptr`, so the suppression
/// GEP'd a 72-byte `Ou` out of it and stored zeros at offsets 16 and 24 —
/// off the end, into the neighbouring destination alloca.
///
/// Pre-fix, all three consequences of one wild store: `t0`/`dS0` at -O0
/// (the clobber landed on the destination's `id`), `t1`/`dS1` at -O2, and
/// `free(): double free detected in tcache 2` under the JIT, whose frame
/// layout put a live pointer where the zeros went. valgrind read 0 errors
/// at both AOT levels throughout, which is why an ASAN-only check would
/// have called the AOT legs clean while they were reading a clobbered
/// value.
///
/// `c_flat` is B-2026-09-08-6's shape, which must stay fixed, and
/// `c_straight` is the un-promoted control that was correct throughout.
#[test]
fn e2e_rc_promoted_deep_chain_field_move_out_in_loop() {
    let Some(out) = run_program(
        "struct Rs { id: i64, name: String }\n\
             impl Drop for Rs {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"dS{self.id}\")\n\
             \x20   }\n\
             }\n\
             fn mks(i: i64) -> Rs { return Rs { id: i, name: f\"h{i}\" }; }\n\
             struct In { mut r: Rs, mut q: Rs }\n\
             struct Ou { mut h: In, mut k: i64 }\n\
             struct Bs { mut one: Rs, mut two: Rs }\n\
             fn c_deep() {\n\
             \x20   let mut o = Ou { h: In { r: mks(1), q: mks(2) }, k: 5 };\n\
             \x20   let mut i = 0;\n\
             \x20   while i < 1 { let x = o.h.r; println(f\"t{x.id}\"); i = i + 1; }\n\
             }\n\
             fn c_deep3() {\n\
             \x20   let mut o = Ou { h: In { r: mks(3), q: mks(4) }, k: 5 };\n\
             \x20   let mut i = 0;\n\
             \x20   while i < 3 { let x = o.h.r; i = i + 1; }\n\
             \x20   println(\"u\");\n\
             }\n\
             fn c_flat() {\n\
             \x20   let mut g = Bs { one: mks(5), two: mks(6) };\n\
             \x20   let mut i = 0;\n\
             \x20   while i < 1 { let y = g.one; println(f\"v{y.id}\"); i = i + 1; }\n\
             }\n\
             fn c_straight() {\n\
             \x20   let mut o = Ou { h: In { r: mks(7), q: mks(8) }, k: 5 };\n\
             \x20   let x = o.h.r;\n\
             \x20   println(f\"w{x.id}\");\n\
             }\n\
             fn main() { c_deep(); c_deep3(); c_flat(); c_straight(); println(\"end\"); }\n",
    ) else {
        return;
    };
    assert_eq!(
        out,
        "t1\ndS1\ndS2\ndS1\ndS3\ndS3\ndS3\nu\ndS4\ndS3\nv5\ndS5\ndS6\ndS5\n\
             dS8\nw7\ndS7\nend\n"
    );
}

/// B-2026-09-08-6 — a field move-out inside a `while` that iterates ONCE
/// is the same single move as the straight-line spelling, but the loop
/// makes the consume and the later use dominance-INCOMPARABLE, so the
/// ownership pass answers with an RC-FALLBACK PROMOTION rather than a
/// `UseAfterMove`. A promoted binding's alloca holds the `{i64 rc, T}` box
/// HANDLE, and every arm of the move-out machinery is written against a
/// binding whose alloca holds the VALUE.
///
/// Pre-fix: `free(): double free detected in tcache 2` on the JIT and at
/// -O0 (SIGABRT, rc=134, valgrind `Invalid free()`), and SIX bodies where
/// three are due at -O2 — one of them `dS0`, run over the husk, because the
/// walker the disarm re-registered GEPs a two-field struct out of the
/// 8-byte pointer slot. `KARAC_AUTO_PAR=0` reproduced both AOT readings.
///
/// `c2` is the straight-line control, correct before and after, and it is
/// what isolates the loop as the whole difference. `c3` keeps the
/// `{ptr,len,cap}` shape the three sibling rows already fixed
/// (B-2026-09-07-23 / -29 / -30) passing, since the gap this closes is
/// exactly the STRUCT-shaped field their self-gate turns away.
#[test]
fn e2e_rc_promoted_base_field_move_out_in_loop() {
    let Some(out) = run_program(
        "struct Rs { id: i64, name: String }\n\
             impl Drop for Rs {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"dS{self.id}\")\n\
             \x20   }\n\
             }\n\
             fn mks(i: i64) -> Rs { return Rs { id: i, name: f\"h{i}\" }; }\n\
             struct Bs { mut one: Rs, mut two: Rs }\n\
             struct Ps { mut a: String, mut b: i64 }\n\
             fn main() {\n\
             \x20   println(\"c1\");\n\
             \x20   let mut g = Bs { one: mks(1), two: mks(2) };\n\
             \x20   let mut i = 0;\n\
             \x20   while i < 1 { let taken = g.one; println(f\"t{taken.id}\"); i = i + 1; }\n\
             \x20   println(\"c2\");\n\
             \x20   let mut h = Bs { one: mks(3), two: mks(4) };\n\
             \x20   let straight = h.one;\n\
             \x20   println(f\"t{straight.id}\");\n\
             \x20   println(\"c3\");\n\
             \x20   let mut p = Ps { a: \"payload\", b: 1 };\n\
             \x20   let mut j = 0;\n\
             \x20   while j < 2 { let s = p.a; println(f\"L{s.len()}\"); j = j + 1; }\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(
        out,
        "c1\nt1\ndS1\nc2\ndS4\nt3\ndS3\nc3\nL7\nL7\nend\ndS2\ndS1\n"
    );
}

/// B-2026-09-06-62 — the RECEIVER spelling of B-2026-09-06-52. `impl R { fn
/// take(self) -> i64 { let m = self; .. } }` over a struct with a `shared`
/// field aborted `free(): double free detected in tcache 2` under the JIT
/// and at -O0, and survived -O2 as an invalid read of the freed refcount
/// block, while the byte-identical FREE FUNCTION one receiver-spelling over
/// was already clean: `self` parses as `SelfValue`, so neither the
/// param-view test nor the caller-retained test that row added ever saw it.
///
/// Three parts, each measured: the retained test now reads a bare `self`;
/// the non-view registration declines the binding's own wrapper for a
/// receiver the prologue refused to own, registering bodies only (the
/// free-function path registers nothing there, but an owned-`self`
/// receiver's temp registrar declines once the callee binds a part out, so
/// nothing would run the body); and the induction runs through locals, so
/// `let m = self; let n = m;` declines twice rather than once.
///
/// The keep-memory downgrade `suppress_user_drop_body_keeping_memory` makes
/// at the call site also had to learn the `shared` field: it replaced the
/// wrapper with the plain struct drop, which by design leaves a direct
/// `shared` field to the binding's own `let` cleanup — the wrapper it just
/// removed — so the box leaked 16 bytes per call once the callee stopped
/// double-freeing it.
///
/// Twin of `tests/interpreter.rs`'s `test_owned_self_rebind_of_a_shared_field_struct`, pinned to the same string.
#[test]
fn e2e_owned_self_rebind_of_a_shared_field_struct() {
    let Some(out) = run_program(
        r#"shared struct Inner { v: i64 }
struct R { id: i64, name: String, inner: Inner }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
struct P { id: i64, name: String, xs: Vec[i64] }
impl Drop for P { fn drop(mut ref self) { println(f"  dP{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}", inner: Inner { v: i } }; }
fn mkp(i: i64) -> P { return P { id: i, name: f"p{i}", xs: [i] }; }
impl R {
    fn take(self) -> i64 { let m = self; return m.id; }
    fn twice(self) -> i64 { let m = self; let n = m; return n.id; }
    fn plain(self) -> i64 { return self.id; }
    fn borrowed(ref self) -> i64 { return self.id; }
}
impl P { fn take(self) -> i64 { let m = self; return m.id; } }
fn top(r: R) -> i64 { let m = r; return m.id; }

fn main() {
    println("temp_receiver"); println(f"  v={mk(1).take()}");
    println("named_receiver"); let a = mk(2); println(f"  v={a.take()}");
    println("twice"); println(f"  v={mk(3).twice()}");
    println("borrowed"); let b = mk(5); println(f"  v={b.borrowed()}");
    println("copyable_struct"); println(f"  v={mkp(6).take()}");
    println("free_function"); println(f"  v={top(mk(7))}");
    println("end");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"temp_receiver
  dR1
  v=1
named_receiver
  dR2
  v=2
twice
  dR3
  v=3
borrowed
  v=5
  dR5
copyable_struct
  dP6
  v=6
free_function
  dR7
  v=7
end
"#
    );
}

/// B-2026-09-07-48 — a method called DIRECTLY on a field projected off an
/// RC-FALLBACK-PROMOTED local (`t.a.len()`) read through the box HANDLE
/// instead of the box, and silently returned garbage.
///
/// `lower_field_access_ptr` recognised three receiver slot shapes — a
/// `shared` handle, a `ref T` param, a module global — and fell through to
/// the raw `slot.ptr` for everything else. A promoted binding is the
/// FOURTH, and its slot holds a `{i64 rc, T}` box handle, so the field GEP
/// indexed an 8-byte alloca as if it were the struct.
///
/// THE ROW READ THE DEFECT AS POSITIONAL and it is not. It reported a sharp
/// "post-loop" boundary, because the loop is what PROMOTES the binding and
/// its other cells happened to read through a local. Promotion is a
/// whole-function property, so cell 2 — the SAME read placed BEFORE the
/// loop — is equally wrong on the parent (16, not 38). That is why this
/// test is not written as a post-loop fixture.
///
/// SILENT AND THREE-WAY, because the garbage depends on which word the
/// field's offset lands on: the row measured 152 / -1 / 117 for cell 1
/// across interpreter / AOT / JIT, and no backend aborted. Cells 4-6 are
/// the controls that localise it to the receiver GEP — the VALUE is intact
/// on the parent (a `let` hop or a non-heap field reads correctly), so a
/// fix that touched the box's contents rather than the read would pass
/// cell 1 and break these.
#[test]
fn e2e_rc_promoted_projection_method_receiver_resolves_through_the_box() {
    const PRE: &str = r#"struct P { a: String, b: i64 }
fn seed() -> i64 { env.args().len() }
fn payload() -> String { f"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa" }
fn mkp(n: i64) -> P { return P { a: payload(), b: n }; }
"#;
    // (source tail, expected stdout, cell name)
    let cells: [(&str, &str, &str); 6] = [
        // 1 — the row's own repro: 3 trips through the loop (114) plus the
        // post-loop read (38). Parent printed -1 compiled, 117 on the JIT.
        (
            r#"fn go() -> i64 { let t = mkp(9); let mut i = 0i64; let mut n = 0i64;
  while i < 3i64 { let s = t.a; n = n + s.len(); i = i + 1; }
  return n + t.a.len(); }
fn main() { println(go()); }
"#,
            "152",
            "post_loop_projection_read",
        ),
        // 2 — THE SAME READ, BEFORE THE LOOP. This is the cell that refutes
        // the row's positional framing: the binding is promoted for the
        // whole function, so the parent is wrong here too (16).
        (
            r#"fn go() -> i64 { let t = mkp(9); let pre = t.a.len(); let mut i = 0i64;
  while i < 2i64 { let s = t.a; i = i + 1; }
  return pre; }
fn main() { println(go()); }
"#,
            "38",
            "pre_loop_projection_read",
        ),
        // 3 — the row's second shape: the consume is a `push` argument
        // rather than a `let`. Parent printed 17825792 compiled, 0 on JIT.
        (
            r#"fn go() -> i64 { let t = mkp(9); let mut v: Vec[String] = Vec.new(); let mut i = 0i64;
  while i < 3i64 { v.push(t.a); i = i + 1; }
  return t.a.len(); }
fn main() { println(go()); }
"#,
            "38",
            "push_consume_then_projection_read",
        ),
        // 4 — CONTROL: the same read routed through a `let` hop. Correct on
        // the parent, which is what proves the box's CONTENTS were never
        // damaged and localises the defect to the receiver GEP.
        (
            r#"fn go() -> i64 { let t = mkp(9); let mut i = 0i64;
  while i < 2i64 { let s = t.a; i = i + 1; }
  let q = t.a; return q.len(); }
fn main() { println(go()); }
"#,
            "38",
            "control_read_via_let_hop",
        ),
        // 5 — CONTROL: a NON-HEAP field off the same promoted binding. It
        // reads correctly on the parent (9) because an i64 field's offset
        // lands on a word that survives the bad GEP; keeping it pins that
        // the fix did not shift the base for the whole struct.
        (
            r#"fn go() -> i64 { let t = mkp(9); let mut i = 0i64;
  while i < 2i64 { let s = t.a; i = i + 1; }
  return t.b; }
fn main() { println(go()); }
"#,
            "9",
            "control_non_heap_field_unchanged",
        ),
        // 6 — CONTROL: no loop, so no promotion and no box. The plain
        // `slot.ptr` fall-through is the correct answer here, and the new
        // arm must not divert it.
        (
            r#"fn go() -> i64 { let t = mkp(9); return t.a.len(); }
fn main() { println(go()); }
"#,
            "38",
            "control_no_promotion",
        ),
    ];
    for (tail, want, name) in cells {
        let Some(cap) = run_program_capturing(&format!("{PRE}{tail}")) else {
            return;
        };
        assert_eq!(cap.stdout.trim(), want, "cell {name}: stdout");
        assert!(
            cap.status.success(),
            "cell {name}: exited {:?}; stderr={:?}",
            cap.status,
            cap.stderr
        );
    }
}

/// B-2026-09-07-29 — a WHOLE consume inside a loop that actually RUNS
/// double-frees an RC-fallback-promoted local. No projection is involved:
/// `takep(t)` takes the entire binding and there is no `t.a` in the
/// program, which is what separates this from the row directly below.
///
/// The whole-program transfer gate (`codegen::param_transfer`) may hand a
/// by-value struct param to the callee OUTRIGHT — no entry copy — only when
/// every call site passes a binding the caller can then stop owning. It
/// enforced that by subtracting `use_after_move_consume_sites`, which is
/// only ONE of the ownership pass's two answers for a source that outlives
/// its consume; for a consume inside a LOOP that pass RC-FALLBACK PROMOTES
/// the binding instead, and those sites were never subtracted. The promoted
/// binding's cleanup is an `RcDec` on a `{i64 rc, T}` box, so the caller's
/// retraction — a `StructDrop`/`UserDrop` scan keyed on the binding — finds
/// nothing and no-ops, and both frames free the same buffer, once per TRIP.
///
/// STDOUT ALONE IS ENOUGH HERE, unlike its projection sibling below. This
/// double free lands inside the loop rather than at scope exit, so the
/// parent aborts with `free(): double free detected in tcache 2` BEFORE
/// printing anything and the stdout assertion fails on its own. The status
/// assertion is kept regardless, because which side of the print an abort
/// lands on is not a property worth relying on.
///
/// The VALUES are what rule out the fix that looks equivalent. Declining
/// the callee's ownership instead of its transfer would leave the buffer
/// with no owner as soon as the callee touched it; taking the value OUT of
/// the box would give the second trip an emptied `t`. Every cell sums
/// `t.b` across its trips, so a `t` that lost its contents reads 0 rather
/// than 27 / 45 / 30 — and all four surfaces (interpreter, JIT, and the
/// compiled build at both auto-par settings) print what is pinned here.
#[test]
fn e2e_whole_consume_in_a_running_loop_keeps_the_rc_box_the_only_owner() {
    // The payload must be HEAP-allocated: a string LITERAL does not
    // allocate, so a literal-payload fixture gives the box no buffer for a
    // second owner to free and pins nothing (measured on this family's
    // sibling, which shipped such a fixture once).
    const PRE: &str = r#"struct P { a: String, b: i64 }
fn seed() -> i64 { env.args().len() }
fn payload() -> String { f"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa" }
fn mkp(n: i64) -> P { return P { a: payload(), b: n }; }
fn takep(p: P) -> i64 { return p.b; }
"#;
    // (source tail, expected stdout, cell name)
    let cells: [(&str, &str, &str); 8] = [
        // 1 — the row's own spelling: three trips, three extra frees.
        (
            r#"fn go() -> i64 { let t = mkp(9); let mut i = 0i64; let mut n = 0i64;
  while i < 3i64 { n = n + takep(t); i = i + 1; }
  return n; }
fn main() { println(go()); }
"#,
            "27",
            "three_trips",
        ),
        // 2 — ONE trip: the smallest dirty cell, and the one that shows the
        // damage is per-iteration rather than per-loop.
        (
            r#"fn go() -> i64 { let t = mkp(9); let mut i = 0i64; let mut n = 0i64;
  while i < 1i64 { n = n + takep(t); i = i + 1; }
  return n; }
fn main() { println(go()); }
"#,
            "9",
            "one_trip",
        ),
        // 3 — FIVE trips, the other end of the scaling.
        (
            r#"fn go() -> i64 { let t = mkp(9); let mut i = 0i64; let mut n = 0i64;
  while i < 5i64 { n = n + takep(t); i = i + 1; }
  return n; }
fn main() { println(go()); }
"#,
            "45",
            "five_trips",
        ),
        // 4 — the `for` spelling: the promotion is about the consume being
        // inside a loop, not about the keyword.
        (
            r#"fn go() -> i64 { let t = mkp(9); let mut n = 0i64;
  for _k in 0i64..3i64 { n = n + takep(t); }
  return n; }
fn main() { println(go()); }
"#,
            "27",
            "for_loop",
        ),
        // 5 — CONTROL: no loop, so no promotion. This is the population the
        // transfer gate exists to serve; it must keep its transfer.
        (
            r#"fn go() -> i64 { let t = mkp(9); return takep(t); }
fn main() { println(go()); }
"#,
            "9",
            "control_no_loop",
        ),
        // 6 — CONTROL: the loop is NEVER ENTERED. Promotion is static so
        // `t` is still boxed, but nothing is transferred — B-2026-09-07-17's
        // own shape, kept so a regression there fails here too.
        (
            r#"fn go() -> i64 { let t = mkp(9); let mut i = 0i64; let mut n = 0i64;
  while i < 0i64 { n = n + takep(t); i = i + 1; }
  return n; }
fn main() { println(go()); }
"#,
            "0",
            "control_never_entered",
        ),
        // 7 — CONTROL, and the cell that pinned the fix's shape: the same
        // program with an `impl Drop for P`. A type reaching a user `Drop`
        // was already transfer-INELIGIBLE, so this was clean on the parent
        // — the entry-copy path handling the shape correctly while the
        // transfer path did not. The body fires ONCE, from the box, not
        // once per trip.
        (
            r#"impl Drop for P { fn drop(mut ref self) { println(f"dP{self.b}"); } }
fn go() -> i64 { let t = mkp(9); let mut i = 0i64; let mut n = 0i64;
  while i < 3i64 { n = n + takep(t); i = i + 1; }
  return n; }
fn main() { println(go()); }
"#,
            "dP9\n27",
            "control_user_drop_fires_once",
        ),
        // 8 — the promoted binding in the SECOND parameter slot, beside a
        // fresh temp that disqualifies the first on its own. The gate is
        // keyed by `(callee, index)`.
        (
            r#"fn two(x: P, y: P) -> i64 { return x.b + y.b; }
fn go() -> i64 { let t = mkp(9); let mut i = 0i64; let mut n = 0i64;
  while i < 3i64 { n = n + two(mkp(1), t); i = i + 1; }
  return n; }
fn main() { println(go()); }
"#,
            "30",
            "second_param_slot",
        ),
    ];
    for (tail, want, name) in cells {
        let Some(cap) = run_program_capturing(&format!("{PRE}{tail}")) else {
            return;
        };
        assert_eq!(cap.stdout.trim(), want, "cell {name}: stdout");
        assert!(
            cap.status.success(),
            "cell {name}: exited {:?}; stderr={:?}",
            cap.status,
            cap.stderr
        );
    }
}

/// B-2026-09-07-23 — a heap field PROJECTED out of an RC-FALLBACK-PROMOTED
/// local shared one buffer with the box, and both freed it once per loop
/// iteration. (Its never-entered sibling, B-2026-09-07-19, is clean and was
/// closed as fixed in passing by `9a50182`.)
///
/// The disarms all reach the source field by GEP-ing the binding's slot,
/// and a promoted slot holds a `{i64 rc, T}` box HANDLE rather than the
/// struct, so every one of them bails and the cap is never zeroed. The
/// destination takes ownership the box has not given up; inside a loop that
/// is once per TRIP, so it scales with the trip count and aborts
/// `free(): double free detected in tcache 2` on every compiled backend.
///
/// THIS TEST ASSERTS THE EXIT STATUS, NOT ONLY STDOUT, and that is the
/// whole reason it is written with `run_program_capturing`. The double free
/// fires at scope-exit drop, AFTER the program has printed — so an
/// stdout-only assertion passes against the defect. Same shape as
/// B-2026-06-09-1, which is why `CapturedRun::status` exists.
///
/// The OUTPUT half still earns its place: it rules out the fix that looks
/// equivalent. Disarming the box's OWN field would neutralize the source,
/// but the box is the surviving owner exactly because the binding is
/// re-used after the consume — so the second trip would read a ZEROED
/// string. Cell 2 reads the projected field's length on every trip and
/// would print `0` under that fix rather than `38`; cell 3 mutates its copy
/// and would see a short string. The interpreter prints what is pinned
/// here, on all three columns.
#[test]
fn e2e_rc_boxed_projection_copies_instead_of_sharing_an_owner() {
    // The payload must be HEAP-allocated. A string LITERAL does not
    // allocate, so a literal-payload fixture cannot reproduce this defect
    // at all: an earlier draft used one, measured 9 allocations against the
    // 17 here, and PASSED against the parent commit — pinning nothing. The
    // f-string is what gives the box a buffer for a second owner to free.
    const PRE: &str = r#"struct P { a: String, b: i64 }
fn seed() -> i64 { env.args().len() }
fn payload() -> String { f"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa" }
fn mkp(n: i64) -> P { return P { a: payload(), b: n }; }
"#;
    // (source tail, expected stdout, cell name)
    let cells: [(&str, &str, &str); 5] = [
        // 1 — the row's spelling with the loop ENTERED, plus the source
        // read afterwards (which is what drives the promotion at all).
        (
            r#"fn go() -> i64 { let t = mkp(9); let mut i = 0i64;
  while i < 3i64 { let p = P { a: t.a, b: 1 }; i = i + p.b; }
  return t.b; }
fn main() { println(go()); }
"#,
            "9",
            "literal_loop_entered",
        ),
        // 2 — READS the projected field on every trip. 38 chars each time,
        // so `i` advances by 1 per trip and the loop terminates at 3.
        (
            r#"fn go() -> i64 { let t = mkp(9); let mut i = 0i64; let mut n = 0i64;
  while i < 3i64 { let p = P { a: t.a, b: 1 }; n = p.a.len(); i = i + 1; }
  return n; }
fn main() { println(go()); }
"#,
            "38",
            "field_read_each_trip",
        ),
        // 3 — the projected binding is MUTATED, so its copy must be
        // INDEPENDENT of the box's buffer: 38 + 2 every trip, not growing.
        (
            r#"fn go() -> i64 { let t = mkp(9); let mut i = 0i64; let mut n = 0i64;
  while i < 3i64 { let mut s = t.a; s.push_str("XY"); n = s.len(); i = i + 1; }
  return n; }
fn main() { println(go()); }
"#,
            "40",
            "mutated_destination",
        ),
        // 4 — the BARE projection, no literal anywhere: the literal in the
        // row's title is incidental to the defect.
        (
            r#"fn go() -> i64 { let t = mkp(9); let mut i = 0i64; let mut n = 0i64;
  while i < 3i64 { let s = t.a; n = n + s.len(); i = i + 1; }
  return n; }
fn main() { println(go()); }
"#,
            "114",
            "bare_projection_no_literal",
        ),
        // 5 — CONTROL: no loop, so no promotion. The disarm works and the
        // destination legitimately owns; nothing may change here.
        (
            r#"fn go() -> i64 { let t = mkp(9); let p = P { a: t.a, b: 1 };
  return p.a.len() + p.b; }
fn main() { println(go()); }
"#,
            "39",
            "no_promotion_control",
        ),
    ];
    for (tail, want, name) in cells {
        let Some(cap) = run_program_capturing(&format!("{PRE}{tail}")) else {
            return;
        };
        assert_eq!(cap.stdout.trim(), want, "cell {name}: stdout");
        // The defect is a scope-exit double free, which lands after the
        // print — so this, not the line above, is what fails on the parent.
        assert!(
            cap.status.success(),
            "cell {name}: exited {:?}; stderr={:?}",
            cap.status,
            cap.stderr
        );
    }
}

#[test]
fn e2e_discarded_branch_of_body_less_heap_literals_keeps_one_owner() {
    let hdr = "struct P { a: String, b: i64 }\n\
                   struct D { a: String, b: i64 }\n\
                   impl Drop for D { fn drop(mut ref self) { println(f\"dD{self.b}\") } }\n\
                   struct W { r: D, b: i64 }\n\
                   fn pay() -> String { return \"heap\"; }\n\
                   fn mkd(n: i64) -> D { return D { a: pay(), b: n }; }\n\
                   fn mkw(n: i64) -> W { return W { r: mkd(n), b: n }; }\n";
    for (label, body, want) in [
        // The row's own shapes: no body anywhere in `P`, so nothing prints
        // and the only thing that changed is memory.
        (
            "body-less literal through a bare if",
            "if n == 0 { P { a: pay(), b: 1 } } else { P { a: pay(), b: 2 } };",
            "end\n",
        ),
        (
            "body-less literal through a wildcard let match",
            "let _ = match n { 0 => { P { a: pay(), b: 1 } } _ => { P { a: pay(), b: 2 } } };",
            "end\n",
        ),
        // A type that declares its OWN `Drop` never reaches the new
        // fall-through (its wrapper carries body and memory together).
        // One body, not two.
        (
            "own-Drop literal through a branch stays single",
            "let _ = if n == 0 { D { a: pay(), b: 1 } } else { D { a: pay(), b: 2 } };",
            "dD1\nend\n",
        ),
        (
            "own-Drop call through a branch stays single",
            "let _ = if n == 0 { mkd(1) } else { mkd(2) };",
            "dD1\nend\n",
        ),
        // No own `Drop` but a Drop-BEARING field: this one takes the arm
        // BELOW the new one, which registers the field-bodies walk. It is
        // the nearest neighbour to the changed branch and the one most
        // likely to double if the fall-through were placed wrong.
        (
            "Drop-bearing field through a branch stays single",
            "let _ = if n == 0 { W { r: mkd(1), b: 1 } } else { W { r: mkd(2), b: 2 } };",
            "dD1\nend\n",
        ),
        // The direct-discard spellings, which already carried memory AND
        // bodies through `track_inline_owned_aggregate_arg` and must be
        // untouched by this row.
        (
            "control: direct own-Drop literal discard",
            "let _ = D { a: pay(), b: 1 };",
            "dD1\nend\n",
        ),
        (
            "control: bare-statement own-Drop literal discard",
            "D { a: pay(), b: 1 };",
            "dD1\nend\n",
        ),
        // The GUARD's admitted kinds, carrying a body so a wrong decline
        // or a double is visible as a count rather than only as bytes.
        // The GUARD's two sides, carrying a body so a wrong answer shows
        // up as a count rather than only as bytes. The declined shape
        // (a field initializer projected off a FRESH TEMP) must still be
        // correct — the guard withholds the memory registration, not the
        // body — and the admitted shape must stay single.
        (
            "guard declines the aliasing shape and it is still single",
            "let _ = if n == 0 { W { r: mkw(7).r, b: 1 } } else { W { r: mkd(2), b: 2 } };",
            "dD7\nend\n",
        ),
        (
            "guard admits arms that mint their own field",
            "let _ = if n == 0 { W { r: mkd(7), b: 1 } } else { W { r: mkd(2), b: 2 } };",
            "dD7\nend\n",
        ),
    ] {
        let src = format!("{hdr}fn main() {{\nlet n = 0;\n{body}\nprintln(\"end\");\n}}\n");
        assert_eq!(run_program(&src).as_deref(), Some(want), "[{label}]");
    }
    // B-2026-08-31-35 — the WHOLE-LOCAL move is FIXED and asserted as
    // correct: a discarded branch whose arm literal consumes a local now
    // runs that local's body ONCE, at the discard, on every backend. The
    // discarded statement position is an escaping site exactly when this
    // site owns the merged value, so the taken arm's consumed sources are
    // disarmed on the path that handed them over.
    let src = format!(
        "{hdr}fn main() {{\nlet n = 0;\nlet t = mkd(7);\n\
             let _ = if n == 0 {{ W {{ r: t, b: 1 }} }} else {{ W {{ r: mkd(2), b: 2 }} }};\n\
             println(\"end\");\n}}\n"
    );
    assert_eq!(
        run_program(&src).as_deref(),
        Some("dD7\nend\n"),
        "[discarded arm literal consuming a whole local runs one body]"
    );
    // B-2026-09-01-17 — the PROJECTED spelling (`W { r: t.r, .. }`) is FIXED
    // and asserted as correct. The surplus body was the SOURCE local's
    // field-bodies walk, fired early via NLL — established by backtrace
    // (`fire_due_drops` -> `drop_user_drop_fields_of_binding`), not by print
    // order, which misleads here because the source's walk runs AHEAD of the
    // consumer's. `compile_struct_init`'s field loop carried
    // `suppress_struct_field_move_into_literal` (the MEMORY half of a
    // struct-field move-out) without its BODIES peer, which that helper's
    // own doc says belongs at the same positions.
    //
    // Landed AFTER B-2026-09-13-26, and the order is load-bearing: until a
    // discarded ARRAY literal registered an owner, the source's walk was the
    // only thing running an array element's body, and standing it down took
    // that body to ZERO. The array and bare-block spellings are asserted
    // alongside this one for exactly that reason.
    let src = format!(
        "{hdr}fn main() {{\nlet n = 0;\nlet t = mkw(7);\n\
             let _ = if n == 0 {{ W {{ r: t.r, b: 1 }} }} else {{ W {{ r: mkd(2), b: 2 }} }};\n\
             println(\"end\");\n}}\n"
    );
    assert_eq!(
        run_program(&src).as_deref(),
        Some("dD7\nend\n"),
        "[discarded arm literal consuming a local's FIELD runs one body]"
    );
    // B-2026-09-13-26 / B-2026-09-13-28 — the two spellings whose bodies had
    // NO owner until this landed, pinned here beside the fix that depends on
    // them. An array literal's elements and an aliased literal at a bare
    // block tail each ran zero bodies; with owners in place both run one, and
    // the projection disarm above composes instead of zeroing them.
    for (label, body) in [
            (
                "discarded array literal of a projected field",
                "let t = mkw(7);\n                 let _ = if n == 0 { [W { r: t.r, b: 1 }] } else { [W { r: mkd(2), b: 2 }] };",
            ),
            (
                "aliased literal at a bare block tail",
                "let t = mkw(7);\nlet _ = { W { r: t.r, b: 1 } };",
            ),
            (
                "discarded array literal of fresh elements",
                "let _ = if n == 0 { [mkd(7), mkd(8)] } else { [mkd(2)] };",
            ),
        ] {
            let src = format!("{hdr}fn main() {{\nlet n = 0;\n{body}\nprintln(\"end\");\n}}\n");
            let want = if label.contains("fresh elements") {
                "dD7\ndD8\nend\n"
            } else {
                "dD7\nend\n"
            };
            assert_eq!(run_program(&src).as_deref(), Some(want), "[{label}]");
        }
}

/// B-2026-08-17-18 — heap-class deferred initialization: String and
/// Vec (scalar / String elements). Zero-header at the declaration,
/// sidecar registration from the declared TypeExpr, cap-guarded frees.
/// Interp-parity pins; the memory dimension is the ASAN trio
/// `asan_deferred_init_*`. A Map deferred-init stays a loud codegen
/// deferral (fail-closed elem gate), pinned by the message assert.
#[test]
fn heap_deferred_initialization_lowers() {
    assert_eq!(
            run_program(
                "fn main() { let c = true; let mut s: String; if c { s = f\"hi{1}\"; } else { s = f\"no{2}\"; } s = s + \"!\"; println(s); }"
            ),
            Some("hi1!\n".to_string()),
            "String branch-init + append"
        );
    assert_eq!(
            run_program(
                "fn main() { let c = false; let mut v: Vec[i64]; if c { v = [1, 2]; } else { v = Vec.new(); } v.push(7); println(v.len() + v[0]); }"
            ),
            Some("8\n".to_string()),
            "Vec[i64] branch-init + push"
        );
    assert_eq!(
            run_program(
                "fn main() { let mut v: Vec[String]; v = Vec.new(); v.push(f\"a{1}\"); println(v[0]); }"
            ),
            Some("a1\n".to_string()),
            "Vec[String] deferred init"
        );
}

/// B-2026-07-28-6: assigning through a `shared struct` FIELD of a plain
/// struct must write through the RC handle, so the mutation is visible to
/// every other holder of that cell.
///
/// It wrote into the field SLOT instead. The mechanism was a guard, not the
/// GEP: `compile_field_store`'s nested branch resolved the parent's layout
/// via `struct_types`, where a `shared struct` is never registered (it lives
/// in `shared_types`), so the branch fell through to the function's no-op
/// tail and the store was dropped with no diagnostic. The emitted IR loaded
/// the handle and then simply did not write, after which the optimizer
/// const-folded the later read to the pre-write value.
///
/// The asymmetry is what identifies it, and is asserted here: READS through
/// the field were always correct, so a test that only checked reads passes
/// either way. This is the `shared struct` + `mut` field pattern the
/// `examples/tangle/undo_redo.kara` dogfood exists to prove — it printed
/// `30 / 30 / 30 / 30` against a documented `30 / 20 / 10 / 20`.
#[test]
fn test_e2e_shared_struct_field_assign_writes_through_handle() {
    // Write through the holder is visible at the source (the defect), and
    // the reverse direction (which always worked) still holds.
    assert_eq!(
        run_program(
            r#"
shared struct Cell { mut value: i64 }
struct Holder { cell: Cell }
fn main() {
    let c = Cell { value: 10 };
    let mut h = Holder { cell: c };
    h.cell.value = 99;
    println(f"{c.value}");
    c.value = 7;
    println(f"{h.cell.value}");
}
"#
        ),
        Some("99\n7\n".to_string())
    );

    // Index-rooted parent (`v[0].cell.value = …`): the same branch, reached
    // with an `Index` object instead of a `FieldAccess` one — the container
    // shape `undo_redo`'s history stack puts these cells in.
    assert_eq!(
        run_program(
            r#"
shared struct Cell { mut value: i64 }
struct Holder { cell: Cell }
fn main() {
    let c = Cell { value: 10 };
    let mut v: Vec[Holder] = Vec.new();
    v.push(Holder { cell: c });
    v[0].cell.value = 99;
    println(f"{c.value}");
}
"#
        ),
        Some("99\n".to_string())
    );

    // Depth >= 3: the handle load has to happen inside the place walk, not
    // just at the one call site, or `a.b.c.value` regresses.
    assert_eq!(
        run_program(
            r#"
shared struct Cell { mut value: i64 }
struct Inner { cell: Cell }
struct Outer { inner: Inner }
fn main() {
    let c = Cell { value: 1 };
    let mut o = Outer { inner: Inner { cell: c } };
    o.inner.cell.value = 42;
    println(f"{c.value}");
}
"#
        ),
        Some("42\n".to_string())
    );

    // A plain (non-shared) nested struct field must keep its in-place
    // semantics — the fallback arm of the same branch.
    assert_eq!(
        run_program(
            r#"
struct Leaf { mut n: i64 }
struct Mid { leaf: Leaf }
fn main() {
    let mut m = Mid { leaf: Leaf { n: 1 } };
    m.leaf.n = 5;
    println(f"{m.leaf.n}");
}
"#
        ),
        Some("5\n".to_string())
    );
}

#[test]
fn test_ir_fence_acquire_release_orderings() {
    // Each non-Relaxed ordering maps to its LLVM spelling. Acquire and
    // Release cover the load/store-half barriers; AcqRel the combined form.
    let ir = ir_for(
        r#"
fn barriers() {
    // Safety: paired with matching accesses on other threads.
    unsafe {
        fence(MemoryOrdering.Acquire);
        fence(MemoryOrdering.Release);
        fence(MemoryOrdering.AcqRel);
    }
}
fn main() { barriers(); }
"#,
    );
    assert!(
        ir.contains("fence acquire"),
        "missing `fence acquire`; IR:\n{ir}"
    );
    assert!(
        ir.contains("fence release"),
        "missing `fence release`; IR:\n{ir}"
    );
    assert!(
        ir.contains("fence acq_rel"),
        "missing `fence acq_rel`; IR:\n{ir}"
    );
}

#[test]
fn test_ir_critical_section_acquire_emits_acquire_and_scope_exit_release() {
    // `critical_section.acquire()` lowers to a call to the runtime
    // `karac_critical_section_acquire`, and the guard's scope-exit Drop
    // (hand-rolled `@CriticalSectionGuard.drop`) calls
    // `karac_critical_section_release` — both appear in module IR.
    let ir = ir_for(
        r#"
fn f() with writes(Hardware) {
    let _guard = critical_section.acquire();
    println(7);
}
fn main() { f(); }
"#,
    );
    assert!(
        ir.contains("call i64 @karac_critical_section_acquire()"),
        "expected an acquire call; IR:\n{ir}"
    );
    assert!(
        ir.contains("@karac_critical_section_release"),
        "expected a scope-exit release call (guard Drop); IR:\n{ir}"
    );
    // The Drop wrapper the RAII machinery routes through must exist.
    assert!(
        ir.contains("@\"CriticalSectionGuard.drop\"") || ir.contains("@CriticalSectionGuard.drop"),
        "expected the hand-rolled CriticalSectionGuard.drop body; IR:\n{ir}"
    );
}

#[test]
fn test_e2e_shared_scrutinee_shadowed_by_local_no_corruption() {
    // B-2026-07-12-31 correctness pin (the ASAN/leak gate lives in
    // tests/memory_sanitizer.rs::asan_shared_scrutinee_shadowed_by_local).
    // A `match e { … }` arm over a by-value shared-enum param `e` that
    // declares a local `let mut e = 0` shadows the scrutinee's slot. The
    // param's scope-exit RC-dec reloaded its pointer BY NAME, picking up
    // the i64 shadow slot and dec'ing a garbage address (segfault at O2 /
    // hang at O0). The fix gates the reload on the slot being pointer-typed.
    // This pins that the shadow is honored (the arm returns the i64 sum)
    // AND the program completes cleanly.
    let out = run_program(
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
    println(chk(B(10)));
}
"#,
    );
    if let Some(out) = out {
        // 10 (payload) + 3 (0+1+2) = 13.
        assert_eq!(out.trim(), "13");
    }
}

#[test]
fn for_loop_shared_bearing_struct_element_destructure_and_whole_move() {
    // B-2026-07-18-2: a for-loop over `Vec[S]` where S carries a DIRECT
    // `shared` handle field. `field_copy_supported` hard-bailed on the
    // bare-shared field, so the element was never registered in
    // `for_loop_owned_agg_vars` — a destructured String leaf
    // (`let S { name, .. } = lf; sink.push(name)`) and a whole-move
    // (`let x = lf`) both aliased the element's buffers and double-freed
    // against the container's per-element drain (SIGABRT; interp correct —
    // surfaced by the selfhost codegen generator's StructLit arm). Now the
    // for-loop registration runs copy-support in allow-bare-shared mode and
    // the move-out copies rc-INC the handle, symmetric with the drain's
    // rc-DEC.
    let src = "shared enum T2 { Num(i64) }\n\
                   struct Slf { name: String, value: T2 }\n\
                   fn main() {\n\
                   \x20   let mut fs: Vec[Slf] = Vec.new();\n\
                   \x20   fs.push(Slf { name: \"ab\", value: T2.Num(3) });\n\
                   \x20   fs.push(Slf { name: \"cde\", value: T2.Num(4) });\n\
                   \x20   let mut lit_names: Vec[String] = Vec.new();\n\
                   \x20   let mut n = 0;\n\
                   \x20   for lf in fs {\n\
                   \x20       let Slf { name: fname, value } = lf;\n\
                   \x20       match value { T2.Num(k) => { n = n + k; } _ => {} }\n\
                   \x20       lit_names.push(fname);\n\
                   \x20   }\n\
                   \x20   for lf2 in fs {\n\
                   \x20       let x = lf2;\n\
                   \x20       n = n + x.name.len();\n\
                   \x20   }\n\
                   \x20   println(lit_names.len());\n\
                   \x20   println(n);\n\
                   }\n";
    // n = 3+4 (payloads) + 2+3 (name lens) = 12; both loops complete.
    assert_eq!(run_program(src).as_deref(), Some("2\n12\n"));
}

/// Returning a heap FIELD through a BORROWED receiver (`fn name(ref self) ->
/// String { self.n }`) — the borrow does not own the field, so codegen must
/// deep-clone it on return (`maybe_defensive_copy_param_arg`). Previously it
/// returned an alias of the receiver's buffer, which the caller's drop of
/// the receiver then double-freed (surfaced while making generic
/// associated-type field-returns usable, but fires non-generically too).
/// The receiver is USED AFTER the call (`x.name()` then `x.n`), so a move
/// would be wrong — the clone must leave the field intact. Covers `ref
/// self`, `mut ref self`, a String field and a `Vec[i64]` field; memory
/// safety pinned by `tests/memory_sanitizer.rs::asan_ref_self_field_return_*`.
#[test]
fn e2e_ref_self_heap_field_return_codegen() {
    if let Some(out) = run_program(
        "struct Person { n: String, tags: Vec[i64] }\n\
             impl Person {\n\
                 fn name(ref self) -> String { self.n }\n\
                 fn take_tags(mut ref self) -> Vec[i64] { self.tags }\n\
             }\n\
             fn main() {\n\
                 let mut p = Person { n: \"alice\".to_string(), tags: [1i64, 2i64, 3i64] };\n\
                 let a = p.name();\n\
                 println(a);\n\
                 println(p.n);\n\
                 let t = p.take_tags();\n\
                 println(f\"{t.len()}\");\n\
                 println(f\"{p.tags.len()}\");\n\
             }",
    ) {
        assert_eq!(out, "alice\nalice\n3\n3\n");
    }
}

#[test]
fn e2e_let_init_returns_with_live_heap_local() {
    // A heap local is in scope when the init returns: the return edge's
    // scope-exit cleanup drains it (no leak — see the asan twin).
    if let Some(out) = run_program(
        "fn t() -> i64 {\n\
                 let s = f\"heap-{40 + 2}\";\n\
                 let x = { return s.len(); };\n\
                 0\n\
             }\n\
             fn main() { println(f\"{t()}\"); }",
    ) {
        assert_eq!(out, "7\n");
    }
}

#[test]
fn test_e2e_partition_heap_elements() {
    // B-2026-07-19-15 follow-on — `partition` over HEAP elements (String).
    // Each element pushed into a partition Vec is CLONED (`param.clone()`) so
    // the owning target Vec doesn't alias the borrowed source element (a
    // shallow push would double-free); the source Vec survives intact. Must
    // match the interpreter; leak-freedom is gated in
    // `tests/memory_sanitizer.rs::asan_partition_heap_string_no_leak`.
    if let Some(out) = run_program(
            "fn main() {\n\
                 let words: Vec[String] = [\"apple\", \"banana\", \"avocado\", \"cherry\", \"apricot\"];\n\
                 let (a, other): (Vec[String], Vec[String]) = words.iter().partition(|w| w.starts_with(\"a\"));\n\
                 println(a.len());\n\
                 println(a.get(0));\n\
                 println(a.get(2));\n\
                 println(other.len());\n\
                 println(other.get(0));\n\
                 // source survives\n\
                 println(words.len());\n\
                 println(words.get(0));\n\
             }",
        ) {
            assert_eq!(
                out,
                "3\nSome(apple)\nSome(apricot)\n2\nSome(banana)\n5\nSome(apple)\n"
            );
        }
}

#[test]
fn test_e2e_shared_struct_heap_field_read_is_a_copy() {
    // B-2026-08-13-6's VALUE side, and the leg that decides between the two
    // candidate fixes. Cap-zeroing the source — the move model that
    // reconciles a NON-shared local's field move-out — would empty the RC
    // box, so `j.word` (a SECOND handle to the same box, taken after the
    // field was bound out through the first) is the assertion that rules it
    // out: it must still read `a1`.
    //
    // `hs[0].tag` last: the scalar sibling of the cloned field must survive,
    // which a clone emitted at the wrong offset would corrupt.
    assert_eq!(
        run_program(
            "shared struct Inner { word: String }\n\
                 struct Holder { inner: Inner, tag: i64 }\n\
                 fn main() {\n\
                     let k = 1;\n\
                     let i = Inner { word: f\"a{k}\" };\n\
                     let w = i.word;\n\
                     println(w);\n\
                     println(i.word);\n\
                     let j = i;\n\
                     println(j.word);\n\
                     let mut hs: Vec[Holder] = Vec.new();\n\
                     hs.push(Holder { inner: Inner { word: f\"c{k}\" }, tag: 2 });\n\
                     let hop = hs[0].inner.word;\n\
                     println(hop);\n\
                     println(hs[0].inner.word);\n\
                     println(hs[0].tag);\n\
                 }"
        )
        .as_deref(),
        Some("a1\na1\na1\nc1\nc1\n2\n"),
    );
}

#[test]
fn test_e2e_generic_struct_multi_heap_field_method_receiver() {
    // B-2026-07-15-17: a `<generic-struct>.field.method()` receiver GEP'd the
    // field using the BASE generic struct type (every generic-param field
    // erased to i64 = 1 word) instead of the per-monomorph type. So for
    // `Pair[Vec, Vec]` the field-1 GEP landed at byte 8 (word 1 of field 0's
    // `{ptr,len,cap}`) — `p.second.len()` silently returned the FIRST field's
    // length. A single wide field / scalar field was fine (the scalar-field
    // read path uses the loaded mono VALUE); only the field-receiver
    // method-dispatch GEP was wrong. Fixed by GEPing with the mono struct
    // type. Covers read (`.len()`), mutate (`.push()` write-through), and a
    // 3-field / 2-type-param interleave. (The struct itself leaks its heap
    // fields at scope exit — the separate B-2026-07-15-11 generic-struct drop
    // gap — so this is an output-only regression, not an asan test.)
    if let Some(out) = run_program(
            "struct Pair[A, B] { first: A, second: B }\n\
             struct Rec[A, B] { a: A, b: B, c: A }\n\
             fn main() {\n\
                 let mut p: Pair[Vec[i64], Vec[i64]] = Pair { first: [10, 20], second: [1, 2, 3] };\n\
                 println(p.first.len());\n\
                 println(p.second.len());\n\
                 p.second.push(99);\n\
                 p.first.push(30);\n\
                 println(p.first.len());\n\
                 println(p.second.len());\n\
                 println(p.second[3]);\n\
                 println(p.first[2]);\n\
                 let mut r: Rec[Vec[i64], String] = Rec { a: [1, 2], b: \"mid\", c: [7, 8, 9, 10] };\n\
                 r.c.push(11);\n\
                 println(r.a.len());\n\
                 println(r.b.len());\n\
                 println(r.c.len());\n\
                 println(r.c[4]);\n\
             }",
        ) {
            assert_eq!(out, "2\n3\n3\n4\n99\n30\n2\n3\n5\n11\n");
        }
}

#[test]
fn test_e2e_reduction_over_shared_pool_declined_and_correct() {
    // B-2026-07-16-6: the auto-par reduction recognizer lowered
    // `total = total + sum(pool[k % 8])` into a multi-threaded
    // `karac_par_reduce` worker even though the captured pool holds
    // plain `shared` trees — every worker then raced the NON-atomic
    // rc-inc (worker body, element retain) / rc-dec (callee-drop
    // inside `sum`) pairs on the same 8 root nodes, driving refcounts
    // to zero while the trees were still live: use-after-free reads
    // (garbage `n.val` → spurious "integer overflow" panics) and
    // glibc heap-corruption aborts within ~500 iterations. The fix
    // gates reduction recognition on the same cross-task-safe
    // predicate as explicit `spawn` captures, so this loop lowers
    // sequentially and must produce the exact sum deterministically.
    if let Some(out) = run_program(
        "shared struct TreeNode {\n\
                 val: i64,\n\
                 left: Option[TreeNode],\n\
                 right: Option[TreeNode],\n\
             }\n\
             fn build(depth: i64, counter: i64) -> Option[TreeNode] {\n\
                 if depth == 0 {\n\
                     return None;\n\
                 }\n\
                 let left = build(depth - 1, counter * 2);\n\
                 let right = build(depth - 1, counter * 2 + 1);\n\
                 return Some(TreeNode { val: counter, left: left, right: right });\n\
             }\n\
             fn sum(node: Option[TreeNode]) -> i64 {\n\
                 match node {\n\
                     None => 0,\n\
                     Some(n) => n.val + sum(n.left) + sum(n.right),\n\
                 }\n\
             }\n\
             fn main() {\n\
                 let mut pool: Vec[Option[TreeNode]] = Vec.new();\n\
                 let mut i = 0;\n\
                 while i < 8 {\n\
                     pool.push(build(5, 1));\n\
                     i = i + 1;\n\
                 }\n\
                 let mut total = 0;\n\
                 let mut rep = 0;\n\
                 while rep < 1000 {\n\
                     total = total + sum(pool[rep % 8].clone());\n\
                     rep = rep + 1;\n\
                 }\n\
                 println(total);\n\
             }",
    ) {
        assert_eq!(out, "496000\n");
    }
}

#[test]
fn test_e2e_heap_example() {
    // examples/heap.kara — a generic binary MIN-heap `Heap[T: Ord]` +
    // heapsort. Validates the GENERIC-MONOMORPHIZATION surface end to end,
    // composing the fixes this dogfood thread produced: the associated
    // constructor `Heap.new()` (B-2026-07-11-25), the void sift helpers with
    // tail/early-return `if`s (B-2026-07-11-28), `T: Ord` `>`/`<` comparison,
    // `Vec[T]` index read/assign through `self`, and `Option[T]` return/match.
    // Heapsort of a fixed permutation yields ascending order; a PQ drain does
    // too; an empty pop yields None.
    if let Some(out) = run_program(include_str!("../../examples/heap.kara")) {
        assert_eq!(
            out,
            "0 1 2 3 4 5 6 7 8 9\nsize=6\n4 17 23 42 58 99\nempty-ok\n"
        );
    }
}

#[test]
fn test_generic_shared_struct_heap_field_rejected_in_codegen() {
    // B-2026-07-13-9 — a generic `shared struct Box[T] { v: T }`
    // instantiated at a HEAP type (`Box[String]`) erases its `v: T` field
    // to a single word (the shared heap layout is built once, at
    // declaration, before any instantiation), so the 3-word `String`
    // aggregate overflowed the slot: silent wrong output on native and a
    // `free(): invalid next size` abort under the allocator on JIT. The
    // native/JIT backend does not monomorphize the shared heap layout (nor
    // its RC-drop field classifier) at v1, so the store site now refuses
    // LOUDLY rather than corrupt. (Scalar `Box[i64]` still compiles — see
    // test_e2e_generic_shared_struct_scalar_field_ok; the interpreter
    // handles the heap case correctly — see the interpreter oracle pin
    // test_generic_shared_struct_heap_field_aliases in tests/interpreter.rs.)
    let err = ir_result(
        "shared struct Box[T] { mut v: T }\n\
             fn main() {\n\
                 let a = Box { v: \"hi\" };\n\
                 let b = a;\n\
                 b.v = \"bye\";\n\
                 println(a.v);\n\
             }",
    )
    .expect_err("expected a codegen refusal for a heap-typed generic shared-struct field");
    assert!(
        err.contains("shared") && err.contains("generic") && err.contains("B-2026-07-13-9"),
        "expected a loud generic-shared-heap-field refusal, got: {err}"
    );
}

#[test]
fn test_e2e_generic_shared_struct_scalar_field_ok() {
    // B-2026-07-13-9 sibling — the rejection is scoped to a value WIDER than
    // the erased 1-word slot. A scalar instantiation (`Box[i64]`) fits the
    // slot exactly, so reference-semantics aliasing through the RC handle
    // still compiles and runs: `b = a; b.v = 9; a.v` reads 9.
    if let Some(out) = run_program(
        "shared struct Box[T] { mut v: T }\n\
             fn main() {\n\
                 let a = Box { v: 5i64 };\n\
                 let b = a;\n\
                 b.v = 9i64;\n\
                 println(f\"{a.v}\");\n\
             }",
    ) {
        assert_eq!(out.trim(), "9");
    }
}

#[test]
fn test_e2e_borrow_return_method_on_heap_result() {
    // B-2026-06-10-5: direct-use of a borrow-returning call as a method
    // receiver, on a HEAP (cap>0) source. `name_of(s).len()` after
    // `push_str` previously crashed: the value-receiver `len` path
    // materialized the loaded borrow as a "fresh owned temp" and queued a
    // free of `s`'s buffer. Asserts the value AND post-use of `s` (a
    // duplicate free would corrupt the second read or crash). The
    // `test_e2e_borrow_return_direct_use` test covers the cap-0
    // `String.from` case that masked this.
    let out = run_program(
        "fn name_of(u: ref String) -> ref String { u }\n\
             fn main() {\n\
             \x20   let mut s: String = \"\";\n\
             \x20   s.push_str(\"hello\");\n\
             \x20   println(name_of(s).len());\n\
             \x20   println(name_of(s).is_empty());\n\
             \x20   println(s);\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "5\nfalse\nhello");
    }
}

#[test]
fn e2e_fn_value_shared_param_still_receives_the_handle_not_its_slot() {
    // The discrimination the by-pointer branch must NOT get wrong: a
    // `shared` param also lowers to `ptr`, but its argument is ALREADY a
    // pointer. Taking the address of the binding's slot would hand the
    // callee a pointer-to-pointer, and `n.v` would read the stack.
    let out = run_program(
        "shared struct Node { v: i64 }\n\
             fn get(n: Node) -> i64 { return n.v; }\n\
             fn main() {\n\
                 let n: Node = Node { v: 41i64 };\n\
                 let f = get;\n\
                 println(f(n) + 1i64);\n\
             }",
    );
    assert_eq!(out, Some("42\n".to_string()));
}

// ── Shared struct (RC) tests ────────────────────────────────

#[test]
fn test_ir_shared_struct_malloc() {
    let ir = ir_for(
        r#"
shared struct Node { val: i64 }
fn make() -> Node { Node { val: 42 } }
"#,
    );
    // Should heap-allocate via malloc and store refcount = 1.
    assert!(ir.contains("@malloc"), "shared struct should call malloc");
    assert!(
        ir.contains("store i64 1"),
        "should store initial refcount of 1"
    );
}

#[test]
fn test_ir_shared_struct_field_gep() {
    let ir = ir_for(
        r#"
shared struct Point { x: i64, y: i64 }
fn read_x(p: Point) -> i64 { p.x }
"#,
    );
    // Field access on shared type should use GEP, not extractvalue.
    assert!(
        ir.contains("getelementptr"),
        "shared struct field access should use GEP"
    );
}

// `self.field` inside a `ref self` / `mut ref self` method on a shared
// (RC) struct. Before this fix, `shared_type_for_expr` didn't resolve the
// `SelfValue` receiver, so `self.field` skipped the heap-GEP path and fell
// to a const-0 fallback — a *read* returned 0 and a *write* missed the heap
// object (didn't persist). The interpreter always handled these; these pin
// the AOT path.

#[test]
fn test_e2e_shared_method_field_read() {
    // `get(ref self) -> i64 { self.n }` must return the stored field, not 0.
    let out = run_program(
        r#"
shared struct Scell { n: i64 }
impl Scell { pub fn get(ref self) -> i64 { self.n } }
fn main() { let c = Scell { n: 5 }; println(c.get()); }
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "5");
    }
}

#[test]
fn test_e2e_shared_method_field_mutate_persists() {
    // `inc(mut ref self) { self.n = self.n + 1 }` must persist to the heap
    // object so a subsequent read observes it.
    let out = run_program(
        r#"
shared struct Scell { mut n: i64 }
impl Scell {
    pub fn inc(mut ref self) { self.n = self.n + 1; }
    pub fn get(ref self) -> i64 { self.n }
}
fn main() { let c = Scell { n: 5 }; c.inc(); println(c.get()); }
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "6");
    }
}

#[test]
fn test_e2e_shared_method_field_mutation_aliases() {
    // RC reference semantics: a mutation through one binding is visible
    // through an alias — confirms the write hits the shared heap object,
    // not a per-binding copy.
    let out = run_program(
        r#"
shared struct Scell { mut n: i64 }
impl Scell {
    pub fn inc(mut ref self) { self.n = self.n + 10; }
    pub fn get(ref self) -> i64 { self.n }
}
fn main() { let a = Scell { n: 1 }; let b = a; a.inc(); println(b.get()); }
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "11");
    }
}

#[test]
fn test_ir_shared_struct_rc_dec_free() {
    let ir = ir_for(
        r#"
shared struct Token { id: i64 }
fn use_token() {
    let t = Token { id: 1 };
}
"#,
    );
    // Scope exit should decrement and conditionally free.
    assert!(
        ir.contains("@free"),
        "shared struct scope exit should call free"
    );
}

#[test]
fn test_ir_shared_struct_rc_inc_on_copy() {
    let ir = ir_for(
        r#"
shared struct Obj { data: i64 }
fn copy_shared() {
    let a = Obj { data: 10 };
    let b = a;
}
"#,
    );
    // Copying `a` to `b` should increment refcount.
    // The IR should contain at least two references to the rc add pattern.
    let rc_inc_count = ir.matches("add i64 %rc").count();
    assert!(
        rc_inc_count >= 1,
        "copying shared var should produce rc_inc (found {} occurrences)",
        rc_inc_count
    );
}

#[test]
fn test_e2e_shared_struct_basic() {
    let out = run_program(
        r#"
shared struct Counter { val: i64 }
fn main() {
    let c = Counter { val: 42 };
    println(c.val);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "42");
    }
}

#[test]
fn test_e2e_shared_struct_structural_equality() {
    // C1 (B-2026-06-19-9): structural `==`/`!=` on `shared struct` values
    // now lowers in codegen (was interpreter-only). Covers the direct
    // scalar-field compare, the `Arc::ptr_eq` self-compare fast-path,
    // nested shared-struct fields, and a String field — A/B parity with
    // the interpreter test `test_shared_struct_structural_equality`.
    let out = run_program(
        r#"
#[derive(Eq, PartialEq)]
shared struct P { x: i64, y: i64 }
#[derive(Eq, PartialEq)]
shared struct Line { a: P, b: P, label: String }
fn main() {
    let a = P { x: 1, y: 2 };
    let b = P { x: 1, y: 2 };
    let c = P { x: 9, y: 2 };
    if a == b { println("eq"); }
    if a != c { println("ne"); }
    if a == a { println("self"); }
    let l1 = Line { a: P { x: 1, y: 2 }, b: P { x: 3, y: 4 }, label: "seg" };
    let l2 = Line { a: P { x: 1, y: 2 }, b: P { x: 3, y: 4 }, label: "seg" };
    let l3 = Line { a: P { x: 1, y: 2 }, b: P { x: 3, y: 9 }, label: "seg" };
    let l4 = Line { a: P { x: 1, y: 2 }, b: P { x: 3, y: 4 }, label: "DIFF" };
    if l1 == l2 { println("line-eq"); }
    if l1 != l3 { println("line-ne-field"); }
    if l1 != l4 { println("line-ne-string"); }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(
            lines,
            vec![
                "eq",
                "ne",
                "self",
                "line-eq",
                "line-ne-field",
                "line-ne-string"
            ]
        );
    }
}

#[test]
fn test_e2e_shared_struct_alias() {
    let out = run_program(
        r#"
shared struct Data { x: i64 }
fn main() {
    let a = Data { x: 100 };
    let b = a;
    println(a.x);
    println(b.x);
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["100", "100"]);
    }
}

#[test]
fn test_e2e_shared_struct_passed_to_fn() {
    let out = run_program(
        r#"
shared struct Wrapper { val: i64 }
fn read_val(w: Wrapper) -> i64 { w.val }
fn main() {
    let w = Wrapper { val: 77 };
    println(read_val(w));
    println(w.val);
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["77", "77"]);
    }
}

#[test]
fn test_e2e_shared_struct_multiple_fields() {
    let out = run_program(
        r#"
shared struct Vec2 { x: i64, y: i64 }
fn sum(v: Vec2) -> i64 { v.x + v.y }
fn main() {
    let v = Vec2 { x: 3, y: 7 };
    println(sum(v));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "10");
    }
}

// ── Bug #7 regression: shared-struct move-out from a tracked local ──
//
// The let-site `track_rc_var` queues a scope-exit `RcDec` for the
// freshly-constructed shared local. When that local is then moved
// into a sink that takes ownership (function tail-return, `Map.insert`'s
// bucket, `Vec.push`'s buffer), the source's scope-exit dec used to
// fire against the construction-time RC=1 and free the allocation
// *before* the consumer could observe it.  Symptom: silent data
// corruption on the moved-out value (Repro A) or a hang in the
// follow-on caller-side `rc_inc` against use-after-free memory
// (Repro B). The fix balances the upcoming dec by emitting an
// `rc_inc` at each move-out site so the consumer holds an
// independent ref — symmetric to the Vec/String `cap=0` skip and
// to the existing `let b = a;` aliasing inc.

#[test]
fn test_e2e_bug7_shared_struct_return_from_helper() {
    // The minimal repro: `let n = SharedT { … }; n` as the tail
    // expression of a helper.  Before the fix this printed garbage
    // (`4` or `0` depending on what the freed alloc got reused as);
    // after the fix it prints the original 42.
    let out = run_program(
        r#"
shared struct Node { val: i64 }
fn helper() -> Node {
    let n = Node { val: 42 };
    n
}
fn main() {
    let r = helper();
    println(r.val);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "42");
    }
}

#[test]
fn test_ir_bug7_shared_struct_move_out_emits_rc_inc() {
    // IR-level gate so a future regression that drops the inc-on-
    // move-out is caught immediately, not just by the e2e tests.
    // The helper body must contain at least two RC adjustments
    // against `n` (the move-out `rc_inc` + the scope-exit `rc_dec`),
    // both lowering to plain non-atomic `add/sub i64` on the
    // refcount field at offset 0 of the heap struct.  Before the
    // fix only the `rc_dec` was emitted, leaving the moved-out
    // pointer at RC=0 (freed) at the `ret`.
    let ir = ir_for(
        r#"
shared struct Node { val: i64 }
fn helper() -> Node {
    let n = Node { val: 42 };
    n
}
"#,
    );
    let inc_count = ir.matches("add i64 %rc").count();
    let dec_count = ir.matches("sub i64 %rc").count();
    assert!(
        inc_count >= 1,
        "helper should emit rc_inc on move-out (found {} `add i64 %rc` ops)",
        inc_count
    );
    assert!(
        dec_count >= 1,
        "helper should still emit rc_dec on scope exit (found {} `sub i64 %rc` ops)",
        dec_count
    );
}

// ── Bug #8: call-chain field access on shared-struct return ───
//
// Sibling of bug #7's move-out aliasing class.  The bug #7 fix made
// a tail-return `n` on a shared-struct local emit `rc_inc` so the
// returned pointer arrives at the caller with RC ≥ 1.  When the
// caller binds the result to a local (`let r = helper(); r.val`),
// the `let` registration through `track_rc_var` schedules a
// scope-exit `rc_dec` and the field-access path lowers through the
// existing `shared_type_for_expr` Identifier arm.  But when the
// result is *not* bound — `println(helper().val)` — neither piece
// applied: the field access fell through to the generic
// `StructValue` extract (the call returns a `PointerValue`, not a
// struct value), which silently returns `i64 0` because the
// unknown-shape path uses that as its inert default.  The fix adds
// a call-shaped `shared_type_for_call_like` recognizer and lowers
// the access via GEP + load + `rc_dec` on the temp so the heap
// object the callee handed us is released after the field is read.
// Symmetric to the cleanup pattern a `let` would attach.

#[test]
fn test_e2e_bug8_call_chain_field_shared_return() {
    // The minimal repro: `println(helper().val)` where `helper()`
    // returns a shared struct.  Before the fix this printed 0
    // (the field-access fall-through default); after the fix it
    // prints the original 42.
    let out = run_program(
        r#"
shared struct Node { val: i64 }
fn helper() -> Node {
    let n = Node { val: 42 };
    n
}
fn main() {
    println(helper().val);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "42");
    }
}

#[test]
fn test_e2e_splice_retain_before_release() {
    // Regression for the `Option[shared T]` field-store retain-order
    // bug (2026-05-30, kata #19). `slow.next = target.next` splices a
    // node out of a list: the new value (`target.next`) is reachable
    // only *through* the old value (`target`). The store must retain
    // the new inner BEFORE releasing the old — otherwise the old
    // node's drop recursively dec_refs the new node to zero and frees
    // it out from under the store, and the result list is corrupt
    // (pre-fix: trap / garbage like `[1,2,3,3]`).
    //
    // Shape mirrors LeetCode #19 `remove_nth_from_end`: build a list
    // in `from_array`, wrap it in a dummy, two-pointer to the node
    // before the target, splice. Removing the 2nd-from-end of
    // [1,2,3,4,5] drops the 4 → [1,2,3,5].
    let out = run_program(
        r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
fn from_array(arr: Slice[i64]) -> Option[ListNode] {
    let n = arr.len();
    if n == 0 { return None; }
    let head = ListNode { val: arr[0], next: None };
    let mut tail = head;
    for i in 1..n {
        let node = ListNode { val: arr[i], next: None };
        tail.next = Some(node);
        tail = node;
    }
    Some(head)
}
fn remove_nth_from_end(head: Option[ListNode], n: i64) -> Option[ListNode] {
    let dummy = ListNode { val: 0, next: head };
    let mut fast = head;
    let mut i = 0i64;
    while i < n {
        if let Some(node) = fast { fast = node.next; }
        i = i + 1i64;
    }
    let mut slow = dummy;
    loop {
        match fast {
            Some(node) => { fast = node.next; if let Some(s) = slow.next { slow = s; } }
            None => break,
        }
    }
    if let Some(target) = slow.next { slow.next = target.next; }
    dummy.next
}
fn main() {
    let data: Array[i64, 5] = [1, 2, 3, 4, 5];
    let head = from_array(data);
    let out = remove_nth_from_end(head, 2i64);
    let mut cur = out;
    loop {
        match cur {
            Some(node) => { println(node.val); cur = node.next; }
            None => break,
        }
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "1\n2\n3\n5");
    }
}

#[test]
fn test_e2e_continue_drains_loop_body_shared_binding() {
    // `continue` sibling of the `break` drain: jumping to the loop
    // header skips the body-end back-edge drain, so a shared
    // binding bound earlier in the iteration kept its +1 forever
    // (leak) — and the next iteration's re-bind inc'd again.
    // `compile_continue` now drains frames `[cleanup_depth..]`
    // before branching (emit-only, null-guarded reload-by-name).
    // The observable here is correctness of the counts: the chain
    // must still be fully walkable afterward (an over-dec variant
    // would free nodes mid-walk and print garbage).
    let out = run_program(
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
fn sum_odd_positions(head: Option[ListNode]) -> i64 {
    let mut total = 0;
    let mut cur = head;
    let mut idx = 0;
    loop {
        if let Some(node) = cur {
            cur = node.next;
            idx = idx + 1;
            if idx - (idx / 2) * 2 == 0 {
                continue;
            }
            total = total + node.val;
        } else {
            break;
        }
    }
    total
}
fn main() {
    let head = build(6);
    println(sum_odd_positions(head)); // 1 + 3 + 5 = 9
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "9");
    }
}

#[test]
fn test_e2e_bug8_method_call_chain_field_shared_return() {
    // MethodCall sibling of `test_e2e_bug8_call_chain_field_shared_return`.
    // `holder.make().val` is a MethodCall on an Identifier
    // receiver that returns a shared struct, then bare `.val`
    // access. Before this fix `shared_type_for_call_like`
    // hard-deferred MethodCall to a None branch — the field
    // fell through to the generic StructValue extract and
    // silently loaded `i64 0`. After the fix the same
    // `fn_return_type_names` lookup as the free-fn / 2-segment
    // Path paths fires, keyed by the synthesized `Type.method`
    // name (`Holder.make`), and the field path lowers correctly.
    let out = run_program(
        r#"
shared struct S { val: i64 }
struct Holder { tag: i64 }
impl Holder {
    fn make(self) -> S {
        let s = S { val: 99 };
        s
    }
}
fn main() {
    let h = Holder { tag: 0 };
    println(h.make().val);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "99");
    }
}

// ── Bug #8 regression: receive-side double-inc on function return ──
//
// After bug #7's fix added a callee-side `rc_inc` at each move-out
// site, the callee transferred +1 to the caller via the return
// value (its scope-exit `rc_dec` balanced its own move-out inc, so
// the returned pointer carries a net +1 over what the caller held
// before the call). The receive site in `compile_stmt` —
// `let x = make()` — kept incrementing on receive, doubling the
// refcount: the receiver's scope-exit dec then dropped rc from 2
// to 1 instead of 1 to 0, so the heap object was never freed.
// Symmetric one-ref leak on every shared-struct function-return
// crossing.
//
// Convention after the fix: the function-return path owns the +1.
// The caller does NOT emit `rc_inc` on receive when the RHS of a
// let-binding (or an Assign target's RHS) is itself a `Call` — the
// value already carries a freshly-transferred ref. Identifier /
// FieldAccess / Index RHS shapes still alias an existing ref and
// need the inc.

#[test]
fn test_ir_bug8_shared_struct_return_receive_no_double_inc() {
    // The IR-level gate: across `make()` + `let x = make()`, the
    // module must contain exactly one `add i64 %rc` (the callee-
    // side move-out inc inside `make`) and exactly two `sub i64
    // %rc` (callee's scope-exit dec on `s` + caller's scope-exit
    // dec on `x`). Before the fix `add` appeared twice — once
    // inside `make` and once at the `let x = make()` receive site
    // — which leaked one ref per crossing.
    let ir = ir_for(
        r#"
shared struct S { val: i64 }
fn make() -> S {
    let s = S { val: 42 };
    s
}
fn use_it() {
    let x = make();
}
"#,
    );
    let inc_count = ir.matches("add i64 %rc").count();
    let dec_count = ir.matches("sub i64 %rc").count();
    assert_eq!(
        inc_count, 1,
        "make+receive should emit exactly one rc_inc (callee move-out only; \
             receiver must not inc on a Call RHS — that doubles the refcount and leaks)\n\
             found {} `add i64 %rc` ops in:\n{}",
        inc_count, ir
    );
    assert_eq!(
        dec_count, 2,
        "make+receive should emit two rc_decs (callee scope-exit + caller scope-exit); \
             found {} `sub i64 %rc` ops in:\n{}",
        dec_count, ir
    );
}

#[test]
fn test_e2e_bug8_shared_struct_return_no_leak() {
    // E2E guard for the asymmetric move-out/receive-cross convention.
    // The repro itself prints `42` (the value side was correct
    // before this fix too — refcount=2 vs refcount=1 doesn't change
    // the pointee's bytes); locking the e2e here documents the
    // intended program behavior and pairs with the IR-level gates
    // above so a future regression that flips back to double-incing
    // is caught at both surfaces.
    let out = run_program(
        r#"
shared struct S { val: i64 }
fn make() -> S {
    let s = S { val: 42 };
    s
}
fn main() {
    let x = make();
    println(x.val);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "42");
    }
}

#[test]
fn test_e2e_block_expr_value_heap_return() {
    // B-2026-06-11-2: a block expression USED AS A VALUE whose tail is a
    // scope-registered heap value (an f-string accumulator or a block-local
    // `let`-bound String) lost the value under AOT — it printed empty. The
    // block's `compile_block_with_frame` loaded the tail, then drained its
    // own frame, freeing the tail's buffer between the load and the value
    // escaping (a use-after-free). Fix: suppress the tail value's cleanup
    // before the block-frame drain (mirrors `compile_function`'s tail
    // handling) so the consumer's own binding is the sole owner. Covers
    // every consumer the bug surfaced across: plain let-RHS (f-string and
    // identifier tail), `if`-arm value, a block-`let` inside a loop, a
    // brace-WRAPPED match arm (f-string and identifier tail), a nested
    // block, a function-return block, and a struct-field initializer block.
    let out = run_program(
        r#"
enum E { A(String), B }
fn mk() -> String { { f"ret{1}" } }
struct S { name: String }
fn main() {
    let a = { f"a{1}" };
    println(a);
    let b = { let p = "x" + "y"; p };
    println(b);
    let c = if true { f"yes{2}" } else { f"no{0}" };
    println(c);
    let mut i = 0i64;
    while i < 2i64 { let v = { f"v{i}" }; println(v); i = i + 1i64; }
    let e1 = E.A("m");
    let d = match e1 { E.A(n) => { f"<{n}>" }, E.B => "z" };
    println(d);
    let e2 = E.A("k");
    let g = match e2 { E.A(n) => { let p = f"[{n}]"; p }, E.B => "z" };
    println(g);
    let h = { { f"nest{3}" } };
    println(h);
    println(mk());
    let s = S { name: { f"fld{4}" } };
    println(s.name);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "a1\nxy\nyes2\nv0\nv1\n<m>\n[k]\nnest3\nret1\nfld4\n");
    }
}

// ── RC-fallback codegen E2E ──────────────────────────────────

#[test]
fn test_e2e_rc_fallback_param_copy_type() {
    // A Copy-type (i64) parameter flagged by the ownership checker for RC-fallback
    // (consumed in an if-branch, then used again after). The value should still be
    // accessible after the branch — RC boxing allows the second use.
    let out = run_program_with_ownership(
        r#"
fn sink(x: i64) { }
fn main() {
    let val: i64 = 99;
    let cond: bool = false;
    if cond { sink(val); }
    println(val);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "99");
    }
}

#[test]
fn test_e2e_rc_fallback_struct_param() {
    // A non-Copy struct parameter consumed in one branch then used after.
    // With RC boxing the second use loads T from the heap object and behaves
    // identically to the non-RC case (the observable output is the same).
    let out = run_program_with_ownership(
        r#"
struct Point { x: i64, y: i64 }
fn consume_p(p: Point) { }
fn main() {
    let cond: bool = false;
    let p = Point { x: 3, y: 7 };
    if cond { consume_p(p); }
    println(p.x + p.y);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "10");
    }
}

#[test]
fn test_e2e_rc_fallback_binding_at_ref_arg_site() {
    // B-2026-06-13-1: an RC-fallback-promoted binding passed at a
    // `ref`/`mut ref` arg position (method *and* free fn). The previous
    // `get_data_ptr` returned the binding's alloca slot — which, for an
    // RC-promoted binding, holds a *pointer to the heap box* `{ i64 rc, T }`,
    // not the value. The callee then read the refcount header as the
    // struct's first field (observably wrong: `total=-1, read=-1`), and a
    // field write-through could zero the box pointer → later `null+8`
    // deref. The field-*read* path (`load_variable`) was already RC-aware;
    // this guards the ref-*arg* path now that `get_data_ptr` is too.
    //
    // Shape: a non-Copy struct consumed in one branch (dominance-
    // incomparable consume/use → RC fallback, not UseAfterMove), then
    // passed by `ref` to a method and to a free fn. Must run with the
    // ownership result threaded so `is_rc_fallback_binding` fires.
    let out = run_program_with_ownership(
        r#"
struct Src { x: i64, y: i64 }
struct Acc { total: i64 }
fn consume_src(s: Src) { }
fn read_ref(s: ref Src) -> i64 { s.x + s.y }
impl Acc {
    fn add(mut ref self, s: ref Src) { self.total = self.total + s.x + s.y; }
}
fn main() {
    let cond: bool = false;
    let mut a = Acc { total: 0 };
    let s = Src { x: 3, y: 7 };
    if cond { consume_src(s); }
    a.add(s);
    let r: i64 = read_ref(s);
    println(a.total);
    println(r);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "10\n10\n");
    }
}

#[test]
fn test_e2e_rc_fallback_binding_at_mut_ref_arg_write() {
    // B-2026-06-13-1, severe variant: an RC-fallback-promoted binding
    // passed at a `mut ref` arg whose body *writes* a field. With the old
    // `get_data_ptr` the callee received the box-pointer slot address, so
    // `s.x = ...` wrote the new field value over the box pointer itself;
    // the later `s.x` read then loaded a null box pointer and deref'd
    // `null + 8` → segfault (or, as observed pre-fix, a silent `x=0, y=0`
    // miscompile). With `get_data_ptr` RC-aware the write lands in the
    // heap box's value at field 1, so the mutation and later read agree.
    let out = run_program_with_ownership(
        r#"
struct Src { x: i64, y: i64 }
fn consume_src(s: Src) { }
fn bump(s: mut ref Src) { s.x = s.x + 100; }
fn main() {
    let cond: bool = false;
    let mut s = Src { x: 3, y: 7 };
    if cond { consume_src(s); }
    bump(mut s);
    println(s.x);
    println(s.y);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "103\n7\n");
    }
}

// ── RC elision phase A (ownership/elision.rs) ──────────────────
//
// Trivial intra-fn single-owner shared bindings skip the
// dec/zero-test/drop-fn scope-exit dance: the let-site queues
// `FreeSharedElided` (unconditional null-guarded free). Design
// record: phase-7-codegen.md § "RC elision for provably-
// single-owner `shared struct` values".

#[test]
fn test_ir_rc_elision_scratch_binding_frees_without_dec() {
    let ir = ir_for_with_ownership(
        r#"
shared struct Stats { mut count: i64, mut total: i64 }
fn main() {
    let s = Stats { count: 0, total: 0 };
    s.total = s.total + 5;
    println(s.total);
}
"#,
    );
    let body = function_body(&ir, "main").expect("main body");
    assert!(
        body.contains("elide_free_do"),
        "elided binding should free via the FreeSharedElided arm; main:\n{body}"
    );
    assert!(
        !body.contains("s_rc_cleanup"),
        "elided binding must not take the RcDec path; main:\n{body}"
    );
    // The rc=1 header store stays (layout uniformity — design
    // decision 2); only the count OPERATIONS are elided.
    assert!(
        body.contains("rc_alloc"),
        "allocation keeps the rc header; main:\n{body}"
    );
}

#[test]
fn test_ir_rc_elision_aliased_binding_keeps_rc_dec() {
    // Control: the alias disqualifies, so the binding keeps the
    // conventional RcDec cleanup and never takes the elided free.
    let ir = ir_for_with_ownership(
        r#"
shared struct Stats { mut count: i64, mut total: i64 }
fn main() {
    let s = Stats { count: 0, total: 0 };
    let t = s;
    println(t.total);
}
"#,
    );
    let body = function_body(&ir, "main").expect("main body");
    assert!(
        body.contains("s_rc_cleanup"),
        "aliased binding must keep the RcDec cleanup; main:\n{body}"
    );
    assert!(
        !body.contains("s_elide_cleanup"),
        "aliased binding must not take the elided free; main:\n{body}"
    );
}

#[test]
fn test_e2e_rc_elision_scratch_loop() {
    // Per-iteration elided scratch object: field writes, ref-self
    // methods, a read-only declared-owned callee (inferred Ref —
    // the would-be-mode gate), and a ref-arg callee. Exact total
    // proves value correctness; the ASAN sibling proves the frees
    // balance.
    let out = run_program_with_ownership(
        r#"
shared struct Stats { mut count: i64, mut total: i64, active: bool }
fn reader(s: ref Stats) -> i64 {
    s.total
}
fn read_only(s: Stats) -> i64 {
    s.count
}
impl Stats {
    fn bump(mut ref self, n: i64) {
        self.count = self.count + 1;
        self.total = self.total + n;
    }
    fn snapshot(ref self) -> i64 {
        self.total
    }
}
fn main() {
    let mut grand = 0;
    let mut iter = 0;
    while iter < 64 {
        let s = Stats { count: 0, total: 0, active: true };
        s.bump(3);
        s.bump(4);
        grand = grand + s.snapshot() + reader(s) + read_only(s);
        iter = iter + 1;
    }
    println(grand);
}
"#,
    );
    if let Some(out) = out {
        // per iter: snapshot 7 + reader 7 + read_only(count) 2 = 16
        assert_eq!(out.trim(), "1024");
    }
}

#[test]
fn test_e2e_rc_elision_conditional_binding_null_guard() {
    // The elided let sits in a conditional branch — when skipped,
    // the slot carries the entry-block null sentinel and the
    // FreeSharedElided arm's null-guard must skip the free.
    let out = run_program_with_ownership(
        r#"
shared struct Stats { mut count: i64, mut total: i64 }
fn main() {
    let mut grand = 0;
    let mut iter = 0;
    while iter < 8 {
        if iter > 3 {
            let s = Stats { count: iter, total: iter * 2 };
            grand = grand + s.total;
        }
        iter = iter + 1;
    }
    println(grand);
}
"#,
    );
    if let Some(out) = out {
        // iters 4..7: 8+10+12+14 = 44
        assert_eq!(out.trim(), "44");
    }
}

#[test]
fn test_ir_cluster_escaping_chain_keeps_rc_dec() {
    // Control: the chain escapes via a call — no cluster, the
    // root keeps the standard dec/drop path.
    let ir = ir_for_with_ownership(
        r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
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
fn run() -> i64 {
    let dummy = ListNode { val: 0, next: None };
    let mut tail = dummy;
    let node = ListNode { val: 1, next: None };
    tail.next = Some(node);
    sum(dummy.next)
}
fn main() { println(run()); }
"#,
    );
    let body = function_body(&ir, "run").expect("fn body");
    assert!(
        !body.contains("cw_loop"),
        "escaping chain must not free-walk; body:\n{body}"
    );
    assert!(
        body.contains("dummy_rc_cleanup"),
        "root keeps RcDec; body:\n{body}"
    );
}

#[test]
fn test_e2e_arc_binding_runtime_correctness() {
    // Atomic-RC inc/dec must produce the same observable behavior as
    // plain RC. The par block runs both branches; we verify the program
    // completes and produces the expected output, which exercises the
    // alloc + atomic inc + atomic dec drop-to-zero paths.
    //
    // ASAN (when enabled via tests/memory_sanitizer.rs) is what catches
    // a real refcount race — at the IR level this is an end-to-end
    // smoke check that the atomic codegen path links and runs.
    let out = run_program_with_ownership(
        r#"
shared struct Counter { val: i64 }
fn use_c(c: Counter) -> i64 { c.val }
fn main() {
    let cond: bool = false;
    let c = Counter { val: 7 };
    let d = c;
    if cond { use_c(d); }
    par {
        println(use_c(d));
        println(use_c(d));
    }
}
"#,
    );
    if let Some(out) = out {
        // Two branches, each prints 7. Order is unspecified across
        // threads, but both '7' tokens must appear.
        let count = out.matches('7').count();
        assert!(
            count >= 2,
            "expected '7' to be printed twice (once per par branch); got: {out:?}"
        );
    }
}

#[test]
fn test_e2e_shadow_heap_typed_reverts_no_leak() {
    // A String shadow: inner `s` prints "inner", outer restored to "outer".
    // Both buffers free exactly once (LSan-covered by the sibling
    // memory_sanitizer test); here we assert the VALUE reverts.
    let out = run_program(
        "fn main() {\n\
             let s = f\"outer\";\n\
             { let s = f\"inner\"; println(s); }\n\
             println(s);\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "inner\nouter");
    }
}

#[test]
fn test_release_strips_error_trace_instrumentation() {
    // The `?`-error-return-trace is debug-only instrumentation: a debug
    // build emits a `karac_error_trace_push` at the `?` failure site (and a
    // `karac_error_trace_clear` on the success path); a release build emits
    // neither, paying zero `?`-site cost (peer to contract stripping).
    let src = r#"
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
"#;
    // Assert on the `call` instruction, not the bare symbol — the runtime
    // fns are always *declared* in `Codegen::new` (the `declare void @…`
    // line survives either way); only the emitted *calls* are stripped.
    let debug = ir_for(src);
    assert!(
        debug.contains("call void @karac_error_trace_push"),
        "debug build must emit the `?`-trace push call"
    );
    let stripped = ir_for_error_trace_stripped(src);
    assert!(
        !stripped.contains("call void @karac_error_trace_push"),
        "release must strip the `?`-trace push call:\n{stripped}"
    );
    assert!(
        !stripped.contains("call void @karac_error_trace_clear"),
        "release must strip the `?`-trace clear call too"
    );
}

/// B-2026-08-14-26 — a field assignment through a CHAINED shared parent
/// (`outer.inner.field = v`, both `shared struct`) was silently dropped
/// under `karac build` while `--interp` applied it. `karac check` passed and
/// no surface reported anything; the program just kept the old value.
///
/// The walker's own `struct_types` lookup was the whole defect. A
/// `shared struct` is registered ONLY in `shared_types`, so
/// `nested_store_place_ptr`'s FieldAccess arm answered "not a struct" for
/// every shared parent and returned `None` — and `compile_field_store`'s
/// guard, which needs both a pointer and a type name, exited through the
/// no-op tail. `compile_field_store` had learned the `shared_types`-first
/// order in B-2026-07-28-6; the walk it depends on had not, and that arm
/// already carried the SAME row's fix for the shared FIELD one line below.
///
/// Line 01 is the scalar case, and it is the one that says what this is: no
/// heap, no container, no ownership question — a plain `i64` write that
/// vanished. Line 02 is the container case, whose displaced `Vec` also has
/// to be released now that the store lands (96 bytes, caught only by
/// `asan_chained_shared_field_assignment_releases_the_displaced_container`).
/// Lines 06 and 08 are the controls that must not move: the depth-1 shared
/// store B-2026-08-14-18 covers, and the plain-to-plain nested store that
/// has worked all along.
#[test]
fn test_e2e_chained_shared_field_assignment_lands() {
    let src = r#"
shared struct Inner { mut n: i64, mut v: Vec[String] }
shared struct Outer { mut inner: Inner, mut tag: i64 }
shared struct L3 { mut n: i64 }
shared struct L2 { mut c: L3 }
shared struct L1 { mut b: L2 }
struct PlainMid { mut inner: Inner }
struct PlainInner { mut n: i64 }
struct PlainNest { mut mid: PlainInner }

impl Outer {
    fn bump(ref self, k: i64) { self.inner.n = k; }
}

fn viaref(o: ref Outer, k: i64) { o.inner.n = k; }

fn main() {
    let o = Outer { inner: Inner { n: 5, v: Vec.new() }, tag: 0 };
    o.inner.n = 9;
    let b = o.inner;
    println(f"01 {b.n}");

    let a = o.inner;
    a.v.push("one");
    a.v.push("two");
    let fresh: Vec[String] = Vec.new();
    o.inner.v = fresh;
    let c = o.inner;
    println(f"02 {c.v.len()}");

    let x = L1 { b: L2 { c: L3 { n: 1 } } };
    x.b.c.n = 42;
    let m = x.b;
    let k = m.c;
    println(f"03 {k.n}");

    o.bump(77i64);
    let d = o.inner;
    println(f"04 {d.n}");

    viaref(o, 88i64);
    let e = o.inner;
    println(f"05 {e.n}");

    let g = Inner { n: 1, v: Vec.new() };
    g.n = 3;
    println(f"06 {g.n}");

    let mut p = PlainMid { inner: Inner { n: 5, v: Vec.new() } };
    p.inner.n = 11;
    let q = p.inner;
    println(f"07 {q.n}");

    let mut pn = PlainNest { mid: PlainInner { n: 2 } };
    pn.mid.n = 4;
    println(f"08 {pn.mid.n}");

    o.tag = 6;
    println(f"09 {o.tag} {o.inner.n}");
}
"#;
    assert_eq!(
        run_program(src).as_deref(),
        Some(
            "01 9\n\
                 02 0\n\
                 03 42\n\
                 04 77\n\
                 05 88\n\
                 06 3\n\
                 07 11\n\
                 08 4\n\
                 09 6 88\n"
        ),
    );
}

#[test]
fn e2e_a_shared_struct_comparison_calls_the_impl_on_both_backends() {
    // B-2026-08-26-25. A `shared struct` was absent from BOTH branches of
    // the ordering gate — they match `Type::Named` and a shared type is
    // `Type::Shared` — so `a < b` on one was admitted with NO derive and NO
    // impl, then failed on both backends ("operator 'Lt' is not defined for
    // operands of type 'SharedStruct'" / "Binary op Lt: left operand has
    // non-comparable type PointerType").
    //
    // EQUALITY was silently wrong rather than loud: `==` compared
    // structurally and never reached the `impl PartialEq`, while the
    // typechecker's own message promised that adding the `impl Eq` marker
    // would make it dispatch. Both comparators here DISAGREE with structural
    // comparison on purpose — `eq` always says false for two nodes whose
    // fields are equal, and `partial_cmp` reverses — so a structural or
    // declaration-order answer fails this test rather than passing it.
    let out = run_program(
        r#"
shared struct Node { id: i64 }
impl PartialEq for Node { fn eq(ref self, other: ref Node) -> bool { false } }
impl Eq for Node {}
impl PartialOrd for Node {
    fn partial_cmp(ref self, other: ref Node) -> Option[Ordering] { Some(other.id.cmp(self.id)) }
}
fn main() {
    let a = Node { id: 1 };
    let b = Node { id: 5 };
    let c = Node { id: 1 };
    println(f"{a < b} {a > b}");
    println(f"{a == c} {a != c}");
}
"#,
    );
    let out = out.expect("a shared struct with real comparators must build");
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(
        lines,
        vec![
            // reversed: 1 vs 5 compares Greater, so `<` false and `>` true
            "false true",
            // `eq` always false, so `==` false and `!=` true — structural
            // comparison of two `{id: 1}` nodes would print "true false"
            "false true",
        ]
    );
}

#[test]
fn test_e2e_lock_break_releases_then_reacquire() {
    // Release-on-all-paths: a `break` out of a lock body must still release
    // the lock (codegen seeds `CleanupAction::ReleaseMutex` on the body's
    // cleanup frame, drained by `emit_scope_cleanup_from` on the break
    // path). If the break leaked the lock, the post-loop re-acquire would
    // spin forever (deadlock → test hang). Reaching the println proves the
    // release fired; the value 3 proves the three pre-break increments ran.
    let out = run_program(
        r#"
fn main() {
    let m = Mutex.new(0);
    let mut i = 0;
    loop {
        lock m x {
            if i >= 3 { break; }
            x = x + 1;
        }
        i = i + 1;
    }
    lock m v { println(v); }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "3");
    }
}

#[test]
fn test_e2e_lock_return_releases_then_reacquire() {
    // Release-on-all-paths, `return` arm: an early `return` out of a lock
    // body releases the lock via `emit_scope_cleanup` (whole-stack drain
    // walks the release frame). The caller then re-acquires the same mutex
    // — a leaked lock would deadlock. 7 (returned) + 7 (re-read) = 14.
    let out = run_program(
        r#"
fn take(m: mut ref Mutex[i64]) -> i64 {
    lock m x { return x; }
    0
}
fn main() {
    let mut m = Mutex.new(7);
    let a = take(mut m);
    lock m v { println(a + v); }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "14");
    }
}

// ── Shared-type heap-identity regression (B-2026-06-20-6) ────────
//
// Two distinct `shared enum`s whose heap layouts are STRUCTURALLY
// IDENTICAL (same payload word-count) must still receive DISTINCT LLVM
// heap `StructType`s. Before the fix, shared heap types were anonymous
// (`context.struct_type`), which LLVM uniques by structure — so two
// equal-width shared enums collapsed to one `StructType` object. The
// refcount-drop dispatch recovers a shared type's name from its heap
// type by object identity (`emit_rc_dec`, `struct_name_for_heap_type`),
// so the collision made `Vec[Alfa]`'s per-element rc-dec call the WRONG
// `__karac_rc_drop_<T>` (random, HashMap-order-dependent), corrupting the
// heap. This surfaced as the slice-3b self-host type-oracle double-free
// (`Pattern` collided with `TypeExpr`, both 12 payload words). Naming the
// heap types (`%karac.shared.<T>`) makes them identity-distinct.
//
// `Alfa` and `Bravo` here are deliberately layout-twins: each variant set
// is `{ Leaf(String), Node(struct { Vec[Self], String }) }`, so both heap
// layouts are `{ rc, tag, <Vec=3>, <String=3> }` — identical word-for-word.
#[test]
fn shared_enums_with_identical_layout_get_distinct_heap_types() {
    let ir = ir_for(
        "shared enum Alfa { ALeaf(String), ANode(NodeA) }\n\
             shared enum Bravo { BLeaf(String), BNode(NodeB) }\n\
             struct NodeA { kids: Vec[Alfa], name: String }\n\
             struct NodeB { kids: Vec[Bravo], name: String }\n\
             fn mk_a() -> Alfa { Alfa.ALeaf(\"payload-string-long-enough\".to_string()) }\n\
             fn mk_b() -> Bravo { Bravo.BLeaf(\"payload-string-long-enough\".to_string()) }\n\
             fn use_a() {\n\
             \x20   let mut v: Vec[Alfa] = Vec.new();\n\
             \x20   v.push(mk_a());\n\
             \x20   v.push(mk_a());\n\
             }\n\
             fn use_b() {\n\
             \x20   let mut v: Vec[Bravo] = Vec.new();\n\
             \x20   v.push(mk_b());\n\
             \x20   v.push(mk_b());\n\
             }\n\
             fn main() { use_a(); use_b(); }\n",
    );

    // Each shared enum owns a uniquely-NAMED heap struct type — the fix's
    // mechanism. Anonymous (pre-fix) layouts would not appear by name and
    // the two enums would share one `StructType`.
    assert!(
        ir.contains("%karac.shared.Alfa = type"),
        "missing named heap type for Alfa — shared heap types must be \
             named, not anonymous, so layout-twins stay identity-distinct\n{ir}"
    );
    assert!(
        ir.contains("%karac.shared.Bravo = type"),
        "missing named heap type for Bravo\n{ir}"
    );

    // Behavioral guarantee: a per-element rc-dec helper for one enum must
    // dispatch to THAT enum's recursive drop, never the layout-twin's. A
    // mismatch here is the exact heap-corrupting confusion the bug caused.
    for (vec_elem, expected_drop) in [
        ("__karac_vec_elem_rc_dec_Alfa", "@__karac_rc_drop_Alfa("),
        ("__karac_vec_elem_rc_dec_Bravo", "@__karac_rc_drop_Bravo("),
    ] {
        if let Some(body) = function_body(&ir, vec_elem) {
            assert!(
                body.contains(expected_drop),
                "{vec_elem} must call {expected_drop} (its OWN drop), not the \
                     layout-twin's — heap-type collision regression\n--- body ---\n{body}"
            );
        }
    }
}

/// A heap field read through a DEEPER PLACE rooted at a `ref` binding is a
/// COPY — `fn peek(w: ref W) -> String { w.r.name }` (B-2026-08-28-25).
///
/// The abort this fixes is a memory error, gated under LSan by
/// `test_ref_rooted_deep_place_heap_read_is_cloned_not_aliased`. What THIS
/// test pins is the semantics the fix had to pick, which no sanitizer can
/// see, and which the two candidate fixes disagree about.
///
/// Cloning the read gives the caller its own buffer and leaves the borrowed
/// struct alone. Cap-zeroing the source — the move model — also silences
/// the abort, and it is wrong here in a way it is not even for an owned
/// source: a `ref` binding does not own the caller's storage, so zeroing it
/// strands the caller's buffer with no owner and leaves the caller reading
/// a field it no longer holds. The interpreter settles it by printing the
/// field twice — once from the callee, once from the caller reading it back
/// — so every row here reads the source again afterwards, and the mutation
/// rows ask the question directly.
#[test]
fn test_e2e_ref_rooted_deep_place_heap_read_is_a_copy() {
    let src = r#"
struct R { id: i64, name: String }
struct W { r: R, n: i64 }

fn peek(w: ref W) -> String { return w.r.name; }
fn peek_tail(w: ref W) -> String { w.r.name }
fn tuple_hop(p: ref (R, i64)) -> String { return p.0.name; }

fn main() {
    let v = W { r: R { id: 41, name: f"n{41}" }, n: 1 };
    let mut s = peek(v);
    s = s + "X";
    println(s);
    println(v.r.name);
    println(peek_tail(v));
    println(v.r.name);

    let p = (R { id: 7, name: f"m{7}" }, 1);
    let mut t = tuple_hop(p);
    t = t + "Y";
    println(t);
    println(p.0.name);
}
"#;
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(
        interp_errs.is_empty(),
        "interpreter errors: {interp_errs:?}"
    );
    let expected = interp_out.join("");
    // `n41X` then `n41` is the copy: a move model prints `n41X` then empty
    // or garbage for the borrowed struct's field.
    assert_eq!(
        expected, "n41X\nn41\nn41\nn41\nm7Y\nm7\n",
        "interpreter oracle is wrong for a ref-rooted deep place read",
    );
    let Some(aot) = run_program(src) else { return };
    assert_eq!(
        aot, expected,
        "a heap field read through a ref-rooted deeper place must be a copy",
    );
}

/// A SCALAR field read on a `shared struct` block / `if` receiver
/// (B-2026-08-28-7 leg 1).
///
/// The non-shared spelling of this program has worked since
/// B-2026-08-27-49; the shared one did not, because every shared field path
/// gates on knowing the receiver's type BEFORE `compile_expr` runs (it has
/// to decide it is emitting a GEP through an RC pointer rather than
/// extracting from an aggregate), and a block cannot answer then — its type
/// is its tail's, recorded only as the block compiles. The read therefore
/// reached the generic tail with a POINTER where the `StructValue` guard
/// wanted an aggregate, and died on the loud gap.
///
/// SCALAR fields only, which was a DELIBERATE restriction when this landed:
/// a `String`/`Vec` field on a shared TEMPORARY receiver was a separate,
/// pre-existing defect, so that shape was left failing loudly rather than
/// built on top of a broken release protocol. B-2026-08-28-14 fixed the
/// protocol and lifted the restriction — see
/// `test_e2e_heap_field_read_on_a_shared_temporary_receiver` for the heap
/// half. This test stays scalar-only on purpose: it is the gate for the
/// receiver-TYPING half, and keeping it free of the clone machinery means a
/// failure here still points at typing rather than at ownership.
#[test]
fn test_e2e_scalar_field_read_on_a_shared_block_receiver() {
    let src = r#"
shared struct Node { v: i64, w: i64 }
shared struct Wrap { inner: i64 }

fn make(k: i64) -> Node { return Node { v: k, w: k * 100 }; }
fn mkw() -> Wrap { return Wrap { inner: 9 }; }

fn main() {
    println({ let n = make(1); n }.v);
    println({ let n = make(2); n }.w);
    println({ let n = make(3); n }.v + 10);
    let k = { let n = make(4); n }.v;
    println(k);
    let c = true;
    println(if c { make(5) } else { make(6) }.v);
    println({ let w = mkw(); w }.inner);
    let b = make(8);
    println(b.v);
}
"#;
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(
        interp_errs.is_empty(),
        "interpreter errors: {interp_errs:?}"
    );
    let expected = interp_out.join("");
    // Anti-vacuity: line 2 reads the SECOND field, so a GEP that ignored the
    // field index would print 2 rather than 200, and the closing bound
    // receiver pins that the pre-existing identifier path still works.
    assert_eq!(
        expected, "1\n200\n13\n4\n5\n9\n8\n",
        "interpreter oracle is wrong for a shared block receiver",
    );
    let Some(aot) = run_program(src) else { return };
    assert_eq!(
        aot, expected,
        "a compiled scalar field read on a shared block receiver must match the interpreter",
    );
}

/// A `String` / `Vec` field read on a shared-struct TEMPORARY receiver
/// (B-2026-08-28-14).
///
/// Two defects met here, and each hid the other. The temporary's release
/// fell back to a SHALLOW `free(box)` — the recursive drop fn is
/// synthesized lazily by `track_rc_var`, i.e. by a BINDING, and a temporary
/// has none — so the receiver's heap payload was never freed. Making the
/// release deep then exposed the second: the loaded `{ptr,len,cap}` is an
/// ALIAS of a buffer the box owns, and the deep copy that makes the read
/// independent runs one step later, so the release had to move after it.
///
/// The behaviour depended on an unrelated statement elsewhere in the
/// program, which is what made it worth a test rather than a one-line fix:
/// with no `Node` binding anywhere the read leaked, and adding a `let w =
/// make(9);` on any line made the same read a use-after-free instead.
///
/// All three temporary shapes are covered because they share one release
/// helper: a CALL result, a value BLOCK, and an `if`. The ownership half is
/// gated separately by
/// `memory_sanitizer::test_shared_temp_receiver_heap_field_read_owns_its_copy`;
/// this test is the CORRECTNESS half, against the interpreter oracle.
#[test]
fn test_e2e_heap_field_read_on_a_shared_temporary_receiver() {
    let src = r#"
shared struct Node { v: i64, tag: String, xs: Vec[i64] }

fn make(k: i64) -> Node {
    return Node { v: k, tag: f"t{k}", xs: [k, k + 1] };
}

shared struct Outer { id: i64, inner: Node }

fn make_outer(k: i64) -> Outer {
    return Outer { id: k, inner: make(k) };
}

fn main() {
    println(make(1).tag);
    println({ let n = make(2); n }.tag);
    let c = true;
    println(if c { make(3) } else { make(4) }.tag);
    println(if not c { make(5) } else { make(6) }.tag);
    println(make(7).xs.len());
    println(make(8).v);
    let kept = make(9).tag;
    println(kept);
    let b = make(10);
    println(b.tag);
    println(make(11).tag.len());
    println(make_outer(12).inner.tag);
    println(make(13).tag + "!");
}
"#;
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(
        interp_errs.is_empty(),
        "interpreter errors: {interp_errs:?}"
    );
    let expected = interp_out.join("");
    // Anti-vacuity. Every line carries its own seed, so a receiver resolved
    // from the wrong temporary prints a different number rather than
    // coinciding; rows 3 and 4 take OPPOSITE `if` branches; row 5 reads the
    // `Vec` field and rows 6 and 9 the scalar and the bound receiver, so the
    // heap path cannot pass by claiming every field. Row 11 is the NESTED
    // bare-`shared` hop, which is what forced the inner-refcount
    // compensation — without it this row printed garbage on both compiled
    // backends while the interpreter was right.
    assert_eq!(
        expected, "t1\nt2\nt3\nt6\n2\n8\nt9\nt10\n3\nt12\nt13!\n",
        "interpreter oracle is wrong for a shared temporary receiver",
    );
    let Some(aot) = run_program(src) else { return };
    assert_eq!(
        aot, expected,
        "a compiled heap field read on a shared temporary receiver must match the interpreter",
    );
}

/// B-2026-08-27-5, the half that closes the INCONSISTENCY the previous fix
/// exposed. B-2026-08-27-4 made a shared map key match structurally while
/// `==` still compared a niche field's surrounding bytes, so one program
/// could answer `contains_key(b) == true` and `a == b` == false for the
/// same pair. There were two independent structural comparators for one
/// type — `karac_eq_<T>` for keys and `karac_sheq_<T>` for `==` — and only
/// one of them knew about niches.
///
/// They are one implementation now (the key comparator is a slot adapter
/// over the `==` walk), and this asserts the property that requires:
/// whatever the two are asked, they agree.
#[test]
fn test_e2e_shared_niche_key_and_eq_operator_agree() {
    assert_eq!(
        run_program(
            r#"
#[derive(Hash, Eq, PartialEq)]
shared struct Node { v: i64, next: Option[Node] }
fn main() {
    let leaf = Node { v: 2, next: None };
    let a = Node { v: 1, next: Some(leaf) };
    let leaf2 = Node { v: 2, next: None };
    let b = Node { v: 1, next: Some(leaf2) };
    let mut m: Map[Node, i64] = Map.new();
    m.insert(a, 7);
    println(f"contains={m.contains_key(b)}");
    println(f"operator={a == b}");
}
"#
        ),
        Some("contains=true\noperator=true\n".to_string())
    );
}

/// B-2026-08-27-4 — the compiled twin of
/// `a_shared_key_is_matched_structurally_not_by_pointer_identity`.
///
/// A `shared` map/set key was matched by POINTER IDENTITY here and
/// structurally by the interpreter: `contains_key` answered `false` for an
/// equal twin, `remove` removed nothing, `Set` failed to dedup. The struct
/// arms of `emit_eq_fn_for_type_expr` / `emit_hash_fn_for_type_expr`
/// exclude shared types, so a shared key fell to the byte-compare
/// fallback — and a shared key's blob IS a pointer.
///
/// EQ AND HASH HAD TO MOVE TOGETHER, which is what the `dedup` and
/// `removed` lines are really checking. Equal keys that hash differently
/// never land in the same bucket, so fixing equality alone leaves every
/// lookup missing exactly as before — and the fix would have looked
/// correct in a test that only compared two keys directly.
///
/// The `-ne` lines are not padding. A comparator that answered `true`
/// unconditionally passes every positive assertion here, and that is a
/// plausible way to get the null-guard branch wrong.
#[test]
fn test_e2e_shared_key_is_matched_structurally() {
    assert_eq!(
        run_program(
            r#"
#[derive(Hash, Eq, PartialEq)]
shared struct Scalar { id: i64, n: i64 }
#[derive(Hash, Eq, PartialEq)]
shared struct Named { id: i64, name: String }
#[derive(Hash, Eq, PartialEq)]
shared struct Inner { v: i64 }
#[derive(Hash, Eq, PartialEq)]
shared struct Outer { id: i64, inner: Inner }
#[derive(Hash, Eq, PartialEq)]
shared enum Tag { A { x: i64 }, B }
fn main() {
    let mut a: Map[Scalar, i64] = Map.new();
    a.insert(Scalar { id: 1, n: 2 }, 7);
    println(f"scalar={a.contains_key(Scalar { id: 1, n: 2 })}");

    let mut b: Map[Named, i64] = Map.new();
    b.insert(Named { id: 1, name: f"aa" }, 7);
    println(f"named={b.contains_key(Named { id: 1, name: f"aa" })}");
    println(f"named-ne={b.contains_key(Named { id: 1, name: f"ab" })}");

    let mut c: Map[Outer, i64] = Map.new();
    c.insert(Outer { id: 1, inner: Inner { v: 5 } }, 7);
    println(f"nested={c.contains_key(Outer { id: 1, inner: Inner { v: 5 } })}");
    println(f"nested-ne={c.contains_key(Outer { id: 1, inner: Inner { v: 6 } })}");

    let mut d: Map[Tag, i64] = Map.new();
    d.insert(Tag.A { x: 1 }, 7);
    println(f"enum={d.contains_key(Tag.A { x: 1 })}");
    println(f"enum-ne={d.contains_key(Tag.A { x: 2 })}");

    let mut s: Set[Scalar] = Set.new();
    s.insert(Scalar { id: 1, n: 2 });
    s.insert(Scalar { id: 1, n: 2 });
    println(f"dedup={s.len()}");

    let mut m: Map[Named, i64] = Map.new();
    m.insert(Named { id: 1, name: f"aa" }, 7);
    m.remove(Named { id: 1, name: f"aa" });
    println(f"removed={m.len()}");
}
"#
        ),
        Some(
            "scalar=true\nnamed=true\nnamed-ne=false\n\
                 nested=true\nnested-ne=false\n\
                 enum=true\nenum-ne=false\n\
                 dedup=1\nremoved=0\n"
                .to_string()
        )
    );
}

/// A heap element through the same path — the `Option` payload for a
/// `String` is multi-word, so an end-relative index must carry it as
/// intactly as the no-argument form does.
#[test]
fn test_e2e_last_end_relative_carries_a_heap_element() {
    let src = r#"
fn main() {
    let v: Vec[String] = ["alpha", "beta", "gamma"];
    match v.last(2) { Some(x) => { println(x); } None => { println("none"); } }
    match v.last(0) { Some(x) => { println(x); } None => { println("none"); } }
    match v.last(9) { Some(x) => { println(x); } None => { println("none"); } }
    println(v[0]);
}
"#;
    assert_eq!(
        run_program(src).as_deref(),
        Some("alpha\ngamma\nnone\nalpha\n")
    );
}

/// The `shared struct` field arm of the same transplant: the receiver's
/// slot holds a HANDLE, so the place is one load in at the header-shifted
/// offset. `mut_ref_place_arg_ptr` routes that through
/// `shared_mut_ref_place_arg_ptr`; reaching it from the method path is what
/// this pins.
#[test]
fn method_mut_ref_shared_field_arg_writes_through() {
    assert_eq!(
        run_program(
            "shared struct S { mut val: i64 }\n\
                 struct H { acc: i64 }\n\
                 impl H { fn bump(ref self, x: mut ref i64) -> i64 { x = x + 1; x } }\n\
                 fn free_bump(x: mut ref i64) -> i64 { x = x + 1; x }\n\
                 fn main() {\n\
                     let g1 = S { val: 7 };\n\
                     free_bump(mut g1.val);\n\
                     println(g1.val);\n\
                     let h = H { acc: 0 };\n\
                     let g2 = S { val: 7 };\n\
                     h.bump(mut g2.val);\n\
                     println(g2.val);\n\
                 }"
        ),
        Some("8\n8\n".to_string())
    );
}

#[test]
fn e2e_field_rooted_container_element_heap_read_is_cloned() {
    const DECL: &str = "struct R { id: i64, name: String }\n";
    let cases: &[(&str, &str, &str)] = &[
            // The row's own repro: a `Vec` field, `let`-bound.
            (
                "vec-field",
                "struct H { xs: Vec[R] }\n\
                 fn main() { let h = H { xs: [R { id: 41, name: f\"n{41}\" }] };\n\
                 let s = h.xs[0].name; println(f\"{s}\"); println(f\"{h.xs[0].name}\") }",
                "n41\nn41\n",
            ),
            // An `Array` field. Its elements are struct-owned and struct-dropped
            // (B-2026-08-27-32), which is what makes an un-cloned read off one a
            // double free rather than a leak — measured, aborts identically.
            (
                "array-field",
                "struct H { xs: Array[R, 1] }\n\
                 fn main() { let h = H { xs: [R { id: 41, name: f\"n{41}\" }] };\n\
                 let s = h.xs[0].name; println(f\"{s}\"); println(f\"{h.xs[0].name}\") }",
                "n41\nn41\n",
            ),
            // `self`-rooted, and escaping the frame as a return value.
            (
                "self-rooted-return",
                "struct H { xs: Vec[R] }\n\
                 impl H { fn peek(ref self) -> String { self.xs[0].name } }\n\
                 fn main() { let h = H { xs: [R { id: 41, name: f\"n{41}\" }] };\n\
                 println(f\"{h.peek()}\"); println(f\"{h.xs[0].name}\") }",
                "n41\nn41\n",
            ),
            // A `Vec.push` destination — one of the three consuming positions
            // that do NOT route through `clone_owned_vec_index_element`, which
            // is why the clone belongs at the READ rather than at the consumer.
            (
                "push-destination",
                "struct H { xs: Vec[R] }\n\
                 fn main() { let h = H { xs: [R { id: 41, name: f\"n{41}\" }] };\n\
                 let mut out: Vec[String] = Vec.new(); out.push(h.xs[0].name);\n\
                 println(f\"{out[0]}\"); println(f\"{h.xs[0].name}\") }",
                "n41\nn41\n",
            ),
            // The TUPLE-HOP sibling gate: `h.xs[0].0` over `Vec[(R, i64)]`.
            (
                "field-rooted-tuple-hop",
                "struct H { xs: Vec[(R, i64)] }\n\
                 fn main() { let h = H { xs: [(R { id: 41, name: f\"n{41}\" }, 1)] };\n\
                 let x = h.xs[0].0; println(f\"{x.id} {x.name}\"); println(f\"{h.xs[0].0.name}\") }",
                "41 n41\nn41\n",
            ),
            // ---- controls: already correct, and must STAY correct ----
            // The IDENTIFIER-rooted spelling — the one position the gate always
            // admitted, kept as a fixed point.
            (
                "control-identifier-root",
                "fn main() { let v: Vec[R] = [R { id: 41, name: f\"n{41}\" }];\n\
                 let s = v[0].name; println(f\"{s}\"); println(f\"{v[0].name}\") }",
                "n41\nn41\n",
            ),
            (
                "control-identifier-tuple-hop",
                "fn main() { let v: Vec[(R, i64)] = [(R { id: 41, name: f\"n{41}\" }, 1)];\n\
                 let x = v[0].0; println(f\"{x.id} {x.name}\"); println(f\"{v[0].0.name}\") }",
                "41 n41\nn41\n",
            ),
            // NON-CONSUMING: the read is used and discarded. Nothing takes the
            // clone over, so this is where an over-firing clone leaks instead of
            // aborting. Behaviourally it can only be checked for correctness
            // here; the ASAN twin proves it does not leak.
            (
                "control-non-consuming",
                "struct H { xs: Vec[R] }\n\
                 fn main() { let h = H { xs: [R { id: 41, name: f\"n{41}\" }] };\n\
                 println(f\"{h.xs[0].name.len()}\"); println(f\"{h.xs[0].name}\") }",
                "3\nn41\n",
            ),
            // A SCALAR field off the same element: no heap, so the gate must
            // not start cloning where there is nothing to clone.
            (
                "control-scalar-field",
                "struct H { xs: Vec[R] }\n\
                 fn main() { let h = H { xs: [R { id: 41, name: f\"n{41}\" }] };\n\
                 let n = h.xs[0].id; println(f\"{n}\"); println(f\"{h.xs[0].name}\") }",
                "41\nn41\n",
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
fn e2e_struct_with_shared_field_handed_back_has_one_owner() {
    let Some(out) = run_program(
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
    ) else {
        return;
    };
    assert_eq!(out, "t1\ndS1\nt2\nt3\nb4\ndS4\nb5\nt6\ndS6\nt7 7\ndS7\nh\ni\ndS11\nj\nk12 13\ndS13\nl1 1\nm0\nm1\nm2\nend\n", "got:\n{out}");
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
fn e2e_dropless_shared_field_struct_on_method_and_mixed_paths_has_one_owner() {
    let Some(out) = run_program(
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
    ) else {
        return;
    };
    assert_eq!(
        out, "a1 2\nb\nc5 8\nd9 99 99\ne13\nf1 0\ng1\nh\ni99\ni1\ni99\nend\n",
        "got:\n{out}"
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
///
/// THIS e2e PIN PASSES WITHOUT THE FIX, and says so rather than pretending
/// otherwise: `run_program` compiles without the concurrency analysis, so the
/// first fault's auto-par body path is never taken, and the second fault's use
/// after free and leaks do not reach stdout at -O2. It pins the output of the
/// fixed build against the interpreter; the ASAN twin, which runs the analysis,
/// is the fixture that fails without the fix (measured on fa4928288: output
/// `dS3` and `dS4` twice, and `heap-use-after-free` in
/// `__karac_vec_elem_full_drop_S3` under the instrumented leg).
#[test]
fn e2e_param_handed_on_to_mixed_path_callee_has_one_owner() {
    let Some(out) = run_program(
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
    ) else {
        return;
    };
    assert_eq!(out, "a1 98\ndS98\nb3\ndS3\ndS4\nb98\ndS98\nc5 98\ns3\ns3\ndS9\ns2\ndS10\ndS99\ns2\ndR11\nsR\ndR12\ndR99\nsR\ndR13\ne13\nn14\nnx\nn99\nnx\nnx\nn17\ndS17\nnx\nnx\ndS18\ng21\ng98\ng1\ng98\ndS24\nh22 25\ndS25\nend\n", "got:\n{out}");
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
///
/// The output was already right on main; this twin pins it against the
/// hand-over disarms turning a leak into a double free, and the ASAN twin is
/// the one main fails.
#[test]
fn e2e_option_or_generic_enum_payload_with_shared_field_is_released() {
    let Some(out) = run_program(
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
    ) else {
        return;
    };
    assert_eq!(out, "atrue\nbtrue\ndS2\nctrue\nd\ne5\nf66\ndS6\ng11\ndS8\ndS7\nhtrue\ndS9\ndS10\ndS11\ndS12\ni\njtrue\njtrue\njtrue\nktruetrue\ndS14\nl1\ndS13\nntrue\nntrue\ndS22\nnho\nn\nmtrue\ndS15\nend\n", "got:\n{out}");
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
fn e2e_generic_enum_handback_of_shared_field_drop_struct_runs_body_once() {
    let Some(out) = run_program(
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
    ) else {
        return;
    };
    assert_eq!(out, "dS1\na\ndS2\nb\ndS3\nc\ndS4\nd\ndS5\ne\nf\ngot6\ndS6\nf after\ng\ngot7\ndS7\ng after\nrd8\ndS8\nrd9\ndS9\nh after\ndS10\ni10\ndS11\ni11\ndS13\nj\nk14\ndS14\nx15\ndS15\nrd16\ndS16\nend\n", "got:\n{out}");
}

/// B-2026-09-26-15 / B-2026-09-26-11 — a `Drop` struct with a `shared`
/// field (so a generic callee FORWARDS it, and the caller keeps the memory)
/// handed back inside an enum by `fn mid[T](v: T, c: bool) -> Option[T]` or
/// `fn mkh[T](v: T) -> Ho[T]`. Moving the payload out of the result (`a_`..
/// `g_`, `n_`, `q_`) freed it twice on every compiled surface: the caller's
/// source and the new owner both released it. The result now takes the
/// source's memory on the path that hands it back (a per-path flag for
/// `mid`'s `None` edge). That made the result's box the payload's owner, and
/// a read-only arm, `if let` or `let … else` whose binding registers no drop
/// (a struct that declines copy support) then stripped the box's interior
/// free and leaked the `shared` field (`h_`..`m_`, `o_`, `p_`; `j_` and `k_`
/// are B-2026-09-26-11's local-scrutinee cells, which leaked the same way
/// before any hand-back). Such an arm now leaves the payload with the box.
#[test]
fn e2e_payload_moved_out_of_generic_enum_handback_freed_once() {
    let Some(out) = run_program(
        r#"shared struct Sh { k: i64 }
struct S2 { h: Sh, id: i64 }
impl Drop for S2 { fn drop(mut ref self) { println(f"dS{self.id}") } }
struct S3 { h: Sh, id: i64 }
enum Ho[T] { Full(T), Empty }
fn mk2(i: i64) -> S2 { return S2 { h: Sh { k: i }, id: i } }
fn mk3(i: i64) -> S3 { return S3 { h: Sh { k: i }, id: i } }
fn mkh[T](v: T) -> Ho[T] { return Ho.Full(v); }
fn mid[T](v: T, c: bool) -> Option[T] { if c { return Some(v) } return None }
fn rd(x: ref S2) { println(f"rd{x.id}") }
fn eat2(s: S2) { println(f"x{s.id}") }
fn a_unwrap() { let s = mk2(1); let o = mid(s, true); let v = o.unwrap(); println(f"a{v.id}") }
fn b_unwrap_nodrop() { let s = mk3(2); let o = mid(s, true); let v = o.unwrap(); println(f"b{v.id}") }
fn c_ho_move() {
    let s = mk2(3); let h = mkh(s);
    match h { Ho.Full(x) => { let y = x; println(f"c{y.id}") }, Ho.Empty => println("e") }
}
fn d_mid_move() {
    let s = mk2(4); let o = mid(s, true);
    match o { Some(x) => { let y = x; println(f"d{y.id}") }, None => println("n") }
}
fn e_push() { let s = mk2(5); let o = mid(s, true); let mut v: Vec[Option[S2]] = Vec.new(); v.push(o); println(f"e{v.len()}") }
fn f_loop() {
    for i in 6..10 { let s = mk2(i); let o = mid(s, i % 2 == 0); match o { Some(x) => { let y = x; println(f"f{y.id}") }, None => println(f"n{i}") } }
}
fn g_ho_loop() {
    for i in 10..12 { let s = mk2(i); let h = mkh(s); match h { Ho.Full(x) => { let y = x; println(f"g{y.id}") }, Ho.Empty => println("e") } }
}
fn h_ho_read() { let s = mk2(12); let h = mkh(s); match h { Ho.Full(x) => println(f"h{x.id}"), Ho.Empty => println("e") } }
fn i_ho_ref() { let s = mk2(13); let h = mkh(s); match h { Ho.Full(x) => rd(x), Ho.Empty => println("e") } }
fn j_local_read() { let o = Ho.Full(mk2(14)); match o { Ho.Full(x) => println(f"j{x.id}"), Ho.Empty => println("e") } }
fn k_local_ifl() { let o = Ho.Full(mk2(15)); if let Ho.Full(x) = o { println(f"k{x.id}") } }
fn l_ho_ifl() { let s = mk2(16); let o = mkh(s); if let Ho.Full(x) = o { println(f"l{x.id}") } }
fn m_ho_shared_field() { let s = mk2(17); let o = mkh(s); if let Ho.Full(x) = o { let k = x.h; println(f"m{k.k}") } }
fn n_ho_ifl_move() { let s = mk2(18); let o = mkh(s); if let Ho.Full(x) = o { let y = x; println(f"n{y.id}") } }
fn o_local_shared_field() { let o = Ho.Full(mk2(19)); match o { Ho.Full(x) => { let k = x.h; println(f"o{k.k}") }, Ho.Empty => println("e") } }
fn p_local_eat() { let o = Ho.Full(mk2(20)); match o { Ho.Full(x) => eat2(x), Ho.Empty => println("e") } }
fn q_letelse_move() { let s = mk2(21); let o = mkh(s); let Ho.Full(x) = o else { return }; let y = x; println(f"q{y.id}") }
fn r_local_nodrop() { let o = Ho.Full(mk3(22)); match o { Ho.Full(x) => println(f"r{x.id}"), Ho.Empty => println("e") } }
fn main() {
    a_unwrap(); println("a.")
    b_unwrap_nodrop(); println("b.")
    c_ho_move(); println("c.")
    d_mid_move(); println("d.")
    e_push(); println("e.")
    f_loop(); println("f.")
    g_ho_loop(); println("g.")
    h_ho_read(); println("h.")
    i_ho_ref(); println("i.")
    j_local_read(); println("j.")
    k_local_ifl(); println("k.")
    l_ho_ifl(); println("l.")
    m_ho_shared_field(); println("m.")
    n_ho_ifl_move(); println("n.")
    o_local_shared_field(); println("o.")
    p_local_eat(); println("p.")
    q_letelse_move(); println("q.")
    r_local_nodrop(); println("r.")
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "a1\ndS1\na.\nb2\nb.\nc3\ndS3\nc.\nd4\ndS4\nd.\ne1\ndS5\ne.\nf6\ndS6\ndS7\nn7\nf8\ndS8\ndS9\nn9\nf.\ng10\ndS10\ng11\ndS11\ng.\nh12\ndS12\nh.\nrd13\ndS13\ni.\nj14\ndS14\nj.\nk15\ndS15\nk.\nl16\ndS16\nl.\nm17\ndS17\nm.\nn18\ndS18\nn.\no19\ndS19\no.\nx20\ndS20\np.\nq21\ndS21\nq.\nr22\nr.\nend\n", "got:\n{out}");
}

/// B-2026-09-26-17 — the CONCRETE twin of B-2026-09-25-38 / B-2026-09-26-15:
/// a `Drop` struct with a `shared` field (so the callee FORWARDS it) handed
/// back inside an enum by a non-generic callee and bound by a `let`:
/// `fn wrapS(v: S2) -> Option[S2] { return Some(v) }`, `mkhS` / `mkHoS`, the
/// conditional `midS(v, c)`, and the method (`k_`..`m_`) and associated-fn
/// (`n_`) spellings. The call site stands the argument down (whole, or to the
/// callee per path for `midS`), while the let's payload owner declined the
/// memory as possibly caller-retained, so the 16-byte `Sh` was freed by
/// nobody on every compiled surface. The let now takes it when every
/// payload-typed argument is a local the call has already stood down. The
/// moving cells (`f_`, `g_`, `j_`, `m_`) pin that the result is then the
/// only owner.
#[test]
fn e2e_concrete_enum_handback_of_forwarded_struct_frees_its_field() {
    let Some(out) = run_program(
        r#"shared struct Sh { k: i64 }
struct S2 { h: Sh, id: i64 }
impl Drop for S2 { fn drop(mut ref self) { println(f"dS{self.id}") } }
struct S3 { h: Sh, id: i64 }
enum Ho[T] { Full(T), Empty }
enum HoS { FullS(S2), EmptyS }
fn mk2(i: i64) -> S2 { return S2 { h: Sh { k: i }, id: i } }
fn mk3(i: i64) -> S3 { return S3 { h: Sh { k: i }, id: i } }
fn wrapS(v: S2) -> Option[S2] { return Some(v) }
fn mkhS(v: S2) -> Ho[S2] { return Ho.Full(v) }
fn mkHoS(v: S2) -> HoS { return HoS.FullS(v) }
fn midS(v: S2, c: bool) -> Option[S2] { if c { return Some(v) } return None }
struct H { n: i64 }
impl H {
    fn wrapm(ref self, v: S2) -> Option[S2] { return Some(v) }
    fn midm(ref self, v: S2, c: bool) -> Option[S2] { if c { return Some(v) } return None }
    fn wrapa(v: S2) -> Option[S2] { return Some(v) }
}
fn a_wrap() { let s = mk2(1); let o = wrapS(s); println("a") }
fn b_mkh() { let s = mk2(2); let h = mkhS(s); println("b") }
fn c_mid_some() { let s = mk2(3); let o = midS(s, true); println("c") }
fn d_mid_none() { let s = mk2(4); let o = midS(s, false); println("d") }
fn f_wrap_unwrap() { let s = mk2(6); let o = wrapS(s); let v = o.unwrap(); println(f"f{v.id}") }
fn g_mid_move() { let s = mk2(7); let o = midS(s, true); match o { Some(x) => { let y = x; println(f"g{y.id}") }, None => println("n") } }
fn h_mkh_read() { let s = mk2(8); let h = mkhS(s); match h { Ho.Full(x) => println(f"h{x.id}"), Ho.Empty => println("e") } }
fn i_mono_read() { let s = mk2(9); let o = mkHoS(s); match o { HoS.FullS(x) => println(f"i{x.id}"), HoS.EmptyS => println("e") } }
fn j_mono_move() { let s = mk2(10); let o = mkHoS(s); match o { HoS.FullS(x) => { let y = x; println(f"j{y.id}") }, HoS.EmptyS => println("e") } }
fn k_meth() { let h = H { n: 0 }; let s = mk2(11); let o = h.wrapm(s); println("k") }
fn l_meth_mid_none() { let h = H { n: 0 }; let s = mk2(12); let o = h.midm(s, false); println("l") }
fn m_meth_mid_unwrap() { let h = H { n: 0 }; let s = mk2(13); let o = h.midm(s, true); let v = o.unwrap(); println(f"m{v.id}") }
fn n_assoc() { let s = mk2(14); let o = H.wrapa(s); println("n") }
fn o_loop() { for i in 15..19 { let s = mk2(i); let o = midS(s, i % 2 == 0); match o { Some(x) => println(f"o{x.id}"), None => println(f"on{i}") } } }
fn main() {
    a_wrap(); println("a.")
    b_mkh(); println("b.")
    c_mid_some(); println("c.")
    d_mid_none(); println("d.")
    f_wrap_unwrap(); println("f.")
    g_mid_move(); println("g.")
    h_mkh_read(); println("h.")
    i_mono_read(); println("i.")
    j_mono_move(); println("j.")
    k_meth(); println("k.")
    l_meth_mid_none(); println("l.")
    m_meth_mid_unwrap(); println("m.")
    n_assoc(); println("n.")
    o_loop(); println("o.")
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "dS1\na\na.\ndS2\nb\nb.\ndS3\nc\nc.\ndS4\nd\nd.\nf6\ndS6\nf.\ng7\ndS7\ng.\nh8\ndS8\nh.\ni9\ndS9\ni.\nj10\ndS10\nj.\ndS11\nk\nk.\ndS12\nl\nl.\nm13\ndS13\nm.\ndS14\nn\nn.\ndS15\non15\no16\ndS16\ndS17\non17\no18\ndS18\no.\nend\n", "got:\n{out}");
}

/// B-2026-09-26-17 — an `Option` of a `Drop` struct with a `shared` field,
/// handed to a fn that returns it (`fn keep(o: Option[S2]) -> Option[S2] {
/// return o }`). The result aliases the argument, which stays the owner, so
/// any move OUT of the result (`p.unwrap()`, `Some(x) => { let y = x }`, a
/// rebind, `keep(o).unwrap()`, `return keep(o)`, a field initializer) handed
/// the payload to a second owner while the argument's own drop still freed it:
/// a double free on every compiled surface. The result now records the
/// argument as its owner, and each move out of it disarms that owner. `j_` and
/// `r_` are the spellings B-2026-09-26-17's own fix reached (`let o =
/// wrapS(s)` now owns its payload), which were clean before only because `o`
/// owned nothing; the rest failed on origin/main.
#[test]
fn e2e_option_passed_through_a_fn_that_returns_it_is_freed_once() {
    let Some(out) = run_program(
        r#"shared struct Sh { k: i64 }
struct S2 { h: Sh, id: i64 }
impl Drop for S2 { fn drop(mut ref self) { println(f"dS{self.id}") } }
struct S3 { h: Sh, id: i64 }
struct W { p: Option[S2] }
fn mk2(i: i64) -> S2 { return S2 { h: Sh { k: i }, id: i } }
fn mk3(i: i64) -> S3 { return S3 { h: Sh { k: i }, id: i } }
fn wrapS(v: S2) -> Option[S2] { return Some(v) }
fn keep(o: Option[S2]) -> Option[S2] { return o }
fn keepg[T](o: Option[T]) -> Option[T] { return o }
fn keep3(o: Option[S3]) -> Option[S3] { return o }
fn maybe(o: Option[S2], c: bool) -> Option[S2] { if c { return o } return None }
fn rk(i: i64) -> Option[S2] { let o = Some(mk2(i)); return keep(o) }
fn a_unwrap() { let o = Some(mk2(1)); let p = keep(o); let v = p.unwrap(); println(f"a{v.id}") }
fn b_match_move() { let o = Some(mk2(2)); let p = keep(o); match p { Some(x) => { let y = x; println(f"b{y.id}") }, None => println("bn") } }
fn c_match_read() { let o = Some(mk2(3)); let p = keep(o); match p { Some(x) => println(f"c{x.id}"), None => println("cn") } }
fn d_iflet() { let o = Some(mk2(4)); let p = keep(o); if let Some(x) = p { let y = x; println(f"d{y.id}") } }
fn e_plain() { let o = Some(mk2(5)); let p = keep(o); println(f"e{p.is_some()}") }
fn f_chain() { let o = Some(mk2(6)); let p = keep(o); let q = keep(p); let v = q.unwrap(); println(f"f{v.id}") }
fn g_generic() { let o = Some(mk2(7)); let p = keepg(o); let v = p.unwrap(); println(f"g{v.id}") }
fn h_maybe() { let o = Some(mk2(8)); let p = maybe(o, true); let v = p.unwrap(); println(f"h{v.id}") }
fn i_maybe_none() { let o = Some(mk2(9)); let p = maybe(o, false); println(f"i{p.is_none()}") }
fn j_wrap_chain() { let s = mk2(10); let o = wrapS(s); let p = keep(o); let v = p.unwrap(); println(f"j{v.id}") }
fn k_s3() { let o = Some(mk3(11)); let p = keep3(o); let v = p.unwrap(); println(f"k{v.id}") }
fn l_rebind() { let o = Some(mk2(12)); let p = keep(o); let q = p; let v = q.unwrap(); println(f"l{v.id}") }
fn m_unwrap_or() { let o = Some(mk2(13)); let p = keep(o); let v = p.unwrap_or(mk2(93)); println(f"m{v.id}") }
fn n_call_unwrap() { let o = Some(mk2(14)); let v = keep(o).unwrap(); println(f"n{v.id}") }
fn o_ret() { let p = rk(15); let v = p.unwrap(); println(f"o{v.id}") }
fn p_ret_plain() { let p = rk(16); println(f"p{p.is_some()}") }
fn q_field() { let o = Some(mk2(17)); let w = W { p: keep(o) }; println(f"q{w.p.is_some()}") }
fn r_wrap_call_unwrap() { let s = mk2(18); let o = wrapS(s); let v = keep(o).unwrap(); println(f"r{v.id}") }
fn s_loop() {
    let mut i = 0;
    while i < 2 {
        let o = Some(mk2(19 + i)); let p = keep(o); let v = p.unwrap(); println(f"s{v.id}");
        i = i + 1;
    }
}
fn t_reassign() { let o = Some(mk2(21)); let p = keep(o); let mut q = p; q = Some(mk2(22)); println(f"t{q.is_some()}") }
fn main() {
    a_unwrap(); println("a.")
    b_match_move(); println("b.")
    c_match_read(); println("c.")
    d_iflet(); println("d.")
    e_plain(); println("e.")
    f_chain(); println("f.")
    g_generic(); println("g.")
    h_maybe(); println("h.")
    i_maybe_none(); println("i.")
    j_wrap_chain(); println("j.")
    k_s3(); println("k.")
    l_rebind(); println("l.")
    m_unwrap_or(); println("m.")
    n_call_unwrap(); println("n.")
    o_ret(); println("o.")
    p_ret_plain(); println("p.")
    q_field(); println("q.")
    r_wrap_call_unwrap(); println("r.")
    s_loop(); println("s.")
    t_reassign(); println("t.")
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "a1\ndS1\na.\nb2\ndS2\nb.\nc3\ndS3\nc.\nd4\ndS4\nd.\netrue\ndS5\ne.\nf6\ndS6\nf.\ng7\ndS7\ng.\nh8\ndS8\nh.\ndS9\nitrue\ni.\nj10\ndS10\nj.\nk11\nk.\nl12\ndS12\nl.\ndS93\nm13\ndS13\nm.\nn14\ndS14\nn.\no15\ndS15\no.\nptrue\ndS16\np.\nqtrue\ndS17\nq.\nr18\ndS18\nr.\ns19\ndS19\ns20\ndS20\ns.\ndS21\nttrue\ndS22\nt.\nend\n", "got:\n{out}");
}
