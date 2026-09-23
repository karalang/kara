//! ref and mut ref params, borrows, escape analysis, elision -- fixtures for `tests/codegen.rs`.
//!
//! Split out of `tests/codegen.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test codegen` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test codegen borrows::
//!
//! New fixtures about ref and mut ref params, borrows, escape analysis, elision belong in this file.

use super::*;

/// B-2026-08-05-1 and -2 — a TUPLE ELEMENT passed to a `ref` or `mut ref`
/// parameter must be borrowed IN PLACE, the way a struct field already is.
///
/// Both were one missing arm. B-2026-07-12-1 taught the ref-argument path to
/// pass a pointer to a struct FIELD rather than let it fall through to the
/// rvalue path, which shallow-copies the `{ptr,len,cap}` header into a temp
/// and queues a scope-exit free of it — a buffer the owner still holds. The
/// tuple-element spelling never got the sibling arm, so it fell through and
/// produced the same `free(): double free detected` that fix was written
/// for, with the interpreter correct (arm a).
///
/// The `mut ref` half is the same gap wearing a different symptom, which is
/// why one arm closes both: the callee got a pointer to that temp copy, so
/// `v.push(42)` grew the temp and the write never reached the tuple —
/// `t2.0.len()` stayed 2 and the following `t2.0[2]` panicked (arm b). A
/// lost mutation is the quieter half; on a shape without a bounds-checked
/// read after it, it is a silent wrong answer.
///
/// Arms (c) and (d) widen it off the first-position `Vec` special case — a
/// second-position element and a `String` element. Arms (e) and (f) are the
/// struct-field controls that localized the bug and must stay correct: they
/// are the reference implementation this arm was made to match.
///
/// Found by a systematic tuple-vs-field parity sweep rather than in the
/// wild. Seeded from `env.args().len()` with element and byte reads so the
/// buffers survive `-O2` (B-2026-08-04-17).
#[test]
fn e2e_tuple_element_borrowed_in_place_for_ref_params() {
    let Some(out) = run_program(
            "struct H { a: Vec[i64], b: i64 }\n\
             fn mkv(k: i64) -> Vec[i64] { let mut v: Vec[i64] = Vec.new(); v.push(k); v.push(k + 1i64); return v; }\n\
             fn mks(k: i64) -> String { let mut s: String = String.new(); s.push_str(f\"pay-{k}\"); return s; }\n\
             fn dig(i: i64) -> String { let mut d: String = String.new(); d.push_str(f\"{i}\"); return d; }\n\
             fn peek(v: ref Vec[i64]) -> i64 { return v[0i64] + v.len(); }\n\
             fn bump(v: mut ref Vec[i64]) { v.push(42i64); }\n\
             fn slen(s: ref String) -> i64 { return s.len(); }\n\
             fn main() {\n\
             \x20   let n: i64 = env.args().len();\n\
             \x20   // (a) ref param, TUPLE element — was a double free\n\
             \x20   let t1: (Vec[i64], i64) = (mkv(n), 5i64);\n\
             \x20   println(f\"a:{peek(t1.0)}:{t1.0[1i64]}\");\n\
             \x20   // (b) mut ref param, TUPLE element — the mutation was lost, then panicked\n\
             \x20   let mut t2: (Vec[i64], i64) = (mkv(n), 5i64);\n\
             \x20   bump(mut t2.0);\n\
             \x20   println(f\"b:{t2.0.len()}:{t2.0[2i64]}\");\n\
             \x20   // (c) ref param, tuple element in SECOND position\n\
             \x20   let t3: (i64, Vec[i64]) = (5i64, mkv(n));\n\
             \x20   println(f\"c:{peek(t3.1)}\");\n\
             \x20   // (d) ref param, STRING tuple element\n\
             \x20   let t4: (String, i64) = (mks(n), 5i64);\n\
             \x20   if t4.0.contains(dig(n)) { println(f\"d:{slen(t4.0)}\"); } else { println(\"d:BAD\"); }\n\
             \x20   // CONTROL: the struct-FIELD spelling, correct since B-2026-07-12-1\n\
             \x20   let h1: H = H { a: mkv(n), b: 5i64 };\n\
             \x20   println(f\"e:{peek(h1.a)}:{h1.a[1i64]}\");\n\
             \x20   let mut h2: H = H { a: mkv(n), b: 5i64 };\n\
             \x20   bump(mut h2.a);\n\
             \x20   println(f\"f:{h2.a.len()}:{h2.a[2i64]}\");\n\
             \x20   println(\"end\");\n\
             }\n",
        ) else {
            return;
        };
    assert_eq!(out, "a:3:2\nb:3:42\nc:3\nd:5\ne:3:2\nf:3:42\nend\n");
}

/// B-2026-08-05-37 — a `mut ref` parameter given a PLACE argument must
/// write back into that place.
///
/// The callee's write used to land on a shallow COPY of the place and be
/// silently discarded: every arm below printed the PRE-call value, on all
/// three backends, with no diagnostic at any phase. Only a bare identifier
/// argument worked.
///
/// This is the general form of two earlier one-shape fixes. B-2026-07-12-1
/// (struct field) and B-2026-08-05-2 (tuple element) both taught this path
/// to borrow in place, but gated it to a `{ptr,len,cap}` element — correct
/// scoping for what those rows measured, since only a heap element can
/// DOUBLE FREE. The lost-write half is not type-specific, and B-2026-08-05-2's
/// own text predicted exactly this: "a lost `mut ref` write on a shape with
/// no bounds-checked read after it is a SILENT wrong answer." So the gate
/// is now the PARAMETER MODE rather than the payload type.
///
/// Every arm is a distinct place shape or payload class, because the bug
/// was invisible to the type-gated fixes precisely by being uniform across
/// them: field, nested field, tuple element, `Vec` element, a field of a
/// `Vec` element, a whole struct payload, `f64`, `bool`, the FORWARDING
/// spelling through a `mut ref` parameter (the one the typechecker directs
/// authors to — "this argument is already a mut-ref; drop the `mut`
/// marker"), and a generic callee (its own mono call path, which needed
/// the same arm). The last two arms are the heap controls that must stay
/// correct: they are what the earlier fixes bought.
///
/// Seeded from `env.args().len()` so no arm folds away at `-O2`
/// (B-2026-08-04-17).
#[test]
fn e2e_mut_ref_place_argument_writes_back() {
    let Some(out) = run_program(
        "struct Q { v: i64 }\n\
             struct P { q: Q }\n\
             struct F { v: f64 }\n\
             struct B { v: bool }\n\
             struct S { v: i64 }\n\
             struct V { a: Vec[i64] }\n\
             fn bump(x: mut ref i64) { x = x + 1i64; }\n\
             fn bumpf(x: mut ref f64) { x = x + 1.0; }\n\
             fn setb(x: mut ref bool) { x = true; }\n\
             fn setq(x: mut ref Q) { x.v = 9i64; }\n\
             fn setg[T](x: mut ref T, d: T) { x = d; }\n\
             fn fwd(p: mut ref S) { bump(p.v); }\n\
             fn push2(v: mut ref Vec[i64]) { v.push(42i64); }\n\
             fn main() {\n\
             \x20   let n: i64 = env.args().len();\n\
             \x20   // (a) struct field\n\
             \x20   let mut a: S = S { v: n + 6i64 };\n\
             \x20   bump(mut a.v);\n\
             \x20   println(f\"a:{a.v}\");\n\
             \x20   // (b) NESTED struct field\n\
             \x20   let mut b: P = P { q: Q { v: n + 6i64 } };\n\
             \x20   bump(mut b.q.v);\n\
             \x20   println(f\"b:{b.q.v}\");\n\
             \x20   // (c) tuple element\n\
             \x20   let mut c: (i64, i64) = (n + 6i64, 0i64);\n\
             \x20   bump(mut c.0);\n\
             \x20   println(f\"c:{c.0}\");\n\
             \x20   // (d) Vec element\n\
             \x20   let mut d: Vec[i64] = Vec.new();\n\
             \x20   d.push(n + 6i64);\n\
             \x20   bump(mut d[0i64]);\n\
             \x20   println(f\"d:{d[0i64]}\");\n\
             \x20   // (e) field of a Vec element\n\
             \x20   let mut e: Vec[S] = Vec.new();\n\
             \x20   e.push(S { v: n + 6i64 });\n\
             \x20   bump(mut e[0i64].v);\n\
             \x20   println(f\"e:{e[0i64].v}\");\n\
             \x20   // (f) a whole STRUCT payload, not a scalar\n\
             \x20   let mut f: P = P { q: Q { v: n + 6i64 } };\n\
             \x20   setq(mut f.q);\n\
             \x20   println(f\"f:{f.q.v}\");\n\
             \x20   // (g) f64 payload\n\
             \x20   let mut g: F = F { v: 7.0 };\n\
             \x20   bumpf(mut g.v);\n\
             \x20   println(f\"g:{g.v as i64}\");\n\
             \x20   // (h) bool payload\n\
             \x20   let mut h: B = B { v: false };\n\
             \x20   setb(mut h.v);\n\
             \x20   println(f\"h:{h.v}\");\n\
             \x20   // (i) FORWARDING through a `mut ref` param — no marker, per\n\
             \x20   //     design.md; this is the spelling the typechecker names\n\
             \x20   let mut i2: S = S { v: n + 6i64 };\n\
             \x20   fwd(mut i2);\n\
             \x20   println(f\"i:{i2.v}\");\n\
             \x20   // (j) GENERIC callee — its own mono call path\n\
             \x20   let mut j: S = S { v: n + 6i64 };\n\
             \x20   setg(mut j.v, n + 7i64);\n\
             \x20   println(f\"j:{j.v}\");\n\
             \x20   // CONTROLS: the heap shapes the earlier fixes bought\n\
             \x20   let mut k: V = V { a: Vec.new() };\n\
             \x20   k.a.push(n);\n\
             \x20   push2(mut k.a);\n\
             \x20   println(f\"k:{k.a.len()}:{k.a[1i64]}\");\n\
             \x20   let mut l: (Vec[i64], i64) = (Vec.new(), 0i64);\n\
             \x20   l.0.push(n);\n\
             \x20   push2(mut l.0);\n\
             \x20   println(f\"l:{l.0.len()}:{l.0[1i64]}\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(
        out,
        "a:8\nb:8\nc:8\nd:8\ne:8\nf:9\ng:8\nh:true\ni:8\nj:8\nk:2:42\nl:2:42\n"
    );
}

/// B-2026-08-25-5 — a `mut ref self` SIBLING call inside a generic impl
/// silently discarded the callee's mutations.
///
/// `self` parses as `ExprKind::SelfValue`, not `Identifier("self")`. The
/// generic call path's ref-argument lowering produced a pointer from three
/// arms — an `Identifier` via `get_data_ptr`, an index borrow, and
/// `mut_ref_place_arg_ptr` (FieldAccess / TupleIndex only, on the stated
/// assumption that "a bare identifier already took the `get_data_ptr` fast
/// path") — and `SelfValue` matched NONE of them. It fell through to
/// `materialize_rvalue_for_ref_arg`, so the callee got a pointer to a COPY
/// of the receiver and wrote into that.
///
/// Filed as a HANG, and the loop is how it was noticed — `while
/// self.xs.len() > 0` never goes false when the pop lands on a copy. But
/// the loop is NOT required and the row's narrowing on that point was
/// wrong: a SINGLE call already loses the mutation. Case (a) is that
/// minimal shape, and it is the one to keep if this test is ever trimmed —
/// the earlier narrowing reported one call as "correct" because it checked
/// only the returned value, which is right either way. The receiver's state
/// afterward is what has to be asserted.
///
/// Controls: (c) is the same shape on a NON-generic impl, which always
/// worked (the concrete path resolves `SelfValue` elsewhere); (d) reads
/// through `ref self` rather than mutating. If (c)/(d) ever fail the fix
/// has over-reached into the concrete path. Verified RED against the
/// pre-fix compiler: (a) printed 2 and (b) hung until killed.
#[test]
fn e2e_mut_ref_self_sibling_call_in_generic_impl_mutates_the_receiver() {
    let src = r#"
struct Box[T] { xs: Vec[T] }
impl[T] Box[T] {
    fn one(mut ref self) -> Option[T] { self.xs.pop() }
    fn probe(mut ref self) -> i64 {
        match self.one() { Some(v) => { println(v); } None => {} }
        self.xs.len()
    }
    fn drain(mut ref self) -> Vec[T] {
        let mut out: Vec[T] = Vec.new();
        while self.xs.len() > 0 {
            match self.one() { Some(v) => { out.push(v); } None => {} }
        }
        out
    }
    fn peek(ref self) -> i64 { self.xs.len() }
}
struct Plain { xs: Vec[i64] }
impl Plain {
    fn one(mut ref self) -> Option[i64] { self.xs.pop() }
    fn probe(mut ref self) -> i64 {
        match self.one() { Some(v) => { println(v); } None => {} }
        self.xs.len()
    }
}
fn main() {
    // (a) ONE sibling call: the receiver must be one shorter afterward.
    let mut a = Box { xs: [7, 9] };
    println(a.probe());
    // (b) the filed shape — the drain loop must terminate and yield both.
    let mut b = Box { xs: [7, 9] };
    let o = b.drain();
    println(o.len());
    // (b2) same at a heap-carrying element type.
    let mut s = Box { xs: ["x", "yy"] };
    println(s.drain().len());
    // (c) NON-generic control: always worked, must stay working.
    let mut p = Plain { xs: [7, 9] };
    println(p.probe());
    // (d) `ref self` control: a read-only sibling receiver is unaffected.
    let r = Box { xs: [7, 9, 11] };
    println(r.peek());
}
"#;
    assert_eq!(run_program(src).as_deref(), Some("9\n1\n2\n2\n9\n1\n3\n"));
}

