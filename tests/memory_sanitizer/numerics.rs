//! integer and float behaviour, SIMD lanes, math intrinsics -- fixtures for `tests/memory_sanitizer.rs`.
//!
//! Split out of `tests/memory_sanitizer.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test memory_sanitizer` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test memory_sanitizer numerics::
//!
//! New fixtures about integer and float behaviour, SIMD lanes, math intrinsics belong in this file.

use super::*;

/// B-2026-08-31-18 — the memory half of giving a `Vector[T, N]` enum
/// payload its true word count.
///
/// Counting a 4-lane `i64` vector as ONE word meant it never exceeded a
/// variant's payload area, so the oversize-boxing path was unreachable for
/// vectors. With the true count an `Option[Vector[i64, 8]]` (8 words
/// against Option's 3-word area) heap-boxes on every construction — a
/// `malloc` that did not exist before this change, on a payload the drop
/// path had never been asked to free. A missing box-free leaks 64 bytes per
/// iteration; a double free or a read through the freed box is the other
/// direction. Neither shows in an output comparison, which is why the E2E
/// twin cannot stand in for this.
///
/// The loop also exercises the ALIGNMENT fix: the box store is emitted at
/// alignment 8 rather than the vector's natural 32, because `malloc`
/// guarantees only 16. Without it this program dies on `vmovaps` — and
/// heap-state-dependently, which is what makes an ASAN run over many
/// iterations a better witness than a single-shot fixture.
#[test]
fn asan_vector_enum_payload_box_frees_once() {
    let Some((out, status)) = run_under_asan(
        r#"fn mk(n: i64) -> Option[Vector[i64, 8]] {
    return Some(Vector[i64, 8](n, n+1, n+2, n+3, n+4, n+5, n+6, n+7))
}

fn main() {
    let n: i64 = env.args().len();
    let mut i = 0;
    while i < 20 {
        let o = mk(i + n);
        match o { Some(w) => { println(f"w {w}"); } None => {} }
        i = i + 1;
    }
    println("end");
}
"#,
        "asan_vector_enum_payload_box_frees_once",
    ) else {
        return;
    };
    assert!(status.success(), "ASAN/LSan reported a problem:\n{out}");
    assert_eq!(
        out.matches("w Vector(").count(),
        20,
        "every iteration must render its payload:\n{out}"
    );
    assert!(
        out.contains("w Vector(1, 2, 3, 4, 5, 6, 7, 8)") && out.contains("end"),
        "payload values are wrong:\n{out}"
    );
}

#[test]
fn asan_string_push_counted_loop_no_overflow() {
    // B-2026-07-18-15: a `String` accumulator built by `push(char)` inside a
    // counted `while` loop was mis-lowered through the Vec *tabulate*
    // reduction path. A `String` shares `Vec`'s `{ptr,len,cap}` layout, so
    // `llvm_ty_is_vec_struct` cannot tell them apart — the tabulate reserved
    // `trip_count * elem_size` (elem_size = 1 for the byte buffer) and did
    // fixed-stride *element* stores, but each `push(char)` writes a 4-byte
    // char slot, overrunning the buffer by 3 bytes per char (heap
    // buffer-overflow — valgrind: "Invalid write of size 4 … inside a block
    // of size N"). The fix bails a String accumulator out of both the
    // tabulate and Collect reduction lowerings to the correct per-push
    // codegen (which grows for the char encoding). Regression guard: the
    // 7-char build must be ASAN-clean and print the string + its length.
    assert_clean_asan_run(
        r#"
fn build(n: i64) -> String {
    let mut out = "";
    let mut i = 0;
    while i < n {
        out.push('x');
        i = i + 1;
    }
    return out;
}
fn main() {
    let s = build(7);
    println(s);
    println(s.len().to_string());
}
"#,
        &["xxxxxxx", "7"],
        "string_push_counted_loop_no_overflow",
    );
}

// ── L5: NUL-safe print over heap + literal storage ────────────
//
// The print path uses `fwrite(data, 1, len, stdout)` so interior NUL
// bytes are emitted, not truncated. ASAN guards the no-over-read
// property at the byte boundary: a heap String from concat (`a + b`) is
// `len`-prefixed and NOT NUL-terminated, so `fwrite` reading exactly
// `len` bytes must not touch the byte past the buffer (the prior `%s`
// path did, an ASAN heap-buffer-overflow). The literal `"AB\0"` global
// is sized `len + 1` and its interior NUL must survive the concat memcpy.

