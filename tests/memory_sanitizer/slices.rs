//! slices, indexing, windows and chunks -- fixtures for `tests/memory_sanitizer.rs`.
//!
//! Split out of `tests/memory_sanitizer.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test memory_sanitizer` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test memory_sanitizer slices::
//!
//! New fixtures about slices, indexing, windows and chunks belong in this file.

use super::*;

/// A struct FIELD of type `Array[T, N]` has its elements dropped
/// (B-2026-08-27-32).
///
/// Unlike its two siblings this needs no temporary, no comparison and no
/// index to leak -- the owner is an ordinary binding whose drop was simply
/// incomplete. `struct Holder { a: Array[String, 2] }` lost 13 bytes in 2
/// allocations on the program below with the last two lines deleted.
///
/// EVERY classifier in `emit_struct_drop_synthesis_impl` missed it, for two
/// independent reasons: the name-based pass reads the field's last path
/// segment and an array `TypeExpr` is `TypeKind::Array`, not a `Path`; and
/// the type-driven nested-aggregate pass destructures a `StructType` while
/// an array lowers to `ArrayType`. So the field was classified no-cleanup
/// twice over.
///
/// THE BY-VALUE-PARAM LEG IS THE ONE THAT MATTERS. Widening a drop obliges
/// the move sites to widen with it, and here that is not a theoretical
/// pairing: with the drop landed alone, `fn take(h: Holder) -> String {
/// return h.a[0]; }` turned this row's 13-byte leak into a
/// HEAP-USE-AFTER-FREE -- the callee's scope-exit drop freed the very
/// element it had just handed back. The paired half is
/// `vec_index_elem_type_expr`'s array arm, which is what makes
/// `maybe_defensive_copy_param_arg` deep-clone the read; the `Vec` twin of
/// that program was already clean through exactly that resolver. The leg
/// stays in this fixture because it fails LOUDLY (exit code, not a leak
/// count) the moment the drop is widened without the copy.
///
/// The move-out leg is the other direction of the same pairing: `let taken
/// = h.a;` moves the field, so `h`'s drop must not free it too.
/// `Array[i64, 2]` pins that a scalar element still classifies as
/// no-cleanup, `Array[Vec[i64], 2]` that the element policy is shared with
/// the `Vec` drain rather than keyed on `String`, and `Vec[Holder]` that
/// the field drop is reached through a container's element drain and not
/// only at a bare scope exit.
/// B-2026-09-14-1 — an index-assign into an `Array` element RELEASES the
/// buffer it displaces.
///
/// The store wrote over the slot and nothing freed what was there, so
/// `a[0] = f"MUTATED-{n}"` over `Array[String, 2]` orphaned the overwritten
/// element: 10 bytes in 1 block at `-O0`, and 30 in 3 blocks for `c2`'s
/// three stores, so the loss is per-STORE rather than per-binding.
///
/// `c6` IS THE ORACLE AND THE REASON THIS IS ARRAY-SPECIFIC. The identical
/// program over `Vec[String]` was always clean (18 allocs / 18 frees) —
/// its leg has released the displaced element since B-2026-06-19-7 — which
/// is the first thing the row asked to measure, because a `Vec` leak of the
/// same shape would have made this the whole indexed-store family instead
/// of one arm.
///
/// `c4` and `c5` are the guard's two decline paths, and they are cells
/// rather than assumptions: `c4` stores over RODATA elements, which the
/// cap guard must skip (`cap == 0`) or it frees a static buffer, and `c5`
/// is a scalar `Array[i64, 3]` whose slot owns no heap at all. `c3` is the
/// compound form, where the RHS reads the same element it overwrites.
///
/// THE ALIASING HAZARD THE `Vec` LEG GUARDS AGAINST IS NOT EXPRESSIBLE
/// HERE, which is why this arm needs no equivalent: `a[0] = a[1]` and
/// `a[0] = a[0]` are both refused by `E_INDEX_MOVE_NON_COPY`, so a store is
/// the only way to displace an element and its RHS can never be another
/// live element of the same array.
///
/// FLOORED because this class hides under DCE, and the floor is what keeps
/// the cell from collapsing to nothing: an orphaned allocation whose only
/// consumer is the slot it is overwritten in is exactly what LLVM deletes.
/// 29 allocations measured at `-O2` against 35 at `-O0`.
///
/// IT BITES AT BOTH OPT LEVELS, which is worth stating because the
/// single-store repro does NOT: that program is clean under valgrind at
/// `-O2` (the allocation is deleted outright) and it was the basis for an
/// earlier draft of this comment claiming the `-O0` leg was the only gate.
/// MEASURED on the unfixed compiler instead: `50 byte(s) leaked in 5
/// allocation(s)` at `-O0` -- 10 bytes for each of the five displacing
/// stores, `c1` plus `c2`'s three plus `c3` -- and `10 byte(s) leaked in 1
/// allocation(s)` at `-O2`, where DCE takes four of the five and one
/// survives. So the `--features llvm` leg catches this too; the `-O0` leg
/// is simply the one that sees all of it.
///
/// TWO CELLS DELIBERATELY ABSENT, both still on the row. A STRUCT element
/// carrying a heap field (`Array[D, N]`, `D { s: String }`) is not a
/// vec-struct slot, so the guard declines and its 10 bytes still leak —
/// along with the displaced element's `Drop` BODY, which never runs either.
/// And `Array[Vec[i64], N]` has a pre-existing `Invalid free()` (measured
/// on this same tree before the fix, 17 allocs / 17 frees with 32 bytes
/// lost); the fix closes its leak half and leaves the invalid free, filed
/// separately. Neither belongs in a fixture that asserts a clean run.
#[test]
fn asan_array_index_store_releases_the_displaced_element() {
    assert_clean_asan_run_min_allocs(
        r#"
fn main() {
    let n = env.args().len() as i64;
    let mut a: Array[String, 2] = [f"aaaaaaaa-{n}", f"bbbbbbbb-{n}"];
    a[0] = f"MUTATED-{n}";
    println(f"c1:{a[0]}");
    let mut b: Array[String, 3] = [f"pppppppp-{n}", f"qqqqqqqq-{n}", f"rrrrrrrr-{n}"];
    b[0] = f"M1-{n}";
    b[1] = f"M2-{n}";
    b[2] = f"M3-{n}";
    println(f"c2:{b[0]}{b[1]}{b[2]}");
    let mut c: Array[String, 2] = [f"cccccccc-{n}", f"dddddddd-{n}"];
    c[0] = c[0] + f"X{n}";
    println(f"c3:{c[0]}");
    let mut d: Array[String, 2] = ["lit-a", "lit-b"];
    d[0] = "lit-c";
    println(f"c4:{d[0]}");
    let mut e: Array[i64, 3] = [n, n + 1, n + 2];
    e[0] = n + 9;
    println(f"c5:{e[0]}");
    let mut v: Vec[String] = Vec.new();
    v.push(f"vvvvvvvv-{n}");
    v[0] = f"VM-{n}";
    println(f"c6:{v[0]}");
    println("end");
}
"#,
        &[
            "c1:MUTATED-1",
            "c2:M1-1M2-1M3-1",
            "c3:cccccccc-1X1",
            "c4:lit-c",
            "c5:10",
            "c6:VM-1",
            "end",
        ],
        "asan_array_index_store_releases_the_displaced_element",
        20,
    );
}

/// B-2026-09-14-30 — the MOVED-IN SOURCE of an `Array` index-store gives up
/// its own cleanup, which is the half B-2026-09-14-1 left open.
///
/// That row freed the element the store DISPLACES; this one is the other
/// side of the same transfer. `a[0] = v3` for a named local `v3` moved its
/// `{ptr,len,cap}` into the element slot while `v3`'s scope-exit cleanup
/// stayed armed, so the array's element drop and `v3` both freed the same
/// 32-byte buffer: `free(): double free detected in tcache 2`, exit 134 on
/// the JIT, on a default `karac build`, and at `-O0`. The interpreter was
/// correct throughout, so this was a run-vs-build divergence as well as a
/// memory-safety error.
///
/// `vectwin` IS THE ORACLE, and measuring it FIRST is what said this was one
/// arm rather than the whole indexed-store family: the identical program
/// over `Vec[Vec[i64]]` is clean at 18 allocs / 18 frees. The `Vec` leg has
/// suppressed the moved-in source since B-2026-06-19-7; the `Array` leg
/// never did, because the gate asks `vec_elem_types` / `var_elem_type_exprs`
/// and an `Array` local records its element in `array_elem_type_exprs`.
///
/// `structelem` COVERS THE SECOND ARM, the one that disarms a moved-in
/// struct's heap-field caps. Its displaced elements are deliberately
/// RODATA (`"b30-static-one"`, cap 0): a heap-bearing displaced element
/// still leaks under B-2026-09-14-29, which is a different row and does not
/// belong in a fixture asserting a clean run. The MOVED-IN `b` does own a
/// live buffer, which is what arms the double free this asserts against.
///
/// `refparam` is here because a `mut ref Array[..]` param indexes the
/// CALLER's array, which still owns its elements — it aborted identically
/// before the fix, and the predicate reads `borrow_vars.ref_params` so it
/// resolves.
///
/// CONTROLS, clean before AND after: `freshrhs` has no named source to
/// suppress; `scalar` is an `Array[i64, 2]` whose element owns no heap, so
/// it pins the new predicate as a no-op where the container will free
/// nothing; `vectwin` is the oracle above.
///
/// FLOORED for the reason its sibling is: the orphaned-then-double-freed
/// buffers are exactly what LLVM deletes when nothing observes them, and a
/// fixture that allocates nothing at `-O2` asserts nothing.
#[test]
fn asan_array_index_store_disarms_the_moved_in_source() {
    assert_clean_asan_run_min_allocs(
        r#"
struct D { id: i64, s: String }

fn put(a: mut ref Array[Vec[i64], 2], n: i64) {
    let mut vr: Vec[i64] = Vec.new();
    vr.push(n + 9);
    a[0] = vr;
}

fn main() {
    let mut i: i64 = 0;
    while i < 2 {
        let mut v1: Vec[i64] = Vec.new();  v1.push(i);
        let mut v2: Vec[i64] = Vec.new();  v2.push(i + 1);
        let mut a: Array[Vec[i64], 2] = [v1, v2];
        let mut v3: Vec[i64] = Vec.new();  v3.push(i + 9);
        a[0] = v3;
        println(f"vecelem:{a[0].len()}");

        let mut sa: Array[String, 2] = [f"b30-one-aaaaaaaaaaaaaaaa-{i}", f"b30-two-bbbbbbbbbbbbbbbb-{i}"];
        let s3 = f"b30-three-cccccccccccccccc-{i}";
        sa[0] = s3;
        println(f"strelem:{sa[0].len()}");

        let mut da: Array[D, 2] = [D { id: 1, s: "b30-static-one" }, D { id: 2, s: "b30-static-two" }];
        let b = D { id: 3, s: f"b30-heap-{i}" };
        da[0] = b;
        println(f"structelem:{da[0].id}");

        let mut r1: Vec[i64] = Vec.new();  r1.push(i);
        let mut r2: Vec[i64] = Vec.new();  r2.push(i + 1);
        let mut ra: Array[Vec[i64], 2] = [r1, r2];
        put(mut ra, i);
        println(f"refparam:{ra[0].len()}");

        let mut l1: Vec[i64] = Vec.new();  l1.push(i);
        let mut l2: Vec[i64] = Vec.new();  l2.push(i + 1);
        let mut la: Array[Vec[i64], 2] = [l1, l2];
        let mut j: i64 = 0;
        while j < 3 {
            let mut vn: Vec[i64] = Vec.new();
            vn.push(i + j);
            la[0] = vn;
            j = j + 1;
        }
        println(f"loop:{la[0].len()}");

        let mut f1: Vec[i64] = Vec.new();  f1.push(i);
        let mut f2: Vec[i64] = Vec.new();  f2.push(i + 1);
        let mut fa: Array[Vec[i64], 2] = [f1, f2];
        fa[0] = Vec.new();
        println(f"freshrhs:{fa[0].len()}");

        let mut na: Array[i64, 2] = [i, i + 1];
        let x = i + 9;
        na[0] = x;
        println(f"scalar:{na[0]}");

        let mut w1: Vec[i64] = Vec.new();  w1.push(i);
        let mut w: Vec[Vec[i64]] = Vec.new();
        w.push(w1);
        let mut w3: Vec[i64] = Vec.new();  w3.push(i + 9);
        w[0] = w3;
        println(f"vectwin:{w[0].len()}");

        i = i + 1;
    }
}
"#,
        &[
            "vecelem:1",
            "strelem:28",
            "structelem:3",
            "refparam:1",
            "loop:1",
            "freshrhs:0",
            "scalar:9",
            "vectwin:1",
            "vecelem:1",
            "strelem:28",
            "structelem:3",
            "refparam:1",
            "loop:1",
            "freshrhs:0",
            "scalar:10",
            "vectwin:1",
        ],
        "asan_array_index_store_disarms_the_moved_in_source",
        20,
    );
}

/// `Slice[T] == Slice[T]` and `Array[T, N] == Array[T, N]` borrow both
/// operands and free neither (B-2026-08-27-24, -25).
///
/// Both new comparators take POINTERS, so the intercept spills a by-value
/// operand into an alloca before the call — the same shape as the `Vec`
/// sibling, and the same risk: spilling a heap-owning value is how a second
/// owner appears, and a heap ELEMENT is where a stray free shows up as a
/// double-free rather than as a wrong answer.
///
/// A slice is the sharper of the two, because it OWNS NOTHING: the buffer
/// belongs to the `Vec` or `Array` it views, so any free at all on this
/// path is a double-free of someone else's memory, not a leak. The
/// `Array[String, 2]` legs cover the owning direction, including the `Map`
/// key, which moves an array of heap Strings into the table on a DUPLICATE
/// insert — the displaced key must be destroyed exactly once.
///
/// Deliberately absent: a FRESH-TEMPORARY array operand (`mk(2) == mk(2)`).
/// Measured leaking 26 bytes in 4 allocations — the four element Strings,
/// the same signature B-2026-08-27-26 records for the `Vec[String]` shape
/// of the identical defect, which is that a temporary operand has no owner
/// to run its element drops. It is filed as B-2026-08-27-30 rather than
/// pinned green here; a bound operand of the same type is clean, which is
/// what these legs assert.
#[test]
fn asan_slice_and_array_equality_are_ownership_neutral() {
    assert_clean_asan_run(
        r#"
fn mk(n: i64) -> Array[String, 2] {
    return Array[f"item{n}", f"item{n + 1}"];
}

fn main() {
    let mut a: Vec[String] = Vec.new();
    a.push(f"hello");
    a.push(f"world");
    let mut b: Vec[String] = Vec.new();
    b.push(f"hello");
    b.push(f"world");
    let sa: Slice[String] = a[0..2];
    let sb: Slice[String] = b[0..2];
    println(f"{sa == sb}");
    println(f"{a[0..1] == b[0..1]}");
    println(a[1]);

    let p = mk(1);
    let q = mk(1);
    println(f"{p == q}");
    println(p[0]);

    let mut m: Map[Array[String, 2], i64] = Map.new();
    let k1 = mk(3);
    let k2 = mk(3);
    m.insert(k1, 1);
    m.insert(k2, 2);
    println(f"{m.len()}");
}
"#,
        &["true", "true", "world", "true", "item1", "1"],
        "asan_slice_and_array_equality_are_ownership_neutral",
    );
}

#[test]
fn asan_direct_index_match_result_shared_no_leak() {
    // B-2026-07-12-24 — the index-read (`match v[i]`) `Result[shared]` case
    // (sibling of the Option B-21 index fix). The index deep-clone rc-INCs
    // the node; the synthetic let-bound scrutinee the rewrite emits now
    // releases it via `track_rc_result_var`. Looped 200x; prints 2000.
    assert_clean_asan_run(
        r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn xfer() -> i64 {
    let mut dst: Vec[Result[Node, i64]] = Vec.new();
    dst.push(Ok(Node { val: 10, left: None, right: None }));
    let mut r: i64 = 0;
    match dst[0] {
        Err(_) => {}
        Ok(nd) => { r = nd.val; }
    }
    r
}
fn main() {
    let mut i: i64 = 0;
    let mut t: i64 = 0;
    while i < 200 {
        t = t + xfer();
        i = i + 1;
    }
    println(f"{t}");
}
"#,
        &["2000"],
        "direct_index_match_result_shared_no_leak",
    );
}

#[test]
fn asan_index_store_into_tuple_element_vec_no_leak() {
    // B-2026-07-20-3 companion: storing into a HEAP element of a
    // tuple-element `Vec` (`t.1[i] = "..."`) must free the OVERWRITTEN
    // element's buffer exactly once (no leak) and not double-free the tuple
    // on scope exit. Rebuild the tuple every iteration and overwrite a
    // `String` slot, amplifying any leak/double-free.
    assert_clean_asan_run(
        r#"
fn make() -> (Vec[i64], Vec[String]) {
    let mut a: Vec[i64] = Vec.new();
    a.push(1);
    let mut b: Vec[String] = Vec.new();
    b.push("first");
    b.push("second");
    (a, b)
}
fn main() {
    let mut acc: i64 = 0;
    for _ in 0..50 {
        let mut t = make();
        t.1[0] = "overwritten";
        acc = acc + t.1[0].len() + t.1[1].len();
    }
    println(acc);
}
"#,
        // per iter: len("overwritten")=11 + len("second")=6 = 17; × 50 = 850
        &["850"],
        "asan_index_store_into_tuple_element_vec_no_leak",
    );
}