/// B-2026-08-05-39 — a `mut ref` AGGREGATE parameter's whole-value
/// REASSIGNMENT must write through the borrow.
///
/// `x = mk()` on a `mut ref String` used to store the 24-byte
/// `{ptr,len,cap}` into the 8-byte alloca that holds the borrow POINTER:
/// the caller's value never changed (AOT printed the pre-call length while
/// the interpreter printed the right one — a run/build divergence) and the
/// store ran past the slot into adjacent stack storage. `mut ref Vec` and a
/// `mut ref` STRUCT had the same shape. Only scalars were routed through
/// the borrow.
///
/// IN-PLACE mutation was always correct — `x.push_str("c")`, `x.f = v` —
/// which is why the gap survived: the note at the fix site said an
/// aggregate `mut ref` "mutates through methods / field stores that already
/// deref", true of every shape anyone tested and false of reassignment.
///
/// Five RHS classes, because they reclaim differently: an f-string (which
/// stages into an accumulator slot that must be disarmed — leaving it armed
/// aborts with `free(): double free detected`, measured), a
/// self-referential concat, a moved local binding, a call result, and a
/// struct literal with heap fields. Plus the displaced OLD value in every
/// one: overwriting the caller's storage orphans what it held, so the
/// pointee is reclaimed before the store. That half is invisible here —
/// disabling the reclamation still prints the right answer while leaking
/// 200 blocks per shape — so it is gated by the ASAN twin
/// `asan_mut_ref_aggregate_param_reassignment_no_leak`.
///
/// The expected total is DERIVED, not read off a run: per iteration
/// `1 + 1 + 1 + (i + 3) + 1 + (i + 1) = 2i + 8`, and
/// `sum(2i for i in 0..=199) + 8 * 200 = 39800 + 1600 = 41400`.
#[test]
fn e2e_mut_ref_aggregate_param_reassignment_writes_through() {
    let Some(out) = run_program(
        r#"struct Q { s: String, v: Vec[i64] }

fn mkv(k: i64) -> Vec[i64] {
    let mut v: Vec[i64] = Vec.new();
    v.push(k);
    v.push(k + 1i64);
    return v;
}

fn reps(x: mut ref String, k: i64) { x = f"fresh-{k}-payload"; }
fn repsc(x: mut ref String) { x = x + "tail"; }
fn repbind(x: mut ref String, k: i64) { let t: String = f"bind-{k}-payload"; x = t; }
fn repv(v: mut ref Vec[i64], k: i64) { v = mkv(k); }
fn repq(q: mut ref Q, k: i64) { q = Q { s: f"q-{k}-payload", v: mkv(k) }; }

fn main() {
    let n: i64 = env.args().len();
    let mut acc: i64 = 0;
    let mut i: i64 = 0;
    while i < n + 199i64 {
        let mut s: String = f"seed-{i}-payload";
        reps(mut s, i);
        if s.contains("fresh") { acc = acc + 1i64; }
        repsc(mut s);
        if s.contains("tail") { acc = acc + 1i64; }
        repbind(mut s, i);
        if s.contains("bind") { acc = acc + 1i64; }
        let mut v: Vec[i64] = mkv(i);
        repv(mut v, i + 1i64);
        acc = acc + v[0i64] + v.len();
        let mut q: Q = Q { s: f"orig-{i}-payload", v: mkv(i) };
        repq(mut q, i);
        if q.s.contains("payload") { acc = acc + 1i64; }
        acc = acc + q.v[1i64];
        i = i + 1i64;
    }
    println(acc);
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "41400\n");
}

/// B-2026-08-27-52 — the MONO half of B-2026-07-12-1. That row gave the
/// non-generic call path an in-place borrow for a struct-FIELD argument to
/// a `ref` param; the generic path computes a place pointer only when the
/// param is `mut ref`, so a read-only `ref` container param fell through to
/// `materialize_rvalue_for_ref_arg`, which shallow-copies the field's
/// `{ptr,len,cap}` header into a temp and queues a scope-exit FREE of it.
/// The receiver still owns that buffer, so its own field drop doubled the
/// free: `free(): double free detected in tcache 2`, abort, on both
/// compiled backends while the interpreter was correct.
///
/// The `mut ref` spelling of the identical call was already clean (it took
/// the arm above), and so was the non-generic callee — which is what
/// isolates this to the generic path. Guarded under ASAN too; these cases
/// pin the run/build agreement.
#[test]
fn e2e_ref_container_param_borrows_a_struct_field_in_place() {
    // Generic callee, generic impl receiver, String element.
    let generic_impl = "struct Bag[=T] { xs: Vec[T] }\n\
             fn vlen[T](v: ref Vec[T]) -> i64 { return v.len(); }\n\
             impl[T] Bag[T] {\n\
                 fn go(ref self) -> i64 { return vlen(self.xs); }\n\
             }\n\
             fn main() {\n\
                 let mut a: Bag[String] = Bag { xs: Vec.new() };\n\
                 a.xs.push(\"aa\"); println(f\"{a.go()}\");\n\
             }\n";
    assert_eq!(run_program(generic_impl).as_deref(), Some("1\n"));
    // Generic callee, PLAIN struct, called from `main` — no impl and no
    // generic receiver, so the generic CALLEE is what routes it here.
    let generic_callee_plain = "struct Bag { xs: Vec[String] }\n\
             fn vlen[T](v: ref Vec[T]) -> i64 { return v.len(); }\n\
             fn main() {\n\
                 let mut a: Bag = Bag { xs: Vec.new() };\n\
                 a.xs.push(\"aa\"); println(f\"{vlen(a.xs)}\");\n\
             }\n";
    assert_eq!(run_program(generic_callee_plain).as_deref(), Some("1\n"));
    // The NON-generic callee control: clean before the fix (B-2026-07-12-1
    // covers it), so it pins the gap to the mono path.
    let nongeneric_control = "struct Bag { xs: Vec[String] }\n\
             fn vlen(v: ref Vec[String]) -> i64 { return v.len(); }\n\
             fn main() {\n\
                 let mut a: Bag = Bag { xs: Vec.new() };\n\
                 a.xs.push(\"aa\"); println(f\"{vlen(a.xs)}\");\n\
             }\n";
    assert_eq!(run_program(nongeneric_control).as_deref(), Some("1\n"));
}

/// B-2026-09-10-32 — an indexed-receiver method call on a BORROWED fixed
/// array lowers.
///
/// `fn f(a: ref Array[String, 2]) -> i64 { a[0].len() }` failed codegen
/// with "outer is not a Vec/Slice/Array", which was wrong about the cause:
/// the outer IS an Array. A `ref`/`mut ref` array param's slot holds the
/// BORROW (a `ptr` to the caller's `[N x T]`), so the dispatch chain's last
/// arm — which inspects `slot.ty` for an `ArrayType` — fell through.
///
/// THE FOUR CONTROLS ARE WHY THIS IS BORROW-SPECIFIC RATHER THAN
/// ARRAY-SPECIFIC. By-value `Array` works because its slot really is an
/// `ArrayType`; `ref Vec` works because a Vec outer is claimed several arms
/// earlier by `vec_elem_types`, before any slot-type inspection; and
/// field-then-method works because the field projection never reaches the
/// indexed-receiver path at all. `mut ref` and a SCALAR element type are
/// the two cells the row did not have — both failed identically before,
/// which is what shows the gap was neither about mutability nor about heap
/// elements.
#[test]
fn e2e_indexed_receiver_method_on_a_borrowed_array() {
    for (label, src, want) in [
        (
            "ref-array",
            "fn f(a: ref Array[String, 2]) -> i64 { return a[0].len(); }\n\
                 fn main() { let a: Array[String, 2] = [\"abcdefghij\", \"abcdefghij\"]; \
                 println(f\"{f(a)}\"); }\n",
            "10\n",
        ),
        (
            "mut-ref-array",
            "fn f(a: mut ref Array[String, 2]) -> i64 { return a[1].len(); }\n\
                 fn main() { let mut a: Array[String, 2] = [\"abcdefghij\", \"abcdefghij\"]; \
                 println(f\"{f(mut a)}\"); }\n",
            "10\n",
        ),
        (
            "ref-array-scalar-elem",
            "fn f(a: ref Array[i64, 2]) -> i64 { return a[0].abs(); }\n\
                 fn main() { let a: Array[i64, 2] = [0 - 7, 3]; println(f\"{f(a)}\"); }\n",
            "7\n",
        ),
        (
            "control-by-value",
            "fn f(a: Array[String, 2]) -> i64 { return a[0].len(); }\n\
                 fn main() { let a: Array[String, 2] = [\"abcdefghij\", \"abcdefghij\"]; \
                 println(f\"{f(a)}\"); }\n",
            "10\n",
        ),
        (
            "control-ref-vec",
            "fn f(a: ref Vec[String]) -> i64 { return a[0].len(); }\n\
                 fn main() { let a: Vec[String] = [\"abcdefghij\", \"abcdefghij\"]; \
                 println(f\"{f(a)}\"); }\n",
            "10\n",
        ),
        (
            "control-field-then-method",
            "struct W { name: String }\n\
                 fn f(a: ref Array[W, 2]) -> i64 { return a[0].name.len(); }\n\
                 fn main() { let a: Array[W, 2] = [W { name: \"abcdefghij\" }, \
                 W { name: \"abcdefghij\" }]; println(f\"{f(a)}\"); }\n",
            "10\n",
        ),
    ] {
        assert_eq!(run_program(src).as_deref(), Some(want), "{label}");
    }
}

#[test]
fn test_e2e_ref_binding_is_a_live_alias() {
    let Some(out) = run_program(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(4);
    v.push(5);
    let r = ref v[1];
    println(f"{r}");
    v.swap(0, 1);
    println(f"{r}");
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "5\n4\n", "ref must observe the swap; got: {out:?}");
}

/// B-2026-08-27-21 leg 1 — a METHOD on an element read through a `ref`
/// binding. `let row = ref d[0]; row[0].clone()` SIGSEGV'd under codegen
/// while `--interp` returned "aa".
///
/// The sibling test above pins the plain read `r[j]`, which the same fix
/// for B-2026-08-26-36 made work by registering `vec_elem_types[r]` with
/// the INNER element type. `var_elem_type_exprs[r]` — the TypeExpr twin of
/// that table, and the one `compile_indexed_receiver_method` reads to
/// register a synth receiver — kept the binding's OWN type instead, so the
/// two disagreed. The synth for a `String` element was registered as a
/// `Vec[String]` over a slot pointing at the String's `{ptr, len, cap}`:
/// `clone` read "aa"'s header as a Vec header, took `len = 2` as an element
/// COUNT, and cloned two `String`s out of the letter bytes.
///
/// `.len()` / `.starts_with()` / `.substring()` / `.to_string()` on the
/// same receiver all returned correctly throughout, which is what made this
/// look method-specific rather than table-specific: none of them reads the
/// element TypeExpr, and Vec's `len` happens to load the same word String's
/// does. They are asserted here alongside `clone` so a future regression
/// that breaks them is not mistaken for this one.
#[test]
fn test_e2e_method_on_an_element_through_a_ref_binding() {
    let Some(out) = run_program(
        r#"
fn main() {
    let mut d: Vec[Vec[String]] = Vec.new();
    let mut a: Vec[String] = Vec.new();
    a.push("aa");
    a.push("bb");
    d.push(a);
    let row = ref d[0];
    let c: String = row[0].clone();
    println(c);
    println(row[0].len());
    println(row[0].starts_with("a"));
    println(row[1].substring(0, 1));
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out, "aa\n2\ntrue\nb\n",
        "clone through a borrow; got: {out:?}"
    );
}

/// B-2026-08-26-36 — a `ref` binding over a STRUCT FIELD container
/// (`b.xs[i]`), which is the shape `std.autograd`'s tape reads take and
/// therefore the one the `E_INDEX_MOVE_NON_COPY` fix-it will lean on.
/// Borrows a non-`Copy` element, so it also pins that the borrow neither
/// moves out of the container nor runs the element's cleanup.
#[test]
fn test_e2e_ref_binding_over_a_struct_field_container() {
    let Some(out) = run_program(
        r#"
struct Item { id: i64, tag: String }
struct Bag { xs: Vec[Item] }
fn main() {
    let mut b = Bag { xs: Vec.new() };
    b.xs.push(Item { id: 7, tag: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string() });
    b.xs.push(Item { id: 9, tag: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string() });
    let r = ref b.xs[0];
    println(f"read {r.id}");
    b.xs.swap(0, 1);
    println(f"after swap {r.id}");
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "read 7\nafter swap 9\n", "got: {out:?}");
}

/// B-2026-09-01-15 — every ROOT a nested `ref c[i][j]` chain can start
/// from, not just a `Vec` local.
///
/// B-2026-09-01-6 lowered the Vec-rooted chain and deliberately declined
/// the rest rather than guess a slot layout; this closes the four it left,
/// plus `Array[Array]`, which nobody had filed.
///
/// The classes really do differ, which is why they were declined rather
/// than waved through: a `Vec` slot holds a `{ptr, i64, i64}` HEADER while
/// an `Array` slot IS the storage, and reading one as the other is exactly
/// the B-2026-08-31-29 segfault (element 0 taken as the data pointer). So
/// the resolver dispatches on the recorded `TypeExpr` at every hop instead
/// of inferring from the pointer, and the by-pointer array core added here
/// mirrors the `Vec` one B-2026-08-09-21 split out for the same reason —
/// an `Array` inline in another container's buffer has no `VarSlot` to
/// describe it.
///
/// `field` is the shape that motivated the row: a 2D grid on a struct,
/// reached as `self.grid[i][j]` from a method. `alias` is load-bearing —
/// a write through the container is visible through the borrow, so these
/// are pointers into the nested buffer rather than copies of the element.
///
/// Twin of `tests/interpreter.rs`'s
/// `test_ref_binding_over_nested_chain_roots`, pinned to the same string.
#[test]
fn e2e_ref_binding_over_nested_chain_roots() {
    let Some(out) = run_program(
        "struct Hh { grid: Vec[Vec[i64]] }\n\
             fn main() {\n\
             \x20   let av: Array[Vec[i64], 1] = Array[[1, 2]];\n\
             \x20   let g1 = ref av[0][1];\n\
             \x20   println(f\"arrayvec {g1}\");\n\
             \x20   let b: Vec[Vec[i64]] = [[3, 4]];\n\
             \x20   let sl: Slice[Vec[i64]] = b.as_slice();\n\
             \x20   let g2 = ref sl[0][1];\n\
             \x20   println(f\"slicevec {g2}\");\n\
             \x20   let va: Vec[Array[i64, 2]] = [Array[5, 6]];\n\
             \x20   let g3 = ref va[0][1];\n\
             \x20   println(f\"vecarray {g3}\");\n\
             \x20   let aa: Array[Array[i64, 2], 1] = Array[Array[7, 8]];\n\
             \x20   let g4 = ref aa[0][1];\n\
             \x20   println(f\"arrayarray {g4}\");\n\
             \x20   let h: Hh = Hh { grid: [[9, 10]] };\n\
             \x20   let g5 = ref h.grid[0][1];\n\
             \x20   println(f\"field {g5}\");\n\
             \x20   let mut m: Vec[Array[i64, 2]] = [Array[1, 2]];\n\
             \x20   let ga = ref m[0][1];\n\
             \x20   m[0][1] = 66;\n\
             \x20   println(f\"alias {ga}\");\n\
             }",
    ) else {
        return;
    };
    assert_eq!(
        out, "arrayvec 2\nslicevec 4\nvecarray 6\narrayarray 8\nfield 10\nalias 66\n",
        "a nested `ref` chain must lower from every container root"
    );
}

/// B-2026-09-07-52 — a field assign whose base is a BORROWED view fires
/// the displaced old value's Drop body, on every surface.
///
/// Two spellings were losing it. `self.one = r` inside a method never
/// reached `emit_displaced_field_bodies` at all, because that emitter
/// flattens the target's base over `Identifier`/`FieldAccess` and `self`
/// parses as `SelfValue` — the same shape B-2026-08-26-18 closed in the
/// INDEX-assign twin. `h.one = r` over a `mut ref Bs` param DID reach it
/// and then failed the `full_armed` gate, which reads the ROOT's
/// scope-exit `UserDrop` action: a frame that only borrows the aggregate
/// registers none, so a borrowed base failed it unconditionally.
///
/// Nobody else fires this body. The caller's own scope-exit drop reads the
/// field AFTER the store, so it covers the NEW value (`dS7` here); the
/// displaced one has no other owner, and design.md pins "a value is
/// dropped exactly once".
///
/// `c2` is the control the row was filed against — the caller-side
/// spelling `b.one = mks(7)`, which has fired since B-2026-08-01-20 and
/// must keep firing. `c3` is the free-function param view (the `full_armed`
/// half alone), `c4` a method assign whose RHS is a literal rather than a
/// param (so B-2026-08-01-19's owned-param retraction is not what is being
/// measured), and `c5` the deep chain `self.inner.one = r`.
///
/// Measured on the parent: every cell but `c2` printed no `dS1`, on
/// `--interp` and both compiled backends alike. Twin of
/// `tests/interpreter.rs`'s
/// `test_borrowed_base_field_assign_displaced_bodies`.
#[test]
fn e2e_borrowed_base_field_assign_displaced_bodies() {
    let Some(out) = run_program(
        "struct Rs { id: i64, name: String }\n\
             impl Drop for Rs {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"dS{self.id}\")\n\
             \x20   }\n\
             }\n\
             fn mks(i: i64) -> Rs { return Rs { id: i, name: f\"h{i}\" }; }\n\
             struct Bs { mut one: Rs }\n\
             struct Outer { mut inner: Bs }\n\
             impl Bs {\n\
             \x20   fn set(mut ref self, r: Rs) { self.one = r; }\n\
             \x20   fn set_lit(mut ref self) { self.one = mks(9); }\n\
             }\n\
             impl Outer {\n\
             \x20   fn set_deep(mut ref self, r: Rs) { self.inner.one = r; }\n\
             }\n\
             fn setf(h: mut ref Bs, r: Rs) { h.one = r; }\n\
             fn main() {\n\
             \x20   println(\"c1\");\n\
             \x20   let mut a = Bs { one: mks(1) };\n\
             \x20   a.set(mks(7));\n\
             \x20   println(f\"o{a.one.id}\");\n\
             \x20   println(\"c2\");\n\
             \x20   let mut b = Bs { one: mks(1) };\n\
             \x20   b.one = mks(7);\n\
             \x20   println(f\"o{b.one.id}\");\n\
             \x20   println(\"c3\");\n\
             \x20   let mut c = Bs { one: mks(1) };\n\
             \x20   setf(mut c, mks(7));\n\
             \x20   println(f\"o{c.one.id}\");\n\
             \x20   println(\"c4\");\n\
             \x20   let mut d = Bs { one: mks(1) };\n\
             \x20   d.set_lit();\n\
             \x20   println(f\"o{d.one.id}\");\n\
             \x20   println(\"c5\");\n\
             \x20   let mut e = Outer { inner: Bs { one: mks(1) } };\n\
             \x20   e.set_deep(mks(7));\n\
             \x20   println(f\"o{e.inner.one.id}\");\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(
        out,
        "c1\ndS1\no7\ndS7\nc2\ndS1\no7\ndS7\nc3\ndS1\no7\ndS7\n\
             c4\ndS1\no9\ndS9\nc5\ndS1\no7\ndS7\nend\n"
    );
}

/// B-2026-09-08-2 — a `mut ref` ARGUMENT whose place projects off an
/// RC-FALLBACK-PROMOTED local silently discarded the write, and through a
/// NESTED projection stored through a wild pointer.
///
/// `mut_ref_place_root_ptr` ended `Some(slot)` for any root that is not a
/// `ref` param. A promoted binding's slot holds a `{i64 rc, T}` box HANDLE,
/// so the callee was handed a pointer to an rvalue COPY (cells 1-2, write
/// lost) or — once a second GEP compounded the error — an address that is
/// not the program's at all (cell 3).
///
/// SIBLING OF B-2026-09-07-48 AT A SECOND RESOLVER. That row fixed the READ
/// receiver (`lower_field_access_ptr`); a `mut ref` argument resolves its
/// pointer independently, so its fix does not reach here. A THIRD resolver
/// shares the shape and deliberately keeps the old policy:
/// `field_chain_place_ptr`'s drop-suppression callers must NOT follow the
/// handle (`projection_root_is_rc_boxed` records why), which is why the
/// walk is parameterised rather than widened.
///
/// CELL 3 IS THE ONE THAT MATTERS MOST. Its parent values are not merely
/// wrong, they are not even stable — measured -2305843009213693952
/// compiled, and 8070450532247928832 then 6917529027641081856 on two JIT
/// runs of the same program — because the store lands wherever the walked-off
/// GEP points. Cell 5 pins that the write reaches a STABLE place: two
/// increments must compose to 11, which a fresh copy per call cannot do.
#[test]
fn e2e_mut_ref_arg_place_off_a_promoted_local_reaches_the_box() {
    const PRE: &str = r#"struct Inner { v: i64 }
struct P { a: String, b: i64 }
struct Outer { q: Inner, a: String }
fn seed() -> i64 { env.args().len() }
fn payload() -> String { f"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa" }
fn mkp(n: i64) -> P { return P { a: payload(), b: n }; }
fn mko() -> Outer { return Outer { q: Inner { v: 5 }, a: payload() }; }
fn bump(s: mut ref String) { s.push_str("XY"); }
fn bumpi(v: mut ref i64) { v = v + 1; }
"#;
    // (source tail, expected stdout, cell name)
    let cells: [(&str, &str, &str); 6] = [
        // 1 — heap field. Parent: 0 compiled, 38 on the JIT (call a no-op).
        (
            r#"fn go() -> i64 { let mut t = mkp(9); let mut i = 0i64;
  while i < 2i64 { let s = t.a; i = i + 1; }
  bump(mut t.a); return t.a.len(); }
fn main() { println(go()); }
"#,
            "40",
            "heap_field_write_lands",
        ),
        // 2 — NON-heap field off the same promoted binding. Rules out
        // anything String-specific: the write is lost just as completely.
        // Parent: 9 on all three compiled backends.
        (
            r#"fn go() -> i64 { let mut t = mkp(9); let mut i = 0i64;
  while i < 2i64 { let s = t.a; i = i + 1; }
  bumpi(mut t.b); return t.b; }
fn main() { println(go()); }
"#,
            "10",
            "non_heap_field_write_lands",
        ),
        // 3 — NESTED projection. Parent stores through a wild pointer and
        // the value is not reproducible between runs.
        (
            r#"fn go() -> i64 { let mut t = mko(); let mut i = 0i64;
  while i < 2i64 { let s = t.a; i = i + 1; }
  bumpi(mut t.q.v); return t.q.v; }
fn main() { println(go()); }
"#,
            "6",
            "nested_projection_write_lands",
        ),
        // 4 — CONTROL: no loop, so no promotion and no box. The plain slot
        // is the correct place here; the new arm must not divert it.
        (
            r#"fn go() -> i64 { let mut t = mkp(9); bumpi(mut t.b); return t.b; }
fn main() { println(go()); }
"#,
            "10",
            "control_no_promotion",
        ),
        // 5 — the place must be STABLE, not merely written once: two
        // increments through the same projection compose only if both
        // reached the box. A per-call rvalue copy yields 10.
        (
            r#"fn go() -> i64 { let mut t = mkp(9); let mut i = 0i64;
  while i < 2i64 { let s = t.a; i = i + 1; }
  bumpi(mut t.b); bumpi(mut t.b); return t.b; }
fn main() { println(go()); }
"#,
            "11",
            "repeated_write_composes",
        ),
        // 6 — CONTROL: the projection READ that B-2026-09-07-48 fixed, kept
        // beside the write so a regression in either direction is visible
        // in one fixture.
        (
            r#"fn go() -> i64 { let t = mkp(9); let mut i = 0i64; let mut n = 0i64;
  while i < 3i64 { let s = t.a; n = n + s.len(); i = i + 1; }
  return n + t.a.len(); }
fn main() { println(go()); }
"#,
            "152",
            "control_read_path_still_correct",
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

/// B-2026-08-01-1 — `len` on an `unwrap`ped borrow accessor over a
/// fresh-temp container (`mk_rows().first().unwrap().len()`) must not
/// drop-track the unwrapped row: it ALIASES element storage the
/// get-family materialization already frees per-element, and the second
/// free aborted the binary (glibc double-free). The intercept now skips
/// tracking for `unwrap`/`expect` over `scrutinee_is_borrow_call`
/// receivers (`expr_is_unwrap_of_borrow_accessor`).
#[test]
fn e2e_len_of_unwrapped_borrow_row_single_free() {
    let Some(out) = run_program(
        "fn mk_rows() -> Vec[Vec[i64]] {\n\
             \x20   let mut v: Vec[Vec[i64]] = Vec.new();\n\
             \x20   v.push(Vec[1, 2, 3]);\n\
             \x20   v.push(Vec[4, 5]);\n\
             \x20   v\n\
             }\n\
             fn main() {\n\
             \x20   println(f\"{mk_rows().first().unwrap().len()}\");\n\
             \x20   println(f\"{mk_rows().last().unwrap().len()}\");\n\
             \x20   println(f\"{mk_rows().get(1).unwrap().len()}\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "3\n2\n2\n");
}

#[test]
fn test_e2e_arena_as_ref_param() {
    // Arena/Interner as function PARAMETERS — the primary arena use case
    // ("pass the arena to functions that build nodes"). Was loud-fail
    // ("no handler for method 'push' on variable 'a'") because a
    // `ref Arena[T]` param wasn't registered in `arena_vars`;
    // `register_var_from_type_expr` now registers it (params route
    // through it), and `get_data_ptr` already resolves the ref-param
    // indirection so the handle load works for both a local binding and
    // a `ref` param. Covers scalar + struct elements and a returned
    // `ArenaRef[T]` handle. interp == JIT == AOT.
    let out = run_program(
        r#"
struct Node { val: i64, kids: i64 }
fn build(a: ref Arena[Node], v: i64) -> ArenaRef[Node] {
    a.push(Node { val: v, kids: 0 })
}
fn total(a: ref Arena[i64]) -> i64 {
    let r0 = a.push(3);
    let r1 = a.push(4);
    a.get(r0) + a.get(r1)
}
fn main() {
    let a: Arena[Node] = Arena.new();
    let r0 = build(a, 10);
    let r1 = build(a, 20);
    println(a.get(r0).val);
    println(a.get(r1).val);
    println(a.len());
    let b: Arena[i64] = Arena.new();
    println(total(b));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "10\n20\n2\n7");
    }
}

#[test]
fn test_e2e_interner_as_ref_param() {
    // `ref Interner` parameter dispatch (same registration fix). A helper
    // interns through a borrowed interner; dedup + count survive across
    // the call boundary.
    let out = run_program(
        r#"
fn add(t: ref Interner, s: ref String) -> Symbol {
    t.intern(s)
}
fn main() {
    let mut t: Interner = Interner.new();
    let a = add(t, "x");
    let b = add(t, "x");
    let c = add(t, "y");
    println(a == b);
    println(a == c);
    println(t.len());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "true\nfalse\n2");
    }
}

// ── Assign-through on a `mut ref` scalar PARAMETER (B-2026-06-30-9/10) ──
//
// `a = a + b` on a `mut ref T` scalar lvalue desugars to `*a = *a + b`
// (design.md :5306): the RHS reads through the borrow, and the write must
// propagate to the caller's `T`. Two distinct bugs were fixed together:
//   1. typecheck (B-2026-06-30-9) — the arithmetic arm of `infer_binary`
//      didn't auto-deref a numeric-scalar borrow operand, so `x = x + 1`
//      HARD-errored on `check`/`build` while `run` warned-and-applied.
//   2. codegen (B-2026-06-30-10) — even once typecheck passed, the Assign /
//      CompoundAssign identifier store wrote into the borrow-param's alloca
//      (which holds the POINTER) instead of THROUGH it, so the built binary
//      silently printed the un-incremented value (10) while `run` printed 11.
// These lock the build side to the interpreter's answer (11) — sibling of
// B-2026-06-30-6 (that fixed `mut ref Vec` element typing; this fixes a
// `mut ref` scalar used directly).

/// `x = x + 1i64` (explicit assign, no `*`) on a `mut ref i64` param
/// updates the caller's value in the built binary — was 10 (silent
/// clobber-the-pointer miscompile), now 11 matching `karac run`.
#[test]
fn mut_ref_scalar_param_implicit_assign_through_e2e() {
    let src = "fn inc(x: mut ref i64) { x = x + 1i64; }\n\
                   fn main() {\n\
                   \x20   let mut n: i64 = 10i64; inc(mut n); println(n);\n\
                   }\n";
    assert_eq!(run_program(src).as_deref(), Some("11\n"));
}

/// `x += 1i64` (compound assign) on a `mut ref i64` param — the compound
/// sibling, same store-through fix.
#[test]
fn mut_ref_scalar_param_compound_assign_through_e2e() {
    let src = "fn inc(x: mut ref i64) { x += 1i64; }\n\
                   fn main() {\n\
                   \x20   let mut n: i64 = 10i64; inc(mut n); println(n);\n\
                   }\n";
    assert_eq!(run_program(src).as_deref(), Some("11\n"));
}

/// A `ref i64` (immutable borrow) operand reads through the borrow in
/// arithmetic and returns the value — no assign-through. Prints 11.
#[test]
fn ref_scalar_param_read_through_arithmetic_e2e() {
    let src = "fn plus1(x: ref i64) -> i64 { x + 1i64 }\n\
                   fn main() {\n\
                   \x20   let n: i64 = 10i64; println(plus1(n));\n\
                   }\n";
    assert_eq!(run_program(src).as_deref(), Some("11\n"));
}

/// End-to-end: the elided read-only binding produces the correct result, and
/// — critically — the UAF-shaped negatives (overwrite / grow) still compute
/// the right answer at runtime (a wrongly-borrowed binding would read freed
/// memory). B-2026-06-19-6.
#[test]
fn e2e_borrow_elision_correctness_positive_and_negatives() {
    // Positive: sum of 4 single-element [1] vectors = 4.
    if let Some(out) = run_program(
        "fn main() {\n\
             let mut out: Vec[Vec[i64]] = Vec.new();\n\
             let mut b: Vec[i64] = Vec.new(); b.push(1i64);\n\
             let mut k = 0i64; while k < 4i64 { out.push(b.clone()); k = k + 1i64; }\n\
             let mut acc = 0i64; let m = out.len(); let mut j = 0i64;\n\
             while j < m { let r = ref out[j]; let mut i = 0i64; let rl = r.len();\n\
                 while i < rl { acc = acc + r[i]; i = i + 1i64; } j = j + 1i64; }\n\
             println(f\"{acc}\");\n\
             }",
    ) {
        assert_eq!(out, "4\n");
    }
    // Negative (escape): each `r` is moved into `keep`, so the binding must
    // be a clone (an aliasing borrow would dangle once `out` drops). Sum over
    // 4 single-element [7] vectors = 28; a mis-borrowed `r` would dangle.
    if let Some(out) = run_program(
            "fn main() {\n\
             let mut out: Vec[Vec[i64]] = Vec.new();\n\
             let mut b: Vec[i64] = Vec.new(); b.push(7i64);\n\
             let mut k = 0i64; while k < 4i64 { out.push(b.clone()); k = k + 1i64; }\n\
             let mut keep: Vec[Vec[i64]] = Vec.new();\n\
             let mut j = 0i64; while j < out.len() { let r = out[j].clone(); keep.push(r); j = j + 1i64; }\n\
             let mut acc = 0i64; let mut i = 0i64;\n\
             while i < keep.len() { let z = ref keep[i]; acc = acc + z[0i64]; i = i + 1i64; }\n\
             println(f\"{acc}\");\n\
             }",
        ) {
            assert_eq!(out, "28\n");
        }
}

#[test]
fn e2e_generic_refinement_alias_projects_for_reads() {
    // B-2026-07-29-26 — the AOT twin of the interpreter test of the
    // same shape: a generic refinement alias narrowed with `as`, then
    // iterated, reading a field and casting a second refinement out to
    // its base inside the loop. Codegen already resolved refinement
    // aliases through its own `refinement_base` maps, so this leg was
    // *already* correct — the defect was the typechecker rejecting the
    // program before `karac build` ever reached codegen. Pinned here so
    // the two lanes stay in agreement, and because this harness runs
    // codegen directly (it does not gate on typecheck errors), which is
    // precisely why the divergence went unnoticed.
    if let Some(out) = run_program(
        "pub type PositiveQty = i64 where self > 0;\n\
             pub type NonEmpty[T] = Vec[T] where self.len() > 0;\n\
             pub struct Item { price: f64, qty: PositiveQty }\n\
             pub fn total(rows: NonEmpty[Item]) -> f64 {\n\
                 let mut t = 0.0;\n\
                 for r in rows { t = t + r.price * (r.qty as f64); }\n\
                 t\n\
             }\n\
             fn main() {\n\
                 let mut xs: Vec[Item] = Vec.new();\n\
                 xs.push(Item { price: 1.5, qty: 2 });\n\
                 xs.push(Item { price: 0.25, qty: 4 });\n\
                 println(total(xs as NonEmpty[Item]));\n\
             }",
    ) {
        assert_eq!(out, "4\n");
    }
}

// ── Ref-param-rooted nested store (the codegen follow-on) ──
//
// `self.inner.x = v` / `p.inner.x = v` where the chain root is a `ref` /
// `mut ref` plain-struct parameter. The slot holds a pointer-to-struct, so
// the nested-store path must deref the root (via `get_data_ptr`) before
// GEPing the chain — otherwise the store targets the pointer slot and the
// write is silently dropped (the interpreter already handled this). Depth-1
// `self.x = v` / `p.x = v` always worked; this closes the depth >= 2 case.

#[test]
fn e2e_nested_store_through_mut_ref_param() {
    if let Some(out) = run_program(
        "struct Inner { x: i64 }\n\
             struct Outer { inner: Inner }\n\
             fn bump(o: mut ref Outer) {\n\
                 o.inner.x = 99;\n\
             }\n\
             fn main() {\n\
                 let mut o = Outer { inner: Inner { x: 1 } };\n\
                 bump(mut o);\n\
                 println(o.inner.x);\n\
             }",
    ) {
        assert_eq!(out, "99\n");
    }
}

#[test]
fn e2e_nested_store_through_mut_ref_self() {
    if let Some(out) = run_program(
        "struct Inner { x: i64 }\n\
             struct Outer { inner: Inner }\n\
             impl Outer {\n\
                 fn bump(mut ref self) {\n\
                     self.inner.x = 42;\n\
                 }\n\
             }\n\
             fn main() {\n\
                 let mut o = Outer { inner: Inner { x: 1 } };\n\
                 o.bump();\n\
                 println(o.inner.x);\n\
             }",
    ) {
        assert_eq!(out, "42\n");
    }
}

#[test]
fn e2e_compound_nested_store_through_mut_ref_self() {
    if let Some(out) = run_program(
        "struct Inner { x: i64 }\n\
             struct Outer { inner: Inner }\n\
             impl Outer {\n\
                 fn bump(mut ref self) {\n\
                     self.inner.x += 100;\n\
                 }\n\
             }\n\
             fn main() {\n\
                 let mut o = Outer { inner: Inner { x: 5 } };\n\
                 o.bump();\n\
                 println(o.inner.x);\n\
             }",
    ) {
        assert_eq!(out, "105\n");
    }
}

#[test]
fn test_e2e_mut_ref_scalar_value_reads() {
    // B-2026-07-15-3: a `mut ref` scalar param reads as its value type in
    // an annotated let, an index expression, an argument, and a cast —
    // and the write-back through the borrow still reaches the caller.
    // Must match the interpreter.
    if let Some(out) = run_program(
        "fn take(v: i64) -> i64 { v * 2 }\n\
             fn probe(xs: Vec[i64], cur: mut ref i64) {\n\
                 let ci: i64 = cur;\n\
                 println(ci);\n\
                 println(xs[cur]);\n\
                 println(take(cur));\n\
                 let c2 = cur as i64;\n\
                 println(c2);\n\
                 cur = cur + 1;\n\
             }\n\
             fn main() {\n\
                 let xs: Vec[i64] = [10, 20, 30];\n\
                 let mut c: i64 = 1;\n\
                 probe(xs, mut c);\n\
                 println(c);\n\
             }",
    ) {
        assert_eq!(out, "1\n20\n2\n1\n2\n");
    }
}

/// B-2026-09-23-2 — a borrowed scalar reads as its value as an `if` /
/// `while` condition, a match guard, and under `not`, `and` / `or`, unary
/// `-`, `~` and the five bitwise operators (a `u8` borrow included, so the
/// narrow-width path is covered), and the writes through the borrow reach the
/// caller. Strict: the interpreter twin `test_ref_scalar_operators_and_
/// conditions` asserts the same bytes.
#[test]
fn test_e2e_ref_scalar_operators_and_conditions() {
    assert_eq!(
        run_program(
            "fn ops(flag: mut ref bool, x: mut ref i64, b: mut ref u8, r: ref f64, seen: ref bool) -> i64 {\n\
                 let mut out = 0;\n\
                 if flag { out = out + 1; }\n\
                 if not flag { out = out + 1000; }\n\
                 if flag and seen { out = out + 2; }\n\
                 if seen or flag { out = out + 4; }\n\
                 let neg = -x;\n\
                 let inv = ~x;\n\
                 let bits = (x & 6) + (x | 1) + (x ^ 3) + (x << 2) + (x >> 1);\n\
                 let nb1 = b & 15;\n\
                 let nb2 = b | 16;\n\
                 let nb3 = b >> 1;\n\
                 let nb4 = ~b;\n\
                 let nb5 = b ^ 255;\n\
                 let nb6 = b << 1;\n\
                 let nr = -r;\n\
                 let pick = if flag { 10 } else { 20 };\n\
                 let mut spins = 0;\n\
                 while flag {\n\
                     spins = spins + 1;\n\
                     if spins == 3 { flag = false; }\n\
                 }\n\
                 let g = match 7 {\n\
                     v if seen => v * 100,\n\
                     v => v,\n\
                 };\n\
                 println(f\"neg {neg} inv {inv} bits {bits} nb {nb1} {nb2} {nb3} {nb4} {nb5} {nb6} nr {nr} pick {pick} spins {spins} g {g}\");\n\
                 x = x + 1;\n\
                 b = b + 1;\n\
                 return out;\n\
             }\n\
             \n\
             fn main() {\n\
                 let mut flag = true;\n\
                 let mut x = 5;\n\
                 let mut b: u8 = 100;\n\
                 let r = 1.5;\n\
                 let seen = false;\n\
                 let o1 = ops(mut flag, mut x, mut b, r, seen);\n\
                 println(f\"o1 {o1} flag {flag} x {x} b {b}\");\n\
                 let o2 = ops(mut flag, mut x, mut b, r, true);\n\
                 println(f\"o2 {o2} flag {flag} x {x} b {b}\");\n\
             }"
        ),
        Some("neg -5 inv -6 bits 37 nb 4 116 50 155 155 200 nr -1.5 pick 10 spins 3 g 7\no1 5 flag false x 6 b 101\nneg -6 inv -7 bits 45 nb 5 117 50 154 154 202 nr -1.5 pick 20 spins 0 g 700\no2 1004 flag false x 7 b 102\n".to_string())
    );
}

/// B-2026-08-13-16 — the MUTATION half of `test_e2e_field_bound_out_of_
/// local_is_a_copy`, asserted as a three-surface differential.
///
/// That test pinned that a field READ leaves the source intact. This pins
/// that a WRITE through a rebinding is not visible through the source, and
/// it is here rather than only in the interpreter suite because the two
/// surfaces DISAGREED — with the interpreter, unusually, the wrong one. It
/// aliased (`Value::Struct` copies its field map but Arc-bumps a `Vec`
/// field), while codegen already produced the copy design.md's CONSUME
/// classification of `let y = v` requires.
///
/// Five positions, all the ones where the compiled backends can express the
/// shape today. The `t.0`-rooted and whole-tuple rebinds from the
/// interpreter twin are absent on purpose: the first still EMPTIES its
/// source under codegen (the tuple-element analogue of B-2026-08-13-14's
/// field-source defect) and the second does not compile at all
/// (B-2026-08-02-10's tuple-element receiver gap), so neither has a
/// meaningful compiled answer to compare against yet.
#[test]
fn test_e2e_let_rebinding_is_a_copy_not_an_alias() {
    assert_eq!(
        run_program(
            "#[derive(Clone)]
struct A { mut lines: Vec[String] }\n\
                 struct B { mut a: A }\n\
                 fn main() {\n\
                     let mut a1 = A { lines: Vec.new() };\n\
                     a1.lines.push(f\"x\");\n\
                     let mut r1 = a1;\n\
                     r1.lines.push(f\"y\");\n\
                     println(f\"{r1.lines.len()} {a1.lines.len()}\");\n\
                     let mut a2 = A { lines: Vec.new() };\n\
                     a2.lines.push(f\"x\");\n\
                     let mut r2 = a2.lines;\n\
                     r2.push(f\"y\");\n\
                     println(f\"{r2.len()} {a2.lines.len()}\");\n\
                     let mut inner = A { lines: Vec.new() };\n\
                     inner.lines.push(f\"x\");\n\
                     let mut b3 = B { a: inner };\n\
                     let mut r3 = b3.a;\n\
                     r3.lines.push(f\"y\");\n\
                     let chk3 = b3.a;\n\
                     println(f\"{r3.lines.len()} {chk3.lines.len()}\");\n\
                     let mut v4: Vec[A] = Vec.new();\n\
                     v4.push(A { lines: Vec.new() });\n\
                     v4[0].lines.push(f\"x\");\n\
                     let mut r4 = v4[0].clone();\n\
                     r4.lines.push(f\"y\");\n\
                     println(f\"{r4.lines.len()} {v4[0].lines.len()}\");\n\
                     let mut m6: Map[i64, Vec[i64]] = Map.new();\n\
                     let _ = m6.insert(1, Vec.new());\n\
                     m6[1].push(1);\n\
                     let mut r6 = m6;\n\
                     r6[1].push(2);\n\
                     println(f\"{r6[1].len()} {m6[1].len()}\");\n\
                 }"
        )
        .as_deref(),
        Some("2 1\n2 1\n2 1\n2 1\n2 1\n"),
    );
}

#[test]
fn test_e2e_ref_borrow_stored_positions_no_double_free() {
    // B-2026-07-16-5: two borrow-materialization legs that freed the
    // LENDER's buffer (glibc double-free under AOT; interp correct).
    // Leg 1 — `Option[ref String]`: `Some(s)` with `s: ref String`
    // packed the lender's {ptr,len,cap} into the payload, so the
    // match-arm binding cleanup freed the borrowed buffer; fixed by
    // zeroing the payload's cap word at pack time (borrow-view
    // discipline). Leg 2 — a declared `ref String` STRUCT field passed
    // to a `ref` param (`shout(p.source)`): the ref-arg rvalue path
    // deref'd the borrow-pointer slot into a temp and queued a
    // cap-guarded free of the lender's buffer; fixed by forwarding the
    // stored borrow pointer directly (the slot already holds the exact
    // `ptr` ABI the callee expects).
    if let Some(out) = run_program(
        "struct Parser { source: ref String, position: i64 }\n\
             fn make_parser(s: ref String) -> ref Parser {\n\
                 Parser { source: s, position: 0 }\n\
             }\n\
             fn shout(x: ref String) { println(x); }\n\
             fn wrap(s: ref String) -> Option[ref String] {\n\
                 Option.Some(s)\n\
             }\n\
             fn main() {\n\
                 let mut s: String = \"\";\n\
                 s.push_str(\"a longer payload string\");\n\
                 match wrap(s) {\n\
                     Some(n) => println(n.len()),\n\
                     None => println(0),\n\
                 }\n\
                 println(s);\n\
                 let s2 = String.from(\"input data\");\n\
                 let p = make_parser(s2);\n\
                 shout(p.source);\n\
             }",
    ) {
        assert_eq!(out, "23\na longer payload string\ninput data\n");
    }
}

#[test]
fn e2e_contains_on_borrow_local_codegen() {
    // The borrow-local-method path (commit 2b9e2de3) that surfaced
    // B-2026-06-10-1: `contains` on a `ref`-bound Vec/String receiver.
    if let Some(out) = run_program(
        "fn has(xs: ref Vec[i64], s: ref String) -> bool {\n\
                 let found_num = xs.contains(2);\n\
                 let found_sub = s.contains(\"ll\");\n\
                 found_num and found_sub\n\
             }\n\
             fn main() {\n\
                 let mut xs: Vec[i64] = Vec.new();\n\
                 xs.push(1);\n\
                 xs.push(2);\n\
                 let s: String = \"hello\";\n\
                 println(has(xs, s));\n\
             }",
    ) {
        assert_eq!(out, "true\n");
    }
}

#[test]
fn test_e2e_borrow_return_let_bound() {
    // B-2026-06-07-5: returning a borrow (`-> ref T`) — a field reached
    // through a `ref` param (String + scalar) and a forwarded `ref`
    // param — derefs correctly at a let-bound caller.
    let out = run_program(
        "struct User { name: String, age: i64 }\n\
             fn name_of(u: ref User) -> ref String { u.name }\n\
             fn age_of(p: ref User) -> ref i64 { p.age }\n\
             fn fwd(s: ref String) -> ref String { s }\n\
             fn main() {\n\
             \x20   let u = User { name: \"ada\", age: 36 };\n\
             \x20   let n = name_of(u); println(n);\n\
             \x20   let a = age_of(u); println(a);\n\
             \x20   let s = \"hello\"; let f = fwd(s); println(f);\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "ada\n36\nhello");
    }
}

#[test]
fn test_e2e_borrow_return_local_read_methods() {
    // B-2026-06-07-5 residue: read-only methods BEYOND `len`/`is_empty` on
    // a borrow-LOCAL (`let n = sid(s)`, registered in `ref_params`) now
    // dispatch through the value-receiver Vec/String path
    // (`compile_vec_method`), reading through the borrow via
    // `get_data_ptr`'s ref-deref. Before the fix these fell through to
    // "no handler for method X on variable n" — only `len`/`is_empty` had a
    // dedicated borrow-local arm. Covers String `starts_with`/`is_empty`
    // and Vec `get`/`first`/`last`.
    let out = run_program(
        "fn sid(s: ref String) -> ref String { s }\n\
             fn vid(v: ref Vec[i64]) -> ref Vec[i64] { v }\n\
             fn main() {\n\
             \x20   let s: String = \"hello world\";\n\
             \x20   let n = sid(s);\n\
             \x20   println(n.starts_with(\"hello\"));\n\
             \x20   println(n.is_empty());\n\
             \x20   let xs: Vec[i64] = [10, 20, 30];\n\
             \x20   let m = vid(xs);\n\
             \x20   match m.get(1) { Some(x) => println(x), None => println(0 - 1) }\n\
             \x20   match m.first() { Some(x) => println(x), None => println(0 - 1) }\n\
             \x20   match m.last() { Some(x) => println(x), None => println(0 - 1) }\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "true\nfalse\n20\n10\n30");
    }
}

#[test]
fn test_e2e_borrow_return_if_multi_source() {
    // Tier 2 (B-2026-06-07-5): `longer`-style `if` over two `ref`
    // params returns a borrow from whichever branch runs — phi of
    // pointers, deref'd correctly at the let-bound caller.
    let out = run_program(
        "fn longer(a: ref String, b: ref String) -> ref String {\n\
             \x20   if a.len() > b.len() { a } else { b }\n\
             }\n\
             fn main() {\n\
             \x20   let x = \"hi\"; let y = \"wordy\";\n\
             \x20   let n = longer(x, y); println(n);\n\
             \x20   let p = \"aaaaaa\"; let q = \"bb\";\n\
             \x20   let m = longer(p, q); println(m);\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "wordy\naaaaaa");
    }
}

#[test]
fn test_e2e_borrow_return_method_accessor() {
    // B-2026-06-07-5 method-ref: `ref self -> ref T` accessors. The
    // call result is bound as a ref-local (via the lowering span table)
    // and derefs correctly at use.
    let out = run_program(
        "struct User { name: String, age: i64 }\n\
             impl User {\n\
             \x20   fn name(ref self) -> ref String { self.name }\n\
             \x20   fn age(ref self) -> ref i64 { self.age }\n\
             }\n\
             fn main() {\n\
             \x20   let u = User { name: \"alice\", age: 30 };\n\
             \x20   let n = u.name(); println(n);\n\
             \x20   let a = u.age(); println(a);\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "alice\n30");
    }
}

#[test]
fn test_e2e_method_ref_struct_arg() {
    // B-2026-06-12-8: a struct-typed `ref`/`mut ref` NON-receiver arg in a
    // METHOD call must be passed by address, not by value. The method-call
    // arg-lowering path used to emit the struct by value into a `ptr` param
    // slot (invalid IR / module-verify failure). Two halves to the fix:
    //   (1) ownership no longer classifies a `ref`/`mut ref` method arg as a
    //       container-store consume, so the borrowed binding is NOT spuriously
    //       RC-promoted (which boxed it on the heap and segfaulted at use);
    //   (2) codegen passes the arg's address for a `ref`/`mut ref` struct param.
    // Covers a `ref` arg (read), a `mut ref` arg (write-through, observed
    // back in the caller), and an accumulating loop reusing the borrow.
    let out = run_program(
            "struct Acc { total: i64 }\n\
             struct Src { val: i64 }\n\
             impl Acc {\n\
             \x20   fn add(mut ref self, s: ref Src) { self.total = self.total + s.val; }\n\
             \x20   fn drain(mut ref self, s: mut ref Src) { self.total = self.total + s.val; s.val = 0; }\n\
             }\n\
             fn main() {\n\
             \x20   let mut a: Acc = Acc { total: 0 };\n\
             \x20   let mut s: Src = Src { val: 5 };\n\
             \x20   let mut i: i64 = 0;\n\
             \x20   while i < 4 { a.add(s); i = i + 1; }\n\
             \x20   println(f\"{a.total}\");\n\
             \x20   a.drain(s);\n\
             \x20   println(f\"{a.total}\");\n\
             \x20   println(f\"{s.val}\");\n\
             \x20   a.add(Src { val: 7 });\n\
             \x20   println(f\"{a.total}\");\n\
             }\n",
        );
    if let Some(out) = out {
        // 4*5=20; +5=25, s.val zeroed; +7=32.
        assert_eq!(out.trim(), "20\n25\n0\n32");
    }
}

#[test]
fn test_e2e_borrow_return_chained_call() {
    // Chained borrow return (B-2026-06-07-5): a borrow-returning free-fn
    // call in tail position (`echo(t)`) and at an explicit `return`. The
    // call lowers to the borrow `ptr` directly (compiled once, gate
    // bypassed); the let-bound result derefs correctly, and `.len()` on
    // the borrow goes through the ref-local read-only len path.
    let out = run_program(
        "fn echo(s: ref String) -> ref String { s }\n\
             fn echo_twice(s: ref String) -> ref String {\n\
             \x20   let t = echo(s);\n\
             \x20   echo(t)\n\
             }\n\
             fn echo_thrice(s: ref String) -> ref String {\n\
             \x20   let t = echo(s);\n\
             \x20   return echo_twice(t);\n\
             }\n\
             fn main() {\n\
             \x20   let s = String.from(\"chained\");\n\
             \x20   let r = echo_twice(s); println(r); println(r.len());\n\
             \x20   let q = String.from(\"again\");\n\
             \x20   let w = echo_thrice(q); println(w);\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "chained\n7\nagain");
    }
}

#[test]
fn test_e2e_borrow_return_direct_use() {
    // Direct use of a borrow-returning call result (Tier-1.5,
    // B-2026-06-07-5) — consumed in place, not bound to a `let`. Three
    // accepted positions, each lowered correctly:
    //   • value position: `println(name_of(s))` loads the pointee.
    //   • ref-parameter argument: `shout(name_of(s))` forwards the borrow
    //     pointer into another `ref String` param (no value copy/free).
    //   • method-on-result: `name_of(s).len()` reads through the borrow.
    let out = run_program(
        "fn name_of(u: ref String) -> ref String { u }\n\
             fn shout(x: ref String) { println(x); }\n\
             fn main() {\n\
             \x20   let s = String.from(\"hello\");\n\
             \x20   println(name_of(s));\n\
             \x20   shout(name_of(s));\n\
             \x20   println(name_of(s).len());\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "hello\nhello\n5");
    }
}

#[test]
fn test_e2e_borrow_return_call_into_ref_arg() {
    // B-2026-06-10-4: a borrow-returning call forwarded straight into a
    // `ref` parameter (`first(pick(v))`). The call result IS the borrow
    // ptr; it must be forwarded directly, NOT loaded-into-a-value-then-
    // stored-in-a-cleanup-tracked-temp (which double-freed the source's
    // heap buffer — the source `v`/`s` is freed once by its own binding).
    // Asserts both the value AND that the source stays usable afterward
    // (a premature/duplicate free would corrupt the post-use read or
    // crash). Covers Vec (cap>0 heap) and String (push_str-grown heap).
    let out = run_program(
        "fn pickv(v: ref Vec[i64]) -> ref Vec[i64] { v }\n\
             fn firstv(v: ref Vec[i64]) -> i64 { v[0] }\n\
             fn picks(s: ref String) -> ref String { s }\n\
             fn lens(s: ref String) -> i64 { s.len() }\n\
             fn main() {\n\
             \x20   let mut v: Vec[i64] = Vec.new();\n\
             \x20   v.push(10); v.push(20);\n\
             \x20   println(firstv(pickv(v)));\n\
             \x20   println(v[1]);\n\
             \x20   let mut s: String = \"\";\n\
             \x20   s.push_str(\"hello\");\n\
             \x20   println(lens(picks(s)));\n\
             \x20   println(s);\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "10\n20\n5\nhello");
    }
}

#[test]
fn test_e2e_borrow_return_borrowed_struct() {
    // Borrowed-struct return (design.md Feature 4 Part 3, B-2026-06-07-5):
    // `-> ref Parser` returns the struct BY VALUE with its `ref` field
    // holding a forwarded borrow of the caller's `s`. This also exercises
    // the construction path that was previously never codegenned (the
    // `asan_borrowed_struct_construction` vacuum). Reads go through the
    // returned value for the OWNED field (`position`) AND the BORROWED
    // field (`source`): a `ref`-typed field access deref's-on-use through
    // the stored borrow pointer in a value position (`println(p.source)`),
    // and read-only `.len()`/`.is_empty()` on it route through the value
    // receiver. (Non-`len` methods on a borrowed field remain a follow-on.)
    let out = run_program(
        "struct Parser { source: ref String, position: i64 }\n\
             fn make_parser(s: ref String) -> ref Parser {\n\
             \x20   Parser { source: s, position: 7 }\n\
             }\n\
             fn main() {\n\
             \x20   let s = String.from(\"input data\");\n\
             \x20   let p = make_parser(s);\n\
             \x20   println(p.position);\n\
             \x20   println(p.source);\n\
             \x20   println(p.source.len());\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "7\ninput data\n10");
    }
}

#[test]
fn test_e2e_raw_ptr_deref_reads_byte() {
    // B-2026-06-11-3: `unsafe { *p }` on a `*const T` / `*mut T` used to
    // yield the pointer value (the address) instead of loading through it.
    // The deref arm now emits a real `load` of the pointee for raw-pointer
    // operands (those the lowering pass flags in `raw_pointer_pointee_types`),
    // while `ref T` / `mut ref T` keep the load_variable pass-through.
    // Reads a byte back through both an `Array.as_ptr` and a `CStr.as_ptr`.
    let src = r#"
fn main() {
    let a: Array[u8, 3] = [65u8, 66u8, 67u8];
    let pa = a.as_ptr();
    // Safety: `pa` addresses element 0 of the live owned array.
    let ba: u8 = unsafe { *pa };
    println(ba);
    let s = c"hi";
    let ps = s.as_ptr();
    // Safety: `ps` addresses the first byte of the live c-string literal.
    let bs: u8 = unsafe { *ps };
    println(bs);
}
"#;
    let out = run_program(src);
    if let Some(out) = out {
        // a[0]=65, "hi"[0]='h'(104) — values, not addresses.
        assert_eq!(out, "65\n104\n");
    }
}

#[test]
fn e2e_fn_value_ref_param_takes_a_fresh_rvalue_argument() {
    // The rvalue leg of the by-pointer branch: no named binding whose
    // address to take, so the argument is materialized into a temp. This is
    // the ownership-sensitive path a wrong marshalling would leak or
    // double-free. The payload's BYTES are read (`contains`) and the seed is
    // opaque (`env.args().len()`), so the allocations are not dead and the
    // loop really runs — B-2026-08-04-17's authoring rule.
    let out = run_program(
        "fn score(s: ref String, needle: ref String) -> i64 {\n\
                 if s.contains(needle) { return s.len(); }\n\
                 return 1i64;\n\
             }\n\
             fn mk(k: i64) -> String {\n\
                 let mut s: String = String.new();\n\
                 s.push_str(f\"payload-row-{k}-tail\");\n\
                 return s;\n\
             }\n\
             fn main() {\n\
                 let a: i64 = env.args().len();\n\
                 let f = score;\n\
                 let mut acc = 0i64;\n\
                 let mut i = 0i64;\n\
                 while i < 20i64 {\n\
                     let nd: String = mk(i + a);\n\
                     acc = acc + f(mk(i + a), nd);\n\
                     i = i + 1i64;\n\
                 }\n\
                 println(acc);\n\
             }",
    );
    // Oracle: the interpreter's answer for the same source.
    assert_eq!(out, Some("371\n".to_string()));
}

/// B-2026-09-14-30, the `Drop`-BODY half of the moved-in-source disarm.
///
/// The memory half is `asan_array_index_store_disarms_the_moved_in_source`
/// in `tests/memory_sanitizer.rs`. This asserts the OTHER thing the second
/// arm does: `suppress_user_drop_for_var` retracts the moved-from source's
/// own `Drop` registration, so `dD3` runs ONCE — from the array element
/// that now owns the value — rather than once there and once for `b`.
///
/// A KNOWN GAP IS PINNED HERE AS MEASURED RATHER THAN FIXED: the DISPLACED
/// element's body (`dD1`) runs on `--interp` and on NO compiled backend.
/// That is B-2026-09-14-29, whose own filing left "does the interpreter run
/// `dD1`?" unmeasured — it does, so that row is a run-vs-build divergence
/// as well as a leak. Asserting the compiled output as it stands means a
/// change either way is noticed here.
///
/// The displaced elements are RODATA strings so this program stays memory
/// clean (17 allocs / 17 frees) while B-2026-09-14-29 is still open; the
/// moved-in `b` owns a live buffer, which is what the disarm is about.
/// B-2026-09-15-4 — a method returning `ref (T, U)` used in a VALUE
/// position.
///
/// The callee lowers to the `ptr` borrow ABI and the consumer read that
/// pointer AS the tuple: SIGSEGV for a `Map[(String, String), i64]` key,
/// `p0:0` for an inline `.0`, and `missing` for a scalar-tuple key that had
/// just been inserted. `--interp` was correct in all three, so this was a
/// run-vs-build divergence as well as a miscompile.
///
/// WHAT MAKES THIS THE METHOD ARM OF AN EXISTING RULE RATHER THAN A NEW
/// ONE: `free:` below is the FREE-FUNCTION spelling of `proj:`, and it was
/// correct before the fix — `compile_call` has loaded the pointee for a
/// borrow-returning free-fn call since B-2026-06-07-5. The method path had
/// a guard that was supposed to REFUSE this shape instead, and the guard
/// had silently stopped firing: it keyed its lookup on the RECEIVER's span
/// on the premise that the parser set `MethodCall.span == receiver.span`,
/// and B-2026-08-18-24 removed that premise. A guard whose key stops
/// matching does not fail loudly; it lets the value through.
///
/// `bound:` is the control that localized it — the SAME method is correct
/// when its result is bound as a `ref`, because the `let` arm sets
/// `compiling_ref_return_let_rhs` first, so the callee was never the
/// problem. `mapscalar:` is the cheapest cell: no heap, no crash, just a
/// lookup that answered `missing` for a key that is present, which is the
/// shape most likely to reach a user as a silently wrong program.
#[test]
fn test_e2e_ref_tuple_return_used_in_a_value_position() {
    assert_eq!(
        run_program(
            "struct Hold { pr: (String, String) }\n\
                 struct Sc { pr: (i64, i64) }\n\
                 impl Hold { fn peek(ref self) -> ref (String, String) { return self.pr; } }\n\
                 impl Sc { fn peek(ref self) -> ref (i64, i64) { return self.pr; } }\n\
                 fn peekf(h: ref Hold) -> ref (String, String) { return h.pr; }\n\
                 fn main() {\n\
                 \x20   let h = Hold { pr: (f\"left-{40 + 2}\", f\"right-{7 * 6}\") };\n\
                 \x20   println(f\"proj:{h.peek().0}\");\n\
                 \x20   let p: ref (String, String) = h.peek();\n\
                 \x20   println(f\"bound:{p.1}\");\n\
                 \x20   println(f\"free:{peekf(h).0}\");\n\
                 \x20   let mut m: Map[(String, String), i64] = Map.new();\n\
                 \x20   m.insert((f\"left-{40 + 2}\", f\"right-{7 * 6}\"), 9);\n\
                 \x20   match m.get(h.peek()) {\n\
                 \x20       Some(v) => { println(f\"mapheap:{v}\"); }\n\
                 \x20       None => { println(\"mapheap:missing\"); }\n\
                 \x20   }\n\
                 \x20   let s = Sc { pr: (11, 12) };\n\
                 \x20   let mut sm: Map[(i64, i64), i64] = Map.new();\n\
                 \x20   sm.insert((11, 12), 5);\n\
                 \x20   match sm.get(s.peek()) {\n\
                 \x20       Some(v) => { println(f\"mapscalar:{v}\"); }\n\
                 \x20       None => { println(\"mapscalar:missing\"); }\n\
                 \x20   }\n\
                 \x20   println(f\"scalarproj:{s.peek().1}\");\n\
                 }"
        )
        .as_deref(),
        Some(
            "proj:left-42\nbound:right-42\nfree:left-42\nmapheap:9\nmapscalar:5\n\
                 scalarproj:12\n"
        )
    );
}

// Cross-function SoA read: a SoA-laid-out Vec passed BY REF into a helper
// function, read there via `.len()`, whole-element index (`let e = es[i]`),
// and direct indexed field access (`es[i].y`). Before the fix, a `ref` SoA
// param's slot holds a POINTER to the caller's SoA struct, but the SoA
// access paths (compile_soa_index_read / compile_soa_method) GEP'd the slot
// alloca directly — reading the pointer's bytes as group-ptrs/len → a garbage
// `len` and a SIGTRAP. The fix derefs the ref-param slot once before GEPing,
// so the callee operates on the caller's existing SoA layout. Surfaced by the
// Slipstream LBM dogfood (its kernel splits the grid across `ref Vec[LbmNode]`
// helpers). NOTE: by-value SoA params and SoA return values remain a separate
// (ABI-level) follow-on — this covers the by-ref read path only.
#[test]
fn test_e2e_soa_by_ref_param_read_across_function() {
    let out = run_program(
        r#"
struct E { x: f64, y: f64 }
layout es: Vec[E] { group g1 { x } group g2 { y } }
fn sumall(es: ref Vec[E]) -> f64 {
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
    es.push(E { x: 5.0, y: 6.0 });
    println(sumall(es));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out.trim(),
            "21",
            "SoA Vec read by ref across a function boundary"
        );
    }
}

#[test]
fn test_e2e_soa_by_ref_param_caller_different_name() {
    // Per-layout monomorphization slice 4 (multi-buffer / differing-name,
    // borrow form): a SoA-laid-out `Vec[E]` is read through a shared helper
    // by `ref Vec[E]` whose param name (`data`) does NOT match the layout
    // block (`grid`). The name-keyed by-ref-reads path can only lower a ref
    // param SoA when the param name itself has a `layout` block, so without
    // slice 4 `data` lowers AoS and reads the caller's SoA struct pointer as
    // `{ptr,len,cap}` — garbage len → SIGTRAP / wrong output. Slice 4 makes
    // a `ref Vec[E]` param layout-carrying: forward inference reads the
    // argument's buffer layout and monomorphizes `sumall$data_soa_grid`,
    // whose body derefs the pointer once (`ref_params`) and lowers SoA — the
    // borrow analog of slice 2's by-value differently-named param.
    let out = run_program(
        r#"
struct E { x: f64, y: f64 }
layout grid: Vec[E] { group g1 { x } group g2 { y } }
fn sumall(data: ref Vec[E]) -> f64 {
    let mut s = 0.0;
    let mut i = 0;
    while i < data.len() {
        let e = data[i];
        s = s + e.x + data[i].y;
        i = i + 1;
    }
    s
}
fn main() {
    let mut grid: Vec[E] = Vec.new();
    grid.push(E { x: 1.0, y: 2.0 });
    grid.push(E { x: 3.0, y: 4.0 });
    grid.push(E { x: 5.0, y: 6.0 });
    println(sumall(grid));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out.trim(),
            "21",
            "SoA Vec read by ref into a differently-named param (layout-mono borrow form)"
        );
    }
}

#[test]
fn test_e2e_soa_two_buffers_through_one_ref_helper_distinct_monos() {
    // Per-layout monomorphization slice 4 (multi-buffer distinctness, by
    // ref): two distinct `layout` blocks (`grid`, `coll`) over one element
    // type flow through ONE by-ref helper `total`. Each call monomorphizes a
    // distinct symbol (`total$data_soa_grid`, `total$data_soa_coll`) keyed
    // on the caller's buffer layout — the borrow analog of the by-value
    // two-layouts-through-one-helper test. Both must read correctly through
    // their OWN grouping: grid (1+2)+(3+4)=10, coll 10+20=30. A single
    // shared body could not read both groupings — distinct correct results
    // are the proof of distinct monomorphs.
    let out = run_program(
        r#"
struct E { x: f64, y: f64 }
layout grid: Vec[E] { group g1 { x } group g2 { y } }
layout coll: Vec[E] { group c1 { x } group c2 { y } }
fn total(data: ref Vec[E]) -> f64 {
    let mut s = 0.0;
    let mut i = 0;
    while i < data.len() {
        s = s + data[i].x + data[i].y;
        i = i + 1;
    }
    s
}
fn main() {
    let mut grid: Vec[E] = Vec.new();
    grid.push(E { x: 1.0, y: 2.0 });
    grid.push(E { x: 3.0, y: 4.0 });
    let mut coll: Vec[E] = Vec.new();
    coll.push(E { x: 10.0, y: 20.0 });
    println(total(grid));
    println(total(coll));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out.trim(),
            "10\n30",
            "two SoA buffers through one by-ref helper produce distinct correct monos"
        );
    }
}

/// B-2026-07-12-1: passing a struct FIELD (`self.names`, a `Vec[String]`)
/// BY REF to a FREE function double-freed the field's backing Vec under AOT
/// codegen (`free(): double free detected`) while the interpreter was
/// correct — a silent run/build divergence. The `ref`-arg path had a
/// fast-path for a local Identifier (pass the slot pointer) but NONE for a
/// FieldAccess place, so `self.names` fell through to the rvalue path, which
/// shallow-copied the field's `{ptr,len,cap}` header into a temp and queued a
/// scope-exit FREE of its buffer — double-freeing what the receiver's own
/// field-drop still owns. Now a FieldAccess place borrows the field in place
/// (a GEP off the receiver), so only the owner frees it. Covers the read
/// (`ref`) shape over String elements AND a `mut ref` field whose mutation
/// must propagate back through the borrow. Memory safety is pinned in
/// `tests/memory_sanitizer.rs::asan_struct_field_by_ref_to_free_fn_no_double_free`.
#[test]
fn e2e_struct_field_by_ref_to_free_fn() {
    if let Some(out) = run_program(
        "fn scan(names: ref Vec[String], name: ref String) -> i64 {\n\
             \x20   let mut i = 0;\n\
             \x20   loop {\n\
             \x20       if i >= names.len() { return -1; }\n\
             \x20       if names[i] == name { return i; }\n\
             \x20       i = i + 1;\n\
             \x20   }\n\
             }\n\
             fn addall(v: mut ref Vec[i64], n: i64) {\n\
             \x20   let mut i = 0;\n\
             \x20   loop { if i >= v.len() { return; } v[i] = v[i] + n; i = i + 1; }\n\
             }\n\
             struct T { names: Vec[String], xs: Vec[i64] }\n\
             impl T {\n\
             \x20   fn find(ref self, q: ref String) -> i64 { scan(self.names, q) }\n\
             \x20   fn bump(mut ref self, n: i64) { addall(mut self.xs, n) }\n\
             }\n\
             fn main() {\n\
             \x20   let mut t = T { names: Vec.new(), xs: Vec.new() };\n\
             \x20   t.names.push(\"a\".to_string());\n\
             \x20   t.names.push(\"b\".to_string());\n\
             \x20   t.xs.push(10); t.xs.push(20);\n\
             \x20   let q = \"b\".to_string();\n\
             \x20   println(t.find(q).to_string());\n\
             \x20   t.bump(5);\n\
             \x20   println(t.xs[0].to_string());\n\
             \x20   println(t.xs[1].to_string());\n\
             }",
    ) {
        assert_eq!(out, "1\n15\n25\n");
    }
}

#[test]
fn test_ir_borrowed_param_walk_is_count_free() {
    // Phase C2a: kata #2\'s adder body emits ZERO rc_inc anywhere —
    // the borrowed-family walk cursors skip the alias-acquire and
    // advance counts (previously the dominant count traffic), the
    // b2 literal build was already count-free, and option params
    // carry no entry inc. The params\' balanced exit RcDecOption
    // stays (the caller\'s arg-site inc transfers ownership per
    // call) — that residual pair is C2b\'s to remove under the
    // headerless purity gate.
    let ir = ir_for_with_ownership(
        r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
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
fn main() {
    let x = ListNode { val: 7, next: None };
    let y = ListNode { val: 5, next: None };
    let r = add_two_numbers(Some(x), Some(y));
    if r.is_some() { println(r.unwrap().val); }
}
"#,
    );
    let body = function_body(&ir, "add_two_numbers").expect("fn body");
    assert!(
        !body.contains("rc_inc") && !body.contains("opt.alias.inc"),
        "borrowed walk must emit no incs; body:\n{body}"
    );
    assert!(
        body.contains("opt_rc_cleanup"),
        "params keep the balanced exit dec; body:\n{body}"
    );
    assert!(
        body.contains("elide_free") && !body.contains("cw_loop"),
        "the literal cluster\'s RootLink transfer is untouched; body:\n{body}"
    );
}

#[test]
fn test_e2e_borrowed_param_walks_reuse_chains() {
    // Phase C2a end-to-end: the same two chains are walked by a
    // borrowing adder 100 times — an unbalanced borrow either
    // frees a chain mid-loop (wrong sum / UAF) or leaks per call.
    let out = run_program_with_ownership(
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
    let l1 = from_three(2, 4, 3);
    let l2 = from_three(5, 6, 4);
    let mut total = 0;
    let mut iter = 0;
    while iter < 100 {
        let r = add_two_numbers(l1, l2);
        total = total + sum_chain(r);
        iter = iter + 1;
    }
    total = total + sum_chain(l1) + sum_chain(l2);
    println(total);
}
"#,
    );
    // 100 * 15 + (2+4+3) + (5+6+4) = 1524.
    assert_eq!(out.as_deref(), Some("1524\n"));
}

// ── Bounds-check elision via dominating loop guard ────────────────
//
// The same indexing pattern `Vec.get_unchecked` skips at runtime, but
// through safe `v[i]` reads when the loop guard already proves
// `0 <= i < v.len()`. Output correctness is the regression gate;
// perf is validated separately on the kata bench (`wip-kata5-perf.md`).
// The end-to-end shape these tests pin: build the vec, walk it under
// a guard that asserts both halves of the bound, and confirm the
// expected element values flow through. If the elision pass were
// unsound (e.g. mis-classified a fact, skipped a real-check), one of
// these would either crash or produce wrong output.

#[test]
fn test_e2e_bounds_elision_while_guard_proves_both() {
    // `while i >= 0 and i < n` asserts both halves; v[i] inside skips
    // both bounds checks. Sum across all elements proves we read
    // every cell correctly.
    let out = run_program(
        r#"
fn sum_all(v: ref Vec[i64]) -> i64 {
    let n = v.len();
    let mut i = 0i64;
    let mut acc = 0i64;
    while i >= 0 and i < n {
        acc = acc + v[i];
        i = i + 1;
    }
    acc
}
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1);
    v.push(2);
    v.push(3);
    v.push(4);
    v.push(5);
    println(sum_all(v));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "15");
    }
}

#[test]
fn test_e2e_bounds_elision_two_pointer_in_guard() {
    // Kata #5's exact pattern: `lo >= 0 and hi < n and v[lo] == v[hi]`.
    // The indexing happens INSIDE the guard's short-circuit `and`,
    // so the asserted bounds must propagate through the short-circuit
    // RHS evaluation, not just into the loop body. Returns the size
    // of the longest palindrome seeded at the middle of the input.
    let out = run_program(
        r#"
fn expand(chars: ref Vec[i64], lo0: i64, hi0: i64) -> i64 {
    let mut lo = lo0;
    let mut hi = hi0;
    let n = chars.len();
    while lo >= 0 and hi < n and chars[lo] == chars[hi] {
        lo = lo - 1;
        hi = hi + 1;
    }
    hi - lo - 1
}
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1);
    v.push(2);
    v.push(3);
    v.push(2);
    v.push(1);
    println(expand(v, 2, 2));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "5");
    }
}