#[test]
fn asan_println_interior_nul_no_overflow() {
    assert_clean_asan_run(
        r#"
fn main() {
    let a = "AB\0";
    let b = "CD";
    let s = a + b;
    println(s);
    println('\0');
}
"#,
        &["AB\u{0}CD", "\u{0}"],
        "println_interior_nul_no_overflow",
    );
}

#[test]
fn asan_vec_extend_from_slice_triggers_grow_clean() {
    // Forces a grow mid-extend (dst cap=2, src len=4). The
    // grow path replaces dst's data pointer; the old buffer
    // must be freed on grow (not on scope exit), and the new
    // buffer must be freed on scope exit (not on grow).
    assert_clean_asan_run(
        r#"
fn main() {
    let src: Vec[i64] = Vec.filled(4, 5);
    let mut dst: Vec[i64] = Vec.with_capacity(2);
    dst.push(1);
    dst.extend_from_slice(src);
    println(dst.len());
}
"#,
        &["5"],
        "vec_extend_from_slice_triggers_grow_clean",
    );
}

#[test]
fn asan_vec_try_extend_from_slice_triggers_grow_clean() {
    // Fallible sibling of `asan_vec_extend_from_slice_triggers_grow_clean`
    // (phase-8-stdlib-floor item 8). `try_extend_from_slice` shares the
    // grow CFG with the panicking base but allocates through
    // `karac_alloc_fallible`; the success path must still free the old
    // buffer on grow (not on scope exit) and free the new buffer on scope
    // exit (not on grow). dst cap=2, src len=4 forces the grow.
    assert_clean_asan_run(
        r#"
fn main() {
    let src: Vec[i64] = Vec.filled(4, 5);
    let mut dst: Vec[i64] = Vec.with_capacity(2);
    dst.push(1);
    let _ = dst.try_extend_from_slice(src);
    println(dst.len());
}
"#,
        &["5"],
        "vec_try_extend_from_slice_triggers_grow_clean",
    );
}

#[test]
fn test_proven_index_add_overflow_elision_stays_in_bounds() {
    // B-2026-08-05-21: the index adds in this shape now emit a PLAIN add —
    // the overflow trap is elided because BCE proved the sum in bounds. So
    // there is no longer any runtime check of any kind on `base + lo` /
    // `base + hi`: not a bounds check (B-2026-08-04-8 removed that) and not
    // an overflow trap. ASAN is the only backstop left.
    //
    // Deliberately WIDE rows (1009 x 63, an odd width and a prime row
    // count) so `base` grows large across the scan and the elided add is
    // exercised at many magnitudes rather than only near zero — the buffer
    // is exactly `n * len`, so the final row's `base + hi_init` must land
    // on the last element with no slack.
    assert_clean_asan_run(
        r#"
fn main() {
    let n = 1009i64;
    let len = 63i64;
    let mut v: Vec[i64] = Vec.filled(n * len, 0i64);
    let mut i = 0i64;
    while i < n {
        let base = i * len;
        let mut lo = 0i64;
        let mut hi = len - 1i64;
        while lo <= hi {
            v[base + lo] = v[base + lo] + 1i64;
            v[base + hi] = v[base + hi] + 1i64;
            lo = lo + 1i64;
            hi = hi - 1i64;
        }
        i = i + 1;
    }
    let mut total = 0i64;
    let mut k = 0i64;
    while k < n * len {
        total = total + v[k];
        k = k + 1i64;
    }
    println(f"{total}");
}
"#,
        // Odd width: every cell gets +1 and the middle cell gets +1 again,
        // so 63 + 1 = 64 per row, and 64 * 1009 = 64576.
        &["64576"],
        "proven_index_add_overflow_elision",
    );
}