#[test]
fn asan_heap_vec_index_read_into_sinks_no_double_free() {
    // General heap-index-read-into-owning-sink double-free (found fixing the
    // heap-zip leg). Reading `v[i]` (heap String element) into a tuple
    // literal, a `push`, and a struct field must deep-clone so the source
    // `v` stays the sole owner of its originals and each sink owns an
    // independent buffer. Before the fix each sink aliased the source buffer
    // → double-free (exit 133). 40× ≥40-byte payloads; `v` re-read each
    // round to expose UAF.
    assert_clean_asan_run(
            r#"
struct Pair { x: String, y: String }
fn main() {
    let mut round: i64 = 0i64;
    while round < 40i64 {
        let mut v: Vec[String] = Vec[
            "index-sink-element-alpha-aaaaaaaaaaaaaaaaaaaa".to_string(),
            "index-sink-element-bravo-bbbbbbbbbbbbbbbbbbbb".to_string(),
            "index-sink-element-charlie-cccccccccccccccccc".to_string()
        ];
        let t: (String, String) = (v[0i64], v[2i64]);
        let mut d: Vec[String] = Vec.new();
        d.push(v[1i64]);
        let p: Pair = Pair { x: v[0i64], y: v[1i64] };
        println(f"{t.0} {t.1} {d[0i64]} {p.x} {p.y} {v.len()}");
        round = round + 1i64;
    }
}
"#,
            [
                "index-sink-element-alpha-aaaaaaaaaaaaaaaaaaaa index-sink-element-charlie-cccccccccccccccccc index-sink-element-bravo-bbbbbbbbbbbbbbbbbbbb index-sink-element-alpha-aaaaaaaaaaaaaaaaaaaa index-sink-element-bravo-bbbbbbbbbbbbbbbbbbbb 3",
            ]
            .repeat(40)
            .as_slice(),
            "asan_heap_vec_index_read_into_sinks_no_double_free",
        );
}

#[test]
fn asan_nested_vec_index_bind_no_double_free() {
    // The `matrix[i][j]` clone gap: binding a nested heap element out of a
    // `Vec[Vec[String]]` (`let x = m[i][j]`) shallow-aliased the innermost
    // buffer, so both the binding's drop and the container's recursive drop
    // freed it (double-free, exit 133). `clone_owned_vec_index_element` now
    // peels one Vec layer per index level (`vec_index_elem_type_expr`) and
    // deep-clones, so the binding owns an independent buffer and the source
    // survives. Surfaced binding a `chunks()` result element. 40x >=40-byte
    // payloads; the source matrix is re-read each round.
    assert_clean_asan_run(
            r#"
fn main() {
    let mut round: i64 = 0i64;
    while round < 40i64 {
        let mut v: Vec[String] = Vec[
            "nested-idx-bind-alpha-aaaaaaaaaaaaaaaaaaaa".to_string(),
            "nested-idx-bind-bravo-bbbbbbbbbbbbbbbbbbbb".to_string(),
            "nested-idx-bind-charlie-cccccccccccccccccc".to_string(),
            "nested-idx-bind-delta-dddddddddddddddddddd".to_string()
        ];
        let m: Vec[Vec[String]] = v.iter().chunks(2i64).collect();
        let r0: Vec[String] = m[0i64].clone();
        let r1: Vec[String] = m[1i64].clone();
        let a: String = r0[0i64].clone();
        let b: String = r1[1i64].clone();
        println(f"{a} {b} {m.len()} {v.len()}");
        round = round + 1i64;
    }
}
"#,
            [
                "nested-idx-bind-alpha-aaaaaaaaaaaaaaaaaaaa nested-idx-bind-delta-dddddddddddddddddddd 2 4",
            ]
            .repeat(40)
            .as_slice(),
            "asan_nested_vec_index_bind_no_double_free",
        );
}

/// Borrow-elision (B-2026-06-19-6): a read-only `let r = out[j]` over a
/// `Vec[Vec[i64]]` binds `r` as a borrow of the element and SKIPS both the
/// deep clone and the binding's scope-exit free. ASAN must confirm this is
/// clean — no leak (the container still owns and frees each buffer), no
/// double-free (the borrow doesn't free), no use-after-free. Inner vectors
/// carry 8 i64s (64 bytes) so a wrongly-skipped owned-buffer free would be a
/// reachable-at-exit leak LSan can see (≥36-byte payload rule).
#[test]
fn asan_borrow_elision_read_only_vecvec_index_is_clean() {
    assert_clean_asan_run(
        r#"
fn main() {
    let mut out: Vec[Vec[i64]] = Vec.new();
    let mut k = 0i64;
    while k < 32i64 {
        let mut b: Vec[i64] = Vec.new();
        let mut p = 0i64;
        while p < 8i64 { b.push(k * 8i64 + p); p = p + 1i64; }
        out.push(b);
        k = k + 1i64;
    }
    let mut acc = 0i64;
    let m = out.len();
    let mut j = 0i64;
    while j < m {
        let r = out[j].clone();
        let mut i = 0i64;
        let rl = r.len();
        while i < rl { acc = acc + r[i]; i = i + 1i64; }
        j = j + 1i64;
    }
    println(acc);
}
"#,
        &["32640"],
        "borrow_elision_read_only_vecvec_index",
    );
}

/// Index-store of a heap-owning Vec element (B-2026-06-19-7): `out[j] = nb`
/// over a `Vec[Vec[i64]]` in a loop. The store must (a) drop the old element
/// buffer (no leak) and (b) suppress the moved source binding's cleanup (no
/// double-free); pre-fix the AOT binary SIGTRAPped. ASAN confirms a clean run
/// (no use-after-free / double-free; Linux CI LSan covers the leak arm).
/// Inner vectors carry 8 i64s (64 bytes) for LSan reachability. Sum of heads
/// j=0..31 = 496.
#[test]
fn asan_index_store_heap_vec_element_no_double_free() {
    assert_clean_asan_run(
        r#"
fn main() {
    let mut out: Vec[Vec[i64]] = Vec.new();
    let mut k = 0i64;
    while k < 32i64 {
        let mut b: Vec[i64] = Vec.new();
        let mut p = 0i64;
        while p < 8i64 { b.push(7i64); p = p + 1i64; }
        out.push(b);
        k = k + 1i64;
    }
    let mut acc = 0i64;
    let mut j = 0i64;
    while j < 32i64 {
        let mut nb: Vec[i64] = Vec.new();
        let mut q = 0i64;
        while q < 8i64 { nb.push(j); q = q + 1i64; }
        out[j] = nb;
        acc = acc + out[j][0i64];
        j = j + 1i64;
    }
    println(acc);
}
"#,
        &["496"],
        "index_store_heap_vec_element",
    );
}

/// B-2026-08-06-4 — the MEMORY leg of the shared-struct Vec-field slice
/// coercion.
///
/// The value witness is the codegen twin; this guards the OWNERSHIP
/// question the new path raises, which no earlier test can cover because
/// no program of this shape could be built at all before the fix.
///
/// The slice header the fix builds points INTO the Vec buffer that lives
/// inside the shared node's RC box, and the callee writes through it. So
/// the header must be a pure BORROW: it must not make the callee an owner
/// (freeing the box's buffer at the callee's scope exit, leaving the reads
/// after the call dangling and the node teardown double-freeing), and it
/// must not disarm the node's own reclaim (leaking one buffer per
/// iteration). Both failures are new to this path, and the RC header is
/// why a plain-struct slice coercion cannot stand in: there the buffer is
/// reclaimed by a scope-exit struct drop, here by node teardown at
/// refcount zero.
///
/// Carries a `Vec[String]` field alongside so a mis-resolved field offset
/// shows up as a corrupted heap pointer under ASAN rather than as a merely
/// wrong integer, and exercises both spellings — a `mut Slice[T]` write
/// and a read-only `Slice[T]` — since both newly route through this arm.
///
/// Floored per B-2026-08-04-17: the payload is runtime-derived and read
/// through `.len()` on a String the loop keeps live, so the entries are
/// not dead allocations at -O2.
#[test]
fn asan_shared_struct_vec_field_slice_coercion_no_leak() {
    assert_clean_asan_run_min_allocs(
        r#"
shared struct H { tag: i64, mut v: Vec[String], mut w: Vec[i64] }

fn zap(s: mut Slice[i64]) { s[0] = 99; }

fn total(s: Slice[i64]) -> i64 {
    let mut t = 0;
    let mut i = 0;
    while i < s.len() { t = t + s[i]; i = i + 1; }
    t
}

fn main() {
    let n = env.args().len() as i64;
    let mut acc: i64 = 0;
    let mut i: i64 = 0;
    while i < 40 {
        let mut sv: Vec[String] = Vec.new();
        let mut s: String = String.new();
        s.push_str("payload-");
        s.push_str(n.to_string());
        s.push_str("-padded-out-to-force-heap");
        sv.push(s);
        let mut wv: Vec[i64] = Vec.new();
        wv.push(i);
        wv.push(i + 1);
        wv.push(i + 2);
        let h = H { tag: i, v: sv, w: wv };
        zap(mut h.w);
        acc = acc + total(h.w) + h.v[0].len();
        i = i + 1;
    }
    println(acc);
}
"#,
        // Per iteration `w` is [99, i+1, i+2] (summing to 102 + 2i) plus a
        // 34-char payload. Over i = 0..39: 40*(102 + 34) + 2*780 = 7000.
        &["7000"],
        "shared_struct_vec_field_slice_coercion",
        100,
    );
}

// ── extend_from_slice ─────────────────────────────────────────
// Memcpy + grow path; both source and destination get a
// scope-exit free, neither is freed twice, no leak in the
// grown-buffer hand-off.

#[test]
fn asan_vec_extend_from_slice_no_grow_clean() {
    assert_clean_asan_run(
        r#"
fn main() {
    let src: Vec[i64] = Vec.filled(4, 7);
    let mut dst: Vec[i64] = Vec.with_capacity(8);
    dst.push(1);
    dst.push(2);
    dst.extend_from_slice(src);
    println(dst.len());
}
"#,
        &["6"],
        "vec_extend_from_slice_no_grow_clean",
    );
}

#[test]
fn asan_vec_extend_from_slice_nested_vec_elements_independent() {
    // Vec[Vec[i64]] source — the inner Vec storage must be
    // deep-cloned into dest. Without the fix, dst[0]'s inner Vec
    // aliases src[0]'s buffer; both scope-exit frees the same
    // pointer.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut src: Vec[Vec[i64]] = Vec.new();
    let mut a: Vec[i64] = Vec.new();
    a.push(1);
    a.push(2);
    src.push(a);
    let mut b: Vec[i64] = Vec.new();
    b.push(3);
    src.push(b);
    let mut dst: Vec[Vec[i64]] = Vec.new();
    dst.extend_from_slice(src);
    println(dst[0].len());
    println(dst[1].len());
}
"#,
        &["2", "1"],
        "vec_extend_from_slice_nested_vec_elements_independent",
    );
}

#[test]
fn asan_vec_from_slice_nested_index_source_clean() {
    // `Vec.from_slice(rows[r])` on Vec[Vec[T]] — symmetric to the
    // extend_from_slice nested-index test. The new codegen branch
    // compiles `rows[r]` directly, extracts {data, len}, and
    // routes through the standard alloc + memcpy/clone path.
    // Catches RC-aliasing bugs that would surface if the per-
    // element clone path missed the new entry shape.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut rows: Vec[Vec[i64]] = Vec.new();
    let mut r0: Vec[i64] = Vec.new();
    r0.push(11);
    r0.push(22);
    rows.push(r0);
    let copy: Vec[i64] = Vec.from_slice(rows[0]);
    println(copy.len());
    println(copy[0]);
    println(copy[1]);
}
"#,
        &["2", "11", "22"],
        "vec_from_slice_nested_index_source_clean",
    );
}

#[test]
fn asan_vec_extend_from_slice_nested_index_source_clean() {
    // Source is `rows[r]` on Vec[Vec[T]] — the kata-6 case.
    // The codegen fallback path compiles the Index expression
    // and reads its {ptr, len}. Memcpy aliases the source
    // pointer into the destination's buffer for the duration
    // of the memcpy, but the destination has independent
    // storage afterwards. Scope-exit cleanup of `rows`
    // recursively frees each inner Vec's buffer; `out`'s own
    // buffer is freed independently. Catches double-free if
    // the codegen accidentally aliases the source's buffer
    // into the destination's data pointer instead of memcpy.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut rows: Vec[Vec[i64]] = Vec.new();
    let mut r0: Vec[i64] = Vec.new();
    r0.push(10);
    r0.push(20);
    rows.push(r0);
    let mut r1: Vec[i64] = Vec.new();
    r1.push(30);
    rows.push(r1);
    let mut out: Vec[i64] = Vec.with_capacity(8);
    let mut i = 0i64;
    while i < 2 {
        out.extend_from_slice(rows[i]);
        i = i + 1;
    }
    println(out.len());
}
"#,
        &["3"],
        "vec_extend_from_slice_nested_index_source_clean",
    );
}

// ── extend_from_slice: source-alias rejection (grow path) ────
// When the source slice points into the receiver's own heap
// buffer (e.g. `v.extend_from_slice(v.as_slice())`) and grow
// fires, the grow path frees the old buffer before reading
// from `src_data` — a use-after-free that previously silently
// corrupted the extended elements (the read returned whatever
// the allocator handed back from the recycled slot, often the
// freshly-malloc'd new buffer's tail). The runtime overlap
// guard in `extend_from_slice` detects the case before the
// free and `emit_panic`s instead. Test verifies (a) the
// guard fires with the expected message, and (b) the
// disjoint-source counterpart still runs cleanly.

#[test]
fn asan_vec_extend_from_slice_self_alias_rejects() {
    assert_asan_panics_with(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.with_capacity(2);
    v.push(1);
    v.push(2);
    v.extend_from_slice(v.as_slice());
    println(v.len());
}
"#,
        "source slice aliases destination buffer",
        "vec_extend_from_slice_self_alias_rejects",
    );
}

/// B-2026-08-05-40 — the MEMORY half of the place → `Slice[T]` coercion.
///
/// The header the fix synthesizes points INTO the caller's Vec buffer: no
/// copy, no ownership transfer, and the callee only borrows. That is the
/// claim worth a leak/UAF gate, because the same path could equally have
/// been written to materialize a temp — which would compile, read
/// correctly, silently drop every write through a `mut Slice`, and either
/// leak the temp or free a buffer the place still owns.
///
/// NOT VACUOUS (B-2026-08-04-17): opaque `env.args().len()` seed, every
/// Vec and tag built from it at runtime, byte-level `contains` read, and
/// 200 iterations so nothing unrolls away.
#[test]
fn asan_slice_param_from_a_place_argument_no_leak() {
    assert_clean_asan_run_min_allocs(
        r#"struct P { a: Vec[i64], tag: String }
struct Inner { a: Vec[i64] }
struct Outer { q: Inner }

fn mkv(k: i64) -> Vec[i64] {
    let mut v: Vec[i64] = Vec.new();
    v.push(k);
    v.push(k + 1i64);
    v.push(k + 2i64);
    return v;
}

fn total(s: Slice[i64]) -> i64 {
    let mut t: i64 = 0;
    let mut i: i64 = 0;
    while i < s.len() { t = t + s[i]; i = i + 1i64; }
    return t;
}

fn rtotal(s: ref Slice[i64]) -> i64 {
    let mut t: i64 = 0;
    let mut i: i64 = 0;
    while i < s.len() { t = t + s[i]; i = i + 1i64; }
    return t;
}

fn bumpall(s: mut Slice[i64]) {
    let mut i: i64 = 0;
    while i < s.len() { s[i] = s[i] + 1i64; i = i + 1i64; }
}

fn main() {
    let n: i64 = env.args().len();
    let mut acc: i64 = 0;
    let mut i: i64 = 0;
    while i < n + 199i64 {
        let mut g: P = P { a: mkv(i), tag: f"tag-{i}-payload" };
        bumpall(mut g.a);
        acc = acc + total(g.a);
        if g.tag.contains("payload") { acc = acc + 1i64; }
        let mut g2: P = P { a: mkv(i), tag: f"tag2-{i}-payload" };
        bumpall(mut g2.a);
        acc = acc + rtotal(g2.a);
        let mut t: (Vec[i64], i64) = (mkv(i), 0i64);
        bumpall(mut t.0);
        acc = acc + total(t.0);
        let mut vv: Vec[Vec[i64]] = Vec.new();
        vv.push(mkv(i));
        bumpall(mut vv[0i64]);
        acc = acc + total(vv[0i64]);
        let mut o: Outer = Outer { q: Inner { a: mkv(i) } };
        bumpall(mut o.q.a);
        acc = acc + total(o.q.a);
        i = i + 1;
    }
    println(acc);
}"#,
        &["304700"],
        "slice_param_from_a_place_argument_no_leak",
        // 200 iterations x (5 Vecs + 2 tag Strings) is well over a
        // thousand allocations; the floor sits far above the 3 of a
        // folded-away run.
        400,
    );
}

