//! drop bodies, destructors, drop ordering -- fixtures for `tests/memory_sanitizer.rs`.
//!
//! Split out of `tests/memory_sanitizer.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test memory_sanitizer` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test memory_sanitizer drop_order::
//!
//! New fixtures about drop bodies, destructors, drop ordering belong in this file.

use super::*;

/// B-2026-08-29-63, the other direction — the four shapes the transfer must
/// DECLINE still behave exactly as they did, and still allocate the copy.
///
/// Each is a distinct reason, and each was a measured double free or
/// use-after-free on a prototype that transferred unconditionally:
///
///   * `readf` is called once with a FIELD place (`h.r`) and once with a
///     fresh temp, so the whole-program prepass disqualifies the param and
///     BOTH sites keep the copy — one body is emitted, so one fact has to
///     cover every site.
///   * `peek`'s argument is READ AGAIN after the move. Use-after-move is a
///     non-fatal warning on this surface (B-2026-08-29-64), and the entry
///     copy is the mechanism that keeps the reuse safe; the prepass
///     excludes exactly the sites `use_after_move_consume_sites` names.
///   * `guard_eat` takes a type with a user `impl Drop`. Transfer moves the
///     value's death into the callee, which would move the BODY earlier than
///     the interpreter runs it — a run-vs-build divergence rather than a
///     trade — so a `Drop`-bearing type is declined outright.
///
/// `guard=71` printing after `drop 70` is the pre-existing ordering, and is
/// the assertion that would break first if the `Drop` exclusion were lifted.
#[test]
fn asan_by_value_struct_param_transfer_declines_reuse_places_temps_and_drop() {
    assert_clean_asan_run(
        r#"
struct Res { id: i64, buf: Vec[i64] }
struct Guard { id: i64, buf: Vec[i64] }
impl Drop for Guard { fn drop(mut ref self) { println(f"drop {self.id}") } }
struct Holder { r: Res }

fn mk(n: i64) -> Res {
    let mut v: Vec[i64] = Vec.new();
    let mut i = 0;
    while i < 8 { v.push(i + n); i = i + 1; }
    return Res { id: n, buf: v };
}
fn mkg(n: i64) -> Guard {
    let mut v: Vec[i64] = Vec.new();
    let mut i = 0;
    while i < 8 { v.push(i + n); i = i + 1; }
    return Guard { id: n, buf: v };
}

fn readf(r: Res) -> i64 { return r.buf[3]; }
fn peek(r: Res) -> i64 { return r.buf[4]; }
fn guard_eat(g: Guard) -> i64 { return g.buf[1]; }

fn main() {
    let h = Holder { r: mk(40) };
    let rf = readf(h.r);
    println(f"field={rf}");
    let rt = readf(mk(50));
    println(f"temp={rt}");

    let e = mk(60);
    let rp = peek(e);
    println(f"peek={rp}");
    println(f"reuse={e.buf[5]}");

    let g = mkg(70);
    let rg = guard_eat(g);
    println(f"guard={rg}");
}
"#,
        &[
            "field=43", "temp=53", "peek=64", "reuse=65", "drop 70", "guard=71",
        ],
        "b63-transfer-declines",
    );
}

/// B-2026-09-04-7 — a scalar read through a `Drop`-bearing field touches no
/// memory, before OR after the fix.
///
/// The filing row measured `9 allocs, 9 frees, 0 bytes in use at exit` on
/// the BROKEN binary against `13 allocs, 13 frees` on the correct
/// whole-field spelling: the four missing allocations were the two lost
/// drop bodies' own f-string buffers. So the defect was invisible to ASAN,
/// valgrind and the exit status alike — a lost user `Drop` BODY with the
/// books balanced, which is the silent-wrong-behaviour profile a resource
/// release would have hit and nothing would have reported.
///
/// This case therefore does not guard a leak that once existed. It guards
/// the FIX: the bodies now run, so they allocate, and handing a binding
/// back a walk another site may already own is exactly how this family
/// produces double frees. The `shared` cell earns its place here rather
/// than only in the A/B fixture — declining a mask on an RC handle is the
/// one arm of this change that could unbalance a refcount. Three
/// iterations, because a per-iteration imbalance accumulates rather than
/// hiding in one pass.
#[test]
fn asan_scalar_read_through_drop_field_is_balanced() {
    assert_clean_asan_run_min_allocs(
        r#"
struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}") } }
fn mk(n: i64) -> R { return R { id: n, tag: f"t{n}" }; }
impl R { fn idm(ref self) -> i64 { return self.id; } }
struct H2 { a: R, b: R }
struct H3 { a: R, b: R, c: R }
struct HOwn { a: R, b: R }
impl Drop for HOwn { fn drop(mut ref self) { println("dHOwn") } }
shared struct Box1 { v: i64 }
struct R2 { id: i64, sh: Box1 }
impl Drop for R2 { fn drop(mut ref self) { println(f"dR2{self.id}/{self.sh.v}") } }
struct Hb { a: R2, b: R2 }
struct Inner { r: R }
struct Outer { h: Inner }
fn c_scalar() { let h = H2 { a: mk(1), b: mk(101) }; let z = h.a.id; println(f"  z{z}") }
fn c_method() { let h = H2 { a: mk(2), b: mk(102) }; let z = h.a.idm(); println(f"  z{z}") }
fn c_three()  { let h = H3 { a: mk(3), b: mk(103), c: mk(203) }; let z = h.a.id; println(f"  z{z}") }
fn c_own()    { let h = HOwn { a: mk(4), b: mk(104) }; let z = h.a.id; println(f"  z{z}") }
fn c_move()   { let o = Outer { h: Inner { r: mk(5) } }; let x = o.h.r; println(f"  z{x.id}") }
fn c_shared() { let h = Hb { a: R2 { id: 7, sh: Box1 { v: 71 } }, b: R2 { id: 8, sh: Box1 { v: 81 } } }; let sv = h.a.sh; println(f"  s{sv.v}") }
fn c_live()   { let h = H2 { a: mk(6), b: mk(106) }; let z = h.a.id; println(f"  z{z}"); println(f"  b{h.b.id}") }
fn main() {
    let mut i = 0;
    while i < 3 {
        println("scalar"); c_scalar();
        println("method"); c_method();
        println("three"); c_three();
        println("own"); c_own();
        println("move"); c_move();
        println("shared"); c_shared();
        println("live"); c_live();
        i = i + 1;
    }
}
"#,
        // The helper compares the WHOLE stdout and `main` loops three
        // times, so the per-iteration block appears three times.
        &[
            "scalar",
            "dR101/t101",
            "dR1/t1",
            "  z1",
            "method",
            "dR102/t102",
            "dR2/t2",
            "  z2",
            "three",
            "dR203/t203",
            "dR103/t103",
            "dR3/t3",
            "  z3",
            "own",
            "dHOwn",
            "dR104/t104",
            "dR4/t4",
            "  z4",
            "move",
            "  z5",
            "dR5/t5",
            "shared",
            "dR28/81",
            "dR27/71",
            "  s71",
            "live",
            "  z6",
            "  b106",
            "dR106/t106",
            "dR6/t6",
            "scalar",
            "dR101/t101",
            "dR1/t1",
            "  z1",
            "method",
            "dR102/t102",
            "dR2/t2",
            "  z2",
            "three",
            "dR203/t203",
            "dR103/t103",
            "dR3/t3",
            "  z3",
            "own",
            "dHOwn",
            "dR104/t104",
            "dR4/t4",
            "  z4",
            "move",
            "  z5",
            "dR5/t5",
            "shared",
            "dR28/81",
            "dR27/71",
            "  s71",
            "live",
            "  z6",
            "  b106",
            "dR106/t106",
            "dR6/t6",
            "scalar",
            "dR101/t101",
            "dR1/t1",
            "  z1",
            "method",
            "dR102/t102",
            "dR2/t2",
            "  z2",
            "three",
            "dR203/t203",
            "dR103/t103",
            "dR3/t3",
            "  z3",
            "own",
            "dHOwn",
            "dR104/t104",
            "dR4/t4",
            "  z4",
            "move",
            "  z5",
            "dR5/t5",
            "shared",
            "dR28/81",
            "dR27/71",
            "  s71",
            "live",
            "  z6",
            "  b106",
            "dR106/t106",
            "dR6/t6",
        ],
        "scalar_read_through_drop_field",
        60,
    );
}

/// B-2026-09-04-2 — the projection-source struct destructure hands each leaf
/// B-2026-09-04-4, ARGUMENT half — the fixture that CAUGHT the regression,
/// kept as the gate against its return.
///
/// Moving a generic `Drop` binding into a by-value param double-freed the
/// moment the binding started registering a per-monomorph `UserDrop`
/// wrapper: the caller's retraction matches a `StructDrop` and skips a
/// `UserDrop`, so both frames owned the same buffers. Measured as
/// `AddressSanitizer: attempting double-free` on the 8-byte `v` region,
/// with a non-generic control and a no-`Drop` generic control both clean.
/// The stdout twin cannot see this class at all — the printed lines are
/// identical whether one frame frees or two — which is the whole reason
/// this fixture exists separately.
///
/// Three iterations so an imbalance accumulates; `parami` keeps a second
/// live monomorph on the same path, `temp` covers the fresh-literal
/// argument, and `plain` is the non-generic control. Measured after the
/// fix: 64 malloc calls, exit 0, no LeakSanitizer report.
#[test]
fn asan_generic_impl_drop_by_value_argument_is_balanced() {
    assert_clean_asan_run_min_allocs(
        r#"
struct Box3[T] { v: T, tag: String }
impl[T] Drop for Box3[T] { fn drop(mut ref self) { println(f"dB{self.tag.len()}") } }
struct Pl { v: String, tag: String }
impl Drop for Pl { fn drop(mut ref self) { println(f"dP{self.tag.len()}") } }
fn take(b: Box3[String]) { println(f"  t{b.v.len()}") }
fn takei(b: Box3[i64]) { println(f"  ti{b.v}") }
fn takep(b: Pl) { println(f"  tp{b.v.len()}") }
fn make() -> Box3[String] { return Box3 { v: f"mmmmmmmm", tag: f"rrrrrrr" }; }
fn c_param() { let b: Box3[String] = Box3 { v: f"pppppppp", tag: f"qqqqqq" }; take(b); println("  after") }
fn c_parami() { let b: Box3[i64] = Box3 { v: 5, tag: f"iiiii" }; takei(b) }
fn c_ret()   { let r = make(); println(f"  r{r.v.len()}") }
fn c_temp()  { take(Box3 { v: f"tttttttt", tag: f"sssss" }) }
fn c_plain() { let b = Pl { v: f"pppppppp", tag: f"qqqqqq" }; takep(b) }
fn main() {
    let mut i = 0;
    while i < 3 {
        println("param");  c_param();
        println("parami"); c_parami();
        println("ret");    c_ret();
        println("temp");   c_temp();
        println("plain");  c_plain();
        i = i + 1;
    }
}
"#,
        // The helper compares the WHOLE stdout and `main` loops three
        // times, so the per-iteration block appears three times.
        &[
            "param", "  t8", "dB6", "  after", "parami", "  ti5", "dB5", "ret", "  r8", "dB7",
            "temp", "  t8", "dB5", "plain", "  tp8", "dP6", "param", "  t8", "dB6", "  after",
            "parami", "  ti5", "dB5", "ret", "  r8", "dB7", "temp", "  t8", "dB5", "plain",
            "  tp8", "dP6", "param", "  t8", "dB6", "  after", "parami", "  ti5", "dB5", "ret",
            "  r8", "dB7", "temp", "  t8", "dB5", "plain", "  tp8", "dP6",
        ],
        "generic_impl_drop_by_value_argument",
        40,
    );
}

/// B-2026-09-04-4 — the memory half of the generic-`Drop` fix: emitting a
/// per-monomorph body must not add a free, and must not lose one.
///
/// The wrapper this fix builds REPLACES the memory drop the binding used to
/// register (B-2026-09-03-35's fallback) rather than adding to it — the
/// wrapper's own last step IS that memory drop. Get that wrong in either
/// direction and the failure is silent in the output the E2E twin asserts:
/// register both and every `String` field is double-freed; register neither
/// and they all leak. So this fixture exists to catch what a stdout
/// comparison structurally cannot.
///
/// Six cells over three iterations, so a per-iteration imbalance
/// accumulates rather than hiding in one pass. `two` is the cell that
/// matters most here: `Box3[String]` and `Box3[i64]` reach two DIFFERENT
/// per-monomorph memory drops through two different wrappers, and running
/// `String`'s drain over the `i64` layout would free an `i64` as a bogus
/// `{ptr,len,cap}`. Measured on this fixture: 82 malloc calls, exit 0, no
/// LeakSanitizer report. (LSan is Linux-only; a green macOS run of this
/// proves no double-free and says nothing about leaks — see CLAUDE.md.)
#[test]
fn asan_generic_impl_drop_is_balanced_per_monomorph() {
    assert_clean_asan_run_min_allocs(
        r#"
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct Box3[T] { v: T, tag: String }
impl[T] Drop for Box3[T] { fn drop(mut ref self) { println(f"dB{self.tag.len()}") } }
struct G[T] { v: T, r: R }
impl[T] Drop for G[T] { fn drop(mut ref self) { println("dG") } }
struct H[T] { items: Vec[T] }
impl[T] Drop for H[T] { fn drop(mut ref self) { println(f"dH{self.items.len()}") } }
struct P { s: String }
impl Drop for P { fn drop(mut ref self) { println("dP") } }
fn c_str() { let b: Box3[String] = Box3 { v: f"vvvvvvvv", tag: f"ttttttt" }; println(f"  v{b.v.len()}"); println("  after") }
fn c_two() { let a: Box3[String] = Box3 { v: f"aaaaaaaa", tag: f"ttttttttt" }; println(f"  a{a.v.len()}"); let b: Box3[i64] = Box3 { v: 7, tag: f"uuuuuuuu" }; println(f"  b{b.v}") }
fn c_field() { let g: G[String] = G { v: f"gggggggg", r: R { id: 3 } }; println(f"  g{g.v.len()}") }
fn c_vec() { let h: H[String] = H { items: [f"aaaaaaaa", f"bbbbbbbb"] }; println(f"  h{h.items.len()}") }
fn c_nest() { let d: Box3[Vec[String]] = Box3 { v: [f"zzzzzzzz"], tag: f"wwwwww" }; println(f"  d{d.v.len()}") }
fn c_plain() { let p = P { s: f"pppppppp" }; println(f"  p{p.s.len()}") }
fn main() {
    let mut i = 0;
    while i < 3 {
        println("str"); c_str();
        println("two"); c_two();
        println("field"); c_field();
        println("vec"); c_vec();
        println("nest"); c_nest();
        println("plain"); c_plain();
        i = i + 1;
    }
}
"#,
        // The helper compares the WHOLE stdout and `main` loops three
        // times, so the per-iteration block appears three times.
        &[
            "str", "  v8", "dB7", "  after", "two", "  a8", "dB9", "  b7", "dB8", "field", "  g8",
            "dG", "dR3", "vec", "  h2", "dH2", "nest", "  d1", "dB6", "plain", "  p8", "dP", "str",
            "  v8", "dB7", "  after", "two", "  a8", "dB9", "  b7", "dB8", "field", "  g8", "dG",
            "dR3", "vec", "  h2", "dH2", "nest", "  d1", "dB6", "plain", "  p8", "dP", "str",
            "  v8", "dB7", "  after", "two", "  a8", "dB9", "  b7", "dB8", "field", "  g8", "dG",
            "dR3", "vec", "  h2", "dH2", "nest", "  d1", "dB6", "plain", "  p8", "dP",
        ],
        "generic_impl_drop_per_monomorph",
        60,
    );
}

/// B-2026-09-04-27 — the memory gate on the container-element half of the
/// generic-`Drop` fix: running a per-monomorph body from a BODIES-ONLY
/// walker must add no free and lose none.
///
/// The walkers this fix touches (`__karac_dropelems_*`, the slot primitive,
/// the `Map` half walks) free nothing by construction — element memory
/// stays with the scope-exit drain — so the risk is not the walker but the
/// body it now calls: `S.drop$<concrete>` is compiled as a whole function
/// from inside a half-built walker, and `Box3[i64]`'s body must read `tag`
/// at the `i64` layout's offset, not `String`'s. Get that wrong and the
/// failure is a read past the element that the E2E twin's stdout may not
/// show. Six cells over three iterations so a per-iteration imbalance
/// accumulates. Measured on this fixture: 126 malloc calls, exit 0, no
/// LeakSanitizer report. (LSan is Linux-only; see CLAUDE.md.)
#[test]
fn asan_generic_impl_drop_container_elements_are_balanced() {
    assert_clean_asan_run_min_allocs(
        r#"
struct Box3[T] { v: T, tag: String }
impl[T] Drop for Box3[T] { fn drop(mut ref self) { println(f"dB{self.tag.len()}") } }
struct Pl { tag: String }
impl Drop for Pl { fn drop(mut ref self) { println(f"dP{self.tag.len()}") } }
fn cell_vec() { let v: Vec[Box3[String]] = [Box3 { v: f"e1111111", tag: f"a" }]; println(f"  n{v.len()}"); println("  after") }
fn cell_two() { let a: Vec[Box3[String]] = [Box3 { v: f"aaaaaaaa", tag: f"tt" }]; println(f"  a{a.len()}"); let b: Vec[Box3[i64]] = [Box3 { v: 7, tag: f"uuu" }]; println(f"  b{b.len()}") }
fn cell_nest() { let vv: Vec[Vec[Box3[String]]] = [[Box3 { v: f"e3333333", tag: f"cccc" }]]; println(f"  vv{vv.len()}") }
fn cell_arr() { let a: Array[Box3[String], 2] = [Box3 { v: f"e4444444", tag: f"ddddd" }, Box3 { v: f"e5555555", tag: f"eeeeee" }]; println(f"  arr{a[1].tag.len()}") }
fn cell_map() { let mut m: Map[String, Box3[String]] = Map.new(); m.insert(f"k", Box3 { v: f"e6666666", tag: f"fffffff" }); println(f"  m{m.len()}") }
fn cell_plain() { let p: Vec[Pl] = [Pl { tag: f"pppppppp" }]; println(f"  p{p.len()}") }
fn main() {
    let mut i = 0;
    while i < 3 {
        println("vec"); cell_vec();
        println("two"); cell_two();
        println("nest"); cell_nest();
        println("arr"); cell_arr();
        println("map"); cell_map();
        println("plain"); cell_plain();
        i = i + 1;
    }
}
"#,
        &[
            "vec", "  n1", "dB1", "  after", "two", "  a1", "dB2", "  b1", "dB3", "nest", "  vv1",
            "dB4", "arr", "  arr6", "dB5", "dB6", "map", "  m1", "dB7", "plain", "  p1", "dP8",
            "vec", "  n1", "dB1", "  after", "two", "  a1", "dB2", "  b1", "dB3", "nest", "  vv1",
            "dB4", "arr", "  arr6", "dB5", "dB6", "map", "  m1", "dB7", "plain", "  p1", "dP8",
            "vec", "  n1", "dB1", "  after", "two", "  a1", "dB2", "  b1", "dB3", "nest", "  vv1",
            "dB4", "arr", "  arr6", "dB5", "dB6", "map", "  m1", "dB7", "plain", "  p1", "dP8",
        ],
        "generic_impl_drop_container_elems",
        90,
    );
}

/// B-2026-09-06-8 — projecting a `Drop`-carrying element OUT of a by-value
/// param place (`let x: R = t.0` / `h.pe.0`, and the two-level
/// destructure-then-project `let (inner, y) = h.pe; let x: R = inner.0`)
/// frees `x`'s moved-in interior exactly once.
///
/// The let-site marks `x` a param VIEW and suppresses its body (the caller
/// runs it); for an own-`Drop` type that also suppressed the free
/// (`karac_drop_<T>` is body + fields together), and the source element was
/// cap-zeroed, so nobody freed `x`'s `tag`/`xs` buffers — 10 B per
/// projection at `KARAC_OPT_LEVEL=0`, output otherwise correct. The fix
/// gives the sole-owner view its own memory-only synthesis
/// (`register_projection_view_mem_drop`), and marks a destructure leaf that
/// owns callee memory so a projection out of IT is reached too. The cells
/// read `x`'s heap (`x.tag`, `x.xs.len()`) so the buffers are live at `-O2`
/// as well, not only on the `-O0` leg where the leak was first measured.
#[test]
fn asan_tuple_param_drop_element_projection_freed_once() {
    assert_clean_asan_run_min_allocs(
        r#"
struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"tag{i}", xs: [i, i] }; }
struct H1 { pe: (R, i64) }
struct H2 { pe: ((R, i64), i64) }
fn t_proj(t: (R, i64)) { let x: R = t.0; println(f"tp {x.id} {x.tag} {x.xs.len()}"); }
fn f_proj(h: H1) { let x: R = h.pe.0; println(f"fp {x.id} {x.tag}"); }
fn v_proj(h: H2) { let (inner, y) = h.pe; let x: R = inner.0; println(f"vp {x.id} {x.tag}"); }
fn main() {
    let mut i = 0;
    while i < 3 {
        t_proj((mk(1), 9));
        f_proj(H1 { pe: (mk(2), 9) });
        v_proj(H2 { pe: ((mk(3), 1), 2) });
        i = i + 1;
    }
}
"#,
        &[
            "tp 1 tag1 2",
            "dR1",
            "fp 2 tag2",
            "dR2",
            "vp 3 tag3",
            "dR3",
            "tp 1 tag1 2",
            "dR1",
            "fp 2 tag2",
            "dR2",
            "vp 3 tag3",
            "dR3",
            "tp 1 tag1 2",
            "dR1",
            "fp 2 tag2",
            "dR2",
            "vp 3 tag3",
            "dR3",
        ],
        "tuple_param_drop_element_projection_freed_once",
        18,
    );
}

