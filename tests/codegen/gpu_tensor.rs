//! GPU dispatch, tensors, autograd -- fixtures for `tests/codegen.rs`.
//!
//! Split out of `tests/codegen.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test codegen` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test codegen gpu_tensor::
//!
//! New fixtures about GPU dispatch, tensors, autograd belong in this file.

use super::*;

#[test]
fn test_e2e_tensor_from_arrow_ipc_shape_mismatch_traps() {
    // The half of shape reconciliation the round-trip test can't show: a
    // stream whose shape does NOT satisfy the receiver's annotation must
    // trap, not be silently reinterpreted. Both cases below carry the
    // right element COUNT (6) and would "work" if the reader merely
    // reshaped, which is exactly why they are the interesting ones — a
    // [2,3] tensor read as [3,2] is a different tensor, and read as [6] a
    // different rank.
    //
    // No interpreter oracle here: the interpreter builds its tensor from
    // the stream's own dims without consulting the annotation at all, so
    // it accepts both (B-2026-07-28-10's Tensor face). The compiled
    // behaviour is the correct one, so this asserts it directly.
    for (label, annotated) in [
        ("transposed shape", "Tensor[i64, [3, 2]]"),
        ("flattened rank", "Tensor[i64, [6]]"),
    ] {
        let src = format!(
            "fn main() {{\n\
                     let t = Tensor.from([[1, 2, 3], [4, 5, 6]]);\n\
                     let bad: {annotated} = Tensor.from_arrow_ipc(t.to_arrow_ipc());\n\
                     println(bad.sum());\n\
                 }}"
        );
        if let Some(cap) = run_program_capturing(&src) {
            assert_eq!(
                cap.status.code(),
                Some(101),
                "{label}: expected a trap; stdout={:?} stderr={:?}",
                cap.stdout,
                cap.stderr
            );
            assert!(
                !cap.stdout.contains("21"),
                "{label}: a mismatched shape must not produce a tensor"
            );
            // `emit_panic` fprintfs to stderr, so that is where the fault
            // lands (B-2026-08-23-17).
            assert!(
                cap.stderr
                    .contains("shape does not match the declared shape"),
                "{label}: expected the shape-reconciliation panic, got stderr={:?}",
                cap.stderr
            );
        }
    }
}

#[test]
fn test_e2e_vec_of_weak_read_back_upgrades_and_leaves_the_target_intact() {
    // B-2026-08-08-4 gap B (read-back). B-2026-08-08-5 landed the STORE
    // half and left a `Vec[weak T]` store-only: `match v[i] { Some(x) => .. }`
    // failed codegen with `Undefined variable 'x'`, so a weak container was
    // usable for cycle-breaking back-edges (which exist not to be
    // traversed) and not for a parent-pointer walk.
    //
    // Row 1 is NOT about the read at all — it is the silent corruption the
    // read exposed. `weak_targeted_types` was collected from DIRECT `weak T`
    // fields only, so a `Vec[weak N]` never forced `N`'s two-word
    // `{ strong, weak, … }` box; `karac_weak_downgrade` then incremented
    // what it took to be the weak count at word 1, which in a one-word
    // layout is the struct's FIRST FIELD. A bare `w.push(a)` changed `a.v`
    // from 41 to 42 with no diagnostic and no sanitizer report — shipped,
    // and reproducible on a stashed tree.
    //
    // Rows 2-4 are the read itself: the value while the target is alive,
    // and `None` once it is gone. The None row is what makes this a weak
    // ref rather than an awkward strong one.
    let out = run_program(
        r#"
shared struct N { mut v: i64 }
fn fill(w: mut ref Vec[weak N]) {
    let a: N = N { v: 7i64 };
    w.push(a);
    match w[0] { Some(x) => { println(x.v); } None => { println(0 - 1); } }
}
fn main() {
    let keep: N = N { v: 41i64 };
    let mut c: Vec[weak N] = Vec.new();
    c.push(keep);
    println(keep.v);

    let mut w: Vec[weak N] = Vec.new();
    fill(mut w);
    match w[0] { Some(x) => { println(x.v); } None => { println(0 - 2); } }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "41\n7\n-2");
    }
}

/// B-2026-08-09-2 — `Map[K, weak V]` was store-only: B-2026-08-08-29 landed
/// the store half and typed `get` as `Option[weak V]`, so the `Some` binding
/// was a bare `weak V` and every field access through it was rejected
/// ("no field 'v' on type 'this type'").
///
/// The row filed that as LOW — "a refusal, not a miscompile". Relaxing the
/// typecheck alone turned it into one, which is the whole reason these three
/// assertions exist rather than a single accept test: with `get` typed
/// `Option[V]` but codegen still handing back the raw stored word, an ALIVE
/// referent read correctly by accident (the weak slot holds the box pointer)
/// while a DEAD one read the freed box and printed its stale field —
/// valgrind `Invalid read of size 8`, no diagnostic. Line 3 is the one that
/// discriminates; lines 1-2 pass against the broken compiler.
#[test]
fn test_e2e_map_of_weak_get_upgrades_and_reads_none_once_the_target_dies() {
    let out = run_program(
        r#"
shared struct N { mut v: i64 }

fn build_map() -> Map[i64, weak N] {
    let mut m: Map[i64, weak N] = Map.new();
    let a: N = N { v: 41i64 };
    m.insert(1i64, a);
    match m.get(1i64) { Some(x) => { println(x.v); } None => { println(0 - 1); } }
    return m;
}

// Overwrite the dead frame so a stale pointer cannot read as live by residue.
fn churn(n: i64) -> i64 {
    if n <= 0 { return 0; }
    return n + churn(n - 1);
}

fn main() {
    let m: Map[i64, weak N] = build_map();
    println(churn(60));
    // The referent died with `build_map`'s frame; the map's weak ref must
    // upgrade to `None` rather than hand back the freed box.
    match m.get(1i64) { Some(y) => { println(y.v); } None => { println(0 - 99); } }
    // A key that was never inserted is the same `None`, and it must not read
    // the uninitialised value slot the runtime leaves untouched on a miss.
    match m.get(7i64) { Some(z) => { println(z.v); } None => { println(0 - 7); } }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "41\n1830\n-99\n-7");
    }
}

#[test]
fn vec_of_weak_push_downgrades_and_takes_no_strong_count() {
    let program = |elem: &str| {
        format!(
            "shared struct N {{ v: i64, mut ns: Vec[{elem}] }}\n\
                 fn build(s: i64) -> i64 {{\n\
                     let a = N {{ v: s, ns: Vec.new() }};\n\
                     let b = N {{ v: s + 1, ns: Vec.new() }};\n\
                     a.ns.push(b);\n\
                     b.ns.push(a);\n\
                     a.ns.len() + b.ns.len()\n\
                 }}\n\
                 fn main() {{ println(f\"{{build(1)}}\"); }}\n"
        )
    };

    let weak_ir = ir_for(&program("weak N"));
    assert!(
        weak_ir.contains("karac_weak_downgrade"),
        "a `Vec[weak N]` push must downgrade on the way in; without it the \
             container stores a strong pointer into a weak slot"
    );
    // B-2026-08-08-29 renamed this from `__karac_vec_elem_weak_drop`: the
    // same fn now serves a `Map[K, weak V]` value slot, and a `vec_elem`
    // name on a Map value drop is the misnomer class that hid
    // B-2026-08-08-20.
    assert!(
        weak_ir.contains("__karac_weak_slot_drop"),
        "a `Vec[weak N]` must weak-drop each element at scope exit, or it \
             never releases what its pushes took"
    );
    // Counted on the SSA definition, not on substring hits: an `rc_inc`
    // name appears twice per retain.
    let weak_incs = weak_ir.matches("= add i64 %rc").count();

    let strong_ir = ir_for(&program("N"));
    let strong_incs = strong_ir.matches("= add i64 %rc").count();
    assert!(
        !strong_ir.contains("karac_weak_downgrade"),
        "the strong control must NOT downgrade — otherwise the weak rows \
             above prove nothing about the `weak` spelling"
    );
    assert!(
        strong_incs > weak_incs,
        "a weak element takes no strong count, so the weak build must emit \
             FEWER retains than the identical strong-element build; got \
             weak={weak_incs} strong={strong_incs}"
    );
}

/// B-2026-08-08-29 — the `Map` twin of the row above, asserted the same
/// way: on the IR, against the identical program with the `weak` dropped.
///
/// `Map` had the typecheck relaxation and none of the codegen, so an insert
/// stored a STRONG pointer into a slot the author declared `weak` — no
/// downgrade, the ordinary transfer inc still firing, and no scope-exit
/// drain at all. All three are checked here because the leak needs only one
/// of them to come back, and the runtime shape (24 bytes against a clean
/// strong twin) is the same for each.
#[test]
fn map_of_weak_value_insert_downgrades_and_takes_no_strong_count() {
    let program = |val: &str| {
        format!(
            "shared struct N {{ mut v: i64 }}\n\
                 fn build(s: i64) -> i64 {{\n\
                     let mut m: Map[i64, {val}] = Map.new();\n\
                     let a = N {{ v: s }};\n\
                     m.insert(1i64, a);\n\
                     a.v + m.len()\n\
                 }}\n\
                 fn main() {{ println(f\"{{build(1)}}\"); }}\n"
        )
    };

    let weak_ir = ir_for(&program("weak N"));
    assert!(
        weak_ir.contains("karac_weak_downgrade"),
        "a `Map[i64, weak N]` insert must downgrade on the way in; without \
             it the bucket holds a strong pointer in a weak slot"
    );
    assert!(
        weak_ir.contains("__karac_weak_slot_drop"),
        "a `Map[i64, weak N]` must weak-drop each value at scope exit, or \
             it never releases what its inserts took"
    );
    let weak_incs = weak_ir.matches("= add i64 %rc").count();

    let strong_ir = ir_for(&program("N"));
    let strong_incs = strong_ir.matches("= add i64 %rc").count();
    assert!(
        !strong_ir.contains("karac_weak_downgrade"),
        "the strong control must NOT downgrade — otherwise the weak rows \
             above prove nothing about the `weak` spelling"
    );
    assert!(
        strong_incs > weak_incs,
        "a weak value takes no strong count, so the weak build must emit \
             FEWER retains than the identical strong-value build; got \
             weak={weak_incs} strong={strong_incs}"
    );
}

/// The workaround B-2026-08-26-10's equality diagnostic prescribes is TRUE:
/// `impl PartialEq` + the `Eq` marker really does dispatch `==` to the
/// user's `eq` body, on the compiled backends as well as the interpreter.
///
/// This is the honesty pin for that message. The diagnostic tells an author
/// with a hand-written `impl PartialEq` to add `impl Eq for T {}`; if
/// lowering ever stops gating on the `Eq` marker
/// (`target_type_name` in `lowering.rs`), the advice becomes a lie that
/// silently returns structural comparison, and no typechecker test would
/// notice — the program still compiles, it just answers wrongly.
///
/// `eq` returns `false` unconditionally and the two values are IDENTICAL,
/// so structural comparison and the user's body give opposite answers:
/// structural says `true`/`false`, the body says `false`/`true`. A test
/// whose `eq` merely ignored a field would pass under either.
#[test]
fn e2e_user_eq_impl_with_eq_marker_is_dispatched_by_the_operator() {
    let Some(out) = run_program(
        "struct Item { id: i64, tag: i64 }\n\
             impl PartialEq for Item {\n\
             \x20   fn eq(ref self, other: ref Item) -> bool { false }\n\
             }\n\
             impl Eq for Item {}\n\
             fn main() {\n\
             \x20   let a = Item { id: 7, tag: 7 };\n\
             \x20   let b = Item { id: 7, tag: 7 };\n\
             \x20   println(a == b);\n\
             \x20   println(a != b);\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(
        out, "false\ntrue\n",
        "`==` must call the user's `eq` (false), not compare structurally (true)"
    );
}

#[test]
fn test_e2e_u64_column_tensor_sort_unsigned_match_run() {
    // B-2026-07-07-2 (follow-on to B-2026-07-04-8): `Column[u64]` /
    // `Tensor[u64]` `sorted`/`argsort` under `karac build` used to
    // loud-reject (the shared scratch sort keyed integers at signed
    // `sgt`, misordering values ≥ 2⁶³). Now `SortKey` carries the
    // signedness and `emit_sort_scratch` picks `ugt` for u64, so build
    // matches the interpreter's u64 model exactly. The oracle is the
    // `karac run` output pinned in tests/interpreter.rs
    // (test_interp_column_u64_sorted_and_argsort_unsigned).
    let src = "fn main() {\n\
                   \x20   let c: Column[u64] = Column.from_vec([1u64 << 63, 5u64, 1u64 << 62]);\n\
                   \x20   let s = c.sorted();\n\
                   \x20   println(f\"{s[0]},{s[1]},{s[2]}\");\n\
                   \x20   let a = c.argsort();\n\
                   \x20   println(f\"{a[0]},{a[1]},{a[2]}\");\n\
                   \x20   let t: Tensor[u64, [3]] = Tensor.from([1u64 << 63, 5u64, 1u64 << 62]);\n\
                   \x20   let ts = t.sorted();\n\
                   \x20   println(f\"{ts[0]},{ts[1]},{ts[2]}\");\n\
                   \x20   let ta = t.argsort();\n\
                   \x20   println(f\"{ta[0]},{ta[1]},{ta[2]}\");\n\
                   }\n";
    // sorted ascending: 5, 2⁶², 2⁶³; argsort: original indices 1, 2, 0.
    assert_eq!(
        run_program(src).as_deref(),
        Some(
            "5,4611686018427387904,9223372036854775808\n\
                 1,2,0\n\
                 5,4611686018427387904,9223372036854775808\n\
                 1,2,0\n"
        )
    );
}

/// The third non-`let` site, the tuple destructure, through the SAME
/// consuming oracle as its for-loop and match siblings above.
///
/// It was pinned through a non-consuming chain when B-2026-08-27-36 landed,
/// because consuming a generic struct destructured out of a tuple PARAM
/// double-freed (B-2026-08-27-37: the mono got no entry-copy while the
/// caller still dropped its temp). That is fixed, so this now uses the
/// consuming shape the comment there asked for — the site is covered by one
/// oracle across all three bindings rather than a weaker stand-in.
#[test]
fn e2e_generic_tuple_destructure_binding_dispatches_to_the_monomorph() {
    let src = "struct Bag[=T] { xs: Vec[T] }\n\
                   impl[T: Ord] Bag[T] {\n    \
                       fn swap2(mut ref self, i: i64, j: i64) { self.xs.swap(i, j); }\n    \
                       fn arrange(mut ref self) { let n = self.xs.len(); if n > 1 { self.swap2(0, n - 1); } }\n    \
                       fn inner(self) -> Vec[T] { let mut b = self; b.arrange(); b.xs }\n    \
                       fn mk(p: (Bag[T], i64)) -> Vec[T] { let (b, _n) = p; b.inner() }\n\
                   }\n\
                   fn main() {\n    \
                       let a = Bag.mk((Bag { xs: [\"x\", \"y\", \"z\"] }, 0)); println(a[0]);\n    \
                       let b = Bag.mk((Bag { xs: [1, 2, 3] }, 0)); println(b[0]);\n\
                   }\n";
    assert_eq!(run_program(src).as_deref(), Some("z\n3\n"));
}

