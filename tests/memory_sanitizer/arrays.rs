//! fixed-size arrays and SoA element storage -- fixtures for `tests/memory_sanitizer.rs`.
//!
//! Split out of `tests/memory_sanitizer.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test memory_sanitizer` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test memory_sanitizer arrays::
//!
//! New fixtures about fixed-size arrays and SoA element storage belong in this file.

use super::*;

/// A fresh `Array[T, N]` temporary compared with `==` drops its elements
/// (B-2026-08-27-30).
///
/// The `Vec` and `Slice` siblings hang their cleanup off a buffer free; an
/// array is a by-value `[N x T]` with NO header, so nothing on the
/// comparison path could free it and the elements simply survived.
/// Measured before the fix: `mk(2) == mk(2)` over `Array[String, 2]` lost
/// 26 bytes in 4 allocations.
///
/// The comparison BORROWS both operands, so only a temporary dies here.
/// The non-fresh legs are what keep that honest, and each is read back
/// afterwards so a wrong drop surfaces as a use-after-free rather than
/// staying latent: a BOUND operand is owned by its binding, and rodata
/// LITERAL elements are not heap at all -- dropping either would be a
/// double-free or a free of static memory. The literal-vs-fresh row mixes
/// the two in ONE comparison, which is where an implementation that
/// dropped "both operands" rather than "each fresh operand" fails.
///
/// `Array[Vec[i64], 2]` is here because the element policy is shared with
/// the `Vec` drain (`vec_element_drain_fn`): a fix keyed on `String`
/// specifically would pass every other row and fail this one.
#[test]
fn asan_fresh_array_temp_eq_drops_its_elements() {
    assert_clean_asan_run(
        r#"
fn mk(n: i64) -> Array[String, 2] { return Array[f"item{n}", f"item{n + 1}"]; }
fn mkv(n: i64) -> Array[Vec[i64], 2] {
    let mut x: Vec[i64] = Vec.new();
    x.push(n);
    let mut y: Vec[i64] = Vec.new();
    y.push(n + 1);
    return Array[x, y];
}

fn main() {
    println(f"{mk(2) == mk(2)}");
    println(f"{mk(2) != mk(4)}");
    println(f"{mkv(1) == mkv(1)}");
    println(f"{Array["aa", "bb"] == Array["aa", "bb"]}");
    println(f"{Array["item5", "item6"] == mk(5)}");
    let p = mk(7);
    let q = mk(7);
    println(f"{p == q}");
    println(p[0]);
    println(q[1]);
}
"#,
        &[
            "true", "true", "true", "true", "true", "true", "item7", "item8",
        ],
        "asan_fresh_array_temp_eq_drops_its_elements",
    );
}

/// An INDEXED fresh `Array[T, N]` temporary drops its elements
/// (B-2026-08-27-31).
///
/// The `==`-operand position got this in B-2026-08-27-30; this is the same
/// array with no header and no owner, reached one expression kind over.
/// Measured before the fix: `mk(4)[0]` over `Array[String, 2]` lost 13
/// bytes in 2 allocations -- BOTH elements, the indexed one included.
///
/// Lowered exactly as `compile_inline_temp_vec_index_ex` lowers the `Vec`
/// temporary it is the peer of -- read the element, DEEP-CLONE it, drop the
/// whole temporary -- which is why the `Vec` spelling of this program was
/// already clean. The clone is what makes dropping the indexed element
/// safe, and it is also what the interpreter does: indexing does not
/// consume, so the read is a copy. The clone in turn has no consuming
/// binding, so `expr_is_inline_temp_vec_heap_index` admits the shape and
/// every owning consumer frees it -- without that half the fix trades 2
/// leaked elements for 1 leaked clone.
///
/// The BOUND legs are the controls, and they are not decoration: a bound
/// array is owned by its binding, so a drop here would double-free. Each is
/// read back after the indexing so a wrong drop surfaces as a
/// use-after-free rather than staying latent. `Array[i64, N]` pins the
/// element policy (`vec_element_drain_fn`, shared with the `Vec` drain) --
/// a scalar element clones nothing and drops nothing.
///
/// An `Array[Vec[..], N]` TEMPORARY is deliberately absent: reading an
/// element out of one needs either a nested index or an indexed-receiver
/// method call, and both are separate v1 codegen gaps ("requires the
/// indexed container to be a named variable"). That element type is
/// covered as a struct FIELD in the sibling fixture below, which reaches
/// the same `emit_drop_fn_for_array` with the same element policy.
///
/// The loop leg pins that the drop is emitted at the READ rather than
/// registered at scope exit: a scope-exit registration hoists one entry
/// alloca and frees only the last iteration's value, which is the
/// B-2026-08-25-33 shape `free_fresh_owned_struct_key_arg` documents.
#[test]
fn asan_indexed_fresh_array_temp_drops_its_elements() {
    assert_clean_asan_run(
        r#"
fn mk(n: i64) -> Array[String, 2] { return Array[f"item{n}", f"item{n + 1}"]; }
fn mknum(n: i64) -> Array[i64, 2] { return Array[n, n + 1]; }

fn main() {
    println(mk(4)[0]);
    println(mk(4)[1]);
    println(mknum(9)[1]);
    for i in 0..3 {
        println(mk(i)[0]);
    }
    let p = mk(1);
    println(p[0]);
    println(p[1]);
}
"#,
        &[
            "item4", "item5", "10", "item0", "item1", "item2", "item1", "item2",
        ],
        "asan_indexed_fresh_array_temp_drops_its_elements",
    );
}

/// An INDEXED array LITERAL temporary drops its elements
/// (B-2026-08-27-35).
///
/// The producer shape B-2026-08-27-31 left: that row covers a fresh array
/// temporary from a CALL (`mk(4)[0]`), and the two were separated only by
/// `expr_yields_fresh_owned_temp`, which admits `Call`/`MethodCall` and
/// nothing else. A literal owns its elements just as a call temporary does
/// -- `compile_array_literal` moves each one in and suppresses whatever
/// independent cleanup produced it -- so nothing else was ever going to
/// free them.
///
/// MEASURE THIS ONE AT `KARAC_OPT_LEVEL=0`, which is what the number below
/// is: `println(Array[f"aaaa{n}", f"b{n}"][0])` lost 7 bytes in 2
/// allocations, BOTH elements. At the default -O2 the same program reports
/// a single 5-byte leak, because the non-indexed element is dead and LLVM
/// removes its `malloc` -- reading that asymmetry as ownership ("the other
/// elements already have an owner") is what the filing row did, and it is
/// an artifact. The suite runs at the default level, so the loop leg is
/// what keeps both elements live here.
///
/// The RODATA leg is the control the widening is riskiest against:
/// `Array["aa", "bb"][0]` reaches the same `emit_drop_fn_for_array`, whose
/// String drain is cap-guarded (`cap > 0`), so a static element is a no-op
/// rather than a free of unmalloc'd memory. The BOUND-source leg
/// (`Array[s, t][0]`) pins the other direction: those elements are MOVED
/// into the literal (the bindings' caps are zeroed), so the array is their
/// only owner and a drop here is not a double-free.
#[test]
fn asan_indexed_array_literal_temp_drops_its_elements() {
    assert_clean_asan_run(
        r#"
fn mk(n: i64) -> String { return f"item{n}"; }

fn main() {
    let n = 3;
    println(Array[f"aaaa{n}", f"b{n}"][0]);
    println(Array[f"aaaa{n}", f"b{n}"][1]);
    println(Array["aa", "bb"][0]);
    println(Array["aa", f"cc{n}"][1]);
    println(Array[mk(1), mk(2)][1]);
    println(Array[n, n + 1][1]);
    let s = f"sssss{n}";
    let t = f"t{n}";
    println(Array[s, t][0]);
    for i in 0..3 {
        println(Array[f"x{i}", f"y{i}"][1]);
    }
}
"#,
        &[
            "aaaa3", "b3", "aa", "cc3", "item2", "4", "sssss3", "y0", "y1", "y2",
        ],
        "asan_indexed_array_literal_temp_drops_its_elements",
    );
}

/// An array LITERAL operand of `==` drops its elements
/// (B-2026-08-27-35, the sibling position).
///
/// B-2026-08-27-30 gave the `==` site its array drop and gated it on
/// `expr_yields_fresh_owned_temp`, so the CALL operand was covered and the
/// literal was not: measured at `KARAC_OPT_LEVEL=0`,
/// `Array[f"aaaa{n}", f"b{n}"] == Array[f"cccccc{n}", f"d{n}"]` lost all
/// four elements (16 bytes in 4 allocations). Unlike the indexed shape this
/// one leaks at the default -O2 too -- the comparator reads every element,
/// so none of the allocations is dead.
///
/// The rodata leg is the same control as in the sibling fixture and matters
/// more here: `Array["aa", "bb"] == Array["aa", "bb"]` was already in
/// B-2026-08-27-30's fixture as a shape that must NOT be dropped, and it is
/// dropped now -- safely, through the drain's `cap > 0` guard. The mixed
/// leg pins that one operand can be droppable and the other not within a
/// single comparison.
#[test]
fn asan_array_literal_eq_operand_drops_its_elements() {
    assert_clean_asan_run(
        r#"
fn mk(n: i64) -> Array[String, 2] { return Array[f"item{n}", f"item{n + 1}"]; }

fn main() {
    let n = 3;
    println(f"{Array[f"aaaa{n}", f"b{n}"] == Array[f"cccccc{n}", f"d{n}"]}");
    println(f"{Array[f"aaaa{n}", f"b{n}"] == Array[f"aaaa{n}", f"b{n}"]}");
    println(f"{Array["aa", "bb"] == Array["aa", "bb"]}");
    println(f"{Array[f"item5", f"item6"] == mk(5)}");
    let p = mk(7);
    println(f"{Array[f"item7", f"item8"] == p}");
    println(p[0]);
    for i in 0..3 {
        println(f"{Array[f"x{i}", f"y{i}"] == Array[f"x{i}", f"y{i}"]}");
    }
}
"#,
        &[
            "false", "true", "true", "true", "true", "item7", "true", "true", "true",
        ],
        "asan_array_literal_eq_operand_drops_its_elements",
    );
}

/// B-2026-09-15-15 — a MULTI-FIELD enum variant owns its boxed
/// `Array[T, N]` payload, and this is the fixture that actually proves it.
///
/// THE CODEGEN TWIN CANNOT CARRY THIS ROW, which is worth stating because
/// it usually can. Both failure modes here are invisible to an output
/// assertion: the leak by nature, and the double free because
/// `run_program` returns `None` on a non-zero exit and the tolerant
/// `if let Some(aot)` form then asserts nothing. Verified — with the fix
/// reverted, `e2e_multi_field_variant_owns_its_boxed_array_payload` passes.
/// It is kept for A/B output parity; THIS fixture is the guard, and unlike
/// B-2026-09-15-3's memory twin it is non-vacuous, because a double free
/// and a leak are both malloc/free BOOKKEEPING, which ASAN intercepts even
/// though it does not instrument `karac`'s emitted IR.
///
/// The cells, each measured broken at `-O0` before the fix:
///
///     two / mix / mix2 / tri / struct / heapsib   48 B per array payload,
///                                                 plus its elements, LEAKED
///     tuplehand / out / structhand                the same leak, and a
///                                                 DOUBLE FREE once the
///                                                 classifier widens
///     struct1hand                                 a PRE-EXISTING double
///                                                 free on stock `main`
///
/// `struct1hand` is the one that is not this row's own regression: a
/// SINGLE-field struct-shaped variant was already classified `BoxedArray`,
/// but `register_boxed_array_payload_alias` matched only `TupleVariant`
/// patterns, so an arm binding `St1.S { a }` and handing `a` on had two
/// owners and aborted. It is in this fixture because the disarm's struct
/// arm is what fixes it, and a fixture that covered only the widened
/// shapes would let it regress silently.
///
/// `srcread` pins the use-after-free the leak was MASKING — with nothing
/// freeing the box, the moved-from source stayed readable by accident.
/// B-2026-09-15-3's per-argument defensive copy is what keeps it readable
/// now that the box is freed, so this cell is a cross-row guard.
///
/// `single` is the control that was always balanced.
///
/// FLOORED at 60 allocations: the stranded boxes are exactly what LLVM
/// deletes when nothing observes them, so an `-O2`-only zero proves
/// nothing. Measured 178 allocs / 178 frees.
#[test]
fn asan_multi_field_variant_owns_its_boxed_array_payload() {
    assert_clean_asan_run_min_allocs(
        r#"
struct R { s: String }
enum Two { Both(Array[String, 2], Array[String, 2]), None2 }
enum Mix { M(Array[String, 2], i64), N }
enum Mix2 { M(i64, Array[String, 2]), N }
enum Tri { T(Array[String, 2], i64, Array[String, 2]), N }
enum St { S { a: Array[String, 2], n: i64 }, N }
enum St1 { S { a: Array[String, 2] }, N }
enum Mx { M(Array[String, 2], String), N }
enum Wrp { Full(Array[String, 2]), Empty }

fn mka(t: String) -> Array[String, 2] {
    return [f"b1515-{t}-aaaaaaaaaaaaaaaa", f"b1515-{t}-bbbbbbbbbbbbbbbb"];
}
fn eat(a: Array[String, 2]) -> i64 { return a[0].len(); }
fn tlen(t: Two) -> i64 { match t { Two.Both(x, y) => { return x[0].len() + y[1].len(); } Two.None2 => { return 0; } } }
fn mlen(m: Mix) -> i64 { match m { Mix.M(x, k) => { return x[0].len() + k; } Mix.N => { return 0; } } }
fn mlen2(m: Mix2) -> i64 { match m { Mix2.M(k, x) => { return x[0].len() + k; } Mix2.N => { return 0; } } }
fn trilen(t: Tri) -> i64 { match t { Tri.T(x, k, y) => { return x[0].len() + k + y[1].len(); } Tri.N => { return 0; } } }
fn stlen(s: St) -> i64 { match s { St.S { a, n } => { return a[0].len() + n; } St.N => { return 0; } } }
fn sthand(s: St) -> i64 { match s { St.S { a, n } => { return eat(a) + n; } St.N => { return 0; } } }
fn st1hand(s: St1) -> i64 { match s { St1.S { a } => { return eat(a); } St1.N => { return 0; } } }
fn mhand(m: Mix) -> i64 { match m { Mix.M(x, k) => { return eat(x) + k; } Mix.N => { return 0; } } }
fn mout(m: Mix) -> Array[String, 2] { match m { Mix.M(x, k) => { return x; } Mix.N => { return mka("z"); } } }
fn mxlen(m: Mx) -> i64 { match m { Mx.M(x, s) => { return x[0].len() + s.len(); } Mx.N => { return 0; } } }
fn wlen(w: Wrp) -> i64 { match w { Wrp.Full(x) => { return x[0].len(); } Wrp.Empty => { return 0; } } }

fn main() {
    let mut i: i64 = 0;
    while i < 2 {
        { let w = Two.Both(mka(f"p{i}"), mka(f"q{i}")); println(f"two:{tlen(w)}"); }
        { let w = Mix.M(mka(f"a{i}"), 5); println(f"mix:{mlen(w)}"); }
        { let w = Mix2.M(5, mka(f"b{i}")); println(f"mix2:{mlen2(w)}"); }
        { let w = Tri.T(mka(f"c{i}"), 5, mka(f"d{i}")); println(f"tri:{trilen(w)}"); }
        { let w = St.S { a: mka(f"e{i}"), n: 5 }; println(f"struct:{stlen(w)}"); }
        { let w = St.S { a: mka(f"f{i}"), n: 5 }; println(f"structhand:{sthand(w)}"); }
        { let w = St1.S { a: mka(f"g{i}") }; println(f"struct1hand:{st1hand(w)}"); }
        { let w = Mix.M(mka(f"h{i}"), 5); println(f"tuplehand:{mhand(w)}"); }
        { let w = Mix.M(mka(f"j{i}"), 5); let r = mout(w); println(f"out:{r[1].len()}"); }
        { let w = Mx.M(mka(f"k{i}"), f"b1515-sib-aaaaaaaaaaaaaaaa"); println(f"heapsib:{mxlen(w)}"); }
        let src = mka(f"m{i}");
        { let w = Mix.M(src, 5); println(f"srcmove:{mlen(w)}"); }
        println(f"srcread:{src[0].len()}");
        { let w = Wrp.Full(mka(f"n{i}")); println(f"single:{wlen(w)}"); }
        i = i + 1;
    }
}
"#,
        &[
            "two:50",
            "mix:30",
            "mix2:30",
            "tri:55",
            "struct:30",
            "structhand:30",
            "struct1hand:25",
            "tuplehand:30",
            "out:25",
            "heapsib:51",
            "srcmove:30",
            "srcread:25",
            "single:25",
            "two:50",
            "mix:30",
            "mix2:30",
            "tri:55",
            "struct:30",
            "structhand:30",
            "struct1hand:25",
            "tuplehand:30",
            "out:25",
            "heapsib:51",
            "srcmove:30",
            "srcread:25",
            "single:25",
        ],
        "asan_multi_field_variant_owns_its_boxed_array_payload",
        60,
    );
}

/// B-2026-09-15-18 — the GENERIC twin of the fixture above: a multi-field
/// variant whose boxed field is the erased parameter `T`.
///
/// B-2026-09-15-15 gave the NON-generic spelling an owner by classifying
/// the field `BoxedArray` at declaration time. A generic variant's field
/// type is the parameter, so `array_elem_and_len` answers `None` there and
/// nothing is classified; the monomorphic path that would catch it
/// (`user_enum_boxed_payload_variants`) declined any variant with more than
/// one field, so the box was nobody's. Measured broken at `-O0` under
/// `valgrind --leak-check=full`, two rounds each, `Array[i64, 4]` payloads:
///
///     mix / mix2 / struct / hand / out  64 B per cell (one 32 B box a round)
///     two                               128 B (two boxes a round)
///     single                            clean — the control the row names
///
/// THE PAYLOAD ELEMENT TYPE IS `i64` ON PURPOSE, and that is the one thing
/// to know before editing this. An `Array[String, N]` payload leaks its
/// ELEMENTS whatever this row does — the registration is box-only, and the
/// interior is B-2026-09-16-15's subject — so a `String`-element cell could
/// not appear in a clean-run fixture at all. With `i64` elements the box is
/// the only heap in the program, which makes the cells measure exactly the
/// thing this row fixed and nothing else. The row's own reproduction notes
/// the same shape: "192 B for an `Array[i64, 3]` that owns no heap at all
/// and leaked purely the needless box."
///
/// ONE SHAPE IS DELIBERATELY ABSENT. A variant mixing the erased field with
/// a heap-bearing SIBLING — `enum Gh[T] { Y(T, String), N }` — is declined
/// by the classifier and still strands its box, so it is not here. That is
/// not caution: admitting it was measured, and it traded the box for the
/// sibling. At `T = Array[String, 2]`, two rounds, passing the value to
/// `fn gh(g: Gh[Array[String, 2]])`, the box (96 B over two rounds) came
/// back and the two sibling `String`s (10 B in 2 blocks) stopped being
/// freed, because membership of `boxed_enum_payload_vars` arms an
/// argument-move suppressor that zeroes the WHOLE slot and so neutralizes
/// the sibling's `cap > 0` guard along with the box's tag guard. Its
/// non-generic twin is the `heapsib` cell in the fixture above, which stays
/// correct because both of its fields are classified at declaration and
/// neither takes this registration. See the whole-variant stand-down in
/// `user_enum_boxed_payload_variants` for the full note.
///
/// `hand` (the arm hands the array to a by-value callee) and `out` (the arm
/// returns it) are here because they are where a widened classifier turns a
/// leak into a DOUBLE FREE if the box gains a second owner — the hazard the
/// row's own "worth checking when it is done" paragraph names. Both are
/// clean, and valgrind reports no invalid free on either.
///
/// FLOORED at 12 allocations: the stranded boxes are exactly what LLVM
/// deletes when nothing observes them, so an `-O2`-only zero proves
/// nothing.
///
/// AND THAT IS NOT A FLOOR REMARK HERE, IT IS THE WHOLE GUARD: this cell is
/// non-vacuous ONLY at `-O0`, so `scripts/asan-o0-leg.sh` is what catches a
/// regression in it and the default `--features llvm` run is not. Measured
/// both ways on the pre-fix tree, with the fix stashed out of `src/` and a
/// marker `grep -c` printed as the guard that the stash really took:
/// at the harness default it reported `ok`, and at `KARAC_OPT_LEVEL=0` it
/// failed with exit 23 and `ERROR: LeakSanitizer`. The mechanism behind the
/// green default run was not isolated — CLAUDE.md's standing reason (at
/// `-O2` LLVM deletes an allocation nothing observes) fits, and so does a
/// stale stack slot keeping the box reachable for LSan's conservative scan;
/// which one it is does not change what to run.
/// Do not read a green default run as evidence about this fixture.
#[test]
fn asan_generic_multi_field_variant_owns_its_boxed_array_payload() {
    assert_clean_asan_run_min_allocs(
        r#"
enum Ga[T] { Y(T, i64), N }
enum Gb[T] { Y(i64, T), N }
enum Gt[T] { Y(T, T), N }
enum Gs[T] { Y { a: T, n: i64 }, N }
enum G1[T] { Y(T), N }

fn mki(t: i64) -> Array[i64, 4] { return [t, t + 1, t + 2, t + 3]; }
fn eat(a: Array[i64, 4]) -> i64 { return a[0]; }

fn ga(g: Ga[Array[i64, 4]]) -> i64 { match g { Ga.Y(x, k) => { return x[0] + k; } Ga.N => { return 0; } } }
fn gb(g: Gb[Array[i64, 4]]) -> i64 { match g { Gb.Y(k, x) => { return x[0] + k; } Gb.N => { return 0; } } }
fn gt(g: Gt[Array[i64, 4]]) -> i64 { match g { Gt.Y(x, y) => { return x[0] + y[1]; } Gt.N => { return 0; } } }
fn gs(g: Gs[Array[i64, 4]]) -> i64 { match g { Gs.Y { a, n } => { return a[0] + n; } Gs.N => { return 0; } } }
fn ghand(g: Ga[Array[i64, 4]]) -> i64 { match g { Ga.Y(x, k) => { return eat(x) + k; } Ga.N => { return 0; } } }
fn gout(g: Ga[Array[i64, 4]]) -> Array[i64, 4] { match g { Ga.Y(x, k) => { return x; } Ga.N => { return mki(0); } } }
fn g1(g: G1[Array[i64, 4]]) -> i64 { match g { G1.Y(x) => { return x[0]; } G1.N => { return 0; } } }

fn main() {
    let mut i: i64 = 0;
    while i < 2 {
        { let g = Ga.Y(mki(10 + i), 5); println(f"mix:{ga(g)}"); }
        { let g = Gb.Y(5, mki(20 + i)); println(f"mix2:{gb(g)}"); }
        { let g = Gt.Y(mki(30 + i), mki(40 + i)); println(f"two:{gt(g)}"); }
        { let g = Gs.Y { a: mki(50 + i), n: 5 }; println(f"struct:{gs(g)}"); }
        { let g = Ga.Y(mki(60 + i), 5); println(f"hand:{ghand(g)}"); }
        { let g = Ga.Y(mki(70 + i), 5); let r = gout(g); println(f"out:{r[1]}"); }
        { let g = G1.Y(mki(80 + i)); println(f"single:{g1(g)}"); }
        i = i + 1;
    }
}
"#,
        &[
            "mix:15",
            "mix2:25",
            "two:71",
            "struct:55",
            "hand:65",
            "out:71",
            "single:80",
            "mix:16",
            "mix2:26",
            "two:73",
            "struct:56",
            "hand:66",
            "out:72",
            "single:81",
        ],
        "asan_generic_multi_field_variant_owns_its_boxed_array_payload",
        12,
    );
}

/// B-2026-09-15-3 — the MEMORY half of an `Array[T, N]` local moved into a
/// user ENUM VARIANT CONSTRUCTOR.
///
/// The constructor stood the caller's element drop down
/// (`suppress_array_local_move_into_ctor`) in a loop that ran BEFORE any
/// payload was compiled, while the defensive copy that retraction depends
/// on is emitted by `maybe_defensive_copy_param_arg` several lines into the
/// payload loop. The skip keys on the copy having HAPPENED, so it could not
/// fire that early: the source was retracted anyway and the payload's drop
/// freed buffers the later read still pointed at. The output half is
/// `e2e_array_moved_into_an_enum_ctor_leaves_the_source_readable` in
/// `tests/codegen.rs`, and THAT is the half that catches the dangle — say
/// so plainly, because the split is not the usual one. `karac`'s own
/// emitted IR carries no ASAN instrumentation (only the linked C shim
/// does), so this harness sees `malloc`/`free` BOOKKEEPING — double frees,
/// invalid frees, leaks — and an invalid READ in compiled Kāra is
/// invisible to it. Verified: this fixture passes on the pre-fix compiler,
/// at `KARAC_OPT_LEVEL=0` as well, because ASAN's quarantine keeps the
/// freed block off the reuse path and the stale read returns the old
/// contents intact. valgrind DOES report it (`Invalid read of size 1`).
///
/// What this fixture guards is the LEAK dimension, and it earns its place
/// on a regression measured during the fix rather than on principle. An
/// intermediate version emitted its own defensive copy ahead of the
/// retraction, not noticing that `maybe_defensive_copy_param_arg` already
/// makes one further down the payload loop; every repaired cell then
/// stranded the duplicate — 46 B in 2 blocks per array at `-O0`, output
/// still perfect, codegen twin still green. LeakSanitizer catches exactly
/// that, and nothing else in the suite would have.
///
/// Every cell lets the enum DIE FIRST — an inner block, or a callee that
/// takes it by value — because with the enum outliving the read the
/// program is correct by timing alone and asserts nothing. `nocallee` is
/// the smallest of them: no function call anywhere, the block's own drop is
/// the free.
///
/// TWO SHAPES ARE DELIBERATELY ABSENT, both of which leak identically
/// before and after this row and would make a clean-run fixture impossible
/// to write: a `shared enum` payload strands 48 B plus its elements
/// (B-2026-09-15-10), and a MULTI-field variant strands its boxed array
/// payload the same way, 48 B per array (B-2026-09-15-15). Both are
/// asserted for OUTPUT in the codegen twin, which is the half of them this
/// row does affect.
///
/// FLOORED at 60 allocations for the usual reason: the orphaned buffers are
/// exactly what LLVM deletes when nothing observes them, so a fixture that
/// allocates nothing at `-O2` asserts nothing. The measured run is 186
/// allocs / 186 frees.
#[test]
fn asan_array_moved_into_an_enum_ctor_is_balanced() {
    assert_clean_asan_run_min_allocs(
        r#"
struct P { s: String }
enum Wrp { Full(Array[String, 2]), Empty }
enum G[T] { Y(T), N }
enum Wi { Full(Array[i64, 2]), Empty }
enum Wp { Full(Array[P, 2]), Empty }

fn wlen(w: Wrp) -> i64 { match w { Wrp.Full(x) => { return x[0].len(); } Wrp.Empty => { return 0; } } }
fn glen(g: G[Array[String, 2]]) -> i64 { match g { G.Y(x) => { return x[0].len(); } G.N => { return 0; } } }
fn ilen(w: Wi) -> i64 { match w { Wi.Full(x) => { return x[0]; } Wi.Empty => { return 0; } } }
fn plen(w: Wp) -> i64 { match w { Wp.Full(x) => { return x[0].s.len(); } Wp.Empty => { return 0; } } }
fn takeo(o: Option[Array[String, 2]]) -> i64 { match o { Some(x) => { return x[0].len(); } None => { return 0; } } }
fn viaparam(a: Array[String, 2]) -> i64 {
    { let w = Wrp.Full(a); println(f"param-in:{wlen(w)}"); }
    return a[0].len();
}
fn mka(t: String) -> Array[String, 2] {
    return [f"b153-{t}-aaaaaaaaaaaaaaaaaaaa", f"b153-{t}-bbbbbbbbbbbbbbbbbbbb"];
}

fn main() {
    let mut i: i64 = 0;
    while i < 2 {

        let a1 = mka(f"n{i}");
        { let w = Wrp.Full(a1); match w { Wrp.Full(x) => { println(f"nocallee-in:{x[0].len()}"); } Wrp.Empty => { println("e"); } } }
        println(f"nocallee:{a1[0].len()}");

        let a2 = mka(f"c{i}");
        { let w = Wrp.Full(a2); println(f"callee-in:{wlen(w)}"); }
        println(f"callee:{a2[1].len()}");

        let a3 = mka(f"f{i}");
        let w3 = Wrp.Full(a3);
        println(f"fnscope-in:{wlen(w3)}");
        println(f"fnscope:{a3[0].len()}");

        let a4 = mka(f"g{i}");
        println(f"argpos-in:{wlen(Wrp.Full(a4))}");
        println(f"argpos:{a4[0].len()}");

        let a5 = mka(f"h{i}");
        { let g = G.Y(a5); println(f"generic-in:{glen(g)}"); }
        println(f"generic:{a5[0].len()}");

        let a6 = mka(f"p{i}");
        println(f"param:{viaparam(a6)}");

        let a7: Array[P, 2] = [P { s: f"b153-s{i}-aaaaaaaaaaaaaaaaaaaa" }, P { s: f"b153-s{i}-bbbbbbbbbbbbbbbbbbbb" }];
        { let w = Wp.Full(a7); println(f"structelem-in:{plen(w)}"); }
        println(f"structelem:{a7[1].s.len()}");

        let d1 = mka(f"o{i}");
        { println(f"seeded-in:{takeo(Option.Some(d1))}"); }
        println(f"seeded:{d1[0].len()}");

        let d3: Array[i64, 2] = [i + 7, i + 8];
        { let w = Wi.Full(d3); println(f"scalar-in:{ilen(w)}"); }
        println(f"scalar:{d3[1]}");

        { let w = Wrp.Full(mka(f"v{i}")); println(f"fresh:{wlen(w)}"); }
        i = i + 1;
    }
}
"#,
        &[
            "nocallee-in:28",
            "nocallee:28",
            "callee-in:28",
            "callee:28",
            "fnscope-in:28",
            "fnscope:28",
            "argpos-in:28",
            "argpos:28",
            "generic-in:28",
            "generic:28",
            "param-in:28",
            "param:28",
            "structelem-in:28",
            "structelem:28",
            "seeded-in:28",
            "seeded:28",
            "scalar-in:7",
            "scalar:8",
            "fresh:28",
            "nocallee-in:28",
            "nocallee:28",
            "callee-in:28",
            "callee:28",
            "fnscope-in:28",
            "fnscope:28",
            "argpos-in:28",
            "argpos:28",
            "generic-in:28",
            "generic:28",
            "param-in:28",
            "param:28",
            "structelem-in:28",
            "structelem:28",
            "seeded-in:28",
            "seeded:28",
            "scalar-in:8",
            "scalar:9",
            "fresh:28",
        ],
        "asan_array_moved_into_an_enum_ctor_is_balanced",
        60,
    );
}

/// B-2026-09-15-12 — the MEMORY half of a `-> ref Array[T, N]` method
/// result used in a value position, and the reason that row could not be
/// closed by widening B-2026-09-15-4's filter.
///
/// That widening was tried: it made `h.peek()[0]` compile, and the program
/// then died with `free(): double free detected in tcache 2`. Loading the
/// pointee makes the loaded array a second owner of the element buffers
/// while nothing retracts the original's drop, so a compile-time refusal
/// would have been traded for a runtime abort. The fix routes instead —
/// the borrow is bound to an anonymous ref-local and the index re-dispatched
/// against it — and a view owns nothing, which is what this run asserts.
///
/// THE ROUTING WAS ONLY HALF THE FIX, and this cell is what says which
/// half is which. With the routing alone the program compiles and still
/// dies: `expr_yields_fresh_owned_temp` classified the borrow-returning
/// accessor as a fresh owned temp, so the argument chokepoint freed the
/// element it had just read out of a BORROWED array. Reverting only that
/// second change reproduces it here as
/// `AddressSanitizer: heap-use-after-free … in memcpy` (measured), and
/// glibc reports the same defect in the single-shot spelling as
/// `free(): double free detected in tcache 2`.
///
/// The output half
/// (`test_e2e_ref_array_and_struct_return_used_in_a_value_position` in
/// `tests/codegen.rs`) does notice that revert too, but only as a program
/// that aborts with EMPTY output — which is the same signature as a
/// routing failure. What this cell adds is the name of the defect and the
/// allocation it happened to, which is what makes the next regression
/// diagnosable rather than merely detected.
///
/// `orig:` reads the original field after two borrows of it in the same
/// iteration, so an over-eager free would dangle rather than leak; the loop
/// makes a per-iteration imbalance accumulate instead of hiding in a single
/// teardown; and `free:` is the free-function spelling, correct all along,
/// kept as the oracle the method arm was made to match.
#[test]
fn asan_ref_array_and_struct_return_in_a_value_position_is_balanced() {
    assert_clean_asan_run(
        r#"
struct Pair { a: String, b: String }
struct Hold { arr: Array[String, 2] }
struct Named { p: Pair }
struct Seq { v: Vec[String] }
impl Hold { fn peek(ref self) -> ref Array[String, 2] { return self.arr; } }
impl Named { fn peek(ref self) -> ref Pair { return self.p; } }
impl Seq { fn peek(ref self) -> ref Vec[String] { return self.v; } }
fn peekh(h: ref Hold) -> ref Array[String, 2] { return h.arr; }

fn main() {
    let mut i: i64 = 0;
    while i < 2 {
        let h = Hold { arr: [f"b151212-arr-aaaaaaaaaaaaaaaa-{i}", f"b151212-arr-bbbbbbbbbbbbbbbb-{i}"] };
        println(f"arr:{h.peek()[0]}");
        println(f"free:{peekh(h)[1]}");
        println(f"orig:{h.arr[0]}");

        let n = Named { p: Pair { a: f"b151212-pair-cccccccccccccccc-{i}", b: f"b151212-pair-dddddddddddddddd-{i}" } };
        println(f"fld:{n.peek().a}");

        let q = Seq { v: [f"b151212-vec-eeeeeeeeeeeeeeee-{i}"] };
        println(f"vec:{q.peek()[0]}");

        i = i + 1;
    }
}
"#,
        &[
            "arr:b151212-arr-aaaaaaaaaaaaaaaa-0",
            "free:b151212-arr-bbbbbbbbbbbbbbbb-0",
            "orig:b151212-arr-aaaaaaaaaaaaaaaa-0",
            "fld:b151212-pair-cccccccccccccccc-0",
            "vec:b151212-vec-eeeeeeeeeeeeeeee-0",
            "arr:b151212-arr-aaaaaaaaaaaaaaaa-1",
            "free:b151212-arr-bbbbbbbbbbbbbbbb-1",
            "orig:b151212-arr-aaaaaaaaaaaaaaaa-1",
            "fld:b151212-pair-cccccccccccccccc-1",
            "vec:b151212-vec-eeeeeeeeeeeeeeee-1",
        ],
        "asan_ref_array_and_struct_return_in_a_value_position_is_balanced",
    );
}

#[test]
fn asan_array_struct_field_drops_its_elements() {
    assert_clean_asan_run(
        r#"
fn mk(n: i64) -> Array[String, 2] { return Array[f"item{n}", f"item{n + 1}"]; }
fn mkv(n: i64) -> Array[Vec[i64], 2] {
    let mut x: Vec[i64] = Vec.new();
    x.push(n);
    let mut y: Vec[i64] = Vec.new();
    y.push(n + 1);
    return Array[x, y];
}

struct Holder { a: Array[String, 2] }
struct Nums { ns: Array[i64, 2] }
struct VHold { vs: Array[Vec[i64], 2] }

fn take(h: Holder) -> String { return h.a[0]; }

fn main() {
    let h = Holder { a: mk(7) };
    println(h.a[0]);
    println(h.a[1]);
    println(h.a[0]);

    println(take(Holder { a: mk(3) }));

    let moved = Holder { a: mk(5) };
    let taken = moved.a;
    println(taken[0]);

    let n = Nums { ns: Array[10, 20] };
    println(n.ns[1]);

    let vh = VHold { vs: mkv(2) };
    println(vh.vs[0].len());

    let mut v: Vec[Holder] = Vec.new();
    v.push(Holder { a: mk(1) });
    v.push(Holder { a: mk(3) });
    println(v.len());
}
"#,
        &["item7", "item8", "item7", "item3", "item5", "20", "1", "2"],
        "asan_array_struct_field_drops_its_elements",
    );
}

#[test]
fn asan_fixed_array_element_bodies_are_memory_balanced() {
    const H: &str = "struct S { id: i64, name: String }\n\
             impl Drop for S { fn drop(mut ref self) { println(f\"drop S{self.name}\") } }\n";
    assert_clean_asan_run(
        &format!(
            "{H}fn main() {{ let a: Array[S, 2] = [S {{ id: 1, name: f\"x{{1}}\" }},\n\
             \x20                                     S {{ id: 2, name: f\"y{{2}}\" }}];\n\
             \x20            println(\"mid\"); }}\n"
        ),
        &["drop Sx1", "drop Sy2", "mid"],
        "heap-struct-elems",
    );
    // MOVED ON, un-annotated destination — the source-record fallback. A
    // per-owner widening double-frees here.
    assert_clean_asan_run(
        &format!(
            "{H}fn main() {{ let a: Array[S, 2] = [S {{ id: 1, name: f\"x{{1}}\" }},\n\
             \x20                                     S {{ id: 2, name: f\"y{{2}}\" }}];\n\
             \x20            let b = a; println(\"moved\"); }}\n"
        ),
        &["drop Sx1", "drop Sy2", "moved"],
        "moved-on",
    );
    // ENUM elements whose variant payload owns the heap — the walker's
    // enum leg, reached through `emit_slot_drop_bodies_at` rather than the
    // struct-field one.
    assert_clean_asan_run(
        "enum E { A(String), B }\n\
             impl Drop for E { fn drop(mut ref self) { println(\"drop E\") } }\n\
             fn main() { let a: Array[E, 2] = [E.B, E.A(f\"n{7}\")];\n\
             \x20            println(\"mid\"); }\n",
        &["drop E", "drop E", "mid"],
        "enum-elems-heap-payload",
    );
    // NESTED SCOPE — the inner frame's drain owns both actions.
    assert_clean_asan_run(
        &format!(
            "{H}fn main() {{ {{ let a: Array[S, 1] = [S {{ id: 1, name: f\"x{{1}}\" }}];\n\
             \x20                 println(\"inner\") }}\n\
             \x20            println(\"outer\"); }}\n"
        ),
        &["drop Sx1", "inner", "outer"],
        "nested-scope",
    );
}