/// B-2026-08-01-3 residual (pass-roundtrip) — `e = pass(e)` over an
/// own-Drop enum, plus the struct twins (`s = pass_res(s)`,
/// `p = remake(p)` with a scalar-field mention), leaked the OLD value's
/// heap: owned args are caller-retains (the callee deep-copies at entry
/// and frees its copy), but the Assign arm's mentions-guard assumed the
/// old value moved into the callee and skipped the eager free — the
/// store then orphaned the original buffer. The fix eager-frees the old
/// slot (cap-guarded, memory only — the NLL channel fires the bodies)
/// when the RHS is a free-fn call to a user function returning an owned
/// value. LSan (Linux CI) is the leak gate; ASAN guards the eager free
/// against dangling the value the callee's copy returned.
#[test]
fn asan_roundtrip_reassign_frees_displaced_original() {
    assert_clean_asan_run(
        r#"
struct Res { id: i64, name: String }
impl Drop for Res {
    fn drop(mut ref self) { println(f"drop {self.id} {self.name}") }
}
enum Loud { Hold(Res), Quiet }
impl Drop for Loud {
    fn drop(mut ref self) { println("loud drop") }
}
struct Plain { id: i64, name: String }
fn mk_loud(n: i64) -> Loud { return Loud.Hold(Res { id: n, name: f"l{n}" }); }
fn pass(b: Loud) -> Loud { return b; }
fn mk_res(n: i64) -> Res { return Res { id: n, name: f"r{n}" }; }
fn pass_res(b: Res) -> Res { return b; }
fn mk_plain(n: i64) -> Plain { return Plain { id: n, name: f"p{n}" }; }
fn remake(b: Plain) -> Plain { return Plain { id: b.id + 1, name: f"p{b.id + 1}" }; }
fn main() {
    let mut e = mk_loud(7);
    e = pass(e);
    println("s1");
    let mut s = mk_res(4);
    s = pass_res(s);
    println(s.name);
    println("s2");
    let mut p = mk_plain(1);
    println(p.name);
    p = remake(p);
    println(p.name);
    println("end");
}
"#,
        &[
            "loud drop",
            "drop 7 l7",
            "s1",
            "r4",
            "drop 4 r4",
            "s2",
            "p1",
            "p2",
            "end",
        ],
        "roundtrip_reassign_frees_displaced_original",
    );
}

#[test]
fn asan_soa_field_index_store_no_overflow() {
    // B-2026-06-20-7: the buggy field-level SoA index-store strided the SoA
    // struct as a contiguous AoS element, so a store at index >= 1 wrote PAST
    // the target group's buffer — a heap-buffer-overflow ASAN catches. The
    // fix addresses the field's own group buffer at [i] by the group
    // sub-struct stride. This scatters field writes across BOTH groups
    // (`physics.x` and `combat.hp`) at indices 0..2, then reads them back,
    // looped 20x so any stray address trips ASAN. ≥36 bytes of live payload
    // per element so a reachable leak isn't masked by LSan's blind spot.
    assert_clean_asan_run(
        r#"
struct Body { x: f64, y: f64, hp: i64 }
layout bodies: Vec[Body] {
    group physics { x, y }
    group combat { hp }
}
fn main() {
    let mut sum = 0;
    let mut k = 0;
    while k < 20 {
        let mut bodies: Vec[Body] = Vec.new();
        bodies.push(Body { x: 1.0, y: 2.0, hp: 100 });
        bodies.push(Body { x: 3.0, y: 4.0, hp: 200 });
        bodies.push(Body { x: 5.0, y: 6.0, hp: 300 });
        let mut i = 0;
        while i < bodies.len() {
            bodies[i].hp = bodies[i].hp + 1;
            i = i + 1;
        }
        let mut j = 0;
        while j < bodies.len() {
            sum = sum + bodies[j].hp;
            j = j + 1;
        }
        k = k + 1;
    }
    println(sum);
}
"#,
        &["12060"],
        "soa_field_index_store_no_overflow",
    );
}