/// B-2026-08-26-36 — a `ref` binding over a TENSOR element, the shape
/// `std.autograd`'s tape actually holds (`Vec[Tensor[f32, [?]]]`), read
/// through a struct field exactly as `TensorVar.grad_at` reads it.
///
/// Asserts the VALUES, not merely that it compiles, and that is the whole
/// point of the test. The first attempt at this feature bound a borrowed
/// tensor as a `ref_params` shim AND registered `tensor_var_infos`, which
/// left one indirection too many: `tensor_ptr_for_var` loads through the
/// shim via `get_data_ptr` and then loads again, reading the control
/// block's first word as a pointer. It did not fail loudly — it compiled
/// and printed denormal garbage (`0.000…15956413663` where `2 4 8` was
/// expected). A test that only checked for successful compilation would
/// have passed on the broken version.
#[test]
fn test_e2e_ref_binding_over_a_tensor_element() {
    let Some(out) = run_program(
        r#"
shared struct Tape { mut grads: Vec[Tensor[f32, [?]]] }
fn main() {
    let tp: Tape = Tape { grads: Vec.new() };
    let z: Tensor[f32, [?]] = Tensor.from([2.0, 4.0, 8.0]);
    tp.grads.push(z);
    let g = ref tp.grads[0];
    println(f"{g[0]} {g[1]} {g[2]}");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out, "2 4 8\n",
        "borrowed tensor element must read the container's own block; got: {out:?}"
    );
}

#[test]
fn test_e2e_map_set_method_dispatch_from_enum_payload() {
    // B-2026-07-23-3 — a `Map`/`Set`-family collection bound out of a
    // USER-ENUM variant payload kept its container type for codegen method
    // dispatch, so `m.len()` / `s.contains(x)` route to the collection arm
    // instead of failing "no handler for method". The via-ptr (`ref V`) fast
    // path used to bind the single-pointer Map word as a bare ref-to-i64
    // (its i64 fallback LLVM type masqueraded as a real payload word) and
    // skipped the dispatch side-tables; now it defers to the value-source
    // path, which registers them. Extended to the whole Map/Set family
    // (`SortedMap` / `SortedSet` share the K/V/elem extraction). Covers
    // by-ref and by-value scrutinees; must match `karac run`.
    let out = run_program(
            "enum MapBox { Table(Map[String, i64]) }\n\
             enum SetBox { Items(Set[i64]) }\n\
             enum SmapBox { T(SortedMap[i64, i64]) }\n\
             enum SsetBox { S(SortedSet[i64]) }\n\
             fn map_len_ref(v: ref MapBox) -> i64 { match v { Table(m) => m.len() as i64 } }\n\
             fn map_len_val(v: MapBox) -> i64 { match v { Table(m) => m.len() as i64 } }\n\
             fn set_len_ref(v: ref SetBox) -> i64 { match v { Items(s) => s.len() as i64 } }\n\
             fn set_has_ref(v: ref SetBox, x: i64) -> bool { match v { Items(s) => s.contains(x) } }\n\
             fn smap_len_ref(v: ref SmapBox) -> i64 { match v { T(m) => m.len() as i64 } }\n\
             fn sset_len_ref(v: ref SsetBox) -> i64 { match v { S(s) => s.len() as i64 } }\n\
             fn main() {\n\
                 let mut mp: Map[String, i64] = Map.new();\n\
                 let _ = mp.insert(\"a\", 1);\n\
                 let _ = mp.insert(\"b\", 2);\n\
                 let t = MapBox.Table(mp);\n\
                 println(map_len_ref(t));\n\
                 let mut mp2: Map[String, i64] = Map.new();\n\
                 let _ = mp2.insert(\"x\", 9);\n\
                 println(map_len_val(MapBox.Table(mp2)));\n\
                 let mut st: Set[i64] = Set.new();\n\
                 st.insert(1); st.insert(2); st.insert(2);\n\
                 let sv = SetBox.Items(st);\n\
                 println(set_len_ref(sv));\n\
                 println(set_has_ref(sv, 2));\n\
                 println(set_has_ref(sv, 5));\n\
                 let mut sm: SortedMap[i64, i64] = SortedMap.new();\n\
                 let _ = sm.insert(3, 1);\n\
                 let _ = sm.insert(1, 2);\n\
                 println(smap_len_ref(SmapBox.T(sm)));\n\
                 let mut ss: SortedSet[i64] = SortedSet.new();\n\
                 ss.insert(7); ss.insert(4); ss.insert(7);\n\
                 println(sset_len_ref(SsetBox.S(ss)));\n\
             }",
        );
    if let Some(out) = out {
        assert_eq!(out, "2\n1\n2\ntrue\nfalse\n2\n2\n");
    }
}

#[test]
fn test_e2e_multiversion_dispatches_correctly() {
    // Phase-11 `#[multiversion(baseline, "avx2", "avx512f")]` (design.md §
    // Multiversioning): the attribute desugars to per-feature
    // `#[target_feature]` variant clones + a `cpu.supports` dispatch thunk.
    // All variants compute the SAME result (only the instruction set differs),
    // so a correct output proves the thunk dispatched to a working variant on
    // the running host. `run` == `build`.
    if let Some(out) = run_program(
        r#"
#[multiversion(baseline, "avx2", "avx512f")]
fn addup(a: i64, b: i64) -> i64 { a + b }

#[multiversion(baseline, "avx2")]
fn scale(v: i64) -> i64 { v * 3 }

fn main() {
    println(addup(20, 22));
    println(scale(14));
}
"#,
    ) {
        assert_eq!(out, "42\n42\n");
    }
}

#[test]
fn test_ir_multiversion_emits_variants_and_dispatch() {
    // The desugar synthesizes a `$baseline` clone + one `$<feat>` clone per
    // feature (each tagged with a `target-features` attribute) and rewrites
    // the public function into a thunk that calls `karac_cpu_supports`.
    // Asserted on the unoptimized module IR (before variants inline away).
    // `ir_for_desugared` runs `desugar_program` first — the multiversion
    // rewrite is a pre-resolve desugar, so plain `ir_for` would miss it.
    let ir = ir_for_desugared(
        "#[multiversion(baseline, \"avx2\", \"avx512f\")]\n\
             fn addup(a: i64, b: i64) -> i64 { a + b }\n\
             fn main() { let _ = addup(1, 2); }",
    );
    assert!(
        ir.contains("addup$baseline") && ir.contains("addup$avx2") && ir.contains("addup$avx512f"),
        "expected baseline + per-feature variant functions; IR:\n{ir}"
    );
    assert!(
        ir.contains("karac_cpu_supports"),
        "expected the dispatch thunk to call karac_cpu_supports; IR:\n{ir}"
    );
    assert!(
        ir.contains("+avx2") && ir.contains("+avx512f"),
        "expected per-variant target-features attributes; IR:\n{ir}"
    );
}

#[test]
fn test_e2e_multiversion_method_and_generic_dispatch() {
    // Phase-11 `#[multiversion]` follow-on: dispatch on `self`-receiver
    // methods (all three receiver modes) and on generic free functions.
    // Every variant computes the same result, so correct output proves the
    // thunk dispatched to a working variant on the running host. `run` ==
    // `build`; interpreter parity in
    // tests/interpreter.rs::test_multiversion_method_and_generic_dispatch.
    if let Some(out) = run_program(
        r#"
struct Acc { base: i64 }
impl Acc {
    #[multiversion(baseline, "avx2", "avx512f")]
    fn dot(ref self, x: i64) -> i64 { self.base + x }

    #[multiversion(baseline, "avx2")]
    fn scale(mut ref self, k: i64) -> i64 {
        self.base = self.base * k;
        self.base
    }

    #[multiversion(baseline, "avx2")]
    fn consume(self, y: i64) -> i64 { self.base + y }
}

#[multiversion(baseline, "avx2", "avx512f")]
fn gadd[T: Add](a: T, b: T) -> T { a + b }

fn main() {
    let a = Acc { base: 100 };
    println(a.dot(5));
    let mut b = Acc { base: 3 };
    println(b.scale(4));
    let c = Acc { base: 7 };
    println(c.consume(1));
    println(gadd(20, 22));
}
"#,
    ) {
        assert_eq!(out, "105\n12\n8\n42\n");
    }
}

/// Binding a row out of a BORROWED nested collection — `let row = m[i]`
/// where `m: ref Vec[Vec[i64]]` — must dispatch `row.len()` / `row[j]` as
/// the inner `Vec[i64]`. The integer-index inference used to peel `ref`
/// only on the range-index (`m[a..b]`) and Tensor/Column paths, so a scalar
/// index of a borrowed Vec inferred `Type::Error` for the binding and
/// codegen failed with "no handler for method 'len' on variable 'row'".
/// (Surfaced by kata #48 Rotate Image's matrix display over a `ref`-passed
/// `Vec[Vec[i64]]`.) The direct double-index form `m[i][j]` already worked;
/// this covers the let-bound-row form that the fix repaired.
#[test]
fn ref_param_nested_vec_row_binding_dispatches() {
    let src = "fn row_sum(m: ref Vec[Vec[i64]], i: i64) -> i64 {\n\
                   \x20   let row = ref m[i];\n\
                   \x20   let mut s = 0i64;\n\
                   \x20   let mut j = 0i64;\n\
                   \x20   let n = row.len();\n\
                   \x20   while j < n { s = s + row[j]; j = j + 1i64; }\n\
                   \x20   s\n\
                   }\n\
                   fn main() {\n\
                   \x20   let mut m: Vec[Vec[i64]] = Vec.new();\n\
                   \x20   let mut a: Vec[i64] = Vec.new(); a.push(1i64); a.push(2i64); a.push(3i64);\n\
                   \x20   let mut b: Vec[i64] = Vec.new(); b.push(4i64); b.push(5i64); b.push(6i64);\n\
                   \x20   m.push(a); m.push(b);\n\
                   \x20   println(f\"{row_sum(m, 0i64) + row_sum(m, 1i64)}\");\n\
                   }\n";
    assert_eq!(run_program(src).as_deref(), Some("21\n"));
}

/// The `mut ref` sibling of the above: a row bound out of a `mut ref
/// Vec[Vec[i64]]` must dispatch the same way (the fix peels `MutRef` too).
#[test]
fn mut_ref_param_nested_vec_row_binding_dispatches() {
    let src = "fn first_len(m: mut ref Vec[Vec[i64]]) -> i64 {\n\
                   \x20   let row = ref m[0i64];\n\
                   \x20   row.len()\n\
                   }\n\
                   fn main() {\n\
                   \x20   let mut m: Vec[Vec[i64]] = Vec.new();\n\
                   \x20   let mut a: Vec[i64] = Vec.new(); a.push(7i64); a.push(8i64);\n\
                   \x20   m.push(a);\n\
                   \x20   println(f\"{first_len(mut m)}\");\n\
                   }\n";
    assert_eq!(run_program(src).as_deref(), Some("2\n"));
}

/// Regression (B-2026-07-03-8): a trait DEFAULT method is callable on an
/// implementor that does not re-implement it, and behaves end-to-end under
/// `karac build`. The desugar pass `synthesize_trait_default_methods`
/// copies each non-overridden default body into the impl, so codegen emits
/// it as an ordinary `Type.method` fn. Covers a pure default (empty impl),
/// a default that calls a required method, a default that calls another
/// default, and an override taking precedence over the default.
#[test]
fn e2e_trait_default_methods_dispatch_on_implementor() {
    if let Some(out) = run_program(
        "trait T {\n\
             \x20   fn base(self) -> i64;\n\
             \x20   fn plus_one(self) -> i64 { self.base() + 1 }\n\
             \x20   fn plus_two(self) -> i64 { self.plus_one() + 1 }\n\
             \x20   fn constant(self) -> i64 { 42 }\n\
             }\n\
             struct A { n: i64 }\n\
             struct B { n: i64 }\n\
             impl T for A { fn base(self) -> i64 { self.n } }\n\
             impl T for B {\n\
             \x20   fn base(self) -> i64 { self.n }\n\
             \x20   fn constant(self) -> i64 { 100 }\n\
             }\n\
             fn main() {\n\
             \x20   let a = A { n: 10 };\n\
             \x20   println(f\"{a.plus_one()}\");\n\
             \x20   let a2 = A { n: 10 };\n\
             \x20   println(f\"{a2.plus_two()}\");\n\
             \x20   let a3 = A { n: 10 };\n\
             \x20   println(f\"{a3.constant()}\");\n\
             \x20   let b = B { n: 20 };\n\
             \x20   println(f\"{b.plus_two()}\");\n\
             \x20   let b2 = B { n: 20 };\n\
             \x20   println(f\"{b2.constant()}\");\n\
             }",
    ) {
        // a: plus_one=11, plus_two=12, constant(default)=42
        // b: plus_two=22, constant(override)=100
        assert_eq!(out, "11\n12\n42\n22\n100\n");
    }
}

/// Regression (B-2026-07-03-10): a GENERIC trait's default method is
/// inherited onto an implementor end-to-end under `karac build`, with the
/// impl's trait-args substituted through the copied signature + body. The
/// desugar pass zips the trait's declared params against `impl Tr[Args]`
/// and rewrites every `T` in the copy, so codegen emits an ordinary
/// concrete `Type.method` fn. Covers TWO distinct concrete args (i64 vs
/// f64) of the same generic trait proving the substitution is per-impl —
/// before the fix the pass skipped generic traits entirely — plus a
/// `let picked: T` body annotation + default-calls-required (`choose`), a
/// `T.zero()` associated-fn path in a default body (`seed` -> `Cnt.zero`),
/// a default-calls-default chain (`make` -> `seed`), and an override
/// taking precedence (`tag`). Call results that return an aggregate are
/// bound to a temp before field access to sidestep the orthogonal
/// pre-existing `f().field` codegen miscompile (see the bug ledger).
#[test]
fn e2e_generic_trait_default_methods_dispatch_on_implementor() {
    if let Some(out) = run_program(
        "trait Chooser[T] {\n\
             \x20   fn base(ref self) -> T;\n\
             \x20   fn choose(ref self, alt: T, use_alt: bool) -> T {\n\
             \x20       let picked: T = if use_alt { alt } else { self.base() };\n\
             \x20       picked\n\
             \x20   }\n\
             }\n\
             struct IBox { v: i64 }\n\
             struct FBox { v: f64 }\n\
             impl Chooser[i64] for IBox { fn base(ref self) -> i64 { self.v } }\n\
             impl Chooser[f64] for FBox { fn base(ref self) -> f64 { self.v } }\n\
             trait Zeroish { fn zero() -> Self; }\n\
             struct Cnt { n: i64 }\n\
             impl Zeroish for Cnt { fn zero() -> Cnt { Cnt { n: 7 } } }\n\
             trait Maker[T: Zeroish] {\n\
             \x20   fn seed(ref self) -> T { T.zero() }\n\
             \x20   fn make(ref self) -> T { self.seed() }\n\
             \x20   fn tag(ref self) -> i64 { 0 }\n\
             }\n\
             struct Gen {}\n\
             impl Maker[Cnt] for Gen { fn tag(ref self) -> i64 { 9 } }\n\
             fn main() {\n\
             \x20   let a = IBox { v: 7 };\n\
             \x20   let b = FBox { v: 2.5 };\n\
             \x20   println(f\"{a.choose(99, false)}\");\n\
             \x20   println(f\"{a.choose(99, true)}\");\n\
             \x20   println(f\"{b.choose(1.5, false)}\");\n\
             \x20   println(f\"{b.choose(1.5, true)}\");\n\
             \x20   let g = Gen {};\n\
             \x20   let m = g.make();\n\
             \x20   println(f\"{m.n}\");\n\
             \x20   let s = g.seed();\n\
             \x20   println(f\"{s.n}\");\n\
             \x20   println(f\"{g.tag()}\");\n\
             }",
    ) {
        // i64 default: base=7 / alt=99; f64 default: base=2.5 / alt=1.5.
        // make->seed->Cnt.zero()=7; seed=7; tag override=9.
        assert_eq!(out, "7\n99\n2.5\n1.5\n7\n7\n9\n");
    }
}

/// B-2026-08-12-24 — inside a generic impl, `let`-binding a `T`-typed
/// struct FIELD and then calling a trait method on that local failed the
/// whole build: `codegen: no handler for method 'describe' on variable
/// 'a'`, on a program `karac check` accepts and the interpreter runs.
///
/// A GENERIC PARAMETER REACHES THE BINDING RECORDER UNDER TWO SPELLINGS,
/// and only one was handled — which is why the boundary looked so strange.
/// A `T`-typed PARAM infers `Type::Named { name: "T" }` and records into
/// `pattern_binding_types`; a `T`-typed FIELD READ infers
/// `Type::TypeParam("T")` and fell through every arm of
/// `bind_pattern_types`, so codegen got NO binding type and had no
/// concrete receiver to dispatch on. The monomorph never lost the type —
/// it was never told.
///
/// Covers the shapes the boundary distinguishes, plus the two the filing
/// did not reach:
///
///   * `let a = self.cells[0];` — the filed repro (field read to a local)
///   * `let a = self.get(i)?;` — the ORIGINAL probe's shape, one level out
///   * `match self.get(i) { Ok(v) => v.describe() }` — the match-arm
///     sibling, which has its own recorder
///     (`record_pattern_binding_surface_types`) with the same hole
///
/// TWO MONOMORPHS, a scalar (`i64`) and a user struct (`P`), because
/// recording the param NAME is only right if the subst resolves it per
/// mono — one instantiation would pass with a hardcoded answer.
#[test]
fn e2e_generic_impl_local_bound_from_t_field_dispatches() {
    let Some(out) = run_program(
            "trait Zero { fn describe(ref self) -> String; }\n\
             impl Zero for i64 { fn describe(ref self) -> String { return f\"i64:{self}\"; } }\n\
             struct P { n: i64 }\n\
             impl Zero for P { fn describe(ref self) -> String { return f\"P:{self.n}\"; } }\n\
             struct Grid[T] { cells: Vec[T] }\n\
             impl[T: Zero] Grid[T] {\n\
             \x20   fn get(ref self, i: i64) -> Result[T, String] {\n\
             \x20       if i < 0 { return Result.Err(\"oob\"); }\n\
             \x20       return Result.Ok(self.cells[i]);\n\
             \x20   }\n\
             \x20   fn show(ref self, i: i64) -> Result[String, String] {\n\
             \x20       let a = self.get(i)?;\n\
             \x20       return Result.Ok(a.describe());\n\
             \x20   }\n\
             \x20   fn show_match(ref self, i: i64) -> String {\n\
             \x20       match self.get(i) {\n\
             \x20           Result.Ok(v) => { return v.describe(); }\n\
             \x20           Result.Err(e) => { return f\"err:{e}\"; }\n\
             \x20       }\n\
             \x20   }\n\
             \x20   fn direct(ref self) -> String { let a = ref self.cells[0]; return a.describe(); }\n\
             }\n\
             fn main() {\n\
             \x20   let mut cs: Vec[i64] = Vec.new();\n\
             \x20   cs.push(7);\n\
             \x20   let g: Grid[i64] = Grid { cells: cs };\n\
             \x20   match g.show(0) { Result.Ok(s) => println(s), Result.Err(e) => println(e) }\n\
             \x20   println(g.show_match(0));\n\
             \x20   println(g.direct());\n\
             \x20   let mut ps: Vec[P] = Vec.new();\n\
             \x20   ps.push(P { n: 5 });\n\
             \x20   let h: Grid[P] = Grid { cells: ps };\n\
             \x20   match h.show(0) { Result.Ok(s) => println(s), Result.Err(e) => println(e) }\n\
             \x20   println(h.show_match(0));\n\
             \x20   println(h.direct());\n\
             }",
        ) else {
            return;
        };
    // Matches `karac run --interp` on the identical source.
    assert_eq!(out, "i64:7\ni64:7\ni64:7\nP:5\nP:5\nP:5\n");
}

/// B-2026-07-03-11: dispatch a trait method called through a GENERIC
/// TYPE-PARAMETER BOUND under `karac build`. `fn use_it[X: Tagged](x: X)`
/// calling `x.tag()` used to die with "no handler for method tag on
/// variable x (method dispatch fell through)" — codegen had no arm for a
/// receiver whose type is a bound generic param. The mono param prologue
/// now registers `x` under the CONCRETE impl type (via the name-level
/// `type_subst_names`), so `inferred_receiver_type` resolves `X.tag` to
/// `A.tag` / `B.tag`. Also guards the mangle-collision half: `A` and `B`
/// both lower to `{i64}`, so `use_it$A` / `use_it$B` would share one
/// symbol (the second reusing the first's body) without the per-mono
/// concrete-name mangle axis — `use_it(b)` would print A's `18`-shaped
/// result. Covers method args and a nested bound-param pass-through.
#[test]
fn e2e_generic_bound_trait_method_dispatch() {
    if let Some(out) = run_program(
        "trait Tagged {\n\
             \x20   fn tag(self) -> i64;\n\
             \x20   fn combine(self, other: i64) -> i64;\n\
             }\n\
             struct A { v: i64 }\n\
             struct B { v: i64 }\n\
             impl Tagged for A {\n\
             \x20   fn tag(self) -> i64 { 10 }\n\
             \x20   fn combine(self, other: i64) -> i64 { self.v + other }\n\
             }\n\
             impl Tagged for B {\n\
             \x20   fn tag(self) -> i64 { 20 }\n\
             \x20   fn combine(self, other: i64) -> i64 { self.v * other }\n\
             }\n\
             fn use_it[X: Tagged](x: X) -> i64 { x.tag() + x.combine(3) }\n\
             fn forward[Y: Tagged](y: Y) -> i64 { use_it(y) }\n\
             fn main() {\n\
             \x20   println(f\"{use_it(A { v: 5 })}\");\n\
             \x20   println(f\"{use_it(B { v: 7 })}\");\n\
             \x20   println(f\"{forward(A { v: 2 })}\");\n\
             }",
    ) {
        // A: 10 + (5+3) = 18; B: 20 + (7*3) = 41; forward(A): 10 + (2+3) = 15
        assert_eq!(out, "18\n41\n15\n");
    }
}

/// B-2026-07-06-5 (blanket `impl Trait for Vec[i64]`): a user surface-trait
/// impl over the builtin `Vec` with LOOP bodies (`for x in self { … }`),
/// reached BOTH directly and through TWO bound-generic monomorphizations
/// (`callsum[C: Redu]` / `callprod[C: Redu]`). The two-loop-method mono case
/// previously SIGTRAPped (exit 133) — the reported "mono symbol collision"
/// was actually a `for x in self` double-free: `self` (a `ref Vec`
/// receiver, `SelfValue`) fell through the for-loop dispatch to the
/// value path, which materialize-iterate-DROPS the borrowed Vec. Now the
/// `SelfValue` arm routes to the non-dropping `compile_for_vec_var`, so
/// direct + both-mono + a single mono combining both methods all match
/// `karac run`.
#[test]
fn e2e_blanket_vec_impl_loop_body_mono_dispatch() {
    if let Some(out) = run_program(
        "trait Redu {\n\
             \x20   fn tsum(ref self) -> i64;\n\
             \x20   fn tprod(ref self) -> i64;\n\
             }\n\
             impl Redu for Vec[i64] {\n\
             \x20   fn tsum(ref self) -> i64 { let mut a = 0; for x in self { a = a + x; } a }\n\
             \x20   fn tprod(ref self) -> i64 { let mut a = 1; for x in self { a = a * x; } a }\n\
             }\n\
             fn callsum[C: Redu](c: ref C) -> i64 { c.tsum() }\n\
             fn callprod[C: Redu](c: ref C) -> i64 { c.tprod() }\n\
             fn both[C: Redu](c: ref C) -> i64 { c.tsum() + c.tprod() }\n\
             fn main() {\n\
             \x20   let mut v = Vec.new();\n\
             \x20   v.push(1); v.push(2); v.push(3); v.push(4);\n\
             \x20   println(f\"{v.tsum()}\");\n\
             \x20   println(f\"{callsum(v)}\");\n\
             \x20   println(f\"{callprod(v)}\");\n\
             \x20   println(f\"{both(v)}\");\n\
             }",
    ) {
        // sum=1+2+3+4=10, prod=24, both=34 — direct, two monos, combined mono.
        assert_eq!(out, "10\n10\n24\n34\n");
    }
}

/// B-2026-07-03-5: a user-defined trait impl on a PRIMITIVE integer/float
/// target (`impl Tag for u8 { ... }`) is dispatched end-to-end for a
/// DIRECT value-receiver call (`x.tag()`). Pre-fix the impl never
/// registered (the lowered primitive target fell through env_add_impl's
/// `_ => return`), so the call errored "no method 'tag' on type 'u8'" at
/// typecheck under `build`. Each width has a DISTINCT impl returning a
/// distinguishing value, so this asserts the CORRECT per-width impl is
/// selected (not the `i64`-keyed one shadowing every narrow width — the
/// bug that the multi-impl case would otherwise hit). Covers all eight
/// scalar widths i8/i16/i32/u8/u16/u32/f32/f64. The generic-BOUND form
/// (`fn f[T: Tag](x: T) { x.tag() }`) is a deeper separate surface tracked
/// as its own ledger entry — this covers the direct-call scope B-5 fixes.
#[test]
fn e2e_primitive_trait_impl_direct_dispatch() {
    if let Some(out) = run_program(
        "trait Tag { fn tag(self) -> i64; }\n\
             impl Tag for i8  { fn tag(self) -> i64 { -8 } }\n\
             impl Tag for i16 { fn tag(self) -> i64 { -16 } }\n\
             impl Tag for i32 { fn tag(self) -> i64 { -32 } }\n\
             impl Tag for u8  { fn tag(self) -> i64 { 8 } }\n\
             impl Tag for u16 { fn tag(self) -> i64 { 16 } }\n\
             impl Tag for u32 { fn tag(self) -> i64 { 32 } }\n\
             impl Tag for f32 { fn tag(self) -> i64 { 320 } }\n\
             impl Tag for f64 { fn tag(self) -> i64 { 640 } }\n\
             fn main() {\n\
             \x20   let a: i8 = 1; let b: i16 = 1; let c: i32 = 1;\n\
             \x20   let d: u8 = 1; let e: u16 = 1; let f: u32 = 1;\n\
             \x20   let g: f32 = 1.0; let h: f64 = 1.0;\n\
             \x20   println(a.tag()); println(b.tag()); println(c.tag());\n\
             \x20   println(d.tag()); println(e.tag()); println(f.tag());\n\
             \x20   println(g.tag()); println(h.tag());\n\
             }",
    ) {
        assert_eq!(out, "-8\n-16\n-32\n8\n16\n32\n320\n640\n");
    }
}

/// B-2026-07-03-24: a generic BOUND over a primitive trait impl
/// (`fn tag_it[T: Tag](x: T) -> i64 { x.tag() }`) dispatches to the CORRECT
/// per-width impl under codegen. Narrow ints are widened to i64 before the
/// call, so `infer_type_args` bound `T` to i64 for every narrow width and
/// all instantiations mangled to `tag_it$i64` — the second reused the
/// first's body, dispatching every width's `x.tag()` to whichever impl
/// compiled first (`8 8 8` for i8/i16/i32). The mangle now appends the
/// concrete primitive NAME from `subst_names`, so each width is a distinct
/// mono that dispatches correctly. Distinguishing per-width impls across
/// all eight scalar widths (incl. the f32-vs-f64 case the LLVM token would
/// also erase for unsigned/narrow spellings).
#[test]
fn e2e_generic_bound_primitive_dispatch() {
    if let Some(out) = run_program(
        "trait Tag { fn tag(self) -> i64; }\n\
             impl Tag for i8  { fn tag(self) -> i64 { -8 } }\n\
             impl Tag for i16 { fn tag(self) -> i64 { -16 } }\n\
             impl Tag for i32 { fn tag(self) -> i64 { -32 } }\n\
             impl Tag for u8  { fn tag(self) -> i64 { 8 } }\n\
             impl Tag for u16 { fn tag(self) -> i64 { 16 } }\n\
             impl Tag for u32 { fn tag(self) -> i64 { 32 } }\n\
             impl Tag for f32 { fn tag(self) -> i64 { 320 } }\n\
             impl Tag for f64 { fn tag(self) -> i64 { 640 } }\n\
             fn tag_it[T: Tag](x: T) -> i64 { x.tag() }\n\
             fn main() {\n\
             \x20   let a: i8 = 1; let b: i16 = 1; let c: i32 = 1;\n\
             \x20   let d: u8 = 1; let e: u16 = 1; let f: u32 = 1;\n\
             \x20   let g: f32 = 1.0; let h: f64 = 1.0;\n\
             \x20   println(tag_it(a)); println(tag_it(b)); println(tag_it(c));\n\
             \x20   println(tag_it(d)); println(tag_it(e)); println(tag_it(f));\n\
             \x20   println(tag_it(g)); println(tag_it(h));\n\
             }",
    ) {
        assert_eq!(out, "-8\n-16\n-32\n8\n16\n32\n320\n640\n");
    }
}

// ── User `impl Display` dispatch (codegen) ──

#[test]
fn e2e_user_impl_display_dispatches_to_to_string() {
    // A user `impl Display { fn to_string }` must win over the built-in
    // renderer in every Display position — `.to_string()`, `f"{x}"`, and
    // `println(x)` — for unit enums, tuple-variant enums, and structs. The
    // unit-enum case returns a string LITERAL (non-owning, cap==0), which
    // exercises the `cap > 0` free guard in `emit_write_and_free_string`
    // (an unconditional free of the literal aborts). GAP-W4.
    if let Some(out) = run_program(
            "enum Color { Red, Green, Blue }\n\
             impl Display for Color { fn to_string(ref self) -> String { match self { Red => \"red\", Green => \"green\", Blue => \"blue\" } } }\n\
             enum Msg { Info(i64), Quit }\n\
             impl Display for Msg { fn to_string(ref self) -> String { match self { Info(c) => f\"info#{c}\", Quit => \"quit\" } } }\n\
             struct Point { x: i64, y: i64 }\n\
             impl Display for Point { fn to_string(ref self) -> String { f\"({self.x}, {self.y})\" } }\n\
             fn main() {\n\
                 let c = Color.Green; println(c.to_string()); println(f\"c={c}\"); println(c);\n\
                 let m = Msg.Info(7); println(f\"m={m}\"); println(m);\n\
                 let p = Point { x: 3, y: 4 }; println(f\"p={p}\"); println(p);\n\
             }",
        ) {
            assert_eq!(
                out,
                "green\nc=green\ngreen\nm=info#7\ninfo#7\np=(3, 4)\n(3, 4)\n"
            );
        }
}

#[test]
fn e2e_unsigned_whole_domain_match_dispatches() {
    // The runtime half of B-2026-08-20-6. The typechecker no longer refuses
    // a `u64` match that partitions its whole domain — this pins that the
    // arms then dispatch CORRECTLY, which the wrapped carrier makes
    // non-obvious: the upper arm's bounds are negative on the carrier, so a
    // comparison done in the wrong space would send every value one way.
    //
    // The two values either side of 2^63 are the whole point; the rest
    // bracket them.
    if let Some(out) = run_program(
        "fn half(n: u64) -> String {\n\
             match n {\n\
             0u64..=9223372036854775807u64 => return \"lo\",\n\
             9223372036854775808u64..=18446744073709551615u64 => return \"hi\",\n\
             }\n\
             }\n\
             fn main() {\n\
             println(half(0u64));\n\
             println(half(9223372036854775807u64));\n\
             println(half(9223372036854775808u64));\n\
             println(half(18446744073709551615u64));\n\
             }",
    ) {
        assert_eq!(out, "lo\nlo\nhi\nhi\n");
    }
}

#[test]
fn e2e_wp_result_binding_string_method_dispatch_tail() {
    // B-2026-07-31-20: a heap-typed wp-result binding must register its
    // method-dispatch metadata. The wp call used to type as a silent
    // `Error`, so codegen had nothing to register and `s.len()`
    // loud-bailed ("no handler for method 'len'"). Tail-String shape —
    // no `return` involved.
    if let Some(out) = run_program(&format!(
        "{WP_PREAMBLE}\
             fn f() -> i64 with reads(Ctr) {{\n\
                 let s = with_provider[Ctr](InMem {{ n: 42 }}, || {{ f\"v-{{read()}}\" }});\n\
                 s.len()\n\
             }}\n\
             fn main() with reads(Ctr) {{ println(f\"{{f()}}\"); }}",
    )) {
        assert_eq!(out, "4\n");
    }
}

#[test]
fn e2e_wp_result_binding_string_method_dispatch_return() {
    // The closure-return sibling: the String arrives via a retargeted
    // closure-scoped `return` (B-2026-07-31-16) and dispatch on the
    // binding must still work.
    if let Some(out) = run_program(&format!(
            "{WP_PREAMBLE}\
             fn f() -> i64 with reads(Ctr) {{\n\
                 let s = with_provider[Ctr](InMem {{ n: 42 }}, || {{ return f\"v-{{read()}}\"; }});\n\
                 s.len()\n\
             }}\n\
             fn main() with reads(Ctr) {{ println(f\"{{f()}}\"); }}",
        )) {
            assert_eq!(out, "4\n");
        }
}

#[test]
fn e2e_wp_result_binding_vec_method_dispatch() {
    // Vec sibling: element access + len on a wp-result Vec binding.
    if let Some(out) = run_program(&format!(
        "{WP_PREAMBLE}\
             fn f() -> i64 with reads(Ctr) {{\n\
                 let v = with_provider[Ctr](InMem {{ n: 3 }}, || {{\n\
                     let mut out = Vec.new();\n\
                     out.push(read());\n\
                     out.push(read() * 2);\n\
                     out\n\
                 }});\n\
                 v.len() * 100 + v[0] + v[1]\n\
             }}\n\
             fn main() with reads(Ctr) {{ println(f\"{{f()}}\"); }}",
    )) {
        assert_eq!(out, "209\n");
    }
}

// ── String-literal `match` dispatch (selfhost-lexer-profile.md #1 lever) ──

/// A `match` over ≥4 string literals lowers to the switch tree
/// (`switch len → switch first-byte → residual memcmp`) instead of the
/// linear `memcmp` cascade. Pin the IR shape: the `match.strdisp.*` blocks
/// and a `switch i64` on the length must appear.
#[test]
fn test_string_match_lowers_to_dispatch_switch() {
    let ir = ir_for(
        "fn kw(s: String) -> i64 {\n\
                 match s {\n\
                     \"fn\" => 1,\n\
                     \"for\" => 2,\n\
                     \"struct\" => 3,\n\
                     \"static\" => 4,\n\
                     \"let\" => 5,\n\
                     other => 0,\n\
                 }\n\
             }",
    );
    assert!(
        ir.contains("match.strdisp.len"),
        "≥4-arm string match must build the length-switch dispatch tree:\n{ir}"
    );
    assert!(
        ir.contains("switch i64"),
        "dispatch must switch on the scrutinee length:\n{ir}"
    );
}

/// End-to-end behavioral equivalence: the switch tree must classify
/// exactly as the cascade would, across the tricky cases — two keywords
/// sharing length+first-byte (forces the residual `memcmp`), a length-1
/// keyword, the empty string, and several non-matching inputs routed to
/// the catch-all.
#[test]
fn e2e_string_match_dispatch_matches_cascade_semantics() {
    if let Some(out) = run_program(
        "fn classify(s: String) -> i64 {\n\
                 match s {\n\
                     \"fn\" => 1,\n\
                     \"for\" => 2,\n\
                     \"fun\" => 3,\n\
                     \"struct\" => 4,\n\
                     \"static\" => 5,\n\
                     \"x\" => 6,\n\
                     \"\" => 7,\n\
                     other => 99,\n\
                 }\n\
             }\n\
             fn check(s: String) { let r = classify(s); println(f\"{r}\"); }\n\
             fn main() {\n\
                 check(\"fn\");\n\
                 check(\"for\");\n\
                 check(\"fun\");\n\
                 check(\"struct\");\n\
                 check(\"static\");\n\
                 check(\"x\");\n\
                 check(\"\");\n\
                 check(\"forge\");\n\
                 check(\"fo\");\n\
                 check(\"y\");\n\
                 check(\"structs\");\n\
             }",
    ) {
        assert_eq!(out, "1\n2\n3\n4\n5\n6\n7\n99\n99\n99\n99\n");
    }
}

/// B-2026-08-14-17 — indexing a tensor-valued TEMPORARY compiles, at parity
/// with the same read through a named binding.
///
/// `(t * 2)[0]` typechecked and ran under `--interp` but could not be
/// built, in three different messages depending on how the temporary was
/// produced. One cause: the parser stamps a postfix expression with its
/// RECEIVER's span, so the `Index` and its object share a key in
/// `expr_types` and the index's scalar element type wins. Everything
/// downstream that decides "is this a tensor" reads a table derived from
/// that map, so `compile_expr` on the `Binary` skipped the tensor lowering
/// and lowered two control-block POINTERS as scalar arithmetic.
///
/// Every shape is here because each reached a different guard: arithmetic
/// ("Binary op Mul: left operand has non-comparable type PointerType(…)"),
/// a bare constructor ("Index operator applied to non-array type"), a unary
/// ("Cannot convert PointerType(…) to float"), and a nested temporary. The
/// last line is the named binding, which always worked — it is the control,
/// and the value the other spellings have to match.
#[test]
fn test_e2e_index_a_tensor_temporary() {
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let t: Tensor[f64, [2]] = Tensor.from([1.0, 2.0]);\n\
                     println((t * 2)[0]);\n\
                     println((t + t)[1]);\n\
                     println((0.0 - t)[0]);\n\
                     println(Tensor.from([5.0, 6.0])[1]);\n\
                     println(((t + t) * 2)[0]);\n\
                     let m: Tensor[i64, [2, 2]] = Tensor.from([[1, 2], [3, 4]]);\n\
                     println((m + m)[1, 0]);\n\
                     let r = t * 2;\n\
                     println(r[0]);\n\
                 }"
        )
        .as_deref(),
        Some("2\n4\n-1\n6\n4\n6\n2\n"),
    );
}

#[test]
fn test_e2e_user_trait_impl_on_map_and_set_dispatches() {
    // B-2026-08-12-34 — DIRECT dispatch of a user trait impl on `Map` /
    // `Set`, end to end. Unlike `String` (B-2026-08-12-32) these are
    // `Type::Named`, so registration always worked and only dispatch was
    // missing: the typechecker early-returned into the builtin surface, and
    // codegen loud-failed with "codegen: Map.<m> not yet implemented".
    //
    // `m.len()` / `s.len()` alongside prove the builtin surface keeps
    // precedence — the builtin returns `i64` while the shadowing-name test
    // in tests/typechecker.rs covers the collision case.
    //
    // The trait-BOUND spelling is deliberately ABSENT. `Map`'s bound path
    // is a known, pre-existing run-vs-build divergence (check-green, both
    // backends dead) that this row does not fix; routing it through the
    // Vec/String user-impl fallthrough was implemented and REVERTED because
    // it returns the WRONG impl when a second one exists — with both a
    // `Map` and a `Set` impl in scope, `show(set)` printed the Map's
    // answer. A wrong answer is worse than the build error, so the build
    // error stays. See the follow-up row.
    assert_eq!(
            run_program(
                "trait Zero { fn describe(ref self) -> String; }\n\
                 impl Zero for Map[String, i64] { fn describe(ref self) -> String { return f\"m\"; } }\n\
                 impl Zero for Set[i64] { fn describe(ref self) -> String { return f\"s\"; } }\n\
                 fn main() {\n\
                     let mut m: Map[String, i64] = Map.new();\n\
                     m.insert(\"a\", 1);\n\
                     println(m.describe());\n\
                     println(m.len());\n\
                     let mut s: Set[i64] = Set.new();\n\
                     s.insert(7);\n\
                     println(s.describe());\n\
                     println(s.len());\n\
                 }"
            )
            .as_deref(),
            Some("m\n1\ns\n1\n"),
        );
}

#[test]
fn test_e2e_same_head_impls_dispatch_to_their_own_target() {
    // B-2026-08-13-8 — two impls of one trait on two instantiations of one
    // type. Both wanted the symbol `Vec.describe`; LLVM renamed the second,
    // so `get_function` handed the FIRST to every receiver and the compiled
    // program printed `VEC-I64` twice. The interpreter, keying the same
    // erased name into its env, kept the LAST and printed `VEC-STR` twice —
    // so this was a miscompile AND a run-vs-build divergence at once, with
    // `karac check` accepting the program because the typechecker's impl
    // table is keyed on `(name, args)` and had resolved both correctly.
    //
    // The receivers are interleaved rather than grouped, and `a.len()` sits
    // between them, so a fix that merely made the LAST call correct would
    // still fail here.
    //
    // The `Box` pair earns its lines: the scope is EVERY generic type, not
    // just the builtin containers the row was found on. A user struct
    // reproduces it identically, which is what rules out a
    // container-dispatcher explanation.
    assert_eq!(
            run_program(
                "struct Box[T] { v: T }\n\
                 trait Zero { fn describe(ref self) -> String; }\n\
                 impl Zero for Vec[i64] { fn describe(ref self) -> String { return f\"VEC-I64\"; } }\n\
                 impl Zero for Vec[String] { fn describe(ref self) -> String { return f\"VEC-STR\"; } }\n\
                 impl Zero for Box[i64] { fn describe(ref self) -> String { return f\"BOX-I64\"; } }\n\
                 impl Zero for Box[String] { fn describe(ref self) -> String { return f\"BOX-STR\"; } }\n\
                 fn main() {\n\
                     let mut a: Vec[i64] = Vec.new();\n\
                     a.push(1);\n\
                     let mut b: Vec[String] = Vec.new();\n\
                     b.push(\"x\");\n\
                     println(a.describe());\n\
                     println(a.len());\n\
                     println(b.describe());\n\
                     println(a.describe());\n\
                     let p: Box[i64] = Box { v: 1 };\n\
                     let q: Box[String] = Box { v: \"s\" };\n\
                     println(p.describe());\n\
                     println(q.describe());\n\
                 }"
            )
            .as_deref(),
            Some("VEC-I64\n1\nVEC-STR\nVEC-I64\nBOX-I64\nBOX-STR\n"),
        );
}

#[test]
fn test_e2e_two_from_impls_dispatch_by_source_type() {
    // B-2026-08-27-1 — the CODEGEN half, and the one that made this severe.
    // Two `impl From[X] for AppError` both wanted the symbol
    // `AppError.from`; LLVM renamed the second, so `get_function` handed
    // the FIRST to every conversion site. The interpreter kept the LAST, so
    // the backends ran different functions on the same program — and
    // `karac check` accepted it, because `find_from_impl` had already
    // matched on the source type. Only the NAME was lossy.
    //
    // The payloads have deliberately different shapes (`String` vs `i64`),
    // which is what turns a wrong dispatch into a type confusion: feeding a
    // `ParseError` to the `DbError` impl made the compiled program read the
    // `String`'s three words as three `i64` fields and print a raw heap
    // pointer — an ASLR disclosure, with exit status 0 and no diagnostic.
    //
    // Both impl ORDERS run, because a single-order fixture passes on one
    // backend by luck: the two backends were wrong in OPPOSITE directions,
    // so whichever order suits the backend under test hides the bug.
    let expect = Some("PARSE:p\nDB:7\nPARSE:p\nDB:7\nPARSE:p\nDB:7\n");
    assert_eq!(run_program(&two_from_impls_src(true)).as_deref(), expect);
    assert_eq!(run_program(&two_from_impls_src(false)).as_deref(), expect);
}

#[test]
fn test_e2e_same_head_impls_dispatch_through_a_chain() {
    // B-2026-08-13-8 — the chained-receiver case, which is where a
    // span-keyed side table normally goes wrong: the parser sets
    // `MethodCall.span == receiver.span`, so `a.describe().len()` gives the
    // inner and outer calls ONE span. Every sibling table lives with that by
    // re-checking the method segment afterwards; this one carries the method
    // name in the key instead, so the inner link cannot read the outer
    // call's entry at all. Pinned here because a table that got this wrong
    // would still pass the direct-call test above.
    assert_eq!(
            run_program(
                "trait Zero { fn describe(ref self) -> String; }\n\
                 impl Zero for Vec[i64] { fn describe(ref self) -> String { return f\"I64\"; } }\n\
                 impl Zero for Vec[String] { fn describe(ref self) -> String { return f\"STRING\"; } }\n\
                 fn main() {\n\
                     let mut a: Vec[i64] = Vec.new();\n\
                     a.push(1);\n\
                     let mut b: Vec[String] = Vec.new();\n\
                     b.push(\"x\");\n\
                     println(a.describe().len());\n\
                     println(b.describe().to_uppercase());\n\
                 }"
            )
            .as_deref(),
            Some("3\nSTRING\n"),
        );
}

