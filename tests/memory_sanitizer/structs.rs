//! structs, fields, SoA layouts, tuples, repr/ABI -- fixtures for `tests/memory_sanitizer.rs`.
//!
//! Split out of `tests/memory_sanitizer.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test memory_sanitizer` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test memory_sanitizer structs::
//!
//! New fixtures about structs, fields, SoA layouts, tuples, repr/ABI belong in this file.

use super::*;

/// B-2026-09-04-29 — a by-value param destructure leaf REBOUND (`let c = b;`)
/// runs the payload's body exactly once.
///
/// The caller retains a by-value param's field bodies (it runs them on its temp
/// after the call), which is why the callee's own leaves are param views taking
/// memory-only drops. The let-site rebind gave `c` a bodies walker of its own,
/// so the body ran in the callee and again in the caller — on both compiled
/// backends, for the identifier source (`rebind`, `rebindu`, `rebindcall`,
/// `twice`) and its projection (`prebind`). `orebind` / `porebind` are the
/// `Option` twins (always correct — the boxed path — kept as controls), `direct`
/// the un-rebound leaf.
///
/// ASAN twin of `e2e_param_destructure_leaf_rebind_runs_body_once` (tests/codegen.rs).
#[test]
fn asan_param_destructure_leaf_rebind_is_balanced() {
    assert_clean_asan_run(
        r#"struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}") } }
fn mk(n: i64) -> R { return R { id: n, tag: f"t{n}" }; }
struct HoRes { a: R, b: Result[R, String] }
struct HoOpt { a: R, b: Option[R] }
struct WrapR { inner: HoRes }
struct WrapO { inner: HoOpt }
fn eat(x: R) { println(f"  eat{x.id}") }

fn rebind(h: HoRes)    { let HoRes { a, b } = h; let c = b;
                         match c { Result.Ok(r) => println(f"  ok{r.tag}"), Result.Err(e) => println(f"  er{e}") } }
fn rebindu(h: HoRes)   { let HoRes { a, b } = h; let c = b; println("  m") }
fn rebindcall(h: HoRes){ let HoRes { a, b } = h; let c = b;
                         match c { Result.Ok(r) => eat(r), Result.Err(e) => println(f"  er{e}") } }
fn orebind(h: HoOpt)   { let HoOpt { a, b } = h; let c = b;
                         match c { Option.Some(r) => println(f"  ok{r.tag}"), Option.None => println("  none") } }
fn prebind(w: WrapR)   { let HoRes { a, b } = w.inner; let c = b;
                         match c { Result.Ok(r) => println(f"  ok{r.tag}"), Result.Err(e) => println(f"  er{e}") } }
fn porebind(w: WrapO)  { let HoOpt { a, b } = w.inner; let c = b;
                         match c { Option.Some(r) => println(f"  ok{r.tag}"), Option.None => println("  none") } }
fn direct(h: HoRes)    { let HoRes { a, b } = h;
                         match b { Result.Ok(r) => println(f"  ok{r.tag}"), Result.Err(e) => println(f"  er{e}") } }
fn twice(h: HoRes)     { let HoRes { a, b } = h; let c = b; let d = c;
                         match d { Result.Ok(r) => println(f"  ok{r.tag}"), Result.Err(e) => println(f"  er{e}") } }

fn main() {
  println("rebind");     rebind(HoRes { a: mk(1), b: Result.Ok(mk(101)) })
  println("rebindu");    rebindu(HoRes { a: mk(2), b: Result.Ok(mk(102)) })
  println("rebindcall"); rebindcall(HoRes { a: mk(3), b: Result.Ok(mk(103)) })
  println("orebind");    orebind(HoOpt { a: mk(4), b: Option.Some(mk(104)) })
  println("prebind");    prebind(WrapR { inner: HoRes { a: mk(5), b: Result.Ok(mk(105)) } })
  println("porebind");   porebind(WrapO { inner: HoOpt { a: mk(6), b: Option.Some(mk(106)) } })
  println("direct");     direct(HoRes { a: mk(7), b: Result.Ok(mk(107)) })
  println("twice");      twice(HoRes { a: mk(8), b: Result.Ok(mk(108)) })
  println("done")
}
"#,
        &[
            "rebind",
            "  okt101",
            "dR101/t101",
            "dR1/t1",
            "rebindu",
            "  m",
            "dR102/t102",
            "dR2/t2",
            "rebindcall",
            "  eat103",
            "dR103/t103",
            "dR3/t3",
            "orebind",
            "  okt104",
            "dR104/t104",
            "dR4/t4",
            "prebind",
            "  okt105",
            "dR105/t105",
            "dR5/t5",
            "porebind",
            "  okt106",
            "dR106/t106",
            "dR6/t6",
            "direct",
            "  okt107",
            "dR107/t107",
            "dR7/t7",
            "twice",
            "  okt108",
            "dR108/t108",
            "dR8/t8",
            "done",
        ],
        "asan_param_destructure_leaf_rebind_is_balanced",
    );
}

/// B-2026-09-04-29 — a by-value `self` RECEIVER destructured (`let HoRes { a, b }
/// = self;`, and `= self.inner` through a projection) frees each field exactly
/// once.
///
/// The `SelfValue` source reached none of the callee-owned transfer an
/// `Identifier` param takes, so a consuming arm's binding freed the payload the
/// receiver's drop freed again: glibc's `free(): double free detected in tcache
/// 2` under the JIT, and the same double free on aot (valgrind clean only
/// because of how the optimizer folded it). Balance across three iterations is
/// the pin. The STDOUT here is the compiled column and is NOT what `--interp`
/// prints: a temp receiver's field bodies are lost on the compiled backends
/// (`s_match` loses `a`'s and `b`'s, `s_call` keeps `b`'s through the arm
/// binding) because codegen treats `self` as caller-retained and a temp
/// receiver has no caller walk — a family that is agreed-wrong at its root
/// (`fn m(self) { .. }` on a temp runs no body on ANY surface) and has its own
/// row, B-2026-09-04-30. Pinned so a change there shows up as a changed line,
/// never as a silent double free.
///
/// B-2026-09-06-16 — the `p_match` / `p_unused` cells (a destructure from
/// the PROJECTION `self.inner` on a fresh-temp receiver) now run each
/// field's body once (`dR104/t104 dR4/t4`, `dR105/t105 dR5/t5`): a `let`
/// from a self-rooted projection stopped counting as a bind-out in
/// `fn_binds_self_part_out`, so the temp receiver's bodies are retained
/// caller-side. The `s_*` cells (a bare `self` destructure, the transfer)
/// still print no field body on the compiled backends — B-2026-09-04-30's
/// remaining gap, pinned here as before.
#[test]
fn asan_self_receiver_destructure_frees_each_field_once() {
    assert_clean_asan_run(
        r#"struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}") } }
fn mk(n: i64) -> R { return R { id: n, tag: f"t{n}" }; }
struct HoRes { a: R, b: Result[R, String] }
struct WrapR { inner: HoRes }
fn eat(x: R) { println(f"  eat{x.id}") }
impl HoRes {
    fn s_match(self)  { let HoRes { a, b } = self; println(f"  rd{a.id}")
                        match b { Result.Ok(r) => println(f"  ok{r.tag}"), Result.Err(e) => println(f"  er{e}") } }
    fn s_unused(self) { let HoRes { a, b } = self; println(f"  rd{a.id}") }
    fn s_call(self)   { let HoRes { a, b } = self;
                        match b { Result.Ok(r) => eat(r), Result.Err(e) => println(f"  er{e}") } }
}
impl WrapR {
    fn p_match(self)  { let HoRes { a, b } = self.inner; println(f"  rd{a.id}")
                        match b { Result.Ok(r) => println(f"  ok{r.tag}"), Result.Err(e) => println(f"  er{e}") } }
    fn p_unused(self) { let HoRes { a, b } = self.inner; println(f"  rd{a.id}") }
}
fn main() {
    println("start")
    let mut i = 0;
    while i < 3 {
        let k = i * 10;
        HoRes { a: mk(k + 1), b: Result.Ok(mk(k + 101)) }.s_match()
        HoRes { a: mk(k + 2), b: Result.Ok(mk(k + 102)) }.s_unused()
        HoRes { a: mk(k + 3), b: Result.Ok(mk(k + 103)) }.s_call()
        WrapR { inner: HoRes { a: mk(k + 4), b: Result.Ok(mk(k + 104)) } }.p_match()
        WrapR { inner: HoRes { a: mk(k + 5), b: Result.Ok(mk(k + 105)) } }.p_unused()
        i = i + 1;
    }
}
"#,
        &[
            "start",
            "  rd1",
            "  okt101",
            "  rd2",
            "  eat103",
            "dR103/t103",
            "  rd4",
            "  okt104",
            "dR104/t104",
            "dR4/t4",
            "  rd5",
            "dR105/t105",
            "dR5/t5",
            "  rd11",
            "  okt111",
            "  rd12",
            "  eat113",
            "dR113/t113",
            "  rd14",
            "  okt114",
            "dR114/t114",
            "dR14/t14",
            "  rd15",
            "dR115/t115",
            "dR15/t15",
            "  rd21",
            "  okt121",
            "  rd22",
            "  eat123",
            "dR123/t123",
            "  rd24",
            "  okt124",
            "dR124/t124",
            "dR24/t24",
            "  rd25",
            "dR125/t125",
            "dR25/t25",
        ],
        "asan_self_receiver_destructure_frees_each_field_once",
    );
}

/// B-2026-09-02-23 — A BARE-TUPLE ELEMENT BINDING MUST NOT REGISTER A
/// SECOND OWNER FOR THE TUPLE'S ELEMENT.
///
/// `match t { (r, k) => … }` over a tuple whose element is a heap-owning
/// struct bound `r` as a bit-copy of `t.0` AND registered it on the
/// struct-drop channel, while the tuple's own `__karac_drop_tuple_*` was
/// already freeing that element — both ran on the same buffers.
///
/// IT NEEDS NO FIELD READ, NO `impl Drop`, AND NO PARTICULAR FIELD MIX.
/// The row was first characterised as needing a `Vec` + `String` pair read
/// in a specific order, which was wrong in an instructive way: at the
/// DEFAULT `-O2` the optimizer folded away one of the two frees for almost
/// every shape, so the handful that still aborted looked like the trigger
/// condition. Under `KARAC_OPT_LEVEL=0` every one of them aborted,
/// including `match t { (r, k) => { println("hi") } }`. That is why this
/// case lives in the ASAN suite rather than as an output pin: ASAN catches
/// the double free at any optimization level, and an `-O2` transcript pin
/// would have passed vacuously on all but one shape.
///
/// THE CONTROLS ARE THE POINT, because the fix REMOVES an owner and the
/// failure mode of over-reaching is a leak (which LSan catches here):
/// - `bare` — the same struct as a plain by-value param, never in a tuple.
///   Correct before and after; it must keep its own drop.
/// - `letd` — the `let (r, k) = t` destructure, which was always correct
///   and goes through different machinery (`finish_place_source_tuple_
///   destructure`), so it must stay untouched.
/// - `enu` — the enum-payload twin, correct before and after via the arm
///   channel; it is what showed the tuple family was simply behind.
///
/// A LOCAL tuple scrutinee is included because it was the ONE shape that
/// aborted even at `-O2` — the case a user would actually have hit.
#[test]
fn asan_bare_tuple_element_binding_is_not_a_second_owner() {
    assert_clean_asan_run(
            "struct H { id: i64, xs: Vec[i64], name: String }\n\
             fn mk(id: i64) -> H {\n\
             \x20   let mut v: Vec[i64] = Vec.new();\n\
             \x20   v.push(id);\n\
             \x20   return H { id: id, xs: v, name: f\"n{id}\" }\n\
             }\n\
             enum E { A(H), B }\n\
             fn noread(t: (H, i64)) { match t { (r, k) => { println(\"noread\") } } }\n\
             fn readboth(t: (H, i64)) { match t { (r, k) => { println(f\"  rb{r.xs.len()}:{r.name}\") } } }\n\
             fn loc() { let t = (mk(3), 0); match t { (r, k) => { println(f\"  loc{r.id}\") } } }\n\
             fn bare(h: H) { println(f\"  bare{h.id}\") }\n\
             fn letd(t: (H, i64)) { let (r, k) = t; println(f\"  letd{r.id}\") }\n\
             fn enu(e: E) { match e { E.A(r) => { println(f\"  enu{r.id}\") } E.B => { } } }\n\
             fn main() {\n\
             \x20   noread((mk(1), 0));\n\
             \x20   readboth((mk(2), 0));\n\
             \x20   loc();\n\
             \x20   bare(mk(4));\n\
             \x20   letd((mk(5), 0));\n\
             \x20   enu(E.A(mk(6)));\n\
             \x20   println(\"end\")\n\
             }\n",
            &[
                "noread",
                "  rb1:n2",
                "  loc3",
                "  bare4",
                "  letd5",
                "  enu6",
                "end",
            ],
            "b23-bare-tuple-element-single-owner",
        );
}

/// B-2026-09-06-34 — the rest-covered fields of a struct destructure under
/// ASAN + LSan: a fresh literal's rest field is freed by the discard
/// walker, a fresh call's by the bottom arm's slot (bodies only here), a
/// named local's by its own walk — one owner each, nothing freed twice,
/// nothing leaked.
#[test]
fn asan_struct_destructure_rest_fields_have_one_owner() {
    assert_clean_asan_run(
            "struct R { id: i64, name: String, xs: Vec[i64] }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"  dR{self.id}\") } }\n\
             fn mk(i: i64) -> R { return R { id: i, name: f\"n{i}\", xs: [i] }; }\n\
             struct S3 { a: R, b: R }\n\
             struct S4 { a: R, b: R, c: R, n: i64 }\n\
             fn mks(i: i64) -> S3 { return S3 { a: mk(i), b: mk(i + 1) }; }\n\
             \n\
             fn local_a(i: i64) -> i64 { let s = S3 { a: mk(i), b: mk(i + 1) }; let S3 { a, .. } = s; println(\"  mid\"); return a.id; }\n\
             fn local_ab(i: i64) -> i64 { let s = S4 { a: mk(i), b: mk(i + 1), c: mk(i + 2), n: 4 }; let S4 { a, b, .. } = s; println(\"  mid\"); return a.id + b.id; }\n\
             fn local_all_rest(i: i64) -> i64 { let s = S3 { a: mk(i), b: mk(i + 1) }; let S3 { .. } = s; println(\"  mid\"); return 1; }\n\
             fn local_wild(i: i64) -> i64 { let s = S3 { a: mk(i), b: mk(i + 1) }; let S3 { a, b: _ } = s; println(\"  mid\"); return a.id; }\n\
             fn lit_a(i: i64) -> i64 { let S3 { a, .. } = S3 { a: mk(i), b: mk(i + 1) }; println(\"  mid\"); return a.id; }\n\
             fn lit_wild(i: i64) -> i64 { let S3 { a, b: _ } = S3 { a: mk(i), b: mk(i + 1) }; println(\"  mid\"); return a.id; }\n\
             fn call_a(i: i64) -> i64 { let S3 { a, .. } = mks(i); println(\"  mid\"); return a.id; }\n\
             fn param_a(s: S3) -> i64 { let S3 { a, .. } = s; println(\"  mid\"); return 1; }\n\
             fn view_w(r: R) -> i64 { let s = S3 { a: mk(92), b: r }; let S3 { a, b: _ } = s; println(\"  mid\"); return a.id; }\n\
             fn view_a(r: R) -> i64 { let s = S3 { a: mk(90), b: r }; let S3 { a, .. } = s; println(\"  mid\"); return a.id; }\n\
             \n\
             fn main() {\n\
             \x20   println(\"local_a\"); let v1 = local_a(1); println(f\"  v={v1}\");\n\
             \x20   println(\"local_ab\"); let v2 = local_ab(10); println(f\"  v={v2}\");\n\
             \x20   println(\"local_all_rest\"); let v3 = local_all_rest(20); println(f\"  v={v3}\");\n\
             \x20   println(\"local_wild\"); let v4 = local_wild(30); println(f\"  v={v4}\");\n\
             \x20   println(\"lit_a\"); let v5 = lit_a(40); println(f\"  v={v5}\");\n\
             \x20   println(\"lit_wild\"); let v6 = lit_wild(50); println(f\"  v={v6}\");\n\
             \x20   println(\"call_a\"); let v7 = call_a(60); println(f\"  v={v7}\");\n\
             \x20   println(\"param_a\"); let v8 = param_a(S3 { a: mk(70), b: mk(71) }); println(f\"  v={v8}\");\n\
             \x20   println(\"view_a\"); let v9 = view_a(mk(80)); println(f\"  v={v9}\");\n\
             \x20   println(\"view_w\"); let v10 = view_w(mk(82)); println(f\"  v={v10}\");\n\
             \x20   println(\"end\");\n\
             }\n",
            &[
                "local_a",
                "  dR2",
                "  mid",
                "  dR1",
                "  v=1",
                "local_ab",
                "  dR12",
                "  mid",
                "  dR11",
                "  dR10",
                "  v=21",
                "local_all_rest",
                "  dR21",
                "  dR20",
                "  mid",
                "  v=1",
                "local_wild",
                "  dR31",
                "  mid",
                "  dR30",
                "  v=30",
                "lit_a",
                "  dR41",
                "  mid",
                "  dR40",
                "  v=40",
                "lit_wild",
                "  dR51",
                "  mid",
                "  dR50",
                "  v=50",
                "call_a",
                "  dR61",
                "  mid",
                "  dR60",
                "  v=60",
                "param_a",
                "  mid",
                "  dR71",
                "  dR70",
                "  v=1",
                "view_a",
                "  mid",
                "  dR90",
                "  dR80",
                "  v=90",
                "view_w",
                "  mid",
                "  dR92",
                "  dR82",
                "  v=92",
                "end"
            ],
            "struct_destructure_rest_fields",
        );
}