#[test]
fn asan_soa_whole_element_index_store_no_overflow() {
    // Follow-on (whole-element SoA index store `grid[i] = E { … }`): the
    // pre-fix store wrote the full AoS element over a SINGLE group's narrower
    // stride — a heap-buffer-overflow at the last element of every group
    // buffer (write 40-byte Cell at offset i*16 into the `lo` group's
    // 16-byte stride). ASAN flags the over-stride write directly; this is the
    // regression guard for the silent-overflow class. Scatter all 8 elements
    // by whole-element assignment each of 20 frames, then read back across
    // both groups so a dropped/over-stride write also changes the sum. Cell
    // is 40 bytes (two groups, both buffers past LSan's short-alloc blind
    // spot). grid[i] = {i+1, …}: sum of `a` (i+1, i 0..8) = 36, ×20 = 720.
    assert_clean_asan_run(
        r#"
struct Cell { a: f64, b: f64, c: f64, d: f64, e: f64 }
layout grid: Vec[Cell] { group lo { a, b } group hi { c, d, e } }
fn main() with panics {
    let mut sum = 0.0;
    let mut k = 0;
    while k < 20 {
        let mut grid: Vec[Cell] = Vec.new();
        let mut i = 0;
        while i < 8 { grid.push(Cell { a: 0.0, b: 0.0, c: 0.0, d: 0.0, e: 0.0 }); i = i + 1; }
        let mut j = 0;
        while j < grid.len() {
            grid[j] = Cell { a: (j + 1) as f64, b: 1.0, c: 2.0, d: 3.0, e: 4.0 };
            j = j + 1;
        }
        let mut r = 0;
        while r < grid.len() { sum = sum + grid[r].a; r = r + 1; }
        k = k + 1;
    }
    println(sum);
}
"#,
        &["720"],
        "soa_whole_element_index_store",
    );
}

#[test]
fn asan_gsort_tuple_and_float_elements() {
    // B-2026-06-30-15: tuple elements (per-field lexicographic — (1,z)
    // < (2,a) < (2,b)) and float elements both sort via the comparator
    // family now; both previously errored in codegen.
    //
    // B-2026-08-11-7 moved the float half from `Vec[f64]` to `Vec[F64]`.
    // `Vec[f64].sort()` is now a typecheck error, because with a NaN
    // present it left the sequence COMPLETELY unsorted on every backend
    // (`[3.0, NaN, 1.0, 2.0]` came back untouched) — a silent wrong
    // answer, and design.md's own counter-example. The wrapper is the
    // remedy that gate prescribes, so exercising it here keeps the
    // memory-safety coverage AND moves it onto the path users are now
    // directed to — which is a strictly better thing to ASAN, since the
    // element is a struct rather than a scalar. The `let` bindings before
    // the prints are required: Display of a struct in a non-place
    // position (`println(f[0])`) is a standing codegen restriction for
    // all user structs, not specific to the wrapper.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut v: Vec[(i64, String)] = Vec.new();
    v.push((2, f"bb padded beyond thirty-six bytes junk {1}"));
    v.push((1, f"zz padded beyond thirty-six bytes junk {1}"));
    v.push((2, f"aa padded beyond thirty-six bytes junk {1}"));
    v.sort();
    for t in v {
        println(t.0);
    }
    let mut f: Vec[F64] = Vec.new();
    f.push(F64.from(2.5));
    f.push(F64.from(1.5));
    f.push(F64.from(3.5));
    f.sort();
    let lo = f[0];
    let hi = f[2];
    println(lo);
    println(hi);
}
"#,
        &["1", "2", "2", "1.5", "3.5"],
        "gsort_tuple_and_float_elements",
    );
}

#[test]
fn asan_numeric_try_from_err_string_no_leak() {
    // Numeric narrowing `T.try_from(x) -> Result[T, String]`: the `Err`
    // payload is a static (`cap=0`) String, so the failure path must
    // allocate nothing and free nothing. Loops the Err arm (out-of-range)
    // and the Ok arm many times; any per-iteration String leak or bad free
    // accumulates for ASAN + LSan. Both the match-consumed `e` and the
    // discarded-Result shapes are exercised.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0;
    let mut oks: i64 = 0;
    let mut errs: i64 = 0;
    while i < 200 {
        match i8.try_from(i) {
            Ok(v) => { oks = oks + 1; }
            Err(e) => { if e.len() > 0 { errs = errs + 1; } }
        }
        i = i + 1;
    }
    println(f"{oks}");
    println(f"{errs}");
}
"#,
        &["128", "72"],
        "numeric_try_from_err_string_no_leak",
    );
}

