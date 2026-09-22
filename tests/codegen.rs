//! This file was split on 2026-09-21. The fixtures now live in per-area
//! files under `tests/codegen/`, because this file had grown into an
//! append target nobody could read, review or blame. The TEST TARGET is
//! unchanged -- `cargo test --features llvm --test codegen` still runs
//! every one of them, so CI needs no edit. A separate test target per
//! area was measured at ~118 MiB of link output each (~250 MiB at the
//! default debug level), which the session disk allowance cannot carry;
//! modules cost nothing.
//!
//! Run one area alone:  cargo test --features llvm --test codegen <area>::

//! Integration tests for Phase 7: LLVM code generation.
//!
//! Each test compiles a Kāra program snippet to LLVM IR and verifies:
//! - The IR can be generated without errors.
//! - Key IR patterns are present (function definitions, arithmetic, control flow, etc.).
//!
//! End-to-end execution tests (compile → link → run → compare output) are
//! gated on the host having `cc` available and are marked accordingly.
//!
//! **W3.3 (LLJIT bulk migration, 2026-05-29).** When built with the
//! `lljit_prototype` feature *and* run with `KARAC_TEST_JIT=1`, the
//! E2E `run_program_capturing_inner` dispatches through LLJIT (in-
//! process JIT compile + execute) instead of the AOT
//! object-and-spawn path. This lets the existing ~543 E2E tests
//! retarget at the JIT runtime via the same source; failures bucket
//! into known causes (missing runtime symbol in the force-link list,
//! missing `KARAC_*` global stand-in, stderr-dependent assertion).

mod common;

// ── LLJIT test-binary linkage scaffolding ─────────────────────────────
// In AOT builds, the runtime's `extern KARAC_SPAWN_SITES*` declarations
// (runtime/src/lib.rs L1059, `#[cfg(not(test))]`-gated) are satisfied by
// codegen-emitted globals in each user binary. In the JIT path, codegen
// emits those globals per-JITted-module (visible only inside that
// module's JITDylib), so the test binary's static link of the runtime
// rlib has no satisfier — we provide neutral stand-ins at the test
// binary level. JITted user code reads its own module-local defs;
// these stand-ins only satisfy the test binary's link, not user-
// program semantics.

#[cfg(feature = "llvm")]
#[no_mangle]
#[allow(non_upper_case_globals)]
pub static KARAC_SPAWN_SITES_ENABLED: u8 = 0;

#[cfg(feature = "llvm")]
#[no_mangle]
#[allow(non_upper_case_globals)]
pub static KARAC_SPAWN_SITES_LEN: u32 = 0;

#[cfg(feature = "llvm")]
#[no_mangle]
#[allow(non_upper_case_globals)]
pub static KARAC_SPAWN_SITES: KaracSpawnSitesPad = KaracSpawnSitesPad([0; 4]);

#[cfg(feature = "llvm")]
#[repr(C, align(8))]
pub struct KaracSpawnSitesPad([u64; 4]);

#[cfg(feature = "llvm")]
unsafe impl Sync for KaracSpawnSitesPad {}

#[cfg(feature = "llvm")]
#[used]
static _FORCE_LINK_CALL_SITE: fn() -> usize = force_link_karac_runtime;

#[cfg(feature = "llvm")]
fn force_link_karac_runtime() -> usize {
    karac_runtime::__preserve_no_mangle_symbols()
}

#[cfg(feature = "llvm")]
#[path = "codegen/mod.rs"]
mod codegen_tests;

#[cfg(feature = "llvm")]
mod ref_return_receiver_single_call {
    /// Emit a program's IR. The ownership pass is wired in because the
    /// return-position defensive copy these tests are about is only emitted
    /// with it — `compile_to_ir(_, None, _)` silently skips it.
    fn module_ir(src: &str) -> String {
        let mut parsed = karac::parse(src);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let ownership = karac::ownershipcheck(&parsed.program, &typed);
        karac::codegen::compile_to_ir(&parsed.program, Some(&ownership), None).expect("codegen")
    }