#[test]
fn test_e2e_generic_impl_on_builtin_container_dispatches() {
    // B-2026-08-13-8 half A — `impl[T] Zero for Vec[T]` was check-green and
    // interp-green with the build dead ("Vec/String method 'describe' is not
    // yet supported in codegen"). The generic machinery was never the
    // problem: the identical impl on a USER struct worked on every surface,
    // because no builtin dispatcher intercepts that receiver first.
    //
    // A generic impl method is routed to the monomorphizer and mangled per
    // instantiation, so it owns NO unmangled `Vec.describe` symbol. The four
    // builtin container dispatchers each decided whether to loud-fail or
    // fall through by asking `module.get_function("Vec.describe")` alone,
    // which is always None for such a method — so they all loud-failed just
    // before the generic dispatch that reads exactly that key. All four now
    // ask one shared helper that consults `generic_fns` too.
    //
    // Map and Set are here because they had the same hole; the row was only
    // ever reported against Vec.
    assert_eq!(
            run_program(
                "trait Zero { fn describe(ref self) -> String; }\n\
                 impl[T] Zero for Vec[T] { fn describe(ref self) -> String { return f\"VEC\"; } }\n\
                 impl[K, V] Zero for Map[K, V] { fn describe(ref self) -> String { return f\"MAP\"; } }\n\
                 impl[T] Zero for Set[T] { fn describe(ref self) -> String { return f\"SET\"; } }\n\
                 fn main() {\n\
                     let mut v: Vec[i64] = Vec.new();\n\
                     v.push(1);\n\
                     let mut w: Vec[String] = Vec.new();\n\
                     w.push(\"x\");\n\
                     let mut m: Map[String, i64] = Map.new();\n\
                     m.insert(\"k\", 1);\n\
                     let mut s: Set[i64] = Set.new();\n\
                     s.insert(7);\n\
                     println(v.describe());\n\
                     println(w.describe());\n\
                     println(m.describe());\n\
                     println(s.describe());\n\
                     println(v.len());\n\
                 }"
            )
            .as_deref(),
            Some("VEC\nVEC\nMAP\nSET\n1\n"),
        );
}

#[test]
fn test_e2e_user_trait_bound_call_over_builtin_containers_dispatches() {
    // B-2026-08-13-9 — a TRAIT-BOUND call (`fn show[T: Zero](x: ref T)`)
    // over a user-impl'd builtin container. Direct dispatch on all of these
    // receivers already worked; the bound spelling was the last one dead,
    // and the row's own warning is what this fixture is shaped around:
    //
    //   "Routing it through the Vec/String fallthrough was tried and
    //    REVERTED — it returns the WRONG impl once a second one exists."
    //
    // So FIVE impls are live at once and every receiver is exercised through
    // the bound. A body that serves more than one instantiation cannot
    // produce this output; it prints one impl's answer for several
    // receivers, which is what the earlier attempt did (`m s m m` where the
    // interpreter said `m s m s`).
    //
    // The cause was a mangle collision, not a missing dispatcher arm: `Map`,
    // `Set` and `Slice` all lower to `ptr`, so `show$ptr` served every
    // instantiation. `String`/`Vec` were never in that collision (the
    // `$<p>_ct_<token>` axis disambiguates them element-awarely) and are
    // here as the controls that say the fix did not disturb them.
    //
    // `outer` is the NESTED generic call: inside a monomorph the inner
    // call's LLVM-type binding is empty (the typechecker drops the
    // self-referential `T -> T`), so it appended nothing at all and both
    // outer monos shared one `inner`. `pair` covers two bound params in one
    // signature, and both slice spellings are here — a binding and an inline
    // range — because they resolve by different routes.
    //
    // Distinct from B-2026-08-13-8's fixture next door, which is the same
    // erasure on the ELEMENT axis (two impls under one head); this one is
    // the HEAD axis (one impl each on five different heads).
    assert_eq!(
            run_program(
                "trait Zero { fn describe(ref self) -> String; }\n\
                 impl Zero for Map[String, i64] { fn describe(ref self) -> String { return \"m\"; } }\n\
                 impl Zero for Set[i64] { fn describe(ref self) -> String { return \"s\"; } }\n\
                 impl Zero for Slice[i64] { fn describe(ref self) -> String { return \"l\"; } }\n\
                 impl Zero for Vec[i64] { fn describe(ref self) -> String { return \"v\"; } }\n\
                 impl Zero for String { fn describe(ref self) -> String { return \"t\"; } }\n\
                 fn show[T: Zero](x: ref T) -> String { return x.describe(); }\n\
                 fn outer[T: Zero](x: ref T) -> String { return show(x) + \"!\"; }\n\
                 fn pair[A: Zero, B: Zero](x: ref A, y: ref B) -> String {\n\
                     return x.describe() + y.describe();\n\
                 }\n\
                 fn main() {\n\
                     let mut m: Map[String, i64] = Map.new();\n\
                     m.insert(\"k\", 1);\n\
                     let mut s: Set[i64] = Set.new();\n\
                     s.insert(7);\n\
                     let mut v: Vec[i64] = Vec.new();\n\
                     v.push(3);\n\
                     v.push(4);\n\
                     let t: String = \"hi\";\n\
                     let sl = v[0..2];\n\
                     println(show(m) + show(s) + show(sl) + show(v) + show(t));\n\
                     println(show(v[0..2]));\n\
                     println(outer(m) + outer(s));\n\
                     println(pair(m, s) + pair(s, m));\n\
                     println(m.describe() + s.describe() + v.describe() + t.describe());\n\
                 }"
            )
            .as_deref(),
            Some("mslvt\nl\nm!s!\nmssm\nmsvt\n"),
        );
}

#[test]
fn test_e2e_user_trait_impl_on_slice_dispatches() {
    // B-2026-08-13-7 — DIRECT dispatch of a user trait impl on `Slice[T]`,
    // end to end. This one was reverted TWICE before landing, and the order
    // of work is the whole story: its typecheck and codegen halves were
    // implemented and worked on both compiled backends, but the interpreter
    // snapshots a `Value::Slice` receiver into a `Value::Array` before
    // dispatch, which renames it `Vec`, so `--interp` reported `no method`
    // on a program `karac build` ran. Naming the slice receiver had to land
    // first; the interpreter now skips that snapshot for exactly the names
    // the builtin surface does not answer.
    //
    // BOTH a `Slice` and a `Vec` impl are in scope on purpose. Every
    // single-impl probe passed for the sibling `Map`/`Set` attempt too, and
    // the wrong-impl selection only appeared once a second impl existed —
    // any retry in this family needs a two-impl fixture as its first check.
    // A slice variable is registered in `vec_elem_types` (it is built from
    // one), so the Vec dispatcher gets the call first and its fallthrough
    // has to discriminate rather than hand every receiver to whichever impl
    // it finds.
    //
    // `s.len()` alongside proves the builtin surface keeps precedence, and
    // `sub` proves a non-full-range window dispatches the same way.
    //
    // `take_mut` earns its line. A slice binding gets NO `var_type_names`
    // entry (`register_var_from_type_expr` files it under
    // `slice_elem_types` and returns early), so a `mut Slice[T]` param
    // receiver had no name to qualify dispatch with and both compiled
    // backends died on it — check-green and interp-green — while every other
    // slice spelling in this program worked. `inferred_receiver_type` now
    // falls back to `Slice` for a slice-registered binding. It runs on its
    // own `w` because a `mut Slice` of `v` would conflict with the immutable
    // windows still live above it — an ownership error, not a dispatch one.
    //
    // The trait-BOUND spelling is deliberately ABSENT — same reason as the
    // `Map`/`Set` twin above: `inferred_receiver_type` resolves one receiver
    // name per compiled generic body, so a bound call selects a single impl
    // for every instantiation. That needs per-receiver monomorphization, not
    // another fallthrough.
    assert_eq!(
            run_program(
                "trait Zero { fn describe(ref self) -> String; }\n\
                 impl Zero for Slice[i64] { fn describe(ref self) -> String { return f\"S{self.len()}\"; } }\n\
                 impl Zero for Vec[i64] { fn describe(ref self) -> String { return f\"V{self.len()}\"; } }\n\
                 fn via_param(s: ref Slice[i64]) -> String { return s.describe(); }\n\
                 fn take_mut(s: mut Slice[i64]) -> String { return s.describe(); }\n\
                 fn main() {\n\
                     let mut v: Vec[i64] = Vec.new();\n\
                     v.push(10);\n\
                     v.push(20);\n\
                     v.push(30);\n\
                     let s: Slice[i64] = v[..];\n\
                     println(s.describe());\n\
                     println(s.len());\n\
                     let sub: Slice[i64] = v[1..3];\n\
                     println(sub.describe());\n\
                     println(via_param(sub));\n\
                     println(v.describe());\n\
                     let mut w: Vec[i64] = Vec.new();\n\
                     w.push(1);\n\
                     w.push(2);\n\
                     println(take_mut(w.as_slice_mut()));\n\
                 }"
            )
            .as_deref(),
            Some("S3\n3\nS2\nS2\nV3\nS2\n"),
        );
}

#[test]
fn test_e2e_user_trait_impl_on_string_dispatches() {
    // B-2026-08-12-32 — the END-TO-END half. The typecheck fix alone was
    // measured check-green, interp-green and DEAD on both compiled backends
    // ("Vec/String method 'describe' is not yet supported in codegen"),
    // which is a strictly worse failure than the `no method` it replaced —
    // so this asserts the compiled output, not just that it builds.
    //
    // A `String` variable is registered in `vec_elem_types` (it shares Vec's
    // `{ptr,len,cap}` shape), so the call lands in the builtin Vec/String
    // dispatcher; the fix lets it fall through to user-impl dispatch when
    // `String.<method>` was emitted, exactly as the blanket-`Vec` case
    // already did.
    //
    // `s.len()` and the bound call are in the same program on purpose: the
    // builtin surface must keep working alongside the user impl, and the
    // generic `show` proves bound dispatch reaches the same function.
    assert_eq!(
            run_program(
                "trait Zero { fn describe(ref self) -> String; }\n\
                 impl Zero for String { fn describe(ref self) -> String { return f\"s:{self}\"; } }\n\
                 fn show[T: Zero](x: ref T) -> String { return x.describe(); }\n\
                 fn main() {\n\
                     let s: String = \"hi\";\n\
                     println(s.describe());\n\
                     println(show(s));\n\
                     println(s.len());\n\
                 }"
            )
            .as_deref(),
            Some("s:hi\ns:hi\n2\n"),
        );
}

#[test]
fn e2e_user_len_family_method_on_freshtemp_dispatches_to_user() {
    // B-2026-07-11-26: a USER method named `len` / `count` / `is_empty` on a
    // fresh-temp receiver (`make().count()`) must dispatch to the user impl,
    // not the collection/iterator `len`-family method-chain intercept. When
    // `count` joined that intercept (B-2026-07-11-9 gap 1) it collided with a
    // user `fn count(self)`: the intercept speculatively compiled the
    // receiver, found it wasn't a Vec/String struct, and fell through —
    // leaking the discarded temp (the ASAN sibling
    // `asan_freshtemp_shared_struct_method_no_double_free` guards the leak).
    // This gate locks the VALUE: the user method must run and return its
    // result, for a shared-struct and a plain-struct receiver, across all
    // three colliding names.
    if let Some(out) = run_program(
            "shared struct Bag { items: Vec[i64] }\n\
             impl Bag {\n\
                 fn count(self) -> i64 { self.items.len() * 10 }\n\
                 fn is_empty(self) -> bool { false }\n\
             }\n\
             struct Plain { n: i64 }\n\
             impl Plain { fn len(self) -> i64 { self.n + 7 } }\n\
             fn bag() -> Bag { let mut v: Vec[i64] = Vec.new(); v.push(1); v.push(2); Bag { items: v } }\n\
             fn plain() -> Plain { Plain { n: 5 } }\n\
             fn main() {\n\
                 println(bag().count());\n\
                 println(bag().is_empty());\n\
                 println(plain().len());\n\
             }",
        ) {
            assert_eq!(out, "20\nfalse\n12\n");
        }
}

// ── Phase 6 line 17 slice 9d — TcpStream / TcpListener close-on-drop ──
//
// Hand-rolled `@TcpStream.drop` / `@TcpListener.drop` LLVM bodies
// call `karac_runtime_tcp_close(self.fd)` and return void. The
// user-Drop wrapper machinery (Prereq.2-5) then invokes these
// bodies at scope exit. Together they close the kernel-side fd
// when a kara binding goes out of scope, replacing the previous
// "kernel reaps fds on process exit" leak.

/// `TcpStream.connect(addr)` lowers to the PARKED connect pair —
/// `connect_start` (non-blocking initiate) + a write-readiness park +
/// `connect_finish` (SO_ERROR) — and wraps the fd via the shared
/// `build_fd_construct_result` (the `tcp.connect.*` labels). Pins the
/// assoc-dispatch arm + the parked-connect extern wiring (so the upstream
/// connect suspends on the reactor instead of blocking it).
#[test]
fn test_ir_tcp_stream_connect_dispatches_to_runtime_ffi() {
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
        body.contains("call i64 @karac_runtime_tcp_connect_start(")
            && body.contains("call i64 @karac_runtime_tcp_connect_finish("),
        "connect should call the parked pair @karac_runtime_tcp_connect_start \
             + @karac_runtime_tcp_connect_finish; body was:\n{}",
        body
    );
    assert!(
        body.contains("tcp.connect.park") && body.contains("tcp.connect.final"),
        "connect should park on write-readiness then phi the final fd \
             (tcp.connect.park / tcp.connect.final); body was:\n{}",
        body
    );
    assert!(
        body.contains("tcp.connect.is_ok"),
        "connect should wrap its fd via build_fd_construct_result \
             (tcp.connect.is_ok branch); body was:\n{}",
        body
    );
}

/// Relay dogfood slice 3 — `TcpStream.try_clone()` dispatches to the
/// `karac_runtime_tcp_try_clone` FFI (a `dup(2)`) and wraps the new fd
/// via the shared `build_fd_construct_result` (the `tcp.try_clone.*`
/// labels). Pins the method-dispatch arm + extern wiring for the
/// full-duplex-splice primitive (`examples/relay/relay.kara`).
#[test]
fn test_ir_tcp_stream_try_clone_dispatches_to_runtime_ffi() {
    let ir = ir_for(
        r#"
fn main() {
    let s = TcpStream.connect("127.0.0.1:8080").unwrap();
    let c = s.try_clone().unwrap();
    println(s.fd);
    println(c.fd);
}
"#,
    );
    let body = function_body(&ir, "main").expect("main body");
    assert!(
        body.contains("call i64 @karac_runtime_tcp_try_clone("),
        "try_clone should call @karac_runtime_tcp_try_clone; body was:\n{}",
        body
    );
    assert!(
        body.contains("tcp.try_clone.is_ok"),
        "try_clone should wrap its fd via build_fd_construct_result \
             (tcp.try_clone.is_ok branch); body was:\n{}",
        body
    );
}

/// Relay dogfood slice 3 — `TcpStream.shutdown_write()` dispatches to the
/// `karac_runtime_tcp_shutdown` FFI (with `how = 1` = Write) and builds a
/// `Result[Unit, TcpError]` from the 0/-1 status via `build_unit_status_
/// result` (the `tcp.shutwr.*` labels). Pins the half-close primitive that
/// propagates EOF across the full-duplex splice (`examples/relay/relay.kara`).
#[test]
fn test_ir_tcp_stream_shutdown_write_dispatches_to_runtime_ffi() {
    let ir = ir_for(
        r#"
fn main() {
    let s = TcpStream.connect("127.0.0.1:8080").unwrap();
    let _ = s.shutdown_write();
    println(s.fd);
}
"#,
    );
    let body = function_body(&ir, "main").expect("main body");
    assert!(
        body.contains("call i32 @karac_runtime_tcp_shutdown("),
        "shutdown_write should call @karac_runtime_tcp_shutdown; body was:\n{}",
        body
    );
    assert!(
        body.contains("tcp.shutwr.is_ok"),
        "shutdown_write should build Result[Unit, TcpError] via \
             build_unit_status_result (tcp.shutwr.is_ok branch); body was:\n{}",
        body
    );
}

#[test]
fn test_e2e_struct_destructure_field_method_dispatch() {
    // The bound field must dispatch methods (was a hard codegen error).
    let src = format!(
            "{STRUCT_DESTRUCTURE_PRELUDE}fn main() {{\n    let Point {{ items, count }} = make();\n    println(items.len() + count);\n}}\n"
        );
    if let Some(out) = run_program(&src) {
        assert_eq!(out.trim(), "7");
    }
}

#[test]
fn test_e2e_nested_struct_pattern_dispatch() {
    // `let Outer { inner: Inner { data }, n } = make()` — a NESTED struct
    // pattern. `bind_pattern` allocates the nested leaf `data`, but its
    // dispatch side-tables were never registered, so `data.len()` failed
    // with "no handler for method 'len' on variable 'data'". The recursive
    // register_struct_pattern_dispatch closes that; the nested field's heap
    // is still freed once by the enclosing-field discard (no double-free).
    let out = run_program(
        r#"
struct Inner { data: Vec[i64] }
struct Outer { inner: Inner, n: i64 }
fn make() -> Outer {
    let mut v: Vec[i64] = Vec.new();
    v.push(10_i64);
    v.push(20_i64);
    return Outer { inner: Inner { data: v }, n: 5 };
}
fn main() {
    let Outer { inner: Inner { data }, n } = make();
    println(data.len() + n);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "7");
    }
}