#[test]
fn test_e2e_bounds_elision_partial_proof_still_safe() {
    // Only the lower bound is proven (`i >= 0`). The upper-half
    // bounds check must still fire — without it, the off-by-one
    // indexing past `n - 1` would either UB or pull garbage. The
    // assert here is that the program panics (specifically: the
    // upper bounds check fires) rather than producing wrong output.
    // The runner returns None when the compiled binary exits non-zero;
    // we just check that it doesn't silently succeed.
    let out = run_program(
        r#"
fn check_within(v: ref Vec[i64]) -> i64 {
    let n = v.len();
    let mut i = 0i64;
    let mut acc = 0i64;
    while i >= 0 and i < n + 1 {
        acc = acc + v[i];
        i = i + 1;
    }
    acc
}
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(10);
    v.push(20);
    println(check_within(v));
}
"#,
    );
    // Should NOT silently succeed with a value — must either panic
    // (out is None / not the legitimate sum) or be obviously wrong.
    // The legitimate sum would be 30; if elision were unsound, that
    // could be the output. Assert it's not.
    if let Some(out) = out {
        assert_ne!(
            out.trim(),
            "30",
            "elision must NOT skip upper bound when only lower is proven"
        );
    }
}

#[test]
fn test_e2e_bounds_elision_rolling_dp_length_pin_correct() {
    // Rolling-DP length pin (bce_length_pin.rs, kata #62): `dp` is filled to
    // exactly `cols` elements by a counted `while j < cols` from 0, then the
    // inner scan `dp[c] = dp[c] + dp[c - 1]` runs under `while c < cols`.
    // The guard bound `cols` is a plain variable, NOT `dp.len()`, so only the
    // length pin lets the upper-half checks on `dp[c]` and `dp[c - 1]` elide.
    // `unique_paths(3, 7) == 28` — the correct answer proves every rolling
    // read/write landed on the right cell (a wrong elision would read past
    // the buffer and corrupt the fold).
    let out = run_program(
        r#"
fn unique_paths(m: i64, n: i64) -> i64 {
    let mut rows = m;
    let mut cols = n;
    if cols > rows { let t = rows; rows = cols; cols = t; }
    let mut dp: Vec[i64] = Vec.new();
    let mut j = 0i64;
    while j < cols { dp.push(1i64); j = j + 1i64; }
    let mut i = 1i64;
    while i < rows {
        let mut c = 1i64;
        while c < cols { dp[c] = dp[c] + dp[c - 1i64]; c = c + 1i64; }
        i = i + 1i64;
    }
    dp[cols - 1i64]
}
fn main() { println(unique_paths(3i64, 7i64)); }
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "28");
    }
}

