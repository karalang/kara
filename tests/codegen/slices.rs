//! slices, indexing, windows and chunks -- fixtures for `tests/codegen.rs`.
//!
//! Split out of `tests/codegen.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test codegen` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test codegen slices::
//!
//! New fixtures about slices, indexing, windows and chunks belong in this file.

use super::*;

#[test]
fn e2e_index_into_tuple_element_vec() {
    // B-2026-07-20-2: indexing a `Vec` in a TUPLE element (`t.0[i]`) failed
    // codegen LOUD ("Index operator applied to non-array type") while the
    // interpreter read the element. The index dispatch had a `FieldAccess`
    // root arm but no `TupleIndex` sibling. Covers scalar and `String`
    // element reads over an inferred tuple binding.
    if let Some(out) = run_program(
        "fn make() -> (Vec[i64], Vec[String]) {\n\
                 let mut a: Vec[i64] = Vec.new();\n\
                 a.push(10); a.push(20); a.push(30);\n\
                 let mut b: Vec[String] = Vec.new();\n\
                 b.push(\"x\"); b.push(\"yy\");\n\
                 (a, b)\n\
             }\n\
             fn main() {\n\
                 let t = make();\n\
                 println(f\"{t.0[0]} {t.0[2]}\");\n\
                 println(t.1[1]);\n\
             }",
    ) {
        assert_eq!(out, "10 30\nyy\n");
    }
}

#[test]
fn e2e_index_store_into_tuple_element_vec() {
    // B-2026-07-20-3: index STORE into a Vec in a tuple element
    // (`t.0[i] = v`) was rejected loud by codegen ("Index assignment
    // target must be a variable") — and dropped SILENTLY by the
    // interpreter. Now the store lands; the heap store also frees the
    // overwritten `String` (checked leak-clean by the memsan sibling).
    if let Some(out) = run_program(
        "fn make() -> (Vec[i64], Vec[String]) {\n\
                 let mut a: Vec[i64] = Vec.new();\n\
                 a.push(1); a.push(2); a.push(3);\n\
                 let mut b: Vec[String] = Vec.new();\n\
                 b.push(\"x\"); b.push(\"y\");\n\
                 (a, b)\n\
             }\n\
             fn main() {\n\
                 let mut t = make();\n\
                 t.0[0] = 10;\n\
                 t.0[2] = 30;\n\
                 t.1[1] = \"zebra\";\n\
                 println(f\"{t.0[0] + t.0[1] + t.0[2]}\");\n\
                 println(t.1[0]);\n\
                 println(t.1[1]);\n\
             }",
    ) {
        assert_eq!(out, "42\nx\nzebra\n");
    }
}

/// B-2026-08-27-53 — a `Slice[T]` parameter fed from a field of a
/// BORROWED binding: `head(self.xs)` in an impl method, `head(g.xs)` for
/// `g: ref Bag[T]`, and `head(w.b.xs)` one hop deeper.
///
/// `coerce_to_slice`'s place arm resolves the header through
/// `field_chain_place_ptr`, which BAILS when the chain's root is a
/// `ref`/`mut ref` param or a `ref self` receiver — correctly, for its
/// other callers, which suppress move-outs and must not write through a
/// borrow the callee does not own. The coercion read that `None` as
/// "carry on" and forwarded a raw 3-word `{ptr,len,cap}` to the 2-word
/// `Slice[T]` formal, so LLVM module verification hard-failed and the
/// program did not build at all — on a shape the interpreter runs.
///
/// The row that reported this named a GENERIC-impl method, but genericity
/// is not the axis: a plain `impl` and a plain `ref` param failed
/// identically, while an OWNED param of the same struct built and ran.
/// The arms here pin the borrow axis rather than the generic one — a
/// generic impl receiver, a generic `ref` param, both element types, and
/// a two-hop chain whose owned spelling already built. Twin of
/// `tests/interpreter.rs`'s
/// `test_slice_param_from_a_borrowed_field_place`.
#[test]
fn e2e_slice_param_from_a_borrowed_field_place() {
    let Some(out) = run_program(
        r#"struct Bag[=T] { xs: Vec[T] }
struct Wrap { b: Bag[i64] }
fn head[T](s: Slice[T]) -> T { return s[0]; }
fn via_ref[T](g: ref Bag[T]) -> T { return head(g.xs); }
fn nested(w: ref Wrap) -> i64 { return head(w.b.xs); }
impl[T] Bag[T] {
    fn first(ref self) -> T { return head(self.xs); }
}
fn main() {
    let mut a: Bag[String] = Bag { xs: Vec.new() };
    a.xs.push("aa"); a.xs.push("bb");
    println(a.first());
    println(via_ref(a));
    let mut n: Bag[i64] = Bag { xs: Vec.new() };
    n.xs.push(7); n.xs.push(8);
    println(f"{n.first()}");
    println(f"{via_ref(n)}");
    let w: Wrap = Wrap { b: n };
    println(f"{nested(w)}");
    println("end");
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "aa\naa\n7\n7\n7\nend\n");
}

/// B-2026-08-27-53, write leg — the same coercion at a `mut Slice[i64]`
/// formal, through `mut ref self` and through a named `mut ref` param.
///
/// Load-bearing for the same reason the `mut` arms of
/// `e2e_slice_param_from_a_place_argument` are: a header built over a COPY
/// of the borrowed struct would compile and read back fine while silently
/// dropping every write. `go` then `poke` each add 100 to element 0, so
/// the printed `101` and `201` are what prove the header points at the
/// caller's real buffer, and `a.xs[1]` staying `2` proves the write landed
/// at the right offset. Twin of `tests/interpreter.rs`'s
/// `test_mut_slice_param_from_a_borrowed_field_place`.
#[test]
fn e2e_mut_slice_param_from_a_borrowed_field_place() {
    let Some(out) = run_program(
        r#"struct Bag { xs: Vec[i64] }
fn bump(s: mut Slice[i64]) { s[0] = s[0] + 100; }
fn head(s: Slice[i64]) -> i64 { return s[0]; }
fn poke(b: mut ref Bag) { bump(b.xs); }
impl Bag {
    fn go(mut ref self) { bump(mut self.xs); }
    fn peek(ref self) -> i64 { return head(self.xs); }
}
fn main() {
    let mut a: Bag = Bag { xs: Vec.new() };
    a.xs.push(1); a.xs.push(2);
    a.go();
    println(f"{a.peek()}");
    poke(mut a);
    println(f"{a.peek()} {a.xs[1]}");
    println("end");
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "101\n201 2\nend\n");
}

/// B-2026-08-05-40 — a `Slice[T]` parameter fed from a PLACE.
///
/// `coerce_to_slice` understood a bare identifier, a range index, a
/// collection literal and a fresh rvalue, but no PLACE. So `f(g.a)`,
/// `f(g.q.a)`, `f(t.0)` and `f(vv[0])` reached the call as a raw 3-word
/// `{ptr,len,cap}` against a 2-word `{ptr,i64}` slot and LLVM module
/// verification hard-failed with `Call parameter type does not match
/// function signature!` — the program did not compile at all, on a shape
/// the interpreter runs correctly.
///
/// Both by-value `Slice[T]` and `mut Slice[T]` were refused; read-only
/// `ref Slice[T]` was not, because its argument takes the borrow path in
/// `call_dispatch.rs` and never reaches the coercion. That asymmetry is
/// what makes the `rtotal` arm here load-bearing: it is the spelling that
/// already worked, and the fix has to leave it working — a ref slot takes
/// a POINTER, so letting the value-producing place arm win there would
/// reintroduce the same verification failure one slot-shape over.
///
/// Four place spellings, each MUTATED through `mut Slice` before it is
/// read: a header built over a COPY would compile and read fine while
/// silently dropping every write, so the mutation is what proves the
/// header points at the caller's real buffer.
///
/// Expected total is DERIVED, not read off a run: `mkv(i)` is
/// `[i, i+1, i+2]`, `bumpall` makes it `[i+1, i+2, i+3]` summing to
/// `3i + 6`; five such rounds plus the tag check give `15i + 31`, and
/// `sum(15i for i in 0..=199) + 31 * 200 = 298500 + 6200 = 304700`.
#[test]
fn e2e_slice_param_from_a_place_argument() {
    let Some(out) = run_program(
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
        i = i + 1i64;
    }
    println(acc);
}"#,
    ) else {
        return;
    };
    assert_eq!(out, "304700\n");
}

/// B-2026-09-02-11 — A DEFENSIVE COPY OF AN INDEXED ELEMENT MUST NOT RUN ITS
/// PAYLOAD'S `Drop` BODY EITHER. The payload-level twin of
/// `e2e_defensive_scrutinee_copy_does_not_rerun_the_enum_own_drop_body`,
/// one level in from the enum wrapper that row already withheld.
///
/// `match v[i]` stages the element into a deep clone
/// (`clone_owned_vec_index_element`) so an arm binding never aliases the
/// container's buffer. B-2026-08-29-37 stopped the ENUM's own body from
/// running on that clone; the PAYLOAD's body is a different call — the
/// field/payload walk, not the type wrapper — and it was never withheld, so
/// `bind_pattern_values` registered the full `karac_drop_<T>` wrapper on the
/// clone's binding while the container went on running the element's own
/// body at its NLL death. Measured on `7ecebf2`: `bare` printed
/// `got 2 dR1 dE dR1` against `--interp`'s `got 2 dE dR1`, on all three
/// compiled surfaces (JIT, default `karac build`, `KARAC_AUTO_PAR=0`).
///
/// SIX LEGS, three that were wrong and three controls, because the fix
/// withholds a body and the failure mode of over-reaching is a body that
/// never runs at all:
///   - `bare` — `match v[0]`, the shape the row reported.
///   - `field` — `match h.xs[0]`, the FIELD-ROOTED index
///     (`expr_is_heap_vec_index_field_rooted`), a separate predicate that
///     would have been missed by keying on the bare spelling alone.
///   - `scalar` — a payload struct with NO heap at all. It doubles for the
///     same reason, which is what shows the defect is about body
///     registration rather than about buffer ownership.
///   - `wild` — THE UNBOUND CONTROL. `E.A(_)` binds nothing, so no
///     registration was ever made and this leg was already correct; it
///     catches a fix that silences the container's own fire instead.
///   - `fresh` — `match mk(5)`, a genuine owned temp with no other owner.
///     Its payload body MUST still run at the binding (B-2026-07-11-26's
///     population), so an over-reaching gate loses it here.
///   - `local` — a named local scrutinee, likewise owned by the arm's
///     binding rather than by a container.
#[test]
fn e2e_index_element_clone_does_not_rerun_the_payload_drop_body() {
    let Some(out) = run_program(INDEX_ELEM_CLONE_PAYLOAD_BODY_SRC) else {
        return;
    };
    assert_eq!(out, INDEX_ELEM_CLONE_PAYLOAD_BODY_EXPECTED);
}

/// The ANTI-VACUITY GUARD for the E2E above: the fix must withhold the
/// BODY, not the memory cleanup, and an output comparison cannot tell those
/// apart (the clone's buffer leaking is silent).
///
/// So assert the two halves separately against `leg_bare`'s IR: the element
/// clone IS still emitted and a struct cleanup IS still registered on the
/// binding, while the user body wrapper is NOT. `leg_fresh` is the paired
/// positive — the same enum, the same arm shape, a genuine owned temp —
/// so "no `karac_drop_R` anywhere" cannot pass by losing the call entirely.
#[test]
fn ir_index_element_clone_keeps_memory_cleanup_and_drops_the_payload_body() {
    let ir = ir_for(INDEX_ELEM_CLONE_PAYLOAD_BODY_SRC);
    let body_of = |sym: &str| -> String {
        let mut out = String::new();
        let mut inside = false;
        for l in ir.lines() {
            if !inside && l.starts_with("define") && l.contains(sym) {
                inside = true;
            }
            if inside {
                out.push_str(l);
                out.push('\n');
                if l == "}" {
                    break;
                }
            }
        }
        assert!(!out.is_empty(), "no define block for {sym}\n{ir}");
        out
    };

    let bare = body_of("@leg_bare(");
    // The element clone still happens — this is what keeps the binding off
    // the container's buffer.
    assert!(
        bare.contains("karac_vec_clone") || bare.contains("elem.clone"),
        "the indexed-element clone stopped being emitted, so the E2E twin \
             is now vacuous:\n{bare}"
    );
    // …but the payload's user `Drop` body is NOT run on it.
    assert!(
        !bare.contains("call void @karac_drop_R("),
        "B-2026-09-02-11: the payload's Drop body runs on a defensive copy \
             of an element the container still owns:\n{bare}"
    );

    // The control: a genuine fresh temp keeps its payload body.
    let fresh = body_of("@leg_fresh(");
    assert!(
        fresh.contains("call void @karac_drop_R("),
        "a fresh owned temp lost its payload Drop body — B-2026-07-11-26 \
             regressed:\n{fresh}"
    );
}

/// B-2026-09-02-15 — THE `if let` FAMILY NEVER CLONED AN INDEXED ELEMENT, SO
/// IT DROP-TRACKED A BIT-COPY OF THE CONTAINER'S SLOT AND THE PROGRAM DIED.
///
/// `compile_match` deep-clones a heap-`Vec` element scrutinee before
/// materializing it (B-2026-06-14-12's direct-match sibling), and
/// `materialize_freshtemp_enum_scrutinee`'s `heap_index` leg is written on
/// that assumption — it drop-tracks whatever it is handed. The three
/// `control_flow.rs` siblings (`compile_if_let`, `compile_while_let`,
/// `compile_let_else`) called the materializer directly and never called the
/// cloner, so `__karac_drop_E` freed the container's buffer at the
/// construct's exit and the container freed it again at its own. Measured on
/// `811781c`: `free(): double free detected in tcache 2` on the JIT, the
/// default `karac build` and `KARAC_AUTO_PAR=0` alike, where `--interp`
/// printed a clean `A got 4 dE dR3 B`; valgrind named it
/// `Invalid free() / delete / delete[] / realloc()`.
///
/// SIX LEGS, and the last two are the controls that keep the fix honest:
///   - `iflet` — `if let E.A(r) = v[0]`, the shape as reported.
///   - `field` — `if let E.A(r) = h.xs[0]`, the field-rooted index.
///   - `whilelet` — the `while let` spelling, whose scrutinee is
///     re-evaluated (and so re-cloned) every iteration.
///   - `unbound` — `E.A(_)`. It crashed too, which is what shows the defect
///     was the SCRUTINEE's ownership and never the binding's.
///   - `nodrop` — the same shape with NO `impl Drop` anywhere. It crashed
///     too, so this is pure memory and no body count can stand in for it.
///   - `fresh` — `if let E.A(r) = mk(6)`, a genuine owned temp. The cloner
///     must stay a no-op there, and its `dR6 dE` is what proves it.
///
/// The `let … else` spelling is fixed by the same call but is NOT in this
/// fixture: its interpreter answer carried one extra payload body, a
/// separate interpreter-side divergence filed at this fix's close as
/// B-2026-09-02-17 and fixed there. Its own fixture is
/// `e2e_let_else_over_an_indexed_element_runs_one_payload_body`, below.
#[test]
fn e2e_index_element_clone_reaches_the_if_let_family() {
    let Some(out) = run_program(IFLET_INDEX_ELEM_CLONE_SRC) else {
        return;
    };
    assert_eq!(out, IFLET_INDEX_ELEM_CLONE_EXPECTED);
}