#[test]
fn test_e2e_match_arm_map_set_method_dispatch() {
    // A `Map`/`Set` bound by a match arm must dispatch methods
    // (`m.len()` / `s.contains()`). Before phase-6 line 562 only the Vec
    // arm wired the match-binding dispatch side-tables, so a Map/Set
    // match binding failed codegen with "no handler for method". The
    // typechecker now records the full Map/Set `TypeExpr` for the
    // binding and codegen routes it through `register_var_from_type_expr`.
    let out = run_program(
        r#"
fn build_map() -> Option[Map[i64, i64]] {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(1, 10);
    m.insert(2, 20);
    m.insert(3, 30);
    return Some(m);
}
fn build_set() -> Option[Set[i64]] {
    let mut s: Set[i64] = Set.new();
    s.insert(7);
    s.insert(8);
    return Some(s);
}
fn main() {
    match build_map() {
        Some(m) => println(m.len()),
        None => println(-1),
    }
    match build_set() {
        Some(s) => {
            if s.contains(7) { println(s.len()); } else { println(-1); }
        }
        None => println(-1),
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "3\n2");
    }
}

#[test]
fn test_ir_gpu_scalar_kernel_lowers_for_over_range() {
    // B-2026-08-18-40 increment 3: `for v in a..b` lowers to a native WGSL
    // `for`, inclusive ranges compare with `<=`, loops nest, and the outer
    // counter named `i` renames off the wrapper's thread index so the
    // parameter still reads `input[i]`. Codegen-only, so CI-safe; the
    // executed twin ran on lavapipe and matched the interpreter (11, 23).
    let src = r#"
#[gpu]
fn poly(x: f32) -> f32 {
    let mut acc: f32 = 0.0;
    for i in 0..4 {
        for k in 1..=2 {
            acc = acc + x;
        }
    }
    acc
}

fn main() {
    let mut v: Vec[f32] = Vec.new();
    v.push(2.0);
    let out = gpu.dispatch(poly, v);
    println(out[0] as i64);
}
"#;
    let ir = ir_for_with_ownership(src);
    assert!(
        ir.contains("for (var i_k = 0; i_k < 4; i_k = i_k + 1)"),
        "an exclusive range must compare with `<`, with the counter renamed off \
             the wrapper's thread index; got:\n{ir}"
    );
    assert!(
        ir.contains("for (var k = 1; k <= 2; k = k + 1)"),
        "an inclusive range must compare with `<=`; got:\n{ir}"
    );
    assert!(
        ir.contains("acc = (acc + input[i]);"),
        "the param must still read the THREAD element; got:\n{ir}"
    );
}

#[test]
fn test_ir_gpu_scalar_kernel_lowers_while_loop() {
    // B-2026-08-18-40 increment 2: `while` + mutable locals + assignment,
    // i.e. the accumulator shape every reduction needs. The loop counter is
    // named `i` on purpose: `i` is the generated wrapper's THREAD index, so
    // the local must be renamed and the parameter must keep resolving to
    // `input[i]` (the thread's element), not to the counter. Codegen-only
    // assertion on the embedded shader, so CI-safe; the executed twin ran
    // on lavapipe during development and matched the interpreter (8, 24).
    let src = r#"
#[gpu]
fn poly(x: f32) -> f32 {
    let mut acc: f32 = 0.0;
    let mut i: i32 = 0;
    while i < 4 {
        acc = acc + x;
        i = i.wrapping_add(1);
    }
    acc * 2.0
}

fn main() {
    let mut v: Vec[f32] = Vec.new();
    v.push(1.0);
    let out = gpu.dispatch(poly, v);
    println(out[0] as i64);
}
"#;
    let ir = ir_for_with_ownership(src);
    assert!(
        ir.contains("var acc = 0.0;"),
        "a mutable local must lower to a WGSL `var`; got:\n{ir}"
    );
    assert!(
        ir.contains("var i_k = 0;") && ir.contains("while ((i_k < 4))"),
        "the counter must be renamed off the wrapper's thread index; got:\n{ir}"
    );
    assert!(
        ir.contains("acc = (acc + input[i]);"),
        "the param must still read the THREAD element, not the counter; got:\n{ir}"
    );
    assert!(
        ir.contains("output[i] = (acc * 2.0);"),
        "the tail must read the accumulator; got:\n{ir}"
    );
}

#[test]
fn test_ir_gpu_scalar_kernel_lowers_let_locals() {
    // B-2026-08-18-40: a scalar `#[gpu]` kernel body may name intermediates
    // with `let` instead of being one hand-inlined expression. Each binding
    // lowers to a WGSL `let` inside `main`, in source order, with the kernel
    // parameter resolving to `input[i]` and later bindings seeing earlier
    // ones. Codegen-only assertion on the embedded shader, so this is
    // CI-safe (no GPU is touched); the executed twin ran on lavapipe during
    // development and matched the interpreter.
    let src = r#"
#[gpu]
fn k(x: f32) -> f32 {
    let doubled: f32 = x * 2.0;
    let shifted: f32 = doubled + 1.0;
    shifted * shifted
}

fn main() {
    let mut v: Vec[f32] = Vec.new();
    v.push(1.0);
    let out = gpu.dispatch(k, v);
    println(out[0] as i64);
}
"#;
    let ir = ir_for_with_ownership(src);
    assert!(
        ir.contains("let doubled = (input[i] * 2.0);"),
        "the first `let` must lower to a WGSL `let` reading the param; got:\n{ir}"
    );
    assert!(
        ir.contains("let shifted = (doubled + 1.0);"),
        "a later `let` must see the earlier binding by name; got:\n{ir}"
    );
    assert!(
        ir.contains("output[i] = (shifted * shifted);"),
        "the tail expression must read the bindings; got:\n{ir}"
    );
}

// ── Theme 6: R.method(args) dispatch (sub-step 4) ────────────────────
//
// Structural tests pinning the `karac_provider_lookup` + extractvalue
// + GEP + indirect-call sequence emitted at each `R.method(...)`
// call site where R is a known provider resource. Pairs with the
// sub-step 3 push/pop tests above; together they verify the full
// `with_provider[R](p, || R.method())` shape compiles to the
// expected runtime-stack-walk lowering.

#[test]
fn test_provider_dispatch_emits_lookup_and_indirect_call() {
    let ir = ir_for(
            "pub trait Recorder { fn record(mut ref self, value: i64); }\n\
             pub struct Counter { n: i64 }\n\
             impl Recorder for Counter { fn record(mut ref self, value: i64) { self.n = value; } }\n\
             pub effect resource Metric: Recorder;\n\
             fn run() {\n\
               let p = Counter { n: 0 };\n\
               with_provider[Metric](p, || { Metric.record(42) });\n\
             }",
        );
    assert!(
        ir.contains("call %ProviderLookupResult @karac_provider_lookup")
            || ir.contains("call { ptr, ptr } @karac_provider_lookup"),
        "expected karac_provider_lookup call; IR: {}",
        ir
    );
    // The dispatch loads the fn ptr from the vtable (`load ptr` on
    // `wp.fn`) and indirect-calls. Inkwell's load instruction names
    // come from the third arg to build_load.
    assert!(
        ir.contains("wp.fn"),
        "expected vtable fn pointer load named `wp.fn`; IR: {}",
        ir
    );
}

#[test]
fn test_provider_dispatch_resource_id_matches_declaration_order() {
    // Resource IDs assigned in source-declaration order. The
    // dispatch's lookup call carries the same i32 as the push call
    // at the surrounding with_provider site — verifies the two
    // halves of the ABI agree.
    let ir = ir_for(
            "pub trait Recorder { fn record(mut ref self, value: i64); }\n\
             pub struct Counter { n: i64 }\n\
             impl Recorder for Counter { fn record(mut ref self, value: i64) { self.n = value; } }\n\
             pub effect resource A: Recorder;\n\
             pub effect resource B: Recorder;\n\
             pub effect resource C: Recorder;\n\
             fn run() {\n\
               let p = Counter { n: 0 };\n\
               with_provider[C](p, || { C.record(0) });\n\
             }",
        );
    // Both calls — push and lookup — should reference i32 2 (C is third).
    let push_lines: Vec<&str> = ir
        .lines()
        .filter(|l| l.contains("karac_provider_push"))
        .collect();
    let lookup_lines: Vec<&str> = ir
        .lines()
        .filter(|l| l.contains("karac_provider_lookup"))
        .collect();
    assert!(
        push_lines.iter().any(|l| l.contains("i32 2")),
        "expected push with i32 2 (C is third resource); push lines: {:?}",
        push_lines
    );
    assert!(
        lookup_lines.iter().any(|l| l.contains("i32 2")),
        "expected lookup with i32 2 (C is third resource); lookup lines: {:?}",
        lookup_lines
    );
}

#[test]
fn test_with_provider_e2e_owned_self_dispatch() {
    // Bug surfaced during slice c.2b: `R.method()` on a trait method
    // declared with owned `self` (not `ref` / `mut ref`) tripped
    // `Call parameter type does not match function signature!` from
    // the LLVM module verifier. Root cause: `try_compile_provider_dispatch`
    // unconditionally passed the provider's data-pointer as the
    // first arg, but the impl method's lowered signature takes
    // `Self` by value for owned `self` — `ptr` vs `{ struct fields }`.
    // Fix: branch on the self-param's LLVM type; load the struct
    // from `data_ptr` for owned, pass the ptr directly for ref /
    // mut ref. This E2E asserts the loaded value comes through
    // intact end-to-end.
    let src = "pub trait Counter { fn count(self) -> i64; }\n\
            pub struct H { val: i64 }\n\
            impl Counter for H { fn count(self) -> i64 { self.val } }\n\
            pub effect resource Cnt: Counter;\n\
            fn main() {\n\
              let p = H { val: 42 };\n\
              with_provider[Cnt](p, || { println(Cnt.count()); });\n\
            }";
    let Some(out) = run_program(src) else {
        eprintln!("skipping with_provider owned-self e2e: runtime/linker unavailable");
        return;
    };
    assert_eq!(
        out.trim(),
        "42",
        "expected owned-self dispatch to read self.val (42)"
    );
}

#[test]
fn test_provider_dispatch_skipped_for_non_resource_path() {
    // `Vec::new()` style 2-segment paths must continue routing to
    // compile_assoc_call, not the provider dispatch. No call to
    // karac_provider_lookup should appear for non-provider calls
    // (the extern's `declare` is always emitted at codegen init,
    // so we filter for `call ... @karac_provider_lookup` lines
    // specifically rather than the bare symbol).
    let ir = ir_for(
        "fn main() {\n\
               let v: Vec[i64] = Vec.new();\n\
             }",
    );
    let has_call = ir
        .lines()
        .any(|l| l.contains("call") && l.contains("@karac_provider_lookup"));
    assert!(
        !has_call,
        "non-resource Vec.new must not emit a call to karac_provider_lookup; IR: {}",
        ir
    );
}

// ── Pattern-bound element-type dispatch ──────────────────────────
//
// PB sibling slice (Phase 7.2 — 2026-05-09) closes the gap surfaced
// by CP slice's *Out of scope, still open*: direct method dispatch
// on a pattern-bound `Vec[T]` / `Slice[T]` payload (e.g. `xs.len()`
// where `xs` is the binding for a `V(Vec[i64])` payload) used to
// route through a generic fallback that didn't know the payload's
// parameterized inner type. The PB sibling slice surfaces the inner
// element type through the typechecker → lowering → codegen
// side-table chain so `compile_method_call`'s Vec/Slice arms
// dispatch through the right element-typed path.
//
// The 5 tests below pin the registration: (1) direct `xs.len()` on
// a `Vec[i64]` payload (the headline regression gate, contrasted
// with `test_compound_enum_vec_payload_round_trip` above which kept
// the function-arg work-around path), (2) direct `xs.len()` /
// `xs[0]` on a `Slice[i64]` payload, (3) index-read + push (via
// `let mut`-rebind) on a `Vec[i64]` payload, (4) `Vec[String]`
// element-type round-trip, (5) nested-tuple-Vec destructure as the
// PB5 cross-check.

#[test]
fn test_pattern_bound_vec_payload_method_dispatch_direct() {
    // Headline regression gate. Pre-PB this required routing `xs`
    // through a `ref Vec[i64]` function parameter — see
    // `test_compound_enum_vec_payload_round_trip` for the legacy
    // shape. Post-PB the direct dispatch on the bound name works.
    let out = run_program(
        r#"
enum E { V(Vec[i64]) }
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(7);
    v.push(8);
    v.push(9);
    let e = V(v);
    match e {
        V(xs) => println(xs.len()),
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "3");
    }
}

#[test]
fn test_pattern_bound_slice_payload_method_dispatch_direct() {
    // Slice-payload counterpart to the headline test. Constructs a
    // slice from an Array via `as_slice()`, parks it in a variant
    // payload, and verifies `.len()` and indexing on the bound name
    // dispatch through the slice element-type registry.
    let out = run_program(
        r#"
enum E { V(Slice[i64]) }
fn main() {
    let a: Array[i64, 3] = [10, 20, 30];
    let s: Slice[i64] = a.as_slice();
    let e = V(s);
    match e {
        V(xs) => {
            println(xs.len());
            println(xs[0]);
            println(xs[2]);
        }
    }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["3", "10", "30"]);
    }
}

#[test]
fn test_pattern_bound_vec_of_strings_method_dispatch() {
    // Verifies `String` as the inner element type round-trips
    // through the `type_to_type_expr` helper added by the PB
    // sibling slice — the lowered `TypeExpr` for `String` is a
    // `TypeKind::Path("String")` which `llvm_type_for_name` lowers
    // to the same Vec-shaped struct used at the call-site
    // function-arg path. `.len()` on the bound name returns the
    // element count regardless of element width.
    let out = run_program(
        r#"
enum E { V(Vec[String]) }
fn main() {
    let mut v: Vec[String] = Vec.new();
    v.push("alpha");
    v.push("beta");
    v.push("gamma");
    let e = V(v);
    match e {
        V(xs) => println(xs.len()),
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "3");
    }
}

#[test]
fn test_e2e_slice_pattern_match_dispatches_on_length_for_vec() {
    let out = run_program(
        r#"
fn classify(v: Vec[i64]) -> String {
    match v {
        [] => "0",
        [_] => "1",
        [_, _] => "2",
        [_, .., _] => "3+",
    }
}
fn main() {
    let a: Vec[i64] = Vec.new();
    let mut b: Vec[i64] = Vec.new();
    b.push(1);
    let mut c: Vec[i64] = Vec.new();
    c.push(1); c.push(2);
    let mut d: Vec[i64] = Vec.new();
    d.push(1); d.push(2); d.push(3); d.push(4);
    println(classify(a));
    println(classify(b));
    println(classify(c));
    println(classify(d));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "0\n1\n2\n3+\n");
    }
}

/// Bug #5 regression: `mut ref Map[K, V]` parameter receivers must
/// dispatch through the Map-method codegen path. Pre-fix the
/// dispatcher in `compile_method_call` walked the side-tables
/// (`map_key_types` / `set_elem_types` / `vec_elem_types`) but
/// `compile_function`'s parameter-registration only seeded those
/// tables for owned + `ref Vec[T]` / `ref String` shapes. A
/// `mut ref Map[K,V]` param landed in `variables` but missed
/// `map_key_types`, so dispatch fell through to the "no handler
/// for method 'X' on variable 'v'" diagnostic.
///
/// Fix: route parameter-side registration through
/// `register_var_from_type_expr` against the inner type for
/// Ref/MutRef params — uniform with let-bindings and for-loop
/// bindings. Companion fix in `compile_map_method` /
/// `compile_set_method` (plus the matching for-loop iterator
/// sites in `control_flow_for.rs` and the entry-chain site in
/// `entry_chains.rs`) routes the handle load through
/// `get_data_ptr` so the ref-param's alloca contents are
/// dereferenced before the opaque-handle load.
///
/// Symmetric structural gap to commit `394cd64` (for-loop
/// binding type registration). The fix incidentally unblocks
/// `mut ref Set[T]`, `mut ref VecDeque[T]`, and `mut ref String`
/// receivers — all collection shapes flow through the same
/// registrar.
#[test]
fn test_e2e_mut_ref_map_param_method_dispatch() {
    let src = r#"
shared struct Node { val: i64 }
fn helper(node: Node, visited: mut ref Map[i64, Node]) -> Node {
    match visited.get(node.val) {
        Some(x) => x,
        None => {
            let _ = visited.insert(node.val, node);
            node
        }
    }
}
fn main() {
    let mut m: Map[i64, Node] = Map.new();
    let n = Node { val: 42 };
    let r = helper(n, mut m);
    println(r.val);
}
"#;
    if let Some(out) = run_program(src) {
        assert_eq!(
            out, "42\n",
            "mut ref Map[K,V] parameter — .get / .insert dispatch must reach \
                 compile_map_method, not the no-handler fall-through"
        );
    }
}

/// Side benefit of the bug #5 fix: `mut ref Set[T]` parameters
/// participate in the same registrar path and dispatch through
/// `compile_set_method` correctly. Pre-fix this errored with
/// the same "no handler for method 'insert'" message.
#[test]
fn test_e2e_mut_ref_set_param_method_dispatch() {
    let src = r#"
fn add_to(s: mut ref Set[i64], x: i64) -> i64 {
    let _ = s.insert(x);
    s.len()
}
fn main() {
    let mut s: Set[i64] = Set.new();
    let _ = add_to(mut s, 5);
    let n = add_to(mut s, 10);
    println(n);
}
"#;
    if let Some(out) = run_program(src) {
        assert_eq!(
            out, "2\n",
            "mut ref Set[T] parameter — .insert / .len dispatch must reach \
                 compile_set_method"
        );
    }
}

/// 'push_back'" message because `vec_elem_types` was unset
/// for the `mut ref` shape.
#[test]
fn test_e2e_mut_ref_vec_deque_param_method_dispatch() {
    let src = r#"
fn push_to(q: mut ref VecDeque[i64], x: i64) -> i64 {
    q.push_back(x);
    q.len()
}
fn main() {
    let mut q: VecDeque[i64] = VecDeque.new();
    let _ = push_to(mut q, 5);
    let n = push_to(mut q, 10);
    println(n);
}
"#;
    if let Some(out) = run_program(src) {
        assert_eq!(
            out, "2\n",
            "mut ref VecDeque[T] parameter — .push_back / .len dispatch must \
                 reach compile_vec_method"
        );
    }
}

#[test]
fn test_e2e_parse_option_payload_method_dispatch() {
    // #11 (phase-12): an UNANNOTATED builtin-parse result must carry its
    // `Option[<int>]` element type so the match-bound `Some(v)` payload is
    // a typed scalar for method dispatch. Previously `v.to_string()` (and
    // any method on a value derived from `v`) failed codegen with "no
    // handler for method 'to_string' on variable 'v'" because the integer
    // parses rode the untyped passthrough; only the annotated form
    // (`let o: Option[i64] = …`) worked. Now `<int>.parse` /
    // `from_str_radix` are typed `Option[<int>]` in the typechecker.
    let output = run_program(
            "fn main() {\n\
                 match i64.parse(\"42\") { Some(v) => println(v.to_string()), None => println(\"x\") }\n\
                 let o = i64.parse(\"7\");\n\
                 match o { Some(v) => { let w = v + 1; println(w.to_string()) } None => println(\"x\") }\n\
                 match u8.parse(\"255\") { Some(v) => println(v.to_string()), None => println(\"x\") }\n\
                 match i64.from_str_radix(\"ff\", 16) { Some(v) => println(v.to_string()), None => println(\"x\") }\n\
             }",
        )
        .expect("compile + run failed");
    assert_eq!(output, "42\n8\n255\n255\n");
}

#[test]
fn test_park_on_fd_caller_routes_through_dispatcher_yield_intercept() {
    // The driver that calls `karac_park_on_fd(socket_fd, 0)` routes
    // through the dispatcher-yield helper: allocate the state struct
    // via `__kara_state_new_karac_park_on_fd`, invoke the poll-fn once
    // (state_0), then on Pending block on the completion slot
    // (`kara.park.poll_wait`) and deregister the fd afterwards. This is
    // the leaf primitive's yield to the dispatcher — distinct from the
    // synchronous spin-loop the generic intercept still uses.
    let ir = ir_for_with_state_struct_layouts(park_on_fd_source());
    let driver = function_body(&ir, "driver").expect("driver body must be present");
    assert!(
        driver.contains("@__kara_state_new_karac_park_on_fd"),
        "driver must invoke state-struct constructor for karac_park_on_fd:\n{driver}"
    );
    assert!(
        driver.contains("@__kara_poll_karac_park_on_fd"),
        "driver must invoke poll-fn for karac_park_on_fd:\n{driver}"
    );
    assert!(
        driver.contains("kara.park.poll_wait"),
        "driver must block on the per-park completion slot (dispatcher-yield):\n{driver}"
    );
    assert!(
        driver.contains("@karac_runtime_park_slot_wait"),
        "driver must wait on the completion slot:\n{driver}"
    );
    assert!(
        driver.contains("@karac_runtime_event_loop_deregister_fd"),
        "driver must deregister the fd after the park completes:\n{driver}"
    );
}

// ── Phase 6 line 236 follow-on (a): FileSystem.read_to_string ──
//
// `FileSystem.read_to_string(path) -> Result[String, IoError]`
// had no codegen lowering — the call fell through to `i64 0`, so a
// `match FileSystem.read_to_string(p) { Ok(s) => ..., Err(_) => ...}`
// matched an IntValue scrutinee against a variant pattern and the
// binding never registered, surfacing as "Undefined variable 's'".
// That symptom was originally mis-filed as a separate match-arm
// codegen bug (follow-on (b)); it was always this missing lowering.
// The lowering returns the file's bytes through the KaracIoResult
// buffer fields (the `StringPayload` Ok arm) and rebuilds the
// `String` aggregate.

#[test]
fn test_ir_read_to_string_dispatches_to_runtime_ffi() {
    let ir = ir_for(
        r#"
fn load(path: String) -> String {
    match FileSystem.read_to_string(path) {
        Ok(s) => s,
        Err(_) => "",
    }
}
"#,
    );
    assert!(
        ir.contains("declare void @karac_runtime_file_read_to_string"),
        "expected read_to_string FFI declaration; ir:\n{ir}"
    );
    let body = function_body(&ir, "load").expect("load fn must lower");
    assert!(
        body.contains("call void @karac_runtime_file_read_to_string"),
        "load body should call the FFI; body:\n{body}"
    );
}

#[test]
fn test_ir_read_lines_dispatches_to_runtime_ffi() {
    // `FileSystem.read_lines(path) -> Result[Vec[String], IoError]`
    // (B-2026-07-11-38) lowers to the two-out-param FFI
    // `karac_runtime_fs_read_lines(out_io, out_vec, path_ptr, path_len)`.
    let ir = ir_for(
        r#"
fn load(path: String) -> i64 {
    match FileSystem.read_lines(path) {
        Ok(lines) => lines.len(),
        Err(_) => 0,
    }
}
"#,
    );
    assert!(
        ir.contains("declare void @karac_runtime_fs_read_lines"),
        "expected read_lines FFI declaration; ir:\n{ir}"
    );
    let body = function_body(&ir, "load").expect("load fn must lower");
    assert!(
        body.contains("call void @karac_runtime_fs_read_lines"),
        "load body should call the FFI; body:\n{body}"
    );
    // The Ok arm branches on the KaracIoResult error_kind and builds
    // the Result via the named blocks / GEPs from `compile_fs_read_lines`.
    for needle in ["rl.is_ok", "rl.vec.val"] {
        assert!(
            body.contains(needle),
            "expected read_lines lowering marker '{needle}'; body:\n{body}"
        );
    }
}

// A `let` binding with no type annotation bound to a String-returning
// call must still route `.len()` (and other String methods) through
// the String/Vec dispatch. The typechecker records "String" in
// `pattern_binding_types` for the inferred `Type::Str`; codegen's
// let-statement handler must wire that surface type into
// `string_vars` / `vec_elem_types` the same way the explicit-
// annotation path (`let r: String = …`) does. Pre-fix this fell
// through to the "no handler for method 'len' on variable 'r'"
// codegen error.
#[test]
fn test_e2e_inferred_string_binding_len_dispatch() {
    let out = run_program(
        r#"
fn make() -> String { "fl" }
fn main() {
    let r = make();
    println(r.len());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "2");
    }
}

/// B-2026-08-10-19: phase 2 now picks between a branchy and a branchless
/// merge kernel per pass, so shuffled input at a size large enough to
/// outrun the 256-output probe executes a kernel that NO other test in
/// this file reaches — every other sort test is either too small or too
/// ordered to leave the branchy path.
///
/// Teeth: the branchless kernel selects the value and advances both
/// cursors by `zext` of the same `cmp <= 0` predicate. Weaken it to
/// `cmp < 0` and it takes the RIGHT run on a tie, inverting equal keys —
/// the first block has 64 duplicates per key and `.1` carries the
/// original position, so that shows up immediately. Get either cursor
/// increment wrong and sortedness and the index-sum both fail.
#[test]
fn adaptive_merge_kernel_is_stable_on_shuffled_input() {
    let out = run_program(
        r#"
fn check(v: Vec[(i64, i64)], n: i64) -> i64 {
    let mut bad: i64 = 0i64;
    let mut s: i64 = 0i64;
    let mut i: i64 = 0i64;
    while i < v.len() {
        if i > 0i64 {
            let p: (i64, i64) = v[i - 1i64];
            let c: (i64, i64) = v[i];
            if p.0 > c.0 { bad = bad + 1i64; }
            if p.0 == c.0 and p.1 > c.1 { bad = bad + 1i64; }
        }
        s = s + v[i].1;
        i = i + 1i64;
    }
    // Every original index must survive exactly once.
    if s != n * (n - 1i64) / 2i64 { bad = bad + 1000i64; }
    if v.len() != n { bad = bad + 1000i64; }
    return bad;
}
fn build(n: i64, m: i64) -> Vec[(i64, i64)] {
    let mut v: Vec[(i64, i64)] = Vec.new();
    let mut s: i64 = 12345i64;
    let mut i: i64 = 0i64;
    while i < n {
        s = (s * 1103515245i64 + 12345i64) % 2147483648i64;
        v.push((s % m, i));
        i = i + 1i64;
    }
    return v;
}
fn main() {
    let mut bad: i64 = 0i64;
    // Heavy ties over a 64-key alphabet: shuffled enough for the probe to
    // commit passes to the branchless kernel, with enough duplicates that a
    // flipped tie-break cannot hide.
    let mut a: Vec[(i64, i64)] = build(4096i64, 64i64);
    a.sort_by(|x, y| x.0.cmp(y.0));
    bad = bad + check(a, 4096i64);
    // Near-distinct keys: a maximally unpredictable take sequence, so the
    // branchless kernel is certain to be selected here.
    let mut b: Vec[(i64, i64)] = build(4096i64, 1000000i64);
    b.sort_by(|x, y| x.0.cmp(y.0));
    bad = bad + check(b, 4096i64);
    println(bad);
}
"#,
    );
    assert_eq!(out.as_deref().map(str::trim), Some("0"));
}

/// B-2026-08-10-19 sibling, structural. The two merge kernels produce
/// IDENTICAL output by construction, so no behavioural test can tell
/// which one ran — delete the branchless kernel and every sort test in
/// this file still passes, just slower. This pins the shape instead, so
/// the 1.3x on shuffled input cannot be silently dropped.
#[test]
fn adaptive_merge_emits_both_kernels_and_the_probe() {
    let src = r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    let mut i: i64 = 0;
    while i < 5000 {
        v.push((i * 37 + 11) % 5000);
        i = i + 1;
    }
    v.sort_by(|a, b| a.cmp(b));
    println(v[0]);
}
"#;
    let mut parsed = karac::parse(src);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);
    let ownership = karac::ownershipcheck(&parsed.program, &typed);
    let ir =
        karac::codegen::compile_to_ir(&parsed.program, Some(&ownership), None).expect("codegen");
    for label in ["p2.pr.cmp", "p2.bl.body", "p2.m.cmp", "p2.mode.pick"] {
        assert!(
            ir.contains(label),
            "the mono sort must still emit `{label}` — phase 2 needs the \
                 probe, both merge kernels and the per-pass dispatch"
        );
    }
}

#[test]
fn e2e_user_ord_cmp_dispatch_across_struct_shapes() {
    // The operator fix must not be specific to the single-i64-field struct
    // that exposed the codegen half (that one flattens to a bare int, which
    // is exactly why the builtin comparator could intercept it). Three
    // shapes that lower differently, each with a `cmp` whose answer no
    // structural comparison can imitate:
    //
    //   Pair  — two fields, compares by the SECOND only, so declaration
    //           order (which reads `a` first) gives the opposite answer;
    //   Name  — a HEAP field, compared in REVERSE (also exercises the
    //           owned-String receiver through the synthesised call);
    //   Level — a unit-only enum whose `cmp` is unconditionally `Greater`.
    let out = run_program(
        r#"
struct Pair { a: i64, b: i64 }
impl PartialEq for Pair { fn eq(ref self, other: ref Pair) -> bool { self.b == other.b } }
impl Eq for Pair {}
impl PartialOrd for Pair { fn partial_cmp(ref self, other: ref Pair) -> Option[Ordering] { Some(self.b.cmp(other.b)) } }
impl Ord for Pair { fn cmp(ref self, other: ref Pair) -> Ordering { self.b.cmp(other.b) } }

struct Name { s: String }
impl PartialEq for Name { fn eq(ref self, other: ref Name) -> bool { self.s == other.s } }
impl Eq for Name {}
impl PartialOrd for Name { fn partial_cmp(ref self, other: ref Name) -> Option[Ordering] { Some(other.s.cmp(self.s)) } }
impl Ord for Name { fn cmp(ref self, other: ref Name) -> Ordering { other.s.cmp(self.s) } }

enum Level { Lo, Hi }
impl PartialEq for Level { fn eq(ref self, other: ref Level) -> bool { true } }
impl Eq for Level {}
impl PartialOrd for Level { fn partial_cmp(ref self, other: ref Level) -> Option[Ordering] { Some(Ordering.Greater) } }
impl Ord for Level { fn cmp(ref self, other: ref Level) -> Ordering { Ordering.Greater } }

fn main() {
    let p1 = Pair { a: 9, b: 1 };
    let p2 = Pair { a: 1, b: 9 };
    println(p1 < p2);
    let n1 = Name { s: "aaa" };
    let n2 = Name { s: "zzz" };
    println(n1 < n2);
    println(Level.Lo < Level.Hi);
    println(Level.Lo >= Level.Hi);
}
"#,
    );
    let out = out.expect("user-`impl Ord` operators must build for every struct shape");
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines, vec!["true", "false", "false", "true"]);
}

#[test]
fn test_e2e_user_trait_bound_dispatch_column_and_tensor_monos() {
    // S6a probe p7: one bound-generic fn instantiated at BOTH
    // handle-backed builtins. Guards three fixes at once:
    // (1) the mono var-side-table leak (mono #1's `c → Column` entry
    //     leaked into mono #2, compiling the Tensor as a Column —
    //     SIGSEGV; `SavedVarSideTables`),
    // (2) the same-LLVM-shape mono collision (both args are `ptr`, so
    //     both instantiations mangled identically and shared one
    //     body; `mono_handle_param_infos` adds the
    //     `$c_col_i64`/`$c_ten_i64_4` axis),
    // (3) the typechecker's bound trait-arg substitution
    //     (`C: MyReduce[i64]` typed `c.sum()` as raw `T`).
    let src = r#"
trait MyReduce[T] {
    fn sum(ref self) -> T;
    fn min(ref self) -> T;
    fn max(ref self) -> T;
    fn mean(ref self) -> f64;
}
impl[T] MyReduce[T] for Column[T] {
    fn sum(ref self) -> T {
        self.sum()
    }
    fn min(ref self) -> T {
        self.min()
    }
    fn max(ref self) -> T {
        self.max()
    }
    fn mean(ref self) -> f64 {
        self.mean()
    }
}
impl[T, ...S] MyReduce[T] for Tensor[T, S] {
    fn sum(ref self) -> T {
        self.sum()
    }
    fn min(ref self) -> T {
        self.min()
    }
    fn max(ref self) -> T {
        self.max()
    }
    fn mean(ref self) -> f64 {
        self.mean()
    }
}
fn report[C: MyReduce[i64]](c: ref C) -> i64 {
    println(f"{c.sum()} {c.min()} {c.max()} {c.mean()}");
    c.sum()
}
fn main() {
    let c: Column[i64] = Column.from_vec([10, 20, 30]);
    let t: Tensor[i64, [4]] = Tensor.from([2, 4, 6, 8]);
    println(f"{report(c) + report(t)}");
}
"#;
    let out = run_program(src).expect("program should compile and run");
    assert_eq!(out, "60 10 30 20\n20 2 8 5\n80\n");
}

#[test]
fn test_e2e_stdlib_reduce_trait_bound_dispatch() {
    // S6a acceptance: the BAKED `Reduce[T]` trait + the
    // `#[compiler_builtin]` Column/Tensor impls. Bound-generic
    // dispatch routes to the reduction kernels on both
    // instantiations; concrete-receiver calls are unchanged.
    let src = r#"
fn spread[C: Reduce[i64]](c: ref C) -> i64 {
    c.max() - c.min()
}
fn avg[C: Reduce[i64]](c: ref C) -> f64 {
    c.mean()
}
fn main() {
    let c: Column[i64] = Column.from_vec([10, 20, 30]);
    let t: Tensor[i64, [4]] = Tensor.from([2, 4, 6, 8]);
    println(f"{spread(c)} {spread(t)}");
    println(f"{avg(c)} {avg(t)}");
    println(f"{c.sum()} {t.sum()}");
}
"#;
    let out = run_program(src).expect("program should compile and run");
    assert_eq!(out, "20 6\n20 5\n60 20\n");
}

#[test]
fn test_e2e_elementwise_ord_trait_bound_dispatch() {
    // S6c: the baked `impl ElementwiseOrd for Column/Tensor` makes a
    // bound-generic `fn f[C: ElementwiseOrd[i64]](c: ref C)` monomorphize
    // argmin/argmax/sorted to the order-statistics kernel on both
    // instantiations under `karac build` — parity with the Reduce bound
    // dispatch. Also a user trait impl over a container calls the methods
    // on `self`. `run` == `build`.
    let src = r#"
fn lo[C: ElementwiseOrd[i64]](c: ref C) -> i64 { c.argmin().unwrap() }
fn hi[C: ElementwiseOrd[i64]](c: ref C) -> i64 { c.argmax().unwrap() }
fn top[C: ElementwiseOrd[i64]](c: ref C) -> i64 { let s: Vec[i64] = c.sorted(); s[2] }
trait Ranked { fn span(ref self) -> i64; }
impl Ranked for Column[i64] {
    fn span(ref self) -> i64 { self.argmin().unwrap() + self.argmax().unwrap() }
}
fn main() {
    let c: Column[i64] = Column.from_vec([3, 1, 2]);
    let t: Tensor[i64, [3]] = Tensor.from([30, 10, 20]);
    println(f"{lo(c)} {hi(c)} {top(c)}");
    println(f"{lo(t)} {hi(t)} {top(t)}");
    println(f"{c.span()}");
}
"#;
    let out = run_program(src).expect("program should compile and run");
    // c [3,1,2]: argmin@1, argmax@0, sorted[2]=3. t [30,10,20]: 1, 0, 30.
    // c.span = argmin(1) + argmax(0) = 1.
    assert_eq!(out, "1 0 3\n1 0 30\n1\n");
}

#[test]
fn test_e2e_bound_generic_dispatch_over_user_type() {
    // B-2026-07-06-2: a bound-generic `fn f[C: Trait](c: ref C) { c.m() }`
    // whose type param monomorphizes to a USER struct implementing the
    // trait failed under `karac build` ("no handler for method 'm' on
    // variable 'c'") while `karac run` computed it. Root cause: the mono
    // param-type-name registration only matched a bare `Path` param, so a
    // `ref C` receiver never recorded `var_type_names["c"] = "Wrap"`, and
    // `inferred_receiver_type` returned None inside the mono → the
    // `Wrap.dbl` dispatch was skipped. Containers were unaffected (their
    // kernel intercept fires without `var_type_names`). Fix peels a leading
    // `ref`/`mut ref` before the name registration. Covers a plain user
    // trait (ref, owned, and mut-ref receivers) plus the stdlib `Reduce`
    // surface over a user implementor — all under `build`. `run` == `build`.
    let src = r#"
trait Doubler[T] { fn dbl(ref self) -> T; }
struct Wrap { v: i64 }
impl Doubler[i64] for Wrap { fn dbl(ref self) -> i64 { self.v + self.v } }

trait Owned[T] { fn triple(self) -> T; }
struct Own { v: i64 }
impl Owned[i64] for Own { fn triple(self) -> i64 { self.v * 3 } }

trait Bump { fn bump(mut ref self) -> i64; }
struct Ctr { n: i64 }
impl Bump for Ctr { fn bump(mut ref self) -> i64 { self.n = self.n + 1; self.n } }

struct Pair { a: i64, b: i64 }
impl Reduce[i64] for Pair {
    fn sum(ref self) -> i64 { self.a + self.b }
    fn prod(ref self) -> i64 { self.a * self.b }
    fn min(ref self) -> i64 { if self.a < self.b { self.a } else { self.b } }
    fn max(ref self) -> i64 { if self.a > self.b { self.a } else { self.b } }
    fn mean(ref self) -> f64 { 0.0 }
    fn fold[A](ref self, init: A, f: Fn(A, i64) -> A) -> A { f(f(init, self.a), self.b) }
}

fn dref[C: Doubler[i64]](c: ref C) -> i64 { c.dbl() }
fn towned[C: Owned[i64]](c: C) -> i64 { c.triple() }
fn twice[C: Bump](c: mut ref C) -> i64 { c.bump() + c.bump() }
fn total[C: Reduce[i64]](c: ref C) -> i64 { c.sum() + c.max() }

fn main() {
    let w: Wrap = Wrap { v: 21 };
    let mut ct: Ctr = Ctr { n: 0 };
    let p: Pair = Pair { a: 3, b: 4 };
    println(f"{dref(w)}");
    println(f"{towned(Own { v: 5 })}");
    println(f"{twice(mut ct)}");
    println(f"{total(p)}");
}
"#;
    let out = run_program(src).expect("program should compile and run");
    // dbl(21)=42; triple(5)=15; bump twice 1+2=3; sum(7)+max(4)=11.
    assert_eq!(out, "42\n15\n3\n11\n");
}

#[test]
fn test_e2e_user_trait_impl_over_tensor() {
    // S6c-12 slice 2: the Tensor twin of `test_e2e_user_trait_impl_over_column`.
    // The typechecker `self_type` fix and codegen `self`-arg threading already
    // covered Tensor; the only codegen gap was `try_compile_tensor_reduce`
    // rejecting a `SelfValue` receiver, so `self.sum()` in the body hit the
    // "no handler ... on non-identifier receiver" fall-through. `run` == `build`.
    let src = r#"
trait Combo[T] { fn twice(ref self) -> T; fn quad(ref self) -> T; }
impl Combo[i64] for Tensor[i64, [4]] {
    fn twice(ref self) -> i64 { self.sum() + self.sum() }
    fn quad(ref self) -> i64 { self.twice() + self.twice() }
}
trait Spread[T] { fn spread(ref self) -> T; }
impl Spread[f64] for Tensor[f64, [3]] {
    fn spread(ref self) -> f64 { self.max() - self.min() }
}
fn main() {
    let ti: Tensor[i64, [4]] = Tensor.from([1, 2, 3, 4]);
    let tf: Tensor[f64, [3]] = Tensor.from([1.5, 4.0, 2.5]);
    println(f"{ti.quad()}");
    println(f"{tf.spread()}");
}
"#;
    let out = run_program(src).expect("program should compile and run");
    // quad = 4*sum = 40; spread = 4.0-1.5 = 2.5.
    assert_eq!(out, "40\n2.5\n");
}

#[test]
fn test_e2e_user_generic_trait_impl_over_tensor_operator_widths() {
    // B-2026-07-04-16: a generic `impl[T: Add] Trait[T] for Tensor[T, S]`
    // (and the Column twin) whose body applies an operator to the element
    // type (`self.sum() + self.sum()`). Two coupled codegen fixes:
    //   1. The impl `T` was left UNBOUND for a Tensor receiver — the
    //      receiver's recorded instantiation carries a SHAPE arg (`[3]`)
    //      alongside the element, and counting it tripped the `explicit`
    //      binding's `<= n_params` gate. `T` then defaulted to `i64`, so an
    //      `f64` Tensor's `T + T` lowered as a CHECKED INTEGER add →
    //      "integer overflow" under `build` (Column, a single-arg
    //      instantiation, was unaffected).
    //   2. Even bound, the impl `T` was sourced from the span-recorded
    //      instantiation, whose element is the constructor LITERAL's default
    //      (`f64` for a narrow-`f32` tensor built from `[1.0, …]`). Reading
    //      the `f32` buffer with that `f64` stride produced SILENT GARBAGE
    //      for both Column and Tensor. `T` is now sourced from the
    //      container's registered (annotation-derived) element type.
    // Exercises f64, f32, and a narrow unsigned int across BOTH containers.
    let src = r#"
trait Doubler[T: Add] { fn doubled_sum(ref self) -> T; }
impl[T: Add] Doubler[T] for Tensor[T, [3]] {
    fn doubled_sum(ref self) -> T { self.sum() + self.sum() }
}
impl[T: Add] Doubler[T] for Column[T] {
    fn doubled_sum(ref self) -> T { self.sum() + self.sum() }
}
fn main() {
    let tf: Tensor[f64, [3]] = Tensor.from([1.5, 2.5, 3.0]);
    let ts: Tensor[f32, [3]] = Tensor.from([1.0, 2.0, 3.0]);
    let tu: Tensor[u32, [3]] = Tensor.from([10u32, 20u32, 30u32]);
    let cf: Column[f32] = Column.from_vec([1.0, 2.0, 3.0]);
    println(f"{tf.doubled_sum()} {ts.doubled_sum()} {tu.doubled_sum()} {cf.doubled_sum()}");
}
"#;
    let out = run_program(src).expect("program should compile and run");
    // Tensor f64 sum=7.0 → 14; f32 sum=6.0 → 12; u32 sum=60 → 120;
    // Column f32 sum=6.0 → 12.
    assert_eq!(out, "14 12 120 12\n");
}

#[test]
fn test_e2e_builtin_column_tensor_range() {
    // The BAKED `Reduce[T]::range` DEFAULT method (`max - min`) on the
    // BUILTIN implementors `Column[T]` / `Tensor[T, S]`. These don't inherit
    // it via the user-impl splice (their reductions dispatch through the
    // dedicated value-shape intercepts), so a `range` arm was added at each
    // Column/Tensor reduce dispatch site (interp + codegen). Covers i64
    // (integer sub) and f64 (fsub) for both containers; `run` and `build`
    // (and default auto-par) must agree, matching `min`/`max`.
    let src = r#"
fn main() {
    let ci: Column[i64] = Column.from_vec([3, 9, 1, 7]);
    let cf: Column[f64] = Column.from_vec([1.5, 9.0, 4.0]);
    let ti: Tensor[i64, [4]] = Tensor.from([2, 4, 6, 8]);
    let tf: Tensor[f64, [3]] = Tensor.from([1.5, 9.0, 4.0]);
    println(f"{ci.range()} {ti.range()}");
    println(f"{cf.range()} {tf.range()}");
}
"#;
    let out = run_program(src).expect("program should compile and run");
    assert_eq!(out, "8 6\n7.5 7.5\n");
}