    fn count_calls(ir: &str, name: &str) -> usize {
        ir.lines()
            .filter(|l| l.contains(&format!("@{name}(")) && l.contains(" call "))
            .count()
    }

    /// Count the `call`s to `name` in `main`'s body only.
    fn calls_to(src: &str, name: &str) -> usize {
        let ir = module_ir(src);
        let main = ir
            .split("define i32 @main")
            .nth(1)
            .unwrap_or(&ir)
            .split("\n}")
            .next()
            .unwrap_or("")
            .to_string();
        count_calls(&main, name)
    }

    /// Count the `call`s to `name` anywhere in the module.
    fn calls_to_anywhere(src: &str, name: &str) -> usize {
        count_calls(&module_ir(src), name)
    }

    #[test]
    fn tensor_iter_axis_for_loop_fuses_only_when_the_row_cannot_escape() {
        // B-2026-07-29-13. `for row in t.iter_axis(n)` used to materialize
        // every sub-tensor as a copy before the loop ran — 30 MB of fresh
        // allocation for a `[10000, 768]` corpus, of which the loop needs one
        // row at a time. The fused lowering gathers into ONE reused row
        // buffer; `t.fia.cond` is its loop header, so its presence in the IR
        // is the structural signal that fusion engaged.
        //
        // The guard is what makes reuse sound: a body that lets the row
        // OUTLIVE its iteration must keep the materializing path, or the next
        // gather overwrites a live value. Asserted in both directions here —
        // a whitelisted read fuses, a `push` of the row does not.
        let fusable = module_ir(
            "fn main() {\n\
                 let t: Tensor[f32, [4, 3]] = Tensor.zeros([4, 3]);\n\
                 let mut a: f32 = 0.0;\n\
                 for row in t.iter_axis(0) { a = a + row.sum(); }\n\
                 println(a.to_string());\n\
             }",
        );
        assert!(
            fusable.contains("t.fia.cond"),
            "a read-only row use must fuse; IR:\n{fusable}"
        );

        let escaping = module_ir(
            "fn main() {\n\
                 let t: Tensor[f32, [4, 3]] = Tensor.zeros([4, 3]);\n\
                 let mut keep: Vec[Tensor[f32, [3]]] = Vec.new();\n\
                 for row in t.iter_axis(0) { keep.push(row); }\n\
                 println(keep.len().to_string());\n\
             }",
        );
        assert!(
            !escaping.contains("t.fia.cond"),
            "a row pushed into a Vec outlives its iteration and must NOT fuse; \
             IR:\n{escaping}"
        );

        // B-2026-07-29-24: the BOUND spelling reaches the same lowering,
        // because `lowering::inline_single_use_iter_axis_lets` collapses a
        // single-use `let` that is immediately followed by the loop.
        let bound = module_ir(
            "fn main() {\n\
                 let t: Tensor[f32, [4, 3]] = Tensor.zeros([4, 3]);\n\
                 let rows = t.iter_axis(0);\n\
                 let mut a: f32 = 0.0;\n\
                 for row in rows { a = a + row.sum(); }\n\
                 println(a.to_string());\n\
             }",
        );
        assert!(
            bound.contains("t.fia.cond"),
            "the bound spelling must collapse into the direct one and fuse; \
             IR:\n{bound}"
        );

        // …but only when the binding is used ONCE. A second loop over it
        // means the Vec is genuinely needed, so the collapse must not fire.
        let bound_twice = module_ir(
            "fn main() {\n\
                 let t: Tensor[f32, [4, 3]] = Tensor.zeros([4, 3]);\n\
                 let rows = t.iter_axis(0);\n\
                 let mut a: f32 = 0.0;\n\
                 for row in rows { a = a + row.sum(); }\n\
                 for row in rows { a = a + row.sum(); }\n\
                 println(a.to_string());\n\
             }",
        );
        assert!(
            !bound_twice.contains("t.fia.cond"),
            "a binding iterated twice must keep the materialized Vec; \
             IR:\n{bound_twice}"
        );
    }