/// B-2026-09-15-33 — an index-assign whose RHS is a bare IDENTIFIER runs
/// the displaced element's user `Drop` body.
///
/// `store_destroys_displaced` is the DESTROY-vs-RELOCATE discriminator, and
/// it declined an identifier RHS on the grounds that the value already
/// existed somewhere, so the slot's previous occupant was being shuffled
/// rather than ended. That reasoning was B-2026-08-26-21's: a two-element
/// swap written `let t = b.xs[0]; b.xs[0] = b.xs[1]; b.xs[1] = t;` printed
/// FIVE bodies for two values.
///
/// THAT PROGRAM NO LONGER COMPILES, which is what makes the arm safe to
/// add. `E_INDEX_MOVE_NON_COPY` rejects both of its legs, and it rejects
/// them for a scalar-only `struct Item { id: i64 }` carrying an
/// `impl Drop` — -08-26-21's own element type — because a `Drop` impl makes
/// a type non-`Copy` whatever its fields are. This predicate's answer is
/// observable only for an element whose `Drop` body runs, every such
/// element is non-`Copy`, so no type is left for which the relocation shape
/// typechecks at all. The surviving idiom is `v.swap(i, j)`, a codegen
/// intrinsic that never reaches this path.
///
/// EVERY CELL HERE WAS BROKEN, which is the real shape of the row: the
/// predicate declined on the RHS's SYNTAX, so every root spelling and both
/// container legs went down with it. Measured against `origin/main` with a
/// named-tree control whose marker threshold is derived from the base tree
/// rather than typed: the before arm prints ten bodies where eighteen are
/// due, losing `d11 d21 d31 d41 d51 d61 d62 d71 d81` — one per
/// displacement — identically on the JIT, `-O0` and `-O2`.
///
/// `one` is the row's own cell and `two` the `Array` leg, the two the row
/// reported. `three` is field-rooted and `four` stores through a `mut ref`
/// container param — the two spellings the row listed as NOT MEASURED, and
/// they diverged identically, so the gap is the STORE and not the root.
///
/// `five` IS THE CELL WITH NO RELOCATION READING AVAILABLE, and it is why
/// the row's own program was not reused: the value is POPPED out of the
/// container before being stored back over another element, so `len` is
/// already down by one and the displaced element is dead beyond argument.
///
/// `six` STORES TWICE OVER THE SAME SLOT and is the double-fire guard —
/// two displacements must give exactly two bodies, not three and not one.
/// `seven` binds the RHS to a CALL RESULT rather than to a constructor,
/// the one spelling in which the declined reading ("the value already
/// existed, so this is a shuffle") was least obviously wrong; it lost its
/// body too.
///
/// THE ASSERTION IS AN EXACT-ONCE ACCOUNTING, which is the guard a
/// widening of a leak gate needs and the reason the expected string is
/// written out in full: eighteen `D` values are constructed and eighteen
/// bodies are expected, one per value. A widening that fired where it
/// should not shows up as a nineteenth. No MUST-STAY-DECLINED cell can be
/// written for this predicate — the relocation shape it was guarding does
/// not typecheck for any element type whose body is observable, per the
/// paragraph above — so the exact-once count and the two ASAN ratchet legs
/// are what stand in for one.
///
/// `eight` stores through `self.xs[0]` inside a method taking
/// `mut ref self`. It is the row's defect in one more root spelling and is
/// fixed with the rest, and it is the ONE cell in this battery where
/// `--interp` is the backend that is wrong: it prints `eight:82 d82` and
/// loses `d81`, which the hand-derived sequence puts at the store. The fix
/// is codegen-only, so that divergence predates it and is filed on its own.
#[test]
fn e2e_index_assign_identifier_rhs_destroys_displaced_element() {
    let Some(out) = run_program(
        r#"
struct D { id: i64, s: String }
impl Drop for D { fn drop(mut ref self) { println(f"d{self.id}") } }
struct Holder { xs: Vec[D] }
struct Bag { xs: Vec[D] }
impl Bag {
  fn put(mut ref self, t: D) { self.xs[0] = t; }
}
fn mk(k: i64) -> D { return D { id: k, s: f"m{k}" } }
fn one() { let mut a: Vec[D] = [D { id: 11, s: f"a" }]; let t: D = D { id: 12, s: f"b" }; a[0] = t; println(f"one:{a[0].id}") }
fn two() { let mut a: Array[D, 2] = [D { id: 21, s: f"a" }, D { id: 22, s: f"b" }]; let t: D = D { id: 23, s: f"c" }; a[0] = t; println(f"two:{a[0].id}") }
fn three() { let mut h: Holder = Holder { xs: [D { id: 31, s: f"a" }] }; let t: D = D { id: 32, s: f"b" }; h.xs[0] = t; println(f"three:{h.xs[0].id}") }
fn four(a: mut ref Vec[D]) { let t: D = D { id: 42, s: f"b" }; a[0] = t; println(f"four:{a[0].id}") }
fn five() { let mut a: Vec[D] = [D { id: 51, s: f"a" }, D { id: 52, s: f"b" }]; let t: D = a.pop().unwrap(); a[0] = t; println(f"five:{a[0].id}") }
fn six() { let mut a: Vec[D] = [D { id: 61, s: f"a" }]; let p: D = D { id: 62, s: f"b" }; let q: D = D { id: 63, s: f"c" }; a[0] = p; a[0] = q; println(f"six:{a[0].id}") }
fn seven() { let mut a: Vec[D] = [D { id: 71, s: f"a" }]; let t: D = mk(72); a[0] = t; println(f"seven:{a[0].id}") }
fn eight() { let mut b: Bag = Bag { xs: [D { id: 81, s: f"a" }] }; b.put(D { id: 82, s: f"b" }); println(f"eight:{b.xs[0].id}") }
fn main() {
  one();
  two();
  three();
  { let mut a: Vec[D] = [D { id: 41, s: f"a" }]; four(mut a); }
  five();
  six();
  seven();
  eight();
  println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        "d11\none:12\nd12\n\
             d21\ntwo:23\nd23\nd22\n\
             d31\nthree:32\nd32\n\
             d41\nfour:42\nd42\n\
             d51\nfive:52\nd52\n\
             d61\nd62\nsix:63\nd63\n\
             d71\nseven:72\nd72\n\
             d81\neight:82\nd82\n\
             end\n"
    );
}

/// B-2026-08-01-22 leg a — a FIELD-ROOTED index-assign
/// (`h.xs[i] = Res { .. }`) displaces the old element: its Drop bodies
/// fire and its field buffers free before the store (pre-fix: bodies
/// silent on both backends, buffers leaked under karac build). The
/// NEW element's body at h's death (`drop 5 y5`) fires per leg b —
/// struct-field Vec element bodies at owner death. Twin of
/// `tests/interpreter.rs`'s
/// `test_field_rooted_index_assign_displaced_elem_bodies`; the leak is
/// pinned by `asan_field_rooted_index_assign_displaced_elem_freed`.
#[test]
fn e2e_field_rooted_index_assign_displaced_elem_bodies() {
    let Some(out) = run_program(
        "struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"drop {self.id} {self.name}\")\n\
             \x20   }\n\
             }\n\
             struct Holder { xs: Vec[Res] }\n\
             fn main() {\n\
             \x20   println(\"a\");\n\
             \x20   let mut h = Holder { xs: Vec.new() };\n\
             \x20   h.xs.push(Res { id: 9, name: f\"z{9}\" });\n\
             \x20   h.xs[0] = Res { id: 5, name: f\"y{5}\" };\n\
             \x20   println(f\"held {h.xs[0].id}\");\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "a\ndrop 9 z9\nheld 5\ndrop 5 y5\nend\n");
}

/// B-2026-08-01-21 — an INDEX-assign over a struct element with heap
/// fields (`v[i] = Res { .. }`) displaces the old element: its Drop
/// bodies fire and its field buffers free before the store (pre-fix:
/// bodies silent on both backends, field buffers leaked under karac
/// build). Twin of `tests/interpreter.rs`'s
/// `test_index_assign_displaced_elem_bodies`; the leak itself is
/// pinned by `tests/memory_sanitizer.rs`'s
/// `asan_index_assign_displaced_elem_fields_freed`.
#[test]
fn e2e_index_assign_displaced_elem_bodies() {
    let Some(out) = run_program(
        "struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"drop {self.id} {self.name}\")\n\
             \x20   }\n\
             }\n\
             fn main() {\n\
             \x20   println(\"a\");\n\
             \x20   let mut v: Vec[Res] = Vec.new();\n\
             \x20   v.push(Res { id: 9, name: f\"z{9}\" });\n\
             \x20   v[0] = Res { id: 5, name: f\"y{5}\" };\n\
             \x20   println(f\"held {v[0].id}\");\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "a\ndrop 9 z9\nheld 5\ndrop 5 y5\nend\n");
}

/// B-2026-08-12-22 — the INCOMING side of that same store, and a DOUBLE
/// FREE rather than the leak its sibling guards. `ps[0] = b` for a named
/// `b` whose type is a struct with a heap field moved `b`'s field pointers
/// into the element slot while `b`'s own drop stayed armed, so the
/// container's element drain and `b` freed the same buffer:
/// `free(): double free detected in tcache 2` under `karac build`, a
/// `ptr::copy_nonoverlapping` UB panic under the JIT, correct output from
/// the interpreter.
///
/// This is the OBSERVABLE half — the swap at the heart of any hand-written
/// sort, checked against the interpreter oracle. The memory half is
/// `tests/memory_sanitizer.rs`'s
/// `asan_index_assign_named_struct_source_freed_once`, which carries the
/// full source matrix.
///
/// Every string here is an f-STRING on purpose: a string LITERAL has
/// `cap` 0, so the second free is a guarded no-op and the shape looks
/// clean. Three of the filing's "safe" boundary rows were literal-valued
/// and abort once the field is genuinely allocated.
#[test]
fn e2e_index_assign_named_struct_source_swap() {
    let Some(out) = run_program(
        "struct Pair { word: String, n: i64 }\n\
             fn main() {\n\
             \x20   let k = 1;\n\
             \x20   let mut ps: Vec[Pair] = Vec.new();\n\
             \x20   ps.push(Pair { word: f\"alpha{k}\", n: 1 });\n\
             \x20   ps.push(Pair { word: f\"beta{k}\", n: 2 });\n\
             \x20   ps.swap(0, 1);\n\
             \x20   println(ps[0].word + \" \" + ps[1].word);\n\
             \x20   let c = Pair { word: f\"gamma{k}\", n: 3 };\n\
             \x20   ps[0] = c;\n\
             \x20   println(ps[0].word + \" \" + ps[1].word);\n\
             }\n",
    ) else {
        return;
    };
    // Matches `karac run --interp` on the identical source.
    assert_eq!(out, "beta1 alpha1\ngamma1 alpha1\n");
}

/// B-2026-08-26-31 — the BODIES half of the move B-2026-08-12-22 fixed the
/// MEMORY half of. `zero_struct_move_caps` disarmed the moved-from source's
/// heap cleanup, but its `UserDrop` action stayed armed, so a local moved
/// into a container's element slot (`b.xs[j] = t`) still ran its `impl Drop`
/// BODY a second time at the binding's live-range end. Two live values here,
/// so exactly two bodies; the third `D1` was the bug.
///
/// Because the memory half was already correct, ASAN stayed silent on this
/// shape — only the observable body count showed it. The sanitizer fixtures
/// in `tests/memory_sanitizer.rs`
/// (`asan_local_moved_into_struct_elem_slot_is_not_double_freed`,
/// `asan_swap_rotation_over_heap_bearing_elements_is_clean`) keep that half
/// from regressing while this one pins the bodies.
///
/// DELIBERATELY NOT AN A/B ORACLE — do not "fix" one backend to match the
/// other in isolation. The interpreter still prints an extra `D2` for the
/// element that `b.xs[0] = b.xs[1]` displaces, because it treats an element
/// READ as clone-then-destroy while codegen's `store_destroys_displaced`
/// treats the same store as a RELOCATION. That divergence is the open
/// remainder of B-2026-08-26-21, and it is a model question design.md does
/// not yet answer (what does `let t = v[i]` mean for a non-`Copy` element —
/// move, clone, or error?). Pinning codegen's side here keeps the SETTLED
/// half from regressing; when the model question is decided, this
/// expectation and the interpreter's must be re-derived together.
/// B-2026-08-26-21 — `Vec.swap` is the SANCTIONED spelling for an element
/// exchange, and this pins the guarantee that makes it sanctioned: neither
/// value is destroyed, so exactly one `Drop` body runs per element, at the
/// container's death, and NONE during the swap itself.
///
/// This is the shape the row was really about. The hand-written spelling
/// (`let t = xs[i]; xs[i] = xs[j]; xs[j] = t`) is the one design.md §
/// "The index operator (`expr[i]`)" now rejects for non-`Copy` `T`, because
/// the two backends disagreed on what it meant. `swap` has one meaning and
/// both backends now agree on it.
///
/// Unlike its sibling above, this one IS an A/B oracle: `karac run` and
/// `karac build` produce identical output, verified for this exact fixture
/// under the interpreter, the default (auto-par) build, and
/// `KARAC_AUTO_PAR=0`.
///
/// Codegen had no `swap` arm at all before this — `karac build` failed with
/// "Vec/String method 'swap' is not yet supported in codegen" on a program
/// the interpreter ran correctly, so the method the rejection points people
/// toward was itself a run-vs-build divergence.
/// B-2026-08-26-36 — the codegen half of the `ref` binding: `let r = ref
/// v[i]` compiles to a POINTER into the container's buffer, so a read
/// through `r` after the container changes sees the new value.
///
/// This is an A/B oracle by construction: the interpreter's
/// `ref_binding_is_a_live_alias_not_a_snapshot` asserts the identical
/// sequence via `Value::ElemRef`. The two must not drift — a copy on
/// either side reintroduces the run-vs-build divergence of
/// B-2026-08-26-21, which is the whole reason the borrow aliases rather
/// than snapshots.
/// B-2026-08-26-36 — INDEXING THROUGH a `ref` binding: `let r = ref vv[i]`
/// over a `Vec[Vec[T]]`, then `r[j]`.
///
/// Two registrations were missing and BOTH were needed. The value-based
/// index path (`inline_index_recv_vec_te` → `compile_inline_temp_vec_index_ex`)
/// claimed the shim identifier, but a `ref` binding's name loads a POINTER
/// rather than the `{ptr, len, cap}` struct that path materialises — and
/// that path may free its temporary, which a borrow must never be. With it
/// excluded, the borrow then sailed PAST the named-Vec path too, because
/// that one gates on `vec_elem_types.contains_key(name)` and the shim had
/// never registered the inner element type; it landed in the generic tail,
/// which handles only Array/Vector LLVM types.
///
/// Both failure modes reported the same "Index operator applied to
/// non-array type" while `--interp` read the element correctly, so the
/// message alone could not tell them apart — the second was found by
/// tagging all three emit sites and re-running, not by inspection.
#[test]
fn test_e2e_index_through_a_ref_binding() {
    let Some(out) = run_program(
        r#"
fn main() {
    let mut vv: Vec[Vec[i64]] = Vec.new();
    let mut a: Vec[i64] = Vec.new();
    a.push(10);
    a.push(20);
    vv.push(a);
    let r = ref vv[0];
    println(f"{r[1]}");
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "20\n", "index through a borrow; got: {out:?}");
}

/// B-2026-08-01-30 leg B — a COMPUTED pure-scalar index (`v[base - 1] =
/// <new>`) takes the same displaced-element fire as the -21 literal /
/// identifier shapes. The typechecker desugars `base - 1` into
/// `i64.sub(base, 1)` before either backend sees the AST, so the purity
/// gates accept exactly the primitive arithmetic intrinsics over pure
/// operands (pre-fix: both backends silently skipped the bodies AND
/// karac build leaked the displaced element's field buffers). Twin of
/// `tests/interpreter.rs`'s
/// `test_computed_index_assign_displaced_elem_bodies`.
#[test]
fn e2e_computed_index_assign_displaced_elem_bodies() {
    let Some(out) = run_program(
        "struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"drop {self.id} {self.name}\")\n\
             \x20   }\n\
             }\n\
             fn main() {\n\
             \x20   println(\"a\");\n\
             \x20   let mut v: Vec[Res] = Vec.new();\n\
             \x20   v.push(Res { id: 9, name: f\"z{9}\" });\n\
             \x20   v.push(Res { id: 8, name: f\"w{8}\" });\n\
             \x20   let base = 1;\n\
             \x20   v[base - 1] = Res { id: 5, name: f\"y{5}\" };\n\
             \x20   println(f\"held {v[0].id}\");\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "a\ndrop 9 z9\nheld 5\ndrop 5 y5\ndrop 8 w8\nend\n");
}

/// B-2026-08-02-15 — an indexed field store whose INDEX is not a bare
/// identifier / int literal (`v[f()].field = x`) was silently dropped on
/// every codegen surface, and the subscript was never evaluated at all, so
/// an observable side effect inside it vanished along with the write.
/// `field_chain_place_ptr`'s Index arm declines a non-pure subscript by
/// design (`vec_index_elem_ptr` re-evaluates it) and
/// `field_rooted_index_place_ptr` declines a bare-identifier container, so
/// both resolvers returned None and the store exited through
/// `compile_field_store`'s no-op tail — the same tail as B-2026-08-01-35 and
/// B-2026-07-13-10, uncovered cell: identifier-rooted container x non-pure
/// index. The index is now materialized ONCE into a temp and the resolvers
/// retried against it, so the write lands and the call runs exactly once.
///
/// The interpreter evaluates the subscript TWICE for this shape (a separate
/// open defect, the second leg of the entry), so this pins the codegen
/// surfaces only — `eval` must appear exactly once per store.
#[test]
fn e2e_indexed_field_store_non_pure_index_runs_once() {
    let Some(out) = run_program(
        "struct P { mut id: i64, mut name: String }\n\
             struct O { mut hs: Vec[P] }\n\
             fn idx() -> i64 {\n\
             \x20   println(\"eval\");\n\
             \x20   return 0;\n\
             }\n\
             fn main() {\n\
             \x20   let mut v: Vec[P] = Vec.new();\n\
             \x20   v.push(P { id: 1, name: \"a\" });\n\
             \x20   v[idx()].id = 9;\n\
             \x20   println(f\"v={v[0].id}\");\n\
             \x20   let mut o = O { hs: Vec.new() };\n\
             \x20   o.hs.push(P { id: 1, name: \"a\" });\n\
             \x20   o.hs[idx()].name = f\"b\";\n\
             \x20   println(f\"o={o.hs[0].name}\");\n\
             }\n",
    ) else {
        return;
    };
    // One `eval` per store — never zero (dropped) and never two (re-evaluated).
    assert_eq!(out, "eval\nv=9\neval\no=b\n");
}

/// B-2026-08-02-5 — tuple-element assignment targets (`t.0 = v`,
/// `t.0.f = v`, `o.t.0 = v`, `o.t.0.f = v`) were accepted by every
/// checking phase but unimplemented on both backends: direct
/// TupleIndex targets ICE'd the interpreter (unreachable panic) and
/// karac build silently dropped every shape (stale reads, no
/// diagnostic). The Assign arm now dispatches TupleIndex targets
/// through `compile_tuple_index_store` (place-chain GEP + displaced
/// old-element drop + width-coerced store), and the chain machinery
/// admits tuple links. Twin of `tests/interpreter.rs`'s
/// `test_tuple_index_assignment_targets`.
#[test]
fn e2e_tuple_index_assignment_targets() {
    let Some(out) = run_program(
        "struct Pu { id: i64, name: String }\n\
             struct Ow { t: (Pu, i64) }\n\
             fn main() {\n\
             \x20   let mut t = (1, f\"a{2}\");\n\
             \x20   t.0 = 5;\n\
             \x20   println(f\"t0 {t.0}\");\n\
             \x20   let mut u = (Pu { id: 9, name: f\"z{9}\" }, 3);\n\
             \x20   u.0.id = 6;\n\
             \x20   println(f\"u {u.0.id} {u.0.name}\");\n\
             \x20   let mut o = Ow { t: (Pu { id: 9, name: f\"z{9}\" }, 3) };\n\
             \x20   o.t.0 = Pu { id: 7, name: f\"y{5}\" };\n\
             \x20   o.t.0.id = 8;\n\
             \x20   println(f\"o {o.t.0.id} {o.t.0.name}\");\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "t0 5\nu 6 z9\no 8 y5\nend\n");
}

/// B-2026-08-02-8 — tuple-element COMPOUND assignment (`t.0 += 5`) was
/// silently dropped under karac build (the CompoundAssign place-store
/// match had no TupleIndex arm; the B-2026-08-02-5 fix covered plain
/// `=` only), and `v[0].0 = 5` (tuple element of a Vec element)
/// loud-bailed because `place_chain_aggregate_llvm_type` had no Index
/// arm. Twin of `tests/interpreter.rs`'s
/// `test_tuple_index_compound_and_vec_elem_targets`.
#[test]
fn e2e_tuple_index_compound_and_vec_elem_targets() {
    let Some(out) = run_program(
        "fn main() {\n\
             \x20   let mut t = (10, f\"a{2}\");\n\
             \x20   t.0 += 5;\n\
             \x20   println(f\"t0 {t.0}\");\n\
             \x20   let mut v: Vec[(i64, String)] = Vec.new();\n\
             \x20   v.push((9, f\"z{9}\"));\n\
             \x20   v[0].0 = 5;\n\
             \x20   println(f\"held {v[0].0} {v[0].1}\");\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "t0 15\nheld 5 z9\nend\n");
}

/// B-2026-09-16-2 — an index-assign runs the DISPLACED element's `Drop`
/// body when the element is a tuple, a nested array, or a nested
/// `Vec`/`VecDeque`.
///
/// `a[0] = <new>` over `Array[(D, i64), N]`, `Array[Array[D, 1], M]` or
/// `Vec[Vec[D]]` printed nothing for the displaced value on all four
/// surfaces, where the flat `Array[D, N]` beside them prints correctly.
/// B-2026-09-15-31/-32 closed the MEMORY half of these shapes and
/// deliberately left the bodies, because the interpreter declined too and
/// firing one side alone converts an agreed gap into a divergence.
///
/// THE INVARIANT IS SCOPED, NOT RELAXED. The interpreter's
/// `value_runs_user_drop` classifies a bare Tuple/Array as false at top
/// level to keep "the dedicated container walkers the sole firers FOR
/// DIRECT BINDINGS" — and a displacement is not a direct binding. That
/// invariant has a premise: a container walker comes later and is the
/// firer. At a displacement the slot is overwritten, so no scope-exit walk
/// ever visits the old value (the same observation that made
/// B-2026-09-15-20 a real bug), and design.md line 866 puts the body at
/// the value's live-range end. A direct binding of a bare Tuple/Array
/// still classifies false.
///
/// SIX SITES, and the two that made the first attempts INERT are the
/// interesting ones:
///
///   * `store_destroys_displaced`, the destroy-vs-relocate discriminator
///     that feeds `run_bodies`, listed only `StructLiteral | Call |
///     MethodCall`. A tuple literal and an array literal — this row's two
///     shapes — answered RELOCATE, so widening codegen's shape arms
///     measured as NO behaviour change at all: the arms were reached and
///     their memory half ran (which is why -15-31/-15-32's fixtures pass),
///     but bodies were gated off upstream.
///   * and `ArrayLiteral` was not enough, because `lowering.rs`
///     canonicalizes a `Vec`-typed array literal into
///     `PrefixCollectionLiteral` (B-2026-09-15-22's arm: codegen's array
///     literal emits a fixed `[N x T]` aggregate where a Vec needs a
///     `{ptr,len,cap}` handle). So two RHSs that are the SAME literal in
///     source answered differently — `Vec[(D, i64)]` true, `Vec[Vec[D]]`
///     false. Found by probing `run_bodies` at the call site; reading the
///     dispatch chain had produced two confident wrong answers first.
///
///   The other four: codegen's tuple / nested-array / `Vec`-element arms
///   of `emit_displaced_index_elem_drop`, and the interpreter's TWO
///   index-assign displacement blocks (field-rooted and identifier-rooted,
///   which is also how the row's unmeasured `h.xs[0] = ..` question got
///   answered).
///
/// The `Vec`-element arm is BODIES ONLY, measured rather than assumed:
/// valgrind on `Vec[Vec[D]]` with `v[0] = [mkd(3)]` at `-O0` reports 17
/// allocs / 17 frees and 0 errors before the change, so the displaced
/// inner buffer is already freed on another channel and a memory call
/// there would double-free it.
///
/// RELOCATION is the hazard this must not step into —
/// B-2026-08-26-21's five-bodies-for-two-values. Two things keep it out,
/// both measured: the caller's `expr_mentions_name_deep` alias guard
/// returns before any shape arm, and `a[i] = a[j]` on a non-`Copy` element
/// does not typecheck at all (`E_INDEX_MOVE_NON_COPY`, whose diagnostic
/// points at `v.swap(i, j)`). `v.swap(0, 1)` gives exactly two bodies for
/// two values, unchanged, and is a cell below.
#[test]
fn e2e_index_store_runs_the_displaced_aggregate_elements_drop_body() {
    const HDR: &str = "struct D { s: String, id: i64 }\n\
                           impl Drop for D { fn drop(mut ref self) { println(f\"dD{self.id}\") } }\n\
                           fn mkd(n: i64) -> D { return D { s: f\"heap-{n}\", id: n }; }\n";
    for (label, body, want) in [
        (
            "the row's first cell — a TUPLE element",
            "let mut a: Array[(D, i64), 2] = [(mkd(1), 10), (mkd(2), 20)];\n\
                 a[0] = (mkd(3), 30);",
            "dD1\ndD3\ndD2\nmid\n",
        ),
        (
            "the row's second cell — a NESTED ARRAY element",
            "let mut a: Array[Array[D, 1], 2] = [[mkd(1)], [mkd(2)]];\n\
                 a[0] = [mkd(3)];",
            "dD1\ndD3\ndD2\nmid\n",
        ),
        (
            // Not in the row as filed: reached by the interpreter's new arm
            // the moment it landed (a `Vec` element is a `Value::Array` at
            // runtime exactly as a fixed array is), so it had to close in
            // the same commit or be a fresh divergence.
            "a nested Vec element — the divergence this fix would otherwise have created",
            "let mut v: Vec[Vec[D]] = [[mkd(1)], [mkd(2)]];\n\
                 v[0] = [mkd(3)];",
            "dD1\ndD3\ndD2\nmid\n",
        ),
        (
            "the Vec container leg with a tuple element",
            "let mut v: Vec[(D, i64)] = [(mkd(1), 10), (mkd(2), 20)];\n\
                 v[0] = (mkd(3), 30);",
            "dD1\ndD3\ndD2\nmid\n",
        ),
        (
            // The row lists the field-rooted spelling as NOT MEASURED.
            // Both interpreter displacement blocks are structurally
            // identical, so answering it cost nothing.
            "the FIELD-rooted spelling (the row's unmeasured question)",
            "let mut h: Hh = Hh { xs: [(mkd(1), 10), (mkd(2), 20)] };\n\
                 h.xs[0] = (mkd(3), 30);",
            "dD1\ndD3\ndD2\nmid\n",
        ),
        (
            "control: the flat named-struct element, correct since B-2026-09-14-29",
            "let mut a: Array[D, 2] = [mkd(1), mkd(2)];\n\
                 a[0] = mkd(3);",
            "dD1\ndD3\ndD2\nmid\n",
        ),
        (
            // RELOCATION, via the idiom the typechecker actually offers.
            // Exactly two bodies for two values — B-2026-08-26-21's
            // five-for-two is what a regression here would look like.
            "control: relocation via v.swap — two bodies for two values, not more",
            "let mut v: Vec[D] = [mkd(1), mkd(2)];\n\
                 v.swap(0, 1);",
            "dD2\ndD1\nmid\n",
        ),
        (
            "control: the same for a tuple element",
            "let mut v: Vec[(D, i64)] = [(mkd(1), 10), (mkd(2), 20)];\n\
                 v.swap(0, 1);",
            "dD2\ndD1\nmid\n",
        ),
        (
            "control: a non-Drop tuple element runs nothing",
            "let mut a: Array[(i64, i64), 2] = [(1, 10), (2, 20)];\n\
                 a[0] = (3, 30);\n\
                 println(f\"v:{a[0].0}\");",
            "v:3\nmid\n",
        ),
    ] {
        let src = format!(
                "{HDR}struct Hh {{ xs: Array[(D, i64), 2] }}\nfn main() {{\n{body}\nprintln(\"mid\");\n}}\n"
            );
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

#[test]
fn test_e2e_direct_index_match_option_shared_correct() {
    // B-2026-07-12-21 correctness pin (the ASAN/leak gate lives in
    // tests/memory_sanitizer.rs::asan_direct_index_match_option_shared_no_leak).
    // A direct `match vec[i]` on an `Option[shared]` index-read leaked the
    // extracted node; the lowering fix rewrites it into a let-bound
    // scrutinee. This pins that the rewrite preserves the value — the bound
    // arm must still read the right node's field. Non-ASAN so it guards the
    // value everywhere.
    let out = run_program(
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
    println(t);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "2000");
    }
}

#[test]
fn test_e2e_direct_index_match_result_shared_correct() {
    // B-2026-07-12-24 correctness pin — the index-read (`match v[i]`)
    // `Result[shared]` case (sibling of the Option B-21 index fix). Pins
    // that the bound arm reads the right node's field after the rewrite +
    // rc release.
    let out = run_program(
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
    println(t);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "2000");
    }
}

/// A read-only, non-escaping `let r = out[j]` over a `Vec[Vec[i64]]` whose
/// container is not mutated in scope binds `r` as a BORROW of the element
/// (no deep clone) — the read loop's index clone is elided. Only the
/// build-loop `out.push(b.clone())` clone remains in `@main` (without the
/// elision there would be two). B-2026-06-19-6.
#[test]
fn borrow_elision_elides_read_only_vecvec_index_binding() {
    let ir = ir_for(
        "fn main() {\n\
             let mut out: Vec[Vec[i64]] = Vec.new();\n\
             let mut b: Vec[i64] = Vec.new(); b.push(1i64);\n\
             let mut k = 0i64; while k < 4i64 { out.push(b.clone()); k = k + 1i64; }\n\
             let mut acc = 0i64; let m = out.len(); let mut j = 0i64;\n\
             while j < m { let r = out[j]; let mut i = 0i64; let rl = r.len();\n\
                 while i < rl { acc = acc + r[i]; i = i + 1i64; } j = j + 1i64; }\n\
             println(f\"{acc}\");\n\
             }",
    );
    assert_eq!(
        main_vec_clone_calls(&ir),
        1,
        "read-only let r = out[j] should be borrow-elided; only build-loop clone remains"
    );
}

/// Adversarial NEGATIVES: each pattern would be a use-after-free if `r`
/// borrowed the element, so each MUST retain the deep clone (≥ the build-loop
/// clone PLUS the index clone). B-2026-06-19-6 — the gate is whitelist-only,
/// so anything it doesn't prove read-only/non-escaping/container-stable falls
/// back to cloning.
#[test]
fn borrow_elision_keeps_clone_for_unsafe_index_bindings() {
    // (a) element overwritten while `r` live (frees the old buffer).
    let overwrite = ir_for(
        "fn main() {\n\
             let mut out: Vec[Vec[i64]] = Vec.new();\n\
             let mut b: Vec[i64] = Vec.new(); b.push(1i64);\n\
             let mut k = 0i64; while k < 4i64 { out.push(b.clone()); k = k + 1i64; }\n\
             let mut acc = 0i64; let m = out.len(); let mut j = 0i64;\n\
             while j < m { let r = out[j];\n\
                 let mut nb: Vec[i64] = Vec.new(); nb.push(9i64); out[j] = nb;\n\
                 acc = acc + r[0i64]; j = j + 1i64; }\n\
             println(f\"{acc}\");\n\
             }",
    );
    // (b) `r` moved into another container (escapes).
    let escape = ir_for(
            "fn main() {\n\
             let mut out: Vec[Vec[i64]] = Vec.new();\n\
             let mut b: Vec[i64] = Vec.new(); b.push(1i64);\n\
             let mut k = 0i64; while k < 4i64 { out.push(b.clone()); k = k + 1i64; }\n\
             let mut keep: Vec[Vec[i64]] = Vec.new();\n\
             let mut j = 0i64; while j < out.len() { let r = out[j]; keep.push(r); j = j + 1i64; }\n\
             println(f\"{keep.len()}\");\n\
             }",
        );
    // (c) container grown (realloc) while `r` live.
    let grow = ir_for(
            "fn main() {\n\
             let mut out: Vec[Vec[i64]] = Vec.new();\n\
             let mut b: Vec[i64] = Vec.new(); b.push(1i64);\n\
             let mut k = 0i64; while k < 4i64 { out.push(b.clone()); k = k + 1i64; }\n\
             let mut acc = 0i64; let mut j = 0i64;\n\
             while j < 4i64 { let r = out[j]; out.push(b.clone()); acc = acc + r[0i64]; j = j + 1i64; }\n\
             println(f\"{acc}\");\n\
             }",
        );
    // (d) `r` passed to a function (own-mode arg → escapes).
    let callarg = ir_for(
        "fn sink(v: Vec[i64]) -> i64 { v.len() }\n\
             fn main() {\n\
             let mut out: Vec[Vec[i64]] = Vec.new();\n\
             let mut b: Vec[i64] = Vec.new(); b.push(1i64);\n\
             let mut k = 0i64; while k < 4i64 { out.push(b.clone()); k = k + 1i64; }\n\
             let mut acc = 0i64; let mut j = 0i64;\n\
             while j < out.len() { let r = out[j]; acc = acc + sink(r); j = j + 1i64; }\n\
             println(f\"{acc}\");\n\
             }",
    );
    for (label, ir) in [
        ("overwrite", &overwrite),
        ("escape", &escape),
        ("grow", &grow),
        ("callarg", &callarg),
    ] {
        assert!(
                main_vec_clone_calls(ir) >= 2,
                "negative '{label}' MUST keep the index clone (borrowing would UAF); got {} clone calls",
                main_vec_clone_calls(ir)
            );
    }
}

#[test]
fn e2e_index_store_heap_vec_element_no_double_free() {
    // Single overwrite: out[0] becomes [99]; read it back.
    if let Some(out) = run_program(
        "fn main() {\n\
             let mut out: Vec[Vec[i64]] = Vec.new();\n\
             let mut b: Vec[i64] = Vec.new(); b.push(5i64);\n\
             let mut k = 0i64; while k < 4i64 { out.push(b.clone()); k = k + 1i64; }\n\
             let mut nb: Vec[i64] = Vec.new(); nb.push(99i64);\n\
             out[0i64] = nb;\n\
             println(f\"{out[0i64][0i64]}\");\n\
             }",
    ) {
        assert_eq!(out, "99\n");
    }
    // Loop of overwrites: each out[j] becomes [j]; sum the heads. Exercises
    // the old-element drop (no leak) and source-cleanup suppression (no
    // double-free) on every iteration. Sum_{j=0}^{99} j = 4950.
    if let Some(out) = run_program(
        "fn main() {\n\
             let mut out: Vec[Vec[i64]] = Vec.new();\n\
             let mut b: Vec[i64] = Vec.new(); b.push(0i64);\n\
             let mut k = 0i64; while k < 100i64 { out.push(b.clone()); k = k + 1i64; }\n\
             let mut acc = 0i64; let mut j = 0i64;\n\
             while j < 100i64 {\n\
                 let mut nb: Vec[i64] = Vec.new(); nb.push(j);\n\
                 out[j] = nb;\n\
                 acc = acc + out[j][0i64];\n\
                 j = j + 1i64;\n\
             }\n\
             println(f\"{acc}\");\n\
             }",
    ) {
        assert_eq!(out, "4950\n");
    }
}

/// B-2026-07-11-32: the classic in-place index-swap idiom over a NON-COPY
/// element (`let t = v[i]; v[i] = v[j]; v[j] = t;` on `Vec[String]`) was a
/// silent double-free — an index-read in ASSIGNMENT-RHS position only loaded
/// the `{ptr,len,cap}` header (unlike the Let arm, which deep-clones), so the
/// destination and the source element co-owned the buffer and freed it twice
/// at scope exit. The output was correct (values read before the free), which
/// is why a native run printed the right answer THEN aborted `free(): double
/// free detected`. Now the assign-RHS index-read deep-clones (bare-`v[i]` and
/// field-rooted `h.xs[i]` alike), a named-binding RHS is move-suppressed into
/// the field-rooted slot, and an f-string temporary stored into a projection
/// place has its accumulator cap zeroed. Verifies the functional result over
/// both a bare `Vec[String]` swap and a struct-field `Vec[String]` swapped
/// through a `mut ref self` method; the memory-safety (no double-free / no
/// leak) is pinned in `tests/memory_sanitizer.rs::asan_index_swap_*`.
#[test]
fn e2e_index_swap_noncopy_element() {
    if let Some(out) = run_program(
        "struct H { xs: Vec[String] }\n\
             impl H {\n\
             \x20   fn swap(mut ref self, i: i64, j: i64) {\n\
             \x20       self.xs.swap(i, j);\n\
             \x20   }\n\
             }\n\
             fn main() {\n\
             \x20   let mut v: Vec[String] = [f\"alpha\", f\"bravo\", f\"charlie\"];\n\
             \x20   v.swap(0, 1);\n\
             \x20   println(v[0]); println(v[1]); println(v[2]);\n\
             \x20   let mut h = H { xs: [f\"one\", f\"two\", f\"three\"] };\n\
             \x20   h.swap(0, 2);\n\
             \x20   println(h.xs[0]); println(h.xs[1]); println(h.xs[2]);\n\
             }",
    ) {
        assert_eq!(out, "bravo\nalpha\ncharlie\nthree\ntwo\none\n");
    }
}

/// B-2026-07-11-35 (read-resolution layer): a field-rooted index READ of a
/// GENERIC container's non-Copy element (`h.xs[i]` where `h: H[String]`,
/// `xs: Vec[T]`) used to mis-resolve the element to the i64 unknown-name
/// DEFAULT — `lower_field_access_ptr` took the field's declared `Vec[T]`
/// verbatim, so codegen read an 8-byte scalar off a 24-byte {ptr,len,cap}
/// and printed garbage. It now resolves the element to the container's
/// concrete instantiation (via `resolve_generic_field_te`, sourced from the
/// variable's recorded `H[String]` instantiation with the active monomorph
/// subst as fallback), so the read has the correct `String` stride. Covers a
/// String and an f64 element through a direct field read; `H[i64]` is the
/// unchanged (i64-is-the-default) baseline. NOTE: this is only the READ leg —
/// the generic PUSH / owned-`T`-param ownership layers of B-2026-07-11-35
/// remain open (see the ledger), so this test builds the container by struct
/// LITERAL, not by generic `push`.
#[test]
fn e2e_generic_container_field_index_read_resolves_element() {
    if let Some(out) = run_program(
        "struct H[T] { xs: Vec[T] }\n\
             fn main() {\n\
             \x20   let hs: H[String] = H { xs: [f\"hello\", f\"world\"] };\n\
             \x20   println(hs.xs[0]); println(hs.xs[1]);\n\
             \x20   let hf: H[f64] = H { xs: [1.5, 2.5] };\n\
             \x20   println(f\"{hf.xs[1]}\");\n\
             \x20   let hi: H[i64] = H { xs: [7, 8] };\n\
             \x20   println(f\"{hi.xs[0]}\");\n\
             }",
    ) {
        assert_eq!(out, "hello\nworld\n2.5\n7\n");
    }
}

/// B-2026-07-11-35 (return leg): a method/fn whose tail is a FIELD-rooted
/// index element (`fn get(ref self) -> String { self.xs[i] }`,
/// `fn getf(h: ref H, i) -> String { h.xs[i] }`) returned an ALIAS of the
/// container's element — a `ref self`/`ref Struct` can't move it out, so the
/// returned owned `String` and the container's element double-freed at scope
/// exit (B-31's tests escaped it by returning static string literals, cap=0).
/// The fn-tail now deep-clones a field-rooted index read (the bare-`v[i]`
/// return already produced an independent value, so only the field-rooted
/// shape needed it). A trivially-copyable element (`i64`) is unchanged (the
/// clone self-gates). The memory-safety is pinned in
/// `tests/memory_sanitizer.rs::asan_return_field_index_element_no_double_free`.
#[test]
fn e2e_return_field_index_element_clones() {
    if let Some(out) = run_program(
        "struct H { xs: Vec[String] }\n\
             impl H {\n\
             \x20   fn get(ref self, i: i64) -> String { self.xs[i] }\n\
             }\n\
             fn getf(h: ref H, i: i64) -> String { h.xs[i] }\n\
             fn main() {\n\
             \x20   let h = H { xs: [f\"alpha\", f\"bravo\", f\"charlie\"] };\n\
             \x20   println(h.get(0));\n\
             \x20   let b: String = getf(h, 1);\n\
             \x20   println(b);\n\
             \x20   println(h.xs[2]);\n\
             }",
    ) {
        assert_eq!(out, "alpha\nbravo\ncharlie\n");
    }
}

/// B-2026-07-03-9: a by-value generic `Slice[T]` param called with a Vec /
/// Array argument. `fn gfirst[T](s: Slice[T]) -> T { s[0] }; gfirst(vec)`
/// failed codegen module verification — the mono call passed the raw
/// `{ptr,i64,i64}` Vec value against the mono's `{ptr,i64}` Slice-typed
/// param, because `compile_generic_call`'s direct-call arg loop never ran
/// the `coerce_to_slice` header synthesis the non-generic path
/// (call_dispatch.rs) and the `mut Slice[T]` state-machine path already
/// used. `karac check` was clean and the interpreter correct. Covers a
/// narrow (u8) and heap (String) element, an Array argument (its own
/// coercion), and two distinct element instantiations of the same generic
/// (`gsum$i64` / `gsum$String` must stay separate monos — the S6b-1
/// element-collision guard, B-2026-07-02-41).
#[test]
fn e2e_generic_by_value_slice_param_coercion() {
    if let Some(out) = run_program(
        "fn gsum[T](s: Slice[T]) -> T { s[0] }\n\
             fn glen[T](s: Slice[T]) -> i64 { s.len() }\n\
             fn main() {\n\
             \x20   let vi: Vec[i64] = [10, 20, 30];\n\
             \x20   let vu: Vec[u8] = [255u8, 1u8];\n\
             \x20   let vs: Vec[String] = [\"alpha-long-enough-payload-string\", \"beta\"];\n\
             \x20   let arr: Array[i64, 3] = [7, 8, 9];\n\
             \x20   println(f\"{gsum(vi)}\");\n\
             \x20   let u: u8 = gsum(vu);\n\
             \x20   println(f\"{u}\");\n\
             \x20   println(f\"{glen(vs)}\");\n\
             \x20   println(f\"{gsum(arr)}\");\n\
             }",
    ) {
        // gsum(vi)=10 (i64 elem); u8 gsum(vu)=255 (narrow elem, typed
        // binding); glen(vs)=2 (String slice header ptr+len synthesized
        // correctly from the Vec); gsum(arr)=7 (Array-arg coercion). This
        // harness compiles sequentially (analysis=None); the narrow-elem
        // print-signedness under an auto-par par group is a separate open
        // bug (B-2026-07-03-21).
        assert_eq!(out, "10\n255\n2\n7\n");
    }
}

/// Regression (B-2026-07-29-35): a generic `fn f[T](s: Slice[T]) -> T` over a
/// HEAP-OWNED element returned an empty String under `karac build` while the
/// interpreter was correct — silent wrong output, no diagnostic.
///
/// `augment_subst_from_arg_elem_types` bound `T`'s LLVM *type* (so the mono's
/// signature and 3-word return were right), but nothing bound `T` by NAME, so
/// the element clone emitted `call @karac_clone_T` — a helper for a type that
/// does not exist — instead of `@karac_clone_String`. The element was never
/// deep-cloned, so the returned value shallow-aliased the container's buffer.
///
/// The elements here are built with `push("on" + "e")` ON PURPOSE. The sibling
/// test above uses a `Vec` of string LITERALS, whose elements are static
/// globals with `cap 0` — the container's drop frees nothing, so the shallow
/// alias stayed readable and the bug was invisible for exactly that shape.
/// Only genuinely heap-owned elements expose it, and the result must be used
/// INLINE: binding it to a `let` first was also correct pre-fix.
///
/// Each call gets its OWN Vec because a bare `Slice[T]` param consumes its
/// argument (B-2026-07-01-10), so reusing one Vec across two calls is a real
/// use-after-move — the ownership gate rejected the first draft of this test,
/// correctly, and only sees it at all because of B-2026-07-29-28.
#[test]
fn e2e_generic_slice_heap_elem_return_is_deep_cloned() {
    if let Some(out) = run_program(
        "fn first[T](s: Slice[T]) -> T { s[0] }\n\
             fn second[T](s: Slice[T]) -> T { s[1] }\n\
             fn main() {\n\
             \x20   let mut a: Vec[String] = Vec.new();\n\
             \x20   a.push(\"heap-owned-\" + \"alpha\");\n\
             \x20   println(f\"[{first(a)}]\");\n\
             \x20   let mut b: Vec[String] = Vec.new();\n\
             \x20   b.push(\"filler\" + \"-0\");\n\
             \x20   b.push(\"heap-owned-\" + \"beta\");\n\
             \x20   println(f\"[{second(b)}]\");\n\
             \x20   let mut ns: Vec[i64] = Vec.new();\n\
             \x20   ns.push(41i64 + 1i64);\n\
             \x20   println(f\"[{first(ns)}]\");\n\
             }",
    ) {
        assert_eq!(out, "[heap-owned-alpha]\n[heap-owned-beta]\n[42]\n");
    }
}

/// Regression (B-2026-07-29-32): a FRESH owned `Vec` rvalue passed where a
/// `Slice[T]` is expected. Pre-fix `coerce_to_slice` understood only a named
/// local, a `ref` param, a range index and a bare literal, so a call result
/// reached the call as a raw 3-word `{ptr,len,cap}` against a 2-word
/// `{ptr,len}` param and LLVM module verification aborted the build —
/// `karac check` passed and `karac run` worked, so it was a run-vs-build
/// divergence with no user-level diagnostic.
///
/// Landing the coercion alone was not enough and is why this bug outlived
/// B-2026-07-29-35: once these arguments compiled, they arrived with `T`
/// unbound because BOTH mono substitution sources keyed on the argument being
/// a plain identifier. Binding only the name reproduced -35's
/// `karac_clone_T`; binding only the LLVM type gave `Function return type
/// does not match operand type of return inst`. The name and the type have to
/// move together, which is what the two `*_arg_elem_*` helpers ensure.
///
/// Covers all four shapes the fix repairs: generic and concrete params, a
/// `.clone()` and a `Vec`-returning call as the fresh argument, and a scalar
/// element (the case that masked the original bug, since an unbound `T`
/// defaults to `i64` and matched by luck).
#[test]
fn e2e_fresh_vec_rvalue_coerces_to_slice_param() {
    if let Some(out) = run_program(
        "fn first[T](s: Slice[T]) -> T { s[0] }\n\
             fn firstc(s: Slice[String]) -> String { s[0] }\n\
             fn make() -> Vec[String] { [\"mk-alpha\", \"mk-beta\"] }\n\
             fn main() {\n\
             \x20   let vs: Vec[String] = [\"own-\" + \"a\", \"own-\" + \"b\"];\n\
             \x20   println(f\"[{first(vs.clone())}]\");\n\
             \x20   println(f\"[{firstc(vs.clone())}]\");\n\
             \x20   println(f\"[{first(make())}]\");\n\
             \x20   let ns: Vec[i64] = [7i64, 8i64];\n\
             \x20   println(f\"[{first(ns.clone())}]\");\n\
             }",
    ) {
        assert_eq!(out, "[own-a]\n[own-a]\n[mk-alpha]\n[7]\n");
    }
}

/// B-2026-08-21-24 — a Vec RVALUE argument in a `ref Slice[T]` parameter.
///
/// `coerce_to_slice` builds the `{ptr, i64}` header as a VALUE; a `ref`
/// slot declares a `ptr`. The place arms pass a pointer already, so every
/// OTHER ref-slot shape pushed the bare header into a pointer parameter and
/// LLVM rejected the module. `karac check` passed and `--interp` printed 3.
///
/// The row calls the argument an "array literal", which is worth
/// correcting here because it points at the wrong fix: `[1u8, 2u8, 3u8]`
/// parses as a `PrefixCollectionLiteral{type_name: "Vec"}`, a Vec rvalue,
/// so `arg_is_array_source` is correctly false and giving it an
/// `ArrayLiteral` arm changes nothing. What the shape actually lacks is
/// storage to borrow — hence spilling the synthesized header.
///
/// The by-value control is kept beside it: that spelling always worked, so
/// a regression that broke only the `ref` slot would otherwise look like a
/// passing test.
#[test]
fn e2e_vec_rvalue_into_a_ref_slice_param() {
    let src = "fn total(b: ref Slice[u8]) -> i64 { b.len() }\n\
                   fn total_val(b: Slice[u8]) -> i64 { b.len() }\n\
                   fn main() {\n\
                   println(total([1u8, 2u8, 3u8]));\n\
                   println(total_val([1u8, 2u8, 3u8, 4u8]));\n\
                   }";
    assert_eq!(run_program(src).as_deref(), Some("3\n4\n"));
}

/// B-2026-08-21-39 — an `Array[T, N]` argument to a `mut Slice[T]`
/// parameter of an ASSOCIATED function (`Type.f(..)`, no receiver).
///
/// That call path consulted `fn_param_ref` but never
/// `fn_param_slice_elem`, so the argument arrived as the raw `[3 x i8]`
/// aggregate against a `{ptr, i64}` slot — no header synthesized at all.
/// The free-function spelling has worked since B-2026-06-19-1 and the
/// instance-method spelling since 08f57a7; this was the last of the three.
///
/// `bm[0]` after the call is the load-bearing line: it proves the slice
/// ALIASES the array rather than borrowing a copy, which a header
/// synthesized over a spilled temporary would not.
#[test]
fn e2e_array_into_a_mut_slice_param_of_an_assoc_fn() {
    let src = "struct H { acc: i64 }\n\
                   impl H {\n\
                   fn f(b: mut Slice[u8]) -> i64 { b[0] = 9u8; b.len() }\n\
                   }\n\
                   fn free_f(b: mut Slice[u8]) -> i64 { b[0] = 8u8; b.len() }\n\
                   fn main() {\n\
                   let mut bm: Array[u8, 3] = [10u8, 20u8, 30u8];\n\
                   println(H.f(mut bm));\n\
                   println(bm[0]);\n\
                   let mut cm: Array[u8, 3] = [10u8, 20u8, 30u8];\n\
                   println(free_f(mut cm));\n\
                   println(cm[0]);\n\
                   }";
    assert_eq!(run_program(src).as_deref(), Some("3\n9\n3\n8\n"));
}

/// Regression (B-2026-07-30-3): an `Array[T, N]` argument passed to a
/// GENERIC `ref Slice[T]` parameter. This is B-2026-06-19-1 — an Array
/// binding's storage is its raw elements with no `{ptr,len}` header — left
/// open on the MONOMORPHIZED call path. That entry's fix synthesizes the
/// header for Array sources on the non-generic path (call_dispatch.rs); the
/// generic path's ref-arg arm (mono.rs) went straight to `get_data_ptr`, so
/// the callee received `&array[0]` and read `ptr = elem0, len = elem1`.
///
/// The `size` line is the load-bearing assertion, and it is a SILENT wrong
/// answer rather than a crash: pre-fix, `s.len()` over `[100, 7, 3, 4]`
/// returned **7** — literally the second element — on both `karac build`
/// and the JIT, while the interpreter returned 4. Reading elem0 as a
/// pointer then trapped (SIGTRAP for `String` elements, SIGSEGV for
/// scalars), which is why the element reads are covered too.
///
/// Element type is irrelevant to the bug (the header is wrong before any
/// element is touched), so a scalar and a heap element are both covered.
/// The three shapes that were always correct are asserted alongside as
/// controls, because they are what localizes the fault to generic + `ref` +
/// Array specifically: a CONCRETE `ref Slice[String]` param takes the
/// non-generic path that already had the fix, a BARE generic `Slice[T]`
/// param takes the by-value arm that coerces via `coerce_to_slice`, and a
/// `Vec` argument to the very same generic `ref Slice[T]` signature
/// forwards correctly because its storage starts with a `{ptr,len}` header
/// superset. That last one must keep working: the fix is deliberately
/// restricted to Array sources so the Vec/Slice forward is not re-coerced.
#[test]
fn e2e_array_arg_to_generic_ref_slice_param_gets_a_real_header() {
    if let Some(out) = run_program(
        "fn first[T](s: ref Slice[T]) -> T { s[0] }\n\
             fn size[T](s: ref Slice[T]) -> i64 { s.len() }\n\
             fn firstc(s: ref Slice[String]) -> String { s[0] }\n\
             fn firstv[T](s: Slice[T]) -> T { s[0] }\n\
             fn main() {\n\
             \x20   let ns: Array[i64, 4] = [100i64, 7i64, 3i64, 4i64];\n\
             \x20   println(f\"[{size(ns)}]\");\n\
             \x20   println(f\"[{first(ns)}]\");\n\
             \x20   let ss: Array[String, 2] = [\"lit-x\", \"lit-y\"];\n\
             \x20   println(f\"[{first(ss)}]\");\n\
             \x20   println(f\"[{firstc(ss)}]\");\n\
             \x20   println(f\"[{firstv(ss)}]\");\n\
             \x20   let vs: Vec[String] = [\"vec-x\", \"vec-y\"];\n\
             \x20   println(f\"[{first(vs)}]\");\n\
             }",
    ) {
        assert_eq!(out, "[4]\n[100]\n[lit-x]\n[lit-x]\n[lit-x]\n[vec-x]\n");
    }
}

/// Regression (B-2026-07-30-3, third defect): an Array REF PARAM forwarded
/// to a `Slice[T]` parameter, e.g. `fn via(a: ref Array[String, 2]) ->
/// String { first(a) }`.
///
/// B-2026-06-19-1 synthesizes the `{ptr,len}` header for Array sources, but
/// its gate tested only `variables[name].ty` for an LLVM array type — true
/// for an owned Array local, FALSE for an Array `ref` param, whose alloca
/// holds a `ptr`. The declared array type lives in `ref_params` instead, so
/// the forward fell through to `get_data_ptr` and the callee read
/// `ptr = elem0, len = elem1` — SIGSEGV.
///
/// This one was NOT generic-specific: it reproduced on plain `main` with a
/// CONCRETE `ref Slice[String]` callee, i.e. on the non-generic path that
/// already had -06-19-1's fix. Both callee spellings are asserted, since the
/// two call sites now share `arg_is_array_source` and either could regress.
#[test]
fn e2e_array_ref_param_forwarded_to_slice_param_gets_a_real_header() {
    if let Some(out) = run_program(
            "fn firstg[T](s: ref Slice[T]) -> T { s[0] }\n\
             fn firstc(s: ref Slice[String]) -> String { s[0] }\n\
             fn sizeg[T](s: ref Slice[T]) -> i64 { s.len() }\n\
             fn via_generic(a: ref Array[String, 3]) -> String { firstg(a) }\n\
             fn via_concrete(a: ref Array[String, 3]) -> String { firstc(a) }\n\
             fn via_len(a: ref Array[i64, 4]) -> i64 { sizeg(a) }\n\
             fn main() {\n\
             \x20   let ss: Array[String, 3] = [\"own-\" + \"x\", \"own-\" + \"y\", \"own-\" + \"z\"];\n\
             \x20   println(f\"[{via_generic(ss)}]\");\n\
             \x20   println(f\"[{via_concrete(ss)}]\");\n\
             \x20   let ns: Array[i64, 4] = [100i64, 7i64, 3i64, 4i64];\n\
             \x20   println(f\"[{via_len(ns)}]\");\n\
             }",
        ) {
            assert_eq!(out, "[own-x]\n[own-x]\n[4]\n");
        }
}

/// Regression (B-2026-07-30-18): a `ref Slice[T]` / `ref Vec[T]` PARAM
/// forwarded to a BY-VALUE `Slice[T]` parameter — `fn via(v: ref Slice[i64])
/// -> i64 { blen(v) }` with `fn blen(v: Slice[i64]) -> i64 { v.len() }`.
///
/// The sibling of B-2026-07-30-6, one level down: that one was the `ref`-ARG
/// path (the callee also takes `ref`), this one is the by-value coercion in
/// `coerce_to_slice`. Its Identifier fast-path read the payload straight out
/// of `slot.ptr`, which is the header only for an OWNED local; for a `ref`
/// param the alloca holds a POINTER to the caller's header, so the load
/// produced `{header_ptr, whatever sits next on the stack}`. Symptoms, all
/// with a green `karac check`: `.len()` returned a stack address under the
/// JIT and a constant (1073741824) under AOT; `v[1]` read the len field
/// (returning 4 for a 4-element slice) because the data pointer was the
/// header itself, an OUT-OF-BOUNDS read the JIT did not trap; a `String`
/// element printed garbage bytes. The interpreter was correct throughout.
///
/// Source is irrelevant (Array and Vec both), which is what separates this
/// from the Array-header family — the defect is in how the FORWARDING
/// binding is read, not in what it points at.
#[test]
fn e2e_ref_container_param_forwarded_to_by_value_slice_param() {
    if let Some(out) = run_program(
        "fn blen(v: Slice[i64]) -> i64 { v.len() }\n\
             fn bidx(v: Slice[i64]) -> i64 { v[1] }\n\
             fn bfirst(v: Slice[String]) -> String { v[0] }\n\
             fn via_refslice_len(v: ref Slice[i64]) -> i64 { blen(v) }\n\
             fn via_refslice_idx(v: ref Slice[i64]) -> i64 { bidx(v) }\n\
             fn via_refvec_len(v: ref Vec[i64]) -> i64 { blen(v) }\n\
             fn via_refvec_idx(v: ref Vec[i64]) -> i64 { bidx(v) }\n\
             fn via_refslice_str(v: ref Slice[String]) -> String { bfirst(v) }\n\
             fn main() {\n\
             \x20   let a: Array[i64, 4] = [100i64, 7i64, 3i64, 4i64];\n\
             \x20   println(f\"{via_refslice_len(a)}\");\n\
             \x20   println(f\"{via_refslice_idx(a)}\");\n\
             \x20   let mut v: Vec[i64] = Vec.new();\n\
             \x20   v.push(100i64); v.push(7i64); v.push(3i64); v.push(4i64);\n\
             \x20   println(f\"{via_refslice_len(v)}\");\n\
             \x20   println(f\"{via_refvec_len(v)}\");\n\
             \x20   println(f\"{via_refvec_idx(v)}\");\n\
             \x20   let mut s: Vec[String] = Vec.new();\n\
             \x20   s.push(\"own-\" + \"x\");\n\
             \x20   s.push(\"own-\" + \"y\");\n\
             \x20   println(f\"[{via_refslice_str(s)}]\");\n\
             }",
    ) {
        assert_eq!(out, "4\n7\n4\n4\n7\n[own-x]\n");
    }
}

/// `self.field[i] = v` (and the `self.field[i]` read) on a **plain** struct
/// receiver. The store path used to fall to the "Index assignment target
/// must be a variable" gate because `compile_index_store`'s FieldAccess arm
/// did not normalise `SelfValue → Identifier("self")` the way the read path
/// does. Regression-guards the store reaching the field-index helper.
#[test]
fn e2e_self_field_index_store_plain_struct() {
    if let Some(out) = run_program(
        "struct Bag { mut items: Vec[i64] }\n\
             impl Bag {\n\
                 fn push(mut ref self, v: i64) { self.items.push(v); }\n\
                 fn get(ref self, i: u64) -> i64 { self.items[i] }\n\
                 fn set(mut ref self, i: u64, v: i64) { self.items[i] = v; }\n\
             }\n\
             fn main() {\n\
                 let mut b = Bag { items: Vec.new() };\n\
                 b.push(1); b.push(2);\n\
                 b.set(0, 7);\n\
                 println(b.get(0));\n\
                 println(b.get(1));\n\
             }",
    ) {
        assert_eq!(out, "7\n2\n");
    }
}

/// `self.field[i]` read + store on a **shared** struct with an **owned**
/// `self` receiver. The handle is a single heap pointer; the field-index
/// helper resolves it via `compile_expr`, which loads once for an owned
/// binding.
#[test]
fn e2e_self_field_index_shared_struct_owned_self() {
    if let Some(out) = run_program(
        "shared struct Bag { mut items: Vec[i64] }\n\
             impl Bag {\n\
                 fn push(self, v: i64) { self.items.push(v); }\n\
                 fn get(self, i: u64) -> i64 { self.items[i] }\n\
                 fn set(self, i: u64, v: i64) { self.items[i] = v; }\n\
             }\n\
             fn main() {\n\
                 let b = Bag { items: Vec.new() };\n\
                 b.push(100); b.push(200);\n\
                 b.set(1, 222);\n\
                 println(b.get(0));\n\
                 println(b.get(1));\n\
             }",
    ) {
        assert_eq!(out, "100\n222\n");
    }
}

/// `self.field[i]` read + store on a **shared** struct with a `mut ref self`
/// receiver — the case that segfaulted (exit 133) before the fix. A shared
/// `ref self` slot holds a *pointer to* the handle, so the receiver needs a
/// **double-load** (deref the ref-param slot → handle, then handle → heap
/// struct). A single load lands one indirection short and the field GEP
/// reads a garbage `{ptr,len,cap}`. The field-index helper now resolves the
/// shared receiver via `compile_expr` (mirroring `compile_field_store`),
/// which walks the correct load chain for both owned and ref bindings.
#[test]
fn e2e_self_field_index_shared_struct_mut_ref_self() {
    if let Some(out) = run_program(
        "shared struct Bag { mut items: Vec[i64] }\n\
             impl Bag {\n\
                 fn push(mut ref self, v: i64) { self.items.push(v); }\n\
                 fn get(mut ref self, i: u64) -> i64 { self.items[i] }\n\
                 fn set(mut ref self, i: u64, v: i64) { self.items[i] = v; }\n\
             }\n\
             fn main() {\n\
                 let b = Bag { items: Vec.new() };\n\
                 b.push(10); b.push(20); b.push(30);\n\
                 b.set(1, 99);\n\
                 println(b.get(0));\n\
                 println(b.get(1));\n\
                 println(b.get(2));\n\
             }",
    ) {
        assert_eq!(out, "10\n99\n30\n");
    }
}

/// B-2026-08-14-5 — a field read through a FIXED-SIZE ARRAY index compiles,
/// at parity with the `Vec` spelling of the same read.
///
/// `arr[0].a` failed to build with codegen's own "cannot resolve field …
/// (its type was not recorded for codegen)" while `v[0].a` on a `Vec` of
/// the same struct compiled and `--interp` ran it. Two tables, one split
/// deliberately: an Array binding's element type lives in
/// `array_elem_type_exprs`, not `var_elem_type_exprs`, because ~170 readers
/// treat an entry in the latter as "this binding is a Vec/Slice/Map". So
/// the Array had to be added as a fallback READ at each resolution point,
/// not by widening the table.
///
/// FOUR SHAPES, because the row's single repro understated the gap — the
/// Vec arm has three siblings the Array path also lacked, each failing the
/// same way while its Vec twin compiled:
///
///   arr[0].f          the reported one (and with a variable index)
///   arr[0].f.g        the receiver of the SECOND field needs typing too
///   h.arr[0].f        an array held in a struct FIELD
///   sa[0].f           an array of `shared struct`, which resolves its RC
///                     handle by value rather than through the
///                     Vec/Slice-only element-pointer helpers
///
/// Every line is a loud build failure pre-fix, never a wrong answer, which
/// is why the row was medium rather than high — and why this test asserts
/// values rather than needing a sanitizer twin.
#[test]
fn test_e2e_field_read_through_an_array_index_compiles() {
    assert_eq!(
        run_program(
            "struct Plain { a: u8, n: i64 }\n\
                 struct Inner { q: i64 }\n\
                 struct Outer { i: Inner }\n\
                 struct HolderA { arr: Array[Plain, 2] }\n\
                 shared struct Sh { v: i64 }\n\
                 fn main() {\n\
                     let m = Plain { a: 200, n: 7 };\n\
                     let m2 = Plain { a: 5, n: 9 };\n\
                     let arr: Array[Plain, 2] = [m, m2];\n\
                     println(f\"{arr[0].a} {arr[0].n}\");\n\
                     let i = 1;\n\
                     println(f\"{arr[i].a} {arr[i].n}\");\n\
                     let o = Outer { i: Inner { q: 5 } };\n\
                     let oa: Array[Outer, 1] = [o];\n\
                     println(f\"{oa[0].i.q}\");\n\
                     let m3 = Plain { a: 9, n: 6 };\n\
                     let m4 = Plain { a: 4, n: 2 };\n\
                     let h = HolderA { arr: [m3, m4] };\n\
                     println(f\"{h.arr[0].a} {h.arr[1].n}\");\n\
                     let s = Sh { v: 42 };\n\
                     let sa: Array[Sh, 1] = [s];\n\
                     println(f\"{sa[0].v}\");\n\
                 }"
        )
        .as_deref(),
        Some("200 7\n5 9\n5\n9 2\n42\n"),
    );
}

#[test]
fn test_e2e_slice_read_accessors_have_codegen() {
    assert_eq!(
            run_program(
                "fn main() {\n\
                     let mut v: Vec[i64] = Vec.new();\n\
                     v.push(3); v.push(1); v.push(2);\n\
                     let s: Slice[i64] = v.as_slice();\n\
                     println(s.contains(2));\n\
                     println(s.contains(9));\n\
                     match s.first() { Some(x) => { println(x); } None => { println(-1); } }\n\
                     match s.last() { Some(x) => { println(x); } None => { println(-1); } }\n\
                     match s.get(1i64) { Some(x) => { println(x); } None => { println(-1); } }\n\
                     match s.get(9i64) { Some(x) => { println(x); } None => { println(-1); } }\n\
                     let mut w: Vec[String] = Vec.new();\n\
                     let mut a = String.new(); a.push_str(\"alpha\"); w.push(a);\n\
                     let mut b = String.new(); b.push_str(\"beta\"); w.push(b);\n\
                     let t: Slice[String] = w.as_slice();\n\
                     match t.first() { Some(x) => { println(x); } None => { println(\"none\"); } }\n\
                     match t.get(1i64) { Some(x) => { println(x); } None => { println(\"none\"); } }\n\
                     let mut cs = String.new(); cs.push_str(\"beta\");\n\
                     println(t.contains(cs));\n\
                     println(v.len());\n\
                     println(w.len());\n\
                 }"
            )
            .as_deref(),
            Some("true\nfalse\n3\n2\n1\n-1\nalpha\nbeta\ntrue\n3\n2\n"),
        );
}

#[test]
fn test_e2e_index_assign_call_rhs_carrying_container_heap_values_survive() {
    // B-2026-08-12-33's VALUE side. The displaced-element drop now fires for
    // an RHS that passes container heap into a call, which means a buffer
    // the call READ FROM is freed between the call and the store. What has
    // to hold is that the value stored is the callee's independent copy —
    // so this asserts the results, and its asan twin asserts the run is
    // clean.
    //
    // `passthru(ps[0])` is the sharpest: the element makes a full round
    // trip through the callee and back into the slot it came from, so a
    // fix that freed the wrong side prints garbage here rather than in a
    // leak report. `join(ps[0].word, ps[1].word)` mixes the two proofs in
    // one call — a cloned field read from the slot being overwritten and
    // one from a slot that must be left alone, which is what `ps[1]`
    // printing unchanged pins.
    //
    // The `ref`-param leg is a DECLINE that must stay correct: `peek` takes
    // a borrow, so no copy happens anywhere and the guard keeps refusing.
    // It still leaks (its own row); what it must not do is change value.
    assert_eq!(
        run_program(
            "#[derive(Clone)]\n\
                 struct Pair { word: String, n: i64 }\n\
                 fn passthru(p: Pair) -> Pair { p }\n\
                 fn takes(s: String) -> Pair { Pair { word: s + \"!\", n: 1 } }\n\
                 fn join(a: String, b: String) -> Pair { Pair { word: a + b, n: 2 } }\n\
                 fn peek(p: ref Pair) -> String { p.word }\n\
                 fn main() {\n\
                     let k = 1;\n\
                     let mut ps: Vec[Pair] = Vec.new();\n\
                     ps.push(Pair { word: f\"a{k}\", n: 7 });\n\
                     ps.push(Pair { word: f\"b{k}\", n: 8 });\n\
                     ps[0] = passthru(ps[0].clone());\n\
                     println(f\"{ps[0].word} {ps[0].n}\");\n\
                     ps[0] = takes(ps[0].word);\n\
                     println(f\"{ps[0].word} {ps[0].n}\");\n\
                     ps[0] = join(ps[0].word, ps[1].word);\n\
                     println(f\"{ps[0].word} {ps[1].word}\");\n\
                     ps[0] = Pair { word: peek(ps[0]), n: 9 };\n\
                     println(f\"{ps[0].word} {ps[0].n}\");\n\
                 }"
        )
        .as_deref(),
        Some("a1 7\na1! 1\na1!b1 b1\na1!b1 9\n"),
    );
}

/// B-2026-08-21-48 — the UN-ANNOTATED slice binding, sibling of
/// `test_e2e_user_trait_impl_on_slice_dispatches` below.
///
/// `let s = v[0..2]; s.f()` failed dispatch ("no handler for method 'f' on
/// variable 's'") while the ANNOTATED binding and the PARAMETER spelling of
/// the same call both worked, and `--interp` printed the right answer.
///
/// The row read this as a MISSING registration — "an un-annotated
/// `let s = v[0..2]` does not register `s` in `slice_elem_types`". It
/// does; that table was correct all along. The defect is an overwrite of a
/// DIFFERENT table: `type_name_of`'s `Index` arm ignored the index
/// expression, so a RANGE index reported the ELEMENT name ("i64"), the
/// `let` path wrote that into `var_type_names`, and it clobbered the
/// "Slice" a correct earlier registration had already recorded.
/// `inferred_receiver_type` reads `var_type_names` FIRST and only falls
/// back to `slice_elem_types`, so the wrong name shadowed the right one and
/// the fallback never ran.
///
/// Both controls are asserted beside the fixed shape, because they are what
/// makes the bug legible as annotation-sensitive rather than a missing
/// feature — and a regression that broke only the un-annotated path would
/// otherwise still look plausible.
///
/// The last two rows are the ones a careless widening breaks: a String
/// range subscript is a String view, NOT a `Slice`, and a SCALAR index
/// still has to report its element type.
#[test]
fn test_e2e_user_trait_impl_on_an_unannotated_slice_binding() {
    let prelude = "trait H { fn f(self) -> i64; }\n\
                       impl H for Slice[i64] { fn f(self) -> i64 { return self.len(); } }\n";
    assert_eq!(
        run_program(&format!(
            "{prelude}\
                 fn main() {{\n\
                 let v: Vec[i64] = Vec[5, 6, 7];\n\
                 let s = v[0..2];\n\
                 println(s.f());\n\
                 }}"
        ))
        .as_deref(),
        Some("2\n"),
        "un-annotated binding — the shape that failed dispatch"
    );
    assert_eq!(
        run_program(&format!(
            "{prelude}\
                 fn take(s: Slice[i64]) -> i64 {{ return s.f(); }}\n\
                 fn main() {{\n\
                 let v: Vec[i64] = Vec[5, 6, 7];\n\
                 let a: Slice[i64] = v[0..2];\n\
                 println(a.f());\n\
                 println(take(v[0..2]));\n\
                 }}"
        ))
        .as_deref(),
        Some("2\n2\n"),
        "annotated binding and parameter receiver — the two controls"
    );
    assert_eq!(
        run_program(
            "fn main() {\n\
                 let v: Vec[i64] = Vec[5, 6, 7];\n\
                 let s = v[0..2];\n\
                 println(s.len());\n\
                 println(s[0]);\n\
                 let x = v[1];\n\
                 println(x);\n\
                 let t: String = \"hello\";\n\
                 let w = t[1..3];\n\
                 println(w);\n\
                 }"
        )
        .as_deref(),
        Some("2\n5\n6\nel\n"),
        "builtins on the same binding, plus the SCALAR-index and STRING-range \
             shapes the range arm must not claim"
    );
}

#[test]
fn test_e2e_index_assign_elem_to_elem_swaps_correctly() {
    // B-2026-08-12-26's VALUE side. The fix frees the displaced element
    // before the store, which is a free-before-store on a slot the RHS was
    // just read from — so the thing to prove is not only that the leak is
    // gone but that the values survive.
    //
    // The one-temp swap is the shape that motivated the row (a hand-written
    // sort leaked one buffer per swap through it), and it is also the
    // sharpest ordering test: `qs[0] = qs[1]` frees slot 0's old buffer
    // while `t` still holds it, so a fix that freed the wrong side would
    // print garbage or abort here rather than merely leak.
    //
    // `ps[0] = ps[0]` is the degenerate case the guard used to decline on
    // aliasing grounds. It must survive intact — freeing the displaced
    // occupant and storing the clone of that same occupant is only safe
    // because the clone happens first.
    assert_eq!(
        run_program(
            "struct Pair { word: String, n: i64 }\n\
                 fn main() {\n\
                     let k = 1;\n\
                     let mut qs: Vec[Pair] = Vec.new();\n\
                     qs.push(Pair { word: f\"a{k}\", n: 1 });\n\
                     qs.push(Pair { word: f\"b{k}\", n: 2 });\n\
                     qs.swap(0, 1);\n\
                     println(qs[0].word + \" \" + qs[1].word);\n\
                     println(qs[0].n + qs[1].n * 10);\n\
                     let mut ps: Vec[Pair] = Vec.new();\n\
                     ps.push(Pair { word: f\"x{k}\", n: 7 });\n\
                     ps.swap(0, 0);\n\
                     println(ps[0].word);\n\
                     println(ps[0].n);\n\
                 }"
        )
        .as_deref(),
        Some("b1 a1\n12\nx1\n7\n"),
    );
}

#[test]
fn test_e2e_shared_vec_field_index_field_read_and_store() {
    // B-2026-07-13-10 — a chained field access/store through a Vec that is
    // itself a FIELD of a shared struct: `root.kids[i].val`. The
    // identifier-rooted form (`nodes[i].field`) had dedicated read/store
    // branches, but a FieldAccess-rooted index fell to the generic tails:
    // the READ returned the const-0 placeholder (`root.kids[0].val` → 0
    // instead of 2), and the STORE was silently dropped (`root.kids[0].val
    // = 99` compiled clean but never persisted, so the next read saw the
    // stale value). Both are reference-semantics-visible: the pushed element
    // is the SAME RC object as the original handle, so a write through the
    // Vec-field chain is observable via that handle. This pins read, store,
    // compound `x = x + k` store, and cross-handle aliasing on all backends.
    if let Some(out) = run_program(
        "shared struct Node { mut val: i64, mut kids: Vec[Node] }\n\
             fn main() {\n\
                 let root = Node { val: 1, kids: Vec.new() };\n\
                 let a = Node { val: 10, kids: Vec.new() };\n\
                 let b = Node { val: 20, kids: Vec.new() };\n\
                 root.kids.push(a);\n\
                 root.kids.push(b);\n\
                 println(f\"{root.kids[0].val}\");\n\
                 root.kids[0].val = root.kids[0].val + 5;\n\
                 root.kids[1].val = 99i64;\n\
                 println(f\"{root.kids[0].val}\");\n\
                 println(f\"{root.kids[1].val}\");\n\
                 println(f\"{a.val}\");\n\
                 println(f\"{b.val}\");\n\
             }",
    ) {
        // read=10; after +5 → 15; b set to 99; aliases a/b see the writes.
        assert_eq!(out.trim(), "10\n15\n99\n15\n99");
    }
}

#[test]
fn test_e2e_shared_vec_field_index_heap_field_read() {
    // B-2026-07-13-10 sibling — the element carries a heap (String) field
    // read through the Vec-field chain (`t.members[i].name`). Exercises the
    // shared GEP-deref against a 3-word field slot, not just a scalar.
    if let Some(out) = run_program(
        "shared struct Person { name: String, age: i64 }\n\
             shared struct Team { mut members: Vec[Person] }\n\
             fn main() {\n\
                 let t = Team { members: Vec.new() };\n\
                 t.members.push(Person { name: \"Ana\", age: 30 });\n\
                 t.members.push(Person { name: \"Bob\", age: 25 });\n\
                 println(t.members[0].name);\n\
                 println(f\"{t.members[1].age}\");\n\
             }",
    ) {
        assert_eq!(out.trim(), "Ana\n25");
    }
}

#[test]
fn test_e2e_index_vec_field_through_self() {
    // Regression for the self-hosting lexer index blocker: indexing a
    // `Vec` field through the `self` receiver (`self.bytes[self.current]`)
    // died with "Index operator applied to non-array type". The
    // field-access-rooted index arm resolves the receiver through
    // `lower_field_access_ptr`, whose `match` handled `Identifier` and
    // `Index` inners but not `SelfValue` — so `self.field[i]` fell through
    // to the generic tail and compiled the field to a Vec `{ptr,len,cap}`
    // VALUE rather than indexing it. The identical access through a named
    // `ref S` param (`s.field[i]`) already worked; `self` is registered
    // under the name "self" in the same per-binding registries, so the fix
    // routes `SelfValue` through the Identifier path with that name.
    //
    // Covers both `ref self` (read) and `mut ref self` (the lexer's
    // `advance`) and a `u8` element (the lexer's byte buffer).
    if let Some(out) = run_program(
        "struct Buf { bytes: Vec[u8], pos: i64 }\n\
             impl Buf {\n\
                 fn peek(ref self) -> u8 { self.bytes[self.pos] }\n\
                 fn advance(mut ref self) -> u8 {\n\
                     let c = self.bytes[self.pos];\n\
                     self.pos = self.pos + 1;\n\
                     c\n\
                 }\n\
             }\n\
             fn main() {\n\
                 let mut b: Vec[u8] = Vec.new();\n\
                 b.push(65u8); b.push(66u8); b.push(67u8);\n\
                 let mut buf = Buf { bytes: b, pos: 0 };\n\
                 println(buf.peek().to_string());\n\
                 let a = buf.advance();\n\
                 println(a.to_string());\n\
                 println(buf.peek().to_string());\n\
             }",
    ) {
        assert_eq!(out, "65\n65\n66\n");
    }
}

#[test]
fn test_e2e_self_field_vec_index_struct_element_field() {
    // #32 (phase-12 self-hosting, parser stage): a struct-element field read
    // AND an enum-field match THROUGH a `self`-field-rooted Vec index —
    // `self.toks[self.pos].off` / `match self.toks[self.pos].tok { … }`. The
    // sibling `self.bytes[i]` (a Copy scalar ELEMENT, #4 above) worked, but
    // reading a FIELD of a struct element via a `self`/FieldAccess-rooted
    // index was a SILENT MISCOMPILE: `type_name_of_expr` had no `Index` arm,
    // so `field_index_for` couldn't find the element struct's field layout
    // and `compile_field_access`'s generic tail returned the `i64 0`
    // placeholder (scalar fields read 0, an enum field mis-resolved its match
    // scrutinee). The fix adds an `Index` arm resolving the element type from
    // the indexed collection's element/field `TypeExpr` (Identifier root via
    // `var_elem_type_exprs`, FieldAccess/`self` root via the field's `Vec[E]`).
    // This is the parser's core token-access shape (`self.tokens[self.pos]…`).
    if let Some(out) = run_program(
        "enum Tk { A, Id(String), Num(i64) }\n\
             struct Sp { tok: Tk, off: i64 }\n\
             struct P { toks: Vec[Sp], pos: i64 }\n\
             impl P {\n\
                 fn off_now(ref self) -> i64 { self.toks[self.pos].off }\n\
                 fn kind_now(ref self) -> i64 {\n\
                     match self.toks[self.pos].tok { Id(_) => 1, Num(_) => 2, A => 3 }\n\
                 }\n\
                 fn take(mut ref self) -> String {\n\
                     match self.toks[self.pos].tok {\n\
                         Id(s) => { self.pos = self.pos + 1; s }\n\
                         Num(n) => { self.pos = self.pos + 1; n.to_string() }\n\
                         A => { self.pos = self.pos + 1; \"a\".to_string() }\n\
                     }\n\
                 }\n\
             }\n\
             fn main() {\n\
                 let mut w: Vec[Sp] = Vec.new();\n\
                 w.push(Sp { tok: Tk.Id(\"hello\".to_string()), off: 5 });\n\
                 w.push(Sp { tok: Tk.Num(7), off: 9 });\n\
                 let mut p = P { toks: w, pos: 0 };\n\
                 println(p.off_now().to_string());\n\
                 println(p.kind_now().to_string());\n\
                 println(p.take());\n\
                 println(p.off_now().to_string());\n\
                 println(p.take());\n\
             }",
    ) {
        assert_eq!(out, "5\n1\nhello\n9\n7\n");
    }
}

/// General heap-index-read-into-owning-sink double-free (found while fixing
/// the heap-zip leg, B-2026-07-04-2): reading a heap element by index from a
/// named `Vec` (`v[i]`) and moving it into an OWNING sink — a tuple literal,
/// `push`, or a struct field — shallow-aliased the container's element
/// buffer, so both the container's element-drop and the sink's owner freed
/// it (double-free, exit 133). `let s = v[i]` already deep-cloned; this
/// closes the twin gap at the by-value consume sites via
/// `maybe_defensive_copy_param_arg` → `clone_owned_vec_index_element`. The
/// sources must survive (asserted via `.len()`), and the reads must be
/// correct.
#[test]
fn e2e_heap_vec_index_read_into_owning_sinks() {
    if let Some(out) = run_program(
        r#"
struct Pair { x: String, y: String }
fn main() {
    let v: Vec[String] = Vec["aa".to_string(), "bb".to_string(), "cc".to_string()];
    // tuple literal
    let t: (String, String) = (v[0i64], v[2i64]);
    // push into another Vec
    let mut d: Vec[String] = Vec.new();
    d.push(v[1i64]);
    // struct field
    let p: Pair = Pair { x: v[0i64], y: v[1i64] };
    println(f"{t.0} {t.1} {d[0i64]} {p.x} {p.y} {v.len()}");
}
"#,
    ) {
        assert_eq!(out, "aa cc bb aa bb 3\n");
    }
}

/// The `matrix[i][j]` nested-index clone gap (found via chunks/windows): a
/// `let x = m[i][j]` bind out of a `Vec[Vec[String]]` shallow-aliased the
/// innermost buffer -> double-free at the two scope exits. The clone helper
/// now peels one Vec layer per index level (`vec_index_elem_type_expr`), so
/// the binding owns an independent deep clone and the source survives. POD
/// nested elements are unaffected (trivially copyable). ASAN twin:
/// asan_nested_vec_index_bind_no_double_free.
#[test]
fn e2e_nested_vec_index_heap_bind_codegen() {
    if let Some(out) = run_program(
        r#"
fn main() {
    let mut v: Vec[String] = Vec.new();
    v.push("row-alpha".to_string());
    v.push("row-bravo".to_string());
    v.push("row-charlie".to_string());
    let m: Vec[Vec[String]] = v.iter().chunks(2i64).collect();
    let r0: Vec[String] = m[0i64].clone();
    let r1: Vec[String] = m[1i64].clone();
    let a: String = r0[0i64].clone();
    let b: String = r0[1i64].clone();
    let c: String = r1[0i64].clone();
    println(f"{a} {b} {c} {m.len()} {v.len()}");
}
"#,
    ) {
        assert_eq!(out, "row-alpha row-bravo row-charlie 2 3\n");
    }
}

#[test]
fn e2e_try_extend_from_slice_fallible_codegen() {
    // phase-8-stdlib-floor item 8: `Vec.try_extend_from_slice` — fallible
    // `extend_from_slice`. Trivially-copyable elements (i64) take the memcpy
    // path; dst starts cap 2 and src len 4 forces the grow (fallible alloc).
    // Verify the result matches `Ok` AND every element actually landed.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let src: Vec[i64] = Vec.filled(4_i64, 5_i64);\n\
                 let mut dst: Vec[i64] = Vec.with_capacity(2);\n\
                 dst.push(1_i64);\n\
                 match dst.try_extend_from_slice(src) {\n\
                     Ok(_) => println(\"ok\"),\n\
                     Err(_) => println(\"err\"),\n\
                 }\n\
                 println(dst.len());\n\
                 println(dst[0]);\n\
                 println(dst[4]);\n\
             }",
    ) {
        assert_eq!(out, "ok\n5\n1\n5\n");
    }
}

#[test]
fn e2e_try_extend_from_slice_clone_path_codegen() {
    // The heap-element path (Vec[String]) takes the per-element clone loop
    // (not the bit-copy memcpy), so the cloned strings are independent of the
    // source and survive content round-trip through `Ok(())`. (`?`-propagation
    // for the companion is covered by the try_push composition test.)
    if let Some(out) = run_program(
        "fn main() {\n\
                 let mut src: Vec[String] = Vec.new();\n\
                 src.push(\"ab\");\n\
                 src.push(\"cd\");\n\
                 let mut dst: Vec[String] = Vec.new();\n\
                 dst.push(\"x\");\n\
                 match dst.try_extend_from_slice(src) {\n\
                     Ok(_) => println(\"ok\"),\n\
                     Err(_) => println(\"err\"),\n\
                 }\n\
                 println(dst.len());\n\
                 println(dst[0]);\n\
                 println(dst[1]);\n\
                 println(dst[2]);\n\
             }",
    ) {
        assert_eq!(out, "ok\n3\nx\nab\ncd\n");
    }
}

#[test]
fn e2e_try_from_slice_fallible_codegen() {
    // phase-8-stdlib-floor item 8: `Vec.try_from_slice` — fallible
    // `from_slice` returning `Result[Vec[T], AllocError]`. The freshly-built
    // `Vec` aggregate is wrapped in `Result.Ok(_)` and round-trips through
    // match-extraction (the Vec-in-Result payload concern). Trivial element
    // (i64) takes the memcpy path.
    if let Some(out) = run_program(
            "fn main() {\n\
                 let src: Vec[i64] = Vec.filled(3_i64, 7_i64);\n\
                 match Vec.try_from_slice(src) {\n\
                     Ok(v) => { println(\"ok\"); println(v.len()); println(v[0]); println(v[2]); }\n\
                     Err(_) => println(\"err\"),\n\
                 }\n\
             }",
        ) {
            assert_eq!(out, "ok\n3\n7\n7\n");
        }
}

#[test]
fn test_e2e_ref_array_index_read_and_mut_store() {
    // B-2026-06-17-1: indexing a `ref`/`mut ref Array[T, N]` param used to
    // fail codegen ("Index operator applied to non-array type") because the
    // borrow slot's LLVM type is `ptr`, not `[N x T]`, and the dispatcher had
    // no ref-Array route. `ref_array_index_target` loads the data pointer and
    // GEPs through the recorded `[N x T]`, for both index-read and
    // index-store. `read_ref` reads through a `ref` borrow; `store_mut`
    // writes through a `mut ref` borrow; the write is observable back in the
    // owner via indexing.
    let src = r#"
fn read_ref(xs: ref Array[i64, 4]) -> i64 { xs[0] + xs[3] }
fn store_mut(xs: mut ref Array[i64, 4]) { xs[2] = 99; }

fn main() {
    let mut a: Array[i64, 4] = [10, 20, 30, 40];
    let r = read_ref(a);
    store_mut(mut a);
    println(r);
    println(a[2]);
}
"#;
    let out = run_program(src);
    if let Some(out) = out {
        assert_eq!(out, "50\n99\n");
    }
}

#[test]
fn test_ir_ptr_mut_on_tuple_and_index_place_compiles() {
    // Tuple-index place (`pair.0`) and Vec-index place (`v[0]`) both resolve
    // via `ptr_place_addr` rather than falling through.
    let ir = ir_for(
        "fn main() { let mut pair: (i32, i32) = (1, 2); \
             let pt: *mut i32 = ptr.mut(pair.0); \
             let mut v: Vec[i32] = Vec.new(); v.push(9); \
             let pv: *mut i32 = ptr.mut(v[0]); }",
    );
    assert!(
        !ir.contains("method dispatch fell through"),
        "ptr.mut on tuple / index places must not fall through; got IR:\n{ir}"
    );
}

#[test]
fn test_ir_array_index_read() {
    let ir = ir_for(
        r#"
fn second() -> i64 {
    let a = [10, 20, 30];
    a[1]
}
"#,
    );
    // Should contain GEP into the array and a bounds check.
    assert!(
        ir.contains("getelementptr"),
        "expected getelementptr for array index, got:\n{}",
        ir
    );
    assert!(
        ir.contains("idx.oob"),
        "expected bounds-check OOB block, got:\n{}",
        ir
    );
    assert!(
        ir.contains("idx.ok"),
        "expected bounds-check OK block, got:\n{}",
        ir
    );
}

#[test]
fn test_ir_converging_skip_refused_when_index_steps_before_use() {
    // The soundness gate, at the IR level: stepping `lo` BEFORE the index
    // means `lo` can exceed the bound the `lo <= hi` guard established, so
    // the skip must NOT fire and the full combined check must survive.
    // Same program as above with only the statement order changed.
    let src = CONV_TWO_POINTER_SRC.replace(
        "acc = acc + (v[base + lo] as i64) - (v[base + hi] as i64);\n            lo = lo + 1i64;",
        "lo = lo + 1i64;\n            acc = acc + (v[base + lo] as i64) - (v[base + hi] as i64);",
    );
    assert_ne!(src, CONV_TWO_POINTER_SRC, "reorder anchor did not match");
    let ir = ir_for(&src);
    assert!(
        ir.contains("vidx.ok"),
        "expected the reordered loop to KEEP its combined bounds check \
             (the skip is unsound there), got:\n{ir}"
    );
}

#[test]
fn test_ir_array_index_store() {
    let ir = ir_for(
        r#"
fn main() {
    let mut a: Array[i64, 3] = [1, 2, 3];
    a[0] = 42;
}
"#,
    );
    // `Array[i64, 3]` annotation pins the fixed-array store path (bare
    // `[…]` is now a Vec — see `test_ir_array_literal_construction`).
    assert!(
        ir.contains("arr.store.ptr"),
        "expected store GEP for index assignment, got:\n{}",
        ir
    );
}

#[test]
fn test_e2e_array_index_read() {
    let out = run_program(
        r#"
fn main() {
    let a = [10, 20, 30];
    println(a[1]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "20");
    }
}

#[test]
fn test_e2e_array_index_store() {
    let out = run_program(
        r#"
fn main() {
    let mut a = [10, 20, 30];
    a[2] = 99;
    println(a[2]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "99");
    }
}

#[test]
fn test_e2e_slice_basics_example() {
    let src = include_str!("../../examples/slice_basics.kara");
    let out = run_program(src);
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(
            lines,
            vec!["10", "600", "90", "1", "2", "10"],
            "slice_basics.kara output mismatch"
        );
    }
}

// Anonymous array literals in Slice contexts (phase-8 / kata-393 audit).
// Named arrays already coerced to `Slice[T]` params and range-sliced;
// bare literals did not. `f([1,2,3])` failed LLVM verification
// (`[N x i8]` vs `{ptr,i64}` param mismatch) and `f([1,2,3][a..b])`
// errored "range-slice requires a named source variable". Both now
// materialize the literal to a temp alloca and build a slice header.
// The interpreter always accepted these forms; codegen now matches.

#[test]
fn test_e2e_array_literal_arg_to_slice_param() {
    let out = run_program(
        r#"
fn sum(xs: Slice[u8]) -> i64 {
    let mut s = 0_i64;
    for x in xs { s = s + (x as i64); }
    s
}
fn main() {
    println(sum([4u8, 5u8, 6u8]));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "15");
    }
}

#[test]
fn test_e2e_empty_array_binding_to_slice_param() {
    // B-2026-06-14-30: `let a: Array[T, 0] = []` passed to a `Slice[T]`
    // param used to lower as a scalar `i64 0` (compile_array_literal's
    // empty fallback), so the Array → Slice coercion was skipped and the
    // raw i64 failed LLVM verification (`{ptr,i64}` vs `i64`). The fix
    // allocates a real `[0 x T]` slot from the annotation; the empty array
    // now coerces to a zero-length slice header. The non-empty binding
    // alongside it confirms the ordinary array path is unaffected.
    let out = run_program(
        r#"
fn take(s: Slice[i64], len: i64) -> i64 {
    let mut sum = 0i64;
    for i in 0..len { sum = sum + s[i]; }
    sum
}
fn main() {
    let a: Array[i64, 0] = [];
    println(take(a, 0));
    let b: Array[i64, 3] = [1, 2, 3];
    println(take(b, 3));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "0\n6");
    }
}

#[test]
fn test_e2e_array_arg_to_ref_slice_param() {
    // Regression for B-2026-06-19-1: an `Array[T, N]` passed to a
    // `ref Slice[T]` param mis-built. `ref Slice` was classified as a bare
    // ref param (extract_slice_elem_type returned None through the `Ref`
    // wrapper), so the call site's `get_data_ptr` identifier fast-path
    // passed the array's raw element storage as the slice arg; the callee
    // read `{ptr,len}` out of the array's first two elements — a bogus slice
    // (ptr = elem0, len = elem1) → out-of-bounds / segfault. The interpreter
    // always built a real header, so it was a run/build divergence. The fix
    // synthesizes a `{ptr,len}` header for an Array source and passes a
    // pointer to it.
    //
    // Three shapes, all of which segfaulted/OOB'd before the fix:
    //   - direct Array -> ref Slice, indexed read;
    //   - FORWARDING a ref-slice binding to another ref-slice param (must
    //     keep using get_data_ptr, NOT re-coerce — the narrowing guard);
    //   - the same forward, where the callee also builds a `Vec[bool]` sized
    //     by `n` and indexes it by a slice value (the kata-41 seen shape) —
    //     a corrupted slice made `n`/the read value wrong and OOB'd the Vec.
    let out = run_program(
        r#"
fn sum_ref(nums: ref Slice[i64], n: i64) -> i64 {
    let mut s = 0i64;
    let mut i = 0i64;
    while i < n { s = s + nums[i]; i = i + 1i64; }
    s
}
fn forward(nums: ref Slice[i64], n: i64) -> i64 {
    sum_ref(nums, n)
}
fn first_missing(nums: ref Slice[i64], n: i64) -> i64 {
    let mut seen: Vec[bool] = Vec.new();
    let mut k = 0i64;
    while k <= n { seen.push(false); k = k + 1i64; }
    let mut i = 0i64;
    while i < n {
        let v = nums[i];
        if v >= 1i64 and v <= n { seen[v] = true; }
        i = i + 1i64;
    }
    let mut v = 1i64;
    while v <= n {
        if not seen[v] { return v; }
        v = v + 1i64;
    }
    n + 1i64
}
fn main() {
    let a: Array[i64, 4] = [10, 20, 30, 40];
    println(sum_ref(a, 4));      // 100 — direct Array -> ref Slice
    println(forward(a, 4));      // 100 — forwarded ref-slice binding
    let b: Array[i64, 5] = [3, 4, -1, 1, 9];
    println(first_missing(b, 5)); // 2 — ref-slice forward + Vec[bool] index
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "100\n100\n2");
    }
}

#[test]
fn test_e2e_soa_index_read_with_cold_group() {
    // Exercises the cold-group branch of compile_soa_index_read: `vy`
    // lives in a separate cold allocation, `x`/`y` in one hot group,
    // `vx` in another. Materializing entities[i] must reassemble
    // fields from all three buffers in struct order.
    let src = r#"
struct Entity { x: i64, y: i64, vx: i64, vy: i64 }
layout entities: Vec[Entity] {
    group pos { x, y }
    group vel { vx }
    cold { vy }
}
fn main() {
    let mut entities: Vec[Entity] = Vec.new();
    let mut i: i64 = 0;
    while i < 5 {
        entities.push(Entity { x: i, y: i + 1, vx: i + 2, vy: i + 3 });
        i = i + 1;
    }
    let e = entities[4];
    println(e.x);
    println(e.y);
    println(e.vx);
    println(e.vy);
    println(entities[2].vy);
}
"#;
    if let Some(out) = run_program(src) {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["4", "5", "6", "7", "5"]);
    }
}

#[test]
fn test_e2e_vec_of_vec_index_ref_arg() {
    // Regression: passing `stake[idx]` (an aggregate element of a
    // Vec[Vec[T]]) to a `ref Vec[T]` parameter shallow-copied the
    // element struct and dropped the copy as a call-temp, double-
    // freeing the buffer the outer Vec still owned. Symptom was a
    // hang/SIGTRAP once the loop count grew (heap corruption). The
    // fix passes a pointer to the element in place (borrow, no drop).
    // Loop to 200 so any double-free corrupts the allocator.
    let out = run_program(
        r#"
fn make() -> Vec[char] {
    let mut v: Vec[char] = Vec.with_capacity(4);
    v.push('a');
    v.push('b');
    v
}
fn sumlen(r: ref Vec[char]) -> i64 {
    r.len()
}
fn main() {
    let mut stake: Vec[Vec[char]] = Vec.with_capacity(2);
    stake.push(make());
    stake.push(make());
    let mut sum: i64 = 0;
    let mut k: i64 = 0;
    while k < 200 {
        let idx: i64 = k % 2;
        sum = sum + sumlen(stake[idx]);
        k = k + 1;
    }
    println(sum);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "400");
    }
}

/// B-2026-08-09-21 — a nested index whose base is a struct FIELD
/// (`h.data[i][j]`), read and write.
///
/// RUN-VS-BUILD: `--interp` ran these; `karac run` (JIT) and `karac build`
/// both refused them. The boundary was oddly narrow and is what identifies
/// the cause — `d[i][j]` on a named local BUILT, `h.data[i]` (single index
/// on a field) BUILT, `let row = h.data[i]; row[j]` BUILT; only the DOUBLE
/// index rooted at a field failed. `compile_nested_index_read` recovered the
/// element `TypeExpr` from `var_elem_type_exprs[outer_name]`, which is
/// keyed by variable name and so has no entry for a field.
///
/// Cases 3 and 4 are the ones that matter most in practice, and they are why
/// the read arm resolves the base through `lower_field_access_ptr` rather
/// than `field_chain_place_ptr`: the latter deliberately declines a
/// `ref`-param root (its slot holds a pointer, not the aggregate), which
/// would have left every `ref self` method and `ref H` parameter failing —
/// the shape a 2D-cursor struct actually writes, and the motivating kata's
/// canonical spelling (`v.data[v.row][v.col]`).
///
/// Case 5 pins what stays deferred: a TRIPLE index is still refused by the
/// separate MR5 guard, loudly. Widening the field base was not an excuse to
/// generalise that too.
#[test]
fn test_e2e_nested_index_rooted_at_struct_field() {
    // 1. The filed reproduction — read.
    assert_eq!(
        run_program(
            "struct H { data: Vec[Vec[i64]] }\n\
                 fn main() {\n\
                     let mut i: Vec[i64] = Vec.new();\n\
                     i.push(1i64);\n\
                     let mut d: Vec[Vec[i64]] = Vec.new();\n\
                     d.push(i);\n\
                     let h = H { data: d };\n\
                     println(f\"{h.data[0][0]}\");\n\
                 }"
        )
        .as_deref(),
        Some("1\n")
    );
    // 2. The WRITE half, which failed on a different gate ("Index
    // assignment target must be a variable") and needed its own arm.
    assert_eq!(
        run_program(
            "struct H { data: Vec[Vec[i64]] }\n\
                 fn main() {\n\
                     let mut i: Vec[i64] = Vec.new();\n\
                     i.push(1i64);\n\
                     let mut d: Vec[Vec[i64]] = Vec.new();\n\
                     d.push(i);\n\
                     let mut h = H { data: d };\n\
                     h.data[0][0] = 42i64;\n\
                     println(f\"{h.data[0][0]}\");\n\
                 }"
        )
        .as_deref(),
        Some("42\n")
    );
    // 3. `ref self` / `mut ref self` methods — the canonical 2D-cursor shape.
    assert_eq!(
        run_program(
            "struct G { grid: Vec[Vec[i64]] }\n\
                 impl G {\n\
                 \x20   fn get(ref self, i: i64, j: i64) -> i64 { return self.grid[i][j]; }\n\
                 \x20   fn set(mut ref self, i: i64, j: i64, v: i64) { self.grid[i][j] = v; }\n\
                 }\n\
                 fn main() {\n\
                     let mut r0: Vec[i64] = Vec.new();\n\
                     r0.push(1i64); r0.push(2i64);\n\
                     let mut g0: Vec[Vec[i64]] = Vec.new();\n\
                     g0.push(r0);\n\
                     let mut g = G { grid: g0 };\n\
                     println(f\"{g.get(0i64, 1i64)}\");\n\
                     g.set(0i64, 1i64, 99i64);\n\
                     println(f\"{g.get(0i64, 1i64)}\");\n\
                 }"
        )
        .as_deref(),
        Some("2\n99\n")
    );
    // 4. A `ref` / `mut ref` PARAM root, the free-function form of case 3.
    assert_eq!(
        run_program(
            "struct H { data: Vec[Vec[i64]] }\n\
                 fn read(h: ref H, i: i64, j: i64) -> i64 { return h.data[i][j]; }\n\
                 fn bump(h: mut ref H) { h.data[0][0] = h.data[0][0] + 1i64; }\n\
                 fn main() {\n\
                     let mut r: Vec[i64] = Vec.new();\n\
                     r.push(5i64);\n\
                     let mut d: Vec[Vec[i64]] = Vec.new();\n\
                     d.push(r);\n\
                     let mut h = H { data: d };\n\
                     println(f\"{read(h, 0i64, 0i64)}\");\n\
                     bump(mut h);\n\
                     println(f\"{read(h, 0i64, 0i64)}\");\n\
                 }"
        )
        .as_deref(),
        Some("5\n6\n")
    );
    // 5. The triple index. This case was added to pin the MR5 guard as a
    // LOUD deferral, on the principle that widening the field base was not
    // an excuse to generalise that guard too. B-2026-08-20-33 then
    // generalised it deliberately, through a shared place resolver rather
    // than a fourth hand-written base shape — so the case is INVERTED here
    // rather than deleted: it is the one that pins where the boundary sits,
    // and it now sits one level deeper.
    let triple = ir_result(
        "fn main() {\n\
                 let mut a: Vec[i64] = Vec.new();\n\
                 a.push(1i64);\n\
                 let mut b: Vec[Vec[i64]] = Vec.new();\n\
                 b.push(a);\n\
                 let mut c: Vec[Vec[Vec[i64]]] = Vec.new();\n\
                 c.push(b);\n\
                 println(f\"{c[0][0][0]}\");\n\
             }",
    );
    assert!(
        triple.is_ok(),
        "triple index must lower since B-2026-08-20-33, got {triple:?}"
    );
    // 6. Bounds checking survives the new path — the inner index goes
    // through the same checked helper the named-base path uses, so an OOB
    // outer index panics rather than reading out of the buffer.
    if let Some(c) = run_program_capturing(
        "struct H { data: Vec[Vec[i64]] }\n\
             fn main() {\n\
                 let mut r: Vec[i64] = Vec.new();\n\
                 r.push(1i64);\n\
                 let mut d: Vec[Vec[i64]] = Vec.new();\n\
                 d.push(r);\n\
                 let h = H { data: d };\n\
                 println(f\"{h.data[0][5]}\");\n\
             }",
    ) {
        assert!(
            c.stderr.contains("vec index out of bounds"),
            "expected OOB panic, got stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
    }
    // 7. A shared-struct receiver, and a loop driving computed indices
    // through both the read and the write.
    assert_eq!(
        run_program(
            "struct H { data: Vec[Vec[i64]] }\n\
                 fn main() {\n\
                     let mut r0: Vec[i64] = Vec.new();\n\
                     r0.push(10i64); r0.push(20i64);\n\
                     let mut r1: Vec[i64] = Vec.new();\n\
                     r1.push(30i64); r1.push(40i64);\n\
                     let mut d: Vec[Vec[i64]] = Vec.new();\n\
                     d.push(r0); d.push(r1);\n\
                     let mut h = H { data: d };\n\
                     let mut i: i64 = 0;\n\
                     let mut total: i64 = 0;\n\
                     while i < 2 {\n\
                         let mut j: i64 = 0;\n\
                         while j < 2 {\n\
                             total = total + h.data[i][j];\n\
                             h.data[i][j] = h.data[i][j] + 1i64;\n\
                             j = j + 1;\n\
                         }\n\
                         i = i + 1;\n\
                     }\n\
                     println(f\"{total}\");\n\
                     println(f\"{h.data[1][1]}\");\n\
                 }"
        )
        .as_deref(),
        Some("100\n41\n")
    );
}

/// B-2026-09-09-25 — a nested index whose OUTER container is a fixed
/// `Array[T, N]`, write and read.
///
/// RUN-VS-BUILD in the form where one side produces no binary at all:
/// `--interp` ran `a[0][1] = 99` over `Array[Vec[i64], 2]` and every
/// compiled configuration (JIT, AOT, both opt levels, auto-par on and off)
/// refused it with "Index assignment target must be a variable". The
/// identical declaration with a `Vec` outer stored fine, and the READ half
/// of the SAME declaration had already been fixed by B-2026-09-09-9 — so
/// the element could be printed but not written.
///
/// One registry miss caused both halves, one call site apart. An
/// `Array[T, N]` records its element `TypeExpr` in `array_elem_type_exprs`
/// rather than the shared `var_elem_type_exprs` (widening the shared table
/// is unsafe — ~170 readers treat a present entry as "this binding is a
/// Vec/Slice/Map"), and `container_place_name` — the resolver both
/// index-rooted paths go through — consulted only the shared table. It
/// returned `None` for every array base BEFORE reaching its own
/// `ArrayType` dispatch arm, which was therefore unreachable code.
///
/// Case 6 is why the fix is two arms rather than one: with the store fixed,
/// `h.a[0][1] = 99` built and the read-back on the next line refused with
/// "nested indexed read requires the outer container to be a named
/// variable" — `nested_index_field_base_elem` resolves a field base through
/// `vec_inner_type_expr`, which answers only for a `Vec` head. A write that
/// compiles and a read of the same place that does not is a worse state
/// than the uniform refusal it replaced.
///
/// Case 8 is the control that keeps the fix honest about WHICH side was
/// broken: an Array INNER under a `Vec` outer always worked, because that
/// base is a Vec and resolves through the shared table.
#[test]
fn test_e2e_nested_index_store_over_an_array_outer() {
    // 1. The filed reproduction — store, then read the same place back.
    assert_eq!(
        run_program(
            "fn main() {\n\
                 \x20   let mut a: Array[Vec[i64], 2] = [[10, 11], [20]];\n\
                 \x20   a[0][1] = 99;\n\
                 \x20   println(f\"{a[0][1]}\");\n\
                 \x20   println(f\"{a[1][0]}\");\n\
                 }"
        )
        .as_deref(),
        Some("99\n20\n")
    );
    // 2. The COMPOUND spelling, which the row left unmeasured and which
    // refused with the same message.
    assert_eq!(
        run_program(
            "fn main() {\n\
                 \x20   let mut a: Array[Vec[i64], 2] = [[10, 11], [20]];\n\
                 \x20   a[0][1] += 1;\n\
                 \x20   println(f\"{a[0][1]}\");\n\
                 }"
        )
        .as_deref(),
        Some("12\n")
    );
    // 3. THREE levels. Resolving through `container_place_name` recurses,
    // so depth costs nothing extra — the same property that let
    // B-2026-08-20-33 lift the chained-store deferral.
    assert_eq!(
        run_program(
            "fn main() {\n\
                 \x20   let mut a: Array[Vec[Vec[i64]], 2] = [[[1, 2, 3]], [[4]]];\n\
                 \x20   a[0][0][2] = 99;\n\
                 \x20   println(f\"{a[0][0][2]}\");\n\
                 }"
        )
        .as_deref(),
        Some("99\n")
    );
    // 4. Array all the way down — the inner element is itself a fixed
    // array, so the synth minted for `a[0]` registers through
    // `array_elem_type_exprs` in turn.
    assert_eq!(
        run_program(
            "fn main() {\n\
                 \x20   let mut a: Array[Array[i64, 2], 2] = [[10, 11], [20, 21]];\n\
                 \x20   a[0][1] = 99;\n\
                 \x20   println(f\"{a[0][1]}:{a[1][0]}\");\n\
                 }"
        )
        .as_deref(),
        Some("99:20\n")
    );
    // 5. A HEAP element, so the store has a displaced buffer to release.
    // Measured under valgrind at -O0: all heap blocks freed, 0 errors.
    assert_eq!(
        run_program(
            "fn main() {\n\
                 \x20   let mut a: Array[Vec[String], 2] = [[\"x\", \"y\"], [\"z\"]];\n\
                 \x20   a[0][1] = \"MUT\";\n\
                 \x20   println(f\"{a[0][1]}\");\n\
                 }"
        )
        .as_deref(),
        Some("MUT\n")
    );
    // 6. A struct FIELD base — the spelling a 2D cursor in a struct
    // actually writes, and the one that needed the read arm too.
    assert_eq!(
        run_program(
            "struct H { a: Array[Vec[i64], 2] }\n\
                 fn main() {\n\
                 \x20   let mut h = H { a: [[10, 11], [20]] };\n\
                 \x20   h.a[0][1] = 99;\n\
                 \x20   println(f\"{h.a[0][1]}\");\n\
                 }"
        )
        .as_deref(),
        Some("99\n")
    );
    // 7. `mut ref self` — the field base reached from inside a method,
    // where the receiver normalisation runs.
    assert_eq!(
        run_program(
            "struct H { a: Array[Vec[i64], 2] }\n\
                 impl H {\n\
                 \x20   fn bump(mut ref self) {\n\
                 \x20       self.a[0][1] = self.a[0][1] + 1;\n\
                 \x20   }\n\
                 }\n\
                 fn main() {\n\
                 \x20   let mut h = H { a: [[10, 11], [20]] };\n\
                 \x20   h.bump();\n\
                 \x20   println(f\"{h.a[0][1]}\");\n\
                 }"
        )
        .as_deref(),
        Some("12\n")
    );
    // 8. CONTROL — an Array INNER under a Vec outer, which built before
    // this fix and must still.
    assert_eq!(
        run_program(
            "fn main() {\n\
                 \x20   let r0: Array[i64, 2] = [10, 11];\n\
                 \x20   let r1: Array[i64, 2] = [20, 21];\n\
                 \x20   let mut v: Vec[Array[i64, 2]] = Vec.new();\n\
                 \x20   v.push(r0);\n\
                 \x20   v.push(r1);\n\
                 \x20   v[0][1] = 99;\n\
                 \x20   println(f\"{v[0][1]}:{v[1][0]}\");\n\
                 }"
        )
        .as_deref(),
        Some("99:20\n")
    );
}

/// B-2026-09-14-30's cell. THE EXPECTATION FLIPPED, under
/// B-2026-09-15-33, and the whole point of this note is that the flip was
/// PREDICTED TWICE and denied once before it happened.
///
/// B-2026-09-14-29's prose expected closing it to add a leading `dD1`
/// here. It did not, and this comment used to say why: `a[0] = b` stores
/// a NAMED LOCAL, which `store_destroys_displaced` classified as a
/// RELOCATION rather than a destruction, so `run_bodies` was false and the
/// displaced body was deliberately suppressed. -14-29's fix gave the
/// emitter its missing Array element-type and addressing and did not touch
/// that gate, so a fresh-literal RHS gained `dD1` (see
/// `e2e_array_index_store_runs_the_displaced_elements_drop_body`) and this
/// named-local spelling did not. All of that was accurate, and the run/build
/// divergence it recorded was filed as B-2026-09-15-33 rather than resolved
/// by loosening the gate.
///
/// -15-33 then answered the question it was filed to ask — is `a[0] = b` a
/// relocation at all — and the answer turned out to be about what still
/// typechecks rather than about drop timing. The swap idiom the
/// suppression existed to protect (B-2026-08-26-21, five `Drop` bodies for
/// two values across three lines) NO LONGER COMPILES: `E_INDEX_MOVE_NON_COPY`
/// rejects its index reads, including for a scalar-only element struct,
/// because an `impl Drop` makes a type non-`Copy` whatever its fields are.
/// The gate now admits an identifier RHS, so this cell prints the leading
/// `dD1` that -14-29 expected and `--interp` always produced.
///
/// THE OLD EXPECTATION PINNED A DEFECT'S ANSWER, which is why it is called
/// out rather than quietly edited: a test whose recorded output is the
/// wrong one reads green through the fix that corrects it, and the only
/// thing that distinguishes the two cases is a comment saying which it is.
/// This one is now the DUE sequence — the displaced `D{1}` has no owner
/// after the store and its body belongs there.
///
/// AND THE SHAPE OF THE MISTAKE OUTLIVES THIS CELL, which is why it is
/// written here and not only in the ledger row: A COMMENT RECORDING THAT A
/// PREDICTED CHANGE DID NOT HAPPEN IS A MEASUREMENT OF ONE TREE. It decays
/// exactly the way a guard comment does, and the dangerous part is that it
/// reads as REASONING rather than as DATA, so nobody re-measures it. The
/// paragraph above was accurate the day it was written, stood for five
/// days, and was false by the time the fix it anticipated arrived — while
/// still reading, the whole time, like an argument for why the expectation
/// was correct.
#[test]
fn test_e2e_array_index_store_runs_the_moved_in_source_body_once() {
    assert_eq!(
            run_program(
                "struct D { id: i64, s: String }\n\
                 impl Drop for D { fn drop(mut ref self) { println(f\"dD{self.id}\"); } }\n\
                 fn main() {\n\
                 \x20   let mut a: Array[D, 2] = [D { id: 1, s: \"b30-static-one\" }, D { id: 2, s: \"b30-static-two\" }];\n\
                 \x20   let b = D { id: 3, s: f\"b30-heap-payload\" };\n\
                 \x20   a[0] = b;\n\
                 \x20   println(f\"a0:{a[0].id}\");\n\
                 }"
            )
            .as_deref(),
            Some("dD1\na0:3\ndD3\ndD2\n")
        );
}

#[test]
fn e2e_array_index_store_runs_the_displaced_elements_drop_body() {
    const H: &str = "struct D { s: String, id: i64 }\n\
             impl Drop for D { fn drop(mut ref self) { println(f\"dD{self.id}\") } }\n";
    // Heap field + Drop body: the row's own repro.
    assert_eq!(
            run_program(&format!(
                "{H}fn main() {{\n\
                 \x20   let mut a: Array[D, 2] = [D {{ s: f\"aaaaaaaa-1\", id: 1 }}, D {{ s: f\"bbbbbbbb-2\", id: 2 }}];\n\
                 \x20   a[0] = D {{ s: f\"MUTATED-3\", id: 3 }};\n\
                 \x20   println(f\"a0:{{a[0].id}}\");\n\
                 }}"
            ))
            .as_deref(),
            Some("dD1\na0:3\ndD3\ndD2\n")
        );
    // A `Drop` body with NO heap field — the body half alone.
    assert_eq!(
        run_program(
            "struct F { id: i64 }\n\
                 impl Drop for F { fn drop(mut ref self) { println(f\"dF{self.id}\") } }\n\
                 fn main() {\n\
                 \x20   let mut a: Array[F, 2] = [F { id: 1 }, F { id: 2 }];\n\
                 \x20   a[0] = F { id: 3 };\n\
                 \x20   println(f\"a0:{a[0].id}\");\n\
                 }"
        )
        .as_deref(),
        Some("dF1\na0:3\ndF3\ndF2\n")
    );
    // THE ORACLE — the `Vec[D]` twin, correct before and after.
    assert_eq!(
            run_program(&format!(
                "{H}fn main() {{\n\
                 \x20   let mut a: Vec[D] = [D {{ s: f\"aaaaaaaa-1\", id: 1 }}, D {{ s: f\"bbbbbbbb-2\", id: 2 }}];\n\
                 \x20   a[0] = D {{ s: f\"MUTATED-3\", id: 3 }};\n\
                 \x20   println(f\"a0:{{a[0].id}}\");\n\
                 }}"
            ))
            .as_deref(),
            Some("dD1\na0:3\ndD3\ndD2\n")
        );
    // A FIELD-rooted array (`h.xs[0] = ..`) reaches the same emitter
    // through its synth-identifier path, and lands on the SAME transcript as
    // the local-array and `Vec` cells above: `dD1` at the store for the
    // displaced element, then the field walk at scope exit for the two
    // survivors.
    //
    // THIS CELL SHIPPED WITH A STALE EXPECTATION -- `dD1 a0:3`, recorded
    // without the two survivors -- and that value matched NO tree on `main`.
    // It is this fix (`bfeeb86`, the displaced element's release) composed
    // with `645ea3b`'s Array-typed-struct-field element walk (B-2026-09-15-26,
    // filed off this fix's own probe sweep), which had already landed. Measured on the parent commit `8a68502`, the
    // field-rooted output was `a0:3 dD3 dD2`: the walk present, the
    // displaced body missing. So `dD1 a0:3` could only have come from a
    // branch forked BEFORE `645ea3b`, measured there, and not re-run after
    // rebasing onto it -- the same pre-rebase/post-push window that
    // B-2026-09-01-27 records. Corrected here rather than in a new row,
    // because the behaviour was already right and only the pin was wrong.
    assert_eq!(
            run_program(&format!(
                "{H}struct H2 {{ xs: Array[D, 2] }}\n\
                 fn main() {{\n\
                 \x20   let mut h: H2 = H2 {{ xs: [D {{ s: f\"aaaaaaaa-1\", id: 1 }}, D {{ s: f\"bbbbbbbb-2\", id: 2 }}] }};\n\
                 \x20   h.xs[0] = D {{ s: f\"MUTATED-3\", id: 3 }};\n\
                 \x20   println(f\"a0:{{h.xs[0].id}}\");\n\
                 }}"
            ))
            .as_deref(),
            Some("dD1\na0:3\ndD3\ndD2\n")
        );
}

/// B-2026-08-21-4 — `as_slice()` on a `ref`-mode receiver, and a call
/// declared to return `Slice[T]`.
///
/// THE SILENT HALF was `as_slice` reading the slice header straight out of
/// the receiver's SLOT. For an owned binding the alloca IS the aggregate,
/// so that was right by accident; for a `ref` / `mut ref` PARAMETER the
/// alloca holds a POINTER to the caller's aggregate, and the lowering read
/// that pointer as the Vec's `data` and whatever sat beside it on the stack
/// as `len`. `v.as_slice().len()` returned a pointer-sized number, exit 0,
/// no diagnostic. Case 1 needs no `Slice` return at all, which is what
/// identifies the cause: the row was filed against the return boundary, but
/// the header is built wrong before it ever reaches a `return`.
///
/// THE LOUD HALF was the element type: a call result had no entry in
/// either slice-element resolver, so `pick(v)[2]` and `for x in s` refused.
/// The filing row measured that arm ALONE and reverted it — with the header
/// still wrong it turned the loud failures into silent ones (`pick(v)[2]`
/// gave 3, the length, instead of 30). Both land together here, and case 5
/// is the one that proves the order mattered.
#[test]
fn e2e_as_slice_on_a_ref_receiver_and_a_slice_returning_call() {
    // 1. No `Slice` return anywhere — the minimal shape of the miscompile.
    //    The owned twin was always correct and is the control.
    assert_eq!(
        run_program(
            "fn probe(v: ref Vec[i64]) -> i64 {\n\
                 \x20   let s = v.as_slice();\n\
                 \x20   return s.len();\n\
                 }\n\
                 fn owned(v: Vec[i64]) -> i64 {\n\
                 \x20   let s = v.as_slice();\n\
                 \x20   return s.len();\n\
                 }\n\
                 fn main() {\n\
                 \x20   let v: Vec[i64] = [10, 20, 30];\n\
                 \x20   println(f\"{probe(v)}\");\n\
                 \x20   let w: Vec[i64] = [1, 2];\n\
                 \x20   println(f\"{owned(w)}\");\n\
                 }\n"
        )
        .as_deref(),
        Some("3\n2\n")
    );
    // 2. The row's own repro: an inline call through a `Slice[T]` return.
    assert_eq!(
        run_program(
            "fn pick(v: ref Vec[i64]) -> Slice[i64] {\n\
                 \x20   return v.as_slice();\n\
                 }\n\
                 fn main() {\n\
                 \x20   let v: Vec[i64] = [10, 20, 30];\n\
                 \x20   println(f\"{pick(v).len()}\");\n\
                 }\n"
        )
        .as_deref(),
        Some("3\n")
    );
    // 3. Bound to a local, then a method on it.
    assert_eq!(
        run_program(
            "fn pick(v: ref Vec[i64]) -> Slice[i64] {\n\
                 \x20   return v.as_slice();\n\
                 }\n\
                 fn main() {\n\
                 \x20   let v: Vec[i64] = [10, 20, 30];\n\
                 \x20   let s = pick(v);\n\
                 \x20   println(f\"{s.len()}\");\n\
                 }\n"
        )
        .as_deref(),
        Some("3\n")
    );
    // 4. Iterated — the shape that flooded garbage words rather than
    //    printing three elements.
    assert_eq!(
        run_program(
            "fn pick(v: ref Vec[i64]) -> Slice[i64] {\n\
                 \x20   return v.as_slice();\n\
                 }\n\
                 fn main() {\n\
                 \x20   let v: Vec[i64] = [10, 20, 30];\n\
                 \x20   let s = pick(v);\n\
                 \x20   for x in s {\n\
                 \x20       println(f\"{x}\");\n\
                 \x20   }\n\
                 }\n"
        )
        .as_deref(),
        Some("10\n20\n30\n")
    );
    // 5. INDEXED, inline and bound. This is the case the filing row saw
    //    return 3 (the length) when the element type was registered while
    //    the header was still wrong — so `30` here is what proves the
    //    header fix has to come first, not merely alongside.
    assert_eq!(
        run_program(
            "fn pick(v: ref Vec[i64]) -> Slice[i64] {\n\
                 \x20   return v.as_slice();\n\
                 }\n\
                 fn main() {\n\
                 \x20   let v: Vec[i64] = [10, 20, 30];\n\
                 \x20   println(f\"{pick(v)[2]}\");\n\
                 \x20   let s = pick(v);\n\
                 \x20   println(f\"{s[0]}\");\n\
                 }\n"
        )
        .as_deref(),
        Some("30\n10\n")
    );
    // 6. A METHOD declared `-> Slice[T]`. The impl pass emits it as a
    //    `Type.method` function and records its return in the same table,
    //    so resolving the receiver's type name is the whole of the extra
    //    work. The filing row measured this sibling as a hard refusal and
    //    asked for it to be re-measured once the header was fixed.
    assert_eq!(
        run_program(
            "struct Buf { v: Vec[i64] }\n\
                 impl Buf {\n\
                 \x20   fn view(ref self) -> Slice[i64] {\n\
                 \x20       return self.v.as_slice();\n\
                 \x20   }\n\
                 }\n\
                 fn main() {\n\
                 \x20   let b = Buf { v: [7, 8, 9] };\n\
                 \x20   println(f\"{b.view()[2]}\");\n\
                 \x20   let s = b.view();\n\
                 \x20   println(f\"{s.len()}\");\n\
                 \x20   println(f\"{b.view().len()}\");\n\
                 }\n"
        )
        .as_deref(),
        Some("9\n3\n3\n")
    );
    // 7. A `mut ref` receiver, and a slice taken through it after a
    //    mutation — the length must track the caller's Vec, not a stale
    //    copy of the header.
    assert_eq!(
        run_program(
            "fn grow(v: mut ref Vec[i64]) -> i64 {\n\
                 \x20   v.push(40i64);\n\
                 \x20   let s = v.as_slice();\n\
                 \x20   return s.len();\n\
                 }\n\
                 fn main() {\n\
                 \x20   let mut v: Vec[i64] = [10, 20, 30];\n\
                 \x20   println(f\"{grow(mut v)}\");\n\
                 }\n"
        )
        .as_deref(),
        Some("4\n")
    );
}

/// B-2026-08-20-35 — a nested index whose outer container is a MAP.
///
/// The READ (`m[k][i]`) was refused `outer is not a Vec/Slice/Array`. The
/// WRITE was worse than refused: `m[k][i] = v` BUILT and then SEGFAULTED.
/// `var_elem_type_exprs` holds a map's VALUE TypeExpr, so for
/// `Map[K, Vec[V]]` that is `Vec[V]` and the nested store's "is the outer a
/// Vec of Vecs?" test happily said yes — handing a MAP HANDLE to
/// `compile_nested_vec_vec_index_store`, which indexed it as a Vec of Vecs.
/// The row that filed this described a clean refusal on both halves; the
/// store half was a silent miscompile.
///
/// Case 4 is the one that shows the fix generalised rather than special-
/// cased: a `Vec[Map[K, V]]` matches NEITHER test — the outer is a Vec and
/// its element is not — and is served by the shared place resolver.
#[test]
fn e2e_nested_index_through_a_map() {
    // 1. The read half.
    assert_eq!(
        run_program(
            "fn main() {\n\
                 \x20   let mut m: Map[i64, Vec[i64]] = Map.new();\n\
                 \x20   m.insert(1i64, [7i64, 8i64]);\n\
                 \x20   println(f\"{m[1i64][0]}\");\n\
                 }\n"
        )
        .as_deref(),
        Some("7\n")
    );
    // 2. The write half — the one that used to build and segfault. The
    // read-back also proves the store reached the MAP's storage rather
    // than a temporary.
    assert_eq!(
        run_program(
            "fn main() {\n\
                 \x20   let mut m: Map[i64, Vec[i64]] = Map.new();\n\
                 \x20   m.insert(1i64, [7i64, 8i64]);\n\
                 \x20   m.insert(2i64, [9i64]);\n\
                 \x20   m[1i64][1] = 55i64;\n\
                 \x20   println(f\"{m[1i64][0]} {m[1i64][1]} {m[2i64][0]}\");\n\
                 }\n"
        )
        .as_deref(),
        Some("7 55 9\n")
    );
    // 3. A Map whose values are Maps.
    assert_eq!(
        run_program(
            "fn main() {\n\
                 \x20   let mut mm: Map[i64, Map[i64, i64]] = Map.new();\n\
                 \x20   let mut inner: Map[i64, i64] = Map.new();\n\
                 \x20   inner.insert(5i64, 6i64);\n\
                 \x20   mm.insert(1i64, inner);\n\
                 \x20   mm[1i64][5i64] = 66i64;\n\
                 \x20   println(f\"{mm[1i64][5i64]}\");\n\
                 }\n"
        )
        .as_deref(),
        Some("66\n")
    );
    // 4. A VEC whose elements are Maps — neither the vec-of-vec test nor
    // the map test matches, so this only works through the shared
    // resolver. It was still refused after the first two were fixed.
    assert_eq!(
        run_program(
            "fn main() {\n\
                 \x20   let mut vm: Vec[Map[i64, i64]] = Vec.new();\n\
                 \x20   let mut e: Map[i64, i64] = Map.new();\n\
                 \x20   e.insert(3i64, 4i64);\n\
                 \x20   vm.push(e);\n\
                 \x20   vm[0][3i64] = 44i64;\n\
                 \x20   println(f\"{vm[0][3i64]}\");\n\
                 }\n"
        )
        .as_deref(),
        Some("44\n")
    );
    // 5. A Map reached through a struct FIELD.
    assert_eq!(
        run_program(
            "struct Holder { buckets: Map[i64, Vec[i64]] }\n\
                 fn main() {\n\
                 \x20   let mut h = Holder { buckets: Map.new() };\n\
                 \x20   h.buckets.insert(1i64, [1i64, 2i64]);\n\
                 \x20   h.buckets[1i64][0] = 21i64;\n\
                 \x20   println(f\"{h.buckets[1i64][0]} {h.buckets[1i64][1]}\");\n\
                 }\n"
        )
        .as_deref(),
        Some("21 2\n")
    );
}

/// B-2026-08-20-33 — a THREE-level index, read and write.
///
/// `a[i][j][k] = v` was refused `Index assignment target must be a
/// variable` — a message that named the wrong thing, since the target IS a
/// variable, just reached through two indices. The read half was refused
/// separately as an explicit MR5 deferral. Two levels worked on both sides,
/// so the boundary was exactly one index deep.
///
/// The fix is a shared recursive place resolver rather than a fourth
/// hand-written base shape. That is what the earlier siblings had each
/// added one of — B-2026-08-09-21 for a struct-FIELD base, B-2026-08-10-5
/// for a TUPLE-field base — and each still required whatever sat under it
/// to be a bare identifier, which is why a chained base fell through all of
/// them. Cases 4-6 below are the ones that prove it GENERALISED rather than
/// moved the boundary by one: depth four, a struct-field root at depth
/// three, and computed (non-literal) indices at every level.
#[test]
fn e2e_three_level_index_read_and_write() {
    // 1. The row's own repro — the write half.
    assert_eq!(
        run_program(
            "fn main() {\n\
                 \x20   let mut d: Vec[Vec[Vec[i64]]] = [[[5]]];\n\
                 \x20   d[0][0][0] = 42i64;\n\
                 \x20   println(f\"{d[0][0][0]}\");\n\
                 }\n"
        )
        .as_deref(),
        Some("42\n")
    );
    // 2. The read half, which failed on the separate MR5 guard.
    assert_eq!(
        run_program(
            "fn main() {\n\
                 \x20   let deep: Vec[Vec[Vec[i64]]] = [[[5, 6]]];\n\
                 \x20   println(f\"{deep[0][0][1]}\");\n\
                 }\n"
        )
        .as_deref(),
        Some("6\n")
    );
    // 3. Through a `mut ref` parameter — the store has to reach the
    // caller's buffer, not a copy.
    assert_eq!(
        run_program(
            "fn bump(d: mut ref Vec[Vec[Vec[i64]]]) {\n\
                 \x20   d[0][0][0] = d[0][0][0] + 1i64;\n\
                 }\n\
                 fn main() {\n\
                 \x20   let mut d: Vec[Vec[Vec[i64]]] = [[[5]]];\n\
                 \x20   bump(mut d);\n\
                 \x20   println(f\"{d[0][0][0]}\");\n\
                 }\n"
        )
        .as_deref(),
        Some("6\n")
    );
    // 4. DEPTH FOUR — the resolver recurses, so one more level costs
    // nothing. A per-depth special case would fail here.
    assert_eq!(
        run_program(
            "fn main() {\n\
                 \x20   let mut d: Vec[Vec[Vec[Vec[i64]]]] = [[[[1]]]];\n\
                 \x20   d[0][0][0][0] = 4i64;\n\
                 \x20   println(f\"{d[0][0][0][0]}\");\n\
                 }\n"
        )
        .as_deref(),
        Some("4\n")
    );
    // 5. A struct-FIELD root at depth three — the B-2026-08-09-21 base
    // shape one level deeper than that row could reach.
    assert_eq!(
        run_program(
            "struct Grid { cells: Vec[Vec[Vec[i64]]] }\n\
                 fn main() {\n\
                 \x20   let mut g = Grid { cells: [[[9]]] };\n\
                 \x20   g.cells[0][0][0] = 11i64;\n\
                 \x20   println(f\"{g.cells[0][0][0]}\");\n\
                 }\n"
        )
        .as_deref(),
        Some("11\n")
    );
    // 6. COMPUTED indices at every level, and a neighbour left alone —
    // constant-folding a literal index would pass cases 1-5 while getting
    // this wrong.
    assert_eq!(
        run_program(
            "fn main() {\n\
                 \x20   let mut d: Vec[Vec[Vec[i64]]] = [[[0, 0]], [[0, 0]]];\n\
                 \x20   let i: i64 = 1;\n\
                 \x20   let j: i64 = 0;\n\
                 \x20   let k: i64 = 1;\n\
                 \x20   d[i][j][k] = 77i64;\n\
                 \x20   println(f\"{d[1][0][1]} {d[0][0][0]}\");\n\
                 }\n"
        )
        .as_deref(),
        Some("77 0\n")
    );
}

#[test]
fn test_e2e_index_through_a_slice_typed_tuple_field() {
    // 1. Read and write through a slice-typed tuple field.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let mut v: Vec[i64] = [1i64, 2i64, 3i64];\n\
                     let mut t: (mut Slice[i64], i64) = (v.as_slice_mut(), 5i64);\n\
                     let r: i64 = t.0[1];\n\
                     t.0[0] = 9i64;\n\
                     println(f\"{r} {v[0]} {v[1]}\");\n\
                 }"
        )
        .as_deref(),
        Some("2 9 2\n")
    );
    // 2. The motivating shape — writing through `split_at_mut` halves.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let mut buf: Vec[u8] = [0u8, 0u8, 0u8, 0u8];\n\
                     let mut p: (mut Slice[u8], mut Slice[u8]) = buf.split_at_mut(2i64);\n\
                     p.0[0] = 7u8;\n\
                     p.1[1] = 9u8;\n\
                     println(f\"{buf[0]} {buf[1]} {buf[2]} {buf[3]}\");\n\
                 }"
        )
        .as_deref(),
        Some("7 0 0 9\n")
    );
    // 3. CONTROL — a Vec-typed tuple field, which already worked.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let mut t: (Vec[i64], i64) = ([1i64, 2i64], 5i64);\n\
                     t.0[0] = 9i64;\n\
                     println(f\"{t.0[0]} {t.0[1]} {t.1}\");\n\
                 }"
        )
        .as_deref(),
        Some("9 2 5\n")
    );
}

#[test]
fn test_e2e_nested_index_store_releases_the_displaced_element() {
    // 1. The row's own repro.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let mut r: Vec[String] = Vec.new();\n\
                     r.push(f\"aa\"); r.push(f\"bb\");\n\
                     let mut d: Vec[Vec[String]] = Vec.new();\n\
                     d.push(r);\n\
                     d[0][0] = f\"zz\";\n\
                     println(f\"{d[0][0]} {d[0][1]}\");\n\
                 }"
        )
        .as_deref(),
        Some("zz bb\n")
    );
    // 2. Struct-FIELD base — the B-2026-08-09-21 route into this path.
    assert_eq!(
        run_program(
            "struct H { data: Vec[Vec[String]] }\n\
                 fn main() {\n\
                     let mut r: Vec[String] = Vec.new();\n\
                     r.push(f\"aa\"); r.push(f\"bb\");\n\
                     let mut h: H = H { data: Vec.new() };\n\
                     h.data.push(r);\n\
                     h.data[0][0] = f\"zz\";\n\
                     println(f\"{h.data[0][0]} {h.data[0][1]}\");\n\
                 }"
        )
        .as_deref(),
        Some("zz bb\n")
    );
    // 3. SELF-ALIAS — releasing before storing must not free the source.
    //
    // Spelled without an element-to-element store, which reads a non-`Copy`
    // element out of a container and is rejected now (B-2026-08-26-21). The
    // property under test is unchanged and both halves survive: `swap(0, 0)`
    // is the degenerate same-slot case that must not free anything, and the
    // clone-then-store still makes slot 1 hold slot 0's value while slot 0
    // stays alive — which is what "releasing before storing must not free
    // the source" means. The row goes through a temporary because
    // `d[0][0].clone()` is a chained indexed receiver, deferred in codegen.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let mut r: Vec[String] = Vec.new();\n\
                     r.push(f\"aa\"); r.push(f\"bb\");\n\
                     let mut d: Vec[Vec[String]] = Vec.new();\n\
                     d.push(r);\n\
                     d[0].swap(0, 0);\n\
                     let row: Vec[String] = d[0].clone();\n\
                     d[0][1] = row[0].clone();\n\
                     println(f\"{d[0][0]} {d[0][1]}\");\n\
                 }"
        )
        .as_deref(),
        Some("aa aa\n")
    );
    // 4. SCALAR leaf — no release applies; must be untouched.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let mut r: Vec[i64] = Vec.new();\n\
                     r.push(1i64); r.push(2i64);\n\
                     let mut d: Vec[Vec[i64]] = Vec.new();\n\
                     d.push(r);\n\
                     d[0][0] = 9i64;\n\
                     println(f\"{d[0][0]} {d[0][1]}\");\n\
                 }"
        )
        .as_deref(),
        Some("9 2\n")
    );
}

#[test]
fn test_e2e_vec_extend_from_slice_basic() {
    // Append a Vec[i64] to another Vec[i64]. Memcpy path —
    // single allocation, no per-element work.
    let out = run_program(
        r#"
fn main() {
    let src: Vec[i64] = Vec.filled(3, 7);
    let mut dst: Vec[i64] = Vec.with_capacity(8);
    dst.push(1);
    dst.push(2);
    dst.extend_from_slice(src);
    println(dst.len());
    println(dst[0]);
    println(dst[2]);
    println(dst[4]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "5\n1\n7\n7");
    }
}

#[test]
fn test_e2e_vec_extend_from_slice_nested_index_source() {
    // The kata-6 use case: source is `rows[r]` on
    // Vec[Vec[T]]. The fallback path in the extend_from_slice
    // arm compiles the Index expression directly, extracts
    // the inner Vec's {ptr, len} fields, and memcpys.
    let out = run_program(
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
    println(out[0]);
    println(out[2]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "3\n10\n30");
    }
}

#[test]
fn test_e2e_vec_from_slice_nested_index_source() {
    // Sibling to `extend_from_slice_nested_index_source` —
    // `Vec.from_slice(rows[r])` where rows is `Vec[Vec[T]]`.
    // Pre-fix this errored "source must currently be a named
    // slice / vec / array variable"; the new branch in
    // assoc_call.rs unwraps the outer Vec via vec_inner_type_expr
    // for the element type and compiles the Index expression
    // directly for the {data, len} extraction.
    let out = run_program(
        r#"
fn main() {
    let mut rows: Vec[Vec[i64]] = Vec.new();
    let mut r0: Vec[i64] = Vec.new();
    r0.push(7);
    r0.push(8);
    r0.push(9);
    rows.push(r0);
    let copy: Vec[i64] = Vec.from_slice(rows[0]);
    println(copy.len());
    println(copy[0]);
    println(copy[2]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "3\n7\n9");
    }
}

#[test]
fn test_e2e_deep_tuple_index_field_read_and_match() {
    // #25 (phase-12 self-hosting, B-2026-06-14-4) — reading a struct field
    // through a `<struct>.tuplefield.0.<field>` place chain. The element-0
    // struct's type wasn't resolved by `type_name_of_expr`'s `TupleIndex`
    // arm (Identifier-rooted only), so `field_index_for` found no field and
    // `compile_field_access` returned the `i64 0` placeholder — a scalar
    // field `h.ps.0.n` read 0 instead of 42, and an enum field
    // `match h.ps.0.tok { Id(s) => s.len() }` build-failed because the
    // scrutinee never resolved to its enum (so the arm binding `s` got no
    // String dispatch — "no handler for method 'len' on variable 's'"). Fix:
    // resolve the element struct type via the deep-chain walk
    // (`place_chain_tuple_tes`) for a non-Identifier-rooted tuple. The
    // match-arm payload consume is guardmalloc-clean (the existing #21
    // tuple-index match suppression cap-zeros the source); see
    // `asan_deep_tuple_index_match_no_double_free`. Element 1 (`h.ps.1`, a
    // scalar) always read correctly — only element-0-as-aggregate was broken.
    if let Some(out) = run_program(
        r#"
enum Tok { Id(String), Num(i64) }
struct Inner { tok: Tok, n: i64 }
struct Hs { ps: (Inner, i64) }
fn main() {
    let h = Hs { ps: (Inner { tok: Tok.Id("hello".to_string()), n: 42 }, 7) };
    // Scalar field through the tuple chain (was 0).
    println(h.ps.0.n.to_string());
    // Scalar second element (always worked — regression guard).
    println(h.ps.1.to_string());
    // Enum field as a match scrutinee — the arm binding dispatch (was build-fail).
    match h.ps.0.tok {
        Id(s) => { println(s.len().to_string()); }
        Num(n) => { println(n.to_string()); }
    }
    // Borrow arm (prints the payload directly).
    let h2 = Hs { ps: (Inner { tok: Tok.Id("world".to_string()), n: 1 }, 2) };
    match h2.ps.0.tok {
        Id(s) => { println(s); }
        Num(n) => { println(n.to_string()); }
    }
}
"#,
    ) {
        assert_eq!(out, "42\n7\n5\nworld\n");
    }
}

// ── Monotone-variable BCE — llvm.assume range facts ────────────────
// control_flow_bce.rs § monotone scan: a `let mut` cursor whose every
// in-loop write is `x = x ± <non-negative literal>` gets
// `llvm.assume(x >=/<= loop-entry value)` at body entry, letting LLVM
// fold bounds checks on conditionally-updated write heads (the
// kata-26/88 shape — docs/investigations/bce_monotonic_assume.md).
// Sound because AOT arithmetic traps on overflow (the update panics
// before a wrapped value could violate the assume).

#[test]
fn test_ir_monotone_index_var_emits_assume() {
    let ir = ir_for(
        r#"
fn dedup(v: mut Slice[i64], n: i64) -> i64 {
    let mut k = 1;
    for i in 1..n {
        if v[i] != v[k - 1] {
            v[k] = v[i];
            k = k + 1;
        }
    }
    k
}
"#,
    );
    assert!(
        ir.contains("llvm.assume"),
        "monotone index var must emit llvm.assume, IR:\n{ir}"
    );
    assert!(
        ir.contains("k.mono.fact"),
        "assume operand must carry the k.mono.fact label, IR:\n{ir}"
    );
    assert!(
        ir.contains("k.mono.init"),
        "preheader must load the loop-entry value, IR:\n{ir}"
    );
}

#[test]
fn test_e2e_nested_receiver_push_index_then_field() {
    // B-2026-07-11-11: a method on a NESTED place-expression receiver
    // (`o.inners[i].xs.push(v)` — a field of an INDEXED element of a field)
    // fell through method dispatch with "no handler for method push on
    // non-identifier receiver". `lower_field_access_ptr`'s Index arm now
    // hoists a FieldAccess container (`o.inners`) to a synth Vec identifier
    // before indexing, so the receiver pointer resolves through the whole
    // place chain (the sibling of the B-2026-07-09-1 hoist). Covers a
    // `mut ref self` root, multi-push, and read-back through the same chain.
    let out = run_program(
        r#"
struct Inner { xs: Vec[i64] }
struct Outer { inners: Vec[Inner] }
impl Outer {
    fn seed(mut ref self) {
        self.inners[0].xs.push(10);
        self.inners[0].xs.push(20);
        self.inners[1].xs.push(30);
    }
}
fn main() {
    let mut o = Outer { inners: Vec.new() };
    o.inners.push(Inner { xs: Vec.new() });
    o.inners.push(Inner { xs: Vec.new() });
    o.seed();
    println(o.inners[0].xs.len());
    println(o.inners[0].xs[1]);
    println(o.inners[1].xs[0]);
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["2", "20", "30"]);
    }
}

#[test]
fn test_e2e_field_index_read_plain_and_shared() {
    // FieldAccess-rooted index READ (`obj.field[i]`) — kata-133-audit
    // bug, 2026-06-05: previously the generic index tail compiled the
    // field access to a struct VALUE (Vec's `{ptr,len,cap}`) in a temp
    // alloca and died on "Index operator applied to non-array type",
    // for plain AND shared structs alike (the interpreter handled
    // both). Now routed through `lower_field_access_ptr` (the FR-slice
    // helper) + a synth identifier, so the existing identifier-keyed
    // Vec dispatch handles it — covering plain structs, shared
    // structs, `ref` params (deref shape), and `outer[i].field[j]`.
    let out = run_program(
        r#"
struct Holder { tag: i64, mut items: Vec[i64] }
shared struct Cell { mut vals: Vec[i64] }
fn read_ref(h: ref Holder) -> i64 { h.items[1] }
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(41);
    v.push(42);
    let h = Holder { tag: 7, items: v };
    println(h.items[0]);
    println(read_ref(h));

    let mut w: Vec[i64] = Vec.new();
    w.push(5);
    let c = Cell { vals: w };
    println(c.vals[0]);

    let mut outer: Vec[Holder] = Vec.new();
    let mut v2: Vec[i64] = Vec.new();
    v2.push(10);
    v2.push(20);
    outer.push(Holder { tag: 1, items: v2 });
    println(outer[0].items[1]);
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["41", "42", "5", "20"]);
    }
}

#[test]
fn test_e2e_field_index_store_plain_and_shared() {
    // FieldAccess-rooted index STORE (`obj.field[i] = v`) — the write
    // half of the kata-133-audit bug. Previously fell to the "Index
    // assignment target must be a variable" gate in
    // `compile_index_store`; the interpreter SILENTLY no-op'd the same
    // shape (`set_index`'s catch-all `_ => return` arm) — both fixed
    // in the same slice. Mirrors the read test's struct-kind coverage.
    let out = run_program(
        r#"
struct Holder { tag: i64, mut items: Vec[i64] }
shared struct Cell { mut vals: Vec[i64] }
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(41);
    v.push(42);
    let mut h = Holder { tag: 7, items: v };
    h.items[0] = 99;
    println(h.items[0]);
    println(h.items[1]);

    let mut w: Vec[i64] = Vec.new();
    w.push(5);
    let c = Cell { vals: w };
    c.vals[0] = 6;
    println(c.vals[0]);
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["99", "42", "6"]);
    }
}

#[test]
fn test_e2e_indexed_receiver_slice_path_len() {
    // The outer is a `mut Slice[Vec[i64]]` view; indexed-receiver
    // dispatch goes through the slice lowering path.
    let out = run_program(
        r#"
fn outer_lens(xs: mut Slice[Vec[i64]]) {
    println(xs[0].len());
    println(xs[1].len());
}
fn main() {
    let mut a: Vec[i64] = Vec.new();
    a.push(1);
    a.push(2);
    let mut b: Vec[i64] = Vec.new();
    b.push(10);
    b.push(20);
    b.push(30);
    let mut arr: Array[Vec[i64], 2] = [a, b];
    outer_lens(mut arr);
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["2", "3"]);
    }
}

#[test]
fn test_e2e_soa_field_index_store_strided() {
    // B-2026-06-20-7: field-level SoA index-store `bodies[i].field = expr`.
    // The destination must address the field's OWN group buffer at [i], by
    // the group sub-struct stride — not stride the SoA struct as a contiguous
    // AoS element. Before the fix the store fell into the nested-plain-struct
    // path which treated SoA as AoS: index 0 coincidentally hit group-0 slot
    // 0, but index >= 1 wrote PAST the group buffer (a silent heap overflow),
    // so the store was dropped. This exercises the `e.pos += e.vel` idiom
    // (a field store whose RHS reads two OTHER groups of the same element) and
    // reads back across a third group. bodies[0].x=0+1.5=1.5, bodies[1].x=
    // 10+2.5=12.5; total = (1.5+100)+(12.5+50) = 164. A dropped index-1 store
    // (the bug) would leave bodies[1].x=10 -> 161.5.
    let out = run_program(
        r#"
struct Body { x: f64, vx: f64, health: f64 }
layout bodies: Vec[Body] { group pos { x } group vel { vx } group hp { health } }
fn main() {
    let mut bodies: Vec[Body] = Vec.new();
    bodies.push(Body { x: 0.0, vx: 1.5, health: 100.0 });
    bodies.push(Body { x: 10.0, vx: 2.5, health: 50.0 });
    let mut i = 0;
    while i < bodies.len() {
        bodies[i].x = bodies[i].x + bodies[i].vx;
        i = i + 1;
    }
    let mut total = 0.0;
    let mut j = 0;
    while j < bodies.len() {
        total = total + bodies[j].x + bodies[j].health;
        j = j + 1;
    }
    println(total);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out.trim(),
            "164",
            "field-level SoA index-store must persist at every index (cross-group write + read)"
        );
    }
}

#[test]
fn test_e2e_soa_whole_element_index_store() {
    // Follow-on: WHOLE-element SoA index store `grid[i] = E { … }` (the
    // scatter-by-assignment a stateful kernel writes, the sibling of the
    // field-level store above). The RHS is the full AoS element; its fields
    // must scatter into each group's OWN buffer at [i], strided by the group
    // sub-struct — NOT written as one contiguous AoS element over group-0's
    // narrower stride. Before `compile_soa_index_store` the store fell into
    // `compile_vec_index_store`, which read the SoA 4-field struct's field-0
    // (the x-group buffer) as an AoS `{ptr,len,cap}` data pointer and wrote
    // 16-byte AoS elements over the 8-byte x-group stride — a silent heap
    // overflow that scrambled every group (this program printed 3, not 36).
    // grid[i] = {i+1, i*10}: (1+0)+(2+10)+(3+20) = 36.
    let out = run_program(
        r#"
struct E { x: f64, y: f64 }
layout grid: Vec[E] { group g1 { x } group g2 { y } }
fn main() with panics {
    let mut grid: Vec[E] = Vec.new();
    grid.push(E { x: 0.0, y: 0.0 });
    grid.push(E { x: 0.0, y: 0.0 });
    grid.push(E { x: 0.0, y: 0.0 });
    let mut i = 0;
    while i < grid.len() {
        grid[i] = E { x: (i + 1) as f64, y: (i * 10) as f64 };
        i = i + 1;
    }
    let mut s = 0.0;
    let mut j = 0;
    while j < grid.len() { s = s + grid[j].x + grid[j].y; j = j + 1; }
    println(s);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out.trim(),
            "36",
            "whole-element SoA index-store must scatter fields into per-group buffers at [i]"
        );
    }
}

#[test]
fn test_e2e_soa_whole_element_index_store_mut_ref() {
    // Follow-on: the exact shape the spike named as the open item — a kernel
    // that scatters WHOLE elements by `mut ref Vec[E]` index assignment
    // across a function boundary. The callee's slot holds a POINTER to the
    // caller's SoA struct; `compile_soa_index_store` derefs once (via
    // `ref_params`) before GEPing each group, so the writes land in the
    // caller's buffers. Three fields across two groups (one multi-field hot
    // group) proves the per-group decomposition crosses the boundary intact.
    // ps[i] = {i+1, i+2, i+3}: (1+2+3)+(2+3+4) = 15.
    let out = run_program(
        r#"
struct P { x: f64, y: f64, m: f64 }
layout ps: Vec[P] { group pos { x, y } group mass { m } }
fn scatter(ps: mut ref Vec[P]) {
    let mut i = 0;
    while i < ps.len() {
        ps[i] = P { x: (i + 1) as f64, y: (i + 2) as f64, m: (i + 3) as f64 };
        i = i + 1;
    }
}
fn main() with panics {
    let mut ps: Vec[P] = Vec.new();
    ps.push(P { x: 0.0, y: 0.0, m: 0.0 });
    ps.push(P { x: 0.0, y: 0.0, m: 0.0 });
    scatter(mut ps);
    let mut s = 0.0;
    let mut j = 0;
    while j < ps.len() { s = s + ps[j].x + ps[j].y + ps[j].m; j = j + 1; }
    println(s);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out.trim(),
            "15",
            "whole-element SoA scatter through `mut ref Vec[E]` must write the caller's groups"
        );
    }
}

#[test]
fn test_e2e_soa_whole_element_index_store_cold_group() {
    // Follow-on: whole-element SoA index store into a layout WITH a cold
    // group — the scatter must also reach the trailing cold allocation (the
    // separate-malloc branch), not just the hot groups. `vy` lives in cold,
    // `x`/`y` in one hot group, `vx` in another; `entities[j] = {j,j+1,j+2,
    // j+3}` then reads back from every buffer. entities[4] -> 4,5,6,7;
    // entities[2].vy -> 5.
    let src = r#"
struct Entity { x: i64, y: i64, vx: i64, vy: i64 }
layout entities: Vec[Entity] {
    group pos { x, y }
    group vel { vx }
    cold { vy }
}
fn main() with panics {
    let mut entities: Vec[Entity] = Vec.new();
    let mut i: i64 = 0;
    while i < 5 { entities.push(Entity { x: 0, y: 0, vx: 0, vy: 0 }); i = i + 1; }
    let mut j: i64 = 0;
    while j < 5 {
        entities[j] = Entity { x: j, y: j + 1, vx: j + 2, vy: j + 3 };
        j = j + 1;
    }
    let e = entities[4];
    println(e.x); println(e.y); println(e.vx); println(e.vy);
    println(entities[2].vy);
}
"#;
    if let Some(out) = run_program(src) {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(
            lines,
            vec!["4", "5", "6", "7", "5"],
            "whole-element SoA store must scatter into the cold buffer too"
        );
    }
}

// ── Slice[T] end-to-end ────────────────────────────────────────

#[test]
fn test_e2e_slice_sum_over_array_coercion() {
    let out = run_program(
        r#"
fn sum(xs: Slice[i64]) -> i64 {
    let mut acc = 0;
    for x in xs { acc = acc + x; }
    acc
}
fn main() {
    let a: Array[i64, 4] = [1, 2, 3, 4];
    println(sum(a));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "10");
    }
}

#[test]
fn test_e2e_slice_element_index() {
    let out = run_program(
        r#"
fn second(xs: Slice[i64]) -> i64 { xs[1] }
fn main() {
    let a: Array[i64, 3] = [7, 8, 9];
    println(second(a));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "8");
    }
}

#[test]
fn test_e2e_as_slice_on_array() {
    let out = run_program(
        r#"
fn sum(xs: Slice[i64]) -> i64 {
    let mut acc = 0;
    for x in xs { acc = acc + x; }
    acc
}
fn main() {
    let a: Array[i64, 3] = [42, 100, 200];
    let s = a.as_slice();
    println(sum(s));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "342");
    }
}

#[test]
fn test_e2e_slice_len_after_array_coercion() {
    // Regression: `Slice.len()` had no codegen handler — fell through to
    // the dispatcher's silent-`0` catch-all (line ~4163 pre-fix). Manifested
    // as `Slice.len() == 0` on any slice constructed by Array → Slice
    // coercion at a call site. See docs/known_bugs.md § B1.
    let out = run_program(
        r#"
fn dump(xs: Slice[i64]) {
    println(xs.len());
    println(xs[0]);
    println(xs[3]);
}
fn main() {
    let a: Array[i64, 4] = [2, 7, 11, 15];
    dump(a);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "4\n2\n15");
    }
}

#[test]
fn test_e2e_slice_len_through_nested_call() {
    // Companion regression: `Slice.len()` correct after Slice → Slice
    // forwarding (matches the LeetCode #1 `report` → `two_sum` shape that
    // exposed the bug originally).
    let out = run_program(
        r#"
fn inner(xs: Slice[i64]) -> i64 { xs[0] + xs[1] }
fn outer(xs: Slice[i64]) {
    println(inner(xs));
    println(xs.len());
}
fn main() {
    let a: Array[i64, 4] = [2, 7, 11, 15];
    outer(a);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "9\n4");
    }
}

#[test]
fn test_e2e_slice_is_empty() {
    // `Slice.is_empty()` shares the dispatcher path with `len`. Pre-fix
    // it fell through to the silent-`0` catch-all so both empty and
    // non-empty slices reported "empty" (i1 zero). Use the bool directly
    // as a return value to avoid hitting the unrelated empty-array-literal
    // gap and the if-as-statement Unit-coercion path.
    let out = run_program(
        r#"
fn empty_flag(xs: Slice[i64]) -> bool { xs.is_empty() }
fn main() {
    let a: Array[i64, 3] = [1, 2, 3];
    let r = empty_flag(a);
    if r { println(1); } else { println(0); }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "0");
    }
}

#[test]
fn test_e2e_slice_brute_force_two_sum() {
    // End-to-end: the LeetCode #1 brute-force shape that exposed B1.
    // Pre-fix: returned [-1, -1] regardless of input because `nums.len()`
    // returned 0 → for-loop never executed.
    let out = run_program(
        r#"
fn two_sum(nums: Slice[i64], target: i64) -> Array[i64, 2] {
    let n = nums.len();
    for i in 0..n {
        for j in (i + 1)..n {
            if nums[i] + nums[j] == target {
                return [i, j];
            }
        }
    }
    [-1, -1]
}
fn main() {
    let nums: Array[i64, 4] = [2, 7, 11, 15];
    let r = two_sum(nums, 9);
    println(r[0]);
    println(r[1]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "0\n1");
    }
}

#[test]
fn test_e2e_mut_slice_indexing_writes_back() {
    // `mut Slice[T]` indexing writes through the slice's data pointer.
    // When the slice aliases an Array on the caller's stack, the write
    // should be observable in that Array.
    let out = run_program(
        r#"
fn set_first(xs: mut Slice[i64]) {
    xs[0] = 99;
}
fn main() {
    let mut a: Array[i64, 3] = [1, 2, 3];
    set_first(mut a);
    println(a[0]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "99");
    }
}

#[test]
fn test_e2e_slice_from_vec_coercion() {
    let out = run_program(
        r#"
fn sum(xs: Slice[i64]) -> i64 {
    let mut acc = 0;
    for x in xs { acc = acc + x; }
    acc
}
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(100);
    v.push(200);
    v.push(300);
    println(sum(v));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "600");
    }
}

// ── Slice.get_unchecked — unsafe direct-index, no bounds check ──────
//
// The Slice mirror of Vec.get_unchecked: the escape hatch for hot
// scanners (KMP `needle[j]`, merge `nums1[k]`) where the in-range fact
// is programmer-provable but not compiler-provable. Returns T by value;
// sound for the Copy element types scanners use. Tested in NON-reduction
// loops — slice get_unchecked inside an auto-par REDUCTION worker is a
// tracked gap (captured slices don't register in the worker's
// slice_elem_types; use `xs[i]` or KARAC_AUTO_PAR=0 there). See
// phase-7-codegen.md § BCE table-range tier.

#[test]
fn test_e2e_slice_get_unchecked_in_bounds_returns_element() {
    // `at` borrows the slice (`ref Slice[i64]`) so the same slice can be
    // read across three calls without an ownership-move error — the
    // check-clean shape enabled by the B-2026-07-02-28 ref-Slice
    // get_unchecked codegen deref fix.
    let out = run_program(
        r#"
fn at(xs: ref Slice[i64], i: i64) -> i64 {
    unsafe { xs.get_unchecked(i) }
}
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(10);
    v.push(20);
    v.push(30);
    let s: Slice[i64] = v.as_slice();
    println(at(s, 0));
    println(at(s, 1));
    println(at(s, 2));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "10\n20\n30");
    }
}

#[test]
fn test_e2e_slice_get_unchecked_in_scan_loop() {
    // KMP-shaped use: read a Slice element with the bounds check skipped
    // inside a non-reduction while loop (the real scanner pattern).
    let out = run_program(
        r#"
fn scan(xs: Slice[i64], n: i64) {
    let mut i = 0i64;
    while i < n {
        // SAFETY: i < n <= xs.len() by construction at the call site.
        println(unsafe { xs.get_unchecked(i) });
        i = i + 1i64;
    }
}
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(5);
    v.push(15);
    v.push(25);
    scan(v, 3);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "5\n15\n25");
    }
}

#[test]
fn test_e2e_bounds_elision_slice_under_while_guard() {
    // Same elision pass widened to `Slice[T]` indexed reads. The pass
    // mirrors compile_vec_index's wiring through emit_split_bounds_check
    // with the Slice's struct type. Output correctness is the gate;
    // the perf impact varies by workload (kata-88's pattern is neutral
    // because its bounds aren't expressible from source guards; kata-5's
    // would benefit if its expand function took Slice instead of Vec).
    let out = run_program(
        r#"
fn sum_first(xs: Slice[i64], k: i64) -> i64 {
    let mut i = 0i64;
    let mut acc = 0i64;
    let n = xs.len();
    while i >= 0 and i < n and i < k {
        acc = acc + xs[i];
        i = i + 1;
    }
    acc
}
fn main() {
    let arr: Array[i64, 5] = [10, 20, 30, 40, 50];
    println(sum_first(arr, 3));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "60");
    }
}

// ── Method dispatcher hardening (regression) ────────────────────────

#[test]
fn test_codegen_rejects_unsupported_slice_method() {
    // Regression: `compile_method_call` used to silently return
    // const-0 for any method it didn't know how to dispatch (the
    // 2026-05-04 `Slice.len()` wrong-answer bug came from this).
    // Both fall-through sites now return a typed `Err`.
    //
    // THE PROBE IS NO LONGER A REAL METHOD, because there is no longer a
    // real one to use. This test cycled through `first()` (until
    // B-2026-08-14-8 routed the read accessors) and then `windows()` (until
    // B-2026-08-14-9 implemented the mutators and view-producers), each
    // time on the comment's own instruction to "swap this to any other
    // typechecker-accepted method without a codegen arm". Every name in
    // `SLICE_BUILTIN_METHODS` now has one, so the probe is a name the
    // TYPECHECKER rejects instead — which still reaches the dispatcher,
    // because this harness drives codegen past typecheck errors, and still
    // pins the only thing the test was ever about: the fall-through must
    // return an `Err` that names the method, never a silent zero.
    let src = r#"
fn main() {
    let xs: Array[i64, 3] = [1, 2, 3];
    let s: Slice[i64] = xs.as_slice();
    let _ = s.no_such_slice_method(2i64);
}
"#;
    let mut parsed = karac::parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    karac::prepare_for_resolve(&mut parsed.program);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);
    let err = compile_to_ir(&parsed.program, None, None)
        .expect_err(
            "expected codegen to Err on unsupported slice method; \
             the dispatcher silent-zero must not be re-introduced",
        )
        .message;
    assert!(
        err.contains("no_such_slice_method"),
        "expected diagnostic to name the missing slice method; got: {}",
        err
    );
}

/// B-2026-08-14-9 — the eight `Slice[T]` methods that typechecked and (some
/// of them) interpreted but had no codegen at all: the mutators
/// `fill`/`reverse`/`sort`/`sort_by_key`/`swap` and the view-producers
/// `chunks`/`windows`/`split_at`. Every line here was
/// `codegen: no handler for slice method '<m>'` before.
///
/// FOUR of them are routed rather than reimplemented, extending
/// B-2026-08-14-8's borrowed `{ptr, len, cap: 0}` Vec view. That row held
/// the mutators back because `cap == 0` is a lie to anything that could
/// grow — true of `push`, and the reason it must never come this way, but
/// `reverse` / `sort` / `sort_by` / `sort_by_key` are permutations of
/// `[0, len)` that never touch field 2. The write lands in the caller's
/// buffer because the view aliases it.
///
/// `swap` and `fill` have no Vec arm to route to and are implemented over
/// the 2-field header. `chunks` / `windows` build a `Vec` of slice headers
/// borrowing the receiver, and `split_at` shares the existing
/// `split_at_mut` arm — its halves may alias, which is what the
/// interpreter has always done and what makes an immutable view free.
///
/// Line 06 is the one that would catch a header-offset mistake: the
/// receiver is the SECOND half of a `split_at_mut`, so its `ptr` is not the
/// collection's base and a fix that sorted from index 0 would corrupt the
/// untouched first half. Lines 07-08 pin the narrow-element stride, 11 and
/// 14 the degenerate counts (`n` larger than the slice, an empty
/// receiver), 13 a zero-length half.
#[test]
fn test_e2e_slice_mutators_and_view_producers() {
    let src = r#"
fn msort(xs: mut Slice[i64]) { xs.sort(); }
fn mrev(xs: mut Slice[i64]) { xs.reverse(); }
fn mfill(xs: mut Slice[i64]) { xs.fill(9i64); }
fn mswap(xs: mut Slice[i64]) { xs.swap(0i64, 1i64); }
fn mkey(xs: mut Slice[i64]) { xs.sort_by_key(|x| 0i64 - x); }
fn mby(xs: mut Slice[i64]) { xs.sort_by(|a, b| b.cmp(a)); }
fn sfill(xs: mut Slice[u8]) { xs.fill(200u8); }
fn ssort(xs: mut Slice[u8]) { xs.sort(); }

fn main() {
    let mut a: Vec[i64] = Vec.new();
    a.push(3i64); a.push(1i64); a.push(2i64); a.push(5i64);
    msort(a.as_slice_mut());
    println(f"01 {a[0i64]} {a[1i64]} {a[2i64]} {a[3i64]}");
    mrev(a.as_slice_mut());
    println(f"02 {a[0i64]} {a[1i64]} {a[2i64]} {a[3i64]}");
    mkey(a.as_slice_mut());
    println(f"03 {a[0i64]} {a[1i64]} {a[2i64]} {a[3i64]}");
    mswap(a.as_slice_mut());
    println(f"04 {a[0i64]} {a[1i64]} {a[2i64]} {a[3i64]}");
    mby(a.as_slice_mut());
    println(f"05 {a[0i64]} {a[1i64]} {a[2i64]} {a[3i64]}");

    let mut b: Vec[i64] = Vec.new();
    b.push(9i64); b.push(8i64); b.push(3i64); b.push(1i64); b.push(2i64);
    let mut bs = b.as_slice_mut();
    let h = bs.split_at_mut(2i64);
    msort(h.1);
    println(f"06 {b[0i64]} {b[1i64]} {b[2i64]} {b[3i64]} {b[4i64]}");

    let mut u: Vec[u8] = Vec.new();
    u.push(3u8); u.push(1u8); u.push(2u8);
    ssort(u.as_slice_mut());
    println(f"07 {u[0i64]} {u[1i64]} {u[2i64]}");
    sfill(u.as_slice_mut());
    println(f"08 {u[0i64]} {u[1i64]} {u[2i64]}");

    let mut v: Vec[i64] = Vec.new();
    v.push(3i64); v.push(1i64); v.push(2i64); v.push(5i64); v.push(4i64);
    let s = v.as_slice();
    let cs = s.chunks(2i64);
    let c0 = cs[0i64];
    let c2 = cs[2i64];
    println(f"09 {cs.len()} {c0.len()} {c0[0i64]} {c0[1i64]} {c2.len()} {c2[0i64]}");
    let ws = s.windows(3i64);
    let w1 = ws[1i64];
    println(f"10 {ws.len()} {w1.len()} {w1[0i64]} {w1[1i64]} {w1[2i64]}");
    println(f"11 {s.chunks(1i64).len()} {s.chunks(9i64).len()} {s.windows(5i64).len()} {s.windows(9i64).len()}");
    let p = s.split_at(2i64);
    println(f"12 {p.0.len()} {p.1.len()} {p.0[0i64]} {p.1[0i64]}");
    let q = s.split_at(0i64);
    println(f"13 {q.0.len()} {q.1.len()}");

    let e: Vec[i64] = Vec.new();
    let es = e.as_slice();
    println(f"14 {es.chunks(2i64).len()} {es.windows(2i64).len()} {es.split_at(0i64).0.len()}");

    let mut m: Vec[i64] = Vec.new();
    m.push(1i64); m.push(2i64); m.push(3i64);
    mswap(m.as_slice_mut());
    println(f"15 {m[0i64]} {m[1i64]} {m[2i64]}");
    mfill(m.as_slice_mut());
    println(f"16 {m[0i64]} {m[1i64]} {m[2i64]}");
}
"#;
    assert_eq!(
        run_program(src).as_deref(),
        Some(
            "01 1 2 3 5\n\
                 02 5 3 2 1\n\
                 03 5 3 2 1\n\
                 04 3 5 2 1\n\
                 05 5 3 2 1\n\
                 06 9 8 1 2 3\n\
                 07 1 2 3\n\
                 08 200 200 200\n\
                 09 3 2 3 1 1 4\n\
                 10 3 3 1 2 5\n\
                 11 5 1 1 0\n\
                 12 2 3 3 2\n\
                 13 0 5\n\
                 14 0 0 0\n\
                 15 2 1 3\n\
                 16 9 9 9\n"
        ),
    );
}

/// B-2026-08-14-37 — a read-only `Slice[T]` formal borrows, so the caller
/// keeps its container.
///
/// The ownership half is checked in `tests/ownership.rs` and the
/// once-callability half in `tests/closures.rs`; what this pins is that
/// the programs those two now ACCEPT actually run, and run correctly.
/// That matters in both directions. Line 03 is the shape that was a hard
/// `E_ONCE_FN_INTO_FN_SLOT` error — it could not be built at all — and
/// lines 01/02/05 are the ones that merely warned, which means codegen
/// was already compiling them with a `use_after_move_consume_sites`
/// defensive copy at each flagged reuse. Reclassifying the argument as a
/// borrow removes those copies, so the values printed here are the
/// evidence that removing them did not change any answer.
#[test]
fn test_e2e_read_only_slice_param_borrows_its_argument() {
    let src = r#"
fn count(xs: Slice[String]) -> i64 { xs.len() }
fn scount(xs: Slice[i64]) -> i64 { xs.len() }
fn total(xs: Slice[i64]) -> i64 panics {
    let mut t = 0i64;
    let mut i = 0i64;
    while i < xs.len() { t = t + xs[i]; i = i + 1i64; }
    t
}
fn twice(f: Fn() -> i64) -> i64 { f() + f() }

fn main() {
    let words: Vec[String] = ["alphabetical", "betamax", "gamma-ray-burst"];
    println(f"01 {count(words)} {count(words)} {words[0i64]} {words.len()}");

    let nums: Vec[i64] = [10i64, 20i64, 30i64];
    println(f"02 {total(nums)} {total(nums)} {total(nums)} {nums[2i64]}");

    println(f"03 {twice(|| scount(nums))}");

    println(f"04 {total(nums.as_slice())} {total(nums[0..2])}");

    println(f"05 {nums.len()} {words[2i64]}");
}
"#;
    assert_eq!(
        run_program(src).as_deref(),
        Some(
            "01 3 3 alphabetical 3\n\
                 02 60 60 60 30\n\
                 03 6\n\
                 04 60 30\n\
                 05 3 gamma-ray-burst\n"
        ),
    );
}

#[test]
fn test_pattern_bound_vec_payload_index_read_and_is_empty_direct() {
    // Index-read and `.is_empty()` directly on the pattern binding.
    // Pre-PB these dispatched through the same generic fallback as
    // `.len()` and either silently produced wrong codegen or failed
    // with a "no handler" diagnostic. Post-PB the binding-name → Vec
    // element-type registration lights up both paths in one go (the
    // registry is shared across all Vec method dispatchers).
    //
    // `xs.push(...)` on the pattern binding directly is still
    // off-limits because the parser binds tuple-variant pattern
    // names without a mut bit (`mut xs` isn't part of the surface
    // pattern grammar today), and the conventional `let mut xs2 =
    // xs;` rebind exercises a separate let-from-Identifier
    // propagation gap that's outside this slice's scope. Mutation
    // tests on pattern-bound collections wait until either pattern
    // mut bindings or let-from-Identifier propagation lands.
    let out = run_program(
        r#"
enum E { V(Vec[i64]) }
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(100);
    v.push(200);
    v.push(300);
    let e = V(v);
    match e {
        V(xs) => {
            println(xs[0]);
            println(xs[1]);
            println(xs[2]);
            if xs.is_empty() {
                println(0);
            } else {
                println(1);
            }
        }
    }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["100", "200", "300", "1"]);
    }
}