/// B-2026-09-07-21 cell 1 — the SEQUENTIAL lane of the `a statement
/// follows` shape, which auto-par's own fixture cannot see.
///
/// The cell is already covered at `KARAC_AUTO_PAR=1` by
/// `asan_discarded_literal_projected_field_keeps_its_owner`, and passes
/// there. That is not a claim about this lane: the row was filed because
/// the same program at `KARAC_AUTO_PAR=0` stranded 38 B, and every
/// `assert_clean_asan_run` in this file runs with auto-par ON, so no
/// fixture in the suite exercised the leaking side.
///
/// What made the two lanes disagree was a DROPPED GUARD rather than a
/// genuine conflict. `expr_in_fanned_out_stmt` declines the
/// discarded-literal registration for a statement auto-par fans out,
/// because the `__par_branch_*` worker already frees the value. Its own
/// doc claimed it answers `false` when no concurrency analysis was
/// threaded in — but `auto_par_disabled` gates only the EMISSION, and
/// `concurrency_decisions` is populated either way, exactly as
/// `functions.rs`'s deque-head gate says in the comment above its own
/// `auto_par_disabled` check. So at `KARAC_AUTO_PAR=0` the decline still
/// fired and no worker existed to free anything.
#[test]
fn asan_discarded_literal_projected_field_seq_lane_keeps_its_owner() {
    assert_clean_asan_run_seq_lane(
        "struct P { a: String, b: i64 }\n\
             fn seed() -> i64 { env.args().len() }\n\
             fn payload() -> String { f\"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa\" }\n\
             fn mkp(n: i64) -> P { return P { a: payload(), b: n }; }\n\
             fn main() { println(go()); }\n\
             fn go() -> i64 { let t = mkp(9);\n\
             \x20 if seed() > 0 { P { a: t.a, b: 1 } } else { P { a: payload(), b: 2 } };\n\
             \x20 let z = payload();\n\
             \x20 z.len() - z.len() + 1 }\n",
        &["1"],
        "discarded_literal_projected_field_seq_lane",
    );
}

/// B-2026-09-01-21 — a DISCARDED struct literal MIXING a live-local source
/// with a minted sibling now registers an owner on the compiled backends,
/// where one non-fresh field used to decline the whole literal.
///
/// The memory question this pins is the one the widening creates: the
/// literal's field walk becomes the single owner of the moved source, and
/// the source's own UserDrop is retracted at the statement site. Get the
/// retraction wrong in one direction and the source's wrapper fires over
/// the moved-from slot; wrong in the other and its `String` is freed twice.
/// The `String` field is what gives ASAN something to catch either way.
#[test]
fn asan_a_discarded_literal_mixing_a_source_and_a_mint_frees_once() {
    const H: &str = "struct R { id: i64, s: String }\n\
             struct S { r: R, k: i64 }\n\
             struct S2 { r: R, s: R, k: i64 }\n\
             fn seed() -> i64 { env.args().len() }\n\
             fn payload() -> String { f\"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa\" }\n\
             fn mk(i: i64) -> R { return R { id: i, s: payload() }; }\n\
             fn main() { println(go()); }\n";
    let rows: &[(&str, &str)] = &[
        // The row's shape, in all three discarded spellings.
        (
            "mixed literal, bare statement",
            "fn go() -> i64 { let t = mk(7);\n\
                 \x20  S2 { r: t, s: mk(9), k: 1 };\n\
                 \x20  1 }\n",
        ),
        (
            "mixed literal, wildcard `let`",
            "fn go() -> i64 { let t = mk(7);\n\
                 \x20  let _ = S2 { r: t, s: mk(9), k: 1 };\n\
                 \x20  1 }\n",
        ),
        (
            "mixed literal behind a block wrapper",
            "fn go() -> i64 { let t = mk(7);\n\
                 \x20  { S2 { r: t, s: mk(9), k: 1 } };\n\
                 \x20  1 }\n",
        ),
        (
            "mixed literal, MINTED field first",
            "fn go() -> i64 { let t = mk(7);\n\
                 \x20  S2 { r: mk(9), s: t, k: 1 };\n\
                 \x20  1 }\n",
        ),
        // TWO sources and no mint: the widening admits this too, so the
        // retraction has to cover both or one wrapper fires over a husk.
        (
            "two sources, no mint",
            "fn go() -> i64 { let t = mk(7); let u = mk(8);\n\
                 \x20  S2 { r: t, s: u, k: 1 };\n\
                 \x20  1 }\n",
        ),
        (
            "one source, single-field literal",
            "fn go() -> i64 { let t = mk(7);\n\
                 \x20  S { r: t, k: 1 };\n\
                 \x20  1 }\n",
        ),
        // The TUPLE leg's own shape (B-2026-08-01-8), which B-2026-09-01-21
        // did not touch: `let _ = (t, 20);` leaks its `String` at -O0 too
        // (38 B / 1 allocation). It belongs here because it shows the
        // memory gap is SHARED by the tuple and struct legs rather than
        // introduced by the struct admission — the tuple leg has had the
        // movable-place hatch for a month and leaks identically.
        (
            "the TUPLE leg's own movable place leaks the same way",
            "fn go() -> i64 { let t = mk(7);\n\
                 \x20  let _ = (t, 20);\n\
                 \x20  1 }\n",
        ),
        // ── double-own guards: a second owner here is a DOUBLE FREE ──
        (
            "guard: the BOUND `let` is owned by its binding, not the site",
            "fn go() -> i64 { let t = mk(7);\n\
                 \x20  let w = S2 { r: t, s: mk(9), k: 1 };\n\
                 \x20  w.k }\n",
        ),
        (
            "guard: an ALL-MINTED discarded literal keeps its own owner",
            "fn go() -> i64 { S2 { r: mk(7), s: mk(9), k: 1 };\n\
                 \x20  1 }\n",
        ),
        // ── controls, clean before and after ─────────────────────────
        // NOT a control here: `S2 { r: mk(1), s: mk(9), k: t.id };` — a
        // SCALAR field read of a live local inside the literal — leaks
        // both minted objects on the compiled backends (76 B / 2 allocs).
        // Measured identical on unmodified `main`, so it predates this
        // fix and is the same "one non-fresh field declines the whole
        // literal" mechanism with a different field shape. Filed as
        // B-2026-09-01-24 rather than folded in; a scalar read moves
        // nothing, so its fix is an admission with no retraction, not
        // this row's admission-plus-retraction.
        (
            "control: the local is read after the statement, not inside it",
            "fn go() -> i64 { let t = mk(7);\n\
                 \x20  S2 { r: mk(1), s: mk(9), k: 5 };\n\
                 \x20  t.id - t.id + 1 }\n",
        ),
    ];
    for (label, body) in rows {
        assert_clean_asan_run(&format!("{H}{body}"), &["1"], label);
    }
}