    #[test]
    fn ref_returning_field_accessor_emits_no_dead_clone() {
        // B-2026-07-29-21: `fn label(ref self) -> ref String { self.name }`
        // ran the borrowed-receiver field-return defensive copy at its tail,
        // then threw the result away and returned the borrow pointer:
        //
        //     call void @karac_clone_String(ptr %retfld.clone.src, ptr %..dst)
        //     %retfld.cloned = load { ptr, i64, i64 }, ptr %..dst   ; unused
        //     ret ptr %ret_borrow_name
        //
        // One malloc'd copy per call that nothing owns and nothing frees —
        // a 1000-iteration loop leaked 1000 blocks. The `ref Vec[T]` sibling
        // emitted the same dead clone; LLVM DCEs its pure-IR malloc helper,
        // so only the String leg (which tail-calls the opaque runtime
        // `karac_string_clone`) showed up under valgrind. Both are asserted
        // here so the Vec leg cannot regress into a leak if that helper ever
        // grows an opaque call.
        assert_eq!(
            calls_to_anywhere(
                "struct H { name: String }\n\
                 impl H { fn label(ref self) -> ref String { self.name } }\n\
                 fn main() {\n\
                     let h = H { name: \"kara\".to_string() };\n\
                     let a = h.label();\n\
                     println(a.len().to_string());\n\
                 }",
                "karac_clone_String",
            ),
            0,
            "a `-> ref String` accessor must not clone the field it borrows"
        );
        assert_eq!(
            calls_to_anywhere(
                "struct H { items: Vec[i64] }\n\
                 impl H { fn view(ref self) -> ref Vec[i64] { self.items } }\n\
                 fn main() {\n\
                     let mut v: Vec[i64] = Vec.new();\n\
                     v.push(1);\n\
                     let h = H { items: v };\n\
                     let a = h.view();\n\
                     println(a.len().to_string());\n\
                 }",
                "karac_clone_Vec_i64",
            ),
            0,
            "a `-> ref Vec[T]` accessor must not clone the field it borrows"
        );
        // The gate is on RETURN position only. An ARGUMENT-position read of
        // the same borrowed field inside the same `-> ref String` function
        // still needs its copy — without it the push'd element and the
        // receiver free one buffer. Asserted as a live clone call.
        assert!(
            calls_to_anywhere(
                "struct H { name: String }\n\
                 impl H {\n\
                     fn label(ref self) -> ref String {\n\
                         let mut v: Vec[String] = Vec.new();\n\
                         v.push(self.name);\n\
                         println(v.len().to_string());\n\
                         self.name\n\
                     }\n\
                 }\n\
                 fn main() {\n\
                     let h = H { name: \"kara\".to_string() };\n\
                     println(h.label().len().to_string());\n\
                 }",
                "karac_clone_String",
            ) > 0,
            "an argument-position borrowed-field read must still be copied"
        );
    }

    #[test]
    fn ref_returning_accessor_receiver_is_called_exactly_once() {
        // B-2026-07-29-15: `h.view().is_empty()` emitted the accessor TWICE —
        //
        //     %usermethod  = call ptr @H.view(ptr %h)   ; discarded
        //     %usermethod1 = call ptr @H.view(ptr %h)
        //     store ptr %usermethod1, ptr %__refrecv_tmp_0
        //
        // because the materialize-the-borrow helper sat *after* an arm that
        // already compiled the receiver and fell through, so the helper's own
        // `compile_expr(object)` produced a second call. Pure for these two
        // accessors, but wrong for any accessor with an effect, and for one
        // that allocates the discarded result is leaked outright. Hoisting the
        // helper ahead of every receiver-compiling arm is the fix; this test
        // pins the property that made the defect invisible.
        assert_eq!(
            calls_to(
                "struct H { items: Vec[i64] }\n\
                 impl H { fn view(ref self) -> ref Vec[i64] { self.items } }\n\
                 fn main() {\n\
                     let mut v: Vec[i64] = Vec.new();\n\
                     v.push(3);\n\
                     let h = H { items: v };\n\
                     println(h.view().len().to_string());\n\
                 }",
                "H.view",
            ),
            1,
            "the Vec accessor must be emitted once per source call site"
        );
        assert_eq!(
            calls_to(
                "struct H { name: String }\n\
                 impl H { fn label(ref self) -> ref String { self.name } }\n\
                 fn main() {\n\
                     let h = H { name: \"kara\".to_string() };\n\
                     println(h.label().len().to_string());\n\
                 }",
                "H.label",
            ),
            1,
            "the String accessor must be emitted once per source call site"
        );
    }
}