#[test]
fn asan_deep_tuple_index_match_no_double_free() {
    // #25 (phase-12 self-hosting, B-2026-06-14-4) — the read-path fix that
    // lets `match h.ps.0.tok { Id(s) => … }` (a `<struct>.tuplefield.0.<enum
    // field>` scrutinee) compile must not introduce a double-free: the arm
    // CONSUMES the enum payload (`s`) that the owning `h`'s tuple drop also
    // frees. The #21 tuple-index match suppression
    // (`suppress_destructured_struct_field_enum_cleanup` via the
    // `place_chain_type_name` TupleIndex hop) cap-zeros the source, so `s`
    // is the sole owner. Loop-stressed over consume (`s.len()`), borrow
    // (`println(s)`), and the scalar second element (`h.ps.1`, regression).
    // f-string payloads keep the heap non-foldable so Linux LSan sees a real
    // leak if the suppression mis-fires. The heap `let`-binding form
    // (`let inr = h.ps.0`) is a SEPARATE pre-existing move-out double-free
    // (tracker #27) and is deliberately not exercised here.
    assert_clean_asan_run(
        r#"
enum Tok { Id(String), Num(i64) }
struct Inner { tok: Tok, n: i64 }
struct Hs { ps: (Inner, i64) }
fn main() {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 8 {
        let h = Hs { ps: (Inner { tok: Tok.Id(f"id-{i}"), n: i }, i) };
        // Consume arm — `s` owns the payload; h's tuple drop must skip it.
        match h.ps.0.tok {
            Id(s) => { acc = acc + s.len(); }
            Num(n) => { acc = acc + n; }
        }
        // Borrow arm over a fresh value.
        let h2 = Hs { ps: (Inner { tok: Tok.Id(f"v-{i}"), n: i }, i) };
        match h2.ps.0.tok {
            Id(s) => { acc = acc + s.len(); }
            Num(n) => { acc = acc + n; }
        }
        // Scalar second element (regression guard).
        acc = acc + h.ps.1;
        i = i + 1;
    }
    if acc > 999999 { println("never"); }
    println("done");
}
"#,
        &["done"],
        "deep_tuple_index_match_no_double_free",
    );
}

/// B-2026-08-01-22 leg a — `h.xs[i] = Res { .. }` (field-rooted
/// container) leaked the displaced element's field buffers like the
/// direct-binding -21 shape; the displacement emitter now resolves the
/// field's storage via the same synth-identifier dance the store arm
/// uses. LSan gates the leak; ASAN guards the pre-store release.
#[test]
fn asan_field_rooted_index_assign_displaced_elem_freed() {
    assert_clean_asan_run(
        r#"
struct Res { id: i64, name: String }
impl Drop for Res {
    fn drop(mut ref self) { println(f"drop {self.id} {self.name}") }
}
struct Holder { xs: Vec[Res] }
fn main() {
    println("a");
    let mut h = Holder { xs: Vec.new() };
    h.xs.push(Res { id: 9, name: f"z{9}" });
    h.xs[0] = Res { id: 5, name: f"y{5}" };
    println(f"held {h.xs[0].id}");
    println("end");
}
"#,
        &["a", "drop 9 z9", "held 5", "drop 5 y5", "end"],
        "field_rooted_index_assign_displaced_elem_freed",
    );
}

/// B-2026-08-01-21 — `v[i] = Res { .. }` over a struct element with
/// heap fields leaked the displaced element's field buffers (the
/// element-reassign machinery freed direct {ptr,len,cap} elements but
/// never walked struct fields). The displacement path now runs the
/// cap-guarded field-heap synthesizer (plus the bodies walk) on the
/// old element before the store. LSan (Linux CI) gates the leak; ASAN
/// guards the new pre-store release against double-freeing the
/// element the Vec's own scope-exit drop covers.
#[test]
fn asan_index_assign_displaced_elem_fields_freed() {
    assert_clean_asan_run(
        r#"
struct Res { id: i64, name: String }
impl Drop for Res {
    fn drop(mut ref self) { println(f"drop {self.id} {self.name}") }
}
struct Plain { id: i64, name: String }
fn main() {
    println("a");
    let mut v: Vec[Res] = Vec.new();
    v.push(Res { id: 9, name: f"z{9}" });
    v[0] = Res { id: 5, name: f"y{5}" };
    println(f"held {v[0].id}");
    let mut p: Vec[Plain] = Vec.new();
    p.push(Plain { id: 3, name: f"w{3}" });
    p[0] = Plain { id: 4, name: f"v{4}" };
    println(f"kept {p[0].id}");
    println("end");
}
"#,
        &["a", "drop 9 z9", "held 5", "drop 5 y5", "kept 4", "end"],
        "index_assign_displaced_elem_fields_freed",
    );
}

/// B-2026-08-12-22 — the OTHER side of the same store. The fixture above
/// covers the DISPLACED element (the slot's old occupant); this covers the
/// INCOMING one. `ps[0] = b` for a named `b` whose type is a struct with a
/// live heap field moved `b`'s field pointers into the slot while `b`'s own
/// `StructDrop` stayed armed, so the container's element drain and `b`
/// freed the same buffer: `free(): double free detected in tcache 2` on a
/// default `karac build`, a `ptr::copy_nonoverlapping` UB panic under the
/// JIT, and correct output from the interpreter.
///
/// `target_owns_heap_vec_elem` did not reach it because that gate asks
/// whether the ELEMENT is a `{ptr,len,cap}` — which is why a `Vec[String]`
/// was safe and a `Vec[<struct with a String>]` was not.
///
/// EVERY LEG USES AN f-STRING, never a string literal, and that is the
/// point rather than a style choice. The filing's boundary table recorded a
/// fresh struct literal and a field-wise rebuild as SAFE; both abort once
/// the field is genuinely allocated. Those probes used string literals,
/// whose `cap` is 0, so the second free was a guarded no-op and the shape
/// only looked clean. A literal-valued fixture here would pass without the
/// fix and prove nothing.
///
/// The legs are the distinct sources a named binding can have, plus the two
/// container spellings and the motivating swap:
///
///   * `elem`  — the binding is an element READ (the filed repro)
///   * `lit`   — the binding is a fresh struct literal
///   * `call`  — the binding is a function result
///   * `field` — the container is field-rooted (`h.xs[j] = b`)
///   * `veck`  — the heap field is a `Vec`, not a `String`
///   * `swap`  — the swap at the heart of any hand-written sort, which is
///     what makes this worth a high severity despite the narrow shape.
///     Spelled with TWO temps (`let t = qs[0]; let u = qs[1]; qs[0] = u;
///     qs[1] = t;`) rather than the terser one-temp form, because the
///     terser form contains `qs[0] = qs[1]` and would drag in the
///     unrelated pre-existing leak described below. Both spellings
///     DOUBLE-FREED before this fix; this one exercises the suppression
///     twice and is leak-clean, so it can gate.
///
/// THE OBVIOUS CONTROL IS DEFERRED, NOT OMITTED. `ps[0] = ps[1]` (no
/// binding) would be the natural must-not-disturb leg, and it cannot go
/// here because it is RED for an unrelated, pre-existing reason: it leaks
/// one buffer per assignment, measured identically with and without this
/// fix (400 B in 40 allocations either way). Its RHS is an `Index`, not an
/// `Identifier`, so the suppression added here never runs for it. It has
/// its own deferred fixture immediately below and its own row.
#[test]
fn asan_index_assign_named_struct_source_freed_once() {
    assert_clean_asan_run(
        r#"
#[derive(Clone)]
struct Pair { word: String, n: i64 }
#[derive(Clone)]
struct Bag { xs: Vec[i64], n: i64 }
struct Holder { ps: Vec[Pair] }
fn mk(k: i64) -> Pair { return Pair { word: f"call{k}", n: k }; }
fn main() {
    let k = env.args().len() as i64;
    let mut i = 0i64;
    let mut acc = 0i64;
    while i < 40 {
        let mut ps: Vec[Pair] = Vec.new();
        ps.push(Pair { word: f"alpha{k + i}", n: 1 });
        ps.push(Pair { word: f"beta{k + i}", n: 2 });
        // elem: the filed repro.
        let b = ps[1].clone();
        ps[0] = b;
        acc = acc + ps[0].word.len() + ps[1].word.len();
        // lit: a fresh literal bound to a name first.
        let lit = Pair { word: f"gamma{k + i}", n: 3 };
        ps[0] = lit;
        acc = acc + ps[0].word.len();
        // call: a function result bound to a name.
        let c = mk(k + i);
        ps[1] = c;
        acc = acc + ps[1].word.len();
        // swap: the sort primitive.
        let mut qs: Vec[Pair] = Vec.new();
        qs.push(Pair { word: f"one{k + i}", n: 1 });
        qs.push(Pair { word: f"two{k + i}", n: 2 });
        let t = qs[0].clone();
        let u = qs[1].clone();
        qs[0] = u;
        qs[1] = t;
        acc = acc + qs[0].word.len() + qs[1].word.len();
        // field: a field-rooted container.
        let mut h = Holder { ps: Vec.new() };
        h.ps.push(Pair { word: f"hx{k + i}", n: 1 });
        h.ps.push(Pair { word: f"hy{k + i}", n: 2 });
        let hb = h.ps[1].clone();
        h.ps[0] = hb;
        acc = acc + h.ps[0].word.len();
        // veck: the heap field is a Vec rather than a String.
        let mut v1: Vec[i64] = Vec.new();
        v1.push(7);
        let mut v2: Vec[i64] = Vec.new();
        v2.push(9);
        let mut bs: Vec[Bag] = Vec.new();
        bs.push(Bag { xs: v1, n: 1 });
        bs.push(Bag { xs: v2, n: 2 });
        let bb = bs[1].clone();
        bs[0] = bb;
        acc = acc + bs[0].xs[0] + bs[1].xs[0];
        i = i + 1;
    }
    println(acc > 0);
}
"#,
        &["true"],
        "index_assign_named_struct_source_freed_once",
    );
}

/// B-2026-08-12-31 — the displaced element is freed when an index-assign's
/// RHS reaches the container only through SCALAR reads.
///
/// `ps[0] = mk(ps[0].n + k)` leaked 400 B in 40 allocations for the same
/// reason B-2026-08-12-26 did — `emit_displaced_index_elem_drop`'s guard
/// declined — but for a different arm of it. -26 relaxed the guard for an
/// RHS that had already been deep-cloned; a call gets no such proof, so it
/// kept declining.
///
/// WHAT THE GUARD IS ACTUALLY ASKING is whether the already-computed RHS
/// value points INTO the buffer about to be freed. A textual mention of the
/// container is a proxy for that, and a coarse one: this RHS names `ps`
/// solely to read an `i64` out of it, which cannot carry a buffer anywhere.
/// The predicate now answers by REACHABILITY — an expression that never
/// names the container cannot carry its heap, and a call whose every
/// argument is safe cannot either, because the callee is handed no pointer
/// into the container to hand back. That is what makes it sound without any
/// escape analysis of the callee.
///
/// THE `rs` LEG IS THE ONE THAT WOULD CATCH A SLOPPY VERSION: the RHS reads
/// scalars from BOTH elements, including the one being overwritten, so a
/// predicate that only checked the assigned index would wave it through for
/// the wrong reason.
///
/// STILL DECLINING, deliberately, and each is a leak rather than corruption:
/// `ps[0] = passthru(ps[0])` hands the callee the element's own buffer, and
/// `ps[0] = takes(ps[0].word)` hands it a heap FIELD. Both are measured at
/// 400 B / 40 and are filed as their own row — they need the callee's escape
/// behaviour, which nothing here establishes.
#[test]
fn asan_index_assign_scalar_reaching_call_rhs_frees_displaced() {
    assert_clean_asan_run(
        r#"
struct Pair { word: String, n: i64 }
fn mk(n: i64) -> Pair { Pair { word: f"m{n}", n: n } }
fn main() {
    let k = env.args().len() as i64;
    let mut i = 0i64;
    let mut acc = 0i64;
    while i < 40 {
        let mut ps: Vec[Pair] = Vec.new();
        ps.push(Pair { word: f"alpha{k + i}", n: 1 });
        ps[0] = mk(ps[0].n + k);
        acc = acc + ps[0].word.len();
        let mut qs: Vec[Pair] = Vec.new();
        qs.push(Pair { word: f"beta{k + i}", n: 2 });
        qs[0] = mk(k + i);
        acc = acc + qs[0].word.len();
        let mut rs: Vec[Pair] = Vec.new();
        rs.push(Pair { word: f"gamma{k + i}", n: 3 });
        rs.push(Pair { word: f"delta{k + i}", n: 4 });
        rs[1] = mk(rs[0].n * 2 + rs[1].n);
        acc = acc + rs[1].word.len();
        i = i + 1;
    }
    println(acc > 0);
}
"#,
        &["true"],
        "index_assign_scalar_reaching_call_rhs_frees_displaced",
    );
}

/// B-2026-08-13-3, second half — with the double free gone, an index-assign
/// over an element whose struct has a NESTED STRUCT field is covered by
/// B-2026-08-12-33's entry-copy arm like any other.
///
/// THE FIXTURE READS ONLY SCALARS back out of the element, which is not
/// squeamishness: binding a NESTED heap field off a Vec element
/// (`let w = ds[0].inner.word`) double-frees on its own, with no call and
/// no index-assign anywhere near it — B-2026-08-12-27 clones the ONE-level
/// `ps[0].word` and has no nested sibling. That is filed as its own row and
/// reproduces identically before this change; reading it here would assert
/// that bug rather than this one.
///
/// That arm shipped with a narrower gate excluding exactly this shape,
/// because the shape aborted for an unrelated reason and no property could
/// be reasoned from a program that double-frees. The gate is gone now, and
/// this is the measurement that retired it: `ds[0] = bump(ds[0])` leaked
/// 60 B in 20 blocks with the gate in place and is clean without it.
#[test]
fn asan_index_assign_nested_struct_element_frees_displaced() {
    assert_clean_asan_run(
        r#"
struct Pair { word: String, n: i64 }
#[derive(Clone)]
struct Deep { inner: Pair, tag: i64 }
fn bump(d: Deep) -> Deep { Deep { inner: Pair { word: d.inner.word, n: d.inner.n + 1 }, tag: d.tag } }
fn main() {
    let k = env.args().len() as i64;
    let mut i = 0i64;
    let mut acc = 0i64;
    while i < 20 {
        let mut ds: Vec[Deep] = Vec.new();
        ds.push(Deep { inner: Pair { word: f"seed{k + i}", n: 0 }, tag: 5 });
        let d0 = ds[0].clone();
        ds[0] = bump(d0);
        let d1 = ds[0].clone();
        ds[0] = bump(d1);
        acc = acc + ds[0].inner.n + ds[0].tag;
        i = i + 1;
    }
    println(acc > 0);
}
"#,
        &["true"],
        "index_assign_nested_struct_element_frees_displaced",
    );
}

/// B-2026-08-12-33 — the displaced element is freed when an index-assign's
/// RHS passes container HEAP into a call, the case B-2026-08-12-31 left
/// open because a call "gets no proof" that its argument is not the
/// container's own buffer.
///
/// There are two proofs, and neither trusts the callee. `takes(ps[0].word)`
/// hands over a heap FIELD read, which B-2026-08-12-27 already deep-clones
/// AT THE READ — so the callee holds an independent buffer and the element
/// keeps its own. That clone records its span when it fires and the guard
/// reads the record, rather than restating the clone's eight admission
/// conditions and drifting from them. `passthru(ps[0])` hands over the
/// whole element, which nothing clones caller-side — but an owned bare
/// `Path` aggregate param is callee-owned by ENTRY COPY, so again what the
/// callee can hand back is its copy. That arm asks
/// `aggregate_param_copy_supported_struct`, the same predicate the entry
/// copy is gated on, and deliberately excludes own-by-transfer
/// (B-2026-08-05-33), which copies nothing and whose callee drop would
/// then collide with this free.
///
/// EVERY SHAPE HERE LEAKED 200 B IN 40 BLOCKS before the fix, measured
/// under valgrind, and the `bs` leg is the one that proves the arm is not
/// String-shaped luck: its element carries a `Vec[String]`, so the freed
/// occupant owns a buffer of buffers.
///
/// THE ORDERING IS THE RISK, not the arithmetic: the drop runs after the
/// call and before the store, i.e. it frees a buffer the RHS was computed
/// from. If either proof were wrong this fixture would abort with a double
/// free or a use-after-free rather than merely leak, which is why the
/// values are asserted alongside the clean run.
#[test]
fn asan_index_assign_call_rhs_carrying_container_heap_frees_displaced() {
    assert_clean_asan_run(
        r#"
#[derive(Clone)]
struct Pair { word: String, n: i64 }
#[derive(Clone)]
struct Bag { tags: Vec[String], n: i64 }
fn passthru(p: Pair) -> Pair { p }
fn takes(s: String) -> Pair { Pair { word: s, n: 1 } }
fn join(a: String, b: String) -> Pair { Pair { word: a + b, n: 2 } }
fn grow(b: Bag) -> Bag { Bag { tags: b.tags, n: b.n + 1 } }
fn main() {
    let k = env.args().len() as i64;
    let mut i = 0i64;
    let mut acc = 0i64;
    while i < 40 {
        let mut ps: Vec[Pair] = Vec.new();
        ps.push(Pair { word: f"alpha{k + i}", n: 1 });
        ps.push(Pair { word: f"beta{k + i}", n: 2 });
        let p0 = ps[0].clone();
        ps[0] = passthru(p0);
        ps[0] = takes(ps[0].word);
        ps[0] = join(ps[0].word, ps[1].word);
        acc = acc + ps[0].word.len() + ps[0].n;
        let mut bs: Vec[Bag] = Vec.new();
        let mut t: Vec[String] = Vec.new();
        t.push(f"tag{k + i}");
        bs.push(Bag { tags: t, n: 0 });
        let b0 = bs[0].clone();
        bs[0] = grow(b0);
        acc = acc + bs[0].n + bs[0].tags[0].len();
        i = i + 1;
    }
    println(acc > 0);
}
"#,
        &["true"],
        "index_assign_call_rhs_carrying_container_heap_frees_displaced",
    );
}