#[test]
fn asan_print_a_vec_place_expression_does_not_free_it() {
    // B-2026-08-14-30: printing a `Vec` read out of a PLACE — a struct
    // field, an element, a shared node's field — used to register that
    // container's own `{ptr, len, cap}` for cleanup, because both Vec
    // display paths took "not a bound identifier" to mean "a materialized
    // temporary with no other owner". A place expression is neither, so the
    // buffer was freed twice: `println(b.xs)` on a `struct B { xs: Vec[i64] }`
    // aborted with `free(): double free detected`, and a `Vec[String]` or a
    // nested `Vec` SEGFAULTED because the drain walked elements it did not
    // own.
    //
    // This is the ASAN half; the value half is the codegen/interpreter
    // twin. LOOPED 20 times because the failure this guards against is a
    // double free on the FIRST iteration but the fix's risk is the opposite
    // — a place expression whose buffer nobody frees — and only repetition
    // makes a per-print leak large enough to be unmistakable.
    assert_clean_asan_run(
        r#"
struct B { xs: Vec[String], ns: Vec[i64], nested: Vec[Vec[i64]] }
shared struct S { xs: Vec[String] }
fn main() {
    let b = B { xs: ["alphabetalphabet", "gammagammagamma"], ns: [1, 2, 3], nested: [[1, 2], [3]] };
    let s = S { xs: ["deltadeltadeltad", "epsilonepsilonep"] };
    let mut k = 0;
    while k < 20 {
        println(f"{b.xs}");
        println(b.ns);
        println(f"{b.nested}");
        println(f"{b.nested[0]}");
        println(f"{s.xs}");
        k = k + 1;
    }
    println("done");
}
"#,
        &[
            "[alphabetalphabet, gammagammagamma]",
            "[1, 2, 3]",
            "[[1, 2], [3]]",
            "[1, 2]",
            "[deltadeltadeltad, epsilonepsilonep]",
        ]
        .iter()
        .cycle()
        .take(100)
        .copied()
        .chain(std::iter::once("done"))
        .collect::<Vec<_>>(),
        "asan_print_a_vec_place_expression_does_not_free_it",
    );
}