#[test]
fn test_e2e_bounds_elision_length_pin_bound_reassigned_still_checks() {
    // The length pin must NOT fire when the bound variable is reassigned
    // after the fill: here `cols` grows past `dp.len()`, so a later
    // `dp[c]` with `c < cols` is genuinely out of bounds. The whole-function
    // invariance scan refuses the pin, the bounds check survives, and the
    // program panics rather than reading past the buffer.
    if let Some(c) = run_program_capturing(
        r#"
fn go(n: i64) -> i64 {
    let mut cols = n;
    let mut dp: Vec[i64] = Vec.new();
    let mut j = 0i64;
    while j < cols { dp.push(1i64); j = j + 1i64; }
    cols = cols + 5i64;
    let mut acc = 0i64;
    let mut c = 0i64;
    while c < cols { acc = acc + dp[c]; c = c + 1i64; }
    acc
}
fn main() { println(go(4i64)); }
"#,
    ) {
        assert!(
            !c.status.success(),
            "reassigned bound must keep the bounds check (panic), got stdout={:?}",
            c.stdout
        );
    }
}

#[test]
fn test_e2e_bounds_elision_length_pin_nonbare_bound() {
    // Follow-up (3): a `cols + 1` arithmetic bound, filled and indexed under
    // the identical expression, pins by normalised structural match. 1-indexed
    // rolling DP; dp has cols+1 cells, dp[cols] is the answer for 3x7 → 28.
    let out = run_program(
        r#"
fn build(rows: i64, cols: i64) -> i64 {
    let mut dp: Vec[i64] = Vec.new();
    let mut j = 0i64;
    while j < cols + 1i64 { dp.push(1i64); j = j + 1i64; }
    let mut r = 1i64;
    while r < rows {
        let mut c = 1i64;
        while c < cols + 1i64 { dp[c] = dp[c] + dp[c - 1i64]; c = c + 1i64; }
        r = r + 1i64;
    }
    dp[cols]
}
fn main() { println(build(7i64, 2i64)); }
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "28");
    }
}