/// B-2026-09-06-41 — the compiled side of the interpreter-crash row under
/// ASAN + LSan: a scalar or `String`-length read off a destructured leaf
/// of a by-value param leaves the caller-retained walk whole, one owner
/// per field, nothing freed twice, nothing leaked.
#[test]
fn asan_scalar_read_off_a_param_destructure_leaf_keeps_one_owner() {
    assert_clean_asan_run(
            "struct R { id: i64, name: String, xs: Vec[i64] }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"  dR{self.id}\") } }\n\
             fn mk(i: i64) -> R { return R { id: i, name: f\"n{i}\", xs: [i] }; }\n\
             struct S3 { a: R, b: R }\n\
             \n\
             fn full_ret(s: S3) -> i64 { let S3 { a, b } = s; println(\"  mid\"); return b.id; }\n\
             fn full_let(s: S3) -> i64 { let S3 { a, b } = s; println(\"  mid\"); let x = a.id + b.id; return x; }\n\
             fn rest_ret(s: S3) -> i64 { let S3 { a, .. } = s; println(\"  mid\"); return a.id; }\n\
             fn wild_ret(s: S3) -> i64 { let S3 { a, b: _ } = s; println(\"  mid\"); return a.id; }\n\
             fn rest_len(s: S3) -> i64 { let S3 { a, .. } = s; println(\"  mid\"); let n = a.name.len(); return n; }\n\
             fn rest_none(s: S3) -> i64 { let S3 { a, .. } = s; println(\"  mid\"); return 1; }\n\
             fn whole_b(s: S3) -> R { let S3 { a, b } = s; println(\"  mid\"); return b; }\n\
             \n\
             fn main() {\n\
             \x20   println(\"full_ret\"); let v1 = full_ret(S3 { a: mk(1), b: mk(2) }); println(f\"  v={v1}\");\n\
             \x20   println(\"full_let\"); let v2 = full_let(S3 { a: mk(3), b: mk(4) }); println(f\"  v={v2}\");\n\
             \x20   println(\"rest_ret\"); let v3 = rest_ret(S3 { a: mk(5), b: mk(6) }); println(f\"  v={v3}\");\n\
             \x20   println(\"wild_ret\"); let v4 = wild_ret(S3 { a: mk(7), b: mk(8) }); println(f\"  v={v4}\");\n\
             \x20   println(\"rest_len\"); let v5 = rest_len(S3 { a: mk(9), b: mk(10) }); println(f\"  v={v5}\");\n\
             \x20   println(\"rest_none\"); let v6 = rest_none(S3 { a: mk(11), b: mk(12) }); println(f\"  v={v6}\");\n\
             \x20   println(\"whole_b\"); let r7 = whole_b(S3 { a: mk(13), b: mk(14) }); println(f\"  v={r7.id}\");\n\
             \x20   println(\"named\"); let s8 = S3 { a: mk(15), b: mk(16) }; let v8 = full_ret(s8); println(f\"  v={v8}\");\n\
             \x20   println(\"end\");\n\
             }\n",
            &[
                "full_ret",
                "  mid",
                "  dR2",
                "  dR1",
                "  v=2",
                "full_let",
                "  mid",
                "  dR4",
                "  dR3",
                "  v=7",
                "rest_ret",
                "  mid",
                "  dR6",
                "  dR5",
                "  v=5",
                "wild_ret",
                "  mid",
                "  dR8",
                "  dR7",
                "  v=7",
                "rest_len",
                "  mid",
                "  dR10",
                "  dR9",
                "  v=2",
                "rest_none",
                "  mid",
                "  dR12",
                "  dR11",
                "  v=1",
                "whole_b",
                "  mid",
                "  dR13",
                "  v=14",
                "  dR14",
                "named",
                "  mid",
                "  dR16",
                "  dR15",
                "  v=16",
                "end"
            ],
            "scalar_read_off_param_destructure_leaf",
        );
}

/// B-2026-09-06-45 — the MEMORY half of `tests/codegen.rs`'s
/// `e2e_nested_self_rebind_runs_each_body_once` under ASAN + LSan. The
/// callee-side registration is the binding's OWN wrapper rather than a
/// bodies-only walk, because standing the caller down leaves the receiver
/// owning its heap at the call and the prologue's per-field deep copy is
/// then taken: a bodies-only registration ran the right bodies and freed
/// that copy nowhere (22 allocations against 20 frees under valgrind, the
/// copy's `String` and `Vec` definitely lost, on a program calling the
/// method once each way). This pins one owner and one free per object on
/// every spelling.
#[test]
fn asan_nested_self_rebind_keeps_one_owner() {
    assert_clean_asan_run(
            "struct R { id: i64, tag: String, xs: Vec[i64] }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"  dR{self.id}\") } }\n\
             fn mk(i: i64) -> R { return R { id: i, tag: f\"t{i}\", xs: [i] }; }\n\
             struct S { r: R, n: i64 }\n\
             impl Drop for S { fn drop(mut ref self) { println(f\"  dS{self.n}\") } }\n\
             enum E { A(R), B }\n\
             impl Drop for E { fn drop(mut ref self) { println(\"  dE\") } }\n\
             \n\
             impl E {\n\
             \x20   fn cond_match(self, c: bool) -> i64 {\n\
             \x20       if c { let e = self; match e { E.A(r) => { return r.id; } E.B => { return 0; } } }\n\
             \x20       else { match self { E.A(r) => { return r.id + 100; } E.B => { return 100; } } }\n\
             \x20   }\n\
             \x20   fn cond_bare(self, c: bool) -> i64 { if c { let e = self; return 7; } return 0; }\n\
             \x20   fn cond_mut(self, c: bool) -> i64 { if c { let mut e = self; match e { E.A(r) => { return r.id; } E.B => { return 0; } } } else { return 5; } }\n\
             \x20   fn cond_loop(self, n: i64) -> i64 { let mut i = 0; while i < n { let e = self; return 9; } return 0; }\n\
             \x20   fn cond_arm(self, k: i64) -> i64 { match k { 1 => { let e = self; return 1; } _ => { return 2; } } }\n\
             \x20   fn top_let(self) -> i64 { let e = self; match e { E.A(r) => { return r.id; } E.B => { return 0; } } }\n\
             \x20   fn plain(self, c: bool) -> i64 { if c { return 1; } return 2; }\n\
             \x20   fn borrowed(ref self, c: bool) -> i64 { if c { return 1; } return 2; }\n\
             }\n\
             \n\
             impl S {\n\
             \x20   fn cond_struct(self, c: bool) -> i64 { if c { let s2 = self; return s2.n; } return self.n + 100; }\n\
             \x20   fn both_arms(self, c: bool) -> i64 { if c { let s1 = self; return s1.n; } else { let s2 = self; return s2.n + 50; } }\n\
             }\n\
             \n\
             fn main() {\n\
             \x20   println(\"enum_true\"); let a = E.A(mk(1)); let v1 = a.cond_match(true); println(f\"  v={v1}\");\n\
             \x20   println(\"enum_false\"); let b = E.A(mk(2)); let v2 = b.cond_match(false); println(f\"  v={v2}\");\n\
             \x20   println(\"bare_true\"); let c = E.A(mk(3)); let v3 = c.cond_bare(true); println(f\"  v={v3}\");\n\
             \x20   println(\"bare_false\"); let d = E.A(mk(4)); let v4 = d.cond_bare(false); println(f\"  v={v4}\");\n\
             \x20   println(\"mut_true\"); let e = E.A(mk(5)); let v5 = e.cond_mut(true); println(f\"  v={v5}\");\n\
             \x20   println(\"loop_once\"); let f = E.A(mk(6)); let v6 = f.cond_loop(1); println(f\"  v={v6}\");\n\
             \x20   println(\"loop_zero\"); let g = E.A(mk(7)); let v7 = g.cond_loop(0); println(f\"  v={v7}\");\n\
             \x20   println(\"arm_taken\"); let h = E.A(mk(8)); let v8 = h.cond_arm(1); println(f\"  v={v8}\");\n\
             \x20   println(\"arm_other\"); let i = E.A(mk(9)); let v9 = i.cond_arm(3); println(f\"  v={v9}\");\n\
             \x20   println(\"temp_true\"); let v10 = E.A(mk(10)).cond_match(true); println(f\"  v={v10}\");\n\
             \x20   println(\"temp_false\"); let v11 = E.A(mk(11)).cond_match(false); println(f\"  v={v11}\");\n\
             \x20   println(\"struct_true\"); let j = S { r: mk(12), n: 1 }; let v12 = j.cond_struct(true); println(f\"  v={v12}\");\n\
             \x20   println(\"struct_false\"); let k = S { r: mk(13), n: 2 }; let v13 = k.cond_struct(false); println(f\"  v={v13}\");\n\
             \x20   println(\"both_arms\"); let l = S { r: mk(14), n: 3 }; let v14 = l.both_arms(false); println(f\"  v={v14}\");\n\
             \x20   println(\"top_let\"); let m = E.A(mk(15)); let v15 = m.top_let(); println(f\"  v={v15}\");\n\
             \x20   println(\"plain\"); let n = E.A(mk(16)); let v16 = n.plain(true); println(f\"  v={v16}\");\n\
             \x20   println(\"borrowed\"); let o = E.A(mk(17)); let v17 = o.borrowed(true); println(f\"  v={v17}\");\n\
             \x20   println(\"end\");\n\
             }\n",
            &[
                "enum_true",
                "  dE",
                "  dR1",
                "  v=1",
                "enum_false",
                "  dR2",
                "  dE",
                "  v=102",
                "bare_true",
                "  dE",
                "  dR3",
                "  v=7",
                "bare_false",
                "  dE",
                "  v=0",
                "mut_true",
                "  dE",
                "  dR5",
                "  v=5",
                "loop_once",
                "  dE",
                "  dR6",
                "  v=9",
                "loop_zero",
                "  dE",
                "  v=0",
                "arm_taken",
                "  dE",
                "  dR8",
                "  v=1",
                "arm_other",
                "  dE",
                "  v=2",
                "temp_true",
                "  dE",
                "  dR10",
                "  v=10",
                "temp_false",
                "  dR11",
                "  dE",
                "  v=111",
                "struct_true",
                "  dS1",
                "  dR12",
                "  v=1",
                "struct_false",
                "  dS2",
                "  dR13",
                "  v=102",
                "both_arms",
                "  dS3",
                "  dR14",
                "  v=53",
                "top_let",
                "  dE",
                "  dR15",
                "  v=15",
                "plain",
                "  dE",
                "  dR16",
                "  v=1",
                "borrowed",
                "  dE",
                "  dR17",
                "  v=1",
                "end"
            ],
            "nested_self_rebind_one_owner",
        );
}

/// B-2026-09-06-64 — the same program under ASAN + LSan. The row was a
/// compiler crash, so this pins that what now compiles is also memory-clean:
/// one owner and one free per object, envelope included.
#[test]
fn asan_self_referential_struct_compiles() {
    assert_clean_asan_run(
            "struct Node { id: i64, next: Option[Node], tag: String }\n\
             impl Drop for Node { fn drop(mut ref self) { println(f\"  dN{self.id}\") } }\n\
             struct Plain { id: i64, next: Option[Plain], tag: String }\n\
             struct Env { id: i64, inner: Option[Option[i64]] }\n\
             fn mkn(i: i64) -> Node { return Node { id: i, next: Option.None, tag: f\"t{i}\" }; }\n\
             fn mkp(i: i64) -> Plain { return Plain { id: i, next: Option.None, tag: f\"p{i}\" }; }\n\
             fn top(n: Node) -> i64 { let m = n; return m.id; }\n\
             fn read(n: Node) -> i64 { return n.id; }\n\
             fn topp(p: Plain) -> i64 { let m = p; return m.id; }\n\
             fn enve(e: Env) -> i64 { return e.id; }\n\
             impl Node { fn take(self) -> i64 { let m = self; return m.id; } }\n\
             \n\
             fn main() {\n\
             \x20   println(\"bare_local\"); let a = mkp(1); println(f\"  v={a.id}\");\n\
             \x20   println(\"drop_local\"); let b = mkn(2); println(f\"  v={b.id}\");\n\
             \x20   println(\"free_fn_rebind\"); println(f\"  v={top(mkn(3))}\");\n\
             \x20   println(\"free_fn_read\"); println(f\"  v={read(mkn(4))}\");\n\
             \x20   println(\"plain_struct_rebind\"); println(f\"  v={topp(mkp(5))}\");\n\
             \x20   println(\"owned_self_rebind\"); println(f\"  v={mkn(6).take()}\");\n\
             \x20   println(\"boxed_envelope\"); println(f\"  v={enve(Env { id: 7, inner: Option.Some(Option.Some(8)) })}\");\n\
             \x20   println(\"end\");\n\
             }\n",
            &[
                "bare_local",
                "  v=1",
                "drop_local",
                "  v=2",
                "  dN2",
                "free_fn_rebind",
                "  dN3",
                "  v=3",
                "free_fn_read",
                "  dN4",
                "  v=4",
                "plain_struct_rebind",
                "  v=5",
                "owned_self_rebind",
                "  dN6",
                "  v=6",
                "boxed_envelope",
                "  v=7",
                "end"
            ],
            "self_referential_struct_compiles",
        );
}

/// B-2026-09-07-6 — the memory side of the same program: the body was
/// lost with the buffers balanced, so this pins that RESTORING it did not
/// cost a second free anywhere — one owner per element on every spelling,
/// named-tuple sources included.
#[test]
fn asan_whole_tuple_argument_to_a_method() {
    assert_clean_asan_run(
            "struct Q { id: i64, name: String }\n\
             impl Drop for Q { fn drop(mut ref self) { println(f\"  dQ{self.id}\") } }\n\
             fn mkq(i: i64) -> Q { return Q { id: i, name: f\"q{i}\" }; }\n\
             struct Hold { n: i64 }\n\
             impl Hold { fn thrut(ref self, t: (Q, i64)) -> (Q, i64) { return t; } }\n\
             impl Hold { fn mkt(ref self) -> (Q, i64) { return (mkq(4), 1); } }\n\
             impl Hold { fn eatt(ref self, t: (Q, i64)) -> i64 { return t.1; } }\n\
             impl Q { fn passt(t: (Q, i64)) -> (Q, i64) { return t; } }\n\
             fn passt(t: (Q, i64)) -> (Q, i64) { return t; }\n\
             fn main() {\n\
               let h = Hold { n: 0 };\n\
               println(\"method_tuple\"); let a = h.thrut((mkq(1), 7)); println(f\"  v={a.1}\");\n\
               println(\"assoc_tuple\"); let b = Q.passt((mkq(2), 8)); println(f\"  v={b.1}\");\n\
               println(\"free_tuple\"); let c = passt((mkq(3), 9)); println(f\"  v={c.1}\");\n\
               println(\"method_mints\"); let d = h.mkt(); println(f\"  v={d.1}\");\n\
               println(\"method_eats\"); println(f\"  v={h.eatt((mkq(5), 2))}\");\n\
               println(\"named_tuple_method\"); let t = (mkq(6), 3); let e = h.thrut(t); println(f\"  v={e.1}\");\n\
               println(\"named_tuple_assoc\"); let u = (mkq(7), 4); let f = Q.passt(u); println(f\"  v={f.1}\");\n\
               println(\"end\");\n\
             }\n",
            &[
                "method_tuple",
                "  v=7",
                "  dQ1",
                "assoc_tuple",
                "  v=8",
                "  dQ2",
                "free_tuple",
                "  v=9",
                "  dQ3",
                "method_mints",
                "  v=1",
                "  dQ4",
                "method_eats",
                "  dQ5",
                "  v=2",
                "named_tuple_method",
                "  v=3",
                "  dQ6",
                "named_tuple_assoc",
                "  v=4",
                "  dQ7",
                "end"
            ],
            "whole_tuple_argument_to_a_method",
        );
}

/// B-2026-09-06-7 — the MEMORY half of
/// `tests/codegen.rs`'s `e2e_two_step_nested_tuple_field_destructure_runs_one_body`:
/// the same program under ASAN + LSan, so a leaf that stops being a second
/// owner of the struct's walk cannot leave the element's `String` / `Vec`
/// buffers to nobody (valgrind was clean BEFORE the fix — the doubled body
/// read a live value both times — so this pins that the mask did not
/// convert a doubled body into a leaked element).
#[test]
fn asan_two_step_nested_tuple_field_destructure_leaf_is_sole_owner() {
    assert_clean_asan_run(
            "struct R { id: i64, tag: String, xs: Vec[i64] }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"  dR{self.id}\") } }\n\
             fn mk(i: i64) -> R { return R { id: i, tag: f\"t{i}\", xs: [i] } }\n\
             fn consume(x: R) -> i64 { return x.id }\n\
             struct H1 { pe: (R, i64) }\n\
             struct H2 { pe: ((R, i64), i64) }\n\
             struct H3 { pe: (((R, i64), i64), i64) }\n\
             fn mkpair() -> (R, i64) { return (mk(11), 1) }\n\
             \n\
             fn local3() { let h: H2 = H2 { pe: ((mk(9), 1), 2) }; let (inner, y) = h.pe; let (r, x) = inner; let m: R = r; println(f\"  l3 {m.id}\") }\n\
             fn norebind() { let h: H2 = H2 { pe: ((mk(7), 1), 2) }; let (inner, y) = h.pe; let (r, x) = inner; println(f\"  nr {r.id}\") }\n\
             fn viacall() { let h: H2 = H2 { pe: ((mk(4), 1), 2) }; let (inner, y) = h.pe; let (r, x) = inner; let d = consume(r); println(f\"  vc {d}\") }\n\
             fn three() { let h: H3 = H3 { pe: (((mk(3), 1), 2), 3) }; let (mid, z) = h.pe; let (inner, y) = mid; let (r, x) = inner; let m: R = r; println(f\"  t3 {m.id}\") }\n\
             fn leftin() { let h: H2 = H2 { pe: ((mk(2), 1), 2) }; let (inner, y) = h.pe; let (r, x) = inner; println(f\"  li {x}\") }\n\
             fn nestedblock() { let h: H2 = H2 { pe: ((mk(13), 1), 2) }; let (inner, y) = h.pe; { let (r, x) = inner; let m: R = r; println(f\"  nb {m.id}\") } println(\"  after\") }\n\
             fn flat() { let h: H1 = H1 { pe: (mk(8), 1) }; let (r, k) = h.pe; let m: R = r; println(f\"  fl {m.id}\") }\n\
             fn plainlocal() { let t: ((R, i64), i64) = ((mk(6), 1), 2); let (inner, y) = t; let (r, x) = inner; let m: R = r; println(f\"  pl {m.id}\") }\n\
             fn fromcall() { let inner = mkpair(); let (r, x) = inner; let m: R = r; println(f\"  fc {m.id}\") }\n\
             fn wholecopy() { let h: H2 = H2 { pe: ((mk(12), 1), 2) }; let t = h.pe; let (inner, y) = t; let (r, x) = inner; let m: R = r; println(f\"  wc {m.id}\") }\n\
             fn param3(h: H2) { let (inner, y) = h.pe; let (r, x) = inner; let m: R = r; println(f\"  p3 {m.id}\") }\n\
             \n\
             fn main() {\n\
             \x20   println(\"local3\"); local3();\n\
             \x20   println(\"norebind\"); norebind();\n\
             \x20   println(\"viacall\"); viacall();\n\
             \x20   println(\"three\"); three();\n\
             \x20   println(\"leftin\"); leftin();\n\
             \x20   println(\"nestedblock\"); nestedblock();\n\
             \x20   println(\"flat\"); flat();\n\
             \x20   println(\"plainlocal\"); plainlocal();\n\
             \x20   println(\"fromcall\"); fromcall();\n\
             \x20   println(\"wholecopy\"); wholecopy();\n\
             \x20   println(\"param3\"); param3(H2 { pe: ((mk(5), 1), 2) });\n\
             \x20   println(\"end\");\n\
             }\n",
            &[
                "local3",
                "  l3 9",
                "  dR9",
                "norebind",
                "  nr 7",
                "  dR7",
                "viacall",
                "  dR4",
                "  vc 4",
                "three",
                "  t3 3",
                "  dR3",
                "leftin",
                "  dR2",
                "  li 1",
                "nestedblock",
                "  nb 13",
                "  dR13",
                "  after",
                "flat",
                "  fl 8",
                "  dR8",
                "plainlocal",
                "  pl 6",
                "  dR6",
                "fromcall",
                "  fc 11",
                "  dR11",
                "wholecopy",
                "  wc 12",
                "  dR12",
                "param3",
                "  p3 5",
                "  dR5",
                "end",
            ],
            "b7-two-step-nested-tuple-field-destructure",
        );
}