/// B-2026-08-12-26 — `ps[0] = ps[1]` over a struct element with a heap
/// field leaked one buffer per assignment (400 B in 40 allocations). Filed
/// deferred alongside B-2026-08-12-22 and `#[ignore]`d; now live.
///
/// THE LEAK WAS THE DISPLACED OCCUPANT, not anything about the read. Read
/// off the emitted IR, the whole statement is one clone and one store:
///
///   vidx.ok:  call @karac_clone_struct_Pair(ps[1] -> %vidx.elem.clone)
///   v.st.ok:  store %vidx.elem.cloned -> ps[0]      ; and no free
///
/// Slot 0's old buffer is simply overwritten. `emit_displaced_index_elem_drop`
/// exists to free exactly that and declined, because its "RHS mentions the
/// container" guard matched `ps[1]`.
///
/// THE SELF-ASSIGN LEG IS THE PROOF the guard was the cause rather than the
/// aliasing it guards against: `ps[0] = ps[0]` leaked identically, and no
/// aliasing argument can justify declining there — the value has already
/// been deep-cloned by the time the drop would run. The guard is now
/// skipped when the RHS was cloned, which is the one condition under which
/// the hazard it protects against cannot exist.
///
/// The `qs` legs are why the row mattered more than its shape suggests: the
/// one-temp swap `let t = qs[0]; qs[0] = qs[1]; qs[1] = t;` contains this
/// statement, so a hand-written sort leaked one element buffer per swap
/// while the two-temp spelling was clean — a surprising thing to have to
/// know. Both spellings are here, and they must agree.
///
/// An f-STRING is required to see any of it: a string literal has `cap` 0,
/// so the buffer is static and the leak is invisible.
#[test]
fn asan_index_assign_elem_to_elem_no_leak() {
    assert_clean_asan_run(
        r#"
#[derive(Clone)]
struct Pair { word: String, n: i64 }
fn main() {
    let k = env.args().len() as i64;
    let mut i = 0i64;
    let mut acc = 0i64;
    while i < 40 {
        let mut ps: Vec[Pair] = Vec.new();
        ps.push(Pair { word: f"alpha{k + i}", n: 1 });
        ps.push(Pair { word: f"beta{k + i}", n: 2 });
        ps[0] = ps[1].clone();
        acc = acc + ps[0].word.len();
        ps[0] = ps[0].clone();
        acc = acc + ps[0].word.len();
        let mut qs: Vec[Pair] = Vec.new();
        qs.push(Pair { word: f"gamma{k + i}", n: 3 });
        qs.push(Pair { word: f"delta{k + i}", n: 4 });
        qs.swap(0, 1);
        acc = acc + qs[0].word.len() + qs[1].word.len();
        let mut rs: Vec[Pair] = Vec.new();
        rs.push(Pair { word: f"gamma{k + i}", n: 3 });
        rs.push(Pair { word: f"delta{k + i}", n: 4 });
        rs.swap(0, 1);
        acc = acc + rs[0].word.len() + rs[1].word.len();
        let j = 1i64;
        let mut ss: Vec[Pair] = Vec.new();
        ss.push(Pair { word: f"eps{k + i}", n: 5 });
        ss.push(Pair { word: f"zeta{k + i}", n: 6 });
        ss[0] = ss[j].clone();
        acc = acc + ss[0].word.len();
        i = i + 1;
    }
    println(acc > 0);
}
"#,
        &["true"],
        "index_assign_elem_to_elem_no_leak",
    );
}

/// B-2026-08-02-5 — tuple-element assignment over HEAP elements: the
/// displaced old element's buffer frees in place (String elems via the
/// recorded elem TE, or the LLVM vec-struct fallback when a plain
/// tuple binding's element type name is unrecorded), an f-string RHS's
/// accumulator is neutralised after the move (else the tuple's element
/// drop double-frees it), and a struct element's old fields free via
/// its drop fn. Owner-death walks free exactly the NEW values.
#[test]
fn asan_tuple_index_assignment_heap_elems_freed() {
    assert_clean_asan_run(
        r#"
struct Ph { id: i64, name: String }
struct Oh { t: (String, Ph) }
fn main() {
    let mut t = (f"a{1}", 2);
    println(f"pre {t.0}");
    t.0 = f"b{3}";
    println(f"post {t.0}");
    let mut o = Oh { t: (f"c{4}", Ph { id: 9, name: f"z{9}" }) };
    println(f"opre {o.t.0} {o.t.1.name}");
    o.t.0 = f"d{5}";
    o.t.1 = Ph { id: 6, name: f"w{6}" };
    println(f"opost {o.t.0} {o.t.1.id} {o.t.1.name}");
    println("end");
}
"#,
        &["pre a1", "post b3", "opre c4 z9", "opost d5 6 w6", "end"],
        "tuple_index_assignment_heap_elems_freed",
    );
}

#[test]
fn asan_shared_vec_field_index_field_mutation_no_leak() {
    // B-2026-07-13-10 leak/UAF gate. The read/store fixes GEP a shared
    // element's heap field through a Vec that is itself a FIELD of a shared
    // struct (`root.kids[i].val`). Both paths reach the element handle via
    // `compile_expr(Index)`, which is a PURE read (the field-access-rooted
    // index mints a synth Vec identifier and recurses — no rc_inc), so
    // neither adds an owned ref that would need a matching dec. This churns
    // the shape 300× — each iter builds a shared root + two shared children,
    // pushes the children into the `kids` Vec field (RC co-ownership),
    // mutates them through the chained store, and reads them back — so a
    // stray inc on the chain (leak) or a missed dec of the pushed children
    // at Vec-drop (leak) is caught by LSan on Linux, and a double-free of a
    // child box would trip ASAN.
    assert_clean_asan_run(
        r#"
shared struct Node { mut val: i64, mut kids: Vec[Node] }
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 300 {
        let root = Node { val: 1, kids: Vec.new() };
        let a = Node { val: 10, kids: Vec.new() };
        let b = Node { val: 20, kids: Vec.new() };
        root.kids.push(a);
        root.kids.push(b);
        root.kids[0].val = root.kids[0].val + 5;
        root.kids[1].val = 99;
        total = total + root.kids[0].val + root.kids[1].val;
        i = i + 1;
    }
    println(total.to_string());
}
"#,
        // Each iter: kids[0]=15, kids[1]=99 → 114. 300*114 = 34200.
        &["34200"],
        "shared_vec_field_index_field_mutation_no_leak",
    );
}

