//! Vec -- fixtures for `tests/memory_sanitizer.rs`.
//!
//! Split out of `tests/memory_sanitizer.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test memory_sanitizer` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test memory_sanitizer vecs::
//!
//! New fixtures about Vec belong in this file.

use super::*;

/// B-2026-09-10-20 — the MEMORY half of `tests/codegen.rs`'s
/// `e2e_declared_vec_enum_payload_runs_element_drop_bodies` under
/// ASAN + LSan.
///
/// The row is a BODIES-channel gap — every `Vec` cell was memory-balanced
/// before the fix, which is exactly why no sanitizer leg could see it. This
/// fixture holds the other direction: the new walker reads through a
/// `{ptr, len, cap}` handle at a payload word and runs a body per element,
/// so a wrong field index, a wrong boxing threshold or a double walk all
/// land here as a use-after-free or a double free rather than as a
/// transcript diff. `vecmixed` (a two-field variant `P(Vec[S1], i64)`) is
/// the cell that pins the index, `vecempty` and `unitvar` pin the
/// zero-element and no-payload paths.
///
/// THE `gensh` CELL OF THE TRANSCRIPT TWIN IS DELIBERATELY ABSENT HERE.
/// `G.X(SMono.P(..))` strands its 88-byte RC control block — a real leak,
/// measured identically before and after this fix, and B-2026-09-17-15's
/// subject rather than this row's. Carrying it would redden LSan for
/// something this commit does not touch; quarantining it would put a live
/// entry on a ratchet list that is otherwise fully drained. The cell stays
/// pinned in the transcript fixtures, where it is visible without being
/// load-bearing.
#[test]
fn asan_declared_vec_enum_payload_elements_keep_one_owner() {
    assert_clean_asan_run(
            "struct R2 { s: String, t: String, u: String }\n\
             impl Drop for R2 { fn drop(mut ref self) { println(f\"  d2:{self.s.len()}\") } }\n\
             fn mkr(i: i64) -> R2 { return R2 { s: f\"aaaaaaaaa\", t: f\"b\", u: f\"c\" } }\n\
             struct S1 { v: i64 }\n\
             impl Drop for S1 { fn drop(mut ref self) { println(f\"  dS{self.v}\") } }\n\
             enum Mono { P(R2), Q }\n\
             shared enum SMono { P(R2), Q }\n\
             enum G[T] { X(T), Y }\n\
             enum H3 { P(SMono), Q }\n\
             enum H4 { P(Vec[Mono]), Q }\n\
             enum H5 { P(Vec[S1]), Q }\n\
             enum H6 { P(Vec[S1], i64), Q }\n\
             enum H7 { P(Array[S1, 2]), Q }\n\
             \n\
             fn main() {\n\
             \x20\x20\x20\x20println(\"vecenum\"); { let mut w: Vec[Mono] = []; w.push(Mono.P(mkr(1))); let h = H4.P(w); println(\"  x\") }\n\
             \x20\x20\x20\x20println(\"vecstruct\"); { let mut w: Vec[S1] = []; w.push(S1 { v: 7 }); w.push(S1 { v: 8 }); let h = H5.P(w); println(\"  x\") }\n\
             \x20\x20\x20\x20println(\"vecmixed\"); { let mut w: Vec[S1] = []; w.push(S1 { v: 9 }); let h = H6.P(w, 3); println(\"  x\") }\n\
             \x20\x20\x20\x20println(\"vecempty\"); { let w: Vec[S1] = []; let h = H5.P(w); println(\"  x\") }\n\
             \x20\x20\x20\x20println(\"unitvar\"); { let h = H5.Q; println(\"  x\") }\n\
             \x20\x20\x20\x20println(\"array\"); { let h = H7.P([S1 { v: 1 }, S1 { v: 2 }]); println(\"  x\") }\n\
             \x20\x20\x20\x20println(\"struct\"); { let g = G.X(mkr(1)); println(\"  x\") }\n\
             \x20\x20\x20\x20println(\"sharedec\"); { let h = H3.P(SMono.P(mkr(1))); println(\"  x\") }\n\
             \x20\x20\x20\x20println(\"genvec\"); { let mut w: Vec[Mono] = []; w.push(Mono.P(mkr(1))); let g = G.X(w); println(\"  x\") }\n\
             \x20\x20\x20\x20println(\"end\")\n\
             }\n\
",
            &[
                "vecenum",
                "  d2:9",
                "  x",
                "vecstruct",
                "  dS7",
                "  dS8",
                "  x",
                "vecmixed",
                "  dS9",
                "  x",
                "vecempty",
                "  x",
                "unitvar",
                "  x",
                "array",
                "  dS1",
                "  dS2",
                "  x",
                "struct",
                "  d2:9",
                "  x",
                "sharedec",
                "  x",
                // B-2026-09-17-19 — the shared enum payload's body now runs at
                // the refcount's 0-transition. ASAN was already clean here and
                // stays clean; only the line is new.
                "  d2:9",
                "genvec",
                // B-2026-09-20-62 — the generic enum's `Vec` payload now runs
                // its element's body on every surface, so this cell is an
                // agreement at the correct answer rather than the agreed gap
                // it pinned. ASAN was clean here before the flip and stays
                // clean: the failure this line fixes was an OUTPUT mismatch
                // ("ASAN passed, but output mismatched"), not a memory error.
                "  d2:9",
                "  x",
                "end",
            ],
            "declared_vec_enum_payload_one_owner",
        );
}

/// B-2026-09-01-41 — a struct's `Vec` FIELD, pushed to inside a loop whose
/// body also makes an unrelated heap temporary, never freed anything: the
/// field's whole `Vec`, buffer and every element. The row measured 173 B in
/// 4 allocations for three iterations, scaling by one element per iteration
/// (158 / 173 / 188 B at 2 / 3 / 4), so the field's cleanup was not firing
/// at all rather than firing at a stale length.
///
/// THE CONJUNCTION IS THE FIXTURE, because removing any one condition made
/// the row's program clean and would make this case vacuous:
///   1. the struct local is declared OUTSIDE the loop,
///   2. its `Vec` field is pushed to INSIDE it, and
///   3. the loop body also produces a heap temporary — the `println`
///      f-string, which touches nothing the struct owns.
///
/// The third is the one that reads like a coincidence and is not: dropping
/// that one line made the identical program clean at every iteration count
/// the row measured.
#[test]
fn asan_struct_vec_field_pushed_in_a_loop_with_a_temp_is_freed() {
    let Some((out, status)) = run_under_asan(
        r#"struct Bag { xs: Vec[Option[String]] }

fn main() {
    let mut b = Bag { xs: Vec.new() };
    let mut i = 0;
    while i < 3 {
        b.xs.push(Some(f"payloadpayload{i}"));
        println(f"{i}");
        i = i + 1;
    }
}
"#,
        "asan_struct_vec_field_pushed_in_a_loop_with_a_temp_is_freed",
    ) else {
        return;
    };
    assert!(status.success(), "ASAN/LSan reported a problem:\n{out}");
    assert_eq!(out, "0\n1\n2\n", "unexpected transcript:\n{out}");
}

/// B-2026-08-28-60 — a tuple binding whose element is a STRUCT carrying a
/// `Vec[<heap>]` frees that `Vec`'s elements, not just its buffer.
///
/// `tuple_elem_needs_deep_drop` chooses between two whole drop strategies
/// for every tuple binding. It admitted an element that IS a `Vec[<heap>]`
/// and not one that merely CONTAINS one, so the struct-wrapped spelling
/// took the LLVM-type drop — which reaches the `{ptr,len,cap}` buffer and
/// cannot see whether the elements behind it are scalars or `String`s.
///
/// THE TWO SPELLINGS SIDE BY SIDE ARE THE DIAGNOSIS, and `direct-vec-elem`
/// is here because it was already correct: one ownership question answered
/// two ways, with the difference being only whether a struct sits between
/// the tuple and the `Vec`.
///
/// `direct-string-field` is the boundary in the other direction. A struct
/// whose only heap is a plain `String` is freed correctly by the LLVM walk,
/// so the widened gate must NOT claim it — admitting every heap-owning
/// struct would move tuples that are already correct onto a different drop
/// for no reason.
#[test]
fn asan_tuple_binding_frees_a_struct_elements_vec_leaves() {
    const N: &str =
        "fn mkv(n: i64) -> Vec[String] { let mut v = Vec[String].new(); v.push(f\"t{n}\"); v }\n";
    assert_clean_asan_run(
            &format!(
                "{N}struct R {{ id: i64, tags: Vec[String] }}\n\
             \x20            fn main() {{ let p = (R {{ id: 41, tags: mkv(3) }}, 1); println(f\"{{p.1}}\") }}\n"
            ),
            &["1"],
            "struct-wrapped-vec",
        );
    // The same `Vec` reached through TWO struct levels.
    assert_clean_asan_run(
            &format!(
                "{N}struct Inner {{ tags: Vec[String] }}\n\
             \x20            struct Outer {{ id: i64, inner: Inner }}\n\
             \x20            fn main() {{ let p = (Outer {{ id: 41, inner: Inner {{ tags: mkv(3) }} }}, 1);\n\
             \x20            println(f\"{{p.1}}\") }}\n"
            ),
            &["1"],
            "two-struct-levels",
        );
    // Already correct, and the spelling that shows the gate was the axis.
    assert_clean_asan_run(
        &format!("{N}fn main() {{ let p = (mkv(3), 1); println(f\"{{p.1}}\") }}\n"),
        &["1"],
        "direct-vec-elem",
    );
    // BOUNDARY — a struct whose only heap is a direct `String`. The LLVM
    // walk frees this correctly; the widened gate must leave it alone.
    assert_clean_asan_run(
        "struct S { id: i64, name: String }\n\
             fn main() { let p = (S { id: 41, name: f\"n{41}\" }, 1);\n\
             \x20            println(f\"{p.0.name} {p.1}\") }\n",
        &["n41 1"],
        "direct-string-field",
    );
    // The element MOVED OUT of the tuple — the move neutralizer has to keep
    // working against the drop the widening newly selects.
    assert_clean_asan_run(
        &format!(
            "{N}struct R {{ id: i64, tags: Vec[String] }}\n\
             \x20            fn take(r: R) -> i64 {{ r.tags.len() }}\n\
             \x20            fn main() {{ let p = (R {{ id: 41, tags: mkv(3) }}, 1);\n\
             \x20            println(f\"{{take(p.0) + p.1}}\") }}\n"
        ),
        &["2"],
        "element-moved-out",
    );
    // A Drop-BEARING struct element carrying the same `Vec`: one body, and
    // the heap still fully freed.
    assert_clean_asan_run(
            &format!(
                "{N}struct R {{ id: i64, tags: Vec[String] }}\n\
             \x20            impl Drop for R {{ fn drop(mut ref self) {{ println(f\"drop {{self.id}}\") }} }}\n\
             \x20            fn main() {{ let p = (R {{ id: 41, tags: mkv(3) }}, 1); println(f\"{{p.1}}\") }}\n"
            ),
            &["1", "drop 41"],
            "drop-bearing-struct-elem",
        );
}

/// A fresh `Vec` temporary operand frees its ELEMENTS, not only its
/// buffer (B-2026-08-27-26).
///
/// `free_fresh_owned_str_arg` released the outer `{ptr, len, cap}` buffer
/// and nothing else. That is the whole allocation for a `String`, which is
/// what it was written for, and only the outer array for a container whose
/// elements own heap. Measured before the fix: the `Vec[String]` leg lost
/// 26 bytes in 4 allocations (the four element strings) and the
/// `Vec[Vec[i64]]` leg lost 64 bytes in 2 (the two inner buffers).
///
/// The `String` leg is the control that must stay clean, and it is not
/// decoration: the fix REPLACES the buffer free with a whole-`Vec` drop
/// rather than adding to it, since `karac_drop_Vec_<T>` releases the
/// buffer as well and running both would double-free. A regression that
/// re-added the buffer free would show up here rather than as a leak.
///
/// `push` and the map key are the consuming siblings. `push` MOVES its
/// argument into the container and the map key is moved by `insert`, so
/// both are shapes where freeing at the call site would double-free —
/// they pin that this fires only where the temporary genuinely dies.
/// `Vec.contains` frees a fresh-owned needle, and only a fresh one
/// (B-2026-08-27-28).
///
/// The arm BORROWS its needle -- the scan compares it against each element
/// and never stores it -- but it was the one lookup on `Vec` that never
/// freed it, while `Set.contains`, `Map.contains_key`, `starts_with` and
/// `binary_search` all did. So the leak was a missing call rather than a
/// guard declining, and it grew once per call. Measured before the fix:
/// 10 bytes in 2 allocations for the `String` needle over two calls, and
/// 202 bytes in 4 for the `Vec[String]` needle -- the needle ENTIRELY,
/// buffer and elements.
///
/// The `Vec` needle leg is what distinguishes this from B-2026-08-27-26:
/// there a free FIRED and released only the outer buffer, here none fired
/// at all. It also depends on that fix, since the freed needle's own
/// elements are drained by it.
///
/// THE THREE NON-FRESH LEGS ARE THE POINT OF THE FIXTURE, not padding. A
/// bound needle is owned by its binding, a place expression by its
/// container, and a literal by rodata -- freeing any of them here would be
/// a double-free or a free of static memory, so each is read again AFTER
/// the lookup to make a use-after-free observable rather than merely
/// possible.
#[test]
fn asan_vec_contains_frees_only_a_fresh_needle() {
    assert_clean_asan_run(
        r#"
fn name(n: i64) -> String { return f"item{n}"; }
fn ids(n: i64) -> Vec[String] {
    let mut v: Vec[String] = Vec.new();
    v.push(f"item{n}");
    return v;
}

fn main() {
    let mut v: Vec[String] = Vec.new();
    v.push("item1");
    v.push("item2");

    // fresh temporaries: freed here
    println(f"{v.contains(name(1))}");
    println(f"{v.contains(name(9))}");

    // bound: owned by the binding, must survive the lookup
    let bound = name(2);
    println(f"{v.contains(bound)}");
    println(bound);

    // place expression: owned by its container
    println(f"{v.contains(v[0])}");
    println(v[0]);

    // literal: rodata
    println(f"{v.contains("item1")}");

    // nested container needle, fresh and bound
    let mut outer: Vec[Vec[String]] = Vec.new();
    outer.push(ids(1));
    println(f"{outer.contains(ids(1))}");
    let held = ids(1);
    println(f"{outer.contains(held)}");
    println(held[0]);
}
"#,
        &[
            "true", "false", "true", "item2", "true", "item1", "true", "true", "true", "item1",
        ],
        "asan_vec_contains_frees_only_a_fresh_needle",
    );
}

#[test]
fn asan_fresh_vec_temp_operand_frees_its_elements() {
    assert_clean_asan_run(
        r#"
fn ids(n: i64) -> Vec[String] {
    let mut v: Vec[String] = Vec.new();
    v.push(f"item{n}");
    v.push(f"item{n + 1}");
    return v;
}
fn rows(n: i64) -> Vec[Vec[i64]] {
    let mut r: Vec[i64] = Vec.new();
    r.push(n);
    r.push(n + 1);
    let mut v: Vec[Vec[i64]] = Vec.new();
    v.push(r);
    return v;
}
fn name(n: i64) -> String { return f"item{n}"; }

fn main() {
    println(f"{ids(1) == ids(1)}");
    println(f"{rows(1) == rows(1)}");
    println(f"{name(1) == name(1)}");

    let mut outer: Vec[Vec[String]] = Vec.new();
    outer.push(ids(1));
    println(outer[0][0]);

    let mut m: Map[Vec[String], i64] = Map.new();
    m.insert(ids(3), 10);
    println(f"{m.get(ids(3))}");
}
"#,
        &["true", "true", "true", "item1", "Some(10)"],
        "asan_fresh_vec_temp_operand_frees_its_elements",
    );
}

#[test]
fn asan_vec_equality_borrows_both_operands() {
    assert_clean_asan_run(
        r#"
fn ids() -> Vec[String] {
    let mut v: Vec[String] = Vec.new();
    v.push("alpha");
    v.push("beta");
    return v;
}
fn main() {
    let a = ids();
    let b = ids();
    println(f"{a == b}");
    println(f"{ids() == ids()}");
    println(a[0]);
    println(b[1]);
}
"#,
        &["true", "true", "alpha", "beta"],
        "asan_vec_equality_borrows_both_operands",
    );
}

// B-2026-08-15-14 — a `Vec[shared struct]` TEMPORARY (a `.clone()` not
// bound to a `let`) freed its buffer while releasing NONE of the element
// references the clone had just taken. `karac_clone_Vec_Node` rc-INCs every
// element; the temp's `FreeVecBuffer` carried no per-element drop at all,
// so each element's RC box leaked once per temp.
//
// The named-binding sibling (`let c = v.clone();`) has always routed
// through `track_vec_of_aggs_var` and was clean — which is the whole
// discriminator: `let c = v.clone(); f(c)` leaked nothing, `f(v.clone())`
// leaked every element. THREE registration sites materialize such a temp,
// and each fixture below covers exactly one, so a partial revert fails a
// specific test rather than a random one:
//
//   (a) by-value call arg      -> `materialize_owned_temp`
//                                 (fixed in fd7254d)
//   (b) `ref` param call arg   -> `queue_ref_rvalue_arg_cleanup`
//   (c) `.len()` on the temp   -> `try_track_len_family_recv_temp`, whose
//       typechecker-side table skipped a `Type::Shared` element entirely
//
// (a) is already covered by `asan_inline_container_temp_arg_releases_its_
// elements`; the fixture kept here is the MULTI-ELEMENT form, for the
// reason below.
//
// MULTI-ELEMENT ON PURPOSE. With one element these leak one 32-byte box,
// and LSan's conservative stack scan sometimes finds a stale pointer to a
// single surviving box and reports CLEAN — measured, on the row's own
// one-element repro, as 2 leaked objects where 3 were stranded. A
// one-element fixture is therefore a flaky assertion; three elements put
// the count beyond that noise.

#[test]
fn asan_vec_shared_struct_clone_temp_by_value_arg_releases_elements() {
    // (a) the row's own shape, reduced: a clone passed straight into a
    // by-value `Vec[Node]` param, never bound.
    assert_clean_asan_run(
        r#"
shared struct Node { label: String }
fn agg(ns: Vec[Node]) -> i64 { return ns.len(); }
fn main() {
    let mut ns: Vec[Node] = Vec.new();
    ns.push(Node { label: "aaaaaaaaaaaaaaaa" });
    ns.push(Node { label: "bbbbbbbbbbbbbbbb" });
    ns.push(Node { label: "cccccccccccccccc" });
    println(agg(ns.clone()));
}
"#,
        &["3"],
        "asan_vec_shared_struct_clone_temp_by_value_arg_releases_elements",
    );
}

#[test]
fn asan_vec_shared_struct_clone_temp_ref_arg_releases_elements() {
    // (b) the same temp against a `ref Vec[Node]` param — a DIFFERENT
    // registration site (`queue_ref_rvalue_arg_cleanup`): the callee only
    // borrows, so the caller owns the temp and must release its elements.
    assert_clean_asan_run(
        r#"
shared struct Node { label: String }
fn agg(ns: ref Vec[Node]) -> i64 { return ns.len(); }
fn main() {
    let mut ns: Vec[Node] = Vec.new();
    ns.push(Node { label: "aaaaaaaaaaaaaaaa" });
    ns.push(Node { label: "bbbbbbbbbbbbbbbb" });
    ns.push(Node { label: "cccccccccccccccc" });
    println(agg(ns.clone()));
}
"#,
        &["3"],
        "asan_vec_shared_struct_clone_temp_ref_arg_releases_elements",
    );
}

#[test]
fn asan_vec_shared_struct_clone_temp_len_chain_releases_elements() {
    // (c) no user function at all — `v.clone().len()`. The parser gives a
    // MethodCall its RECEIVER's span, so the chain's scalar result
    // span-clobbers the receiver's `Vec[T]` in `expr_types` and the
    // `owned_temp_drops` hint is absent; the dedicated len-family table is
    // the only signal, and it recorded no `Type::Shared` element.
    assert_clean_asan_run(
        r#"
shared struct Node { label: String }
fn main() {
    let mut ns: Vec[Node] = Vec.new();
    ns.push(Node { label: "aaaaaaaaaaaaaaaa" });
    ns.push(Node { label: "bbbbbbbbbbbbbbbb" });
    ns.push(Node { label: "cccccccccccccccc" });
    println(ns.clone().len());
}
"#,
        &["3"],
        "asan_vec_shared_struct_clone_temp_len_chain_releases_elements",
    );
}

#[test]
fn asan_vec_shared_struct_clone_temp_nested_heap_field_released() {
    // The element's own heap is the INDIRECT half: a leaked `Node` box also
    // strands its `Vec[i64]`. A fix that rc-dec'd the handle but never
    // reached rc 0 would still leak these, so assert a Node whose field is
    // itself heap — 242 KB of indirect leak before the fix, vs 96 bytes of
    // direct.
    assert_clean_asan_run(
        r#"
shared struct Node { data: Vec[i64] }
fn mk(n: i64) -> Vec[i64] {
    let mut v: Vec[i64] = Vec.new();
    let mut i = 0;
    while i < n { v.push(i); i = i + 1; }
    return v;
}
fn main() {
    let mut ns: Vec[Node] = Vec.new();
    ns.push(Node { data: mk(3) });
    ns.push(Node { data: mk(300) });
    ns.push(Node { data: mk(3000) });
    println(ns.clone().len());
}
"#,
        &["3"],
        "asan_vec_shared_struct_clone_temp_nested_heap_field_released",
    );
}

#[test]
fn asan_vec_shared_struct_clone_temp_repeated_no_over_release() {
    // The OVER-release guard. Every fixture above would also pass if the
    // fix released too much, because LSan does not see a double-free —
    // ASAN does. Clone the same Vec many times in a loop and keep reading
    // the ORIGINAL afterward: an extra dec per temp drives the shared
    // count to 0 while `ns` still holds it, which is a use-after-free on
    // the read and a double-free at `main`'s exit.
    assert_clean_asan_run(
        r#"
shared struct Node { label: String }
fn agg(ns: Vec[Node]) -> i64 { return ns.len(); }
fn main() {
    let mut ns: Vec[Node] = Vec.new();
    ns.push(Node { label: "aaaaaaaaaaaaaaaa" });
    ns.push(Node { label: "bbbbbbbbbbbbbbbb" });
    ns.push(Node { label: "cccccccccccccccc" });
    let mut k = 0;
    let mut t = 0;
    while k < 20 { t = t + agg(ns.clone()); k = k + 1; }
    println(t);
    println(ns[0].label);
    println(ns.len());
}
"#,
        &["60", "aaaaaaaaaaaaaaaa", "3"],
        "asan_vec_shared_struct_clone_temp_repeated_no_over_release",
    );
}

#[test]
fn asan_vec_shared_enum_clone_temp_releases_elements() {
    // A `shared ENUM` element rides the same `Type::Shared` predicate and
    // the same per-element rc-dec. Included because the row's repro used a
    // shared STRUCT, and a name-keyed fix that happened to cover only
    // structs would pass every fixture above.
    assert_clean_asan_run(
        r#"
shared enum Shape { Circle(i64), Rect(i64, i64) }
fn agg(ss: Vec[Shape]) -> i64 { return ss.len(); }
fn main() {
    let mut ss: Vec[Shape] = Vec.new();
    ss.push(Shape.Circle(3i64));
    ss.push(Shape.Rect(4i64, 5i64));
    ss.push(Shape.Circle(6i64));
    println(agg(ss.clone()));
    println(ss.clone().len());
}
"#,
        &["3", "3"],
        "asan_vec_shared_enum_clone_temp_releases_elements",
    );
}

#[test]
fn asan_push_aliased_bare_shared_element_retained_no_uaf() {
    // B-2026-07-21-13: `node.neighbors.push(nodes[j])` — pushing an ALIASING
    // bare `shared struct` element (an indexed pool-Vec read, reference-
    // semantic so no clone/inc) into another node's `Vec[Node]` must RETAIN
    // it. `build_graph` links `nodes[0].neighbors -> nodes[1]` then RETURNS
    // the pool (its local scope ends); without the retain the returned pool's
    // element `nodes[1]` was freed while `nodes[0].neighbors` still pointed at
    // it — reading `neighbors[0].val` was a use-after-free / garbage (kata
    // #133 Clone Graph, build/JIT diverged from interp). ACYCLIC here so it is
    // fully reclaimable — a cyclic adjacency would leak by RC construction
    // (out of scope, like the #141 linked-list cycle). Must print 200 and be
    // LSan-clean.
    assert_clean_asan_run(
        r#"
shared struct Node { val: i64, mut neighbors: Vec[Node] }
fn build() -> Vec[Node] {
    let mut nodes: Vec[Node] = Vec.new();
    nodes.push(Node { val: 100i64, neighbors: Vec.new() });
    nodes.push(Node { val: 200i64, neighbors: Vec.new() });
    nodes[0i64].neighbors.push(nodes[1i64]);
    return nodes;
}
fn main() {
    let g: Vec[Node] = build();
    println(g[0i64].neighbors[0i64].val.to_string());
}
"#,
        &["200"],
        "asan_push_aliased_bare_shared_element_retained_no_uaf",
    );
}

#[test]
fn asan_vec_indexed_shared_option_field_store_no_crash_no_leak() {
    // B-2026-07-19-6: `v[i].next = Some(v[j])` — a field store of an
    // `Option[shared]` into a Vec-INDEXED shared-struct element (identifier
    // root). Two codegen bugs on this path, both fixed:
    //   (1) OFFSET — the store hardcoded `info.heap_type` + `(idx+1)`, but a
    //       HEADERLESS shared struct allocates without the refcount word, so
    //       the write landed 8 bytes PAST the element block (heap overflow →
    //       SIGSEGV). Now routed through `shared_gep_layout`.
    //   (2) RETAIN — the store was a raw `build_store` with no RC retain, so
    //       the linked node was under-counted and both the field drop and the
    //       Vec element drop freed it (double free). Now routed through
    //       `emit_[niche_]option_shared_field_store` like the bare-identifier
    //       branch.
    // Build a 5-node linear list by index-store, walk it via a node handle,
    // and drop — must print the value sum (20) and be ASAN/LSan-clean. (An
    // ACYCLIC list frees fully; a `random`-style back-pointer would form an
    // RC cycle, which reference counting leaks by construction — out of scope
    // here.)
    assert_clean_asan_run(
        r#"
shared struct Node { val: i64, id: i64, mut next: Option[Node] }
fn main() {
    let mut v: Vec[Node] = Vec.new();
    let mut i = 0i64;
    while i < 5i64 {
        v.push(Node { val: i * 2i64, id: i, next: None });
        i = i + 1;
    }
    i = 0i64;
    while i < 5i64 {
        if i + 1i64 < 5i64 {
            v[i].next = Some(v[i + 1i64]);
        }
        i = i + 1;
    }
    let mut h = 0i64;
    let mut cur: Option[Node] = Some(v[0i64]);
    let mut go = true;
    while go {
        match cur {
            None => { go = false; }
            Some(n) => { h = h + n.val; cur = n.next; }
        }
    }
    println(h.to_string());
}
"#,
        &["20"],
        "asan_vec_indexed_shared_option_field_store_no_crash_no_leak",
    );
}

#[test]
fn asan_vec_of_weak_read_back_balanced_no_use_after_free() {
    // B-2026-08-08-4 gap B (read-back) — the balancing acquire.
    //
    // A weak read is a BORROW: `emit_weak_field_upgrade` hands out a box
    // pointer with NO strong retain, so an `Option[shared T]` binding
    // initialized from one owns no +1 and its scope-exit `RcDecOption`
    // needs a matching inner acquire. `expr_is_weak_field_read` — the gate
    // for that acquire — matched only `ExprKind::FieldAccess`, so a
    // container-ELEMENT read got the dec without the inc and over-released
    // its target: valgrind reported `Invalid read of size 8` against a
    // 24-byte box already freed, and `karac run` (JIT) printed the right
    // answer and then died in `malloc(): unaligned tcache chunk detected`.
    // Exactly the failure B-2026-07-21-21 measured for the field form,
    // reached through the container instead.
    //
    // The read happens in a HELPER whose frame then dies, and the target
    // dies with it — both because that is the `None` path worth pinning,
    // and because B-2026-08-08-4 documents that LSan under-reports when the
    // last handles survive in an unoverwritten dead frame.
    assert_clean_asan_run(
        r#"
shared struct N { mut v: i64 }
fn fill(w: mut ref Vec[weak N]) {
    let a: N = N { v: 7i64 };
    w.push(a);
    match w[0] { Some(x) => { println(x.v); } None => { println(0 - 1); } }
}
fn churn(d: i64) -> i64 { if d <= 0i64 { 0i64 } else { d + churn(d - 1i64) } }
fn main() {
    let mut w: Vec[weak N] = Vec.new();
    fill(mut w);
    println(churn(64i64));
    match w[0] { Some(x) => { println(x.v); } None => { println(0 - 2); } }
}
"#,
        &["7", "2080", "-2"],
        "vec_of_weak_read_back_balanced",
    );
}

#[test]
fn asan_weak_vec_field_element_read_balanced_no_use_after_free() {
    // B-2026-08-08-28 — the sibling above fixed the balancing acquire for a
    // weak element read through a BARE VARIABLE (`w[0]`). The gate it added
    // matched `ExprKind::Index` only when the receiver was an
    // `ExprKind::Identifier`, so a Vec held as a struct FIELD (`a.ns[0]`)
    // fell through both halves of `expr_is_weak_field_read` — the index arm
    // rejected the receiver, and the field arm never ran because the
    // expression is an `Index`, not a `FieldAccess`.
    //
    // That is the shape every real weak program has: design.md's own graph
    // example is `shared struct Node { mut neighbors: Vec[weak Node] }`. It
    // took the dec without the inc and over-released, with three distinct
    // symptoms depending on how many reads happened:
    //
    //   1 read                -> 48 bytes leaked (5 allocs, 4 frees)
    //   2 reads, same element -> SILENT WRONG ANSWER: the second read of a
    //                            plainly-live target yields `None`
    //                            (interp 4, JIT and AOT both 1)
    //   2 reads, two owners   -> SIGSEGV under JIT and AOT, while
    //                            `--interp` printed the right answer
    //
    // This fixture is the two-owner shape — the one that crashed — because
    // it also covers the other two: it reads through both nodes of a weak
    // cycle, so a missing acquire shows up as a UAF long before the
    // scope-exit accounting is reached. The reads happen in a HELPER whose
    // frame then dies, per B-2026-08-08-4's note that LSan under-reports
    // when the last handles survive in an unoverwritten dead frame.
    assert_clean_asan_run(
        r#"
shared struct N { v: i64, mut ns: Vec[weak N] }
fn build(seed: i64) -> i64 {
    let a = N { v: seed, ns: Vec.new() };
    let b = N { v: seed + 1i64, ns: Vec.new() };
    a.ns.push(b);
    b.ns.push(a);
    let mut acc: i64 = 0i64;
    match a.ns[0] { Some(x) => { acc = acc + x.v; } None => { acc = acc - 1i64; } }
    match b.ns[0] { Some(y) => { acc = acc + y.v; } None => { acc = acc - 1i64; } }
    match a.ns[0] { Some(z) => { acc = acc + z.v; } None => { acc = acc - 1i64; } }
    return acc;
}
fn churn(d: i64) -> i64 { if d <= 0i64 { 0i64 } else { d + churn(d - 1i64) } }
fn main() {
    println(build(1i64));
    println(churn(64i64));
}
"#,
        &["5", "2080"],
        "weak_vec_field_element_read_balanced",
    );
}

#[test]
fn asan_vec_truncate_heap_no_leak() {
    // `Vec[String].truncate(n)` drops the [n, len) tail — each dropped
    // String's buffer must be freed exactly once, the surviving prefix and
    // the buffer stay owned, and a re-push after truncation reuses the
    // capacity cleanly. A SECOND Vec keeps the auto-parallelizer's racing
    // group present (truncate is seeded receiver-mutating, B-2026-07-14-17,
    // so it serializes — no double-free). Looped over heap elements so any
    // per-iteration imbalance accumulates for ASan/LSan.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0i64;
    while i < 3i64 {
        let mut s: Vec[String] = Vec.new();
        s.push(f"a-{i}-padding-padding");
        s.push(f"b-{i}-padding-padding");
        s.push(f"c-{i}-padding-padding");
        s.truncate(1);
        println(s.len());
        println(s[0]);
        s.push(f"d-{i}-padding-padding");
        println(s[1]);
        let mut t: Vec[String] = Vec.new();
        t.push(f"t-{i}-padding-padding");
        println(t.len());
        i = i + 1;
    }
}
"#,
        &[
            "1",
            "a-0-padding-padding",
            "d-0-padding-padding",
            "1",
            "1",
            "a-1-padding-padding",
            "d-1-padding-padding",
            "1",
            "1",
            "a-2-padding-padding",
            "d-2-padding-padding",
            "1",
        ],
        "asan_vec_truncate_heap_no_leak",
    );
}

#[test]
fn asan_vec_swap_remove_heap_no_leak() {
    // `Vec[String].swap_remove(i)` moves element `i` OUT (returned + printed,
    // then dropped) and moves the LAST element into slot `i` — pure moves, no
    // clone: element `i`'s buffer transfers to the return value and the last
    // element's buffer transfers into slot `i`, so the vacated tail slot is
    // abandoned by `len--` with nothing to free. ASan/LSan flag a double-free
    // if slot `i` were also freed, or a leak if the moved-out element weren't.
    // A second Vec keeps the auto-parallelizer's racing group present
    // (swap_remove is seeded receiver-mutating, B-2026-07-14-17). Looped so
    // any per-iteration imbalance accumulates.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0i64;
    while i < 3i64 {
        let mut s: Vec[String] = Vec.new();
        s.push(f"a-{i}-padding-padding");
        s.push(f"b-{i}-padding-padding");
        s.push(f"c-{i}-padding-padding");
        let removed = s.swap_remove(0);
        println(removed);
        println(s.len());
        println(s[0]);
        let mut t: Vec[String] = Vec.new();
        t.push(f"t-{i}-padding-padding");
        println(t.len());
        i = i + 1;
    }
}
"#,
        &[
            "a-0-padding-padding",
            "2",
            "c-0-padding-padding",
            "1",
            "a-1-padding-padding",
            "2",
            "c-1-padding-padding",
            "1",
            "a-2-padding-padding",
            "2",
            "c-2-padding-padding",
            "1",
        ],
        "asan_vec_swap_remove_heap_no_leak",
    );
}

#[test]
fn asan_match_bound_enum_payload_vec_field_copy_no_double_free() {
    // B-2026-07-17-20: copying a Vec field OUT of a match-bound struct
    // payload that aliases a BORROWED ref-Vec enum element
    // (`for it in items { match it { Fu(f) => { let ps = f.params; … } } }`,
    // `items: ref Vec[It]`) shallow-aliased the container's buffer, so the
    // copy's scope-exit free AND the caller's element drop freed the same
    // buffer → `free(): double free`, SIGABRT 134. The struct payload binding
    // now deep-copies the extracted field (the enum-payload sibling of the
    // for-loop struct-element path). 300 iters, both Vec[i64-struct] and
    // Vec[String] payload elements.
    assert_clean_asan_run(
        r#"
struct P { n: i64 }
struct F { name: String, params: Vec[P], tags: Vec[String] }
enum It { Fu(F), Other }
fn collect(items: ref Vec[It]) -> i64 {
    let mut total = 0;
    for it in items {
        match it {
            It.Fu(f) => {
                let ps = f.params;
                for p in ps { total = total + p.n; }
                let ts = f.tags;
                for t in ts { total = total + t.len(); }
            }
            It.Other => {}
        }
    }
    total
}
fn main() {
    let mut total: i64 = 0;
    let mut i: i64 = 0;
    while i < 300 {
        let mut items: Vec[It] = Vec.new();
        let mut ps: Vec[P] = Vec.new();
        ps.push(P { n: 1 });
        ps.push(P { n: 2 });
        let mut tg: Vec[String] = Vec.new();
        tg.push("tag");
        items.push(It.Fu(F { name: f"n-{i}", params: ps, tags: tg }));
        items.push(It.Other);
        total = total + collect(items);
        i = i + 1;
    }
    println(total.to_string());
}
"#,
        // per iter: params 1+2=3, tags len("tag")=3 → 6/iter × 300 = 1800.
        &["1800"],
        "match_bound_enum_payload_vec_field_copy_no_double_free",
    );
}

#[test]
fn asan_match_bound_struct_variant_vec_field_reborrow_no_double_free() {
    // B-2026-07-18-4: a STRUCT-VARIANT payload's Vec field bound DIRECTLY
    // (`match it { Fu { params } => … }`) then whole-moved into a local
    // (`let ps = params`) over `items: ref Vec[It]`. `params` is typed
    // `ref Vec[T]` (a borrow of the container-owned buffer), so `ps` is a
    // re-borrow that must alias with NO scope-exit free. Pre-fix `ps` was
    // unregistered (AOT compiled `for p in ps` to an empty Vec — wrong
    // answer); the naive dispatch-only fix then armed a `FreeVecBuffer` on
    // the alias → `free(): double free`. The alias-bind fix keeps the
    // container the sole owner. 300 iters over Vec[i64-struct] and
    // Vec[String] payload elements: a missed alias double-frees (ASAN); an
    // over-eager copy that never frees leaks (LSan).
    assert_clean_asan_run(
        r#"
struct P { n: i64 }
struct Q { s: String }
enum It { Fu { params: Vec[P], tags: Vec[Q] }, Other }
fn collect(items: ref Vec[It]) -> i64 {
    let mut total = 0;
    for it in items {
        match it {
            It.Fu { params, tags } => {
                let ps = params;
                for p in ps { total = total + p.n; }
                let ts = tags;
                for t in ts { total = total + t.s.len(); }
            }
            It.Other => {}
        }
    }
    total
}
fn main() {
    let mut total: i64 = 0;
    let mut i: i64 = 0;
    while i < 300 {
        let mut items: Vec[It] = Vec.new();
        let mut ps: Vec[P] = Vec.new();
        ps.push(P { n: 1 });
        ps.push(P { n: 2 });
        let mut tg: Vec[Q] = Vec.new();
        tg.push(Q { s: f"tag-{i}" });
        items.push(It.Fu { params: ps, tags: tg });
        items.push(It.Other);
        total = total + collect(items);
        i = i + 1;
    }
    println(total.to_string());
}
"#,
        // per iter: params 1+2=3, tags len("tag-<i>") for i in 0..299.
        // i<10 → 5 chars, 10..=99 → 6, 100..=299 → 7:
        // 3*300 + (5*10 + 6*90 + 7*200) = 900 + (50 + 540 + 1400) = 2890.
        &["2890"],
        "match_bound_struct_variant_vec_field_reborrow_no_double_free",
    );
}

#[test]
fn asan_owned_vec_param_match_arm_return_no_leak_or_double_free() {
    // B-2026-07-13-1, nested-heap `match`-arm sibling: a `Vec[String]`
    // param returned from a `match` arm deep-copies the outer buffer AND
    // each String element. 100 iterations catch any element or outer leak.
    assert_clean_asan_run(
        r#"
fn choose(a: Vec[String], b: Vec[String], first: bool) -> Vec[String] {
    match first {
        true => a,
        false => b,
    }
}

fn main() {
    let mut i: i64 = 0i64;
    let mut total: i64 = 0i64;
    while i < 100i64 {
        let mut x: Vec[String] = Vec.new();
        x.push(f"one{i}");
        x.push(f"two{i}");
        let mut y: Vec[String] = Vec.new();
        y.push(f"three{i}");
        let r = choose(x, y, false);
        total = total + r.len();
        i = i + 1;
    }
    println(total.to_string());
}
"#,
        // `choose(.., false)` returns y (len 1) each of 100 iters = 100.
        &["100"],
        "owned_vec_param_match_arm_return_no_leak_or_double_free",
    );
}

// NOTE: the field-push residual UAF half (DEFECT 2) is covered by the
// sibling's stronger `asan_field_read_option_shared_push_no_leak_or_uaf`
// (200-iteration loop, drains + reads back) — no duplicate here. The two
// tests below cover the pop-consume DRAIN leak (DEFECT 1), which the
// sibling's for-loop-drain test does not exercise.
#[test]
fn asan_vec_option_shared_pop_consume_no_leak() {
    // B-2026-07-12-4 (leak half) — draining a `Vec[Option[shared]]` via
    // `match vec.pop() { Some(opt) => match opt { Some(n) => .. } }` used to
    // leak every popped node: the pop result is `Option[Option[shared]]`
    // whose boxed inner-Option payload was freed WITHOUT rc-deccing the node
    // (the box drop's inner drop fn was None). The boxed-scrutinee /
    // let-binding box drop now runs the inner `Option[T]` element drop.
    // Covers both a fresh `Some(Node)` push and a field-read push, drained to
    // empty (`total = 2 + 3 = 5`).
    assert_clean_asan_run(
        r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn main() {
    let root = Some(Node { val: 5, left: Some(Node { val: 3, left: None, right: None }), right: None });
    let mut stack: Vec[Option[Node]] = Vec.new();
    stack.push(Some(Node { val: 2, left: None, right: None }));
    match root {
        None => {}
        Some(n) => { stack.push(n.left); }
    }
    let mut total = 0;
    while stack.len() > 0 {
        let x = stack.pop();
        match x {
            Some(opt) => { match opt { Some(node) => { total = total + node.val; } None => {} } }
            None => {}
        }
    }
    println(total.to_string());
}
"#,
        &["5"],
        "vec_option_shared_pop_consume_no_leak",
    );
}

#[test]
fn asan_generic_assoc_fn_vec_field_no_leak() {
    // B-2026-07-11-25 — a generic struct `S[T]` whose associated constructor
    // `S.new()` returns `S { items: Vec.new() }`, then pushes through
    // `mut ref self`. Before the fix the constructor returned a ZEROED struct
    // (its `items` a garbage Vec header), so pushes reallocated against
    // garbage (OOM / corruption). Now that `S.new()` monomorphizes correctly,
    // this asserts the Vec[T] field it builds is a real `{null,0,0}` that
    // grows and frees cleanly — no leak, no double-free — across a
    // String-element instantiation (heap payloads) built and dropped in a loop.
    assert_clean_asan_run(
        r#"
struct S[T] { items: Vec[T] }
impl[T] S[T] {
    fn new() -> S[T] { S { items: Vec.new() } }
    fn push(mut ref self, x: T) { self.items.push(x); }
    fn len(ref self) -> i64 { self.items.len() }
}
fn main() {
    let mut r: i64 = 0;
    while r < 3 {
        let mut s: S[String] = S.new();
        s.push(f"row-{r}-aaaa");
        s.push(f"row-{r}-bbbb");
        s.push(f"row-{r}-cccc");
        println(f"{s.len()}");
        r = r + 1;
    }
}
"#,
        &["3", "3", "3"],
        "asan_generic_assoc_fn_vec_field_no_leak",
    );
}

#[test]
fn asan_with_capacity_zero_no_leak() {
    // B-2026-07-11-15 — a `with_capacity(n)` whose `n` evaluates to 0 at
    // runtime leaked one byte per call. `karac_alloc_or_panic(0)` normalizes
    // `0 → 1` and returns a real non-null buffer, but the zero-cap collection
    // stores `cap = 0`, and the `cap > 0 ⇔ owned heap` drop convention skips
    // freeing a `cap == 0` buffer — orphaning that 1-byte allocation. The
    // `presize.rs` pass makes this common by rewriting
    // `let mut v = Vec.new(); while i < k { v.push(..) }` to
    // `Vec.with_capacity(k)`, so a `k == 0` counted-fill loop (here the VM's
    // `run(prog, 0)` with no locals) leaked once per call.
    //
    // Exercises all three affected constructors at a zero runtime capacity:
    // the presize-driven `Vec.with_capacity` (via the counted push loop), a
    // direct `Vec[i64].with_capacity(0)`, a `String.with_capacity(0)`, and
    // the fallible `Vec.try_with_capacity(0)` / `String.try_with_capacity(0)`
    // (whose zero case must be `Ok`, not a spurious OOM `Err`). Every one must
    // drop to `{null, 0, 0}` (bit-identical to `.new()`) — nothing to free.
    assert_clean_asan_run(
        r#"
fn fill(n: i64) -> i64 {
    // presize rewrites `Vec.new()` -> `Vec.with_capacity(n)`; n == 0 here.
    let mut v: Vec[i64] = Vec.new();
    let mut i = 0i64;
    while i < n {
        v.push(i);
        i = i + 1;
    }
    v.len()
}
fn main() {
    let mut total: i64 = 0i64;
    let mut r: i64 = 0i64;
    while r < 3i64 {
        total = total + fill(0i64);          // zero-cap presized Vec
        let a: Vec[i64] = Vec.with_capacity(0i64);
        total = total + a.len();             // direct zero-cap Vec
        let s: String = String.with_capacity(0i64);
        total = total + s.len();             // direct zero-cap String
        let tv: Vec[i64] = Vec.try_with_capacity(0i64).unwrap();
        total = total + tv.len();            // fallible zero-cap Vec -> Ok
        let ts: String = String.try_with_capacity(0i64).unwrap();
        total = total + ts.len();            // fallible zero-cap String -> Ok
        r = r + 1i64;
    }
    println(f"{total}");
    // A nonzero cap on the SAME path still grows and frees correctly.
    let mut w: Vec[i64] = Vec.with_capacity(0i64);
    w.push(7i64);
    w.push(8i64);
    println(f"{w.len()}");
}
"#,
        &["0", "2"],
        "asan_with_capacity_zero_no_leak",
    );
}

#[test]
fn asan_for_loop_var_into_tuple_push_no_double_free() {
    // B-2026-07-04-3: an inline tuple `(i, x)` whose heap component `x` is a
    // `for`-loop element variable, pushed into a Vec, double-freed the heap
    // component (exit 133/134) — codegen iterates the Vec in place so `x`
    // ALIASES the source buffer; the tuple then aliased it too, and both the
    // source's scope-exit free and the pushed Vec's element drop released
    // it. `compile_tuple` now `maybe_defensive_copy_param_arg`s each element
    // (exactly as `v.push(x)` / struct-literal fields / call args do), so a
    // retaining source (for-loop borrow, owned param) is deep-copied into
    // the tuple. Exercises `.iter()` iteration, owned iteration, the heap
    // component in either tuple slot, and an owned-param-into-tuple — reading
    // an element each time to expose the UAF. `.clone()` and plain-local
    // elements (already clean) are the control.
    assert_clean_asan_run(
            r#"
fn wrap_param(s: String, k: i64) -> Vec[(i64, String)] {
    let mut v: Vec[(i64, String)] = Vec.new();
    v.push((k, s));
    return v;
}
fn main() {
    let mut round: i64 = 0i64;
    while round < 40i64 {
        let w: Vec[String] = Vec[
            "alpha-loop-element-payload-aaaaaaaaaaaaaaaaaaaa".to_string(),
            "bravo-loop-element-payload-bbbbbbbbbbbbbbbbbbbb".to_string(),
            "charlie-loop-element-payload-cccccccccccccccccc".to_string()
        ];
        let mut a: Vec[(i64, String)] = Vec.new();
        let mut i: i64 = 0i64;
        for x in w.iter() { a.push((i, x)); i = i + 1i64; }
        let pa = a[2].clone();
        let mut b: Vec[(String, i64)] = Vec.new();
        for y in w.iter() { b.push((y, 9i64)); }
        let pb = b[0].clone();
        let owned: Vec[String] = Vec[
            "delta-owned-element-payload-dddddddddddddddddddd".to_string(),
            "echo-owned-element-payload-eeeeeeeeeeeeeeeeeeeee".to_string()
        ];
        let mut c: Vec[(i64, String)] = Vec.new();
        for z in owned { c.push((0i64, z)); }
        let pc = c[1].clone();
        let d: Vec[(i64, String)] = wrap_param("param-element-payload-ffffffffffffffffffff".to_string(), 5i64);
        let pd = d[0].clone();
        println(f"{pa.0} {pa.1} {pb.0} {pb.1} {pc.0} {pc.1} {pd.0} {pd.1}");
        round = round + 1i64;
    }
}
"#,
            [
                "2 charlie-loop-element-payload-cccccccccccccccccc alpha-loop-element-payload-aaaaaaaaaaaaaaaaaaaa 9 0 echo-owned-element-payload-eeeeeeeeeeeeeeeeeeeee 5 param-element-payload-ffffffffffffffffffff",
            ]
            .repeat(40)
            .as_slice(),
            "asan_for_loop_var_into_tuple_push_no_double_free",
        );
}

/// Type-changing shadow (phase-5-diagnostics "codegen
/// type-changing-shadow"): `let v = v.len()` rebinds a heap `Vec` to a
/// scalar. The shadow dance purges `v`'s `vec_elem_types` tag so the new
/// i64 binding dispatches correctly — but the OLD Vec's scope-exit free
/// MUST still fire. Scope-exit drops are queued by alloca at bind time
/// (`scope_cleanup_actions`), not re-derived from the purged name-maps, so
/// forgetting the metadata cannot drop the cleanup. If it could, the
/// 128-byte buffer (16 i64s — well past the ≥36-byte LSan reachability
/// floor) would leak. ASAN must report clean: no leak, no double-free.
#[test]
fn asan_type_changing_shadow_vec_to_scalar_frees_old_buffer() {
    assert_clean_asan_run(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    let mut i = 0i64;
    while i < 16i64 { v.push(i * 7i64); i = i + 1i64; }
    let v = v.len();
    println(v);
}
"#,
        &["16"],
        "type_changing_shadow_vec_to_scalar",
    );
}

#[test]
fn asan_vec_shared_elem_into_some_returned_no_leak_no_uaf() {
    // B-2026-06-15-1 (#226 invert-binary-tree): a bare `shared` struct read
    // out of a `Vec` element into an enum-ctor payload (`Some(nodes[i])`)
    // shallow-aliases without an rc-inc. `rhs_yields_fresh_ref` treats the
    // ctor as fresh, so the return/field consumers skip their inc; the
    // payload was then under-counted and freed when the source `Vec`
    // dropped (its correct per-element dec landed in 0890627c). Building a
    // chain through `nodes[i]`, returning `Some(nodes[0])`, then walking it
    // AFTER the Vec drops read freed memory — non-deterministic garbage /
    // crash (mac ASAN: UAF). The fix (`share_bare_shared_ctor_payload`,
    // scoped to the `v[i]` index) rc-inc's the aliased element so the
    // returned chain outlives the Vec. The loop makes any over-inc visible
    // to the Linux-CI LSan gate (the broad first cut leaked on fresh-local
    // `Some(node)` payloads — this pins the index-only scope).
    assert_clean_asan_run(
        r#"
shared struct N { v: i64, mut next: Option[N] }
fn build(k: i64) -> Option[N] {
    let mut nodes: Vec[N] = Vec.new();
    let mut i: i64 = 0;
    while i < k {
        nodes.push(N { v: i, next: None });
        i = i + 1;
    }
    let mut j: i64 = 1;
    while j < k {
        let mut cur = nodes[j - 1];
        cur.next = Some(nodes[j]);
        j = j + 1;
    }
    return Some(nodes[0]);
}
fn sum_chain(root: Option[N]) -> i64 {
    let mut s: i64 = 0;
    let mut cur = root;
    loop {
        match cur {
            None => { break; },
            Some(n) => { s = s + n.v; cur = n.next; },
        }
    }
    return s;
}
fn main() {
    let mut iter: i64 = 0;
    let mut total: i64 = 0;
    while iter < 50 {
        let r = build(20);
        total = total + sum_chain(r);
        iter = iter + 1;
    }
    println(total);
}
"#,
        &["9500"],
        "vec_shared_elem_into_some_returned",
    );
}

#[test]
fn asan_refinement_try_from_vec_no_double_free() {
    // `Refined.try_from(v)` over a collection base (`type NonEmptyV =
    // Vec[String] where ...`) consumes `v`: on the Ok path the buffer lives
    // in the `Ok` payload, so the source must not free it again. Looped so
    // a per-iteration double-free trips ASAN. The Weave
    // `NonEmpty.try_from(enriched)` class.
    assert_clean_asan_run(
        r#"
type NonEmptyV = Vec[String] where self.len() > 0;
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 50 {
        let mut v: Vec[String] = Vec.new();
        v.push(f"a-{i}");
        v.push(f"b-{i}");
        match NonEmptyV.try_from(v) {
            Ok(rows) => { total = total + rows.len(); }
            Err(_)   => { total = total + 0; }
        }
        i = i + 1;
    }
    println(total);
}
"#,
        &["100"],
        "refinement_try_from_vec",
    );
}

// ── `Vec[String].clear()` + `.extend(...)` — heap-element drop ownership ──
//
// `clear()` must DROP every element before resetting the length — for a
// `Vec[String]` those are heap buffers, so a "just set len=0" implementation
// would leak them (LSan). `extend(other)` appends CLONES of `other`'s
// elements, so both vectors own their strings and each buffer is freed
// exactly once (a stray alias would double-free under ASAN; a missed clone
// would leak the source at scope exit). Looped 1000× with 40-byte payloads
// so LSan catches a per-iter leak past any short-String fast path; the
// reuse-after-clear (push + extend rebuild the buffer) exercises the
// reset-to-`{null,0,0}` header path.
#[test]
fn asan_vec_clear_extend_heap_no_leak_no_double_free() {
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 1000 {
        let mut v: Vec[String] = Vec.new();
        v.push("payload-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        v.push("payload-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        v.push("payload-cccccccccccccccccccccccccccccccc");
        v.clear();
        v.push("payload-dddddddddddddddddddddddddddddddd");
        let mut w: Vec[String] = Vec.new();
        w.push("payload-eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee");
        w.push("payload-ffffffffffffffffffffffffffffffff");
        v.extend(w);
        total = total + v[0].len() + v[1].len() + v[2].len();
        i = i + 1;
    }
    println(f"{total}");
}
"#,
        // Each payload is 40 bytes; after clear+push+extend `v` is [d, e, f]
        // → 120/iter, ×1000 = 120000.
        &["120000"],
        "vec_clear_extend_heap",
    );
}

#[test]
fn asan_vec_get_ref_t_over_heap_source_no_double_free() {
    // `Vec[String].get(i)` types as `Option[ref T]` (B-2026-06-07-5
    // Option[ref T] slice). The `Some(n)` binding is a by-value alias of
    // the element's heap buffer — cleanup is suppressed via
    // `scrutinee_is_borrow_call` so the binding does NOT register a second
    // free against a buffer the Vec still owns. Each element is forced
    // onto the heap (`"al" + "ice"` concat allocates), read through a
    // `ref String` param + `.len()`, in a loop (alloca reuse). The Vec
    // frees each String exactly once at scope exit; a regression where the
    // borrow binding re-frees the aliased buffer surfaces here as ASAN
    // double-free / heap-use-after-free.
    assert_clean_asan_run(
        r#"
fn shout(s: ref String) {
    println(s);
    println(s.len());
}

fn main() {
    let mut names: Vec[String] = Vec.new();
    names.push("al" + "ice");
    names.push("b" + "ob");
    names.push("ca" + "rol");
    let mut i = 0;
    while i < 3 {
        match names.get(i) {
            Some(n) => shout(n),
            None => println("none"),
        };
        i = i + 1;
    };
}
"#,
        &["alice", "5", "bob", "3", "carol", "5"],
        "vec_get_ref_t_over_heap_source_no_double_free",
    );
}

#[test]
fn asan_freshtemp_vec_get_no_double_free() {
    // General owned-temp tracking, slice 3b: `make_vec().get(i)` on a
    // FRESH-TEMP receiver in a loop. Each iteration builds a fresh Vec
    // temp whose heap buffer the `get` borrows read-only; codegen
    // materializes it into a `__vrecv_tmp` slot and frees the buffer once
    // per iteration. A regression that skipped the free leaks (caught by
    // LeakSanitizer on Linux CI); one that freed it twice (or freed a
    // buffer the scalar `Option[ref i64]` result still read) double-frees /
    // UAFs (caught here on macOS too). The loop forces alloca reuse so a
    // per-iteration leak accumulates.
    assert_clean_asan_run(
        r#"
fn ids() -> Vec[i64] {
    let mut v: Vec[i64] = Vec.new();
    v.push(100_i64);
    v.push(200_i64);
    v.push(300_i64);
    return v;
}

fn main() {
    let mut i = 0;
    while i < 3 {
        match ids().get(i) {
            Some(x) => println(x),
            None => println(0_i64),
        };
        i = i + 1;
    };
}
"#,
        &["100", "200", "300"],
        "freshtemp_vec_get_no_double_free",
    );
}

#[test]
fn asan_freshtemp_vec_first_last_contains_no_double_free() {
    // Slice 3b: the remaining element-type-aware read methods on fresh-temp
    // receivers — `first`/`last` (return `Option[ref i64]`) and `contains`
    // (returns `bool`). Each receiver Vec temp's buffer must be freed
    // exactly once after the borrow is read. The `make_vec` body forces a
    // real heap buffer (three `push`es past the inline cap).
    assert_clean_asan_run(
        r#"
fn nums() -> Vec[i64] {
    let mut v: Vec[i64] = Vec.new();
    v.push(7_i64);
    v.push(8_i64);
    v.push(9_i64);
    return v;
}

fn main() {
    match nums().first() {
        Some(x) => println(x),
        None => println(0_i64),
    };
    match nums().last() {
        Some(x) => println(x),
        None => println(0_i64),
    };
    println(nums().contains(8_i64));
    println(nums().contains(42_i64));
}
"#,
        &["7", "9", "true", "false"],
        "freshtemp_vec_first_last_contains_no_double_free",
    );
}

#[test]
fn asan_freshtemp_vec_nested_get_no_double_free() {
    // Slice 3e: `make_grid().get(i)` on a fresh-temp `Vec[Vec[i64]]` in a
    // loop. `get` returns `Option[ref Vec[i64]]` — a borrow into an inner row
    // *inside* the temp's outer buffer, which `__vrecv_tmp`'s `FreeVecBuffer`
    // frees at frame exit. Hazards mirror the `Vec[String]` case: (1) the
    // `Some(r)` arm binds a `ref Vec[i64]` that must NOT be independently
    // dropped (`scrutinee_is_borrow_call`), else it double-frees the inner
    // row the vec-struct recursion also frees (macOS ASAN); (2) the inner row
    // data buffers must be per-element freed before the outer buffer, else
    // they leak (Linux LSan). Each row has several elements so its buffer is
    // a real heap allocation; the loop accumulates.
    assert_clean_asan_run(
        r#"
fn make_grid() -> Vec[Vec[i64]] {
    let mut g: Vec[Vec[i64]] = Vec.new();
    let mut a: Vec[i64] = Vec.new();
    a.push(10_i64);
    a.push(11_i64);
    a.push(12_i64);
    a.push(13_i64);
    g.push(a);
    let mut b: Vec[i64] = Vec.new();
    b.push(20_i64);
    b.push(21_i64);
    b.push(22_i64);
    b.push(23_i64);
    g.push(b);
    return g;
}

fn main() {
    let mut i = 0;
    while i < 2 {
        match make_grid().get(i) {
            Some(r) => println(r[1]),
            None => println(0_i64),
        };
        i = i + 1;
    };
}
"#,
        &["11", "21"],
        "freshtemp_vec_nested_get_no_double_free",
    );
}

#[test]
fn asan_freshtemp_vec_struct_get_no_double_free() {
    // Slice 3f: `make_recs().get(i)` on a fresh-temp `Vec[Rec]` (Rec has a
    // String field) in a loop. `get` returns `Option[ref Rec]` borrowing an
    // element inside the temp's buffer, which `__vrecv_tmp`'s `FreeVecBuffer`
    // — now carrying the per-element `__karac_drop_struct_Rec` agg drop —
    // frees at frame exit. Hazards: (1) the `Some(r)` arm binds a `ref Rec`
    // that must NOT be independently dropped (`scrutinee_is_borrow_call`),
    // else it double-frees the element's String field the agg drop also frees
    // (macOS ASAN); (2) each element's String field must be freed by the agg
    // drop before the outer buffer, else they leak (Linux LSan). ≥36-byte
    // String fields defeat LSan short-string reachability; loop accumulates.
    assert_clean_asan_run(
        r#"
struct Rec { name: String, n: i64 }

fn make_recs() -> Vec[Rec] {
    let mut v: Vec[Rec] = Vec.new();
    v.push(Rec { name: "first record name field padded beyond thirty-six bytes", n: 10_i64 });
    v.push(Rec { name: "second record name field padded beyond thirty-six byte", n: 20_i64 });
    v.push(Rec { name: "third record name field padded beyond thirty-six bytess", n: 30_i64 });
    return v;
}

fn main() {
    let mut i = 0;
    while i < 3 {
        match make_recs().get(i) {
            Some(r) => println(r.n),
            None => println(0_i64),
        };
        i = i + 1;
    };
}
"#,
        &["10", "20", "30"],
        "freshtemp_vec_struct_get_no_double_free",
    );
}

#[test]
fn asan_freshtemp_vec_enum_get_no_double_free() {
    // Slice 3g: `make_toks().get(i)` on a fresh-temp `Vec[Tok]` where `Tok`
    // is a user enum with a heap-bearing variant (`Word { s: String }`), in
    // a loop. `get` returns `Option[ref Tok]` borrowing an element inside
    // the temp's buffer, which `__vrecv_tmp`'s `FreeVecBuffer` — now carrying
    // the per-element `__karac_drop_Tok` agg drop (slice 3f machinery, enum
    // routed via `emit_enum_drop_switch`) — frees at frame exit. Hazards:
    // (1) the `Some(t)` arm binds a `ref Tok` that must NOT be independently
    // dropped (`scrutinee_is_borrow_call`), else it double-frees the `Word`
    // variant's String the agg drop also frees (macOS ASAN); (2) each
    // element's payload String must be freed by the agg drop before the
    // outer buffer, else it leaks (Linux LSan). The borrow is further matched
    // on its variant and the payload String is read through it (`s.len()`),
    // aliasing the very buffer the agg drop frees. ≥36-byte payloads defeat
    // LSan short-string reachability; loop accumulates.
    assert_clean_asan_run(
        r#"
enum Tok { Word { s: String }, Num { n: i64 } }

fn make_toks() -> Vec[Tok] {
    let mut v: Vec[Tok] = Vec.new();
    v.push(Tok.Word { s: "first token payload string padded beyond thirty-six b" });
    v.push(Tok.Num { n: 20_i64 });
    v.push(Tok.Word { s: "third token payload string padded beyond thirty-six b" });
    return v;
}

fn main() {
    let mut i = 0;
    while i < 3 {
        match make_toks().get(i) {
            Some(t) => match t {
                Word { s } => println(s.len()),
                Num { n } => println(n),
            },
            None => println(0_i64),
        };
        i = i + 1;
    };
}
"#,
        &["53", "20", "53"],
        "freshtemp_vec_enum_get_no_double_free",
    );
}

#[test]
fn asan_vec_get_unwrap_struct_heap_field_no_leak_no_double_free() {
    // B-2026-07-20-9 memory leg: `let a = v.get(i).unwrap()` on a
    // `Vec[struct{String,..}]` now registers the binding as its struct type
    // (previously unlabeled/mislabeled), which makes the struct-drop
    // registration reachable — the drop must stay SUPPRESSED
    // (`is_borrowed_vec_get_unwrap_struct` borrow-elides the alias) so the
    // binding's shallow-aliased String is freed exactly once by the
    // container's per-element drain (a mis-registration would double-free;
    // a lost drain would leak — LSan/ASAN catch both). Loop so any
    // per-iteration imbalance accumulates.
    assert_clean_asan_run(
        r#"
struct Acct { name: String, txns: i64 }
fn main() {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 40 {
        let mut v: Vec[Acct] = Vec.new();
        v.push(Acct { name: "alice".to_string(), txns: 2 });
        v.push(Acct { name: "bobby".to_string(), txns: 3 });
        let a = v.get(0).unwrap();
        let b = v.last().unwrap();
        acc = acc + a.name.len() + a.txns + b.name.len() + b.txns;
        i = i + 1;
    }
    println(acc);
}
"#,
        &["600"],
        "vec_get_unwrap_struct_heap_field_no_leak_no_double_free",
    );
}

#[test]
fn asan_option_vec_let_unused_freed() {
    // `Option[Vec[i64]]` let-unused: the payload Vec's element buffer
    // must be freed by the scope-exit FreeInlineOptionPayload.
    assert_clean_asan_run(
        r#"
fn mk() -> Option[Vec[i64]] {
    let mut v: Vec[i64] = Vec.new();
    v.push(1); v.push(2); v.push(3);
    Some(v)
}
fn main() {
    let x = mk();
    println("done");
}
"#,
        &["done"],
        "option_vec_let_unused_freed",
    );
}

// ── Vec: owned heap buffer, scope-exit free ───────────────────
// Exercises `emit_scope_vec_cleanup` — the Vec's data pointer must be
// freed when `v` goes out of scope at the end of `main`.

#[test]
fn asan_vec_push_scope_exit_free() {
    assert_clean_asan_run(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1);
    v.push(2);
    v.push(3);
    println(v.len());
}
"#,
        &["3"],
        "vec_push_scope_exit_free",
    );
}

// ── Vec growth: multiple reallocations ────────────────────────
// Forces Vec growth (2x doubling, floor 4) so the scope-exit free has
// to release a larger buffer than the initial allocation. Catches
// bugs where growth replaces the data pointer without freeing the old
// buffer (leak) or where the grown pointer is freed twice.

#[test]
fn asan_vec_growth_multiple_reallocs() {
    assert_clean_asan_run_min_allocs(
        r#"
fn main() {
    let base: i64 = env.args().len();
    let mut v: Vec[i64] = Vec.new();
    let mut i = base;
    while i < base + 32 {
        v.push(i);
        i = i + 1;
    }
    println(f"{v.len()}:{v[0i64]}:{v[31i64]}");
}
"#,
        &["32:1:32"],
        "vec_growth_multiple_reallocs",
        // B-2026-08-04-17: a literal seed let this fold away entirely at -O2
        // (measured 0 program allocations). Opaque seed + a live read of the
        // buffer, with a floor so it cannot drift back.
        6,
    );
}

// ── Vec.with_capacity: scope-exit free of pre-allocated buffer ─
// `with_capacity(N)` malloc's a buffer up front; the scope-exit
// cleanup must free it once, even though `len == 0` and no push
// ever fired. Catches a regression where the free path keys off
// `len > 0` instead of `cap > 0` and leaks the entire buffer.

#[test]
fn asan_vec_with_capacity_unused_buffer_freed() {
    assert_clean_asan_run_min_allocs(
        r#"
fn main() {
    let base: i64 = env.args().len();
    let v: Vec[i64] = Vec.with_capacity(base + 15i64);
    println(f"{v.len()}");
}
"#,
        &["0"],
        "vec_with_capacity_unused_buffer_freed",
        // B-2026-08-04-17: a literal capacity let the whole allocation fold
        // away at -O2. An OPAQUE capacity survives even though the buffer is
        // never read, which is what this fixture needs — its subject is a
        // buffer that is allocated, never used, and must still be freed, so
        // it cannot force liveness with a read the way the others do.
        6,
    );
}

// B-2026-07-08-7: `Vec.filled(n, 0)` now allocates its buffer via the
// `calloc`-backed zeroed wrapper instead of malloc + fill loop. The buffer
// is still an ordinary heap allocation freed by the standard Vec drop —
// this pins that the calloc path leaks nothing and double-frees nothing
// (indexed writes into the zeroed buffer, then scope-exit free).
#[test]
fn asan_vec_filled_zero_calloc_buffer_freed() {
    assert_clean_asan_run(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.filled(8, 0);
    v[3] = 42;
    println(v.len());
    println(v[3]);
    println(v[0]);
}
"#,
        &["8", "42", "0"],
        "vec_filled_zero_calloc_buffer_freed",
    );
}

// `with_capacity(N)` + push exactly N times — every slot fits in
// the pre-allocated buffer, no realloc fires, scope-exit frees
// the single original allocation. Counterpart to
// `asan_vec_growth_multiple_reallocs` which verifies the grow
// path; this one verifies the no-grow path.

#[test]
fn asan_vec_with_capacity_push_exact_n_no_grow() {
    assert_clean_asan_run_min_allocs(
        r#"
fn main() {
    let base: i64 = env.args().len();
    let mut v: Vec[i64] = Vec.with_capacity(16);
    let mut i = base;
    while i < base + 16 {
        v.push(i);
        i = i + 1;
    }
    println(f"{v.len()}:{v[0i64]}:{v[15i64]}");
}
"#,
        &["16:1:16"],
        "vec_with_capacity_push_exact_n_no_grow",
        // B-2026-08-04-17: a literal seed let this fold away entirely at -O2
        // (measured 0 program allocations). Opaque seed + a live read of the
        // buffer, with a floor so it cannot drift back.
        6,
    );
}

// `with_capacity(N)` + push more than N times — forces a grow
// mid-flight, so both the original `with_capacity` malloc'd
// buffer AND the grown buffer need to be tracked correctly
// (old freed on grow, new freed on scope-exit). Catches a
// double-free if the grow path doesn't free the original
// before swapping the data pointer.
//
// B-2026-08-04-17: this fixture used a literal loop bound and read the
// result back through `v.len()` alone, and at `-O2` that made it assert
// nothing at all — the whole loop folded to the constant 16 and the binary
// performed ZERO heap allocations (measured: 1 alloc, the println buffer;
// 3 at `-O0`, which is what the comment above describes). A test for the
// Vec GROW path that never grows a Vec. The loop bound now comes from
// `env.args().len()` (a stable 1 — the binary is exec'd with no extra argv)
// so nothing folds, the assertion reads two ELEMENTS so the buffer is not a
// dead allocation LLVM can delete, and the allocation floor keeps both
// properties from silently regressing.
#[test]
fn asan_vec_with_capacity_push_past_n_grows_once() {
    assert_clean_asan_run_min_allocs(
        r#"
fn main() {
    let base: i64 = env.args().len();
    let mut v: Vec[i64] = Vec.with_capacity(4);
    let mut i = 0;
    while i < base + 15 {
        v.push(i);
        i = i + 1;
    }
    println(f"{v.len()}:{v[0i64]}:{v[15i64]}");
}
"#,
        &["16:0:15"],
        "vec_with_capacity_push_past_n_grows_once",
        // with_capacity(4) plus the 4->8->16 grows, the argv Vec and its
        // String, and the println buffer. The floor only has to sit above
        // the 3 an allocation-free run reports.
        6,
    );
}

#[test]
fn asan_vec_try_extend_alias_grow_and_clone_paths_clean() {
    // `Vec.try_extend` (B-2026-08-25-20) shares the
    // `try_extend_from_slice` lowering, so what needs proving under ASAN
    // is that the ALIAS reaches the same arm rather than some other one —
    // a mis-routed name would silently take the panicking `extend` path,
    // whose ownership handling of the grown buffer differs. Exercises
    // both element classes in one program: i64 takes the trivial memcpy
    // path, String takes the per-element clone path (the one that
    // double-frees if the source and destination both claim the elements).
    // Spelled with `match`, not `?` in `main`: the latter segfaults under
    // the JIT for an inline-scalar-payload error enum (B-2026-08-25-33).
    assert_clean_asan_run(
        r#"
fn main() {
    let src: Vec[i64] = Vec.filled(4, 5);
    let mut dst: Vec[i64] = Vec.with_capacity(2);
    dst.push(1);
    match dst.try_extend(src) {
        Ok(_) => println(dst.len()),
        Err(_) => println("err"),
    }
    let ssrc: Vec[String] = Vec.filled(3, "abc");
    let mut sdst: Vec[String] = Vec.with_capacity(1);
    sdst.push("z");
    match sdst.try_extend(ssrc) {
        Ok(_) => println(sdst.len()),
        Err(_) => println("err"),
    }
    println(sdst[3]);
    println(ssrc[0]);
}
"#,
        &["5", "4", "abc", "abc"],
        "vec_try_extend_alias_grow_and_clone_paths_clean",
    );
}

#[test]
fn asan_vec_nested_indexed_write_clean() {
    // `rows[r][c] = val` on Vec[Vec[T]] — nested-index store path
    // (codegen `compile_nested_vec_vec_index_store`). The leaf
    // store overwrites a slot inside `rows.data[r].data` (the
    // pre-filled inner buffer); scope-exit cleanup walks rows
    // recursively and frees the inner buffers cleanly. Catches
    // any aliasing bug where the GEP arithmetic stomps past the
    // inner Vec aggregate (write goes into `rows.data` itself,
    // not the inner buffer).
    assert_clean_asan_run(
        r#"
fn main() {
    let mut rows: Vec[Vec[i64]] = Vec.new();
    let r0: Vec[i64] = Vec.filled(4, 0);
    let r1: Vec[i64] = Vec.filled(4, 0);
    rows.push(r0);
    rows.push(r1);
    rows[0][2] = 100;
    rows[1][3] = 200;
    println(rows[0][2]);
    println(rows[1][3]);
}
"#,
        &["100", "200"],
        "vec_nested_indexed_write_clean",
    );
}

// ── Vec inside a nested scope ─────────────────────────────────
// Nested block scope — the inner Vec must be freed at the inner
// block's close, not deferred to the outer `main` exit. ASAN alone
// can't catch "freed at outer scope instead of inner" (both are
// eventual free, no leak); combined with a later allocation that
// reuses the same pool we at least smoke-test that nested cleanup
// doesn't double-free or leak.

#[test]
fn asan_vec_nested_scope() {
    assert_clean_asan_run(
        r#"
fn main() {
    {
        let mut inner: Vec[i64] = Vec.new();
        inner.push(1);
        inner.push(2);
        println(inner.len());
    }
    let mut outer: Vec[i64] = Vec.new();
    outer.push(99);
    println(outer.len());
}
"#,
        &["2", "1"],
        "vec_nested_scope",
    );
}

// ── Clone trait surface (canonical: phase-8-stdlib-floor.md
//    "Clone trait surface for collections") ───────────────────────────

#[test]
fn asan_vec_clone_independent_buffers() {
    // Both the source and the cloned Vec own heap buffers; both must
    // be freed exactly once on scope exit. ASAN catches double-free
    // (two frees of the same allocation) and leak (no free) — a
    // working clone keeps them independent.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1_i64);
    v.push(2_i64);
    v.push(3_i64);
    let w: Vec[i64] = v.clone();
    println(v.len());
    println(w.len());
}
"#,
        &["3", "3"],
        "vec_clone_independent_buffers",
    );
}

#[test]
fn asan_vec_clone_empty_no_leak() {
    // Empty Vec clone hits the fast path (no malloc); the resulting
    // Vec has cap=0 and its scope-exit free must be a no-op rather
    // than calling free(null) repeatedly or leaking a placeholder
    // allocation.
    assert_clean_asan_run(
        r#"
fn main() {
    let v: Vec[i64] = Vec.new();
    let w: Vec[i64] = v.clone();
    println(w.len());
}
"#,
        &["0"],
        "vec_clone_empty_no_leak",
    );
}

/// B-2026-08-08-5 — a cyclic graph whose back-edges live in a
/// `Vec[weak N]` is reclaimed completely.
///
/// This is design.md § Cycles' own worked example (`mut neighbors:
/// Vec[weak GraphNode]`, `b.neighbors.push(a); // bidirectional, no cycle
/// leak`), which did not compile until the container store learned the
/// downgrade. The strong-`Vec[N]` twin of this graph leaks its entire self
/// — 72 bytes per node — and is tracked as B-2026-08-08-4.
///
/// THE GRAPH IS BUILT IN A HELPER AND THE STACK IS THEN CHURNED, and that
/// is load-bearing rather than incidental. LSan scans the stack region
/// conservatively, so handles left in a dead frame read as reachable and
/// the leak goes UNREPORTED: the identical 2-node cycle passes this suite
/// when built inline in `main`, and reports 144 bytes the moment a
/// recursion overwrites the frame (B-2026-08-08-4). Without the churn this
/// test would be green against a completely broken implementation.
///
/// Three counts have to balance for this to pass, which is why it is one
/// fixture rather than three: the push must NOT take a strong count (it
/// took one at first, and the payloads freed while every control block
/// stayed — measured), the push MUST downgrade (or the slot holds a strong
/// pointer the container later weak-drops), and the container's scope-exit
/// drain must weak-drop each element (or the counts never reach zero).
#[test]
fn asan_vec_of_weak_back_edges_reclaims_a_cycle_no_leak() {
    assert_clean_asan_run_min_allocs(
        r#"
shared struct N { v: i64, mut ns: Vec[weak N] }

fn build(seed: i64) -> i64 {
    let a = N { v: seed, ns: Vec.new() };
    let b = N { v: seed + 1, ns: Vec.new() };
    let c = N { v: seed + 2, ns: Vec.new() };
    let d = N { v: seed + 3, ns: Vec.new() };
    a.ns.push(b); a.ns.push(d);
    b.ns.push(a); b.ns.push(c);
    c.ns.push(b); c.ns.push(d);
    d.ns.push(a); d.ns.push(c);
    return a.ns.len() + b.ns.len() + c.ns.len() + d.ns.len();
}

// Overwrites the dead frame the handles lived in, so LSan cannot mistake
// stack residue for reachability. See the doc comment.
fn churn(n: i64) -> i64 {
    if n <= 0 { return 0; }
    let pad: i64 = n * 3;
    return pad + churn(n - 1);
}

fn main() {
    let seed: i64 = env.args().len();
    let edges = build(seed);
    let noise = churn(2000);
    println(edges + noise - noise);
}
"#,
        // 8 back-edges across the 4-node cycle.
        &["8"],
        "vec_of_weak_back_edges_reclaims_a_cycle",
        // 4 node boxes + 4 element buffers.
        8,
    );
}

#[test]
fn asan_compound_enum_drop_invokes_vec_destructor() {
    // Vec[i64] payload — same `cap > 0 ? free(data)` cleanup
    // shape as String, exercised through the second drop-kind
    // entry in `field_drop_kinds`.
    assert_clean_asan_run(
        r#"
enum E { V(Vec[i64]) }
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1);
    v.push(2);
    v.push(3);
    let _e = V(v);
    println(1);
}
"#,
        &["1"],
        "compound_enum_drop_invokes_vec_destructor",
    );
}

#[test]
fn asan_vec_clone_repeat_stresses_scope_cleanup() {
    // Clone in a fresh scope across multiple loop iterations —
    // verifies the scope-exit free fires for each loop-local clone
    // so allocations don't accumulate. ASAN catches a missing free.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1_i64);
    v.push(2_i64);
    v.push(3_i64);
    let mut iter: i64 = 0;
    let mut total: i64 = 0;
    while iter < 5_i64 {
        let w: Vec[i64] = v.clone();
        total = total + w.len();
        iter = iter + 1_i64;
    }
    println(total);
}
"#,
        &["15"], // 5 clones × 3 elements each
        "vec_clone_repeat_stresses_scope_cleanup",
    );
}

// ── Match-arm Vec/String cleanup (2026-05-13) ─────────────────
// Per-arm scope frame + `track_vec_var` registration at
// `bind_pattern_values` together close the leak where a Vec/String
// extracted from an enum payload (`match opt { Some(v) => ... }`)
// wasn't tracked for scope-exit cleanup. ASAN catches the leak
// (Vec data buffer never freed) on the bound-then-discarded path
// and double-free on the move-out path (`Some(v) => v` returns the
// buffer; the per-arm move-aware suppression must zero the source's
// cap so the caller's cleanup is the unique owner).
//
// Canonical bfs_sieve-style pattern: `bucket.remove(k)` extracts a
// `Vec[i64]` from a Map, the match-arm binding receives it, the
// arm body iterates it via `into_iter` (which doesn't drop in
// karac today — see `compile_for` Vec/Slice arm), and the per-arm
// drain frees the data buffer at end of arm.

#[test]
fn asan_match_arm_vec_binding_freed_on_arm_exit() {
    assert_clean_asan_run(
        r#"
fn inner() -> i64 {
    let mut bucket: Map[i64, Vec[i64]] = Map.new();
    let mut i = 0i64;
    while i < 50 {
        bucket.entry(i).or_insert(Vec.new()).push(i);
        i = i + 1;
    }
    let mut k = 0i64;
    while k < 50 {
        match bucket.remove(k) {
            Some(indices) => {
                let _len = indices.len();
            },
            None => {},
        }
        k = k + 1;
    }
    0i64
}
fn main() {
    let mut s = 0i64;
    let mut iter = 0i64;
    while iter < 10 {
        s = s + inner();
        iter = iter + 1;
    }
    println(s);
}
"#,
        &["0"],
        "match_arm_vec_binding_freed_on_arm_exit",
    );
}

#[test]
fn asan_match_arm_vec_move_out_no_double_free() {
    // Canonical `Option<Vec>::unwrap_or_default` shape: the arm binding
    // is the arm's tail expression, so the value is moved into the
    // match's result. The per-arm move-aware suppression must zero
    // the source's `cap` before the per-arm drain so the caller's
    // own scope cleanup is the unique owner. ASAN catches the
    // double-free that the naive "always track" change introduced
    // and required the suppress mechanism to prevent.
    assert_clean_asan_run(
        r#"
fn make() -> Vec[i64] {
    let mut v: Vec[i64] = Vec.new();
    v.push(1i64);
    v.push(2i64);
    v
}
fn unwrap_or_default(opt: Option[Vec[i64]]) -> Vec[i64] {
    match opt {
        Some(v) => v,
        None => Vec.new(),
    }
}
fn main() {
    let v = unwrap_or_default(Some(make()));
    println(v[0]);
    let w = unwrap_or_default(None);
    println(w.len());
}
"#,
        &["1", "0"],
        "match_arm_vec_move_out_no_double_free",
    );
}

// ── Struct field drop synthesis (2026-05-14, slice γ) ────────
// `track_struct_var` + `emit_struct_drop_synthesis` emit a per-struct
// `__karac_drop_struct_<Name>` function that frees each heap-owning
// field's content on scope exit. Vec / String fields free their data
// buffer (`cap > 0` guard); Map / Set fields call
// `karac_map_free_with_drop_vec`. The move-aware
// `suppress_source_vec_cleanup_for_arg` is extended for struct
// identifiers — walks fields and zeros each Vec/String field's `cap`
// — so `return h` / `let g = h` / `consume(h)` don't double-free
// the inner buffer against the consumer's own tracking.

#[test]
fn asan_struct_with_vec_field_freed_on_scope_exit() {
    // Struct with Vec field — the canonical "compose a heap-owning
    // type into a value-type wrapper" pattern. Pre-fix the struct
    // had no scope-exit drop, so the inner Vec's data buffer leaked
    // when h went out of scope.
    assert_clean_asan_run_min_allocs(
        r#"
struct Holder { v: Vec[i64] }
fn build(k: i64) -> i64 {
    let mut inner: Vec[i64] = Vec.new();
    inner.push(k);
    inner.push(k + 1i64);
    inner.push(k + 2i64);
    let h: Holder = Holder { v: inner };
    return h.v[0i64] + h.v[2i64];
}
fn main() {
    let base: i64 = env.args().len();
    let mut s = 0i64;
    let mut i = base;
    while i < base + 10i64 {
        s = s + build(i);
        i = i + 1;
    }
    println(f"{s}");
}
"#,
        &["130"],
        "struct_with_vec_field_freed_on_scope_exit",
        // B-2026-08-04-17: a literal seed let this fold away entirely at -O2
        // (measured 0 program allocations). Opaque seed + a live read of the
        // buffer, with a floor so it cannot drift back.
        10,
    );
}

#[test]
fn asan_struct_with_vec_field_returned_no_double_free() {
    // `return h` where h has a Vec field — the move-aware suppress
    // in `suppress_source_vec_cleanup_for_arg` must walk h's fields
    // and zero each Vec field's cap so the function-end StructDrop
    // is a no-op for the returned value (the caller now owns it
    // and will run its own StructDrop). Pre-suppress, this
    // double-freed and SIGABRTed / hung on macOS allocator.
    assert_clean_asan_run(
        r#"
struct Holder { v: Vec[i64] }
fn build() -> Holder {
    let mut inner: Vec[i64] = Vec.new();
    inner.push(1i64);
    inner.push(2i64);
    let h: Holder = Holder { v: inner };
    h
}
fn first_elem(h: Holder) -> i64 {
    let inner = h.v;
    inner[0]
}
fn main() {
    let h = build();
    let f = first_elem(h);
    println(f);
}
"#,
        &["1"],
        "struct_with_vec_field_returned_no_double_free",
    );
}

#[test]
fn asan_owned_param_into_vec_literal_no_double_free() {
    // B-2026-07-18-38: an owned (caller-retains) String/Vec param used as a
    // Vec-literal element (`fn dup(x: String) -> Vec[String] { [x] }`) — the
    // by-value header ABI leaves the buffer's free with the caller, so the
    // returned Vec's element aliased the caller's arg and both freed it (a
    // double-free the move-out cap-zero couldn't cover: an owned param has
    // no callee-side FreeVecBuffer). `compile_vec_prefix_literal` now
    // deep-copies owned-param elements, like every other retaining consume
    // site. Covers a returned Vec, a non-escaping local Vec, and a Vec[Vec].
    assert_clean_asan_run(
        r#"
fn dup(x: String) -> Vec[String] { [x] }
fn local_use(x: String) -> i64 { let v = [x]; v.len() }
fn dupv(x: Vec[i64]) -> Vec[Vec[i64]] { [x] }
fn main() {
    println(dup("z".to_string())[0]);
    println(local_use("hi".to_string()));
    let vv = dupv([1, 2, 3]);
    println(vv[0][2]);
}
"#,
        &["z", "1", "3"],
        "owned_param_into_vec_literal",
    );
}

#[test]
fn asan_generic_struct_vec_field_move_out_no_double_free() {
    // B-2026-07-18-45: a generic struct with a whole-Vec-typed param field
    // returned (`get[T](b: Box[T]) -> T { b.v }` at T=Vec[i64]). The mono
    // entry-copy now deep-copies the Vec field (its element type is unified
    // from the arg's concrete instantiation), so the returned value owns an
    // independent buffer. Verify no double-free / leak for Vec[i64] and
    // Vec[String] fields, free-fn and method forms.
    assert_clean_asan_run(
        r#"
struct Box[T] { v: T }
fn get[T](b: Box[T]) -> T { b.v }
impl[T] Box[T] { fn take(self) -> T { self.v } }
fn main() {
    let a = Box { v: [1, 2, 3] };
    println(get(a).len());
    let m = Box { v: [4, 5] };
    println(m.take().len());
    let s = Box { v: ["x".to_string(), "y".to_string()] };
    println(get(s).len());
}
"#,
        &["3", "2", "2"],
        "generic_struct_vec_field_move_out",
    );
}

#[test]
fn asan_struct_with_multiple_vec_fields_freed_on_scope_exit() {
    // Two Vec fields in one struct — verifies the per-field loop
    // in `emit_struct_drop_synthesis` correctly emits cleanup for
    // both, not just the first.
    assert_clean_asan_run_min_allocs(
        r#"
struct Pair { a: Vec[i64], b: Vec[i64] }
fn build(k: i64) -> i64 {
    let mut x: Vec[i64] = Vec.new();
    x.push(k * 10i64);
    let mut y: Vec[i64] = Vec.new();
    y.push(k * 20i64);
    y.push(k * 30i64);
    let p: Pair = Pair { a: x, b: y };
    return p.a[0i64] + p.b[1i64];
}
fn main() {
    let base: i64 = env.args().len();
    let mut s = 0i64;
    let mut i = base;
    while i < base + 5i64 {
        s = s + build(i);
        i = i + 1;
    }
    println(f"{s}");
}
"#,
        &["600"],
        "struct_with_multiple_vec_fields_freed_on_scope_exit",
        // B-2026-08-04-17: a literal seed let this fold away entirely at -O2
        // (measured 0 program allocations). Opaque seed + a live read of the
        // buffer, with a floor so it cannot drift back.
        10,
    );
}

/// B-2026-08-13-11 — a FIELD-ROOTED Vec element read (`d.lines[i]`) consumed
/// into an argument is deep-cloned, so it no longer double-frees.
///
/// THE ROW'S NECESSARY-CONDITIONS TABLE WAS WRONG, and the correction is the
/// finding. It required, together: an enum with a heap payload, a function
/// taking it by value and returning a new one, TWO reads (one out of the Vec
/// into a returned variant, one back in and out again), and TWO chained calls
/// so the value completed a `Vec -> enum -> Vec -> enum` loop. None of that
/// is needed. One call, one read, no round trip:
///
///     fn take(d: mut ref Doc, at: i64) -> Cmd {
///         Cmd.Delete(at, d.lines[at as usize])
///     }
///
/// What the row's controls could not see, because every one of them used the
/// same container shape, is that the STRUCT FIELD is the trigger: the
/// identical enum round trip over a bare `Vec[String]` param is clean, and
/// so is the field-rooted read when it is BOUND first (`let g = d.lines[i]`)
/// rather than consumed directly. The enum is not load-bearing — it is just
/// one of the ~25 argument positions that route through
/// `maybe_defensive_copy_param_arg`, whose gate asked for an `Identifier`
/// container and got a `FieldAccess`.
///
/// The legs here are that corrected axis: the row's own 14-line repro, the
/// one-call reduction, an owning `Vec.push` destination, a `ref self` method
/// (`self.lines[0]`, the shape the self-hosted parser is full of), a
/// 200-iteration loop that reports blocks if the clone's cleanup stops
/// firing, and a SCALAR-element control that must NOT clone.
#[test]
fn asan_field_rooted_vec_elem_read_into_arg_cloned() {
    assert_clean_asan_run(
        r#"
enum Cmd { Insert(i64, String), Delete(i64, String) }
struct Doc { lines: Vec[String] }
struct Nums { xs: Vec[i64] }
enum Got { N(i64) }
impl Doc { fn first(ref self) -> String { let g = self.lines[0].clone(); g } }
fn apply(d: mut ref Doc, c: Cmd) -> Cmd {
    match c {
        Insert(at, text) => { d.lines.insert(at as usize, text); Cmd.Delete(at, d.lines[at as usize].clone()) }
        Delete(at, _text) => { let gone = d.lines[at as usize].clone(); d.lines.remove(at as usize); Cmd.Insert(at, gone) }
    }
}
fn one(d: mut ref Doc, at: i64) -> Cmd { Cmd.Delete(at, d.lines[at as usize].clone()) }
fn scalar(n: ref Nums) -> Got { Got.N(n.xs[0]) }
fn main() {
    let k = env.args().len() as i64;
    let mut l: Vec[String] = Vec.new();
    let mut a = String.new(); a.push_str("alpha"); a.push_str(k.to_string());
    let mut b = String.new(); b.push_str("beta"); b.push_str(k.to_string());
    l.push(a); l.push(b);
    let mut d = Doc { lines: l };
    let inv = apply(mut d, Cmd.Delete(1, "beta"));
    let i2 = apply(mut d, inv);
    let mut acc = d.lines.len() as i64;
    match i2 { Insert(i, t) => { acc = acc + t.len(); } Delete(i, t) => { acc = acc + t.len(); } }
    let c = one(mut d, 0);
    match c { Insert(i, t) => { acc = acc + t.len(); } Delete(i, t) => { acc = acc + t.len(); } }
    let mut out: Vec[String] = Vec.new();
    out.push(d.lines[0]);
    acc = acc + out[0].len();
    acc = acc + d.first().len();
    let mut i = 0i64;
    while i < 200 {
        let cc = one(mut d, 0);
        match cc { Insert(x, t) => { acc = acc + t.len(); } Delete(x, t) => { acc = acc + t.len(); } }
        i = i + 1;
    }
    let mut v: Vec[i64] = Vec.new();
    v.push(7);
    let n = Nums { xs: v };
    match scalar(n) { N(x) => { acc = acc + x; } }
    println(acc > 0);
}
"#,
        &["true"],
        "field_rooted_vec_elem_read_into_arg_cloned",
    );
}

/// B-2026-08-13-20 — a cap-zeroed (disarmed) Vec must not walk its
/// elements, so the buffer's real owner frees each element exactly once.
///
/// The E2E twin asserts the program does not crash. This asserts the thing
/// a crash-free run still cannot: that the counts are right in BOTH
/// directions. The unfixed compiler double-freed — the emitted
/// `karac_drop_Vec_<E>` walked `0..len` on a slot whose cap said it owned
/// nothing, over a buffer the new owner had already released. Guarding the
/// walk could equally have gone wrong the other way, skipping a walk that
/// was owed, and LeakSanitizer is the only thing that would say so.
///
/// `Vec[String]` throughout, because a `Vec[i64]` element drop is a no-op
/// and the whole defect is invisible on it — that is exactly why this went
/// unnoticed. The realloc loop makes the freed-then-rewalked buffer MOVE,
/// so the second walk reads memory the allocator has really handed back.
#[test]
fn asan_disarmed_vec_skips_its_element_walk() {
    assert_clean_asan_run(
        r#"
struct A { mut lines: Vec[String] }
fn main() {
    let k = env.args().len() as i64;
    let mut v: Vec[String] = Vec.new();
    let mut i = 0i64;
    while i < 600 {
        let mut q = String.new(); q.push_str("alpha"); q.push_str(k.to_string());
        v.push(q);
        i = i + 1;
    }
    let t: (Vec[String], i64) = (v, 2);
    let mut r = t.0;
    let mut j = 0i64;
    while j < 600 {
        let mut q = String.new(); q.push_str("beta"); q.push_str(k.to_string());
        r.push(q);
        j = j + 1;
    }
    let mut acc = r.len() as i64;
    acc = acc + r[0].len();

    let mut v2: Vec[String] = Vec.new();
    let mut m = 0i64;
    while m < 300 {
        let mut q = String.new(); q.push_str("gamma"); q.push_str(k.to_string());
        v2.push(q);
        m = m + 1;
    }
    let a = A { lines: v2 };
    let b = a;
    acc = acc + b.lines.len() as i64;
    acc = acc + b.lines[0].len();
    println(acc);
}
"#,
        // 1200 + 6 ("alpha1") + 300 + 6 ("gamma1") = 1512.
        // `k` is a stable 1 (the binary runs with no args); it exists only
        // to defeat literal folding, since a string LITERAL is static with
        // `cap == 0` — which in this family is the very marker under test.
        &["1512"],
        "disarmed_vec_skips_its_element_walk",
    );
}

/// B-2026-08-13-4 — a NESTED heap field read off a Vec ELEMENT
/// (`ds[0].inner.word`) is deep-cloned at the read, so it no longer
/// double-frees.
///
/// The one-level sibling is B-2026-08-12-27: `ps[0].word` loads a
/// `{ptr,len,cap}` ALIAS of the container's buffer, so every owning
/// destination shared one pointer with the element and both freed it. Its
/// clone is gated on the field sitting DIRECTLY on the element struct, so
/// a field of a field slipped through with the identical alias.
///
/// CLONING RATHER THAN MOVING is inherited from that row and is the whole
/// reason this is not fixed by cap-zeroing the element: the semantics is a
/// COPY. `karac check` accepts reading `ds[0].inner.word` after binding it
/// and the interpreter still has the value, so a move model would trade
/// this double free for a silent use-after-free — the alternative -27
/// measured and rejected. Every leg here reads the element again AFTER
/// consuming the field, which is what pins that.
///
/// THE LEGS ARE THE CONSUMING DESTINATIONS the alias escaped into, each
/// reaching the clone by a different route: a `let` binding, `Vec.push`, a
/// call argument, and a struct-literal field. `xs` carries the read TWO
/// hops deep (`xs[0].mid.inner.word`) since the walk is a loop, and the
/// `ps` leg is the one-level form — already correct, kept here so a
/// refactor cannot fix the nested case and lose its sibling.
#[test]
fn asan_nested_vec_elem_field_read_cloned_not_aliased() {
    assert_clean_asan_run(
        r#"
struct Pair { word: String, n: i64 }
struct Deep { inner: Pair, tag: i64 }
struct Outer { mid: Deep, label: String }
fn sink(s: String) -> String { s + "!" }
fn main() {
    let k = env.args().len() as i64;
    let mut i = 0i64;
    let mut acc = 0i64;
    while i < 20 {
        let mut ds: Vec[Deep] = Vec.new();
        ds.push(Deep { inner: Pair { word: f"a{k + i}", n: 1 }, tag: 2 });
        let bound = ds[0].inner.word;
        acc = acc + bound.len();
        let mut out: Vec[String] = Vec.new();
        out.push(ds[0].inner.word);
        acc = acc + out[0].len();
        let passed = sink(ds[0].inner.word);
        acc = acc + passed.len();
        let held = Pair { word: ds[0].inner.word, n: 9 };
        acc = acc + held.word.len() + held.n;
        let still = ds[0].inner.word;
        acc = acc + still.len();
        let mut xs: Vec[Outer] = Vec.new();
        xs.push(Outer {
            mid: Deep { inner: Pair { word: f"b{k + i}", n: 3 }, tag: 4 },
            label: f"L{k + i}",
        });
        let deep = xs[0].mid.inner.word;
        acc = acc + deep.len();
        let mut ps: Vec[Pair] = Vec.new();
        ps.push(Pair { word: f"c{k + i}", n: 5 });
        let flat = ps[0].word;
        acc = acc + flat.len() + ps[0].word.len();
        i = i + 1;
    }
    println(acc > 0);
}
"#,
        &["true"],
        "nested_vec_elem_field_read_cloned_not_aliased",
    );
}

/// B-2026-08-12-27 — a heap FIELD read out of a Vec element
/// (`ps[0].word`) used to hand back a SHALLOW ALIAS of the container's
/// buffer. Consumed into any owning destination, the destination and the
/// container both freed it. The read is now deep-cloned, so each owns its
/// own buffer — the rule the WHOLE-element read has always followed.
///
/// EIGHT DESTINATIONS, which is why this is one program rather than a
/// smoke test: the filing named only the struct literal, and every one of
/// these aborted with `free(): double free detected in tcache 2` on a
/// default `karac build` while the interpreter printed the right answer.
///
///   struct-literal field   `Pair { word: ps[0].word, .. }`
///   `Vec.push`             `ws.push(ps[0].word)`
///   field assign           `o.word = ps[0].word`
///   index assign           `ws[0] = ps[0].word`
///   map insert             `m.insert(ps[0].word, 5)`
///   return                 `return ps[0].word`
///   tuple construction     `(ps[0].word, 1)`
///   enum payload           `Wrap.Has(ps[0].word)`
///
/// THE LAST LINE OF THE LOOP IS THE OTHER HALF OF THE ASSERTION: after all
/// eight consumptions the container's own field is read again. Cloning is
/// only correct if the ELEMENT still owns its buffer, so a fix that
/// disowned the source instead — the tempting one, and the one the `let`
/// site used to do — would leave this read dangling rather than fix it.
///
/// It also guards the leak direction. The clone carries its own scope
/// cleanup so a NON-consuming read cannot leak, and a consuming
/// destination takes it over by zeroing the clone's cap; if that takeover
/// stopped firing, every leg here would double-free, and if it fired for a
/// non-consuming read, LSan would report the orphan.
#[test]
fn asan_vec_elem_heap_field_read_freed_once() {
    assert_clean_asan_run(
        r#"
struct Pair { word: String, n: i64 }
struct Box3 { word: String }
enum Wrap { Has(String), Non }
fn ret(ps: Vec[Pair]) -> String { return ps[0].word; }
fn main() {
    let k = env.args().len() as i64;
    let mut i = 0i64;
    let mut acc = 0i64;
    while i < 20 {
        let mut ps: Vec[Pair] = Vec.new();
        ps.push(Pair { word: f"src{k + i}", n: 1 });
        // 1. struct-literal field
        let lit = Pair { word: ps[0].word, n: 9 };
        acc = acc + lit.word.len();
        // 2. Vec.push
        let mut ws: Vec[String] = Vec.new();
        ws.push(ps[0].word);
        acc = acc + ws[0].len();
        // 3. field assign
        let mut o = Box3 { word: f"old{k + i}" };
        o.word = ps[0].word;
        acc = acc + o.word.len();
        // 4. index assign
        let mut zs: Vec[String] = Vec.new();
        zs.push(f"pad{k + i}");
        zs[0] = ps[0].word;
        acc = acc + zs[0].len();
        // 5. map insert
        let mut m: Map[String, i64] = Map.new();
        m.insert(ps[0].word, 5);
        acc = acc + 1;
        // 6. return out of a callee
        acc = acc + ret(ps).len();
        // 7. tuple construction
        let t = (ps[0].word, 1);
        acc = acc + t.0.len();
        // 8. enum payload
        let w = Wrap.Has(ps[0].word);
        acc = acc + match w { Wrap.Has(s) => s.len(), Wrap.Non => 0 };
        // The container's own field must still read correctly afterwards.
        acc = acc + ps[0].word.len();
        i = i + 1;
    }
    println(acc > 0);
}
"#,
        &["true"],
        "vec_elem_heap_field_read_freed_once",
    );
}

/// B-2026-08-02-22 — `Vec[(Res, i64)]`: the container's per-element
/// machinery had no tuple arm, so nothing freed the heap reachable
/// through a tuple element and every element's String buffer leaked.
/// The element drop now routes through the same TypeExpr-driven tuple
/// walk the struct-field NestedTuple arm uses. Covers both the INLINE
/// element (no move at all — the minimal repro that refuted this row's
/// first diagnosis) and the NAMED source, whose own-body action used to
/// stay armed and fire over the moved-from slot.
#[test]
fn asan_discarded_vec_removal_no_leak() {
    // B-2026-08-03-2 (class 2) — the LEAK half. A discarded `v.remove(i);`
    // orphaned the element's heap entirely (32 direct + 3 indirect in the
    // minimal repro) because nothing owned the by-value element the builtin
    // handed back. Fires the body AND frees now; the bound form is the
    // control that always did.
    assert_clean_asan_run(
        r#"
struct Res { id: i64, name: String }
impl Drop for Res {
    fn drop(mut ref self) { println(f"drop {self.id} {self.name}") }
}
fn main() {
    println("remove:");
    {
        let mut v: Vec[Res] = Vec.new();
        v.push(Res { id: 1, name: f"rr{1}" });
        v.remove(0);
        println(v.len());
    }
    println("swapremove:");
    {
        let mut w: Vec[Res] = Vec.new();
        w.push(Res { id: 2, name: f"ss{2}" });
        w.swap_remove(0);
        println(w.len());
    }
    println("end");
}
"#,
        &[
            "remove:",
            "drop 1 rr1",
            "0",
            "swapremove:",
            "drop 2 ss2",
            "0",
            "end",
        ],
        "discarded_vec_removal_no_leak",
    );
}

#[test]
fn asan_vec_of_tuple_element_heap_freed() {
    assert_clean_asan_run(
        r#"
struct Res { id: i64, name: String }
impl Drop for Res {
    fn drop(mut ref self) { println(f"drop {self.id} {self.name}") }
}
fn main() {
    println("inline:");
    {
        let mut t: Vec[(Res, i64)] = Vec.new();
        t.push((Res { id: 9, name: f"in{9}" }, 8));
        println(t.len());
    }
    println("named:");
    {
        let mut u: Vec[(Res, i64)] = Vec.new();
        let r = Res { id: 4, name: f"tt{4}" };
        u.push((r, 8));
        println(u.len());
    }
    println("end");
}
"#,
        &[
            "inline:",
            "1",
            "drop 9 in9",
            "named:",
            "1",
            "drop 4 tt4",
            "end",
        ],
        "vec_of_tuple_element_heap_freed",
    );
}

/// B-2026-08-01-32 — Vec.filled with an all-zero aggregate fill value
/// (Vec.new() / String.new()) takes the calloc fast path instead of a
/// per-slot clone loop, so the memory pairing needs pinning: pushes
/// into individual calloc'd slots stay independent, the pushed
/// element heap frees exactly once at owner death, and the untouched
/// all-zero slots drop as no-ops — no leak, no double-free.
#[test]
fn asan_vec_filled_empty_aggregate_fill_and_drop() {
    assert_clean_asan_run(
        r#"
fn main() {
    let mut grid: Vec[Vec[i64]] = Vec.filled(4, Vec.new());
    grid[0].push(11);
    grid[0].push(12);
    grid[2].push(33);
    println(f"g0 {grid[0].len()} {grid[0][0]} {grid[0][1]}");
    println(f"g1 {grid[1].len()} g2 {grid[2].len()} {grid[2][0]} g3 {grid[3].len()}");
    let mut names: Vec[String] = Vec.filled(3, String.new());
    names[1] = f"nm{7}";
    println(f"s {names.len()} {names[0].len()} {names[1]}");
    println("end");
}
"#,
        &["g0 2 11 12", "g1 0 g2 1 33 g3 0", "s 3 0 nm7", "end"],
        "vec_filled_empty_aggregate_fill_and_drop",
    );
}

/// B-2026-08-01-29 — duplicate-key inserts of a for-loop STRUCT element
/// must not orphan the staged deep copy: the exists path keeps the
/// bucket's stored key and never adopts the staged bytes, and the
/// pre-existing no-adopt frees are vec-struct-gated (String/Vec keys
/// only). The staged copy is now reclaimed with the memory-only
/// `__karac_drop_struct_<T>` on the exists/OOM branches of the Set and
/// Map insert arms. LSan (Linux) is the lane that catches the pre-fix
/// leak (one field-buffer set per duplicate insert).
#[test]
fn asan_dup_key_for_loop_elem_insert_no_leak() {
    assert_clean_asan_run(
        r#"
#[derive(Hash, Eq, Ord)]
struct P { a: i64, s: String }
fn main() {
    let mut ps: Vec[P] = Vec.new();
    ps.push(P { a: 1, s: f"x{1}" });
    ps.push(P { a: 1, s: f"x{1}" });
    let mut set: Set[P] = Set.new();
    for p in ps {
        set.insert(p);
    }
    println(set.len());
    let mut qs: Vec[P] = Vec.new();
    qs.push(P { a: 1, s: f"x{1}" });
    qs.push(P { a: 1, s: f"x{1}" });
    let mut m: Map[P, i64] = Map.new();
    let mut i = 0;
    for p in qs {
        let _ = m.insert(p, i);
        i = i + 1;
    }
    println(m.len());
    println("end");
}
"#,
        &["1", "1", "end"],
        "dup_key_for_loop_elem_insert_no_leak",
    );
}

/// B-2026-08-01-24 — a heap-owning `for`-loop struct/enum ELEMENT
/// pushed whole into another container (`for h in hs { out.push(h) }`)
/// double-freed its heap payload pre-fix: the loop binding is a shallow
/// bit-copy of the source container's element slot, the push's
/// move-suppression zeroed only the binding's ALLOCA, and the source
/// vec's per-element drain plus the destination's element drop both
/// freed the same buffers (abort exit 134). The fix deep-copies the
/// stored element's heap in place at the destination slot (struct
/// fields via `deep_copy_struct_heap_fields_in_place`, enum live-variant
/// payload via `deep_copy_enum_heap_payload_in_place`) — ASAN guards
/// the double-free half, LSan (Linux CI) the no-leak half of the copy.
#[test]
fn asan_for_loop_elem_push_move_freed() {
    assert_clean_asan_run(
        r#"
struct Header { name: String, value: String }
enum Item {
    Named(String),
    Plain(i64),
}
fn move_all(headers: Vec[Header]) -> Vec[Header] {
    let mut out: Vec[Header] = Vec.new();
    for h in headers {
        out.push(h);
    }
    out
}
fn main() {
    let stamp = "20150830T123600Z";
    let mut hs: Vec[Header] = Vec.new();
    hs.push(Header { name: "X-Amz-Date", value: stamp.clone() });
    let moved = move_all(hs);
    for h in moved { println(h.value); }
    let mut src: Vec[Item] = Vec.new();
    src.push(Item.Named(f"x{1}"));
    src.push(Item.Plain(7));
    let mut out2: Vec[Item] = Vec.new();
    for it in src {
        out2.push(it);
    }
    for it in out2 {
        match it {
            Item.Named(s) => println(s),
            Item.Plain(n) => println(f"{n}"),
        }
    }
    println("end");
}
"#,
        &["20150830T123600Z", "x1", "7", "end"],
        "for_loop_elem_push_move_freed",
    );
}

#[test]
fn asan_ref_arg_nested_vec_elem_freed() {
    // Slice 2 part B: a fresh `Vec[String]` rvalue passed to a `ref
    // Vec[String]` param. The prior `ref_rvalue_arg` path freed only the
    // outer buffer (`track_vec_var(temp, None)`), leaking each String
    // element's `{ptr,len,cap}` data. `queue_ref_rvalue_arg_cleanup` now
    // recovers the element type from `owned_temp_drops`, so the recursive
    // `FreeVecBuffer` walk frees the inner String buffers too. 8-iteration
    // loop: Linux `detect_leaks=1` is the leak oracle for the element
    // closure; macOS catches any double-free of the outer buffer.
    assert_clean_asan_run(
        r#"
fn make_vv() -> Vec[String] {
    let mut v: Vec[String] = Vec.new();
    v.push("alpha");
    v.push("beta");
    return v;
}

fn show(v: ref Vec[String]) {
    println(v.len());
}

fn main() {
    let mut i = 0;
    while i < 8 {
        show(make_vv());
        i = i + 1;
    }
}
"#,
        &["2", "2", "2", "2", "2", "2", "2", "2"],
        "ref_arg_nested_vec_elem_freed",
    );
}

#[test]
fn asan_method_chain_intermediate_vec_freed() {
    // Slice 3: `make_vec().len()` — a fresh-owned Vec temp is the receiver
    // of `len` (borrow). The receiver's heap buffer must drop after the
    // statement instead of leaking. 8-iteration loop: each iteration's
    // receiver temp is freed exactly once — Linux `detect_leaks=1` is the
    // leak oracle, macOS catches any double-free (e.g. a per-site reused
    // temp slot freed against a stale buffer, or compounding).
    assert_clean_asan_run_min_allocs(
        r#"
fn make_vec(k: i64) -> Vec[i64] {
    let mut v: Vec[i64] = Vec.new();
    v.push(k);
    v.push(k + 1_i64);
    v.push(k + 2_i64);
    return v;
}

fn main() {
    let base: i64 = env.args().len();
    let mut total = 0i64;
    let mut i = base;
    while i < base + 8i64 {
        total = total + make_vec(i).len();
        i = i + 1;
    }
    println(f"{total}");
}
"#,
        &["24"],
        "method_chain_intermediate_vec_freed",
        // B-2026-08-04-17: a literal seed let this fold away entirely at -O2
        // (measured 0 program allocations). The subject here is `.len()` on a
        // fresh temp, so the fixture cannot read the buffer's contents without
        // changing the shape under test — instead the temp is built from an
        // opaque argument, which is enough to keep the allocation live.
        10,
    );
}

// ── general owned-temp tracking, slice 1 (phase-6 line 489/497) ──
//
// docs/spikes/general-owned-temp-tracking.md slice 1: a fresh-owned
// Vec/String produced in statement-discard position (`make_vec();`) has
// no binding to drop it; the owned-temp chokepoint materializes it into an
// `__owned_tmp` slot and frees it at the `;`. On Linux this is a leak gate
// (LeakSanitizer flags the unfreed buffer); on macOS (no LSan) it is a
// *double-free* gate — if the chokepoint wrongly freed a buffer that some
// other cleanup also owns, the repeated discard in a loop faults under
// ASAN's quarantine. The `make()` call in a loop amplifies any
// per-iteration imbalance into a deterministic crash, mirroring
// `asan_ref_arg_repeated_calls_no_compound_leak`.
#[test]
fn asan_discarded_vec_temp_freed_no_double_free() {
    assert_clean_asan_run(
        r#"
fn make_vec() -> Vec[i64] {
    let mut v: Vec[i64] = Vec.new();
    v.push(1_i64);
    v.push(2_i64);
    return v;
}

fn main() {
    let mut i = 0;
    while i < 8 {
        make_vec();
        i = i + 1;
    }
    println("done");
}
"#,
        &["done"],
        "discarded_vec_temp_freed_no_double_free",
    );
}

#[test]
fn asan_vec_of_vec_of_struct_scope_exit_drop_no_leak() {
    // Slice 3o: a `Vec[Vec[Rec]]` where `Rec` owns a heap `String` field,
    // dropped at scope exit, looped. 3n's recursive drop handled collection
    // inners; 3o threads the struct-field drop (`__karac_drop_struct_Rec`)
    // through the recursive `karac_drop_Vec_Rec` so each element's `name`
    // String frees. Leak (innermost field Strings) is the Linux-LSan gate;
    // a double-free (aliased element vs its clone) shows on macOS ASAN.
    // ≥36-byte field strings defeat LSan short-string reachability.
    assert_clean_asan_run(
        r#"
struct Rec { name: String, n: i64 }
fn build(n: i64) -> Vec[Vec[Rec]] {
    let mut outer: Vec[Vec[Rec]] = Vec.new();
    let mut a: Vec[Rec] = Vec.new();
    a.push(Rec { name: f"alpha string padded out beyond thirty-six bytes {n}", n: 1_i64 });
    a.push(Rec { name: f"beta string padded out beyond thirty-six bytes {n}", n: 2_i64 });
    outer.push(a);
    return outer;
}
fn main() {
    let mut i = 0;
    while i < 3 {
        let vv = build(i);
        println(vv.len());
        i = i + 1;
    };
}
"#,
        &["1", "1", "1"],
        "vec_of_vec_of_struct_scope_exit_drop_no_leak",
    );
}

#[test]
fn asan_vec_of_vec_of_enum_scope_exit_drop_no_leak() {
    // Slice 3o enum sibling: a `Vec[Vec[Tok]]` where `Tok` has a heap variant
    // (`Word(String)`). The enum drop-switch (`__karac_drop_Tok`) threaded
    // through the recursive `karac_drop_Vec_Tok` frees each live `Word`
    // payload String. Same leak/double-free hazards as the struct case.
    assert_clean_asan_run(
        r#"
enum Tok { Word(String), Num(i64) }
fn build(n: i64) -> Vec[Vec[Tok]] {
    let mut outer: Vec[Vec[Tok]] = Vec.new();
    let mut a: Vec[Tok] = Vec.new();
    a.push(Tok.Word(f"alpha string padded out beyond thirty-six bytes {n}"));
    a.push(Tok.Num(2_i64));
    outer.push(a);
    return outer;
}
fn main() {
    let mut i = 0;
    while i < 3 {
        let vv = build(i);
        println(vv.len());
        i = i + 1;
    };
}
"#,
        &["1", "1", "1"],
        "vec_of_vec_of_enum_scope_exit_drop_no_leak",
    );
}

#[test]
fn asan_vec_of_vec_of_shared_struct_scope_exit_drop_no_leak() {
    // Slice 3o shared sibling: a `Vec[Vec[Node]]` where `Node` is a `shared
    // struct` owning a `String`. A shared element's per-element drop is an
    // RC-dec (`__karac_vec_elem_rc_dec_Node`), threaded through the recursive
    // `karac_drop_Vec_Node`; at rc→0 the box (and its String) frees. A missing
    // dec leaks the whole box (Linux LSan); a double-dec frees the box twice
    // (macOS ASAN). The `te_recursive_drop_fully_supported` gate admits shared
    // types via `shared_types`.
    assert_clean_asan_run(
        r#"
shared struct Node { label: String }
fn build(n: i64) -> Vec[Vec[Node]] {
    let mut outer: Vec[Vec[Node]] = Vec.new();
    let mut a: Vec[Node] = Vec.new();
    a.push(Node { label: f"alpha string padded out beyond thirty-six bytes {n}" });
    outer.push(a);
    return outer;
}
fn main() {
    let mut i = 0;
    while i < 3 {
        let vv = build(i);
        println(vv.len());
        i = i + 1;
    };
}
"#,
        &["1", "1", "1"],
        "vec_of_vec_of_shared_struct_scope_exit_drop_no_leak",
    );
}

#[test]
fn asan_vec_push_option_binding_no_double_free() {
    // Slice 3p double-free regression (caught by this exact probe during
    // development, exit 133): `let o = Some(f"..."); v.push(o)` — the push
    // bit-copies the option aggregate into the vec, whose per-element
    // `karac_drop_Option_String` now frees the payload; the source binding
    // `o`'s `FreeInlineOptionPayload` would free the SAME buffer. The push
    // family (push/push_back/try_push/push_front/try_push_front) disarms the
    // source via `suppress_inline_option_payload_cleanup_for_moved_arg`
    // (cap-zeroes option field 3), making the container the unique owner.
    // macOS ASAN catches the double-free; Linux LSan the leak if the element
    // drop went missing instead.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i = 0;
    while i < 4 {
        let mut v: Vec[Option[String]] = Vec.new();
        let o = Some(f"a payload string padded beyond thirty-six bytes {i}");
        v.push(o);
        println(v.len());
        i = i + 1;
    };
}
"#,
        &["1", "1", "1", "1"],
        "vec_push_option_binding_no_double_free",
    );
}

#[test]
fn asan_vec_of_option_vec_scope_exit_drop_no_leak() {
    // Slice 3p Vec-payload sibling: `Vec[Option[Vec[i64]]]`. The payload
    // drop recurses through `karac_drop_Vec_i64` (the payload's own family
    // fn) — the inner Vec's data buffer frees once per `Some` element.
    assert_clean_asan_run(
        r#"
fn build(n: i64) -> Vec[Option[Vec[i64]]] {
    let mut v: Vec[Option[Vec[i64]]] = Vec.new();
    let mut inner: Vec[i64] = Vec.new();
    inner.push(n);
    inner.push(n + 1);
    v.push(Some(inner));
    v.push(None);
    return v;
}
fn main() {
    let mut i = 0;
    while i < 3 {
        let v = build(i);
        println(v.len());
        i = i + 1;
    };
}
"#,
        &["2", "2", "2"],
        "vec_of_option_vec_scope_exit_drop_no_leak",
    );
}

#[test]
fn asan_vec_push_result_binding_no_double_free() {
    // Slice 3q: `let r = Ok(f"..."); v.push(r)` — the push family disarms
    // the source binding's `FreeInlineResultPayload` (cap-zero, the Result
    // sibling of the Option moved-arg suppression) so the container's
    // element drop is the unique owner.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i = 0;
    while i < 4 {
        let mut v: Vec[Result[String, i64]] = Vec.new();
        let r: Result[String, i64] = Ok(f"a payload padded out beyond thirty-six bytes {i}");
        v.push(r);
        println(v.len());
        i = i + 1;
    };
}
"#,
        &["1", "1", "1", "1"],
        "vec_push_result_binding_no_double_free",
    );
}

#[test]
fn asan_for_match_vec_option_element_no_double_free() {
    // Slice 3q regression pin — this exact shape SIGTRAP'd (exit 133) after
    // slice 3p armed the `Vec[Option[String]]` element drop: `for o in v`
    // copies the element into the loop binding, and `match o { Some(s) => …
    // }` bound the payload OUT of the copy, registering its own free — a
    // double-free against the container's element drop. The loop binding is
    // now marked in `for_loop_borrow_vars` (Option/Result-with-heap-payload
    // elements) and `scrutinee_is_borrowed_binding` treats it as a borrow,
    // so the arm binding aliases and the container's element drop is the
    // single owner. Output must also match the interpreter (188).
    assert_clean_asan_run(
        r#"
fn build(n: i64) -> Vec[Option[String]] {
    let mut v: Vec[Option[String]] = Vec.new();
    v.push(Some(f"alpha payload padded beyond thirty-six bytes {n}"));
    v.push(None);
    return v;
}
fn main() {
    let mut total = 0;
    let mut i = 0;
    while i < 4 {
        let v = build(i);
        for o in v {
            match o {
                Some(s) => { total = total + s.len(); },
                None => { total = total + 1; },
            };
        }
        i = i + 1;
    };
    println(total);
}
"#,
        &["188"],
        "for_match_vec_option_element_no_double_free",
    );
}

#[test]
fn asan_for_match_vec_result_element_no_double_free() {
    // Slice 3q: the Result sibling of the loop-element match pin, with a
    // mixed heap/scalar Result (Ok(String) / Err(i64)) — the Err arm binds a
    // scalar (no free either way), the Ok arm reads the borrowed payload.
    assert_clean_asan_run(
        r#"
fn build(n: i64) -> Vec[Result[String, i64]] {
    let mut v: Vec[Result[String, i64]] = Vec.new();
    v.push(Ok(f"alpha ok payload padded out beyond thirty-six bytes {n}"));
    v.push(Err(7_i64));
    return v;
}
fn main() {
    let mut total = 0;
    let mut i = 0;
    while i < 4 {
        let v = build(i);
        for r in v {
            match r {
                Ok(s) => { total = total + s.len(); },
                Err(e) => { total = total + e; },
            };
        }
        i = i + 1;
    };
    println(total);
}
"#,
        &["240"],
        "for_match_vec_result_element_no_double_free",
    );
}

#[test]
fn asan_for_iflet_vec_option_element_no_double_free() {
    // Slice 3q: the `if let` sibling — the if-let/while-let/let-else bind
    // sites never consulted `scrutinee_is_borrowed_binding` at all (only
    // `match` set `pattern_binding_is_borrow`), so
    // `for o in v { if let Some(s) = o { … } }` double-freed even after the
    // match path was fixed. All three bind sites now set the flag for a
    // borrowed identifier scrutinee.
    assert_clean_asan_run(
        r#"
fn build(n: i64) -> Vec[Option[String]] {
    let mut v: Vec[Option[String]] = Vec.new();
    v.push(Some(f"alpha payload padded beyond thirty-six bytes {n}"));
    v.push(None);
    return v;
}
fn main() {
    let mut total = 0;
    let mut i = 0;
    while i < 4 {
        let v = build(i);
        for o in v {
            if let Some(s) = o {
                total = total + s.len();
            }
        }
        i = i + 1;
    };
    println(total);
}
"#,
        &["184"],
        "for_iflet_vec_option_element_no_double_free",
    );
}

#[test]
fn asan_soa_pop_remove_no_leak_or_uaf() {
    // Exercises every SoA mutator together (pop, pop_front, remove)
    // alongside the scope-exit FreeSoaGroups cleanup. The shift-
    // memmoves run against the same group buffers the cleanup will
    // later free, so a wrong shift pointer / wrong byte count
    // would surface as ASAN heap-buffer-overflow or UAF. Two hot
    // groups exercise the per-group shift loop. (Primitive fields
    // here; the heap-field SoA element drops are covered by the
    // dedicated `asan_soa_string_field_*` / `asan_soa_vec_pod_field_*`
    // tests above.)
    //
    // The struct is 4 i64 words (`label` in a cold group): `pop()`
    // returns `Option[Entity]`, whose payload area is only 3 words, so
    // the popped 4-word `Entity` is heap-BOXED (see
    // docs/spikes/oversized-enum-payload.md). Re-widened from the 3-word
    // B#1 stop-gap now that the fresh-temp-scrutinee box-free (§1) lands:
    // `match entities.pop() { Some(e) => … }` reads the 4th word `label`
    // back through the box (was truncated/garbage before boxing) AND the
    // `BoxedEnumDrop` queued for this fresh-temp scrutinee frees the box,
    // so the run must stay ASAN-clean (a leaked box or a double-free with
    // the SoA group cleanup would surface here). Cold-group *layout*
    // codegen is covered separately in tests/codegen.rs.
    assert_clean_asan_run(
        r#"
struct Entity { x: i64, y: i64, hp: i64, label: i64 }
layout entities: Vec[Entity] {
    group physics { x, y }
    group combat { hp }
    group meta { label }
}
fn main() {
    let mut entities: Vec[Entity] = Vec.new();
    let mut i: i64 = 0;
    while i < 6 {
        entities.push(Entity { x: i, y: i * 10, hp: i * 100, label: i * 1000 + 7 });
        i = i + 1;
    }
    let _front = entities.pop_front();
    let _middle = entities.remove(2);
    match entities.pop() {
        Some(e) => {
            println(e.x);
            println(e.label);
        }
        None => println(-1),
    }
    println(entities.len());
}
"#,
        &["5", "5007", "3"],
        "soa_pop_remove_no_leak_or_uaf",
    );
}

#[test]
fn asan_soa_vec_pod_field_no_leak() {
    // A `Vec[i64]` (Vec over a POD element) SoA field: the per-element
    // drop frees each element's outer Vec buffer at scope exit. Each
    // `make(12)` is a 96-byte buffer (past LSan's blind spot); 8 per
    // frame × 20 = 160 buffers that pre-fix leaked. Sum of `tag` = 560.
    assert_clean_asan_run(
        r#"
struct Row { tag: i64, data: Vec[i64] }
layout rows: Vec[Row] { group tags { tag } group bulk { data } }
fn make(n: i64) -> Vec[i64] {
    let mut v: Vec[i64] = Vec.new();
    let mut i = 0;
    while i < n { v.push(i); i = i + 1; }
    v
}
fn main() with panics {
    let mut total = 0;
    let mut k = 0;
    while k < 20 {
        let mut rows: Vec[Row] = Vec.new();
        let mut i = 0;
        while i < 8 { rows.push(Row { tag: i, data: make(12) }); i = i + 1; }
        let mut j = 0;
        while j < rows.len() { total = total + rows[j].tag; j = j + 1; }
        k = k + 1;
    }
    println(total);
}
"#,
        &["560"],
        "soa_vec_pod_field",
    );
}

#[test]
fn asan_vec_of_shared_push_drop_singleton() {
    // B-2026-07-11-33 guard (+ the B-36 investigation): a `Vec[shared]` /
    // `Vec[Option[shared]]` rc-dec's its elements and frees its buffer at
    // scope exit, for the small SINGLE-element shape. Under Linux LSan this
    // is CLEAN — macOS `leaks` over-reported this shape (a false positive on
    // the `karac_realloc_or_panic` buffer, whose custom-allocator wrapper
    // the `leaks` tool doesn't track; B-36 was closed as a macOS-`leaks`
    // artifact, not a real leak). This test is the authoritative (LSan) guard.
    assert_clean_asan_run(
        r#"
shared struct N { val: i64, mut next: Option[N] }
fn main() {
    let n1 = N { val: 1, next: None };
    let mut v: Vec[N] = Vec.new();
    v.push(n1);
    let a = N { val: 2, next: None };
    let mut w: Vec[Option[N]] = Vec.new();
    w.push(Some(a));
    println(99);
}
"#,
        &["99"],
        "vec_of_shared_push_drop_singleton",
    );
}

#[test]
fn asan_shared_list_build_remove_repeat() {
    // Regression for the `shared struct` RC over-dec (2026-05-30): a
    // tail-cursor-built list, removed via `remove_nth_from_end`
    // (returns `dummy.next`, which aliases the `head` param), repeated
    // in a loop. Pre-fix the caller's binding shared the source's single
    // ref and the second scope-exit dec drove the refcount negative — a
    // double-free ASAN flags (and the build leaked). Must run clean.
    assert_clean_asan_run(
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
    while i < n { if let Some(node) = fast { fast = node.next; } i = i + 1i64; }
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
fn head_val(list: Option[ListNode]) -> i64 {
    match list { Some(node) => node.val, None => 0i64 }
}
fn main() {
    let data: Array[i64, 8] = [1, 2, 3, 4, 5, 6, 7, 8];
    let mut sum: i64 = 0i64;
    let mut k: i64 = 0i64;
    while k < 64i64 {
        let list = from_array(data);
        let n: i64 = (k % 8i64) + 1i64;
        let out = remove_nth_from_end(list, n);
        sum = sum + head_val(out);
        k = k + 1i64;
    }
    println(sum);
}
"#,
        &["72"],
        "shared_list_build_remove_repeat",
    );
}

#[test]
fn asan_shared_enum_vec_shared_payload_drop_no_leak() {
    // B-2026-07-13-13 (the shared-enum sibling of B-2026-07-13-11). A
    // `shared enum Tree { Branch(Vec[Node]) }` (Node shared) dropped with its
    // payload INTACT (RC → 0 while the Branch still holds the Vec) froze the
    // `{ptr,len,cap}` buffer but never dec'd the shared ELEMENTS —
    // `emit_shared_enum_field_drop`'s Vec/String arm had no element-drain
    // loop. One ref leaked per element (LSan: 16-byte Node boxes). This
    // churns the DIRECT-DROP path (construct, never match out the payload,
    // drop at scope exit) — the path this fix targets; the arm now drains
    // each element (the shared element's `emit_vec_elem_rc_dec_fn`) before
    // the buffer free. (The separate match-move shape `Branch(xs) => …`,
    // where the payload Vec is bound out, is tracked by B-2026-07-13-14.)
    assert_clean_asan_run(
        r#"
shared struct Node { mut val: i64 }
shared enum Tree { Leaf, Branch(Vec[Node]) }
fn build(n: i64) -> Tree {
    let mut v: Vec[Node] = Vec.new();
    v.push(Node { val: n });
    v.push(Node { val: n + 1 });
    Tree.Branch(v)
}
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 50 {
        let t = build(i);
        total = total + 1;
        i = i + 1;
    }
    println(total.to_string());
}
"#,
        &["50"],
        "shared_enum_vec_shared_payload_drop_no_leak",
    );
}

#[test]
fn asan_match_move_vec_shared_enum_payload_no_leak() {
    // B-2026-07-13-14. Matching a shared enum and binding a `Vec[shared T]`
    // payload OUT (`match t { Branch(xs) => … }`) leaked the shared elements.
    // The move-out suppression zeros the box's Vec `cap` so the enum's own
    // rc-drop skips the payload (the binding is the sole owner), but the
    // binding was tracked buffer-only (`track_vec_var`) — it freed the Vec
    // buffer and left every element's RC box unreferenced (LSan: 16-byte Node
    // boxes). `bind_pattern_values` now upgrades a `Vec[shared]` payload
    // binding to the element-draining tracker (`track_vec_of_aggs_var` → the
    // shared element's `emit_vec_elem_rc_dec_fn`) at the same single-owner
    // point, so no double-free (the source's drain is suppressed exactly as
    // its buffer-free already is — verified against move-further shapes:
    // return-out, re-move to another local, and a String payload which stays
    // buffer-only). 50 iters × 2 elements.
    assert_clean_asan_run(
        r#"
shared struct Node { mut val: i64 }
shared enum Tree { Leaf, Branch(Vec[Node]) }
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 50 {
        let mut v: Vec[Node] = Vec.new();
        v.push(Node { val: 1 });
        v.push(Node { val: 2 });
        let t = Tree.Branch(v);
        match t {
            Tree.Leaf => {}
            Tree.Branch(xs) => { total = total + xs.len(); }
        }
        i = i + 1;
    }
    println(total.to_string());
}
"#,
        &["100"],
        "match_move_vec_shared_enum_payload_no_leak",
    );
}

#[test]
fn asan_vec_insert_heap_no_double_free() {
    // `Vec[String].insert(idx, value)` MOVES the heap value into the
    // container (the `insert` codegen arm carries push's ownership-
    // suppression set), so the source binding must not also free the buffer.
    // Churns 100× — each iter builds a fresh owned String and inserts it at
    // the front (forcing a full memmove of the growing tail) — so a missed
    // source-cleanup suppression double-frees (ASAN) and a stray extra owner
    // leaks (LSan). The tail memmove also exercises the grow/realloc path.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut v: Vec[String] = Vec.new();
    let mut i: i64 = 0;
    while i < 100 {
        let s = f"item-{i}";
        v.insert(0, s);
        i = i + 1;
    }
    println(v.len().to_string());
}
"#,
        &["100"],
        "vec_insert_heap_no_double_free",
    );
}

#[test]
fn asan_remove_key_drop_body_runs_before_the_free() {
    // B-2026-08-27-2. The body travels into the runtime as a function
    // pointer and is invoked ON THE STORED KEY IN PLACE, in the window
    // between `lookup` and `free_stored_key`. That is the only place it can
    // read the key's fields — and the only place it can go wrong: run it
    // one step later and it reads freed memory; let it own the memory and
    // the runtime's `drop_key` free becomes a double free.
    //
    // The key's fields are SCALAR on purpose. A `String`-bearing struct key
    // would be the more obvious fixture, and it leaks that field on every
    // `remove` for a reason that has nothing to do with drop bodies:
    // `drop_key` is `llvm_ty_is_vec_struct(key_ty)`, false for a struct, so
    // the runtime never frees a struct key's inner buffer. Measured
    // identically with the key's `Drop` impl DELETED, and a bare
    // `Map[String, V]` is clean — filed as its own row. Using such a key
    // here would assert someone else's bug and mask this one.
    //
    // What is left is exactly this row's hazard: the body reads the stored
    // key through a pointer the runtime hands it mid-operation, and the
    // printed count is the oracle for it firing exactly once per removal —
    // which no sanitizer can see, because a double body is not a memory
    // error.
    let label = "remove_key_drop_body";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let src = r#"
#[derive(Hash, Eq, PartialEq)]
struct K { n: i64 }
struct V { s: String }
impl Drop for K { fn drop(mut ref self) { bump(self.n) } }
impl Drop for V { fn drop(mut ref self) { bump(0) } }
let mut FIRED: i64 = 0;
fn bump(n: i64) { FIRED = FIRED + 1; }
fn main() {
    let mut m: Map[K, V] = Map.new();
    let mut i = 0;
    while i < 200 {
        m.insert(K { n: i }, V { s: f"val-{i}-padding-padding" });
        m.remove(K { n: i });
        i = i + 1;
    }
    println(f"fired={FIRED} len={m.len()}");
}
"#;
    let Some((stdout, status)) = run_under_asan(src, label) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN run failed (status {status:?}); stdout:\n{stdout}"
    );
    // 200 removals x (ARGUMENT key body + STORED key body + value body)
    // = 600.
    //
    // B-2026-09-15-5 RAISED THIS FROM 400. `m.remove(K { n: i })` destroys
    // TWO keys, not one: the fresh argument temporary that is built,
    // hashed and discarded at the call, and the key the map was holding.
    // This row fixed the stored key's body; the argument temporary's was
    // still being dropped on the floor at every lookup entry point, so 400
    // looked complete and was in fact this defect written down.
    //
    // The BOUND-key spelling (`let p = K { n: i }; m.remove(p)`) measures
    // 600 as well, which is the invariant worth keeping in view: the count
    // does not depend on how the key is spelled, only on WHO owns each
    // body -- the binding's own live-range end in that spelling, the
    // lookup site in this one. Measured byte-identical on both backends.
    assert_eq!(
        stdout.trim(),
        "fired=600 len=0",
        "[{label}] stdout:\n{stdout}"
    );
}

// ── Owned Vec/String param moved into a local (kata-23, 2026-06-07) ──
//
// `let mut work = lists;` where `lists` is a bare by-value Vec/String
// param: the caller retains the buffer's scope-exit free (kata-22
// owned-param ABI), so the let-move must deep-copy — pre-fix the moved
// binding's `FreeVecBuffer` and the caller's free double-freed the
// same buffer. macOS malloc only trapped on some heap layouts (kata-23's
// ten cases split unpredictably); ASAN catches it deterministically.

#[test]
// B-2026-07-12-29 (FIXED): the compound index-assign of a shared/
// Option[shared] Vec element — `work[i] = merge_two(work[i], …)` — orphaned
// the OVERWRITTEN old node with no rc-dec, an ARM64-only leak (balanced on
// x86 via an arch-dependent struct-move/ABI path, unbalanced on arm64). The
// fix adds the ARC setter rule (retain-new → store → release-old) to
// `compile_vec_index_store`, releasing the old value via the same per-element
// drop the scope-exit drain uses. Previously `#[cfg_attr(aarch64, ignore)]`;
// the ignore is removed now that the arm64 leg is clean.
fn asan_owned_vec_param_let_move_interval_merge() {
    // kata-23 merge_k_lists shape: param Vec[Option[shared]] moved to
    // a mut local, in-place interval element reads/assignments, slot 0
    // returned, caller walks the spliced chain.
    assert_clean_asan_run(
        r#"
shared struct ListNode {
    val: i64,
    mut next: Option[ListNode],
}

fn merge_two(l1: Option[ListNode], l2: Option[ListNode]) -> Option[ListNode] {
    let dummy = ListNode { val: 0, next: None };
    let mut tail = dummy;
    let mut a = l1;
    let mut b = l2;
    loop {
        if let Some(na) = a {
            if let Some(nb) = b {
                if na.val <= nb.val {
                    tail.next = Some(na);
                    tail = na;
                    a = na.next;
                } else {
                    tail.next = Some(nb);
                    tail = nb;
                    b = nb.next;
                }
            } else {
                tail.next = a;
                break;
            }
        } else {
            tail.next = b;
            break;
        }
    }
    dummy.next
}

fn merge_k(lists: Vec[Option[ListNode]]) -> Option[ListNode] {
    let mut work = lists;
    let k = work.len();
    if k == 0 {
        return None;
    }
    let mut interval = 1;
    while interval < k {
        let mut i = 0;
        while i + interval < k {
            work[i] = merge_two(work[i].clone(), work[i + interval].clone());
            i = i + 2 * interval;
        }
        interval = 2 * interval;
    }
    work[0]
}

fn main() {
    let n1 = ListNode { val: 1, next: None };
    let n2 = ListNode { val: 2, next: None };
    let n3 = ListNode { val: 3, next: None };
    let mut v: Vec[Option[ListNode]] = Vec.new();
    v.push(Some(n1));
    v.push(Some(n2));
    v.push(Some(n3));
    let mut cur = merge_k(v);
    loop {
        match cur {
            Some(node) => {
                println(node.val);
                cur = node.next;
            }
            None => break,
        }
    }
}
"#,
        &["1", "2", "3"],
        "owned_vec_param_let_move_interval_merge",
    );
}

#[test]
fn asan_rebound_vec_shared_param_co_owns_its_elements() {
    // B-2026-09-07-56 — the MINIMAL shape, and the one the row missed.
    // `let mut work = xs` over a by-value `Vec[shared T]` param deep-copies
    // the buffer (the caller keeps the original's scope-exit drain), but the
    // copy is a flat memcpy of RC HANDLES: both containers then drain the
    // same boxes. Each element's count goes one below its true owner set,
    // so the caller's own binding dec reads — and writes — a freed refcount
    // word. No index-assign is involved; the row's fixture just happened to
    // carry one.
    assert_clean_asan_run(
        r#"
shared struct Node { val: i64 }

fn probe(xs: Vec[Node]) -> i64 {
    let mut work = xs;
    work[0].val
}

fn main() {
    let a = Node { val: 7 };
    let b = Node { val: 9 };
    let mut v: Vec[Node] = Vec.new();
    v.push(a);
    v.push(b);
    println(probe(v));
}
"#,
        &["7"],
        "rebound_vec_shared_param_co_owns_its_elements",
    );
}

#[test]
fn asan_rebound_vec_shared_param_survives_repeated_passes() {
    // The escalation the quarantine file warns about: a single pass drives
    // each count to -1 (one bad read+write), and each further pass frees a
    // box the other container still holds. Three passes is a double free,
    // not a stray read — proof that the count is genuinely restored rather
    // than the fixture's traffic happening to land back on zero.
    assert_clean_asan_run(
        r#"
shared struct Node { val: i64 }

fn probe(xs: Vec[Node]) -> i64 {
    let mut work = xs;
    work[0].val
}

fn main() {
    let a = Node { val: 7 };
    let b = Node { val: 9 };
    let mut v: Vec[Node] = Vec.new();
    v.push(a);
    v.push(b);
    let mut total = 0;
    total = total + probe(v);
    total = total + probe(v);
    total = total + probe(v);
    println(total);
}
"#,
        &["21"],
        "rebound_vec_shared_param_repeated_passes",
    );
}

#[test]
fn asan_owned_struct_param_vec_shared_field_move_co_owns() {
    // CONTROL, and labelled as one because it passes BOTH WITH AND WITHOUT
    // the fix. `let ks = p.kids` moving a `Vec[shared T]` FIELD out of a
    // by-value struct param was already balanced: its IR shows the
    // entry-copy machinery's `viewvsh` / `p14a.shvec` retain loops and NO
    // `dcopy.rc` loop, i.e. it never reaches
    // `emit_vecstr_defensive_copy`'s element chain at all.
    //
    // It earns its place anyway, on the hazard rather than the bug: an
    // over-retain here would keep each box alive but STILL REACHABLE from a
    // live container, which valgrind's default leak-check and LSan both
    // stay silent about. Pinning the shape is how a future widening of the
    // new arm gets caught.
    assert_clean_asan_run(
        r#"
shared struct Node { val: i64 }
struct Bag { kids: Vec[Node] }

fn probe(p: Bag) -> i64 {
    let ks = p.kids;
    ks[0].val + ks[1].val
}

fn main() {
    let a = Node { val: 7 };
    let b = Node { val: 9 };
    let mut v: Vec[Node] = Vec.new();
    v.push(a);
    v.push(b);
    let bag = Bag { kids: v };
    println(probe(bag));
}
"#,
        &["16"],
        "owned_struct_param_vec_shared_field_move",
    );
}

#[test]
fn asan_vec_option_shared_param_rebind_is_unchanged() {
    // Control. An `Option[shared T]` element was never affected: it reaches
    // the aggregate arm through `te_owns_option_heap_payload` and clones via
    // `karac_clone_Option_<T>`, which already rc-incs. This has to stay
    // exactly one retain — the new bare-`shared` arm must not also fire.
    assert_clean_asan_run(
        r#"
shared struct Node { val: i64 }

fn probe(xs: Vec[Option[Node]]) -> i64 {
    let mut work = xs;
    match work[0] {
        Option.Some(n) => n.val,
        Option.None => 0,
    }
}

fn main() {
    let a = Node { val: 7 };
    let b = Node { val: 9 };
    let mut v: Vec[Option[Node]] = Vec.new();
    v.push(Option.Some(a));
    v.push(Option.Some(b));
    println(probe(v));
}
"#,
        &["7"],
        "vec_option_shared_param_rebind_unchanged",
    );
}

#[test]
fn asan_rebound_vec_weak_param_co_owns_its_elements() {
    // B-2026-09-08-1 — B-2026-09-07-56's shape one OWNERSHIP TIER down, and
    // the reason that row's fix declined `weak` rather than covering it: a
    // weak slot is `TypeKind::Weak`, not a `Path`, so the strong arm never
    // sees it. Declining was right; what it exposed is that the weak tier
    // needs the WEAK-count version of the same operation.
    //
    // `let work = xs` over a by-value `Vec[weak T]` param deep-copies the
    // buffer for the same caller-retains reason, and the copy was a flat
    // memcpy of weak handles. Measured in the IR: exactly ONE
    // `karac_weak_downgrade` (the `push`) against TWO
    // `__karac_weak_slot_drop` call sites — the callee's copy and `main`'s
    // original both drain the same box.
    assert_clean_asan_run(
        r#"
shared struct N { v: i64 }

fn probe(xs: Vec[weak N]) -> i64 {
    let work = xs;
    match work[0] { Some(x) => { x.v } None => { 0 - 1 } }
}

fn main() {
    let a: N = N { v: 7 };
    let mut w: Vec[weak N] = Vec.new();
    w.push(a);
    println(probe(w));
    println(a.v);
}
"#,
        &["7", "7"],
        "rebound_vec_weak_param_co_owns_its_elements",
    );
}

#[test]
fn asan_rebound_vec_weak_param_leaks_per_element_not_per_program() {
    // The under-count is PER SLOT, so the loss scales with the element
    // count rather than being one fixed block. Measured pre-fix under
    // valgrind: one 24-byte loss record for the single-element fixture
    // above and THREE for this one, one per element. A per-program leak
    // would look identical at one element and is what a single-element
    // regression alone would fail to distinguish.
    assert_clean_asan_run(
        r#"
shared struct N { v: i64 }

fn at(xs: ref Vec[weak N], i: i64) -> i64 {
    match xs[i] { Some(x) => { x.v } None => { 0 - 1 } }
}

fn probe(xs: Vec[weak N]) -> i64 {
    let work = xs;
    at(work, 0) + at(work, 1) + at(work, 2)
}

fn main() {
    let a: N = N { v: 7 };
    let b: N = N { v: 11 };
    let c: N = N { v: 13 };
    let mut w: Vec[weak N] = Vec.new();
    w.push(a);
    w.push(b);
    w.push(c);
    println(probe(w));
}
"#,
        &["31"],
        "rebound_vec_weak_param_per_element",
    );
}

#[test]
fn asan_rebound_vec_weak_param_with_dead_referent_is_not_a_use_after_free() {
    // THE CLASS IS LIFETIME-DEPENDENT, and this is the fixture that shows
    // it. B-2026-09-08-1's own mechanism note reasoned that the defect
    // "reads as a LEAK rather than the strong tier's use-after-free"
    // because "a weak dec never frees a payload, it only fails to". That
    // is true only while the STRONG owner outlives the container.
    //
    // Here the referent is already dead when the containers drain (`build`
    // returns the weak Vec and drops its strong binding), so the FIRST of
    // the two drops takes weak 1 -> 0 against strong == 0 and
    // `karac_weak_box_strong_zero_release` frees the control block; the
    // second drop then reads it. Measured pre-fix: `Invalid read of size
    // 8`, not a leak. Same defect, same fix, a strictly worse symptom —
    // which is why the row's `medium` severity was the floor rather than
    // the ceiling.
    assert_clean_asan_run(
        r#"
shared struct N { v: i64 }

fn probe(xs: Vec[weak N]) -> i64 {
    let work = xs;
    match work[0] { Some(x) => { x.v } None => { 0 - 1 } }
}

fn build() -> Vec[weak N] {
    let a: N = N { v: 7 };
    let mut w: Vec[weak N] = Vec.new();
    w.push(a);
    w
}

fn main() {
    let dead = build();
    println(probe(dead));
}
"#,
        &["-1"],
        "rebound_vec_weak_param_dead_referent",
    );
}

#[test]
fn asan_returned_vec_weak_param_co_owns_its_elements() {
    // The SECOND consume site of the same copy helper. `let work = xs` and
    // `return xs` both reach `emit_vecstr_defensive_copy`, so a fix keyed
    // on the rebind statement rather than on the copy would leave this one
    // broken — measured pre-fix at 24 bytes lost, exactly as the rebind.
    // Pinning both is what makes the fix's placement (in the copy, not at
    // the statement) load-bearing rather than incidental.
    assert_clean_asan_run(
        r#"
shared struct N { v: i64 }

fn hand_back(xs: Vec[weak N]) -> Vec[weak N] {
    xs
}

fn main() {
    let a: N = N { v: 7 };
    let mut w: Vec[weak N] = Vec.new();
    w.push(a);
    let back = hand_back(w);
    match back[0] { Some(x) => { println(x.v) } None => { println(0 - 1) } }
    println(a.v);
}
"#,
        &["7", "7"],
        "returned_vec_weak_param_co_owns",
    );
}

#[test]
fn asan_struct_field_vec_weak_view_is_unchanged() {
    // CONTROL — the other shape that reaches a `Vec[weak T]` through a
    // by-value param, and the one that was already clean: `let ks = h.kids`
    // off an owned struct param is handled by the entry-copy machinery
    // rather than by the copy helper's element chain. Clean before and
    // after, so it pins that this fix did not widen into that path.
    assert_clean_asan_run(
        r#"
shared struct N { v: i64 }

struct Holder { kids: Vec[weak N] }

fn probe(h: Holder) -> i64 {
    let ks = h.kids;
    match ks[0] { Some(x) => { x.v } None => { 0 - 1 } }
}

fn main() {
    let a: N = N { v: 7 };
    let mut w: Vec[weak N] = Vec.new();
    w.push(a);
    let h: Holder = Holder { kids: w };
    println(probe(h));
    println(a.v);
}
"#,
        &["7", "7"],
        "struct_field_vec_weak_view_unchanged",
    );
}

#[test]
fn asan_nested_vec_weak_container_drains_every_slot() {
    // B-2026-09-08-7 — the row's minimal reproducer, and note it takes NO
    // function parameter at all: this family is reachable with none of the
    // caller-retains machinery B-2026-09-08-1 was about, which is what made
    // the two separate rows rather than one bug seen twice.
    //
    // `te_recursive_drop_fully_supported` answered false for a
    // `TypeKind::Weak` leaf (the `_ => false` catch-all), so a
    // `Vec[Vec[weak T]]` took the one-level buffer-only fast path and no
    // weak slot inside it was ever drained. 24 bytes per slot, at both opt
    // levels.
    assert_clean_asan_run(
        r#"
shared struct N { v: i64 }

fn main() {
    let a: N = N { v: 7 };
    let mut inner: Vec[weak N] = Vec.new();
    inner.push(a);
    let mut outer: Vec[Vec[weak N]] = Vec.new();
    outer.push(inner);
    match outer[0][0] { Some(x) => { println(x.v) } None => { println(0 - 1) } }
    println(a.v);
}
"#,
        &["7", "7"],
        "nested_vec_weak_container_drains_every_slot",
    );
}

#[test]
fn asan_nested_vec_weak_container_without_any_read_drains() {
    // The SAME container, never read. This fixture is why the row could be
    // split from the two defects sitting on top of it: with the read gone,
    // the drain gap is the only thing left, and this went leak -> clean on
    // the drain fix ALONE. The fixture above stayed red at that point and
    // would have made a correct fix look wrong.
    //
    // Worth keeping as its own case rather than folding into the one above:
    // a future regression in either half is distinguishable by which of the
    // two goes red.
    assert_clean_asan_run(
        r#"
shared struct N { v: i64 }

fn main() {
    let a: N = N { v: 7 };
    let mut inner: Vec[weak N] = Vec.new();
    inner.push(a);
    let mut outer: Vec[Vec[weak N]] = Vec.new();
    outer.push(inner);
    println(a.v);
}
"#,
        &["7"],
        "nested_vec_weak_container_without_read",
    );
}

#[test]
fn asan_cloned_row_of_nested_vec_weak_co_owns_its_slots() {
    // The clone half. `emit_clone_fn_for_type_expr` had no `TypeKind::Weak`
    // arm, so a weak leaf fell to the primitive fallback whose body is
    // load-store-return -- a flat ALIAS of the source's handle, handed to a
    // destination that runs its own `__karac_weak_slot_drop`.
    //
    // Same rule as B-2026-09-08-1 one copy site over: whatever hands out an
    // independent owner has to hand out an independent count.
    assert_clean_asan_run(
        r#"
shared struct N { v: i64 }

fn main() {
    let a: N = N { v: 7 };
    let mut inner: Vec[weak N] = Vec.new();
    inner.push(a);
    let mut outer: Vec[Vec[weak N]] = Vec.new();
    outer.push(inner);
    let row: Vec[weak N] = outer[0].clone();
    match row[0] { Some(x) => { println(x.v) } None => { println(0 - 1) } }
    println(a.v);
}
"#,
        &["7", "7"],
        "cloned_row_of_nested_vec_weak_co_owns",
    );
}

#[test]
fn asan_nested_vec_option_shared_read_is_unchanged() {
    // CONTROL — the non-weak peer of the read fixture above, clean before
    // and after. `Option[shared N]` reaches the chained index through the
    // ordinary owned-payload path rather than through a weak upgrade, so it
    // never needed the balancing acquire and must not gain one. Pins that
    // the resolver widening did not spill into the strong tier.
    assert_clean_asan_run(
        r#"
shared struct N { v: i64 }

fn main() {
    let a: N = N { v: 7 };
    let mut inner: Vec[Option[N]] = Vec.new();
    inner.push(Option.Some(a));
    let mut outer: Vec[Vec[Option[N]]] = Vec.new();
    outer.push(inner);
    match outer[0][0] { Some(x) => { println(x.v) } None => { println(0 - 1) } }
    println(a.v);
}
"#,
        &["7", "7"],
        "nested_vec_option_shared_read_unchanged",
    );
}

#[test]
fn asan_owned_vec_param_assign_move() {
    // Assign-arm sibling: `work = v;` deep-copies; the LHS's prior
    // buffer is eagerly freed (no leak), the caller's free stays
    // valid.
    assert_clean_asan_run(
        r#"
fn second(v: Vec[i64]) -> i64 {
    let mut work: Vec[i64] = Vec.new();
    work.push(0);
    work = v;
    work[1]
}

fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(7);
    v.push(9);
    println(second(v));
}
"#,
        &["9"],
        "owned_vec_param_assign_move",
    );
}

#[test]
fn asan_let_bound_heap_vec_element_no_double_free() {
    // `let w = v[i]` where v: Vec[String] with heap-owned (f-string) elements
    // — the index returns a SHALLOW element struct aliasing v's buffer.
    // Binding it owned must DEEP-CLONE (B-2026-06-14-11): both w's drop and
    // v's element-drop run at scope exit, so without the clone they free the
    // same buffer (double-free). The element stays in v (interp clones), so v[i]
    // is reused afterward to confirm it wasn't consumed/corrupted. Loops so the
    // clone path runs many times (each w dropped per-iteration).
    assert_clean_asan_run(
        r#"
fn main() {
    let mut v: Vec[String] = Vec.new();
    let mut i = 0i64;
    while i < 8 { v.push(f"token-{i}-payload"); i = i + 1i64; }
    let mut total = 0i64;
    let mut j = 0i64;
    while j < 4000 {
        let w = v[j & 7i64].clone();          // deep-clone bind; v[j&7] stays valid
        total = total + w.bytes().len();
        j = j + 1i64;
    }
    println(total);
    println(v[0i64]);                 // v still intact after 4000 binds
    println(v.len());
}
"#,
        &["60000", "token-0-payload", "8"],
        "let_bound_heap_vec_element_no_double_free",
    );
}

#[test]
fn asan_let_bound_vec_enum_struct_element_no_double_free() {
    // B-2026-06-14-12 (sibling of B-11): a `Vec` element that is a user ENUM
    // or STRUCT carrying a heap String payload. Indexing returns a SHALLOW
    // copy aliasing v's buffer, so it must be deep-cloned (emit_enum_clone_fn
    // / emit_struct_clone_fn) — otherwise the binding and v's element-drop
    // free the same buffer (double-free). Covers all three element-read shapes
    // the fix touches: (1) `let e = es[i]` + `match e` move-out (enum), (2)
    // `let p = ps[i]` + field access (struct), and (3) the DIRECT
    // `match es[i] { Word(s) => … }` scrutinee (the lexer's token-consume
    // shape), which clones `scrut` itself so the arm extracts the clone and
    // the freshtemp materialization drop-tracks it. Each source vec is reused
    // after the binds to confirm it stayed intact; the loop runs the
    // synthesized clone + suppression paths thousands of times.
    assert_clean_asan_run(
        r#"
#[derive(Clone)]
enum Tok { Word(String), End }
#[derive(Clone)]
struct Pair { name: String, n: i64 }
fn main() {
    let mut es: Vec[Tok] = Vec.new();
    let mut ps: Vec[Pair] = Vec.new();
    let mut i = 0i64;
    while i < 8 {
        es.push(Tok.Word(f"w-{i}-payload"));
        ps.push(Pair { name: f"p-{i}-payload", n: i });
        i = i + 1;
    }
    let mut total = 0i64;
    let mut j = 0i64;
    while j < 4000 {
        let e = es[j & 7i64].clone();            // deep-clone bind of enum element
        match e { Tok.Word(s) => { total = total + s.bytes().len(); }, Tok.End => {} }
        let p = ps[j & 7i64].clone();            // deep-clone bind of struct element
        total = total + p.name.bytes().len() + p.n;
        j = j + 1i64;
    }
    println(total);
    match es[0i64] { Tok.Word(s) => println(s), Tok.End => println("end") }
    println(ps[0i64].name);             // both vecs intact after 4000 binds
    println(es.len());
}
"#,
        &["102000", "w-0-payload", "p-0-payload", "8"],
        "let_bound_vec_enum_struct_element_no_double_free",
    );
}

#[test]
fn asan_try_clone_vec_tuple_scalar_deep_free() {
    // Non-heap tuple element (`Vec[(i64, i64)]`) routes through the tuple
    // fallible-clone fn (per-field recursion) nested inside the Vec
    // fallible-clone loop. No inner heap, so the only allocation is the
    // buffer; source and clone free their buffers exactly once.
    //
    // The tuple-WITH-heap-element variant (`Vec[(i64, String)]`) is NOT
    // covered: it UAFs identically under the *panicking* `.clone()` too —
    // a pre-existing defect in the Vec-of-tuple-with-heap-element clone path
    // (bugs.md B-2026-06-10-5), independent of `try_clone`.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut src: Vec[(i64, i64)] = Vec.new();
    src.push((1i64, 2i64));
    src.push((3i64, 4i64));
    match src.try_clone() {
        Ok(c) => {
            println(c[0].0); println(c[0].1);
            println(c[1].0); println(c[1].1);
        }
        Err(_) => println("err"),
    }
    src.push((5i64, 6i64));
    println(src.len());
}
"#,
        &["1", "2", "3", "4", "3"],
        "try_clone_vec_tuple_scalar_deep_free",
    );
}

#[test]
fn asan_vec_tuple_heap_element_push_read() {
    // B-2026-06-10-5 (core UAF): pushing a tuple with an inline f-string
    // heap field into a `Vec[(i64, String)]` and reading it back. Before
    // the fix `compile_tuple` left the f-string accumulator's
    // `FreeVecBuffer` armed; it freed the String buffer right after the
    // push, leaving the Vec element dangling (heap-use-after-free on the
    // read / scope-exit). The fix suppresses the inline-f-string acc as
    // the tuple takes ownership, and the Vec's scope-exit drain recurses
    // into the tuple element's owned String field (one free, at scope
    // exit). Multiple pushes exercise the grow path too.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut src: Vec[(i64, String)] = Vec.new();
    src.push((1i64, f"p{1}"));
    src.push((2i64, f"q{2}"));
    println(src[0].1);
    println(src[1].1);
    println(src.len());
}
"#,
        &["p1", "q2", "2"],
        "vec_tuple_heap_element_push_read",
    );
}

#[test]
fn asan_vec_tuple_heap_element_clone_deep_free() {
    // B-2026-06-10-5 (the headline `.clone()` repro): build a
    // `Vec[(i64, String)]` by pushing inline-f-string tuples, then
    // `.clone()` it. The reported UAF (`karac_string_clone` reading a
    // freed pointer) was NOT a shallow clone — `emit_vec_clone_fn`
    // already deep-clones per element. The real cause was upstream: the
    // push left each inline f-string accumulator's `FreeVecBuffer` armed,
    // so it freed the source String right after the push; `clone` then
    // read that freed buffer. The fix suppresses the inline-f-string acc
    // at tuple construction (source valid for clone) and recurses the
    // Vec scope-exit drain into the tuple element's String (one free per
    // Vec). Source and clone own independent buffers; each frees once.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut src: Vec[(i64, String)] = Vec.new();
    src.push((1i64, f"p{1}"));
    src.push((2i64, f"q{2}"));
    let c = src.clone();
    println(c[0].1);
    println(c[1].1);
    println(src.len());
}
"#,
        &["p1", "q2", "2"],
        "vec_tuple_heap_element_clone_deep_free",
    );
}

#[test]
fn asan_vec_tuple_heap_element_try_clone_deep_free() {
    // B-2026-06-10-5 closes the original tracker's deferred coverage:
    // `Vec[(i64, String)].try_clone()` (the fallible sibling of the
    // `.clone()` case above) over a heap tuple element. Same root cause
    // and fix; source + the `Ok`-bound clone each free their own buffers
    // exactly once. The scalar-tuple variant
    // (`asan_try_clone_vec_tuple_scalar_deep_free`) guarded the non-heap
    // path; this guards the heap-element path it deferred.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut src: Vec[(i64, String)] = Vec.new();
    src.push((1i64, f"p{1}"));
    src.push((2i64, f"q{2}"));
    match src.try_clone() {
        Ok(c) => {
            println(c[0].1);
            println(c[1].1);
            println(c.len());
        }
        Err(_) => println("err"),
    }
    println(src.len());
}
"#,
        &["p1", "q2", "2", "2"],
        "vec_tuple_heap_element_try_clone_deep_free",
    );
}

#[test]
fn asan_struct_wrapped_deep_build_and_vec_children_no_leak() {
    // Recursive builder to depth N, trees built in a loop and pushed into
    // a `Vec[Expr]`, and a `Call` variant with a `Vec[Expr]` of
    // heterogeneous children. Looping makes any per-iteration leak of a
    // tree (or its RC children) visible to the Linux-CI LSan gate.
    assert_clean_asan_run(
        r#"
shared enum Expr { Num(i64), Add(BinOp), Neg(Unary), Call(CallExpr) }
struct BinOp { left: Expr, right: Expr }
struct Unary { operand: Expr }
struct CallExpr { callee: Expr, args: Vec[Expr] }
fn eval(e: Expr) -> i64 {
    match e {
        Num(n) => n,
        Add(b) => eval(b.left) + eval(b.right),
        Neg(u) => 0 - eval(u.operand),
        Call(c) => {
            let mut acc = eval(c.callee);
            for a in c.args { acc = acc + eval(a); }
            acc
        }
    }
}
fn build(n: i64) -> Expr {
    if n <= 0 { Num(0) }
    else { Neg(Unary { operand: Add(BinOp { left: Num(n), right: build(n - 1) }) }) }
}
fn main() {
    let mut args: Vec[Expr] = Vec.new();
    let mut i: i64 = 0;
    while i < 10 { args.push(Num(i)); i = i + 1; }
    let call = Call(CallExpr { callee: Num(100), args: args });
    let mut forest: Vec[Expr] = Vec.new();
    let mut j: i64 = 0;
    while j < 30 { forest.push(build(j)); j = j + 1; }
    let mut total: i64 = eval(call);
    for t in forest { total = total + eval(t); }
    println(total);
}
"#,
        &["-80"],
        "struct_wrapped_deep_build_and_vec_children",
    );
}

#[test]
fn asan_vec_of_shared_enum_elements_freed_no_leak() {
    // B-2026-06-14-28 (Vec side) — a `Vec[Expr]` whose elements are a
    // `shared enum` (the AST-port `Call(args: Vec[Expr])` sequence-child
    // shape). Each element is an 8-byte RC pointer; the per-element drop
    // must rc-dec it (and recurse into the box's children), not value-drop
    // it. Pre-fix, `vec_elem_agg_drop_for_type_expr` routed a shared enum
    // to `emit_enum_drop_switch` (the VALUE drop) — which never
    // decremented the refcount, leaking every element (and its struct-
    // wrapped children). Builds Vecs of heterogeneous variants in a loop,
    // consumes some via a for-loop move-out and drops others whole.
    assert_clean_asan_run(
        r#"
shared enum Expr { Num(i64), Add(BinOp) }
struct BinOp { left: Expr, right: Expr }
fn eval(e: Expr) -> i64 {
    match e {
        Num(n) => n,
        Add(b) => eval(b.left) + eval(b.right),
    }
}
fn build(n: i64) -> Vec[Expr] {
    let mut v: Vec[Expr] = Vec.new();
    let mut i: i64 = 0;
    while i < n {
        v.push(Add(BinOp { left: Num(i), right: Num(1) }));
        i = i + 1;
    }
    v
}
fn main() {
    let mut total: i64 = 0;
    let mut k: i64 = 0;
    while k < 20 {
        let v: Vec[Expr] = build(8);
        // consume via for-loop move-out
        for e in v {
            total = total + eval(e);
        }
        // a second Vec dropped WHOLE (not consumed)
        let w: Vec[Expr] = build(4);
        total = total + (w.len() as i64);
        k = k + 1;
    }
    println(total);
}
"#,
        &["800"],
        "vec_of_shared_enum_elements_freed",
    );
}

#[test]
fn asan_struct_wrapped_vec_field_in_struct_payload_no_leak() {
    // B-2026-06-14-31 (leak #1) — a `Vec[Expr]` FIELD inside a struct
    // payload of a shared enum (`Call(CallExpr { args: Vec[Expr] })`, the
    // AST-port sequence-child shape), with the enum box dropped WHOLE
    // (never consumed). Pre-fix, `emit_nested_struct_shared_rc_decs` had no
    // Vec arm: the inline Vec's `{data,len,cap}` buffer (an 80-byte direct
    // alloc) AND every element box leaked when the shared-enum box freed —
    // silent under mac ASAN, caught by the Linux-CI LSan gate. Also covers
    // a struct whose ONLY shared content is a `Vec[shared]` field
    // (`Wrap { items: Vec[Expr] }`) — that requires the
    // `field_owns_shared`/`struct_owns_shared_field` Vec[shared] classifier
    // arm, or the variant gets no drop block at all. Looped to surface a
    // per-iteration leak.
    assert_clean_asan_run(
        r#"
shared enum Expr { Num(i64), Call(CallExpr), Wrapped(Wrap) }
struct CallExpr { callee: Expr, args: Vec[Expr] }
struct Wrap { items: Vec[Expr] }
fn sum_args(e: Expr) -> i64 {
    match e {
        Num(n) => n,
        Call(c) => c.args.len() as i64,
        Wrapped(w) => w.items.len() as i64,
    }
}
fn main() {
    let mut total: i64 = 0;
    let mut k: i64 = 0;
    while k < 25 {
        let mut args: Vec[Expr] = Vec.new();
        let mut i: i64 = 0;
        while i < 10 { args.push(Num(i)); i = i + 1; }
        let call: Expr = Call(CallExpr { callee: Num(100), args: args });
        total = total + sum_args(call);
        // a struct whose ONLY shared content is a Vec[shared] field
        let mut items: Vec[Expr] = Vec.new();
        let mut j: i64 = 0;
        while j < 5 { items.push(Num(j)); j = j + 1; }
        let wrapped: Expr = Wrapped(Wrap { items: items });
        total = total + sum_args(wrapped);
        k = k + 1;
    }
    println(total);
}
"#,
        &["375"],
        "struct_wrapped_vec_field_in_struct_payload",
    );
}

#[test]
fn asan_value_enum_nested_struct_vec_shared_inplace_drop_no_leak_no_double_free() {
    // B-2026-06-14-34 — a NON-shared enum (`Stmt`) whose variant wraps a
    // struct (`CallExpr`) that owns BOTH an inline `shared` field (`callee:
    // Expr`) AND a `Vec[shared]` (`args: Vec[Expr]`), dropped IN PLACE via
    // the value-drop synthesizer `emit_enum_drop_switch`. This is the shape
    // the self-host lexer hit: the B-31 `Vec[shared]` drain arm of
    // `emit_nested_struct_shared_rc_decs` (1) appended its blocks to
    // `self.current_fn` (the OUTER fn, not the drop fn) → cross-function
    // basic-block reference → module-verification failure, masked on
    // `--test codegen` because `compile_to_object` skips verification and
    // the optimizer DCE'd the orphan blocks; and (2) unconditionally
    // `free`d the Vec buffer that `__karac_drop_struct_CallExpr` ALSO freed
    // → double-free once the blocks became reachable, plus a use-after-free
    // if the drain ran after the buffer free. The fix threads the real
    // `drop_fn` + scoped `current_fn` through the walker, gates the buffer
    // `free` on `owns_buffer_free` (false in the value path — the struct
    // drop owns it), and orders the element-drain BEFORE the struct drop.
    // On Linux CI/LSan this faults if the element rc-dec drain regresses (a
    // leak); on macOS it is the double-free / UAF gate.
    assert_clean_asan_run(
        r#"
shared enum Expr { Num(i64), Add(BinOp) }
struct BinOp { left: Expr, right: Expr }
enum Stmt { Call(CallExpr) }
struct CallExpr { callee: Expr, args: Vec[Expr] }
fn main() {
    let mut total: i64 = 0;
    let mut j: i64 = 0;
    while j < 20 {
        let mut args: Vec[Expr] = Vec.new();
        let mut i: i64 = 0;
        while i < 3 { args.push(Num(i)); i = i + 1; }
        // Dropped IN PLACE (wildcard match consumes nothing) — exercises the
        // VALUE-drop of `Stmt`, whose `Call(CallExpr)` payload owns a
        // `Vec[shared Expr]`: the walker must rc-dec the inline `callee` Expr
        // box AND drain the `args` element boxes, while `__karac_drop_struct_
        // CallExpr` frees the `args` buffer exactly once (and runs AFTER the
        // drain, so the drain reads a live buffer).
        let s = Call(CallExpr { callee: Num(100), args: args });
        let k = match s { Call(_) => 1 };
        total = total + k;
        j = j + 1;
    }
    println(total);
}
"#,
        &["20"],
        "value_enum_nested_struct_vec_shared_inplace_drop",
    );
}

#[test]
fn asan_struct_field_vec_of_struct_with_enum_field_drop_no_leak() {
    // #35 (phase-12 self-hosting, parser stage): a `Vec[Sp]`
    // (`Sp { tok: Tk, off }`, heap enum `Tk`) held in a struct FIELD
    // (`P { toks: Vec[Sp] }`), dropped with live `Id(String)` elements
    // still in the buffer. The owning struct's synthesized drop
    // (`__karac_drop_struct_P`) used to free only the Vec's `{ptr,len,cap}`
    // buffer and NEVER drain elements, so every unconsumed element's String
    // payload leaked — the Vec-element peer of the #15 / #18 / #21
    // struct-drop-ignores-heap-leaf family. The fix drains each element
    // through `vec_elem_agg_drop_for_type_expr` (→ `Sp`'s
    // `__karac_drop_struct_Sp` → `Tk`'s `__karac_drop_Tk` switch) before
    // the buffer free, so each live element's payload frees exactly once.
    // Looped so a per-iteration leak accumulates visibly under LSan; the
    // dropped Vec keeps ALL elements live (nothing consumed) to exercise
    // the element-drain path. This is the parser's own shape: the `Parser`
    // drops its `Vec[SpannedToken]` with unconsumed trailing tokens whose
    // `Token` enums carry Strings.
    // `mk` returns the `Vec[Sp]` (its local cleanup is move-suppressed),
    // so inside the loop the ONLY live owner of the buffer is `p`, and the
    // ONLY cleanup is `__karac_drop_struct_P` — there is no sibling local
    // Vec-of-aggs drain to mask the gap. This is the parser's exact shape:
    // `Parser { tokens: Vec[SpannedToken] }` holds the sole reference and
    // is dropped with unconsumed tokens.
    assert_clean_asan_run(
        r#"
enum Tk { A, Id(String), Num(i64) }
struct Sp { tok: Tk, off: i64 }
struct P { toks: Vec[Sp], pos: i64 }
fn mk() -> Vec[Sp] {
    let mut w: Vec[Sp] = Vec.new();
    w.push(Sp { tok: Tk.Id("ident_long_enough_to_force_a_heap_buffer".to_string()), off: 5 });
    w.push(Sp { tok: Tk.Id("another_heap_allocated_identifier_string".to_string()), off: 9 });
    w.push(Sp { tok: Tk.Num(7), off: 3 });
    w
}
fn main() {
    let mut total: i64 = 0;
    let mut i: i64 = 0;
    while i < 50 {
        let p = P { toks: mk(), pos: 0 };
        total = total + p.toks[0].off + p.toks[1].off + p.toks[2].off;
        i = i + 1;
    }
    println(total);
}
"#,
        &["850"],
        "struct_field_vec_of_struct_with_enum_field_drop",
    );
}

// Phase-12 #43 (label-in-plain-drop leak) — FIXED by B-2026-08-07-20, and
// un-ignored here, which is the regression the checklist entry named as the
// proof. A LABELED call node (`Arg { label: Some(..), value: Expr }`) is
// built and PLAIN-DROPPED without consuming (read by `ref`, no `for a in
// args` / render). The shared-enum RC box-walker rc-dec'd the shared `value`
// and freed the Vec buffer but never freed the plain `Option[String]` label,
// because `Arg`'s struct drop refused to promote an `Option` field: the
// copy-support arm is closed by the shared `value` and the own-by-transfer
// arm is too (B-2026-08-05-32 keeps a shared-owning struct on
// caller-retains). `shared_owning_struct_sole_field_owner` supplies the
// third arm.
//
// #43 predicted the fix would have to be move-out SUPPRESSION (zero the
// plain-heap caps in the box's retained payload alias, keep the shared-edge
// pointers). It went the other way: the drain frees the label, and the
// for-loop element registration DUPLICATES it on extract so the consume path
// owns an independent copy — the same clone-on-extract the bare-`shared` and
// `String` leaves already take. Duplication rather than suppression, so
// nothing has to reach across into the box's alias. The consume peer
// (`asan_vec_of_struct_shared_and_option_field_consumed_no_leak`, directly
// below) is the other half of that pairing and guards it.
// All Strings >=36 bytes for LSan visibility.
#[test]
fn asan_vec_of_struct_labeled_plain_drop_no_leak_43() {
    assert_clean_asan_run(
        r#"
shared enum Expr { Lit(LitNode), Call(CallNode) }
struct LitNode { name: String, val: i64 }
struct Arg { label: Option[String], value: Expr }
struct CallNode { callee: Expr, args: Vec[Arg], nargs: i64 }
fn lit(s: String, v: i64) -> Expr { Expr.Lit(LitNode { name: s, val: v }) }
fn mk() -> Expr {
    let mut args: Vec[Arg] = Vec.new();
    args.push(Arg { label: Some("first_argument_label_long_enough_to_force_heap".to_string()), value: lit("first_arg_value_identifier_long_enough_for_heap".to_string(), 1) });
    args.push(Arg { label: None, value: lit("second_arg_value_identifier_long_enough_for_heap".to_string(), 2) });
    Expr.Call(CallNode { callee: lit("callee_identifier_name_long_enough_for_heap_buf".to_string(), 100), args: args, nargs: 2 })
}
fn root(e: ref Expr) -> i64 {
    match e {
        Lit(n) => n.val,
        Call(c) => c.nargs,
    }
}
fn main() {
    let mut total: i64 = 0;
    let mut i: i64 = 0;
    while i < 50 {
        let e = mk();
        total = total + root(e);
        i = i + 1;
    }
    println(total);
}
"#,
        &["100"],
        "vec_of_struct_labeled_plain_drop_leaks_pinned_43",
    );
}

#[test]
fn asan_vec_of_struct_with_shared_field_drop_no_leak() {
    // B-2026-06-19 (phase-12 parser slice 2a): the self-hosted parser's
    // `Call(CallExpr { args: Vec[CallArg] })` shape — a `Vec[Arg]` whose
    // element struct `Arg { value: Expr }` owns a SHARED-enum field, the Vec
    // living inside a shared-enum struct payload (`Call(CallNode)`). The
    // whole `Call` tree is built and dropped on the PLAIN scope-exit path
    // (read by `ref`, never consumed), so its cleanup is the shared-enum RC
    // drop walker. The Vec element's value drop (`__karac_drop_struct_Arg`)
    // skips its shared `value` field by design (a local struct's shared
    // fields are rc-dec'd by the `let` cleanup — B-2026-06-14-28 #3), but a
    // Vec ELEMENT has no let-cleanup, so each arg's shared box leaked once
    // per element. `vec_elem_agg_drop_for_type_expr` now routes a
    // shared-owning struct element through `__karac_vec_elem_full_drop_Arg`
    // (the value drop PLUS `emit_nested_struct_shared_rc_decs`, which
    // rc-dec's the shared field). The drain is refcount-safe in BOTH the
    // pure-drop path (here) and the by-value-consume path (the renderer /
    // later compiler phases) — the consume site rc-incs the shared handle
    // on the element copy, balancing the box-walker's dec.
    // Looped so a per-iteration leak accumulates visibly under the
    // authoritative Linux-CI LSan gate; every String is >=36 bytes so the
    // freed-but-reachable short-String LSan blind spot can't mask it.
    // (NOTE: the `CallArg.label: Option[String]` field is intentionally
    // omitted here. The shared-value drain above is consume-safe; an
    // Option[String]/String *plain* heap field is NOT refcount-protected,
    // so freeing it in the box-walker double-frees against a by-value
    // consumer that also frees it — the render/consume path stays leak-free
    // because the consumer frees the label, but a labeled call built and
    // plain-dropped without consuming leaks its label. Closing that residual
    // needs move-out suppression at the Vec[struct] consume site — tracked
    // as a phase-12 parser follow-on, not a regression here.)
    assert_clean_asan_run(
        r#"
shared enum Expr { Lit(LitNode), Call(CallNode) }
struct LitNode { name: String, val: i64 }
struct Arg { value: Expr }
struct CallNode { callee: Expr, args: Vec[Arg], nargs: i64 }
fn lit(s: String, v: i64) -> Expr { Expr.Lit(LitNode { name: s, val: v }) }
fn mk() -> Expr {
    let mut args: Vec[Arg] = Vec.new();
    args.push(Arg { value: lit("first_arg_value_identifier_long_enough_for_heap".to_string(), 1) });
    args.push(Arg { value: lit("second_arg_value_identifier_long_enough_for_heap".to_string(), 2) });
    Expr.Call(CallNode { callee: lit("callee_identifier_name_long_enough_for_heap_buf".to_string(), 100), args: args, nargs: 2 })
}
fn root(e: ref Expr) -> i64 {
    match e {
        Lit(n) => n.val,
        Call(c) => c.nargs,
    }
}
fn main() {
    let mut total: i64 = 0;
    let mut i: i64 = 0;
    while i < 50 {
        let e = mk();
        total = total + root(e);
        i = i + 1;
    }
    println(total);
}
"#,
        &["100"],
        "vec_of_struct_with_shared_field_drop",
    );
}

#[test]
fn asan_vec_of_struct_shared_and_option_field_consumed_no_leak() {
    // B-2026-06-19 (phase-12 parser slice 2a) — the CONSUME peer of
    // `asan_vec_of_struct_with_shared_field_drop_no_leak`, and the shape the
    // self-hosted parser's renderer / later phases actually exercise:
    // `Arg { label: Option[String], value: Expr }` elements of a
    // `Vec[Arg]` are MOVED OUT (by-value match + `for a in args` +
    // destructure) and consumed — the `Option[String]` label is read then
    // dropped, the shared `value` recursively consumed. Here the consumer
    // frees the label (so the Option[String] is leak-free on THIS path,
    // unlike the pure-drop path where it's a documented residual), and the
    // shared-value recursion frees each box AND its inner `LitNode.name`
    // String (the force-synth pre-pass in `emit_nested_struct_shared_rc_decs`
    // makes the rc-dec dispatch to the recursive `__karac_rc_drop_Expr`
    // rather than inline-`free`ing the box and stranding the name). Looped;
    // all Strings >=36 bytes for LSan visibility.
    assert_clean_asan_run(
        r#"
shared enum Expr { Lit(LitNode), Call(CallNode) }
struct LitNode { name: String, val: i64 }
struct Arg { label: Option[String], value: Expr }
struct CallNode { callee: Expr, args: Vec[Arg], nargs: i64 }
fn lit(s: String, v: i64) -> Expr { Expr.Lit(LitNode { name: s, val: v }) }
fn mk() -> Expr {
    let mut args: Vec[Arg] = Vec.new();
    args.push(Arg { label: Some("first_argument_label_long_enough_to_force_heap".to_string()), value: lit("first_arg_value_identifier_long_enough_for_heap".to_string(), 1) });
    args.push(Arg { label: None, value: lit("second_arg_value_identifier_long_enough_for_heap".to_string(), 2) });
    Expr.Call(CallNode { callee: lit("callee_identifier_name_long_enough_for_heap_buf".to_string(), 100), args: args, nargs: 2 })
}
fn consume(e: Expr) -> i64 {
    match e {
        Lit(n) => n.val,
        Call(c) => {
            let CallNode { callee, args, nargs } = c;
            let mut acc = consume(callee) + nargs;
            for a in args {
                let Arg { label, value } = a;
                match label {
                    Some(l) => { if l.len() > 0 { acc = acc + 1000; } }
                    None => {}
                }
                acc = acc + consume(value);
            }
            acc
        }
    }
}
fn main() {
    let mut total: i64 = 0;
    let mut i: i64 = 0;
    while i < 50 {
        let e = mk();
        total = total + consume(e);
        i = i + 1;
    }
    println(total);
}
"#,
        &["55250"],
        "vec_of_struct_shared_and_option_field_consumed",
    );
}

// ── `Vec[v; n]` repeat literal — heap fill lifecycle ──────────────
// `Vec[v; n]` / `vec![v; n]` allocate a heap buffer via the shared
// `build_vec_filled` (same path as `Vec.filled(n, v)`). This pins that the
// resulting `{ptr, len, cap}` Vec is dropped exactly once: a fill, a
// push-after-fill (grow-realloc reclaims the original buffer), an index
// read, and a `vec![v; n]` with a larger payload all in one scope. On
// Linux CI LSan additionally catches a missed free of the fill buffer.
#[test]
fn asan_vec_repeat_literal_fill_push_index() {
    assert_clean_asan_run(
        r#"
fn main() {
    let mut g: Vec[i64] = Vec[5; 8];
    g.push(1);
    g.push(2);
    let mut total = 0;
    for x in g { total = total + x; }
    println(total);
    let n = 100;
    let r: Vec[i64] = vec![3; n];
    println(r.len());
    println(r[99]);
}
"#,
        &["43", "100", "3"],
        "vec_repeat_literal_fill_push_index",
    );
}

/// `Vec.filled(rows, Vec.filled(cols, x))` — the canonical 2D DP table.
/// Before the per-slot deep-clone fix, codegen bit-copied the inner heap
/// Vec into every row, so all rows aliased ONE backing buffer: writes to
/// one row corrupted the others and the N rows N-fold-freed the same buffer
/// on drop (AOT SIGTRAP). ASAN must confirm each row owns a distinct buffer
/// — no aliasing (rows independent), no double-free, no leak. Inner rows
/// carry 5 i64s (40 bytes > the LSan short-alloc floor) so a wrongly-shared
/// or wrongly-freed row buffer is caught.
#[test]
fn asan_vec_filled_2d_rows_are_independent_buffers() {
    assert_clean_asan_run(
        r#"
fn main() {
    let rows = 4i64;
    let cols = 5i64;
    let mut dp: Vec[Vec[i64]] = Vec.filled(rows, Vec.filled(cols, 0i64));
    let mut i = 0i64;
    while i < rows {
        let mut j = 0i64;
        while j < cols {
            dp[i][j] = i * 10i64 + j;
            j = j + 1i64;
        }
        i = i + 1;
    }
    println(dp[0][0]);
    println(dp[3][4]);
    println(dp[2][3]);
}
"#,
        &["0", "34", "23"],
        "vec_filled_2d_rows_independent",
    );
}

#[test]
fn asan_letbound_struct_vec_shared_elem_moved_into_enum_ctor_no_uaf() {
    // B-2026-07-10-1: a LET-BOUND struct with a `Vec[<enum owning a shared
    // field>]` field AND an `Option[shared]` field, MOVED whole into a
    // shared-enum ctor (`let b = Block { stmts, tail, span }; Expr.Blk(b)`).
    // The struct's combined drop (`__karac_vec_elem_full_drop_<S>`) frees the
    // Vec buffer under a `cap > 0` guard but runs a SEPARATE, LEN-driven
    // per-element rc-dec walk for the shared-bearing elements. The whole-struct
    // move-suppression (`zero_struct_move_caps`) zeroed only the Vec `cap`, so
    // that len-driven walk still rc-dec'd the moved-out elements' shared handles
    // — which the boxed enum payload co-owns — a use-after-free (the self-hosted
    // parser's `{ a; }` statement-expr read back as a garbage `Error` node under
    // AOT while correct under interp). Fix: `zero_struct_move_caps` also zeroes
    // the Vec `len`. Looped x20 under the LSan gate; the shared-elem count must
    // survive the move so the reader sees the real payload.
    assert_clean_asan_run(
        r#"
struct IdentExpr { name: String, val: i64 }
shared enum Expr { Ident(IdentExpr), Blk(Block) }
struct ExprStmt { expr: Expr }
enum Stmt { Exp(ExprStmt) }
struct Block { stmts: Vec[Stmt], tail: Option[Expr], span: i64 }
fn render_expr(e: Expr) -> i64 {
    match e {
        Ident(n) => n.val,
        Blk(b) => {
            let Block { stmts, tail, span } = b;
            let mut acc = span;
            for s in stmts { match s { Exp(n) => { let ExprStmt { expr } = n; acc = acc + render_expr(expr); } } }
            acc
        }
    }
}
fn mk_expr() -> Expr {
    let mut stmts: Vec[Stmt] = Vec.new();
    stmts.push(Stmt.Exp(ExprStmt { expr: Expr.Ident(IdentExpr { name: "aaaaaaaaaaaaaaaaaaaaaaaa".to_string(), val: 7 }) }));
    stmts.push(Stmt.Exp(ExprStmt { expr: Expr.Ident(IdentExpr { name: "bbbbbbbbbbbbbbbbbbbbbbbb".to_string(), val: 11 }) }));
    let block = Block { stmts: stmts, tail: None, span: 100 };
    Expr.Blk(block)
}
fn main() {
    let mut total: i64 = 0;
    let mut i: i64 = 0;
    while i < 20 { total = total + render_expr(mk_expr()); i = i + 1; }
    println(total);
}
"#,
        &["2360"],
        "letbound_struct_vec_shared_elem_moved_into_enum_ctor",
    );
}

#[test]
fn asan_shared_enum_view_field_access_move_vec_shared_no_double_free() {
    // B-2026-07-09-12 (clone-on-extract half — FIELD-ACCESS-MOVE form): a
    // `Vec[shared]` field moved out of a shared-enum-payload VIEW by field
    // access into a `let` (`let a = c.args`), NOT a destructure. The moved-out
    // Vec aliased the box's buffer + element handles; the leaf's per-element
    // rc-dec drop AND the box's rc-drop both freed each element box (SEGV /
    // heap corruption). `deep_copy_owned_struct_param_field_move`, extended to
    // view sources, deep-copies the buffer and rc-INCs each element. The bare-
    // shared and Option[shared] field-move forms are already balanced by
    // `compile_field_access`'s read-inc. Looped x20 under LSan.
    assert_clean_asan_run(
        r#"
shared enum Expr { Lit(i64), Call(CallNode) }
struct CallNode { args: Vec[Expr], tag: i64 }
fn ev(e: Expr) -> i64 {
    match e {
        Lit(n) => n,
        Call(c) => {
            let a = c.args;
            let mut acc = c.tag;
            for x in a { acc = acc + ev(x); }
            acc
        }
    }
}
fn main() {
    let mut total: i64 = 0;
    let mut i: i64 = 0;
    while i < 20 {
        let mut v: Vec[Expr] = Vec.new();
        v.push(Expr.Lit(5)); v.push(Expr.Lit(6)); v.push(Expr.Lit(7));
        total = total + ev(Expr.Call(CallNode { args: v, tag: 100 }));
        i = i + 1;
    }
    println(total);
}
"#,
        &["2360"],
        "shared_enum_view_field_access_move_vec_shared",
    );
}

#[test]
fn asan_shared_enum_view_destructure_vec_shared_and_option_shared_no_double_free() {
    // B-2026-07-09-12 (clone-on-extract half — Vec[shared] + Option[shared]
    // leaves): the AST sequence-child (`CallNode { args: Vec[Expr] }`) and
    // optional-child (`NodeData { tail: Option[Expr] }`) shapes, both moved out
    // of a shared-enum-payload VIEW via destructure and consumed. Pre-fix the
    // extracted `args` elements / `tail` `Some` box aliased the RC box's heap,
    // so the leaf's consume AND the box's rc-drop double-freed them. The
    // clone-on-extract fix deep-copies the Vec buffer + rc-INCs each element
    // (`rc_inc_vec_shared_elements`) and rc-INCs the `Some` box
    // (`deep_copy_option_inline_payload_in_place` + `track_rc_option_var`).
    // Looped x20 under the LSan gate.
    assert_clean_asan_run(
        r#"
shared enum Expr { Lit(i64), Call(CallNode), Node(NodeData) }
struct CallNode { args: Vec[Expr], tag: i64 }
struct NodeData { val: i64, tail: Option[Expr] }
fn ev(e: Expr) -> i64 {
    match e {
        Lit(n) => n,
        Call(c) => {
            let CallNode { args, tag } = c;
            let mut acc = tag;
            for a in args { acc = acc + ev(a); }
            acc
        }
        Node(nd) => {
            let NodeData { val, tail } = nd;
            match tail { Some(t) => val + ev(t), None => val }
        }
    }
}
fn main() {
    let mut total: i64 = 0;
    let mut i: i64 = 0;
    while i < 20 {
        let mut v: Vec[Expr] = Vec.new();
        v.push(Expr.Lit(5)); v.push(Expr.Lit(6)); v.push(Expr.Lit(7));
        let leaf = Expr.Node(NodeData { val: 9, tail: None });
        v.push(Expr.Node(NodeData { val: 3, tail: Some(leaf) }));
        let call = Expr.Call(CallNode { args: v, tag: 100 });
        total = total + ev(call);
        i = i + 1;
    }
    println(total);
}
"#,
        &["2600"],
        "shared_enum_view_destructure_vec_shared_and_option_shared",
    );
}

#[test]
fn asan_getmove_match_arm_push_consume_no_double_free() {
    // Slice 3s: the NON-tail consume — the arm body pushes the bound
    // payload into a Vec. The move-detector must classify a method-arg
    // occurrence as a move (the Vec bit-copies + suppresses, so without
    // the clone the Vec and the map both own the same buffer).
    assert_clean_asan_run(
        r#"
fn main() {
    let mut out: Vec[String] = Vec.new();
    let mut i = 0;
    while i < 3 {
        let mut m: Map[i64, String] = Map.new();
        m.insert(i, f"map string payload padded beyond thirty-six bytes {i}");
        match m.get(i) {
            Some(x) => { out.push(x); },
            None => {},
        }
        i = i + 1;
    };
    println(out.len());
}
"#,
        &["3"],
        "getmove_match_arm_push_consume_no_double_free",
    );
}

#[test]
fn asan_structpat_vec_field_destructure_no_double_free() {
    // Slice 3t: a `Vec[String]` FIELD destructured out — the consumed
    // field's cap-zero must recurse correctly (the box walk would
    // otherwise free the Vec's buffer AND its element strings that the
    // binding now owns).
    assert_clean_asan_run(
        r#"
struct Bag { items: Vec[String], id: i64 }
fn main() {
    let mut i = 0;
    while i < 3 {
        let mut v: Vec[String] = Vec.new();
        v.push(f"bag item payload padded beyond thirty-six bytes {i}");
        let o: Option[Bag] = Some(Bag { items: v, id: i });
        match o {
            Some(Bag { items, id }) => { println((items[0].len() as i64) + id); },
            None => { println("missing"); },
        }
        i = i + 1;
    };
}
"#,
        &["49", "50", "51"],
        "structpat_vec_field_destructure_no_double_free",
    );
}

#[test]
fn asan_boxelem_vec_option_wide_struct_scope_drop_no_leak() {
    // Slice 3u: `Vec[Option[Holder]]` — Holder (4 words) exceeds
    // Option's 3-word inline area, so each Some element carries a heap
    // BOX. The 3p element-drop gate admitted only inline String/Vec
    // payloads; boxed elements leaked box + interior (336 bytes / 3
    // iterations pre-fix). The extended `karac_drop_Option_Holder`
    // walks the box (inner struct drop + free) on the Some tag.
    assert_clean_asan_run(
        r#"
struct Holder { name: String, id: i64 }
fn build(n: i64) -> Vec[Option[Holder]] {
    let mut v: Vec[Option[Holder]] = Vec.new();
    v.push(Some(Holder { name: f"holder payload padded beyond thirty-six bytes {n}", id: n }));
    v.push(None);
    v
}
fn main() {
    let mut i = 0;
    while i < 3 {
        let v = build(i);
        println(v.len());
        i = i + 1;
    };
}
"#,
        &["2", "2", "2"],
        "boxelem_vec_option_wide_struct_scope_drop_no_leak",
    );
}

#[test]
fn asan_boxelem_push_boxed_binding_no_double_free() {
    // Slice 3u: `let o = Some(Holder{...}); v.push(o)` — the moved
    // BOXED binding's `BoxedEnumDrop` must disarm (tag=None store, the
    // boxed sibling of the inline cap-zero) or it double-frees against
    // the newly-armed element drop.
    assert_clean_asan_run(
        r#"
struct Holder { name: String, id: i64 }
fn main() {
    let mut i = 0;
    while i < 3 {
        let mut v: Vec[Option[Holder]] = Vec.new();
        let o: Option[Holder] = Some(Holder { name: f"holder payload padded beyond thirty-six bytes {i}", id: i });
        v.push(o);
        println(v.len());
        i = i + 1;
    };
}
"#,
        &["1", "1", "1"],
        "boxelem_push_boxed_binding_no_double_free",
    );
}

#[test]
fn asan_tail3v_vec_of_vecdeque_scope_drop_no_leak() {
    // Slice 3v: `Vec[VecDeque[String]]` — VecDeque shares Vec's linear
    // {ptr,len,cap} layout (memmove push_front, not a ring), so the
    // recursive element drop is exact; the gates simply never admitted
    // the VecDeque head (strings leaked, one per element, pre-fix).
    assert_clean_asan_run(
        r#"
fn build(n: i64) -> Vec[VecDeque[String]] {
    let mut d: VecDeque[String] = VecDeque.new();
    d.push_back(f"deque payload padded beyond thirty-six bytes {n}");
    let mut v: Vec[VecDeque[String]] = Vec.new();
    v.push(d);
    v
}
fn main() {
    let mut i = 0;
    while i < 3 {
        let v = build(i);
        println(v.len());
        i = i + 1;
    };
}
"#,
        &["1", "1", "1"],
        "tail3v_vec_of_vecdeque_scope_drop_no_leak",
    );
}

#[test]
fn asan_parvec_deep_nested_slot() {
    // B-2026-07-02-4: `Vec[Vec[Vec[i64]]]` slot + index-read binding
    // (the 16-byte w1 repro).
    assert_clean_asan_run(
        r#"
fn main() {
    let mut v: Vec[Vec[Vec[i64]]] = Vec.new();
    let mut a: Vec[Vec[i64]] = Vec.new();
    a.push(Vec[2]);
    v.push(a);
    let first = v[0].clone();
    println(first.len());
}
"#,
        &["1"],
        "parvec_deep_nested_slot",
    );
}

#[test]
fn asan_vecvec_heap_element_consumed_by_value_no_double_free() {
    // B-2026-07-11-24 (borrow-elision leg): a `let r = grid[i]` inner
    // `Vec[String]` read out of a `Vec[Vec[String]]`, whose element `r[j]` is
    // then passed BY VALUE to a consuming callee (`take(r[j])`). The
    // borrow-elision pre-pass used to treat `r[j]` as a read and borrow-elide
    // `r` into a shallow alias of the container's buffer; `take`'s owned
    // `String` param then freed a buffer the container still owned →
    // double-free. The element-copyability oracle now recognises the
    // heap-element consume and forces the deep clone, so `r` owns an
    // independent buffer freed exactly once. A trivially-copyable element
    // (`Vec[Vec[i64]]`, `acc + m[i][j]`) stays borrow-elided — covered by the
    // `borrow_elision_elides_read_only_vecvec_index_binding` codegen test.
    assert_clean_asan_run(
        r#"
fn take(s: String) -> i64 { s.len() as i64 }
fn main() {
    let mut grid: Vec[Vec[String]] = Vec.new();
    let mut row: Vec[String] = Vec.new();
    row.push("hello".to_string());
    row.push("world".to_string());
    grid.push(row);
    let mut total = 0;
    let mut i = 0;
    while i < grid.len() {
        let r = grid[i].clone();
        let mut j = 0;
        while j < r.len() { total = total + take(r[j].clone()); j = j + 1; }
        i = i + 1;
    }
    println(total);
}
"#,
        &["10"], // len("hello") + len("world") = 5 + 5
        "vecvec_heap_element_consumed_by_value_no_double_free",
    );
}

#[test]
fn asan_vecvec_option_shared_scope_exit_drop_no_leak() {
    // B-2026-07-11-29 layer 4 (leak): dropping a `Vec[Vec[Option[shared]]]`
    // local only freed the inner Vec BUFFERS one level deep and treated
    // their `Option[shared]` elements as opaque, leaking every shared node
    // inside (LSan). `te_recursive_drop_fully_supported` now accepts an
    // `Option[shared T]` payload, so the outer drop routes to the
    // strictly-recursive `emit_vec_drop_fn` (→ `emit_option_drop_fn`, which
    // tag-guards and rc-decs the boxed shared payload) instead of the
    // one-level buffer-only fast path.
    assert_clean_asan_run(
        r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn count_nodes(node: Option[Node]) -> i64 {
    match node { None => 0, Some(n) => 1 + count_nodes(n.left) + count_nodes(n.right) }
}
fn mk() -> Vec[Vec[Option[Node]]] {
    let mut shapes: Vec[Vec[Option[Node]]] = Vec.new();
    let mut base: Vec[Option[Node]] = Vec.new();
    base.push(Some(Node { val: 1, left: Some(Node { val: 2, left: None, right: None }), right: None }));
    shapes.push(base);
    shapes
}
fn main() {
    let shapes = mk();
    let lefts = shapes[0].clone();
    println(count_nodes(lefts[0].clone()));
}
"#,
        &["2"], // the shared node + its child, dropped exactly once
        "vecvec_option_shared_scope_exit_drop_no_leak",
    );
}

#[test]
fn asan_let_bound_vec_option_shared_reused_no_uaf() {
    // B-2026-07-11-29 (`let s = v[i]` reuse leg): binding a
    // `Vec[Option[shared]]` element (`let s = src[0]`) retain-clones the
    // inner (`karac_clone_Option_Node`) but the binding was left UNREGISTERED
    // in `var_option_shared_heap`, so subsequent by-value passes got no
    // caller-retains arg-inc and the callee's exit-dec over-decremented on
    // the second pass → use-after-free. Case (f) in stmts.rs now registers
    // the `let s = v[i]` binding into the caller-retains model.
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
    let mut src: Vec[Option[Node]] = Vec.new();
    src.push(Some(Node { val: 1, left: Some(Node { val: 2, left: None, right: None }), right: None }));
    let s = src[0].clone();
    let l0 = clone_offset(s, 10);
    let l1 = clone_offset(s, 20);
    println(count_nodes(l0) + count_nodes(l1));
}
"#,
        &["4"],
        "let_bound_vec_option_shared_reused_no_uaf",
    );
}

#[test]
fn asan_push_bound_option_shared_binding_no_uaf() {
    // B-2026-07-11-29 (push-move leg): pushing a tracked `Option[shared]`
    // BINDING into a `Vec[Option[shared]]` (`out.push(orig)`) co-owns the
    // node under reference semantics, but push (a builtin) never emitted the
    // caller-retains inc that consuming CALL sites do, while the source
    // binding's scope-exit `RcDecOption` still fired — freeing the node while
    // the container still pointed at it (use-after-free). The push arm now
    // emits `share_option_shared_ref_for_arg` for the moved binding.
    assert_clean_asan_run(
        r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn count_nodes(node: Option[Node]) -> i64 {
    match node { None => 0, Some(n) => 1 + count_nodes(n.left) + count_nodes(n.right) }
}
fn main() {
    let mut out: Vec[Option[Node]] = Vec.new();
    let orig = Some(Node { val: 1, left: Some(Node { val: 2, left: None, right: None }), right: None });
    out.push(orig);
    println(count_nodes(out[0].clone()));
}
"#,
        &["2"],
        "push_bound_option_shared_binding_no_uaf",
    );
}

#[test]
fn asan_generic_container_method_push_no_leak() {
    // B-2026-07-11-35 (push leg): a GENERIC container `Box[T] { xs: Vec[T] }`
    // built via a generic constructor (`Box.new()`) and filled through a
    // generic method (`fn add(mut ref self, x: T) { self.xs.push(x) }`) with
    // NON-COPY (String) elements. Two coupled defects: (1) the mono param
    // prologue registered `x: T` off the bare `T`, so `self.xs.push(x)` MOVED
    // the caller's buffer (garbage reads — the correctness leg, pinned in
    // `tests/codegen.rs`); (2) once the push deep-copies (the fix), the
    // element buffers leaked because the struct DROP was synthesized ONCE per
    // struct NAME and resolved the `Vec[T]` field from bare `T`, never
    // draining the concrete `Vec[String]`. Per-monomorph struct-drop synthesis
    // (`__karac_drop_struct_Box$String`, distinct from `Box$i64`) now drains
    // each element. Loops so any per-iteration leak accumulates for LSan, and
    // coexists `Box[String]` with `Box[i64]` so the String drain never runs
    // over the i64 Vec (a name-shared drop would `free` each i64 as a bogus
    // `{ptr,len,cap}` — a heap-buffer-overflow / invalid-free, not just a leak).
    assert_clean_asan_run(
        r#"
struct Box[T] { xs: Vec[T] }
impl[T] Box[T] {
    fn new() -> Box[T] { Box { xs: Vec.new() } }
    fn add(mut ref self, x: T) { self.xs.push(x); }
    fn at(ref self, i: i64) -> T { self.xs[i] }
    fn size(ref self) -> i64 { self.xs.len() }
}
fn main() {
    let mut r: i64 = 0;
    while r < 3 {
        let mut s: Box[String] = Box.new();
        s.add(f"row-{r}-aaaaaa");
        s.add(f"row-{r}-bbbbbb");
        s.add(f"row-{r}-cccccc");
        let mut n: Box[i64] = Box.new();
        n.add(r * 10);
        n.add(r * 10 + 1);
        println(s.at(0));
        println(s.at(2));
        println(f"{n.at(1)} {s.size()} {n.size()}");
        r = r + 1;
    }
}
"#,
        &[
            "row-0-aaaaaa",
            "row-0-cccccc",
            "1 3 2",
            "row-1-aaaaaa",
            "row-1-cccccc",
            "11 3 2",
            "row-2-aaaaaa",
            "row-2-cccccc",
            "21 3 2",
        ],
        "generic_container_method_push_no_leak",
    );
}

#[test]
fn asan_field_read_option_shared_push_no_leak_or_uaf() {
    // B-2026-07-12-4: pushing a FIELD-READ `Option[shared]` (`stack.push(
    // n.left)`) onto a `Vec[Option[shared]]` and dropping the Vec with
    // residual elements is aliasing co-ownership — the pushed handle stays
    // live at its source node `n`. `Vec.push` is a builtin that bypassed the
    // generic method-arg retain, so the field read went un-inc'd: the Vec's
    // per-element drop AND `n`'s own drop both released the node — a
    // use-after-free (read of a freed 32-byte block; a leak before the Vec
    // per-element drop began releasing residuals). The fix inc's the
    // field-read inner on push (`share_option_shared_field_ref_for_arg`).
    // Looped to amplify any per-iteration imbalance well past noise; reads
    // the pushed values back so a wrong-node miscompile would also surface.
    assert_clean_asan_run(
        r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 200 {
        let root = Some(Node {
            val: 1,
            left: Some(Node { val: 2, left: None, right: None }),
            right: Some(Node { val: 3, left: None, right: None }),
        });
        let mut stack: Vec[Option[Node]] = Vec.new();
        match root {
            None => {}
            Some(n) => { stack.push(n.left); stack.push(n.right); }
        }
        for item in stack {
            match item { None => {} Some(node) => { total = total + node.val; } }
        }
        i = i + 1;
    }
    println(f"{total}");
}
"#,
        &["1000"],
        "field_read_option_shared_push_no_leak_or_uaf",
    );
}

#[test]
fn asan_rc_elide_is_mirror_symmetric_walk_preserved_no_leak() {
    // Positive control — the #101 win. With the flag on, `is_symmetric[root]`
    // and `is_mirror[a,b]` are BOTH elided (verified via KARAC_RC_ELIDE_DEBUG):
    // the two hot, bool-returning, scrutinee-only walkers whose payloads are
    // used ONLY via field projections (`an.left`, `n.right`) into `ref`
    // positions — never moved out. Must stay leak-free: a hand-built
    // symmetric tree, is_symmetric 200x → 200 trues.
    assert_clean_asan_run(
        r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn is_mirror(a: Option[Node], b: Option[Node]) -> bool {
    match a {
        None => { match b { None => true, Some(_) => false } }
        Some(an) => { match b { None => false, Some(bn) => an.val == bn.val and is_mirror(an.left, bn.right) and is_mirror(an.right, bn.left) } }
    }
}
fn is_symmetric(root: Option[Node]) -> bool { match root { None => true, Some(n) => is_mirror(n.left, n.right) } }
fn main() {
    let leftsub = Some(Node { val: 2i64, left: Some(Node { val: 3i64, left: None, right: None }), right: None });
    let rightsub = Some(Node { val: 2i64, left: None, right: Some(Node { val: 3i64, left: None, right: None }) });
    let root = Some(Node { val: 1i64, left: leftsub, right: rightsub });
    let mut pool: Vec[Option[Node]] = Vec.new();
    pool.push(root);
    let mut t: i64 = 0i64;
    let mut rep: i64 = 0i64;
    while rep < 200i64 { let sym = is_symmetric(pool[0i64].clone()); t = t + (if sym { 1i64 } else { 0i64 }); rep = rep + 1i64; }
    println(f"{t}")
}
"#,
        &["200"],
        "rc_elide_is_mirror_symmetric_walk_preserved_no_leak",
    );
}

#[test]
fn asan_vec_get_unwrap_heap_element_no_double_free() {
    // B-2026-07-14-11: `let row = g.get(i).unwrap()` / `.first()`/`.last()`
    // on a `Vec[Vec[T]]` / `Vec[String]`. `Vec.get` returns `Option[ref elem]`
    // (a borrow that packs the element VALUE), so the unwrapped binding
    // shallow-aliases the container's element buffer. It is now registered
    // for method dispatch AND treated as borrow-elided (no owned scope-exit
    // drop) — the container stays the sole owner. Without the elision both
    // the binding's drop and the container's per-element drain would free the
    // same buffer. Covers `Vec` and `String` elements across get/first/last;
    // must be double-free-clean.
    assert_clean_asan_run(
        r#"
fn main() {
    let g: Vec[Vec[i64]] = [[1, 2, 3], [4, 5, 6], [7, 8]];
    let row = g.get(1).unwrap();
    println(row.len());
    let f = g.first().unwrap();
    println(f.len());
    let l = g.last().unwrap();
    println(l.len());

    let words: Vec[String] = ["hello", "world", "kara"];
    let w = words.get(2).unwrap();
    println(w.len());
    let fw = words.first().unwrap();
    println(fw.len());
}
"#,
        &["3", "3", "2", "4", "5"],
        "vec_get_unwrap_heap_element_no_double_free",
    );
}

#[test]
fn asan_vec_get_unwrap_struct_with_heap_field_no_double_free() {
    // B-2026-07-14-16 (struct leg): `let p = people.get(i).unwrap()` on a
    // `Vec[Struct]` whose Struct owns heap (a `String` field). `Vec.get`
    // returns `Option[ref Struct]` packing the element VALUE, so `p`'s
    // `String` field shallow-aliases the container's element buffer;
    // registering an owned struct-drop for `p` freed it a second time (the
    // container's per-element drain already frees it → double-free). `p` is
    // now recognised as a borrow alias (`is_borrowed_vec_get_unwrap_struct`)
    // and takes NO owned drop — the container stays the sole owner. Covers
    // get + first; must be double-free-clean and print the aliased fields
    // correctly.
    assert_clean_asan_run(
        r#"
struct Person { name: String, age: i64 }
fn main() {
    let mut people: Vec[Person] = Vec.new();
    let mut n1 = String.from("");
    n1.push_str("Alice");
    people.push(Person { name: n1, age: 30 });
    let mut n2 = String.from("");
    n2.push_str("Bob");
    people.push(Person { name: n2, age: 25 });
    let p = people.get(0).unwrap();
    println(p.name);
    println(p.age);
    let f = people.first().unwrap();
    println(f.age);
}
"#,
        &["Alice", "30", "30"],
        "vec_get_unwrap_struct_with_heap_field_no_double_free",
    );
}

#[test]
fn asan_vec_shared_whole_variable_reassign_no_leak() {
    // B-2026-07-12-30: overwriting a `Vec[shared]` local (`current = next`,
    // the BFS-worklist idiom) freed the OLD buffer but skipped its
    // per-element rc-release, stranding every shared node the overwritten Vec
    // held. The Assign overwrite now runs the same element-releasing walk the
    // scope-exit `FreeVecBuffer` cleanup does. A single `current = next` over a
    // `Vec[Node]` holding one shared node must be leak-clean.
    assert_clean_asan_run(
        r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn main() {
    let root = Some(Node { val: 1, left: Some(Node { val: 2, left: None, right: None }), right: None });
    let mut current: Vec[Node] = Vec.new();
    match root { None => {} Some(n) => { current.push(n); } }
    let mut next: Vec[Node] = Vec.new();
    match current[0].left { None => {} Some(l) => { next.push(l); } }
    current = next;
    println(f"len: {current.len()}");
}
"#,
        &["len: 1"],
        "vec_shared_whole_variable_reassign_no_leak",
    );
}

#[test]
fn asan_vec_shared_bfs_level_order_loop_no_leak() {
    // The real kata #102 shape: a `while` BFS that rebuilds `next` each level
    // and does `current = next`. Every level's overwrite must release the
    // prior level's shared nodes. A 3-node tree traversed to completion,
    // leak-clean, summing all values.
    assert_clean_asan_run(
        r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn main() {
    let root = Node { val: 1, left: Some(Node { val: 2, left: None, right: None }), right: Some(Node { val: 3, left: None, right: None }) };
    let mut current: Vec[Node] = Vec.new();
    current.push(root);
    let mut total = 0;
    while current.len() > 0 {
        let mut next: Vec[Node] = Vec.new();
        let mut i = 0;
        while i < current.len() {
            total = total + current[i].val;
            match current[i].left { None => {} Some(l) => { next.push(l); } }
            match current[i].right { None => {} Some(r) => { next.push(r); } }
            i = i + 1;
        }
        current = next;
    }
    println(f"total: {total}");
}
"#,
        &["total: 6"],
        "vec_shared_bfs_level_order_loop_no_leak",
    );
}

#[test]
fn asan_vec_shared_pop_match_releases_temp_ref_no_leak() {
    // B-2026-07-15-1: `Vec.pop` TRANSFERS the vec's +1 ref into the
    // returned `Option[shared]` temp; the match payload binding takes its
    // own +1 (balanced by the arm-exit dec), so without a release of the
    // TEMP's transferred ref every popped shared element stranded one
    // count. Covers the pop-reassign loop (the kata #105 iterative
    // stack-builder shape), the `Some(_)` wildcard arm (no binding — the
    // enum-inst-type fallback resolves the payload), a moved-out payload
    // (must NOT double-free), and a read-only bind.
    assert_clean_asan_run(
        r#"
shared struct Node { val: i64, mut next: Option[Node] }
fn main() {
    let mut stack: Vec[Node] = Vec.new();
    let mut i: i64 = 0;
    while i < 50 {
        stack.push(Node { val: i, next: None });
        i = i + 1;
    }
    let mut node = Node { val: 0 - 1, next: None };
    let mut sum: i64 = 0;
    while stack.len() > 0 {
        match stack.pop() {
            None => {}
            Some(popped) => { node = popped; }
        }
        sum = sum + node.val;
    }
    println(sum);
    let mut s2: Vec[Node] = Vec.new();
    s2.push(Node { val: 5, next: None });
    match s2.pop() { None => {} Some(_) => { println("popped"); } }
    let mut s4: Vec[Node] = Vec.new();
    s4.push(Node { val: 42, next: None });
    let mut kept: Vec[Node] = Vec.new();
    match s4.pop() { None => {} Some(p) => { kept.push(p); } }
    println(kept[0].val);
    let mut s5: Vec[Node] = Vec.new();
    s5.push(Node { val: 9, next: None });
    match s5.pop() { None => {} Some(p) => { println(p.val); } }
}
"#,
        &["1225", "popped", "42", "9"],
        "vec_shared_pop_match_releases_temp_ref_no_leak",
    );
}

#[test]
fn asan_write_only_vec_shared_single_push_no_leak() {
    // B-2026-07-15-2: a `Vec[shared]` local with exactly ONE pushed
    // element and NO read after the push leaked the element + the
    // realloc'd buffer — auto-par branched the push (nothing read `v`
    // afterward, so the captured-mutation gate saw no outside read), and
    // the branch-local realloc never propagated back to the parent's
    // header. The parent's scope-exit drop is an implicit read the
    // analyzer now models via `captured_container_mutations`: any
    // captured mutation of a heap-owning container local compiles the
    // group sequentially. The linked payload deepens the walk.
    assert_clean_asan_run(
        r#"
shared struct Item { val: i64, tag: i64 }
shared struct Node { val: i64, mut next: Option[Node] }
fn main() {
    let mut v: Vec[Item] = Vec.new();
    v.push(Item { val: 1, tag: 2 });
    let mut w: Vec[Node] = Vec.new();
    w.push(Node { val: 1, next: Some(Node { val: 2, next: None }) });
    println("done");
}
"#,
        &["done"],
        "write_only_vec_shared_single_push_no_leak",
    );
}

/// B-2026-08-07-11 leg (c) — the `Vec`-ELEMENT peer, and the only member of
/// this family that leaks at the DEFAULT `-O2`.
///
/// `vec_elem_agg_drop_for_type_expr`'s `Option` arm asked three questions of
/// the PAYLOAD — is it an inline `{ptr,len,cap}` overlay, a shared handle, a
/// droppable struct/enum — and a boxed scalar answers no to all three, so a
/// `Vec[Option[Option[i64]]]` element got no per-element drop at all. Same
/// missing-owner shape 31768650 fixed for a struct FIELD, one container
/// over, and it takes the same admission test
/// (`option_payload_boxed_envelope_only`).
///
/// THE SINGLE-BOX ARM IS THE DIAGNOSIS, not just a control. `v2` holds
/// `Option[Option[i64]]` — exactly one envelope, no chain anywhere — and it
/// leaked, which is what distinguishes a missing OWNER from a missing chain
/// walk. Legs (a) and (b) of this row both had to be fixed in that order too:
/// give the outermost envelope an owner first, and the chain follows.
///
/// AND IT IS NOT `-O0`-ONLY, which every other shape in this family was.
/// A `Vec` keeps its buffer live past the point LLVM can fold a frame-local
/// envelope away, so pre-fix this program lost 5,120 B definitely plus
/// 3,840 B indirectly at the DEFAULT opt level (6,400 + 5,120 at `-O0`).
/// An ordinary `karac build` leaks here. That is why this fixture carries a
/// real floor while its two siblings are unfloored — at `-O2` it performs
/// 766 allocations rather than the baseline 6.
///
/// ARMS: `v2` single box; `v3` a chain; `v4` two envelopes below the first;
/// `vs` a `Vec[Option[String]]` inline-overlay control that the existing
/// `option_payload_inline_recursive_drop_ok` route already covered and that
/// must not gain a second owner; `vp` a `Vec[Option[i64]]` heapless control
/// with no box at all; `vu` a pushed element never read back. The
/// `Some(None)` and `None` elements in `v2` are the guard controls — each
/// leaves the payload words holding a value rather than a pointer.
///
/// The expected value is COMPUTED: four arms subtract the opaque
/// `env.args().len()` seed back out to leave `i` and the String arm
/// contributes 1 — `4i + 1` per iteration, so `4 * (0+…+39) + 40 = 3160`.
#[test]
fn asan_vec_element_boxed_enum_envelope_owned() {
    assert_clean_asan_run_min_allocs(
        r#"
fn mkstr(n: i64) -> String {
    let mut s: String = String.new();
    s.push_str("envelope-");
    s.push_str(n.to_string());
    s.push_str("-padding-to-force-heap");
    s
}
fn main() {
    let n = env.args().len() as i64;
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 40 {
        let mut v2: Vec[Option[Option[i64]]] = Vec.new();
        v2.push(Option.Some(Option.Some(n + i)));
        v2.push(Option.Some(Option.None));
        v2.push(Option.None);
        match v2[0] { Option.Some(Option.Some(x)) => { acc = acc + x - n; } _ => { acc = acc - 1; } }

        let a: Option[Option[Option[i64]]] = Option.Some(Option.Some(Option.Some(n + i)));
        let mut v3: Vec[Option[Option[Option[i64]]]] = Vec.new();
        v3.push(a);
        match v3[0] { Option.Some(Option.Some(Option.Some(x))) => { acc = acc + x - n; } _ => { acc = acc - 1; } }

        let mut v4: Vec[Option[Option[Option[Option[i64]]]]] = Vec.new();
        v4.push(Option.Some(Option.Some(Option.Some(Option.Some(n + i)))));
        match v4[0] { Option.Some(Option.Some(Option.Some(Option.Some(x)))) => { acc = acc + x - n; } _ => { acc = acc - 1; } }

        let mut vs: Vec[Option[String]] = Vec.new();
        vs.push(Option.Some(mkstr(n + i)));
        match vs[0] { Option.Some(t) => { if t.contains("envelope-") { acc = acc + 1; } } Option.None => { acc = acc - 1; } }

        let mut vp: Vec[Option[i64]] = Vec.new();
        vp.push(Option.Some(n + i));
        match vp[0] { Option.Some(x) => { acc = acc + x - n; } Option.None => { acc = acc - 1; } }

        let mut vu: Vec[Option[Option[Option[i64]]]] = Vec.new();
        vu.push(Option.Some(Option.Some(Option.Some(n + i))));

        i = i + 1;
    }
    println(acc);
}
"#,
        &["3160"],
        "vec_element_boxed_enum_envelope_owned",
        300,
    );
}

#[test]
fn asan_vec_retain_drops_filtered_heap_elements_no_leak() {
    // B-2026-07-15-19: the AOT `Vec.retain` lowering must FREE the elements
    // the predicate filters out (and must NOT double-free the kept ones that
    // compact forward, nor the whole Vec at scope exit). Loop a heap
    // `Vec[String]` (filtered `String` buffers must be freed each iteration)
    // and a nested `Vec[Vec[i64]]` (filtered inner-Vec buffers must be freed)
    // so any per-iteration strand accumulates into a visible LSan leak. The
    // compaction is drop-safe only because `len = w` excludes the stale
    // byte-duplicate tail from the Vec's own scope-exit drop — a bug there
    // would surface as a double-free (SIGABRT) rather than a leak.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 40 {
        let mut v: Vec[String] = Vec.new();
        v.push(i.to_string());
        v.push("keepme".to_string());
        v.push("x".to_string());
        v.push("droplong".to_string());
        v.retain(|s| s.len() >= 3);
        let mut j: i64 = 0;
        while j < v.len() {
            acc = acc + v[j].len();
            j = j + 1;
        }
        let mut nv: Vec[Vec[i64]] = Vec.new();
        let mut p: Vec[i64] = Vec.new();
        p.push(1);
        let mut q: Vec[i64] = Vec.new();
        q.push(1); q.push(2); q.push(3);
        nv.push(p);
        nv.push(q);
        nv.retain(|inner| inner.len() > 1);
        acc = acc + nv[0].len();
        i = i + 1;
    }
    println(acc);
}
"#,
        &["680"],
        "vec_retain_drops_filtered_heap_elements_no_leak",
    );
}

#[test]
fn asan_vec_dedup_drops_removed_heap_duplicates_no_leak() {
    // `Vec[T].dedup()` — the AOT lowering must FREE each removed consecutive
    // duplicate (distinct owned `String` buffers even when content-equal),
    // must NOT double-free the kept run-leaders that compact forward, and
    // `len = w` must exclude the stale byte-duplicate tail from the Vec's own
    // scope-exit drop (a bug there is a double-free / SIGABRT, not a leak).
    // Loop a heap `Vec[String]` with runs so any per-iteration strand
    // accumulates into a visible LSan leak.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 40 {
        let mut v: Vec[String] = Vec.new();
        v.push("aa".to_string());
        v.push("aa".to_string());
        v.push("bbbb".to_string());
        v.push("bbbb".to_string());
        v.push("bbbb".to_string());
        v.push("c".to_string());
        v.dedup();
        let mut j: i64 = 0;
        while j < v.len() {
            acc = acc + v[j].len();
            j = j + 1;
        }
        i = i + 1;
    }
    println(acc);
}
"#,
        &["280"],
        "vec_dedup_drops_removed_heap_duplicates_no_leak",
    );
}

#[test]
fn asan_vec_split_off_moves_heap_tail_no_leak_no_double_free() {
    // `Vec[T].split_off(i)` MOVES the [i, len) tail into a fresh Vec (byte-copy
    // of each String `{ptr,len,cap}`). `self.len = i` must exclude the moved
    // tail from self's scope-exit drop, and the returned Vec must own+free it
    // exactly once — a bug is a double-free (SIGABRT) or a per-iteration leak.
    // Loop a heap `Vec[String]`, split, and drop BOTH halves each iteration.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 40 {
        let mut v: Vec[String] = Vec.new();
        v.push("alpha".to_string());
        v.push("beta".to_string());
        v.push("gamma".to_string());
        v.push("delta".to_string());
        let t: Vec[String] = v.split_off(2);
        let mut j: i64 = 0;
        while j < v.len() { acc = acc + v[j].len(); j = j + 1; }
        let mut k: i64 = 0;
        while k < t.len() { acc = acc + t[k].len(); k = k + 1; }
        i = i + 1;
    }
    println(acc);
}
"#,
        &["760"],
        "vec_split_off_moves_heap_tail_no_leak_no_double_free",
    );
}

#[test]
fn asan_vec_element_field_move_by_assignment_no_double_free() {
    // B-2026-08-11-25 — `out = stats[0].region` moves a heap field out of a
    // struct held as a Vec ELEMENT into an EXISTING binding. Nothing
    // cap-zeroed the field in its owner, so the owner's drop freed the
    // buffer the target now owned: `free(): double free detected in tcache
    // 2` on both compiled backends, with `karac check` silent and the
    // interpreter correct.
    //
    // THE PAYLOAD MUST BE BUILT AT RUN TIME. A `String` LITERAL field is
    // static, so the second free lands on a non-heap pointer and passes
    // silently — the row records that two early controls looked clean for
    // exactly this reason and nearly went in as false conditions. Hence
    // `String.new()` + `push_str` and a `+` concat here, never a literal.
    //
    // ASAN is the only gate that can tell the FIX from a relabelling: the
    // repair is to stop the owner freeing, so an over-broad version turns
    // the double free into a per-iteration leak, which the value pins in
    // tests/codegen.rs cannot see. The loop is here to make such a leak
    // accumulate rather than hide.
    //
    // THAT LAST SENTENCE OVERSTATED WHAT THIS LOOP DOES, and B-2026-08-12-4
    // was the leak it let through. `best_region` is overwritten only when
    // the running max improves — twice over these 40 elements — so exactly
    // ONE buffer is displaced, and one leaked pointer left in a stale stack
    // slot reads to LeakSanitizer as still-reachable. This fixture was green
    // under ASAN on a tree where valgrind reported that leak on every run.
    // `asan_place_field_move_assign_overwrite_no_leak` is the one that
    // actually accumulates (200 unconditional overwrites); keep both.
    assert_clean_asan_run(
        r#"
struct S { region: String, revenue: i64 }
struct V { xs: Vec[i64] }
fn main() {
    let mut stats: Vec[S] = Vec.new();
    let mut i: i64 = 0;
    while i < 40 {
        let mut nm = String.new();
        nm.push_str("region");
        stats.push(S { region: nm, revenue: (i * 7) % 11 });
        i = i + 1;
    }
    let mut best: i64 = 0;
    let mut best_region = String.new();
    let mut j: i64 = 0;
    while j < stats.len() {
        if stats[j].revenue > best {
            best = stats[j].revenue;
            best_region = stats[j].region;
        }
        j = j + 1;
    }
    let a = "no".to_string();
    let b = "rth".to_string();
    let mut cat: Vec[S] = Vec.new();
    cat.push(S { region: a + b, revenue: 1 });
    let mut out = String.new();
    out = cat[0].region;
    let mut vs: Vec[V] = Vec.new();
    let mut inner: Vec[i64] = Vec.new();
    inner.push(9);
    vs.push(V { xs: inner });
    let mut got: Vec[i64] = Vec.new();
    got = vs[0].xs;
    println(f"{best} {best_region.len()} {out} {got.len()}");
}
"#,
        &["10 6 north 1"],
        "vec_element_field_move_by_assignment_no_double_free",
    );
}

#[test]
fn asan_vec_sort_by_partition_path_in_bounds() {
    // B-2026-08-11-10 § Direction 7 — `Vec.sort_by` on a low-cardinality
    // input above the entry probe's length floor takes the full-array
    // stable partition instead of the merge. That path writes through raw
    // GEPs into the phase-2 scratch it borrows, with two cursors advancing
    // from opposite ends of a computed split, so an off-by-one in the split
    // or in the parity copy-back is an out-of-bounds write rather than a
    // wrong answer.
    //
    // 5000 elements over 8 keys is chosen to exercise all three exits:
    // above SPAN it partitions, the halves fall below SPAN and hand back to
    // the merge, and the equal blocks take the all-equal early return.
    // Elements are `i64` and an all-int struct because those are the only
    // shapes `should_use_mono_vec_sort_by_for` admits — no heap-owning
    // element ever reaches this path, so the class here is bounds, not
    // ownership.
    //
    // The second half sorts 5000 records that ALL compare equal and checks
    // every one is still at its original index: that is the all-equal early
    // exit returning a range it declared sorted, and stability is the only
    // thing that can catch it having reordered anything.
    assert_clean_asan_run(
        r#"
struct K { key: i64, ord: i64 }
fn main() {
    let mut v: Vec[i64] = Vec.new();
    let mut seed: i64 = 12345;
    let mut i: i64 = 0;
    while i < 5000 {
        seed = (seed * 1103515245 + 12345) % 2147483648;
        v.push(seed % 8);
        i = i + 1;
    }
    v.sort_by(|a, b| a.cmp(b));
    let mut inv: i64 = 0;
    let mut sum: i64 = 0;
    let mut j: i64 = 1;
    while j < v.len() { if v[j] < v[j - 1] { inv = inv + 1; } j = j + 1; }
    let mut t: i64 = 0;
    while t < v.len() { sum = sum + v[t] * (t % 7); t = t + 1; }
    println(f"{inv}");
    println(f"{sum}");

    let mut w: Vec[K] = Vec.new();
    let mut m: i64 = 0;
    while m < 5000 { w.push(K { key: 7, ord: m }); m = m + 1; }
    w.sort_by(|a, b| a.key.cmp(b.key));
    let mut bad: i64 = 0;
    let mut q: i64 = 0;
    while q < w.len() { if w[q].ord != q { bad = bad + 1; } q = q + 1; }
    println(f"{bad}");
}
"#,
        // inversions, an order-sensitive checksum, and stability violations
        // through the all-equal exit.
        &["0", "52493", "0"],
        "vec_sort_by_partition_path_in_bounds",
    );
}

#[test]
fn asan_return_struct_field_vec_element_no_double_free() {
    // B-2026-07-27-1: `return <struct>.<vecfield>[i];` — a field-rooted heap
    // element read as the WHOLE expression of an explicit `return` STATEMENT
    // — handed back the container's own element buffer without a copy, so
    // the caller freed it AND the struct's per-element drop freed it again
    // (Invalid free() / SIGABRT under both JIT and AOT; `--interp` clean).
    // The TAIL spelling of the identical read (`{ self.xs[i] }`) was already
    // correct — it routes through `compile_tail_final_expr`'s field-rooted
    // clone arm — so the two return spellings disagreed. Both now share one
    // clone gate (`compile_field_rooted_index_return`).
    //
    // Covers all four trigger conditions plus the contrasts that must stay
    // balanced (no DOUBLE clone → no leak): the method form, the free-fn
    // form (`ref S` param — not `self`-specific), the tail form, the
    // let-binding form, the concat form, a `Vec[i64]` (Copy) field that must
    // NOT clone, and a `Vec[Vec[String]]` field whose element is itself a
    // heap Vec. Looped so a leak accumulates past LSan's noise floor.
    let label = "return_struct_field_vec_element";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan_with_full_pipeline(
        r#"
struct S { xs: Vec[String] }
struct G { g: Vec[Vec[String]] }
struct N { ns: Vec[i64] }

impl S {
    fn get(ref self, i: i64) -> String { return self.xs[i]; }
    fn tail(ref self, i: i64) -> String { self.xs[i] }
    fn via_let(ref self, i: i64) -> String { let t = self.xs[i].clone(); return t; }
    fn concat(ref self, i: i64) -> String { return self.xs[i] + "!"; }
}
impl G { fn get(ref self, i: i64) -> Vec[String] { return self.g[i]; } }
impl N { fn get(ref self, i: i64) -> i64 { return self.ns[i]; } }

fn free_get(s: ref S, i: i64) -> String { return s.xs[i]; }

fn main() {
    let mut s = S { xs: Vec.new() };
    let mut i = 0i64;
    while i < 50i64 {
        s.xs.push(f"e{i}");
        i = i + 1;
    }

    let mut n = 0i64;
    let mut j = 0i64;
    while j < 50i64 {
        n = n + s.get(j).len();
        n = n + free_get(s, j).len();
        n = n + s.tail(j).len();
        n = n + s.via_let(j).len();
        n = n + s.concat(j).len();
        j = j + 1i64;
    }
    println(f"str={n}");

    // The source container must be intact after all those reads.
    println(f"live={s.xs[7]} len={s.xs.len()}");

    // Copy element: must NOT be cloned, and must stay correct.
    let mut nn = N { ns: Vec.new() };
    let mut k = 0i64;
    while k < 20i64 {
        nn.ns.push(k * 3i64);
        k = k + 1i64;
    }
    let mut sum = 0i64;
    let mut p = 0i64;
    while p < 20i64 {
        sum = sum + nn.get(p);
        p = p + 1i64;
    }
    println(f"scalar={sum}");

    // Heap element that is itself a Vec.
    let mut gg = G { g: Vec.new() };
    let mut q = 0i64;
    while q < 10i64 {
        let mut inner: Vec[String] = Vec.new();
        inner.push(f"i{q}");
        inner.push(f"j{q}");
        gg.g.push(inner);
        q = q + 1i64;
    }
    let mut gl = 0i64;
    let mut r = 0i64;
    while r < 10i64 {
        let got = gg.get(r);
        gl = gl + got.len();
        r = r + 1i64;
    }
    println(f"nested={gl}");
}
"#,
        label,
    ) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN/LSAN reported a memory error (exit code {:?}) — a \
             `return <struct>.<vecfield>[i];` hands back the container's element \
             buffer, so look for a double-free of the returned String (missing \
             clone) or a leak of the clone (cloned twice)",
        status.code()
    );
    assert_eq!(
        stdout.trim().lines().collect::<Vec<_>>(),
        vec!["str=750", "live=e7 len=50", "scalar=570", "nested=20",],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

// B-2026-07-30-11 (Vec leg) — element bodies added to `Vec[T]`'s drop.
//
// Same direction as the two tests above: the bug was a LEAK, and the fix is
// bodies-only (the element MEMORY was always freed by the `FreeVecBuffer`
// drain), so LSan cannot witness the fix landing. What it guards is
// OVER-firing — a body run once per element is correct, twice is a
// use-after-free for anything that touches heap, and the walk is a fresh
// `0..len` loop over a buffer another drain also visits.
//
// Non-vacuity, same lesson as above: `buf.clear()` alone lets LLVM delete
// every element allocation. The `self.buf[0]` read observes the BYTES and
// keeps them. The guard is never true (values are `i >= 0`), so the branch
// is dead at runtime but not to the optimizer.
#[test]
fn asan_vec_element_user_drop_bodies_fire_once() {
    assert_clean_asan_run(
        r#"
struct Res { tag: i64, buf: Vec[i64] }
impl Drop for Res {
    fn drop(mut ref self) {
        if let Some(v) = self.buf.first() { if v < 0i64 { println(v); } }
        self.buf.clear();
    }
}
struct Wrap { r: Res }

fn main() {
    let mut n = 0i64;
    let mut i = 0i64;
    while i < 200i64 {
        let mut b1: Vec[i64] = Vec.new();
        b1.push(i);
        let mut b2: Vec[i64] = Vec.new();
        b2.push(i);
        let v: Vec[Res] = [Res { tag: 1, buf: b1 }, Res { tag: 2, buf: b2 }];
        n = n + v.len();

        // Drop-bearing element FIELD, reached through the container — the
        // shape B-2026-07-29-39's field walk went silent on one Vec deep.
        let mut b3: Vec[i64] = Vec.new();
        b3.push(i);
        let w: Vec[Wrap] = [Wrap { r: Res { tag: 3, buf: b3 } }];
        n = n + w.len();
        i = i + 1;
    }
    println(n);
}
"#,
        // 2 + 1 per iteration x 200 = 600.
        &["600"],
        "vec_element_user_drop_bodies_fire_once",
    );
}

/// B-2026-08-09-9 under ASAN — a `Vec[String]` enum payload consumed by an
/// arm over a live local must leave BOTH copies independently freeable.
///
/// This is the fixture the output test cannot be: the defect was two
/// invalid reads plus two invalid frees on element buffers the two copies
/// SHARED, while the program's stdout was already correct. Only a
/// sanitizer distinguishes "right answer" from "right answer over freed
/// memory".
///
/// It guards both directions. Too shallow a copy and the elements alias, so
/// the arm's consumption frees buffers the live source still owns
/// (use-after-free, then a double free at scope exit). Too deep a copy in
/// the wrong place and the duplicated elements have no owner — which is not
/// hypothetical either: making element depth unconditional rather than
/// opt-in leaked 1990 bytes across 300 allocations in
/// `asan_match_bound_struct_variant_vec_field_reborrow_no_double_free`.
/// LSan on the Linux CI leg is what catches that half.
///
/// Case 3's `Vec[i64]` payload is the scalar-element control: a bit-copy is
/// already exact there, so it must stay clean without the element walk.
///
/// Every payload is read BYTE-WISE (`println` of the string itself, never
/// `.len()`), because a buffer whose bytes are never read is a provably
/// dead allocation LLVM deletes outright — the fixture would then assert
/// nothing at all.
#[test]
fn asan_consuming_match_over_live_enum_vec_payload_frees_each_buffer_once() {
    assert_clean_asan_run(
        r#"
enum E { A(Vec[String]), B }
enum N { A(Vec[i64]), B }
fn main() {
    let mut i: i64 = 0i64;
    while i < 4i64 {
        // 1. The bug: consumed by the arm, source read again afterwards.
        let e: E = E.A([f"aa{i}", f"bb{i}"]);
        match e { E.A(v) => { for s in v { println(s); } } E.B => { println("-"); } }
        match e { E.A(v) => { for s in v { println(s); } } E.B => { println("-"); } }
        // 2. CONTROL — source DEAD after the consuming match, so no clone.
        let d: E = E.A([f"dd{i}"]);
        match d { E.A(v) => { for s in v { println(s); } } E.B => { println("-"); } }
        // 3. CONTROL — scalar elements, exact without the element walk.
        let n: N = N.A([i, i]);
        match n { N.A(v) => { for s in v { println(s); } } N.B => { println("-"); } }
        match n { N.A(v) => { for s in v { println(s); } } N.B => { println("-"); } }
        i = i + 1;
    }
    println("end");
}
"#,
        &[
            "aa0", "bb0", "aa0", "bb0", "dd0", "0", "0", "0", "0", "aa1", "bb1", "aa1", "bb1",
            "dd1", "1", "1", "1", "1", "aa2", "bb2", "aa2", "bb2", "dd2", "2", "2", "2", "2",
            "aa3", "bb3", "aa3", "bb3", "dd3", "3", "3", "3", "3", "end",
        ],
        "consuming_match_live_enum_vec_payload",
    );
}

/// B-2026-08-09-13 — the NON-consuming half of the fixture above: when
/// nothing takes a `Vec[String]` enum payload's elements, the enum's own
/// drop has to.
///
/// `emit_enum_drop_switch`'s `VecOrString` arm freed the `{ptr,len,cap}`
/// buffer and stopped, so every element buffer was owned by nobody. It went
/// unnoticed because the shapes that DO consume the payload (case 3's `for`
/// loop) are clean — their arm binding drains the elements — and because at
/// the default `-O2` the optimizer deletes an f-string allocation whose
/// bytes are never read, which is why every case below prints the string
/// ITSELF rather than its `.len()`.
///
/// Case 1 is the borrow path B-2026-08-08-25 leg 3 installed: the arm reads
/// `v[0]` and never escapes it, so the binding registers no drop of its own
/// and this drop fn is the payload's sole owner. Case 2 is the `ref`-param
/// spelling of the same ownership, which leaked on its own terms before leg
/// 3 existed.
///
/// Case 3 is the DOUBLE-FREE control, and the reason the element drain sits
/// INSIDE the `cap > 0` guard: a consuming arm's
/// `suppress_destructured_enum_payload_cleanup` zeroes the source's cap, so
/// neither the drain nor the buffer free runs here and the arm stays the
/// only owner. A drain hoisted above that guard frees each element twice.
///
/// Case 4 is the scalar-element control — a `Vec[i64]` payload owns no
/// element heap, so it must stay exactly as clean as before.
#[test]
fn asan_readonly_match_over_enum_vec_payload_frees_each_element_once() {
    assert_clean_asan_run(
        r#"
enum E { A(Vec[String]), B }
enum N { A(Vec[i64]), B }
fn peek(e: ref E) {
    match e { E.A(v) => { println(v[0]); } E.B => { println("-"); } }
}
fn main() {
    let mut i: i64 = 0i64;
    while i < 4i64 {
        // 1. The bug: read-only arm, nothing consumes the elements.
        let e: E = E.A([f"aa{i}", f"bb{i}"]);
        match e { E.A(v) => { println(v[0]); println(v[1]); } E.B => { println("-"); } }
        // 2. The `ref`-param spelling of the same ownership.
        let r: E = E.A([f"rr{i}"]);
        peek(r);
        // 3. CONTROL — the arm CONSUMES the elements, so the source's cap is
        //    zeroed and this drop must stay a no-op (else: double free).
        let c: E = E.A([f"cc{i}"]);
        match c { E.A(v) => { for s in v { println(s); } } E.B => { println("-"); } }
        // 4. CONTROL — scalar elements own no heap under the buffer.
        let n: N = N.A([i, i]);
        match n { N.A(v) => { println(v[0]); } N.B => { println("-"); } }
        i = i + 1;
    }
    println("end");
}
"#,
        &[
            "aa0", "bb0", "rr0", "cc0", "0", "aa1", "bb1", "rr1", "cc1", "1", "aa2", "bb2", "rr2",
            "cc2", "2", "aa3", "bb3", "rr3", "cc3", "3", "end",
        ],
        "readonly_match_enum_vec_payload_elements",
    );
}

/// B-2026-09-07-30 — a projection off an RC-FALLBACK-PROMOTED local whose
/// destination is a `Vec.push` ARGUMENT or an EXISTING BINDING hands that
/// destination the box's buffer, which the box does not give up.
///
///     let t = mkp(9); while i < 3i64 { v.push(t.a); i = i + 1; }   // 4 owners
///     let t = mkp(9); while i < 3i64 { s = t.a;     i = i + 1; }   // 2, then more
///
/// This is B-2026-09-07-23's defect at the two destinations that row's fix
/// did not reach. Every disarm reaches the source field by GEP-ing the
/// binding's slot, and a promoted slot holds a `{i64 rc, T}` box HANDLE
/// rather than the struct, so each bails on its shape test and the field's
/// cap is never zeroed. `-23` gave the `let` binding and the struct-literal
/// field an independent buffer; the push argument and the assignment target
/// kept taking the alias.
///
/// THE `push` CELL ABORTS ON EVERY COMPILED CONFIGURATION, which is what
/// the row filing this understated as a leak: one buffer acquires FOUR
/// owners (three elements and the box), so the program dies with
/// `free(): double free detected in tcache 2` before printing, against an
/// interpreter that prints `3`. Measured 18 allocs / 21 frees with 3
/// invalid frees at `KARAC_AUTO_PAR=0`, 51/29 with 1 under fan-out.
///
/// THE `assign` CELL IS THE ONE AUTO-PAR HID. With fan-out ON it prints
/// correctly and merely strands 40 B; only a `KARAC_AUTO_PAR=0` BUILD
/// aborts, at 25 allocs / 26 frees. Same defect, two lanes — which is why
/// the row that found it recorded a leak and not a corruption.
///
/// WHY AUTO-PAR IS OFF HERE: see `assert_clean_asan_run_no_auto_par`. The
/// residual 40 B under fan-out is a different defect with a different
/// trigger set, filed separately; the output-and-exit-status half of the
/// auto-par lane is covered by the `codegen.rs` and `par_codegen.rs` twins.
///
/// COPYING AT EACH DESTINATION'S OWN LOWERING, not in the shared argument
/// disarm, and the difference is measured rather than stylistic. Routing the
/// push copy through `suppress_source_vec_cleanup_for_arg_ex` — which ~59
/// call sites funnel through, including the `let` and struct-literal
/// destinations `-23` already fixed — STACKS a second copy on theirs and
/// leaks the first: 114 B in 3 blocks on both of those cells, 38 B per trip.
/// Cells 7 and 8 are those two shapes, kept as controls precisely so that
/// regression fails here.
#[test]
fn asan_rc_boxed_projection_push_and_assign_destinations_copy() {
    const OWN: &str = "struct P { a: String, b: i64 }\n\
             fn seed() -> i64 { env.args().len() }\n\
             fn payload() -> String { f\"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa\" }\n\
             fn mkp(n: i64) -> P { return P { a: payload(), b: n }; }\n\
             fn main() { println(go()); }\n";
    // 1 — the row's `push` cell. Three trips, four owners, aborts on the
    // parent before it can print.
    assert_clean_asan_run_no_auto_par(
            &format!(
                "{OWN}fn go() -> i64 {{ let t = mkp(9); let mut v: Vec[String] = Vec.new(); let mut i = 0i64;\n\
                 \x20 while i < 3i64 {{ v.push(t.a); i = i + 1; }}\n\
                 \x20 return v.len(); }}\n"
            ),
            &["3"],
            "rc_boxed_proj_push_three_trips",
        );
    // 2 — ONE trip: the smallest dirty `push` cell.
    assert_clean_asan_run_no_auto_par(
            &format!(
                "{OWN}fn go() -> i64 {{ let t = mkp(9); let mut v: Vec[String] = Vec.new(); let mut i = 0i64;\n\
                 \x20 while i < 1i64 {{ v.push(t.a); i = i + 1; }}\n\
                 \x20 return v.len(); }}\n"
            ),
            &["1"],
            "rc_boxed_proj_push_one_trip",
        );
    // 3 — FIVE trips: the damage is per-iteration, so this is six owners.
    assert_clean_asan_run_no_auto_par(
            &format!(
                "{OWN}fn go() -> i64 {{ let t = mkp(9); let mut v: Vec[String] = Vec.new(); let mut i = 0i64;\n\
                 \x20 while i < 5i64 {{ v.push(t.a); i = i + 1; }}\n\
                 \x20 return v.len(); }}\n"
            ),
            &["5"],
            "rc_boxed_proj_push_five_trips",
        );
    // 4 — the `assign` cell: the destination is an EXISTING binding, which
    // also frees its own displaced value each trip.
    assert_clean_asan_run_no_auto_par(
        &format!(
            "{OWN}fn go() -> i64 {{ let t = mkp(9); let mut s = payload(); let mut i = 0i64;\n\
                 \x20 while i < 3i64 {{ s = t.a; i = i + 1; }}\n\
                 \x20 return s.len(); }}\n"
        ),
        &["38"],
        "rc_boxed_proj_assign_three_trips",
    );
    // 5 — the LIVE-VALUE oracle, and what rules out the fix that looks
    // equivalent. Zeroing the box's own field would neutralize the source,
    // but the box is the surviving owner precisely because the binding is
    // read again — so `t.a` must still be 38 bytes on every trip, not 38
    // once and 0 after. 3 x 38 + 3 = 117, which is what the interpreter
    // prints.
    assert_clean_asan_run_no_auto_par(
            &format!(
                "{OWN}fn go() -> i64 {{ let t = mkp(9); let mut v: Vec[String] = Vec.new(); let mut i = 0i64; let mut n = 0i64;\n\
                 \x20 while i < 3i64 {{ v.push(t.a); n = n + t.a.len(); i = i + 1; }}\n\
                 \x20 return n + v.len(); }}\n"
            ),
            &["117"],
            "rc_boxed_proj_push_source_stays_live",
        );
    // 6 — the ELEMENTS must be independent of each other, not three
    // aliases of one buffer: reading two of them back is 76, and a fix
    // that shared one buffer would still print 76 while double-freeing —
    // which is why this cell's value is a check on the copy COUNT, and the
    // clean-exit assertion is what makes it bite.
    assert_clean_asan_run_no_auto_par(
            &format!(
                "{OWN}fn go() -> i64 {{ let t = mkp(9); let mut v: Vec[String] = Vec.new(); let mut i = 0i64;\n\
                 \x20 while i < 3i64 {{ v.push(t.a); i = i + 1; }}\n\
                 \x20 return v[0].len() + v[2].len(); }}\n"
            ),
            &["76"],
            "rc_boxed_proj_push_elements_are_independent",
        );
    // 7 — CONTROL, and the first half of the stacking regression: the
    // `let`-bound destination B-2026-09-07-23 already fixed. It must keep
    // taking exactly ONE copy.
    assert_clean_asan_run_no_auto_par(
        &format!(
            "{OWN}fn go() -> i64 {{ let t = mkp(9); let mut i = 0i64; let mut n = 0i64;\n\
                 \x20 while i < 3i64 {{ let s = t.a; n = n + s.len(); i = i + 1; }}\n\
                 \x20 return n; }}\n"
        ),
        &["114"],
        "rc_boxed_proj_control_let_destination_unchanged",
    );
    // 8 — CONTROL, the other half: the struct-literal field destination,
    // likewise already fixed and likewise at risk of a second copy.
    assert_clean_asan_run_no_auto_par(
            &format!(
                "{OWN}fn go() -> i64 {{ let t = mkp(9); let mut i = 0i64; let mut n = 0i64;\n\
                 \x20 while i < 3i64 {{ let p = P {{ a: t.a, b: 1 }}; n = n + p.a.len(); i = i + 1; }}\n\
                 \x20 return n; }}\n"
            ),
            &["114"],
            "rc_boxed_proj_control_literal_destination_unchanged",
        );
    // 9 — CONTROL: no loop, so no promotion. The disarm works and the
    // destination legitimately takes the buffer; nothing may change.
    assert_clean_asan_run_no_auto_par(
        &format!(
            "{OWN}fn go() -> i64 {{ let t = mkp(9); let mut v: Vec[String] = Vec.new();\n\
                 \x20 v.push(t.a);\n\
                 \x20 return v.len(); }}\n"
        ),
        &["1"],
        "rc_boxed_proj_control_no_promotion",
    );
}

/// B-2026-08-09-12 under ASAN — the `<refparam>.field` ref-chain enum clone
/// must be ELEMENT-deep, so a consuming arm never frees a string the caller
/// still owns.
///
/// The defect produced correct stdout, so only a sanitizer separates it
/// from a working program: with the outer-only copy the clone's `Vec`
/// buffer was fresh but its element `String`s were the caller's, and the
/// arm's `for` loop freed them out from under `s` — one invalid read and
/// one invalid free per call.
///
/// The final match in `main` is load-bearing for the LEAK half of the gate,
/// not decoration: it drains the SOURCE's elements. Without it the program
/// leaks two blocks — not from the clone, but from the pre-existing class
/// where an enum's `Vec[heap]` payload elements are freed by nothing unless
/// something consumes them (B-2026-08-09-13). Draining them here keeps this
/// fixture measuring the clone's depth rather than that separate gap.
///
/// Every payload is read BYTE-WISE (`println` of the string itself, never
/// `.len()`), because a buffer whose bytes are never read is a provably
/// dead allocation LLVM deletes outright.
#[test]
fn asan_ref_chain_enum_vec_payload_clone_is_element_deep() {
    assert_clean_asan_run(
        r#"
enum E { A(Vec[String]), B }
struct S { e: E }
fn take(s: ref S) {
    match s.e { E.A(x) => { for t in x { println(t); } } E.B => { println("-"); } }
}
fn main() {
    let mut i: i64 = 0i64;
    while i < 4i64 {
        let s: S = S { e: E.A([f"aa{i}", f"bb{i}"]) };
        // Twice: the second call reads what the first would have freed.
        take(s);
        take(s);
        // Drain the source's own elements — see the doc above.
        match s.e { E.A(x) => { for t in x { println(t); } } E.B => { println("-"); } }
        i = i + 1;
    }
    println("end");
}
"#,
        &[
            "aa0", "bb0", "aa0", "bb0", "aa0", "bb0", "aa1", "bb1", "aa1", "bb1", "aa1", "bb1",
            "aa2", "bb2", "aa2", "bb2", "aa2", "bb2", "aa3", "bb3", "aa3", "bb3", "aa3", "bb3",
            "end",
        ],
        "ref_chain_enum_vec_payload_clone",
    );
}

/// B-2026-08-09-20 — a `File` MOVED into a `Vec[File]` was owned by nobody.
/// B-2026-08-09-17 stopped the ORIGIN binding closing a moved-out handle
/// (the fix that made the move usable at all), and nothing took the closing
/// over: `vec_elem_agg_drop_for_type_expr` had no `File` arm, so the buffer
/// free reclaimed the SLOT and leaked the `Box<KaracFile>` the slot pointed
/// at — and with it the fd, until process exit.
///
/// LSan is the right gate for this. The user-visible symptom is descriptor
/// exhaustion (253 of 400 opens under `ulimit -n 256`, silently — `File.open`
/// simply starts returning `Err`), which is awkward to assert portably; the
/// leaked Box behind each fd is the same defect one level down, and it is
/// exactly what LSan reports.
#[test]
fn asan_file_moved_into_a_vec_is_closed_by_the_vec() {
    let path = file_fixture_path("file_into_vec");
    assert_clean_asan_run(
        &format!(
            r#"
fn main() with reads(FileSystem) {{
    let mut i: i64 = 0i64;
    let mut n: i64 = 0i64;
    while i < 40i64 {{
        let mut hs: Vec[File] = Vec.new();
        match File.open("{path}") {{
            Ok(f) => {{ hs.push(f); n = n + 1i64; }}
            Err(_) => {{ n = n - 1i64; }}
        }}
        i = i + 1;
    }}
    println(n.to_string());
}}
"#
        ),
        &["40"],
        "file_moved_into_vec_closed",
    );
}

/// The nested shape, which rides in for free on the same element arm:
/// `emit_drop_fn_for_type_expr` delegates a named leaf to
/// `vec_elem_agg_drop_for_type_expr`, so teaching that one dispatch about
/// `File` also teaches the recursive drop family — provided
/// `te_recursive_drop_fully_supported` admits `File`, which is the half that
/// keeps a `Vec[Vec[File]]` off the one-level buffer-only fast path.
#[test]
fn asan_file_inside_a_nested_vec_is_closed_at_every_level() {
    let path = file_fixture_path("file_nested_vec");
    assert_clean_asan_run(
        &format!(
            r#"
fn main() with reads(FileSystem) {{
    let mut i: i64 = 0i64;
    let mut n: i64 = 0i64;
    while i < 40i64 {{
        let mut outer: Vec[Vec[File]] = Vec.new();
        let mut inner: Vec[File] = Vec.new();
        match File.open("{path}") {{
            Ok(f) => {{ inner.push(f); n = n + 1i64; }}
            Err(_) => {{ n = n - 1i64; }}
        }}
        outer.push(inner);
        i = i + 1;
    }}
    println(n.to_string());
}}
"#
        ),
        &["40"],
        "file_nested_vec_closed",
    );
}

/// B-2026-08-14-15 leg B — an `Option[Vec[<aggregate>]]` bound to a `let`
/// and then matched.
///
/// The binding keeps the payload on its own tag-guarded overlay free,
/// which released the element ARRAY and nothing else: each element's own
/// heap (the `String` inside `P`) was stranded. The inline `match mk()`
/// form binds `v` as an owned `Vec[P]` and drains per element, so the two
/// spellings of the same program disagreed.
///
/// The `Result` half is here too, and it is not symmetry-by-assumption: it
/// was measured leaking the same 8 bytes on the `Ok` side, and each half is
/// gated independently, so `Result[Vec[P], Vec[P]]` exercises both.
#[test]
fn asan_bound_option_vec_of_aggregates_drains_elements() {
    assert_clean_asan_run(
        r#"
struct P { tag: String }
fn mk(k: i64) -> Option[Vec[P]] {
    let mut c: Vec[P] = Vec.new();
    let mut s = String.new(); s.push_str("alpha"); s.push_str(k.to_string());
    c.push(P { tag: s });
    Some(c)
}
fn mkr(k: i64) -> Result[Vec[P], Vec[P]] {
    let mut c: Vec[P] = Vec.new();
    let mut s = String.new(); s.push_str("beta"); s.push_str(k.to_string());
    c.push(P { tag: s });
    if k > 100 { return Err(c); }
    Ok(c)
}
fn main() {
    let k = env.args().len() as i64;
    let held = mk(k);
    match held { Some(v) => println(v[0].tag.len()), None => println(0) }
    let ok = mkr(k);
    match ok { Ok(v) => println(v[0].tag.len()), Err(e) => println(e.len()) }
    let er = mkr(200);
    match er { Ok(v) => println(v[0].tag.len()), Err(e) => println(e[0].tag.len()) }
}
"#,
        // "alpha1" / "beta1" / "beta200" — `k` is a stable 1 (the binary
        // runs with no args), appended so each payload string is
        // heap-allocated rather than a static literal.
        &["6", "5", "7"],
        "bound_option_vec_of_aggregates_drains_elements",
    );
}

#[test]
fn asan_if_arm_vec_element_bind_no_double_free() {
    // B-2026-08-14-32: an index read into a heap-owning container, used as
    // the value of an `if` / `match` ARM, aliased the container's own
    // `{ptr, len, cap}` while the binding registered an owned cleanup over
    // it — freed once by the binding, once by the container.
    // `let w = if c { v[1] } else { "x" }` aborted with
    // `free(): double free detected in tcache 2` before the next statement
    // ran, while the DIRECT form `let w = v[1]` was correct: the let path's
    // clone and its borrow-elision predicate both match `ExprKind::Index`
    // at the TOP LEVEL, so the same read one level down inside a branch got
    // neither the clone nor the elision.
    //
    // Every arm shape that crashed is here, plus a read of the container
    // AFTER the binding to prove the element survived the bind, and a
    // `Vec[Vec[i64]]` to show it was never String-specific. LOOPED because
    // the fix's risk is the mirror image — a clone nobody frees — and only
    // repetition makes that leak unmistakable to LSan.
    assert_clean_asan_run(
        r#"
fn fresh() -> String { "zetazetazetazeta" }
fn main() {
    let mut v: Vec[String] = Vec.new();
    v.push("alphabetalphabet");
    v.push("gammagammagamma");
    v.push("deltadeltadeltad");
    let mut n: Vec[Vec[i64]] = Vec.new();
    n.push([1, 2]);
    n.push([3, 4]);
    let mut k = 0;
    while k < 20 {
        let a = if v.len() > 1 { v[1] } else { "x" };
        let b = if v.len() > 1 { v[1] } else { v[0] };
        let c = if v.len() > 9 { v[0] } else { fresh() };
        let d = match v.len() > 1 { true => v[2], false => "x" };
        let e = if v.len() > 2 { if v.len() > 9 { v[0] } else { v[1] } } else { "x" };
        let g = if n.len() > 1 { n[1] } else { n[0] };
        println(f"{a} {b} {c} {d} {e} {g[0]} {v[1]}");
        k = k + 1;
    }
    println("done");
}
"#,
        std::iter::repeat_n(
            "gammagammagamma gammagammagamma zetazetazetazeta deltadeltadeltad \
                 gammagammagamma 3 gammagammagamma",
            20,
        )
        .chain(std::iter::once("done"))
        .collect::<Vec<_>>()
        .as_slice(),
        "asan_if_arm_vec_element_bind_no_double_free",
    );
}

/// B-2026-08-14-38 — indexing a Vec-returning METHOD call
/// (`v.clone()[1]`, `b.copy_items()[0]`, `nums[1..3].to_vec()[0]`) now
/// materializes the nameless temporary and reads through it. The read is
/// the easy half; the temporary's BUFFER is the half a wrong fix loses.
/// Each spelling here allocates a Vec nothing else owns, so a missing drop
/// is a per-iteration leak and a drop of a non-owned view would be a
/// double free — LSan sees both. The `Vec[String]` cases carry the second
/// obligation: the element read is deep-cloned before the temp's buffer is
/// drained, and that CLONE needs an owner too. The first cut of the fix
/// dropped the buffer correctly and leaked every clone, because the
/// consumer-side predicate that frees such an element
/// (`expr_is_inline_temp_vec_heap_index`) still listed only the two older
/// receiver shapes — which is why both halves resolve through one helper
/// now.
///
/// Looped for the same reason as the map fixture above: at one print site
/// per shape every stranded buffer is still reachable from its entry
/// alloca at exit, and LSan reports nothing. Iterating overwrites the
/// slots. Do not unroll this fixture.
#[test]
fn asan_indexing_a_vec_returning_method_call_owns_its_temporary() {
    assert_clean_asan_run(
        r#"
struct Bag { items: Vec[String] }
impl Bag {
    pub fn copy_items(ref self) -> Vec[String] { return self.items.clone(); }
}
fn main() {
    let mut k = 0;
    while k < 20 {
        let names: Vec[String] = ["alphaalphaalphaalpha", "betabetabetabetabeta"];
        println(names.clone()[1]);
        let b = Bag { items: names.clone() };
        println(b.copy_items()[0]);
        let nums: Vec[i64] = [10, 20, 30, 40];
        println(f"{nums[1..3].to_vec()[0]}");
        k = k + 1;
    }
    println("done");
}
"#,
        &["betabetabetabetabeta", "alphaalphaalphaalpha", "20"]
            .repeat(20)
            .into_iter()
            .chain(std::iter::once("done"))
            .collect::<Vec<_>>(),
        "asan_indexing_a_vec_returning_method_call_owns_its_temporary",
    );
}

/// B-2026-08-15-7 — a fresh-owned `Vec` temporary passed by value to a
/// GENERIC fn's owned param was never dropped, while the identical call into
/// a non-generic twin was clean.
///
/// The mono prologue enters every bare non-borrow param that lands in
/// `vec_elem_types` into `owned_vecstr_params`, which is the caller-retains
/// convention: the callee deep-copies at each retaining consume site and
/// never frees the caller's buffer. The ordinary call path answers that by
/// materializing every fresh heap `Vec` temp into the caller's scope; the
/// monomorph path did so only for a param spelled as a BARE type parameter,
/// so `fn take[T](v: Vec[T])` — the ordinary way to write it — stranded one
/// buffer per call.
///
/// `head` is deliberately the callee that READS the buffer. A callee like
/// `take` that only asks for `len()` leaves the cloned bytes unread, and at
/// the default opt level LLVM then deletes the allocation outright and the
/// fixture asserts over memory that was never touched — this leak is
/// invisible at `-O2` and plain at `-O0` for exactly that reason. Both are
/// kept: `take` for the row's headline spelling, `head` so the fixture cannot
/// go vacuous if the `-O0` leg is ever dropped.
///
/// `pick` is the double-free direction. The bare-`T` arm excludes a param
/// whose type param appears in the return type, because a forwarding tail
/// hands the caller's own buffer back out; the container arm deliberately
/// does not inherit that exclusion, so a forwarding tail in CONTAINER
/// spelling is pinned here as single-free. `sink` pins the `mut ref`
/// accumulator, whose pushed element must be the callee's deep copy rather
/// than the caller's buffer.
#[test]
fn asan_generic_owned_vec_param_temp_arg_no_leak() {
    assert_clean_asan_run(
        r#"
fn take[T](v: Vec[T]) -> i64 { return v.len(); }
fn head[T](v: Vec[T]) -> T { return v[0]; }
fn passthru[T](v: Vec[T]) -> Vec[T] { return v; }
fn id[T](v: Vec[T]) -> Vec[T] { return v; }
fn pick[T](a: Vec[T], b: Vec[T]) -> Vec[T] { return id(a); }
fn sink[T](acc: mut ref Vec[Vec[T]], v: Vec[T]) { acc.push(v); }
fn mk() -> Vec[i64] { let v: Vec[i64] = [11, 22, 33, 44, 55, 66, 77, 88]; return v; }
fn main() {
    let nums: Vec[i64] = [1, 2, 3, 4, 5, 6, 7, 8];
    let ns: Vec[String] = ["alphaalphaalpha", "betabetabeta", "gammagammagamma"];
    let mut acc: Vec[Vec[i64]] = Vec.new();
    let mut k = 0;
    while k < 20 {
        println(take(nums.clone()));
        println(take(ns.clone()));
        println(head(nums.clone()));
        println(head(ns.clone()));
        println(take(mk()));
        println(take([7, 8, 9]));
        let p = passthru(ns.clone());
        println(p[1]);
        let w = pick(nums.clone(), mk());
        println(w[0]);
        sink(mut acc, nums.clone());
        k = k + 1;
    }
    println(acc.len());
    println(acc[19][7]);
    println("done");
}
"#,
        &[
            "8",
            "3",
            "1",
            "alphaalphaalpha",
            "8",
            "3",
            "betabetabeta",
            "1",
        ]
        .repeat(20)
        .into_iter()
        .chain(["20", "8", "done"])
        .collect::<Vec<_>>(),
        "asan_generic_owned_vec_param_temp_arg_no_leak",
    );
}

/// B-2026-08-21-10 — `Vec.from_fn` with a HEAP element.
///
/// The function's result is MOVED into the buffer, so the body's own
/// temporary must be disarmed or the last iteration's allocation gets two
/// owners. Measured before the disarm: `Vec.from_fn(3, |i| f"n{i}")`
/// aborted with "free(): double free detected in tcache 2" on both AOT
/// and JIT while `--interp` printed correctly.
///
/// Both heap body shapes are swept — an f-string (which builds through a
/// scope-registered accumulator) and a method result — inside a loop so a
/// per-call imbalance accumulates rather than hiding in one iteration.
#[test]
fn asan_vec_from_fn_heap_element_has_one_owner() {
    assert_clean_asan_run(
        r#"fn main() {
    let mut round = 0i64;
    let mut last: String = "";
    let mut total = 0i64;
    while round < 30i64 {
        let interp: Vec[String] = Vec.from_fn(4, |i| f"n{i}");
        let built: Vec[String] = Vec.from_fn(4, |i| i.to_string());
        total = total + interp.len() + built.len();
        last = built[3].clone();
        round = round + 1i64;
    }
    println(last);
    println(total);
}
"#,
        &["3", "240"],
        "asan_vec_from_fn_heap_element_has_one_owner",
    );
}

/// A `Vec` carrier out of a LABELED loop, broken from inside a nested
/// `while` — the frames between the break and the loop boundary are what
/// `emit_scope_cleanup_from` drains, so this exercises the deepest
/// suppression path.
#[test]
fn asan_labeled_loop_break_vec_value_single_owner() {
    assert_clean_asan_run_min_allocs(
        "fn pick() -> Vec[i64] {\n\
             \x20   let mut i: i64 = env.args().len() - 1;\n\
             \x20   outer: loop {\n\
             \x20       i = i + 1;\n\
             \x20       let mut j = 0;\n\
             \x20       while j < 2 {\n\
             \x20           j = j + 1;\n\
             \x20           if i == 2 { break outer [i, j, i + j] }\n\
             \x20       }\n\
             \x20   }\n\
             }\n\
             fn main() { let v = pick(); println(v[0]); println(v[1]); println(v[2]); }\n",
        &["2", "1", "3"],
        "labeled-loop-break-vec",
        4,
    );
}

/// B-2026-08-26-9 regression oracle, ASAN leg. The caller of a function
/// that MOVES its by-value argument into a place the caller still holds
/// used to run its own fresh-temp drop anyway, so an `impl Drop` element
/// fired its body once at the push and again at the pop. Heap-FREE element
/// on purpose: it isolates the drop COUNT, which is what this row was
/// about, from `PriorityQueue`'s separate struct-element buffer leak
/// (B-2026-08-26-18) that a `String` field would drag in.
#[test]
fn asan_priority_queue_push_drops_a_drop_element_exactly_once() {
    assert_clean_asan_run(
        r#"
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Item { id: i64 }
impl Drop for Item { fn drop(mut ref self) { println(f"drop {self.id}") } }
fn main() {
    let mut q: PriorityQueue[Item] = PriorityQueue.new();
    q.push(Item { id: 3 });
    q.push(Item { id: 1 });
    println("built");
    while q.len() > 0 { match q.pop() { Some(v) => { println(f"pop {v.id}"); } None => {} } }
    println("end");
}
"#,
        &["built", "pop 1", "drop 1", "pop 3", "drop 3", "end"],
        "pq-push-drop-once",
    );
}

#[test]
fn asan_tuple_arg_vec_element_heap_is_freed() {
    // B-2026-09-02-35 — a by-value TUPLE ARGUMENT whose element carries a
    // `Vec` of heap-owning things leaked every one of that Vec's elements.
    //
    // The caller's tuple temporary was registered through the shallow
    // `track_tuple_var` fallback, whose LLVM-type-driven walker
    // (`emit_aggregate_heap_field_frees`) frees a `{ptr,len,cap}` field's
    // outer buffer and cannot recurse — the element type is erased at the
    // LLVM level. It took that fallback because `infer_arg_elem_te` could
    // not name a CALL element, so `(mkv(), 0)`'s element types came back as
    // EMPTY paths, read as no-drop, and the deep `synthesize_tuple_drop_fn_te`
    // branch right above it never fired.
    //
    // NO `match`, DESTRUCTURE OR MOVE-OUT IS INVOLVED, which is what
    // separates this from the whole B-2026-09-02-23/-27/-34 family it was
    // found next to: the argument is merely passed and read.
    //
    // Every cell allocates 45-byte strings, past any small-string
    // threshold, so the elements are real heap rather than inline bytes.
    //
    // THE CONTROLS ARE THE POINT, because the fix makes MORE type
    // information reach a registrar that decides whether to free: widening
    // it in the wrong direction turns a leak into a double free. `plain`
    // passes the same Vec as its own parameter, `local` never passes it at
    // all, and `handback` returns the element out of the callee so the
    // caller's binding owns it — the shape that would abort first if the
    // caller's temp started freeing a buffer the callee gave away. Looped,
    // so anything unbalanced compounds instead of cancelling.
    assert_clean_asan_run(
        r#"
fn pad(t: i64) -> String {
    let mut s: String = String.new();
    s.push_str("payload-padded-out-well-past-thirty-six-byte");
    s.push_str(f"{t}");
    return s;
}
fn mkv(n: i64) -> Vec[String] {
    let mut v: Vec[String] = Vec.new();
    v.push(pad(n));
    v.push(pad(n));
    return v;
}
fn mkvv() -> Vec[Vec[i64]] {
    let mut o: Vec[Vec[i64]] = Vec.new();
    let mut a: Vec[i64] = Vec.new();
    a.push(1);
    o.push(a);
    let mut b: Vec[i64] = Vec.new();
    b.push(3);
    o.push(b);
    return o;
}
struct H { id: i64, xs: Vec[String] }
fn mkh(id: i64) -> H { return H { id: id, xs: mkv(id) }; }
fn takev(t: (Vec[String], i64)) { println(f"v{t.0.len()}:{t.1}"); }
fn takevv(t: (Vec[Vec[i64]], i64)) { println(f"n{t.0.len()}:{t.1}"); }
fn takeh(t: (H, i64)) { println(f"h{t.0.xs.len()}:{t.1}"); }
fn plain(v: Vec[String]) { println(f"p{v.len()}"); }
fn handback(t: (Vec[String], i64)) -> Vec[String] { return t.0; }
fn main() {
    let mut i = 0;
    while i < 3 {
        takev((mkv(1), 0));
        takevv((mkvv(), 5));
        takeh((mkh(2), 9));
        plain(mkv(3));
        let t = (mkv(4), 7);
        println(f"l{t.0.len()}:{t.1}");
        let r = handback((mkv(5), 8));
        println(f"b{r.len()}");
        i = i + 1;
    }
    println("done");
}
"#,
        &[
            "v2:0", "n2:5", "h2:9", "p2", "l2:7", "b2", "v2:0", "n2:5", "h2:9", "p2", "l2:7", "b2",
            "v2:0", "n2:5", "h2:9", "p2", "l2:7", "b2", "done",
        ],
        "b0902-35-tuple-arg-vec-element-heap",
    );
}

/// B-2026-09-05-1 — the memory side of `e2e_shared_field_into_literal_and_push_aliases_source`
/// (tests/codegen.rs): a `shared` field copied into a struct literal, a
/// shared-holder literal, a deeper-place literal and a `push`, with the
/// source read afterwards (each was a null dereference), plus the
/// caller-retains param returns that must NOT be double-inc'd (each was a
/// 40 B leak mid-fix). ASAN + LSan, clean exit, exact stdout.
#[test]
fn asan_shared_field_into_literal_and_push_clean() {
    let label = "shared_field_into_literal_and_push";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan(
        r#"
shared struct S { id: i64, tag: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
struct W { s: S }
struct H { s: S }
shared struct Sh { s: S }
struct G { w: W }
fn pick(w: W) -> S { return w.s }
fn pick2(w: W) -> S { w.s }
fn pickr(w: ref W) -> S { return w.s }
fn main() {
    { let w: W = W { s: S { id: 1, tag: "a" } }; let h: H = H { s: w.s }; println(f"v{h.s.id}{w.s.id}"); }
    { let w: W = W { s: S { id: 2, tag: "a" } }; let h: Sh = Sh { s: w.s }; println(f"v{h.s.id}{w.s.id}"); }
    { let g: G = G { w: W { s: S { id: 3, tag: "a" } } }; let h: H = H { s: g.w.s }; println(f"v{h.s.id}{g.w.s.id}"); }
    { let w: W = W { s: S { id: 4, tag: "a" } }; let mut v: Vec[S] = []; v.push(w.s); println(f"v{v[0].id}{w.s.id}"); }
    { let w: W = W { s: S { id: 5, tag: "a" } }; let s: S = pick(w); println(f"v{s.id}"); }
    { let w: W = W { s: S { id: 6, tag: "a" } }; let s: S = pick2(w); println(f"v{s.id}"); }
    { let w: W = W { s: S { id: 7, tag: "a" } }; let s: S = pickr(w); println(f"v{s.id}{w.s.id}"); }
    { let w: W = W { s: S { id: 8, tag: "a" } }; let W { s } = w; println(f"v{s.id}"); }
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
            "v11", "dS1", "v22", "dS2", "v33", "dS3", "v44", "dS4", "v5", "dS5", "v6", "dS6",
            "v77", "dS7", "v8", "dS8", "end",
        ],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// B-2026-09-10-2 — a USER GENERIC enum's `Drop`-bearing payload, in every
/// position, on both the body channel and the memory one.
///
/// `enum G[T] { X(T), Y }` at `T = R2` ran NO `Drop` body anywhere and
/// stranded the payload's own heap — 27 B per value, the three `String`s —
/// while the monomorphic control `enum Mono { P(R2), Q }` in the same
/// positions was correct and clean. Only the enum's genericity differs, and
/// that is the whole of it: the by-value param the row was filed against
/// was never the axis (a DISCARDED LOCAL that is passed nowhere loses the
/// body identically), and neither was boxing (a one-word `R3` payload that
/// never boxes loses it too).
///
/// Three name-keyed gates each declined a generic-param payload, because a
/// generic enum's declared payload IS the parameter and resolving `T` by
/// name would match a user type called `T` (B-2026-08-03-5). That skip is
/// load-bearing and stays; what closes this is its exact COMPLEMENT, keyed
/// on the INSTANTIATION rather than the name, so the two partition the
/// variants and neither slot can be walked twice.
///
/// The `k*` cells are the coordination this needed. An arm that binds the
/// payload out takes the interior — whether it CONSUMES it (`let z = r`) or
/// only READS it (`r.s.len()`) — so the box drop must give up the interior
/// walk and the payload-bodies walker must retract, or the two owners free
/// the same buffer: a `double free detected in tcache 2` on the first pass
/// at this fix, caught here and not by the leak count.
///
/// `k1` is the row's own cell 4, a run-vs-build divergence that predates
/// this row's parent: it ran the body on the compiled backends and NOTHING
/// under `--interp`. It is fixed here rather than separately because the
/// interpreter mirrored the codegen gap ON PURPOSE — the two had to move
/// together or one of them would be wrong at every cell.
///
/// NOTE ON THE CONTROLS: every payload is built from an f-string, never a
/// string LITERAL. A literal is static and never reaches the allocator, so
/// a cell built from one sits at the baseline alloc count and proves
/// nothing about a leak — the row records losing a wrong answer to exactly
/// that for a while, reporting 0 bytes lost and reading as already-fixed.
/// B-2026-09-21-11 (memory twin) — the ORDERING, which is the half an
/// output oracle alone cannot certify.
///
/// The elements' `Drop` bodies now run from the arm's BINDING rather than
/// from the scrutinee husk, and the binding also owns the buffer those
/// bodies read. The cleanup frame drains LIFO, so the registration has to
/// be queued AFTER the buffer free to fire BEFORE it; queued where the
/// decision is taken, it fired last and read released memory — two garbage
/// ids and an invalid read of size 8. Correct output and a clean ASAN run
/// together are what separate the working order from the broken one, since
/// the broken one printed the right NUMBER of lines.
///
/// `vs` and `vw` fire their bodies; `vi` is a scalar element that owes
/// none and must not disturb the buffer free.
///
/// WHY THE TWO AGREED-GAP CELLS ARE NOT HERE, because their absence is a
/// measurement and not an oversight. This fixture carried `vd` (a
/// discarding arm beside a binding one) and `vo` (an `Option` payload)
/// under the assumption that an agreed gap is memory-clean — that failing
/// to run a `Drop` BODY costs only the body. It is not: at
/// `KARAC_OPT_LEVEL=0` the two of them leak 123 B between them, measured
/// in isolation as 82 B for `vd` (64 direct, the `Vec`'s own buffer, plus
/// 18 indirect in two `String`s) and 41 B for `vo` (32 direct plus 9
/// indirect in one). That is the whole payload, envelope included, not
/// just the elements' bodies — B-2026-09-21-12, which predates this row
/// and is open. The same three cells above measure 0 errors and 0 bytes
/// lost in the same run, which is what attributes the 123 B away from
/// this row's fix.
///
/// So the gap cells keep their OUTPUT pins in the codegen and interpreter
/// twins, where what is asserted is that no body runs, and they stay out
/// of here, where what is asserted is a clean run they cannot give. Put
/// them back only with the leak fixed; a `#[test]` that asserts a clean
/// run over a known leak is a red on the two ASAN ratchet legs and green
/// on every other leg, since `-O2` elides an allocation nothing observes.
#[test]
fn asan_arm_binding_vec_payload_elem_bodies_run_before_its_buffer_free() {
    assert_clean_asan_run(
        r#"
struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i, s: f"payload-{i}" }; }
struct W { v: R }
impl Drop for W { fn drop(mut ref self) { println("dW") } }
enum Slot[T] { S(T), N }

fn main() {
    let a: Vec[R] = [mkr(1), mkr(2)];
    match Slot.S(a) { Slot.S(v) => { println(f"vs{v[0].id}") } Slot.N => { println("no") } }

    let b: Vec[W] = [W { v: mkr(3) }, W { v: mkr(4) }];
    match Slot.S(b) { Slot.S(v) => { println("vw") } Slot.N => { println("no") } }

    let e: Vec[i64] = [1, 2];
    match Slot.S(e) { Slot.S(v) => { println(f"vi{v[0]}") } Slot.N => { println("no") } }
    println("end");
}
"#,
        &[
            // The fix, and the ORDER is the point: the bodies precede the
            // buffer free that would have released what they read.
            "vs1", "dR1", "dR2",
            // An element with a body of its own over a Drop-bearing field.
            "vw", "dW", "dR3", "dW", "dR4",
            // A scalar element owes no body and must not disturb the free.
            "vi1", "end",
        ],
        "asan_arm_binding_vec_payload_elem_bodies_run_before_its_buffer_free",
    );
}

/// B-2026-09-15-27 — a `Vec[Array[T, N]]`-typed STRUCT FIELD frees its
/// elements' heap. 34 B in 2 blocks definitely lost before the fix on a
/// plain `let h: H` with no reassignment anywhere.
///
/// The struct-field element policy `vec_element_drain_fn` resolves through
/// `vec_elem_agg_drop_for_type_expr` (name-keyed, plus a Tuple arm) and
/// then `elem_te_needs_direct_recursive_drain` (a literal list of head
/// names), and NEITHER has an `Array` case — so the field freed its buffer
/// and left every fixed array's interior unowned.
///
/// B-2026-09-10-8/-26 saw this exact position and put its recursion in
/// `emit_drop_fn_for_array` instead, on the ground that a
/// `Vec[Array[String, 2]]` built as `let e = [..]; v.push(e)` is already
/// owned by the source local, so widening the shared policy would make a
/// second owner. That is true of a `Vec` LOCAL — its cell 16 in
/// `asan_nested_fixed_array_elements_are_freed_at_every_position` is
/// unchanged by this fix and is the control for it — and FALSE of a FIELD:
/// the push-built spelling leaks as a field exactly as the literal one
/// does, because `v.push(e)` disarms `e` and moving `v` into the field
/// leaves nothing else owning the elements. Both spellings are cells here.
///
/// MUST be read at `-O0`: at `-O2` LLVM deletes the allocations nothing
/// observes, which is the `asan-o0-leg.sh` case.
///
/// BODIES are NOT asserted beyond `end`: a nested container in a `Vec`
/// field is still silent on both backends, which is B-2026-09-15-23's
/// subject. Bodies and memory are separate channels (B-2026-08-28-57) and
/// this row is the memory one.
#[test]
fn asan_vec_of_fixed_arrays_in_a_struct_field_frees_its_elements() {
    const H: &str = "struct D { id: i64, s: String }\n\
             impl Drop for D { fn drop(mut ref self) { println(f\"dD{self.id}\") } }\n\
             fn mkd(i: i64) -> D { return D { id: i, s: f\"tttttttttttttttt{i}\" } }\n\
             struct H { f: Vec[Array[D, 1]] }\n";
    // The reported shape: a Vec literal of array literals, no reassignment.
    assert_clean_asan_run(
        &format!(
            "{H}fn main() {{\n\
                 \x20   let h: H = H {{ f: [[mkd(1)], [mkd(2)]] }};\n\
                 \x20   println(\"end\");\n\
                 }}\n"
        ),
        &["dD1", "dD2", "end"],
        "b27-vec-of-arrays-field-literal",
    );
    // The PUSH-BUILT spelling, and the cell that refutes the premise the
    // recursion was originally kept out of the shared policy on: 3 B in 1
    // block before the fix, because the push disarms the source local and
    // moving the `Vec` into the field leaves no owner behind.
    assert_clean_asan_run(
        &format!(
            "{H}fn main() {{\n\
                 \x20   let mut v: Vec[Array[D, 1]] = [];\n\
                 \x20   let e1: Array[D, 1] = [mkd(1)];\n\
                 \x20   v.push(e1);\n\
                 \x20   let h: H = H {{ f: v }};\n\
                 \x20   println(\"end\");\n\
                 }}\n"
        ),
        &["dD1", "end"],
        "b27-vec-of-arrays-field-pushed",
    );
    // An element with more than one slot, so a fix that walks only index 0
    // is visible.
    assert_clean_asan_run(
        "struct D { id: i64, s: String }\n\
             impl Drop for D { fn drop(mut ref self) { println(f\"dD{self.id}\") } }\n\
             fn mkd(i: i64) -> D { return D { id: i, s: f\"tttttttttttttttt{i}\" } }\n\
             struct H { f: Vec[Array[D, 2]] }\n\
             fn main() {\n\
             \x20   let h: H = H { f: [[mkd(1), mkd(2)], [mkd(3), mkd(4)]] };\n\
             \x20   println(\"end\");\n\
             }\n",
        &["dD1", "dD2", "dD3", "dD4", "end"],
        "b27-vec-of-two-slot-arrays-field",
    );
    // CONTROL — the same type as a LOCAL, clean before and after. This is
    // the half the original reasoning got right, and a widening that
    // started double-freeing it aborts here.
    assert_clean_asan_run(
        &format!(
            "{H}fn main() {{\n\
                 \x20   let v: Vec[Array[D, 1]] = [[mkd(1)], [mkd(2)]];\n\
                 \x20   println(\"end\");\n\
                 }}\n"
        ),
        &["dD1", "dD2", "end"],
        "b27-vec-of-arrays-local-control",
    );
    // B-2026-09-15-26's shape on the memory channel: the flat `Array[D, N]`
    // FIELD, whose bodies that row restored. It was clean throughout —
    // pinned so the bodies fix cannot quietly acquire a second owner.
    assert_clean_asan_run(
        "struct D { id: i64, s: String }\n\
             impl Drop for D { fn drop(mut ref self) { println(f\"dD{self.id}\") } }\n\
             fn mkd(i: i64) -> D { return D { id: i, s: f\"tttttttttttttttt{i}\" } }\n\
             struct H { f: Array[D, 2] }\n\
             fn main() {\n\
             \x20   let h: H = H { f: [mkd(1), mkd(2)] };\n\
             \x20   println(\"end\");\n\
             }\n",
        &["dD1", "dD2", "end"],
        "b26-array-field-bodies-memory-control",
    );
}

/// B-2026-09-21-15 — the memory twin: a `Vec` enum payload whose ELEMENT is a generic
/// struct keeps its vec shape when bound in a match arm.
///
/// `pattern_binding_inner_types` holds the binding's own type for some
/// binding kinds and the ELEMENT type for a container (that is what the
/// `Array` / `Vector` arms read it for), and three separate sites asked
/// "is this a concretely-instantiated generic struct?" of it without
/// checking which. A `Vec[G[i64]]` binding records `bt = "Vec"` beside
/// `inner = G[i64]`, so all three answered with `G[i64]` and all three
/// outranked the explicit `"Vec" => 3 words / vec_struct_type()` arms
/// below them: the payload was sized at 1 word, which made the BOXED
/// payload look inline, and the deboxed `{ ptr, i64, i64 }` was rebuilt as
/// G's 1-field `{ i64 }` keeping only word 0. The binding held the buffer
/// POINTER where its length belonged, so `v.len()` returned an address —
/// an unbounded drop walk, then a SIGSEGV or `free(): double free detected
/// in tcache 2`, on a program `--interp` ran correctly.
///
/// The `b:` cell is the one-character control: `P` is the same struct with
/// its type parameter removed, and it always compiled correctly. It is
/// here so a regression that reaches only the generic side is still
/// distinguishable from one that breaks both.
///
/// No `Drop` impl is needed to trigger this — `a:` through `e:` carry
/// none. The `f:` cell adds one only to show the element bodies still fire
/// once each once the binding has the right shape.
///
/// The three sites are now one predicate
/// (`generic_struct_binding_type_expr`), which tests that the recorded
/// surface name agrees with the `TypeExpr`'s head — the thing the tier
/// always meant, and true of B-2026-07-12-2's `bt = "Wrap"` beside
/// `inner = Wrap[String]` that it exists for.
///
/// The ASAN channel is what the output oracle cannot see on its own. The
/// binding held a buffer POINTER in its length slot, so the drop walk read
/// element after element past the end of a two-element buffer — ten
/// million `Invalid read of size 8` in ninety seconds under valgrind
/// before the process died.
#[test]
fn asan_vec_payload_of_generic_struct_elements_keeps_its_vec_shape() {
    assert_clean_asan_run(
        r#"
struct G[T] { v: T }
struct P { v: i64 }
enum Slot[T] { S(T), N }

struct D { id: i64 }
impl Drop for D { fn drop(mut ref self) { println(f"dD{self.id}") } }
struct Gd[T] { d: T }

fn main() {
    let a: Vec[G[i64]] = [G { v: 1 }, G { v: 2 }];
    match Slot.S(a) { Slot.S(v) => { println(f"a:{v.len()}:{v[1].v}") } Slot.N => { println("no") } }

    let b: Vec[P] = [P { v: 3 }, P { v: 4 }];
    match Slot.S(b) { Slot.S(v) => { println(f"b:{v.len()}:{v[1].v}") } Slot.N => { println("no") } }

    let c: Vec[G[i64]] = [G { v: 5 }, G { v: 6 }];
    let s: Slot[Vec[G[i64]]] = Slot.S(c);
    match s { Slot.S(v) => { println(f"c:{v.len()}:{v[0].v}") } Slot.N => { println("no") } }

    let d: Vec[G[String]] = [G { v: "ab" }, G { v: "cd" }];
    match Option.Some(d) { Option.Some(v) => { println(f"d:{v.len()}:{v[1].v}") } Option.None => { println("no") } }

    let e: Vec[G[i64]] = [G { v: 7 }, G { v: 8 }];
    if let Slot.S(v) = Slot.S(e) { println(f"e:{v.len()}:{v[0].v}") }

    let f: Vec[Gd[D]] = [Gd { d: D { id: 1 } }, Gd { d: D { id: 2 } }];
    match Slot.S(f) { Slot.S(v) => { println(f"f:{v.len()}") } Slot.N => { println("no") } }

    println("end");
}
"#,
        &[
            "a:2:2", "b:2:4", "c:2:5", "d:2:cd", "e:2:7", "f:2", "dD1", "dD2", "end",
        ],
        "asan_vec_payload_of_generic_struct_elements_keeps_its_vec_shape",
    );
}

#[test]
/// B-2026-09-23-38 — a tuple member read TWO or more hops below a container
/// element (`v[0].1.0` over `Vec[(i64, (String, i64))]`) cloned the whole
/// intermediate tuple at the inner hop, registered no cleanup for a tuple
/// clone, and handed the leaf out of it: one leaked String per
/// `println(v[0].1.0)`. The chain is now walked to its container read and only
/// the LEAF is cloned, with the same cleanup and consuming-destination takeover
/// the one-hop `v[0].0` read has. Legs: non-consuming reads in a loop, eight
/// consuming destinations, three hops, a `Vec` leaf, and the whole inner tuple.
fn asan_nested_container_tuple_index_read_frees_its_copy() {
    assert_clean_asan_run_min_allocs(
        r#"
struct H { s: String }
fn take(s: String) -> i64 { s.len() }
fn pick(v: ref Vec[(i64, (String, i64))]) -> String { v[0].1.0 }
fn mkv() -> Vec[(i64, (String, i64))] { [(1, (f"alpha-{1}-long-enough-to-heap", 7)), (2, (f"beta-{2}-long-enough-to-heap", 8))] }
fn leg_reads() {
    let v = mkv();
    let mut i = 0;
    while i < 3 { println(v[1].1.0); i = i + 1; }
    println(f"rd {v[0].1.0} {v[0].1.0.len()} {v[0].1.0 == "x"} {v[0].1.1}");
}
fn leg_consumers() {
    let v = mkv();
    let s = v[0].1.0;
    let mut o: Vec[String] = [];
    o.push(v[1].1.0);
    o.push(v[0].1.0);
    let h = H { s: v[1].1.0 };
    let t = (v[0].1.0, 3);
    let mut a = String.new();
    a = v[1].1.0;
    println(f"co {s} {o.len()} {o[1]} {h.s} {t.0} {a} {take(v[0].1.0)} {pick(v)}");
    println(f"co {v[0].1.0} {v[1].1.0}");
}
fn leg_three_levels() {
    let v: Vec[(i64, (i64, (String, i64)))] = [(1, (2, (f"gamma-{3}-long-enough-to-heap", 7)))];
    println(v[0].1.1.0);
    let s = v[0].1.1.0;
    println(f"tl {s} {v[0].1.1.1}");
}
fn leg_vec_leaf() {
    let v: Vec[(i64, (Vec[String], i64))] = [(1, ([f"delta-{4}-long-enough-to-heap"], 7))];
    println(f"vl {v[0].1.0.len()}");
    let w = v[0].1.0;
    println(f"vl {w[0]} {v[0].1.0[0]}");
}
fn leg_whole_inner() {
    let v = mkv();
    let t = v[0].1;
    println(f"wi {t.0} {t.1} {v[0].1.0}");
}
fn main() {
    leg_reads();
    leg_consumers();
    leg_three_levels();
    leg_vec_leaf();
    leg_whole_inner();
    println("done");
}
"#,
        &[
            "beta-2-long-enough-to-heap",
            "beta-2-long-enough-to-heap",
            "beta-2-long-enough-to-heap",
            "rd alpha-1-long-enough-to-heap 27 false 7",
            "co alpha-1-long-enough-to-heap 2 alpha-1-long-enough-to-heap beta-2-long-enough-to-heap alpha-1-long-enough-to-heap beta-2-long-enough-to-heap 27 alpha-1-long-enough-to-heap",
            "co alpha-1-long-enough-to-heap beta-2-long-enough-to-heap",
            "gamma-3-long-enough-to-heap",
            "tl gamma-3-long-enough-to-heap 7",
            "vl 1",
            "vl delta-4-long-enough-to-heap delta-4-long-enough-to-heap",
            "wi alpha-1-long-enough-to-heap 7 alpha-1-long-enough-to-heap",
            "done",
        ],
        "asan_nested_container_tuple_index_read_frees_its_copy",
        15,
    );
}

/// B-2026-09-24-2 — the memory half: a tuple-hop field read clones only its
/// LEAF (no whole-struct copy of the receiver left behind), and an enum field
/// moved out by `match` / `if let` through the same hops is a clone the arm
/// owns, so the container frees its own payload exactly once.
#[test]
fn asan_tuple_hop_enum_field_move_frees_once() {
    assert_clean_asan_run_min_allocs(
        r#"
enum K { A(String), B }
struct H { s: String, n: i64 }
struct G { k: K, n: i64 }
struct C { xs: Vec[(i64, (H, i64))] }
impl C { fn show(ref self) -> String { f"c {self.xs[0].1.0.s} {self.xs[0].1.0.n}" } }
fn mk(i: i64) -> String { f"alpha-{i}-long-enough-to-heap" }
fn by_ref(v: ref Vec[(i64, (H, i64))]) -> i64 { v[0].1.0.s.len() + v[0].1.0.n }
fn field_reads() {
    let v: Vec[(i64, (H, i64))] = [(1, (H { s: mk(1), n: 9 }, 2))];
    let mut i = 0;
    while i < 2 { println(f"{v[0].1.0.n} {v[0].1.0.s} {v[0].1.0.s.len()}"); i = i + 1; }
    let a = v[0].1.0.s;
    println(f"a {a} {by_ref(v)}");
    let w: Vec[(i64, (i64, (H, i64)))] = [(1, (2, (H { s: mk(2), n: 8 }, 3)))];
    println(f"w {w[0].1.1.0.s} {w[0].1.1.0.n}");
    let c = C { xs: [(1, (H { s: mk(3), n: 7 }, 2))] };
    println(c.show());
}
fn enum_fields() {
    let g1: Vec[(i64, G)] = [(1, G { k: K.A(mk(4)), n: 6 })];
    let g2: Vec[(i64, (G, i64))] = [(1, (G { k: K.A(mk(5)), n: 5 }, 2))];
    match g1[0].1.k { K.A(s) => println(f"k1 {s.len()} {s}"), K.B => println("k1 b") }
    match g2[0].1.0.k { K.A(_) => println("k2 a"), K.B => println("k2 b") }
    let mut out: Vec[String] = [];
    match g1[0].1.k { K.A(s) => out.push(s), K.B => {} }
    match g2[0].1.0.k { K.A(s) => out.push(s), K.B => {} }
    if let K.A(s) = g2[0].1.0.k { out.push(s); }
    println(f"m {out.len()} {out[0]} {out[1]} {out[2]}");
    println(f"n {g1[0].1.n} {g2[0].1.0.n}");
}
fn main() {
    field_reads();
    enum_fields();
    println("done");
}
"#,
        &[
            "9 alpha-1-long-enough-to-heap 27",
            "9 alpha-1-long-enough-to-heap 27",
            "a alpha-1-long-enough-to-heap 36",
            "w alpha-2-long-enough-to-heap 8",
            "c alpha-3-long-enough-to-heap 7",
            "k1 27 alpha-4-long-enough-to-heap",
            "k2 a",
            "m 3 alpha-4-long-enough-to-heap alpha-5-long-enough-to-heap alpha-5-long-enough-to-heap",
            "n 6 5",
            "done",
        ],
        "asan_tuple_hop_enum_field_move_frees_once",
        8,
    );
}

/// B-2026-09-24-2 (follow-up) — the enum-field scrutinee clone for a struct
/// reached through tuple hops below a container element must take a USER enum
/// only. An `Option` field's clone has no owner, so extending the clone to it
/// leaked the copy: 27 B definitely lost on `match g[0].1.o { Some(s) => … }`,
/// read-only or consuming, at one and two hops.
#[test]
fn asan_tuple_hop_option_field_match_takes_no_orphan_copy() {
    assert_clean_asan_run_min_allocs(
        r#"
struct G { o: Option[String], n: i64 }
fn mk(i: i64) -> String { f"alpha-{i}-long-enough-to-heap" }
fn main() {
    let g1: Vec[(i64, G)] = [(1, G { o: Some(mk(6)), n: 5 })];
    match g1[0].1.o { Some(s) => println(s), None => {} }
    let g2: Vec[(i64, G)] = [(1, G { o: Some(mk(7)), n: 4 })];
    let mut out: Vec[String] = [];
    match g2[0].1.o { Some(s) => out.push(s), None => {} }
    let g3: Vec[(i64, (G, i64))] = [(1, (G { o: Some(mk(8)), n: 3 }, 2))];
    match g3[0].1.0.o { Some(s) => out.push(s), None => {} }
    println(f"{out.len()} {out[0]} {out[1]} {g1[0].1.n}");
}
"#,
        &[
            "alpha-6-long-enough-to-heap",
            "2 alpha-7-long-enough-to-heap alpha-8-long-enough-to-heap 5",
        ],
        "asan_tuple_hop_option_field_match_takes_no_orphan_copy",
        6,
    );
}

// B-2026-09-24-6 — reading an `Option`/`Result` field out of a container
// element (`match g[0].o`, `let x = g[0].o`, `z = g[0].o`, and through a
// tuple hop) copies the payload and leaves the element intact, as the
// interpreter does; and a tuple holding a struct whose heap hangs off an
// `Option` field frees it.
#[test]
fn asan_elem_optres_field_reads_copy_and_free_once() {
    assert_clean_asan_run_min_allocs(
        r#"
struct G { o: Option[String], r: Result[String, i64], n: i64 }
fn mk(i: i64) -> String { f"alpha-{i}-long-enough-to-heap" }
fn mg(i: i64) -> G { G { o: Some(mk(i)), r: Ok(mk(i + 10)), n: i } }
fn cons(o: Option[String]) -> i64 { match o { Some(s) => s.len(), None => 0 } }
fn main() {
    let g: Vec[G] = [mg(1), mg(2)];
    let mut out: Vec[String] = [];
    match g[0].o { Some(s) => println(s), None => {} }
    match g[0].o { Some(s) => out.push(s), None => {} }
    if let Some(s) = g[0].o { out.push(s); }
    match g[1].r { Ok(s) => out.push(s), Err(_) => {} }
    let x = g[1].o;
    let y = g[1].o;
    let mut z: Option[String] = None;
    z = g[0].o;
    println(f"{out.len()} {cons(x)} {cons(y)} {cons(z)} {g[0].o.is_some()}");
    let h: Vec[(i64, G)] = [(1, mg(3))];
    match h[0].1.o { Some(s) => out.push(s), None => {} }
    match h[0].1.o { Some(s) => out.push(s), None => {} }
    let mut q: Option[String] = None;
    q = h[0].1.o;
    q = h[0].1.o;
    println(f"{out.len()} {cons(q)} {out[4]}");
    let t = (7, mg(4));
    println(f"{t.0} {t.1.n}");
}
"#,
        &[
            "alpha-1-long-enough-to-heap",
            "3 27 27 27 true",
            "5 27 alpha-3-long-enough-to-heap",
            "7 4",
        ],
        "asan_elem_optres_field_reads_copy_and_free_once",
        8,
    );
}

// B-2026-09-24-17 — a consuming match over a container element's `Option`
// field whose pattern binds PART of the payload (a nested variant
// `Some(K.A(s))`, a tuple `Some((s, k))`, a two-field variant with a `_`) or a
// `Map` payload copies each binding out and leaves the element intact, as the
// interpreter does. Before the fix these emptied the element (a second match
// found nothing), leaked a `_` field, or double-freed (`if let` and the tuple).
#[test]
fn asan_elem_optres_field_nested_patterns_copy_and_free_once() {
    assert_clean_asan_run_min_allocs(
        r#"
enum K { A(String), B }
enum K2 { A(String, i64), B }
struct G { q: Option[K], w: Option[K2], t: Option[(String, i64)], m: Option[Map[i64, String]], r: Result[K, String] }
fn mk(i: i64) -> String { f"alpha-{i}-long-enough-to-heap" }
fn mg(i: i64) -> G {
    let mut mm: Map[i64, String] = Map.new();
    mm.insert(i, mk(i));
    G { q: Some(K.A(mk(i))), w: Some(K2.A(mk(i), 5)), t: Some((mk(i), 3)), m: Some(mm), r: Ok(K.A(mk(i))) }
}
fn main() {
    let g: Vec[G] = [mg(1), mg(22)];
    let mut keep: Vec[String] = [];
    let mut n = 0;
    match g[0].q { Some(K.A(s)) => { n = n + s.len(); } _ => {} }
    match g[0].q { Some(K.A(s)) => { n = n + s.len(); } _ => {} }
    if let Some(K.A(s)) = g[0].q { n = n + s.len(); }
    match g[0].q { Some(k) => { match k { K.A(s) => { n = n + s.len(); } K.B => {} } } None => {} }
    match g[0].q { Some(K.A(s)) if s.len() > 100 => { n = n + 1000; } Some(K.A(s)) => { keep.push(s); } _ => {} }
    println(f"q {n} {keep.len()}");
    n = 0;
    match g[0].w { Some(K2.A(_, k)) => { n = n + k; } _ => {} }
    match g[0].w { Some(K2.A(s, k)) => { n = n + s.len() + k; } _ => {} }
    match g[0].t { Some((s, k)) => { keep.push(s); n = n + k; } _ => {} }
    match g[0].t { Some((s, _)) => { n = n + s.len(); } _ => {} }
    match g[1].r { Ok(K.A(s)) => { n = n + s.len(); } _ => {} }
    match g[1].r { Ok(K.A(s)) => { n = n + s.len(); } _ => {} }
    println(f"w {n} {keep.len()}");
    n = 0;
    match g[1].m { Some(m) => { n = n + m.len(); } None => {} }
    if let Some(m) = g[1].m { n = n + m.get(22).unwrap().len(); }
    match g[1].m { Some(m) => { n = n + m.len(); } None => {} }
    println(f"m {n} {keep[0]} {keep[1]}");
}
"#,
        &[
            "q 108 1",
            "w 123 2",
            "m 30 alpha-1-long-enough-to-heap alpha-1-long-enough-to-heap",
        ],
        "asan_elem_optres_field_nested_patterns_copy_and_free_once",
        8,
    );
}

// B-2026-09-24-24 / B-2026-09-24-26 — reading a container element's `Option`
// field copies it and leaves the element intact, as the interpreter does, in
// the spellings the first fix missed: a `let` copy of a boxed enum or a `Map`
// payload, a struct sub-pattern `Some(P { name, k })`, a whole-struct binding
// `Some(pp)` moved on (`let q = pp`), and a `for` loop variable's fields
// (match, `if let`, `let`, and a struct holding a tuple `Option`). Before the
// fix these emptied the element, leaked the copy, or double-freed.
#[test]
fn asan_elem_optres_field_let_struct_pattern_and_loop_free_once() {
    assert_clean_asan_run_min_allocs(
        r#"
struct P { name: String, k: i64 }
enum K { A(String), B }
struct G { o: Option[String], p: Option[P], q: Option[K], m: Option[Map[i64, String]], t: Option[(String, i64)], n: i64 }
fn mk(i: i64) -> String { f"alpha-{i}-long-enough-to-heap" }
fn mg(i: i64) -> G {
    let mut mm: Map[i64, String] = Map.new();
    mm.insert(i, mk(i));
    G { o: Some(mk(i)), p: Some(P { name: mk(i), k: i }), q: Some(K.A(mk(i))), m: Some(mm), t: Some((mk(i), 3)), n: i }
}
fn main() {
    let g: Vec[G] = [mg(1), mg(22)];
    let mut n = 0;
    let a = g[0].q;
    match a { Some(K.A(s)) => { n = n + s.len(); } _ => {} }
    match g[0].q { Some(K.A(s)) => { n = n + s.len(); } _ => {} }
    let b = g[0].m;
    match b { Some(m) => { n = n + m.len(); } None => {} }
    match g[0].m { Some(m) => { n = n + m.len(); } None => {} }
    println(f"let {n}");
    n = 0;
    match g[0].p { Some(P { name, k }) => { n = n + name.len() + k; } _ => {} }
    match g[0].p { Some(pp) => { n = n + pp.name.len(); } _ => {} }
    match g[0].p { Some(pp) => { let q = pp; n = n + q.name.len(); } _ => {} }
    if let Some(P { name, k }) = g[0].p { n = n + name.len() + k; }
    println(f"pat {n}");
    n = 0;
    for x in g {
        match x.o { Some(s) => { n = n + s.len(); } None => {} }
        if let Some(K.A(s)) = x.q { n = n + s.len(); }
        let q = x.q;
        match q { Some(K.A(s)) => { n = n + s.len(); } _ => {} }
        match x.p { Some(P { name, k }) => { n = n + name.len() + k; } _ => {} }
        match x.t { Some((s, k)) => { n = n + s.len() + k; } _ => {} }
    }
    println(f"loop {n}");
}
"#,
        &["let 56", "pat 110", "loop 304"],
        "asan_elem_optres_field_let_struct_pattern_and_loop_free_once",
        8,
    );
}

// B-2026-09-24-25 — `.clone()` on an `Option` whose payload is a `Map`, a
// `Set`, a tuple (with a `Vec` or a `Map` in it) or an `Array` makes an
// independent copy, as the interpreter does; before the fix it segfaulted. An
// unconsumed clone (`let cs2 = os2.clone()`) is freed.
#[test]
fn asan_option_clone_map_tuple_array_payload_free_once() {
    assert_clean_asan_run_min_allocs(
        r#"
fn mk(i: i64) -> String { f"alpha-{i}-long-enough-to-heap" }
fn main() {
    let mut mm: Map[i64, String] = Map.new();
    mm.insert(1, mk(1));
    let o: Option[Map[i64, String]] = Some(mm);
    let mut c = o.clone();
    match c { Some(ref_m) => { let mut m2 = ref_m; m2.insert(2, mk(2)); println(f"c {m2.len()}"); } None => {} }
    match o { Some(m) => println(f"o {m.len()}"), None => println("none") }
    let mut st: Set[String] = Set.new();
    st.insert(mk(3));
    let os: Option[Set[String]] = Some(st);
    let cs = os.clone();
    match cs { Some(s) => println(f"cs {s.len()}"), None => {} }
    match os { Some(s) => println(f"os {s.len()}"), None => {} }
    let ot: Option[(Vec[String], i64)] = Some(([mk(4), mk(5)], 9));
    let ct = ot.clone();
    match ct { Some((v, k)) => println(f"ct {v.len()} {k} {v[1]}"), None => {} }
    match ot { Some((v, k)) => println(f"ot {v.len()} {k}"), None => {} }
    let oa: Option[Array[String, 2]] = Some([mk(6), mk(7)]);
    let ca = oa.clone();
    match ca { Some(a) => println(f"ca {a[0]}"), None => {} }
    match oa { Some(a) => println(f"oa {a[1]}"), None => {} }
    let on: Option[Map[i64, String]] = None;
    let cn = on.clone();
    println(f"none {cn.is_none()}");
    let mut m9: Map[i64, String] = Map.new(); m9.insert(9, mk(9)); let ow: Option[(String, Map[i64, String])] = Some((mk(8), m9));
    let cw = ow.clone();
    match cw { Some((s, m)) => println(f"cw {s} {m.len()}"), None => {} }
    match ow { Some((s, m)) => println(f"ow {s.len()} {m.len()}"), None => {} }
    let os2: Option[String] = Some(mk(10));
    let cs2 = os2.clone();
    println(f"str {cs2.is_some()} {os2.is_some()}");
}
"#,
        &[
            "c 2",
            "o 1",
            "cs 1",
            "os 1",
            "ct 2 9 alpha-5-long-enough-to-heap",
            "ot 2 9",
            "ca alpha-6-long-enough-to-heap",
            "oa alpha-7-long-enough-to-heap",
            "none true",
            "cw alpha-8-long-enough-to-heap 1",
            "ow 27 1",
            "str true true",
        ],
        "asan_option_clone_map_tuple_array_payload_free_once",
        8,
    );
}

// B-2026-09-24-22 — a consuming match (or a `let`) over an `Option`/`Result`
// field of a match PAYLOAD binding (`It.S(n) => match n.doc { Some(s) => .. }`
// in a by-value enum param) copies the payload out, since the enum still owns
// and frees it. Before the fix it zeroed only the binding's bit-copy and the
// payload was freed twice.
#[test]
fn asan_payload_binding_optres_field_match_free_once() {
    assert_clean_asan_run_min_allocs(
        r#"
struct N { doc: Option[String], k: i64 }
struct R { doc: Result[String, i64], k: i64 }
enum It { S(N), T(R), E(i64) }
fn show(it: It) -> String {
    match it {
        It.S(n) => match n.doc { Some(s) => f"s{n.k} {s}", None => f"s{n.k} none" },
        It.T(r) => match r.doc { Ok(s) => f"t{r.k} {s}", Err(e) => f"t{r.k} {e}" },
        It.E(x) => f"e{x}",
    }
}
fn show_let(it: It) -> String {
    match it {
        It.S(n) => { let d = n.doc; match d { Some(s) => f"l{n.k} {s}", None => f"l{n.k} none" } }
        _ => "other",
    }
}
fn main() {
    println(show(It.S(N { doc: Some(f"heap-string-longer-than-sso-1"), k: 1 })));
    println(show(It.T(R { doc: Ok(f"heap-string-longer-than-sso-2"), k: 2 })));
    println(show(It.E(3)));
    println(show_let(It.S(N { doc: Some(f"heap-string-longer-than-sso-4"), k: 4 })));
    let v: Vec[N] = [N { doc: Some(f"heap-string-longer-than-sso-5"), k: 5 }];
    let mut t = 0;
    for n in v { let d = n.doc; match d { Some(s) => { t = t + s.len(); } None => {} } }
    println(f"loop {t}");
}
"#,
        &[
            "s1 heap-string-longer-than-sso-1",
            "t2 heap-string-longer-than-sso-2",
            "e3",
            "l4 heap-string-longer-than-sso-4",
            "loop 29",
        ],
        "asan_payload_binding_optres_field_match_free_once",
        8,
    );
}

/// B-2026-09-24-19 — the ASAN twin of
/// `test_e2e_generic_fn_by_value_optres_param_has_one_owner`: a generic
/// function's by-value `Option[T]` / `Result[T, E]` param frees its payload
/// once, named or temporary.
#[test]
fn asan_generic_fn_by_value_optres_param_frees_once() {
    assert_clean_asan_run_min_allocs(
        r#"fn pick[T](a: Option[T], d: T) -> T { match a { Some(s) => s, None => d } }
fn peek[T](a: Option[T]) -> i64 { match a { Some(_) => 1, None => 0 } }
fn st[T](a: Option[T]) -> Vec[Option[T]] { let mut v = Vec.new(); v.push(a); v }
fn rb[T](a: Option[T]) -> i64 { let c = a; match c { Some(_) => 1, None => 0 } }
fn rpick[T](a: Result[T, String], d: T) -> T { match a { Ok(s) => s, Err(_) => d } }
fn main() {
    let a = Some(f"heap-string-longer-than-sso-1"); println(pick(a, f"d"));
    println(pick(Some(f"heap-string-longer-than-sso-2"), f"d"));
    println(f"p{peek(Some(f"heap-string-longer-than-sso-3"))}");
    let b = Some(f"heap-string-longer-than-sso-4"); let v = st(b); println(f"s{v.len()}");
    let w = st(Some(f"heap-string-longer-than-sso-5")); println(f"s{w.len()}");
    println(f"r{rb(Some(f"heap-string-longer-than-sso-6"))}");
    let c = Some(f"heap-string-longer-than-sso-7"); println(f"r{rb(c)}");
    let e: Result[String, String] = Err(f"heap-string-longer-than-sso-e8"); println(rpick(e, f"heap-string-longer-than-sso-d8"));
    let o: Result[String, String] = Ok(f"heap-string-longer-than-sso-o9"); println(rpick(o, f"d"));
    println("end")
}
"#,
        &[
            "heap-string-longer-than-sso-1",
            "heap-string-longer-than-sso-2",
            "p1",
            "s1",
            "s1",
            "r1",
            "r1",
            "heap-string-longer-than-sso-d8",
            "heap-string-longer-than-sso-o9",
            "end",
        ],
        "asan_generic_fn_by_value_optres_param_frees_once",
        8,
    );
}

/// B-2026-09-24-27 — a DISCARDED tuple literal (`(s, 1);`, `let _ = (s, 1);`)
/// that names a `String` or `Vec` local takes nothing over, so the local keeps
/// its free. Each discarded cell below leaked its buffer at `-O0` before; the
/// call nested in a literal (`(f(g), 6)`) still moves `g`.
#[test]
fn asan_discarded_tuple_literal_keeps_its_string_and_vec_sources() {
    assert_clean_asan_run_min_allocs(
        r#"fn f(s: String) -> i64 { s.len() }
fn mk(i: i64) -> String { f"heap-string-longer-than-sso-{i}" }
fn main() {
    let mut n = 0;
    while n < 2 {
        let a = mk(n + 1); println(a); let _ = (a, 1);
        let b = mk(n + 2); println(b); (b, 2);
        let c: Vec[i64] = [n, 2, 3]; println(f"c{c[0]}"); let _ = (c, 3);
        let d = mk(n + 4); println(d); let label: Option[String] = Some(mk(n + 40)); (label, d);
        let e = mk(n + 5); println(e); let _ = ((e, 5), 5);
        let g = mk(n + 6); println(f"n{(f(g), 6).0}");
        let h = mk(n + 7); println(h); let x = if n > 0 { (h, 7); 3 } else { 4 }; println(f"x{x}");
        n = n + 1;
    }
    println("end")
}
"#,
        &[
            "heap-string-longer-than-sso-1",
            "heap-string-longer-than-sso-2",
            "c0",
            "heap-string-longer-than-sso-4",
            "heap-string-longer-than-sso-5",
            "n29",
            "heap-string-longer-than-sso-7",
            "x4",
            "heap-string-longer-than-sso-2",
            "heap-string-longer-than-sso-3",
            "c1",
            "heap-string-longer-than-sso-5",
            "heap-string-longer-than-sso-6",
            "n29",
            "heap-string-longer-than-sso-8",
            "x3",
            "end",
        ],
        "asan_discarded_tuple_literal_keeps_its_string_and_vec_sources",
        8,
    );
}

/// B-2026-09-24-20 — a by-value `Result[S, i64]` param rebound in the callee
/// (`let c = a;`) frees its payload once: the callee's rebind is a view and the
/// caller owns the temporary. It leaked before, and the arm-binding spelling
/// double-freed.
#[test]
fn asan_optres_param_rebound_in_callee_frees_once() {
    assert_clean_asan_run_min_allocs(
        r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct S { r: R, s: String }
fn mk(i: i64) -> S { S { r: R { id: i }, s: f"heap-string-longer-than-sso-{i}" } }
fn eat(o: Option[R]) -> i64 { match o { Some(r) => r.id, None => 0 } }
fn keep(a: Option[R]) -> i64 { let c = a; 5 }
fn look(a: Option[R]) -> i64 { let c = a; match c { Some(r) => r.id, None => 0 } }
fn fwd(a: Option[R]) -> i64 { let c = a; eat(c) }
fn tag(a: Option[R]) -> i64 { let c = a; match c { Some(_) => 1, None => 0 } }
fn rkeep(a: Result[S, i64]) -> i64 { let c = a; 5 }
fn rtag(a: Result[S, i64]) -> i64 { let c = a; match c { Ok(_) => 1, Err(e) => e } }
fn main() {
    { let a = Some(R { id: 1 }); println(f"k{keep(a)}"); }
    println(f"k{keep(Some(R { id: 2 }))}");
    { let a = Some(R { id: 3 }); println(f"k{look(a)}"); }
    println(f"k{look(Some(R { id: 4 }))}");
    { let a = Some(R { id: 5 }); println(f"k{fwd(a)}"); }
    { let a = Some(R { id: 6 }); println(f"k{tag(a)}"); }
    println(f"k{tag(Some(R { id: 7 }))}");
    { let a: Result[S, i64] = Ok(mk(8)); println(f"k{rkeep(a)}"); }
    println(f"k{rkeep(Ok(mk(9)))}");
    { let a: Result[S, i64] = Ok(mk(10)); println(f"k{rtag(a)}"); }
    println(f"k{rtag(Ok(mk(11)))}");
    println("end")
}
"#,
        &[
            "k5", "d1", "d2", "k5", "k3", "d3", "d4", "k4", "k5", "d5", "k1", "d6", "d7", "k1",
            "k5", "d8", "d9", "k5", "k1", "d10", "d11", "k1", "end",
        ],
        "asan_optres_param_rebound_in_callee_frees_once",
        4,
    );
}

/// B-2026-09-24-38 / B-2026-09-24-36 — memory twin of
/// `test_e2e_optres_param_bodiless_escape_keeps_caller_body`.
#[test]
fn asan_optres_param_bodiless_escape_frees_once() {
    assert_clean_asan_run_min_allocs(
        r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct S { r: R, s: String }
fn mk(i: i64) -> S { S { r: R { id: i }, s: f"heap-string-longer-than-sso-{i}" } }
fn tag(a: Result[S, i64]) -> i64 { match a { Ok(_) => 1, Err(e) => e } }
fn read(a: Result[S, i64]) -> i64 { match a { Ok(x) => x.r.id, Err(e) => e } }
fn held(a: Result[S, i64]) -> i64 { match a { Ok(x) => { let n = x.r.id; println("mid"); n }, Err(e) => e } }
fn fwd(a: Result[S, i64]) -> i64 { let c = a; read(c) }
fn peek(a: Option[S]) -> i64 { match a { Some(x) => x.r.id, None => 0 } }
fn main() {
    { let a: Result[S, i64] = Ok(mk(1)); println(f"k{tag(a)}"); }
    println(f"k{tag(Ok(mk(2)))}");
    { let a: Result[S, i64] = Ok(mk(3)); println(f"k{read(a)}"); }
    println(f"k{read(Ok(mk(4)))}");
    { let e: Result[S, i64] = Err(9); println(f"k{read(e)}"); }
    { let a: Result[S, i64] = Ok(mk(5)); println(f"k{fwd(a)}"); }
    { let a: Result[S, i64] = Ok(mk(6)); println(f"k{held(a)}"); }
    println(f"k{held(Ok(mk(7)))}");
    println(f"k{peek(Some(mk(8)))}");
    println("end")
}
"#,
        &[
            "k1", "d1", "d2", "k1", "k3", "d3", "d4", "k4", "k9", "k5", "d5", "mid", "k6", "d6",
            "mid", "d7", "k7", "d8", "k8", "end",
        ],
        "asan_optres_param_bodiless_escape_frees_once",
        4,
    );
}

/// B-2026-09-24-33 — see `test_e2e_optres_array_payload_handed_out_frees_once`.
#[test]
fn asan_optres_array_payload_handed_out_frees_once() {
    assert_clean_asan_run_min_allocs(
        r#"struct W { a: Array[String, 2] }
fn pick[T](a: Option[T], d: T) -> T { match a { Some(s) => s, None => d } }
fn orelse[T](a: Option[T], d: T) -> T { match a { Some(s) => s, None => d } }
fn one(a: Option[Array[String, 1]]) -> Array[String, 1] { match a { Some(s) => s, None => panic("n") } }
fn two(a: Option[Array[String, 2]]) -> Array[String, 2] { match a { Some(s) => { s }, None => [f"d", f"e"] } }
fn three(a: Option[Array[String, 3]]) -> Array[String, 3] { match a { Some(s) => s, None => panic("n") } }
fn early(a: Option[Array[String, 2]], c: bool) -> Array[String, 2] { if let Some(s) = a { if c { return s; } println(f"kept {s[1]}") } [f"d", f"e"] }
fn held(a: Option[Array[String, 1]]) -> i64 { let r: Array[String, 1] = match a { Some(s) => s, None => panic("n") }; r[0].len() }
fn rebind(a: Option[Array[String, 2]]) -> Array[String, 2] { match a { Some(s) => { let t = s; t }, None => panic("n") } }
fn rewrap(a: Option[Array[String, 2]]) -> Option[Array[String, 2]] { match a { Some(s) => Some(s), None => None } }
fn mk(i: i64) -> Option[Array[String, 2]] { Some([f"heap-string-longer-than-sso-{i}", f"r{i}"]) }
fn main() {
    let a = Some([f"heap-string-longer-than-sso-1", f"x"]);
    let dd = [f"d", f"e"];
    let r = pick(a, dd);
    println(f"{r[0]} {r.len()}");
    let n: Option[Vec[String]] = None;
    let q = orelse(n, [f"heap-string-longer-than-sso-2", f"y"]);
    println(q[0]);
    let b: Option[Array[String, 1]] = Some([f"heap-string-longer-than-sso-3"]);
    println(one(b)[0]);
    let t = two(mk(4));
    println(t[0]);
    println(two(Some([f"heap-string-longer-than-sso-5", f"w"]))[0]);
    let e: Option[Array[String, 3]] = Some([f"heap-string-longer-than-sso-6", f"u", f"v"]);
    println(three(e)[2]);
    println(early(mk(7), false)[0]);
    println(early(mk(8), true)[0]);
    let k: Option[Array[String, 1]] = Some([f"heap-string-longer-than-sso-9"]);
    println(f"{held(k)}");
    println(rebind(mk(10))[1]);
    match rewrap(mk(11)) { Some(s) => println(s[0]), None => {} }
    let m = mk(12);
    let local = match m { Some(s) => s, None => panic("n") };
    println(local[0]);
    let mut v: Vec[Array[String, 2]] = Vec.new();
    match mk(13) { Some(s) => v.push(s), None => {} }
    println(v[0][1]);
    let w = match mk(14) { Some(s) => W { a: s }, None => panic("n") };
    println(w.a[0]);
    println("end")
}
"#,
        &[
            "heap-string-longer-than-sso-1 2",
            "heap-string-longer-than-sso-2",
            "heap-string-longer-than-sso-3",
            "heap-string-longer-than-sso-4",
            "heap-string-longer-than-sso-5",
            "v",
            "kept r7",
            "d",
            "heap-string-longer-than-sso-8",
            "29",
            "r10",
            "heap-string-longer-than-sso-11",
            "heap-string-longer-than-sso-12",
            "r13",
            "heap-string-longer-than-sso-14",
            "end",
        ],
        "asan_optres_array_payload_handed_out_frees_once",
        20,
    );
}

/// B-2026-09-25-7 — a generic fn's monomorph is shared by every caller of the
/// instantiation, and whether its body deep-copied a bare `x: T` param
/// depended on the FIRST caller's argument spelling: a named binding exposed
/// the element and got a copy, a temporary did not and got a move. So a
/// temp-first pair double-freed the named caller's buffer, and a named-first
/// pair leaked the temporary. Each generic below is called both ways, in one
/// order or the other, across return, `Option` payload, `if` branch,
/// forwarding, `Vec[i64]` and `VecDeque[String]`.
#[test]
fn asan_generic_mono_shared_by_temp_and_named_args_frees_once() {
    assert_clean_asan_run_min_allocs(
        r#"fn id2[T](d: T) -> T { d }
fn id3[T](d: T) -> T { d }
fn wrap[T](x: T) -> Option[T] { Some(x) }
fn sel[T](c: bool, x: T, y: T) -> T { if c { x } else { y } }
fn inner[T](d: T) -> T { d }
fn outer[U](y: U) -> U { inner(y) }
fn ints[T](x: T) -> T { x }
fn dq[T](x: T) -> T { x }
fn mkq(s: String) -> VecDeque[String] { let mut d = VecDeque[String].new(); d.push_back(s); d }
fn main() {
    let a = id2([f"heap-string-longer-than-sso-1", f"x"]);
    let n1 = [f"heap-string-longer-than-sso-2", f"y"];
    let b = id2(n1);
    println(f"{a[0]} {b[0]}");
    let n2 = [f"heap-string-longer-than-sso-3", f"z"];
    let c = id3(n2);
    let d = id3([f"heap-string-longer-than-sso-4", f"w"]);
    println(f"{c[0]} {d[0]}");
    match wrap([f"heap-string-longer-than-sso-5"]) { Some(v) => println(v[0]), None => {} }
    let n3 = [f"heap-string-longer-than-sso-6"];
    match wrap(n3) { Some(v) => println(v[0]), None => {} }
    let n4 = [f"heap-string-longer-than-sso-7"];
    let n5 = [f"heap-string-longer-than-sso-8"];
    let g = sel(true, n4, n5);
    println(g[0]);
    let h = sel(false, [f"heap-string-longer-than-sso-9"], [f"heap-string-longer-than-sso-10"]);
    println(h[0]);
    let i = outer([f"heap-string-longer-than-sso-11"]);
    println(i[0]);
    let n6 = [f"heap-string-longer-than-sso-12"];
    let j = outer(n6);
    println(j[0]);
    let n7 = [1, 2, 3];
    let e = ints(n7);
    let f = ints([4, 5, 6]);
    println(f"{e[2] + f[2]}");
    let k = dq(mkq(f"heap-string-longer-than-sso-13"));
    println(k[0]);
    let n8 = mkq(f"heap-string-longer-than-sso-14");
    let l = dq(n8);
    println(l[0]);
    println("end")
}
"#,
        &[
            "heap-string-longer-than-sso-1 heap-string-longer-than-sso-2",
            "heap-string-longer-than-sso-3 heap-string-longer-than-sso-4",
            "heap-string-longer-than-sso-5",
            "heap-string-longer-than-sso-6",
            "heap-string-longer-than-sso-7",
            "heap-string-longer-than-sso-10",
            "heap-string-longer-than-sso-11",
            "heap-string-longer-than-sso-12",
            "9",
            "heap-string-longer-than-sso-13",
            "heap-string-longer-than-sso-14",
            "end",
        ],
        "asan_generic_mono_shared_by_temp_and_named_args_frees_once",
        20,
    );
}

/// B-2026-09-25-11 — indexing a generic call's result in place. A generic
/// free function is never declared, so it had no `fn_return_type_exprs` entry
/// and its return was spelled in its own type params anyway; both the plain
/// index (`id2(x)[1]`) and an indexed-receiver method (`id2(mkv(i))[0].len()`)
/// found no container type and failed `karac build` with "Index operator
/// applied to non-array type" / "requires the indexed container to be a named
/// variable". It now lowers through the nameless-`Vec` index path, which
/// frees the temporary after the read, once, whatever the argument spelling.
#[test]
fn asan_index_generic_call_result_in_place_frees_once() {
    assert_clean_asan_run_min_allocs(
        r#"fn id2[T](d: T) -> T { d }
fn first[T](d: Vec[T]) -> Vec[T] { d }
fn pick[T](a: Option[T], d: T) -> T { match a { Some(s) => s, None => d } }
fn mkd[T](d: T) -> VecDeque[T] { let mut q = VecDeque[T].new(); q.push_back(d); q }
fn two[T](a: T, b: T) -> Array[T, 2] { [a, b] }
fn head[U: Copy](y: Vec[U]) -> U { id2(y)[0] }
fn mkv(i: i64) -> Vec[String] { let mut v = Vec[String].new(); v.push(f"heap-string-longer-than-sso-{i}"); v.push(f"b{i}"); v }
fn main() {
    let x = [1, 2, 3];
    println(id2(x)[1] + id2(x)[2]);
    println(id2(mkv(1))[0]);
    let n = mkv(2);
    println(id2(n)[1]);
    println(first(mkv(3))[0]);
    let a = Some(mkv(4));
    println(pick(a, mkv(5))[0]);
    println(mkd(f"heap-string-longer-than-sso-6")[0]);
    println(two(f"heap-string-longer-than-sso-7", f"x")[0]);
    println(head([8, 9]));
    let mut t = 0;
    for i in 0..10 { t = t + id2(mkv(i))[0].len(); }
    println(t);
    println("end")
}
"#,
        &[
            "5",
            "heap-string-longer-than-sso-1",
            "b2",
            "heap-string-longer-than-sso-3",
            "heap-string-longer-than-sso-4",
            "heap-string-longer-than-sso-6",
            "heap-string-longer-than-sso-7",
            "8",
            "290",
            "end",
        ],
        "asan_index_generic_call_result_in_place_frees_once",
        20,
    );
}

/// B-2026-09-25-9 — a field read on a generic call's struct result in place:
/// `pick(a, P { .. }).n` over `fn pick[T](a: Option[T], d: T) -> T`. A generic
/// free function is never declared, so `type_name_of_expr` found no
/// `fn_return_type_names` entry for the call and `karac build` stopped with
/// "cannot resolve field 'n' on this receiver"; `--interp` printed `1`.
/// The field read also registers the fresh result temp's drop, so every
/// read below has to free the struct it read from exactly once.
#[test]
fn asan_field_read_on_generic_call_result_frees_once() {
    assert_clean_asan_run_min_allocs(
        r#"struct Q { a: i64 }
struct P { s: String, n: i64 }
struct R { q: Q, s: String }
struct W[T] { v: T, k: i64 }
fn pick[T](a: Option[T], d: T) -> T { match a { Some(s) => s, None => d } }
fn id2[T](d: T) -> T { d }
fn get[U](u: U) -> U { id2(u) }
fn inner(p: P) -> i64 { id2(p).n }
fn sel[T](c: bool, a: T, b: T) -> T { if c { a } else { b } }
fn wrap[T](x: T) -> W[T] { W { v: x, k: 3 } }
fn main() {
    for i in 0..2 {
        let a = Some(P { s: f"heap-string-longer-than-sso-1-{i}", n: 1 });
        println(f"{pick(a, P { s: f"d", n: 2 }).n}");
        println(id2(R { q: Q { a: 7 }, s: f"heap-string-longer-than-sso-2-{i}" }).q.a);
        let p = P { s: f"heap-string-longer-than-sso-3-{i}", n: 4 };
        println(inner(p));
        println(get(P { s: f"heap-string-longer-than-sso-4-{i}", n: 5 }).n);
        let x = P { s: f"heap-string-longer-than-sso-5-{i}", n: 6 };
        let y = P { s: f"heap-string-longer-than-sso-6-{i}", n: 8 };
        println(sel(false, x, y).n);
        let z = P { s: f"heap-string-longer-than-sso-7-{i}", n: 9 };
        println(id2(z).s);
        println(wrap(5).k + wrap(9).v);
    }
    println("end")
}
"#,
        &[
            "1",
            "7",
            "4",
            "5",
            "8",
            "heap-string-longer-than-sso-7-0",
            "12",
            "1",
            "7",
            "4",
            "5",
            "8",
            "heap-string-longer-than-sso-7-1",
            "12",
            "end",
        ],
        "asan_field_read_on_generic_call_result_frees_once",
        8,
    );
}