// ── Slice / array patterns (phase-5 § Slice and array patterns — sub-item 4)

#[test]
fn test_e2e_slice_pattern_empty_matches_empty_vec() {
    let out = run_program(
        r#"
fn label(v: Vec[i64]) -> String {
    match v {
        [] => "empty",
        _ => "non-empty",
    }
}
fn main() {
    let a: Vec[i64] = Vec.new();
    let mut b: Vec[i64] = Vec.new();
    b.push(7);
    println(label(a));
    println(label(b));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "empty\nnon-empty\n");
    }
}

/// B-2026-07-14-13: a slice pattern matched on a `ref Vec[T]` PARAM
/// mis-dispatched — the `ref` param's alloca holds a POINTER to the
/// caller's `{ptr,len,cap}`, but `resolve_slice_source` GEP'd the borrow
/// pointer's own bits as the struct, so the slice LENGTH came out garbage
/// and every arm's length check took the wrong branch (JIT/native gave
/// wrong answers; the interpreter was correct). Fixed by dereferencing the
/// borrow before reading data/len. The owned-param case above already
/// worked (slot IS the struct); this pins the `ref`-param case.
#[test]
fn test_e2e_slice_pattern_on_ref_vec_param() {
    let out = run_program(
        r#"
fn label(v: ref Vec[i64]) -> String {
    match v {
        [] => "empty",
        [a, b] => "pair",
        _ => "other",
    }
}
fn sum2(v: ref Vec[i64]) -> i64 {
    match v {
        [a, b] => a + b,
        _ => -1,
    }
}
fn main() {
    let e: Vec[i64] = Vec.new();
    let mut p: Vec[i64] = Vec.new();
    p.push(3);
    p.push(4);
    let mut t: Vec[i64] = Vec.new();
    t.push(1);
    t.push(2);
    t.push(3);
    println(label(e));
    println(label(p));
    println(label(t));
    println(f"{sum2(p)}");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "empty\npair\nother\n7\n");
    }
}