#[test]
fn asan_vec_option_shared_index_overwrite_place_rhs_no_leak() {
    // B-2026-07-12-29 minimal repro (Option[shared] element, place RHS):
    // `work[0] = work[1]` overwrites slot 0's OLD node with slot 1's — the
    // overwritten node must be rc-dec'd or it leaks on arm64. The retain of
    // the RHS is upstream (the index-read clone rc-incs), so the store site
    // must release the old. slot0 holds a 2-node chain so BOTH orphaned
    // nodes would leak pre-fix; the driver keeps slot 1 alive and walks it.
    assert_clean_asan_run(
        r#"
shared struct ListNode {
    val: i64,
    mut next: Option[ListNode],
}

fn probe(lists: Vec[Option[ListNode]]) -> Option[ListNode] {
    let mut work = lists;
    work[0] = work[1].clone();
    work[0]
}

fn main() {
    let a2 = ListNode { val: 10, next: None };
    let a1 = ListNode { val: 11, next: Some(a2) };
    let b = ListNode { val: 22, next: None };
    let mut v: Vec[Option[ListNode]] = Vec.new();
    v.push(Some(a1));
    v.push(Some(b));
    let mut cur = probe(v);
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
        &["22"],
        "vec_option_shared_index_overwrite_place_rhs",
    );
}

#[test]
fn asan_vec_plain_shared_index_overwrite_place_rhs_no_leak() {
    // B-2026-07-12-29 sibling — plain `Vec[shared T]` (non-Option) element.
    // The element clone is a SHALLOW pointer copy (no rc-inc), so
    // `work[0] = work[1]` pre-fix BOTH double-freed slot 1's box (two slots
    // aliasing one un-inc'd box → two scope-exit decs) AND leaked slot 0's
    // old box. The setter rule retains the new (place RHS) and releases the
    // old, balancing both.
    assert_clean_asan_run(
        r#"
shared struct Node { val: i64 }

fn probe(xs: Vec[Node]) -> i64 {
    let mut work = xs;
    work[0] = work[1];
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
        &["9"],
        "vec_plain_shared_index_overwrite_place_rhs",
    );
}

#[test]
fn asan_nested_index_weak_read_takes_a_balancing_acquire() {
    // The read half, isolated one nesting level deeper so the chained index
    // is the only variable. `expr_is_weak_field_read` decides whether a
    // weak read needs the balancing acquire that matches the
    // `RcDecOption` its binding queues, and it resolved the receiver only
    // through a bare identifier (`v[i]`) or a struct field (`a.ns[i]`) --
    // `_ => None` for a nested `Index`. So `outer[0][0]` took the dec
    // without the inc and over-released the referent by exactly one, the
    // identical failure B-2026-07-21-21 measured for the field spelling and
    // B-2026-08-08-28 for the field-rooted element one.
    //
    // Three levels rather than two so the resolver's RECURSION is what is
    // pinned, not a single hard-coded extra level.
    assert_clean_asan_run(
        r#"
shared struct N { v: i64 }

fn main() {
    let a: N = N { v: 7 };
    let mut lvl0: Vec[weak N] = Vec.new();
    lvl0.push(a);
    let mut lvl1: Vec[Vec[weak N]] = Vec.new();
    lvl1.push(lvl0);
    let mut lvl2: Vec[Vec[Vec[weak N]]] = Vec.new();
    lvl2.push(lvl1);
    match lvl2[0][0][0] { Some(x) => { println(x.v) } None => { println(0 - 1) } }
    println(a.v);
}
"#,
        &["7", "7"],
        "nested_index_weak_read_balancing_acquire",
    );
}

#[test]
fn asan_self_field_vec_index_match_move_out_no_double_free() {
    // #32 (phase-12 self-hosting, parser stage): reading + matching a token
    // through a `self`-field-rooted Vec index — `self.toks[self.pos].tok` —
    // is the parser's core token-access shape. A scalar field read and a
    // payload-binding match both go through `compile_field_access`'s generic
    // value path, which now resolves the element struct type via
    // `type_name_of_expr`'s `Index` arm (the fix). The payload-binding match
    // moves a `String` out of the element via the existing #16/#25 source
    // suppression; this asserts no double-free (macOS ASAN) and no leak
    // (Linux LSan). Every heap element is CONSUMED by `take` so the dropped
    // Vec holds no live enum-String payloads — isolating the #32 read +
    // move-out from the SEPARATE pre-existing Vec[struct-with-enum-field]-
    // element-drop leak ([#35]), which a Vec left holding live heap-enum
    // elements would otherwise trip.
    assert_clean_asan_run(
        r#"
enum Tk { A, Id(String), Num(i64) }
struct Sp { tok: Tk, off: i64 }
struct P { toks: Vec[Sp], pos: i64 }
impl P {
    fn off_now(ref self) -> i64 { self.toks[self.pos].off }
    fn kind_now(ref self) -> i64 {
        match self.toks[self.pos].tok { Id(_) => 1, Num(_) => 2, A => 3 }
    }
    fn take(mut ref self) -> String {
        match self.toks[self.pos].tok {
            Id(s) => { self.pos = self.pos + 1; s }
            Num(n) => { self.pos = self.pos + 1; n.to_string() }
            A => { self.pos = self.pos + 1; "a".to_string() }
        }
    }
}
fn main() {
    let mut w: Vec[Sp] = Vec.new();
    w.push(Sp { tok: Tk.Id("hello".to_string()), off: 5 });
    w.push(Sp { tok: Tk.Id("world".to_string()), off: 9 });
    let mut p = P { toks: w, pos: 0 };
    println(p.off_now().to_string());
    println(p.kind_now().to_string());
    println(p.take());
    println(p.off_now().to_string());
    println(p.take());
}
"#,
        &["5", "1", "hello", "9", "world"],
        "self_field_vec_index_match_move_out",
    );
}

#[test]
fn asan_borrowed_index_field_enum_scrutinee_binding_outlives_container_no_uaf() {
    // #38 (phase-12 self-hosting, parser stage): matching a `.token`
    // FieldAccess rooted on a Vec `Index` (`self.toks[self.pos].tok`) on a
    // BORROWED receiver, binding a `String` payload that ESCAPES the call
    // (returned out, outliving the `Parser`'s token `Vec`). Without the
    // `clone_borrowed_index_field_enum_scrutinee` clone, the binding
    // shallow-ALIASES the Vec element's `{ptr,len,cap}`; when the container
    // drops it frees that buffer, leaving the escaped String dangling — a
    // use-after-free on the next read and a double-free at its own drop.
    //
    // This isolated ASAN test was IMPOSSIBLE before [#35] was fixed: the
    // old struct-field Vec drop freed only the buffer and leaked the live
    // `Id(String)` element, so the aliased buffer stayed allocated and the
    // dangle was never an observable UAF. Now that [#35] drains each
    // element's payload on the container's drop, the alias (if the #38
    // clone is removed) becomes a genuine ASAN-flaggable UAF + double-free.
    // The two fixes are complementary: #35 frees the original element once,
    // #38 gives the escaped binding an independent buffer freed once.
    // Looped + heap-sized payloads so a per-iteration fault is unmissable.
    assert_clean_asan_run(
        r#"
enum Tk { Id(String), Num(i64) }
struct Sp { tok: Tk, off: i64 }
struct P { toks: Vec[Sp], pos: i64 }
impl P {
    fn name_now(ref self) -> String {
        match self.toks[self.pos].tok {
            Id(s) => s,
            Num(n) => n.to_string(),
        }
    }
}
fn mk() -> Vec[Sp] {
    let mut w: Vec[Sp] = Vec.new();
    w.push(Sp { tok: Tk.Id("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string()), off: 5 });
    w
}
fn grab() -> String {
    let p = P { toks: mk(), pos: 0 };
    p.name_now()
}
fn main() {
    let mut total: i64 = 0;
    let mut i: i64 = 0;
    while i < 50 {
        let n = grab();
        total = total + n.len();
        i = i + 1;
    }
    println(total);
}
"#,
        &["2000"],
        "borrowed_index_field_enum_scrutinee_binding_outlives_container",
    );
}

#[test]
fn asan_vec_option_shared_index_reused_across_consuming_calls_no_uaf() {
    // B-2026-07-11-29 layer 3 (corruption): a `Vec[Option[shared]]` ELEMENT
    // read by index (`src[0]`) passed BY VALUE to a consuming (cloning)
    // callee TWICE. The niche Vec-element read loads the inner pointer
    // WITHOUT an inc, so the callee's `Option[shared]` param `RcDecOption`
    // over-decremented the element the container still owns — freeing it
    // mid-sequence; a later alloc reused the slot and the second `src[0]`
    // read returned the wrong node (interpreter `4`, codegen corrupted /
    // use-after-free). The Index companion `share_option_shared_index_ref_for_arg`
    // now retains the loaded inner per pass, mirroring the Identifier /
    // FieldAccess arg companions.
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
    let l0 = clone_offset(src[0].clone(), 10);
    let l1 = clone_offset(src[0].clone(), 20);
    println(count_nodes(l0) + count_nodes(l1));
}
"#,
        &["4"], // two 2-node clones off the same live element
        "vec_option_shared_index_reused_across_consuming_calls_no_uaf",
    );
}

#[test]
fn asan_struct_field_shares_vec_option_shared_index_no_leak() {
    // B-2026-07-11-29 (struct-field share leg): sharing a
    // `Vec[Option[shared]]` element DIRECTLY into a struct-literal field
    // (`Node { left: src[0] }`) double-inc'd the node — `maybe_defensive_copy_param_arg`
    // retain-cloned it (`karac_clone_Option_Node`) AND the field capture-inc
    // fired — with no binding to carry a matching dec, so the node's rc never
    // returned to zero and it leaked (LSan). The capture-inc is now skipped
    // for an already-retained `v[i]` field value.
    assert_clean_asan_run(
        r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn count_nodes(node: Option[Node]) -> i64 {
    match node { None => 0, Some(n) => 1 + count_nodes(n.left) + count_nodes(n.right) }
}
fn main() {
    let mut src: Vec[Option[Node]] = Vec.new();
    src.push(Some(Node { val: 1, left: Some(Node { val: 2, left: None, right: None }), right: None }));
    let mut cur: Vec[Option[Node]] = Vec.new();
    cur.push(Some(Node { val: 0, left: src[0], right: None }));
    cur.push(Some(Node { val: 9, left: src[0], right: None }));
    println(count_nodes(cur[0].clone()) + count_nodes(cur[1].clone()));
}
"#,
        &["6"], // two 3-node trees sharing the same left subtree
        "struct_field_shares_vec_option_shared_index_no_leak",
    );
}

#[test]
fn asan_index_swap_vecvec_no_double_free() {
    // Same idiom with a `Vec[Vec[i64]]` (a non-Copy INNER Vec element): the
    // inner `{ptr,len,cap}` was aliased on `v[0] = v[1]`. Confirms the clone
    // fires for any non-trivially-copyable element, not just String.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut v: Vec[Vec[i64]] = [[10, 11], [20, 21], [30, 31]];
    let t = v[0].clone();
    v[0] = v[1].clone();
    v[1] = t;
    println(v[0][0]);
    println(v[1][0]);
    println(v[2][0]);
}
"#,
        &["20", "10", "30"],
        "index_swap_vecvec_no_double_free",
    );
}

#[test]
fn asan_index_read_into_var_no_double_free_or_leak() {
    // `s = v[i]` — an index-read into an EXISTING (already heap-owning)
    // binding. Two hazards in one: the RHS clone must fire (else `s` and
    // `v[1]` alias → double-free), AND the overwritten old `s` buffer must
    // be eagerly freed (else LeakSanitizer flags the orphaned "old-padding").
    assert_clean_asan_run(
        r#"
fn main() {
    let mut v: Vec[String] = [f"aa-padding", f"bb-padding"];
    let mut s: String = f"old-padding";
    s = v[1].clone();
    println(s);
    println(v[0]);
    println(v[1]);
}
"#,
        &["bb-padding", "aa-padding", "bb-padding"],
        "index_read_into_var_no_double_free_or_leak",
    );
}

#[test]
fn asan_return_field_index_element_no_double_free() {
    // B-2026-07-11-35 (return leg): a method/fn that returns a field-rooted
    // index element (`fn get(ref self) -> String { self.xs[i] }`,
    // `fn getf(h: ref H, i) -> String { h.xs[i] }`) used to return an ALIAS
    // of the container's element — a `ref self`/`ref Struct` can't move it
    // out, so the returned owned `String` and the container's element both
    // freed the buffer at scope exit (double-free; the bare-`v[i]` return
    // already produced an independent value). The fn-tail now deep-clones a
    // field-rooted index read. Covers both the `self.field[i]` and the
    // `h.field[i]`-via-ref-param shapes, with the returned value bound AND
    // consumed directly, over heap String elements built and dropped.
    assert_clean_asan_run(
        r#"
struct H { xs: Vec[String] }
impl H {
    fn get(ref self, i: i64) -> String { self.xs[i] }
}
fn getf(h: ref H, i: i64) -> String { h.xs[i] }
fn main() {
    let mut r: i64 = 0;
    while r < 3 {
        let h = H { xs: [f"alpha-{r}-pad", f"bravo-{r}-pad", f"charlie-{r}"] };
        println(h.get(0));
        let bound: String = getf(h, 1);
        println(bound);
        println(h.xs[2]);
        r = r + 1;
    }
}
"#,
        &[
            "alpha-0-pad",
            "bravo-0-pad",
            "charlie-0",
            "alpha-1-pad",
            "bravo-1-pad",
            "charlie-1",
            "alpha-2-pad",
            "bravo-2-pad",
            "charlie-2",
        ],
        "return_field_index_element_no_double_free",
    );
}

#[test]
fn asan_direct_index_match_option_shared_no_leak() {
    // B-2026-07-12-21: a direct `match vec[i]` whose scrutinee is an
    // `Option[shared]` index-read LEAKED the extracted node once per match.
    // The index read deep-cloned the element (rc-INC via the concrete
    // `Option[shared Node]` clone fn), but the fresh-temp match scrutinee's
    // drop was resolved from the ERASED generic `Option` layout (all-`None`
    // drop-kinds), so `has_droppable` was false and the retained rc was
    // never released. Fix (lowering): rewrite `match vec[i] { … }` into the
    // proven-clean let-bound form `{ let s = vec[i]; match s { … } }`, whose
    // binding carries the concrete type so its cleanup releases the clone.
    // Looped in a HELPER fn (not `main`) so the per-iteration leak is real
    // and not masked by `main`'s final-scope drop elision; reads `nd.val`
    // back so a wrong-node miscompile would also surface.
    assert_clean_asan_run(
        r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn xfer() -> i64 {
    let mut dst: Vec[Option[Node]] = Vec.new();
    dst.push(Some(Node { val: 10, left: None, right: None }));
    let mut r: i64 = 0;
    match dst[0] {
        None => {}
        Some(nd) => { r = nd.val; }
    }
    r
}
fn main() {
    let mut i: i64 = 0;
    let mut t: i64 = 0;
    while i < 200 {
        t = t + xfer();
        i = i + 1;
    }
    println(f"{t}");
}
"#,
        &["2000"],
        "direct_index_match_option_shared_no_leak",
    );
}

/// B-2026-08-10-1 — a nested indexed store frees the element it displaces.
///
/// `d[i][j] = <String>` left the overwritten buffer orphaned;
/// `compile_nested_vec_vec_index_store` ended in a bare `build_store`
/// while the single-index path had freed the displaced element since
/// B-2026-06-19-7.
///
/// COVERAGE, measured against the pre-fix compiler rather than assumed:
/// 24 B in 8 blocks at `KARAC_OPT_LEVEL=0` **and** the same at the default
/// `-O2`. So unlike its neighbours this fixture does NOT depend on the
/// `-O0` leg — the loop-derived labels (`f"r{i}a"`) keep the displaced
/// buffers from folding into dead allocations LLVM deletes, which is the
/// trap B-2026-08-09-13 and B-2026-08-09-4 document and the reason the row
/// insists on `-O0` for hand measurements of this path. A bare `d[0][0] =
/// f"zz"` probe is `-O0`-only; this one witnesses at both levels.
///
/// The 8 blocks are two per row: the displaced leaf from `o[j][0] = …`,
/// and the one from the self-alias store, whose RHS is cloned so the old
/// buffer is orphaned just the same.
///
/// The self-alias row (`o[j][0] = o[j][0]`) is the direction where an
/// over-eager release frees the buffer it is about to store back — an
/// invalid free ASAN catches, as opposed to the leak LSan catches.
#[test]
fn asan_nested_index_store_frees_the_displaced_element() {
    assert_clean_asan_run(
        r#"
fn main() {
    let mut o: Vec[Vec[String]] = Vec.new();
    let mut i: i64 = 0i64;
    while i < 4i64 {
        let mut row: Vec[String] = Vec.new();
        row.push(f"r{i}a");
        row.push(f"r{i}b");
        o.push(row);
        i = i + 1;
    }
    let mut j: i64 = 0i64;
    while j < 4i64 {
        // Overwrite a heap leaf — the displaced buffer must be freed.
        o[j][0] = f"n{j}";
        // SELF-ALIAS — the release must not free the value being stored.
        let row: Vec[String] = o[j].clone();
        o[j][1] = row[1].clone();
        j = j + 1i64;
    }
    let mut k: i64 = 0i64;
    while k < 4i64 {
        println(o[k][0]);
        println(o[k][1]);
        k = k + 1i64;
    }
    println("end");
}
"#,
        &["n0", "r0b", "n1", "r1b", "n2", "r2b", "n3", "r3b", "end"],
        "nested_index_store_displaced_elem",
    );
}

/// B-2026-08-14-9 — the heap-element half of the new `Slice` mutators.
///
/// `fill` is the only one of the eight that changes what a slot OWNS, and
/// it has three ways to go wrong that no output comparison can see: leaking
/// every value it overwrites, aliasing one buffer into every slot so scope
/// exit double-frees, and leaking the ARGUMENT when there is no slot to
/// move it into. All three are exercised here — a non-empty fill over
/// already-owned strings, and a fill over an EMPTY slice whose argument
/// nothing consumes.
///
/// `sort` / `reverse` / `swap` are ownership-neutral (they permute values
/// that stay in the same buffer) and `chunks` / `windows` / `split_at`
/// produce borrowed headers that own nothing, so this fixture runs them
/// alongside to pin that they add no allocation of their own.
#[test]
fn asan_slice_mutators_and_views_on_heap_elements() {
    assert_clean_asan_run(
        r#"
fn ssort(xs: mut Slice[String]) { xs.sort(); }
fn srev(xs: mut Slice[String]) { xs.reverse(); }
fn sswap(xs: mut Slice[String]) { xs.swap(0i64, 2i64); }
fn sfill(xs: mut Slice[String]) { xs.fill("zz"); }

fn main() {
    let mut v: Vec[String] = Vec.new();
    v.push("pear"); v.push("apple"); v.push("fig");
    ssort(v.as_slice_mut());
    srev(v.as_slice_mut());
    sswap(v.as_slice_mut());
    {
        let sv = v.as_slice();
        let cs = sv.chunks(2i64);
        let c0 = cs[0i64];
        println(f"{cs.len()} {c0.len()} {c0[0i64]}");
        let ws = sv.windows(2i64);
        println(f"{ws.len()}");
        let p = sv.split_at(1i64);
        println(f"{p.0.len()} {p.1.len()}");
    }
    sfill(v.as_slice_mut());
    println(f"{v[0i64]} {v[1i64]} {v[2i64]}");
    let mut e: Vec[String] = Vec.new();
    sfill(e.as_slice_mut());
    println(f"{e.len()}");
    println("end");
}
"#,
        &["2 2 apple", "2", "1 2", "zz zz zz", "0", "end"],
        "slice_mutators_heap_elements",
    );
}

/// B-2026-08-14-37 — reclassifying a read-only `Slice[T]` formal from
/// OWNED to BORROW changes what codegen emits, and this is the gate on
/// that change being safe.
///
/// The reclassification is not diagnostics-only. A `UseAfterMove` is
/// non-fatal by design: codegen reads
/// `use_after_move_consume_sites` and, at every flagged reuse,
/// deep-copies the value AND skips the source's cleanup disarm — both
/// halves, because a copy alone leaks the source and a disarm-skip alone
/// turns the use-after-free into a double free. Every `count(words)` here
/// used to be such a site. With the argument correctly a borrow those
/// sites disappear, so the container is now passed and freed exactly once
/// by its own scope-exit drop — and the two ways to get that wrong are a
/// leak (the defensive copies are gone but something still disarms the
/// source) and a double free (the source drops and a stale copy's cleanup
/// survives). `String` elements make either one visible; the printed
/// values are identical in all three worlds.
#[test]
fn asan_read_only_slice_param_borrows_its_argument() {
    assert_clean_asan_run(
        r#"
fn count(xs: Slice[String]) -> i64 { xs.len() }
fn scount(xs: Slice[i64]) -> i64 { xs.len() }
fn twice(f: Fn() -> i64) -> i64 { f() + f() }
fn mk() -> Vec[String] {
    let mut v: Vec[String] = Vec.new();
    v.push("alphabetical"); v.push("betamax");
    v
}

fn main() {
    let words: Vec[String] = ["alphabetical", "betamax", "gamma-ray-burst"];
    println(f"{count(words)} {count(words)} {count(words)}");
    println(f"{words[0i64]} {words.len()}");

    let nums: Vec[i64] = [10i64, 20i64, 30i64];
    println(f"{twice(|| scount(nums))} {nums.len()}");

    println(f"{count(mk())}");
    println(f"{count(words.as_slice())} {count(words[0..2])}");
    println("end");
}
"#,
        &["3 3 3", "alphabetical 3", "6 3", "2", "3 2", "end"],
        "read_only_slice_param_borrows",
    );
}

/// B-2026-08-15-24, memory half — the TUPLE-element twin of the fixture
/// below, on the same reasoning.
///
/// This store did not merely go missing before the fix: it failed the
/// BUILD, so no binary existed to check and the displaced value's fate was
/// never exercised at all. Making it lower is what makes the overwrite
/// observable, and the overwrite must free the old buffer exactly once
/// while the caller's element keeps the new one — a leak or a double free
/// that the E2E value assertions cannot see either way.
///
/// The `mut ref Vec` half runs the identical workload through the arm that
/// worked all along (B-2026-08-02-8), so a divergence between the two is a
/// slice-specific defect rather than a property of tuple-element stores.
#[test]
fn asan_tuple_elem_store_through_mut_slice_frees_displaced() {
    assert_clean_asan_run(
        r#"
fn mk(n: i64) -> String {
    let mut s = "";
    let mut i = 0;
    while i < n { s = s + "abcdefghij"; i = i + 1; }
    return s;
}
fn set_slice(s: mut Slice[(String, i64)]) {
    let mut i = 0;
    while i < s.len() { s[i].0 = mk(12); i = i + 1; }
}
fn set_vec(v: mut ref Vec[(String, i64)]) {
    let mut i = 0;
    while i < v.len() { v[i].0 = mk(12); i = i + 1; }
}
fn set_nested(s: mut Slice[(Vec[String], i64)]) { s[0].0 = ["freshfreshfresh"]; }
fn main() {
    let mut ps: Vec[(String, i64)] = Vec.new();
    ps.push((mk(30), 1));
    ps.push((mk(40), 2));
    // Repeated, so a per-store imbalance accumulates instead of hiding in
    // a single displacement.
    let mut k = 0;
    while k < 5 { set_slice(mut ps); k = k + 1; }
    println(f"{ps[0].0.len()} {ps[1].0.len()}");

    let mut qs: Vec[(String, i64)] = Vec.new();
    qs.push((mk(30), 1));
    let mut j = 0;
    while j < 5 { set_vec(mut qs); j = j + 1; }
    println(f"{qs[0].0.len()}");

    // A displaced CONTAINER element strands more than one buffer if the old
    // value is dropped shallowly or not at all.
    let mut ns: Vec[(Vec[String], i64)] = Vec.new();
    let seed: Vec[String] = ["oldoldoldoldold", "olderolderolder"];
    ns.push((seed, 1));
    set_nested(mut ns);
    println(f"{ns[0].0[0]} {ns[0].0.len()}");
    println("end");
}
"#,
        &["120 120", "120", "freshfreshfresh 1", "end"],
        "tuple_elem_store_through_mut_slice_frees_displaced",
    );
}

/// B-2026-08-15-21, memory half — the field store 9b284cb makes LAND is
/// also the first one that can displace a heap value on this path.
///
/// Before, the store was never emitted, so nothing was ever overwritten and
/// the old `String` stayed reachable through the caller's Vec. Making the
/// write happen is what makes the displaced value's fate observable: the
/// overwrite has to free the old buffer exactly once, and the caller's
/// element must still own the NEW one at scope exit. A missing displaced
/// drop leaks; a doubled one is a double free; neither shows up in the
/// output assertion the E2E twin makes.
///
/// Added on top of the fix rather than with it: the row's E2E and
/// interpreter pins assert the VALUES, and every one of them would still
/// pass if the overwrite stranded the old buffer or freed it twice.
#[test]
fn asan_field_store_through_mut_slice_param_frees_the_displaced_value() {
    assert_clean_asan_run(
        r#"
struct E { name: String, n: i64 }
struct Tags { items: Vec[String], n: i64 }

fn set_name(s: mut Slice[E]) { s[0].name = "replacedreplacedreplaced"; }
fn set_all(s: mut Slice[E]) {
    let mut i = 0;
    while i < s.len() { s[i].name = "uniformuniformuniform"; i = i + 1; }
}
fn set_items(s: mut Slice[Tags]) { s[0].items = ["freshfreshfresh"]; }

fn main() {
    let mut es: Vec[E] = Vec.new();
    es.push(E { name: "alphabetical", n: 7 });
    es.push(E { name: "betamaximum", n: 8 });

    // One displaced String.
    set_name(mut es);
    println(f"{es[0].name} {es[1].name}");

    // Every element displaced, in a loop.
    set_all(mut es);
    println(f"{es[0].name} {es[1].name} {es.len()}");

    // A displaced CONTAINER field, which strands more than one buffer if the
    // old value is dropped shallowly or not at all.
    let mut ts: Vec[Tags] = Vec.new();
    let seed: Vec[String] = ["oldoldoldoldold", "olderolderolder"];
    ts.push(Tags { items: seed, n: 1 });
    set_items(mut ts);
    println(f"{ts[0].items[0]} {ts[0].items.len()}");
    println("end");
}
"#,
        &[
            "replacedreplacedreplaced betamaximum",
            "uniformuniformuniform uniformuniformuniform 2",
            "freshfreshfresh 1",
            "end",
        ],
        "field_store_through_mut_slice_param_frees_displaced",
    );
}

/// B-2026-08-20-40 — reading an element out of a SLICE temporary
/// (`v.as_slice()[i]`, `s.bytes()[i]`) now lowers. The header is a BORROW
/// of storage a live binding owns, so the new path deliberately frees
/// nothing and clones nothing — it materializes the `{ptr, len}` into a
/// synth local and performs exactly the element read a named slice binding
/// performs. This pins that choice from both sides: a heap element read
/// through the temporary must not hand the element's buffer a second owner
/// (double-free), and the temporary must not acquire one of its own and
/// free the Vec's buffer out from under it (use-after-free). Loops so any
/// per-iteration imbalance accumulates past anything LSan could miss.
#[test]
fn asan_index_into_slice_temporary_frees_nothing() {
    assert_clean_asan_run(
        r#"fn main() {
    let names: Vec[String] = ["alpha", "beta", "gamma"];
    let s: String = "hello";
    let mut i = 0i64;
    let mut last: String = "";
    let mut sum = 0i64;
    while i < 60i64 {
        let sl = names.as_slice();
        last = sl[i % 3i64].clone();
        sum = sum + s.bytes()[i % 5i64] as i64;
        i = i + 1;
    }
    println(last);
    println(sum);
    println(names[0]);
}
"#,
        &["gamma", "6384", "alpha"],
        "asan_index_into_slice_temporary_frees_nothing",
    );
}