#[test]
fn test_e2e_tensor_fold() {
    // `Tensor.fold[A](init, |acc, x| ...)` — the general left-fold, parity
    // with `Column.fold`. A tensor has no null concept, so EVERY element
    // folds (in C order — a 2-D tensor folds all cells); an empty tensor
    // would return `init` (the loop doesn't run — the fold identity, no
    // trap). Covers sum, product, predicate count, sum-of-squares, a 2-D
    // fold-over-all-cells, an f64 accumulator, an outer-variable capture,
    // and param shadowing. `run` == `build` == default auto-par.
    let src = r#"
fn main() {
    let t: Tensor[i64, [5]] = Tensor.from([1, 2, 3, 4, 5]);
    println(f"{t.fold(0, |a, x| a + x)}");
    println(f"{t.fold(1, |a, x| a * x)}");
    println(f"{t.fold(0, |a, x| if x > 2 { a + 1 } else { a })}");
    println(f"{t.fold(0, |a, x| a + x * x)}");
    let m: Tensor[i64, [2, 3]] = Tensor.from([[1, 2, 3], [4, 5, 6]]);
    println(f"{m.fold(0, |a, x| a + x)}");
    let tf: Tensor[f64, [3]] = Tensor.from([1.5, 2.5, 4.0]);
    println(f"{tf.fold(0.0, |a, x| a + x)}");
    let k: i64 = 10;
    println(f"{t.fold(0, |a, x| a + x + k)}");
    let a: i64 = 7;
    let s = t.fold(0, |a, x| a + x);
    println(f"{s} {a}");
}
"#;
    let out = run_program(src).expect("program should compile and run");
    assert_eq!(out, "15\n120\n3\n55\n21\n8\n65\n15 7\n");
}

#[test]
fn test_e2e_tensor_from_narrow_element_width_stores_at_declared_width() {
    // B-2026-07-03-35: `Tensor.from([...])` stored its leaves at the leaf
    // literals' DEFAULT width (i64/f64) rather than the binding's declared
    // NARROW element width, so every reader — which strides at
    // `tensor_elem_size(info.elem)` — landed on the wrong bytes: SILENT
    // wrong output under `karac build` (`t[0..3]` read [40,0,10,0], sum 50),
    // correct under `karac run`. The fix threads the declared element type
    // via `pending_let_tensor_info` (like zeros/ones/full) and coerces each
    // leaf to it. Covers i32/i16/u8/u32/f32 element reads + reductions,
    // sign-extension of negatives on read-back, a 2-D narrow tensor, and
    // the i64/f64 no-op widths (regression guard). run == build == auto-par.
    let src = r#"
fn main() {
    let t: Tensor[i32, [4]] = Tensor.from([40, 10, 30, 20]);
    println(f"{t[0]} {t[1]} {t[2]} {t[3]} {t.sum()}");
    let n: Tensor[i16, [3]] = Tensor.from([-100, -200, 300]);
    println(f"{n[0]} {n[1]} {n[2]} {n.sum()}");
    let u: Tensor[u8, [4]] = Tensor.from([1, 2, 50, 3]);
    println(f"{u[0]} {u[2]} {u[3]} {u.sum()}");
    let w: Tensor[u32, [3]] = Tensor.from([10, 20, 30]);
    println(f"{w[0]} {w[2]} {w.sum()}");
    let f: Tensor[f32, [3]] = Tensor.from([1.5, 2.5, 3.0]);
    println(f"{f[0]} {f[2]} {f.sum()}");
    let m: Tensor[i32, [2, 2]] = Tensor.from([[1, 2], [3, 4]]);
    println(f"{m[0, 0]} {m[0, 1]} {m[1, 0]} {m[1, 1]} {m.sum()}");
    let bi: Tensor[i64, [3]] = Tensor.from([5, 6, 7]);
    let bf: Tensor[f64, [2]] = Tensor.from([1.5, 2.5]);
    println(f"{bi[1]} {bi.sum()} {bf[0]} {bf.sum()}");
}
"#;
    let out = run_program(src).expect("program should compile and run");
    assert_eq!(
        out,
        "40 10 30 20 100\n-100 -200 300 0\n1 50 3 56\n10 30 60\n\
             1.5 3 7\n1 2 3 4 10\n6 18 1.5 4\n"
    );
}

#[test]
fn test_e2e_tensor_map() {
    // `Tensor.map(|x| ...) -> Tensor[T, ...S]` — element-wise map producing
    // a fresh tensor of the same shape (every C-order element, no null
    // gate). Covers a 1-D map, a 2-D fold-over-all-cells result, and a
    // captured outer variable. `run` == `build` == default auto-par.
    let src = r#"
fn main() {
    let t: Tensor[i64, [4]] = Tensor.from([1, 2, 3, 4]);
    let d = t.map(|x| x * 2);
    println(f"{d.sum()}");
    let m: Tensor[i64, [2, 3]] = Tensor.from([[1, 2, 3], [4, 5, 6]]);
    let e = m.map(|x| x + 10);
    println(f"{e.sum()}");
    let tf: Tensor[f64, [3]] = Tensor.from([1.5, 2.5, 4.0]);
    let df = tf.map(|x| x + 0.5);
    println(f"{df.sum()}");
    let k: i64 = 100;
    let g = t.map(|x| x + k);
    println(f"{g.sum()}");
}
"#;
    let out = run_program(src).expect("program should compile and run");
    // sum([2,4,6,8])=20; sum([11..16])=81; sum([2.0,3.0,4.5])=9.5;
    // sum([101,102,103,104])=410.
    assert_eq!(out, "20\n81\n9.5\n410\n");
}

#[test]
fn test_e2e_tensor_map_rejects_noninline() {
    // Same inline-literal boundary as `Column.map` / the folds.
    let non_inline = r#"
fn main() {
    let t: Tensor[i64, [3]] = Tensor.from([1, 2, 3]);
    let g = |x: i64| x * 2;
    let d = t.map(g);
    println(f"{d.sum()}");
}
"#;
    let err = ir_result(non_inline).expect_err("a non-inline closure must be rejected");
    assert!(err.contains("inline closure literal"), "got: {err}");
}

#[test]
fn test_e2e_tensor_zip_with_ref_param_cosine_similarity() {
    // B-2026-07-13-5 gap C: a `ref Tensor[f32, [D]]` PARAM forwarded as the
    // `other` argument to `zip_with` (whose baked param is `other: ref Self`)
    // must typecheck — previously `check_assignable(Tensor, ref Tensor)`
    // rejected it. This is the shape the numerical stdlib needs:
    // `cosine_similarity` uses each `ref` input multiple times (a `dot(a,a)`
    // norm + `dot(a,b)`), which owned params can't express (double-move).
    // Orthogonal vectors → 0, identical → 1 (exact).
    let src = r#"
fn dot[D](a: ref Tensor[f32, [D]], b: ref Tensor[f32, [D]]) -> f32 {
    let p = a.zip_with(b, |x, y| x * y);
    p.sum()
}
fn cosine_similarity[D](a: ref Tensor[f32, [D]], b: ref Tensor[f32, [D]]) -> f32 {
    let na: f32 = dot(a, a).sqrt();
    let nb: f32 = dot(b, b).sqrt();
    dot(a, b) / (na * nb)
}
fn main() {
    let a: Tensor[f32, [3]] = Tensor.from([1.0f32, 0.0f32, 0.0f32]);
    let b: Tensor[f32, [3]] = Tensor.from([0.0f32, 1.0f32, 0.0f32]);
    let c: Tensor[f32, [3]] = Tensor.from([1.0f32, 0.0f32, 0.0f32]);
    println(cosine_similarity(a, b));
    println(cosine_similarity(a, c));
}
"#;
    let out = run_program(src).expect("program should compile and run");
    assert_eq!(out, "0\n1\n");
}

#[test]
fn test_e2e_tensor_zip_with() {
    // `Tensor.zip_with` — element-wise combine of two same-shape tensors
    // (in C order); a runtime shape-equality guard. No bitmap (a tensor has
    // no null concept). Covers 1-D, 2-D, and an f64 pair.
    let src = r#"
fn main() {
    let t1: Tensor[i64, [4]] = Tensor.from([1, 2, 3, 4]);
    let t2: Tensor[i64, [4]] = Tensor.from([2, 2, 2, 2]);
    let z1 = t1.zip_with(t2, |x, y| x * y);
    println(f"{z1.sum()}");
    let m1: Tensor[i64, [2, 2]] = Tensor.from([[1, 2], [3, 4]]);
    let m2: Tensor[i64, [2, 2]] = Tensor.from([[10, 20], [30, 40]]);
    let z2 = m1.zip_with(m2, |x, y| x + y);
    println(f"{z2.sum()}");
    let f1: Tensor[f64, [3]] = Tensor.from([1.0, 2.0, 3.0]);
    let f2: Tensor[f64, [3]] = Tensor.from([0.5, 0.5, 0.5]);
    let z3 = f1.zip_with(f2, |x, y| x + y);
    println(f"{z3.sum()}");
}
"#;
    // 2+4+6+8=20; 11+22+33+44=110; 1.5+2.5+3.5=7.5.
    let out = run_program(src).expect("program should compile and run");
    assert_eq!(out, "20\n110\n7.5\n");
}

#[test]
fn test_e2e_tensor_reduce_on_chain_receiver() {
    // B-2026-07-13-5 legs A/C: a scalar reduction on a NON-IDENTIFIER
    // (method-chain) receiver — `a.zip_with(b, f).sum()`, `a.map(g).max()`
    // — failed codegen with "no handler for method 'sum' on non-identifier
    // receiver" (try_compile_tensor_reduce only matched Identifier/Self).
    // The receiver's element type is unrecoverable from the collided span
    // via tensor_typed_exprs, so the typechecker records it in
    // temp_recv_elem_types (keyed by the reduce call span) for codegen to
    // reduce the compiled temp and free it. Covers `sum`/`prod`/`max`/`min`,
    // a `map` chain, i64 + f32 elements, and a generic-shape-param `[D]`
    // function (leg C).
    let src = r#"
fn combine[D](a: ref Tensor[f32, [D]], b: ref Tensor[f32, [D]]) -> f32 {
    a.zip_with(b, |x, y| x + y).sum()
}
fn main() {
    let a1: Tensor[f32, [3]] = Tensor.from([1.0f32, 2.0f32, 3.0f32]);
    let b1: Tensor[f32, [3]] = Tensor.from([4.0f32, 5.0f32, 6.0f32]);
    println(f"{a1.zip_with(b1, |x, y| x + y).sum()}");
    let a2: Tensor[f32, [3]] = Tensor.from([1.0f32, 2.0f32, 3.0f32]);
    let b2: Tensor[f32, [3]] = Tensor.from([4.0f32, 5.0f32, 6.0f32]);
    println(f"{a2.zip_with(b2, |x, y| x * y).prod()}");
    let a3: Tensor[f32, [3]] = Tensor.from([1.0f32, 2.0f32, 3.0f32]);
    println(f"{a3.map(|x| x * 2.0f32).max()}");
    let ia: Tensor[i64, [3]] = Tensor.from([10, 20, 30]);
    let ib: Tensor[i64, [3]] = Tensor.from([1, 2, 3]);
    println(f"{ia.zip_with(ib, |x, y| x - y).sum()}");
    let ga: Tensor[f32, [3]] = Tensor.from([1.0f32, 2.0f32, 3.0f32]);
    let gb: Tensor[f32, [3]] = Tensor.from([4.0f32, 5.0f32, 6.0f32]);
    println(f"{combine(ga, gb)}");
}
"#;
    // 21; 4*10*18=720; max(2,4,6)=6; (9+18+27)=54; combine=21.
    let out = run_program(src).expect("program should compile and run");
    assert_eq!(out, "21\n720\n6\n54\n21\n");
}

#[test]
fn test_e2e_tensor_body_annotation_generic_shape_param() {
    // B-2026-07-13-5 leg B: a BODY type annotation naming the enclosing fn's
    // generic shape param (`let p: Tensor[f32, [D]]` inside `fn scale[D]`)
    // now checks and runs on both backends — the shape-dim `D` resolves as a
    // symbolic dim via `enclosing_bounds` instead of erroring in the const-
    // evaluator. scale([1,2,3], 2.0) = sum([2,4,6]) = 12.
    let src = r#"
fn scale[D](a: ref Tensor[f32, [D]], k: f32) -> f32 {
    let p: Tensor[f32, [D]] = a * k;
    p.sum()
}
fn main() {
    let x: Tensor[f32, [3]] = Tensor.from([1.0f32, 2.0f32, 3.0f32]);
    println(f"{scale(x, 2.0f32)}");
}
"#;
    let out = run_program(src).expect("program should compile and run");
    assert_eq!(out, "12\n");
}

#[test]
fn test_e2e_tensor_argmin_argmax() {
    // `Tensor.argmin()`/`argmax() -> Option[i64]` (ElementwiseOrd, S6c): the
    // flat C-order index of the first min/max over ALL elements (no null
    // concept). Covers ties (first occurrence), a 2-D tensor (C-order flat
    // index), and an f64 tensor. `run` == `build` == default auto-par.
    let src = r#"
fn show(o: Option[i64]) {
    match o {
        Some(i) => println(f"{i}"),
        None => println("none"),
    }
}
fn main() {
    let t: Tensor[i64, [6]] = Tensor.from([4, 2, 7, 2, 9, 9]);
    show(t.argmin());
    show(t.argmax());
    let m: Tensor[i64, [2, 3]] = Tensor.from([[3, 1, 4], [1, 5, 9]]);
    show(m.argmin());
    show(m.argmax());
    let tf: Tensor[f64, [4]] = Tensor.from([2.5, 8.0, 1.0, 8.0]);
    show(tf.argmin());
    show(tf.argmax());
}
"#;
    let out = run_program(src).expect("program should compile and run");
    // t: min 2 first at idx 1, max 9 first at idx 4.
    // m (flat [3,1,4,1,5,9]): min 1 first at flat idx 1, max 9 at flat idx 5.
    // tf: min 1.0 at idx 2, max 8.0 first at idx 1.
    assert_eq!(out, "1\n4\n1\n5\n2\n1\n");
}

#[test]
fn test_e2e_tensor_sorted_argsort() {
    // `Tensor.sorted() -> Vec[T]` / `argsort() -> Vec[i64]` (ElementwiseOrd,
    // S6c) over ALL elements in flat C-order (no null concept). Covers a
    // 1-D i64 tensor with ties, a 2-D tensor (flattened first), and an f64
    // tensor. `run` == `build` == default auto-par.
    let src = r#"
fn main() {
    let t: Tensor[i64, [6]] = Tensor.from([4, 2, 7, 2, 9, 9]);
    let ts: Vec[i64] = t.sorted();
    println(f"{ts[0]} {ts[1]} {ts[2]} {ts[3]} {ts[4]} {ts[5]}");
    let ta: Vec[i64] = t.argsort();
    println(f"{ta[0]} {ta[1]} {ta[2]} {ta[3]} {ta[4]} {ta[5]}");
    let m: Tensor[i64, [2, 3]] = Tensor.from([[3, 1, 4], [1, 5, 9]]);
    let ms: Vec[i64] = m.sorted();
    println(f"{ms[0]} {ms[1]} {ms[2]} {ms[3]} {ms[4]} {ms[5]}");
    let ma: Vec[i64] = m.argsort();
    println(f"{ma[0]} {ma[1]} {ma[2]} {ma[3]} {ma[4]} {ma[5]}");
    let tf: Tensor[f64, [4]] = Tensor.from([2.5, 8.0, 1.0, 8.0]);
    let fs: Vec[f64] = tf.sorted();
    println(f"{fs[0]} {fs[1]} {fs[2]} {fs[3]}");
    let fa: Vec[i64] = tf.argsort();
    println(f"{fa[0]} {fa[1]} {fa[2]} {fa[3]}");
}
"#;
    let out = run_program(src).expect("program should compile and run");
    // t sorted [2,2,4,7,9,9]; argsort stable [1,3,0,2,4,5].
    // m flat [3,1,4,1,5,9] sorted [1,1,3,4,5,9]; argsort [1,3,0,2,4,5].
    // tf sorted [1.0,2.5,8.0,8.0]; argsort [2,0,1,3].
    assert_eq!(
        out,
        "2 2 4 7 9 9\n1 3 0 2 4 5\n1 1 3 4 5 9\n1 3 0 2 4 5\n1 2.5 8 8\n2 0 1 3\n"
    );
}

#[test]
fn test_e2e_tensor_narrow_element_storage_and_ops() {
    // B-2026-07-03-35: `Tensor.from` / `Tensor.full` now store elements at
    // the annotated width (a bare int literal is i64, a float literal f64,
    // so an uncoerced store wrote 8 bytes into a narrow-strided slot). Every
    // reader — indexing, `sum`/`min`/`max`/`mean`, `map`/`reshape`/`zip_with`
    // — reads the right bytes. Covers i32 (indexing + reductions + map +
    // reshape + zip), u32 (sum), f32 (indexing + sum + mean, incl. the mean
    // f32→f64 divide fix), and an f64 tensor from INTEGER literals (sitofp).
    // `run` == `build` == default auto-par.
    let src = r#"
fn main() {
    let t: Tensor[i32, [2, 3]] = Tensor.from([[4, 2, 7], [1, 5, 3]]);
    println(f"{t[0, 0]} {t[1, 2]} {t.sum()} {t.min()} {t.max()}");
    let m: Tensor[i32, [2, 3]] = t.map(|x| x + 1);
    let r: Tensor[i32, [6]] = t.reshape([6]);
    let z: Tensor[i32, [2, 3]] = t.zip_with(t, |a, b| a + b);
    println(f"{m[0, 0]} {r[5]} {z[1, 2]}");
    let u: Tensor[u32, [3]] = Tensor.from([30, 10, 20]);
    let a: Tensor[i32, [3]] = Tensor.full([3], 7);
    println(f"{u.sum()} {a[0]} {a.sum()}");
    let f: Tensor[f32, [3]] = Tensor.from([1.5, 2.5, 3.5]);
    println(f"{f[0]} {f[2]} {f.sum()} {f.mean()}");
    let d: Tensor[f64, [3]] = Tensor.from([1, 2, 3]);
    println(f"{d[0]} {d.sum()}");
}
"#;
    let out = run_program(src).expect("program should compile and run");
    // t[0,0]=4 t[1,2]=3 sum=22 min=1 max=7; map+1 m[0,0]=5; reshape r[5]=3;
    // zip z[1,2]=6; u.sum=60; full a[0]=7 a.sum=21; f[0]=1.5 f[2]=3.5
    // f.sum=7.5 f.mean=2.5; d from ints [1,2,3] d[0]=1 d.sum=6.
    assert_eq!(out, "4 3 22 1 7\n5 3 6\n60 7 21\n1.5 3.5 7.5 2.5\n1 6\n");
}

#[test]
fn test_e2e_tensor_sorted_argsort_narrow_widths() {
    // B-2026-07-03-35 fixed → `Tensor.sorted`/`argsort` sort every numeric
    // width via the widened 8-byte scratch sort (i8/i16/i32 sext, u8/u16/u32
    // zext, f32 fpext), mirroring the Column narrow-widths path. Covers i32
    // (with ties), u32, and f32 tensors. `run` == `build` == default
    // auto-par. (u64 also sorts now — B-2026-07-07-2.)
    let src = r#"
fn main() {
    let ti: Tensor[i32, [5]] = Tensor.from([40, 10, 30, 20, 10]);
    let si: Vec[i32] = ti.sorted();
    println(f"{si[0]} {si[1]} {si[2]} {si[3]} {si[4]}");
    let ai: Vec[i64] = ti.argsort();
    println(f"{ai[0]} {ai[1]} {ai[2]} {ai[3]} {ai[4]}");
    let tu: Tensor[u32, [4]] = Tensor.from([40, 10, 30, 20]);
    let su: Vec[u32] = tu.sorted();
    println(f"{su[0]} {su[1]} {su[2]} {su[3]}");
    let au: Vec[i64] = tu.argsort();
    println(f"{au[0]} {au[1]} {au[2]} {au[3]}");
    let tf: Tensor[f32, [4]] = Tensor.from([2.5, 0.5, 3.5, 1.5]);
    let sf: Vec[f32] = tf.sorted();
    println(f"{sf[0]} {sf[1]} {sf[2]} {sf[3]}");
    let af: Vec[i64] = tf.argsort();
    println(f"{af[0]} {af[1]} {af[2]} {af[3]}");
}
"#;
    let out = run_program(src).expect("program should compile and run");
    // i32 [40,10,30,20,10] sorted [10,10,20,30,40]; argsort stable [1,4,3,2,0].
    // u32 [40,10,30,20] sorted [10,20,30,40]; argsort [1,3,2,0].
    // f32 [2.5,0.5,3.5,1.5] sorted [0.5,1.5,2.5,3.5]; argsort [1,3,0,2].
    assert_eq!(
        out,
        "10 10 20 30 40\n1 4 3 2 0\n10 20 30 40\n1 3 2 0\n0.5 1.5 2.5 3.5\n1 3 0 2\n"
    );
}

#[test]
fn test_e2e_refinement_i64_value_dispatch_unaffected() {
    // Refinements over i64 already work end-to-end (base layout
    // coincides with codegen's i64 default); the value-dispatch
    // normalization must not regress them.
    let out = run_program(
        r#"
type Even = i64 where self % 2 == 0;
fn main() {
    let e = 4 as Even;
    let f = e + 2;
    println(f);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "6");
    }
}

// ── `self.method()` self-dispatch (codegen method dispatch) ───────
//
// Inside an impl body, `self` parses as `ExprKind::SelfValue`, not
// `Identifier("self")`. Before this slice the user-method dispatch in
// `compile_method_call` only matched identifier receivers, so a method
// calling another method on `self` fell through to the "no handler for
// method 'X' on non-identifier receiver" codegen error. The fix adds a
// `SelfValue` arm to `inferred_receiver_type` (resolving the type via the
// synthesized `self` param registered in `var_type_names`) and to the
// receiver-storage path (addressing `self`'s alloca for the ptr-self ABI).

#[test]
fn test_e2e_self_dispatch_ref_self_returns_value() {
    // A `pub ref self` method calls a private `ref self` helper via
    // `self.doubled()` and uses its return value. Exercises the
    // SelfValue receiver through the ptr-self (ref) calling convention.
    let out = run_program(
        r#"
struct Counter { n: i64 }
impl Counter {
    fn doubled(ref self) -> i64 { self.n * 2 }
    pub fn report(ref self) -> i64 { self.doubled() + 1 }
}
fn main() { let c = Counter { n: 10 }; println(c.report()); }
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "21");
    }
}

#[test]
fn test_e2e_self_dispatch_mut_ref_self_mutation_persists() {
    // A `pub mut ref self` method calls a private `mut ref self` helper
    // via `self.bump()` twice. The mutation must address the *same*
    // storage `self` points at, so it accumulates and persists back to
    // the caller's `c`.
    let out = run_program(
        r#"
struct Counter { n: i64 }
impl Counter {
    fn bump(mut ref self) { self.n = self.n + 5; }
    pub fn run(mut ref self) -> i64 { self.bump(); self.bump(); self.n }
}
fn main() { let mut c = Counter { n: 0 }; println(c.run()); }
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "10");
    }
}

#[test]
fn test_e2e_self_dispatch_helper_invariant_fires() {
    // Ties self-dispatch back to contracts: a `pub` method calls a
    // private `mut ref self` helper via `self.dec()` that breaks the
    // `impl invariant`. Reaching the helper at all proves dispatch
    // works; the helper's own exit-invariant check then aborts.
    let captured = run_program_capturing(
        r#"
struct Counter { n: i64, impl invariant self.n >= 0 }
impl Counter {
    fn dec(mut ref self) { self.n = self.n - 1; }
    pub fn drive(mut ref self) { self.dec(); }
}
fn main() { let mut c = Counter { n: 0 }; c.drive(); println(42); }
"#,
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("contract violated"),
            "expected the helper's impl-invariant abort via self-dispatch, \
                 got stdout={:?} stderr={:?}",
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
fn test_e2e_tensor_map_f16_transcendental_matches_the_interpreter() {
    // The SECOND caller of `apply_vector_float_unary`, and the reason
    // B-2026-08-29-53's fix belongs at that shared site rather than in the
    // `Vector` method arm: the tensor-map vectorizer routes a
    // `t.map(|x| x.tanh())` inner loop through the identical lowering, so
    // it carried the identical NaN. Measured pre-fix at both a 4-element
    // and a 16-element tensor (`map tanh 1 NaN NaN NaN` either way), which
    // also rules out "it only bites when the map does not vectorize".
    assert_eq!(
        run_program(
            r#"
fn main() {
    let t: Tensor[f16, [4]] = Tensor.from([4.0f16, 6.0f16, 7.0f16, 20.0f16]);
    let m = t.map(|x| x.tanh());
    println(f"{m[0]} {m[1]} {m[2]} {m[3]}");
}
"#,
        ),
        Some("0.99951171875 1 1 1\n".to_string())
    );
}

// ── Tensor codegen (phase-11 numerical stdlib, core slice) ─────
// Value layout: one malloc'd `[i64 rank][rank × i64 dims][C-order
// data]` block, tensor value = single pointer. See
// `src/codegen/tensor.rs`.

#[test]
fn test_tensor_static_literal_index_elides_bounds_checks() {
    // Fully-static dims + literal indices: the typechecker already
    // proved the access in-bounds, so codegen emits no `t.idx.oob`
    // compare — the bounds-check elision the tracker commits to.
    let ir = ir_for(
        "fn main() {\n\
                 let t: Tensor[f64, [2, 3]] = Tensor.zeros([2, 3]);\n\
                 let x = t[1, 2];\n\
                 println(x);\n\
             }\n",
    );
    assert!(
        !ir.contains("t.idx.oob"),
        "static-dim literal-index access must elide bounds checks:\n{}",
        ir
    );
    // The construction-boundary dim asserts ARE present (array-
    // literal dims are runtime values checked against static dims).
    assert!(
        ir.contains("t.guard.fail"),
        "construction-boundary static-dim asserts must be emitted:\n{}",
        ir
    );
}

#[test]
fn test_tensor_runtime_index_emits_bounds_check() {
    let ir = ir_for(
        "fn main() {\n\
                 let t: Tensor[f64, [2, 3]] = Tensor.zeros([2, 3]);\n\
                 let i = 1;\n\
                 let x = t[i, 2];\n\
                 println(x);\n\
             }\n",
    );
    assert!(
        ir.contains("t.idx.oob"),
        "runtime-index access must emit a bounds check:\n{}",
        ir
    );
}

#[test]
fn test_e2e_tensor_constructors_index_shape() {
    let out = run_program(
        "fn main() {\n\
                 let t: Tensor[f64, [2, 3]] = Tensor.zeros([2, 3]);\n\
                 println(t.rank());\n\
                 let s = t.shape();\n\
                 println(s[0]);\n\
                 println(s[1]);\n\
                 println(t[1, 2]);\n\
                 let mut u: Tensor[i64, [2, 2]] = Tensor.full([2, 2], 7);\n\
                 u[0, 1] = 42;\n\
                 println(u[0, 0]);\n\
                 println(u[0, 1]);\n\
                 let f = Tensor.from([[1.5, 2.5], [3.5, 4.5]]);\n\
                 println(f[1, 0]);\n\
                 println(f.rank());\n\
                 let o: Tensor[f64, [4]] = Tensor.ones([4]);\n\
                 println(o[3]);\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(
            out, "2\n2\n3\n0\n7\n42\n3.5\n2\n1\n",
            "tensor constructors / multi-dim index / shape must match \
                 the interpreter's semantics",
        );
    }
}

#[test]
fn test_e2e_tensor_dynamic_dims_fn_boundary_moves() {
    // `?` dims read from the value header; tensors cross fn
    // boundaries as single pointers (owned arg + return); `let b =
    // a;` moves without double-free (FreeTensor null-guard +
    // move-suppression null store).
    let out = run_program(
        "fn make() -> Tensor[f64, [2, 2]] {\n\
                 let t: Tensor[f64, [2, 2]] = Tensor.full([2, 2], 9.0);\n\
                 t\n\
             }\n\
             fn first(t: Tensor[f64, [2, 2]]) -> f64 {\n\
                 t[0, 0]\n\
             }\n\
             fn main() {\n\
                 let d: Tensor[f64, [2, ?]] = Tensor.zeros([2, 5]);\n\
                 let s = d.shape();\n\
                 println(s[1]);\n\
                 println(d[1, 4]);\n\
                 let m = make();\n\
                 println(m[1, 1]);\n\
                 let x = first(make());\n\
                 println(x);\n\
                 let a = Tensor.from([[1, 2], [3, 4]]);\n\
                 let b = a;\n\
                 println(b[1, 0]);\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(
            out, "5\n0\n9\n9\n3\n",
            "dynamic dims / fn-boundary / move semantics must hold",
        );
    }
}