#[test]
fn test_e2e_bounds_elision_length_pin_nested_block_fill() {
    // Nested-block fill: the DP buffer is built fresh inside an outer loop's
    // body (per-iteration rebuild). The pin is recognised there and elides
    // the inner scan; correctness proves every rolling read landed right.
    // 3 iterations × unique_paths(3,7)=28 → 84.
    let out = run_program(
        r#"
fn main() {
    let mut acc = 0i64;
    let mut k = 0i64;
    while k < 3i64 {
        let mut dp: Vec[i64] = Vec.new();
        let mut j = 0i64;
        while j < 7i64 { dp.push(1i64); j = j + 1i64; }
        let mut i = 1i64;
        while i < 3i64 {
            let mut c = 1i64;
            while c < 7i64 { dp[c] = dp[c] + dp[c - 1i64]; c = c + 1i64; }
            i = i + 1i64;
        }
        acc = acc + dp[6i64];
        k = k + 1i64;
    }
    println(acc);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "84");
    }
}

#[test]
fn test_e2e_bounds_elision_length_pin_nested_shadow_bound_still_checks() {
    // Soundness: a nested block re-binds the BOUND var `n` larger; `dp[c]`
    // under `while c < n` would read past `dp.len()`. The pin must NOT fire
    // (bound-ident rebind detected) — the program panics instead of silently
    // reading garbage. (Regression for the silent-wrong-answer variant.)
    if let Some(c) = run_program_capturing(
        r#"
fn go(n: i64) -> i64 {
    let mut dp: Vec[i64] = Vec.new();
    let mut j = 0i64;
    while j < n { dp.push(1i64); j = j + 1i64; }
    let mut acc = 0i64;
    let mut once = 1i64;
    while once > 0i64 {
        let n = 20i64;
        let mut c = 0i64;
        while c < n { acc = acc + dp[c]; c = c + 1i64; }
        once = 0i64;
    }
    acc
}
fn main() { println(go(3i64)); }
"#,
    ) {
        assert!(
            !c.status.success(),
            "nested shadow of the bound var must keep the check (panic), got stdout={:?}",
            c.stdout
        );
    }
}