#[test]
fn asan_array_of_shared_elements_releases_every_refcount_block() {
    // B-2026-09-15-8 — an `Array[S, N]` of a `shared struct` registered no
    // scope-exit drop at all, so every element's 16-byte refcount block was
    // stranded: 32 B in 2 blocks at `-O0` for a bare local, scaling with N,
    // with no store and no move anywhere in the program.
    //
    // TWO GATES DISAGREED. `synthesize_array_drop_fn_te` — the emitter —
    // already admitted a shared element through its
    // `tuple_elem_needs_deep_drop` disjunct, and
    // `emit_drop_fn_for_type_expr` already answers a shared element with
    // `emit_vec_elem_rc_dec_fn` (B-2026-09-03-36). The walk was built; only
    // `array_elem_owns_callee_drop` — the registrar that arms it — never
    // asked about a shared element, because `type_expr_has_drop_heap`
    // answers `false` for a shared type by design.
    //
    // The cells are the positions where the array stays a LOCAL, which is
    // where the defect lives: a plain local, a whole-array rebind, a
    // by-value argument, and a return. The positions that hand the array to
    // a destination owning its own drop (a container literal, a struct
    // field, an enum constructor, `push`) were clean BEFORE this and are
    // pinned here too, because arming a new drop is exactly how a leak
    // becomes a double release.
    //
    // -O0 ONLY. At the default opt level LLVM deletes the allocation
    // nothing observes and every cell here is clean on the unfixed tree, so
    // the ordinary `--features llvm` run is vacuous for this fixture and
    // `scripts/asan-o0-leg.sh` is where it has teeth.
    assert_clean_asan_run(
        r#"
shared struct S { v: i64 }
struct Holder { a: Array[S, 2] }

fn take(a: Array[S, 2]) -> i64 { return a[0].v; }
fn give(n: i64) -> Array[S, 2] { return [S { v: n }, S { v: n + 1 }]; }

fn main() {
    let n = env.args().len() as i64;
    let a: Array[S, 2] = [S { v: n }, S { v: n + 1 }];
    println(a[0].v);
    let b: Array[S, 2] = [S { v: n }, S { v: n + 1 }];
    let c = b;
    println(c[1].v);
    let d: Array[S, 2] = [S { v: n }, S { v: n + 1 }];
    println(take(d));
    let e = give(n);
    println(e[0].v);
    let f: Array[S, 2] = [S { v: n }, S { v: n + 1 }];
    let h = Holder { a: f };
    println(h.a[0].v);
}
"#,
        &["1", "2", "1", "1", "1"],
        "array_of_shared_elements_releases_every_refcount_block",
    );
}

#[test]
fn asan_array_local_moved_into_a_container_literal_has_one_owner() {
    // B-2026-09-15-2 — a named `Array[T, N]` local moved into an enclosing
    // ARRAY or VEC literal was owned twice: the local keeps the
    // `StructDrop` `make_array_param_callee_owned` gave it, the literal
    // bit-copies its `N` element descriptors and registers its own walk,
    // and both free the same buffers. `free(): double free detected in
    // tcache 2` at exit 134 on every compiled backend against a correct
    // `--interp`, with `karac check` printing `All checks passed` and NO
    // diagnostic of any kind — nothing in the ownership pass models a
    // container literal as a move.
    //
    // The rule was already written in `compile_array_literal`'s own
    // comment ("arming the array drop without suppressing here
    // DOUBLE-FREES"); it had simply never been applied to an array-typed
    // ELEMENT, only to the Vec/String one. `v.push(a)` has disarmed since
    // B-2026-09-13-15, which is what isolated the gap to the two literal
    // positions.
    assert_clean_asan_run(
        r#"
fn mka(t: String) -> Array[String, 2] {
    return [f"{t}-aaaaaaaaaaaaaaaaaaaa", f"{t}-bbbbbbbbbbbbbbbbbbbb"];
}

fn main() {
    let a = mka("arr");
    let n: Array[Array[String, 2], 1] = [a];
    println(n[0][1]);
    let b = mka("vec");
    let v: Vec[Array[String, 2]] = [b];
    println(v.len());
}
"#,
        &["arr-bbbbbbbbbbbbbbbbbbbb", "1"],
        "array_local_moved_into_a_container_literal_has_one_owner",
    );
}

#[test]
fn asan_array_local_read_after_a_container_literal_keeps_its_own_buffers() {
    // B-2026-09-15-2 composing with B-2026-09-14-27: when the ownership
    // pass DOES see the source read again, the literal is handed an
    // independent copy and the source keeps its drop, so the retraction
    // above must NOT also fire. It does not —
    // `suppress_array_local_move_into_ctor` stands down on
    // `uam_copied_sites` — and this cell is what pins the two halves
    // against each other: retracting as well would free nothing and leak
    // the copy, copying without retracting on the aliasing spelling is the
    // double free the sibling test covers.
    assert_clean_asan_run(
        r#"
fn mka(t: String) -> Array[String, 2] {
    return [f"{t}-aaaaaaaaaaaaaaaaaaaa", f"{t}-bbbbbbbbbbbbbbbbbbbb"];
}

fn main() {
    let a = mka("arr");
    let n: Array[Array[String, 2], 1] = [a];
    println(a[0]);
    println(n[0][1]);
}
"#,
        &["arr-aaaaaaaaaaaaaaaaaaaa", "arr-bbbbbbbbbbbbbbbbbbbb"],
        "array_local_read_after_a_container_literal_keeps_its_own_buffers",
    );
}

/// B-2026-08-22-18 — moving a heap ELEMENT out of an owned `Array[T, N]`
/// parameter double-frees it. The callee frees the whole array at exit,
/// including the element it just returned, and the caller frees it again.
///
/// The array literal was simply a missing member of the move-aware
/// element-ownership family the Vec and tuple literals already belong to
/// (B-2026-07-04-1): it never suppressed an f-string element's accumulator
/// cleanup, so that free and the array's owner released the same pointer.
///
/// Found while measuring B-2026-08-21-43, which wants to admit a user impl
/// on a non-scalar `Array` head called on a TEMPORARY. It is not caused by
/// that arm — the BOUND spelling double-frees identically — so widening
/// that gate would only add a second spelling for an already-unsafe
/// lowering. -43 is blocked on this.
#[test]
fn asan_array_elem_moved_out_of_an_owned_param_is_balanced() {
    assert_clean_asan_run(
        r#"fn take_first(a: Array[String, 2]) -> String { return a[0]; }
fn mk(i: i64) -> Array[String, 2] { return [f"a{i}", f"b{i}"]; }

fn main() {
    let mut i = 0i64;
    let mut n = 0i64;
    let mut last: String = "";
    while i < 50i64 {
        last = take_first(mk(i));
        n = n + last.len();
        i = i + 1;
    }
    println(last);
    println(n);
}
"#,
        &["a49", "140"],
        "asan_array_elem_moved_out_of_an_owned_param_is_balanced",
    );
}

/// B-2026-08-21-43 — the TEMPORARY spelling of the same ownership question:
/// a user impl on a non-scalar `Array` head, called on a value with no
/// name (`mk().tag()`). The arm spills the temporary into a slot and
/// re-dispatches by identifier, so what it must not do is create a second
/// owner of the element buffers.
///
/// Paired with the bound twin deliberately: the two spellings have to be
/// balanced the SAME way, and this arm's whole justification is that it
/// inherits the bound path's answer rather than inventing one.
#[test]
fn asan_nonscalar_array_user_impl_on_a_temporary_is_balanced() {
    assert_clean_asan_run(
        r#"trait Tag { fn tag(self) -> i64; }
impl Tag for Array[String, 2] { fn tag(self) -> i64 { return 42i64; } }
fn mk(i: i64) -> Array[String, 2] { return [f"a{i}", f"b{i}"]; }

fn main() {
    let mut i = 0i64;
    let mut total = 0i64;
    while i < 50i64 {
        total = total + mk(i).tag();
        i = i + 1;
    }
    println(total);
}
"#,
        &["2100"],
        "asan_nonscalar_array_user_impl_on_a_temporary_is_balanced",
    );
}

/// The harder half of B-2026-08-21-43: a `String` flows OUT of the consumed
/// temporary. The returned element must survive while its SIBLING is still
/// freed exactly once — the shape where "free the whole array" and "free
/// nothing" are both wrong, in opposite directions. This is the case that
/// double-freed before B-2026-08-22-18, under BOTH spellings.
#[test]
fn asan_nonscalar_array_temp_returning_an_element_frees_only_the_rest() {
    assert_clean_asan_run(
        r#"trait Tag { fn take_first(self) -> String; }
impl Tag for Array[String, 2] { fn take_first(self) -> String { return self[0]; } }
fn mk(i: i64) -> Array[String, 2] { return [f"a{i}", f"b{i}"]; }

fn main() {
    let mut i = 0i64;
    let mut n = 0i64;
    let mut last: String = "";
    while i < 50i64 {
        last = mk(i).take_first();
        n = n + last.len();
        i = i + 1;
    }
    println(last);
    println(n);
}
"#,
        &["a49", "140"],
        "asan_nonscalar_array_temp_returning_an_element_frees_only_the_rest",
    );
}

/// B-2026-08-22-18 follow-up — the NESTED-passing guard for the owned
/// fixed-array element drop. An owned `Array[String, N]` param moved BY VALUE
/// into a second callee (`outer(a)` → `inner(a)`) transfers ownership: the
/// caller's element drop is retracted at the call site
/// (`suppress_array_binding_move_arg`) so the two frames don't both free the
/// shared buffers. Without that retraction this double-freed (leak → double
/// free is strictly worse); without the callee drop it would leak the
/// non-returned element. `inner` returns element 1, so element 0 is freed by
/// `inner`'s drop and element 1 flows out to `last` — each buffer freed once.
#[test]
fn asan_array_owned_param_moved_into_nested_call_is_balanced() {
    assert_clean_asan_run(
        r#"fn inner(a: Array[String, 2]) -> String { return a[1]; }
fn outer(a: Array[String, 2]) -> String { return inner(a); }
fn mk(i: i64) -> Array[String, 2] { return [f"a{i}", f"b{i}"]; }

fn main() {
    let mut i = 0i64;
    let mut n = 0i64;
    let mut last: String = "";
    while i < 50i64 {
        last = outer(mk(i));
        n = n + last.len();
        i = i + 1;
    }
    println(last);
    println(n);
}
"#,
        &["b49", "140"],
        "asan_array_owned_param_moved_into_nested_call_is_balanced",
    );
}

/// B-2026-08-23-4 — a LOCAL fixed array is now owned WHOLE (one array-keyed
/// drop on the slot) rather than element-wise, matching the param path.
///
/// These four exist because the change has a double-free hazard on one side
/// and a leak on the other, and neither shows up in stdout. The array
/// literal BIT-COPIES each element's `{ptr,len,cap}` header, so arming the
/// array drop without disarming the element sources frees each buffer
/// twice; disarming without arming leaks. One shape per element-source kind,
/// because the suppression is per-kind: an f-string accumulator, a named
/// binding moved in, and a call result.
#[test]
fn asan_local_fixed_array_owned_whole_is_balanced_for_every_element_source() {
    // f-string elements — accumulator neutralized by B-2026-08-22-18.
    assert_clean_asan_run(
        r#"fn mk(i: i64) -> Array[String, 2] { return [f"a{i}", f"b{i}"]; }
fn main() {
    let mut i = 0i64; let mut n = 0i64;
    while i < 50i64 { let a = mk(i); n = n + a[0].len(); i = i + 1i64; }
    println(n);
}
"#,
        &["140"],
        "local_fixed_array_fstr_elements",
    );

    // NAMED bindings moved in — the shape where the source's own scope-exit
    // free and the array's drop would both fire without suppression.
    assert_clean_asan_run(
        r#"fn mk(i: i64) -> String { return f"s{i}"; }
fn main() {
    let mut i = 0i64; let mut n = 0i64;
    while i < 50i64 {
        let x = mk(i);
        let y = mk(i);
        let a: Array[String, 2] = [x, y];
        n = n + a[0].len() + a[1].len();
        i = i + 1;
    }
    println(n);
}
"#,
        &["280"],
        "local_fixed_array_named_binding_elements",
    );

    // CALL-RESULT elements — fresh temps with no independent owner.
    assert_clean_asan_run(
        r#"fn mk(i: i64) -> String { return f"s{i}"; }
fn main() {
    let mut i = 0i64; let mut n = 0i64;
    while i < 50i64 {
        let a: Array[String, 2] = [mk(i), mk(i)];
        n = n + a[0].len();
        i = i + 1;
    }
    println(n);
}
"#,
        &["140"],
        "local_fixed_array_call_result_elements",
    );
}

/// The ESCAPE case, kept separate because it rules out the opposite error.
/// An array built from named bindings and RETURNED out of its function must
/// not have its slot drop fire — the caller owns it now. Arming the local
/// drop without suppressing it on the escape path is a use-after-free, not
/// a leak, so this is the shape that would catch that.
#[test]
fn asan_local_fixed_array_returned_out_is_not_dropped_twice() {
    assert_clean_asan_run(
        r#"fn mk(i: i64) -> Array[String, 2] {
    let x = f"a{i}";
    let y = f"b{i}";
    let a: Array[String, 2] = [x, y];
    return a;
}
fn main() {
    let mut i = 0i64; let mut n = 0i64;
    while i < 50i64 { let r = mk(i); n = n + r[0].len(); i = i + 1i64; }
    println(n);
}
"#,
        &["140"],
        "local_fixed_array_returned_out",
    );
}

/// B-2026-08-24-5 — the same escape as the fixture above, but reached
/// through the OTHER return position: a bare TAIL expression with no
/// `return` keyword. The whole-array drop retraction lives at two sites
/// (`suppress_cleanup_for_tail_return` and the `ExprKind::Return` arm in
/// `exprs.rs`), and hooking only one leaves the other double-freeing, so
/// each gets a fixture rather than trusting one to stand for both.
#[test]
fn asan_local_fixed_array_tail_expression_is_not_dropped_twice() {
    assert_clean_asan_run(
        r#"fn mk(i: i64) -> Array[String, 2] {
    let x = f"a{i}";
    let y = f"b{i}";
    let a: Array[String, 2] = [x, y];
    a
}
fn main() {
    let mut i = 0i64; let mut n = 0i64;
    while i < 50i64 { let r = mk(i); n = n + r[0].len(); i = i + 1i64; }
    println(n);
}
"#,
        &["140"],
        "local_fixed_array_tail_expression",
    );
}

/// B-2026-08-24-5 — a MID-BODY `return a;` (inside an `if`, with other
/// statements after it), which routes through the `ExprKind::Return` arm
/// rather than the tail walk. Without the retraction there this frame frees
/// the element buffers that the caller also owns.
#[test]
fn asan_local_fixed_array_conditional_return_is_not_dropped_twice() {
    assert_clean_asan_run(
        r#"fn mk(i: i64) -> Array[String, 2] {
    let x = f"a{i}";
    let y = f"b{i}";
    let a: Array[String, 2] = [x, y];
    if i >= 0i64 {
        return a;
    }
    let p = f"p{i}";
    let q = f"q{i}";
    let b: Array[String, 2] = [p, q];
    b
}
fn main() {
    let mut i = 0i64; let mut n = 0i64;
    while i < 50i64 { let r = mk(i); n = n + r[0].len(); i = i + 1i64; }
    println(n);
}
"#,
        &["140"],
        "local_fixed_array_conditional_return",
    );
}

/// B-2026-08-24-5 — the PARAM sibling: an owned `Array[T, N]` PARAMETER
/// returned straight back out. `make_array_param_callee_owned` has armed a
/// slot drop for array params since well before the local-array
/// registration existed, so this shape reaches the same missing retraction
/// by an older route. Kept as a standing fixture so the param path cannot
/// regress independently of the local one.
#[test]
fn asan_owned_array_param_returned_out_is_not_dropped_twice() {
    assert_clean_asan_run(
        r#"fn pass(a: Array[String, 2]) -> Array[String, 2] { return a; }
fn mk(i: i64) -> Array[String, 2] {
    let x = f"a{i}";
    let y = f"b{i}";
    let a: Array[String, 2] = [x, y];
    return a;
}
fn main() {
    let mut i = 0i64; let mut n = 0i64;
    while i < 50i64 { let r = pass(mk(i)); n = n + r[0].len(); i = i + 1i64; }
    println(n);
}
"#,
        &["140"],
        "owned_array_param_returned_out",
    );
}

/// B-2026-09-04-34 — a moved-out `Array[E, N]` FIELD frees its element
/// buffers exactly once, and leaks none of them.
///
/// The abort half of this defect is pinned by value in
/// `test_e2e_array_field_move_out_disarms_the_source` (tests/codegen.rs): a
/// `let x = h.a;` off `struct Ha { a: Array[R, 2] }` double-freed the
/// element `String`s at scope exit, because every arm of
/// `suppress_struct_field_move_by_name` matches a `StructType` field and an
/// array field is an `ArrayType` — so the move suppressed nothing and the
/// source's drop kept freeing what the binding now owned.
///
/// This is the quiet half. A neutralizer that disarms the source is one
/// symmetric mistake away from disarming BOTH owners, which frees nothing
/// and shows up as a leak rather than an abort — the trade the `Vec` cap
/// arm names ("skipping the disarm where no copy was made leaves two owners
/// of one buffer", and doing it twice leaves none). LSan on the Linux CI leg
/// is what catches that direction; the abort test cannot.
///
/// THE SCALAR CELL IS THE OVER-REACH CONTROL. `Array[i64, 2]` elements own
/// nothing, so the walk must emit no store at all; a rule keyed on the
/// array HEAD rather than on the element type still passes the two heap
/// cells and only shows up here.
#[test]
fn asan_array_field_move_out_frees_once() {
    assert_clean_asan_run(
        "struct R { id: i64, tag: String }\n\
             struct Ha { a: Array[R, 2] }\n\
             struct Hs { a: Array[String, 2] }\n\
             struct Hn { a: Array[i64, 2] }\n\
             fn mk(n: i64) -> R { return R { id: n, tag: f\"tag{n}\" }; }\n\
             fn structElem() {\n\
             \x20   let h = Ha { a: [mk(2), mk(3)] };\n\
             \x20   let x = h.a;\n\
             \x20   let e = ref x[0];\n\
             \x20   println(f\"se{e.id}:{e.tag}\")\n\
             }\n\
             fn strElem() {\n\
             \x20   let h = Hs { a: [f\"one\", f\"two\"] };\n\
             \x20   let x = h.a;\n\
             \x20   println(f\"st{x[0]}:{x[1]}\")\n\
             }\n\
             fn scalarElem() {\n\
             \x20   let h = Hn { a: [7, 9] };\n\
             \x20   let x = h.a;\n\
             \x20   println(f\"sc{x[0]}:{x[1]}\")\n\
             }\n\
             fn annotated() {\n\
             \x20   let h = Ha { a: [mk(4), mk(5)] };\n\
             \x20   let x: Array[R, 2] = h.a;\n\
             \x20   println(f\"an{x[1].id}:{x[1].tag}\")\n\
             }\n\
             fn main() {\n\
             \x20   structElem();\n\
             \x20   strElem();\n\
             \x20   scalarElem();\n\
             \x20   annotated();\n\
             \x20   println(\"end\")\n\
             }\n",
        &["se2:tag2", "stone:two", "sc7:9", "an5:tag5", "end"],
        "b0904-34-array-field-move-out",
    );
}

/// B-2026-09-12-12 / B-2026-09-12-18 — a boxed `Array[T, N]` enum payload,
/// across the two axes that each produced a distinct defect in this family.
///
/// The BOX had no owner for a MONOMORPHIC enum.
/// `user_enum_boxed_payload_variants` returns every variant whose payload
/// outgrows its area, but bailed on a non-generic path because "only a
/// generic instantiation can outgrow its own area". An array refutes that:
/// the under-sizing comes from the SPELLING a variant declaration is forced
/// to use — `Path(["Array"], ..)`, which `payload_word_count_for_type_expr`
/// sizes through its conservative tail, while the real-width
/// `TypeKind::Array` arm only ever sees a type recovered from inference. So
/// `enum E { A(Array[String, 2]), B }` boxed exactly like an erased `T` and
/// leaked 384 B direct plus 384 B indirect over 8 rounds; `Array[i64, 3]`,
/// which owns no heap at all, leaked 192 B purely to the box.
///
/// The INTERIOR walk double-freed a payload moved out of a LOCAL. An array
/// local moved into an enum payload keeps its own `StructDrop` plus a
/// `FreeVecBuffer` per element — no array move-out disarm is wired to that
/// site — so the source freed the elements and the walk freed them again:
/// SIGABRT for `String`, struct, `Drop`-bearing and nested-array elements,
/// SIGSEGV for `Vec[String]`. The `l*` cells below are that shape, and the
/// whole fixture aborts on the pre-fix compiler (measured: rc=134).
///
/// `g5` is the INLINE spelling, which was always correct and must stay
/// walked — it is the cell that fails if the gate is turned off rather than
/// narrowed. `m2` uses the UNQUALIFIED constructor and arm pattern while
/// every other cell is qualified, because that axis is exactly what the
/// generated matrix preceding this fix failed to vary.
///
/// All-`Array` on purpose and no user `Drop` bodies: this asserts MEMORY, so
/// no cell can pass for a body-count reason.
///
/// Each `l*` cell READS a byte out of the local before moving it. Without
/// that the fixture tripped its own vacuity floor at 77 allocations — the
/// optimizer deleted roughly half the payload strings because nothing
/// observed them, and a clean ASAN run over allocations that never happened
/// proves nothing. The read is taken BEFORE the move, deliberately: reading
/// through a match arm instead would hand the interior to the binding and
/// mask the very ownership overlap these cells exist to pin.
#[test]
fn asan_enum_boxed_array_payload_frees_its_box_and_not_its_source() {
    let mut expected: Vec<&str> = Vec::new();
    for _ in 0..8 {
        expected.extend_from_slice(&[
            "g1:true", "g2:true", "g3:1", "g4", "g5", "m1", "m2:true", "m3",
        ]);
    }
    expected.push("end");
    assert_clean_asan_run_min_allocs(
        r#"
struct Pr { a: String, b: i64 }
enum G[T] { Y(T), N }
enum E { A(Array[String, 2]), B }
enum Ei { Ai(Array[i64, 3]), Bi }

fn main() {
    let n = env.args().len() as i64;
    let mut i: i64 = 0i64;
    while i < 8i64 {
        let l1: Array[String, 2] = [f"row-cccccccccccc-{i}-{n}", f"row-dddddddddddd-{i}-{n}"];
        let r1 = l1[1].contains("row");
        let g1: G[Array[String, 2]] = G.Y(l1);
        println(f"g1:{r1}");
        let l2: Array[Pr, 2] = [Pr { a: f"row-eeeeeeeeeeee-{i}-{n}", b: i }, Pr { a: f"row-ffffffffffff-{i}-{n}", b: i }];
        let r2 = l2[1].a.contains("row");
        let g2: G[Array[Pr, 2]] = G.Y(l2);
        println(f"g2:{r2}");
        let l3: Array[Vec[String], 2] = [[f"row-gggggggggggg-{i}-{n}"], [f"row-hhhhhhhhhhhh-{i}-{n}"]];
        let r3 = l3[1].len();
        let g3: G[Array[Vec[String], 2]] = G.Y(l3);
        println(f"g3:{r3}");
        let l4: Array[Array[String, 2], 2] = [[f"row-iiiiiiiiiiii-{i}-{n}", f"row-jjjjjjjjjjjj-{i}-{n}"], [f"row-kkkkkkkkkkkk-{i}-{n}", f"row-llllllllllll-{i}-{n}"]];
        let g4: G[Array[Array[String, 2], 2]] = G.Y(l4);
        println("g4");
        let g5: G[Array[String, 2]] = G.Y([f"row-mmmmmmmmmmmm-{i}-{n}", f"row-nnnnnnnnnnnn-{i}-{n}"]);
        println("g5");
        let m1: E = E.A([f"row-oooooooooooo-{i}-{n}", f"row-pppppppppppp-{i}-{n}"]);
        println("m1");
        let m2: E = A([f"row-qqqqqqqqqqqq-{i}-{n}", f"row-rrrrrrrrrrrr-{i}-{n}"]);
        println(f"m2:{match m2 { A(t) => { t[1].contains("row") } B => { false } }}");
        let m3: Ei = Ei.Ai([i, i + n, i * 2i64]);
        println("m3");
        i = i + 1i64;
    }
    println("end");
}
"#,
        &expected,
        "asan_enum_boxed_array_payload_frees_its_box_and_not_its_source",
        // 93 measured at the default level post-fix. Not a guess about what
        // the optimizer keeps: this fixture's real proof is that the exact
        // program aborts on the pre-fix compiler (rc=134, `free(): double
        // free detected in tcache 2`) and runs valgrind-clean on this one,
        // and the floor only has to catch a version folded away entirely,
        // which reaches ~10.
        60,
    );
}

#[test]
fn asan_nested_indexed_read_on_an_array_outer_owns_its_elements() {
    // 1 — the row's own shape: an `Array[Vec[String], N]` bound out of an
    //     `Option` arm and read two levels deep.
    assert_clean_asan_run(
            "fn plainV(x: Option[Array[Vec[String], 2]]) {\n\
             \x20   match x { Some(t) => { println(f\"s:{t[0][0]}\") } None => { println(\"n\") } }\n\
             }\n\
             fn main() {\n\
             \x20   let a: Array[Vec[String], 2] = [[f\"aaaaaaaa0\", f\"aaaaaaaa1\"], [f\"bbbbbbbb0\"]];\n\
             \x20   plainV(Some(a));\n\
             }\n",
            &["s:aaaaaaaa0"],
            "b9-arm-array-vec-string",
        );
    // 2 — no arm at all. A plain annotated `let` failed identically, so the
    //     arm was never the discriminator and this cell is the base case.
    assert_clean_asan_run(
            "fn main() {\n\
             \x20   let a: Array[Vec[String], 2] = [[f\"aaaaaaaa0\", f\"aaaaaaaa1\"], [f\"bbbbbbbb0\"]];\n\
             \x20   println(f\"s:{a[0][0]}\");\n\
             }\n",
            &["s:aaaaaaaa0"],
            "b9-annotated-let-base",
        );
    // 3 — the array crosses a call boundary as an owned param, so the
    //     callee frame is the one doing the two-level read.
    assert_clean_asan_run(
            "fn takes(a: Array[Vec[String], 2]) { println(f\"s:{a[0][0]}\"); }\n\
             fn main() {\n\
             \x20   let a: Array[Vec[String], 2] = [[f\"aaaaaaaa0\", f\"aaaaaaaa1\"], [f\"bbbbbbbb0\"]];\n\
             \x20   takes(a);\n\
             }\n",
            &["s:aaaaaaaa0"],
            "b9-fn-param-base",
        );
    // 4 — both levels arrays. No heap at all, so this one is about the
    //     synth minted for the inner element not outliving its owner.
    assert_clean_asan_run(
        "fn main() {\n\
             \x20   let a: Array[Array[i64, 2], 2] = [[10, 11], [20, 21]];\n\
             \x20   println(f\"s:{a[1][0]}\");\n\
             }\n",
        &["s:20"],
        "b9-array-of-array",
    );
}

/// B-2026-09-09-23 — an annotated rebind of an `Array[T, N]` with a
/// heap-owning element gave the destination a second memory drop over the
/// source's elements. ASAN is the gate that matters here: the failure is a
/// double free, and `-O2` hid it (the optimizer deletes buffers nothing
/// observes), so only an unoptimized run and the JIT ever aborted.
///
/// The fix REMOVES a drop, so cells 4-6 carry the leak direction: a bare
/// rebind, a scalar element and the `Vec` container all still have to be
/// freed exactly once by whoever owns them.
#[test]
fn asan_array_rebind_leaves_memory_with_one_owner() {
    // 1 — the minimal reproducer: no index, no read, no call.
    assert_clean_asan_run(
        "fn main() {\n\
             \x20   let a: Array[Vec[i64], 2] = [[10, 11], [20]];\n\
             \x20   let b: Array[Vec[i64], 2] = a;\n\
             \x20   println(\"done\");\n\
             }\n",
        &["done"],
        "b23-annotated-rebind-vec-element",
    );
    // 2 — a user struct element, the spelling B-2026-08-28-57's comment
    //     measured when it drew the bodies/memory line.
    assert_clean_asan_run(
        "struct S { s: String }\n\
             fn main() {\n\
             \x20   let a: Array[S, 2] = [S { s: f\"aaaaaaaa0\" }, S { s: f\"bbbbbbbb1\" }];\n\
             \x20   let b: Array[S, 2] = a;\n\
             \x20   println(\"done\");\n\
             }\n",
        &["done"],
        "b23-annotated-rebind-struct-element",
    );
    // 3 — the destination is read afterwards, so the stand-down has to
    //     leave a LIVE array and not merely a balanced one.
    assert_clean_asan_run(
        "fn main() {\n\
             \x20   let a: Array[Vec[i64], 2] = [[10, 11], [20]];\n\
             \x20   let b: Array[Vec[i64], 2] = a;\n\
             \x20   println(f\"s:{b[0][1]}\");\n\
             }\n",
        &["s:11"],
        "b23-annotated-rebind-then-read",
    );
    // 4 — CONTROL, leak direction: the bare rebind never registered the
    //     destination, and the source must still free its elements.
    assert_clean_asan_run(
        "struct S { s: String }\n\
             fn main() {\n\
             \x20   let a: Array[S, 2] = [S { s: f\"aaaaaaaa0\" }, S { s: f\"bbbbbbbb1\" }];\n\
             \x20   let b = a;\n\
             \x20   println(\"done\");\n\
             }\n",
        &["done"],
        "b23-bare-rebind-control",
    );
    // 5 — CONTROL: B-2026-09-09-9's rebind read, admitted here now that
    //     its blocker is gone.
    assert_clean_asan_run(
        "fn main() {\n\
             \x20   let a: Array[Vec[i64], 2] = [[10, 11], [20]];\n\
             \x20   let b = a;\n\
             \x20   println(f\"s:{b[0][1]}\");\n\
             }\n",
        &["s:11"],
        "b23-bare-rebind-then-read",
    );
    // 6 — CONTROL: the `Vec` container, whose move disarms the source's
    //     cap and which must keep freeing exactly once.
    assert_clean_asan_run(
        "fn main() {\n\
             \x20   let v: Vec[Vec[i64]] = [[10, 11], [20]];\n\
             \x20   let w: Vec[Vec[i64]] = v;\n\
             \x20   println(\"done\");\n\
             }\n",
        &["done"],
        "b23-vec-container-control",
    );
}