/// B-2026-09-02-25 — the `let`-destructure spelling of the same rule, on a
/// HEAP-CARRYING element.
///
/// The fix records a `let (r, k) = t;` leaf in `param_view_locals`, which
/// moves the later `let m = r;` off the body-registering path and onto the
/// memory-only one. Body counting is pinned by the E2E twins; what only ASAN
/// can answer is whether the buffers still have exactly one owner once that
/// registration changes — a withheld body is also a withheld drop action, and
/// the leaf's memory arrives by `zero_tuple_elem_cap_at` handing the element
/// away from the source's tuple drop.
///
/// `two` carries the shape most likely to go wrong: the rebound element and
/// an untouched sibling in the same tuple, so a cap-zero that reached one
/// index too far would free `q`'s buffers twice. `chain` re-moves the view a
/// second time, and `letelse` covers the enum spelling the same commit
/// converged.
///
/// The transcript is asserted alongside so the case fails if the bodies stop
/// running rather than merely staying memory-clean — the `Drop` body reads
/// both heap fields, so a premature free is a use-after-free here rather than
/// a silent leak.
#[test]
fn asan_let_tuple_destructure_leaf_has_one_owner() {
    assert_clean_asan_run(
            "struct R { id: i64, name: String, xs: Vec[i64] }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}:{self.name}:{self.xs.len()}\") } }\n\
             enum W { A(R), B }\n\
             fn mk(id: i64) -> R {\n\
             \x20   let mut v: Vec[i64] = Vec.new();\n\
             \x20   v.push(id);\n\
             \x20   return R { id: id, name: f\"n{id}\", xs: v }\n\
             }\n\
             fn one(t: (R, i64)) { let (r, k) = t; let m = r; println(f\"  one{m.xs.len()}:{m.name}\") }\n\
             fn chain(t: (R, i64)) { let (r, k) = t; let m = r; let m2 = m; println(f\"  chain:{m2.name}\") }\n\
             fn two(t: (R, R)) { let (r, q) = t; let m = r; println(f\"  two:{m.name}:{q.name}\") }\n\
             fn letelse(w: W) { let W.A(r) = w else { return }; let m = r; println(f\"  le:{m.name}\") }\n\
             fn norebind(t: (R, i64)) { let (r, k) = t; println(f\"  nr{r.xs.len()}:{r.name}\") }\n\
             fn main() {\n\
             \x20   one((mk(1), 0));\n\
             \x20   chain((mk(2), 0));\n\
             \x20   two((mk(3), mk(4)));\n\
             \x20   letelse(W.A(mk(5)));\n\
             \x20   norebind((mk(6), 0));\n\
             \x20   println(\"end\")\n\
             }\n",
            &[
                "one1:n1",
                "dR1:n1:1",
                "  chain:n2",
                "dR2:n2:1",
                "  two:n3:n4",
                "dR3:n3:1",
                "dR4:n4:1",
                "  le:n5",
                "dR5:n5:1",
                "  nr1:n6",
                "dR6:n6:1",
                "end",
            ],
            "b25-let-tuple-destructure-single-owner",
        );
}

/// B-2026-09-02-40 — the heap-carrying PROJECTION-source destructure under
/// ASAN + LSan.
///
/// The fix records a `let (r, k) = h.pe;` leaf in `param_view_locals`, the
/// same registration `asan_let_tuple_destructure_leaf_has_one_owner` covers
/// for a bare-param source, reached one hop further in through a field. Body
/// counting is pinned by the E2E twins; what only ASAN can answer is whether
/// the buffers still have exactly one owner once the leaf stops registering
/// a body — a withheld body is also a withheld drop action, and the leaf's
/// memory arrives by `zero_tuple_elem_cap_at` handing the element away from
/// the OWNING STRUCT's tuple walk rather than from a bare tuple param's.
/// That is the part this case exists for: the source of the cap-zero is a
/// different owner here, so an index computed against the wrong base would
/// free a neighbour twice.
///
/// `ownstr` is the cell that earns its place. Its struct carries a `String`
/// of its OWN beside the tuple, so the param is a caller-retains
/// `owned_struct_params` deep copy — exactly the shape an earlier draft of
/// this fix excluded on the theory that codegen bailed there. It does not
/// bail, and the exclusion split the backends. Pinning the shape under ASAN
/// says the two buffers (the struct's `name` and the element's) are freed
/// once each, which is the thing a body-count assertion cannot see.
///
/// `twohop` runs the cap-zero two fields deep, and `norebind` is the
/// over-reach control: the element is only READ, so the tuple must still
/// free it exactly once.
///
/// The transcript is asserted alongside so the case fails if the bodies stop
/// running rather than merely staying memory-clean — each `Drop` body reads
/// both heap fields, so a premature free is a use-after-free here rather
/// than a silent leak.
#[test]
fn asan_projection_source_destructure_leaf_has_one_owner() {
    assert_clean_asan_run(
            "struct R { id: i64, name: String, xs: Vec[i64] }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}:{self.name}:{self.xs.len()}\") } }\n\
             struct H { pe: (R, i64) }\n\
             struct Hs { pe: (R, i64), name: String }\n\
             struct G { h: H }\n\
             fn mk(id: i64) -> R {\n\
             \x20   let mut v: Vec[i64] = Vec.new();\n\
             \x20   v.push(id);\n\
             \x20   return R { id: id, name: f\"n{id}\", xs: v }\n\
             }\n\
             fn plain(h: H) { let (r, k) = h.pe; let m = r; println(f\"  p{m.xs.len()}:{m.name}\") }\n\
             fn ownstr(hs: Hs) { let (r, k) = hs.pe; let m = r; println(f\"  o:{m.name}:{hs.name}\") }\n\
             fn twohop(g: G) { let (r, k) = g.h.pe; let m = r; println(f\"  t:{m.name}\") }\n\
             fn norebind(h: H) { let (r, k) = h.pe; println(f\"  nr{r.xs.len()}:{r.name}\") }\n\
             fn main() {\n\
             \x20   plain(H { pe: (mk(1), 0) });\n\
             \x20   ownstr(Hs { pe: (mk(2), 0), name: \"s\" });\n\
             \x20   twohop(G { h: H { pe: (mk(3), 0) } });\n\
             \x20   norebind(H { pe: (mk(4), 0) });\n\
             \x20   println(\"end\")\n\
             }\n",
            &[
                "p1:n1",
                "dR1:n1:1",
                "  o:n2:s",
                "dR2:n2:1",
                "  t:n3",
                "dR3:n3:1",
                "  nr1:n4",
                "dR4:n4:1",
                "end",
            ],
            "b40-projection-source-destructure-single-owner",
        );
}

#[test]
/// B-2026-09-07-20 — a struct value that is DISPLACED by a store, or handed
/// to a callee whose store does not happen, has an owner in some frame.
///
/// Both halves were found by running `scripts/asan-o0-leg.sh`, which is how
/// this row exists at all: `asan_stored_argument_is_owned_by_its_new_home_not_the_caller`
/// asserted a clean run at both opt levels from the day it landed and had
/// been leaking 35 B at `-O0` ever since, invisibly, because nothing runs
/// that leg. Reduced with valgrind, the 35 B is two unrelated defects —
/// 19 B on its `k` cell and 16 B on its `b` cell — and this fixture carries
/// both roots plus the neighbours that locate them.
///
///   * `a`/`b`/`c`/`e` — a CONDITIONAL store (`if k { self.xs.push(r); }`)
///     at `k = false`, over a param whose prologue REFUSED to own it (a
///     `shared` field declines copy support, and the transfer bargain with
///     it). B-2026-08-30-28 registers such a param BODIES ONLY, on the
///     premise that "the caller still owns the memory" — false for exactly
///     this class, which is FORWARDED, and for which the caller has stood
///     all the way down. So the `Drop` body ran and the whole of `R`'s heap
///     went unowned: `definitely lost: 19 bytes in 2 blocks`, its `String`
///     and its `shared` field's refcount block. Method, free-fn, assoc-fn
///     and named-local spellings are all carried because all four leaked
///     and one predicate now covers them.
///   * `d` — the STORING path of the same callee, which was clean before
///     and must stay clean: the registration is guarded per path, so a fix
///     that fired the wrapper here would double-free what the container
///     drains.
///   * `f`/`g` — a struct field ASSIGN displacing a value whose type owns a
///     `shared` field. `emit_struct_drop_synthesis` skips `shared` fields BY
///     DESIGN (a live binding releases through a separate scope-exit
///     channel), and a displaced value has no such channel, so the block
///     was released by nobody — 16 B per assignment, confirmed by a
///     two-assignment probe losing exactly 32 B in 2 blocks.
///   * `h` — the INDEX-assign twin (`v[0] = mk(38)`), the same root one
///     site over, and THE CELL THAT GATES ON THE ORDINARY LEG: it leaks at
///     the default `-O2` as well as at `-O0`, because the element buffer
///     stays reachable through the container and LLVM has no dead
///     allocation to elide. Every other cell here is `-O0`-only, so without
///     `h` this fixture would assert nothing on the leg CI actually runs.
///
/// Measured on the parent: 35 B in 3 allocations for the two cells the
/// owning fixture carries, 120 B in 12 blocks for the conditional store
/// over a six-trip loop, and 16 B at BOTH opt levels for `h`. On the fix:
/// 42 allocs / 42 frees, 0 valgrind errors at `-O0` and at `-O2`.
///
/// STDOUT IS BYTE-IDENTICAL ACROSS THE FIX, and identical under `--interp`,
/// so there is no E2E or interpreter twin to pair with this — a leak of a
/// value nothing reads again is invisible to every output assertion, which
/// is the whole reason the row needed a sanitizer to find it.
///
/// `f` EXPECTS `dR1` SINCE B-2026-09-07-52 CLOSED. It did not when this
/// fixture landed: a field assign written `self.one = r` INSIDE a method
/// lost the displaced value's user `Drop` body, while the caller-side
/// spelling `h.one = mk(37)` in `g` ran it (`dR2`). That was never this
/// row's leak — it was cross-backend consistent, bodies-only, and the
/// memory here was balanced either way; it was filed separately and its
/// fix added the line.
fn asan_conditionally_unstored_and_displaced_struct_values_have_an_owner() {
    assert_clean_asan_run_min_allocs(
        r#"
shared struct Inner { v: i64 }
struct R { id: i64, name: String, inner: Inner }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}", inner: Inner { v: i } }; }
struct Box2 { mut xs: Vec[R] }
impl Box2 {
    fn maybe(mut ref self, r: R, k: bool) { if k { self.xs.push(r); } }
    fn amaybe(b: mut ref Box2, r: R, k: bool) { if k { b.xs.push(r); } }
}
struct Box3 { mut one: R }
impl Box3 { fn set(mut ref self, r: R) { self.one = r; } }
fn fcond(b: mut ref Box2, r: R, k: bool) { if k { b.xs.push(r); } }

fn c_cond()  { let mut b = Box2 { xs: Vec.new() }; b.maybe(mk(31), false); println(f"a{b.xs.len()}"); }
fn c_condf() { let mut b = Box2 { xs: Vec.new() }; fcond(mut b, mk(32), false); println(f"b{b.xs.len()}"); }
fn c_conda() { let mut b = Box2 { xs: Vec.new() }; Box2.amaybe(mut b, mk(33), false); println(f"c{b.xs.len()}"); }
fn c_condy() { let mut b = Box2 { xs: Vec.new() }; b.maybe(mk(34), true); println(f"d{b.xs.len()}"); }
fn c_condn() { let mut b = Box2 { xs: Vec.new() }; let r = mk(35); b.maybe(r, false); println(f"e{b.xs.len()}"); }
fn c_setm()  { let mut h = Box3 { one: mk(1) }; h.set(mk(36)); println(f"f{h.one.id}"); }
fn c_setd()  { let mut h = Box3 { one: mk(2) }; h.one = mk(37); println(f"g{h.one.id}"); }
fn c_elem()  { let mut v: Vec[R] = Vec.new(); v.push(mk(3)); v[0] = mk(38); println(f"h{v[0].id}"); }
fn main() { c_cond(); c_condf(); c_conda(); c_condy(); c_condn(); c_setm(); c_setd(); c_elem(); println("end"); }
"#,
        &[
            "dR31", "a0", "dR32", "b0", "dR33", "c0", "d1", "dR34", "dR35", "e0", "dR1", "f36",
            "dR36", "dR2", "g37", "dR37", "dR3", "h38", "dR38", "end",
        ],
        "b0907-20-unstored-and-displaced",
        30,
    );
}

#[test]
fn asan_place_struct_arg_escaping_field_frees_once() {
    assert_clean_asan_run_min_allocs(
        r#"
struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] }; }
struct Cd { r: R, z: i64 }
fn cEsc(h: Cd) -> R { let Cd { r, z } = h; println("in"); return r; }
fn zEsc(h: Cd) -> i64 { println("in5"); return h.z; }
struct Dd { a: R, b: R }
fn dEsc(h: Dd) -> R { let Dd { a, b } = h; println("in2"); return a; }
fn pEsc(h: Cd) -> R { println("in3"); return h.r; }
struct Gd[T] { r: T, z: i64 }
fn gEsc[T](h: Gd[T]) -> T { let Gd { r, z } = h; println("in4"); return r; }
fn round() {
  let g1 = Cd { r: mk(13), z: 9 };  let o1 = cEsc(g1); println(f"got{o1.id}");
  let g2 = Dd { a: mk(31), b: mk(32) }; let o2 = dEsc(g2); println(f"got{o2.id}");
  let g3 = Cd { r: mk(41), z: 9 };  let o3 = pEsc(g3); println(f"got{o3.id}");
  let g4 = Gd { r: mk(81), z: 9 };  let o4 = gEsc(g4); println(f"got{o4.id}");
  let g5 = Cd { r: mk(91), z: 9 };  let _ = cEsc(g5); println("after");
  let g6 = Cd { r: mk(51), z: 5 };  let v6 = zEsc(g6); println(f"gotz{v6}");
}
fn main() {
    let mut i = 0;
    while i < 3 { round(); i = i + 1; }
    println("done");
}
"#,
        &[
            "in", "got13", "dR13", "in2", "dR32", "got31", "dR31", "in3", "got41", "dR41", "in4",
            "got81", "dR81", "in", "dR91", "after", "in5", "dR51", "gotz5", "in", "got13", "dR13",
            "in2", "dR32", "got31", "dR31", "in3", "got41", "dR41", "in4", "got81", "dR81", "in",
            "dR91", "after", "in5", "dR51", "gotz5", "in", "got13", "dR13", "in2", "dR32", "got31",
            "dR31", "in3", "got41", "dR41", "in4", "got81", "dR81", "in", "dR91", "after", "in5",
            "dR51", "gotz5", "done",
        ],
        "b0905-6-place-struct-arg-escaping-field",
        24,
    );
}

/// its `Drop` body; this is the memory gate on that hand-off.
///
/// The fix moves BODIES ONLY and deliberately never takes the memory, which
/// is the whole reason it is safe: the identifier-source hand-off takes the
/// leaf type's whole-value wrapper when that type declares its own `Drop`,
/// correct when the source is being CONSUMED, but a projection source is
/// not — `w` still owns the storage and the leaves are views into it, so
/// taking the memory here would free a buffer `w`'s own drop frees again.
/// A regression that reached for the wrapper would show up here as a double
/// free rather than as a changed line of output.
///
/// B-2026-09-04-21 — that holds for the STRUCT leaf (`pln`) and still does.
/// The `Option`/`Result` leaf is the exception this row made: a view there
/// left a consuming arm freeing the root's buffer (`free(): double free`),
/// so those leaves now TRANSFER the field out of the root (cap-zeroed in
/// place, as the by-value-param destructure does) or own their defensive
/// copy when the root is read again. `res`, `two` and `live` therefore
/// gain the unused `Result` leaf's body at the destructure (`dR101`,
/// `dR104`, `dR105`), matching `opt`, and the heap stays balanced — which
/// is exactly what this fixture is here to say.
///
/// The `String` `tag` on every `R` is what makes a mis-masked field visible:
/// mask one field too many and its payload leaks; mask one too few and the
/// leaf and the root both free it. Five cells — `Result`, `Option[R]`, plain
/// `R`, a two-hop root, and a root read again AFTER the destructure — over
/// three iterations, so a per-iteration imbalance accumulates instead of
/// hiding in a single pass. Measured under valgrind on the compiled binary:
/// 68 allocs, 68 frees, 0 bytes in use at exit, 0 errors.
///
/// `live` emits an ownership WARNING ("value 'w' moved here, used again
/// here") — pre-existing, from the ownership phase rather than this fix, and
/// recorded here so a future reader does not mistake it for fallout. It is a
/// warning: the program compiles and runs, and the cell is kept because a
/// root that outlives the destructure is exactly where a mask that ran too
/// early would be caught.
#[test]
fn asan_projection_source_struct_destructure_leaves_memory_alone() {
    assert_clean_asan_run_min_allocs(
        r#"
struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}") } }
fn mk(n: i64) -> R { return R { id: n, tag: f"t{n}" }; }
struct HoRes { a: R, b: Result[R, String] }
struct HoOpt { a: R, b: Option[R] }
struct HoPln { a: R, b: R }
struct WrapR { inner: HoRes }
struct WrapO { inner: HoOpt }
struct WrapP { inner: HoPln }
struct Outer { h: WrapR }
fn c_res()  { let w = WrapR { inner: HoRes { a: mk(1), b: Result.Ok(mk(101)) } }; let HoRes { a, b } = w.inner; println(f"  rd{a.id}") }
fn c_opt()  { let w = WrapO { inner: HoOpt { a: mk(2), b: Option.Some(mk(102)) } }; let HoOpt { a, b } = w.inner; println(f"  rd{a.id}") }
fn c_pln()  { let w = WrapP { inner: HoPln { a: mk(3), b: mk(103) } };            let HoPln { a, b } = w.inner; println(f"  rd{a.id}") }
fn c_two()  { let g = Outer { h: WrapR { inner: HoRes { a: mk(4), b: Result.Ok(mk(104)) } } }; let HoRes { a, b } = g.h.inner; println(f"  rd{a.id}") }
fn c_live() { let w = WrapR { inner: HoRes { a: mk(5), b: Result.Ok(mk(105)) } }; let HoRes { a, b } = w.inner; println(f"  rd{a.id}"); println(f"  w{w.inner.a.id}") }
fn main() {
    let mut i = 0;
    while i < 3 {
        println("res"); c_res();
        println("opt"); c_opt();
        println("pln"); c_pln();
        println("two"); c_two();
        println("live"); c_live();
        i = i + 1;
    }
}
"#,
        // The helper compares the WHOLE stdout and `main` loops three times,
        // so the per-iteration block appears three times. Three iterations is
        // the point: a per-iteration imbalance accumulates rather than hiding
        // in a single pass.
        &[
            "res",
            "dR101/t101",
            "  rd1",
            "dR1/t1",
            "opt",
            "dR102/t102",
            "  rd2",
            "dR2/t2",
            "pln",
            "dR103/t103",
            "  rd3",
            "dR3/t3",
            "two",
            "dR104/t104",
            "  rd4",
            "dR4/t4",
            "live",
            "dR105/t105",
            "  rd5",
            "dR5/t5",
            "  w5",
            "res",
            "dR101/t101",
            "  rd1",
            "dR1/t1",
            "opt",
            "dR102/t102",
            "  rd2",
            "dR2/t2",
            "pln",
            "dR103/t103",
            "  rd3",
            "dR3/t3",
            "two",
            "dR104/t104",
            "  rd4",
            "dR4/t4",
            "live",
            "dR105/t105",
            "  rd5",
            "dR5/t5",
            "  w5",
            "res",
            "dR101/t101",
            "  rd1",
            "dR1/t1",
            "opt",
            "dR102/t102",
            "  rd2",
            "dR2/t2",
            "pln",
            "dR103/t103",
            "  rd3",
            "dR3/t3",
            "two",
            "dR104/t104",
            "  rd4",
            "dR4/t4",
            "live",
            "dR105/t105",
            "  rd5",
            "dR5/t5",
            "  w5",
        ],
        "projection_source_struct_destructure_leaves_memory_alone",
        40,
    );
}