#[test]
fn test_e2e_slice_pattern_single_element_fixed_arity_array() {
    let out = run_program(
        r#"
fn main() {
    let a: Array[i64, 1] = [42];
    let [x] = a;
    println(x);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "42\n");
    }
}

#[test]
fn test_e2e_slice_pattern_fixed_arity_let_binds_all_elements() {
    let out = run_program(
        r#"
fn main() {
    let arr: Array[i64, 3] = [10, 20, 30];
    let [a, b, c] = arr;
    println(a);
    println(b);
    println(c);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "10\n20\n30\n");
    }
}

#[test]
fn test_e2e_slice_pattern_head_only_ignored_rest_on_vec() {
    let out = run_program(
        r#"
fn head_or(v: Vec[i64], default: i64) -> i64 {
    match v {
        [first, ..] => first,
        [] => default,
    }
}
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(10);
    v.push(20);
    v.push(30);
    let empty: Vec[i64] = Vec.new();
    println(head_or(v, -1));
    println(head_or(empty, -1));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "10\n-1\n");
    }
}

#[test]
fn test_e2e_slice_pattern_tail_only_ignored_rest_on_vec() {
    let out = run_program(
        r#"
fn last_or(v: Vec[i64], default: i64) -> i64 {
    match v {
        [.., last] => last,
        [] => default,
    }
}
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(10);
    v.push(20);
    v.push(30);
    println(last_or(v, -1));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "30\n");
    }
}