/// B-2026-09-10-4 — the `match`-arm-bound sibling of
/// [`asan_array_rebind_leaves_memory_with_one_owner`]. -23 removed the
/// duplicate owner a `let`-bound array's rebind took, and keyed the
/// stand-down on `owned_array_params` — the set the two
/// `make_array_param_callee_owned` call sites populate. An arm-bound
/// `Array` payload is in neither, because the ARM frees it, so
/// `Some(t) => { let u: Array[String, 2] = t; … }` walked past the guard
/// and took a second drop over elements the arm still owns.
///
/// THE ANNOTATION IS WHAT MADE THIS LIVE ON `main`, not a held-back read.
/// The row that filed it recorded the un-annotated spelling, which refused
/// to build for an unrelated reason (no element type resolved for the
/// destination, so the nested read had nothing to index through) and read
/// as "deliberately held back". The ANNOTATED spelling needs no such
/// resolution: it built, it ran, and it corrupted. On a pre-fix tree cells
/// 1 and 3 abort `free(): double free detected in tcache 2` under the JIT
/// and at `KARAC_OPT_LEVEL=0`, and are clean at `-O2` — the optimizer
/// deletes buffers nothing observes, so the default `karac build` passed
/// and only `karac run`, the first thing anyone tries, ever aborted.
///
/// CELL 2 IS THE WORST OF THE THREE AND LOOKS THE MILDEST. Two levels of
/// heap under the element, and it prints NOTHING AT ALL on every compiled
/// surface — not the `println`, not an abort message — while `--interp`
/// prints `held`. valgrind reports 11 errors, invalid READS as well as
/// invalid frees, at `-O2` and `-O0` alike. It is also the cell this
/// fixture actually catches, since the ASAN harness builds at `-O2` where
/// cells 1 and 3 are clean.
///
/// Cells 7-9 carry the leak direction, since the fix REMOVES a drop.
/// B-2026-09-06-49 / B-2026-09-10-6 — a boxed `Array` payload of a seeded
/// `Option`/`Result` has EXACTLY ONE owner of its interior, in each of the
/// four places the payload can come from and go to.
///
/// The two rows are one defect seen from its two ends, and neither is
/// visible without the other. An `Array[String, 2]` payload is 6 words, so
/// it boxes; the box's drop was box-ONLY, exactly the "box reclaimed,
/// contents not" signature B-2026-09-04-12 fixed for a TUPLE payload. What
/// hid it is that a NAMED array local carries its own `StructDrop` from
/// `make_array_param_callee_owned` and nothing stood that down at the move
/// into the variant, so it was an ACCIDENTAL owner:
///
///   f(Some([f"a", f"b"]))       nobody owns the interior  -> 54 B in 6
///                               blocks over three calls (-49)
///   let p = [..]; f(Some(p))    the caller's local owns it -> clean, and
///                               that accident is what made -49 look like
///                               an inline-literal-only bug
///   .. plus `Some(t) => take(t)` a second owner appears     -> `free():
///                               double free detected in tcache 2` (-6)
///
/// So the fix cannot be one-sided: giving the box its interior without
/// retracting the caller converts every named cell into -6, and retracting
/// the caller without giving the box its interior converts every named cell
/// into -49. Both directions were measured on the way here.
///
/// THE RETRACTION IS GATED ON THE CALLEE, and that gate is the whole
/// correction to this fix's first shape. Written at the variant-CONSTRUCTOR
/// site it is unconditional, and that site cannot see who consumes the
/// value it builds — so it also disarmed the caller for a GENERIC callee,
/// whose monomorph never reaches `compile_function`'s registration and
/// supplies nothing in its place. `fn takesOpt[T: Display](x: Option[T])`
/// is CLEAN on `main` and leaks 54 B under that form; cells 8 and 9 are the
/// controls that keep it that way.
///
/// THE ARM-SIDE HANDOVER IS NOT THE TUPLE'S. A tuple payload retracts at
/// the arm via `binding_only_borrowed`, whose own doc records that it
/// models a free-function argument as entry-copied and NON-consuming —
/// right for a user `Drop` BODY, which the callee never runs, and wrong for
/// MEMORY, which an owned array param does take. Widening that predicate
/// instead retracts on `let u = t` too, where the rebind creates no owner
/// at all (B-2026-09-10-4 registers an arm-bound `Array` in the type tables
/// and deliberately in no memory table): measured as a fresh 72 B leak on
/// the read-only rebind cell, which is why the retraction lives at the
/// ARGUMENT site, where a second owner actually appears.
///
/// `-O2` IS THE WRONG PLACE TO MEASURE ANY OF THIS and this harness builds
/// there, so the leak cells are carried by the `-O0` ratchet leg
/// (`scripts/asan-o0-leg.sh`) that runs this same suite; at `-O2` LLVM
/// deletes the buffers nothing observes and every leak cell here is
/// vacuously green. What this fixture catches at `-O2` is the double-free
/// half, which aborts at every level.
#[test]
fn asan_boxed_array_payload_interior_has_exactly_one_owner() {
    // 1 — -49 proper: an inline literal payload, nobody owning the two
    //     `String`s. 54 B in 6 blocks over three calls before the fix.
    assert_clean_asan_run(
            "fn plainA(x: Option[Array[String, 2]]) -> i64 {\n\
             \x20   match x { Some(t) => { println(f\"s:{t[0]}\"); 1 } None => { println(\"n\"); 0 } }\n\
             }\n\
             fn main() {\n\
             \x20   let mut s = 0;\n\
             \x20   for i in 0..3 {\n\
             \x20       s = s + plainA(Some([f\"aaaaaaaa{i}\", f\"bbbbbbbb{i}\"]));\n\
             \x20   }\n\
             \x20   println(f\"n:{s}\");\n\
             }\n",
            &["s:aaaaaaaa0", "s:aaaaaaaa1", "s:aaaaaaaa2", "n:3"],
            "b49-inline-literal-payload-param",
        );
    // 2 — the NAMED-local spelling of cell 1. Clean before the fix by
    //     accident, and the cell that turns into a double free if the box
    //     is given the interior without retracting the caller's local.
    assert_clean_asan_run(
            "fn plainA(x: Option[Array[String, 2]]) -> i64 {\n\
             \x20   match x { Some(t) => { println(f\"s:{t[0]}\"); 1 } None => { println(\"n\"); 0 } }\n\
             }\n\
             fn main() {\n\
             \x20   let mut s = 0;\n\
             \x20   for i in 0..3 {\n\
             \x20       let p: Array[String, 2] = [f\"aaaaaaaa{i}\", f\"bbbbbbbb{i}\"];\n\
             \x20       s = s + plainA(Some(p));\n\
             \x20   }\n\
             \x20   println(f\"n:{s}\");\n\
             }\n",
            &["s:aaaaaaaa0", "s:aaaaaaaa1", "s:aaaaaaaa2", "n:3"],
            "b49-named-local-payload-param",
        );
    // 3 — -6 proper: the arm's whole-payload binding handed BY VALUE to a
    //     callee that copies it in and frees it at scope exit. Aborts
    //     `free(): double free detected in tcache 2` before the fix, 14
    //     frees against 12 allocs.
    assert_clean_asan_run(
        "fn take(a: Array[String, 2]) { println(f\"t:{a[0]}\") }\n\
             fn plainP(x: Option[Array[String, 2]]) {\n\
             \x20   match x { Some(t) => { take(t) } None => { println(\"n\") } }\n\
             }\n\
             fn main() {\n\
             \x20   let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
             \x20   plainP(Some(a));\n\
             }\n",
        &["t:aaaaaaaa0"],
        "b6-arm-binding-passed-by-value",
    );
    // 4 — the same consumer reached through a REBIND. The alias has to hop
    //     `t -> u`, or the arg site cannot find the box to stand down.
    assert_clean_asan_run(
        "fn take(a: Array[String, 2]) { println(f\"t:{a[0]}\") }\n\
             fn plainM(x: Option[Array[String, 2]]) {\n\
             \x20   match x { Some(t) => { let u = t; take(u) } None => { println(\"n\") } }\n\
             }\n\
             fn main() {\n\
             \x20   let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
             \x20   plainM(Some(a));\n\
             }\n",
        &["t:aaaaaaaa0"],
        "b6-arm-binding-rebound-then-passed",
    );
    // 5 — the LET site, which has its own registration and needed the same
    //     pair. 18 B at `-O0` before the fix for the inline spelling, and a
    //     double free for the named one once the consumer is added.
    assert_clean_asan_run(
        "fn take(a: Array[String, 2]) { println(f\"t:{a[0]}\") }\n\
             fn main() {\n\
             \x20   let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
             \x20   let o = Some(a);\n\
             \x20   match o { Some(t) => { take(t) } None => { println(\"n\") } }\n\
             }\n",
        &["t:aaaaaaaa0"],
        "b6-let-site-named-payload-consumed",
    );
    // 6 — the `Result` twin of cell 5. The let site derives its payload
    //     through `option_generic_arg_type_expr`, which answers `None` for
    //     a `Result`, so this side needed a per-VARIANT derivation rather
    //     than the `Option`-shaped one beside it.
    assert_clean_asan_run(
        "fn take(a: Array[String, 2]) { println(f\"t:{a[0]}\") }\n\
             fn main() {\n\
             \x20   let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
             \x20   let o: Result[Array[String, 2], i64] = Ok(a);\n\
             \x20   match o { Ok(t) => { take(t) } Err(e) => { println(f\"e:{e}\") } }\n\
             }\n",
        &["t:aaaaaaaa0"],
        "b6-let-site-result-ok-payload-consumed",
    );
    // 7 — the `Err` SIDE carries a payload of its own, and both sides of a
    //     `Result` register against one slot. `BoxedEnumDrop`'s tag guard
    //     is what keeps exactly one of them firing.
    assert_clean_asan_run(
        "fn take(a: Array[String, 2]) { println(f\"t:{a[0]}\") }\n\
             fn plainE(x: Result[i64, Array[String, 2]]) {\n\
             \x20   match x { Ok(v) => { println(f\"v:{v}\") } Err(t) => { take(t) } }\n\
             }\n\
             fn main() {\n\
             \x20   let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
             \x20   plainE(Err(a));\n\
             }\n",
        &["t:aaaaaaaa0"],
        "b6-result-err-side-payload-consumed",
    );
    // 8 — GENERIC CALLEE CONTROL, named payload. Clean on `main` and the
    //     cell an unconditional caller-side retraction leaks 54 B on: the
    //     monomorph registers nothing to replace what was retracted.
    assert_clean_asan_run(
            "fn takesOpt[T: Display](x: Option[T]) -> i64 {\n\
             \x20   match x { Some(t) => { println(f\"s:{t}\"); 1 } None => { println(\"n\"); 0 } }\n\
             }\n\
             fn main() {\n\
             \x20   let mut s = 0;\n\
             \x20   for i in 0..3 {\n\
             \x20       let p: Array[String, 2] = [f\"aaaaaaaa{i}\", f\"bbbbbbbb{i}\"];\n\
             \x20       s = s + takesOpt(Some(p));\n\
             \x20   }\n\
             \x20   println(f\"n:{s}\");\n\
             }\n",
            &[
                "s:[aaaaaaaa0, bbbbbbbb0]",
                "s:[aaaaaaaa1, bbbbbbbb1]",
                "s:[aaaaaaaa2, bbbbbbbb2]",
                "n:3",
            ],
            "b49-generic-callee-named-control",
        );
    // 9 — the generic control with a WILDCARD arm, which binds nothing and
    //     so cannot be covered by any arm-level retraction.
    assert_clean_asan_run(
        "fn takesOpt[T: Display](x: Option[T]) -> i64 {\n\
             \x20   match x { Some(_) => { println(\"s\"); 1 } None => { println(\"n\"); 0 } }\n\
             }\n\
             fn main() {\n\
             \x20   let mut s = 0;\n\
             \x20   for i in 0..3 {\n\
             \x20       let p: Array[String, 2] = [f\"aaaaaaaa{i}\", f\"bbbbbbbb{i}\"];\n\
             \x20       s = s + takesOpt(Some(p));\n\
             \x20   }\n\
             \x20   println(f\"n:{s}\");\n\
             }\n",
        &["s", "s", "s", "n:3"],
        "b49-generic-callee-wildcard-control",
    );
    // 10 — a `ref` param takes no ownership, so the box must KEEP its
    //      interior here. The mirror of cell 3, and the cell that fails if
    //      the arg-site retraction forgets to check the borrow flags.
    assert_clean_asan_run(
        "fn peek(a: ref Array[String, 2]) { println(f\"p:{a[0]}\") }\n\
             fn plainB(x: Option[Array[String, 2]]) {\n\
             \x20   match x { Some(t) => { peek(t) } None => { println(\"n\") } }\n\
             }\n\
             fn main() {\n\
             \x20   let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
             \x20   plainB(Some(a));\n\
             }\n",
        &["p:aaaaaaaa0"],
        "b6-ref-param-keeps-the-box-owner",
    );
    // 11 — an all-SCALAR element needs no interior drop at all, and
    //      `option_payload_struct_or_enum_drop_ok` is what keeps it from
    //      getting one.
    assert_clean_asan_run(
        "fn plainI(x: Option[Array[i64, 2]]) {\n\
             \x20   match x { Some(t) => { println(f\"s:{t[0]}\") } None => { println(\"n\") } }\n\
             }\n\
             fn main() {\n\
             \x20   let a: Array[i64, 2] = [7, 9];\n\
             \x20   plainI(Some(a));\n\
             }\n",
        &["s:7"],
        "b49-scalar-element-control",
    );
    // 12 — a user `Drop` element, so the BODIES channel is exercised
    //      alongside the memory one. Bodies follow the move and memory does
    //      not (B-2026-08-28-57), so a fix that conflates them prints the
    //      body twice or not at all.
    //
    //      B-2026-09-10-27 gave this shape its bodies, so the expectation
    //      moved from `s:1` alone to `s:1 / dR1 / dR2`. THE MEMORY SIDE IS
    //      WHAT THIS FIXTURE GUARDS AND IT DID NOT MOVE: ASAN passed both
    //      before and after — the failure that flagged this cell was the
    //      output assertion alone ("ASAN passed, but output mismatched"),
    //      which is exactly the split the two channels are supposed to
    //      keep. ONE body per element, so a later change that runs the
    //      walk twice still fails here.
    assert_clean_asan_run(
            "struct R9 { id: i64 }\n\
             impl Drop for R9 { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             fn plainD(x: Option[Array[R9, 2]]) {\n\
             \x20   match x { Some(t) => { println(f\"s:{t[0].id}\") } None => { println(\"n\") } }\n\
             }\n\
             fn main() {\n\
             \x20   let a: Array[R9, 2] = [R9 { id: 1 }, R9 { id: 2 }];\n\
             \x20   plainD(Some(a));\n\
             }\n",
            &["s:1", "dR1", "dR2"],
            "b49-user-drop-element",
        );
    // 13 — THE ESCAPE CONTROL, and the cell this fix actually tripped over.
    //      `let back = passthru(Some(e))` RECEIVES a box built at the
    //      call's argument site; the named array local `e` upstream is
    //      still its interior's owner, and this `let` has no source to
    //      retract. Registering the interior here anyway makes two owners
    //      — ASAN `attempting double-free`, on every surface including
    //      `-O2`.
    //
    //      The gate is that the RHS must be a seeded variant CONSTRUCTOR,
    //      i.e. that this `let` BUILDS the box rather than receiving one.
    //      `asan_generic_callee_boxed_optres_temp_arg_frees_its_box` cell
    //      5 is the same control, written for this hazard before this row
    //      existed, and it is what caught the miss; this cell keeps the
    //      pairing legible from inside this row's own fixture.
    assert_clean_asan_run(
            "fn passthru[T](x: Option[T]) -> Option[T] { return x; }\n\
             fn takesOpt[T: Display](x: Option[T]) -> i64 {\n\
             \x20   match x { Some(t) => { println(f\"o:{t}\"); 1 } None => { println(\"n\"); 0 } }\n\
             }\n\
             fn main() {\n\
             \x20   let e: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
             \x20   let back = passthru(Some(e));\n\
             \x20   let c = takesOpt(back);\n\
             \x20   println(f\"c{c}\");\n\
             }\n",
            &["o:[aaaaaaaa0, bbbbbbbb0]", "c1"],
            "b49-passthrough-escape-control",
        );
}

/// B-2026-09-17-4 — a NAMED `Array` local moved into an `Option` ctor used to
/// DOUBLE FREE its elements' heap fields on every compiled backend.
///
/// `let a: Array[S, 2] = [..]; let x: Option[Array[S, 2]] = Some(a);` aborted
/// with `free(): double free detected in tcache 2` under jit / `karac build`
/// / `KARAC_AUTO_PAR=0 build`, against a correct `--interp`. No `match`, no
/// call and no index were needed — constructing the envelope and holding it
/// was enough. At `-O0` valgrind reported two `Invalid free()`s, one per
/// element, each naming an already-freed block: the source local's cleanup
/// and the envelope's payload drop taking the same buffers.
///
/// THE CONJUNCTION IS FOUR-WAY and every cell below is one leg removed.
/// `try_compile_enum_variant_at` excluded the SEEDED pair from
/// `suppress_array_local_move_into_ctor`, so the named source kept its
/// element cleanup. That exclusion is b98707ee9's and is NOT simply wrong —
/// its message states the reason, "a SEEDED `Option`/`Result` payload is
/// owned by a different channel this does not arm, so disarming there
/// retracts without arming", and it measured that over `Array[String, 2]`,
/// whose element runs no user `Drop`. For such an element the exclusion is
/// still exactly right, which is what `nodrop/elem` and `bare/string` pin.
/// An element that DOES run a user `Drop` and owns a heap field has a second
/// channel which frees, and there the source had to stand down. So the
/// exclusion is now CONDITIONAL on `elem_te_runs_user_drop` — the predicate
/// the arm channel already uses for this question — rather than lifted.
///
/// `userenum` is the control that located the fault: the same move into
/// `enum W { P(Array[S, 2]), N }` was clean throughout, because a user enum
/// was never excluded. `noheap/elem` (a `Drop` with no heap field),
/// `freshtemp` (no named source) and `localrebind` (no envelope at all) are
/// the other three legs. `arg/seeded` is the same fault in ARGUMENT
/// position, which is the spelling b98707ee9 hooked the disarm at.
///
/// THE LEAK MIRROR IS THE THING TO RE-MEASURE when touching this, and it is
/// `asan_boxed_array_payload_interior_has_exactly_one_owner`'s
/// `b49-generic-callee-named-control` cell: a fix that trades this double
/// free for that leak has moved the bug rather than closed it. That fixture
/// passes at `-O0` with this change, verified as its own run.
///
/// `-O0` IS THE ONLY LEG THAT SEES ANY OF THIS, as for B-2026-09-09-24's
/// sibling fixture: at `-O2` the optimizer deletes these allocations, so a
/// green `--features llvm` run is no evidence either way. The crash,
/// however, aborted at BOTH opt levels before the fix.
///
/// ONE CELL IS PINNED AS IT STANDS RATHER THAN AS IT SHOULD BE. Every
/// envelope cell here runs its element bodies at the CONSTRUCTOR statement
/// rather than at the holder's death — `local/seeded` prints both `dS` lines
/// before `held`, though `x` owns the array until the block ends. That is
/// pre-existing and agreed on all four surfaces (`userenum` printed exactly
/// this before the fix, when it was the clean control), so it is pinned as
/// measured and filed as its own row rather than quietly blessed here.
///
/// Measured on this tree: 0 bytes in 0 blocks at `-O0` under
/// `valgrind --leak-check=full`, and the stdout below is byte-identical
/// across `--interp` / jit / `karac build` / `KARAC_AUTO_PAR=0 karac build`.
#[test]
fn asan_named_array_local_into_seeded_ctor_has_one_owner() {
    assert_clean_asan_run(
        r#"struct S { tag: String }
impl Drop for S { fn drop(mut ref self) { println(f"  dS{self.tag}") } }
struct N { id: i64 }
impl Drop for N { fn drop(mut ref self) { println(f"  dN{self.id}") } }
struct P { tag: String }
enum W { P(Array[S, 2]), N }
fn takes(x: Option[Array[S, 2]]) { println("  in") }
fn takesP(x: Option[Array[P, 2]]) { println("  in") }
fn readsIt(x: Option[Array[S, 2]]) {
    match x { Some(t) => { println(f"  r:{t[0].tag}") } None => { println("  n") } }
}
fn main() {
    println("local/seeded");   { let a: Array[S, 2] = [S { tag: f"aaaaaaaa0" }, S { tag: f"aaaaaaaa1" }]; let x: Option[Array[S, 2]] = Some(a); println("  held") }
    println("arg/seeded");     { let a: Array[S, 2] = [S { tag: f"bbbbbbbb0" }, S { tag: f"bbbbbbbb1" }]; takes(Some(a)) }
    println("arg/read");       { let a: Array[S, 2] = [S { tag: f"cccccccc0" }, S { tag: f"cccccccc1" }]; readsIt(Some(a)) }
    println("userenum");       { let a: Array[S, 2] = [S { tag: f"dddddddd0" }, S { tag: f"dddddddd1" }]; let w: W = W.P(a); println("  held") }
    println("noheap/elem");    { let a: Array[N, 2] = [N { id: 1 }, N { id: 2 }]; let x: Option[Array[N, 2]] = Some(a); println("  held") }
    println("nodrop/elem");    { let a: Array[P, 2] = [P { tag: f"eeeeeeee0" }, P { tag: f"eeeeeeee1" }]; takesP(Some(a)); println("  held") }
    println("bare/string");    { let a: Array[String, 2] = [f"ffffffff0", f"ffffffff1"]; let x: Option[Array[String, 2]] = Some(a); println("  held") }
    println("freshtemp");      { readsIt(Some([S { tag: f"gggggggg0" }, S { tag: f"gggggggg1" }])) }
    println("localrebind");    { let a: Array[S, 2] = [S { tag: f"hhhhhhhh0" }, S { tag: f"hhhhhhhh1" }]; let b: Array[S, 2] = a; println(f"  r:{b[0].tag}") }
    println("end");
}
"#,
        &[
            "local/seeded",
            "  dSaaaaaaaa0",
            "  dSaaaaaaaa1",
            "  held",
            "arg/seeded",
            "  in",
            "  dSbbbbbbbb0",
            "  dSbbbbbbbb1",
            "arg/read",
            "  r:cccccccc0",
            "  dScccccccc0",
            "  dScccccccc1",
            "userenum",
            "  dSdddddddd0",
            "  dSdddddddd1",
            "  held",
            "noheap/elem",
            "  dN1",
            "  dN2",
            "  held",
            "nodrop/elem",
            "  in",
            "  held",
            "bare/string",
            "  held",
            "freshtemp",
            "  r:gggggggg0",
            "  dSgggggggg0",
            "  dSgggggggg1",
            "localrebind",
            "  r:hhhhhhhh0",
            "  dShhhhhhhh0",
            "  dShhhhhhhh1",
            "end",
        ],
        "asan_named_array_local_into_seeded_ctor_has_one_owner",
    );
}

/// B-2026-09-19-58 — the MATCH-SCRUTINEE sibling of
/// `asan_named_array_local_into_seeded_ctor_has_one_owner` above, and the
/// spelling that row's cells never reach.
///
/// `match Option.Some(a) { .. }` over a named `Array` local armed the box's
/// interior walk without retracting the local's own `StructDrop`, so every
/// element buffer had two owners: `free(): double free detected in tcache 2`
/// under the JIT and at `-O0`, with two invalid frees under valgrind and a
/// correct `--interp`. The registration site's own comment asserted no
/// disarm was owed because a fresh-temp scrutinee has "no named source still
/// owning the interior" — true of the ENVELOPE, false of its PAYLOAD.
///
/// `m/str` is the cell that fixes the predicate: its element is a bare
/// `String`, which runs no user `Drop` at all, and it aborted identically.
/// So the retraction is the AGGREGATE one (`array_elem_owns_callee_drop`,
/// "does this array own a drop at all") rather than the callee-keyed one,
/// which excludes a user-`Drop` element on purpose.
///
/// The `b/` cells must stay exactly as they are: `b/noheap` arms no interior
/// walk, `b/fresh` has no named source, `b/let` was already correct, and
/// `b/mono` is a user enum this path declines.
///
/// The INLINE-payload width is deliberately omitted, for the reason the
/// `gensh` and `b-arrenum` omissions elsewhere in this file give: an
/// `Array[S, 1]` payload is three words, fits the `Option` area, never
/// boxes, and still aborts on the `let` spelling as well as this one. It is
/// a different defect on the other side of the same width gate, filed as its
/// own row; carrying it here would redden the leg for something this commit
/// does not touch.
///
/// Measured on this tree: 0 bytes in 0 blocks and 0 errors at `-O0` under
/// `valgrind --leak-check=full`, and the stdout below is byte-identical
/// across `--interp` / jit / `karac build` / `KARAC_AUTO_PAR=0 karac build`.
#[test]
fn asan_named_array_local_into_seeded_match_scrutinee_has_one_owner() {
    assert_clean_asan_run(
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
        &[
            "m/arr",
            "  r:aaaaaaaa0",
            "  dSaaaaaaaa0",
            "  dSaaaaaaaa1",
            "m/wild",
            "  w",
            "m/str",
            "  r:cccccccc0",
            "m/res",
            "  r",
            "  dSdddddddd0",
            "  dSdddddddd1",
            "m/one",
            "  r",
            "  dR1",
            "b/noheap",
            "  r",
            "  dN2",
            "  dN3",
            "b/fresh",
            "  r:gggggggg0",
            "  dSgggggggg0",
            "  dSgggggggg1",
            "b/let",
            "  r",
            "  dShhhhhhhh0",
            "  dShhhhhhhh1",
            "b/mono",
            "  r",
            "end",
        ],
        "asan_named_array_local_into_seeded_match_scrutinee_has_one_owner",
    );
}

/// B-2026-09-19-61 — the BY-VALUE PARAM sibling of
/// `asan_named_array_local_into_seeded_match_scrutinee_has_one_owner` above,
/// where the two owners sit on opposite sides of a FUNCTION BOUNDARY.
///
/// `fn f(a: Array[S, 2]) { match Option.Some(a) { .. } }` armed the box's
/// interior walk over buffers the CALLER still owned. `@main` keeps its
/// `__karac_drop_array_te_S_2` deliberately —
/// `array_param_elem_is_callee_owned` is `array_elem_owns_callee_drop(elem) &&
/// !elem_te_runs_user_drop(elem)`, so a user-`Drop` element fails the second
/// conjunct and the caller retains on purpose (B-2026-09-14-25 / -27 measured
/// the 44 B that retracting there costs) — and the callee freed the same
/// buffers on its way out: `free(): double free detected in tcache 2`, exit
/// 134 at `-O0`.
///
/// NEITHER RETRACT NOR SENTINEL, but DECLINE TO ARM. The family's standing
/// rule is "retract when the source is a BINDING, write a sentinel when it is
/// a FIELD", and a by-value param is a binding — but the caller's action here
/// is deliberately not retractable, because the element's `Drop` BODIES ride a
/// caller-side channel and fire while the caller still holds the value.
/// `seeded_array_payload_stays_with_caller` names the third branch: a param of
/// this function that `owned_array_params` does NOT hold.
///
/// `p/noheap` is the cell that carries the BODIES half, and it is the one an
/// ASAN run can see at all: `N` owns no heap, so it never aborted — it printed
/// `dN2 dN3 dN2 dN3`, each element's body run once by this frame's arm binding
/// and once by the caller's channel. Its single pair here is what the
/// `pattern_binding_seeded_array_payload_stays_with_caller` gate buys; without
/// that gate the memory half alone leaves it doubled.
///
/// `p/str` and `p/gen` must stay: both were ALREADY CLEAN on the unfixed
/// compiler (measured, exit 0), so they pin the two directions this predicate
/// must not widen into — an `Array[String, N]` param IS callee-owned and its
/// disarm works, and a generic callee never armed the walk. `b/local` is
/// B-2026-09-19-58's own shape and pins the non-regression: a LOCAL source
/// must keep arming.
///
/// TWO NEIGHBOURS ARE DELIBERATELY ABSENT because they still abort on this
/// tree and would redden the leg for something this commit does not fix: a
/// USER-enum seeded scrutinee over the same param (`match W.P(a)`, B-2026-09-22-6),
/// and the same param moved into a STRUCT-LITERAL field (`Box2 { v: a }`,
/// B-2026-09-22-7). Both were measured aborting on the UNFIXED compiler too, so
/// neither is this fix's doing.
///
/// Measured on this tree: 0 bytes in 0 blocks and 0 errors at `-O0` under
/// `valgrind --leak-check=full`, and the stdout below is byte-identical across
/// jit / `karac build` / `KARAC_OPT_LEVEL=0 karac build`. `--interp` DIVERGES
/// — it still runs each element body twice, unchanged by this fix and filed as
/// B-2026-09-22-8 — which is why there is no interpreter twin of this fixture.
#[test]
fn asan_array_param_into_seeded_match_scrutinee_stays_with_caller() {
    assert_clean_asan_run(
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
        &[
            "p/arr",
            "  r:aaaaaaaa0",
            "  dSaaaaaaaa0",
            "  dSaaaaaaaa1",
            "p/wild",
            "  w",
            "  dSbbbbbbbb0",
            "  dSbbbbbbbb1",
            "p/str",
            "  r:cccccccc0",
            "p/res",
            "  r",
            "  dSdddddddd0",
            "  dSdddddddd1",
            "p/gen",
            "  r",
            "  dSeeeeeeee0",
            "  dSeeeeeeee1",
            "p/three",
            "  r",
            "  dSffffffff0",
            "  dSffffffff1",
            "  dSffffffff2",
            "p/noheap",
            "  r",
            "  dN2",
            "  dN3",
            "p/nonefirst",
            "  r",
            "  dShhhhhhhh0",
            "  dShhhhhhhh1",
            "b/local",
            "  r",
            "  dSllllllll0",
            "  dSllllllll1",
            "end",
        ],
        "asan_array_param_into_seeded_match_scrutinee_stays_with_caller",
    );
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
/// Measured on this tree: 0 bytes in 0 blocks and 0 errors at `-O0` under
/// `valgrind --leak-check=full`, and the stdout below is byte-identical across
/// jit / `karac build` / `KARAC_OPT_LEVEL=0 karac build`.
/// `--interp` diverges on `u/letelse` alone, where it runs that cell's element
/// bodies twice — unchanged by this fix, which touches codegen only, and the
/// same remainder B-2026-09-22-8 records for the seeded spelling.
#[test]
fn asan_array_param_into_user_enum_match_scrutinee_stays_with_caller() {
    assert_clean_asan_run(
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
        &[
            "u/bind",
            "  r",
            "  dSaaaaaaaa0",
            "  dSaaaaaaaa1",
            "u/wild",
            "  w",
            "  dSbbbbbbbb0",
            "  dSbbbbbbbb1",
            "u/read",
            "  r:cccccccc0",
            "  dScccccccc0",
            "  dScccccccc1",
            "u/iflet",
            "  r",
            "  dSdddddddd0",
            "  dSdddddddd1",
            "u/letelse",
            "  r",
            "  dSeeeeeeee0",
            "  dSeeeeeeee1",
            "u/three",
            "  r",
            "  dSffffffff0",
            "  dSffffffff1",
            "  dSffffffff2",
            "u/noheap",
            "  r",
            "  dN2",
            "  dN3",
            "b/str",
            "  r:gggggggg0",
            "b/local",
            "  r",
            "end",
        ],
        "asan_array_param_into_user_enum_match_scrutinee_stays_with_caller",
    );
}

/// B-2026-09-22-10 (sanitizer twin) — the NAMED-LOCAL spelling of B-2026-09-22-6, which is a
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
fn asan_array_param_into_named_enum_local_match_scrutinee_stays_with_caller() {
    assert_clean_asan_run(
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
        &[
            "l/bind",
            "  r",
            "  dSaaaaaaaa0",
            "  dSaaaaaaaa1",
            "l/wild",
            "  w",
            "  dSbbbbbbbb0",
            "  dSbbbbbbbb1",
            "l/read",
            "  r:cccccccc0",
            "  dScccccccc0",
            "  dScccccccc1",
            "l/iflet",
            "  r",
            "  dSdddddddd0",
            "  dSdddddddd1",
            "l/letelse",
            "  r",
            "  dSeeeeeeee0",
            "  dSeeeeeeee1",
            "l/three",
            "  r",
            "  dSffffffff0",
            "  dSffffffff1",
            "  dSffffffff2",
            "l/noheap",
            "  r",
            "  dN2",
            "  dN3",
            "l/chain",
            "  r",
            "  dShhhhhhhh0",
            "  dShhhhhhhh1",
            "b/stale",
            "  r",
            "  dSrrrrrrrr0",
            "  dSrrrrrrrr1",
            "  dSssssssss0",
            "  dSssssssss1",
            "b/str",
            "  r:gggggggg0",
            "b/local",
            "  r",
            "  dSllllllll0",
            "  dSllllllll1",
            "b/ctl",
            "  r",
            "  dSjjjjjjjj0",
            "  dSjjjjjjjj1",
            "b/fresh",
            "  r",
            "  dSkkkkkkkk0",
            "  dSkkkkkkkk1",
            "end",
        ],
        "asan_array_param_into_named_enum_local_match_scrutinee_stays_with_caller",
    );
}

/// B-2026-09-22-7 (sanitizer twin) — a by-value `Array` param moved into a STRUCT-LITERAL FIELD
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
/// spelling (B-2026-09-22-9) has no such per-field form: its switch frees a
/// variant's whole payload in ONE arm, so standing that arm down would leak the
/// sibling outright, which is why that row is still open.
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
fn asan_array_param_into_struct_literal_field_stays_with_caller() {
    assert_clean_asan_run(
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
        &[
            "s/bind",
            "  in",
            "  d1",
            "  d2",
            "s/two",
            "  in:zz",
            "  d11",
            "  d12",
            "s/three",
            "  in",
            "  d21",
            "  d22",
            "  d23",
            "s/noheap",
            "  in",
            "  dN31",
            "  dN32",
            "s/read",
            "  in:41",
            "  d41",
            "  d42",
            "n/str",
            "  in:s51",
            "n/tuple",
            "  in",
            "  d71",
            "  d72",
            "b/local",
            "  d91",
            "  d92",
            "  in",
            "b/discard",
            "  in",
            "  d101",
            "  d102",
            "b/ctl",
            "  in",
            "  d111",
            "  d112",
            "end",
        ],
        "asan_array_param_into_struct_literal_field_stays_with_caller",
    );
}

/// B-2026-09-17-9 — the LEAK MIRROR its own row's fix walked into, and the
/// shape that fix's controls could not reach.
///
/// B-2026-09-17-4 (`e312de9`) narrowed `disarm_array_sources`' seeded
/// exclusion with `elem_te_runs_user_drop`, at the CONSTRUCTOR. That is the
/// right predicate in the wrong place: `try_compile_enum_variant_at` cannot
/// see who consumes the box it builds, and for a GENERIC callee the
/// monomorph gives the argument temp a plain `free` and no interior walk at
/// all, so standing the named source down leaves the elements' heap with NO
/// owner. Retract without arming — precisely the mirror b98707ee9's
/// exclusion existed to prevent, and precisely the mistake
/// B-2026-09-06-49's own first shape made in this same function.
///
/// WHY IT WENT GREEN THROUGH EVERY GATE, which is the part worth keeping.
/// The standing control for this mirror is
/// `b49-generic-callee-named-control` in
/// `asan_boxed_array_payload_interior_has_exactly_one_owner`, and its
/// element is a bare `String` — so it runs no user `Drop`, never satisfies
/// the new predicate, and is untouched by construction. `-17-4` re-measured
/// exactly that cell, correctly found it clean, and concluded the mirror was
/// clear. The mirror needs an element that runs a user `Drop` AND owns heap
/// — the same two conditions its own double free needed — reached through a
/// GENERIC callee rather than a concrete one. One generic parameter away
/// from every cell either row pinned.
///
/// Measured at `-O0` under `valgrind --leak-check=full` on `e312de9`:
/// cell 1 lost 18 B in 2 blocks, cell 2 lost 4 B in 2 blocks, and a
/// printing-`Drop` variant of cell 1 lost 4 B in 2 blocks. All three read
/// `All heap blocks were freed` with the retraction moved to the two
/// consumer sites that hold the arming decision.
///
/// Cell 1's `Drop` body prints NOTHING on purpose. A generic callee's
/// element bodies are separately missing on the compiled backends, so a
/// printing body would make this cell assert THAT defect's output and redden
/// when it is fixed. What it asserts is the memory, which is the channel
/// this row is about. Cell 2's body does print, because the passthrough
/// spelling's bodies are correct on all four surfaces.
///
/// B-2026-09-17-10 is `e312de9`'s author's own row for this same
/// regression, filed independently with their own measurements and
/// deliberately numbered to leave `-17-9` free. Cell 4 is their repro, and
/// it earns its place rather than restating cells 1-2: their payload is
/// spelled `Option[Array[T, 2]]`, which is INLINE and syntactically an
/// array, so it is declined for a different reason than a bare
/// `Option[T]`. Their row judged the retract half unfixable -- "nothing at
/// the ctor site can see its consumer... so 'disarm only when the consumer
/// is concrete' cannot be spelled there" -- and that is exactly right about
/// the ctor. It is why the ask moves to the consumer, where two sites
/// already held the arming gate; the ARM-half repair that row proposes
/// (teach the generic monomorph to own the interior) stays available and
/// is the larger change.
///
/// Measured on this tree: all four cells `All heap blocks were freed` with
/// `ERROR SUMMARY: 0 errors` at `-O0`, and each stdout below
/// byte-identical across `--interp` / jit / `karac build` /
/// `KARAC_AUTO_PAR=0 karac build`.
#[test]
fn asan_generic_callee_seeded_array_source_keeps_its_owner() {
    // 1 — THE MIRROR. A bare-type-param callee: the monomorph arms no
    //     interior walk, so the named source must KEEP its drop.
    assert_clean_asan_run(
        r#"struct Q179 { tag: String }
impl Drop for Q179 { fn drop(mut ref self) { } }
fn takesOpt[T](x: Option[T]) -> i64 {
    match x { Some(_) => { println("s"); 1 } None => { println("n"); 0 } }
}
fn main() {
    let a: Array[Q179, 2] = [Q179 { tag: f"b179-aaaaaaaa0" }, Q179 { tag: f"b179-bbbbbbbb1" }];
    let c = takesOpt(Some(a));
    println(f"c{c}");
}
"#,
        &["s", "c1"],
        "b179-generic-callee-leak-mirror",
    );
    // 2 — the PASSTHROUGH spelling of the same mirror. The `let` RECEIVES
    //     a box built at the call's argument site, so the `let` site must
    //     decline as well — `takes_over` is what declines it.
    assert_clean_asan_run(
        r#"struct S179 { tag: String }
impl Drop for S179 { fn drop(mut ref self) { println(f"drop:{self.tag}") } }
fn passthru[T](x: Option[T]) -> Option[T] { return x; }
fn takesOpt[T](x: Option[T]) -> i64 {
    match x { Some(_) => { println("o"); 1 } None => { println("n"); 0 } }
}
fn main() {
    let e: Array[S179, 2] = [S179 { tag: f"b179-cccccccc0" }, S179 { tag: f"b179-dddddddd1" }];
    let back = passthru(Some(e));
    let c = takesOpt(back);
    println(f"c{c}");
}
"#,
        &["o", "drop:b179-cccccccc0", "drop:b179-dddddddd1", "c1"],
        "b179-passthrough-user-drop-leak-mirror",
    );
    // 3 — THE OTHER SIDE OF THE PARTITION, kept here so the fixture states
    //     it rather than implying it: a CONCRETE callee DOES take the
    //     interior, so the same source MUST be retracted. Without this
    //     cell, "decline the generic case" could be satisfied by declining
    //     everything, which is the double free `-17-4` closed.
    assert_clean_asan_run(
        r#"struct S179b { tag: String }
impl Drop for S179b { fn drop(mut ref self) { println(f"drop:{self.tag}") } }
fn takesOpt(o: Option[Array[S179b, 2]]) {
    match o { Some(v) => { println(f"v:{v[0].tag}") } None => { println("n") } }
}
fn main() {
    let a: Array[S179b, 2] = [S179b { tag: f"b179-eeeeeeee0" }, S179b { tag: f"b179-ffffffff1" }];
    takesOpt(Some(a));
    println("held");
}
"#,
        &[
            "v:b179-eeeeeeee0",
            "drop:b179-eeeeeeee0",
            "drop:b179-ffffffff1",
            "held",
        ],
        "b179-concrete-callee-still-retracts",
    );
    // 4 — THE INLINE-PAYLOAD spelling of the mirror, `Option[Array[T, 2]]`
    //     rather than `Option[T]`. A different path and not a restatement:
    //     `Array[S, 2]` is two words, so this payload does NOT box, and the
    //     annotated payload IS syntactically an array — so
    //     `callee_takes_boxed_array_payload_interior` declines it on
    //     `option_payload_is_boxed` rather than on the array test that
    //     declines cells 1 and 2. Two independent reasons for the same
    //     answer, and a later widening could break either one alone.
    //
    //     This is B-2026-09-17-10's own repro, filed by `e312de9`'s author
    //     for the same regression; measured at `-O0` it lost 28 B in 2
    //     blocks with a silent `Drop` body and 18 B in 2 blocks with their
    //     printing one. Silent here for cell 1's reason: their row records
    //     that the element bodies were already missing on this shape before
    //     `e312de9` and are missing after, so a printing body would assert
    //     that separate defect's output.
    assert_clean_asan_run(
        r#"struct Z179 { tag: String }
impl Drop for Z179 { fn drop(mut ref self) { } }
fn takesG[T](x: Option[Array[T, 2]]) { println("in") }
fn main() {
    let a: Array[Z179, 2] = [Z179 { tag: f"b179-gggggggg0" }, Z179 { tag: f"b179-hhhhhhhh1" }];
    takesG(Some(a));
    println("held");
}
"#,
        &["in", "held"],
        "b179-inline-payload-generic-callee-mirror",
    );
}

/// B-2026-09-09-24 — the two `Array`-payload-INDEXED shapes that
/// `asan_boxed_array_payload_interior_has_exactly_one_owner` had to leave out
/// are clean now, and this is the fixture that stops them regressing.
///
/// That row recorded both as leaking at `KARAC_OPT_LEVEL=0` and clean at `=2`:
/// an `Array[String, 2]` bound out of an `Option` arm and indexed once lost
/// 18 B in 2 blocks (both elements), and a user enum carrying
/// `Array[Vec[String], 2]` read two levels deep lost 48 B in 1. Its neighbour
/// fixture names them in its own doc-comment as deliberately absent, which is
/// the interlock working as intended — the exclusion was a placeholder for
/// this fixture.
///
/// NEITHER WAS FIXED HERE, and both were fixed separately, which is why the
/// row closes against two SHAs rather than one. Bisected over the 184 commits
/// since the row was filed, each step rebuilt and re-measured under
/// `valgrind --leak-check=full` at `-O0`:
///
///   * shape 1 → `4dc4bdf4e` ("a boxed Array payload's interior gets exactly
///     one owner"): 18 B at its parent, clean at it.
///   * shape 2 → `b98707ee9` ("the box owns its array interior — disarm the
///     source at the ctor instead of withholding the walk"): still 48 B at
///     `4dc4bdf4e`, clean at `b98707ee9`.
///
/// The cells answer the two questions the row left open. `3:` is the N=3
/// payload form — the row could not tell whether the loss scaled with N,
/// because the only N=3 control it had was let-bound and therefore clean;
/// with the leak gone the cell can only guard the fix, but it guards it at a
/// second arity. `ov:` is the `Option` envelope over shape 2's payload, the
/// reverse of the question B-2026-09-09-9's `Vec` sibling raised, where the
/// envelope was the whole discriminator: here it is not — user enum and
/// `Option` behave alike.
///
/// `1b:` is the single-index spelling of shape 1 (the row's own repro reads
/// both elements). `ni:` is the payload binding NEVER indexed and `let:` the
/// same index off a `let` — the row's two controls, which were clean
/// throughout and must stay that way, since it was their cleanliness that
/// placed the fault in the conjunction rather than in either half.
///
/// THIS FIXTURE ONLY BITES ON THE `-O0` RATCHET LEG, and that has to be said
/// here because the plain `--features llvm` leg finds it VACUOUS. That leg
/// builds at `-O2`, where the optimizer deletes exactly these allocations —
/// which is why the row recorded both shapes as clean at `=2` in the first
/// place. Measured, rather than assumed: against `4dc4bdf4e^` and
/// `b98707ee9^` (`git checkout <sha> -- src/`, `tests/` kept at HEAD) this
/// test PASSES under a default `cargo test --features llvm` and FAILS under
/// `KARAC_OPT_LEVEL=0` — 243 B in 16 allocations at the first, 66 B in 4 at
/// the second, with HEAD clean before and after. So `scripts/asan-o0-leg.sh`
/// is its gate; a green `--features llvm` run is no evidence about it at all.
///
/// Measured on this tree: 0 bytes in 0 blocks at `-O0` under
/// `valgrind --leak-check=full`, and the stdout below is byte-identical
/// across `--interp` / jit / `karac build` / `KARAC_AUTO_PAR=0 karac build`.
#[test]
fn asan_indexed_array_payload_interior_has_exactly_one_owner() {
    assert_clean_asan_run(
        r#"enum E { A(Array[Vec[String], 2]), B }
fn p1(x: Option[Array[String, 2]]) { match x { Some(t) => { println(f"1:{t[0]}|{t[1]}") } None => { println("n") } } }
fn p1b(x: Option[Array[String, 2]]) { match x { Some(t) => { println(f"1b:{t[0]}") } None => { println("n") } } }
fn p3(x: Option[Array[String, 3]]) { match x { Some(t) => { println(f"3:{t[0]}|{t[2]}") } None => { println("n") } } }
fn pe(x: E) { match x { E.A(t) => { println(f"e:{t[1][0]}") } E.B => { println("n") } } }
fn pov(x: Option[Array[Vec[String], 2]]) { match x { Some(t) => { println(f"ov:{t[1][0]}") } None => { println("n") } } }
fn pnoidx(x: Option[Array[String, 2]]) { match x { Some(t) => { println(f"ni:{t}") } None => { println("n") } } }
fn main() {
    p1(Some([f"aaaaaaaa0", f"bbbbbbbb1"]));
    p1b(Some([f"cccccccc0", f"dddddddd1"]));
    p3(Some([f"eeeeeeee0", f"ffffffff1", f"gggggggg2"]));
    pe(E.A([[f"hhhhhhhh0"], [f"iiiiiiii1"]]));
    pov(Some([[f"jjjjjjjj0"], [f"kkkkkkkk1"]]));
    pnoidx(Some([f"llllllll0", f"mmmmmmmm1"]));
    let a: Array[String, 2] = [f"nnnnnnnn0", f"oooooooo1"];
    println(f"let:{a[0]}|{a[1]}");
    println("end");
}
"#,
        &[
            "1:aaaaaaaa0|bbbbbbbb1",
            "1b:cccccccc0",
            "3:eeeeeeee0|gggggggg2",
            "e:iiiiiiii1",
            "ov:kkkkkkkk1",
            "ni:[llllllll0, mmmmmmmm1]",
            "let:nnnnnnnn0|oooooooo1",
            "end",
        ],
        "asan_indexed_array_payload_interior_has_exactly_one_owner",
    );
}

/// B-2026-09-10-8 / B-2026-09-10-26 — an `Array` whose ELEMENT is itself an
/// `Array` drops its whole interior, in every position an array can occupy.
///
/// The two rows are one defect. -8 found it as an `Option` payload
/// (`Some(t) => t[0][0]`) while probing element shapes for B-2026-09-10-4
/// and left "does this leak outside a `match` arm?" as its cheapest
/// unmeasured discriminator; -26 answered it while closing B-2026-09-06-49
/// — a plain local, with no enum anywhere, loses the identical 36 B in 4
/// blocks. So the envelope was never part of the mechanism.
///
/// TWO HALVES THAT DISAGREED ABOUT WHAT AN ARRAY IS, and either alone
/// leaves the leak:
///
///   - the EMITTER. `emit_drop_fn_for_array` resolves its per-element drop
///     through `vec_element_drain_fn`, whose two legs are
///     `vec_elem_agg_drop_for_type_expr` (name-keyed: Map/Set/Option/
///     Result/shared/struct/enum/File, plus a Tuple arm) and
///     `elem_te_needs_direct_recursive_drain` (String/str/Vec/Map/Set).
///     Neither has an `Array` case, so an `Array` element answered `None`
///     and the OUTER array emitted no walk at all.
///
///   - the ADMISSION. `make_array_param_callee_owned` and
///     `synthesize_array_drop_fn_te` gate on `type_expr_has_drop_heap`,
///     which has no `Array` arm either. With only the emitter fixed, the
///     payload and struct-field routes came clean and the plain local and
///     by-value param stayed at 36 B — measured, and the reason this
///     fixture carries cells 1 and 2 separately from cells 3 and 5.
///
/// WHY THE RECURSION IS NOT IN THE SHARED ELEMENT POLICY. Cell 16 is a
/// `Vec[Array[String, 2]]`, and it was already clean before this fix —
/// the source local pushed into the `Vec` owns those buffers. Widening
/// `vec_elem_agg_drop_for_type_expr` (or
/// `elem_te_needs_direct_recursive_drain`) to know about `Array` would
/// reach that shape too and give it a second owner, turning a fixed leak
/// into a double free. Recursing on the array's own element instead
/// reaches every leaking position and nothing already owned. The same
/// argument keeps `type_expr_has_drop_heap` untouched: it has 69 call
/// sites choosing copy depth and drop strategy across the backend, so the
/// admission widening is a separate array-only predicate
/// (`nested_array_needs_drop`) asked in the two gates that are already
/// about arrays.
///
/// NOT FIXED HERE, and deliberately: `let b = passthru(a)` through a
/// GENERIC identity fn double frees. That is not this defect — the
/// ORDINARY one-level `Array[String, 2]` aborts identically on an
/// unmodified `main` (B-2026-09-10-34), because a monomorph's param
/// registration reads the DECLARED type `T` and never takes ownership
/// while the caller keeps its drop and the result binding adds one. This
/// fix does change the nested spelling's symptom from a 36 B leak to that
/// same abort, which is the honest consequence of giving the nested array
/// the owner a one-level array always had.
///
/// `-O2` HIDES ALL BUT ONE CELL. This harness builds at `-O2`, where the
/// optimizer deletes buffers nothing observes, so cells 1-10 and 12-13 are
/// carried by the `-O0` ratchet leg (`scripts/asan-o0-leg.sh`) that runs
/// this same suite. Cell 11 (`==`) is the exception: the comparison reads
/// every buffer, so its 72 B survives the optimizer and fails here.
#[test]
fn asan_nested_array_element_interior_has_an_owner() {
    // 1 — the plain LOCAL. No enum, no param, no field: the shape that
    //     separates this from B-2026-09-06-49, whose probe happened to be
    //     wrapped in an `Option`. 36 B in 4 blocks at `-O0`.
    assert_clean_asan_run(
            "fn main() {\n\
             \x20\x20\x20\x20let a: Array[Array[String, 2], 2] =\n\
             \x20\x20\x20\x20\x20\x20\x20\x20[[f\"aaaaaaaa0\", f\"bbbbbbbb0\"], [f\"cccccccc0\", f\"dddddddd0\"]];\n\
             \x20\x20\x20\x20println(f\"s:{a[0][0]}\");\n\
             }\n",
            &["s:aaaaaaaa0"],
            "nested-array-plain-local",
        );
    // 2 — the by-value PARAM, at the identical count. Both this and cell
    //     1 register through `make_array_param_callee_owned`, whose gate
    //     was the half that answered `false` for an `Array` element.
    assert_clean_asan_run(
            "fn eat(a: Array[Array[String, 2], 2]) { println(f\"e:{a[0][0]}\") }\n\
             fn main() {\n\
             \x20\x20\x20\x20let a: Array[Array[String, 2], 2] =\n\
             \x20\x20\x20\x20\x20\x20\x20\x20[[f\"aaaaaaaa0\", f\"bbbbbbbb0\"], [f\"cccccccc0\", f\"dddddddd0\"]];\n\
             \x20\x20\x20\x20eat(a);\n\
             }\n",
            &["e:aaaaaaaa0"],
            "nested-array-by-value-param",
        );
    // 3 — the `Option` PAYLOAD spelling, which is how B-2026-09-10-8
    //     found it (a hazard probe off B-2026-09-10-4).
    assert_clean_asan_run(
            "fn plainNN(x: Option[Array[Array[String, 2], 2]]) {\n\
             \x20\x20\x20\x20match x { Some(t) => { println(f\"s:{t[0][0]}\") } None => { println(\"n\") } }\n\
             }\n\
             fn main() {\n\
             \x20\x20\x20\x20let a: Array[Array[String, 2], 2] =\n\
             \x20\x20\x20\x20\x20\x20\x20\x20[[f\"aaaaaaaa0\", f\"bbbbbbbb0\"], [f\"cccccccc0\", f\"dddddddd0\"]];\n\
             \x20\x20\x20\x20plainNN(Some(a));\n\
             }\n",
            &["s:aaaaaaaa0"],
            "nested-array-option-payload",
        );
    // 4 — the `Result` side of cell 3. Both envelopes route the payload
    //     drop through the same array walk, so a one-sided repair shows up
    //     here.
    assert_clean_asan_run(
            "fn plainR(x: Result[Array[Array[String, 2], 2], i64]) {\n\
             \x20\x20\x20\x20match x { Ok(t) => { println(f\"s:{t[1][0]}\") } Err(e) => { println(f\"n:{e}\") } }\n\
             }\n\
             fn main() {\n\
             \x20\x20\x20\x20let a: Array[Array[String, 2], 2] =\n\
             \x20\x20\x20\x20\x20\x20\x20\x20[[f\"aaaaaaaa0\", f\"bbbbbbbb0\"], [f\"cccccccc0\", f\"dddddddd0\"]];\n\
             \x20\x20\x20\x20plainR(Ok(a));\n\
             }\n",
            &["s:cccccccc0"],
            "nested-array-result-ok-payload",
        );
    // 5 — a STRUCT FIELD. Reaches the walk by a different route (the
    //     `FieldDrop::ArrayField` classification, which asks
    //     `emit_drop_fn_for_array` whether a walk exists), so it pins the
    //     emitter half rather than the admission half.
    assert_clean_asan_run(
            "struct W { a: Array[Array[String, 2], 2] }\n\
             fn main() {\n\
             \x20\x20\x20\x20let w = W { a: [[f\"aaaaaaaa0\", f\"bbbbbbbb0\"], [f\"cccccccc0\", f\"dddddddd0\"]] };\n\
             \x20\x20\x20\x20println(\"s:ok\");\n\
             }\n",
            &["s:ok"],
            "nested-array-struct-field",
        );
    // 6 — THREE levels, 72 B in 8 blocks. The recursion is on the
    //     element, so depth is not a special case; a fix that hardcoded
    //     one level of unwrapping passes cells 1-5 and fails here.
    assert_clean_asan_run(
            "fn main() {\n\
             \x20\x20\x20\x20let a: Array[Array[Array[String, 2], 2], 2] =\n\
             \x20\x20\x20\x20\x20\x20\x20\x20[[[f\"aaaaaaaa0\", f\"bbbbbbbb0\"], [f\"cccccccc0\", f\"dddddddd0\"]],\n\
             \x20\x20\x20\x20\x20\x20\x20\x20\x20[[f\"eeeeeeee0\", f\"ffffffff0\"], [f\"gggggggg0\", f\"hhhhhhhh0\"]]];\n\
             \x20\x20\x20\x20println(\"s:ok\");\n\
             }\n",
            &["s:ok"],
            "nested-array-three-levels",
        );
    // 7 — the inner element is a user STRUCT, not a `String`. Same 36 B:
    //     the gap is the missing `Array` case, not anything about the leaf
    //     type.
    assert_clean_asan_run(
            "struct S { s: String }\n\
             fn main() {\n\
             \x20\x20\x20\x20let a: Array[Array[S, 2], 2] =\n\
             \x20\x20\x20\x20\x20\x20\x20\x20[[S { s: f\"aaaaaaaa0\" }, S { s: f\"bbbbbbbb0\" }], [S { s: f\"cccccccc0\" }, S { s: f\"dddddddd0\" }]];\n\
             \x20\x20\x20\x20println(\"s:ok\");\n\
             }\n",
            &["s:ok"],
            "nested-array-struct-element",
        );
    // 8 — a REBIND. Carries the double-free direction: `let u = a` must
    //     move the single owner, not add one (B-2026-09-10-4's
    //     `rebind_source_keeps_array_memory` is what keeps it at one, and
    //     this cell is what notices if a new registration walks past it).
    assert_clean_asan_run(
            "fn main() {\n\
             \x20\x20\x20\x20let a: Array[Array[String, 2], 2] =\n\
             \x20\x20\x20\x20\x20\x20\x20\x20[[f\"aaaaaaaa0\", f\"bbbbbbbb0\"], [f\"cccccccc0\", f\"dddddddd0\"]];\n\
             \x20\x20\x20\x20let u: Array[Array[String, 2], 2] = a;\n\
             \x20\x20\x20\x20println(f\"s:{u[0][1]}\");\n\
             }\n",
            &["s:bbbbbbbb0"],
            "nested-array-rebind",
        );
    // 9 — the arm-bound rebind, i.e. cell 8 inside cell 3. The arm frees
    //     the payload and the rebind creates no owner, so a second
    //     registration here aborts rather than leaks.
    assert_clean_asan_run(
            "fn plainNN(x: Option[Array[Array[String, 2], 2]]) {\n\
             \x20\x20\x20\x20match x { Some(t) => { let u: Array[Array[String, 2], 2] = t; println(f\"s:{u[1][1]}\") } None => { println(\"n\") } }\n\
             }\n\
             fn main() {\n\
             \x20\x20\x20\x20let a: Array[Array[String, 2], 2] =\n\
             \x20\x20\x20\x20\x20\x20\x20\x20[[f\"aaaaaaaa0\", f\"bbbbbbbb0\"], [f\"cccccccc0\", f\"dddddddd0\"]];\n\
             \x20\x20\x20\x20plainNN(Some(a));\n\
             }\n",
            &["s:dddddddd0"],
            "nested-array-arm-bound-rebind",
        );
    // 10 — RETURNED out of a callee. The escaping position: the callee's
    //      own drop must be retracted and the caller's binding must take
    //      it, which is one owner in each frame and never two.
    assert_clean_asan_run(
            "fn mk() -> Array[Array[String, 2], 2] {\n\
             \x20\x20\x20\x20return [[f\"aaaaaaaa0\", f\"bbbbbbbb0\"], [f\"cccccccc0\", f\"dddddddd0\"]];\n\
             }\n\
             fn main() {\n\
             \x20\x20\x20\x20let a = mk();\n\
             \x20\x20\x20\x20println(f\"s:{a[1][0]}\");\n\
             }\n",
            &["s:cccccccc0"],
            "nested-array-returned",
        );
    // 11 — `==` over two nested arrays. The ONE cell here that leaked at
    //      `-O2` as well as `-O0` (72 B in 8 blocks at both), because the
    //      comparison observes every buffer and the optimizer cannot
    //      delete them. It is therefore the cell that fails in THIS
    //      harness's default `-O2` build rather than only on the ratchet
    //      leg.
    assert_clean_asan_run(
            "fn mk(n: i64) -> Array[Array[String, 2], 2] {\n\
             \x20\x20\x20\x20return [[f\"aaaaaaaa{n}\", f\"bbbbbbbb{n}\"], [f\"cccccccc{n}\", f\"dddddddd{n}\"]];\n\
             }\n\
             fn main() {\n\
             \x20\x20\x20\x20let x: Array[Array[String, 2], 2] = mk(0);\n\
             \x20\x20\x20\x20let y: Array[Array[String, 2], 2] = mk(0);\n\
             \x20\x20\x20\x20println(f\"s:{x == y}\");\n\
             }\n",
            &["s:true"],
            "nested-array-equality",
        );
    // 12 — a LOOP body, 108 B over three iterations. Per-iteration
    //      scope exit rather than function exit.
    assert_clean_asan_run(
            "fn mk(n: i64) -> Array[Array[String, 2], 2] {\n\
             \x20\x20\x20\x20return [[f\"aaaaaaaa{n}\", f\"bbbbbbbb{n}\"], [f\"cccccccc{n}\", f\"dddddddd{n}\"]];\n\
             }\n\
             fn main() {\n\
             \x20\x20\x20\x20let mut i = 0;\n\
             \x20\x20\x20\x20while i < 3 {\n\
             \x20\x20\x20\x20\x20\x20\x20\x20let a: Array[Array[String, 2], 2] = mk(i);\n\
             \x20\x20\x20\x20\x20\x20\x20\x20println(f\"s:{i}\");\n\
             \x20\x20\x20\x20\x20\x20\x20\x20i = i + 1;\n\
             \x20\x20\x20\x20}\n\
             }\n",
            &["s:0", "s:1", "s:2"],
            "nested-array-loop-body",
        );
    // 13 — two DIFFERENT inner extents in one program, 90 B before the
    //      fix. The emitted walk is memoised on
    //      `karac_drop_Array_<elem>_<N>`, and the element half of that name
    //      is `display_mangle_te` of the inner array — so this cell fails
    //      loudly if the nested name ever collapses to a bare `Array` and
    //      the 2-wide walk is handed to the 3-wide type.
    assert_clean_asan_run(
            "fn main() {\n\
             \x20\x20\x20\x20let a: Array[Array[String, 2], 2] =\n\
             \x20\x20\x20\x20\x20\x20\x20\x20[[f\"aaaaaaaa0\", f\"bbbbbbbb0\"], [f\"cccccccc0\", f\"dddddddd0\"]];\n\
             \x20\x20\x20\x20let b: Array[Array[String, 3], 2] =\n\
             \x20\x20\x20\x20\x20\x20\x20\x20[[f\"eeeeeeee0\", f\"ffffffff0\", f\"gggggggg0\"], [f\"hhhhhhhh0\", f\"iiiiiiii0\", f\"jjjjjjjj0\"]];\n\
             \x20\x20\x20\x20println(\"s:ok\");\n\
             }\n",
            &["s:ok"],
            "nested-array-two-extents",
        );
    // 14 — CONTROL, the no-op direction: an all-scalar nest owns no heap,
    //      so the walk must still decline. `nested_array_needs_drop`
    //      carries the `None` contract inward; a version that answered
    //      `true` for any array would emit a walk over `i64`s here.
    assert_clean_asan_run(
        "fn eat(a: Array[Array[i64, 2], 2]) { println(f\"e:{a[0][1]}\") }\n\
             fn main() {\n\
             \x20\x20\x20\x20let a: Array[Array[i64, 2], 2] = [[1, 2], [3, 4]];\n\
             \x20\x20\x20\x20eat(a);\n\
             }\n",
        &["e:2"],
        "nested-array-scalar-control",
    );
    // 15 — CONTROL: the ONE-LEVEL array, which was correct throughout.
    //      Guards the direction where a widened element policy gives an
    //      already-owned array a second owner.
    assert_clean_asan_run(
        "fn main() {\n\
             \x20\x20\x20\x20let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
             \x20\x20\x20\x20println(f\"s:{a[0]}\");\n\
             }\n",
        &["s:aaaaaaaa0"],
        "one-level-array-control",
    );
    // 16 — CONTROL, and the reason the recursion is inside
    //      `emit_drop_fn_for_array` rather than in the shared element
    //      policy: a `Vec[Array[String, 2]]` is already clean, because the
    //      SOURCE LOCAL pushed into it owns those buffers. Teaching
    //      `vec_elem_agg_drop_for_type_expr` about `Array` would give this
    //      cell a second owner and abort it.
    assert_clean_asan_run(
        "fn main() {\n\
             \x20\x20\x20\x20let mut v: Vec[Array[String, 2]] = Vec.new();\n\
             \x20\x20\x20\x20let e0: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
             \x20\x20\x20\x20let e1: Array[String, 2] = [f\"cccccccc0\", f\"dddddddd0\"];\n\
             \x20\x20\x20\x20v.push(e0);\n\
             \x20\x20\x20\x20v.push(e1);\n\
             \x20\x20\x20\x20println(f\"s:{v.len()}\");\n\
             }\n",
        &["s:2"],
        "vec-of-one-level-arrays-control",
    );
}

#[test]
fn asan_generic_callee_array_param_has_exactly_one_owner() {
    // B-2026-09-10-34. An owned `Array[T, N]` handed to a GENERIC callee
    // had no owner story at all: `compile_function`'s array-param arm
    // reads the DECLARED type, and a monomorph of `fn passthru[T](x: T)`
    // declares `T`, so the monomorph never took ownership -- while the
    // caller's own retraction sat behind `transfer_ident`, which is keyed
    // on a STRUCT type name an array does not have. Caller and callee both
    // kept the buffers: `free(): double free detected in tcache 2` under
    // the JIT and at `-O0`, 12 frees against 10 allocs under valgrind.
    //
    // Every cell here is a DOUBLE FREE pre-fix, not a leak, so `-O2`'s
    // dead-allocation deletion does not hide them -- but the `-O0` leg is
    // still where the counts are read.
    //
    // Cells 6-10 are the controls that matter. Three separate wrong fixes
    // passed cells 1-5 and failed one of these: a caller-only retraction
    // (leaks the consume cell), a callee registration keyed off the
    // substitution maps (registers nothing -- those channels are empty for
    // a bare-`T` whole param by design, `type_param_is_a_whole_param_type`),
    // and a record written under a non-final `mangled` (populated under a
    // key nothing reads). Cells 1-5 alone cannot tell a working fix from
    // any of them.
    //
    // 1 -- the reported shape: generic passthru, result BOUND.
    assert_clean_asan_run(
        "fn passthru[T](x: T) -> T { return x; }\n\
             fn main() {\n\
             \x20\x20\x20\x20let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
             \x20\x20\x20\x20let b: Array[String, 2] = passthru(a);\n\
             \x20\x20\x20\x20println(f\"s:{b[0]}\");\n\
             }\n",
        &["s:aaaaaaaa0"],
        "generic-array-param-bound",
    );
    // 2 -- the NESTED element, which B-2026-09-10-8/-26 gave an owner and
    //      which therefore reached this same hole one level down. 16 frees
    //      against 12 allocs pre-fix -- the only cell here with 4 invalid
    //      frees rather than 2.
    assert_clean_asan_run(
            "fn passthru[T](x: T) -> T { return x; }\n\
             fn main() {\n\
             \x20\x20\x20\x20let a: Array[Array[String, 2], 2] =\n\
             \x20\x20\x20\x20\x20\x20\x20\x20[[f\"aaaaaaaa0\", f\"bbbbbbbb0\"], [f\"cccccccc0\", f\"dddddddd0\"]];\n\
             \x20\x20\x20\x20let b: Array[Array[String, 2], 2] = passthru(a);\n\
             \x20\x20\x20\x20println(f\"s:{b[0][0]}\");\n\
             }\n",
            &["s:aaaaaaaa0"],
            "generic-nested-array-param-bound",
        );
    // 3 -- a generic METHOD with an array argument. The row listed this as
    //      NOT MEASURED; it aborts identically, because a generic method
    //      routes through `compile_generic_call` like a free function.
    assert_clean_asan_run(
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
        &["s:bbbbbbbb0"],
        "generic-method-array-arg",
    );
    // 4 -- a TWO-parameter generic where only one param is an array, also
    //      listed NOT MEASURED. Pins that the record is per-parameter and
    //      not "this call has an array somewhere".
    assert_clean_asan_run(
        "fn firstof[T, U](x: T, y: U) -> T { return x; }\n\
             fn main() {\n\
             \x20\x20\x20\x20let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
             \x20\x20\x20\x20let k: i64 = 3;\n\
             \x20\x20\x20\x20let b: Array[String, 2] = firstof(a, k);\n\
             \x20\x20\x20\x20println(f\"s:{b[0]}\");\n\
             }\n",
        &["s:aaaaaaaa0"],
        "generic-two-param-one-array",
    );
    // 5 -- a generic caller FORWARDING its own type param into a second
    //      generic. Not in the row at all, and the cell that rejects the
    //      obvious fix: inside `outer`, `U` resolves through substitution
    //      channels that are empty for a whole param, so the inner call
    //      could see neither that `passthru` takes `y` nor what `y` is.
    //      The resolver falls back to the caller's own `owned_array_params`
    //      entry for exactly this.
    assert_clean_asan_run(
        "fn passthru[T](x: T) -> T { return x; }\n\
             fn outer[U](y: U) -> U { return passthru(y); }\n\
             fn main() {\n\
             \x20\x20\x20\x20let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
             \x20\x20\x20\x20let b: Array[String, 2] = outer(a);\n\
             \x20\x20\x20\x20println(f\"s:{b[0]}\");\n\
             }\n",
        &["s:aaaaaaaa0"],
        "generic-forwarded-through-generic-caller",
    );
    // 6 -- CONTROL: the generic callee CONSUMES rather than returns. The
    //      caller stands down, so the monomorph must actually register the
    //      drop or these buffers have no owner at all. A caller-only fix
    //      leaks 18 B in 2 blocks here while cells 1-5 look repaired.
    assert_clean_asan_run(
        "fn eat[T](x: T) -> i64 { return 7; }\n\
             fn main() {\n\
             \x20\x20\x20\x20let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
             \x20\x20\x20\x20let n: i64 = eat(a);\n\
             \x20\x20\x20\x20println(f\"s:{n}\");\n\
             }\n",
        &["s:7"],
        "generic-array-param-consumed-control",
    );
    // 7 -- CONTROL: a `ref` generic param transfers nothing, so neither
    //      half may fire. The caller's binding still owns the buffers.
    assert_clean_asan_run(
        "fn peek[T](x: ref T) -> i64 { return 1; }\n\
             fn main() {\n\
             \x20\x20\x20\x20let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
             \x20\x20\x20\x20let n: i64 = peek(a);\n\
             \x20\x20\x20\x20println(f\"s:{n}{a[0]}\");\n\
             }\n",
        &["s:1aaaaaaaa0"],
        "generic-ref-array-param-control",
    );
    // 8 -- CONTROL: a scalar element owns no heap, so the array must not
    //      be registered at all. Separates "an array moved" from "a
    //      drop-bearing array moved".
    assert_clean_asan_run(
        "fn passthru[T](x: T) -> T { return x; }\n\
             fn main() {\n\
             \x20\x20\x20\x20let a: Array[i64, 3] = [1, 2, 3];\n\
             \x20\x20\x20\x20let b: Array[i64, 3] = passthru(a);\n\
             \x20\x20\x20\x20println(f\"s:{b[0]}\");\n\
             }\n",
        &["s:1"],
        "generic-scalar-array-control",
    );
    // 9 -- CONTROL: the `Vec` peer, clean before this change and after.
    //      The row asked whether the collection peers were already handled
    //      by a different mechanism; they are, and this pins that the
    //      array record did not disturb it.
    assert_clean_asan_run(
        "fn passthru[T](x: T) -> T { return x; }\n\
             fn main() {\n\
             \x20\x20\x20\x20let a: Vec[String] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
             \x20\x20\x20\x20let b: Vec[String] = passthru(a);\n\
             \x20\x20\x20\x20println(f\"s:{b[0]}\");\n\
             }\n",
        &["s:aaaaaaaa0"],
        "generic-vec-peer-control",
    );
    // 10 -- CONTROL: a fresh TEMP argument, which has no caller binding to
    //       retract. The retraction must no-op rather than reach for a
    //       root that is not there.
    assert_clean_asan_run(
        "fn mk() -> Array[String, 2] {\n\
             \x20\x20\x20\x20let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
             \x20\x20\x20\x20return a;\n\
             }\n\
             fn passthru[T](x: T) -> T { return x; }\n\
             fn main() {\n\
             \x20\x20\x20\x20let b: Array[String, 2] = passthru(mk());\n\
             \x20\x20\x20\x20println(f\"s:{b[0]}\");\n\
             }\n",
        &["s:aaaaaaaa0"],
        "generic-array-temp-arg-control",
    );
}

#[test]
fn asan_discarded_array_result_has_exactly_one_owner() {
    // B-2026-09-12-2. A discarded call result of an array-returning
    // function was owned by NOBODY, and every individual step of the
    // ownership chain was right -- which is what made it invisible. The
    // caller's binding is retracted at the call
    // (`suppress_array_binding_move_arg`) because the callee takes
    // ownership by transfer; the callee registers its own scope-exit
    // element drop (`make_array_param_callee_owned`) and RETRACTS it at
    // `return x` (B-2026-08-24-5), correctly, since the value is leaving
    // the frame. The result therefore arrives at the call site owned by
    // nobody, and an expression statement binds nothing. Measured 18 B in
    // 2 blocks at `-O0`; silent on all six surfaces, and clean at `-O2`
    // where the optimizer deletes an allocation nothing observes.
    //
    // Cells 7-10 are the controls that matter, and cell 7 is the one that
    // caught a wrong fix: registering the drop WITHOUT proving the result
    // owned turns a borrow-projection return into a double free, spreading
    // an existing unsound copy (the W0299 `borrow_projection_copy` class,
    // B-2026-09-06-31) to a second spelling. Cells 1-6 alone cannot tell
    // that fix from this one.
    //
    // 1 -- the reported shape: concrete callee, result DISCARDED.
    assert_clean_asan_run(
        "fn passthru(x: Array[String, 2]) -> Array[String, 2] { return x; }\n\
             fn main() {\n\
             \x20\x20\x20\x20let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
             \x20\x20\x20\x20passthru(a);\n\
             \x20\x20\x20\x20println(\"s:ok\");\n\
             }\n",
        &["s:ok"],
        "discarded-array-result-concrete",
    );
    // 2 -- the GENERIC spelling, which was clean before 55e767a only
    //      because nothing on that leg transferred ownership at all, so
    //      the caller's binding stayed the single correct owner. Giving a
    //      monomorph's array param the owner it always should have had
    //      moved it onto the concrete path's behaviour, this hole
    //      included -- convergence, not a new defect. It declines unless
    //      the return type is resolved through the per-call substitution:
    //      `fn_return_type_exprs` holds no entry for a generic callee.
    assert_clean_asan_run(
        "fn passthru[T](x: T) -> T { return x; }\n\
             fn main() {\n\
             \x20\x20\x20\x20let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
             \x20\x20\x20\x20passthru(a);\n\
             \x20\x20\x20\x20println(\"s:ok\");\n\
             }\n",
        &["s:ok"],
        "discarded-array-result-generic",
    );
    // 3 -- the NESTED element, listed NOT MEASURED on the row. 36 B in 4
    //      blocks pre-fix, twice the one-level count, because the walk is
    //      the recursive one B-2026-09-10-8/-26 built.
    assert_clean_asan_run(
            "fn passthru(x: Array[Array[String, 2], 2]) -> Array[Array[String, 2], 2] {\n\
             \x20\x20\x20\x20return x;\n\
             }\n\
             fn main() {\n\
             \x20\x20\x20\x20let a: Array[Array[String, 2], 2] =\n\
             \x20\x20\x20\x20\x20\x20\x20\x20[[f\"aaaaaaaa0\", f\"bbbbbbbb0\"], [f\"cccccccc0\", f\"dddddddd0\"]];\n\
             \x20\x20\x20\x20passthru(a);\n\
             \x20\x20\x20\x20println(\"s:ok\");\n\
             }\n",
            &["s:ok"],
            "discarded-array-result-nested",
        );
    // 4 -- a discarded METHOD result, also listed NOT MEASURED. Resolves
    //      through the `Type.method` key rather than the free-function
    //      table, so it pins the second of the two lookups.
    assert_clean_asan_run(
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
            &["s:ok"],
            "discarded-array-result-method",
        );
    // 5 -- the `match`-arm return, the third NOT-MEASURED shape. This is
    //      the cell that fails if the return walk pushes a tail CONSTRUCT
    //      rather than its arms' tails: the `match` node is neither an
    //      identifier nor a literal, so the whole call gets refused and
    //      the leak comes back at the identical count.
    assert_clean_asan_run(
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
        &["s:ok"],
        "discarded-array-result-match-arm",
    );
    // 6 -- a callee that MINTS the array rather than forwarding a param.
    //      Nothing upstream ever owned these buffers, so this is the one
    //      target cell whose fix cannot be confused with a retraction.
    assert_clean_asan_run(
        "fn mint() -> Array[String, 2] { return [f\"aaaaaaaa0\", f\"bbbbbbbb0\"]; }\n\
             fn main() {\n\
             \x20\x20\x20\x20mint();\n\
             \x20\x20\x20\x20println(\"s:ok\");\n\
             }\n",
        &["s:ok"],
        "discarded-array-result-minted",
    );
    // 7 -- THE CONTROL THAT CAUGHT A WRONG FIX: a BORROW PROJECTION
    //      return. `w.a` copies out of a struct `w` still owns, so a drop
    //      registered here is a double free -- measured as four
    //      `Invalid free()` at `-O0` with an ungated arm. This cell is
    //      clean on unmodified `main` precisely because nothing frees the
    //      copy, which is why only a control can see the regression.
    assert_clean_asan_run(
        "struct W { a: Array[String, 2] }\n\
             fn get(w: ref W) -> Array[String, 2] { return w.a; }\n\
             fn main() {\n\
             \x20\x20\x20\x20let w: W = W { a: [f\"aaaaaaaa0\", f\"bbbbbbbb0\"] };\n\
             \x20\x20\x20\x20get(w);\n\
             \x20\x20\x20\x20println(\"s:ok\");\n\
             }\n",
        &["s:ok"],
        "discarded-borrow-projection-control",
    );
    // 8 -- CONTROL: the BOUND result, which was always clean because the
    //      binding registers the owner the discard had no place for.
    //      Guards the direction where the new arm gives it a second.
    assert_clean_asan_run(
        "fn passthru(x: Array[String, 2]) -> Array[String, 2] { return x; }\n\
             fn main() {\n\
             \x20\x20\x20\x20let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
             \x20\x20\x20\x20let b: Array[String, 2] = passthru(a);\n\
             \x20\x20\x20\x20println(f\"s:{b[0]}\");\n\
             }\n",
        &["s:aaaaaaaa0"],
        "discarded-array-bound-control",
    );
    // 9 -- CONTROL: a SCALAR element. The synthesizer's own gate declines
    //      it, so this cell must register nothing at all; a widened
    //      admission would emit a walk over `i64`s.
    assert_clean_asan_run(
        "fn passthru(x: Array[i64, 2]) -> Array[i64, 2] { return x; }\n\
             fn main() {\n\
             \x20\x20\x20\x20let a: Array[i64, 2] = [11, 22];\n\
             \x20\x20\x20\x20passthru(a);\n\
             \x20\x20\x20\x20println(\"s:ok\");\n\
             }\n",
        &["s:ok"],
        "discarded-scalar-array-control",
    );
    // 10 -- CONTROL: the `Vec` peer at the same shape, clean throughout
    //       (14 allocs / 14 frees). Whatever owns a discarded `Vec`
    //       result must keep owning it -- the array arm sits beside that
    //       machinery, not in front of it.
    assert_clean_asan_run(
        "fn passthru(x: Vec[String]) -> Vec[String] { return x; }\n\
             fn main() {\n\
             \x20\x20\x20\x20let a: Vec[String] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
             \x20\x20\x20\x20passthru(a);\n\
             \x20\x20\x20\x20println(\"s:ok\");\n\
             }\n",
        &["s:ok"],
        "discarded-vec-peer-control",
    );
}

#[test]
fn asan_array_binding_moved_into_struct_field_has_one_owner() {
    // B-2026-09-12-14. A NAMED `Array` binding moved into a struct field
    // kept the scope-exit element drop `make_array_param_callee_owned`
    // gave it at its `let`, while the struct's own drop walked the same
    // buffers -- so both ran. 10 allocs / 12 frees with two
    // `Invalid free()` at `-O0`, needing NO call and NO generics, and two
    // more per additional array field.
    //
    // It stayed invisible for two compounding reasons. glibc's tcache
    // absorbs the duplicate free rather than aborting, so every backend
    // printed correctly and exited 0; and the two neighbouring spellings
    // are clean for UNRELATED reasons -- a direct literal into the field
    // has no source binding to leave an owner behind, and a `Vec` field is
    // covered by `suppress_source_vec_cleanup_for_arg`, which sits three
    // lines above the gap in the same battery.
    //
    // Cell 12 is the control that caught a regression in the fix: an
    // ungated disarm turned the DISCARDED literal spelling from a clean
    // 10 allocs / 10 frees into an 18 B leak, because a discarded literal
    // takes nothing over and its sources must keep their owners.
    //
    // 1 -- the reported shape.
    assert_clean_asan_run(
        "struct W { a: Array[String, 2] }\n\
             fn main() {\n\
             \x20\x20\x20\x20let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
             \x20\x20\x20\x20let w: W = W { a: a };\n\
             \x20\x20\x20\x20println(f\"s:{w.a[0]}\");\n\
             }\n",
        &["s:aaaaaaaa0"],
        "array-binding-into-struct-field",
    );
    // 2 -- TWO array fields. Four invalid frees rather than two, which is
    //      what shows the duplicate is per SOURCE BINDING and not one
    //      per struct.
    assert_clean_asan_run(
        "struct W { a: Array[String, 2], b: Array[String, 2] }\n\
             fn main() {\n\
             \x20\x20\x20\x20let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
             \x20\x20\x20\x20let b: Array[String, 2] = [f\"cccccccc0\", f\"dddddddd0\"];\n\
             \x20\x20\x20\x20let w: W = W { a: a, b: b };\n\
             \x20\x20\x20\x20println(f\"s:{w.b[1]}\");\n\
             }\n",
        &["s:dddddddd0"],
        "two-array-fields",
    );
    // 3 -- the NESTED element, through the recursive walk
    //      B-2026-09-10-8/-26 built.
    assert_clean_asan_run(
            "struct W { a: Array[Array[String, 2], 2] }\n\
             fn main() {\n\
             \x20\x20\x20\x20let a: Array[Array[String, 2], 2] =\n\
             \x20\x20\x20\x20\x20\x20\x20\x20[[f\"aaaaaaaa0\", f\"bbbbbbbb0\"], [f\"cccccccc0\", f\"dddddddd0\"]];\n\
             \x20\x20\x20\x20let w: W = W { a: a };\n\
             \x20\x20\x20\x20println(\"s:ok\");\n\
             }\n",
            &["s:ok"],
            "nested-array-field",
        );
    // 4 -- a `Vec` ELEMENT inside the moved array.
    assert_clean_asan_run(
        "struct W { a: Array[Vec[String], 2] }\n\
             fn main() {\n\
             \x20\x20\x20\x20let mut v0: Vec[String] = Vec.new();\n\
             \x20\x20\x20\x20v0.push(f\"aaaaaaaa0\");\n\
             \x20\x20\x20\x20let mut v1: Vec[String] = Vec.new();\n\
             \x20\x20\x20\x20v1.push(f\"bbbbbbbb0\");\n\
             \x20\x20\x20\x20let a: Array[Vec[String], 2] = [v0, v1];\n\
             \x20\x20\x20\x20let w: W = W { a: a };\n\
             \x20\x20\x20\x20println(\"s:ok\");\n\
             }\n",
        &["s:ok"],
        "vec-element-array-field",
    );
    // 5 -- the ESCAPING struct, returned from the fn that built it. The
    //      duplicate travels with it, so this is the shape where the
    //      second free lands in the CALLER's frame.
    assert_clean_asan_run(
        "struct W { a: Array[String, 2] }\n\
             fn mk() -> W {\n\
             \x20\x20\x20\x20let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
             \x20\x20\x20\x20return W { a: a };\n\
             }\n\
             fn main() {\n\
             \x20\x20\x20\x20let w: W = mk();\n\
             \x20\x20\x20\x20println(f\"s:{w.a[0]}\");\n\
             }\n",
        &["s:aaaaaaaa0"],
        "escaping-struct-field",
    );
    // 6 -- a GENERIC struct at the same shape.
    assert_clean_asan_run(
        "struct Box[T] { v: T }\n\
             fn main() {\n\
             \x20\x20\x20\x20let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
             \x20\x20\x20\x20let w: Box[Array[String, 2]] = Box[Array[String, 2]] { v: a };\n\
             \x20\x20\x20\x20println(f\"s:{w.v[0]}\");\n\
             }\n",
        &["s:aaaaaaaa0"],
        "generic-struct-array-field",
    );
    // 7-10 -- the four DOWNSTREAM positions, none of which the row
    //         recorded and all of which double-freed identically: the
    //         literal pushed into a `Vec`, passed as a call argument,
    //         wrapped in a user enum, and wrapped in `Option`.
    assert_clean_asan_run(
        "struct W { a: Array[String, 2] }\n\
             fn main() {\n\
             \x20\x20\x20\x20let mut vs: Vec[W] = Vec.new();\n\
             \x20\x20\x20\x20let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
             \x20\x20\x20\x20vs.push(W { a: a });\n\
             \x20\x20\x20\x20println(f\"s:{vs.len()}\");\n\
             }\n",
        &["s:1"],
        "struct-literal-pushed-into-vec",
    );
    assert_clean_asan_run(
        "struct W { a: Array[String, 2] }\n\
             fn eat(w: W) -> i64 { return 1; }\n\
             fn main() {\n\
             \x20\x20\x20\x20let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
             \x20\x20\x20\x20let n: i64 = eat(W { a: a });\n\
             \x20\x20\x20\x20println(f\"s:{n}\");\n\
             }\n",
        &["s:1"],
        "struct-literal-as-call-arg",
    );
    assert_clean_asan_run(
        "struct W { a: Array[String, 2] }\n\
             enum E { Full(W), Empty }\n\
             fn main() {\n\
             \x20\x20\x20\x20let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
             \x20\x20\x20\x20let e: E = E.Full(W { a: a });\n\
             \x20\x20\x20\x20println(\"s:ok\");\n\
             }\n",
        &["s:ok"],
        "struct-literal-in-enum-payload",
    );
    assert_clean_asan_run(
        "struct W { a: Array[String, 2] }\n\
             fn main() {\n\
             \x20\x20\x20\x20let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
             \x20\x20\x20\x20let o: Option[W] = Some(W { a: a });\n\
             \x20\x20\x20\x20println(\"s:ok\");\n\
             }\n",
        &["s:ok"],
        "struct-literal-in-option",
    );
    // 11 -- inside a LOOP, where the duplicate recurs per iteration.
    assert_clean_asan_run(
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
            &["s:aaaaaaaa0", "s:aaaaaaaa0", "s:aaaaaaaa0"],
            "array-field-move-in-loop",
        );
    // 12 -- THE CONTROL THAT CAUGHT A REGRESSION: the DISCARDED literal.
    //       It takes nothing over, so the source keeps its owner and this
    //       cell is clean on unmodified `main`. An ungated disarm leaks
    //       18 B in 2 blocks here -- this row's double free traded for a
    //       leak one statement over. The gate is `in_discarded_aggregate_tail`,
    //       the predicate B-2026-09-01-5 / B-2026-09-07-14 built for the
    //       four place-shaped disarms in the same battery.
    assert_clean_asan_run(
        "struct W { a: Array[String, 2] }\n\
             fn main() {\n\
             \x20\x20\x20\x20let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
             \x20\x20\x20\x20W { a: a };\n\
             \x20\x20\x20\x20println(\"s:ok\");\n\
             }\n",
        &["s:ok"],
        "discarded-struct-literal-control",
    );
    // 13 -- CONTROL: the SHARED struct, clean at 11 allocs / 11 frees
    //       throughout because the RC path owns the field through a
    //       different channel. The disarm is deliberately not wired into
    //       that branch; adding it there would retract the only owner.
    assert_clean_asan_run(
        "shared struct W { a: Array[String, 2] }\n\
             fn main() {\n\
             \x20\x20\x20\x20let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
             \x20\x20\x20\x20let w: W = W { a: a };\n\
             \x20\x20\x20\x20println(f\"s:{w.a[0]}\");\n\
             }\n",
        &["s:aaaaaaaa0"],
        "shared-struct-array-field-control",
    );
    // 14 -- CONTROL: a direct ARRAY LITERAL into the field. No source
    //       binding exists, so there was never a second owner.
    assert_clean_asan_run(
        "struct W { a: Array[String, 2] }\n\
             fn main() {\n\
             \x20\x20\x20\x20let w: W = W { a: [f\"aaaaaaaa0\", f\"bbbbbbbb0\"] };\n\
             \x20\x20\x20\x20println(f\"s:{w.a[0]}\");\n\
             }\n",
        &["s:aaaaaaaa0"],
        "direct-literal-field-control",
    );
    // 15 -- CONTROL: the `Vec` field, clean because
    //       `suppress_source_vec_cleanup_for_arg` already covers it. It is
    //       the peer this fix was modelled on, so a regression here would
    //       mean the new disarm displaced it.
    assert_clean_asan_run(
        "struct W { a: Vec[String] }\n\
             fn main() {\n\
             \x20\x20\x20\x20let mut a: Vec[String] = Vec.new();\n\
             \x20\x20\x20\x20a.push(f\"aaaaaaaa0\");\n\
             \x20\x20\x20\x20let w: W = W { a: a };\n\
             \x20\x20\x20\x20println(f\"s:{w.a[0]}\");\n\
             }\n",
        &["s:aaaaaaaa0"],
        "vec-field-control",
    );
    // 16 -- CONTROL: a SCALAR element owns no heap, so the disarm must
    //       find nothing to retract.
    assert_clean_asan_run(
        "struct W { a: Array[i64, 2] }\n\
             fn main() {\n\
             \x20\x20\x20\x20let a: Array[i64, 2] = [11, 22];\n\
             \x20\x20\x20\x20let w: W = W { a: a };\n\
             \x20\x20\x20\x20println(f\"s:{w.a[0]}\");\n\
             }\n",
        &["s:11"],
        "scalar-array-field-control",
    );
}

#[test]
fn asan_arm_bound_array_rebind_leaves_memory_with_one_owner() {
    // 1 — the live bug: an ANNOTATED rebind of an arm-bound payload.
    assert_clean_asan_run(
            "fn plainA(x: Option[Array[String, 2]]) {\n\
             \x20   match x { Some(t) => { let u: Array[String, 2] = t; println(f\"s:{u[0]}\") } None => { println(\"n\") } }\n\
             }\n\
             fn main() {\n\
             \x20   let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
             \x20   plainA(Some(a));\n\
             }\n",
            &["s:aaaaaaaa0"],
            "b4-annotated-arm-rebind-string-element",
        );
    // 2 — two levels of heap under the element. This is the cell that is
    //     corrupt at EVERY opt level rather than only at `-O0`.
    assert_clean_asan_run(
            "fn plainAV(x: Option[Array[Vec[String], 2]]) {\n\
             \x20   match x { Some(t) => { let u: Array[Vec[String], 2] = t; println(\"held\") } None => { println(\"n\") } }\n\
             }\n\
             fn main() {\n\
             \x20   let a: Array[Vec[String], 2] = [[f\"aaaaaaaa0\", f\"aaaaaaaa1\"], [f\"bbbbbbbb0\"]];\n\
             \x20   plainAV(Some(a));\n\
             }\n",
            &["held"],
            "b4-annotated-arm-rebind-vec-string-element",
        );
    // 3 — a user STRUCT element that owns a String, the shape
    //     B-2026-08-28-57's rule was written against.
    assert_clean_asan_run(
            "struct S4 { s: String }\n\
             fn plainAS(x: Option[Array[S4, 2]]) {\n\
             \x20   match x { Some(t) => { let u: Array[S4, 2] = t; println(f\"s:{u[0].s}\") } None => { println(\"n\") } }\n\
             }\n\
             fn main() {\n\
             \x20   let a: Array[S4, 2] = [S4 { s: f\"aaaaaaaa0\" }, S4 { s: f\"bbbbbbbb0\" }];\n\
             \x20   plainAS(Some(a));\n\
             }\n",
            &["s:aaaaaaaa0"],
            "b4-annotated-arm-rebind-struct-element",
        );
    // 4 — the UN-annotated spelling the row actually recorded. It refused
    //     to build before this fix, so it could not corrupt anything; it
    //     builds now, and has to be single-owner too.
    assert_clean_asan_run(
            "fn plainV(x: Option[Array[Vec[String], 2]]) {\n\
             \x20   match x { Some(t) => { let u = t; println(f\"s:{u[0][0]}\") } None => { println(\"n\") } }\n\
             }\n\
             fn main() {\n\
             \x20   let a: Array[Vec[String], 2] = [[f\"aaaaaaaa0\", f\"aaaaaaaa1\"], [f\"bbbbbbbb0\"]];\n\
             \x20   plainV(Some(a));\n\
             }\n",
            &["s:aaaaaaaa0"],
            "b4-bare-arm-rebind-then-nested-read",
        );
    // 5 — the `Result` spelling of the same arm, so the fix is keyed on the
    //     binding and not on `Option`.
    assert_clean_asan_run(
            "fn plainR(x: Result[Array[Vec[String], 2], i64]) {\n\
             \x20   match x { Ok(t) => { let u = t; println(f\"s:{u[0][0]}\") } Err(e) => { println(f\"e:{e}\") } }\n\
             }\n\
             fn main() {\n\
             \x20   let a: Array[Vec[String], 2] = [[f\"aaaaaaaa0\", f\"aaaaaaaa1\"], [f\"bbbbbbbb0\"]];\n\
             \x20   plainR(Result.Ok(a));\n\
             }\n",
            &["s:aaaaaaaa0"],
            "b4-result-arm-rebind",
        );
    // 6 — TWO rebinds in a row. Each hop has to stand down, or the last one
    //     owns what the arm still owns.
    assert_clean_asan_run(
            "fn plainC(x: Option[Array[Vec[String], 2]]) {\n\
             \x20   match x { Some(t) => { let u = t; let v = u; println(f\"s:{v[0][0]}\") } None => { println(\"n\") } }\n\
             }\n\
             fn main() {\n\
             \x20   let a: Array[Vec[String], 2] = [[f\"aaaaaaaa0\", f\"aaaaaaaa1\"], [f\"bbbbbbbb0\"]];\n\
             \x20   plainC(Some(a));\n\
             }\n",
            &["s:aaaaaaaa0"],
            "b4-chained-arm-rebind",
        );
    // 7 — LEAK DIRECTION: the arm with no rebind at all still has to free
    //     its payload exactly once.
    assert_clean_asan_run(
            "fn plainP(x: Option[Array[Vec[String], 2]]) {\n\
             \x20   match x { Some(t) => { println(f\"s:{t[0][0]}\") } None => { println(\"n\") } }\n\
             }\n\
             fn main() {\n\
             \x20   let a: Array[Vec[String], 2] = [[f\"aaaaaaaa0\", f\"aaaaaaaa1\"], [f\"bbbbbbbb0\"]];\n\
             \x20   plainP(Some(a));\n\
             }\n",
            &["s:aaaaaaaa0"],
            "b4-plain-arm-no-rebind-control",
        );
    // 8 — CONTROL: a scalar element has no heap for a second owner to free,
    //     so it was correct either way and must stay so.
    assert_clean_asan_run(
            "fn plainI(x: Option[Array[i64, 2]]) {\n\
             \x20   match x { Some(t) => { let u = t; println(f\"s:{u[0]}\") } None => { println(\"n\") } }\n\
             }\n\
             fn main() {\n\
             \x20   let a: Array[i64, 2] = [11, 22];\n\
             \x20   plainI(Some(a));\n\
             }\n",
            &["s:11"],
            "b4-scalar-element-control",
        );
    // 9 — CONTROL: -23's own shape. The `let`-bound rebind must not have
    //     regressed into a leak now that the guard covers more sources.
    assert_clean_asan_run(
        "fn main() {\n\
             \x20   let a: Array[String, 2] = [f\"aaaaaaaa0\", f\"bbbbbbbb0\"];\n\
             \x20   let b: Array[String, 2] = a;\n\
             \x20   println(f\"s:{b[0]}\");\n\
             }\n",
        &["s:aaaaaaaa0"],
        "b4-let-bound-rebind-control",
    );
}

/// B-2026-09-13-16 — the CONSUMING controls for the wrapped-array-return
/// fix, which is the half that can be asserted leak-free.
///
/// That row is a use-after-free: `fn wrap(a: Array[String, 2]) ->
/// Option[Array[String, 2]] { return Some(a); }` freed the param's `N`
/// buffers at callee exit while the returned `Option` still carried their
/// `{ptr,len,cap}` triples. Its observable is WRONG OUTPUT, so the wrapping
/// cell itself is pinned in `tests/codegen.rs`
/// (`e2e_array_param_wrapped_into_an_option_return_survives_the_callee`) —
/// it cannot live here, because retracting the callee's drop leaves the
/// buffers to the caller's ARM-BOUND binding, whose owner is the one
/// B-2026-09-13-2 left standing, so the cell still leaks the 132 B that row
/// records for its `passthru` sibling. Asserting zero here would fail for a
/// defect this fix does not own.
///
/// What DOES belong here is the other direction, which is the dangerous
/// one: a disarm that fires too widely retracts the callee's drop where
/// nothing else registers one, and the buffers get no owner at all. Each
/// cell below consumes its param and must stay at `definitely lost: 0`:
///   - `eat` returns a SCALAR while naming the param in its return, the
///     shape a "mentioned in the return" rule would wrongly strip;
///   - `sink` consumes it and returns nothing;
///   - `passthru_arr` is the bare `return a;` the original B-2026-08-24-5
///     disarm was built for, which must keep working unchanged.
#[test]
fn asan_consuming_array_params_keep_their_callee_drop() {
    assert_clean_asan_run(
        r#"
fn eat(a: Array[String, 2]) -> i64 { return a[0].len(); }
fn sink(a: Array[String, 2]) { println(f"s:{a[1]}"); }
fn passthru_arr(a: Array[String, 2]) -> Array[String, 2] { return a; }

fn main() {
    let mut j: i64 = 0;
    while j < 3 {
        let e2: Array[String, 2] = [f"eat-aaaaaaaaaaaaaaaa-{j}", f"eat-bbbbbbbbbbbbbbbb-{j}"];
        println(f"e:{eat(e2)}");
        let e3: Array[String, 2] = [f"snk-aaaaaaaaaaaaaaaa-{j}", f"snk-bbbbbbbbbbbbbbbb-{j}"];
        sink(e3);
        let e4: Array[String, 2] = [f"pt-aaaaaaaaaaaaaaaa-{j}", f"pt-bbbbbbbbbbbbbbbb-{j}"];
        let r = passthru_arr(e4);
        println(f"p:{r[0]}");
        j = j + 1;
    }
    println("end");
}
"#,
        &[
            "e:22",
            "s:snk-bbbbbbbbbbbbbbbb-0",
            "p:pt-aaaaaaaaaaaaaaaa-0",
            "e:22",
            "s:snk-bbbbbbbbbbbbbbbb-1",
            "p:pt-aaaaaaaaaaaaaaaa-1",
            "e:22",
            "s:snk-bbbbbbbbbbbbbbbb-2",
            "p:pt-aaaaaaaaaaaaaaaa-2",
            "end",
        ],
        "asan_consuming_array_params_keep_their_callee_drop",
    );
}

/// B-2026-09-13-17 — the `Option[Array[T, N]]` a `Vec` POP hands back was
/// owned by nobody. `let o = v.pop()` over a `Vec[Array[String, 2]]` leaked
/// 176 B in 8 blocks over four pops: the box reclaimed, its eight `String`
/// buffers not — the same "box reclaimed, contents not" signature
/// B-2026-09-13-2 records for every route it fixed.
///
/// THE RECEIVER PREDICATE IS THE WHOLE ROW. B-2026-09-13-2 fixed the `Map`
/// hand-back of the identical value shape and filed this one rather than
/// guessing, because its own registration keys on `map_key_type_exprs` and
/// it judged there was "no equally settled `this receiver is a tracked Vec`
/// test to key on, and inventing one under an ownership registration is how
/// a leak fix becomes a double free". One exists and has been load-bearing
/// since B-2026-07-12-4: `nested_option_shared_pop_inner_drop` registers an
/// inner drop for a popped `Option[shared T]` keyed on exactly this method
/// set and exactly this table, so the fix inherits a shipped answer.
///
/// The sole-ownership argument is the `Map` sibling's: `pop` decrements
/// `len` and the buffer drain walks `0..len`, so the popped slot is beyond
/// the walked range and the container can no longer reach it.
///
/// CELLS THAT MUST NOT DOUBLE-FREE: `first`/`last` (borrow-shaped, aliasing
/// live storage — the reason `get` is absent from the `Map` list),
/// `moved-to-callee` (the popped array handed to an owning param, whose
/// callee frees it — this is why only three of four pops leaked before),
/// `str`/`vec` (a `Vec[String]` and `Vec[Vec[String]]` pop, which the
/// predicate declines because the element is not an array), `scalar` (an
/// `Array[i64, 3]` with nothing to free), `empty` (a pop past the end,
/// returning `None`), and `maprm` (B-2026-09-13-2's own route, which must
/// stay exactly as it was).
///
/// Measured at `-O0`: 132 B in 6 blocks before, clean after. Six is three
/// arrays of two `String`s — the two bound arms and the wildcard — with the
/// fourth pop's array freed by the callee it was moved into.
///
/// THE FIXTURE POPS THE `Vec` EMPTY DELIBERATELY, and that is not tidiness.
/// A `Vec[Array[..]]` still holding elements at teardown leaks them through
/// a different open row (B-2026-09-10-36, fed from a temporary), and
/// feeding one from a block-scoped named local corrupts through another
/// (B-2026-09-14-14). Draining it is what makes this fixture measure THIS
/// row rather than either of those.
///
/// NOT COVERED, because it is a different registration: the FRESH-TEMP
/// spelling `match v.pop() { .. }` with no binding in between. It still
/// leaks the same 176 B, unchanged by this fix, because it routes through
/// the boxed fresh-temp scrutinee path rather than the `let`-binding one —
/// that is B-2026-09-14-13, filed with its own measurements.
#[test]
fn asan_vec_pop_handback_owns_its_array_interior() {
    assert_clean_asan_run(
        r#"
fn take(a: Array[String, 2]) -> i64 {
    println(f"moved-to-callee:{a[0]}");
    return 1;
}

fn main() {
    let mut v: Vec[Array[String, 2]] = Vec.new();
    let mut i: i64 = 0;
    while i < 4 {
        v.push(Array[f"row-aaaaaaaaaaaaaaaa-{i}", f"col-bbbbbbbbbbbbbbbb-{i}"]);
        i = i + 1;
    }
    match v.first() { Some(a) => { println(f"first:{a[0]}"); } None => {} }
    match v.last() { Some(a) => { println(f"last:{a[1]}"); } None => {} }
    let o0 = v.pop();
    match o0 { Some(a) => { println(f"bound:{a[0]}"); } None => {} }
    let o1 = v.pop();
    match o1 { Some(_) => { println("wild"); } None => {} }
    let o2 = v.pop();
    match o2 { Some(a) => { let _ = take(a); } None => {} }
    let o3 = v.pop();
    match o3 { Some(a) => { println(f"bound:{a[0]}"); } None => {} }
    let o4 = v.pop();
    match o4 { Some(a) => { println(f"never:{a[0]}"); } None => { println("empty"); } }

    let mut sv: Vec[String] = Vec.new();
    sv.push(f"str-cccccccccccccccc-0");
    let so = sv.pop();
    match so { Some(s) => { println(f"str:{s}"); } None => {} }

    let mut nv: Vec[Vec[String]] = Vec.new();
    let mut inner: Vec[String] = Vec.new();
    inner.push(f"vec-dddddddddddddddd-0");
    nv.push(inner);
    let no = nv.pop();
    match no { Some(w) => { println(f"vec:{w[0]}"); } None => {} }

    let mut iv: Vec[Array[i64, 3]] = Vec.new();
    iv.push(Array[7, 8, 9]);
    let io = iv.pop();
    match io { Some(a) => { println(f"scalar:{a[0]}"); } None => {} }

    let mut m: Map[i64, Array[String, 2]] = Map.new();
    m.insert(1, Array[f"map-gggggggggggggggg-0", f"map-hhhhhhhhhhhhhhhh-1"]);
    let mo = m.remove(1);
    match mo { Some(a) => { println(f"maprm:{a[0]}"); } None => {} }

    println("end");
}
"#,
        &[
            "first:row-aaaaaaaaaaaaaaaa-0",
            "last:col-bbbbbbbbbbbbbbbb-3",
            "bound:row-aaaaaaaaaaaaaaaa-3",
            "wild",
            "moved-to-callee:row-aaaaaaaaaaaaaaaa-1",
            "bound:row-aaaaaaaaaaaaaaaa-0",
            "empty",
            "str:str-cccccccccccccccc-0",
            "vec:vec-dddddddddddddddd-0",
            "scalar:7",
            "maprm:map-gggggggggggggggg-0",
            "end",
        ],
        "asan_vec_pop_handback_owns_its_array_interior",
    );
}

/// B-2026-09-14-14 + B-2026-09-10-36 — the `Vec[Array[T, N]]` ownership
/// PAIR. Neither half is correct alone, which is why one fixture covers
/// both.
///
/// THE CORRUPTION HALF (-14-14). A block-scoped named `Array` local moved
/// into `Vec.push` was never disarmed, so the container and the local
/// aliased the same element buffers and the local's scope-exit drop freed
/// what the `Vec` still pointed at. Reads returned garbage on both compiled
/// backends while `--interp` was correct, at exit code 0 with no
/// diagnostic. At FUNCTION scope the drop runs after every read so the
/// window never opens — the `fn:` cell — which is why this hid. A leak
/// check sees nothing either: the buffers ARE freed, once too early.
///
/// ALL FOUR ELEMENT-MOVING ARMS take the argument by move and all four
/// corrupted identically; `push`/`push_back`, `try_push`/`try_push_back`,
/// `push_front` and `try_push_front` each carry the standdown, because a
/// pattern-matched edit catching only the first pair leaves the rest
/// dangling — measured that way mid-fix.
///
/// THE LEAK HALF (-10-36). With the source stood down, nobody freed the
/// elements at all: `vec_element_drain_fn`'s policy has no `Array` case, so
/// the container never had an element drop. Before the pair, the two errors
/// CANCELLED for a function-scope source — one owner too many against one
/// walk too few — which is why `fn:` measured clean on main and would have
/// regressed to a leak had the standdown landed by itself.
///
/// WHERE THE CONTAINER HALF GOES, and it is not where four previous
/// attempts put it. `vec_elem_agg_drop_for_type_expr` is the obvious home
/// and has 28 callers; widening it double-freed two boxed-payload paths
/// containing no `Vec` at all. The element walk is chosen at the `Vec`'s
/// REGISTRATION site instead, and the live one for a `let`-bound
/// `Vec[Array[..]]` was found by instrumenting `track_vec_var` with
/// `#[track_caller]` — none of the three dispatches those attempts patched
/// is it, which is why they moved the top-level cells by zero bytes.
///
/// CELLS. `tmp:` is -10-36's own headline repro (temporary-fed, 80 B);
/// `blk:`/`loop:` are -14-14's corruption; `fn:` is the cancelling pair
/// above; `par:` a by-value array param, which corrupted too and is not in
/// either row's text; `front:` the `push_front` arm; `pop:` the hand-back
/// crossing B-2026-09-13-17's route. Must-not-regress: `scalar:` (an
/// `Array[i64, 3]` with nothing to free), `str:` (the `String` element
/// spelling that was always clean and is what identified a MISSING arm),
/// and `map:` (which has had both halves since B-2026-09-12-13 /
/// B-2026-09-13-1 and must stay exactly as it was).
///
/// Measured at `-O0`: 10 invalid reads, 2 invalid frees, 88 B lost in 4
/// blocks and divergent output before; clean and matching after.
#[test]
fn asan_vec_array_element_has_exactly_one_owner() {
    assert_clean_asan_run(
        r#"
fn mk(n: i64) -> Array[String, 2] {
    return Array[f"tmp-aaaaaaaaaaaaaaaa-{n}", f"tmp-bbbbbbbbbbbbbbbb-{n}"];
}
fn takes(a: Array[String, 2], v: mut ref Vec[Array[String, 2]]) {
    v.push(a);
}

fn main() {
    let mut tmpfed: Vec[Array[String, 2]] = Vec.new();
    tmpfed.push(mk(0));
    tmpfed.push(mk(1));
    println(f"tmp:{tmpfed[0][0]}");

    let mut blockfed: Vec[Array[String, 2]] = Vec.new();
    if true {
        let a: Array[String, 2] = Array[f"blk-cccccccccccccccc-0", f"blk-dddddddddddddddd-1"];
        blockfed.push(a);
    }
    println(f"blk:{blockfed[0][0]}");

    let mut loopfed: Vec[Array[String, 2]] = Vec.new();
    let mut i: i64 = 0;
    while i < 3 {
        let e: Array[String, 2] = Array[f"lp-eeeeeeeeeeeeeeee-{i}", f"lp-ffffffffffffffff-{i}"];
        loopfed.push(e);
        i = i + 1;
    }
    println(f"loop:{loopfed[2][0]}");

    let mut fnfed: Vec[Array[String, 2]] = Vec.new();
    let fa: Array[String, 2] = Array[f"fn-gggggggggggggggg-0", f"fn-hhhhhhhhhhhhhhhh-1"];
    fnfed.push(fa);
    println(f"fn:{fnfed[0][0]}");

    let mut paramfed: Vec[Array[String, 2]] = Vec.new();
    takes(Array[f"par-iiiiiiiiiiiiiiii-0", f"par-jjjjjjjjjjjjjjjj-1"], mut paramfed);
    println(f"par:{paramfed[0][0]}");

    let mut frontfed: Vec[Array[String, 2]] = Vec.new();
    if true {
        let b: Array[String, 2] = Array[f"frt-kkkkkkkkkkkkkkkk-0", f"frt-llllllllllllllll-1"];
        frontfed.push_front(b);
    }
    println(f"front:{frontfed[0][0]}");

    let mut popped: Vec[Array[String, 2]] = Vec.new();
    if true {
        let c: Array[String, 2] = Array[f"pop-mmmmmmmmmmmmmmmm-0", f"pop-nnnnnnnnnnnnnnnn-1"];
        popped.push(c);
    }
    let po = popped.pop();
    match po { Some(x) => { println(f"pop:{x[0]}"); } None => {} }

    let mut scalars: Vec[Array[i64, 3]] = Vec.new();
    if true { let s: Array[i64, 3] = Array[7, 8, 9]; scalars.push(s); }
    println(f"scalar:{scalars[0][0]}");

    let mut strs: Vec[String] = Vec.new();
    if true { let t: String = f"str-oooooooooooooooo-0"; strs.push(t); }
    println(f"str:{strs[0]}");

    let mut m: Map[i64, Array[String, 2]] = Map.new();
    if true {
        let d: Array[String, 2] = Array[f"map-pppppppppppppppp-0", f"map-qqqqqqqqqqqqqqqq-1"];
        m.insert(3, d);
    }
    match m.get(3) { Some(x) => { println(f"map:{x[0]}"); } None => {} }

    println("end");
}
"#,
        &[
            "tmp:tmp-aaaaaaaaaaaaaaaa-0",
            "blk:blk-cccccccccccccccc-0",
            "loop:lp-eeeeeeeeeeeeeeee-2",
            "fn:fn-gggggggggggggggg-0",
            "par:par-iiiiiiiiiiiiiiii-0",
            "front:frt-kkkkkkkkkkkkkkkk-0",
            "pop:pop-mmmmmmmmmmmmmmmm-0",
            "scalar:7",
            "str:str-oooooooooooooooo-0",
            "map:map-pppppppppppppppp-0",
            "end",
        ],
        "asan_vec_array_element_has_exactly_one_owner",
    );
}

/// B-2026-09-14-13 — a fresh-temp `Option`/`Result` scrutinee whose payload
/// is a BOXED `Array` lost it, in two different ways depending on the arm,
/// and the two have different causes.
///
/// UNDER A BOUND ARM the box was freed and its element buffers were not
/// (102 B in 6 over three rounds): the payload-kind dispatch in
/// `track_freshtemp_boxed_enum_scrutinee` has arms for a tuple binding, a
/// struct binding, a wildcard resolving to a struct NAME, and a struct
/// destructure — and an `Array[T, N]` has no struct name, so it fell to
/// `_ => None` and the free went box-only. That is the "box reclaimed,
/// contents not" signature B-2026-09-13-2 records for every route it fixed.
///
/// UNDER A WILDCARD ARM nothing was registered at all and the box leaked
/// too (144 B in 3, plus its interior indirectly). The width gate sizes a
/// wildcard payload from a struct-name lookup, which an array declines, so
/// `payload_words` fell to its 1-word default, `payload_words <= area`
/// held, and the arm was `continue`d before any registration. Sizing from
/// the payload TYPE — one step earlier than the name the resolver throws
/// away — is what lets that arm through.
///
/// THE INTERIOR WALK NEEDS A CONSUMPTION GATE, which is the part that had
/// to be measured rather than reasoned. A wildcard binds nothing, so the
/// walk is unconditionally the sole owner. A BINDING is only safe when the
/// arm merely borrows it: `Some(a) => take(a)` and `Some(a) => v.push(a)`
/// hand the array to a destination that frees it, and both aborted 134 with
/// two invalid frees when the walk registered unconditionally. The `push`
/// cell is the sharper one — B-2026-09-10-36 had just given
/// `Vec[Array[..]]` its own element walk, so that destination acquired an
/// owner the same day this registration did.
///
/// AND THE BORROW TEST ALONE IS NOT ENOUGH: `consume_class` scores a
/// FREE-FN ARGUMENT as non-consuming (B-2026-07-23-4 records the quirk), so
/// `take(a)` still double-freed until the gate also asked
/// `bindings_passed_whole_to_free_fn_arg`. `v.push(a)` was declined by the
/// borrow test; `take(a)` needed the second check. Both are in the fixture.
///
/// CELLS. `read`/`wild` are the row's two failing arms; `moved-to-callee`
/// and `pushed` are the two consuming shapes that must NOT double-free;
/// `held` is the `let`-bound spelling that was always clean; `borrow` is a
/// `Map.get` whose box interior aliases the container's storage and is
/// excluded for that reason. `wide`/`wide-wild` (a user struct) and
/// `scalar` (an `Array[i64, 6]` with nothing to free) must stay exactly as
/// they were.
///
/// TWO CELLS BEYOND THE ROW, both leaking before and clean after:
/// `vecarr`, an `Array[Vec[String], 2]` payload (576 B in 12 over three
/// rounds — the largest single cell in the sweep), and `err`, the `Result`
/// Err side, which the row only measured on `Option`.
///
/// Measured at `-O0`: 656 B in 14 blocks before, clean after, with output
/// matching `--interp` throughout.
///
/// NOT COVERED, and this row's own NOT-MEASURED list: the `if let` /
/// `while let` / `let else` siblings. Those three callers supply no arm
/// bodies, so the gate cannot classify a binding there and DECLINES it
/// rather than guessing — a wildcard payload still registers, a bound one
/// keeps the box-only free it had.
#[test]
fn asan_boxed_array_scrutinee_payload_has_exactly_one_owner() {
    assert_clean_asan_run(
        r#"
struct Wide { a: String, b: String, c: String, d: i64 }

fn mkarr(j: i64) -> Option[Array[String, 2]] {
    if j >= 0 { return Option.Some(Array[f"arr-aaaaaaaaaaaaaaaa-{j}", f"arr-bbbbbbbbbbbbbbbb-{j}"]); }
    return Option.None;
}
fn mkvecarr(j: i64) -> Option[Array[Vec[String], 2]] {
    let mut p: Vec[String] = Vec.new();
    p.push(f"vec-cccccccccccccccc-{j}");
    let mut q: Vec[String] = Vec.new();
    q.push(f"vec-dddddddddddddddd-{j}");
    if j >= 0 { return Option.Some(Array[p, q]); }
    return Option.None;
}
fn mkerr(j: i64) -> Result[i64, Array[String, 2]] {
    if j < 0 { return Result.Ok(1); }
    return Result.Err(Array[f"err-eeeeeeeeeeeeeeee-{j}", f"err-ffffffffffffffff-{j}"]);
}
fn mkwide(j: i64) -> Option[Wide] {
    if j >= 0 { return Option.Some(Wide { a: f"wid-gggggggggggggggg-{j}", b: f"wid-hhhhhhhhhhhhhhhh-{j}", c: f"wid-iiiiiiiiiiiiiiii-{j}", d: j }); }
    return Option.None;
}
fn mkscalar(j: i64) -> Option[Array[i64, 6]] {
    if j >= 0 { return Option.Some(Array[j, j+1, j+2, j+3, j+4, j+5]); }
    return Option.None;
}
fn take(a: Array[String, 2]) -> i64 { println(f"moved-to-callee:{a[0]}"); return 1; }

fn main() {
    let mut j: i64 = 0;
    while j < 2 {
        match mkarr(j) { Option.Some(a) => { println(f"read:{a[0]}"); } Option.None => {} }
        match mkarr(j) { Option.Some(_) => { println("wild"); } Option.None => {} }
        match mkarr(j) { Option.Some(a) => { let _ = take(a); } Option.None => {} }
        match mkvecarr(j) { Option.Some(a) => { println(f"vecarr:{a[0][0]}"); } Option.None => {} }
        match mkerr(j) { Result.Ok(v) => { println(f"ok:{v}"); } Result.Err(e) => { println(f"err:{e[0]}"); } }
        match mkwide(j) { Option.Some(w) => { println(f"wide:{w.a}"); } Option.None => {} }
        match mkwide(j) { Option.Some(_) => { println("wide-wild"); } Option.None => {} }
        match mkscalar(j) { Option.Some(s) => { println(f"scalar:{s[0]}"); } Option.None => {} }
        let held = mkarr(j);
        match held { Option.Some(a) => { println(f"held:{a[0]}"); } Option.None => {} }
        j = j + 1;
    }

    let mut v: Vec[Array[String, 2]] = Vec.new();
    match mkarr(0) { Option.Some(a) => { v.push(a); } Option.None => {} }
    println(f"pushed:{v[0][0]}");

    let mut m: Map[i64, Array[String, 2]] = Map.new();
    m.insert(5, Array[f"map-jjjjjjjjjjjjjjjj-0", f"map-kkkkkkkkkkkkkkkk-1"]);
    match m.get(5) { Option.Some(a) => { println(f"borrow:{a[0]}"); } Option.None => {} }
    println(f"maplen:{m.len()}");

    println("end");
}
"#,
        &[
            "read:arr-aaaaaaaaaaaaaaaa-0",
            "wild",
            "moved-to-callee:arr-aaaaaaaaaaaaaaaa-0",
            "vecarr:vec-cccccccccccccccc-0",
            "err:err-eeeeeeeeeeeeeeee-0",
            "wide:wid-gggggggggggggggg-0",
            "wide-wild",
            "scalar:0",
            "held:arr-aaaaaaaaaaaaaaaa-0",
            "read:arr-aaaaaaaaaaaaaaaa-1",
            "wild",
            "moved-to-callee:arr-aaaaaaaaaaaaaaaa-1",
            "vecarr:vec-cccccccccccccccc-1",
            "err:err-eeeeeeeeeeeeeeee-1",
            "wide:wid-gggggggggggggggg-1",
            "wide-wild",
            "scalar:1",
            "held:arr-aaaaaaaaaaaaaaaa-1",
            "pushed:arr-aaaaaaaaaaaaaaaa-0",
            "borrow:map-jjjjjjjjjjjjjjjj-0",
            "maplen:1",
            "end",
        ],
        "asan_boxed_array_scrutinee_payload_has_exactly_one_owner",
    );
}

/// B-2026-09-14-21 — a DISCARDED same-type assoc-fn enum whose payload is a
/// heap-boxed `Array` was owned by nobody, losing its box, its interior AND
/// its `Drop` bodies.
///
/// TWO CORRECT CHANGES COMBINED INTO A HOLE, which is the whole lesson and
/// why this fixture carries the controls for both of them.
/// `track_inline_owned_aggregate_arg_inst` stands down when
/// `enum_param_owned_by_transfer` is true (B-2026-09-07-16), on the premise
/// "the callee owns this temp outright" — right for an ARGUMENT, and it
/// really did fix a double free. B-2026-09-14-12 then added `BoxedArray` to
/// that predicate, right for the by-value param it was fixing. At a
/// DISCARD there is no callee, so the stand-down left the value unowned.
///
/// The cell leaked the same 144 B BEFORE that second change, for an
/// unrelated reason (`heap_payload` was false, the switch having no arm for
/// the payload), so the symptom never moved while the mechanism did — which
/// is how the owning row came to blame the wrong function in its own
/// "WHERE TO LOOK".
///
/// Measured at `-O0` under valgrind, before -> after, whole fixture:
/// 464 B in 10 blocks plus 336 B indirect in 16 -> clean. And the BODIES,
/// which no leak check can see: `main` printed `round:0 round:1 end` with
/// every `dR` missing; the fix prints `dR0 dR100` per round, identical
/// under `--interp`, `-O0` and `KARAC_AUTO_PAR=0`. That half makes the
/// defect a run-vs-build divergence rather than the leak it was filed as.
///
/// Cells: `Array[String, 2]` (box + interior), `Array[i64, 3]` (a box with
/// NO interior — 72 B, which is what proves the box itself was unowned
/// rather than just its contents), `Array[R, 2]` with `impl Drop for R`
/// (the bodies), and the `if` / `match` branch-tail spellings, since the
/// bare discard is one of three positions that reach this registrar.
/// `Es { A(String) }` is the inline-payload control that was always clean.
///
/// The ARGUMENT side is deliberately untouched: the stand-down still fires
/// for real arguments, which is why
/// `asan_discarded_cross_type_assoc_enum_has_exactly_one_owner` and the
/// `Drop`-bearing same-type cells beside it must stay green — they are what
/// would catch this being "fixed" by loosening the shared predicate instead
/// of separating the two positions.
#[test]
fn asan_discarded_boxed_array_assoc_temp_has_exactly_one_owner() {
    assert_clean_asan_run(
        r#"
struct R { id: i64, s: String }

impl Drop for R {
    fn drop(mut ref self) { println(f"dR{self.id}"); }
}

enum Ea { A(Array[String, 2]), B }
enum Eg { A(Array[i64, 3]), B }
enum Ed { A(Array[R, 2]), B }
enum Es { A(String), B }

impl Ea {
    fn mk(n: i64) -> Ea {
        let p: Array[String, 2] = [f"bx-aaaaaaaaaaaaaaaa-{n}", f"bx-bbbbbbbbbbbbbbbb-{n}"];
        return Ea.A(p);
    }
}
impl Eg {
    fn mk(n: i64) -> Eg {
        let p: Array[i64, 3] = [n, n + 1, n + 2];
        return Eg.A(p);
    }
}
impl Ed {
    fn mk(n: i64) -> Ed {
        let p: Array[R, 2] = [R { id: n, s: f"dr-cccccccccccccccc-{n}" }, R { id: n + 100, s: f"dr-dddddddddddddddd-{n}" }];
        return Ed.A(p);
    }
}
impl Es {
    fn mk(n: i64) -> Es { return Es.A(f"st-eeeeeeeeeeeeeeee-{n}"); }
}

fn main() {
    let mut j: i64 = 0;
    while j < 2 {
        Ea.mk(j);
        Eg.mk(j);
        Ed.mk(j);
        Es.mk(j);

        let c: bool = j > 0;
        let _ = if c { Ea.mk(j) } else { Ea.mk(j) };
        let _ = match j {
            0 => { Ea.mk(j) }
            _ => { Ea.mk(j) }
        };

        println(f"round:{j}");
        j = j + 1;
    }
    println("end");
}
"#,
        &["dR0", "dR100", "round:0", "dR1", "dR101", "round:1", "end"],
        "asan_discarded_boxed_array_assoc_temp_has_exactly_one_owner",
    );
}

/// B-2026-09-14-12 — an enum whose variant payload is a heap-BOXED
/// `Array[T, N]` was freed ONLY by the five sites that registered a
/// `BoxedEnumDrop` explicitly (`let`, by-value param, return, mono, and
/// B-2026-09-14-9's discard). Everywhere else the value is reached through
/// the enum's own DROP SWITCH, and the switch had no case for it: held in a
/// struct FIELD, a `Vec` element or a `Map` value it lost its box and
/// interior at the container's destruction, with no discard anywhere in the
/// program.
///
/// The repair classifies the payload `EnumDropKind::BoxedArray`, which
/// HANDS those five sites' job to the switch rather than adding a sixth
/// owner — `user_enum_boxed_payload_variants` stands a variant down the
/// moment its kind is not `None`. That is why the arm frees the box AND
/// walks the interior, where the `BoxedOptRes` arm beside it frees the box
/// alone.
///
/// Measured at `-O0` under valgrind, three rounds per cell, before → after:
///
/// * struct field / `Vec` element / `Map` value — 144 B in 3 blocks plus
///   interior, each → clean (the row's own three cells).
/// * `Array[i64, 3]` in a `Vec` — 72 B in 3, pure box, no interior → clean.
/// * `Array[Vec[String], 2]` in a `Vec` — 144 B + 594 B indirect in 12 →
///   clean.
/// * `v.clone()` and `#[derive(Clone)]` over the enum — 144 B + interior →
///   clean (the clone had to learn to DEEP-copy the box; a shallow copy
///   aliases the element buffers the new interior walk frees).
/// * `enum S { A(Array[i64, 2]), C(String) }` — 48 B in 3 → clean, and
///   `enum K { A(Array[String, 2]), C(Wide) }` — 144 B + interior → clean.
///   These two are the reason the boxing test reads the FIELD's word slot
///   and not the enum-wide area: both have a wider sibling variant, both
///   are boxed by the pack side all the same, and an area test declines
///   them.
/// * by-value param (`eat(e)` and `eat(mk(i))`) and the bare discard
///   `mk(j);` — clean before AND after, and the three cells that caught
///   three wrong intermediate fixes. All three are the same mistake:
///   `enum_has_heap_payload` folds `is_heap_bearing()`, which answers "must
///   the entry copy duplicate this?" and not "does this need an owner at
///   scope exit?" — and classifying the payload stands the explicit
///   registration down, so a gate that then declines leaves the value owned
///   by nobody. Without the `param_own` admission the callee registered
///   nothing (144 B); without the transfer clause both frames registered
///   against one box (7 invalid frees); without routing the discard gate
///   through `enum_needs_scope_exit_owner` the temp owned nothing (144 B),
///   which is B-2026-09-14-9's own cell and was caught by ITS fixture on
///   the ASAN ratchet leg, after `cargo test --features llvm` had gone
///   green — the Commands block's warning about those legs, paying for
///   itself.
///
/// NOT COVERED, deliberately: a match arm that CONSUMES the payload
/// (`E.A(a) => take(a)`). That aborts 134 before and after this fix — it is
/// B-2026-09-14-17, whose cause is a different predicate
/// (`boxed_payload_interior_taken_by_arm` answering FALSE for every array),
/// and giving the value an owner does not repair a wrong answer about who
/// the owner is. The fresh-temp spelling of that one arm is the single cell
/// this fix moves in the wrong direction — from a 144 B leak to that same
/// abort — because the temp previously had no owner at all; its read-only
/// and wildcard siblings go from 288 B + 78 B indirect to clean.
#[test]
fn asan_boxed_array_enum_payload_is_freed_by_the_drop_switch() {
    assert_clean_asan_run(
        r#"
enum E { A(Array[String, 2]), B }
enum N { A(Array[Vec[String], 2]), B }
enum G { A(Array[i64, 3]), B }
struct Wide { a: i64, b: i64, c: i64, d: i64, e: i64, f: i64, g: i64, h: i64 }
enum K { A(Array[String, 2]), C(Wide) }
enum S { A(Array[i64, 2]), C(String) }

struct Held { e: E, k: i64 }

#[derive(Clone)]
struct HeldC { e: E, k: i64 }

fn mk(n: i64) -> E {
    let a: Array[String, 2] = [f"ba-left-aaaaaaaaaaaaaaaa-{n}", f"ba-right-bbbbbbbbbbbbbbbb-{n}"];
    return E.A(a);
}
fn mkn(n: i64) -> N {
    let mut p: Vec[String] = Vec.new();
    p.push(f"ba-vec-cccccccccccccccc-{n}");
    let mut q: Vec[String] = Vec.new();
    q.push(f"ba-vec-dddddddddddddddd-{n}");
    let a: Array[Vec[String], 2] = [p, q];
    return N.A(a);
}
fn mkg(n: i64) -> G {
    let a: Array[i64, 3] = [n, n + 1, n + 2];
    return G.A(a);
}
fn mkk(n: i64) -> K {
    let a: Array[String, 2] = [f"ba-wide-eeeeeeeeeeeeeeee-{n}", f"ba-wide-ffffffffffffffff-{n}"];
    return K.A(a);
}
fn mks(n: i64) -> S {
    let a: Array[i64, 2] = [n, n + 10];
    return S.A(a);
}
fn eat(e: E) -> i64 { return 1; }

fn main() {
    let mut j: i64 = 0;
    while j < 2 {
        let h = Held { e: mk(j), k: j };
        println(f"field:{h.k}");

        let held: E = mk(j);
        println(f"let:{j}");

        let named: E = mk(j);
        println(f"named:{eat(named)}");
        println(f"temp:{eat(mk(j))}");

        match mk(j) {
            E.A(a) => { println(f"read:{a[0]}"); }
            E.B => {}
        }

        match mkk(j) {
            K.A(a) => { println(f"widearea:{a[0]}"); }
            K.C(w) => { println(f"wide:{w.a}"); }
        }

        match mks(j) {
            S.A(a) => { println(f"inline:{a[0]}"); }
            S.C(s) => { println(f"str:{s}"); }
        }

        let hc = HeldC { e: mk(j), k: j };
        let hc2 = hc.clone();
        println(f"structclone:{hc2.k}");

        mk(j);

        j = j + 1;
    }

    let mut v: Vec[E] = Vec.new();
    let mut vn: Vec[N] = Vec.new();
    let mut vg: Vec[G] = Vec.new();
    let mut m: Map[i64, E] = Map.new();
    let mut i: i64 = 0;
    while i < 2 {
        v.push(mk(i));
        vn.push(mkn(i));
        vg.push(mkg(i));
        m.insert(i, mk(i));
        i = i + 1;
    }
    let vc: Vec[E] = v.clone();
    println(f"vec:{v.len()} vecclone:{vc.len()} nested:{vn.len()} scalar:{vg.len()} map:{m.len()}");

    println("end");
}
"#,
        &[
            "field:0",
            "let:0",
            "named:1",
            "temp:1",
            "read:ba-left-aaaaaaaaaaaaaaaa-0",
            "widearea:ba-wide-eeeeeeeeeeeeeeee-0",
            "inline:0",
            "structclone:0",
            "field:1",
            "let:1",
            "named:1",
            "temp:1",
            "read:ba-left-aaaaaaaaaaaaaaaa-1",
            "widearea:ba-wide-eeeeeeeeeeeeeeee-1",
            "inline:1",
            "structclone:1",
            "vec:2 vecclone:2 nested:2 scalar:2 map:2",
            "end",
        ],
        "asan_boxed_array_enum_payload_is_freed_by_the_drop_switch",
    );
}

/// B-2026-09-14-17 — a match arm that CONSUMES a heap-boxed `Array` enum
/// payload freed its element buffers twice and aborted the program: 2
/// invalid frees, `18 allocs / 24 frees`, `free(): double free detected in
/// tcache 2`, `exit 134` on every compiled backend against a correct
/// `--interp`. NINE spellings, one cause.
///
/// WHERE THE ROW POINTED AND WHY IT IS NOT THERE. The row named
/// `boxed_payload_interior_taken_by_arm`'s unconditional FALSE for an array
/// payload, which was right when it was filed and stale by the time it was
/// worked: B-2026-09-14-12 landed in between and classified the payload
/// `EnumDropKind::BoxedArray`, which stands the five explicit
/// `BoxedEnumDrop` registrations down and hands the interior to the enum's
/// own drop SWITCH. That predicate is consulted only by those
/// registrations, so for this shape it is no longer reached at all —
/// instrumenting it printed nothing for any cell here. The retraction it
/// gates (`clear_boxed_enum_inner_drop`) likewise mutates a `CleanupAction`
/// that no longer exists, so both halves of the named repair would have
/// been no-ops.
///
/// WHAT IT ACTUALLY IS. The switch's `BoxedArray` arm frees the box AND
/// walks its interior, and a drop switch is emitted once per ENUM and
/// cannot see any arm. The ordinary match-out cap-zeroing declines the
/// position because `EnumDropKind::is_heap_bearing()` is false for
/// `BoxedArray` — correct for the sibling `BoxedOptRes`, whose arm is
/// box-only, and wrong here, where the interior IS freed. So the source
/// keeps its interior owner while the arm hands the same buffers to a
/// second one.
///
/// THE REPAIR records the arm's binding as an alias of the box interior
/// (`register_boxed_array_payload_alias`) and zeroes the BOX'S CONTENTS —
/// not the enum's payload word — at the hand-off, so the interior walk
/// becomes a no-op while the envelope keeps the owner that frees it.
/// Zeroing the word instead would strand the envelope, the leak
/// `is_heap_bearing`'s doc warns about.
///
/// IT HOOKS THE TWO SHARED HAND-OFF HELPERS rather than the call sites.
/// `suppress_array_binding_move_arg` and `suppress_array_local_move_into_ctor`
/// are already invoked from every position that mints a new owner for an
/// array, so the read-only and rebind arms stay clean BY CONSTRUCTION —
/// neither reaches a hand-off. That matters: a syntactic arm scan gets the
/// rebind wrong in both directions, and `call_dispatch`'s own note on the
/// `Option` sibling records `let u = t` measured as a fresh 72 B leak when
/// the retraction was widened that way.
///
/// MEASURED at `KARAC_OPT_LEVEL=0` under valgrind, and on all four surfaces
/// (`--interp`, JIT, `karac build`, `-O0` no-auto-par), before → after:
///
/// * free-fn arg, method arg (`v.push`), struct literal, by-value param,
///   FRESH-TEMP scrutinee, `if let`, `let else`, and the FRESH-TEMP forms of
///   those last two — each `exit 134` with 2 invalid frees → clean, and the
///   whole program `142 allocs / 142 frees`.
///
/// The two fresh-temp `if let` / `let else` cells are a SECOND registration
/// site, not a second cause: those legs call
/// `suppress_destructured_enum_payload_cleanup_at` directly and so bypassed
/// the registration the named-scrutinee leg gets inside
/// `suppress_destructured_enum_payload_cleanup`. They were still aborting
/// after the first seven were clean, which is why they are pinned here.
/// * the method-arg cell aborted at the DEFAULT opt level too, not only at
///   `-O0`; the other six had their double free deleted by `-O2` and shipped
///   the abort at `-O0` alone.
/// * read-only arm, rebind arm, inline `String` payload, `Array[i64, 3]`
///   payload — clean before AND after, and the four cells that pin the
///   disarm to hand-offs rather than to "the arm binds something".
///
/// THE `Drop`-BODY ELEMENT IS DELIBERATELY EXCLUDED, and the `dropelem`
/// cell below is what holds that line. `Array[D, 2]` for a `D` with an
/// `impl Drop` and NO heap is clean already, and admitting it turned a
/// correct `body:7` / `body:107` into `body:0` / `body:0` — the payload's
/// BODIES walker reads the words this disarm zeroes. The variant where the
/// element carries heap AND a body double-frees today and is not repaired
/// here, because the same zeroing would trade its abort for silent wrong
/// output; it is filed with its measurements rather than half-fixed.
#[test]
fn asan_consuming_arm_over_a_boxed_array_payload_frees_the_interior_once() {
    assert_clean_asan_run(
        r#"
struct D { id: i64 }
impl Drop for D { fn drop(mut ref self) { println(f"body:{self.id}"); } }

enum E { A(Array[String, 2]), B }
enum G { A(Array[i64, 3]), B }
enum Bd { A(Array[D, 2]), B }
enum E3 { A(String), B }

struct W { inner: Array[String, 2] }

fn mk(n: i64) -> E {
    let a: Array[String, 2] = [f"ba17-left-aaaaaaaaaaaaaaaa-{n}", f"ba17-right-bbbbbbbbbbbbbbbb-{n}"];
    return E.A(a);
}
fn mkg(n: i64) -> G { let a: Array[i64, 3] = [n, n + 1, n + 2]; return G.A(a); }
fn mkb(n: i64) -> Bd { let a: Array[D, 2] = [D { id: n }, D { id: n + 100 }]; return Bd.A(a); }
fn mk3(n: i64) -> E3 { return E3.A(f"ba17-inline-cccccccccccccccc-{n}"); }

fn take(a: Array[String, 2]) -> i64 { return a[0].len(); }
fn takeg(a: Array[i64, 3]) -> i64 { return a[0] + a[2]; }
fn takeb(a: Array[D, 2]) -> i64 { return a[0].id; }
fn take3(s: String) -> i64 { return s.len(); }

fn eat(e: E) -> i64 {
    match e {
        E.A(a) => { return take(a); }
        E.B => { return 0; }
    }
}
fn viaTempLetElse(n: i64) -> i64 {
    let E.A(a) = mk(n) else { return 0; };
    return take(a);
}
fn viaLetElse(n: i64) -> i64 {
    let e: E = mk(n);
    let E.A(a) = e else { return 0; };
    return take(a);
}

fn main() {
    let mut i: i64 = 0;
    while i < 2 {
        let e1: E = mk(i);
        match e1 {
            E.A(a) => { println(f"freefn:{take(a)}"); }
            E.B => {}
        }

        let mut v: Vec[Array[String, 2]] = Vec.new();
        let e2: E = mk(i);
        match e2 {
            E.A(a) => { v.push(a); }
            E.B => {}
        }
        println(f"method:{v.len()}");

        let e3: E = mk(i);
        match e3 {
            E.A(a) => { let w = W { inner: a }; println(f"structlit:{w.inner[0].len()}"); }
            E.B => {}
        }

        println(f"param:{eat(mk(i))}");

        match mk(i) {
            E.A(a) => { println(f"freshtemp:{take(a)}"); }
            E.B => {}
        }

        let e4: E = mk(i);
        if let E.A(a) = e4 { println(f"iflet:{take(a)}"); }

        println(f"letelse:{viaLetElse(i)}");

        if let E.A(a) = mk(i) { println(f"tempiflet:{take(a)}"); }

        println(f"templetelse:{viaTempLetElse(i)}");

        let e5: E = mk(i);
        match e5 {
            E.A(a) => { println(f"readonly:{a[1].len()}"); }
            E.B => {}
        }

        let e6: E = mk(i);
        match e6 {
            E.A(a) => { let b = a; println(f"rebind:{b[1].len()}"); }
            E.B => {}
        }

        let e7: E3 = mk3(i);
        match e7 {
            E3.A(s) => { println(f"inline:{take3(s)}"); }
            E3.B => {}
        }

        let e8: G = mkg(i);
        match e8 {
            G.A(a) => { println(f"ints:{takeg(a)}"); }
            G.B => {}
        }

        let e9: Bd = mkb(i);
        match e9 {
            Bd.A(a) => { println(f"dropelem:{takeb(a)}"); }
            Bd.B => {}
        }

        i = i + 1;
    }
}
"#,
        &[
            "freefn:28",
            "method:1",
            "structlit:28",
            "param:28",
            "freshtemp:28",
            "iflet:28",
            "letelse:28",
            "tempiflet:28",
            "templetelse:28",
            "readonly:29",
            "rebind:29",
            "inline:30",
            "ints:2",
            "dropelem:0",
            "body:0",
            "body:100",
            "freefn:28",
            "method:1",
            "structlit:28",
            "param:28",
            "freshtemp:28",
            "iflet:28",
            "letelse:28",
            "tempiflet:28",
            "templetelse:28",
            "readonly:29",
            "rebind:29",
            "inline:30",
            "ints:4",
            "dropelem:1",
            "body:1",
            "body:101",
        ],
        "asan_consuming_arm_over_a_boxed_array_payload_frees_the_interior_once",
    );
}

/// B-2026-09-14-25 — an `Array[D, N]` enum payload whose element carries
/// BOTH heap and a user `Drop` body double-freed its element buffers on a
/// consuming arm: `exit 134`, 2 invalid frees, `15 allocs / 17 frees`, at
/// BOTH opt levels, against a correct `--interp`.
///
/// Filed as the shape B-2026-09-14-17 deliberately excluded, and the reason
/// it could not ride that fix is that the two owners are on different
/// channels: -17 zeroes the box's CONTENTS so the interior walk skips, and
/// the payload's BODIES walker reads those same words afterwards. Admitting
/// the element there turned a correct `body:7` into `body:0`.
///
/// THE ENUM TURNED OUT NOT TO BE THE CAUSE, and the cell that showed it has
/// no enum in it. A plain `let a: Array[D, 2]` passed by value to
/// `fn take(a: Array[D, 2])` prints GARBAGE in its `Drop` bodies on both
/// compiled backends at exit 0 — `body-7-<binary junk>` — while `--interp`
/// is correct. The callee frees the element heap under the callee-owns
/// convention `owned_array_param_te` applies, and the CALLER still runs the
/// elements' `Drop` bodies at its own scope exit, over buffers that frame
/// already returned. Put that same array in a boxed enum payload and the
/// payload's interior walk frees them a second time, which is the abort.
///
/// THE CONVENTION IS THE DEFECT, and the two sibling shapes settle which
/// way it should go. Measured on `fn taked(d: D)` and `fn takev(v: Vec[D])`,
/// the body prints AFTER the call statement on all four surfaces — the
/// caller is still holding the value when it runs, i.e. a by-value
/// aggregate param is CALLER-RETAINS for both memory and bodies. `Array` was
/// the only aggregate that was callee-owns, so it is the outlier rather
/// than the precedent.
///
/// THE REPAIR withholds callee-ownership for exactly the elements that run
/// a user `Drop` body (`array_elem_owns_callee_drop`). Because the caller's
/// retraction and the callee's registration are ONE predicate — as that
/// function's own doc insists — a single line flips both, and there is no
/// window in which one side has moved and the other has not.
///
/// THE REJECTED ALTERNATIVE, recorded because it looks like the obvious
/// one: move the BODIES to the callee instead, so both halves live with the
/// new owner. It was implemented and measured. It fixes the memory and runs
/// each body INSIDE the callee, before the caller's call statement
/// finishes — diverging from `--interp` and from both sibling shapes above.
/// Trading an abort for a run-vs-build divergence is the same bad trade
/// -17 refused, so the convention was restored instead of extended.
///
/// MEASURED at `KARAC_OPT_LEVEL=0` under valgrind, all four surfaces,
/// before → after: the consuming arm over `Hd` — `exit 134`, 2 invalid
/// frees → clean, and the whole fixture `78 allocs / 78 frees`,
/// byte-identical on `--interp`, JIT, `karac build` and `-O0` no-auto-par.
///
/// CONTROLS, clean before AND after, each pinning one edge of the gate:
/// the READ-ONLY arm over the same enum (already clean, so the gate must
/// not disturb it); an `Array[P, 2]` whose element has a `Drop` body and NO
/// heap (clean before — it is the cell that proves the heap is what makes
/// the difference); an `Array[String, 2]` payload, which has no user `Drop`
/// at all and so STAYS callee-owned, keeping B-2026-09-13-15 and
/// B-2026-09-13-16's leaks closed; and `v.push` of a `Drop`-bearing array,
/// which was already correct and is the reference the repair reasons from.
///
/// ONE KNOWN GAP IS PINNED HERE RATHER THAN FIXED: the FRESH-TEMP consuming
/// arm (`consume:` below) runs NO element bodies at all. It does so on
/// every surface including `--interp`, so it is a both-backends-silent gap
/// (B-2026-09-10-7's family) and not a divergence this row introduced —
/// asserted as measured so that a change either way is noticed.
///
/// A WRONG FIRST CUT, and the reason the predicate is split in two. The
/// exclusion first went into `array_elem_owns_callee_drop` itself, which
/// reads as the natural home for it. That predicate has a THIRD consumer:
/// despite its name, `make_array_param_callee_owned` is also what a
/// `let`-bound array local calls to register its own scope-exit element
/// drop, so the exclusion silently removed the LOCAL's memory drop as well.
/// It cost `asan_fixed_array_element_bodies_are_memory_balanced`'s
/// `heap-struct-elems` row 4 bytes in 2 allocations on a program with no
/// function call in it, and the leak was briefly mistaken for a
/// pre-existing defect this fix merely unmasked — it was neither
/// pre-existing nor unmasked, it was caused. Hence
/// `array_param_elem_is_callee_owned`: the by-value-param question, asked
/// by both sides of the transfer and by nobody else.
///
/// THE CALLER'S RETRACTION NEEDED THE SAME GATE, for the mirror reason.
/// `suppress_array_binding_move_arg` keyed on `owned_array_params`
/// MEMBERSHIP, and a `let`-local is in that map too, so it went on
/// retracting the local's drop for a callee that now registers nothing — 44
/// B in 2 blocks on the by-value cell while the identical unmoved local was
/// clean. Both sides now ask one predicate, which is what that function's
/// own doc demands and what the first cut proved by violating.
#[test]
fn asan_drop_bearing_array_element_is_freed_once_through_a_consuming_arm() {
    assert_clean_asan_run(
        r#"
struct D { id: i64, s: String }
impl Drop for D { fn drop(mut ref self) { println(f"hd-{self.id}-{self.s}"); } }

struct P { id: i64 }
impl Drop for P { fn drop(mut ref self) { println(f"np-{self.id}"); } }

enum Hd { A(Array[D, 2]), B }
enum Np { A(Array[P, 2]), B }
enum Sp { A(Array[String, 2]), B }

fn mkhd(n: i64) -> Hd {
    let a: Array[D, 2] = [D { id: n, s: f"b25-left-aaaaaaaaaaaaaaaa-{n}" }, D { id: n + 100, s: f"b25-right-bbbbbbbbbbbbbbbb-{n}" }];
    return Hd.A(a);
}
fn mknp(n: i64) -> Np {
    let a: Array[P, 2] = [P { id: n }, P { id: n + 100 }];
    return Np.A(a);
}
fn mksp(n: i64) -> Sp {
    let a: Array[String, 2] = [f"b25-sp-cccccccccccccccc-{n}", f"b25-sp-dddddddddddddddd-{n}"];
    return Sp.A(a);
}

fn takehd(a: Array[D, 2]) -> i64 { return a[0].id; }
fn takenp(a: Array[P, 2]) -> i64 { return a[0].id; }
fn takesp(a: Array[String, 2]) -> i64 { return a[0].len(); }

fn main() {
    let mut i: i64 = 0;
    while i < 2 {
        match mkhd(i) {
            Hd.A(a) => { println(f"consume:{takehd(a)}"); }
            Hd.B => {}
        }

        let e2: Hd = mkhd(i);
        match e2 {
            Hd.A(a) => { println(f"bound:{takehd(a)}"); }
            Hd.B => {}
        }

        let e3: Hd = mkhd(i);
        match e3 {
            Hd.A(a) => { println(f"readonly:{a[1].id}"); }
            Hd.B => {}
        }

        let e4: Np = mknp(i);
        match e4 {
            Np.A(a) => { println(f"noheap:{takenp(a)}"); }
            Np.B => {}
        }

        let e5: Sp = mksp(i);
        match e5 {
            Sp.A(a) => { println(f"strpay:{takesp(a)}"); }
            Sp.B => {}
        }

        let bv: Array[D, 2] = [D { id: i + 20, s: f"b25-byval-gggggggggggggggg-{i}" }, D { id: i + 21, s: f"b25-byval-hhhhhhhhhhhhhhhh-{i}" }];
        println(f"byval:{takehd(bv)}");

        let pv: Array[D, 2] = [D { id: i + 7, s: f"b25-push-eeeeeeeeeeeeeeee-{i}" }, D { id: i + 8, s: f"b25-push-ffffffffffffffff-{i}" }];
        let mut v: Vec[Array[D, 2]] = Vec.new();
        v.push(pv);
        println(f"push:{v.len()}");

        i = i + 1;
    }
}
"#,
        &[
            "consume:0",
            "bound:0",
            "hd-0-b25-left-aaaaaaaaaaaaaaaa-0",
            "hd-100-b25-right-bbbbbbbbbbbbbbbb-0",
            "readonly:100",
            "hd-0-b25-left-aaaaaaaaaaaaaaaa-0",
            "hd-100-b25-right-bbbbbbbbbbbbbbbb-0",
            "noheap:0",
            "np-0",
            "np-100",
            "strpay:25",
            "byval:20",
            "hd-20-b25-byval-gggggggggggggggg-0",
            "hd-21-b25-byval-hhhhhhhhhhhhhhhh-0",
            "push:1",
            "hd-7-b25-push-eeeeeeeeeeeeeeee-0",
            "hd-8-b25-push-ffffffffffffffff-0",
            "consume:1",
            "bound:1",
            "hd-1-b25-left-aaaaaaaaaaaaaaaa-1",
            "hd-101-b25-right-bbbbbbbbbbbbbbbb-1",
            "readonly:101",
            "hd-1-b25-left-aaaaaaaaaaaaaaaa-1",
            "hd-101-b25-right-bbbbbbbbbbbbbbbb-1",
            "noheap:1",
            "np-1",
            "np-101",
            "strpay:25",
            "byval:21",
            "hd-21-b25-byval-gggggggggggggggg-1",
            "hd-22-b25-byval-hhhhhhhhhhhhhhhh-1",
            "push:1",
            "hd-8-b25-push-eeeeeeeeeeeeeeee-1",
            "hd-9-b25-push-ffffffffffffffff-1",
        ],
        "asan_drop_bearing_array_element_is_freed_once_through_a_consuming_arm",
    );
}

/// B-2026-09-13-19 — a BARE DISCARDED call statement lost a wide `Option`
/// payload and its box.
///
/// `mk(j);` over `fn mk(..) -> Option[Array[String, 2]]` leaked 192 B in 4
/// blocks plus 176 B indirect in 8: the 4 direct blocks are the boxed
/// `[2 x {ptr,len,cap}]` payloads and the 8 indirect are the `String`s
/// reachable through them — the "box unowned, interior unreachable" split.
///
/// `try_track_discarded_boxed_option` already had a TUPLE arm and a STRUCT
/// arm for exactly this shape. An `Array[T, N]` is spelled as a `Path` whose
/// head is `Array`, so it reached the struct branch, failed the
/// `struct_types` lookup, and fell through to `materialize_owned_temp` —
/// which claims a Vec/String by LLVM shape and a Map/Set handle or RC box by
/// name, and has no Option arm at all, so nothing was queued.
///
/// Keyed through `array_elem_and_len` rather than a `TypeKind::Array` match:
/// a return type is ANNOTATED, so the payload arrives as
/// `Path(["Array"], ..)` and a kind-keyed test misses every one of them
/// while still compiling. That is the same trap this family has now hit at
/// six separate registration sites.
///
/// CELLS. `discard` is the defect. `bound` (`let o = mk(j)`) and `str` (an
/// `Option[String]` discard) were clean before and must stay clean — they
/// are what would catch a fix that widened the discard chokepoint instead of
/// adding the arm beside its tuple peer. The `<= 3` word gate partitions
/// against the INLINE tracker, which now admits a one-element array
/// (B-2026-09-13-18), so `inline1` pins that the two do not both claim it —
/// a double-free if they did.
#[test]
fn asan_discarded_boxed_array_option_temp_frees_its_box_and_interior() {
    assert_clean_asan_run(
        r#"
fn mk(n: i64) -> Option[Array[String, 2]] {
    if n < 0 { return None; }
    return Some([f"row-aaaaaaaaaaaaaaaa-{n}", f"col-bbbbbbbbbbbbbbbb-{n}"]);
}
fn mk1(n: i64) -> Option[Array[String, 1]] {
    if n < 0 { return None; }
    return Some(Array[f"one-aaaaaaaaaaaaaaaa-{n}"]);
}
fn mkstr(n: i64) -> Option[String] {
    if n < 0 { return None; }
    return Some(f"str-aaaaaaaaaaaaaaaa-{n}");
}

fn main() {
    let mut j: i64 = 0;
    while j < 4 {
        mk(j);
        mk1(j);
        mkstr(j);
        let o = mk(j);
        match o { Some(a) => { println(f"bound:{a[0]}"); } None => { println("none"); } }
        j = j + 1;
    }
    println("end");
}
"#,
        &[
            "bound:row-aaaaaaaaaaaaaaaa-0",
            "bound:row-aaaaaaaaaaaaaaaa-1",
            "bound:row-aaaaaaaaaaaaaaaa-2",
            "bound:row-aaaaaaaaaaaaaaaa-3",
            "end",
        ],
        "asan_discarded_boxed_array_option_temp_frees_its_box_and_interior",
    );
}

/// B-2026-09-13-15 — a boxed `Array[T, N]` enum payload read through an
/// OWNED CALLEE leaked its elements, and the fix INVERTS the rule the
/// previous one established.
///
/// B-2026-09-12-18 made the box's array interior walk conditional on
/// whether THAT construction site built the payload in place
/// (`user_variant_ctor_builds_payload_inline`), because `G.Y(a)` left the
/// local `a`'s own element cleanup armed and walking would free it twice.
/// Correct locally, and it leaves the interior owned by NOBODY once the
/// enum is handed to an owned callee: that callee's param registration is
/// emitted ONCE per monomorph, not per call site, so it cannot ask which
/// spelling produced any particular payload. 288 B in 16 blocks.
///
/// So the source is stood down at the MOVE instead
/// (`suppress_array_local_move_into_ctor`), and the walk is then
/// unconditional at both registration sites. The box owns its array
/// interior, full stop; the per-construction-site question is deleted.
///
/// HOOKED AT THE CONSTRUCTOR LOWERING, not at the `let` that binds one.
/// A let-site hook misses a constructor in ARGUMENT position and the walk
/// is armed anyway — measured `free(): double free detected in tcache 2` on
/// `take(Filled(a))`, a cell that was clean before. `try_compile_enum_variant_at`
/// is the one site every spelling passes through.
///
/// USER ENUMS ONLY, which is the other half of the pairing and was also
/// measured the hard way. A SEEDED `Option`/`Result` payload is owned by a
/// different channel that this change does not arm, so disarming there
/// retracts without arming — the LEAK mirror of the double free above,
/// caught as `takesOpt(Some(p))` over an array local going from clean to a
/// LeakSanitizer report. `shared` enums are out for the same reason: their
/// payload is RC-managed, not box-owned.
///
/// FIVE CELLS, one per spelling, because each of the three failures above
/// showed up in a different one:
///   - `callee_inplace` is the row's defect (in-place ctor, owned callee);
///   - `callee_named` is the local-source spelling, clean before and after,
///     and the cell that turns into a double free if the walk is armed
///     without the disarm;
///   - `arg_named` is `take(Filled(a))`, the one a let-site hook misses;
///   - `arg_inline` is its literal twin, which has no source to stand down;
///   - `no_callee` matches in place with no callee hop at all, which is
///     where the defect does NOT appear and so pins that the owned-callee
///     hop is the axis.
#[test]
fn asan_boxed_array_enum_payload_interior_has_one_owner_through_an_owned_callee() {
    assert_clean_asan_run(
        r#"
enum Slot[T] { Filled(T), Blank }

fn take(s: Slot[Array[String, 2]]) -> bool {
    match s { Filled(x) => x[0].contains("row"), Blank => false, }
}

fn main() {
    let mut i: i64 = 0;
    while i < 4 {
        let g: Slot[Array[String, 2]] =
            Filled([f"row-bbbbbbbbbbbb-{i}", f"row-cccccccccccc-{i}"]);
        println(f"callee_inplace:{take(g)}");

        let a: Array[String, 2] = [f"row-dddddddddddd-{i}", f"row-eeeeeeeeeeee-{i}"];
        let h: Slot[Array[String, 2]] = Filled(a);
        println(f"callee_named:{take(h)}");

        let b: Array[String, 2] = [f"row-ffffffffffff-{i}", f"row-gggggggggggg-{i}"];
        println(f"arg_named:{take(Filled(b))}");

        println(f"arg_inline:{take(Filled([f"row-hhhhhhhhhhhh-{i}", f"row-iiiiiiiiiiii-{i}"]))}");

        let c: Array[String, 2] = [f"row-jjjjjjjjjjjj-{i}", f"row-kkkkkkkkkkkk-{i}"];
        let k: Slot[Array[String, 2]] = Filled(c);
        match k { Filled(x) => { println(f"no_callee:{x[0]}"); } Blank => {} }

        i = i + 1;
    }
    println("end");
}
"#,
        &[
            "callee_inplace:true",
            "callee_named:true",
            "arg_named:true",
            "arg_inline:true",
            "no_callee:row-jjjjjjjjjjjj-0",
            "callee_inplace:true",
            "callee_named:true",
            "arg_named:true",
            "arg_inline:true",
            "no_callee:row-jjjjjjjjjjjj-1",
            "callee_inplace:true",
            "callee_named:true",
            "arg_named:true",
            "arg_inline:true",
            "no_callee:row-jjjjjjjjjjjj-2",
            "callee_inplace:true",
            "callee_named:true",
            "arg_named:true",
            "arg_inline:true",
            "no_callee:row-jjjjjjjjjjjj-3",
            "end",
        ],
        "asan_boxed_array_enum_payload_interior_has_one_owner_through_an_owned_callee",
    );
}

#[test]
fn asan_an_array_option_payload_a_call_hands_back_has_an_owner() {
    // B-2026-09-13-2. Filed as "the `Option[Array[T, N]]` a `Map` hands
    // back is owned by nobody", and the row's own note says to MEASURE
    // rather than trust its framing. Measuring moved the axis twice.
    //
    // FIRST: the `Map` is not the axis. A PLAIN function returning
    // `Option[Array[String, 2]]`, with no `Map` anywhere in the program,
    // leaks byte-identically to `Map.remove` -- 176 B in 8 blocks
    // arm-bound, 192 B in 4 plus 176 indirect discarded.
    //
    // SECOND: boxing is not the axis either. At `KARAC_OPT_LEVEL=0`, four
    // calls, arm-bound and read:
    //
    //     Option[S4 { 4 Strings }]       96 B, boxed          clean
    //     Option[(String x4)]            96 B, boxed          clean
    //     Option[S2 { 2 Strings }]       48 B, boxed          clean
    //     Option[(String, String)]       48 B, boxed          clean
    //     Option[Array[String, 2]]       48 B, boxed      176 B / 8
    //     Option[Array[String, 1]]       24 B, INLINE      88 B / 4
    //
    // Same-width struct and tuple payloads are clean at both widths, so
    // the axis is `Array` AS THE PAYLOAD TYPE. What was missing was
    // ROUTING, one route at a time, which is why the by-value PARAM and
    // the `let o = Some([..])` LITERAL spellings were already clean
    // (B-2026-09-06-49 / B-2026-09-10-6 and this row's own let site) while
    // every CALL-sourced spelling leaked.
    //
    // Four registrations, each with its own sole-ownership argument:
    //
    //   * `let o = mk(j)` -- the let site refused a CALL RHS wholesale
    //     against a real hazard (`let back = passthru(Some(e))` receives a
    //     box whose interior an upstream local still owns). Widened by
    //     exactly one spelling, `call_builds_its_own_optres_box`: a callee
    //     NONE of whose arguments flows into its return cannot be handing
    //     back a box the caller supplied.
    //   * `let o = m.remove(k)` -- `map_handback_moves_value_out`. No
    //     callee AST to ask, so the argument is that `remove` tombstones
    //     the bucket and `insert` replaces it. `get` is excluded because
    //     its payload ALIASES live storage.
    //   * the DISCARD forms -- `map_val_array_reclaim_on_discard_for`
    //     arms the reclaim that already handled an array, plus
    //     `free_discarded_wide_payload_box` for the 48-byte box the pack
    //     allocates for an envelope nobody reads.
    //   * `match m.remove(k) { Some(a) => .. }` -- the `Array` peer of the
    //     whole-TUPLE arm-binding branch in `control_flow_match`.
    //
    // 1 -- THE ARM-BOUND SHAPE IS STILL OPEN and is deliberately not
    //      asserted. `match m.remove(k) { Some(a) => .. }` leaks 176 B in
    //      8 blocks. The registration that closes it -- the `Array` peer of
    //      the whole-TUPLE arm-binding branch -- has now been attempted
    //      TWICE, and each attempt found a different hazard:
    //
    //      1. The retraction could not reach a FRESH-TEMP scrutinee at all,
    //         so an arm that moved the binding onward double-freed. That is
    //         B-2026-09-13-21, and it is FIXED -- its own battery is
    //         `asan_a_freshtemp_boxed_payload_binding_the_arm_moves_on_has_one_owner`
    //         below. With it in place the three move spellings
    //         (`let b = a`, `m.insert(2, a)`, `Bx { a: a }`) are all clean
    //         for an array too.
    //      2. What still is not: a by-value CALL. `Some(a) => { eat(a) }`
    //         over `Map[i64, Array[String, 2]]` aborts with a tcache double
    //         free, while the TUPLE twin of the same spelling is clean. A
    //         by-value array param is CALLEE-OWNED (B-2026-09-06-49 /
    //         B-2026-09-10-6) so the call transfers, whereas a
    //         copy-supported tuple param is entry-copied and the caller
    //         retains. `binding_only_borrowed`, which the retraction uses,
    //         encodes the entry-copy convention and so calls that array
    //         call-arg non-consuming -- no retraction, and the callee's
    //         free plus the box's interior drop collide.
    //
    //      An array arm therefore wants `consume_class::binding_materialized`
    //      (`free_fn_arg_transfers: true`) chosen per binding off the
    //      recorded type tag, not the tag simply added to the tuple
    //      predicate. Left open rather than bought at the price of a double
    //      free for the second time.
    // 2 -- the row's `remove` leg DISCARDED, both spellings. The bare form
    //      is the one that shows the BOX: the general statement-result
    //      cleanup reaches an inline payload and not a heap-boxed one.
    assert_clean_asan_run(
            "fn main() {\n\
             \x20\x20\x20\x20let mut m: Map[i64, Array[String, 2]] = Map.new();\n\
             \x20\x20\x20\x20let mut i: i64 = 0;\n\
             \x20\x20\x20\x20while i < 4 {\n\
             \x20\x20\x20\x20\x20\x20\x20\x20m.insert(i, Array[f\"raaaaaaaaaaaa{i}\", f\"cbbbbbbbbbbbb{i}\"]);\n\
             \x20\x20\x20\x20\x20\x20\x20\x20i = i + 1;\n\
             \x20\x20\x20\x20}\n\
             \x20\x20\x20\x20m.remove(0);\n\
             \x20\x20\x20\x20let _ = m.remove(1);\n\
             \x20\x20\x20\x20println(f\"n:{m.len()}\");\n\
             }\n",
            &["n:2"],
            "map-remove-array-value-discarded",
        );
    // 3 -- the row's `insert`-overwrite leg: the DISPLACED old value,
    //      discarded. 336 B in 7 blocks plus 308 indirect in 14 before.
    assert_clean_asan_run(
            "fn main() {\n\
             \x20\x20\x20\x20let mut m: Map[i64, Array[String, 2]] = Map.new();\n\
             \x20\x20\x20\x20let mut i: i64 = 0;\n\
             \x20\x20\x20\x20while i < 4 {\n\
             \x20\x20\x20\x20\x20\x20\x20\x20m.insert(3, Array[f\"raaaaaaaaaaaa{i}\", f\"cbbbbbbbbbbbb{i}\"]);\n\
             \x20\x20\x20\x20\x20\x20\x20\x20i = i + 1;\n\
             \x20\x20\x20\x20}\n\
             \x20\x20\x20\x20println(f\"n:{m.len()}\");\n\
             }\n",
            &["n:1"],
            "map-insert-overwrite-array-value-discarded",
        );
    // 4 -- the same leg BOUND, so the displaced value is read rather than
    //      thrown away. A different owner from cell 3 and it must not be
    //      the same one twice.
    assert_clean_asan_run(
            "fn main() {\n\
             \x20\x20\x20\x20let mut m: Map[i64, Array[String, 2]] = Map.new();\n\
             \x20\x20\x20\x20let mut i: i64 = 0;\n\
             \x20\x20\x20\x20while i < 3 {\n\
             \x20\x20\x20\x20\x20\x20\x20\x20let o = m.insert(3, Array[f\"raaaaaaaaaaaa{i}\", f\"cbbbbbbbbbbbb{i}\"]);\n\
             \x20\x20\x20\x20\x20\x20\x20\x20match o {\n\
             \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20Some(a) => { println(f\"o:{a[0]}\"); }\n\
             \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20None => { println(\"o:fresh\"); }\n\
             \x20\x20\x20\x20\x20\x20\x20\x20}\n\
             \x20\x20\x20\x20\x20\x20\x20\x20i = i + 1;\n\
             \x20\x20\x20\x20}\n\
             \x20\x20\x20\x20println(f\"n:{m.len()}\");\n\
             }\n",
            &["o:fresh", "o:raaaaaaaaaaaa0", "o:raaaaaaaaaaaa1", "n:1"],
            "map-insert-overwrite-array-value-bound",
        );
    // 5 -- NO MAP IN THE PROGRAM. This is the cell that says the row's own
    //      framing was wrong, and it is the general fix rather than the
    //      map-shaped one: a plain call return, bound annotated, bound
    //      inferred, and never matched at all.
    assert_clean_asan_run(
            "fn mk(n: i64) -> Option[Array[String, 2]] {\n\
             \x20\x20\x20\x20if n < 0 { return None; }\n\
             \x20\x20\x20\x20return Some(Array[f\"raaaaaaaaaaaa{n}\", f\"cbbbbbbbbbbbb{n}\"]);\n\
             }\n\
             fn main() {\n\
             \x20\x20\x20\x20let a: Option[Array[String, 2]] = mk(1);\n\
             \x20\x20\x20\x20match a { Some(v) => { println(f\"a:{v[0]}\"); } None => { println(\"a:none\"); } }\n\
             \x20\x20\x20\x20let b = mk(2);\n\
             \x20\x20\x20\x20match b { Some(v) => { println(f\"b:{v[0]}\"); } None => { println(\"b:none\"); } }\n\
             \x20\x20\x20\x20let c: Option[Array[String, 2]] = mk(3);\n\
             \x20\x20\x20\x20println(\"c:unread\");\n\
             }\n",
            &["a:raaaaaaaaaaaa1", "b:raaaaaaaaaaaa2", "c:unread"],
            "call-returned-array-option-bound-and-unread",
        );
    // 6 -- the `Result` twin of cell 5, both variants live.
    assert_clean_asan_run(
            "fn mk(n: i64) -> Result[Array[String, 2], i64] {\n\
             \x20\x20\x20\x20if n < 0 { return Err(7); }\n\
             \x20\x20\x20\x20return Ok(Array[f\"raaaaaaaaaaaa{n}\", f\"cbbbbbbbbbbbb{n}\"]);\n\
             }\n\
             fn main() {\n\
             \x20\x20\x20\x20let a = mk(1);\n\
             \x20\x20\x20\x20match a { Ok(v) => { println(f\"a:{v[0]}\"); } Err(e) => { println(\"a:err\"); } }\n\
             \x20\x20\x20\x20let b = mk(0 - 1);\n\
             \x20\x20\x20\x20match b { Ok(v) => { println(f\"b:{v[0]}\"); } Err(e) => { println(\"b:err\"); } }\n\
             }\n",
            &["a:raaaaaaaaaaaa1", "b:err"],
            "call-returned-array-result-both-variants",
        );
    // 7 -- THE ALIASING CONTROL, and the cell that fails if
    //      `map_handback_moves_value_out` ever admits `get`. `get`'s
    //      payload interior aliases the bucket's stored value; only the box
    //      is fresh. Two reads of the same key, so a disarmed bucket shows
    //      up as a use-after-free on the second rather than as a leak.
    assert_clean_asan_run(
            "fn main() {\n\
             \x20\x20\x20\x20let mut m: Map[i64, Array[String, 2]] = Map.new();\n\
             \x20\x20\x20\x20m.insert(1, Array[f\"raaaaaaaaaaaa1\", f\"cbbbbbbbbbbbb1\"]);\n\
             \x20\x20\x20\x20match m.get(1) { Some(a) => { println(f\"p:{a[0]}\"); } None => { println(\"p:none\"); } }\n\
             \x20\x20\x20\x20match m.get(1) { Some(a) => { println(f\"q:{a[0]}\"); } None => { println(\"q:none\"); } }\n\
             \x20\x20\x20\x20println(f\"n:{m.len()}\");\n\
             }\n",
            &["p:raaaaaaaaaaaa1", "q:raaaaaaaaaaaa1", "n:1"],
            "map-get-array-value-aliases-storage-control",
        );
    // 8 -- THE PASSTHROUGH HAZARD is NOT asserted here, deliberately.
    //      `let back = passthru(Some(e))` is the shape the let site's gate
    //      was built against, and this row keeps it DECLINED:
    //      `passthru` returns its argument, so
    //      `call_arg_flows_into_return` is true, no registration happens,
    //      and the upstream local stays the interior's only owner. It
    //      cannot be a cell in this battery because that spelling still
    //      leaks 132 B in 6 blocks -- unchanged by this row, measured
    //      before and after -- so an `assert_clean_asan_run` would fail on
    //      a pre-existing leak rather than guard this fix.
    //
    //      Its guard is the cell written for exactly this hazard:
    //      `asan_generic_callee_boxed_optres_temp_arg_frees_its_box`
    //      cell 5, which aborts with `attempting double-free` if the
    //      registration is ever widened to reach it.
    // 9 -- the CONSUMING arm: the bound array is handed to a by-value
    //      callee, so the arm's binding takes over and the box's
    //      registration must not free it a second time. This is the cell
    //      that turns a wrong registration from a silent leak into an
    //      abort.
    assert_clean_asan_run(
            "fn mk(n: i64) -> Option[Array[String, 2]] {\n\
             \x20\x20\x20\x20if n < 0 { return None; }\n\
             \x20\x20\x20\x20return Some(Array[f\"raaaaaaaaaaaa{n}\", f\"cbbbbbbbbbbbb{n}\"]);\n\
             }\n\
             fn eat(a: Array[String, 2]) -> i64 { return a[0].len(); }\n\
             fn main() {\n\
             \x20\x20\x20\x20let mut i: i64 = 0;\n\
             \x20\x20\x20\x20while i < 3 {\n\
             \x20\x20\x20\x20\x20\x20\x20\x20let o = mk(i);\n\
             \x20\x20\x20\x20\x20\x20\x20\x20match o {\n\
             \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20Some(a) => { println(f\"n:{eat(a)}\"); }\n\
             \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20None => { println(\"n:none\"); }\n\
             \x20\x20\x20\x20\x20\x20\x20\x20}\n\
             \x20\x20\x20\x20\x20\x20\x20\x20i = i + 1;\n\
             \x20\x20\x20\x20}\n\
             }\n",
            &["n:14", "n:14", "n:14"],
            "call-returned-array-option-consuming-arm",
        );
    // 10 -- the SCALAR-element control. `emit_drop_fn_for_array` declines a
    //       heapless element, so every registration above must emit nothing
    //       and an `Array[i64, N]` value keeps its exact no-op.
    assert_clean_asan_run(
            "fn mk(n: i64) -> Option[Array[i64, 2]] {\n\
             \x20\x20\x20\x20if n < 0 { return None; }\n\
             \x20\x20\x20\x20return Some(Array[n, n + 1]);\n\
             }\n\
             fn main() {\n\
             \x20\x20\x20\x20let mut m: Map[i64, Array[i64, 2]] = Map.new();\n\
             \x20\x20\x20\x20m.insert(1, Array[11, 22]);\n\
             \x20\x20\x20\x20m.remove(1);\n\
             \x20\x20\x20\x20let o = mk(5);\n\
             \x20\x20\x20\x20match o { Some(a) => { println(f\"s:{a[0]}\"); } None => { println(\"s:none\"); } }\n\
             }\n",
            &["s:5"],
            "array-option-scalar-element-control",
        );
}

/// B-2026-09-15-20 — a whole-container reassignment over a FIXED
/// `Array[T, N]` stranded the displaced elements' heap.
///
/// The MEMORY twin of B-2026-09-14-23, which gave that same displacement
/// its `Drop` BODIES. The two are separate channels (B-2026-08-28-57) and
/// the bodies walker frees nothing on purpose — "the array's memory stays
/// owned by the scope-exit `__karac_drop_array_te_*`", which is right for a
/// slot that survives to scope exit and wrong for one being OVERWRITTEN:
/// the displaced generation gets no later visit.
///
/// The `Vec` spelling is the control that localizes it to the fixed-array
/// position — it reclaims its displaced buffer via the eager-free path and
/// measured 0 lost both before and after.
///
/// MUST be read at `-O0`: at `-O2` LLVM deletes the allocation for these
/// short element strings and the cell asserts nothing, which is the
/// `asan-o0-leg.sh` case exactly.
#[test]
fn asan_fixed_array_reassign_frees_the_displaced_elements() {
    const H: &str = "struct D { id: i64, s: String }\n\
             impl Drop for D { fn drop(mut ref self) { println(f\"dD{self.id}\") } }\n\
             fn mkd(i: i64) -> D { return D { id: i, s: f\"tttttttttttttttt{i}\" } }\n";
    // Flat `Array[D, 2]`: 34 B in 2 blocks before the fix.
    assert_clean_asan_run(
        &format!(
            "{H}fn main() {{\n\
                 \x20   let mut v: Array[D, 2] = [mkd(1), mkd(2)];\n\
                 \x20   v = [mkd(3), mkd(4)];\n\
                 \x20   println(\"mid\");\n\
                 \x20   println(\"end\");\n\
                 }}\n"
        ),
        &["dD1", "dD2", "dD3", "dD4", "mid", "end"],
        "b20-array-reassign-flat",
    );
    // Nested `Array[Array[D, 1], 2]`: the same 34 B, through the recursive
    // element drop rather than the leaf one.
    assert_clean_asan_run(
        &format!(
            "{H}fn main() {{\n\
                 \x20   let mut v: Array[Array[D, 1], 2] = [[mkd(1)], [mkd(2)]];\n\
                 \x20   v = [[mkd(3)], [mkd(4)]];\n\
                 \x20   println(\"mid\");\n\
                 \x20   println(\"end\");\n\
                 }}\n"
        ),
        &["dD1", "dD2", "dD3", "dD4", "mid", "end"],
        "b20-array-reassign-nested",
    );
    // THE UNBOUNDED CASE, and the reason the severity is what it is: three
    // trips stranded 102 B in 6 blocks, growing with the loop. Also the
    // cell that would catch a fix which frees the NEW generation instead of
    // the displaced one — that reads as a double free here rather than as a
    // leak.
    assert_clean_asan_run(
        &format!(
            "{H}fn main() {{\n\
                 \x20   let mut v: Array[D, 2] = [mkd(0), mkd(0)];\n\
                 \x20   let mut i = 1;\n\
                 \x20   while i < 4 {{\n\
                 \x20       v = [mkd(i), mkd(i)];\n\
                 \x20       i = i + 1;\n\
                 \x20   }}\n\
                 \x20   println(\"end\");\n\
                 }}\n"
        ),
        &[
            "dD0", "dD0", "dD1", "dD1", "dD2", "dD2", "dD3", "dD3", "end",
        ],
        "b20-array-reassign-loop",
    );
    // CONTROL — the `Vec` spelling, clean before and after. A widening that
    // started double-freeing the Vec path shows up here.
    assert_clean_asan_run(
        &format!(
            "{H}fn main() {{\n\
                 \x20   let mut v: Vec[D] = [mkd(1), mkd(2)];\n\
                 \x20   v = [mkd(3), mkd(4)];\n\
                 \x20   println(\"mid\");\n\
                 \x20   println(\"end\");\n\
                 }}\n"
        ),
        &["dD1", "dD2", "dD3", "dD4", "mid", "end"],
        "b20-vec-reassign-control",
    );
    // An element whose heap is a `Vec` rather than a `String` — the same
    // position, reached through the element's own recursive drop, and the
    // largest of these cells: 164 B in 6 blocks before the fix (96 direct
    // + 68 INDIRECT), against 34 B in 2 for the String-bearing element.
    // The indirect half is what makes it worth its own cell: it is the one
    // that says the displaced element's drop is reached in full rather
    // than just at its first level.
    assert_clean_asan_run(
        "struct D { id: i64, v: Vec[String] }\n\
             impl Drop for D { fn drop(mut ref self) { println(f\"dD{self.id}\") } }\n\
             fn mkd(i: i64) -> D {\n\
             \x20   return D { id: i, v: [f\"tttttttttttttttt{i}\", f\"uuuuuuuuuuuuuuuu{i}\"] }\n\
             }\n\
             fn main() {\n\
             \x20   let mut v: Array[D, 2] = [mkd(1), mkd(2)];\n\
             \x20   v = [mkd(3), mkd(4)];\n\
             \x20   println(\"mid\");\n\
             \x20   println(\"end\");\n\
             }\n",
        &["dD1", "dD2", "dD3", "dD4", "mid", "end"],
        "b20-array-reassign-vec-heap",
    );
}

/// B-2026-09-13-23 — an `Array[T, N]` held in a TUPLE frees its element
/// buffers, in every position the tuple can occupy.
///
/// FIVE SITES. Four are the walk and its disarms; the fifth is a LAYOUT
/// guard, and leaving it out is what made the first attempt at this row
/// (`506a91d`) ship an invalid free and get reverted (`49e75a8`,
/// B-2026-09-16-9). See `asan_array_in_a_tuple_enum_payload_is_not_corrupted`
/// below, which is the cell that catches it.
///
/// An array spells as `Path(["Array"], [Type(T), Const(N)])`, so it reaches
/// the `Path` arm of each tuple helper and then falls through every test in
/// it:
///
///   1 `tuple_elem_needs_deep_drop` never ARMED the tuple's deep drop.
///   2 `emit_tuple_elem_drops` dispatched `"Array"` into a catch-all that
///     tests only shared/enum/struct and so emitted nothing.
///   3 the MOVE-SUPPRESSION dual, which is where two earlier attempts
///     stalled. Pieces 1+2 alone turn the leak into a DOUBLE FREE on
///     `let t2 = t;`, which is strictly worse, and both attempts reverted.
///     It is not one site but two, reached by different shapes: a bare
///     tuple move goes through `zero_aggregate_field_caps` (gated by
///     `aggregate_has_heap_field`, which is LLVM-type-driven and reads a
///     `[2 x {ptr,len,cap}]` field as no-heap), while a tuple held in a
///     STRUCT field goes through `zero_tuple_elem_cap_at`. Both were
///     measured double-freeing independently; fixing either alone leaves
///     the other. That is why the earlier attempt's `zero_tuple_elem_cap_at`
///     arm "changed nothing" — it is the right arm for the struct shape and
///     was tested against the bare-tuple cell, which never reaches it.
///   4 the by-value PARAM entry copy. `make_tuple_param_callee_owned` gates
///     on `type_expr_has_drop_heap`, array-blind too, so the param aliased
///     the caller's buffers — harmless while nothing walked the array, a
///     double free once something did and the value escaped by `return`.
///   5 the ENUM-PAYLOAD layout guard in `declare_enums`. Site 1 widens
///     `tuple_elem_needs_deep_drop`, which the enum drop CLASSIFIER also
///     consults, so a tuple payload holding an array newly classified
///     `EnumDropKind::NestedTuple` — a kind that hands the payload's word
///     region straight to the tuple's drop fn. The region is two words wide
///     and the tuple is seven, because a source-written array sizes as the
///     conservative 1 word, so the walker strode through a boxed payload
///     and freed a `String`'s length word as a pointer. Not visible from
///     any position a tuple occupies in a function BODY, which is where all
///     28 of the first attempt's cells came from.
///
/// THE `let t2 = t;` CELL IS FIRST ON PURPOSE. With pieces 1+2 in and the
/// suppression missing, both ASAN ratchet legs matched their quarantine
/// lists exactly and the whole suite passed — nothing in it exercised a
/// whole-tuple move of a tuple holding an array. The green suite was not
/// evidence for either earlier attempt and is not evidence now; this
/// fixture is what makes it one.
///
/// MUST be read at `-O0`: at `-O2` LLVM deletes the allocations, which is
/// the `scripts/asan-o0-leg.sh` case.
///
/// TWO SHAPES REMAINED LEAKING when this row landed and are now CLOSED by
/// B-2026-09-16-5 — `asan_tuple_array_param_move_and_temp_arg_have_owners`
/// below asserts both clean. This paragraph is kept rather than deleted
/// because the shapes it names are the ones that row measured, and the
/// prediction it recorded turned out to be wrong in an instructive way.
///
/// It read them as ONE fault ("the param-ownership model resolved") and
/// they are TWO, additive and independently reachable: a by-value tuple
/// param moved to a local loses its drop in BOTH the `Array` and the `Vec`
/// spelling (this paragraph's "the entry copy orphans a temporary's
/// buffers" cannot explain the named-local cell at all), while a FRESH
/// TEMPORARY tuple argument is orphaned in EVERY callee body — including
/// one that only reads its param, which no cell here or in that row ever
/// measured. The cell holding both leaked exactly twice 20 B, which is
/// what separated them.
///
/// "A caller-side disarm that has no hook for a tuple argument" was also
/// wrong: the hook exists (B-2026-08-27-44 built it,
/// `arg_is_entry_copied_heap_tuple`) and simply could not see an `Array`,
/// the same blind spot as sites 1-4 above, one caller further out.
#[test]
fn asan_array_inside_a_tuple_frees_its_element_buffers() {
    const H: &str = "fn pay(i: i64) -> String { return f\"tttttttttttttttt{i}\" }\n";
    // THE BLOCKING CELL — a whole-tuple move. Double-frees with pieces 1+2
    // and no suppression dual; nothing else in the suite covers it.
    assert_clean_asan_run(
        &format!(
            "{H}fn main() {{\n\
                 \x20   let t: (Array[String, 2], i64) = ([pay(1), pay(2)], 7);\n\
                 \x20   let t2 = t;\n\
                 \x20   println(f\"a0:{{t2.0[0]}}\");\n\
                 }}\n"
        ),
        &["a0:tttttttttttttttt1"],
        "b23-tuple-array-whole-move",
    );
    // The row's own repro: a plain annotated `let`, 20 B in 2 blocks.
    assert_clean_asan_run(
        &format!(
            "{H}fn main() {{\n\
                 \x20   let t: (Array[String, 2], i64) = ([pay(1), pay(2)], 7);\n\
                 \x20   println(f\"a0:{{t.0[0]}}\");\n\
                 \x20   println(f\"n:{{t.1}}\");\n\
                 }}\n"
        ),
        &["a0:tttttttttttttttt1", "n:7"],
        "b23-tuple-array-let",
    );
    // A STRUCT FIELD holding the tuple, then a move of the STRUCT — the
    // second suppression site, which double-freed while only the
    // bare-tuple one was fixed.
    assert_clean_asan_run(
        &format!(
            "{H}struct W {{ t: (Array[String, 2], i64) }}\n\
                 fn main() {{\n\
                 \x20   let w: W = W {{ t: ([pay(1), pay(2)], 7) }};\n\
                 \x20   let w2 = w;\n\
                 \x20   println(f\"a0:{{w2.t.0[0]}}\");\n\
                 }}\n"
        ),
        &["a0:tttttttttttttttt1"],
        "b23-tuple-array-struct-field-move",
    );
    // A by-value PARAM returned — the fourth site. Double-freed until the
    // param gained its entry copy.
    assert_clean_asan_run(
        &format!(
            "{H}fn thru(p: (Array[String, 2], i64)) -> (Array[String, 2], i64) {{ return p; }}\n\
                 fn main() {{\n\
                 \x20   let t: (Array[String, 2], i64) = ([pay(1), pay(2)], 7);\n\
                 \x20   let u = thru(t);\n\
                 \x20   println(f\"a0:{{u.0[0]}}\");\n\
                 }}\n"
        ),
        &["a0:tttttttttttttttt1"],
        "b23-tuple-array-param-returned",
    );
    // A RETURN of a freshly built tuple, and a DESTRUCTURE.
    assert_clean_asan_run(
        &format!(
            "{H}fn mk() -> (Array[String, 2], i64) {{ return ([pay(1), pay(2)], 7); }}\n\
                 fn main() {{\n\
                 \x20   let t = mk();\n\
                 \x20   let (a, j) = t;\n\
                 \x20   println(f\"a0:{{a[0]}} j:{{j}}\");\n\
                 }}\n"
        ),
        &["a0:tttttttttttttttt1 j:7"],
        "b23-tuple-array-return-destructure",
    );
    // An array of CONTAINERS, moved.
    assert_clean_asan_run(
        &format!(
            "{H}fn main() {{\n\
                 \x20   let t: (Array[Vec[String], 2], i64) = ([[pay(1)], [pay(2)]], 7);\n\
                 \x20   let t2 = t;\n\
                 \x20   println(f\"n:{{t2.1}}\");\n\
                 }}\n"
        ),
        &["n:7"],
        "b23-tuple-array-of-vecs-move",
    );
    // A user `Drop` element: memory clean AND the bodies still exactly
    // once. The row measured this shape as memory-only — bodies were
    // already correct — so it guards against the memory fix buying itself
    // a duplicated body (B-2026-08-28-57: separate channels).
    assert_clean_asan_run(
        "struct D { id: i64, s: String }\n\
             impl Drop for D { fn drop(mut ref self) { println(f\"dD{self.id}\") } }\n\
             fn mkd(i: i64) -> D { return D { id: i, s: f\"ssssssssssssssss{i}\" } }\n\
             fn main() {\n\
             \x20   let t: (Array[D, 2], i64) = ([mkd(1), mkd(2)], 7);\n\
             \x20   let t2 = t;\n\
             \x20   println(f\"n:{t2.1}\");\n\
             }\n",
        &["n:7", "dD1", "dD2"],
        "b23-tuple-array-user-drop-move",
    );
    // CONTROLS — a scalar array stays a no-op, the plain `Array` local
    // (always clean) must not acquire a second owner, and the `Vec`
    // element spelling is the positive control that identified the
    // working disarm path.
    assert_clean_asan_run(
        &format!(
            "{H}fn main() {{\n\
                 \x20   let t: (Array[i64, 2], i64) = ([3, 4], 7);\n\
                 \x20   let t2 = t;\n\
                 \x20   println(f\"a0:{{t2.0[0]}}\");\n\
                 }}\n"
        ),
        &["a0:3"],
        "b23-tuple-scalar-array-control",
    );
    assert_clean_asan_run(
        &format!(
            "{H}fn main() {{\n\
                 \x20   let a: Array[String, 2] = [pay(1), pay(2)];\n\
                 \x20   println(f\"a0:{{a[0]}}\");\n\
                 }}\n"
        ),
        &["a0:tttttttttttttttt1"],
        "b23-plain-array-local-control",
    );
    assert_clean_asan_run(
        &format!(
            "{H}fn main() {{\n\
                 \x20   let t: (Vec[String], i64) = ([pay(1), pay(2)], 7);\n\
                 \x20   let t2 = t;\n\
                 \x20   println(f\"a0:{{t2.0[0]}}\");\n\
                 }}\n"
        ),
        &["a0:tttttttttttttttt1"],
        "b23-tuple-vec-move-control",
    );
}

/// B-2026-09-16-5 — the two shapes B-2026-09-13-23 left leaking. They are
/// TWO faults, not the one "model conflict" the row was filed as, and the
/// grid that separates them is the whole result.
///
///                              named arg          temporary arg
///     callee reads p              clean              34 B          <- never measured before
///     callee does `let q = p`     34 B               68 B          <- 68 is what proves they add
///     callee returns p            clean              34 B          <- the row's cell (b)
///
/// The `Vec` spelling of the same grid leaks ONLY in the `let q = p` row,
/// which refutes the row's "the `Vec` spelling is clean in every position"
/// — the sentence its COPY-versus-TRANSFER argument rests on.
///
/// FAULT 1, the move. `let q = p` over a by-value tuple param disarms the
/// SOURCE (`zero_aggregate_field_caps`, which needs no element types) and
/// then arms nothing, because `tuple_binding_elem_tes`' bare-rebind arm
/// read the raw `var_types.tuple_var_elem_tes` map, where a PARAM is never
/// recorded — a param goes into `tuple_var_elem_type_exprs`, and the
/// ACCESSOR `Self::tuple_var_elem_tes()` is the reader that consults both.
/// The field and the method are spelled identically, so the raw read looks
/// exactly like the accessor. Retraction without arming, the same shape as
/// B-2026-09-17-21 one channel over.
///
/// The two spellings failed DIFFERENTLY, which is why one grep could not
/// have found it and why both are asserted here. `Array` got no drop at
/// all (`aggregate_has_heap_field` matches `StructType`, and `[2 x
/// {ptr,len,cap}]` is an `ArrayType`); `Vec` fell through to
/// `track_tuple_var`'s LLVM-type walker and got a drop that frees the
/// Vec's BUFFER and not its elements — the erasure
/// `emit_aggregate_heap_field_frees`' own doc warns about. Both leak the
/// two `String`s; only the second leaves a drop call in the IR, so an
/// IR-shaped check would have called the `Vec` cell covered.
///
/// FAULT 2, the temporary. `arg_is_entry_copied_heap_tuple` answers "will
/// the callee entry-copy this tuple argument?" and is read by
/// `escapes_without_entry_copy` one caller out. Array-blind, it said NO, so
/// the argument registrar was never invoked AT ALL on the escape path and
/// the caller's originals were orphaned while the callee returned its copy
/// — `main` held no tuple drop call whatsoever in the emitted IR. It now
/// MIRRORS `make_tuple_param_callee_owned`'s admission gate, array arm
/// included, because the two have to agree in both directions: YES where
/// the copy declines retracts the caller's owner against a callee that
/// registered none, and NO where it copies is this leak.
///
/// MUST be read at `-O0` — `scripts/asan-o0-leg.sh` — for the reason the
/// parent fixture states: at `-O2` LLVM deletes allocations nothing
/// observes, so a zero there is evidence of nothing.
///
/// The `Drop`-body cell is not decoration. Every cell above is
/// memory-shaped, and an over-firing body is memory-CLEAN, so no column in
/// this file could see a duplicated body bought by the memory fix. All
/// eight body cells were measured on four surfaces (`build` at `-O0` and
/// `-O2`, `run`, `--interp`) and agree.
#[test]
fn asan_tuple_array_param_move_and_temp_arg_have_owners() {
    const H: &str = "fn pay(i: i64) -> String { return f\"tttttttttttttttt{i}\" }\n";

    // FAULT 1 — the row's cell (a). A by-value tuple param moved to a
    // local inside the callee.
    assert_clean_asan_run(
        &format!(
            "{H}fn eat(p: (Array[String, 2], i64)) {{ let q = p; println(f\"in:{{q.0[0]}}\"); }}\n\
                 fn main() {{\n\
                 \x20   let t: (Array[String, 2], i64) = ([pay(1), pay(2)], 7);\n\
                 \x20   eat(t);\n\
                 }}\n"
        ),
        &["in:tttttttttttttttt1"],
        "b165-param-moved-to-local",
    );
    // FAULT 1, the `Vec` spelling. The row recorded this position as
    // clean; it is not, and nothing in the suite covered it.
    assert_clean_asan_run(
        &format!(
            "{H}fn eat(p: (Vec[String], i64)) {{ let q = p; println(f\"in:{{q.0[0]}}\"); }}\n\
                 fn main() {{\n\
                 \x20   let t: (Vec[String], i64) = ([pay(1), pay(2)], 7);\n\
                 \x20   eat(t);\n\
                 }}\n"
        ),
        &["in:tttttttttttttttt1"],
        "b165-param-moved-to-local-vec",
    );

    // FAULT 2 — the row's cell (b). A fresh TEMPORARY argument whose param
    // is returned.
    assert_clean_asan_run(
        &format!(
            "{H}fn thru(p: (Array[String, 2], i64)) -> (Array[String, 2], i64) {{ return p; }}\n\
                 fn main() {{\n\
                 \x20   let u = thru(([pay(1), pay(2)], 7));\n\
                 \x20   println(f\"a0:{{u.0[0]}}\");\n\
                 }}\n"
        ),
        &["a0:tttttttttttttttt1"],
        "b165-temp-arg-param-returned",
    );
    // FAULT 2 WITH NO RETURN AND NO MOVE — the callee only READS. This is
    // the cell that shows fault 2 is about the temporary having no owner
    // and not about the `return` the row named, and it is the one neither
    // row measured.
    assert_clean_asan_run(
        &format!(
            "{H}fn eat(p: (Array[String, 2], i64)) {{ println(f\"in:{{p.0[0]}}\"); }}\n\
                 fn main() {{\n\
                 \x20   eat(([pay(1), pay(2)], 7));\n\
                 }}\n"
        ),
        &["in:tttttttttttttttt1"],
        "b165-temp-arg-read-only",
    );
    // BOTH FAULTS IN ONE CELL — 68 B before, two independent pairs of
    // buffers. Keeps the two fixes from being collapsed into one.
    assert_clean_asan_run(
        &format!(
            "{H}fn eat(p: (Array[String, 2], i64)) {{ let q = p; println(f\"in:{{q.0[0]}}\"); }}\n\
                 fn main() {{\n\
                 \x20   eat(([pay(1), pay(2)], 7));\n\
                 }}\n"
        ),
        &["in:tttttttttttttttt1"],
        "b165-temp-arg-and-param-moved",
    );
    // A METHOD receiver's by-value tuple param, which the row listed as
    // NOT MEASURED.
    assert_clean_asan_run(
            &format!(
                "{H}struct H2 {{ n: i64 }}\n\
                 impl H2 {{ fn eat(ref self, p: (Array[String, 2], i64)) {{ let q = p; println(f\"in:{{q.0[0]}}\"); }} }}\n\
                 fn main() {{\n\
                 \x20   let h = H2 {{ n: 1 }};\n\
                 \x20   h.eat(([pay(1), pay(2)], 7));\n\
                 }}\n"
            ),
            &["in:tttttttttttttttt1"],
            "b165-method-tuple-param-moved",
        );

    // BODIES EXACTLY ONCE, both faults' shapes. Memory-clean says nothing
    // about this: a doubled body frees nothing twice.
    assert_clean_asan_run(
        "struct D { id: i64, s: String }\n\
             impl Drop for D { fn drop(mut ref self) { println(f\"dD{self.id}\") } }\n\
             fn mkd(i: i64) -> D { return D { id: i, s: f\"ssssssssssssssss{i}\" } }\n\
             fn eat(p: (Array[D, 2], i64)) { let q = p; println(f\"in:{q.1}\"); }\n\
             fn main() {\n\
             \x20   let t: (Array[D, 2], i64) = ([mkd(1), mkd(2)], 7);\n\
             \x20   eat(t);\n\
             }\n",
        &["in:7", "dD1", "dD2"],
        "b165-param-moved-user-drop-bodies",
    );
    assert_clean_asan_run(
        "struct D { id: i64, s: String }\n\
             impl Drop for D { fn drop(mut ref self) { println(f\"dD{self.id}\") } }\n\
             fn mkd(i: i64) -> D { return D { id: i, s: f\"ssssssssssssssss{i}\" } }\n\
             fn thru(p: (Array[D, 2], i64)) -> (Array[D, 2], i64) { return p; }\n\
             fn main() {\n\
             \x20   let u = thru(([mkd(1), mkd(2)], 7));\n\
             \x20   println(f\"n:{u.1}\");\n\
             }\n",
        &["n:7", "dD1", "dD2"],
        "b165-temp-arg-returned-user-drop-bodies",
    );

    // CONTROLS. These were clean BEFORE this fix and must stay clean: each
    // one is a position where a second owner is the failure mode, so they
    // prove the two registrations did not widen past their cell.
    //
    // A NAMED-LOCAL argument whose param is returned — the binding already
    // carries its own drop, and the argument registrar claims only
    // producer shapes, so nothing here may give it a second.
    assert_clean_asan_run(
        &format!(
            "{H}fn thru(p: (Array[String, 2], i64)) -> (Array[String, 2], i64) {{ return p; }}\n\
                 fn main() {{\n\
                 \x20   let t: (Array[String, 2], i64) = ([pay(1), pay(2)], 7);\n\
                 \x20   let u = thru(t);\n\
                 \x20   println(f\"a0:{{u.0[0]}}\");\n\
                 }}\n"
        ),
        &["a0:tttttttttttttttt1"],
        "b165-named-arg-param-returned-control",
    );
    // A BARE `Array[T, N]` argument — the TRANSFER model, which the row
    // named as the one consistent alternative. Untouched by either fix and
    // the shape a widened predicate would double-free first.
    assert_clean_asan_run(
        &format!(
            "{H}fn thru(p: Array[String, 2]) -> Array[String, 2] {{ return p; }}\n\
                 fn main() {{\n\
                 \x20   let u = thru([pay(1), pay(2)]);\n\
                 \x20   println(f\"a0:{{u[0]}}\");\n\
                 }}\n"
        ),
        &["a0:tttttttttttttttt1"],
        "b165-bare-array-temp-returned-control",
    );
    assert_clean_asan_run(
        &format!(
            "{H}fn eat(p: Array[String, 2]) {{ let q = p; println(f\"in:{{q[0]}}\"); }}\n\
                 fn main() {{\n\
                 \x20   let a: Array[String, 2] = [pay(1), pay(2)];\n\
                 \x20   eat(a);\n\
                 }}\n"
        ),
        &["in:tttttttttttttttt1"],
        "b165-bare-array-param-moved-control",
    );
    // A SCALAR array element stays a no-op on both new gates.
    assert_clean_asan_run(
        &format!(
            "{H}fn eat(p: (Array[i64, 2], i64)) {{ let q = p; println(f\"in:{{q.0[0]}}\"); }}\n\
                 fn main() {{\n\
                 \x20   eat(([3, 4], 7));\n\
                 }}\n"
        ),
        &["in:3"],
        "b165-scalar-array-control",
    );
}

/// B-2026-09-13-23, THE GATE CELL — a tuple holding an `Array[T, N]` as an
/// ENUM PAYLOAD must not be corrupted by the array walk this row adds.
///
/// THIS IS THE FIXTURE THAT WOULD HAVE CAUGHT `506a91d`. That commit landed
/// the four body-position sites without the layout guard, and this exact
/// shape went from a 448 B leak to `free(): invalid pointer` — a regression
/// in KIND, on `main`, reverted as `49e75a8` and filed by another session as
/// B-2026-09-16-9 (high) before this one noticed.
///
/// WHY NOTHING CAUGHT IT: all 28 cells of that attempt were positions a
/// tuple can occupy in a function BODY — local, struct field, return,
/// destructure, by-value param, chained move, nested array, array of Vecs.
/// Not one put the tuple inside an enum payload, and no fixture in the
/// suite did either, so both ASAN ratchet legs matched their quarantine
/// lists exactly and seven gate legs went green over undefined behaviour.
/// The transferable rule, which this fixture exists to enforce: WHEN A FIX
/// WIDENS A SHARED WALKER, DERIVE THE CELL SET FROM THE WALKER'S CALLERS,
/// NOT FROM THE BUG'S SYMPTOMS. `emit_tuple_elem_drops` has an
/// enum-payload caller; the cell set never asked who calls it.
///
/// THE MECHANISM, measured rather than argued. Site 1 widens
/// `tuple_elem_needs_deep_drop`, and the enum drop CLASSIFIER
/// (`declare_enums`) consults that same predicate — so the variant newly
/// classified `EnumDropKind::NestedTuple`, which hands the payload's word
/// region to the tuple's own drop fn. Traced side by side with the `Vec`
/// spelling, which is the positive control:
///
///     (Vec[String], i64)        enum `{i64, i64, i64, i64, i64}`  4 payload words, tuple is 4  INLINE, correct
///     (Array[String, 2], i64)   enum `{i64, i64, i64}`            2 payload words, tuple is 7  BOXED, corrupt
///
/// Two words because a variant declaration can only spell an array as
/// `Path(["Array"], [Type(T), Const(N)])` and
/// `payload_word_count_for_type_expr`'s real-width arm is keyed on
/// `TypeKind::Array`, a kind only inference produces. The walker strode
/// seven words through two and freed `0x11` — a `String`'s length word.
///
/// THE FIX IS THE KIND, NOT THE WIDTH, and that direction was measured too.
/// Correcting the width so the payload lands inline regresses a DIRECT
/// array payload (`enum E { A(Array[String, 2]) }`) from clean to a 34 B
/// leak: the conservative 1 is load-bearing there, being exactly what
/// routes such a payload to the pack side's boxing where
/// `EnumDropKind::BoxedArray` frees it correctly — as that pass's own
/// comment says. So the guard stands the KIND down instead, and the
/// position keeps its PRE-EXISTING leak (56 B box + 34 B elements,
/// identical before and after), which is B-2026-09-12-10's remaining
/// enum-payload cell rather than this row's. A leak is the correct floor to
/// land on; the alternative is UB.
///
/// VISIBLE ONLY AT `-O0`, SO THIS IS A GATE ON `scripts/asan-o0-leg.sh` AND
/// NOT ON THE DEFAULT `--features llvm` LEG. Measured three ways with the
/// guard removed and a marker `grep -c` printing zero first:
///
///     no fix at all      leak fixture FAILS, this one passes (nothing walks the array)
///     sites 1-4 only     leak fixture passes, THIS ONE FAILS
///     all five           both pass
///
/// At the harness default the failing run is green, because the
/// allocations LLVM deletes take the corruption with them — the same reason
/// the row's own history insists a leak class is read at `-O0`. The ASAN
/// report at `-O0` names the route exactly:
///
///     karac_free_buf <- karac_drop_String <- karac_drop_Array_String_2
///       <- __karac_drop_tuple_te_Array_gString_xg_i64$in... <- __karac_drop_E
///     Address 0x0000000011 is a wild pointer
///
/// LEAK CHECKING IS NOW ON (B-2026-09-12-10). It was off while the guard's
/// `None` left this position with its pre-existing leak, and that row's
/// enum-payload cell is now closed by `EnumDropKind::BoxedTuple` — the guard
/// still stands `NestedTuple` down, and hands the position a kind that
/// DEREFS the box word before running the same interior walk. So the
/// paragraph above still describes the layout exactly; only the floor moved,
/// from "leak rather than UB" to "neither".
///
/// The row itself does NOT close on that: its other open cell is
/// `(bool, String)`, a sub-word-packed tuple `type_expr_word_aligned`
/// declines deliberately, which is a layout change and a different shape
/// from anything here.
#[test]
fn asan_array_in_a_tuple_enum_payload_is_not_corrupted() {
    if !asan_available() {
        eprintln!("[b23-enum-payload] ASAN unavailable on this host — skipping");
        return;
    }
    const H: &str = "fn pay(i: i64) -> String { return f\"tttttttttttttttt{i}\" }\n";
    // Three spellings: a named local moved in, an inline literal, and an
    // arm that binds the payload but reads only its SCALAR element. The
    // first two corrupted identically under `506a91d`, which is what ruled
    // out an unowned temporary as the cause.
    let cells: [(&str, &str); 3] = [
        (
            "b23-enum-payload-named-local",
            "\x20   let t: (Array[String, 2], i64) = ([pay(1), pay(2)], 7);\n\
                 \x20   let e = E.A(t);\n",
        ),
        (
            "b23-enum-payload-inline-literal",
            "\x20   let e = E.A(([pay(1), pay(2)], 7));\n",
        ),
        (
            "b23-enum-payload-repeated",
            "\x20   let mut e = E.B;\n\
                 \x20   for _i in 0..4 { e = E.A(([pay(1), pay(2)], 7)); }\n",
        ),
    ];
    for (label, build) in cells {
        let src = format!(
            "{H}enum E {{ A((Array[String, 2], i64)), B }}\n\
                 fn main() {{\n\
                 {build}\
                 \x20   match e {{\n\
                 \x20       E.A(u) => {{ println(f\"a0:{{u.0[0]}} n:{{u.1}}\") }}\n\
                 \x20       E.B => {{ println(\"b\") }}\n\
                 \x20   }}\n\
                 }}\n"
        );
        // B-2026-09-12-10 — LEAK CHECKING IS NOW ON. The doc above asked for
        // this flip once that row's enum-payload cell closed, and
        // `EnumDropKind::BoxedTuple` closes it: the guard below still stands
        // `NestedTuple` down, but it now hands the position a kind that
        // DEREFS the box word before running the same interior walk, so the
        // 56 B envelope and its 34 B of elements are freed instead of
        // stranded. The no-error assertion this replaces is subsumed —
        // `assert_clean_asan_run` fails on an ASAN report just as loudly.
        assert_clean_asan_run(&src, &["a0:tttttttttttttttt1 n:7"], label);
    }
    // A NESTED tuple around the array — `((Array[String, 2], i64), i64)`.
    // The guard's width helper recurses, so this stands down for the same
    // reason rather than corrupting one level in. Leak-unchecked for the
    // same reason as the cells above.
    {
        let label = "b23-enum-payload-nested-tuple";
        let src = format!(
            "{H}enum E {{ A(((Array[String, 2], i64), i64)), B }}\n\
                 fn main() {{\n\
                 \x20   let e = E.A(((Array[pay(1), pay(2)], 7), 9));\n\
                 \x20   match e {{\n\
                 \x20       E.A(u) => {{ println(f\"a0:{{u.0.0[0]}} k:{{u.1}}\") }}\n\
                 \x20       E.B => {{ println(\"b\") }}\n\
                 \x20   }}\n\
                 }}\n"
        );
        // B-2026-09-12-10 — leak-checked too, for the same reason: the
        // guard's width helper recurses, so a nested tuple around the array
        // reaches `BoxedTuple` at the outer level and is freed.
        assert_clean_asan_run(&src, &["a0:tttttttttttttttt1 k:9"], label);
    }
    // The `Vec` spelling is the POSITIVE CONTROL: its payload really is
    // inline, so it keeps `NestedTuple` and must stay fully clean — leak
    // checking included. If the guard ever widened to stand this down too,
    // the leak it would reintroduce fails HERE rather than quietly.
    assert_clean_asan_run(
        &format!(
            "{H}enum E {{ A((Vec[String], i64)), B }}\n\
                 fn main() {{\n\
                 \x20   let e = E.A(([pay(1), pay(2)], 7));\n\
                 \x20   match e {{\n\
                 \x20       E.A(u) => {{ println(f\"a0:{{u.0[0]}} n:{{u.1}}\") }}\n\
                 \x20       E.B => {{ println(\"b\") }}\n\
                 \x20   }}\n\
                 }}\n"
        ),
        &["a0:tttttttttttttttt1 n:7"],
        "b23-enum-payload-vec-control",
    );
    // A WIDE tuple payload with no array at all: its slot width is exact,
    // so the guard must not fire and this must stay clean. The guard's own
    // blast radius, pinned.
    assert_clean_asan_run(
        &format!(
            "{H}enum E {{ A((String, String, String)), B }}\n\
                 fn main() {{\n\
                 \x20   let e = E.A((pay(1), pay(2), pay(3)));\n\
                 \x20   match e {{\n\
                 \x20       E.A(u) => {{ println(f\"a0:{{u.0}} a2:{{u.2}}\") }}\n\
                 \x20       E.B => {{ println(\"b\") }}\n\
                 \x20   }}\n\
                 }}\n"
        ),
        &["a0:tttttttttttttttt1 a2:tttttttttttttttt3"],
        "b23-enum-payload-wide-string-tuple-control",
    );
}

/// B-2026-09-13-2 — the last of the four hand-back shapes: an arm-bound
/// `Array[T, N]` payload the arm MOVES ON had an owner on neither side.
///
/// `match m.remove(k) { Some(a) => { let b = a; read } }` over
/// `Map[i64, Array[String, 2]]` lost 176 B in 8 blocks over four hand-backs
/// at `-O0`, with the box reclaimed and nothing indirectly lost — exactly
/// the element buffers.
///
/// TWO REGISTRATIONS, EACH CORRECT ALONE, BOTH DISCLAIMING THIS SHAPE.
/// `track_freshtemp_boxed_enum_scrutinee`'s array arm withholds the box's
/// interior walker when the arm consumes the binding, because registering it
/// there double-freed `push`, a struct literal and `m.insert` — all three of
/// which acquire an owner at the destination.
/// `rebind_source_keeps_array_memory` then stands the `let b = a;`
/// DESTINATION down because the source is an arm-bound array, on the mirror
/// premise that "the ARM frees the payload". The withholding is now recorded
/// and that guard answers `false` for those sources alone.
///
/// THE FOUR MOVE CELLS ARE THE POINT OF THIS FIXTURE, not the leak cell.
/// This row has been attempted four times; `push`, `lit` and `insert` are
/// the exact spellings that double-freed on attempts one and two, and they
/// are here so the next attempt cannot repeat it silently. Each is clean
/// before and after this fix — the set the fix consults is read at one site
/// and a rebind is the only destination that reaches it.
///
/// `read` and `empty` are the non-moving controls, clean throughout: they
/// are what establish that the interior IS owned when the arm keeps it, so
/// the leak is the consuming path and not the hand-back.
///
/// NOT A CELL, DELIBERATELY: `Some(a) => { eat(a) }` — a by-value array
/// param, which is CALLEE-owned. The row records it clean on 2026-09-16;
/// measured on the unfixed tree today it is an `Invalid free`, i.e. a
/// regression from something that landed in between. It has no rebind in it,
/// so this fix cannot reach it (measured identical before and after) and it
/// is filed as its own corruption-class row. Including it would redden the
/// suite for that row's defect.
///
/// Observable at `-O0` only, like every cell in this family: at the default
/// opt level LLVM deletes an allocation nothing observes, so the ordinary
/// `--features llvm` run of this fixture is vacuous and
/// `scripts/asan-o0-leg.sh` is what exercises it.
#[test]
fn asan_arm_bound_array_payload_moved_on_has_an_owner() {
    // `HDR` builds the map, `FTR` closes the removal loop; a cell is just
    // its arm body, so the cells differ in exactly the thing under test.
    // Four removals against eight inserts, so the `None` arm never runs and
    // every cell exercises the hand-back four times. Each arm prints a
    // fixed marker — a READ of the moved payload where the shape has one,
    // via `.len()`, so the buffer has to be alive to answer.
    const HDR: &str = "fn eat(a: Array[String, 2]) -> i64 { return a[0].len() as i64; }\n\
             struct Bx { a: Array[String, 2], k: i64 }\n\
             fn main() {\n\
             \x20   let mut v: Map[i64, Array[String, 2]] = Map.new();\n\
             \x20   let mut i: i64 = 0i64;\n\
             \x20   while i < 8i64 {\n\
             \x20       v.insert(i, [f\"row-aaaaaaaaaaaaaaaa-{i}\", f\"col-bbbbbbbbbbbbbbbb-{i}\"]);\n\
             \x20       i = i + 1i64;\n\
             \x20   }\n\
             \x20   let mut j: i64 = 0i64;\n\
             \x20   while j < 4i64 {\n";
    const FTR: &str = "\x20       j = j + 1i64;\n\
             \x20   }\n\
             \x20   println(f\"end\");\n\
             }\n";
    let four = |m: &str| -> Vec<String> {
        let mut v: Vec<String> = (0..4).map(|_| m.to_string()).collect();
        v.push("end".to_string());
        v
    };

    for (arm, marker, label) in [
            // The row's cell.
            (
                "        match v.remove(j) { Option.Some(a) => { let b = a; println(f\"b:{b[0].len()}\"); } Option.None => { println(f\"n\") } }\n",
                "b:22",
                "b1302-rebind-to-local",
            ),
            // The three spellings that double-freed on the earlier attempts.
            (
                "        let mut keep: Vec[Array[String, 2]] = Vec.new();\n\
                 \x20       match v.remove(j) { Option.Some(a) => { keep.push(a); println(f\"p\") } Option.None => { println(f\"n\") } }\n",
                "p",
                "b1302-move-into-vec",
            ),
            (
                "        match v.remove(j) { Option.Some(a) => { v.insert(100i64 + j, a); println(f\"i\") } Option.None => { println(f\"n\") } }\n",
                "i",
                "b1302-move-back-into-map",
            ),
            (
                "        match v.remove(j) { Option.Some(a) => { let x = Bx { a: a, k: j }; println(f\"x:{x.a[0].len()}\") } Option.None => { println(f\"n\") } }\n",
                "x:22",
                "b1302-move-into-struct-literal",
            ),
            // The non-moving controls.
            (
                "        match v.remove(j) { Option.Some(a) => { println(f\"b:{a[0].len()}\") } Option.None => { println(f\"n\") } }\n",
                "b:22",
                "b1302-read-only-arm",
            ),
            (
                "        match v.remove(j) { Option.Some(a) => { println(f\"h\") } Option.None => { println(f\"n\") } }\n",
                "h",
                "b1302-empty-arm",
            ),
        ] {
            let src = format!("{HDR}{arm}{FTR}");
            let expected = four(marker);
            let refs: Vec<&str> = expected.iter().map(String::as_str).collect();
            assert_clean_asan_run(&src, &refs, label);
        }
}

/// B-2026-09-19-40 — AN ARM THAT ONLY HANDS ITS BOXED `Array` PAYLOAD TO A
/// FREE FUNCTION DOUBLE-FREED THE ELEMENT BUFFERS.
///
/// `src/consume_class.rs`'s founding assumption is that passing a value to
/// a user function transfers nothing: the callee entry-copies its by-value
/// params, so the caller still owns the original. A by-value
/// `Array[T, N]` param whose elements own a callee drop is the one
/// exception (B-2026-09-13-15 / B-2026-09-13-16) — there the CALLEE frees
/// the element buffers.
///
/// So `G.Y(x) => { return eats(x); }` over a heap-boxed `Array[String, 2]`
/// payload classified as READ-ONLY, `clear_boxed_enum_inner_drop` declined
/// to retract the box's interior walk, and both sides freed the same two
/// `String` buffers: `free(): double free detected in tcache 2`, exit 134
/// on AOT and under `karac run`, against a correct `--interp`. Valgrind
/// counted 2 invalid frees.
///
/// Three rows, and the two controls are the point rather than padding: the
/// MONO spelling was already correct (it retracts through a different
/// channel — the `field_drop_kinds` alias, which a generic enum's
/// declaration, spelled `T`, never reaches), and a read-only arm that hands
/// the payload NOWHERE must keep its interior drop, which is the double
/// free this fix would cause if it over-retracted.
#[test]
fn asan_boxed_array_payload_handed_to_callee_owned_param_no_double_free() {
    assert_clean_asan_run(
        r#"
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
"#,
        &[
            "hg:13", "hm:13", "rg:13", "hg:13", "hm:13", "rg:13", "hg:13", "hm:13", "rg:13", "end",
        ],
        "asan_boxed_array_payload_handed_to_callee_owned_param_no_double_free",
    );
}

/// B-2026-09-16-15 — a boxed generic-enum payload handed to a GENERIC
/// callee had its box freed and its ARRAY interior stranded. The monomorph
/// param registration in `mono.rs` was the one site `297237c` left at
/// `array_interior_ok: false` after B-2026-09-13-15's constructor
/// retraction removed the second owner that gate was defending against, so
/// the caller stood down and the callee declined to pick the interior up.
///
/// Whole program at `KARAC_OPT_LEVEL=0` under valgrind, same binary, one
/// line changed in `mono.rs`:
///
///     before   364 allocs / 274 frees, definitely lost 1,248 B in 84
///              blocks, indirectly lost 90 B in 6
///     after    364 allocs / 364 frees, all heap blocks freed
///
/// with stdout byte-identical between the two arms, which is what says the
/// change frees memory rather than moving a `Drop` body. Per element type,
/// measured one cell at a time: 26 B/2 (`String`), 24 B/2 (plain struct),
/// 24 B/2 (`impl Drop` struct, both bodies firing on both arms), 48+24 B
/// (`Vec[String]`), 24 B/2 (`Option[String]`), 20 B/4 (nested `Array`).
/// `Array[i64, 2]` is clean on both arms — nothing to free.
///
/// Cells 7-14 are the MUST-STAY-DECLINED controls: every spelling where a
/// named source could still own the interior, which is the shape
/// B-2026-09-12-18 measured as a double free when this arm first landed
/// ungated. They are clean before and after. A widening that reaches one of
/// them reddens this test instead of leaving a double free to be found by
/// the next person.
///
/// Cell 13 uses the NON-matching `mlen`. Its matching sibling — an
/// owned-`self` receiver on a generic impl whose body matches the receiver
/// — strands the same two element buffers (33 B in 2) IDENTICALLY on both
/// arms, so it is a neighbour rather than this row: filed separately.
/// `param_name != "self"` (B-2026-09-16-31) carves the receiver out of this
/// registration, so the caller is meant to own it and does not.
#[test]
fn asan_generic_callee_boxed_array_payload_interior_has_an_owner() {
    assert_clean_asan_run_min_allocs(
        r#"
struct Pr { a: String, b: i64 }
struct Rec { s: String }
impl Drop for Rec { fn drop(mut ref self) { println(f"dRec:{self.s}") } }
struct Hold { arr: Array[String, 2] }
enum G[T] { Y(T), N }

fn gshow[T](g: G[T]) -> i64 { match g { G.Y(v) => { println(f"  p={v}"); return 1; } G.N => { return 0; } } }
fn gnomatch[T](g: G[T]) -> i64 { return 2 }
fn two[T](p: G[T], q: G[T]) -> i64 { return 3 }

impl[T] G[T] { fn mlen(self) -> i64 { return 4 } }

fn main() {
    let base: i64 = env.args().len();
    let mut i: i64 = 0;
    while i < 3 {
        let k: i64 = base + i;
        let c1: G[Array[String, 2]] = G.Y([f"aaaaaaaaaaa-1-{k}", f"bbbbbbbbbbb-1-{k}"]);
        println(f"inline_nomatch:{gnomatch(c1)}");
        let c2: G[Array[String, 2]] = G.Y([f"cccccccccccc-2-{k}", f"dddddddddddd-2-{k}"]);
        println(f"inline_match:{gshow(c2)}");
        let c3: G[Array[Pr, 2]] = G.Y([Pr { a: f"eeeeeeeeeee-3-{k}", b: k }, Pr { a: f"fffffffffff-3-{k}", b: k }]);
        println(f"struct_elem:{gnomatch(c3)}");
        let c4: G[Array[Rec, 2]] = G.Y([Rec { s: f"ggggggggggg-4-{k}" }, Rec { s: f"hhhhhhhhhhh-4-{k}" }]);
        println(f"drop_elem:{gnomatch(c4)}");
        let c5: G[Array[Vec[String], 2]] = G.Y([vec![f"iiiiiiiiiii-5-{k}"], vec![f"jjjjjjjjjjj-5-{k}"]]);
        println(f"vec_elem:{gshow(c5)}");
        let c6: G[Array[Array[String, 2], 2]] = G.Y([[f"kkk-6-{k}", f"lll-6-{k}"], [f"mmm-6-{k}", f"nnn-6-{k}"]]);
        println(f"nested_elem:{gshow(c6)}");
        let a7: Array[String, 2] = [f"ooooooooooo-7-{k}", f"ppppppppppp-7-{k}"];
        let c7: G[Array[String, 2]] = G.Y(a7);
        println(f"named_local:{gshow(c7)}");
        let a8: Array[String, 2] = [f"qqqqqqqqqqq-8-{k}", f"rrrrrrrrrrr-8-{k}"];
        let c8: G[Array[String, 2]] = G.Y(a8);
        println(f"read_after_move:{gnomatch(c8)} {a8[0]}");
        let a9: Array[String, 2] = [f"sssssssssss-9-{k}", f"ttttttttttt-9-{k}"];
        let c9: G[Array[String, 2]] = G.Y(a9);
        println(f"call_then_read:{gnomatch(c9)}");
        println(f"still_live:{a9[1]}");
        let e10: String = f"uuuuuuuuuuu-10-{k}";
        let f10: String = f"vvvvvvvvvvv-10-{k}";
        let c10: G[Array[String, 2]] = G.Y([e10, f10]);
        println(f"elem_locals:{gshow(c10)}");
        let h11: Hold = Hold { arr: [f"wwwwwwwwwww-11-{k}", f"xxxxxxxxxxx-11-{k}"] };
        let c11: G[Array[String, 2]] = G.Y(h11.arr);
        println(f"struct_field:{gshow(c11)}");
        let p12: G[Array[String, 2]] = G.Y([f"yyyyyyyyyyy-12-{k}", f"zzzzzzzzzzz-12-{k}"]);
        let q12: G[Array[String, 2]] = G.Y([f"aaaaaaaaaab-12-{k}", f"bbbbbbbbbbc-12-{k}"]);
        println(f"two_params:{two(p12, q12)}");
        let c13: G[Array[String, 2]] = G.Y([f"ccccccccccd-13-{k}", f"ddddddddddde-13-{k}"]);
        println(f"self_recv:{c13.mlen()}");
        let c14: G[Array[i64, 2]] = G.Y([k, k + 1]);
        println(f"scalar_elem:{gshow(c14)}");
        i = i + 1;
    }
    println("end");
}
"#,
        &[
            "inline_nomatch:2",
            "  p=[cccccccccccc-2-1, dddddddddddd-2-1]",
            "inline_match:1",
            "struct_elem:2",
            "dRec:ggggggggggg-4-1",
            "dRec:hhhhhhhhhhh-4-1",
            "drop_elem:2",
            "  p=[[iiiiiiiiiii-5-1], [jjjjjjjjjjj-5-1]]",
            "vec_elem:1",
            "  p=[[kkk-6-1, lll-6-1], [mmm-6-1, nnn-6-1]]",
            "nested_elem:1",
            "  p=[ooooooooooo-7-1, ppppppppppp-7-1]",
            "named_local:1",
            "read_after_move:2 qqqqqqqqqqq-8-1",
            "call_then_read:2",
            "still_live:ttttttttttt-9-1",
            "  p=[uuuuuuuuuuu-10-1, vvvvvvvvvvv-10-1]",
            "elem_locals:1",
            "  p=[wwwwwwwwwww-11-1, xxxxxxxxxxx-11-1]",
            "struct_field:1",
            "two_params:3",
            "self_recv:4",
            "  p=[1, 2]",
            "scalar_elem:1",
            "inline_nomatch:2",
            "  p=[cccccccccccc-2-2, dddddddddddd-2-2]",
            "inline_match:1",
            "struct_elem:2",
            "dRec:ggggggggggg-4-2",
            "dRec:hhhhhhhhhhh-4-2",
            "drop_elem:2",
            "  p=[[iiiiiiiiiii-5-2], [jjjjjjjjjjj-5-2]]",
            "vec_elem:1",
            "  p=[[kkk-6-2, lll-6-2], [mmm-6-2, nnn-6-2]]",
            "nested_elem:1",
            "  p=[ooooooooooo-7-2, ppppppppppp-7-2]",
            "named_local:1",
            "read_after_move:2 qqqqqqqqqqq-8-2",
            "call_then_read:2",
            "still_live:ttttttttttt-9-2",
            "  p=[uuuuuuuuuuu-10-2, vvvvvvvvvvv-10-2]",
            "elem_locals:1",
            "  p=[wwwwwwwwwww-11-2, xxxxxxxxxxx-11-2]",
            "struct_field:1",
            "two_params:3",
            "self_recv:4",
            "  p=[2, 3]",
            "scalar_elem:1",
            "inline_nomatch:2",
            "  p=[cccccccccccc-2-3, dddddddddddd-2-3]",
            "inline_match:1",
            "struct_elem:2",
            "dRec:ggggggggggg-4-3",
            "dRec:hhhhhhhhhhh-4-3",
            "drop_elem:2",
            "  p=[[iiiiiiiiiii-5-3], [jjjjjjjjjjj-5-3]]",
            "vec_elem:1",
            "  p=[[kkk-6-3, lll-6-3], [mmm-6-3, nnn-6-3]]",
            "nested_elem:1",
            "  p=[ooooooooooo-7-3, ppppppppppp-7-3]",
            "named_local:1",
            "read_after_move:2 qqqqqqqqqqq-8-3",
            "call_then_read:2",
            "still_live:ttttttttttt-9-3",
            "  p=[uuuuuuuuuuu-10-3, vvvvvvvvvvv-10-3]",
            "elem_locals:1",
            "  p=[wwwwwwwwwww-11-3, xxxxxxxxxxx-11-3]",
            "struct_field:1",
            "two_params:3",
            "self_recv:4",
            "  p=[3, 4]",
            "scalar_elem:1",
            "end",
        ],
        "asan_generic_callee_boxed_array_payload_interior_has_an_owner",
        120,
    );
}

/// B-2026-09-17-21 — a `shared enum`'s heap-BOXED nameless-aggregate
/// payload now owns its INTERIOR, not just its envelope.
///
/// B-2026-09-15-10 gave the box itself an owner and left the elements
/// "to whoever owns them, and for a shared enum that is still the SOURCE".
/// The row filed against that remainder described the axis as PROVENANCE —
/// a temporary leaks, a named local reaches 0/0 — and proposed arming the
/// interior for a temp source only. Measured at `KARAC_OPT_LEVEL=0
/// KARAC_AUTO_PAR=0` under `valgrind --leak-check=full`, with an
/// invalid-read column, there are THREE regimes and the axis is not
/// provenance but WHICH DIES FIRST:
///
///     named local, handle dies inside its scope    0 lost,  0 invalid
///     named local, handle OUTLIVES the block       0 lost,  2 INVALID READS
///     named local, handle leaves the frame        54 lost,  0 invalid
///     temporary, every spelling                52-162 lost,  0 invalid
///
/// Cell D below is the second regime. The source's own drop frees a
/// 27-byte element string at the block's exit and `memmove` reads it back
/// through the surviving handle — and it prints the RIGHT characters, so
/// every output oracle in this tree scores that cell as correct. Only the
/// invalid-read column sees it, which is why this fixture exists in the
/// ASAN file and has no twin in `tests/codegen.rs`.
///
/// WHAT IS ASSERTED HERE THAT PROSE CANNOT BE: the MUST-STAY-DECLINED
/// cells. Arming the box without retracting the named source double-frees
/// every cell where the source's own drop still runs, so cells 8, 9, A, B,
/// H and I are the real gate on this change — a double free there is what
/// a wrong version of it looks like. Cell B is the read-after-move shape,
/// where `suppress_array_local_move_into_ctor` declines because a defensive
/// copy happened: two objects, two frees, and the source's own read still
/// intact (it prints the same string twice on purpose). Cell E is the arm
/// that hands the payload out of the box, which reported TWO INVALID FREES
/// against an intermediate version of this fix that armed the box without
/// admitting shared enums to `register_boxed_array_payload_alias`.
///
/// EVERY CELL'S STRINGS HAVE A DISTINCT LENGTH, so if the widening ever
/// goes wrong the leaked or double-freed byte count names which cell moved
/// rather than leaving a total to apportion.
///
/// NO CELL'S ELEMENT RUNS A USER `Drop` BODY, and the omission is
/// deliberate. `shared enum Sh { S(Array[R, 2]), N }` for an `R` with an
/// `impl Drop` is memory-clean under this fix (54 B leaked before, 0
/// after) but runs NEITHER body on the compiled backends while `--interp`
/// runs both — measured identically on `origin/main`, so it is not this
/// change's doing. That divergence is B-2026-09-20-2. A cell carrying it
/// would have to assert the wrong output to stay green, which is a pin on
/// a bug's wrong answer, so it is cited rather than written.
///
/// FLOORED at 80 allocations for the usual reason: orphaned element
/// buffers are exactly what LLVM deletes when nothing observes them, so
/// every cell reads its payload's CONTENTS rather than its length.
#[test]
fn asan_shared_enum_boxed_array_payload_interior_has_an_owner() {
    assert_clean_asan_run_min_allocs(
        r#"
shared enum Sh { S(Array[String, 2]), N }
shared enum Nst { S(Array[Array[String, 2], 2]), N }
shared enum Two { A(Array[String, 2]), B(Array[i64, 3]), N }
shared enum Sc { S(Array[i64, 2]), N }

fn mk1(t: String) -> Array[String, 2] { return [f"c1-{t}-a", f"c1-{t}-bb"]; }
fn mk2(t: String) -> Array[String, 2] { return [f"c2-{t}-aaa", f"c2-{t}-bbbb"]; }
fn mk3(t: String) -> Array[String, 2] { return [f"c3-{t}-aaaaa", f"c3-{t}-bbbbbb"]; }
fn mk4(t: String) -> Array[String, 2] { return [f"c4-{t}-aaaaaaa", f"c4-{t}-bbbbbbbb"]; }
fn mk7(t: String) -> Array[String, 2] { return [f"c7-{t}-aaaaaaaaa", f"c7-{t}-bbbbbbbbbb"]; }
fn mk8(t: String) -> Array[String, 2] { return [f"c8-{t}-aaaaaaaaaaa", f"c8-{t}-bbbbbbbbbbbb"]; }
fn mkB(t: String) -> Array[String, 2] { return [f"cB-{t}-aaaaaaaaaaaaa", f"cB-{t}-bbbbbbbbbbbbbb"]; }
fn mkC(t: String) -> Array[String, 2] { return [f"cC-{t}-aaaaaaaaaaaaaaa", f"cC-{t}-bbbbbbbbbbbbbbbb"]; }
fn mkD(t: String) -> Array[String, 2] { return [f"cD-{t}-aaaaaaaaaaaaaaaaa", f"cD-{t}-bbbbbbbbbbbbbbbbbb"]; }
fn mkE(t: String) -> Array[String, 2] { return [f"cE-{t}-aaaaaaaaaaaaaaaaaaa", f"cE-{t}-bbbbbbbbbbbbbbbbbbbb"]; }
fn mkF(t: String) -> Array[String, 2] { return [f"cF-{t}-aaaaaaaaaaaaaaaaaaaaa", f"cF-{t}-bbbbbbbbbbbbbbbbbbbbbb"]; }

fn look(s: Sh) -> i64 { match s { Sh.S(x) => { return x[0].len(); } Sh.N => { return 0; } } }
fn takeout(s: Sh) -> Array[String, 2] { match s { Sh.S(x) => { return x; } Sh.N => { return mk1("z"); } } }
fn fwd(a: Array[String, 2]) -> Array[String, 2] { return a; }
fn build() -> Sh { let a = mkC("fr"); return Sh.S(a); }

fn main() {
    // 1 TEMP from a call.
    { let s = Sh.S(mk1("t")); match s { Sh.S(x) => { println(f"1:{x[0]}"); } Sh.N => { println("e"); } } }
    // 2 TEMP, two handles to ONE box: exactly one free.
    { let s1 = Sh.S(mk2("t")); let s2 = s1; match s2 { Sh.S(x) => { println(f"2:{x[1]}"); } Sh.N => { println("e"); } } }
    // 3 TEMP, three constructions in a loop.
    let mut i: i64 = 0;
    while i < 3 {
        { let s = Sh.S(mk3(f"{i}")); match s { Sh.S(x) => { println(f"3:{x[0]}"); } Sh.N => { println("e"); } } }
        i = i + 1;
    }
    // 4 TEMP, two of them held by a Vec.
    {
        let mut v: Vec[Sh] = Vec.new();
        v.push(Sh.S(mk4("p")));
        v.push(Sh.S(mk4("q")));
        match v[0] { Sh.S(x) => { println(f"4:{x[0]}"); } Sh.N => { println("e"); } }
        match v[1] { Sh.S(x) => { println(f"4:{x[1]}"); } Sh.N => { println("e"); } }
    }
    // 5 TEMP, nested aggregate as an inline literal.
    { let s = Nst.S([[f"c5-aaaaaaaaaaaaaaaaaaaaaaa", f"c5-bbbbbbbbbbbbbbbbbbbbbbbb"], [f"c5-ccccccccccccccccccccccccc", f"c5-dddddddddddddddddddddddddd"]]);
      match s { Nst.S(x) => { println(f"5:{x[0][0]}/{x[1][1]}"); } Nst.N => { println("e"); } } }
    // 6 TEMP, inline flat literal -- a literal is a temp too.
    { let s = Sh.S([f"c6-aaaaaaaaaaaaaaaaaaaaaaaaaaa", f"c6-bbbbbbbbbbbbbbbbbbbbbbbbbbbb"]);
      match s { Sh.S(x) => { println(f"6:{x[0]}"); } Sh.N => { println("e"); } } }
    // 7 TEMP routed through a by-value parameter and returned.
    { let s = Sh.S(fwd(mk7("t"))); match s { Sh.S(x) => { println(f"7:{x[0]}"); } Sh.N => { println("e"); } } }

    // 8 NAMED, handle dies inside the local's scope -- MUST STAY DECLINED.
    let a8 = mk8("n");
    { let s = Sh.S(a8); match s { Sh.S(x) => { println(f"8:{x[0]}"); } Sh.N => { println("e"); } } }
    // 9 NAMED nested -- MUST STAY DECLINED.
    let a9: Array[Array[String, 2], 2] = [[f"c9-aaaaaaaaaaaaaaaaaaaaaaaaaaaaa", f"c9-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"], [f"c9-ccccccccccccccccccccccccccccc", f"c9-dddddddddddddddddddddddddddddd"]];
    { let s = Nst.S(a9); match s { Nst.S(x) => { println(f"9:{x[1][1]}"); } Nst.N => { println("e"); } } }
    // A NAMED, two handles -- MUST STAY DECLINED, and still exactly one free.
    let aA: Array[String, 2] = [f"cA-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", f"cA-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"];
    { let s1 = Sh.S(aA); let s2 = s1; match s2 { Sh.S(x) => { println(f"A:{x[1]}"); } Sh.N => { println("e"); } } }
    // B NAMED, READ AFTER the move: the defensive copy gives the box its own
    // buffers, so the source keeps its drop and this must not double-free.
    let aB = mkB("n");
    { let s = Sh.S(aB); match s { Sh.S(x) => { println(f"B:{x[0]}"); } Sh.N => { println("e"); } } }
    println(f"B:{aB[0]}");

    // C NAMED, handle leaves the FRAME -- leaked before this fix.
    { let s = build(); match s { Sh.S(x) => { println(f"C:{x[1]}"); } Sh.N => { println("e"); } } }
    // D NAMED, handle OUTLIVES the local's block -- READ FREED MEMORY before.
    let mut keep: Sh = Sh.N;
    { let aD = mkD("n"); keep = Sh.S(aD); }
    match keep { Sh.S(x) => { println(f"D:{x[0]}"); } Sh.N => { println("e"); } }

    // E an arm RETURNS the payload out of the box: the box must not free what
    // the arm handed on. Two invalid frees without the alias disarm.
    { let a = takeout(Sh.S(mkE("t"))); println(f"E:{a[1]}"); }
    // F the handle is REASSIGNED: the first box is freed exactly once.
    { let mut s: Sh = Sh.S(mkF("one")); match s { Sh.S(x) => { println(f"F:{x[0]}"); } Sh.N => { println("e"); } }
      s = Sh.S(mkF("two")); match s { Sh.S(x) => { println(f"F:{x[0]}"); } Sh.N => { println("e"); } } }
    // G the handle passed BY VALUE to a function.
    { let s = Sh.S(mk1("g")); println(f"G:{look(s)}"); }
    // H two boxing variants, one named source each.
    let aH: Array[String, 2] = [f"cH-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", f"cH-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"];
    { let s = Two.A(aH); match s { Two.A(v) => { println(f"H:{v[0]}"); } Two.B(w) => { println(f"{w[0]}"); } Two.N => { println("e"); } } }
    let aI: Array[i64, 3] = [1, 2, 3];
    { let s = Two.B(aI); match s { Two.A(v) => { println(f"{v[0]}"); } Two.B(w) => { println(f"I:{w[2]}"); } Two.N => { println("e"); } } }
    // J scalar payload: no interior at all, the cleanest control on the walk.
    { let s = Sc.S([7, 8]); match s { Sc.S(x) => { println(f"J:{x[0]}/{x[1]}"); } Sc.N => { println("e"); } } }
    println("done");
}
"#,
        &[
            "1:c1-t-a",
            "2:c2-t-bbbb",
            "3:c3-0-aaaaa",
            "3:c3-1-aaaaa",
            "3:c3-2-aaaaa",
            "4:c4-p-aaaaaaa",
            "4:c4-q-bbbbbbbb",
            "5:c5-aaaaaaaaaaaaaaaaaaaaaaa/c5-dddddddddddddddddddddddddd",
            "6:c6-aaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "7:c7-t-aaaaaaaaa",
            "8:c8-n-aaaaaaaaaaa",
            "9:c9-dddddddddddddddddddddddddddddd",
            "A:cA-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "B:cB-n-aaaaaaaaaaaaa",
            "B:cB-n-aaaaaaaaaaaaa",
            "C:cC-fr-bbbbbbbbbbbbbbbb",
            "D:cD-n-aaaaaaaaaaaaaaaaa",
            "E:cE-t-bbbbbbbbbbbbbbbbbbbb",
            "F:cF-one-aaaaaaaaaaaaaaaaaaaaa",
            "F:cF-two-aaaaaaaaaaaaaaaaaaaaa",
            "G:6",
            "H:cH-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "I:3",
            "J:7/8",
            "done",
        ],
        "asan_shared_enum_boxed_array_payload_interior_has_an_owner",
        80,
    );

    // The Arc path. Same layout, different release function — the half the
    // first cut of B-2026-09-15-10 left leaking, asked again here for the
    // interior. Temp, named, and the escaping-named regime in one program.
    assert_clean_asan_run(
        r#"
par enum Pr { S(Array[String, 2]), N }
fn mkp(t: String) -> Array[String, 2] { return [f"p-{t}-aaaaaaaaaaaaaaaaaaaa", f"p-{t}-bbbbbbbbbbbbbbbbbbbb"]; }
fn main() {
    { let s = Pr.S(mkp("t")); match s { Pr.S(x) => { println(f"par-temp:{x[0]}"); } Pr.N => { println("e"); } } }
    let a = mkp("n");
    { let s = Pr.S(a); match s { Pr.S(x) => { println(f"par-named:{x[1]}"); } Pr.N => { println("e"); } } }
    let mut keep: Pr = Pr.N;
    { let b = mkp("d"); keep = Pr.S(b); }
    match keep { Pr.S(x) => { println(f"par-escape:{x[0]}"); } Pr.N => { println("e"); } }
    println("done");
}
"#,
        &[
            "par-temp:p-t-aaaaaaaaaaaaaaaaaaaa",
            "par-named:p-n-bbbbbbbbbbbbbbbbbbbb",
            "par-escape:p-d-aaaaaaaaaaaaaaaaaaaa",
            "done",
        ],
        "b1721-par-enum-arc-path-interior",
    );
}
