//! control flow, loops, bindings, assignment, scopes -- fixtures for `tests/interpreter.rs`.
//!
//! Split out of `tests/interpreter.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test interpreter` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test interpreter control::
//!
//! New fixtures about control flow, loops, bindings, assignment, scopes belong in this file.

use super::*;

#[test]
fn fn_value_returned_then_called() {
    assert_eq!(
        run("fn doubler(n: i64) -> i64 { n * 2 }\n\
             fn pick() -> Fn(i64) -> i64 { doubler }\n\
             fn main() { let f = pick(); println(f(21)); }\n"),
        "42\n"
    );
}

// ── Same-scope `let` shadowing (design.md § Variables > Shadowing) ──

#[test]
fn test_same_scope_let_shadowing_value() {
    // The shadowing initializer reads the previous binding; the new binding
    // wins for later uses.
    assert_eq!(
        run_no_errors("fn main() { let x = 5; let x = x + 1; println(x); }"),
        "6\n"
    );
}

#[test]
fn test_same_scope_let_shadowing_changes_type() {
    // Canonical type-changing shadow: String rebound to its length (i64).
    assert_eq!(
        run_no_errors("fn main() { let s = \"hello\"; let s = s.len(); println(s); }"),
        "5\n"
    );
}

#[test]
fn test_same_scope_let_mut_shadowing_value() {
    // `let mut` shadowing is permitted regardless of the prior mutability.
    assert_eq!(
        run_no_errors("fn main() { let mut y = 10; let y = y * 2; println(y); }"),
        "20\n"
    );
}