#[cfg(feature = "llvm")]
mod string_append_in_place {
    //! B-2026-08-14-23 — `s = s + x` and `s += x` extend the left buffer in
    //! place instead of allocating a fresh concatenation of both operands.
    //!
    //! The measurement that identified the bug is that APPEND AND PREPEND COST
    //! THE SAME (25 ms vs 24 ms on 2,000 x 400 appends), since nobody can reuse
    //! a buffer when prepending. A wall-clock assertion would be flaky in CI, so
    //! these pin the STRUCTURAL fact the timing was evidence for: the append
    //! shapes emit `push_str`'s grow-and-copy and no longer call `String.add`,
    //! while every declined shape still does.

    fn module_ir(src: &str) -> String {
        let mut parsed = karac::parse(src);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let ownership = karac::ownershipcheck(&parsed.program, &typed);
        karac::codegen::compile_to_ir(&parsed.program, Some(&ownership), None).expect("codegen")
    }

    /// `pstr.grow` is the label prefix `push_str` gives its growth block, so it
    /// is present exactly when the in-place path was taken.
    fn appends_in_place(body: &str) -> bool {
        module_ir(&format!("fn main() {{ {body} }}")).contains("pstr.grow")
    }

    #[test]
    fn both_append_spellings_reach_the_in_place_path() {
        // The plain and compound spellings of the same append. `s += x` IS
        // `s = s + x`, so a fix that got only one of them would leave the more
        // common spelling quadratic.
        assert!(
            appends_in_place("let mut s = \"\"; s = s + \"abc\"; println(s);"),
            "`s = s + x` must append in place"
        );
        assert!(
            appends_in_place("let mut s = \"\"; s += \"abc\"; println(s);"),
            "`s += x` must append in place"
        );
        // A chain appends each operand in turn.
        assert!(
            appends_in_place("let mut s = \"\"; let t = \"b\"; s = s + \" \" + t; println(s);"),
            "`s = s + a + b` must append in place"
        );
        // A single call-valued operand is admitted; only a multi-operand chain
        // needs its trailing operands to be call-free.
        assert!(
            appends_in_place("let mut s = \"\"; let n = 3i64; s = s + n.to_string(); println(s);"),
            "a single call-valued operand must append in place"
        );
    }

    /// THE DECLINES ARE THE LOAD-BEARING HALF. `push_str`'s grow path reallocs
    /// the destination and copies from a source pointer captured before the
    /// grow, so an aliasing source is a use-after-free (B-2026-08-15-2 —
    /// `s.push_str(s)` is exactly that). `s = s + s` is CORRECT today because it
    /// concatenates through a fresh buffer; routing it here would trade a slow
    /// program for a broken one. Prepending has no left buffer to extend at all.
    #[test]
    fn aliasing_and_prepend_shapes_decline_the_in_place_path() {
        assert!(
            !appends_in_place("let mut s = \"abc\"; s = s + s; println(s);"),
            "a self-append must NOT reach `push_str` — see B-2026-08-15-2"
        );
        assert!(
            !appends_in_place("let mut s = \"abc\"; s += s; println(s);"),
            "a compound self-append must NOT reach `push_str` either"
        );
        assert!(
            !appends_in_place("let mut s = \"abc\"; s = s + s[0..2]; println(s);"),
            "a slice OF the target aliases its buffer and must decline"
        );
        assert!(
            !appends_in_place("let mut s = \"abc\"; s = \"X\" + s; println(s);"),
            "a prepend has no left buffer to extend"
        );
        assert!(
            !appends_in_place("let mut s = \"abc\"; let t = \"q\"; s = t + \"z\"; println(s);"),
            "a concatenation not rooted at the target must decline"
        );
    }
}