/// Masking a returned field's `Drop` BODY out of an own-`Drop` parent's
/// wrapper does not mask its MEMORY (B-2026-08-28-21).
///
/// The wrapper is three steps — the parent's own body, the Drop-bearing
/// fields' bodies, then every field's frees — and the fix masks the MIDDLE
/// one for the fields the callee hands back, because their bodies now belong
/// to the result's owner. Their memory does not: the callee received a copy
/// of the aggregate, not its allocation, so the caller temp is still the one
/// that has to release the escaping field's buffer. A mask that reached
/// `emit_struct_drop_synthesis` would trade the row's double body for a
/// silent leak, which is the failure this fixture exists to catch — and it
/// is invisible to the E2E twin, whose fixtures carry no heap at all.
///
/// Every row therefore gives `R` a `String` field, and the escaping value is
/// READ afterwards so a mask that went the other way — freeing what escaped
/// — surfaces as a use-after-free rather than staying latent.
///
/// `two-droppers` is the row that separates a per-FIELD mask from the
/// wholesale `karac_dropnf_<T>`: `b` dies in the call and its buffer must be
/// freed there, while `a`'s travels out with the result.
///
/// Each escaping value's body lands right after its `println` rather than at
/// scope exit, because a `UserDrop` action fires at its binding's NLL live
/// range end — the placement that makes this backend agree with the
/// interpreter. The expectation is spelled that way rather than grouped at
/// the end, so a regression that moved the drop back to scope exit fails
/// here instead of passing on a coincidence of counts.
#[test]
fn asan_own_drop_parent_masking_a_returned_field_frees_it() {
    assert_clean_asan_run(
        r#"
struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"drop {self.name}") } }

struct W { r: R, n: i64 }
impl Drop for W { fn drop(mut ref self) { println(f"drop W{self.n}") } }

struct Two { a: R, b: R }
impl Drop for Two { fn drop(mut ref self) { println("drop Two") } }

#[allow(partial_move_of_drop_struct)]
fn take(w: W) -> R { let W { r, n } = w; r }
// B-2026-09-03-20 — `return w.r;` is a partial move out of an own-`Drop` `W`
// exactly as the destructure above is; it went unflagged only while the rule
// was site-based. B-2026-09-01-43 deliberately left this one WITHOUT an
// `#[allow]` so it would fail the moment the rule reached `return`, rather
// than pre-masking the fixture that proves the widening works.
#[allow(partial_move_of_drop_struct)]
fn take_field(w: W) -> R { return w.r; }
#[allow(partial_move_of_drop_struct)]
fn take_a(t: Two) -> R { let Two { a, b } = t; a }
fn use_n(w: W) -> i64 { return w.n; }
fn mk() -> W { return W { r: R { id: 7, name: f"m{7}" }, n: 9 }; }

fn main() {
    let x = take(W { r: R { id: 41, name: f"a{1}" }, n: 1 });
    println(x.name);
    let y = take_field(W { r: R { id: 42, name: f"b{2}" }, n: 2 });
    println(y.name);
    let z = take_a(Two { a: R { id: 43, name: f"c{3}" }, b: R { id: 44, name: f"d{4}" } });
    println(z.name);
    let k = use_n(W { r: R { id: 45, name: f"e{5}" }, n: 3 });
    println(f"{k}");
    let q = take(mk());
    println(q.name);
}
"#,
        &[
            "drop W1", "a1", "drop a1", "drop W2", "b2", "drop b2", "drop Two", "drop d4", "c3",
            "drop c3", "drop W3", "drop e5", "3", "drop W9", "m7", "drop m7",
        ],
        "asan_own_drop_parent_masking_a_returned_field_frees_it",
    );
}

/// Assert a program runs cleanly under ASAN and produces the expected
/// stdout. Skips (prints a notice, passes the test) if the host can't
/// support ASAN — see `asan_available` for the rationale.
/// B-2026-08-27-37 — a by-value TUPLE param of a MONOMORPH got neither an
/// entry-copy nor a scope-exit drop, while the caller's tuple-literal arm
/// registered its temp drop regardless, on the stated assumption that "the
/// callee now entry-copies a heap-bearing tuple param, so this caller temp
/// is an INDEPENDENT buffer". For a mono that was false: both sides aliased
/// one buffer, so moving the struct's heap field out handed the SAME `Vec`
/// to the result and still left the caller's temp to free it.
///
/// `compile_mono_function`'s owned-param arm was gated `TypeKind::Path(_)`,
/// so tuples fell through it — the same "the two MUST stay paired" rule
/// B-2026-07-08-6 established for named aggregates, one param shape further.
///
/// Both element types run: this fired at `T = i64` as well as `T = String`,
/// so it is NOT the wrong-monomorph family and a String-only test would not
/// have pinned the scalar half. The `(i64, Bag[T])` row is here because
/// tuple POSITION was an early wrong guess — it fails in either slot.
/// B-2026-08-28-2 — the MEMORY half of the double-body row: when the
/// callee pulls a heap-carrying element out of an owned tuple param and
/// RETURNS it, is the extra user `Drop` body accompanied by an extra
/// RELEASE?
///
/// The row could not answer that from stdout. It observed `drop 41` twice
/// for one `R` on all three backends, noted the program still exits 0 with
/// a `String` field, and left the question open with a request for exactly
/// this fixture. Answered here: the pre-fix double body is bodies ONLY —
/// the same program under ASAN + LSan is clean before the fix as well as
/// after, so the memory side was balanced throughout and the defect never
/// had a use-after-free or a leak in it.
///
/// That is worth pinning rather than dropping, for two reasons. It bounds
/// the row honestly (a wrong side-effect count, not corruption), and it is
/// the direction a REGRESSION would take: the fix suppresses a caller-side
/// registration, and suppressing one element too many — or suppressing the
/// memory walk along with the bodies — turns this into a leak that no
/// stdout assertion in the pair of behavioural twins would notice.
///
/// `two-droppers` carries the same load here as in those twins: element 0
/// escapes through the result and element 1 dies in the call, so an
/// argument-wide suppression leaks element 1's buffer.
///
/// The `p.0` PROJECTION spelling is deliberately absent, and its absence is
/// a finding rather than an oversight. At a heap-carrying `R` it is a
/// genuine heap-use-after-free — the projection copies the element's
/// control block while the source tuple still frees the buffer, so the
/// result's `Drop` body reads freed memory — and it reproduces IDENTICALLY
/// on a compiler built without this fix, so it is neither caused nor
/// repaired here. It is filed as its own row; adding it to this fixture
/// would only make this test red for someone else's bug. The behavioural
/// twins DO cover that spelling, at a scalar `R` where no buffer exists to
/// dangle, which is what makes the body-count claim complete without
/// dragging the memory defect in.
#[test]
fn asan_returned_tuple_param_element_drop_body_is_memory_balanced() {
    const DROPPER: &str = "struct R { id: i64, name: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id} {self.name}\") } }\n";
    for (label, body, want) in [
            (
                "destructure-return",
                "fn take(p: (R, i64)) -> R { let (r, n) = p; r }\n\
                 fn main() { let x = take((R { id: 41, name: f\"n{41}\" }, 1)); println(f\"{x.id}\"); }\n",
                vec!["41", "drop 41 n41"],
            ),
            // B-2026-08-28-16 — the LOCAL-argument spelling, which reaches the
            // escape through a different caller-side owner (the local's own
            // element walk rather than the fresh-temp walk) and is therefore a
            // separate registration to get wrong. Same memory claim: the fix
            // masks BODIES only, so element 0's buffer must still be freed
            // exactly once, by the result's owner.
            //
            // The `p.0` projection spelling is absent here for the reason given
            // above — at a heap-carrying `R` it is a pre-existing
            // use-after-free filed on its own row, and the behavioural twins
            // cover that spelling at a scalar `R`.
            (
                "local-arg-destructure-return",
                "fn take(p: (R, i64)) -> R { let (r, n) = p; r }\n\
                 fn main() { let q = (R { id: 41, name: f\"n{41}\" }, 1);\n\
                 \x20            let x = take(q); println(f\"{x.id}\"); }\n",
                vec!["41", "drop 41 n41"],
            ),
            // The per-element load-bearing row, at a heap `R`: masking one
            // element too many leaks element 1's buffer, which no stdout
            // assertion in the behavioural twins would notice.
            (
                "local-arg-two-droppers",
                "fn take(p: (R, R)) -> R { let (a, b) = p; a }\n\
                 fn main() { let q = (R { id: 41, name: f\"n{41}\" },\n\
                 \x20                 R { id: 42, name: f\"n{42}\" });\n\
                 \x20            let x = take(q); println(f\"{x.id}\"); }\n",
                vec!["drop 42 n42", "41", "drop 41 n41"],
            ),
            (
                "result-discarded",
                "fn take(p: (R, i64)) -> R { let (r, n) = p; r }\n\
                 fn main() { take((R { id: 41, name: f\"n{41}\" }, 1)); println(\"end\"); }\n",
                vec!["drop 41 n41", "end"],
            ),
            (
                "two-droppers",
                "fn take(p: (R, R)) -> R { let (a, b) = p; a }\n\
                 fn main() { let x = take((R { id: 41, name: f\"n{41}\" }, R { id: 42, name: f\"n{42}\" }));\n\
                 \x20            println(f\"{x.id}\"); }\n",
                // B-2026-08-28-19 — `drop 42` used to come LAST, with the
                // caller-side walk sitting on the scope frame; it now fires at
                // statement end, matching the interpreter.
                vec!["drop 42 n42", "41", "drop 41 n41"],
            ),
            // CONTROL — the dropper dies inside the call; its buffer must still
            // be released exactly once. B-2026-08-28-19 moved the BODY ahead of
            // the `1`; the free is unchanged, which is what this fixture is for.
            (
                "other-element-control",
                "fn take(p: (R, i64)) -> i64 { let (r, n) = p; n }\n\
                 fn main() { let x = take((R { id: 41, name: f\"n{41}\" }, 1)); println(f\"{x}\"); }\n",
                vec!["drop 41 n41", "1"],
            ),
        ] {
            assert_clean_asan_run(&format!("{DROPPER}{body}"), &want, label);
        }
}

/// B-2026-08-28-17 — the MEMORY half of the struct-twin double-body row,
/// and the place where the struct leg and the tuple leg genuinely differ.
///
/// The behavioural twins prove the pre-fix double `Drop` body is gone at a
/// scalar `R`. This asks the question they cannot: with a heap-carrying `R`,
/// was the extra body accompanied by an extra RELEASE, and does masking a
/// field out of the caller-side walk leak the fields it no longer visits?
/// Answer on both counts is no — clean under ASAN + LSan before the fix as
/// well as after — so the defect was a wrong side-effect count throughout,
/// never corruption. Pinning that bounds the row honestly and guards the
/// direction a regression would take: the fix SUPPRESSES a caller-side
/// registration, and suppressing one field too many — or letting the mask
/// reach the memory walk instead of the bodies walk — becomes a leak that no
/// stdout assertion in either behavioural twin would notice.
///
/// The `w.r` PROJECTION row is present here, and that is the asymmetry
/// worth recording. Its tuple counterpart `p.0` had to be left OUT of
/// `asan_returned_tuple_param_element_drop_body_is_memory_balanced`: at a
/// heap-carrying element it is a real double free (B-2026-08-28-15),
/// reproducing identically on a compiler built without that fix. The struct
/// spelling has no such hazard — measured rc 0 with correct output on all
/// five shapes here — so this fixture covers both spellings at a heap type,
/// which the tuple one cannot. Same-looking syntax, different memory
/// outcome, and the difference is a property of the two lowerings rather
/// than of the Drop-body fix either row is about.
///
/// `two-droppers` carries the same load as in the behavioural twins: `a`
/// escapes through the result and `b` dies in the call, so an
/// argument-wide suppression leaks `b`'s buffer rather than merely
/// miscounting a println.
#[test]
fn asan_returned_struct_param_field_drop_body_is_memory_balanced() {
    const DROPPER: &str = "struct R { id: i64, name: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id} {self.name}\") } }\n";
    for (label, body, want) in [
        (
            "destructure-return",
            "struct W { r: R, n: i64 }\n\
                 fn take(w: W) -> R { let W { r, n } = w; r }\n\
                 fn main() { let x = take(W { r: R { id: 41, name: f\"n{41}\" }, n: 1 });\n\
                 \x20            println(f\"{x.id}\"); }\n",
            vec!["41", "drop 41 n41"],
        ),
        // The spelling its tuple counterpart cannot test at a heap type.
        (
            "projection-return",
            "struct W { r: R, n: i64 }\n\
                 fn take(w: W) -> R { w.r }\n\
                 fn main() { let x = take(W { r: R { id: 41, name: f\"n{41}\" }, n: 1 });\n\
                 \x20            println(f\"{x.id}\"); }\n",
            vec!["41", "drop 41 n41"],
        ),
        (
            "result-discarded",
            "struct W { r: R, n: i64 }\n\
                 fn take(w: W) -> R { let W { r, n } = w; r }\n\
                 fn main() { take(W { r: R { id: 41, name: f\"n{41}\" }, n: 1 });\n\
                 \x20            println(\"end\"); }\n",
            vec!["drop 41 n41", "end"],
        ),
        // `a` escapes, `b` dies in the call — both buffers released once.
        (
            "two-droppers",
            "struct W { a: R, b: R }\n\
                 fn take(w: W) -> R { let W { a, b } = w; a }\n\
                 fn main() { let x = take(W { a: R { id: 41, name: f\"n{41}\" },\n\
                 \x20                          b: R { id: 42, name: f\"n{42}\" } });\n\
                 \x20            println(f\"{x.id}\"); }\n",
            vec!["drop 42 n42", "41", "drop 41 n41"],
        ),
        // CONTROL — nothing escapes; the field dies inside the call and its
        // buffer must still be released exactly once.
        (
            "no-field-escapes-control",
            "struct W { r: R, n: i64 }\n\
                 fn take(w: W) -> i64 { let W { r, n } = w; n }\n\
                 fn main() { let x = take(W { r: R { id: 41, name: f\"n{41}\" }, n: 1 });\n\
                 \x20            println(f\"{x}\"); }\n",
            vec!["drop 41 n41", "1"],
        ),
    ] {
        assert_clean_asan_run(&format!("{DROPPER}{body}"), &want, label);
    }
}

/// B-2026-08-28-56 — a tuple-typed destructure leaf owns its element's heap
/// even when the element type declares no `Drop`.
///
/// B-2026-08-28-26 gave the tuple leaf its registration, gated on the
/// bodies walker existing. An element type with no `Drop` produces no
/// walker, so the whole registration — memory included — was skipped and
/// nothing owned the element. Same shape as B-2026-08-28-50, where a
/// discarded struct FIELD with no `Drop` got no walker and therefore no
/// owner; the halves now resolve independently.
///
/// THE TWO SOURCES LEAK DIFFERENT AMOUNTS because they lose different
/// things, which is worth pinning rather than averaging: the fresh literal
/// has no owner for the element at all (98 bytes — the `Vec` buffer and its
/// element), while the place source's own walk still frees most of it and
/// loses only the inner `String` (2 bytes).
///
/// The `Vec` is the repro, not the defect: a bare `String` element is
/// leak-clean in this shape only because LLVM deletes the whole dead
/// allocation chain. `mkv` builds its buffer through a runtime `push` the
/// optimizer cannot fold, which is the only reason the bytes are
/// observable — a fixture written the obvious way would pass against the
/// broken compiler.
#[test]
fn asan_drop_free_tuple_leaf_owns_its_element_heap() {
    const N: &str = "struct R { id: i64, tags: Vec[String] }\n\
             fn mkv(n: i64) -> Vec[String] { let mut v = Vec[String].new(); v.push(f\"t{n}\"); v }\n";
    assert_clean_asan_run(
        &format!(
            "{N}fn main() {{ let (inner, n) = ((R {{ id: 41, tags: mkv(3) }}, 2), 1);\n\
             \x20            println(f\"{{n}}\") }}\n"
        ),
        &["1"],
        "fresh-literal-source",
    );
    assert_clean_asan_run(
        &format!(
            "{N}fn main() {{ let p = ((R {{ id: 41, tags: mkv(3) }}, 2), 1);\n\
             \x20            let (inner, n) = p; println(f\"{{n}}\") }}\n"
        ),
        &["1"],
        "place-source",
    );
    // NO no-destructure control here, deliberately. That shape LEAKS the
    // same inner `String` — 2 bytes, measured identical before and after
    // this fix — because the tuple binding's own aggregate drop does not
    // reach the nested struct's `Vec` elements the way the leaf's
    // `TypeExpr`-driven drop does. It is a pre-existing defect at a
    // different site and is filed on its own row; asserting it clean here
    // would be asserting something false.
    // BOUNDARY — the same leaf with a Drop-BEARING element type, which the
    // bodies walker does claim. It must stay at one body and one free.
    assert_clean_asan_run(
            "struct R { id: i64, name: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id} {self.name}\") } }\n\
             fn main() { let (inner, n) = ((R { id: 41, name: f\"n{41}\" }, 2), 1);\n\
             \x20            println(f\"{n}\") }\n",
            &["drop 41 n41", "1"],
            "drop-bearing-element",
        );
}

/// B-2026-08-28-50 — an UNBOUND destructure field of a struct-LITERAL
/// source is owned exactly once whether or not its type has a `Drop`.
///
/// B-2026-08-28-29 established that a struct-literal temp owns nothing and
/// that its DISCARDED Drop-bearing fields are owned by the discard walker.
/// A field whose type has NO `Drop` gets no walker there — nothing is
/// emitted — so on that source it had no owner at all and leaked. The other
/// two sources were clean, which is what localizes it.
///
/// THE `Vec` IS THE REPRO, NOT THE DEFECT. A bare `String` field is
/// leak-clean in the same shape only because LLVM deletes the whole dead
/// allocation chain — the DCE mask B-2026-08-28-12 and -29 both record. The
/// element here is pushed through a runtime call the optimizer cannot fold,
/// which is the only reason the 98 bytes are observable at all.
///
/// `drop-bearing-field` is the boundary in the other direction: that field
/// IS claimed by the walker, and letting the unbound-field arm claim it too
/// aborts at 12 frees for 11 allocs.
#[test]
fn asan_unbound_drop_free_field_of_a_struct_literal_is_owned_once() {
    const N: &str = "struct R { id: i64, tags: Vec[String] }\n\
             struct W { r: R, n: i64 }\n\
             fn mkv(n: i64) -> Vec[String] { let mut v = Vec[String].new(); v.push(f\"t{n}\"); v }\n";
    assert_clean_asan_run(
            &format!(
                "{N}fn main() {{ let W {{ r: _, n }} = W {{ r: R {{ id: 41, tags: mkv(3) }}, n: 1 }};\n\
             \x20            println(f\"{{n}}\") }}\n"
            ),
            &["1"],
            "literal-source",
        );
    // The same field over the two sources that were already clean.
    assert_clean_asan_run(
        &format!(
            "{N}fn mk() -> W {{ W {{ r: R {{ id: 41, tags: mkv(3) }}, n: 1 }} }}\n\
             \x20            fn main() {{ let W {{ r: _, n }} = mk(); println(f\"{{n}}\") }}\n"
        ),
        &["1"],
        "call-source-control",
    );
    assert_clean_asan_run(
        &format!(
            "{N}fn main() {{ let w = W {{ r: R {{ id: 41, tags: mkv(3) }}, n: 1 }};\n\
             \x20            let W {{ r: _, n }} = w; println(f\"{{n}}\") }}\n"
        ),
        &["1"],
        "place-source-control",
    );
    // BOUNDARY — the same position with a Drop-BEARING field type. The
    // discard walker owns this one, and the unbound-field arm must keep
    // declining it or the two claims abort at 12 frees for 11 allocs.
    assert_clean_asan_run(
            "struct R { id: i64, name: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id} {self.name}\") } }\n\
             struct W { r: R, n: i64 }\n\
             fn main() { let W { r: _, n } = W { r: R { id: 41, name: f\"n{41}\" }, n: 1 };\n\
             \x20            println(f\"{n}\") }\n",
            &["drop 41 n41", "1"],
            "drop-bearing-field",
        );
}

#[test]
fn asan_pool_acquire_release_reuse_and_drop_no_leak() {
    // Pool[T] codegen (phase-8): a 40-iteration loop that mints a connection,
    // reads it, releases it (explicit) AND lets the binding drop (idempotent
    // return), then re-acquires — reusing the idle slot. Verifies (1) the
    // `KaracPool` is freed at the `Pool` binding's scope exit (`Pool.drop` →
    // `karac_runtime_pool_drop`) rather than leaked, and (2) the explicit
    // `release` + scope-exit `PooledConnection.drop` hand the slot back
    // exactly once (release is idempotent on `conn_id`) — a missed free
    // leaks the pool, a double return would be caught by the reuse-count
    // assertion, and any slot double-free trips ASAN.
    assert_clean_asan_run(
        r#"
fn make_conn() -> i64 { 100i64 }
fn main() {
    let mut i: i64 = 0i64;
    let mut total: i64 = 0i64;
    while i < 40i64 {
        let pool: Pool[i64] = Pool.new(make_conn, 1i64, 2i64);
        match pool.acquire(0i64) {
            Ok(c) => { total = total + c.val; pool.release(c); }
            Err(_e) => {}
        }
        i = i + 1;
    }
    println(total.to_string());
}
"#,
        // 40 * 100 = 4000
        &["4000"],
        "pool_acquire_release_reuse_and_drop_no_leak",
    );
}