// ── Prefix dereference operator ───────────────────────────────────────────

#[test]
fn test_e2e_deref_read_ref_param() {
    // *r where r: ref i64 should load the pointed-to value.
    let out = run_program(
        r#"
fn read_val(r: ref i64) -> i64 { *r }
fn main() {
    let x: i64 = 42;
    println(read_val(x));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "42");
    }
}

#[test]
fn test_e2e_deref_write_through_mut_ref() {
    // *r = v where r: mut ref i64 should store through the pointer.
    let out = run_program(
        r#"
fn set_val(r: mut ref i64) { *r = 99; }
fn main() {
    let mut x: i64 = 1;
    set_val(mut x);
    println(x);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "99");
    }
}

#[test]
fn test_e2e_deref_double_in_place() {
    // *r = *r * 2 — read and write through a mut ref in the same statement.
    let out = run_program(
        r#"
fn double_in_place(r: mut ref i64) { *r = *r * 2; }
fn main() {
    let mut n: i64 = 5;
    double_in_place(mut n);
    println(n);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "10");
    }
}

// ── Ref-self / mut-ref-self method codegen ───────────────────────────
//
// Prerequisite for Theme 6's `R.method(...)` dispatch: impl methods
// declared with `ref self` / `mut ref self` must compile to functions
// that take a pointer-to-Self as the receiver, and the call site must
// pass the receiver's address rather than its loaded value. Before
// this slice, `make_impl_method_function` rewrote every `ref self` /
// `mut ref self` to value-typed `self`, so mutations through the
// receiver were lost on a copy.

#[test]
fn test_mut_ref_self_method_mutation_persists_through_caller() {
    let captured = run_program_capturing(
        "struct Counter { n: i64 }\n\
             impl Counter { fn bump(mut ref self) { self.n = self.n + 1; } }\n\
             fn main() { let mut c = Counter { n: 42 }; c.bump(); c.bump(); println(c.n); }",
    );
    if let Some(c) = captured {
        assert!(
            c.stdout.lines().any(|l| l.trim() == "44"),
            "expected 44 (42 + 1 + 1), got: {:?}",
            c.stdout
        );
    }
}

#[test]
fn test_ref_self_method_reads_through_pointer() {
    let captured = run_program_capturing(
        "struct Pair { x: i64, y: i64 }\n\
             impl Pair { fn read_y(ref self) -> i64 { self.y } }\n\
             fn main() { let p = Pair { x: 7, y: 99 }; println(p.read_y()); }",
    );
    if let Some(c) = captured {
        assert!(
            c.stdout.lines().any(|l| l.trim() == "99"),
            "expected 99 (Pair.y), got: {:?}",
            c.stdout
        );
    }
}

#[test]
fn test_mut_ref_free_function_param_mutation_persists() {
    // Cross-check that the same fix path applies to non-method
    // mut-ref params on free functions — the call site decides
    // the calling convention by inspecting the resolved fn's
    // first param type.
    let captured = run_program_capturing(
        "struct Counter { n: i64 }\n\
             fn bump(c: mut ref Counter) { c.n = c.n + 1; }\n\
             fn main() { let mut c = Counter { n: 42 }; bump(mut c); bump(mut c); println(c.n); }",
    );
    if let Some(c) = captured {
        assert!(
            c.stdout.lines().any(|l| l.trim() == "44"),
            "expected 44 (42 + 1 + 1), got: {:?}",
            c.stdout
        );
    }
}

#[test]
fn test_with_provider_e2e_mut_ref_self_mutation_visible_after_pop() {
    // Full Theme 6 round-trip: push → R.method() → pop, where the
    // method writes through `mut ref self` to the provider's storage.
    // After the with_provider scope ends, the provider variable
    // reflects the mutation — proving the data pointer that flowed
    // through karac_provider_push survived round-trip back into
    // karac_provider_lookup and the indirect call wrote through it.
    let src = "pub trait Recorder { fn record(mut ref self, value: i64); }\n\
            pub struct Counter { n: i64 }\n\
            impl Recorder for Counter { fn record(mut ref self, value: i64) { self.n = value; } }\n\
            pub effect resource Metric: Recorder;\n\
            fn main() {\n\
              let mut p = Counter { n: 0 };\n\
              with_provider[Metric](p, || { Metric.record(99); });\n\
              println(p.n);\n\
            }";
    let Some(out) = run_program(src) else {
        eprintln!("skipping with_provider e2e: runtime/linker unavailable");
        return;
    };
    assert_eq!(
        out.trim(),
        "99",
        "expected p.n == 99 after with_provider mutated through mut ref self"
    );
}

#[test]
fn test_ir_ref_param_with_arithmetic_rvalue() {
    let ir = ir_for(
        "fn use_it(n: ref i32) -> i32 { *n }\n\
             fn app_main() -> i32 { use_it(7i32 + 35i32) }",
    );
    // The arithmetic result is the rvalue; the materialization
    // pattern is the same as for a bare literal.
    assert!(
        ir.contains("%ref_rvalue_arg0"),
        "arithmetic-result rvalue should be materialized:\n{ir}"
    );
    assert!(
        ir.contains("call i32 @use_it(ptr %ref_rvalue_arg0)"),
        "call should pass the temp pointer:\n{ir}"
    );
}

#[test]
fn test_ir_ref_param_with_identifier_keeps_fast_path() {
    // The identifier fast-path should still pass `get_data_ptr`
    // directly without minting a new entry alloca named
    // `ref_rvalue_argN`. Regression guard for the order of the
    // two `if is_ref` branches inside `compile_call`.
    let ir = ir_for(
        "fn take(n: ref i32) -> i32 { *n }\n\
             fn app_main() -> i32 { let x: i32 = 99i32; take(x) }",
    );
    assert!(
        !ir.contains("%ref_rvalue_arg0"),
        "identifier arg should not be re-materialized:\n{ir}"
    );
}

#[test]
fn test_ir_ref_param_scalar_rvalue_skips_cleanup() {
    // Counterpart of the Vec/String case: a scalar (i32) at a
    // `ref T` position must not be registered with
    // `track_vec_var` — its LLVM type doesn't match the
    // `{ptr,i64,i64}` shape, so the dispatch in `compile_call`
    // skips the registration. Regression guard for the
    // `llvm_ty_is_vec_struct` predicate that gates the
    // registration call.
    let ir = ir_for(
        "fn take(n: ref i32) -> i32 { *n }\n\
             fn app_main() -> i32 { take(42i32) }",
    );
    // A scalar temp's alloca prints as `alloca i32`; if the
    // codegen mistakenly routed it through track_vec_var, the
    // scope-exit walker would emit a `getelementptr inbounds
    // { ptr, i64, i64 }, ptr %ref_rvalue_arg0` against the i32
    // alloca, which would be a type mismatch the verifier would
    // reject. Easiest assertion: confirm the alloca is `i32`,
    // not `{ ptr, i64, i64 }`.
    assert!(
        ir.contains("%ref_rvalue_arg0 = alloca i32"),
        "i32 rvalue should alloca as i32, not as vec-struct:\n{ir}"
    );
}

#[test]
fn test_e2e_field_receiver_method_through_ref_param() {
    // `t.title.clone()` where `t: ref Todo` — codegen's
    // field-receiver method dispatch in `calls.rs` was using
    // `slot.ptr` directly as the GEP base for plain (non-shared)
    // structs. For `ref T` parameters, `slot.ptr` is an alloca
    // holding a pointer to the struct, not the struct itself, so
    // the GEP read past the slot's 8-byte pointer into junk. The
    // resulting bad String triple was then passed to
    // `karac_clone_String`, which dereferenced it and segfaulted
    // silently. Fix dereferences the ref-param slot before the
    // GEP, mirroring the shared-struct handle pattern.
    //
    // Surfaced by the backend TODO API kata Slice 4 helper
    // `todo_to_json(t: ref Todo) -> Json` that embeds
    // `Json.String(t.title.clone())` in a returned Json.Object.
    let output = run_program(
        "struct Todo { id: i64, title: String, completed: bool }\n\
             fn todo_to_json(t: ref Todo) -> Json {\n\
                 let mut fields: Vec[(String, Json)] = Vec.new();\n\
                 fields.push((\"id\", Json.Number(t.id as f64)));\n\
                 fields.push((\"title\", Json.String(t.title.clone())));\n\
                 fields.push((\"completed\", Json.Bool(t.completed)));\n\
                 Json.Object(fields)\n\
             }\n\
             fn main() {\n\
                 let t: Todo = Todo { id: 42, title: \"Learn Kara\", completed: true };\n\
                 let j: Json = todo_to_json(t);\n\
                 println(j.stringify());\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(
        output,
        "{\"id\":42.0,\"title\":\"Learn Kara\",\"completed\":true}\n"
    );
}

#[test]
fn test_e2e_ref_param_field_let_move_all_leaves() {
    // B-2026-07-21-11: `let p = <refparam>.field;` — a whole-field move
    // through a `ref` param bit-copy-aliased the caller's field while the
    // binding got owned tracking (the #16/#19 source suppressions bail on
    // the borrowed root), so binding and caller freed the same heap —
    // double-free abort on all compiled backends; interp correct. Now
    // `clone_ref_chain_field_move_rhs` deep-copies the RHS in place for
    // every heap-bearing leaf: user struct, user enum, String, Vec[i64],
    // Vec[String], and Option[String] (inline payloads; Result stays
    // status quo — no dispatcher deep clone). Caller reuse after every
    // call pins that the caller's fields survive. Sibling LSan test
    // guards the leak/double-free halves.
    let output = run_program(
        "enum Tok { Plus, Ident(String) }\n\
             struct Pt { s: String, x: i64 }\n\
             struct Holder { inner: Pt, tok: Tok, name: String, items: Vec[i64],\n\
                             strs: Vec[String], opt: Option[String], n: i64 }\n\
             fn f_struct(h: ref Holder) -> String {\n\
                 let p = h.inner;\n\
                 return \"l:\".to_string() + p.s + \":\" + p.x.to_string();\n\
             }\n\
             fn f_enum(h: ref Holder) -> String {\n\
                 let t = h.tok;\n\
                 match t {\n\
                     Ident(nm) => { return \"t:\".to_string() + nm; }\n\
                     Plus => { return \"+\".to_string(); }\n\
                 }\n\
                 return \"?\".to_string();\n\
             }\n\
             fn f_str(h: ref Holder) -> String {\n\
                 let s = h.name;\n\
                 return \"n:\".to_string() + s;\n\
             }\n\
             fn f_vec(h: ref Holder) -> i64 {\n\
                 let v = h.items;\n\
                 return v.len() + v[0];\n\
             }\n\
             fn f_vstr(h: ref Holder) -> String {\n\
                 let v = h.strs;\n\
                 return v[0] + v[1];\n\
             }\n\
             fn f_opt(h: ref Holder) -> i64 {\n\
                 let o = h.opt;\n\
                 match o {\n\
                     Some(s) => { return s.len(); }\n\
                     None => { return 0; }\n\
                 }\n\
                 return -1;\n\
             }\n\
             fn main() {\n\
                 let a = Holder {\n\
                     inner: Pt { s: \"ld\".to_string(), x: 6 },\n\
                     tok: Tok.Ident(\"mv\".to_string()),\n\
                     name: \"st\".to_string(),\n\
                     items: [7, 8],\n\
                     strs: [\"ab\", \"cd\"],\n\
                     opt: Some(\"xy\".to_string()),\n\
                     n: 1,\n\
                 };\n\
                 println(f_struct(a));\n\
                 println(f_struct(a));\n\
                 println(f_enum(a));\n\
                 println(f_enum(a));\n\
                 println(f_str(a));\n\
                 println(f_str(a));\n\
                 println(f_vec(a));\n\
                 println(f_vstr(a));\n\
                 println(f_vstr(a));\n\
                 println(f_opt(a));\n\
                 println(f_opt(a));\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(
        output,
        "l:ld:6\nl:ld:6\nt:mv\nt:mv\nn:st\nn:st\n9\nabcd\nabcd\n2\n2\n"
    );
}