/// B-2026-08-15-3 — loop-bound pre-sizing recognizes the two ASSIGNMENT
/// spellings of an append, not just the `push`/`push_str` method call.
///
/// The three spellings compile to the same in-place append (B-2026-08-14-23),
/// so recognizing only the method call left `src/presize.rs` disagreeing with
/// itself: an accumulator filled by `s = s + x` or `s += x` started at capacity
/// 0 and re-grew 8 -> 16 -> ... on every round. Measured 5.8 ms vs 2.7 ms on
/// 2,000 rounds of 400 appends, entirely from the missing reservation.
///
/// THESE RUN THE REAL PIPELINE ON PURPOSE. `presize.rs`'s own unit tests call
/// `presize_block` on a freshly parsed body, which is pre-lowering by
/// construction — so they cannot see the trap B-2026-08-14-23 documented, where
/// a recognizer that matches only `Binary` is silently dead because lowering
/// has already rewritten `s + x` into a `String.add` call. `karac::lower` is
/// what orders `presize_block` before the operator rewrite; these assert
/// through it, so a reordering fails here rather than quietly costing 2x.
#[cfg(feature = "llvm")]
mod presize_reservation {
    /// THE PARSE AND TYPECHECK ASSERTIONS ARE WHAT KEEP THE DECLINE TEST
    /// HONEST. Its four probes assert that `str_with_cap.buf` is ABSENT — which
    /// a source that never compiled would satisfy just as well, so a typo'd
    /// probe would read as a deliberate decline forever. Nothing here needs to
    /// survive a broken probe, so refusing one costs nothing.
    fn module_ir(src: &str) -> String {
        let mut parsed = karac::parse(src);
        assert!(
            parsed.errors.is_empty(),
            "probe does not parse: {:?}",
            parsed.errors
        );
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        assert!(
            typed.errors.is_empty(),
            "probe does not typecheck: {:?}",
            typed.errors
        );
        karac::lower(&mut parsed.program, &typed);
        let ownership = karac::ownershipcheck(&parsed.program, &typed);
        karac::codegen::compile_to_ir(&parsed.program, Some(&ownership), None).expect("codegen")
    }

    /// `str_with_cap.buf` is the name `String.with_capacity` gives its
    /// allocation, so it is present exactly when the accumulator was reserved
    /// up front rather than grown from zero.
    fn reserves(body: &str) -> bool {
        module_ir(&format!(
            "fn f(n: i64) {{ {body} }}\nfn main() {{ f(4i64); }}\n"
        ))
        .contains("str_with_cap.buf")
    }

    #[test]
    fn assign_append_spellings_reserve_up_front() {
        // The baseline the two spellings have to reach: the method call has
        // always been pre-sized.
        assert!(
            reserves("let mut s = \"\"; let mut i = 0i64; while i < n { s.push_str(\"x\"); i = i + 1i64; } println(s);"),
            "`s.push_str(x)` must reserve (the pre-existing behavior)"
        );
        assert!(
            reserves("let mut s = \"\"; let mut i = 0i64; while i < n { s = s + \"x\"; i = i + 1i64; } println(s);"),
            "`s = s + x` must reserve — it is the same append"
        );
        assert!(
            reserves("let mut s = \"\"; let mut i = 0i64; while i < n { s += \"x\"; i = i + 1i64; } println(s);"),
            "`s += x` must reserve — it is the same append"
        );
        assert!(
            reserves("let mut s = \"\"; for i in 0i64..n { s += \"x\"; } println(s);"),
            "a `for`-range fill in the operator spelling must reserve too"
        );
        // The seed prelude in its assignment spelling: `find_fill_bound` used to
        // bail on it (a non-`Expr` statement mentioning `s`) while folding in
        // the identical `s.push_str("seed")`.
        assert!(
            reserves("let mut s = \"\"; s = s + \"seed\"; let mut i = 0i64; while i < n { s += \"x\"; i = i + 1i64; } println(s);"),
            "an assignment-spelled seed append must be folded in, not bailed on"
        );
    }