/// B-2026-08-28-26 — a tuple-typed leaf that takes its inner element's
/// `Drop` body takes the element's heap with it, exactly once.
///
/// The body half was a run-vs-build divergence. The memory half is what the
/// fix had to get right alongside it, and the first cut did not: the leaf
/// registered BODIES only while the cap-zero handed the element's ownership
/// away from the source, so nothing freed it — 3 bytes leaked on a
/// heap-carrying element. The same shape B-2026-08-28-12 and -29 each hit
/// from their own sites, which is why the memory registration is paired
/// with the cap-zero rather than left to the source.
///
/// The two sources answer differently and both are here: a PLACE source's
/// element is cap-zeroed out of the source's walk, while a FRESH one has no
/// named source to disarm.
#[test]
fn asan_tuple_typed_binding_leaf_owns_its_inner_element_once() {
    const H: &str = "struct R { id: i64, name: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id} {self.name}\") } }\n";
    assert_clean_asan_run(
        &format!(
            "{H}fn main() {{ let p = ((R {{ id: 41, name: f\"n{{41}}\" }}, 2), 1);\n\
             \x20            let (inner, n) = p; println(f\"{{n}}\") }}\n"
        ),
        &["drop 41 n41", "1"],
        "place-source",
    );
    // The leaf READ before it dies — a wrong owner shows up here as a
    // use-after-free rather than as a count.
    assert_clean_asan_run(
        &format!(
            "{H}fn main() {{ let p = ((R {{ id: 41, name: f\"n{{41}}\" }}, 2), 1);\n\
             \x20            let (inner, n) = p; println(f\"{{inner.0.name}} {{n}}\") }}\n"
        ),
        &["n41 1", "drop 41 n41"],
        "place-source-leaf-used",
    );
    assert_clean_asan_run(
        &format!(
            "{H}fn main() {{ let (inner, n) = ((R {{ id: 41, name: f\"n{{41}}\" }}, 2), 1);\n\
             \x20            println(f\"{{n}}\") }}\n"
        ),
        &["drop 41 n41", "1"],
        "fresh-literal-source",
    );
    assert_clean_asan_run(
        &format!(
            "{H}fn mk() -> ((R, i64), i64) {{ ((R {{ id: 41, name: f\"n{{41}}\" }}, 2), 1) }}\n\
             \x20            fn main() {{ let (inner, n) = mk(); println(f\"{{n}}\") }}\n"
        ),
        &["drop 41 n41", "1"],
        "call-source",
    );
    // CONTROL — the same source never destructured, where the source's own
    // walk owns the element. Clean before this fix and after it.
    assert_clean_asan_run(
        &format!(
            "{H}fn main() {{ let p = ((R {{ id: 41, name: f\"n{{41}}\" }}, 2), 1);\n\
             \x20            println(f\"{{p.1}}\") }}\n"
        ),
        &["1", "drop 41 n41"],
        "no-destructure-control",
    );
}

/// B-2026-08-28-29 — a fresh-struct-source destructure owns its leaf's heap
/// exactly once, at each of the four (source x leaf) combinations.
///
/// The body half was a run-vs-build divergence. This is the half that
/// decides whether closing it is safe, and the row named the risk in
/// advance: "adding a body to a discarded fresh struct element WITHOUT
/// checking ownership produced an ASAN `double-free`, since a fresh struct
/// temp — unlike a fresh TUPLE temp — already carries its own
/// whole-aggregate drop. Any fix here must answer 'who already frees this'
/// per source kind rather than assuming freshness implies unowned."
///
/// Measured, the answer is not uniform, and BOTH errors were reached before
/// the split below settled:
///
///   * a BOUND leaf over a struct LITERAL owns the field outright — it
///     moves out, and nothing follows it. Leaving the memory registration
///     on the narrow `fresh` LEAKED 3 bytes here;
///   * an UNBOUND field over the same source is claimed by the discard
///     walker instead, and letting the unbound-field arm claim it as well
///     ABORTED at 12 frees for 11 allocs.
///
/// The `-control` rows are the shapes that were already balanced and had to
/// stay so; `no-drop-*` carries no `Drop` at all, which is what proves the
/// memory answers are about ownership rather than about the body.
#[test]
fn asan_fresh_struct_source_destructure_owns_its_leaf_once() {
    const H: &str = "struct R { id: i64, name: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id} {self.name}\") } }\n\
             struct W { r: R, n: i64 }\n\
             fn mk() -> W { W { r: R { id: 41, name: f\"n{41}\" }, n: 1 } }\n";
    assert_clean_asan_run(
        &format!("{H}fn main() {{ let W {{ r, n }} = mk(); println(f\"{{n}}\") }}\n"),
        &["drop 41 n41", "1"],
        "bound-call",
    );
    assert_clean_asan_run(
            &format!(
                "{H}fn main() {{ let W {{ r, n }} = W {{ r: R {{ id: 41, name: f\"n{{41}}\" }}, n: 1 }};\n\
             \x20            println(f\"{{n}}\") }}\n"
            ),
            &["drop 41 n41", "1"],
            "bound-literal",
        );
    // The row's own double-free warning, at the arm where it is real.
    assert_clean_asan_run(
            &format!(
                "{H}fn main() {{ let W {{ r: _, n }} = W {{ r: R {{ id: 41, name: f\"n{{41}}\" }}, n: 1 }};\n\
             \x20            println(f\"{{n}}\") }}\n"
            ),
            &["drop 41 n41", "1"],
            "wildcard-literal",
        );
    assert_clean_asan_run(
        &format!("{H}fn main() {{ let W {{ r: _, n }} = mk(); println(f\"{{n}}\") }}\n"),
        &["drop 41 n41", "1"],
        "wildcard-call-control",
    );
    // A CONSUMED leaf — the buffer outlives the destructure and is read
    // before it dies, so a wrong owner shows up as a use-after-free rather
    // than as a count.
    assert_clean_asan_run(
        &format!("{H}fn main() {{ let W {{ r, n }} = mk(); println(f\"{{r.name}} {{n}}\") }}\n"),
        &["n41 1", "drop 41 n41"],
        "consumed-leaf",
    );
    // NO `Drop` anywhere. The bound-literal row here is a pre-existing
    // 3-byte LEAK that the same widening closes, and it is the measurement
    // that shows the literal source owns nothing — no body is in play to
    // keep the buffer alive, so DCE cannot be masking the answer.
    const N: &str = "struct R { id: i64, name: String }\n\
             struct W { r: R, n: i64 }\n";
    assert_clean_asan_run(
            &format!(
                "{N}fn main() {{ let W {{ r, n }} = W {{ r: R {{ id: 41, name: f\"n{{41}}\" }}, n: 1 }};\n\
             \x20            println(f\"{{r.name}} {{n}}\") }}\n"
            ),
            &["n41 1"],
            "no-drop-bound-literal",
        );
    assert_clean_asan_run(
            &format!(
                "{N}fn mk() -> W {{ W {{ r: R {{ id: 41, name: f\"n{{41}}\" }}, n: 1 }} }}\n\
             \x20            fn main() {{ let W {{ r, n }} = mk(); println(f\"{{r.name}} {{n}}\") }}\n"
            ),
            &["n41 1"],
            "no-drop-bound-call-control",
        );
}

// ── Vec[Tensor] element ownership (B-2026-07-17-1) ───────────
// A `Vec[Tensor]` (the tensor-valued-autograd `Tape` grads/values columns)
// exercises three tensor-element ownership paths that were each leaking or
// double-freeing:
//   (a) a shared-struct `Vec[Tensor]` FIELD drop drains each element block
//       (`emit_tensor_drop_fn`, wired into the shared-struct VecOrString
//       drain) — previously the buffer was freed but the per-element
//       `[rank][dims][data]` blocks leaked;
//   (b) an index-store overwrite (`grads[i] = old + g`) frees the displaced
//       old block before taking over the fresh one — previously it leaked;
//   (c) a MOVED named-tensor store (`grads[i] = seed`) suppresses the source
//       binding's `FreeTensor` — previously both the Vec's element-drop and
//       `seed`'s cleanup freed the block (double-free).

/// B-2026-08-21-22 — `Vec.filled(n, f"...")` DOUBLE-FREED under both
/// compiled backends (`free(): double free detected in tcache 2`, SIGABRT
/// exit 134) while `--interp` printed correctly: a silent run/build
/// divergence that aborts the process.
///
/// The fill value is moved into every slot of the buffer, but the f-string
/// accumulator kept its OWN scope-exit cleanup, so that cleanup and the
/// Vec's owner both freed the same pointer. `Vec.push` and `Vec.from_fn`
/// already called `suppress_fstr_acc_if_moved_out`; `filled` did not — at
/// any of its THREE entry points, which is why both spellings are pinned
/// here. The call has to sit immediately after the value's `compile_expr`:
/// `last_fstr_acc` is a single slot that the count's compile overwrites.
///
/// Controls from the row, both always clean and both still exercised
/// elsewhere: `Vec.filled(3, "x".to_string())` and a `push` loop.
/// `Vec[T] == Vec[T]` borrows both operands and frees neither
/// (B-2026-08-27-10).
///
/// `karac_eq_Vec_<T>` takes POINTERS to the two `{ ptr, len, cap }` control
/// blocks, so the intercept spills the loaded struct values into allocas
/// before the call. Spilling a heap-owning value is exactly the shape that
/// grows a second owner if the callee or the call site frees it, and the
/// heap-element case (`Vec[String]`) is where a stray free shows up as a
/// double-free rather than as a wrong answer.
///
/// The TEMPORARY operands matter more than the bound ones: `ids() == ids()`
/// builds two vecs that nothing else holds, and their only reference is the
/// alloca the comparison made. Both legs are here so a leak on the
/// temporary path can't hide behind a clean bound-operand run.
/// Comparing a three-field struct with heap fields neither leaks nor
/// double-frees (B-2026-08-27-18).
///
/// The fix routes EVERY user struct `==` through `karac_eq_<S>`, which
/// takes pointers, so both operands are spilled to allocas. Spilling a
/// value that owns heap is the shape that grows a second owner, and the
/// widened gate means structs that never took this path before now do —
/// so the risk is a regression in the ordinary case, not just the fixed
/// one.
///
/// The `shared` field is the leg that pins refcounting: `Shared3` is the
/// shape whose `{ptr, i64, i64}` layout made it compare WRONG before the
/// fix, and a comparison must read through the RC pointer without
/// retaining or releasing it. Temporaries are compared alongside bound
/// values because only the temporary has no binding to free it.
#[test]
fn asan_three_field_struct_equality_is_ownership_neutral() {
    assert_clean_asan_run(
        r#"
#[derive(PartialEq, Eq)]
shared struct Node { v: i64 }
#[derive(PartialEq, Eq)]
struct Shared3 { h: Node, a: i64, b: i64 }
#[derive(PartialEq, Eq)]
struct Str3 { s: String, x: i64, y: i64 }

fn mk(n: i64) -> Str3 {
    return Str3 { s: f"item{n}", x: n, y: 0 };
}

fn main() {
    let p = Shared3 { h: Node { v: 1 }, a: 5, b: 6 };
    let q = Shared3 { h: Node { v: 1 }, a: 5, b: 9 };
    println(f"{p == q}");
    println(f"{p.h.v}");

    let u = mk(1);
    let v = mk(1);
    println(f"{u == v}");
    println(f"{mk(2) == mk(2)}");
    println(u.s);
}
"#,
        &["false", "1", "true", "true", "item1"],
        "asan_three_field_struct_equality_is_ownership_neutral",
    );
}

#[test]
fn asan_weak_field_dead_target_reads_none_no_uaf() {
    // B-2026-07-19-8: reading a `weak` field whose target's last STRONG ref
    // has been dropped must yield `None` — never a dangling `Some` over freed
    // payload. `victim` is downgraded into `holder.random` inside a helper,
    // then its strong ref dies at the helper's return; the later read sees
    // strong == 0 and reads `None`. LSan-clean + no invalid reads.
    assert_clean_asan_run(
        r#"
shared struct Node { mut val: i64, mut random: weak Node }
fn make_and_drop(holder: Node) {
    let victim: Node = Node { val: 99i64, random: None };
    holder.random = victim;
}
fn main() {
    let holder: Node = Node { val: 1i64, random: None };
    make_and_drop(holder);
    match holder.random {
        Some(r) => { println(r.val.to_string()); }
        None => { println("none"); }
    }
}
"#,
        &["none"],
        "asan_weak_field_dead_target_reads_none_no_uaf",
    );
}

/// B-2026-08-01-35 — the field store through a FIELD-ROOTED indexed
/// container now lands (it was silently dropped), so the memory
/// pairing needs pinning: the nested store's old-value drop frees the
/// displaced z9 buffer exactly once, and the element's scope-exit
/// drain frees the NEW value exactly once — no leak, no double-free.
#[test]
fn asan_field_rooted_indexed_container_store_freed() {
    assert_clean_asan_run(
        r#"
struct Res { id: i64, name: String }
impl Drop for Res {
    fn drop(mut ref self) { println(f"drop {self.id} {self.name}") }
}
struct Hi { r: Res }
struct Oi { hs: Vec[Hi] }
fn main() {
    println("a");
    let mut o = Oi { hs: Vec.new() };
    o.hs.push(Hi { r: Res { id: 9, name: f"z{9}" } });
    o.hs[0].r = Res { id: 5, name: f"y{5}" };
    println(f"held {o.hs[0].r.id}");
    println("end");
}
"#,
        &["a", "held 5", "drop 5 y5", "end"],
        "field_rooted_indexed_container_store_freed",
    );
}

/// B-2026-08-01-30 (memory leg) — the nested plain-parent field store
/// (`o.h.r = <new>`, depth >= 2) was a BARE overwrite: the displaced
/// old value's heap was never freed. LLVM DCE masked the leak whenever
/// the old value was provably unread, so the probe READS it first
/// (`pre {o.h.r.name}`) to keep the allocation live; pre-fix LSan
/// (Linux CI) flags the stranded z9 buffer. The store now runs the same
/// in-place old-value drop the depth-1 fall-through uses; ASAN guards
/// it against double-freeing the new value the root's drop covers.
#[test]
fn asan_nested_field_store_displaced_old_freed() {
    assert_clean_asan_run(
        r#"
struct R2 { id: i64, name: String }
struct H2 { r: R2 }
struct O2 { h: H2 }
fn main() {
    let mut o = O2 { h: H2 { r: R2 { id: 9, name: f"z{9}" } } };
    println(f"pre {o.h.r.name}");
    o.h.r = R2 { id: 5, name: f"y{5}" };
    println(f"post {o.h.r.name}");
    println("end");
}
"#,
        &["pre z9", "post y5", "end"],
        "nested_field_store_displaced_old_freed",
    );
}

#[test]
fn asan_headerless_abi_full_pipeline_repeat() {
    // Phase C2b under ASAN: program-wide headerless ListNode —
    // 16-byte nodes with NO rc word — through the full kata-#2
    // composition, 200 iterations with chain reuse. The failure
    // modes are vicious and deterministic: any survived count op
    // corrupts val/next (wrong total), any layout disagreement
    // GEPs off by 8 (ASAN OOB on the trailing field), an
    // unbalanced borrow/adoption double-frees or leaks per
    // iteration. Exact total: 200*15 + 9 + 15 = 3024.
    assert_clean_asan_run(
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
    while iter < 200 {
        let r = add_two_numbers(l1, l2);
        total = total + sum_chain(r);
        iter = iter + 1;
    }
    total = total + sum_chain(l1) + sum_chain(l2);
    println(total);
}
"#,
        &["3024"],
        "headerless_abi_full_pipeline_repeat",
    );
}

// ── Parser pre-port: recursive-heap gate (AST tree shape) ──────
//
// The self-hosting parser builds the AST at scale. karac v1 forbids a
// direct nested-enum payload (`E_ENUM_NESTED_ENUM_PAYLOAD`), so the AST
// port wraps recursive edges as `shared enum` (RC pointer = the
// `Box<Expr>` analog), tagged-union operands as plain `struct`, and
// sequence children as `Vec[Expr]`:
//
//     shared enum Expr { Num(i64), Add(BinOp), Neg(Unary), Call(CallExpr) }
//     struct BinOp { left: Expr, right: Expr }
//     struct Unary { operand: Expr }
//     struct CallExpr { callee: Expr, args: Vec[Expr] }
//
// The existing `asan_*_recursive_shared_enum_*` cases above use the
// DIRECT-payload shape (`Add(Expr, Expr)`); these exercise the
// struct-wrapped shape the port actually uses, plus the operations the
// parser hammers: deep build, RC-share fan-out, move-out of Vec
// elements, and a by-value transform that returns a NEW tree (the
// parser-rewrite shape). They are the durable artifact of the
// "recursive-heap family quiet" green-light for the parser port. The
// Linux-CI LSan job is the authoritative leak gate (mac ASAN catches
// double-free / UAF only).

#[test]
fn asan_struct_wrapped_recursive_tree_freed_once() {
    // Build/drop a struct-wrapped recursive `shared enum` tree (the AST
    // wrapping convention). Each `Expr` child is an RC handle inside a
    // plain-`struct` operand wrapper; the whole tree must be freed
    // exactly once (no leak of the per-node boxes, no double-free of the
    // RC-shared children).
    assert_clean_asan_run(
        r#"
shared enum Expr { Num(i64), Add(BinOp), Neg(Unary) }
struct BinOp { left: Expr, right: Expr }
struct Unary { operand: Expr }
fn eval(e: Expr) -> i64 {
    match e {
        Num(n) => n,
        Add(b) => eval(b.left) + eval(b.right),
        Neg(u) => 0 - eval(u.operand),
    }
}
fn main() {
    let inner = Add(BinOp { left: Num(2), right: Num(3) });
    let sum = Add(BinOp { left: Num(1), right: inner });
    let t = Neg(Unary { operand: sum });
    println(eval(t));
}
"#,
        &["-6"],
        "struct_wrapped_recursive_tree_freed_once",
    );
}