/// Slice 3d-i (self-hosting parser tail): dropping an item node that carries
/// `Vec[AttrNode]` — the attribute list — where each `AttrNode` owns a
/// `Vec[String]` path, a `Vec[AttrArgNode]` (each arg an `Option[String]`
/// name + an `Option[Expr]` value), and an `Option[String]` string value.
/// The `value` field mirrors the port's real `Option[Expr]` where `Expr` is
/// a `shared enum` (RC) — modeled here as `Option[shared enum Val]`. This
/// nested `Vec[struct{ Vec[String], Vec[struct{Option[String],
/// Option[shared enum]}], Option[String] }]` is the Cluster-1
/// heap-in-Vec-in-struct shape; exercises BOTH the consume path (each node
/// moved into a render-like fn and dropped there) and the plain-drop path (a
/// built list dropped at scope exit without consuming). All heap payloads are
/// ≥36 bytes so LSan sees any leaked buffer; a missed drop leaks, a
/// double-drop aborts. (An `Option[PLAIN enum]` payload — which the port does
/// NOT use — leaks under LSan; that separate gap is pinned in
/// `asan_option_plain_enum_heap_payload_undestructured_drop_leaks_pinned`.)
///
/// B-2026-07-03-28 (FIXED — Phase 2 of the caller-retains model): was 240 B /
/// 6 allocs (down from 1434 B / 36 in earlier steps). The residual was the
/// `AttrArgNode` (`ArgN`) options nested in `Vec[ArgN]`: `name: Option[String]`
/// leaked because `ArgN` was NOT copy-supported (its `value: Option[shared]`
/// field failed `field_copy_supported`), so the value drop's `OptionInline`
/// gate — keyed on `aggregate_param_copy_supported_struct` — stayed OFF and
/// `name`'s buffer was never freed. Closed by making `Option[shared]`
/// copy-supported (the shared leg): (1) `field_copy_supported` admits an
/// `Option[shared]` field; (2) `deep_copy_option_inline_payload_in_place`
/// rc-INCs the inline box (word 1) on entry-copy — symmetric with the
/// `emit_nested_struct_shared_rc_decs_ex` / `RcDecOption` rc-DEC on drop; and
/// (3) `track_struct_var` registers the COMBINED drop
/// (`emit_vec_elem_struct_with_shared_drop_fn` = value-drop PLUS the
/// shared-field rc-dec walker) for any struct owning shared fields, so a
/// scope-exit drop of an owning struct local / callee-owned by-value param
/// rc-decs its `shared` / `Option[shared]` children (the value drop alone
/// skips them). With `ArgN` copy-supported, `OptionInline` frees `name`, and
/// the shared box balances (inc == dec). The consume path (each node moved
/// into `render_*` and destructured) self-balances via the entry-copy +
/// destructure-leaf rc-dec; the plain-drop path (`more`, dropped at scope) is
/// fixed by the combined drop. NOTE: element-DEEP entry-copy of a `Vec[struct]`
/// FIELD (the "piece (b)" the original scope named) is NOT needed here — this
/// test consumes `args` via a for-loop; it is only needed for an
/// entry-copy-THEN-whole-drop of such a field, a separate PRE-EXISTING
/// double-free tracked as B-2026-07-04-9. Run: `scripts/lsan-local.sh
/// "asan_attr_node_list_drop_consume_and_plain"`.
#[test]
fn asan_attr_node_list_drop_consume_and_plain() {
    assert_clean_asan_run(
        r#"
shared enum Val { Nothing, Ident(String), Num(i64) }
struct ArgN { name: Option[String], value: Option[Val] }
struct AttrN { path: Vec[String], args: Vec[ArgN], string_value: Option[String] }

fn render_arg(a: ArgN) -> i64 {
    let ArgN { name, value } = a;
    let mut touched = 0;
    match name { Some(s) => { if s.len() >= 0 { touched = touched + 1; } } None => {} }
    // Flat `Some(_)` — the `Option[Val]` value field still drops wholesale
    // (recursing into the `Val::Ident` String), which is the drop path under
    // test; the exact variant is irrelevant to the count.
    match value { Some(_) => { touched = touched + 1; } None => {} }
    touched
}

fn render_attr(a: AttrN) -> i64 {
    let AttrN { path, args, string_value } = a;
    let mut touched = 0;
    for seg in path { if seg.len() >= 0 { touched = touched + 1; } }
    for arg in args { touched = touched + render_arg(arg); }
    match string_value { Some(s) => { if s.len() >= 0 { touched = touched + 1; } } None => {} }
    touched
}

fn build() -> Vec[AttrN] {
    let mut v: Vec[AttrN] = Vec.new();
    let mut i = 0;
    while i < 6 {
        let mut path: Vec[String] = Vec.new();
        path.push("diagnostic_namespace_segment_alpha_aaaaa".to_string());
        path.push("on_unimplemented_attribute_segment_betaa".to_string());
        let mut args: Vec[ArgN] = Vec.new();
        args.push(ArgN {
            name: Some("note_argument_name_key_gamma_ccccccccccc".to_string()),
            value: Some(Val.Ident("clone_derive_identifier_value_ddddddddd".to_string())),
        });
        args.push(ArgN { name: None, value: Some(Val.Num(42)) });
        v.push(AttrN {
            path: path,
            args: args,
            string_value: Some("string_value_payload_epsilon_eeeeeeeeee".to_string()),
        });
        i = i + 1;
    }
    v
}

fn main() {
    let attrs = build();
    let mut total = 0;
    for a in attrs { total = total + render_attr(a); }
    let more = build();
    total = total + more.len();
    println(total);
}
"#,
        &["42"],
        "attr_node_list_drop_consume_and_plain",
    );
}

// ── By-value aggregate (tuple / literal / nested) heap-field drops ──
//
// B-2026-06-11-4: by-value aggregates leaked their String/Vec fields across
// shapes the named-struct drop path didn't reach — a let-bound tuple (no
// type name → no `track_struct_var`), a tuple/struct LITERAL arg (no binding
// → no owner), and a nested-struct field (the synthesized struct drop didn't
// recurse). Fix: `track_tuple_var` (anonymous-aggregate drop at the let
// site), aggregate-literal materialization at the call site, and
// nested-aggregate recursion in `emit_struct_drop_synthesis`; tuple moves
// (`let u = t` / `return t`) suppress the source via `zero_aggregate_field_
// caps`. The loop builds a fresh heap aggregate each iteration in every
// shape; a leaked field trips Linux LSan, and a double-free (if a moved
// tuple or a materialized literal were owned twice) trips macOS ASAN.
#[test]
fn asan_by_value_aggregate_drops_single_free() {
    assert_clean_asan_run(
        r#"
struct S { k: i64, name: String }
struct Inner { name: String }
struct Outer { id: i64, inner: Inner }
fn show_tup(p: (i64, String)) { if p.0 > 99999 { println(p.1); } }
fn fwd(p: (i64, String)) { show_tup(p); }
fn show_s(s: S) { if s.k > 99999 { println(s.name); } }
fn show_o(o: Outer) { if o.id > 99999 { println(o.inner.name); } }
fn mk(n: i64) -> (i64, String) { (n, f"r-{n}") }
fn main() {
    let mut i: i64 = 0;
    while i < 5 {
        let t = (i, f"let-{i}");
        show_tup(t);
        let u = (i, f"mv-{i}");
        let w = u;
        if w.0 > 99999 { println(w.1); }
        let r = mk(i);
        if r.0 > 99999 { println(r.1); }
        fwd((i, f"fwd-{i}"));
        show_tup((i, f"lit-{i}"));
        show_s(S { k: i, name: f"slit-{i}" });
        let o = Outer { id: i, inner: Inner { name: f"nest-{i}" } };
        show_o(o);
        i = i + 1;
    }
    println("done");
}
"#,
        &["done"],
        "by_value_aggregate_drops",
    );
}

/// B-2026-08-02-14 — a GENERIC-mono parent's Drop-carrying field: the
/// subst-aware bodies walk fires exactly once per owner death, and the
/// mono element drop frees the element's String buffer the base
/// synthesis used to leak (`Vec[Box2[Res]]` leaked one name buffer per
/// element) — no leak, no double-free, LSan-clean.
#[test]
fn asan_generic_parent_drop_field_bodies_freed() {
    assert_clean_asan_run(
        r#"
struct Res { id: i64, name: String }
impl Drop for Res {
    fn drop(mut ref self) { println(f"drop {self.id} {self.name}") }
}
struct Box2[T] { item: T, tag: i64 }
fn main() {
    println("a");
    {
        let b: Box2[Res] = Box2 { item: Res { id: 3, name: f"ggg{3}" }, tag: 1 };
        println(f"tag {b.tag}");
    }
    {
        let mut v: Vec[Box2[Res]] = Vec.new();
        v.push(Box2 { item: Res { id: 4, name: f"hhhhh{4}" }, tag: 2 });
        println(f"vlen {v.len()}");
    }
    println("end");
}
"#,
        &[
            "a",
            "tag 1",
            "drop 3 ggg3",
            "vlen 1",
            "drop 4 hhhhh4",
            "end",
        ],
        "generic_parent_drop_field_bodies_freed",
    );
}

#[test]
fn asan_shared_struct_user_drop_recursive_chain_leak_free() {
    // phase-7 L938: a `shared struct` with a user `impl Drop` over a
    // recursive `Option[Self]` chain. The user body fires once per
    // link at that link's refcount→0 (the iterative self-chain fast
    // path is disabled when a user Drop exists, so each link routes
    // through `__karac_rc_drop_Node`). This must be leak-clean AND
    // free each node exactly once — no double-free from the body
    // running on top of the field walk + heap free.
    //
    // B-2026-08-09-3 retimed the expectation from `0 1 2 3` to
    // `1 2 3 0`. `a` is never read, and a never-used binding dies at
    // its own `let` (design.md § Drop ordering; the value tier pins
    // the same rule in `test_ir_user_drops_nll_placement_never_used_
    // bindings`) — so the chain unwinds at `a`'s declaration, ahead of
    // `println(0)`, not at the closing brace. The old order was the RC
    // tier's scope-exit drain, which is precisely the divergence that
    // bug filed: `--interp` on this program prints `1 2 0`, firing the
    // chain BEFORE the `0` on both counts.
    //
    // The interpreter's missing `3` is a separate, pre-existing gap and
    // NOT what changed here: it has no Arc-drop hook for a link held in
    // another shared struct's FIELD rather than by an env binding (see
    // `invoke_user_drop_if_applicable`'s note, tracked under the L940
    // drop-reconciliation item). Codegen walks the field chain and is
    // the more complete backend; this fixture is what guards that.
    assert_clean_asan_run(
        r#"
shared struct Node { val: i64, mut next: Option[Node] }
impl Drop for Node {
    fn drop(mut ref self) {
        println(self.val);
    }
}
fn main() {
    let c = Node { val: 3, next: None };
    let b = Node { val: 2, next: Some(c) };
    let a = Node { val: 1, next: Some(b) };
    println(0);
}
"#,
        &["1", "2", "3", "0"],
        "shared_struct_user_drop_recursive_chain_leak_free",
    );
}

// ── SoA-laid-out Vec drop ────────────────────────────────────
// `layout entities: Vec[Entity]` lowers to multi-allocation storage —
// one buffer per hot group plus an optional cold-group buffer — and
// the outer struct shape is `{ ptr_g0, ..., ptr_g(N-1), [ptr_cold,]
// i64 len, i64 cap }` rather than the plain Vec `{ptr, len, cap}`.
// Before the `FreeSoaGroups` cleanup variant landed, the scope-exit
// walker routed SoA through `FreeVecBuffer`, which both (a) read the
// `cap > 0` guard from the wrong slot (offset 16 in a 2-hot-group
// SoA is the `len` field, not cap) and (b) freed only the first
// group pointer, leaking every other hot group and the cold buffer.
// These tests are the load-bearing ASAN coverage for that fix.

#[test]
fn asan_soa_drop_two_hot_groups_primitive() {
    assert_clean_asan_run(
        r#"
struct Entity { x: f64, y: f64, hp: i64 }
layout entities: Vec[Entity] {
    group physics { x, y }
    group combat { hp }
}
fn main() {
    let mut entities: Vec[Entity] = Vec.new();
    entities.push(Entity { x: 1.0, y: 2.0, hp: 100 });
    entities.push(Entity { x: 3.0, y: 4.0, hp: 200 });
    entities.push(Entity { x: 5.0, y: 6.0, hp: 300 });
    entities.push(Entity { x: 7.0, y: 8.0, hp: 400 });
    entities.push(Entity { x: 9.0, y: 10.0, hp: 500 });
    println(entities.len());
}
"#,
        &["5"],
        "soa_drop_two_hot_groups_primitive",
    );
}

#[test]
fn asan_soa_drop_with_cold_group_primitive() {
    // Cold group adds an extra buffer that pre-fix codegen never
    // freed (the cold pointer sits between the hot pointers and the
    // len/cap pair; the legacy free path read field 0 only). Five
    // pushes cross the cap 0 → 4 → 8 realloc boundary so the prior
    // cold-buffer free path is also exercised.
    assert_clean_asan_run(
        r#"
struct Entity { x: f64, y: f64, hp: i64, label: i64 }
layout entities: Vec[Entity] {
    group physics { x, y }
    group combat { hp }
    cold { label }
}
fn main() {
    let mut entities: Vec[Entity] = Vec.new();
    entities.push(Entity { x: 1.0, y: 2.0, hp: 100, label: 11 });
    entities.push(Entity { x: 3.0, y: 4.0, hp: 200, label: 22 });
    entities.push(Entity { x: 5.0, y: 6.0, hp: 300, label: 33 });
    entities.push(Entity { x: 7.0, y: 8.0, hp: 400, label: 44 });
    entities.push(Entity { x: 9.0, y: 10.0, hp: 500, label: 55 });
    println(entities.len());
}
"#,
        &["5"],
        "soa_drop_with_cold_group_primitive",
    );
}

#[test]
fn asan_rc_fallback_tuple_heap_field_drop_no_leak() {
    // B-2026-06-10-8: a let-bound tuple with a heap (String) field that
    // the ownership checker routes to RC-fallback boxing leaked the
    // field's buffer at scope exit — the box `{i64 rc, value}` was freed
    // at rc==0 without recursing into the boxed value's heap fields. The
    // fix synthesizes a per-box value-drop fn (`register_rc_fallback_box_drop`)
    // that `emit_rc_dec` invokes before the box free. Non-foldable
    // (loop-index) strings so each `t` is a real heap allocation; macOS
    // ASAN proves no double-free, Linux `detect_leaks=1` proves no leak
    // (the leak this closes was LSan-visible, invisible to macOS ASAN).
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i = 0i64;
    while i < 3i64 {
        let t = (i, f"item-{i}");
        println(t.1);
        i = i + 1;
    }
}
"#,
        &["item-0", "item-1", "item-2"],
        "rc_fallback_tuple_heap_field_drop",
    );
}

#[test]
fn asan_rc_fallback_struct_heap_field_drop_no_leak() {
    // B-2026-06-10-8, the non-shared-struct sibling of the tuple case: a
    // let-bound `struct Pair { n: i64, s: String }` routed to RC-fallback
    // boxing leaked its `String` field. The structural heap-field walk
    // (`emit_aggregate_heap_field_frees`) handles tuples and structs
    // uniformly — both lower to an LLVM struct whose String fields are
    // `vec_struct_type()`-shaped.
    assert_clean_asan_run(
        r#"
struct Pair { n: i64, s: String }
fn main() {
    let mut i = 0i64;
    while i < 3i64 {
        let t = Pair { n: i, s: f"item-{i}" };
        println(t.s);
        i = i + 1;
    }
}
"#,
        &["item-0", "item-1", "item-2"],
        "rc_fallback_struct_heap_field_drop",
    );
}

// Two `shared enum`s whose heap layouts are STRUCTURALLY IDENTICAL
// (`Alfa` and `Bravo` are variant-for-variant layout-twins: each is
// `{ Leaf(String), Node(struct { Vec[Self], String }) }`) must NOT share
// one LLVM heap `StructType`. They did before the fix — shared heap types
// were anonymous (`context.struct_type`), which LLVM uniques by structure
// — so the refcount-drop dispatch, which recovers a shared type's name
// from its heap type by object identity, confused the two: dropping a
// `Vec[Alfa]` element ran it through `__karac_rc_drop_Bravo` (or vice
// versa), reading the wrong variant tag/offsets and double-freeing. This
// was the slice-3b self-host type-oracle crash (B-2026-06-20-6,
// `Pattern` vs `TypeExpr`, both 12 payload words); fixed by giving each
// shared type a uniquely NAMED heap struct (`%karac.shared.<T>`). Here
// we BUILD and DROP many
// recursive trees of both twins (each `make_*(4)` nests `Vec[Self]`
// children that rc-dec on scope exit) and assert a clean ASAN run.
#[test]
fn asan_layout_twin_shared_enums_drop_through_correct_rc_drop() {
    assert_clean_asan_run(
            "shared enum Alfa { ALeaf(String), ANode(NodeA) }\n\
             shared enum Bravo { BLeaf(String), BNode(NodeB) }\n\
             struct NodeA { kids: Vec[Alfa], name: String }\n\
             struct NodeB { kids: Vec[Bravo], name: String }\n\
             fn make_a(d: i64) -> Alfa {\n\
             \x20   if d <= 0 { return Alfa.ALeaf(\"alfa-leaf-payload-string-long\".to_string()); }\n\
             \x20   let mut kids: Vec[Alfa] = Vec.new();\n\
             \x20   kids.push(make_a(d - 1));\n\
             \x20   kids.push(make_a(d - 1));\n\
             \x20   Alfa.ANode(NodeA { kids: kids, name: \"alfa-node-name-payload\".to_string() })\n\
             }\n\
             fn make_b(d: i64) -> Bravo {\n\
             \x20   if d <= 0 { return Bravo.BLeaf(\"bravo-leaf-payload-string-long\".to_string()); }\n\
             \x20   let mut kids: Vec[Bravo] = Vec.new();\n\
             \x20   kids.push(make_b(d - 1));\n\
             \x20   kids.push(make_b(d - 1));\n\
             \x20   Bravo.BNode(NodeB { kids: kids, name: \"bravo-node-name-payload\".to_string() })\n\
             }\n\
             fn main() {\n\
             \x20   let mut i = 0;\n\
             \x20   while i < 20 {\n\
             \x20       let a = make_a(4);\n\
             \x20       let b = make_b(4);\n\
             \x20       i = i + 1;\n\
             \x20   }\n\
             \x20   println(\"done\");\n\
             }\n",
            &["done"],
            "asan_layout_twin_shared_enums_drop_through_correct_rc_drop",
        );
}

#[test]
fn asan_fnret_drop_temp_arg_passthrough_and_discard_single_fire() {
    // B-2026-07-01-7: fn-call-RETURNED Drop temps — as a consume arg
    // (drops once after the call), DISCARDED at statement position
    // (drops once), and passed THROUGH a `pass(g) -> Guard { g }`
    // into a binding (drops exactly once via the binding; the
    // passthrough guard skips the arg-temp registration — pre-guard
    // this shape double-fired AND double-freed the heap field on
    // both surfaces, probe f6).
    //
    // B-2026-07-30-12 — the body prints `self.name`, and that is the only
    // reason this test covers anything. With `self.id` alone (as it was
    // until -12) the `name` buffers were never observed, so LLVM deleted
    // every malloc/free pair and the program's WHOLE runtime heap was the
    // 4 KiB stdio buffer — measured, 1 alloc / 1 free. It passed while
    // asserting nothing about the ownership balance it exists to pin.
    // `x.name.len()` does not rescue it: a length is a field read, not a
    // read of the bytes. Printing the name gives 7 allocs / 7 frees — five
    // real Guard buffers driven through the drop path. The change had to
    // wait for -12, because the leak it exposed was real.
    //
    // Heap-carrying Guard so the wrapper's
    // field cleanup is exercised; program structured so NLL and
    // scope-exit drop orders coincide (output is surface-identical).
    assert_clean_asan_run(
        r#"
struct Guard { name: String, id: i64 }
impl Drop for Guard {
    fn drop(mut ref self) { println(self.name); println(self.id); }
}
fn make(n: i64) -> Guard {
    Guard { name: f"guard payload padded beyond thirty-six bytes {n}", id: n }
}
fn consume(g: Guard) { println(100 + g.id); }
fn pass(g: Guard) -> Guard { g }
fn main() {
    let mut i = 0;
    while i < 3 {
        consume(make(i));
        i = i + 1;
    };
    make(60);
    println(999);
    let x = pass(make(50));
    println(x.name.len());
}
"#,
        &[
            "100",
            "guard payload padded beyond thirty-six bytes 0",
            "0",
            "101",
            "guard payload padded beyond thirty-six bytes 1",
            "1",
            "102",
            "guard payload padded beyond thirty-six bytes 2",
            "2",
            "guard payload padded beyond thirty-six bytes 60",
            "60",
            "999",
            "47",
            "guard payload padded beyond thirty-six bytes 50",
            "50",
        ],
        "fnret_drop_temp_arg_passthrough_and_discard_single_fire",
    );
}