#[test]
fn test_e2e_tensor_construction_dim_mismatch_panics() {
    // The construction-boundary check of design.md § Runtime
    // equality check: runtime dims must agree with every static
    // annotated dim.
    let captured = run_program_capturing(
        "fn main() {\n\
                 let bad: Tensor[f64, [3, ?]] = Tensor.zeros([7, 5]);\n\
                 println(bad.rank());\n\
             }\n",
    );
    if let Some(c) = captured {
        assert!(
            c.stderr
                .contains("runtime dim 0 does not match the annotated static dim"),
            "expected the construction-boundary dim assert, got stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
        assert!(
            !c.stdout.contains("\n3\n"),
            "must trap before reaching the post-construction println"
        );
    }
}

#[test]
fn test_e2e_tensor_runtime_index_oob_panics() {
    let captured = run_program_capturing(
        "fn main() {\n\
                 let t: Tensor[f64, [2, ?]] = Tensor.zeros([2, 5]);\n\
                 let i = 9;\n\
                 println(t[1, i]);\n\
             }\n",
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("tensor index out of bounds for dim 1"),
            "expected the runtime index bounds trap, got stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
    }
}

#[test]
fn test_e2e_tensor_dims_from_vec_variable() {
    // Non-literal dims argument: read through the Vec value, with
    // the rank-agreement assert.
    let out = run_program(
        "fn main() {\n\
                 let dims = [2, 3];\n\
                 let t: Tensor[i64, [2, 3]] = Tensor.zeros(dims);\n\
                 println(t[1, 2]);\n\
                 println(t.rank());\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(out, "0\n2\n", "Vec-variable dims must construct correctly");
    }
}

// ── Shape-generic function body — tensor-param indexing ──────────
// A `fn f[N](a: Tensor[T, [N, N]], ...)` body that indexes its tensor
// params (`a[i, j]`) lowers in codegen: `compile_mono_function`
// registers each `Tensor` param in `tensor_var_infos` (the shape
// literal's named `Dim`s become runtime `?` dims read from the header;
// the element type resolves through the active `type_subst`), so the
// multi-dim index / `shape()` / transform lowering applies inside the
// monomorphized body. Before this the params were opaque pointers and
// `a[i, j]` died with "Index operator applied to non-array type".
// (Tensor *locals* in such bodies and inline `a.shape()[k]` are
// separate gaps — see phase-11-stdlib-longtail.md.)

#[test]
fn test_e2e_shape_generic_body_tensor_param_index() {
    // `a[i, i]` on a shape-generic tensor param, scalar return — the
    // diagonal trace. Dim bound passed explicitly (avoids the separate
    // inline-`shape()[k]` gap).
    let out = run_program(
            "fn trace_diag[N](a: Tensor[f64, [N, N]], n: i64) -> f64 {\n\
                 let mut s = 0.0;\n\
                 for i in 0..n { s = s + a[i, i]; }\n\
                 s\n\
             }\n\
             fn main() {\n\
                 let a: Tensor[f64, [3, 3]] = Tensor.from([[1.0,2.0,3.0],[4.0,5.0,6.0],[7.0,8.0,9.0]]);\n\
                 println(trace_diag(a, 3));\n\
             }\n",
        );
    if let Some(out) = out {
        assert_eq!(out, "15\n", "1+5+9 = 15 (codegen must match karac run)");
    }
}

#[test]
fn test_e2e_shape_generic_body_two_tensor_params() {
    // Two shape-generic tensor params, both indexed in the body — proves
    // each registers independently in the mono body.
    let out = run_program(
        "fn diag_sum[N](a: Tensor[f64, [N, N]], b: Tensor[f64, [N, N]], n: i64) -> f64 {\n\
                 let mut s = 0.0;\n\
                 for i in 0..n { s = s + a[i, i] + b[i, i]; }\n\
                 s\n\
             }\n\
             fn main() {\n\
                 let a: Tensor[f64, [2, 2]] = Tensor.from([[1.0, 2.0], [3.0, 4.0]]);\n\
                 let b: Tensor[f64, [2, 2]] = Tensor.from([[10.0, 0.0], [0.0, 20.0]]);\n\
                 println(diag_sum(a, b, 2));\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(out, "35\n", "1+4+10+20 = 35 (codegen must match karac run)");
    }
}

#[test]
fn test_shape_generic_body_tensor_param_index_ir_lowers() {
    // The generic body compiles (no "Index operator applied to
    // non-array type") and the mono specialization is emitted.
    let ir = ir_for(
        "fn first_elem[N](a: Tensor[f64, [N, N]]) -> f64 { a[0, 0] }\n\
             fn main() {\n\
                 let a: Tensor[f64, [2, 2]] = Tensor.zeros([2, 2]);\n\
                 println(first_elem(a));\n\
             }\n",
    );
    assert!(
        ir.contains("@first_elem"),
        "the shape-generic specialization must be emitted"
    );
}

// ── Mono-body owned-local cleanup ────────────────────────────────
// A monomorphized body that binds an owned local needing drop (a
// `Tensor` / `Vec` / `String` local) now compiles: `compile_mono_function`
// pushes a function-level `scope_cleanup_actions` frame and drains it
// (with move-aware tail suppression) at the return, and
// `compile_generic_call` saves/restores that frame stack + the
// `branch_cancel_ptr` so the inline body is hermetic. Before this, a
// local's `FreeTensor` cleanup leaked into the caller's frame ("does
// not dominate all uses"), and an auto-par branch from an earlier mono
// left a stale cancel ptr that the next mono's first call referenced
// ("Referring to an argument in another function").

#[test]
fn test_e2e_mono_body_builds_and_returns_tensor() {
    // Full matmul: the generic body builds its `out` result tensor
    // (`Tensor.zeros` local), fills it in auto-par-eligible nested
    // loops, and returns it — the returned local is moved out
    // (tail-return suppression), the caller owns + frees it.
    let out = run_program(
            "fn matmul[M, K, N](a: Tensor[f64, [M, K]], b: Tensor[f64, [K, N]], m: i64, k: i64, n: i64) -> Tensor[f64, [M, N]] {\n\
                 let mut out: Tensor[f64, [?, ?]] = Tensor.zeros([m, n]);\n\
                 for i in 0..m {\n\
                     for j in 0..n {\n\
                         let mut acc = 0.0;\n\
                         for p in 0..k { acc = acc + a[i, p] * b[p, j]; }\n\
                         out[i, j] = acc;\n\
                     }\n\
                 }\n\
                 out\n\
             }\n\
             fn main() {\n\
                 let a: Tensor[f64, [2, 3]] = Tensor.from([[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]]);\n\
                 let b: Tensor[f64, [3, 2]] = Tensor.from([[1.0, 0.0], [0.0, 1.0], [1.0, 1.0]]);\n\
                 let c = matmul(a, b, 2, 3, 2);\n\
                 println(c[0, 0]); println(c[0, 1]); println(c[1, 0]); println(c[1, 1]);\n\
             }\n",
        );
    if let Some(out) = out {
        assert_eq!(
            out, "4\n5\n10\n11\n",
            "matmul result (codegen must match karac run)"
        );
    }
}

#[test]
fn test_e2e_mono_body_drops_tensor_local() {
    // The generic body binds a `Tensor` local that is NOT returned — its
    // `FreeTensor` must fire at the mono's scope exit (and not corrupt
    // the function, which returns a scalar). Two generic fns where the
    // first's auto-par'd loop previously left a stale cancel ptr for the
    // second — the regression both fixes guard.
    let out = run_program(
        "fn build_id[N](n: i64) -> Tensor[f64, [N, N]] {\n\
                 let mut out: Tensor[f64, [?, ?]] = Tensor.zeros([n, n]);\n\
                 for i in 0..n { out[i, i] = 1.0; }\n\
                 out\n\
             }\n\
             fn diag_then_drop[N](n: i64) -> f64 {\n\
                 let mut t: Tensor[f64, [?, ?]] = Tensor.zeros([n, n]);\n\
                 for i in 0..n { t[i, i] = 2.0; }\n\
                 let mut s = 0.0;\n\
                 for i in 0..n { s = s + t[i, i]; }\n\
                 s\n\
             }\n\
             fn main() {\n\
                 let a = build_id(3);\n\
                 println(a[2, 2]);\n\
                 println(diag_then_drop(4));\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(
            out, "1\n8\n",
            "build_id diagonal + diag_then_drop sum (codegen must match karac run)"
        );
    }
}

// ── Cross-argument `?`-dim asserts at a call boundary ────────────
// design.md § Runtime equality check, the call-boundary flavor: two
// `Tensor` params sharing a named `Dim` (the `K` in
// `mm(a: [M, K], b: [K, N])`) must bind equal argument dims; the
// compiler inserts the check the type system can't prove statically
// for two `?` dims. Emitted in `compile_generic_call` via
// `emit_tensor_crossarg_dim_asserts`. The callee body is trivial here
// (the asserts fire at the call site, before the body); shape-generic
// tensor-param *indexing* now lowers (cluster above), but tensor
// *locals* / inline `shape()[k]` in such bodies remain separate gaps
// (tracked in phase-11-stdlib-longtail.md).

#[test]
fn test_e2e_tensor_crossarg_dim_match_ok() {
    // K agrees (4 == 4): no trap, the trivial body runs.
    let captured = run_program_capturing(
        "fn mm[M, K, N](a: Tensor[f64, [M, K]], b: Tensor[f64, [K, N]]) -> f64 { 99.0 }\n\
             fn mk(rows: i64, cols: i64) -> Tensor[f64, [?, ?]] {\n\
                 let t: Tensor[f64, [?, ?]] = Tensor.zeros([rows, cols]);\n\
                 t\n\
             }\n\
             fn main() {\n\
                 let a: Tensor[f64, [?, ?]] = mk(2, 4);\n\
                 let b: Tensor[f64, [?, ?]] = mk(4, 5);\n\
                 println(mm(a, b));\n\
             }\n",
    );
    if let Some(c) = captured {
        assert!(
            c.stdout.contains("99"),
            "matching K must not trap; stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
        assert!(
            !c.stdout.contains("shape mismatch"),
            "no cross-argument trap on agreeing dims; stdout={:?}",
            c.stdout
        );
    }
}

#[test]
fn test_e2e_tensor_crossarg_dim_mismatch_dynamic_panics() {
    // Both K positions are `?` (runtime): a's dim 1 = 4, b's dim 0 = 7.
    // The type system can't prove the inequality, so the inserted
    // equality check traps before `mm`'s body runs.
    let captured = run_program_capturing(
        "fn mm[M, K, N](a: Tensor[f64, [M, K]], b: Tensor[f64, [K, N]]) -> f64 { 99.0 }\n\
             fn mk(rows: i64, cols: i64) -> Tensor[f64, [?, ?]] {\n\
                 let t: Tensor[f64, [?, ?]] = Tensor.zeros([rows, cols]);\n\
                 t\n\
             }\n\
             fn main() {\n\
                 let a: Tensor[f64, [?, ?]] = mk(2, 4);\n\
                 let b: Tensor[f64, [?, ?]] = mk(7, 5);\n\
                 println(mm(a, b));\n\
             }\n",
    );
    if let Some(c) = captured {
        assert!(
            c.stderr
                .contains("shape mismatch — dim 'K' differs between arguments"),
            "expected the cross-argument equality trap; stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
        assert!(
            !c.stdout.contains("99"),
            "must trap before reaching the callee body / println"
        );
    }
}

#[test]
fn test_e2e_tensor_crossarg_dim_mismatch_static_panics() {
    // One K position is concrete (b: [4, 5] ⇒ K = 4), the other is
    // `?` (a: [3, ?], runtime dim 1 = 7). The check folds to a bounds
    // check of a's runtime dim against the static value 4.
    let captured = run_program_capturing(
        "fn mm[M, K, N](a: Tensor[f64, [M, K]], b: Tensor[f64, [K, N]]) -> f64 { 99.0 }\n\
             fn mk(rows: i64, cols: i64) -> Tensor[f64, [3, ?]] {\n\
                 let t: Tensor[f64, [3, ?]] = Tensor.zeros([rows, cols]);\n\
                 t\n\
             }\n\
             fn main() {\n\
                 let a: Tensor[f64, [3, ?]] = mk(3, 7);\n\
                 let b: Tensor[f64, [4, 5]] = Tensor.zeros([4, 5]);\n\
                 println(mm(a, b));\n\
             }\n",
    );
    if let Some(c) = captured {
        assert!(
            c.stderr
                .contains("shape mismatch — dim 'K' of argument 'a' (dim 1) must be 4"),
            "expected the concrete-vs-? bounds trap; stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
        assert!(
            !c.stdout.contains("99"),
            "must trap before reaching the callee body / println"
        );
    }
}

#[test]
fn test_tensor_crossarg_dim_assert_ir_present_and_absent() {
    // A shared named dim (`K`) emits the equality guard; a signature
    // with no shared dim emits none.
    let shared = ir_for(
        "fn mm[M, K, N](a: Tensor[f64, [M, K]], b: Tensor[f64, [K, N]]) -> f64 { 0.0 }\n\
             fn mk(r: i64, c: i64) -> Tensor[f64, [?, ?]] {\n\
                 let t: Tensor[f64, [?, ?]] = Tensor.zeros([r, c]);\n\
                 t\n\
             }\n\
             fn main() {\n\
                 let a: Tensor[f64, [?, ?]] = mk(2, 4);\n\
                 let b: Tensor[f64, [?, ?]] = mk(4, 5);\n\
                 println(mm(a, b));\n\
             }\n",
    );
    assert!(
        shared.contains("t.kdim.ok"),
        "shared named dim must emit the cross-argument equality guard"
    );

    let unshared = ir_for(
        "fn pairfn[A, B, C, D](a: Tensor[f64, [A, B]], b: Tensor[f64, [C, D]]) -> f64 { 0.0 }\n\
             fn mk(r: i64, c: i64) -> Tensor[f64, [?, ?]] {\n\
                 let t: Tensor[f64, [?, ?]] = Tensor.zeros([r, c]);\n\
                 t\n\
             }\n\
             fn main() {\n\
                 let a: Tensor[f64, [?, ?]] = mk(2, 4);\n\
                 let b: Tensor[f64, [?, ?]] = mk(4, 5);\n\
                 println(pairfn(a, b));\n\
             }\n",
    );
    assert!(
        !unshared.contains("t.kdim.ok"),
        "no shared dim ⇒ no cross-argument guard"
    );
}

// ── Tensor shape-transform family codegen (phase-11, 2026-06-08) ──
// `reshape` / `permute` / `slice` / `squeeze`: each produces a fresh
// copy; rank/dims read from the runtime header, element type from
// the result side-table. See `src/codegen/tensor.rs §
// Shape-transform family` and the interpreter twins in
// `src/interpreter/method_call_tensor.rs`.

#[test]
fn test_tensor_reshape_emits_data_copy() {
    // reshape reads the receiver's element count from the header
    // (the `t.cnt.*` loop) and copies the data block with a memcpy
    // into a fresh allocation.
    let ir = ir_for(
        "fn main() {\n\
                 let a = Tensor.from([[1, 2, 3], [4, 5, 6]]);\n\
                 let r = a.reshape([3, 2]);\n\
                 println(r[2, 1]);\n\
             }\n",
    );
    assert!(
        ir.contains("t.cnt.head"),
        "reshape must read the receiver element count from the header:\n{}",
        ir
    );
    assert!(
        ir.contains("llvm.memcpy"),
        "reshape must copy the data block (memcpy) — tensors are value types:\n{}",
        ir
    );
}

#[test]
fn test_tensor_permute_emits_reorder_loop() {
    // permute reorders elements one-by-one through a div/rem
    // decomposition of the output flat index (`t.prm.coord`).
    let ir = ir_for(
        "fn main() {\n\
                 let a = Tensor.from([[1, 2, 3], [4, 5, 6]]);\n\
                 let p = a.permute([1, 0]);\n\
                 println(p[2, 1]);\n\
             }\n",
    );
    assert!(
        ir.contains("t.prm.head") && ir.contains("t.prm.coord"),
        "permute must emit the per-element reorder loop:\n{}",
        ir
    );
}

#[test]
fn test_e2e_tensor_matmul_integer_overflow_traps() {
    // B-2026-08-20-27, the AOT half of
    // `tests/interpreter.rs::test_tensor_matmul_integer_overflow_traps`.
    // Before the fix this printed `0` — the wrapped i64 — where the
    // interpreter printed 8589934592, so the pair was a silent miscompile
    // and a run/build divergence at once. Both surfaces now trap, and the
    // pair fails if either one stops.
    let src = "fn main() {\n\
             \x20   let a: Tensor[i32, [?, ?]] = Tensor.from([[65536, 65536]]);\n\
             \x20   let b: Tensor[i32, [?, ?]] = Tensor.from([[65536], [65536]]);\n\
             \x20   println(a.matmul(b)[0, 0]);\n\
             }";
    if let Some(cap) = run_program_capturing(src) {
        assert_eq!(cap.status.code(), Some(101), "stderr={:?}", cap.stderr);
        assert!(
            cap.stderr.contains("integer overflow"),
            "an overflowing integer matmul must trap under `karac build`, \
                 got stdout={:?} stderr={:?}",
            cap.stdout,
            cap.stderr
        );
    }
}

#[test]
fn test_e2e_tensor_matmul_integer_in_range_is_unchanged() {
    // The checked arithmetic must not cost the ordinary case, signed or
    // unsigned.
    let out = run_program(
        "fn main() {\n\
             \x20   let a: Tensor[i32, [?, ?]] = Tensor.from([[1, 2], [3, 4]]);\n\
             \x20   let b: Tensor[i32, [?, ?]] = Tensor.from([[5, 6], [7, 8]]);\n\
             \x20   let c = a.matmul(b);\n\
             \x20   println(c[0, 0]); println(c[1, 1]);\n\
             \x20   let u: Tensor[u32, [?, ?]] = Tensor.from([[1u32, 2u32]]);\n\
             \x20   let v: Tensor[u32, [?, ?]] = Tensor.from([[3u32], [4u32]]);\n\
             \x20   println(u.matmul(v)[0, 0]);\n\
             }",
    );
    assert_eq!(out, Some("19\n50\n11\n".to_string()));
}

#[test]
fn test_e2e_tensor_matmul_f32_accumulates_at_element_width() {
    // B-2026-08-20-21, the AOT half of
    // `tests/interpreter.rs::test_tensor_matmul_f32_accumulates_at_element_width`.
    // Codegen already accumulated in the element LLVM type; this pins the
    // value the interpreter was fixed to match, so a later change to
    // EITHER surface fails one of the pair rather than silently reopening
    // the divergence. See the interpreter test for why the fixture looks
    // the way it does.
    let out = run_program(
            "fn main() {\n\
             \x20   let a: Tensor[f32, [?, ?]] = Tensor.from([[1.0, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001]]);\n\
             \x20   let b: Tensor[f32, [?, ?]] = Tensor.from([[1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0]]);\n\
             \x20   println(a.matmul(b)[0, 0]);\n\
             }",
        );
    assert_eq!(out, Some("1\n".to_string()));
}

#[test]
fn test_e2e_tensor_matmul_and_transpose() {
    // B-2026-07-14-18 (was a phantom method pair: typechecker accepted
    // matmul/transpose, neither backend implemented them). matmul:
    // [[1,2],[3,4]] @ [[5,6],[7,8]] = [[19,22],[43,50]]; non-square
    // [2x3] @ [3x2] = [[58,64],[139,154]]; integer elements accumulate
    // in the integer domain. transpose reverses the axes (rank 2 and 3;
    // rank-1 identity), and chains with matmul (the chained receiver is
    // a fresh temp that must be freed — ASAN twin covers the leak side).
    let out = run_program(
        "fn main() {\n\
                 let a = Tensor.from([[1.0, 2.0], [3.0, 4.0]]);\n\
                 let b = Tensor.from([[5.0, 6.0], [7.0, 8.0]]);\n\
                 let c = a.matmul(b);\n\
                 println(c[0, 0]); println(c[0, 1]); println(c[1, 0]); println(c[1, 1]);\n\
                 let w = Tensor.from([[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]]);\n\
                 let x = Tensor.from([[7.0, 8.0], [9.0, 10.0], [11.0, 12.0]]);\n\
                 let y = w.matmul(x);\n\
                 println(y[0, 0]); println(y[0, 1]); println(y[1, 0]); println(y[1, 1]);\n\
                 let ia = Tensor.from([[1, 2], [3, 4]]);\n\
                 let ib = Tensor.from([[5, 6], [7, 8]]);\n\
                 let ic = ia.matmul(ib);\n\
                 println(ic[0, 0]); println(ic[1, 1]);\n\
                 let t = w.transpose();\n\
                 let ts = t.shape();\n\
                 println(ts[0]); println(ts[1]);\n\
                 println(t[0, 0]); println(t[0, 1]); println(t[2, 1]);\n\
                 let t3 = Tensor.from([[[1, 2], [3, 4]], [[5, 6], [7, 8]]]);\n\
                 let t3t = t3.transpose();\n\
                 println(t3t[0, 0, 0]); println(t3t[1, 0, 0]); println(t3t[0, 1, 1]);\n\
                 let chained = a.matmul(b).transpose();\n\
                 println(chained[0, 1]); println(chained[1, 0]);\n\
                 let v = Tensor.from([9.0, 8.0]);\n\
                 let vt = v.transpose();\n\
                 println(vt[1]);\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(
            out,
            "19\n22\n43\n50\n58\n64\n139\n154\n19\n50\n3\n2\n1\n4\n6\n1\n2\n7\n43\n22\n8\n"
        );
    }
}

#[test]
fn test_e2e_tensor_shape_transforms() {
    // All four transforms in one program — AOT output is verified
    // byte-identical to `karac run` (the interpreter twins). Covers
    // reshape (C-order preserved), permute (transpose reordering),
    // slice (contiguous band along an axis), and both squeeze forms.
    let out = run_program(
        "fn main() {\n\
                 let a = Tensor.from([[1, 2, 3], [4, 5, 6]]);\n\
                 let r = a.reshape([3, 2]);\n\
                 println(r.rank());\n\
                 let rs = r.shape();\n\
                 println(rs[0]); println(rs[1]);\n\
                 println(r[0,0]); println(r[0,1]); println(r[2,1]);\n\
                 let p = a.permute([1, 0]);\n\
                 let ps = p.shape();\n\
                 println(ps[0]); println(ps[1]);\n\
                 println(p[0,0]); println(p[0,1]); println(p[2,1]);\n\
                 let sl = a.slice(1, 1, 3);\n\
                 let sls = sl.shape();\n\
                 println(sls[0]); println(sls[1]);\n\
                 println(sl[0,0]); println(sl[1,1]);\n\
                 let b = Tensor.from([[[7], [8], [9]]]);\n\
                 let sq = b.squeeze();\n\
                 println(sq.rank()); println(sq[0]); println(sq[2]);\n\
                 let c = Tensor.from([[[1, 2]], [[3, 4]]]);\n\
                 let sq1 = c.squeeze(1);\n\
                 println(sq1.rank()); println(sq1[0,0]); println(sq1[1,1]);\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(
            out, "2\n3\n2\n1\n2\n6\n3\n2\n1\n4\n6\n2\n2\n2\n6\n1\n7\n9\n2\n1\n4\n",
            "shape-transform AOT output must match the interpreter twins",
        );
    }
}

#[test]
fn test_e2e_tensor_shape_transform_chained() {
    // Chained transforms (`a.permute(..).reshape(..)`): the inner
    // call's result pointer feeds the outer call via `compile_expr`;
    // the receiver pointer comes from the value, not a binding slot.
    let out = run_program(
        "fn main() {\n\
                 let a = Tensor.from([[1, 2, 3], [4, 5, 6]]);\n\
                 let r = a.permute([1, 0]).reshape([6]);\n\
                 println(r[0]); println(r[1]); println(r[5]);\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(
            out, "1\n4\n6\n",
            "chained transpose→reshape must reorder then flatten"
        );
    }
}

#[test]
fn test_e2e_tensor_fnret_receiver() {
    // A shape transform whose receiver is a fresh OWNED tensor
    // temporary — a free-function return (`make().reshape(..)`) or a
    // non-transform method return (`f.build().slice(..)`) — must read
    // the temp's header correctly AND free it after the copy
    // (phase-11 line 39: `tensor_receiver_is_owned_fresh_temp`).
    // Output correctness here proves the receiver pointer is sourced
    // from the call value and the receiver free does not corrupt the
    // result; the leak fix itself is pinned by the ASAN lifecycle
    // test (Linux detect_leaks).
    let out = run_program(
        "fn make() -> Tensor[i64, [2, 3]] {\n\
                 Tensor.from([[1, 2, 3], [4, 5, 6]])\n\
             }\n\
             struct Factory {}\n\
             impl Factory {\n\
                 fn build(ref self) -> Tensor[i64, [2, 3]] {\n\
                     Tensor.from([[10, 20, 30], [40, 50, 60]])\n\
                 }\n\
             }\n\
             fn main() {\n\
                 let r = make().reshape([3, 2]);\n\
                 println(r[0, 0]); println(r[2, 1]);\n\
                 let f = Factory {};\n\
                 let m = f.build().slice(0, 1, 2);\n\
                 println(m[0, 0]); println(m[0, 2]);\n\
                 let p = make().permute([1, 0]);\n\
                 println(p[0, 0]); println(p[2, 1]);\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(
            out, "1\n6\n40\n60\n1\n6\n",
            "owned fn-return / method-return receiver transforms must \
                 match the interpreter twins",
        );
    }
}

#[test]
fn test_e2e_tensor_iter_axis_bound_form_collapse_respects_its_guards() {
    // B-2026-07-29-24. `let rows = t.iter_axis(0); for row in rows` is
    // collapsed into the direct form so it reaches the fused lowering —
    // but only when the binding is used exactly once AND the loop
    // immediately follows, since collapsing moves the call to the loop's
    // position. Four shapes, each byte-checked against the interpreter:
    //
    //   A  single use, adjacent      -> collapses (and fuses)
    //   B  iterated twice            -> must not collapse; both loops right
    //   C  a statement in between    -> must not collapse
    //   D  also read via `.len()`    -> two uses, must not collapse
    //
    // B and D are the ones that would silently corrupt: collapsing them
    // would leave the second reader with no binding at all.
    let output = run_program(
        "fn main() {\n\
                 let mut t: Tensor[f32, [4, 3]] = Tensor.zeros([4, 3]);\n\
                 let mut i = 0;\n\
                 while i < 4 {\n\
                     let mut j = 0;\n\
                     while j < 3 { t[i, j] = (i * 3 + j) as f32; j = j + 1; }\n\
                     i = i + 1;\n\
                 }\n\
                 let ra = t.iter_axis(0);\n\
                 let mut a: f32 = 0.0;\n\
                 for row in ra { a = a + row.sum(); }\n\
                 println(a.to_string());\n\
                 let rb = t.iter_axis(0);\n\
                 let mut b1: f32 = 0.0;\n\
                 for row in rb { b1 = b1 + row.sum(); }\n\
                 let mut b2: f32 = 0.0;\n\
                 for row in rb { b2 = b2 + row.sum(); }\n\
                 println((b1 + b2).to_string());\n\
                 let rc = t.iter_axis(0);\n\
                 println(\"mid\");\n\
                 let mut c: f32 = 0.0;\n\
                 for row in rc { c = c + row.sum(); }\n\
                 println(c.to_string());\n\
                 let rd = t.iter_axis(0);\n\
                 let n = rd.len();\n\
                 let mut d: f32 = 0.0;\n\
                 for row in rd { d = d + row.sum(); }\n\
                 println((n as f32 + d).to_string());\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "66\n132\nmid\n66\n70\n");
}

#[test]
fn test_e2e_tensor_iter_axis_fused_loop_matches_the_materializing_path() {
    // B-2026-07-29-13. Seven shapes through the fused `for row in
    // t.iter_axis(n)` lowering, each byte-checked against the interpreter
    // separately: read-only body, a strided axis-1 gather, `break`,
    // `continue`, a row that ESCAPES into a Vec (must fall back to the
    // materializing path and still be correct), the `zip_with(row, …)`
    // argument position the shipping kernels use, and nested fused loops
    // sharing one receiver.
    //
    // The reused row buffer makes the escape case load-bearing: if the
    // guard ever let it fuse, the pushed values would all read back as the
    // last row.
    let output = run_program(
        "fn main() {\n\
                 let mut t: Tensor[f32, [4, 3]] = Tensor.zeros([4, 3]);\n\
                 let mut i = 0;\n\
                 while i < 4 {\n\
                     let mut j = 0;\n\
                     while j < 3 { t[i, j] = (i * 3 + j) as f32; j = j + 1; }\n\
                     i = i + 1;\n\
                 }\n\
                 let mut a: f32 = 0.0;\n\
                 for row in t.iter_axis(0) { a = a + row.sum(); }\n\
                 println(a.to_string());\n\
                 let mut b: f32 = 0.0;\n\
                 for col in t.iter_axis(1) { b = b + col.sum(); }\n\
                 println(b.to_string());\n\
                 let mut c: f32 = 0.0;\n\
                 for row in t.iter_axis(0) {\n\
                     c = c + row.sum();\n\
                     if c > 10.0 { break; }\n\
                 }\n\
                 println(c.to_string());\n\
                 let mut d: f32 = 0.0;\n\
                 let mut k = 0;\n\
                 for row in t.iter_axis(0) {\n\
                     k = k + 1;\n\
                     if k == 2 { continue; }\n\
                     d = d + row.sum();\n\
                 }\n\
                 println(d.to_string());\n\
                 let mut keep: Vec[f32] = Vec.new();\n\
                 for row in t.iter_axis(0) { keep.push(row.sum()); }\n\
                 println(keep.len().to_string());\n\
                 let q: Tensor[f32, [3]] = Tensor.ones([3]);\n\
                 let mut e: f32 = 0.0;\n\
                 for row in t.iter_axis(0) { e = e + q.zip_with(row, |x, y| x * y).sum(); }\n\
                 println(e.to_string());\n\
                 let mut f: f32 = 0.0;\n\
                 for r1 in t.iter_axis(0) {\n\
                     for r2 in t.iter_axis(0) { f = f + r1.sum() + r2.sum(); }\n\
                 }\n\
                 println(f.to_string());\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "66\n66\n15\n54\n4\n66\n528\n");
}

#[test]
fn test_e2e_tensor_ref_return() {
    // `ref Tensor` / `mut ref Tensor` returns use the BY-VALUE ABI
    // (phase-11 line 40): a tensor value is a single block pointer, so
    // a borrow needs no extra indirection. Before the fix the caller
    // did an extra load (dereferencing the rank word as a pointer →
    // garbage), trapping in AOT though it worked under `karac run`.
    // Covers all four forms: free-fn return used inline as a transform
    // receiver and bound to a let (a borrow, indexable, not dropped);
    // and a user `-> ref Tensor` accessor method, likewise inline and
    // let-bound. The trailing `a[0,0]` / second `h.view()` confirm the
    // owner's block is still live (the borrows did not free it).
    let out = run_program(
        "fn firstrow(t: ref Tensor[i64, [2, 3]]) -> ref Tensor[i64, [2, 3]] {\n\
                 t\n\
             }\n\
             struct Holder { t: Tensor[i64, [2, 3]] }\n\
             impl Holder {\n\
                 fn view(ref self) -> ref Tensor[i64, [2, 3]] {\n\
                     self.t\n\
                 }\n\
             }\n\
             fn main() {\n\
                 let a = Tensor.from([[1, 2, 3], [4, 5, 6]]);\n\
                 let r = firstrow(a).reshape([3, 2]);\n\
                 println(r[2, 1]);\n\
                 let b = firstrow(a);\n\
                 println(b[1, 2]);\n\
                 println(a[0, 0]);\n\
                 let h = Holder { t: Tensor.from([[10, 20, 30], [40, 50, 60]]) };\n\
                 let m = h.view().permute([1, 0]);\n\
                 println(m[2, 1]);\n\
                 let v = h.view();\n\
                 println(v[1, 2]);\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(
            out, "6\n6\n1\n60\n60\n",
            "ref Tensor returns (free-fn + method, inline + let-bound) \
                 must match the interpreter twins",
        );
    }
}

#[test]
fn test_e2e_tensor_slice_runtime_axis() {
    // Runtime-valued slice bounds degrade the sliced dim to `?` in
    // the type but codegen reads everything from the header, so the
    // band is computed correctly at runtime.
    let out = run_program(
        "fn main() {\n\
                 let a = Tensor.from([[1, 2, 3, 4], [5, 6, 7, 8]]);\n\
                 let s = 1;\n\
                 let e = 3;\n\
                 let sl = a.slice(1, s, e);\n\
                 let sh = sl.shape();\n\
                 println(sh[0]); println(sh[1]);\n\
                 println(sl[0,0]); println(sl[0,1]); println(sl[1,1]);\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(
            out, "2\n2\n2\n3\n7\n",
            "runtime-axis slice band must be correct"
        );
    }
}