#[test]
fn asan_print_an_owned_vec_temporary_still_frees_it() {
    // B-2026-08-14-30's other direction, and the reason the fix enumerates
    // PRODUCERS rather than excluding places. A collection literal and a
    // call result really are materialized temporaries with no other owner:
    // if the narrowed gate stopped tracking them, the double free would
    // become a leak of every printed temporary. Looped so that leak would
    // be 80 buffers rather than four.
    assert_clean_asan_run(
        r#"
fn mk() -> Vec[String] { ["alphabetalphabet", "gammagammagamma"] }
fn main() {
    let mut k = 0;
    while k < 20 {
        println([9, 8]);
        println(f"{[7, 6]}");
        println(mk());
        println(f"{mk()}");
        k = k + 1;
    }
    println("done");
}
"#,
        &[
            "[9, 8]",
            "[7, 6]",
            "[alphabetalphabet, gammagammagamma]",
            "[alphabetalphabet, gammagammagamma]",
        ]
        .iter()
        .cycle()
        .take(80)
        .copied()
        .chain(std::iter::once("done"))
        .collect::<Vec<_>>(),
        "asan_print_an_owned_vec_temporary_still_frees_it",
    );
}

#[test]
fn asan_print_a_map_or_set_place_expression_does_not_free_it() {
    // B-2026-08-14-31's memory half. The row itself is a wrong ANSWER — a
    // Map/Set place expression printed its control pointer — but the arm
    // that fixes it is the same SHAPE as the one B-2026-08-14-30 had to
    // repair: materialize a value into a temp and render it. That row's
    // lesson is that such an arm must not take ownership of a place
    // expression, or the container's handle is freed twice.
    //
    // So this guards the fix rather than the bug: every place spelling the
    // new arm serves, looped 20 times, asserting that rendering a Map or Set
    // field neither frees the owner's handle (a double free on the first
    // iteration) nor strands anything (a leak that only 20 iterations makes
    // unmistakable). The value half is the codegen/interpreter twin.
    // B-2026-08-14-36 made this load-bearing rather than precautionary: that
    // row DID give the arm ownership of a produced temporary, so what keeps
    // these spellings safe is now a live predicate
    // (`print_vec_operand_is_owned_temp`) rather than the absence of any
    // tracking at all. Tuple element and `Vec[Map]` element are here for the
    // same reason as the struct field — each reads a handle its container
    // still owns, and each would be a double free if the predicate widened.
    assert_clean_asan_run(
        r#"
struct B { m: Map[String, i64], s: Set[String] }
fn main() {
    let mut m: Map[String, i64] = Map.new();
    m.insert("keykeykeykeykey", 1);
    let mut st: Set[String] = Set.new();
    st.insert("elemelemelemelem");
    let b = B { m: m, s: st };
    let mut tm: Map[String, i64] = Map.new();
    tm.insert("tuptuptuptuptup", 2);
    let t = (tm, 0);
    let mut vm: Map[String, i64] = Map.new();
    vm.insert("vecvecvecvecvec", 3);
    let v: Vec[Map[String, i64]] = [vm];
    let mut k = 0;
    while k < 20 {
        println(f"{b.m}");
        println(b.s);
        println(f"{t.0}");
        println(f"{v[0]}");
        k = k + 1;
    }
    println("done");
}
"#,
        &[
            "{keykeykeykeykey: 1}",
            "Set{elemelemelemelem}",
            "{tuptuptuptuptup: 2}",
            "{vecvecvecvecvec: 3}",
        ]
        .iter()
        .cycle()
        .take(80)
        .copied()
        .chain(std::iter::once("done"))
        .collect::<Vec<_>>(),
        "asan_print_a_map_or_set_place_expression_does_not_free_it",
    );
}

#[test]
fn asan_deferred_init_vec_i64_push_realloc() {
    assert_clean_asan_run(
        r#"
fn main() {
    let c = false;
    let mut v: Vec[i64];
    if c { v = [1, 2]; } else { v = Vec.new(); }
    let mut i = 0;
    while i < 100 {
        v.push(i);
        i = i + 1;
    }
    println(v.len());
    println(v[99]);
}
"#,
        &["100", "99"],
        "asan_deferred_init_vec_i64_push_realloc",
    );
}