// ── Aggregate field user-`impl Drop` glue (B-2026-07-29-39) ──────
// The bug was a LEAK: an aggregate never ran its fields' `Drop`, so any
// resource stored in a struct field was held for the program's lifetime.
// Fixing it means new drop calls on the hot path, and the way to get that
// wrong is to over-fire — a field whose body also frees, run twice, is a
// double free. So this asserts both directions at once: the Drop body owns
// a `Vec` it drains, the holder also owns heap of its own, and the whole
// thing runs in a loop so an under-drop shows up as a definite LSan report
// and an over-drop as a double-free abort.
//
// `MovedOut` is the sharp case the fix has to disarm: `let taken = m.res;`
// hands the field's `Drop` to `taken`, so the holder must stop running it.
#[test]
fn asan_aggregate_field_user_drop_fires_once() {
    assert_clean_asan_run(
        r#"
struct Res { tag: i64, buf: Vec[i64] }
impl Drop for Res {
    fn drop(mut ref self) { self.buf.clear(); }
}
struct Holder { label: String, r: Res }
struct MovedOut { res: Res }

fn main() {
    let mut n = 0i64;
    let mut i = 0i64;
    while i < 200i64 {
        let mut inner: Vec[i64] = Vec.new();
        inner.push(i);
        let h = Holder { label: "held", r: Res { tag: i, buf: inner } };
        n = n + h.label.len();

        // Field moved OUT: exactly one drop of `res` must happen (the
        // destination's), never the holder's as well.
        let mut inner2: Vec[i64] = Vec.new();
        inner2.push(i);
        let m = MovedOut { res: Res { tag: i, buf: inner2 } };
        let taken = m.res;
        n = n + taken.tag - i;
        i = i + 1;
    }
    println(n);
}
"#,
        // 4 ("held") per iteration; `taken.tag - i` is 0. ×200 = 800.
        &["800"],
        "aggregate_field_user_drop_fires_once",
    );
}

// B-2026-07-30-11 SHAPE 2 — the same glue for an owned aggregate TEMP
// (`consume(H { .. })`, and the return-passthrough `pass(H { .. })`).
//
// What this gate is for, stated precisely, because it is NOT the original
// bug: SHAPE 2 was a leak of a resource held in a field, and the fix is
// bodies-only — it emits no free and moves none, so LSan cannot see the
// fix land. Its job is the OPPOSITE direction. Adding drop-body calls on
// the temp path risks OVER-firing, and an intermediate version of the fix
// did exactly that: it ran the body on both the caller's temp and the
// passthrough result. For a body that touches heap, twice is a
// use-after-free. So the passthrough arm is here as the disarm case, the
// way `MovedOut` is in the test above.
//
// NON-VACUITY, deliberately engineered (B-2026-07-30-12's lesson: an ASAN
// test whose allocations LLVM elides passes while proving nothing). With
// `self.buf.clear()` as the only body, BOTH shapes optimize down to a
// single 4 KiB stdio buffer — 1 alloc, nothing exercised. The `self.buf[0]`
// read is what keeps them: it observes the BYTES, so the malloc cannot be
// deleted. Measured: 601 allocs / 601 frees, 400 real Vec buffers driven
// through the drop path. The guard is never true (every pushed value is
// `i >= 0`), so the `println` is dead at runtime but not to the optimizer.
#[test]
fn asan_owned_aggregate_temp_field_drop_fires_once() {
    assert_clean_asan_run(
        r#"
struct Res { tag: i64, buf: Vec[i64] }
impl Drop for Res {
    fn drop(mut ref self) {
        if let Some(v) = self.buf.first() { if v < 0i64 { println(v); } }
        self.buf.clear();
    }
}
struct Holder { label: String, r: Res }

fn consume(h: Holder) -> i64 { h.r.tag + h.label.len() }
fn pass(h: Holder) -> Holder { h }

fn main() {
    let mut n = 0i64;
    let mut i = 0i64;
    while i < 200i64 {
        // Inline struct-literal arg: the caller temp owns the body AND the
        // buffer, so this arm has to fire exactly once and free exactly once.
        let mut b1: Vec[i64] = Vec.new();
        b1.push(i);
        n = n + consume(Holder { label: "lit", r: Res { tag: 1, buf: b1 } });

        // Return passthrough: the callee entry-copies and returns an
        // independent copy, so the caller temp's MEMORY is freed here but its
        // BODY belongs to `p`. Firing both is the use-after-free this catches.
        let mut b2: Vec[i64] = Vec.new();
        b2.push(i);
        let p = pass(Holder { label: "pt", r: Res { tag: 1, buf: b2 } });
        n = n + p.r.tag;
        i = i + 1;
    }
    println(n);
}
"#,
        // Per iteration: 1 + 3 ("lit") from `consume`, plus 1 from `p.r.tag`
        // = 5. ×200 = 1000.
        &["1000"],
        "owned_aggregate_temp_field_drop_fires_once",
    );
}

/// B-2026-09-07-17 — an RC-FALLBACK-PROMOTED local's user `Drop` ran over
/// the box after it had already been released, and freed it a second time.
///
/// The ownership pass's loop-of-consume rule promotes a binding consumed
/// inside a loop, so its alloca stops holding the value and starts holding
/// a `{i64 rc, T}` box handle. The `let` site went on registering the
/// value-typed `karac_drop_<T>` against that 8-byte pointer slot — exactly
/// the hazard the gate three lines above it spells out for a `shared`
/// struct ("pass `alloca` — the slot holding the heap *pointer* — to
/// `<T>.drop`"), which RC-fallback promotion had no equivalent of.
///
/// THE LOOP BODY NEVER RUNS in the first cell, which is what makes the
/// shape worth pinning: the promotion is a static decision, so the defect
/// does not need the consume to execute. Measured on the parent: `drop P 0`
/// against the interpreter's `drop P 38`, 19 allocations against 20 frees,
/// three invalid reads and an invalid write into the released box, and an
/// invalid free of the box itself.
///
/// Cell 2 is the same shape with the loop running, cell 3 a struct that
/// merely CARRIES a Drop-bearing field (whose bodies-only walk had the same
/// slot problem), and cell 4 the FIELD-projection consume, whose body was
/// lost outright on the compiled backends rather than merely misread. Cell
/// 5 is the control that must stay clean: no `Drop` anywhere, so the box's
/// own field-free walk is still the right answer.
#[test]
fn asan_rc_fallback_boxed_local_drops_through_its_box() {
    const OWN: &str = "struct P { a: String, b: i64 }\n\
             impl Drop for P { fn drop(mut ref self) { println(f\"drop P {self.a.len()}\"); } }\n\
             fn seed() -> i64 { env.args().len() }\n\
             fn payload() -> String { f\"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa\" }\n\
             fn mkp(n: i64) -> P { return P { a: payload(), b: n }; }\n\
             fn takep(p: P) -> i64 { return p.b; }\n\
             fn main() { println(go()); }\n";
    // The loop-of-consume promotion fires on the CONSUME's presence, not on
    // the trip count, so the never-entered loop is the sharpest cell.
    assert_clean_asan_run_min_allocs(
        &format!(
            "{OWN}fn go() -> i64 {{ let t = mkp(9); let mut i = 0i64;\n\
                 \x20 while i < 0i64 {{ takep(t); i = i + 1; }}\n\
                 \x20 return 1; }}\n"
        ),
        &["drop P 38", "1"],
        "rc_fb_own_drop_loop_never_entered",
        10,
    );
    assert_clean_asan_run_min_allocs(
        &format!(
            "{OWN}fn go() -> i64 {{ let t = mkp(9); let mut i = 0i64;\n\
                 \x20 while i < 3i64 {{ let k = takep(t); i = i + k - k + 1; }}\n\
                 \x20 return 1; }}\n"
        ),
        &["drop P 38", "1"],
        "rc_fb_own_drop_loop_entered",
        10,
    );
    // A struct with NO `Drop` of its own but a Drop-BEARING FIELD: the
    // bodies-only walk is registered on the same slot and had the same
    // pointer-to-pointer confusion.
    const FIELD: &str = "struct S { v: String }\n\
             impl Drop for S { fn drop(mut ref self) { println(f\"drop S {self.v.len()}\"); } }\n\
             struct P { a: S, b: i64 }\n\
             fn seed() -> i64 { env.args().len() }\n\
             fn payload() -> String { f\"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa\" }\n\
             fn mkp(n: i64) -> P { return P { a: S { v: payload() }, b: n }; }\n\
             fn takep(p: P) -> i64 { return p.b; }\n\
             fn takes(s: S) -> i64 { return s.v.len(); }\n\
             fn main() { println(go()); }\n";
    assert_clean_asan_run_min_allocs(
        &format!(
            "{FIELD}fn go() -> i64 {{ let t = mkp(9); let mut i = 0i64;\n\
                 \x20 while i < 0i64 {{ takep(t); i = i + 1; }}\n\
                 \x20 return 1; }}\n"
        ),
        &["drop S 38", "1"],
        "rc_fb_field_drop_whole_consume",
        10,
    );
    // The FIELD-projection consume. On the parent this printed NOTHING at
    // all on the compiled backends — the body was lost, not merely misread.
    assert_clean_asan_run_min_allocs(
        &format!(
            "{FIELD}fn go() -> i64 {{ let t = mkp(9); let mut i = 0i64;\n\
                 \x20 while i < 0i64 {{ let s = t.a; i = i + s.v.len(); }}\n\
                 \x20 return 1; }}\n"
        ),
        &["drop S 38", "1"],
        "rc_fb_field_drop_projection_consume",
        10,
    );
    // CONTROL — no `Drop` anywhere, so the box's own heap-field walk is
    // still the whole answer and nothing may change for it.
    assert_clean_asan_run_min_allocs(
        "struct S { v: String }\n\
             struct P { a: S, b: i64 }\n\
             fn seed() -> i64 { env.args().len() }\n\
             fn payload() -> String { f\"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa\" }\n\
             fn mkp(n: i64) -> P { return P { a: S { v: payload() }, b: n }; }\n\
             fn takep(p: P) -> i64 { return p.b; }\n\
             fn go() -> i64 { let t = mkp(9); let mut i = 0i64;\n\
             \x20 while i < 0i64 { takep(t); i = i + 1; }\n\
             \x20 return 1; }\n\
             fn main() { println(go()); }\n",
        &["1"],
        "rc_fb_no_drop_anywhere_control",
        // 8, measured. The `Drop`-bearing cells above genuinely reach 10;
        // this control has no `Drop` body and so no body-side allocation,
        // which is exactly why it is the control. The shared 10 was an
        // estimate applied across cells with different real counts, and the
        // raw-count predicate could not tell them apart (B-2026-09-07-26).
        8,
    );
}