#[test]
fn test_e2e_ref_param_tuple_field_consume() {
    // B-2026-07-21-10: the TUPLE-leaf sibling of the ref-chain family —
    // `match <refparam>.pair { (s, x) => <consume s> … }` over a
    // `(String, i64)` field double-freed (binding aliased the caller's
    // element; both freed it). Now `clone_escaping_borrowed_ref_chain_-
    // tuple` deep-clones the tuple (dispatcher per-element clone),
    // registers a tuple StructDrop on the clone, and consuming arms zero
    // the consumed elements' caps in the clone slot — a `_` element stays
    // owned by the clone drop. Covers match, a wildcard-element arm over
    // a two-String tuple (the unbound String freed exactly once by the
    // clone drop), if-let, let-else, and caller reuse.
    let output = run_program(
        "struct Holder { pair: (String, i64), two: (String, String), n: i64 }\n\
             fn render(h: ref Holder) -> String {\n\
                 match h.pair {\n\
                     (s, x) => { return \"t:\".to_string() + s + \":\" + x.to_string(); }\n\
                 }\n\
                 return \"?\".to_string();\n\
             }\n\
             fn left_only(h: ref Holder) -> String {\n\
                 match h.two {\n\
                     (a, _) => { return \"L:\".to_string() + a; }\n\
                 }\n\
                 return \"?\".to_string();\n\
             }\n\
             fn ifl(h: ref Holder) -> String {\n\
                 if let (s, x) = h.pair {\n\
                     return \"i:\".to_string() + s + \":\" + x.to_string();\n\
                 }\n\
                 return \"?\".to_string();\n\
             }\n\
             fn lel(h: ref Holder) -> String {\n\
                 let (s, x) = h.pair else {\n\
                     return \"?\".to_string();\n\
                 }\n\
                 return \"e:\".to_string() + s + \":\" + x.to_string();\n\
             }\n\
             fn main() {\n\
                 let a = Holder { pair: (\"tp\".to_string(), 4),\n\
                                  two: (\"aa\".to_string(), \"bb\".to_string()), n: 1 };\n\
                 println(render(a));\n\
                 println(render(a));\n\
                 println(left_only(a));\n\
                 println(left_only(a));\n\
                 println(ifl(a));\n\
                 println(lel(a));\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "t:tp:4\nt:tp:4\nL:aa\nL:aa\ni:tp:4\ne:tp:4\n");
}

#[test]
fn test_lowercase_ambient_aliases_resolve_and_typecheck_clean() {
    // The lowercase module aliases (`clock`, `rand`, `stdin`, `stdout`,
    // `stderr`, `fs`) must resolve AND typecheck — NOT just survive the
    // `run_program` codegen path, which tolerates resolve/typecheck errors
    // (the codegen-run_program-bypasses-typecheck hazard). Before this
    // slice only `env` was wired; the other six errored "undefined name
    // 'clock'" at resolve, so every lowercase E2E test below passed
    // FALSELY through the bypass. This guards the real frontend surface:
    // resolver `push`, the typechecker lowercase→capitalized alias map
    // (which finds each method's exact return type via the baked `impl`),
    // and that the `Result[String, IoError]` of `fs.read_to_string` flows
    // to the `Ok(s)` binding so `s.len()` dispatches.
    let src = r#"
fn main() with reads(Env) writes(FileSystem) reads(FileSystem) {
    let _t = clock.now();
    let _r = rand.next_u64();
    let _a = env.args();
    stdout.print("a");
    stdout.println("b");
    stdout.flush();
    stderr.print("c");
    stderr.println("d");
    stderr.flush();
    let _w = fs.write("/tmp/karac_lc_alias_tc.txt", "x");
    match fs.read_to_string("/tmp/karac_lc_alias_tc.txt") {
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
        "resolve errors for lowercase ambient aliases: {:?}",
        resolved.errors
    );
    let typed = karac::typecheck(&parsed.program, &resolved);
    assert!(
        typed.errors.is_empty(),
        "typecheck errors for lowercase ambient aliases: {:?}",
        typed.errors
    );
}

#[test]
fn test_e2e_local_var_shadows_lowercase_alias() {
    // A local binding of the same name shadows the module alias:
    // `let clock = Timer { .. }; clock.now()` must dispatch to the user's
    // `Timer::now`, NOT the ambient `Clock`. This is the parity case that
    // surfaced an interpreter/codegen split during the slice — codegen and
    // the typechecker guarded on a same-name local; the interpreter alias
    // map did not, and dispatched to the ambient resource. All three now
    // apply the shadow guard. Codegen E2E witness (the interpreter side is
    // covered in `tests/interpreter.rs`).
    let out = run_program(
        r#"
struct Timer { ticks: i64 }
impl Timer {
    fn now(ref self) -> i64 { self.ticks }
}
fn main() {
    let clock = Timer { ticks: 42 };
    let t = clock.now();
    if t == 42 { println("shadowed-ok"); } else { println("BUG"); }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "shadowed-ok");
    }
}

// B-2026-06-07-3 regression: a borrow-mode pattern bind of a `File`
// must NOT register its own scope-exit `karac_runtime_file_close`.
// The fd is owned by the binding's source (here the `ref whole @`
// by_ref alias keeps drop responsibility on the matched value), so a
// second close drains the same fd twice — a double-close. The fix
// gates the `File` arm's `track_file_var` in `bind_pattern_values`
// on `!pattern_binding_is_borrow`, exactly like the Vec/String
// `track_vec_var` site. We assert against the owned form (plain
// `Ok(f)`), which legitimately DOES register one close, so the test
// also catches the gate over-firing and silently leaking fds.
#[test]
fn test_ir_borrow_mode_file_bind_registers_no_close() {
    let close_count = |src: &str| -> usize {
        let ir = ir_for(src);
        function_body(&ir, "main")
            .unwrap_or_default()
            .matches("karac_runtime_file_close")
            .count()
    };
    // Borrow mode: `ref whole @ Ok(f)` sets `pattern_binding_is_borrow`,
    // so `f` aliases the fd the source owns — no close of its own.
    let borrow = close_count(
        r#"
fn main() with reads(FileSystem) {
    match File.open("/tmp/karac_b070703_borrow") {
        ref whole @ Ok(f) => println("ok"),
        Err(_) => println("err"),
    }
}
"#,
    );
    // Owned mode: plain `Ok(f)` owns the fd, so its scope-exit close
    // IS registered (this is the existing F4b drop behavior).
    let owned = close_count(
        r#"
fn main() with reads(FileSystem) {
    match File.open("/tmp/karac_b070703_owned") {
        Ok(f) => println("ok"),
        Err(_) => println("err"),
    }
}
"#,
    );
    assert_eq!(
        borrow, 0,
        "borrow-mode File bind must not register its own file_close \
             (double-close on the source's fd); got {borrow}"
    );
    assert_eq!(
        owned, 1,
        "owned File bind must still register exactly one scope-exit \
             file_close; got {owned}"
    );
}

#[test]
fn test_e2e_ref_params_of_handle_backed_builtins() {
    // B-2026-07-02-27: a `ref Column[i64]` (and ref Tensor / ref
    // DataFrame) parameter's slot holds a pointer to the CALLER's
    // slot, but `column_ptr_for_var` (and the tensor/dataframe
    // twins) loaded the control pointer with a single deref — the
    // caller's alloca address was read as a control block, so the
    // callee saw stack garbage (empty column / wrong data). Covers:
    // ref param, ref forwarded through a second ref call, `mut ref`
    // push mutating through the borrow, indexing through the ref
    // (the original ledger repro), and an owned-param control.
    let src = r#"
fn colsum(c: ref Column[i64]) -> i64 {
    c.sum()
}
fn colsum2(c: ref Column[i64]) -> i64 {
    colsum(c)
}
fn colown(c: Column[i64]) -> i64 {
    c.sum()
}
fn colpush(c: mut ref Column[i64]) {
    c.push(5);
}
fn fst(c: ref Column[i64], i: i64) -> i64 {
    match c[i] {
        Some(v) => v,
        None => -1,
    }
}
fn tsum(t: ref Tensor[i64, [3]]) -> i64 {
    t.sum()
}
fn dfrows(d: ref DataFrame) -> i64 {
    d.height()
}
fn main() {
    let c: Column[i64] = Column.from_vec([10, 20, 30]);
    println(colsum(c));
    println(colsum2(c));
    println(fst(c, 1));
    let mut cm: Column[i64] = Column.from_vec([1, 2]);
    colpush(mut cm);
    println(cm.sum());
    let t: Tensor[i64, [3]] = Tensor.from([7, 8, 9]);
    println(tsum(t));
    let mut d = DataFrame.new();
    d.insert("a", Column.from_vec([1, 2, 3, 4]));
    println(dfrows(d));
    let c2: Column[i64] = Column.from_vec([100, 200]);
    println(colown(c2));
}
"#;
    let out = run_program(src).expect("program should compile and run");
    assert_eq!(out, "60\n60\n20\n8\n24\n4\n300\n");
}

#[test]
fn test_e2e_plain_alias_param_and_return() {
    // B-2026-07-30-7's minimal repro, all three arms: a generic plain
    // alias, its non-generic twin, and a `String` alias that hits the
    // RETURN position too. Each passed `karac check` and `karac run
    // --interp` while failing LLVM module verification under
    // `karac build`, so this is a run-vs-build pin, not a wrong-answer
    // one. Iteration and `+` on the aliased bindings also exercise the
    // side-table registration (`vec_elem_types` / `string_vars`) that
    // resolving the alias to its base feeds.
    if let Some(out) = run_program(
        "type Plain[T] = Vec[T];\n\
             type Ints = Vec[i64];\n\
             type Name = String;\n\
             fn total(xs: Plain[i64]) -> i64 {\n\
                 let mut t = 0;\n\
                 for x in xs { t = t + x; }\n\
                 t\n\
             }\n\
             fn count(xs: Ints) -> i64 { xs.len() }\n\
             fn shout(n: Name) -> Name { n + \"!\" }\n\
             fn main() {\n\
                 println(total(vec![1, 2, 3]));\n\
                 println(count(vec![4, 5]));\n\
                 println(shout(\"hi\"));\n\
             }",
    ) {
        assert_eq!(out, "6\n2\nhi!\n");
    }
}

#[test]
fn test_e2e_plain_alias_struct_field_and_nested_access() {
    // B-2026-07-30-7, the struct-field leg: a field declared with a plain
    // alias records the ALIAS name in `struct_field_type_names`, which
    // resolves to no struct/collection downstream — so `h.pt.x` failed
    // `field_index_for` ("cannot resolve field 'x' on this receiver") and
    // `h.nums.len()` had no element registration. Peeling the alias at
    // `register_struct_metadata` is what makes both walk into the base.
    if let Some(out) = run_program(
        "type Name = String;\n\
             type Ints = Vec[i64];\n\
             type Pt = Point;\n\
             struct Point { x: i64, y: i64 }\n\
             struct Holder { label: Name, nums: Ints, pt: Pt }\n\
             fn dist(p: Pt) -> i64 { p.x + p.y }\n\
             fn main() {\n\
                 let h = Holder { label: \"L\", nums: vec![1, 2], pt: Point { x: 3, y: 4 } };\n\
                 println(h.label);\n\
                 println(h.nums.len());\n\
                 println(h.pt.x);\n\
                 println(dist(h.pt));\n\
             }",
    ) {
        assert_eq!(out, "L\n2\n3\n7\n");
    }
}

#[test]
fn test_e2e_plain_alias_chain_and_cycle_are_bounded() {
    // Two guards in one program. (a) A CHAIN of plain aliases
    // (`type Alias2 = Ints; type Ints = Vec[i64];`) must peel all the way
    // to `Vec[i64]` — the peel recurses. (b) A self-referential alias
    // (`type Loop = Loop;`) is nonsense no upstream phase rejects; before
    // `prune_cyclic_type_aliases` the peel recursion would not terminate.
    // It must fall back to the (wrong but bounded) unknown-name layout
    // rather than overflow the compiler's stack, so the rest of the
    // program still compiles and runs.
    if let Some(out) = run_program(
        "type Ints = Vec[i64];\n\
             type Alias2 = Ints;\n\
             type Loop = Loop;\n\
             fn count(xs: Alias2) -> i64 { xs.len() }\n\
             fn main() { println(count(vec![7, 8, 9])); }",
    ) {
        assert_eq!(out, "3\n");
    }
}

/// The other half of the allowlist: a break value read out of a PLACE the
/// container still owns must keep being refused.
///
/// `break o.inner` hands out a handle with no retain behind it, so
/// admitting it would be a use-after-free rather than the compile error it
/// gets. The predicate is an allowlist of forms that MANUFACTURE ownership
/// for exactly this reason, and this test is what stops a later
/// "simplification" to a not-obviously-borrowed test — which would accept
/// this program.
#[test]
fn test_loop_break_borrowed_field_stays_refused() {
    let src = r#"
shared struct Inner { v: i64 }
struct Outer { inner: Inner }

fn pick(o: Outer) -> Inner {
    let mut i = 0;
    loop {
        i = i + 1;
        if i == 2 { break o.inner }
    }
}

fn main() { let o = Outer { inner: Inner { v: 5 } }; println(pick(o).v); }
"#;
    let mut parsed = karac::parse(src);
    assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
    karac::prepare_for_resolve(&mut parsed.program);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);
    let ownership = karac::ownershipcheck(&parsed.program, &typed);
    let err = compile_to_ir(&parsed.program, Some(&ownership), None)
        .expect_err("a borrowed place must not travel out through the result slot")
        .message;
    assert!(
        err.contains("Module verification failed"),
        "expected the loud refusal, got: {err}"
    );
}

#[test]
fn mut_ref_param_emits_noalias() {
    let ir = ir_for(
        r#"
fn bump(x: mut ref i64) { x = x + 1; }
fn read_only(y: ref i64) -> i64 { return y; }
fn two(a: mut ref i64, b: mut ref i64) { a = a + b; }
fn main() { let mut n: i64 = 41; bump(mut n); print(n); }
"#,
    );
    // `mut ref` → noalias.
    assert!(
        define_line(&ir, "@bump(").contains("noalias"),
        "mut ref param should carry noalias: {}",
        define_line(&ir, "@bump(")
    );
    // `ref i64` (shared read borrow of a Freeze type) → `readonly`, and
    // never `noalias` (shared borrows may alias).
    assert!(
        define_line(&ir, "@read_only(").contains("readonly"),
        "ref-to-Freeze param should carry readonly: {}",
        define_line(&ir, "@read_only(")
    );
    assert!(
        !define_line(&ir, "@read_only(").contains("noalias"),
        "ref param must not carry noalias: {}",
        define_line(&ir, "@read_only(")
    );
    // Every `mut ref` parameter is marked independently.
    assert_eq!(
        define_line(&ir, "@two(").matches("noalias").count(),
        2,
        "both mut ref params should be noalias: {}",
        define_line(&ir, "@two(")
    );
}

#[test]
fn mut_ref_self_receiver_emits_noalias() {
    // The `mut ref self` receiver is desugared into params[0] upstream, so
    // the exclusive-borrow attribute reaches method receivers too.
    let ir = ir_for(
        r#"
struct Counter { n: i64 }
impl Counter {
    fn bump(mut ref self) { self.n = self.n + 1; }
    fn peek(ref self) -> i64 { return self.n; }
}
fn main() {
    let mut c: Counter = Counter { n: 0 };
    c.bump();
    print(c.peek());
}
"#,
    );
    assert!(
        define_line(&ir, "@Counter.bump(").contains("noalias"),
        "mut ref self receiver should carry noalias: {}",
        define_line(&ir, "@Counter.bump(")
    );
    // `ref self` on a Freeze struct (`Counter { n: i64 }`) → `readonly`,
    // never `noalias`.
    assert!(
        define_line(&ir, "@Counter.peek(").contains("readonly"),
        "ref self receiver on a Freeze type should carry readonly: {}",
        define_line(&ir, "@Counter.peek(")
    );
    assert!(
        !define_line(&ir, "@Counter.peek(").contains("noalias"),
        "ref self receiver must not carry noalias: {}",
        define_line(&ir, "@Counter.peek(")
    );
}

#[test]
fn readonly_ref_freeze_predicate() {
    // `ref T` → `readonly` iff `T` is Freeze (no transitive interior
    // mutability). Primitives, all-Freeze structs, and Freeze-element
    // containers qualify; a struct with an `Atomic`/`Mutex` field, or a
    // `shared` type, does not. See `ref_referent_is_freeze`.
    let ir = ir_for(
        r#"
struct Plain { a: i64, b: f64 }
struct HasAtomic { c: Atomic[i64] }
struct HasMutex { m: Mutex[i64] }
shared struct Node { v: i64 }
struct WrapsShared { node: Node }
fn r_scalar(x: ref i64) -> i64 { return x; }
fn r_string(s: ref String) -> i64 { return s.len(); }
fn r_plain(p: ref Plain) -> i64 { return p.a; }
fn r_vec(v: ref Vec[i64]) -> i64 { return v.len(); }
fn r_atomic(h: ref HasAtomic) -> i64 { return 0; }
fn r_mutex(h: ref HasMutex) -> i64 { return 0; }
fn r_shared(w: ref WrapsShared) -> i64 { return 0; }
fn main() { let x: i64 = 7; print(r_scalar(x)); }
"#,
    );
    // Freeze referents → readonly.
    for sym in ["@r_scalar(", "@r_string(", "@r_plain(", "@r_vec("] {
        assert!(
            define_line(&ir, sym).contains("readonly"),
            "ref-to-Freeze `{}` should carry readonly: {}",
            sym,
            define_line(&ir, sym)
        );
    }
    // Interior-mutable / (transitively) shared referents → NOT readonly.
    for sym in ["@r_atomic(", "@r_mutex(", "@r_shared("] {
        assert!(
            !define_line(&ir, sym).contains("readonly"),
            "ref-to-interior-mutable `{}` must not carry readonly: {}",
            sym,
            define_line(&ir, sym)
        );
    }
}

#[test]
fn readonly_ref_freeze_in_monomorph() {
    // The `ref T` → `readonly` arm reaches monomorphized specializations:
    // a bare `ref T` resolved (via `type_subst_names`) to a Freeze type
    // gets `readonly`; resolved to an interior-mutable type it does not.
    let ir = ir_for(
        r#"
struct Plain { a: i64 }
struct HasAtomic { c: Atomic[i64] }
fn rgen[T](x: ref T) -> i64 { return 0; }
fn main() {
    let p: Plain = Plain { a: 1 };
    print(rgen(p));
    let h: HasAtomic = HasAtomic { c: Atomic.new(2) };
    print(rgen(h));
}
"#,
    );
    // The `T = Plain` mono → readonly; the `T = HasAtomic` mono → not.
    assert!(
        define_line(&ir, "rgen$Plain").contains("readonly"),
        "ref T resolved to a Freeze type should carry readonly: {}",
        define_line(&ir, "rgen$Plain")
    );
    assert!(
        !define_line(&ir, "rgen$HasAtomic").contains("readonly"),
        "ref T resolved to an interior-mutable type must not: {}",
        define_line(&ir, "rgen$HasAtomic")
    );
}

#[test]
fn owned_value_ptr_param_emits_noalias() {
    // An OWNED value-semantics type that lowers to a single heap `ptr`
    // (`Tensor` / `Map`) is moved into the callee — the sole live handle to
    // its block — so it carries `noalias`. A reference-semantics handle
    // (`Sender`, refcounted — every clone shares one channel pointer) and
    // non-pointer params (a bare `i64`, an owned by-value struct) do NOT.
    // See `owned_ptr_param_is_noalias_safe` in src/codegen/functions.rs.
    let ir = ir_for(
        r#"
fn take_tensor(t: Tensor[f64, [2, 2]]) -> i64 { return t.rank(); }
fn take_map(m: Map[String, i64]) -> i64 { return m.len(); }
fn take_sender(s: Sender[i64]) -> i64 { return 0; }
fn take_scalar(n: i64) -> i64 { return n; }
struct Point { x: i64, y: i64 }
fn take_struct(p: Point) -> i64 { return p.x; }
fn main() {
    let a: Tensor[f64, [2, 2]] = Tensor.zeros([2, 2]);
    print(take_tensor(a));
}
"#,
    );
    // Owned value-semantics `ptr` types → noalias.
    assert!(
        define_line(&ir, "@take_tensor(").contains("noalias"),
        "owned Tensor param should carry noalias: {}",
        define_line(&ir, "@take_tensor(")
    );
    assert!(
        define_line(&ir, "@take_map(").contains("noalias"),
        "owned Map param should carry noalias: {}",
        define_line(&ir, "@take_map(")
    );
    // Reference-semantics handle (refcounted channel end) → excluded.
    assert!(
        !define_line(&ir, "@take_sender(").contains("noalias"),
        "owned Sender (refcounted) must not carry noalias: {}",
        define_line(&ir, "@take_sender(")
    );
    // Non-pointer params — nothing to annotate.
    assert!(
        !define_line(&ir, "@take_scalar(").contains("noalias"),
        "owned scalar param must not carry noalias: {}",
        define_line(&ir, "@take_scalar(")
    );
    assert!(
        !define_line(&ir, "@take_struct(").contains("noalias"),
        "owned by-value struct param must not carry noalias: {}",
        define_line(&ir, "@take_struct(")
    );
}

#[test]
fn owned_value_ptr_param_noalias_in_monomorph() {
    // The owned-param `noalias` reaches monomorphized specializations too:
    // `declare_mono_function` calls `emit_param_alias_attrs`, and a bare
    // generic param is resolved through `type_subst_names` before the
    // allowlist check. Here `keep[T]` specialized with `T = Tensor[...]`
    // (an owned bare-`T` param) and `tsum[T](Tensor[T, ...])` (an owned
    // `Tensor`-headed param) both emit `noalias` on the moved-in tensor.
    let ir = ir_for(
        r#"
fn tsum[T](t: Tensor[T, [2, 2]]) -> i64 { return t.rank(); }
fn keep[T](x: T) -> T { return x; }
fn main() {
    let a: Tensor[f64, [2, 2]] = Tensor.zeros([2, 2]);
    print(tsum(a));
    let b: Tensor[f64, [2, 2]] = Tensor.zeros([2, 2]);
    let c: Tensor[f64, [2, 2]] = keep(b);
    print(c.rank());
}
"#,
    );
    // The mono symbols are mangled + quoted (`@"tsum$f64"`), so match on
    // the `<name>$` mangling prefix rather than `@<name>`.
    assert!(
        define_line(&ir, "tsum$").contains("noalias"),
        "owned Tensor param in a monomorph should carry noalias: {}",
        define_line(&ir, "tsum$")
    );
    // Bare generic `T` resolved to `Tensor` through the mono subst.
    assert!(
        define_line(&ir, "keep$").contains("noalias"),
        "owned bare-T param resolved to Tensor should carry noalias: {}",
        define_line(&ir, "keep$")
    );
}

// ── Comparison auto-derefs reference operands ───────────────────
// The typechecker now accepts `value == borrow` of the same type
// (`String == ref String`). At codegen, `load_variable` already
// derefs ref-param / ref-bound-local operands to their pointee value
// before the value-wise compare, and `compile_binop_typed` additionally
// loads a borrowed operand that reaches it as a raw pointer. This E2E
// pins the runtime truth values across both operand orders and `==` /
// `!=` / `<`, for `String` and a scalar — the shape the self-hosted
// parser's `label_known` lookup needs (was worked around with `dup_str`).
#[test]
fn e2e_compare_value_against_borrow_of_same_type() {
    if let Some(out) = run_program(
        "fn s_eq_rb(a: ref String, b: String) -> bool { return a == b }\n\
             fn s_eq_br(a: String, b: ref String) -> bool { return a == b }\n\
             fn s_ne_rb(a: ref String, b: String) -> bool { return a != b }\n\
             fn s_lt_rb(a: ref String, b: String) -> bool { return a < b }\n\
             fn i_eq_rb(a: ref i64, b: i64) -> bool { return a == b }\n\
             fn main() {\n\
             \x20   let key: String = \"hello\";\n\
             \x20   let same: String = \"hello\";\n\
             \x20   let diff: String = \"world\";\n\
             \x20   let apple: String = \"apple\";\n\
             \x20   let world2: String = \"world\";\n\
             \x20   println(s_eq_rb(key, same));\n\
             \x20   println(s_eq_br(apple, key));\n\
             \x20   println(s_ne_rb(key, diff));\n\
             \x20   println(s_lt_rb(key, world2));\n\
             \x20   let n: i64 = 42i64;\n\
             \x20   println(i_eq_rb(n, 42i64));\n\
             \x20   println(i_eq_rb(n, 7i64));\n\
             }\n",
    ) {
        // s_eq_rb(\"hello\",\"hello\")=true; s_eq_br(\"apple\",&\"hello\")=false;
        // s_ne_rb(\"hello\",\"world\")=true; s_lt_rb(\"hello\",\"world\")=true;
        // i_eq_rb(42,42)=true; i_eq_rb(42,7)=false.
        assert_eq!(out, "true\nfalse\ntrue\ntrue\ntrue\nfalse\n");
    }
}

/// B-2026-08-14-21 — `s += x` on a `mut ref String` PARAMETER must reach
/// the caller. The compiled program silently printed the pre-call value:
/// the compound-assign arm handled only a SCALAR `mut ref` target, so an
/// aggregate one fell through to a store into the 8-byte alloca holding the
/// borrow POINTER instead of a store through it. Its plain-`Assign` twin was
/// widened past scalars by B-2026-08-05-39 and this arm was left behind,
/// which is exactly why `s = s + x` through the same parameter worked.
///
/// All five lines are the pin, not just the first: `push_str` and `s = s + x`
/// are the two spellings that already worked (a fix that broke either would
/// be trading one miscompile for another), the loop returned the EMPTY string
/// rather than a short one, and the scalar `mut ref` is the control for the
/// branch this change did not touch. Oracle: the interpreter twin
/// `tests/interpreter.rs::test_compound_assign_through_mut_ref_param`, which
/// was right on all of them.
#[test]
fn test_e2e_compound_assign_through_mut_ref_param() {
    assert_eq!(
        run_program(
            "fn append_plus_eq(s: mut ref String) { s += \"abc\"; }\n\
                 fn append_push(s: mut ref String) { s.push_str(\"abc\"); }\n\
                 fn append_assign(s: mut ref String) { s = s + \"abc\"; }\n\
                 fn append_loop(s: mut ref String) {\n\
                     let mut i = 0i64;\n\
                     while i < 3i64 { s += \"z\"; i = i + 1i64; }\n\
                 }\n\
                 fn bump(n: mut ref i64) { n += 5i64; }\n\
                 fn main() {\n\
                     let mut a = \"X\"; append_plus_eq(mut a); println(a);\n\
                     let mut b = \"X\"; append_push(mut b); println(b);\n\
                     let mut c = \"X\"; append_assign(mut c); println(c);\n\
                     let mut d = \"X\"; append_loop(mut d); println(d);\n\
                     let mut n = 1i64; bump(mut n); println(n);\n\
                 }\n"
        ),
        Some("Xabc\nXabc\nXabc\nXzzz\n6\n".to_string())
    );
}

/// The minimal red member of the same family: two field moves of one
/// binding, then borrow reads of the source. At baseline the SECOND
/// move's suppression zeroed `e.doc.lines`'s len with no copy, so
/// `text(e.doc)`'s `lines[0]` PANICKED out of bounds — the same defect
/// as the fixture above with a louder symptom and half the moving
/// parts. (A chain of consuming CALLS on a plain binding — `eat(a);
/// eat(a); eat(a)` — is NOT in this family: measured green at baseline,
/// heap-backed or not, so by-value call args reach a different copy
/// path. Field moves are the shape the dedup starved.)
#[test]
fn test_e2e_two_field_moves_then_borrow_reads() {
    assert_eq!(
        run_program(
            r#"
struct Doc { lines: Vec[String] }
struct Ed { doc: Doc }
fn count(d: ref Doc) -> i64 { return d.lines.len(); }
fn text(d: ref Doc) -> String { return d.lines[0]; }
fn main() {
    let mut l: Vec[String] = Vec.new();
    l.push("alphaalphaalpha");
    let mut e = Ed { doc: Doc { lines: l } };
    let cur = e.doc;
    let after = e.doc;
    println(f"{cur.lines[0]} {after.lines[0]} [{text(e.doc)}] {count(e.doc)}");
}
"#
        ),
        Some("alphaalphaalpha alphaalphaalpha [alphaalphaalpha] 1\n".to_string())
    );
}

/// A TUPLE-MEMBER leaf read through a BORROWED place is a COPY —
/// `fn peek(t: ref Wt) -> String { t.pair.0 }` (B-2026-08-28-37).
///
/// Semantic twin of the field-leaf oracle above, and here for the same
/// reason: the abort is gated under LSan, but only the interpreter settles
/// which of the two silencing fixes is correct. Cap-zeroing the source
/// would strand the caller's buffer AND leave the borrowed struct's member
/// empty; cloning leaves it readable, which is what the interpreter does.
///
/// The `let` row is carried alongside the return rows because a tuple
/// member was broken at BOTH positions, unlike the field spelling — so a
/// fix that widened only the consuming arm still fails here.
#[test]
fn test_e2e_borrowed_tuple_member_leaf_is_a_copy() {
    let src = r#"
struct Wt { pair: (String, i64), k: i64 }

fn ret(t: ref Wt) -> String { return t.pair.0; }
fn bound(t: ref Wt) -> String { let s = t.pair.0; return s; }

fn main() {
    let v = Wt { pair: (f"n{41}", 7), k: 2 };
    let mut a = ret(v);
    a = a + "X";
    println(a);
    println(v.pair.0);
    let mut b = bound(v);
    b = b + "Y";
    println(b);
    println(v.pair.0);
    println(v.pair.1);
}
"#;
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(
        interp_errs.is_empty(),
        "interpreter errors: {interp_errs:?}"
    );
    let expected = interp_out.join("");
    // `n41X` then `n41` is the copy: a move model prints `n41X` then empty
    // or garbage for the borrowed struct's member.
    assert_eq!(
        expected, "n41X\nn41\nn41Y\nn41\n7\n",
        "interpreter oracle is wrong for a borrowed tuple-member leaf",
    );
    let Some(aot) = run_program(src) else { return };
    assert_eq!(
        aot, expected,
        "a tuple member read through a borrowed place must be a copy",
    );
}

/// The STRUCT half of the same name-keyed dispatch, which was reachable on
/// its own before the enum shadow was ever recorded (B-2026-08-15-12).
///
/// `owned_ptr_param_is_noalias_safe` is a third list keyed on the built-in
/// name, and it did not consult the shadow registry either. A user
/// `struct Map` whose shadow WAS recorded therefore lowered correctly to an
/// aggregate while the alias pass still stamped `noalias` on it — tripping
/// the debug-assert that guards that arm, so `karac build` PANICKED on a
/// program `karac check` accepts.
///
/// Only the seven `OWNED_VALUE_PTR_TYPES` names take the owned-`ptr` arm,
/// and only an OWNED param reaches it (`ref`/`mut ref` are excluded by
/// shape), so that is exactly what this sweeps.
#[test]
fn test_e2e_user_struct_shadowing_an_owned_ptr_handle_takes_no_alias_attr() {
    for name in [
        "Tensor",
        "Column",
        "DataFrame",
        "Map",
        "Set",
        "SortedMap",
        "SortedSet",
    ] {
        let Some(out) = run_program(&format!(
            "struct {name} {{ v: i64 }}\n\
                 fn g(e: {name}) -> i64 {{ return e.v + 1; }}\n\
                 fn main() {{ println(g({name} {{ v: 4 }})); }}\n"
        )) else {
            return;
        };
        assert_eq!(out, "5\n", "{name}: shadowing struct read wrong");
    }
}

/// B-2026-08-21-38 (codegen half) — a `mut ref T` METHOD parameter fed a
/// FIELD or TUPLE-INDEX place. The identifier fast path does not cover
/// either, so the argument fell through to the rvalue path and the callee
/// mutated a COPY: `h.bump(mut b.v)` answered the pre-call value while the
/// byte-identical FREE function `free_bump(mut b.v)` answered the
/// incremented one on every surface. The free-function and generic paths
/// got this in B-2026-08-05-41; the method path never did.
#[test]
fn method_mut_ref_field_place_arg_writes_through() {
    assert_eq!(
        run_program(
            "struct Box { v: i64 }\n\
                 struct H { acc: i64 }\n\
                 impl H { fn bump(ref self, x: mut ref i64) -> i64 { x = x + 1; x } }\n\
                 fn free_bump(x: mut ref i64) -> i64 { x = x + 1; x }\n\
                 fn main() {\n\
                     let mut a = Box { v: 4 };\n\
                     println(free_bump(mut a.v));\n\
                     println(a.v);\n\
                     let h = H { acc: 0 };\n\
                     let mut b = Box { v: 4 };\n\
                     println(h.bump(mut b.v));\n\
                     println(b.v);\n\
                 }"
        ),
        Some("5\n5\n5\n5\n".to_string())
    );
}

/// A read-only `ref` method parameter must NOT be switched to the place
/// pointer — the transplanted arm is gated on the parameter MODE, and a
/// widening to every `ref` param is the regression shape this family has
/// a history of. A reader still sees the caller's value, and a `mut ref
/// self` receiver plus a `mut ref` field argument still publish to two
/// different places.
#[test]
fn method_ref_field_arg_reads_and_mut_ref_self_still_writes() {
    assert_eq!(
        run_program(
            "struct Box { v: i64 }\n\
                 struct H { acc: i64 }\n\
                 impl H {\n\
                     fn peek(ref self, x: ref i64) -> i64 { x + 1 }\n\
                     fn bump(mut ref self, x: mut ref i64) -> i64 {\n\
                         self.acc = self.acc + 1;\n\
                         x = x + 10;\n\
                         x\n\
                     }\n\
                 }\n\
                 fn main() {\n\
                     let mut h = H { acc: 0 };\n\
                     let b = Box { v: 4 };\n\
                     println(h.peek(b.v));\n\
                     println(b.v);\n\
                     let mut c = Box { v: 1 };\n\
                     println(h.bump(mut c.v));\n\
                     println(c.v);\n\
                     println(h.acc);\n\
                 }"
        ),
        Some("5\n4\n11\n11\n1\n".to_string())
    );
}

/// B-2026-08-28-13 — a heap field moved OUT of a BORROWED binding inside a
/// GENERIC fn/impl aliased the caller's buffer, so both freed it: SIGABRT
/// (`free(): double free detected in tcache 2`) under LLJIT and AOT alike,
/// on a program `karac check` passes and the interpreter runs correctly.
///
/// The interpreter is the oracle and it never moved: the local is a COPY,
/// so mutating it leaves the caller's field untouched (the `mutate` row) —
/// which is what settles the ownership question the row raised. The move is
/// legal; codegen owed the local a deep copy and the generic path lost it.
/// `clone_ref_chain_field_move_rhs` read the field type from the struct
/// DECLARATION table (`Vec[T]` inside `impl[T] Bag[T]`) and
/// `borrow_payload_clone_supported` then declined the unsubstituted param,
/// skipping the clone silently. Same type-param erasure family as
/// B-2026-08-25-10/-11, at the borrowed-receiver site rather than the owned
/// one, and fixed the way those were: substitute the active monomorph.
///
/// `readback` is the row's sharpest case: it reads the CALLER's field after
/// the call, so a shallow alias shows up as a use-after-free rather than as
/// two frees racing at exit. `nested-concrete` covers `T = Vec[i64]` —
/// pre-fix that one HUNG under LLJIT instead of aborting, the same defect
/// wearing a different symptom.
///
/// The last two rows are controls that were already correct pre-fix, and
/// they are what isolate the trigger to erasure: `control-nongeneric` never
/// erases the field type, and `control-string-field` is a bare `String`
/// field with no parameter to substitute.
#[test]
fn e2e_generic_borrowed_field_move_out_is_a_copy_not_an_alias() {
    let cases: &[(&str, &str, &str)] = &[
            (
                "generic-impl",
                "struct Bag[=T] { xs: Vec[T] }\n\
                 impl[T] Bag[T] { fn n(ref self) -> i64 { let v = self.xs; return v.len(); } }\n\
                 fn main() { let mut a: Bag[i64] = Bag { xs: Vec.new() };\n\
                 a.xs.push(1); a.xs.push(2); println(f\"{a.n()}\"); }",
                "2\n",
            ),
            (
                "generic-free-fn",
                "struct Bag[=T] { xs: Vec[T] }\n\
                 fn n[T](b: ref Bag[T]) -> i64 { let v = b.xs; return v.len(); }\n\
                 fn main() { let mut a: Bag[i64] = Bag { xs: Vec.new() };\n\
                 a.xs.push(1); a.xs.push(2); println(f\"{n(a)}\"); }",
                "2\n",
            ),
            (
                "readback",
                "struct Bag[=T] { xs: Vec[T] }\n\
                 impl[T] Bag[T] { fn n(ref self) -> i64 { let v = self.xs; return v.len(); } }\n\
                 fn main() { let mut a: Bag[i64] = Bag { xs: Vec.new() };\n\
                 a.xs.push(1); a.xs.push(2); println(f\"{a.n()}\"); println(f\"{a.xs[0]}\"); }",
                "2\n1\n",
            ),
            (
                "string-elem",
                "struct Bag[=T] { xs: Vec[T] }\n\
                 impl[T] Bag[T] { fn n(ref self) -> i64 { let v = self.xs; return v.len(); } }\n\
                 fn main() { let mut a: Bag[String] = Bag { xs: Vec.new() };\n\
                 a.xs.push(\"hi\"); a.xs.push(\"yo\"); println(f\"{a.n()}\"); }",
                "2\n",
            ),
            (
                "mut-ref-param",
                "struct Bag[=T] { xs: Vec[T] }\n\
                 fn n[T](b: mut ref Bag[T]) -> i64 { let v = b.xs; return v.len(); }\n\
                 fn main() { let mut a: Bag[i64] = Bag { xs: Vec.new() };\n\
                 a.xs.push(1); a.xs.push(2); println(f\"{n(mut a)}\"); }",
                "2\n",
            ),
            (
                "second-type-param",
                "struct Pair[=K, =V] { ks: Vec[K], vs: Vec[V] }\n\
                 impl[K, V] Pair[K, V] { fn nv(ref self) -> i64 { let v = self.vs; return v.len(); } }\n\
                 fn main() { let mut a: Pair[i64, String] = Pair { ks: Vec.new(), vs: Vec.new() };\n\
                 a.vs.push(\"x\"); a.vs.push(\"y\"); println(f\"{a.nv()}\"); }",
                "2\n",
            ),
            (
                "nested-concrete",
                "struct Bag[=T] { xs: Vec[T] }\n\
                 impl[T] Bag[T] { fn n(ref self) -> i64 { let v = self.xs; return v.len(); } }\n\
                 fn main() { let mut a: Bag[Vec[i64]] = Bag { xs: Vec.new() };\n\
                 let mut inner: Vec[i64] = Vec.new(); inner.push(7); a.xs.push(inner);\n\
                 println(f\"{a.n()}\"); }",
                "1\n",
            ),
            (
                // The local is a COPY: pushing to it must NOT reach the caller.
                "mutate",
                "struct Bag[=T] { xs: Vec[T] }\n\
                 impl[T] Bag[T] { fn n(ref self) -> i64 { let mut v = self.xs; v.push(99); return v.len(); } }\n\
                 fn main() { let mut a: Bag[i64] = Bag { xs: Vec.new() };\n\
                 a.xs.push(1); a.xs.push(2); println(f\"{a.n()}\"); println(f\"{a.xs.len()}\"); }",
                "3\n2\n",
            ),
            (
                "control-nongeneric",
                "struct Bag { xs: Vec[i64] }\n\
                 impl Bag { fn n(ref self) -> i64 { let v = self.xs; return v.len(); } }\n\
                 fn main() { let mut a: Bag = Bag { xs: Vec.new() };\n\
                 a.xs.push(1); a.xs.push(2); println(f\"{a.n()}\"); println(f\"{a.xs[0]}\"); }",
                "2\n1\n",
            ),
            (
                "control-string-field",
                "struct Bag[=T] { name: String, xs: Vec[T] }\n\
                 impl[T] Bag[T] { fn nm(ref self) -> i64 { let s = self.name; return s.len(); } }\n\
                 fn main() { let a: Bag[i64] = Bag { name: \"hello\", xs: Vec.new() };\n\
                 println(f\"{a.nm()}\"); println(f\"{a.name}\"); }",
                "5\nhello\n",
            ),
        ];
    for (label, src, want) in cases {
        assert_eq!(
            run_program(src).as_deref(),
            Some(*want),
            "[{label}] compiled output diverged from the interpreter oracle"
        );
    }
}

// ── An argument STORED into an outliving place (B-2026-08-29-49) ────
//
// `fn take(sink: mut ref Vec[Res], r: Res) { sink.push(r); }` hands the
// argument to a container the caller already holds, so the container's
// drain is the one owner and the caller must not also fire the body.
// `fn_moves_param_into_outliving_place` has said so since B-2026-08-26-9 —
// but only the FRESH-TEMP registrar ever asked. An identifier argument is
// skipped by that walk and drops through its own binding, which consulted
// nothing, so ONE callee compiled ONCE gave two answers depending on how
// the argument was spelled at the call:
//
//     take(mut sink, Res { id: 1 });   // `pushed 1  drop 1`      one body
//     let c = Res { id: 2 };
//     take(mut sink, c);               // `drop 2  pushed 1  drop 2`  two
//
// Unanimous on interp / JIT / AOT / `KARAC_AUTO_PAR=0`, so no A/B gate
// reports it; the fresh-temp spelling is the oracle that makes it visible.
//
// The row was filed as a METHOD defect over an enum payload. It is neither:
// the free-function spelling counts identically, and the enum is a SECOND,
// independent hole — `moves()` matches the param by NAME, so a callee that
// destructures it first (`match b { Full(r) => sink.push(r) }`) stores `r`,
// never `b`, and the predicate answered false for a value that plainly
// escapes. With it blind, even the fresh-temp enum spelling ran two bodies.

/// Leg 1 — the NAMED-binding spelling alone.
#[test]
fn e2e_named_arg_stored_into_mut_ref_param_runs_one_body() {
    assert_eq!(
        run_program(
            "struct Res { id: i64 }\n\
                 impl Drop for Res {\n\
                 \x20   fn drop(mut ref self) { println(f\"drop {self.id}\"); }\n\
                 }\n\
                 fn take(sink: mut ref Vec[Res], r: Res) { sink.push(r); }\n\
                 fn main() {\n\
                 \x20   let mut sink: Vec[Res] = Vec.new();\n\
                 \x20   let carg: Res = Res { id: 7 };\n\
                 \x20   take(mut sink, carg);\n\
                 \x20   println(f\"pushed {sink.len()}\");\n\
                 }\n"
        ),
        Some("pushed 1\ndrop 7\n".to_string())
    );
}