#[test]
fn asan_boxed_payload_view_does_not_mint_a_second_bodies_walk() {
    const DECLS: &str = "struct P { id: i64 }\n\
             impl Drop for P { fn drop(mut ref self) { println(f\"dP{self.id}\") } }\n\
             struct Q { id: i64 }\n\
             impl Drop for Q { fn drop(mut ref self) { println(f\"dQ{self.id}\") } }\n\
             struct W { a: P, b: Q, n: i64, p: i64 }\n";

    // Two Drop fields, the SECOND moved: the first field's body doubled.
    assert_clean_asan_run(
            &format!(
                "{DECLS}\
                 fn eat() {{\n\
                 \x20   let o: Option[W] = Option.Some(W {{ a: P {{ id: 6 }}, b: Q {{ id: 7 }}, n: 2, p: 3 }});\n\
                 \x20   match o {{ Option.Some(t) => {{ let y = t.b; println(\"mid\"); }} Option.None => {{ println(\"n\"); }} }}\n\
                 }}\n\
                 fn main() {{ eat(); println(\"end\"); }}\n"
            ),
            &["dQ7", "mid", "dP6", "end"],
            "b11914-payload-view-second-field-moved",
        );

    // The FIRST moved, so this is not about which index survives.
    assert_clean_asan_run(
            &format!(
                "{DECLS}\
                 fn eat() {{\n\
                 \x20   let o: Option[W] = Option.Some(W {{ a: P {{ id: 6 }}, b: Q {{ id: 7 }}, n: 2, p: 3 }});\n\
                 \x20   match o {{ Option.Some(t) => {{ let y = t.a; println(\"mid\"); }} Option.None => {{ println(\"n\"); }} }}\n\
                 }}\n\
                 fn main() {{ eat(); println(\"end\"); }}\n"
            ),
            &["dP6", "mid", "dQ7", "end"],
            "b11914-payload-view-first-field-moved",
        );

    // A by-value PARAM scrutinee: the mint is the SOLE owner here, so the
    // guard must NOT fire. Correct before this fix and after it.
    assert_clean_asan_run(
            &format!(
                "{DECLS}\
                 fn eat(o: Option[W]) {{\n\
                 \x20   match o {{ Option.Some(t) => {{ let y = t.b; println(\"mid\"); }} Option.None => {{ println(\"n\"); }} }}\n\
                 }}\n\
                 fn main() {{ eat(Option.Some(W {{ a: P {{ id: 6 }}, b: Q {{ id: 7 }}, n: 2, p: 3 }})); println(\"end\"); }}\n"
            ),
            &["dQ7", "mid", "dP6", "end"],
            "b11914-payload-view-by-value-param",
        );

    // ONE Drop field: the mask empties the walk, which an older early
    // return already handled. Control.
    assert_clean_asan_run(
            "struct P { id: i64 }\n\
             impl Drop for P { fn drop(mut ref self) { println(f\"dP{self.id}\") } }\n\
             struct V { a: P, n: i64, p: i64, q: i64 }\n\
             fn eat() {\n\
             \x20   let o: Option[V] = Option.Some(V { a: P { id: 6 }, n: 1, p: 2, q: 3 });\n\
             \x20   match o { Option.Some(t) => { let y = t.a; println(\"mid\"); } Option.None => { println(\"n\"); } }\n\
             }\n\
             fn main() { eat(); println(\"end\"); }\n",
            &["dP6", "mid", "end"],
            "b11914-payload-view-single-drop-field-control",
        );

    // Nothing moved: the envelope's walk owes both bodies. Control.
    assert_clean_asan_run(
            &format!(
                "{DECLS}\
                 fn eat() {{\n\
                 \x20   let o: Option[W] = Option.Some(W {{ a: P {{ id: 6 }}, b: Q {{ id: 7 }}, n: 2, p: 3 }});\n\
                 \x20   match o {{ Option.Some(t) => {{ println(f\"mid{{t.a.id}}\"); }} Option.None => {{ println(\"n\"); }} }}\n\
                 }}\n\
                 fn main() {{ eat(); println(\"end\"); }}\n"
            ),
            &["mid6", "dQ7", "dP6", "end"],
            "b11914-payload-view-readonly-control",
        );
}