/// B-2026-09-07-28 — the TUPLE sibling of the two boxes above, which the
/// enum row offered as its clean contrast and got half right.
///
/// A tuple IS the struct-shaped layout `emit_aggregate_heap_field_frees`
/// assumes, so the box's MEMORY half really was correct for the common
/// shapes — that is what the enum row measured. What it did not measure is
/// the BODY: no element-bodies walk was ever armed on the box, so
/// `let t = (S { .. }, 7)` consumed in a loop printed nothing against the
/// interpreter's `drop S` while measuring allocation-balanced.
///
/// The tuple needs its own arm rather than a widened name lookup because
/// the other two arms resolve everything from a type NAME and a tuple has
/// none; the element `TypeExpr`s are its substitute identity, and the `let`
/// site already resolves them for the non-boxed spelling of the same
/// binding.
///
/// The memory half moved to the same `TypeExpr` walk in the process, which
/// is what the last two cells pin: with NO user `Drop` anywhere, an enum
/// element and an `Option` element each lost their payload to the
/// enum-blind aggregate walk (7 B in 1 block, pre-existing and unrelated to
/// any body).
#[test]
fn asan_rc_fallback_boxed_tuple_local_drops_through_its_box() {
    const OWN: &str = "struct S { s: String }\n\
             impl Drop for S { fn drop(mut ref self) { println(\"drop S\"); } }\n\
             fn seed() -> i64 { env.args().len() }\n\
             fn payload() -> String { f\"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa\" }\n\
             fn mkt() -> (S, i64) { return (S { s: payload() }, 7); }\n\
             fn take(t: (S, i64)) -> i64 { return 1; }\n\
             fn main() { println(go()); }\n";
    // As in the struct and enum fixtures, the promotion fires on the
    // CONSUME's presence rather than the trip count.
    assert_clean_asan_run_min_allocs(
        &format!(
            "{OWN}fn go() -> i64 {{ let t = mkt(); let mut i = 0i64;\n\
                 \x20 while i < 0i64 {{ take(t); i = i + 1; }}\n\
                 \x20 return 1; }}\n"
        ),
        &["drop S", "1"],
        "rc_fb_tuple_elem_drop_loop_never_entered",
        // 8, measured on both hosts; the 9 was an estimate. See
        // B-2026-09-07-26 and [`asan_alloc_floor`].
        8,
    );
    assert_clean_asan_run_min_allocs(
        &format!(
            "{OWN}fn go() -> i64 {{ let t = mkt(); let mut i = 0i64;\n\
                 \x20 while i < 3i64 {{ let k = take(t); i = i + k - k + 1; }}\n\
                 \x20 return 1; }}\n"
        ),
        &["drop S", "1"],
        "rc_fb_tuple_elem_drop_loop_entered",
        // 8, measured on both hosts; the 9 was an estimate. See
        // B-2026-09-07-26 and [`asan_alloc_floor`].
        8,
    );
    // The NESTED spelling the row flagged as untested: the element is a
    // struct that CARRIES a `Drop`-bearing field rather than declaring
    // `Drop` itself, so the body is reached one level down.
    assert_clean_asan_run_min_allocs(
        "struct R { s: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop R {self.s.len()}\"); } }\n\
             struct W { r: R, n: i64 }\n\
             fn seed() -> i64 { env.args().len() }\n\
             fn payload() -> String { f\"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa\" }\n\
             fn mkt() -> (W, i64) { return (W { r: R { s: payload() }, n: 3 }, 7); }\n\
             fn take(t: (W, i64)) -> i64 { return 1; }\n\
             fn go() -> i64 { let t = mkt(); let mut i = 0i64;\n\
             \x20 while i < 0i64 { take(t); i = i + 1; }\n\
             \x20 return 1; }\n\
             fn main() { println(go()); }\n",
        &["drop R 38", "1"],
        "rc_fb_tuple_nested_field_drop_body",
        // AUDITED, per cell (B-2026-09-07-26). Every floor in this family is
        // now the count `KARAC_ASAN_ALLOC_AUDIT=1` reports for that exact
        // cell, not a family-wide estimate: most sit at 8, the `Drop`-body
        // cells at 10, `rc_boxed_proj_mutated_destination` at 15 and
        // `rc_fb_twin_shape_both_boxed` at 183. Until the predicate became
        // floor-relative none of them could be checked — the comparison was
        // against ASAN's raw process-wide count, whose host start-up floor
        // (10 arm64 Linux, 199 macOS) exceeds most of these numbers on its
        // own. See [`asan_alloc_floor`].
        10,
    );
    // An ENUM element with a `Drop` of its own — body through the tuple
    // walk, and the payload memory the aggregate walk could not reach.
    assert_clean_asan_run_min_allocs(
        "enum E { A(String), B }\n\
             impl Drop for E { fn drop(mut ref self) { println(\"drop E\"); } }\n\
             fn seed() -> i64 { env.args().len() }\n\
             fn payload() -> String { f\"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa\" }\n\
             fn mkt() -> (E, i64) { return (E.A(payload()), 7); }\n\
             fn take(t: (E, i64)) -> i64 { return 1; }\n\
             fn go() -> i64 { let t = mkt(); let mut i = 0i64;\n\
             \x20 while i < 0i64 { take(t); i = i + 1; }\n\
             \x20 return 1; }\n\
             fn main() { println(go()); }\n",
        &["drop E", "1"],
        "rc_fb_tuple_enum_elem_own_drop",
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
    // MEMORY ONLY, no user `Drop` anywhere in either program: the enum
    // element and the `Option` element each leaked their payload to the
    // enum-blind aggregate walk before the `TypeExpr` walk replaced it.
    assert_clean_asan_run_min_allocs(
        "enum E { A(String), B }\n\
             fn seed() -> i64 { env.args().len() }\n\
             fn payload() -> String { f\"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa\" }\n\
             fn mkt() -> (E, i64) { return (E.A(payload()), 7); }\n\
             fn take(t: (E, i64)) -> i64 { return 1; }\n\
             fn go() -> i64 { let t = mkt(); let mut i = 0i64;\n\
             \x20 while i < 0i64 { take(t); i = i + 1; }\n\
             \x20 return 1; }\n\
             fn main() { println(go()); }\n",
        &["1"],
        "rc_fb_tuple_enum_elem_no_drop_memory",
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
    assert_clean_asan_run_min_allocs(
        "fn seed() -> i64 { env.args().len() }\n\
             fn payload() -> String { f\"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa\" }\n\
             fn mkt() -> (Option[String], i64) { return (Option.Some(payload()), 7); }\n\
             fn take(t: (Option[String], i64)) -> i64 { return 1; }\n\
             fn go() -> i64 { let t = mkt(); let mut i = 0i64;\n\
             \x20 while i < 0i64 { take(t); i = i + 1; }\n\
             \x20 return 1; }\n\
             fn main() { println(go()); }\n",
        &["1"],
        "rc_fb_tuple_option_elem_no_drop_memory",
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
    // CONTROLS — the shapes both walks already covered, which must stay
    // byte-identical, and the same tuple NOT promoted.
    assert_clean_asan_run_min_allocs(
        "fn seed() -> i64 { env.args().len() }\n\
             fn payload() -> String { f\"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa\" }\n\
             fn mkt() -> (String, i64) { return (payload(), 7); }\n\
             fn take(t: (String, i64)) -> i64 { return 1; }\n\
             fn go() -> i64 { let t = mkt(); let mut i = 0i64;\n\
             \x20 while i < 0i64 { take(t); i = i + 1; }\n\
             \x20 return 1; }\n\
             fn main() { println(go()); }\n",
        &["1"],
        "rc_fb_tuple_string_elem_control",
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
    assert_clean_asan_run_min_allocs(
        &format!("{OWN}fn go() -> i64 {{ let t = mkt(); return 1; }}\n"),
        &["drop S", "1"],
        "rc_fb_tuple_unpromoted_control",
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

/// B-2026-09-07-18 — the box runs the BINDING's own `Drop`, not that of a
/// same-shaped twin.
///
/// B-2026-09-07-17 named the boxed value by reverse lookup over
/// `struct_types`, on the premise that "struct types are named and interned,
/// so the reverse lookup is exact". `declare_structs` builds every struct
/// with `context.struct_type(..)` — a LITERAL type, interned STRUCTURALLY —
/// so `P` and `Q` below are one `StructType` and the lookup chose between
/// their names by HashMap iteration order. Eight identical compiles of this
/// program printed `drop P` six times and `drop Q` twice.
///
/// The fixture pins the two halves that make the answer stable: the name is
/// read off the BINDING (`var_type_names`, which the `ast_hint` lookup ~200
/// lines above already fills in with an ambiguity-refusing,
/// `drop_method_keys`-excluding answer), and it is validated against the
/// boxed LLVM type before use so a shadowing `let`'s stale entry cannot
/// stand in.
#[test]
fn asan_rc_fallback_box_runs_its_own_types_drop_not_a_twins() {
    const TWINS: &str = "struct P { s: String, n: i64 }\n\
             impl Drop for P { fn drop(mut ref self) { println(f\"drop P {self.s.len()}\"); } }\n\
             struct Q { s: String, n: i64 }\n\
             impl Drop for Q { fn drop(mut ref self) { println(f\"drop Q {self.s.len()}\"); } }\n\
             fn seed() -> i64 { env.args().len() }\n\
             fn payload() -> String { f\"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa\" }\n\
             fn mkp() -> P { return P { s: payload(), n: 1i64 }; }\n\
             fn mkq() -> Q { return Q { s: payload(), n: 2i64 }; }\n\
             fn takep(p: P) -> i64 { return p.n; }\n\
             fn takeq(q: Q) -> i64 { return q.n; }\n";
    assert_clean_asan_run_min_allocs(
        &format!(
            "{TWINS}fn go() -> i64 {{ let t = mkp(); let mut i = 0i64;\n\
                 \x20 while i < 0i64 {{ takep(t); i = i + 1; }}\n\
                 \x20 return 1; }}\n\
                 fn main() {{ println(go()); }}\n"
        ),
        &["drop P 38", "1"],
        "rc_fb_twin_shape_single_box",
        10,
    );
    // BOTH twins promoted in one module. The box heap type is
    // `{i64, <value>}`, and it is interned structurally too, so the two
    // boxes are ONE LLVM type — which is what the per-box-type memo in
    // `register_rc_fallback_box_drop` is keyed on. Naming the value
    // correctly is necessary but not sufficient if the memo then hands the
    // second box the first one's fn.
    assert_clean_asan_run_min_allocs(
        &format!(
            "{TWINS}fn gop() -> i64 {{ let t = mkp(); let mut i = 0i64;\n\
                 \x20 while i < 0i64 {{ takep(t); i = i + 1; }}\n\
                 \x20 return 1; }}\n\
                 fn goq() -> i64 {{ let u = mkq(); let mut j = 0i64;\n\
                 \x20 while j < 0i64 {{ takeq(u); j = j + 1; }}\n\
                 \x20 return 2; }}\n\
                 fn main() {{ println(gop()); println(goq()); }}\n"
        ),
        &["drop P 38", "1", "drop Q 38", "2"],
        "rc_fb_twin_shape_both_boxed",
        // Measured: 183 on macOS 26.6 / M5, 75 on arm64 Linux, 64-72 on
        // x86_64 Linux. The old note here called that HOST-dependence and
        // guessed at a per-`println` cost on macOS. It is not the host: the
        // count moves ON ONE HOST with CPU CONTENTION (B-2026-09-09-4).
        // Measured on this box, affinity held constant, only load varying:
        //
        //     quiet    72        8 CPU hogs   62        taskset -c 0   54
        //
        // and it returns to 72 when the load goes away. `par` work
        // DISTRIBUTION is the variable — a saturated box spreads less work
        // across workers and so allocates fewer per-worker blocks — which
        // is why re-running the test alone always "fixes" it and why a
        // full-suite run is where it fails. A 60 floor is inside that
        // range, so the cell failed the suite on a green tree.
        //
        // Floored at lowest-observed minus twice the observed swing
        // (64 - 2*7), rounded down. A folded-away payload still trips it:
        // the sibling `single_box` cell pins the same mechanism at a
        // contention-INSENSITIVE 10 (bit-identical across three sweeps), so
        // a collapse here lands far below 40.
        40,
    );
}

/// B-2026-08-25-11 — a monomorph's receiver drop was selected from an
/// ELEMENTLESS instantiation, so its per-element drop was an empty stub and
/// the elements it owned leaked.
///
/// `concrete_generic_struct_inst` records the receiver's concrete
/// instantiation by resolving each bare type param through
/// `type_subst_names`, which is name -> NAME. A param bound to a type that
/// has generic args of its own (`T = Vec[i64]`) cannot survive that
/// round-trip: it came back as the bare head `Vec`, and the recorded
/// instantiation became `Heap[Vec]`. The scope-exit drop selected from it
/// mangled `__karac_drop_struct_Heap$Vec`, whose per-element drop is
/// `karac_drop_Vec` — body `ret void`, because a `Vec` with no element gives
/// the drop synthesizer nothing to free. The caller's own drop of the same
/// struct resolved correctly to `$Vec_i64`, so one program emitted both and
/// only the monomorph's elements leaked. Fixed by preferring the
/// element-aware `type_subst_type_exprs`, the precedence
/// `subst_monomorph_type_params` already used.
///
/// `take` deliberately removes ONE element and drops the rest: a method that
/// drains the container completely leaves nothing behind and is CLEAN even
/// while the bug is fully present. An earlier draft of this fixture used
/// `into_sorted` and passed pre-fix for exactly that reason.
///
/// (b) and (c) are controls that were already clean, and they are what
/// identify the trigger: `String` and `i64` are SINGLE NAMES, which the
/// name -> name map represents losslessly, so only a param bound to a
/// generic-args-carrying type reaches the defect. (d) is the non-generic
/// twin. Verified RED pre-fix: 24 bytes leaked in 2 allocations — exactly
/// the two elements case (a) leaves behind.
#[test]
fn asan_mono_receiver_drop_frees_elements_of_a_generic_arg_bearing_param() {
    assert_clean_asan_run(
        r#"
struct Heap[T] { xs: Vec[T] }
impl[T] Heap[T] {
    fn pop_one(mut ref self) -> Option[T] { self.xs.pop() }
    // Takes ONE element and drops the rest on the floor. The elements left
    // behind are the ones that leaked.
    fn take(self) -> Option[T] { let mut h = self; h.pop_one() }
}
struct PlainHeap { xs: Vec[Vec[i64]] }
impl PlainHeap {
    fn pop_one(mut ref self) -> Option[Vec[i64]] { self.xs.pop() }
    fn take(self) -> Option[Vec[i64]] { let mut h = self; h.pop_one() }
}
fn main() {
    // (a) the filed shape: `T` bound to `Vec[i64]`, which carries its own
    // generic arg and so cannot survive a name-to-name substitution.
    let a = Heap { xs: [[1, 2], [3], [4, 5, 6]] };
    match a.take() { Some(v) => { println(f"a={v.len()}"); } None => {} }
    // (b) control: HEAP-allocated Strings. Clean pre-fix — `String` is a single
    // name, so the head-only map loses nothing.
    let mut ss: Vec[String] = Vec.new();
    let mut i = 0;
    while i < 3 { ss.push(f"item{i}"); i = i + 1; }
    let b = Heap { xs: ss };
    match b.take() { Some(v) => { println(f"b={v}"); } None => {} }
    // (c) control: scalar element.
    let c = Heap { xs: [7, 8, 9] };
    match c.take() { Some(v) => { println(f"c={v}"); } None => {} }
    // (d) control: the NON-generic twin at the same element type.
    let d = PlainHeap { xs: [[1], [2], [3]] };
    match d.take() { Some(v) => { println(f"d={v.len()}"); } None => {} }
}
"#,
        &["a=3", "b=item2", "c=9", "d=1"],
        "mono-receiver-drop-elementless-inst",
    );
}

/// B-2026-08-25-16 — a materialized RECEIVER TEMPORARY was dropped by bare
/// name, so its element buffers leaked.
///
/// `Heap { .. }.take()` materializes the receiver into a `__urecv_tmp` slot
/// and drop-tracks it via `track_struct_var`, i.e.
/// `track_struct_var_inst(.., None)`. The name-shared
/// `__karac_drop_struct_Heap` resolves the `xs: Vec[T]` field from the
/// erased `T` and is outer-only: it freed the outer buffer and never walked
/// the elements. The instantiation was already in hand — the same block
/// seeds `enum_inst_var_types[synth]` so the CALLEE is selected at the right
/// monomorph — so a program emitted the correct `$Vec_i64` drop for its
/// named bindings and the erased one for its temporaries.
///
/// (c) is the control that localises it: the identical call on a BOUND
/// receiver was always clean, because the `let` site records the
/// instantiation and the binding's drop is selected from it. Only the
/// temporary took the name-keyed path.
///
/// (a) uses three DIFFERENT element sizes on purpose. With uniform sizes the
/// leak report cannot say which buffers survived, and an earlier reading of
/// this bug mis-recorded "only two of three leak" from a uniform fixture —
/// distinct sizes show all three, and the outer buffer's absence from the
/// report is what proves the temp was dropped outer-only rather than not at
/// all. Verified RED pre-fix: 58 bytes in 5 allocations — (a)'s 8 + 16 + 24
/// plus (b)'s two surviving 5-byte Strings.
#[test]
fn asan_receiver_temporary_drop_frees_its_elements() {
    assert_clean_asan_run(
        r#"
struct Heap[T] { xs: Vec[T] }
impl[T] Heap[T] {
    fn take(self) -> Option[T] { let mut h = self; h.xs.pop() }
}
struct PlainHeap { xs: Vec[Vec[i64]] }
impl PlainHeap {
    fn take(self) -> Option[Vec[i64]] { let mut h = self; h.xs.pop() }
}
fn main() {
    // (a) TEMPORARY receiver, nested-Vec elements at three distinct sizes.
    match Heap { xs: [[1], [2, 2], [3, 3, 3]] }.take() {
        Some(v) => { println(f"a={v.len()}"); } None => {}
    }
    // (b) TEMPORARY receiver, HEAP-allocated String elements.
    let mut ss: Vec[String] = Vec.new();
    let mut i = 0;
    while i < 3 { ss.push(f"item{i}"); i = i + 1; }
    match Heap { xs: ss }.take() { Some(v) => { println(f"b={v}"); } None => {} }
    // (c) control: BOUND receiver, same everything. Clean before the fix.
    let c = Heap { xs: [[9], [8, 8]] };
    match c.take() { Some(v) => { println(f"c={v.len()}"); } None => {} }
    // (d) control: NON-generic twin with a temporary receiver.
    match PlainHeap { xs: [[1], [2]] }.take() { Some(v) => { println(f"d={v.len()}"); } None => {} }
    // (e) control: scalar element, nothing inner to own.
    match Heap { xs: [5, 6, 7] }.take() { Some(v) => { println(f"e={v}"); } None => {} }
}
"#,
        &["a=3", "b=item2", "c=2", "d=1", "e=7"],
        "receiver-temporary-drop-elements",
    );
}

#[test]
fn asan_priority_queue_peek_drop_count_is_one_per_returned_copy() {
    assert_clean_asan_run(
        r#"
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Item { id: i64, name: String }
impl Drop for Item {
    fn drop(mut ref self) { println(f"drop {self.id} {self.name}") }
}
fn main() {
    let mut q: PriorityQueue[Item] = PriorityQueue.new();
    q.push(Item { id: 3, name: f"ccc{3}" });
    q.push(Item { id: 1, name: f"a{1}" });
    q.push(Item { id: 2, name: f"bb{2}" });
    println("built");
    match q.peek() { Some(v) => { println(f"peek {v.id}"); } None => {} }
    match q.peek() { Some(v) => { println(f"peek {v.id}"); } None => {} }
    println(q.len());
    println("draining");
    while q.len() > 0 {
        match q.pop() { Some(v) => { println(f"pop {v.id}"); } None => {} }
    }
    println("end");
}
"#,
        &[
            "built",
            "peek 1",
            "drop 1 a1",
            "peek 1",
            "drop 1 a1",
            "3",
            "draining",
            "pop 1",
            "drop 1 a1",
            "pop 2",
            "drop 2 bb2",
            "pop 3",
            "drop 3 ccc3",
            "end",
        ],
        "priority-queue-peek-drop-count",
    );
}

/// B-2026-08-26-9, the FREE-FUNCTION leg — and the one no run-vs-build
/// check could have caught, because the interpreter got it wrong the same
/// way AOT did (both printed `drop 7` twice). The `String` field is
/// load-bearing here rather than incidental: the fix suppresses the
/// argument's Drop BODY but must keep its MEMORY registration, since the
/// callee's `push` defensive-copies the buffer and leaves the caller's
/// original orphaned. Suppressing both halves traded the double body for a
/// silent 9-byte leak, which is exactly what this fixture would catch.
#[test]
fn asan_free_fn_storing_a_by_value_param_into_a_ref_param_drops_once() {
    assert_clean_asan_run(
        r#"
struct Item { id: i64, name: String }
impl Drop for Item { fn drop(mut ref self) { println(f"drop {self.id} {self.name}") } }
fn add_to(v: mut ref Vec[Item], x: Item) { v.push(x); }
fn main() {
    let mut v: Vec[Item] = Vec.new();
    add_to(mut v, Item { id: 7, name: f"nn{7}" });
    println("built");
    while v.len() > 0 { match v.pop() { Some(e) => { println(f"pop {e.id}"); } None => {} } }
    println("end");
}
"#,
        &["built", "pop 7", "drop 7 nn7", "end"],
        "freefn-store-into-ref-param-drop-once",
    );
}

/// B-2026-08-26-9, the METHOD leg (`self.xs.push(x)` on `mut ref self`) —
/// the shape `PriorityQueue.push` has and the only one of the three that
/// showed up as a run-vs-build divergence, since the interpreter runs its
/// fresh-temp arg drops on the free-function path only. Same bodies-vs-
/// memory split as the free-fn sibling above.
#[test]
fn asan_method_storing_a_by_value_param_into_self_drops_once() {
    assert_clean_asan_run(
        r#"
struct Item { id: i64, name: String }
impl Drop for Item { fn drop(mut ref self) { println(f"drop {self.id} {self.name}") } }
struct Bag { xs: Vec[Item] }
impl Bag { fn add(mut ref self, x: Item) { self.xs.push(x); } }
fn main() {
    let mut b = Bag { xs: Vec.new() };
    b.add(Item { id: 7, name: f"nn{7}" });
    println("built");
    while b.xs.len() > 0 { match b.xs.pop() { Some(e) => { println(f"pop {e.id}"); } None => {} } }
    println("end");
}
"#,
        &["built", "pop 7", "drop 7 nn7", "end"],
        "method-store-into-self-drop-once",
    );
}

/// B-2026-08-28-1 — the MEMORY half of registering a user `Drop` body on a
/// destructure leaf.
///
/// The fix routes a leaf whose type declares `impl Drop` through the
/// `karac_drop_<T>` WRAPPER, which is body + fields + memory in ONE action,
/// and therefore has to REPLACE the leaf's memory registration rather than
/// join it. Registering both frees the same buffers twice, so every case
/// here carries a heap `String` field: with an i64-only payload a double
/// free has nothing to free and ASAN stays quiet.
///
/// The tuple-PARAM case is the control that must not double-DROP either —
/// its source already owns the elements, so the leaf takes memory only, and
/// the expected stdout below pins the body at exactly one occurrence.
///
/// The rodata case guards the tuple-LITERAL widening: `("aa", 1)` holds a
/// static element, so a leaf free that was not cap-guarded would free
/// read-only memory.
#[test]
fn asan_user_drop_body_on_a_let_destructure_leaf() {
    for (label, prog, want) in [
        (
            "tuple-local",
            r#"
struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"drop {self.name}"); } }
fn main() {
    let p = (R { id: 1, name: "payload-aa" }, 2);
    let (r, n) = p;
    println(f"n={r.id + n}");
}
"#,
            &["n=3", "drop payload-aa"][..],
        ),
        (
            "call-result",
            r#"
struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"drop {self.name}"); } }
fn make() -> (R, i64) { return (R { id: 1, name: "payload-bb" }, 2); }
fn main() {
    let (r, n) = make();
    println(f"n={r.id + n}");
}
"#,
            &["n=3", "drop payload-bb"][..],
        ),
        (
            "tuple-literal",
            r#"
struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"drop {self.name}"); } }
fn main() {
    let (r, n) = (R { id: 1, name: "payload-cc" }, 2);
    println(f"n={r.id + n}");
}
"#,
            &["n=3", "drop payload-cc"][..],
        ),
        (
            "field-bearing-leaf",
            r#"
struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"drop {self.name}"); } }
struct W { r: R }
fn main() {
    let p = (W { r: R { id: 1, name: "payload-dd" } }, 2);
    let (w, n) = p;
    println(f"n={w.r.id + n}");
}
"#,
            &["n=3", "drop payload-dd"][..],
        ),
        (
            "tuple-param-control",
            r#"
struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"drop {self.name}"); } }
fn take(p: (R, i64)) {
    let (r, n) = p;
    println(f"n={r.id + n}");
}
fn main() { take((R { id: 1, name: "payload-ee" }, 2)); }
"#,
            &["n=3", "drop payload-ee"][..],
        ),
        (
            "rodata-literal-element",
            r#"
fn main() {
    let (a, b) = ("aa", 1);
    println(f"{a}{b}");
}
"#,
            &["aa1"][..],
        ),
    ] {
        assert_clean_asan_run(prog, want, label);
    }
}

/// B-2026-08-28-65 — the retained-and-flagged `Drop` action of a local
/// returned from a NESTED `return` stays memory-balanced on both paths.
///
/// The behavioural twin
/// (`e2e_nested_return_local_user_drop_body_runs_on_the_fallthrough`) proves
/// the BODY count; this proves the fix did not buy that body with a double
/// free or a leak, which is the specific risk of replacing a compile-time
/// frame removal with a runtime guard. Pre-fix the fall-through row ran no
/// body at all, so a naive repair — dropping the removal without the flag —
/// would free the returned buffer in the callee AND at the caller.
///
/// The two directions are a pair and must stay that way. `fallthrough` is
/// the leak side: `h` dies in the callee, so its buffer must be freed there.
/// `return-taken` is the double-free side: `h` escapes, so the callee must
/// free nothing and the caller frees once. A guard that is never cleared
/// passes the first and double-frees the second; a removal that is never
/// replaced passes the second and leaks the first.
///
/// `displaced` covers the predicate the fix changes the answer of: retaining
/// the action makes `has_armed_user_drop` true, which re-enables the
/// displaced-value leg's body AND its frees on the reassignment. Firing that
/// on a moved-from slot is B-2026-07-31-38's shape and would show here as a
/// free through moved-from bits rather than as wrong stdout.
#[test]
fn asan_nested_return_local_drop_body_is_memory_balanced() {
    const DROPPER: &str = "struct H { id: i64, name: String }\n\
             impl Drop for H { fn drop(mut ref self) { println(f\"drop {self.name}\") } }\n";
    for (label, body, want) in [
        (
            "fallthrough",
            "fn take(k: bool) -> H { let h = H { id: 41, name: f\"n{41}\" };\n\
                 \x20  if k { return h; } H { id: 99, name: f\"n{99}\" } }\n\
                 fn main() { let x = take(false); println(f\"{x.id}\"); }\n",
            vec!["drop n41", "99", "drop n99"],
        ),
        (
            "return-taken",
            "fn take(k: bool) -> H { let h = H { id: 41, name: f\"n{41}\" };\n\
                 \x20  if k { return h; } H { id: 99, name: f\"n{99}\" } }\n\
                 fn main() { let x = take(true); println(f\"{x.id}\"); }\n",
            vec!["41", "drop n41"],
        ),
        (
            "displaced",
            "fn take(k: bool) -> H { let mut h = H { id: 41, name: f\"n{41}\" };\n\
                 \x20  if k { return h; }\n\
                 \x20  h = H { id: 99, name: f\"n{99}\" }; h }\n\
                 fn main() { let x = take(false); println(f\"{x.id}\"); }\n",
            vec!["drop n41", "99", "drop n99"],
        ),
        // CONTROL — the UNCONDITIONAL `return`, which keeps the static
        // removal. Its memory answer must be untouched by the fix.
        (
            "unconditional-return-control",
            "fn take() -> H { let h = H { id: 41, name: f\"n{41}\" }; return h; }\n\
                 fn main() { let x = take(); println(f\"{x.id}\"); }\n",
            vec!["41", "drop n41"],
        ),
    ] {
        assert_clean_asan_run(&format!("{DROPPER}{body}"), &want, label);
    }
}

/// B-2026-08-28-53 — reordering a DISCARDED own-`Drop` parent temp's body
/// ahead of the returned field's stays memory-balanced.
///
/// The behavioural twin
/// (`e2e_discarded_own_drop_parent_temp_orders_its_body_first`) proves the
/// ORDER; this proves the reorder did not buy it with a use-after-free,
/// which is the specific risk. The parent's drop wrapper frees its heap
/// fields, and the result is a field moved OUT of that parent — so putting
/// the parent's body (and its frees) first is exactly the sequence that
/// could hand the field's body freed memory.
///
/// It cannot, and `bound` is why rather than an argument: that spelling
/// ALREADY ran in this order before the fix, over this same shape, and
/// measured clean. The moved-out field is masked out of the parent's
/// wrapper (`karac_dropnf_<T>`), so the parent never frees what it handed
/// on. `discarded` and `discarded-wildcard` are the two spellings the fix
/// moved into that same order.
///
/// Both bodies READ their heap field, deliberately: a body printing a
/// literal would let LLVM delete the allocation at -O2 and assert nothing.
#[test]
fn asan_discarded_own_drop_parent_temp_order_is_memory_balanced() {
    const D: &str = "struct R { id: i64, name: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop R{self.name}\") } }\n\
             struct W { r: R, tag: String }\n\
             impl Drop for W { fn drop(mut ref self) { println(f\"drop W{self.tag}\") } }\n\
             #[allow(partial_move_of_drop_struct)]\n\
             fn take(w: W) -> R { let W { r, tag } = w; r }\n";
    const MK: &str = "W { r: R { id: 47, name: f\"n{47}\" }, tag: f\"t{5}\" }";
    for (label, body, want) in [
        (
            "discarded",
            format!("{D}fn main() {{ take({MK}); println(\"end\"); }}\n"),
            vec!["drop Wt5", "drop Rn47", "end"],
        ),
        (
            "discarded-wildcard",
            format!("{D}fn main() {{ let _ = take({MK}); println(\"end\"); }}\n"),
            vec!["drop Wt5", "drop Rn47", "end"],
        ),
        // The spelling that already ran in this order before the fix — the
        // proof that parent-frees-then-field-body is safe for this shape.
        (
            "bound",
            format!(
                "{D}fn main() {{ let got = take({MK});\n\
                     \x20            println(f\"got {{got.id}}\"); println(\"end\"); }}\n"
            ),
            vec!["drop Wt5", "got 47", "drop Rn47", "end"],
        ),
    ] {
        assert_clean_asan_run(&body, &want, label);
    }
}