#[test]
fn a_user_type_shadowing_a_prelude_name_keeps_its_own_comparison() {
    // B-2026-08-27-11. The total-order float wrapper machinery was keyed on
    // the bare NAME, so a user `struct F64 { .. }` inherited the wrapper's
    // comparison — which reads ONE float field and stops. Two values whose
    // second field differed compared EQUAL on the compiled backends: a
    // FALSE-POSITIVE equality, worse than a false negative because a dedup, a
    // cache hit, or a guard clause then takes the wrong branch instead of
    // merely doing redundant work.
    //
    // design.md § Module System is what makes this the user's type to define:
    // "Users can shadow prelude names in their own code — the language does
    // not reserve them." The existing shadowing test covers LAYOUT, which was
    // already correct; comparison was the half that was captured.
    //
    // Both struct kinds, because the shadow was recorded for PLAIN structs
    // only — a `shared struct F64` was never marked at all, so every consumer
    // of `user_shadowed_prelude_types` went on treating it as the stdlib type.
    // Codegen twin: `test_e2e_shadowed_prelude_name_keeps_its_own_comparison`.
    let src = "#[derive(PartialEq)]
        struct F64 { x: i64, tag: i64 }
        #[derive(PartialEq)]
        shared struct F32 { x: i64, tag: i64 }
        #[derive(Eq, PartialEq)]
        struct F16 { x: i64, tag: i64 }
        fn main() {
            let a = F64 { x: 1, tag: 1 };
            let b = F64 { x: 1, tag: 2 };
            let c = F64 { x: 1, tag: 1 };
            println(f\"plain-ne={a == b}\");
            println(f\"plain-eq={a == c}\");

            let d = F32 { x: 1, tag: 1 };
            let e = F32 { x: 1, tag: 2 };
            println(f\"shared-ne={d == e}\");

            let g = F16 { x: 1, tag: 1 };
            let h = F16 { x: 1, tag: 2 };
            println(f\"derived-ne={g == h}\");

            println(f\"fields={a.x} {a.tag}\");
        }";
    assert_eq!(
        run_no_errors(src),
        "plain-ne=false\nplain-eq=true\nshared-ne=false\n\
         derived-ne=false\nfields=1 1\n"
    );
}

// ── Variables & Let Bindings ───────────────────────────────────

#[test]
fn test_let_binding() {
    assert_eq!(run("fn main() { let x = 42; println(x); }"), "42\n");
}

#[test]
fn test_let_mut_reassign() {
    assert_eq!(
        run("fn main() { let mut x = 1; x = 2; println(x); }"),
        "2\n"
    );
}

#[test]
fn test_compound_assignment() {
    assert_eq!(
        run("fn main() { let mut x = 10; x += 5; println(x); }"),
        "15\n"
    );
}

// ── Control Flow ───────────────────────────────────────────────

#[test]
fn test_if_true() {
    assert_eq!(run("fn main() { if true { println(1); } }"), "1\n");
}

#[test]
fn test_if_false_else() {
    assert_eq!(
        run("fn main() { if false { println(1); } else { println(2); } }"),
        "2\n"
    );
}

#[test]
fn test_if_else_expression() {
    assert_eq!(
        run("fn main() { let x = if true { 10 } else { 20 }; println(x); }"),
        "10\n"
    );
}

#[test]
fn test_while_loop() {
    assert_eq!(
        run("fn main() {\n\
                 let mut i = 0;\n\
                 while i < 3 {\n\
                     i += 1;\n\
                 }\n\
                 println(i);\n\
             }"),
        "3\n"
    );
}

#[test]
fn test_loop_break() {
    assert_eq!(
        run("fn main() {\n\
                 let mut i = 0;\n\
                 loop {\n\
                     i += 1;\n\
                     if i == 5 { break; }\n\
                 }\n\
                 println(i);\n\
             }"),
        "5\n"
    );
}

/// The same program written the way design.md and syntax.md write it — with the
/// trailing `;` after the else block. It used to be a parse error while the
/// spelling above compiled (B-2026-08-21-12); both are accepted now and must
/// behave identically.
#[test]
fn test_let_else_with_the_grammars_trailing_semicolon_behaves_the_same() {
    assert_eq!(
        run("fn make(empty: bool) -> Option[i64] {\n\
                 if empty { return Option.None; }\n\
                 return Option.Some(7_i64);\n\
             }\n\
             fn check(empty: bool) {\n\
                 let Some(x) = make(empty) else {\n\
                     println(0_i64);\n\
                     return;\n\
                 };\n\
                 println(x);\n\
             }\n\
             fn main() {\n\
                 check(false);\n\
                 check(true);\n\
                 println(99_i64);\n\
             }"),
        "7\n0\n99\n"
    );
}

#[test]
fn test_deep_recursion_grows_stack() {
    // Depth 5000 — LeetCode's linked-list bound (one frame per node at
    // k = 1 in kata #25, which found this). At ~8 Rust frames per Kāra
    // call this blows any fixed thread stack (16 MB included) unless
    // `eval_body_growing` re-homes the recursion onto heap segments via
    // `stacker::maybe_grow`. Regression: this aborted with a stack
    // overflow before the helper existed.
    assert_eq!(
        run("fn countdown(n: i64) -> i64 {\n\
                 if n <= 0 { return 0; }\n\
                 countdown(n - 1) + 1\n\
             }\n\
             fn main() { println(countdown(5000)); }"),
        "5000\n"
    );
}

#[test]
fn test_early_return() {
    assert_eq!(
        run("fn abs(x: i64) -> i64 {\n\
                 if x < 0 { return -x; }\n\
                 x\n\
             }\n\
             fn main() { println(abs(-5)); }"),
        "5\n"
    );
}

#[test]
fn test_bare_literal_for_loop_sum() {
    assert_eq!(
        run("fn main() {\n\
                 let v = [1, 2, 3, 4];\n\
                 let mut s = 0;\n\
                 for x in v { s = s + x; }\n\
                 println(s);\n\
             }"),
        "10\n"
    );
}

#[test]
fn test_function_scope_isolation() {
    // Variables defined in one function call should not leak to the next
    assert_eq!(
        run("fn f() -> i64 {\n\
                 let local = 42;\n\
                 local\n\
             }\n\
             fn main() {\n\
                 println(f());\n\
                 println(f());\n\
             }"),
        "42\n42\n"
    );
}

// ── Edge Cases: Recursion & Control Flow ───────────────────────

#[test]
fn test_mutual_recursion() {
    assert_eq!(
        run("fn is_even(n: i64) -> bool {\n\
                 if n == 0 { true } else { is_odd(n - 1) }\n\
             }\n\
             fn is_odd(n: i64) -> bool {\n\
                 if n == 0 { false } else { is_even(n - 1) }\n\
             }\n\
             fn main() {\n\
                 println(is_even(4));\n\
                 println(is_odd(5));\n\
             }"),
        "true\ntrue\n"
    );
}

#[test]
fn test_break_with_variable_value() {
    assert_eq!(
        run("fn main() {\n\
                 let mut i = 0;\n\
                 let x = loop {\n\
                     i += 1;\n\
                     if i == 3 {\n\
                         break i;\n\
                     }\n\
                 };\n\
                 println(x);\n\
             }"),
        "3\n"
    );
}

#[test]
fn test_while_with_return() {
    assert_eq!(
        run("fn count_to_return(limit: i64) -> i64 {\n\
                 let mut i = 0;\n\
                 while true {\n\
                     i += 1;\n\
                     if i >= limit { return i; }\n\
                 }\n\
                 0\n\
             }\n\
             fn main() { println(count_to_return(5)); }"),
        "5\n"
    );
}

// ── Edge Cases: Multiple Return Paths ──────────────────────────

#[test]
fn test_multiple_returns() {
    assert_eq!(
        run("fn sign(x: i64) -> i64 {\n\
                 if x > 0 { return 1; }\n\
                 if x < 0 { return -1; }\n\
                 0\n\
             }\n\
             fn main() {\n\
                 println(sign(42));\n\
                 println(sign(-7));\n\
                 println(sign(0));\n\
             }"),
        "1\n-1\n0\n"
    );
}

// ── Seq block ───────────────────────────────────────────────────

#[test]
fn test_seq_block_empty() {
    // seq {} evaluates to unit
    assert_eq!(run("fn main() { let x = seq { }; println(0); }"), "0\n");
}

#[test]
fn test_seq_block_value() {
    // seq { let x = 42; x } evaluates to 42
    assert_eq!(
        run("fn main() { let result = seq { let x = 42; x }; println(result); }"),
        "42\n"
    );
}

#[test]
fn test_seq_block_scoping() {
    // Variables inside seq are scoped to the block
    assert_eq!(
        run("fn main() {\n\
                 let a = 1;\n\
                 let b = seq { let inner = 10; inner + a };\n\
                 println(b);\n\
             }"),
        "11\n"
    );
}

// ── Labeled Loops ──────────────────────────────────────────────

#[test]
fn test_labeled_break_exits_outer_loop() {
    // `break label expr` syntax is required; `break label;` alone parses as break-with-value
    assert_eq!(
        run("fn main() {\n\
                 let mut count = 0;\n\
                 outer: for x in [1, 2, 3] {\n\
                     for y in [10, 20, 30] {\n\
                         count = count + 1;\n\
                         break outer ();\n\
                     }\n\
                 }\n\
                 println(count);\n\
             }"),
        "1\n"
    );
}

#[test]
fn test_labeled_continue_skips_outer_iteration() {
    assert_eq!(
        run("fn main() {\n\
                 let mut count = 0;\n\
                 outer: for x in [1, 2, 3] {\n\
                     for y in [10, 20] {\n\
                         count = count + 1;\n\
                         continue outer;\n\
                     }\n\
                 }\n\
                 println(count);\n\
             }"),
        "3\n"
    );
}

#[test]
fn test_labeled_break_with_value() {
    assert_eq!(
        run("fn main() {\n\
                 let result = outer: loop {\n\
                     loop {\n\
                         break outer 42;\n\
                     }\n\
                 };\n\
                 println(result);\n\
             }"),
        "42\n"
    );
}

#[test]
fn test_labeled_break_while_loop() {
    assert_eq!(
        run("fn main() {\n\
                 let mut i = 0;\n\
                 outer: while i < 5 {\n\
                     let mut j = 0;\n\
                     while j < 5 {\n\
                         if j == 2 {\n\
                             break outer ();\n\
                         }\n\
                         j = j + 1;\n\
                     }\n\
                     i = i + 1;\n\
                 }\n\
                 println(i);\n\
             }"),
        "0\n"
    );
}

#[test]
fn test_unlabeled_break_still_works() {
    assert_eq!(
        run("fn main() {\n\
                 let mut count = 0;\n\
                 outer: for x in [1, 2, 3] {\n\
                     for y in [10, 20, 30] {\n\
                         count = count + 1;\n\
                         break;\n\
                     }\n\
                 }\n\
                 println(count);\n\
             }"),
        "3\n"
    );
}

// ── Labeled blocks runtime ───────────────────────────────────
//
// Sibling slice to `tests/codegen.rs` "Labeled blocks runtime" — the
// interpreter side of the LBC4 design choice. The labeled-block expr
// arm in `eval_expr_inner` matches `ControlFlow::Break { label, value
// }` against its own label; non-matching labels propagate. See
// `docs/implementation_checklist/phase-5-diagnostics.md` § 5.2.

#[test]
fn test_interpreter_labeled_block_break_with_value() {
    // Mirror of codegen test 1: `lbl: { break lbl 42; -1 }` evaluates
    // to 42 through the tree-walk path.
    assert_eq!(
        run("fn main() {\n\
             let x: i64 = lbl: { break lbl 42; -1 };\n\
             println(x);\n\
         }"),
        "42\n"
    );
}

#[test]
fn test_interpreter_labeled_block_bare_break_unit() {
    // Bare `break label` exits with `Value::Unit`. Observable check:
    // post-block println runs.
    assert_eq!(
        run("fn main() {\n\
             lbl: { break lbl; };\n\
             println(7);\n\
         }"),
        "7\n"
    );
}

#[test]
fn test_interpreter_labeled_block_tail_expression() {
    // No break: the block falls through normally and the tail value
    // becomes the labeled block's value.
    assert_eq!(
        run("fn main() {\n\
             let x: i64 = lbl: { 99 };\n\
             println(x);\n\
         }"),
        "99\n"
    );
}

// ── .into() expected-type threading (Slice 3a) ────────────────

#[test]
fn test_into_at_let_annotation_uses_from_impl() {
    // `let y: i64 = x.into()` should dispatch through the `i64.from(x)`
    // widening impl — same semantics as calling `i64.from(x)` directly.
    let output = run("fn main() { let x: i32 = 42; let y: i64 = x.into(); println(y); }");
    assert_eq!(output, "42\n");
}

#[test]
fn test_into_at_return_position() {
    let output = run("fn widen(x: i32) -> i64 { x.into() }\n\
         fn main() { println(widen(7)); }");
    assert_eq!(output, "7\n");
}

// ── Poison discipline on Assign/CompoundAssign/LetElse (B-2026-07-31-15) ──
//
// A control-flow signal escaping an expression (a `break` out of an
// enclosing loop through a `with_provider` closure body, or through a plain
// block expression) reaches the statement layer as `pending_cf` plus a
// poison Unit value. The `Assign` arm used to store the poison into the
// target BEFORE anyone checked the pending signal, so an i64 accumulator
// became Unit and the function returned `()` from an i64 signature (the
// codegen twins in tests/codegen.rs print the correct value). Same class:
// `CompoundAssign` fed the poison into `eval_binary`, and `LetElse` fell
// into the `else` block spuriously.

#[test]
fn test_break_through_with_provider_body_does_not_corrupt_assign_target() {
    // The B-2026-07-31-11 ledger repro shape: acc accumulates 0+1+2, the
    // closure breaks when read() == 3, and the fn must return 3 — not Unit.
    let output = run("trait Counter { fn get(ref self) -> i64; }
         effect resource Ctr: Counter;
         struct InMem { n: i64 }
         impl Counter for InMem { fn get(ref self) -> i64 { self.n } }
         fn read() -> i64 with reads(Ctr) { Ctr.get() }
         fn brk() -> i64 with reads(Ctr) {
             let mut acc = 0;
             for i in 0..5 {
                 acc = acc + with_provider[Ctr](InMem { n: i }, || {
                     if read() == 3 { break; }
                     read()
                 });
             }
             acc
         }
         fn main() with reads(Ctr) { println(f\"{brk()}\"); }");
    assert_eq!(output, "3\n");
}

#[test]
fn test_continue_through_with_provider_body_skips_iteration_only() {
    // `continue` is the sibling signal: skip i == 2's accumulation, keep the
    // loop running. Pre-fix the poison store corrupted acc to Unit and the
    // NEXT iteration died on `Unit + Int`.
    let output = run("trait Counter { fn get(ref self) -> i64; }
         effect resource Ctr: Counter;
         struct InMem { n: i64 }
         impl Counter for InMem { fn get(ref self) -> i64 { self.n } }
         fn read() -> i64 with reads(Ctr) { Ctr.get() }
         fn cont() -> i64 with reads(Ctr) {
             let mut acc = 0;
             for i in 0..5 {
                 acc = acc + with_provider[Ctr](InMem { n: i }, || {
                     if read() == 2 { continue; }
                     read()
                 });
             }
             acc
         }
         fn main() with reads(Ctr) { println(f\"{cont()}\"); }");
    assert_eq!(output, "8\n");
}

#[test]
fn test_break_through_provider_body_compound_assign_target_untouched() {
    // CompoundAssign leg: `acc += <poison>` must propagate the break without
    // running the `+` on the poison (which hit eval_binary's internal
    // unreachable) or writing the target.
    let output = run("trait Counter { fn get(ref self) -> i64; }
         effect resource Ctr: Counter;
         struct InMem { n: i64 }
         impl Counter for InMem { fn get(ref self) -> i64 { self.n } }
         fn read() -> i64 with reads(Ctr) { Ctr.get() }
         fn cmp() -> i64 with reads(Ctr) {
             let mut acc = 100;
             for i in 0..5 {
                 acc += with_provider[Ctr](InMem { n: i }, || {
                     if read() == 3 { break; }
                     read()
                 });
             }
             acc
         }
         fn main() with reads(Ctr) { println(f\"{cmp()}\"); }");
    assert_eq!(output, "103\n");
}

#[test]
fn test_break_through_provider_body_let_else_does_not_run_else() {
    // LetElse leg: the poison scrutinee used to MISS the pattern and run the
    // else block spuriously (adding 1000) instead of propagating the break.
    let output = run("trait Counter { fn get(ref self) -> i64; }
         effect resource Ctr: Counter;
         struct InMem { n: i64 }
         impl Counter for InMem { fn get(ref self) -> i64 { self.n } }
         fn read() -> i64 with reads(Ctr) { Ctr.get() }
         fn le() -> i64 with reads(Ctr) {
             let mut acc = 0;
             for i in 0..5 {
                 let Some(v) = with_provider[Ctr](InMem { n: i }, || {
                     if read() == 3 { break; }
                     Some(read())
                 }) else {
                     acc = acc + 1000;
                     continue
                 }
                 acc = acc + v;
             }
             acc
         }
         fn main() with reads(Ctr) { println(f\"{le()}\"); }");
    assert_eq!(output, "3\n");
}

#[test]
fn test_break_out_of_block_expr_assign_rhs_no_providers() {
    // The provider-free minimal shape of the same statement-level bug: a
    // `break` escaping a block expression in an Assign RHS. No closures, no
    // resources — just the poison-store ordering.
    let output = run("fn f() -> i64 {
             let mut acc = 0;
             for i in 0..5 {
                 acc = acc + { if i == 3 { break; } i };
             }
             acc
         }
         fn main() { println(f\"{f()}\"); }");
    assert_eq!(output, "3\n");
}

// ── `providers { R => p, ... } in { body }` block ───────────────

#[test]
fn test_providers_block_binds_each_resource() {
    let output = run("effect resource UserDB;
         effect resource AuditLog;
         struct Db { tag: i64 }
         impl Db { fn id(self) -> i64 { self.tag } }
         struct Log { count: i64 }
         impl Log { fn count(self) -> i64 { self.count } }
         fn main() {
             providers {
                 UserDB   => Db { tag: 7 },
                 AuditLog => Log { count: 3 },
             } in {
                 println(UserDB.id());
                 println(AuditLog.count());
             }
         }");
    assert_eq!(output, "7\n3\n");
}

#[test]
fn test_providers_block_returns_body_value() {
    let output = run("effect resource UserDB;
         struct Db { tag: i64 }
         impl Db { fn id(self) -> i64 { self.tag } }
         fn main() {
             let r = providers {
                 UserDB => Db { tag: 42 },
             } in {
                 UserDB.id()
             };
             println(r);
         }");
    assert_eq!(output, "42\n");
}

#[test]
fn test_providers_block_single_trailing_comma_accepted() {
    let output = run("effect resource UserDB;
         struct Db { tag: i64 }
         impl Db { fn id(self) -> i64 { self.tag } }
         fn main() {
             providers {
                 UserDB => Db { tag: 9 },
             } in {
                 println(UserDB.id());
             }
         }");
    assert_eq!(output, "9\n");
}

#[test]
fn test_providers_block_frames_popped_after_body() {
    // After the block exits, all bindings are released — the trailing
    // `UserDB.id()` should fail with a missing-provider error, confirming
    // every pushed frame was popped.
    let errors = runtime_errors(
        "effect resource UserDB;
         effect resource AuditLog;
         struct Db { tag: i64 }
         impl Db { fn id(self) -> i64 { self.tag } }
         struct Log { count: i64 }
         impl Log { fn count(self) -> i64 { self.count } }
         fn main() {
             providers {
                 UserDB   => Db { tag: 1 },
                 AuditLog => Log { count: 2 },
             } in {
                 println(UserDB.id());
             }
             println(UserDB.id());
         }",
    );
    assert!(
        errors.iter().any(|e| e
            .message
            .contains("no provider bound for resource 'UserDB'")),
        "expected missing-provider error after block exit, got {:?}",
        errors
    );
}

#[test]
fn test_providers_block_evaluates_all_expressions_before_scope() {
    // Evaluate-all-then-scope semantics: every provider expression runs in
    // source order before any resource scope opens. We observe this by
    // threading a captured `i` counter through side-effecting constructors
    // that each append a trace string; the body ties off the sequence.
    let output = run("effect resource R1;
         effect resource R2;
         struct P { n: i64 }
         impl P { fn n(self) -> i64 { self.n } }
         fn mk1() -> P { println(\"eval1\"); P { n: 1 } }
         fn mk2() -> P { println(\"eval2\"); P { n: 2 } }
         fn main() {
             providers {
                 R1 => mk1(),
                 R2 => mk2(),
             } in {
                 println(\"body\");
                 println(R1.n());
                 println(R2.n());
             }
         }");
    assert_eq!(output, "eval1\neval2\nbody\n1\n2\n");
}

#[test]
fn test_providers_block_inner_binding_overrides_outer_with_provider() {
    // Nesting a `providers { R => ... }` block inside an outer
    // `with_provider[R]` should shadow the outer binding for the body's
    // duration and restore it on exit — same stack semantics as
    // nested `with_provider`.
    let output = run("effect resource UserDB;
         struct Db { tag: i64 }
         impl Db { fn id(self) -> i64 { self.tag } }
         fn main() {
             with_provider[UserDB](Db { tag: 1 }, || {
                 println(UserDB.id());
                 providers {
                     UserDB => Db { tag: 2 },
                 } in {
                     println(UserDB.id());
                 }
                 println(UserDB.id());
             });
         }");
    assert_eq!(output, "1\n2\n1\n");
}

// ── Pool[T] — connection-pool primitive surface ────────────────────

#[test]
fn test_pool_new_returns_pool_value_with_handle() {
    // v1 surface check: construct a `Pool[i64]`. The Kāra Pool value
    // is opaque except for the side-table handle_id, which the
    // intrinsic mints fresh via the monotonic counter — non-zero
    // tells us `Pool.new` actually fired (vs falling through to the
    // typecheck-only placeholder body that would leave handle_id 0).
    let output = run(r#"fn make_int() -> i64 { 42 }
         fn main() {
             let pool: Pool[i64] = Pool.new(make_int, 4, 8);
             println(pool.handle_id > 0);
         }"#);
    assert_eq!(output, "true\n");
}

#[test]
fn test_pool_acquire_at_cap_returns_timeout() {
    // `max_connections` is the hard cap. Filling it and trying
    // another acquire fires `PoolError.Timeout` immediately —
    // single-threaded interpreter has no peer to free a slot mid-wait.
    let output = run(r#"fn make_int() -> i64 { 7 }
         fn main() {
             let pool: Pool[i64] = Pool.new(make_int, 2, 8);
             match pool.acquire(0) {
                 Ok(_) => {
                     match pool.acquire(0) {
                         Ok(_) => {
                             match pool.acquire(0) {
                                 Ok(_) => println("ok??"),
                                 Err(PoolError.Timeout) => println("timeout"),
                                 Err(_) => println("other_err"),
                             }
                         }
                         Err(_) => println("acq2_err"),
                     }
                 }
                 Err(_) => println("acq1_err"),
             }
         }"#);
    assert_eq!(output, "timeout\n");
}

#[test]
fn test_pool_acquire_on_uninitialized_handle_returns_pool_closed() {
    // A hand-rolled `Pool { handle_id: 0 }` bypasses `Pool.new` so
    // there's no entry in the side-table — acquire surfaces this
    // as `PoolError.PoolClosed` rather than panicking. Defensive
    // against user code that constructs Pool literals manually.
    let output = run(r#"fn main() {
         let pool: Pool[i64] = Pool { handle_id: 0 };
         match pool.acquire(0) {
             Ok(_) => println("ok??"),
             Err(PoolError.Timeout) => println("timeout"),
             Err(PoolError.PoolClosed) => println("closed"),
             Err(PoolError.CreateFailed) => println("create_failed"),
         }
     }"#);
    assert_eq!(output, "closed\n");
}

// ── Arena[T] — bulk-allocation primitive surface ───────────────────

#[test]
fn test_arena_new_returns_handle() {
    // v1 surface check: `Arena.new()` mints a fresh side-table handle.
    // A non-zero handle_id tells us the `"Arena.new"` path arm actually
    // fired (vs falling through to the placeholder body that leaves
    // handle_id 0).
    let output = run(r#"fn main() {
             let a: Arena[i64] = Arena.new();
             println(a.handle_id > 0);
         }"#);
    assert_eq!(output, "true\n");
}

// ── Symbol + Interner — dedup string-handle primitive ──────────────

#[test]
fn test_interner_new_returns_handle() {
    // v1 surface check: `Interner.new()` mints a fresh side-table handle.
    // A non-zero handle_id tells us the `"Interner.new"` path arm fired.
    let output = run(r#"fn main() {
             let tab: Interner = Interner.new();
             println(tab.handle_id > 0);
         }"#);
    assert_eq!(output, "true\n");
}

/// `Request.header(name)` interpreter shape: returns `Option[String]`,
/// with the interpreter (no real HTTP server) always falling through
/// to `None`. Pins: the method dispatches at all, the args slot
/// accepts a `String`, and the result pattern-matches as `Option`.
/// Real header lookup happens through the codegen path —
/// `tests/http_server.rs::test_server_serve_handler_reads_header`.
#[test]
fn test_server_serve_handler_request_header_returns_none() {
    let output = run(r#"
fn main() {
    let req = Request { };
    match req.header("content-type") {
        Some(v) => println(v),
        None => println("none"),
    }
}
"#);
    assert_eq!(output, "none\n");
}

/// `Request.headers()` / `.query()` interpreter shape: both return a
/// `Vec[(String, String)]`. With no real HTTP server the stub Request
/// carries no data, so each is empty. Pins that the methods dispatch
/// and produce a Vec (whose `.len()` is 0) rather than a scalar. Real
/// iteration is exercised by the codegen E2E tests in
/// `tests/http_server.rs`.
#[test]
fn test_server_serve_handler_request_headers_and_query_return_empty() {
    let output = run(r#"
fn main() {
    let req = Request { };
    let h = req.headers();
    let q = req.query();
    println(h.len());
    println(q.len());
}
"#);
    assert_eq!(output, "0\n0\n");
}

#[test]
fn test_dbg_returns_its_argument() {
    // dbg() is an identity function — the value flows through.
    let src = r#"fn main() {
    let x = dbg(3) + dbg(4);
    println(x);
}
"#;
    let (stdout, dbg) = run_program_with_dbg(src, DbgOutputMode::Terminal);
    assert_eq!(stdout.join(""), "7\n");
    assert_eq!(dbg.len(), 2);
}

#[test]
fn test_module_binding_mut_reassignment_from_fn() {
    // A `let mut` module binding accumulates across function calls — the
    // reassignment resolves against the global slot.
    let out = run_no_errors(
        "let mut TOTAL: i64 = 0i64;\n\
         fn add(n: i64) { TOTAL = TOTAL + n; }\n\
         fn main() {\n\
             add(10i64);\n\
             add(5i64);\n\
             println(TOTAL);\n\
         }",
    );
    assert_eq!(out, "15\n");
}

#[test]
fn test_module_binding_local_shadow_takes_precedence() {
    // Parity with tests/codegen.rs::test_e2e_modbind_local_shadow_takes_precedence
    // — a function-local binding shadows the module binding for its scope.
    let out = run_no_errors(
        "let mut COUNTER: i64 = 100i64;\n\
         fn local_shadows() -> i64 {\n\
             let COUNTER: i64 = 7i64;\n\
             COUNTER\n\
         }\n\
         fn main() {\n\
             println(local_shadows());\n\
             println(COUNTER);\n\
         }",
    );
    assert_eq!(out, "7\n100\n");
}

// ── Script mode (design.md § Script mode, phase-8 Q7) ─────────────────────

#[test]
fn test_script_mode_top_level_statements_execute() {
    // A main-less file of top-level statements runs via the synthesized
    // `fn main()` — previously the statements were silently dropped and
    // the file was a no-op under the interpreter.
    assert_eq!(run("let x = 41;\nprintln(x + 1);\n"), "42\n");
}

#[test]
fn test_script_mode_items_hoist_and_statements_run_in_order() {
    assert_eq!(
        run("fn double(x: i64) -> i64 { x * 2 }\nlet y = double(21);\nprintln(y);\nprintln(y + 1);\n"),
        "42\n43\n"
    );
}

/// B-2026-08-31-7 (the hazard the fix above had to clear first) — A `let` STARTS
/// A NEW GENERATION OF ITS NAMES, so a stale param-VIEW mark must not survive it.
///
/// `owned_param_names_stack`'s top frame (this backend) and `param_view_locals`
/// (codegen) were both write-only: every site inserted, none removed, and each
/// was cleared only at function entry. A name marked a view by an earlier
/// construct therefore stayed one across a later, unrelated `let` of the SAME
/// NAME — and view-ness means "someone else runs the body", so the fresh value's
/// body ran NOWHERE.
///
/// FOUND ON THE ENUM PATH, which predates -31-7 entirely: `enum` below printed
/// `arm2 fresh98 dR2` where `dR98` is due as well, here and on both compiled
/// backends alike. Both were wrong and AGREED, which is exactly why it stayed
/// invisible — no run-vs-build signal to trip over.
///
/// It surfaced only because -31-7's tuple marking was probed for over-reach: the
/// `tuple` case below regressed the moment bare-tuple elements were marked, and
/// the enum control proved the hazard belonged to the mechanism rather than to
/// the new caller. Repairing codegen alone would have converted an agreed-wrong
/// answer into a fresh divergence, so both sides land together.
///
/// `selfrebind` is the exemption that keeps the clear honest: `let r = r;` reads
/// the very generation being cleared and must still inherit its view-ness — one
/// body, not two.
///
/// Twin of `tests/codegen.rs`'s `e2e_a_let_clears_a_stale_param_view_mark`,
/// pinned to the same string.
#[test]
fn test_a_let_clears_a_stale_param_view_mark() {
    assert_eq!(
        run(r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
enum E { A(R), B }

fn tup(t: (R, i64)) {
    match t { (r, k) => { println(f"  arm{r.id}"); } }
    let r = R { id: 99 };
    let m = r;
    println(f"  fresh{m.id}")
}

fn enu(t: E) {
    match t { E.A(r) => { let m = r; println(f"  arm{m.id}"); } E.B => { } }
    let r = R { id: 98 };
    let m2 = r;
    println(f"  fresh{m2.id}")
}

fn selfrebind(t: (R, i64)) {
    match t { (r, k) => { let r = r; println(f"  arm{r.id}"); } }
}

fn main() {
    println("tuple");      tup((R { id: 1 }, 0));  println("tuple end");
    println("enum");       enu(E.A(R { id: 2 }));  println("enum end");
    println("selfrebind"); selfrebind((R { id: 3 }, 0)); println("selfrebind end");
    println("done");
}
"#),
        r#"tuple
  arm1
  fresh99
dR99
dR1
tuple end
enum
  arm2
  fresh98
dR98
dR2
enum end
selfrebind
  arm3
dR3
selfrebind end
done
"#
    );
}

/// B-2026-08-29-14, interpreter leg — a METHOD that hands its owned param back
/// with `return r;` rather than as a BLOCK TAIL runs the body exactly once.
///
/// The interpreter twin of
/// `e2e_return_spelling_method_returned_param_body_runs_once`. This backend was
/// already CORRECT for the row's headline program — it was the compiled ones
/// that doubled — so these cases are here to keep it correct: the fix widened
/// `fn_always_returns_param`, which this backend also consults (to decide
/// whether a method frame claims an owned param), so the risk it introduces
/// here is the OPPOSITE failure, a frame that stops claiming a param nothing
/// else owns and drops the body entirely.
///
/// That is what the `keeps-frame-claim` cases pin: each names a shape the
/// widened predicate must still DECLINE. Three of them — a unit-returning
/// method with a bare `return;`, a `let ... else { return .. }`, and a `return`
/// of some other value — are declined by the "no bad return" test alone, since
/// each has a visible `return` handing something else back.
///
/// The LAST one is different and is the case that pins the no-tail arm's own
/// guards. It has no `return` at all and no tail, so nothing contradicts
/// "always returns the param" except `f.return_type.is_some() && any_good_return`.
/// Verified by deleting those two conditions: that case alone drops its body
/// (`saw 41` / `after`) while every other case here stays green.
#[test]
fn test_return_spelling_method_returned_param_body_runs_once() {
    const DROPPER: &str = "struct R { id: i64 }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id}\") } }\n\
         struct B5 { n: i64 }\n";
    for (label, body, want) in [
        // The row's headline program. Correct here before the fix; pinned so a
        // later widening cannot quietly take the body away.
        (
            "method-return-spelling",
            "impl B5 { fn take(ref self, r: R) -> R { return r; } }\n\
             fn main() { let b = B5 { n: 1 }; let x = b.take(R { id: 41 }); \
             println(f\"{x.id}\") }\n",
            "41\ndrop 41\n",
        ),
        // The aggregate `return`, which the compiled backends doubled and this
        // one got right.
        (
            "aggregate-return",
            "struct Hh { r: R }\n\
             impl B5 { fn wrap(ref self, r: R) -> Hh { return Hh { r: r }; } }\n\
             fn main() { let b = B5 { n: 1 }; let h = b.wrap(R { id: 41 }); \
             println(f\"{h.r.id}\") }\n",
            "41\ndrop 41\n",
        ),
        // KEEPS FRAME CLAIM — a unit-returning method has nothing to hand the
        // param back THROUGH, so the frame must still own it. Over-admit and
        // this prints `after` alone.
        (
            "unit-bare-return-keeps-frame-claim",
            "impl B5 { fn eat(ref self, r: R, k: bool) { if k { return; } \
             println(f\"kept {r.id}\") } }\n\
             fn main() { let b = B5 { n: 1 }; b.eat(R { id: 41 }, true); \
             println(\"after\") }\n",
            "drop 41\nafter\n",
        ),
        // KEEPS FRAME CLAIM — `let ... else { return .. }` is the language's
        // most common bare `return`, behind a statement kind the predicate's
        // walk did not visit until this fix taught it to.
        (
            "let-else-return-keeps-frame-claim",
            "impl B5 { fn take(ref self, r: R, o: Option[i64]) -> R {\n\
             let Some(v) = o else { return R { id: 98 }; };\n\
             println(f\"v {v}\"); return r; } }\n\
             fn main() { let b = B5 { n: 1 }; let x = b.take(R { id: 41 }, None); \
             println(f\"{x.id}\") }\n",
            "drop 41\n98\ndrop 98\n",
        ),
        // KEEPS FRAME CLAIM — a `return` of a DIFFERENT value. Having no
        // counter-example is not the same as always handing the param back,
        // which is why the no-tail arm also requires a return that yields it.
        (
            "returns-other-value-keeps-frame-claim",
            "impl B5 { fn swapout(ref self, r: R) -> R { println(f\"saw {r.id}\"); \
             return R { id: 99 }; } }\n\
             fn main() { let b = B5 { n: 1 }; let x = b.swapout(R { id: 41 }); \
             println(f\"{x.id}\") }\n",
            "saw 41\ndrop 41\n99\ndrop 99\n",
        ),
        // KEEPS FRAME CLAIM, and the ONLY case here the no-tail arm's own
        // guards decide — see this test's doc. No `return` anywhere and no
        // tail: without `f.return_type.is_some() && any_good_return` the
        // predicate would read "nothing contradicts it" as "always returns the
        // param", the frame would stop claiming, and this body would vanish.
        (
            "no-return-unit-method-keeps-frame-claim",
            "impl B5 { fn eat2(ref self, r: R) { println(f\"saw {r.id}\"); } }\n\
             fn main() { let b = B5 { n: 1 }; b.eat2(R { id: 41 }); \
             println(\"after\") }\n",
            "saw 41\ndrop 41\nafter\n",
        ),
        // CONTROL — the free-function UNIT twin of the case above, which is the
        // oracle that makes one body the right answer there.
        (
            "free-fn-unit-oracle",
            "fn eatf(r: R) { println(f\"saw {r.id}\"); }\n\
             fn main() { eatf(R { id: 41 }); println(\"after\") }\n",
            "saw 41\ndrop 41\nafter\n",
        ),
        // CONTROL — the free-function twin, correct throughout.
        (
            "free-fn-return-oracle",
            "fn idf(r: R) -> R { return r; }\n\
             fn main() { let x = idf(R { id: 41 }); println(f\"{x.id}\") }\n",
            "41\ndrop 41\n",
        ),
    ] {
        assert_eq!(run(&format!("{DROPPER}{body}")), want, "{label}");
    }
}

/// B-2026-08-30-53 — the ORACLE half of the conditional param-view
/// assignment. `out = r` inside a match arm, where `r` is a payload view of
/// an owned param, must run `out`'s OWN body exactly once on the path where
/// the arm never ran — `out` still holds the value it was declared with and
/// nobody else owns it, because the scrutinee is `E.B` and has no payload to
/// walk.
///
/// This half was already GREEN before the fix. Codegen enforced the taken
/// path's suppression with an all-paths retraction, so it lost the body on
/// the not-taken path too (`mid dE a0` against this `mid dR0 dE a0`); the
/// interpreter is path-sensitive for free. Pinned here for the usual reason:
/// it is the oracle the compiled half was moved onto, so a future change
/// that "reconciles" the two by moving the interpreter has to break this
/// first.
///
/// The parity twin of
/// `codegen::test_e2e_conditional_param_view_assign_keeps_target_body_per_path`.
#[test]
fn conditional_param_view_assign_keeps_the_targets_own_body() {
    const H: &str = "struct R { id: i64, tag: String }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
         enum E { A(R), B }\n\
         impl Drop for E { fn drop(mut ref self) { println(\"dE\") } }\n\
         fn dies(b: E) -> i64 {\n\
             let mut out: R = R { id: 0, tag: f\"t0\" };\n\
             match b { E.A(r) => { out = r; } E.B => { } }\n\
             println(\"mid\");\n\
             return out.id\n\
         }\n\
         fn fresh(b: E) -> i64 {\n\
             let mut out: R = R { id: 1, tag: f\"t1\" };\n\
             match b { E.A(r) => { out = R { id: 7, tag: f\"t7\" }; } E.B => { } }\n\
             return out.id\n\
         }\n\
         fn uncond(p: R) -> i64 {\n\
             let mut out: R = R { id: 2, tag: f\"t2\" };\n\
             out = p;\n\
             return out.id\n\
         }\n\
         fn looped(b: E) -> i64 {\n\
             let mut i: i64 = 0;\n\
             let mut acc: i64 = 0;\n\
             while i < 2 {\n\
                 let mut out: R = R { id: 90 + i, tag: f\"t\" };\n\
                 match b { E.A(r) => { out = r; } E.B => { } }\n\
                 acc = acc + out.id;\n\
                 i = i + 1;\n\
             }\n\
             return acc\n\
         }\n\
         fn refreshed(b: E) -> i64 {\n\
             let mut out: R = R { id: 0, tag: f\"t0\" };\n\
             match b { E.A(r) => { out = r; } E.B => { } }\n\
             println(\"m1\");\n\
             out = R { id: 5, tag: f\"t5\" };\n\
             println(\"m2\");\n\
             return out.id\n\
         }\n";
    for (label, body, want) in [
        // The row's repro: the arm is NOT taken, so `out` still owns its
        // initializer and must run its body.
        (
            "arm-not-taken",
            "println(f\"a{dies(E.B)}\")\n",
            "mid\ndR0\ndE\na0\n",
        ),
        // The taken path, unchanged: the displacement fires the OLD value at
        // the assignment, ahead of `mid`, and the moved-in value's body is the
        // caller's.
        (
            "arm-taken",
            "println(f\"b{dies(E.A(R { id: 5, tag: f\"t5\" }))}\")\n",
            "dR0\nmid\ndE\ndR5\nb5\n",
        ),
        // The in-loop sibling of the repro: a fresh `out` per iteration, the
        // arm never taken, one body each.
        (
            "in-a-loop",
            "println(f\"e{looped(E.B)}\")\n",
            "dR90\ndR91\ndE\ne181\n",
        ),
        // Boundaries that were already correct everywhere. A FRESH value
        // assigned in the arm never triggers the suppression at all, and an
        // UNCONDITIONAL param-view assign has only one path, so its
        // suppression was never wrong.
        (
            "fresh-value-in-arm",
            "println(f\"c{fresh(E.B)}\")\n",
            "dR1\ndE\nc1\n",
        ),
        (
            "unconditional-assign",
            "println(f\"d{uncond(R { id: 9, tag: f\"t9\" })}\")\n",
            "dR2\ndR9\nd9\n",
        ),
        // The RE-ARM row. A target that held a view and is then given a FRESH
        // value owns that value and must run its body. The interpreter has
        // always been right here; it is pinned because codegen's first cut of
        // the flag lost `dR5`, having replaced a retraction that
        // `rearm_container_bodies_for_name` used to undo on every assignment.
        (
            "view-then-fresh-value",
            "println(f\"f{refreshed(E.A(R { id: 8, tag: f\"t8\" }))}\")\n",
            "dR0\nm1\nm2\ndR5\ndE\ndR8\nf5\n",
        ),
    ] {
        let src = format!("{H}fn main() {{ {body} }}\n");
        assert_eq!(run(&src), want, "{label}");
    }
}

/// B-2026-09-03-30 — the interpreter twin of B-2026-08-30-53's re-arm, for the
/// DIRECT-param spelling the conditional test above does not cover.
///
/// `a = h;` where `h` is the by-value PARAM (not a match-arm payload view)
/// takes `suppress_assign_move_user_drop`'s param branch, which retracts the
/// TARGET `a`'s Drop slot and marks it a view. That branch had no re-arm
/// counterpart, so when `a` was given a value of its OWN again (`a = R { .. }`,
/// or `a = g;` for a local `g`) the retracted slot stayed gone and the fresh
/// value's scope-exit body fired nowhere: `a = h; a = R{5}` printed
/// `dR1 dR4 v5`, losing `dR5` against every compiled backend's
/// `dR1 dR5 dR4 v5`. (The match-arm spelling `out = r; out = R{5}` retracts the
/// SOURCE `r`'s slot instead, so the interpreter was always right there — it is
/// the `refreshed`/"view-then-fresh-value" case above, which stays green.)
///
/// Parity twin of `codegen::test_e2e_direct_param_view_assign_then_fresh_value`.
#[test]
fn direct_param_view_assign_then_fresh_value_reruns_exit_body() {
    const H: &str = "struct R { id: i64, tag: String }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
         fn mk9() -> R { return R { id: 9, tag: f\"n\" } }\n\
         fn refresh(h: R) -> i64 {\n\
             let mut a: R = R { id: 1, tag: f\"a\" };\n\
             a = h;\n\
             a = R { id: 5, tag: f\"c\" };\n\
             return a.id\n\
         }\n\
         fn refresh_twice(h: R) -> i64 {\n\
             let mut a: R = R { id: 1, tag: f\"a\" };\n\
             a = h;\n\
             a = R { id: 5, tag: f\"c\" };\n\
             a = R { id: 6, tag: f\"d\" };\n\
             return a.id\n\
         }\n\
         fn local_source() -> i64 {\n\
             let g: R = mk9();\n\
             let mut a: R = R { id: 1, tag: f\"a\" };\n\
             a = g;\n\
             a = R { id: 5, tag: f\"c\" };\n\
             return a.id\n\
         }\n";
    for (label, body, want) in [
        // The row's repro: `dR1` displaces the initializer at `a = h`, `dR5` is
        // the surviving fresh value's scope-exit body (the fire that was lost),
        // then the caller drops the param `h` (`dR4`).
        (
            "refresh",
            "println(f\"v{refresh(R { id: 4, tag: f\"b\" })}\")\n",
            "dR1\ndR5\ndR4\nv5\n",
        ),
        // Discriminator: the DISPLACEMENT fire was always kept — only the
        // exit fire was lost. `dR5` here fires as `R{6}` displaces it; `dR6`
        // is the exit fire the re-arm restores.
        (
            "refresh-twice",
            "println(f\"v{refresh_twice(R { id: 4, tag: f\"b\" })}\")\n",
            "dR1\ndR5\ndR6\ndR4\nv6\n",
        ),
        // Boundary: a LOCAL source in place of the param view never marks `a`
        // a view, so it was clean before and must stay clean after.
        (
            "local-source",
            "println(f\"v{local_source()}\")\n",
            "dR1\ndR9\ndR5\nv5\n",
        ),
    ] {
        let src = format!("{H}fn main() {{ {body} }}\n");
        assert_eq!(run(&src), want, "{label}");
    }
}

/// B-2026-08-30-18 — the ORACLE half of the return-position aggregate
/// literal. An aggregate literal built directly at an explicit `return`,
/// carrying a `Vec` of `Drop` elements moved in from a named local, runs
/// each element's body exactly ONCE, at the caller's binding death.
///
/// This half was already GREEN before the fix — the compiled backends ran
/// the body twice (`mid dR14 v14 dR14 post` against this `mid v14 dR14
/// post`) because their explicit-`return` hook set was missing
/// B-2026-08-02-23 leg 2's aggregate-literal source disarm. It is pinned
/// here for the same reason as the sibling below: it is the oracle the
/// compiled half was moved onto, so a future change that "reconciles" the
/// two by moving the interpreter has to break this first.
///
/// The parity twin of
/// `codegen::test_e2e_aggregate_literal_at_explicit_return_single_fire`.
#[test]
fn aggregate_literal_at_explicit_return_fires_element_bodies_once() {
    const H: &str = "struct R { id: i64, tag: String }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
         struct Box3 { xs: Vec[R] }\n\
         struct Two { xs: Vec[R], ys: Vec[R] }\n\
         fn build(k: i64) -> Box3 {\n\
             let mut v: Vec[R] = Vec.new();\n\
             v.push(R { id: k, tag: f\"t\" });\n\
             println(\"mid\");\n\
             return Box3 { xs: v }\n\
         }\n\
         fn build_tuple(k: i64) -> (Vec[R], i64) {\n\
             let mut v: Vec[R] = Vec.new();\n\
             v.push(R { id: k, tag: f\"t\" });\n\
             return (v, 7)\n\
         }\n\
         fn build_two(k: i64) -> Two {\n\
             let mut a: Vec[R] = Vec.new();\n\
             a.push(R { id: k, tag: f\"t\" });\n\
             let mut b: Vec[R] = Vec.new();\n\
             b.push(R { id: k + 1, tag: f\"u\" });\n\
             return Two { xs: a, ys: b }\n\
         }\n\
         fn build_cond(k: i64) -> Box3 {\n\
             let mut v: Vec[R] = Vec.new();\n\
             v.push(R { id: k, tag: f\"t\" });\n\
             if k > 0 { return Box3 { xs: v } }\n\
             return Box3 { xs: v }\n\
         }\n";
    for (label, body, want) in [
        // The row's own repro: the struct literal at the return site.
        (
            "struct-literal-at-return",
            "let a: Box3 = build(14); println(f\"v{a.xs[0].id}\")\n",
            "mid\nv14\ndR14\n",
        ),
        // A TUPLE literal in the same position — the other aggregate form
        // the disarm walks.
        (
            "tuple-literal-at-return",
            "let t: (Vec[R], i64) = build_tuple(20); println(f\"t{t.1}\")\n",
            "t7\ndR20\n",
        ),
        // Two independent sources moved into ONE literal.
        (
            "two-vec-fields",
            "let w: Two = build_two(30); println(f\"w{w.xs[0].id}{w.ys[0].id}\")\n",
            "w3031\ndR31\ndR30\n",
        ),
        // A CONDITIONAL return: the compiled disarm is static, so it has to
        // cover every returning path rather than the syntactic tail alone.
        (
            "conditional-return",
            "let c: Box3 = build_cond(40); println(f\"c{c.xs[0].id}\")\n",
            "c40\ndR40\n",
        ),
        // The two spellings that were ALREADY correct on every surface,
        // kept as boundaries: routing through a named binding, and the bare
        // tail with no `return` at all. A fix that over-disarms would show
        // up here as a MISSING body.
        (
            "named-binding-then-return",
            "let a: Box3 = { let mut v: Vec[R] = Vec.new(); v.push(R { id: 50, tag: f\"t\" }); let bx: Box3 = Box3 { xs: v }; bx };\n\
             println(f\"v{a.xs[0].id}\")\n",
            "v50\ndR50\n",
        ),
        (
            "bare-tail-literal",
            "let a: Box3 = { let mut v: Vec[R] = Vec.new(); v.push(R { id: 60, tag: f\"t\" }); Box3 { xs: v } };\n\
             println(f\"v{a.xs[0].id}\")\n",
            "v60\ndR60\n",
        ),
    ] {
        let src = format!("{H}fn main() {{ {body} }}\n");
        assert_eq!(run(&src), want, "{label}");
    }
}

/// B-2026-08-29-57 and -65, interpreter leg — the twin of
/// `codegen::e2e_tail_return_hands_the_value_out_like_a_return_statement`,
/// asserting the same strings so a later one-sided edit cannot re-negotiate
/// them.
///
/// A `return x` in a block's TAIL position, with
/// no trailing semicolon, hands `x` to the caller exactly as `return x;`
/// does. One `Drop` body is due either way, and the semicolon must not be
/// a semantic change.
///
/// It was one, twice over, in two different places:
///
///   * a returned LOCAL ran its body twice on the INTERPRETER (-57) —
///     `mid dR1 v1 dR1` against `mid v1 dR1` compiled. The block-exit hook
///     drives `suppress_tail_expr_user_drop` and its container twin, both
///     of which match on `Identifier`; handed the `Return` node itself they
///     silently did nothing, so the callee dropped the local at its own
///     scope exit as well as handing it out. The statement loop's
///     `suppress_return_stmt_user_drop` already unwrapped the `Return`,
///     which is exactly why `return out;` was correct.
///   * a returned PARAM ran its body twice on EVERY backend (-64), so no
///     A/B gate could see it. `fn_always_returns_param` collects the body's
///     leaf tails and requires each to YIELD the param; a tail `return r`
///     reached that walk as the `Return` node, yielded nothing, and the
///     predicate declined for a body whose only exit hands the param back.
///     The no-tail arm is what made `return r;` correct.
///
/// Both are the same shape one level apart, which is why they are pinned
/// together: every tail-`return` row here has its `return x;` twin beside
/// it, and the two must print the same string.
///
/// `branch-*` and `param-dies-*` are the guards on the widened walk. The
/// first keeps a sibling local that is NOT returned dying inside; the
/// second keeps the predicate declining when no `return` yields the param,
/// where a too-permissive walk would stand the caller down and LOSE a body
/// rather than duplicate one.
#[test]
fn tail_return_hands_the_value_out_like_a_return_statement() {
    const H: &str = "struct R { id: i64, tag: String }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
         enum E { A(R), B }\n\
         impl Drop for E { fn drop(mut ref self) { println(\"dE\") } }\n\
         struct T { n: i64 }\n\
         fn t_local(k: i64) -> R { let out: R = R { id: k, tag: f\"t\" }; println(\"mid\"); return out }\n\
         fn s_local(k: i64) -> R { let out: R = R { id: k, tag: f\"t\" }; println(\"mid\"); return out; }\n\
         fn b_local(k: i64) -> R { let out: R = R { id: k, tag: f\"t\" }; println(\"mid\"); out }\n\
         fn t_enum(k: i64) -> E { let out: E = E.A(R { id: k, tag: f\"t\" }); println(\"mid\"); return out }\n\
         fn t_opt(k: i64) -> Option[R] { let o: Option[R] = Some(R { id: k, tag: f\"t\" }); println(\"mid\"); return o }\n\
         fn t_param(r: R) -> R { println(\"mid\"); return r }\n\
         fn s_param(r: R) -> R { println(\"mid\"); return r; }\n\
         fn t_branch(k: i64, flag: bool) -> R { let a: R = R { id: k, tag: f\"a\" }; let b: R = R { id: k + 50, tag: f\"b\" }; println(\"mid\"); if flag { return a } else { return b } }\n\
         fn t_dies(r: R, flag: bool) -> i64 { if flag { return 1 } println(\"mid\"); return 0 }\n\
         impl T {\n\
         \x20   fn m_local(ref self, k: i64) -> R { let out: R = R { id: k, tag: f\"t\" }; println(\"mid\"); return out }\n\
         \x20   fn m_param(ref self, r: R) -> R { println(\"mid\"); return r } }\n";
    for (label, body, want) in [
            // THE ROW: a tail `return out` with NO trailing semicolon.
            (
                "local-tail-return",
                "let v: R = t_local(1); println(f\"v{v.id}\");\n",
                "mid\nv1\ndR1\npost\n",
            ),
            // The same function with a semicolon, and with no `return` at all.
            // All three must print the same thing.
            (
                "local-stmt-return",
                "let v: R = s_local(1); println(f\"v{v.id}\");\n",
                "mid\nv1\ndR1\npost\n",
            ),
            (
                "local-bare-tail",
                "let v: R = b_local(1); println(f\"v{v.id}\");\n",
                "mid\nv1\ndR1\npost\n",
            ),
            (
                "enum-local-tail-return",
                "let v: E = t_enum(4);\n\
                 \x20 match v { E.A(r) => { println(f\"v{r.id}\") } E.B => { println(\"vB\") } }\n",
                "mid\nv4\ndE\ndR4\npost\n",
            ),
            (
                "option-local-tail-return",
                "let v: Option[R] = t_opt(5);\n\
                 \x20 match v { Some(r) => { println(f\"v{r.id}\") } None => { println(\"vN\") } }\n",
                "mid\nv5\ndR5\npost\n",
            ),
            // The PARAM half — `fn_always_returns_param`'s leaf-tail walk. This
            // pair doubled on EVERY backend, so no A/B gate saw it.
            (
                "param-tail-return",
                "let p: R = R { id: 6, tag: f\"p\" }; let v: R = t_param(p); println(f\"v{v.id}\");\n",
                "mid\nv6\ndR6\npost\n",
            ),
            (
                "param-stmt-return",
                "let p: R = R { id: 7, tag: f\"p\" }; let v: R = s_param(p); println(f\"v{v.id}\");\n",
                "mid\nv7\ndR7\npost\n",
            ),
            (
                "method-local-tail-return",
                "let t = T { n: 1 }; let v: R = t.m_local(8); println(f\"v{v.id}\");\n",
                "mid\nv8\ndR8\npost\n",
            ),
            (
                "method-param-tail-return",
                "let t = T { n: 1 }; let p: R = R { id: 9, tag: f\"p\" };\n\
                 \x20 let v: R = t.m_param(p); println(f\"v{v.id}\");\n",
                "mid\nv9\ndR9\npost\n",
            ),
            // GUARD for the leaf-tail walk seeing through `return`: only the
            // returned local escapes, the sibling still dies inside.
            (
                "branch-tail-returns-then",
                "let v: R = t_branch(10, true); println(f\"v{v.id}\");\n",
                "mid\ndR60\nv10\ndR10\npost\n",
            ),
            (
                "branch-tail-returns-else",
                "let v: R = t_branch(11, false); println(f\"v{v.id}\");\n",
                "mid\ndR11\nv61\ndR61\npost\n",
            ),
            // GUARD in the other direction: no `return` yields the param, so
            // the predicate must keep declining and the caller must keep
            // firing. A too-permissive leaf-tail walk loses these bodies.
            (
                "param-dies-early-return",
                "let p: R = R { id: 12, tag: f\"p\" }; let v: i64 = t_dies(p, true); println(f\"v{v}\");\n",
                "dR12\nv1\npost\n",
            ),
            (
                "param-dies-late-return",
                "let p: R = R { id: 13, tag: f\"p\" }; let v: i64 = t_dies(p, false); println(f\"v{v}\");\n",
                "mid\ndR13\nv0\npost\n",
            ),
    ] {
        let src = format!("{H}fn main() {{ {body} println(\"post\") }}\n");
        assert_eq!(run(&src), want, "{label}");
    }
}

#[test]
fn test_bare_statement_container_removal_discard_fires() {
    // B-2026-08-03-2 (class 3) — a BUILTIN container removal discarded as a
    // BARE STATEMENT (`v.pop();`, `m.remove(k);`) was silent here while codegen
    // fired it: a run-vs-build split in the interpreter-silent direction. The
    // `let _ = v.pop();` form already worked, through the same method list, so
    // the two discard STATEMENT SHAPES disagreed with each other as well. The
    // third block is the bound control, correct throughout.
    //
    // Deliberately Option-returning removals only: that is codegen's current
    // reach, and `v.remove(i)` / `v.swap_remove(i)` (bare `T`) remain silent on
    // BOTH backends — admitting them here would have created a fresh
    // divergence rather than removing one. They stay class 2 on the row.
    assert_eq!(
        run("struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) { println(f\"drop {self.id} {self.name}\") }\n\
             }\n\
             fn main() {\n\
                 println(\"vecpop:\");\n\
                 {\n\
                     let mut v: Vec[Res] = Vec.new();\n\
                     v.push(Res { id: 1, name: f\"a{1}\" });\n\
                     v.pop();\n\
                     println(v.len());\n\
                 }\n\
                 println(\"mapremove:\");\n\
                 {\n\
                     let mut m: Map[i64, Res] = Map.new();\n\
                     m.insert(5, Res { id: 2, name: f\"b{2}\" });\n\
                     m.remove(5);\n\
                     println(m.len());\n\
                 }\n\
                 println(\"boundpop:\");\n\
                 {\n\
                     let mut w: Vec[Res] = Vec.new();\n\
                     w.push(Res { id: 3, name: f\"c{3}\" });\n\
                     let p = w.pop();\n\
                     println(w.len());\n\
                 }\n\
                 println(\"end\");\n\
             }\n"),
        "vecpop:\ndrop 1 a1\n0\nmapremove:\ndrop 2 b2\n0\nboundpop:\ndrop 3 c3\n0\nend\n"
    );
}

#[test]
fn test_passthrough_arg_and_returned_literal_single_fire() {
    // B-2026-08-02-23 leg 2 — interpreter twin of `tests/codegen.rs`'s
    // `e2e_passthrough_arg_and_returned_literal_single_fire`. Both backends
    // fired the body twice for the two passthrough shapes; the third block
    // (`consume`, which does NOT return its param) is the control that must
    // keep firing exactly once at the call.
    assert_eq!(
        run("struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) { println(f\"drop {self.id} {self.name}\") }\n\
             }\n\
             struct Holder { xs: Vec[Res], tag: i64 }\n\
             fn passthru(v: Vec[Res]) -> Vec[Res] { v }\n\
             fn mk(v: Vec[Res]) -> Holder { Holder { xs: v, tag: 9 } }\n\
             fn consume(v: Vec[Res]) -> i64 { v.len() }\n\
             fn main() {\n\
                 println(\"a\");\n\
                 {\n\
                     let mut xs: Vec[Res] = Vec.new();\n\
                     xs.push(Res { id: 1, name: f\"a{1}\" });\n\
                     let ys = passthru(xs);\n\
                     println(ys.len());\n\
                 }\n\
                 {\n\
                     let mut zs: Vec[Res] = Vec.new();\n\
                     zs.push(Res { id: 2, name: f\"b{2}\" });\n\
                     let h = mk(zs);\n\
                     println(h.tag);\n\
                 }\n\
                 {\n\
                     let mut ws: Vec[Res] = Vec.new();\n\
                     ws.push(Res { id: 3, name: f\"c{3}\" });\n\
                     println(consume(ws));\n\
                 }\n\
                 println(\"end\");\n\
             }\n"),
        "a\n1\ndrop 1 a1\n9\ndrop 2 b2\n1\ndrop 3 c3\nend\n"
    );
}

/// B-2026-09-08-5 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_deeper_frame_reassign_rearms_the_new_value`, same three sources and
/// expected strings. This backend has no per-frame cleanup list for an inner
/// scope to drain, so a reassign inside a branch or block re-owns the field on
/// the path that runs and leaves it moved on the path that does not — which is
/// the reference the codegen side is now matched to.
#[test]
fn test_deeper_frame_reassign_rearms_the_new_value() {
    assert_eq!(
        run("struct Rs { id: i64, name: String }\n\
             impl Drop for Rs {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"dS{self.id}\")\n\
                 }\n\
             }\n\
             fn mks(i: i64) -> Rs {\n\
                 return Rs { id: i, name: f\"h{i}\" };\n\
             }\n\
             struct Bs { mut one: Rs, mut two: Rs }\n\
             fn main() {\n\
                 let f = true;\n\
                 let mut g = Bs { one: mks(1), two: mks(2) };\n\
                 let taken = g.one;\n\
                 if f { g.one = mks(7); }\n\
                 println(f\"t{taken.id}\");\n\
             }\n"),
        "dS2\ndS7\nt1\ndS1\n"
    );
    assert_eq!(
        run("struct Rs { id: i64, name: String }\n\
             impl Drop for Rs {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"dS{self.id}\")\n\
                 }\n\
             }\n\
             fn mks(i: i64) -> Rs {\n\
                 return Rs { id: i, name: f\"h{i}\" };\n\
             }\n\
             struct Bs { mut one: Rs, mut two: Rs }\n\
             fn main() {\n\
                 let f = false;\n\
                 let mut g = Bs { one: mks(1), two: mks(2) };\n\
                 let taken = g.one;\n\
                 if f { g.one = mks(7); }\n\
                 println(f\"t{taken.id}\");\n\
             }\n"),
        "dS2\nt1\ndS1\n"
    );
    assert_eq!(
        run("struct Rs { id: i64, name: String }\n\
             impl Drop for Rs {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"dS{self.id}\")\n\
                 }\n\
             }\n\
             fn mks(i: i64) -> Rs {\n\
                 return Rs { id: i, name: f\"h{i}\" };\n\
             }\n\
             struct Bs { mut one: Rs, mut two: Rs }\n\
             fn main() {\n\
                 let mut g = Bs { one: mks(1), two: mks(2) };\n\
                 let taken = g.one;\n\
                 { g.one = mks(7); }\n\
                 println(f\"t{taken.id}\");\n\
             }\n"),
        "dS2\ndS7\nt1\ndS1\n"
    );
}

/// B-2026-08-01-16 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_assign_param_rebind_and_displaced_bodies`, same source and
/// expected string. Pre-fix the interpreter fired the caller-retained
/// value's body a second time off the target binding's still-armed Drop
/// slot (the Assign path had no view propagation — the Let-only gate of
/// B-2026-08-01-15); the `suppress_assign_move_user_drop` extension
/// retracts the target's slot and propagates view-ness, leaving exactly
/// the caller's single fire, and the displaced-value fires stay put.
#[test]
fn test_assign_param_rebind_and_displaced_bodies() {
    assert_eq!(
        run("struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"drop {self.id} {self.name}\")\n\
                 }\n\
             }\n\
             struct Holder { r: Res }\n\
             enum E2 { B(Res), Empty }\n\
             fn take(h: Holder) {\n\
                 let mut h2 = Holder { r: Res { id: 9, name: f\"z{9}\" } };\n\
                 h2 = h;\n\
                 println(f\"held {h2.r.id}\");\n\
                 println(\"take done\");\n\
             }\n\
             fn take_enum(w: E2) {\n\
                 let mut w2 = E2.Empty;\n\
                 w2 = w;\n\
                 match w2 {\n\
                     E2.B(r) => { println(f\"got {r.id}\"); }\n\
                     E2.Empty => { println(\"none\"); }\n\
                 }\n\
                 println(\"take2 done\");\n\
             }\n\
             fn main() {\n\
                 println(\"a\");\n\
                 let x = Holder { r: Res { id: 5, name: f\"y{5}\" } };\n\
                 take(x);\n\
                 println(\"b\");\n\
                 let mut d = Holder { r: Res { id: 3, name: f\"w{3}\" } };\n\
                 d = Holder { r: Res { id: 4, name: f\"v{4}\" } };\n\
                 println(f\"kept {d.r.id}\");\n\
                 println(\"c\");\n\
                 let e = E2.B(Res { id: 7, name: f\"q{7}\" });\n\
                 take_enum(e);\n\
                 println(\"end\");\n\
             }\n"),
        "a\ndrop 9 z9\nheld 5\ntake done\ndrop 5 y5\nb\ndrop 3 w3\nkept 4\ndrop 4 v4\nc\ngot 7\ntake2 done\ndrop 7 q7\nend\n"
    );
}

/// The panic the spec requires: "Panics if `mid > self.len()`".
///
/// `split_at` clamps with `.min(v.len())`; this must NOT. A silently-clamped
/// mutable partition hands back an empty second half where the caller expected
/// a writable tail, and the write then goes nowhere — the same invisible-loss
/// failure the aliasing test above guards, arrived at from the other side.
#[test]
fn split_at_mut_past_the_end_panics_rather_than_clamping() {
    let errors = runtime_errors(
        "fn main() {\n\
             let mut v: Vec[i64] = [1i64, 2i64];\n\
             let mut p: (mut Slice[i64], mut Slice[i64]) = v.split_at_mut(5i64);\n\
             println(f\"{p.0.len()}\");\n\
         }",
    );
    assert!(
        errors.iter().any(|e| e.message.contains("out of bounds")),
        "expected an out-of-bounds fault, got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
    // mid == len is the legal boundary, not a fault.
    assert_eq!(
        run_no_errors(
            "fn main() {\n\
                 let mut v: Vec[i64] = [1i64, 2i64];\n\
                 let mut p: (mut Slice[i64], mut Slice[i64]) = v.split_at_mut(2i64);\n\
                 println(f\"{p.0.len()} {p.1.len()}\");\n\
             }"
        )
        .trim(),
        "2 0"
    );
}

#[test]
fn test_loop_break_dominated_init_oracle() {
    // Oracle twin of `tests/codegen.rs`'s
    // `test_e2e_loop_break_dominated_deferred_init` (B-2026-08-17-17): a
    // bare `loop` runs at least once, so an assignment dominating every
    // `break` initializes the deferred binding.
    let out = run("\n\
         fn main() {\n\
             let x: i64;\n\
             loop { x = 1; break; }\n\
             println(x);\n\
             let c = true;\n\
             let mut y: i64;\n\
             outer: loop {\n\
                 loop {\n\
                     if c { y = 9; break outer; }\n\
                     y = 1;\n\
                     break;\n\
                 }\n\
                 y = 4;\n\
                 break;\n\
             }\n\
             println(y);\n\
         }\n");
    assert_eq!(out, "1\n9\n");
}

/// The interpreter's width-sensitive METHOD paths read 128-bit widths
/// (B-2026-08-19-8 stage 3b).
///
/// Stage 1 widened `Value::Int` to i128, but `int_width_at` had no 128 arms, so
/// every one of these methods fell to its signed-64 default and answered for 64
/// bits — while codegen (stage 3a) answered for 128. Nothing could observe the
/// divergence because the type was still rejected at type-check at the time —
/// which is exactly why it needed a test rather than a bug report. The
/// rejection is gone (B-2026-08-19-19), so the 128-bit half is now observable;
/// it is asserted end-to-end in tests/codegen.rs, where both backends can be
/// compared against each other.
///
/// These assert on the WIDTH, not on the arithmetic: each value is chosen so a
/// 64-bit answer and a 128-bit answer differ.
#[test]
fn interpreter_reads_128bit_widths_for_width_sensitive_methods() {
    // `IntW` is private, so the checks go through the public evaluator using
    // i64-typed receivers — the widths that must NOT regress while the 128-bit
    // arms exist. The 128-bit half is covered end-to-end in tests/codegen.rs,
    // where both backends can be compared.
    assert_eq!(
        run_no_errors(
            "fn main() {\n\
             let a: i64 = 9223372036854775807i64;\n\
             println(a.wrapping_add(1i64));\n\
             let b: i64 = 0i64 - 1i64;\n\
             println(b.count_ones());\n\
             let c: i64 = 1i64;\n\
             println(c.rotate_left(1u32));\n\
             println(c.leading_zeros());\n\
             }"
        ),
        "-9223372036854775808\n64\n2\n63\n"
    );
}

#[test]
fn test_fault_in_return_operand_stops_the_program() {
    // `return v[99]` used to be DOWNGRADED to `return ()`: the faulting
    // operand yielded the Unit poison, and `set_cf(Return(poison))`
    // overwrote the RuntimeError marker already in `pending_cf`. The caller
    // then resumed and printed `got ()` — a Unit where an `i64` belongs —
    // from a statement the compiled backends never reach.
    assert_stops_at_fault(
        "fn boom() -> i64 { let v = Vec[1, 2]; return v[99] }\n\
         fn main() { let x = boom(); println(f\"got {x}\"); }\n",
        "out of bounds",
        "got",
    );
}

#[test]
fn test_fault_in_break_operand_stops_the_program() {
    // `break` carries a value through the same funnel, so it downgraded the
    // fault identically. The row named only `return`; this is the shape that
    // showed the defect belongs to `set_cf`, not to one statement.
    assert_stops_at_fault(
        "fn boom() -> i64 {\n\
             let v = Vec[1, 2];\n\
             let r = loop { break v[99] };\n\
             r\n\
         }\n\
         fn main() { let x = boom(); println(f\"got {x}\"); }\n",
        "out of bounds",
        "got",
    );
}

#[test]
fn test_fault_in_returned_call_stops_the_program() {
    // The fault need not be lexically inside the `return` operand: a callee
    // that faults sets the same marker through `call_function`'s propagate
    // arm, and the caller's `return` overwrote it just as readily.
    assert_stops_at_fault(
        "fn inner() -> i64 { let v = Vec[1, 2]; v[99] }\n\
         fn boom() -> i64 { return inner() }\n\
         fn main() { let x = boom(); println(f\"got {x}\"); }\n",
        "out of bounds",
        "got",
    );
}

#[test]
fn test_divide_by_zero_in_return_operand_stops_the_program() {
    // Fault-kind-agnostic: every `record_runtime_error` caller sets the same
    // marker, so pinning only the index-OOB shape would leave the rest of the
    // family free to regress.
    assert_stops_at_fault(
        "fn boom(d: i64) -> i64 { return 10 / d }\n\
         fn main() { let x = boom(0); println(f\"got {x}\"); }\n",
        "division by zero",
        "got",
    );
}

#[test]
fn test_unwrap_of_none_in_return_operand_stops_the_program() {
    assert_stops_at_fault(
        "fn boom() -> i64 { let o: Option[i64] = None; return o.unwrap() }\n\
         fn main() { let x = boom(); println(f\"got {x}\"); }\n",
        "unwrap",
        "got",
    );
}

#[test]
fn test_healthy_return_and_break_still_carry_their_values() {
    // The inverse failure the guard could cause: `set_cf` now declines to
    // write when an unwind is pending, so a stale or over-broad marker would
    // silently swallow ordinary control flow. Both carriers must still work.
    assert_eq!(
        run_no_errors(
            "fn pick() -> i64 { let v = Vec[7, 8]; return v[1] }\n\
             fn main() { println(pick()); }\n"
        ),
        "8\n"
    );
    // The `break` carrier is exercised through a `let` binding rather than a
    // tail-position `loop`: a tail `loop` drops its break value in BOTH
    // backends (filed separately — it predates this fix and is not a
    // run-vs-build divergence), which would make this test fail for an
    // unrelated reason.
    assert_eq!(
        run_no_errors(
            "fn pick() -> i64 {\n\
                 let mut i = 0;\n\
                 let r = loop { i = i + 1; if i == 3 { break i * 10 } };\n\
                 r\n\
             }\n\
             fn main() { println(pick()); }\n"
        ),
        "30\n"
    );
}

/// A user `struct StableHash` shadows the built-in namespace, and the built-in
/// arm must REFUSE rather than quietly answer for it (B-2026-08-02-13 class).
///
/// Measured before the guard: a user `siphash24` whose body returned `42`
/// printed the SipHash digest instead, on the interpreter AND on AOT. Stdlib
/// and user types share one flat namespace, so the shadowing program
/// typechecks — nothing upstream catches it. For a hash function that is the
/// worst shape of the bug: the wrong answer is a plausible `u64` a caller may
/// go on to store as a content address.
///
/// `codegen/assoc_call.rs` refuses the same program via
/// `reject_shadowed_prelude_types`, so the two backends agree about it.
#[test]
fn stable_hash_refuses_to_answer_for_a_user_shadowed_namespace() {
    let errors = runtime_errors(
        "struct StableHash { }\n\
         impl StableHash {\n\
             fn siphash24(bytes: Slice[u8], k0: u64, k1: u64) -> u64 { 42 }\n\
         }\n\
         fn main() {\n\
             let v: Vec[u8] = [1u8, 2u8, 3u8];\n\
             println(StableHash.siphash24(v, 0u64, 0u64));\n\
         }\n",
    );
    assert!(
        errors.iter().any(
            |e| e.message.contains("declares its own `struct StableHash`")
                && e.message.contains("Rename")
        ),
        "expected a rename diagnostic for the shadowed namespace, got: {errors:?}"
    );
}

/// B-2026-08-31-46 — a fresh-temp or named argument escaping inside a returned
/// `Option`/`Result` CONSTRUCTOR runs its `Drop` body exactly once, on every
/// surface, in every spelling.
///
/// Before this the compiled backends ran the escaping call's body TWICE in all
/// four spellings (method/free × fresh-temp/named) while `--interp` was right
/// only for the method+fresh-temp one; the named and free spellings doubled on
/// both backends. The three-call control in `S1` (`false`, `true`, `false`) is
/// the load-bearing cell: the row's own analysis said the obvious fix — seeing
/// through the ctor in `fn_returns_param` — would skip the caller-side drop on
/// the two `false` calls too, turning one doubled body into two LOST ones. This
/// pin holds all three at exactly one.
///
/// `Rok`/`Rer` cover `Result.Ok` and `Result.Err` (the latter takes the
/// error-path drain); `tail` is the arm-tail spelling, which reaches the
/// per-path flag through `arm_conditional_move_tail_flag` rather than the
/// `return` handler. One string across all four surfaces.
/// B-2026-09-05-10 — an UNCONDITIONAL `return Option.Some(r)` (and the `Result`
/// and nested-aggregate spellings) of a by-value param runs the `Drop` body
/// ONCE. This is the DIRECT cell of that row; before this it ran twice on every
/// surface (the caller's fresh-temp walk AND the discarded result binding), a
/// double no A/B gate sees because all four surfaces agreed.
///
/// The fix is `fn_always_returns_param`'s `yields` gaining the constructor arm
/// `option_result_ctor_payload` already provides — safe on this ALL-paths
/// predicate (a true answer stands the caller down only where the result
/// binding owns the value on EVERY path) where the same widening on the union
/// `fn_returns_param` was not (B-2026-08-31-46's trap). The REBOUND spelling
/// (`let m = r; return Option.Some(m)`) is a separate hole — the predicates are
/// name-keyed and blind to the alias, and standing the caller down there needs
/// the callee flip to drop the alias on the non-escaping path — split to its
/// own row.
#[test]
fn test_unconditional_ctor_return_of_param_runs_one_body() {
    assert_eq!(
        run(r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"drop {self.id} {self.name}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}" }; }
fn top(r: R) -> Option[R] { return Option.Some(r); }
fn topwrap(r: R) -> Result[R, String] { return Result.Ok(r); }
fn main() {
    println("top");    let _ = top(mk(1));
    println("wrap");   let _ = topwrap(mk(2));
    println("done")
}"#),
        "top\ndrop 1 h1\nwrap\ndrop 2 h2\ndone\n",
        "an unconditional ctor-wrapped return of a param runs one body"
    );
}

#[test]
fn test_ctor_wrapped_conditional_return_runs_one_body() {
    assert_eq!(
        run(r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"drop {self.id} {self.name}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}" }; }
struct K { n: i64 }
impl K {
    fn f(ref self, r: R, keep: bool) -> Option[R] { if keep { return Option.Some(r); } return Option.None; }
    fn ok(ref self, r: R, keep: bool) -> Result[R, i64] { if keep { return Result.Ok(r); } return Result.Err(0); }
    fn er(ref self, r: R, keep: bool) -> Result[i64, R] { if keep { return Result.Err(r); } return Result.Ok(0); }
    fn t(ref self, r: R, keep: bool) -> Option[R] { if keep { Option.Some(r) } else { Option.None } }
}
fn freefn_opt(r: R, keep: bool) -> Option[R] { if keep { return Option.Some(r); } return Option.None; }
fn ft(r: R, keep: bool) -> Option[R] { if keep { Option.Some(r) } else { Option.None } }
fn main() {
    let k = K { n: 0 };
    println("S1"); let _ = k.f(mk(3), false); let _ = k.f(mk(4), true); let _ = k.f(mk(5), false);
    println("S2"); let a1 = mk(14); let _ = k.f(a1, true);
    println("S3"); let _ = freefn_opt(mk(24), true);
    println("S4"); let a2 = mk(34); let _ = freefn_opt(a2, true);
    println("Rok"); let _ = k.ok(mk(41), false); let _ = k.ok(mk(42), true);
    println("Rer"); let _ = k.er(mk(43), false); let _ = k.er(mk(44), true);
    println("tail"); let _ = k.t(mk(51), false); let _ = k.t(mk(52), true); let _ = ft(mk(53), false); let _ = ft(mk(54), true);
    println("done")
}
"#),
        "S1\ndrop 3 h3\ndrop 4 h4\ndrop 5 h5\nS2\ndrop 14 h14\nS3\ndrop 24 h24\nS4\ndrop 34 h34\nRok\ndrop 41 h41\ndrop 42 h42\nRer\ndrop 43 h43\ndrop 44 h44\ntail\ndrop 51 h51\ndrop 52 h52\ndrop 53 h53\ndrop 54 h54\ndone\n",
        "one body per call, every spelling, ctor-wrapped conditional return"
    );
}

/// B-2026-09-06-13 — the INTERPRETER twin of
/// `e2e_param_handed_to_conditionally_returning_callee_runs_one_body`, same
/// program and the same expected string; the shape was agreed-wrong on all
/// four surfaces, and both backends read the one new predicate.
#[test]
fn test_param_handed_to_conditionally_returning_callee_runs_one_body() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] }; }
fn keepc(r: R, k: bool) -> R { if k { return r; } return mk(99); }
fn keepo(r: R, k: bool) -> Option[R] { if k { return Option.Some(r); } return Option.None; }
fn s_cond(r: R, k: bool) { let w: R = keepc(r, k); println(f"sc {w.id}") }
fn s_cond_unread(r: R, k: bool) { let w: R = keepc(r, k); println("scu") }
fn s_cond_disc(r: R, k: bool) { let _ = keepc(r, k); println("scd") }
fn s_cond_stmt(r: R, k: bool) { keepc(r, k); println("scs") }
fn s_cond_ret(r: R, k: bool) -> R { let w: R = keepc(r, k); return w; }
fn s_cond_direct(r: R, k: bool) -> R { return keepc(r, k); }
fn s_cond_opt(r: R, k: bool) { let o: Option[R] = keepo(r, k); match o { Option.Some(x) => println(f"so {x.id}"), Option.None => println("so none") } }
fn s_cond_nested(r: R, k: bool, j: bool) { if j { let w: R = keepc(r, k); println(f"scn {w.id}"); } println("scn-out") }
fn s_local(k: bool) { let l = mk(50); let w: R = keepc(l, k); println(f"sl {w.id}") }
struct K { n: i64 }
impl K {
    fn m_cond(ref self, r: R, k: bool) { let w: R = keepc(r, k); println(f"mc {w.id}") }
}
fn main() {
    let k = K { n: 0 };
    println("one-t"); s_cond(mk(1), true);
    println("two-f"); s_cond(mk(2), false);
    println("three-t"); s_cond_unread(mk(3), true);
    println("four-f"); s_cond_unread(mk(4), false);
    println("five-t"); s_cond_disc(mk(5), true);
    println("six-f"); s_cond_disc(mk(6), false);
    println("seven-t"); s_cond_stmt(mk(7), true);
    println("eight-f"); s_cond_stmt(mk(8), false);
    println("nine-t"); let a = s_cond_ret(mk(9), true); println(f"got {a.id}");
    println("ten-f"); let b = s_cond_ret(mk(10), false); println(f"got {b.id}");
    println("eleven-t"); let c = s_cond_direct(mk(11), true); println(f"got {c.id}");
    println("twelve-f"); let d = s_cond_direct(mk(12), false); println(f"got {d.id}");
    println("thirteen-t"); s_cond_opt(mk(13), true);
    println("fourteen-f"); s_cond_opt(mk(14), false);
    println("fifteen-tt"); s_cond_nested(mk(15), true, true);
    println("sixteen-ft"); s_cond_nested(mk(16), false, true);
    println("seventeen-tf"); s_cond_nested(mk(17), true, false);
    println("eighteen-lt"); s_local(true);
    println("nineteen-lf"); s_local(false);
    println("twenty-mt"); k.m_cond(mk(20), true);
    println("twentyone-mf"); k.m_cond(mk(21), false);
    println("twentytwo-nt"); let e = mk(22); s_cond(e, true);
    println("twentythree-nf"); let f = mk(23); s_cond(f, false);
    println("end");
}"#),
        "one-t\nsc 1\ndR1\ntwo-f\ndR2\nsc 99\ndR99\nthree-t\ndR3\nscu\nfour-f\ndR4\ndR99\nscu\nfive-t\ndR5\nscd\nsix-f\ndR6\ndR99\nscd\nseven-t\ndR7\nscs\neight-f\ndR8\ndR99\nscs\nnine-t\ngot 9\ndR9\nten-f\ndR10\ngot 99\ndR99\neleven-t\ngot 11\ndR11\ntwelve-f\ndR12\ngot 99\ndR99\nthirteen-t\nso 13\ndR13\nfourteen-f\ndR14\nso none\nfifteen-tt\nscn 15\ndR15\nscn-out\nsixteen-ft\ndR16\nscn 99\ndR99\nscn-out\nseventeen-tf\nscn-out\ndR17\neighteen-lt\nsl 50\ndR50\nnineteen-lf\ndR50\nsl 99\ndR99\ntwenty-mt\nmc 20\ndR20\ntwentyone-mf\ndR21\nmc 99\ndR99\ntwentytwo-nt\nsc 22\ndR22\ntwentythree-nf\ndR23\nsc 99\ndR99\nend\n",
        "a param handed to a conditionally-returning callee has one owner per path"
    );
}

/// B-2026-09-06-44 — the backend that was already right: the caller-retained
/// walk runs each field's body once after the call whether the callee reads
/// a field off the param by projection or by destructure. Pinned so the
/// compiled twin's `$keep`-walk fix stays measured against it.
///
/// Twin of `tests/codegen.rs`'s `e2e_projection_off_a_param_runs_the_sibling_body_once`, pinned to the same string.
#[test]
fn test_projection_off_a_param_runs_the_sibling_body_once() {
    assert_eq!(
        run(r#"struct R { id: i64, name: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"n{i}", xs: [i] }; }
struct S3 { a: R, b: R }
struct T { r: R, v: Vec[i64] }

fn proj_none(s: S3) -> i64 { let a = s.a; println("  mid"); return 1; }
fn proj_ret(s: S3) -> i64 { let a = s.a; println("  mid"); return a.id; }
fn proj_b(s: S3) -> i64 { let b = s.b; println("  mid"); return b.id; }
fn proj_both(s: S3) -> i64 { let a = s.a; let b = s.b; println("  mid"); return a.id + b.id; }
fn proj_reads(s: S3) -> i64 { let a = s.a; println(f"  m{a.id}"); let b = s.b; println(f"  m{b.id}"); return 1; }
fn proj_whole(s: S3) -> R { let a = s.a; println("  mid"); return a; }
fn proj_heap(t: T) -> i64 { let r = t.r; println("  mid"); return r.id + t.v.len(); }
fn destr(s: S3) -> i64 { let S3 { a, b } = s; println("  mid"); return a.id; }
impl S3 { fn m(self) -> i64 { let a = self.a; println("  mid"); return a.id; } }

fn main() {
    println("proj_none"); let v1 = proj_none(S3 { a: mk(1), b: mk(2) }); println(f"  v={v1}");
    println("proj_ret"); let v2 = proj_ret(S3 { a: mk(3), b: mk(4) }); println(f"  v={v2}");
    println("proj_b"); let v3 = proj_b(S3 { a: mk(5), b: mk(6) }); println(f"  v={v3}");
    println("proj_both"); let v4 = proj_both(S3 { a: mk(7), b: mk(8) }); println(f"  v={v4}");
    println("proj_reads"); let v5 = proj_reads(S3 { a: mk(9), b: mk(10) }); println(f"  v={v5}");
    println("proj_whole"); let r6 = proj_whole(S3 { a: mk(11), b: mk(12) }); println(f"  v={r6.id}");
    println("proj_heap"); let v7 = proj_heap(T { r: mk(13), v: [1, 2] }); println(f"  v={v7}");
    println("self_root"); let v8 = S3 { a: mk(14), b: mk(15) }.m(); println(f"  v={v8}");
    println("named"); let s9 = S3 { a: mk(16), b: mk(17) }; let v9 = proj_none(s9); println(f"  v={v9}");
    println("destr"); let v10 = destr(S3 { a: mk(18), b: mk(19) }); println(f"  v={v10}");
    println("end");
}
"#),
        r#"proj_none
  mid
  dR2
  dR1
  v=1
proj_ret
  mid
  dR4
  dR3
  v=3
proj_b
  mid
  dR6
  dR5
  v=6
proj_both
  mid
  dR8
  dR7
  v=15
proj_reads
  m9
  m10
  dR10
  dR9
  v=1
proj_whole
  mid
  dR12
  v=11
  dR11
proj_heap
  mid
  dR13
  v=15
self_root
  mid
  dR15
  dR14
  v=14
named
  mid
  dR17
  dR16
  v=1
destr
  mid
  dR19
  dR18
  v=18
end
"#
    );
}

/// B-2026-09-06-26 — a free function whose arm RETURNS A SCALAR LEAF of an
/// owned-param struct destructure ran NO `Drop` body at all under `--interp`:
/// `fn p_bare(h: H2) -> i64 { match h { H2 { e, n } => { return n; } } }` printed
/// `x100` alone against `dE dR3 x100` on jit / -O0 / -O2, and so did every
/// spelling that mentioned the `i64` leaf on the way out (`r.id + n`, a rebind
/// `let z = n * 2`, a renamed field `n: k`, `if let`, a `bool`/`f64` leaf, a
/// returned `String` leaf). The caller's stand-down is `record_passthrough_arg_moves`
/// → `fn_returns_param_payload_of`, whose scanner counted every name the arm binds
/// and asked only whether it LEAVES the frame — an `i64` leaving proves nothing
/// about the argument's bodies, and for a plain-struct pattern there is one
/// "variant" (the struct itself), so the whole walk over the argument stood down.
/// The arithmetic spellings left because, by the time the interpreter runs,
/// `lower_program` has rewritten `r.id + n` into `i64.add(r.id, n)`: a `Call` with
/// a `Path` callee, which the scanner's "unknown callee keeps the escape" fallback
/// counted as taking `n` over.
///
/// Fixed in the scanner (`escaping_param_payload_variants_impl`): a lowered
/// primitive operator (`consume_class::is_lowered_primitive_operator`) never takes
/// an argument over, and — program-aware — a leaf whose DECLARED type cannot carry
/// a user `Drop` body (`payload_names_that_can_carry_a_body`: scalar primitives,
/// unit, `String`) is dropped from the arm's names before the escape question is
/// asked. The compiled backends never asked this predicate of a struct argument,
/// which is why they were right on every struct cell; the ENUM cells are where
/// both backends consult it, and `sk/S` (`E3.S { k, r } => k`, `k: i64`) is the
/// one that moved on every surface: reporting `S` masked `r`'s body out of the
/// walk for a value that never left (`dE3 x32`, no `dR132`, agreed-and-wrong), and
/// now runs it. `ecall/B` / `earith/B` pin that a scalar-payload variant still
/// hands nothing back. `read` / `slen` are the controls that were right throughout.
///
/// Twin of `tests/codegen.rs`'s `e2e_scalar_leaf_return_keeps_the_arg_walk`, pinned to the same string.
#[test]
fn test_scalar_leaf_return_keeps_the_arg_walk() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("  dE") } }
struct H2 { e: E, n: i64 }
struct Hs { e: E, s: String }
struct Hb { e: E, b: bool, f: f64 }
enum E2 { A(R), B(i64) }
impl Drop for E2 { fn drop(mut ref self) { println("  dE2") } }
enum E3 { A(R), S { k: i64, r: R } }
impl Drop for E3 { fn drop(mut ref self) { println("  dE3") } }
fn consume(x: R) -> i64 { return x.id }

fn p_sum(h: H2) -> i64 { match h { H2 { e, n } => { match e { E.A(r) => { return r.id + n; } E.B => { return n; } } } } }
fn p_bare(h: H2) -> i64 { match h { H2 { e, n } => { return n; } } }
fn p_ren(h: H2) -> i64 { match h { H2 { e, n: k } => { return k; } } }
fn p_iflet(h: H2) -> i64 { if let H2 { e, n } = h { return n; } else { return 0; } }
fn p_alias(h: H2) -> i64 { match h { H2 { e, n } => { let z = n * 2; match e { E.A(r) => { return r.id + z; } E.B => { return 0; } } } } }
fn p_bf(h: Hb) -> f64 { match h { Hb { e, b, f } => { if b { return f; } return 0.0; } } }
fn p_s(h: Hs) -> String { match h { Hs { e, s } => { return s; } } }
fn p_slen(h: Hs) -> i64 { match h { Hs { e, s } => { return s.len(); } } }
fn p_read(h: H2) -> i64 { match h { H2 { e, n } => { match e { E.A(r) => { return r.id; } E.B => { return 0; } } } } }
fn e_call(b: E2) -> i64 { match b { E2.A(r) => { return consume(r); } E2.B(k) => { return k; } } }
fn e_arith(b: E2) -> i64 { match b { E2.A(r) => { return r.id * 2; } E2.B(k) => { return k + 1; } } }
fn s_k(b: E3) -> i64 { match b { E3.A(r) => { return r.id; } E3.S { k, r } => { return k; } } }

fn main() {
    println("sum/local"); let a1 = H2 { e: E.A(mk(1)), n: 100 }; let x1 = p_sum(a1); println(f"  x{x1}");
    println("sum/temp"); let x2 = p_sum(H2 { e: E.A(mk(2)), n: 100 }); println(f"  x{x2}");
    println("bare/local"); let a3 = H2 { e: E.A(mk(3)), n: 100 }; let x3 = p_bare(a3); println(f"  x{x3}");
    println("bare/temp"); let x4 = p_bare(H2 { e: E.A(mk(4)), n: 100 }); println(f"  x{x4}");
    println("ren/local"); let a5 = H2 { e: E.A(mk(5)), n: 100 }; let x5 = p_ren(a5); println(f"  x{x5}");
    println("iflet/local"); let a6 = H2 { e: E.A(mk(6)), n: 100 }; let x6 = p_iflet(a6); println(f"  x{x6}");
    println("alias/local"); let a7 = H2 { e: E.A(mk(7)), n: 100 }; let x7 = p_alias(a7); println(f"  x{x7}");
    println("bf/local"); let a8 = Hb { e: E.A(mk(8)), b: true, f: 2.5 }; let x8 = p_bf(a8); println(f"  x{x8}");
    println("s/local"); let a9 = Hs { e: E.A(mk(9)), s: "abc".to_string() }; let x9 = p_s(a9); println(f"  x{x9}");
    println("slen/local"); let a10 = Hs { e: E.A(mk(10)), s: "abcd".to_string() }; let x10 = p_slen(a10); println(f"  x{x10}");
    println("read/local"); let a11 = H2 { e: E.A(mk(11)), n: 100 }; let x11 = p_read(a11); println(f"  x{x11}");
    println("ecall/A"); let b1 = E2.A(mk(21)); let y1 = e_call(b1); println(f"  x{y1}");
    println("ecall/B"); let b2 = E2.B(22); let y2 = e_call(b2); println(f"  x{y2}");
    println("earith/A"); let b3 = E2.A(mk(23)); let y3 = e_arith(b3); println(f"  x{y3}");
    println("earith/B"); let b4 = E2.B(24); let y4 = e_arith(b4); println(f"  x{y4}");
    println("sk/A"); let c1 = E3.A(mk(31)); let z1 = s_k(c1); println(f"  x{z1}");
    println("sk/S"); let c2 = E3.S { k: 32, r: mk(132) }; let z2 = s_k(c2); println(f"  x{z2}");
    println("sk/S-temp"); let z3 = s_k(E3.S { k: 33, r: mk(133) }); println(f"  x{z3}");
    println("end");
}
"#),
        r#"sum/local
  dE
  dR1
  x101
sum/temp
  dE
  dR2
  x102
bare/local
  dE
  dR3
  x100
bare/temp
  dE
  dR4
  x100
ren/local
  dE
  dR5
  x100
iflet/local
  dE
  dR6
  x100
alias/local
  dE
  dR7
  x207
bf/local
  dE
  dR8
  x2.5
s/local
  dE
  dR9
  xabc
slen/local
  dE
  dR10
  x4
read/local
  dE
  dR11
  x11
ecall/A
  dE2
  dR21
  x21
ecall/B
  dE2
  x22
earith/A
  dE2
  dR23
  x46
earith/B
  dE2
  x25
sk/A
  dE3
  dR31
  x31
sk/S
  dE3
  dR132
  x32
sk/S-temp
  dE3
  dR133
  x33
end
"#
    );
}