    /// `with_cap.buf` is the name `Vec.with_capacity` gives its allocation —
    /// the Vec twin of `reserves`'s `str_with_cap.buf`.
    fn reserves_vec(body: &str) -> bool {
        module_ir(&format!("fn main() {{ {body} }}\n")).contains("with_cap.buf")
    }

    /// B-2026-08-20-3 — a `chars()` fill reserves `s.len()` up front, in both
    /// the hand-written and the fused-`collect()` spelling.
    ///
    /// THE `collect()` HALF IS THE ONE THAT NEEDED WIRING. Its block is
    /// synthesized at CODEGEN time by `compile_chars_collect_to_vec`, so it
    /// never passes through `lowering`, where `presize_block` normally runs —
    /// the pass had to be invoked on the synthesized block explicitly. That
    /// makes this test the only thing standing between the two spellings and a
    /// silent 2.5x divergence, since they compile through different modules.
    ///
    /// Why it matters beyond one allocation: every `realloc` in the grow chain
    /// takes the glibc arena lock, which under auto-par is shared by all
    /// fan-out workers. Before the reservation a 1M-round `chars().collect()`
    /// loop measured 0.08s sequential / 0.30s parallel — a 3.75x
    /// PESSIMIZATION from fanning out. After: 0.03s / 0.01s.
    #[test]
    fn chars_fill_reserves_up_front() {
        assert!(
            reserves_vec(
                "let s: String = \"hello\"; let mut v: Vec[char] = Vec.new(); \
                 for c in s.chars() { v.push(c); } println(v.len());"
            ),
            "a hand-written chars() fill must reserve s.len()"
        );
        assert!(
            reserves_vec(
                "let s: String = \"hello\"; let v: Vec[char] = s.chars().collect(); \
                 println(v.len());"
            ),
            "the fused chars().collect() must reserve too — same loop, \
             synthesized at codegen time"
        );
        assert!(
            reserves_vec(
                "let s: String = \"hello\"; let mut v: Vec[u8] = Vec.new(); \
                 for b in s.bytes() { v.push(b); } println(v.len());"
            ),
            "a bytes() fill must reserve s.len()"
        );
    }

    /// The other side of that boundary. The collection-capacity-presizing spike
    /// measured accumulator pre-sizing on 2026-07-09 and declined it for
    /// HEAP-element sources (`Vec[String].iter()...collect()` at 0.72x), so
    /// `iterable_len_bound` matches `chars`/`bytes` and nothing else. A
    /// `for w in xs.iter()` fill is syntactically one method name away from the
    /// chars fill above, which is exactly why it is asserted here rather than
    /// left to the reader.
    #[test]
    fn iter_fill_still_declines_per_the_spike() {
        assert!(
            !reserves_vec(
                "let xs: Vec[String] = Vec[\"a\".to_string(), \"bb\".to_string()]; \
                 let mut v: Vec[i64] = Vec.new(); \
                 for w in xs.iter() { v.push(w.len()); } println(v.len());"
            ),
            "iter() over a heap source is the declined shape — see \
             docs/spikes/collection-capacity-presizing.md"
        );
    }

    /// The declines. Pre-sizing is only a capacity hint, so none of these could
    /// change an answer — but a heuristic that fires on shapes it cannot
    /// account for stops meaning anything, and these are the shapes whose final
    /// length is NOT the trip count.
    #[test]
    fn non_append_assignment_shapes_still_decline() {
        assert!(
            !reserves("let mut s = \"\"; let mut i = 0i64; while i < n { s = \"x\" + s; i = i + 1i64; } println(s);"),
            "a prepend is not the append this reserves for"
        );
        assert!(
            !reserves("let mut s = \"\"; let t = \"q\"; let mut i = 0i64; while i < n { s = t + \"z\"; i = i + 1i64; } println(s);"),
            "an assignment not rooted at the target REPLACES it — nothing accumulates"
        );
        assert!(
            !reserves("let mut s = \"\"; let mut i = 0i64; while i < n { if i > 0i64 { s += \"x\"; } i = i + 1i64; } println(s);"),
            "a conditional append is <= the trip count, not equal to it"
        );
        assert!(
            !reserves("let mut s = \"\"; let mut i = 0i64; while i < n { s += \"x\"; s = s + \"y\"; i = i + 1i64; } println(s);"),
            "two appends per iteration is not one per iteration"
        );
    }
}