#[test]
fn asan_struct_wrapped_recursive_cycle_accepted_and_freed() {
    // B-2026-06-14-28 regression (memory side): the struct-wrapped
    // recursive shape (`shared enum Expr` whose recursive edge passes
    // through a plain `struct BinOp`) is a *breakable* cycle — the
    // ownership checker must accept it (see the ownership.rs unit tests),
    // and the resulting RC tree must be freed exactly once. Looped to
    // surface a per-iteration leak to the Linux-CI LSan gate.
    assert_clean_asan_run(
        r#"
shared enum Expr { Num(i64), Bin(BinOp) }
struct BinOp { left: Expr, right: Expr }
fn eval(e: Expr) -> i64 {
    match e {
        Num(n) => n,
        Bin(b) => eval(b.left) + eval(b.right),
    }
}
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 40 {
        let t: Expr = Bin(BinOp { left: Num(i), right: Bin(BinOp { left: Num(i), right: Num(2) }) });
        total = total + eval(t);
        i = i + 1;
    }
    println(total);
}
"#,
        &["1640"],
        "struct_wrapped_recursive_cycle_accepted_and_freed",
    );
}

#[test]
fn asan_derive_eq_struct_temp_tracking_controls() {
    // B-2026-08-11-33 over-fire matrix. The fix gives a fresh temp struct
    // operand an OWNER, so the failure mode it risks is the opposite of
    // the bug: freeing something that is owned elsewhere. Every shape here
    // must stay clean — a double free or use-after-free shows up as an
    // ASAN abort, not a leak, so these are the rows that actually license
    // the change.
    for (shape, src, expect) in [
        // A struct LOCAL on the fresh side: an identifier is a place
        // expression, already owned by its binding, and must NOT be
        // tracked — tracking it would drop the same fields twice.
        (
            "local_vs_ref_param",
            r#"
#[derive(Eq)]
struct P { name: String }
fn f(other: ref P) -> bool { let p = P { name: "abcd" }; p == other }
fn main() { let q = P { name: "abcd" }; println(f"{f(q)}"); }
"#,
            "true",
        ),
        // A FIELD ACCESS operand — also a place, owned by its parent.
        (
            "field_access_vs_ref_param",
            r#"
#[derive(Eq)]
struct P { name: String }
struct Holder { inner: P }
fn f(h: ref Holder, other: ref P) -> bool { h.inner == other }
fn main() {
    let h = Holder { inner: P { name: "abcd" } };
    let q = P { name: "abcd" };
    println(f"{f(h, q)}");
}
"#,
            "true",
        ),
        // BOTH operands fresh temps: both get tracked, and each must own
        // exactly its own fields.
        (
            "temp_vs_temp",
            r#"
#[derive(Eq)]
struct P { name: String }
fn mk(s: ref String) -> P { P { name: s.substring(0, 4) } }
fn f(a: ref String, b: ref String) -> bool { mk(a) == mk(b) }
fn main() { let x = "abcdefgh"; let y = "abcdzzzz"; println(f"{f(x, y)}"); }
"#,
            "true",
        ),
        // The `!=` form, and the borrowed operand READ AFTER the
        // comparison — if the fix dropped the wrong side, or dropped too
        // early, this reads freed memory.
        (
            "ne_then_read_borrowed_operand",
            r#"
#[derive(Eq)]
struct P { name: String }
fn mk(s: ref String) -> P { P { name: s.substring(0, 4) } }
fn f(hay: ref String, other: ref P) -> i64 {
    let differs = mk(hay) != other;
    let n = other.name.len();
    if differs { n } else { n + 100 }
}
fn main() {
    let h = "zzzzefgh";
    let p = P { name: "abcd" };
    println(f"{f(h, p)}");
}
"#,
            "4",
        ),
        // A `Vec[String]` field — the registered drop must drain the
        // ELEMENTS, not just the vector's buffer, and must not
        // double-drop them.
        //
        // The comparison's RESULT is deliberately discarded and not
        // asserted: `#[derive(Eq)]` over a `Vec[String]` field is itself
        // wrong in codegen (interp says the two are equal, both compiled
        // backends say they are not — B-2026-08-12-5, found while
        // building this control and unrelated to the leak). Asserting it
        // here would either encode a wrong answer as expected or make this
        // memory test fail for a reason that has nothing to do with
        // memory. The temp is still built, compared, tracked and dropped,
        // which is all this row is here to exercise.
        (
            "vec_field_temp",
            r#"
#[derive(Eq)]
struct B { items: Vec[String] }
fn mk(s: ref String) -> B {
    let mut v: Vec[String] = Vec.new();
    v.push(s.substring(0, 4));
    v.push(s.substring(4, 8));
    B { items: v }
}
fn f(a: ref String, other: ref B) -> i64 {
    let _ = mk(a) == other;
    other.items.len()
}
fn main() {
    let x = "abcdefgh";
    let mut w: Vec[String] = Vec.new();
    w.push("abcd");
    w.push("efgh");
    let o = B { items: w };
    println(f"{f(x, o)}");
}
"#,
            "2",
        ),
        // A comparison in a LOOP — the per-evaluation drop must fire per
        // evaluation, without accumulating or re-freeing.
        (
            "temp_eq_in_loop",
            r#"
#[derive(Eq)]
struct P { name: String }
fn mk(s: ref String, i: i64) -> P { P { name: s.substring(i, i + 4) } }
fn f(hay: ref String, other: ref P) -> i64 {
    let mut hits = 0;
    let mut i = 0;
    while i + 4 <= hay.len() {
        if mk(hay, i) == other { hits = hits + 1; }
        i = i + 1;
    }
    hits
}
fn main() {
    let h = "abcdabcdabcd";
    let p = P { name: "abcd" };
    println(f"{f(h, p)}");
}
"#,
            "3",
        ),
        // Scalar-only struct: nothing to free, so the gate must skip it
        // entirely rather than emit a spurious drop.
        (
            "scalar_struct_temp",
            r#"
#[derive(Eq)]
struct Pt { x: i64, y: i64 }
fn mk(a: i64) -> Pt { Pt { x: a, y: a + 1 } }
fn f(other: ref Pt) -> bool { mk(1) == other }
fn main() { let q = Pt { x: 1, y: 2 }; println(f"{f(q)}"); }
"#,
            "true",
        ),
    ] {
        assert_clean_asan_run(src, &[expect], &format!("derive_eq_ctl_{shape}"));
    }
}

#[test]
fn asan_forloop_struct_element_clean_shapes_stay_clean() {
    // Regression guard for the shapes that were already CLEAN (must stay
    // clean, NOT over-copied into a leak): a for-loop DESTRUCTURE and a
    // pass-by-value call (the callee entry-copies). Neither is a move into a
    // new local owner, so the defensive copy must NOT fire.
    assert_clean_asan_run(
        r#"
struct A { s: String }
fn take(a: A) -> i64 { a.s.len() }
fn build() -> Vec[A] {
    let mut v: Vec[A] = Vec.new();
    let mut i = 0;
    while i < 4 { v.push(A { s: "clean_shape_regression_guard_payload_mu_zz".to_string() }); i = i + 1; }
    v
}
fn main() {
    let items = build();
    let mut n: i64 = 0;
    for a in items {
        let A { s } = a;
        n = n + s.len();
    }
    let more = build();
    for a in more {
        n = n + take(a);
    }
    println(n);
}
"#,
        &["336"], // 4 * 42 (destructure) + 4 * 42 (pass-by-value)
        "forloop_struct_element_clean_shapes_stay_clean",
    );
}

/// B-2026-08-07-2 shape 4 — a box inside a box. The nested-box action freed
/// the OUTERMOST envelope and every one below it leaked.
///
/// `Result[Option[Option[Option[i64]]], E]` boxes twice: the `Ok` payload is
/// inline in Result's 5-word area, its own 4-word payload is boxed, and what
/// THAT box holds is another `Option` whose 4-word payload is boxed again.
/// One box per level below the first leaked — 32 B for the triple nest, and
/// the quadruple arm below measured 320 B definitely lost PLUS 320 B
/// indirectly lost pre-fix, which is the signature of a chain rather than a
/// single miss.
///
/// WHY WALKING THE CHAIN IS SAFE where widening the free to the payload's
/// drop was measured wrong (B-2026-08-06-32 aborted with a glibc double free
/// at both opt levels doing that). Every level here is an ENVELOPE minted by
/// `coerce_to_payload_words`. An envelope is never named by the source
/// program, so no match arm can bind one out and no other action can own
/// one. The INTERIOR is the opposite — a `String` an arm binds out already
/// has an owner — and the walk still does not touch it. The `tristr` arm is
/// that boundary as an assertion: it binds the `String` out from the bottom
/// of a two-envelope chain, so if the walk ever reached past the envelopes
/// it would double-free there.
///
/// ORDER IS THE OTHER HALF. The pointer to the next envelope lives INSIDE
/// the current one, so each level loads its successor BEFORE freeing itself;
/// freeing on the way down and reading afterwards is a use-after-free. The
/// emitter descends first and frees on the join.
///
/// The `duo` arm is the empty-chain control — `Result[Option[Option[i64]],
/// E]` has exactly one envelope and must stay at exactly one free. The
/// `Ok(Some(None))`, `Ok(None)` and `Err` arms are the guard controls: each
/// leaves some level's words holding a value rather than a pointer, and a
/// missing tag or null check frees a scalar.
///
/// COVERAGE: pre-fix this leaks 5,120 B in 160 blocks plus 1,280 B in 40
/// blocks indirectly at `KARAC_OPT_LEVEL=0`, and is CLEAN at the default
/// `-O2` where the boxes fold away — so the memory half rides entirely on
/// the `-O0` leg (`scripts/asan-o0-leg.sh`, B-2026-08-04-17). The `-O2` run
/// still asserts the accumulated value and the allocation floor.
///
/// The expected value is COMPUTED: payloads are seeded from the opaque
/// `env.args().len()`; five arms subtract the seed back out to leave `i`,
/// the String arm contributes 1 and the two payload-absent arms -1 each —
/// `5i - 1` per iteration, so `5 * (0+…+39) - 40 = 3860`.
#[test]
fn asan_nested_box_chain_frees_every_envelope() {
    assert_clean_asan_run_min_allocs(
        r#"
fn tri(r: Result[Option[Option[Option[i64]]], i64]) -> i64 {
    match r {
        Result.Ok(Option.Some(Option.Some(Option.Some(x)))) => x,
        Result.Ok(_) => -1,
        Result.Err(e) => e,
    }
}
fn quad(r: Result[Option[Option[Option[Option[i64]]]], i64]) -> i64 {
    match r {
        Result.Ok(Option.Some(Option.Some(Option.Some(Option.Some(x))))) => x,
        Result.Ok(_) => -1,
        Result.Err(e) => e,
    }
}
fn duo(r: Result[Option[Option[i64]], i64]) -> i64 {
    match r {
        Result.Ok(Option.Some(Option.Some(x))) => x,
        Result.Ok(_) => -1,
        Result.Err(e) => e,
    }
}
fn tristr(r: Result[Option[Option[Option[String]]], i64]) -> i64 {
    match r {
        Result.Ok(Option.Some(Option.Some(Option.Some(s)))) => if s.len() > 0 { 1 } else { 0 },
        Result.Ok(_) => -1,
        Result.Err(e) => e,
    }
}
fn main() {
    let n = env.args().len() as i64;
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 40 {
        let a: Result[Option[Option[Option[i64]]], i64] = Result.Ok(Option.Some(Option.Some(Option.Some(n + i))));
        acc = acc + tri(a) - n;

        let b: Result[Option[Option[Option[Option[i64]]]], i64] = Result.Ok(Option.Some(Option.Some(Option.Some(Option.Some(n + i)))));
        acc = acc + quad(b) - n;

        acc = acc + tri(Result.Ok(Option.Some(Option.Some(Option.Some(n + i))))) - n;

        let c: Result[Option[Option[i64]], i64] = Result.Ok(Option.Some(Option.Some(n + i)));
        acc = acc + duo(c) - n;

        let d: Result[Option[Option[Option[i64]]], i64] = Result.Ok(Option.Some(Option.None));
        acc = acc + tri(d);

        let e1: Result[Option[Option[Option[i64]]], i64] = Result.Ok(Option.None);
        acc = acc + tri(e1);

        let g: Result[Option[Option[Option[i64]]], i64] = Result.Err(n + i);
        acc = acc + tri(g) - n;

        let s = f"p{n + i}";
        let h: Result[Option[Option[Option[String]]], i64] = Result.Ok(Option.Some(Option.Some(Option.Some(s))));
        acc = acc + tristr(h);

        i = i + 1;
    }
    println(acc);
}
"#,
        &["3860"],
        "nested_box_chain_frees_every_envelope",
        30,
    );
}

// B-2026-07-30-11 (displaced-value leg) — reassigning a struct binding
// must FREE the displaced old value's field heap exactly once. Before
// the fix nothing on the reassign path touched the old value: its
// String field was definitely-lost whenever LLVM couldn't dead-code the
// allocation (13 bytes/iteration, valgrind-verified on the probe). This
// case guards the DOUBLE-FREE direction: a displaced-value cleanup
// emitted on the wrong side of the store (or a moved source left armed)
// frees the survivor's buffer twice and aborts loudly under ASAN. The
// LEAK direction is NOT loud here — verified: this test passes with the
// fix reverted, because LSan's conservative stack scan keeps the loop's
// orphaned buffers "reachable" (the same LSan blind spot
// B-2026-07-31-27 documented; valgrind is the authoritative tool for
// this shape). The leak-direction gate is the valgrind-verified E2E
// `e2e_struct_reassign_displaced_drop_semantics`, which fails without
// the fix on the body side.
#[test]
fn asan_struct_reassign_displaced_value_freed_once() {
    assert_clean_asan_run(
        r#"
struct G { id: i64, s: String }
impl Drop for G {
    fn drop(mut ref self) {
        if self.id < 0i64 { println(self.id); }
    }
}
fn main() {
    let mut n = 0i64;
    let mut it = 0i64;
    while it < 200i64 {
        let mut a = G { id: it, s: f"payload-a-{it}" };
        n = n + a.s.len();
        a = G { id: it + 1i64, s: f"payload-b-{it}" };
        n = n + a.s.len();

        let mut x = G { id: it, s: f"payload-x-{it}" };
        n = n + x.s.len();
        let y = G { id: it + 2i64, s: f"payload-y-{it}" };
        x = y;
        n = n + x.s.len();

        let mut h = G { id: it, s: f"plain-{it}" };
        n = n + h.s.len();
        h = G { id: it + 3i64, s: f"plain2-{it}" };
        n = n + h.s.len();

        it = it + 1i64;
    }
    println(n);
}
"#,
        // Sum of the six per-iteration payload lengths: bases
        // ("payload-a-" x4 = 40, "plain-" = 6, "plain2-" = 7) = 53, plus
        // 6 x digits(it) — 10 one-digit + 90 two-digit + 100 three-digit
        // iterations: 200*53 + 6*(10 + 180 + 300) = 13540.
        &["13540"],
        "struct_reassign_displaced_value_freed_once",
    );
}

/// A heap field read through a TUPLE ELEMENT OF A CONTAINER ELEMENT is
/// cloned — `v[0].0.name` over `Vec[(R, i64)]` (B-2026-08-28-34).
///
/// The second defect that fix exposed, and the fourth time this family has
/// produced it: teaching the resolver to name the receiver made the read
/// COMPILE for the first time, which made its ownership reachable for the
/// first time — and it double-freed, because
/// `clone_vec_elem_heap_field_read` walks the hops down to the `Index` root
/// and only knew FIELD hops. A tuple hop in the middle ended the walk, so
/// the read handed out a shallow alias of the container element's buffer.
///
/// Every consuming position is here for the reason B-2026-08-28-24
/// established: they do not share a mechanism, and a fixture that stopped
/// at `let` would pass against a fix that misses `Vec.push` and the
/// struct-literal field. The non-consuming rows are the other direction —
/// the clone carries its own cleanup, so a read nothing takes over must
/// free it rather than leak.
///
/// Re-reading the container at the end keeps a source-cap-zeroing "fix"
/// from passing: that silences the abort and empties the element the
/// container still owns.
///
/// A FIELD-ROOTED container (`self.xs[0].0.name`) is deliberately absent:
/// this cloner requires an IDENTIFIER container, so `h.xs[0].name` — the
/// plain field spelling, with no tuple hop at all — double-frees on
/// unmodified main too. That is B-2026-08-28-41, pre-existing and a
/// different gate from the hop walk this row widened.
#[test]
fn test_container_element_tuple_field_read_is_cloned() {
    let src = r#"
struct R { id: i64, name: String }
struct Box2 { w: String }

fn sink(s: String) -> i64 { return s.len(); }
fn take(v: Vec[(R, i64)]) -> String { return v[0].0.name; }

fn main() {
    let v: Vec[(R, i64)] = [(R { id: 41, name: f"n{41}" }, 1)];
    // consuming positions
    let s = v[0].0.name;
    println(s);
    let b = Box2 { w: v[0].0.name };
    println(b.w);
    let mut o: Vec[String] = Vec.new();
    o.push(v[0].0.name);
    println(o[0]);
    println(sink(v[0].0.name));
    println(take(v));
    // non-consuming reads: the clone must free itself
    println(v[0].0.name + "!");
    println(v[0].0.name.len());
    // scalar members must not be cloned at all
    println(v[0].0.id);
    println(v[0].1);
    // the container is still intact
    println(v[0].0.name);
}
"#;
    assert_clean_asan_run(
        src,
        &[
            "n41", "n41", "n41", "3", "n41", "n41!", "3", "41", "1", "n41",
        ],
        "container-element-tuple-field-read",
    );
}