#[test]
fn test_e2e_slice_pattern_both_ends_ignored_rest_on_vec() {
    let out = run_program(
        r#"
fn ends(v: Vec[i64]) -> i64 {
    match v {
        [first, .., last] => first + last,
        [only] => only,
        [] => -1,
    }
}
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1);
    v.push(2);
    v.push(3);
    v.push(4);
    v.push(5);
    println(ends(v));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "6\n");
    }
}

#[test]
fn test_e2e_slice_pattern_single_bound_rest_at_tail_array() {
    let out = run_program(
        r#"
fn main() {
    let arr: Array[i64, 5] = [10, 20, 30, 40, 50];
    let [first, ..rest] = arr;
    println(first);
    println(rest.len());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "10\n4\n");
    }
}

#[test]
fn test_e2e_slice_pattern_single_bound_rest_at_head_array() {
    let out = run_program(
        r#"
fn main() {
    let arr: Array[i64, 4] = [10, 20, 30, 40];
    let [..rest, last] = arr;
    println(rest.len());
    println(last);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "3\n40\n");
    }
}

#[test]
fn test_e2e_slice_pattern_two_bound_middle_rest_array() {
    let out = run_program(
        r#"
fn main() {
    let arr: Array[i64, 5] = [1, 2, 3, 4, 5];
    let [first, ..mid, last] = arr;
    println(first);
    println(mid.len());
    println(last);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "1\n3\n5\n");
    }
}