#[test]
fn test_e2e_tensor_reshape_count_mismatch_panics() {
    // The element-count product is asserted equal to the receiver's
    // at runtime (re-emitted from the typechecker's compile-time
    // check, since `run_program` doesn't gate on typecheck).
    let captured = run_program_capturing(
        "fn main() {\n\
                 let a = Tensor.from([[1, 2, 3], [4, 5, 6]]);\n\
                 let n = 4;\n\
                 let bad = a.reshape([n]);\n\
                 println(bad[0]);\n\
             }\n",
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("reshape element counts must match"),
            "expected the reshape count-mismatch trap, got stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
    }
}

#[test]
fn test_e2e_tensor_slice_oob_panics() {
    let captured = run_program_capturing(
        "fn main() {\n\
                 let a = Tensor.from([[1, 2, 3], [4, 5, 6]]);\n\
                 let e = 9;\n\
                 let sl = a.slice(1, 0, e);\n\
                 println(sl[0,0]);\n\
             }\n",
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("slice end out of bounds for the axis"),
            "expected the slice bounds trap, got stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
    }
}

#[test]
fn test_e2e_tensor_squeeze_nonunit_panics() {
    // `squeeze(n)` on a `?`/runtime-axis non-1 dim is checked at
    // runtime (the typechecker can't prove the size statically).
    let captured = run_program_capturing(
        "fn main() {\n\
                 let c = Tensor.from([[[1, 2]], [[3, 4]]]);\n\
                 let n = 0;\n\
                 let sq = c.squeeze(n);\n\
                 println(sq[0,0]);\n\
             }\n",
    );
    if let Some(c) = captured {
        assert!(
            c.stderr
                .contains("cannot squeeze an axis whose size is not 1"),
            "expected the squeeze size-1 trap, got stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
    }
}

// ── Tensor iter_axis codegen (phase-11 follow-on, 2026-06-08) ──
// `t.iter_axis(n)` returns `Vec[Tensor[...]]` (or `Vec[T]` for a
// rank-1 receiver): the bucket gather loop in `src/codegen/tensor.rs`,
// the Vec-of-pointer element drop (`track_vec_of_tensors_var` +
// `cleanup.tdrop` in runtime.rs), and the for-loop method-source
// materialization in control_flow_for.rs.

#[test]
fn test_tensor_iter_axis_emits_bucket_loop_and_tensor_drop() {
    let ir = ir_for(
        "fn main() {\n\
                 let a = Tensor.from([[1, 2, 3], [4, 5, 6]]);\n\
                 let rows = a.iter_axis(0);\n\
                 println(rows.len());\n\
             }\n",
    );
    assert!(
        ir.contains("t.ia.bh"),
        "iter_axis must emit the per-bucket gather loop:\n{}",
        ir
    );
    assert!(
        ir.contains("cleanup.tdrop.cond"),
        "Vec[Tensor] binding must emit the tensor-element drop loop:\n{}",
        ir
    );
}

#[test]
fn test_e2e_tensor_iter_axis() {
    // rows / cols (rank ≥ 2 → Vec[Tensor]), rank-1 → Vec[T], and
    // indexing a bound Vec[Tensor]. Output verified vs `karac run`.
    let out = run_program(
        "fn main() {\n\
                 let a = Tensor.from([[1, 2, 3], [4, 5, 6]]);\n\
                 let rows = a.iter_axis(0);\n\
                 println(rows.len());\n\
                 for r in rows { println(r[0]); println(r[2]); }\n\
                 let cols = a.iter_axis(1);\n\
                 println(cols.len());\n\
                 for c in cols { println(c[1]); }\n\
                 let v = Tensor.from([10, 20, 30]);\n\
                 let scal = v.iter_axis(0);\n\
                 println(scal.len());\n\
                 for x in scal { println(x); }\n\
                 let subs = a.iter_axis(0);\n\
                 println(subs[1][2]);\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(
            out, "2\n1\n3\n4\n6\n3\n4\n5\n6\n3\n10\n20\n30\n6\n",
            "iter_axis AOT output must match the interpreter twin",
        );
    }
}

#[test]
fn test_e2e_tensor_iter_axis_runtime_axis() {
    // A runtime-valued axis: dims/axis read from the header, so the
    // bucketing is computed at runtime (the item shape degrades to
    // all-`?` in the type but the data is correct).
    let out = run_program(
        "fn main() {\n\
                 let a = Tensor.from([[1, 2, 3], [4, 5, 6]]);\n\
                 let ax = 1;\n\
                 let parts = a.iter_axis(ax);\n\
                 println(parts.len());\n\
                 for p in parts { println(p[0]); println(p[1]); }\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(
            out, "3\n1\n4\n2\n5\n3\n6\n",
            "runtime-axis iter_axis must bucket correctly"
        );
    }
}

#[test]
fn test_e2e_tensor_iter_axis_oob_panics() {
    let captured = run_program_capturing(
        "fn main() {\n\
                 let a = Tensor.from([[1, 2], [3, 4]]);\n\
                 let ax = 5;\n\
                 let parts = a.iter_axis(ax);\n\
                 println(parts.len());\n\
             }\n",
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("iter_axis axis out of bounds"),
            "expected the iter_axis bounds trap, got stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
    }
}

#[test]
fn test_e2e_tensor_elementwise_arithmetic() {
    // + - * / between tensors, a chained `a * b - a`, scalar broadcast
    // both sides (incl. int-literal promotion), and unary neg. Operand
    // reuse afterward confirms borrow-not-move. Output verified vs
    // `karac run`.
    let out = run_program(
        "fn main() {\n\
                 let a: Tensor[f64, [2, 2]] = Tensor.from([[1.0, 2.0], [3.0, 4.0]]);\n\
                 let b: Tensor[f64, [2, 2]] = Tensor.from([[10.0, 20.0], [30.0, 40.0]]);\n\
                 let c = a + b;\n\
                 println(c[0, 0]); println(c[1, 1]);\n\
                 let d = a * b - a;\n\
                 println(d[0, 1]);\n\
                 let s = a + 100.0;\n\
                 println(s[0, 0]);\n\
                 let p = a + 2;\n\
                 println(p[0, 0]);\n\
                 let n = -a;\n\
                 println(n[1, 0]);\n\
                 let sl = 100.0 - a;\n\
                 println(sl[0, 0]);\n\
                 println(a[0, 0]);\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(
            out, "11\n44\n38\n101\n3\n-3\n99\n1\n",
            "tensor element-wise arithmetic AOT output must match the interpreter twin",
        );
    }
}

#[test]
fn test_e2e_tensor_neg_signed_zero() {
    // B-2026-07-01-1: `-t` on a float tensor must be a true IEEE `fneg`
    // — `-0.0` for element `0.0` — matching the interpreter's `-f`. The
    // old `0.0 - x` lowering lost the signed zero (printed `0`).
    let out = run_program(
        "fn main() {\n\
                 let t: Tensor[f64, [2]] = Tensor.from([0.0, 1.5]);\n\
                 let n = -t;\n\
                 println(n[0]);\n\
                 println(n[1]);\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(
            out, "-0\n-1.5\n",
            "float tensor neg must preserve the signed zero (fneg, not 0 - x)"
        );
    }
}

#[test]
fn test_e2e_tensor_f32_literal_index_store() {
    // B-2026-07-22-7: an untyped float literal (defaults to f64)
    // index-assigned to an f32 tensor element must `fptrunc` to f32.
    // Without the coercion the store wrote 8 bytes into the 4-byte
    // slot, and the later f32 read got the low half — zero for many
    // round values (`double 5.0` low word = 0) — so the element
    // silently read 0. Covers constant and variable indices, a
    // matching-width `f32` suffix (control), and stores read back +
    // reduced to their exact values (no transcendental / no divergence).
    let out = run_program(
        "fn main() {\n\
                 let mut a: Tensor[f32, [4]] = Tensor.zeros(vec![4]);\n\
                 a[0] = 5.0; a[1] = 6.5;\n\
                 let k: i64 = 2;\n\
                 a[k] = 1.25;\n\
                 a[3] = 3.0f32;\n\
                 println(a[0]); println(a[1]); println(a[2]); println(a[3]);\n\
                 println(a.sum());\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(
            out, "5\n6.5\n1.25\n3\n15.75\n",
            "f32 tensor literal index-store must fptrunc and round-trip exactly",
        );
    }
}

#[test]
fn test_e2e_tensor_int_arithmetic_chained() {
    // Int tensors, chained `a + b + c` and `(a + b) * c` (the fresh-temp
    // intermediate free path), integer division. Output vs `karac run`.
    let out = run_program(
        "fn main() {\n\
                 let a: Tensor[i64, [3]] = Tensor.from([1, 2, 3]);\n\
                 let b: Tensor[i64, [3]] = Tensor.from([10, 20, 30]);\n\
                 let c: Tensor[i64, [3]] = Tensor.from([100, 200, 300]);\n\
                 let r = a + b + c;\n\
                 println(r[0]); println(r[2]);\n\
                 let r2 = (a + b) * c;\n\
                 println(r2[1]);\n\
                 let q = c / b;\n\
                 println(q[1]);\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(
            out, "111\n333\n4400\n10\n",
            "tensor int arithmetic / chained AOT output must match the interpreter twin",
        );
    }
}

#[test]
fn test_e2e_tensor_arithmetic_runtime_shape_mismatch_traps() {
    // Two `?`-dim tensors that differ at runtime — the static check can't
    // prove inequality, so the codegen runtime shape guard traps.
    let captured = run_program_capturing(
        "fn mk(n: i64) -> Tensor[f64, [?]] {\n\
                 let t: Tensor[f64, [?]] = Tensor.zeros([n]);\n\
                 t\n\
             }\n\
             fn main() {\n\
                 let a: Tensor[f64, [?]] = mk(3);\n\
                 let b: Tensor[f64, [?]] = mk(4);\n\
                 let c = a + b;\n\
                 println(c[0]);\n\
             }\n",
    );
    if let Some(c) = captured {
        assert!(
            c.stderr
                .contains("tensor shape mismatch in element-wise operator"),
            "expected the element-wise shape-mismatch trap, got stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
    }
}

#[test]
fn test_e2e_tensor_arithmetic_shape_guard_static_elision() {
    // Two fully-static identical shapes: the typechecker proved equality,
    // so codegen emits NO runtime shape guard. A `?`-dim operand keeps it.
    let concrete = ir_for(
        "fn main() {\n\
                 let a: Tensor[i64, [4]] = Tensor.from([1, 2, 3, 4]);\n\
                 let b: Tensor[i64, [4]] = Tensor.from([5, 6, 7, 8]);\n\
                 let c = a + b;\n\
                 println(c[0]);\n\
             }\n",
    );
    assert!(
        !concrete.contains("t.bin.rankeq"),
        "static-equal shapes should elide the runtime shape guard:\n{concrete}",
    );
    let dynamic = ir_for(
        "fn mk(n: i64) -> Tensor[i64, [?]] {\n\
                 let t: Tensor[i64, [?]] = Tensor.zeros([n]);\n\
                 t\n\
             }\n\
             fn main() {\n\
                 let a: Tensor[i64, [?]] = mk(4);\n\
                 let b: Tensor[i64, [?]] = mk(4);\n\
                 let c = a + b;\n\
                 println(c[0]);\n\
             }\n",
    );
    assert!(
        dynamic.contains("t.bin.rankeq"),
        "a `?`-dim operand must keep the runtime shape guard:\n{dynamic}",
    );
}

#[test]
fn test_e2e_tensor_full_reduce() {
    // sum / prod / min / max / mean → scalar. Means are exact decimals so
    // the AOT float formatting matches the interpreter byte-for-byte.
    // Receiver reuse after the reduce confirms it's read, not consumed.
    let out = run_program(
        "fn main() {\n\
                 let a: Tensor[i64, [2, 3]] = Tensor.from([[1, 2, 3], [4, 5, 6]]);\n\
                 println(a.sum());\n\
                 println(a.prod());\n\
                 println(a.min());\n\
                 println(a.max());\n\
                 println(a.mean());\n\
                 let v: Tensor[f64, [4]] = Tensor.from([2.0, 4.0, 6.0, 8.0]);\n\
                 println(v.sum());\n\
                 println(v.mean());\n\
                 println(a[0, 0]);\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(
            out, "21\n720\n1\n6\n3.5\n20\n5\n1\n",
            "tensor full-reduce AOT output must match the interpreter twin",
        );
    }
}

#[test]
fn test_ir_float_tensor_reduce_carries_reassoc() {
    // The float reduction fold must be tagged `reassoc` so LLVM's loop
    // vectorizer can turn the scalar `fadd` chain into packed adds + a
    // horizontal sum (`tag_reduce_reassoc`, kernel.rs). Without the flag
    // LLVM refuses to reassociate float adds and the reduction stays
    // scalar. A `ref` param source keeps the reduction from const-folding.
    let ir = ir_for(
        "fn s(t: ref Tensor[f32, [64]]) -> f32 { t.sum() }\n\
             fn main() { let t: Tensor[f32, [64]] = Tensor.ones([64]); println(s(t)); }\n",
    );
    assert!(
        ir.contains("fadd reassoc"),
        "float Tensor.sum fold must emit `fadd reassoc` to unlock vectorization; IR:\n{ir}"
    );

    // The integer twin must NOT carry a float reassoc flag: an i64 fold
    // lowers to a defined-overflow-trapping integer `add`, which is
    // order-independent already and byte-identical across backends.
    let int_ir = ir_for(
        "fn s(t: ref Tensor[i64, [64]]) -> i64 { t.sum() }\n\
             fn main() { let t: Tensor[i64, [64]] = Tensor.ones([64]); println(s(t)); }\n",
    );
    assert!(
        !int_ir.contains("fadd reassoc"),
        "integer Tensor.sum must not carry a float reassoc flag; IR:\n{int_ir}"
    );
}

#[test]
fn test_e2e_float_tensor_reduce_exact_value_bit_identical() {
    // A large float reduction that DOES vectorize (reassociated packed
    // adds), over exactly-representable inputs. Reassociation reorders the
    // adds but 1024 copies of 1.0 sum to 1024.0 in any order, so the AOT
    // result stays bit-identical to the ordered interpreter twin — the
    // "exactly-representable values stay bit-identical" guarantee in
    // `tag_reduce_reassoc`'s doc.
    let out = run_program(
        "fn s(t: ref Tensor[f32, [1024]]) -> f32 { t.sum() }\n\
             fn main() {\n\
                 let t: Tensor[f32, [1024]] = Tensor.ones([1024]);\n\
                 println(s(t));\n\
                 let g: Tensor[f64, [1024]] = Tensor.ones([1024]);\n\
                 println(g.mean());\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(
            out, "1024\n1\n",
            "vectorized float reduction over exact values must stay bit-identical \
                 to the interpreter twin",
        );
    }
}

#[test]
fn test_ir_float_tensor_minmax_carries_nnan_nsz() {
    // The float min/max compare+select must be tagged `nnan nsz` so LLVM's
    // loop vectorizer recognizes the FP min/max reduction idiom
    // (`tag_minmax_vectorizable`, kernel.rs) — the min/max leg of the
    // data-spine reduction win (sum/mean landed via `reassoc`). Without
    // the flags the reduction stays scalar `vminss`/`vmaxss` (measured 0
    // packed ops before tagging).
    let ir = ir_for(
        "fn m(t: ref Tensor[f32, [64]]) -> f32 { t.min() }\n\
             fn x(t: ref Tensor[f32, [64]]) -> f32 { t.max() }\n\
             fn main() { let t: Tensor[f32, [64]] = Tensor.ones([64]); \
             println(m(t)); println(x(t)); }\n",
    );
    assert!(
        ir.contains("fcmp nnan nsz olt") && ir.contains("fcmp nnan nsz ogt"),
        "float Tensor.min/max compares must carry `nnan nsz` to unlock the FP \
             min/max reduction idiom; IR:\n{ir}"
    );
    assert!(
        ir.contains("select nnan nsz"),
        "the min/max select must carry `nnan nsz` too (LLVM keys the recurrence \
             on the select's flags); IR:\n{ir}"
    );

    // The integer twin must stay flag-free: integer compares/selects are
    // order-independent already and byte-identical across backends.
    let int_ir = ir_for(
        "fn m(t: ref Tensor[i64, [64]]) -> i64 { t.min() }\n\
             fn main() { let t: Tensor[i64, [64]] = Tensor.ones([64]); println(m(t)); }\n",
    );
    assert!(
        !int_ir.contains("nnan"),
        "integer Tensor.min must not carry float fast-math flags; IR:\n{int_ir}"
    );
}

#[test]
fn test_e2e_float_tensor_minmax_bit_identical() {
    // Unlike the reassoc sum (bit-identical only for exactly-representable
    // inputs), min/max is EXACT — no rounding reorder — so any NaN-free
    // data must produce bit-identical results across backends, vectorized
    // or not. Varied f32/f64/i64 data, both extremes exercised.
    let out = run_program(
        "fn main() {\n\
                 let mut t: Tensor[f32, [1000]] = Tensor.zeros(vec![1000]);\n\
                 for i in 0..1000 { t[i] = ((i * 37) % 101) as f32 - 50.5; }\n\
                 println(t.min()); println(t.max());\n\
                 let mut d: Tensor[f64, [777]] = Tensor.zeros(vec![777]);\n\
                 for i in 0..777 { d[i] = ((i * 53) % 97) as f64 * 0.25 - 12.0; }\n\
                 println(d.min()); println(d.max());\n\
                 let mut it: Tensor[i64, [500]] = Tensor.zeros(vec![500]);\n\
                 for i in 0..500 { it[i] = ((i * 41) % 89) - 44; }\n\
                 println(it.min()); println(it.max());\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(
            out, "-50.5\n49.5\n-12\n12\n-44\n44\n",
            "vectorized float min/max must stay bit-identical to the interpreter \
                 twin on NaN-free data",
        );
    }
}

#[test]
fn test_ir_tensor_transcendental_map_vectorizes() {
    // The data-spine transcendental-map leg: a standalone (bound)
    // `t.map(|x| x.exp())` lifts to a strip-mined `<8 x float>` loop
    // routing exp through the SIMD polynomial (try_emit_vectorized_map,
    // tensor.rs), instead of the scalar `expf` call that blocks
    // vectorization. The `<8 x float>` arithmetic is the polynomial —
    // the scalar map path never emits vector-typed ops.
    let ir = ir_for(
        "fn act(t: ref Tensor[f32, [64]]) -> Tensor[f32, [64]] { t.map(|x| x.exp()) }\n\
             fn main() {\n\
                 let t: Tensor[f32, [64]] = Tensor.ones([64]);\n\
                 let r = act(t);\n\
                 println(r.sum());\n\
             }\n",
    );
    assert!(
        ir.contains("<8 x float>"),
        "standalone f32 transcendental map must lift to a <8 x float> vector loop"
    );

    // A ln map lifts the same way; an f64 map lifts to <4 x double>.
    let ir64 = ir_for(
        "fn act(t: ref Tensor[f64, [64]]) -> Tensor[f64, [64]] { t.map(|x| x.ln()) }\n\
             fn main() {\n\
                 let t: Tensor[f64, [64]] = Tensor.ones([64]);\n\
                 let r = act(t);\n\
                 println(r.sum());\n\
             }\n",
    );
    assert!(
        ir64.contains("<4 x double>"),
        "standalone f64 transcendental map must lift to a <4 x double> vector loop"
    );

    // A pure-arithmetic map (no transcendental) is NOT intercepted —
    // it stays on the bit-exact scalar path (no forced vectorization,
    // no divergence). No `<8 x float>` from a vectorized map body.
    let ir_arith = ir_for(
        "fn act(t: ref Tensor[f32, [64]]) -> Tensor[f32, [64]] { t.map(|x| x * 2.0) }\n\
             fn main() {\n\
                 let t: Tensor[f32, [64]] = Tensor.ones([64]);\n\
                 let r = act(t);\n\
                 println(r.sum());\n\
             }\n",
    );
    assert!(
        !ir_arith.contains("<8 x float>"),
        "a pure-arithmetic map must NOT be force-vectorized (stays bit-exact scalar)"
    );
}

#[test]
fn test_e2e_tensor_transcendental_map_accuracy() {
    // The vectorized transcendental map must stay within the documented
    // f32-polynomial tolerance of the true math value — self-checking so
    // it holds on BOTH backends (interp keeps f64 libm; AOT uses the
    // SIMD polynomial; both are within 1e-3 of the sigmoid reference).
    // A bound map over a `Tensor.from` literal (avoids the f32-literal
    // index-assign miscompile B-2026-07-22-3).
    let out = run_program(
            "fn main() {\n\
                 let t: Tensor[f32, [8]] = Tensor.from([-2.0, -1.0, -0.5, 0.0, 0.5, 1.0, 2.0, 3.0]);\n\
                 let m = t.map(|x| 1.0f32 / (1.0f32 + (0.0f32 - x).exp()));\n\
                 let xs: Tensor[f32, [8]] = Tensor.from([-2.0, -1.0, -0.5, 0.0, 0.5, 1.0, 2.0, 3.0]);\n\
                 let mut ok: bool = true;\n\
                 for i in 0..8 {\n\
                     let want: f32 = 1.0f32 / (1.0f32 + (0.0f32 - xs[i]).exp());\n\
                     let d: f32 = m[i] - want;\n\
                     let ad: f32 = if d < 0.0f32 { 0.0f32 - d } else { d };\n\
                     if ad > 0.001f32 { ok = false; }\n\
                 }\n\
                 println(ok);\n\
             }\n",
        );
    if let Some(out) = out {
        assert_eq!(
            out, "true\n",
            "vectorized sigmoid map must stay within 1e-3 of the reference on both backends",
        );
    }
}

#[test]
fn test_e2e_tensor_axis_reduce() {
    // sum_axis / mean_axis → rank-1-lower tensor; rank-1 → scalar.
    let out = run_program(
        "fn main() {\n\
                 let a: Tensor[i64, [2, 3]] = Tensor.from([[1, 2, 3], [4, 5, 6]]);\n\
                 let s0 = a.sum_axis(0);\n\
                 println(s0[0]); println(s0[1]); println(s0[2]);\n\
                 let s1 = a.sum_axis(1);\n\
                 println(s1[0]); println(s1[1]);\n\
                 let m0 = a.mean_axis(0);\n\
                 println(m0[0]); println(m0[2]);\n\
                 let v: Tensor[i64, [4]] = Tensor.from([1, 2, 3, 4]);\n\
                 println(v.sum_axis(0));\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(
            out, "5\n7\n9\n6\n15\n2.5\n4.5\n10\n",
            "tensor axis-reduce AOT output must match the interpreter twin",
        );
    }
}

#[test]
fn test_e2e_tensor_reduce_runtime_axis_and_generic() {
    // A runtime axis arg (no static slot to drop) + a `Numeric`-bounded
    // generic full reduce with a concrete shape (the mono-body path).
    let out = run_program(
        "fn trace[T: Numeric](t: Tensor[T, [2, 2]]) -> T { t.sum() }\n\
             fn main() {\n\
                 let a: Tensor[i64, [2, 3]] = Tensor.from([[1, 2, 3], [4, 5, 6]]);\n\
                 let ax = 1;\n\
                 let r = a.sum_axis(ax);\n\
                 println(r[0]); println(r[1]);\n\
                 let g: Tensor[i64, [2, 2]] = Tensor.from([[1, 2], [3, 4]]);\n\
                 println(trace(g));\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(
            out, "6\n15\n10\n",
            "runtime-axis reduce + generic full-reduce must match `karac run`",
        );
    }
}

#[test]
fn test_e2e_tensor_reduce_empty_traps() {
    let captured = run_program_capturing(
        "fn main() {\n\
                 let e: Tensor[f64, [0]] = Tensor.zeros([0]);\n\
                 println(e.max());\n\
             }\n",
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("cannot reduce an empty tensor"),
            "expected the empty-reduce trap, got stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
    }
}

#[test]
fn test_e2e_tensor_broadcast() {
    // Row / column / rank-mismatch broadcasting, two-singleton expand, a
    // fresh-temp argument (`a + b`), and float division. Operand reuse
    // afterward confirms borrow-not-move. Output verified vs `karac run`.
    let out = run_program(
        "fn main() {\n\
                 let m: Tensor[i64, [2, 3]] = Tensor.from([[1, 2, 3], [4, 5, 6]]);\n\
                 let row: Tensor[i64, [1, 3]] = Tensor.from([[10, 20, 30]]);\n\
                 let r = m.broadcast_add(row);\n\
                 println(r[0, 0]); println(r[1, 2]);\n\
                 let col: Tensor[i64, [2, 1]] = Tensor.from([[100], [200]]);\n\
                 let c = m.broadcast_mul(col);\n\
                 println(c[0, 1]); println(c[1, 0]);\n\
                 let v: Tensor[i64, [3]] = Tensor.from([1, 2, 3]);\n\
                 let d = m.broadcast_sub(v);\n\
                 println(d[0, 0]); println(d[1, 1]);\n\
                 // two singletons broadcast UP to [2, 3].\n\
                 let g = row.broadcast_add(col);\n\
                 println(g[1, 0]);\n\
                 // fresh-temp argument (the `b + b` intermediate is freed).\n\
                 let b: Tensor[i64, [1, 3]] = Tensor.from([[1, 1, 1]]);\n\
                 let h = m.broadcast_add(b + b);\n\
                 println(h[0, 0]);\n\
                 // float division.\n\
                 let fm: Tensor[f64, [2, 2]] = Tensor.from([[2.0, 4.0], [6.0, 8.0]]);\n\
                 let fc: Tensor[f64, [2, 1]] = Tensor.from([[2.0], [4.0]]);\n\
                 let fq = fm.broadcast_div(fc);\n\
                 println(fq[1, 0]);\n\
                 println(m[1, 2]);\n\
             }\n",
    );
    if let Some(out) = out {
        // r[0,0]=1+10=11, r[1,2]=6+30=36; c[0,1]=2*100=200, c[1,0]=4*200=800;
        // d[0,0]=1-1=0, d[1,1]=5-2=3; g[1,0]=row[0,0]+col[1,0]=10+200=210;
        // h[0,0]=1+(1+1)=3; fq[1,0]=6/4=1.5; m reused=6.
        assert_eq!(
            out, "11\n36\n200\n800\n0\n3\n210\n3\n1.5\n6\n",
            "tensor broadcast AOT output must match the interpreter twin",
        );
    }
}

#[test]
fn test_e2e_tensor_broadcast_runtime_dims_and_trap() {
    // `?`-dim operands: a compatible broadcast (row [1,?] over [?,?]) and
    // an incompatible one that the codegen runtime guard traps. The static
    // checker can't prove either, so both go through the header-driven
    // runtime path.
    let ok = run_program(
        "fn mk_mat(r: i64, c: i64) -> Tensor[i64, [?, ?]] {\n\
                 let t: Tensor[i64, [?, ?]] = Tensor.full([r, c], 2);\n\
                 t\n\
             }\n\
             fn mk_row(n: i64) -> Tensor[i64, [1, ?]] {\n\
                 let t: Tensor[i64, [1, ?]] = Tensor.full([1, n], 5);\n\
                 t\n\
             }\n\
             fn main() {\n\
                 let m: Tensor[i64, [?, ?]] = mk_mat(2, 3);\n\
                 let row: Tensor[i64, [1, ?]] = mk_row(3);\n\
                 let r = m.broadcast_add(row);\n\
                 println(r[0, 0]); println(r[1, 2]);\n\
             }\n",
    );
    if let Some(out) = ok {
        assert_eq!(
            out, "7\n7\n",
            "runtime-dim broadcast must match `karac run`"
        );
    }
    let captured = run_program_capturing(
        "fn mk(r: i64, c: i64) -> Tensor[i64, [?, ?]] {\n\
                 let t: Tensor[i64, [?, ?]] = Tensor.full([r, c], 1);\n\
                 t\n\
             }\n\
             fn main() {\n\
                 let a: Tensor[i64, [?, ?]] = mk(2, 3);\n\
                 let b: Tensor[i64, [?, ?]] = mk(2, 4);\n\
                 let r = a.broadcast_add(b);\n\
                 println(r[0, 0]);\n\
             }\n",
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("not broadcast-compatible"),
            "expected the broadcast-incompatible trap, got stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
    }
}

