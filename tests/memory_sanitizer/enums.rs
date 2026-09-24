//! enum definitions, variants, discriminants, payloads -- fixtures for `tests/memory_sanitizer.rs`.
//!
//! Split out of `tests/memory_sanitizer.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test memory_sanitizer` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test memory_sanitizer enums::
//!
//! New fixtures about enum definitions, variants, discriminants, payloads belong in this file.

use super::*;

/// B-2026-09-01-34 — a discarded struct-variant literal in a two-tail `if`
/// or a `match` arm, under ASAN + LSan.
///
/// This row was filed `leak` on STRUCTURAL grounds rather than a
/// measurement: the compiled backends emitted no body at all for the shape,
/// not even the enum's OWN, which means the value reached its death with no
/// owner registered -- and an unowned aggregate is how heap goes
/// unreclaimed. The row said the heap half still needed measuring.
///
/// This case is that measurement, in the direction that matters now: with
/// the owner registered the payload's `String` and `Vec` are reclaimed and
/// LSan is clean on Linux. The transcript is asserted alongside so a
/// regression that drops the registration again fails here on output even
/// where a leak alone might not trip the checker.
/// B-2026-09-07-44 — A FRESH-TEMP ENUM SCRUTINEE WHOSE CONSUMING ARM BINDS
/// A COPY-DECLINED STRUCT PAYLOAD LEAKED THE PAYLOAD'S INTERIOR.
///
/// `bind_pattern_values`'s user-struct arm registers an owner only when the
/// payload is copy-supported — a PROXY for "the source is callee-owned".
/// B-2026-09-07-38 admitted the transfer-owned param as a second such
/// source; a fresh owning temp is the third, and for the same reason: it
/// has no name, so by construction no later reader and no second owner for
/// a use-after-free to race. The consuming arm zeroes the temp's payload
/// words and frees the envelope, leaving the CONTENTS to a binding the gate
/// had refused to register.
///
/// ALL THREE SPELLINGS ARE HERE because the defect needed all three of a
/// fresh-temp scrutinee, an arm binding the WHOLE payload, and a payload the
/// entry copy declines — the `let`-bound scrutinee and the `W.T(_)` arm were
/// already clean, and a fix that broke either would be trading this leak for
/// a double free.
///
/// THE SUITE'S DEFAULT OPT LEVEL CANNOT SEE THIS. Nothing reads the binding,
/// so at -O2 LLVM elides the allocation outright and all three spellings
/// compile to one BYTE-IDENTICAL binary that valgrind calls clean. Measured
/// at `KARAC_OPT_LEVEL=0`: 10 allocs / 9 frees before the fix, 10 / 10
/// after — a restored free, not a removed allocation. This case therefore
/// earns its keep on the `-O0` leg (`scripts/asan-o0-leg.sh`); on the
/// default leg it is a transcript assertion only.
#[test]
fn asan_fresh_temp_enum_scrutinee_binding_copy_declined_payload_owns_it() {
    assert_clean_asan_run(
            "struct X1 { a: Option[i64], s: String }\n\
             enum W { T(X1), U(i64) }\n\
             fn mkx(i: i64) -> X1 { return X1 { a: Option.Some(i), s: f\"s{i}\" }; }\n\
             fn main() {\n\
             \x20   match W.T(mkx(1)) { W.T(x) => println(f\"t{x.a.unwrap_or(0)}\"), W.U(n) => println(\"u\") }\n\
             \x20   let w = W.T(mkx(2));\n\
             \x20   match w { W.T(x) => println(f\"l{x.a.unwrap_or(0)}\"), W.U(n) => println(\"u\") }\n\
             \x20   match W.T(mkx(3)) { W.T(_) => println(\"w\"), W.U(n) => println(\"u\") }\n\
             \x20   println(\"end\")\n\
             }\n",
            &["t1", "l2", "w", "end"],
            "b44-fresh-temp-enum-payload-binding",
        );
}

#[test]
fn asan_discarded_branch_struct_variant_literal_owns_its_payload() {
    assert_clean_asan_run(
            "struct R { id: i64, s: String, xs: Vec[i64] }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}:{self.s}:{self.xs.len()}\") } }\n\
             enum Sv { Hold { inner: R }, Nil }\n\
             impl Drop for Sv { fn drop(mut ref self) { println(\"dSv\") } }\n\
             fn main() {\n\
             \x20   let c = true;\n\
             \x20   let _ = if c { Sv.Hold { inner: R { id: 7, s: \"pay\", xs: [1, 2, 3] } } } else { Sv.Nil };\n\
             \x20   let n = 1;\n\
             \x20   let _ = match n { 1 => { Sv.Hold { inner: R { id: 8, s: \"pay2\", xs: [4, 5] } } } _ => { Sv.Nil } };\n\
             \x20   println(\"end\")\n\
             }\n",
            &["dSv", "dR7:pay:3", "dSv", "dR8:pay2:2", "end"],
            "b34-discarded-branch-struct-variant",
        );
}

/// B-2026-09-06-36 — the MEMORY half of
/// `e2e_unconsumed_enum_leaf_of_a_local_struct_scrutinee_runs_its_body`
/// (tests/codegen.rs), pinning the half that was never broken.
///
/// That row is BODY-only and the row says so: valgrind was clean at `-O0`
/// before the fix and is clean after (40 allocs / 40 frees, ERROR SUMMARY
/// 0). It is pinned anyway because the fix hands a bodies-only walker to a
/// binding whose heap the memory half already gave it, and getting that
/// pairing wrong is a use-after-free rather than a miscount — an output
/// assertion would not catch it, since a body reading freed bytes still
/// prints.
#[test]
fn asan_unconsumed_enum_leaf_of_a_local_struct_scrutinee_is_balanced() {
    assert_clean_asan_run(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("  dE") } }
struct H1 { e: E }
struct H2 { r: R }
struct H3 { e: E, k: i64 }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
fn eat(e: E) -> i64 { match e { E.A(r) => { return r.id; } E.B => { return 0; } } }

fn unread() { let c: H1 = H1 { e: E.A(mk(1)) }; match c { H1 { e } => { println("  m"); } } }
fn bound_result() -> i64 { let c: H1 = H1 { e: E.A(mk(2)) }; let k: i64 = match c { H1 { e } => { 9 } }; return k; }
fn iflet() { let c: H1 = H1 { e: E.A(mk(3)) }; if let H1 { e } = c { println("  i"); } }
fn read_only() -> i64 { let c: H3 = H3 { e: E.A(mk(4)), k: 5 }; match c { H3 { e, k } => { return k; } } }
fn consumed() -> i64 { let c: H1 = H1 { e: E.A(mk(6)) }; match c { H1 { e } => { return eat(e); } } }
fn by_value_param(h: H1) -> i64 { match h { H1 { e } => { return 9; } } }
fn struct_leaf() { let c: H2 = H2 { r: mk(8) }; match c { H2 { r } => { println("  s"); } } }
fn wildcard() { let c: H1 = H1 { e: E.A(mk(9)) }; match c { H1 { e: _ } => { println("  w"); } } }

fn main() {
    println("unread"); unread();
    println("bound_result"); let a: i64 = bound_result(); println(f"  ={a}");
    println("iflet"); iflet();
    println("read_only"); let b: i64 = read_only(); println(f"  ={b}");
    println("consumed"); let c2: i64 = consumed(); println(f"  ={c2}");
    println("by_value_param"); let d: i64 = by_value_param(H1 { e: E.A(mk(7)) }); println(f"  ={d}");
    println("struct_leaf"); struct_leaf();
    println("wildcard"); wildcard();
    println("end");
}
"#,
        &[
            "unread",
            "  m",
            "  dE",
            "  dR1",
            "bound_result",
            "  dE",
            "  dR2",
            "  =9",
            "iflet",
            "  i",
            "  dE",
            "  dR3",
            "read_only",
            "  dE",
            "  dR4",
            "  =5",
            "consumed",
            "  dE",
            "  dR6",
            "  =6",
            "by_value_param",
            "  dE",
            "  dR7",
            "  =9",
            "struct_leaf",
            "  s",
            "  dR8",
            "wildcard",
            "  w",
            "  dE",
            "  dR9",
            "end",
        ],
        "unconsumed_enum_leaf_of_a_local_struct_scrutinee_is_balanced",
    );
}

/// B-2026-09-06-38 — the fresh-temp ENUM receiver's bodies are new
/// registrations on the same frame as its `track_enum_var` free: the
/// shell's own `E.drop` for an owned `self`, and shell + payload-bodies walk
/// for a `ref self`. Both are bodies-only fns registered AFTER the free so
/// they drain BEFORE it (LIFO), the struct arm's load-bearing order; this
/// pins that every body reads live storage and nothing frees twice across
/// the whole cell battery of `e2e_fresh_temp_owned_enum_receiver_runs_the_shell_body`.
/// valgrind measured 0 errors at -O0 and -O2 before this landed as a test.
///
/// B-2026-09-06-39 — REPINNED (stdout only; ASAN stayed clean throughout).
/// `read/*`, `print/temp` and `iflet/temp` put the shell's body first now. `r/*`
/// (a hand-back) and `chain/temp` are unchanged — the chain link keeps its
/// payload body because the fresh-temp registrar was widened to admit a
/// MethodCall receiver for that walk.
#[test]
fn asan_fresh_temp_owned_enum_receiver_runs_the_shell_body_balanced() {
    assert_clean_asan_run(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("  dE") } }
struct W { e: E }
enum F { A(R), B }
impl E {
    fn m_read(self) -> i64 { match self { E.A(r) => { return r.id; } E.B => { return 0; } } }
    #[allow(partial_move_of_drop_enum)]
    fn m_r(self) -> R { match self { E.A(r) => { return r; } E.B => { return mk(0); } } }
    fn m_print(self) { match self { E.A(r) => { println(f"  p{r.id}"); } E.B => { println("  pB"); } } }
    fn m_ref(ref self) -> i64 { match self { E.A(r) => { return r.id; } E.B => { return 0; } } }
    fn m_iflet(self) -> i64 { if let E.A(r) = self { return r.id; } else { return 0; } }
    fn me(self) -> E { return self; }
    fn wrap(self) -> W { return W { e: self }; }
    fn m_opt(self) -> Option[R] { match self { E.A(r) => { return Some(r); } E.B => { return None; } } }
    fn m_optself(self) -> Option[E] { return Some(self); }
    fn m_mut(mut ref self) -> i64 { match self { E.A(r) => { return r.id; } E.B => { return 0; } } }
}
impl F {
    fn m_read(self) -> i64 { match self { F.A(r) => { return r.id; } F.B => { return 0; } } }
    fn m_ref(ref self) -> i64 { match self { F.A(r) => { return r.id; } F.B => { return 0; } } }
}
fn main() {
    println("read/local"); let a = E.A(mk(1)); let x = a.m_read(); println(f"  x{x}");
    println("read/temp"); let x2 = E.A(mk(2)).m_read(); println(f"  x{x2}");
    println("r/local"); let b = E.A(mk(3)); let y = b.m_r(); println(f"  y{y.id}");
    println("r/temp"); let y2 = E.A(mk(4)).m_r(); println(f"  y{y2.id}");
    println("print/temp"); E.A(mk(5)).m_print(); println("  after");
    println("ref/temp"); let x6 = E.A(mk(6)).m_ref(); println(f"  x{x6}");
    println("iflet/temp"); let x7 = E.A(mk(7)).m_iflet(); println(f"  x{x7}");
    println("unit/temp"); let x8 = E.B.m_read(); println(f"  x{x8}");
    println("noshell/temp"); let x9 = F.A(mk(8)).m_read(); println(f"  x{x9}");
    println("me/temp"); let e = E.A(mk(9)).me(); println("  held");
    println("wrap/temp"); let w = E.A(mk(10)).wrap(); println("  held");
    println("chain/temp"); let x11 = E.A(mk(11)).me().m_read(); println(f"  x{x11}");
    println("ref/local"); let c = E.A(mk(12)); let x12 = c.m_ref(); println(f"  x{x12}");
    println("refnoshell/temp"); let x13 = F.A(mk(13)).m_ref(); println(f"  x{x13}");
    println("refnoshell/local"); let d = F.A(mk(14)); let x14 = d.m_ref(); println(f"  x{x14}");
    println("opt/temp"); let o16 = E.A(mk(16)).m_opt(); println("  held");
    println("optself/temp"); let o17 = E.A(mk(17)).m_optself(); println("  held");
    println("mut/temp"); let x18 = E.A(mk(18)).m_mut(); println(f"  x{x18}");
    println("end");
}
"#,
        &[
            "read/local",
            "  dE",
            "  dR1",
            "  x1",
            "read/temp",
            "  dE",
            "  dR2",
            "  x2",
            "r/local",
            "  dE",
            "  y3",
            "  dR3",
            "r/temp",
            "  dE",
            "  y4",
            "  dR4",
            "print/temp",
            "  p5",
            "  dE",
            "  dR5",
            "  after",
            "ref/temp",
            "  dE",
            "  dR6",
            "  x6",
            "iflet/temp",
            "  dE",
            "  dR7",
            "  x7",
            "unit/temp",
            "  dE",
            "  x0",
            "noshell/temp",
            "  dR8",
            "  x8",
            "me/temp",
            "  dE",
            "  dR9",
            "  held",
            "wrap/temp",
            "  dE",
            "  dR10",
            "  held",
            "chain/temp",
            "  dR11",
            "  x11",
            "ref/local",
            "  dE",
            "  dR12",
            "  x12",
            "refnoshell/temp",
            "  dR13",
            "  x13",
            "refnoshell/local",
            "  dR14",
            "  x14",
            "opt/temp",
            "  dE",
            "  dR16",
            "  held",
            "optself/temp",
            "  dE",
            "  dR17",
            "  held",
            "mut/temp",
            "  dE",
            "  dR18",
            "  x18",
            "end",
        ],
        "asan_fresh_temp_owned_enum_receiver_runs_the_shell_body_balanced",
    );
}

/// B-2026-09-06-17 — the MEMORY half of
/// `tests/codegen.rs`'s `e2e_projected_enum_payload_handed_out_runs_one_body`:
/// the same program under ASAN + LSan. The payload handed out of a
/// projected enum now has one body owner; this pins that masking it in the
/// caller's walk — in place on a named binding, payload-only in a fresh
/// temp's walker — left exactly one owner of its `String` / `Vec` buffers
/// and freed nothing twice, on every spelling and both receiver kinds.
#[test]
fn asan_projected_enum_payload_handed_out_is_single_owner() {
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
             \x20   fn out(self) -> R { match self.e { E.A(r) => { return r; } E.B => { return mk(0); } } }\n\
             #[allow(partial_move_of_drop_enum)]\n\
             \x20   fn out_iflet(self) -> R { if let E.A(r) = self.e { return r; } else { return mk(0); } }\n\
             #[allow(partial_move_of_drop_enum)]\n\
             \x20   fn out_tail(self) -> R { match self.e { E.A(r) => r, E.B => mk(0) } }\n\
             #[allow(partial_move_of_drop_enum)]\n\
             \x20   fn out_some(self, k: bool) -> R { match self.e { E.A(r) => { if k { return r; } return mk(1); } E.B => { return mk(0); } } }\n\
             #[allow(partial_move_of_drop_enum)]\n\
             \x20   fn read(self) -> i64 { match self.e { E.A(r) => { return r.id; } E.B => { return 0; } } }\n\
             }\n\
             impl H2 { #[allow(partial_move_of_drop_enum)] fn out2(self) -> R { match self.s.e { E.A(r) => { return r; } E.B => { return mk(0); } } } }\n\
             #[allow(partial_move_of_drop_enum)]\n\
             fn p_out(h: H1) -> R { match h.e { E.A(r) => { return r; } E.B => { return mk(0); } } }\n\
             #[allow(partial_move_of_drop_enum)]\n\
             fn p_out2(h: H2) -> R { match h.s.e { E.A(r) => { return r; } E.B => { return mk(0); } } }\n\
             fn p_read(h: H1) -> i64 { match h.e { E.A(r) => { return r.id; } E.B => { return 0; } } }\n\
             #[allow(partial_move_of_drop_enum)]\n\
             fn e_out(b: E) -> R { match b { E.A(r) => { return r; } E.B => { return mk(0); } } }\n\
             \n\
             fn main() {\n\
             \x20   println(\"out/local\"); let a1 = H1 { e: E.A(mk(1)) }; let r1 = a1.out(); println(f\"  got{r1.id}\");\n\
             \x20   println(\"out/temp\"); let r2 = H1 { e: E.A(mk(2)) }.out(); println(f\"  got{r2.id}\");\n\
             \x20   println(\"out_iflet/local\"); let a3 = H1 { e: E.A(mk(3)) }; let r3 = a3.out_iflet(); println(f\"  got{r3.id}\");\n\
             \x20   println(\"out_tail/local\"); let a4 = H1 { e: E.A(mk(4)) }; let r4 = a4.out_tail(); println(f\"  got{r4.id}\");\n\
             \x20   println(\"out_some/taken\"); let a5 = H1 { e: E.A(mk(5)) }; let r5 = a5.out_some(true); println(f\"  got{r5.id}\");\n\
             \x20   println(\"out_some/not\"); let a6 = H1 { e: E.A(mk(6)) }; let r6 = a6.out_some(false); println(f\"  got{r6.id}\");\n\
             \x20   println(\"read/local\"); let a7 = H1 { e: E.A(mk(7)) }; let x7 = a7.read(); println(f\"  r{x7}\");\n\
             \x20   println(\"out2/local\"); let a8 = H2 { s: S { e: E.A(mk(8)) } }; let r8 = a8.out2(); println(f\"  got{r8.id}\");\n\
             \x20   println(\"p_out/local\"); let a9 = H1 { e: E.A(mk(9)) }; let r9 = p_out(a9); println(f\"  got{r9.id}\");\n\
             \x20   println(\"p_out/temp\"); let r10 = p_out(H1 { e: E.A(mk(10)) }); println(f\"  got{r10.id}\");\n\
             \x20   println(\"p_out2/local\"); let a11 = H2 { s: S { e: E.A(mk(11)) } }; let r11 = p_out2(a11); println(f\"  got{r11.id}\");\n\
             \x20   println(\"p_read/local\"); let a12 = H1 { e: E.A(mk(12)) }; let x12 = p_read(a12); println(f\"  r{x12}\");\n\
             \x20   println(\"e_out/local\"); let a13 = E.A(mk(13)); let r13 = e_out(a13); println(f\"  got{r13.id}\");\n\
             \x20   println(\"end\");\n\
             }\n",
            &[
                "out/local",
                "  dE",
                "  got1",
                "  dR1",
                "out/temp",
                "  got2",
                "  dR2",
                "out_iflet/local",
                "  dE",
                "  got3",
                "  dR3",
                "out_tail/local",
                "  dE",
                "  got4",
                "  dR4",
                "out_some/taken",
                "  dE",
                "  got5",
                "  dR5",
                "out_some/not",
                "  dE",
                "  got1",
                "  dR1",
                "read/local",
                "  dE",
                "  dR7",
                "  r7",
                "out2/local",
                "  dE",
                "  got8",
                "  dR8",
                "p_out/local",
                "  dE",
                "  got9",
                "  dR9",
                "p_out/temp",
                "  dE",
                "  got10",
                "  dR10",
                "p_out2/local",
                "  dE",
                "  got11",
                "  dR11",
                "p_read/local",
                "  dE",
                "  dR12",
                "  r12",
                "e_out/local",
                "  dE",
                "  got13",
                "  dR13",
                "end",
            ],
            "b17-projected-enum-payload-handed-out",
        );
}

/// B-2026-09-16-31 — the MEMORY half of `tests/codegen.rs`'s
/// `e2e_generic_enum_with_generic_drop_impl_survives_an_owned_self_method`
/// under ASAN + LSan, same fifteen cells and same transcript.
///
/// The row was a SIGSEGV, so the free side is the half that has to stay
/// pinned: two of its four defects were an owner registered twice (the
/// monomorph's `self` prologue against the caller's binding; the generic
/// impl's arm channel against the named receiver), and two more were owners
/// missing entirely (a `G[R]` temp receiver's box and interior, 56 + 11
/// bytes per call under valgrind at `-O0`, which the CONCRETE `impl G[R]`
/// spelling leaked before this row too).
///
/// The fifth hazard is the one only a sanitizer can hold: instantiating
/// `G.drop$R` from inside a `let` cleared the caller's `payload_vars`
/// tables, so the NEXT statement's by-value call moved the box to the
/// callee and left the caller's box drop armed — three invalid reads and an
/// invalid free of one 56-byte block, and clean at `-O2` only because the
/// optimizer removes the pair. A transcript fixture cannot see that; this
/// one can.
#[test]
fn asan_generic_enum_with_generic_drop_impl_keeps_one_owner() {
    assert_clean_asan_run(
            "struct R { id: i64, tag: String, xs: Vec[i64] }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"  dR{self.id}\") } }\n\
             fn mk(i: i64) -> R { return R { id: i, tag: f\"t{i}\", xs: [i] } }\n\
             \n\
             enum G[T] { X(T), Y }\n\
             impl[T] Drop for G[T] { fn drop(mut ref self) { println(\"  dG\") } }\n\
             impl[T] G[T] {\n\
             \x20\x20\x20\x20fn gread(self) -> i64 { match self { G.X(t) => { return 1; } G.Y => { return 0; } } }\n\
             \x20\x20\x20\x20fn gnone(self) -> i64 { return 5 }\n\
             }\n\
             \n\
             enum K[T] { X(T), Y }\n\
             impl Drop for K[R] { fn drop(mut ref self) { println(\"  dK\") } }\n\
             impl[T] K[T] { fn kread(self) -> i64 { match self { K.X(t) => { return 1; } K.Y => { return 0; } } } }\n\
             \n\
             enum H[T] { X(T), Y }\n\
             impl[T] H[T] {\n\
             \x20\x20\x20\x20fn hnone(self) -> i64 { return 5 }\n\
             \x20\x20\x20\x20fn hread(self) -> i64 { match self { H.X(t) => { return 1; } H.Y => { return 0; } } }\n\
             }\n\
             \n\
             enum P[T] { X(T), Y }\n\
             impl P[R] { fn pnone(self) -> i64 { return 5 } }\n\
             fn mkp(i: i64) -> P[R] { return P.X(mk(i)) }\n\
             \n\
             enum E { A(R), B }\n\
             impl Drop for E { fn drop(mut ref self) { println(\"  dE\") } }\n\
             impl E {\n\
             \x20\x20\x20\x20fn eread(self) -> i64 { match self { E.A(t) => { return 1; } E.B => { return 0; } } }\n\
             \x20\x20\x20\x20fn enone(self) -> i64 { return 5 }\n\
             }\n\
             \n\
             struct S[T] { v: T }\n\
             impl[T] Drop for S[T] { fn drop(mut ref self) { println(\"  dS\") } }\n\
             impl[T] S[T] { fn snone(self) -> i64 { return 5 } }\n\
             \n\
             fn main() {\n\
             \x20\x20\x20\x20println(\"repro\");  { let g: G[R] = G.X(mk(20)); println(f\"  x{g.gread()}\") }\n\
             \x20\x20\x20\x20println(\"gnone\");  { let g: G[R] = G.X(mk(21)); println(f\"  x{g.gnone()}\") }\n\
             \x20\x20\x20\x20println(\"glocal\"); { let g: G[i64] = G.X(7); println(\"  x1\") }\n\
             \x20\x20\x20\x20println(\"gnarrow\");{ let g: G[i64] = G.X(7); println(f\"  x{g.gnone()}\") }\n\
             \x20\x20\x20\x20println(\"kread\");  { let k: K[R] = K.X(mk(22)); println(f\"  x{k.kread()}\") }\n\
             \x20\x20\x20\x20println(\"hnone\");  { let h: H[R] = H.X(mk(23)); println(f\"  x{h.hnone()}\") }\n\
             \x20\x20\x20\x20println(\"hread\");  { let h: H[R] = H.X(mk(24)); println(f\"  x{h.hread()}\") }\n\
             \x20\x20\x20\x20println(\"htemp\");  { println(f\"  x{H.X(mk(25)).hnone()}\") }\n\
             \x20\x20\x20\x20println(\"pnone\");  { let p: P[R] = P.X(mk(26)); println(f\"  x{p.pnone()}\") }\n\
             \x20\x20\x20\x20println(\"ptemp\");  { println(f\"  x{P.X(mk(27)).pnone()}\") }\n\
             \x20\x20\x20\x20println(\"pcall\");  { println(f\"  x{mkp(28).pnone()}\") }\n\
             \x20\x20\x20\x20println(\"twomono\");{ let a: G[R] = G.X(mk(29)); println(f\"  x{a.gnone()}\"); let b: G[i64] = G.X(7); println(f\"  y{b.gnone()}\") }\n\
             \x20\x20\x20\x20println(\"enone\");  { let e: E = E.A(mk(30)); println(f\"  x{e.enone()}\") }\n\
             \x20\x20\x20\x20println(\"eread\");  { let e: E = E.A(mk(31)); println(f\"  x{e.eread()}\") }\n\
             \x20\x20\x20\x20println(\"snone\");  { let s: S[R] = S { v: mk(32) }; println(f\"  x{s.snone()}\") }\n\
             \x20\x20\x20\x20println(\"end\")\n\
             }\n\
",
            &[
                "repro",
                "  x1",
                "  dG",
                "  dR20",
                "gnone",
                "  x5",
                "  dG",
                "  dR21",
                "glocal",
                "  dG",
                "  x1",
                "gnarrow",
                "  x5",
                "  dG",
                "kread",
                "  x1",
                "  dK",
                "  dR22",
                "hnone",
                "  x5",
                "  dR23",
                "hread",
                "  dR24",
                "  x1",
                "htemp",
                "  dR25",
                "  x5",
                "pnone",
                "  x5",
                "  dR26",
                "ptemp",
                "  dR27",
                "  x5",
                "pcall",
                "  dR28",
                "  x5",
                "twomono",
                "  x5",
                "  dG",
                "  dR29",
                "  y5",
                "  dG",
                "enone",
                "  x5",
                "  dE",
                "  dR30",
                "eread",
                "  x1",
                "  dE",
                "  dR31",
                "snone",
                "  x5",
                "  dS",
                "  dR32",
                "end",
            ],
            "generic_enum_generic_drop_one_owner",
        );
}

/// B-2026-09-10-20 — the MEMORY half of `tests/codegen.rs`'s
/// `e2e_enum_container_payload_in_struct_field_runs_element_drop_bodies` under
/// ASAN + LSan.
///
/// The transcript twin proves the bodies now RUN at the struct-field position;
/// this proves they run exactly once. The fix admits a field into
/// `user_drop_field_indices_mono` that the gate previously excluded, so the
/// parent's bodies walker is emitted where none was before and reaches the enum's
/// payload container through `emit_enum_payload_user_drop_bodies_fn`. Every way
/// that can be wrong — a walk that duplicates the element bodies the value's own
/// scope-exit channel already runs, a GEP at the wrong field index, a handle read
/// after the payload moved — lands here as a double free or a use-after-free
/// rather than as a transcript diff.
///
/// `f-second` is the cell that pins the index (the enum sits at field 1 behind a
/// scalar) and `f-bind` the one that pins a field initialized from a named local
/// rather than a fresh temp, which is the spelling where the local's own drop and
/// the parent's could both claim the elements.
///
/// `b-unit` holds the no-payload path, and the whole-program valgrind profile at
/// `KARAC_OPT_LEVEL=0` is byte-identical before and after the fix — the change is
/// on the BODIES channel, which is why no sanitizer leg could have caught the
/// defect it closes.
///
/// THE `b-arrenum` CELL OF THE TRANSCRIPT TWINS IS DELIBERATELY ABSENT HERE,
/// for the reason `gensh` is absent from
/// `asan_declared_vec_enum_payload_elements_keep_one_owner` one row over.
/// `En.P([Mono.P(mkr(13))])` in a struct field strands 40 B + 3 B, and that
/// leak is the direct consequence of the silence the twins pin: a `Drop` body
/// that never runs is a payload that is never freed. Measured IDENTICALLY on
/// the parent tree and on this one, per cell under valgrind at
/// `KARAC_OPT_LEVEL=0`, so it is this commit's BOUNDARY rather than its
/// doing. Carrying it here reddens the `-O0` ratchet leg for something the
/// commit does not touch -- it did, on this fixture's first run, which is
/// what that leg is for -- and quarantining it would put a live entry on a
/// list that is otherwise fully drained. It stays pinned in the transcript
/// fixtures, where it is visible without being load-bearing, and it is the
/// cell that must move when its own row closes.
#[test]
fn asan_enum_container_payload_in_struct_field_keeps_one_owner() {
    assert_clean_asan_run(
            "struct R { id: i64, s: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"d{self.id}\") } }\n\
             fn mkr(i: i64) -> R { return R { id: i, s: f\"aaa\" } }\n\
             enum Mono { P(R), Q }\n\
             enum Ea { P(Array[R, 2]), Q }\n\
             enum Ev { P(Vec[R]), Q }\n\
             enum Em { P(Vec[Mono]), Q }\n\
             enum Et { P((R, R)), Q }\n\
             struct Ha { h: Ea }\n\
             struct Hv { h: Ev }\n\
             struct Hm { h: Em }\n\
             struct Ht { h: Et }\n\
             struct Hw { lead: i64, h: Ea }\n\
             \n\
             fn main() {\n\
             \x20\x20\x20\x20println(\"f-arr\");   { let a: Array[R, 2] = [mkr(1), mkr(2)]; let g = Ha { h: Ea.P(a) }; println(\"m\") }\n\
             \x20\x20\x20\x20println(\"f-vec\");   { let mut w: Vec[R] = []; w.push(mkr(3)); let g = Hv { h: Ev.P(w) }; println(\"m\") }\n\
             \x20\x20\x20\x20println(\"f-venum\"); { let mut w: Vec[Mono] = []; w.push(Mono.P(mkr(4))); let g = Hm { h: Em.P(w) }; println(\"m\") }\n\
             \x20\x20\x20\x20println(\"f-bind\");  { let a: Array[R, 2] = [mkr(5), mkr(6)]; let h = Ea.P(a); let g = Ha { h: h }; println(\"m\") }\n\
             \x20\x20\x20\x20println(\"f-second\"); { let a: Array[R, 2] = [mkr(7), mkr(8)]; let g = Hw { lead: 9, h: Ea.P(a) }; println(\"m\") }\n\
             \x20\x20\x20\x20println(\"l-arr\");   { let a: Array[R, 2] = [mkr(10), mkr(11)]; let h = Ea.P(a); println(\"m\") }\n\
             \x20\x20\x20\x20println(\"l-vec\");   { let mut w: Vec[R] = []; w.push(mkr(12)); let h = Ev.P(w); println(\"m\") }\n\
             \x20\x20\x20\x20println(\"b-tuple\");  { let g = Ht { h: Et.P((mkr(14), mkr(15))) }; println(\"m\") }\n\
             \x20\x20\x20\x20println(\"b-unit\");   { let g = Ha { h: Ea.Q }; println(\"m\") }\n\
             \x20\x20\x20\x20println(\"end\")\n\
             }\n",
            &[
                    "f-arr",
                    "d1",
                    "d2",
                    "m",
                    "f-vec",
                    "d3",
                    "m",
                    "f-venum",
                    "d4",
                    "m",
                    "f-bind",
                    "d5",
                    "d6",
                    "m",
                    "f-second",
                    "d7",
                    "d8",
                    "m",
                    "l-arr",
                    "d10",
                    "d11",
                    "m",
                    "l-vec",
                    "d12",
                    "m",
                    "b-tuple",
                    "m",
                    "b-unit",
                    "m",
                    "end",
            ],
            "enum_container_payload_in_struct_field_keeps_one_owner",
        );
}

/// B-2026-09-10-25 — the MEMORY half of `tests/codegen.rs`'s
/// `e2e_bare_variant_ctor_tuple_elem_runs_its_drop_body` under ASAN +
/// LSan.
///
/// The fix gives a BARE enum-variant constructor in a tuple element the
/// same ownership its qualified twin already had, so the question this
/// fixture answers is whether the newly-claimed owner double-frees or
/// strands anything. Each cell's bodies must fire exactly once and the run
/// must be clean.
///
/// The cells are the two shapes of the family that are CLEAN on the parent
/// tree as well, paired with their qualified twins, so a regression here is
/// unambiguously this change's. The row's own repro shape
/// (`(Option[R], i64)` — a tuple whose ONLY heap-bearing element is the
/// `Option`) is deliberately ABSENT: it already leaked 32 B + 1 B at `-O0`
/// before this change, in the bare and qualified spellings alike, and still
/// does. That leak is orthogonal and filed separately; including the shape
/// here would quarantine a pre-existing leak under this row's name rather
/// than measure anything. The parent-vs-fixed sweep found every cell's
/// valgrind numbers byte-identical, which is this change's actual memory
/// claim.
#[test]
fn asan_bare_variant_ctor_tuple_elem_keeps_one_owner() {
    assert_clean_asan_run(
            "struct R { id: i64, tag: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"  dR{self.id}/{self.tag}\") } }\n\
             enum W { A(R), N }\n\
             fn mk(i: i64) -> R { return R { id: i, tag: f\"t{i}\" }; }\n\
             fn uvArg(t: (W, i64)) -> i64 { println(f\"  in{t.1}\"); return 0; }\n\
             fn mixArg(t: (Option[R], R)) -> i64 { println(f\"  in{t.1.id}\"); return 0; }\n\
             \n\
             fn main() {\n\
             \x20\x20\x20\x20println(\"bareuv\");    { let _ = uvArg((A(mk(16)), 7)); println(\"  x\") }\n\
             \x20\x20\x20\x20println(\"qualuv\");    { let _ = uvArg((W.A(mk(17)), 7)); println(\"  x\") }\n\
             \x20\x20\x20\x20println(\"mixed\");     { let _ = mixArg((Some(mk(18)), mk(19))); println(\"  x\") }\n\
             \x20\x20\x20\x20println(\"qualmixed\"); { let _ = mixArg((Option.Some(mk(20)), mk(21))); println(\"  x\") }\n\
             \x20\x20\x20\x20println(\"unitvar\");   { let _ = uvArg((N, 7)); println(\"  x\") }\n\
             \x20\x20\x20\x20println(\"end\")\n\
             }\n\
",
            &[
                "bareuv",
                "  in7",
                "  dR16/t16",
                "  x",
                "qualuv",
                "  in7",
                "  dR17/t17",
                "  x",
                "mixed",
                "  in19",
                "  dR18/t18",
                "  dR19/t19",
                "  x",
                "qualmixed",
                "  in21",
                "  dR20/t20",
                "  dR21/t21",
                "  x",
                "unitvar",
                "  in7",
                "  x",
                "end",
            ],
            "bare_variant_ctor_tuple_elem_one_owner",
        );
}

#[test]
fn asan_self_referential_populated_payload_frees_its_box() {
    // B-2026-09-06-66 — the cell the fixture above deliberately EXCLUDES.
    // Every spelling it covers leaves `next` EMPTY; populate it and the
    // payload's box is freed by nobody. 67 bytes (64 direct: the boxed
    // `Node`; 3 indirect: its `tag`) at both opt levels, with both `Drop`
    // bodies running in the right order on every backend -- so no A/B gate
    // could see it and only a leak checker can.
    //
    // B-2026-09-06-64 made `aggregate_param_copy_supported_struct` decline
    // a self-referential struct (its entry copy has no finite emission),
    // and `struct_param_transfer_eligible` declines it too. Both refusals
    // are about DUPLICATION, but all three disjuncts of the struct drop's
    // `struct_callee_owned` gate then said no, so the `Option` field
    // classification never ran and the free went with them. Copy-declines /
    // drop-still-frees is the pair to keep.
    //
    // The DEPTH case is the second half: two levels lose 131 bytes pre-fix
    // (64 direct + 67 indirect), which is what shows the free has to recurse
    // rather than reach one level down.
    assert_clean_asan_run(
        r#"
struct Node { id: i64, next: Option[Node], tag: String }
impl Drop for Node { fn drop(mut ref self) { println(f"  dN{self.id}") } }

fn mkn(i: i64) -> Node { return Node { id: i, next: Option.None, tag: f"t{i}" }; }

fn main() {
    let one: Node = Node { id: 9, next: Option.Some(mkn(10)), tag: "n" };
    println(one.id);
    let deep: Node = Node { id: 11, next: Option.Some(Node { id: 12, next: Option.Some(mkn(13)), tag: "d" }), tag: "e" };
    println(deep.id);
}
"#,
        // `one` dies at its LAST USE, not at the end of the frame, so its
        // two bodies land before `11` prints -- on `--interp` and every
        // compiled backend alike. Measured, not assumed.
        &["9", "  dN9", "  dN10", "11", "  dN11", "  dN12", "  dN13"],
        "self_referential_populated_payload_frees_its_box",
    );
}

#[test]
fn asan_self_referential_populated_payload_without_drop_impl_frees_its_box() {
    // The `Drop`-free twin, and it is NOT redundant with the fixture above.
    // The row localised this defect by the fact that both `Drop` bodies run
    // correctly, which invites reading it as a `Drop`-channel problem; the
    // leak is in the MEMORY channel and is identical for a struct with no
    // `impl Drop` at all. Measured the same 67 bytes.
    //
    // It is also the shape with no user-visible output whatsoever, so a
    // leak checker is the ONLY thing that can observe it.
    assert_clean_asan_run(
        r#"
struct Plain { id: i64, next: Option[Plain], tag: String }

fn mkp(i: i64) -> Plain { return Plain { id: i, next: Option.None, tag: f"p{i}" }; }

fn main() {
    let c: Plain = Plain { id: 9, next: Option.Some(mkp(10)), tag: "n" };
    println(c.id);
}
"#,
        &["9"],
        "self_referential_populated_payload_no_drop_impl",
    );
}

/// B-2026-09-01-33 — the heap-carrying producer-call argument under
/// ASAN + LSan.
///
/// This case pins the REORDER, which is the part of the fix that could go
/// wrong. The arm previously pushed the enum's memory free AFTER the
/// wrapper, so the frame drained the free FIRST; that was harmless only
/// while no walker existed to read through it. Adding the payload walker
/// without moving the free ahead of it would have had `dR5`'s body read a
/// freed `String` and `Vec` -- a use-after-free ASAN catches, unlike the
/// missing body that motivated the fix.
///
/// The transcript is asserted alongside so the case also fails if the
/// walker stops running rather than merely staying memory-clean.
#[test]
fn asan_producer_call_arg_enum_payload_body_reads_live_heap() {
    assert_clean_asan_run(
            "struct R { id: i64, s: String, xs: Vec[i64] }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}:{self.s}:{self.xs.len()}\") } }\n\
             enum Sv { Hold { inner: R }, Nil }\n\
             impl Drop for Sv { fn drop(mut ref self) { println(\"dSv\") } }\n\
             enum Tv { A(R), Nil }\n\
             impl Drop for Tv { fn drop(mut ref self) { println(\"dTv\") } }\n\
             fn eatS(v: Sv) { println(\"e\") }\n\
             fn eatT(v: Tv) { println(\"e\") }\n\
             fn mkS(i: i64) -> Sv { return Sv.Hold { inner: R { id: i, s: \"pay\", xs: [1, 2, 3] } }; }\n\
             fn mkT(i: i64) -> Tv { return Tv.A(R { id: i, s: \"pay\", xs: [1, 2] }); }\n\
             fn main() { eatS(mkS(5)); eatT(mkT(6)) }\n",
            &["e", "dSv", "dR5:pay:3", "e", "dTv", "dR6:pay:2"],
            "b33-producer-call-arg-enum-payload",
        );
}

/// B-2026-08-31-8 — a heap-carrying enum STRUCT-VARIANT payload passed as a
/// fresh-temp argument is neither leaked nor double-freed.
///
/// The row was measured on a scalar payload and asked for a heap-carrying
/// one before the memory side was assumed balanced. This is that
/// measurement — and it is worth being exact about what it can see.
///
/// The fix was INTERPRETER-only, and ASAN never runs the interpreter, so
/// this case was already green before it. What it pins is the COMPILED
/// reference the interpreter now has to match, with a `String` and a `Vec`
/// in the payload: that the three shapes each free exactly once here is
/// what makes the twinned output fixtures' expectations the right ones to
/// have matched. `named` and `tuple` are the two that already had an owner,
/// and a later widening on THIS backend that reached them would surface
/// here as a double free rather than as a wrong line of output.
#[test]
fn asan_fresh_temp_struct_variant_arg_payload_is_balanced() {
    assert_clean_asan_run(
        r#"
struct R { id: i64, tag: String, buf: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}:{self.buf.len()}") } }
enum Sv { Hold { inner: R }, Nil }
impl Drop for Sv { fn drop(mut ref self) { println("dSv") } }
enum Tv { A(R), Nil }
impl Drop for Tv { fn drop(mut ref self) { println("dTv") } }

fn mkr(n: i64) -> R {
    let mut v: Vec[i64] = Vec.new();
    let mut i = 0;
    while i < 32 { v.push(i + n); i = i + 1; }
    return R { id: n, tag: "t", buf: v };
}

fn eat(v: Sv) { println("e"); }
fn eatt(v: Tv) { println("t"); }

fn main() {
    eat(Sv.Hold { inner: mkr(1) });
    let a = Sv.Hold { inner: mkr(2) };
    eat(a);
    eatt(Tv.A(mkr(3)));
}
"#,
        &[
            "e", "dSv", "dR1:32", "e", "dSv", "dR2:32", "t", "dTv", "dR3:32",
        ],
        "fresh_temp_struct_variant_arg_payload_is_balanced",
    );
}

/// B-2026-09-01-32 — the UNQUALIFIED enum struct-variant payload is neither
/// leaked nor double-freed.
///
/// The ASAN twin of B-2026-08-31-8's case one spelling over. Unlike that
/// one, this backend was NOT already correct: `enum_name_of_expr`'s
/// struct-literal arm was guarded `path.len() >= 2`, so the one-segment
/// spelling got no caller-side owner and ran neither body.
///
/// The heap payload is the point. The row was measured on a SCALAR payload,
/// where the whole defect is a missing line of output and the memory side
/// says nothing — so a fix could look complete against it while leaving a
/// heap-carrying value unbalanced. A `String` and a 32-element `Vec` make
/// the frees observable: this case is what says the newly-registered
/// owner frees exactly once rather than once too many.
///
/// `qualified` and `named` are the controls that already had an owner. A
/// widening of the new arm that reached either would surface here as a
/// double free rather than as a wrong line of output — which is the failure
/// mode the output fixtures cannot see.
#[test]
fn asan_fresh_temp_unqualified_struct_variant_arg_payload_is_balanced() {
    assert_clean_asan_run(
        r#"
struct R { id: i64, tag: String, buf: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}:{self.buf.len()}") } }
enum Sv { Hold { inner: R }, Nil }
impl Drop for Sv { fn drop(mut ref self) { println("dSv") } }

fn mkr(n: i64) -> R {
    let mut v: Vec[i64] = Vec.new();
    let mut i = 0;
    while i < 32 { v.push(i + n); i = i + 1; }
    return R { id: n, tag: "t", buf: v };
}

fn eat(v: Sv) { println("e"); }

fn main() {
    eat(Hold { inner: mkr(1) });
    eat(Sv.Hold { inner: mkr(2) });
    let a = Hold { inner: mkr(3) };
    eat(a);
}
"#,
        &[
            "e", "dSv", "dR1:32", "e", "dSv", "dR2:32", "e", "dSv", "dR3:32",
        ],
        "fresh_temp_unqualified_struct_variant_arg_payload_is_balanced",
    );
}

#[test]
/// B-2026-09-07-13 — the OWNERSHIP half of
/// `test_e2e_stored_enum_argument_is_owned_by_its_new_home_not_the_caller`.
///
/// That row's defect was a DOUBLE `Drop` BODY with balanced memory, so this
/// fixture is not asserting that the fix stopped a leak — it is asserting
/// that standing the caller down did not START one. The carve-out drops the
/// caller's `Drop` body and its payload walker on the escape path and keeps
/// ONLY the memory registration; the failure mode to guard is therefore the
/// mirror image of the row's own, and it is exactly the trade
/// B-2026-09-07-5 refused when it declined to widen the shared predicate:
/// stand the whole registration down and the callee's entry copy leaves the
/// caller's original orphaned. LSan is the only gate that can see that,
/// because the output is correct either way.
///
/// `Es`-ONLY, and both omissions are deliberate rather than incidental. The
/// `Ev` cells of the E2E fixture carry a `shared` field whose 16-byte
/// refcount block is stranded on the CORRECT ctor cell too
/// (B-2026-09-06-72's class, not this row's), and the return-route cell
/// strands its payload at `-O0` — an `-O0`-only residual that `-O2` hides by
/// eliding the dead malloc, on a path this row's arm never runs for. Both
/// are pre-existing and filed separately; carrying either here would pin a
/// defect this fix neither caused nor addresses.
///
/// All four legs are present — method (`a`), free (`c`), assoc (`d`),
/// monomorph (`e`) — plus the ctor spelling (`b`) that was already correct
/// and a plain `let` (`f`). Measured on the fix: 27 allocs / 27 frees at
/// `-O2`, 28 / 28 at `-O0`, 0 valgrind errors at both.
fn asan_stored_enum_argument_is_owned_by_its_new_home_not_the_caller() {
    assert_clean_asan_run_min_allocs(
        r#"
enum Es { A(String), B }
impl Drop for Es { fn drop(mut ref self) { println("dEs") } }
fn mkes(i: i64) -> Es { return Es.A(f"e{i}"); }
impl Es { fn is_a(ref self) -> i64 { match self { Es.A(s) => { return 1; } Es.B => { return 0; } } } }

struct BoxS { mut ys: Vec[Es] }
impl BoxS {
    fn puts(mut ref self, e: Es) { self.ys.push(e); }
    fn stash(b: mut ref BoxS, e: Es) { b.ys.push(e); }
}
fn pute(b: mut ref BoxS, e: Es) { b.ys.push(e); }
fn stashg[T](v: mut ref Vec[T], x: T) { v.push(x); }

fn c_meth()   { let mut d = BoxS { ys: Vec.new() }; d.puts(mkes(61)); println(f"a{d.ys.len()}"); }
fn c_ctor()   { let mut d = BoxS { ys: Vec.new() }; d.puts(Es.A("z")); println(f"b{d.ys.len()}"); }
fn c_free()   { let mut d = BoxS { ys: Vec.new() }; pute(mut d, mkes(76)); println(f"c{d.ys.len()}"); }
fn c_assoc()  { let mut d = BoxS { ys: Vec.new() }; BoxS.stash(mut d, mkes(78)); println(f"d{d.ys.len()}"); }
fn c_generic(){ let mut v: Vec[Es] = Vec.new(); stashg(mut v, mkes(75)); println(f"e{v.len()}"); }
fn c_plain()  { let e = mkes(79); println(f"f{e.is_a()}"); }

fn main() {
    c_meth(); c_ctor(); c_free(); c_assoc(); c_generic(); c_plain();
    println("end");
}
"#,
        &[
            "a1", "dEs", "b1", "dEs", "c1", "dEs", "d1", "dEs", "e1", "dEs", "f1", "dEs", "end",
        ],
        "b0907-13-stored-enum-arg",
        // 19, measured on both hosts; the 20 was an estimate one above the
        // real count (B-2026-09-07-26).
        19,
    );
}

/// B-2026-08-29-37 — the memory half of "withhold the body, keep the copy".
///
/// The fix stops `materialize_freshtemp_enum_scrutinee` from registering the
/// enum's user `Drop` body on a defensive copy of a borrowed place. It
/// deliberately leaves the MEMORY registration alone, because the copy really
/// does own duplicated buffers — so the failure mode it could have introduced
/// is a leak of exactly those buffers, which LSan sees and no output
/// comparison does.
///
/// `take` is called three times over the same borrowed `S`, so the ref-chain
/// clone is emitted and consumed three times while the caller keeps owning
/// the original. A `Vec[String]` payload puts a real element buffer behind
/// each clone: a lost memory registration leaks three of them, and a
/// double-registration frees the caller's buffer out from under it.
#[test]
fn asan_ref_chain_enum_scrutinee_clone_frees_once() {
    let Some((out, status)) = run_under_asan(
        r#"struct S { e: E }
enum E { A(Vec[String]), B }
impl Drop for E { fn drop(mut ref self) { println("dE") } }

fn mkv(n: i64) -> Vec[String] {
    let mut v = Vec.new();
    v.push("abcdefghijklmnopqrstuvwxyz0123456789");
    v.push(f"tag{n}");
    return v
}

#[allow(partial_move_of_drop_enum)]
fn take(s: ref S) -> String {
    match s.e { E.A(v) => { let m = v; return m[1] } E.B => { return "none" } }
}

fn main() {
    let s = S { e: E.A(mkv(7)) };
    println(take(s));
    println(take(s));
    println(take(s));
    println("end");
}
"#,
        "asan_ref_chain_enum_scrutinee_clone_frees_once",
    ) else {
        return;
    };
    assert!(status.success(), "ASAN/LSan reported a problem:\n{out}");
    // One `dE` — the caller's, at its own scope exit. Three would be the
    // pre-fix count (one per clone) and would mean the body registration
    // came back.
    assert_eq!(
        out.matches("dE").count(),
        1,
        "unexpected Drop body count:\n{out}"
    );
    assert!(
        out.contains("tag7"),
        "the clone did not carry a live payload:\n{out}"
    );
}

/// B-2026-08-28-31 — an enum that declares its OWN `impl Drop`, discarded
/// by a wildcard destructure leaf, runs its own body once and stays memory
/// balanced.
///
/// The body half was a run-vs-build divergence: the interpreter ran
/// `drop E` and both compiled backends ran nothing. This fixture is about
/// the other half, which is what the row wanted answered before the shape
/// could be wired — whether the bodies-only walker leaves the payload's
/// heap owned exactly once.
///
/// It does, and the two sources answer it differently, which is the same
/// per-site `free_memory` split B-2026-08-28-12 established by measurement:
/// a FRESH tuple temp carries no aggregate drop of its own, so the discard
/// site frees; a PLACE source is freed by the source aggregate's own drop,
/// so it must not. Reaching for the combined `karac_drop_<E>` wrapper —
/// body AND memory in one — is what the row expected this to need, and
/// would have double-freed the place row.
///
/// B-2026-08-28-40 later added the live payload's body to both discard
/// rows, bringing them level with `bound-control`. Running `drop R41` here
/// makes the payload's `String` observably live at a point the optimizer
/// could otherwise have deleted its allocation, so the balance question is
/// re-asked rather than inherited — the same way B-2026-08-28-12's
/// bodies-only first cut turned a dead allocation into a real 3-byte leak.
/// B-2026-08-28-46 / -47 — the memory half of running an own-`Drop` enum
/// member's body when the owner dies without ever being destructured.
///
/// The body fix is bodies-only on both backends: codegen's tuple-element
/// leg calls `<E>.drop` and `__karac_dropelems_enum_<E>`, neither of which
/// frees, and the interpreter's arms are the same shape. The owner's own
/// memory drop is untouched. This fixture is what holds that claim to
/// account, because it is exactly the claim B-2026-08-28-12's first cut got
/// wrong in the other direction — a bodies-without-free widening there
/// turned a dead allocation into a real leak once the body made the payload
/// observably live.
///
/// So every row carries a heap `String` that the newly-running body READS.
/// Under LSan a missed free shows as a leak and a doubled one as a double
/// free; passing both ways is the balance claim.
#[test]
fn asan_own_drop_enum_member_body_is_memory_balanced() {
    const H: &str = "enum E { A(R), B }\n\
             impl Drop for E { fn drop(mut ref self) { println(\"drop E\") } }\n\
             struct R { id: i64, name: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop R{self.name}\") } }\n";
    // TUPLE element, payload variant — the payload's buffer is freed by the
    // tuple's own drop; the body must read it and not free it.
    assert_clean_asan_run(
        &format!(
            "{H}fn main() {{ let p = (E.A(R {{ id: 7, name: f\"n{{7}}\" }}), 1);\n\
             \x20            println(f\"{{p.1}}\"); }}\n"
        ),
        &["1", "drop E", "drop Rn7"],
        "tuple-elem-payload",
    );
    // STRUCT field, payload variant — same value reached through the field
    // walker instead, whose enum leg is the `-46` half.
    assert_clean_asan_run(
        &format!(
            "{H}struct W {{ e: E, n: i64 }}\n\
             fn main() {{ let w = W {{ e: E.A(R {{ id: 7, name: f\"n{{7}}\" }}), n: 1 }};\n\
             \x20            println(f\"{{w.n}}\"); }}\n"
        ),
        &["1", "drop E", "drop Rn7"],
        "struct-field-payload",
    );
    // COMPOSED — tuple inside a struct field, the third walker.
    assert_clean_asan_run(
        &format!(
            "{H}struct W {{ p: (E, i64) }}\n\
             fn main() {{ let w = W {{ p: (E.A(R {{ id: 7, name: f\"n{{7}}\" }}), 1) }};\n\
             \x20            println(\"hi\"); }}\n"
        ),
        &["drop E", "drop Rn7", "hi"],
        "tuple-inside-struct-field",
    );
    // MOVED ON — the destination owns it. A per-owner rather than
    // per-value widening double-frees here.
    assert_clean_asan_run(
        &format!(
            "{H}fn main() {{ let p = (E.A(R {{ id: 7, name: f\"n{{7}}\" }}), 1);\n\
             \x20            let q = p; println(f\"{{q.1}}\"); }}\n"
        ),
        &["1", "drop E", "drop Rn7"],
        "moved-on",
    );
    // B-2026-08-28-55 — the `Vec` element peer. Its walker runs the body
    // per element INSIDE the loop while the elements' memory is freed by
    // the Vec's own drop, so a bodies-leg that also freed would double-free
    // once per element -- which is why this row exists rather than resting
    // on the tuple ones above.
    assert_clean_asan_run(
        &format!(
            "{H}fn main() {{ let mut v = Vec.new();\n\
             \x20            v.push(E.A(R {{ id: 7, name: f\"n{{7}}\" }}));\n\
             \x20            println(f\"{{v.len()}}\"); }}\n"
        ),
        &["1", "drop E", "drop Rn7"],
        "vec-elem-payload",
    );
    // B-2026-08-28-54 — a payload-only enum (no own `Drop`). The body that
    // now runs is the PAYLOAD's, reached without any own-body call, so this
    // exercises a different call shape than every row above: walker only,
    // no `<E>.drop`. The `String` it reads is freed by the owner's drop.
    assert_clean_asan_run(
        "enum E2 { A(R2), B }\n\
             struct R2 { id: i64, name: String }\n\
             impl Drop for R2 { fn drop(mut ref self) { println(f\"drop R{self.name}\") } }\n\
             fn main() { let p = (E2.A(R2 { id: 5, name: f\"n{5}\" }), 1);\n\
             \x20            println(f\"{p.1}\"); }\n",
        &["1", "drop Rn5"],
        "payload-only-enum",
    );
    // DESTRUCTURED — the pre-existing path, re-asserted alongside so a
    // regression that double-runs the body shows up as a double free here
    // rather than only as a count in the interpreter suite.
    assert_clean_asan_run(
        &format!(
            "{H}fn main() {{ let p = (E.A(R {{ id: 7, name: f\"n{{7}}\" }}), 1);\n\
             \x20            let (_, n) = p; println(f\"{{n}}\"); }}\n"
        ),
        &["drop E", "drop Rn7", "1"],
        "destructured-control",
    );
}

/// B-2026-08-28-59 — adding the enum's own `Drop` body to a named tuple
/// leaf leaves its payload heap owned exactly once.
///
/// The body half was a run-vs-build divergence: the compiled backends ran
/// the payload body and lost `<E>.drop`. This is the half that decides
/// whether adding it is safe, and the risk is specific — the enum arm
/// already registers TWO actions on this slot (`track_enum_var` for the
/// memory and the payload walk), so a third has to be complementary rather
/// than a second claim on the same heap.
///
/// It is: the `karac_drop_<E>` wrapper's field-cleanup half is a no-op for
/// an enum name, so it contributes the body alone — the same dual the
/// argument-position registrar performs. `consumed-leaf` is where a wrong
/// answer would surface as a use-after-free rather than as a count.
#[test]
fn asan_named_enum_tuple_leaf_owns_its_payload_once() {
    const H: &str = "struct R { id: i64, name: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id} {self.name}\") } }\n\
             enum E { A(R), B }\n\
             impl Drop for E { fn drop(mut ref self) { println(\"dE\") } }\n";
    assert_clean_asan_run(
        &format!(
            "{H}fn main() {{ let (gv, gn) = (E.A(R {{ id: 5, name: f\"n{{5}}\" }}), 6);\n\
             \x20            println(f\"{{gn}}\") }}\n"
        ),
        &["dE", "dR5 n5", "6"],
        "fresh-source",
    );
    assert_clean_asan_run(
        &format!(
            "{H}fn main() {{ let g = (E.A(R {{ id: 5, name: f\"n{{5}}\" }}), 6);\n\
             \x20            let (gv, gn) = g; println(f\"{{gn}}\") }}\n"
        ),
        &["dE", "dR5 n5", "6"],
        "place-source",
    );
    // The leaf handed to a callee — the payload buffer outlives the
    // destructure, so a wrong owner shows up as a use-after-free.
    assert_clean_asan_run(
            &format!(
                "{H}fn take(e: E) -> i64 {{ 7 }}\n\
             \x20            fn main() {{ let (gv, gn) = (E.A(R {{ id: 5, name: f\"n{{5}}\" }}), 6);\n\
             \x20            println(f\"{{take(gv) + gn}}\") }}\n"
            ),
            &["13", "dE", "dR5 n5"],
            "consumed-leaf",
        );
    // CONTROL — a by-value tuple PARAM source, whose elements' bodies are
    // owned caller-side. One body, one free.
    assert_clean_asan_run(
        &format!(
            "{H}fn take(p: (E, i64)) -> i64 {{ let (gv, gn) = p; gn }}\n\
             \x20            fn main() {{ let arg = (E.A(R {{ id: 5, name: f\"n{{5}}\" }}), 6);\n\
             \x20            println(f\"{{take(arg)}}\") }}\n"
        ),
        &["6", "dE", "dR5 n5"],
        "param-source-control",
    );
    // CONTROL — the same enum BOUND directly, no destructure.
    assert_clean_asan_run(
        &format!(
            "{H}fn main() {{ let gv = E.A(R {{ id: 5, name: f\"n{{5}}\" }}); println(\"mid\") }}\n"
        ),
        &["dE", "dR5 n5", "mid"],
        "bound-control",
    );
}

/// B-2026-08-28-43 — a BARE unit variant of an own-`impl Drop` enum is
/// owned exactly once at each fresh-temp position.
///
/// The body half was a soundness gap: `B` ran no `Drop` body where `E.B`
/// ran one, at every position below. This fixture is the half that decides
/// whether closing it is safe, and the risk it measures is specific. The
/// registrar these positions feed declines every `Identifier` precisely
/// because a LET-BOUND enum's drop belongs to its binding; admitting the
/// bare variant means teaching it a distinction it did not have, and
/// getting that wrong in the permissive direction is a double free rather
/// than an extra print.
///
/// The enum carries a `String` in its OTHER variant, which is the point of
/// the shape: the value under test is the payloadless `B`, so a spurious
/// second owner would run the drop switch twice over a live heap word, and
/// the constructed variant is the one that decides whether that word is
/// real. `bound-then-passed` is the double-free direction stated directly —
/// a local, owned by its binding, passed on.
#[test]
fn asan_bare_unit_variant_of_own_drop_enum_is_owned_once() {
    const H: &str = "enum E { A(String), B }\n\
             impl Drop for E { fn drop(mut ref self) { println(\"drop E\") } }\n";
    assert_clean_asan_run(
        &format!("{H}fn main() {{ let _ = B; println(\"end\") }}\n"),
        &["drop E", "end"],
        "let-discard-bare",
    );
    assert_clean_asan_run(
        &format!("{H}fn main() {{ B; println(\"end\") }}\n"),
        &["drop E", "end"],
        "statement-bare",
    );
    assert_clean_asan_run(
        &format!(
            "{H}fn take(e: E) -> i64 {{ 7 }}\n\
             \x20            fn main() {{ let x = take(B); println(f\"{{x}}\") }}\n"
        ),
        &["drop E", "7"],
        "argument-bare",
    );
    assert_clean_asan_run(
        &format!("{H}fn main() {{ let p = (B, 1); let (_, n) = p; println(f\"{{n}}\") }}\n"),
        &["drop E", "1"],
        "tuple-leaf-bare",
    );
    // The DOUBLE-FREE direction: a local, whose binding already owns the
    // drop, handed to a callee. One body, one free.
    assert_clean_asan_run(
        &format!(
                "{H}fn take(e: E) -> i64 {{ 7 }}\n\
             \x20            fn main() {{ let e = E.A(f\"n{{41}}\"); println(f\"{{take(e)}}\") }}\n"
            ),
        &["7", "drop E"],
        "bound-then-passed",
    );
    // The LIVE-payload variant at the same positions, so the frees the
    // admission newly reaches are exercised against a real heap word rather
    // than an empty tag.
    assert_clean_asan_run(
        &format!(
                "{H}fn take(e: E) -> i64 {{ 7 }}\n\
             \x20            fn main() {{ let x = take(E.A(f\"n{{41}}\")); println(f\"{{x}}\") }}\n"
            ),
        &["drop E", "7"],
        "payload-argument-control",
    );
}

/// B-2026-09-10-5 — the moved-from-slot disarm zeroed `Option`'s FOUR
/// words into a slot that was only as wide as the binding's own enum, so a
/// named-local generic enum passed BY VALUE wrote 16 bytes past its alloca.
///
/// ASAN is the right gate for it even though the row's headline symptom is
/// a SIGSEGV: the fault only reproduces where the overrun happens to land
/// on the saved return address, which is a frame-layout accident — it
/// crashed at `-O0` and ran clean at `-O2`, under the JIT and on
/// `--interp`. The stack redzone makes the WRITE itself the finding, at
/// whatever opt level the suite runs, so this pins the defect rather than
/// the crash it happened to cause.
///
/// Plain POD on purpose — no `Drop` impl and no heap anywhere in `W9`. The
/// bug is in the move disarm, not in the drop machinery, and a fixture
/// carrying a `Drop`-bearing payload would have implied otherwise.
#[test]
fn asan_generic_enum_named_local_by_value_arg_does_not_overrun_its_slot() {
    const SRC: &str = "struct W9 { a: i64, b: i64, c: i64, d: i64, e: i64, f: i64, g: i64, h: i64, i: i64 }\n\
             enum G[T] { X(T), Y }\n\
             fn hg(g: G[W9]) { println(\"ig\") }\n\
             fn main() { let a = G.X(W9 { a: 1, b: 2, c: 3, d: 4, e: 5, f: 6, g: 7, h: 8, i: 9 }); hg(a); println(\"end\") }\n";
    assert_clean_asan_run(
        SRC,
        &["ig", "end"],
        "generic-enum-named-local-by-value-arg-slot-overrun",
    );
}

#[test]
fn asan_generic_enum_heap_payload_bind_return_no_leak_or_double_free() {
    // B-2026-07-13-3: a GENERIC enum's bare-`T` variant payload (`enum
    // Opt[T] { Yes(T) }`) sizes its payload AREA for the erased `T` (1 word)
    // at declare time, so a heap monomorph (T=String/Vec, 3 words) is stored
    // BOXED. `match o { Opt.Yes(v) => v, Opt.No => d }` at T=String must
    // debox `v` (loading the full `{ptr,i64,i64}` from the heap box, freeing
    // the box) and return it deep-copied; the No arm returns the owned param
    // `d`. 200 iterations exercise both arms for String and Vec[i64]: a
    // missed debox / box-free leaks (LSan), a double-counted free aborts
    // (ASAN). Before the fix this failed codegen outright (`ret i64 0` vs
    // `{ptr,i64,i64}` module-verification failure), so it never linked.
    assert_clean_asan_run(
        r#"
enum Opt[T] { Yes(T), No }
fn get[T](o: Opt[T], d: T) -> T { match o { Opt.Yes(v) => v, Opt.No => d } }
fn main() {
    let mut i: i64 = 0i64;
    let mut total: i64 = 0i64;
    while i < 200i64 {
        let a: String = get(Opt.Yes(f"yes{i}"), f"fb");
        total = total + a.len();
        let b: String = get(Opt.No, f"no{i}");
        total = total + b.len();
        let mut vv: Vec[i64] = Vec.new();
        vv.push(i);
        vv.push(i);
        vv.push(i);
        let empty: Vec[i64] = Vec.new();
        let w: Vec[i64] = get(Opt.Yes(vv), empty);
        total = total + w.len();
        i = i + 1;
    }
    println(total.to_string());
}
"#,
        // String Yes ("yes{i}", len 3+digits): 10*4 + 90*5 + 100*6 = 1090.
        // String No ("no{i}", len 2+digits):   10*3 + 90*4 + 100*5 = 890.
        // Vec Yes (len 3 each): 200*3 = 600. Grand total = 1090+890+600 = 2580.
        &["2580"],
        "generic_enum_heap_payload_bind_return_no_leak_or_double_free",
    );
}

/// B-2026-08-05-7 (generic-enum box leg): a generic USER enum whose
/// monomorph binds `T` wider than the declaration-time payload area leaked
/// the heap box, once per construction.
///
/// `enum Opt[T] { Yes(T), No }` lays `T` out ERASED at one word, so an
/// `Opt[String]` monomorph packs a 3-word payload into a 1-word area and
/// `coerce_to_payload_words` heap-boxes it. Nothing freed that envelope:
/// `boxed_enum_payload_variants`, which drives the box-drop registration,
/// matches on the enum NAME and knew only the seeded `Option`/`Result`, so
/// every user enum fell through to "no box drop at all".
///
/// It is specific to the generic case, and the sibling fixtures prove the
/// controls: a CONCRETE user enum sizes its area to its widest variant so
/// nothing boxes, and a scalar monomorph fits the erased area. Both were
/// always clean.
///
/// Covers all three sites a box can be owned from, which is what the fix
/// needed: the monomorph's own by-value PARAM (the shape that actually
/// bites — a temp handed straight into a generic callee has no binding
/// anywhere to hang a drop on), and a let-bound value in the caller.
///
/// NOTE: pins at -O0 only. At -O2 the optimizer deletes the box outright,
/// which is exactly why this leaked unnoticed; the -O0 sweep is the gate.
#[test]
fn asan_generic_enum_wide_monomorph_box_freed() {
    assert_clean_asan_run_min_allocs(
        r#"
enum Opt[T] { Yes(T), No }
fn get[T](o: Opt[T], d: T) -> T { match o { Opt.Yes(v) => v, Opt.No => d } }
fn main() {
    let base: i64 = env.args().len();
    let mut i: i64 = 0i64;
    let mut total: i64 = 0i64;
    while i < base + 39i64 {
        let a: String = get(Opt.Yes(f"yes{i}"), f"fb");
        if a.starts_with("yes") { total = total + 1i64; }
        total = total + a.len();
        let o: Opt[String] = Opt.Yes(f"bound{i}");
        let b: String = match o { Opt.Yes(v) => v, Opt.No => f"fb" };
        if b.starts_with("bound") { total = total + 1i64; }
        total = total + b.len();
        i = i + 1;
    }
    println(total);
}
"#,
        &["540"],
        "generic_enum_wide_monomorph_box_freed",
        40,
    );
}

#[test]
fn asan_generic_enum_struct_heap_payload_bind_no_leak_or_double_free() {
    // B-2026-07-13-3, user-struct payload sibling: a generic enum's bare-`T`
    // payload resolved to a USER STRUCT with a heap field (`enum Opt[T] {
    // Yes(T) }` at `T = struct Box { s: String }`) is also stored BOXED (the
    // 1-word erased area can't hold the 1-field `{ {ptr,i64,i64} }` struct).
    // The debox must rebuild at the struct's exact aggregate (not the 3-word
    // vec heuristic) and the moved-out struct's inner String must be freed
    // exactly once. 500 iters, both arms. Before the extension this rebuilt
    // as `{ptr,i64,i64}` and failed module verification (`ret i64 0`).
    assert_clean_asan_run(
        r#"
struct Box { s: String }
enum Opt[T] { Yes(T), No }
fn get[T](o: Opt[T], d: T) -> T { match o { Opt.Yes(v) => v, Opt.No => d } }
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 500 {
        let b: Box = Box { s: f"inside-{i}" };
        let db: Box = Box { s: f"default" };
        let r: Box = get(Opt.Yes(b), db);
        total = total + r.s.len();
        let db2: Box = Box { s: f"fallback-{i}" };
        let r2: Box = get(Opt.No, db2);
        total = total + r2.s.len();
        i = i + 1;
    }
    println(total.to_string());
}
"#,
        &["10780"],
        "generic_enum_struct_heap_payload_bind_no_leak_or_double_free",
    );
}

#[test]
fn asan_borrow_payload_field_direct_consume_no_double_free() {
    // B-2026-08-25-15: a heap FIELD read out of a BORROW-accessor match
    // payload (`match self.values.get(i) { Some(pv) => … }` on a `ref self`
    // receiver) and consumed DIRECTLY, with no intervening `let`. `pv` is a
    // shallow bit-copy of the container's element, so the container's
    // per-element drain owns and frees its `String` fields — but the copier
    // that gives such a field an independent buffer only fired at a `let`,
    // so `return pv.value` and `out.push(pv.value)` handed the sink a live
    // alias: `free(): double free detected in tcache 2`, SIGABRT 134 under
    // JIT/AOT while `--interp` printed the right answer. The arg/return-site
    // copier now admits the same root class its two siblings already carry.
    //
    // This is also the LEAK gate for that widening: the mirror-image failure
    // of an over-eager copy is a leak, invisible to a macOS asan run and
    // caught only by LSan on Linux. 300 iterations so a per-call leak
    // accumulates well past noise, and every payload is an f-string — a
    // string LITERAL is static with `cap == 0`, so every free over it is a
    // no-op and NEITHER failure mode would be observable (two earlier
    // reductions of this bug were false negatives for exactly that reason).
    // The trailing second `h.direct()` proves the container survived the
    // escapes rather than merely not crashing on the way out.
    assert_clean_asan_run(
        r#"
struct Pv { name: String, value: String }
struct Holder { values: Vec[Pv] }
impl Holder {
    fn direct(ref self) -> String {
        match self.values.get(0) {
            Some(pv) => { return pv.value; }
            None => { return "none"; }
        }
    }
    fn collect(ref self) -> Vec[String] {
        let mut out: Vec[String] = Vec.new();
        let mut i = 0;
        while i < self.values.len() {
            match self.values.get(i) {
                Some(pv) => { out.push(pv.value); }
                None => {}
            }
            i = i + 1;
        }
        return out;
    }
}
fn main() {
    let mut total: i64 = 0;
    let mut i: i64 = 0;
    while i < 300 {
        let mut v: Vec[Pv] = Vec.new();
        v.push(Pv { name: f"k-{i}", value: f"a-{i}" });
        v.push(Pv { name: f"m-{i}", value: f"bb-{i}" });
        let h = Holder { values: v };
        total = total + h.direct().len();
        let got = h.collect();
        for g in got { total = total + g.len(); }
        total = total + h.direct().len();
        i = i + 1;
    }
    println(total.to_string());
}
"#,
        // per iter, d = digits of i: two `direct()` at 2+d each, `collect()`
        // at (2+d)+(3+d) → 9+4d. 10 iters at d=1, 90 at d=2, 200 at d=3.
        &["5860"],
        "borrow_payload_field_direct_consume_no_double_free",
    );
}

/// B-2026-09-04-12 — A BOXED TUPLE PAYLOAD REACHED THROUGH A BY-VALUE PARAM
/// LOST ITS INTERIOR IN EVERY ARM SHAPE.
///
/// `fn plainT(x: Option[(String, String)])` boxes its payload (6 words,
/// past the 3-word `Option` area). The callee's owned-param registration
/// (`functions.rs`, B-2026-08-06-9 leg A) freed the BOX and nothing walked
/// the tuple's own heap elements, because that arm derives its inner drop
/// from a struct NAME and a tuple has none. Measured at `-O0` under
/// valgrind, three calls each: 54 B in 6 blocks for `(String, String)`,
/// 81 B in 9 for a 3-`String` tuple, 27 B in 3 for
/// `(String, i64, i64, i64)`, and 288 B direct + 27 indirect for a
/// `Vec[String]` element.
///
/// The identical shape bound as a NAMED LOCAL was already clean, which is
/// what located the fix: the let site (B-2026-08-05-3) arms the tuple's own
/// drop on the same `BoxedEnumDrop`, and the param site is now its twin.
///
/// All three arm shapes are pinned because the fix has to leave the
/// ownership split intact, and two of them are the double-free directions:
///   - `Some(t)` whole binding, read-only — the interior drop must SURVIVE
///     (this is the reported leak);
///   - `Some((a, b))` per-element destructure that CONSUMES the leaves into
///     a `Vec` — `retract_boxed_tuple_inner_drop_for_arm` must downgrade the
///     box back to box-only, or the box and the `Vec` free the same buffers;
///   - `Some(_)` wildcard — binds nothing, so the box is the only owner the
///     interior can have.
///
/// The `Array[String, 2]` cell is the CONTROL and must stay clean: its
/// interior is owned by the caller's never-disarmed array drop, so
/// `option_payload_struct_or_enum_drop_ok` declining an array is what keeps
/// the fix from making a second owner of it.
#[test]
fn asan_boxed_tuple_param_payload_frees_its_interior() {
    assert_clean_asan_run(
        r#"
fn whole(x: Option[(String, String)]) {
    match x { Some(t) => { println(f"w:{t.0}"); } None => { println("wn"); } }
}

fn wild(x: Option[(String, String)]) {
    match x { Some(_) => { println("i"); } None => { println("in"); } }
}

fn consume(x: Option[(String, String)], out: mut ref Vec[String]) {
    match x { Some((a, b)) => { out.push(a); out.push(b); } None => {} }
}

fn wide(x: Option[(String, i64, i64, i64)]) {
    match x { Some(t) => { println(f"x:{t.0}"); } None => { println("xn"); } }
}

fn arr(x: Option[Array[String, 2]]) {
    match x { Some(t) => { println(f"a:{t[0]}"); } None => { println("an"); } }
}

fn main() {
    let mut out: Vec[String] = Vec.new();
    let mut n = 0;
    while n < 3 {
        whole(Some((f"whole-{n}-padpad", f"snd-{n}-padpad")));
        wild(Some((f"wild-{n}-padpad", f"snd-{n}-padpad")));
        consume(Some((f"con-{n}-padpad", f"snd-{n}-padpad")), mut out);
        wide(Some((f"wide-{n}-padpad", 1, 2, 3)));
        let a: Array[String, 2] = [f"arr-{n}-padpad", f"snd-{n}-padpad"];
        arr(Some(a));
        n = n + 1;
    }
    println(f"c:{out.len()}");
}
"#,
        &[
            "w:whole-0-padpad",
            "i",
            "x:wide-0-padpad",
            "a:arr-0-padpad",
            "w:whole-1-padpad",
            "i",
            "x:wide-1-padpad",
            "a:arr-1-padpad",
            "w:whole-2-padpad",
            "i",
            "x:wide-2-padpad",
            "a:arr-2-padpad",
            "c:6",
        ],
        "asan_boxed_tuple_param_payload_frees_its_interior",
    );
}

/// B-2026-08-28-75 (FIXED): reassigning an `Option`/`Result` binding whose
/// boxed payload arrived by a whole-value MOVE from another binding
/// (`let mut vv = value;`) orphaned the payload BOX on every store.
///
/// The eager free that reclaims a displaced box (B-2026-08-07-4) declined
/// for exactly this population, gated on a `boxed_moved_in_vars` set
/// recorded at the let site whenever the RHS was an identifier. That gate
/// was PROVENANCE-based rather than ownership-based, and the population it
/// excluded is a large and ordinary one: every `let mut vv = value;`
/// followed by any store to `vv`.
///
/// THE ROW WAS FILED MUCH NARROWER THAN THE DEFECT, and the rows below are
/// the narrowing that corrected it. It reported a consuming `while let`
/// over an `Option[enum]`, which made three things look load-bearing that
/// are not: the `while let` (the `match` and no-construct-at-all spellings
/// leak identically), consuming the payload (`Some(_v)` with the binding
/// unused leaks the same 76 bytes), and the ENUM payload (a boxed STRUCT
/// payload leaks its 100). What actually matters is the pair
/// MOVED-IN BOX + ANY LATER STORE — the row's own "minimal shape", which
/// dropped the alias, does not reproduce at all.
///
/// THE HAZARD ROWS ARE THE POINT OF THE FIXTURE. The excluded gate had a
/// real reason: with the payload HANDED OUT of an arm, freeing the box at
/// the store site also runs its interior walk, which frees heap the
/// destination now owns — a heap-use-after-free rather than a leak. That
/// turned out to be a missing retraction rather than a reason to skip the
/// free: `compile_if_let` never ran the arm-tail box-view neutralizer its
/// `match` twin has had since B-2026-08-04-2. With both halves in, the walk
/// is retracted wherever the payload escapes and the free reclaims the
/// envelope only. `iflet-hands-out` FAILS AS A UAF with only the eager-free
/// half and leaks with neither, so it pins the pair rather than one side.
///
/// `discarded-iflet` is the boundary that keeps the new retraction honest:
/// an if-let whose value nothing takes has no destination to inherit the
/// payload, so neutralizing there strands the box instead of transferring
/// it. It is what the `branch_value_is_owned` guard is for, and it is RED
/// without it — 68 bytes in 1 object, measured by replacing the guard with
/// `true`, exactly the leak the `match` site's own comment predicts.
///
/// THE READS ARE `contains`, NOT `len`, AND THAT IS WHY THIS GATES AT
/// `-O2`. Every row but `no-construct-at-all` reads its payload through a
/// borrow-only arm before the store, which is what keeps the allocation
/// alive under the optimizer — but the first draft read it with `s.len()`
/// and was GREEN at `-O2` against the unfixed compiler. `len` reads the
/// `{ptr,len,cap}` header, not the bytes; the length of an f-string is
/// known at the construction site, so LLVM propagates it and deletes the
/// `malloc` outright, leaving the fixture asserting nothing on the default
/// leg. `contains` runs a real search over the buffer, so the bytes are
/// live. With that one change the fixture went from `-O0`-only to RED at
/// BOTH levels on the unfixed tree (`-O0`: 77 B / 2 objects on
/// `whilelet-reassign`; `-O2`: 293 B / 6 on `loop-reassign`). The payload
/// is also runtime-derived via `env.args().len()` for the same reason —
/// a literal one folds — and is ≥36 bytes for LSan reachability under the
/// Linux gate.
///
/// `no-construct-at-all` stays `-O0`-only by construction: reading the
/// payload would require the very construct the row exists to exclude, and
/// the `asan-o0` CI leg is its gate.
/// B-2026-09-12-25 — a nested destructure that binds a struct leaf out of a
/// BOXED enum payload gave the leaf two owners: the arm's binding, and the
/// boxed payload's own drop still walking to the field the pattern moved
/// out. Both went through `__karac_drop_struct_R2` and the second aborted.
///
/// FOUR CELLS, and the second is the one the row got wrong. It was filed
/// with the escaping form as the defect and the READ-ONLY form as a clean
/// control, concluding that the trigger was the leaf escaping into a
/// `mut ref` accumulator. Both double-free identically: glibc's tcache
/// check missed the read-only one, while ASAN and macOS libmalloc catch it.
/// The trigger is the nested move-out, whatever the arm then does.
///
/// The `Result` cell and the INLINE cell bound the fix. Inline is the
/// control that matters most: its payload has no box, so the disarm must
/// NOT fire there — reading word 1 as a pointer would be reading the
/// payload's own bytes as an address.
#[test]
fn asan_nested_boxed_payload_leaf_has_one_owner() {
    const H: &str = "struct R2 { s: String }\n\
             enum K { A(R2), B }\n\
             impl Drop for K { fn drop(mut ref self) { println(\"dK\") } }\n\
             struct Small { n: i64 }\n\
             enum S { A(Small), B }\n";
    for (label, body, want) in [
        // The row's own shape: the leaf escapes into a `mut ref` accumulator.
        (
            "escaping-into-mut-ref",
            "#[allow(partial_move_of_drop_enum)]\n\
                 fn show(x: Option[K], acc: mut ref Vec[R2]) {\n\
                 \x20  match x {\n\
                 \x20    Option.Some(K.A(r)) => { acc.push(r) }\n\
                 \x20    Option.Some(K.B) => {}\n\
                 \x20    Option.None => {}\n\
                 \x20  } }\n\
                 #[allow(partial_move_of_drop_enum)]\n\
                 fn main() {\n\
                 \x20  let mut acc: Vec[R2] = [];\n\
                 \x20  show(Option.Some(K.A(R2 { s: f\"z\" })), mut acc);\n\
                 \x20  println(f\"len:{acc.len()}\");\n\
                 \x20  println(\"end\") }\n",
            vec!["len:1", "end"],
        ),
        // The row's "clean control", which is not clean: the leaf is only
        // READ and still had two owners.
        (
            "read-only-leaf",
            "fn show(x: Option[K]) {\n\
                 \x20  match x {\n\
                 \x20    Option.Some(K.A(r)) => { println(f\"a:{r.s}\") }\n\
                 \x20    Option.Some(K.B) => {}\n\
                 \x20    Option.None => {}\n\
                 \x20  } }\n\
                 fn main() { show(Option.Some(K.A(R2 { s: f\"z\" }))); println(\"end\") }\n",
            vec!["a:z", "dK", "end"],
        ),
        // The `Result` spelling of the same nesting.
        (
            "result-ok-side",
            "fn show(x: Result[K, i64]) {\n\
                 \x20  match x {\n\
                 \x20    Result.Ok(K.A(r)) => { println(f\"a:{r.s}\") }\n\
                 \x20    Result.Ok(K.B) => {}\n\
                 \x20    Result.Err(e) => { println(f\"e:{e}\") }\n\
                 \x20  } }\n\
                 fn main() { show(Result.Ok(K.A(R2 { s: f\"y\" }))); println(\"end\") }\n",
            vec!["a:y", "dK", "end"],
        ),
        // INLINE payload — no box. The disarm must decline here; the outer
        // zeroing already covers it.
        (
            "inline-payload-untouched",
            "fn show(x: Option[S]) {\n\
                 \x20  match x {\n\
                 \x20    Option.Some(S.A(m)) => { println(f\"n:{m.n}\") }\n\
                 \x20    Option.Some(S.B) => {}\n\
                 \x20    Option.None => {}\n\
                 \x20  } }\n\
                 fn main() { show(Option.Some(S.A(Small { n: 7 }))); println(\"end\") }\n",
            vec!["n:7", "end"],
        ),
    ] {
        let src = format!("{H}{body}");
        assert_clean_asan_run(&src, &want, label);
    }
}

#[test]
fn asan_reassigning_a_moved_in_boxed_payload_frees_the_envelope() {
    const H: &str = "enum Val { Nothing, Ident(String) }\n\
             struct Big { s: String, a: i64, b: i64, c: i64, d: i64 }\n\
             struct A { value: Option[Val] }\n\
             fn ident_len(v: Val) -> i64 { match v { Val.Ident(s) => (if s.contains(\"moved-in\") { s.len() } else { 0 }), Val.Nothing => 0 } }\n\
             fn big_len(w: Big) -> i64 { if w.s.contains(\"moved-in\") { w.s.len() } else { 0 } }\n\
             fn val_none() -> Option[Val] { Option.None }\n\
             fn big_none() -> Option[Big] { Option.None }\n\
             fn seed() -> i64 { env.args().len() }\n\
             fn payload() -> String { f\"payload-moved-in-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaa\" }\n\
             fn mk_val() -> Option[Val] { Some(Val.Ident(payload())) }\n\
             fn mk_big() -> Option[Big] { Some(Big { s: payload(), a: 1, b: 2, c: 3, d: 4 }) }\n\
             fn mk_a() -> A { A { value: mk_val() } }\n";
    for (label, body, want) in [
            // ── the row's own shape, and the three axes it wrongly implicated ──
            (
                "whilelet-reassign",
                "fn f(a: A) -> i64 { let A { value } = a; let mut vv = value; let mut acc = 0;\n\
                 \x20  while let Some(v) = vv { acc = acc + ident_len(v); vv = val_none(); } acc }\n\
                 fn main() { println(f(mk_a())); }\n",
                "45",
            ),
            // No struct, no destructure — a plain local alias is enough.
            (
                "local-alias",
                "fn f() -> i64 { let value = mk_val(); let mut vv = value; let mut acc = 0;\n\
                 \x20  while let Some(v) = vv { acc = acc + ident_len(v); vv = val_none(); } acc }\n\
                 fn main() { println(f()); }\n",
                "45",
            ),
            // The alias source is a PARAM rather than a local.
            (
                "param-alias",
                "fn f(value: Option[Val]) -> i64 { let mut vv = value; let mut acc = 0;\n\
                 \x20  while let Some(v) = vv { acc = acc + ident_len(v); vv = val_none(); } acc }\n\
                 fn main() { println(f(mk_val())); }\n",
                "45",
            ),
            // `match`, not `while let` — the loop was never load-bearing.
            (
                "match-not-whilelet",
                "fn f() -> i64 { let value = mk_val(); let mut vv = value;\n\
                 \x20  let n = match vv { Some(v) => ident_len(v), None => 0 };\n\
                 \x20  vv = val_none(); n + (if vv.is_some() { 1 } else { 0 }) }\n\
                 fn main() { println(f()); }\n",
                "45",
            ),
            // The payload is never CONSUMED — a borrow-only read still leaks.
            (
                "borrow-only-arm",
                "fn f() -> i64 { let value = mk_val(); let mut vv = value; let mut acc = 0;\n\
                 \x20  while let Some(v) = vv { acc = acc + ident_len(v); vv = val_none(); }\n\
                 \x20  acc }\n\
                 fn main() { println(f()); }\n",
                "45",
            ),
            // A boxed STRUCT payload, not an enum one — same 100-byte orphan.
            (
                "boxed-struct-payload",
                "fn f() -> i64 { let value = mk_big(); let mut vv = value;\n\
                 \x20  let n = match vv { Some(w) => big_len(w), None => 0 };\n\
                 \x20  vv = big_none(); n + (if vv.is_some() { 1 } else { 0 }) }\n\
                 fn main() { println(f()); }\n",
                "45",
            ),
            // Reassigned to another `Some`, not to `None`.
            (
                "reassign-to-some",
                "fn f() -> i64 { let value = mk_val(); let mut vv = value;\n\
                 \x20  let n = match vv { Some(v) => ident_len(v), None => 0 };\n\
                 \x20  vv = Some(Val.Nothing);\n\
                 \x20  n + (match vv { Some(v) => ident_len(v), None => 0 }) }\n\
                 fn main() { println(f()); }\n",
                "45",
            ),
            // N stores orphaned N boxes, so the shape is linear rather than
            // one-off. Six iterations over runtime-derived payloads.
            (
                "loop-reassign",
                "fn fresh(i: i64) -> Option[Val] { Some(Val.Ident(f\"payload-moved-in-{i}-aaaaaaaaaaaaaaaaaaaaaaaaaa\")) }\n\
                 fn f() -> i64 { let value = mk_val(); let mut vv = value; let mut acc = 0; let mut i = 0;\n\
                 \x20  while i < 6 { acc = acc + (match vv { Some(v) => ident_len(v), None => 0 });\n\
                 \x20                vv = fresh(i); i = i + 1; }\n\
                 \x20  acc + (match vv { Some(v) => ident_len(v), None => 0 }) }\n\
                 fn main() { println(f()); }\n",
                "315",
            ),
            // ── the hazard rows: the payload ESCAPES, then the slot is stored ──
            (
                "match-hands-out",
                "fn f() -> i64 { let value = mk_val(); let mut vv = value;\n\
                 \x20  let k = match vv { Some(g) => g, None => Val.Nothing };\n\
                 \x20  vv = val_none();\n\
                 \x20  ident_len(k) + (if vv.is_some() { 1 } else { 0 }) }\n\
                 fn main() { println(f()); }\n",
                "45",
            ),
            // The `if let` spelling of the row above — a UAF with only the
            // eager-free half of the fix, a leak with neither.
            (
                "iflet-hands-out",
                "fn f() -> i64 { let value = mk_val(); let mut vv = value;\n\
                 \x20  let k = if let Some(g) = vv { g } else { Val.Nothing };\n\
                 \x20  vv = val_none();\n\
                 \x20  ident_len(k) + (if vv.is_some() { 1 } else { 0 }) }\n\
                 fn main() { println(f()); }\n",
                "45",
            ),
            // Braced `if let` tail — the block-vs-bare distinction that
            // B-2026-08-28-66 found on the `match` side.
            (
                "iflet-hands-out-braced",
                "fn f() -> i64 { let value = mk_val(); let mut vv = value;\n\
                 \x20  let k = if let Some(g) = vv { { g } } else { Val.Nothing };\n\
                 \x20  vv = val_none();\n\
                 \x20  ident_len(k) + (if vv.is_some() { 1 } else { 0 }) }\n\
                 fn main() { println(f()); }\n",
                "45",
            ),
            // Two aliases off one source, only the second reassigned.
            (
                "two-aliases-one-stored",
                "fn f() -> i64 { let value = mk_val(); let mut vv = value; let mut ww = vv;\n\
                 \x20  ww = val_none();\n\
                 \x20  match ww { Some(g) => ident_len(g), None => 45 } }\n\
                 fn main() { println(f()); }\n",
                "45",
            ),
            // ── boundaries: each was CLEAN before the fix and must stay so ──
            //
            // The retraction must NOT fire for a discarded if-let: nothing
            // downstream owns the payload, so neutralizing the box's walk
            // there trades this row's leak for a double free.
            (
                "discarded-iflet",
                "fn f() -> i64 { let value = mk_val(); let mut vv = value;\n\
                 \x20  if let Some(g) = vv { g };\n\
                 \x20  vv = val_none(); if vv.is_some() { 1 } else { 0 } }\n\
                 fn main() { println(f()); }\n",
                "0",
            ),
            // No store at all — the scope-exit action is the only owner and
            // was always correct.
            (
                "alias-never-stored",
                "fn f() -> i64 { let value = mk_val(); let mut vv = value; let mut acc = 0;\n\
                 \x20  while let Some(v) = vv { acc = acc + ident_len(v); break } acc }\n\
                 fn main() { println(f()); }\n",
                "45",
            ),
            // A directly-initialized binding — never in the excluded set, so
            // this is the axis rather than a confirmation.
            (
                "direct-init-control",
                "fn f() -> i64 { let mut vv = mk_val();\n\
                 \x20  let n = match vv { Some(v) => ident_len(v), None => 0 };\n\
                 \x20  vv = val_none(); n + (if vv.is_some() { 1 } else { 0 }) }\n\
                 fn main() { println(f()); }\n",
                "45",
            ),
            // `vv = vv` must not free the buffer it is about to store back.
            (
                "self-assign",
                "fn f() -> i64 { let value = mk_val(); let mut vv = value; vv = vv;\n\
                 \x20  match vv { Some(v) => ident_len(v), None => 0 } }\n\
                 fn main() { println(f()); }\n",
                "45",
            ),
            // A roundtrip through a callee — the RHS mentions the target, so
            // the old value may have moved into the call; the free declines.
            (
                "roundtrip",
                "fn pass(o: Option[Val]) -> Option[Val] { o }\n\
                 fn f() -> i64 { let value = mk_val(); let mut vv = value; vv = pass(vv);\n\
                 \x20  match vv { Some(v) => ident_len(v), None => 0 } }\n\
                 fn main() { println(f()); }\n",
                "45",
            ),
            // The one row with NO match, no `if let` and no `while let`: an
            // alias and a store, nothing else. `-O0`-only, per the doc above.
            (
                "no-construct-at-all",
                "fn f() -> i64 { let value = mk_val(); let mut vv = value; vv = val_none();\n\
                 \x20  if vv.is_some() { 1 } else { 0 } }\n\
                 fn main() { println(f()); }\n",
                "0",
            ),
        ] {
            assert_clean_asan_run(&format!("{H}{body}"), &[want], label);
        }
}

// ── Direct recursive shared enum (RC tree) ────────────────────
//
// `shared enum Expr { Num(i64), Add(Expr, Expr) }` builds an RC tree whose
// children are RC handles. `eval` recursively consumes each child (passing
// it by value moves the handle). ASAN guards that the tree is freed exactly
// once — no leak (every node reclaimed) and no double-free (a moved child is
// not freed again at the parent's scope exit). This is the allocation
// correctness check for the direct-recursion feature; the by-value layout
// bug that preceded it would have ICE'd before reaching a binary at all.

#[test]
fn asan_direct_recursive_shared_enum_tree_freed_once() {
    assert_clean_asan_run(
        r#"
shared enum Expr {
    Num(i64),
    Add(Expr, Expr),
}
fn eval(e: Expr) -> i64 {
    match e {
        Num(n) => n,
        Add(a, b) => eval(a) + eval(b),
    }
}
fn main() {
    let e = Add(Num(3), Add(Num(4), Num(5)));
    println(eval(e));
}
"#,
        &["12"],
        "direct_recursive_shared_enum_tree",
    );
}

#[test]
fn asan_direct_recursive_shared_enum_single_field_no_leak() {
    assert_clean_asan_run(
        r#"
shared enum Wrap {
    Leaf(i64),
    Box(Wrap),
}
fn depth(w: Wrap) -> i64 {
    match w {
        Leaf(n) => 0,
        Box(inner) => 1 + depth(inner),
    }
}
fn main() {
    let w = Box(Box(Box(Leaf(7))));
    println(depth(w));
}
"#,
        &["3"],
        "direct_recursive_shared_enum_single_field",
    );
}

#[test]
fn asan_recursive_shared_enum_children_freed_no_leak() {
    // B-2026-06-13-11: a recursive `shared enum` (AST/tree shape) must
    // recursively rc-dec its child boxes when the parent box's refcount
    // hits zero. Pre-fix `emit_rc_dec` plain-`free`d a shared enum box with
    // NO payload walk (shared enums cached `None` in `rc_drop_fns`), so
    // every child `Bin`/`Num` box leaked (~96 B / iter over the loop). The
    // new `emit_shared_enum_rc_drop_fn` tag-switches and walks each
    // variant's shared children. Looping makes the per-iteration leak
    // visible to the Linux-CI LSan gate; mac checks no double-free / UAF on
    // the recursive free. (Base-case-first variant order — recursive-first
    // is the separate B-2026-06-13-10 layout overflow.)
    assert_clean_asan_run(
        r#"
shared enum Expr { Num(i64), Bin(Expr, Expr) }
fn eval(e: Expr) -> i64 {
    match e {
        Num(n) => n,
        Bin(l, r) => eval(l) + eval(r),
    }
}
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 40 {
        let t: Expr = Bin(Num(i), Bin(Num(i), Num(2)));
        total = total + eval(t);
        i = i + 1;
    }
    println(total);
}
"#,
        &["1640"],
        "recursive_shared_enum_children_freed",
    );
}

#[test]
fn asan_shared_enum_fnret_temp_arg_freed_no_leak() {
    // B-2026-06-19-3 — the self-hosted parser's `render_expr(parse_expr(src))`
    // leak: a bare `shared enum` AST node, produced as a function-return (or inline
    // variant-ctor) TEMPORARY and passed BY VALUE to a consumer, was never
    // freed. A bare-shared by-value param is NET-ZERO (callee `emit_refcount_inc`
    // at entry + `track_rc_var` dec at exit — the caller-keeps-reference
    // convention), so the caller still owns the temp's +1; but a directly
    // passed temp has no binding to carry that dec, so the box leaked once per
    // call (input `"1"`: a single 80-byte node; a deep parse: the whole tree).
    // A let-bound producer (`let e = parse(...); render(e)`) was always freed
    // via the binding's scope-exit dec — only the *temporary* arg leaked. The
    // fix queues the caller-side dec for a fresh bare-shared box arg
    // (`fresh_arg_bare_shared_heap_type` + `track_rc_var`, call_dispatch.rs),
    // mirroring the Vec/String fresh-temp-arg arm next to it.
    //
    // Shape mirrors `render_expr(parse_expr(src))`: `build` returns a fresh RC
    // tree, `render` consumes it by value and recurses on the destructured
    // children. Looped so the per-iteration box leak accumulates LSan-visibly;
    // each `Expr` box is >=48 bytes, above the short-allocation reachability
    // floor LSan silently tolerates. Payloads are all-scalar (`Span`) on
    // purpose — this pins the BOX free, isolated from the orthogonal
    // whole-struct-payload-binding String-field drop (a separate gap).
    assert_clean_asan_run(
        r#"
struct Span { line: i64, column: i64, offset: i64, length: i64 }
struct BinData { left: Expr, right: Expr, span: Span }
shared enum Expr { Int(Span), Bin(BinData) }
fn build(depth: i64, v: i64) -> Expr {
    if depth <= 0 {
        return Expr.Int(Span { line: v, column: 0, offset: v, length: 1 });
    }
    let l = build(depth - 1, v);
    let r = build(depth - 1, v + 1);
    return Expr.Bin(BinData { left: l, right: r, span: Span { line: 0, column: 0, offset: 0, length: 2 } });
}
fn render(e: Expr) -> String {
    let mut out = "".to_string();
    match e {
        Int(n) => { out.push_str("(int "); out.push_str(n.line.to_string()); out.push_str(")"); }
        Bin(b) => {
            let BinData { left, right, span } = b;
            out.push_str("(bin ");
            out.push_str(span.length.to_string());
            out.push_str(" ");
            out.push_str(render(left));
            out.push_str(" ");
            out.push_str(render(right));
            out.push_str(")");
        }
    }
    out
}
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 50 {
        // render(build(..)) — the tree is a function-return temporary passed by
        // value; render(Expr.Int(..)) — an inline variant-ctor temporary.
        total = total + render(build(3, i)).len();
        total = total + render(Expr.Int(Span { line: i, column: 0, offset: i, length: 1 })).len();
        i = i + 1;
    }
    if total > 0 { println("ok"); } else { println("bad"); }
}
"#,
        &["ok"],
        "shared_enum_fnret_temp_arg_freed",
    );
}

#[test]
fn asan_shared_enum_struct_variant_no_leak_no_double_free() {
    // B-2026-06-13-8: a shared enum struct-variant with a heap (String)
    // payload field — construct the RC box, match-bind the field, drop. The
    // box and its String buffer must be freed exactly once (the Linux-CI
    // LSan job is the leak gate; mac catches double-free/UAF). Looped to
    // make a per-iteration leak or double-free trip the sanitizer. (Uses
    // the base-case-first variant order — recursive-variant-first is a
    // separate pre-existing layout overflow, B-2026-06-13-9.)
    assert_clean_asan_run(
        r#"
shared enum Msg { Empty, Text { body: String, code: i64 } }
fn render(m: Msg) -> i64 {
    match m {
        Text { body, code } => body.len() + code,
        Empty => 0,
    }
}
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 50 {
        let m: Msg = Msg.Text { body: f"line-{i}", code: i };
        total = total + render(m);
        i = i + 1;
    }
    println(total);
}
"#,
        &["1565"],
        "shared_enum_struct_variant",
    );
}

#[test]
fn asan_freshtemp_enum_method_no_double_free() {
    // Slice 3k: a user method on a fresh-temp VALUE-ENUM receiver
    // (`make().size()`), looped. The `Text` variant owns a heap `String`; the
    // temp materializes into `__urecv_tmp` and is drop-tracked via
    // `track_enum_var`, whose scope-exit `EnumDrop` runs `__karac_drop_Msg` to
    // free the payload String once. Hazards: the temp must free exactly once
    // (macOS ASAN catches a double-free), and the payload String must free at
    // all (Linux LSan catches a leak). ≥36-byte payload defeats LSan
    // short-string reachability; the loop re-materializes each pass.
    assert_clean_asan_run(
        r#"
enum Msg { Text(String), Empty }
impl Msg {
    fn size(self) -> i64 {
        match self {
            Msg.Text(s) => return s.len(),
            Msg.Empty => return 0_i64,
        };
    }
}
fn make() -> Msg { Msg.Text("a message payload string padded beyond thirty-six bytes") }
fn main() {
    let mut p = 0;
    while p < 3 {
        println(make().size());
        p = p + 1;
    };
}
"#,
        &["55", "55", "55"],
        "freshtemp_enum_method_no_double_free",
    );
}

#[test]
fn asan_freshtemp_shared_enum_method_no_double_free() {
    // Slice 3k: the shared-ENUM sibling. A `shared enum` receiver is `Shared`
    // and RC-managed, so it rides the same `track_rc_var` path as the shared
    // struct (`track_enum_var` no-ops for shared enums — DP3). The temp
    // materializes into `__urecv_tmp` and one scope-exit `RcDec` →
    // `__karac_rc_drop_Expr` frees the box and the live `Name` payload String.
    // Same double-free (ASAN) / leak (LSan) hazards as the shared-struct case;
    // guards that the shared-enum branch isn't mis-routed to the value-enum
    // drop (which would double-count).
    assert_clean_asan_run(
        r#"
shared enum Expr { Lit(i64), Name(String) }
impl Expr {
    fn weight(self) -> i64 {
        match self {
            Expr.Lit(n) => return n,
            Expr.Name(s) => return s.len(),
        };
    }
}
fn make() -> Expr { Expr.Name("an expr name payload padded beyond thirty-six bytes") }
fn main() {
    let mut p = 0;
    while p < 3 {
        println(make().weight());
        p = p + 1;
    };
}
"#,
        &["51", "51", "51"],
        "freshtemp_shared_enum_method_no_double_free",
    );
}

#[test]
fn asan_ref_param_enum_field_payload_consume_no_leak_no_double_free() {
    // B-2026-07-21-5/-6 memory leg: an ESCAPING `match <refparam>.field {
    // Ident(name) => <consume name> }` now deep-clones the scrutinee
    // (`clone_escaping_borrowed_ref_chain_enum`) instead of GEPing the ref
    // param's pointer slot out of bounds (which corrupted adjacent stack
    // slots into empty-string reads or double-frees, flipping with
    // opt-level/layout). The clone rides the freshtemp enum drop-tracking:
    // a consuming arm's binding frees the clone's payload exactly once, a
    // no-bind arm's freshtemp drop frees the untouched clone (a lost track
    // would LEAK — LSan), and the CALLER's value is never cap-zeroed
    // through the borrow, so its own drop still frees its buffer exactly
    // once (a stolen payload would DOUBLE-FREE — ASAN). Exercises the
    // concat-consume, identity-return, no-bind-arm, and repeat-call
    // shapes in a loop so any per-iteration imbalance accumulates.
    assert_clean_asan_run(
        r#"
enum Tok { Plus, Ident(String) }
struct SpTok { tok: Tok, n: i64 }
fn render(st: ref SpTok) -> String {
    match st.tok {
        Ident(name) => { return "id:".to_string() + name; }
        Plus => { return "+".to_string(); }
    }
    return "?".to_string();
}
fn take_name(st: ref SpTok) -> String {
    match st.tok {
        Ident(name) => { return name; }
        Plus => { return "+".to_string(); }
    }
    return "?".to_string();
}
fn main() {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 40 {
        let a = SpTok { tok: Tok.Ident("payload".to_string()), n: 1 };
        acc = acc + render(a).len();
        acc = acc + render(a).len();
        acc = acc + take_name(a).len();
        let p = SpTok { tok: Tok.Plus, n: 2 };
        acc = acc + render(p).len();
        i = i + 1;
    }
    println(acc);
}
"#,
        &["1120"],
        "ref_param_enum_field_payload_consume",
    );
}

/// B-2026-07-11-26: a fresh-temp HEAP-bearing enum scrutinee with a user
/// `impl Drop`, matched in an if-let that MOVES the heap payload into a
/// binding. The user Drop body runs (side effect `D`) AND the moved-out Vec
/// is freed exactly once — the binding owns it, the enum user-drop wrapper
/// runs only the body (its field handoff is struct-only), and item-B's
/// field cleanup is suppressed for the moved-in field. Asserts no leak
/// (LSan) and no use-after-free / double-free (ASAN) — the class the new
/// user-drop registration on materialized enum scrutinees could regress.
#[test]
fn asan_freshtemp_enum_scrutinee_user_drop_no_double_free() {
    assert_clean_asan_run(
            "enum Msg { Text(Vec[i64]), Empty }\n\
             impl Drop for Msg { fn drop(mut ref self) { println(\"D\"); } }\n\
             fn mk(hit: bool) -> Msg { if hit { Msg.Text([1, 2, 3]) } else { Msg.Empty } }\n\
             fn main() {\n\
                 if let Msg.Text(v) = mk(true) { println(f\"{v.len()}\"); } else { println(\"m\"); }\n\
             }",
            &["3", "D"],
            "freshtemp_enum_scrutinee_user_drop_no_double_free",
        );
}

#[test]
fn asan_struct_move_nulls_shared_enum_handle_field() {
    // The `shared enum` sibling: its handle is the same inline `ptr`, and
    // it fell through the same gap (skipped by the `is_shared` guard on the
    // `enum_layouts` arm). Read back through the Vec rather than through a
    // second binding — unlike a shared STRUCT handle, the ownership checker
    // treats a shared enum handle moved into a struct field as a real move
    // and rejects a later use of the source.
    assert_clean_asan_run(
        r#"
shared enum Node { Leaf(i64) }
struct Holder { node: Node }
fn main() {
    let mut v: Vec[Holder] = Vec.new();
    let h = Holder { node: Node.Leaf(3) };
    v.push(h);
    match v[0].node { Leaf(x) => println(x) }
}
"#,
        &["3"],
        "struct_move_nulls_shared_enum_handle_field",
    );
}

#[test]
fn asan_compound_enum_drop_skips_no_payload_variant() {
    // No-payload variant lands on the default `ret` arm of the
    // tag-switch — no spurious free, no UAF on the unset payload
    // words. `V2` has zero heap-bearing fields, but the enum
    // itself has at least one heap-bearing variant so the drop
    // fn is still synthesized; verifies the per-variant arm
    // structure handles the trivial case correctly.
    assert_clean_asan_run(
        r#"
enum E { V1(String), V2 }
fn main() {
    let _e = V2;
    println(1);
}
"#,
        &["1"],
        "compound_enum_drop_skips_no_payload_variant",
    );
}

#[test]
fn asan_compound_enum_drop_handles_mixed_width_variants() {
    // Mixed-width: V1(i64) at one tag, V2(String) at another.
    // Constructing each in turn must route through the right
    // cleanup arm — V1's primitive payload triggers no work,
    // V2's String payload frees the buffer. Each construction
    // is in a nested scope to test the per-scope drain timing
    // (the heap String buffer is freed at the inner block's
    // close, not deferred to `main`'s exit).
    assert_clean_asan_run(
        r#"
enum E { V1(i64), V2(String) }
fn main() {
    {
        let _a = V1(42);
    }
    {
        let mut s = String.new();
        s.push_str("hello");
        let _b = V2(s);
    }
    println(1);
}
"#,
        &["1"],
        "compound_enum_drop_handles_mixed_width_variants",
    );
}

#[test]
fn asan_let_bound_enum_heap_payload_moved_out_no_double_free() {
    // #9 (phase-12 self-hosting): a bare `let`-bound enum whose active
    // variant carries a heap payload, moved OUT of the binding by `return`
    // (`fn make() { let e = E.A(..); e }`) or by `let g = f`, transfers
    // ownership — the source's `EnumDrop` is suppressed (cap-zeroed) so
    // only the consumer frees. Without the fix the source double-frees the
    // String buffer (use-after-free → SIGTRAP / ASAN double-free). Covers
    // BOTH fixed move paths plus the non-heap `N` variant and a loop (to
    // surface a missing source-suppression OR a missing consumer free).
    // NOTE: the by-value-call-arg-then-transfer path (passing the enum to
    // a fn that re-wraps it into its return) is the SEPARATE, general
    // blocker #14 (it double-frees for structs too) and is NOT covered.
    assert_clean_asan_run(
        r#"
enum E { A(String), B(i64, String), N(i64) }
fn make(tag: i64) -> E {
    if tag == 0 {
        let e = E.A("alpha".to_string());
        e
    } else if tag == 1 {
        let e = E.B(7, "beta".to_string());
        e
    } else {
        let e = E.N(99);
        e
    }
}
fn main() {
    // return-of-let-bound-enum, consumed by an INLINE match (not re-transferred
    // through another call — that transfer path is #14, not covered here).
    let r = make(0);
    match r { A(s) => println(s), B(n, s) => { println(n.to_string()); println(s); } N(x) => println(x.to_string()) }
    let r1 = make(1);
    match r1 { A(s) => println(s), B(n, s) => { println(n.to_string()); println(s); } N(x) => println(x.to_string()) }
    let r2 = make(2);
    match r2 { A(s) => println(s), B(n, s) => { println(n.to_string()); println(s); } N(x) => println(x.to_string()) }
    // `let g = f` enum move — g is the sole owner, f's drop suppressed.
    let f = make(0);
    let g = f;
    match g { A(s) => println(s), B(n, s) => { println(n.to_string()); println(s); } N(x) => println(x.to_string()) }
    // loop: each iteration's return-bound enum frees exactly once.
    let mut i = 0;
    while i < 3 {
        let x = make(0);
        match x { A(s) => println(s), B(n, s) => { println(n.to_string()); println(s); } N(y) => println(y.to_string()) }
        i = i + 1;
    }
}
"#,
        &[
            "alpha", "7", "beta", "99", "alpha", "alpha", "alpha", "alpha",
        ],
        "let_bound_enum_heap_payload_moved_out_no_double_free",
    );
}

#[test]
fn asan_struct_with_direct_enum_field_no_leak_no_double_free() {
    // #15 (phase-12 self-hosting): a non-shared struct's synthesized drop
    // used to IGNORE enum-typed fields (an enum's LLVM layout is all-i64
    // words, invisible to the type-driven nested-aggregate pass), leaking
    // the live variant's String/Vec payload at the owning struct's scope
    // exit. `emit_struct_drop_synthesis` now invokes the enum's own
    // `__karac_drop_<E>` switch on a DIRECT enum field (Linux LSan catches a
    // regression of the leak; the `Span` shape mirrors the bootstrap's
    // `SpannedToken { tok: Token, .. }`).
    //
    // #15 is NOT coupled to a #14 double-free: under the caller-retains
    // model, a by-value aggregate param is UNTRACKED in the callee, so only
    // ONE tracked binding (the caller's source, or the transferred-out
    // result) ever frees the enum payload — freeing the enum field at drop
    // does not introduce an alias double-free. Verified empirically across
    // direct-return transfer-out, read-then-reuse, and consume-and-drop.
    // (The struct->struct->enum NESTED leak — `Wrap { sp: Span }` — is a
    // pre-existing, deeper instance left to #18; it is deliberately NOT
    // exercised here so Linux `detect_leaks=1` stays green.)
    assert_clean_asan_run(
        r#"
enum Tok { Id(String), Int(i64) }
struct Span { tok: Tok, off: i64 }
fn wrap(s: Span) -> Span { s }
fn make_spanned(t: Tok, o: i64) -> Span { Span { tok: t, off: o } }
fn peek(s: ref Span) -> i64 { s.off }
fn sink(s: String) -> i64 { s.len() }
fn drop_only(s: Span) { if s.off > 99999 { println(s.off.to_string()); } }
fn main() {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 4 {
        // Leak path: built, kept live (off read), dropped without destructure.
        let a = Span { tok: Tok.Id(f"a-{i}"), off: i };
        if a.off > 99999 { println("never"); }

        // `match spanned.tok` that CONSUMES the bound payload (moves it into
        // `sink`) — the bootstrap pattern. The owning struct's drop must skip
        // the consumed field (the double-free #15 had to suppress); `sink`
        // owning + struct drop freeing the same buffer would abort under ASAN.
        let b = Span { tok: Tok.Id(f"b-{i}"), off: i };
        let c = wrap(b);
        match c.tok { Id(s) => { acc = acc + sink(s); } Int(n) => { acc = acc + n; } }

        // The `make_spanned(token)` shape: a callee-owned enum param wrapped
        // into a returned struct literal, then field-matched + consumed.
        let t = Tok.Id(f"t-{i}");
        let sp = make_spanned(t, i);
        match sp.tok { Id(s) => { acc = acc + sink(s); } Int(n) => { acc = acc + n; } }

        // Local struct, field-match + consume (no function in the path).
        let d = Span { tok: Tok.Id(f"d-{i}"), off: i };
        match d.tok { Id(s) => { acc = acc + sink(s); } Int(n) => { acc = acc + n; } }

        // Read-then-reuse: forces caller-retains aliasing (the source must
        // survive two by-value uses) — a missing entry-copy here would crash;
        // #15 freeing the enum field must still free it exactly once.
        let r = Span { tok: Tok.Id(f"r-{i}"), off: i };
        let x = peek(r);
        let y = peek(r);
        if x + y > 99999 { println("never"); }

        // Consume-and-drop (no transfer) of a struct-with-enum-field.
        let e = Span { tok: Tok.Id(f"e-{i}"), off: i };
        drop_only(e);

        i = i + 1;
    }
    if acc > 99999 { println("never"); }
    println("done");
}
"#,
        &["done"],
        "struct_with_direct_enum_field_no_leak_no_double_free",
    );
}

#[test]
fn asan_struct_nested_enum_leaf_no_leak_no_double_free() {
    // #18 (phase-12 self-hosting): a struct whose only heap is TRANSITIVELY
    // inside an enum nested under ANOTHER struct field — `Wrap { sp: Span }`
    // where `Span { tok: Tok }` and `Tok` is heap-bearing. #15 freed only a
    // DIRECT enum field; the nested path went through the type-driven
    // `emit_aggregate_heap_field_frees`, which is enum-blind (an enum's
    // layout is all-i64 words). `emit_struct_drop_synthesis` now routes a
    // NAMED nested struct field through that struct's own
    // `__karac_drop_struct_<S>` (which post-#15 frees its enum fields), so
    // `Wrap`'s drop reaches `sp.tok`'s payload. Linux LSan catches a
    // regression of the leak; ASAN everywhere catches the double-free the
    // nested match-consume could introduce.
    //
    // Double-free coupling (mirrors #15's struct-field-match sub-fix, one
    // level deeper): once `Wrap`'s drop frees `sp.tok`, a `match c.sp.tok`
    // arm that CONSUMES the bound payload would double-free unless the match
    // suppression cap-zeros the consumed field in the SOURCE struct.
    // `suppress_destructured_struct_field_enum_cleanup` walks the full
    // `ident.f1.f2…` field-access chain for exactly this case.
    //
    // All payloads are the `Id(String)` variant and every `Int`/numeric arm
    // is guarded behind an impossible `> 99999`, so no `to_string` temp is
    // ever materialized — keeping Linux `detect_leaks=1` green against the
    // separate, pre-existing `println(x.to_string())` argument-temp leak.
    assert_clean_asan_run(
        r#"
enum Tok { Id(String), Int(i64) }
struct Span { tok: Tok, off: i64 }
struct Wrap { sp: Span, hi: i64 }
struct Deep { w: Wrap, tag: i64 }
fn sink(s: String) -> i64 { s.len() }
fn mk_wrap(n: i64) -> Wrap { Wrap { sp: Span { tok: Tok.Id(f"w-{n}"), off: n }, hi: n + 1 } }
fn fwd(w: Wrap) -> Wrap { w }
fn peek(w: ref Wrap) -> i64 { w.hi }
fn main() {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 4 {
        // Nested leak path: Wrap built, kept live (hi read), dropped WITHOUT
        // destructure — Wrap's drop must free sp.tok's payload (the #18 leak).
        let a = Wrap { sp: Span { tok: Tok.Id(f"a-{i}"), off: i }, hi: i };
        if a.hi > 99999 { println("never"); }

        // Struct-literal MOVE: a Span local moved into a Wrap literal, then the
        // Wrap dropped undestructured. The source `span` must not also free the
        // enum payload (move-suppression vs the now-active nested drop).
        let span = Span { tok: Tok.Id(f"s-{i}"), off: i };
        let m = Wrap { sp: span, hi: i };
        if m.hi > 99999 { println("never"); }

        // Transfer-out + nested match-consume (the double-free risk): the bound
        // String moves into `sink`; `c`'s drop must skip the consumed field.
        let b = mk_wrap(i);
        let c = fwd(b);
        match c.sp.tok { Id(s) => { acc = acc + sink(s); } Int(n) => { if n > 99999 { println("never"); } } }

        // Local nested match-consume (no function in the path).
        let d = Wrap { sp: Span { tok: Tok.Id(f"d-{i}"), off: i }, hi: i };
        match d.sp.tok { Id(s) => { acc = acc + sink(s); } Int(n) => { if n > 99999 { println("never"); } } }

        // Read-then-reuse a Wrap (caller-retains aliasing), then drop it
        // undestructured — the nested payload must be freed exactly once.
        let r = mk_wrap(i);
        let x = peek(r);
        let y = peek(r);
        if x + y > 99999 { println("never"); }

        // Three-level nesting: Deep -> Wrap -> Span -> Tok, dropped
        // undestructured — the recursive struct-drop routing must descend.
        let deep = Deep { w: mk_wrap(i), tag: i };
        if deep.tag > 99999 { println("never"); }

        i = i + 1;
    }
    if acc > 99999 { println("never"); }
    println("done");
}
"#,
        &["done"],
        "struct_nested_enum_leaf_no_leak_no_double_free",
    );
}

#[test]
fn asan_struct_tuple_enum_leaf_no_leak_no_double_free() {
    // #21 (phase-12 self-hosting): a struct field that is an anonymous TUPLE
    // whose only heap is inside an enum leaf — `struct H { pe: (Tok, i64) }`
    // with heap enum `Tok`. The struct drop's `NestedTuple` path now frees
    // the tuple's enum leaf (the bounded leak #21 reported), paired with
    // cap-zero suppression at every move-out site and entry-copy of
    // heap-bearing tuple params (the cross-function P6 case). This stresses
    // the whole move-out matrix in one loop: undestructured drop (the leak),
    // full-tuple destructure + consume, direct tuple-index match, tuple-index
    // let-move, whole-tuple let + cross-fn consume, whole-tuple arg, enum
    // arg, whole-struct move, nested struct/tuple, and a tuple-literal arg —
    // each clean on main; with the partial #21 fix the consume shapes
    // double-freed. Every numeric arm is guarded behind an impossible
    // `> 99999` so no `to_string` temp is materialized (keeps Linux LSan
    // green vs the separate println-temp leak).
    assert_clean_asan_run(
        r#"
enum Tok { Id(String), Int(i64) }
struct Inner { tok: Tok, k: i64 }
struct H { pe: (Tok, i64), tag: i64 }
struct Hn { pn: ((Tok, i64), i64), tag: i64 }
struct Hs { ps: (Inner, i64), tag: i64 }
struct Hv { pv: (Vec[i64], i64), tag: i64 }
fn mk(n: i64) -> H { H { pe: (Tok.Id(f"e-{n}"), n), tag: n } }
fn mkn(n: i64) -> Hn { Hn { pn: ((Tok.Id(f"n-{n}"), n), n), tag: n } }
fn mks(n: i64) -> Hs { Hs { ps: (Inner { tok: Tok.Id(f"s-{n}"), k: n }, n), tag: n } }
fn mkv(n: i64) -> Hv { let mut v: Vec[i64] = Vec.new(); v.push(n); Hv { pv: (v, n), tag: n } }
fn sink(s: String) -> i64 { s.len() }
fn sinkv(v: Vec[i64]) -> i64 { v.len() }
fn sinkt(p: (Tok, i64)) -> i64 { match p.0 { Id(s) => s.len(), Int(z) => { if z > 99999 { 1 } else { 0 } } } }
fn sinke(t: Tok) -> i64 { match t { Id(s) => s.len(), Int(z) => { if z > 99999 { 1 } else { 0 } } } }
fn sinki(p: (Inner, i64)) -> i64 { match p.0.tok { Id(s) => s.len(), Int(z) => { if z > 99999 { 1 } else { 0 } } } }
fn main() {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 8 {
        // P0 — undestructured drop (the #21 leak target).
        let a = mk(i);
        if a.tag > 99999 { println("never"); }

        // P1 — full-tuple destructure then consume the enum leaf via match.
        let b = mk(i);
        let (t, n) = b.pe;
        match t { Id(s) => { acc = acc + sink(s); } Int(z) => { if z > 99999 { println("never"); } } }
        acc = acc + n;

        // P3 — direct tuple-index match scrutinee.
        let c = mk(i);
        match c.pe.0 { Id(s) => { acc = acc + sink(s); } Int(z) => { if z > 99999 { println("never"); } } }

        // P4 — tuple-index let-move into an enum binding, then consume.
        let d = mk(i);
        let x = d.pe.0;
        match x { Id(s) => { acc = acc + sink(s); } Int(z) => { if z > 99999 { println("never"); } } }

        // P5 / P6 — whole-tuple by value to a fn that matches an element
        // internally (cross-boundary; needs entry-copy of the tuple param).
        let e = mk(i);
        let pp = e.pe;
        acc = acc + sinkt(pp);
        let f = mk(i);
        acc = acc + sinkt(f.pe);

        // P7 — enum arg extracted from a tuple field (caller-retained).
        let g = mk(i);
        acc = acc + sinke(g.pe.0);

        // P8 — whole-struct move then drop undestructured.
        let h = mk(i);
        let h2 = h;
        if h2.tag > 99999 { println("never"); }

        // Tuple-literal arg with an enum element (caller-temp + entry-copy).
        acc = acc + sinkt((Tok.Id(f"L-{i}"), i));

        // Nested tuple / nested struct in a tuple, undestructured + consumed.
        let nt = mkn(i);
        if nt.tag > 99999 { println("never"); }
        match nt.pn.0.0 { Id(s) => { acc = acc + sink(s); } Int(z) => { if z > 99999 { println("never"); } } }
        let ns = mks(i);
        acc = acc + sinki(ns.ps);

        // Direct-Vec tuple regression (must stay clean).
        let v = mkv(i);
        let (vv, vn) = v.pv;
        acc = acc + sinkv(vv) + vn;

        i = i + 1;
    }
    if acc > 999999 { println("never"); }
    println("done");
}
"#,
        &["done"],
        "struct_tuple_enum_leaf_no_leak_no_double_free",
    );
}

#[test]
fn asan_enum_field_struct_field_move_out_no_double_free() {
    // #19 (phase-12 self-hosting): the bootstrap lexer's `render()` shape —
    // iterate a `Vec[SpannedToken]` and pass each element BY VALUE to a fn that
    // moves the enum field OUT of its (now entry-copied) param into a local
    // (`let tk = t.token; match tk { … }`). The enum-field move-out cap-zeros
    // the source field in the owning struct's slot
    // (`suppress_struct_field_move_into_literal`'s enum arm) so the param's
    // struct drop and the moved-out local's drop free distinct buffers; without
    // it this double-freed (exit 133). All taken arms are `Id(String)` so no
    // `to_string` temp materializes.
    assert_clean_asan_run(
        r#"
enum Tok { Id(String), Eof }
struct Span2 { offset: i64, length: i64 }
struct Span { token: Tok, span: Span2 }
fn render(t: Span) -> String {
    let off = t.span.offset;
    let mut line = f"{off}:";
    let tk = t.token;
    match tk {
        Id(s) => line.push_str(s),
        Eof => line.push_str("eof"),
    }
    line
}
fn build(n: i64) -> Vec[Span] {
    let mut out: Vec[Span] = Vec.new();
    let mut i: i64 = 0;
    while i < n {
        out.push(Span { token: Tok.Id(f"t{i}"), span: Span2 { offset: i, length: 1 } });
        i = i + 1;
    }
    out.push(Span { token: Tok.Eof, span: Span2 { offset: n, length: 0 } });
    out
}
fn main() {
    let toks = build(3_i64);
    for t in toks {
        println(render(t));
    }
    println("done");
}
"#,
        &["0:t0", "1:t1", "2:t2", "3:eof", "done"],
        "enum_field_struct_field_move_out_no_double_free",
    );
}

#[test]
fn asan_enum_method_owned_self_payload_no_double_free() {
    // B-2026-07-18-47: an enum method with owned `self` matching its heap
    // payload — SelfValue was missed by the payload-move-out suppression, so
    // `self`'s enum-drop and the payload binding both freed the buffer.
    // Covers payload returned and payload consumed-to-scalar (the latter
    // double-freed even without moving the payload out).
    // Method names avoid the builtin Vec/String method namespace
    // (misrouted by a separate dispatch bug, B-2026-07-18-48).
    assert_clean_asan_run(
        r#"
enum E { V(String) }
impl E { fn extract(self) -> String { match self { E.V(s) => s } } }
impl E { fn length(self) -> i64 { match self { E.V(s) => s.len() } } }
fn main() {
    let e = E.V("hi".to_string());
    println(e.extract());
    let e2 = E.V("abcd".to_string());
    println(e2.length());
}
"#,
        &["hi", "4"],
        "enum_method_owned_self_payload",
    );
}

/// B-2026-08-01-3 — reassigning an enum binding over a variant whose
/// STRUCT payload carries interior heap (`Full(Res { name: String })`)
/// leaked the displaced payload's String on every assignment: the
/// Assign arm's eager-free ladder had Vec/Map/struct legs but no enum
/// leg. The leak was DCE-masked until the payload-bodies walk
/// (B-2026-07-30-11's enum-assign leg) made the old value live. The fix
/// runs the tag-dispatched `__karac_drop_<E>` switch on the old slot
/// after the bodies, before the store. Covers both the plain and the
/// own-`impl Drop` enum shapes; LSan (Linux CI) is the leak-side gate,
/// ASAN guards the switch against double-freeing what the bodies read.
#[test]
fn asan_enum_reassign_displaced_payload_heap_freed() {
    assert_clean_asan_run(
        r#"
struct Res { id: i64, name: String }
impl Drop for Res {
    fn drop(mut ref self) {
        println(f"drop {self.id} {self.name}")
    }
}
enum SBox { Full(Res), Empty }
enum Loud { Hold(Res), Quiet }
impl Drop for Loud {
    fn drop(mut ref self) {
        println("loud drop")
    }
}
fn mk(n: i64) -> SBox {
    return SBox.Full(Res { id: n, name: f"s{n}" });
}
fn mk_loud(n: i64) -> Loud {
    return Loud.Hold(Res { id: n, name: f"l{n}" });
}
fn main() {
    let mut a = mk(1);
    a = mk(2);
    let mut b = mk_loud(3);
    b = mk_loud(4);
    println("end");
}
"#,
        &[
            "drop 1 s1",
            "drop 2 s2",
            "loud drop",
            "drop 3 l3",
            "loud drop",
            "drop 4 l4",
            "end",
        ],
        "enum_reassign_displaced_payload_heap_freed",
    );
}

/// B-2026-08-01-14 — a fresh enum-ctor arg to a PASSTHROUGH callee
/// (`pass2(E2.B(Res { .. }))` where the callee returns its param): the
/// callee entry-copies the payload and returns the COPY, so the
/// ORIGINAL aggregate was orphaned — one payload buffer lost per call
/// (the enum sibling of B-2026-07-08-6's struct entry-copy premise
/// break). The caller now frees the original, memory only. LSan gates
/// the leak; ASAN guards the eager free against the copy the result
/// binding owns.
#[test]
fn asan_enum_ctor_arg_passthrough_original_freed() {
    assert_clean_asan_run(
        r#"
struct Res { id: i64, name: String }
impl Drop for Res {
    fn drop(mut ref self) { println(f"drop {self.id} {self.name}") }
}
enum E2 { B(Res), Empty }
fn pass2(w: E2) -> E2 { println("passing"); return w; }
fn main() {
    println("a");
    let f = pass2(E2.B(Res { id: 8, name: f"x{8}" }));
    println("mid");
    println("end");
}
"#,
        &["a", "passing", "drop 8 x8", "mid", "end"],
        "enum_ctor_arg_passthrough_original_freed",
    );
}

#[test]
fn asan_letelse_freshtemp_enum_bound_field_no_double_free() {
    // let-else surface, bound field: `let Full(v, n) = make() else { … }`.
    // The escaped `v` binding frees the Vec; the materialized temp's
    // EnumDrop (drained at enclosing-scope exit) must skip it. macOS
    // double-free gate for the let-else suppression edge.
    let src = format!(
            "{B_ASAN_PRELUDE}\nfn count() -> i64 {{\n    let Holder.Full(v, n) = make() else {{ return 0 }}\n    return v.len() + n\n}}\nfn main() {{\n    let mut i = 0;\n    while i < 8 {{\n        println(count());\n        i = i + 1;\n    }}\n    println(i);\n}}\n"
        );
    assert_clean_asan_run(
        &src,
        &["44", "44", "44", "44", "44", "44", "44", "44", "8"],
        "letelse_freshtemp_enum_bound_field_no_double_free",
    );
}

#[test]
fn asan_enum_nested_struct_payload_inplace_drop_no_leak() {
    // B-2026-06-13-13 part 1: an enum variant whose payload is a nested
    // non-shared user struct that carries heap (`Wrap(Inner { data: Vec, … })`
    // — the lexer's `CStringLiteral(CStr { bytes: Vec[u8], … })` shape). The
    // enum drop now recurses into the nested struct's `__karac_drop_struct_<S>`
    // (it previously classified the payload `None` and leaked the inner Vec).
    // Exercises the WHOLE-VALUE in-place drop path — the one the lexer hits
    // when it drops a `Vec[SpannedToken]` wholesale — via a non-consuming
    // wildcard match so the enum is dropped, not moved out. On Linux CI this
    // faults under LeakSanitizer if the nested-struct drop regresses; on macOS
    // (no LSan) it is the double-free gate — the deep-copy-on-entry keeps the
    // callee copy and caller original independent, so re-dropping would fault.
    assert_clean_asan_run(
        r#"
struct Inner { data: Vec[i64], tag: i64 }
enum E { Wrap(Inner), Empty }
fn mkvec(x: i64) -> Vec[i64] {
    let mut v: Vec[i64] = Vec.new();
    v.push(x);
    v.push(x + 1);
    return v;
}
fn main() {
    let mut sum: i64 = 0;
    let mut i = 0;
    while i < 50 {
        let e = E.Wrap(Inner { data: mkvec(i), tag: i });
        let k = match e {
            Wrap(_) => 1,
            Empty => 0,
        };
        sum = sum + k;
        i = i + 1;
    }
    println(sum);
}
"#,
        &["50"],
        "enum_nested_struct_payload_inplace_drop_no_leak",
    );
}

#[test]
fn asan_enum_nested_struct_payload_moved_out_no_leak_no_double_free() {
    // B-2026-06-13-13 residual A: a nested-struct enum payload MOVED OUT of
    // the enum — bound by a `match` (`Wrap(inner)`), passed by value into a
    // fn that binds it out, returned as the arm tail, and re-used after a
    // consuming call. Each path now registers the moved-out struct binding
    // for `StructDrop` (pattern_binding.rs), kept symmetric with the source
    // move-suppression so it frees exactly once. On Linux CI this faults
    // under LeakSanitizer if the binding drop regresses (a leak); on macOS it
    // is the double-free gate — the copy-supported deep-copy keeps the caller
    // original valid after the callee frees its copy (the `sink+reuse` arm),
    // so a missed-suppression double-free or a stale-alias use-after-free
    // faults here. `Inner` is copy-supported (Vec + i64), so the binding IS
    // tracked; an `Option`/`Result` or Map-bearing payload is excluded
    // (covered by `asan_freshtemp_boxed_option_match_move_out_no_double_free`).
    assert_clean_asan_run(
        r#"
struct Inner { data: Vec[i64], n: i64 }
enum E { Wrap(Inner), Empty }
fn mk(x: i64) -> Vec[i64] {
    let mut v: Vec[i64] = Vec.new();
    v.push(x);
    v.push(x + 1);
    return v;
}
fn sink(e: E) -> i64 {
    match e {
        Wrap(inner) => inner.n,
        Empty => 0,
    }
}
fn unwrap_or(e: E) -> Inner {
    match e {
        Wrap(inner) => inner,
        Empty => Inner { data: mk(0), n: 0 },
    }
}
fn main() {
    let mut t: i64 = 0;
    let mut i = 0;
    while i < 20 {
        // match-bind move-out into a local, then consume
        let e1 = E.Wrap(Inner { data: mk(i), n: 1 });
        let r1 = match e1 {
            Wrap(inner) => inner.data.len() + inner.n,
            Empty => 0,
        };
        // by-value pass into a fn that binds the payload out
        let e2 = E.Wrap(Inner { data: mk(i), n: 2 });
        let r2 = sink(e2);
        // tail-return the moved-out struct binding
        let e3 = E.Wrap(Inner { data: mk(i), n: 3 });
        let got = unwrap_or(e3);
        // consuming call, then re-use the (copy-supported, callee-owned) original
        let e4 = E.Wrap(Inner { data: mk(i), n: 4 });
        let a = sink(e4);
        let b = match e4 {
            Wrap(inner) => inner.n,
            Empty => 0,
        };
        t = t + r1 + r2 + got.n + a + b;
        i = i + 1;
    }
    println(t);
}
"#,
        &["320"],
        "enum_nested_struct_payload_moved_out_no_leak_no_double_free",
    );
}

#[test]
fn asan_user_enum_field_in_struct_heap_payload() {
    // Memory-safety companion to the `enum-in-struct-field` codegen
    // blocker fix (two-pass struct declaration). A struct field whose
    // type is a user enum with a HEAP (String) payload, held in a Vec of
    // such structs, matched + read, then dropped at scope exit. The fix
    // makes the field lower at the enum's real tagged-union shape (not
    // the i64 fall-through); this guards that the heap payload inside the
    // enum inside the struct inside the Vec is freed exactly once — no
    // UAF on the read, no double-free at scope exit. Non-foldable
    // (loop-index) strings so the buffers are real heap allocations.
    assert_clean_asan_run(
        r#"
enum Token { Ident(String), Eof }
struct Spanned { start: i64, tok: Token }
fn main() {
    let mut toks: Vec[Spanned] = Vec.new();
    let mut i = 0i64;
    while i < 3i64 {
        toks.push(Spanned { start: i, tok: Token.Ident(f"id-{i}") });
        i = i + 1;
    }
    let mut j = 0;
    while j < toks.len() {
        match toks[j].tok {
            Ident(name) => println(name),
            Eof => println("eof"),
        }
        j = j + 1;
    }
}
"#,
        &["id-0", "id-1", "id-2"],
        "user_enum_field_in_struct_heap_payload",
    );
}

#[test]
fn asan_struct_wrapped_enum_payload_rc_children_freed_no_leak() {
    // B-2026-06-14-28 (leak side) — when a `shared enum`'s variant payload
    // is a plain `struct` that owns `shared` fields (`Add(BinOp)` +
    // `struct BinOp { left: Expr, right: Expr }`), the inline RC children
    // must be rc-dec'd when the enum box is freed. Pre-fix, the
    // shared-enum-box RC drop walker (`emit_shared_enum_rc_drop_fn`)
    // classified the struct payload non-walkable (the value-path struct
    // drop `__karac_drop_struct_<S>` has no shared-field arm; a local
    // binding's shared fields are dec'd by its let cleanup, which an enum
    // payload has not) — so every inline `Expr` child leaked (~192 B /
    // tree on macOS `leaks`, silent under mac ASAN; the Linux-CI LSan job
    // is the gate). Looped to make the per-iteration leak visible.
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
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 60 {
        let t: Expr = Neg(Unary { operand: Add(BinOp { left: Num(i), right: Num(2) }) });
        total = total + eval(t);
        i = i + 1;
    }
    println(total);
}
"#,
        &["-1890"],
        "struct_wrapped_enum_payload_rc_children_freed",
    );
}

#[test]
fn asan_shared_enum_struct_variant_whole_binding_readonly_no_leak() {
    // B-2026-06-19-4 (the ledger's EXACT repro): a `shared enum E` whose
    // struct-variant payload owns a String (`Ident(Id { name: String })`),
    // bound WHOLE as `n` in a match arm that only READS it (`n.name.len()`)
    // — NOT consumed, and NOT a child of a recursive box. This is the
    // single-level direct case `render(make())` the sibling recursive test
    // (`..._recursive_struct_payload_string_freed`) does not exercise: there
    // the arm CONSUMES via `push_str(n.name)` and the leaf is a CHILD of a
    // Binary box. Here `n` is a shallow by-value VIEW of the still-live RC
    // box's inline payload; `bind_pattern_values` deliberately does NOT
    // `track_struct_var` it for a shared-enum scrutinee (pattern_binding.rs,
    // `!pattern_binding_scrutinee_is_shared_enum`), so the box's rc-drop
    // walker is the SOLE owner of `name`'s buffer. The box-walk frees it
    // (8a78ee6d / phase-12 #41: `type_expr_has_drop_heap` now flags a
    // String-owning plain-struct payload walkable). Exactly one free → no
    // leak (this test) and no double-free (the read-only binding never
    // frees). ≥36 B name so the leak is unambiguous under LSan.
    assert_clean_asan_run(
        r#"
struct Id { name: String, off: i64 }
shared enum E { Ident(Id) }
fn render(e: E) -> i64 {
    match e {
        Ident(n) => { n.name.len() }
    }
}
fn make() -> E {
    E.Ident(Id { name: "an_identifier_long_enough_to_force_heap".to_string(), off: 0 })
}
fn main() {
    let mut total: i64 = 0;
    let mut i: i64 = 0;
    while i < 20 {
        total = total + render(make());
        i = i + 1;
    }
    println(total);
}
"#,
        &["780"],
        "shared_enum_struct_variant_whole_binding_readonly",
    );
}

#[test]
fn asan_shared_enum_struct_payload_child_moveout_no_double_free() {
    // B-2026-07-09-12 (copy-supported half): a for-loop over a `Vec[shared
    // enum]` whose arm DESTRUCTURES the struct payload and MOVES a heap child
    // OUT of it (`let Id { name, .. } = n; name` returns the extracted
    // String). The reconstructed struct payload `n` is a by-value VIEW of the
    // RC box's inline buffer; without the fix the returned `name` and the
    // box's rc-drop both free the same String buffer — a double-free (the
    // minimal parser-runtime repro). The fix upgrades the view to an OWNED
    // deep clone at the shared-enum match bind, gated on
    // `struct_clone_fully_duplicates` (payload heap is String / Vec[non-
    // shared] / nested such — reproduced exactly by `emit_struct_clone_fn`).
    // Second variant exercises the Vec[non-shared] clone leg (`Row { cells:
    // Vec[i64] }`, sum a moved-out Vec). Looped so a per-iteration double-free
    // or leak is visible to the Linux-CI LSan gate.
    assert_clean_asan_run(
        r#"
struct Id { name: String, span: i64 }
struct Row { first: String, cells: Vec[i64] }
shared enum Node { Ident(Id), Rowed(Row) }
fn render_ident(e: Node) -> String {
    match e {
        Ident(n) => { let Id { name, span } = n; name }
        Rowed(r) => { let Row { first, cells } = r; first }
    }
}
fn sum_row(e: Node) -> i64 {
    match e {
        Ident(_) => 0,
        Rowed(r) => {
            let Row { first, cells } = r;
            let mut acc: i64 = 0;
            for c in cells { acc = acc + c; }
            acc
        }
    }
}
fn main() {
    let mut total: i64 = 0;
    let mut last = "".to_string();
    let mut k: i64 = 0;
    while k < 20 {
        let mut out = "".to_string();
        let mut v: Vec[Node] = Vec.new();
        v.push(Node.Ident(Id { name: "hi".to_string(), span: 1 }));
        let mut cs: Vec[i64] = Vec.new();
        cs.push(3); cs.push(4);
        v.push(Node.Rowed(Row { first: "row".to_string(), cells: cs }));
        for e in v { out.push_str(render_ident(e)); }
        let mut v2: Vec[Node] = Vec.new();
        let mut cs2: Vec[i64] = Vec.new();
        cs2.push(5); cs2.push(6); cs2.push(7);
        v2.push(Node.Rowed(Row { first: "r2".to_string(), cells: cs2 }));
        for e in v2 { total = total + sum_row(e); }
        last = out;
        k = k + 1;
    }
    println(last);
    println(total);
}
"#,
        &["hirow", "360"],
        "shared_enum_struct_payload_child_moveout_no_double_free",
    );
}

/// B-2026-09-07-55 — a nested struct read out of a by-value VIEW of a
/// shared enum's boxed payload is a COPY, so its shared children each get a
/// ref of their own.
///
/// `nd` is a view: the match binds the box's inline `IfNode` by value into a
/// local alloca. `let tb = nd.then_block` then hands `tb` the SAME
/// `Option[shared]` handle the box holds, with no inc.
/// `suppress_struct_field_move_by_name` does fire for this shape — measured,
/// `view=true` — and cannot help: it GEPs the VIEW's private alloca while
/// the box's `emit_nested_struct_shared_rc_decs_ex` walk reads the BOX, so
/// the neutralization lands where nothing looks. Both sides then dec one
/// ref and the second dec reads the block the first freed.
///
/// WHY ONLY THE INSTRUMENTED LEG SEES IT. The count never reaches zero
/// twice, so there is no double free and no leak — the fixture this row was
/// filed against is even named `no_double_free` and is correct about that.
/// It is a READ of freed memory, which is not an allocator event
/// (B-2026-09-07-40). Measured on the parent: green on the default and `-O0`
/// legs, `heap-use-after-free ... in __karac_rc_drop_E` under
/// `KARAC_SANITIZE_ADDRESS=1`. valgrind at `-O0` also reports it
/// (`Invalid read of size 8`), which is what made the reduction iterable.
///
/// THE BUFFER IS DELIBERATELY NOT COPIED and this fixture is where that
/// stays honest. The box's walker runs with `nested_buffer_free =
/// Some(false)` and never frees a nested struct's `Vec`/`String`, so the
/// moved-out leaf is their only owner — one free, correct. Case (b) is that
/// control: the same move with NO shared grandchild is clean before the fix
/// and after it, so a future change that starts deep-copying buffers here
/// (stranding the box's originals) fails it. The mirror-image defect — the
/// walker freeing nothing when the leaf is NEVER moved out — is a leak, is
/// B-2026-09-07-62, and is deliberately still open: this fix does not touch
/// it and its 32 B cells measure identically before and after.
///
/// SCOPE OF THE NEW WALKER, stated plainly. It mirrors
/// `emit_nested_struct_shared_rc_decs_ex`'s two arms so the inc and the dec
/// cannot classify a field differently, but only the `Option[shared]` arm
/// has a cell that is RED on the parent. A nested struct with a DIRECT
/// `shared` field measures clean on the parent as well as on the fix, so it
/// is not pinned here — the arm exists for symmetry with the dec walker, not
/// because a known defect needs it.
#[test]
fn asan_shared_enum_view_field_move_incs_the_leaf_shared_children() {
    // (a) The defect: nested struct moved out of a view, carrying an
    // `Option[shared]` grandchild that the box's walk also decs.
    assert_clean_asan_run(
        r#"
struct Span { a: i64, b: i64, c: i64, d: i64 }
shared enum E { Lit(i64), Iff(IfNode), Blk(Block) }
struct Block { stmts: Vec[i64], tail: Option[E], span: Span }
struct IfNode { cond: E, then_block: Block, span: Span }
fn mk_block(first: i64, sp: i64) -> Block {
    let mut s: Vec[i64] = Vec.new();
    s.push(first); s.push(first + 1);
    Block { stmts: s, tail: Some(E.Lit(99)), span: Span { a: sp, b: 0, c: 0, d: 0 } }
}
fn main() {
    let ife = E.Iff(IfNode { cond: E.Lit(7), then_block: mk_block(20, 2), span: Span { a: 5, b: 0, c: 0, d: 0 } });
    match ife {
        Lit(n) => println(n),
        Iff(nd) => { println(nd.span.a); let tb = nd.then_block; println(tb.span.a); println(tb.stmts[0]); }
        Blk(_) => println(-9)
    }
}
"#,
        &["5", "2", "20"],
        "b0907-55-view-field-move-option-shared-child",
    );
    // (b) The buffer control: the same move with NO shared grandchild. Clean
    // before the fix and after — it fails if the leaf ever starts
    // deep-copying the nested buffer the box does not free.
    assert_clean_asan_run(
        r#"
struct Span { a: i64, b: i64, c: i64, d: i64 }
shared enum E { Lit(i64), Iff(IfNode), Blk(Block) }
struct Block { stmts: Vec[i64], tail: Option[E], span: Span }
struct IfNode { cond: E, then_block: Block, span: Span }
fn mk_block_novec(first: i64, sp: i64) -> Block {
    let mut s: Vec[i64] = Vec.new();
    s.push(first);
    Block { stmts: s, tail: None, span: Span { a: sp, b: 0, c: 0, d: 0 } }
}
fn main() {
    let ife = E.Iff(IfNode { cond: E.Lit(7), then_block: mk_block_novec(20, 2), span: Span { a: 5, b: 0, c: 0, d: 0 } });
    match ife {
        Lit(n) => println(n),
        Iff(nd) => { println(nd.span.a); let tb = nd.then_block; println(tb.span.a); println(tb.stmts[0]); }
        Blk(_) => println(-9)
    }
}
"#,
        &["5", "2", "20"],
        "b0907-55-view-field-move-no-shared-child-control",
    );
}

#[test]
fn asan_shared_enum_boxed_struct_payload_moveout_no_double_free() {
    // B-2026-06-20: a shared-enum variant whose struct payload is heap-BOXED
    // (`Blk(Block)` / `Iff(IfNode)` — wider than the payload area). The box
    // rc-drop must unbox + free the box AND reclaim its DIRECT Vec/String
    // buffers and shared/`Option[shared]` children (the tests-1/2 leak), but
    // must NOT recurse into a nested heap struct field that the match moved
    // out (`let tb = nd.then_block`, freed by `tb`'s own drop) — re-freeing it
    // double-frees. Pins the build + match + scope-exit drop of both the
    // direct boxed payload and the nested (Iff) one with no double-free and no
    // leak. The IR sibling of `test_e2e_shared_enum_payload_with_nested_heap_
    // struct_field` (codegen.rs), under the LSan + ASAN gate.
    assert_clean_asan_run(
            "struct Span { a: i64, b: i64, c: i64, d: i64 }\n\
             shared enum E { Lit(i64), Iff(IfNode), Blk(Block) }\n\
             struct Block { stmts: Vec[i64], tail: Option[E], span: Span }\n\
             struct IfNode { cond: E, then_block: Block, span: Span }\n\
             fn mk_block(first: i64, sp: i64) -> Block {\n\
             \x20   let mut s: Vec[i64] = Vec.new();\n\
             \x20   s.push(first); s.push(first + 1);\n\
             \x20   Block { stmts: s, tail: Some(E.Lit(99)), span: Span { a: sp, b: 0, c: 0, d: 0 } }\n\
             }\n\
             fn main() {\n\
             \x20   let be = E.Blk(mk_block(10, 1));\n\
             \x20   match be {\n\
             \x20       Lit(n) => println(n),\n\
             \x20       Iff(nd) => println(nd.span.a),\n\
             \x20       Blk(b) => {\n\
             \x20           println(b.span.a);\n\
             \x20           println(b.stmts[0]);\n\
             \x20           println(b.stmts[1]);\n\
             \x20           match b.tail { Some(t) => match t { Lit(v) => println(v), Iff(_) => println(-1), Blk(_) => println(-2) }, None => println(-3) }\n\
             \x20       }\n\
             \x20   }\n\
             \x20   let ife = E.Iff(IfNode { cond: E.Lit(7), then_block: mk_block(20, 2), span: Span { a: 5, b: 0, c: 0, d: 0 } });\n\
             \x20   match ife {\n\
             \x20       Lit(n) => println(n),\n\
             \x20       Iff(nd) => {\n\
             \x20           println(nd.span.a);\n\
             \x20           let tb = nd.then_block;\n\
             \x20           println(tb.span.a);\n\
             \x20           println(tb.stmts[0]);\n\
             \x20       }\n\
             \x20       Blk(_) => println(-9)\n\
             \x20   }\n\
             }",
            &["1", "10", "11", "99", "5", "2", "20"],
            "shared_enum_boxed_struct_payload_moveout_no_double_free",
        );
}

#[test]
fn asan_forloop_bare_enum_element_whole_move_no_double_free() {
    // B-2026-07-05-2: the residual B-2026-07-04-17 left open — a BARE
    // `Vec[<user enum>]` element moved WHOLE to a new owner. `x` aliases the
    // container's live-variant payload; without the enum-let-binding
    // deep-copy, `x`'s EnumDrop and the container's per-element drain free
    // the same `String` buffer (double-free). Repeated across a
    // heap-payload variant and a scalar variant so both the copied and the
    // no-op paths run.
    assert_clean_asan_run(
        r#"
enum E { Tag(String), Num(i64) }
fn build() -> Vec[E] {
    let mut v: Vec[E] = Vec.new();
    let mut i = 0;
    while i < 6 {
        if i % 2 == 0 {
            v.push(E.Tag("bare_enum_variant_heap_payload_omicron_field".to_string()));
        } else {
            v.push(E.Num(i));
        }
        i = i + 1;
    }
    v
}
fn main() {
    let items = build();
    let mut n: i64 = 0;
    for a in items {
        let x = a;
        match x {
            E.Tag(s) => { n = n + s.len(); }
            E.Num(k) => { n = n + k; }
        }
    }
    println(n);
}
"#,
        &["141"], // 3 * 44 (Tag payload len) + (1 + 3 + 5) Num
        "forloop_bare_enum_element_whole_move_no_double_free",
    );
}

#[test]
fn asan_forloop_enum_element_nested_struct_payload_no_double_free() {
    // B-2026-07-05-2, the NestedStruct-payload variant: an enum whose live
    // variant carries a heap-bearing struct inline (`Wrap(Inner)`). Exercises
    // the `NestedStruct` arm of `deep_copy_enum_heap_payload_in_place` via the
    // for-loop-element path — a distinct branch from the sibling
    // `asan_forloop_bare_enum_element_whole_move_no_double_free`, which only
    // covers a `VecOrString` payload. The deep-copy must recurse into the
    // inline struct's own heap fields (copy-depth == drop-depth) so the inner
    // String does not double-free on a whole-element move.
    assert_clean_asan_run(
        r#"
struct Inner { s: String }
enum Node { Leaf, Wrap(Inner) }
fn build() -> Vec[Node] {
    let mut v: Vec[Node] = Vec.new();
    let mut i = 0;
    while i < 5 { v.push(Node.Wrap(Inner { s: "forloop_enum_nested_struct_payload_iota_field_yy".to_string() })); i = i + 1; }
    v
}
fn main() {
    let items = build();
    let mut n: i64 = 0;
    for a in items {
        let x = a;
        match x { Node.Wrap(inner) => { n = n + inner.s.len(); } Node.Leaf => {} }
    }
    println(n);
}
"#,
        &["240"], // 5 * len("forloop_enum_nested_struct_payload_iota_field_yy") = 5 * 48
        "forloop_enum_element_nested_struct_payload_no_double_free",
    );
}

#[test]
fn asan_recursive_shared_enum_arg_no_leak() {
    // B-2026-07-12-25: a freshly-constructed `shared enum` value passed by
    // value into a recursive self-call leaked the whole RC chain at ODD
    // constructor-nesting depth. `fresh_arg_bare_shared_heap_type`'s
    // passthrough self-exclusion (correct for a `g(make())` function chain)
    // recursed through the constructor's payload arg and flipped Some/None
    // per level, so the caller-side RC-dec was registered only at even
    // depth; odd depths (`Node(Leaf)`, `Node(Node(Node(Leaf)))`) registered
    // nothing and leaked every node. Fix: skip the guard for a variant
    // constructor, which owns its payload via its recursive drop. Uses an
    // ODD (depth-3) chain — the leaking case pre-fix — looped so LSan sees
    // the per-iteration leak.
    assert_clean_asan_run(
        r#"
shared enum E { Leaf(i64), Node(E) }
fn chk(e: E) -> i64 {
    match e {
        Leaf(n) => n,
        Node(x) => chk(x)
    }
}
fn main() {
    let mut i: i64 = 0;
    let mut t: i64 = 0;
    while i < 200 {
        t = t + chk(Node(Node(Node(Leaf(1)))));
        i = i + 1;
    }
    println(f"{t}");
}
"#,
        &["200"],
        "recursive_shared_enum_arg_no_leak",
    );
}

#[test]
fn asan_rc_elide_consumed_payload_projection_caller_no_double_free() {
    // Residual shape, direct consume: `match p { Some(n) => sink(n) }` moves
    // payload `n` by value into owned `sink`. Condition 4 declines to elide
    // `probe`, so it runs balanced. Alternating idx over a 2-node pool, 200
    // reps: 100*5 + 100*9.
    assert_clean_asan_run(
        r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn sink(x: Node) -> i64 { x.val }
fn probe(p: Option[Node]) -> i64 { match p { None => 0i64, Some(n) => sink(n) } }
fn main() {
    let mut pool: Vec[Option[Node]] = Vec.new();
    pool.push(Some(Node { val: 5i64, left: None, right: None }));
    pool.push(Some(Node { val: 9i64, left: None, right: None }));
    let mut t: i64 = 0i64;
    let mut rep: i64 = 0i64;
    while rep < 200i64 { let idx = rep % 2i64; t = t + probe(pool[idx].clone()); rep = rep + 1i64; }
    println(f"{t}")
}
"#,
        &["1400"],
        "rc_elide_consumed_payload_projection_caller_no_double_free",
    );
}

#[test]
fn asan_rc_elide_forwarded_payload_no_double_free() {
    // Residual shape, two-level consume chain: payload `n` forwarded through
    // `forward` into `sink`. Condition 4 declines to elide `probe2` (payload
    // moved out), so it runs on the balanced-RC path. Prints 1400.
    assert_clean_asan_run(
        r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn sink(x: Node) -> i64 { x.val }
fn forward(y: Node) -> i64 { sink(y) }
fn probe2(p: Option[Node]) -> i64 { match p { None => 0i64, Some(n) => forward(n) } }
fn main() {
    let mut pool: Vec[Option[Node]] = Vec.new();
    pool.push(Some(Node { val: 5i64, left: None, right: None }));
    pool.push(Some(Node { val: 9i64, left: None, right: None }));
    let mut t: i64 = 0i64;
    let mut rep: i64 = 0i64;
    while rep < 200i64 { let idx = rep % 2i64; t = t + probe2(pool[idx].clone()); rep = rep + 1i64; }
    println(f"{t}")
}
"#,
        &["1400"],
        "rc_elide_forwarded_payload_no_double_free",
    );
}

/// B-2026-08-07-1 — a BOXED-payload enum binding that ESCAPES its frame by
/// being returned. The frame frees the box on the way out; the caller then
/// reads and frees the same pointer. A double free plus invalid reads, at
/// BOTH opt levels — memory corruption, not a leak.
///
/// `Wide` is 5 LLVM words against `Option`'s 3-word area, so the payload is
/// heap-boxed and the let site queues a `BoxedEnumDrop`. Returning the
/// binding hands the caller an enum whose word 0 still points at the callee's
/// box. `suppress_cleanup_for_tail_return` retracts a returned binding's Vec
/// / String / Map / channel / user-Drop cleanup; `BoxedEnumDrop` post-dates
/// most of that list and was never added to it.
///
/// THE DISARM IS A RUNTIME WORD-0 ZERO, not a retraction of the queued
/// action, and that choice is what most of these arms exist to pin. A
/// binding can be returned on ONE path and consumed on another; a
/// compile-time retract is flow-insensitive, so it would strand the box on
/// the consuming path — trading the double free for a leak. Zeroing stores
/// only on the path that actually returns, and the box drop's existing
/// null-guard then skips. Both directions are therefore covered rather than
/// traded.
///
/// FOUR distinct escape positions, because each is a separate walk and none
/// sees the others' leaves — this is the part a narrower reading misses:
///
///   * the function-body TAIL (`{ let b = …; b }`);
///   * an explicit `return b;` NESTED IN AN `if`, which is not the body's
///     tail, so the tail walk never sees it;
///   * an `if`/`else` BRANCH LEAF (`if hit { b } else { Option.None }`),
///     suppressed in the branch's own block before its frame drains;
///   * a MATCH-ARM leaf (`match k { 0 => b, _ => Option.None }`), likewise
///     per-arm.
///
/// The over-suppression direction is guarded just as hard, since a disarm
/// that fires too widely turns a working free into a leak:
///
///   * `never_ret` — a binding that is NOT returned must still free its box;
///   * `two_bind` — two boxed bindings, only one returned, so the other's
///     free must still fire from the same frame;
///   * `else_taken` and `match_tail` — both branches genuinely run (the
///     scrutinee alternates), so the non-returning path must free while the
///     returning path must not. A flow-insensitive disarm leaks here.
///   * `split` — returns on one path, CONSUMES on the other.
///   * the BLOCK tail (`let x = { let bb = …; bb };`) is not a return at
///     all: the consumer owns the box and the block frame must not free it,
///     but nothing may disarm the consumer either.
///   * `fresh_ret` — a returned fresh temp has no binding, so there is
///     nothing to disarm and the arm proves the walk does not invent one.
///
/// Also covers `Result` (a 6-word payload against its 5-word area — the
/// obvious `Wide` probe does NOT box there, since 5 is not > 5, and an
/// early version of this fixture was green for exactly that wrong reason)
/// and a HEAP-BEARING interior, where the box drop carries an
/// `inner_drop_fn` and the returned value must keep its String intact.
///
/// COVERAGE: unlike its nested sibling this does NOT depend on the `-O0`
/// leg. The pre-fix compiler ABORTS at the default `-O2` as well, so the
/// default suite run is a real gate here; the `-O0` leg re-runs it for the
/// paths where a box survives optimization.
///
/// The expected value is COMPUTED, not read off a run: every payload is
/// seeded from the opaque `env.args().len()`, all eleven arms subtract the
/// seed back out to leave `i` (both branches of each two-path arm included,
/// which is why the `None` arms return `i` rather than a sentinel), and the
/// String arm adds 1 for a successful byte read — `11i + 1` per iteration,
/// so `11 * (0+…+39) + 40 = 8620`.
#[test]
fn asan_returned_boxed_payload_binding_not_freed_by_its_own_frame() {
    assert_clean_asan_run_min_allocs(
        r#"
struct Wide { a: i64, b: i64, c: i64, d: i64, e: i64 }
struct Wide6 { a: i64, b: i64, c: i64, d: i64, e: i64, f: i64 }
struct WideS { a: i64, b: i64, c: i64, d: i64, s: String }

fn tail_ret(k: i64) -> Option[Wide] {
    let b: Option[Wide] = Option.Some(Wide { a: k, b: 1, c: 2, d: 3, e: 4 });
    b
}
fn early_ret(k: i64) -> Option[Wide] {
    let b: Option[Wide] = Option.Some(Wide { a: k, b: 1, c: 2, d: 3, e: 4 });
    if k >= 0 { return b; }
    Option.None
}
fn fresh_ret(k: i64) -> Option[Wide] { Option.Some(Wide { a: k, b: 1, c: 2, d: 3, e: 4 }) }
fn never_ret(k: i64) -> i64 {
    let b: Option[Wide] = Option.Some(Wide { a: k, b: 1, c: 2, d: 3, e: 4 });
    match b { Option.Some(w) => w.a, Option.None => -1 }
}
fn two_bind(k: i64) -> Option[Wide] {
    let dead: Option[Wide] = Option.Some(Wide { a: 99, b: 1, c: 2, d: 3, e: 4 });
    let live: Option[Wide] = Option.Some(Wide { a: k, b: 1, c: 2, d: 3, e: 4 });
    let x = match dead { Option.Some(w) => w.a, Option.None => 0 };
    if x == 99 { live } else { Option.None }
}
fn split(k: i64) -> Option[Wide] {
    let b: Option[Wide] = Option.Some(Wide { a: k, b: 1, c: 2, d: 3, e: 4 });
    if k % 2 == 0 { return b; }
    let c = match b { Option.Some(w) => w.a, Option.None => -1 };
    Option.Some(Wide { a: c, b: 1, c: 2, d: 3, e: 4 })
}
fn res_ret(k: i64) -> Result[Wide6, i64] {
    let b: Result[Wide6, i64] = Result.Ok(Wide6 { a: k, b: 1, c: 2, d: 3, e: 4, f: 5 });
    b
}
fn heap_ret(k: i64) -> Option[WideS] {
    let b: Option[WideS] = Option.Some(WideS { a: k, b: 1, c: 2, d: 3, s: "x" + k.to_string() });
    b
}
fn match_tail(k: i64) -> Option[Wide] {
    let b: Option[Wide] = Option.Some(Wide { a: k, b: 1, c: 2, d: 3, e: 4 });
    match k % 2 { 0 => b, _ => Option.None }
}
fn else_taken(k: i64) -> Option[Wide] {
    let b: Option[Wide] = Option.Some(Wide { a: k, b: 1, c: 2, d: 3, e: 4 });
    if k % 2 == 0 { b } else { Option.None }
}

fn main() {
    let n = env.args().len() as i64;
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 40 {
        let k = n + i;
        acc = acc + match tail_ret(k)  { Option.Some(w) => w.a - n, Option.None => -1 };
        acc = acc + match early_ret(k) { Option.Some(w) => w.a - n, Option.None => -1 };
        acc = acc + match fresh_ret(k) { Option.Some(w) => w.a - n, Option.None => -1 };
        acc = acc + never_ret(k) - n;
        acc = acc + match two_bind(k)  { Option.Some(w) => w.a - n, Option.None => -1 };
        acc = acc + match split(k)     { Option.Some(w) => w.a - n, Option.None => -1 };
        acc = acc + match res_ret(k)   { Result.Ok(w) => w.a - n, Result.Err(e) => e };
        acc = acc + match heap_ret(k)  { Option.Some(w) => w.a - n + (if w.s.len() > 0 { 1 } else { 0 }), Option.None => -1 };
        acc = acc + match match_tail(k) { Option.Some(w) => w.a - n, Option.None => i };
        acc = acc + match else_taken(k) { Option.Some(w) => w.a - n, Option.None => i };
        let x: Option[Wide] = { let bb: Option[Wide] = Option.Some(Wide { a: k, b: 1, c: 2, d: 3, e: 4 }); bb };
        acc = acc + match x { Option.Some(w) => w.a - n, Option.None => -1 };
        i = i + 1;
    }
    println(acc);
}
"#,
        &["8620"],
        "returned_boxed_payload_binding_escape",
        60,
    );
}

/// B-2026-08-07-4 — reassigning a `let mut` binding whose enum payload is
/// heap-BOXED orphaned the OVERWRITTEN value's box.
///
/// Every registration in this family is keyed to a SLOT and fires once, at
/// scope exit, so it frees whatever the slot holds LAST. A slot that holds
/// N values over its life needs N frees and no scope-exit action can supply
/// the other N-1 — by then the displaced pointers are gone. The free has to
/// happen at the STORE, which is a different site from every other fix in
/// this family.
///
/// Measured 32 B per overwrite at `-O0`, LINEAR in stores (a
/// three-store binding leaks two boxes), and identical for the DIRECT and
/// NESTED actions — which is why the row's original filing as a
/// nested-only shape was wrong. A boxed STRUCT payload leaks its whole box
/// too (40 B for a 5-field one), a shape the row did not list at all.
///
/// THE FIX EMITS THE BINDING'S OWN QUEUED CLEANUP ACTION before the store
/// rather than a hand-rolled free, and that is what makes the ownership
/// question answer itself: THE QUEUE IS THE ARMED SET. A binding whose
/// registration was retracted — moved into a callee, returned, aliased by a
/// passthrough — has no action to find, so nothing is emitted and the
/// consumer that took the box over is not double-freed. The row's central
/// caution ("the store must consult the same armed set the scope-exit
/// action does or it will double-free") is therefore satisfied
/// structurally, not by a second predicate that could drift from the first.
///
/// The over-freeing direction is what most of these arms are for, since
/// this fix ADDS a free and every one of them could be one too many:
///
///   * `a6 = a6` — a self-alias is CLEAN today; freeing first would hand
///     the store a pointer it had just released. Guarded, and the arm is
///     here so the guard cannot be dropped silently.
///   * `a7` — MOVED into a callee, then reassigned. The callee owns that
///     box; a store-site free that ignored the armed set double-frees here.
///   * `a9 = bump(a9)` — the RHS mentions the target, so the old value may
///     have moved into the callee. Skipped ⇒ still leaks in the general
///     case, which is the safe direction; this shape happens to be clean
///     because `bump` consumes its argument.
///   * `a8 = Option.None` then back — the tag guard must skip a payload-
///     absent slot rather than free word 0 of a `None`.
///   * `a5` — the store is INSIDE an `if`, so only executed stores may
///     free (pre-fix this leaked 160 B / 5 over ten iterations, not 320 /
///     10). Both branches produce the same value so the arm's contribution
///     is uniform.
///   * `outer` — declared OUTSIDE the loop and assigned inside, so the
///     binding's action lives in a frame below the current one. Pins the
///     all-frames scan; a top-frame-only search finds nothing and leaks.
///   * a `Vec` and a `String` reassignment, which were already correct
///     before this change (their store sites have freed eagerly for a long
///     time) and must stay that way — they are the reason this was
///     diagnosable as a missing arm rather than an unsolved problem.
///
/// NOT COVERED: `b = c` between two boxed bindings. That shape ALREADY
/// double-frees on main independently of reassignment (both slots end up
/// holding one pointer and both stay armed); this change removes its leak
/// half but not its double free, and it is filed separately rather than
/// folded in.
///
/// COVERAGE: against the pre-fix compiler this leaks 6,480 B in 195 blocks
/// at `KARAC_OPT_LEVEL=0` (165 boxes of 32 B plus 30 struct boxes of 40 B)
/// and is CLEAN at the default `-O2`, where the boxes fold away — so the
/// memory half rides the `-O0` leg (`scripts/asan-o0-leg.sh`,
/// B-2026-08-04-17) and the `-O2` leg carries the value plus the allocation
/// floor.
///
/// The expected value is COMPUTED, not read off a run: every payload is
/// seeded from the opaque `env.args().len()` and each arm subtracts the
/// seed and its own offset back out to leave `i` (the `a7` arm contributes
/// `2i`, being measured before and after its reassignment), with the String
/// arm adding 1 for a successful byte read — `12i + 1` per iteration over
/// 30 iterations, so `12 * (0+…+29) + 30 = 5250`.
#[test]
fn asan_reassigned_boxed_payload_binding_frees_displaced_box() {
    assert_clean_asan_run_min_allocs(
        r#"
struct Wide { a: i64, b: i64, c: i64, d: i64, e: i64 }
fn consume(v: Option[Option[i64]]) -> i64 {
    match v { Option.Some(Option.Some(x)) => x, Option.Some(Option.None) => -1, Option.None => -2 }
}
fn bump(v: Option[Option[i64]]) -> Option[Option[i64]] {
    match v { Option.Some(Option.Some(x)) => Option.Some(Option.Some(x + 100)), _ => Option.None }
}
fn main() {
    let n = env.args().len() as i64;
    let mut outer: Option[Option[i64]] = Option.None;
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 30 {
        let k = n + i;

        let mut a1: Option[Option[i64]] = Option.Some(Option.Some(k));
        a1 = Option.Some(Option.Some(k + 100));
        acc = acc + consume(a1) - n - 100;

        let mut a2: Result[Option[Option[i64]], i64] = Result.Ok(Option.Some(Option.Some(k)));
        a2 = Result.Ok(Option.Some(Option.Some(k + 100)));
        acc = acc + (match a2 { Result.Ok(Option.Some(Option.Some(x))) => x, Result.Ok(_) => -1, Result.Err(e) => e }) - n - 100;

        let mut a3: Option[Wide] = Option.Some(Wide { a: k, b: 1, c: 2, d: 3, e: 4 });
        a3 = Option.Some(Wide { a: k + 100, b: 1, c: 2, d: 3, e: 4 });
        acc = acc + (match a3 { Option.Some(w) => w.a, Option.None => -1 }) - n - 100;

        let mut a4: Option[Option[i64]] = Option.Some(Option.Some(k));
        a4 = Option.Some(Option.Some(k + 100));
        a4 = Option.Some(Option.Some(k + 200));
        acc = acc + consume(a4) - n - 200;

        let mut a5: Option[Option[i64]] = Option.Some(Option.Some(k));
        if i % 2 == 0 { a5 = Option.Some(Option.Some(k)); }
        acc = acc + consume(a5) - n;

        let mut a6: Option[Option[i64]] = Option.Some(Option.Some(k));
        a6 = a6;
        acc = acc + consume(a6) - n;

        let mut a7: Option[Option[i64]] = Option.Some(Option.Some(k));
        acc = acc + consume(a7) - n;
        a7 = Option.Some(Option.Some(k + 100));
        acc = acc + consume(a7) - n - 100;

        let mut a8: Option[Option[i64]] = Option.Some(Option.Some(k));
        a8 = Option.None;
        a8 = Option.Some(Option.Some(k + 100));
        acc = acc + consume(a8) - n - 100;

        let mut a9: Option[Option[i64]] = Option.Some(Option.Some(k));
        a9 = bump(a9);
        acc = acc + consume(a9) - n - 100;

        outer = Option.Some(Option.Some(k));
        acc = acc + consume(outer) - n;

        let mut v: Vec[i64] = Vec.new();
        v.push(k);
        v = Vec.new();
        v.push(k + 100);
        acc = acc + v[0] - n - 100;

        let mut s: String = "a" + k.to_string();
        s = "bb" + k.to_string();
        acc = acc + (if s.len() > 0 { 1 } else { 0 });

        i = i + 1;
    }
    println(acc);
}
"#,
        &["5250"],
        "reassigned_boxed_payload_displaced_box",
        20,
    );
}

/// B-2026-08-07-5 — a whole-value MOVE between two boxed-payload enum
/// bindings left both slots holding one box, both armed.
///
/// `b = c` copies the enum struct, box pointer included. Both slots then
/// hold that one pointer and both keep their let-site drop, so scope exit
/// frees it twice — a glibc double free, not a leak.
///
/// B-2026-08-05-20 fixed exactly this for the DECLARATION form
/// (`let b2 = body;`) and the suppressor it added is applied at every other
/// move position — call arguments, struct-literal fields, method receivers,
/// channel sends. The ASSIGNMENT position was simply not among its call
/// sites, alongside the Map/Set and struct move-suppressions the same arm
/// already performs.
///
/// THE NESTED ACTION NEEDED ITS OWN SUPPRESSOR, at BOTH positions, and the
/// `let` half of that was a gap no row had recorded: B-2026-08-05-20 only
/// ever knew direct boxes, and B-2026-08-06-32 introduced the nested action
/// without exercising a binding-to-binding move at all. The existing
/// suppressor cannot serve it twice over — it is gated on
/// `boxed_enum_payload_vars` (nested bindings are deliberately in their own
/// set) and it zeroes word 0 of the OUTER enum, which for a nested box is
/// the inner enum's TAG rather than the pointer. The new one reads the word
/// index off the queued action (`inner_tag_field + 1`) so it cannot drift
/// from the layout arithmetic that produced it.
///
/// The over-suppression direction is what half these arms are for, since
/// this fix DISARMS a source and an over-eager disarm strands the box:
///
///   * `a5 = a5` and its nested twin — a self-assign must NOT be disarmed,
///     or the only owner of the value just stored is gone.
///   * `a6` — the move is inside an `if`, so on the untaken path the source
///     must still free its own box. The suppressor is a runtime word zero
///     emitted in the branch's block, which is what makes that work; a
///     compile-time retraction could not tell the paths apart.
///   * `a7` — the source is REASSIGNED after being moved from, so its
///     action must still free the NEW box it then owns.
///   * `a8` — the destination is reassigned after the move, which pairs
///     this with B-2026-08-07-4's eager free: that free must reclaim the
///     box this move handed over, and must not fire twice with it.
///   * `a9` — a CHAIN (`a = s; d = a;`), where the middle binding is both a
///     disarmed source and a live destination.
///
/// Also covers a boxed STRUCT payload and a HEAP-BEARING interior, where
/// the value moved must arrive with its String intact.
///
/// COVERAGE: the pre-fix compiler ABORTS at BOTH opt levels (`free():
/// double free detected in tcache 2`, with `Invalid free()` under
/// valgrind), so unlike this family's leak rows the default suite run is a
/// real gate here rather than only the `-O0` leg. What the `-O0` leg adds
/// is the over-suppression half: at `-O2` only a handful of allocations
/// survive folding, so a stranded box would not be visible there.
///
/// The expected value is COMPUTED, not read off a run: every payload is
/// seeded from the opaque `env.args().len()` and each arm subtracts the
/// seed and its own offset back out to leave `i` (the `a7` arm contributes
/// `2i`, being read through both bindings), with the String arm adding 1
/// for a successful byte read — `11i + 1` per iteration over 30 iterations,
/// so `11 * (0+…+29) + 30 = 4815`.
#[test]
fn asan_boxed_payload_binding_move_between_bindings_frees_once() {
    assert_clean_asan_run(
        r#"
struct Wide { a: i64, b: i64, c: i64, d: i64, e: i64 }
struct WideS { a: i64, b: i64, c: i64, d: i64, s: String }
fn u(v: Option[Option[i64]]) -> i64 {
    match v { Option.Some(Option.Some(x)) => x, Option.Some(Option.None) => -1, Option.None => -2 }
}
fn ur(v: Result[Option[Option[i64]], i64]) -> i64 {
    match v { Result.Ok(Option.Some(Option.Some(x))) => x, Result.Ok(_) => -1, Result.Err(e) => e }
}
fn main() {
    let n = env.args().len() as i64;
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 30 {
        let k = n + i;

        let mut a1: Option[Option[i64]] = Option.Some(Option.Some(k));
        let s1: Option[Option[i64]] = Option.Some(Option.Some(k + 100));
        a1 = s1;
        acc = acc + u(a1) - n - 100;

        let mut a2: Result[Option[Option[i64]], i64] = Result.Ok(Option.Some(Option.Some(k)));
        let s2: Result[Option[Option[i64]], i64] = Result.Ok(Option.Some(Option.Some(k + 100)));
        a2 = s2;
        acc = acc + ur(a2) - n - 100;

        let b3: Result[Option[Option[i64]], i64] = Result.Ok(Option.Some(Option.Some(k)));
        let a3: Result[Option[Option[i64]], i64] = b3;
        acc = acc + ur(a3) - n;

        let mut a4: Option[Wide] = Option.Some(Wide { a: k, b: 1, c: 2, d: 3, e: 4 });
        let s4: Option[Wide] = Option.Some(Wide { a: k + 100, b: 1, c: 2, d: 3, e: 4 });
        a4 = s4;
        acc = acc + (match a4 { Option.Some(w) => w.a, Option.None => -1 }) - n - 100;

        let mut a5: Option[Option[i64]] = Option.Some(Option.Some(k));
        a5 = a5;
        acc = acc + u(a5) - n;

        let mut a6: Option[Option[i64]] = Option.Some(Option.Some(k));
        let s6: Option[Option[i64]] = Option.Some(Option.Some(k));
        if i % 2 == 0 { a6 = s6; }
        acc = acc + u(a6) - n;

        let mut a7: Option[Option[i64]] = Option.Some(Option.Some(k));
        let mut s7: Option[Option[i64]] = Option.Some(Option.Some(k + 100));
        a7 = s7;
        s7 = Option.Some(Option.Some(k + 200));
        acc = acc + u(a7) - n - 100;
        acc = acc + u(s7) - n - 200;

        let mut a8: Option[Option[i64]] = Option.Some(Option.Some(k));
        let s8: Option[Option[i64]] = Option.Some(Option.Some(k + 100));
        a8 = s8;
        a8 = Option.Some(Option.Some(k + 200));
        acc = acc + u(a8) - n - 200;

        let mut a9: Option[Option[i64]] = Option.Some(Option.Some(k));
        let s9: Option[Option[i64]] = Option.Some(Option.Some(k + 100));
        let mut d9: Option[Option[i64]] = Option.Some(Option.Some(k + 200));
        a9 = s9;
        d9 = a9;
        acc = acc + u(d9) - n - 100;

        let mut aa: Option[WideS] = Option.Some(WideS { a: k, b: 1, c: 2, d: 3, s: "p" + k.to_string() });
        let sa: Option[WideS] = Option.Some(WideS { a: k + 100, b: 1, c: 2, d: 3, s: "q" + k.to_string() });
        aa = sa;
        acc = acc + (match aa { Option.Some(w) => w.a + (if w.s.len() > 0 { 1 } else { 0 }), Option.None => -1 }) - n - 100;

        i = i + 1;
    }
    println(acc);
}
"#,
        &["4815"],
        "boxed_payload_binding_move_between_bindings",
    );
}

/// B-2026-08-07-2 shape 3 — a STRUCT between the levels. The envelope is
/// heap even when what it holds is not, and nobody owned it.
///
/// `struct W { o: Option[Option[i64]] }` boxes: an `Option` is 4 LLVM words
/// against `Option`'s own 3-word payload area, so the inner one is spilled
/// behind a pointer with not a byte of heap inside it. Every predicate in
/// this family asks whether the PAYLOAD owns heap and so answers no, which
/// left `field_copy_supported` reading `W` as caller-retains — and that in
/// turn switches off `emit_struct_drop_synthesis`'s entire `OptionInline`
/// pass, so the field got no drop at all.
///
/// THE ROW POINTED SOMEWHERE ELSE, and the `never_read` / `bare` arms are
/// why. It filed this as a `Result[W, E]` problem and proposed widening the
/// Result-level `nested_boxed_enum_payload_variants` to walk struct fields,
/// naming `__karac_drop_struct_W` as the rival owner to rule out. That
/// widening was implemented and reverted for double-freeing. The wrapper is
/// irrelevant: a bare `let w: W` whose field is NEVER READ leaks the same
/// 320 B / 10, so this was always the struct's own field drop.
///
/// THE CONTESTED OWNER RESOLVES ITSELF once the fix is at the field. The
/// `moved_out` arms are the shapes the row said made the owner contested —
/// `let inner = w.o` gives `inner`'s let site a `BoxedEnumDrop` over the
/// same box — and they are clean here because classifying the field the way
/// its heap-payload sibling is classified also inherits that sibling's
/// move-out neutralization, which keys on the same classifier.
///
/// CONTROLS. `W2` is the heap-interior sibling that always worked and must
/// not start double-freeing. `H` is a boxed all-scalar STRUCT payload,
/// admitted by the same predicate. `Outer` nests `W` a level down and
/// `Vec[W]` puts it in a container, both of which reach the field drop by
/// different routes.
///
/// NOT COVERED, deliberately, so this fixture stays green on what it
/// asserts: a fresh-temp `W` passed BY VALUE still orphans the caller's
/// envelope (B-2026-08-07-12 leg 2 — that gate declines correctly on heap
/// grounds and the envelope is not heap by its reckoning), and an
/// `Option[(i64, i64, i64, i64)]` tuple payload still leaks because the
/// entry copy cannot duplicate it, which is exactly why the predicate here
/// refuses it rather than pairing a drop with a copy that does nothing.
///
/// Expected value is COMPUTED: 1+2+4+8+16+32+64+128+256 = 511 per
/// iteration, x40 = 20440.
/// B-2026-08-12-15 — a boxed `Option` FIELD envelope inside an inline
/// STRUCT payload has an owner in every frame that does not consume it.
///
/// THE FIRST LEG IS THE POINT OF THE FIXTURE. `let r: Result[W, i64] =
/// Result.Ok(W { … });` with the value never read, never passed and never
/// matched leaked 32 B per construction — with NO CALL ANYWHERE IN THE
/// PROGRAM. The row was filed against the by-value call and against
/// c24343b3's entry copy, and four fixes aimed there failed; this leg is
/// what refutes both. The entry copy had only removed an ACCIDENTAL owner
/// (the callee's arm, which used to free the caller's envelope) from one of
/// the forms.
///
/// Neither existing predicate could name the box. `W` is 4 words and fits
/// `Result`'s 5-word area, so nothing boxes at the outer level and
/// `boxed_enum_payload_variants` reports nothing; the inline payload is a
/// STRUCT rather than an `Option`/`Result`, so
/// `nested_boxed_enum_payload_variants` reports nothing either. Hence the
/// third sibling, `struct_payload_boxed_field_variants`.
///
/// THE ARMS ARE THE THREE OWNING FRAMES, one leg each, because a fix at any
/// one of them alone is wrong at the other two:
///
///   * `nocall` — the LET SITE, and the leg that refutes the row's cause:
///     not one function call appears in it. It also checks the HANDOFF,
///     because its arm binds `v` out and `__karac_drop_struct_V` frees the
///     box too — with the let site armed and no handoff this aborts with a
///     glibc `double free detected in tcache 2`.
///   * `wild` — the arm that does NOT bind (`Ok(_)`) must NOT hand over;
///     the scrutinee stays the owner. The mirror of `nocall`, and the
///     reason the handoff is gated on `pattern_consumes_field` rather than
///     on the arm merely existing.
///   * `cls(Result.Ok(…))` — the FRESH TEMP, the one form with no binding
///     to hang a drop on. Owned at the argument spill.
///   * `cls(bound)` twice and `cls(idr(through))` — a bound argument and a
///     passthrough, both of which must KEEP the caller's registration
///     rather than retract it: the callee declines this population, so
///     retracting hands the box to nobody. Passing the same binding twice
///     is the shape whose pre-c24343b3 spelling double-freed, so it is also
///     the guard against paying for this leak with that crash.
///
/// `V` PUTS A FIELD AHEAD OF THE BOX on purpose. The cleanup walks a
/// FLATTENED payload, so the inner tag is at `1 + <words of the fields
/// before it>` — `V.a` shifts it, and a hardcoded 1 (which is all the
/// sibling predicate ever needed) would read `a` as the tag and the tag as
/// a pointer. `W`, whose `Option` is first, is the unshifted control. One
/// scalar is the most `V` can carry: an `Option` is 4 words against
/// `Result`'s 5-word area, so a second field boxes the payload WHOLE and
/// leaves this code path unexercised.
///
/// THE `sbox` LEG IS WHAT MAKES THIS FIXTURE NON-VACUOUS, and it is worth
/// saying why it has to be there. Every other leg is ALL-SCALAR, and an
/// all-scalar envelope does not survive `-O2`: the box round-trip is pure
/// arithmetic the optimizer folds away, so the allocation never happens and
/// a clean ASAN run over it proves nothing. Two versions of this fixture
/// tripped the harness's anti-vacuity floor at 8 allocations before that
/// was understood — reading the payload back does NOT rescue it, because
/// the read folds too. `sbox` carries a `String` through the same envelope,
/// which cannot be folded, so the allocations are real at both opt levels.
/// The scalar legs still assert at `-O0`, where the same fixture runs under
/// `scripts/asan-o0-leg.sh` and where the row was originally caught.
///
/// `sbox` is deliberately the LET-AND-MATCH spelling and not the temp
/// argument one. The temp-argument String shape still leaks its INTERIOR
/// (this family frees envelopes box-only by design) and has its own open
/// row; using it here would pin a known leak.
///
/// WHERE THIS ACTUALLY PINS: `-O0`, i.e. `scripts/asan-o0-leg.sh`, NOT the
/// default `cargo test --features llvm` run. Measured against the
/// pre-fix compiler: at `-O0` it fails with `LeakSanitizer: 5056 byte(s)
/// leaked in 158 allocation(s)`; at `-O2` it PASSES, because the scalar
/// envelopes the fix is about are folded away before they can leak. Stated
/// rather than left implicit, because a green default run over this fixture
/// is not evidence the row stays fixed — the `-O0` leg is.
///
/// NOT COVERED, deliberately, and each is a live leak with its own open row
/// rather than something papered over here: an enum ctor wrapping a LIVE
/// binding (`cls(Result.Ok(w))` for a bound `w`), a callee-manufactured
/// value handed straight on (`cls(mk(n))`), and the String INTERIOR of a
/// temp-argument envelope — this family frees envelopes box-only by design,
/// so the interior keeps whatever owner it had, which for that one shape is
/// nobody.
/// B-2026-08-12-17 — the two by-value ARGUMENT forms B-2026-08-12-15 left
/// leaking, where the argument carries a boxed field envelope that no
/// construction at the call site minted.
///
/// ONE ROOT CAUSE, not two, and the bound spellings are what prove it. Each
/// leaking form has a `let`-bound twin that was already clean:
///
///   let r = Result.Ok(w);  -- clean        takw(Result.Ok(w));  -- leaked
///   let m = mkw(n);        -- clean        takw(mkw(n));        -- leaked
///
/// So neither the ctor move nor the callee's return is at fault: both are
/// handled the moment a binding exists to own the result. What was missing
/// is the ARGUMENT-POSITION owner, and both forms were refused by the same
/// predicate (`optres_arg_mints_field_envelope`) for the same reason — it
/// admitted only constructions, because a place read can alias an envelope
/// its owner still frees.
///
/// THE DISCRIMINATOR IS `nested_boxed_owner_source_of`, which already
/// resolves passthrough chains and the alias map to a fixpoint. It is what
/// separates the two `idw(...)` legs here, and they are the whole reason
/// this fixture has controls:
///
///   * `takw(idw(mkw(n)))` — the passthrough forwards a TEMP, nothing owns
///     it, so the caller must. Leaked before this row.
///   * `takw(idw(b))` — the passthrough forwards a BINDING, whose let site
///     owns it. Clean before and after; registering here would be a double
///     free, so this leg is the guard against buying the fix with a crash.
///
/// `takw(Result.Ok(w))` is the matching guard on the other side: `w` is a
/// live struct binding, and the temp may own its envelope only because the
/// ctor move disarms `w`. `let r2 = Result.Ok(w2)` is the same move with a
/// binding on the far side, which must stay single-owner too.
///
/// The `sbox` leg and the `-O0` caveat are as described on the sibling
/// fixture above: all-scalar envelopes fold away at `-O2`, so the String
/// leg is what makes the allocation count real, and `scripts/asan-o0-leg.sh`
/// is where this actually pins.
#[test]
fn asan_struct_payload_boxed_field_envelope_owned_in_arg_position() {
    assert_clean_asan_run_min_allocs(
        r#"
struct W { o: Option[Option[i64]] }
struct S { o: Option[Option[String]] }
fn takw(r: Result[W, i64]) -> i64 {
    match r { Result.Ok(w) => match w.o { Option.Some(Option.Some(x)) => x, _ => -1 }, Result.Err(e) => e }
}
fn idw(r: Result[W, i64]) -> Result[W, i64] { r }
fn mkw(n: i64) -> Result[W, i64] { Result.Ok(W { o: Option.Some(Option.Some(n)) }) }
fn main() {
    let n = env.args().len() as i64;
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 40 {
        let sbox: Result[S, i64] = Result.Ok(S { o: Option.Some(Option.Some(f"s{n + i}")) });
        acc = acc + match sbox {
            Result.Ok(s) => match s.o { Option.Some(Option.Some(t)) => t.len() as i64, _ => -1 },
            Result.Err(e) => e,
        };
        let w: W = W { o: Option.Some(Option.Some(n + i)) };
        acc = acc + takw(Result.Ok(w));
        acc = acc + takw(mkw(n + i));
        acc = acc + takw(idw(mkw(n + i)));
        let b: Result[W, i64] = Result.Ok(W { o: Option.Some(Option.Some(n + i)) });
        acc = acc + takw(idw(b));
        let w2: W = W { o: Option.Some(Option.Some(n + i)) };
        let r2: Result[W, i64] = Result.Ok(w2);
        acc = acc + match r2 {
            Result.Ok(v) => match v.o { Option.Some(Option.Some(x)) => x, _ => -1 },
            Result.Err(e) => e,
        };
        i = i + 1;
    }
    println(acc);
}
"#,
        &["4211"],
        "struct_payload_boxed_field_envelope_owned_in_arg_position",
        40,
    );
}

#[test]
fn asan_struct_payload_boxed_field_envelope_owned_without_call() {
    assert_clean_asan_run_min_allocs(
        r#"
struct W { o: Option[Option[i64]] }
struct V { a: i64, o: Option[Option[i64]] }
struct S { o: Option[Option[String]] }
fn cls(r: Result[W, i64]) -> i64 {
    match r { Result.Ok(w) => match w.o { Option.Some(Option.Some(x)) => x, _ => -1 }, Result.Err(e) => e }
}
fn idr(r: Result[W, i64]) -> Result[W, i64] { r }
fn main() {
    let n = env.args().len() as i64;
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 40 {
        let sbox: Result[S, i64] = Result.Ok(S { o: Option.Some(Option.Some(f"s{n + i}")) });
        acc = acc + match sbox {
            Result.Ok(s) => match s.o { Option.Some(Option.Some(t)) => t.len() as i64, _ => -1 },
            Result.Err(e) => e,
        };
        let nocall: Result[V, i64] = Result.Ok(V { a: 1, o: Option.Some(Option.Some(n + i)) });
        acc = acc + match nocall {
            Result.Ok(v) => match v.o { Option.Some(Option.Some(x)) => v.a + x, _ => -1 },
            Result.Err(e) => e,
        };
        let wild: Result[W, i64] = Result.Ok(W { o: Option.Some(Option.Some(n + i)) });
        acc = acc + match wild { Result.Ok(_) => 2, Result.Err(e) => e };
        acc = acc + cls(Result.Ok(W { o: Option.Some(Option.Some(n + i)) }));
        let bound: Result[W, i64] = Result.Ok(W { o: Option.Some(Option.Some(n + i)) });
        acc = acc + cls(bound);
        acc = acc + cls(bound);
        let through: Result[W, i64] = Result.Ok(W { o: Option.Some(Option.Some(n + i)) });
        acc = acc + cls(idr(through));
        i = i + 1;
    }
    println(acc);
}
"#,
        &["4331"],
        "struct_payload_boxed_field_envelope_owned_without_call",
        40,
    );
}

/// B-2026-08-07-6 — the DIRECT sibling of the chain above: the same box
/// inside a box, with no `Result` wrapper, so a different action owns the
/// outermost envelope.
///
/// `Option[Option[Option[i64]]]` boxes at the OUTER level — its 4-word
/// payload outgrows Option's 3-word area — so `BoxedEnumDrop` owns that
/// box, correctly, for one box. What the box HOLDS is another `Option`
/// whose own 4-word payload is boxed again, and nothing freed the second
/// envelope. The fix is the SAME walk the `Result`-wrapped twin already
/// used (`emit_nested_box_chain_free`), reached from the other action; the
/// only judgement was which registration sites get a chain, and the answer
/// is measured rather than uniform — see the residue note below.
///
/// ARMS, each a shape that moved or a control that must not:
///   * `a` — the row's own shape, triple nest matched in place. 320 B / 10
///     pre-fix.
///   * `b` — quadruple nest, i.e. TWO envelopes below the first. Pre-fix
///     this is the arm that shows a chain rather than an off-by-one:
///     definitely-lost AND indirectly-lost both move.
///   * `c` — a heap `String` INTERIOR under a two-envelope chain. This is
///     the envelope/interior boundary as an assertion: the walk frees the
///     envelopes and must not reach the `String` the arm binds out, which
///     already has an owner. A double free here is what getting the
///     boundary wrong looks like, and B-2026-08-06-32 aborted exactly that
///     way when the free was widened to the payload's drop.
///   * `d` — the EMPTY-chain control. `Option[Option[i64]]` has exactly one
///     envelope and must stay at exactly one free; it is the shape every
///     pre-existing single-box registration takes.
///   * `e` — an arm that binds the boxed CONTENT out rather than
///     destructuring through it.
///   * `f` — reassignment, where B-2026-08-07-4's eager free at the STORE
///     has to carry the chain too. 640 B / 10 pre-fix, i.e. both values.
///   * `g` / `h` / `j` — the guard controls. Each leaves some level's
///     payload words holding a value rather than a pointer, so a missing
///     tag or null check frees a scalar instead of an envelope.
///
/// COVERAGE, and it is honest about which leg carries it: pre-fix this
/// leaks 8,960 B in 280 blocks plus 1,280 B in 40 blocks indirectly at
/// `KARAC_OPT_LEVEL=0`, and is CLEAN at the default `-O2`, where the
/// envelopes are local and LLVM deletes the malloc/free pairs outright. So
/// the memory half rides entirely on the `-O0` leg
/// (`scripts/asan-o0-leg.sh`, B-2026-08-04-17) — the `-O2` run asserts the
/// value and the floor only. Worth recording: POST-fix the `-O2` binary
/// allocates 206 where pre-fix it allocated 126, because the chain walk
/// makes the pairs non-removable. The fixture therefore got LESS vacuous by
/// being fixed, which is the opposite of the usual direction and not
/// something to rely on.
///
/// RESIDUE, deliberately absent so this stays green: passing the binding by
/// value to an owned param, moving it into a struct literal, and pushing it
/// into a `Vec` all still leak the chain (320 B, and 320+320 B for the two
/// move shapes). Each is a different registration site, each measured
/// identical before and after this change, and each is filed rather than
/// folded in.
///
/// The expected value is COMPUTED, not read off a run: payloads are seeded
/// from the opaque `env.args().len()` and five arms subtract the seed back
/// out to leave `i`, the String arm contributes 1 and the three guard arms
/// -1 each — `5i - 2` per iteration, so `5 * (0+…+39) - 2 * 40 = 3820`.
#[test]
fn asan_direct_boxed_enum_chain_frees_every_envelope() {
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
        let a: Option[Option[Option[i64]]] = Option.Some(Option.Some(Option.Some(n + i)));
        match a {
            Option.Some(Option.Some(Option.Some(x))) => { acc = acc + x - n; }
            _ => { acc = acc - 1; }
        }

        let b: Option[Option[Option[Option[i64]]]] = Option.Some(Option.Some(Option.Some(Option.Some(n + i))));
        match b {
            Option.Some(Option.Some(Option.Some(Option.Some(x)))) => { acc = acc + x - n; }
            _ => { acc = acc - 1; }
        }

        let c: Option[Option[Option[String]]] = Option.Some(Option.Some(Option.Some(mkstr(n + i))));
        match c {
            Option.Some(Option.Some(Option.Some(s))) => { if s.contains("envelope-") { acc = acc + 1; } }
            _ => { acc = acc - 1; }
        }

        let d: Option[Option[i64]] = Option.Some(Option.Some(n + i));
        match d {
            Option.Some(Option.Some(x)) => { acc = acc + x - n; }
            _ => { acc = acc - 1; }
        }

        let e: Option[Option[Option[i64]]] = Option.Some(Option.Some(Option.Some(n + i)));
        match e {
            Option.Some(inner) => {
                match inner {
                    Option.Some(Option.Some(x)) => { acc = acc + x - n; }
                    _ => { acc = acc - 1; }
                }
            }
            Option.None => { acc = acc - 1; }
        }

        let mut f: Option[Option[Option[i64]]] = Option.Some(Option.Some(Option.Some(n + i)));
        f = Option.Some(Option.Some(Option.Some(n + i)));
        match f {
            Option.Some(Option.Some(Option.Some(x))) => { acc = acc + x - n; }
            _ => { acc = acc - 1; }
        }

        let g: Option[Option[Option[i64]]] = Option.Some(Option.Some(Option.None));
        match g { Option.Some(Option.Some(Option.Some(x))) => { acc = acc + x; } _ => { acc = acc - 1; } }

        let h: Option[Option[Option[i64]]] = Option.Some(Option.None);
        match h { Option.Some(Option.Some(Option.Some(x))) => { acc = acc + x; } _ => { acc = acc - 1; } }

        let j: Option[Option[Option[i64]]] = Option.None;
        match j { Option.Some(Option.Some(Option.Some(x))) => { acc = acc + x; } _ => { acc = acc - 1; } }

        i = i + 1;
    }
    println(acc);
}
"#,
        &["3820"],
        "direct_boxed_enum_chain_frees_every_envelope",
        100,
    );
}

/// B-2026-08-07-11 leg (a) — the same envelope chain, owned at the OWNED
/// PARAM rather than at the let site.
///
/// B-2026-08-07-6 gave `BoxedEnumDrop` a `deeper_tags` chain at the LET site
/// only, deliberately: `BoxedEnumDrop` is registered from a dozen places and
/// each has its own ownership argument. `functions.rs`'s owned-param arm was
/// the first of the residue, and it is the cheapest because the callee is
/// ALREADY the owner of the outermost envelope on that arm — the deeper ones
/// live inside it, so nothing else can reach them once it frees the box they
/// hang off. 320 B / 10 pre-fix.
///
/// THE CALLER CANNOT BE A COMPETING OWNER, and the pre-fix measurement is
/// what says so rather than an argument: the let site's registration is
/// disarmed at the by-value move by
/// `suppress_inline_option_result_binding_move`, so the failure was a LEAK
/// of one envelope. Had the caller still been armed it would have been a
/// double free instead. That distinction is the whole reason this arm is
/// safe to widen and the two below are not.
///
/// ARMS: `tri` is the row's shape; `quad` puts TWO envelopes below the
/// first; `tristr` is the envelope/interior boundary again, binding a heap
/// `String` out from the bottom of a chain passed by value; `duo` is the
/// empty-chain control that must stay at exactly one free.
///
/// THE TWO ESCAPE CONTROLS ARE THE POINT OF THE REST. `idc` RETURNS its
/// param and `fwd` FORWARDS it to a second by-value param. Neither is
/// registered at all — `result_shared_nonescaping_param_names` excludes
/// them — so the terminal consumer stays the only owner. This is the exact
/// pair that double-freed at both opt levels, then SIGSEGV'd at `-O0`, when
/// the owned-param registration was first written without an escape guard
/// (recorded in that arm's comment). A chain makes each such mistake free
/// one more box than it should, so they are re-asserted here rather than
/// assumed still covered.
///
/// COVERAGE: pre-fix 320 B / 10 at `KARAC_OPT_LEVEL=0`, CLEAN at the default
/// `-O2` where the envelopes fold away — so like its let-site sibling the
/// memory half rides on the `-O0` leg (B-2026-08-04-17). Measured 646
/// allocations at `-O0` and 126 at `-O2`.
///
/// STILL RED and deliberately absent: moving the binding into a struct
/// literal, and pushing it into a `Vec`. Those are NOT chain gaps — a
/// SINGLE-box `Option[Option[i64]]` leaks 320 B through either, with no
/// chain anywhere — so they are a destination-ownership defect that
/// B-2026-08-07-11 now records correctly and keeps open.
///
/// The expected value is COMPUTED: five arms subtract the opaque
/// `env.args().len()` seed back out to leave `i` and the String arm
/// contributes 1 — `5i + 1` per iteration, so `5 * (0+…+39) + 40 = 3940`.
#[test]
fn asan_owned_param_boxed_enum_chain_frees_every_envelope() {
    assert_clean_asan_run_min_allocs(
        r#"
fn mkstr(n: i64) -> String {
    let mut s: String = String.new();
    s.push_str("envelope-");
    s.push_str(n.to_string());
    s.push_str("-padding-to-force-heap");
    s
}
fn tri(b: Option[Option[Option[i64]]]) -> i64 {
    match b { Option.Some(Option.Some(Option.Some(x))) => x, _ => 0 }
}
fn quad(b: Option[Option[Option[Option[i64]]]]) -> i64 {
    match b { Option.Some(Option.Some(Option.Some(Option.Some(x)))) => x, _ => 0 }
}
fn duo(b: Option[Option[i64]]) -> i64 {
    match b { Option.Some(Option.Some(x)) => x, _ => 0 }
}
fn tristr(b: Option[Option[Option[String]]]) -> i64 {
    match b { Option.Some(Option.Some(Option.Some(s))) => { if s.contains("envelope-") { 1 } else { 0 } } _ => -1 }
}
fn idc(o: Option[Option[Option[i64]]]) -> Option[Option[Option[i64]]] { o }
fn deep(o: Option[Option[Option[i64]]]) -> i64 {
    match o { Option.Some(Option.Some(Option.Some(x))) => x, _ => 0 }
}
fn fwd(o: Option[Option[Option[i64]]]) -> i64 { deep(o) }
fn main() {
    let n = env.args().len() as i64;
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 40 {
        let a: Option[Option[Option[i64]]] = Option.Some(Option.Some(Option.Some(n + i)));
        acc = acc + tri(a) - n;

        let b: Option[Option[Option[Option[i64]]]] = Option.Some(Option.Some(Option.Some(Option.Some(n + i))));
        acc = acc + quad(b) - n;

        acc = acc + tristr(Option.Some(Option.Some(Option.Some(mkstr(n + i)))));

        let d: Option[Option[i64]] = Option.Some(Option.Some(n + i));
        acc = acc + duo(d) - n;

        let p: Option[Option[Option[i64]]] = Option.Some(Option.Some(Option.Some(n + i)));
        let back = idc(p);
        match back { Option.Some(Option.Some(Option.Some(x))) => { acc = acc + x - n; } _ => { acc = acc - 1; } }

        let f: Option[Option[Option[i64]]] = Option.Some(Option.Some(Option.Some(n + i)));
        acc = acc + fwd(f) - n;

        i = i + 1;
    }
    println(acc);
}
"#,
        &["3940"],
        "owned_param_boxed_enum_chain_frees_every_envelope",
        100,
    );
}

/// B-2026-08-07-11 leg (b) — the envelope chain inside a STRUCT FIELD.
///
/// Two fixes meet here and neither works alone. 31768650 (B-2026-08-07-2
/// shape 3) gave the OUTERMOST envelope an owner: a field of type
/// `Option[Option[i64]]` now carries a `FieldDrop::OptionInline`, because
/// the envelope is heap even when what it holds is not. That left a chain
/// field freeing exactly one box — `Option[Option[Option[i64]]]` went from
/// 320 + 320 lost to 320 + 0, i.e. it BECAME a pure chain gap. This closes
/// the rest, in `emit_option_drop_fn`: its boxed branch asked
/// `emit_drop_fn_for_type_expr` for the payload's drop, which bottoms out
/// in the primitive no-op for a heapless-but-boxed `Option`, so every
/// envelope below the first was freed by nobody.
///
/// `emit_option_drop_fn(P)` drops an `Option[P]`, so the recursion takes the
/// INNER payload and terminates on the same `> 3` word test that created
/// the box — each level is strictly the next type down, and the walk stops
/// at the first payload that fits its area, which is a value rather than an
/// envelope.
///
/// ARMS: `H3` is the row's shape; `H4` puts two envelopes below the first
/// and is the one that pre-fix showed indirectly-lost as well as
/// definitely-lost; `H2` is the single-box control that 31768650 fixed and
/// that must stay at exactly one free; `hu` builds the struct and NEVER
/// READS the field, which is the shape that proves the drop is on the field
/// rather than on the match.
///
/// UNFLOORED DELIBERATELY, and this is the honest version of
/// B-2026-08-04-17's rule rather than an exception to it. At `-O2` this
/// program performs 6 allocations — the ASAN baseline plus `println` — so a
/// floor would assert something the run does not do. All four arms are
/// scalar-payload chains whose envelopes are frame-local, and LLVM deletes
/// the malloc/free pairs outright; the whole memory half rides on the `-O0`
/// leg (`scripts/asan-o0-leg.sh`), where the same program allocates 326.
/// Pre-fix every arm leaked 1,280 B at `-O0` and `H4` a further 1,280 B
/// indirectly. A floor here would be coverage-shaped rather than coverage,
/// which is the exact failure that row exists to name.
///
/// STILL RED and deliberately absent: `struct Hs { b:
/// Option[Option[Option[String]]] }` leaks 1,280 B at `-O0`, measured
/// IDENTICAL before and after this change. It takes the classifier's OTHER
/// branch — `payload_droppable` routes it to
/// `vec_elem_agg_drop_for_type_expr` rather than to the envelope path
/// touched here — so it is a different gap in the same classifier and is
/// recorded on B-2026-08-07-11 rather than folded in.
///
/// The expected value is COMPUTED: three arms subtract the opaque
/// `env.args().len()` seed back out to leave `i` and the fourth contributes
/// nothing — `3i` per iteration, so `3 * (0+…+39) = 2340`.
#[test]
fn asan_struct_field_boxed_enum_chain_frees_every_envelope() {
    assert_clean_asan_run(
        r#"
struct H3 { b: Option[Option[Option[i64]]] }
struct H4 { b: Option[Option[Option[Option[i64]]]] }
struct H2 { b: Option[Option[i64]] }
fn main() {
    let n = env.args().len() as i64;
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 40 {
        let a: Option[Option[Option[i64]]] = Option.Some(Option.Some(Option.Some(n + i)));
        let h3: H3 = H3 { b: a };
        match h3.b { Option.Some(Option.Some(Option.Some(x))) => { acc = acc + x - n; } _ => { acc = acc - 1; } }

        let q: Option[Option[Option[Option[i64]]]] = Option.Some(Option.Some(Option.Some(Option.Some(n + i))));
        let h4: H4 = H4 { b: q };
        match h4.b { Option.Some(Option.Some(Option.Some(Option.Some(x)))) => { acc = acc + x - n; } _ => { acc = acc - 1; } }

        let d: Option[Option[i64]] = Option.Some(Option.Some(n + i));
        let h2: H2 = H2 { b: d };
        match h2.b { Option.Some(Option.Some(x)) => { acc = acc + x - n; } _ => { acc = acc - 1; } }

        let u: Option[Option[Option[i64]]] = Option.Some(Option.Some(Option.Some(n + i)));
        let hu: H3 = H3 { b: u };
        acc = acc + 0;

        i = i + 1;
    }
    println(acc);
}
"#,
        &["2340"],
        "struct_field_boxed_enum_chain_frees_every_envelope",
    );
}

/// A NON-GENERIC user enum whose single-payload variant is itself an enum —
/// a shape with EXACTLY ONE legitimate owner, guarded from both directions.
///
/// Such a payload gets a 1-word carve-out rather than being sized to the
/// variant, so it boxes where a struct payload does not: `WrapE` is 4 words
/// against an area of 1, `WrapR` 6 against 1. The `WrapS`/`Big` arm is the
/// control for the other side — 6 words against an area of 6, no box at
/// all, and it must STAY unregistered because a `BoxedEnumDrop` over an
/// inline payload frees a word that was never a pointer.
///
/// 70c15e83 gave these boxes an owner at CONSTRUCTION. This fixture exists
/// because that is not the only place one could plausibly be registered,
/// and a second owner is not a harmless belt-and-braces: adding a
/// callee-side param registration on top (which looks right in isolation —
/// the callee does receive the box by value) double-frees the `Holder` arm
/// below at -O0, measured. So the four ways the value reaches its consumer
/// are all here — matched in place, moved out of a struct field into a
/// by-value param, and handed to one as a fresh temp — and the fixture is
/// red for a SECOND owner just as surely as for none.
///
/// Floored per B-2026-08-04-17 — `env.args().len()` seed, runtime-built
/// payloads, `contains`/`ends_with` byte reads. Without all three the boxes
/// are dead allocations LLVM deletes at -O2 and the fixture asserts nothing
/// (measured: 1 alloc with literal payloads, 957 with these).
///
/// The leak direction is `-O0`-only, and the suite's opt level comes from
/// the process `KARAC_OPT_LEVEL`, so it is red only under
/// `KARAC_OPT_LEVEL=0 cargo test --features llvm --test memory_sanitizer`
/// (8640 bytes in 240 allocations against a compiler without 70c15e83).
/// The double-free direction fails at either level.
#[test]
fn asan_nongeneric_enum_nested_enum_payload_box_no_leak() {
    assert_clean_asan_run_min_allocs(
        r#"
struct Big { a: i64, b: i64, c: i64, d: i64, e: i64, s: String }
enum WrapS { W(Big), Empty }
enum WrapE { W(Option[String]), Empty }
enum WrapR { W(Result[String, i64]), Empty }
struct Holder { o: WrapE, n: i64 }
fn consume(w: WrapE) -> i64 {
    match w {
        WrapE.W(Option.Some(s)) => if s.contains("payload") { 1 } else { 0 },
        WrapE.W(Option.None) => -1,
        WrapE.Empty => -2,
    }
}
fn mk(n: i64, i: i64, tag: String) -> String {
    let mut s: String = String.new();
    s.push_str("payload-");
    s.push_str(tag);
    s.push_str("-");
    s.push_str((n + i).to_string());
    s.push_str("-padding-to-force-heap");
    s
}
fn main() {
    let n = env.args().len() as i64;
    let mut acc: i64 = 0;
    let mut i: i64 = 0;
    while i < 60 {
        let s: WrapS = WrapS.W(Big { a: 1, b: 2, c: 3, d: 4, e: 5, s: mk(n, i, "struct") });
        match s {
            WrapS.W(b) => { if b.s.ends_with("heap") { acc = acc + 1; } }
            WrapS.Empty => { acc = acc - 1; }
        }
        let e: WrapE = WrapE.W(Option.Some(mk(n, i, "opt")));
        match e {
            WrapE.W(Option.Some(t)) => { if t.contains("payload") { acc = acc + 1; } }
            WrapE.W(Option.None) => { acc = acc - 1; }
            WrapE.Empty => { acc = acc - 1; }
        }
        let r: WrapR = WrapR.W(Result.Ok(mk(n, i, "res")));
        match r {
            WrapR.W(Result.Ok(t)) => { if t.contains("payload") { acc = acc + 1; } }
            WrapR.W(Result.Err(x)) => { acc = acc + x; }
            WrapR.Empty => { acc = acc - 1; }
        }
        let h = Holder { o: WrapE.W(Option.Some(mk(n, i, "field"))), n: 1 };
        acc = acc + consume(h.o);
        acc = acc + consume(WrapE.W(Option.Some(mk(n, i, "temp"))));
        i = i + 1;
    }
    println(acc);
}
"#,
        &["300"],
        "nongeneric_enum_nested_enum_payload_box",
        300,
    );
}

/// B-2026-08-06-21 — a boxed `Option`/`Result` binding passed by value to a
/// PASSTHROUGH callee was freed TWICE.
///
/// The by-value arg loop deliberately skips its move-suppression when
/// `call_arg_flows_into_return` holds: the callee hands the same value
/// back, so the caller keeps its own cleanup. That is right for an INLINE
/// payload (an `Option[String]` passthrough is single-free, measured) and
/// for the entry-copied heap struct that path carves out, where two
/// allocations genuinely exist. It is wrong for a BOXED one — nothing
/// entry-copies a box, so the source binding and the result binding hold
/// ONE pointer and both `BoxedEnumDrop`s fire. Confirmed in the emitted
/// module before fixing: one `@malloc`, two `@free`s.
///
/// The fix skips the RESULT binding's registration, leaving the source as
/// sole owner. The other direction — zeroing the source so the result owns
/// it — was tried and is wrong: when the result is DISCARDED the source is
/// the only owner there is, and zeroing it turned a clean program into a
/// 320-byte leak. The `idnest(d);` line below is that control and must stay
/// clean; narrowing a registration is the safe direction, widening a free
/// is not.
///
/// `Option[Wide]` carries a heap-owning payload and aborted at the DEFAULT
/// -O2 as well as -O0, so this fixture is red on both legs rather than only
/// under `KARAC_OPT_LEVEL=0` — the row scoped the bug as -O0-only from its
/// single `Option[Option[i64]]` shape.
///
/// Floored per B-2026-08-04-17 — opaque `env.args().len()` seed,
/// runtime-built payloads, `contains` byte reads (126 allocations at -O2).
#[test]
fn asan_boxed_enum_passthrough_arg_single_owner() {
    assert_clean_asan_run_min_allocs(
        r#"
struct Wide { a: i64, b: i64, c: i64, d: i64, e: i64, s: String }
fn idnest(o: Option[Option[i64]]) -> Option[Option[i64]] { o }
fn idwide(o: Option[Wide]) -> Option[Wide] { o }
fn mk(n: i64, i: i64) -> String {
    let mut s: String = String.new();
    s.push_str("payload-");
    s.push_str((n + i).to_string());
    s.push_str("-padding-to-force-heap");
    s
}
fn main() {
    let n = env.args().len() as i64;
    let mut acc: i64 = 0;
    let mut i: i64 = 0;
    while i < 40 {
        let a: Option[Option[i64]] = Option.Some(Option.Some(n + i));
        let abk = idnest(a);
        match abk { Option.Some(Option.Some(x)) => { acc = acc + x; } _ => { acc = acc - 1; } }
        let w: Option[Wide] = Option.Some(Wide { a: 1, b: 2, c: 3, d: 4, e: 5, s: mk(n, i) });
        let wbk = idwide(w);
        match wbk { Option.Some(x) => { if x.s.contains("payload") { acc = acc + 1; } } _ => { acc = acc - 1; } }
        let d: Option[Option[i64]] = Option.Some(Option.Some(n + i));
        idnest(d);
        i = i + 1;
    }
    println(acc);
}
"#,
        &["860"],
        "boxed_enum_passthrough_arg_single_owner",
        100,
    );
}

/// B-2026-08-05-7: `Option.ok_or(e)` evaluates `e` EAGERLY and then selects
/// between `Ok(payload)` and `Err(e)`. A fresh heap `e` that the statement
/// machinery temp-tracks was therefore owned twice — the tracked temp's
/// scope-exit free and the Result's Err-payload free hit one buffer, so
/// every `None` iteration double-freed (SIGABRT). The mirror-image hole sat
/// on the `Some` path, where `e` is discarded and nothing freed it at all.
///
/// This aborted on a DEFAULT -O2 build, not just at -O0: the sibling fixture
/// above only looked clean because it reads the Err payload through `.len()`
/// alone, which lets LLVM delete the allocation and with it the second free.
/// So this fixture reads the payload's BYTES (`starts_with`) and seeds from
/// `env.args().len()`, keeping the buffer genuinely live.
///
/// Both directions are pinned: a double-free if the Err payload is ever
/// owned twice again, and a leak if the `Some`-path free goes missing.
#[test]
fn asan_ok_or_fresh_err_payload_owned_exactly_once() {
    assert_clean_asan_run_min_allocs(
        r#"
fn main() {
    let base: i64 = env.args().len();
    let mut i: i64 = 0;
    let mut errs: i64 = 0;
    let mut hits: i64 = 0;
    while i < base + 59 {
        let o: Option[i64] = if i % 2 == 0 { Some(i * 10) } else { None };
        let r: Result[i64, String] = o.ok_or(f"absent-{i}-padded-well-past-inline-width");
        match r {
            Ok(v) => { errs = errs + v; },
            Err(e) => {
                if e.starts_with("absent") { hits = hits + 1; }
                errs = errs + e.len();
            },
        }
        i = i + 1;
    }
    println(errs);
    println(hits);
}
"#,
        // base=1 -> 60 iterations, 30 Some / 30 None. The 30 None arms each
        // add a ~38-char payload length and one hit.
        &["9865", "30"],
        "ok_or_fresh_err_payload_owned_exactly_once",
        // One f-string payload per iteration, every iteration — well above
        // the 3 an allocation-free run reports.
        40,
    );
}

// B-2026-07-30-11 (enum leg) — payload bodies added to a value enum's drop,
// plus the match/if-let move-out disarm.
//
// Same direction as the Vec leg: the bug was a LEAK and the fix is
// bodies-only (payload MEMORY was always freed by `__karac_drop_<E>`'s
// `NestedStruct` arm), so LSan cannot witness the fix landing. What it
// guards is the two ways this can go wrong in the unsafe direction:
//
//  * OVER-firing — the payload body runs against a payload a match arm
//    already moved out, whose words the memory-side cap-zeroing has wiped.
//    Reading `self.buf[0]` there is a use-after-free.
//  * DOUBLE-freeing — the bodies walk sits beside an existing free of the
//    same payload region, so a second memory-touching call would show up
//    here immediately.
//
// Non-vacuity, same lesson as the Vec leg: `buf.clear()` alone lets LLVM
// delete every element allocation. The `self.buf[0]` read observes the
// BYTES and keeps them; the guard is never true, so the branch is dead at
// runtime but not to the optimizer.
#[test]
fn asan_enum_payload_user_drop_bodies_fire_once() {
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
enum Slot { Empty, Full(Res) }
enum Nest { Nil, Held(Wrap) }

fn mk(i: i64) -> Res {
    let mut b: Vec[i64] = Vec.new();
    b.push(i);
    return Res { tag: 1, buf: b };
}

fn main() {
    let mut n = 0i64;
    let mut i = 0i64;
    while i < 200i64 {
        // Held to the binding's end — the payload body fires here.
        let s = Slot.Full(mk(i));
        n = n + 1i64;

        // Drop-bearing payload FIELD, one struct deeper.
        let w = Nest.Held(Wrap { r: mk(i) });
        n = n + 1i64;

        // Payload MOVED OUT by a match arm — the source must run no body
        // against the wiped payload words.
        let t = Slot.Full(mk(i));
        match t {
            Slot.Full(r) => { n = n + r.tag; }
            Slot.Empty => { n = n + 100i64; }
        }

        // Same, through `if let`.
        let u = Slot.Full(mk(i));
        if let Slot.Full(q) = u { n = n + q.tag; }
        i = i + 1;
    }
    println(n);
}
"#,
        // 1 + 1 + 1 + 1 per iteration x 200 = 800.
        &["800"],
        "enum_payload_user_drop_bodies_fire_once",
    );
}

/// B-2026-08-18-48 — a heap-boxed enum payload MOVED into a by-value call
/// is freed by nobody.
///
/// The caller's move-out sentinel zeroes its slot, so the scope-exit
/// `BoxedEnumDrop` reloads tag 0 and skips; the callee takes an
/// `Option[W]` by value and emits no free at all. Ownership passes from a
/// party that gave it up to one that never took it.
///
/// AUTO-PAR IS LOAD-BEARING, not incidental. The same source with
/// `KARAC_AUTO_PAR=0` is clean at 2000 iterations, and NOT because anything
/// frees: everything inlines into one function, the box never escapes, and
/// LLVM deletes the allocation outright. Under auto-par the pointer crosses
/// a thread boundary through the fork's return struct, so the allocation is
/// real and the missing free is a real leak. The `min_allocs` floor is what
/// keeps that honest — if a future optimizer change elides the box again,
/// this fixture fails as "optimized away" rather than passing vacuously.
///
/// `W` is four `i64`s deliberately: 32 bytes exceeds the 3-word inline
/// payload area, so `Option[W]` boxes. Two `let`s from independent `get`
/// calls are what auto-par forks on, and `tail(hit) + tail(miss)` is what
/// moves both.
#[test]
fn asan_boxed_payload_moved_into_by_value_call_is_freed() {
    assert_clean_asan_run_min_allocs_auto_par(
        r#"
struct W { f0: i64, f1: i64, f2: i64, f3: i64 }

fn get(v: ref Vec[W], k: i64) -> Option[W] {
    let mut i = 0;
    while i < v.len() {
        if v[i].f0 == k {
            return Some(v[i]);
        }
        i = i + 1;
    }
    return None;
}

fn tail(o: Option[W]) -> i64 {
    match o {
        Some(w) => w.f3,
        None => -1i64,
    }
}

fn build(seed: i64) -> i64 {
    let mut ns: Vec[W] = Vec.new();
    ns.push(W { f0: seed, f1: seed + 1i64, f2: seed + 2i64, f3: seed + 3i64 });
    let hit = get(ns, seed);
    let miss = get(ns, seed + 99i64);
    return tail(hit) + tail(miss);
}

fn main() {
    let mut total = 0;
    let mut n = 0;
    while n < 2000 {
        total = total + build(n);
        n = n + 1;
    }
    println(total.to_string());
}
"#,
        // sum over n in 0..2000 of (n + 3) + (-1) = 1999000 + 4000.
        &["2003000"],
        "boxed_payload_moved_into_by_value_call_is_freed",
        2000,
    );
}

/// B-2026-08-09-11 — the BLOCK-spelling sibling of the fixture above.
///
/// The live-local clone leg now fires at `if let` and `while let` as well
/// as at `match`, and each site carries its own drop wiring: the if-let
/// clone rides the enclosing frame's freshtemp drop, while the `while let`
/// clone is remade per evaluation with a miss-edge free at loop exit.
/// Those are two different ways to leak or double-free the same buffer, so
/// each gets a case rather than trusting the match-site fixture.
///
/// `let…else` is absent because the leg is not wired there — see
/// `compile_let_else`: the checker rejects the shape that would need it.
///
/// Case 3 is the liveness control in the other direction: with the source
/// DEAD the clone must NOT fire, and one that fired without disarming the
/// source would leak the original every pass — the LSan-only signature the
/// sibling fixtures document.
///
/// The `while let` case deliberately uses a LIVE source, to keep this
/// fixture on THIS leg's wiring. Its dead-source twin exercises the
/// transfer path instead, and double-freed on a cause that predates this
/// leg (B-2026-08-09-14, since fixed); it has its own fixture below rather
/// than being folded in here.
///
/// Every payload is read BYTE-WISE for the reason the siblings document:
/// a buffer whose bytes are never read is a dead allocation LLVM deletes,
/// and the fixture would assert nothing.
#[test]
fn asan_live_local_block_spellings_over_user_enum_free_each_buffer_once() {
    assert_clean_asan_run(
        r#"
enum E { A(String), B }
fn main() {
    let mut i: i64 = 0i64;
    while i < 4i64 {
        // 1. `if let`, consuming, source LIVE — the row's own shape.
        let a: E = E.A(f"if{i}");
        if let E.A(v) = a { let k: String = v; println(k); }
        if let E.A(v) = a { println(v); }
        // 2. `while let`, consuming, source LIVE — clone per evaluation,
        //    freed on the miss edge at loop exit.
        let mut c: E = E.A(f"wl{i}");
        while let E.A(v) = c { let k: String = v; println(k); c = E.B; }
        if let E.A(w) = c { println(w); } else { println("wl-empty"); }
        // 3. CONTROL — `if let` with the source DEAD, so no clone may be made.
        let d: E = E.A(f"dif{i}");
        if let E.A(v) = d { let k: String = v; println(k); }
        i = i + 1;
    }
    println("end");
}
"#,
        &[
            "if0", "if0", "wl0", "wl-empty", "dif0", "if1", "if1", "wl1", "wl-empty", "dif1",
            "if2", "if2", "wl2", "wl-empty", "dif2", "if3", "if3", "wl3", "wl-empty", "dif3",
            "end",
        ],
        "live_local_block_spellings_user_enum",
    );
}

/// B-2026-09-07-18 — the ENUM sibling of the box above, on both of its
/// axes, plus the NAMING defect B-2026-09-07-17 introduced while fixing the
/// struct one.
///
/// `karac_drop_<E>` is body-only for an enum name: the wrapper's other two
/// steps are a struct field-bodies walk and `emit_struct_drop_synthesis`,
/// and both resolve to nothing for a name that is not in `struct_types`.
/// So the box needs three fns where a struct needs one — own body, payload
/// bodies, payload memory — which is why widening `-17`'s name lookup to
/// `enum_layouts` would have restored the body and left the memory answer
/// (a STRUCT-layout field walk over a tagged union) freeing nothing.
///
/// The no-`Drop`-anywhere cell is the one that proves the second axis is
/// not just the first one's gate missing an arm: with no user `Drop` in the
/// program at all, the RC-boxed enum still lost its payload.
#[test]
fn asan_rc_fallback_boxed_enum_local_drops_through_its_box() {
    const OWN: &str = "enum E { A(String), B }\n\
             impl Drop for E { fn drop(mut ref self) { println(\"drop E\"); } }\n\
             fn seed() -> i64 { env.args().len() }\n\
             fn payload() -> String { f\"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa\" }\n\
             fn mke() -> E { return E.A(payload()); }\n\
             fn take(e: E) -> i64 { return 1; }\n\
             fn main() { println(go()); }\n";
    // As in the struct fixture, the promotion fires on the CONSUME's
    // presence rather than the trip count, so the never-entered loop is the
    // sharpest cell.
    assert_clean_asan_run_min_allocs(
        &format!(
            "{OWN}fn go() -> i64 {{ let t = mke(); let mut i = 0i64;\n\
                 \x20 while i < 0i64 {{ take(t); i = i + 1; }}\n\
                 \x20 return 1; }}\n"
        ),
        &["drop E", "1"],
        "rc_fb_enum_own_drop_loop_never_entered",
        // 8, measured on both hosts; the 9 was an estimate. See
        // B-2026-09-07-26 and [`asan_alloc_floor`].
        8,
    );
    assert_clean_asan_run_min_allocs(
        &format!(
            "{OWN}fn go() -> i64 {{ let t = mke(); let mut i = 0i64;\n\
                 \x20 while i < 3i64 {{ let k = take(t); i = i + k - k + 1; }}\n\
                 \x20 return 1; }}\n"
        ),
        &["drop E", "1"],
        "rc_fb_enum_own_drop_loop_entered",
        // 8, measured on both hosts; the 9 was an estimate. See
        // B-2026-09-07-26 and [`asan_alloc_floor`].
        8,
    );
    // A `Drop`-bearing STRUCT PAYLOAD under an enum that declares no `Drop`
    // of its own: the payload-bodies walker is the piece that carries it,
    // and it is a different fn from the enum's own wrapper.
    const PAYLOAD: &str = "struct R { s: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop R {self.s.len()}\"); } }\n\
             enum E { A(R), B }\n\
             fn seed() -> i64 { env.args().len() }\n\
             fn payload() -> String { f\"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa\" }\n\
             fn mke() -> E { return E.A(R { s: payload() }); }\n\
             fn take(e: E) -> i64 { return 1; }\n\
             fn main() { println(go()); }\n";
    assert_clean_asan_run_min_allocs(
        &format!(
            "{PAYLOAD}fn go() -> i64 {{ let t = mke(); let mut i = 0i64;\n\
                 \x20 while i < 0i64 {{ take(t); i = i + 1; }}\n\
                 \x20 return 1; }}\n"
        ),
        &["drop R 38", "1"],
        "rc_fb_enum_payload_drop_body",
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
    // BOTH — the enum's own body first, then the payload's, which is the
    // interpreter's order and the one the straight-line call sequence in
    // `register_rc_fallback_box_drop` has to reproduce.
    assert_clean_asan_run_min_allocs(
        "struct R { s: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop R {self.s.len()}\"); } }\n\
             enum E { A(R), B }\n\
             impl Drop for E { fn drop(mut ref self) { println(\"drop E\"); } }\n\
             fn seed() -> i64 { env.args().len() }\n\
             fn payload() -> String { f\"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa\" }\n\
             fn mke() -> E { return E.A(R { s: payload() }); }\n\
             fn take(e: E) -> i64 { return 1; }\n\
             fn go() -> i64 { let t = mke(); let mut i = 0i64;\n\
             \x20 while i < 0i64 { take(t); i = i + 1; }\n\
             \x20 return 1; }\n\
             fn main() { println(go()); }\n",
        &["drop E", "drop R 38", "1"],
        "rc_fb_enum_own_and_payload_drop",
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
    // The second axis on its own: NO user `Drop` anywhere in the program,
    // so nothing here is about a body. The RC-boxed enum still leaked its
    // payload, because the box's memory step was a struct-layout walk.
    assert_clean_asan_run_min_allocs(
        "enum E { A(String), B }\n\
             fn seed() -> i64 { env.args().len() }\n\
             fn payload() -> String { f\"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa\" }\n\
             fn mke() -> E { return E.A(payload()); }\n\
             fn take(e: E) -> i64 { return 1; }\n\
             fn go() -> i64 { let t = mke(); let mut i = 0i64;\n\
             \x20 while i < 0i64 { take(t); i = i + 1; }\n\
             \x20 return 1; }\n\
             fn main() { println(go()); }\n",
        &["1"],
        "rc_fb_enum_no_drop_payload_memory",
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
    // CONTROL — the same enum NOT promoted (no consume, so no loop-of-
    // consume rule). This was already correct and must stay byte-identical.
    assert_clean_asan_run_min_allocs(
        &format!("{OWN}fn go() -> i64 {{ let t = mke(); return 1; }}\n"),
        &["drop E", "1"],
        "rc_fb_enum_unpromoted_control",
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

/// surplus stays a body and never becomes a second free.
#[test]
fn asan_wrapped_param_view_payload_frees_once() {
    assert_clean_asan_run(
        r#"
struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id} {self.name}") } }
enum W { One(R), None2 }
fn take(r: R) -> i64 { let w = W.One(r); 7 }
fn main() {
    let v = take(R { id: 1, name: f"heap-one" });
    println(f"v={v}");
}
"#,
        &["dR1 heap-one", "v=7"],
        "wrapped_param_view_payload",
    );
    assert_clean_asan_run(
        r#"
struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id} {self.name}") } }
enum W2 { Two(R, R), None3 }
fn take(r: R) -> i64 { let w = W2.Two(r, R { id: 2, name: f"fresh" }); 7 }
fn main() {
    let v = take(R { id: 1, name: f"heap-one" });
    println(f"v={v}");
}
"#,
        &["dR2 fresh", "dR1 heap-one", "v=7"],
        "wrapped_param_view_mixed_payloads",
    );
    // B-2026-08-29-24 — the MIXED case above used to read
    // `dR1 heap-one, dR2 fresh, dR1 heap-one`: one walker covered both
    // slots, so B-2026-08-29-19 could only leave it armed. The per-slot
    // mask fixed the count; what this case is here to prove is that it did
    // not move a FREE. Both payloads carry heap, `r`'s buffer is shared
    // between the caller's copy and the wrapped one, and masking a body has
    // no business changing who frees it.
    //
    // The remaining wrap kinds, with heap on both sides of every mixed one.
    // A struct literal and a tuple literal each register their memory
    // separately from the body walk, so a mask that reached the wrong
    // channel would surface here as a leak or a double free rather than as
    // a body count.
    for (label, body, want) in [
        (
            "wrapped_param_view_struct_literal",
            "let s = S { r: r };",
            vec!["dR1 heap-one", "v=7"],
        ),
        (
            "wrapped_param_view_tuple_literal",
            "let t = (r, 5);",
            vec!["dR1 heap-one", "v=7"],
        ),
        (
            "wrapped_param_view_option",
            "let q = Some(r);",
            vec!["dR1 heap-one", "v=7"],
        ),
        (
            "wrapped_param_view_mixed_struct",
            "let s = S3 { a: r, b: R { id: 2, name: f\"fresh\" } };",
            vec!["dR2 fresh", "dR1 heap-one", "v=7"],
        ),
        (
            "wrapped_param_view_mixed_tuple",
            "let t = (r, R { id: 2, name: f\"fresh\" });",
            vec!["dR2 fresh", "dR1 heap-one", "v=7"],
        ),
    ] {
        assert_clean_asan_run(
            &format!(
                r#"
struct R {{ id: i64, name: String }}
impl Drop for R {{ fn drop(mut ref self) {{ println(f"dR{{self.id}} {{self.name}}") }} }}
struct S {{ r: R }}
struct S3 {{ a: R, b: R }}
fn take(r: R) -> i64 {{ {body} 7 }}
fn main() {{
    let v = take(R {{ id: 1, name: f"heap-one" }});
    println(f"v={{v}}");
}}
"#
            ),
            &want,
            label,
        );
    }
}

/// B-2026-08-09-16 — a `let` that moves an arm payload taken off an OWNED
/// ENUM PARAM must retract the source's memory action, not only its body.
///
/// `let k: Res = r;` suppressed the source's `UserDrop` and stopped there,
/// which is enough only when the source's memory rides that action's
/// `karac_drop_<T>` wrapper. Under the owned-param gate the wrapper is
/// withheld (the caller runs the body) and the binding carries a plain
/// `StructDrop` instead, so the free survived the move: the callee freed the
/// payload and the caller's result binding freed it again.
///
/// Cases 1 and 2 are the defect, free-function and method. Case 3 is the
/// direct `return r;` spelling, which was already clean because the
/// tail-return path cap-zeroes the returned slot — it is here so a fix that
/// over-retracts turns it into a leak LSan reports rather than a silent
/// change. Case 4 keeps the payload inside the callee, where the binding is
/// the sole owner and the free must still happen.
///
/// Case 4's `Drop` body printed TWICE until B-2026-08-29-17, and this pin
/// was explicitly "observed rather than preferred, so a later change to that
/// shape shows up here instead of passing silently". It did, and the change
/// was the right direction, so the expectation is now ONE body.
///
/// The reading that justified two — "the callee's param is entry-copied, so
/// caller and callee hold genuinely distinct buffers, and each buffer runs
/// one body" — is refuted by the free-function ORACLE, the same test this
/// file uses elsewhere to decide questions like it. Strip the enum wrapper
/// and nothing else changes about the ownership:
///
///     fn take_keep(p: Res) -> i64 { let k: Res = p; k.id }
///
/// Same owned param, same entry copy, same heap payload, same rebind, same
/// non-escape — and it printed ONE body before B-2026-08-29-17 and one
/// after, on every backend. If entry-copying really implied two bodies, the
/// oracle would print two. It never has. What differed was only that the
/// PAYLOAD spelling failed to propagate the param's view-ness to the rebind,
/// which is the defect that row fixed.
///
/// ASAN is clean either way, and that is worth stating: the frees were
/// balanced before and are balanced now, so this line was never a memory
/// question. Only the body count moved.
///
/// Every payload is read BYTE-WISE, and it is what makes the bug visible at
/// all: with a `Drop` body that never touches `name`, the optimizer deletes
/// the allocation together with both of its frees and the double free
/// disappears from an ordinary `-O2` run.
#[test]
fn asan_let_aliased_enum_param_payload_frees_once() {
    assert_clean_asan_run(
        r#"
struct Res { id: i64, name: String }
impl Drop for Res { fn drop(mut ref self) { println(f"drop {self.id} {self.name}") } }
enum Box2 { Full(Res), Empty }
struct Taker { tag: i64 }
fn take_alias(b: Box2) -> Res {
    match b {
        Box2.Full(r) => { let k: Res = r; return k; }
        Box2.Empty => { return Res { id: 0, name: f"z" }; }
    }
}
fn take_direct(b: Box2) -> Res {
    match b {
        Box2.Full(r) => { return r; }
        Box2.Empty => { return Res { id: 0, name: f"z" }; }
    }
}
fn take_keep(b: Box2) -> i64 {
    match b {
        Box2.Full(r) => { let k: Res = r; return k.id; }
        Box2.Empty => { return 0i64; }
    }
}
impl Taker {
    fn take(ref self, b: Box2) -> Res {
        match b {
            Box2.Full(r) => { let k: Res = r; return k; }
            Box2.Empty => { return Res { id: 0, name: f"z" }; }
        }
    }
}
fn main() {
    let t: Taker = Taker { tag: 1i64 };
    let mut i: i64 = 0i64;
    while i < 3i64 {
        // 1. The row's shape — free function, payload aliased by a `let`.
        let a: Box2 = Box2.Full(Res { id: i, name: f"al{i}" });
        let ra: Res = take_alias(a);
        println(ra.name);
        // 2. Same through an impl method, which reaches a different arg loop.
        let b: Box2 = Box2.Full(Res { id: i, name: f"me{i}" });
        let rb: Res = t.take(b);
        println(rb.name);
        // 3. CONTROL — direct `return r;`, already clean; must not become a leak.
        let c: Box2 = Box2.Full(Res { id: i, name: f"di{i}" });
        let rc: Res = take_direct(c);
        println(rc.name);
        // 4. CONTROL — the alias never leaves the callee, so it owns the free.
        let d: Box2 = Box2.Full(Res { id: i, name: f"kp{i}" });
        let v: i64 = take_keep(d);
        println(f"kp {v}");
        i = i + 1;
    }
    println("end");
}
"#,
        &[
            "al0",
            "drop 0 al0",
            "me0",
            "drop 0 me0",
            "di0",
            "drop 0 di0",
            "drop 0 kp0",
            "kp 0",
            "al1",
            "drop 1 al1",
            "me1",
            "drop 1 me1",
            "di1",
            "drop 1 di1",
            "drop 1 kp1",
            "kp 1",
            "al2",
            "drop 2 al2",
            "me2",
            "drop 2 me2",
            "di2",
            "drop 2 di2",
            "drop 2 kp2",
            "kp 2",
            "end",
        ],
        "let_aliased_enum_param_payload",
    );
}

/// The `shared enum` sibling — a different heap layout through the same
/// retain-based transfer.
#[test]
fn asan_loop_break_shared_enum_single_owner() {
    assert_clean_asan_run_min_allocs(
        "shared enum Tree { Leaf(i64), Node(i64, i64) }\n\
             fn pick() -> Tree {\n\
             \x20   let mut i: i64 = env.args().len() - 1;\n\
             \x20   loop {\n\
             \x20       i = i + 1;\n\
             \x20       let t = Tree.Node(i, i * 2);\n\
             \x20       if i == 2 { break t }\n\
             \x20   }\n\
             }\n\
             fn main() { match pick() { Leaf(a) => println(a), Node(a, b) => println(a + b) } }\n",
        &["6"],
        "loop-break-shared-enum",
        4,
    );
}

/// The `shared enum` rvalue sibling. A variant construction parses as a
/// CALL, so it reaches the fix through a different arm than the struct
/// literal above while landing on the same no-op transfer.
#[test]
fn asan_loop_break_shared_enum_rvalue_single_owner() {
    assert_clean_asan_run_min_allocs(
        "shared enum Tree { Leaf(i64), Node(i64, i64) }\n\
             fn pick() -> Tree {\n\
             \x20   let mut i: i64 = env.args().len() - 1;\n\
             \x20   loop {\n\
             \x20       i = i + 1;\n\
             \x20       let scratch = Tree.Leaf(i);\n\
             \x20       let stop = match scratch { Leaf(a) => a == 2, Node(a, b) => a == b };\n\
             \x20       if stop { break Tree.Node(i, i * 2) }\n\
             \x20   }\n\
             }\n\
             fn main() { match pick() { Leaf(a) => println(a), Node(a, b) => println(a + b) } }\n",
        &["6"],
        "loop-break-shared-enum-rvalue",
        4,
    );
}

/// B-2026-08-28-74 (leak 1) — a `match` / `if let` / `let…else` /
/// `while let` over a FRESH `shared` enum TEMPORARY never released the RC
/// box, so the box and everything it owned leaked.
///
/// The two fresh-temp trackers that would have owned it both declined: the
/// value-enum one (`materialize_freshtemp_enum_scrutinee`) fails its
/// `StructValue` gate because a shared enum's value is the box POINTER,
/// and the `Option[shared T]` one only recognizes an `Option` wrapper.
/// `track_freshtemp_shared_enum_scrutinee` is the missing bare sibling.
///
/// THE FIXTURE MUST BE A RECURSIVE ENUM BUILT RECURSIVELY, and that is the
/// whole reason this shape was chosen over the one-line `match mk() { … }`
/// the bug reproduces on. This suite compiles at **-O2** (`read_opt_level_env`
/// defaults to `2`), and at -O2 LLVM promotes a never-freed, non-escaping
/// box out of existence: measured pre-fix, the flat shapes drop from 9/10/11
/// allocations to 8 and report NO leak, while only this recursive one keeps
/// its allocations and leaks 32 B directly + 192 B indirectly (15 allocs,
/// 8 frees). A flatter fixture here would pass whether or not the bug is
/// present. `min_allocs` is the second half of that guard.
///
/// The BOUND spellings are the oracle rows: `let t = mk(…); match t { … }`
/// was already clean on every payload shape, and the fix's whole claim is
/// that the unnamed temporary now frees identically.
#[test]
fn asan_freshtemp_shared_enum_scrutinee_releases_its_box() {
    // Recursive shared enum + recursive builder — the one shape -O2 cannot
    // optimize the leak away in. Each row is (source, expected, label).
    let rows: [(&str, &str, &str); 7] = [
        // The discarded fresh temp, in each construct that takes a scrutinee.
        (
            "match mk(3) { Num(a) => println(a), Bin(l, r) => println(7) }",
            "7",
            "freshtemp-shared-enum-match",
        ),
        (
            "if let Bin(l, r) = mk(3) { println(7) } else { println(0) }",
            "7",
            "freshtemp-shared-enum-iflet",
        ),
        (
            "let Bin(l, r) = mk(3) else { return; }; println(7);",
            "7",
            "freshtemp-shared-enum-letelse",
        ),
        // An arm that binds NOTHING: the box's tag-switched drop walk owns
        // the whole payload, so this is the wholesale-free edge.
        (
            "match mk(3) { Num(a) => println(a), Bin(_, _) => println(7) }",
            "7",
            "freshtemp-shared-enum-unbound",
        ),
        // The BOUND oracle: already clean pre-fix, and must stay clean —
        // this is the row that would fail if the new dec double-freed.
        (
            "let t = mk(3); match t { Num(a) => println(a), Bin(l, r) => println(7) }",
            "7",
            "freshtemp-shared-enum-bound-oracle",
        ),
        // A PLACE scrutinee reached through a second binding — the tracker
        // must keep declining here, or the box is released early and the
        // later read is a use-after-free ASAN would report.
        (
            "let t = mk(3); let u = t; match u { Num(a) => println(a), Bin(l, r) => println(7) }",
            "7",
            "freshtemp-shared-enum-place-declines",
        ),
        // `while let` registers in the PER-ITERATION body frame rather than
        // the enclosing one, and a `break` leaves that frame by a different
        // edge than falling off the end — so this row is the one that would
        // catch a registration the break path drains past. A fresh temp is
        // loop-invariant, so the `break` is what makes the shape expressible
        // at all.
        (
            "while let Bin(l, r) = mk(3) { println(7); break; }",
            "7",
            "freshtemp-shared-enum-whilelet-break",
        ),
    ];
    for (body, expected, label) in rows {
        let src = format!(
                "shared enum E {{ Num(i64), Bin(E, E) }}\n\
                 fn mk(n: i64) -> E {{ if n <= 0 {{ return E.Num(n); }} return E.Bin(mk(n - 1), E.Num(n)); }}\n\
                 fn main() {{ {body} }}\n"
            );
        // 7, measured — the recursive spine's boxes. Was 8, an estimate
        // one above what every row actually allocates; nothing could catch
        // that while the predicate compared raw counts against a host floor
        // of 10 (B-2026-09-07-26).
        assert_clean_asan_run_min_allocs(&src, &[expected], label, 7);
    }
}

/// The payload-shape matrix for the fix above, on the shape that actually
/// exercises the move-out interaction: a heap payload an arm either TAKES
/// or LEAVES.
///
/// Taking it and freeing the box are the two halves that must not overlap.
/// They do not, and the reason is that `suppress_shared_enum_payload_move_out`
/// already fires per-arm for any pointer-valued scrutinee — independent of
/// the new registration — zeroing the consumed field's words in the box so
/// the drop walk skips what the binding now owns. Leaving it unbound is the
/// other side: the walk must free it, or the payload leaks under the box.
///
/// Both rows carry the recursive spine for the -O2 reason above; the
/// `String` rides on the outer node.
#[test]
fn asan_freshtemp_shared_enum_payload_moveout_is_balanced() {
    let rows: [(&str, &str, &str); 2] = [
        (
            "match mk(3) { Num(a) => println(a), Tag(s, l) => println(s) }",
            "tag-0123456789abcdef",
            "freshtemp-shared-enum-payload-taken",
        ),
        (
            "match mk(3) { Num(a) => println(a), Tag(_, _) => println(7) }",
            "7",
            "freshtemp-shared-enum-payload-left",
        ),
    ];
    for (body, expected, label) in rows {
        let src = format!(
                "shared enum E {{ Num(i64), Tag(String, E) }}\n\
                 fn lbl() -> String {{ let a = \"tag-\"; let b = \"0123456789abcdef\"; return a + b; }}\n\
                 fn mk(n: i64) -> E {{ if n <= 0 {{ return E.Num(n); }} return E.Tag(lbl(), mk(n - 1)); }}\n\
                 fn main() {{ {body} }}\n"
            );
        // 7, measured — see the sibling table above.
        assert_clean_asan_run_min_allocs(&src, &[expected], label, 7);
    }
}

/// B-2026-09-01-39 — the MEMORY half: a live enum local handed out of a
/// discarded branch, or discarded directly, now runs its payload body from
/// the discard site (interpreter) or its own walker (codegen) instead of
/// losing it; every payload here carries a `String` so that a second owner
/// would be a double free and a lost one a leak, not just a miscount.
#[test]
fn asan_discarded_live_enum_local_payload_is_memory_balanced() {
    assert_clean_asan_run(
        r#"struct S { id: i64, name: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
enum E3 { A(S), B }
impl Drop for E3 { fn drop(mut ref self) { println("dE3") } }
enum E4 { A(S), B }
fn mk(n: i64) -> S { return S { id: n, name: f"nm-{n}-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" }; }
fn if_let(c: bool) { let e = E3.A(mk(1)); let _ = if c { E3.A(mk(8)) } else { e }; println("mid") }
fn if_let4(c: bool) { let e = E4.A(mk(2)); let _ = if c { E4.A(mk(8)) } else { e }; println("mid") }
fn match_let(c: bool) { let e = E3.A(mk(3)); let _ = match c { true => E3.A(mk(8)), _ => e }; println("mid") }
fn direct_let() { let e = E3.A(mk(4)); let _ = e; println("mid") }
fn direct_let4() { let e = E4.A(mk(5)); let _ = e; println("mid") }
fn if_bare(c: bool) { let e = E3.A(mk(6)); if c { E3.A(mk(8)) } else { e }; println("mid") }
fn main() {
    let base: i64 = env.args().len();
    if_let(base == 0); if_let(base == 1); if_let4(base == 0); match_let(base == 0); direct_let(); direct_let4(); if_bare(base == 0);
    println("end");
}"#,
        &[
            "dE3", "dS1", "mid", "dE3", "dS8", "dE3", "dS1", "mid", "dS2", "mid", "dE3", "dS3",
            "mid", "dE3", "dS4", "mid", "dS5", "mid", "dE3", "dS6", "mid", "end",
        ],
        "discarded-live-enum-local-payload",
    );
}

/// B-2026-08-29-4 — a METHOD that hands an inline `Option`/`Result`
/// argument back ALIASES the source binding's payload, so the result must
/// be recorded as an alias rather than tracked as a fresh owner.
///
/// Pre-fix both bindings freed the SAME buffer once the result was
/// consumed: `free(): double free detected in tcache 2` on all three
/// compiled backends, valgrind `Invalid free()`, 10 allocs against 11
/// frees — while the interpreter and the free-function twin (10 allocs, 10
/// frees) were correct. The equal ALLOCATION count is what shows the two
/// bindings genuinely alias rather than one entry-copying: nothing here
/// makes a second buffer, so exactly one of them may own it.
///
/// The failure is a SIGNAL, not an output mismatch, so a revert kills the
/// process rather than printing wrong bytes.
///
/// SCOPE, deliberately: every case binds the result with `let` first. The
/// INLINE-scrutinee spelling (`match b.take(s) { .. }`) double-frees too,
/// but it does so for a FREE FUNCTION as well, so it is a different and
/// wider defect — filed separately rather than folded in here. Writing this
/// fixture in the inline form is what surfaced that, after each shape had
/// passed individually in the `let`-bound form.
///
/// Payloads are built with f-strings carrying an interpolation, not
/// literals: a literal is rodata with `cap == 0` and is never freed, so it
/// cannot witness a double free. That is not a stylistic note — an earlier
/// pass of this investigation drew a wrong conclusion from a `f"heap-1"`
/// oracle that silently tested nothing.
#[test]
fn asan_method_passthrough_arg_aliases_the_source_payload() {
    assert_clean_asan_run(
        r#"
struct Bx { n: i64 }
impl Bx {
    fn take(ref self, o: Option[String]) -> Option[String] { o }
    fn take_mut(mut ref self, o: Option[String]) -> Option[String] { self.n = self.n + 1; o }
    fn take_res(ref self, o: Result[String, i64]) -> Result[String, i64] { o }
    fn second(ref self, a: Option[String], b: Option[String]) -> Option[String] { b }
    fn first(ref self, a: Option[String], b: Option[String]) -> Option[String] { a }
    fn fresh(ref self, o: Option[String]) -> Option[String] { Option.Some(f"fresh-{self.n}") }
}
fn main() {
    let base: i64 = env.args().len();
    let b = Bx { n: base };
    let s1 = Option.Some(f"a{base}");
    let o1 = b.take(s1);
    match o1 { Option.Some(v) => { println(f"got {v}"); } Option.None => { println("none"); } }
    let mut c = Bx { n: base };
    let s2 = Option.Some(f"b{base}");
    let o2 = c.take_mut(s2);
    match o2 { Option.Some(v) => { println(f"got {v}"); } Option.None => { println("none"); } }
    let s3: Result[String, i64] = Result.Ok(f"c{base}");
    let o3 = b.take_res(s3);
    match o3 { Result.Ok(v) => { println(f"got {v}"); } Result.Err(e) => { println(f"err {e}"); } }
    let p = Option.Some(f"d{base}");
    let q = Option.Some(f"e{base}");
    let o4 = b.second(p, q);
    match o4 { Option.Some(v) => { println(f"got {v}"); } Option.None => { println("none"); } }
    let r1 = Option.Some(f"f{base}");
    let r2 = Option.Some(f"g{base}");
    let o5 = b.first(r1, r2);
    match o5 { Option.Some(v) => { println(f"got {v}"); } Option.None => { println("none"); } }
    // CONTROL — the method does NOT hand the argument back, so the result owns
    // a freshly produced payload and the source owns its own. No alias, and the
    // fix must not record one.
    let s4 = Option.Some(f"h{base}");
    let o6 = b.fresh(s4);
    match o6 { Option.Some(v) => { println(f"got {v}"); } Option.None => { println("none"); } }
    println("end");
}
"#,
        &[
            "got a1",
            "got b1",
            "got c1",
            "got e1",
            "got f1",
            "got fresh-1",
            "end",
        ],
        "method-passthrough-arg-alias",
    );
}

/// B-2026-08-30-15, the MEMORY half — the part the row that filed it did not
/// see. A fresh-temp struct scrutinee had no owner at all, so an arm that
/// binds nothing leaked the payload as well as losing both `Drop` bodies:
/// `match mkS() { S { r: _ } => … }` over a one-`String` payload measured 9
/// allocs / 8 frees, 15 B definitely lost (valgrind, `KARAC_OPT_LEVEL=0`);
/// post-fix 10 / 10, clean. The row saw only the DESTRUCTURING arm, where
/// the moved-out binding happens to own the field's buffer — which is
/// exactly why the leak hid behind it.
///
/// WHAT EACH LEG ACTUALLY CATCHES, measured against the pre-fix compiler
/// rather than assumed from the loop: at `KARAC_OPT_LEVEL=0` it fails as a
/// LEAK — "60 byte(s) leaked in 4 allocation(s)" — and at the default `-O2`
/// it fails on the OUTPUT only, the bodies being absent. So the memory half
/// rides on the `-O0` leg (`scripts/asan-o0-leg.sh`) like its neighbours,
/// and the loop's contribution is four independent payloads in the leak
/// count rather than -O2 coverage it does not buy.
///
/// The ORDER is asserted, not just the counts, because the whole defect was
/// a missing side effect: `dS` before `dR[…]` is the interpreter's sequence
/// and all four surfaces agree on it after the fix.
#[test]
fn asan_freshtemp_struct_scrutinee_frees_its_payload() {
    assert_clean_asan_run(
        r#"
struct R { s: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR[{self.s}]"); } }
struct S { r: R }
impl Drop for S { fn drop(mut ref self) { println("dS"); } }
fn mkS(n: i64) -> S { return S { r: R { s: f"payloadpayload{n}" } }; }

fn main() {
    let mut i = 0;
    while i < 3 {
        match mkS(i) { S { r: _ } => { println(f"w{i}"); } }
        i = i + 1;
    }
    if let S { r: _ } = mkS(9) { println("g"); }
    println("done");
}
"#,
        &[
            "w0",
            "dS",
            "dR[payloadpayload0]",
            "w1",
            "dS",
            "dR[payloadpayload1]",
            "w2",
            "dS",
            "dR[payloadpayload2]",
            "g",
            "dS",
            "dR[payloadpayload9]",
            "done",
        ],
        "b30-15-freshtemp-struct-scrutinee-payload",
    );
}

/// B-2026-09-01-35 — the caller owns a fresh `Option`/`Result` temp
/// argument exactly when the callee provably does NOT let it escape.
///
/// `track_optres_arg_temp` gave the caller ownership whenever the param's
/// TYPE admitted an entry copy. The callee emits that copy only for a param
/// in `by_value_nonescaping_param_names`, so the two disagreed in both
/// directions at once: the caller believing a copy was made when it was not
/// is a LEAK, and believing one was not made when it was is a DOUBLE FREE.
/// Both sides now read the same analysis, so they agree by construction.
///
/// THE THREE ESCAPE ROUTES ARE ALL HERE, because each was reachable through
/// a different door and only one of them had a guard:
///
///   * RETURNED BARE — `call_arg_flows_into_return` already covered it.
///   * STORED through a `mut ref` param — B-2026-09-01-29's syntactic gate
///     covered it, and this replaces that gate.
///   * RETURNED INSIDE AN AGGREGATE (`v.push(x); return Bag { xs: v }`) —
///     covered by NEITHER, and a live double-free abort on `main` in the
///     assoc-fn spelling. The free-fn and method spellings were clean only
///     because the payload spelling that reached the ownership was narrow;
///     widening it for the leak half made all three abort, which is why the
///     two halves had to land together.
///
/// THE LEAK HALF IS THE OTHER SIX ROWS. `show(Some(vs))` and
/// `show(Some(vs.clone()))` leaked 333 B / 336 B in 6 allocations because
/// `optres_arg_is_unowned_temp` recursed into the constructor's payload and
/// called an identifier "owned elsewhere" — false at a call site, where the
/// constructor MOVES it in and disarms the binding. The `.clone()` row is
/// the one that shows this was the wrong QUESTION rather than an incomplete
/// answer: that result is unambiguously a fresh temp, rejected for being
/// spelled as a `MethodCall`.
///
/// The CONTROLS are load-bearing in both directions: a NAMED binding by
/// value was always clean and must stay so (owning it here would be the
/// double free the old recursion was avoiding), and a manufactured payload
/// was the one spelling that always worked.
/// B-2026-08-31-38 — THE RESTORED `Drop` BODY MUST READ LIVE MEMORY.
///
/// The fix stops the `if let` / `while let` legs retracting the source's
/// payload-BODIES walk for a binding that does not own the payload, so
/// that walk runs the body again on shapes where it had been silenced.
/// The body dereferences `self.s`, so the question this fixture asks is
/// not "does it print" — `tests/codegen.rs` asks that — but whether the
/// buffer it reads is still live, and whether adding the body back
/// introduces a second free anywhere.
///
/// The bodies are deliberately CHATTY here rather than silent: reading
/// `self.s.len()` is what makes a use-after-free land inside the body,
/// which is the failure this fix could plausibly have caused. `pad()`
/// gives every payload a real heap buffer, and the loop reallocates after
/// each pass so a freed block gets reused rather than sitting quietly
/// unreclaimed.
#[test]
fn asan_restored_let_family_payload_body_reads_live_memory() {
    assert_clean_asan_run(
        r#"
fn pad(t: i64) -> String {
    let mut s: String = String.new();
    s.push_str("payload-padded-out-well-past-thirty-six-bytes-");
    s.push_str(f"{t}");
    return s;
}
struct R { s: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.s.len()}"); } }
struct H { r: R, n: i64 }
fn main() {
    let mut i = 0;
    while i < 3 {
        let o: Option[H] = Some(H { r: R { s: pad(i) }, n: 4 });
        if let Some(H { r, .. }) = o { println(f"{r.s.len()}") }
        let k: Option[H] = Some(H { r: R { s: pad(i) }, n: 5 });
        if let Some(H { r, n }) = k { println(f"{r.s.len()}{n}") }
        let e: Result[H, i64] = Ok(H { r: R { s: pad(i) }, n: 6 });
        if let Ok(H { r, .. }) = e { println(f"{r.s.len()}") }
        let mut q: Option[H] = Some(H { r: R { s: pad(i) }, n: 7 });
        while let Some(H { r, .. }) = q {
            println(f"{r.s.len()}");
            q = None;
        }
        i = i + 1;
    }
    println("done");
}
"#,
        &[
            "47", "dR47", "475", "dR47", "47", "dR47", "47", "dR47", "47", "dR47", "475", "dR47",
            "47", "dR47", "47", "dR47", "47", "dR47", "475", "dR47", "47", "dR47", "47", "dR47",
            "done",
        ],
        "b38-let-family-payload-body-live",
    );
}

/// B-2026-09-07-16 — the memory half of
/// `test_e2e_by_value_enum_param_with_owning_struct_payload_transfers`.
///
/// One free per buffer across all ten call shapes and all four payload
/// classes. The E2E twin pins the OUTPUT, which a double free does not
/// always disturb — `X3`'s `-O2` cell aborted while printing the right
/// answer first — so the abort-and-leak surface needs its own gate; on
/// Linux CI this is also the LSan gate for the class.
///
/// RED pre-fix on every non-control cell, in three different ways:
/// `free(): double free detected in tcache 2` (`X1`, `Xd`, and `X3` at
/// `-O2`), and a SEGV (`X3` at `-O0`, and the inline `M` at both levels,
/// whose valgrind trace is four invalid reads inside
/// `karac_map_free_with_drop_vec` over a 72-byte block freed twice).
///
/// FLOOR, not a ceiling: the payloads are the point. Every cell allocates
/// a `String` or a `Map` the callee must end up owning exactly once, and a
/// fixture whose payloads the optimizer folded away would pass this
/// vacuously — the callees are empty, which is exactly the shape LLVM
/// likes to delete.
#[test]
fn asan_by_value_enum_param_with_owning_struct_payload_transfers() {
    assert_clean_asan_run_min_allocs(
            "struct X1 { a: Option[i64], s: String }\n\
struct X3 { a: Option[i64], m: Map[i64, String] }\n\
struct M  { m: Map[i64, String], n: i64 }\n\
struct R2 { id: i64, s: String }\n\
impl Drop for R2 { fn drop(mut ref self) { println(f\"dR2{self.id}\") } }\n\
struct Xd { a: Option[i64], r: R2 }\n\
struct Ctl { s: String }\n\
impl Drop for Ctl { fn drop(mut ref self) { println(f\"dC{self.s}\") } }\n\
enum W  { T(X1), U(i64) }\n\
enum V  { T(X3), U(i64) }\n\
enum Y  { T(M),  U(i64) }\n\
enum D  { T(Xd), U(i64) }\n\
enum C  { T(Ctl), U(i64) }\n\
struct H { n: i64 }\n\
fn sink(w: W) {}\n\
fn eat(w: W) -> i64 { return match w { W.T(x) => x.a.unwrap_or(0), W.U(n) => n } }\n\
fn hand(w: W) -> W { return w; }\n\
fn hop(w: W) { sink(w); }\n\
fn sinkv(v: V) {}\n\
fn sinky(y: Y) {}\n\
fn sinkd(d: D) {}\n\
fn sinkc(c: C) {}\n\
impl H { fn pv(ref self, w: W) {} }\n\
impl W { fn av(w: W) {} }\n\
fn mkx(i: i64) -> X1 { return X1 { a: Option.Some(i), s: f\"s{i}\" }; }\n\
fn mkm(i: i64) -> Map[i64, String] { let mut m: Map[i64, String] = Map.new(); m.insert(i, f\"v{i}\"); return m; }\n\
fn main() {\n\
  // boxed, owns heap -- fresh temp / named local / match-consuming / hand-back / two-hop\n\
  sink(W.T(mkx(1)));                       println(\"c1\")\n\
  let a = W.T(mkx(2)); sink(a);            println(\"c2\")\n\
  println(f\"c3={eat(W.T(mkx(3)))}\")\n\
  let b = W.T(mkx(4)); println(f\"c4={eat(b)}\")\n\
  let z = hand(W.T(mkx(5))); println(f\"c5={eat(z)}\")\n\
  hop(W.T(mkx(6)));                        println(\"c6\")\n\
  // method and assoc, fresh temp and named local (the assoc named-local leg)\n\
  let h = H { n: 1 };\n\
  h.pv(W.T(mkx(7)));                       println(\"c7\")\n\
  let c = W.T(mkx(8)); h.pv(c);            println(\"c8\")\n\
  W.av(W.T(mkx(9)));                       println(\"c9\")\n\
  let d = W.T(mkx(10)); W.av(d);           println(\"c10\")\n\
  // boxed with a Map field, and the INLINE Map payload\n\
  sinkv(V.T(X3 { a: Option.Some(11), m: mkm(11) })); println(\"c11\")\n\
  let e = V.T(X3 { a: Option.Some(12), m: mkm(12) }); sinkv(e); println(\"c12\")\n\
  sinky(Y.T(M { m: mkm(13), n: 13 }));     println(\"c13\")\n\
  let f = Y.T(M { m: mkm(14), n: 14 }); sinky(f); println(\"c14\")\n\
  // a Drop-bearing payload field that owns heap -- the body must fire exactly once\n\
  sinkd(D.T(Xd { a: Option.Some(15), r: R2 { id: 15, s: \"h15\" } })); println(\"c15\")\n\
  let g = D.T(Xd { a: Option.Some(16), r: R2 { id: 16, s: \"h16\" } }); sinkd(g); println(\"c16\")\n\
  // CONTROL: a copy-supported (NestedStruct) payload stays entry-copied\n\
  sinkc(C.T(Ctl { s: \"17\" }));             println(\"c17\")\n\
  let i = C.T(Ctl { s: \"18\" }); sinkc(i);  println(\"c18\")\n\
  println(\"end\")\n\
}\n\
",
            &[
                "c1", "c2", "c3=3", "c4=4", "c5=5", "c6", "c7", "c8", "c9", "c10", "c11", "c12",
                "c13", "c14", "dR215", "c15", "dR216", "c16", "dC17", "c17", "dC18", "c18", "end",
            ],
            "by_value_enum_param_owning_struct_payload_transfers",
            26,
        );
}

/// B-2026-09-05-26 — the memory half of
/// `e2e_user_enum_struct_payload_owns_its_heap`: every cell is one free
/// per buffer (valgrind: every block freed at -O0 and -O2), including the
/// boxed `Ho2` payload's envelope on the unbound, `_`-arm, bound and
/// destructured paths. Before the fix the `Two` cells lost 22 B each and
/// the `Ho2` cells 88 B each, and LSan on Linux CI is the gate for this.
#[test]
fn asan_user_enum_struct_payload_owns_its_heap_clean() {
    let label = "user_enum_struct_payload_owns_its_heap";
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
enum Wrap { W(Ho2), T(Two), N }
enum E { V(R), N }
fn mk(k: i64) -> R { return R { id: k, tag: f"t{k}", xs: [k] } }
fn main() {
    { let w: Wrap = Wrap.T(Two { a: mk(1), b: mk(101) }); println("one") }
    { let w: Wrap = Wrap.T(Two { a: mk(2), b: mk(102) }); match w { _ => println("n") } println("two") }
    { let w: Wrap = Wrap.T(Two { a: mk(3), b: mk(103) }); match w { Wrap.T(h) => { println("in") }, _ => println("n") } println("three") }
    { let w: Wrap = Wrap.W(Ho2 { a: mk(4), b: Option.Some(mk(104)) }); println("four") }
    { let w: Wrap = Wrap.W(Ho2 { a: mk(5), b: Option.Some(mk(105)) }); match w { _ => println("n") } println("five") }
    { let w: Wrap = Wrap.W(Ho2 { a: mk(6), b: Option.Some(mk(106)) }); match w { Wrap.W(h) => { println("in") }, _ => println("n") } println("six") }
    { let w: Wrap = Wrap.W(Ho2 { a: mk(7), b: Option.Some(mk(107)) }); match w { Wrap.W(h) => { let Ho2 { a, b } = h; println("in") }, _ => println("n") } println("seven") }
    { let w: Wrap = Wrap.W(Ho2 { a: mk(8), b: Option.None }); match w { Wrap.W(h) => { println("in") }, _ => println("n") } println("eight") }
    { let e: E = E.V(mk(9)); println("nine") }
    { let w: Wrap = Wrap.N; match w { Wrap.W(h) => { println("in") }, _ => println("n") } println("ten") }
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
            "dR101", "dR1", "one", "n", "dR102", "dR2", "two", "in", "dR103", "dR3", "three",
            "dR104", "dR4", "four", "n", "dR105", "dR5", "five", "in", "dR106", "dR6", "six",
            "dR107", "dR7", "in", "seven", "in", "dR8", "eight", "dR9", "nine", "n", "ten", "end",
        ],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// B-2026-09-06-4 — a by-value enum param whose struct payload was heap
/// BOXED must copy the BOX, not just what is inside it.
///
/// `payload_word_count_for_type_expr` sizes an `Option` / `Result` / enum
/// FIELD at one word against a real LLVM width of four or six, so any
/// payload struct carrying one is under-sized and `coerce_to_payload_words`
/// heap-boxes it. The drop switch recomputes that same predicate and frees
/// the envelope; the ENTRY COPY did not, so the callee's payload word kept
/// pointing at the CALLER's box and both frames freed it —
/// `free(): double free detected in tcache 2` at `-O0`, and at `-O2` as well
/// for the copy-supported half (valgrind: two frees of one 40-byte block).
/// This is what aborted `tests/selfhost_resolver.rs`'s two oracles four
/// times per run on glibc: the resolver's `Item.Impl(ImplBlockNode { generics:
/// Option[…], trait_ty: Option[…], … })` reaching `walk_item(item)`.
///
/// Both payload KINDS are here because the entry copy treated them
/// differently and both were wrong:
///
///   * `Sm { a: Option[i64], sp: i64 }` is `NestedOwnedStruct` — the copy
///     paths decline it (an `Option[i64]` field is not copy-supported) and
///     the walk skipped it outright. It owns no drop-heap, so the box IS its
///     whole cleanup and duplicating the envelope is the complete fix.
///   * `S1 { a: Option[String], n: i64 }` is `NestedStruct` — copy-supported,
///     so the walk DID run, but it handed the struct copier a pointer to the
///     box POINTER as though the payload were inline. Envelope plus contents.
///
/// Four call shapes per kind's worth of coverage, because the caller-side
/// registration differs across them and only the callee's prologue is being
/// fixed: a fresh ctor temp (`t1`/`t5`), a NAMED LOCAL (`t2`/`t6`), and a
/// MATCH-CONSUMING callee over both spellings (`t3`/`t4`/`t7`) — the arm
/// binds the payload out, which suppresses the param's own drop, so a fixture
/// with only the pass-through shapes would not notice a copy that duplicated
/// the wrong thing.
///
/// RED pre-fix on the single-statement spellings of `t1`, `t3`, `t4` and
/// `t5`, each measured on its own ten-line program; `--interp` prints the
/// expected output on every one of them, so this is a compiled-backend
/// defect throughout.
#[test]
fn asan_boxed_struct_payload_of_by_value_enum_param_copies_its_box() {
    assert_clean_asan_run(
        r#"
struct Sm { a: Option[i64], sp: i64 }
struct S1 { a: Option[String], n: i64 }
enum It { A(Sm), B(i64) }
enum W1 { T(S1), U(i64) }

fn sink(it: It) -> i64 { return 1 }
fn eat(it: It) -> i64 { return match it { It.A(s) => s.sp, It.B(n) => n } }
fn sinkw(w: W1) -> i64 { return 2 }
fn eatw(w: W1) -> i64 { return match w { W1.T(s) => s.n, W1.U(n) => n } }

fn main() {
    println(f"t1={sink(It.A(Sm { a: None, sp: 2 }))}")
    let a = It.A(Sm { a: None, sp: 3 });
    println(f"t2={sink(a)}")
    println(f"t3={eat(It.A(Sm { a: None, sp: 4 }))}")
    let b = It.A(Sm { a: None, sp: 5 });
    println(f"t4={eat(b)}")
    println(f"t5={sinkw(W1.T(S1 { a: Option.Some("hi"), n: 6 }))}")
    let c = W1.T(S1 { a: Option.Some("ho"), n: 7 });
    println(f"t6={sinkw(c)}")
    println(f"t7={eatw(W1.T(S1 { a: Option.Some("he"), n: 8 }))}")
    println("end")
}
"#,
        &[
            "t1=1", "t2=1", "t3=4", "t4=5", "t5=2", "t6=2", "t7=8", "end",
        ],
        "b4-boxed-enum-struct-payload-box-copy",
    );
}

#[test]
fn asan_transfer_owned_enum_param_rebind_owns_its_payload() {
    const PRE: &str = "struct X1 { a: Option[i64], s: String }\n\
             enum W { T(X1), U(i64) }\n\
             fn mkx(i: i64) -> X1 { return X1 { a: Option.Some(i), s: f\"s{i}\" }; }\n";
    // 1 — the row's cell: the rebind spelling.
    assert_clean_asan_run_no_auto_par(
            &format!(
                "{PRE}fn rebind(w: W) -> i64 {{ let v = w; return match v {{ W.T(x) => x.a.unwrap_or(0), W.U(n) => n }}; }}\n\
                 fn main() {{ println(f\"c={{rebind(W.T(mkx(5)))}}\"); println(\"end\") }}\n"
            ),
            &["c=5", "end"],
            "b41-rebind",
        );
    // 2 — CONTROL: the direct `match w`, clean before and after.
    assert_clean_asan_run_no_auto_par(
            &format!(
                "{PRE}fn rebind(w: W) -> i64 {{ return match w {{ W.T(x) => x.a.unwrap_or(0), W.U(n) => n }}; }}\n\
                 fn main() {{ println(f\"c={{rebind(W.T(mkx(5)))}}\"); println(\"end\") }}\n"
            ),
            &["c=5", "end"],
            "b41-direct-control",
        );
}

/// B-2026-09-07-62 — a BOXED shared-enum payload's NESTED struct field had
/// no owner for its buffers unless that exact field was moved out by a
/// FIELD-ACCESS `let`.
///
/// `emit_shared_enum_field_drop`'s boxed arm walked the nested struct with
/// `nested_buffer_free = Some(false)`, deferring its Vec/String buffers to
/// a move-out owner. That is right in exactly ONE of four spellings —
/// measured, `-O0` and `-O2`, valgrind, on the row's own type:
///
///     no move-out            nobody frees             32 B lost
///     SIBLING move-out       nobody frees             32 B lost
///     destructure move-out   leaf owns a COPY         32 B lost
///     field-access move-out  leaf owns the ORIGINAL   clean
///
/// The walker is now `Some(true)` and the field-access move-out DUPLICATES
/// the nested struct's buffers (`stmts.rs`, B-2026-09-07-55's site), so the
/// box owns its originals on every spelling and each moved-out leaf owns a
/// copy. Flipping the walker alone would have turned the one clean spelling
/// into a double free, which is why cells 3 and 4 are here: on Linux CI
/// LSan pins the leak, and a double free aborts on every platform at any
/// opt level.
///
/// The nested field shapes are the classifier-PARITY cells the row named as
/// its blocker. The caller-side duplicator must classify a nested field the
/// same way the box's rc-dec walker does, or the two disagree and
/// B-2026-09-07-55's use-after-free returns. It does, and not by
/// coincidence: `deep_copy_one_aggregate_field`'s bare-`shared` branch is
/// gated on `shared_heap_type_for_type_expr` and its `Option` arm on
/// `option_inner_shared_type_for_type_expr` — the two predicates
/// `rc_inc_struct_shared_children_in_place` walks — and the latter
/// dispatches to `rc_inc_option_inline_shared_payload_in_place`, the
/// identical call. Cells 5-7 exercise a bare `shared` child, an
/// `Option[shared]` child, and both at once, in the BOXED regime.
///
/// A NOTE ON REACHING THAT REGIME, because it is easy to miss and makes a
/// fixture vacuous: the payload is boxed only when it exceeds the enum's
/// 3-word inline area. A nested struct carrying just a `Vec` and a `String`
/// stays INLINE, takes a different arm entirely, and was clean before and
/// after. Every cell here carries an `Option[String]` field for the sole
/// purpose of forcing the box.
#[test]
fn asan_boxed_shared_enum_nested_struct_field_has_one_owner() {
    // `EXTRA`/`INIT` vary the nested struct's child classes; `pad` forces
    // the payload to be BOXED (see the note above).
    fn prog(extra: &str, init: &str, use_expr: &str) -> String {
        format!(
                "struct Sp4n {{ a: i64, b: i64, c: i64, d: i64 }}\n\
                 shared struct Sh {{ v: i64 }}\n\
                 shared enum E {{ Lit(i64), Iff(IfNode), Blk(Block) }}\n\
                 struct Block {{ stmts: Vec[i64], {extra} pad: Option[String], sp: Sp4n }}\n\
                 struct IfNode {{ cond: E, then_block: Block, sp: Sp4n }}\n\
                 fn mk_block(first: i64, s: i64) -> Block {{\n\
                 \x20   let mut v: Vec[i64] = Vec.new(); v.push(first); v.push(first + 1);\n\
                 \x20   let mut w: Vec[String] = Vec.new(); w.push(f\"t{{first}}\");\n\
                 \x20   return Block {{ stmts: v, {init} pad: Option.Some(f\"p{{first}}\"), sp: Sp4n {{ a: s, b: 0, c: 0, d: 0 }} }};\n\
                 }}\n\
                 fn mk() -> E {{ return E.Iff(IfNode {{ cond: E.Lit(7), then_block: mk_block(20, 2), sp: Sp4n {{ a: 5, b: 0, c: 0, d: 0 }} }}); }}\n\
                 fn main() {{\n\
                 \x20   let ife = mk();\n\
                 \x20   match ife {{ E.Lit(n) => println(f\"{{n}}\"), E.Iff(nd) => {{ {use_expr} }}, E.Blk(_) => println(\"-9\") }}\n\
                 \x20   println(\"end\");\n\
                 }}\n"
            )
    }
    const NONE: &str = "println(f\"v{nd.sp.a}\");";
    const SIB: &str = "let c = nd.cond; println(f\"v{nd.sp.a}\");";
    const FA: &str = "let tb = nd.then_block; println(f\"v{tb.stmts.len()}\");";

    // 1-2 — the row's own cells: no move-out, and a SIBLING move-out.
    for (label, u) in [("b62-no-moveout", NONE), ("b62-sibling-moveout", SIB)] {
        assert_clean_asan_run_no_auto_par(&prog("", "", u), &["v5", "end"], label);
    }
    // 3 — DESTRUCTURE move-out: the leaf already took a copy, so the box's
    //     original was stranded. Same leak, different reason.
    assert_clean_asan_run_no_auto_par(
        &prog(
            "",
            "",
            "let Block { stmts, pad, sp } = nd.then_block; println(f\"v{stmts.len()}\");",
        ),
        &["v2", "end"],
        "b62-destructure-moveout",
    );
    // 4 — HAZARD / the one clean spelling: field-access move-out. Before
    //     this change the leaf ALIASED the box's buffer; it now owns a copy.
    //     A missing copy here is a double free, not a leak.
    assert_clean_asan_run_no_auto_par(&prog("", "", FA), &["v2", "end"], "b62-fieldaccess-moveout");
    // 5-7 — classifier PARITY, in the boxed regime: a bare `shared` child,
    //       an `Option[shared]` child, and both. A duplicator that
    //       classified either differently from the box's rc-dec walker
    //       would double-dec and abort (B-2026-09-07-55's use-after-free).
    for (label, extra, init) in [
        ("b62-parity-bare-shared", "sh: Sh,", "sh: Sh { v: 3 },"),
        (
            "b62-parity-option-shared",
            "osh: Option[Sh],",
            "osh: Option.Some(Sh { v: 4 }),",
        ),
        (
            "b62-parity-both",
            "sh: Sh, osh: Option[Sh],",
            "sh: Sh { v: 3 }, osh: Option.Some(Sh { v: 4 }),",
        ),
    ] {
        for u in [NONE, FA] {
            let want = if u == NONE { "v5" } else { "v2" };
            assert_clean_asan_run_no_auto_par(&prog(extra, init, u), &[want, "end"], label);
        }
    }
    // 8 — the SECOND half of this fix: the walker's `Vec[T]` arm resolved
    //     its per-element drop through `vec_elem_agg_drop_for_type_expr`,
    //     which answers `None` for a DIRECT `String` element, so every
    //     element was dropped on the floor. Pre-existing and level-
    //     INDEPENDENT — reproduced on a `Vec[String]` field of the boxed
    //     payload struct itself, with no nesting involved. It surfaces here
    //     because the field-access copy above stops the moved-out leaf from
    //     incidentally draining the box's originals.
    //
    //     A string LITERAL element hides it (static, `cap == 0`), so these
    //     build their elements with f-strings deliberately.
    for (label, u) in [
        ("b62-vecstring-nested", NONE),
        ("b62-vecstring-nested-fa", FA),
    ] {
        assert_clean_asan_run_no_auto_par(
            &prog("tags: Vec[String],", "tags: w,", u),
            &[if u == NONE { "v5" } else { "v2" }, "end"],
            label,
        );
    }
    assert_clean_asan_run_no_auto_par(
            "struct Sp4n { a: i64, b: i64, c: i64, d: i64 }\n\
             shared enum E { Lit(i64), Iff(IfNode) }\n\
             struct IfNode { cond: E, tags: Vec[String], pad: Option[String], sp: Sp4n }\n\
             fn mk() -> E {\n\
             \x20   let mut w: Vec[String] = Vec.new(); w.push(f\"t{20}\"); w.push(f\"u{21}\");\n\
             \x20   return E.Iff(IfNode { cond: E.Lit(7), tags: w, pad: Option.Some(f\"p{1}\"), sp: Sp4n { a: 5, b: 0, c: 0, d: 0 } });\n\
             }\n\
             fn main() {\n\
             \x20   let ife = mk();\n\
             \x20   match ife { E.Lit(n) => println(f\"{n}\"), E.Iff(nd) => { println(f\"v{nd.sp.a}\"); } }\n\
             \x20   println(\"end\");\n\
             }\n",
            &["v5", "end"],
            "b62-vecstring-direct-field",
        );
}

/// B-2026-09-06-67 — a boxed user *ENUM* payload of a by-value param had no
/// owner for its BOX, on both seeded enums.
///
/// `owned_boxed_option_param_struct` and `owned_boxed_result_param_structs`
/// both filtered the payload name through `struct_types`, so a boxed user
/// enum payload was dropped even though `boxed_enum_payload_variants`
/// admits one. Both spellings leaked identically, which is what said the
/// axis is the payload being an ENUM rather than anything about `Result`'s
/// per-variant boxing.
///
/// THE ENVELOPE ONLY — the registration passes no interior drop for an enum
/// payload, and cell 5 is why. That is not caution; it is the measured
/// difference between two arm shapes:
///
///   `Some(K.A(r))`  nested TupleVariant — nobody owns the interior, and
///                   registering it here is clean (23 allocs / 23 frees).
///   `Some(k)`       whole-payload Binding — the local owns the interior,
///                   and registering it here DOUBLE-FREES: 9 invalid frees
///                   in 3 contexts, 32 frees against 23 allocs.
///
/// WHICH CELLS PIN WHAT, stated because the obvious choice of cell does not
/// work here. The row's own spelling has a `String`-bearing payload whose
/// INTERIOR is still unowned under a nested arm (81 B, filed separately),
/// so it cannot assert clean and is covered for OUTPUT in the
/// `tests/codegen.rs` twin instead. Cells 1-2 use a POD payload: no
/// interior to confound the measurement, so the envelope fix alone takes
/// them from 168 B leaked to clean, on both seeded enums. They are the
/// leak-pinning cells for this row.
///
/// Cell 5 is the hazard that would fail loudly if the interior were ever
/// folded in without the callee-side disarm first.
#[test]
fn asan_boxed_enum_payload_param_owns_its_box() {
    const POD: &str = "struct P4 { a: i64, b: i64, c: i64, d: i64, e: i64, f: i64 }\n\
             enum Kp { A(P4), B }\n\
             fn mkp(i: i64) -> P4 { return P4 { a: i, b: 0, c: 0, d: 0, e: 0, f: 0 }; }\n";
    // 1-2 — THE LEAK, on both seeded enums. 168 B in 3 blocks each, parent.
    assert_clean_asan_run_no_auto_par(
            &format!(
                "{POD}fn show(x: Option[Kp]) {{ match x {{ Option.Some(Kp.A(p)) => {{ println(f\"a:{{p.a}}\"); }}, Option.Some(Kp.B) => {{}}, Option.None => {{}} }} }}\n\
                 fn main() {{ let mut i = 0; while i < 3 {{ show(Option.Some(Kp.A(mkp(i)))); i = i + 1; }} println(\"end\") }}\n"
            ),
            &["a:0", "a:1", "a:2", "end"],
            "b67-option-pod-payload",
        );
    assert_clean_asan_run_no_auto_par(
            &format!(
                "{POD}fn show(x: Result[Kp, i64]) {{ match x {{ Result.Ok(Kp.A(p)) => {{ println(f\"a:{{p.a}}\"); }}, Result.Ok(Kp.B) => {{}}, Result.Err(e) => {{}} }} }}\n\
                 fn main() {{ let mut i = 0; while i < 3 {{ show(Result.Ok(Kp.A(mkp(i)))); i = i + 1; }} println(\"end\") }}\n"
            ),
            &["a:0", "a:1", "a:2", "end"],
            "b67-result-pod-payload",
        );
    // 3 — the `Err` side, which the row listed as NOT MEASURED.
    assert_clean_asan_run_no_auto_par(
            &format!(
                "{POD}fn show(x: Result[i64, Kp]) {{ match x {{ Result.Ok(n) => {{ println(f\"n{{n}}\"); }}, Result.Err(Kp.A(p)) => {{ println(f\"e:{{p.a}}\"); }}, Result.Err(Kp.B) => {{}} }} }}\n\
                 fn main() {{ let mut i = 0; while i < 3 {{ show(Result.Err(Kp.A(mkp(i)))); i = i + 1; }} println(\"end\") }}\n"
            ),
            &["e:0", "e:1", "e:2", "end"],
            "b67-result-err-side",
        );
    // 4 — BOTH sides boxed enums: two registrations against one slot, made
    //     mutually exclusive by `BoxedEnumDrop`'s tag guard. 336 B parent.
    assert_clean_asan_run_no_auto_par(
            &format!(
                "{POD}enum Jp {{ C(P4), D }}\n\
                 fn show(x: Result[Kp, Jp]) {{ match x {{ Result.Ok(Kp.A(p)) => {{ println(f\"a:{{p.a}}\"); }}, Result.Ok(Kp.B) => {{}}, Result.Err(Jp.C(p)) => {{ println(f\"c:{{p.a}}\"); }}, Result.Err(Jp.D) => {{}} }} }}\n\
                 fn main() {{ let mut i = 0; while i < 3 {{ show(Result.Ok(Kp.A(mkp(i)))); show(Result.Err(Jp.C(mkp(i)))); i = i + 1; }} println(\"end\") }}\n"
            ),
            &["a:0", "c:0", "a:1", "c:1", "a:2", "c:2", "end"],
            "b67-both-sides",
        );
    // 5 — THE HAZARD, and the reason the interior is not registered. The arm
    //     binds the WHOLE payload, so the local owns the interior. This uses
    //     a String-bearing payload deliberately: with a POD one there would
    //     be no interior to double-free and the cell would prove nothing.
    //     240 B parent -> clean; folding the interior in makes it 9 invalid
    //     frees.
    assert_clean_asan_run_no_auto_par(
            "struct R2 { s: String, t: String, u: String }\n\
             enum K { A(R2), B }\n\
             fn mkr(i: i64) -> R2 { return R2 { s: f\"ssssssss{i}\", t: f\"tttttttt{i}\", u: f\"uuuuuuuu{i}\" }; }\n\
             fn show(x: Option[K]) { match x { Option.Some(k) => { match k { K.A(r) => { println(f\"a:{r.s}\"); }, K.B => {} } }, Option.None => {} } }\n\
             fn main() { let mut i = 0; while i < 3 { show(Option.Some(K.A(mkr(i)))); i = i + 1; } println(\"end\") }\n",
            &["a:ssssssss0", "a:ssssssss1", "a:ssssssss2", "end"],
            "b67-whole-payload-binding-hazard",
        );
    // 6-7 — CONTROLS that must not move: a payload flowing into the return
    //       (the caller must NOT register at all), and a plain struct
    //       payload (the pre-existing class, whose interior IS still
    //       registered). Both clean before and after.
    assert_clean_asan_run_no_auto_par(
            &format!(
                "{POD}fn pass(x: Option[Kp]) -> Option[Kp] {{ return x; }}\n\
                 fn main() {{ let mut i = 0; while i < 3 {{ let y = pass(Option.Some(Kp.A(mkp(i)))); match y {{ Option.Some(Kp.A(p)) => {{ println(f\"a:{{p.a}}\"); }}, Option.Some(Kp.B) => {{}}, Option.None => {{}} }} i = i + 1; }} println(\"end\") }}\n"
            ),
            &["a:0", "a:1", "a:2", "end"],
            "b67-flows-into-return-control",
        );
    assert_clean_asan_run_no_auto_par(
            "struct R2 { s: String, t: String, u: String }\n\
             fn mkr(i: i64) -> R2 { return R2 { s: f\"ssssssss{i}\", t: f\"tttttttt{i}\", u: f\"uuuuuuuu{i}\" }; }\n\
             fn show(x: Option[R2]) { match x { Option.Some(r) => { println(f\"a:{r.s}\"); }, Option.None => {} } }\n\
             fn main() { let mut i = 0; while i < 3 { show(Option.Some(mkr(i))); i = i + 1; } println(\"end\") }\n",
            &["a:ssssssss0", "a:ssssssss1", "a:ssssssss2", "end"],
            "b67-struct-payload-control",
        );
}

/// B-2026-09-09-10 — THE INTERIOR of a boxed enum payload reached through a
/// by-value param, which B-2026-09-06-67 closed the ENVELOPE half of and
/// deliberately left.
///
/// `fn show(x: Option[K])` over `enum K { A(R2), B }` with a
/// `String`-bearing `R2` leaked 81 B in 9 blocks over three calls at `-O0`
/// (23 allocs / 14 frees) — `R2`'s three buffers. The `Result` spelling
/// measured identically. The sibling fixture above uses a POD payload, so
/// it pins the box and can never see this.
///
/// THE CALLER COULD NOT REGISTER THE INTERIOR UNTIL THE DISARM WORKED, and
/// that is the whole shape of the row. Registering it alone fixes the
/// nested arm and turns the WHOLE-PAYLOAD arm into 9 invalid frees in 3
/// contexts (23 allocs / 32 frees), because there the callee's own
/// bindings already own the interior. The disarm that should have stood
/// them down — `register_boxed_payload_alias` — tested only
/// `boxed_enum_payload_vars`, the set of what a frame OWNS, and a PARAM is
/// never in it (measured: `in_owned_set=false in_param_reach_set=true`).
/// It now also accepts `boxed_struct_payload_param_vars`, the companion set
/// that grants reach, so the alias is recorded and both spellings have
/// exactly one owner.
///
/// The cells are the two directions plus the controls that must not move:
///   - `nested` / `nestedres`: the reported leak, both seeded enums;
///   - `whole` / `wholeres`: the DOUBLE-FREE direction — the callee binds
///     the payload whole and matches it again, so its bindings own the
///     interior and the caller's registration must be disarmed;
///   - `wildcard`: binds nothing, so the caller's drop is the only owner
///     and must SURVIVE;
///   - `structpay`: a struct payload, whose interior already travelled —
///     byte-for-byte unchanged by this row;
///   - `passthru`: the param flows into the return, so the caller-side
///     registration must not happen at all and the local `y` owns it;
///   - `handsout`: the arm binds the payload and passes it to a CONSUMING
///     call. It leaked 81 B before this fix and is clean after, and it is
///     pinned because it LOOKS like the double-free direction and is not —
///     the arm's own retraction already covers a transfer out of the arm.
///
/// THE INTERIOR IS GATED ON THE CALLEE KEEPING THE PARAM IN ITS FRAME, and
/// the shape that forced that gate is not an arm shape at all: `let y = x;`
/// moves the param into a LOCAL whose let site registers its own owner, so
/// a caller-side interior becomes a second one — 23 allocs / 35 frees, 11
/// invalid frees, and it is what turned
/// `asan_reassigning_a_moved_in_boxed_payload_frees_the_envelope`'s
/// `param-alias` cell red on the first attempt.
/// `by_value_nonescaping_param_names` separates it from every cell above: a
/// scrutinee use and a read-only hole keep the payload in frame, a move
/// into another binding does not.
///
/// DELIBERATELY NOT A CELL: `let y = x; match y { .. }` over this same type
/// is a PRE-EXISTING double free — 1 invalid free, 23 allocs / 26 frees,
/// measured identically on the tree before this fix and after it, so this
/// row neither caused nor repairs it. It is filed on its own row; pinning
/// it here would fail this fixture for a defect it does not own.
#[test]
fn asan_boxed_enum_payload_param_owns_its_interior() {
    assert_clean_asan_run(
        r#"
struct R2 { s: String, t: String, u: String }
struct W2 { s: String, t: String, u: String }
enum K { A(R2), B }
fn mkr(i: i64) -> R2 { return R2 { s: f"ssssssss{i}", t: f"tttttttt{i}", u: f"uuuuuuuu{i}" }; }
fn mkw(i: i64) -> W2 { return W2 { s: f"wwwwwwww{i}", t: f"tttttttt{i}", u: f"uuuuuuuu{i}" }; }

fn nested(x: Option[K]) {
    match x { Option.Some(K.A(r)) => { println(f"n:{r.s}"); } Option.Some(K.B) => {} Option.None => {} }
}
fn nestedres(x: Result[K, i64]) {
    match x { Result.Ok(K.A(r)) => { println(f"nr:{r.s}"); } Result.Ok(K.B) => {} Result.Err(e) => {} }
}
fn whole(x: Option[K]) {
    match x { Option.Some(k) => { match k { K.A(r) => { println(f"w:{r.s}"); } K.B => {} } } Option.None => {} }
}
fn wholeres(x: Result[K, i64]) {
    match x { Result.Ok(k) => { match k { K.A(r) => { println(f"wr:{r.s}"); } K.B => {} } } Result.Err(e) => {} }
}
fn wildcard(x: Option[K]) { match x { Option.Some(_) => { println("wc"); } Option.None => {} } }
fn structpay(x: Option[W2]) { match x { Option.Some(w) => { println(f"s:{w.s}"); } Option.None => {} } }
fn passthru(x: Option[K]) -> Option[K] { return x; }
fn eat(r: R2) -> i64 { if r.s.contains("ssss") { return r.s.len(); } return 0; }
fn handsout(x: Option[K]) {
    match x { Option.Some(K.A(r)) => { println(f"h:{eat(r)}"); } Option.Some(K.B) => {} Option.None => {} }
}

fn main() {
    let mut i = 0;
    while i < 3 {
        nested(Option.Some(K.A(mkr(i))));
        nestedres(Result.Ok(K.A(mkr(i))));
        whole(Option.Some(K.A(mkr(i))));
        wholeres(Result.Ok(K.A(mkr(i))));
        wildcard(Option.Some(K.A(mkr(i))));
        structpay(Option.Some(mkw(i)));
        handsout(Option.Some(K.A(mkr(i))));
        let y = passthru(Option.Some(K.A(mkr(i))));
        match y { Option.Some(K.A(r)) => { println(f"p:{r.s}"); } Option.Some(K.B) => {} Option.None => {} }
        i = i + 1;
    }
    println("end");
}
"#,
        &[
            "n:ssssssss0",
            "nr:ssssssss0",
            "w:ssssssss0",
            "wr:ssssssss0",
            "wc",
            "s:wwwwwwww0",
            "h:9",
            "p:ssssssss0",
            "n:ssssssss1",
            "nr:ssssssss1",
            "w:ssssssss1",
            "wr:ssssssss1",
            "wc",
            "s:wwwwwwww1",
            "h:9",
            "p:ssssssss1",
            "n:ssssssss2",
            "nr:ssssssss2",
            "w:ssssssss2",
            "wr:ssssssss2",
            "wc",
            "s:wwwwwwww2",
            "h:9",
            "p:ssssssss2",
            "end",
        ],
        "asan_boxed_enum_payload_param_owns_its_interior",
    );
}

/// B-2026-09-07-36 — an enum argument on the RETURN route whose entry copy
/// the admission gate cannot see, across all FOUR legs of that gate.
///
/// The gate admits the caller-side memory registration only when the callee
/// ENTRY-COPIES the argument, because then the callee returns the copy and
/// the caller's original is genuinely orphaned. Two predicates answer that
/// one question and differ only in which spelling they resolve:
/// `arg_is_entry_copied_heap_enum` routes through `enum_name_of_expr`,
/// whose `Call` arm reaches a variant CONSTRUCTOR and not a function that
/// returns the enum, so `passe(mkes(71))` answered false where
/// `passe(Es.A(..))` answered true — same type, same callee, same copy.
///
/// The via-call sibling existed (B-2026-09-07-5) but fed the STORE clause
/// only, deliberately, on a premise its doc recorded: the return-route
/// spelling was "clean today with the registration DECLINED". That reading
/// was taken at -O2 ONLY, where LLVM elides a malloc nothing observes. At
/// -O0 the orphan is real, and the premise does not survive.
///
/// FIVE shapes leaked 3 B in 1 block at -O0 (10 allocs / 9 frees each), one
/// per leg plus the mono leg's second spelling, and all five are clean
/// after:
///
///   - `p1` free fn, call-spelled arg — the cell the row was filed on;
///   - `p3` static assoc fn (`assoc_call.rs`), `p4` method
///     (`method_call.rs`) — the same gate, kept in step per B-2026-08-29-54;
///   - `p5` / `p6` the GENERIC leg (`mono.rs`), which excluded BOTH enum
///     predicates, not just the via-call one. Its note deferred them for
///     want of a measurement — "whether a generic enum arg leaks the same
///     way is an unmeasured question" — and the answer is yes on both
///     spellings. That deferral is retired here rather than re-derived.
///
/// FOUR controls, clean before AND after with identical alloc/free counts:
///
///   - `p2` the CTOR spelling on the free leg. It is the direct evidence
///     that admitting the call spelling is right rather than risky: it
///     already takes this route, with the same callee and the same entry
///     copy. SPELL IT WITH AN F-STRING, not `Es.A("x")` — a static literal
///     never reaches the allocator, so the literal cell sits at the
///     BASELINE alloc count and is clean vacuously. The first draft of this
///     fixture made that mistake and the cell proved nothing;
///   - `p7` a generic callee that CONSUMES rather than returns, so no
///     escape clause fires at all;
///   - `p8` a generic STRUCT on the return route — the shape the mono leg
///     already admitted, pinning that this change did not disturb it;
///   - `p9` the generic STORE route (`stashg(mut v, mkes(56))`), the clause
///     the via-call predicate was originally added for. It is the
///     double-free direction: the store clause and the return clause now
///     share one disjunction, so a regression there would show as an
///     invalid free rather than a leak.
///
/// DELIBERATELY NOT A CELL: a SHARED-payload enum (`enum Et { A(Sh) }` over
/// a `shared struct`) on the same return route strands its 16-byte refcount
/// block, measured byte-identical before and after this change (9 allocs /
/// 8 frees). Different mechanism — the helper's `shared` clause asks whether
/// the ENUM is shared, not whether its PAYLOAD is — and it is filed on its
/// own row. Pinning it here would fail this fixture for a defect it does
/// not own.
#[test]
fn asan_enum_arg_on_the_return_route_frees_its_orphan() {
    assert_clean_asan_run(
        r#"
enum Es { A(String), B }
impl Drop for Es { fn drop(mut ref self) { println("dEs") } }
struct H { n: i64 }
impl H {
    fn spass(e: Es) -> Es { return e; }
    fn mpass(ref self, e: Es) -> Es { return e; }
}
fn mkes(i: i64) -> Es { return Es.A(f"ee{i}"); }
fn passe(e: Es) -> Es { return e; }
fn passg[T](e: T) -> T { return e; }
fn eatg[T](e: T) -> i64 { return 1; }
fn stashg[T](v: mut ref Vec[T], x: T) { v.push(x); }

struct Rs { id: i64, name: String }
impl Drop for Rs { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mkr(i: i64) -> Rs { return Rs { id: i, name: f"hh{i}" }; }

fn main() {
    let h = H { n: 1 };
    let i = 71;

    let z1 = passe(mkes(i));            println("p1");
    let z2 = passe(Es.A(f"ee{i}"));     println("p2");
    let z3 = H.spass(mkes(i));          println("p3");
    let z4 = h.mpass(mkes(i));          println("p4");
    let z5 = passg(mkes(i));            println("p5");
    let z6 = passg(Es.A(f"ee{i}"));     println("p6");
    let n7 = eatg(mkes(i));             println(f"p7={n7}");
    let z8 = passg(mkr(9));             println("p8");
    let mut v: Vec[Es] = Vec.new();
    stashg(mut v, mkes(56));            println(f"p9={v.len()}");
    println("end");
}
"#,
        &[
            "dEs", "p1", "dEs", "p2", "dEs", "p3", "dEs", "p4", "dEs", "p5", "dEs", "p6", "dEs",
            "p7=1", "dR9", "p8", "p9=1", "dEs", "end",
        ],
        "asan_enum_arg_on_the_return_route_frees_its_orphan",
    );
}

/// B-2026-09-16-13's memory twin — a fresh enum returned BY A CALL, in
/// argument position, had no caller-side owner, so its payload's heap was
/// never freed.
///
/// `eat(mk(i))` where `mk` returns an enum: the callee's by-value param
/// declines the entry copy, and the caller wrote no `__owned_agg_tmp` for a
/// CALL-produced enum, though it already did for a call-produced STRUCT
/// (B-2026-08-02-28) and for an INLINE enum constructor. Measured at `-O0`
/// on this program before the fix: 41 allocs against 36 frees, 192 bytes
/// definitely lost in 4 blocks, 4 valgrind errors. After: 42 / 42, zero.
///
/// Four of the rows here are the same defect rather than neighbours — the
/// `String` payload the row was filed on, a struct-with-`Drop` payload, a
/// `Vec` payload, and a callee that destructures — and every one leaked on
/// the pre-fix tree. `Td` is the one enum that was always correct, because
/// its own `impl Drop` routes it through `has_user_drop` to a different
/// owner; it is a HARM GUARD, not evidence of work. So are the last four
/// rows (named local, inline constructor, struct return, `Option` return),
/// which read identically on both arms by design and catch an over-broad
/// repair as a double free rather than as a leak.
///
/// The output twin, which pins the `dR2` body the pre-fix tree also lost,
/// is in `tests/codegen.rs`.
///
/// The last three rows are the CALL SPELLING axis, added after the rest of
/// this grid had already gone green: every other cell here is a free
/// function, and the row records that a method argument leaks identically.
/// Measured against the named parent tree, they were not guards — a method
/// argument, a `Drop`-bearing method argument and an associated-function
/// argument together lost 120 B in 4 blocks with `dR11` ABSENT, and are
/// clean with the body present after. A grid can be wide on payload type
/// and blind on how the callee is spelled.
#[test]
fn asan_enum_call_return_in_argument_position_has_an_owner() {
    assert_clean_asan_run(
        r#"
struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
enum Ts { A(String), B }
enum Tr { A(R), B }
enum Tv { A(Vec[String]), B }
enum Td { A(String), B }
impl Drop for Td { fn drop(mut ref self) { println("dTd") } }
struct W { a: String }

fn mks(n: i64) -> Ts { return Ts.A(f"b1613-payload-aaaaaaaaaaaaaaaa-{n}"); }
fn mkr(n: i64) -> Tr { return Tr.A(R { id: n, s: f"b1613-payload-aaaaaaaaaaaaaaaa-{n}" }); }
fn mkv(n: i64) -> Tv { let mut v: Vec[String] = Vec.new(); v.push(f"b1613-payload-aaaaaaaaaaaaaaaa-{n}"); return Tv.A(v); }
fn mkd(n: i64) -> Td { return Td.A(f"b1613-payload-aaaaaaaaaaaaaaaa-{n}"); }
fn mkw(n: i64) -> W { return W { a: f"b1613-payload-aaaaaaaaaaaaaaaa-{n}" }; }
fn mko(n: i64) -> Option[String] { return Option.Some(f"b1613-payload-aaaaaaaaaaaaaaaa-{n}"); }

fn eats(e: Ts) -> i64 { return 7; }
fn eatr(e: Tr) -> i64 { return 7; }
fn eatv(e: Tv) -> i64 { return 7; }
fn eatd(e: Td) -> i64 { return 7; }
fn eatw(e: W) -> i64 { return 7; }
fn eato(e: Option[String]) -> i64 { return 7; }
fn takes(e: Ts) -> i64 { match e { Ts.A(s) => { return 1 }, Ts.B => { return 0 } } }
struct H { n: i64 }
impl H {
    fn meth(ref self, e: Ts) -> i64 { return 7; }
    fn methr(ref self, e: Tr) -> i64 { return 7; }
    fn assoc(e: Ts) -> i64 { return 7; }
}

fn main() {
    println(f"s={eats(mks(1))}");
    println(f"r={eatr(mkr(2))}");
    println(f"v={eatv(mkv(3))}");
    println(f"m={takes(mks(4))}");
    println(f"d={eatd(mkd(5))}");
    let e6: Ts = mks(6);
    println(f"local={eats(e6)}");
    println(f"inline={eats(Ts.A(f"b1613-payload-aaaaaaaaaaaaaaaa-7"))}");
    println(f"struct={eatw(mkw(8))}");
    println(f"option={eato(mko(9))}");
    let h = H { n: 1 };
    println(f"meth={h.meth(mks(10))}");
    println(f"methr={h.methr(mkr(11))}");
    println(f"assoc={H.assoc(mks(12))}");
    println("end");
}
"#,
        &[
            "s=7", "dR2", "r=7", "v=7", "m=1", "dTd", "d=7", "local=7", "inline=7", "struct=7",
            "option=7", "meth=7", "dR11", "methr=7", "assoc=7", "end",
        ],
        "asan_enum_call_return_in_argument_position_has_an_owner",
    );
}

/// B-2026-09-20-63 (memory twin) — the LEAK this row is filed about.
///
/// A constructor used directly as a `match` scrutinee is a temporary bound
/// under no name, and `materialize_freshtemp_enum_scrutinee` declined it
/// outright for a GENERIC enum: both of the answers it gates on are read
/// off the DECLARATION, where a payload spelled `T` classifies as carrying
/// nothing. So the heap box holding the instantiated payload had no owner
/// at all — valgrind read 11 allocs / 10 frees with 24 B lost for the `Vec`
/// spelling and 10 / 9 with 16 B lost for the `Array` one, one block each,
/// the container's own envelope rather than its elements.
///
/// THE BODY COUNT IS THE OTHER HALF OF THIS CELL AND IT IS WHY THIS IS NOT
/// A LEAK-ONLY FIXTURE. The obvious repair — register the box's interior
/// drop alongside — is a DOUBLE FREE for a payload an arm's binding takes
/// over (`free(): double free detected in tcache 2`, plus an invalid read
/// from walking the moved-out payload, measured on the `Vec` cell), and
/// ASAN sees that where an output oracle would not. The expected lines
/// below carry both channels: `av` fires twice and `vv` not at all, which
/// is the split between a payload the husk owns and one the arm does.
#[test]
fn asan_freshtemp_generic_enum_scrutinee_payload_has_exactly_one_owner() {
    assert_clean_asan_run(
        r#"
struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i, s: f"payload-{i}" }; }
enum Slot[T] { S(T), N }
enum Ev { V(Vec[R]), N }

fn main() {
    let a: Array[R, 2] = [mkr(1), mkr(2)];
    match Slot.S(a) { Slot.S(v) => { println(f"av{v[0].id}") } Slot.N => { println("no") } }

    let b: Array[R, 2] = [mkr(3), mkr(4)];
    match Slot.S(b) { Slot.S(_) => { println("ad") } Slot.N => { println("no") } }

    let c: Vec[R] = [mkr(5), mkr(6)];
    match Slot.S(c) { Slot.S(v) => { println(f"vv{v[0].id}") } Slot.N => { println("no") } }

    let d: Vec[R] = [mkr(7), mkr(8)];
    match Ev.V(d) { Ev.V(v) => { println(f"ev{v[0].id}") } Ev.N => { println("no") } }

    let e: Array[R, 2] = [mkr(9), mkr(10)];
    let _ = Slot.S(e);
    println("end");
}
"#,
        &[
            // The fix: the husk is the payload's only owner, so both bodies
            // run once, after the arm.
            "av1", "dR1", "dR2",
            // AGREED GAP, pinned: a discarding arm runs no body on any
            // surface. Only its leak is closed, which is what this fixture
            // is here to hold.
            "ad",
            // The Vec spelling: the arm's binding owns the buffer, so the
            // husk neither frees nor walks it. SILENT WHEN THIS FIXTURE WAS
            // WRITTEN and no longer — B-2026-09-21-11 registered the bodies
            // against that binding, where they fire ahead of its own buffer
            // free. Pinned at the correct answer now rather than at the gap.
            "vv5", "dR5", "dR6", // The declared-container spelling, its own agreed gap.
            "ev7", // A whole-value discard, correct through B-2026-09-20-15.
            "dR9", "dR10", "end",
        ],
        "asan_freshtemp_generic_enum_scrutinee_payload_has_exactly_one_owner",
    );
}

#[test]
fn asan_generic_enum_payload_runs_its_drop_and_frees_its_interior() {
    assert_clean_asan_run(
        r#"
struct R2 { s: String, t: String, u: String }
impl Drop for R2 { fn drop(mut ref self) { println(f"d2:{self.s.len()}") } }
struct R3 { id: i64 }
impl Drop for R3 { fn drop(mut ref self) { println(f"d3:{self.id}") } }

enum G[T] { X(T), Y }
enum Two[T] { A(T), B(T), C }
enum Mono { P(R2), Q }

fn mkr(n: i64) -> R2 { return R2 { s: f"aaaaaaaa{n}", t: f"bbbbbbbb{n}", u: f"cccccccc{n}" }; }
fn holdgen(g: G[R2]) { println("hg"); }
fn holdmono(m: Mono) { println("hm"); }

fn main() {
    let p = G.X(mkr(1));                    println("c1");
    let q = Mono.P(mkr(2));                 println("c2");
    holdgen(G.X(mkr(3)));                   println("c3");
    holdmono(Mono.P(mkr(4)));               println("c4");
    let a = G.X(mkr(5)); holdgen(a);        println("c5");
    let b = Mono.P(mkr(6)); holdmono(b);    println("c6");

    let r1 = G.X(mkr(7));
    match r1 { G.X(r) => { println(f"k1:{r.s.len()}"); }, G.Y => { println("y"); } }
    let r2 = G.X(mkr(8));
    match r2 { G.X(r) => { let z = r; println("k2"); }, G.Y => { println("y"); } }
    let r3 = G.X(mkr(9));
    match r3 { G.X(_) => { println("k3"); }, G.Y => { println("y"); } }

    let w = G.X(R3 { id: 7 });              println("k4");
    let t1 = Two.A(mkr(10));
    let t2 = Two.B(mkr(11));                println("k5");
    let u: G[R2] = G.Y;                     println("k6");
    println("end");
}
"#,
        &[
            "d2:9", "c1", "d2:9", "c2", "hg", "d2:9", "c3", "hm", "d2:9", "c4", "hg", "d2:9", "c5",
            "hm", "d2:9", "c6", "k1:9", "d2:9", "d2:9", "k2", "k3", "d2:9", "d3:7", "k4", "d2:10",
            "d2:10", "k5", "k6", "end",
        ],
        "asan_generic_enum_payload_runs_its_drop_and_frees_its_interior",
    );
}

/// B-2026-09-11-4 — the four payload shapes B-2026-09-11-3 deliberately
/// left alone: a TUPLE, an `Array`, an `Option[String]` and a user GENERIC
/// struct, each inside `enum Slot[T] { Filled(T), Blank }`.
///
/// Same mechanism as the parent, one payload type over. The erased payload
/// area is ONE word (the classifier reads the DECLARATION, where the
/// payload is the bare parameter `T`), so `coerce_to_payload_words` boxes
/// any wider monomorph; the box drop reclaimed the envelope and
/// `enum_boxed_payload_interior_drop` answered `None` for all four, so
/// nothing owned what was inside it. The tuple and the array fell out at
/// the resolver's first guard because neither is a `TypeKind::Path` it
/// could read; `Option[String]` and `Wrap[String]` are `Path` WITH generic
/// args, and the two user-type lookups are gated on `generic_args.is_none()`
/// because a name-shared drop cannot free instantiation-specific heap.
///
/// `c8` AND `c9` ARE THE DOUBLE-FREE CELLS, and on this row they are the
/// half that actually had teeth. Unlike the parent's shapes, an
/// `Option[String]` payload that is MATCHED OUT was already clean before
/// this fix — its arm binding genuinely owns the interior — so installing
/// an interior drop without the `clear_boxed_enum_inner_drop` retraction
/// would have freed those buffers twice rather than merely leaked them.
/// They were clean pre-fix and must stay clean; a leak count alone does not
/// check that direction.
///
/// `c7` is the control: a bare user struct payload, which B-2026-09-10-2
/// already resolved, clean throughout.
///
/// NOT VACUOUS, and unusually well-evidenced for this class: pre-fix this
/// program loses **1,120 B in 56 blocks at `KARAC_OPT_LEVEL=0` AND 320 B in
/// 16 blocks at the default `-O2`** — it fails against the unfixed compiler
/// at BOTH levels, so it does not depend on the `-O0` leg to have any
/// force. Every payload is still seeded from the opaque `env.args().len()`,
/// and the read cells go through `contains` (bytes, not length), because
/// the first draft of the PARENT's fixture was literal-seeded and folded to
/// nothing at `-O2`, passing against the compiler it was written to fail.
///
/// `c10`–`c17` ARE B-2026-09-12-5, the MOVE-OUT half this row's fix
/// deliberately did not touch, added here rather than in a fresh fixture
/// because that row's own instruction was to extend this one — `c8`/`c9`
/// are the double-free cells its gate must not disturb, and they only
/// prove that while they sit in the same program.
///
/// The split across those eight is the whole point, and it is two-by-two:
///
///  * `c10`–`c12` READ the payload out of a call-site arm (tuple, generic
///    struct, `Array`). These leaked 160 / 160 / 320 B at `-O0` before the
///    gate, because `clear_boxed_enum_inner_drop` retracted the box's
///    interior walk for a binding that registers no owner of its own.
///  * `c13`, `c14` are the same read through an OWNED CALLEE.
///  * `c15`–`c17` CONSUME the payload into a local that outlives the match.
///    These were clean before and after: the retraction is right there,
///    because the value's new home owns the interior. They are what forces
///    the gate to test the arm's borrow verdict rather than the payload
///    shape alone.
///
/// THE NEGATIVE CONTROL WAS RUN FOR THESE EIGHT, not just assumed: with
/// `src/codegen` checked out at the pre-fix commit and rebuilt, this test
/// FAILS; with the fix restored it passes. (`git checkout`, not `git
/// archive` — the latter restores original mtimes and cargo then rebuilds
/// nothing, so the run measures the previous binary and every revision
/// looks identical.) The per-cell numbers behind the aggregate, measured
/// directly under valgrind at `KARAC_OPT_LEVEL=0` with one verdict line per
/// cell: 160 B for each tuple cell, 160 B for each `Wrap[String]` cell and
/// 320 B for the `Array` cell, over 8 rounds.
///
/// NO `Slot[Array[..]]` THROUGH AN OWNED CALLEE, and the absence is
/// deliberate: that cell still leaks 320 B, because the PARAM registration
/// site passes `array_interior_ok: false` and so installs no array
/// interior drop for the gate to preserve. B-2026-09-12-18 put that
/// restriction there against a double free, so lifting it is its own
/// measurement and its own row — not something to smuggle in behind a
/// fixture cell.
#[test]
fn asan_generic_enum_boxed_aggregate_payloads_free_their_interiors() {
    let mut expected: Vec<&str> = Vec::new();
    for _ in 0..8 {
        expected.extend_from_slice(&[
            "c1", "c2", "c3", "c4", "c5:true", "c6:true", "c7", "c8:true", "c9:true", "c10:true",
            "c11:true", "c12:true", "c13:true", "c14:true", "c15:true", "c16:true", "c17:true",
        ]);
    }
    expected.push("end");
    assert_clean_asan_run_min_allocs(
        r#"
struct Wrap[T] { val: T }
struct Plain { s: String }
enum Slot[T] { Filled(T), Blank }

fn opeek(s: ref Slot[Option[String]]) -> bool {
    match s { Filled(x) => match x { Some(y) => y.contains("row"), None => false, }, Blank => false, }
}
fn otake(s: Slot[Option[String]]) -> bool {
    match s { Filled(x) => match x { Some(y) => y.contains("row"), None => false, }, Blank => false, }
}
fn wpeek(s: ref Slot[Wrap[String]]) -> bool {
    match s { Filled(x) => x.val.contains("row"), Blank => false, }
}
fn ttake(s: Slot[(String, i64)]) -> bool {
    match s { Filled(x) => x.0.contains("row"), Blank => false, }
}
fn wtake(s: Slot[Wrap[String]]) -> bool {
    match s { Filled(x) => x.val.contains("row"), Blank => false, }
}

fn main() {
    let n = env.args().len() as i64;
    let mut i: i64 = 0i64;
    while i < 8i64 {
        let c1: Slot[(String, i64)] = Filled((f"row-aaaaaaaaaaaa-{i}-{n}", 1i64));
        println("c1");
        let c2: Slot[Array[String, 2]] = Filled([f"row-bbbbbbbbbbbb-{i}-{n}", f"row-cccccccccccc-{i}-{n}"]);
        println("c2");
        let c3: Slot[Option[String]] = Filled(Some(f"row-dddddddddddd-{i}-{n}"));
        println("c3");
        let c4: Slot[Wrap[String]] = Filled(Wrap { val: f"row-eeeeeeeeeeee-{i}-{n}" });
        println("c4");
        let c5: Slot[Option[String]] = Filled(Some(f"row-ffffffffffff-{i}-{n}"));
        println(f"c5:{opeek(c5)}");
        let c6: Slot[Wrap[String]] = Filled(Wrap { val: f"row-gggggggggggg-{i}-{n}" });
        println(f"c6:{wpeek(c6)}");
        let c7: Slot[Plain] = Filled(Plain { s: f"row-hhhhhhhhhhhh-{i}-{n}" });
        println("c7");
        let c8: Slot[Option[String]] = Filled(Some(f"row-iiiiiiiiiiii-{i}-{n}"));
        println(f"c8:{match c8 { Filled(x) => match x { Some(y) => y.contains("row"), None => false, }, Blank => false, }}");
        let c9: Slot[Option[String]] = Filled(Some(f"row-jjjjjjjjjjjj-{i}-{n}"));
        println(f"c9:{otake(c9)}");
        let c10: Slot[(String, i64)] = Filled((f"row-kkkkkkkkkkkk-{i}-{n}", 2i64));
        println(f"c10:{match c10 { Filled(x) => x.0.contains("row"), Blank => false, }}");
        let c11: Slot[Wrap[String]] = Filled(Wrap { val: f"row-llllllllllll-{i}-{n}" });
        println(f"c11:{match c11 { Filled(x) => x.val.contains("row"), Blank => false, }}");
        let c12: Slot[Array[String, 2]] = Filled([f"row-mmmmmmmmmmmm-{i}-{n}", f"row-nnnnnnnnnnnn-{i}-{n}"]);
        println(f"c12:{match c12 { Filled(x) => x[0].contains("row"), Blank => false, }}");
        let c13: Slot[(String, i64)] = Filled((f"row-oooooooooooo-{i}-{n}", 3i64));
        println(f"c13:{ttake(c13)}");
        let c14: Slot[Wrap[String]] = Filled(Wrap { val: f"row-pppppppppppp-{i}-{n}" });
        println(f"c14:{wtake(c14)}");
        let c15: Slot[(String, i64)] = Filled((f"row-qqqqqqqqqqqq-{i}-{n}", 4i64));
        let m15: (String, i64) = match c15 { Filled(x) => x, Blank => (f"zzzzzzzzzzzz-{i}", 0i64), };
        println(f"c15:{m15.0.contains("row")}");
        let c16: Slot[Wrap[String]] = Filled(Wrap { val: f"row-rrrrrrrrrrrr-{i}-{n}" });
        let m16: Wrap[String] = match c16 { Filled(x) => x, Blank => Wrap { val: f"zzzzzzzzzzzz-{i}" }, };
        println(f"c16:{m16.val.contains("row")}");
        let c17: Slot[Array[String, 2]] = Filled([f"row-ssssssssssss-{i}-{n}", f"row-tttttttttttt-{i}-{n}"]);
        let m17: Array[String, 2] = match c17 { Filled(x) => x, Blank => [f"zzzzzzzzzzzz-{i}", f"yyyyyyyyyyyy-{i}"], };
        println(f"c17:{m17[0].contains("row")}");
        i = i + 1i64;
    }
    println("end");
}
"#,
        &expected,
        "asan_generic_enum_boxed_aggregate_payloads_free_their_interiors",
        // 117 measured at the default level post-fix (the pre-fix binary
        // reaches only 77, because the leaked allocations are the ones the
        // optimizer could delete). A version folded away entirely reaches
        // ~10, so this floor separates them with room for host drift.
        60,
    );
}

/// B-2026-09-12-8 — an enum's TUPLE payload. No generics, no boxing, no
/// erased payload area: `enum T2 { P((String, i64)), Q }` lost 24 B a round
/// on plain scope exit while `enum S2 { R(Pr), Z }` holding a user struct
/// of the same two fields was clean.
///
/// `enum_drop_kind_for_type_expr` matched only `TypeKind::Path`, so a tuple
/// fell to the `_` tail and classified `EnumDropKind::None`. The machinery
/// to free it already existed and already worked in every OTHER position a
/// tuple can hold heap — a plain local, a struct field, a `Vec` element,
/// all measured clean against the same compiler — so the enum payload was
/// the one position left out, and the fix is a `NestedTuple` kind wired to
/// the tuple drop those three already use.
///
/// `c2` AND `c6` ARE THE DOUBLE-FREE CELLS AND THEY EARNED THEIR PLACE.
/// Adding the drop alone — without the symmetric arm in
/// `deep_copy_enum_heap_payload_in_place` — turned this row's leak into a
/// double free the moment the enum was passed BY VALUE: the callee's
/// bit-copied param aliased the caller's element buffers and both drops
/// freed them (`Invalid free() ... 0 bytes inside a block of size 24
/// free'd`, once per round). That intermediate state passed a leak-only
/// check with flying colours — every leak cell read clean — which is why
/// the by-value cell is in the fixture and not merely in the prose.
///
/// `c8` and `c9` are the controls (a user-struct payload and a bare
/// `String` payload), clean before and after in all five call shapes.
///
/// NOT VACUOUS: pre-fix this program loses **1,344 B in 56 blocks plus 320
/// B indirect at `KARAC_OPT_LEVEL=0`, and 800 B in 40 blocks at the default
/// `-O2`** — it fails against the unfixed compiler at BOTH levels. Payloads
/// are seeded from the opaque `env.args().len()` and read through
/// `contains` (bytes, not length).
#[test]
fn asan_enum_tuple_payload_frees_its_elements() {
    let mut expected: Vec<&str> = Vec::new();
    for _ in 0..8 {
        expected.extend_from_slice(&[
            "c1", "c2:true", "c3", "c4:true", "c5:true", "c6:2", "c7", "c8:true", "c9:true",
        ]);
    }
    expected.push("end");
    assert_clean_asan_run_min_allocs(
        r#"
struct Pr { s: String, k: i64 }
enum T2 { P((String, i64)), Q }
enum Tvec { Pvec((Vec[String], i64)), Qvec }
enum Tnest { Pnest(((String, i64), i64)), Qnest }
enum S2 { R(Pr), Z }
enum V2 { W(String), Y }

fn take2(s: T2) -> bool { match s { P(x) => x.0.contains("row"), Q => false, } }
fn peek2(s: ref T2) -> bool { match s { P(x) => x.0.contains("row"), Q => false, } }
fn mk2(i: i64, n: i64) -> T2 { return P((f"row-cccccccccccc-{i}-{n}", 1i64)); }
fn takev(s: Tvec) -> i64 {
    match s { Pvec(x) => { let mut k: i64 = 0i64; for e in x.0 { if e.contains("row") { k = k + 1i64; } } return k; }, Qvec => { return 0i64; } }
}
fn takes(s: S2) -> bool { match s { R(x) => x.s.contains("row"), Z => false, } }
fn takew(s: V2) -> bool { match s { W(x) => x.contains("row"), Y => false, } }

fn main() {
    let n = env.args().len() as i64;
    let mut i: i64 = 0i64;
    while i < 8i64 {
        let c1: T2 = P((f"row-aaaaaaaaaaaa-{i}-{n}", 1i64));
        println("c1");
        let c2: T2 = P((f"row-bbbbbbbbbbbb-{i}-{n}", 1i64));
        println(f"c2:{take2(c2)}");
        let c3: T2 = mk2(i, n);
        println("c3");
        let c4: T2 = P((f"row-dddddddddddd-{i}-{n}", 1i64));
        println(f"c4:{peek2(c4)}");
        let c5: T2 = P((f"row-eeeeeeeeeeee-{i}-{n}", 1i64));
        println(f"c5:{match c5 { P(x) => x.0.contains("row"), Q => false, }}");
        let c6: Tvec = Pvec((Vec[f"row-ffffffffffff-{i}-{n}", f"row-gggggggggggg-{i}-{n}"], 1i64));
        println(f"c6:{takev(c6)}");
        let c7: Tnest = Pnest(((f"row-hhhhhhhhhhhh-{i}-{n}", 1i64), 2i64));
        println("c7");
        let c8: S2 = R(Pr { s: f"row-iiiiiiiiiiii-{i}-{n}", k: 1i64 });
        println(f"c8:{takes(c8)}");
        let c9: V2 = W(f"row-jjjjjjjjjjjj-{i}-{n}");
        println(f"c9:{takew(c9)}");
        i = i + 1i64;
    }
    println("end");
}
"#,
        &expected,
        "asan_enum_tuple_payload_frees_its_elements",
        // 173 measured at the default level post-fix (the pre-fix binary
        // reaches 133 — the leaked allocations are the ones the optimizer
        // could delete). A version folded away entirely reaches ~10.
        90,
    );
}

/// B-2026-09-09-13 — a by-value param whose boxed payload the CALLER owns,
/// REBOUND to a local by the callee, is freed twice.
///
/// The arg-site arm that gives a boxed param payload's box a caller-scope
/// owner reasons that "params register no drop of their own, so the caller
/// is the only frame that can own it". True of params and not of LOCALS: a
/// bare rebind (`let mut vv = value;`) gives `vv` a full `BoxedEnumDrop` at
/// its let site, and the rebind's disarm targets its SOURCE — which, for a
/// param, is nothing to disarm. Both frames then free the box. A callee
/// cannot reach its caller's registration, so the stand-down is at the call
/// site: `callee_rebinds_param_to_local`.
///
/// TWO DIFFERENT HISTORIES, which is why both spellings are here:
///
///   * the ENUM payload is a REGRESSION. 99f54104e widened the arm to admit
///     one; measured clean at its parent, `Invalid free` at it and on every
///     commit after. It is what turned both ASAN ratchet legs red on main.
///   * the STRUCT payload was ALREADY broken, before that commit and
///     independent of it — measured `Invalid free` at 99f54104e~1 too. The
///     existing `boxed-struct-payload` cell of
///     `asan_reassigning_a_moved_in_boxed_payload_frees_the_envelope` binds
///     its source from a LOCAL, so the param axis had no cell at all.
///
/// Both abort with `free(): double free detected in tcache 2` rather than
/// leaking, so the failure is loud once a fixture exists to hear it.
///
/// NO STRING-BEARING NO-ALIAS CELL, deliberately. That shape still loses its
/// payload interior (B-2026-09-09-10, open and held elsewhere), so it cannot
/// assert clean; the POD control below covers the no-rebind path instead,
/// which is the axis this fix turns on. The gate does not fire without a
/// rebind, so that row's shape is untouched by this change — measured
/// identical (68 B) before and after.
#[test]
fn asan_caller_owned_boxed_param_payload_rebound_to_a_local_is_freed_once() {
    const ENUM_PRE: &str = "enum Val { Nothing, Ident(String) }\n\
             fn ident_len(v: Val) -> i64 { match v { Val.Ident(s) => s.len(), Val.Nothing => 0 } }\n\
             fn val_none() -> Option[Val] { Option.None }\n\
             fn seed() -> i64 { env.args().len() }\n\
             fn payload() -> String { f\"payload-moved-in-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaa\" }\n\
             fn mk_val() -> Option[Val] { Option.Some(Val.Ident(payload())) }\n";
    const STRUCT_PRE: &str = "struct R2 { s: String, t: String, u: String, a: i64, b: i64 }\n\
             fn seed() -> i64 { env.args().len() }\n\
             fn mkr(i: i64) -> R2 { return R2 { s: f\"ssssssss{i}{seed()}\", t: f\"tttttttt{i}\", u: f\"uuuuuuuu{i}\", a: 1, b: 2 }; }\n\
             fn r2_none() -> Option[R2] { Option.None }\n";

    // 1 — the REGRESSION: boxed user ENUM payload, param rebound to a local.
    assert_clean_asan_run(
            &format!(
                "{ENUM_PRE}\
                 fn f(value: Option[Val]) -> i64 {{ let mut vv = value; let mut acc = 0;\n\
                 \x20  while let Some(v) = vv {{ acc = acc + ident_len(v); vv = val_none(); }} acc }}\n\
                 fn main() {{ println(f(mk_val())); }}\n"
            ),
            &["45"],
            "b13-enum-payload-param-rebind",
        );

    // 2 — the PRE-EXISTING one: boxed user STRUCT payload, same rebind.
    //     Broken at 99f54104e~1 as well, so this cell is not a regression
    //     guard but first coverage of the param axis for a struct payload.
    assert_clean_asan_run(
        &format!(
            "{STRUCT_PRE}\
                 fn f(value: Option[R2]) -> i64 {{ let mut vv = value; let mut acc = 0;\n\
                 \x20  while let Some(r) = vv {{ acc = acc + r.s.len(); vv = r2_none(); }} acc }}\n\
                 fn main() {{ println(f(Option.Some(mkr(1)))); }}\n"
        ),
        &["10"],
        "b13-struct-payload-param-rebind",
    );

    // 3 — the `Result` spelling. Clean on every commit measured, including
    //     both sides of the regression, so this pins the arm the fix also
    //     gates rather than guarding a known break: the two arg-site arms
    //     ask the same question and must keep answering it the same way.
    assert_clean_asan_run(
            &format!(
                "{ENUM_PRE}\
                 fn val_err() -> Result[Val, i64] {{ Result.Err(7) }}\n\
                 fn f(value: Result[Val, i64]) -> i64 {{ let mut vv = value; let mut acc = 0;\n\
                 \x20  while let Ok(v) = vv {{ acc = acc + ident_len(v); vv = val_err(); }} acc }}\n\
                 fn main() {{ println(f(Result.Ok(Val.Ident(payload())))); }}\n"
            ),
            &["45"],
            "b13-result-payload-param-rebind",
        );

    // 4 — the LOCAL-source control. Both registrations are then in one
    //     frame, where the rebind disarms its source, so this was clean
    //     throughout and must stay clean: it is what says the axis is the
    //     source being a PARAM and not the rebind itself.
    assert_clean_asan_run(
            &format!(
                "{ENUM_PRE}\
                 fn f() -> i64 {{ let value = mk_val(); let mut vv = value; let mut acc = 0;\n\
                 \x20  while let Some(v) = vv {{ acc = acc + ident_len(v); vv = val_none(); }} acc }}\n\
                 fn main() {{ println(f()); }}\n"
            ),
            &["45"],
            "b13-local-source-control",
        );

    // 5 — the NO-REBIND control, POD payload so the interior residual of
    //     B-2026-09-09-10 cannot muddy it. The callee reads the param
    //     directly, no local is created, and the caller must KEEP its
    //     registration: a fix that stood down unconditionally would leak
    //     the box here instead, which is the error direction this cell
    //     exists to catch.
    assert_clean_asan_run(
            "struct W { a: i64, b: i64, c: i64, d: i64, e: i64, f: i64 }\n\
             fn seed() -> i64 { env.args().len() }\n\
             fn w_none() -> Option[W] { Option.None }\n\
             fn f(value: Option[W]) -> i64 { let mut acc = 0;\n\
             \x20  while let Some(w) = value { acc = acc + w.a + w.f; value = w_none(); } acc }\n\
             fn main() { println(f(Option.Some(W { a: seed(), b: 2, c: 3, d: 4, e: 5, f: 6 }))); }\n",
            &["7"],
            "b13-no-rebind-pod-control",
        );
}

/// B-2026-09-09-17 — the same two-owner shape as
/// [`asan_caller_owned_boxed_param_payload_rebound_to_a_local_is_freed_once`],
/// in the spelling that carries no `mut`.
///
/// B-2026-09-09-13 gated the caller's stand-down on `mut_rebinds` alone,
/// reasoning that "the reassignment is what frees the displaced box". True,
/// and not the only way: SCOPE EXIT frees it too and needs no `mut`, so a
/// bare `let y = x;` gives the callee-side local a drop over a box the
/// caller also registered. Measured at f6f5818e with -13 and -16 both
/// fixed, three calls, `KARAC_OPT_LEVEL=0` + `KARAC_AUTO_PAR=0` under
/// valgrind — frees MINUS allocs, which is stable where valgrind's
/// per-stack error count is not:
///
///   Option[K]     let y = x               +3   (1 per call)
///   Result[K,i64] let y = x               +3
///   Option[R2]    let y = x              +12   (4 per call)
///   Option[K]     let y = x; let z = y;   +3   — does NOT scale with the
///                                              number of rebindings
///
/// The struct payload is the loud one for the same reason it was under -16:
/// its three `String` fields each get a second free, where the enum
/// spelling loses only the box.
///
/// CELL 2 FAILS FOR A DIFFERENT REASON THAN THE REST, and that is why it is
/// here rather than folded into cell 1. Widening the predicate fixed cells
/// 1, 3 and 4 and left cell 2 byte-identical at +3, because B-2026-09-09-13
/// wired the stand-down into `owned_boxed_option_param_struct` ALONE: its
/// peer `owned_boxed_result_param_structs` never asked
/// `callee_rebinds_param_whole` at all, so on that path the `mut`-versus-
/// immutable distinction was never reached. The fix is therefore two
/// changes, and this cell is what tells them apart — it stays red if only
/// the predicate is widened.
///
/// `-16`'s own `Result` cell could not catch that: it is `Result[Val, i64]`
/// with an ENUM payload, which the arm's `boxed_param_payload_owns_its_box`
/// filter declines to register in the first place. It was passing because
/// the arm never fires for that shape, not because the arm stands down
/// correctly — coverage in appearance only. Both of `-16`'s mutable cells
/// were re-measured against the Result-side stand-down and stay clean
/// (17/17 and 18/18 allocs/frees), which is the check that says this change
/// does not convert them into leaks.
///
/// THE THREE CONTROLS ARE THE POINT OF THIS FIXTURE, not the four cells
/// above. Widening the predicate makes the caller stand down more often,
/// and the error direction that buys is a LEAK — nobody owns the box. Each
/// control was clean before the fix and must stay clean after:
///
///   * a NO-REBIND struct payload, String-bearing, which is exactly the
///     shape a blanket stand-down would strand;
///   * a NO-REBIND POD payload, where there is no interior to muddy the
///     reading;
///   * a LOCAL-source immutable rebind, which says the axis is the source
///     being a PARAM and not the rebind itself — both registrations are in
///     one frame there, where the rebind disarms its source.
#[test]
fn asan_caller_owned_boxed_param_payload_rebound_immutably_is_freed_once() {
    const PRE: &str = "struct R2 { s: String, t: String, u: String }\n\
             enum K { A(R2), B }\n\
             fn seed() -> i64 { env.args().len() }\n\
             fn mkr(i: i64) -> R2 { return R2 { s: f\"ssssssss{i}{seed()}\", t: f\"tttttttt{i}\", u: f\"uuuuuuuu{i}\" }; }\n";

    // 1 — the row's cell: a boxed user ENUM payload, immutably rebound.
    assert_clean_asan_run(
            &format!(
                "{PRE}\
                 fn show(x: Option[K]) {{ let y = x;\n\
                 \x20  match y {{ Option.Some(K.A(r)) => {{ println(f\"a:{{r.s}}\"); }} _ => {{ println(\"other\"); }} }} }}\n\
                 fn main() {{ let mut i = 0; while i < 2 {{ show(Option.Some(K.A(mkr(i)))); i = i + 1; }} }}\n"
            ),
            &["a:ssssssss01", "a:ssssssss11"],
            "b17-option-enum-immutable-rebind",
        );

    // 2 — the `Result` spelling, which asks the same arg-site question.
    assert_clean_asan_run(
            &format!(
                "{PRE}\
                 fn show(x: Result[K, i64]) {{ let y = x;\n\
                 \x20  match y {{ Result.Ok(K.A(r)) => {{ println(f\"a:{{r.s}}\"); }} _ => {{ println(\"other\"); }} }} }}\n\
                 fn main() {{ let mut i = 0; while i < 2 {{ show(Result.Ok(K.A(mkr(i)))); i = i + 1; }} }}\n"
            ),
            &["a:ssssssss01", "a:ssssssss11"],
            "b17-result-enum-immutable-rebind",
        );

    // 3 — the STRUCT payload, four times louder: each `String` field is
    //     freed twice, not just the box.
    assert_clean_asan_run(
            &format!(
                "{PRE}\
                 fn show(x: Option[R2]) {{ let y = x;\n\
                 \x20  match y {{ Option.Some(r) => {{ println(f\"a:{{r.s}}\"); }} _ => {{ println(\"other\"); }} }} }}\n\
                 fn main() {{ let mut i = 0; while i < 2 {{ show(Option.Some(mkr(i))); i = i + 1; }} }}\n"
            ),
            &["a:ssssssss01", "a:ssssssss11"],
            "b17-option-struct-immutable-rebind",
        );

    // 4 — a CHAIN of immutable rebinds. `close_rebind_aliases` already
    //     walked this set to build the alias chain; only the last step
    //     discarded it, so this cell fails identically without the fix.
    assert_clean_asan_run(
            &format!(
                "{PRE}\
                 fn show(x: Option[K]) {{ let y = x; let z = y;\n\
                 \x20  match z {{ Option.Some(K.A(r)) => {{ println(f\"a:{{r.s}}\"); }} _ => {{ println(\"other\"); }} }} }}\n\
                 fn main() {{ let mut i = 0; while i < 2 {{ show(Option.Some(K.A(mkr(i)))); i = i + 1; }} }}\n"
            ),
            &["a:ssssssss01", "a:ssssssss11"],
            "b17-chained-immutable-rebind",
        );

    // 5 — CONTROL, no rebind, String-bearing struct payload. The caller
    //     must KEEP its registration here; a stand-down that fired without
    //     a rebind would leak all three fields and the box.
    assert_clean_asan_run(
            &format!(
                "{PRE}\
                 fn show(x: Option[R2]) {{\n\
                 \x20  match x {{ Option.Some(r) => {{ println(f\"a:{{r.s}}\"); }} _ => {{ println(\"other\"); }} }} }}\n\
                 fn main() {{ let mut i = 0; while i < 2 {{ show(Option.Some(mkr(i))); i = i + 1; }} }}\n"
            ),
            &["a:ssssssss01", "a:ssssssss11"],
            "b17-no-rebind-struct-control",
        );

    // 6 — CONTROL, no rebind, POD payload: no interior at all, so this one
    //     reads the box's ownership on its own.
    assert_clean_asan_run(
            "struct W { a: i64, b: i64, c: i64, d: i64, e: i64, g: i64 }\n\
             fn seed() -> i64 { env.args().len() }\n\
             fn show(x: Option[W]) {\n\
             \x20  match x { Option.Some(w) => { println(f\"a:{w.a}\"); } _ => { println(\"other\"); } } }\n\
             fn main() { let mut i = 0; while i < 2 { show(Option.Some(W { a: seed(), b: 2, c: 3, d: 4, e: 5, g: 6 })); i = i + 1; } }\n",
            &["a:1", "a:1"],
            "b17-no-rebind-pod-control",
        );

    // 7 — CONTROL, the rebind source is a LOCAL rather than the param. Both
    //     registrations are in one frame, the rebind disarms its source,
    //     and this was clean throughout: it is what says the axis is the
    //     PARAM.
    assert_clean_asan_run(
            &format!(
                "{PRE}\
                 fn mko(i: i64) -> Option[K] {{ return Option.Some(K.A(mkr(i))); }}\n\
                 fn show(i: i64) {{ let x = mko(i); let y = x;\n\
                 \x20  match y {{ Option.Some(K.A(r)) => {{ println(f\"a:{{r.s}}\"); }} _ => {{ println(\"other\"); }} }} }}\n\
                 fn main() {{ let mut i = 0; while i < 2 {{ show(i); i = i + 1; }} }}\n"
            ),
            &["a:ssssssss01", "a:ssssssss11"],
            "b17-local-source-control",
        );
}

/// B-2026-09-10-11 — a `shared` / `par` type used DIRECTLY as a
/// non-shared enum variant payload was never rc-dec'd: the payload word
/// holds an RC pointer and nothing released it. 16 B in 1 block per value
/// at `-O0`, correct output on every surface.
///
/// THE ROW'S OWN ATTRIBUTION WAS WRONG, and recording that is half the
/// point of this fixture. It blamed the caller-side entry-copy predicates
/// on the return route (`arg_is_entry_copied_heap_enum`), from a repro that
/// went through two function calls. It reproduces with NO FUNCTION CALL IN
/// THE PROGRAM — cell 1 is `let z = Et.A(Sh { n: 3 });` — so no argument
/// gate is involved, and neither of the two repairs the row proposed
/// (exclude a shared payload from those predicates / give that registrar an
/// rc-dec arm) would have touched it.
///
/// WHERE IT WAS: a TABLE-TIMING hazard in `enum_drop_kind_for_type_expr`.
/// All three of its struct arms mean to exclude a shared payload and none
/// of them can — their guard is `!shared_types.contains_key(..)`, and
/// `shared_types` is filled by the struct LLVM build, which runs AFTER
/// `declare_enums`. So a shared struct payload classified `NestedStruct`,
/// an INLINE aggregate, and that arm walked the RC pointer as the struct's
/// own fields and called a value-drop that does not exist for a shared
/// type: the emitted switch GEP'd the payload word and did nothing at all.
/// A shared ENUM payload fell to the `_ => None` tail instead, whose doc
/// premise ("handled by the shared-type RC machinery") holds for a payload
/// of a SHARED enum and not for one of a plain enum.
///
/// The classifier now asks `shared_type_decl_names` — the name-only set
/// `register_struct_metadata` fills for exactly this window, whose doc says
/// B-2026-06-14-28 added it so this classifier could see that a struct
/// FIELD's type is shared. The DIRECT payload position was never wired to
/// it. B-2026-09-12-10 hit the same hazard from the TUPLE-payload side and
/// fixed it the same way with the sibling name-only set, which is why this
/// is a third instance of one pattern rather than a new one.
///
/// THIS FIXTURE ONLY BITES ON THE `-O0` RATCHET LEG. Measured, not assumed:
/// against the pre-fix `src/` these cells are CLEAN at `KARAC_OPT_LEVEL=2`
/// (8 allocs / 8 frees — the optimizer deletes the RC allocation wholesale
/// for a value nothing reads) and LEAK at `=0` (9 allocs / 8 frees). Same
/// caveat, same reason, as `asan_indexed_array_payload_interior_has_exactly_one_owner`:
/// a green `--features llvm` run is no evidence about it, and
/// `scripts/asan-o0-leg.sh` is its gate.
///
/// Cells 8-11 are the positions that were ALREADY clean and must stay so —
/// they are what placed the fault in this one classification rather than in
/// the RC machinery. Cell 11 is the classifier's OTHER consumer: a `par`
/// channel element reaches `enum_drop_kind_for_type_expr` from
/// `channel.rs`'s `elem_keeps_source_owner` during function compilation,
/// when `shared_types` IS populated, so it was already getting the `None`
/// tail and is byte-identical across this change. (A `shared` non-`par`
/// type cannot cross a channel at all — the typechecker rejects it with
/// `E_NOT_CROSS_TASK` — which is why that cell must be spelled `par`.)
///
/// THE CLASSIFIER ARM ALONE IS NOT THE WHOLE FIX, and cells 12-17 are why.
/// Making the drop switch real exposed a latent second owner that the
/// no-op switch had been masking: a binding moved into an owned `self`
/// keeps its `EnumDrop`, because a method RECEIVER never reaches
/// `move_declined_copy_struct_arg_for` — "the shared by-value-owned-arg
/// choke point so every call-arg site is covered". Two decs on one RC
/// block. So the fix is four gates that must agree, and each cell below
/// pins one of them:
///
///   * the classifier arm (`shared_type_decl_names`) — cells 1-11;
///   * the receiver-side memory retraction, the peer of the BODIES one
///     B-2026-08-01-7 already put beside it — cells 12, 13, gated on
///     `SelfParam::Owned` so cell 14 keeps its drop;
///   * `enum_param_owned_by_transfer`, which the retraction asks and which
///     admits a payload the entry copy cannot duplicate — cell 17 is the
///     payload it CAN duplicate and must stay clean;
///   * `enum_needs_scope_exit_owner`, the callee's half of that same
///     bargain — cell 15 leaked with the caller standing down and the
///     callee registering nothing, and cell 16 had no owner at all.
///
/// THAT REMAINDER IS NOW CLOSED, and cells 18-27 are it (B-2026-09-17-15).
/// It read: a GENERIC enum (`enum Box2[T] { V(T) }` over a shared `T`)
/// classifies the ERASED `T`, which no name set can contain, so it needs
/// per-instantiation drop synthesis — `field_drop_kinds` is written once
/// per enum NAME in `declare_enums`; and `Box2[String]` is clean, so
/// something already resolves the instantiation for a buffer payload.
///
/// The something was `user_enum_boxed_payload_variants`, and the reason it
/// did not reach a shared payload is the whole answer: its population is
/// payloads WIDER than the erased area, because it asks whether
/// `coerce_to_payload_words` heap-boxed one. An RC handle is exactly ONE
/// word, so it never boxes and never entered that path. The repair is
/// `generic_enum_shared_payload_arms` + `track_rc_generic_enum_var`, which
/// queue the same tag-guarded `RcDecOption` per shared arm that
/// `track_rc_result_var` queues for a SEEDED all-`None` layout — cells 25-27
/// are the boxed / fits / no-payload controls that place it.
///
/// One remainder of its own, pinned in `e2e_generic_enum_shared_payload_is_rc_released`
/// rather than here because it still leaks: `let b = a` over an
/// already-bound generic value registers at neither binding, and closing it
/// needs the source-defusing channel that is `Option`-only today.
///
/// Measured on this tree: all seventeen cells clean at `-O0` under
/// `valgrind --leak-check=full` — and the verdict asserts on
/// `ERROR SUMMARY: 0 errors` as well as on the leak line, because
/// `All heap blocks were freed -- no leaks are possible` PRINTS ALONGSIDE
/// `Invalid read` / `Invalid write` when a use-after-free frees everything
/// exactly once. A leak-only verdict read cells 12 and 13 as passing; that
/// is what a per-cell PASS/FAIL line has to guard against. Each stdout
/// below is byte-identical across `--interp` / jit / `karac build` /
/// `KARAC_AUTO_PAR=0 karac build`, at both opt levels.
#[test]
fn asan_shared_payload_in_plain_enum_is_rc_released() {
    // 1 — THE MINIMAL REPRO. No function call in the program.
    assert_clean_asan_run(
        r#"shared struct Sh { n: i64 }
enum Et { A(Sh), B }
fn main() { let z = Et.A(Sh { n: 3 }); println("ok"); }
"#,
        &["ok"],
        "b1011-bare-let-shared-payload",
    );
    // 2 — a shared ENUM payload, which fell to the `None` tail rather than
    //     to `NestedStruct`. Same leak, other half of the classifier.
    assert_clean_asan_run(
        r#"shared enum Inner { X(i64), Y }
enum Et { A(Inner), B }
fn main() { let z = Et.A(Inner.X(3)); println("ok"); }
"#,
        &["ok"],
        "b1011-shared-enum-payload",
    );
    // 3 — a `par` payload. `shared_type_decl_names` records `is_par` too,
    //     and the RC machinery is the same; a fix keyed on `shared` alone
    //     would leave this one leaking.
    assert_clean_asan_run(
        r#"par struct Pa { n: i64 }
enum Et { A(Pa), B }
fn main() { let z = Et.A(Pa { n: 3 }); println("ok"); }
"#,
        &["ok"],
        "b1011-par-payload",
    );
    // 4 — a payload WIDER than its allotted payload word, where the word is
    //     still the RC pointer. 32 B rather than 16, and the cell that would
    //     fail if the fix ever read the word as a heap BOX pointer.
    assert_clean_asan_run(
        r#"shared struct Big { a: i64, b: i64, c: i64 }
enum Et { A(Big), B }
fn main() {
    let z = Et.A(Big { a: 1, b: 2, c: 3 });
    match z { Et.A(g) => { println(f"a:{g.a} b:{g.b} c:{g.c}") } Et.B => { println("b") } }
}
"#,
        &["a:1 b:2 c:3"],
        "b1011-wide-shared-payload-read",
    );
    // 5 — THE ROW'S OWN REPRO, the return route through two calls. Kept
    //     because it is what the row reported, not because the calls matter.
    assert_clean_asan_run(
        r#"shared struct Sh { n: i64 }
enum Et { A(Sh), B }
fn mket(i: i64) -> Et { return Et.A(Sh { n: i }); }
fn passt(e: Et) -> Et { return e; }
fn main() { let z = passt(mket(3)); println("ok"); }
"#,
        &["ok"],
        "b1011-return-route-repro",
    );
    // 6 — MOVED OUT and dropped once. `passt` zeroes the payload word
    //     (`move.enum.suppress.wp`) before dropping its vacated param, so
    //     the new null guard is what keeps this at exactly ONE dec. Without
    //     that guard this cell is a double free, not a leak.
    assert_clean_asan_run(
        r#"shared struct Sh { n: i64 }
enum Et { A(Sh), B }
fn passt(e: Et) -> Et { return e; }
fn main() {
    let z = passt(Et.A(Sh { n: 5 }));
    match z { Et.A(s) => { println(f"n:{s.n}") } Et.B => { println("b") } }
}
"#,
        &["n:5"],
        "b1011-moved-out-decs-once",
    );
    // 7 — TWO payload-bearing variants and a non-shared heap one beside
    //     them: both RC blocks released, and the `String` still freed by the
    //     arm it always was.
    assert_clean_asan_run(
        r#"shared struct Sh { n: i64 }
enum Et { A(Sh), C(Sh), S(String), B }
fn main() {
    let z = Et.A(Sh { n: 3 });
    let w = Et.C(Sh { n: 4 });
    let y = Et.S(f"b1011-aaaaaaaaaa");
    println("ok");
}
"#,
        &["ok"],
        "b1011-two-shared-plus-string",
    );
    // 8 — CONTROL: the shared value wrapped in ONE plain struct, which is
    //     B-2026-06-14-28's `struct_owns_shared_field` arm. Clean
    //     throughout, and the cell that localised the fault: one field of
    //     indirection away, the rc-dec was already emitted.
    assert_clean_asan_run(
        r#"shared struct Sh { n: i64 }
struct Wrap { e: Sh }
enum Et { A(Wrap), B }
fn main() { let z = Et.A(Wrap { e: Sh { n: 3 } }); println("ok"); }
"#,
        &["ok"],
        "b1011-struct-wrapped-control",
    );
    // 9 — CONTROL: a plain (non-shared) struct payload, the class
    //     `NestedStruct` is actually for. Must keep its inline walk.
    assert_clean_asan_run(
        r#"struct Sh2 { n: i64 }
enum Et { A(Sh2), B }
fn mket(i: i64) -> Et { return Et.A(Sh2 { n: i }); }
fn passt(e: Et) -> Et { return e; }
fn main() { let z = passt(mket(3)); println("ok"); }
"#,
        &["ok"],
        "b1011-plain-payload-control",
    );
    // 10 — CONTROL: a SHARED enum wrapper, where the box's own rc-drop
    //      already owns the payload. `emit_enum_drop_switch` declines such
    //      an enum outright (`layout.is_shared`), which is what makes the
    //      new arm incapable of doubling with it — this cell is the proof.
    assert_clean_asan_run(
        r#"shared struct Sh { n: i64 }
shared enum Et { A(Sh), B }
fn mket(i: i64) -> Et { return Et.A(Sh { n: i }); }
fn passt(e: Et) -> Et { return e; }
fn main() { let z = passt(mket(3)); println("ok"); }
"#,
        &["ok"],
        "b1011-shared-enum-wrapper-control",
    );
    // 11 — CONTROL: the classifier's OTHER consumer. A `par` channel
    //      element asks the same predicate from `channel.rs`, after
    //      `shared_types` is populated, so it was already getting `None`
    //      and must be byte-identical.
    assert_clean_asan_run(
        r#"par struct Pa { n: i64 }
fn main() {
    let (tx, rx): (Sender[Pa], Receiver[Pa]) = Channel.new();
    let s = Pa { n: 7 };
    tx.send(s);
    let v = rx.recv();
    println(f"got:{v.n}")
}
"#,
        &["got:7"],
        "b1011-par-channel-elem-control",
    );
    // 12 — AN OWNED-`self` METHOD, named receiver. THE UAF CELL: making the
    //      drop switch real exposed that a binding moved into an owned
    //      `self` keeps its `EnumDrop` while the callee's frame owns the
    //      same value. Two decs on one RC block — `Invalid read` +
    //      `Invalid write` on a freed 16-byte block, and
    //      `malloc(): unaligned tcache chunk detected` under `karac run`.
    //      A method RECEIVER is not an ARG, so it never reached
    //      `move_declined_copy_struct_arg_for`, "the shared
    //      by-value-owned-arg choke point so every call-arg site is
    //      covered".
    assert_clean_asan_run(
        r#"shared struct Sh { n: i64 }
enum Et { A(Sh), B }
impl Et { fn passm(self) -> Et { return self; } }
fn main() { let a = Et.A(Sh { n: 3 }); let z = a.passm(); println("ok"); }
"#,
        &["ok"],
        "b1011-owned-self-method-named-recv",
    );
    // 13 — the same method on a FRESH TEMP receiver, which is materialized
    //      into a synth `__urecv_tmp` slot. A second spelling of cell 12
    //      reached through a different caller-side slot, so a fix that only
    //      handles the named binding still fails here.
    assert_clean_asan_run(
        r#"shared struct Sh { n: i64 }
enum Et { A(Sh), B }
impl Et { fn passm(self) -> Et { return self; } }
fn main() { let z = Et.A(Sh { n: 3 }).passm(); println("ok"); }
"#,
        &["ok"],
        "b1011-owned-self-method-temp-recv",
    );
    // 14 — CONTROL: a BORROWING receiver takes no ownership, so it must
    //      KEEP its drop. The cell that fails if the receiver retraction
    //      forgets to ask about `self`'s mode — the opposite polarity to
    //      cells 12 and 13, and the reason the gate is
    //      `SelfParam::Owned` rather than "is a method".
    assert_clean_asan_run(
        r#"shared struct Sh { n: i64 }
enum Et { A(Sh), B }
impl Et { fn peek(ref self) -> i64 { match self { Et.A(s) => { return s.n; } Et.B => { return 0; } } } }
fn main() { let a = Et.A(Sh { n: 3 }); println(f"p:{a.peek()}"); }
"#,
        &["p:3"],
        "b1011-ref-self-receiver-keeps-its-drop",
    );
    // 15 — A BY-VALUE PARAM, the ARG spelling of cells 12-13's move. This
    //      is the cell that LEAKED once the caller-side retraction was
    //      admitted for this kind and the callee-side registration was not:
    //      `enum_needs_scope_exit_owner` returns early on a payload
    //      `enum_has_heap_payload` cannot see, so the callee registered
    //      nothing while the caller stood down. Both frames key off one
    //      predicate on purpose; this cell is what proves they agree.
    assert_clean_asan_run(
        r#"shared struct Sh { n: i64 }
enum Et { A(Sh), B }
fn use2(e: Et) { match e { Et.A(s) => { println(f"u:{s.n}") } Et.B => { println("b") } } }
fn main() { let z = Et.A(Sh { n: 3 }); use2(z); println("ok"); }
"#,
        &["u:3", "ok"],
        "b1011-by-value-param-both-frames-agree",
    );
    // 16 — A DISCARDED ctor result. It registers no cleanup action of its
    //      own, so it was the one spelling still leaking after the
    //      classifier arm alone; the scope-exit-owner gate is what gives it
    //      an owner. NOT the discard in general — the same statement over a
    //      `String` payload was clean throughout (cell 17).
    assert_clean_asan_run(
        r#"shared struct Sh { n: i64 }
enum Et { A(Sh), B }
fn mket(i: i64) -> Et { return Et.A(Sh { n: i }); }
fn main() { mket(3); println("ok"); }
"#,
        &["ok"],
        "b1011-discarded-ctor-result",
    );
    // 17 — CONTROL: the BUFFER payload through the same owned-`self`
    //      method. It is clean both before and after, and it must stay
    //      clean, because there the callee's entry copy
    //      (`deep_copy_enum_heap_payload_in_place`) really does duplicate
    //      the payload — so the caller's original still needs its own free
    //      and standing it down would be a LEAK. That asymmetry is why the
    //      retraction's gate is `enum_param_owned_by_transfer` ("the entry
    //      copy has no arm for the kind") rather than "the receiver moved".
    assert_clean_asan_run(
        r#"enum Et { S(String), B }
impl Et { fn passm(self) -> Et { return self; } }
fn main() {
    let a = Et.S(f"b1011-buffer-aaaaaaaa");
    let z = a.passm();
    match z { Et.S(s) => { println(f"s:{s}") } Et.B => { println("b") } }
}
"#,
        &["s:b1011-buffer-aaaaaaaa"],
        "b1011-buffer-payload-owned-self-control",
    );
    // 18 — B-2026-09-17-15, THE GENERIC LEG, which cell 17's paragraph
    //      above recorded as this fixture's one remainder. `Box2[T]` at
    //      `T = Sh` classifies the ERASED `T`, so all four gates agreed
    //      there was nothing to own: 16 B per value, the same block cell 1
    //      strands without the parent fix.
    assert_clean_asan_run(
        r#"shared struct Sh { n: i64 }
enum Box2[T] { V(T), N }
fn main() { let z: Box2[Sh] = Box2.V(Sh { n: 3 }); println("ok"); }
"#,
        &["ok"],
        "b1715-generic-bare-let",
    );
    // 19 — the row's own repro: the value makes a by-value round trip
    //      through a generic-enum param and back into a binding.
    assert_clean_asan_run(
        r#"shared struct Sh { n: i64 }
enum Box2[T] { V(T), N }
fn passb(b: Box2[Sh]) -> Box2[Sh] { return b }
fn main() { let z = passb(Box2.V(Sh { n: 3 })); println("ok"); }
"#,
        &["ok"],
        "b1715-generic-roundtrip",
    );
    // 20 — an arm that BINDS the payload out. The binding takes and
    //      releases its own `+1`, so the envelope's dec must still happen
    //      exactly once; a fix that stood down here would leak and one that
    //      double-counted would free the node under the arm.
    assert_clean_asan_run(
        r#"shared struct Sh { n: i64 }
enum Box2[T] { V(T), N }
fn main() {
    let z: Box2[Sh] = Box2.V(Sh { n: 3 });
    match z { Box2.V(s) => { println(f"got{s.n}") } Box2.N => { println("no") } }
}
"#,
        &["got3"],
        "b1715-generic-arm-binds-payload",
    );
    // 21 — a FRESH TEMP handed straight to a by-value param. No binding
    //      exists for the let-site registration to hang on, which is why
    //      the param site is a second leg rather than a redundant one.
    assert_clean_asan_run(
        r#"shared struct Sh { n: i64 }
enum Box2[T] { V(T), N }
fn eat(b: Box2[Sh]) { println("eaten") }
fn main() { eat(Box2.V(Sh { n: 3 })); println("ok"); }
"#,
        &["eaten", "ok"],
        "b1715-generic-freshtemp-arg",
    );
    // 22 — THE DOUBLE-DEC CELL. A named binding handed to a by-value param
    //      has an owner site at BOTH ends. It runs once because both gate on
    //      `crate::result_escape`: a binding that escapes into a param is
    //      absent from the let-site set, so the terminal consumer's dec is
    //      the only one — the arbitration `Result[shared]` has used since
    //      B-2026-07-12-24. Without that, an invalid free, not a leak.
    assert_clean_asan_run(
        r#"shared struct Sh { n: i64 }
enum Box2[T] { V(T), N }
fn eat(b: Box2[Sh]) { println("eaten") }
fn main() { let z: Box2[Sh] = Box2.V(Sh { n: 3 }); eat(z); println("ok"); }
"#,
        &["eaten", "ok"],
        "b1715-generic-named-arg",
    );
    // 23 — a TWO-PARAMETER generic, carrying the shared payload in one arm
    //      and a scalar in the other, in one program. The registration is
    //      per-ARM and tag-guarded; a per-ENUM answer would dec on the `R`
    //      tag too and free an integer as a pointer.
    assert_clean_asan_run(
        r#"shared struct Sh { n: i64 }
enum Pair[A, B] { L(A), R(B) }
fn main() {
    { let p: Pair[Sh, i64] = Pair.L(Sh { n: 3 }); println("l"); }
    { let q: Pair[Sh, i64] = Pair.R(7); println("r"); }
}
"#,
        &["l", "r"],
        "b1715-generic-two-params-one-shared-arm",
    );
    // 24 — a `par` payload through the generic envelope, the erased peer of
    //      cell 3. `shared_types` records `is_par` and the RC machinery is
    //      the same; a fix keyed on the `shared` spelling alone leaves this
    //      one leaking.
    assert_clean_asan_run(
        r#"par struct Pa { n: i64 }
enum Box2[T] { V(T), N }
fn main() { let z: Box2[Pa] = Box2.V(Pa { n: 3 }); println("ok"); }
"#,
        &["ok"],
        "b1715-generic-par-payload",
    );
    // 25-27 — the three CONTROLS that place the fault between the two
    //      generic machineries rather than in either. A `String` payload is
    //      3 words against the 1-word erased area, so `coerce_to_payload_words`
    //      heap-BOXES it and `user_enum_boxed_payload_variants` already owned
    //      it; a scalar FITS and owns nothing; the unit variant has no
    //      payload word at all. Only a ONE-WORD RC handle falls between the
    //      two — too narrow to box, too erased to classify — and all three
    //      of these were clean before this fix and must stay so.
    assert_clean_asan_run(
        r#"enum Box2[T] { V(T), N }
fn main() { let z: Box2[String] = Box2.V("aaaaaaaaaaaaaaaa"); println("ok"); }
"#,
        &["ok"],
        "b1715-generic-string-payload-control",
    );
    assert_clean_asan_run(
        r#"enum Box2[T] { V(T), N }
fn main() { let z: Box2[i64] = Box2.V(7); println("ok"); }
"#,
        &["ok"],
        "b1715-generic-scalar-payload-control",
    );
    assert_clean_asan_run(
        r#"shared struct Sh { n: i64 }
enum Box2[T] { V(T), N }
fn main() { let z: Box2[Sh] = Box2.N; println("ok"); }
"#,
        &["ok"],
        "b1715-generic-unit-variant-control",
    );
}

/// B-2026-09-10-9 — the payload's user `Drop` BODIES ran through a box the
/// CALLEE had already freed.
///
/// This is the gate for the defect itself; `tests/codegen.rs`'s
/// `e2e_boxed_tuple_payload_param_runs_its_element_bodies` pins the output
/// of the same family. It is an ASAN fixture rather than a leak one because
/// the allocation count BALANCES exactly — valgrind reports 18 allocs, 18
/// frees and nothing lost — and the row was filed on the belief that the
/// balance meant no memory tool could see it. Valgrind saw it on that same
/// run, in the section above the leak summary: two `Invalid read of size 8`,
/// each `inside a block of size 64 free'd`.
///
/// WHICH LEG REPORTS WHAT, measured with the fix reverted, because the two
/// answers are different and only one of them is an ASAN report:
///
///   * the DEFAULT leg (`-O2`, object NOT instrumented) fails on the
///     OUTPUT. ASAN's quarantine fill is what the body reads, so element 0
///     prints `d-4702111234474983746` (0xbe…) deterministically rather than
///     the run-to-run heap addresses an ordinary build shows. Reliable, but
///     it is an assertion failure, not a sanitizer diagnosis.
///   * the INSTRUMENTED leg (`scripts/asan-instrumented-leg.sh`, i.e.
///     `KARAC_SANITIZE_ADDRESS=1`) reports the defect for what it is:
///     `ERROR: AddressSanitizer: heap-use-after-free`, `READ of size 32`,
///     `freed by thread T0 here`, `in Rt.drop`.
///
/// So the uninstrumented legs catch this only through the printed value —
/// which is the concrete reason B-2026-09-07-40's instrumentation leg
/// exists, on a defect filed after it landed and still described in its row
/// as invisible to sanitizers.
#[test]
fn asan_boxed_tuple_payload_param_bodies_precede_the_box_free() {
    const PRE: &str = "fn seed() -> i64 { env.args().len() }\n\
             struct Rt { id: i64, name: String }\n\
             impl Drop for Rt { fn drop(mut ref self) { println(f\"d{self.id}\") } }\n";

    // 1 — the row's shape. A fresh temp handed to a by-value param, an arm
    //     that binds the whole tuple and never reads it. The bodies used to
    //     run after the callee's `boxdrop_free`.
    assert_clean_asan_run(
            &format!(
                "{PRE}\
                 fn takeR(x: Option[(Rt, Rt)]) {{\n\
                 \x20  match x {{ Some(t) => {{ println(\"ok\") }} None => {{ println(\"n\") }} }} }}\n\
                 fn main() {{ let mut i = 0; while i < 2 {{\n\
                 \x20  takeR(Some((Rt {{ id: 70 + i + seed(), name: f\"aaaaaaaa{{i}}\" }},\n\
                 \x20               Rt {{ id: 80 + i, name: f\"bbbbbbbb{{i}}\" }}))); i = i + 1; }} }}\n"
            ),
            &["ok", "d71", "d80", "ok", "d72", "d81"],
            "b9-boxed-tuple-param-whole-binding",
        );

    // 2 — the `Result` twin. It only joined this family when 92eeb8a84 gave
    //     `Result` the box owner it lacked; before that the box leaked and
    //     the caller's walk read live memory.
    assert_clean_asan_run(
            &format!(
                "{PRE}\
                 fn takeR(x: Result[(Rt, Rt), i64]) {{\n\
                 \x20  match x {{ Ok(t) => {{ println(\"ok\") }} Err(e) => {{ println(\"n\") }} }} }}\n\
                 fn main() {{ let mut i = 0; while i < 2 {{\n\
                 \x20  takeR(Result.Ok((Rt {{ id: 70 + i + seed(), name: f\"aaaaaaaa{{i}}\" }},\n\
                 \x20                    Rt {{ id: 80 + i, name: f\"bbbbbbbb{{i}}\" }}))); i = i + 1; }} }}\n"
            ),
            &["ok", "d71", "d80", "ok", "d72", "d81"],
            "b9-boxed-tuple-param-result-twin",
        );

    // 3 — a NAMED local. The caller's let-site walk is disarmed at the
    //     call-arg move, so this spelling had no caller-side reader to
    //     fault; it printed no body at all on either backend. Included
    //     because the callee-side registration now has to serve it, and a
    //     registration that fired twice here would double-free the
    //     interior rather than merely double-print.
    assert_clean_asan_run(
            &format!(
                "{PRE}\
                 fn takeR(x: Option[(Rt, Rt)]) {{\n\
                 \x20  match x {{ Some(t) => {{ println(\"ok\") }} None => {{ println(\"n\") }} }} }}\n\
                 fn main() {{ let mut i = 0; while i < 2 {{\n\
                 \x20  let o: Option[(Rt, Rt)] = Some((Rt {{ id: 70 + i + seed(), name: f\"aaaaaaaa{{i}}\" }},\n\
                 \x20                                  Rt {{ id: 80 + i, name: f\"bbbbbbbb{{i}}\" }}));\n\
                 \x20  takeR(o); i = i + 1; }} }}\n"
            ),
            &["ok", "d71", "d80", "ok", "d72", "d81"],
            "b9-boxed-tuple-param-named-local",
        );

    // 4 — a DESTRUCTURING arm, where the leaves own the elements. The
    //     arm-level disarm still has to fire here: leaving the place armed
    //     as well runs each element's body twice, and its `drop_user_drop_
    //     fields_of_value` half frees each `String` twice with it.
    assert_clean_asan_run(
            &format!(
                "{PRE}\
                 fn takeR(x: Option[(Rt, Rt)]) {{\n\
                 \x20  match x {{ Some((a, b)) => {{ println(f\"a{{a.id}}\") }} None => {{ println(\"n\") }} }} }}\n\
                 fn main() {{ let mut i = 0; while i < 2 {{\n\
                 \x20  takeR(Some((Rt {{ id: 70 + i + seed(), name: f\"aaaaaaaa{{i}}\" }},\n\
                 \x20               Rt {{ id: 80 + i, name: f\"bbbbbbbb{{i}}\" }}))); i = i + 1; }} }}\n"
            ),
            &["a71", "d71", "d80", "a72", "d72", "d81"],
            "b9-boxed-tuple-param-destructured",
        );

    // 5 — the arm FORWARDS its binding into a second call, so the tuple
    //     crosses one more frame before it dies. One set of bodies is owed.
    assert_clean_asan_run(
            &format!(
                "{PRE}\
                 fn eat(p: (Rt, Rt)) {{ println(\"eat\") }}\n\
                 fn takeR(x: Option[(Rt, Rt)]) {{\n\
                 \x20  match x {{ Some(t) => {{ eat(t) }} None => {{ println(\"n\") }} }} }}\n\
                 fn main() {{ let mut i = 0; while i < 2 {{\n\
                 \x20  takeR(Some((Rt {{ id: 70 + i + seed(), name: f\"aaaaaaaa{{i}}\" }},\n\
                 \x20               Rt {{ id: 80 + i, name: f\"bbbbbbbb{{i}}\" }}))); i = i + 1; }} }}\n"
            ),
            &["eat", "d71", "d80", "eat", "d72", "d81"],
            "b9-boxed-tuple-param-forwarded",
        );

    // 6 — CONTROL, a boxed STRUCT payload: the callee's arm declines it and
    //     the CALLER keeps the box, so its walk always preceded its own
    //     free. Clean before this commit and after it.
    assert_clean_asan_run(
            &format!(
                "{PRE}\
                 struct Wt {{ a: Rt, b: Rt }}\n\
                 fn takeW(x: Option[Wt]) {{\n\
                 \x20  match x {{ Some(t) => {{ println(\"ok\") }} None => {{ println(\"n\") }} }} }}\n\
                 fn main() {{ let mut i = 0; while i < 2 {{\n\
                 \x20  takeW(Some(Wt {{ a: Rt {{ id: 70 + i + seed(), name: f\"aaaaaaaa{{i}}\" }},\n\
                 \x20                   b: Rt {{ id: 80 + i, name: f\"bbbbbbbb{{i}}\" }} }})); i = i + 1; }} }}\n"
            ),
            &["ok", "d80", "d71", "ok", "d81", "d72"],
            "b9-boxed-struct-payload-control",
        );

    // 7 — CONTROL, an ESCAPING param. `outerT` hands its argument straight
    //     back, so the callee registers nothing — neither box nor bodies —
    //     and the terminal consumer stays the only owner. A bodies
    //     registration that ignored the escape set would run a body over a
    //     value it had already handed on.
    assert_clean_asan_run(
            &format!(
                "{PRE}\
                 fn innerT(x: Option[(Rt, Rt)]) {{\n\
                 \x20  match x {{ Some((a, b)) => {{ println(f\"a{{a.id}}\") }} None => {{ println(\"n\") }} }} }}\n\
                 fn outerT(x: Option[(Rt, Rt)]) -> Option[(Rt, Rt)] {{ return x; }}\n\
                 fn main() {{ let mut i = 0; while i < 2 {{\n\
                 \x20  innerT(outerT(Some((Rt {{ id: 70 + i + seed(), name: f\"aaaaaaaa{{i}}\" }},\n\
                 \x20                       Rt {{ id: 80 + i, name: f\"bbbbbbbb{{i}}\" }})))); i = i + 1; }} }}\n"
            ),
            &["a71", "d71", "d80", "a72", "d72", "d81"],
            "b9-boxed-tuple-escaping-param-control",
        );
}

/// B-2026-09-12-25 — the memory half, and specifically the CEILING leg.
/// `6ea22e3b4` reached through the box correctly and disarmed every bound
/// position, which is right for a leaf at or under the envelope's payload
/// area and a LEAK above it: it left four fixtures red on both ASAN legs.
/// The wide-leaf cells here are that boundary, pinned so the ceiling cannot
/// be dropped again.
///
/// See `codegen::e2e_boxed_user_enum_tuple_variant_payload_destructure_has_one_owner`
/// for the output half and the full cell matrix; this pins the frees.
#[test]
fn asan_boxed_user_enum_tuple_variant_payload_has_one_owner() {
    // THE ROW'S SHAPE, three iterations so the doubled free is not a
    // one-off. `free(): double free detected in tcache 2` before the fix.
    //
    // MEASURE THIS CELL AT `KARAC_OPT_LEVEL=0` — it is the `asan-o0-leg.sh`
    // leg that catches it. At the harness's default `-O2` LLVM deletes the
    // doubled malloc/free pair for the short leaf strings, which is exactly
    // how the row came to record the read-only twin below as clean.
    assert_clean_asan_run(
            "struct R2z { s: String }\n\
             enum Kz { A(R2z), B }\n\
             impl Drop for Kz { fn drop(mut ref self) { println(\"dKz\") } }\n\
             #[allow(partial_move_of_drop_enum)]\n\
             fn show(x: Option[Kz], acc: mut ref Vec[R2z]) { match x { Option.Some(Kz.A(r)) => { acc.push(r) } Option.Some(Kz.B) => {} Option.None => {} } }\n\
             fn main() { let mut acc: Vec[R2z] = []; for i in 0..3 { show(Option.Some(Kz.A(R2z { s: f\"aaaaaaaaaaaaaaaaaaaa-{i}\" })), mut acc); } println(f\"len:{acc.len()}\") }\n",
            &["len:3"],
            "b25-tuple-variant-leaf-escapes",
        );
    // The READ-ONLY twin. The row recorded it as clean on all four surfaces;
    // it aborts identically at `-O0`. Pinned so the escape axis cannot be
    // reintroduced as a gate on a later pass.
    assert_clean_asan_run(
            "struct R2z { s: String }\n\
             enum Kz { A(R2z), B }\n\
             impl Drop for Kz { fn drop(mut ref self) { println(\"dKz\") } }\n\
             #[allow(partial_move_of_drop_enum)]\n\
             fn show(x: Option[Kz]) { match x { Option.Some(Kz.A(r)) => { println(f\"a:{r.s.len()}\") } Option.Some(Kz.B) => {} Option.None => {} } }\n\
             fn main() { for i in 0..3 { show(Option.Some(Kz.A(R2z { s: f\"bbbbbbbbbbbbbbbbbbbb-{i}\" }))); } println(\"end\") }\n",
            &["a:22", "dKz", "a:22", "dKz", "a:22", "dKz", "end"],
            "b25-tuple-variant-leaf-read-only",
        );
    // FOUR `String` leaves in one tuple variant. This cell aborts at the
    // DEFAULT opt level as well, so it guards the class on this leg rather
    // than only under the `-O0` ratchet.
    assert_clean_asan_run(
            "enum Kz4 { A(String, String, String, String), B }\n\
             impl Drop for Kz4 { fn drop(mut ref self) { println(\"dKz4\") } }\n\
             #[allow(partial_move_of_drop_enum)]\n\
             fn show(x: Option[Kz4], acc: mut ref Vec[String]) { match x { Option.Some(Kz4.A(s, t, u, v)) => { acc.push(s); acc.push(t); acc.push(u); acc.push(v) } Option.Some(Kz4.B) => {} Option.None => {} } }\n\
             fn main() { let mut acc: Vec[String] = []; for i in 0..3 { show(Option.Some(Kz4.A(f\"cccccccccccccccccccc-{i}\", f\"dddddddddddddddddddd-{i}\", f\"eeeeeeeeeeeeeeeeeeee-{i}\", f\"ffffffffffffffffffff-{i}\")), mut acc); } println(f\"len:{acc.len()}\") }\n",
            &["len:12"],
            "b25-tuple-variant-four-string-leaves",
        );
    // The accumulator is a plain struct FIELD, not a `Vec` — the row's other
    // unmeasured shape. Aborts at the default opt level too.
    assert_clean_asan_run(
            "struct R2z { s: String }\n\
             struct Accz { held: R2z }\n\
             enum Kz { A(R2z), B }\n\
             impl Drop for Kz { fn drop(mut ref self) { println(\"dKz\") } }\n\
             #[allow(partial_move_of_drop_enum)]\n\
             fn show(x: Option[Kz], acc: mut ref Accz) { match x { Option.Some(Kz.A(r)) => { acc.held = r; } Option.Some(Kz.B) => {} Option.None => {} } }\n\
             fn main() { let mut acc = Accz { held: R2z { s: f\"gggggggggggggggggggg-0\" } }; for i in 0..3 { show(Option.Some(Kz.A(R2z { s: f\"hhhhhhhhhhhhhhhhhhhh-{i}\" })), mut acc); } println(f\"h:{acc.held.s}\") }\n",
            &["h:hhhhhhhhhhhhhhhhhhhh-2"],
            "b25-tuple-variant-leaf-into-struct-field",
        );
    // CONTROL, and the cell an over-broad fix would turn into a LEAK: the
    // leaf is a wildcard, so nothing takes it and the box must stay its only
    // owner. Clean before and after.
    assert_clean_asan_run(
            "struct R2z { s: String }\n\
             enum Kz { A(R2z), B }\n\
             impl Drop for Kz { fn drop(mut ref self) { println(\"dKz\") } }\n\
             #[allow(partial_move_of_drop_enum)]\n\
             fn show(x: Option[Kz]) { match x { Option.Some(Kz.A(_)) => { println(\"a\") } Option.Some(Kz.B) => {} Option.None => {} } }\n\
             fn main() { for i in 0..3 { show(Option.Some(Kz.A(R2z { s: f\"iiiiiiiiiiiiiiiiiiii-{i}\" }))); } println(\"end\") }\n",
            &["a", "dKz", "a", "dKz", "a", "dKz", "end"],
            "b25-tuple-variant-wildcard-leaf-control",
        );
    // CONTROL: the STRUCT-shaped spelling of cell 1, which B-2026-08-31-23
    // already disarmed. Pinned beside it so the pair keeps naming the axis.
    assert_clean_asan_run(
            "struct R2z { s: String }\n\
             enum Kzs { A { r: R2z }, B }\n\
             impl Drop for Kzs { fn drop(mut ref self) { println(\"dKzs\") } }\n\
             #[allow(partial_move_of_drop_enum)]\n\
             fn show(x: Option[Kzs], acc: mut ref Vec[R2z]) { match x { Option.Some(Kzs.A { r }) => { acc.push(r) } Option.Some(Kzs.B) => {} Option.None => {} } }\n\
             fn main() { let mut acc: Vec[R2z] = []; for i in 0..3 { show(Option.Some(Kzs.A { r: R2z { s: f\"jjjjjjjjjjjjjjjjjjjj-{i}\" } }), mut acc); } println(f\"len:{acc.len()}\") }\n",
            &["len:3"],
            "b25-struct-variant-leaf-escapes-control",
        );
    // THE CEILING, from the side a broader fix breaks. A 9-word leaf is wider
    // than `Option`'s 3-word payload area, so the arm's binding is a view and
    // the box must keep freeing it. Disarming on the BINDING alone leaked all
    // three `String`s per call here — the shape of four existing fixtures,
    // which is how both ASAN ratchets caught the first version of this fix.
    assert_clean_asan_run(
            "struct R39 { s: String, t: String, u: String }\n\
             enum Kw9 { A(R39), B }\n\
             fn mkr(i: i64) -> R39 { return R39 { s: f\"ssssssssssss{i}\", t: f\"tttttttttttt{i}\", u: f\"uuuuuuuuuuuu{i}\" }; }\n\
             fn nested(x: Option[Kw9]) { match x { Option.Some(Kw9.A(r)) => { println(f\"n:{r.s.len()}\") } Option.Some(Kw9.B) => {} Option.None => {} } }\n\
             fn main() { for i in 0..3 { nested(Option.Some(Kw9.A(mkr(i)))); } println(\"end\") }\n",
            &["n:13", "n:13", "n:13", "end"],
            "b25-wide-leaf-read-only-keeps-the-box-as-owner",
        );
    // Why the ceiling is the AREA and not the constant 3: a 5-word leaf is over
    // `Option`'s ceiling and under `Result`'s, and under `Result` it owns
    // itself. Aborted before the fix.
    assert_clean_asan_run(
            "struct R5w { s: String, a: i64, b: i64 }\n\
             enum K5w { A(R5w), B }\n\
             fn nested(x: Result[K5w, i64], acc: mut ref Vec[R5w]) { match x { Result.Ok(K5w.A(r)) => { acc.push(r) } Result.Ok(K5w.B) => {} Result.Err(e) => { println(\"er\") } } }\n\
             fn main() { let mut acc: Vec[R5w] = []; for i in 0..3 { nested(Result.Ok(K5w.A(R5w { s: f\"ssssssssssss{i}\", a: 7, b: 8 })), mut acc); } println(f\"len:{acc.len()}\") }\n",
            &["len:3"],
            "b25-five-word-leaf-under-result-is-disarmed",
        );
}

/// B-2026-09-14-11 — a discarded enum returned by a CROSS-TYPE associated
/// function was owned by nobody. `Host.mk(i);` over
/// `impl Host { fn mk(n) -> Ey }` registered nothing at all, for every
/// payload kind.
///
/// `try_track_discarded_user_drop_temp`'s assoc-fn arm ended in
/// `resolved.filter(|d| !enum_layouts.contains_key(d))` — it refused EVERY
/// enum return. That came from B-2026-09-06-70, whose cell was
/// `Ev.mke();` over `impl Ev { fn mke() -> Ev }`, i.e. the assoc fn's owner
/// type IS the returned enum. That shape really does have an owner and
/// really did double its `Drop` body; what was over-general is the jump
/// from it to all enums.
///
/// Measured at `-O0` under valgrind, before -> after, CROSS-TYPE:
///
/// * `String` payload — 21 B in 3 blocks -> clean.
/// * `Vec[String]` — 288 B in 3 plus 21 indirect in 3 -> clean.
/// * `Map[i64, String]` — 216 B in 3 plus 1,605 indirect in 9 -> clean.
///   An order of magnitude past the 144 B the owning row reported, which
///   is why the row's `Array`-shaped framing understated it.
/// * `Array[String, 2]` — 144 B in 3 plus 51 indirect in 6 -> clean.
///
/// Whole fixture: 482 B in 8 blocks plus 1,256 B indirect in 12 before,
/// clean after, identical under `--interp` and `-O0`.
///
/// THE SAME-TYPE CELLS ARE THE POINT, and they are here as controls that
/// must NOT move: `Ev.mke()` (Drop-bearing payload), `Ew.mkw()` (heap
/// struct payload) and `Ed.mkd()` (a `Drop` on the enum itself) are clean
/// before AND after, one body per round. An earlier gate on
/// `type_runs_user_drop` passed this row's own cells and turned the
/// same-type `Vec`, `Map` and struct payloads into 4, 18 and 1 invalid
/// frees — the owner tracks the assoc fn's SELF TYPE, not whether a user
/// `Drop` exists anywhere. Without these three controls that gate looks
/// correct.
///
/// NOT COVERED: the same-type spelling with a BOXED `Array` payload
/// (`impl Ex { fn mk() -> Ex }` over `enum Ex { A(Array[String, 2]) }`),
/// which still leaks 144 B in 3 plus 42 indirect in 6 — the owner that
/// covers every other payload kind on that path does not cover a boxed
/// array. Its own row; excluded here deliberately rather than missed.
#[test]
fn asan_discarded_cross_type_assoc_enum_has_exactly_one_owner() {
    assert_clean_asan_run(
        r#"
struct Q { id: i64, s: String }

impl Drop for Q {
    fn drop(mut ref self) { println(f"dQ{self.id}"); }
}

struct W { s: String, k: i64 }

enum Ey { S(String), V(Vec[String]), M(Map[i64, String]), A(Array[String, 2]), N }
enum Ev { A(Q), B }
enum Ew { A(W), B }
enum Ed { A(String), B }

impl Drop for Ed {
    fn drop(mut ref self) { println("dEd"); }
}

struct Host { k: i64 }

impl Host {
    fn mkstr(n: i64) -> Ey { return Ey.S(f"xt-str-aaaaaaaaaaaaaaaa-{n}"); }
    fn mkvec(n: i64) -> Ey {
        let mut p: Vec[String] = Vec.new();
        p.push(f"xt-vec-bbbbbbbbbbbbbbbb-{n}");
        return Ey.V(p);
    }
    fn mkmap(n: i64) -> Ey {
        let mut p: Map[i64, String] = Map.new();
        p.insert(n, f"xt-map-cccccccccccccccc-{n}");
        return Ey.M(p);
    }
    fn mkarr(n: i64) -> Ey {
        let p: Array[String, 2] = [f"xt-arr-dddddddddddddddd-{n}", f"xt-arr-eeeeeeeeeeeeeeee-{n}"];
        return Ey.A(p);
    }
}

impl Ev {
    fn mke(n: i64) -> Ev { return Ev.A(Q { id: n, s: f"st-q-ffffffffffffffff-{n}" }); }
}

impl Ew {
    fn mkw(n: i64) -> Ew { return Ew.A(W { s: f"st-w-gggggggggggggggg-{n}", k: n }); }
}

impl Ed {
    fn mkd(n: i64) -> Ed { return Ed.A(f"st-d-hhhhhhhhhhhhhhhh-{n}"); }
}

fn main() {
    let mut j: i64 = 0;
    while j < 2 {
        Host.mkstr(j);
        Host.mkvec(j);
        Host.mkmap(j);
        Host.mkarr(j);

        Ev.mke(j);
        Ew.mkw(j);
        Ed.mkd(j);

        println(f"round:{j}");
        j = j + 1;
    }
    println("end");
}
"#,
        &["dQ0", "dEd", "round:0", "dQ1", "dEd", "round:1", "end"],
        "asan_discarded_cross_type_assoc_enum_has_exactly_one_owner",
    );
}

/// B-2026-09-16-10 — the MEMORY half of the generic-enum-argument crash.
/// Ten spellings, all of them SIGSEGV before the fix (exit 139, invalid
/// reads AND invalid frees in this program), all clean after: 49 allocs /
/// 49 frees, zero errors, identical output on `--interp`, the JIT and
/// `karac build`.
///
/// The crash is that both the caller and the monomorph's prologue freed the
/// same payload box: the prologue registers a `BoxedEnumDrop` for a by-value
/// generic-enum param, and `compile_generic_call` never emitted the
/// caller-side disarm that `compile_call` emits for the monomorphic twin.
///
/// SIX OF THE TEN CELLS ARE CONTROLS and each one pins a condition the
/// repair is gated on, so a future widening has to break one of them
/// visibly: a `ref` param (the prologue declines it), a generic STRUCT, a
/// bare-`T` param, an `Option` payload (the seeded pair keeps its own
/// path), a monomorphic callee (already correct, must stay byte-identical),
/// and an `Array[i64, N]` payload with no inner heap — which aborted with
/// `free(): double free` rather than segfaulting, the same defect wearing a
/// different failure.
///
/// THE FULLY-CLEAN SUBSET ONLY, and the exclusions are deliberate rather
/// than convenient. `G[Array[String, N]]` still strands its element buffers
/// (64 B in 4 blocks over two rounds) and a payload matched into an unused
/// arm binding still strands the payload itself (10 B per call); both are
/// exit-0 leaks in the same family, filed on their own rows. The codegen
/// fixture `e2e_by_value_generic_enum_arg_to_a_generic_fn_does_not_crash`
/// carries those shapes and asserts the OUTPUT, which is what the crash
/// took away.
#[test]
fn asan_by_value_generic_enum_arg_frees_its_payload_box_once() {
    assert_clean_asan_run(
        r#"
enum G1[T] { Y(T), N }
enum G2[T] { Y(T) }
struct S1[T] { v: T }

fn gplain[T](g: G1[T]) -> i64 { return 7; }
fn g2len[T](g: G2[T]) -> i64 { return 7; }
fn gints[T](g: G1[T]) -> i64 {
    match g { G1.Y(v) => { return 1; } G1.N => { return 0; } }
}
fn gref[T](g: ref G1[T]) -> i64 { return 7; }
fn slen[T](s: S1[T]) -> i64 { return 7; }
fn bare[T](x: T) -> i64 { return 7; }
fn optlen[T](o: Option[T]) -> i64 { return 7; }
fn monolen(g: G1[String]) -> i64 { return 7; }
fn mkg(n: i64) -> G1[String] { return G1.Y(f"b1610-fresh-aaaaaaaaaaaa-{n}"); }

fn main() {
    let mut t = 0;
    let mut i = 0;
    while i < 2 {
        let a: G2[String] = G2.Y(f"b1610-unit-bbbbbbbbbbbb-{i}");
        t = t + g2len(a);

        let b: G1[Vec[String]] = G1.Y([f"b1610-vec-cccccccccccc-{i}"]);
        t = t + gplain(b);

        let c: G1[Array[i64, 2]] = G1.Y([i, i + 1]);
        t = t + gints(c);

        t = t + gplain(mkg(i));

        let d: G1[String] = G1.Y(f"b1610-loop-dddddddddddd-{i}");
        t = t + gplain(d);

        let e: G1[String] = G1.Y(f"b1610-ref-eeeeeeeeeeee-{i}");
        t = t + gref(e);

        let f2: S1[String] = S1 { v: f"b1610-struct-ffffffffffff-{i}" };
        t = t + slen(f2);

        let g2: String = f"b1610-baret-gggggggggggg-{i}";
        t = t + bare(g2);

        let h: Option[Array[String, 2]] = Option.Some([f"b1610-opt-hhhhhhhhhhhh-{i}", f"b1610-opt-iiiiiiiiiiii-{i}"]);
        t = t + optlen(h);

        let j: G1[String] = G1.Y(f"b1610-mono-jjjjjjjjjjjj-{i}");
        t = t + monolen(j);

        i = i + 1;
    }
    println(f"total {t}");
}
"#,
        &["total 128"],
        "asan_by_value_generic_enum_arg_frees_its_payload_box_once",
    );
}

/// B-2026-09-16-16 — a PASSTHROUGH generic param over a boxed enum payload
/// double-freed: `fn idG[T](g: G1[T]) -> G1[T] { return g; }` over
/// `enum G1[T] { Y(T), N }` at `T = String` SIGSEGVed with no stdout at all,
/// `12 allocs / 14 frees`, against a correct `--interp`.
///
/// THE TWO OWNERS, read off the IR: `main` emits a `BoxedEnumDrop` for the
/// RESULT binding AND one for the ARGUMENT binding, over one box word. The
/// two extra frees are the box (freed by both) and the `String` interior
/// (freed by the arm's binding and again by the argument's inner drop) —
/// which is why the result's action is correctly box-only: `interior_arm_owned`
/// cleared its inner drop because the arm takes the interior.
///
/// THE MONOMORPHIC TWIN IS CLEAN, and that asymmetry is what located it:
/// `enum M1 { Y(String), N }` has an INLINE payload, so the by-value param
/// entry-copies and the result holds its own buffer. Only the erased
/// generic boxes, and only the box is passed by pointer. The `mono` and
/// `pod` cells pin both halves of that.
///
/// WHY B-2026-09-16-10'S FIX COULD NOT REACH IT, which the row states and
/// which holds up: that fix gates on the mono prologue's own escape
/// predicate, and a RETURNED param is in neither escape set — so the
/// prologue registers nothing and the caller-side disarm correctly emits
/// nothing. Both halves stand down in step. The box simply does not stay
/// with the ARGUMENT binding either: it is handed out, and the convention
/// B-2026-09-02-46 is written against is that the caller's RESULT binding
/// owns it. Nothing was standing the argument down.
///
/// `disc` IS THE GUARD ON THE FIX, not a decoration. `idG(d);` hands the
/// box to nobody, so the same disarm strands it — 24 B definitely lost,
/// measured, on a cell that was clean before the fix. The repair carries a
/// discarded-statement window for exactly that, and this cell is what fails
/// if the window is ever dropped (LSan catches it; plain valgrind under the
/// default check does not report it as an error).
///
/// `drop` pins that the payload's user `Drop` body fires exactly ONCE
/// through the hand-off, and `noarm` the same shape with no match arm to
/// take the interior — both were double frees before. `mk` (result-only
/// ownership) and `eat` (B-2026-09-16-10's non-escaping shape) are controls
/// that were correct throughout and must stay byte-identical.
///
/// Two neighbours are deliberately NOT here because they are still broken
/// and out of this fix's reach by construction: a MIXED-path callee (the
/// all-paths gate declines it, correctly — no static answer at the call is
/// right for both legs) and the `Option`/`Result` head (an inline payload,
/// so a different channel with a one-free signature). Both filed.
#[test]
fn asan_passthrough_generic_boxed_payload_arg_is_freed_once() {
    assert_clean_asan_run(
        r#"
struct D { s: String }
impl Drop for D { fn drop(mut ref self) { println(f"dD"); } }

enum G1[T] { Y(T), N }
enum M1 { Y(String), N }

fn idG[T](g: G1[T]) -> G1[T] { return g; }
fn idM(g: M1) -> M1 { return g; }
fn mk[T](x: T) -> G1[T] { return G1.Y(x); }
fn eat[T](g: G1[T]) -> i64 { return 1; }

fn main() {
    let a: G1[String] = G1.Y(f"b1616-esc-aaaaaaaaaa");
    let back = idG(a);
    match back { G1.Y(v) => { println(f"esc {v.len()}"); } G1.N => { println(f"esc 0"); } }

    let b: G1[String] = G1.Y(f"b1616-noarm-bbbbbbbb");
    let back2 = idG(b);
    println(f"noarm ok");

    let c: G1[D] = G1.Y(D { s: f"b1616-drop-cccccccccc" });
    let back3 = idG(c);
    match back3 { G1.Y(v) => { println(f"drop {v.s.len()}"); } G1.N => { println(f"drop 0"); } }

    let d: G1[String] = G1.Y(f"b1616-disc-dddddddddd");
    idG(d);
    println(f"disc ok");

    let e: M1 = M1.Y(f"b1616-mono-eeeeeeeeee");
    let back4 = idM(e);
    match back4 { M1.Y(v) => { println(f"mono {v.len()}"); } M1.N => { println(f"mono 0"); } }

    let f: G1[i64] = G1.Y(42);
    let back5 = idG(f);
    match back5 { G1.Y(v) => { println(f"pod {v}"); } G1.N => { println(f"pod 0"); } }

    let back6 = mk(f"b1616-mk-ffffffffffff");
    match back6 { G1.Y(v) => { println(f"mk {v.len()}"); } G1.N => { println(f"mk 0"); } }

    let g: G1[String] = G1.Y(f"b1616-eat-gggggggggg");
    println(f"eat {eat(g)}");
}
"#,
        &[
            "esc 20", "noarm ok", "drop 21", "dD", "disc ok", "mono 21", "pod 42", "mk 21", "eat 1",
        ],
        "asan_passthrough_generic_boxed_payload_arg_is_freed_once",
    );
}

/// B-2026-09-16-34 — a bare `shared` FIELD of a by-value enum param's
/// payload was bit-copied with no rc-INC while the callee registered a
/// scope-exit drop that rc-DECs it: one INC against two DECs, and the
/// second DEC reads and writes a refcount block the first already freed.
/// `Invalid read of size 8` + `Invalid write of size 8`, "0 bytes inside a
/// block of size 32 free'd", at BOTH opt levels, on a BALANCED 11 allocs /
/// 11 frees and a program that prints the right answer.
///
/// THE COUNT IS PER-FIELD, which is what says where the defect lives: two
/// `shared` fields give FOUR errors, one pair each. The `two` cell pins
/// that; `mixed` (a `shared` field beside a `String`) and `extra` (beside
/// an `i64`) pin that the sibling fields are not involved.
///
/// READ OFF THE IR rather than inferred. The callee's `p14e.Full`
/// entry-copy block emits a GEP and nothing else, then `p14e.merge` calls
/// `__karac_drop_Wsh(%w)` — while the caller's own
/// `__karac_drop_Wsh(%__owned_agg_tmp)` decs the same box.
/// `deep_copy_one_aggregate_field`'s bare-`shared` arm is gated on
/// `deep_copy_rc_inc_bare_shared`, and this entry-copy path left it false.
///
/// THE FLAG'S OWN DOC WARNS THAT A BUMP "LEAKED", and that warning is about
/// the STRUCT-param entry copy, which registers no separate owner. This is
/// the ENUM-param one, three lines above a `track_enum_var` that does — the
/// same pairing `uam_defensive_copy`'s user-struct arm and the for-loop
/// whole-element move both raise the flag for. `struct` is the cell that
/// keeps the two apart: a by-value struct param over the same payload type
/// was clean before and stays clean.
///
/// `tuple` rides along and was broken the same way — the enum walker
/// reaches a tuple payload's elements through the same
/// `deep_copy_one_aggregate_field`, so a `shared` element had the identical
/// missing INC. `read` is a consuming arm that reaches through the payload,
/// `named` the non-temp spelling, and `nocall` the control that never
/// passes the enum by value and was clean throughout.
///
/// ONE NEIGHBOUR IS DELIBERATELY ABSENT because it is a different defect
/// and still open: a `shared struct` used as the payload DIRECTLY
/// (`enum Wd { Full(ShIn) }`) LEAKS its refcount block — 32 B, 12 allocs /
/// 10 frees — identically before and after this fix, so it loses its DEC
/// entirely rather than running one too many. Filed separately.
#[test]
fn asan_shared_field_of_an_enum_param_payload_keeps_its_refcount() {
    assert_clean_asan_run(
        r#"
shared struct ShIn { s: String }
struct One { i: ShIn }
struct Two { a: ShIn, b: ShIn }
struct Mixed { i: ShIn, t: String }
struct Extra { i: ShIn, n: i64 }

enum W1 { Full(One), Empty }
enum W2 { Full(Two), Empty }
enum Wm { Full(Mixed), Empty }
enum We { Full(Extra), Empty }
enum Wt { Full((ShIn, i64)), Empty }

fn e1(w: W1) -> i64 { return 7; }
fn e2(w: W2) -> i64 { return 7; }
fn em(w: Wm) -> i64 { return 7; }
fn ee(w: We) -> i64 { return 7; }
fn et(w: Wt) -> i64 { return 7; }
fn eread(w: W1) -> i64 {
    match w { W1.Full(q) => { let i = q.i; return i.s.len(); } W1.Empty => { return 0; } }
}
fn estruct(o: One) -> i64 { return 7; }

fn main() {
    println(f"one {e1(W1.Full(One { i: ShIn { s: f"b1634-one-aaaaaaaaaa" } }))}");

    let x = ShIn { s: f"b1634-two-bbbbbbbbbb" };
    println(f"two {e2(W2.Full(Two { a: x, b: x }))}");

    println(f"mixed {em(Wm.Full(Mixed { i: ShIn { s: f"b1634-m1-cccccccccc" }, t: f"b1634-m2-dddddddddd" }))}");
    println(f"extra {ee(We.Full(Extra { i: ShIn { s: f"b1634-ex-eeeeeeeeee" }, n: 3 }))}");
    println(f"tuple {et(Wt.Full((ShIn { s: f"b1634-tu-ffffffffff" }, 3)))}");

    let w = W1.Full(One { i: ShIn { s: f"b1634-named-hhhhhhhh" } });
    println(f"named {e1(w)}");

    println(f"read {eread(W1.Full(One { i: ShIn { s: f"b1634-read-iiiiiiiiii" } }))}");
    println(f"struct {estruct(One { i: ShIn { s: f"b1634-st-jjjjjjjjjj" } })}");

    let k = W1.Full(One { i: ShIn { s: f"b1634-nocall-kkkkkkkk" } });
    println(f"nocall ok");
}
"#,
        &[
            "one 7",
            "two 7",
            "mixed 7",
            "extra 7",
            "tuple 7",
            "named 7",
            "read 21",
            "struct 7",
            "nocall ok",
        ],
        "asan_shared_field_of_an_enum_param_payload_keeps_its_refcount",
    );
}

/// B-2026-09-12-10 — `EnumDropKind::BoxedTuple`, closing this row's
/// `(Array[String, 2], i64)` enum-payload cell, measured across every
/// POSITION the enum can occupy rather than only the one the row reports.
///
/// THE ROW ALREADY REDUCED THIS ONE, and the reduction is what made it
/// small: B-2026-09-13-23's layout guard stands `NestedTuple` down for this
/// payload because the tuple is BOXED (two payload words against seven), so
/// handing the word region to a seven-word walker freed a `String`'s length
/// word — the invalid free `506a91d` shipped and `49e75a8` reverted. The
/// guard's `None` restored the LEAK, which its own comment calls "the
/// correct floor to land on". The walk was never wrong; it was aimed at the
/// wrong bytes. `BoxedTuple` derefs the box word first and runs the SAME
/// interior walker, exactly as `BoxedArray` does one pass up.
///
/// NEITHER THE WIDTH NOR THE WALKER MOVES, which is the whole reason this is
/// safe where the obvious repair is not. The row measured correcting
/// `payload_word_count_for_type_expr` instead and found it regresses two
/// currently-clean cells to 34 B leaks, because the conservative 1 is
/// load-bearing — it is what routes a BARE array payload to the pack side's
/// boxing where `BoxedArray` frees it. Both of those cells are here (`bare`,
/// `bare2`) and are clean before and after.
///
/// THE POSITION MATRIX is the point of this fixture. The guard's history is
/// two reverts, both of which traded a leak for corruption, so the cells are
/// derived from where a boxed enum payload can be REACHED rather than from
/// the row's symptom: a by-value param, a return, a `Vec` element, a
/// whole-payload move out of an arm, a multi-field variant, `N = 3`, a
/// `Vec`-typed element, a `shared` element, and the unit variant that must
/// touch nothing. `vectup` is the positive control the guard must never fire
/// for — its payload really is inline, so it keeps `NestedTuple`.
///
/// MEASURED at `KARAC_OPT_LEVEL=0`, valgrind, the whole program: 66 allocs
/// against 33 frees before, 552 B directly and 431 B indirectly lost —
/// exactly half the frees missing — and 66 / 66 with nothing lost after, on
/// all four opt/auto-par surfaces and byte-identical to `--interp`.
///
/// `dropelem` PRINTS NO `dR`, deliberately pinned that way. A user `Drop`
/// BODY on an array element inside a tuple payload runs on no backend, and
/// it AGREES between `--interp` and compiled code, so it is not a divergence
/// and not this row's — it is the B-2026-09-12-6 / B-2026-09-15-17 family.
/// Pinned so that whoever fixes the body channel sees this cell change here
/// rather than discovering it downstream; the MEMORY half is what this
/// fixture asserts and it is clean.
///
/// LIKE ITS SIBLING, THIS CLASS IS VISIBLE ONLY AT `-O0`. At the default opt
/// level LLVM deletes the allocations, so a green default `--features llvm`
/// run proves nothing here — `scripts/asan-o0-leg.sh` is the gate.
#[test]
fn asan_boxed_tuple_enum_payload_is_freed_in_every_position() {
    assert_clean_asan_run(
        r#"
shared struct S { s: String }
struct R { s: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR") } }

enum M { P((Array[String, 2], i64)), Q }
enum M3 { P((Array[String, 3], i64)), Q }
enum Mf { P((Array[String, 2], i64), i64), Q }
enum Mv { P((Array[Vec[String], 2], i64)), Q }
enum Ms { P((Array[S, 2], i64)), Q }
enum Mr { P((Array[R, 2], i64)), Q }
enum Bare { A(Array[String, 2]), B }
enum Bare2 { A(Array[String, 2], i64), B }
enum Tv { P((Vec[String], i64)), Q }

fn pay(i: i64) -> String { return f"tttttttttttttttt{i}" }
fn eat(m: M) -> i64 { return 1; }
fn mk(i: i64) -> M { return M.P(([pay(i), pay(i + 1i64)], i)); }

fn main() {
    println(f"byval {eat(M.P(([pay(1), pay(2)], 7)))}");

    let g = mk(3);
    match g { M.P(t) => { println(f"ret {t.1}"); } M.Q => { println(f"q"); } }

    let mut v: Vec[M] = [];
    v.push(M.P(([pay(4), pay(5)], 8)));
    v.push(M.P(([pay(6), pay(7)], 9)));
    println(f"vec {v.len()}");

    let h = M.P(([pay(10), pay(11)], 12));
    match h { M.P(t) => { let u = t; println(f"move {u.1}"); } M.Q => { println(f"q"); } }

    let a3 = M3.P(([pay(13), pay(14), pay(15)], 16));
    match a3 { M3.P(t) => { println(f"n3 {t.1}"); } M3.Q => { println(f"q"); } }

    let mf = Mf.P(([pay(17), pay(18)], 19), 20);
    println(f"mf ok");

    let mv = Mv.P(([[pay(21)], [pay(22)]], 23));
    println(f"vecelem ok");

    let x = S { s: f"shared-tttttttttt" };
    let ms = Ms.P(([x, x], 24));
    println(f"shared ok");

    let mr = Mr.P(([R { s: pay(25) }, R { s: pay(26) }], 27));
    println(f"dropelem ok");

    let b1 = Bare.A([pay(28), pay(29)]);
    println(f"bare ok");
    let b2 = Bare2.A([pay(30), pay(31)], 32);
    println(f"bare2 ok");

    let tv = Tv.P(([pay(33), pay(34)], 35));
    match tv { Tv.P(t) => { println(f"vectup {t.1}"); } Tv.Q => { println(f"q"); } }

    let e = M.Q;
    match e { M.P(t) => { println(f"p {t.1}"); } M.Q => { println(f"unit ok"); } }
}
"#,
        &[
            "byval 1",
            "ret 3",
            "vec 2",
            "move 12",
            "n3 16",
            "mf ok",
            "vecelem ok",
            "shared ok",
            "dropelem ok",
            "bare ok",
            "bare2 ok",
            "vectup 35",
            "unit ok",
        ],
        "asan_boxed_tuple_enum_payload_is_freed_in_every_position",
    );
}

/// B-2026-09-12-19 — an UN-ANNOTATED `let` bound from a call returning a
/// CONCRETE user enum registered no owner at all: 192 B in 4 blocks (the
/// boxed `[2 x {ptr,len,cap}]` payloads) plus 136 B indirect in 8 (their
/// `String`s), over four rounds.
///
/// NOT AN OWNERSHIP QUESTION — a missing TYPE. The let-site boxed-enum
/// registration resolves its payload type from the annotation, else from
/// `enum_inst_type_exprs`. That table carries a generic INSTANTIATION; a
/// call returning a concrete user enum records none, so an un-annotated
/// `let` resolved to `None` and the registration never ran. Concrete enums
/// have been in scope for it since B-2026-09-12-12 — a declared
/// `Array[T, N]` payload takes `payload_word_count_for_type_expr`'s
/// conservative `_ => 1` tail and boxes exactly like an erased `T` — so
/// only the type was missing. The fallback reads the callee's declared
/// return type, through the same two-map lookup
/// (`fn_return_type_exprs`, then `generic_fns`) the sibling sites use.
///
/// THE PAIR IS THE POINT: `annotated` and `bare` are the identical value
/// one token apart, and only the second leaked. If this regresses they
/// diverge again, which is a sharper signal than either alone.
///
/// CONTROLS, all clean before and after, each ruling out a wider reading:
/// `tuple` and `strpay` are the same concrete enum returned the same way
/// with non-array payloads (so the defect is array-shaped, not
/// return-shaped); `generic` is the generic sibling (already clean, and
/// what made the concrete case easy to miss); `local` builds the payload at
/// the `let` (clean by a different route, which is why the leak needed the
/// call-return hop).
///
/// NOT COVERED HERE, because it is a different SITE: the bare DISCARD
/// `mk(i);` of the same concrete enum, which leaked the same 192 + 136 and
/// was filed as B-2026-09-14-9. It is fixed and has its own fixture,
/// `asan_discarded_concrete_user_enum_temp_frees_its_payload` below.
#[test]
fn asan_unannotated_let_from_a_concrete_enum_return_owns_its_box() {
    assert_clean_asan_run(
        r#"
enum E { A(Array[String, 2]), B }
enum E2 { A((String, String, String)), B }
enum E3 { A(String), B }
enum G[T] { Y(T), N }

fn mk(i: i64) -> E { return E.A([f"aa-{i}-xxxxxxxxxxxx", f"bb-{i}-yyyyyyyyyyyy"]); }
fn mk2(i: i64) -> E2 { return E2.A((f"cc-{i}-xxxxxxxxxxxx", f"dd-{i}-yyyyyyyyyyyy", f"ee-{i}-zzzzzzzzzzzz")); }
fn mk3(i: i64) -> E3 { return E3.A(f"ff-{i}-xxxxxxxxxxxx"); }
fn mkg(i: i64) -> G[Array[String, 2]] { return G.Y([f"gg-{i}-xxxxxxxxxxxx", f"hh-{i}-yyyyyyyyyyyy"]); }

fn main() {
    let mut i: i64 = 0;
    while i < 4 {
        let bare = mk(i);
        match bare { E.A(x) => { println(f"bare:{x[0]}"); } E.B => {} }

        let annotated: E = mk(i);
        match annotated { E.A(x) => { println(f"annotated:{x[0]}"); } E.B => {} }

        let tuple = mk2(i);
        match tuple { E2.A(t) => { println(f"tuple:{t.0}"); } E2.B => {} }

        let strpay = mk3(i);
        match strpay { E3.A(s) => { println(f"strpay:{s}"); } E3.B => {} }

        let generic = mkg(i);
        match generic { G.Y(x) => { println(f"generic:{x[0]}"); } G.N => {} }

        let local: E = E.A([f"ii-{i}-xxxxxxxxxxxx", f"jj-{i}-yyyyyyyyyyyy"]);
        match local { E.A(x) => { println(f"local:{x[0]}"); } E.B => {} }

        i = i + 1;
    }
    println("end");
}
"#,
        &[
            "bare:aa-0-xxxxxxxxxxxx",
            "annotated:aa-0-xxxxxxxxxxxx",
            "tuple:cc-0-xxxxxxxxxxxx",
            "strpay:ff-0-xxxxxxxxxxxx",
            "generic:gg-0-xxxxxxxxxxxx",
            "local:ii-0-xxxxxxxxxxxx",
            "bare:aa-1-xxxxxxxxxxxx",
            "annotated:aa-1-xxxxxxxxxxxx",
            "tuple:cc-1-xxxxxxxxxxxx",
            "strpay:ff-1-xxxxxxxxxxxx",
            "generic:gg-1-xxxxxxxxxxxx",
            "local:ii-1-xxxxxxxxxxxx",
            "bare:aa-2-xxxxxxxxxxxx",
            "annotated:aa-2-xxxxxxxxxxxx",
            "tuple:cc-2-xxxxxxxxxxxx",
            "strpay:ff-2-xxxxxxxxxxxx",
            "generic:gg-2-xxxxxxxxxxxx",
            "local:ii-2-xxxxxxxxxxxx",
            "bare:aa-3-xxxxxxxxxxxx",
            "annotated:aa-3-xxxxxxxxxxxx",
            "tuple:cc-3-xxxxxxxxxxxx",
            "strpay:ff-3-xxxxxxxxxxxx",
            "generic:gg-3-xxxxxxxxxxxx",
            "local:ii-3-xxxxxxxxxxxx",
            "end",
        ],
        "asan_unannotated_let_from_a_concrete_enum_return_owns_its_box",
    );
}

/// B-2026-09-14-9 — a DISCARDED concrete user-enum temp owned nothing at
/// all. `mk(i);` over `fn mk(..) -> E` with
/// `enum E { A(Array[String, 2]), B }` leaked 192 B in 4 blocks (the boxed
/// `[2 x {ptr,len,cap}]` payloads) plus 136 B indirect in 8 (their
/// `String`s), while `let v = mk(i);` — the same call one binding away —
/// was clean since 57cc2e8.
///
/// THE DEFECT WAS A `return`, NOT A MISSING SHAPE ARM, which is what the
/// row's own "WHERE TO LOOK" got wrong and what the controls here pin
/// down. `try_track_discarded_user_drop_temp` resolves the type correctly
/// and reaches its bodies decision; for an enum whose payload declares no
/// `Drop` anywhere, both body-walker lookups answer `None` — correctly,
/// there is no body to run — and the arm returned on that answer, above
/// the MEMORY registrations that are this temp's only owner. That is
/// exactly the mistake B-2026-08-29-32 fixed one branch over for STRUCTS,
/// and the repair is the same: answer "no bodies", set `memory_only`, and
/// fall through (keeping that arm's alias guard, so a branch tail handing
/// back someone else's temp is still not claimed twice).
///
/// NOT ARRAY-SHAPED, which is how the `return` was identified rather than
/// a missing payload-shape arm: the `String` and 3-tuple payloads of the
/// same enum discarded the same way leaked 68 B / 4 and 204 B / 12
/// respectively, and a `Vec[String]` payload leaked too. All four are
/// cells here; a shape-keyed fix would have left three of them leaking.
///
/// THE SECOND HALF is `array_interior_ok`, and it only became visible once
/// the `return` was gone: with the fall-through alone, the array cell's
/// BOX was freed and its eight `String`s were not (136 B in 8 at `-O0`).
/// This site passed `false` because B-2026-09-12-18 measured the interior
/// walk double-freeing an interior a moved-from LOCAL still owned;
/// B-2026-09-13-15 removed that hazard at the root by standing every array
/// source down at the constructor LOWERING, so the box is the interior's
/// sole owner and this site now passes `true` like the `let` and
/// by-value-param sites. Flipping it ALONE, before the fall-through
/// landed, changed nothing at all — the row records that measurement, and
/// it is why the two halves belong in one fixture.
///
/// CONTROLS THAT MUST NOT DOUBLE-FIRE, each a neighbouring channel this
/// registration could have collided with: `mkd` is an enum with its OWN
/// `Drop` (one `dD` per round, never two — it takes the wrapper path and
/// must not also take the fall-through), `mkp` is an enum whose PAYLOAD
/// carries a `Drop` (one `dR:` per round — it reaches a real body walker,
/// so the `None` leg is never taken), and `kept` is the `let`-bound
/// spelling that already had an owner. The `if`/`match` cells are bare
/// discards NESTED in branch arms, which route through the same arm.
///
/// STILL LEAKING AND DELIBERATELY ABSENT, measured identical on the pre-
/// and post-fix compilers and filed as B-2026-09-14-11 rather than folded
/// in: a BOXED payload in the spellings whose TYPE cannot be resolved,
/// because `untyped_let_boxed_enum_te` answers only for a direct
/// free-function `Call` (plus the `Vec.pop` family). So
/// `let _ = if c { mk(i) } else { mk(i) };`, `h.makeb(i);` and
/// `H.assoc(i);` each still lose 144 B in 3 blocks plus 102 B indirect in
/// 6 over three rounds — while their INLINE-payload siblings
/// (`h.make(i);`, `let _ = if c { mk3(i) } else { mk3(i) };`) are cells
/// here and went clean, which is what localizes the remainder to the
/// boxed registration's type lookup rather than to this arm. A discarded
/// SHARED enum is out of scope by design (it returns early onto the RC
/// path).
#[test]
fn asan_discarded_concrete_user_enum_temp_frees_its_payload() {
    assert_clean_asan_run(
        r#"
enum E { A(Array[String, 2]), B }
enum E2 { A((String, String, String)), B }
enum E3 { A(String), B }
enum Ev { A(Vec[String]), B }
enum G[T] { Y(T), N }

struct R { name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR:{self.name}"); } }
enum P { A(R), B }

enum D { A(String), B }
impl Drop for D { fn drop(mut ref self) { println("dD"); } }

fn mk(i: i64) -> E { return E.A([f"aa-{i}-xxxxxxxxxxxx", f"bb-{i}-yyyyyyyyyyyy"]); }
fn mk2(i: i64) -> E2 { return E2.A((f"cc-{i}-xxxxxxxxxxxx", f"dd-{i}-yyyyyyyyyyyy", f"ee-{i}-zzzzzzzzzzzz")); }
fn mk3(i: i64) -> E3 { return E3.A(f"ff-{i}-xxxxxxxxxxxx"); }
fn mkv(i: i64) -> Ev { let mut v: Vec[String] = Vec.new(); v.push(f"vv-{i}-xxxxxxxxxxxx"); return Ev.A(v); }
fn mkg(i: i64) -> G[Array[String, 2]] { return G.Y([f"gg-{i}-xxxxxxxxxxxx", f"hh-{i}-yyyyyyyyyyyy"]); }
fn mkp(i: i64) -> P { return P.A(R { name: f"pp-{i}" }); }
fn mkd(i: i64) -> D { return D.A(f"dd-{i}-xxxxxxxxxxxx"); }

fn main() {
    let mut i: i64 = 0;
    while i < 3 {
        mk(i);
        let _ = mk(i);
        mk2(i);
        mk3(i);
        mkv(i);
        mkg(i);
        if i > 0 { mk(i); } else { mk(i); }
        match i { 0 => { mk3(i); } _ => { mk3(i); } }
        println(f"round:{i}");
        mkp(i);
        mkd(i);
        let kept = mk(i);
        match kept { E.A(x) => { println(f"kept:{x[0]}"); } E.B => {} }
        i = i + 1;
    }
    println("end");
}
"#,
        &[
            "round:0",
            "dR:pp-0",
            "dD",
            "kept:aa-0-xxxxxxxxxxxx",
            "round:1",
            "dR:pp-1",
            "dD",
            "kept:aa-1-xxxxxxxxxxxx",
            "round:2",
            "dR:pp-2",
            "dD",
            "kept:aa-2-xxxxxxxxxxxx",
            "end",
        ],
        "asan_discarded_concrete_user_enum_temp_frees_its_payload",
    );
}

/// B-2026-09-13-10 — the STRUCT-shaped spelling of a nested boxed-payload
/// destructure, which leaked one block per leaf field because its disarm
/// had no WIDTH CEILING.
///
/// `Option.Some(Kws.A { r })` over `enum Kws { A { r: R3 }, B }` routes
/// through `suppress_boxed_payload_struct_destructure_at`'s
/// `BoxedPayloadShape::EnumVariant` arm, which called the UNLIMITED
/// `suppress_destructured_enum_payload_cleanup_at` — disarming every
/// position the pattern BINDS. For a leaf at or under the envelope's
/// payload area that is right (the leaf is materialised as an owning
/// copy); ABOVE it the leaf is a view into the box and owns nothing, so
/// the disarm handed its fields to nobody. Measured on an unmodified tree:
/// `definitely lost: 27 bytes in 3 blocks` at `-O0`, one per `R3` field,
/// with correct output on all four surfaces.
///
/// The TUPLE-shaped twin (`Kws2.A(r)`) was already clean, having been given
/// the ceiling by B-2026-09-12-25 leg 2 — which is what identifies the
/// PATTERN SHAPE as the axis rather than the width or the nesting.
///
/// FOUR CELLS, chosen so that neither direction of the fix can pass alone:
///
/// - `nested` — the row's own repro, a READ-ONLY arm over a 9-word leaf.
///   Leaked before, clean after: the defect itself.
/// - `tup` — the tuple-shaped twin, clean before AND after. It pins the
///   ceiling the struct arm is being given, so a fix that removed it from
///   both would fail here.
/// - `moved` — the same struct-shaped pattern whose arm MOVES the leaf
///   (`acc.push(r)`). A move mints a second owner whatever the leaf's
///   width, so B-2026-09-13-9's exemption must lift the ceiling for it; a
///   bare ceiling with no exemption turns this cell into a DOUBLE FREE. It
///   is the cell that makes the fix's second half load-bearing.
/// - `narrow` — a leaf UNDER the area (`R1 { s: String }`, 3 words), read
///   only. At or below the ceiling the disarm still fires, so this pins
///   that the ceiling did not simply disable the arm.
#[test]
fn asan_struct_shaped_boxed_enum_payload_leaf_respects_the_width_ceiling() {
    assert_clean_asan_run(
        r#"
struct R3 { s: String, t: String, u: String }
struct R1 { s: String }
enum Kws { A { r: R3 }, B }
enum Kws2 { A(R3), B }
enum Kn { A { r: R1 }, B }

fn mk3(i: i64) -> R3 { return R3 { s: f"ssssssss{i}", t: f"tttttttt{i}", u: f"uuuuuuuu{i}" }; }

fn nested(x: Option[Kws]) {
    match x { Option.Some(Kws.A { r }) => { println(f"n:{r.s}"); } Option.Some(Kws.B) => {} Option.None => {} }
}
fn tup(x: Option[Kws2]) {
    match x { Option.Some(Kws2.A(r)) => { println(f"t:{r.s}"); } Option.Some(Kws2.B) => {} Option.None => {} }
}
fn moved(x: Option[Kws], acc: mut ref Vec[R3]) {
    match x { Option.Some(Kws.A { r }) => { acc.push(r); } Option.Some(Kws.B) => {} Option.None => {} }
}
fn narrow(x: Option[Kn]) {
    match x { Option.Some(Kn.A { r }) => { println(f"w:{r.s}"); } Option.Some(Kn.B) => {} Option.None => {} }
}

fn main() {
    let mut acc: Vec[R3] = [];
    let mut i = 0;
    while i < 3 {
        nested(Option.Some(Kws.A { r: mk3(i) }));
        tup(Option.Some(Kws2.A(mk3(i))));
        moved(Option.Some(Kws.A { r: mk3(i) }), mut acc);
        narrow(Option.Some(Kn.A { r: R1 { s: f"nnnnnnnn{i}" } }));
        i = i + 1;
    }
    println(f"len:{acc.len()}");
    println("end");
}
"#,
        &[
            "n:ssssssss0",
            "t:ssssssss0",
            "w:nnnnnnnn0",
            "n:ssssssss1",
            "t:ssssssss1",
            "w:nnnnnnnn1",
            "n:ssssssss2",
            "t:ssssssss2",
            "w:nnnnnnnn2",
            "len:3",
            "end",
        ],
        "asan_struct_shaped_boxed_enum_payload_leaf_respects_the_width_ceiling",
    );
}

#[test]
fn asan_an_enum_tuple_payload_sees_a_struct_element_that_owns_heap() {
    // B-2026-09-12-10 — two of the row's three cells, and one it did not
    // list. An enum's TUPLE payload got no drop at all when an element was
    // a user STRUCT, because the admit gate's heap question was
    // ORDER-DEPENDENT: `type_expr_has_drop_heap` (and
    // `struct_elem_owns_shared_field`) tested `struct_types` — the LLVM
    // type map — before reading `struct_field_type_exprs`, and user structs
    // enter that map in a LATER declaration pass than the one classifying
    // an enum payload. So the predicate answered "owns no heap" for every
    // user-struct element and the payload classified `None`.
    //
    // WHAT IDENTIFIES THE TABLE rather than the predicate is cell 3: the
    // identical tuple is clean as a plain local and as a struct field,
    // positions classified late enough for the map to hold the name. Both
    // guards are dropped; the authoritative field-type-expr table answers
    // the same question and is populated by then.
    //
    // THIS FIXTURE ONLY BITES AT `-O0`, so a green default `--features
    // llvm` run is NOT coverage for it: measured, the unfixed tree is clean
    // at the default opt level on every cell below, because LLVM deletes an
    // allocation nothing observes. Adding a read of the payload does not
    // rescue it either — tried, and `-O2` still eliminates the whole
    // round. `scripts/asan-o0-leg.sh` is what actually holds this, where
    // the unfixed tree reports `72 byte(s) leaked in 3 allocation(s)`.
    //
    // MEASURED, `KARAC_OPT_LEVEL=0`, valgrind, three rounds, value never
    // read:
    //
    //     (Rec2, i64)  enum payload      72 B / 3  ->  clean
    //     (Wrap, i64)  shared-owning     48 B / 3  ->  clean
    //     (Rec2, i64)  plain local       clean     ->  clean   (control)
    //     (String, i64) enum payload     clean     ->  clean   (control)
    //
    // STILL OPEN on the row, deliberately untouched here: `(bool, String)`
    // is declined by the word-alignment gate this fix's arm depends on
    // (widening it would free at the wrong offsets), and
    // `(Option[String], i64)` is ADMITTED by the gate and still leaks —
    // 40 B definite + 24 B indirect a round, the signature of a BOXED
    // payload whose box nobody frees, which is the boxed-payload family and
    // not this one.
    // 1 -- a plain USER STRUCT element. 72 B in 3 blocks before, at -O0.
    //      `Rec2` deliberately has NO `impl Drop`: the same cell WITH one
    //      still runs its body zero times (B-2026-09-12-6, a different
    //      defect on the same shape), so pinning the body count here would
    //      tie this fixture to that row. Memory only.
    assert_clean_asan_run(
            "struct Rec2 { s: String }\n\
             enum M { P((Rec2, i64)), Q }\n\
             fn main() {\n\
             \x20\x20\x20\x20let n = env.args().len() as i64;\n\
             \x20\x20\x20\x20let mut i: i64 = 0i64;\n\
             \x20\x20\x20\x20while i < 3i64 {\n\
             \x20\x20\x20\x20\x20\x20\x20\x20let g: M = P((Rec2 { s: f\"row-aaaaaaaaaaaaaaaa-{i}-{n}\" }, 1i64));\n\
             \x20\x20\x20\x20\x20\x20\x20\x20println(\"t\");\n\
             \x20\x20\x20\x20\x20\x20\x20\x20i = i + 1i64;\n\
             \x20\x20\x20\x20}\n\
             \x20\x20\x20\x20println(\"end\");\n\
             }",
            &["t", "t", "t", "end"],
            "enum-tuple-payload-struct-element",
        );

    // 2 -- a struct element owning a `shared` field. 48 B in 3 blocks
    //      before. PREDICTED from cell 1's root rather than found:
    //      B-2026-09-06-72 established this element shape for tuples, and
    //      `struct_elem_owns_shared_field` carried the same
    //      `struct_types` guard, so it was broken in the enum-payload
    //      position alone.
    assert_clean_asan_run(
        "shared struct Inner { v: i64 }\n\
             struct Wrap { i: Inner }\n\
             enum M { P((Wrap, i64)), Q }\n\
             fn main() {\n\
             \x20\x20\x20\x20let n = env.args().len() as i64;\n\
             \x20\x20\x20\x20let mut i: i64 = 0i64;\n\
             \x20\x20\x20\x20while i < 3i64 {\n\
             \x20\x20\x20\x20\x20\x20\x20\x20let g: M = P((Wrap { i: Inner { v: n } }, 1i64));\n\
             \x20\x20\x20\x20\x20\x20\x20\x20println(\"t\");\n\
             \x20\x20\x20\x20\x20\x20\x20\x20i = i + 1i64;\n\
             \x20\x20\x20\x20}\n\
             \x20\x20\x20\x20println(\"end\");\n\
             }",
        &["t", "t", "t", "end"],
        "enum-tuple-payload-shared-owning-element",
    );

    // 3 -- THE POSITION CONTROL, and the cell that identified the table
    //      rather than the predicate: the SAME tuple as a plain local was
    //      clean before this fix and stays clean, because the let-site is
    //      classified late enough for `struct_types` to hold `Rec2`.
    assert_clean_asan_run(
            "struct Rec2 { s: String }\n\
             fn main() {\n\
             \x20\x20\x20\x20let n = env.args().len() as i64;\n\
             \x20\x20\x20\x20let mut i: i64 = 0i64;\n\
             \x20\x20\x20\x20while i < 3i64 {\n\
             \x20\x20\x20\x20\x20\x20\x20\x20let p: (Rec2, i64) = (Rec2 { s: f\"row-aaaaaaaaaaaaaaaa-{i}-{n}\" }, 1i64);\n\
             \x20\x20\x20\x20\x20\x20\x20\x20println(\"t\");\n\
             \x20\x20\x20\x20\x20\x20\x20\x20i = i + 1i64;\n\
             \x20\x20\x20\x20}\n\
             \x20\x20\x20\x20println(\"end\");\n\
             }",
            &["t", "t", "t", "end"],
            "plain-local-tuple-struct-element-control",
        );

    // 4 -- the shape B-2026-09-12-8 already covered, kept as a
    //      no-regression control on the arm this fix widens.
    assert_clean_asan_run(
            "enum M { P((String, i64)), Q }\n\
             fn main() {\n\
             \x20\x20\x20\x20let n = env.args().len() as i64;\n\
             \x20\x20\x20\x20let mut i: i64 = 0i64;\n\
             \x20\x20\x20\x20while i < 3i64 {\n\
             \x20\x20\x20\x20\x20\x20\x20\x20let g: M = P((f\"row-aaaaaaaaaaaaaaaa-{i}-{n}\", 1i64));\n\
             \x20\x20\x20\x20\x20\x20\x20\x20println(\"t\");\n\
             \x20\x20\x20\x20\x20\x20\x20\x20i = i + 1i64;\n\
             \x20\x20\x20\x20}\n\
             \x20\x20\x20\x20println(\"end\");\n\
             }",
            &["t", "t", "t", "end"],
            "enum-tuple-payload-string-element-control",
        );

    // 5 -- A SUB-WORD FIRST FIELD, which is the cell that shows this fix
    //      does not free at wrong offsets. `type_expr_word_aligned` also
    //      gated on `struct_types`, so for a user-struct element it never
    //      consulted `struct_payload_word_aligned` and fell through to
    //      "aligned" — meaning the alignment precondition the arm relies on
    //      was never actually CHECKED for the elements this fix newly
    //      admits. It holds anyway, because a struct pads its fields to
    //      word granularity (only a `bool` as a DIRECT TUPLE element packs
    //      sub-word, which is why the row's `(bool, String)` cell reports
    //      `word_aligned=false` and stays declined). 72 B / 3 before,
    //      clean after, valgrind 0 errors and no invalid free.
    assert_clean_asan_run(
            "struct Small { b: bool, s: String }\n\
             enum M { P((Small, i64)), Q }\n\
             fn main() {\n\
             \x20\x20\x20\x20let n = env.args().len() as i64;\n\
             \x20\x20\x20\x20let mut i: i64 = 0i64;\n\
             \x20\x20\x20\x20while i < 3i64 {\n\
             \x20\x20\x20\x20\x20\x20\x20\x20let g: M = P((Small { b: true, s: f\"row-aaaaaaaaaaaaaaaa-{i}-{n}\" }, 1i64));\n\
             \x20\x20\x20\x20\x20\x20\x20\x20println(\"t\");\n\
             \x20\x20\x20\x20\x20\x20\x20\x20i = i + 1i64;\n\
             \x20\x20\x20\x20}\n\
             \x20\x20\x20\x20println(\"end\");\n\
             }",
            &["t", "t", "t", "end"],
            "enum-tuple-payload-subword-struct-field",
        );

    // 6 -- the same question one level down: a struct CONTAINING a sub-word
    //      tuple, which is exactly what `struct_payload_word_aligned` exists
    //      to reject and what the skipped check would have caught. Also
    //      72 B / 3 before and clean after with 0 errors — the struct's own
    //      drop fn knows its real layout, so the word region is never the
    //      thing being indexed.
    assert_clean_asan_run(
            "struct Wrap2 { t: (bool, String) }\n\
             enum M { P((Wrap2, i64)), Q }\n\
             fn main() {\n\
             \x20\x20\x20\x20let n = env.args().len() as i64;\n\
             \x20\x20\x20\x20let mut i: i64 = 0i64;\n\
             \x20\x20\x20\x20while i < 3i64 {\n\
             \x20\x20\x20\x20\x20\x20\x20\x20let g: M = P((Wrap2 { t: (true, f\"row-aaaaaaaaaaaaaaaa-{i}-{n}\") }, 1i64));\n\
             \x20\x20\x20\x20\x20\x20\x20\x20println(\"t\");\n\
             \x20\x20\x20\x20\x20\x20\x20\x20i = i + 1i64;\n\
             \x20\x20\x20\x20}\n\
             \x20\x20\x20\x20println(\"end\");\n\
             }",
            &["t", "t", "t", "end"],
            "enum-tuple-payload-struct-wrapping-subword-tuple",
        );

    // 7 -- A DIRECTLY-`shared` ELEMENT, found by the sweep the row asks
    //      for rather than from its cell list. 48 B in 3 blocks before.
    //
    //      Same ordering root as cells 1 and 2, one table further: the
    //      disjunct that recognises a shared element resolves through
    //      `shared_types`, which the shared-struct declaration pass fills
    //      AFTER the enum pass — so when an enum payload is classified that
    //      map is EMPTY (measured: len 0, against 46 struct types already
    //      known). The name-only `shared_type_names` set is filled earlier
    //      and already holds the element then, so it is consulted as a
    //      second disjunct rather than replacing the typed lookup the later
    //      positions want.
    assert_clean_asan_run(
        "shared struct Shd { v: i64 }\n\
             enum M { P((Shd, i64)), Q }\n\
             fn main() {\n\
             \x20\x20\x20\x20let n = env.args().len() as i64;\n\
             \x20\x20\x20\x20let mut i: i64 = 0i64;\n\
             \x20\x20\x20\x20while i < 3i64 {\n\
             \x20\x20\x20\x20\x20\x20\x20\x20let g: M = P((Shd { v: n }, 1i64));\n\
             \x20\x20\x20\x20\x20\x20\x20\x20println(\"t\");\n\
             \x20\x20\x20\x20\x20\x20\x20\x20i = i + 1i64;\n\
             \x20\x20\x20\x20}\n\
             \x20\x20\x20\x20println(\"end\");\n\
             }",
        &["t", "t", "t", "end"],
        "enum-tuple-payload-directly-shared-element",
    );
}

/// B-2026-09-12-10 — the row's LAST cell: a tuple enum payload with a
/// SUB-WORD element gets its drop, when the overlay the drop needs actually
/// holds.
///
/// `enum M { P((bool, String)), Q }` lost 192 B over 8 rounds at `-O0`, while
/// `(String, i64)` beside it was clean. `type_expr_word_aligned` rejects any
/// tuple with a sub-8-byte element, because the `NestedTuple` drop hands the
/// payload's WORD REGION to the tuple's own drop fn and that is only sound
/// when the tuple's LLVM fields sit at the word offsets. The gate is a
/// SYNTACTIC stand-in for that, and it is wrong in one direction: a `bool`
/// followed by an 8-byte-aligned element pads to exactly those offsets.
///
/// So the gate is not widened — the row is right that widening it is the
/// wrong repair. A second disjunct asks the precondition itself, measured
/// against `TargetData`: element `i`'s LLVM offset must equal `8 *` its word
/// index, the word image being what `coerce_to_payload_words` writes (one
/// word stream per field, never a memcpy of the tuple value). It fails closed
/// on anything unmeasurable, so it can only admit a shape the syntactic gate
/// rejected.
///
/// `bis` IS THE CELL THAT MUST STAY DECLINED, and it is what makes the
/// distinction real rather than verbal: in `(bool, i32, String)` the `i32`
/// genuinely shares the `bool`'s word while the payload image gives it its
/// own, so the offsets disagree, the disjunct answers false, and the payload
/// keeps leaking. That remainder is B-2026-09-17-28 — it needs the
/// word-per-element pack layout the row describes, not this predicate.
/// It is deliberately NOT a cell here (it would redden the suite for its own
/// row); the assertion that it stays declined is the leak it still has.
///
/// THREE CELLS THE ROW DID NOT LIST are fixed by the same two lines, and they
/// are here because each was measured before and after: `(i32, String)`
/// 192 B/8 -> clean, `(bool, String, String)` 368 B/16 -> clean, and
/// `(bool, Rec)` with a user `Drop` on `Rec` 66 B/3 -> clean.
///
/// THE TWO BARE-ARRAY CELLS are the regression guard, not coverage: the row
/// measured the OTHER candidate repair (widening
/// `payload_word_count_for_type_expr`) regressing both from clean to 34 B,
/// because the under-sizing is load-bearing for `EnumDropKind::BoxedArray`.
/// This fix touches neither the width nor the walker, and they are clean
/// before and after.
///
/// `(bool, Rec)` RUNS ITS USER `Drop` BODY ON NO BACKEND, before and after,
/// `--interp` included — so it is agreed rather than divergent, and it is the
/// B-2026-09-12-6 body-channel family, not this row. Recorded here so whoever
/// repairs that channel sees this position.
///
/// OBSERVABLE ONLY AT `-O0`, which the row insists on and which makes the
/// ordinary `--features llvm` run of this fixture VACUOUS: at the default opt
/// level LLVM deletes an allocation nothing observes, and adding a read of the
/// payload does not rescue it. `scripts/asan-o0-leg.sh` is where the unfixed
/// tree reports the leak, so a green default run here is not evidence.
#[test]
fn asan_subword_element_tuple_enum_payload_is_dropped() {
    const EIGHT: [&str; 9] = ["t", "t", "t", "t", "t", "t", "t", "t", "end"];
    const ROUNDS: &str = "    let mut i: i64 = 0i64;\n\
             \x20   while i < 8i64 {\n";

    // The row's own cell.
    assert_clean_asan_run(
        &format!(
            "enum M {{ P((bool, String)), Q }}\n\
                 fn main() {{\n{ROUNDS}\
                 \x20       let g: M = P((true, f\"row-aaaaaaaaaaaaaaaa-{{i}}\"));\n\
                 \x20       println(f\"t\");\n\
                 \x20       i = i + 1i64;\n\
                 \x20   }}\n\
                 \x20   println(f\"end\");\n\
                 }}\n"
        ),
        &EIGHT,
        "b1210-bool-string",
    );

    // Sub-word element FIRST, a different primitive width.
    assert_clean_asan_run(
        &format!(
            "enum M {{ P((i32, String)), Q }}\n\
                 fn main() {{\n{ROUNDS}\
                 \x20       let g: M = P((3i32, f\"row-aaaaaaaaaaaaaaaa-{{i}}\"));\n\
                 \x20       println(f\"t\");\n\
                 \x20       i = i + 1i64;\n\
                 \x20   }}\n\
                 \x20   println(f\"end\");\n\
                 }}\n"
        ),
        &EIGHT,
        "b1210-i32-string",
    );

    // TWO heap elements after the sub-word one.
    assert_clean_asan_run(
            &format!(
                "enum M {{ P((bool, String, String)), Q }}\n\
                 fn main() {{\n{ROUNDS}\
                 \x20       let g: M = P((true, f\"row-aaaaaaaaaaaaaaaa-{{i}}\", f\"two-bbbbbbbbbbbbbbbb-{{i}}\"));\n\
                 \x20       println(f\"t\");\n\
                 \x20       i = i + 1i64;\n\
                 \x20   }}\n\
                 \x20   println(f\"end\");\n\
                 }}\n"
            ),
            &EIGHT,
            "b1210-bool-two-strings",
        );

    // A user-`Drop` STRUCT element behind the sub-word one. Its body runs on
    // no backend (B-2026-09-12-6); the memory is this row's and is clean.
    assert_clean_asan_run(
        "struct Rec { id: i64, s: String }\n\
             impl Drop for Rec { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             enum M { P((bool, Rec)), Q }\n\
             fn main() {\n\
             \x20   let mut i: i64 = 0i64;\n\
             \x20   while i < 3i64 {\n\
             \x20       let g: M = P((true, Rec { id: i, s: f\"row-aaaaaaaaaaaaaaaa-{i}\" }));\n\
             \x20       println(f\"t\");\n\
             \x20       i = i + 1i64;\n\
             \x20   }\n\
             \x20   println(f\"end\");\n\
             }\n",
        &["t", "t", "t", "end"],
        "b1210-drop-struct-element",
    );

    // ── regression guard: the two bare-array cells the OTHER candidate fix
    //    regressed from clean to 34 B. Clean before and after this one.
    for (src, label) in [
            (
                "enum E { A(Array[String, 2]), B }\n\
                 fn main() {\n\
                 \x20   let mut i: i64 = 0i64;\n\
                 \x20   while i < 8i64 {\n\
                 \x20       let a: Array[String, 2] = [f\"one-aaaaaaaaaaaaaaaa-{i}\", f\"two-bbbbbbbbbbbbbbbb-{i}\"];\n\
                 \x20       let g: E = A(a);\n\
                 \x20       println(f\"t\");\n\
                 \x20       i = i + 1i64;\n\
                 \x20   }\n\
                 \x20   println(f\"end\");\n\
                 }\n",
                "b1210-bare-array-guard",
            ),
            (
                "enum E { A(Array[String, 2], i64), B }\n\
                 fn main() {\n\
                 \x20   let mut i: i64 = 0i64;\n\
                 \x20   while i < 8i64 {\n\
                 \x20       let a: Array[String, 2] = [f\"one-aaaaaaaaaaaaaaaa-{i}\", f\"two-bbbbbbbbbbbbbbbb-{i}\"];\n\
                 \x20       let g: E = A(a, 7i64);\n\
                 \x20       println(f\"t\");\n\
                 \x20       i = i + 1i64;\n\
                 \x20   }\n\
                 \x20   println(f\"end\");\n\
                 }\n",
                "b1210-bare-array-two-field-guard",
            ),
        ] {
            assert_clean_asan_run(src, &EIGHT, label);
        }
}

#[test]
fn asan_shared_enum_nameless_aggregate_payload_box_is_freed() {
    const STRS: &str = "[f\"aaaaaaaaaaaaaaaaaaaa\", f\"bbbbbbbbbbbbbbbbbbbb\"]";

    let single = format!(
        "shared enum Sh {{ S(Array[String, 2]), N }}\n\
             fn main() {{\n\
             \x20   let a: Array[String, 2] = {STRS};\n\
             \x20   {{ let s = Sh.S(a); println(f\"one\"); }}\n\
             \x20   println(f\"done\");\n\
             }}\n"
    );
    assert_clean_asan_run(&single, &["one", "done"], "b1510-named-local");

    // No interior at all — the clearest proof that the ENVELOPE is what was
    // unowned, since there is nothing here for any interior walk to reach.
    assert_clean_asan_run(
        "shared enum Sh { S(Array[i64, 2]), N }\n\
             fn main() {\n\
             \x20   let a: Array[i64, 2] = [1, 2];\n\
             \x20   { let s = Sh.S(a); println(f\"one\"); }\n\
             \x20   println(f\"done\");\n\
             }\n",
        &["one", "done"],
        "b1510-scalar-no-interior",
    );

    // TWO handles to ONE box. Exactly one free must happen: a leak here
    // means the fix regressed, an ASAN double-free report means it was
    // moved to a per-binding registration.
    let handles = format!(
        "shared enum Sh {{ S(Array[String, 2]), N }}\n\
             fn main() {{\n\
             \x20   let a: Array[String, 2] = {STRS};\n\
             \x20   {{ let s1 = Sh.S(a); let s2 = s1; println(f\"two\"); }}\n\
             \x20   println(f\"done\");\n\
             }}\n"
    );
    assert_clean_asan_run(&handles, &["two", "done"], "b1510-two-handles-one-box");

    // TWO boxing variants: the switch needs an arm per boxing variant, not
    // just the one the program happens to construct.
    let two = format!(
        "shared enum Sh {{ A(Array[String, 2]), B(Array[i64, 3]), N }}\n\
             fn main() {{\n\
             \x20   let x: Array[String, 2] = {STRS};\n\
             \x20   {{ let s = Sh.A(x); println(f\"a\"); }}\n\
             \x20   let y: Array[i64, 3] = [1, 2, 3];\n\
             \x20   {{ let s = Sh.B(y); println(f\"b\"); }}\n\
             }}\n"
    );
    assert_clean_asan_run(&two, &["a", "b"], "b1510-two-boxing-variants");

    // A NESTED aggregate — the box holds arrays of arrays.
    assert_clean_asan_run(
        "shared enum Sh { S(Array[Array[String, 2], 2]), N }\n\
             fn main() {\n\
             \x20   let a: Array[Array[String, 2], 2] =\n\
             \x20       [[f\"aaaaaaaaaaaaaaaa\", f\"bbbbbbbbbbbbbbbb\"],\n\
             \x20        [f\"cccccccccccccccc\", f\"dddddddddddddddd\"]];\n\
             \x20   { let s = Sh.S(a); println(f\"n\"); }\n\
             \x20   println(f\"done\");\n\
             }\n",
        &["n", "done"],
        "b1510-nested-array-payload",
    );

    // The Arc path. Same layout, different release function — this is the
    // cell the first cut of the fix left leaking.
    let par = format!(
        "par enum Sh {{ S(Array[String, 2]), N }}\n\
             fn main() {{\n\
             \x20   let a: Array[String, 2] = {STRS};\n\
             \x20   {{ let s = Sh.S(a); println(f\"one\"); }}\n\
             \x20   println(f\"done\");\n\
             }}\n"
    );
    assert_clean_asan_run(&par, &["one", "done"], "b1510-par-enum-arc-path");
}

/// B-2026-09-17-34 — moving a heap-carrying `Drop` field OUT of a BOXED
/// (spilled) `Option`/`Result` payload struct.
///
/// `let x = t.r` inside a whole-payload arm makes `x` the owner of the
/// field's body AND its memory, while the envelope kept both of its own
/// walks over the same object — the `__karac_dropelems_opt_*` bodies walk
/// and the box's interior memory walk. Three owners for one `String`
/// buffer: `free(): double free detected in tcache 2`, exit 134, with NO
/// program output at all, on `build`, `build KARAC_AUTO_PAR=0` and
/// `karac run` alike, against an `--interp` that printed the due answer.
///
/// NOTHING IN THIS SUITE COVERED THE SHAPE, which is why it reached a
/// release-shaped abort: the neighbouring double-free fixtures are all
/// boxed enum payloads or nested DESTRUCTURES (B-2026-09-12-25,
/// B-2026-09-13-9, B-2026-09-09-22), never a `let x = t.<field>` projection
/// off a whole-payload arm binding. The E2E suites could not have caught it
/// either: their tolerant `if let Some(out) = run_program(..)` form returns
/// `None` on a non-zero exit, so a fixture written there would have passed
/// vacuously.
///
/// Five cells, and each one is a DIFFERENT owner pair rather than a
/// restatement — that is what the fix had to reconcile, and every one of
/// them aborted before it:
///
///   local     no call in the program at all, so both walks sit in ONE
///             function. The cell that proves this is not a caller/callee
///             ownership question.
///   temp      a fresh-temp argument: the caller owns the box through
///             `__optbox_arg_tmp{i}` and the callee's arm owns the field.
///   named     a NAMED local argument, which reaches neither of the above —
///             the box is owned by the local's own `let`-site registration.
///             It is the cell that needed the bodies channel masked as well
///             as the memory one; with only the memory mask it stopped
///             aborting and started printing a body over a freed string,
///             which is strictly worse.
///   two       TWO heap-carrying `Drop` fields with ONE moved. The cell that
///             forbids the easy fix: retracting the interior walk outright
///             stops the double free and leaks the sibling.
///   wide      a `Result` payload wide enough to spill its own 5-word inline
///             area. `Result` escaped the original report only because its
///             narrower payload stays INLINE — the trigger is the BOXING,
///             not the head, and this cell is what settles that.
///
/// Each asserts the interpreter's own output, which was correct throughout.
#[test]
fn asan_boxed_payload_field_move_out_no_double_free() {
    const DECLS: &str = "struct R { name: String, id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}/{self.name}\") } }\n";

    // The LOCAL cell — no call anywhere in the program.
    assert_clean_asan_run(
            &format!(
                "{DECLS}\
                 struct Hd {{ r: R, n: i64 }}\n\
                 fn eat() {{\n\
                 \x20   let o: Option[Hd] = Option.Some(Hd {{ r: R {{ name: f\"a\", id: 5 }}, n: 9 }});\n\
                 \x20   match o {{ Option.Some(t) => {{ let x = t.r; println(\"mid\"); }} Option.None => {{ println(\"n\"); }} }}\n\
                 }}\n\
                 fn main() {{ eat(); println(\"end\"); }}\n"
            ),
            &["dR5/a", "mid", "end"],
            "b1734-boxed-payload-field-move-local",
        );

    // The FRESH-TEMP argument cell — the row's own headline spelling.
    assert_clean_asan_run(
            &format!(
                "{DECLS}\
                 struct Hd {{ r: R, n: i64 }}\n\
                 fn eat(o: Option[Hd]) {{\n\
                 \x20   match o {{ Option.Some(t) => {{ let x = t.r; println(\"mid\"); }} Option.None => {{ println(\"n\"); }} }}\n\
                 }}\n\
                 fn main() {{ eat(Option.Some(Hd {{ r: R {{ name: f\"a\", id: 5 }}, n: 9 }})); println(\"end\"); }}\n"
            ),
            &["dR5/a", "mid", "end"],
            "b1734-boxed-payload-field-move-temp",
        );

    // The NAMED-LOCAL argument cell — the one that needs BOTH channels.
    assert_clean_asan_run(
            &format!(
                "{DECLS}\
                 struct Hd {{ r: R, n: i64 }}\n\
                 fn eat(o: Option[Hd]) {{\n\
                 \x20   match o {{ Option.Some(t) => {{ let x = t.r; println(\"mid\"); }} Option.None => {{ println(\"n\"); }} }}\n\
                 }}\n\
                 fn main() {{\n\
                 \x20   let a = Option.Some(Hd {{ r: R {{ name: f\"a\", id: 5 }}, n: 9 }});\n\
                 \x20   eat(a);\n\
                 \x20   println(\"end\");\n\
                 }}\n"
            ),
            &["dR5/a", "mid", "end"],
            "b1734-boxed-payload-field-move-named",
        );

    // TWO heap `Drop` fields, ONE moved — the sibling must still be freed
    // AND must still run its body, which is what makes the mask per-field
    // rather than a retraction.
    assert_clean_asan_run(
            &format!(
                "{DECLS}\
                 struct Hd2 {{ r: R, q: R }}\n\
                 fn eat(o: Option[Hd2]) {{\n\
                 \x20   match o {{ Option.Some(t) => {{ let x = t.r; println(\"mid\"); }} Option.None => {{ println(\"n\"); }} }}\n\
                 }}\n\
                 fn main() {{ eat(Option.Some(Hd2 {{ r: R {{ name: f\"a\", id: 5 }}, q: R {{ name: f\"b\", id: 6 }} }})); println(\"end\"); }}\n"
            ),
            &["dR5/a", "mid", "dR6/b", "end"],
            "b1734-boxed-payload-field-move-two-fields",
        );

    // The WIDE `Result` payload — spills its own 5-word inline area, so the
    // head that escaped the original report aborts too.
    assert_clean_asan_run(
            &format!(
                "{DECLS}\
                 struct Wide {{ r: R, a: i64, b: i64, c: i64, d: i64, e: i64 }}\n\
                 fn eat(o: Result[Wide, i64]) {{\n\
                 \x20   match o {{ Result.Ok(t) => {{ let x = t.r; println(\"mid\"); }} Result.Err(v) => {{ println(\"n\"); }} }}\n\
                 }}\n\
                 fn main() {{ eat(Result.Ok(Wide {{ r: R {{ name: f\"a\", id: 5 }}, a: 1, b: 2, c: 3, d: 4, e: 5 }})); println(\"end\"); }}\n"
            ),
            &["dR5/a", "mid", "end"],
            "b1734-boxed-payload-field-move-wide-result",
        );

    // The READ-ONLY control, which was correct throughout. It is here so a
    // future widening of the mask cannot silence a field that never moved:
    // every cell above asserts that a body still RUNS, and this one asserts
    // it runs for a payload the arm only reads.
    assert_clean_asan_run(
            &format!(
                "{DECLS}\
                 struct Hd {{ r: R, n: i64 }}\n\
                 fn eat(o: Option[Hd]) {{\n\
                 \x20   match o {{ Option.Some(t) => {{ println(f\"mid{{t.r.id}}\"); }} Option.None => {{ println(\"n\"); }} }}\n\
                 }}\n\
                 fn main() {{ eat(Option.Some(Hd {{ r: R {{ name: f\"a\", id: 5 }}, n: 9 }})); println(\"end\"); }}\n"
            ),
            &["mid5", "dR5/a", "end"],
            "b1734-boxed-payload-field-move-readonly-control",
        );
}

/// B-2026-09-19-9 — a `Drop`-bearing element moved out of a boxed TUPLE
/// payload ran its body twice, the second time over freed memory.
///
/// The tuple sibling of B-2026-09-17-34. That row masked a moved-out FIELD
/// of a struct payload out of the envelope's bodies walk; a tuple payload
/// reaches a different arm of the same walker, and that arm had no seat for
/// a mask. So `let x = t.0` gave the element's body to `x` and left the
/// envelope running it again over the husk, reading the `String` the first
/// body had already freed.
///
/// MEMORY IS BALANCED THROUGHOUT — 12 allocs / 12 frees before, 11 / 11
/// after — so nothing aborts and a leak-only verdict reads the cell as
/// CLEAN. The symptom is a `Drop` body printing garbage (`dR5/d` for
/// `dR5/a`) plus one valgrind `Invalid read`. That is why these cells are
/// asserted on their OUTPUT rather than on their byte counts.
///
/// The cells:
///
///   opt       the row's own spelling, `Option[(R, i64, i64, i64)]`.
///   res       the `Result` head, which the row listed as unmeasured. It
///             fails and is fixed identically; the head only sets how wide
///             the payload must be before it spills.
///   two       TWO `Drop`-bearing elements with ONE moved — the cell that
///             forbids the easy fix, since retracting the walk outright
///             would silence the sibling that must still run. Both elements
///             are HEAP-FREE on purpose: the heap-bearing spelling strands
///             the unmoved sibling's buffer, which is a SEPARATE and
///             pre-existing defect (identical 1-byte loss before and after
///             this fix), filed and since fixed as B-2026-09-19-11, whose
///             own fixture below carries the heap-bearing spelling. Letting
///             it in here would have reddened the ASAN legs for a bug this
///             fixture is not about; it stays heap-free so the two rows
///             keep failing independently.
///   readonly  an arm that reads the element and moves nothing, where the
///             walker is the SOLE owner and must keep running.
///
/// Each asserts the interpreter's own output, which was correct on every
/// cell here.
#[test]
fn asan_boxed_tuple_payload_elem_move_out_no_double_body() {
    const DECLS: &str = "struct R { name: String, id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}/{self.name}\") } }\n";

    // The row's headline cell.
    assert_clean_asan_run(
            &format!(
                "{DECLS}\
                 fn eat(o: Option[(R, i64, i64, i64)]) {{\n\
                 \x20   match o {{ Option.Some(t) => {{ let x = t.0; println(\"mid\"); }} Option.None => {{ println(\"n\"); }} }}\n\
                 }}\n\
                 fn main() {{ eat(Option.Some((R {{ name: f\"a\", id: 5 }}, 1, 2, 3))); println(\"end\"); }}\n"
            ),
            &["dR5/a", "mid", "end"],
            "b1729b-tuple-payload-elem-move-opt",
        );

    // The `Result` head, wide enough to spill its own 5-word inline area.
    assert_clean_asan_run(
            &format!(
                "{DECLS}\
                 fn eat(o: Result[(R, i64, i64, i64, i64, i64), i64]) {{\n\
                 \x20   match o {{ Result.Ok(t) => {{ let x = t.0; println(\"mid\"); }} Result.Err(e) => {{ println(\"n\"); }} }}\n\
                 }}\n\
                 fn main() {{ eat(Result.Ok((R {{ name: f\"a\", id: 5 }}, 1, 2, 3, 4, 5))); println(\"end\"); }}\n"
            ),
            &["dR5/a", "mid", "end"],
            "b1729b-tuple-payload-elem-move-res",
        );

    // TWO Drop-bearing elements, ONE moved: the sibling must still run.
    // Heap-free on both, for the reason the doc above gives.
    assert_clean_asan_run(
            "struct P { id: i64 }\n\
             impl Drop for P { fn drop(mut ref self) { println(f\"dP{self.id}\") } }\n\
             struct Q { id: i64 }\n\
             impl Drop for Q { fn drop(mut ref self) { println(f\"dQ{self.id}\") } }\n\
             fn eat(o: Option[(P, Q, i64, i64)]) {\n\
             \x20   match o { Option.Some(t) => { let x = t.0; println(\"mid\"); } Option.None => { println(\"n\"); } }\n\
             }\n\
             fn main() { eat(Option.Some((P { id: 5 }, Q { id: 6 }, 2, 3))); println(\"end\"); }\n",
            &["dP5", "mid", "dQ6", "end"],
            "b1729b-tuple-payload-elem-move-two",
        );

    // Read-only arm: the walker is the sole owner and must keep running.
    assert_clean_asan_run(
            &format!(
                "{DECLS}\
                 fn eat(o: Option[(R, i64, i64, i64)]) {{\n\
                 \x20   match o {{ Option.Some(t) => {{ println(f\"mid{{t.0.id}}\"); }} Option.None => {{ println(\"n\"); }} }}\n\
                 }}\n\
                 fn main() {{ eat(Option.Some((R {{ name: f\"a\", id: 5 }}, 1, 2, 3))); println(\"end\"); }}\n"
            ),
            &["mid5", "dR5/a", "end"],
            "b1729b-tuple-payload-elem-move-readonly-control",
        );
}

/// B-2026-09-19-11 — a tuple element moved out of a BOXED `Option`/`Result`
/// payload must not strand its UNMOVED siblings' heap.
///
/// The memory-channel half of B-2026-09-19-9's shape, and the reason it is
/// a separate row: that one is a doubled `Drop` BODY reading freed memory,
/// this one is a silent leak with correct output on every surface. A
/// program cannot see it at all — only LSan/valgrind can.
///
/// `Some(t) => { let x = t.0 }` hands element 0 to `x`, so the box's
/// interior walk must not free it again. The arm site's answer was to
/// retract that walk ENTIRELY, which is right for element 0 and wrong for
/// every other element: their heap then had no owner at all. Both error
/// directions are live here and a probe measured each — retracting nothing
/// double-frees element 0 (`free(): double free detected in tcache 2`),
/// retracting everything leaks the siblings — so the walk has to be MASKED,
/// which is what `synthesize_tuple_drop_fn_te_skipping` is for.
///
/// The cells:
///
///   two-heap   the row's own spelling: two `Drop`-bearing struct elements
///              each owning a `String`, one moved out.
///   sibling    a bare `String` sibling rather than a struct one, so the
///              mask is exercised on the `Vec`/`String` arm of the element
///              walk rather than the named-struct one.
///   asym       the sibling's string is long and the moved element's is
///              short. Before the fix the loss tracked the SIBLING's length
///              (32 B here, 1 B with the lengths swapped), which is what
///              identified the stranded block; the cell keeps that pinned.
///   both       BOTH elements moved out, so nothing survives the mask and
///              the full retraction is the correct answer. Guards the
///              `synthesize_tuple_drop_fn_te_skipping` -> `None` path,
///              where a mask that freed anything would double-free.
///   nomove     two heap-bearing elements and no move at all: the walk is
///              the sole owner and must run unmasked.
///
/// Asserted on output AND on being leak-clean, since the output was already
/// correct on every cell before the fix.
#[test]
fn asan_boxed_tuple_payload_elem_move_out_frees_siblings() {
    const DECLS: &str = "struct R { name: String, id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}/{self.name.len()}\") } }\n";

    // Two Drop-bearing, heap-owning elements; element 0 moved out.
    assert_clean_asan_run(
            &format!(
                "{DECLS}\
                 fn eat(o: Option[(R, R, i64, i64)]) {{\n\
                 \x20   match o {{ Option.Some(t) => {{ let x = t.0; println(\"mid\"); }} Option.None => {{ println(\"n\"); }} }}\n\
                 }}\n\
                 fn main() {{ eat(Option.Some((R {{ name: f\"a\", id: 5 }}, R {{ name: f\"b\", id: 6 }}, 2, 3))); println(\"end\"); }}\n"
            ),
            &["dR5/1", "mid", "dR6/1", "end"],
            "b11911-tuple-payload-sibling-heap-two",
        );

    // A bare `String` sibling: the Vec/String arm of the element walk.
    assert_clean_asan_run(
            &format!(
                "{DECLS}\
                 fn eat(o: Option[(R, String, i64, i64)]) {{\n\
                 \x20   match o {{ Option.Some(t) => {{ let x = t.0; println(\"mid\"); }} Option.None => {{ println(\"n\"); }} }}\n\
                 }}\n\
                 fn main() {{ eat(Option.Some((R {{ name: f\"a\", id: 5 }}, f\"sibling-string-here\", 2, 3))); println(\"end\"); }}\n"
            ),
            &["dR5/1", "mid", "end"],
            "b11911-tuple-payload-sibling-heap-string",
        );

    // Long sibling, short moved element: the loss tracked the sibling.
    assert_clean_asan_run(
            &format!(
                "{DECLS}\
                 fn eat(o: Option[(R, R, i64, i64)]) {{\n\
                 \x20   match o {{ Option.Some(t) => {{ let x = t.0; println(\"mid\"); }} Option.None => {{ println(\"n\"); }} }}\n\
                 }}\n\
                 fn main() {{ eat(Option.Some((R {{ name: f\"a\", id: 5 }}, R {{ name: f\"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\", id: 6 }}, 2, 3))); println(\"end\"); }}\n"
            ),
            &["dR5/1", "mid", "dR6/32", "end"],
            "b11911-tuple-payload-sibling-heap-asym",
        );

    // BOTH elements moved: nothing survives the mask, full retraction is due.
    assert_clean_asan_run(
            &format!(
                "{DECLS}\
                 fn eat(o: Option[(R, R, i64, i64)]) {{\n\
                 \x20   match o {{ Option.Some(t) => {{ let x = t.0; let y = t.1; println(\"mid\"); }} Option.None => {{ println(\"n\"); }} }}\n\
                 }}\n\
                 fn main() {{ eat(Option.Some((R {{ name: f\"a\", id: 5 }}, R {{ name: f\"b\", id: 6 }}, 2, 3))); println(\"end\"); }}\n"
            ),
            &["dR5/1", "dR6/1", "mid", "end"],
            "b11911-tuple-payload-sibling-heap-both",
        );

    // No move at all: the walk is the sole owner and must run unmasked.
    assert_clean_asan_run(
            &format!(
                "{DECLS}\
                 fn eat(o: Option[(R, R, i64, i64)]) {{\n\
                 \x20   match o {{ Option.Some(t) => {{ println(f\"mid{{t.0.id}}\"); }} Option.None => {{ println(\"n\"); }} }}\n\
                 }}\n\
                 fn main() {{ eat(Option.Some((R {{ name: f\"a\", id: 5 }}, R {{ name: f\"b\", id: 6 }}, 2, 3))); println(\"end\"); }}\n"
            ),
            &["mid5", "dR5/1", "dR6/1", "end"],
            "b11911-tuple-payload-sibling-heap-nomove",
        );
}

/// B-2026-09-19-13 — the mask an arm accumulates must not outlive the arm.
///
/// `boxed_payload_moved_fields` records which payload elements an arm has
/// moved out, and both boxed-payload suppressors accumulate-then-re-read it
/// so a second `let y = t.1` in ONE arm masks both. The map is keyed by
/// BINDING NAME and was cleared only per FUNCTION, so that union outlived
/// its arm: a later match in the same function binding the same name
/// inherited it and masked an element whose body nothing else ran.
///
/// The cells are a PAIR and only the pair is evidence. The `bleed` cell
/// puts a site that moves BOTH elements ahead of one that moves only the
/// second; `control` is that second site alone. Before the fix `bleed`
/// dropped `dP7` and `control` was already correct — so a single-site
/// fixture would have passed on the broken tree, and the contrast is what
/// identifies this as a scoping defect rather than a walker defect.
///
/// Both `Drop` types are HEAP-FREE, for the reason
/// `asan_boxed_tuple_payload_elem_move_out_no_double_body`'s `two` cell
/// gives: this fixture is about a body that does not run, and a heap
/// payload would couple it to the sibling-heap row. Memory is balanced
/// throughout, so these assert on OUTPUT rather than on byte counts.
#[test]
fn asan_boxed_payload_moved_mask_does_not_outlive_its_arm() {
    const DECLS: &str = "struct P { id: i64 }\n\
             impl Drop for P { fn drop(mut ref self) { println(f\"dP{self.id}\") } }\n\
             struct Q { id: i64 }\n\
             impl Drop for Q { fn drop(mut ref self) { println(f\"dQ{self.id}\") } }\n";

    // A site that moves BOTH elements, then a site that moves only the
    // second. The second site's element 0 body is the one that vanished.
    assert_clean_asan_run(
            &format!(
                "{DECLS}\
                 fn eat() {{\n\
                 \x20   let a: Option[(P, Q, i64, i64)] = Option.Some((P {{ id: 4 }}, Q {{ id: 5 }}, 2, 3));\n\
                 \x20   match a {{ Option.Some(t) => {{ let x = t.0; let y = t.1; println(\"mid1\"); }} Option.None => {{ println(\"n\"); }} }}\n\
                 \x20   let b: Option[(P, Q, i64, i64)] = Option.Some((P {{ id: 6 }}, Q {{ id: 7 }}, 2, 3));\n\
                 \x20   match b {{ Option.Some(t) => {{ let y = t.1; println(\"mid2\"); }} Option.None => {{ println(\"n\"); }} }}\n\
                 }}\n\
                 fn main() {{ eat(); println(\"end\"); }}\n"
            ),
            &["dP4", "dQ5", "mid1", "dQ7", "mid2", "dP6", "end"],
            "b11913-moved-mask-arm-scope-bleed",
        );

    // The same second site with no sibling ahead of it. Correct before the
    // fix and after it, which is what makes the cell above discriminating.
    assert_clean_asan_run(
            &format!(
                "{DECLS}\
                 fn eat() {{\n\
                 \x20   let b: Option[(P, Q, i64, i64)] = Option.Some((P {{ id: 6 }}, Q {{ id: 7 }}, 2, 3));\n\
                 \x20   match b {{ Option.Some(t) => {{ let y = t.1; println(\"mid2\"); }} Option.None => {{ println(\"n\"); }} }}\n\
                 }}\n\
                 fn main() {{ eat(); println(\"end\"); }}\n"
            ),
            &["dQ7", "mid2", "dP6", "end"],
            "b11913-moved-mask-arm-scope-control",
        );
}

/// B-2026-09-19-14 — an arm binding over a boxed payload must not MINT a
/// second bodies walk beside the envelope's.
///
/// `Some(t) => { let y = t.b }` is a field move out of a struct payload,
/// so it reaches the ordinary move-out disarm, which mints a masked
/// `$keep` walk on any binding that holds none. `t` holds none — it is a
/// bit-copy VIEW of the box interior — but the ENVELOPE's walk already
/// covers those bodies, and B-2026-09-17-34's suppressor masks that walk
/// per moved field at the same statement. So the mint became a second
/// owner of every field the mask left alive, and the emitted arm called
/// `__karac_dropbodies_W$keep0$s1(t)` and
/// `__karac_dropelems_opt_W_v$skipW_1(o)` back to back.
///
/// The guard asks whether the ENVELOPE still owns a bodies walk, not
/// merely whether the binding is a view — the `param` cell below is why.
///
/// The cells:
///
///   second    the row's own spelling: two `Drop` fields, the SECOND
///             moved. The unmoved first field's body is the one that
///             doubled.
///   first     the same with the FIRST moved, so the defect is not about
///             which index survives.
///   param     a by-value PARAM scrutinee, where the envelope owns no walk
///             in the callee and the mint is the SOLE owner. Keying the
///             guard on the view alone made this print `dQ7 mid end`,
///             losing `dP6` — a lost body traded for a doubled one, which
///             is the worse direction. It is the cell that shaped the fix.
///   one       a single `Drop` field, where the mask empties the walk and
///             an older early return already retracted the envelope's copy
///             — correct before this fix, by accident of that path.
///   readonly  an arm that moves nothing, where the envelope's walk is the
///             sole owner of both fields and must keep running.
///
/// Both `Drop` types are HEAP-FREE and memory is balanced throughout, so
/// these assert on OUTPUT rather than on byte counts. Each asserts the
/// interpreter's own output, which was correct on every cell here.
/// B-2026-09-17-19 — a `shared enum`'s payload `Drop` body now runs on the
/// refcount's 0-transition, and it runs BEFORE the memory walk that frees
/// the payload's buffers. That ordering is the whole risk of the change: a
/// body reads its own fields (`self.s.len()` here), so running it after the
/// walk would be a use-after-free, and running it twice — once as a payload
/// body, once through some other owner — would be a double free of the
/// `String`s it drops.
///
/// The cells are the three payload shapes the drop fn now has to reach: a
/// heap-owning struct, a heap-FREE struct (the case that forced the
/// whole-fn gate to widen, since it is not "walkable"), and a second handle
/// on the same box (where the body must fire once, at the last one).
#[test]
fn asan_shared_enum_payload_body_runs_once_before_the_memory_walk() {
    const DECLS: &str = "struct R { s: String, t: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.s.len()}\") } }\n\
             struct Z { n: i64 }\n\
             impl Drop for Z { fn drop(mut ref self) { println(f\"dZ{self.n}\") } }\n\
             shared enum SMono { P(R), Q }\n\
             shared enum Sz { P(Z), Q }\n";

    // A heap-owning payload: the body reads `self.s` before the walk frees it.
    assert_clean_asan_run(
        &format!(
            "{DECLS}\
                 fn main() {{\n\
                 \x20   {{ let s: SMono = SMono.P(R {{ s: \"ssssssss\", t: \"tttttttt\" }}); }}\n\
                 \x20   println(\"end\");\n\
                 }}\n"
        ),
        &["dR8", "end"],
        "b91719-shared-enum-payload-heap",
    );

    // A heap-FREE payload with a body: not walkable, so the drop fn used to
    // decline outright and nothing ran.
    assert_clean_asan_run(
        &format!(
            "{DECLS}\
                 fn main() {{\n\
                 \x20   {{ let s: Sz = Sz.P(Z {{ n: 5 }}); }}\n\
                 \x20   println(\"end\");\n\
                 }}\n"
        ),
        &["dZ5", "end"],
        "b91719-shared-enum-payload-noheap",
    );

    // Two handles on one box: once, at the last one.
    assert_clean_asan_run(
            &format!(
                "{DECLS}\
                 fn main() {{\n\
                 \x20   {{ let a: SMono = SMono.P(R {{ s: \"ssssssss\", t: \"tttttttt\" }}); let b = a; }}\n\
                 \x20   println(\"end\");\n\
                 }}\n"
            ),
            &["dR8", "end"],
            "b91719-shared-enum-payload-two-handles",
        );

    // THE UNIT-VARIANT CELL, which this fixture deliberately left out until
    // B-2026-09-17-22 was fixed. `{ let s: SMono = SMono.Q; }` used to
    // strand its whole RC shell — 64 B at `-O0`, the `{ i64 rc, i64 tag,
    // .. }` allocation itself — so a cell carrying it would have made this
    // fixture red for a reason that is not its own (B-2026-09-15-10's
    // fixture left the same construction out, in those words). It is in now
    // because the construction is clean, and it belongs here rather than
    // only in that row's own fixture: it is this fixture's enum, so if the
    // payload work ever regresses the shell free, the cell that catches it
    // sits beside the cells that caused it.
    assert_clean_asan_run(
        &format!(
            "{DECLS}\
                 fn main() {{\n\
                 \x20   {{ let s: SMono = SMono.Q; }}\n\
                 \x20   println(\"end\");\n\
                 }}\n"
        ),
        &["end"],
        "b91719-shared-enum-unit-variant",
    );

    // A NAMED source moved into the constructor. This is the cell
    // `e2e_shared_enum_ctor_named_source_runs_no_husk_drop_body` warns
    // about: `d2:0` there is the husk body B-2026-09-17-25 removed, firing
    // off the zeroed staging slot, and this asserts that the `len 8` line
    // it now prints instead comes from live memory with no use-after-free
    // and no double free behind it.
    assert_clean_asan_run(
        &format!(
            "{DECLS}\
                 fn main() {{\n\
                 \x20   let r = R {{ s: \"ssssssss\", t: \"tttttttt\" }};\n\
                 \x20   {{ let s: SMono = SMono.P(r); }}\n\
                 \x20   println(\"end\");\n\
                 }}\n"
        ),
        &["dR8", "end"],
        "b91719-shared-enum-payload-named-source",
    );
}

/// B-2026-09-17-22 — A `shared enum`'s UNIT VARIANT FREES ITS RC SHELL.
///
/// `{ let s = U.Ua; }` over `shared enum U { Ua, Ub }` lost the whole
/// `{ i64 rc, i64 tag, .. }` allocation on every compiled surface, once per
/// evaluation and so unbounded in a loop. The leaked block is the shell
/// itself, sized to the enum's widest variant — 16 B here, 24 B and 64 B in
/// the two shapes the row was filed on — and the payload-carrying variant of
/// the same enum was always clean, which is what localizes this to the UNIT
/// spelling.
///
/// The `let` site retained a value `emit_rc_alloc` had already set to
/// `rc = 1`: `rhs_yields_fresh_ref` matched `Call` / `MethodCall` /
/// `StructLiteral`, and a unit variant is the one fresh-ref source spelled
/// as a two-segment `Path` (`U.Ua`) or a bare `Identifier` (`Ub`). So the
/// count sat at 2 against a single scope-exit dec and LLVM's `rc_free`
/// block was unreachable at runtime.
///
/// The cells are the spellings that read that one predicate: qualified,
/// bare, the assign site, a second handle on the same box, and a use after
/// the binding. `par` is the Arc path, which leaked identically. `shadow`
/// is the cell with teeth in the OTHER direction — a local binding whose
/// name is a variant's makes `let s = Uc;` an ordinary read, and calling
/// that fresh would skip an inc the alias genuinely owes, which is a use
/// after free rather than a leak. It asserts the compiled answer (`n7`);
/// `--interp` prints the VARIANT there instead, a shadowing divergence that
/// predates this row and is B-2026-09-19-37, which is why the paired output
/// fixtures have no `shadow` cell.
///
/// The output twins are `tests/codegen.rs`'s
/// `e2e_shared_enum_unit_variant_runs_its_drop_body` and its interpreter
/// sibling — the bodies below are the visible half of the same defect, since
/// the shell's free is what runs them.
#[test]
fn asan_shared_enum_unit_variant_frees_its_rc_shell() {
    const DECLS: &str = "shared enum U { Ua, Ub, Uc }\n\
             impl Drop for U { fn drop(mut ref self) { println(f\"dU\") } }\n\
             par enum W { Wa, Wb }\n\
             impl Drop for W { fn drop(mut ref self) { println(f\"dW\") } }\n\
             fn tag(u: ref U) -> i64 { match u { U.Ua => { return 1 } U.Ub => { return 0 } U.Uc => { return 2 } } }\n";

    // The qualified spelling — the shape the row was filed on.
    assert_clean_asan_run(
        &format!(
            "{DECLS}\
                 fn main() {{\n\
                 \x20   {{ let s = U.Ua; }}\n\
                 \x20   println(\"end\");\n\
                 }}\n"
        ),
        &["dU", "end"],
        "b91722-unit-variant-qualified",
    );

    // The bare spelling, which reaches the same predicate by a different
    // `ExprKind`.
    assert_clean_asan_run(
        &format!(
            "{DECLS}\
                 fn main() {{\n\
                 \x20   {{ let s = Ub; }}\n\
                 \x20   println(\"end\");\n\
                 }}\n"
        ),
        &["dU", "end"],
        "b91722-unit-variant-bare",
    );

    // The ASSIGN site reads the predicate too, and leaked one shell per
    // store: 32 B for these two constructions.
    assert_clean_asan_run(
        &format!(
            "{DECLS}\
                 fn main() {{\n\
                 \x20   {{ let mut s = U.Ua; s = U.Ub; }}\n\
                 \x20   println(\"end\");\n\
                 }}\n"
        ),
        &["dU", "dU", "end"],
        "b91722-unit-variant-reassign",
    );

    // A second handle on one box: the body fires once, at the last one.
    assert_clean_asan_run(
        &format!(
            "{DECLS}\
                 fn main() {{\n\
                 \x20   {{ let s = U.Ua; let t = s; }}\n\
                 \x20   println(\"end\");\n\
                 }}\n"
        ),
        &["dU", "end"],
        "b91722-unit-variant-alias",
    );

    // A use AFTER the binding — the release lands at the live-range end, so
    // the read must still be on live memory.
    assert_clean_asan_run(
        &format!(
            "{DECLS}\
                 fn main() {{\n\
                 \x20   {{ let s = U.Ua; println(f\"t{{tag(s)}}\"); }}\n\
                 \x20   println(\"end\");\n\
                 }}\n"
        ),
        &["t1", "dU", "end"],
        "b91722-unit-variant-use-later",
    );

    // The Arc path, which leaked identically.
    assert_clean_asan_run(
        &format!(
            "{DECLS}\
                 fn main() {{\n\
                 \x20   {{ let s = W.Wa; }}\n\
                 \x20   println(\"end\");\n\
                 }}\n"
        ),
        &["dW", "end"],
        "b91722-unit-variant-par",
    );

    // The guard in the other direction: a local binding shadows the variant
    // name, so this is a READ and must keep the inc it owes.
    assert_clean_asan_run(
        &format!(
            "{DECLS}\
                 fn main() {{\n\
                 \x20   {{ let Uc = 7; let s = Uc; println(f\"n{{s}}\"); }}\n\
                 \x20   println(\"end\");\n\
                 }}\n"
        ),
        &["n7", "end"],
        "b91722-unit-variant-shadowed",
    );
}

/// B-2026-09-17-7 — the memory half of the two paired output fixtures.
///
/// A generic callee that MAY hand its boxed payload back (`fn mid[T](g:
/// G[T], c: bool) -> G[T] { if c { return g } return G.N }`) left two
/// owners on one box when it did, and stranded the box when a disarm was
/// applied statically to stop that. The runtime compare-and-zero after the
/// call is what tells the two legs apart, so every cell below is the same
/// call under a different answer: handed back, kept inside, always handed
/// back, never handed back, and handed to nobody.
///
/// The last two cells are the ones that fail in OPPOSITE directions, which
/// is why they are both here. `discarded` and `blocktail` leak one box each
/// if the disarm fires where nothing consumes the result; `handback` and
/// `braced` double-free if it does not fire where something does. `braced`
/// is the shape a discarded BLOCK statement hid: its flat twin was clean
/// throughout, so only the braces make it visible.
#[test]
fn asan_generic_handback_leaves_exactly_one_owner_on_the_payload_box() {
    const DECLS: &str = "enum G[T] { Y(T), N }\n\
             fn mid[T](g: G[T], c: bool) -> G[T] { if c { return g } return G.N }\n\
             fn allpaths[T](g: G[T]) -> G[T] { return g }\n\
             fn diesinside[T](g: G[T]) -> G[T] { return G.N }\n\
             fn show(g: G[String]) { match g { G.Y(v) => { println(f\"n{v.len()}\") } G.N => { println(\"none\") } } }\n";

    // Handed back: the caller's binding and the result are the same box.
    assert_clean_asan_run(
        &format!(
            "{DECLS}\
                 fn main() {{\n\
                 \x20   let a: G[String] = G.Y(\"aaaaaaaa-1\");\n\
                 \x20   let b = mid(a, true);\n\
                 \x20   show(b);\n\
                 \x20   println(\"end\");\n\
                 }}\n"
        ),
        &["n10", "end"],
        "b91707-handback",
    );

    // The same call BRACED, with a statement after it. Only this spelling
    // armed the discarded-statement window over the inner call.
    assert_clean_asan_run(
            &format!(
                "{DECLS}\
                 fn main() {{\n\
                 \x20   {{ let a: G[String] = G.Y(\"aaaaaaaa-1\"); let b = mid(a, true); show(b) }}\n\
                 \x20   println(\"end\");\n\
                 }}\n"
            ),
            &["n10", "end"],
            "b91707-braced",
        );

    // Kept inside: the callee owns the box and frees it.
    assert_clean_asan_run(
            &format!(
                "{DECLS}\
                 fn main() {{\n\
                 \x20   {{ let a: G[String] = G.Y(\"bbbbbbbb-2\"); let b = mid(a, false); show(b) }}\n\
                 \x20   println(\"end\");\n\
                 }}\n"
            ),
            &["none", "end"],
            "b91707-kept-inside",
        );

    // Handed back on EVERY path, and on none: the two static twins.
    assert_clean_asan_run(
            &format!(
                "{DECLS}\
                 fn main() {{\n\
                 \x20   {{ let a: G[String] = G.Y(\"cccccccc-3\"); let b = allpaths(a); show(b) }}\n\
                 \x20   {{ let a: G[String] = G.Y(\"dddddddd-4\"); let b = diesinside(a); show(b) }}\n\
                 \x20   println(\"end\");\n\
                 }}\n"
            ),
            &["n10", "none", "end"],
            "b91707-static-twins",
        );

    // Handed to NOBODY, in both spellings the window has to stay armed for:
    // the call as the discarded statement, and the call as a discarded
    // block's tail. Each leaks one box if the disarm fires here.
    assert_clean_asan_run(
        &format!(
            "{DECLS}\
                 fn main() {{\n\
                 \x20   {{ let a: G[String] = G.Y(\"eeeeeeee-5\"); mid(a, true); }}\n\
                 \x20   {{ let a: G[String] = G.Y(\"ffffffff-6\"); mid(a, true) }};\n\
                 \x20   println(\"end\");\n\
                 }}\n"
        ),
        &["end"],
        "b91707-discarded",
    );
}

/// B-2026-09-19-21 — the memory half, one wrapping out from
/// [`Self::asan_generic_handback_leaves_exactly_one_owner_on_the_payload_box`].
///
/// A generic callee that hands its boxed payload back INSIDE AN AGGREGATE
/// (`fn wrap[T](g: G1[T], c: bool) -> H[T] { if c { return H { g: g } }
/// return H { g: G1.N } }`) left two owners on one box. B-2026-09-17-7's
/// runtime compare looks at word 1 of the RETURN, and with a return type of
/// `H[T]` the word to compare sits a field deeper — the shapes do not even
/// agree in type, so the compare declined by construction. The scan now
/// reaches every position inside the returned aggregate whose type is the
/// argument's own enum type.
///
/// Every cell is the same call under a different answer, and the negative
/// ones carry the weight: a WIDER disarm is the direction that strands
/// boxes, so `structF` / `tupleF` (dies-inside legs returning a
/// payload-free variant, box word zero), `allpaths` (the static spelling
/// this change must not disturb), `bare` / `bareF` (the sibling row's own
/// cells) and `discard` (the result consumed by nobody) are what fail if
/// the scan ever admits a word it should not.
///
/// THE RESULT IS CONSUMED BY A CALL, not by an inline `match`, and that is
/// deliberate rather than incidental. `let h = wrap(g, true); match h.g
/// { .. }` strands the box — 24 bytes, one block — because the caller's
/// result binding never arms a box drop for a generic enum sitting in a
/// returned aggregate's field. On the tree BEFORE this change that leak was
/// invisible on these cells: the callee argument's still-armed drop freed
/// the box anyway, which is the invalid second free this row is about, so
/// what the change really does here is stop a wrong free from standing in
/// for a missing one. The ALL-PATHS spelling `wrapAll` is the cell that
/// shows the leak pre-dates the change — its argument was already disarmed
/// by B-2026-09-16-16's static arm, so it leaked 24 bytes on the parent
/// tree too. B-2026-09-19-35 owns that missing drop; writing the cells with
/// an inline `match` here would pin it rather than this row's double free,
/// and would fail the Linux LeakSanitizer leg today.
#[test]
fn asan_generic_aggregate_handback_leaves_exactly_one_owner_on_the_payload_box() {
    const DECLS: &str = "enum G1[T] { Y(T), N }\n\
             struct H[T] { g: G1[T] }\n\
             struct H2[T] { h: H[T] }\n\
             fn wrap[T](g: G1[T], c: bool) -> H[T] { if c { return H { g: g } } return H { g: G1.N } }\n\
             fn wrapAll[T](g: G1[T]) -> H[T] { return H { g: g } }\n\
             fn wrapTup[T](g: G1[T], c: bool) -> (G1[T], i64) { if c { return (g, 7) } return (G1.N, 7) }\n\
             fn wrapNest[T](g: G1[T], c: bool) -> H2[T] { if c { return H2 { h: H { g: g } } } return H2 { h: H { g: G1.N } } }\n\
             fn bare[T](g: G1[T], c: bool) -> G1[T] { if c { return g } return G1.N }\n\
             fn shw(g: G1[String]) { match g { G1.Y(v) => { println(f\"mx {v.len()}\") } G1.N => { println(\"mx 0\") } } }\n";

    // Handed back inside a STRUCT — the cell that double freed.
    assert_clean_asan_run(
            &format!(
                "{DECLS}\
                 fn main() {{\n\
                 \x20   {{ let g: G1[String] = G1.Y(\"aaaaaaaa-1\"); let h = wrap(g, true); shw(h.g) }}\n\
                 \x20   println(\"end\");\n\
                 }}\n"
            ),
            &["mx 10", "end"],
            "b91921-struct",
        );

    // Inside a TUPLE and inside a NESTED struct — the same defect through
    // the other two aggregate shapes.
    assert_clean_asan_run(
            &format!(
                "{DECLS}\
                 fn main() {{\n\
                 \x20   {{ let g: G1[String] = G1.Y(\"bbbbbbbb-2\"); let t = wrapTup(g, true); shw(t.0) }}\n\
                 \x20   {{ let g: G1[String] = G1.Y(\"cccccccc-3\"); let h = wrapNest(g, true); shw(h.h.g) }}\n\
                 \x20   println(\"end\");\n\
                 }}\n"
            ),
            &["mx 10", "mx 10", "end"],
            "b91921-tuple-and-nested",
        );

    // The DIES-INSIDE legs: the callee keeps the box and returns a
    // payload-free variant, so the caller must still free its own.
    assert_clean_asan_run(
            &format!(
                "{DECLS}\
                 fn main() {{\n\
                 \x20   {{ let g: G1[String] = G1.Y(\"dddddddd-4\"); let h = wrap(g, false); shw(h.g) }}\n\
                 \x20   {{ let g: G1[String] = G1.Y(\"eeeeeeee-5\"); let t = wrapTup(g, false); shw(t.0) }}\n\
                 \x20   println(\"end\");\n\
                 }}\n"
            ),
            &["mx 0", "mx 0", "end"],
            "b91921-dies-inside",
        );

    // The cells this change must NOT disturb: the static all-paths
    // aggregate spelling, and the sibling row's bare hand-back.
    assert_clean_asan_run(
            &format!(
                "{DECLS}\
                 fn main() {{\n\
                 \x20   {{ let g: G1[String] = G1.Y(\"ffffffff-6\"); let h = wrapAll(g); shw(h.g) }}\n\
                 \x20   {{ let g: G1[String] = G1.Y(\"gggggggg-7\"); let b = bare(g, true); shw(b) }}\n\
                 \x20   {{ let g: G1[String] = G1.Y(\"hhhhhhhh-8\"); let b = bare(g, false); shw(b) }}\n\
                 \x20   println(\"end\");\n\
                 }}\n"
            ),
            &["mx 10", "mx 10", "mx 0", "end"],
            "b91921-untouched",
        );

    // Handed to NOBODY: the result is discarded, so the caller's binding is
    // the box's only owner and a disarm here would strand it.
    assert_clean_asan_run(
        &format!(
            "{DECLS}\
                 fn main() {{\n\
                 \x20   {{ let g: G1[String] = G1.Y(\"iiiiiiii-9\"); wrap(g, true); }}\n\
                 \x20   println(\"end\");\n\
                 }}\n"
        ),
        &["end"],
        "b91921-discarded",
    );
}

/// B-2026-09-19-35 — the MEMORY twin of `tests/codegen.rs`'s
/// `e2e_boxed_erased_payload_survives_every_handoff_spelling`, which
/// asserts stdout only.
///
/// Both halves of this row are invisible to a stdout assertion in one
/// direction each. A missing free is a LEAK, which prints nothing and is
/// seen only under LSan. A missing NEUTRALIZER is a double free, which
/// aborts — that one an E2E cell does catch, but by truncation, so it
/// reports the same way as any other crash. Only this leg says which.
///
/// The three spellings are three EMISSION SITES, not three phrasings of
/// one: `shw(h.g);` drains at `compile_stmt`, the same call as a block's
/// final expression at `compile_block`, and as a function body's final
/// expression at `compile_function_body`. The tail forms were the ones
/// with no drain at all.
#[test]
fn asan_boxed_erased_payload_survives_every_handoff_spelling() {
    const DECLS: &str = "enum G1[T] { Y(T), N }\n\
             struct H[T] { g: G1[T] }\n\
             struct H2[T] { h: H[T] }\n\
             struct In1[T] { g: G1[T] }\n\
             struct Out1[T] { g: In1[T] }\n\
             fn wrap[T](g: G1[T], c: bool) -> H[T] { if c { return H { g: g } } return H { g: G1.N } }\n\
             fn wrapNest[T](g: G1[T], c: bool) -> H2[T] { if c { return H2 { h: H { g: g } } } return H2 { h: H { g: G1.N } } }\n\
             fn wrapSame[T](g: G1[T], c: bool) -> Out1[T] { if c { return Out1 { g: In1 { g: g } } } return Out1 { g: In1 { g: G1.N } } }\n\
             fn shw(g: G1[String]) { match g { G1.Y(v) => { println(f\"mx {v.len()}\") } G1.N => { println(\"mx 0\") } } }\n\
             fn fnTail() { let g: G1[String] = G1.Y(\"ffffffff-6\"); let h = wrap(g, true); shw(h.g) }\n";

    // The three emission sites, on the payload-CARRYING variant.
    assert_clean_asan_run(
            &format!(
                "{DECLS}\
                 fn main() {{\n\
                 \x20   {{ let g: G1[String] = G1.Y(\"aaaaaaaa-1\"); let h = wrap(g, true); shw(h.g) }}\n\
                 \x20   {{ let g: G1[String] = G1.Y(\"bbbbbbbb-2\"); let h = wrap(g, true); shw(h.g); }}\n\
                 \x20   fnTail();\n\
                 \x20   println(\"end\");\n\
                 }}\n"
            ),
            &["mx 10", "mx 10", "mx 10", "end"],
            "b1935-tail-stmt-fntail",
        );

    // A CHAINED place, one hop deeper than the neutralizer used to reach,
    // and the same chain with the hop and the field sharing a name.
    assert_clean_asan_run(
            &format!(
                "{DECLS}\
                 fn main() {{\n\
                 \x20   {{ let g: G1[String] = G1.Y(\"cccccccc-3\"); let h = wrapNest(g, true); shw(h.h.g) }}\n\
                 \x20   {{ let g: G1[String] = G1.Y(\"dddddddd-4\"); let o = wrapSame(g, true); shw(o.g.g) }}\n\
                 \x20   println(\"end\");\n\
                 }}\n"
            ),
            &["mx 10", "mx 10", "end"],
            "b1935-chain",
        );

    // The payload-FREE variant through the same spellings: a neutralizer
    // that fired unconditionally on a variant holding no box would show
    // here, where the caller is still the only owner of nothing.
    assert_clean_asan_run(
            &format!(
                "{DECLS}\
                 fn main() {{\n\
                 \x20   {{ let g: G1[String] = G1.Y(\"eeeeeeee-5\"); let h = wrap(g, false); shw(h.g) }}\n\
                 \x20   {{ let g: G1[String] = G1.Y(\"gggggggg-7\"); let h = wrapNest(g, false); shw(h.h.g) }}\n\
                 \x20   {{ let g: G1[String] = G1.Y(\"hhhhhhhh-8\"); let o = wrapSame(g, false); shw(o.g.g) }}\n\
                 \x20   println(\"end\");\n\
                 }}\n"
            ),
            &["mx 0", "mx 0", "mx 0", "end"],
            "b1935-empty-variant",
        );
}

/// B-2026-09-17-8 — the memory half of the two paired output fixtures.
///
/// Compiling a monomorph mid-caller wiped the caller's payload-ownership
/// records, so the binding that received a passthrough generic's result
/// took a second registration over the argument's payload and both freed
/// it. The cells are the shapes that told the channels apart while the
/// root cause was being found: an inline `Option` payload, the `Result`
/// head, no match arm at all, and two calls to ONE monomorph — the last
/// being the cell that refutes the argument-side disarm this row first
/// reached for, which fixes the first call and leaks every later one.
///
/// The `Drop`-bearing cells are here for the opposite failure. They are
/// memory-balanced either way, so nothing in this file catches the body
/// doubling the memory fix exposed — that is the paired output fixtures'
/// job. What they assert here is that retracting the source's payload
/// walk did not also strand its heap: `W` carries a `String`, and
/// dropping the walk without the memory would lose that buffer.
#[test]
fn asan_generic_passthrough_leaves_one_owner_on_the_payload() {
    const DECLS: &str = "struct D { id: i64 }\n\
             impl Drop for D { fn drop(mut ref self) { println(f\"dD{self.id}\") } }\n\
             struct W { id: i64, s: String }\n\
             impl Drop for W { fn drop(mut ref self) { println(f\"dW{self.id}\") } }\n\
             fn idOpt[T](g: Option[T]) -> Option[T] { return g }\n\
             fn idRes[T](g: Result[T, i64]) -> Result[T, i64] { return g }\n";

    // The inline `Option` payload — the row's own repro.
    assert_clean_asan_run(
            &format!(
                "{DECLS}\
                 fn main() {{\n\
                 \x20   let a: Option[String] = Option.Some(\"aaaaaaaa-1\");\n\
                 \x20   let b = idOpt(a);\n\
                 \x20   match b {{ Option.Some(v) => {{ println(f\"n{{v.len()}}\") }} Option.None => {{ println(\"none\") }} }}\n\
                 \x20   println(\"end\");\n\
                 }}\n"
            ),
            &["n10", "end"],
            "b91708-inline-option",
        );

    // The `Result` head, and the no-arm spelling: the defect needs
    // neither a match arm nor a consumed result.
    assert_clean_asan_run(
            &format!(
                "{DECLS}\
                 fn main() {{\n\
                 \x20   {{ let a: Result[String, i64] = Result.Ok(\"bbbbbbbb-2\"); let b = idRes(a); match b {{ Result.Ok(v) => {{ println(f\"n{{v.len()}}\") }} Result.Err(e) => {{ println(\"err\") }} }} }}\n\
                 \x20   {{ let a: Option[String] = Option.Some(\"cccccccc-3\"); let b = idOpt(a); }}\n\
                 \x20   println(\"end\");\n\
                 }}\n"
            ),
            &["n10", "end"],
            "b91708-result-and-noarm",
        );

    // TWO calls to one monomorph. The argument-side disarm this row first
    // reached for fixes the first and leaks every later one, so a
    // single-call cell cannot tell the two repairs apart.
    assert_clean_asan_run(
            &format!(
                "{DECLS}\
                 fn main() {{\n\
                 \x20   let a: Option[String] = Option.Some(\"dddddddd-4\");\n\
                 \x20   let x = idOpt(a);\n\
                 \x20   let c: Option[String] = Option.Some(\"eeeeeeee-5\");\n\
                 \x20   let y = idOpt(c);\n\
                 \x20   match x {{ Option.Some(v) => {{ println(f\"n{{v.len()}}\") }} Option.None => {{ println(\"none\") }} }}\n\
                 \x20   match y {{ Option.Some(v) => {{ println(f\"n{{v.len()}}\") }} Option.None => {{ println(\"none\") }} }}\n\
                 \x20   println(\"end\");\n\
                 }}\n"
            ),
            &["n10", "n10", "end"],
            "b91708-two-calls",
        );

    // The two `Drop`-bearing payload classes, one word and four. Balanced
    // either way, so what these assert is that retracting the source's
    // payload walk left `W`'s `String` with an owner.
    assert_clean_asan_run(
            &format!(
                "{DECLS}\
                 fn main() {{\n\
                 \x20   {{ let a: Option[D] = Option.Some(D {{ id: 6 }}); let b = idOpt(a); match b {{ Option.Some(v) => {{ println(f\"g{{v.id}}\") }} Option.None => {{ println(\"none\") }} }} }}\n\
                 \x20   {{ let a: Option[W] = Option.Some(W {{ id: 7, s: \"ssssssss\" }}); let b = idOpt(a); match b {{ Option.Some(v) => {{ println(f\"g{{v.id}}\") }} Option.None => {{ println(\"none\") }} }} }}\n\
                 \x20   {{ let a: Option[W] = Option.Some(W {{ id: 8, s: \"tttttttt\" }}); let b = idOpt(a); }}\n\
                 \x20   println(\"end\");\n\
                 }}\n"
            ),
            &["g6", "dD6", "g7", "dW7", "dW8", "end"],
            "b91708-drop-payloads",
        );
}

/// B-2026-09-19-39 — A GENERIC MULTI-FIELD VARIANT'S BOXED `Array` PAYLOAD
/// WAS STRANDED, BECAUSE REGISTERING IT WOULD HAVE COST THE SIBLING.
///
/// B-2026-09-15-18 gave such a variant a `BoxedEnumDrop` but stood the
/// WHOLE variant down the moment any field carried a live drop kind,
/// deliberately: membership of `boxed_enum_payload_vars` arms
/// `suppress_inline_option_result_binding_move_impl`, which disarms a moved
/// binding by zeroing the whole slot. That is sound for every shape it was
/// written against, where the box IS the payload. In a multi-field variant
/// a heap-bearing SIBLING has its own `cap > 0` guard in the same slot, so
/// the whole-slot store cleared that too and the sibling's buffer was owned
/// by nobody — the box recovered at the cost of a previously-freed buffer,
/// which that row correctly declined to trade.
///
/// The disarm now zeroes the box's OWN words, so both stand: the stand-down
/// is per-FIELD and the sibling keeps its guard. Measured at `-O0` under
/// `valgrind --leak-check=full`, this program loses 256 B in 8 blocks
/// before and is clean after, with `--interp` and AOT byte-identical
/// throughout.
///
/// FIVE ROWS, and three of them are here because two earlier versions of
/// this fix passed the first two and broke them:
///
///  * `Y(T, String)` — the row's own shape, box FIRST.
///  * `Y(String, T)` — box SECOND, so the disarmed word is not word 0.
///  * `Y(T, T)` — TWO boxes on one binding. A version that recorded one
///    word per binding kept only the last and left the first box armed
///    through the move: exit 134 on a cell that had been clean.
///  * `Y(T, Vec[String])` — the sibling's heap is a `Vec`, not a `String`.
///  * the same shape matched LOCALLY, never passed to a callee, which is
///    what says the strand is the registration rather than the
///    argument-move path the row guessed at.
///
/// The element type is `i64` DELIBERATELY. An `Array[String, N]` payload
/// still leaks its ELEMENTS here — the registration is box-only and the
/// interior is B-2026-09-16-15's subject — so a `String`-element cell
/// cannot appear in a clean-run fixture while that row is open. Every cell
/// below owns exactly one box plus its sibling's heap, which is what this
/// row is about.
#[test]
fn asan_generic_multi_field_variant_box_and_heap_sibling_both_freed() {
    assert_clean_asan_run(
        r#"
enum Gh[T] { Y(T, String), N }
enum Ghs[T] { Y(String, T), N }
enum Gt[T] { Y(T, T), N }
enum Gv[T] { Y(T, Vec[String]), N }
fn mkI(i: i64) -> Array[i64, 4] { return [i, i + 1, i + 2, i + 3]; }
fn mkv(s: String) -> Vec[String] { let mut v: Vec[String] = Vec.new(); v.push(s); return v; }
fn fh(g: Gh[Array[i64, 4]]) -> i64 { match g { Gh.Y(x, s) => { return x[0] + s.len(); } Gh.N => { return 0; } } }
fn fhs(g: Ghs[Array[i64, 4]]) -> i64 { match g { Ghs.Y(s, x) => { return x[0] + s.len(); } Ghs.N => { return 0; } } }
fn ft(g: Gt[Array[i64, 4]]) -> i64 { match g { Gt.Y(x, y) => { return x[0] + y[1]; } Gt.N => { return 0; } } }
fn fv(g: Gv[Array[i64, 4]]) -> i64 { match g { Gv.Y(x, v) => { return x[0] + v.len(); } Gv.N => { return 0; } } }
fn main() {
    let mut i = 1;
    while i < 3 {
        { let g: Gh[Array[i64, 4]] = Gh.Y(mkI(i), f"sib-{i}-padpadpad"); println(f"h:{fh(g)}"); }
        { let g: Ghs[Array[i64, 4]] = Ghs.Y(f"sib-{i}-padpadpad", mkI(i)); println(f"s:{fhs(g)}"); }
        { let g: Gt[Array[i64, 4]] = Gt.Y(mkI(i), mkI(i + 10)); println(f"t:{ft(g)}"); }
        { let g: Gv[Array[i64, 4]] = Gv.Y(mkI(i), mkv(f"v-{i}-padpadpad")); println(f"v:{fv(g)}"); }
        { let g: Gh[Array[i64, 4]] = Gh.Y(mkI(i), f"loc-{i}-padpadpad");
          match g { Gh.Y(x, s) => { println(f"L:{x[0] + s.len()}"); } Gh.N => { println("n"); } } }
        i = i + 1;
    }
    println("end");
}
"#,
        &[
            "h:16", "s:16", "t:13", "v:2", "L:16", "h:17", "s:17", "t:15", "v:3", "L:17", "end",
        ],
        "asan_generic_multi_field_variant_box_and_heap_sibling_both_freed",
    );
}

/// B-2026-09-19-51 — a struct's ENUM FIELD handed to a by-value callee that
/// owns it BY TRANSFER was freed in both frames.
///
/// A by-value enum param whose payload is BOXED is owned by transfer
/// (`enum_param_owned_by_transfer`): the callee frees the box, and
/// `param_own`'s prologue records that this is held "in LOCKSTEP with the
/// three caller-side retractions". `eatb(h.g)` is a fourth caller shape
/// none of those three reached — they match a bare identifier, and a
/// field's free lives inside `__karac_drop_struct_<S>`, a function in no
/// scope's action list to retract. Stock `main` reported 11 allocs / 14
/// frees, 3 `Invalid free` and 4 `Invalid read`, all in `main`.
///
/// THIS IS THE ONLY FIXTURE THAT CAN SEE THE DEFECT, which is why there is
/// no output twin for it. Every cell below prints the SAME thing before and
/// after the fix — the values were never wrong and `--interp` was always
/// correct. A transcript fixture would pass on the broken tree.
///
/// `Eb`'s payload is `Array[String, 2]` and is boxed even though nothing
/// here is generic: a variant DECLARATION spells an array as
/// `Path(["Array"], [Type, Const])`, which `payload_word_count_for_type_expr`
/// measures at its conservative one-word tail (B-2026-09-12-12).
///
/// The last three cells are the ones a WIDER neutralizer breaks, and they
/// are the point of the fixture as much as the first two. An INLINE-payload
/// enum field is entry-COPIED by the callee, so the caller still owns its
/// own buffer and a zero there would strand it; the `Vec` field is the same
/// question one type over; and the whole-struct move already had its own
/// neutralizer and must not gain a second.
///
/// NOT COVERED, both measured against stock rather than assumed, both still
/// this row's double free: `self.g` inside a method (14 errors / 8 invalid,
/// unchanged — an owned by-value receiver entry-copies without duplicating
/// the box, so three frames own it) and a CHAINED place `k.h.g` (7 errors,
/// unchanged — this resolves one hop only).
#[test]
fn asan_enum_field_handed_to_a_by_value_callee_has_one_owner() {
    const DECLS: &str = "enum Eb { A(Array[String, 2]), B }
             struct Hb { g: Eb }
             enum Ei { A(String), B }
             struct Hi { g: Ei }
             struct Hv { v: Vec[String] }
             struct Sink { n: i64 }
             impl Sink { fn take(ref self, g: Eb) { println(\"mtake\") } }
             fn eatb(g: Eb) { println(\"ate\") }
             fn eati(g: Ei) { println(\"atei\") }
             fn eatv(v: Vec[String]) { println(f\"atev{v.len()}\") }
             fn eath(h: Hb) { println(\"ateh\") }
             fn mk() -> Array[String, 2] { return [f\"aaaaaaaa-1\", f\"bbbbbbbb-2\"]; }
";

    // The filed cell: a free function, and the method spelling beside it —
    // the two the caller-side stand-down now reaches.
    assert_clean_asan_run(
        &format!(
            "{DECLS}                 fn main() {{
                 \x20   {{ let h = Hb {{ g: Eb.A(mk()) }}; eatb(h.g); }}
                 \x20   {{ let h = Hb {{ g: Eb.A(mk()) }}; let s = Sink {{ n: 1 }}; s.take(h.g); }}
                 \x20   println(\"end\");
                 }}
"
        ),
        &["ate", "mtake", "end"],
        "b91951-field-to-callee",
    );

    // The direction a WIDER stand-down breaks: an inline payload and a
    // `Vec` field are entry-copied, so the caller still owns its buffer and
    // zeroing would strand it; the whole-struct move already stands itself
    // down and must not do it twice.
    assert_clean_asan_run(
            &format!(
                "{DECLS}                 fn main() {{
                 \x20   {{ let h = Hi {{ g: Ei.A(f\"cccccccc-3\") }}; eati(h.g); }}
                 \x20   {{ let mut v: Vec[String] = Vec.new(); v.push(f\"dddddddd-4\"); let h = Hv {{ v: v }}; eatv(h.v); }}
                 \x20   {{ let h = Hb {{ g: Eb.A(mk()) }}; eath(h); }}
                 \x20   println(\"end\");
                 }}
"
            ),
            &["atei", "atev1", "ateh", "end"],
            "b91951-must-stay-clean",
        );
}

/// B-2026-09-20-14 — a GENERIC enum's heap-BOXED payload, held as a TUPLE
/// ELEMENT, had no owner at all.
///
/// A generic enum sizes its payload area from the DECLARATION, so
/// `payload_word_count_for_type_expr` reads `T` through its `_ => 1` tail
/// and gives one word; a monomorph that outgrows it is heap-BOXED by
/// `coerce_to_payload_words`. The tuple let-site never learned that: every
/// namer in `infer_arg_elem_te` ends in `generic_args: None`, so
/// `let t = (g, 7)` named the element bare `G1`, the admit gate read the
/// tuple as heapless, and NO tuple drop function was synthesized. Measured
/// 24 B direct with ZERO indirect — only the envelope, because the match
/// arm already took the payload's own heap, so the program's output says
/// nothing is wrong.
///
/// The last cell is the one that separates the two candidate explanations.
/// "The payload carries heap" is NOT the discriminator — payload WIDTH is.
/// `Wide` is three words of pure `i64` with no heap anywhere in the
/// program, and it boxed and leaked exactly like the `String` payload,
/// while `G1[i64]` (one word, fits) is clean on both. A matrix varying
/// scalar-versus-heap here is really varying inline-versus-boxed, and the
/// two come apart only at a payload that is scalar AND wide.
///
/// The NEVER-MATCHED spelling (`let t = (g, 7);` with no match on `t.0`)
/// is deliberately absent, for the reason
/// `asan_generic_multi_field_variant_box_and_heap_sibling_both_freed`
/// gives one row over: the element walker is envelope-only, so that
/// spelling still leaks the payload's own 13 B and cannot appear in a
/// clean-run fixture while that remainder is open. It went from 24 B + 13
/// indirect to 13 B and stays on the row.
#[test]
fn asan_boxed_generic_enum_tuple_element_envelope_is_freed() {
    assert_clean_asan_run(
        r#"
struct Wide { a: i64, b: i64, c: i64 }
enum G1[T] { Y(T), N }
fn main() {
    { let g: G1[String] = G1.Y(f"payload-alpha"); let t = (g, 7);
      match t.0 { G1.Y(s) => { println(f"mv {s}") } G1.N => { println("mv none") } } }
    { let g: G1[String] = G1.Y(f"payload-beta"); let t = (g, 8);
      match t.0 { G1.Y(s) => { println(f"ml {s.len()}") } G1.N => { println("ml none") } } }
    { let g: G1[i64] = G1.Y(41); let t = (g, 9);
      match t.0 { G1.Y(n) => { println(f"mn {n}") } G1.N => { println("mn none") } } }
    { let g: G1[Wide] = G1.Y(Wide { a: 1, b: 2, c: 3 }); let t = (g, 10);
      match t.0 { G1.Y(w) => { println(f"mw {w.a}{w.b}{w.c}") } G1.N => { println("mw none") } } }
    println("end")
}
"#,
        &["mv payload-alpha", "ml 12", "mn 41", "mw 123", "end"],
        "b92014-tuple-element-box",
    );
}

/// B-2026-09-20-15 — a DISCARDED generic enum whose monomorph's payload is
/// heap-BOXED had no owner, so the box leaked outright.
///
/// The twin of `e2e_discarded_boxed_generic_enum_payload_runs_its_drop_body`
/// in `tests/codegen.rs`, and both are needed because the site made two
/// separate omissions on two separate channels. This one scores the box:
/// `heap_payload` is keyed on the enum's declared payload NAME, which for a
/// generic is the erased `T`, so it answered false and nothing freed the
/// allocation `coerce_to_payload_words` had just made. Measured at base,
/// `KARAC_OPT_LEVEL=0`: 32 B direct + 9 indirect on the `String` payload,
/// 16 B on the all-scalar one.
///
/// `dN46` is the monomorph that FITS the one-word area and so never boxed;
/// it was clean before the fix and must stay clean, since an owner that
/// registered for it would free a pointer that was never allocated. The
/// last two blocks are the positions that already HAD an owner — the
/// non-payload variant of a boxing enum, and the argument position, whose
/// callee owns it — so a double free is what a too-wide repair looks like
/// here, and ASAN is the instrument that sees it.
#[test]
fn asan_discarded_boxed_generic_enum_payload_box_is_freed() {
    assert_clean_asan_run(
        r#"
struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct W { id: i64, n2: i64 }
impl Drop for W { fn drop(mut ref self) { println(f"dW{self.id}") } }
struct N { id: i64 }
impl Drop for N { fn drop(mut ref self) { println(f"dN{self.id}") } }
enum Gen[T] { Y(T), Z }
enum Mix[T] { W(T), N(i64) }
fn mkR(i: i64) -> R { return R { id: i, s: f"p{i}" }; }
fn eat(g: Gen[R]) { println("ate") }
fn main() {
    { let _ = Gen.Y(R { id: 47, s: f"payload-a" }); }
    { let _ = Gen.Y(mkR(48)); }
    { let _ = Gen.Y(W { id: 49, n2: 1 }); }
    { let _ = Gen.Y(N { id: 46 }); }
    { let _ = Mix.W(R { id: 62, s: f"payload-m" }); }
    { let _ = Mix[R].N(63); }
    { eat(Gen.Y(R { id: 64, s: f"payload-e" })); }
    println("end")
}
"#,
        &["dR47", "dR48", "dW49", "dN46", "dR62", "ate", "dR64", "end"],
        "b92015-discarded-boxed-payload",
    );
}

/// B-2026-09-20-13 — the memory half of
/// `e2e_reused_by_value_generic_enum_argument_is_not_freed_under_the_caller`.
///
/// The callee's by-value param registers the box's free
/// (`track_boxed_enum_var_with_inner_drop_for_payload`), on the premise —
/// written down at the discard-side stand-down in `call_dispatch.rs` — that
/// at an argument the callee owns the box. That premise holds only while
/// the caller does not touch the binding again. When it does, the caller
/// reads a freed box and its own scope-exit drop frees it a second time.
///
/// Each block's payload has a DISTINCT LENGTH so a leaked or double-freed
/// byte count names the block that produced it rather than the fixture.
/// Un-indented on purpose: the helper trims leading whitespace on the first
/// line only.
#[test]
fn asan_reused_by_value_generic_enum_argument_is_freed_exactly_once() {
    assert_clean_asan_run(
        r#"
enum Gen[T] { Y(T), N }
struct Holder { g: Gen[String] }
struct Hg[T] { g: Gen[T] }
struct Wide { a: String, b: String, c: String }

fn shw(g: Gen[String]) { match g { Gen.Y(v) => { println(f"s:{v}") } Gen.N => { println("s:none") } } }
fn idf(g: Gen[String]) -> Gen[String] { return g }
fn shs(g: Gen[Wide]) { match g { Gen.Y(v) => { println(f"w:{v.a}") } Gen.N => { println("w:none") } } }
fn shv(g: Gen[Vec[String]]) { match g { Gen.Y(v) => { println(f"v:{v.len()}") } Gen.N => { println("v:none") } } }
fn wrap[T](g: Gen[T], c: bool) -> Hg[T] { if c { return Hg { g: g } } return Hg { g: Gen.N } }

fn main() {
    let a: Gen[String] = Gen.Y(f"aa-local-2"); shw(a); shw(a)
    let b: Gen[String] = Gen.Y(f"bbb-thrice-33"); shw(b); shw(b); shw(b)
    let c: Holder = Holder { g: Gen.Y(f"cccc-field-444") }; shw(c.g); shw(c.g)
    let d: Gen[Wide] = Gen.Y(Wide { a: f"ddddd-struct-5555", b: f"ddddd-struct-5556", c: f"ddddd-struct-5557" }); shs(d); shs(d)
    let mut q: Vec[String] = Vec.new(); q.push(f"eeeeee-vecelem-66666"); let e: Gen[Vec[String]] = Gen.Y(q); shv(e); shv(e)
    let f: Gen[String] = Gen.Y(f"fffffff-escape-777777"); let h: Gen[String] = idf(f); shw(f); shw(h)
    let i: Hg[String] = Hg { g: Gen.Y(f"gggggggg-genfield-8888888") }; shw(i.g); shw(i.g)
    let j: Gen[String] = Gen.Y(f"hhhhhhhhh-wrapped-99999999"); let k: Hg[String] = wrap(j, true); shw(k.g); shw(k.g)
    println("done")
}
"#,
        &[
            "s:aa-local-2",
            "s:aa-local-2",
            "s:bbb-thrice-33",
            "s:bbb-thrice-33",
            "s:bbb-thrice-33",
            "s:cccc-field-444",
            "s:cccc-field-444",
            "w:ddddd-struct-5555",
            "w:ddddd-struct-5555",
            "v:1",
            "v:1",
            "s:fffffff-escape-777777",
            "s:fffffff-escape-777777",
            "s:gggggggg-genfield-8888888",
            "s:gggggggg-genfield-8888888",
            "s:hhhhhhhhh-wrapped-99999999",
            "s:hhhhhhhhh-wrapped-99999999",
            "done",
        ],
        "b2026-09-20-13-reused-by-value-generic-enum-arg",
    );
}

/// B-2026-09-21-10 — a generic enum's NARROW variant (`A(T)`, one payload
/// word) leaks its heap-boxed payload when a WIDER sibling variant
/// (`B(Vec[T])`, three words) inflates the enum's payload area past it.
///
/// `coerce_to_payload_words` boxes when the instantiated value outgrows the
/// words THIS variant's field was declared with — one word for a bare `T` —
/// but `user_enum_boxed_payload_variants` sized the box-free test against the
/// enum's AREA, the widest variant's width. For `Mix[Vec[R]]` those are 1 and
/// 3, so `A`'s 3-word `Vec[R]` payload was boxed by the constructor and then
/// `3 > 3` classified it inline: the box (24 B) and its buffer (16 B) leaked,
/// with no `match` in the program. The single-payload twin
/// `Slot[T] { S(T), N }` at the same `T` is clean only because its area equals
/// its one variant's width — it is the DISAGREEMENT between the two widths, not
/// boxing itself, and this is the box-free twin of the bodies channel
/// B-2026-09-20-62 fixed.
///
/// The `Array[R, 2]` instantiation is the same fault via the array-spelling
/// path (a variant declaration can only write `Array[T, N]`, which
/// `payload_word_count_for_type_expr` sizes at one word); it leaked 16 B. The
/// consuming-`match` cell is here because widening the box-free test could in
/// principle double-free where an arm takes the payload — it does not: the arm
/// owns the interior and the box drop stands down, so ASAN stays clean.
///
/// A pure output oracle (`e2e_generic_enum_container_payload_positions`)
/// already carries the drop-body lines and passed throughout — a body
/// assertion cannot see a leak, which is the blind spot this ASAN twin closes.
#[test]
fn asan_generic_enum_narrow_variant_boxed_payload_freed() {
    assert_clean_asan_run(
        r#"
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
enum Mix[T] { A(T), B(Vec[T]), N }
fn main() {
    let a: Vec[R] = [R { id: 1 }, R { id: 2 }];
    let m: Mix[Vec[R]] = Mix.A(a);
    println("mA");

    let b: Array[R, 2] = [R { id: 3 }, R { id: 4 }];
    let n: Mix[Array[R, 2]] = Mix.A(b);
    println("mArr");

    let c: Vec[R] = [R { id: 5 }, R { id: 6 }];
    let o: Mix[Vec[R]] = Mix.A(c);
    match o { Mix.A(v) => { println(f"got{v.len()}") } Mix.B(_) => { println("b") } Mix.N => { println("n") } }
    println("end");
}
"#,
        &[
            "dR1", "dR2", "mA", "dR3", "dR4", "mArr", "got2", "dR5", "dR6", "end",
        ],
        "b2026-09-21-10-generic-enum-narrow-variant-box",
    );
}

/// B-2026-09-20-55 — a MULTI-FIELD enum variant's boxed `Array` payload whose
/// element runs a user `Drop` body had no free on any backend.
///
/// 136 bytes per value at `-O0` — 64 direct (the box) and 72 indirect (the
/// elements' `String`s) — and the row's own shape is the generic one, but the
/// CONCRETE spelling leaks identically and runs its bodies correctly, which is
/// what says the leak is keyed on the variant's ARITY and not on erasure. The
/// no-call cell is the decisive one: there is no callee in it, so the missing
/// free was never about the by-value param path.
///
/// `declarations.rs` declined to classify the field `EnumDropKind::BoxedArray`
/// at multi-field width, because classifying it flipped
/// `enum_param_owned_by_transfer` and moved the element bodies into the callee
/// — B-2026-09-15-17's ordering divergence. That class is caller-sequenced now,
/// so the clause came out and the box is freed. The order half is pinned by
/// `codegen`'s and `interpreter`'s
/// `drop_order::*_boxed_array_enum_payload_bodies_are_caller_sequenced`.
///
/// OBSERVABLE ONLY AT `-O0`: at the default opt level LLVM deletes an
/// allocation nothing observes, so an ordinary `--features llvm` run of this
/// fixture is VACUOUS and `scripts/asan-o0-leg.sh` is where an unfixed tree
/// reports it. Measured on the unfixed tree as 136 B per round.
#[test]
fn asan_multi_field_boxed_array_enum_payload_is_freed() {
    // The row's own shape: the value is handed to a by-value callee.
    assert_clean_asan_run(
        "struct R { id: i64, s: String }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
         enum C2 { X(Array[R, 2], i64) }\n\
         fn mkarr(b: i64) -> Array[R, 2] {\n\
         \x20   return [R { id: b, s: f\"pay-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-{b}\" },\n\
         \x20           R { id: b + 1, s: f\"pay-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-{b}\" }];\n\
         }\n\
         fn eat2(w: C2) -> i64 { match w { C2.X(a, n) => { return a[0].id + n } } }\n\
         fn main() {\n\
         \x20   let mut i: i64 = 0i64;\n\
         \x20   while i < 3i64 {\n\
         \x20       { let v: C2 = C2.X(mkarr(i), 20); println(f\"s{eat2(v)}\") }\n\
         \x20       i = i + 1i64;\n\
         \x20   }\n\
         \x20   println(\"end\");\n\
         }\n",
        &[
            "s20", "dR0", "dR1", "s21", "dR1", "dR2", "s22", "dR2", "dR3", "end",
        ],
        "b2055-multi-field-boxed-array-call",
    );

    // NO CALLEE AT ALL — the cell that says the missing free was never about
    // the by-value param path. Leaked the same 136 B per round before the fix.
    assert_clean_asan_run(
        "struct R { id: i64, s: String }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
         enum C2 { X(Array[R, 2], i64) }\n\
         fn mkarr(b: i64) -> Array[R, 2] {\n\
         \x20   return [R { id: b, s: f\"pay-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-{b}\" },\n\
         \x20           R { id: b + 1, s: f\"pay-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-{b}\" }];\n\
         }\n\
         fn eat2(w: C2) -> i64 { match w { C2.X(a, n) => { return a[0].id + n } } }\n\
         fn main() {\n\
         \x20   let mut i: i64 = 0i64;\n\
         \x20   while i < 3i64 {\n\
         \x20       { let v: C2 = C2.X(mkarr(i), 20); println(\"s\") }\n\
         \x20       i = i + 1i64;\n\
         \x20   }\n\
         \x20   println(\"end\");\n\
         }\n",
        &[
            "dR0", "dR1", "s", "dR1", "dR2", "s", "dR2", "dR3", "s", "end",
        ],
        "b2055-multi-field-boxed-array-no-call",
    );
}

/// B-2026-09-23-26 — a by-value `Option[R]` / `Result[R, i64]` param whose
/// payload runs a user `Drop`, returned on some exits only: a tail `if`, a
/// `let`-bound `if`, a tail `match` with bare arms, and a `let`-bound `match`.
/// Before the fix the `let`-bound spellings kept the caller's argument armed
/// while the local handed it back (a segfault on every compiled surface, the
/// body twice under `--interp`), and the tail spellings ran no body at all on
/// the exit where the value died inside the callee, on every surface.
#[test]
fn asan_conditional_optres_param_handback_runs_one_body() {
    assert_clean_asan_run(
        r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"  d{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn tl(a: Option[R], c: bool) -> Option[R] { if c { a } else { None } }
fn lt(a: Option[R], c: bool) -> Option[R] { let r: Option[R] = if c { a } else { None }; println("  mid"); r }
fn mt(a: Option[R], c: bool) -> Option[R] { match c { true => a, false => None } }
fn ml(a: Option[R], c: bool) -> Option[R] { let r: Option[R] = match c { true => a, false => None }; println("  mid"); r }
fn rs(a: Result[R, i64], c: bool) -> Result[R, i64] { let r: Result[R, i64] = if c { a } else { Err(5) }; r }
fn show(o: Option[R]) { match o { Some(x) => println(f"  y{x.id}"), None => println("  none") } }
fn main() {
    for c in [true, false] {
        println(f"c={c}");
        { let a = Some(mkr(1)); let b = tl(a, c); show(b); }
        { let a = Some(mkr(2)); let b = lt(a, c); show(b); }
        { let a = Some(mkr(3)); let b = mt(a, c); show(b); }
        { let a = Some(mkr(4)); let b = ml(a, c); show(b); }
        { let a: Result[R, i64] = Ok(mkr(5)); let b = rs(a, c); match b { Ok(x) => println(f"  y{x.id}"), Err(e) => println(f"  e{e}") } }
    }
    { let b = lt(Some(mkr(9)), true); show(b); }
    println("end")
}
"#,
        &[
            "c=true", "  y1", "  d1", "  mid", "  y2", "  d2", "  y3", "  d3", "  mid", "  y4",
            "  d4", "  y5", "  d5", "c=false", "  d1", "  none", "  mid", "  d2", "  none", "  d3",
            "  none", "  mid", "  d4", "  none", "  d5", "  e5", "  mid", "  y9", "  d9", "end",
        ],
        "asan_conditional_optres_param_handback_runs_one_body",
    );
}

/// B-2026-09-23-42 — the ASSOCIATED (`H.sf(a, c)`) and METHOD (`h.mf(a, c)`)
/// spellings of B-2026-09-23-26's conditional `Option` / `Result` hand-back.
/// The let site's passthrough skip matched a bare-identifier callee only, so
/// the result binding registered its own drop of the box the argument's
/// binding still owned: `double free` on every compiled surface at `c = true`,
/// and at `c = false` the method spelling lost the body the free spelling ran.
/// A no-`Drop` payload (`h.nf`) double freed the same way.
#[test]
fn asan_conditional_optres_param_handback_assoc_and_method() {
    assert_clean_asan_run(
        r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct N { id: i64, s: String }
fn mkr(i: i64) -> R { return R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn mkn(i: i64) -> N { return N { id: i, s: f"heap-string-longer-than-sso-{i}" } }
struct H { k: i64 }
impl H {
    fn sf(a: Option[R], c: bool) -> Option[R] { if c { a } else { None } }
    fn mf(ref self, a: Option[R], c: bool) -> Option[R] { if c { a } else { None } }
    fn rf(a: Result[R, String], c: bool) -> Result[R, String] { if c { a } else { Err("no") } }
    fn nf(ref self, a: Option[N], c: bool) -> Option[N] { if c { a } else { None } }
}
fn show(o: Option[R]) { match o { Some(x) => println(f"y{x.id}"), None => println("none") } }
fn run(c: bool, base: i64) {
    let h = H { k: 1 };
    let a = Some(mkr(base + 1));
    let b = H.sf(a, c);
    show(b);
    let a2 = Some(mkr(base + 2));
    let b2 = h.mf(a2, c);
    show(b2);
    let a3: Result[R, String] = Ok(mkr(base + 3));
    let b3 = H.rf(a3, c);
    match b3 { Ok(x) => println(f"y{x.id}"), Err(e) => println(e) }
    let a4 = Some(mkn(base + 4));
    let b4 = h.nf(a4, c);
    match b4 { Some(x) => println(f"n{x.id} {x.s}"), None => println("none") }
}
fn main() { run(true, 0); run(false, 10); println("end") }
"#,
        &[
            "y1",
            "d1",
            "y2",
            "d2",
            "y3",
            "d3",
            "n4 heap-string-longer-than-sso-4",
            "d11",
            "none",
            "d12",
            "none",
            "d13",
            "no",
            "none",
            "end",
        ],
        "asan_conditional_optres_param_handback_assoc_and_method",
    );
}

/// B-2026-09-24-3 — an `Option` / `Result` handed through a passthrough
/// callee TWICE (`let b = f(a); let e = f(b)`, `f(f(a))`, a method or
/// associated hop in the chain, or a discarded second hop). The let site
/// skips a one-hop result's registration and leaves the source sole owner,
/// but recorded the alias only for the population a callee takes over, so the
/// second hop found `b` neither armed nor aliased and `e` registered its own
/// drop of `a`'s box: a segfault on every compiled surface for a struct
/// payload with or without `Drop`, and a double free for `Option[String]` and
/// `Result[String, _]`, while `--interp` was right.
#[test]
fn asan_chained_optres_passthrough_owns_once() {
    assert_clean_asan_run(
        r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct N { id: i64, s: String }
fn mkr(i: i64) -> R { return R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn mkn(i: i64) -> N { return N { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn f(a: Option[R]) -> Option[R] { a }
fn fc(a: Option[R], c: bool) -> Option[R] { if c { a } else { None } }
fn fn_(a: Option[N]) -> Option[N] { a }
fn fs(a: Option[String]) -> Option[String] { a }
fn fr(a: Result[String, i64]) -> Result[String, i64] { a }
struct H { k: i64 }
impl H { fn g(ref self, a: Option[R]) -> Option[R] { a } fn s(a: Option[R]) -> Option[R] { a } }
fn eat(o: Option[R]) { match o { Some(x) => println(f"eat{x.id}"), None => println("none") } }
fn show(o: Option[R]) { match o { Some(x) => println(f"y{x.id}"), None => println("none") } }
fn main() {
    let h = H { k: 1 };
    { let a = Some(mkr(1)); let b = f(a); let e = f(b); show(e); }
    { let a = Some(mkr(2)); let e = f(f(a)); show(e); }
    { let a = Some(mkr(3)); let b = f(a); let c = h.g(b); let e = H.s(c); show(e); }
    { let a = Some(mkr(4)); let b = f(a); let c = f(b); eat(c); }
    { let a = Some(mkr(5)); let b = f(a); let _ = f(b); }
    { let a = Some(mkr(6)); let e = h.g(f(a)); eat(e); }
    { let a = Some(mkr(7)); let b = fc(a, true); let e = fc(b, true); show(e); }
    { let a = Some(mkr(8)); let b = fc(a, true); let e = fc(b, false); show(e); }
    { let a = Some(mkr(9)); let b = f(a); let e = f(b); println("kept"); }
    { let a = Some(mkn(10)); let b = fn_(a); let e = fn_(b); match e { Some(x) => println(f"n{x.id} {x.s}"), None => println("none") } }
    { let a = Some(f"heap-string-longer-than-sso-{11}"); let b = fs(a); let c = fs(b); let e = fs(fs(c)); match e { Some(x) => println(x), None => println("none") } }
    { let a: Result[String, i64] = Ok(f"heap-string-longer-than-sso-{12}"); let b = fr(a); let e = fr(b); match e { Ok(x) => println(x), Err(q) => println(f"e{q}") } }
    println("end")
}
"#,
        &[
            "y1",
            "d1",
            "y2",
            "d2",
            "y3",
            "d3",
            "eat4",
            "d4",
            "d5",
            "eat6",
            "d6",
            "y7",
            "d7",
            "d8",
            "none",
            "d9",
            "kept",
            "n10 heap-string-longer-than-sso-10",
            "heap-string-longer-than-sso-11",
            "heap-string-longer-than-sso-12",
            "end",
        ],
        "asan_chained_optres_passthrough_owns_once",
    );
}

/// B-2026-09-23-43 — the result of a call that hands an `Option` / `Result`
/// argument back (`id(a)`, `tl(a, c)`, `h.g(a)`) consumed directly rather than
/// bound: as a by-value argument (`show(id(a))`) or as a `match` scrutinee. The
/// let site treats such a result as an alias of the argument's binding, which
/// stays the sole owner of the box; these spellings took it for a manufactured
/// temp instead and registered a second box drop beside the source's, so every
/// compiled surface crashed, and a `let`-bound `Result` hand-back used as a
/// scrutinee lost its body. `if let` / `let ... else` over the same result,
/// and a consuming arm, keep exactly one body.
#[test]
fn asan_optres_handback_result_consumed_unbound() {
    assert_clean_asan_run(
        r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct N { id: i64, s: String }
fn mkr(i: i64) -> R { return R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn mkn(i: i64) -> N { return N { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn id(a: Option[R]) -> Option[R] { a }
fn tl(a: Option[R], c: bool) -> Option[R] { if c { a } else { None } }
fn rs(a: Result[R, i64], c: bool) -> Result[R, i64] { let r: Result[R, i64] = if c { a } else { Err(5) }; r }
fn idn(a: Option[N]) -> Option[N] { a }
fn eat(r: R) { println(f"eat{r.id}") }
fn show(o: Option[R]) { match o { Some(x) => println(f"y{x.id}"), None => println("none") } }
struct H { k: i64 }
impl H { fn g(ref self, a: Option[R]) -> Option[R] { a } }
fn main() {
    let h = H { k: 1 };
    for c in [true, false] {
        println(f"c={c}");
        { let a = Some(mkr(1)); show(tl(a, c)); }
        { let a = Some(mkr(2)); match tl(a, c) { Some(x) => println(f"y{x.id}"), None => println("none") } }
        { let a: Result[R, i64] = Ok(mkr(3)); match rs(a, c) { Ok(x) => println(f"y{x.id}"), Err(e) => println(f"e{e}") } }
    }
    { let a = Some(mkr(4)); show(id(a)); }
    { let a = Some(mkr(5)); show(id(id(a))); }
    { let a = Some(mkr(6)); match h.g(a) { Some(x) => println(f"y{x.id}"), None => println("none") } }
    { let a = Some(mkr(7)); match id(a) { Some(x) => eat(x), None => println("none") } }
    { let a = Some(mkr(8)); if let Some(x) = id(a) { println(f"y{x.id}") } }
    { let a = Some(mkr(9)); let Some(x) = id(a) else { return }; println(f"y{x.id}"); }
    { let a = Some(mkn(10)); match idn(a) { Some(x) => println(f"n{x.id}"), None => println("none") } }
    let mut i = 20;
    while i < 22 { let a = Some(mkr(i)); show(id(a)); i = i + 1; }
    println("end")
}
"#,
        &[
            "c=true", "y1", "d1", "y2", "d2", "y3", "d3", "c=false", "d1", "none", "d2", "none",
            "d3", "e5", "y4", "d4", "y5", "d5", "y6", "d6", "eat7", "d7", "y8", "d8", "y9", "d9",
            "n10", "y20", "d20", "y21", "d21", "end",
        ],
        "asan_optres_handback_result_consumed_unbound",
    );
}