#[test]
fn test_e2e_slice_pattern_multi_element_prefix_and_suffix_array() {
    let out = run_program(
        r#"
fn main() {
    let arr: Array[i64, 6] = [10, 20, 30, 40, 50, 60];
    let [a, b, .., y, z] = arr;
    println(a);
    println(b);
    println(y);
    println(z);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "10\n20\n50\n60\n");
    }
}

#[test]
fn test_e2e_slice_pattern_rest_binding_indexing_on_vec() {
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(7);
    v.push(8);
    v.push(9);
    v.push(10);
    match v {
        [_, ..rest] => {
            println(rest.len());
            println(rest[0]);
            println(rest[1]);
            println(rest[2]);
        },
        [] => println(-1),
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "3\n8\n9\n10\n");
    }
}

#[test]
fn test_slice_8w_per_mono_destructor_still_skipped_for_primitive_type_arg() {
    // Regression guard: the slice 8w None-fallback must NOT
    // emit a destructor when the resolved LLVM type is a plain
    // integer (or any non-Vec-struct shape). Primitive-only
    // monos (`T = i64`) still skip-when-empty.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver[T](item: T) { fetch(); }
             fn caller() { driver(42i64); }",
    );
    assert!(
        !ir.contains("@\"__kara_state_drop_driver$i64\""),
        "per-mono destructor must skip for i64 T:\n{ir}"
    );
}

#[test]
fn test_slice_8w_two_monos_only_heap_bearing_one_gets_destructor() {
    // `driver[T]` instantiated with both i64 (primitive — skip)
    // and String (Vec-struct shape — emit). The per-mono
    // destructors emit independently under each mangled key,
    // and the i64 mono's destructor stays absent (skip-when-
    // empty per slice 8u). Pins the asymmetric heap-bearingness
    // across monos at the classification level.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver[T](item: T) { fetch(); }
             fn caller() {
                 driver(42i64);
                 let s = String.new();
                 driver(s);
             }",
    );
    assert!(
        !ir.contains("@\"__kara_state_drop_driver$i64\""),
        "i64 mono must not emit a destructor:\n{ir}"
    );
    assert!(
        ir.contains("@\"__kara_state_drop_driver$struct$T_ct_String\""),
        "String mono must emit a destructor:\n{ir}"
    );
}

#[test]
fn test_slice_8w_non_generic_with_recorded_typename_unchanged() {
    // Regression guard for the slice 8w None-fallback: a
    // non-generic yielding fn whose captured local has a
    // recorded `type_name` (e.g. `items: Vec[i64]` records
    // `Some("Vec")`) still classifies via the existing direct
    // `Some(name)` arm — the None-fallback path doesn't fire
    // here. The destructor emits as it did pre-8w.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver(items: Vec[i64]) { fetch(); }",
    );
    let body = extract_fn_ir(&ir, "__kara_state_drop_driver");
    assert!(
        body.contains("%items.drop.is_heap = icmp sgt i64 %items.drop.cap, 0"),
        "non-generic Vec captured local still emits cap > 0 check:\n{body}"
    );
    assert!(
        body.contains("call void @free(ptr %items.drop.data)"),
        "non-generic Vec captured local still emits free:\n{body}"
    );
}

// ── Phase 6 line 26 slice 8x: body-walk for generic-typed let-bindings ──
//
// Slice 8v Phase 2 / 8w resolved type-parameter-typed *parameters*
// through `lookup_param_type_expr` against the active `type_subst`,
// but a `let copy = item;` introduced inside a `T`-typed yielding
// fn body whose binding is captured across a later yield still
// fell through to the i64 default — the typechecker's
// `pattern_binding_types` recorder emits `type_name: None` for
// type-parameter-typed bindings whose RHS is itself a
// type-parameter-typed identifier, and the `None`-fallback chain
// only consulted the parameter list. Slice 8x widens that chain
// with `lookup_let_type_expr`, which walks `fn_ast.body`
// recursively for a body-level `let`-binding and returns its
// explicit type annotation (if any) or — for bare-identifier
// RHS — recursively resolves through `lookup_param_type_expr`
// (params take priority) then this helper (chained let-bindings).

#[test]
fn test_slice_8x_per_mono_state_struct_lowers_let_binding_for_vec_t() {
    // `fn driver[T](item: T) { fetch(); let copy = item; fetch();
    // }` instantiated with `T = Vec[i64]`. Both `item` (param)
    // and `copy` (body-level let) are in scope at the second
    // yield, so the state-struct layout records both with
    // `type_name: None`. Slice 8w handled `item` via the param
    // lookup; slice 8x handles `copy` via the new let lookup
    // (bare-identifier RHS resolves through `item`'s parameter
    // type, which `type_subst` lowers to the Vec struct shape).
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver[T](item: T) { fetch(); let copy = item; fetch(); }
             fn caller() {
                 let v: Vec[i64] = Vec.new();
                 driver(v);
             }",
    );
    // Per-mono state struct line carries two Vec struct shapes
    // (one for `item`, one for `copy`) alongside the i32 tag.
    let line = ir
        .lines()
        .find(|l| l.starts_with("%\"kara.state.driver$struct$T_ct_Vec_i64\" = type {"))
        .unwrap_or_else(|| panic!("no per-mono state struct type def in IR:\n{ir}"));
    let vec_shape = "{ ptr, i64, i64 }";
    let occurrences = line.matches(vec_shape).count();
    assert!(
            occurrences >= 2,
            "expected two Vec-struct fields (item + copy) in per-mono state struct line, got {occurrences}:\n{line}"
        );
}

#[test]
fn test_slice_8x_per_mono_destructor_emits_free_for_let_binding() {
    // Same source as above. The destructor body must walk both
    // captured-local fields in source-introduction order — `item`
    // (param) first, `copy` (body let) second — and emit
    // `cap > 0 ? free` for each. Without slice 8x, the `copy`
    // field's classification would fall through to `Skip` and
    // its heap buffer would leak on cancel/Err unwinding.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver[T](item: T) { fetch(); let copy = item; fetch(); }
             fn caller() {
                 let v: Vec[i64] = Vec.new();
                 driver(v);
             }",
    );
    assert!(
        ir.contains("@\"__kara_state_drop_driver$struct$T_ct_Vec_i64\""),
        "per-mono destructor must emit for Vec-typed T captured + let:\n{ir}"
    );
    // Both fields emit the `cap > 0 ? free` shape.
    for field_name in ["item", "copy"] {
        assert!(
            ir.contains(&format!("%{field_name}.drop.cap = load i64")),
            "destructor must load cap for `{field_name}` field:\n{ir}"
        );
        assert!(
            ir.contains(&format!(
                "%{field_name}.drop.is_heap = icmp sgt i64 %{field_name}.drop.cap, 0"
            )),
            "destructor must compare cap > 0 for `{field_name}` field:\n{ir}"
        );
        assert!(
            ir.contains(&format!("call void @free(ptr %{field_name}.drop.data)")),
            "destructor must call free on `{field_name}.drop.data`:\n{ir}"
        );
    }
    // Source order: `item.drop.cap` precedes `copy.drop.cap` in
    // the IR text — pins the strict source-introduction order
    // discipline the field walk inherited from slice 8u.
    let item_pos = ir
        .find("%item.drop.cap")
        .expect("item.drop.cap must appear");
    let copy_pos = ir
        .find("%copy.drop.cap")
        .expect("copy.drop.cap must appear");
    assert!(
        item_pos < copy_pos,
        "destructor must walk fields in source order (item before copy):\n{ir}"
    );
}