/// B-2026-09-03-31 — a struct with its own `impl Drop` never released a
/// `shared struct` field's RC box.
///
/// `__karac_drop_struct_<T>` skips a direct `shared` / `Option[shared]`
/// scalar field ON PURPOSE: those are RC machinery, not buffer-owned, and
/// the contract (B-2026-06-14-28 #3) is that the OWNER's own cleanup rc-decs
/// them. `track_struct_var_inst` honours it — it asks
/// `struct_owns_shared_field_subst` and registers the COMBINED
/// `__karac_vec_elem_full_drop_<T>` (value drop + rc-dec walker). The three
/// user-`Drop` wrappers did not, and for a `Drop`-bearing type the wrapper is
/// the ONLY cleanup that runs — `UserDrop` and `StructDrop` are mutually
/// exclusive by construction. So the handle was released by nobody.
///
/// WHAT ISOLATED IT was a one-line discriminator, not a trace: the same
/// struct with the `impl Drop` DELETED is clean, which points at the wrapper
/// rather than the synthesizer. The leak needs no call and no assignment —
/// a plain `let` in `main` loses 16 B — and it is unbounded, one box per
/// live instance.
///
/// THE FIXTURE IS BUILT TO BITE AT `-O2`, which is where the row's own repro
/// does not. A constant-seeded `Sd { m: 7 }` whose `note` is never read folds
/// away entirely and measures clean at the default level — the level CI's
/// ASAN leg runs at — so a fixture written to the repro would have passed on
/// the pre-fix compiler. Seeding from `env.args().len()` (a stable 1, opaque
/// to the optimizer) and READING the payload from inside each `Drop` body
/// keeps the boxes alive through the pass pipeline. Measured against the
/// pre-fix compiler: 720 B definitely + 522 B indirectly lost at
/// `KARAC_OPT_LEVEL=0` AND 360 + 261 at the default `-O2`; both go to zero.
///
/// FIVE CELLS, and the second is the one that fails an over-eager fix:
///
///   * `one` — the row's shape, one owner.
///   * `two_owners` — ONE handle in TWO `Drop`-bearing structs. The refcount
///     is 2 and exactly one `dSd20` may print. A wrapper that released
///     unconditionally rather than through the rc-dec would double-free here.
///   * `opt` — an `Option[shared]` field, the other spelling the value drop
///     classifies as no-cleanup.
///   * `two_fields` — two shared fields on one struct, so a walker that
///     stops after the first shows up as a leak rather than as silence.
///   * `eat` — the struct passed BY VALUE, whose entry copy rc-incs and must
///     be balanced by the callee's own wrapper.
///
/// THE `dSd` LINES ARE NEW OUTPUT, and they are the point: pre-fix the
/// refcount never reached zero, so the shared child's `Drop` body never ran
/// at all. `--interp` still runs none of them — that is B-2026-09-03-9, whose
/// own title asserts "both compiled backends run it exactly once", i.e. this
/// fix is what makes the compiled half match the behaviour that row already
/// describes as correct. This harness asserts the compiled side only.
#[test]
fn asan_own_drop_struct_releases_its_shared_field_rc_box() {
    assert_clean_asan_run(
        r#"
shared struct Sd { m: i64, note: String }
impl Drop for Sd { fn drop(mut ref self) { println(f"dSd{self.m}-{self.note.len()}") } }

struct H { id: i64, s: Sd, tag: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}-{self.tag.len()}") } }

struct G { id: i64, s: Option[Sd], tag: String }
impl Drop for G { fn drop(mut ref self) { println(f"dG{self.id}-{self.tag.len()}") } }

struct T2 { id: i64, a: Sd, b: Sd }
impl Drop for T2 { fn drop(mut ref self) { println(f"dT{self.id}") } }

fn mk(n: i64) -> Sd { return Sd { m: n, note: f"note-payloadpayloadpayload-{n}" } }
fn tg(n: i64) -> String { return f"tag-payloadpayloadpayload-{n}" }

fn one(k: i64) -> i64 { let h: H = H { id: k, s: mk(10 * k), tag: tg(k) }; return h.tag.len() }
fn two_owners(k: i64) -> i64 { let g: Sd = mk(20 * k); let a: H = H { id: 2 * k, s: g, tag: tg(2) }; let b: H = H { id: 3 * k, s: g, tag: tg(3) }; return a.tag.len() + b.tag.len() }
fn opt(k: i64) -> i64 { let q: G = G { id: 4 * k, s: Option.Some(mk(30 * k)), tag: tg(4) }; return q.tag.len() }
fn two_fields(k: i64) -> i64 { let t: T2 = T2 { id: 5 * k, a: mk(40 * k), b: mk(41 * k) }; return t.id }
fn eat(x: H) -> i64 { return x.tag.len() }

fn main() {
    let k: i64 = env.args().len();
    let mut acc: i64 = 0;
    let mut i: i64 = 0;
    while i < 3 {
        acc = acc + one(k);
        acc = acc + two_owners(k);
        acc = acc + opt(k);
        acc = acc + two_fields(k);
        acc = acc + eat(H { id: 6 * k, s: mk(50 * k), tag: tg(6) });
        i = i + 1;
    }
    println(f"acc{acc}");
}
"#,
        &[
            "dH1-27", "dSd10-29", "dH3-27", "dH2-27", "dSd20-29", "dG4-27", "dSd30-29", "dT5",
            "dSd40-29", "dSd41-29", "dH6-27", "dSd50-29", "dH1-27", "dSd10-29", "dH3-27", "dH2-27",
            "dSd20-29", "dG4-27", "dSd30-29", "dT5", "dSd40-29", "dSd41-29", "dH6-27", "dSd50-29",
            "dH1-27", "dSd10-29", "dH3-27", "dH2-27", "dSd20-29", "dG4-27", "dSd30-29", "dT5",
            "dSd40-29", "dSd41-29", "dH6-27", "dSd50-29", "acc420",
        ],
        "b0903-31-own-drop-shared-field-rc",
    );
}

#[test]
fn asan_in_loop_rearmed_drop_flag_frees_each_iteration_exactly_once() {
    // B-2026-09-02-6 — the heap half of the in-loop drop-flag re-arm.
    //
    // The row's own repro carries a scalar plus a one-character tag, so the
    // whole defect there is a missing line of output: nothing about the
    // memory side is observable, and a fix can look complete against it
    // while leaving every restored body unbalanced. That matters more here
    // than usual, because the fix's entire effect is to make bodies run
    // that previously did NOT — which is the change most able to introduce
    // a double free — and the mechanism it does it with is a per-path flag,
    // where re-arming one iteration too many is a use-after-free on a husk
    // the caller still owns.
    //
    // Each `R` owns an eight-element `Vec[String]` of long strings, so a
    // body that fails to run strands nine allocations and one that runs
    // twice is a double free. `dR<id>-<len>` reads the Vec's length from
    // inside the body, so a body running on a moved-from object shows up as
    // a wrong count rather than passing quietly.
    //
    // The three shapes are the ledger row's, and the outer loop runs them
    // three times so a flag that fails to reset between CALLS is caught as
    // well as one that fails to reset between iterations. Pre-fix this
    // stranded nine bodies (three per call: `dR91`/`dR92` from the first
    // shape and `dR62` from the second); `outside` is the control whose
    // hand-over must survive every later iteration, and it is unchanged.
    //
    // Measured 0 definitely/indirectly lost under valgrind, and
    // byte-identical on all four surfaces.
    assert_clean_asan_run(
        r#"
struct R { id: i64, xs: Vec[String] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}-{self.xs.len()}") } }

fn mk(n: i64) -> R {
    let mut v: Vec[String] = Vec.new();
    let mut i: i64 = 0;
    while i < 8 { v.push(f"payloadpayload-{n}-{i}"); i = i + 1; }
    return R { id: n, xs: v }
}

fn while_first(p: R) -> i64 {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 3 {
        let mut out: R = mk(90 + i);
        if i == 0 { out = p; }
        acc = acc + out.id;
        i = i + 1;
    }
    return acc
}

fn while_middle(p: R) -> i64 {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 3 {
        let mut out: R = mk(60 + i);
        if i == 1 { out = p; }
        acc = acc + out.id;
        i = i + 1;
    }
    return acc
}

fn outside(p: R) -> i64 {
    let mut out: R = mk(70);
    let mut i: i64 = 0;
    while i < 3 {
        if i == 1 { out = p; }
        i = i + 1;
    }
    return out.id
}

fn main() {
    let mut i: i64 = 0;
    while i < 3 {
        println(f"a{while_first(mk(5))}");
        println(f"b{while_middle(mk(6))}");
        println(f"c{outside(mk(7))}");
        i = i + 1;
    }
    println("done");
}
"#,
        &[
            "dR90-8", "dR91-8", "dR92-8", "dR5-8", "a188", "dR60-8", "dR61-8", "dR62-8", "dR6-8",
            "b128", "dR70-8", "dR7-8", "c7", "dR90-8", "dR91-8", "dR92-8", "dR5-8", "a188",
            "dR60-8", "dR61-8", "dR62-8", "dR6-8", "b128", "dR70-8", "dR7-8", "c7", "dR90-8",
            "dR91-8", "dR92-8", "dR5-8", "a188", "dR60-8", "dR61-8", "dR62-8", "dR6-8", "b128",
            "dR70-8", "dR7-8", "c7", "done",
        ],
        "b6-in-loop-drop-flag-rearm",
    );
}

/// B-2026-09-05-4 — the LEAK half. A generic struct with its own
/// `impl[T] Drop`, passed as a temp-literal argument, registered no
/// cleanup at all: the caller materialized `%__owned_agg_tmp`, stored the
/// value into it and emitted nothing against it, so the Drop-bearing
/// field's heap was never released.
///
/// The run-vs-build half (the two missing `Drop` bodies) is covered by
/// `e2e_generic_own_drop_struct_temp_arg_runs_its_body_and_its_fields` in
/// `tests/codegen.rs`. This is the memory half, which the row itself did
/// not record. Measured under valgrind on THIS fixture's shape, isolated
/// into its own program: pre-fix 12 allocs / 10 frees with 10 bytes
/// definitely lost in 2 blocks. (The four-cell program in the codegen twin
/// ran 22 / 20 pre-fix with those same 10 bytes and 24 / 24 post-fix; its
/// three control cells are individually balanced, which is what pins the
/// loss on this cell rather than on the program.)
///
/// LSan is the gate that sees this — it runs on Linux and NOT on macOS, so
/// a green local Mac run does not clear it (CLAUDE.md § Leak detection).
///
/// TWO THINGS IN THIS FIXTURE ARE LOAD-BEARING FOR THE LEAK, both found by
/// measuring a simplified draft that reported 8 allocs / 8 frees and no
/// leak at all:
///  * the callee must have a SIDE EFFECT (`println("in")`). With
///    `fn gOwn[T](h: Go[T]) -> i64 { return h.z; }` the same program is
///    balanced pre-fix; adding the `println` back takes it to 12 allocs /
///    10 frees with 10 bytes definitely lost in 2 blocks;
///  * the field type must be HEAP-BEARING. With a heap-free
///    `R { id: i64 }` all four surfaces agree pre-fix, so the fixture would
///    assert nothing. (Not the base copy-support check, which reads the
///    DECLARED field types and bails on `Go`'s bare `T` either way — the
///    two spellings part company further in. Measured, not diagnosed.)
///
/// Simplify either one away and this fixture goes quietly vacuous rather
/// than failing, which is why they are spelled out here.
#[test]
fn asan_generic_own_drop_struct_temp_arg_frees_its_field() {
    assert_clean_asan_run_min_allocs(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] }; }
struct Go[T] { r: T, z: i64 }
impl[T] Drop for Go[T] { fn drop(mut ref self) { println(f"dGo{self.z}") } }
fn gOwn[T](h: Go[T]) -> i64 { println("in"); return h.z; }
fn gTemp() { let _ = gOwn(Go[R] { r: mk(3), z: 9 }); }
fn main() {
    gTemp();
    println("done");
}
"#,
        &["in", "dGo9", "dR3", "done"],
        "B-2026-09-05-4 generic own-Drop temp arg",
        4,
    );
}

/// B-2026-09-05-16 — a struct that is NOT copy-supported AND declares its
/// own `impl Drop`, passed BY VALUE, was owned by BOTH frames: the callee
/// freed the caller's buffers through its own param alloca (own by
/// transfer, B-2026-08-05-33) and the caller freed them again through the
/// aggregate it had copied from. Two allocas holding the same pointers, so
/// neither one's cap-zeroing makes the other no-op. Output parity for the
/// same class is
/// `e2e_copy_unsupported_own_drop_by_value_arg_has_one_owner`.
///
/// THE `Map` SPELLING IS HERE, NOT THE GENERIC-INSTANTIATION ONE THE ROW
/// WAS FILED ON, and that choice is the fixture. The filed shape is
/// `nOwn(Nouter { inner: Go[R] { .. }, z: 56 })` — a NON-generic parent
/// holding a generic-instantiation field, where copy-support recurses into
/// `Go` BY NAME and declines on its declared bare `T`. It aborts under
/// `karac run` and under `KARAC_OPT_LEVEL=0 karac build`, and it is CLEAN at
/// the default `-O2`: 10 allocs / 10 frees, 0 valgrind errors. `-O2` is what
/// this harness builds at, so an ASAN test written on that shape would have
/// passed BEFORE the fix and asserted nothing. A `Map` field closes
/// copy-support the same way and fails at every opt level — 18 invalid
/// operations per cell pre-fix, stdout empty because the abort precedes the
/// flush.
///
/// The opt level is not a detail of the harness here; it is why the row read
/// as LLJIT-only. One latent defect in the IR both compiled backends share,
/// masked at `-O2` exactly as B-2026-09-02-20 and B-2026-08-04-19 were —
/// not two backends disagreeing about ownership.
///
/// BOTH CALLER SPELLINGS, because they are different code and only one had a
/// retraction to widen: `moLocal` moves a NAMED BINDING
/// (`move_declined_copy_struct_arg`, whose `UserDrop` retraction was gated
/// on the bare wrapper being ABSENT) and `moTemp` passes a FRESH LITERAL
/// (registered on a channel `struct_param_owned_by_transfer` did not guard).
///
/// `cs*` is the COPY-SUPPORTED control — a plain `String` field takes the
/// other branch entirely, where the callee entry-copies and the two frames
/// own distinct heap. It must stay clean, and a fix that suppressed the
/// caller's drop for it would have traded this double free for a leak.
#[test]
fn asan_copy_unsupported_own_drop_by_value_arg_has_one_owner() {
    let src = r#"struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}" }; }

struct Mo { m: Map[i64, String], r: R, z: i64 }
impl Drop for Mo { fn drop(mut ref self) { println(f"dMo{self.z}") } }
fn moOwn(h: Mo) -> i64 { println("in"); return h.z; }

struct Cs { s: String, r: R, z: i64 }
impl Drop for Cs { fn drop(mut ref self) { println(f"dCs{self.z}") } }
fn csOwn(h: Cs) -> i64 { println("in"); return h.z; }

fn moTemp()  { let mut m: Map[i64, String] = Map.new(); m.insert(1, f"v1"); let _ = moOwn(Mo { m: m, r: mk(3), z: 21 }); }
fn moLocal() { let mut m: Map[i64, String] = Map.new(); m.insert(2, f"v2"); let h = Mo { m: m, r: mk(4), z: 22 }; let _ = moOwn(h); }
fn csTemp()  { let _ = csOwn(Cs { s: f"s7", r: mk(7), z: 25 }); }
fn csLocal() { let h = Cs { s: f"s8", r: mk(8), z: 26 }; let _ = csOwn(h); }

fn main() {
    moTemp();
    moLocal();
    csTemp();
    csLocal();
    println("done");
}
"#;
    assert_clean_asan_run_min_allocs(
        src,
        &[
            "in", "dMo21", "dR3", "in", "dMo22", "dR4", "in", "dCs25", "dR7", "in", "dCs26", "dR8",
            "done",
        ],
        "asan_copy_unsupported_own_drop_by_value_arg_has_one_owner",
        8,
    );
}

/// B-2026-09-07-41 — a WHOLE-VALUE REBIND of a by-value ENUM param the
/// callee owns BY TRANSFER must still own its payload's contents.
///
/// `fn rebind(w: W) -> i64 { let v = w; match v { W.T(x) => .. } }` freed the
/// payload ENVELOPE and stranded its `String`: 2 B at `-O0` (11 allocs / 10
/// frees), clean at `-O2`, `c=5` on every surface and under `--interp`.
///
/// The gate is `scrutinee_is_transfer_owned_enum_param`, which
/// `bind_pattern_values`'s copy-supported arm reads to tell a callee-owned
/// source from the caller-retains one it otherwise assumes
/// (B-2026-09-07-38). It tested `current_fn_param_names` for a BARE param
/// name, so the rebound local `v` answered false, the gate kept its
/// conservative answer, and the consuming arm's binding got no owner for the
/// contents. It now asks `ident_is_whole_param_alias`, which admits the
/// bare param AND the whole-value rebind aliases `fn_whole_param_aliases`
/// names — while still refusing a PROJECTION view, which is the case the
/// gate's `param_view_locals` exclusion was written for.
///
/// Cell 2 is the direct `match w` control that was already clean: it must
/// stay 11/11, so a widening that double-registered would fail here as an
/// invalid free rather than pass quietly.
///
/// Measured parent -> fix by hand (`karac build`, `KARAC_OPT_LEVEL=0`,
/// `KARAC_AUTO_PAR=0`, valgrind), stdout unchanged on both cells:
/// rebind 11 allocs / 10 frees with 2 B definitely lost -> 11 / 11 clean;
/// direct 11 / 11 -> 11 / 11.
///
/// WHAT THIS FIXTURE DOES AND DOES NOT PIN, stated plainly because the
/// distinction is measurable and was measured: it does NOT pin the leak.
/// The defect is `-O0`-only (the row records 0 errors at `-O2`), this
/// harness compiles above `-O0`, and there is no `-O0` variant among its
/// helpers — verified by reverting the fix, at which point BOTH cells still
/// pass here. What it does pin is the hazard THIS change introduces: a
/// widening that registered a second owner would abort under ASAN as a
/// double free at any opt level, and cell 2 is the already-clean control
/// that would catch it. The leak half needs an `-O0` leg; `tests/cli.rs`
/// can set `KARAC_OPT_LEVEL=0` on a spawned `karac` but has no leak
/// checker, so neither harness can assert it today.
/// B-2026-09-09-2 — a CONDITIONALLY-STORED RC-promoted param stays
/// READABLE after the store, and its value is dropped exactly once.
///
/// A param the callee stores on one path and READS on another is a consume
/// followed by a re-use, so the ownership pass RC-promotes it and
/// `compile_function` boxes it. The storing path then handed the value to
/// the container and invalidated the source the way an ordinary move does —
/// but the promotion exists PRECISELY BECAUSE there is a read still to
/// come, so the invalidation destroyed what that read needs.
///
/// Three reads, three different failures off the one cause, which is why
/// all three are asserted here (measured at `KARAC_OPT_LEVEL` 0 and 2, on
/// the JIT, and under auto-par — identical on every leg):
///
/// | read after `xs.push(r)` | interpreter | compiled, pre-fix |
/// |---|---|---|
/// | `r.id`, a scalar | `s100 n1 dR100 end` | `s100 dR100 n1 dR100 end` |
/// | `r.name`, a `String` | `sh100 …` | `s …` — silently EMPTY |
/// | `r.inner.v`, a `shared` | `s100 …` | SIGSEGV, rc=139 |
///
/// The crash is the loudest member, not the whole bug: `store ptr null` on
/// the `shared` handle made the read a null GEP to field 1, faulting on
/// `0x8`, while the zeroed `String` `len` corrupted a read that stayed
/// silent, and the unguarded box body double-dropped one object.
///
/// The fix disarms the box's value-drop with the per-path bit
/// `arm_conditional_store_flag` already stores for this exact shape, rather
/// than by wrecking the value — so the source stays readable and the
/// container is the sole owner. `k = false` is the CONTROL and pins
/// B-2026-09-07-50's fix: nothing is handed over there, the box is still
/// the owner, and its body must still run (`s100 dR100 n0 end`).
#[test]
fn asan_cond_stored_rc_promoted_param_stays_readable_and_drops_once() {
    if !asan_available() {
        eprintln!("[asan_cond_stored_rc_param] ASAN unavailable — skipping");
        return;
    }
    for (read, want_s, label) in [
        ("{r.id}", "s100", "scalar"),
        ("{r.name}", "sh100", "string"),
        ("{r.inner.v}", "s100", "shared"),
    ] {
        for (k, want) in [
            ("true", format!("{want_s}\nn1\ndR100\nend\n")),
            ("false", format!("{want_s}\ndR100\nn0\nend\n")),
        ] {
            let src = format!(
                r#"
shared struct Inner {{ v: i64 }}
struct R {{ id: i64, name: String, inner: Inner }}
impl Drop for R {{ fn drop(mut ref self) {{ println(f"dR{{self.id}}") }} }}
fn mk(i: i64) -> R {{ return R {{ id: i, name: f"h{{i}}", inner: Inner {{ v: i }} }}; }}
struct Box2 {{ mut xs: Vec[R] }}
impl Box2 {{
  fn m(mut ref self, r: R, k: bool) {{ if k {{ self.xs.push(r); }} println(f"s{read}"); }}
}}
fn main() {{
  let mut b = Box2 {{ xs: Vec.new() }};
  b.m(mk(100), {k});
  println(f"n{{b.xs.len()}}");
  println("end");
}}
"#
            );
            let Some((stdout, status)) =
                run_under_asan(&src, &format!("asan_cond_stored_rc_param_{label}_{k}"))
            else {
                eprintln!("[asan_cond_stored_rc_param] setup failed — skipping");
                return;
            };
            assert!(
                status.success(),
                "[asan_cond_stored_rc_param/{label}/k={k}] ASAN reported an error \
                     (exit {:?}). A SIGSEGV means the store nulled a `shared` field the \
                     read still needs; a double free means the box's value-drop fired on \
                     the path that handed the value away.\nstdout:\n{stdout}",
                status.code()
            );
            assert_eq!(
                stdout, want,
                "[asan_cond_stored_rc_param/{label}/k={k}] output diverges from the \
                     interpreter"
            );
        }
    }
}