/// B-2026-08-28-15 — a heap-carrying element moved out of an owned tuple
/// by `.N` at an ESCAPING position leaves the frame; the tuple's own
/// scope-exit drop must not free it a second time.
///
/// Pre-fix this aborted the process (`free(): double free detected in
/// tcache 2`), which under ASAN reports as a heap-use-after-free inside
/// the `memcpy` that copies the returned element — the projected copy
/// reads the element's control block after the source tuple freed the
/// buffer. Note that the failure is a SIGNAL, not an output mismatch, so a
/// revert of the fix kills the process rather than printing wrong bytes.
///
/// Elements are seeded from `env.args().len()` and read back per element,
/// because the harness compiles at -O2 by default and a provably-dead
/// allocation is deleted along with the evidence (the B-2026-08-24-5
/// lesson). String payloads are built with f-strings for the same reason a
/// sibling fixture gives: a literal is rodata with `cap == 0`, so the
/// second free would be a silent no-op and the fixture would pass
/// vacuously against the very defect it exists for.
///
/// Leak coverage is the other half and is why this belongs here rather
/// than only in `tests/codegen.rs`: the fix ADDS cap-zeroing, and a
/// suppression that fired one position too wide would orphan the buffer
/// instead of double-freeing it — invisible to any exit code, caught by
/// LSan on the Linux CI leg. (e)-(g) are the positions where the consumer
/// does not take ownership, so they must stay un-suppressed.
#[test]
fn asan_tuple_elem_escaping_the_frame_is_freed_exactly_once() {
    assert_clean_asan_run(
        r#"
struct R { id: i64, name: String }
struct V { id: i64, xs: Vec[i64] }
struct H { r: R }
struct B { p: (R, i64) }
impl B { fn take(self) -> R { self.p.0 } }
fn tail(p: (R, i64)) -> R { p.0 }
fn ret(p: (R, i64)) -> R { return p.0; }
fn lit(p: (R, i64)) -> H { H { r: p.0 } }
fn nested(p: (R, i64)) -> String { p.0.name }
fn vecpay(p: (V, i64)) -> V { p.0 }
fn arr_lit(a: Array[R, 2]) -> H { H { r: a[0] } }
fn peek(p: ref (R, i64)) -> i64 { p.0.id }
fn main() {
    let base: i64 = env.args().len();
    // (a) the filed shape: fn tail off an owned tuple param.
    let a = tail((R { id: base, name: f"n{base}" }, 1));
    println(f"a={a.id} {a.name}");
    // (b) explicit `return` — a different lowering path.
    let b = ret((R { id: base + 1, name: f"n{base + 1}" }, 1));
    println(f"b={b.id} {b.name}");
    // (c) moved into an aggregate LITERAL field.
    let c = lit((R { id: base + 2, name: f"n{base + 2}" }, 1));
    println(f"c={c.r.id} {c.r.name}");
    // (d) a field INSIDE the element — the deeper-place peer.
    let d = nested((R { id: base + 3, name: f"n{base + 3}" }, 1));
    println(f"d={d}");
    // (e) a `Vec` payload, and the run-vs-build witness: pre-fix this aborted
    // under LLJIT while the -O2 AOT build survived.
    let mut xs: Vec[i64] = Vec.new();
    xs.push(base + 4);
    xs.push(base + 5);
    let e = vecpay((V { id: base + 4, xs: xs }, 1));
    println(f"e={e.id} {e.xs.len()} {e.xs[1]}");
    // (f) the ARRAY peer at the literal position.
    let f = arr_lit([R { id: base + 6, name: f"n{base + 6}" },
                     R { id: base + 7, name: f"n{base + 7}" }]);
    println(f"f={f.r.id} {f.r.name}");
    // (g) the tuple as a struct FIELD, moved off `self`.
    let g = B { p: (R { id: base + 8, name: f"n{base + 8}" }, 1) }.take();
    println(f"g={g.id} {g.name}");
    // (h) LEAK CONTROL — a `ref` param does not take ownership, so the caller
    // still owns and must still free. An over-firing suppression strands this
    // buffer; LSan is the only thing that would say so.
    let h = (R { id: base + 9, name: f"n{base + 9}" }, 1);
    println(f"h={peek(h)} {h.0.name}");
    // (i) LEAK CONTROL — the source tuple is read again after the move.
    let i = (R { id: base + 10, name: f"n{base + 10}" }, 7);
    let iv = i.0;
    println(f"i={iv.name} {i.1}");
    // (j) LEAK CONTROL — the `let` position, already correct pre-fix.
    let j = (R { id: base + 11, name: f"n{base + 11}" }, 1);
    let jv = j.0;
    println(f"j={jv.id} {jv.name}");
}
"#,
        &[
            "a=1 n1", "b=2 n2", "c=3 n3", "d=n4", "e=5 2 6", "f=7 n7", "g=9 n9", "h=10 n10",
            "i=n11 7", "j=12 n12",
        ],
        "tuple-elem-escaping-position",
    );
}

/// B-2026-08-31-30 — the `if let` / `let … else` / NESTED spellings of a
/// struct destructure double freed the moved-out field's buffer, and unlike
/// the flat `match` spelling's quiet duplicate BODY (B-2026-08-31-26) this
/// one is a real memory error, so it belongs here as well as in the E2E
/// twin.
///
/// `suppress_destructured_struct_pattern_cleanup` (#16) had exactly one
/// caller — the `match` arm loop — so the three statement forms left the
/// source field populated while the binding owned the same buffer. The
/// NESTED spelling reached #16 but stopped at its `whole_move` gate, which
/// correctly declines to mask an outer field whose sub-pattern
/// destructures; #16 now recurses into it.
///
/// `two_fields_one_taken` is the LEAK-direction control, and it is the half
/// a double-free fixture cannot see on its own: over-disarming a field the
/// pattern never bound leaks it, which is the failure both sibling
/// disarmers were fixed for (B-2026-08-04-6, B-2026-08-28-66). LSan catches
/// that only on the Linux CI leg — a local macOS asan run is blind to it.
///
/// Same two fixture rules the sibling rows record: the scrutinee is a
/// FUNCTION-SCOPE local, and the payload's BYTES are read rather than its
/// length, since a `.len()`-only buffer is a dead allocation LLVM deletes
/// outright and the fixture would then prove nothing.
#[test]
fn asan_every_struct_destructure_spelling_frees_once() {
    assert_clean_asan_run_min_allocs(
        r#"
struct R { s: String }
struct H { r: R, n: i64 }
struct Q { h: H }
struct W { a: R, b: R }
fn s_of(i: i64) -> String {
    let mut s: String = String.new();
    s.push_str("payload-padded-out-well-past-thirty-six-bytes-");
    s.push_str(f"{i}");
    return s;
}
fn if_let_spelling(k: i64) -> bool {
    let h: H = H { r: R { s: s_of(k + 1) }, n: 4 };
    let mut hit: bool = false;
    if let H { r, .. } = h { hit = r.s.contains("padded"); }
    return hit;
}
fn let_else_spelling(k: i64) -> bool {
    let h: H = H { r: R { s: s_of(k + 2) }, n: 4 };
    let H { r, .. } = h else { return false };
    return r.s.contains("padded");
}
fn nested_match_spelling(k: i64) -> bool {
    let q: Q = Q { h: H { r: R { s: s_of(k + 3) }, n: 4 } };
    let mut hit: bool = false;
    match q { Q { h: H { r, .. } } => { hit = r.s.contains("padded"); } }
    return hit;
}
fn nested_if_let_spelling(k: i64) -> bool {
    let q: Q = Q { h: H { r: R { s: s_of(k + 4) }, n: 4 } };
    let mut hit: bool = false;
    if let Q { h: H { r, .. } } = q { hit = r.s.contains("padded"); }
    return hit;
}
fn two_fields_one_taken(k: i64) -> bool {
    let w: W = W { a: R { s: s_of(k + 5) }, b: R { s: s_of(k + 6) } };
    let mut hit: bool = false;
    if let W { a, .. } = w { hit = a.s.contains("padded"); }
    return hit;
}
fn flat_match_control(k: i64) -> bool {
    let h: H = H { r: R { s: s_of(k + 7) }, n: 4 };
    let mut hit: bool = false;
    match h { H { r, .. } => { hit = r.s.contains("padded"); } }
    return hit;
}
fn main() {
    // B-2026-09-03-40 — `k` is RUNTIME-OPAQUE (argv length), and every payload
    // is derived from it. With the literal seeds this fixture used to carry,
    // `s_of(1)` builds a compile-time-known string and `contains("padded")` is
    // provably true, so once the spurious auto-par band around these bodies
    // stopped forming and LLVM could see through them, it folded the payloads
    // away entirely: allocations fell 99 -> 31 and the min-allocs floor caught
    // it. The floor was RIGHT and the fixture was leaning on the band to block
    // constant-folding. Seeding from argv restores real allocation.
    let k: i64 = env.args().len();
    println(if_let_spelling(k));
    println(let_else_spelling(k));
    println(nested_match_spelling(k));
    println(nested_if_let_spelling(k));
    println(two_fields_one_taken(k));
    println(flat_match_control(k));
    println("done");
}
"#,
        &["true", "true", "true", "true", "true", "true", "done"],
        "every-struct-destructure-spelling-frees-once",
        // Measured 26 ALLOCATIONS BY THE PROGRAM; the floor sits under that
        // with margin. Its job is unchanged — fail if a future optimizer
        // folds these payloads away, which would drop the count to near
        // zero.
        //
        // That number read 36 until B-2026-09-07-26, and 36 was never a
        // count of this program's allocations: it was ASAN's raw
        // process-wide total, which on the arm64 Linux host it was taken on
        // carries a 10-allocation start-up floor (199 on macOS — the two
        // hosts disagree by 189 for reasons that have nothing to do with
        // any fixture). The predicate is floor-relative now, so 36 - 10 =
        // 26 is the real figure, and the floor moves with it. The margin,
        // and the reasoning below about what the margin is for, are the
        // author's and are unchanged.
        //
        // It read 80 until B-2026-09-03-40, calibrated against a measured
        // 99. That 99 was NOT 99 payload allocations: roughly 63 of them
        // were auto-par machinery (per-branch env structs and the slot
        // buffer) from a band these bodies should never have formed — the
        // band gate's visibility floor was inverted and could not decline a
        // cheap group. With the gate fixed the band is gone and so is its
        // overhead, which is a WIN being reported here as a loss. The
        // payloads themselves are intact: all seven still allocate, and
        // seeding them from argv above (rather than from literals) added
        // five more, because the literal spellings were foldable in
        // principle once the band stopped blocking inlining.
        //
        // Lowering a floor deserves suspicion, so state the check plainly:
        // 26 with payloads present, near zero if they are folded. The
        // separation the floor relies on is intact.
        20,
    );
}

#[test]
fn asan_field_param_view_sibling_bodies_free_exactly_once() {
    // B-2026-09-02-10 — the heap half of the per-FIELD param-view fix.
    //
    // THE ROW'S OWN REPRO COULD NOT HAVE MEASURED THIS. It carries a
    // scalar-plus-short-tag payload, where the whole defect is a missing
    // line of output and the memory side says nothing at all — so a fix can
    // look complete against it while leaving a heap-owning sibling
    // unbalanced in either direction. Restoring a body that had been
    // silenced is exactly the change most able to introduce a DOUBLE free,
    // and re-arming the wrong field would be a use-after-free on a husk.
    //
    // Each `R` here owns a `String` tag and an eight-element `Vec[String]`
    // of long strings, so every body that runs or fails to run is visible
    // to the sanitizer rather than only to the diff. `dR<id>-<len>` reads
    // the Vec's length from inside the body, so a body running on a moved-
    // from or freed object shows up as a wrong count rather than passing
    // quietly.
    //
    // Three cells per iteration, looped so a per-path flag that fails to
    // reset between iterations is caught: only the first view lands, both
    // land, and neither lands. Measured 0 definitely/indirectly lost under
    // valgrind as well, and byte-identical on all four surfaces.
    assert_clean_asan_run(
        r#"
struct R { id: i64, tag: String, xs: Vec[String] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}-{self.xs.len()}") } }
enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("dE") } }
struct H3 { f: R, g: R, k: R }

fn mk(n: i64) -> R {
    let mut v: Vec[String] = Vec.new();
    let mut i: i64 = 0;
    while i < 8 { v.push(f"payloadpayload-{n}-{i}"); i = i + 1; }
    return R { id: n, tag: f"tag-payloadpayload-{n}", xs: v }
}

#[allow(partial_move_of_drop_enum)]
fn two_views(b: E, c: E) -> i64 {
    let mut h: H3 = H3 { f: mk(30), g: mk(31), k: mk(32) };
    match b { E.A(r) => { h.f = r; } E.B => { } }
    match c { E.A(r2) => { h.g = r2; } E.B => { } }
    println("s");
    return h.f.id + h.g.id + h.k.id
}

fn main() {
    let mut i: i64 = 0;
    while i < 3 {
        println(f"a{two_views(E.A(mk(18)), E.B)}");
        println(f"b{two_views(E.A(mk(18)), E.A(mk(19)))}");
        println(f"c{two_views(E.B, E.B)}");
        i = i + 1;
    }
    println("done");
}
"#,
        &[
            "dR30-8", "s", "dR32-8", "dR31-8", "dE", "dE", "dR18-8", "a81", "dR30-8", "dR31-8",
            "s", "dR32-8", "dE", "dR19-8", "dE", "dR18-8", "b69", "s", "dR32-8", "dR31-8",
            "dR30-8", "dE", "dE", "c93", "dR30-8", "s", "dR32-8", "dR31-8", "dE", "dE", "dR18-8",
            "a81", "dR30-8", "dR31-8", "s", "dR32-8", "dE", "dR19-8", "dE", "dR18-8", "b69", "s",
            "dR32-8", "dR31-8", "dR30-8", "dE", "dE", "c93", "dR30-8", "s", "dR32-8", "dR31-8",
            "dE", "dE", "dR18-8", "a81", "dR30-8", "dR31-8", "s", "dR32-8", "dE", "dR19-8", "dE",
            "dR18-8", "b69", "s", "dR32-8", "dR31-8", "dR30-8", "dE", "dE", "c93", "done",
        ],
        "b10-field-param-view-sibling-bodies",
    );
}

/// B-2026-09-02-26 — a LOCAL tuple scrutinee whose arm moves a HEAP-owning
/// element out, under ASAN + LSan.
///
/// The row measured a heap-free payload, so it reported only a doubled
/// `Drop` BODY and left the severity question open. With a `Vec` + `String`
/// element the same shapes are a genuine DOUBLE FREE: the pre-fix compiled
/// binary aborts with `free(): double free detected in tcache 2` on
/// `rebind`, and the interpreter prints each body twice. The tuple's element
/// walk and the value the arm handed away both owned the same buffers.
///
/// `readOnly` and `toCallee` are the controls that keep the retraction from
/// over-reaching — neither moves the element out, so the walk must stay the
/// single owner and both must still free exactly once. `twoElem` pins the
/// per-element mask against a tuple whose SECOND element is untouched, and
/// `mid` pins the element offset at a non-zero index.
///
/// The ESCAPE shape (`match t { (r, k) => { r } }` returning the element) is
/// deliberately absent: its MEMORY half is a separate, pre-existing defect
/// that aborts identically before and after this row — filed as its own row
/// — and it was only masked here because the pre-fix binary aborted on an
/// earlier shape and never reached it. This row fixes that shape's BODY
/// count (pinned on a heap-free payload in `e2e_local_tuple_elem_rebind_runs_one_body`)
/// and leaves its buffers to that row.
#[test]
fn asan_local_tuple_elem_rebind_frees_once() {
    assert_clean_asan_run(
            "struct D { id: i64, xs: Vec[i64], name: String }\n\
             impl Drop for D { fn drop(mut ref self) { println(f\"  dD{self.id}:{self.xs.len()}:{self.name}\") } }\n\
             fn mkD(id: i64) -> D {\n\
             \x20   let mut v: Vec[i64] = Vec.new();\n\
             \x20   v.push(id);\n\
             \x20   return D { id: id, xs: v, name: f\"n{id}\" }\n\
             }\n\
             fn sinkD(d: D) -> i64 { d.xs.len() }\n\
             fn rebind()   { let t = (mkD(1), 0); match t { (r, k) => { let m = r; println(f\"  rb{m.xs.len()}:{m.name}\") } } }\n\
             fn twoElem()  { let t = (mkD(2), mkD(3)); match t { (r, q) => { let m = r; println(f\"  te{m.xs.len()}:{m.name}\") } } }\n\
             fn readOnly() { let t = (mkD(5), 0); match t { (r, k) => { println(f\"  ro{r.xs.len()}:{r.name}\") } } }\n\
             fn toCallee() { let t = (mkD(6), 0); match t { (r, k) => { println(f\"  tc{sinkD(r)}\") } } }\n\
             fn mid()      { let t = (0, mkD(7), 0); match t { (a, r, k) => { let m = r; println(f\"  md{m.xs.len()}:{m.name}\") } } }\n\
             fn main() {\n\
             \x20   rebind();\n\
             \x20   twoElem();\n\
             \x20   readOnly();\n\
             \x20   toCallee();\n\
             \x20   mid();\n\
             \x20   println(\"end\")\n\
             }\n",
            &[
                "rb1:n1",
                "  dD1:1:n1",
                "  te1:n2",
                "  dD2:1:n2",
                "  dD3:1:n3",
                "  ro1:n5",
                "  dD5:1:n5",
                "  tc1",
                "  dD6:1:n6",
                "  md1:n7",
                "  dD7:1:n7",
                "end",
            ],
            "b26-local-tuple-elem-rebind",
        );
}