/// B-2026-08-21-23 — a fixed array handed to a METHOD's `ref Slice[T]`
/// parameter, under the sanitizer.
///
/// Two things need pinning from opposite sides. The OLD lowering passed
/// `&array[0]`, so the callee read `{ptr,len}` out of the first two
/// ELEMENTS and dereferenced element bytes as a pointer — a wild read that
/// segfaulted once the body indexed. The NEW one synthesizes a header in a
/// stack temp, which must stay a BORROW: the array still owns its heap
/// elements, so the header must not hand them a second owner. A `String`
/// element makes both failure modes reachable, and the loop makes a
/// per-call imbalance accumulate rather than hide in one iteration.
#[test]
fn asan_fixed_array_into_a_method_ref_slice_is_a_borrow() {
    assert_clean_asan_run(
        r#"struct H { n: i64 }
struct Held { rows: Array[String, 3] }
impl H {
    fn first(ref self, xs: ref Slice[String]) -> String { xs[0] }
    fn count(ref self, xs: ref Slice[String]) -> i64 { xs.len() }
}
fn main() {
    let h = H { n: 0 };
    let held = Held { rows: ["alpha", "beta", "gamma"] };
    let mut i = 0i64;
    let mut total = 0i64;
    let mut last: String = "";
    while i < 50i64 {
        let owned: Array[String, 3] = ["one", "two", "three"];
        total = total + h.count(owned) + h.count(held.rows);
        last = h.first(owned);
        i = i + 1;
    }
    println(last);
    println(total);
    println(held.rows[2]);
}
"#,
        &["one", "300", "gamma"],
        "asan_fixed_array_into_a_method_ref_slice_is_a_borrow",
    );
}

/// B-2026-08-25-32 — the same `peek`, but on a `T` that reports its own
/// destruction, so the drop COUNT is asserted rather than inferred.
///
/// Two peeks over a three-element queue must produce exactly two extra drops
/// (one per returned copy, each dying at the end of its `match` arm) and the
/// three elements must still drop exactly once each afterwards. A shallow
/// copy would double-free one buffer; a move-out would drop `apple` early
/// and leave the queue two long.
///
/// Two rows had to close before this could run, and the history is worth
/// keeping because each one hid the next. B-2026-08-25-35 fixed the half
/// that made the fixture *uncompilable*: a `#[derive(Ord)]` struct now
/// satisfies the `T: Ord` bound on a generic impl's method, so it can go in
/// a `PriorityQueue` at all — hence the derive form below rather than a
/// hand-written `impl Ord` (which still cannot: see that row for why
/// admitting it would silently overrule the user's `cmp` body). Compiling
/// then exposed B-2026-08-26-9, and this fixture was the oracle for it:
/// the CALLER ran its fresh-temp drop for an argument
/// `PriorityQueue.push` had already moved into the backing `Vec`, so each
/// element dropped once at the push and again at the pop. AOT printed
/// `drop 3 / drop 1 / drop 2` before `built` while `--interp` printed
/// nothing there, and LSan reported 31 bytes leaked in 8 allocations.
///
/// The `31 byte(s) leaked in 8 allocation(s)` the row reported was NOT
/// caused by that double drop and did not go away with it — it was a
/// second, independent defect the row had bundled in, split out as
/// B-2026-08-26-18 and fixed in turn: an index-assign whose container is
/// reached through a field of `self` (`PriorityQueue.swap`'s
/// `self.xs[i] = self.xs[j]`) never released the element it overwrote,
/// because the displaced-element drop gated on a direct `Identifier` root
/// and `self` is its own AST node. With both fixed this fixture is live:
/// the drop SEQUENCE below covers -9 and LSan cleanliness covers -18.
/// B-2026-08-26-18 regression oracle — the MINIMAL shape.
///
/// An index-assign whose container is reached through a field of `self`
/// never released the element it overwrote, so the displaced struct's
/// `String` buffer was orphaned on every store. `self` parses as
/// `ExprKind::SelfValue`, not `Identifier`, and
/// `emit_displaced_index_elem_drop`'s field-rooted arm gated on a direct
/// `Identifier` root — so it returned early for every self-rooted store
/// while `compile_index_store` (which normalises `SelfValue`) happily
/// emitted the store. The store landed; the release did not.
///
/// The contrast that localises it is
/// `named_root_index_assign_releases_displaced_element` below: the SAME
/// statement written against a named root is clean, and pre-fix the two
/// binaries differed by exactly one `karac_free_buf` call.
#[test]
fn asan_self_rooted_index_assign_releases_displaced_element() {
    assert_clean_asan_run(
        r#"
struct Item { id: i64, name: String }
struct Bag { xs: Vec[Item] }
impl Bag {
    fn go(mut ref self) { self.xs[0] = Item { id: 9, name: f"z{9}" }; }
}
fn main() {
    let mut b = Bag { xs: Vec.new() };
    b.xs.push(Item { id: 1, name: f"a{1}" });
    b.go();
    println(f"{b.xs[0].id}");
}
"#,
        &["9"],
        "self-rooted-index-assign-release",
    );
}

/// B-2026-08-26-18 CONTRAST oracle — the named-root twin of the fixture
/// above, which was already clean before the fix. It pins the asymmetry
/// that identified the bug, so a future change that regresses the NAMED
/// path fails here rather than silently making both roots leak and
/// leaving the pair looking consistent.
#[test]
fn asan_named_root_index_assign_releases_displaced_element() {
    assert_clean_asan_run(
        r#"
struct Item { id: i64, name: String }
struct Bag { xs: Vec[Item] }
fn main() {
    let mut b = Bag { xs: Vec.new() };
    b.xs.push(Item { id: 1, name: f"a{1}" });
    b.xs[0] = Item { id: 9, name: f"z{9}" };
    println(f"{b.xs[0].id}");
}
"#,
        &["9"],
        "named-root-index-assign-release",
    );
}

/// B-2026-08-26-18, the ALIASING leg — `self.xs[i] = self.xs[j]`, the
/// shape `PriorityQueue.swap` is built from.
///
/// This is the case that makes the fix's second half mandatory. The alias
/// guard is keyed on the container's NAME and its first act is a
/// `expr_mentions_name_deep` walk that has no `SelfValue` arm, so for a
/// self-rooted container it reports "no mention" for EVERY rhs — including
/// this one — and answers "safe to release" unconditionally. Normalising
/// the receiver without also standing in for that mention test would have
/// released a buffer this rhs still points into: a double free, the one
/// direction the displaced-element family documents as unacceptable.
/// B-2026-08-27-23 — `v[i] = v[j].clone()` must free the element it
/// displaces. It did not: the displaced-drop guard asks whether the RHS
/// could carry the container's heap out, saw the container MENTIONED in
/// `v[j].clone()`, and took the conservative branch — leaking `v[i]`'s old
/// buffer on every such store. A top-level `.clone()` is independent of its
/// receiver by definition, so it can never alias the buffer being freed.
///
/// Neighbouring shapes were already clean and stayed so: `v[0] = mk(9)` (an
/// ordinary call RHS) and `let c = v[0].clone()`. Only the combination
/// leaked, which is why no existing fixture covered it.
#[test]
fn asan_index_store_from_clone_rhs_frees_displaced() {
    assert_clean_asan_run(
        r#"
#[derive(Clone)]
struct It { id: i64, name: String }
fn main() {
    let mut v: Vec[It] = Vec.new();
    v.push(It { id: 1, name: f"aa{1}" });
    v.push(It { id: 2, name: f"bb{2}" });
    v[0] = v[1].clone();
    println(f"{v[0].id}");
}
"#,
        &["2"],
        "index-store-from-clone-rhs-frees-displaced",
    );
}

#[test]
fn asan_self_rooted_index_assign_from_sibling_element_is_not_double_freed() {
    assert_clean_asan_run(
        r#"
#[derive(Clone)]
struct Item { id: i64, name: String }
struct Bag { xs: Vec[Item] }
impl Bag {
    fn go(mut ref self) { self.xs.swap(0, 1); }
}
fn main() {
    let mut b = Bag { xs: Vec.new() };
    b.xs.push(Item { id: 1, name: f"a{1}" });
    b.xs.push(Item { id: 2, name: f"bb{2}" });
    b.go();
    println(f"{b.xs[0].id}");
}
"#,
        &["2"],
        "self-rooted-index-assign-sibling",
    );
}

/// B-2026-08-27-20 — a NESTED index store whose RHS is a NAMED LOCAL
/// double-freed: the store released the displaced element and moved the
/// local's buffer into the slot, then the local's own scope-exit cleanup
/// freed that same buffer.
///
/// The move-suppression that prevents this was gated on the target's object
/// being an `Identifier` or a `FieldAccess`. For `d[0][1] = x` the object is
/// ITSELF an `Index`, so it matched neither and the suppression never ran.
/// A single-level `v[i] = x` was safe only because the `Identifier` arm
/// covers it.
///
/// THE LOOP IS WHAT MAKES THIS TEST WORTH ITS RUNTIME. A suppression fix
/// can fail in two opposite directions, and one run of one store cannot
/// tell them apart: suppress too little and the buffer is freed twice
/// (ASAN), suppress too much and every stored local leaks (LSan, which runs
/// on Linux CI but NOT on macOS). Five iterations each move a fresh local
/// in and displace the previous one, so a leak accumulates to five blocks
/// rather than hiding as one.
///
/// Padded payloads so each allocation is a distinct heap block rather than
/// a small-string optimisation or a folded literal.
#[test]
fn asan_nested_index_store_from_a_named_local_is_balanced() {
    assert_clean_asan_run(
        r#"
fn main() {
    let mut r: Vec[String] = Vec.new();
    r.push(f"a-padding-padding-padding");
    r.push(f"b-padding-padding-padding");
    let mut d: Vec[Vec[String]] = Vec.new();
    d.push(r);
    let mut j = 0;
    while j < 5 {
        let x: String = f"x-{j}-padding-padding-padding";
        d[0][1] = x;
        j = j + 1;
    }
    println(f"{d[0][1]}{d.len()}");
}
"#,
        &["x-4-padding-padding-padding1"],
        "nested-index-store-from-named-local",
    );
}

/// B-2026-09-07-49 — the TUPLE-INDEX twin of the three destinations
/// B-2026-09-07-23 / -30 fixed for `FieldAccess`, pinned CLEAN.
///
/// `uam_defensive_copy_tuple_elem` is one of the three place readers that
/// consult the `UseAfterMove` half of "does this source outlive the move",
/// and it DIVERGES from the RC-fallback half: instrumenting the predicate
/// across the whole `memory_sanitizer` + `codegen` + `par_codegen` corpus
/// found this site answering "not a consume site" for an RC-PROMOTED root
/// (1 occurrence, against 61 at `uam_reclone_source_field` and 45 at
/// `uam_defensive_copy_field`, the two sites -30 and -23 fixed).
///
/// The divergence is real and the shape is nonetheless CLEAN, which is why
/// this is a pin and not a fix. The RC-boxed tuple's own element
/// registration (B-2026-09-07-28) already owns the element, so all three
/// destinations measure clean at 1 and 3 trips. Copying here as well would
/// be the second owner — the 114 B stack B-2026-09-07-30 measured when the
/// RC-boxed copy was routed through a shared disarm.
///
/// Cell 4 is the `FieldAccess` control -30 fixed: it must stay clean, so a
/// failure there means the probe is wrong rather than the tuple path.
#[test]
fn rc_boxed_tuple_index_destinations_stay_clean() {
    const OWN: &str = "fn seed() -> i64 { env.args().len() }\n\
             fn payload() -> String { f\"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa\" }\n\
             fn mkt(n: i64) -> (String, i64) { return (payload(), n); }\n\
             fn main() { println(go()); }\n";
    // 1 — push destination, tuple twin of the -30 `push` cell.
    assert_clean_asan_run_no_auto_par(
            &format!(
                "{OWN}fn go() -> i64 {{ let t = mkt(9); let mut v: Vec[String] = Vec.new(); let mut i = 0i64;\n\
                 \x20 while i < 3i64 {{ v.push(t.0); i = i + 1; }}\n\
                 \x20 return v.len(); }}\n"
            ),
            &["3"],
            "b49_tuple_push_three_trips",
        );
    // 2 — assignment destination, tuple twin of the -30 `assign` cell.
    assert_clean_asan_run_no_auto_par(
        &format!(
            "{OWN}fn go() -> i64 {{ let t = mkt(9); let mut s = String.new(); let mut i = 0i64;\n\
                 \x20 while i < 3i64 {{ s = t.0; i = i + 1; }}\n\
                 \x20 return s.len(); }}\n"
        ),
        &["38"],
        "b49_tuple_assign_three_trips",
    );
    // 3 — `let` destination, tuple twin of the -23 cell.
    assert_clean_asan_run_no_auto_par(
        &format!(
            "{OWN}fn go() -> i64 {{ let t = mkt(9); let mut n = 0i64; let mut i = 0i64;\n\
                 \x20 while i < 3i64 {{ let s = t.0; n = n + s.len(); i = i + 1; }}\n\
                 \x20 return n; }}\n"
        ),
        &["114"],
        "b49_tuple_let_three_trips",
    );
    // 4 — CONTROL: the FieldAccess push cell -30 already fixed. Must stay
    // clean, so a regression here means the probe itself is wrong.
    assert_clean_asan_run_no_auto_par(
            "struct P { a: String, b: i64 }\n\
             fn seed() -> i64 { env.args().len() }\n\
             fn payload() -> String { f\"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa\" }\n\
             fn mkp(n: i64) -> P { return P { a: payload(), b: n }; }\n\
             fn go() -> i64 { let t = mkp(9); let mut v: Vec[String] = Vec.new(); let mut i = 0i64;\n\
             \x20 while i < 3i64 { v.push(t.a); i = i + 1; }\n\
             \x20 return v.len(); }\n\
             fn main() { println(go()); }\n",
            &["3"],
            "b49_control_field_push",
        );
}

/// B-2026-09-15-31 — an index-assign over a container of TUPLES reclaims
/// the displaced tuple's heap elements, on the `Array` AND `Vec` legs.
/// 10 B in 1 block at `-O0` on each before the fix.
///
/// The `Vec` cell is not a control here, it is half the bug: unlike
/// B-2026-09-14-29, both legs lost this, because both reach the same
/// emitter and it returned at its `TypeKind::Path` shape test — a tuple
/// has no name for the `struct_types` lookup that follows.
///
/// THE `Drop`-BEARING CELL PINNED A DELIBERATE NON-CHANGE, AND
/// B-2026-09-16-2 MOVED IT. `(D, i64)` with an `impl Drop for D` printed
/// NO `dD1` for the displaced element on every surface, which was
/// correct-for-now rather than a miss: the interpreter did not run it
/// either (`value_runs_user_drop` classifies a bare Tuple/Array value as
/// false at top level on purpose), so it was an AGREED gap, and running the
/// body on the compiled side alone would have made it a divergence.
///
/// -16-2 moved BOTH backends in one commit, so the cell now asserts `dD1`.
/// The licence was in the invariant's own wording: it keeps the container
/// walkers the sole firers FOR DIRECT BINDINGS, and a displacement is not
/// one — the slot is overwritten, so no scope-exit walk ever visits the old
/// value and there is no later firer to be sole. design.md line 866 puts
/// the body at the value's live-range end, which here is the store.
///
/// What this cell asserts now is that the body moved WITHOUT the memory
/// moving: ASAN stays clean on it (it did before and does after), so the
/// two channels are still separate (B-2026-08-28-57). A `dD1` printed
/// TWICE here would mean the fix had started double-firing on relocation,
/// which is B-2026-08-26-21's hazard; `v.swap(i, j)` is measured at exactly
/// two bodies for two values.
///
/// MUST BE READ AT `-O0`, and this is not boilerplate: MEASURED on the
/// unfixed tree, these cells PASS at the default `-O2` (LLVM deletes an
/// orphan nothing observes) and FAIL at `-O0` with LeakSanitizer
/// reporting 13 bytes. So a green `cargo test --features llvm` proves
/// nothing here — `scripts/asan-o0-leg.sh` is the gate that holds them.
#[test]
fn asan_index_store_frees_the_displaced_tuple_element() {
    // The `Array` leg.
    assert_clean_asan_run(
            "fn main() {\n\
             \x20   let mut a: Array[(String, i64), 2] = [(f\"aaaaaaaaaaaa1\", 1), (f\"bbbbbbbbbbbb2\", 2)];\n\
             \x20   a[0] = (f\"MUTATEDMUTATED3\", 3);\n\
             \x20   println(f\"a0:{a[0].1}\");\n\
             }\n",
            &["a0:3"],
            "b31-tuple-elem-array-leg",
        );
    // The `Vec` leg — the same 10 B, which is what says this was never an
    // Array-vs-Vec asymmetry.
    assert_clean_asan_run(
            "fn main() {\n\
             \x20   let mut a: Vec[(String, i64)] = [(f\"aaaaaaaaaaaa1\", 1), (f\"bbbbbbbbbbbb2\", 2)];\n\
             \x20   a[0] = (f\"MUTATEDMUTATED3\", 3);\n\
             \x20   println(f\"a0:{a[0].1}\");\n\
             }\n",
            &["a0:3"],
            "b31-tuple-elem-vec-leg",
        );
    // A NESTED tuple — the inner tuple's heap is reached too.
    assert_clean_asan_run(
            "fn main() {\n\
             \x20   let mut a: Array[((String, i64), i64), 2] = [((f\"aaaaaaaaaaaa1\", 1), 1), ((f\"bbbbbbbbbbbb2\", 2), 2)];\n\
             \x20   a[0] = ((f\"MUTATEDMUTATED3\", 3), 3);\n\
             \x20   println(f\"a0:{a[0].1}\");\n\
             }\n",
            &["a0:3"],
            "b31-nested-tuple-elem",
        );
    // A tuple carrying a `Drop`-bearing struct: memory reclaimed AND, since
    // B-2026-09-16-2, the displaced element's body run — on both backends
    // together (see the doc comment).
    assert_clean_asan_run(
            "struct D { s: String, id: i64 }\n\
             impl Drop for D { fn drop(mut ref self) { println(f\"dD{self.id}\") } }\n\
             fn main() {\n\
             \x20   let mut a: Array[(D, i64), 2] = [(D { s: f\"aaaaaaaaaaaa1\", id: 1 }, 1), (D { s: f\"bbbbbbbbbbbb2\", id: 2 }, 2)];\n\
             \x20   a[0] = (D { s: f\"MUTATEDMUTATED3\", id: 3 }, 3);\n\
             \x20   println(\"mid\");\n\
             }\n",
            &["dD1", "dD3", "dD2", "mid"],
            "b31-tuple-elem-with-drop-body",
        );
    // THE UNBOUNDED CASE — three trips, so a per-store leak compounds.
    assert_clean_asan_run(
            "fn main() {\n\
             \x20   let mut a: Array[(String, i64), 2] = [(f\"aaaaaaaaaaaa0\", 0), (f\"bbbbbbbbbbbb0\", 0)];\n\
             \x20   let mut i = 1;\n\
             \x20   while i < 4 {\n\
             \x20       a[0] = (f\"MUTATEDMUTATED{i}\", i);\n\
             \x20       i = i + 1;\n\
             \x20   }\n\
             \x20   println(\"end\");\n\
             }\n",
            &["end"],
            "b31-tuple-elem-loop",
        );
}