#[test]
fn test_slice_8x_explicit_let_annotation_resolves_through_type_subst() {
    // `let copy: T = item;` with an explicit annotation hits the
    // primary `lookup_let_type_expr` path (returns the annotation
    // directly without recursing through the RHS chain). With
    // `T = String`, the annotation resolves through `type_subst`
    // to the Vec struct shape and the destructor's `cap > 0 ? free`
    // shape emits for `copy` alongside `item`.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver[T](item: T) { fetch(); let copy: T = item; fetch(); }
             fn caller() {
                 let s = String.new();
                 driver(s);
             }",
    );
    assert!(
        ir.contains("@\"__kara_state_drop_driver$struct$T_ct_String\""),
        "explicit-annotation let must also produce per-mono destructor:\n{ir}"
    );
    assert!(
            ir.contains("%copy.drop.cap = load i64"),
            "explicit annotation `let copy: T = item;` must classify as VecOrString for T=String:\n{ir}"
        );
    assert!(
        ir.contains("call void @free(ptr %copy.drop.data)"),
        "explicit-annotation let must emit free for copy field:\n{ir}"
    );
}

#[test]
fn test_slice_8x_primitive_t_let_binding_still_no_destructor() {
    // Regression guard for skip-when-empty: with `T = i64`, both
    // `item` and `copy` resolve to primitive int — neither field
    // classifies as `VecOrString` (Vec-struct identity check
    // fails) — so the destructor stays absent. Mirrors the
    // slice 8w primitive-only guard for the body-let surface.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver[T](item: T) { fetch(); let copy = item; fetch(); }
             fn caller() { driver(42i64); }",
    );
    assert!(
        !ir.contains("@\"__kara_state_drop_driver$i64\""),
        "primitive-T let-binding mono must not emit a destructor:\n{ir}"
    );
}

// ── Phase 6 line 26 slice 8z: ref / slice-elem params via per-mono intercept ──
//
// Slice 8v Phase 2's per-mono caller-side intercept (in
// `compile_generic_call`, `src/codegen/mono.rs`) stored compiled
// `arg_vals[i]` values directly into state-struct captured-local
// fields. For owned-value params that's correct; for `ref T` /
// `mut ref T` / `mut Slice[T]` params the loaded value (Vec
// struct, i64, etc.) is the wrong shape — the state-struct
// field's type is `ptr` (for ref) or `{ ptr, i64 }` (for slice),
// and the size mismatch produced ill-typed IR that the LLVM
// verifier accepted under opaque pointers but that wrote past
// the field's intended footprint into adjacent state-struct
// bytes. Slice 8z closes the gap by mirroring the slice 8d
// non-generic intercept's arg-handling discipline:
//
// - `declare_mono_function` populates `fn_param_ref` /
//   `fn_param_slice_elem` under the mangled key (mirrors
//   `declare_one_function` for non-generic fns).
// - The per-mono intercept's arg-store loop now consults those
//   tables: ref-flagged params consume the caller-side data ptr
//   via `get_data_ptr` (Identifier args) or materialize into a
//   stack temp (rvalue args, with `track_vec_var` registration
//   for Vec-struct-shaped rvalues so heap cleanup queues
//   correctly); slice-elem params route through `coerce_to_slice`
//   to synthesize the `{ptr, i64}` slice header at the call site.
//
// The same change also surfaced a latent gap in
// `llvm_type_for_name`: the canonical `"Slice"` surface name
// (recorded by the typechecker in `pattern_binding_types` for
// `Type::Slice` bindings) fell through to the i64 default
// because the match arms didn't include "Slice" — so even with
// the intercept storing the right value, the state-struct
// field's layout was 8 bytes for a 16-byte slice header. The
// arm now mirrors `llvm_type_for_type_expr`'s identical Slice
// recognition.

#[test]
fn test_slice_8z_ref_t_identifier_arg_stores_data_ptr() {
    // `fn driver[T](item: ref T)` with `T = Vec[i64]`. The
    // state-struct field for `item` is `ptr` (ref T lowers
    // through `TypeKind::Ref` regardless of T's resolution).
    // The caller-side intercept must store the data ptr
    // (via `get_data_ptr("v")`) into the field, NOT the loaded
    // Vec value. Pre-slice-8z, the loaded `{ ptr, i64, i64 }`
    // got stored into the ptr-sized field — size mismatch.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver[T](item: ref T) { fetch(); }
             fn caller() {
                 let mut v: Vec[i64] = Vec.new();
                 v.push(1);
                 driver(v);
             }",
    );
    // State-struct field is ptr-shaped. The mono is `driver$struct`
    // (T = Vec[i64] now binds via the call-site type-arg record —
    // B-2026-07-02-41; the `$` forces LLVM to quote the type name),
    // so match by substring rather than an exact `driver` prefix.
    let state_line = ir
        .lines()
        .find(|l| l.contains("kara.state.driver") && l.contains("= type {"))
        .unwrap_or_else(|| panic!("no state struct in IR:\n{ir}"));
    assert!(
        state_line.contains("{ i32, ptr }"),
        "ref T field must lower to ptr:\n{state_line}"
    );
    // Caller stores a ptr — slice 8d's `get_data_ptr` pattern.
    // The exact identifier name (`%v`) survives inkwell's
    // mangling because there's only one local binding, but be
    // defensive and just check the value type is ptr.
    let caller = extract_fn_ir(&ir, "caller");
    assert!(
        caller.contains("store ptr %v, ptr %kara.arg0.field_ptr"),
        "intercept must store data ptr (not loaded Vec value) into ref-T field:\n{caller}"
    );
    // Regression guard: the wrong-shape store (loaded Vec
    // struct into the state-struct ref-T field) must not
    // appear. Narrowly check stores TO `kara.arg0.field_ptr`
    // — broader Vec stores from `v.push` etc. are fine.
    assert!(
        !caller.contains("store { ptr, i64, i64 } %v, ptr %kara.arg0.field_ptr")
            && !caller.contains("store { ptr, i64, i64 } %v1, ptr %kara.arg0.field_ptr"),
        "intercept must NOT store loaded Vec struct into ref-T field:\n{caller}"
    );
}

#[test]
fn test_slice_8z_mut_slice_t_arg_synthesizes_slice_header() {
    // `fn driver[T](items: mut Slice[T])` — the state-struct
    // field's LLVM type is `{ ptr, i64 }` (slice struct). The
    // caller's `driver(v)` where `v: Vec[i64]` must synthesize
    // the slice header at the call site (slice 8d's
    // `coerce_to_slice` pattern) and store the resulting `{ ptr,
    // i64 }` into the field. Pre-slice-8z, the field was sized
    // i64 (the "Slice" surface name fell through
    // `llvm_type_for_name`'s default arm) while the store wrote
    // 16 bytes — a corrupt store overflowing into adjacent
    // state-struct fields.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver[T](items: mut Slice[T]) { fetch(); }
             fn caller() {
                 let mut v: Vec[i64] = Vec.new();
                 v.push(1);
                 driver(mut v);
             }",
    );
    // State-struct field is the slice struct shape.
    let state_line = ir
        .lines()
        .find(|l| l.starts_with("%kara.state.driver = type {"))
        .unwrap_or_else(|| panic!("no state struct in IR:\n{ir}"));
    assert!(
        state_line.contains("{ ptr, i64 }"),
        "mut Slice[T] field must lower to slice struct {{ ptr, i64 }}:\n{state_line}"
    );
    // Caller stores the slice header (synthesized from Vec).
    let caller = extract_fn_ir(&ir, "caller");
    assert!(
        caller.contains("store { ptr, i64 }") && caller.contains("ptr %kara.arg0.field_ptr"),
        "intercept must store synthesized slice header into mut Slice[T] field:\n{caller}"
    );
    // Regression guard: the wrong-shape store (loaded Vec
    // struct into the state-struct slice field) must not
    // appear. Narrowly check stores TO `kara.arg0.field_ptr`.
    assert!(
        !caller.contains("store { ptr, i64, i64 } %v, ptr %kara.arg0.field_ptr")
            && !caller.contains("store { ptr, i64, i64 } %v1, ptr %kara.arg0.field_ptr"),
        "intercept must NOT store loaded Vec struct into mut Slice[T] field:\n{caller}"
    );
}

#[test]
fn test_slice_8z_owned_t_param_still_stores_loaded_value() {
    // Regression guard: the slice 8z extension is gated on
    // `fn_param_ref` / `fn_param_slice_elem` tables and must
    // NOT change the storage shape for owned-value T params.
    // `fn driver[T](item: T)` with `T = i64` still stores the
    // loaded `i64` value into the field; no ptr or slice-header
    // synthesis fires.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver[T](item: T) { fetch(); }
             fn caller() { driver(42i64); }",
    );
    let caller = extract_fn_ir(&ir, "caller");
    assert!(
        caller.contains("store i64 42, ptr %kara.arg0.field_ptr"),
        "owned-T param must store the loaded value:\n{caller}"
    );
}

// ── Phase 6 line 26 slice 8af: slice 8z verification gaps ────────────
//
// Slice 8z covered `ref T` (with `T = Vec[i64]`) and `mut Slice[T]`
// (with `T = i64`). Slice 8af closes the remaining type-param-typed
// slice verification gap: composite element type (e.g.
// `mut Slice[T]` with `T = Vec[i64]`).
//
// The other 8af-tracked gap — `mut ref T` per-mono intercept
// verification — was probed 2026-05-20 and surfaced a real
// typechecker bug (`driver(mut n)` with `fn driver[T](item: mut
// ref T)` fails to unify with "expected 'mut ref T', found
// 'i64'", indicating the typechecker doesn't apply the mut-marker
// auto-ref-take when the param is a type-parameter-typed
// `mut ref T`). Per the slice 8af tracker entry's policy ("if
// either test surfaces a real bug, that fix lands as a new
// entry"), the bug is carried as a separate tracker entry rather
// than blocking 8af; the `mut ref T` per-mono intercept dispatch
// path is still structurally covered by `declare_mono_function`'s
// `TypeKind::Ref(_) | TypeKind::MutRef(_)` match arm (slice 8z),
// so the missing test is verification-only — the codegen path is
// correct as soon as the typechecker accepts the call.
//
// Slice 8af ships verification-only; no codegen change.

#[test]
fn test_slice_8af_mut_slice_t_composite_element_type() {
    // `fn driver[T](items: mut Slice[T])` with `T = Vec[i64]` —
    // slice of vecs. The element-type-resolution chain is:
    // `extract_slice_elem_type(mut Slice[T])` → calls
    // `llvm_type_for_type_expr(T)` → type_subst[T] = vec_struct.
    // The state-struct field is the slice struct `{ ptr, i64 }`
    // regardless of element type (slice's run-time width is
    // independent of element width — it's always
    // `{ data_ptr, len }`). The caller-side `coerce_to_slice`
    // must produce the slice header from the `Vec[Vec[i64]]`
    // source binding.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver[T](items: mut Slice[T]) { fetch(); }
             fn caller() {
                 let mut vv: Vec[Vec[i64]] = Vec.new();
                 driver(mut vv);
             }",
    );
    // State-struct field is the slice struct shape — independent
    // of element type.
    let state_line = ir
        .lines()
        .find(|l| l.starts_with("%kara.state.driver = type {"))
        .unwrap_or_else(|| panic!("no state struct in IR:\n{ir}"));
    assert!(
            state_line.contains("{ ptr, i64 }"),
            "mut Slice[T] field must lower to slice struct {{ ptr, i64 }} regardless of element type:\n{state_line}"
        );
    // Caller stores the synthesized slice header.
    let caller = extract_fn_ir(&ir, "caller");
    assert!(
        caller.contains("store { ptr, i64 }") && caller.contains("ptr %kara.arg0.field_ptr"),
        "intercept must store synthesized slice header for Vec[Vec[i64]] arg:\n{caller}"
    );
}

// ── Phase 6 line 26 slice 8ag: re-enabled mut ref T per-mono intercept ─
//
// Carved out of slice 8af 2026-05-20 by the typechecker probe that
// surfaced the missing owned-to-mut-ref coercion arm. With slice
// 8ag's typechecker fix landed (`is_subtype` / `types_compatible`
// / `unify_types` accept owned source against `mut ref T` at call
// boundaries, marker enforcement stays at `check_call_site_marker`),
// the codegen-side dispatch is now reachable end-to-end. The
// dispatch itself is unchanged — slice 8z's `declare_mono_function`
// already handles `TypeKind::MutRef(_)` symmetrically with
// `TypeKind::Ref(_)`; this test exercises the previously-blocked
// verification gap.

#[test]
fn test_slice_8ag_per_mono_mut_ref_typeparam_stores_ptr() {
    // `fn driver[T](item: mut ref T) { fetch(); }` with `T = i64`.
    // The state-struct field for `item` lowers to `ptr` (MutRef
    // mirrors Ref's lowering at the layout level — both are
    // pointer-shaped in the captured-local state struct). The
    // caller's per-mono intercept must store the address-of the
    // owned source binding rather than the loaded i64 value.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver[T](item: mut ref T) { fetch(); }
             fn caller() {
                 let mut n: i64 = 7;
                 driver(mut n);
             }",
    );
    // State-struct field is ptr-shaped. The mono is `driver$i64`
    // (T = i64 now binds via the call-site type-arg record —
    // B-2026-07-02-41; the `$` forces LLVM to quote the type name),
    // so match by substring rather than an exact `driver` prefix.
    let state_line = ir
        .lines()
        .find(|l| l.contains("kara.state.driver") && l.contains("= type {"))
        .unwrap_or_else(|| panic!("no state struct in IR:\n{ir}"));
    assert!(
        state_line.contains("ptr"),
        "mut ref T field must lower to ptr in state struct:\n{state_line}"
    );
    // Caller stores a ptr into the state-struct field — the
    // address of `n`, not the loaded i64 value. Regression guard
    // against `store i64 %v, ptr %kara.arg0.field_ptr`.
    let caller = extract_fn_ir(&ir, "caller");
    assert!(
        caller.contains("store ptr") && caller.contains("ptr %kara.arg0.field_ptr"),
        "intercept must store ptr (address-of source) into mut ref T field:\n{caller}"
    );
    assert!(
        !caller.contains("store i64 7, ptr %kara.arg0.field_ptr")
            && !caller.contains("store i64 %n, ptr %kara.arg0.field_ptr"),
        "intercept must NOT store loaded i64 value into mut ref T field:\n{caller}"
    );
}

// ── Phase 6 line 26 slice 8ad: non-generic state-machine intercept ref/slice ──
//
// Slice 8z closed the per-mono intercept (`compile_generic_call`)
// arg-storing gap for `ref T` / `mut ref T` / `mut Slice[T]`
// params. Slice 8ad closes the identical gap in the parallel non-
// generic intercept (`src/codegen/call_dispatch.rs:271-286`).
// Empirical probe 2026-05-20 verified that `fn driver(item: ref
// Vec[i64]) { fetch(); }` emitted `store { ptr, i64, i64 } %v,
// ptr %kara.arg0.field_ptr` — a 24-byte Vec struct stored into an
// 8-byte ptr-typed state-struct field. The in-code comment at
// `call_dispatch.rs:262-265` acknowledged ref-passing through the
// state struct was deferred. The fix mirrors slice 8z exactly:
// consult `fn_param_ref` / `fn_param_slice_elem` keyed on the
// bare fn name, dispatch by mode. The
// `materialize_rvalue_for_ref_arg` helper from slice 8z is now
// `pub(super)` and shared between both intercepts.

#[test]
fn test_slice_8ad_non_generic_ref_vec_param_stores_data_ptr() {
    // `fn driver(item: ref Vec[i64]) { fetch(); }` — the
    // state-struct field for `item` is `ptr` (TypeKind::Ref
    // lowers to ptr). The caller-side non-generic intercept must
    // store the data ptr (via `get_data_ptr("v")`) into the
    // field, NOT the loaded `{ ptr, i64, i64 }` Vec struct.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver(item: ref Vec[i64]) { fetch(); }
             fn caller() {
                 let mut v: Vec[i64] = Vec.new();
                 v.push(1);
                 driver(v);
             }",
    );
    // State-struct field is ptr-shaped.
    let state_line = ir
        .lines()
        .find(|l| l.starts_with("%kara.state.driver = type {"))
        .unwrap_or_else(|| panic!("no state struct in IR:\n{ir}"));
    assert!(
        state_line.contains("{ i32, ptr }"),
        "ref Vec[i64] field must lower to ptr:\n{state_line}"
    );
    // Caller stores a ptr — slice 8d's `get_data_ptr` pattern
    // now fires inside the non-generic state-machine intercept.
    let caller = extract_fn_ir(&ir, "caller");
    assert!(
        caller.contains("store ptr %v, ptr %kara.arg0.field_ptr"),
        "intercept must store data ptr (not loaded Vec value) into ref field:\n{caller}"
    );
    // Regression guard: no Vec struct stored at the state-struct
    // field (the pre-fix shape).
    assert!(
        !caller.contains("store { ptr, i64, i64 } %v, ptr %kara.arg0.field_ptr")
            && !caller.contains("store { ptr, i64, i64 } %v1, ptr %kara.arg0.field_ptr"),
        "intercept must NOT store loaded Vec struct into ref field:\n{caller}"
    );
}

#[test]
fn test_slice_8ad_non_generic_mut_slice_param_synthesizes_header() {
    // `fn driver(items: mut Slice[i64]) { fetch(); }` — the
    // state-struct field is the slice struct `{ ptr, i64 }`. The
    // caller-side non-generic intercept must synthesize the
    // slice header from the Vec source via `coerce_to_slice`
    // and store it into the field.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver(items: mut Slice[i64]) { fetch(); }
             fn caller() {
                 let mut v: Vec[i64] = Vec.new();
                 v.push(1);
                 driver(mut v);
             }",
    );
    // State-struct field is the slice struct shape.
    let state_line = ir
        .lines()
        .find(|l| l.starts_with("%kara.state.driver = type {"))
        .unwrap_or_else(|| panic!("no state struct in IR:\n{ir}"));
    assert!(
        state_line.contains("{ ptr, i64 }"),
        "mut Slice[i64] field must lower to slice struct {{ ptr, i64 }}:\n{state_line}"
    );
    // Caller stores the synthesized slice header.
    let caller = extract_fn_ir(&ir, "caller");
    assert!(
        caller.contains("store { ptr, i64 }") && caller.contains("ptr %kara.arg0.field_ptr"),
        "intercept must store synthesized slice header for mut Slice[i64] arg:\n{caller}"
    );
    // Regression guard: no Vec struct stored at the field.
    assert!(
        !caller.contains("store { ptr, i64, i64 } %v, ptr %kara.arg0.field_ptr")
            && !caller.contains("store { ptr, i64, i64 } %v1, ptr %kara.arg0.field_ptr"),
        "intercept must NOT store loaded Vec struct into mut Slice field:\n{caller}"
    );
}

#[test]
fn test_slice_8ad_non_generic_owned_param_unchanged() {
    // Regression guard: the slice 8ad extension is gated on
    // `fn_param_ref` / `fn_param_slice_elem` tables and must
    // NOT change the storage shape for owned-value params.
    // `fn driver(n: i64) { fetch(); }` still stores the loaded
    // i64 value into the field.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver(n: i64) { fetch(); }
             fn caller() { driver(42); }",
    );
    let caller = extract_fn_ir(&ir, "caller");
    assert!(
        caller.contains("store i64 42, ptr %kara.arg0.field_ptr"),
        "owned i64 param must store the loaded value (regression guard):\n{caller}"
    );
}

// ── Phase 6 line 26 slice 8ae: method-call state-machine intercept ref/slice ──
//
// Slice 8ad closed the non-generic free-fn intercept's
// ref/slice gap; slice 8ae closes the identical gap in the
// method-call state-machine intercept (`src/codegen/method_call.rs`).
// Two surfaces: (1) method args with `ref T` / `mut Slice[T]`
// shapes, (2) `ref self` / `mut ref self` receivers. Both
// dispatch through `fn_param_ref` / `fn_param_slice_elem` keyed
// on the impl-method's dotted name (e.g. `"Container.run"`) —
// populated by `declare_function` against the synthesized impl-
// method function whose `params[0]` is self after
// `make_impl_method_function` promotes `SelfParam` into a real
// `Param`. So `ref_flags[0]` covers self, `ref_flags[1..]`
// covers method args.
//
// Note: these tests use a distinct `Container` struct name (not
// `Hub`) to avoid confusion with pre-existing `shared struct
// Hub` tests elsewhere in this file.

#[test]
fn test_slice_8ae_method_ref_vec_arg_stores_data_ptr() {
    // `impl Container { fn run(self, items: ref Vec[i64]) {
    // fetch(); } }` — the state-struct field for `items` (param
    // idx 1, state-struct field 2) must be ptr. The caller's
    // `h.run(v)` stores the data ptr (via `get_data_ptr("v")`)
    // into field 2, NOT the loaded Vec struct.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             struct Container { count: i64 }
             impl Container {
                 fn run(self, items: ref Vec[i64]) { fetch(); }
             }
             fn caller() {
                 let h = Container { count: 0 };
                 let mut v: Vec[i64] = Vec.new();
                 v.push(1);
                 h.run(v);
             }",
    );
    // State-struct field 2 (items, after tag at 0 and self at
    // 1) is ptr-shaped.
    let state_line = ir
        .lines()
        .find(|l| l.starts_with("%kara.state.Container.run = type {"))
        .unwrap_or_else(|| panic!("no state struct for Container.run in IR:\n{ir}"));
    assert!(
        state_line.contains("ptr"),
        "ref Vec[i64] method-arg field must contain ptr:\n{state_line}"
    );
    // Caller stores a ptr at the kara.arg0.field_ptr GEP.
    let caller = extract_fn_ir(&ir, "caller");
    assert!(
            caller.contains("store ptr %v, ptr %kara.arg0.field_ptr"),
            "intercept must store data ptr (not loaded Vec value) into method's ref-arg field:\n{caller}"
        );
    // Regression guard: no Vec struct stored at the field.
    assert!(
        !caller.contains("store { ptr, i64, i64 } %v, ptr %kara.arg0.field_ptr")
            && !caller.contains("store { ptr, i64, i64 } %v1, ptr %kara.arg0.field_ptr"),
        "intercept must NOT store loaded Vec struct into method's ref-arg field:\n{caller}"
    );
}

#[test]
fn test_slice_8ae_method_mut_slice_arg_synthesizes_header() {
    // `impl Container { fn run(self, items: mut Slice[i64]) {
    // fetch(); } }` — the state-struct field for `items` is the
    // slice struct. The caller's `h.run(mut v)` synthesizes the
    // slice header from the Vec source via `coerce_to_slice`
    // and stores it.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             struct Container { count: i64 }
             impl Container {
                 fn run(self, items: mut Slice[i64]) { fetch(); }
             }
             fn caller() {
                 let h = Container { count: 0 };
                 let mut v: Vec[i64] = Vec.new();
                 v.push(1);
                 h.run(mut v);
             }",
    );
    // Caller stores the synthesized slice header.
    let caller = extract_fn_ir(&ir, "caller");
    assert!(
        caller.contains("store { ptr, i64 }") && caller.contains("ptr %kara.arg0.field_ptr"),
        "intercept must store synthesized slice header for mut Slice[i64] method arg:\n{caller}"
    );
    // Regression guard: no Vec struct stored at the field.
    assert!(
        !caller.contains("store { ptr, i64, i64 } %v, ptr %kara.arg0.field_ptr")
            && !caller.contains("store { ptr, i64, i64 } %v1, ptr %kara.arg0.field_ptr"),
        "intercept must NOT store loaded Vec struct into method's mut Slice field:\n{caller}"
    );
}

#[test]
fn test_slice_8ae_method_owned_args_unchanged() {
    // Regression guard: owned-value method args still store
    // loaded values. `impl Container { fn run(self, n: i64) {
    // fetch(); } }` — caller `h.run(42)` emits `store i64 42,
    // ptr %kara.arg0.field_ptr`.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             struct Container { count: i64 }
             impl Container {
                 fn run(self, n: i64) { fetch(); }
             }
             fn caller() {
                 let h = Container { count: 0 };
                 h.run(42);
             }",
    );
    let caller = extract_fn_ir(&ir, "caller");
    assert!(
        caller.contains("store i64 42, ptr %kara.arg0.field_ptr"),
        "owned method arg must store the loaded value (regression guard):\n{caller}"
    );
}

// ── Phase 6 line 26 slice 8ab: call_effect_subs reaches Program ─────
//
// Slice 8aa populated `EffectCheckResult.call_effect_subs`. Slice 8ab
// threads that table forward through `cli.rs::Pipeline` into
// `Program.call_effect_subs` so codegen (and slice 8y) can read
// per-call effect-variable resolutions. The codegen pipeline used
// by `ir_for_with_state_struct_layouts` doesn't currently invoke
// the cli pipeline, so the table arrives through the helper's
// build call instead — verify the table reaches the program here
// by running the full pipeline manually.

#[test]
fn test_slice_8ab_call_effect_subs_reaches_program() {
    // `op[T, with E]` called with a closure that reads(Db) — after
    // running the full pipeline, `parsed.program.call_effect_subs`
    // must record one entry binding `E` to a set containing
    // `reads(Db)`. This is the AST-level table consumed by codegen.
    use karac::cli::build_call_effect_subs_table;
    let src = "effect resource Db;\n\
                   pub fn op[T, with E](x: T, cb: Fn(T) -> T with E) -> T with E { cb(x) }\n\
                   pub fn touch_db(x: i64) -> i64 with reads(Db) { x }\n\
                   pub fn main() with reads(Db) {\n\
                       let _ = op(42, |y| touch_db(y));\n\
                   }";
    let mut parsed = karac::parse(src);
    assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    assert!(typed.errors.is_empty(), "type: {:?}", typed.errors);
    let method_types = typed.method_callee_types.clone();
    let call_type_subs = typed.call_type_subs.clone();
    karac::lower(&mut parsed.program, &typed);
    let effects = karac::effectcheck_with_typecheck_data(
        &parsed.program,
        karac::effectchecker::PublicEffectsPolicy::default(),
        karac::manifest::CompileProfile::Default,
        method_types.clone(),
        call_type_subs,
    );
    // Slice 8ab's table-builder converts the effectchecker's
    // `Effect` set into plain `EffectKey` values.
    parsed.program.call_effect_subs = build_call_effect_subs_table(&effects);
    assert!(
        !parsed.program.call_effect_subs.is_empty(),
        "Program.call_effect_subs must record at least one binding for the op(...) call"
    );
    let (_, bindings) = parsed
        .program
        .call_effect_subs
        .iter()
        .next()
        .expect("at least one binding");
    let e = bindings
        .get("E")
        .expect("E must be bound at the op(...) call");
    assert!(
        e.iter().any(|k| k.verb == "reads" && k.resource == "Db"),
        "E must bind to a set containing reads(Db); got: {:?}",
        e
    );
}

// ── Phase 6 line 26 slice 8y: caller-side state-machine intercept gating ──
//
// Slice 8y adds a per-call-site decision on whether the per-mono
// state-machine helpers (emitted by slice 8v Phase 2) actually
// fire at a generic call site or whether the call lowers to a
// direct mango-keyed call instead. The decision draws from two
// tables populated by slices 8aa/8ab/8y at the cli pipeline
// boundary:
//   - `Program.call_effect_subs[(call.span.offset, call.span.length)]`
//     binds each `with E` effect-variable name to the resolved
//     effect set at this call (from the closure args' effects).
//   - `Program.callee_purely_polymorphic_effects` records the
//     set of callees whose declared effects are
//     `DeclaredEffects::Polymorphic` only (purely `with E` /
//     `with _`, no static fixed portion). For these callees the
//     decision-making consults `call_effect_subs` alone; for
//     `Explicit` / `PolymorphicWithFixed` callees the static
//     portion may include network-yield verbs regardless of `E`
//     resolution, so the intercept stays unconditional.
//
// The four tests below cover the side-table population +
// intercept-presence regression coverage:
//   1. Purely-polymorphic callee with body yields is in the
//      marker set.
//   2. `PolymorphicWithFixed` callee with body yields is NOT in
//      the marker set (fixed effects might include
//      `sends(Network)`; conservative keep).
//   3. `Explicit` (non-polymorphic) callee is NOT in the marker
//      set.
//   4. Existing slice 8v Phase 2 fixture — a non-polymorphic-effect
//      callee (no `with` clause at all) whose body yields — still
//      takes the state-machine intercept path, demonstrating the
//      slice 8y change is a strict no-op for callees outside the
//      marker set.

#[test]
fn test_slice_8y_purely_polymorphic_callee_in_marker_set() {
    // `op[T, with E]` declared with `with E` only (no fixed
    // effects) lands in `callee_purely_polymorphic_effects` so
    // codegen knows `call_effect_subs[span][E]` is authoritative
    // for the per-call network-yield classification.
    use karac::cli::build_callee_purely_polymorphic_effects_set;
    let src = "effect resource Network;
                   pub fn fetch() with sends(Network) receives(Network) {}
                   fn op[T, with E](cb: Fn() with E) with E { cb(); }
                   fn caller() { op(|| fetch()); }";
    let mut parsed = karac::parse(src);
    assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    assert!(typed.errors.is_empty(), "type: {:?}", typed.errors);
    karac::lower(&mut parsed.program, &typed);
    let effects = karac::effectcheck_with_typecheck_data(
        &parsed.program,
        karac::effectchecker::PublicEffectsPolicy::default(),
        karac::manifest::CompileProfile::Default,
        typed.method_callee_types.clone(),
        typed.call_type_subs.clone(),
    );
    let set = build_callee_purely_polymorphic_effects_set(&effects);
    assert!(
        set.contains("op"),
        "op (purely polymorphic `with E`) must be in the marker set: {:?}",
        set
    );
    assert!(
        !set.contains("fetch"),
        "fetch (Explicit) must NOT be in the marker set: {:?}",
        set
    );
}

#[test]
fn test_slice_8y_polymorphic_with_fixed_callee_excluded() {
    // `op` with `with reads(Db) E` is `PolymorphicWithFixed` —
    // its fixed portion may carry network-yield verbs at any
    // future call site, so codegen must conservatively keep the
    // state-machine intercept. The marker set excludes it.
    use karac::cli::build_callee_purely_polymorphic_effects_set;
    let src = "effect resource Db;
                   fn write_log() with reads(Db) {}
                   pub fn op[T, with E](cb: Fn() with E) with reads(Db) E { write_log(); cb(); }
                   fn caller() { op(|| write_log()); }";
    let mut parsed = karac::parse(src);
    assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    assert!(typed.errors.is_empty(), "type: {:?}", typed.errors);
    karac::lower(&mut parsed.program, &typed);
    let effects = karac::effectcheck_with_typecheck_data(
        &parsed.program,
        karac::effectchecker::PublicEffectsPolicy::default(),
        karac::manifest::CompileProfile::Default,
        typed.method_callee_types.clone(),
        typed.call_type_subs.clone(),
    );
    let set = build_callee_purely_polymorphic_effects_set(&effects);
    assert!(
        !set.contains("op"),
        "op (PolymorphicWithFixed) must NOT be in the marker set: {:?}",
        set
    );
}

#[test]
fn test_slice_8y_explicit_callee_excluded_from_marker_set() {
    // A callee with a concrete `Explicit` effect set never lands
    // in the marker set — slice 8y's optimization is only sound
    // for callees whose entire effect surface comes from `with E`
    // resolution.
    use karac::cli::build_callee_purely_polymorphic_effects_set;
    let src = "effect resource Network;
                   pub fn fetch() with sends(Network) receives(Network) {}
                   fn driver[T](item: T) { fetch(); }
                   fn caller() { driver(42i64); }";
    let mut parsed = karac::parse(src);
    assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    assert!(typed.errors.is_empty(), "type: {:?}", typed.errors);
    karac::lower(&mut parsed.program, &typed);
    let effects = karac::effectcheck_with_typecheck_data(
        &parsed.program,
        karac::effectchecker::PublicEffectsPolicy::default(),
        karac::manifest::CompileProfile::Default,
        typed.method_callee_types.clone(),
        typed.call_type_subs.clone(),
    );
    let set = build_callee_purely_polymorphic_effects_set(&effects);
    assert!(
        !set.contains("driver"),
        "driver (inferred Explicit via body) must NOT be in the marker set: {:?}",
        set
    );
    assert!(
        !set.contains("fetch"),
        "fetch (declared Explicit) must NOT be in the marker set: {:?}",
        set
    );
}