#[test]
fn test_e2e_autograd_scalar_arithmetic() {
    // `std.autograd` (phase-11) reverse-mode scalar AD — arithmetic ops +
    // the chain rule + gradient accumulation across a shared input.
    //   f(x) = x*x + 3*x  at x=2 → 10; df/dx = 2x+3 = 7 (x used twice, so
    //     its gradient accumulates from both terms).
    //   g(a,b) = (a-b)/b   at a=6,b=2 → 2; dg/da = 1/b = 0.5;
    //     dg/db = -a/b² = -1.5 (b used in both the subtraction and divisor).
    if let Some(out) = run_program(
        r#"
import std.autograd.{Tape, Var};
fn main() {
    let t = Tape.new();
    let x = Var.leaf(t, 2.0);
    let three = Var.leaf(t, 3.0);
    let f = x.mul(x).add(three.mul(x));
    println(f.value());
    f.backward();
    println(x.grad());

    let t2 = Tape.new();
    let a = Var.leaf(t2, 6.0);
    let b = Var.leaf(t2, 2.0);
    let g = a.sub(b).div(b);
    println(g.value());
    g.backward();
    println(a.grad());
    println(b.grad());
}
"#,
    ) {
        assert_eq!(out, "10\n7\n2\n0.5\n-1.5\n");
    }
}

#[test]
fn test_e2e_autograd_activations() {
    // `std.autograd` activations with hand-coded backwards. Exact fixed
    // points: sigmoid(0)=0.5, backward s(1-s)=0.25; relu blocks the
    // gradient on a negative input (0) and passes it (1) on a positive one;
    // tanh(0)=0, backward 1-t²=1.
    if let Some(out) = run_program(
        r#"
import std.autograd.{Tape, Var};
fn main() {
    let t = Tape.new();
    let z = Var.leaf(t, 0.0);
    let s = z.sigmoid();
    println(s.value()); s.backward(); println(z.grad());

    let t2 = Tape.new();
    let n = Var.leaf(t2, -2.0);
    let rn = n.relu();
    println(rn.value()); rn.backward(); println(n.grad());

    let t3 = Tape.new();
    let p = Var.leaf(t3, 3.0);
    let rp = p.relu();
    println(rp.value()); rp.backward(); println(p.grad());

    let t4 = Tape.new();
    let w = Var.leaf(t4, 0.0);
    let y = w.tanh();
    println(y.value()); y.backward(); println(w.grad());
}
"#,
    ) {
        assert_eq!(out, "0.5\n0.25\n0\n0\n3\n1\n0\n1\n");
    }
}

#[test]
fn test_e2e_autograd_tensor_valued() {
    // `std.autograd` tensor-valued surface (phase-11) — reverse-mode AD over
    // element-wise `Tensor[f32, [?]]` nodes, with gradient accumulation
    // across a fanned-out input.
    //   z = x * y + x   (x used in both the product and the sum)
    //   dz/dx = y + 1 = [5, 6];   dz/dy = x = [2, 3]
    // at x=[2,3], y=[4,5]. Exercises the Vec[Tensor] element-ownership
    // paths fixed in B-2026-07-17-7 (field drop, index-store overwrite in
    // `backward`'s accumulation, moved named-tensor store in `record`).
    if let Some(out) = run_program(
        r#"
import std.autograd.{TensorTape, TensorVar};
fn main() {
    let t = TensorTape.new();
    let x0: Tensor[f32, [?]] = Tensor.from([2.0, 3.0]);
    let y0: Tensor[f32, [?]] = Tensor.from([4.0, 5.0]);
    let x = TensorVar.leaf(t, x0);
    let y = TensorVar.leaf(t, y0);
    let z = x.mul(y).add(x);
    z.backward();
    println(x.grad_at(0));
    println(x.grad_at(1));
    println(y.grad_at(0));
    println(y.grad_at(1));
}
"#,
    ) {
        assert_eq!(out, "5\n6\n2\n3\n");
    }
}

#[test]
fn test_e2e_autograd_tensor_activations() {
    // `std.autograd` tensor-valued activations — element-wise relu/sigmoid/
    // tanh with hand-coded backwards. relu gates the gradient on the input
    // sign (grad [0,1] for x=[-1,2]); sigmoid'(0)=0.25; tanh'(0)=1.
    if let Some(out) = run_program(
        r#"
import std.autograd.{TensorTape, TensorVar};
fn main() {
    let t = TensorTape.new();
    let x0: Tensor[f32, [?]] = Tensor.from([-1.0, 2.0]);
    let x = TensorVar.leaf(t, x0);
    let r = x.relu();
    r.backward();
    println(x.grad_at(0));
    println(x.grad_at(1));

    let t2 = TensorTape.new();
    let z0: Tensor[f32, [?]] = Tensor.from([0.0]);
    let z = TensorVar.leaf(t2, z0);
    let s = z.sigmoid();
    s.backward();
    println(z.grad_at(0));

    let t3 = TensorTape.new();
    let w0: Tensor[f32, [?]] = Tensor.from([0.0]);
    let w = TensorVar.leaf(t3, w0);
    let y = w.tanh();
    y.backward();
    println(w.grad_at(0));
}
"#,
    ) {
        assert_eq!(out, "0\n1\n0.25\n1\n");
    }
}

#[test]
fn test_e2e_autograd_activations_and_losses() {
    // Phase-11 autograd activations (`silu`/`softmax`/`gelu`) + losses
    // (`bce`/`cross_entropy`). Leaf inputs are INLINE `Tensor.from([…])` —
    // the fresh-temp `ref Tensor` arg path (B-2026-07-18-9) + the f32
    // element-width threading (B-2026-07-18-10); before those fixes this shape
    // segfaulted / misread the tape under `karac build`. Inputs are chosen so
    // every printed gradient is f32-exact:
    //   silu'(0) = σ(0) = 0.5.
    //   softmax([0,0]) = [0.5,0.5]; weighting by c=[1,0] gives
    //     grad_x = s⊙(c − ⟨c,s⟩) = [0.5·0.5, 0.5·(−0.5)] = [0.25, −0.25].
    //   gelu'(0) = 0.5·(1+tanh(0)) = 0.5.
    //   bce(p=0.8,t=1) grad = (p−t)/(p(1−p))/N = (−0.2)/(0.16)/2 = −0.625.
    //   cross_entropy(logits=[0,0], onehot=[1,0]) grad = softmax−onehot
    //     = [0.5−1, 0.5−0] = [−0.5, 0.5].
    // Byte-identical to `karac run` (all values f32-exact, no precision gap).
    if let Some(out) = run_program(
        r#"
import std.autograd.{TensorTape, TensorVar};
fn main() {
    let t1 = TensorTape.new();
    let a = TensorVar.leaf(t1, Tensor.from([0.0, 1.0]));
    let y = a.silu(); let s = y.sum(); s.backward();
    println(a.grad_at(0));

    let t2 = TensorTape.new();
    let x = TensorVar.leaf(t2, Tensor.from([0.0, 0.0]));
    let c = TensorVar.leaf(t2, Tensor.from([1.0, 0.0]));
    let sm = x.softmax(); let l = sm.mul(c).sum(); l.backward();
    println(x.grad_at(0)); println(x.grad_at(1));

    let t3 = TensorTape.new();
    let g = TensorVar.leaf(t3, Tensor.from([0.0]));
    let gy = g.gelu(); let gs = gy.sum(); gs.backward();
    println(g.grad_at(0));

    let t4 = TensorTape.new();
    let p = TensorVar.leaf(t4, Tensor.from([0.8, 0.3]));
    let tg = TensorVar.leaf(t4, Tensor.from([1.0, 0.0]));
    let lb = p.bce(tg); lb.backward();
    println(p.grad_at(0));

    let t5 = TensorTape.new();
    let xc = TensorVar.leaf(t5, Tensor.from([0.0, 0.0]));
    let oh = TensorVar.leaf(t5, Tensor.from([1.0, 0.0]));
    let lc = xc.cross_entropy(oh); lc.backward();
    println(xc.grad_at(0)); println(xc.grad_at(1));
}
"#,
    ) {
        assert_eq!(out, "0.5\n0.25\n-0.25\n0.5\n-0.625\n-0.5\n0.5\n");
    }
}

#[test]
fn test_e2e_freshtemp_tensor_ref_arg_assoc_call() {
    // B-2026-07-18-9 / B-2026-07-18-10: an inline `Tensor.from([…])` passed as
    // a `ref Tensor[f32,…]` arg to an ASSOCIATED fn. Before the fix the
    // assoc-call path passed the fresh temp by value (crash) and the f64
    // literals were laid out 8-byte-wide for the f32 param (misread). Now the
    // rvalue is materialized to a slot + pointer-passed, and the callee's
    // declared f32 element type is threaded to `Tensor.from`. Reads the
    // element back through the borrow.
    if let Some(out) = run_program(
        r#"
struct P {}
impl P { fn second(v: ref Tensor[f32, [?]]) -> f32 { v[1] } }
fn main() { println(P.second(Tensor.from([-1.0, 2.0, 3.0]))); }
"#,
    ) {
        assert_eq!(out, "2\n");
    }
}

#[test]
fn test_e2e_freshtemp_tensor_ref_arg_free_fn() {
    // B-2026-07-18-10 residual: the FREE-FN sibling of the assoc-call case
    // above. `f(Tensor.from([-1.0, 2.0]))` into a `ref Tensor[f32,…]` free-fn
    // param laid the unsuffixed f64 literals out 8-byte-wide (misread by the
    // f32 reader) — the free-fn rvalue-ref path now threads the callee's
    // declared element type and registers the materialized temp's FreeTensor.
    if let Some(out) = run_program(
        r#"
fn second(v: ref Tensor[f32, [?]]) -> f32 { v[1] }
fn main() { println(second(Tensor.from([-1.0, 2.0, 3.0]))); }
"#,
    ) {
        assert_eq!(out, "2\n");
    }
}

#[test]
fn test_e2e_autograd_tensor_scalar_loss() {
    // `std.autograd` tensor-valued `sum` reduction — the terminal that turns
    // a tensor computation into a scalar loss. L = sum(x²) at x=[1,2,3] → 14;
    // dL/dx = 2x = [2,4,6] (the sum node broadcasts its scalar upstream
    // gradient back to the input shape, then the mul VJP doubles it).
    if let Some(out) = run_program(
        r#"
import std.autograd.{TensorTape, TensorVar};
fn main() {
    let t = TensorTape.new();
    let x0: Tensor[f32, [?]] = Tensor.from([1.0, 2.0, 3.0]);
    let x = TensorVar.leaf(t, x0);
    let sq = x.mul(x);
    let loss = sq.sum();
    let lv = loss.value();
    println(f"{lv[0]}");
    loss.backward();
    println(x.grad_at(0));
    println(x.grad_at(1));
    println(x.grad_at(2));
}
"#,
    ) {
        assert_eq!(out, "14\n2\n4\n6\n");
    }
}

#[test]
fn test_e2e_autograd_tensor_mean_loss() {
    // `std.autograd` tensor-valued `mean` reduction — the averaged sibling of
    // `sum` (the MSE-style loss). L = mean(x²) at x=[2,4] (N=2) → 10;
    // dL/dx = 2x/N = x = [2,4] (the mean node broadcasts g/N, the mul VJP
    // doubles).
    if let Some(out) = run_program(
        r#"
import std.autograd.{TensorTape, TensorVar};
fn main() {
    let t = TensorTape.new();
    let x0: Tensor[f32, [?]] = Tensor.from([2.0, 4.0]);
    let x = TensorVar.leaf(t, x0);
    let sq = x.mul(x);
    let loss = sq.mean();
    let lv = loss.value();
    println(f"{lv[0]}");
    loss.backward();
    println(x.grad_at(0));
    println(x.grad_at(1));
}
"#,
    ) {
        assert_eq!(out, "10\n2\n4\n");
    }
}

#[test]
fn test_e2e_autograd_matmul() {
    // `std.autograd` rank-2 matrix surface (MatTape / MatVar) — matmul with
    // its transpose-product backward. Y = A·B with A=[[1,2],[3,4]], B=I;
    // seeding grad_Y=ones, grad_A = ones·Bᵀ = [[1,1],[1,1]] and
    // grad_B = Aᵀ·ones = [[4,4],[6,6]].
    if let Some(out) = run_program(
        r#"
import std.autograd.{MatTape, MatVar};
fn main() {
    let t = MatTape.new();
    let a0: Tensor[f32, [?, ?]] = Tensor.from([[1.0, 2.0], [3.0, 4.0]]);
    let b0: Tensor[f32, [?, ?]] = Tensor.from([[1.0, 0.0], [0.0, 1.0]]);
    let a = MatVar.leaf(t, a0);
    let b = MatVar.leaf(t, b0);
    let y = a.matmul(b);
    y.backward();
    println(a.grad_at(0, 0));
    println(a.grad_at(1, 1));
    println(b.grad_at(0, 0));
    println(b.grad_at(1, 0));
}
"#,
    ) {
        assert_eq!(out, "1\n1\n4\n6\n");
    }
}

#[test]
fn test_e2e_autograd_mse_loss() {
    // `std.autograd` mean-squared-error loss — a pure composition of sub →
    // mul → mean (no dedicated op-code; backward flows through them).
    // pred=[3,5], target=[1,1] → d=[2,4], mse = mean([4,16]) = 10;
    // dL/dpred = 2d/N = [2,4].
    if let Some(out) = run_program(
        r#"
import std.autograd.{TensorTape, TensorVar};
fn main() {
    let t = TensorTape.new();
    let p0: Tensor[f32, [?]] = Tensor.from([3.0, 5.0]);
    let g0: Tensor[f32, [?]] = Tensor.from([1.0, 1.0]);
    let pred = TensorVar.leaf(t, p0);
    let target = TensorVar.leaf(t, g0);
    let loss = pred.mse(target);
    let lv = loss.value();
    println(f"{lv[0]}");
    loss.backward();
    println(pred.grad_at(0));
    println(pred.grad_at(1));
}
"#,
    ) {
        assert_eq!(out, "10\n2\n4\n");
    }
}

#[test]
fn test_e2e_autograd_gradient_descent_training() {
    // End-to-end reverse-mode TRAINING loop (mirrors
    // examples/autograd_training.kara): minimize mean-squared error by
    // gradient descent, learning target=[3,5,7] from a zero start. Each step
    // builds a fresh tape, backprops the loss, and steps the weights down the
    // gradient (`w = w - lr·dL/dw`). Outputs rounded so both backends agree:
    // initial loss 28 → final loss 0, weights converge to [3,5,7].
    if let Some(out) = run_program(
        r#"
import std.autograd.{TensorTape, TensorVar};
fn loss_of(w: ref Tensor[f32, [?]], target: ref Tensor[f32, [?]]) -> f32 {
    let tape = TensorTape.new();
    let wv = TensorVar.leaf(tape, w);
    let tv = TensorVar.leaf(tape, target);
    let loss = wv.mse(tv);
    let lv = loss.value();
    lv[0]
}
fn main() {
    let target: Tensor[f32, [?]] = Tensor.from([3.0, 5.0, 7.0]);
    let mut w: Tensor[f32, [?]] = Tensor.from([0.0, 0.0, 0.0]);
    // B-2026-08-14-14: annotated at the tensors' element type. Unannotated the
    // literal is f64, and `grad * lr` below narrowed it silently into an f32
    // element-wise op. 0.75 is exact in both widths, so no printed value moves.
    let lr: f32 = 0.75;
    let l0 = loss_of(w, target);
    println(f"{l0.round()}");
    let mut step = 0;
    while step < 40 {
        let tape = TensorTape.new();
        let wv = TensorVar.leaf(tape, w);
        let tv = TensorVar.leaf(tape, target);
        let loss = wv.mse(tv);
        loss.backward();
        let grad: Tensor[f32, [?]] = wv.grad();
        let step_dir: Tensor[f32, [?]] = grad * lr;
        w = w - step_dir;
        step = step + 1;
    }
    let lf = loss_of(w, target);
    println(f"{lf.round()}");
    let w0 = w[0];
    let w1 = w[1];
    let w2 = w[2];
    println(f"{w0.round()}");
    println(f"{w1.round()}");
    println(f"{w2.round()}");
}
"#,
    ) {
        assert_eq!(out, "28\n0\n3\n5\n7\n");
    }
}

#[test]
fn test_e2e_autograd_grad_value_and_grad() {
    // Phase-11 std.autograd higher-order API (`Tape.grad` / `Tape.value_and_grad`,
    // the JAX-style entry point): pass a closure `Fn(Var) -> Var`, get back the
    // derivative (and value) with no tape bookkeeping. Exact oracles:
    //   value_and_grad(x²+3x, 2) = (10, 7)      [f'=2x+3]
    //   grad(x²+x, 2) = 5                        [f'=2x+1]
    //   grad(x³, 2) = 12                         [f'=3x²]
    //   grad(sigmoid, 0) = 0.25;  grad(tanh, 0) = 1
    //   grad(relu, 3) = 1;  grad(relu, -1) = 0
    if let Some(out) = run_program(
        r#"
import std.autograd.{Tape, Var};
fn main() {
    let vg = Tape.value_and_grad(|x| x.mul(x).add(x.add(x).add(x)), 2.0);
    println(vg.0);
    println(vg.1);
    println(Tape.grad(|x| x.mul(x).add(x), 2.0));
    println(Tape.grad(|x| x.mul(x).mul(x), 2.0));
    println(Tape.grad(|x| x.sigmoid(), 0.0));
    println(Tape.grad(|x| x.tanh(), 0.0));
    println(Tape.grad(|x| x.relu(), 3.0));
    println(Tape.grad(|x| x.relu(), 0.0 - 1.0));
}
"#,
    ) {
        assert_eq!(out, "10\n7\n5\n12\n0.25\n1\n1\n0\n");
    }
}

/// A `StringSlice` bound by a MATCH PATTERN lost its method dispatch under
/// `karac build` — every method except `to_string` fell through with
/// "no handler for method '<m>' on variable '<v>'", while `--interp` ran
/// the same program. The identical view reached through a `let`, a tuple
/// element or a struct field compiled, so the defect was specific to the
/// pattern-binding path.
///
/// Cause: in `pattern_binding.rs` one `if` did two jobs. It registered
/// `vec_elem_types` (the side table METHOD DISPATCH reads to pick the
/// String-shaped arm) AND set `bound_vec_elem` (which schedules the
/// end-of-arm `track_vec_var` buffer free) — gated on `String`/`CString`.
/// `StringSlice` was correctly excluded because a borrow has nothing to
/// free, but excluding it from the free also excluded it from dispatch.
/// The two are separable; the let-binding path registers both tables for a
/// `StringSlice` precisely because it shares String's `{ptr,len,cap}`
/// layout and its read-methods dispatch identically.
///
/// This blocked the one zero-copy tokenizing spelling Kāra can express
/// today — a cursor or iterator yielding `Option[StringSlice]` — on the
/// backend where avoiding the per-token allocation is the whole point
/// (B-2026-08-26-13). The lazy `SplitIter` here is that spelling, and it
/// measured 9.0x faster and 5.2x smaller than `s.split(",")` on an
/// equivalent workload once it compiled.
#[test]
fn test_e2e_match_bound_string_slice_keeps_method_dispatch() {
    assert_eq!(
        run_program(
            r#"
fn head(s: ref String) -> Option[StringSlice] { return Some(s.slice(0, 3)); }

struct SplitIter { rest: StringSlice, done: bool }
impl SplitIter {
    fn next(mut ref self) -> Option[StringSlice] {
        if self.done { return None; }
        match self.rest.find(",") {
            Some(k) => {
                let head = self.rest.slice(0, k);
                self.rest = self.rest.slice(k + 1, self.rest.len());
                return Some(head);
            }
            None => { self.done = true; return Some(self.rest); }
        }
    }
}

fn main() {
    let s = "hello";
    match head(s) {
        Some(v) => println(f"len={v.len()}"),
        None => println("none")
    }
    match head(s) {
        Some(v) => println(f"sub={v.slice(0, 2).to_string()}"),
        None => println("none")
    }
    let text = "aa,bb,cc";
    let mut it = SplitIter { rest: text.slice(0, text.len()), done: false };
    let mut n = 0;
    let mut c = 0;
    while true {
        match it.next() { None => break, Some(f) => { n = n + f.len(); c = c + 1; } }
    }
    println(f"{c} fields, {n} bytes")
}
"#
        ),
        Some("len=3\nsub=he\n3 fields, 6 bytes\n".to_string())
    );
}

/// The control for the test above: the depth dispatch fires ONLY for a
/// hand-written impl. A `#[derive(Display)]` type keeps the derived shape
/// at every depth, so the fix cannot have shifted an existing rendering.
#[test]
fn test_e2e_derived_display_unaffected_by_depth_dispatch() {
    assert_eq!(
        run_program(
            r#"
#[derive(Display)]
struct Plain { n: i64 }
fn main() {
    let p = Plain { n: 5 };
    println(f"top={p}")
    let v = [Plain { n: 5 }];
    println(f"vec={v}")
}
"#
        ),
        Some("top=Plain { n: 5 }\nvec=[Plain { n: 5 }]\n".to_string())
    );
}

/// B-2026-08-18-22 — the STRING sibling of B-2026-08-18-14, end to end.
/// `s[0..5].len()` failed the build with "element TypeExpr unknown" while
/// `karac check` and `--interp` both ran it: the receiver fell through to
/// the Vec/Slice/Array-element path, which has no String element to find.
///
/// The readers dispatch on a BORROWED `{ptr, len, cap = 0}` view rather
/// than an allocated slice, which is what separates them from the
/// `to_string`/`clone` arm that has always worked — those RETAIN the slice
/// and must allocate; a scalar reader consumes it inside the call. The
/// companion ASAN fixture pins the allocation count at zero; this pins the
/// answers.
///
/// A `ref String` receiver is included because it records as `Ref(Str)`,
/// the shape a span-keyed String test filters out, and the first cut of the
/// fix declined it silently. `trim()` is NOT here: it can hand back a value
/// aliasing the receiver, so it stays on the bind-to-a-`let` path by design.
#[test]
fn string_range_slice_scalar_readers_dispatch_in_receiver_position() {
    let src = "fn count(s: ref String) -> i64 {\n\
                       let mut n = 0;\n\
                       n = n + s[0..5].len();\n\
                       if s[0..5].starts_with(\"he\") { n = n + 1; }\n\
                       if s[6..11].contains(\"or\") { n = n + 1; }\n\
                       if s[0..5].is_empty() { n = n + 100; }\n\
                       return n;\n\
                   }\n\
                   fn main() {\n\
                       let src: String = \"hello world\";\n\
                       println(count(src).to_string());\n\
                       println(src[0..5].char_count().to_string());\n\
                       println(src[6..11].to_string());\n\
                       println(src.len().to_string());\n\
                   }\n";
    let Some(out) = run_program(src) else {
        return;
    };
    assert_eq!(
        out, "7\n5\nworld\n11\n",
        "a String range subscript must dispatch scalar readers on a borrowed view, \
             and must leave the source intact for its own later use"
    );
}

/// B-2026-08-18-14 — the E2E half: a RANGE SUBSCRIPT used directly as a
/// METHOD RECEIVER. `v[0..3].first_or(-1)` failed the build with "no
/// handler for expression kind Range" while `karac check` and `--interp`
/// both accepted it, which is why `a_slice_impl_head_can_read_its_own_
/// elements` above binds its slice to a local first.
///
/// TWO INDEPENDENT CAUSES, one per spelling, which is why both are here.
///
/// CHAINED (`.to_string()` inline): every postfix node copies its object's
/// span, so a method chain collapses onto its innermost receiver's key and
/// the last write wins — the chain's String-typed tail recorded `Type::Str`
/// against the `Vec[i64]` receiver's OWN span. `compile_index` read that and
/// built a fresh owned String (3-word `{ptr,len,cap}`) where a 2-word
/// `{ptr,len}` view was due, so the dispatcher rejected the shape and the
/// range fell through to the element-pointer lowering. Fixed by asking the
/// static type walk rather than a span-keyed table.
///
/// SPLIT across two statements: no chain, so no collision — and what
/// remained was `try_compile_nonident_slice_method` gating on a list of
/// BUILTIN slice method names, which declined a method a user `impl`
/// declared on the `Slice` head. Its materialize-and-re-dispatch body is
/// method-agnostic, so admitting the user method routes it to the same path
/// its `let`-bound twin already took.
///
/// The EMPTY slice exercises the impl's own `self.len() == 0` arm, so the
/// receiver has to be a real view with a correct length, not merely a value
/// that compiles.
#[test]
fn a_range_subscript_dispatches_a_user_slice_method_in_receiver_position() {
    let src = "trait Head { fn first_or(ref self, d: i64) -> i64; }\n\
                   impl Head for Slice[i64] {\n\
                       fn first_or(ref self, d: i64) -> i64 {\n\
                           if self.len() == 0 { return d; }\n\
                           return self[0];\n\
                       }\n\
                   }\n\
                   fn main() {\n\
                       let v: Vec[i64] = [10, 20, 30];\n\
                       println(v[0..3].first_or(-1).to_string());\n\
                       let r = v[1..3].first_or(-1);\n\
                       println(r.to_string());\n\
                       let empty: Vec[i64] = [];\n\
                       println(empty[0..0].first_or(-1).to_string());\n\
                   }\n";
    let Some(out) = run_program(src) else {
        return;
    };
    assert_eq!(
        out, "10\n20\n-1\n",
        "a range subscript in receiver position must dispatch as the slice view it is"
    );
}

/// B-2026-08-21-42 — `self.<method>()` inside an `impl … for Array[T, N]`
/// body.
///
/// The whole fixed-array method block sits inside an
/// `ExprKind::Identifier(name)` arm and reads `variables[name].ty`. A
/// `self` receiver parses as `ExprKind::SelfValue`, never as
/// `Identifier("self")`, so it could not reach that block and every method
/// of the surface failed inside the impl body while `--interp` ran it. It
/// is the Array twin of the fixed B-2026-08-18-12 (Map/Set) and -11.
///
/// The failure was in the BODY, not at the call site, which is why this
/// sweeps methods rather than call spellings: one `a.v_get()` at the top
/// would have been enough to reproduce, and would have told you nothing
/// about which of the surface's seven methods actually came back.
///
/// Three things beyond the read surface are here because each exercises a
/// different way the body can reach `self`, and the fix routes all of them
/// through one re-dispatch:
///
///   * `self[1]` — indexing, which has its own lowering path, so it
///     confirms the synthetic name reaches more than the method block.
///   * `for x in self` — the loop arm fixed in B-2026-08-21-41, kept here
///     so the two `self` recoveries are pinned against each other rather
///     than in separate files.
///   * `self.v_len() + self.v_get()` — one impl method calling two others.
///     The fix re-dispatches under a synthetic `Identifier("self")`, and
///     this is the case that would loop forever if that recursion were not
///     gated on the receiver provably being an array.
///
/// `ref self` is swept beside owned `self` because they recover storage
/// differently — the owned slot from `variables`, the borrow from
/// `ref_params` + `get_data_ptr` — so a fix that only reached one would
/// pass a test that only tried one. `Fl` gives the float element, and the
/// two free functions at the end read the same array OUTSIDE any impl, so
/// a regression in the general fixed-array read path cannot hide behind a
/// green `self` result.
#[test]
fn test_e2e_self_method_dispatch_in_an_array_impl_body() {
    let src = r#"
trait Reads {
    fn v_get(self) -> i64;
    fn v_first(self) -> i64;
    fn v_last(self) -> i64;
    fn v_len(self) -> i64;
    fn v_empty(self) -> bool;
    fn v_has(self, x: i64) -> bool;
    fn v_sorted(self) -> bool;
    fn v_index(self) -> i64;
    fn v_loop(self) -> i64;
    fn v_chain(self) -> i64;
    fn b_get(ref self) -> i64;
    fn b_len(ref self) -> i64;
    fn b_loop(ref self) -> i64;
}
impl Reads for Array[i64, 3] {
    fn v_get(self) -> i64 { return self.get(0).unwrap(); }
    fn v_first(self) -> i64 { return self.first().unwrap(); }
    fn v_last(self) -> i64 { return self.last().unwrap(); }
    fn v_len(self) -> i64 { return self.len(); }
    fn v_empty(self) -> bool { return self.is_empty(); }
    fn v_has(self, x: i64) -> bool { return self.contains(x); }
    fn v_sorted(self) -> bool { return self.is_sorted(); }
    fn v_index(self) -> i64 { return self[1]; }
    fn v_loop(self) -> i64 { let mut n = 0; for x in self { n = n + x; } return n; }
    fn v_chain(self) -> i64 { return self.v_len() + self.v_get(); }
    fn b_get(ref self) -> i64 { return self.get(2).unwrap(); }
    fn b_len(ref self) -> i64 { return self.len(); }
    fn b_loop(ref self) -> i64 { let mut n = 0; for x in self { n = n + x * 2; } return n; }
}

trait Fl { fn top(self) -> f64; }
impl Fl for Array[f64, 2] { fn top(self) -> f64 { return self.first().unwrap(); } }

fn free_get(a: ref Array[i64, 3]) -> i64 { return a.get(0).unwrap(); }
fn free_len(a: ref Array[i64, 3]) -> i64 { return a.len(); }

fn main() {
    let a: Array[i64, 3] = [7, 8, 9];
    println(a.v_get());
    println(a.v_first());
    println(a.v_last());
    println(a.v_len());
    println(a.v_empty());
    println(a.v_has(8));
    println(a.v_sorted());
    println(a.v_index());
    println(a.v_loop());
    println(a.v_chain());
    println(a.b_get());
    println(a.b_len());
    println(a.b_loop());
    let f: Array[f64, 2] = [1.5, 2.5];
    println(f.top());
    println(free_get(a));
    println(free_len(a));
}
"#;
    assert_eq!(
        run_program(src).as_deref(),
        Some("7\n7\n9\n3\nfalse\ntrue\ntrue\n8\n24\n10\n9\n3\n48\n1.5\n7\n3\n")
    );
}