/// B-2026-09-15-32 — an index-assign over a container whose element is a
/// NESTED ARRAY reclaims that inner array's elements' heap. 10 B in 1
/// block at `-O0` before the fix.
///
/// MEMORY ONLY, and the row is explicit about why: the displaced inner
/// array's element `Drop` bodies are missing on BOTH backends, so closing
/// only the compiled half would manufacture a divergence. These cells
/// assert the heap is reclaimed while the body sequences stay exactly as
/// they were — `dD3 dD2`, never `dD1`.
///
/// The mixed spellings matter because the outer container kind and the
/// inner one are separate lookups: `Vec[Array[..]]` reaches the emitter
/// through the vec element table and `Array[Vec[..]]` through the array
/// one, so a fix keyed on only one of them passes half of this.
///
/// MUST BE READ AT `-O0`, and this is not boilerplate: MEASURED on the
/// unfixed tree, these cells PASS at the default `-O2` (LLVM deletes an
/// orphan nothing observes) and FAIL at `-O0` with LeakSanitizer
/// reporting 13 bytes. So a green `cargo test --features llvm` proves
/// nothing here — `scripts/asan-o0-leg.sh` is the gate that holds them.
#[test]
fn asan_index_store_frees_the_displaced_nested_array_element() {
    const D: &str = "struct D { s: String, id: i64 }\n";
    // The row's own repro.
    assert_clean_asan_run(
            &format!(
                "{D}fn main() {{\n\
                 \x20   let mut a: Array[Array[D, 1], 2] = [[D {{ s: f\"aaaaaaaaaaaa1\", id: 1 }}], [D {{ s: f\"bbbbbbbbbbbb2\", id: 2 }}]];\n\
                 \x20   a[0] = [D {{ s: f\"MUTATEDMUTATED3\", id: 3 }}];\n\
                 \x20   println(\"mid\");\n\
                 }}\n"
            ),
            &["mid"],
            "b32-nested-array-elem",
        );
    // `Vec` outer, `Array` inner.
    assert_clean_asan_run(
            &format!(
                "{D}fn main() {{\n\
                 \x20   let mut a: Vec[Array[D, 1]] = [[D {{ s: f\"aaaaaaaaaaaa1\", id: 1 }}], [D {{ s: f\"bbbbbbbbbbbb2\", id: 2 }}]];\n\
                 \x20   a[0] = [D {{ s: f\"MUTATEDMUTATED3\", id: 3 }}];\n\
                 \x20   println(\"mid\");\n\
                 }}\n"
            ),
            &["mid"],
            "b32-vec-outer-array-inner",
        );
    // DEPTH 3 — the synthesizer is recursive, so one call covers each level.
    assert_clean_asan_run(
            &format!(
                "{D}fn main() {{\n\
                 \x20   let mut a: Array[Array[Array[D, 1], 1], 2] = [[[D {{ s: f\"aaaaaaaaaaaa1\", id: 1 }}]], [[D {{ s: f\"bbbbbbbbbbbb2\", id: 2 }}]]];\n\
                 \x20   a[0] = [[D {{ s: f\"MUTATEDMUTATED3\", id: 3 }}]];\n\
                 \x20   println(\"mid\");\n\
                 }}\n"
            ),
            &["mid"],
            "b32-nested-array-depth-three",
        );
    // THE UNBOUNDED CASE.
    assert_clean_asan_run(
            &format!(
                "{D}fn main() {{\n\
                 \x20   let mut a: Array[Array[D, 1], 2] = [[D {{ s: f\"aaaaaaaaaaaa0\", id: 0 }}], [D {{ s: f\"bbbbbbbbbbbb0\", id: 0 }}]];\n\
                 \x20   let mut i = 1;\n\
                 \x20   while i < 4 {{\n\
                 \x20       a[0] = [D {{ s: f\"MUTATEDMUTATED{{i}}\", id: i }}];\n\
                 \x20       i = i + 1;\n\
                 \x20   }}\n\
                 \x20   println(\"end\");\n\
                 }}\n"
            ),
            &["end"],
            "b32-nested-array-loop",
        );
}

/// B-2026-09-14-29 — an index-assign over an `Array[T, N]` of user
/// structs reclaims the displaced element's heap fields. 10 B in 1 block
/// at `-O0` before the fix.
///
/// The no-`Drop` `E` cell is the one that isolates the MEMORY half: it
/// has a heap field and no body to run, so a fix that only restored the
/// `Drop` bodies leaves it red here while the codegen output test passes.
/// Its sibling — a body with no heap — lives in the codegen suite for the
/// mirror-image reason.
#[test]
fn asan_array_index_store_frees_the_displaced_struct_element() {
    // Heap field + `Drop` body: the row's own repro.
    assert_clean_asan_run(
            "struct D { s: String, id: i64 }\n\
             impl Drop for D { fn drop(mut ref self) { println(f\"dD{self.id}\") } }\n\
             fn main() {\n\
             \x20   let mut a: Array[D, 2] = [D { s: f\"aaaaaaaaaaaa1\", id: 1 }, D { s: f\"bbbbbbbbbbbb2\", id: 2 }];\n\
             \x20   a[0] = D { s: f\"MUTATEDMUTATED3\", id: 3 };\n\
             \x20   println(f\"a0:{a[0].id}\");\n\
             }\n",
            &["dD1", "a0:3", "dD3", "dD2"],
            "b29-array-index-store-struct-elem",
        );
    // A heap field and NO `Drop` body — the memory half with no body to
    // mask it.
    assert_clean_asan_run(
            "struct E { s: String, id: i64 }\n\
             fn main() {\n\
             \x20   let mut a: Array[E, 2] = [E { s: f\"aaaaaaaaaaaa1\", id: 1 }, E { s: f\"bbbbbbbbbbbb2\", id: 2 }];\n\
             \x20   a[0] = E { s: f\"MUTATEDMUTATED3\", id: 3 };\n\
             \x20   println(f\"a0:{a[0].id}\");\n\
             }\n",
            &["a0:3"],
            "b29-array-index-store-no-drop-elem",
        );
    // CONTROL — the `Vec[D]` twin, clean before and after. It is the
    // ORACLE this fix was derived from, so a regression that breaks the
    // two together shows up here rather than looking Array-specific.
    assert_clean_asan_run(
            "struct D { s: String, id: i64 }\n\
             impl Drop for D { fn drop(mut ref self) { println(f\"dD{self.id}\") } }\n\
             fn main() {\n\
             \x20   let mut a: Vec[D] = [D { s: f\"aaaaaaaaaaaa1\", id: 1 }, D { s: f\"bbbbbbbbbbbb2\", id: 2 }];\n\
             \x20   a[0] = D { s: f\"MUTATEDMUTATED3\", id: 3 };\n\
             \x20   println(f\"a0:{a[0].id}\");\n\
             }\n",
            &["dD1", "a0:3", "dD3", "dD2"],
            "b29-vec-index-store-oracle",
        );
}

/// B-2026-09-15-7 — an index store over a container whose element is
/// itself a heap-bearing `Vec` reclaims that element's OWN elements, not
/// just its outer buffer. 5 B in 1 block at `-O0` before the fix, on the
/// `Array` leg and on the `Vec[Vec[String]]` twin alike.
///
/// THE ALIAS CELLS ARE THE POINT OF THIS FIXTURE, not the leak cells. The
/// outer-buffer-only bound existed to avoid double-freeing a live
/// per-element alias, so the deep walk is exactly the change that guard
/// forbade; these three spellings are every way an alias can be obtained
/// today, and each must stay single-free. `let r = a[0]` — the fourth —
/// is rejected by the typechecker (`E_INDEX_MOVE_NON_COPY`) and so cannot
/// be written as a cell at all.
#[test]
fn asan_index_store_frees_the_displaced_vec_elements_own_elements() {
    const MK: &str = "fn main() {\n\
             \x20   let mut v1: Vec[String] = Vec.new();\n\
             \x20   v1.push(f\"one-payload-one\");\n\
             \x20   let mut v2: Vec[String] = Vec.new();\n\
             \x20   v2.push(f\"two-payload-two\");\n";
    // The `Array` leg.
    assert_clean_asan_run(
        &format!(
            "{MK}\x20   let mut a: Array[Vec[String], 2] = [v1, v2];\n\
                 \x20   a[0] = Vec.new();\n\
                 \x20   println(f\"a0:{{a[0].len()}}\");\n\
                 }}\n"
        ),
        &["a0:0"],
        "b7-array-elem-inner-elements",
    );
    // The `Vec[Vec[String]]` TWIN — the cell that says this was never
    // Array-specific, and the reason the fix went in the shared helper.
    assert_clean_asan_run(
        &format!(
            "{MK}\x20   let mut a: Vec[Vec[String]] = [v1, v2];\n\
                 \x20   let nv: Vec[String] = Vec.new();\n\
                 \x20   a[0] = nv;\n\
                 \x20   println(f\"a0:{{a[0].len()}}\");\n\
                 }}\n"
        ),
        &["a0:0"],
        "b7-vec-elem-inner-elements-twin",
    );
    // Depth 3: the recursive drop family walks every level, so one level
    // of deepening is not a special case.
    assert_clean_asan_run(
        "fn main() {\n\
             \x20   let mut inner: Vec[String] = Vec.new();\n\
             \x20   inner.push(f\"deep-payload-deep\");\n\
             \x20   let mut mid: Vec[Vec[String]] = Vec.new();\n\
             \x20   mid.push(inner);\n\
             \x20   let mut a: Array[Vec[Vec[String]], 2] = [mid, Vec.new()];\n\
             \x20   a[0] = Vec.new();\n\
             \x20   println(f\"a0:{a[0].len()}\");\n\
             }\n",
        &["a0:0"],
        "b7-depth-three",
    );
    // CONTROL — no inner heap, so the deep path must decline and leave
    // the buffer-only free byte-for-byte.
    assert_clean_asan_run(
        "fn main() {\n\
             \x20   let mut v1: Vec[i64] = Vec.new();\n\
             \x20   v1.push(1);\n\
             \x20   let mut a: Array[Vec[i64], 2] = [v1, Vec.new()];\n\
             \x20   a[0] = Vec.new();\n\
             \x20   println(f\"a0:{a[0].len()}\");\n\
             }\n",
        &["a0:0"],
        "b7-no-inner-heap-control",
    );
    // ALIAS 1 — a `ref` borrow of the element, read before the store.
    assert_clean_asan_run(
        &format!(
            "{MK}\x20   let mut a: Array[Vec[String], 2] = [v1, v2];\n\
                 \x20   let r = ref a[0];\n\
                 \x20   println(f\"r:{{r.len()}}\");\n\
                 \x20   a[0] = Vec.new();\n\
                 \x20   println(f\"a0:{{a[0].len()}}\");\n\
                 }}\n"
        ),
        &["r:1", "a0:0"],
        "b7-alias-ref-borrow",
    );
    // ALIAS 2 — an independent `clone`, which must SURVIVE the deep walk.
    // `c:1` is the assertion that matters: a walk that reached the
    // clone's buffer would show as a double free here.
    assert_clean_asan_run(
        &format!(
            "{MK}\x20   let mut a: Array[Vec[String], 2] = [v1, v2];\n\
                 \x20   let c = a[0].clone();\n\
                 \x20   a[0] = Vec.new();\n\
                 \x20   println(f\"c:{{c.len()}} a0:{{a[0].len()}}\");\n\
                 }}\n"
        ),
        &["c:1 a0:0"],
        "b7-alias-clone-survives",
    );
    // ALIAS 3 — a `for` binding over the container before the store.
    assert_clean_asan_run(
        &format!(
            "{MK}\x20   let mut a: Array[Vec[String], 2] = [v1, v2];\n\
                 \x20   let mut tot = 0;\n\
                 \x20   for e in a {{\n\
                 \x20       tot = tot + 1;\n\
                 \x20   }}\n\
                 \x20   a[0] = Vec.new();\n\
                 \x20   println(f\"tot:{{tot}} a0:{{a[0].len()}}\");\n\
                 }}\n"
        ),
        &["tot:2 a0:0"],
        "b7-alias-for-binding",
    );
}