#[test]
fn test_slice_8y_non_polymorphic_callee_still_takes_intercept_path() {
    // Regression coverage: the slice 8v Phase 2 fixture's
    // `driver` fn is non-polymorphic (no `with` clause, effects
    // inferred to `Explicit({sends(Network), receives(Network)})`
    // from the body's `fetch()` call). Slice 8y's gate must
    // return `true` (state-machine path) for it — the caller
    // body must still contain the intercept's signature
    // instructions (state-struct constructor call + poll loop +
    // free). A regression that broke this gate would erroneously
    // turn `driver(42)` into a direct call that bypasses the
    // event-loop integration.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver[T](item: T) { fetch(); }
             fn caller() { driver(42i64); }",
    );
    let caller_body = extract_fn_ir(&ir, "caller");
    assert!(
        caller_body.contains("call ptr @\"__kara_state_new_driver$i64\"()"),
        "non-polymorphic callee must still take the state-machine intercept:\n{caller_body}"
    );
    assert!(
        caller_body.contains("kara.poll_loop:"),
        "non-polymorphic callee must still emit the poll-loop block:\n{caller_body}"
    );
}

// ──────────────────────────────────────────────────────────────────
// String.substring(start: i64) -> String — shipped 2026-05-21.
//
// Returns a fresh owned String of the receiver's bytes from byte
// offset `start` to the end. Out-of-range / negative starts
// saturate to an empty String. Codegen lives in `vec_method.rs`
// next to `starts_with` (String/Vec shared layout); the impl uses
// a conditional branch + malloc + memcpy + String aggregate.
// ──────────────────────────────────────────────────────────────────

#[test]
fn test_e2e_chained_slice_len_on_nonident_receiver() {
    // `s.bytes().len()` / `.is_empty()` — the `.len()` receiver is a
    // method-chain result yielding the `{ptr, i64}` slice header, not the
    // `{ptr,len,cap}` Vec struct. Codegen's non-identifier len/is_empty
    // handler only matched the Vec struct, so the chain fell through to
    // the dispatch-fail error (interp handled it). Regression for the
    // kata-katas #722 bench harness's `out[k].bytes().len()`.
    let output = run_program(
        "fn main() {\n\
                 let v: Vec[String] = [\"abc\", \"de\"];\n\
                 println(f\"{v[0].bytes().len()}\");\n\
                 let s: String = \"hello\";\n\
                 println(f\"{s.bytes().len()}\");\n\
                 println(f\"{s.bytes().is_empty()}\");\n\
                 let e: String = \"\";\n\
                 println(f\"{e.bytes().is_empty()}\");\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "3\n5\nfalse\ntrue\n");
}

#[test]
fn slice_alias_mut_and_shared_params() {
    // axpy: `y` is exclusive (`mut Slice`) → its accesses carry
    // `!alias.scope`; `x` is a shared `Slice` → its access carries
    // `!noalias` against `y`'s scope (no scope of its own).
    let ir = ir_for(
        r#"
fn axpy(y: mut Slice[f64], x: Slice[f64], a: f64) {
    let n = y.len();
    let mut i = 0;
    while i < n { y[i] = y[i] + x[i] * a; i = i + 1; }
}
fn main() { print(0); }
"#,
    );
    let body = fn_body(&ir, "@axpy(");
    assert!(
        body.contains("!alias.scope"),
        "exclusive `y` accesses should carry !alias.scope:\n{body}"
    );
    assert!(
        body.contains("!noalias"),
        "shared `x` access should carry !noalias:\n{body}"
    );
    // Distinct domain + scope nodes were emitted.
    assert!(
        ir.contains("distinct !{"),
        "expected distinct scoped-alias metadata nodes"
    );
}

#[test]
fn slice_alias_two_mut_params_mutual() {
    // Two exclusive slices → each access carries BOTH alias.scope (own
    // scope) and noalias (the other's scope) — mutual disambiguation.
    let ir = ir_for(
        r#"
fn copy2(dst: mut Slice[i64], src: mut Slice[i64]) {
    let n = dst.len();
    let mut i = 0;
    while i < n { dst[i] = src[i]; i = i + 1; }
}
fn main() { print(0); }
"#,
    );
    let body = fn_body(&ir, "@copy2(");
    assert!(
        body.lines()
            .any(|l| l.contains("!alias.scope") && l.contains("!noalias")),
        "a copy2 element access should carry both alias.scope and noalias:\n{body}"
    );
}

#[test]
fn slice_alias_single_param_gets_none() {
    // One slice param → nothing to disambiguate → no scoped-alias metadata.
    let ir = ir_for(
        r#"
fn scale(xs: mut Slice[f64], f: f64) {
    let n = xs.len();
    let mut i = 0;
    while i < n { xs[i] = xs[i] * f; i = i + 1; }
}
fn main() { print(0); }
"#,
    );
    let body = fn_body(&ir, "@scale(");
    assert!(
        !body.contains("!alias.scope") && !body.contains("!noalias"),
        "single-slice fn must carry no scoped-alias metadata:\n{body}"
    );
}

#[test]
fn slice_alias_in_monomorph() {
    // The scoped-alias metadata reaches monomorphized slice kernels too
    // (`build_slice_alias_scopes` runs from `compile_mono_function`).
    let ir = ir_for(
        r#"
fn saxpy[T](y: mut Slice[T], x: Slice[T]) {
    let n = y.len();
    let mut i = 0;
    while i < n { y[i] = x[i]; i = i + 1; }
}
fn main() {
    let mut a = [1, 2, 3];
    let b = [4, 5, 6];
    saxpy(a.as_slice_mut(), b.as_slice());
    print(0);
}
"#,
    );
    let body = fn_body(&ir, "saxpy");
    assert!(
        body.contains("!alias.scope") && body.contains("!noalias"),
        "monomorphized slice kernel should carry scoped-alias metadata:\n{body}"
    );
}

/// `Slice[T] == Slice[T]` compares CONTENTS over the viewed range
/// (B-2026-08-27-24).
///
/// The `Vec` sibling's bug one type over, and NOT fixed by that work.
/// `type_supports_partial_eq` has a `Type::Slice` arm next to the `Vec`
/// one, so `karac check` admitted this deliberately — and then all three
/// components disagreed on one binary: `check` passed, `--interp` raised
/// "operator 'Eq' is not defined for operands of type 'Slice'" while
/// claiming the typechecker rejects it (it does not), and `build` refused
/// it as "a reference type", a message written for `shared` handles that a
/// slice is not. A slice reaches `compile_binop` as a bare `ptr`, which is
/// why it took a different wrong arm than the `Vec` did.
///
/// The legs are chosen so no single one can carry the test. Element-wise
/// content equality needs the `Slice[String]` rows — a scalar slice would
/// also pass under a memcmp of the data — and the SAME-BUFFER rows
/// (`c[0..2]` vs `c[1..3]`) are the ones that fail if the comparator reads
/// from one operand's pointer twice. `s1 == s4` pins the length check;
/// `arr[0..2]` pins a slice over a stack `Array`, whose buffer is not a
/// heap `Vec` at all; and the two empty slices pin the zero-trip loop.
#[test]
fn test_e2e_slice_equality_compares_contents() {
    assert_eq!(
        run_program(
            r#"
fn cmp_slices(a: Slice[i64], b: Slice[i64]) -> bool { return a == b; }
fn cmp_mut(a: mut Slice[i64], b: mut Slice[i64]) -> bool { return a == b; }

fn main() {
    let mut a: Vec[i64] = Vec.new();
    a.push(1);
    a.push(2);
    a.push(3);
    let mut c: Vec[i64] = Vec.new();
    c.push(1);
    c.push(2);
    c.push(9);
    let s1: Slice[i64] = a[0..2];
    let s2: Slice[i64] = c[0..2];
    let s3: Slice[i64] = c[1..3];
    let s4: Slice[i64] = a[0..3];
    println(f"{s1 == s2}");
    println(f"{s1 == s3}");
    println(f"{s1 != s3}");
    println(f"{s1 == s4}");
    println(f"{s1 == s1}");
    println(f"{cmp_slices(a[0..2], c[0..2])}");
    println(f"{cmp_mut(mut a[0..2], mut c[0..2])}");
    let mut p: Vec[String] = Vec.new();
    p.push("hello");
    p.push("world");
    let mut q: Vec[String] = Vec.new();
    q.push("hello");
    q.push("WORLD");
    let ps: Slice[String] = p[0..2];
    let qs: Slice[String] = q[0..2];
    let ps1: Slice[String] = p[0..1];
    let qs1: Slice[String] = q[0..1];
    println(f"{ps == qs}");
    println(f"{ps1 == qs1}");
    let arr: Array[i64, 3] = Array[1, 2, 3];
    let as1: Slice[i64] = arr[0..2];
    println(f"{as1 == s1}");
    let e1: Vec[i64] = Vec.new();
    let es1: Slice[i64] = e1[0..0];
    let es2: Slice[i64] = a[0..0];
    println(f"{es1 == es2}");
}
"#,
        ),
        // s1==s2, s1==s3, s1!=s3, s1==s4 (len), s1==s1, param, mut param,
        // Slice[String] differing, Slice[String] equal prefix,
        // slice-over-Array vs slice-over-Vec, two empty slices.
        Some("true\nfalse\ntrue\nfalse\ntrue\ntrue\ntrue\nfalse\ntrue\ntrue\ntrue\n".to_string())
    );
}

/// `Slice[T]` ordering on the compiled backend (B-2026-08-27-45) — the
/// last member of the tuple / array / slice family, all three of which
/// `karac check` admitted (`type_supports_ord` carries an arm for each)
/// and none of which either backend lowered.
///
/// The comparator is the `Vec` one with its HEADER made a parameter: the
/// walk reads fields 0 and 1, which a `{ptr, len, cap}` and a `{ptr, len}`
/// share, so one body serves both. That is the same parameterization slice
/// EQUALITY already uses (`emit_eq_fn_for_ptr_len_header`,
/// B-2026-08-27-24), which is what makes the two operations read a slice
/// header the same way rather than by two separate conventions.
///
/// Rows chosen for what each pins:
///
///   * `p < a` is the PREFIX row: `[1]` against `[1, 2]`, decided by the
///     length tiebreak after the shared element compares equal. Deleting
///     that tiebreak — the obvious simplification when adapting the array
///     comparator, whose extent is fixed — makes two slices of different
///     length compare EQUAL, and every other row here still passes.
///   * The `show(...)` rows pass slices as PARAMETERS. A slice local
///     compiles to the address of its header slot while a parameter
///     arrives as the fat struct by value, and the operator sees both;
///     `compile_slice_ord` normalizes them exactly as `compile_slice_eq`
///     does. Without that the parameter form stores a pointer word and
///     compares addresses.
///   * The `heap` rows are the sharp test for per-element comparison, and
///     are built rather than written as literals on purpose: element 0 of
///     each slice is content-EQUAL but a DISTINCT allocation, so a
///     comparator reading the header's pointer word would return at
///     element 0 with an arbitrary answer instead of walking on to the
///     element that actually differs. Two string literals would likely
///     share one rodata address and let that bug pass.
///   * The `mut Slice[i64]` rows exercise the other surface spelling. It
///     is a `TypeKind::MutSlice`, which `display_mangle_te` renders
///     "unknown", so the comparator is keyed on a canonical `Slice[T]`
///     rebuilt from the element — otherwise every element type collapses
///     onto one `karac_cmp_unknown`, the collision B-2026-08-27-25
///     records for the bare `"Array"` name.
///   * Both element types appear in ONE program, so a collision between
///     their comparators would show up as a wrong answer here rather than
///     as a link error: both take `(ptr, ptr) -> i64`, so sharing a symbol
///     is silent.
///
/// Twinned against the interpreter, whose `value_compare` has had a
/// `Slice` arm as long as its `Vec` one — so that side needed only the
/// dispatch.
#[test]
fn test_e2e_slice_ordering() {
    let src = r#"
fn show(s: Slice[i64], t: Slice[i64]) -> bool { return s < t; }
fn cmp2(s: mut Slice[i64], t: mut Slice[i64]) -> bool { return s < t; }
fn build(p: String) -> String {
    let mut s = String.new();
    s.push_str(p);
    s.push_str("x");
    return s;
}
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1); v.push(2); v.push(1); v.push(3);
    let a: Slice[i64] = v[0..2];
    let b: Slice[i64] = v[2..4];
    let c: Slice[i64] = v[0..2];
    println(f"{a < b}");
    println(f"{b < a}");
    println(f"{a < c}");
    println(f"{a <= c}");
    println(f"{a >= c}");
    println(f"{b > a}");
    println(f"{a == c}");

    let p: Slice[i64] = v[0..1];
    println(f"{p < a}");
    println(f"{a < p}");

    println(f"{show(a, b)}");
    println(f"{show(b, a)}");

    let mut m: Vec[i64] = Vec.new(); m.push(1); m.push(2);
    let mut n: Vec[i64] = Vec.new(); n.push(1); n.push(3);
    println(f"{cmp2(mut m[0..2], mut n[0..2])}");
    println(f"{cmp2(mut n[0..2], mut m[0..2])}");

    let mut w: Vec[String] = Vec.new();
    w.push(build("a")); w.push("m");
    w.push(build("a")); w.push("n");
    let x: Slice[String] = w[0..2];
    let y: Slice[String] = w[2..4];
    println(f"{x < y}");
    println(f"{y < x}");
    println(f"{x <= x}");
}
"#;
    let expected = "true\nfalse\nfalse\ntrue\ntrue\ntrue\ntrue\n\
                        true\nfalse\n\
                        true\nfalse\n\
                        true\nfalse\n\
                        true\nfalse\ntrue\n";
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

/// B-2026-08-27-20 — a NESTED index store whose RHS is a NAMED LOCAL
/// double-freed under codegen, while the same store from a TEMPORARY was
/// clean and the interpreter was right in both. One binding was the whole
/// difference.
///
/// The move-suppression that keeps the source from freeing a buffer the
/// container now owns was gated on the target's object being an `Identifier`
/// (`v[i] = x`) or a `FieldAccess` (`h.xs[j] = t`). For `d[0][1] = x` the
/// object is ITSELF an `Index`, so it matched neither arm and never ran: the
/// store released the displaced element and moved `x`'s buffer into the slot,
/// then `x`'s scope-exit cleanup freed that same buffer.
///
/// `str` is the failing shape. `deep` is the three-level form, which the same
/// arm has to cover because `vec_index_elem_type_expr` peels one Vec layer per
/// index level — a fix that special-cased two levels would pass `str` and
/// still abort here.
///
/// `int` and `whole` are the controls, and they are why this survived. `int`
/// stores a scalar into `Vec[Vec[i64]]`: nothing is owned, so no suppression
/// is due and over-suppressing would be invisible. `whole` replaces an entire
/// inner Vec (`outer[0] = nb`), which takes the `Identifier` arm that already
/// worked. Between them they pin that the new arm neither under- nor
/// over-reaches.
///
/// Every payload is built by `mk(n)` rather than written as a literal: two
/// identical string literals can fold to one global, and a double free of a
/// shared global does not necessarily abort, so distinct allocations are what
/// make the defect reachable at all.
#[test]
fn test_e2e_nested_index_store_from_a_named_local_is_balanced() {
    assert_eq!(
        run_program(
            r#"
fn mk(n: i64) -> String { return f"v{n}"; }

fn main() {
    let mut r: Vec[String] = Vec.new();
    r.push(mk(1)); r.push(mk(2));
    let mut d: Vec[Vec[String]] = Vec.new();
    d.push(r);
    let x: String = mk(3);
    d[0][1] = x;
    println(f"str={d[0][1]}");

    let mut i0: Vec[i64] = Vec.new();
    i0.push(1); i0.push(2);
    let mut di: Vec[Vec[i64]] = Vec.new();
    di.push(i0);
    let n: i64 = 9;
    di[0][1] = n;
    println(f"int={di[0][1]}");

    let mut inner: Vec[String] = Vec.new();
    inner.push(mk(4));
    let mut mid: Vec[Vec[String]] = Vec.new();
    mid.push(inner);
    let mut top: Vec[Vec[Vec[String]]] = Vec.new();
    top.push(mid);
    let y: String = mk(5);
    top[0][0][0] = y;
    println(f"deep={top[0][0][0]}");

    let mut vv: Vec[String] = Vec.new();
    vv.push(mk(6));
    let mut outer: Vec[Vec[String]] = Vec.new();
    outer.push(vv);
    let mut nb: Vec[String] = Vec.new();
    nb.push(mk(7));
    outer[0] = nb;
    println(f"whole={outer[0][0]}");
}
"#
        ),
        Some("str=v3\nint=9\ndeep=v5\nwhole=v7\n".to_string())
    );
}

/// B-2026-08-15-24 — the TUPLE-element sibling of B-2026-08-15-21.
/// `s[0].0 = v` through a `mut Slice[(A, B)]` param failed the build with
/// "tuple-element assignment through this receiver shape is not yet
/// lowered". Loud, not silent — which is why it was filed low — but the
/// same missing-slice-table cause one function over.
///
/// Two halves had to learn about slices separately: -21 taught
/// `field_chain_place_ptr` to hand back the element POINTER, and this
/// taught `place_chain_aggregate_llvm_type` to hand back the element
/// TYPE. After -21 the pointer resolved and the type did not, so the
/// store still bailed. The `mut ref Vec` line below is the control that
/// worked throughout (B-2026-08-02-8 added the Vec arm and no slice arm).
///
/// The `(String, i64)` element is the memory-relevant one: replacing a
/// heap element frees the displaced value, and its ASAN twin is
/// `asan_tuple_elem_store_through_mut_slice_frees_displaced` in
/// `tests/memory_sanitizer.rs`. Paired with an interpreter oracle in
/// `tests/interpreter.rs`.
#[test]
fn test_e2e_tuple_elem_store_through_mut_slice_param_reaches_caller() {
    assert_eq!(
        run_program(
            r#"
fn bump_first(s: mut Slice[(i64, i64)]) { s[0].0 = s[0].0 + 1; }
fn bump_second(s: mut Slice[(i64, i64)]) { s[0].1 = 99; }
fn bump_at(s: mut Slice[(i64, i64)], i: i64) { s[i].0 = 70; }
fn bump_all(s: mut Slice[(i64, i64, i64)]) {
    let mut i = 0;
    while i < s.len() { s[i].2 = i * 10; i = i + 1; }
}
fn swap_text(s: mut Slice[(String, i64)]) { s[0].0 = "replaced"; }
fn bump_vec(v: mut ref Vec[(i64, i64)]) { v[0].0 = v[0].0 + 100; }
fn main() {
    let mut ps: Vec[(i64, i64)] = Vec.new();
    ps.push((3, 7));
    ps.push((4, 8));
    bump_first(mut ps);
    println(f"{ps[0].0}");
    bump_second(mut ps);
    println(f"{ps[0].1}");
    bump_at(mut ps, 1);
    println(f"{ps[1].0} {ps[0].0}");
    bump_vec(mut ps);
    println(f"{ps[0].0}");
    let mut ts: Vec[(i64, i64, i64)] = Vec.new();
    ts.push((0, 0, 0));
    ts.push((0, 0, 0));
    ts.push((0, 0, 0));
    bump_all(mut ts);
    println(f"{ts[0].2} {ts[1].2} {ts[2].2}");
    let mut ss: Vec[(String, i64)] = Vec.new();
    ss.push(("original", 5));
    swap_text(mut ss);
    println(f"{ss[0].0} {ss[0].1}");
}
"#
        ),
        Some("4\n99\n70 4\n104\n0 10 20\nreplaced 5\n".to_string())
    );
}

/// B-2026-08-15-21 — a FIELD assignment through a `mut Slice[T]` parameter
/// emitted NO STORE AT ALL, so the caller's element kept its old value and
/// `karac check` was clean. Not a write-back problem: the read-back inside
/// the same function (`inside=`) also saw the stale value, which is what
/// distinguishes "the store went to a copy" from "no store was emitted".
///
/// The three neighbours are in the same program on purpose — each worked
/// before the fix, and together they are what localizes the bug to the
/// plain-struct element through the Slice ABI rather than to mutation
/// through a parameter in general:
///   `mut ref Vec[P]`  field store   — a different container
///   `mut Slice[P]`    WHOLE element — never reaches `compile_field_store`
///   `mut Slice[i64]`  scalar element — no field involved
///
/// Paired with an interpreter oracle of the same program in
/// `tests/interpreter.rs` — the interpreter was always right here, so the
/// twin is what pins run-vs-build parity from both ends.
#[test]
fn test_e2e_field_store_through_mut_slice_param_reaches_caller() {
    assert_eq!(
        run_program(
            r#"
struct P { x: i64, y: i64 }
fn bump_slice(s: mut Slice[P]) { s[0].x = s[0].x + 1; println(f"inside={s[0].x}"); }
fn bump_at(s: mut Slice[P], i: i64) { s[i].y = 99; }
fn bump_vec(v: mut ref Vec[P]) { v[0].x = v[0].x + 100; }
fn set_whole(s: mut Slice[P]) { s[1] = P { x: 70, y: 80 }; }
fn bump_scalar(s: mut Slice[i64]) { s[0] = s[0] + 1; }
fn main() {
    let mut ps: Vec[P] = Vec.new();
    ps.push(P { x: 3, y: 7 });
    ps.push(P { x: 5, y: 11 });
    bump_slice(mut ps);
    println(f"{ps[0].x}");
    bump_at(mut ps, 1);
    println(f"{ps[1].y}");
    bump_vec(mut ps);
    println(f"{ps[0].x}");
    set_whole(mut ps);
    println(f"{ps[1].x} {ps[1].y}");
    let mut ns: Vec[i64] = Vec.new();
    ns.push(41);
    bump_scalar(mut ns);
    println(f"{ns[0]}");
}
"#
        ),
        Some("inside=4\n4\n99\n104\n70 80\n42\n".to_string())
    );
}

/// B-2026-08-17-44 — the RUNTIME half of the impl-head fix: a concrete
/// builtin-container head keeps its element args, and codegen has to be
/// able to read through the `self` that results.
///
/// Two things had to meet for this to work. The typechecker's
/// `check_impl_block` and codegen's `make_impl_method_function` each
/// carried their own hand-mirrored list of which heads keep their args;
/// they now share `impl_dispatch::impl_head_keeps_type_args`. And the
/// Slice index arm in `compile_index` keyed on `Identifier` only, while
/// `self[0]` arrives as `SelfValue` — so even with the element type
/// registered the body died on "Index operator applied to non-array
/// type" while `--interp` read the element. Both receiver modes are
/// pinned: a slice IS its `{ptr, len}` value, so owned and `ref` alike
/// must work.
#[test]
fn a_slice_impl_head_can_read_its_own_elements() {
    for (label, self_mode) in [("owned self", "self"), ("borrowed self", "ref self")] {
        let src = format!(
            "trait Head {{ fn first_or({self_mode}, d: i64) -> i64; }}\n\
                 impl Head for Slice[i64] {{\n\
                     fn first_or({self_mode}, d: i64) -> i64 {{\n\
                         if self.len() == 0 {{ return d; }}\n\
                         return self[0];\n\
                     }}\n\
                 }}\n\
                 fn main() {{\n\
                     let v: Vec[i64] = [10, 20, 30];\n\
                     let s: Slice[i64] = v[1..3];\n\
                     println(s.first_or(-1).to_string());\n\
                 }}\n"
        );
        let Some(out) = run_program(&src) else {
            return;
        };
        assert_eq!(
            out, "20\n",
            "{label}: the slice head must read its own element, not lose it at the head"
        );
    }
}

/// B-2026-08-20-40 — an inline index of a SLICE temporary. `s.bytes()[i]`
/// is the spelling the typechecker's own diagnostic recommends when it
/// rejects `s[i]` on a String ("or s.bytes()[i] for raw byte access
/// (O(1))"), and design.md § Character type names it as the O(1) form —
/// yet it was check-clean, correct under `--interp`, and failed BOTH
/// codegen backends with "Index operator applied to non-array type". The
/// identifier-keyed slice dispatch fires only for a NAMED binding, so the
/// `{ptr, len}` header a slice-producing expression evaluates to fell
/// through to the generic tail, which handles only `ArrayType` /
/// `VectorType`.
///
/// The last line is the ORACLE the fix is measured against: `let b =
/// s.bytes(); b[1]` always worked, so the inline spelling has to read the
/// same byte. Every index is distinct per receiver (1→101, 4→111, 2 of
/// `v`→30, 2 of `c"abc"`→99, 3→108, 1 of `hs`→"cd") so a receiver bound to
/// the wrong source prints a wrong value rather than coincidentally
/// matching.
#[test]
fn test_e2e_index_slice_temporary_matches_the_bound_spelling() {
    let src = r#"
struct W { s: String }

fn main() {
    let s: String = "hello";
    println(s.bytes()[1]);
    println(f"b={s.bytes()[4]}");
    let mut i = 0i64;
    let mut sum = 0i64;
    while i < 5i64 {
        sum = sum + s.bytes()[i] as i64;
        i = i + 1i64;
    }
    println(sum);
    let v: Vec[i64] = [10, 20, 30];
    println(v.as_slice()[2]);
    println(c"abc".as_bytes()[2]);
    let w = W { s: "hello" };
    println(w.s.bytes()[3]);
    let hs: Vec[String] = ["ab", "cd"];
    println(hs.as_slice()[1]);
    let b = s.bytes();
    println(b[1]);
}
"#;
    assert_eq!(
        run_program(src).as_deref(),
        Some("101\nb=111\n532\n30\n99\n108\ncd\n101\n")
    );
}

/// The bounds check comes along with the lowering: the synth slice local
/// routes through `compile_slice_index`, so an out-of-range index into a
/// slice temporary panics rather than reading past the String's storage.
/// Pinned separately because the whole point of materializing into a synth
/// binding — rather than GEPing the header inline — is to inherit that
/// check, and a regression there would be silent in the value test above.
#[test]
fn test_e2e_index_slice_temporary_bounds_checked() {
    let Some(c) = run_program_capturing(
        "fn main() {\n\
                 let s: String = \"hi\";\n\
                 let i = 7i64;\n\
                 println(s.bytes()[i]);\n\
             }",
    ) else {
        return;
    };
    assert!(
        c.stdout.contains("index out of bounds") || c.stderr.contains("index out of bounds"),
        "expected an OOB panic from a slice temporary, got stdout={:?} stderr={:?}",
        c.stdout,
        c.stderr
    );
}

/// B-2026-08-21-23 — a fixed array handed to a METHOD's `ref Slice[T]`
/// parameter.
///
/// A `ref Slice[T]` slot takes a POINTER to a `{ptr,len}` header; an
/// `Array[T, N]`'s storage is its raw ELEMENTS, with no header anywhere.
/// The method path passed `&array[0]`, so the callee read `{ptr,len}` out
/// of the first two elements. Measured before the fix, all with
/// `--interp` answering correctly:
///
///   * body that only calls `len()` — JIT answered `-129820080518201344`
///   * body that INDEXES — SEGFAULT under both JIT and AOT (exit 139)
///   * a struct-FIELD array — LLVM module verification failure
///
/// which is why every arm below indexes rather than just measuring: the
/// length alone would have caught the garbage but not the wild
/// dereference, and the wild dereference is the reason this is high
/// severity. The free-function spelling was correct throughout, so it is
/// swept beside the method one as the control that pins the two together.
#[test]
fn test_e2e_fixed_array_into_a_method_ref_slice_parameter() {
    let src = r#"
trait Sink { fn feed(mut ref self, xs: ref Slice[i64]) -> i64; }

struct Acc { total: i64 }
impl Sink for Acc {
    fn feed(mut ref self, xs: ref Slice[i64]) -> i64 {
        let mut i = 0;
        while i < xs.len() { self.total = self.total + xs[i]; i = i + 1; }
        xs.len()
    }
}

struct Wrap { small: Array[i64, 2], big: Array[f64, 4] }
impl Wrap {
    fn sum(ref self, xs: ref Slice[i64]) -> i64 {
        let mut s = 0;
        let mut i = 0;
        while i < xs.len() { s = s + xs[i]; i = i + 1; }
        s
    }
    fn count_f64(ref self, xs: ref Slice[f64]) -> i64 { xs.len() }
    // `self.field` as the argument, from inside another method.
    fn sum_own(ref self) -> i64 { self.sum(self.small) }
}

struct U8s { d: Array[u8, 5] }
impl U8s {
    fn sum(ref self, xs: ref Slice[u8]) -> i64 {
        let mut s = 0;
        let mut i = 0;
        while i < xs.len() { s = s + xs[i] as i64; i = i + 1; }
        s
    }
}

// A `ref Array` PARAM forwarded into the method — the shape B-2026-07-30-3
// widened the free-function gate for.
fn fwd(w: ref Wrap, xs: ref Array[i64, 3]) -> i64 { w.sum(xs) }

// The free-function spelling, correct before and after.
fn free_sum(xs: ref Slice[i64]) -> i64 {
    let mut s = 0;
    let mut i = 0;
    while i < xs.len() { s = s + xs[i]; i = i + 1; }
    s
}

fn main() {
    let w = Wrap { small: [7, 8], big: [1.0, 2.0, 3.0, 4.0] };
    let arr: Array[i64, 3] = [100, 200, 300];

    println(w.sum(arr));          // owned local array
    println(w.sum(w.small));      // struct field
    println(w.sum_own());         // self.field from inside a method
    println(w.count_f64(w.big));  // a wider element
    println(fwd(w, arr));         // ref Array param forwarded

    let u = U8s { d: [1u8, 2u8, 3u8, 4u8, 5u8] };
    let ua: Array[u8, 5] = [9u8, 9u8, 9u8, 9u8, 9u8];
    println(u.sum(u.d));
    println(u.sum(ua));

    let mut a = Acc { total: 0 };
    println(a.feed(arr));         // trait method
    println(a.total);

    // Degenerate and larger lengths — the header's len must come from the
    // ARRAY's static length, not from whatever sits in element 1.
    let one: Array[i64, 1] = [42];
    println(w.sum(one));
    let eight: Array[i64, 8] = [1, 2, 3, 4, 5, 6, 7, 8];
    println(w.sum(eight));

    println(free_sum(arr));       // control
}
"#;
    assert_eq!(
        run_program(src).as_deref(),
        Some("600\n15\n15\n4\n600\n15\n45\n3\n600\n42\n36\n600\n")
    );
}

/// B-2026-08-21-44 — `as_slice()` on a fixed-array TEMPORARY.
///
/// The remainder B-2026-08-21-25 left: that fix admits the fixed-array
/// READ surface plus user-impl methods on the `Array` head, and
/// `as_slice` is on neither list — its result is a `{ptr, len}` VIEW into
/// the materialized slot rather than a value read out of the aggregate.
///
/// Its filing treated the view's lifetime as the open question, since an
/// entry alloca is frame-lived and that is not the rule a slice of a named
/// binding gets. The ownership checker already answers it: `let s =
/// e.to_ne_bytes().as_slice();` is rejected with "slice from temporary
/// value escapes the enclosing statement", so every program that reaches
/// codegen uses the view inside the statement that made it — strictly
/// inside the slot's lifetime, committing to no new rule.
///
/// Pinned beside its bound two-line twin, the oracle the fix is written
/// against: the temporary is routed onto the binding's own path, so a
/// regression breaking only that path would otherwise still pass.
///
/// `as_ptr` stays refused BY BOTH backends on this shape (interpreter:
/// "method 'as_ptr' not found"; codegen: dispatch falls through), which is
/// the contrast that makes admitting `as_slice` a divergence CLOSED rather
/// than one opened — the interpreter answers `as_slice` and always did.
#[test]
fn test_e2e_as_slice_on_a_fixed_array_temporary() {
    let src = r#"
fn mk() -> Array[u8, 3] { return [7u8, 8u8, 9u8]; }

fn total(b: ref Slice[u8]) -> i64 {
    let mut s = 0;
    let mut i = 0;
    while i < b.len() { s = s + b[i] as i64; i = i + 1; }
    s
}

fn main() {
    let e: u16 = 258u16;

    // The row's own repro, beside its bound twin.
    println(e.to_ne_bytes().as_slice().len());
    let bound = e.to_ne_bytes();
    println(bound.as_slice().len());

    // A user fn returning a fixed array — the element type arrives by
    // signature lookup rather than from `to_ne_bytes`'s fixed `u8`.
    println(mk().as_slice().len());
    let m = mk();
    println(m.as_slice().len());

    // The view must carry the right BASE, not just the right length: a
    // header built off the wrong place would still say 3 here.
    println(total(mk().as_slice()));
    println(total(m.as_slice()));

    // A byte-string literal, the third receiver shape -25 sweeps.
    println(b"abc".as_slice().len());
}
"#;
    assert_eq!(run_program(src).as_deref(), Some("2\n2\n3\n3\n24\n24\n3\n"));
}