/// B-2026-09-17-36 / B-2026-09-25-26 — the statement-end walk that now runs a
/// fresh temp's `Drop` bodies after a projection read must not free, read or
/// double-run anything the temp's memory drop already owns. Every body reads a
/// heap `String` longer than the inline capacity, so a body that ran after the
/// memory drop, or twice, would read freed storage; the taken and untaken
/// branch, a loop, a `?` that does not exit and a `return` cover the flag's
/// three routes (statement end, untaken path, frame on exit).
#[test]
fn asan_fresh_temp_read_through_a_projection_runs_its_bodies_once() {
    assert_clean_asan_run_min_allocs(
        r#"struct D { id: i64, name: String }
impl Drop for D { fn drop(mut ref self) { println(f"dD{self.id} {self.name}") } }
fn mkd(n: i64) -> D { return D { id: n, name: f"name-string-longer-than-sso-{n}" }; }
struct W { r: D, s: D, b: i64 }
fn mkw(n: i64) -> W { return W { r: mkd(n), s: mkd(n + 100), b: n }; }
struct R { s: String }
impl Drop for R { fn drop(mut ref self) { println(f"drop {self.s}") } }
fn mkr(n: i64) -> R { return R { s: f"r-string-longer-than-sso-{n}" }; }
fn g(fail: bool) -> Result[i64, String] { if fail { return Err(f"e"); } return Ok(1); }
fn ex() -> Result[i64, String] { let x = mkw(5).b + g(false)?; return Ok(x); }
fn ret() -> i64 { return mkw(6).b; }
fn main() {
    println(f"v{mkw(1).r.id}");
    let o = Some(mkr(2)); println(o.unwrap().s.len());
    let c = true; let x = if c { mkw(3).b } else { 0 }; println(f"x{x}");
    let c2 = false; let y = if c2 { mkw(4).b } else { 0 }; println(f"y{y}");
    match ex() { Ok(v) => println(f"ok{v}"), Err(e) => println(e) }
    println(f"r{ret()}");
    for i in 0..2 { println(mkw(10 + i).s.name); }
    println("end")
}
"#,
        &[
            "v1",
            "dD101 name-string-longer-than-sso-101",
            "dD1 name-string-longer-than-sso-1",
            "26",
            "drop r-string-longer-than-sso-2",
            "dD103 name-string-longer-than-sso-103",
            "dD3 name-string-longer-than-sso-3",
            "x3",
            "y0",
            "dD105 name-string-longer-than-sso-105",
            "dD5 name-string-longer-than-sso-5",
            "ok6",
            "dD106 name-string-longer-than-sso-106",
            "dD6 name-string-longer-than-sso-6",
            "r6",
            "name-string-longer-than-sso-110",
            "dD110 name-string-longer-than-sso-110",
            "dD10 name-string-longer-than-sso-10",
            "name-string-longer-than-sso-111",
            "dD111 name-string-longer-than-sso-111",
            "dD11 name-string-longer-than-sso-11",
            "end",
        ],
        "asan_fresh_temp_read_through_a_projection_runs_its_bodies_once",
        20,
    );
}

/// B-2026-09-25-42 — a function or block-closure TAIL now consumes a fresh
/// temp's projection on both backends, and codegen's masked walk resolves a
/// generic temp's fields from its instantiation. Neither may free, read or
/// double-run what the temp's memory drop owns: every body reads a heap
/// `String` longer than the inline capacity, and the moved-out field (`taild`)
/// is read by its new owner after the tail's walk ran.
#[test]
fn asan_fresh_temp_projected_in_a_function_tail_runs_its_bodies_once() {
    assert_clean_asan_run_min_allocs(
        r#"struct D { id: i64, name: String }
impl Drop for D { fn drop(mut ref self) { println(f"dD{self.id} {self.name}") } }
fn mkd(n: i64) -> D { return D { id: n, name: f"name-string-longer-than-sso-{n}" }; }
struct W { r: D, s: D, b: i64 }
fn mkw(n: i64) -> W { return W { r: mkd(n), s: mkd(n + 100), b: n }; }
struct G[T] { v: T, k: i64 }
fn mkg(d: D) -> G[D] { return G { v: d, k: 5 }; }
fn tail() -> i64 { mkw(1).b }
fn tailg() -> i64 { mkg(mkd(2)).k }
fn taild() -> D { mkw(3).r }
fn lastst() { let a = 1; mkw(4).b; }
fn main() {
    println(f"t{tail()}");
    println(f"g{tailg()}");
    let d = taild(); println(d.name);
    lastst();
    let f = |n: i64| { mkw(n).b }; println(f"f{f(5)}");
    let x = mkg(mkd(6)).k; println(f"x{x}");
    println("end")
}
"#,
        &[
            "dD101 name-string-longer-than-sso-101",
            "dD1 name-string-longer-than-sso-1",
            "t1",
            "dD2 name-string-longer-than-sso-2",
            "g5",
            "dD103 name-string-longer-than-sso-103",
            "name-string-longer-than-sso-3",
            "dD3 name-string-longer-than-sso-3",
            "dD104 name-string-longer-than-sso-104",
            "dD4 name-string-longer-than-sso-4",
            "dD105 name-string-longer-than-sso-105",
            "dD5 name-string-longer-than-sso-5",
            "f5",
            "dD6 name-string-longer-than-sso-6",
            "x5",
            "end",
        ],
        "asan_fresh_temp_projected_in_a_function_tail_runs_its_bodies_once",
        12,
    );
}

/// B-2026-09-26-1 — a heap field moved out THROUGH a projection of a fresh
/// temp has one owner: the consumer now zeroes the leaf in the temp's slot at
/// any depth, so the temp's memory drop no longer frees what the new owner
/// holds (the unfixed tree aborts with a double free on the first line). Every
/// moved leaf and every body reads a heap `String` longer than the inline
/// capacity, and the tail (`f5`, `f7`), tuple, three-hop and assignment
/// consumers each take one.
#[test]
fn asan_field_moved_through_a_fresh_temp_projection_has_one_owner() {
    assert_clean_asan_run_min_allocs(
        r#"struct D { id: i64, name: String }
impl Drop for D { fn drop(mut ref self) { println(f"dD{self.id} {self.name}") } }
fn mkd(n: i64) -> D { return D { id: n, name: f"name-string-longer-than-sso-{n}" }; }
struct P { name: String }
struct W2 { p: P, b: i64 }
fn mkw2(n: i64) -> W2 { return W2 { p: P { name: f"p-string-longer-than-sso-{n}" }, b: n }; }
struct P3 { name: String, d: D }
struct W3 { p: P3, q: D, b: i64 }
fn mkw3(n: i64) -> W3 { return W3 { p: P3 { name: f"p3-string-longer-than-sso-{n}", d: mkd(n) }, q: mkd(n + 100), b: n }; }
struct A4 { w: W3, k: i64 }
fn mk4(n: i64) -> A4 { return A4 { w: mkw3(n), k: n }; }
fn f5() -> String { mkw2(2).p.name }
fn f7() -> D { mkw3(6).p.d }
fn main() {
    let s1 = mkw2(1).p.name; println(s1);
    println(f5());
    let s3 = mkw3(3).p.name; println(s3);
    let s4 = mk4(4).w.p.name; println(s4);
    let t = (mkw3(5).p.name, 1); println(t.0);
    let d = f7(); println(d.name);
    let mut s7 = f"x"; s7 = mkw3(7).p.name; println(s7);
    println("end")
}
"#,
        &[
            "p-string-longer-than-sso-1",
            "p-string-longer-than-sso-2",
            "dD103 name-string-longer-than-sso-103",
            "dD3 name-string-longer-than-sso-3",
            "p3-string-longer-than-sso-3",
            "dD104 name-string-longer-than-sso-104",
            "dD4 name-string-longer-than-sso-4",
            "p3-string-longer-than-sso-4",
            "dD105 name-string-longer-than-sso-105",
            "dD5 name-string-longer-than-sso-5",
            "p3-string-longer-than-sso-5",
            "dD106 name-string-longer-than-sso-106",
            "name-string-longer-than-sso-6",
            "dD6 name-string-longer-than-sso-6",
            "dD107 name-string-longer-than-sso-107",
            "dD7 name-string-longer-than-sso-7",
            "p3-string-longer-than-sso-7",
            "end",
        ],
        "asan_field_moved_through_a_fresh_temp_projection_has_one_owner",
        14,
    );
}

/// B-2026-09-26-3 — a scalar taken off a fresh temp whose type has its own
/// `Drop` runs that body exactly once, before the fields' bodies, as the named
/// spelling does; the unfixed tree printed no `dQ` line at all. The own body
/// and the field body each read a heap `String` longer than the inline
/// capacity, so a body run over freed memory or twice is an ASAN report, and
/// every consuming position the fix covers (`let`, function tail, `return`,
/// tuple element, assignment, constructor argument) takes one.
#[test]
fn asan_scalar_taken_off_a_fresh_temp_runs_its_types_own_drop_once() {
    assert_clean_asan_run_min_allocs(
        r#"struct D { id: i64, name: String }
impl Drop for D { fn drop(mut ref self) { println(f"dD{self.id} {self.name}") } }
fn mkd(n: i64) -> D { return D { id: n, name: f"name-string-longer-than-sso-{n}" }; }
struct Q { d: D, s: String, k: i64 }
impl Drop for Q { fn drop(mut ref self) { println(f"dQ{self.k} {self.s}") } }
fn mkq(n: i64) -> Q { return Q { d: mkd(n), s: f"q-string-longer-than-sso-{n}", k: n }; }
fn tailq() -> i64 { mkq(2).k }
fn retq() -> i64 { return mkq(3).k; }
fn main() {
    let y = mkq(1).k; println(f"y{y}");
    println(f"t{tailq()}");
    println(f"r{retq()}");
    let t = (mkq(4).k, 1); println(f"t{t.0}");
    let mut z = 0; z = mkq(5).k; println(f"z{z}");
    let o = Some(mkq(6).k); println(f"o{o.unwrap()}");
    println("end")
}
"#,
        &[
            "dQ1 q-string-longer-than-sso-1",
            "dD1 name-string-longer-than-sso-1",
            "y1",
            "dQ2 q-string-longer-than-sso-2",
            "dD2 name-string-longer-than-sso-2",
            "t2",
            "dQ3 q-string-longer-than-sso-3",
            "dD3 name-string-longer-than-sso-3",
            "r3",
            "dQ4 q-string-longer-than-sso-4",
            "dD4 name-string-longer-than-sso-4",
            "t4",
            "dQ5 q-string-longer-than-sso-5",
            "dD5 name-string-longer-than-sso-5",
            "z5",
            "dQ6 q-string-longer-than-sso-6",
            "dD6 name-string-longer-than-sso-6",
            "o6",
            "end",
        ],
        "asan_scalar_taken_off_a_fresh_temp_runs_its_types_own_drop_once",
        12,
    );
}

/// B-2026-09-26-6 — a fresh temp read inside a function body's tail
/// expression runs its `Drop` bodies once, when the tail's value exists and
/// before the body's scope exit, in a binary operator, a call argument, a
/// `match` arm, beside a local, through a `Drop`-bearing field, through
/// recursion in an `if` arm and in a method body. Every body reads a heap
/// `String` longer than the inline capacity, so a body run over freed memory
/// or twice is an ASAN report.
#[test]
fn asan_fresh_temp_read_in_a_function_tail_runs_its_bodies_once() {
    assert_clean_asan_run_min_allocs(
        r#"struct D { id: i64, name: String }
impl Drop for D { fn drop(mut ref self) { println(f"dD{self.id} {self.name}") } }
fn mkd(n: i64) -> D { return D { id: n, name: f"name-string-longer-than-sso-{n}" }; }
struct W { r: D, s: D, b: i64 }
fn mkw(n: i64) -> W { return W { r: mkd(n), s: mkd(n + 100), b: n }; }
fn id(x: i64) -> i64 { x }
fn p1() -> i64 { mkw(1).b + 0 }
fn p2() -> i64 { id(mkw(2).b) }
fn p3(k: i64) -> i64 { match k { 0 => mkw(3).b, _ => 1 } }
fn p4() -> i64 { let d = mkd(40); mkw(4).b + d.id }
fn p5() -> i64 { mkw(5).r.id * 2 }
fn p6(n: i64) -> i64 { if n == 5 { 0 } else { mkw(n).b + p6(n - 1) } }
struct M { k: i64 }
impl M { fn m1(ref self) -> i64 { mkw(8).b + self.k } }
fn main() {
    println(f"a{p1()}");
    println(f"b{p2()}");
    println(f"c{p3(0)}");
    println(f"d{p4()}");
    println(f"e{p5()}");
    println(f"f{p6(7)}");
    let m = M { k: 1 }; println(f"g{m.m1()}");
    println("end")
}
"#,
        &[
            "dD101 name-string-longer-than-sso-101",
            "dD1 name-string-longer-than-sso-1",
            "a1",
            "dD102 name-string-longer-than-sso-102",
            "dD2 name-string-longer-than-sso-2",
            "b2",
            "dD103 name-string-longer-than-sso-103",
            "dD3 name-string-longer-than-sso-3",
            "c3",
            "dD104 name-string-longer-than-sso-104",
            "dD4 name-string-longer-than-sso-4",
            "dD40 name-string-longer-than-sso-40",
            "d44",
            "dD105 name-string-longer-than-sso-105",
            "dD5 name-string-longer-than-sso-5",
            "e10",
            "dD106 name-string-longer-than-sso-106",
            "dD6 name-string-longer-than-sso-6",
            "dD107 name-string-longer-than-sso-107",
            "dD7 name-string-longer-than-sso-7",
            "f13",
            "dD108 name-string-longer-than-sso-108",
            "dD8 name-string-longer-than-sso-8",
            "g9",
            "end",
        ],
        "asan_fresh_temp_read_in_a_function_tail_runs_its_bodies_once",
        14,
    );
}

/// B-2026-09-26-14 — a heap field moved off a fresh temp through a branch arm
/// or a block's tail is freed ONCE: by the new owner, never again by the
/// temp's scope-exit drop. Covers a block tail, an `if` arm, a `match` arm two
/// hops deep, a `Drop`-bearing field, tuple / struct-literal / array elements,
/// and a function tail's arm and `return` operand. Every string is longer than
/// the inline capacity, so the double free this fixes is an ASAN report.
#[test]
fn asan_fresh_temp_projection_moved_through_an_arm_is_freed_once() {
    assert_clean_asan_run_min_allocs(
        r#"struct D { id: i64, name: String }
impl Drop for D { fn drop(mut ref self) { println(f"dD{self.id} {self.name}") } }
fn mkd(n: i64) -> D { return D { id: n, name: f"name-string-longer-than-sso-{n}" }; }
struct P { name: String }
struct W3 { p: P, q: D, b: i64 }
fn mkw3(n: i64) -> W3 { return W3 { p: P { name: f"p-string-longer-than-sso-{n}" }, q: mkd(n), b: n } }
struct Q { name: String, k: i64 }
fn mkq(n: i64) -> Q { return Q { name: f"q-string-longer-than-sso-{n}", k: n } }
struct H { s: String, k: i64 }
fn r1(c: bool) -> String { if c { mkq(1).name } else { f"x" } }
fn r2(k: i64) -> String { match k { 0 => mkw3(2).p.name, _ => f"y" } }
fn r3(c: bool) -> String { return if c { mkq(3).name } else { f"x" }; }
fn main() {
    let a = { mkq(10).name }; println(a);
    let b = if true { mkq(11).name } else { f"z" }; println(b);
    let c = match 0 { 0 => mkw3(12).p.name, _ => f"z" }; println(c);
    let d = if true { mkw3(13).q } else { mkd(1) }; println(f"d{d.id}");
    let t = (if true { mkq(15).name } else { f"z" }, 1); println(t.0);
    let h = H { s: if true { mkq(16).name } else { f"z" }, k: 1 }; println(h.s);
    let v = [if true { mkq(17).name } else { f"z" }, f"w"]; println(v[0]);
    println(r1(true));
    println(r2(0));
    println(r3(true));
    println("end")
}
"#,
        &[
            "q-string-longer-than-sso-10",
            "q-string-longer-than-sso-11",
            "dD12 name-string-longer-than-sso-12",
            "p-string-longer-than-sso-12",
            "d13",
            "dD13 name-string-longer-than-sso-13",
            "q-string-longer-than-sso-15",
            "q-string-longer-than-sso-16",
            "q-string-longer-than-sso-17",
            "q-string-longer-than-sso-1",
            "dD2 name-string-longer-than-sso-2",
            "p-string-longer-than-sso-2",
            "q-string-longer-than-sso-3",
            "end",
        ],
        "asan_fresh_temp_projection_moved_through_an_arm_is_freed_once",
        14,
    );
}

/// B-2026-09-25-44 — a fresh temp read in a `match` scrutinee, an `if`
/// condition, each evaluation of a `while` condition, a closure's tail and a
/// generic struct runs its `Drop` bodies once, and a closure arm moving a heap
/// field out of a temp frees it once (a double free on every compiled surface
/// before). Every string is longer than the inline capacity, so a body run
/// over freed memory, or twice, is an ASAN report.
#[test]
fn asan_fresh_temp_read_in_a_condition_loop_or_closure_runs_its_bodies_once() {
    assert_clean_asan_run_min_allocs(
        r#"struct D { id: i64, name: String }
impl Drop for D { fn drop(mut ref self) { println(f"dD{self.id} {self.name}") } }
fn mkd(n: i64) -> D { return D { id: n, name: f"name-string-longer-than-sso-{n}" }; }
struct W { r: D, s: D, b: i64 }
fn mkw(n: i64) -> W { return W { r: mkd(n), s: mkd(n + 100), b: n }; }
struct G[T] { v: T, k: i64 }
fn wrap[T](x: T) -> G[T] { return G { v: x, k: 7 }; }
struct Q { name: String, k: i64 }
fn mkq(n: i64) -> Q { return Q { name: f"q-string-longer-than-sso-{n}", k: n } }
fn main() {
    match mkw(1).b { 1 => println("one"), _ => println("other") }
    if mkw(2).b > 1 { println("big") }
    let mut i = 0;
    while mkw(i + 3).b < 5 { i = i + 1; }
    println(f"i{i}");
    let f = |n: i64| mkw(n).b + 1;
    println(f"f{f(6)}");
    let g = |c: bool| if c { mkq(7).name } else { f"z" };
    println(g(true));
    println(f"g{wrap(mkd(8)).k}");
    println("end")
}
"#,
        &[
            "one",
            "dD101 name-string-longer-than-sso-101",
            "dD1 name-string-longer-than-sso-1",
            "big",
            "dD102 name-string-longer-than-sso-102",
            "dD2 name-string-longer-than-sso-2",
            "dD103 name-string-longer-than-sso-103",
            "dD3 name-string-longer-than-sso-3",
            "dD104 name-string-longer-than-sso-104",
            "dD4 name-string-longer-than-sso-4",
            "dD105 name-string-longer-than-sso-105",
            "dD5 name-string-longer-than-sso-5",
            "i2",
            "dD106 name-string-longer-than-sso-106",
            "dD6 name-string-longer-than-sso-6",
            "f7",
            "q-string-longer-than-sso-7",
            "g7",
            "dD8 name-string-longer-than-sso-8",
            "end",
        ],
        "asan_fresh_temp_read_in_a_condition_loop_or_closure_runs_its_bodies_once",
        14,
    );
}

/// B-2026-09-26-5 — a heap field moved out through a GENERIC fresh temp (two
/// hops, a generic hop inside a generic root, a field after a widened `T`, a
/// one-hop `T` at `String`, an arm and a function tail) is freed once. Each was
/// a double free on every compiled surface; the strings are longer than the
/// inline capacity, so a second free is an ASAN report.
#[test]
fn asan_heap_field_moved_through_a_generic_fresh_temp_is_freed_once() {
    assert_clean_asan_run_min_allocs(
        r#"struct P { name: String }
struct G[T] { v: T, k: i64 }
fn mkg2() -> G[P] { return G { v: P { name: f"g-string-longer-than-sso-1" }, k: 5 }; }
struct H2[T] { a: G[T], k: i64 }
fn mkh() -> H2[P] { return H2 { a: G { v: P { name: f"h-string-longer-than-sso-2" }, k: 1 }, k: 2 }; }
struct G2[T] { v: T, w: P, k: i64 }
fn mk2() -> G2[String] { return G2 { v: f"v-string-longer-than-sso-3", w: P { name: f"w-string-longer-than-sso-4" }, k: 1 }; }
fn g1() -> String { mkg2().v.name }
fn main() {
    let a = mkg2().v.name; println(a);
    let b = mkh().a.v.name; println(b);
    let c = mk2().w.name; println(c);
    let d = mk2().v; println(d);
    let e = if true { mkg2().v.name } else { f"z" }; println(e);
    println(g1());
    println("end")
}
"#,
        &[
            "g-string-longer-than-sso-1",
            "h-string-longer-than-sso-2",
            "w-string-longer-than-sso-4",
            "v-string-longer-than-sso-3",
            "g-string-longer-than-sso-1",
            "g-string-longer-than-sso-1",
            "end",
        ],
        "asan_heap_field_moved_through_a_generic_fresh_temp_is_freed_once",
        6,
    );
}