/// B-2026-09-03-28 — TWO TUPLE SHAPES WHOSE ELEMENT TYPES MANGLE ALIKE
/// SHARE ONE MEMORY DROP WALKER, so the second GEPs its elements at the
/// FIRST one's offsets and the `Option` payload is never freed.
///
/// `let t = (0, Option.Some(mkD(8))); let t2 = t;` is clean as a
/// one-function program and leaks a whole 56-byte `D` (plus 34 indirect)
/// once `let t = (f"s{5}", Option.Some(mkD(5)));` is DEFINED alongside it —
/// called or not, `KARAC_AUTO_PAR=0` and under auto-par alike. Every `Drop`
/// BODY still fires exactly once on all four surfaces, so nothing in the
/// program's output says anything is wrong; only the buffers survive.
///
/// THE MEMORY TWIN OF B-2026-09-03-13, one table over, and its own doc
/// comment predicted this exactly: "the element `TypeExpr`s reaching here
/// are unresolved for exactly the elements that matter". Measured with the
/// symbol names dumped at the memo:
///
///   __karac_drop_tuple_te__Option_gDg  agg={ {ptr,i64,i64}, {i64 x4} }  <- strElem0
///   __karac_drop_tuple_te__Option_gDg  agg={ i64,          {i64 x4} }  <- ctlScalar, REUSED
///
/// Both leading elements mangle to the EMPTY string — `infer_arg_elem_te`
/// resolves neither an integer literal nor an f-string to a named type — so
/// `tuple_te_sig` alone cannot tell `(String, Option[D])` from
/// `(i64, Option[D])`. The `Option` sits at byte 24 in the first and byte 8
/// in the second; the shared walker reads the second's tag out of the
/// String's length word, sees no `Some`, and frees nothing.
///
/// The fix is B-2026-09-03-13's, applied to the memory walker: fold the
/// LLVM AGGREGATE TYPE into the memo key, so the symbol and the offsets its
/// body GEPs against agree by construction rather than by the element types
/// happening to be nameable.
///
/// EVERY SHAPE HERE IS A MEASURED INGREDIENT. `strElem0` is the aliasing
/// partner and `ctlScalar` the victim: leave-one-out on the pre-fix program
/// puts 56 of the 58 direct bytes on `ctlScalar` and clears them the moment
/// `strElem0` stops being DEFINED. `optHeap` / `noDrop` / `ctlPlain` /
/// `ctlNoReb` / `ctlNone` are B-2026-09-03-19's controls, kept because the
/// alias only appears among a population of tuple shapes.
///
/// `ctlOptStr` (`(mkD(9), Option.Some(f"p{9}"))`) is DELIBERATELY ABSENT,
/// and its absence is a measurement. B-2026-09-03-28 filed it as the second
/// victim of this key; it is not. Its `Option` payload is an f-string, which
/// `infer_arg_elem_te` types as an EMPTY path, so `tuple_elem_optres_drop_ok`
/// declines, the let-site takes the enum-blind LLVM-type walker, and the
/// payload is never freed — a different root cause, unaffected by this fix,
/// and still 2 bytes here. Its "only when other shapes are defined" framing
/// is OPTIMIZER DCE rather than a shape dependence: the emitted IR for
/// `ctlOptStr` and every function it calls is byte-identical (modulo symbol
/// numbering) between the one-function program that runs clean and the
/// eight-function one that leaks — at `-O2` the lone version's payload
/// `malloc` is provably dead and deleted. Filed as its own row; including it
/// here would leave this test red for someone else's defect.
///
/// The output assertion is the other half: a walker keyed by layout must
/// still run every body exactly once, which is what fails if the key is
/// ever made so tight that two genuinely identical tuples stop sharing.
#[test]
fn asan_two_tuple_shapes_with_alike_element_sigs_free_their_own_offsets() {
    assert_clean_asan_run(
            "struct D { id: i64, xs: Vec[i64], name: String }\n\
             impl Drop for D { fn drop(mut ref self) { println(f\"  dD{self.id}:{self.xs.len()}:{self.name}\") } }\n\
             fn mkD(id: i64) -> D {\n\
             \x20   let mut v: Vec[i64] = Vec.new();\n\
             \x20   v.push(id);\n\
             \x20   return D { id: id, xs: v, name: f\"n{id}\" }\n\
             }\n\
             struct Q { id: i64, xs: Vec[i64], name: String }\n\
             fn mkQ(id: i64) -> Q {\n\
             \x20   let mut v: Vec[i64] = Vec.new();\n\
             \x20   v.push(id);\n\
             \x20   return Q { id: id, xs: v, name: f\"q{id}\" }\n\
             }\n\
             fn optHeap()   { let t = (mkD(1), Option.Some(mkD(2))); let t2 = t; println(\"  oh\") }\n\
             fn noDrop()    { let t = (mkQ(3), Option.Some(mkQ(4))); let t2 = t; println(\"  nd\") }\n\
             fn strElem0()  { let t = (f\"s{5}\", Option.Some(mkD(5))); let t2 = t; println(\"  se\") }\n\
             fn ctlPlain()  { let t = (mkD(6), mkD(7)); let t2 = t; println(\"  cp\") }\n\
             fn ctlScalar() { let t = (0, Option.Some(mkD(8))); let t2 = t; println(\"  cs\") }\n\
             fn ctlNoReb()  { let t = (mkD(10), Option.Some(mkD(11))); println(\"  cn\") }\n\
             fn ctlNone()   { let t: (D, Option[D]) = (mkD(12), Option.None); let t2 = t; println(\"  cz\") }\n\
             fn main() {\n\
             \x20   optHeap(); noDrop(); strElem0(); ctlPlain();\n\
             \x20   ctlScalar();\n\
             \x20   ctlNoReb(); ctlNone();\n\
             \x20   println(\"end\")\n\
             }\n",
            &[
                "dD1:1:n1",
                "  dD2:1:n2",
                "  oh",
                "  nd",
                "  dD5:1:n5",
                "  se",
                "  dD6:1:n6",
                "  dD7:1:n7",
                "  cp",
                "  dD8:1:n8",
                "  cs",
                "  dD10:1:n10",
                "  dD11:1:n11",
                "  cn",
                "  dD12:1:n12",
                "  cz",
                "end",
            ],
            "b28-tuple-shape-keyed-drop-walker",
        );
}