/// B-2026-08-30-27 — the two `float_math` lowering families must be declared
/// equally optimizable.
///
/// Which family a method lands in is an accident of the LLVM-18 pin: `log10`
/// has an intrinsic, `cosh` does not and is emitted as a direct libm call. That
/// made no semantic difference and a large optimization one. `llvm.log10.f32`
/// is declared `memory(none) speculatable willreturn nounwind`, so LICM lifts a
/// loop-invariant call out of the loop; the bare `coshf` declaration karac
/// created carried nothing, and LLVM's own library-attribute inference is
/// errno-conservative enough (`memory(read)`) that the call stayed put.
/// Measured on the same 1000-iteration loop: `log10f` was hoisted above it and
/// `coshf` was called every iteration, spilling the accumulator each time.
///
/// This asserts the DECLARATION rather than the hoist, deliberately. The
/// attributes are the fix; whether a particular loop gets hoisted is LLVM's
/// decision and would make the test a hostage to pipeline changes.
/// `compile_to_ir` emits pre-optimization IR, which is exactly where the
/// declaration is visible.
///
/// The intrinsic half is asserted alongside as the contrast that gives the
/// test its meaning: if a future change dropped the attributes from BOTH, an
/// assertion on `coshf` alone would still be satisfiable by making the two
/// consistently pessimal.
#[cfg(feature = "llvm")]
mod libm_math_declaration_attrs {
    fn module_ir(src: &str) -> String {
        let mut parsed = karac::parse(src);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let ownership = karac::ownershipcheck(&parsed.program, &typed);
        karac::codegen::compile_to_ir(&parsed.program, Some(&ownership), None).expect("codegen")
    }

    /// The body of the attribute group attached to `sym`'s `declare` line.
    /// Resolved through the `#N` reference so the assertion does not depend on
    /// which group number LLVM happened to assign.
    fn declaration_attrs(ir: &str, sym: &str) -> String {
        let line = ir
            .lines()
            .find(|l| l.starts_with("declare") && l.contains(&format!("@{sym}(")))
            .unwrap_or_else(|| panic!("no declaration of `{sym}` in the emitted IR:\n{ir}"));
        assert!(
            line.contains('#'),
            "`{sym}` is declared with NO attribute group at all — that is the \
             B-2026-08-30-27 state:\n{line}"
        );
        let group = line.rsplit('#').next().unwrap().trim().to_string();
        let prefix = format!("attributes #{group} = ");
        ir.lines()
            .find(|l| l.starts_with(&prefix))
            .unwrap_or_else(|| panic!("no `{prefix}` line in IR for `{sym}`:\n{ir}"))
            .to_string()
    }

    #[test]
    fn direct_libm_calls_are_declared_as_optimizable_as_the_intrinsics() {
        let ir = module_ir(
            "fn main() {\n\
                 let n = env.args().len() as i64;\n\
                 let x: f32 = (n as f32);\n\
                 println(f\"{x.cosh()} {x.log10()}\");\n\
             }",
        );

        // The direct-libm arm — the one this bug is about.
        let cosh = declaration_attrs(&ir, "coshf");
        for want in ["memory(none)", "nounwind", "willreturn"] {
            assert!(
                cosh.contains(want),
                "`coshf` declaration is missing `{want}`, so LICM cannot hoist a \
                 loop-invariant call:\n{cosh}"
            );
        }

        // The intrinsic arm — the contrast the fix is measured against.
        let log10 = declaration_attrs(&ir, "llvm.log10.f32");
        assert!(
            log10.contains("memory(none)"),
            "the intrinsic arm lost `memory(none)`, so the comparison this test \
             draws is no longer meaningful:\n{log10}"
        );
    }
}