/// B-2026-09-26-19 — a field projected off a fresh temp and passed as a call
/// argument is freed once: directly to a `ref` / `mut ref` / method param it
/// borrows the temp's field in place (a double free before), and through an
/// arm the arm consumes it, whether the argument is `ref`, by-value, a
/// `println`, an interpolation or a constructor (a double free or a use after
/// free before). Every string is longer than the inline capacity.
#[test]
fn asan_fresh_temp_projection_passed_as_a_call_argument_is_freed_once() {
    assert_clean_asan_run_min_allocs(
        r#"struct D { id: i64, name: String }
impl Drop for D { fn drop(mut ref self) { println(f"dD{self.id} {self.name}") } }
fn mkd(n: i64) -> D { return D { id: n, name: f"name-string-longer-than-sso-{n}" }; }
struct Q { name: String, k: i64 }
fn mkq(n: i64) -> Q { return Q { name: f"q-string-longer-than-sso-{n}", k: n } }
struct W { r: D, s: D, name: String, b: i64 }
fn mkw(n: i64) -> W { return W { r: mkd(n), s: mkd(n + 100), name: f"w-string-longer-than-sso-{n}", b: n }; }
struct G[T] { v: T, k: i64 }
fn mkg(n: i64) -> G[Q] { return G { v: mkq(n), k: n }; }
struct H { k: i64 }
impl H { fn hb(ref self, s: ref String) -> i64 { println(s); 3 } }
fn take(s: String) -> i64 { println(s); 1 }
fn bor(s: ref String) -> i64 { println(s); 2 }
fn grow(s: mut ref String) -> i64 { s.push_str("-grown"); println(s); 4 }
fn main() {
    let a = bor(mkq(1).name); println(f"a{a}");
    let b = bor(mkw(2).r.name); println(f"b{b}");
    let c = bor(mkg(3).v.name); println(f"c{c}");
    let d = grow(mut mkq(4).name); println(f"d{d}");
    let e = H { k: 1 }.hb(mkq(5).name); println(f"e{e}");
    let f = bor(if true { mkq(6).name } else { f"z" }); println(f"f{f}");
    let g = take(match 0 { 0 => mkw(7).name, _ => f"z" }); println(f"g{g}");
    println(if true { mkq(8).name } else { f"z" });
    let s = f"<{if true { mkq(9).name } else { f"z" }}>"; println(s);
    let o = Some(if true { mkq(10).name } else { f"z" }); println(o.unwrap());
    println("end")
}
"#,
        &[
            "q-string-longer-than-sso-1",
            "a2",
            "name-string-longer-than-sso-2",
            "dD102 name-string-longer-than-sso-102",
            "dD2 name-string-longer-than-sso-2",
            "b2",
            "q-string-longer-than-sso-3",
            "c2",
            "q-string-longer-than-sso-4-grown",
            "d4",
            "q-string-longer-than-sso-5",
            "e3",
            "q-string-longer-than-sso-6",
            "f2",
            "dD107 name-string-longer-than-sso-107",
            "dD7 name-string-longer-than-sso-7",
            "w-string-longer-than-sso-7",
            "g1",
            "q-string-longer-than-sso-8",
            "<q-string-longer-than-sso-9>",
            "q-string-longer-than-sso-10",
            "end",
        ],
        "asan_fresh_temp_projection_passed_as_a_call_argument_is_freed_once",
        14,
    );
}

/// B-2026-09-26-23 — a `Drop`-bearing field projected off a fresh temp and
/// passed by value moves into the argument: every `D` built here is dropped
/// exactly once, the siblings at the argument and the moved field after the
/// call. The names are longer than the inline string capacity so each body
/// frees a heap buffer, and every free-fn / method / associated-fn / enum /
/// two-hop / generic-root spelling in the codegen fixture is represented.
#[test]
fn asan_fresh_temp_drop_projection_passed_by_value_is_dropped_once() {
    assert_clean_asan_run_min_allocs(
        r#"struct D { id: i64, name: String }
impl Drop for D { fn drop(mut ref self) { println(f"dD{self.id} {self.name}") } }
fn mkd(n: i64) -> D { return D { id: n, name: f"name-string-longer-than-sso-{n}" }; }
struct W { r: D, s: D, name: String, b: i64 }
fn mkw(n: i64) -> W { return W { r: mkd(n), s: mkd(n + 100), name: f"w-string-longer-than-sso-{n}", b: n }; }
fn eat(d: D) -> i64 { return d.id; }
struct G[T] { v: T, k: i64 }
fn wrap[T](x: T) -> G[T] { return G { v: x, k: 7 }; }
struct H { k: i64 }
impl H { fn take(self, d: D) -> i64 { d.id } fn tk(d: D) -> i64 { d.id } }
enum E { A(D), B }
struct Wx { e: E, s: D }
fn mkwx(n: i64) -> Wx { return Wx { e: E.A(mkd(n)), s: mkd(n + 200) }; }
fn eate(e: E) -> i64 { match e { E.A(d) => d.id, E.B => 0 } }
struct X { w: W, t: D }
fn mkx(n: i64) -> X { return X { w: mkw(n), t: mkd(n + 300) }; }
fn two(a: D, b: D) -> i64 { a.id + b.id }
fn eatw(w: W) -> i64 { w.b }
fn main() {
    println(f"a{eat(mkw(1).r)}");
    println(f"b{eat(wrap(mkd(2)).v)}");
    println(f"c{eat(if true { mkw(3).r } else { mkd(0) })}");
    let h = H { k: 1 };
    println(f"d{h.take(mkw(4).r)}");
    println(f"e{H.tk(mkw(5).r)}");
    println(f"f{eate(mkwx(6).e)}");
    println(f"g{eat(mkx(7).w.r)}");
    println(f"h{two(mkw(8).r, mkw(9).s)}");
    println(f"i{eatw(mkx(10).w)}");
    eat(mkw(11).s);
    println("end")
}
"#,
        &[
            "dD101 name-string-longer-than-sso-101",
            "dD1 name-string-longer-than-sso-1",
            "a1",
            "dD2 name-string-longer-than-sso-2",
            "b2",
            "dD103 name-string-longer-than-sso-103",
            "dD3 name-string-longer-than-sso-3",
            "c3",
            "dD104 name-string-longer-than-sso-104",
            "dD4 name-string-longer-than-sso-4",
            "d4",
            "dD105 name-string-longer-than-sso-105",
            "dD5 name-string-longer-than-sso-5",
            "e5",
            "dD206 name-string-longer-than-sso-206",
            "dD6 name-string-longer-than-sso-6",
            "f6",
            "dD307 name-string-longer-than-sso-307",
            "dD107 name-string-longer-than-sso-107",
            "dD7 name-string-longer-than-sso-7",
            "g7",
            "dD108 name-string-longer-than-sso-108",
            "dD9 name-string-longer-than-sso-9",
            "dD109 name-string-longer-than-sso-109",
            "dD8 name-string-longer-than-sso-8",
            "h117",
            "dD310 name-string-longer-than-sso-310",
            "dD110 name-string-longer-than-sso-110",
            "dD10 name-string-longer-than-sso-10",
            "i10",
            "dD11 name-string-longer-than-sso-11",
            "dD111 name-string-longer-than-sso-111",
            "end",
        ],
        "asan_fresh_temp_drop_projection_passed_by_value_is_dropped_once",
        60,
    );
}

/// B-2026-09-26-34 — a field projected off a fresh temp and handed to a
/// builtin container sink moves into the container: each `D` and each string
/// built here is freed exactly once. Every cell double-freed on the compiled
/// surfaces before the fix. The names are longer than the inline string
/// capacity so each free is a real heap free.
#[test]
fn asan_fresh_temp_projection_moved_into_a_container_sink_is_freed_once() {
    assert_clean_asan_run_min_allocs(
        r#"struct D { id: i64, name: String }
impl Drop for D { fn drop(mut ref self) { println(f"dD{self.id} {self.name}") } }
fn mkd(n: i64) -> D { return D { id: n, name: f"name-string-longer-than-sso-{n}" }; }
struct W { r: D, s: D, name: String, b: i64 }
fn mkw(n: i64) -> W { return W { r: mkd(n), s: mkd(n + 100), name: f"w-string-longer-than-sso-{n}", b: n }; }
struct Q { name: String, k: i64 }
fn mkq(n: i64) -> Q { return Q { name: f"q-string-longer-than-sso-{n}", k: n } }
struct G[T] { v: T, k: i64 }
fn wrap[T](x: T) -> G[T] { return G { v: x, k: 7 }; }
fn main() {
    let mut xs: Vec[D] = Vec.new();
    xs.push(mkw(1).r);
    xs.insert(0, mkw(2).s);
    xs.push(wrap(mkd(3)).v);
    xs.push(if true { mkw(4).r } else { mkd(0) });
    println(f"xs{xs.len()}");
    let mut dq: VecDeque[D] = VecDeque.new();
    dq.push_back(mkw(5).r);
    dq.push_front(mkw(6).r);
    println(f"dq{dq.len()}");
    let mut ss: Vec[String] = Vec.new();
    ss.push(mkq(7).name);
    ss.insert(0, mkw(8).name);
    println(f"ss{ss.len()} {ss[0]} {ss[1]}");
    let mut m: Map[String, D] = Map.new();
    m.insert(mkq(9).name, mkw(10).r);
    println(f"m{m.len()}");
    let mut sm: SortedMap[i64, D] = SortedMap.new();
    sm.insert(1, mkw(11).r);
    println(f"sm{sm.len()}");
    let mut st: Set[String] = Set.new();
    st.insert(mkq(12).name);
    println(f"st{st.len()}");
    println("end")
}
"#,
        &[
            "dD101 name-string-longer-than-sso-101",
            "dD2 name-string-longer-than-sso-2",
            "dD104 name-string-longer-than-sso-104",
            "xs4",
            "dD102 name-string-longer-than-sso-102",
            "dD1 name-string-longer-than-sso-1",
            "dD3 name-string-longer-than-sso-3",
            "dD4 name-string-longer-than-sso-4",
            "dD105 name-string-longer-than-sso-105",
            "dD106 name-string-longer-than-sso-106",
            "dq2",
            "dD6 name-string-longer-than-sso-6",
            "dD5 name-string-longer-than-sso-5",
            "dD108 name-string-longer-than-sso-108",
            "dD8 name-string-longer-than-sso-8",
            "ss2 w-string-longer-than-sso-8 q-string-longer-than-sso-7",
            "dD110 name-string-longer-than-sso-110",
            "m1",
            "dD10 name-string-longer-than-sso-10",
            "dD111 name-string-longer-than-sso-111",
            "sm1",
            "dD11 name-string-longer-than-sso-11",
            "st1",
            "end",
        ],
        "asan_fresh_temp_projection_moved_into_a_container_sink_is_freed_once",
        64,
    );
}

/// B-2026-09-26-33 — a `Drop`-bearing field projected off a fresh temp and
/// handed to a `ref` parameter is read through: the temp keeps the field and
/// runs every field's body once, at the end of the statement. The names are
/// longer than the inline string capacity so each body frees a heap buffer.
#[test]
fn asan_fresh_temp_drop_projection_passed_by_ref_is_dropped_once() {
    assert_clean_asan_run_min_allocs(
        r#"struct D { id: i64, name: String }
impl Drop for D { fn drop(mut ref self) { println(f"dD{self.id} {self.name}") } }
fn mkd(n: i64) -> D { return D { id: n, name: f"name-string-longer-than-sso-{n}" }; }
struct W { r: D, s: D, name: String, b: i64 }
fn mkw(n: i64) -> W { return W { r: mkd(n), s: mkd(n + 100), name: f"w-string-longer-than-sso-{n}", b: n }; }
struct X { w: W, t: D }
fn mkx(n: i64) -> X { return X { w: mkw(n), t: mkd(n + 300) }; }
enum E { A(D), B }
struct Wx { e: E, s: D }
fn mkwx(n: i64) -> Wx { return Wx { e: E.A(mkd(n)), s: mkd(n + 200) }; }
fn peekd(d: ref D) -> i64 { d.id }
fn pe(e: ref E) -> i64 { match e { E.A(d) => d.id, E.B => 0 } }
fn pw(w: ref W) -> i64 { w.b }
struct H { k: i64 }
impl H { fn peek(self, d: ref D) -> i64 { d.id } fn pk(d: ref D) -> i64 { d.id } }
fn main() {
    println(f"a{peekd(mkw(1).r)}");
    let h = H { k: 1 };
    println(f"b{h.peek(mkw(2).s)}");
    println(f"c{H.pk(mkw(3).r)}");
    println(f"d{pe(mkwx(4).e)}");
    println(f"e{peekd(mkx(5).w.r)}");
    println(f"f{pw(mkx(6).w)}");
    let g = peekd(mkw(7).r) + peekd(mkw(8).s);
    println(f"g{g}");
    println("end")
}
"#,
        &[
            "a1",
            "dD101 name-string-longer-than-sso-101",
            "dD1 name-string-longer-than-sso-1",
            "b102",
            "dD102 name-string-longer-than-sso-102",
            "dD2 name-string-longer-than-sso-2",
            "c3",
            "dD103 name-string-longer-than-sso-103",
            "dD3 name-string-longer-than-sso-3",
            "d4",
            "dD204 name-string-longer-than-sso-204",
            "dD4 name-string-longer-than-sso-4",
            "e5",
            "dD305 name-string-longer-than-sso-305",
            "dD105 name-string-longer-than-sso-105",
            "dD5 name-string-longer-than-sso-5",
            "f6",
            "dD306 name-string-longer-than-sso-306",
            "dD106 name-string-longer-than-sso-106",
            "dD6 name-string-longer-than-sso-6",
            "dD108 name-string-longer-than-sso-108",
            "dD8 name-string-longer-than-sso-8",
            "dD107 name-string-longer-than-sso-107",
            "dD7 name-string-longer-than-sso-7",
            "g115",
            "end",
        ],
        "asan_fresh_temp_drop_projection_passed_by_ref_is_dropped_once",
        48,
    );
}

/// B-2026-09-26-35 — a `Drop`-bearing field projected off a NAMED local and
/// moved into a builtin sink (`Vec.push`, an array literal, `Some`, a user
/// variant, `Map.insert`, `VecDeque.push_back` / `push_front`, a push in a
/// taken branch) runs its body once, from the sink; the local's own walk skips
/// it. The names are longer than the inline string capacity so each body
/// frees a heap buffer.
#[test]
fn asan_drop_field_of_a_named_local_moved_into_a_builtin_sink_runs_once() {
    assert_clean_asan_run_min_allocs(
        r#"struct D { id: i64, name: String }
impl Drop for D { fn drop(mut ref self) { println(f"dD{self.id} {self.name}") } }
fn mkd(n: i64) -> D { return D { id: n, name: f"name-string-longer-than-sso-{n}" }; }
struct W { r: D, s: D, name: String, b: i64 }
fn mkw(n: i64) -> W { return W { r: mkd(n), s: mkd(n + 100), name: f"w-string-longer-than-sso-{n}", b: n }; }
enum Bx { P(D), N }
fn a() { let w = mkw(1); let mut xs: Vec[D] = Vec.new(); xs.push(w.r); println(f"a{xs.len()}") }
fn b() { let w = mkw(2); let xs: Vec[D] = [w.s]; println(f"b{xs.len()}") }
fn c() { let w = mkw(3); let o = Some(w.r); println(f"c{o.is_some()}") }
fn d() { let w = mkw(4); let x = Bx.P(w.r); match x { Bx.P(p) => println(f"d{p.id}"), Bx.N => println("dn") } }
fn e() { let w = mkw(5); let mut m: Map[i64, D] = Map.new(); m.insert(1, w.r); println(f"e{m.len()}") }
fn f() { let w = mkw(6); let mut q: VecDeque[D] = VecDeque.new(); q.push_back(w.r); q.push_front(w.s); println(f"f{q.len()}") }
fn g() { let w = mkw(7); let mut xs: Vec[D] = Vec.new(); if w.b > 3 { xs.push(w.r); } println(f"g{xs.len()} {w.name}") }
fn main() {
    a();
    b();
    c();
    d();
    e();
    f();
    g();
    println("end")
}
"#,
        &[
            "dD101 name-string-longer-than-sso-101",
            "a1",
            "dD1 name-string-longer-than-sso-1",
            "dD2 name-string-longer-than-sso-2",
            "b1",
            "dD102 name-string-longer-than-sso-102",
            "dD103 name-string-longer-than-sso-103",
            "ctrue",
            "dD3 name-string-longer-than-sso-3",
            "dD104 name-string-longer-than-sso-104",
            "d4",
            "dD4 name-string-longer-than-sso-4",
            "dD105 name-string-longer-than-sso-105",
            "e1",
            "dD5 name-string-longer-than-sso-5",
            "f2",
            "dD106 name-string-longer-than-sso-106",
            "dD6 name-string-longer-than-sso-6",
            "g1 w-string-longer-than-sso-7",
            "dD7 name-string-longer-than-sso-7",
            "dD107 name-string-longer-than-sso-107",
            "end",
        ],
        "asan_drop_field_of_a_named_local_moved_into_a_builtin_sink_runs_once",
        47,
    );
}

/// B-2026-09-26-40 — a `Drop`-bearing field projected off a fresh temp and handed to a
/// keeping callee (hands it back, stores it, conditionally hands it back), or
/// to a generic `ref T` / read-only by-value param, is freed once and runs
/// each body once. The names are longer than the inline string capacity so
/// each body frees a heap buffer; the keeping cells are the ones where the
/// callee's entry copy escapes and the temp must keep the original's memory.
#[test]
fn asan_fresh_temp_drop_projection_into_a_keeping_or_generic_callee_is_freed_once() {
    assert_clean_asan_run_min_allocs(
        r#"struct D { id: i64, name: String }
impl Drop for D { fn drop(mut ref self) { println(f"dD{self.id} {self.name}") } }
fn mkd(n: i64) -> D { return D { id: n, name: f"name-string-longer-than-sso-{n}" }; }
struct W { r: D, s: D, name: String, b: i64 }
fn mkw(n: i64) -> W { return W { r: mkd(n), s: mkd(n + 100), name: f"w-string-longer-than-sso-{n}", b: n }; }
fn keep(d: D) -> D { d }
fn stash(v: mut ref Vec[D], d: D) { v.push(d); }
fn maybe(d: D, c: bool) -> Option[D] { if c { return Some(d); } None }
struct H { k: i64 }
impl H { fn kp(self, d: D) -> D { d } }
fn gp[T](x: ref T) -> i64 { 2 }
fn gn[T](x: T) -> i64 { 1 }
fn main() {
    let k = keep(mkw(1).r);
    println(f"a{k.id}");
    let h = H { k: 1 };
    let j = h.kp(mkw(2).s);
    println(f"b{j.id}");
    let mut v: Vec[D] = Vec.new();
    stash(mut v, mkw(3).r);
    println(f"c{v.len()}");
    let o = maybe(mkw(4).r, false);
    println(f"d{o.is_some()}");
    println(f"e{gp(mkw(5).r)}");
    println(f"f{gn(mkw(6).s)}");
    println("end")
}
"#,
        &[
            "dD101 name-string-longer-than-sso-101",
            "a1",
            "dD1 name-string-longer-than-sso-1",
            "dD2 name-string-longer-than-sso-2",
            "b102",
            "dD102 name-string-longer-than-sso-102",
            "dD103 name-string-longer-than-sso-103",
            "c1",
            "dD3 name-string-longer-than-sso-3",
            "dD104 name-string-longer-than-sso-104",
            "dD4 name-string-longer-than-sso-4",
            "dfalse",
            "e2",
            "dD105 name-string-longer-than-sso-105",
            "dD5 name-string-longer-than-sso-5",
            "f1",
            "dD106 name-string-longer-than-sso-106",
            "dD6 name-string-longer-than-sso-6",
            "end",
        ],
        "asan_fresh_temp_drop_projection_into_a_keeping_or_generic_callee_is_freed_once",
        35,
    );
}

/// B-2026-09-26-41 — a `Drop`-bearing field projected TWO hops off a fresh temp and
/// handed to a keeping callee (hands it back, stores it, conditionally hands
/// it back) is freed once and runs its body once. The callee takes an entry
/// copy, so the temp keeps and frees the original leaf's heap while the
/// copy runs the body. The names are longer than the inline string capacity
/// so each body frees a heap buffer.
#[test]
fn asan_fresh_temp_two_hop_drop_projection_into_a_keeping_callee_is_freed_once() {
    assert_clean_asan_run_min_allocs(
        r#"struct D { id: i64, name: String }
impl Drop for D { fn drop(mut ref self) { println(f"dD{self.id} {self.name}") } }
fn mkd(n: i64) -> D { return D { id: n, name: f"name-string-longer-than-sso-{n}" }; }
struct W { r: D, s: D, name: String, b: i64 }
fn mkw(n: i64) -> W { return W { r: mkd(n), s: mkd(n + 100), name: f"w-string-longer-than-sso-{n}", b: n }; }
struct X { w: W, t: D }
fn mkx(n: i64) -> X { return X { w: mkw(n), t: mkd(n + 300) }; }
fn keep(d: D) -> D { d }
fn stash(v: mut ref Vec[D], d: D) { v.push(d); }
fn maybe(d: D, c: bool) -> Option[D] { if c { return Some(d); } None }
struct H { k: i64 }
impl H { fn kp(self, d: D) -> D { d } }
fn main() {
    let k = keep(mkx(1).w.r);
    println(f"a{k.id}");
    let h = H { k: 1 };
    let j = h.kp(mkx(2).w.s);
    println(f"b{j.id}");
    let mut v: Vec[D] = Vec.new();
    stash(mut v, mkx(3).w.r);
    println(f"c{v.len()}");
    let o = maybe(mkx(4).w.r, false);
    println(f"d{o.is_some()}");
    println("end")
}
"#,
        &[
            "dD301 name-string-longer-than-sso-301",
            "dD101 name-string-longer-than-sso-101",
            "a1",
            "dD1 name-string-longer-than-sso-1",
            "dD302 name-string-longer-than-sso-302",
            "dD2 name-string-longer-than-sso-2",
            "b102",
            "dD102 name-string-longer-than-sso-102",
            "dD303 name-string-longer-than-sso-303",
            "dD103 name-string-longer-than-sso-103",
            "c1",
            "dD3 name-string-longer-than-sso-3",
            "dD304 name-string-longer-than-sso-304",
            "dD104 name-string-longer-than-sso-104",
            "dD4 name-string-longer-than-sso-4",
            "dfalse",
            "end",
        ],
        "asan_fresh_temp_two_hop_drop_projection_into_a_keeping_callee_is_freed_once",
        35,
    );
}