/// B-2026-09-03-32 / B-2026-09-05-25 — the memory half of
/// `e2e_destructure_discard_dies_in_the_statement_and_masks_die_with_the_block`:
/// the inline residual walk fires BEFORE the source's memory drop and
/// touches no memory of its own, and the block-exit mask purge changes only
/// which bodies run, so this pins that moving the walk earlier freed nothing
/// twice and leaked nothing (valgrind: every block freed at -O0 and -O2).
#[test]
fn asan_destructure_discard_dies_in_the_statement_clean() {
    let label = "destructure_discard_dies_in_the_statement";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan(
        r#"
struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct Two { a: R, b: R }
struct Ho2 { a: R, b: Option[R] }
fn mk(k: i64) -> R { return R { id: k, tag: f"t{k}", xs: [k] } }
fn pDes(h: Two) { let Two { a, b: _ } = h; println("in") }
fn main() {
    { let h: Two = Two { a: mk(1), b: mk(101) }; let Two { a, b: _ } = h; println("one") }
    { let h: Two = Two { a: mk(2), b: mk(102) }; let Two { a: _, b } = h; println("two") }
    { let h: Two = Two { a: mk(3), b: mk(103) }; let Two { a: _, b: _ } = h; println("three") }
    { let h: Two = Two { a: mk(4), b: mk(104) }; let Two { a, b: _ } = h; println(f"use{a.id}"); println("four") }
    { let h: Two = Two { a: mk(5), b: mk(105) }; let Two { a, b } = h; println("five") }
    { let h: Two = Two { a: mk(6), b: mk(106) }; println("six") }
    { pDes(Two { a: mk(7), b: mk(107) }); println("seven") }
    { let h: Two = Two { a: mk(8), b: mk(108) }; let Two { a, b: _ } = h; let q: Two = Two { a: mk(9), b: mk(109) }; println("eight") }
    { let h: Ho2 = Ho2 { a: mk(10), b: Option.Some(mk(110)) }; let Ho2 { a, b: _ } = h; println("nine") }
    { let h: Two = Two { a: mk(11), b: mk(111) }; let Two { a: _, b } = h; println("ten") }
    println("end")
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
            "dR101", "dR1", "one", "dR2", "dR102", "two", "dR103", "dR3", "three", "dR104", "use4",
            "dR4", "four", "dR105", "dR5", "five", "dR106", "dR6", "six", "in", "dR107", "dR7",
            "seven", "dR108", "dR8", "dR109", "dR9", "eight", "dR110", "dR10", "nine", "dR11",
            "dR111", "ten", "end",
        ],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// B-2026-09-05-36 — ASAN twin of `tests/codegen.rs`'s
/// `e2e_destructured_part_or_bare_param_handed_to_a_taking_callee_has_one_owner`:
/// a destructured part or bare param handed to a returning / storing
/// callee is freed exactly once and its body runs exactly once. Heap `R`
/// (`String` + `Vec`) so a lost free is a leak LSan sees.
#[test]
fn asan_destructured_part_or_bare_param_handed_to_a_taking_callee_clean() {
    let label = "destructured_part_or_bare_param_handed_to_a_taking_callee";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct W { r: R, n: i64 }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
fn consume(x: R) -> i64 { return x.id }
fn wrap(x: R) -> R { return x }
fn stash(x: R, v: mut ref Vec[R]) { v.push(x) }
fn wrapw(x: R) -> W { return W { r: x, n: 1 } }
fn t_fwd_let(t: (R, i64)) -> R { let (r, k) = t; wrap(r) }
fn t_fwd_let_ret(t: (R, i64)) -> R { let (r, k) = t; return wrap(r); }
fn t_stash_let(t: (R, i64), v: mut ref Vec[R]) -> i64 { let (r, k) = t; stash(r, v); k }
fn t_consume_let(t: (R, i64)) -> i64 { let (r, k) = t; consume(r) }
fn t_fwdw_let(t: (R, i64)) -> W { let (r, k) = t; wrapw(r) }
fn s_fwd_let(w: W) -> R { let W { r, n } = w; wrap(r) }
fn s_stash_let(w: W, v: mut ref Vec[R]) -> i64 { let W { r, n } = w; stash(r, v); n }
fn b_stash(x: R, v: mut ref Vec[R]) { stash(x, v) }
fn b_stash_ret(x: R, v: mut ref Vec[R]) -> i64 { stash(x, v); 7 }
fn b_fwd(x: R) -> R { wrap(x) }
fn b_consume(x: R) -> i64 { consume(x) }
fn main() {
    { let a: R = t_fwd_let((mk(1), 0)); println(f"r{a.id}"); println("one") }
    { let a: R = t_fwd_let_ret((mk(2), 0)); println(f"r{a.id}"); println("two") }
    { let mut v: Vec[R] = []; let d: i64 = t_stash_let((mk(3), 0), mut v); println(f"r{d} n{v.len()}"); println("three") }
    { let d: i64 = t_consume_let((mk(4), 0)); println(f"r{d}"); println("four") }
    { let w: W = t_fwdw_let((mk(5), 0)); println(f"r{w.r.id}"); println("five") }
    { let a: R = s_fwd_let(W { r: mk(6), n: 1 }); println(f"r{a.id}"); println("six") }
    { let mut v: Vec[R] = []; let d: i64 = s_stash_let(W { r: mk(7), n: 1 }, mut v); println(f"r{d} n{v.len()}"); println("seven") }
    { let mut v: Vec[R] = []; b_stash(mk(8), mut v); println(f"n{v.len()}"); println("eight") }
    { let mut v: Vec[R] = []; let d: i64 = b_stash_ret(mk(9), mut v); println(f"r{d} n{v.len()}"); println("nine") }
    { let a: R = b_fwd(mk(10)); println(f"r{a.id}"); println("ten") }
    { let d: i64 = b_consume(mk(11)); println(f"r{d}"); println("eleven") }
    { let t: (R, i64) = (mk(13), 0); let a: R = t_fwd_let(t); println(f"r{a.id}"); println("thirteen") }
    { let t: (R, i64) = (mk(14), 0); let mut v: Vec[R] = []; let d: i64 = t_stash_let(t, mut v); println(f"r{d} n{v.len()}"); println("fourteen") }
    { let x: R = mk(15); let mut v: Vec[R] = []; b_stash(x, mut v); println(f"n{v.len()}"); println("fifteen") }
    { let w: W = W { r: mk(16), n: 1 }; let a: R = s_fwd_let(w); println(f"r{a.id}"); println("sixteen") }
    println("end")
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
            "r1", "dR1", "one", "r2", "dR2", "two", "r0 n1", "dR3", "three", "dR4", "r4", "four",
            "r5", "dR5", "five", "r6", "dR6", "six", "r1 n1", "dR7", "seven", "n1", "dR8", "eight",
            "r7 n1", "dR9", "nine", "r10", "dR10", "ten", "dR11", "r11", "eleven", "r13", "dR13",
            "thirteen", "r0 n1", "dR14", "fourteen", "n1", "dR15", "fifteen", "r16", "dR16",
            "sixteen", "end",
        ],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// B-2026-09-05-17 — ASAN twin of `tests/codegen.rs`'s
/// `e2e_forwarded_place_struct_arg_field_handed_back_has_one_owner`: a part
/// handed back through a forwarding call is freed once and its body runs
/// once. Heap `R` (`String` + `Vec`) so a lost free is a leak LSan sees.
#[test]
fn asan_forwarded_place_struct_arg_field_handed_back_clean() {
    let label = "forwarded_place_struct_arg_field_handed_back";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
struct Cd { r: R, z: i64 }
struct W { r: R, n: i64 }
fn cEsc(h: Cd) -> R { let Cd { r, z } = h; println("in"); return r; }
fn cProj(h: Cd) -> R { return h.r; }
fn tEsc(t: (R, i64)) -> R { let (r, k) = t; return r; }
fn fwd(g: Cd) -> R { return cEsc(g); }
fn fwd_tail(g: Cd) -> R { cEsc(g) }
fn fwd_proj(g: Cd) -> R { return cProj(g); }
fn fwd2(g: Cd) -> R { return fwd(g); }
fn fwd_t(t: (R, i64)) -> R { return tEsc(t); }
fn fwd_wrap(g: Cd) -> W { return W { r: cEsc(g), n: 1 }; }
fn fwd_cond(g: Cd, c: bool) -> R { if c { return cEsc(g); } return mk(99); }
fn fwd_let(g: Cd) -> R { let x: R = cEsc(g); return x; }
fn fwd_i(g: Cd) -> i64 { let x: R = cEsc(g); return x.id; }
struct H { n: i64 }
impl H { fn m_fwd(ref self, g: Cd) -> R { return cEsc(g); } }
fn main() {
    let h: H = H { n: 1 };
    { let g: Cd = Cd { r: mk(61), z: 9 }; let out: R = fwd(g); println(f"got{out.id}"); println("one") }
    { let g: Cd = Cd { r: mk(62), z: 9 }; let out: R = cEsc(g); println(f"got{out.id}"); println("two") }
    { let g: Cd = Cd { r: mk(63), z: 9 }; let out: R = fwd_tail(g); println(f"got{out.id}"); println("three") }
    { let g: Cd = Cd { r: mk(64), z: 9 }; let out: R = fwd_proj(g); println(f"got{out.id}"); println("four") }
    { let g: Cd = Cd { r: mk(65), z: 9 }; let out: R = fwd2(g); println(f"got{out.id}"); println("five") }
    { let t: (R, i64) = (mk(66), 0); let out: R = fwd_t(t); println(f"got{out.id}"); println("six") }
    { let g: Cd = Cd { r: mk(67), z: 9 }; let out: W = fwd_wrap(g); println(f"got{out.r.id}"); println("seven") }
    { let g: Cd = Cd { r: mk(69), z: 9 }; let out: R = fwd_cond(g, false); println(f"got{out.id}"); println("nine") }
    { let g: Cd = Cd { r: mk(70), z: 9 }; let out: R = fwd_let(g); println(f"got{out.id}"); println("ten") }
    { let g: Cd = Cd { r: mk(71), z: 9 }; let d: i64 = fwd_i(g); println(f"got{d}"); println("eleven") }
    { let out: R = fwd(Cd { r: mk(72), z: 9 }); println(f"got{out.id}"); println("twelve") }
    { let g: Cd = Cd { r: mk(73), z: 9 }; let out: R = h.m_fwd(g); println(f"got{out.id}"); println("thirteen") }
    println("end")
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
            "in", "got61", "dR61", "one", "in", "got62", "dR62", "two", "in", "got63", "dR63",
            "three", "got64", "dR64", "four", "in", "got65", "dR65", "five", "got66", "dR66",
            "six", "in", "got67", "dR67", "seven", "dR69", "got99", "dR99", "nine", "in", "got70",
            "dR70", "ten", "in", "dR71", "got71", "eleven", "in", "got72", "dR72", "twelve", "in",
            "got73", "dR73", "thirteen", "end",
        ],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// B-2026-09-03-4 — ASAN twin of `tests/codegen.rs`'s
/// `e2e_destructured_part_returned_in_a_constructor_has_one_owner`: a part
/// returned inside a constructor is freed once and its body runs once.
/// Heap `R` (`String` + `Vec`) so a lost free is a leak LSan sees.
#[test]
fn asan_destructured_part_returned_in_a_constructor_clean() {
    let label = "destructured_part_returned_in_a_constructor";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
enum E { A(R), B(i64) }
struct W { r: R, n: i64 }
struct Cd { r: R, z: i64 }
fn f_m(t: (R, i64)) -> Option[R] { match t { (r, k) => { Option.Some(r) } } }
fn f_l(t: (R, i64)) -> Option[R] { let (r, k) = t; Option.Some(r) }
fn f_lr(t: (R, i64)) -> Option[R] { let (r, k) = t; return Option.Some(r); }
fn f_res(t: (R, i64)) -> Result[R, i64] { let (r, k) = t; Result.Ok(r) }
fn f_e(t: (R, i64)) -> E { let (r, k) = t; E.A(r) }
fn f_em(t: (R, i64)) -> E { match t { (r, k) => { E.A(r) } } }
fn f_w(t: (R, i64)) -> W { let (r, k) = t; W { r: r, n: k } }
fn f_s(g: Cd) -> Option[R] { let Cd { r, z } = g; Option.Some(r) }
fn f_sm(g: Cd) -> Option[R] { match g { Cd { r, z } => { Option.Some(r) } } }
fn f_local() -> Option[R] { let t: (R, i64) = (mk(9), 0); let (r, k) = t; Option.Some(r) }
fn f_bare(x: R) -> Option[R] { Option.Some(x) }
fn f_two(t: (R, R)) -> Option[R] { let (a, b) = t; Option.Some(a) }
fn main() {
    { let g: Option[R] = f_m((mk(1), 0)); println("got"); println("one") }
    { let g: Option[R] = f_l((mk(2), 0)); println("got"); println("two") }
    { let g: Option[R] = f_lr((mk(3), 0)); println("got"); println("three") }
    { let g: Result[R, i64] = f_res((mk(4), 0)); println("got"); println("four") }
    { let g: E = f_e((mk(5), 0)); println("got"); println("five") }
    { let g: E = f_em((mk(6), 0)); println("got"); println("six") }
    { let g: W = f_w((mk(7), 0)); println(f"got{g.r.id}"); println("seven") }
    { let g: Option[R] = f_s(Cd { r: mk(8), z: 1 }); println("got"); println("eight") }
    { let g: Option[R] = f_local(); println("got"); println("nine") }
    { let g: Option[R] = f_bare(mk(10)); println("got"); println("ten") }
    { let g: Option[R] = f_two((mk(11), mk(12))); println("got"); println("eleven") }
    { let t: (R, i64) = (mk(13), 0); let g: Option[R] = f_m(t); println("got"); println("thirteen") }
    { let t: (R, i64) = (mk(14), 0); let g: Option[R] = f_l(t); println("got"); println("fourteen") }
    { let g: Option[R] = f_sm(Cd { r: mk(15), z: 1 }); println("got"); println("fifteen") }
    { let g: Option[R] = f_m((mk(16), 0)); match g { Option.Some(x) => { println(f"x{x.id}") }, Option.None => { println("none") } } println("sixteen") }
    println("end")
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
            "dR1", "got", "one", "dR2", "got", "two", "dR3", "got", "three", "dR4", "got", "four",
            "dR5", "got", "five", "dR6", "got", "six", "got7", "dR7", "seven", "dR8", "got",
            "eight", "dR9", "got", "nine", "dR10", "got", "ten", "dR12", "dR11", "got", "eleven",
            "dR13", "got", "thirteen", "dR14", "got", "fourteen", "dR15", "got", "fifteen", "x16",
            "dR16", "sixteen", "end",
        ],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// B-2026-09-02-41 — ASAN twin of `tests/codegen.rs`'s
/// `e2e_two_step_destructure_of_a_nested_tuple_field_has_one_owner`: the
/// nested tuple leaf is freed once and its body runs once. Heap `R`
/// (`String` + `Vec`), which is what made the pre-fix state a double free
/// rather than a body miscount.
#[test]
fn asan_two_step_destructure_of_a_nested_tuple_field_clean() {
    let label = "two_step_destructure_of_a_nested_tuple_field";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
struct H2 { pe: ((R, i64), i64) }
struct H1 { pe: (R, i64) }
struct H3 { pe: (((R, i64), i64), i64) }
fn v3(h: H2) { let (inner, y) = h.pe; let (r, x) = inner; let m: R = r; println(f"v3 {m.id}") }
fn v3_read(h: H2) { let (inner, y) = h.pe; let (r, x) = inner; println(f"v3r {r.id}") }
fn v3_unread(h: H2) { let (inner, y) = h.pe; let (r, x) = inner; println("v3u") }
fn v3_ret(h: H2) -> R { let (inner, y) = h.pe; let (r, x) = inner; return r; }
fn v3_one(h: H2) { let (inner, y) = h.pe; println("v3o") }
fn v3_inner_move(h: H2) { let (inner, y) = h.pe; let z: (R, i64) = inner; println(f"v3m {z.0.id}") }
fn flat(h: H1) { let (r, k) = h.pe; let m: R = r; println(f"flat {m.id}") }
fn flat_read(h: H1) { let (r, k) = h.pe; println(f"flatr {r.id}") }
fn deep(h: H3) { let (mid, a) = h.pe; let (inner, b) = mid; let (r, c) = inner; let m: R = r; println(f"deep {m.id}") }
fn local3() { let h: H2 = H2 { pe: ((mk(9), 1), 2) }; let (inner, y) = h.pe; let (r, x) = inner; let m: R = r; println(f"l3 {m.id}") }
fn main() {
    { v3(H2 { pe: ((mk(1), 1), 2) }); println("one") }
    { v3_read(H2 { pe: ((mk(2), 1), 2) }); println("two") }
    { v3_unread(H2 { pe: ((mk(3), 1), 2) }); println("three") }
    { v3_one(H2 { pe: ((mk(5), 1), 2) }); println("five") }
    { flat(H1 { pe: (mk(7), 1) }); println("seven") }
    { flat_read(H1 { pe: (mk(8), 1) }); println("eight") }
    { deep(H3 { pe: (((mk(10), 1), 2), 3) }); println("ten") }
    { let h: H2 = H2 { pe: ((mk(11), 1), 2) }; v3(h); println("eleven") }
    println("end")
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
            "v3 1", "dR1", "one", "v3r 2", "dR2", "two", "v3u", "dR3", "three", "v3o", "dR5",
            "five", "flat 7", "dR7", "seven", "flatr 8", "dR8", "eight", "deep 10", "dR10", "ten",
            "v3 11", "dR11", "eleven", "end",
        ],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// B-2026-09-06-6 — ASAN twin of `tests/codegen.rs`'s
/// `e2e_whole_rebind_of_a_tuple_param_view_runs_one_body`: the rebound
/// tuple view's element is freed once and its body runs once. Heap `R`
/// (`String` + `Vec`), so a second owner would be a double free here and
/// not only a body miscount.
#[test]
fn asan_whole_rebind_of_a_tuple_param_view_clean() {
    let label = "whole_rebind_of_a_tuple_param_view";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
struct H2 { pe: ((R, i64), i64) }
struct H1 { pe: (R, i64) }
fn take(t: (R, i64)) { println(f"take {t.0.id}") }
fn v3m(h: H2) { let (inner, y) = h.pe; let z: (R, i64) = inner; println(f"v3m {z.0.id}") }
fn v3m_unread(h: H2) { let (inner, y) = h.pe; let z: (R, i64) = inner; println("v3mu") }
fn v3m_destr(h: H2) { let (inner, y) = h.pe; let z: (R, i64) = inner; let (r, x) = z; println(f"v3md {r.id}") }
fn v3m_twice(h: H2) { let (inner, y) = h.pe; let z: (R, i64) = inner; let w: (R, i64) = z; println(f"v3mt {w.0.id}") }
fn v3m_ret(h: H2) -> (R, i64) { let (inner, y) = h.pe; let z: (R, i64) = inner; return z; }
fn v3m_untyped(h: H2) { let (inner, y) = h.pe; let z = inner; println(f"vu {z.0.id}") }
fn v3m_take(h: H2) { let (inner, y) = h.pe; let z: (R, i64) = inner; take(z); println("vt") }
fn flat_m(h: H1) { let p: (R, i64) = h.pe; println(f"fm {p.0.id}") }
fn t_m(t: (R, i64)) { let z: (R, i64) = t; println(f"tm {z.0.id}") }
fn t_destr(t: (R, i64)) { let z: (R, i64) = t; let (r, n) = z; println(f"td {r.id} {n}") }
fn t_take(t: (R, i64)) { let z: (R, i64) = t; take(z); println("tt") }
fn t_ret(t: (R, i64)) -> (R, i64) { let z: (R, i64) = t; return z; }
fn t_untyped(t: (R, i64)) { let z = t; println(f"tu {z.0.id}") }
fn main() {
    { v3m(H2 { pe: ((mk(1), 1), 2) }); println("one") }
    { v3m_unread(H2 { pe: ((mk(2), 1), 2) }); println("two") }
    { v3m_destr(H2 { pe: ((mk(3), 1), 2) }); println("three") }
    { v3m_twice(H2 { pe: ((mk(4), 1), 2) }); println("four") }
    { let a: (R, i64) = v3m_ret(H2 { pe: ((mk(5), 1), 2) }); println(f"got{a.0.id}"); println("five") }
    { flat_m(H1 { pe: (mk(6), 1) }); println("six") }
    { t_m((mk(7), 1)); println("seven") }
    { let h: H2 = H2 { pe: ((mk(8), 1), 2) }; v3m(h); println("eight") }
    { v3m_untyped(H2 { pe: ((mk(9), 1), 2) }); println("nine") }
    { v3m_take(H2 { pe: ((mk(10), 1), 2) }); println("ten") }
    { t_destr((mk(11), 11)); println("eleven") }
    { t_take((mk(12), 12)); println("twelve") }
    { let a: (R, i64) = t_ret((mk(13), 13)); println(f"got{a.0.id}"); println("thirteen") }
    { t_untyped((mk(14), 14)); println("fourteen") }
    { let t: (R, i64) = (mk(15), 15); t_m(t); println("fifteen") }
    println("end")
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
            "v3m 1", "dR1", "one", "v3mu", "dR2", "two", "v3md 3", "dR3", "three", "v3mt 4", "dR4",
            "four", "got5", "dR5", "five", "fm 6", "dR6", "six", "tm 7", "dR7", "seven", "v3m 8",
            "dR8", "eight", "vu 9", "dR9", "nine", "take 10", "vt", "dR10", "ten", "td 11 11",
            "dR11", "eleven", "take 12", "tt", "dR12", "twelve", "got13", "dR13", "thirteen",
            "tu 14", "dR14", "fourteen", "tm 15", "dR15", "fifteen", "end"
        ],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// B-2026-09-06-5 — ASAN twin of `tests/codegen.rs`'s
/// `e2e_nested_tuple_element_handed_back_two_levels_deep_runs_one_body`:
/// the leaf handed back two tuple levels deep is freed once and its body
/// runs once. Heap `R` (`String` + `Vec`), so the masked walk must not
/// have moved any free.
#[test]
fn asan_nested_tuple_element_handed_back_two_levels_deep_clean() {
    let label = "nested_tuple_element_handed_back_two_levels_deep";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
struct H2 { pe: ((R, i64), i64) }
struct H3 { pe: (((R, i64), i64), i64) }
struct H2b { pe: ((R, R), i64) }
struct S { r: R, n: i64 }
struct H4 { pe: ((S, i64), i64) }
fn v3_ret(h: H2) -> R { let (inner, y) = h.pe; let (r, x) = inner; return r; }
fn v3_ret_direct(h: H2) -> R { return h.pe.0.0; }
fn v3_ret_z(h: H2) -> R { let z: (R, i64) = h.pe.0; let (r, x) = z; return r; }
fn v4_ret(h: H3) -> R { let (mid, a) = h.pe; let (inner, b) = mid; let (r, c) = inner; return r; }
fn vb_ret0(h: H2b) -> R { let (inner, y) = h.pe; let (r, s) = inner; return r; }
fn vb_ret1(h: H2b) -> R { let (inner, y) = h.pe; let (r, s) = inner; return s; }
fn vs_ret(h: H4) -> R { let (inner, y) = h.pe; let (s, x) = inner; let S { r, n } = s; return r; }
fn v3_ret_tuple(h: H2) -> (R, i64) { let (inner, y) = h.pe; return inner; }
fn main() {
    { let a: R = v3_ret(H2 { pe: ((mk(1), 1), 2) }); println(f"got{a.id}"); println("one") }
    { let a: R = v3_ret_direct(H2 { pe: ((mk(2), 1), 2) }); println(f"got{a.id}"); println("two") }
    { let a: R = v3_ret_z(H2 { pe: ((mk(3), 1), 2) }); println(f"got{a.id}"); println("three") }
    { let a: R = v4_ret(H3 { pe: (((mk(4), 1), 2), 3) }); println(f"got{a.id}"); println("four") }
    { let a: R = vb_ret0(H2b { pe: ((mk(5), mk(6)), 2) }); println(f"got{a.id}"); println("five") }
    { let a: R = vb_ret1(H2b { pe: ((mk(7), mk(8)), 2) }); println(f"got{a.id}"); println("six") }
    { let a: R = vs_ret(H4 { pe: ((S { r: mk(9), n: 1 }, 1), 2) }); println(f"got{a.id}"); println("seven") }
    { let a: (R, i64) = v3_ret_tuple(H2 { pe: ((mk(10), 1), 2) }); println(f"got{a.0.id}"); println("eight") }
    println("end")
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
            "got1", "dR1", "one", "got2", "dR2", "two", "got3", "dR3", "three", "got4", "dR4",
            "four", "dR6", "got5", "dR5", "five", "dR7", "got8", "dR8", "six", "got9", "dR9",
            "seven", "got10", "dR10", "eight", "end"
        ],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// B-2026-09-06-10 — ASAN twin of `tests/codegen.rs`'s
/// `e2e_named_local_handing_back_a_nested_part_runs_one_body`: the part a
/// named local hands back through the callee is freed once and its body
/// runs once. Heap `R` (`String` + `Vec`), so the widened mask must not
/// have moved any free.
#[test]
fn asan_named_local_handing_back_a_nested_part_clean() {
    let label = "named_local_handing_back_a_nested_part";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
struct H1 { pe: (R, i64) }
struct H2 { pe: ((R, i64), i64) }
struct S { r: R, n: i64 }
struct G { s: S, n: i64 }
struct H2b { pe: ((R, R), i64) }
fn flat_ret(h: H1) -> R { let (r, k) = h.pe; return r; }
fn flat_proj(h: H1) -> R { return h.pe.0; }
fn v3_ret(h: H2) -> R { let (inner, y) = h.pe; let (r, x) = inner; return r; }
fn g_ret(g: G) -> R { return g.s.r; }
fn g_destr(g: G) -> R { let G { s, n } = g; let S { r, n: m } = s; return r; }
fn vb_ret1(h: H2b) -> R { let (inner, y) = h.pe; let (r, s) = inner; return s; }
fn flat_in(h: H1) -> R { println("in"); let (r, k) = h.pe; println("mid"); return r; }
fn main() {
    { let h: H1 = H1 { pe: (mk(1), 1) }; let a: R = flat_ret(h); println(f"got{a.id}"); println("one") }
    { let h: H2 = H2 { pe: ((mk(2), 1), 2) }; let a: R = v3_ret(h); println(f"got{a.id}"); println("two") }
    { let g: G = G { s: S { r: mk(3), n: 1 }, n: 2 }; let a: R = g_ret(g); println(f"got{a.id}"); println("three") }
    { let a: R = flat_ret(H1 { pe: (mk(4), 1) }); println(f"got{a.id}"); println("four") }
    { let a: R = g_ret(G { s: S { r: mk(5), n: 1 }, n: 2 }); println(f"got{a.id}"); println("five") }
    { let h: H1 = H1 { pe: (mk(6), 1) }; let a: R = flat_in(h); println("out"); println(f"got{a.id}"); println("six") }
    { let h: H1 = H1 { pe: (mk(7), 1) }; let a: R = flat_in(h); println("out"); println(f"got{a.id}"); println(f"h{h.pe.1}"); println("seven") }
    { let h: H1 = H1 { pe: (mk(8), 1) }; let a: R = flat_proj(h); println(f"got{a.id}"); println("eight") }
    { let g: G = G { s: S { r: mk(9), n: 1 }, n: 2 }; let a: R = g_destr(g); println(f"got{a.id}"); println("nine") }
    { let h: H2b = H2b { pe: ((mk(10), mk(11)), 2) }; let a: R = vb_ret1(h); println(f"got{a.id}"); println("ten") }
    println("end")
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
            "got1", "dR1", "one", "got2", "dR2", "two", "got3", "dR3", "three", "got4", "dR4",
            "four", "got5", "dR5", "five", "in", "mid", "out", "got6", "dR6", "six", "in", "mid",
            "out", "got7", "dR7", "h1", "seven", "got8", "dR8", "eight", "got9", "dR9", "nine",
            "dR10", "got11", "dR11", "ten", "end"
        ],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// B-2026-09-06-11 — ASAN twin of `tests/codegen.rs`'s
/// `e2e_tuple_argument_handing_back_a_nested_part_runs_one_body`: the
/// part a tuple argument hands back below its top level is freed once
/// and its body runs once. Heap `R` (`String` + `Vec`), so the deep
/// masks must not have moved any free.
#[test]
fn asan_tuple_argument_handing_back_a_nested_part_clean() {
    let label = "tuple_argument_handing_back_a_nested_part";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
struct S { r: R, n: i64 }
fn tv_ret(t: ((R, i64), i64)) -> R { let (inner, y) = t; let (r, x) = inner; return r; }
fn tv_proj(t: ((R, i64), i64)) -> R { return t.0.0; }
fn flat_t(t: (R, i64)) -> R { let (r, k) = t; return r; }
fn ts_ret(t: (S, i64)) -> R { let (s, k) = t; return s.r; }
fn t3_ret(t: (((R, i64), i64), i64)) -> R { let (mid, a) = t; let (inner, b) = mid; let (r, c) = inner; return r; }
fn tb_ret1(t: ((R, R), i64)) -> R { let (inner, y) = t; let (a, b) = inner; return b; }
fn tv_read(t: ((R, i64), i64)) -> i64 { let (inner, y) = t; let (r, x) = inner; return r.id; }
fn main() {
    { let t: ((R, i64), i64) = ((mk(1), 1), 2); let a: R = tv_ret(t); println(f"got{a.id}"); println("one") }
    { let t: (R, i64) = (mk(2), 2); let a: R = flat_t(t); println(f"got{a.id}"); println("two") }
    { let t: (S, i64) = (S { r: mk(3), n: 1 }, 2); let a: R = ts_ret(t); println(f"got{a.id}"); println("three") }
    { let a: R = tv_ret(((mk(4), 1), 2)); println(f"got{a.id}"); println("four") }
    { let a: R = ts_ret((S { r: mk(5), n: 1 }, 2)); println(f"got{a.id}"); println("five") }
    { let t: ((R, i64), i64) = ((mk(6), 1), 2); let a: R = tv_proj(t); println(f"got{a.id}"); println("six") }
    { let t: ((R, i64), i64) = ((mk(7), 1), 2); let a: R = tv_ret(t); println(f"got{a.id}"); println(f"k{t.1}"); println("seven") }
    { let t: (((R, i64), i64), i64) = (((mk(8), 1), 2), 3); let a: R = t3_ret(t); println(f"got{a.id}"); println("eight") }
    { let t: ((R, R), i64) = ((mk(9), mk(10)), 2); let a: R = tb_ret1(t); println(f"got{a.id}"); println("nine") }
    { let a: R = tb_ret1(((mk(11), mk(12)), 2)); println(f"got{a.id}"); println("ten") }
    { let d: i64 = tv_read(((mk(13), 1), 2)); println(f"r{d}"); println("eleven") }
    { let t: ((R, i64), i64) = ((mk(14), 1), 2); let d: i64 = tv_read(t); println(f"r{d}"); println("twelve") }
    println("end")
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
            "got1", "dR1", "one", "got2", "dR2", "two", "got3", "dR3", "three", "got4", "dR4",
            "four", "got5", "dR5", "five", "got6", "dR6", "six", "got7", "dR7", "k2", "seven",
            "got8", "dR8", "eight", "dR9", "got10", "dR10", "nine", "dR11", "got12", "dR12", "ten",
            "dR13", "r13", "eleven", "dR14", "r14", "twelve", "end"
        ],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// B-2026-09-06-30 — ASAN twin of `tests/codegen.rs`'s
/// `e2e_let_destructure_of_a_mixed_struct_literal_runs_one_body`: a
/// destructure leaf bound out of a view field / element goes memory-only,
/// so its buffer is still freed exactly once (heap `R`, `String` field)
/// while its body is the caller's.
#[test]
fn asan_let_destructure_of_a_mixed_struct_literal_clean() {
    let label = "let_destructure_of_a_mixed_struct_literal";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan(
        r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"n{i}" }; }
struct S3 { a: R, b: R }
fn d_b(r: R) -> i64 { let s: S3 = S3 { a: r, b: mk(2) }; let S3 { a, b } = s; return b.id; }
fn d_a(r: R) -> i64 { let s: S3 = S3 { a: r, b: mk(4) }; let S3 { a, b } = s; return a.id; }
fn d_unread(r: R) -> i64 { let s: S3 = S3 { a: r, b: mk(6) }; let S3 { a, b } = s; return 1; }
fn d_rebind(r: R) -> i64 { let s: S3 = S3 { a: r, b: mk(8) }; let s2: S3 = s; let S3 { a, b } = s2; return b.id; }
fn d_rebind_leaf(r: R) -> i64 { let s: S3 = S3 { a: r, b: mk(10) }; let S3 { a, b } = s; let m: R = a; return m.id; }
fn d_swap(r: R) -> i64 { let s: S3 = S3 { a: mk(12), b: r }; let S3 { a, b } = s; return a.id; }
fn d_fresh(r: R) -> i64 { let s: S3 = S3 { a: mk(14), b: mk(15) }; let S3 { a, b } = s; return a.id; }
fn t_b(r: R) -> i64 { let t: (R, R) = (r, mk(19)); let (a, b) = t; return b.id; }
fn t_rebind_leaf(r: R) -> i64 { let t: (R, R) = (r, mk(21)); let (a, b) = t; let m: R = a; return m.id; }
fn main() {
    { let v: i64 = d_b(mk(1)); println(f"v={v}"); println("one") }
    { let v: i64 = d_a(mk(3)); println(f"v={v}"); println("two") }
    { let v: i64 = d_unread(mk(5)); println(f"v={v}"); println("three") }
    { let v: i64 = d_rebind(mk(7)); println(f"v={v}"); println("four") }
    { let v: i64 = d_rebind_leaf(mk(9)); println(f"v={v}"); println("five") }
    { let v: i64 = d_swap(mk(11)); println(f"v={v}"); println("six") }
    { let v: i64 = d_fresh(mk(13)); println(f"v={v}"); println("seven") }
    { let v: i64 = t_b(mk(18)); println(f"v={v}"); println("eight") }
    { let v: i64 = t_rebind_leaf(mk(20)); println(f"v={v}"); println("nine") }
    println("end")
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
            "dR2", "dR1", "v=2", "one", "dR4", "dR3", "v=3", "two", "dR6", "dR5", "v=1", "three",
            "dR8", "dR7", "v=8", "four", "dR10", "dR9", "v=9", "five", "dR12", "dR11", "v=12",
            "six", "dR15", "dR14", "dR13", "v=14", "seven", "dR19", "dR18", "v=19", "eight",
            "dR21", "dR20", "v=20", "nine", "end"
        ],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}
