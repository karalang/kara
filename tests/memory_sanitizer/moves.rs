//! moves, owned values, fresh temporaries, discarded values, clones -- fixtures for `tests/memory_sanitizer.rs`.
//!
//! Split out of `tests/memory_sanitizer.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test memory_sanitizer` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test memory_sanitizer moves::
//!
//! New fixtures about moves, owned values, fresh temporaries, discarded values, clones belong in this file.

use super::*;

/// B-2026-09-04-30 — a by-value `self` receiver on a TEMP runs its `Drop`
/// bodies, and the fresh-temp receiver's field walk runs BEFORE its memory is
/// freed.
///
/// Codegen treats a by-value `self` exactly like a by-value param —
/// caller-retained — while B-2026-08-01-5 had excluded owned `self` from the
/// caller's receiver-temp registration to stop a passthrough chain
/// double-firing. A local receiver still had an owner and a by-value param temp
/// always had one (`param-twin`), but a TEMP receiver had none on any surface.
/// The three guard cells (`returns-self`, `hands-field-out`, `generic-return`)
/// pin that a return which can carry the receiver still stands the caller down.
/// `temp-recv-refself` pins the second half — the no-own-`Drop` arm's
/// bodies-then-memory registration order let the LIFO drain free the fields
/// before the walk read them.
///
/// ASAN twin of `e2e_owned_self_temp_receiver_runs_drop_bodies` (tests/codegen.rs).
/// The balance is the pin the stdout cannot give: pre-fix, the `ref self`
/// cell read a freed two-byte tag, which valgrind and LSan both flag and a
/// stdout comparison only renders as mojibake.
#[test]
fn asan_owned_self_temp_receiver_is_balanced() {
    assert_clean_asan_run(
        r#"struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}") } }
fn mk(n: i64) -> R { return R { id: n, tag: f"t{n}" }; }
struct HoRes { a: R, b: Result[R, String] }
struct Pair { a: R, b: R }
struct OwnD { a: R, n: i64 }
impl Drop for OwnD { fn drop(mut ref self) { println(f"dOwnD{self.n}") } }

impl HoRes { fn plain(self) { println(f"  rd{self.a.id}") } }
impl Pair { fn eat(self) { println(f"  pr{self.a.id}") } fn peek(ref self) { println(f"  bo{self.a.id}") } }
impl OwnD { fn eat(self) { println(f"  od{self.n}") } }
impl R {
  fn ident(self) { println(f"  id{self.id}") }
  fn area(self) -> i64 { return self.id * 2; }
  fn me(self) -> R { return self; }
}
struct W { a: R }
impl W { fn unwrap_a(self) -> R { return self.a; } fn opt(self) -> Option[R] { return Option.Some(self.a); } }

fn p_plain(h: HoRes) { println(f"  pd{h.a.id}") }

fn main() {
  println("temp-recv-fields");  HoRes { a: mk(1), b: Result.Ok(mk(101)) }.plain()
  println("temp-recv-own");     mk(2).ident()
  println("temp-recv-ownd");    OwnD { a: mk(3), n: 3 }.eat()
  println("temp-recv-pair");    Pair { a: mk(4), b: mk(104) }.eat()
  println("temp-recv-refself"); Pair { a: mk(5), b: mk(105) }.peek()
  println("param-twin");        p_plain(HoRes { a: mk(6), b: Result.Ok(mk(106)) })
  println("local-recv");        let h = HoRes { a: mk(7), b: Result.Ok(mk(107)) }; h.plain()
  println("scalar-return");     let v = mk(8).area(); println(f"  v{v}")
  println("returns-self");      let m = mk(9).me(); println(f"  m{m.id}")
  println("hands-field-out");   let g = W { a: mk(10) }.unwrap_a(); println(f"  g{g.id}")
  println("generic-return");    let o = W { a: mk(11) }.opt(); println("  built")
  println("done")
}
"#,
        &[
            "temp-recv-fields",
            "  rd1",
            "dR101/t101",
            "dR1/t1",
            "temp-recv-own",
            "  id2",
            "dR2/t2",
            "temp-recv-ownd",
            "  od3",
            "dOwnD3",
            "dR3/t3",
            "temp-recv-pair",
            "  pr4",
            "dR104/t104",
            "dR4/t4",
            "temp-recv-refself",
            "  bo5",
            "dR105/t105",
            "dR5/t5",
            "param-twin",
            "  pd6",
            "dR106/t106",
            "dR6/t6",
            "local-recv",
            "  rd7",
            "dR107/t107",
            "dR7/t7",
            "scalar-return",
            "dR8/t8",
            "  v16",
            "returns-self",
            "  m9",
            "dR9/t9",
            "hands-field-out",
            "  g10",
            "dR10/t10",
            "generic-return",
            "dR11/t11",
            "  built",
            "done",
        ],
        "asan_owned_self_temp_receiver_is_balanced",
    );
}

/// B-2026-09-06-15 — the MEMORY half of
/// `e2e_bare_owned_struct_self_scrutinee_binds_views` (tests/codegen.rs):
/// routing a bare owned struct `self` scrutinee, and the nested arms under a
/// plain-struct pattern's leaves, through the owned-param VIEW channel is a
/// bodies-only change — every leaf keeps the memory registration it had and
/// the nested arm's payload binding takes the memory-only struct drop a
/// one-level `match h.e` already takes. This pins that: balanced on the
/// named-local and fresh-temp receiver, on the free-function twin, and on
/// the `String`-leaf and scalar-leaf spellings. The `shell` cells (a leaf
/// bound and never consumed) are deliberately absent: that leaf's heap
/// leaks at `-O0` on the by-value PARAM spelling too, before and after
/// this fix, and is filed on its own row.
#[test]
fn asan_bare_owned_struct_self_scrutinee_is_balanced() {
    assert_clean_asan_run(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("  dE") } }
struct H1 { e: E }
struct H2 { e: E, n: i64 }
struct Hs { e: E, s: String }
fn consume(x: R) -> i64 { return x.id }

impl H1 {
    #[allow(partial_move_of_drop_enum)]
    fn whole(self) -> i64 { match self { H1 { e } => { match e { E.A(r) => { let m = r; return m.id; } E.B => { return 0; } } } } }
    fn plain(self) -> i64 { match self { H1 { e } => { match e { E.A(r) => { return r.id; } E.B => { return 0; } } } } }
    fn viacall(self) -> i64 { match self { H1 { e } => { match e { E.A(r) => { return consume(r); } E.B => { return 0; } } } } }
    fn rebind(self) -> i64 { match self { H1 { e } => { let k = e; match k { E.A(r) => { return r.id; } E.B => { return 0; } } } } }
}
impl H2 {
    fn two(self) -> i64 { match self { H2 { e, n } => { match e { E.A(r) => { return r.id + n; } E.B => { return n; } } } } }
}
impl Hs {
    fn strleaf(self) -> i64 { match self { Hs { e, s } => { let m = s; println(f"  s{m}"); match e { E.A(r) => { return r.id; } E.B => { return 0; } } } } }
}
impl E {
    #[allow(partial_move_of_drop_enum)]
    fn m_r(self) -> R { match self { E.A(r) => { return r; } E.B => { return mk(0); } } }
}
#[allow(partial_move_of_drop_enum)]
fn p_whole(h: H1) -> i64 { match h { H1 { e } => { match e { E.A(r) => { let m = r; return m.id; } E.B => { return 0; } } } } }
fn p_plain(h: H1) -> i64 { match h { H1 { e } => { match e { E.A(r) => { return r.id; } E.B => { return 0; } } } } }
fn p_rebind(h: H1) -> i64 { match h { H1 { e } => { let k = e; match k { E.A(r) => { return r.id; } E.B => { return 0; } } } } }
fn p_strleaf(h: Hs) -> i64 { match h { Hs { e, s } => { let m = s; println(f"  s{m}"); match e { E.A(r) => { return r.id; } E.B => { return 0; } } } } }

fn main() {
    println("whole/local"); let a1 = H1 { e: E.A(mk(31)) }; let x1 = a1.whole(); println(f"  r{x1}");
    println("whole/temp"); let x2 = H1 { e: E.A(mk(32)) }.whole(); println(f"  r{x2}");
    println("plain/local"); let a3 = H1 { e: E.A(mk(33)) }; let x3 = a3.plain(); println(f"  r{x3}");
    println("plain/temp"); let x4 = H1 { e: E.A(mk(34)) }.plain(); println(f"  r{x4}");
    println("viacall/local"); let a5 = H1 { e: E.A(mk(35)) }; let x5 = a5.viacall(); println(f"  r{x5}");
    println("viacall/temp"); let x6 = H1 { e: E.A(mk(36)) }.viacall(); println(f"  r{x6}");
    println("rebind/local"); let a7 = H1 { e: E.A(mk(37)) }; let x7 = a7.rebind(); println(f"  r{x7}");
    println("rebind/temp"); let x8 = H1 { e: E.A(mk(38)) }.rebind(); println(f"  r{x8}");
    println("two/local"); let a11 = H2 { e: E.A(mk(41)), n: 100 }; let x11 = a11.two(); println(f"  r{x11}");
    println("two/temp"); let x12 = H2 { e: E.A(mk(42)), n: 100 }.two(); println(f"  r{x12}");
    println("strleaf/local"); let a13 = Hs { e: E.A(mk(43)), s: "sa".to_string() }; let x13 = a13.strleaf(); println(f"  r{x13}");
    println("strleaf/temp"); let x14 = Hs { e: E.A(mk(44)), s: "sb".to_string() }.strleaf(); println(f"  r{x14}");
    println("p_whole/local"); let b1 = H1 { e: E.A(mk(51)) }; let y1 = p_whole(b1); println(f"  r{y1}");
    println("p_whole/temp"); let y2 = p_whole(H1 { e: E.A(mk(52)) }); println(f"  r{y2}");
    println("p_plain/local"); let b3 = H1 { e: E.A(mk(53)) }; let y3 = p_plain(b3); println(f"  r{y3}");
    println("p_rebind/local"); let b4 = H1 { e: E.A(mk(54)) }; let y4 = p_rebind(b4); println(f"  r{y4}");
    println("p_strleaf/local"); let b6 = Hs { e: E.A(mk(56)), s: "sc".to_string() }; let y6 = p_strleaf(b6); println(f"  r{y6}");
    println("enum_recv/local"); let c1 = E.A(mk(61)); let r1 = c1.m_r(); println(f"  r{r1.id}");
    println("enum_recv/temp"); let r2 = E.A(mk(62)).m_r(); println(f"  r{r2.id}");
    println("end");
}
"#,
        &[
            "whole/local",
            "  dE",
            "  dR31",
            "  r31",
            "whole/temp",
            "  dE",
            "  dR32",
            "  r32",
            "plain/local",
            "  dE",
            "  dR33",
            "  r33",
            "plain/temp",
            "  dE",
            "  dR34",
            "  r34",
            "viacall/local",
            "  dE",
            "  dR35",
            "  r35",
            "viacall/temp",
            "  dE",
            "  dR36",
            "  r36",
            "rebind/local",
            "  dE",
            "  dR37",
            "  r37",
            "rebind/temp",
            "  dE",
            "  dR38",
            "  r38",
            "two/local",
            "  dE",
            "  dR41",
            "  r141",
            "two/temp",
            "  dE",
            "  dR42",
            "  r142",
            "strleaf/local",
            "  ssa",
            "  dE",
            "  dR43",
            "  r43",
            "strleaf/temp",
            "  ssb",
            "  dE",
            "  dR44",
            "  r44",
            "p_whole/local",
            "  dE",
            "  dR51",
            "  r51",
            "p_whole/temp",
            "  dE",
            "  dR52",
            "  r52",
            "p_plain/local",
            "  dE",
            "  dR53",
            "  r53",
            "p_rebind/local",
            "  dE",
            "  dR54",
            "  r54",
            "p_strleaf/local",
            "  ssc",
            "  dE",
            "  dR56",
            "  r56",
            "enum_recv/local",
            "  dE",
            "  r61",
            "  dR61",
            "enum_recv/temp",
            "  dE",
            "  r62",
            "  dR62",
            "end",
        ],
        "asan_bare_owned_struct_self_scrutinee_is_balanced",
    );
}

/// B-2026-09-06-42 — `let e = self;` inside an owned-`self` method on a value
/// enum with its own `Drop` double-freed the entry-copied payload at -O0 and
/// under the JIT (`free(): double free detected in tcache 2`; valgrind: two
/// frees of one block from `main`), the let-lowering's whole-rebind source
/// cap-zero having admitted an `Identifier` source only. This pins the fixture
/// of `e2e_whole_self_rebind_in_owned_method_runs_each_body_once` under ASAN /
/// LSan: no double free, and no leak from the caller's retained memory action
/// once its bodies stand down. valgrind measured 0 errors (leak check on) at
/// -O0 and -O2 before this landed as a test.
#[test]
fn asan_whole_self_rebind_in_owned_method_is_balanced() {
    assert_clean_asan_run(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("  dE") } }
enum F { A(R), B }
struct S { r: R }
struct Sd { r: R }
impl Drop for Sd { fn drop(mut ref self) { println("  dS") } }
impl E {
    fn m_let(self) -> i64 { let e = self; match e { E.A(r) => { return r.id; } E.B => { return 0; } } }
    fn m_cond(self, c: bool) -> i64 { if c { let e = self; match e { E.A(r) => { return r.id; } E.B => { return 0; } } } else { match self { E.A(r) => { return r.id + 100; } E.B => { return 100; } } } }
    fn m_mut(self) -> i64 { let mut e = self; e = E.B; match e { E.A(r) => { return r.id; } E.B => { return 0; } } }
}
impl F { fn m_let(self) -> i64 { let e = self; match e { F.A(r) => { return r.id; } F.B => { return 0; } } } }
impl S { fn m_let(self) -> i64 { let e = self; return e.r.id; } }
impl Sd { fn m_let(self) -> i64 { let e = self; return e.r.id; } }
fn f_let(x: E) -> i64 { let e = x; match e { E.A(r) => { return r.id; } E.B => { return 0; } } }
fn main() {
    println("enum/local"); let a = E.A(mk(1)); let x1 = a.m_let(); println(f"  x{x1}");
    println("enum/temp"); let x2 = E.A(mk(2)).m_let(); println(f"  x{x2}");
    println("noshell/local"); let b = F.A(mk(3)); let x3 = b.m_let(); println(f"  x{x3}");
    println("noshell/temp"); let x4 = F.A(mk(4)).m_let(); println(f"  x{x4}");
    println("struct/local"); let c = S { r: mk(5) }; let x5 = c.m_let(); println(f"  x{x5}");
    println("struct/temp"); let x6 = S { r: mk(6) }.m_let(); println(f"  x{x6}");
    println("structdrop/local"); let d = Sd { r: mk(7) }; let x7 = d.m_let(); println(f"  x{x7}");
    println("structdrop/temp"); let x8 = Sd { r: mk(8) }.m_let(); println(f"  x{x8}");
    println("free/local"); let g = E.A(mk(9)); let x9 = f_let(g); println(f"  x{x9}");
    println("free/temp"); let x10 = f_let(E.A(mk(10))); println(f"  x{x10}");
    println("cond-true/local"); let h = E.A(mk(11)); let x11 = h.m_cond(true); println(f"  x{x11}");
    println("cond-false/local"); let i = E.A(mk(12)); let x12 = i.m_cond(false); println(f"  x{x12}");
    println("mut/local"); let j = E.A(mk(13)); let x13 = j.m_mut(); println(f"  x{x13}");
    println("end");
}
"#,
        &[
            "enum/local",
            "  dE",
            "  dR1",
            "  x1",
            "enum/temp",
            "  dE",
            "  dR2",
            "  x2",
            "noshell/local",
            "  dR3",
            "  x3",
            "noshell/temp",
            "  dR4",
            "  x4",
            "struct/local",
            "  dR5",
            "  x5",
            "struct/temp",
            "  dR6",
            "  x6",
            "structdrop/local",
            "  dS",
            "  dR7",
            "  x7",
            "structdrop/temp",
            "  dS",
            "  dR8",
            "  x8",
            "free/local",
            "  dE",
            "  dR9",
            "  x9",
            "free/temp",
            "  dE",
            "  dR10",
            "  x10",
            "cond-true/local",
            "  dE",
            "  dR11",
            "  x11",
            "cond-false/local",
            "  dR12",
            "  dE",
            "  x112",
            "mut/local",
            "  dE",
            "  dR13",
            "  dE",
            "  x0",
            "end",
        ],
        "asan_whole_self_rebind_in_owned_method_is_balanced",
    );
}

/// B-2026-09-07-8 — the MEMORY half: the same program under ASAN + LSan,
/// where the pre-fix build double-freed the object on the hand-back path.
/// One owner and one free per object, with the rebind in place.
#[test]
fn asan_rebound_param_into_a_mixed_path_callee() {
    assert_clean_asan_run(
        "shared struct Inner { v: i64 }\n\
             struct R { id: i64, name: String, inner: Inner }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"  dR{self.id}\") } }\n\
             fn mk(i: i64) -> R { return R { id: i, name: f\"h{i}\", inner: Inner { v: i } }; }\n\
             fn f(r: R, c: bool) -> R { let m = r; if c { return m; } return mk(99); }\n\
             fn direct(a: R, c: bool) { f(a, c); }\n\
             fn rebound(a: R, c: bool) { let q = a; f(q, c); println(\"  in\"); }\n\
             fn rebound_bound(a: R, c: bool) -> i64 { let q = a; let w = f(q, c); return w.id; }\n\
             fn rebound_only(a: R) { let q = a; println(\"  only\"); }\n\
             fn main() {\n\
               println(\"direct_handback\"); direct(mk(1), true);\n\
               println(\"rebound_handback\"); rebound(mk(2), true);\n\
               println(\"rebound_bound\"); println(f\"  v={rebound_bound(mk(3), true)}\");\n\
               println(\"rebound_no_call\"); rebound_only(mk(4));\n\
               println(\"end\");\n\
             }\n",
        &[
            "direct_handback",
            "  dR1",
            "rebound_handback",
            "  dR2",
            "  in",
            "rebound_bound",
            "  dR3",
            "  v=3",
            "rebound_no_call",
            "  only",
            "  dR4",
            "end",
        ],
        "rebound_param_into_a_mixed_path_callee",
    );
}

/// B-2026-09-06-16 — the MEMORY half of
/// `tests/codegen.rs`'s `e2e_owned_self_field_let_runs_one_body`: the same
/// program under ASAN + LSan. `let e = self.e` is now a VIEW of the
/// caller-retained receiver on the compiled backends (its bodies cancelled
/// in the let epilogue, its memory the projection-view drop the by-value
/// parameter path already registers), so this pins that the field's
/// `String` / `Vec` buffers have exactly one owner on every spelling,
/// fresh-temp receivers included.
#[test]
fn asan_owned_self_field_let_is_a_view() {
    assert_clean_asan_run(
            "struct R { id: i64, tag: String, xs: Vec[i64] }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"  dR{self.id}\") } }\n\
             fn mk(i: i64) -> R { return R { id: i, tag: f\"t{i}\", xs: [i] } }\n\
             enum E { A(R), B }\n\
             impl Drop for E { fn drop(mut ref self) { println(\"  dE\") } }\n\
             struct S { e: E }\n\
             struct H1 { e: E }\n\
             struct H2 { s: S }\n\
             \n\
             impl H1 {\n\
             #[allow(partial_move_of_drop_enum)]\n\
             \x20   fn fieldlet(self) -> i64 { let e = self.e; match e { E.A(r) => { let m = r; return m.id; } E.B => { return 0; } } }\n\
             \x20   fn readlet(self) -> i64 { let e = self.e; match e { E.A(r) => { return r.id; } E.B => { return 0; } } }\n\
             \x20   fn justlet(self) -> i64 { let e = self.e; return 7; }\n\
             \x20   fn borrowedlet(mut ref self) -> i64 { let e = self.e; match e { E.A(r) => { return r.id; } E.B => { return 0; } } }\n\
             }\n\
             impl H2 {\n\
             #[allow(partial_move_of_drop_enum)]\n\
             \x20   fn deep(self) -> i64 { let e = self.s.e; match e { E.A(r) => { let m = r; return m.id; } E.B => { return 0; } } }\n\
             #[allow(partial_move_of_drop_enum)]\n\
             \x20   fn mid(self) -> i64 { let s = self.s; match s.e { E.A(r) => { let m = r; return m.id; } E.B => { return 0; } } }\n\
             }\n\
             #[allow(partial_move_of_drop_enum)]\n\
             fn p_fieldlet(h: H1) -> i64 { let e = h.e; match e { E.A(r) => { let m = r; return m.id; } E.B => { return 0; } } }\n\
             \n\
             fn main() {\n\
             \x20   println(\"fieldlet/local\"); let a1 = H1 { e: E.A(mk(1)) }; let x1 = a1.fieldlet(); println(f\"  r{x1}\");\n\
             \x20   println(\"fieldlet/temp\"); let x2 = H1 { e: E.A(mk(2)) }.fieldlet(); println(f\"  r{x2}\");\n\
             \x20   println(\"readlet/local\"); let a3 = H1 { e: E.A(mk(3)) }; let x3 = a3.readlet(); println(f\"  r{x3}\");\n\
             \x20   println(\"justlet/local\"); let a4 = H1 { e: E.A(mk(4)) }; let x4 = a4.justlet(); println(f\"  r{x4}\");\n\
             \x20   println(\"deep/local\"); let a5 = H2 { s: S { e: E.A(mk(5)) } }; let x5 = a5.deep(); println(f\"  r{x5}\");\n\
             \x20   println(\"mid/local\"); let a6 = H2 { s: S { e: E.A(mk(6)) } }; let x6 = a6.mid(); println(f\"  r{x6}\");\n\
             \x20   println(\"deep/temp\"); let x7 = H2 { s: S { e: E.A(mk(7)) } }.deep(); println(f\"  r{x7}\");\n\
             \x20   println(\"borrowedlet/local\"); let mut a8 = H1 { e: E.A(mk(8)) }; let x8 = a8.borrowedlet(); println(f\"  r{x8}\");\n\
             \x20   println(\"p_fieldlet/local\"); let a9 = H1 { e: E.A(mk(9)) }; let x9 = p_fieldlet(a9); println(f\"  r{x9}\");\n\
             \x20   println(\"p_fieldlet/temp\"); let x10 = p_fieldlet(H1 { e: E.A(mk(10)) }); println(f\"  r{x10}\");\n\
             \x20   println(\"end\");\n\
             }\n",
            &[
                "fieldlet/local",
                "  dE",
                "  dR1",
                "  r1",
                "fieldlet/temp",
                "  dE",
                "  dR2",
                "  r2",
                "readlet/local",
                "  dE",
                "  dR3",
                "  r3",
                "justlet/local",
                "  dE",
                "  dR4",
                "  r7",
                "deep/local",
                "  dE",
                "  dR5",
                "  r5",
                "mid/local",
                "  dE",
                "  dR6",
                "  r6",
                "deep/temp",
                "  dE",
                "  dR7",
                "  r7",
                "borrowedlet/local",
                "  dE",
                "  dR8",
                "  dE",
                "  dR8",
                "  r8",
                "p_fieldlet/local",
                "  dE",
                "  dR9",
                "  r9",
                "p_fieldlet/temp",
                "  dE",
                "  dR10",
                "  r10",
                "end",
            ],
            "b16-owned-self-field-let",
        );
}

/// B-2026-08-31-43 — the MEMORY half of
/// `tests/codegen.rs`'s `e2e_owned_self_projection_scrutinee_runs_one_payload_body`:
/// the same program with a heap-carrying payload under ASAN + LSan. The
/// arm's binding over an owned `self` projection is now a VIEW of the
/// caller-retained receiver (memory only), so this pins that handing its
/// body back to the caller's walk left exactly one owner of the payload's
/// `String` / `Vec` buffers — and that a fresh-temp receiver's bodies,
/// newly retained caller-side, free nothing twice.
#[test]
fn asan_owned_self_projection_scrutinee_leaf_is_a_view() {
    assert_clean_asan_run(
            "struct R { id: i64, tag: String, xs: Vec[i64] }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"  dR{self.id}\") } }\n\
             fn mk(i: i64) -> R { return R { id: i, tag: f\"t{i}\", xs: [i] } }\n\
             enum E { A(R), B }\n\
             impl Drop for E { fn drop(mut ref self) { println(\"  dE\") } }\n\
             struct S { e: E }\n\
             struct H1 { e: E }\n\
             struct H2 { s: S }\n\
             fn consume(x: R) -> i64 { return x.id }\n\
             \n\
             impl H1 {\n\
             #[allow(partial_move_of_drop_enum)]\n\
             \x20   fn take(self) -> i64 { match self.e { E.A(r) => { let m = r; return m.id; } E.B => { return 0; } } }\n\
             \x20   fn read(self) -> i64 { match self.e { E.A(r) => { return r.id; } E.B => { return 0; } } }\n\
             \x20   fn viacall(self) -> i64 { match self.e { E.A(r) => { return consume(r); } E.B => { return 0; } } }\n\
             #[allow(partial_move_of_drop_enum)]\n\
             \x20   fn iflet(self) -> i64 { if let E.A(r) = self.e { let m = r; return m.id; } else { return 0; } }\n\
             #[allow(partial_move_of_drop_enum)]\n\
             \x20   fn letelse(self) -> i64 { let E.A(r) = self.e else { return 0; }; let m = r; return m.id; }\n\
             #[allow(partial_move_of_drop_enum)]\n\
             \x20   fn whilelet(self) -> i64 { while let E.A(r) = self.e { let m = r; return m.id; } return 0; }\n\
             #[allow(partial_move_of_drop_enum)]\n\
             \x20   fn borrowed(mut ref self) -> i64 { match self.e { E.A(r) => { let m = r; return m.id; } E.B => { return 0; } } }\n\
             }\n\
             impl H2 {\n\
             #[allow(partial_move_of_drop_enum)]\n\
             \x20   fn take2(self) -> i64 { match self.s.e { E.A(r) => { let m = r; return m.id; } E.B => { return 0; } } }\n\
             \x20   fn read2(self) -> i64 { match self.s.e { E.A(r) => { return r.id; } E.B => { return 0; } } }\n\
             }\n\
             #[allow(partial_move_of_drop_enum)]\n\
             fn p_take(h: H1) -> i64 { match h.e { E.A(r) => { let m = r; return m.id; } E.B => { return 0; } } }\n\
             #[allow(partial_move_of_drop_enum)]\n\
             fn p_iflet(h: H1) -> i64 { if let E.A(r) = h.e { let m = r; return m.id; } else { return 0; } }\n\
             \n\
             fn main() {\n\
             \x20   println(\"take/local\"); let a1 = H1 { e: E.A(mk(1)) }; let x1 = a1.take(); println(f\"  r{x1}\");\n\
             \x20   println(\"take/temp\"); let x2 = H1 { e: E.A(mk(2)) }.take(); println(f\"  r{x2}\");\n\
             \x20   println(\"take2/local\"); let a3 = H2 { s: S { e: E.A(mk(3)) } }; let x3 = a3.take2(); println(f\"  r{x3}\");\n\
             \x20   println(\"read/local\"); let a4 = H1 { e: E.A(mk(4)) }; let x4 = a4.read(); println(f\"  r{x4}\");\n\
             \x20   println(\"read2/local\"); let a5 = H2 { s: S { e: E.A(mk(5)) } }; let x5 = a5.read2(); println(f\"  r{x5}\");\n\
             \x20   println(\"viacall/local\"); let a6 = H1 { e: E.A(mk(6)) }; let x6 = a6.viacall(); println(f\"  r{x6}\");\n\
             \x20   println(\"viacall/temp\"); let x7 = H1 { e: E.A(mk(7)) }.viacall(); println(f\"  r{x7}\");\n\
             \x20   println(\"iflet/local\"); let a8 = H1 { e: E.A(mk(8)) }; let x8 = a8.iflet(); println(f\"  r{x8}\");\n\
             \x20   println(\"iflet/temp\"); let x9 = H1 { e: E.A(mk(9)) }.iflet(); println(f\"  r{x9}\");\n\
             \x20   println(\"letelse/local\"); let a10 = H1 { e: E.A(mk(10)) }; let x10 = a10.letelse(); println(f\"  r{x10}\");\n\
             \x20   println(\"whilelet/local\"); let a11 = H1 { e: E.A(mk(11)) }; let x11 = a11.whilelet(); println(f\"  r{x11}\");\n\
             \x20   println(\"borrowed/local\"); let mut a12 = H1 { e: E.A(mk(12)) }; let x12 = a12.borrowed(); println(f\"  r{x12}\");\n\
             \x20   println(\"p_take/local\"); let a13 = H1 { e: E.A(mk(13)) }; let x13 = p_take(a13); println(f\"  r{x13}\");\n\
             \x20   println(\"p_iflet/local\"); let a14 = H1 { e: E.A(mk(14)) }; let x14 = p_iflet(a14); println(f\"  r{x14}\");\n\
             \x20   println(\"end\");\n\
             }\n",
            &[
                "take/local",
                "  dE",
                "  dR1",
                "  r1",
                "take/temp",
                "  dE",
                "  dR2",
                "  r2",
                "take2/local",
                "  dE",
                "  dR3",
                "  r3",
                "read/local",
                "  dE",
                "  dR4",
                "  r4",
                "read2/local",
                "  dE",
                "  dR5",
                "  r5",
                "viacall/local",
                "  dE",
                "  dR6",
                "  r6",
                "viacall/temp",
                "  dE",
                "  dR7",
                "  r7",
                "iflet/local",
                "  dE",
                "  dR8",
                "  r8",
                "iflet/temp",
                "  dE",
                "  dR9",
                "  r9",
                "letelse/local",
                "  dE",
                "  dR10",
                "  r10",
                "whilelet/local",
                "  dE",
                "  dR11",
                "  r11",
                "borrowed/local",
                "  dR12",
                "  dE",
                "  dR12",
                "  r12",
                "p_take/local",
                "  dE",
                "  dR13",
                "  r13",
                "p_iflet/local",
                "  dE",
                "  dR14",
                "  r14",
                "end",
            ],
            "b43-owned-self-projection-scrutinee",
        );
}

/// B-2026-09-02-27 — MOVING A HEAP FIELD OUT OF A BARE-TUPLE ELEMENT
/// BINDING MUST DISARM THAT FIELD IN THE TUPLE, NOT IN THE COPY.
///
/// B-2026-09-02-23 made the tuple the single owner of its element. Every
/// existing move-out disarm (`zero_struct_field_move_cap`) writes into the
/// SOURCE BINDING's alloca -- which for a tuple element is a bit-copy, not
/// the slot `__karac_drop_tuple_*` reads. So `let n = r.name;` handed the
/// buffer to `n` and left the tuple freeing it as well: a double free at
/// BOTH optimization levels (unlike -23's class, which `-O2` masked),
/// against a correct `--interp`.
///
/// THE CONTROL IS THE POINT, because the fix suppresses a free and the
/// failure mode of over-reaching is a leak (LSan catches it here):
/// - `bare` — the identical move off a plain by-value param, never in a
///   tuple. Clean before and after; it must keep zeroing its own slot and
///   must not start leaking.
/// - `noMove` — the same binding with the field only READ, never moved out.
///   The tuple must still free it exactly once.
///
/// The `Vec` field is moved out as well as the `String`, so the fix is
/// exercised on both heap kinds rather than only on the one the row's
/// repro happened to use.
#[test]
fn asan_tuple_element_field_move_out_disarms_the_tuple() {
    assert_clean_asan_run(
            "struct H { id: i64, xs: Vec[i64], name: String }\n\
             fn mk(id: i64) -> H {\n\
             \x20   let mut v: Vec[i64] = Vec.new();\n\
             \x20   v.push(id);\n\
             \x20   return H { id: id, xs: v, name: f\"n{id}\" }\n\
             }\n\
             fn moveStr(t: (H, i64)) {\n\
             \x20   match t { (r, k) => { let n = r.name; println(f\"  s{r.xs.len()}:{n}\") } }\n\
             }\n\
             fn moveVec(t: (H, i64)) {\n\
             \x20   match t { (r, k) => { let w = r.xs; println(f\"  v{w.len()}:{r.name}\") } }\n\
             }\n\
             fn bare(h: H) { let n = h.name; println(f\"  bare{h.xs.len()}:{n}\") }\n\
             fn noMove(t: (H, i64)) { match t { (r, k) => { println(f\"  nm{r.xs.len()}:{r.name}\") } } }\n\
             fn main() {\n\
             \x20   moveStr((mk(1), 0));\n\
             \x20   moveVec((mk(2), 0));\n\
             \x20   bare(mk(3));\n\
             \x20   noMove((mk(4), 0));\n\
             \x20   println(\"end\")\n\
             }\n",
            &["s1:n1", "  v1:n2", "  bare1:n3", "  nm1:n4", "end"],
            "b27-tuple-elem-field-move-out",
        );
}

/// B-2026-09-02-32 — MOVING THE WHOLE ELEMENT OUT OF A BARE-TUPLE PATTERN
/// MUST DISARM THE TUPLE, NOT THE COPY.
///
/// The whole-element sibling of `asan_tuple_element_field_move_out_disarms_
/// the_tuple` above, and the memory analogue of what B-2026-08-31-7 fixed
/// for `Drop` BODIES. Same root shape: every move-out disarm writes into the
/// SOURCE BINDING's alloca, which for a tuple element is a bit-copy rather
/// than the slot `__karac_drop_tuple_*` reads, so after `let m = r;` the
/// tuple went on freeing buffers `m` owns — `free(): double free detected in
/// tcache 2` at BOTH optimization levels and with auto-par either way,
/// against a correct `--interp`.
///
/// -27 zeroes ONE field's `cap` because a field move-out gives away exactly
/// one buffer. A whole-element move gives away all of them and names no
/// single field, so the repair walks the element's entire heap-field set in
/// the tuple's slot instead.
///
/// THE CONTROLS CARRY THE WEIGHT, because the fix SUPPRESSES A FREE and
/// every way of over-reaching is a leak or a silenced body (LSan and the
/// output assertion catch them respectively):
/// - `withBody` — the element type has a user `Drop`. Restoring memory
///   ownership must not also silence the body: `dD2` has to appear exactly
///   once, and its `xs.len()`/`name` read from inside the body prove it ran
///   on a live value rather than on a husk.
/// - `mid` — the moved element sits in the MIDDLE of a three-element tuple,
///   so a repair that GEP'd the wrong index would corrupt a neighbour
///   rather than silently work.
/// - `bare` — the identical whole rebind off a plain by-value param, never
///   in a tuple. It must keep disarming its own slot and must not leak.
/// - `noMove` — the element only READ. The tuple must still free it once.
///
/// `viaProj` is a PROJECTION scrutinee (`match s.t`). B-2026-09-02-27 could
/// not reach that shape and said so; B-2026-09-02-34 widened the slot
/// recording to projection and nested scrutinees, and this fix inherits the
/// widening for free. It is pinned here rather than left as accidental
/// coverage, so a future narrowing of -34 fails loudly instead of silently
/// giving the double free back.
///
/// NOT WIDENED TO THE SIBLING ROWS, which were measured byte-identical
/// before and after this change: B-2026-09-02-25 (the `let (r, k) = t`
/// spelling) and B-2026-09-02-26 (a LOCAL tuple scrutinee) are about the
/// `Drop` BODY count, not the heap, and go through different machinery.
/// (-25 has since been fixed on its own, by the marking pinned in
/// `asan_let_tuple_destructure_leaf_has_one_owner` just below; -26 is still
/// open. The independence recorded here is what made the two commits safe to
/// land separately.)
#[test]
fn asan_tuple_element_whole_move_out_disarms_the_tuple() {
    assert_clean_asan_run(
            "struct H { id: i64, xs: Vec[i64], name: String }\n\
             struct D { id: i64, xs: Vec[i64], name: String }\n\
             struct S { t: (H, i64) }\n\
             impl Drop for D { fn drop(mut ref self) { println(f\"  dD{self.id}:{self.xs.len()}:{self.name}\") } }\n\
             fn mk(id: i64) -> H {\n\
             \x20   let mut v: Vec[i64] = Vec.new();\n\
             \x20   v.push(id);\n\
             \x20   return H { id: id, xs: v, name: f\"n{id}\" }\n\
             }\n\
             fn mkD(id: i64) -> D {\n\
             \x20   let mut v: Vec[i64] = Vec.new();\n\
             \x20   v.push(id);\n\
             \x20   return D { id: id, xs: v, name: f\"n{id}\" }\n\
             }\n\
             fn whole(t: (H, i64)) { match t { (r, k) => { let m = r; println(f\"  w{m.xs.len()}:{m.name}\") } } }\n\
             fn withBody(t: (D, i64)) { match t { (r, k) => { let m = r; println(f\"  b{m.xs.len()}:{m.name}\") } } }\n\
             fn mid(t: (i64, H, i64)) { match t { (a, r, k) => { let m = r; println(f\"  m{m.xs.len()}:{m.name}\") } } }\n\
             fn bare(h: H) { let m = h; println(f\"  bare{m.xs.len()}:{m.name}\") }\n\
             fn noMove(t: (H, i64)) { match t { (r, k) => { println(f\"  nm{r.xs.len()}:{r.name}\") } } }\n\
             fn viaProj(s: S) { match s.t { (r, k) => { let m = r; println(f\"  p{m.xs.len()}:{m.name}\") } } }\n\
             fn main() {\n\
             \x20   whole((mk(1), 0));\n\
             \x20   withBody((mkD(2), 0));\n\
             \x20   mid((0, mk(3), 0));\n\
             \x20   bare(mk(4));\n\
             \x20   noMove((mk(5), 0));\n\
             \x20   viaProj(S { t: (mk(6), 0) });\n\
             \x20   println(\"end\")\n\
             }\n",
            &[
                "w1:n1",
                "  b1:n2",
                "  dD2:1:n2",
                "  m1:n3",
                "  bare1:n4",
                "  nm1:n5",
                "  p1:n6",
                "end",
            ],
            "b32-tuple-elem-whole-move-out",
        );
}

#[test]
fn asan_branch_value_owns_the_container_element_clone_it_hands_out() {
    assert_clean_asan_run(
        r#"
struct P { word: String, n: i64 }

fn mkp() -> Vec[P] {
    return [P { word: f"a{1}", n: 1 }, P { word: f"b{2}", n: 2 }];
}

fn use_it(s: String) -> i64 { return s.len(); }

fn pick_if(ps: ref Vec[P], c: bool) -> String {
    return if c { ps[0].word } else { ps[1].word };
}

fn pick_match(ps: ref Vec[P], c: bool) -> String {
    return match c { true => ps[0].word, false => ps[1].word };
}

fn main() {
    let p = mkp();
    let c = p[0].n == 1;

    println(if c { p[0].word } else { p[1].word });
    println(match c { true => p[0].word, false => p[1].word });

    println(f"{use_it(if c { p[0].word } else { p[1].word })}");
    println(f"{use_it(match c { true => p[0].word, false => p[1].word })}");

    let d = if c { p[0].word } else { p[1].word };
    println(d);
    let e = match c { true => p[0].word, false => p[1].word };
    println(e);

    let mut out: Vec[String] = Vec.new();
    out.push(if c { p[0].word } else { p[1].word });
    out.push(match c { true => p[0].word, false => p[1].word });
    println(out[0]);
    println(out[1]);

    println(pick_if(p, c));
    println(pick_match(p, c));

    if c { p[0].word } else { p[1].word };
    match c { true => p[0].word, false => p[1].word };

    println(p[0].word);
    println(p[1].word);
}
"#,
        &[
            "a1", "a1", "2", "2", "a1", "a1", "a1", "a1", "a1", "a1", "a1", "b2",
        ],
        "asan_branch_value_owns_the_container_element_clone_it_hands_out",
    );
}

#[test]
fn asan_branch_tail_owned_temp_frees_once() {
    assert_clean_asan_run(
        r#"
fn mk(n: i64) -> String {
    return f"p{n}-aaaaaaaaaaaaaaaaaaaaaaaa";
}

fn mkv(n: i64) -> Vec[i64] {
    return [n, n + 1, n + 2, n + 3];
}

fn use_it(s: String) -> i64 {
    return s.len();
}

fn use_v(v: Vec[i64]) -> i64 {
    return v.len();
}

fn pick(c: bool, n: i64) -> String {
    return if c { mk(n) } else { mk(n + 1) };
}

fn main() {
    let n: i64 = env.args().len();
    let c = n > 0;
    let d = n > 5;

    let a1 = if c { mk(n) } else { mk(n + 1) }.contains("p");
    println(f"a1={a1}");
    let a2 = match n { 1 => mk(n), _ => mk(n + 1) }.contains("p");
    println(f"a2={a2}");
    let a3 = { mk(n) }.contains("p");
    println(f"a3={a3}");
    let a4 = if c { mk(n) } else { mk(n + 1) }.len();
    println(f"a4={a4}");
    let a5 = { mkv(n) }.len();
    println(f"a5={a5}");
    let a6 = if c { mkv(n) } else { mkv(n + 1) }.len();
    println(f"a6={a6}");
    let a7 = { mk(n) }.to_string();
    println(f"a7={a7}");
    let a8 = if c { mk(n) } else { mk(n + 1) }.to_uppercase();
    println(f"a8={a8}");

    println(f"b1={use_it(if c { mk(n) } else { mk(n + 1) })}");
    println(f"b2={use_it(match n { 1 => mk(n), _ => mk(n + 1) })}");
    println(f"b3={use_it({ mk(n) })}");
    println(f"b4={use_v(if c { mkv(n) } else { mkv(n + 1) })}");

    println(f"c1={if c { mk(n) } else { mk(n + 1) }}");
    println(f"c2={{ mk(n) }}");

    let e1 = if c { { mk(n) } } else { mk(n + 1) }.contains("p");
    println(f"e1={e1}");
    let e2 = if d { mk(n) } else if c { mk(n + 1) } else { mk(n + 2) }.contains("p");
    println(f"e2={e2}");
    let e3 = match n { 1 => { mk(n) }, _ => { mk(n + 1) } }.contains("p");
    println(f"e3={e3}");
    let e4 = if d { mk(n) } else { mk(n + 7) }.contains("p8");
    println(f"e4={e4}");

    let f1 = if c { mk(n) } else { mk(n + 1) };
    println(f"f1={f1}");
    let f2 = { mk(n) };
    println(f"f2={f2}");
    let mut out: Vec[String] = Vec.new();
    out.push(if c { mk(n) } else { mk(n + 1) });
    out.push({ mk(n) });
    println(f"f3={out[0]}/{out[1]}");
    println(f"f4={pick(c, n)}");
    let mut f5 = if c { mk(n) } else { mk(n + 1) };
    f5 = { mk(n + 3) };
    println(f"f5={f5}");

    if c { mk(n) } else { mk(n + 1) };
    { mk(n) };

    for x in { mkv(n) } {
        println(f"h1={x}");
    }
    for x in if c { mkv(n) } else { mkv(n + 1) } {
        println(f"h2={x}");
    }
    let h3 = if c { mk(n) } else { mk(n + 1) } + "!";
    println(f"h3={h3}");

    for i in 0..3 {
        let g = if c { mk(n + i) } else { mk(n + i + 1) }.contains("p");
        println(f"g={g}");
    }
}
"#,
        &[
            "a1=true",
            "a2=true",
            "a3=true",
            "a4=27",
            "a5=4",
            "a6=4",
            "a7=p1-aaaaaaaaaaaaaaaaaaaaaaaa",
            "a8=P1-AAAAAAAAAAAAAAAAAAAAAAAA",
            "b1=27",
            "b2=27",
            "b3=27",
            "b4=4",
            "c1=p1-aaaaaaaaaaaaaaaaaaaaaaaa",
            "c2=p1-aaaaaaaaaaaaaaaaaaaaaaaa",
            "e1=true",
            "e2=true",
            "e3=true",
            "e4=true",
            "f1=p1-aaaaaaaaaaaaaaaaaaaaaaaa",
            "f2=p1-aaaaaaaaaaaaaaaaaaaaaaaa",
            "f3=p1-aaaaaaaaaaaaaaaaaaaaaaaa/p1-aaaaaaaaaaaaaaaaaaaaaaaa",
            "f4=p1-aaaaaaaaaaaaaaaaaaaaaaaa",
            "f5=p4-aaaaaaaaaaaaaaaaaaaaaaaa",
            "h1=1",
            "h1=2",
            "h1=3",
            "h1=4",
            "h2=1",
            "h2=2",
            "h2=3",
            "h2=4",
            "h3=p1-aaaaaaaaaaaaaaaaaaaaaaaa!",
            "g=true",
            "g=true",
            "g=true",
        ],
        "asan_branch_tail_owned_temp_frees_once",
    );
}

/// B-2026-09-05-29 — the MEMORY gate for the discarded generic whole-param
/// return. The E2E twin sees a body count; this one sees the 11 B in 2
/// blocks (a `String` and a `Vec` buffer) that valgrind measured lost per
/// evaluation pre-fix, unbounded in a loop — and, in the other direction,
/// it is what would catch the fix overshooting into a DOUBLE FREE, which
/// is the failure mode the row warned about: the registrar this feeds
/// takes ownership of what it names, so naming an object that already has
/// an owner is strictly worse than the leak it replaces. Three rounds so a
/// per-round imbalance accumulates rather than cancelling.
///
/// THE NAMED-LOCAL CELL WAS DELIBERATELY ABSENT, and B-2026-09-05-31 has
/// since closed it — see `asan_generic_whole_param_named_local_frees_the_-
/// entry_copy`, which owns that shape and every cell around it. What this
/// row recorded stands: `let g = mk(82); let _ = passG(g);` ran its body
/// exactly once on all four surfaces while leaking, so no body count could
/// see it. What that row corrected is the DIAGNOSIS — the miss is not on
/// the binding's registration, and the owner it needed was not a second
/// one over a single object. The callee ENTRY-COPIES, so there are two
/// objects; the binding freed its original and the returned copy was
/// orphaned. Admitting it is therefore not the double free feared here,
/// provided the callee actually copies, which is the gate that row adds.
#[test]
fn asan_generic_whole_param_discarded_temp_frees_once() {
    assert_clean_asan_run_min_allocs(
        r#"
struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] }; }
struct H { r: R, z: i64 }
fn passG[T](x: T) -> T { println("inP"); return x; }
fn passN(x: R) -> R { println("inN"); return x; }
fn pickB[T](a: T, b: T) -> T { println("inB"); return b; }
fn wrapG[T](x: T) -> H { println("inW"); return H { r: x, z: 1 }; }
fn scalarG[T](x: T) -> i64 { println("inS"); return 3; }
fn round() {
  let _ = passG(mk(80)); println("a");
  let _ = passN(mk(81)); println("b");
  let _ = pickB(mk(83), mk(84)); println("d");
  let _ = wrapG(mk(85)); println("e");
  let _ = scalarG(mk(86)); println("f");
  let k = passG(mk(87)); println(f"k{k.id}");
}
fn main() { let mut i = 0; while i < 3 { round(); i = i + 1; } println("done"); }
"#,
        &[
            "inP", "dR80", "a", "inN", "dR81", "b", "inB", "dR83", "dR84", "d", "inW", "dR85", "e",
            "inS", "dR86", "f", "inP", "k87", "dR87", "inP", "dR80", "a", "inN", "dR81", "b",
            "inB", "dR83", "dR84", "d", "inW", "dR85", "e", "inS", "dR86", "f", "inP", "k87",
            "dR87", "inP", "dR80", "a", "inN", "dR81", "b", "inB", "dR83", "dR84", "d", "inW",
            "dR85", "e", "inS", "dR86", "f", "inP", "k87", "dR87", "done",
        ],
        "b0905-29-generic-whole-param-discarded-temp",
        48,
    );
}

#[test]
/// B-2026-09-07-5 — the FREEING half of
/// `test_e2e_stored_argument_is_owned_by_its_new_home_not_the_caller`.
///
/// That test asserts the OUTPUT, and output is not what was broken here at
/// `-O2`: LLVM inlines the storing callee and the double free degrades into
/// a surviving use-after-free that prints every line correctly. `b`
/// (`self.one = r`) is the clearest instance — on the parent it printed
/// `b17 dR17` at `-O2` and aborted at `-O0`, from the same object being
/// freed twice either way. Only a sanitizer separates "prints the right
/// lines" from "owns its memory once".
///
/// Measured after the fix: 0 valgrind errors. On the parent: 35 errors from
/// 24 contexts, `Invalid free()` and `Invalid read of size 8` in pairs
/// across the method, free-function, assoc-fn and monomorph legs.
///
/// The whole E2E fixture is carried, `k`/`l` and `m`/`n` included, because
/// here they are not redundant controls: `k` is the conditional store's
/// NON-storing path, where a stand-down would strand the value with no
/// owner at all, and `m`/`n` are the copy-supported pair whose original the
/// callee's entry copy really does orphan — a leak in either would mean the
/// gate had over-fired, which no output assertion can see. Unlike
/// `asan_method_and_assoc_arg_registrars_admit_only_when_the_result_owns_it`
/// this fixture omits nothing: every cell in it is leak-free on the fix.
fn asan_stored_argument_is_owned_by_its_new_home_not_the_caller() {
    assert_clean_asan_run_min_allocs(
        r#"
shared struct Inner { v: i64 }
struct R { id: i64, name: String, inner: Inner }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}", inner: Inner { v: i } }; }

struct S { id: i64, name: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mks(i: i64) -> S { return S { id: i, name: f"s{i}" }; }

struct Box2 { mut xs: Vec[R] }
impl Box2 {
    fn push(mut ref self, r: R) { self.xs.push(r); }
    fn put2(mut ref self, a: R, c: R) { self.xs.push(a); self.xs.push(c); }
    fn puts(mut ref self, r: R) -> i64 { self.xs.push(r); return 5; }
    fn maybe(mut ref self, r: R, k: bool) { if k { self.xs.push(r); } }
    fn stash(b: mut ref Box2, r: R) { b.xs.push(r); }
}
struct Box3 { mut one: R }
impl Box3 { fn set(mut ref self, r: R) { self.one = r; } }
struct BoxS { mut xs: Vec[S] }
impl BoxS { fn add(mut ref self, s: S) { self.xs.push(s); } }

fn take(b: mut ref Box2, r: R) { b.xs.push(r); }
fn inner_push(b: mut ref Box2, r: R) { b.xs.push(r); }
fn outer_push(b: mut ref Box2, r: R) { inner_push(b, r); }
fn pushv(v: mut ref Vec[R], r: R) { v.push(r); }
fn stashg[T](v: mut ref Vec[T], x: T) { v.push(x); }
fn takes(b: mut ref BoxS, s: S) { b.xs.push(s); }

fn c_method() { let mut b = Box2 { xs: Vec.new() }; b.push(mk(16)); println(f"a{b.xs.len()}"); }
fn c_field()  { let mut b = Box3 { one: mk(1) }; b.set(mk(17)); println(f"b{b.one.id}"); }
fn c_free()   { let mut b = Box2 { xs: Vec.new() }; take(mut b, mk(18)); println(f"c{b.xs.len()}"); }
fn c_assoc()  { let mut b = Box2 { xs: Vec.new() }; Box2.stash(mut b, mk(19)); println(f"d{b.xs.len()}"); }
fn c_generic(){ let mut v: Vec[R] = Vec.new(); stashg(mut v, mk(20)); println(f"e{v.len()}"); }
fn c_vecref() { let mut v: Vec[R] = Vec.new(); pushv(mut v, mk(21)); println(f"f{v.len()}"); }
fn c_two()    { let mut b = Box2 { xs: Vec.new() }; b.put2(mk(22), mk(23)); println(f"g{b.xs.len()}"); }
fn c_ret()    { let mut b = Box2 { xs: Vec.new() }; let z = b.puts(mk(24)); println(f"h{z}{b.xs.len()}"); }
fn c_lit()    { let mut b = Box2 { xs: Vec.new() }; b.push(R { id: 25, name: "n", inner: Inner { v: 1 } }); println(f"i{b.xs.len()}"); }
fn c_viacall(){ let mut b = Box2 { xs: Vec.new() }; outer_push(mut b, mk(26)); println(f"j{b.xs.len()}"); }
fn c_cond_no(){ let mut b = Box2 { xs: Vec.new() }; b.maybe(mk(27), false); println(f"k{b.xs.len()}"); }
fn c_cond_yes(){ let mut b = Box2 { xs: Vec.new() }; b.maybe(mk(28), true); println(f"l{b.xs.len()}"); }
fn c_copy_m() { let mut b = BoxS { xs: Vec.new() }; b.add(mks(41)); println(f"m{b.xs.len()}"); }
fn c_copy_f() { let mut b = BoxS { xs: Vec.new() }; takes(mut b, mks(42)); println(f"n{b.xs.len()}"); }

fn main() {
    c_method(); c_field(); c_free(); c_assoc(); c_generic(); c_vecref();
    c_two(); c_ret(); c_lit(); c_viacall(); c_cond_no(); c_cond_yes();
    c_copy_m(); c_copy_f();
    println("end");
}
"#,
        &[
            "a1", "dR16", "dR1", "b17", "dR17", "c1", "dR18", "d1", "dR19", "e1", "dR20", "f1",
            "dR21", "g2", "dR22", "dR23", "h51", "dR24", "i1", "dR25", "j1", "dR26", "dR27", "k0",
            "l1", "dR28", "m1", "dS41", "n1", "dS42", "end",
        ],
        "b0907-5-outliving-store-admission",
        40,
    );
}

#[test]
/// B-2026-09-06-69 — a MIXED-PATH callee hands its by-value param back on
/// one exit and lets it die on another, and exactly one frame frees it
/// either way.
///
/// The param class here is the one whose prologue REFUSES to own it — a
/// struct with a `shared` field, or a self-referential one, neither
/// copy-supported nor eligible for the transfer bargain — so the callee
/// FORWARDS the caller's object instead of copying it. With the caller
/// owning the buffer statically, the hand-back exit had two owners (its own
/// temp and the result binding) and aborted `free(): double free detected
/// in tcache 2` under `karac run` and at both opt levels; standing the
/// caller down instead left the dies-inside exit with none and stranded
/// 18 B at `-O0`. Neither half is a fix alone, which is why the memory now
/// sits on the callee's per-path registration beside the `Drop` body and the
/// caller retracts.
///
/// `bare`/`d` is the cell that shows the defect was never about the rebind:
/// the no-rebind spelling reached the caller's stand-down through
/// `fn_returns_param`'s union all along, so its dies-inside exit leaked the
/// same 18 B on the PARENT — the row filed that spelling as "clean on every
/// surface" having measured only its hand-back exit.
///
/// Controls, each pinning one gate rather than decorating the fixture:
/// `copyok` is entry-copied, so the caller keeps its own object and nothing
/// here may move; `never` consumes the param without handing it back, so
/// the conditional predicate must decline it; `selfref` is the second
/// declined-copy class; `wrap` reaches the hand-back through an
/// `Option.Some` constructor rather than a bare tail.
///
/// DELIBERATELY OMITS the `wrap` cell the E2E fixture carries, for the
/// reason `asan_param_handed_back_through_a_rebind_leaves_one_owner` omits
/// its own `tp`/`op` pair: it goes from a double free to a 16-byte leak of
/// the `shared` handle's refcount block at `-O0`, and that residual is not
/// this fix's. Measured on the same tree: `fn op(r: R) -> Option[R] {
/// return Option.Some(r); }` — no rebind, no branch, nothing this row
/// touches — leaks the identical 16 bytes, which is B-2026-09-06-72. Its
/// output is asserted by the E2E twin; including it here would make this
/// fixture red for someone else's bug.
fn asan_conditional_handback_of_a_rebound_param_frees_once() {
    assert_clean_asan_run_min_allocs(
        r#"
shared struct Inner { v: i64 }
struct R { id: i64, name: String, inner: Inner }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}", inner: Inner { v: i } }; }
struct N { id: i64, name: String, next: Option[N] }
impl Drop for N { fn drop(mut ref self) { println(f"dN{self.id}") } }
fn mkn(i: i64) -> N { return N { id: i, name: f"h{i}", next: Option.None }; }
struct P { id: i64, name: String }
impl Drop for P { fn drop(mut ref self) { println(f"dP{self.id}") } }
fn mkP(i: i64) -> P { return P { id: i, name: f"p{i}" }; }

fn reb(r: R, c: bool) -> R { let m = r; if c { return m; } return mk(99); }
fn bare(r: R, c: bool) -> R { if c { return r; } return mk(98); }
fn arm(r: R, c: bool) -> R { let m = r; match c { true => m, false => mk(97) } }
fn two(r: R, c: bool) -> R { let m = r; let n = m; if c { return n; } return mk(96); }
fn rd(r: R, c: bool) -> R { let m = r; if c { return m; } println(f"in={m.name}"); return mk(95); }
fn selfref(n: N, c: bool) -> N { let m = n; if c { return m; } return mkn(94); }
fn copyok(p: P, c: bool) -> P { let m = p; if c { return m; } return mkP(93); }
fn never(r: R, c: bool) -> i64 { let m = r; if c { return m.id; } return 0; }

fn main() {
  let a = reb(mk(1), true);  println(f"a={a.inner.v}");
  let b = reb(mk(2), false); println(f"b={b.inner.v}");
  let c = bare(mk(3), true);  println(f"c={c.inner.v}");
  let d = bare(mk(4), false); println(f"d={d.inner.v}");
  let e = arm(mk(5), true);  println(f"e={e.inner.v}");
  let f = arm(mk(6), false); println(f"f={f.inner.v}");
  let g = two(mk(7), true);  println(f"g={g.inner.v}");
  let h = two(mk(8), false); println(f"h={h.inner.v}");
  let i2 = rd(mk(9), true);  println(f"i={i2.inner.v}");
  let j = rd(mk(10), false); println(f"j={j.inner.v}");
  let m2 = selfref(mkn(13), true);  println(f"m={m2.name}");
  let n2 = selfref(mkn(14), false); println(f"n={n2.name}");
  let o = copyok(mkP(15), true);  println(f"o={o.name}");
  let p2 = copyok(mkP(16), false); println(f"p={p2.name}");
  println(f"q={never(mk(17), true)}");
  println(f"r={never(mk(18), false)}");
  reb(mk(19), true);
  reb(mk(20), false);
  let mut z = 0;
  while z < 2 { let w = reb(mk(21), z == 0); println(f"w={w.inner.v}"); z = z + 1; }
  println("end");
}
"#,
        &[
            "a=1", "dR1", "dR2", "b=99", "dR99", "c=3", "dR3", "dR4", "d=98", "dR98", "e=5", "dR5",
            "dR6", "f=97", "dR97", "g=7", "dR7", "dR8", "h=96", "dR96", "i=9", "dR9", "in=h10",
            "dR10", "j=95", "dR95", "m=h13", "dN13", "dN14", "n=h94", "dN94", "o=p15", "dP15",
            "dP16", "p=p93", "dP93", "dR17", "q=17", "dR18", "r=0", "dR19", "dR20", "dR99", "w=21",
            "dR21", "dR21", "w=99", "dR99", "end",
        ],
        "b0906-69-conditional-handback",
        40,
    );
}

/// B-2026-08-30-3, second half — a DISCARDED value-block keeps its own
/// owner, and the two spellings of one discard agree.
///
/// A SEPARATE PRE-EXISTING LEAK, found while fixing the one above and kept
/// as its own fixture because the mechanism is a different one. Measured at
/// HEAD, before any part of that fix: `{ f"b{n}" };` lost 2 B per
/// evaluation, and so did the same value one and two wrappers deeper. It is
/// recorded that way deliberately — an earlier draft of this fixture called
/// it a regression the widening introduced, on a pre/post comparison whose
/// "pre" compiler could not find the runtime archive and so reported a
/// FAILED BUILD as a clean run.
///
/// `compile_block_with_frame` computes `consumer_frees` from "does my tail
/// mint?", which is really "will a use-site gate free this?" — a question
/// that presupposes a use site. A discarded block has none, so the frame
/// must keep its own owner however the tail is spelled. It stayed hidden
/// because every OTHER tail shape reaching that question is also owned by
/// the statement site (`stmt_owns_block_tail`), which suppresses the
/// discard leg entirely; an f-string tail is the first owned by neither.
///
/// Two gaps, both measured, both fixed here:
///
/// - the bare `{ .. };` statement did not record a value-position block by
///   its own span, while `let _ = { .. };` did — so one spelling of the
///   same discard was clean and the other leaked 2 B;
/// - the recorder took only the OUTERMOST block, so `{ { .. } };` leaked at
///   one wrapper deeper than `{ .. };`. Discard is inherited through a
///   single-tail wrapper exactly as it is through a branch.
///
/// The `mk*` and literal rows are the tripwires that matter: a discarded
/// block with a CALL tail is owned by the statement site, so it must stay
/// on that path and NOT acquire a second owner here — the failure mode is a
/// double free, which is why this asserts a clean ASAN run rather than only
/// the absence of leaks.
#[test]
fn asan_discarded_value_block_keeps_its_owner() {
    assert_clean_asan_run(
        r#"
fn mkS(n: i64) -> String { return f"S{n}-ssssssssssss"; }

fn main() {
    let n: i64 = env.args().len();
    let c = n > 0;

    { f"b{n}" };
    { { f"c{n}" } };
    { { { f"e{n}" } } };
    let _ = { f"d{n}" };
    let _ = { { f"g{n}" } };

    { mkS(n) };
    { { mkS(n) } };
    let _ = mkS(n);
    { if c { mkS(n) } else { mkS(n + 1) } };
    { { if c { f"h{n}" } else { f"i{n}" } } };
    { "lit" };

    println(f"done {n}");
}
"#,
        &["done 1"],
        "asan_discarded_value_block_keeps_its_owner",
    );
}

/// B-2026-09-02-11 — the memory half of "withhold the payload's body, keep
/// the copy", one level in from the row above.
///
/// The fix routes an indexed-element clone's payload binding from the
/// `karac_drop_<T>` wrapper (body + field cleanup) to the memory-only
/// `track_struct_var` channel, exactly as the owned-param caller-retains
/// case already did. The wrapper was freeing the clone's buffers, so the
/// failure mode the change could have introduced is a leak of precisely
/// those — invisible to any output comparison, and what LSan is here for.
///
/// The `match` runs three times over the same element, so the clone is made
/// and destroyed three times while the container keeps owning the original.
/// A `Vec[String]` payload puts real element buffers behind each clone: a
/// lost memory registration leaks six of them, and a double registration
/// frees the container's out from under it.
#[test]
fn asan_indexed_element_clone_frees_once_without_rerunning_the_body() {
    let Some((out, status)) = run_under_asan(
        r#"struct R { id: i64, v: Vec[String] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
enum E { A(R), B }

fn mk(n: i64) -> E {
    let mut v: Vec[String] = Vec.new();
    v.push("abcdefghijklmnopqrstuvwxyz0123456789");
    v.push(f"tag{n}");
    return E.A(R { id: n, v: v })
}

fn main() {
    let mut xs: Vec[E] = Vec.new();
    xs.push(mk(7));
    let mut i = 0;
    while i < 3 {
        match xs[0] { E.A(r) => { println(f"n{r.id} {r.v.len()}"); } E.B => { } }
        i = i + 1;
    }
    println("end");
}
"#,
        "asan_indexed_element_clone_frees_once_without_rerunning_the_body",
    ) else {
        return;
    };
    assert!(status.success(), "ASAN/LSan reported a problem:\n{out}");
    // One `dR7` — the container's, at its own NLL death. Four would be the
    // pre-fix count (one per clone plus the container's).
    assert_eq!(
        out.matches("dR7").count(),
        1,
        "unexpected Drop body count:\n{out}"
    );
    assert_eq!(
        out.matches("n7 2").count(),
        3,
        "the clone did not carry a live payload on every pass:\n{out}"
    );
}

/// B-2026-08-28-36 — a container-element tuple-index read whose leaf is a
/// whole heap-carrying STRUCT frees its clone exactly once, at every
/// position.
///
/// B-2026-08-28-24 made `v[0].0` deep-clone instead of aliasing, which
/// closed the double free. It registered cleanup for the clone only when
/// the projected member is a `{ptr,len,cap}`; a whole-STRUCT member was
/// cloned and then owned by nobody, so any read that no destination adopted
/// leaked it — 4 bytes under LSan for `peek(v[0].0)` through a `ref` param.
///
/// THE TWO ROWS HERE ARE A PAIR AND MUST STAY THAT WAY, because the naive
/// repairs move the bug rather than fix it. Registering nothing (the
/// pre-fix state) leaks `ref-param`. Registering the clone's cleanup
/// without handing it over double-frees `let-bound`, since the binding
/// frees it too — measured, not hypothesized. So the clone keeps its
/// `track_struct_var` AND every consuming destination neutralizes that
/// registration: return / call-arg / block-tail through
/// `suppress_source_vec_cleanup_for_arg_ex`, and a `let` through
/// `take_over_container_elem_struct_clone`.
///
/// Neither failure is visible in stdout — all three variants print the same
/// thing — which is why this is a sanitizer fixture and has no behavioural
/// twin.
#[test]
fn asan_container_elem_struct_leaf_clone_is_owned_once() {
    const H: &str = "struct R { id: i64, name: String }\n";
    // NON-consuming: handed to a `ref` param, so nothing adopts the clone
    // and its own registration has to free it.
    assert_clean_asan_run(
        &format!(
            "{H}fn peek(r: ref R) -> i64 {{ r.id }}\n\
             fn take(v: Vec[(R, i64)]) -> i64 {{ peek(v[0].0) }}\n\
             fn main() {{ let q = [(R {{ id: 41, name: f\"n{{41}}\" }}, 1)];\n\
             \x20            println(f\"{{take(q)}}\"); }}\n"
        ),
        &["41"],
        "ref-param",
    );
    // Consuming `let`: the binding adopts the clone, so the clone's own
    // registration must be neutralized or both free it.
    assert_clean_asan_run(
        &format!(
            "{H}fn take(v: Vec[(R, i64)]) -> i64 {{ let r = v[0].0; r.id }}\n\
             fn main() {{ let q = [(R {{ id: 41, name: f\"n{{41}}\" }}, 1)];\n\
             \x20            println(f\"{{take(q)}}\"); }}\n"
        ),
        &["41"],
        "let-bound",
    );
    // Consuming return — B-2026-08-28-24's own shape, which must stay clean.
    assert_clean_asan_run(
        &format!(
            "{H}fn take(v: Vec[(R, i64)]) -> R {{ v[0].0 }}\n\
             fn main() {{ let q = [(R {{ id: 41, name: f\"n{{41}}\" }}, 1)];\n\
             \x20            let x = take(q); println(f\"{{x.id}} {{x.name}}\"); }}\n"
        ),
        &["41 n41"],
        "return-position",
    );
}

/// B-2026-08-28-57 — the memory half of running a fixed `Array[T, N]`'s
/// element `Drop` bodies.
///
/// `__karac_dropelems_array_*` is bodies-only by construction: it calls
/// `emit_slot_drop_bodies_at` per element and frees nothing, while the
/// array's storage stays owned by the scope-exit `__karac_drop_array_te_*`
/// registered on the same binding. Two actions over one slot is exactly the
/// arrangement that double-frees if the bodies leg also freed, and leaks if
/// adding it displaced the memory one — so every row carries a heap
/// `String` the newly-running body READS.
///
/// `moved-on` is the row that matters most: the rebind fallback registers
/// the walk for the DESTINATION after the move disarms the source. Register
/// it per-owner rather than per-value and both fire over the same elements.
/// B-2026-09-19-3 / B-2026-09-19-7 — a container-typed struct field that
/// receives a MOVE OUT OF A NAMED LOCAL, one row per container shape.
///
/// This is the spelling nothing in the tree exercised. Every array-field
/// cell in `tests/codegen.rs` built the value as a LITERAL into the field,
/// which has no source binding to leave a second owner behind, so the
/// family read as covered while the moved-from-local form had two owners:
/// the local's `StructDrop` and the struct's field walk. `Array[D, 2]` and
/// `Array[Array[D, 1], 2]` double-freed, and `Array[Vec[D], 1]` SEGV'd at
/// the DEFAULT `-O2`.
///
/// The `-O0` leg is what makes these rows mean anything: at `-O2` the
/// optimizer ELIDED the duplicate free for the two `Array`-outer shapes, so
/// they printed correctly and exited 0 while the emitted code was wrong.
/// That is CLAUDE.md's "an `-O2`-only zero is evidence of nothing", and it
/// is why this belongs here and not only in the codegen suite.
///
/// The two `Vec`-outer rows are CONTROLS — always clean, because
/// `suppress_source_vec_cleanup_for_arg` covers them. They are what makes
/// the `Array` rows evidence about the OUTER type rather than about moving.
#[test]
fn asan_container_struct_field_moved_from_a_local_is_memory_balanced() {
    const H: &str = "struct D { id: i64, s: String }\n\
             impl Drop for D { fn drop(mut ref self) { println(f\"dD{self.id}\") } }\n\
             fn mkd(n: i64) -> D { return D { id: n, s: f\"ss{n}\" }; }\n";
    for (ty, init, label) in [
        ("Array[D, 2]", "[mkd(1), mkd(2)]", "array-of-struct"),
        (
            "Array[Array[D, 1], 2]",
            "[[mkd(1)], [mkd(2)]]",
            "array-of-array",
        ),
        ("Array[Vec[D], 1]", "[[mkd(1), mkd(2)]]", "array-of-vec"),
        // Controls — the `Vec`-outer twins, clean before the fix too.
        ("Vec[D]", "[mkd(1), mkd(2)]", "control-vec-of-struct"),
        (
            "Vec[Array[D, 1]]",
            "[[mkd(1)], [mkd(2)]]",
            "control-vec-of-array",
        ),
    ] {
        assert_clean_asan_run(
            &format!(
                "{H}struct W {{ f: {ty} }}\n\
                     fn main() {{ let a: {ty} = {init};\n\
                     \x20            let h = W {{ f: a }};\n\
                     \x20            println(\"end\"); }}\n"
            ),
            &["dD1", "dD2", "end"],
            label,
        );
    }
    // The DISCARDED-aggregate spelling stays clean the other way: `W { f: a };`
    // as a statement takes nothing over, so the source keeps its owner and a
    // widening of the retraction would LEAK here instead. This row is the one
    // that fails if the `in_discarded_aggregate_tail` gate at the call site is
    // ever dropped.
    assert_clean_asan_run(
        &format!(
            "{H}struct W {{ f: Array[D, 2] }}\n\
                 fn main() {{ let a: Array[D, 2] = [mkd(1), mkd(2)];\n\
                 \x20            W {{ f: a }};\n\
                 \x20            println(\"end\"); }}\n"
        ),
        &["dD1", "dD2", "end"],
        "discarded-aggregate-tail",
    );
}

/// B-2026-08-28-10 — moving a place-source leaf's `Drop` body to the leaf
/// keeps its heap owned exactly once.
///
/// The body half was an ordering divergence AND a wrong-value one: the body
/// ran on the source copy `bind_pattern` had moved out of, printing
/// `drop 41 ` where the interpreter printed `drop 41 n41`. This fixture is
/// the half that decides whether moving it is safe, and the answer took
/// three measured corrections, each a different failure:
///
///   * body moved, memory left with the source -> USE-AFTER-FREE, the body
///     reading a buffer the source had already freed at the destructure;
///   * body and memory moved via `zero_struct_field_move_cap` -> still a
///     DOUBLE FREE, because that helper reaches only DIRECT
///     Vec/String/Map/Option fields and a nested STRUCT field's own heap
///     survived it;
///   * both moved correctly, but the `callee_owned_src` arm further down
///     the same loop then registered its OWN memory drop on the leaf ->
///     DOUBLE FREE again, visible only in the emitted IR as
///     `__karac_drop_struct_R` beside `karac_drop_R`.
///
/// `field-only-drop-leaf` is the fourth: a leaf with no wrapper to take
/// keeps the chain's memory owner, so its body must be registered AFTER
/// that owner or it prints from freed memory.
#[test]
fn asan_place_source_destructure_leaf_is_owned_once() {
    const H: &str = "struct R { id: i64, name: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id} {self.name}\") } }\n\
             struct W { r: R, n: i64 }\n";
    assert_clean_asan_run(
        &format!(
            "{H}fn main() {{ let w = W {{ r: R {{ id: 41, name: f\"n{{41}}\" }}, n: 1 }};\n\
             \x20            let W {{ r, n }} = w; println(f\"{{r.name}} {{n}}\") }}\n"
        ),
        &["n41 1", "drop 41 n41"],
        "place-source",
    );
    assert_clean_asan_run(
            &format!(
                "{H}fn take(r: R) -> i64 {{ r.id }}\n\
             \x20            fn main() {{ let w = W {{ r: R {{ id: 41, name: f\"n{{41}}\" }}, n: 1 }};\n\
             \x20            let W {{ r, n }} = w; println(f\"{{take(r) + n}}\") }}\n"
            ),
            &["42", "drop 41 n41"],
            "consumed-leaf",
        );
    // TWO heap-carrying leaves out of one source.
    assert_clean_asan_run(
            "struct R { id: i64, name: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id} {self.name}\") } }\n\
             struct V { a: R, b: R }\n\
             fn main() { let v = V { a: R { id: 41, name: f\"n{41}\" },\n\
             \x20                    b: R { id: 42, name: f\"m{42}\" } };\n\
             \x20            let V { a, b } = v; println(f\"{a.name} {b.name}\") }\n",
            &["n41 m42", "drop 42 m42", "drop 41 n41"],
            "two-heap-leaves",
        );
    // THE MODEL — the field-access spelling of the same move, clean before
    // this and after it.
    assert_clean_asan_run(
        &format!(
            "{H}fn main() {{ let w = W {{ r: R {{ id: 41, name: f\"n{{41}}\" }}, n: 1 }};\n\
             \x20            let x = w.r; println(f\"{{x.name}} {{w.n}}\") }}\n"
        ),
        &["n41 1", "drop 41 n41"],
        "field-access-model",
    );
    // The no-wrapper leaf, whose body is registered AFTER the chain's
    // memory owner rather than instead of it.
    assert_clean_asan_run(
            "struct Res { id: i64, tag: String }\n\
             impl Drop for Res { fn drop(mut ref self) { println(f\"drop res {self.id} {self.tag}\") } }\n\
             struct R { res: Res }\n\
             struct W { r: R, n: i64 }\n\
             fn main() { let w = W { r: R { res: Res { id: 41, tag: f\"t{1}\" } }, n: 1 };\n\
             \x20            let W { r, n } = w; println(f\"{r.res.id + n}\") }\n",
            &["42", "drop res 41 t1"],
            "field-only-drop-leaf",
        );
}

#[test]
fn asan_weak_field_cycle_freed_no_leak() {
    // B-2026-07-19-8: `weak T` fields. A reference CYCLE that plain RC cannot
    // collect (a.random -> b, b.random -> a) is freed clean because the weak
    // back-edges don't contribute to the strong count. Reads upgrade to
    // `Option[T]` (`Some` while the target is strong-alive). Must print both
    // values (b.val=20 via a.random, a.val=10 via b.random) and be LSan-clean —
    // the whole point of weak refs.
    assert_clean_asan_run(
        r#"
shared struct Node { mut val: i64, mut random: weak Node }
fn main() {
    let a: Node = Node { val: 10i64, random: None };
    let b: Node = Node { val: 20i64, random: None };
    a.random = b;
    b.random = a;
    match a.random { Some(r) => { println(r.val.to_string()); } None => { println("none"); } }
    match b.random { Some(r) => { println(r.val.to_string()); } None => { println("none"); } }
}
"#,
        &["20", "10"],
        "asan_weak_field_cycle_freed_no_leak",
    );
}

#[test]
fn asan_weak_field_indexed_deep_copy_no_leak() {
    // B-2026-07-19-8 + the indexed-read drift it surfaced (same class as
    // B-2026-07-19-6, read side): a `Vec[Node]` deep-copy where `Node` has a
    // `weak` field. Indexed reads (`orig[i].val`) and indexed weak store/read
    // (`copies[i].random = copies[r.id]`) must route through `shared_gep_layout`
    // (base 2 for the weak-headered box) — a hardcoded `idx + 1` read the weak
    // count as the value. Deep-copies a 3-node list with random back-edges and
    // prints `val|random_val`; run == build, LSan-clean.
    assert_clean_asan_run(
        r#"
shared struct Node { val: i64, id: i64, mut next: Option[Node], mut random: weak Node }
fn deep_copy(orig: Vec[Node]) -> Vec[Node] {
    let n: i64 = orig.len();
    let mut copies: Vec[Node] = Vec.new();
    let mut i: i64 = 0i64;
    while i < n {
        copies.push(Node { val: orig[i].val, id: orig[i].id, next: None, random: None });
        i = i + 1;
    }
    i = 0i64;
    while i < n {
        if i + 1i64 < n { copies[i].next = Some(copies[i + 1i64]); }
        match orig[i].random { Some(r) => { copies[i].random = copies[r.id]; } None => {} }
        i = i + 1;
    }
    return copies;
}
fn main() {
    let mut orig: Vec[Node] = Vec.new();
    orig.push(Node { val: 7i64, id: 0i64, next: None, random: None });
    orig.push(Node { val: 13i64, id: 1i64, next: None, random: None });
    orig.push(Node { val: 11i64, id: 2i64, next: None, random: None });
    orig[0i64].next = Some(orig[1i64]);
    orig[1i64].next = Some(orig[2i64]);
    orig[1i64].random = orig[0i64];
    orig[2i64].random = orig[1i64];
    let copies: Vec[Node] = deep_copy(orig);
    let mut i: i64 = 0i64;
    while i < 3i64 {
        let rv: i64 = match copies[i].random { Some(r) => r.val, None => 0i64 - 1i64 };
        println(copies[i].val.to_string() + "|" + rv.to_string());
        i = i + 1;
    }
}
"#,
        &["7|-1", "13|7", "11|13"],
        "asan_weak_field_indexed_deep_copy_no_leak",
    );
}

#[test]
fn asan_interner_local_binding_freed_no_leak() {
    // Phase-8 Interner codegen: a local `Interner` binding's scope-exit
    // `FreeInternerHandle` must reclaim the runtime interner + every
    // stored byte string — a missed `karac_runtime_interner_free` leaks
    // the table per iteration (LSan on Linux CI catches it). 40 fresh
    // interners, each interning two distinct strings + one dedup hit and
    // resolving one back (the borrowed `cap = 0` view must NOT be freed
    // by the caller — a double-free here is the other failure mode).
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0i64;
    let mut total: i64 = 0i64;
    while i < 40i64 {
        let mut tab: Interner = Interner.new();
        let a = tab.intern("alpha");
        let _b = tab.intern("beta");
        let a2 = tab.intern("alpha");
        if a == a2 {
            total = total + tab.resolve(a).len();
        }
        total = total + tab.len();
        i = i + 1;
    }
    println(total.to_string());
}
"#,
        // 40 * (len("alpha") + 2 distinct) = 40 * 7 = 280
        &["280"],
        "interner_local_binding_freed_no_leak",
    );
}

#[test]
fn asan_interner_fresh_temp_intern_arg_no_leak() {
    // `intern(p + "pha")` — the fresh concat temp's buffer is orphaned
    // after the runtime copies the bytes, so the intern lowering must
    // materialize it for scope-exit free. One leak per iteration without
    // the materialization.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0i64;
    let mut hits: i64 = 0i64;
    while i < 40i64 {
        let mut tab: Interner = Interner.new();
        let a = tab.intern("alpha");
        let p = "al";
        let b = tab.intern(p + "pha");
        if a == b {
            hits = hits + 1i64;
        }
        i = i + 1;
    }
    println(hits.to_string());
}
"#,
        &["40"],
        "interner_fresh_temp_intern_arg_no_leak",
    );
}

#[test]
fn asan_arena_local_binding_freed_no_leak() {
    // Phase-8 Arena codegen: a local `Arena[T]` binding's scope-exit
    // `FreeArenaHandle` must reclaim the runtime arena + every stored
    // blob — a missed `karac_runtime_arena_free` leaks the table per
    // iteration (LSan on Linux CI catches it). 40 fresh arenas mixing
    // i64 pushes, a String push (arena-owned copy), a `get` read-back
    // (the borrowed `cap = 0` String view must NOT be freed by the
    // caller — double-free is the other failure mode), and a rewind
    // (truncation drops are runtime-owned).
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0i64;
    let mut total: i64 = 0i64;
    while i < 40i64 {
        let a: Arena[i64] = Arena.new();
        let r0 = a.push(7i64);
        let cp = a.high_water_mark();
        let _r1 = a.push(9i64);
        a.rewind_to(cp);
        total = total + a.get(r0) + a.len();
        let s: Arena[String] = Arena.new();
        let sr = s.push("alpha");
        total = total + s.get(sr).len();
        i = i + 1;
    }
    println(total.to_string());
}
"#,
        // 40 * (7 + 1 + 5) = 520
        &["520"],
        "arena_local_binding_freed_no_leak",
    );
}

#[test]
fn asan_generic_struct_method_bare_type_param_temp_arg_no_leak() {
    // B-2026-08-11-3: a generic struct's method whose parameter is the bare
    // TYPE PARAMETER (`fn push(mut ref self, v: T)`) has its arg
    // deep-copied at every retaining consume site in the mono body
    // (`owned_vecstr_params`, B-2026-07-11-35), so the CALLER keeps
    // ownership of the argument. A fresh TEMPORARY argument therefore had
    // no owner anywhere and leaked one element buffer per call — 50
    // iterations leaked 800 bytes in 50 blocks before the fix, while the
    // program printed the right answer on all three backends.
    //
    // Both argument forms are exercised on purpose: the temporary is the
    // defect, and the NAMED binding next to it is the double-free control —
    // its own let-drop already owns the buffer, so a caller-side free that
    // fires for it too would abort under ASAN.
    assert_clean_asan_run(
        r#"
struct Stack[T] { items: Vec[T] }
impl[T] Stack[T] {
    fn new() -> Stack[T] { Stack { items: Vec.new() } }
    fn push(mut ref self, v: T) { self.items.push(v); }
    fn len(ref self) -> i64 { self.items.len() }
}
fn main() {
    let mut temps: Stack[Vec[i64]] = Stack.new();
    let mut named: Stack[Vec[i64]] = Stack.new();
    let mut i: i64 = 0i64;
    while i < 50i64 {
        temps.push([i, i + 1i64, i + 2i64]);
        let v: Vec[i64] = [i, i + 1i64];
        named.push(v);
        i = i + 1;
    }
    println((temps.len() + named.len()).to_string());
}
"#,
        &["100"],
        "generic_struct_method_bare_type_param_temp_arg_no_leak",
    );
}

#[test]
fn asan_backpressure_primitives_no_leak() {
    // `Semaphore` / `RateLimiter` each own a heap `Karac*` object behind
    // their `handle_id`; the synthesized single-owner Drop must free it at
    // scope exit. Loop so LSan catches a per-iteration leak of either
    // runtime object (or the per-key bucket map the RateLimiter grows).
    // The RateLimiter key is a fresh f-string String each round, exercising
    // the borrowed-key path (runtime copies the bytes; the arg temp frees).
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0i64;
    let mut total: i64 = 0i64;
    while i < 40i64 {
        let sem = Semaphore.new(2);
        match sem.acquire(0i64) { Ok(u) => { total = total + 1i64; }, Err(e) => {} }
        match sem.acquire(0i64) { Ok(u) => { total = total + 1i64; }, Err(e) => {} }
        match sem.acquire(0i64) { Ok(u) => { total = total + 1i64; }, Err(e) => {} }
        sem.release();
        let rl = RateLimiter.new_token_bucket(1i64, 2i64);
        let key = f"client-{i}";
        if rl.try_acquire(key) { total = total + 10i64; }
        i = i + 1;
    }
    println(total.to_string());
}
"#,
        // per iter: 2 acquires ok (+2), 1 timeout, try_acquire ok (+10) = 12; * 40 = 480
        &["480"],
        "backpressure_primitives_no_leak",
    );
}

#[test]
fn asan_process_stdin_write_close_no_leak() {
    // `ChildStdin.write` borrows the String argument (runtime reads the
    // descriptor only); the argument temp and the `cat` output String
    // must both free exactly once per iteration.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0i64;
    let mut total: i64 = 0i64;
    while i < 8i64 {
        let cat = Command.new("cat").stdin(Stdio.Piped).stdout(Stdio.Piped);
        match cat.spawn() {
            Ok(child) => {
                match child.stdin() {
                    Some(sin) => {
                        match sin.write("pipe-payload\n") { Ok(u) => {}, Err(e) => {} }
                        match sin.close() { Ok(u) => {}, Err(e) => {} }
                    }
                    None => {}
                }
                match child.stdout() {
                    Some(o) => {
                        match o.read_to_string() {
                            Ok(text) => { total = total + text.len(); }
                            Err(e) => {}
                        }
                    }
                    None => {}
                }
                match child.wait() { Ok(st) => {}, Err(e) => {} }
            }
            Err(e) => {}
        }
        i = i + 1;
    }
    println(total.to_string());
}
"#,
        // "pipe-payload\n".len() == 13; 13 * 8 = 104
        &["104"],
        "process_stdin_write_close_no_leak",
    );
}

#[test]
fn asan_paired_stack_same_tree_no_leak_or_uaf() {
    // B-2026-07-12-4 source (kata #100 iterative paired-stack solver): two
    // `Vec[Option[shared]]` worklists, `stack.push(node.left/right)` field
    // pushes, and an early `false` return that leaves node-pairs resident on
    // the stacks — the exact residual + field-push shape that both UAF'd
    // (residual drop) and leaked (the drained pairs). Trees differ at the
    // right child, so it returns early with residuals still on the stacks.
    assert_clean_asan_run(
        r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn same(p: Option[Node], q: Option[Node]) -> bool {
    let mut sp: Vec[Option[Node]] = Vec.new();
    let mut sq: Vec[Option[Node]] = Vec.new();
    sp.push(p);
    sq.push(q);
    let mut result = true;
    let mut go = true;
    while go {
        if sp.len() == 0 { go = false; }
        else {
            match sp.pop() {
                Some(a) => { match sq.pop() {
                    Some(b) => { match a {
                        Some(an) => { match b {
                            Some(bn) => {
                                if an.val != bn.val { result = false; go = false; }
                                else {
                                    sp.push(an.left); sq.push(bn.left);
                                    sp.push(an.right); sq.push(bn.right);
                                }
                            }
                            None => { result = false; go = false; }
                        } }
                        None => { match b { Some(bn) => { result = false; go = false; } None => {} } }
                    } }
                    None => { go = false; }
                } }
                None => { go = false; }
            }
        }
    }
    result
}
fn main() {
    let t1 = Some(Node { val: 1, left: Some(Node { val: 2, left: None, right: None }), right: Some(Node { val: 3, left: None, right: None }) });
    let t2 = Some(Node { val: 1, left: Some(Node { val: 2, left: None, right: None }), right: Some(Node { val: 9, left: None, right: None }) });
    if same(t1, t2) { println("equal"); } else { println("different"); }
}
"#,
        &["different"],
        "paired_stack_same_tree_no_leak_or_uaf",
    );
}

#[test]
fn asan_bytecode_vm_example_no_leak_or_double_free() {
    // examples/vm.kara under ASAN — the bytecode VM churns three Vecs
    // (Vec[Op] program, Vec[i64] data stack, Vec[i64] locals + call stack)
    // hard across the enum-dispatch loop, with per-program construction and
    // drop. `Op` is POD (i64 payloads only), so this is a buffer-lifecycle
    // check: every Vec allocated by the four `prog_*` builders and by `run`
    // is freed exactly once, no leak, across construct -> execute -> drop.
    assert_clean_asan_run(
        include_str!("../../examples/vm.kara"),
        &["20", "15", "120", "42"],
        "asan_bytecode_vm_example_no_leak_or_double_free",
    );
}

#[test]
fn asan_pipeline_example_no_leak() {
    // examples/pipeline.kara under ASAN — the log-analytics pipeline runs
    // `iter()` chains of `map`/`filter`/`fold`/`collect` over `Req` records
    // whose `method`/`path` are heap `String`s. The `fold` terminal desugar
    // (B-2026-07-11-17) inlines the chain into a `for` loop over the base
    // source; this asserts that lowering leaks no source Vec, no per-element
    // String, and no collected `Vec[String]` (the slow-paths materialization)
    // across construct -> iterate -> aggregate -> drop.
    assert_clean_asan_run(
        include_str!("../../examples/pipeline.kara"),
        &[
            "requests: 10",
            "ok: 7",
            "server_err: 2",
            "client_err: 1",
            "bytes_served: 18432",
            "max_latency_ms: 210",
            "avg_ok_latency_ms: 50",
            "slow_paths: 3",
            "  /api/orders",
            "  /api/users",
            "  /assets/app",
        ],
        "asan_pipeline_example_no_leak",
    );
}

#[test]
fn asan_b05_1_diff_param_name_no_double_free() {
    assert_clean_asan_run(
        r#"
fn main() {
    let mut round: i64 = 0i64;
    while round < 30i64 {
        let mut v: Vec[String] = Vec[
            "b05-diff-alpha-aaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            "b05-diff-bravo-bbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string()
        ];
        let r: Vec[(i64, String)] = v.iter().enumerate().filter(|p| p.0 >= 0i64).inspect(|q| print(q.0)).collect();
        println(f"{r.len()} {r[1i64].0} {v.len()}");
        round = round + 1i64;
    }
}
"#,
        ["012 1 2"].repeat(30).as_slice(),
        "asan_b05_1_diff_param_name_no_double_free",
    );
}

#[test]
fn asan_b05_1_multistage_diff_params_no_double_free() {
    assert_clean_asan_run(
        r#"
fn main() {
    let mut round: i64 = 0i64;
    while round < 30i64 {
        let mut v: Vec[String] = Vec[
            "b05-multi-alpha-aaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            "b05-multi-bravo-bbbbbbbbbbbbbbbbbbbbbbbbbb".to_string(),
            "b05-multi-charlie-cccccccccccccccccccccccc".to_string()
        ];
        let r: Vec[(i64, String)] = v.iter().enumerate().filter(|p| p.0 >= 0i64).filter(|q| q.0 < 5i64).collect();
        println(f"{r.len()} {r[2i64].0} {v.len()}");
        round = round + 1i64;
    }
}
"#,
        ["3 2 3"].repeat(30).as_slice(),
        "asan_b05_1_multistage_diff_params_no_double_free",
    );
}

// ── tuple-destructure leaf cleanup (B-2026-06-13-5) ───────────
//
// `let (a, b) = pair()` extracts each element into a fresh leaf alloca.
// Pre-fix the leaves got NO scope-exit free, so a String/Vec element's
// heap buffer leaked once per destructure (2000 leaks / 46 KB over a
// 1000-iter loop). `finish_owned_tuple_destructure` now frees each
// heap-owning leaf. The Linux-CI LSan job is the leak gate; this run also
// guards against the move-out double-free risk the fix introduces — a leaf
// RETURNED from a fn (`first`, moved out of the destructure) and a leaf
// produced by a NON-fresh destructure (`let (c, d) = t`, a move of an
// existing tuple binding the source frees) must each be freed exactly once.
// Looping a few hundred times makes any double-free / UAF trip ASAN.
#[test]
fn asan_tuple_destructure_leaf_cleanup_no_leak_no_double_free() {
    assert_clean_asan_run(
        r#"
fn pair(n: i64) -> (String, String) { (f"L{n}", f"R{n}") }
fn first(n: i64) -> String { let (a, _) = pair(n); a }
fn main() {
    let (a, b) = pair(1);     // both leaves owned + freed (the reported leak)
    println(a);
    println(b);
    println(first(2));        // returned leaf — moved out, must not double-free
    let t = pair(3);
    let (c, d) = t;           // non-fresh destructure — source frees, not c/d
    println(c);
    println(d);
    let (e, _) = pair(4);     // wildcard-discarded element also freed
    println(e);
}
"#,
        &["L1", "R1", "L2", "L3", "R3", "L4"],
        "tuple_destructure_leaf_cleanup",
    );
}

// ── Block-construct call argument owns its temp (no leak) ──
//
// B-2026-06-11-5 (residual of B-2026-06-11-2): a block passed DIRECTLY as
// a call argument (`take({ f"…" })`) had its tail acc suppressed by
// `suppress_block_tail_cleanup` so a binding/return consumer could own it —
// but a bare call argument has no owning consumer, so the temp orphaned and
// leaked (a DIRECT `take(f"…")` is caller-owned and clean). Fix:
// `materialize_owned_temp` the block-arg value into the caller scope, the
// same caller ownership a direct f-string arg gets. The loop builds a fresh
// heap String/Vec each iteration in argument position; on Linux LSan a
// leaked temp trips, and a double-free / UAF (if the temp were both
// materialized AND owned elsewhere) trips macOS ASAN too.
#[test]
fn asan_block_arg_temp_owned_no_leak() {
    assert_clean_asan_run(
        r#"
fn take_s(s: String) { if s.len() > 99999 { println(s); } }
fn take_v(v: Vec[i64]) { if v.len() > 99999 { println(v.len()); } }
fn two(a: String, b: String) { if a.len() > 99999 { println(a); println(b); } }
fn main() {
    let mut i: i64 = 0;
    while i < 5 {
        take_s({ f"arg{i}-{i}" });
        take_s({ let p = "x" + "y"; p });
        take_v({ Vec[i, i, i] });
        two({ f"p{i}" }, { f"q{i}" });
        take_s(f"direct{i}");
        i = i + 1;
    }
    println("done");
}
"#,
        &["done"],
        "block_arg_temp_owned",
    );
}

#[test]
fn asan_operand_temp_named_binding_not_double_freed() {
    // Slice 3c negative / double-free guard: a NAMED String binding used as
    // a binop operand (`s + " [suffix]"`) must NOT be freed by the
    // operand-temp path — `s` is an `Identifier` (not a fresh-temp shape),
    // so it owns its buffer and frees it at iteration-scope exit. If the
    // operand-free wrongly fired on `s`, the binding's own free would
    // double-free it each iteration (macOS ASAN). The concat result `r` is
    // freed by its own binding.
    assert_clean_asan_run(
        r#"
fn make_s() -> String {
    let s: String = "a freshly allocated heap operand string over thirty-six bytes";
    return s;
}

fn main() {
    let mut i = 0;
    while i < 3 {
        let s = make_s();
        let r = s + " [suffix]";
        println(r);
        i = i + 1;
    };
}
"#,
        &[
            "a freshly allocated heap operand string over thirty-six bytes [suffix]",
            "a freshly allocated heap operand string over thirty-six bytes [suffix]",
            "a freshly allocated heap operand string over thirty-six bytes [suffix]",
        ],
        "operand_temp_named_binding_not_double_freed",
    );
}

#[test]
fn asan_freshtemp_user_method_owned_self_no_double_free() {
    // Slice 3j companion: an OWNED-`self` user method on a fresh-temp struct
    // receiver (`make_counter().consume()`), looped. Owned `self` moves the
    // receiver into the method, which drops its `Vec[String]` field at method
    // scope exit — so the fresh-temp path must NOT drop-track the caller's
    // shallow copy, else the field Vec + Strings are freed twice (macOS ASAN).
    // Conversely the method's own drop must fire, else they leak (Linux LSan).
    // The looped re-materialization accumulates either fault.
    assert_clean_asan_run(
        r#"
struct Counter { items: Vec[String], base: i64 }
impl Counter {
    fn consume(self) -> i64 { return self.base + self.items.len(); }
}
fn make_counter() -> Counter {
    let mut c = Counter { items: Vec.new(), base: 100_i64 };
    c.items.push("first field string padded beyond thirty-six bytes ok");
    c.items.push("second field string padded beyond thirty-six byte");
    return c;
}
fn main() {
    let mut p = 0;
    while p < 3 {
        println(make_counter().consume());
        p = p + 1;
    };
}
"#,
        &["102", "102", "102"],
        "freshtemp_user_method_owned_self_no_double_free",
    );
}

/// B-2026-08-05-27 — the BYTE-READING half of the surface-concat receiver.
/// `("p:".to_string() + s).starts_with(..)` leaked the concat once per
/// evaluation: the fresh-temp receiver materialization gate
/// (`try_compile_string_recv_temp_method`) is built on
/// `expr_yields_fresh_owned_temp`, which matches Call/MethodCall only, so a
/// concat the `String.add` desugar skipped was declined. The Call-receiver
/// twin (`mk().starts_with(..)`) was already clean, which localized it to
/// the predicate rather than to the free.
///
/// UNLIKE the sibling above, this one is a DEFAULT-BUILD leak and pins at
/// -O2: measured 8200 B over 200 iterations against the unfixed compiler,
/// identical at -O0. The sibling consumes its concat through `.len()`,
/// which touches the length word and never the bytes, so at -O2 the buffer
/// is a dead allocation LLVM deletes (B-2026-08-04-17) and the same class
/// reads clean there. `starts_with`/`contains` read the BYTES, so the
/// allocation is live and the leak is visible on the surface users build.
///
/// Written to B-2026-08-04-17's three rules — `env.args().len()` seed,
/// runtime-derived payload content, byte-level read — and floored, so a
/// regression to zero allocations fails loudly instead of passing.
#[test]
fn asan_surface_concat_byte_read_receiver_no_leak() {
    assert_clean_asan_run_min_allocs(
        r#"
fn main() {
    let n = env.args().len() as i64;
    let mut v: Vec[String] = Vec.new();
    let mut b: String = String.new();
    b.push_str("elem-");
    b.push_str(n.to_string());
    b.push_str("-padding-to-force-heap");
    v.push(b);
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 200 {
        match v.first() {
            Some(s) => {
                if ("p:".to_string() + s).starts_with("p:elem") { acc = acc + 1; }
                if ("q:".to_string() + s).contains("elem") { acc = acc + 1; }
            }
            None => { }
        }
        i = i + 1;
    }
    println(acc);
}
"#,
        &["400"],
        "surface_concat_byte_read_receiver",
        100,
    );
}

/// B-2026-08-05-28 — the CHAINED xform on a surface-concat receiver:
/// `("p:" + s).to_uppercase().len()` / `.trim().len()`. Until the
/// span-collision fix these did not COMPILE at all (the dispatcher's own
/// "this is a codegen bug" fall-through), which is why the sibling
/// `asan_surface_concat_byte_read_receiver_no_leak` above covers only
/// `starts_with` / `contains`.
///
/// It gets its own test rather than a line in that one because the
/// ownership half here had NEVER EXECUTED: the fresh-temp receiver free
/// has a dedicated branch treating the String→String xform family as
/// allocating an independent copy (so the receiver is safe to free
/// behind), and that branch was unreachable for a Binary receiver while
/// the call could not be compiled. Compiling it is what makes the free
/// path live, so it needs its own leak evidence, not an assumption.
///
/// TWO buffers per iteration are at stake, which is the shape worth
/// pinning: the concat receiver AND the fresh xform result. Written to
/// B-2026-08-04-17's rules — `env.args()` seed, runtime-derived payload,
/// and `.len()` on the xform RESULT reads the length word, so the result
/// buffer stays live at -O2 — and floored so a regression to zero
/// allocations fails loudly rather than passing vacuously.
#[test]
fn asan_surface_concat_chained_xform_receiver_no_leak() {
    assert_clean_asan_run_min_allocs(
        r#"
fn main() {
    let n = env.args().len() as i64;
    let mut v: Vec[String] = Vec.new();
    let mut b: String = String.new();
    b.push_str("elem-");
    b.push_str(n.to_string());
    b.push_str("-padding-to-force-heap");
    v.push(b);
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 200 {
        match v.first() {
            Some(s) => {
                acc = acc + ("p:".to_string() + s).to_uppercase().len();
                acc = acc + ("q:".to_string() + s).trim().len();
            }
            None => { }
        }
        i = i + 1;
    }
    println(acc);
}
"#,
        // 28-char payload + 2-char prefix = 30 per xform, two per
        // iteration, 200 iterations.
        &["12000"],
        "surface_concat_chained_xform_receiver",
        100,
    );
}

/// B-2026-08-05-33 predicate (a) — a GENERIC wrapper passed by value at a
/// concrete argument, `fn sink(b: Box[String])` for `struct Box[T] { v: T }`.
///
/// Same law as the (b) sibling above and the same own-by-transfer fix, but
/// it stayed live one commit longer for a reason worth pinning: the
/// ownership decision was already right, and the DROP SYNTHESIS emitted
/// nothing. Keyed by bare name it reads the declared `v: T`, classifies the
/// erased field as no-heap, and returns `None` — no `__karac_drop_struct_Box`
/// is defined at all, so `track_struct_var` registered nothing and the
/// (correct) transfer silently freed nothing. The concrete binding lives in
/// the param's own declared type, which a CONCRETE fn has no active
/// monomorph subst to supply; threading it makes the mono drop real.
///
/// The paired double-free risk is `sink` MOVING the field out
/// (`asan_generic_wrapper_accessor_and_move_no_double_free`, B-2026-07-15-11)
/// — that test now runs against a param drop that actually fires, so the two
/// must be read together.
///
/// Floored per B-2026-08-04-17: runtime-derived payload, read through
/// `len()` on the callee side so the entry is not a dead allocation at -O2.
#[test]
fn asan_generic_wrapper_by_value_param_no_leak() {
    assert_clean_asan_run_min_allocs(
        r#"
struct Box[T] { v: T }

fn sink(b: Box[String]) -> i64 { b.v.len() }

fn main() {
    let n = env.args().len() as i64;
    let mut acc: i64 = 0;
    let mut i: i64 = 0;
    while i < 40 {
        let mut s: String = String.new();
        s.push_str("payload-");
        s.push_str(n.to_string());
        s.push_str("-padded-out-to-force-heap");
        let b = Box { v: s };
        acc = acc + sink(b);
        i = i + 1;
    }
    println(acc);
}
"#,
        // "payload-" (8) + "1" (1) + "-padded-out-to-force-heap" (25) = 34,
        // x 40 iterations.
        &["1360"],
        "generic_wrapper_by_value_param",
        100,
    );
}

#[test]
fn asan_freshtemp_field_access_no_leak_no_double_free() {
    // B-2026-07-22-2 memory leg. Leak half: a fresh call-result struct
    // temp read in expression position must drop its aggregate — the
    // read field after a borrow consumer, AND the unread remainder
    // (mk2().s leaves .t). Double-free half: move consumers (let /
    // assign / return / tail / consuming match arms) own the accessed
    // field exactly once against the temp's registered drop. NOTE: x86
    // -O2 folds several of these leaks invisible; the arm64 LSan leg is
    // the real guard for this class (B-2026-07-12-29 precedent). Loop
    // so any per-iteration imbalance accumulates.
    assert_clean_asan_run(
        r#"
struct W { s: String }
struct W2 { s: String, t: String }
struct Wv { v: Vec[i64] }
struct H { opt: Option[String], n: i64 }
enum E { A(String) }
fn mk() -> W { return W { s: "one".to_string() }; }
fn mk2() -> W2 { return W2 { s: "aa".to_string(), t: "bb".to_string() }; }
fn mkv() -> Wv { return Wv { v: [1, 2, 3] }; }
fn mko() -> H { return H { opt: Some("op".to_string()), n: 5 }; }
fn take(x: String) -> i64 { return x.len(); }
fn get() -> String { return mk().s; }
fn get2() -> String { mk().s }
fn f(w: W) -> W { let g = || w; g() }
fn fv(w: Wv) -> Wv { let g = || w; g() }
fn fe(e: E) -> E { let g = || e; g() }
fn main() {
    let mut i: i64 = 0;
    let mut acc2: i64 = 0;
    while i < 40 {
        acc2 = acc2 + mk().s.len();
        acc2 = acc2 + take(mk().s);
        acc2 = acc2 + mkv().v.len();
        acc2 = acc2 + mko().n;
        let s = mk2().s;
        acc2 = acc2 + s.len();
        let mut acc = "seed".to_string();
        acc = mk().s;
        acc2 = acc2 + acc.len();
        acc2 = acc2 + get().len() + get2().len();
        match mko().opt {
            Some(p) => { acc2 = acc2 + p.len(); }
            None => { }
        }
        if let Some(q) = mko().opt {
            acc2 = acc2 + q.len();
        }
        acc2 = acc2 + f(W { s: "one".to_string() }).s.len();
        acc2 = acc2 + fv(Wv { v: [1, 2, 3] }).v.len();
        match fe(E.A("four".to_string())) { E.A(t) => { acc2 = acc2 + t.len(); } }
        i = i + 1;
    }
    println(acc2);
}
"#,
        &["1560"],
        "freshtemp_field_access",
    );
}

#[test]
#[ignore = "B-2026-08-05-35 sweep: does not compile — chained field receivers (`a.b.c…`) are deferred to v1.x. Silently SKIPPED until the harness learned to fail on a codegen error."]
fn asan_byvalue_aggregate_param_transferred_out_no_double_free() {
    // #14 (phase-12 self-hosting): an owned by-value aggregate (struct OR
    // enum) param moved into a call that transfers it OUT (into the callee's
    // return value) used to double-free — the caller's source binding and
    // the returned value aliased the same heap buffer and BOTH freed it.
    // The param is now entry-deep-copied + callee-owned (param_own.rs), so
    // each owns an independent buffer. Covers, under a loop (per-iteration
    // single-free):
    //   * direct enum return (`wrap(e) -> E { e }`),
    //   * struct consumed into a returned struct literal (`Wrap { t: t }`),
    //   * the lexer's bootstrap shape — an enum param wrapped into a returned
    //     struct then destructured (`make_spanned(token)`),
    //   * read-then-reuse of the source (`take(x); take(x)`) — entry-copy
    //     keeps the caller's binding live (the reason the fix is entry-copy,
    //     not a caller-side move).
    assert_clean_asan_run(
        r#"
enum E { A(String), N(i64) }
struct Inner { s: String }
struct Wrap { t: Inner }
struct Spanned { tok: E, off: i64 }
fn wrap_enum(e: E) -> E { e }
fn wrap_struct(t: Inner) -> Wrap { Wrap { t: t } }
fn make_spanned(t: E, o: i64) -> Spanned { Spanned { tok: t, off: o } }
fn read_struct(v: Inner) { if v.s.len() > 99999 { println(v.s); } }
fn main() {
    let mut i: i64 = 0;
    while i < 4 {
        let f = E.A(f"a-{i}");
        let g = wrap_enum(f);
        match g { A(s) => { if s.len() > 99999 { println(s); } } N(n) => println(n.to_string()) }

        let x = Inner { s: f"x-{i}" };
        let w = wrap_struct(x);
        if w.t.s.len() > 99999 { println(w.t.s); }

        let t = E.A(f"t-{i}");
        let sp = make_spanned(t, i);
        match sp.tok { A(name) => { if name.len() > 99999 { println(name); } } N(n) => println(n.to_string()) }

        let y = Inner { s: f"y-{i}" };
        read_struct(y);
        read_struct(y);

        i = i + 1;
    }
    println("done");
}
"#,
        &["done"],
        "byvalue_aggregate_param_transferred_out_no_double_free",
    );
}

#[test]
fn asan_tuple_elem_bind_move_out_no_double_free() {
    // #27 (phase-12 self-hosting, B-2026-06-14-8) — binding a heap-bearing
    // value OUT of a tuple element double-freed at scope exit: the binding's
    // drop AND the owning struct's `NestedTuple` tuple drop both freed the
    // shared buffer. `let inr = h.ps.0` (heap struct moved out — suppressed via
    // `suppress_tuple_index_move_source` → `zero_struct_move_caps`) and
    // `let tk = h.ps.0.tok` (enum field moved out through the tuple element —
    // suppressed via `suppress_place_field_enum_move_source` → place-chain GEP +
    // `zero_enum_payload_caps`). Loop-stressed over drop-only, field-read, and
    // match-consume of both forms; f-string payloads keep the heap non-foldable
    // so Linux LSan catches an over-suppression leak (if the source cap-zero
    // also orphaned a still-owned buffer) and ASAN catches the double-free.
    assert_clean_asan_run(
        r#"
enum Tok { Id(String), Num(i64) }
struct Inner { tok: Tok, n: i64 }
struct Hs { ps: (Inner, i64) }
fn main() {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 8 {
        // Struct element moved out, dropped unused (the headline double-free).
        let ha = Hs { ps: (Inner { tok: Tok.Id(f"a{i}"), n: i }, 7) };
        let inr = ha.ps.0;

        // Struct element moved out, enum field CONSUMED via match.
        let hb = Hs { ps: (Inner { tok: Tok.Id(f"b{i}"), n: i }, 7) };
        let inr2 = hb.ps.0;
        match inr2.tok { Id(s) => { acc = acc + s.len(); } Num(n) => { acc = acc + n; } }

        // Enum field moved out THROUGH the tuple element, dropped unused.
        let hc = Hs { ps: (Inner { tok: Tok.Id(f"c{i}"), n: i }, 7) };
        let tk = hc.ps.0.tok;

        // Enum field moved out through the tuple element, CONSUMED.
        let hd = Hs { ps: (Inner { tok: Tok.Id(f"d{i}"), n: i }, 7) };
        let tk2 = hd.ps.0.tok;
        match tk2 { Id(s) => { acc = acc + s.len(); } Num(n) => { acc = acc + n; } }

        i = i + 1;
    }
    if acc > 999999 { println("never"); }
    println("done");
}
"#,
        &["done"],
        "tuple_elem_bind_move_out_no_double_free",
    );
}

#[test]
fn asan_early_return_move_out_no_double_free() {
    // `return v` where `v` is a tracked Vec — the cleanup-on-return
    // path must apply move-aware suppression (zero the source's cap)
    // before draining so the caller's scope cleanup is the unique
    // owner of the buffer. Mirrors the function-end tail-return
    // suppress mechanism but for explicit `return expr`.
    assert_clean_asan_run(
        r#"
fn maybe_take(flag: bool) -> Vec[i64] {
    let mut v: Vec[i64] = Vec.new();
    v.push(42i64);
    if flag {
        return v;
    }
    v
}
fn main() {
    let v1 = maybe_take(true);
    let v2 = maybe_take(false);
    println(v1[0]);
    println(v2[0]);
}
"#,
        &["42", "42"],
        "early_return_move_out_no_double_free",
    );
}

// ── B-2026-06-10-2: moving a heap field OUT of a by-value struct PARAM ──
// The param is a shallow copy whose field buffer aliases the caller's; the
// moved-out local is deep-copied so it owns an independent buffer (the
// caller's struct-drop frees the original exactly once). The repro above
// (`asan_struct_with_vec_field_returned_no_double_free`) is the base case;
// these pin the reuse / String-field / field-return shapes.

#[test]
fn asan_struct_param_field_move_reuse_no_double_free() {
    // Passing the same struct by value TWICE — each `first_elem(h)`
    // deep-copies `h.v`, so each callee frees its own copy and `main` frees
    // the original once. Pre-fix this double-freed (exit 134).
    assert_clean_asan_run(
        r#"
struct Holder { v: Vec[i64] }
fn build() -> Holder { let mut inner: Vec[i64] = Vec.new(); inner.push(7i64); let h: Holder = Holder { v: inner }; h }
fn first_elem(h: Holder) -> i64 { let inner = h.v; inner[0] }
fn main() { let h = build(); let a = first_elem(h); let b = first_elem(h); println(a + b); }
"#,
        &["14"],
        "struct_param_field_move_reuse",
    );
}

#[test]
fn asan_self_field_move_out_tail_return_no_double_free() {
    // B-2026-07-18-39: a by-value-`self` method returning a heap field as its
    // tail (`fn get(self) -> String { self.v }`) — `self.v` is
    // `FieldAccess { object: SelfValue }`, which the tail-return field-move-out
    // suppression missed (it only matched `Identifier`), so `self`'s
    // callee-owned StructDrop freed the moved buffer the caller now owns.
    assert_clean_asan_run(
        r#"
struct B { v: String, n: i64 }
impl B { fn get(self) -> String { self.v } }
fn main() { let b = B { v: "hi".to_string(), n: 5 }; println(b.get()); }
"#,
        &["hi"],
        "self_field_move_out_tail_return",
    );
}

#[test]
fn asan_concrete_generic_struct_field_move_out_no_double_free() {
    // B-2026-08-06-2 (A). `fn take(b: Box[String]) -> String { b.v }` — the
    // CONCRETE spelling over a generic struct. The callee extracted the
    // field, then called the struct drop with the field's cap still SET, so
    // the drop freed the very buffer being returned and the caller freed it
    // again: a double free on a DEFAULT -O2 build, in both argument forms.
    //
    // The generic monomorph of the same source was already safe — it emits
    // the move-out cap-zero — because the gate resolved its GEP type from
    // the ACTIVE MONOMORPH SUBST. A concrete fn has no subst, fell back to
    // the erased generic base, and the `held == st` comparison then failed.
    //
    // Both argument forms are exercised: the fresh struct LITERAL and a
    // named local. The literal form's separate LEAK is defect (B) of the
    // same row, pinned next door by
    // `asan_generic_struct_literal_arg_entry_copy_no_leak`; this fixture
    // pins the double free.
    assert_clean_asan_run(
        r#"
struct Box[T] { v: T }
struct Two[T] { v: T, n: i64 }
fn take(b: Box[String]) -> String { b.v }
fn take2(b: Two[String]) -> String { b.v }
fn main() {
    // Runtime-derived payload: a constant-folded one is a dead allocation the
    // optimizer deletes, and the fixture then passes vacuously against the
    // unfixed compiler (B-2026-08-04-17 — this pin was written that way first
    // and had to be corrected).
    let n: i64 = env.args().len();
    let mut i: i64 = 0;
    while i < 4 {
        let bx = Box { v: "concrete-payload-past-inline-width".repeat(n) };
        println(take(bx).len());
        let tw = Two { v: "second-payload-past-inline-width".repeat(n), n: i };
        println(take2(tw).len());
        i = i + 1;
    }
}
"#,
        &["34", "32", "34", "32", "34", "32", "34", "32"],
        "concrete_generic_struct_field_move_out",
    );
}

#[test]
fn asan_struct_param_field_returned_no_double_free() {
    // The moved-out field is RETURNED to the caller: the deep-copy is the
    // returned value (caller owns it), the param's original field is freed
    // by the outer `main`'s struct-drop — two independent buffers.
    assert_clean_asan_run(
        r#"
struct Holder { v: Vec[i64] }
fn build() -> Holder { let mut inner: Vec[i64] = Vec.new(); inner.push(9i64); let h: Holder = Holder { v: inner }; h }
fn takev(h: Holder) -> Vec[i64] { let inner = h.v; inner }
fn main() { let h = build(); let v = takev(h); println(v[0]); }
"#,
        &["9"],
        "struct_param_field_returned",
    );
}

/// B-2026-08-13-3 — a NESTED heap field moved out of an owned by-value
/// aggregate param no longer double-frees.
///
/// `fn take(d: Deep) -> String { d.inner.word }` aborted with `free():
/// double free detected in tcache 2` on BOTH compiled backends while the
/// interpreter printed the string. The param is callee-owned (entry-copied
/// at the top of the frame), so moving a heap field out has to zero that
/// field's `cap` in the source or the param's struct drop frees what the
/// caller was just handed. The zeroing resolved its owner by matching the
/// receiver against an `Identifier`, so `d.word` was covered and
/// `d.inner.word` — one hop further — was not.
///
/// THE ONE-LEVEL TWIN HAS ALWAYS WORKED, which is what made this a gap in
/// REACH rather than a disagreement about ownership, and it is pinned here
/// beside the nested form so a future refactor cannot fix one and lose the
/// other.
///
/// THE SHAPES ARE THE FOUR CONSUMING POSITIONS a moved-out field can land
/// in, because the suppression is shared by all of them and each reaches it
/// by a different route: a bare tail RETURN, a struct LITERAL field, a call
/// ARGUMENT, and a `let` binding. The `let` form was already clean before
/// the fix — its own site handles the chain — so it is the control that
/// says the fix did not double up on a position that already worked.
///
/// The `es` leg carries the nesting one level deeper (`Outer { mid: Deep {
/// inner: Pair { word } } }`), since the walk is a loop and a single hop
/// would not distinguish "walks the chain" from "handles exactly two".
#[test]
fn asan_nested_field_move_out_of_owned_param_freed_once() {
    assert_clean_asan_run(
        r#"
struct Pair { word: String, n: i64 }
struct Deep { inner: Pair, tag: i64 }
struct Outer { mid: Deep, label: String }
fn ret(d: Deep) -> String { d.inner.word }
fn lit(d: Deep) -> Deep { Deep { inner: Pair { word: d.inner.word, n: d.inner.n + 1 }, tag: d.tag } }
fn sink(s: String) -> String { s + "!" }
fn arg(d: Deep) -> String { sink(d.inner.word) }
fn bound(d: Deep) -> String { let s = d.inner.word; s }
fn deeper(o: Outer) -> String { o.mid.inner.word }
fn one_level(p: Pair) -> String { p.word }
fn main() {
    let k = env.args().len() as i64;
    println(ret(Deep { inner: Pair { word: f"a{k}", n: 1 }, tag: 2 }));
    let grown = lit(Deep { inner: Pair { word: f"b{k}", n: 3 }, tag: 4 });
    println(grown.inner.word);
    println(arg(Deep { inner: Pair { word: f"c{k}", n: 5 }, tag: 6 }));
    println(bound(Deep { inner: Pair { word: f"d{k}", n: 7 }, tag: 8 }));
    println(deeper(Outer { mid: Deep { inner: Pair { word: f"e{k}", n: 9 }, tag: 10 }, label: f"L{k}" }));
    println(one_level(Pair { word: f"f{k}", n: 11 }));
}
"#,
        &["a1", "b1", "c1!", "d1", "e1", "f1"],
        "nested_field_move_out_of_owned_param_freed_once",
    );
}

/// B-2026-08-01-31 — `let x = o.h.r`: a struct field moved out of a
/// struct reached through a DEEPER field chain never cap-zeroed the
/// source, so BOTH x's cleanup and o's StructDrop freed the same name
/// buffer (double-free abort; `karac check`-clean — the depth-1
/// `let x = h.r` shape was fine). The deeper-place suppressor
/// (`suppress_place_field_struct_move_source`) now zeroes the moved
/// field in place; ASAN guards the free pairing. The Vec/String field
/// sibling (`let s = o.h.name`) rides the same suppressor.
#[test]
fn asan_deep_chain_field_move_out_no_double_free() {
    assert_clean_asan_run(
        r#"
struct R6 { id: i64, name: String }
struct H6 { r: R6 }
struct O6 { h: H6 }
fn main() {
    let o = O6 { h: H6 { r: R6 { id: 9, name: f"z{9}" } } };
    let x = o.h.r;
    println(f"x {x.name}");
    let o2 = O6 { h: H6 { r: R6 { id: 7, name: f"q{7}" } } };
    let s = o2.h.r.name;
    println(f"s {s}");
    println("end");
}
"#,
        &["x z9", "s q7", "end"],
        "deep_chain_field_move_out_no_double_free",
    );
}

#[test]
fn asan_nested_call_temp_owned_arg_freed() {
    // B-2026-08-02-28 — the leak half. A call result consumed directly as
    // another call's owned argument had no memory owner at all: the fn-call
    // arm of the owned-arg registrar registered the bodies walk and
    // returned, so the temp's Vec buffer (32 bytes) plus its element String
    // (3 indirect) leaked once per call while the body printed normally —
    // a leak that prints correct output, invisible to any parity or
    // fire-count check. Includes the `println(use_it(...))` nesting, which
    // is the shape the row was filed from.
    assert_clean_asan_run(
        r#"
struct Res { id: i64, name: String }
impl Drop for Res {
    fn drop(mut ref self) { println(f"drop {self.id} {self.name}") }
}
struct Holder { xs: Vec[Res], tag: i64 }
fn mk(v: Vec[Res]) -> Holder { Holder { xs: v, tag: 9 } }
fn use_it(h: Holder) -> i64 { h.tag }
fn main() {
    println("bound:");
    {
        let mut xs: Vec[Res] = Vec.new();
        xs.push(Res { id: 1, name: f"nn{1}" });
        let n = use_it(mk(xs));
        println(n);
    }
    println("discard:");
    {
        let mut ys: Vec[Res] = Vec.new();
        ys.push(Res { id: 2, name: f"oo{2}" });
        use_it(mk(ys));
    }
    println("end");
}
"#,
        &["bound:", "drop 1 nn1", "9", "discard:", "drop 2 oo2", "end"],
        "nested_call_temp_owned_arg_freed",
    );
}

#[test]
fn asan_passthrough_arg_returned_no_double_free() {
    // B-2026-08-02-23 leg 2 — retracting the caller's arg-site walk when
    // the callee returns that arg must not orphan the buffer: the result
    // binding has to become the sole owner, not neither owner. This is the
    // leak-side guard on the fix (the fire-count side is asserted in
    // tests/codegen.rs). It is also why the retraction uses the
    // CONTAINER-ONLY disarm — the strong form additionally dropped the
    // binding's `karac_drop_<T>` wrapper, which frees memory, and leaked
    // the entry-copied original.
    assert_clean_asan_run(
        r#"
struct Res { id: i64, name: String }
impl Drop for Res {
    fn drop(mut ref self) { println(f"drop {self.id} {self.name}") }
}
struct Holder { xs: Vec[Res], tag: i64 }
fn passthru(v: Vec[Res]) -> Vec[Res] { v }
fn mk(v: Vec[Res]) -> Holder { Holder { xs: v, tag: 9 } }
fn main() {
    println("bare:");
    {
        let mut xs: Vec[Res] = Vec.new();
        xs.push(Res { id: 1, name: f"pp{1}" });
        let ys = passthru(xs);
        println(ys.len());
    }
    println("literal:");
    {
        let mut zs: Vec[Res] = Vec.new();
        zs.push(Res { id: 2, name: f"qq{2}" });
        let h = mk(zs);
        println(h.tag);
    }
    println("end");
}
"#,
        &[
            "bare:",
            "1",
            "drop 1 pp1",
            "literal:",
            "9",
            "drop 2 qq2",
            "end",
        ],
        "passthrough_arg_returned_no_double_free",
    );
}

/// B-2026-08-01-34 — a GENERIC-monomorph field moved out of a deep
/// chain (`let g = o.h.b` with `b: Boxy[String]`) still double-freed
/// after the -31 fix: the deeper-place suppressor declined generic
/// field types outright. On a NON-generic parent the declared field
/// TypeExpr is already the concrete instantiation, so the suppressor
/// now derives the per-monomorph subst from it and zeroes through
/// `zero_struct_move_caps_mono` (generic parents keep declining —
/// bare-param field TEs, the B-2026-07-15-24 base-layout caution).
/// Both a String and a WIDER Vec[i64] instantiation, so a
/// layout-widening arg exercises the mono GEPs.
#[test]
fn asan_deep_chain_generic_mono_field_move_no_double_free() {
    assert_clean_asan_run(
        r#"
struct Boxy[T] { v: T }
struct Hg { b: Boxy[String] }
struct Og { h: Hg }
struct Hgv { b: Boxy[Vec[i64]] }
struct Ogv { h: Hgv }
fn main() {
    let o = Og { h: Hg { b: Boxy { v: f"s{7}" } } };
    let g = o.h.b;
    println(f"g {g.v}");
    let mut bx = Boxy { v: Vec.new() };
    bx.v.push(5);
    bx.v.push(6);
    let o2 = Ogv { h: Hgv { b: bx } };
    let g2 = o2.h.b;
    println(f"g0 {g2.v[0]} g1 {g2.v[1]}");
    println("end");
}
"#,
        &["g s7", "g0 5 g1 6", "end"],
        "deep_chain_generic_mono_field_move_no_double_free",
    );
}

#[test]
fn asan_method_chain_field_receiver_no_double_free() {
    // Double-free guard for slice 3's gate: a field-access receiver
    // (`h.items.len()`) reloads the buffer `h` owns — the receiver path
    // must NOT free it (only `h`'s scope-exit cleanup does). Looping the
    // read keeps `h` alive; a wrongful receiver-temp free would fault
    // under macOS ASAN (and `h`'s own free at scope exit would be the
    // second). Exercises the `expr_yields_fresh_owned_temp` exclusion.
    assert_clean_asan_run(
        r#"
struct Holder { items: Vec[i64] }

fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(10_i64);
    v.push(20_i64);
    let h = Holder { items: v };
    let mut i = 0;
    while i < 8 {
        println(h.items.len());
        i = i + 1;
    }
}
"#,
        &["2", "2", "2", "2", "2", "2", "2", "2"],
        "method_chain_field_receiver_no_double_free",
    );
}

// ── general owned-temp tracking, slice 5 (phase-6 line 497) ──
//
// docs/spikes/general-owned-temp-tracking.md slice 5 closes the
// *tail-expr temp leak*: a fresh owned temp produced in the tail of a
// *discarded* block (`{ make() }` in statement position, or
// `let _ = { make() };`) is the block's return value — its own frame
// drops only the block-local lets, so the escaping tail temp was never
// freed. `discarded_owned_temp_tail` peels the single-tail block wrapper
// and routes the tail through the owned-temp chokepoint. On Linux the
// unfreed buffer is the LeakSanitizer oracle; on macOS (no LSan) the
// repeated discard in a loop is a *double-free* gate — a tail temp freed
// against a buffer some other cleanup also owns would fault under ASAN.

#[test]
fn asan_discarded_block_tail_temp_freed() {
    // `{ make_vv() }` in statement position. `Vec[String]` so the
    // *nested* element buffers must also free (the element TypeExpr flows
    // from `owned_temp_drops` keyed on the peeled tail call's span); an
    // elem_ty: None regression would leak the inner Strings on Linux.
    // 8-iteration loop amplifies any per-iteration imbalance into a
    // deterministic macOS fault.
    assert_clean_asan_run(
        r#"
fn make_vv() -> Vec[String] {
    let mut v: Vec[String] = Vec.new();
    v.push("alpha");
    v.push("beta");
    return v;
}

fn main() {
    let mut i = 0;
    while i < 8 {
        { make_vv() }
        i = i + 1;
    }
    println(i);
}
"#,
        &["8"],
        "discarded_block_tail_temp_freed",
    );
}

#[test]
fn asan_discarded_block_tail_temp_with_block_local_no_double_free() {
    // The block carries BOTH a heap-local `let` (`local`, dropped by the
    // block's own frame at block exit) AND a fresh-owned tail temp
    // (`make_vv()`, dropped by the discard arm's one-shot frame). Each
    // must free exactly once — a regression that materialized the tail
    // against the block-local's slot, or double-counted, would double-free
    // under macOS ASAN. (Drop *order* — tail before local — is a slice-6
    // observation concern and not asserted here; this pins leak/UAF
    // cleanliness only.)
    assert_clean_asan_run(
        r#"
fn make_vv() -> Vec[String] {
    let mut v: Vec[String] = Vec.new();
    v.push("x");
    return v;
}

fn main() {
    let mut i = 0;
    while i < 8 {
        {
            let mut local: Vec[String] = Vec.new();
            local.push("y");
            println(local.len());
            make_vv()
        }
        i = i + 1;
    }
    println(i);
}
"#,
        &["1", "1", "1", "1", "1", "1", "1", "1", "8"],
        "discarded_block_tail_temp_with_block_local_no_double_free",
    );
}

#[test]
fn asan_struct_destructure_bound_and_unbound_no_double_free() {
    // B follow-up #3: an owned struct destructure of a fresh temp where
    // one heap field is bound (`a`, freed via its binding) and another is
    // discarded (`b: _`, freed via a synthetic discard slot). Run in a
    // loop so any per-iteration imbalance — a double-free of `a` against a
    // whole-struct drop, or a missed/extra free of `b` — faults under
    // ASAN's quarantine. macOS has no LeakSanitizer, so the leak closure
    // is pinned by the IR tests; this is the double-free gate.
    assert_clean_asan_run(
        r#"
struct Pair { a: Vec[i64], b: Vec[i64], n: i64 }
fn mk(x: i64) -> Pair {
    let mut va: Vec[i64] = Vec.new();
    va.push(x);
    let mut vb: Vec[i64] = Vec.new();
    vb.push(x * 2);
    return Pair { a: va, b: vb, n: x };
}
fn main() {
    let mut i: i64 = 0;
    while i < 5 {
        let Pair { a, b: _, n } = mk(i);
        println(a.len() + n);
        i = i + 1;
    }
    println(99);
}
"#,
        &["1", "2", "3", "4", "5", "99"],
        "struct_destructure_bound_and_unbound_no_double_free",
    );
}

#[test]
fn asan_soa_return_value_caller_owns_no_leak_or_double_free() {
    // Per-layout monomorphization slice 3 (SoA returns): the OPPOSITE
    // ownership of the by-value param. A builder `make_entities()` builds a
    // SoA `Vec[Entity]` and RETURNS it — bound by a differently-named local
    // `out` and received into the caller's `entities` (`layout entities`).
    // The return is a MOVE OUT: the callee suppresses its own
    // `FreeSoaGroups` for the returned local (it no longer owns the group
    // buffers), and the caller's `entities` binding frees both buffers
    // exactly once at scope exit. Get the ownership transfer wrong and it's
    // either a double-free (callee frees + caller frees → ASAN) or a leak
    // (neither frees → LSan). Looped 20× to amplify either. The struct
    // carries ≥36 bytes of live payload across the groups so a reachable
    // leak isn't masked by LSan's short-allocation blind spot.
    assert_clean_asan_run(
        r#"
struct Entity { x: f64, y: f64, hp: i64 }
layout entities: Vec[Entity] {
    group physics { x, y }
    group combat { hp }
}
fn make_entities() -> Vec[Entity] {
    let mut out: Vec[Entity] = Vec.new();
    out.push(Entity { x: 1.0, y: 2.0, hp: 100 });
    out.push(Entity { x: 3.0, y: 4.0, hp: 200 });
    out.push(Entity { x: 5.0, y: 6.0, hp: 300 });
    out
}
fn main() {
    let mut sum = 0;
    let mut k = 0;
    while k < 20 {
        let entities: Vec[Entity] = make_entities();
        let mut i = 0;
        while i < entities.len() {
            let e = entities[i];
            sum = sum + e.hp;
            i = i + 1;
        }
        k = k + 1;
    }
    println(sum);
}
"#,
        &["12000"],
        "soa_return_value_caller_owns",
    );
}

#[test]
fn asan_soa_layout_named_param_base_aos_and_mono_soa_no_leak() {
    // Per-layout monomorphization slice 5 (origin-only `soa_layouts`): one
    // by-value helper `total(entities: Vec[Entity])` whose param NAME matches
    // the `layout entities` block is called BOTH ways per iteration —
    //   - with the SoA local `entities` → routed to a SoA monomorph
    //     (caller-retains: no callee-side FreeSoaGroups, the SoA local frees
    //     both group buffers once), and
    //   - with an ordinary AoS `plain: Vec[Entity]` → routed to the AoS BASE
    //     symbol (caller-owns: the plain Vec frees its single buffer once).
    // Retiring the name-keyed by-value param ABI moved BOTH routes onto their
    // correct ownership paths; a regression on either is a double-free (ASAN)
    // or a leak (LSan). Looped 20× to amplify. ≥36 bytes of live payload per
    // element across the groups so a reachable leak isn't masked by LSan's
    // short-allocation blind spot.
    assert_clean_asan_run(
        r#"
struct Entity { x: f64, y: f64, hp: i64 }
layout entities: Vec[Entity] {
    group physics { x, y }
    group combat { hp }
}
fn total(entities: Vec[Entity]) -> i64 {
    let mut t = 0;
    let mut i = 0;
    while i < entities.len() {
        let e = entities[i];
        t = t + e.hp;
        i = i + 1;
    }
    t
}
fn main() {
    let mut sum = 0;
    let mut k = 0;
    while k < 20 {
        let mut entities: Vec[Entity] = Vec.new();
        entities.push(Entity { x: 1.0, y: 2.0, hp: 100 });
        entities.push(Entity { x: 3.0, y: 4.0, hp: 200 });
        entities.push(Entity { x: 5.0, y: 6.0, hp: 300 });
        let mut plain: Vec[Entity] = Vec.new();
        plain.push(Entity { x: 7.0, y: 8.0, hp: 7 });
        plain.push(Entity { x: 9.0, y: 1.0, hp: 11 });
        sum = sum + total(entities) + total(plain);
        k = k + 1;
    }
    println(sum);
}
"#,
        &["12360"],
        "soa_layout_named_param_base_aos_and_mono_soa",
    );
}

#[test]
fn asan_soa_reassign_carried_buffer_no_leak_or_double_free() {
    // Slice 6 (the carried-grid double-buffer): a SoA `grid` is built
    // (`init()` returns a counted-loop-filled SoA Vec — the `with_capacity`
    // form), then REASSIGNED each "frame" from a layout-returning call
    // (`grid = bump(grid)`), the exact shape of a stateful sim's per-frame
    // loop. `compile_soa_assign_from_call` frees the OLD group buffers (the
    // by-value param is caller-retains, so the displaced buffers are owned
    // here) before storing the new header; the binding's queued
    // `FreeSoaGroups` frees the final frame's buffers at scope exit. Get the
    // double-buffer accounting wrong and it's a double-free (free old AND
    // scope-free the same buffers → ASAN) or a per-frame leak (never free the
    // displaced buffers → LSan). The grid is rebuilt + reassigned 5× inside a
    // 20× outer loop to amplify either; `Cell` is 40 bytes (two SoA groups,
    // both group buffers well over LSan's short-allocation blind spot). Sum
    // of `a` (0, bumped +1 ×5) over 8 cells × 20 = 800.
    assert_clean_asan_run(
        r#"
struct Cell { a: f64, b: f64, c: f64, d: f64, e: f64 }
layout grid: Vec[Cell] { group lo { a, b } group hi { c, d, e } }
fn bump(g: Vec[Cell]) -> Vec[Cell] {
    let mut out: Vec[Cell] = Vec.new();
    let mut i = 0;
    while i < g.len() {
        let c = g[i];
        out.push(Cell { a: c.a + 1.0, b: c.b, c: c.c, d: c.d, e: c.e });
        i = i + 1;
    }
    out
}
fn init() -> Vec[Cell] {
    let mut grid: Vec[Cell] = Vec.new();
    let mut i = 0;
    while i < 8 { grid.push(Cell { a: 0.0, b: 1.0, c: 2.0, d: 3.0, e: 4.0 }); i = i + 1; }
    grid
}
fn main() {
    let mut sum = 0.0;
    let mut k = 0;
    while k < 20 {
        let mut grid: Vec[Cell] = init();
        let mut f = 0;
        while f < 5 { grid = bump(grid); f = f + 1; }
        let mut i = 0;
        while i < grid.len() { sum = sum + grid[i].a; i = i + 1; }
        k = k + 1;
    }
    println(sum);
}
"#,
        &["800"],
        "soa_reassign_carried_buffer",
    );
}

#[test]
fn asan_soa_early_return_fall_through_no_leak_or_uaf() {
    // Follow-on (branch-leaf / multi-`return` SoA returns): a return-SoA
    // helper with an EARLY `return early;` guarded by a flag, then a tail
    // `late`. Two ownership paths share one cleanup frame:
    //   flag=true  → `early` moved out (must NOT be freed here — the caller
    //                owns it; freeing pre-return is a UAF/double-free ASAN
    //                catches), `late` never allocated.
    //   flag=false → `early` allocated but NOT returned (must be freed at
    //                scope exit — a compile-time cleanup removal would leak
    //                it on this path; LSan catches), `late` moved out.
    // The early move-out uses a runtime `cap = 0` sentinel
    // (`neutralize_moved_soa_groups_slot`), branch-safe precisely because
    // the frame is shared. Both paths run every iteration (×20); `Cell` is
    // 40 bytes (two group buffers past LSan's short-alloc blind spot).
    // g1[0].a (1.0) + g2[0].a (2.0) = 3.0 × 20 = 60.
    assert_clean_asan_run(
        r#"
struct Cell { a: f64, b: f64, c: f64, d: f64, e: f64 }
layout grid: Vec[Cell] { group lo { a, b } group hi { c, d, e } }
fn build(v: f64) -> Vec[Cell] {
    let mut g: Vec[Cell] = Vec.new();
    let mut i = 0;
    while i < 8 { g.push(Cell { a: v, b: 1.0, c: 2.0, d: 3.0, e: 4.0 }); i = i + 1; }
    g
}
fn pick(flag: bool) -> Vec[Cell] {
    let early: Vec[Cell] = build(1.0);
    if flag {
        return early;
    }
    let late: Vec[Cell] = build(2.0);
    late
}
fn main() {
    let mut sum = 0.0;
    let mut k = 0;
    while k < 20 {
        let g1: Vec[Cell] = pick(true);
        let g2: Vec[Cell] = pick(false);
        sum = sum + g1[0].a + g2[0].a;
        k = k + 1;
    }
    println(sum);
}
"#,
        &["60"],
        "soa_early_return_fall_through",
    );
}

#[test]
fn asan_soa_branch_leaf_tail_returns_no_leak_or_uaf() {
    // Follow-on sibling: branch-leaf BARE tails (`if flag { a } else { b }`,
    // no `return` keyword) — both `a` and `b` are returned locals the
    // recursive `soa_return_local_names` seeds SoA, and each block-scoped
    // leaf is moved out of its branch as the function value. Get the
    // per-branch move-out wrong and the unselected branch's buffers either
    // free early (UAF, ASAN) or never (leak, LSan). pick(true)/pick(false)
    // each iteration (×20); 40-byte Cell. g1[0].a (1.0) + g2[0].a (2.0) =
    // 3.0 × 20 = 60.
    assert_clean_asan_run(
        r#"
struct Cell { a: f64, b: f64, c: f64, d: f64, e: f64 }
layout grid: Vec[Cell] { group lo { a, b } group hi { c, d, e } }
fn fill(v: f64) -> Vec[Cell] {
    let mut g: Vec[Cell] = Vec.new();
    let mut i = 0;
    while i < 8 { g.push(Cell { a: v, b: 1.0, c: 2.0, d: 3.0, e: 4.0 }); i = i + 1; }
    g
}
fn pick(flag: bool) -> Vec[Cell] {
    if flag {
        let a: Vec[Cell] = fill(1.0);
        a
    } else {
        let b: Vec[Cell] = fill(2.0);
        b
    }
}
fn main() {
    let mut sum = 0.0;
    let mut k = 0;
    while k < 20 {
        let g1: Vec[Cell] = pick(true);
        let g2: Vec[Cell] = pick(false);
        sum = sum + g1[0].a + g2[0].a;
        k = k + 1;
    }
    println(sum);
}
"#,
        &["60"],
        "soa_branch_leaf_tail_returns",
    );
}

#[test]
fn asan_a2b2_associated_network_opener_owned_param_fanout_clean() {
    // A2b-2 Phase 2 Slice 1: two *associated* (receiver-less) network
    // openers — `Net.open("a"); Net.open("b")`, the `TcpStream.connect`
    // shape — with an OWNED `String` param fed a literal arg, moved through
    // to the return. Extends the Phase 1 ephemeral proof to the 2-segment
    // associated-call codegen path (a fresh path that Phase 1 never fanned
    // out). Memory-safety proof is identical to the free-fn variant: the
    // coroutine owns the moved-in `String` and returns it, so it flows param
    // → return-slot bit-copy → parent (sole drop owner) and is freed EXACTLY
    // once across the fork/join; the literal arg names no parent binding, so
    // no caller-side drop can double-cancel. A double-free or leak surfaces
    // under LSan/ASan.
    assert_clean_asan_run(
        r#"
struct Net { id: i64 }
impl Net {
    fn open(u: String) -> String with sends(Network) receives(Network) { return u; }
}
fn main() {
    let x = Net.open("aaaaaaaaaaaaaaaaaaaa");
    let y = Net.open("bbbbbbbbbbbbbbbbbbbb");
    println(x);
    println(y);
}
"#,
        &["aaaaaaaaaaaaaaaaaaaa", "bbbbbbbbbbbbbbbbbbbb"],
        "asan_a2b2_associated_network_opener_owned_param_fanout_clean",
    );
}

/// std.secret Zeroize-on-drop (design.md § Clone/Drop/Zeroize): a
/// `Secret[String]` overwrites its inner buffer with zeros before freeing
/// it. The added `memset` runs inside the Secret drop, right before the
/// buffer free — this guards that it neither leaks nor double-frees (a
/// stray free / mis-sized memset would trip ASAN). Multiple secrets built,
/// used, and dropped across a fn boundary.
/// `PriorityQueue[String]` under LSan — `pop` transfers ownership of an
/// element OUT of the backing `Vec`, and `swap` moves elements between
/// slots, so every heap-carrying element crosses the ownership boundary
/// twice on the way through a drain.
///
/// The drain is the interesting half: `into_sorted_vec` consumes the queue
/// by value and rebinds it (`let mut h = self`), so the backing Vec's
/// buffer must be freed exactly once while each String it held is
/// transferred, not copied and not double-freed. The `clear()` case is the
/// other direction — elements dropped in place rather than handed out.
///
/// Linux-only in effect: `-fsanitize=address` runs LeakSanitizer on Linux
/// but not on macOS, so a green local Mac run does not clear this (see
/// CLAUDE.md § leak detection).
#[test]
fn asan_user_hash_impl_key_no_leak() {
    // The compiled user-`impl Hash` path allocates a `Vec[u8]` PER KEY HASH
    // — the sink the impl writes into — and the synthesized hash function
    // frees it after the digest is taken (B-2026-08-26-10). That free is on
    // the map's hot path, so a missing one leaks per insert and per lookup
    // rather than once per program.
    //
    // Heap-backed key fields and enough operations that a regressed free
    // would be unmistakable. Lookups use a BOUND key rather than a
    // temporary: `Map.get` with a temporary heap-owning struct key leaks
    // that temporary on the DERIVED path too, so it is a pre-existing defect
    // with its own row and not what this fixture measures.
    let label = "user_hash_impl_key";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let src = r#"
struct Item { id: i64, name: String }
impl PartialEq for Item { fn eq(ref self, other: ref Item) -> bool { self.id == other.id } }
impl Eq for Item {}
impl Hash for Item {
    fn hash[H: Hasher](ref self, hasher: mut ref H) {
        hasher.write_i64(self.id);
        hasher.write(self.name.bytes());
    }
}
fn main() {
    let mut m: Map[Item, i64] = Map.new();
    let mut i = 0;
    while i < 40 {
        m.insert(Item { id: i, name: f"key-{i}-padding-padding-padding" }, i);
        i = i + 1;
    }
    let mut hits = 0;
    let mut j = 0;
    while j < 40 {
        let probe = Item { id: j, name: f"key-{j}-padding-padding-padding" };
        match m.get(probe) {
            Some(v) => { hits = hits + v; }
            None => {}
        }
        j = j + 1;
    }
    println(m.len());
    println(hits);
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
    assert_eq!(stdout.trim(), "40\n780");
}

#[test]
fn asan_derived_hash_key_control_no_leak() {
    // Control for the fixture above: the same shape with derives instead of
    // hand-written impls. Both must be clean — the user-impl path is not
    // allowed to cost an allocation the derived path does not.
    let label = "derived_hash_key_control";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let src = r#"
#[derive(Hash, Eq, PartialEq)]
struct Item { id: i64, name: String }
fn main() {
    let mut m: Map[Item, i64] = Map.new();
    let mut i = 0;
    while i < 40 {
        m.insert(Item { id: i, name: f"key-{i}-padding-padding-padding" }, i);
        i = i + 1;
    }
    println(m.len());
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
    assert_eq!(stdout.trim(), "40");
}

#[test]
fn asan_struct_wrapped_byvalue_transform_returns_new_tree_no_double_free() {
    // The parser-rewrite shape: consume a tree BY VALUE in a recursive
    // transformer that returns a NEW tree (moving the children through),
    // including move-out of `Vec[Expr]` elements into the transformer in
    // a for-loop. The original tree is consumed exactly once and the
    // rebuilt tree freed exactly once — no double-free of a child that
    // was moved into the new tree, no leak of the consumed original.
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
fn fold(e: Expr) -> Expr {
    match e {
        Num(n) => Num(n),
        Neg(u) => Neg(Unary { operand: fold(u.operand) }),
        Add(b) => {
            let l = fold(b.left);
            let r = fold(b.right);
            Add(BinOp { left: l, right: r })
        }
    }
}
fn build(n: i64) -> Expr {
    if n <= 0 { Num(1) }
    else { Add(BinOp { left: Num(n), right: build(n - 1) }) }
}
fn main() {
    let t = build(15);
    let t2 = fold(t);
    let mut v: Vec[Expr] = Vec.new();
    let mut i: i64 = 0;
    while i < 12 { v.push(build(i)); i = i + 1; }
    let mut total: i64 = eval(t2);
    for e in v {
        let folded = fold(e);
        total = total + eval(folded);
    }
    println(total);
}
"#,
        &["419"],
        "struct_wrapped_byvalue_transform_returns_new_tree",
    );
}

#[test]
fn asan_struct_wrapped_move_out_then_consume_no_leak() {
    // B-2026-06-14-31 (leak #2) — `let t2 = t1` (a shared-enum local MOVED
    // to another local) that is then CONSUMED by-value (`eval(t2)`).
    // Pre-fix, the shared-enum let-binding's Identifier-RHS path called the
    // value-enum move suppressor, which emitted a SPURIOUS aliasing-acquire
    // `emit_refcount_inc` on the source on TOP of the destination inc the
    // shared-info path already emitted — pinning the box at rc=1 after both
    // scope-exit `RcDec`s, leaking the whole tree (silent under mac ASAN).
    // The no-double-free crux: the SAME shape WITHOUT consume, and the
    // subtree-into-parent move (a separate edge), must stay leak-free AND
    // not double-free. Looped to surface a per-iteration leak.
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
    let mut total: i64 = 0;
    let mut i: i64 = 0;
    while i < 40 {
        // move-out then consume
        let t1 = Add(BinOp { left: Num(3), right: Num(4) });
        let t2 = t1;
        total = total + eval(t2);
        // move-out then drop WHOLE (no consume)
        let u1 = Neg(Unary { operand: Num(5) });
        let u2 = u1;
        match u2 { Num(n) => { total = total + n; } Add(b) => { total = total + 0; } Neg(x) => { total = total + 0; } }
        // subtree moved into a parent literal, then consumed
        let sub = Add(BinOp { left: Num(2), right: Num(3) });
        let p1 = Add(BinOp { left: sub, right: Num(10) });
        total = total + eval(p1);
        i = i + 1;
    }
    println(total);
}
"#,
        &["880"],
        "struct_wrapped_move_out_then_consume",
    );
}

/// Moving a heap field OUT of an owned by-value struct param (deep-copied at
/// entry, #14/#17) while the param's scope-exit `StructDrop` still freed that
/// field double-freed the moved-out buffer — surfaced by phase-12 selfhost
/// slice 3c-ii (`render_variant` / `render_struct_field`), minimal
/// `fn f(s: S) -> String { s.a }`. Exercises all three move-out shapes:
/// field-access return (`p.a`), destructure-then-return (`let Pair{a,b}=p; a`),
/// and a CONSUMED `Vec` field destructured from the param (`for t in tags`)
/// alongside a moved `String` field — in a loop with ≥36-byte payloads so a
/// leak (LSan) or double-free (ASAN) trips. Fixed by the `FieldAccess`
/// move-out suppressor arm + the callee-owned place-source struct-destructure
/// transfer (`zero_struct_field_move_cap`).
#[test]
fn asan_by_value_struct_field_moveout_no_double_free() {
    assert_clean_asan_run(
            "struct Pair { a: String, b: String }\n\
             struct Node { tags: Vec[String], name: String }\n\
             fn pick_a(p: Pair) -> String { p.a }\n\
             fn pick_a_destructured(p: Pair) -> String { let Pair { a, b } = p; a }\n\
             fn join_tags(n: Node) -> String {\n\
             \x20   let Node { tags, name } = n;\n\
             \x20   let mut out = name;\n\
             \x20   for t in tags { out.push_str(t); }\n\
             \x20   out\n\
             }\n\
             fn main() {\n\
             \x20   let mut i = 0;\n\
             \x20   while i < 50 {\n\
             \x20       let p1 = Pair { a: \"pair-a-field-payload-long-enough-string\".to_string(), b: \"pair-b-field-payload-long-enough-string\".to_string() };\n\
             \x20       let r1 = pick_a(p1);\n\
             \x20       let p2 = Pair { a: \"destructure-a-payload-long-enough-string\".to_string(), b: \"destructure-b-payload-long-enough-string\".to_string() };\n\
             \x20       let r2 = pick_a_destructured(p2);\n\
             \x20       let mut tags: Vec[String] = Vec.new();\n\
             \x20       tags.push(\"tag-element-payload-long-enough-string-one\".to_string());\n\
             \x20       tags.push(\"tag-element-payload-long-enough-string-two\".to_string());\n\
             \x20       let n = Node { tags: tags, name: \"node-name-payload-long-enough-string\".to_string() };\n\
             \x20       let r3 = join_tags(n);\n\
             \x20       i = i + 1;\n\
             \x20   }\n\
             \x20   println(\"done\");\n\
             }\n",
            &["done"],
            "asan_by_value_struct_field_moveout_no_double_free",
        );
}

#[test]
fn asan_getmove_letelse_get_escaping_binding_no_double_free() {
    // Slice 3s: a `let Some(s) = m.get(k) else { … }` binding escapes
    // into the enclosing scope by construction — the payload clone is
    // unconditional there (no arm to analyze).
    assert_clean_asan_run(
        r#"
fn read(m: ref Map[i64, String]) -> i64 {
    let Some(s) = m.get(7) else { return 0 }
    s.len()
}
fn main() {
    let mut i = 0;
    while i < 3 {
        let mut m: Map[i64, String] = Map.new();
        m.insert(7, f"map string payload padded beyond thirty-six bytes {7}");
        println(read(m));
        i = i + 1;
    };
}
"#,
        &["51", "51", "51"],
        "getmove_letelse_get_escaping_binding_no_double_free",
    );
}

#[test]
fn asan_boxelem_loop_elem_destructure_consume_no_double_free() {
    // Slice 3u: `for o in v { match o { Some(Holder { name, id }) => …
    // } }` — the loop binding is a bit-copy of the element; with the
    // element drop armed it must be marked as a BORROW
    // (`for_loop_borrow_vars`, extended to boxed/inline-struct
    // payloads) so the destructured fields alias.
    assert_clean_asan_run(
        r#"
struct Holder { name: String, id: i64 }
fn main() {
    let mut v: Vec[Option[Holder]] = Vec.new();
    let mut i = 0;
    while i < 3 {
        v.push(Some(Holder { name: f"holder payload padded beyond thirty-six bytes {i}", id: i }));
        i = i + 1;
    };
    let mut total = 0;
    for o in v {
        match o {
            Some(Holder { name, id }) => { total = total + (name.len() as i64) + id; },
            None => {},
        }
    }
    println(total);
}
"#,
        &["144"],
        "boxelem_loop_elem_destructure_consume_no_double_free",
    );
}

#[test]
fn asan_tail3v_get_unchecked_binding_no_double_free() {
    // Slice 3v: `let s = v.get_unchecked(0)` bound a shallow element
    // alias with an OWNED track — exit 133 at the two scope exits. Now
    // routes through the same deep-clone as `let s = v[i]`.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i = 0;
    while i < 3 {
        let mut v: Vec[String] = Vec.new();
        v.push(f"unchecked payload padded beyond thirty-six bytes {i}");
        unsafe {
            let s = v.get_unchecked(0);
            println(s.len());
        }
        println(v.len());
        i = i + 1;
    };
}
"#,
        &["50", "1", "50", "1", "50", "1"],
        "tail3v_get_unchecked_binding_no_double_free",
    );
}

#[test]
fn asan_tail3v_whole_tuple_nontail_consume_no_leak() {
    // Slice 3v: `Some(x) => { take(x); }` over a Map.get tuple payload —
    // the whole-tuple clone (3u leg A) is now TRACKED via
    // `synthesize_tuple_drop_fn_te`, whose `type_expr_has_drop_heap`
    // guard needed the inferred `str` spelling (4th/5th trap sites:
    // the classifier, the tuple drop emitter, and its cap-zero dual).
    assert_clean_asan_run(
        r#"
fn take(t: (String, i64)) -> i64 {
    t.1
}
fn main() {
    let mut i = 0;
    while i < 3 {
        let mut m: Map[i64, (String, i64)] = Map.new();
        m.insert(i, (f"tuple payload padded beyond thirty-six bytes {i}", i));
        match m.get(i) {
            Some(x) => { println(take(x)); },
            None => { println(0 - 1); },
        }
        i = i + 1;
    };
}
"#,
        &["0", "1", "2"],
        "tail3v_whole_tuple_nontail_consume_no_leak",
    );
}

#[test]
fn asan_tail3v_whole_tuple_tail_move_no_double_free() {
    // Slice 3v: the tail-move sibling — `let t = match m.get(i) {
    // Some(x) => x, … }` then consume `t`; the cloned tuple's track and
    // the arm-tail move suppression must compose (single free).
    assert_clean_asan_run(
        r#"
fn take(t: (String, i64)) -> i64 { t.1 }
fn main() {
    let mut i = 0;
    while i < 3 {
        let mut m: Map[i64, (String, i64)] = Map.new();
        m.insert(i, (f"tuple payload padded beyond thirty-six bytes {i}", i));
        let t = match m.get(i) {
            Some(x) => x,
            None => (f"none-{i}", 0 - 1),
        };
        println(take(t));
        i = i + 1;
    };
}
"#,
        &["0", "1", "2"],
        "tail3v_whole_tuple_tail_move_no_double_free",
    );
}

#[test]
fn asan_forloop_struct_element_whole_move_no_double_free() {
    // B-2026-07-04-17: iterating an owned `Vec[<heap struct>]` by value and
    // MOVING the loop element whole into a NEW owner (`let x = a`) must not
    // double-free at teardown — the element binding is a bit-copy alias of
    // the container slot, so `x`'s scope drop and the container's per-element
    // drain would free the same String buffer twice.
    assert_clean_asan_run(
        r#"
struct A { s: String }
fn build() -> Vec[A] {
    let mut v: Vec[A] = Vec.new();
    let mut i = 0;
    while i < 6 { v.push(A { s: "forloop_element_whole_move_payload_theta_xx".to_string() }); i = i + 1; }
    v
}
fn main() {
    let items = build();
    let mut n: i64 = 0;
    for a in items {
        let x = a;
        n = n + x.s.len();
    }
    println(n);
}
"#,
        &["258"], // 6 * len("forloop_element_whole_move_payload_theta_xx") = 6 * 43
        "forloop_struct_element_whole_move_no_double_free",
    );
}

#[test]
fn asan_forloop_struct_element_field_move_no_double_free() {
    // B-2026-07-04-17, the field-move form: moving a heap field OUT of a
    // for-loop element into a fresh struct literal (`let w = A { s: a.s }`)
    // must not double-free — same aliasing hazard as the whole-move.
    assert_clean_asan_run(
        r#"
struct A { s: String }
fn build() -> Vec[A] {
    let mut v: Vec[A] = Vec.new();
    let mut i = 0;
    while i < 6 { v.push(A { s: "forloop_element_field_move_payload_iota_xx".to_string() }); i = i + 1; }
    v
}
fn main() {
    let items = build();
    let mut n: i64 = 0;
    for a in items {
        let w = A { s: a.s };
        n = n + w.s.len();
    }
    println(n);
}
"#,
        &["252"], // 6 * len("forloop_element_field_move_payload_iota_xx") = 6 * 42
        "forloop_struct_element_field_move_no_double_free",
    );
}

#[test]
fn asan_return_owned_generic_param_no_double_free() {
    // B-2026-07-11-35 (return-owned-`T`-param leg): returning an owned heap
    // (String / Vec) PARAM from a generic fn (`fn echo[T](x: T) -> T { x }`)
    // handed back the caller's moved-in buffer, which the caller then freed a
    // second time (double-free abort). The mono tail now deep-copies the
    // returned owned-vecstr param, mirroring the non-generic path. Loops so a
    // leak (the dual failure — an over-suppressed copy) accumulates for LSan,
    // and exercises String, Vec[i64], and Vec[String] in one program so the
    // per-instantiation mono symbols (the collision the copy exposed) are all
    // live — a shared body would run one element stride over the others.
    assert_clean_asan_run(
        r#"
fn echo[T](x: T) -> T { x }
fn main() {
    let mut r: i64 = 0;
    while r < 3 {
        let a: String = echo(f"fresh-{r}-aaaa");
        println(a);
        let s: String = f"local-{r}-bbbb";
        let b: String = echo(s);
        println(b);
        let vi: Vec[i64] = echo([r, r + 1, r + 2]);
        println(f"{vi[2]}");
        let vs: Vec[String] = echo([f"e-{r}-x", f"e-{r}-y"]);
        println(vs[1]);
        r = r + 1;
    }
}
"#,
        &[
            "fresh-0-aaaa",
            "local-0-bbbb",
            "2",
            "e-0-y",
            "fresh-1-aaaa",
            "local-1-bbbb",
            "3",
            "e-1-y",
            "fresh-2-aaaa",
            "local-2-bbbb",
            "4",
            "e-2-y",
        ],
        "return_owned_generic_param_no_double_free",
    );
}

/// B-2026-08-07-2 shapes 1, 2 and 5 — the nested box's owner when there is
/// no binding to hang one on, and when the binding escapes.
///
/// B-2026-08-06-32 gave the box an owner at the LET SITE and recorded the
/// positions it could not reach as a separate row: a fresh temp argument
/// and a `Vec.push` have no slot to register against, and a binding
/// RETURNED from its frame has its registration deliberately retracted
/// because freeing an escaped box is a use-after-free. All three were
/// measured leaking 32 B per construction.
///
/// The owner is the CALLEE's owned, non-escaping parameter — the same place
/// B-2026-08-05-7 and B-2026-08-06-9 leg A put it for the directly-boxed
/// sibling, extended to the nested descriptor. That one registration closes
/// all three at once, including the escape shape the row expected to need a
/// different mechanism: the escaped box does get an owner in the caller,
/// and it is whatever consumes the returned value.
///
/// `Vec.push` is the arm that matters most and the reason this is not purely
/// an `-O0` fixture: a `Vec` keeps the allocation live past the point LLVM
/// could fold it away, so that shape leaked at the DEFAULT `-O2` too — the
/// only member of the family that did.
///
/// THE OTHER HALF IS THE DOUBLE-FREE DIRECTIONS, and they are why the last
/// four arms exist. A second owner, not a missing one, is this family's
/// failure mode, and giving the callee ownership creates one wherever a
/// live binding still aliases the box it is handed. Each of these was
/// measured as a glibc `double free detected in tcache 2` at `-O0` during
/// implementation, and each is a strictly worse outcome than the leak it
/// replaced:
///
///   * `cls(idr(b))` — the passthrough result is `b`'s box; `b` keeps its
///     registration because `idr` can return its parameter, so the consumer
///     has to disarm the source it aliases.
///   * `cls(idr(idr(b)))` — the outer passthrough's argument is a CALL, so
///     an identifier-only resolver stops before reaching `b`.
///   * `let c = idr(b); cls(c)` — `c` is an identifier that registers
///     nothing; the owner is one alias-map hop away.
///   * `fwd(b)`, which forwards to another by-value param rather than
///     returning — the control for the three above. The forwarding param
///     escapes, so it must register NOTHING and let the terminal consumer
///     be the sole owner; a rule that registered per-hop would free once
///     per hop.
///
/// The `nobox` arm is the over-suppression control in the other direction:
/// `Result[Option[i64], E]` stores its inner scalar inline and allocates
/// nothing, so a registration there would free a word that was never a
/// pointer.
///
/// The expected value is COMPUTED rather than read off a run: every payload
/// is seeded from the opaque `env.args().len()`, eight arms subtract the
/// seed back out to leave `i`, and the String arm contributes 1 — `8i + 1`
/// per iteration, so `8 * (0+…+39) + 40 = 6280`. Confirmed against the
/// built program at both opt levels (406/406 and 126/126 allocs/frees).
#[test]
fn asan_nested_box_owned_by_callee_when_no_binding_owns_it() {
    assert_clean_asan_run_min_allocs(
        r#"
fn cls(r: Result[Option[Option[i64]], i64]) -> i64 {
    match r {
        Result.Ok(Option.Some(Option.Some(x))) => x,
        Result.Ok(_) => -1,
        Result.Err(e) => e,
    }
}
fn clsstr(r: Result[Option[Option[String]], i64]) -> i64 {
    match r {
        Result.Ok(Option.Some(Option.Some(s))) => if s.len() > 0 { 1 } else { 0 },
        Result.Ok(_) => -1,
        Result.Err(e) => e,
    }
}
fn nobox(r: Result[Option[i64], i64]) -> i64 {
    match r {
        Result.Ok(Option.Some(x)) => x,
        Result.Ok(Option.None) => -1,
        Result.Err(e) => e,
    }
}
fn idr(r: Result[Option[Option[i64]], i64]) -> Result[Option[Option[i64]], i64] { r }
fn fwd(r: Result[Option[Option[i64]], i64]) -> i64 { cls(r) }
fn mk(x: i64) -> Result[Option[Option[i64]], i64] {
    let b: Result[Option[Option[i64]], i64] = Result.Ok(Option.Some(Option.Some(x)));
    b
}

fn main() {
    let n = env.args().len() as i64;
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 40 {
        acc = acc + cls(Result.Ok(Option.Some(Option.Some(n + i)))) - n;

        let mut v: Vec[Result[Option[Option[i64]], i64]] = [];
        v.push(Result.Ok(Option.Some(Option.Some(n + i))));
        acc = acc + cls(v[0]) - n;

        acc = acc + cls(mk(n + i)) - n;

        let b: Result[Option[Option[i64]], i64] = Result.Ok(Option.Some(Option.Some(n + i)));
        acc = acc + cls(idr(b)) - n;

        let c: Result[Option[Option[i64]], i64] = Result.Ok(Option.Some(Option.Some(n + i)));
        acc = acc + cls(idr(idr(c))) - n;

        let d: Result[Option[Option[i64]], i64] = Result.Ok(Option.Some(Option.Some(n + i)));
        let e1 = idr(d);
        acc = acc + cls(e1) - n;

        let g: Result[Option[Option[i64]], i64] = Result.Ok(Option.Some(Option.Some(n + i)));
        acc = acc + fwd(g) - n;

        acc = acc + clsstr(Result.Ok(Option.Some(Option.Some(f"p{n + i}"))));

        acc = acc + nobox(Result.Ok(Option.Some(n + i))) - n;

        i = i + 1;
    }
    println(acc);
}
"#,
        &["6280"],
        "nested_box_owned_by_callee_when_no_binding_owns_it",
        30,
    );
}

/// B-2026-08-07-12 — a struct passed BY VALUE whose only heap lives inside
/// an `Option`/`Result` field. Two independent roots, and the fixture holds
/// both because fixing either alone makes the other worse.
///
/// ROOT A, THE GATE (the `take_o` / `take_r` / `take_ov` rows). The
/// caller-scope drop for a fresh struct-literal argument was gated on two
/// signals that are both blind to `Option`: `aggregate_has_heap_field` is
/// LLVM-structural and an erased `Option` payload is not the `{ptr,len,cap}`
/// shape, and `type_expr_has_drop_heap` hardcodes `"Option" | "Result" =>
/// false`. Nothing was registered and the buffer leaked — at BOTH opt
/// levels, so this was ordinary `karac build` output. The FN-CALL arm of the
/// same registrar already ORed in `option_field_te_has_drop_heap`; the
/// struct-literal arm and `arg_is_entry_copied_heap_struct` did not.
///
/// ROOT B, THE ENTRY COPY (the `take_b` rows). For a BOXED payload the
/// callee's entry copy mallocs a fresh envelope, copies the old box's value
/// into it, and then asked the ERASED generic `Option` layout to duplicate
/// what that value points at — which reports no heap, so nothing was
/// copied. Two boxes, one `String`, and both frames free it.
///
/// WHY BOTH ARE HERE. Root A alone converts the boxed rows from a leak into
/// a DOUBLE FREE (measured: forcing the gate on with a sibling `String`
/// field, which is what `take_m` does, aborted at both opt levels). Root B
/// alone gives the callee its own `String` and then strands the caller's, a
/// NEW -O2 leak in the `take_b` rows. Only together is every row clean, and
/// that is exactly what this fixture asserts.
///
/// `take_b(mk_b(..))` is not decoration: because the fn-call arm already had
/// root A's disjunct, that spelling was a live double free on main before
/// root B — corruption reachable with a callee whose body is a literal.
///
/// CONTROLS. `take_p` is a plain `String` field (always worked, must not
/// double-free now that its neighbours register too). `take_t` is the
/// sibling-field shape whose accidental correctness localized the bug.
/// `borrow_o` is a `ref` param, which must stay caller-owned — registering
/// a caller temp drop there would be a second owner, not a first.
///
/// B-2026-08-07-15 — a struct that is NOT copy-supported, passed as a FRESH
/// TEMP by value, was dropped by BOTH frames. The callee owns it BY TRANSFER
/// (B-2026-08-05-33: no entry copy is possible, so it takes the caller's
/// buffers and registers the drop), and that arm's safety argument is an
/// explicit lockstep with the caller's retraction —
/// `move_declined_copy_struct_arg`, which keys on a MOVED NAMED BINDING. A
/// fresh struct literal is not one, and the caller's own registrar fires on
/// `aggregate_has_heap_field`, which a sibling `String`/`Vec` satisfies. So
/// the temp had an owner on one side and was transferred on the other:
/// measured 480 valgrind errors at the DEFAULT -O2 with a callee whose body
/// is `1`.
///
/// `ig_ms` / `ig_vs` / `ig_ss` are the corrupting rows — any `Map`/`Set`
/// field makes `field_copy_supported` decline ("Heap the outer-buffer copy
/// can't duplicate → bail"), and the sibling heap field turns the caller's
/// gate on. `h.ig` and `ig_gen` carry the same shape through the METHOD and
/// MONOMORPH arg loops, which have their own copies of the registration.
///
/// TWO ROWS SAY THE CLASS IS WIDER THAN "a `Map`/`Set` field", which is how
/// the bug was first titled. What matters is only that
/// `field_copy_supported` DECLINES; a collection field is the commonest way
/// to make it, not the condition:
///   * `ig_nest` reaches it through a NESTED struct — `Outer { a: String,
///     i: Inner }` over `Inner { m: Map[..] }`, where the outer struct names
///     no collection at all. 480 errors pre-fix.
///   * `ig_arr` has NO `Map` or `Set` anywhere in it: an `Array[i64, 4]`
///     field hits `field_copy_supported`'s conservative `_ => false` arm.
///     10 errors pre-fix — the row a search for "Map" would never find.
///
/// FIVE CONTROLS, and each one is a way the fix could have been wrong:
///   * `ig_mo` — the same struct with NO sibling heap field. The gate never
///     fired for it, so it was already clean and must stay clean.
///   * `ig_ps` — a plain `String` field, i.e. COPY-SUPPORTED. It takes the
///     other branch entirely (callee entry-copies, both frames own distinct
///     heap) and its IR must be untouched.
///   * `ig_sh` — a shared-owning struct. Copy-support declines for an
///     unrelated reason and the callee does NOT take ownership
///     (B-2026-08-05-32), so the caller's drop is the box's only rc-dec and
///     suppressing it would strand the box.
///   * `ig_ref` — a `ref` param fed a fresh LITERAL. Legal, already clean,
///     and the caller's temp drop is the SOLE owner there: suppressing it
///     would have traded the corruption for a leak. It survives because a
///     `ref` argument never reaches that registrar at all (the rvalue-ref
///     arm continues through `queue_ref_rvalue_arg_cleanup` first) — which
///     this row establishes by measurement, not by reading control flow.
///   * the `named` binding — the half of the lockstep that always worked,
///     here so a fix that broke it cannot pass.
///
/// A GENERIC STRUCT IS THE THIRD ROUTE IN, and the fix's first cut gave it
/// away wholesale — the predicate excluded every generic struct, because a
/// caller-side reading cannot evaluate the callee's mono rescue.
/// B-2026-08-07-17: sound about the monomorph, too broad for the rest.
/// `ig_mix_conc` takes `Mix[T] { v: T, s: String }` at a CONCRETE param,
/// where there IS no monomorph and no substitution, so the callee reaches
/// the transfer arm like any other and kept both owners — 10 invalid frees
/// per 10 iterations at BOTH opt levels, still there after the first fix
/// landed. The erased bare `T` is just another way to close
/// `field_copy_supported`, alongside the `Map` and the `Array` above.
///
/// `ig_mix_mono[T]` is its opposite number and the reason the flag is the
/// callee's own answer rather than a caller-side guess: there the monomorph
/// resolves `T` and ENTRY-COPIES, so the caller's temp IS orphaned and its
/// drop must stay. Suppressing it re-opens B-2026-08-06-2 defect (B) —
/// measured at 140 B in 4 blocks while writing this.
///
/// Expected value is COMPUTED: 1+2+4+8+16+32+64+128+256+1+512+1024+2048
/// +4096 = 8192 per iteration, x40 = 327680.
#[test]
fn asan_fresh_temp_struct_arg_owned_by_transfer_no_double_free() {
    assert_clean_asan_run_min_allocs(
        r#"
struct Ms { a: String, m: Map[String, i64] }
struct Vs { a: Vec[i64], m: Map[String, i64] }
struct Ss { a: String, s: Set[i64] }
struct Mo { m: Map[String, i64] }
struct Ps { a: String }
struct Inner { m: Map[String, i64] }
struct Outer { a: String, i: Inner }
struct Ar { a: String, arr: Array[i64, 4] }
struct Mix[T] { v: T, s: String }
shared struct Shr { v: i64 }
struct Sh2 { a: String, sh: Shr }
struct Host { k: i64 }
impl Host {
    fn ig(ref self, x: Ms) -> i64 { 8 }
}
fn ig_ms(x: Ms) -> i64 { 1 }
fn ig_vs(x: Vs) -> i64 { 2 }
fn ig_ss(x: Ss) -> i64 { 4 }
fn ig_mo(x: Mo) -> i64 { 16 }
fn ig_ps(x: Ps) -> i64 { 32 }
fn ig_sh(x: Sh2) -> i64 { 64 }
fn ig_ref(x: ref Ms) -> i64 { 128 }
fn ig_gen[T](x: Ms, t: T) -> i64 { 256 }
fn ig_nest(x: Outer) -> i64 { 512 }
fn ig_arr(x: Ar) -> i64 { 1024 }
fn ig_mix_conc(x: Mix[String]) -> i64 { 2048 }
fn ig_mix_mono[T](x: Mix[T]) -> i64 { 4096 }
fn mk_map(k: i64) -> Map[String, i64] {
    let mut m: Map[String, i64] = Map.new();
    m.insert(f"key-padded-out-so-it-heap-allocates-{k}", k);
    m
}
fn main() {
    let n = env.args().len() as i64;
    let h: Host = Host { k: n };
    let mut acc: i64 = 0;
    let mut i: i64 = 0;
    while i < 40 {
        acc = acc + ig_ms(Ms { a: f"ms-padded-out-so-it-heap-allocates-{n + i}", m: mk_map(n + i) });
        let mut v: Vec[i64] = Vec.new();
        v.push(n + i);
        acc = acc + ig_vs(Vs { a: v, m: mk_map(n + i) });
        let mut s: Set[i64] = Set.new();
        s.insert(n + i);
        acc = acc + ig_ss(Ss { a: f"ss-padded-out-so-it-heap-allocates-{n + i}", s: s });
        acc = acc + h.ig(Ms { a: f"me-padded-out-so-it-heap-allocates-{n + i}", m: mk_map(n + i) });
        acc = acc + ig_mo(Mo { m: mk_map(n + i) });
        acc = acc + ig_ps(Ps { a: f"ps-padded-out-so-it-heap-allocates-{n + i}" });
        acc = acc + ig_sh(Sh2 { a: f"sh-padded-out-so-it-heap-allocates-{n + i}", sh: Shr { v: n + i } });
        acc = acc + ig_ref(Ms { a: f"rf-padded-out-so-it-heap-allocates-{n + i}", m: mk_map(n + i) });
        acc = acc + ig_gen(Ms { a: f"gn-padded-out-so-it-heap-allocates-{n + i}", m: mk_map(n + i) }, i);
        acc = acc + ig_nest(Outer { a: f"nt-padded-out-so-it-heap-allocates-{n + i}", i: Inner { m: mk_map(n + i) } });
        let arr: Array[i64, 4] = [n + i, 1, 2, 3];
        acc = acc + ig_arr(Ar { a: f"ar-padded-out-so-it-heap-allocates-{n + i}", arr: arr });
        acc = acc + ig_mix_conc(Mix { v: f"mc-padded-out-so-it-heap-allocates-{n + i}", s: f"mcs-padded-out-so-it-heap-allocates-{n + i}" });
        acc = acc + ig_mix_mono(Mix { v: f"mm-padded-out-so-it-heap-allocates-{n + i}", s: f"mms-padded-out-so-it-heap-allocates-{n + i}" });
        let named: Ms = Ms { a: f"nm-padded-out-so-it-heap-allocates-{n + i}", m: mk_map(n + i) };
        acc = acc + ig_ms(named);
        i = i + 1;
    }
    println(acc);
}
"#,
        &["327680"],
        "fresh_temp_struct_arg_owned_by_transfer",
        100,
    );
}

#[test]
fn asan_generic_mono_bare_t_local_reassign_no_leak() {
    // B-2026-07-15-6: inside a monomorphized generic fn, a bare-`T`
    // annotated local (`let mut best: T = items[0]`) mono'd to String
    // never registered as a tracked heap binding (`is_string_type_expr`
    // saw the raw name "T"), so the Assign arm's eager old-value free
    // never armed and every reassignment (`best = items[i]`) leaked the
    // previous buffer — one leaked allocation per improving element in
    // the running-max scan. The concrete `Vec[String]` twin was always
    // clean. Leak-clean now, with the returned value moved out (its
    // scope-exit free must stay suppressed — no double-free).
    assert_clean_asan_run(
        r#"
fn largest[T: Ord + Clone](items: ref Vec[T]) -> T {
    let mut best: T = items[0].clone();
    for i in 1..items.len() {
        if items[i] > best {
            best = items[i].clone();
        }
    }
    best
}
fn main() {
    let mut words: Vec[String] = Vec.new();
    words.push("alpha");
    words.push("middle");
    words.push("zebra");
    println(largest(words));
    let mut nums: Vec[i64] = Vec.new();
    nums.push(3);
    nums.push(9);
    nums.push(1);
    println(largest(nums));
}
"#,
        &["zebra", "9"],
        "generic_mono_bare_t_local_reassign_no_leak",
    );
}

#[test]
fn asan_nested_struct_field_move_out_no_double_free() {
    // B-2026-07-15-22: `let bound = o.inner` moving a struct-typed heap-bearing
    // field out of an owned struct double-freed the inner Vec buffer at scope
    // exit (both `bound`'s and `o`'s StructDrop freed it → `free(): double free
    // detected`, exit 134). The struct-var tracking path suppressed only
    // `Identifier` / `TupleIndex` move-out RHS, never a `FieldAccess` struct-typed
    // field, so nothing cap-zeroed the source's drop. Fixed by calling
    // `suppress_struct_field_move_into_literal` (its nested-aggregate arm recurses
    // into the moved-out struct's Vec/String leaves) for the FieldAccess RHS.
    // Verify the whole round-trip is clean under ASAN/LSan (no double-free AND no
    // leak — the delicate move-suppression tension: over-suppressing would strand
    // the buffer). Loops so a leak accumulates. Non-generic + generic + a sibling
    // heap field that must still free. NOTE: the reproducing construction is
    // `Vec.new()+push` moved into the struct field — an inline array-literal field
    // (`Inner { data: [1, 2, 3] }`) does NOT reproduce (its temp is materialized
    // directly into the field on a different move path, already cap-balanced), so
    // this test builds each Vec via a tracked local first, exactly as the df3/df4
    // repro did. Was output-clean but ASAN-dirty pre-fix — the double-free fires
    // at scope-exit AFTER the prints, so an output-only harness can't see it.
    assert_clean_asan_run(
        r#"
struct Inner { data: Vec[i64] }
struct Outer { inner: Inner, extra: Vec[i64] }
struct GInner[T] { data: Vec[T] }
struct GOuter[T] { inner: GInner[T] }
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 200 {
        let mut d: Vec[i64] = Vec.new();
        d.push(i); d.push(i + 1); d.push(i + 2);
        let mut ex: Vec[i64] = Vec.new();
        ex.push(i * 10); ex.push(i * 20);
        let o: Outer = Outer { inner: Inner { data: d }, extra: ex };
        let bound: Inner = o.inner;
        total = total + bound.data.len() + o.extra.len();
        let mut gd: Vec[i64] = Vec.new();
        gd.push(i); gd.push(i + 1); gd.push(i + 2); gd.push(i + 3);
        let g: GOuter[i64] = GOuter { inner: GInner { data: gd } };
        let gb: GInner[i64] = g.inner;
        total = total + gb.data.len();
        i = i + 1;
    }
    println(total);
}
"#,
        &["1800"],
        "nested_struct_field_move_out_no_double_free",
    );
}

#[test]
fn asan_generic_wrapper_bare_t_field_scope_exit_no_leak() {
    // B-2026-07-15-11 — a monomorphized single-field generic wrapper
    // `Box[T] { v: T }` whose field IS the bare type param, bound to a heap
    // type (`String` / `Vec[..]` / `Vec[String]`), leaked the whole field
    // buffer at scope exit: the mono struct-drop classifier read the erased
    // declared field name `T` (matching none of Vec/String/Map) and skipped
    // it. The fix resolves the field's declared TypeExpr through the mono
    // subst and classifies a Vec/String monomorph for the buffer free (and a
    // `Vec[String]` monomorph drains its elements). Loop so any per-iteration
    // strand shows up as a large leak under LSan.
    assert_clean_asan_run(
        r#"
struct Box[T] {
    v: T,
}
fn mkvec() -> Vec[String] {
    let mut v = Vec.new();
    v.push((1).to_string());
    v.push((2).to_string());
    v
}
fn main() {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 50 {
        let bs = Box { v: i.to_string() };
        acc = acc + bs.v.len();
        let bv = Box { v: [i, i + 1, i + 2] };
        acc = acc + bv.v.len();
        let bvs = Box { v: mkvec() };
        acc = acc + bvs.v.len();
        i = i + 1;
    }
    println(acc);
}
"#,
        &["340"],
        "generic_wrapper_bare_t_field_scope_exit_no_leak",
    );
}

#[test]
fn asan_generic_wrapper_accessor_and_move_no_double_free() {
    // B-2026-07-15-11 — the coupled double-free half. Once the mono drop
    // frees the bare-T field at scope exit, three sites would double-free
    // against it without the paired fixes: (1) a borrowed-receiver
    // field-return accessor `get(ref self) -> T { self.v }` (must deep-clone
    // with the CONCRETE resolved field type, not the shallow last-writer-wins
    // `karac_clone_T`); (2) a whole-struct move `let c = b` / `return b`
    // (`zero_struct_move_caps` must zero the mono field's cap+len); (3) a
    // by-value consume `sink(b)`. Exercises all three across TWO
    // instantiations (`Box[String]` + `Box[Vec[i64]]`) so a colliding shared
    // clone/drop symbol surfaces.
    assert_clean_asan_run(
        r#"
struct Box[T] {
    v: T,
}
impl[T] Box[T] {
    fn get(ref self) -> T {
        self.v
    }
}
fn sink(b: Box[String]) -> i64 {
    let s = b.v;
    s.len()
}
fn main() {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 40 {
        let a = Box { v: i.to_string() };
        let sa = a.get();
        acc = acc + sa.len();
        let b = Box { v: [i, i + 1, i + 2, i + 3] };
        let sb = b.get();
        acc = acc + sb.len();
        let c = Box { v: i.to_string() };
        let d = c;
        let sd = d.v;
        acc = acc + sd.len();
        let e = Box { v: i.to_string() };
        acc = acc + sink(e);
        i = i + 1;
    }
    println(acc);
}
"#,
        &["370"],
        "generic_wrapper_accessor_and_move_no_double_free",
    );
}

#[test]
#[ignore = "B-2026-08-05-35 sweep: does not compile — chained field receivers (`a.b.c…`) are deferred to v1.x. Silently SKIPPED until the harness learned to fail on a codegen error."]
fn asan_generic_wrapper_nested_struct_field_no_leak() {
    // B-2026-07-15-11 (nested leg) — a non-generic `Outer { inner:
    // Box[String] }` and a generic `Gen[U] { inner: Box[U] }` recurse the
    // parent's drop through the nested `Box`. Before the fix that recursion
    // used the NAME-SHARED `Box` drop (no subst), leaking the bare-T field;
    // the fix threads the nested struct's own mono subst (derived from the
    // declared `Box[String]` field, resolving the parent's subst first)
    // through both the nested drop and the whole-parent move-suppression, so
    // scope-exit AND `let o2 = o` are leak- and double-free-clean.
    assert_clean_asan_run(
        r#"
struct Box[T] {
    v: T,
}
struct Outer {
    inner: Box[String],
}
struct Gen[U] {
    inner: Box[U],
}
fn main() {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 40 {
        let o = Outer { inner: Box { v: i.to_string() } };
        let o2 = o;
        acc = acc + o2.inner.v.len();
        let g = Gen { inner: Box { v: i.to_string() } };
        acc = acc + g.inner.v.len();
        i = i + 1;
    }
    println(acc);
}
"#,
        &["140"],
        "generic_wrapper_nested_struct_field_no_leak",
    );
}

#[test]
fn asan_generic_struct_field_receiver_param_indexed_no_leak() {
    // B-2026-07-15-20: the field-receiver method dispatch fix (record a
    // concrete generic-struct param's instantiation + an indexed-container
    // fallback) added new codegen paths for three receiver shapes that used
    // to loud-bail. Verify all three round-trip a heap `String` field
    // leak-clean under ASAN/LSan (the fix is in dispatch resolution, but the
    // synth field-element it registers drives the drop convention too, so a
    // mis-resolution would strand or double-free the field buffer):
    //   (1) `ref Box[String]` param receiver (`show(b: ref Box[String])` →
    //       `b.v.len()`; the arg stays owned by the caller and drops there),
    //   (2) OWNED `Box[String]` param receiver (`sink(b: Box[String])` →
    //       `b.v.len()`; the callee owns and drops the moved-in box),
    //   (3) INDEXED receiver over `Vec[Box[String]]` (`v[0].v.len()`; the
    //       Vec drops its Box element, which drops the String).
    // Single-field wrappers throughout so the (documented) B-2026-07-15-11
    // multi-field offset-erasure residual doesn't confound the signal. Loops
    // so any per-iteration strand accumulates into a visible LSan leak.
    assert_clean_asan_run(
        r#"
struct Box[T] {
    v: T,
}
fn show(b: ref Box[String]) -> i64 {
    b.v.len()
}
fn sink(b: Box[String]) -> i64 {
    b.v.len()
}
fn main() {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 40 {
        let r = Box { v: i.to_string() };
        acc = acc + show(r);
        let s = Box { v: i.to_string() };
        acc = acc + sink(s);
        let mut v: Vec[Box[String]] = Vec.new();
        v.push(Box { v: i.to_string() });
        acc = acc + v[0].v.len();
        i = i + 1;
    }
    println(acc);
}
"#,
        &["210"],
        "generic_struct_field_receiver_param_indexed_no_leak",
    );
}

#[test]
fn asan_partition_scalar_vecs_no_leak() {
    // B-2026-07-19-14: `partition` lowers to two fresh `Vec[T]` push
    // accumulators returned as a tuple. Even with a SCALAR element the two
    // Vec BUFFERS are heap allocations that must be freed — via the for-loop
    // consumers here (`for e in a` drops the moved Vec) AND, when a result is
    // only read (`.len()`), by scope-exit drop of the destructured tuple
    // halves. Loop many times so any per-iteration leaked buffer is an
    // unmistakable LSan finding.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 50 {
        let v = [1, 2, 3, 4, 5, 6, 7, 8];
        let (evens, odds): (Vec[i64], Vec[i64]) = v.iter().partition(|x| x % 2 == 0);
        for e in evens { acc = acc + e; }
        for o in odds { acc = acc + o; }
        // A partition whose halves are only measured (never iterated) — both
        // Vec buffers must be freed at scope exit.
        let (big, small): (Vec[i64], Vec[i64]) = v.iter().partition(|x| x > 4);
        acc = acc + big.len() + small.len();
        i = i + 1;
    }
    println(acc);
}
"#,
        &["2200"],
        "partition_scalar_vecs_no_leak",
    );
}

#[test]
fn asan_place_field_move_assign_overwrite_no_leak() {
    // B-2026-08-12-4 — the DISPLACED value of the assignment target.
    // `cur = stats[j].region` cap-zeroes the source so the element's owner
    // stops freeing it (B-2026-08-11-25), which leaves whatever `cur` held
    // BEFORE with no owner at all: `trigger_eager_free` classified every
    // other transferring RHS (moved alias, fresh ref, `mk().s` staging,
    // bare `v[i]`) but not a field moved out of a deeper place, so the old
    // buffer was orphaned once per execution.
    //
    // THE SIBLING FIXTURE ABOVE CANNOT CATCH THIS, which is why this one
    // exists rather than an extra line there. Its loop leaks exactly ONE
    // block, and LeakSanitizer did not report it: a single leaked pointer
    // left in a stale stack slot reads as still-reachable, so that fixture
    // was green under ASAN on a tree where valgrind reported `8 bytes in 1
    // blocks definitely lost` every run. The overwrite here runs 200 times
    // so ~199 blocks are orphaned at once — past any reachability
    // accident, and LSan reports it.
    //
    // The payload is built at run time and CONCATENATED so it is a real
    // heap buffer: a `String` literal field is static, and a missed free
    // on a static pointer is invisible.
    assert_clean_asan_run(
        r#"
struct S { region: String }
fn main() {
    let mut stats: Vec[S] = Vec.new();
    let mut i: i64 = 0;
    while i < 200 {
        let mut nm = String.new();
        nm.push_str("region-");
        nm.push_str("payload");
        stats.push(S { region: nm });
        i = i + 1;
    }
    let mut cur = String.new();
    let mut j: i64 = 0;
    while j < stats.len() {
        cur = stats[j].region;
        j = j + 1;
    }
    println(f"{cur.len()}");
}
"#,
        &["14"],
        "place_field_move_assign_overwrite_no_leak",
    );
}

#[test]
fn asan_repeated_place_field_move_assign_keeps_one_owner() {
    // B-2026-08-12-13 — the ALIASING half of the arm above. Reading the
    // same already-moved place twice used to leave the buffer with NO
    // owner: the first `cur = box[j].s` cap-zeroes the source and hands
    // `cur` the buffer, and the second reads a place that now aliases
    // `cur`, so the alias guard's neutralized header was stored back over
    // it and source and target both carried `cap == 0`. Neither freed it.
    // The guard now carries the target's own `cap` across, so `cur` stays
    // the single owner.
    //
    // TWO reads PER ELEMENT, not many reads of one: the leak is one buffer
    // per element that gets re-read, and re-reading a single element 100
    // times still leaks exactly one block — which LSan can miss as
    // still-reachable, the trap B-2026-08-12-4 fell into. 150 elements
    // read twice each leaks 150 blocks (2400 bytes measured pre-fix).
    //
    // The companion value pin is
    // `test_e2e_repeated_place_field_move_assign_reads_correctly`: this
    // fixture's `.len()` would read correctly off a dangling pointer, so
    // the content check lives there.
    assert_clean_asan_run(
        r#"
struct S { s: String }
fn main() {
    let mut box_: Vec[S] = Vec.new();
    let mut i: i64 = 0;
    while i < 150 {
        let mut nm = String.new();
        nm.push_str("region-");
        nm.push_str("payload");
        box_.push(S { s: nm });
        i = i + 1;
    }
    let mut cur = String.new();
    let mut j: i64 = 0;
    while j < box_.len() {
        cur = box_[j].s;
        cur = box_[j].s;
        j = j + 1;
    }
    println(f"{cur.len()}");
}
"#,
        &["14"],
        "repeated_place_field_move_assign_keeps_one_owner",
    );
}

#[test]
fn asan_struct_var_reassign_no_double_free() {
    // B-2026-07-16-18: reassigning a heap-owning STRUCT variable (`a = b`) never
    // suppressed the moved source `b`'s StructDrop, so both `a` (now holding b's
    // value) and `b` freed the same field buffers at scope exit — a double-free
    // (JIT aborts immediately; native masks it at -O but trips under
    // `karac_par_run`). The Assign arm handled Vec/String and Map/Set vars but had
    // no struct-var case. Fixed by suppressing the source's StructDrop
    // (`suppress_source_vec_cleanup_for_arg` → `zero_struct_move_caps`) for a
    // tracked-struct LHS. Exercises: a loop reassign (each iteration overwrites the
    // struct — a leak or double-free accumulates), a move-out-then-reassign 3-way
    // swap (`let tmp = a; a = b; b = tmp`), and a String-field struct. Loops so a
    // per-iteration strand shows as a large LSan leak.
    assert_clean_asan_run(
        r#"
struct Box { items: Vec[i64] }
struct Rec { name: String, id: i64 }
fn main() {
    let mut total: i64 = 0;
    let mut i: i64 = 0;
    while i < 100 {
        // loop reassign — the old value must be reclaimed each iteration
        let mut cur: Box = Box { items: [i] };
        let next: Box = Box { items: [i, i + 1, i + 2] };
        cur = next;
        total = total + cur.items.len();
        // move-out then reassign 3-way swap
        let mut a: Box = Box { items: [1, 2, 3] };
        let mut b: Box = Box { items: [4, 5] };
        let tmp: Box = a;
        a = b;
        b = tmp;
        total = total + a.items.len() + b.items.len();
        // String-field struct reassign
        let mut r: Rec = Rec { name: "first-long-enough-to-heap-alloc", id: 1 };
        let r2: Rec = Rec { name: "second-replacement-heap-string", id: 2 };
        r = r2;
        total = total + r.name.len();
        i = i + 1;
    }
    println(total);
}
"#,
        &["3800"],
        "struct_var_reassign_no_double_free",
    );
}

#[test]
fn asan_unwrap_or_moved_binding_default_no_double_free() {
    // B-2026-07-16-23 leg 1: `Option[T].unwrap_or(d)` where `d` is an OWNED
    // Vec/String binding DOUBLE-FREED — `unwrap_or` consumes (moves) `d`, but
    // the absent path bound a shallow copy of its {ptr,len,cap} to the result
    // (freed at scope) AND the binding `d` was freed at its own scope: two
    // frees of one buffer (native abort / JIT crash; interp fine — a
    // memory-unsafety divergence). Fixed by suppressing the moved binding's
    // scope-exit free (zero its cap, before the tag branch so both paths see
    // it) and freeing the loaded copy on the present path — so the buffer is
    // freed exactly once whichever way the branch goes. Gated to an
    // inline-owned Vec/String binding, so a `ref` binding / scalar default is
    // never touched. Loops 200× over both String and Vec owned-binding
    // defaults, mixing present/absent, so a double-free aborts immediately
    // and any residual per-iteration leak accumulates for LSan.
    assert_clean_asan_run(
        r#"
fn opt_s(i: i64) -> Option[String] {
    if i % 2 == 0 { Some("even-payload".to_string()) } else { None }
}
fn opt_v(i: i64) -> Option[Vec[i64]] {
    if i % 3 == 0 { Some([i, i + 1]) } else { None }
}
fn main() {
    let mut total: i64 = 0;
    let mut i: i64 = 0;
    while i < 200 {
        let ds = "string-fallback".to_string();
        let s = opt_s(i).unwrap_or(ds);
        total = total + (s.len() as i64);
        let dv: Vec[i64] = [7, 7, 7];
        let v = opt_v(i).unwrap_or(dv);
        total = total + (v.len() as i64);
        i = i + 1;
    }
    println(total);
}
"#,
        &["3233"],
        "unwrap_or_moved_binding_default_no_double_free",
    );
}

#[test]
fn asan_owned_self_direct_return_no_double_free() {
    // B-2026-07-17-3 (direct-return leg): an owned `self` receiver method
    // whose tail expression IS `self` (`fn ident(self) -> Bag { self }`)
    // double-freed the self struct's heap (Vec/String) field buffer when
    // non-empty — the tail-return move-out suppression resolved a plain
    // struct `Identifier` but returned early for `ExprKind::SelfValue`, so
    // self's callee-owned StructDrop and the returned value's owner both
    // freed the same buffer (native UAF via realloc / JIT abort; interp
    // correct). Fixed by resolving `SelfValue` to the `self` binding in
    // `suppress_source_vec_cleanup_for_arg_ex` (inline-owned-guarded, so a
    // `ref self` pointer is never touched). Loops 200× building a fresh
    // 2-element Vec each time and round-tripping it through the identity
    // method, so a per-call double-free aborts and any leak accumulates.
    // (The `let b = self; … b` REBIND form is a separate, deeper deep-copy-
    // interaction leg still tracked open under the same ledger id.)
    assert_clean_asan_run(
        r#"
struct Bag { items: Vec[i64] }
impl Bag {
    fn ident(self) -> Bag { self }
    fn count(ref self) -> i64 { self.items.len() }
}
fn main() {
    let mut total: i64 = 0;
    let mut i: i64 = 0;
    while i < 200 {
        let mut b = Bag { items: Vec.new() };
        b.items.push(i);
        b.items.push(i + 1);
        let b2 = b.ident();
        total = total + b2.count();
        i = i + 1;
    }
    println(total);
}
"#,
        &["400"],
        "owned_self_direct_return_no_double_free",
    );
}

#[test]
fn asan_owned_self_rebind_builder_chain_no_double_free() {
    // B-2026-07-17-3 (rebind leg): the common builder/fluent shape —
    // `fn add(self, p) -> Builder { let mut b = self; b.parts.push(p); b }`
    // chained. `let mut b = self` is the same struct move as `let g = f;`
    // (the deep-copied callee-owned aggregate is copied into `b`'s slot and
    // `b`'s StructDrop becomes the owner), but the Let-arm move-suppression
    // gated on `ExprKind::Identifier` and skipped `SelfValue` — self's
    // StructDrop stayed live and the SECOND chained call's push realloc'd a
    // freed buffer (native UAF, JIT "free(): double free"; interp correct).
    // The direct-return leg (`fn ident(self) -> T { self }`) was fixed in
    // 13eda85; this pins the rebind leg (call-site routing of SelfValue
    // into the same suppression helper). String-field + i64-field variants
    // both covered by the two structs.
    assert_clean_asan_run(
        r#"
struct Builder { parts: Vec[i64] }

impl Builder {
    fn add(self, p: i64) -> Builder {
        let mut b = self;
        b.parts.push(p);
        b
    }
}

struct Sb { text: String, n: i64 }

impl Sb {
    fn append(self, s: String) -> Sb {
        let mut b = self;
        b.text = s;
        b.n = b.n + 1;
        b
    }
}

fn main() {
    let b0 = Builder { parts: Vec.new() };
    let b3 = b0.add(1).add(2).add(3);
    println(b3.parts.len());
    let s0 = Sb { text: "start-with-a-heap-sized-string-payload", n: 0 };
    let s2 = s0.append("-a").append("-b");
    println(s2.n);
}
"#,
        &["3", "2"],
        "owned_self_rebind_builder_chain_no_double_free",
    );
}

#[test]
fn asan_while_condition_predicate_call_clone_arg_no_leak() {
    // Dogfood find (2026-07-18): a fresh owned/heap argument temp passed to
    // a predicate call inside a `while` CONDITION leaked one allocation per
    // iteration — an UNBOUNDED leak — because `compile_while` gave the loop
    // body a per-iteration cleanup frame but evaluated the condition with no
    // frame, so the `.clone()` arg temp landed in the enclosing scope's
    // frame (drained once, after the loop). The identical call in an `if`
    // condition was clean (the `if` sits inside a body frame drained each
    // iteration). Fixed by wrapping the while-condition in its own
    // per-iteration frame drained right after the guard is materialized; the
    // short-circuit `and`/`or` RHS (`n < K and f(v[i].clone())`) additionally
    // scopes its conditionally-created temps in `compile_short_circuit` so a
    // skip iteration doesn't re-drain a stale slot (double-free). Loops
    // enough to make a per-iteration leak or double-free unmistakable to
    // LSan. `keep` accumulates so the predicate result is observable.
    assert_clean_asan_run(
        r#"
fn wants(s: String, budget: i64) -> bool { budget > 0 }
fn ranks(cf: i64, cw: String, bf: i64, bw: String) -> bool {
    if cf != bf { cf > bf } else { cw < bw }
}
fn main() {
    let mut words: Vec[String] = Vec.new();
    words.push("alpha-heap-string".to_string());
    words.push("beta-heap-string".to_string());
    // Direct predicate call in a while condition (no short-circuit).
    let mut n = 60i64;
    let mut keep = 0i64;
    while wants(words[0].clone(), n) {
        keep = keep + 1;
        n = n - 1;
    }
    // Short-circuit `and`-RHS predicate call, sometimes skipped.
    let mut i = 0i64;
    let mut hits = 0i64;
    while i < 40 {
        if i % 3 == 0 and ranks(1, words[0].clone(), 0, words[1].clone()) {
            hits = hits + 1;
        }
        i = i + 1;
    }
    println(keep);
    println(hits);
}
"#,
        &["60", "14"],
        "while_condition_predicate_call_clone_arg_no_leak",
    );
}

// ── `#[derive(Clone)]` synthesized `.clone()` (B-2026-07-29-27 /
// B-2026-07-29-31) ───────────────────────────────────────────────
// Making `.clone()` callable on user structs / enums and on `Option[T]`
// puts a NEW deep-copy emitter on the hot path for heap-owning aggregates,
// and the failure mode of getting it wrong is invisible to an output
// assertion: a shallow bitcopy aliases the source's `{ptr,len,cap}`, both
// drops free it, and the program still prints the right answer. Only a
// sanitizer sees it (this is the B-2026-06-14-12 shape, and the class the
// Linux `memory-sanitizer` job exists to gate — macOS asan has no leak
// detection).
//
// Every clone here owns heap at a different depth: a struct with a String
// AND a `Vec[String]` field, an enum whose live variant carries a String,
// and an `Option[String]`. Looped so an under-freed copy accumulates into a
// definite LeakSanitizer report rather than a single stray block.
#[test]
fn asan_derived_clone_deep_copies_no_leak_no_double_free() {
    assert_clean_asan_run(
        r#"
#[derive(Clone)]
struct S { tag: String, items: Vec[String] }

#[derive(Clone)]
enum E { Empty, Payload(String) }

fn dup[T: Clone](x: T) -> T { x.clone() }

fn main() {
    let mut n = 0i64;
    let mut i = 0i64;
    while i < 200i64 {
        let mut a = S { tag: "src", items: Vec.new() };
        a.items.push("one");
        // The copy must own INDEPENDENT buffers: it grows, the source does not.
        let mut b = a.clone();
        b.items.push("two");
        n = n + a.items.len() + b.items.len();
        // Through a `T: Clone` generic body — the bound that used to be
        // decorative. `a` is moved in, so only `c` and `b` reach a drop.
        let c = dup(a);
        n = n + c.tag.len();

        let e = E.Payload("enum-heap");
        let f = e.clone();
        match f {
            E.Empty => {}
            E.Payload(t) => { n = n + t.len(); }
        }

        let o: Option[String] = Option.Some("opt-heap");
        let p = o.clone();
        n = n + p.unwrap().len();
        i = i + 1;
    }
    println(n);
}
"#,
        // per iteration: 1 + 2 (Vec lens) + 3 ("src") + 9 ("enum-heap")
        //              + 8 ("opt-heap") = 23; ×200 = 4600.
        &["4600"],
        "derived_clone_deep_copies_no_leak_no_double_free",
    );
}

/// B-2026-08-04-16 — a named heap value moved into a TUPLE ELEMENT is owned
/// once, by the tuple.
///
/// `compile_tuple_index_store` drops the old element and moves the RHS
/// header into the slot, but the assign arm never got the sibling of
/// B-2026-07-15-25's field-assign move-suppression, so the source binding
/// stayed armed while owning nothing. Both freed the same buffer. The
/// `Vec[String]` element was a triple free — the element buffer and the
/// Strings inside it.
///
/// The move-OUT the report led with is not required: a plain
/// `t.0 = <named source>` aborts identically, and that minimal spelling is
/// what runs here. The report's own `let mut e = t.0; …; t.0 = e;` form is
/// left out because the ownership checker warns on it (B-2026-08-04-18), so
/// it would trip this harness's ownership gate; it reaches the same
/// assignment arm regardless.
/// The last two shapes are the controls that localized it and must stay
/// clean — a fresh-temp RHS has no source binding to disarm, and the
/// struct-FIELD spelling has been correct since B-2026-07-15-25.
///
/// Seeded from `env.args().len()` with element and byte reads throughout so
/// the buffers are neither folded nor dead-stripped at `-O2`
/// (B-2026-08-04-17); ~1.9k allocations, floored well below that.
#[test]
fn asan_named_source_moved_into_a_tuple_element_is_disarmed() {
    assert_clean_asan_run_min_allocs(
        r#"
struct H { items: Vec[i64], n: i64 }
fn mkvec(k: i64) -> Vec[i64] { let mut v: Vec[i64] = Vec.new(); v.push(k); v.push(k + 1i64); return v; }
fn mkstr(k: i64) -> String { let mut s: String = String.new(); s.push_str(f"payload-{k}"); return s; }
fn digits(i: i64) -> String { let mut d: String = String.new(); d.push_str(f"{i}"); return d; }
fn mkvs(k: i64) -> Vec[String] { let mut v: Vec[String] = Vec.new(); v.push(mkstr(k)); return v; }
fn main() {
    let base: i64 = env.args().len();
    let mut acc = 0i64;
    let mut i = base;
    while i < base + 100i64 {
        // A NAMED source with no move-out — the minimal shape.
        let mut t2: (Vec[i64], i64) = (mkvec(i), 3i64);
        let f: Vec[i64] = mkvec(i + 1i64);
        t2.0 = f;
        acc = acc + t2.0[0i64];
        // Second element position.
        let mut t3: (i64, Vec[i64]) = (3i64, mkvec(i));
        let g: Vec[i64] = mkvec(i + 2i64);
        t3.1 = g;
        acc = acc + t3.1[0i64];
        // String element.
        let mut t4: (String, i64) = (mkstr(i), 3i64);
        let s: String = mkstr(i + 3i64);
        t4.0 = s;
        if t4.0.contains(digits(i + 3i64)) { acc = acc + t4.0.len(); }
        // Vec[String] element — the triple-free shape.
        let mut t5: (Vec[String], i64) = (mkvs(i), 3i64);
        let w: Vec[String] = mkvs(i + 4i64);
        t5.0 = w;
        if t5.0[0i64].contains(digits(i + 4i64)) { acc = acc + t5.0[0i64].len(); }
        // CONTROL: a fresh-temp RHS has no source binding to disarm.
        let mut t6: (Vec[i64], i64) = (mkvec(i), 3i64);
        t6.0 = mkvec(i + 5i64);
        acc = acc + t6.0[0i64];
        // CONTROL: the struct-FIELD spelling, correct since B-2026-07-15-25.
        let mut h: H = H { items: mkvec(i), n: 3i64 };
        let hv: Vec[i64] = mkvec(i + 6i64);
        h.items = hv;
        acc = acc + h.items[0i64];
        i = i + 1;
    }
    println(f"{acc}");
}
"#,
        &["23598"],
        "named_source_moved_into_a_tuple_element_is_disarmed",
        500,
    );
}

#[test]
fn asan_module_binding_container_for_loop_no_leak() {
    // B-2026-07-31-30 made these loops LIVE for the first time: the
    // `compile_for` Identifier arm used to skip module-level containers
    // entirely and emit a zero-iteration loop, so the Vec/Map/Set
    // iteration machinery never ran on a global. Iterating a Map allocates
    // a runtime iterator (`karac_map_iter_new`/`_next`/`_free`) whose
    // scope-exit cleanup is what keeps an early `return` out of the body
    // leak-free, and each String key/element yielded is a fresh
    // allocation — none of which was exercised against a global before.
    //
    // Covers all four container arms plus the two shapes most likely to
    // strand the iterator or the yielded elements: an early `return` out
    // of the middle of a module-Map loop, and a `break` out of a
    // module-Set loop.
    assert_clean_asan_run(
        r#"
let mut MV: Vec[String] = Vec.new();
let mut MM: Map[String, i64] = Map.new();
let mut MS: Set[String] = Set.new();
let TITLE: StringSlice = "hi";

fn early_return_out_of_map_loop() -> i64 {
    for (k, v) in MM {
        if v == 2 {
            return v;
        }
    }
    0
}

fn main() {
    MV.push("alpha"); MV.push("beta");
    MM.insert("a", 1); MM.insert("b", 2);
    MS.insert("p"); MS.insert("q");

    let mut n = 0;
    for s in MV { n = n + s.len(); }
    println(n);

    let mut total = 0;
    for (k, v) in MM { total = total + v + k.len(); }
    println(total);

    let mut seen = 0;
    for s in MS {
        seen = seen + 1;
        if seen == 1 { break; }
    }
    println(seen);

    let mut chars = 0;
    for c in TITLE { chars = chars + 1; }
    println(chars);

    println(early_return_out_of_map_loop());
}
"#,
        // MV: "alpha"(5) + "beta"(4) = 9.
        // MM: (1+2) values + (1+1) key lens = 5.
        // MS: breaks after the first element = 1.
        // TITLE: "hi" = 2 chars.  early return finds v == 2.
        &["9", "5", "1", "2", "2"],
        "module_binding_container_for_loop_no_leak",
    );
}

/// B-2026-08-31-44 — the half of B-2026-08-29-32's freshness guard that
/// post-`2b3b9668` `main` no longer needs.
///
/// That guard declined EVERY identifier and EVERY place as a possible
/// alias. Re-measured under LSan, the population splits in two: a BARE
/// BINDING and a non-field place no longer alias — the whole-value move
/// already retracts the source's cleanup, so declining them registered no
/// owner at all and stranded 38 B per evaluation — while a FIELD
/// PROJECTION still does, and stays declined (its leak is filed
/// separately). Only the binding/index half is admitted here.
///
/// The statement forms are load-bearing, and the guard's own history says
/// why: a source dies at its NLL last use when nothing follows it, so a
/// one-statement program cannot tell a missing owner from a correct one.
/// Each shape is therefore measured SOLO (nothing after), STACKED (a
/// second discard after), and in a LOOP. The stacked form turned out to be
/// the one already CLEAN before this change — the leak lives in the other
/// two — which is the opposite of what the guard was tuned against, and is
/// why all three are pinned rather than the stacked one alone.
#[test]
fn asan_discarded_branch_literal_over_a_binding_frees_once() {
    // The ADMITTED half, in every discard spelling and all three statement
    // forms. Each cell stranded 38 B per evaluation before the narrowing —
    // 722 B over the 19 leaking iterations of the loop cell.
    assert_clean_asan_run_min_allocs(
        r#"
struct P { a: String, b: i64 }
fn seed() -> i64 { env.args().len() }
fn payload() -> String { f"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa" }
fn go() -> i64 {
    let s1 = payload();
    if seed() > 0 { P { a: s1, b: 1 } } else { P { a: payload(), b: 2 } };
    let s2 = payload();
    let _ = if seed() > 0 { P { a: s2, b: 1 } } else { P { a: payload(), b: 2 } };
    let s3 = payload();
    match seed() { 1 => { P { a: s3, b: 1 } } _ => { P { a: payload(), b: 2 } } };
    let s4 = payload();
    let _ = match seed() { 1 => { P { a: s4, b: 1 } } _ => { P { a: payload(), b: 2 } } };
    let mut v: Vec[String] = Vec.new();
    v.push(payload());
    if seed() > 0 { P { a: v[0], b: 1 } } else { P { a: payload(), b: 2 } };
    let mut i = 0i64;
    while i < 20i64 {
        let s = payload();
        if seed() > 0 { P { a: s, b: 1 } } else { P { a: payload(), b: 2 } };
        i = i + 1;
    }
    return 1;
}
fn main() { println(go()); }
"#,
        &["1"],
        "discarded_branch_literal_over_a_binding_frees_once",
        40,
    );
    // The RISK shapes for the newly-admitted half — every way the
    // registration could free something someone else still owns or reads.
    // The arm NOT taken is the sharpest: the source is never consumed, so
    // its own cleanup has to remain the one that frees it.
    assert_clean_asan_run_min_allocs(
        r#"
struct P { a: String, b: i64 }
fn seed() -> i64 { env.args().len() }
fn payload() -> String { f"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa" }
fn go() -> i64 {
    let n = payload();
    if seed() > 99i64 { P { a: n, b: 1 } } else { P { a: payload(), b: 2 } };
    let b1 = payload();
    let b2 = payload();
    if seed() > 0 { P { a: b1, b: 1 } } else { P { a: b2, b: 2 } };
    let mut r = payload();
    if seed() > 0 { P { a: r, b: 1 } } else { P { a: payload(), b: 2 } };
    r = payload();
    let outer = payload();
    let mut i = 0i64;
    while i < 5i64 {
        if seed() > 0 { P { a: outer, b: 1 } } else { P { a: payload(), b: 2 } };
        i = i + 1;
    }
    return r.len() - r.len() + 1;
}
fn main() { println(go()); }
"#,
        &["1"],
        "discarded_branch_literal_over_a_binding_risk_shapes",
        20,
    );
}

#[test]
fn asan_discarded_branch_literal_field_over_a_loop_outer_local_declines() {
    assert_clean_asan_run_min_allocs(
        r#"
struct P { a: String, b: i64 }
fn seed() -> i64 { env.args().len() }
fn payload() -> String { f"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa" }
fn mkp(n: i64) -> P { return P { a: payload(), b: n }; }
fn go() -> i64 {
    let t = mkp(9);
    let mut i = 0i64;
    while i < 5i64 {
        if seed() > 0 { P { a: t.a, b: 1 } } else { P { a: payload(), b: 2 } };
        i = i + 1;
    }
    return 1;
}
fn main() { println(go()); }
"#,
        &["1"],
        "discarded_branch_literal_field_over_a_loop_outer_local_declines",
        10,
    );
}

/// B-2026-09-01-5 — a DISCARDED aggregate literal whose field PROJECTS off
/// a named local (`P { a: t.a, b: 1 }`) disarmed the source and registered
/// no owner, stranding the buffer.
///
/// The measurement that fixes the shape of the fix: the projected field is
/// an ALIAS, not a clone and not a mint. `let t = mkp(9); P { a: t.a, b: 1
/// };` allocates exactly what `let t = mkp(9);` alone allocates — 16 in
/// both — and frees one fewer. So nothing is owed a takeover; the disarm
/// simply ran with no consumer to hand the buffer to, and declining it is
/// the whole fix. That retires the double-free hazard the row records
/// (a static one-shot retraction against a per-iteration move), because
/// with no retraction there is nothing to fire once.
///
/// Both discard spellings, because they reach DIFFERENT windows: the branch
/// arm's tail (B-2026-09-07-14's) and the bare statement's own.
#[test]
fn asan_discarded_literal_projected_field_keeps_its_owner() {
    const H: &str = "struct P { a: String, b: i64 }\n\
             struct N { inner: P }\n\
             fn seed() -> i64 { env.args().len() }\n\
             fn payload() -> String { f\"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa\" }\n\
             fn mkp(n: i64) -> P { return P { a: payload(), b: n }; }\n\
             fn mkn(n: i64) -> N { return N { inner: mkp(n) }; }\n\
             fn main() { println(go()); }\n";
    let rows: &[(&str, &str)] = &[
            (
                "branch arm, projecting arm taken",
                "fn go() -> i64 { let t = mkp(9);\n\
                 \x20 if seed() > 0 { P { a: t.a, b: 1 } } else { P { a: payload(), b: 2 } };\n\
                 \x20 1 }\n",
            ),
            // The projecting arm is NOT taken here, and the row records the
            // sharpest half of the defect: the mere PRESENCE of a projecting
            // arm cost the sibling arm its owner, so the decline was
            // per-construct rather than per-path.
            (
                "branch arm, projecting arm NOT taken",
                "fn go() -> i64 { let t = mkp(9);\n\
                 \x20 if seed() > 99i64 { P { a: t.a, b: 1 } } else { P { a: payload(), b: 2 } };\n\
                 \x20 1 }\n",
            ),
            (
                "branch arm, no else",
                "fn go() -> i64 { let t = mkp(9);\n\
                 \x20 if seed() > 0 { P { a: t.a, b: 1 } };\n\
                 \x20 1 }\n",
            ),
            (
                "branch arm, a statement follows",
                "fn go() -> i64 { let t = mkp(9);\n\
                 \x20 if seed() > 0 { P { a: t.a, b: 1 } } else { P { a: payload(), b: 2 } };\n\
                 \x20 let z = payload();\n\
                 \x20 z.len() - z.len() + 1 }\n",
            ),
            (
                "NESTED projection off a named local",
                "fn go() -> i64 { let n = mkn(9);\n\
                 \x20 if seed() > 0 { P { a: n.inner.a, b: 1 } } else { P { a: payload(), b: 2 } };\n\
                 \x20 1 }\n",
            ),
            // The bare STATEMENT spelling — a different window from the arm's.
            (
                "bare statement literal",
                "fn go() -> i64 { let t = mkp(9);\n\
                 \x20 P { a: t.a, b: 1 };\n\
                 \x20 1 }\n",
            ),
            (
                "bare statement literal inside a block",
                "fn go() -> i64 { let t = mkp(9);\n\
                 \x20 { P { a: t.a, b: 1 }; }\n\
                 \x20 1 }\n",
            ),
            // The LOOP cells. `t` inside the loop is a fresh value per
            // iteration; `t` OUTSIDE it is the cell the row could not close and
            // the one `tests/asan-o0-known-failures.txt` quarantined.
            (
                "loop, source declared INSIDE",
                "fn go() -> i64 { let mut i = 0i64;\n\
                 \x20 while i < 5i64 { let t = mkp(9);\n\
                 \x20   if seed() > 0 { P { a: t.a, b: 1 } } else { P { a: payload(), b: 2 } };\n\
                 \x20   i = i + 1; }\n\
                 \x20 1 }\n",
            ),
            // B-2026-09-07-38 — the two loop-outer-local spellings that this
            // row's decline stranded once B-2026-09-07-23 taught the same
            // projection to COPY. The decline was measured on an ALIAS ("16
            // allocations in both, and one fewer free"); an RC-promoted root
            // mints a fresh buffer per trip instead, so the literal has to own
            // what it carries.
            //
            // The IF/ELSE spelling of this shape is deliberately NOT here. It
            // still leaks, it already has a standalone test of its own
            // (`asan_discarded_branch_literal_field_over_a_loop_outer_local_declines`),
            // and that test carries the quarantine entry — which is the whole
            // point of keeping these two out of it: quarantining the function
            // they lived in would have taken them off the leg with it.
            (
                "loop, source declared OUTSIDE, no else",
                "fn go() -> i64 { let t = mkp(9); let mut i = 0i64;\n\
                 \x20 while i < 5i64 {\n\
                 \x20   if seed() > 0 { P { a: t.a, b: 1 } };\n\
                 \x20   i = i + 1; }\n\
                 \x20 1 }\n",
            ),
            (
                "loop, source declared OUTSIDE, read after the loop",
                "fn go() -> i64 { let t = mkp(9); let mut i = 0i64;\n\
                 \x20 while i < 3i64 { P { a: t.a, b: 1 }; i = i + 1; }\n\
                 \x20 t.a.len() - t.a.len() + 1 }\n",
            ),
            // ── guards: a second owner here would be a DOUBLE FREE ────────
            // The READ-AFTER guard moved out to its own `#[test]` below
            // (B-2026-09-07-38): it is the IF/ELSE spelling, which still
            // strands the copy, and it sat AFTER the loop cell in this array —
            // so `assert_clean_asan_run` never reached it while that cell was
            // failing. Two failing cells, one visible. Splitting it keeps the
            // twelve sound cells here ON the -O0 leg.
            (
                "guard: the literal is BOUND, so the binding owns it",
                "fn go() -> i64 { let t = mkp(9);\n\
                 \x20 let p = P { a: t.a, b: 1 };\n\
                 \x20 p.a.len() - p.a.len() + 1 }\n",
            ),
            // ── controls, clean before and after ──────────────────────────
            (
                "control: a BARE BINDING field, not a projection",
                "fn go() -> i64 { let s = payload();\n\
                 \x20 if seed() > 0 { P { a: s, b: 1 } } else { P { a: payload(), b: 2 } };\n\
                 \x20 1 }\n",
            ),
            (
                "control: both arms MINT",
                "fn go() -> i64 {\n\
                 \x20 if seed() > 0 { P { a: payload(), b: 1 } } else { P { a: payload(), b: 2 } };\n\
                 \x20 1 }\n",
            ),
        ];
    for (label, body) in rows {
        assert_clean_asan_run(&format!("{H}{body}"), &["1"], label);
    }
}

/// B-2026-09-07-38 — the IF/ELSE spelling of
/// [`Self::asan_discarded_literal_projected_field_keeps_its_owner`]'s
/// projection, which still strands the copied buffer.
///
/// QUARANTINED at -O0 with the row that owns it, alongside
/// `asan_discarded_branch_literal_field_over_a_loop_outer_local_declines`
/// — the same defect at a different size (that one 190 B in 5 blocks over
/// five trips, this one 38 B in 1).
///
/// It was a CELL of the fixture above until this commit, and an invisible
/// one: it sits after the loop cell, which was already failing, so the
/// helper panicked before reaching it. Measured 38 B in 1 block both with
/// and without the fix that landed here, i.e. pre-existing and merely
/// uncovered.
///
/// Reading `t.a` after the discard is what promotes `t`, so the projection
/// COPIES (B-2026-09-07-23) exactly as the loop spelling's does — and the
/// discarded literal's registrar is not reached through an `if/else`
/// construct, so nothing owns the copy.
#[test]
fn asan_discarded_literal_projected_field_if_else_read_after_declines() {
    assert_clean_asan_run(
        "struct P { a: String, b: i64 }\n\
             fn seed() -> i64 { env.args().len() }\n\
             fn payload() -> String { f\"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa\" }\n\
             fn mkp(n: i64) -> P { return P { a: payload(), b: n }; }\n\
             fn main() { println(go()) }\n\
             fn go() -> i64 { let t = mkp(9);\n\
             \x20 if seed() > 0 { P { a: t.a, b: 1 } } else { P { a: payload(), b: 2 } };\n\
             \x20 t.a.len() - t.a.len() + 1 }\n",
        &["1"],
        "discarded_literal_projected_field_if_else_read_after",
    );
}

/// B-2026-09-07-21 cell 2 — a `.clone()` field in a discarded STATEMENT
/// literal, which leaks on EVERY surface rather than in one lane.
///
/// A `.clone()` MINTS: the literal holds its own fresh buffer and the
/// source keeps its own, which is exactly the population the discard
/// registrar is meant to own. It did not, and the row could not say
/// whether the registrar was never entered or declined further in. It
/// declines further in: `discard_tuple_elem_is_fresh_expr` had arms for
/// literals, tuples and `Call` but NONE for `MethodCall`, so the field
/// fell to the catch-all, failed the scalar test, and one non-fresh field
/// disqualified the whole literal.
///
/// Both lanes are asserted because the row measured this one on every
/// surface, unlike cell 1.
#[test]
fn asan_discarded_literal_cloned_field_keeps_its_owner() {
    const SRC: &str = "struct P { a: String, b: i64 }\n\
             fn seed() -> i64 { env.args().len() }\n\
             fn payload() -> String { f\"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa\" }\n\
             fn mkp(n: i64) -> P { return P { a: payload(), b: n }; }\n\
             fn main() { println(go()); }\n\
             fn go() -> i64 { let t = mkp(9);\n\
             \x20 P { a: t.a.clone(), b: 1 };\n\
             \x20 t.a.len() - t.a.len() + 1 }\n";
    assert_clean_asan_run(SRC, &["1"], "discarded_literal_cloned_field");
    assert_clean_asan_run_seq_lane(SRC, &["1"], "discarded_literal_cloned_field_seq");
}

/// B-2026-09-01-24 — a SCALAR field read of a live local inside a discarded
/// literal declined the all-fresh gate, so the literal registered no owner
/// and every MINTED object leaked: 76 B / 2 allocations, which is how this
/// was found (it was written as a CONTROL in B-2026-09-01-21's ASAN test
/// and failed, on unmodified `main` too).
///
/// Unlike that row's admission this one is a COPY — a scalar read moves
/// nothing — so there is no retraction to get wrong and no double-free
/// direction to guard. What the guards below pin instead is that admitting
/// the projection did not disturb the shapes that already had owners.
#[test]
fn asan_a_scalar_projection_field_leaves_the_literal_owned() {
    const H: &str = "struct R { id: i64, s: String }\n\
             struct Inner { n: i64 }\n\
             struct Outer { inner: Inner, m: i64 }\n\
             struct S2 { r: R, s: R, k: i64 }\n\
             fn seed() -> i64 { env.args().len() }\n\
             fn payload() -> String { f\"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa\" }\n\
             fn mk(i: i64) -> R { return R { id: i, s: payload() }; }\n\
             fn main() { println(go()); }\n";
    let rows: &[(&str, &str)] = &[
        (
            "scalar field read of a live local, bare statement",
            "fn go() -> i64 { let t = mk(7);\n\
                 \x20  S2 { r: mk(1), s: mk(9), k: t.id };\n\
                 \x20  1 }\n",
        ),
        (
            "same, wildcard `let`",
            "fn go() -> i64 { let t = mk(7);\n\
                 \x20  let _ = S2 { r: mk(1), s: mk(9), k: t.id };\n\
                 \x20  1 }\n",
        ),
        (
            "same, behind a block wrapper",
            "fn go() -> i64 { let t = mk(7);\n\
                 \x20  { S2 { r: mk(1), s: mk(9), k: t.id } };\n\
                 \x20  1 }\n",
        ),
        (
            "DEPTH-1 projection of a non-`Drop` struct",
            "fn go() -> i64 { let o = Outer { inner: Inner { n: 3 }, m: 4 };\n\
                 \x20  S2 { r: mk(1), s: mk(9), k: o.m };\n\
                 \x20  1 }\n",
        ),
        (
            "NESTED projection through the object",
            "fn go() -> i64 { let o = Outer { inner: Inner { n: 3 }, m: 4 };\n\
                 \x20  S2 { r: mk(1), s: mk(9), k: o.inner.n };\n\
                 \x20  1 }\n",
        ),
        // ── guards: these already had owners and must keep exactly one ──
        (
            "guard: an INDEX of a scalar element, clean throughout",
            "fn go() -> i64 { let v = [5, 6, 7];\n\
                 \x20  S2 { r: mk(1), s: mk(9), k: v[0] };\n\
                 \x20  1 }\n",
        ),
        (
            "guard: the same read HOISTED into its own `let`",
            "fn go() -> i64 { let t = mk(7); let n = t.id;\n\
                 \x20  S2 { r: mk(1), s: mk(9), k: n };\n\
                 \x20  1 }\n",
        ),
        (
            "guard: the BOUND `let` is owned by its binding",
            "fn go() -> i64 { let t = mk(7);\n\
                 \x20  let w = S2 { r: mk(1), s: mk(9), k: t.id };\n\
                 \x20  w.k - w.k + 1 }\n",
        ),
    ];
    for (label, body) in rows {
        assert_clean_asan_run(&format!("{H}{body}"), &["1"], label);
    }
}

/// B-2026-09-01-22 — a discarded struct literal behind **TWO OR MORE**
/// block wrappers registered ONE OWNER PER WRAPPER, so its consumed local's
/// heap was freed once per nesting level: a double free at depth 2 and a
/// triple at depth 3, on both compiled backends.
///
/// `compile_block_with_frame`'s `stmt_owns_block_tail` guard asked
/// `discarded_owned_literal_tail` while the two statement-discard sites had
/// moved on to the wider `discarded_movable_literal_tail` (B-2026-09-01-21),
/// so the guard reported "the statement does not own this" about a value
/// the statement had already taken AND retracted the source for. Every
/// enclosing block then armed the arm-discard leg over the same value.
///
/// Unlike most of this family's rows this is a DOUBLE FREE rather than a
/// leak, so it reproduces at every optimization level — the freed buffer is
/// live and reachable, not dead code LLVM can delete. That is also why the
/// row was found through its `Drop`-body transcript (`dR7 dR7`) before its
/// memory: the extra body is the visible half of the extra free.
///
/// The one-wrapper depth is the CONTROL and belongs here: it was correct
/// throughout, because its tail is the literal itself and the arm-discard
/// leg declines a bare literal with a non-fresh field.
#[test]
fn asan_a_deeply_wrapped_discarded_literal_frees_once() {
    const H: &str = "struct R { id: i64, s: String }\n\
             struct S { r: R, k: i64 }\n\
             struct T2 { r: R, s: R }\n\
             fn seed() -> i64 { env.args().len() }\n\
             fn payload() -> String { f\"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa\" }\n\
             fn mk(i: i64) -> R { return R { id: i, s: payload() }; }\n\
             fn main() { println(go()); }\n";
    let rows: &[(&str, &str)] = &[
        (
            "TWO wrappers, wildcard `let` — the double free",
            "fn go() -> i64 { let t = mk(7);\n\
                 \x20  let _ = { { S { r: t, k: 1 } } };\n\
                 \x20  1 }\n",
        ),
        (
            "TWO wrappers, bare statement",
            "fn go() -> i64 { let t = mk(7);\n\
                 \x20  { { S { r: t, k: 1 } } };\n\
                 \x20  1 }\n",
        ),
        (
            "THREE wrappers — a triple free before the fix",
            "fn go() -> i64 { let t = mk(7);\n\
                 \x20  let _ = { { { S { r: t, k: 1 } } } };\n\
                 \x20  1 }\n",
        ),
        (
            "TWO wrappers over the TUPLE leg, which shares the predicate",
            "fn go() -> i64 { let t = mk(7);\n\
                 \x20  let _ = { { (t, 20) } };\n\
                 \x20  1 }\n",
        ),
        (
            "TWO wrappers, a moved source beside a MINTED sibling",
            "fn go() -> i64 { let t = mk(7);\n\
                 \x20  let _ = { { T2 { r: t, s: mk(9) } } };\n\
                 \x20  1 }\n",
        ),
        // ── controls: already correct, and must stay owned exactly once ──
        (
            "control: the ONE-wrapper depth, correct throughout",
            "fn go() -> i64 { let t = mk(7);\n\
                 \x20  let _ = { S { r: t, k: 1 } };\n\
                 \x20  1 }\n",
        ),
        (
            "control: an ALL-MINTED literal, two wrappers",
            "fn go() -> i64 {\n\
                 \x20  let _ = { { S { r: mk(7), k: 1 } } };\n\
                 \x20  1 }\n",
        ),
        (
            "control: a CALL tail, two wrappers",
            "fn go() -> i64 {\n\
                 \x20  let _ = { { mk(9) } };\n\
                 \x20  1 }\n",
        ),
        (
            "guard: the BOUND `let` still owns its value at depth",
            "fn go() -> i64 { let t = mk(7);\n\
                 \x20  let w = { { S { r: t, k: 1 } } };\n\
                 \x20  w.k }\n",
        ),
        (
            "guard: a bound `let` reading the minted sibling back, deeper",
            "fn go() -> i64 { let t = mk(7);\n\
                 \x20  let w = { { { T2 { r: t, s: mk(9) } } } };\n\
                 \x20  w.s.id - 8 }\n",
        ),
    ];
    for (label, body) in rows {
        assert_clean_asan_run(&format!("{H}{body}"), &["1"], label);
    }
}

/// B-2026-08-29-25 — the MEMORY half of the discarded-`if` and
/// block-wrapper fix, with HEAP-CARRYING payloads.
///
/// The row that filed this called for the check explicitly: routing a
/// value into the discard battery FREES it as well as running its body,
/// so a gate widening is exactly the change that can turn a missing body
/// into a double free. Every shape this row newly admits carries a
/// `String` here, and the enclosing-local guard is included because it is
/// the one the gate must keep DECLINING — a fire there would free a
/// buffer the local still owns.
#[test]
fn asan_discarded_if_and_block_wrapped_rhs_free_once() {
    assert_clean_asan_run(
        r#"
struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id} {self.name}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"heap-{i}" }; }
fn main() {
    let n = 1;
    let _ = if n == 1 { R { id: 7, name: f"lit-seven" } } else { mk(0) };
    if n == 1 { mk(3) } else { mk(0) };
    let _ = if n == 2 { mk(1) } else if n == 1 { mk(4) } else { mk(0) };
    println("dropped");
}
"#,
        &["dR7 lit-seven", "dR3 heap-3", "dR4 heap-4", "dropped"],
        "discarded_if",
    );
    assert_clean_asan_run(
        r#"
struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id} {self.name}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"heap-{i}" }; }
fn main() {
    let n = 1;
    let _ = { match n { 1 => { mk(5) } _ => { mk(0) } } };
    { match n { 1 => { mk(6) } _ => { mk(0) } } };
    let _ = { mk(8) };
    println("dropped");
}
"#,
        &["dR5 heap-5", "dR6 heap-6", "dR8 heap-8", "dropped"],
        "discarded_block_wrapped",
    );
    // A block-wrapped `if` is the shape where the two mechanisms are most
    // likely to disagree about who owns the value: the statement gate
    // recurses through the wrapper to reach the `if`, while `compile_if`
    // decides whether to arm B-2026-08-29-5's arm-level owner from the
    // condition alone and never sees the wrapper. One free, either way.
    assert_clean_asan_run(
        r#"
struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id} {self.name}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"heap-{i}" }; }
fn main() {
    let n = 1;
    let _ = { if n == 1 { mk(1) } else { mk(0) } };
    { if n == 1 { mk(2) } else { mk(0) } };
    let _ = if n == 1 { { mk(3) } } else { { mk(0) } };
    println("dropped");
}
"#,
        &["dR1 heap-1", "dR2 heap-2", "dR3 heap-3", "dropped"],
        "discarded_block_wrapped_if",
    );
    // The shape `test_ir_discarded_branching_tail_temp_is_tracked` now
    // asserts IS tracked: a branching discard tail whose every arm is a
    // fresh `Vec` call. Slice 5 left this untracked as "a safe leak,
    // deferred"; it is freed now, and this is the check that freeing it
    // is a single free and not the double the old conservatism feared.
    assert_clean_asan_run(
        r#"
fn make_vec(n: i64) -> Vec[i64] {
    let mut v: Vec[i64] = Vec.new();
    v.push(n);
    return v;
}
fn main() {
    let cond = true;
    { if cond { make_vec(1) } else { make_vec(2) } }
    let _ = if cond { make_vec(3) } else { make_vec(4) };
    println("dropped");
}
"#,
        &["dropped"],
        "discarded_branching_tail_vec",
    );
    // The shape the gate must keep declining: the branch hands out an
    // enclosing local. `r`'s buffer is still the local's, and a discard
    // fire would free it a second time — ASAN is the check that it does
    // not, and it passes.
    //
    // The body reads `name`, which is what makes this a content check and
    // not just a count. Against a tree WITHOUT B-2026-08-29-5's fix this
    // printed `dR41 ` on both compiled backends against the interpreter's
    // `dR41 forty-one`: the local's String was moved into the discarded
    // value and freed there, and the scope-exit body then read the
    // emptied field. That fix landed while this row was in flight and the
    // three backends agree now — pinned here so a regression shows up as
    // a content mismatch rather than a silent one.
    assert_clean_asan_run(
        r#"
struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id} {self.name}") } }
fn main() {
    let r = R { id: 41, name: f"forty-one" };
    let n = 0;
    if n == 0 { r } else { R { id: 9, name: f"nine" } };
    println("end");
}
"#,
        &["dR41 forty-one", "end"],
        "discarded_if_enclosing_local",
    );
}

/// B-2026-08-29-19 — the MEMORY half of the wrapped-param-view double body,
/// with a HEAP-CARRYING payload.
///
/// `let w = W.One(r);` over an owned param ran `R`'s `Drop` body twice on
/// every backend. The row that filed it explicitly left open whether the
/// frees were balanced too — "a heap-carrying payload should be re-measured
/// before assuming the memory side is balanced" — and they are: this fixture
/// was ASAN-clean BEFORE the fix as well as after, so the defect was only
/// ever a body count. That is worth pinning rather than merely recording,
/// because the obvious reading of the shape says otherwise: the callee's
/// `w` and the caller's entry copy hold the same String buffer, which looks
/// exactly like a double free and is not one.
///
/// `name` is read BYTE-WISE in the body for this file's usual reason —
/// a `Drop` that never touches the field lets `-O2` delete the allocation
/// and both of its frees, which would hide a real double free here.
///
/// Case 2 keeps the MIXED payload shape (B-2026-08-29-24) under the
/// sanitizer: it still runs one body too many, and this asserts that the
/// B-2026-08-29-46 — reversing the interpreter's fresh-temp argument walk
/// moved a BODY, and this proves it moved no FREE.
///
/// Both argument temporaries carry heap, so a change that reordered the
/// cleanup registration rather than just the body walk would surface here as
/// a leak or a double free instead of as an output-order difference. The
/// expected lines are the compiled backends' order (`dR2` then `dR1`), which
/// is what the fix made `--interp` agree with.
#[test]
fn asan_owned_param_temps_free_once() {
    assert_clean_asan_run(
        r#"
struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id} {self.name}") } }
fn take(r: R, q: R) -> i64 { 7 }
fn main() {
    let v = take(R { id: 1, name: f"heap-one" }, R { id: 2, name: f"heap-two" });
    println(f"v={v}");
}
"#,
        &["dR2 heap-two", "dR1 heap-one", "v=7"],
        "owned_param_temps_two",
    );
    assert_clean_asan_run(
        r#"
struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id} {self.name}") } }
fn take3(a: R, b: R, c: R) -> i64 { 7 }
fn main() {
    let v = take3(
        R { id: 1, name: f"heap-one" },
        R { id: 2, name: f"heap-two" },
        R { id: 3, name: f"heap-three" },
    );
    println(f"v={v}");
}
"#,
        &["dR3 heap-three", "dR2 heap-two", "dR1 heap-one", "v=7"],
        "owned_param_temps_three",
    );
    // The MIXED shape, where the rule is program-order of introduction
    // rather than argument position: the local is introduced first and so
    // pops last, giving a FORWARD print. Included here because it is the
    // case whose memory is split across two different owners — the caller's
    // binding and the argument cleanup frame — which is exactly where a
    // mis-registration would show as a double free.
    assert_clean_asan_run(
        r#"
struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id} {self.name}") } }
fn take(r: R, q: R) -> i64 { 7 }
fn main() {
    let b = R { id: 2, name: f"heap-two" };
    let v = take(R { id: 1, name: f"heap-one" }, b);
    println(f"v={v}");
}
"#,
        &["dR1 heap-one", "dR2 heap-two", "v=7"],
        "owned_param_temps_mixed",
    );
    // B-2026-08-29-54 — a STATIC (associated) function's by-value arguments
    // had no caller-side owner at all, so both `Drop` bodies were missing
    // AND both payload buffers were orphaned. Now both bodies run and the
    // memory is clean.
    //
    // THIS CASE USED TO ASSERT THE OPPOSITE, and the way it went wrong is
    // worth keeping. It expected `["v=7"]` — no bodies — and graded the row
    // "a lost body, not a leak" on the strength of a clean LSan run. The
    // clean run was an artifact: this harness builds at `-O2`, where the
    // only consumer of these two allocations is a callee that ignores its
    // parameters, so LLVM deletes them and LSan is handed a program with
    // nothing to lose. Rebuilt at `-O0` the same source leaked 16 bytes in
    // 2 blocks before the fix and is clean after it. That is precisely the
    // population B-2026-08-04-17 named — "fixtures that allocate nothing at
    // `-O2` and so assert nothing" — reached this time through a fixture
    // that looked like it carried heap on purpose.
    //
    // So it takes a hard allocation FLOOR now rather than the plain
    // assertion: a fixture whose whole point is that two heap payloads get
    // freed must fail loudly if it is ever optimized back down to allocating
    // nothing, instead of quietly passing while proving it.
    assert_clean_asan_run_min_allocs(
        r#"
struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id} {self.name}") } }
struct H { n: i64 }
impl H { fn s2(a: R, b: R) -> i64 { 7 } }
fn main() {
    let v = H.s2(R { id: 1, name: f"heap-one" }, R { id: 2, name: f"heap-two" });
    println(f"v={v}");
}
"#,
        &["dR2 heap-two", "dR1 heap-one", "v=7"],
        "static_method_args_run_bodies_and_free",
        // 5 = the two `name` payloads, the two `Drop` bodies' f-strings, and
        // `f"v={v}"` — every allocation this program makes, measured, and the
        // same on both hosts. The 8 was an estimate; see B-2026-09-07-26.
        // The point of the floor is unchanged: at 0 the two payloads have
        // been optimized away again and the fixture proves nothing.
        5,
    );
}

/// B-2026-08-10-21 under ASAN — the `UseAfterMove` defensive copy, and the
/// first fixture of that mechanism the suite has ever been able to hold.
///
/// It could not exist before this bug's fix: `tests/common/mod.rs`'s
/// ownership gate refused any program with ownership errors, on the
/// premise that "`karac check` would reject it, so codegen is being fed
/// input it never sees in production". That premise is exactly backwards
/// for `UseAfterMove` — `cli.rs` excludes it from the fatal set by design,
/// so production compiles and runs these. The gate now mirrors
/// `is_fatal_ownership_kind`, which is what makes this lane reachable.
/// That structural blind spot is why the copy could be entirely absent
/// without a single one of ~1000 memory fixtures noticing.
///
/// Both failure directions are memory bugs, which is why ASAN rather than
/// output alone. No copy: the consumer's free dangles the source's later
/// read (use-after-free, then a double free at scope exit). A copy whose
/// source disarm still fires: the source's buffer has no owner (LSan
/// catches the leak). The two halves are inseparable, and this fixture is
/// what holds them together.
///
/// Every payload is read BYTE-WISE (`println` of the string itself, never
/// `.len()`) — a `.len()` read never dereferences the buffer, which is
/// precisely how the pre-existing `e2e_let_move_source_frozen` pin passed
/// against a copy that did not exist.
#[test]
fn asan_use_after_move_defensive_copy_frees_each_buffer_once() {
    assert_clean_asan_run(
        r#"
struct H { name: String }
fn main() {
    let mut i: i64 = 0i64;
    while i < 4i64 {
        // 1. String: move into a binding whose scope ends, then read the source.
        let s: String = f"al{i}";
        { let keep: String = s; println(keep); }
        println(s);
        // 2. Vec[String]: element-deep — an outer-only copy would share these.
        let mut w: Vec[String] = Vec.new();
        w.push(f"bb{i}");
        { let keep: Vec[String] = w; println(keep[0]); }
        println(w[0]);
        // 3. The STRUCT-LITERAL consume site. This one is ASAN-only in
        // practice: it prints the right thing at -O0 and only valgrind/ASAN
        // sees the two invalid reads.
        let t: String = f"cc{i}";
        { let h = H { name: t }; println(h.name); }
        println(t);
        // 4. CONTROL — no reuse, so no copy may be made. A copy here without
        // the source disarm would leak the original every pass.
        let d: String = f"dd{i}";
        let moved: String = d;
        println(moved);
        i = i + 1;
    }
    println("end");
}
"#,
        &[
            "al0", "al0", "bb0", "bb0", "cc0", "cc0", "dd0", "al1", "al1", "bb1", "bb1", "cc1",
            "cc1", "dd1", "al2", "al2", "bb2", "bb2", "cc2", "cc2", "dd2", "al3", "al3", "bb3",
            "bb3", "cc3", "cc3", "dd3", "end",
        ],
        "use_after_move_defensive_copy",
    );
}

/// The struct-field half of B-2026-08-09-20. A `File` field is a bare `ptr`
/// to a runtime-owned `Box<KaracFile>`, invisible to every classifier in the
/// struct drop glue (not a vec-struct, not a map handle, not an i64
/// side-table key), so `struct Holder { f: File }` reclaimed its storage and
/// leaked the handle.
///
/// Worth a fixture of its own rather than trusting the Vec one: the two are
/// different code paths (`FieldDrop` classification vs the element-drain
/// hook) that happened to share a symptom, and B-2026-08-09-17's first
/// diagnosis went wrong precisely by assuming a struct field and a Vec
/// element behave alike here.
#[test]
fn asan_file_moved_into_a_struct_field_is_closed_by_the_struct() {
    let path = file_fixture_path("file_into_struct");
    assert_clean_asan_run(
        &format!(
            r#"
struct Holder {{ f: File }}
fn main() with reads(FileSystem) {{
    let mut i: i64 = 0i64;
    let mut n: i64 = 0i64;
    while i < 40i64 {{
        match File.open("{path}") {{
            Ok(fh) => {{ let _h = Holder {{ f: fh }}; n = n + 1i64; }}
            Err(_) => {{ n = n - 1i64; }}
        }}
        i = i + 1;
    }}
    println(n.to_string());
}}
"#
        ),
        &["40"],
        "file_moved_into_struct_closed",
    );
}

/// B-2026-08-15-10 — a move written as a CALL ARGUMENT, whose source is
/// read again afterwards.
///
/// THIS PIN LIVES IN A CALLEE ON PURPOSE. The identical program in `main`
/// passes against the broken compiler, so a `main`-based fixture would have
/// reported green throughout — which is precisely how this class kept
/// reading as closed after B-2026-08-10-21. `main` is not a place where the
/// defensive copy worked; it is where the damage cannot be observed. The
/// disarm zeroes the source's `cap`, so the reuse gets the right BYTES with
/// a cap that makes every drop skip them: a borrow of the consumer's buffer.
/// Nothing double-frees, ASAN is clean, and the output is correct right up
/// until the consumer dies. In `main` it never does before the last read.
/// In a callee the map dies at scope exit and the returned `Vec[Stat]`
/// carries a dangling pointer out — heap-use-after-free, freed by
/// `karac_map_free_with_drop_vec`, read by `karac_string_clone`.
///
/// Both spellings of the reuse, both roots that reach the argument position
/// (a `let` bound off a Vec index, and an owned struct param), and a
/// `Vec[String]` field so a copy that duplicated only the outer buffer
/// would surface as a double free of an element rather than passing.
#[test]
fn asan_use_after_move_as_call_argument_copies_in_a_callee() {
    assert_clean_asan_run(
        r#"
struct Stat { service: String }
#[derive(Clone)]
struct Entry { service: String }
struct Bag { tags: Vec[String] }

fn agg(entries: Vec[Entry]) -> Vec[Stat] {
    let mut index: Map[String, usize] = Map.new();
    let mut stats: Vec[Stat] = Vec.new();
    let mut i = 0;
    while i < entries.len() {
        let e = entries[i].clone();
        let _ = index.insert(e.service, 0 as usize);
        stats.push(Stat { service: e.service });
        i = i + 1;
    }
    return stats;
}

fn tag_once(b: Bag) -> Vec[String] {
    let mut seen: Set[Vec[String]] = Set.new();
    let _ = seen.insert(b.tags);
    return b.tags;
}

fn main() {
    let mut es: Vec[Entry] = Vec.new();
    es.push(Entry { service: "alphabetical" });
    es.push(Entry { service: "betamaximum" });
    let out = agg(es);
    println(f"{out[0].service} {out[1].service} {out.len()}");

    let mut tags: Vec[String] = Vec.new();
    tags.push("gamma-ray-burst");
    let back = tag_once(Bag { tags: tags });
    println(f"{back[0]} {back.len()}");
    println("end");
}
"#,
        &["alphabetical betamaximum 2", "gamma-ray-burst 1", "end"],
        "use_after_move_as_call_argument_in_callee",
    );
}

#[test]
fn asan_generic_unused_bare_type_param_temp_arg_no_leak() {
    assert_clean_asan_run(
        r#"
fn id[T](v: T) -> T { return v; }
fn pick[T](a: T, b: T) -> T { return id(a); }
fn keep[T](a: T, b: T) -> T { return a; }
fn echo[T](x: T) -> T { return x; }
fn nest[T](x: T) -> T { return id(id(x)); }
fn takeout[T](b: T) -> T { match b { v => { return v; } } }
fn three[T](a: T, b: T, c: T) -> T { return b; }
fn mk() -> Vec[i64] { let v: Vec[i64] = [10, 20, 30, 40]; return v; }
fn main() {
    let x: Vec[i64] = [1, 2, 3, 4, 5, 6, 7, 8];
    let y: Vec[i64] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
    let ns: Vec[String] = ["alphaalphaalpha", "betabetabeta"];
    let mut k = 0;
    let mut t = 0;
    while k < 20 {
        let r1 = pick(x.clone(), y.clone()); t = t + r1[0];
        let r2 = keep(x.clone(), y.clone()); t = t + r2[1];
        let r3 = echo(x.clone()); t = t + r3[2];
        let r4 = nest(x.clone()); t = t + r4[3];
        let r5 = takeout(x.clone()); t = t + r5[4];
        let r6 = three(x.clone(), y.clone(), mk()); t = t + r6[0];
        let r7 = keep(ns.clone(), ns.clone()); t = t + r7.len();
        let r8 = keep(x.clone(), [90, 91, 92]); t = t + r8[5];
        k = k + 1;
    }
    println(t);
    println(x.len());
    println(ns[1]);
}
"#,
        &["480", "8", "betabetabeta"],
        "asan_generic_unused_bare_type_param_temp_arg_no_leak",
    );
}

/// The CONTRAST that localizes B-2026-08-22-18 to the array path: the
/// STRUCT analogue of the same move — a heap field returned out of an
/// owned `self` — is already balanced. So the defect is not a general
/// hole in owned-param drop tracking; it is specific to moving out of a
/// fixed array, and this test is what keeps that distinction honest if
/// someone later "fixes" the struct path looking for it.
#[test]
fn asan_struct_field_moved_out_of_owned_self_is_balanced() {
    assert_clean_asan_run(
        r#"struct Pair { a: String, b: String }
impl Pair { fn take_a(self) -> String { return self.a; } }
fn mk(i: i64) -> Pair { return Pair { a: f"a{i}", b: f"b{i}" }; }

fn main() {
    let mut i = 0i64;
    let mut n = 0i64;
    let mut last: String = "";
    while i < 50i64 {
        let p = mk(i);
        last = p.take_a();
        n = n + last.len();
        i = i + 1;
    }
    println(last);
    println(n);
}
"#,
        &["a49", "140"],
        "asan_struct_field_moved_out_of_owned_self_is_balanced",
    );
}

/// B-2026-08-25-10 — a generic impl method that moves a heap-owning field
/// out of an owned `self` double-freed every element buffer.
///
/// The owned by-value aggregate param is deep-copied at entry so the callee
/// owns it, and the comment on that code states the invariant: copy-depth
/// must equal drop-depth. It did not. The copy classified the element by
/// reading the field's declared type, which inside `impl[T] Heap[T]` is the
/// bare param `T` — neither String nor Vec nor Map/Set by name — so it
/// stayed outer-only and memcpy'd the element control blocks. The
/// monomorph's struct drop DOES resolve `T` and walks elements, so the
/// caller and the callee's copy both freed the same element buffers.
///
/// Case (b) is heap-allocated on purpose. An earlier narrowing of this bug
/// recorded "`T = String` is correct", which was a false negative: string
/// LITERALS live in static rodata with cap 0, so the second free is a no-op
/// and a literal-only fixture passes while the bug is fully present. Built
/// via f-string so the elements own real buffers.
///
/// Controls (c) and (d) were both correct pre-fix and pin the diagnosis: a
/// scalar element has no inner buffer to alias, and the non-generic twin
/// never erases the element type. Verified RED pre-fix — (a) aborted with
/// `attempting double-free` under ASAN, naming `karac_drop_Vec_i64` inside
/// `__karac_drop_struct_Heap$Vec_i64`.
#[test]
fn asan_generic_owned_self_field_move_out_does_not_double_free_elements() {
    assert_clean_asan_run(
        r#"
struct Heap[T] { xs: Vec[T] }
impl[T] Heap[T] { fn into_vec(self) -> Vec[T] { self.xs } }
struct PlainHeap { xs: Vec[Vec[i64]] }
impl PlainHeap { fn into_vec(self) -> Vec[Vec[i64]] { self.xs } }
fn main() {
    // (a) nested-Vec element: the filed shape.
    let a = Heap { xs: [[1, 2], [3], [4, 5, 6]] };
    let av = a.into_vec();
    println(f"a={av.len()} {av[0].len()} {av[2].len()}");
    // (b) HEAP-allocated String elements (literals would pass while broken).
    let mut ss: Vec[String] = Vec.new();
    let mut i = 0;
    while i < 3 { ss.push(f"item{i}"); i = i + 1; }
    let b = Heap { xs: ss };
    let bv = b.into_vec();
    println(f"b={bv.len()} {bv[0]}");
    // (c) scalar control: correct before the fix.
    let c = Heap { xs: [7, 8] };
    println(f"c={c.into_vec().len()}");
    // (d) NON-generic control at the same nested-Vec element type.
    let d = PlainHeap { xs: [[1], [2]] };
    println(f"d={d.into_vec().len()}");
}
"#,
        &["a=3 2 3", "b=3 item0", "c=2", "d=2"],
        "generic-owned-self-field-move-out",
    );
}

/// B-2026-08-25-17. A DISCARDED heap-owning result leaked inside a generic
/// impl-method monomorph: `h.xs.pop();` freed nothing at a heap-carrying
/// `T`, one element per call.
///
/// The row asked for the SCOPE to be established before fixing, on the
/// grounds that `pop()` is only a convenient producer. Measuring it made
/// the scope NARROWER, not wider: a 10-case producer matrix at top level
/// (`v.pop()`, `remove`, `swap_remove`, `Map.remove`, and functions/methods
/// returning `String`/`Vec`/an owning struct, discarded) is entirely clean,
/// as are a NON-generic impl method, a generic FREE FUNCTION, a plain local
/// rebind, and a scalar `T`. The trigger needs a generic IMPL METHOD whose
/// receiver was rebound from `self` — the B-2026-08-25-7 shape, whose fix
/// (`cbc545f`) is in and addressed the sibling-dispatch half only.
///
/// Root cause: `enum_inst_type_exprs` is span-keyed and PRE-monomorphization,
/// so inside the monomorph it holds the generic `Option[T]`; the payload
/// element cannot be read off that, every tracker declined, and the temp
/// fell through to `materialize_owned_temp`, which does not free it in this
/// shape.
///
/// Both cases below leak WITHOUT the fix and are clean with it. The
/// `String` arm matters because an earlier measurement said `T = String`
/// was clean — it was built from string LITERALS, which live in rodata with
/// `cap == 0` and never allocate. Built through an f-string it leaks 43
/// bytes, which is what shows the class is "any heap-owning element" rather
/// than "a nested Vec".
#[test]
fn asan_discarded_pop_in_generic_method_frees_element() {
    assert_clean_asan_run(
        r#"
struct Heap[T] { xs: Vec[T] }
impl[T] Heap[T] {
    fn take(self) -> i64 { let mut h = self; h.xs.pop(); h.xs.len() }
}
fn main() {
    let hv = Heap { xs: [[1], [2], [3]] };
    println(f"vec={hv.take()}");
    let mut v: Vec[String] = Vec.new();
    let mut i: i64 = 0;
    while i < 3 { v.push(f"element-number-{i}-with-padding-to-exceed-sso"); i = i + 1; }
    let hs = Heap { xs: v };
    println(f"str={hs.take()}");
}
"#,
        &["vec=2", "str=2"],
        "discarded-pop-generic-method",
    );
}

/// B-2026-08-25-17, precedence guard. The same rebind-and-discard shape in
/// a generic FREE FUNCTION is already handled correctly by the fallback
/// this fix runs alongside, and it must stay that way.
///
/// This is not a hypothetical: the first two attempts at the fix — first
/// substituting the monomorph type in the inline-Option tracker itself,
/// then running that as a last resort — both turned this clean case into a
/// 16-byte leak, because claiming the temp here displaces the handler that
/// was already freeing it. The landed fix is gated to an impl-method
/// monomorph for exactly this reason, and without that gate this test
/// fails while the one above passes.
#[test]
fn asan_discarded_pop_in_generic_free_fn_stays_clean() {
    assert_clean_asan_run(
        r#"
struct Heap[T] { xs: Vec[T] }
fn take[T](s: Heap[T]) -> i64 { let mut h = s; h.xs.pop(); h.xs.len() }
fn main() { let h = Heap { xs: [[1], [2], [3]] }; println(f"n={take(h)}"); }
"#,
        &["n=2"],
        "discarded-pop-generic-free-fn",
    );
}

/// B-2026-08-27-29 — a `.clone()` passed DIRECTLY as a call argument must
/// free its temporary. It did not: an owned by-value aggregate param is
/// callee-owned by ENTRY COPY (`make_aggregate_param_callee_owned_inst`),
/// so the callee duplicates the struct's heap fields into its own frame and
/// frees only that copy — while the caller's clone, having no binding,
/// matched no arm of `track_inline_owned_aggregate_arg_inst` and was
/// registered for nothing. Measured at 680 bytes over 20 allocations here:
/// the cloned `String` field, once per iteration, unbounded in a loop.
///
/// The neighbouring shapes were already clean, which is why no fixture
/// covered this: `take(mk())` (a fn-call temp) has its own arm, and
/// `let c = a.clone(); take(c)` is a binding whose `let`-path drop fires.
/// Only the inline spelling leaked — the exact spelling B-2026-08-26-21's
/// diagnostic points authors at, which is how it surfaced.
#[test]
fn asan_struct_clone_call_argument_frees_temp() {
    assert_clean_asan_run(
        r#"
#[derive(Clone)]
struct It { id: i64, name: String }
fn take(x: It) -> i64 { return x.name.len(); }
fn main() {
    let a = It { id: 1, name: f"payload_aaaaaaaaaaaaaaaaaaaaaaaa{1}" };
    let mut t = 0i64;
    let mut i = 0i64;
    while i < 20i64 { t = t + take(a.clone()); i = i + 1i64; }
    println(f"{t}");
}
"#,
        &["660"],
        "asan-struct-clone-call-arg",
    );
}

/// B-2026-08-27-29, INDEX-RECEIVER spelling — `take(v[i].clone())`. The
/// element read is a borrow and the clone an independent copy of it, so the
/// temporary is the caller's exactly as the identifier form's is. Covered
/// alongside it because this is the form B-2026-08-26-21's migration
/// produces: the rule rejects `let x = v[i]` for a non-`Copy` element and
/// names `.clone()` as one of the two remedies.
#[test]
fn asan_struct_clone_of_element_call_argument_frees_temp() {
    assert_clean_asan_run(
        r#"
#[derive(Clone)]
struct It { id: i64, name: String }
fn take(x: It) -> i64 { return x.name.len(); }
fn main() {
    let mut v: Vec[It] = Vec.new();
    v.push(It { id: 1, name: f"payload_aaaaaaaaaaaaaaaaaaaaaaaa{1}" });
    let mut t = 0i64;
    let mut i = 0i64;
    while i < 20i64 { t = t + take(v[0].clone()); i = i + 1i64; }
    println(f"{t}");
}
"#,
        &["660"],
        "asan-elem-clone-call-arg",
    );
}

/// B-2026-08-26-31, the MEMORY half — and the half that was ALREADY
/// CORRECT, which is the whole point of pinning it.
///
/// A local moved into a container's element slot (`b.xs[2] = t`) had its
/// heap cleanup disarmed by `zero_struct_move_caps` (B-2026-08-12-22) but
/// kept its `UserDrop` registration, so the bug was a duplicate Drop BODY
/// with no double free behind it. This fixture was ASAN-clean BEFORE the
/// bodies fix and is ASAN-clean after; it exists so that editing the
/// bodies half cannot silently re-arm the memory half. Only one of the two
/// halves has a sanitizer behind it, so the observable-count fixture in
/// `tests/codegen.rs`
/// (`test_e2e_local_moved_into_elem_slot_drops_once`) is the other guard.
///
/// Reading an element CLONES, so slot 0 still holds its original `1` after
/// `let t = b.xs[0]` — the vector reads `1,2,1`, not `_,2,1`. That is the
/// model disagreement tracked as the open remainder of B-2026-08-26-21;
/// here it is simply the observed behaviour the expectation encodes.
#[test]
fn asan_local_moved_into_struct_elem_slot_is_not_double_freed() {
    assert_clean_asan_run(
        r#"
#[derive(Clone)]
struct Item { id: i64, tag: String }
struct Bag { xs: Vec[Item] }
fn main() {
    let mut b = Bag { xs: Vec.new() };
    b.xs.push(Item { id: 1i64, tag: f"payload_one_{1}" });
    b.xs.push(Item { id: 2i64, tag: f"payload_two_{2}" });
    b.xs.push(Item { id: 3i64, tag: f"payload_three_{3}" });
    let t = b.xs[0].clone();
    b.xs[2] = t;
    println(f"{b.xs[0].id}{b.xs[1].id}{b.xs[2].id}");
}
"#,
        &["121"],
        "local-moved-into-struct-elem-slot",
    );
}

/// A FRESH TUPLE TEMPORARY has an owner, so the elements a projection does
/// not hand out are freed — `println(twoheap(1).1)` over
/// `fn twoheap(k) -> (Person, String)` (B-2026-08-28-27).
///
/// The temp had no owner at all: measured on the IR, the unbound spelling
/// emitted no drop against the BOUND one's slot store plus
/// `__karac_drop_tuple_0`, and under LSan it lost 12 bytes in 3
/// allocations per call — the whole tuple, `ada` + `unread` + `sib`. The
/// B-2026-07-22-2 family, reached through the one aggregate kind that had
/// no such registration.
///
/// BOTH SIDES OF THE TAKEOVER ARE PINNED HERE, because each was measured
/// failing on its own. Excluding the projected element at materialization
/// (the first attempt) fixes every consuming row and leaves the
/// NON-consuming ones — `println(…​.1)`, `.len()` — leaking the element they
/// hand out. Covering the whole tuple with no takeover fixes those and
/// double-frees at `let` / `push` / an argument. So the consuming and
/// non-consuming rows are not variations on one case; they fail against
/// opposite halves of the fix.
///
/// THE LAST THREE ROWS ARE THE COMPOSITION WITH B-2026-08-28-3, which
/// registers the PROJECTED element when the projection is a struct whose
/// field is then read. That registration is itself a consuming
/// destination: without the handover both it and the tuple's drop free the
/// same buffers, which is an `attempting double-free` — measured, on
/// exactly these three and nothing else. They also cover the leak that fix
/// left behind, a heap SIBLING element going unfreed while the projected
/// struct was reclaimed.
///
/// `plainpair` carries a scalar second element so the Copy member is
/// exercised, and the bound and discarded rows are the must-not-change
/// controls — they were always clean, via `track_tuple_var`.
#[test]
fn test_fresh_tuple_temp_elements_are_owned() {
    let src = r#"
struct Person { name: String, note: String, age: i64 }

fn twoheap(k: i64) -> (Person, String) {
    return (
        Person { name: "ada".to_string(), note: "unread".to_string(), age: k },
        "sib".to_string(),
    );
}
fn twostr(k: i64) -> (String, String) { return (f"a{k}", f"b{k}"); }
fn plainpair(k: i64) -> (String, i64) { return (f"s{k}", k + 1); }
fn structpair(k: i64) -> (Person, i64) {
    return (Person { name: "ada".to_string(), note: "unread".to_string(), age: k }, k);
}

fn main() {
    // non-consuming reads: the temp's drop must free the element it hands out
    println(twoheap(1).1);
    println(twostr(1).0);
    println(plainpair(1).0);
    println(twostr(2).0.len());
    println(plainpair(2).1);
    // consuming positions: the destination takes that element over
    let w = twoheap(3).1;
    println(w);
    let x = twostr(3).0;
    println(x);
    let mut o: Vec[String] = Vec.new();
    o.push(twostr(4).0);
    println(o[0]);
    // both elements read out of separate temps
    println(twostr(5).0);
    println(twostr(5).1);
    // controls that were always clean
    let a = twoheap(6);
    println(a.1);
    let _b = twoheap(7);
    // composition with B-2026-08-28-3: a field read through the projection
    println(structpair(8).0.name);
    println(twoheap(9).0.name);
    let y = twoheap(10).0.name;
    println(y);
}
"#;
    assert_clean_asan_run(
        src,
        &[
            "sib", "a1", "s1", "2", "3", "sib", "a3", "a4", "a5", "b5", "sib", "ada", "ada", "ada",
        ],
        "fresh-tuple-temp-elements-owned",
    );
}

/// A container-element heap read consumed by an unbound STRUCT LITERAL —
/// `println(Box2 { w: p[0].word }.w)` (B-2026-08-28-32, struct-literal
/// shape).
///
/// The read deep-clones (the container keeps its own buffer), and the
/// literal MOVES that clone in — which neutralizes the clone's own cleanup
/// through `vec_elem_field_clone_slots`, exactly as any consuming
/// destination does. Nothing then owned the literal, so the moved buffer
/// had no owner at all and leaked.
///
/// `value_block_hands_out_its_tail` is the gate that arms the fresh-temp
/// struct drop for a receiver, and it admitted a block, an `if` and an
/// `if let` but not a struct literal — the same widening B-2026-08-27-35
/// made for an array literal, on the same reasoning: a literal that moved
/// its initializers in owns them.
///
/// The BOUND spelling is the control and was always clean, because the
/// binding owns the literal. Keeping both is what distinguishes "the
/// literal has no owner" from "the read did not clone".
#[test]
fn test_struct_literal_temp_consuming_a_container_read_is_owned() {
    let src = r#"
struct P { word: String, n: i64 }
struct Box2 { w: String, k: i64 }

fn mkp() -> Vec[P] { return [P { word: f"a{1}", n: 1 }, P { word: f"b{2}", n: 2 }]; }

fn main() {
    let p = mkp();
    // the unbound literal: it owns what it moved in
    println(Box2 { w: p[0].word, k: 1 }.w);
    println(Box2 { w: p[1].word, k: 2 }.k);
    // the bound control
    let b = Box2 { w: p[0].word, k: 3 };
    println(b.w);
    // the container is untouched
    println(p[0].word);
    println(p[1].word);
}
"#;
    assert_clean_asan_run(
        src,
        &["a1", "2", "a1", "a1", "b2"],
        "struct-literal-temp-container-read",
    );
}

/// A container-element heap read consumed by an unbound TUPLE LITERAL —
/// `println((p[1].word, 9).0)` (B-2026-08-28-44, tuple-literal half).
///
/// The literal moves the read's clone in, which neutralizes the clone's own
/// cleanup through `vec_elem_field_clone_slots` exactly as any consuming
/// destination does, and then nothing owned the literal.
/// `track_freshtemp_tuple_object` (B-2026-08-28-27) is what would have
/// owned it, and it declined for a reason specific to literals: it resolves
/// the projected element's `TypeExpr` from the CALLEE's declared return
/// type, and a literal has no callee.
///
/// BOTH PROJECTIONS ARE HERE, and they fail against opposite halves of the
/// fix. Projecting the `{ptr,len,cap}` member needs the exclusion — without
/// it the temp's drop and the consumer both free the moved buffer.
/// Projecting the SCALAR member needs the registration to happen ANYWAY:
/// there is nothing to exclude, and declining outright (the first attempt)
/// left the tuple's String member with no owner at all. Measured, one row
/// each.
///
/// The bound spelling and the call-returned tuple are the controls — the
/// first owns the literal, the second already had its element types.
#[test]
fn test_tuple_literal_temp_consuming_a_container_read_is_owned() {
    let hdr = "\
struct P { word: String, n: i64 }\n\
fn mkp() -> Vec[P] { return [P { word: f\"a{1}\", n: 1 }, P { word: f\"b{2}\", n: 2 }]; }\n\
fn mkt(k: i64) -> (String, i64) { return (f\"s{k}\", k); }\n";
    for (label, body, want) in [
        // the unbound literal, projecting the heap member
        (
            "heap-member",
            "let p = mkp(); println((p[1].word, 9).0);",
            "b2",
        ),
        // and projecting the scalar: the String member still needs an owner
        (
            "scalar-member",
            "let p = mkp(); println((p[1].word, 9).1);",
            "9",
        ),
        // controls
        (
            "bound-literal",
            "let p = mkp(); let t = (p[1].word, 9); println(t.0);",
            "b2",
        ),
        ("call-returned", "println(mkt(1).0);", "s1"),
        ("plain-read", "let p = mkp(); println(p[0].word);", "a1"),
    ] {
        let src = format!("{hdr}\nfn main() {{\n    {body}\n}}\n");
        assert_clean_asan_run(&src, &[want], label);
    }
}

/// B-2026-08-28-70 — the MEMORY half of the method owned-argument
/// ownership fix: every heap-carrying param a method takes by value is
/// allocated once and freed once, on the path where it dies and on the path
/// where it is handed back.
///
/// The body-count half lives in `tests/codegen.rs` and `tests/interpreter.rs`;
/// this pins that neither the caller's stand-down nor the callee's new
/// registration orphans or double-frees a buffer. The direction that most
/// needed proving is `always-returned`: the caller now declines its
/// arg-site fire for a param the method hands back, and a stand-down is
/// only ever correct when someone else owns the value — if the result
/// binding did not, this would be a leak rather than a wrong line of
/// output, and no body-count fixture would see it.
///
/// Seeded from `env.args().len()` and read back per object, because the
/// harness compiles at -O2 and a provably-dead allocation is deleted along
/// with the evidence (the B-2026-08-24-5 lesson). `String` payloads are
/// built with f-strings rather than literals: a literal is rodata with
/// `cap == 0` and is never freed, so it cannot witness a double free.
#[test]
fn asan_method_owned_param_bodies_are_memory_balanced() {
    assert_clean_asan_run(
        r#"
struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"drop {self.name}"); } }
struct H { r: R }
struct B2 { n: i64 }
impl B2 {
    fn eat(ref self, r: R) -> i64 { 7 }
    fn id(ref self, r: R) -> R { r }
    fn pick(ref self, r: R, k: bool) -> R { if k { r } else { R { id: 99, name: f"n99" } } }
    fn wrap(ref self, r: R) -> H { H { r: r } }
    fn two(ref self, a: R, b: R, k: bool) -> R { if k { a } else { b } }
    fn early(ref self, r: R, k: bool) -> R { if k { return R { id: 98, name: f"n98" }; } r }
}
fn main() {
    let base: i64 = env.args().len();
    let b = B2 { n: 1 };
    let v = b.eat(R { id: base, name: f"a{base}" });
    println(f"{v}");
    let w = b.id(R { id: base, name: f"b{base}" });
    println(f"{w.id}");
    let c = b.pick(R { id: base, name: f"c{base}" }, false);
    println(f"{c.id}");
    let d = b.pick(R { id: base, name: f"d{base}" }, true);
    println(f"{d.id}");
    let e = b.wrap(R { id: base, name: f"e{base}" });
    println(f"{e.r.id}");
    let g = b.two(R { id: base, name: f"g{base}" }, R { id: base, name: f"h{base}" }, false);
    println(f"{g.id}");
    let i = b.early(R { id: base, name: f"i{base}" }, true);
    println(f"{i.id}");
    b.eat(R { id: base, name: f"j{base}" });
    println("end");
}
"#,
        &[
            "drop a1", "7", "1", "drop b1", "drop c1", "99", "drop n99", "1", "drop d1", "1",
            "drop e1", "drop g1", "1", "drop h1", "drop i1", "98", "drop n98", "drop j1", "end",
        ],
        "method-owned-param-bodies",
    );
}

/// B-2026-08-31-34 — a heap field read off a FRESH TEMP and consumed by an
/// owning aggregate double freed on both compiled backends. The E2E twin
/// pins the text; this is the memory gate, because that is what the defect
/// actually is.
///
/// `consume_freshtemp_field_move` zeroes the accessed field in the staged
/// temp slot so the temp's struct drop frees only the UNREAD remainder. It
/// was hooked at the let / assign / return / fn-tail sites and at the
/// ordinary call-argument path, and at NONE of the aggregate-literal
/// consume sites — so a struct literal, an array/`Vec` literal, a tuple, or
/// a variant constructor took the same pointer while the temp kept its
/// cleanup, and both freed it.
///
/// `two_fields_one_taken` is the LEAK-direction control and the reason this
/// fixture exists rather than the E2E twin alone: the fix TRANSFERS
/// ownership, so getting it wrong in the other direction — zeroing a field
/// the aggregate never took — leaks silently and prints correctly. `mk2`
/// returns a temp with TWO heap fields and the literal takes one; the
/// other must still be freed by the temp. LSan sees that only on the Linux
/// CI leg, never on a local macOS asan run.
///
/// Same fixture rules as its siblings: FUNCTION-SCOPE locals, and the
/// payload's BYTES read via `contains` rather than its length, since a
/// `.len()`-only buffer is a dead allocation LLVM deletes outright.
#[test]
fn asan_a_fresh_temp_field_read_consumed_by_an_aggregate_frees_once() {
    assert_clean_asan_run_min_allocs(
        r#"
struct P { a: String, b: i64 }
struct W { a: String }
struct TwoHeap { a: String, c: String }
enum E { Ea { a: String }, En }
enum T { Ta(String), Tn }
fn payload(t: i64) -> String {
    let mut s: String = String.new();
    s.push_str("payload-padded-out-well-past-thirty-six-bytes-");
    s.push_str(f"{t}");
    return s;
}
fn mkp(n: i64) -> P { return P { a: payload(n), b: n }; }
fn mk2(n: i64) -> TwoHeap { return TwoHeap { a: payload(n), c: payload(n + 10) }; }
fn struct_literal_field() -> bool {
    let x: P = P { a: mkp(1).a, b: 1 };
    return x.a.contains("padded");
}
fn vec_literal_element() -> bool {
    let v: Vec[String] = [mkp(2).a];
    return v[0].contains("padded");
}
fn tuple_element() -> bool {
    let t: (String, i64) = (mkp(3).a, 3);
    return t.0.contains("padded");
}
fn enum_struct_variant() -> bool {
    let e: E = E.Ea { a: mkp(4).a };
    let mut hit: bool = false;
    match e { E.Ea { a } => { hit = a.contains("padded"); } E.En => {} }
    return hit;
}
fn enum_tuple_variant() -> bool {
    let e: T = T.Ta(mkp(5).a);
    let mut hit: bool = false;
    match e { T.Ta(s) => { hit = s.contains("padded"); } T.Tn => {} }
    return hit;
}
fn optres_ctor_arg() -> bool {
    let o: Option[String] = Some(mkp(6).a);
    let mut hit: bool = false;
    match o { Some(s) => { hit = s.contains("padded"); } None => {} }
    return hit;
}
fn two_fields_one_taken() -> bool {
    let x: W = W { a: mk2(7).a };
    return x.a.contains("padded");
}
fn ordinary_call_arg_control() -> bool {
    let x: W = W { a: payload(8) };
    return x.a.contains("padded");
}
fn main() {
    println(struct_literal_field());
    println(vec_literal_element());
    println(tuple_element());
    println(enum_struct_variant());
    println(enum_tuple_variant());
    println(optres_ctor_arg());
    println(two_fields_one_taken());
    println(ordinary_call_arg_control());
    println("done");
}
"#,
        &[
            "true", "true", "true", "true", "true", "true", "true", "true", "done",
        ],
        "fresh-temp-field-read-consumed-by-an-aggregate",
        // Measured 120 on the fixed compiler's host; 87-102 on x86_64
        // Linux. This count MOVES WITH CPU CONTENTION (B-2026-09-09-4): the
        // program's `par` work DISTRIBUTION decides how many per-worker
        // blocks get allocated, and a saturated box distributes less work,
        // so it allocates less. A three-run corpus sweep put this cell at
        // 102/102/87 against a floor of 90 — ALREADY BREACHED at 87, the
        // one hard failure among 223 floored fixtures, and the cell
        // B-2026-09-09-5 reported as an unexplained intermittent.
        //
        // Floored at lowest-observed minus twice the observed swing
        // (87 - 2*15), rounded down. That is still ~50x above the collapse
        // this guard exists to catch — a folded-away payload lands near
        // zero, and the `two_fields_one_taken` control would go with it —
        // while sitting outside the scheduling noise. Do NOT re-tighten
        // this to a freshly measured number: setting it just under one
        // host's quiet reading is exactly what put it under its own floor.
        50,
    );
}

#[test]
fn asan_bare_tuple_elem_field_move_out_frees_exactly_once() {
    // B-2026-09-02-34 (B-2026-09-02-27 REACH) — the two scrutinee shapes `e49aa9e` left
    // double-freeing: a PROJECTION scrutinee (`match w.t`) and a NESTED
    // bare tuple (`((r, j), k)`). Its recorder took a flat pattern over an
    // identifier scrutinee only; this is the ASAN half of the widening,
    // and the E2E twin is `e2e_bare_tuple_elem_field_move_out_frees_once`.
    //
    // The mechanism is `e49aa9e`'s and unchanged: the arm binding is a
    // bit-copy of the tuple's element (B-2026-09-02-23 made the tuple's
    // own `__karac_drop_tuple_*` the single owner), so `let n = r.name`
    // cap-zeroes storage no drop reads while the tuple's walk still frees
    // `t.0.name` — the buffer `n` now owns. What changed is only WHICH
    // scrutinees can name that home.
    //
    // This is the test that says the mirror is EXACT rather than merely
    // quiet: the field the move did NOT take (`tag`) must still be freed
    // by the tuple, so an over-broad suppression shows up here as a LEAK
    // where the original defect showed up as a double free.
    //
    // `tag` is the untouched sibling String — 45 bytes, past any
    // small-string threshold — read through at every cell, so it is real
    // heap traffic in both directions. `xs` is deliberately `Vec[i64]` and
    // not `Vec[String]`: a tuple PARAM carrying a `Vec[String]` leaks that
    // Vec's ELEMENTS on `main` today, with or without any move-out
    // (B-2026-09-02-35), which would drown this assertion in a leak that is
    // not this row's.
    //
    // All four source shapes the sweep separated: a tuple PARAM and a
    // tuple LOCAL (both already covered by `e49aa9e`), plus the two it
    // left broken — a tuple STRUCT FIELD and a NESTED bare tuple. Looped,
    // so a mirror that fires once and then goes stale between iterations
    // is caught.
    assert_clean_asan_run(
        r#"
fn pad(t: i64) -> String {
    let mut s: String = String.new();
    s.push_str("payload-padded-out-well-past-thirty-six-byte");
    s.push_str(f"{t}");
    return s;
}
struct H { id: i64, xs: Vec[i64], tag: String, name: String }
fn mk(id: i64) -> H {
    let mut v: Vec[i64] = Vec.new();
    v.push(id);
    v.push(id);
    return H { id: id, xs: v, tag: pad(id), name: pad(id) };
}
struct W { t: (H, i64) }
fn take(t: (H, i64)) {
    match t { (r, k) => { let n = r.name; println(f"p{n.len()}:{r.tag.len()}:{r.xs.len()}:{k}"); } }
}
fn nested(t: ((H, i64), i64)) {
    match t { ((r, j), k) => { let n = r.name; println(f"q{n.len()}:{r.tag.len()}:{r.xs.len()}:{j}:{k}"); } }
}
fn main() {
    let mut i = 0;
    while i < 3 {
        take((mk(1), 0));
        let t = (mk(2), 5);
        match t { (r, k) => { let n = r.name; println(f"l{n.len()}:{r.tag.len()}:{r.xs.len()}:{k}"); } }
        let w = W { t: (mk(3), 9) };
        match w.t { (r, k) => { let n = r.name; println(f"w{n.len()}:{r.tag.len()}:{r.xs.len()}:{k}"); } }
        nested(((mk(4), 7), 8));
        i = i + 1;
    }
    println("done");
}
"#,
        &[
            "p45:45:2:0",
            "l45:45:2:5",
            "w45:45:2:9",
            "q45:45:2:7:8",
            "p45:45:2:0",
            "l45:45:2:5",
            "w45:45:2:9",
            "q45:45:2:7:8",
            "p45:45:2:0",
            "l45:45:2:5",
            "w45:45:2:9",
            "q45:45:2:7:8",
            "done",
        ],
        "b0902-27-bare-tuple-elem-field-move-out",
    );
}

/// B-2026-09-05-21 — the memory half of
/// `e2e_nested_param_destructure_leaf_read_and_moved_one_body_each`: the
/// generic leaf moved into a local (`three`/`four`) double-freed on every
/// compiled backend, and the fix hands the leaf its field's memory (the
/// source zeroed under the field's subst), so this pins that every buffer
/// is freed exactly once; the -20 cells are bodies-only and ride along.
#[test]
fn asan_nested_param_destructure_leaf_read_and_moved_clean() {
    let label = "nested_param_destructure_leaf_read_and_moved";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan(
        r#"
struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct Gd[T] { r: T, z: i64 }
struct Gn2[T] { inner: Gd[T], z: i64 }
struct Hd { r: R, z: i64 }
struct Hn { inner: Hd, z: i64 }
fn mk(k: i64) -> R { return R { id: k, tag: f"t{k}", xs: [k] } }
fn hLive(h: Hn) -> i64 { let Hn { inner, z } = h; println("in"); let q: i64 = inner.z; return z + q }
fn hLiveLocal(h: Hn) -> i64 { let Hn { inner, z } = h; println("in"); let g: Hd = inner; println("moved"); return z + g.z }
fn gLive[T](h: Gn2[T]) -> i64 { let Gn2 { inner, z } = h; println("in"); let q: i64 = inner.z; return z + q }
fn gLiveLocal[T](h: Gn2[T]) -> i64 { let Gn2 { inner, z } = h; println("in"); let g: Gd[T] = inner; println("moved"); return z + g.z }
fn main() {
    { let a: i64 = hLive(Hn { inner: Hd { r: mk(1), z: 1 }, z: 9 }); println(f"one{a}") }
    { let n: Hn = Hn { inner: Hd { r: mk(2), z: 1 }, z: 9 }; let a: i64 = hLive(n); println(f"two{a}") }
    { let a: i64 = gLiveLocal(Gn2[R] { inner: Gd[R] { r: mk(3), z: 1 }, z: 9 }); println(f"three{a}") }
    { let n: Gn2[R] = Gn2[R] { inner: Gd[R] { r: mk(4), z: 1 }, z: 9 }; let a: i64 = gLiveLocal(n); println(f"four{a}") }
    { let a: i64 = gLive(Gn2[R] { inner: Gd[R] { r: mk(5), z: 1 }, z: 9 }); println(f"five{a}") }
    { let a: i64 = hLiveLocal(Hn { inner: Hd { r: mk(6), z: 1 }, z: 9 }); println(f"six{a}") }
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
            "in", "dR1", "one10", "in", "dR2", "two10", "in", "moved", "dR3", "three10", "in",
            "moved", "dR4", "four10", "in", "dR5", "five10", "in", "moved", "dR6", "six10", "end",
        ],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// B-2026-09-06-46 — ASAN twin of `tests/codegen.rs`'s
/// `e2e_partial_destructure_over_a_moved_out_source_runs_each_body_once`.
///
/// The defect was a lost `Drop` BODY, not a memory error — the source's
/// whole field-bodies walk was deleted when one field moved out, so the
/// bound leaf's body ran nowhere while every buffer was still freed
/// exactly once. This pin exists for the direction the fix could have
/// gone wrong in: re-arming the walk masks one field and hands another
/// to the leaf, and getting that split wrong frees a field twice or not
/// at all. Asserts the body count as well, since ASAN alone would stay
/// green on the original defect.
#[test]
fn asan_partial_destructure_over_a_moved_out_source_runs_each_body_once() {
    let label = "partial_destructure_over_a_moved_out_source";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan(
        r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"n{i}" }; }
struct S3 { a: R, b: R }
fn p_moved_rest(r: R) -> i64 { let s: S3 = S3 { a: mk(2), b: mk(3) }; let x: R = s.a; let S3 { b, .. } = s; println("mid"); return b.id + x.id; }
fn p_moved_wild(r: R) -> i64 { let s: S3 = S3 { a: mk(5), b: mk(6) }; let x: R = s.a; let S3 { b, a: _ } = s; println("mid"); return b.id + x.id; }
fn p_moved_unread(r: R) -> i64 { let s: S3 = S3 { a: mk(8), b: mk(9) }; let x: R = s.a; let S3 { b, .. } = s; println("mid"); return 1; }
fn main() {
    { let v: i64 = p_moved_rest(mk(1)); println(f"v={v}"); println("one") }
    { let v: i64 = p_moved_wild(mk(4)); println(f"v={v}"); println("two") }
    { let v: i64 = p_moved_unread(mk(7)); println(f"v={v}"); println("three") }
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
            "mid", "dR3", "dR2", "dR1", "v=5", "one", "mid", "dR6", "dR5", "dR4", "v=11", "two",
            "dR8", "dR9", "mid", "dR7", "v=1", "three", "end"
        ],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// B-2026-09-06-55 — ASAN twin of the `deep-chain` / `three-hops` /
/// `discard-the-hop` rows in `tests/codegen.rs`'s
/// `test_e2e_moving_one_field_out_leaves_the_others_their_drop_bodies`.
///
/// Same reason the B-2026-09-06-46 pin above exists, one level deeper. The
/// defect was a lost BODY over an intact free set — valgrind measured
/// `24 allocs, 24 frees, 0 errors` before and after the fix — so ASAN was
/// green on it and would be green on it again. What ASAN DOES cover is the
/// direction the fix could have gone wrong in: the mask moved from the hop
/// to the leaf, so `Inner`'s walker now runs against a slot one of whose
/// fields moved out, and getting that split wrong frees `r`'s buffer twice
/// or leaks `q`'s. The line vector is asserted alongside, because the body
/// count is the half ASAN cannot see.
#[test]
fn asan_deep_chain_field_move_out_keeps_every_sibling_body() {
    let label = "deep_chain_field_move_out_keeps_every_sibling_body";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan(
        r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"n{i}" }; }
struct Inner { r: R, q: R }
struct Outer { h: Inner, k: R }
struct L3 { r: R, q: R }
struct L2 { c: L3, d: R }
struct L1 { b: L2, e: R }
fn p_deep(z: R) -> i64 { let o: Outer = Outer { h: Inner { r: mk(2), q: mk(3) }, k: mk(4) }; let x: R = o.h.r; println("mid"); return x.id; }
fn p_three(z: R) -> i64 { let o: L1 = L1 { b: L2 { c: L3 { r: mk(6), q: mk(7) }, d: mk(8) }, e: mk(9) }; let x: R = o.b.c.r; println("mid"); return x.id; }
fn p_discard(z: R) -> i64 { let o: Outer = Outer { h: Inner { r: mk(11), q: mk(12) }, k: mk(13) }; let x: R = o.h.r; let Outer { k, h: _ } = o; println("mid"); return k.id + x.id; }
fn main() {
    { let v: i64 = p_deep(mk(1)); println(f"v={v}"); println("one") }
    { let v: i64 = p_three(mk(5)); println(f"v={v}"); println("two") }
    { let v: i64 = p_discard(mk(10)); println(f"v={v}"); println("three") }
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
            "dR4", "dR3", "mid", "dR2", "dR1", "v=2", "one", "dR9", "dR8", "dR7", "mid", "dR6",
            "dR5", "v=6", "two", "dR12", "mid", "dR13", "dR11", "dR10", "v=24", "three", "end"
        ],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// B-2026-09-09-14 — a DISCARDED tuple temp (`f(mk(20));`) owns its whole
/// interior and nothing was freeing it.
///
/// `track_discarded_temp_cleanup` is a chain of arms keyed to a return
/// SHAPE — inline `Option`, `Result`, the boxed and shared variants of
/// each — ending at `materialize_owned_temp`, whose chokepoint knows
/// Vec/String/Map/RC scalars and has no aggregate walk. A tuple matched
/// none of them and fell through, so the result's heap had no owner at all.
/// That is strictly more than the aggregate-RETURN family this row came
/// from (B-2026-09-06-72), which loses only the refcount block: here the
/// `String` goes too.
///
/// NO `Drop` BODY IS REGISTERED, and the expected stdout below is what pins
/// that. `--interp` runs no body for this shape either — both backends
/// print only the trailing statement — so registering one on the compiled
/// side alone would turn a leak into a run-vs-build divergence. A body that
/// runs on NEITHER backend is a real defect and a different class; it is
/// filed separately rather than folded into a leak fix. If a later change
/// makes the body fire, these cells fail on stdout, which is the correct
/// outcome: the interpreter has to move in the same commit.
#[test]
fn asan_discarded_tuple_temp_frees_its_interior() {
    // 1 — the row's cell: a `shared` field AND a String, 19 B in 2 blocks
    //     at -O0 before the fix.
    assert_clean_asan_run(
            "shared struct Inner { v: i64 }\n\
             struct R { id: i64, name: String, inner: Inner }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             fn seed() -> i64 { env.args().len() }\n\
             fn mk(i: i64) -> R { return R { id: i, name: f\"hhhhhhhh{i}{seed()}\", inner: Inner { v: i } }; }\n\
             fn f(r: R) -> (R, i64) { return (r, 9); }\n\
             fn main() { f(mk(20)); println(\"ok\"); }\n",
            // B-2026-09-09-21 — `dR20` is NEW here, and this cell is the
            // interlock that row built on purpose. It pinned the bodyless
            // parity so that whichever commit gave this shape its `Drop` body
            // would FAIL here and be forced to move the interpreter in the same
            // change, rather than shipping a compiled-only body and turning a
            // silent agreed-wrong into a run-vs-build divergence. Both halves
            // landed together; all four surfaces now print exactly one body.
            &["dR20", "ok"],
            "b14-discarded-tuple-shared-field",
        );

    // 2 — a DIRECT `String` element, which the first cut of this fix still
    //     leaked: it reused `tuple_elem_needs_deep_drop`, and that
    //     predicate CHOOSES between two walks at a `let`, where the one it
    //     declines still frees the element. In discard position there is no
    //     second walk, so the gate is the union of both let-site tests.
    assert_clean_asan_run(
        "fn seed() -> i64 { env.args().len() }\n\
             fn mkstr(i: i64) -> String { return f\"ssssssss{i}{seed()}\"; }\n\
             fn f(i: i64) -> (String, i64) { return (mkstr(i), 9); }\n\
             fn main() { f(3); println(\"ok\"); }\n",
        &["ok"],
        "b14-discarded-tuple-direct-string",
    );

    // 3 — UNBOUNDED: once per evaluation, not once per program.
    assert_clean_asan_run(
            "shared struct Inner { v: i64 }\n\
             struct R { id: i64, name: String, inner: Inner }\n\
             fn seed() -> i64 { env.args().len() }\n\
             fn mk(i: i64) -> R { return R { id: i, name: f\"hhhhhhhh{i}{seed()}\", inner: Inner { v: i } }; }\n\
             fn f(r: R) -> (R, i64) { return (r, 9); }\n\
             fn main() { let mut i = 0; while i < 8 { f(mk(i)); i = i + 1; } println(\"done\"); }\n",
            &["done"],
            "b14-discarded-tuple-loop",
        );

    // 4 — the ALL-POD control. The arm must decline outright here, leaving
    //     this program's codegen byte-for-byte what it was; a fix that
    //     registered a walk for every discarded tuple would be freeing a
    //     slot with nothing in it.
    assert_clean_asan_run(
        "fn seed() -> i64 { env.args().len() }\n\
             fn f(a: i64) -> (i64, i64) { return (a, 9); }\n\
             fn main() { f(seed()); println(\"ok\"); }\n",
        &["ok"],
        "b14-discarded-tuple-pod-control",
    );

    // 5 — the BOUND spelling of cell 1, which was already clean
    //     (B-2026-09-06-72) and must stay so: it proves the axis is the
    //     DISCARD position rather than the tuple return itself. A double
    //     free here would mean the new arm fires where a binding already
    //     owns the value.
    assert_clean_asan_run(
            "shared struct Inner { v: i64 }\n\
             struct R { id: i64, name: String, inner: Inner }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             fn seed() -> i64 { env.args().len() }\n\
             fn mk(i: i64) -> R { return R { id: i, name: f\"hhhhhhhh{i}{seed()}\", inner: Inner { v: i } }; }\n\
             fn f(r: R) -> (R, i64) { return (r, 9); }\n\
             fn main() { let z = f(mk(20)); println(f\"{z.0.inner.v}\"); }\n",
            &["20", "dR20"],
            "b14-bound-spelling-control",
        );
}

/// B-2026-09-15-16 — a plain struct moved into an OWNING SINK left the
/// source's heap field reading EMPTY on every compiled backend, against a
/// correct `--interp`: `Ws.Full(p)` then `println(p.s)` printed nothing, and
/// so did `v.push(p)` and `m.insert(1, p)`. Exit 0, `karac check` clean apart
/// from the advisory move warning, and valgrind-quiet — so nothing reported
/// it.
///
/// `UseAfterMove` is advisory on the compiled surface BY CONSTRUCTION, and
/// the promise that rests on is that the value the reuse reads is still
/// intact. The CALL-ARGUMENT spelling of the identical program honours it —
/// its entry copy is what keeps the source intact — so the same program was
/// right or wrong depending only on which sink it moved into.
///
/// THE MECHANISM is the whole-struct move suppression, which zeroes the
/// moved-out field's `len` as well as its `cap` (B-2026-07-10-1, for a drop
/// walk that is len-driven and not under the cap guard). `len` is what a
/// READ returns, so the source blanks.
///
/// NARROWING THAT ZERO IS NOT THE REPAIR, and this fixture exists partly to
/// keep that from being re-attempted: a sibling session implemented the
/// narrowing and measured it turning the blank read into a USE-AFTER-FREE
/// (`Invalid read of size 2`, `0 bytes inside a block of size 23 free'd`).
/// The source's `ptr` dangles the moment the destination frees; `len = 0` is
/// the only thing stopping a dereference, so the blank read was accidental
/// masking of a dangling pointer.
///
/// THE REPAIR IS THE DEFENSIVE COPY the call-argument spelling already
/// takes, added at the three OWNING sinks — variant constructor, `Vec.push`,
/// `Map.insert`. The machinery was already present and only its first half
/// was wired: `uam_defensive_copy` has a user-struct arm that duplicates the
/// heap fields and records `uam_copied_sites`, and
/// `suppress_source_vec_cleanup_for_arg_ex` already declines for a recorded
/// site. So the copy and the skip land as ONE PAIR by construction, which
/// matters — a copy at a destination that still takes the source's memory
/// strands the buffer instead.
///
/// THE LAST CELLS ARE CONTROLS. `alive` reads the source while the sink is
/// STILL LIVE, the timing that proves the blanking was the MOVE rather than
/// the sink's death. `call` is the oracle that was correct throughout and
/// must stay byte-identical. `e` pins a `Vec`-typed field and `d` a struct
/// with TWO heap fields — both blanked before, and both shapes a per-field
/// repair could have missed.
#[test]
fn asan_struct_moved_into_an_owning_sink_keeps_the_source_readable() {
    assert_clean_asan_run(
        r#"
struct P { s: String }
struct P2 { s: String, t: String }
struct Pv { v: Vec[String] }
enum Ws { Full(P), Empty }

fn wlen(w: Ws) -> i64 { match w { Ws.Full(q) => { return q.s.len(); } Ws.Empty => { return 0; } } }
fn takep(p: P) -> i64 { return p.s.len(); }
fn mkp(n: i64) -> P { return P { s: f"b1516-src-aaaaaaaaaaaa-{n}" }; }

fn main() {
    let a = mkp(1);
    { let w = Ws.Full(a); println(f"ctor {wlen(w)}"); }
    println(f"a {a.s}");

    let b = mkp(2);
    { let mut v: Vec[P] = []; v.push(b); println(f"push {v.len()}"); }
    println(f"b {b.s}");

    let c = mkp(3);
    { let mut m: Map[i64, P] = Map.new(); m.insert(1, c); println(f"insert {m.len()}"); }
    println(f"c {c.s}");

    let d = P2 { s: f"b1516-two-bbbbbbbbbbbb", t: f"b1516-two-cccccccccccc" };
    { let mut v: Vec[P2] = []; v.push(d); println(f"two {v.len()}"); }
    println(f"d {d.s} {d.t}");

    let e = Pv { v: [f"b1516-vecf-dddddddddddd"] };
    { let mut v: Vec[Pv] = []; v.push(e); println(f"vecf {v.len()}"); }
    println(f"e {e.v.len()}");

    let f = mkp(6);
    { let w = Ws.Full(f); println(f"alive {f.s}"); println(f"w {wlen(w)}"); }
    println(f"f {f.s}");

    let g = mkp(7);
    { println(f"call {takep(g)}"); }
    println(f"g {g.s}");
}
"#,
        &[
            "ctor 24",
            "a b1516-src-aaaaaaaaaaaa-1",
            "push 1",
            "b b1516-src-aaaaaaaaaaaa-2",
            "insert 1",
            "c b1516-src-aaaaaaaaaaaa-3",
            "two 1",
            "d b1516-two-bbbbbbbbbbbb b1516-two-cccccccccccc",
            "vecf 1",
            "e 1",
            "alive b1516-src-aaaaaaaaaaaa-6",
            "w 24",
            "f b1516-src-aaaaaaaaaaaa-6",
            "call 24",
            "g b1516-src-aaaaaaaaaaaa-7",
        ],
        "asan_struct_moved_into_an_owning_sink_keeps_the_source_readable",
    );
}

/// B-2026-09-16-27 — a NESTED-STRUCT field moved out of a match arm's
/// payload binding, or out of a `for` loop's aggregate element, double-freed
/// its inner buffer: `free(): double free detected in tcache 2` / valgrind
/// `Invalid free() … 0 bytes inside a block of size 24 free'd`, at BOTH opt
/// levels and with auto-par on and off, against a correct `--interp`.
///
/// THE ROW'S OWN LEAD IS REFUTED BY THE MINIMAL CASE, and that is worth
/// recording because it names B-2026-09-15-16's defensive copy as the thing
/// that fails to reach a nested field. It reaches it: instrumenting
/// `uam_defensive_copy` shows the struct arm firing on `Out` and
/// `deep_copy_struct_heap_fields_in_place` recursing into `In`. The minimal
/// reproducer has no use-after-move at all —
/// `let w = Wn.Full(Out { i: In { s: … } }); nlen(w)` — so that copy is not
/// on the path. The `arm` cell here is that reproducer.
///
/// THE TWO OWNERS, read straight off the IR. `nlen`'s arm emits
/// `call __karac_drop_struct_In(%i8)` and `call __karac_drop_Wn(%w)` ten
/// bytes apart, and the disarm between them writes `store i64 0` through
/// `%q` — a SEPARATE alloca filled by `extractvalue` from a register load of
/// `%w`. `suppress_struct_field_move_by_name` GEPs the root BINDING's slot,
/// and for these two roots that slot is a bit-copy of storage owned
/// elsewhere, so the zeroing lands where nobody frees through and `%w`'s
/// payload stays armed.
///
/// WHY THE FLAT SHAPE WAS ALREADY CLEAN, which is what hid this: a
/// `String` field of the same binding IS copied — by
/// `deep_copy_owned_struct_param_field_move`, whose `is_param_field` gate
/// admits `borrowed_agg_payload_struct_vars` explicitly. That helper is only
/// ever reached from the Vec/String `let` path, so a struct destination
/// never saw it. The repair is the shape symmetry, not a new policy.
///
/// THE TWO ROOTS DELIBERATELY NOT ADMITTED are the `param` cell (a by-value
/// struct param, entry-copied, so its own slot IS what its drop reads and
/// the disarm reaches the owner) and a `shared` enum payload. Both were
/// measured clean before the fix and would LEAK if copied here.
///
/// `push`, `ctor` and `call` are controls the row listed as NOT MEASURED and
/// that turn out to have been correct throughout — the `Vec.push` and
/// variant-constructor sinks reached for the same field, and the
/// call-argument spelling. `ctorsrc`/`src` pin B-2026-09-15-16's own shape
/// (the source stays readable after the move) so this fix cannot regress it.
#[test]
fn asan_nested_struct_field_moved_off_a_bitcopy_root_is_freed_once() {
    assert_clean_asan_run(
        r#"
struct In { s: String }
struct Out { i: In }
struct A3 { s: String }
struct B3 { a: A3 }
struct C3 { b: B3 }

enum Wn { Full(Out), Empty }
enum W3 { Full(C3), Empty }
enum Bx { One(In), None }

fn arm_move(w: Wn) -> i64 {
    match w { Wn.Full(q) => { let i = q.i; return i.s.len(); } Wn.Empty => { return 0; } }
}
fn arm_move_d3(w: W3) -> i64 {
    match w { W3.Full(q) => { let b = q.b; let a = b.a; return a.s.len(); } W3.Empty => { return 0; } }
}
fn arm_push(w: Wn) -> i64 {
    match w {
        Wn.Full(q) => { let mut v: Vec[In] = []; v.push(q.i); return v[0].s.len(); }
        Wn.Empty => { return 0; }
    }
}
fn arm_ctor(w: Wn) -> i64 {
    match w {
        Wn.Full(q) => {
            let b = Bx.One(q.i);
            match b { Bx.One(k) => { return k.s.len(); } Bx.None => { return 0; } }
        }
        Wn.Empty => { return 0; }
    }
}
fn ilen(i: In) -> i64 { return i.s.len(); }
fn arm_call(w: Wn) -> i64 {
    match w { Wn.Full(q) => { return ilen(q.i); } Wn.Empty => { return 0; } }
}
fn param_move(o: Out) -> i64 { let i = o.i; return i.s.len(); }

fn main() {
    println(f"arm {arm_move(Wn.Full(Out { i: In { s: f"b1627-arm-aaaaaaaaaaaa" } }))}");
    println(f"d3 {arm_move_d3(W3.Full(C3 { b: B3 { a: A3 { s: f"b1627-d3-bbbbbbbbbbbbb" } } }))}");
    println(f"push {arm_push(Wn.Full(Out { i: In { s: f"b1627-push-dddddddddddd" } }))}");
    println(f"ctor {arm_ctor(Wn.Full(Out { i: In { s: f"b1627-ctor-eeeeeeeeeeee" } }))}");

    let mut v: Vec[Out] = [];
    v.push(Out { i: In { s: f"b1627-loop-ffffffffffff" } });
    v.push(Out { i: In { s: f"b1627-loop-gggggggggggg" } });
    let mut total = 0;
    for e in v {
        let i = e.i;
        total = total + i.s.len();
    }
    println(f"loop {total}");

    println(f"param {param_move(Out { i: In { s: f"b1627-param-hhhhhhhhhhh" } })}");

    println(f"call {arm_call(Wn.Full(Out { i: In { s: f"b1627-call-jjjjjjjjjjjj" } }))}");

    let o = Out { i: In { s: f"b1627-live-iiiiiiiiiiii" } };
    { let w = Wn.Full(o); println(f"ctorsrc {arm_move(w)}"); }
    let k = o.i;
    println(f"src {k.s}");
}
"#,
        &[
            "arm 22",
            "d3 22",
            "push 23",
            "ctor 23",
            "loop 46",
            "param 23",
            "call 23",
            "ctorsrc 23",
            "src b1627-live-iiiiiiiiiiii",
        ],
        "asan_nested_struct_field_moved_off_a_bitcopy_root_is_freed_once",
    );
}

/// B-2026-09-16-5, THE SECOND FAMILY — an element moved OUT of a container
/// that lives inside a tuple (`return t.0[0];`). Found only because the
/// spelling of the callee's body was varied as an axis, which is the one
/// thing the cells above did not do.
///
/// THIS IS THE FIXTURE THAT WOULD HAVE CAUGHT THE REGRESSION IN KIND. The
/// leak fix on its own moves `let q = p; return q.0[0];` from a 17 B leak
/// to a use-after-free, because it ARMS a drop on the moved-to local while
/// the element that escapes by return is disarmed by nothing. Leak to UB is
/// the regression `506a91d` shipped in this same family and `49e75a8`
/// reverted, and every cell in the fixture above stays green through it.
///
/// AND FOUR OF THESE SIX CELLS WERE ALREADY BROKEN ON `main`, with no
/// change of this row's involved — measured on `origin/main`'s `src/` with
/// a marker count of 0 and a control canary proving the instrument was not
/// blind. On the compiled surfaces they ABORT with `free(): double free
/// detected in tcache 2`; the `String`-element spelling of the same cells
/// does not abort and reports `definitely lost: 0` with 2 invalid reads and
/// 1 invalid free, which every leak column in this file reads as clean.
/// That is why it survived: only an Invalid-read/free column sees it.
///
/// THE LANGUAGE ALLOWS THE MOVE, and the BARE spelling is what proves it
/// rather than an argument. The typechecker rejects `let s = a[0];` with
/// `E_INDEX_MOVE_NON_COPY` and ACCEPTS `return a[0];` and `W { a: a[0] }`,
/// where `suppress_array_elem_move_source` disarms the source and the cells
/// are clean. Wrapping that identical array in a tuple changes no
/// typechecker answer and loses the disarm, so the two spellings agreed
/// about what is legal and disagreed about what is emitted. The bare-array
/// and bare-Vec cells are carried below as controls for exactly that.
///
/// BOTH CONTAINER SPELLINGS, disarmed differently. An `Array[T, N]` element
/// sits at a constant offset in the tuple's own storage; a `Vec[T]` element
/// lives in the Vec's heap buffer and is reached through the loaded `data`
/// pointer. The bare `Vec` spelling is clean for a THIRD reason — a
/// defensive copy at the move site — so there was no Vec-element disarm to
/// extend and that arm had to be written.
///
/// THE `Drop` BODY STILL FIRES TWICE FOR THE MOVED-OUT ELEMENT, and the
/// cells below PIN that as the current answer rather than as the right one.
/// `dR1` appears before `got:1` and again after: the tuple's element-bodies
/// walk runs on the element that left, and only the MEMORY channel has a
/// move-out disarm — "bodies follow the move; memory does not"
/// (B-2026-08-28-57), here with the two channels the other way round. It is
/// NOT this fix's doing and NOT a backend divergence: `--interp` prints the
/// same doubled body on `origin/main`, so all four surfaces have agreed on
/// it all along. Filed separately; when it closes, these expectations lose
/// one `dR1` each and that is the signal, not a break.
///
/// What this fix does buy on those four cells is exact agreement with
/// `--interp`, where before the compiled surfaces aborted and the
/// interpreter did not.
#[test]
fn asan_tuple_container_elem_moved_out_has_one_owner() {
    const H: &str = "fn pay(i: i64) -> String { return f\"tttttttttttttttt{i}\" }\n";

    // The four PRE-EXISTING cells: no move to a local anywhere, so nothing
    // this row's leak fix touches can be what breaks them.
    assert_clean_asan_run(
        &format!(
            "{H}fn f(p: (Array[String, 2], i64)) -> String {{ return p.0[0]; }}\n\
                 fn main() {{\n\
                 \x20   let t: (Array[String, 2], i64) = ([pay(1), pay(2)], 7);\n\
                 \x20   println(f\"a0:{{f(t)}}\");\n\
                 }}\n"
        ),
        &["a0:tttttttttttttttt1"],
        "b165e-param-elem-returned",
    );
    // The TAIL spelling. Measured identical to the `return` one on every
    // arm — the statement/tail axis is NULL here — and kept because that
    // null is a measurement, and a later change could break one and not
    // the other.
    assert_clean_asan_run(
        &format!(
            "{H}fn f(p: (Array[String, 2], i64)) -> String {{ p.0[0] }}\n\
                 fn main() {{\n\
                 \x20   let t: (Array[String, 2], i64) = ([pay(1), pay(2)], 7);\n\
                 \x20   println(f\"a0:{{f(t)}}\");\n\
                 }}\n"
        ),
        &["a0:tttttttttttttttt1"],
        "b165e-param-elem-tail",
    );
    // NO PARAM AT ALL — a plain local tuple. This is the cell that shows
    // the family is not about parameter ownership.
    assert_clean_asan_run(
        &format!(
            "{H}fn mk() -> String {{\n\
                 \x20   let t: (Array[String, 2], i64) = ([pay(1), pay(2)], 7);\n\
                 \x20   return t.0[0];\n\
                 }}\n\
                 fn main() {{ println(f\"a0:{{mk()}}\"); }}\n"
        ),
        &["a0:tttttttttttttttt1"],
        "b165e-plain-local-elem-returned",
    );
    // The STRUCT-LITERAL position, the third site the two sibling disarms
    // are wired at. It reported 1 invalid free rather than the returns' 18
    // errors, which is the same defect with a shorter blast radius.
    assert_clean_asan_run(
        &format!(
            "{H}struct W2 {{ a: String }}\n\
                 fn main() {{\n\
                 \x20   let t: (Array[String, 2], i64) = ([pay(1), pay(2)], 7);\n\
                 \x20   let w = W2 {{ a: t.0[0] }};\n\
                 \x20   println(f\"a0:{{w.a}}\");\n\
                 }}\n"
        ),
        &["a0:tttttttttttttttt1"],
        "b165e-struct-literal-elem",
    );
    // The `Vec` spelling of the same escape — a DIFFERENT disarm, reaching
    // through the loaded data pointer.
    assert_clean_asan_run(
        &format!(
            "{H}fn f(p: (Vec[String], i64)) -> String {{ return p.0[0]; }}\n\
                 fn main() {{\n\
                 \x20   let t: (Vec[String], i64) = ([pay(1), pay(2)], 7);\n\
                 \x20   println(f\"a0:{{f(t)}}\");\n\
                 }}\n"
        ),
        &["a0:tttttttttttttttt1"],
        "b165e-vec-param-elem-returned",
    );
    // THE TWO CELLS THIS ROW'S LEAK FIX WOULD OTHERWISE REGRESS: a param
    // moved to a local, THEN an element returned. 17 B leak on `main`,
    // use-after-free with the leak fix alone, clean with the disarm.
    assert_clean_asan_run(
        &format!(
            "{H}fn f(p: (Array[String, 2], i64)) -> String {{ let q = p; return q.0[0]; }}\n\
                 fn main() {{\n\
                 \x20   let t: (Array[String, 2], i64) = ([pay(1), pay(2)], 7);\n\
                 \x20   println(f\"a0:{{f(t)}}\");\n\
                 }}\n"
        ),
        &["a0:tttttttttttttttt1"],
        "b165e-moved-local-elem-returned",
    );
    assert_clean_asan_run(
        &format!(
            "{H}fn f(p: (Vec[String], i64)) -> String {{ let q = p; q.0[0] }}\n\
                 fn main() {{\n\
                 \x20   let t: (Vec[String], i64) = ([pay(1), pay(2)], 7);\n\
                 \x20   println(f\"a0:{{f(t)}}\");\n\
                 }}\n"
        ),
        &["a0:tttttttttttttttt1"],
        "b165e-vec-moved-local-elem-tail",
    );

    // USER `Drop` ELEMENTS. Memory clean, and the body expectations below
    // PIN TODAY'S ANSWER, WHICH IS WRONG: `dR1` fires twice for the element
    // that was moved out. See this fixture's doc — the doubled body is
    // pre-existing on all four surfaces including `--interp`, is filed on
    // its own row, and when it closes these two expectations each lose
    // their trailing `dR1`.
    assert_clean_asan_run(
        "struct R2 { id: i64, s: String }\n\
             impl Drop for R2 { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             fn mkr(i: i64) -> R2 { return R2 { id: i, s: f\"ssssssssssssssss{i}\" } }\n\
             fn f(p: (Array[R2, 2], i64)) -> R2 { return p.0[0]; }\n\
             fn main() {\n\
             \x20   let t: (Array[R2, 2], i64) = ([mkr(1), mkr(2)], 7);\n\
             \x20   let r = f(t);\n\
             \x20   println(f\"got:{r.id}\");\n\
             \x20   println(\"end\");\n\
             }\n",
        &["dR1", "dR2", "got:1", "dR1", "end"],
        "b165e-user-drop-elem-returned",
    );
    assert_clean_asan_run(
        "struct R2 { id: i64, s: String }\n\
             impl Drop for R2 { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             fn mkr(i: i64) -> R2 { return R2 { id: i, s: f\"ssssssssssssssss{i}\" } }\n\
             fn f(p: (Vec[R2], i64)) -> R2 { return p.0[0]; }\n\
             fn main() {\n\
             \x20   let t: (Vec[R2], i64) = ([mkr(1), mkr(2)], 7);\n\
             \x20   let r = f(t);\n\
             \x20   println(f\"got:{r.id}\");\n\
             \x20   println(\"end\");\n\
             }\n",
        &["dR1", "dR2", "got:1", "dR1", "end"],
        "b165e-vec-user-drop-elem-returned",
    );

    // CONTROLS — the BARE spellings, which are clean on `main` and must
    // stay clean. They are what identifies the defect as the tuple
    // COMPOSITION rather than the move itself, and the bare `Vec` one is
    // the shape a disarm added in the wrong place would turn into a leak,
    // since its source keeps everything it had.
    assert_clean_asan_run(
        &format!(
            "{H}fn f(a: Array[String, 2]) -> String {{ return a[0]; }}\n\
                 fn main() {{\n\
                 \x20   let x: Array[String, 2] = [pay(1), pay(2)];\n\
                 \x20   println(f\"a0:{{f(x)}}\");\n\
                 }}\n"
        ),
        &["a0:tttttttttttttttt1"],
        "b165e-bare-array-elem-returned-control",
    );
    assert_clean_asan_run(
        &format!(
            "{H}fn mk() -> String {{\n\
                 \x20   let v: Vec[String] = [pay(1), pay(2)];\n\
                 \x20   return v[0];\n\
                 }}\n\
                 fn main() {{ println(f\"a0:{{mk()}}\"); }}\n"
        ),
        &["a0:tttttttttttttttt1"],
        "b165e-bare-vec-elem-returned-control",
    );
    // A SCALAR element: the new disarm must stay a no-op.
    assert_clean_asan_run(
        &format!(
            "{H}fn f(p: (Array[i64, 2], i64)) -> i64 {{ return p.0[1]; }}\n\
                 fn main() {{\n\
                 \x20   let t: (Array[i64, 2], i64) = ([3, 4], 7);\n\
                 \x20   println(f\"a1:{{f(t)}}\");\n\
                 }}\n"
        ),
        &["a1:4"],
        "b165e-scalar-elem-control",
    );
}

/// B-2026-09-15-30 — a discarded branch inside a GENERIC function cloned
/// a container element that nothing would free.
///
/// `compile_mono_function` never installed `discarded_branch_spans`, the
/// span set `compile_function` computes for every ordinary body, so while
/// a monomorph was being emitted the set held whatever the last
/// non-generic function left there — in practice empty, since the keys are
/// spans over a DIFFERENT body and a foreign span cannot match.
/// `branch_value_is_owned` answers `!discarded_branch_spans.contains(..)`,
/// so an empty set says every branch in a monomorph is OWNED, and the
/// arm-tail clone was emitted with no owner to free it.
///
/// This is the leak gate; the output twins in `tests/codegen.rs` and
/// `tests/interpreter.rs` cannot see it, because the leak changes no
/// output at all — the unfixed tree prints the right answer and loses
/// 216 B over these shapes.
///
/// All four discarding positions `compute_discarded_branch_spans`
/// documents are here, since each is recorded by its own rule and a fix
/// that installs the set covers all four at once only if the set is
/// really the thing that was missing.
///
/// The last two cells are the OPPOSITE direction, and they are why the
/// set is saved and restored rather than overwritten. A monomorph is
/// compiled INLINE inside its caller, so an overwrite hands the caller a
/// set keyed over the mono's body — which reads empty, turning the
/// caller's own discarded branch back into the same leak one frame up.
/// Measured: with the restore line disabled and everything else in place,
/// `callerdiscard` alone loses 27 B.
#[test]
fn asan_discarded_branch_in_a_generic_body_clones_nothing() {
    const DECLS: &str = "fn mkVec() -> Vec[String] {\n\
             \x20   let mut v: Vec[String] = Vec.new();\n\
             \x20   v.push(f\"aaaaaaaaaaaaaaaaaaaaaaaa-0\");\n\
             \x20   v.push(f\"bbbbbbbbbbbbbbbbbbbbbbbb-1\");\n\
             \x20   return v;\n\
             }\n\
             fn ident[T](t: T) -> T { return t; }\n";

    // The four discarding positions, each inside a generic body.
    assert_clean_asan_run(
            &format!(
                "{DECLS}\
                 fn stmtD[T](v: Vec[String], c: bool, t: T) -> T {{ if c {{ v[0] }} else {{ v[1] }}; return t; }}\n\
                 fn loopD[T](v: Vec[String], t: T) -> T {{ for i in 0..2 {{ if i == 0 {{ v[0] }} else {{ v[1] }} }} return t; }}\n\
                 fn blockD[T](v: Vec[String], c: bool, t: T) -> T {{ {{ if c {{ v[0] }} else {{ v[1] }} }}; return t; }}\n\
                 fn matchD[T](v: Vec[String], c: i64, t: T) -> T {{ match c {{ 0 => {{ v[0] }} _ => {{ v[1] }} }}; return t; }}\n\
                 fn main() {{\n\
                 \x20   println(f\"n{{stmtD(mkVec(), true, 1)}}\");\n\
                 \x20   println(f\"n{{loopD(mkVec(), 2)}}\");\n\
                 \x20   println(f\"n{{blockD(mkVec(), true, 3)}}\");\n\
                 \x20   println(f\"n{{matchD(mkVec(), 0, 4)}}\");\n\
                 \x20   println(\"end\");\n\
                 }}\n"
            ),
            &["n1", "n2", "n3", "n4", "end"],
            "b91530-discard-positions",
        );

    // A generic body whose branch value IS kept, and one generic calling
    // another. The kept cell is the double-free direction: installing the
    // set must not suppress a clone that has an owner.
    assert_clean_asan_run(
            &format!(
                "{DECLS}\
                 fn keptV[T](v: Vec[String], c: bool, t: T) -> T {{ let s = if c {{ v[0] }} else {{ v[1] }}; println(f\"k{{s.len()}}\"); return t; }}\n\
                 fn innerD[T](v: Vec[String], c: bool, t: T) -> T {{ if c {{ v[0] }} else {{ v[1] }}; return t; }}\n\
                 fn outerC[T](v: Vec[String], c: bool, t: T) -> T {{ let r = innerD(v, c, t); return r; }}\n\
                 fn main() {{\n\
                 \x20   println(f\"n{{keptV(mkVec(), true, 5)}}\");\n\
                 \x20   println(f\"n{{outerC(mkVec(), true, 6)}}\");\n\
                 \x20   println(\"end\");\n\
                 }}\n"
            ),
            &["k26", "n5", "n6", "end"],
            "b91530-kept-and-nested",
        );

    // The caller's OWN branches, with a generic call compiled inline just
    // before each. These are what the restore line protects.
    assert_clean_asan_run(
            &format!(
                "{DECLS}\
                 fn main() {{\n\
                 \x20   {{ let vc = mkVec(); let c = ident(true); if c {{ vc[0] }} else {{ vc[1] }}; println(f\"d{{vc[0].len()}}\"); }}\n\
                 \x20   {{ let vk = mkVec(); let k = ident(true); let s = if k {{ vk[0] }} else {{ vk[1] }}; println(f\"k{{s.len()}}\"); println(f\"r{{vk[0].len()}}\"); }}\n\
                 \x20   println(\"end\");\n\
                 }}\n"
            ),
            &["d26", "k26", "r26", "end"],
            "b91530-caller-spans",
        );
}

/// B-2026-09-16-18 — the MEMORY half of a fresh-temp struct scrutinee's
/// unbound fields, which the body-count fixtures cannot see.
///
/// The husk of `match S3 { a: mk(44), b: mk(45) } { S3 { a, .. } => .. }`
/// was owned by nobody: every field the arm did not bind lost its `Drop`
/// body AND leaked its buffer, on `--interp` / jit / aot / `AUTO_PAR=0`
/// alike. valgrind on the row's own four cells at `KARAC_OPT_LEVEL=0`
/// measured 24 allocs / 20 frees, `definitely lost: 12 bytes in 4 blocks`,
/// ERROR SUMMARY 4 — one 3-byte `name` buffer per unbound field, two of
/// them from the `S3 { .. }` cell that bound nothing at all. After the fix:
/// 28 allocs / 28 frees, ERROR SUMMARY 0.
///
/// Both halves have to be pinned separately, because each is invisible to
/// the other's instrument. A lost body is memory-balanced when the field
/// owns no heap, and a leaked buffer is body-silent when the arm's own
/// output does not change — here they happened to coincide, and the
/// coincidence is not something a later change preserves.
///
/// The loop is what makes the leak UNBOUNDED rather than a one-off, which
/// is the difference between a curiosity and a defect: pre-fix it stranded
/// one buffer per iteration.
#[test]
fn asan_freshtemp_struct_scrutinee_unbound_fields_are_freed() {
    assert_clean_asan_run_min_allocs(
        r#"
struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"name-{i}-padding" }; }
struct S3 { a: R, b: R }
fn mks() -> S3 { return S3 { a: mk(51), b: mk(52) }; }

fn temp_literal() -> i64 { match S3 { a: mk(44), b: mk(45) } { S3 { a, .. } => { return a.id; } } }
fn temp_call() -> i64 { match mks() { S3 { a, .. } => { return a.id; } } }
fn temp_all() -> i64 { match S3 { a: mk(47), b: mk(48) } { S3 { a, b } => { return a.id + b.id; } } }
fn temp_none() -> i64 { match S3 { a: mk(49), b: mk(50) } { S3 { .. } => { return 1; } } }

fn main() {
    println(f"L:{temp_literal()}");
    println(f"C:{temp_call()}");
    println(f"A:{temp_all()}");
    println(f"N:{temp_none()}");
    let mut i = 0;
    while i < 3 {
        println(f"W:{temp_none()}");
        i = i + 1;
    }
    println("done");
}
"#,
        &[
            "dR44", "dR45", "L:44", "dR51", "dR52", "C:51", "dR48", "dR47", "A:95", "dR50", "dR49",
            "N:1", "dR50", "dR49", "W:1", "dR50", "dR49", "W:1", "dR50", "dR49", "W:1", "done",
        ],
        "asan_freshtemp_struct_scrutinee_unbound_fields_are_freed",
        12,
    );
}