/// B-2026-09-16-3: `d[i][j] = x` over a NESTED container leaked the
/// displaced element's heap -- 5 B in 1 block at `-O0` on the filed cell.
/// `emit_displaced_index_elem_drop` destructured its object as an
/// `Identifier` and returned on anything else, so the inner store had no
/// container to release from; the single-level `a[i] = x` spelling was
/// always correct, which is what the two controls below pin.
///
/// The fix releases the displaced HEAP only. The displaced element's user
/// `Drop` BODY still does not run at a nested store -- an AGREED FAULT on
/// all four surfaces, so no A/B sees it -- and that half is a separate row:
/// running it here on the compiled surfaces alone turned a silent leak into
/// a run-vs-build divergence, which is strictly worse. The struct cells
/// below therefore expect the ONE body they get today; when the bodies half
/// lands, they gain the displaced element's body and these expectations
/// move with it.
#[test]
fn asan_nested_index_store_releases_the_displaced_element() {
    // The filed cell: a tuple element under `Vec[Vec[_]]`. Every printed
    // line reads through the stored payload, so a value destroyed at the
    // store cannot pass as a value merely leaked.
    assert_clean_asan_run(
        "fn main() {
                 let n = 7;
                 let mut d: Vec[Vec[(String, i64)]] = [[(f\"one-aaaaaaaaaaaa-{n}\", 1)]];
                 d[0][0] = (f\"replaced-bbbbbbbbbbbb-{n}\", 2);
                 println(f\"r:{d[0][0].1}:{d[0][0].0.len()}\");
             }
",
        &["r:2:23"],
        "b2026-09-16-3-nested-vec-tuple",
    );

    // The outer container is an `Array`, which reaches the element pointer
    // through a different lowering than the `Vec` above.
    assert_clean_asan_run(
        "fn main() {
                 let n = 7;
                 let mut d: Array[Vec[(String, i64)], 1] = [[(f\"one-aaaaaaaaaaaa-{n}\", 1)]];
                 d[0][0] = (f\"replaced-bbbbbbbbbbbb-{n}\", 2);
                 println(f\"r:{d[0][0].1}:{d[0][0].0.len()}\");
             }
",
        &["r:2:23"],
        "b2026-09-16-3-nested-array-outer",
    );

    // A named struct element, and an `Array` INNER container -- the two
    // element shapes whose displaced buffers the `Identifier`-only
    // destructure also stranded. The single body each prints is the
    // survivor's at scope exit; see the note above.
    assert_clean_asan_run(
        "struct S { s: String, k: i64 }
             impl Drop for S { fn drop(mut ref self) { println(f\"dS{self.k}:{self.s.len()}\") } }
             fn main() {
                 let n = 7;
                 let mut d: Vec[Vec[S]] = [[S { s: f\"one-aaaaaaaaaaaa-{n}\", k: 1 }]];
                 d[0][0] = S { s: f\"replaced-bbbbbbbbbbbb-{n}\", k: 2 };
                 println(\"end\");
             }
",
        &["dS2:23", "end"],
        "b2026-09-16-3-nested-struct-elem",
    );

    assert_clean_asan_run(
        "struct D { s: String }
             impl Drop for D { fn drop(mut ref self) { println(f\"dD{self.s.len()}\") } }
             fn main() {
                 let n = 7;
                 let mut d: Vec[Vec[Array[D, 1]]] = [[[D { s: f\"one-aaaaaaaaaaaa-{n}\" }]]];
                 d[0][0] = [D { s: f\"replaced-bbbbbbbbbbbb-{n}\" }];
                 println(\"end\");
             }
",
        &["dD23", "end"],
        "b2026-09-16-3-nested-array-inner",
    );

    // CONTROLS, both clean BEFORE the fix -- so a run in which they are the
    // only cells asserts nothing about it. The first is the single-level
    // store the new arm must not reach twice (a second release here is a
    // double free, not a leak); the second is a nested store whose element
    // carries no heap at all, pinning that the arm stands down rather than
    // releasing a scalar.
    assert_clean_asan_run(
        "struct S { s: String, k: i64 }
             fn main() {
                 let n = 7;
                 let mut a: Vec[S] = [S { s: f\"one-aaaaaaaaaaaa-{n}\", k: 1 }];
                 a[0] = S { s: f\"replaced-bbbbbbbbbbbb-{n}\", k: 2 };
                 println(f\"r:{a[0].k}:{a[0].s.len()}\");
             }
",
        &["r:2:23"],
        "b2026-09-16-3-single-level-control",
    );

    assert_clean_asan_run(
        "fn main() {
                 let n = 7;
                 let mut d: Vec[Vec[Vec[i64]]] = [[[1, n]]];
                 d[0][0] = [2, 3, 4];
                 println(f\"r:{d[0].len()}:{d[0][0][2]}\");
             }
",
        &["r:1:4"],
        "b2026-09-16-3-scalar-nested-control",
    );

    // The THIRD control, and the one a too-wide arm turns into a double
    // free rather than leaving a leak: a BARE `String` element at the same
    // nested store. Measured clean at `-O0` BOTH before and after this fix
    // -- 13/13 and 12/12 allocs/frees, zero errors -- so something already
    // owns that buffer and this pins that the new arm did not become a
    // second owner. B-2026-08-10-1 fixed this exact spelling; WHICH site
    // owns it is deliberately not claimed here, because the obvious guess
    // is refuted -- `emit_displaced_vec_elem_release` is gated to
    // `Vec`/`VecDeque` element heads precisely so a `String` element, also
    // a {ptr,len,cap} slot, does NOT reach the dispatcher behind it.
    assert_clean_asan_run(
        "fn main() {
                 let n = 7;
                 let mut d: Vec[Vec[String]] = [[f\"one-aaaaaaaaaaaa-{n}\"]];
                 d[0][0] = f\"replaced-bbbbbbbbbbbb-{n}\";
                 println(f\"r:{d[0][0]}\");
             }
",
        &["r:replaced-bbbbbbbbbbbb-7"],
        "b2026-09-16-3-bare-string-control",
    );

    assert_clean_asan_run(
        "fn main() {
                 let n = 7;
                 let mut d: Array[Vec[String], 1] = [[f\"one-aaaaaaaaaaaa-{n}\"]];
                 d[0][0] = f\"replaced-bbbbbbbbbbbb-{n}\";
                 println(f\"r:{d[0][0]}\");
             }
",
        &["r:replaced-bbbbbbbbbbbb-7"],
        "b2026-09-16-3-bare-string-array-control",
    );
}

/// B-2026-09-20-29: a SINGLE-LEVEL index store whose RHS mentions its own
/// container leaked the displaced element's heap -- 18 B in 1 block at
/// `-O0` on every test cell below but one, and 34 B in 2 blocks on that one
/// -- whenever the mention was spelled in a way
/// `expr_cannot_carry_container_heap` had no arm for.
///
/// That predicate decides whether the RHS could carry a pointer into the
/// element about to be released; a `false` answer stands the release down.
/// It ended in `_ => false`, so an unrecognised spelling read as "might
/// carry heap" and the displaced element was simply never freed. The
/// conservative direction is a silent leak, which is why the gap outlived
/// three rows' worth of cells over the same predicate.
///
/// TWO faults, not one. The tail is the first. The second is that the arms
/// which did exist were keyed on SYNTAX: the element-read arm destructured
/// `ExprKind::FieldAccess` and resolved the field inline, so it handled one
/// spelling and no composition -- `a[0].k` cleared, `a[0].t.1` declined,
/// both copying one `i64` out of the same element. The fix resolves the
/// CHAIN (`a[i]`, `.f`, `.N` in any composition) down to a declared type
/// and asks that type, so a new spelling is an arm in a resolver rather
/// than another arm here.
///
/// Every printed line reads the STORED payload back -- its contents, or a
/// scalar and a string length together -- so a value destroyed at the store
/// cannot pass as a value merely leaked.
#[test]
fn asan_index_store_rhs_spelling_releases_the_displaced_element() {
    // A TUPLE literal holding a tuple-index read of the element it
    // replaces. Needs BOTH halves of the fix: an arm for the literal, and
    // a resolver that can answer for `a[0].1`. 14 allocs / 13 frees before.
    assert_clean_asan_run(
        "fn main() {
                 let n = 7;
                 let mut a: Vec[(String, i64)] = [(f\"one-aaaaaaaaaaaa-{n}\", 1)];
                 a[0] = (f\"replaced-bbbbbbbbbbbb-{n}\", a[0].1 + 1);
                 println(f\"r:{a[0].1}:{a[0].0.len()}\");
             }
",
        &["r:2:23"],
        "b2026-09-20-29-tuple-literal",
    );

    // A STRUCT literal holding a named-field read. The field arm already
    // cleared this expression as a whole RHS; one level down inside the
    // literal it reached the tail. 13 / 12 before.
    assert_clean_asan_run(
        "struct S { s: String, k: i64 }
             fn main() {
                 let n = 7;
                 let mut a: Vec[S] = [S { s: f\"one-aaaaaaaaaaaa-{n}\", k: 1 }];
                 a[0] = S { s: f\"replaced-bbbbbbbbbbbb-{n}\", k: a[0].k + 1 };
                 println(f\"r:{a[0].k}:{a[0].s.len()}\");
             }
",
        &["r:2:23"],
        "b2026-09-20-29-struct-literal",
    );

    // The COMPOSITION the old inline resolution could not do: an index, a
    // named field, and a tuple index in one chain, nested two literals
    // deep. 13 / 12 before.
    assert_clean_asan_run(
        "struct S { s: String, t: (i64, i64) }
             fn main() {
                 let n = 7;
                 let mut a: Vec[S] = [S { s: f\"one-aaaaaaaaaaaa-{n}\", t: (1, 1) }];
                 a[0] = S { s: f\"replaced-bbbbbbbbbbbb-{n}\", t: (a[0].t.1 + 1, 9) };
                 println(f\"r:{a[0].t.0}:{a[0].s.len()}\");
             }
",
        &["r:2:23"],
        "b2026-09-20-29-tupleindex-composition",
    );

    // The tuple-index spelling with NO literal in the picture at all -- the
    // RHS is a call, so the `Call` arm's per-argument recursion is what
    // reaches the read. This is the cell that isolates the second fault
    // from the first: `mk(a[0].k)` was clean and `mk(a[0].t.1)` leaked,
    // same function, same element, same `i64` word. 13 / 12 before.
    assert_clean_asan_run(
        "struct S { s: String, k: i64, t: (i64, i64) }
             fn mk(n: i64) -> S { return S { s: f\"replaced-bbbbbbbbbbbb-{n}\", k: n, t: (n, n) }; }
             fn main() {
                 let n = 7;
                 let mut a: Vec[S] = [S { s: f\"one-aaaaaaaaaaaa-{n}\", k: 1, t: (1, 1) }];
                 a[0] = mk(a[0].t.1);
                 println(f\"r:{a[0].k}:{a[0].s.len()}\");
             }
",
        &["r:1:23"],
        "b2026-09-20-29-tupleindex-call-arg",
    );

    // AN INTERPOLATED STRING, and the adversarial form of it: a BARE
    // interpolation of the very buffer being released, with no surrounding
    // literal text for a single-part fast path to hide behind. An f-string
    // allocates its own buffer, so it cannot carry the container's heap --
    // but allowing it also asserts the release is ordered AFTER the RHS,
    // and getting that wrong turns a leak into a use-after-free. This cell
    // is what proves the ordering: 14 / 13 with 18 B lost before, 14 / 14
    // and valgrind ERROR SUMMARY 0 after. A cell with surrounding text
    // passes either way.
    assert_clean_asan_run(
        "struct S { s: String, k: i64 }
             fn main() {
                 let n = 7;
                 let mut a: Vec[S] = [S { s: f\"one-aaaaaaaaaaaa-{n}\", k: 1 }];
                 a[0] = S { s: f\"{a[0].s}\", k: 2 };
                 println(f\"r:{a[0].k}:{a[0].s.len()}\");
             }
",
        &["r:2:18"],
        "b2026-09-20-29-fstring-bare-interpolation",
    );

    // An ARRAY literal whose one component is heap-bearing, mentioning the
    // container through `a.len()` inside an f-string. 13 / 12 before.
    assert_clean_asan_run(
        "fn main() {
                 let n = 7;
                 let mut a: Vec[Array[String, 1]] = [[f\"one-aaaaaaaaaaaa-{n}\"]];
                 a[0] = [f\"replaced-bbbbbbbbbbbb-{a.len()}\"];
                 println(f\"r:{a[0][0]}\");
             }
",
        &["r:replaced-bbbbbbbbbbbb-1"],
        "b2026-09-20-29-array-literal",
    );

    // The cell that named the site when the row was filed: the SAME
    // `.clone()` the arm below clears as a whole RHS, one level down inside
    // a struct literal. Nothing about the clone changed -- only whether the
    // walk got there. 13 / 12 before.
    assert_clean_asan_run(
        "struct S { s: String, k: i64 }
             fn main() {
                 let n = 7;
                 let mut a: Vec[S] = [S { s: f\"one-aaaaaaaaaaaa-{n}\", k: 1 }];
                 a[0] = S { s: a[0].s.clone(), k: 2 };
                 println(f\"r:{a[0].k}:{a[0].s.len()}\");
             }
",
        &["r:2:18"],
        "b2026-09-20-29-clone-one-level-down",
    );

    // A `Vec`-typed heap field rather than a `String` one, so the leaked
    // block is not the one shape every other cell here shares: 15 allocs /
    // 13 frees with 34 B in 2 blocks before, 15 / 15 after. Two blocks
    // because the displaced element carried two separate buffers.
    assert_clean_asan_run(
        "struct S { s: String, v: Vec[i64] }
             fn main() {
                 let n = 7;
                 let mut a: Vec[S] = [S { s: f\"one-aaaaaaaaaaaa-{n}\", v: [1, 2] }];
                 a[0] = S { s: f\"replaced-bbbbbbbbbbbb-{n}\", v: a[0].v.clone() };
                 println(f\"r:{a[0].v.len()}:{a[0].s.len()}\");
             }
",
        &["r:2:23"],
        "b2026-09-20-29-vec-field-clone",
    );

    // CONTROLS, all three clean BEFORE the fix -- so a run in which they
    // are the only cells asserts nothing about it. They are here because
    // every widening of a leak guard is a double-free candidate, and
    // nothing above would notice one.
    //
    // A top-level `.clone()`, already cleared by its own arm: the release
    // must not now happen twice.
    assert_clean_asan_run(
        "fn main() {
                 let n = 7;
                 let mut a: Vec[(String, i64)] = [(f\"one-aaaaaaaaaaaa-{n}\", 1)];
                 a[0] = a[0].clone();
                 println(f\"r:{a[0].1}:{a[0].0.len()}\");
             }
",
        &["r:1:18"],
        "b2026-09-20-29-ctl-toplevel-clone",
    );

    // An RHS that never mentions the container, which the predicate clears
    // before it reaches any arm.
    assert_clean_asan_run(
        "struct S { s: String, k: i64 }
             fn main() {
                 let n = 7;
                 let mut a: Vec[S] = [S { s: f\"one-aaaaaaaaaaaa-{n}\", k: 1 }];
                 a[0] = S { s: f\"x-{n}\", k: 2 };
                 println(f\"r:{a[0].k}:{a[0].s.len()}\");
             }
",
        &["r:2:3"],
        "b2026-09-20-29-ctl-no-mention",
    );

    // A `Vec`-typed array literal, which lowers to `PrefixCollectionLiteral`
    // rather than `ArrayLiteral` and has an owner by another route: it is
    // clean with the same mention that leaks through the `Array` spelling
    // above. Deliberately NOT given an arm -- one would change nothing
    // observable while widening the free's reach -- and this cell is what
    // would notice if a later widening reached it.
    assert_clean_asan_run(
        "fn main() {
                 let n = 7;
                 let mut a: Vec[Vec[String]] = [[f\"one-aaaaaaaaaaaa-{n}\"]];
                 a[0] = [f\"replaced-bbbbbbbbbbbb-{a.len()}\"];
                 println(f\"r:{a[0][0]}\");
             }
",
        &["r:replaced-bbbbbbbbbbbb-1"],
        "b2026-09-20-29-ctl-prefix-collection",
    );
}

/// B-2026-09-15-33 — the MEMORY half of the identifier-RHS widening.
///
/// `store_destroys_displaced` gates `run_bodies`, NOT the release: the row
/// measured "All heap blocks were freed" on every one of its cells while
/// the user body was missing. So widening it adds a `Drop` body to a
/// displacement whose heap was ALREADY being released, which is precisely
/// the double-free shape a body count cannot see — an extra free is
/// memory-dirty and body-clean, and the codegen fixture over these same
/// programs would stay green through it.
///
/// No MUST-STAY-DECLINED cell exists to pair with these: the relocation
/// shape the predicate was guarding (B-2026-08-26-21's three-line swap)
/// does not typecheck for any element type whose body is observable, since
/// an `impl Drop` makes a type non-`Copy` and `E_INDEX_MOVE_NON_COPY`
/// rejects the index reads. This leg and the exact-once body count in
/// `tests/codegen.rs` are what stand in for one.
///
/// Each cell's `String` is a DISTINCT LENGTH, so a leaked or double-freed
/// byte count names the cell rather than the family.
#[test]
fn asan_index_store_identifier_rhs_body_does_not_double_free() {
    // The row's own cell: a bare identifier RHS over a `Vec`.
    assert_clean_asan_run(
        "struct D { id: i64, s: String }
             impl Drop for D { fn drop(mut ref self) { println(f\"d{self.id}\") } }
             fn main() {
                 let mut a: Vec[D] = [D { id: 11, s: f\"aaaaaaaaaaaaaaaaa-11\" }];
                 let t: D = D { id: 12, s: f\"bbbbbbbbbbbbbbbbbbbbb-12\" };
                 a[0] = t;
                 println(f\"one:{a[0].id}:{a[0].s.len()}\");
             }
",
        &["d11", "one:12:24", "d12"],
        "b2026-09-15-33-vec-identifier",
    );

    // The `Array` leg, which the row measured diverging identically. The
    // survivor at index 1 is what makes a whole-container over-release
    // visible as well as a slot-local one.
    assert_clean_asan_run(
            "struct D { id: i64, s: String }
             impl Drop for D { fn drop(mut ref self) { println(f\"d{self.id}\") } }
             fn main() {
                 let mut a: Array[D, 2] = [D { id: 21, s: f\"aaaaaaaaaaaaaaaaaaaaaaaaa-21\" }, D { id: 22, s: f\"bb-22\" }];
                 let t: D = D { id: 23, s: f\"ccccccccccccccccccccccccccccc-23\" };
                 a[0] = t;
                 println(f\"two:{a[0].id}:{a[0].s.len()}\");
             }
",
            &["d21", "two:23:32", "d23", "d22"],
            "b2026-09-15-33-array-identifier",
        );

    // A FIELD root and a `mut ref` PARAM root in one program — two of the
    // three spellings the row listed as NOT MEASURED. Both reach the same
    // store, so a root-keyed over-release would show here and not above.
    assert_clean_asan_run(
            "struct D { id: i64, s: String }
             impl Drop for D { fn drop(mut ref self) { println(f\"d{self.id}\") } }
             struct Holder { xs: Vec[D] }
             fn thru(a: mut ref Vec[D]) { let t: D = D { id: 42, s: f\"dddddddddddddddddddddddddddddddddd-42\" }; a[0] = t; println(f\"four:{a[0].s.len()}\"); }
             fn main() {
                 let mut h: Holder = Holder { xs: [D { id: 31, s: f\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaa-31\" }] };
                 let t: D = D { id: 32, s: f\"ccccccccccccccccccccccccccccccc-32\" };
                 h.xs[0] = t;
                 println(f\"three:{h.xs[0].s.len()}\");
                 let mut a: Vec[D] = [D { id: 41, s: f\"eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee-41\" }];
                 thru(mut a);
             }
",
            &["d31", "three:34", "d32", "d41", "four:37", "d42"],
            "b2026-09-15-33-field-and-mutref-roots",
        );

    // A store rooted at a field of `mut ref self`, the spelling the row
    // never asked about. It is the one cell whose fixed answer `--interp`
    // does not share, so the codegen fixture is the only other witness and
    // this is its memory half.
    assert_clean_asan_run(
            "struct D { id: i64, s: String }
             impl Drop for D { fn drop(mut ref self) { println(f\"d{self.id}\") } }
             struct Bag { xs: Vec[D] }
             impl Bag { fn put(mut ref self, t: D) { self.xs[0] = t; } }
             fn main() {
                 let mut b: Bag = Bag { xs: [D { id: 81, s: f\"ffffffffffffffffffffffffffffffffffff-81\" }] };
                 b.put(D { id: 82, s: f\"gggggggggggggggggggggggggggggggggggggg-82\" });
                 println(f\"eight:{b.xs[0].s.len()}\");
             }
",
            &["d81", "eight:41", "d82"],
            "b2026-09-15-33-self-field-root",
        );

    // TWO displacements over the SAME slot. The double-fire guard's memory
    // half: two stores must release exactly two buffers, and a widening
    // that released the slot's occupant twice would report a double free
    // here before anywhere else.
    assert_clean_asan_run(
            "struct D { id: i64, s: String }
             impl Drop for D { fn drop(mut ref self) { println(f\"d{self.id}\") } }
             fn main() {
                 let mut a: Vec[D] = [D { id: 61, s: f\"hhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhh-61\" }];
                 let p: D = D { id: 62, s: f\"iiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiii-62\" };
                 let q: D = D { id: 63, s: f\"jjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjj-63\" };
                 a[0] = p;
                 a[0] = q;
                 println(f\"six:{a[0].s.len()}\");
             }
",
            &["d61", "d62", "six:46", "d63"],
            "b2026-09-15-33-store-twice",
        );
}
