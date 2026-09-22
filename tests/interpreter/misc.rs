//! everything the area rules do not claim -- fixtures for `tests/interpreter.rs`.
//!
//! Split out of `tests/interpreter.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test interpreter` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test interpreter misc::
//!
//! New fixtures about everything the area rules do not claim belong in this file.

use super::*;

/// The interpreter oracle for B-2026-08-30-3's codegen twin
/// (`test_e2e_branch_tail_fstring_arm_values_round_trip`).
///
/// The bug was a codegen-only LEAK, so the interpreter never had it — which is
/// exactly what makes this worth pinning. The fix teaches seven consuming gates
/// to free an f-string arm tail at the use site, and the way that goes wrong is
/// freeing a value someone still reads. This side has no frees to get wrong, so
/// it is the reference the compiled backends must keep matching.
/// B-2026-08-30-23 — the interpreter twin of
/// `codegen_conditional_return_param_drop_matrix` (tests/codegen.rs). Same
/// program, same absolute expectation, so the two backends are pinned to one
/// answer rather than to each other.
///
/// ALL TWELVE CELLS ARE PINNED since B-2026-09-01-44. Two were held out here
/// while the ASSOCIATED spelling was wrong under `--interp` in both directions
/// — `H.apick(R { .. }, true)` ran NO body for the dying argument and
/// `H.apick(r, false)` ran TWO for the returned one, against correct compiled
/// lanes. The cause was not a missing protocol but a missing NAME: an
/// associated call is dispatched by `eval_call`'s free-function arm under the
/// callee's BARE name, so the helpers there that scan `program.items` for an
/// `Item::Function` found nothing. `callee_fn_for_param_ownership` resolves it,
/// and the associated spelling now matches the free-function one exactly, which
/// is what the neighbouring rows of this table assert.
/// B-2026-09-07-24 — A `u64` AT OR ABOVE 2^63 IN A WIDTH-SPEC'D f-STRING HOLE
/// MUST RENDER UNSIGNED UNDER THE INTERPRETER, as it already did on both
/// compiled backends.
///
/// The carrier is a SIGNED `Value::Int(i128)` and `truncate_to_width` stores a
/// `u64` at or above 2^63 WRAPPED — the i64 reinterpretation — so reading it
/// back through `FormatSpec::apply_int` printed `u64::MAX` as `-1`. The
/// UNSPEC'D spelling `f"{x}"` was already correct (B-2026-08-19-27), and so was
/// `f"{x:x}"`, because a non-decimal radix reinterprets as unsigned anyway;
/// only the DECIMAL spec'd hole diverged, which is what kept it narrow enough
/// to survive this long.
///
/// `u8`/`u32` are here to pin the other half: they fit the signed carrier
/// non-negatively, so they must keep rendering through the signed path
/// unchanged.
#[test]
fn test_interp_unsigned_spec_hole_renders_unsigned() {
    let out = run(r#"
fn main() {
    let ubig: u64 = 18446744073709551615u64;
    let umid: u64 = 9223372036854775808u64;
    let usmall: u64 = 42u64;
    let u32v: u32 = 4294967295;
    let u8v: u8 = 255;
    let sneg: i64 = -7;
    println(f"[{ubig:22}][{umid:22}][{usmall:22}]");
    println(f"[{ubig:<24}][{umid:>24}][{ubig:026}]");
    println(f"[{u32v:14}][{u8v:6}][{sneg:6}][{sneg:06}]");
    println(f"[{ubig:x}][{umid:x}]");
}
"#);
    assert_eq!(
        out,
        "[  18446744073709551615][   9223372036854775808][                    42]\n\
         [18446744073709551615    ][     9223372036854775808][00000018446744073709551615]\n\
         [    4294967295][   255][    -7][-00007]\n\
         [ffffffffffffffff][8000000000000000]\n"
    );
}

/// B-2026-09-07-35 — A SPEC'D 128-BIT f-STRING HOLE MUST RENDER LIKE THE
/// COMPILED BACKENDS, which for `u128`/`i128` meant three different wrong
/// answers before this, only one of them loud.
///
///  * A magnitude too wide for `i64` reached `narrow_to_i64` — which PANICS by
///    design rather than truncate silently — so `f"{big:44}"` ABORTED the
///    interpreter outright while all three compiled legs printed it.
///  * A `u128` whose i128 reinterpretation FITS `i64` never reached that panic
///    and printed a NEGATIVE number instead: `u128::MAX` rendered `-1`. That is
///    B-2026-09-07-24 exactly, one width over, and it was silent.
///  * An `i128` under a NON-DECIMAL radix reinterprets at the hole's own width,
///    so reading it at 64 bits gave sixteen f's for `{-1i128:x}` where every
///    compiled backend gives thirty-two.
///
/// The last four holes are the CONTROLS that make the fix falsifiable: a `u64`
/// hole, an `i64` hole under `:x`, and a `u64` hole under `:x` must all keep
/// their 64-bit readings. Widening them along with the 128-bit ones is the way
/// this fix would go wrong — `{-1i64:x}` staying sixteen f's while
/// `{-1i128:x}` becomes thirty-two is the whole point.
///
/// Expected output is the COMPILED oracle: `karac build` (auto-par default),
/// `KARAC_AUTO_PAR=0 karac build` and `karac run` (JIT) all produce these exact
/// bytes, and this asserts the interpreter now joins them.
#[test]
fn test_interp_spec_128_bit_hole_matches_the_compiled_backends() {
    let out = run(r#"
fn main() {
    let umax: u128 = 340282366920938463463374607431768211455u128;
    let umid: u128 = 170141183460469231731687303715884105728u128;
    let usml: u128 = 42u128;
    let imax: i128 = 170141183460469231731687303715884105727i128;
    let imin: i128 = -170141183460469231731687303715884105728i128;
    let ineg: i128 = -1i128;
    let isml: i128 = -7i128;
    let p100: i128 = 1267650600228229401496703205376i128;
    println(f"[{umax:44}][{umid:44}][{usml:44}]");
    println(f"[{umax:<42}][{usml:06}][{p100:34}]");
    println(f"[{imax:44}][{imin:44}][{isml:08}]");
    println(f"[{ineg:x}]");
    println(f"[{ineg:o}]");
    println(f"[{umax:x}][{umid:X}]");
    let u64v: u64 = 18446744073709551615u64;
    let i64v: i64 = -1;
    println(f"[{u64v:22}][{i64v:x}][{u64v:x}]");
}
"#);
    assert_eq!(
        out,
        "[     340282366920938463463374607431768211455][     170141183460469231731687303715884105728][                                          42]\n\
         [340282366920938463463374607431768211455   ][000042][   1267650600228229401496703205376]\n\
         [     170141183460469231731687303715884105727][    -170141183460469231731687303715884105728][-0000007]\n\
         [ffffffffffffffffffffffffffffffff]\n\
         [3777777777777777777777777777777777777777777]\n\
         [ffffffffffffffffffffffffffffffff][80000000000000000000000000000000]\n\
         [  18446744073709551615][ffffffffffffffff][ffffffffffffffff]\n"
    );
}

#[test]
fn test_faulted_method_receiver_reports_the_fault_not_an_ice() {
    // B-2026-08-09-18: a method call whose RECEIVER faulted used to ICE with
    // `internal error: entered unreachable code: len() receiver at L:C was
    // Value::Unit; either an interpreter codepath produced the wrong receiver
    // variant or the typechecker accepted .len() on a type without one`. Both
    // of the assertion's hypotheses were wrong — the typechecker was right and
    // no codepath produced a wrong variant. The fault set `pending_cf` and
    // yielded the `Unit` poison, and the receiver was dispatched on without
    // anyone checking first.
    //
    // Reaching these assertions at all proves the no-ICE half: the old failure
    // was a Rust `unreachable!`, which aborts the test before `errors` can be
    // inspected.
    //
    // Two different FAULTS in the receiver position (index OOB and unwrap of
    // `None`) and two different dispatch ARMS (`len`, `chars`) — each arm
    // carries its own receiver assertion, so covering one proves nothing about
    // the next. `is_empty` is here as the converse control: its arm falls
    // through to a tolerant default rather than asserting, so it reported the
    // fault correctly even before the fix and must still do so after.
    for (label, src, want) in [
        (
            "index OOB, `len` arm — the row's repro",
            "fn main() {\n\
                 let mut v: Vec[Vec[i64]] = Vec.new();\n\
                 let n = v[3].len();\n\
                 println(f\"{n}\");\n\
             }",
            "index 3 out of bounds",
        ),
        (
            "unwrap of None, `len` arm — a different fault, same position",
            "fn main() {\n\
                 let o: Option[String] = Option.None;\n\
                 let n = o.unwrap().len();\n\
                 println(f\"{n}\");\n\
             }",
            "unwrap() on None",
        ),
        (
            "index OOB, `chars` arm — a second assertion site",
            "fn main() {\n\
                 let mut v: Vec[String] = Vec.new();\n\
                 for c in v[2].chars() { println(f\"{c}\"); }\n\
             }",
            "index 2 out of bounds",
        ),
        (
            "index OOB, `is_empty` arm — tolerant-arm control",
            "fn main() {\n\
                 let mut v: Vec[Vec[i64]] = Vec.new();\n\
                 let b = v[1].is_empty();\n\
                 println(f\"{b}\");\n\
             }",
            "index 1 out of bounds",
        ),
    ] {
        let errors = runtime_errors(src);
        assert!(
            errors.iter().any(|e| e.message.contains(want)),
            "{label}: expected the receiver's own fault ({want:?}), got: {:?}",
            errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
        assert!(
            !errors
                .iter()
                .any(|e| e.message.contains("internal") || e.message.contains("compiler bug")),
            "{label}: a faulted receiver is a program error, not a compiler bug, got: {:?}",
            errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }
}

#[test]
fn test_faulted_short_circuit_operand_reports_the_fault_not_an_ice() {
    // B-2026-08-09-19, the sibling B-2026-08-09-18's fix did NOT close. That
    // row guarded the method-call receiver; a faulted operand still ICEd in
    // every position that consumes a Bool, with the same shape — an
    // `unreachable!` whose message blames the typechecker or a wrong-variant
    // codepath when in fact the operand faulted, set `pending_cf`, and yielded
    // the `Unit` poison that is then asserted against.
    //
    // `eval_short_circuit` needed its own guard rather than inheriting the
    // operand check B-2026-07-15-7 added: that one lives in the
    // NON-short-circuit `Binary` evaluator, and `and`/`or` route away from it
    // precisely so the RHS stays unevaluated.
    //
    // Case 4 is the MATCH GUARD, which the row does not list. It consumes its
    // Bool through the same `is_truthy` helper as the two condition sites, so
    // hoisting the check there covered it for free — and leaving it would have
    // been a fourth finding of one root cause.
    //
    // Case 5 is the program that actually exposed the row: kata #251's
    // skip-empty guard with its operands swapped. It is here because the
    // minimised repro filed for B-2026-08-09-18 was fixed while this shape
    // still ICEd, which is the whole reason there is a second row.
    for (label, src, want) in [
        (
            "`and` LHS faulted",
            "fn main() {\n\
                 let mut v: Vec[i64] = Vec.new();\n\
                 if v[3] > 0i64 and true { println(\"y\"); }\n\
             }",
            "index 3 out of bounds",
        ),
        (
            "`or` LHS faulted — the other operator, same site",
            "fn main() {\n\
                 let mut v: Vec[i64] = Vec.new();\n\
                 if v[3] > 0i64 or true { println(\"y\"); }\n\
             }",
            "index 3 out of bounds",
        ),
        (
            "`and` RHS faulted — escapes the short-circuit into the if-condition",
            "fn main() {\n\
                 let mut v: Vec[i64] = Vec.new();\n\
                 if true and v[3] > 0i64 { println(\"y\"); }\n\
             }",
            "index 3 out of bounds",
        ),
        (
            "match GUARD faulted — unreported, covered by the same hoist",
            "fn main() {\n\
                 let mut v: Vec[i64] = Vec.new();\n\
                 let n: i64 = 1i64;\n\
                 match n { x if v[3] > 0i64 => { println(\"a\"); } _ => { println(\"b\"); } }\n\
             }",
            "index 3 out of bounds",
        ),
        (
            "while condition, kata #251's own shape — the case that exposed the row",
            "struct It { data: Vec[Vec[i64]], row: i64, col: i64 }\n\
             fn main() {\n\
                 let it: It = It { data: Vec.new(), row: 0i64, col: 0i64 };\n\
                 while it.col >= it.data[it.row].len() and it.row < it.data.len() {\n\
                     println(\"skip\");\n\
                 }\n\
             }",
            "index 0 out of bounds",
        ),
    ] {
        let errors = runtime_errors(src);
        assert!(
            errors.iter().any(|e| e.message.contains(want)),
            "{label}: expected the operand's own fault ({want:?}), got: {:?}",
            errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
        assert!(
            !errors
                .iter()
                .any(|e| e.message.contains("internal") || e.message.contains("compiler bug")),
            "{label}: a faulted operand is a program error, not a compiler bug, got: {:?}",
            errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }
}

#[test]
fn test_short_circuit_still_skips_its_rhs_and_evaluates_healthy_operands() {
    // The converse of the guard above, and the two things it could break.
    //
    // First: the guard returns EARLY from `eval_short_circuit` on a faulted
    // LHS, so it must not disturb the short-circuit itself — a false `and` and
    // a true `or` still leave the RHS unevaluated. The RHS here would fault if
    // it ran, so the assertion is that the program completes at all.
    //
    // Second: `is_truthy` now answers `false` whenever a fault is pending, so
    // every condition reached with NO pending fault must still decide normally
    // — including one whose operands are themselves fallible and succeeded.
    assert_eq!(
        run_no_errors(
            "fn main() {\n\
                 let mut v: Vec[i64] = Vec.new();\n\
                 if false and v[3] > 0i64 { println(\"unreachable-and\"); }\n\
                 if true or v[3] > 0i64 { println(\"or-short-circuited\"); }\n\
                 v.push(5i64);\n\
                 if v[0] > 0i64 and v.len() == 1 { println(\"healthy-and\"); }\n\
                 let n: i64 = 1i64;\n\
                 match n { x if v[0] > 0i64 => { println(\"healthy-guard\"); } _ => { println(\"no\"); } }\n\
             }"
        )
        .trim(),
        "or-short-circuited\nhealthy-and\nhealthy-guard"
    );
}

// ── Arithmetic & Expressions ───────────────────────────────────

#[test]
fn test_integer_arithmetic() {
    assert_eq!(run("fn main() { println(1 + 2); }"), "3\n");
}

// ── First-class fn values (parity with codegen B-2026-06-20-1 / -06-21-*) ──
// The tree-walking interpreter already handles named fn values as args, as
// `let` bindings, and as return values — these lock that in as regression
// cover so `karac run` and `karac build` stay at parity on the surface the
// codegen slices closed.

#[test]
fn fn_value_passed_to_param() {
    assert_eq!(
        run("fn doubler(n: i64) -> i64 { n * 2 }\n\
             fn apply(f: Fn(i64) -> i64, x: i64) -> i64 { f(x) }\n\
             fn main() { println(apply(doubler, 21)); }\n"),
        "42\n"
    );
}

#[test]
fn test_abs_signed_int() {
    assert_eq!(run("fn main() { println((-5i64).abs()); }"), "5\n");
    assert_eq!(run("fn main() { println((7i64).abs()); }"), "7\n");
    assert_eq!(run("fn main() { println((0i64).abs()); }"), "0\n");
}

#[test]
fn test_abs_float() {
    assert_eq!(run("fn main() { println((-2.5f64).abs()); }"), "2.5\n");
    assert_eq!(run("fn main() { println((2.5f64).abs()); }"), "2.5\n");
}

#[test]
fn test_bit_intrinsics_width_correct() {
    // count_ones / leading_zeros / trailing_zeros are width-dependent: they count
    // within the receiver's bit width. A signed `iN`'s sign-extended model value
    // is masked to width first (`i8 -1` has 8 set bits, not 64), and the all-zero
    // value has `bits` leading/trailing zeros.
    let out = run("fn main() {\n\
             let b: u8 = 200;\n\
             println(b.count_ones());\n\
             let n: u64 = 1024;\n\
             println(n.leading_zeros());\n\
             println(n.trailing_zeros());\n\
             let z: u8 = 0;\n\
             println(z.leading_zeros());\n\
             println(z.trailing_zeros());\n\
             let neg: i8 = -1;\n\
             println(neg.count_ones());\n\
             let m: i32 = 1;\n\
             println(m.leading_zeros());\n\
             let w: u32 = 0;\n\
             println(w.count_ones());\n\
         }");
    assert_eq!(out, "3\n53\n10\n8\n8\n8\n31\n0\n");
}

#[test]
fn test_bit_permute_and_count_zeros_width_correct() {
    // `count_zeros` (complement of count_ones), `reverse_bits`, and `swap_bytes`
    // are all width-dependent (Rust `iN::*`). `reverse_bits` reverses the
    // receiver's `bits`; `swap_bytes` reverses its bytes (identity on u8/i8).
    // A signed narrow result is sign-extended in the i64 model
    // (`1i8.reverse_bits() == -128`). Codegen mirrors this bit-for-bit
    // (`tests/codegen.rs::e2e_bit_permute_and_count_zeros_width_correct`).
    let out = run("fn main() {\n\
             println((200u8).count_zeros());\n\
             println((255u8).count_zeros());\n\
             println((0i64).count_zeros());\n\
             println((1u8).reverse_bits());\n\
             println((258u16).swap_bytes());\n\
             println((1u32).swap_bytes());\n\
             println((5u8).swap_bytes());\n\
             println((4278255360u32).reverse_bits());\n\
             println((1i8).reverse_bits());\n\
             println((1i32).reverse_bits());\n\
             let big: u64 = 1;\n\
             println(big.reverse_bits());\n\
         }");
    assert_eq!(
        out,
        // Last line: `1u64.reverse_bits()` = 2^63, which MUST print as the
        // unsigned 9223372036854775808 (not the signed -9223372036854775808).
        "5\n0\n64\n128\n513\n16777216\n5\n16711935\n-128\n-2147483648\n9223372036854775808\n"
    );
}

#[test]
fn test_bit_rotate_width_correct() {
    // `rotate_left(n)` / `rotate_right(n)` wrap within the receiver's `bits`
    // (Rust `iN::rotate_*`, amount mod width). Signed-narrow results
    // sign-extend (`1i8.rotate_right(1) == -128`); a `u64` result with the
    // high bit set prints unsigned. Codegen mirrors this via `llvm.fshl`/`fshr`
    // (`tests/codegen.rs::e2e_bit_rotate_width_correct`).
    let out = run("fn main() {\n\
             println((1u8).rotate_left(1));\n\
             println((128u8).rotate_left(1));\n\
             println((1u8).rotate_right(1));\n\
             println((16u32).rotate_right(4));\n\
             println((5u8).rotate_left(8));\n\
             println((1i8).rotate_right(1));\n\
             let big: u64 = 1;\n\
             println(big.rotate_left(63));\n\
         }");
    assert_eq!(out, "2\n1\n128\n1\n5\n-128\n9223372036854775808\n");
}

#[test]
fn test_is_power_of_two_unsigned() {
    // `uN::is_power_of_two` -> bool: true iff exactly one bit is set (0 is NOT a
    // power of two). Unsigned-only. Includes the 2^63 `u64` case (high bit set)
    // and narrow widths (u8/u16). Codegen mirrors this bit-for-bit
    // (`tests/codegen.rs::e2e_is_power_of_two_unsigned`).
    let out = run("fn main() {\n\
             println((1u32).is_power_of_two());\n\
             println((2u32).is_power_of_two());\n\
             println((3u32).is_power_of_two());\n\
             println((0u32).is_power_of_two());\n\
             let big: u64 = 1;\n\
             println(big.rotate_left(63).is_power_of_two());\n\
             println((128u8).is_power_of_two());\n\
             println((129u8).is_power_of_two());\n\
             println((1024u16).is_power_of_two());\n\
         }");
    assert_eq!(out, "true\ntrue\nfalse\nfalse\ntrue\ntrue\nfalse\ntrue\n");
}

#[test]
fn test_abs_diff_unsigned_result() {
    // `iN/uN::abs_diff(other) -> uN`: the absolute difference, always
    // non-negative, never overflows. Signed inputs (incl. i8 MIN/MAX → 255u8
    // and a near-full-i64-range diff that exceeds i64::MAX and MUST print as
    // the unsigned u64) and unsigned inputs are all covered. Codegen mirrors
    // this (`tests/codegen.rs::e2e_abs_diff_unsigned_result`).
    let out = run("fn main() {\n\
             let a: i32 = 5;\n\
             let b: i32 = 3;\n\
             println(a.abs_diff(b));\n\
             println(b.abs_diff(a));\n\
             let n: i32 = -5;\n\
             println(n.abs_diff(b));\n\
             let u: u32 = 3;\n\
             let v: u32 = 10;\n\
             println(u.abs_diff(v));\n\
             let x: u8 = 200;\n\
             let y: u8 = 10;\n\
             println(x.abs_diff(y));\n\
             let big: i64 = -9223372036854775807;\n\
             let top: i64 = 9223372036854775807;\n\
             println(big.abs_diff(top));\n\
             let s1: i8 = -128;\n\
             let s2: i8 = 127;\n\
             println(s1.abs_diff(s2));\n\
         }");
    // Line 3 (-5.abs_diff(3)) = 8; line 6 = 2*9223372036854775807 =
    // 18446744073709551614, which MUST print unsigned (not -2).
    assert_eq!(out, "2\n2\n8\n7\n190\n18446744073709551614\n255\n");
}

#[test]
fn test_next_power_of_two_unsigned() {
    // `uN::next_power_of_two`: smallest power of two ≥ self (0 and 1 → 1), at
    // the receiver width. Includes u8 128 (2^7, fits — no trap), u32 2^31, and
    // u64 2^63 / 2^63-1 (both → 2^63, which MUST print unsigned). Codegen
    // mirrors this (`tests/codegen.rs::e2e_next_power_of_two_unsigned`).
    let out = run("fn main() {\n\
             println((0u32).next_power_of_two());\n\
             println((1u32).next_power_of_two());\n\
             println((2u32).next_power_of_two());\n\
             println((3u32).next_power_of_two());\n\
             println((5u32).next_power_of_two());\n\
             println((100u32).next_power_of_two());\n\
             println((128u8).next_power_of_two());\n\
             let h: u32 = 1u32 << 31;\n\
             println(h.next_power_of_two());\n\
             let i: u64 = 1u64 << 63;\n\
             println(i.next_power_of_two());\n\
             let j: u64 = (1u64 << 63) - 1;\n\
             println(j.next_power_of_two());\n\
         }");
    assert_eq!(
        out,
        "1\n1\n2\n4\n8\n128\n128\n2147483648\n9223372036854775808\n9223372036854775808\n"
    );
}

#[test]
fn test_fences_are_interpreter_noops() {
    // `fence` / `compiler_fence` (`runtime/stdlib/intrinsics.kara`) have empty
    // `#[compiler_builtin]` bodies. A single-threaded tree-walk interpreter
    // observes no memory reordering, so a fence is semantically inert there —
    // the empty body runs as a natural no-op (like `forget`), and the program
    // continues normally. (Codegen lowers them to real LLVM `fence`s; the
    // interpreter simply does nothing, which is the correct sequential
    // semantics.)
    assert_eq!(
        run("fn main() { unsafe { fence(MemoryOrdering.SeqCst) } compiler_fence(MemoryOrdering.Acquire); println(7); }"),
        "7\n"
    );
}

#[test]
fn test_integer_subtraction() {
    assert_eq!(run("fn main() { println(10 - 3); }"), "7\n");
}

#[test]
fn test_integer_multiplication() {
    assert_eq!(run("fn main() { println(4 * 5); }"), "20\n");
}

#[test]
fn test_integer_division() {
    assert_eq!(run("fn main() { println(15 / 4); }"), "3\n");
}

#[test]
fn test_integer_modulo() {
    assert_eq!(run("fn main() { println(17 % 5); }"), "2\n");
}

#[test]
fn test_boolean_logic() {
    assert_eq!(run("fn main() { println(true and false); }"), "false\n");
    assert_eq!(run("fn main() { println(true or false); }"), "true\n");
}

// ── Short-circuit `and` / `or` (roadmap.md:425, 429) ────────────

#[test]
fn test_and_short_circuits_skips_rhs_fn_call() {
    // `false and boom()` must NOT call boom().
    let out = run(r#"
        fn boom() -> bool { println("called"); true }
        fn main() {
            if false and boom() { println("then"); } else { println("else"); }
        }
    "#);
    assert_eq!(out, "else\n");
}

#[test]
fn test_or_short_circuits_skips_rhs_fn_call() {
    // `true or boom()` must NOT call boom().
    let out = run(r#"
        fn boom() -> bool { println("called"); true }
        fn main() {
            if true or boom() { println("then"); } else { println("else"); }
        }
    "#);
    assert_eq!(out, "then\n");
}

#[test]
fn test_and_short_circuits_guards_oob_index() {
    // `i > 0 and visited[i - 1]` must not crash when i == 0
    // (RHS would index Vec at -1, but RHS shouldn't run).
    let out = run_no_errors(
        r#"
        fn main() {
            let visited: Vec[bool] = Vec.new();
            let i = 0;
            if i > 0 and visited[i - 1] { println("then"); } else { println("else"); }
        }
    "#,
    );
    assert_eq!(out, "else\n");
}

#[test]
fn test_and_evaluates_rhs_when_lhs_true() {
    // When LHS doesn't short-circuit, RHS must run.
    let out = run(r#"
        fn boom() -> bool { println("called"); true }
        fn main() {
            if true and boom() { println("then"); } else { println("else"); }
        }
    "#);
    assert_eq!(out, "called\nthen\n");
}

#[test]
fn test_or_evaluates_rhs_when_lhs_false() {
    // When LHS doesn't short-circuit, RHS must run.
    let out = run(r#"
        fn boom() -> bool { println("called"); false }
        fn main() {
            if false or boom() { println("then"); } else { println("else"); }
        }
    "#);
    assert_eq!(out, "called\nelse\n");
}

#[test]
fn test_comparison() {
    assert_eq!(run("fn main() { println(3 > 2); }"), "true\n");
    assert_eq!(run("fn main() { println(3 < 2); }"), "false\n");
    assert_eq!(run("fn main() { println(5 == 5); }"), "true\n");
}

#[test]
fn test_comparison_and_equality_post_lowering() {
    // Exercises every comparison/equality op going through the lowered
    // `T.eq` / `T.lt` / etc. Call-path at runtime.
    assert_eq!(run("fn main() { println(3 != 4); }"), "true\n");
    assert_eq!(run("fn main() { println(3 != 3); }"), "false\n");
    assert_eq!(run("fn main() { println(4 <= 4); }"), "true\n");
    assert_eq!(run("fn main() { println(4 >= 5); }"), "false\n");
    assert_eq!(
        run(r#"fn main() { let a: String = "foo"; let b: String = "foo"; println(a == b); }"#),
        "true\n"
    );
    assert_eq!(
        run(r#"fn main() { let a: String = "foo"; let b: String = "bar"; println(a != b); }"#),
        "true\n"
    );
}

#[test]
fn test_bitnot_and_not_post_lowering() {
    // `~int` lowers to `int.not`, `not bool` lowers to `bool.not`.
    assert_eq!(run("fn main() { println(~0); }"), "-1\n");
    assert_eq!(run("fn main() { println(not false); }"), "true\n");
}

#[test]
fn test_user_cmp_method_call_is_not_answered_by_the_builtin_comparator() {
    // The `.cmp()` half of B-2026-08-26-10, which is what made the operator fix
    // land on a wrong answer in compiled output until it was fixed too: codegen's
    // builtin `method == "cmp"` arm answered for a user `impl Ord` whose receiver
    // flattened to an int, comparing the FIELD instead of running the body.
    //
    // `cmp` here returns `Ordering.Greater` unconditionally, so a backend that
    // runs the body must print `false` and one that compares `id` (1 vs 5) must
    // print `true`. The interpreter always got this right; this pins the answer
    // it must keep agreeing with (see the codegen twin,
    // `e2e_user_cmp_method_call_is_not_answered_by_the_builtin_comparator`).
    assert_eq!(
        run("struct Item { id: i64 }
             impl PartialEq for Item { fn eq(ref self, other: ref Item) -> bool { self.id == other.id } }
             impl Eq for Item {}
             impl PartialOrd for Item {
                 fn partial_cmp(ref self, other: ref Item) -> Option[Ordering] { Some(Ordering.Greater) }
             }
             impl Ord for Item {
                 fn cmp(ref self, other: ref Item) -> Ordering { Ordering.Greater }
             }
             fn main() {
                 let a = Item { id: 1 };
                 let b = Item { id: 5 };
                 println(a.cmp(b).is_lt());
                 println(a < b);
             }"),
        "false\nfalse\n"
    );
}

#[test]
fn interp_priority_queue_honours_a_hand_written_ord_impl() {
    // The INTERPRETER half of B-2026-08-26-24, and it needed a different fix
    // from the compiled half — which is the point of pinning it separately.
    //
    // Lowering rewrites `a < b` into `a.cmp(b).is_lt()` when it can see the
    // operand's type, and for a `T: Ord` param it reads the bound. It cannot see
    // either inside a STDLIB body: the baked stdlib is signature-registered, not
    // type-checked in the user's pipeline, so `expr_types` is empty for it and
    // the rewrite is skipped. `PriorityQueue`'s `self.xs[i] < self.xs[j]` is
    // exactly that. codegen was unaffected because it lowers its stdlib from
    // source — so fixing only lowering left the two backends DISAGREEING, with
    // the compiled binary right and the interpreter erroring. The interpreter
    // now dispatches to a hand-written comparator at runtime.
    //
    // The impl reverses, so the queue pops 3, 2, 1; a derive pops 1, 2, 3.
    assert_eq!(
        run("struct Item { id: i64 }
             impl PartialEq for Item { fn eq(ref self, other: ref Item) -> bool { self.id == other.id } }
             impl Eq for Item {}
             impl PartialOrd for Item { fn partial_cmp(ref self, other: ref Item) -> Option[Ordering] { Some(other.id.cmp(self.id)) } }
             impl Ord for Item { fn cmp(ref self, other: ref Item) -> Ordering { other.id.cmp(self.id) } }
             fn main() {
                 let mut q: PriorityQueue[Item] = PriorityQueue.new();
                 q.push(Item { id: 1 });
                 q.push(Item { id: 3 });
                 q.push(Item { id: 2 });
                 while q.len() > 0 { match q.pop() { Some(it) => println(it.id), None => {} } }
             }"),
        "3\n2\n1\n"
    );
    // The derive path must be untouched.
    assert_eq!(
        run("#[derive(PartialEq, Eq, PartialOrd, Ord)]
             struct Item { id: i64 }
             fn main() {
                 let mut q: PriorityQueue[Item] = PriorityQueue.new();
                 q.push(Item { id: 1 });
                 q.push(Item { id: 3 });
                 q.push(Item { id: 2 });
                 while q.len() > 0 { match q.pop() { Some(it) => println(it.id), None => {} } }
             }"),
        "1\n2\n3\n"
    );
}

#[test]
fn test_eq_ord_direct_method_calls() {
    // `i32.lt(a, b)` and peers are callable directly in user code, matching
    // the lowered form of the operator. Verifies the whole typecheck → lower
    // → interpret pipeline for the Eq/Ord method names.
    assert_eq!(
        run("fn main() {
                 let a: i32 = 3;
                 let b: i32 = 5;
                 println(i32.lt(a, b));
                 println(i32.ge(a, b));
                 println(i32.eq(a, a));
                 println(i32.ne(a, b));
             }"),
        "true\nfalse\ntrue\ntrue\n"
    );
}

#[test]
fn test_derive_ord_cmp_method() {
    // roadmap Phase 8 § Eq/Ord: `.cmp() -> Ordering` on a `#[derive(Ord)]`
    // struct/enum — the method form of the already-working `<`/`>` operators,
    // reusing the same lexicographic order (`value_compare`). Struct fields
    // compare in DECLARATION order; enum variants by declaration index. This
    // is what unblocks `min`/`max`/`clamp` and `sort_by` on user Ord types.
    assert_eq!(
        run(r#"
            #[derive(Ord, Eq, PartialEq, PartialOrd)]
            struct Rec { name: String, age: i64 }
            #[derive(Ord, Eq, PartialEq, PartialOrd)]
            enum Priority { Low, Med, High }
            fn tag(o: Ordering) -> String {
                match o { Less => "lt".to_string(), Equal => "eq".to_string(), Greater => "gt".to_string() }
            }
            fn main() {
                let r1 = Rec { name: "alice".to_string(), age: 30 };
                let r2 = Rec { name: "alice".to_string(), age: 40 };
                let r3 = Rec { name: "bob".to_string(), age: 10 };
                println(tag(r1.cmp(r2)));
                println(tag(r1.cmp(r3)));
                println(tag(r2.cmp(r1)));
                let lo = Priority.Low;
                let hi = Priority.High;
                let md = Priority.Med;
                println(tag(lo.cmp(hi)));
                println(tag(hi.cmp(md)));
                println(tag(md.cmp(md)));
                // min/max/clamp on struct types now resolve (they call .cmp)
                let mx = max(r1, r3);
                println(mx.name);
            }
        "#),
        "lt\nlt\ngt\nlt\ngt\neq\nbob\n"
    );
}

#[test]
fn test_unary_negation() {
    assert_eq!(run("fn main() { println(-42); }"), "-42\n");
}

#[test]
fn test_unary_not() {
    assert_eq!(run("fn main() { println(not true); }"), "false\n");
}

// ── Functions ──────────────────────────────────────────────────

#[test]
fn test_function_call() {
    assert_eq!(
        run("fn double(x: i64) -> i64 { x * 2 }\n\
             fn main() { println(double(21)); }"),
        "42\n"
    );
}

#[test]
fn test_recursive_function() {
    assert_eq!(
        run("fn factorial(n: i64) -> i64 {\n\
                 if n <= 1 { 1 } else { n * factorial(n - 1) }\n\
             }\n\
             fn main() { println(factorial(5)); }"),
        "120\n"
    );
}

#[test]
fn test_multiple_params() {
    assert_eq!(
        run("fn add(a: i64, b: i64) -> i64 { a + b }\n\
             fn main() { println(add(3, 4)); }"),
        "7\n"
    );
}

#[test]
fn test_std_cmp_min_max_clamp_free_functions() {
    // roadmap Phase 8 § std.cmp — `min` / `max` / `clamp` are ordinary
    // generic stdlib free functions (real Kāra bodies in `ordering.kara`),
    // registered as bare-callable prelude names. Generics are erased at
    // runtime, so the same fn serves i64 and String receivers.
    assert_eq!(
        run("fn main() {\n\
                 println(min(3, 5));\n\
                 println(max(3, 5));\n\
                 println(min(5, 3));\n\
                 println(max(5, 3));\n\
             }"),
        "3\n5\n3\n5\n"
    );
    // clamp: below / inside / above the inclusive range.
    assert_eq!(
        run("fn main() {\n\
                 println(clamp(-2, 0, 10));\n\
                 println(clamp(7, 0, 10));\n\
                 println(clamp(15, 0, 10));\n\
             }"),
        "0\n7\n10\n"
    );
    // Generic over any Ord type — String receivers use lexicographic order.
    assert_eq!(
        run("fn main() {\n\
                 println(min(\"abc\", \"abd\"));\n\
                 println(max(\"abc\", \"abd\"));\n\
             }"),
        "abc\nabd\n"
    );
}

#[test]
fn test_std_mem_swap_replace() {
    // roadmap Phase 8 § std.mem — `swap` / `replace` move values through
    // `mut ref` places. `#[compiler_builtin]` intrinsics intercepted in
    // `eval_call`. swap exchanges two places; replace writes the new value and
    // returns the old.
    assert_eq!(
        run("fn main() {\n\
                 let mut a = 1i64; let mut b = 2i64;\n\
                 swap(mut a, mut b);\n\
                 println(f\"{a} {b}\");\n\
                 let old = replace(mut a, 99i64);\n\
                 println(f\"{a} {old}\");\n\
             }"),
        "2 1\n99 2\n"
    );
    // Heap values (String) — buffers relocate, old value flows out.
    assert_eq!(
        run("fn main() {\n\
                 let mut s = \"hello\".to_string();\n\
                 let mut t = \"world\".to_string();\n\
                 swap(mut s, mut t);\n\
                 let prev = replace(mut s, \"new\".to_string());\n\
                 println(f\"{s} {t} {prev}\");\n\
             }"),
        "new hello world\n"
    );
    // `mut ref` param forwarding: `swap(slot, mut z)` inside a fn taking
    // `slot: mut ref i64` writes through the borrow.
    assert_eq!(
        run("fn reset(slot: mut ref i64) -> i64 {\n\
                 let mut z = 0i64; swap(slot, mut z); z\n\
             }\n\
             fn main() {\n\
                 let mut n = 77i64;\n\
                 let prev = reset(mut n);\n\
                 println(f\"{n} {prev}\");\n\
             }"),
        "0 77\n"
    );
    // A USER-defined `swap` (different shape — owned params, returns a tuple)
    // must SHADOW the builtin: the intercept defers to the user fn when one is
    // in scope. Peer to codegen's `test_e2e_generic_swap_via_tuple`.
    assert_eq!(
        run("fn swap[T](a: T, b: T) -> (T, T) { (b, a) }\n\
             fn main() {\n\
                 let r = swap(1, 2);\n\
                 println(f\"{r.0} {r.1}\");\n\
             }"),
        "2 1\n"
    );
}

#[test]
fn test_std_mem_take() {
    // roadmap Phase 8 § std.mem — `take[T: Default](dest: mut ref T) -> T`
    // moves the value out of `*dest`, leaving `T.default()` behind, and returns
    // it. A REAL Kāra body (`replace(dest, T.default())`), not a
    // `#[compiler_builtin]` — it monomorphizes per concrete `T`, so it exercises
    // both the `replace` intercept AND the derived/primitive `T.default()`.
    // Primitive `T` — `take` leaves the zero value.
    assert_eq!(
        run("fn main() {\n\
                 let mut n = 7i64;\n\
                 let prev = take(mut n);\n\
                 println(f\"{prev} {n}\");\n\
             }"),
        "7 0\n"
    );
    // Named `#[derive(Default)]` type — `take` leaves the field-wise default
    // (0 / empty String) and returns the original struct (heap String flows out
    // intact).
    assert_eq!(
        run("#[derive(Default)]\n\
             struct S { x: i64, name: String }\n\
             fn main() {\n\
                 let mut a = S { x: 42i64, name: \"hello\".to_string() };\n\
                 let old = take(mut a);\n\
                 println(f\"{old.x} {old.name}\");\n\
                 println(f\"{a.x} [{a.name}]\");\n\
             }"),
        "42 hello\n0 []\n"
    );
    // `mut ref` param forwarding: `take(slot)` inside a fn taking
    // `slot: mut ref String` writes the default through the borrow.
    assert_eq!(
        run("fn steal(slot: mut ref String) -> String { take(slot) }\n\
             fn main() {\n\
                 let mut s = \"owned\".to_string();\n\
                 let got = steal(mut s);\n\
                 println(f\"{got} [{s}]\");\n\
             }"),
        "owned []\n"
    );
}

// ── Pipe Operator ──────────────────────────────────────────────

#[test]
fn test_pipe_basic() {
    assert_eq!(
        run("fn double(x: i64) -> i64 { x * 2 }\n\
             fn main() { println(21 |> double); }"),
        "42\n"
    );
}

#[test]
fn test_pipe_chained() {
    assert_eq!(
        run("fn add1(x: i64) -> i64 { x + 1 }\n\
             fn double(x: i64) -> i64 { x * 2 }\n\
             fn main() { println(5 |> add1 |> double); }"),
        "12\n"
    );
}

// ── `??` (nil coalescing) ──────────────────────────────────────

/// B-2026-08-17-27 — the interpreter's `??` was wrong on three of its four
/// legs. It recognized only a unit `None`, returning the operand UNCHANGED for
/// everything else: `Some(7) ?? -1` gave `Some(7)` where design.md line 782
/// says the wrapper is stripped, and `Err("bad") ?? -1` propagated the error
/// instead of taking the fallback. Only `None` was right.
///
/// All four legs, both wrappers, scalar and heap payloads.
#[test]
fn test_nil_coalesce_strips_the_wrapper_on_every_leg() {
    let prelude = "fn find(k: i64) -> Option[i64] { if k == 7 { return Some(7); } return None; }\n\
                   fn name(k: i64) -> Option[String] { if k == 1 { return Some(\"hit\"); } return None; }\n\
                   fn res(k: i64) -> Result[i64, String] { if k == 1 { return Ok(12); } return Err(\"bad\"); }\n\
                   fn sres(k: i64) -> Result[String, String] { if k == 1 { return Ok(\"fine\"); } return Err(\"bad\"); }\n";
    for (expr, want) in [
        ("find(7) ?? -1", "7\n"),  // Option/Some — used to print `Some(7)`
        ("find(3) ?? -1", "-1\n"), // Option/None — the one leg that was right
        ("res(1) ?? -1", "12\n"),  // Result/Ok   — used to print `Ok(12)`
        ("res(2) ?? -1", "-1\n"),  // Result/Err  — used to print `Err(bad)`
        ("name(1) ?? \"miss\"", "hit\n"),
        ("name(2) ?? \"miss\"", "miss\n"),
        ("sres(1) ?? \"fell\"", "fine\n"),
        ("sres(2) ?? \"fell\"", "fell\n"),
    ] {
        assert_eq!(
            run(&format!("{prelude}fn main() {{ println({expr}); }}")),
            want,
            "`{expr}`"
        );
    }
}

#[test]
fn test_expect_with_message() {
    let errors = runtime_errors(r#"fn main() { let x = None; x.expect("value required"); }"#);
    assert!(
        errors.iter().any(|e| e.message.contains("value required")),
        "expected expect() message to surface in the runtime error, got {:?}",
        errors
    );
}

#[test]
fn test_unified_stack_program_order_lifo() {
    // The drop+defer stack interleaves bindings and defers in
    // program order; LIFO drain means later items pop first.
    // Drop slots are no-ops today, but defer ordering relative to
    // them must respect program order. The test pins the defer-vs-defer
    // ordering across interleaved `let` statements.
    assert_eq!(
        run("fn main() {\n\
                 let _x = 1;\n\
                 defer { print(\"d1\"); }\n\
                 let _y = 2;\n\
                 defer { print(\"d2\"); }\n\
             }"),
        "d2d1"
    );
}

// ── Repeat literal `[v; n]` ────────────────────────────────────

#[test]
fn test_repeat_literal_bare_runtime() {
    // `[v; n]` allocates n copies of v and is iterable.
    assert_eq!(
        run("fn main() {
                 let v = [0; 5];
                 let mut sum = 0;
                 for x in v { sum = sum + x + 1; }
                 println(sum);
             }"),
        "5\n"
    );
}

#[test]
fn test_repeat_literal_with_annotation_runtime() {
    // Bare `[v; n]` coerced to Array[T, N] via let annotation.
    assert_eq!(
        run("fn main() {
                 let a: Array[i64, 4] = [9; 4];
                 println(a[1]);
             }"),
        "9\n"
    );
}

// ── E2E: Complete Programs ─────────────────────────────────────

#[test]
fn test_e2e_fizzbuzz() {
    assert_eq!(
        run("fn fizzbuzz(n: i64) -> i64 {\n\
                 if n % 15 == 0 { 0 }\n\
                 else if n % 3 == 0 { 3 }\n\
                 else if n % 5 == 0 { 5 }\n\
                 else { n }\n\
             }\n\
             fn main() {\n\
                 println(fizzbuzz(15));\n\
                 println(fizzbuzz(9));\n\
                 println(fizzbuzz(10));\n\
                 println(fizzbuzz(7));\n\
             }"),
        "0\n3\n5\n7\n"
    );
}

#[test]
fn test_e2e_fibonacci() {
    // The tree-walk interpreter's eval_expr_inner / eval_call match
    // statements have grown wide; in debug builds on Windows the per-
    // frame allocation exceeds the libtest worker thread's 2 MB stack
    // for the ~10-deep recursion fib(10) produces (90+ Rust frames).
    // Linux/macOS debug frames are smaller and fit, but Windows CI
    // overflows even after prior helper extractions (eval_short_circuit
    // / eval_vec_filled). Spawn the body on a fresh 8 MB thread, same
    // pattern as test_error_trace_truncation_at_64.
    let handle = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            assert_eq!(
                run("fn fib(n: i64) -> i64 {\n\
                         if n <= 1 { n }\n\
                         else { fib(n - 1) + fib(n - 2) }\n\
                     }\n\
                     fn main() {\n\
                         println(fib(0));\n\
                         println(fib(1));\n\
                         println(fib(10));\n\
                     }"),
                "0\n1\n55\n"
            );
        })
        .unwrap();
    handle.join().unwrap();
}

#[test]
fn test_e2e_higher_order_functions() {
    assert_eq!(
        run("fn apply_twice(f: Fn(i64) -> i64, x: i64) -> i64 {\n\
                 f(f(x))\n\
             }\n\
             fn add3(x: i64) -> i64 { x + 3 }\n\
             fn main() {\n\
                 println(apply_twice(add3, 10));\n\
                 println(apply_twice(|x: i64| x * 2, 5));\n\
             }"),
        "16\n20\n"
    );
}

#[test]
fn test_e2e_error_handling() {
    assert_eq!(
        run("fn divide(a: i64, b: i64) -> i64 {\n\
                 if b == 0 { return Err(0); }\n\
                 Ok(a / b)\n\
             }\n\
             fn main() {\n\
                 let r1 = divide(10, 2);\n\
                 println(r1.unwrap());\n\
                 let r2 = divide(10, 0);\n\
                 println(r2.is_err());\n\
             }"),
        "5\ntrue\n"
    );
}

#[test]
fn test_error_trace_multi_level_propagation() {
    let (_output, trace, truncated) = run_program_with_trace(
        "fn inner() {\n\
             let r = Err(99);\n\
             r?\n\
         }\n\
         fn middle() {\n\
             let v = inner()?;\n\
             v\n\
         }\n\
         fn outer() {\n\
             let v = middle()?;\n\
             v\n\
         }\n\
         fn main() {\n\
             let result = outer();\n\
             match result {\n\
                 Ok(v) => println(v),\n\
                 Err(e) => println(e),\n\
             }\n\
         }",
    );
    assert!(!truncated);
    // inner's ?, middle's ?, outer's ? = 3 frames
    assert_eq!(trace.len(), 3, "Expected 3 trace frames, got {:?}", trace);
}

#[test]
fn test_error_trace_cleared_on_ok() {
    let (_output, trace, _truncated) = run_program_with_trace(
        "fn try_thing() {\n\
             let r = Ok(10);\n\
             r?\n\
         }\n\
         fn main() {\n\
             let v = try_thing();\n\
             println(v);\n\
         }",
    );
    assert!(
        trace.is_empty(),
        "Trace should be empty after Ok(?), got {:?}",
        trace
    );
}

#[test]
fn test_error_trace_none_propagation() {
    let (_output, trace, truncated) = run_program_with_trace(
        "fn find() {\n\
             let r = None;\n\
             r?\n\
         }\n\
         fn search() {\n\
             let v = find()?;\n\
             v\n\
         }\n\
         fn main() {\n\
             let result = search();\n\
             match result {\n\
                 Some(v) => println(v),\n\
                 None => println(\"not found\"),\n\
             }\n\
         }",
    );
    assert!(!truncated);
    assert_eq!(
        trace.len(),
        2,
        "Expected 2 trace frames for None propagation, got {:?}",
        trace
    );
}

#[test]
fn test_error_trace_truncation_at_64() {
    // Build a deeply nested chain of ? propagation (65 levels).
    // Needs a larger stack for deep recursion in debug builds.
    let result = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let mut source = String::new();
            source.push_str("fn func0() {\n    let r = Err(1);\n    r?\n}\n");
            for i in 1..=64 {
                source.push_str(&format!(
                    "fn func{i}() {{\n    let v = func{}()?;\n    v\n}}\n",
                    i - 1
                ));
            }
            source.push_str(
                "fn main() {\n\
                     let result = func64();\n\
                     match result {\n\
                         Ok(v) => println(v),\n\
                         Err(e) => println(e),\n\
                     }\n\
                 }",
            );
            let (_output, trace, truncated) = run_program_with_trace(&source);
            // 65 frames total, but max is 64, so oldest is dropped
            assert!(truncated, "Trace should be truncated at 64 frames");
            assert_eq!(trace.len(), 64, "Trace should have exactly 64 frames");
        })
        .expect("failed to spawn thread")
        .join();
    result.expect("test thread panicked");
}

#[test]
fn test_total_order_wrapper_nan_canonicalized_interp_parity() {
    // B-2026-08-11-13 — the wrapper orders and compares by BIT pattern, so
    // before the fix the sign of a NaN decided the answer, and nothing in the
    // source chooses that sign: x86 runtime division yields a NEGATIVE NaN
    // while LLVM's constant folder yields a POSITIVE one, and totalOrder puts
    // the first before `-Infinity` and the second after `+Infinity`. One
    // program therefore gave four different results — interp, JIT, AOT at -O2,
    // and AOT at -O0 — including a `Vec[F64].sort()` that put NaN at BOTH ends
    // (correctly sorted under raw totalOrder; nonsense to a reader, since both
    // print `NaN`) and a two-NaN `Map` whose LEN differed by backend.
    //
    // Every NaN is now canonicalized to one quiet NaN at construction, so all
    // of it collapses to a single answer. Same program + expected output as
    // the codegen E2E twin (`test_e2e_total_order_wrapper_nan_canonicalized`);
    // that pairing is the actual contract — run == build.
    //
    // B-2026-08-14-12: the `F32.from` line carries an explicit `as f32`, in
    // both twins. `z` is `f64`, so `z / z` is a runtime f64 NaN and putting it
    // in an f32 slot is a genuine narrowing — the only real one the float gate
    // found in the suite. The cast preserves the behaviour (NaN narrows to
    // NaN) and states it.
    let output = run("fn main() {\n\
            let n = env.args().len();\n\
            let z = (n as f64) - (n as f64);\n\
            let c: F64 = F64 { value: 0.0 / 0.0 };\n\
            let r: F64 = F64.from(z / z);\n\
            let one: F64 = F64 { value: 1.0 };\n\
            let inf: F64 = F64 { value: 1.0 / 0.0 };\n\
            println(c == r);\n\
            println(c < one);\n\
            println(r < one);\n\
            println(r > inf);\n\
            let mut v: Vec[F64] = Vec.new();\n\
            v.push(r);\n\
            v.push(one);\n\
            v.push(c);\n\
            v.push(F64 { value: 0.0 - 1.0 });\n\
            v.sort();\n\
            println(v[0].value);\n\
            println(v[1].value);\n\
            println(v[3].value);\n\
            let mut m: Map[F64, i64] = Map.new();\n\
            let _ = m.insert(c, 1);\n\
            let _ = m.insert(r, 2);\n\
            println(m.len());\n\
            match m.get(r) { Some(x) => println(x), None => println(0 - 1) }\n\
            let n0: F64 = F64 { value: 0.0 * (0.0 - 1.0) };\n\
            let p0: F64 = F64 { value: 0.0 };\n\
            println(n0 < p0);\n\
            println(n0 == p0);\n\
            let c32: F32 = F32 { value: 0.0 / 0.0 };\n\
            let r32: F32 = F32.from((z / z) as f32);\n\
            println(c32 == r32);\n\
        }");
    // The two `< one` lines are the crux: pre-fix they DISAGREED with each
    // other (the const NaN sorted last, the runtime NaN first) even though
    // both print `NaN`. Then: c==r across provenance, r>inf, sort[0]/[1]/[3],
    // map len (1, not 2), map get, -0<+0 and -0==+0 (B-2026-08-11-14: still
    // DISTINCT — only NaN is normalized), and the F32 twin.
    assert_eq!(
        output,
        "true\nfalse\nfalse\ntrue\n-1\n1\nNaN\n1\n2\ntrue\nfalse\ntrue\n"
    );
}

#[test]
fn test_into_at_call_argument_position() {
    let output = run("fn takes(y: i64) { println(y); }\n\
         fn main() { let x: i32 = 99; takes(x.into()); }");
    assert_eq!(output, "99\n");
}

#[test]
fn test_into_drives_user_from_impl() {
    // User-defined `impl From[Inches] for Cm` should drive `.into()` at
    // a `let: Cm` position.
    let output = run("struct Inches { n: i64 }\n\
         struct Cm { n: i64 }\n\
         impl From for Cm {\n\
             fn from(i: Inches) -> Cm { Cm { n: i.n * 254 / 100 } }\n\
         }\n\
         fn main() {\n\
             let i: Inches = Inches { n: 10 };\n\
             let c: Cm = i.into();\n\
             println(c.n);\n\
         }");
    assert_eq!(output, "25\n");
}

#[test]
fn test_into_wraps_value_in_some_and_ok() {
    // `From[T] for Option[T]` / `From[T] for Result[T, E]` blanket wraps
    // (design.md § Conversion Traits): `.into()` at an `Option[T]`-expected
    // position builds `Some(x)`, and at a `Result[T, E]`-expected position
    // builds `Ok(x)`. `E` comes entirely from the annotation. Covers the
    // let-annotation, if/else-tail return, and struct-field positions, plus a
    // heap `String` payload; the `Err`/`None` arms stay hand-writable.
    let output = run("fn get_opt(present: bool) -> Option[i64] {\n\
             if present { 42.into() } else { None }\n\
         }\n\
         fn get_res(ok: bool) -> Result[i64, String] {\n\
             if ok { 7.into() } else { Err(\"nope\") }\n\
         }\n\
         struct Holder { slot: Option[String] }\n\
         fn main() {\n\
             let o: Option[i64] = 5.into();\n\
             match o { Some(v) => println(v), None => println(-1) };\n\
             match get_opt(true) { Some(v) => println(v), None => println(-1) };\n\
             match get_opt(false) { Some(v) => println(v), None => println(-1) };\n\
             match get_res(true) { Ok(v) => println(v), Err(e) => println(e) };\n\
             match get_res(false) { Ok(v) => println(v), Err(e) => println(e) };\n\
             let h: Holder = Holder { slot: \"hi\".into() };\n\
             match h.slot { Some(s) => println(s), None => println(\"none\") };\n\
         }");
    assert_eq!(output, "5\n42\n-1\n7\nnope\nhi\n");
}

#[test]
fn test_tryinto_drives_user_tryfrom_impl() {
    // A user `impl TryFrom[Celsius] for Kelvin` must drive BOTH the explicit
    // `Kelvin.try_from(c)` call and the `.try_into()` sugar at a
    // `let: Result[Kelvin, _]` position — the fallible sibling of
    // `test_into_drives_user_from_impl`. Exercises the Ok arm (valid
    // conversion) and the Err arm (predicate failure returns the error
    // message). Guards the whole `.try_into()` → `Target.try_from(x)` desugar
    // chain, which had no runtime coverage before.
    let src = "struct Celsius { deg: i64 }\n\
               struct Kelvin { deg: i64 }\n\
               impl TryFrom for Kelvin {\n\
                   type Error = String;\n\
                   fn try_from(c: Celsius) -> Result[Kelvin, String] {\n\
                       if c.deg < -273 { Err(\"below absolute zero\") }\n\
                       else { Ok(Kelvin { deg: c.deg + 273 }) }\n\
                   }\n\
               }\n\
               fn main() {\n\
                   match Kelvin.try_from(Celsius { deg: 27 }) {\n\
                       Ok(k) => println(k.deg),\n\
                       Err(e) => println(e),\n\
                   }\n\
                   let r: Result[Kelvin, String] = (Celsius { deg: -300 }).try_into();\n\
                   match r {\n\
                       Ok(k) => println(k.deg),\n\
                       Err(e) => println(e),\n\
                   }\n\
               }";
    let output = run(src);
    assert_eq!(output, "300\nbelow absolute zero\n");
}

#[test]
fn test_ordering_helper_methods() {
    // `impl Ordering { fn is_lt … }` per design.md § Comparison Traits
    // (lines 5162-5168). Lives in baked source `runtime/stdlib/ordering.kara`;
    // requires the interpreter to walk baked impl blocks.
    let output = run("fn main() {\n\
             let lt = Ordering.Less;\n\
             let eq = Ordering.Equal;\n\
             let gt = Ordering.Greater;\n\
             println(lt.is_lt());\n\
             println(lt.is_le());\n\
             println(lt.is_gt());\n\
             println(lt.is_ge());\n\
             println(lt.is_eq());\n\
             println(eq.is_lt());\n\
             println(eq.is_le());\n\
             println(eq.is_gt());\n\
             println(eq.is_ge());\n\
             println(eq.is_eq());\n\
             println(gt.is_lt());\n\
             println(gt.is_le());\n\
             println(gt.is_gt());\n\
             println(gt.is_ge());\n\
             println(gt.is_eq());\n\
         }");
    assert_eq!(
        output,
        "true\ntrue\nfalse\nfalse\nfalse\n\
         false\ntrue\nfalse\ntrue\ntrue\n\
         false\nfalse\ntrue\ntrue\nfalse\n",
    );
}

#[test]
fn test_interp_primitive_const_f64_infinity() {
    let output = run("fn main() { let x = f64.INFINITY; println(x); }");
    assert_eq!(output.trim(), "inf");
}

#[test]
fn test_interp_primitive_const_f64_neg_infinity() {
    let output = run("fn main() { let x = f64.NEG_INFINITY; println(x); }");
    assert_eq!(output.trim(), "-inf");
}

#[test]
fn test_interp_primitive_const_f64_nan() {
    let output = run("fn main() { let x = f64.NAN; println(x); }");
    assert_eq!(output.trim(), "NaN");
}

#[test]
fn test_interp_primitive_const_f32_epsilon() {
    // f32::EPSILON is 1.1920929e-7. Tree-walk interpreter widens to
    // f64 via `Value::Float`; round-trip through Display preserves
    // enough precision that we can assert a substring.
    let output = run("fn main() { let x = f32.EPSILON; println(x); }");
    assert!(
        output.starts_with("0.000000119"),
        "expected f32.EPSILON to print as 0.000000119...; got {output:?}"
    );
}

#[test]
fn test_with_provider_multiple_resources_resolve_independently() {
    let output = run("effect resource UserDB;
         effect resource AuditLog;
         struct Db { tag: i64 }
         impl Db { fn id(self) -> i64 { self.tag } }
         struct Log { count: i64 }
         impl Log { fn count(self) -> i64 { self.count } }
         fn main() {
             with_provider[UserDB](Db { tag: 42 }, || {
                 with_provider[AuditLog](Log { count: 7 }, || {
                     println(UserDB.id());
                     println(AuditLog.count());
                 });
             });
         }");
    assert_eq!(output, "42\n7\n");
}

#[test]
fn test_filesystem_read_lines_splits_and_strips_newlines() {
    // phase-8 `fs.read_lines()` slice: slurp a file and split into lines with
    // Rust `str::lines()` semantics — split on `\n`, strip a trailing `\r`
    // (CRLF), and a final newline yields no trailing empty element. Yields
    // `Result[Vec[String], IoError]`.
    let tmp = std::env::temp_dir().join("karac_test_fs_read_lines.txt");
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    // CRLF on the middle line + a trailing LF: expect ["one", "two", "three"].
    std::fs::write(&tmp, "one\ntwo\r\nthree\n").expect("temp write");
    let src = format!(
        "fn main() with reads(FileSystem) {{
             match FileSystem.read_lines(\"{path}\") {{
                 Ok(lines) => {{
                     println(lines.len());
                     for l in lines {{ println(l); }}
                 }}
                 Err(_) => println(\"read error\"),
             }}
         }}"
    );
    let out = run_no_errors(&src);
    assert_eq!(out, "3\none\ntwo\nthree\n");
    let _ = std::fs::remove_file(&tmp);
}

// ── Default parameter values ──────────────────────────────────────

#[test]
fn test_default_param_used_when_arg_omitted() {
    let out = run_no_errors(
        "fn greet(name: String, greeting: String = \"Hello\") -> String { greeting + \", \" + name }\n\
         fn main() { println(greet(\"world\")); }",
    );
    assert_eq!(out, "Hello, world\n");
}

#[test]
fn test_default_param_overridden_by_explicit_arg() {
    let out = run_no_errors(
        "fn greet(name: String, greeting: String = \"Hello\") -> String { greeting + \", \" + name }\n\
         fn main() { println(greet(\"world\", \"Hi\")); }",
    );
    assert_eq!(out, "Hi, world\n");
}

#[test]
fn test_default_param_integer() {
    let out = run_no_errors(
        "fn add(x: i64, step: i64 = 1) -> i64 { x + step }\n\
         fn main() { println(add(10)); println(add(10, 5)); }",
    );
    assert_eq!(out, "11\n15\n");
}

#[test]
fn test_multiple_defaults_substitution() {
    // Both trailing params omitted — both defaults fire.
    let out = run_no_errors(
        "fn pair(a: i64, b: i64 = 10, c: i64 = 20) -> i64 { a + b + c }\n\
         fn main() {\n\
             println(pair(1));\n\
             println(pair(1, 5));\n\
             println(pair(1, 5, 7));\n\
         }",
    );
    assert_eq!(out, "31\n26\n13\n");
}

// ── #[derive(Arithmetic)] on distinct types ────────────────────

#[test]
fn test_derive_arithmetic_addition() {
    let output = run_no_errors(
        "#[derive(Arithmetic)]\n\
         distinct type Meters = i64;\n\
         fn main() {\n\
             let a: Meters = 10;\n\
             let b: Meters = 3;\n\
             let sum: Meters = a + b;\n\
             println(sum);\n\
         }",
    );
    assert_eq!(output, "13\n");
}

#[test]
fn test_derive_arithmetic_negation() {
    let output = run_no_errors(
        "#[derive(Arithmetic)]\n\
         distinct type Offset = i64;\n\
         fn main() {\n\
             let x: Offset = 7;\n\
             let neg: Offset = -x;\n\
             println(neg);\n\
         }",
    );
    assert_eq!(output, "-7\n");
}

#[test]
fn test_derive_arithmetic_all_ops() {
    let output = run_no_errors(
        "#[derive(Arithmetic)]\n\
         distinct type Score = i64;\n\
         fn main() {\n\
             let a: Score = 10;\n\
             let b: Score = 3;\n\
             println(a + b);\n\
             println(a - b);\n\
             println(a * b);\n\
             println(a / b);\n\
             println(a % b);\n\
         }",
    );
    assert_eq!(output, "13\n7\n30\n3\n1\n");
}

// ── ExitCode (Phase-8 entry-point contract Slice B) ────────────

#[test]
fn test_exitcode_success_failure_values() {
    // `ExitCode.SUCCESS` / `ExitCode.FAILURE` evaluate to 0 / 1. The
    // interpreter can't observe the process exit (that happens in
    // `cmd_run`), so the value is checked through `.raw()`.
    let output = run_no_errors(
        "fn main() {\n\
             let ok = ExitCode.SUCCESS;\n\
             let bad = ExitCode.FAILURE;\n\
             println(ok.raw());\n\
             println(bad.raw());\n\
         }",
    );
    assert_eq!(output, "0\n1\n");
}

#[test]
fn test_exitcode_from_arbitrary_code() {
    // `ExitCode.from(code)` wraps an arbitrary code (the stdlib
    // identity-wrap body `{ ExitCode(code) }`).
    let output = run_no_errors("fn main() { println(ExitCode.from(42).raw()); }");
    assert_eq!(output, "42\n");
}

#[test]
fn test_distinct_derived_comparison_runs() {
    // With `#[derive(Eq, Ord)]` the comparison operators are admitted and
    // run on the base layout — `UserId(3) < UserId(5)` is `true`,
    // `UserId(5) == UserId(5)` is `true`.
    let output = run_no_errors(
        "#[derive(Eq, Ord)]\n\
         distinct type UserId = i64;\n\
         fn main() {\n\
             println(UserId(3) < UserId(5));\n\
             println(UserId(5) == UserId(5));\n\
         }",
    );
    assert_eq!(output, "true\ntrue\n");
}

#[test]
fn test_pool_acquire_mints_via_create_fn() {
    // Acquire on a fresh pool invokes `create_fn` and hands back a
    // `PooledConnection` carrying the minted value. Verifies the
    // intrinsic's closure-invocation path lights up.
    let output = run(r#"fn make_int() -> i64 { 42 }
         fn main() {
             let pool: Pool[i64] = Pool.new(make_int, 4, 8);
             match pool.acquire(0) {
                 Ok(conn) => println(conn.val),
                 Err(_) => println("err"),
             }
         }"#);
    assert_eq!(output, "42\n");
}

#[test]
fn test_pool_health_check_passes_reuses_idle_slot() {
    // A registered health check that returns true hands the released
    // (idle) slot straight back — no fresh mint. `make_conn` prints
    // "mint" on each call, so a single "mint" proves the second acquire
    // reused the slot rather than re-minting. (The first acquire always
    // mints; the hook only validates *reused* idle slots.)
    let output = run(r#"fn make_conn() -> i64 { println("mint"); 7 }
         fn main() {
             let pool: Pool[i64] = Pool.new(make_conn, 4, 8).with_health_check(|c| { c > 0 });
             match pool.acquire(0) {
                 Ok(c1) => { pool.release(c1); }
                 Err(_) => println("acq1_err"),
             }
             match pool.acquire(0) {
                 Ok(c2) => println(c2.val),
                 Err(_) => println("acq2_err"),
             }
         }"#);
    assert_eq!(output, "mint\n7\n");
}

#[test]
fn test_pool_health_check_fails_evicts_and_mints_fresh() {
    // A health check returning false evicts the reused idle slot and
    // `acquire` mints a fresh one in its place (evict-on-error). The
    // second acquire therefore mints again — two "mint" lines total.
    let output = run(r#"fn make_conn() -> i64 { println("mint"); 7 }
         fn main() {
             let pool: Pool[i64] = Pool.new(make_conn, 4, 8).with_health_check(|c| { c < 0 });
             match pool.acquire(0) {
                 Ok(c1) => { pool.release(c1); }
                 Err(_) => println("acq1_err"),
             }
             match pool.acquire(0) {
                 Ok(c2) => println(c2.val),
                 Err(_) => println("acq2_err"),
             }
         }"#);
    assert_eq!(output, "mint\nmint\n7\n");
}

#[test]
fn test_pool_health_check_eviction_at_cap_does_not_timeout() {
    // The eviction-decrements-active_count contract: a pool at its
    // `max_connections` cap (1) whose only idle slot fails the health
    // check must evict (freeing a cap slot) and mint a replacement — NOT
    // return Timeout. Without the decrement the mint path would see the
    // pool still at cap and fail closed.
    let output = run(r#"fn make_conn() -> i64 { println("mint"); 7 }
         fn main() {
             let pool: Pool[i64] = Pool.new(make_conn, 1, 1).with_health_check(|c| { false });
             match pool.acquire(0) {
                 Ok(c1) => { pool.release(c1); }
                 Err(_) => println("acq1_err"),
             }
             match pool.acquire(0) {
                 Ok(_) => println("ok2"),
                 Err(PoolError.Timeout) => println("timeout2"),
                 Err(_) => println("err2"),
             }
         }"#);
    assert_eq!(output, "mint\nmint\nok2\n");
}

#[test]
fn test_arena_high_water_mark_and_rewind() {
    // Snapshot/restore: record a checkpoint, push past it, then
    // `rewind_to` truncates the backing vec back to the checkpoint
    // length. `len()` reflects the truncation.
    let output = run(r#"fn main() {
             let a: Arena[i64] = Arena.new();
             let _r0 = a.push(1);
             let _r1 = a.push(2);
             let cp = a.high_water_mark();
             let _r2 = a.push(3);
             let _r3 = a.push(4);
             println(a.len());
             a.rewind_to(cp);
             println(a.len());
         }"#);
    assert_eq!(output, "4\n2\n");
}

#[test]
fn test_hex_encode_lowercase() {
    let output = run("fn main() {\n\
             let bs = [255u8, 0u8, 16u8];\n\
             println(Hex.encode(bs));\n\
         }");
    assert_eq!(output, "ff0010\n");
}

#[test]
fn test_hex_encode_upper() {
    let output = run("fn main() {\n\
             let bs = [255u8, 0u8, 16u8];\n\
             println(Hex.encode_upper(bs));\n\
         }");
    assert_eq!(output, "FF0010\n");
}

#[test]
fn test_hex_decode_mixed_case() {
    let output = run("fn main() {\n\
             match Hex.decode(\"FfaA\") {\n\
                 Ok(bs) => println(bs.len()),\n\
                 Err(_) => println(\"err\"),\n\
             }\n\
         }");
    assert_eq!(output, "2\n");
}

#[test]
fn test_hex_decode_odd_length() {
    let output = run("fn main() {\n\
             match Hex.decode(\"abc\") {\n\
                 Ok(_) => println(\"ok\"),\n\
                 Err(_) => println(\"err\"),\n\
             }\n\
         }");
    assert_eq!(output, "err\n");
}

#[test]
fn test_url_decode_invalid_percent() {
    let output = run("fn main() {\n\
             match Url.decode(\"a%2\") {\n\
                 Ok(_) => println(\"ok\"),\n\
                 Err(_) => println(\"err\"),\n\
             }\n\
         }");
    assert_eq!(output, "err\n");
}

#[test]
fn test_chars_count_and_len_interpreter() {
    // B-2026-07-11-9 gap 1: `s.chars().count()` and its alias `s.chars().len()`
    // drain the char-iterator and return the count. Mirrors the codegen E2E
    // `e2e_chars_count_and_len_codegen`.
    let output = run(r#"fn main() {
        let s: String = "hello";
        println(f"{s.chars().count()}");
        println(f"{s.chars().len()}");
        let e: String = "";
        println(f"{e.chars().count()}");
    }"#);
    assert_eq!(output.trim(), "5\n5\n0");
}

#[test]
fn test_f64_parse_interpreter() {
    // Float parse: decimal / scientific / negative / reject / integer-form.
    // The self-hosting lexer's float-literal path.
    let output = run(
        r#"fn pr(o: Option[f64]) { match o { Some(x) => println(x), None => println(-1.0), } }
        fn main() {
            pr(f64.parse("3.14"));
            pr(f64.parse("1e10"));
            pr(f64.parse("-2.5"));
            pr(f64.parse("notnum"));
            pr(f64.parse("42"));
        }"#,
    );
    assert_eq!(output, "3.14\n10000000000\n-2.5\n-1\n42\n");
}

#[test]
fn test_integration_two_traits_one_type() {
    // A single concrete type implementing two factory traits — verify
    // both dispatch correctly in the same program.
    let output = run(r#"
trait Default {
    fn default() -> Self;
}
trait FromI64 {
    fn from_i64(n: i64) -> Self;
}

struct Counter { value: i64 }

impl Default for Counter {
    fn default() -> Counter { Counter { value: 0 } }
}
impl FromI64 for Counter {
    fn from_i64(n: i64) -> Counter { Counter { value: n } }
}

fn main() {
    let a: Counter = default();
    let b: Counter = from_i64(7);
    println(a.value);
    println(b.value);
}
"#);
    assert_eq!(output, "0\n7\n");
}

/// `.cmp()` on a receiver with no name to look up, under the tree-walker
/// (B-2026-08-27-47).
///
/// The defect this guards was codegen's, but the ORACLE for it lives here:
/// `tests/codegen.rs` twins the compiled answer against the interpreter's, so
/// if the tree-walker ever stops ordering these receivers the E2E fixture
/// keeps passing — both backends would simply agree on a new wrong answer.
/// That is the failure mode a twinned test cannot see by itself, and it is why
/// this half pins literal expected output instead of comparing backends.
///
/// It is also the only coverage of this semantics on the default `cargo test`
/// leg, since `tests/codegen.rs` is entirely `#[cfg(feature = "llvm")]`.
///
/// The `Rev` row is the anti-hijack guard: its hand-written `cmp` reverses the
/// order, so a structural declaration-order comparator would print `0` where
/// the user impl prints `2`.
#[test]
fn cmp_on_a_non_identifier_receiver_under_the_tree_walker() {
    let out = run_no_errors(
        r#"
#[derive(Ord, Eq)]
struct P { a: i64, b: i64 }

#[derive(Ord, Eq)]
enum Suit { Clubs, Hearts, Spades }

struct Rev { v: i64 }
impl PartialEq for Rev { fn eq(ref self, other: ref Rev) -> bool { self.v == other.v } }
impl Eq for Rev {}
impl PartialOrd for Rev { fn partial_cmp(ref self, other: ref Rev) -> Option[Ordering] { Some(other.v.cmp(self.v)) } }
impl Ord for Rev { fn cmp(ref self, other: ref Rev) -> Ordering { other.v.cmp(self.v) } }

fn mk(a: i64) -> P { return P { a: a, b: 0 }; }
fn suit() -> Suit { return Suit.Clubs; }

fn tag(o: Ordering) -> i64 {
    if o.is_lt() { return 0; }
    if o.is_eq() { return 1; }
    return 2;
}

fn main() {
    println(f"{tag(P { a: 1, b: 2 }.cmp(P { a: 1, b: 3 }))}");
    println(f"{tag(P { a: 1, b: 2 }.cmp(P { a: 1, b: 2 }))}");
    println(f"{tag(P { a: 9, b: 0 }.cmp(P { a: 1, b: 0 }))}");
    println(f"{tag(mk(1).cmp(mk(2)))}");
    println(f"{tag(suit().cmp(Suit.Spades))}");
    let v = [P { a: 1, b: 2 }, P { a: 1, b: 3 }];
    println(f"{tag(v[0].cmp(v[1]))}");
    let t = (P { a: 5, b: 0 }, P { a: 1, b: 0 });
    println(f"{tag(t.0.cmp(t.1))}");
    println(f"{tag(Rev { v: 1 }.cmp(Rev { v: 2 }))}");
}
"#,
    );
    assert_eq!(out, "0\n1\n2\n0\n0\n0\n2\n2\n");
}

/// `binary_search` carries element signedness under the tree-walker
/// (B-2026-08-28-6).
///
/// The tree-walker's `sort` has ordered `Vec[u64]` / `Vec[usize]` unsigned
/// since B-2026-07-04-8, but its `binary_search` arm kept calling the signed
/// `value_compare`. So the two disagreed WITHIN one backend: `sort` produced
/// `[1, 2, u64.MAX]`, `is_sorted` called it sorted, and `binary_search` then
/// failed to find elements that are in it.
///
/// Codegen's binary_search was signed too, so the E2E twin in
/// `tests/codegen.rs` could not have caught this — both backends returned the
/// same wrong `None`. This is the only coverage of it on the default
/// `cargo test` leg, and it pins literal output for the same reason.
///
/// The signed row is the over-correction guard: reading every integer as
/// unsigned would satisfy the rows above it.
#[test]
fn binary_search_carries_element_signedness_under_the_tree_walker() {
    let out = run_no_errors(
        r#"
fn main() {
    let a: Vec[u64] = [1u64, 2u64, 18446744073709551615u64];
    let s = a.is_sorted();
    println(f"{s}");
    let r1 = a.binary_search(2u64);
    match r1 { Some(i) => println(f"{i}"), None => println("absent"), }
    let r2 = a.binary_search(18446744073709551615u64);
    match r2 { Some(i) => println(f"{i}"), None => println("absent"), }

    let b: Vec[usize] = [1u64 as usize, 9223372036854775808u64 as usize];
    let r3 = b.binary_search(9223372036854775808u64 as usize);
    match r3 { Some(i) => println(f"{i}"), None => println("absent"), }

    let c: Vec[i64] = [0 - 5, 0 - 1, 3];
    let r4 = c.binary_search(0 - 5);
    match r4 { Some(i) => println(f"{i}"), None => println("absent"), }
}
"#,
    );
    // sorted / 2 at index 1 / MAX at index 2 / 2^63 at index 1 / -5 first.
    assert_eq!(out, "true\n1\n2\n1\n0\n");
}

/// `.cmp()` carries operand signedness under the tree-walker
/// (B-2026-08-28-5).
///
/// The tree-walker stores every integer width in an i64/i128 carrier, so a
/// `u64` / `usize` value at or above 2^63 rides as a NEGATIVE two's-complement
/// number. `<` has reinterpreted those bits since B-2026-07-04-8; `.cmp` never
/// did, so `u64.MAX.cmp(1u64)` answered `Less`.
///
/// This half is invisible to the E2E twin in `tests/codegen.rs`, and was
/// invisible to every run-vs-build differential the repo has: codegen's `.cmp`
/// was hardcoded signed too, so the two backends AGREED on the wrong answer.
/// It only surfaced when the compiled side was fixed and the twin started
/// failing. Pinning literal output here is what keeps it from regressing back
/// into agreement, and this is also its only coverage on the default
/// `cargo test` leg.
///
/// The `bool` rows belong with the integers rather than in a test of their
/// own: `bool` is an unsigned 1-bit integer, and it is the member of this
/// class that the compiled backends got wrong.
#[test]
fn cmp_carries_operand_signedness_under_the_tree_walker() {
    let out = run_no_errors(
        r#"
fn tag(o: Ordering) -> i64 {
    if o.is_lt() { return 0; }
    if o.is_eq() { return 1; }
    return 2;
}

fn main() {
    let a: u8 = 200;
    let b: u8 = 100;
    let c: u64 = 18446744073709551615u64;
    let d: u64 = 1;
    let e: usize = 9223372036854775808u64 as usize;
    let f: usize = 1;
    let g: i64 = 0 - 5;
    let h: i64 = 1;
    println(f"{tag(a.cmp(b))}");
    println(f"{tag(c.cmp(d))}");
    println(f"{tag(e.cmp(f))}");
    println(f"{tag(g.cmp(h))}");
    println(f"{tag(false.cmp(true))}");
    println(f"{tag(true.cmp(false))}");
    println(f"{tag(true.cmp(true))}");
}
"#,
    );
    // 200 > 100, u64.MAX > 1, 2^63 > 1 — then the SIGNED row, which must stay
    // signed: -5 < 1 is `Less`, and would read as `Greater` if the fix had
    // made every integer unsigned instead of consulting the operand's type.
    assert_eq!(out, "2\n2\n2\n0\n0\n2\n1\n");
}

#[test]
fn test_interp_refinement_as_cast_ok() {
    // `4 as Even` passes the predicate; the (layout-identical) base value
    // flows through and prints, with no runtime fault.
    let output = run_no_errors(
        r#"
type Even = i64 where self % 2 == 0;
fn main() {
    let e = 4 as Even;
    println(e);
}
"#,
    );
    assert_eq!(output.trim(), "4");
}

#[test]
fn test_interp_refinement_as_cast_violation_faults() {
    // `3 as Even` is the asserting construction form: a false predicate is a
    // contract violation, surfaced as a runtime fault (not a recoverable Err).
    let errors = runtime_errors(
        r#"
type Even = i64 where self % 2 == 0;
fn main() {
    let e = 3 as Even;
    println(e);
}
"#,
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("contract violated") && e.message.contains("Even")),
        "expected a contract-violation fault naming `Even`, got: {errors:?}"
    );
}

#[test]
fn test_cpu_supports_intrinsic() {
    // `cpu.supports("...") -> bool` — interpreter twin of the codegen probe
    // (parity with tests/codegen.rs::test_e2e_cpu_supports_intrinsic). Assert
    // only host-independent facts: an unknown feature is false; a real feature
    // yields a usable bool.
    let out = run_no_errors(
        r#"
fn main() {
    println(cpu.supports("not-a-real-feature-xyz"));
    let a = cpu.supports("avx2");
    println(a == a);
}
"#,
    );
    assert_eq!(out, "false\ntrue\n");
}

#[test]
fn alloc_error_prelude_type_usable_without_import() {
    // The `AllocError` prelude type (phase-8 § Fallible Allocation API) is
    // available without import as `Result[T, AllocError]`, constructs both
    // variants (struct + unit), compares with `==`, renders via Display, and
    // pattern-matches with field binding.
    //
    // The Display line expected `"CapacityOverflow"` — the DERIVED enum shape
    // — until B-2026-08-25-34 gave the type the prose design.md § Fallible
    // Allocation actually pins ("capacity overflow" on this arm). The old
    // expectation was written against what the compiler did, not what the spec
    // said, so it passed while the spec sentence was false. Recorded here so
    // it does not get "fixed" back.
    let output = run(r#"
fn try_make(fail: bool) -> Result[i64, AllocError] {
    if fail {
        Err(AllocError.OutOfMemory { requested_bytes: 64 })
    } else {
        Ok(1)
    }
}

fn main() {
    let oom = AllocError.OutOfMemory { requested_bytes: 64 };
    let oom2 = AllocError.OutOfMemory { requested_bytes: 64 };
    let co = AllocError.CapacityOverflow;
    println(f"{oom == oom2}");
    println(f"{oom == co}");
    println(f"{co}");
    match try_make(true) {
        Ok(_) => println("ok"),
        Err(AllocError.OutOfMemory { requested_bytes }) => println(f"oom:{requested_bytes}"),
        Err(AllocError.CapacityOverflow) => println("co"),
    }
}
"#);
    assert_eq!(output, "true\nfalse\ncapacity overflow\noom:64\n");
}

// ── Module-level `let` / `let mut` bindings in the interpreter ──────
//
// Codegen emits these as globals and `run_program` (build+run) covered
// them, but the interpreter never evaluated/bound them — a module-level
// `let COUNT = 42` read from any function panicked ("variable not found;
// should be caught by resolver"), a run-vs-build divergence. These pin the
// run side; the codegen tests (tests/codegen.rs `test_e2e_modbind_*`) pin
// the identical build output.

#[test]
fn test_module_binding_immutable_read() {
    let out = run_no_errors(
        "let COUNT: i64 = 42i64;\n\
         let FLAG: bool = true;\n\
         fn main() {\n\
             println(COUNT);\n\
             if FLAG { println(1i64); } else { println(0i64); }\n\
         }",
    );
    assert_eq!(out, "42\n1\n");
}

#[test]
fn test_module_binding_computed_cross_referencing() {
    // Parity with tests/codegen.rs::test_e2e_modbind_computed_cross_referencing_initializer
    // (B-2026-07-11-16). A computed initializer referencing another binding.
    let out = run_no_errors(
        "let COUNT: i64 = 42i64;\n\
         let DOUBLED: i64 = COUNT * 2i64;\n\
         let TRIPLED: i64 = DOUBLED + COUNT;\n\
         fn main() {\n\
             println(COUNT);\n\
             println(DOUBLED);\n\
             println(TRIPLED);\n\
         }",
    );
    assert_eq!(out, "42\n84\n126\n");
}

#[test]
fn test_module_binding_computed_unannotated() {
    // Parity with tests/codegen.rs::test_e2e_modbind_computed_unannotated_initializer
    // (B-2026-07-11-16 residual). Computed bindings with NO `: TYPE`
    // annotation — the interpreter evaluates the value expr regardless of
    // annotation, so this always worked on the run side; the codegen side now
    // matches by sizing the global from the inferred type.
    let out = run_no_errors(
        "let COUNT: i64 = 42i64;\n\
         let DOUBLED = COUNT * 2i64;\n\
         let TRIPLED = DOUBLED + COUNT;\n\
         let SMALL: i32 = 7i32;\n\
         let SMALL2 = SMALL + 3i32;\n\
         fn main() {\n\
             println(DOUBLED);\n\
             println(TRIPLED);\n\
             println(SMALL2);\n\
         }",
    );
    assert_eq!(out, "84\n126\n10\n");
}

/// B-2026-08-25-32 — `PriorityQueue.peek`: read the root WITHOUT removing it.
///
/// `peek` returns `Option[T]`, not the `Option[ref T]` Rust's
/// `BinaryHeap::peek` hands back, because a Kāra body cannot construct a
/// borrow-carrying `Option` — see the method's own note in
/// `runtime/stdlib/priority_queue.kara`. The root is therefore COPIED, and
/// the properties worth pinning are that the copy is faithful and that the
/// queue is left alone.
///
/// Both element classes appear for the reason the sibling
/// `..._min_and_max_at_scalar_and_heap_t` states: a scalar `T` rides the
/// all-`i64` base layout, so a `String` failure hides completely behind an
/// `i64`-only fixture (B-2026-08-25-7 looked correct at `T = i64` while every
/// `String` came back empty). Both DIRECTIONS appear because `outranks` is the
/// one branch that differs, and a `peek` that read index 0 without the heap
/// property holding would still look right on a min-first queue.
///
/// The non-disturbance assertions are the load-bearing ones: peek twice and
/// the same element must come back, `len` must not move, and the following
/// `pop` must return exactly what `peek` promised. A `peek` that moved the
/// root out would satisfy the first read and fail all three.
///
/// Twin of `tests/codegen.rs`'s `e2e_stdlib_priority_queue_peek_reads_the_root`,
/// asserting the SAME string: this module's bodies are real Kāra compiled
/// through the baked-stdlib path, so interpreter/codegen parity is what keeps
/// one source honest across two backends.
#[test]
fn test_priority_queue_peek_reads_the_root_without_removing_it() {
    let out = run(r#"
fn main() {
    let e: PriorityQueue[i64] = PriorityQueue.new();
    match e.peek() { Some(v) => { println(v); } None => { println("none"); } }
    let mut q: PriorityQueue[i64] = PriorityQueue.new();
    q.push(5); q.push(1); q.push(4); q.push(9);
    match q.peek() { Some(v) => { println(v); } None => {} }
    match q.peek() { Some(v) => { println(v); } None => {} }
    println(q.len());
    match q.pop() { Some(v) => { println(v); } None => {} }
    match q.peek() { Some(v) => { println(v); } None => {} }
    println(q.len());
    let mut m: PriorityQueue[i64] = PriorityQueue.max_first();
    m.push(5); m.push(1); m.push(4); m.push(9);
    match m.peek() { Some(v) => { println(v); } None => {} }
    let h = PriorityQueue.from([9, 7, 8, 1, 3]);
    match h.peek() { Some(v) => { println(v); } None => {} }
    let hm = PriorityQueue.max_first_from([9, 7, 8, 1, 3]);
    match hm.peek() { Some(v) => { println(v); } None => {} }
    let mut s: PriorityQueue[String] = PriorityQueue.new();
    s.push("pear"); s.push("apple"); s.push("fig");
    match s.peek() { Some(v) => { println(v); } None => {} }
    match s.peek() { Some(v) => { println(v); } None => {} }
    println(s.len());
    match s.pop() { Some(v) => { println(v); } None => {} }
    match s.peek() { Some(v) => { println(v); } None => {} }
}
"#);
    assert_eq!(
        out,
        "none\n1\n1\n4\n1\n4\n3\n9\n1\n9\napple\napple\n3\napple\nfig\n"
    );
}

/// B-2026-08-25-32 — the streaming median over two queues, which is the use
/// case the row was filed from (LeetCode 295, kata 295) and the reason `peek`
/// is worth having at all.
///
/// Hold a max-first queue over the lower half and a min-first queue over the
/// upper half; the median is THE TWO ROOTS. That query is O(1) only because
/// reading a root is O(1) — without `peek` the textbook algorithm has to
/// `pop` and `push` back (O(log n) twice, and mutating where a reader wants
/// `ref`), so it cannot be written at its textbook cost.
///
/// The medians are cross-checked against an independent oracle (Python
/// `bisect.insort` + midpoint over the same feed): 5, 10, 5, 4, 5, 6, 7, 7,
/// 8, 7. The trailing `lo.len() + hi.len()` is the non-disturbance assertion
/// that matters most here — after ten iterations each calling `peek` up to
/// twice, all ten elements must still be in the queues. A `peek` that removed
/// what it read would print a number below 10 while every median above it
/// stayed plausible.
///
/// Twin of `tests/codegen.rs`'s
/// `e2e_stdlib_priority_queue_peek_drives_a_streaming_median`.
#[test]
fn test_priority_queue_peek_drives_a_streaming_median() {
    let out = run(r#"
fn main() {
    let mut lo: PriorityQueue[i64] = PriorityQueue.max_first();
    let mut hi: PriorityQueue[i64] = PriorityQueue.new();
    let feed: Vec[i64] = [5, 15, 1, 3, 8, 7, 9, 10, 20, 2];
    let mut k = 0;
    while k < feed.len() {
        lo.push(feed[k]);
        match lo.pop() { Some(t) => { hi.push(t); } None => {} }
        if hi.len() > lo.len() {
            match hi.pop() { Some(t) => { lo.push(t); } None => {} }
        }
        if lo.len() == hi.len() {
            match lo.peek() {
                Some(a) => {
                    match hi.peek() { Some(b) => { println((a + b) / 2); } None => {} }
                }
                None => {}
            }
        } else {
            match lo.peek() { Some(a) => { println(a); } None => {} }
        }
        k = k + 1;
    }
    println(lo.len() + hi.len());
}
"#);
    assert_eq!(out, "5\n10\n5\n4\n5\n6\n7\n7\n8\n7\n10\n");
}

#[test]
fn test_secret_expose_reads_inner() {
    // `std.secret.Secret[T]` — `Secret.new(v)` wraps a sensitive value and
    // `.expose()` reads the inner value back (both Copy `i64` and non-Copy
    // `String`). The `import` is required — `Secret` is a gated module, not
    // in the prelude.
    let out = run(r#"
import std.secret.{Secret};
fn main() {
    let s = Secret.new(42);
    let v = s.expose();
    println(v);
    let t = Secret.new("hunter2");
    let w = t.expose();
    println(w);
}
"#);
    assert_eq!(out, "42\nhunter2\n");
}

#[test]
fn test_secret_ct_eq() {
    // `std.secret.Secret[String].ct_eq(other)` — constant-time equality. The
    // interpreter upholds the boolean contract (constant time has no observable
    // effect in a tree-walk); the runtime helper `karac_secret_ct_eq` provides
    // the timing guarantee under codegen. Matches codegen's
    // `test_e2e_secret_ct_eq`. Equal contents → true; differing contents of the
    // same OR different length → false.
    let out = run(r#"
import std.secret.{Secret};
fn main() {
    let a = Secret.new("s3cr3t-token-01");
    let b = Secret.new("s3cr3t-token-01");
    let c = Secret.new("s3cr3t-token-99");
    let d = Secret.new("short");
    println(a.ct_eq(b));
    println(a.ct_eq(c));
    println(a.ct_eq(d));
}
"#);
    assert_eq!(out, "true\nfalse\nfalse\n");
}

// ── VolatileCell (MMIO wrapper) ─────────────────────────────────

#[test]
fn test_volatile_cell_rejected_under_tree_walk_interpreter() {
    // `VolatileCell[T]` is a compiled-mode primitive: its `.read()` / `.write(v)`
    // execute the baked method bodies, which call the `volatile_read` /
    // `volatile_write` intrinsics — and the tree-walk interpreter has no
    // raw-pointer representation, so it rejects loudly with a "compile with
    // `karac build`" hint (same posture as the raw volatile intrinsics and the
    // `ptr.*` surface). AOT `karac build` and the default JIT `karac run` both
    // lower it correctly (see the codegen E2E
    // `test_e2e_volatile_cell_read_write_roundtrip`); only the pure tree-walk
    // path (this `run_program` API / `KARAC_RUN_JIT=0`) rejects.
    let errors = runtime_errors(
        "fn main() {\n\
             let mut reg: VolatileCell[i32] = VolatileCell.new(7);\n\
             reg.write(42);\n\
             println(reg.read());\n\
         }\n",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("volatile") && e.message.contains("karac build")),
        "expected a volatile compiled-mode rejection, got: {:?}",
        errors.iter().map(|e| e.message.clone()).collect::<Vec<_>>()
    );
}

// ── f16 / bf16 primitives ───────────────────────────────────────

#[test]
fn test_f16_bf16_arithmetic_tree_walk() {
    // f16 / bf16 run under the tree-walk interpreter (computed at f64 and
    // rounded back to the declared width after each operator — B-2026-08-14-7).
    // Uses values exactly representable in f16/bf16 (halves), so these lines
    // matched the compiled backends even before that rounding existed; the
    // non-exact values it fixed (`1.0bf16 + 0.01bf16` read 1.010009765625 here
    // against 1.0078125 compiled) are pinned in
    // `narrow_float_arithmetic_rounds_to_declared_width` below.
    let out = run_no_errors(
        "fn main() {\n\
             let x: f16 = 1.5f16;\n\
             let y: f16 = 2.25f16;\n\
             println(x + y);\n\
             let a: bf16 = 3.0bf16;\n\
             let b: bf16 = 0.5bf16;\n\
             println(a * b);\n\
         }\n",
    );
    assert_eq!(out, "3.75\n1.5\n");
}

// ── critical_section (interrupt-mask RAII guard) ────────────────

#[test]
fn test_critical_section_inert_under_tree_walk_interpreter() {
    // Unlike the raw MMIO surface, `critical_section.acquire()` runs under the
    // tree-walk interpreter: a single-threaded tree-walk has no real
    // interrupts, so acquire is inert (returns the guard) and the guard's Drop
    // is a no-op (`try_eval_builtin_drop`) — the same posture the memory
    // `fence` intrinsics take. The program runs to completion, guard-drop and
    // all, with no runtime error. (AOT/JIT lower it to the runtime mask calls;
    // see `test_e2e_critical_section_run`.)
    let out = run_no_errors(
        "fn work() writes(Hardware) {\n\
             let _guard = critical_section.acquire();\n\
             println(42);\n\
         }\n\
         fn main() {\n\
             work();\n\
             println(99);\n\
         }\n",
    );
    assert_eq!(out, "42\n99\n");
}

/// B-2026-09-02-2 — AN AGGREGATE LITERAL IN AN ESCAPING POSITION CONSUMES ITS
/// SOURCES. `return W { r: r }` hands `r` to the caller exactly as `return r`
/// does, so ONE `Drop` body is due, at the caller's binding death. The
/// interpreter ran it twice -- `mid dR14 v14 dR14 post` against
/// `mid v14 dR14 post` from all three compiled backends -- because
/// `suppress_tail_expr_user_drop`, the hook both escaping positions drive, read
/// a bare `Identifier` and nothing else, so a source one aggregate deeper was
/// never retracted from the frame's cleanup.
///
/// The TUPLE spelling of the same move was already correct, and that asymmetry
/// is what located the cause. Both escaping positions also call
/// `record_container_bodies_move_sources`, whose `Tuple`/`ArrayLiteral` arms
/// route through `record_container_move_sources_in_aggregate_arg` and put a
/// source carrying its own `impl Drop` on the whole-value channel
/// (B-2026-08-02-27 / B-2026-08-29-45), while its `StructLiteral` arm keeps the
/// container-only recording on purpose: the whole-value channel is wrong for the
/// DISCARD position, where no struct-literal discard walk takes over the
/// retracted body. `discard-guard` is that line, and both backends draw it in
/// the same place. So the struct literal's escaping half went into the hook only
/// the escaping positions reach.
///
/// `enclosing-block-guard` is the shape that was ALREADY correct and says why:
/// a `return` nested one block deeper leaves `r` out of THIS frame's cleanup, so
/// the retraction cannot see it and `record_conditional_move_tail` -- widened to
/// aggregates by B-2026-08-31-35 -- carries it instead. The fix here is the same
/// widening, one hook over.
///
/// `enum-field` and `two-fields` are shapes the row never named and the same
/// change fixes; `sibling-not-moved` and `container-field-control` are the
/// guards in the other direction, where a too-eager retraction would LOSE a body
/// rather than duplicate one.
///
/// Twin of
/// `codegen::e2e_an_escaping_aggregate_literal_consumes_its_sources`, asserting
/// the same strings so a later one-sided edit cannot re-negotiate them.
#[test]
fn an_escaping_aggregate_literal_consumes_its_sources() {
    const H: &str = "struct R { id: i64, tag: String }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
         enum E { A(i64), B }\n\
         impl Drop for E { fn drop(mut ref self) { println(\"dE\") } }\n\
         struct W { r: R }\n\
         struct Outer { w: W }\n\
         struct Two { a: R, b: R }\n\
         struct Box3 { xs: Vec[R] }\n\
         struct Wrap { e: E }\n\
         fn t_lit(k: i64) -> W { let r: R = R { id: k, tag: f\"t\" }; println(\"mid\"); return W { r: r } }\n\
         fn s_lit(k: i64) -> W { let r: R = R { id: k, tag: f\"t\" }; println(\"mid\"); return W { r: r }; }\n\
         fn b_lit(k: i64) -> W { let r: R = R { id: k, tag: f\"t\" }; println(\"mid\"); W { r: r } }\n\
         fn n_lit(k: i64) -> Outer { let r: R = R { id: k, tag: f\"t\" }; println(\"mid\"); return Outer { w: W { r: r } } }\n\
         fn two_lit(k: i64) -> Two { let x: R = R { id: k, tag: f\"x\" }; let y: R = R { id: k + 1, tag: f\"y\" }; println(\"mid\"); return Two { a: x, b: y } }\n\
         fn sib_lit(k: i64) -> W { let x: R = R { id: k, tag: f\"x\" }; let y: R = R { id: k + 50, tag: f\"y\" }; println(f\"mid{y.id}\"); return W { r: x } }\n\
         fn enum_lit(k: i64) -> Wrap { let e: E = E.A(k); println(\"mid\"); return Wrap { e: e } }\n\
         fn tup_lit(k: i64) -> (R, i64) { let r: R = R { id: k, tag: f\"t\" }; println(\"mid\"); return (r, 3) }\n\
         fn named_lit(k: i64) -> W { let r: R = R { id: k, tag: f\"t\" }; println(\"mid\"); let w: W = W { r: r }; return w }\n\
         fn box_lit(k: i64) -> Box3 { let xs: Vec[R] = [R { id: k, tag: f\"t\" }]; println(\"mid\"); return Box3 { xs: xs } }\n\
         fn cond_lit(k: i64) -> W { let r: R = R { id: k, tag: f\"t\" }; println(\"mid\"); if k > 0 { return W { r: r } } return W { r: R { id: 0, tag: f\"z\" } } }\n\
         fn fresh_lit(k: i64) -> W { println(\"mid\"); return W { r: R { id: k, tag: f\"t\" } } }\n";
    for (label, body, want) in [
        // THE ROW, in its three escaping spellings: a `return` statement, a
        // tail `return` with no semicolon, and a bare tail. All three hand the
        // literal out, so all three print one body.
        (
            "lit-return-stmt",
            "let v: W = s_lit(1); println(f\"v{v.r.id}\");\n",
            "mid\nv1\ndR1\npost\n",
        ),
        (
            "lit-tail-return",
            "let v: W = t_lit(2); println(f\"v{v.r.id}\");\n",
            "mid\nv2\ndR2\npost\n",
        ),
        (
            "lit-bare-tail",
            "let v: W = b_lit(3); println(f\"v{v.r.id}\");\n",
            "mid\nv3\ndR3\npost\n",
        ),
        // NESTED literals -- the source walk has to recurse, exactly as the
        // container-move and conditional-move walks already do.
        (
            "nested-literal",
            "let v: Outer = n_lit(4); println(f\"v{v.w.r.id}\");\n",
            "mid\nv4\ndR4\npost\n",
        ),
        // TWO consumed sources in one literal, and an ENUM-typed source: both
        // shapes the row never named, both fixed by the same widening. Fields
        // die in reverse declaration order (design.md § Drop ordering).
        (
            "two-fields",
            "let v: Two = two_lit(10); println(f\"v{v.a.id}-{v.b.id}\");\n",
            "mid\nv10-11\ndR11\ndR10\npost\n",
        ),
        (
            "enum-field",
            "let v: Wrap = enum_lit(30); println(\"v\");\n",
            "mid\ndE\nv\npost\n",
        ),
        // GUARD: only the CONSUMED source is retracted. `y` is never moved
        // into the literal, so it still dies inside `sib_lit`.
        (
            "sibling-not-moved",
            "let v: W = sib_lit(20); println(f\"v{v.r.id}\");\n",
            "mid70\ndR70\nv20\ndR20\npost\n",
        ),
        // The three spellings that were ALREADY correct, kept as controls so a
        // later edit cannot regress them into the fixed one's shape. The tuple
        // is the one that located the cause (a second channel, see the doc
        // comment); the named binding records the move at its `let`; the
        // container source resolves to a `Value::Array` and is left alone.
        (
            "tuple-control",
            "let v: (R, i64) = tup_lit(40); println(f\"v{v.0.id}\");\n",
            "mid\nv40\ndR40\npost\n",
        ),
        (
            "named-binding-control",
            "let v: W = named_lit(50); println(f\"v{v.r.id}\");\n",
            "mid\nv50\ndR50\npost\n",
        ),
        (
            "container-field-control",
            "let v: Box3 = box_lit(60); println(f\"v{v.xs.len()}\");\n",
            "mid\nv1\ndR60\npost\n",
        ),
        // GUARD: a `return` one block deeper. `r` is not in the `if`-block's
        // cleanup, so the retraction is a no-op and the conditional-move set
        // has to carry it -- the path that was already right.
        (
            "enclosing-block-guard",
            "let v: W = cond_lit(70); println(f\"v{v.r.id}\");\n",
            "mid\nv70\ndR70\npost\n",
        ),
        // GUARD: no local is consumed at all, so nothing is retracted.
        (
            "fresh-temp-guard",
            "let v: W = fresh_lit(80); println(f\"v{v.r.id}\");\n",
            "mid\nv80\ndR80\npost\n",
        ),
        // GUARD: the DISCARD position, which is NOT escaping and keeps its own
        // channel. Widening the shared dispatcher instead of this hook would
        // have retracted `r0` here with no discard walk to take the body over,
        // losing it entirely.
        (
            "discard-guard",
            "let r0: R = R { id: 90, tag: f\"t\" }; println(\"mid\"); let _ = W { r: r0 };\n",
            "mid\ndR90\npost\n",
        ),
    ] {
        let src = format!("{H}fn main() {{ {body} println(\"post\") }}\n");
        assert_eq!(run(&src), want, "{label}");
    }
}

/// B-2026-08-01-15 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_param_view_rebind_single_caller_fire`, same source and expected
/// string. Pre-fix the interpreter fired the destructured field's body a
/// second time inside the callee (the rebind escaped the depth-0 param
/// gate); the view-ness propagation in `let_destructures_owned_param`
/// leaves exactly the caller's single fire.
#[test]
fn test_param_view_rebind_single_caller_fire() {
    assert_eq!(
        run("struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"drop {self.id} {self.name}\")\n\
                 }\n\
             }\n\
             struct Holder { r: Res }\n\
             fn take(h: Holder) {\n\
                 let h2 = h;\n\
                 let Holder { r } = h2;\n\
                 println(f\"got {r.id}\");\n\
                 println(\"take done\");\n\
             }\n\
             fn take2(h: Holder) {\n\
                 let h2 = h;\n\
                 println(f\"held {h2.r.id}\");\n\
                 println(\"take2 done\");\n\
             }\n\
             fn main() {\n\
                 println(\"a\");\n\
                 let x = Holder { r: Res { id: 5, name: f\"y{5}\" } };\n\
                 take(x);\n\
                 println(\"b\");\n\
                 take2(Holder { r: Res { id: 7, name: f\"y{7}\" } });\n\
                 println(\"end\");\n\
             }\n"),
        "a\ngot 5\ntake done\ndrop 5 y5\nb\nheld 7\ntake2 done\ndrop 7 y7\nend\n"
    );
}

/// B-2026-08-14-14 — the interpreter twin of
/// `tests/codegen.rs::test_e2e_element_wise_scalar_as_cast_agrees_across_backends`,
/// same source and same expected string.
///
/// This is the surface that TRAPPED. `tu + x` with `tu: Tensor[u8]` and
/// `x: i64 = 300` raised "runtime error: integer overflow" here while the
/// binary printed 45 — the typechecker had admitted a scalar the element type
/// cannot hold, so the two backends met it differently: an honest trap on one,
/// a silent two's-complement wrap on the other. With the mismatch rejected and
/// the truncation written as `as u8`, both agree on 45.
#[test]
fn test_element_wise_scalar_as_cast_agrees_across_backends() {
    assert_eq!(
        run("fn main() {\n\
                 let tu: Tensor[u8, [2]] = Tensor.from([1, 2]);\n\
                 let x: i64 = 300;\n\
                 let r = tu + (x as u8);\n\
                 println(r[0]);\n\
                 let t32: Tensor[f32, [2]] = Tensor.from([1.0, 2.0]);\n\
                 let d: f64 = 0.1;\n\
                 let s = t32 * (d as f32);\n\
                 println(s[0]);\n\
                 let n = t32 * -1.0;\n\
                 println(n[0]);\n\
             }"),
        "45\n0.10000000149011612\n-1\n"
    );
}

/// B-2026-08-02-12 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_expression_position_container_ctors`, same source and expected
/// string (the interp always built real handles for expression-position
/// `Map.new()` / `Set.new()` / `SortedMap.new()` — this pins the parity
/// codegen now shares).
#[test]
fn test_expression_position_container_ctors() {
    assert_eq!(
        run("fn main() {\n\
                 let mut v: Vec[Map[String, i64]] = Vec.new();\n\
                 v.push(Map.new());\n\
                 v.push(Map.new());\n\
                 let _ = v[0].insert(f\"k{1}\", 5);\n\
                 match v[0].get(f\"k{1}\") { Some(x) => println(f\"got {x}\"), None => println(\"miss\") }\n\
                 println(f\"m0 {v[0].len()} m1 {v[1].len()}\");\n\
                 let mut g: Vec[Map[String, i64]] = Vec.filled(2, Map.new());\n\
                 let _ = g[0].insert(f\"a{2}\", 7);\n\
                 println(f\"g0 {g[0].len()} g1 {g[1].len()}\");\n\
                 let mut s: Vec[Set[i64]] = Vec.new();\n\
                 s.push(Set.new());\n\
                 let _ = s[0].insert(9i64);\n\
                 println(f\"s0 {s[0].len()}\");\n\
                 let mut d: Vec[SortedMap[i64, i64]] = Vec.filled(2, SortedMap.new());\n\
                 let _ = d[0].insert(7i64, 70i64);\n\
                 println(f\"d0 {d[0].len()} d1 {d[1].len()}\");\n\
                 let mut q: VecDeque[Map[String, i64]] = VecDeque.new();\n\
                 q.push_back(Map.new());\n\
                 let _ = q[0].insert(f\"z{3}\", 4);\n\
                 match q[0].get(f\"z{3}\") { Some(x) => println(f\"qgot {x}\"), None => println(\"qmiss\") }\n\
                 let mut w: Vec[Set[i64]] = Vec.new();\n\
                 w.insert(0, Set.new());\n\
                 let _ = w[0].insert(9i64);\n\
                 println(f\"w0 {w[0].len()}\");\n\
                 println(\"end\");\n\
             }\n"),
        "got 5\nm0 1 m1 0\ng0 1 g1 0\ns0 1\nd0 1 d1 0\nqgot 4\nw0 1\nend\n"
    );
}

/// B-2026-07-30-5 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_deque_head_fifo_semantics`, same source and expected string. The
/// interpreter's VecDeque is untouched by the codegen head-index lowering;
/// this is the ORACLE half that pins the FIFO semantics codegen's rewritten
/// pop/len/is_empty arms must keep matching.
#[test]
fn test_deque_head_fifo_semantics() {
    assert_eq!(
        run("fn main() {\n\
                 let mut q: VecDeque[i64] = VecDeque.new();\n\
                 let mut i = 0;\n\
                 while i < 1000 {\n\
                     q.push_back(i);\n\
                     i = i + 1;\n\
                 }\n\
                 let mut acc = 0;\n\
                 while not q.is_empty() {\n\
                     match q.pop_front() { Some(x) => { acc = acc + x; } None => {} }\n\
                 }\n\
                 println(acc);\n\
                 let mut p: VecDeque[i64] = VecDeque.new();\n\
                 let mut j = 0;\n\
                 while j < 10 {\n\
                     p.push_back(j * 10);\n\
                     j = j + 1;\n\
                 }\n\
                 match p.pop_front() { Some(x) => { println(x); } None => {} }\n\
                 match p.pop_front() { Some(x) => { println(x); } None => {} }\n\
                 p.push_back(999);\n\
                 println(p.len());\n\
                 let mut d: VecDeque[i64] = VecDeque.new();\n\
                 let mut k = 0;\n\
                 while k < 4 {\n\
                     d.push_back(k);\n\
                     k = k + 1;\n\
                 }\n\
                 while not d.is_empty() {\n\
                     match d.pop_front() { Some(_) => {} None => {} }\n\
                 }\n\
                 d.push_back(77);\n\
                 match d.pop_front() { Some(x) => { println(x); } None => {} }\n\
                 println(d.len());\n\
             }\n"),
        "499500\n0\n10\n9\n77\n0\n"
    );
}

#[test]
fn test_default_parameter_call_site_fill_oracle() {
    // Oracle twin of `tests/codegen.rs`'s
    // `test_e2e_default_parameter_call_site_fill` (B-2026-08-17-19): the
    // stored defaults are filled at the call site, so the three backends
    // agree on what an omitted / label-skipped argument evaluates to.
    let out = run("\n\
         fn create_server(host: i64, port: i64 = 8080, max_connections: i64 = 1000, \
             timeout_ms: i64 = 5000) -> i64 {\n\
             host + port + max_connections + timeout_ms\n\
         }\n\
         fn main() {\n\
             println(create_server(1));\n\
             println(create_server(1, 9090));\n\
             println(create_server(1, max_connections: 100));\n\
             println(create_server(1, 9090, max_connections: 100, timeout_ms: 250));\n\
         }\n");
    assert_eq!(out, "14081\n15091\n13181\n9441\n");
}

#[test]
fn test_derive_dependency_auto_fill_oracle() {
    // Oracle twin of `tests/codegen.rs`'s
    // `test_e2e_derive_dependency_auto_fill` (B-2026-08-17-33). Filling the
    // dependency in must produce WORKING derives, not just a quiet
    // typechecker: `Copy` leaves the source binding usable, and the
    // auto-filled `Eq`/`PartialEq` behind `Hash` are what make two equal keys
    // collapse to one Map entry.
    let out = run("\n\
         #[derive(Copy)]\n\
         struct C { a: i64 }\n\
         #[derive(Hash)]\n\
         struct K { a: i64, b: i64 }\n\
         fn main() {\n\
             let x = C { a: 3 };\n\
             let y = x;\n\
             println(\"copy = \" + x.a.to_string() + \",\" + y.a.to_string());\n\
             let mut m: Map[K, i64] = Map.new();\n\
             m.insert(K { a: 1, b: 2 }, 10);\n\
             m.insert(K { a: 1, b: 2 }, 20);\n\
             m.insert(K { a: 9, b: 9 }, 30);\n\
             println(\"len = \" + m.len().to_string());\n\
             match m.get(K { a: 1, b: 2 }) {\n\
                 Some(v) => println(\"k12 = \" + v.to_string()),\n\
                 None => println(\"k12 missing\"),\n\
             }\n\
         }\n");
    assert_eq!(out, "copy = 3,3\nlen = 2\nk12 = 20\n");
}

/// `MIN / -1` and `MIN % -1` trap, and the SECOND of those is the one a result
/// range-check cannot catch.
///
/// `MIN % -1` is `0` — it fits every width — so nothing about the result says
/// "overflow". The i64 carrier used to report it anyway, because the
/// intermediate division overflowed the carrier itself; `checked_rem` returned
/// `None`. With a wider carrier the operation succeeds and returns 0, so the
/// case is now an explicit width check (`div_overflows_at_width`).
///
/// The `%` half had NO test before this one — it was found by reasoning about
/// what the carrier had been doing for free, not by a failure. `div_euclid` /
/// `rem_euclid` are the same pair one level up and were covered.
#[test]
fn min_div_and_rem_by_negative_one_trap() {
    for op in ["/", "%"] {
        let errors = runtime_errors(&format!(
            "fn main() {{ let m: i64 = -9223372036854775807i64 - 1i64; \
             let n: i64 = 0i64 - 1i64; println(m {op} n); }}"
        ));
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("integer overflow")),
            "expected a MIN {op} -1 overflow trap, got: {:?}",
            errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }
}

/// Ordinary arithmetic is unchanged — values, signs and division semantics all
/// survive the carrier swap. Cheap, and it would catch a sign-extension or
/// truncation slip in the 200-odd conversion sites the widening touched.
#[test]
fn ordinary_integer_arithmetic_is_unchanged_by_the_carrier() {
    assert_eq!(
        run_no_errors(
            "fn main() {\n\
             let a: i64 = 9223372036854775807i64;\n\
             let b: i64 = 0i64 - 9223372036854775807i64 - 1i64;\n\
             println(a);\n\
             println(b);\n\
             println(0i64 - 7i64 / 2i64);\n\
             println(0i64 - 7i64 % 2i64);\n\
             println(6i64 * 7i64);\n\
             }"
        ),
        "9223372036854775807\n-9223372036854775808\n-3\n-1\n42\n"
    );
}

// ── gpu.sum / gpu.prod under the interpreter (B-2026-08-19-10) ─────────────

#[test]
fn gpu_sum_uses_the_gpu_tree_order_not_a_left_fold() {
    // The decision this feature turns on. 64 copies of 0.1 sum to 6.400000
    // under the GPU's tree and 6.399996 under a left fold; the interpreter
    // must print the TREE value, or every compiled run of the same program
    // disagrees with `karac run`.
    let out = run_no_errors(
        "fn main() {\n\
        \x20   let mut v: Vec[f32] = [];\n\
        \x20   for i in 0..64 { v.push(0.1) }\n\
        \x20   println(f\"{gpu.sum(v)}\")\n\
        }",
    );
    // f32 6.4 widened to f64 for printing.
    assert_eq!(out.trim(), "6.400000095367432");
}

#[test]
fn gpu_sum_and_prod_compute_the_obvious_small_cases() {
    let out = run_no_errors(
        "fn main() {\n\
        \x20   let v: Vec[f32] = [1.0, 2.0, 3.0, 4.0];\n\
        \x20   println(f\"{gpu.sum(v)}\")\n\
        }",
    );
    assert_eq!(out.trim(), "10");

    let out = run_no_errors(
        "fn main() {\n\
        \x20   let v: Vec[f32] = [2.0, 3.0, 4.0];\n\
        \x20   println(f\"{gpu.prod(v)}\")\n\
        }",
    );
    assert_eq!(out.trim(), "24");
}

#[test]
fn gpu_argmin_argmax_report_indices_with_first_occurrence_ties() {
    let out = run_no_errors(
        "fn main() {\n\
        \x20   let v: Vec[f32] = [3.0, 1.0, 1.0, 5.0];\n\
        \x20   let m = gpu.argmin(v);\n\
        \x20   match m {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        }",
    );
    assert_eq!(out.trim(), "1", "ties take the FIRST occurrence");

    let out = run_no_errors(
        "fn main() {\n\
        \x20   let v: Vec[f32] = [];\n\
        \x20   let m = gpu.argmax(v);\n\
        \x20   match m {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        }",
    );
    assert_eq!(out.trim(), "empty");

    // Multi-workgroup: the winner lives past the first chunk, so the fold
    // level has to re-read its value through the surviving candidate index.
    let out = run_no_errors(
        "fn main() {\n\
        \x20   let mut v: Vec[f32] = [];\n\
        \x20   for i in 0..200 { v.push(50.0) }\n\
        \x20   v[137] = -1.0;\n\
        \x20   let m = gpu.argmin(v);\n\
        \x20   match m {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        }",
    );
    assert_eq!(out.trim(), "137");
}

#[test]
fn gpu_variance_and_stddev_are_the_population_forms() {
    // `Stats.variance` / `Stats.stddev` are POPULATION (÷ n), so these are
    // too — the two answer the same number on the same buffer. Textbook
    // example: [2,4,4,4,5,5,7,9] has mean 5, variance 4, sd 2.
    let out = run_no_errors(
        "fn main() {\n\
        \x20   let v: Vec[f32] = [2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0];\n\
        \x20   let a = gpu.variance(v);\n\
        \x20   match a {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        }",
    );
    assert_eq!(out.trim(), "4");

    let out = run_no_errors(
        "fn main() {\n\
        \x20   let v: Vec[f32] = [2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0];\n\
        \x20   let a = gpu.stddev(v);\n\
        \x20   match a {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        }",
    );
    assert_eq!(out.trim(), "2");

    // A single element has zero population variance; an empty buffer has
    // none at all. `Stats.variance` raises on empty — every GPU reduction
    // answers `None` instead, and this one is consistent with its siblings.
    for (buf, want) in [("[3.0]", "0"), ("[]", "empty")] {
        let out = run_no_errors(&format!(
            "fn main() {{\n\
            \x20   let v: Vec[f32] = {buf};\n\
            \x20   let a = gpu.variance(v);\n\
            \x20   match a {{\n\
            \x20       Some(x) => println(f\"{{x}}\"),\n\
            \x20       None => println(\"empty\"),\n\
            \x20   }}\n\
            }}"
        ));
        assert_eq!(out.trim(), want, "variance of {buf}");
    }
}

#[test]
fn gpu_integer_arg_orders_by_the_element_type() {
    // Above 2^31 the signed and unsigned orders disagree at BOTH ends:
    // `4294967295` is the largest u32 and `-1` read as i32. So a signed
    // compare on unsigned data answers argmin and argmax backwards — the
    // discriminating case for wiring signedness through the pair tree.
    let out = run_no_errors(
        "fn main() {\n\
        \x20   let v: Vec[u32] = [4294967295, 1];\n\
        \x20   let m = gpu.argmax(v);\n\
        \x20   match m {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        }",
    );
    assert_eq!(out.trim(), "0", "u32::MAX is the LARGEST u32, not -1");

    let out = run_no_errors(
        "fn main() {\n\
        \x20   let v: Vec[u32] = [4294967295, 1];\n\
        \x20   let m = gpu.argmin(v);\n\
        \x20   match m {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        }",
    );
    assert_eq!(out.trim(), "1");

    // Signed negatives order below zero, and ties still take the first.
    let out = run_no_errors(
        "fn main() {\n\
        \x20   let v: Vec[i32] = [5, -7, -7];\n\
        \x20   let m = gpu.argmin(v);\n\
        \x20   match m {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        }",
    );
    assert_eq!(out.trim(), "1");
}

#[test]
fn gpu_integer_mean_traps_when_the_sum_does_not_fit() {
    // The price of computing exactly: the SUM has to fit, even where the mean
    // would. `Stats.mean` promotes first and sails through this. The
    // alternative on a GPU is an f32 promotion, which loses whole integers
    // above 16777216 — strictly worse than an honest trap.
    let errs = runtime_errors(
        "fn main() {\n\
        \x20   let v: Vec[i32] = [2147483647, 2147483647];\n\
        \x20   let m = gpu.mean(v);\n\
        \x20   match m {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        }",
    );
    assert!(
        format!("{errs:?}").contains("integer overflow"),
        "expected the sum to trap, got: {errs:?}"
    );
}

#[test]
fn gpu_integer_reduction_trapping_follows_the_tree_order() {
    // THE consequence of specifying the order: overflow is a property of the
    // INTERMEDIATE sums, and a tree forms different intermediates than a line.
    // This buffer overflows under a left fold (MAX + MAX first) and survives
    // under the specified tree, which pairs each MAX with a -MAX before they
    // ever meet each other. Documented in design.md — a user swapping
    // `v.sum()` for `gpu.sum(v)` on integer data is not making a pure
    // speedup swap.
    let out = run_no_errors(
        "fn main() {\n\
        \x20   let v: Vec[i32] = [2147483647, 2147483647, -2147483647, -2147483647];\n\
        \x20   println(f\"{gpu.sum(v)}\")\n\
        }",
    );
    assert_eq!(out.trim(), "0");
}

#[test]
fn gpu_integer_sum_min_max_agree_with_the_obvious_answers() {
    let out = run_no_errors(
        "fn main() {\n\
        \x20   let v: Vec[i32] = [3, 1, 2];\n\
        \x20   println(f\"{gpu.sum(v)}\")\n\
        }",
    );
    assert_eq!(out.trim(), "6");

    // `min`/`max` are `Option[i32]` — fallibility is a property of the OP, not
    // of the element type: an empty buffer has no minimum whatever it holds.
    let out = run_no_errors(
        "fn main() {\n\
        \x20   let v: Vec[i32] = [-5, 40, 2];\n\
        \x20   let m = gpu.max(v);\n\
        \x20   match m {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        }",
    );
    assert_eq!(out.trim(), "40");

    let out = run_no_errors(
        "fn main() {\n\
        \x20   let v: Vec[i32] = [];\n\
        \x20   let m = gpu.min(v);\n\
        \x20   match m {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        }",
    );
    assert_eq!(out.trim(), "empty");
}

#[test]
fn gpu_mean_is_the_sum_divided_by_the_count() {
    let out = run_no_errors(
        "fn main() {\n\
        \x20   let v: Vec[f32] = [1.0, 2.0, 3.0];\n\
        \x20   let m = gpu.mean(v);\n\
        \x20   match m {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        }",
    );
    assert_eq!(out.trim(), "2");

    // The mean of nothing is not a number. `0.0 / 0` would be NaN — the
    // plausible-looking value that propagates silently — so the answer is
    // `None`, the same refusal min/max give and `Stats.mean` gives by trapping.
    let out = run_no_errors(
        "fn main() {\n\
        \x20   let v: Vec[f32] = [];\n\
        \x20   let m = gpu.mean(v);\n\
        \x20   match m {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        }",
    );
    assert_eq!(out.trim(), "empty");

    // The guarantee, at a length that needs two full fold levels: mean is the
    // SPECIFIED tree sum divided once, so it inherits the sum's grouping
    // rather than having one of its own.
    let out = run_no_errors(
        "fn main() {\n\
        \x20   let mut v: Vec[f32] = [];\n\
        \x20   let mut p: Vec[f32] = [];\n\
        \x20   for i in 0..4096 { v.push(0.1) p.push(0.1) }\n\
        \x20   let m = gpu.mean(v);\n\
        \x20   match m {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        \x20   println(f\"{gpu.sum(p) / 4096.0}\")\n\
        }",
    );
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines, ["0.10000000149011612", "0.10000000149011612"]);
}

#[test]
fn gpu_dot_is_the_sum_of_the_products() {
    let out = run_no_errors(
        "fn main() {\n\
        \x20   let a: Vec[f32] = [1.0, 2.0, 3.0];\n\
        \x20   let b: Vec[f32] = [4.0, 5.0, 6.0];\n\
        \x20   println(f\"{gpu.dot(a, b)}\")\n\
        }",
    );
    assert_eq!(out.trim(), "32");

    // Empty is the additive identity, like an empty sum.
    let out = run_no_errors(
        "fn main() {\n\
        \x20   let a: Vec[f32] = [];\n\
        \x20   let b: Vec[f32] = [];\n\
        \x20   println(f\"{gpu.dot(a, b)}\")\n\
        }",
    );
    assert_eq!(out.trim(), "0");

    // The guarantee, at a length that needs two full fold levels: `dot(a, b)`
    // and `sum(a * b)` are the same number, in the same tree order.
    let out = run_no_errors(
        "fn main() {\n\
        \x20   let mut a: Vec[f32] = [];\n\
        \x20   let mut b: Vec[f32] = [];\n\
        \x20   let mut p: Vec[f32] = [];\n\
        \x20   for i in 0..4096 {\n\
        \x20       let x: f32 = 0.1;\n\
        \x20       let y: f32 = 1.0;\n\
        \x20       a.push(x)\n\
        \x20       b.push(y)\n\
        \x20       p.push(x * y)\n\
        \x20   }\n\
        \x20   println(f\"{gpu.dot(a, b)}\")\n\
        \x20   println(f\"{gpu.sum(p)}\")\n\
        }",
    );
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines, ["409.6000061035156", "409.6000061035156"]);
}

#[test]
fn gpu_dot_refuses_mismatched_lengths() {
    // Truncating to the shorter buffer would silently answer a question
    // nobody asked. The compiled path traps on the same condition with the
    // same two lengths named.
    let errs = runtime_errors(
        "fn main() {\n\
        \x20   let a: Vec[f32] = [1.0, 2.0, 3.0];\n\
        \x20   let b: Vec[f32] = [4.0, 5.0];\n\
        \x20   println(f\"{gpu.dot(a, b)}\")\n\
        }",
    );
    let joined = format!("{errs:?}");
    assert!(
        joined.contains("equal length") && joined.contains("3 vs 2"),
        "expected a refusal naming both lengths, got: {joined}"
    );
}

#[test]
fn gpu_min_folds_past_one_workgroup() {
    // Multi-workgroup min: 200 elements is four chunks, the last one partial,
    // so the +inf padding has to not win. 500 - 199 = 301.
    let out = run_no_errors(
        "fn main() {\n\
        \x20   let mut v: Vec[f32] = [];\n\
        \x20   for i in 0..200 { v.push(500.0 - (i as f32)) }\n\
        \x20   let m = gpu.min(v);\n\
        \x20   match m {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        }",
    );
    assert_eq!(out.trim(), "301");
}

#[test]
fn gpu_sum_folds_past_one_workgroup_instead_of_refusing() {
    // A buffer longer than one workgroup used to be refused; it now folds
    // through the multi-dispatch recursion the runtime performs — chunk into
    // workgroup-wide pieces, reduce each, reduce the partials. 65 elements is
    // the first length that needs a second chunk, and the answer must be 65,
    // not the 64 a truncating implementation would print.
    let out = run_no_errors(
        "fn main() {\n\
        \x20   let mut v: Vec[f32] = [];\n\
        \x20   for i in 0..65 { v.push(1.0) }\n\
        \x20   println(f\"{gpu.sum(v)}\")\n\
        }",
    );
    assert_eq!(out.trim(), "65");

    // Two full levels: 4096 elements is 64 workgroups, then one over the
    // partials.
    let out = run_no_errors(
        "fn main() {\n\
        \x20   let mut v: Vec[f32] = [];\n\
        \x20   for i in 0..4096 { v.push(1.0) }\n\
        \x20   println(f\"{gpu.sum(v)}\")\n\
        }",
    );
    assert_eq!(out.trim(), "4096");
}

// ── B-2026-08-21-10: `to_ne_bytes()` on the integer scalars ─────
//
// The bytes are the value's NATIVE-order memory image. The interpreter's
// i64-backed model sign-extends a narrow `iN`, so taking the low N bytes of
// that image is exactly the reinterpretation codegen's store-and-reload
// performs — which is what keeps the two backends' answers identical.

#[test]
fn to_ne_bytes_gives_the_native_order_image() {
    let out = run("fn main() {\n\
        let n: u16 = 4660u16;\n\
        let b = n.to_ne_bytes();\n\
        let w: u32 = 16909060u32;\n\
        let c = w.to_ne_bytes();\n\
        println(f\"{b.len()} {b[0]} {b[1]} | {c.len()} {c[0]} {c[3]}\");\n\
    }");
    // 0x1234 and 0x01020304 on a little-endian host.
    assert_eq!(out, "2 52 18 | 4 4 1\n");
}

#[test]
fn to_ne_bytes_covers_every_integer_width() {
    // Each result is BOUND before `len()` — a method call on an Array
    // TEMPORARY has no codegen lowering, so chaining here would pin a shape
    // only the interpreter runs.
    let out = run("fn main() {\n\
        let a: u8 = 7u8;\n\
        let b: u16 = 7u16;\n\
        let c: u32 = 7u32;\n\
        let d: u64 = 7u64;\n\
        let e: i16 = 7i16;\n\
        let f: i64 = 7i64;\n\
        let ab = a.to_ne_bytes();\n\
        let bb = b.to_ne_bytes();\n\
        let cb = c.to_ne_bytes();\n\
        let db = d.to_ne_bytes();\n\
        let eb = e.to_ne_bytes();\n\
        let fb = f.to_ne_bytes();\n\
        println(f\"{ab.len()} {bb.len()} {cb.len()} {db.len()} {eb.len()} {fb.len()}\");\n\
    }");
    assert_eq!(out, "1 2 4 8 2 8\n");
}

#[test]
fn to_ne_bytes_of_a_negative_value_is_its_twos_complement_image() {
    // The bytes are the value's memory image, not its magnitude — the model's
    // sign extension must be masked to the receiver's width, or an `i16` -2
    // would report the i64 image's 0xFF bytes at every index.
    let out = run("fn main() {\n\
        let n: i16 = -2i16;\n\
        let b = n.to_ne_bytes();\n\
        println(f\"{b.len()} {b[0]} {b[1]}\");\n\
    }");
    assert_eq!(out, "2 254 255\n");
}

/// The guard both backends apply: a local binding must still shadow a type
/// name, so `fs[0](10)` stays an index-then-call and never reads as type
/// application. This is also why design.md's `[T]` cannot be applied at a call
/// site — the two forms are genuinely ambiguous, and only the receiver
/// position (where a type cannot be indexed) disambiguates by construction.
#[test]
fn indexing_a_value_binding_is_not_type_application() {
    assert_eq!(
        run_no_errors(
            "fn add1(x: i64) -> i64 { return x + 1i64; }\n\
             fn add2(x: i64) -> i64 { return x + 2i64; }\n\
             fn main() {\n\
                 let fs: Vec[Fn(i64) -> i64] = [add1, add2];\n\
                 println(f\"{fs[0](10i64)}\");\n\
                 println(f\"{fs[1](10i64)}\");\n\
             }"
        ),
        "11\n12\n"
    );
}

#[test]
fn two_user_hashers_order_the_same_keys_differently_in_the_interpreter() {
    // The discriminating assertion. A hasher that never ran — or one that
    // degraded to a constant digest — leaves every key in one bucket, and the
    // observable walk falls back to insertion order for BOTH maps, making the
    // two equal. Ten keys make a coincidental agreement 1 in 10!.
    let out = run_no_errors(&format!(
        "{USER_HASHERS}\
         fn main() {{\n\
             let mut a: Map[String, i64, FnvBuild] = Map.new();\n\
             let mut b: Map[String, i64, SumBuild] = Map.new();\n\
             let names = [\"zulu\", \"alpha\", \"mike\", \"bravo\", \"yankee\",\n\
                          \"charlie\", \"xray\", \"delta\", \"whiskey\", \"echo\"];\n\
             for n in names {{\n\
                 a.insert(n.to_string(), 1);\n\
                 b.insert(n.to_string(), 1);\n\
             }}\n\
             let mut x = \"\";\n\
             for k in a.keys() {{ x = x + k + \" \"; }}\n\
             println(x);\n\
             let mut y = \"\";\n\
             for k in b.keys() {{ y = y + k + \" \"; }}\n\
             println(y);\n\
         }}"
    ));
    let mut lines = out.lines();
    let fnv = lines.next().unwrap_or_default();
    let sum = lines.next().unwrap_or_default();
    assert_eq!(
        fnv.split_whitespace().count(),
        10,
        "the FnvBuild map lost keys: {out}"
    );
    assert_ne!(
        fnv, sum,
        "both maps walked in the same order — either the user hasher never ran \
         or its digest degraded to a constant"
    );
    let mut fnv_sorted: Vec<&str> = fnv.split_whitespace().collect();
    let mut sum_sorted: Vec<&str> = sum.split_whitespace().collect();
    fnv_sorted.sort_unstable();
    sum_sorted.sort_unstable();
    assert_eq!(
        fnv_sorted, sum_sorted,
        "the two maps hold different keys, so the order comparison proves \
         nothing: {out}"
    );
}

// ── `StableHash.siphash24` — the stable-digest escape hatch (B-2026-08-25-22) ──
//
// design.md § `Hash` and `Hasher`'s stability policy tells anyone who needs a
// digest that outlives the process NOT to use `Hash`, whose key is drawn from
// a random source once per process, and names this namespace as where to go
// instead. Until the row that these tests close, the namespace did not exist,
// so that paragraph left those users with no option at all.
//
// EVERY EXPECTED VALUE BELOW IS AN ORACLE RESULT, not a transcription of what
// karac printed. `std`'s (deprecated) `SipHasher` IS SipHash-2-4, and each
// constant here was computed with it before being written down. That matters
// more than usual for this function: its entire value proposition is that
// another implementation of `siphash24` computes the same number, so a pin
// taken from our own output would confirm nothing except self-consistency.
// (`hash/src/lib.rs`'s `agrees_with_stds_siphash24` runs the same oracle over
// 4 keys x 41 lengths; these are the end-to-end spot checks through the
// language surface.)

#[test]
fn stable_hash_siphash24_matches_the_reference_algorithm() {
    assert_eq!(
        run("fn main() {\n\
                 let s: String = \"kara\";\n\
                 println(StableHash.siphash24(s.bytes(), 0u64, 0u64));\n\
             }\n"),
        "8829141961400634939\n"
    );
}

/// The `Slice[u8]` parameter must accept every byte source the language
/// offers, because a digest API you have to marshal into is one users route
/// around. All three spellings below are the same three bytes and must give
/// the same digest.
#[test]
fn stable_hash_siphash24_accepts_every_byte_source() {
    assert_eq!(
        run("fn main() {\n\
                 let v: Vec[u8] = [1u8, 2u8, 3u8];\n\
                 println(StableHash.siphash24(v, 0u64, 0u64));\n\
                 println(StableHash.siphash24([1u8, 2u8, 3u8], 0u64, 0u64));\n\
                 let sl = v.as_slice();\n\
                 println(StableHash.siphash24(sl, 0u64, 0u64));\n\
             }\n"),
        "7196089002619860989\n7196089002619860989\n7196089002619860989\n"
    );
}

/// The empty input is legal and has a defined digest — it reaches the runtime
/// as `(null, 0)`. Worth its own test because "no bytes" is exactly the case a
/// null-check written defensively turns into a silent zero, and a digest that
/// returns 0 for empty input collides every empty key with a real one.
#[test]
fn stable_hash_siphash24_hashes_the_empty_input() {
    assert_eq!(
        run("fn main() {\n\
                 let v: Vec[u8] = [];\n\
                 println(StableHash.siphash24(v, 0u64, 0u64));\n\
             }\n"),
        "2202906307356721367\n"
    );
}

/// The key is a real 128-bit key, and its halves are not interchangeable.
///
/// A wiring bug that dropped `k1`, or passed the halves in the wrong order,
/// would still produce a plausible avalanche and would still be stable across
/// runs — so every other test here would pass while the digest silently
/// disagreed with every other `siphash24` in the world. Pinning `(1, 2)` and
/// `(2, 1)` to their two distinct oracle values is what catches that.
#[test]
fn stable_hash_siphash24_uses_both_key_halves_in_order() {
    assert_eq!(
        run("fn main() {\n\
                 let s: String = \"kara\";\n\
                 println(StableHash.siphash24(s.bytes(), 1u64, 2u64));\n\
                 println(StableHash.siphash24(s.bytes(), 2u64, 1u64));\n\
                 println(StableHash.siphash24(s.bytes(), 506097522914230528u64, 1084818905618843912u64));\n\
             }\n"),
        "9152030712822536581\n14841467261521516778\n12078748455302492582\n"
    );
}

#[test]
fn container_bodies_walks_fire_at_the_nll_point() {
    // The INTERPRETER half of the oracle for B-2026-08-27-8's codegen twin
    // (`test_e2e_container_bodies_walks_fire_at_the_nll_point`). design.md
    // § Drop: "destructors fire at each binding's live-range end, not lexical
    // scope end … a value whose last use is mid-scope is dropped at that use
    // and does not appear in the end-of-scope stack at all."
    //
    // The pairing is the point. Codegen decides a container walker's placement
    // per registration, and until B-2026-08-27-8 it decided it by testing the
    // emitted LLVM symbol's NAME — so a walker spelled outside the admitted
    // prefixes was demoted to scope exit, silently. Nothing failed: the body
    // still ran exactly once, and every other walker test here asserts COUNTS
    // rather than sequence, deliberately, because container iteration order is
    // unspecified (B-2026-08-27-7). Only a marker printed between the
    // binding's last use and scope end separates the two placements, and only
    // an interpreter twin makes the compiled result checkable against
    // something.
    //
    // Order-safety comes from ONE entry per container: a single element has a
    // single permutation, so pinning the sequence here does not pin an
    // unspecified order. `e` is never used after its `let`, so its body lands
    // before `[enum]` — its live-range end, and still distinct from scope
    // exit, which would put it after `|enum`.
    let src = "#[derive(Hash, Eq)]
        struct K { id: i64 }
        struct V { id: i64 }
        impl Drop for K { fn drop(mut ref self) { println(f\"K{self.id}\"); } }
        impl Drop for V { fn drop(mut ref self) { println(f\"V{self.id}\"); } }
        enum Payload { Wrap(V), Empty }
        fn vec_case() {
            let mut v: Vec[V] = Vec.new();
            v.push(V { id: 1 });
            println(f\"[vec {v.len()}]\");
            println(\"|vec\");
        }
        fn tuple_case() {
            let t: (V, i64) = (V { id: 2 }, 7);
            println(f\"[tup {t.1}]\");
            println(\"|tup\");
        }
        fn enum_case() {
            let e: Payload = Payload.Wrap(V { id: 3 });
            println(\"[enum]\");
            println(\"|enum\");
        }
        fn opt_case() {
            let o: Option[V] = Some(V { id: 4 });
            println(f\"[opt {o.is_some()}]\");
            println(\"|opt\");
        }
        fn map_case() {
            let mut m: Map[K, V] = Map.new();
            m.insert(K { id: 5 }, V { id: 5 });
            println(f\"[map {m.len()}]\");
            println(\"|map\");
        }
        fn set_case() {
            let mut s: Set[K] = Set.new();
            s.insert(K { id: 6 });
            println(f\"[set {s.len()}]\");
            println(\"|set\");
        }
        fn main() {
            vec_case(); tuple_case(); enum_case(); opt_case(); map_case(); set_case();
        }";
    assert_eq!(
        run_no_errors(src),
        "[vec 1]\nV1\n|vec\n\
         [tup 7]\nV2\n|tup\n\
         V3\n[enum]\n|enum\n\
         [opt true]\nV4\n|opt\n\
         [map 1]\nK5\nV5\n|map\n\
         [set 1]\nK6\n|set\n"
    );
}

/// B-2026-08-29-43 — interpreter twin of
/// `codegen::e2e_mixed_owndrop_literal_masks_only_the_view_body`, same 10
/// shapes in the same order. Both backends dropped their bail in ONE commit,
/// so this file is what proves they moved together.
///
/// A MIXED struct literal over a struct with its OWN
/// `impl Drop`: some Drop-bearing fields moved in from a param VIEW, some
/// minted fresh.
///
/// The view's body belongs to the caller under caller-retains, so this
/// binding must not run it; the fresh field's body belongs to nobody else,
/// so it must. Before the fix the whole literal kept its walk and the view
/// doubled — agreed across all three backends, which is why no A/B gate
/// caught it.
///
/// The danger in fixing it is the OPPOSITE failure: the only wrapper
/// surgery available for an own-`Drop` struct used to be all-or-nothing,
/// and applying that here would have silenced the fresh field too. So every
/// row below pins BOTH halves — the view fires once, the fresh field fires
/// once — and the boundary rows (all-views, all-fresh, no-own-`Drop`) pin
/// that the neighbouring paths did not move.
/// B-2026-08-29-44 — interpreter twin of
/// `codegen::e2e_whole_value_rebind_inherits_the_wrap_mask`, same shapes in the
/// same order. Both backends' transfers landed in ONE commit; this file is
/// what proves they moved together, and the pinned enum row is what proves
/// the one shape they deliberately did NOT move stays agreed.
///
/// A WHOLE-VALUE REBIND (`let s2 = s;`) after a MIXED
/// wrap re-armed the walk the wrap's mask had just withheld.
///
/// Every mask in this family is keyed on the BINDING, and a rebind
/// registers the destination afresh — so the param view's body ran a
/// second time. The ALL-VIEWS case has no such hole: it marks the binding
/// a param view and view-ness already propagates (B-2026-08-01-15), which
/// is why only the MIXED spellings lost their mask.
///
/// Registering full and disarming afterwards does NOT work at a `let` —
/// the disarm helpers re-register rather than replace, leaving the
/// destination with two walkers and MORE bodies. The fix inherits the
/// source's masks first and builds the walker already masked.
#[test]
fn test_whole_value_rebind_inherits_the_wrap_mask() {
    let hdr = "struct R { id: i64, name: String }\n\
               impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
               fn mk(i: i64) -> R { return R { id: i, name: f\"heap-{i}\" }; }\n\
               enum W2 { Two(R, R), None2 }\n\
               struct S3 { a: R, b: R }\n\
               struct S1 { r: R }\n";
    for (label, fns, main, want) in [
        (
            "struct literal MIXED, then rebind",
            "fn take(r: R) -> i64 { let s = S3 { a: r, b: mk(2) }; let s2 = s; return 7; }",
            "let v = take(mk(1)); println(f\"v={v}\");",
            "dR2\ndR1\nv=7\n",
        ),
        (
            "tuple MIXED, then rebind",
            "fn take(r: R) -> i64 { let t = (r, 5); let t2 = t; return 7; }",
            "let v = take(mk(1)); println(f\"v={v}\");",
            "dR1\nv=7\n",
        ),
        (
            "rebound twice — the mask survives the chain",
            "fn take(r: R) -> i64 { let s = S3 { a: r, b: mk(2) }; let s2 = s; let s3 = s2; return 7; }",
            "let v = take(mk(1)); println(f\"v={v}\");",
            "dR2\ndR1\nv=7\n",
        ),
        // CONTROLS: the same wraps WITHOUT a rebind were always correct,
        // which is what isolates the rebind as the trigger; and the
        // all-views wrap WITH one was correct through view-ness
        // propagation, the mechanism the mixed case lacks.
        (
            "control: struct MIXED, no rebind",
            "fn take(r: R) -> i64 { let s = S3 { a: r, b: mk(2) }; return 7; }",
            "let v = take(mk(1)); println(f\"v={v}\");",
            "dR2\ndR1\nv=7\n",
        ),
        (
            "control: tuple MIXED, no rebind",
            "fn take(r: R) -> i64 { let t = (r, 5); return 7; }",
            "let v = take(mk(1)); println(f\"v={v}\");",
            "dR1\nv=7\n",
        ),
        (
            "control: ALL-VIEWS struct, then rebind",
            "fn take(r: R) -> i64 { let s = S1 { r: r }; let s2 = s; return 7; }",
            "let v = take(mk(1)); println(f\"v={v}\");",
            "dR1\nv=7\n",
        ),
        (
            "control: enum ctor MIXED, no rebind",
            "fn take(r: R) -> i64 { let w = W2.Two(r, mk(2)); return 7; }",
            "let v = take(mk(1)); println(f\"v={v}\");",
            "dR2\ndR1\nv=7\n",
        ),
    ] {
        let src = format!("{hdr}{fns}\nfn main() {{ {main} }}\n");
        assert_eq!(run(&src), want, "[{label}]");
    }
    // B-2026-08-31-50 — the ENUM-CTOR spelling, pinned at the DEFECT until
    // this row: it doubled on both backends, because codegen derived a
    // constructor's view slots from the ctor EXPRESSION at the `let` and
    // stored nothing per variable, so a rebind had no mask to inherit — and
    // this backend's ready transfer was withheld so the two would land
    // together. Codegen now keeps `enum_ctor_moved_payload_slots` per
    // binding and inherits it under the rebind gate, this backend transfers
    // `moved_out_enum_payload_slots`, and both print the due `dR2 dR1`.
    let enum_rebind = format!(
        "{hdr}fn take(r: R) -> i64 {{ let w = W2.Two(r, mk(2)); let w2 = w; return 7; }}\n\
         fn main() {{ let v = take(mk(1)); println(f\"v={{v}}\"); }}\n"
    );
    assert_eq!(
        run(&enum_rebind),
        "dR2\ndR1\nv=7\n",
        "[enum ctor MIXED then rebind — the mask is inherited (B-2026-08-31-50)]"
    );
}

/// B-2026-09-06-19 — see the codegen twin. Both legs are shared-predicate or
/// two-backend fixes: the wrap alias lives in the AST scanners, and the
/// return-site projection mask (`record_returned_projection_moves`) is this
/// backend's half of the second leg.
///
/// Twin of `tests/codegen.rs`'s `e2e_param_wrapped_through_local_hands_back_once`, pinned to the same string.
#[test]
fn test_param_wrapped_through_local_hands_back_once() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"  d{self.id}") } }
fn mr(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
struct P2 { r: R, n: i64 }
struct Box2 { r: R }
struct W { p: P2 }
struct K { n: i64 }

fn ftwo(r: R, k: bool) -> Box2 { if k { return Box2 { r: mr(82) }; } let p = P2 { r: r, n: 1 }; return Box2 { r: p.r }; }
fn fone(r: R, k: bool) -> Box2 { if k { return Box2 { r: mr(83) }; } let p = Box2 { r: r }; return p; }
fn fproj(r: R, k: bool) -> R { if k { return mr(84); } let p = P2 { r: r, n: 1 }; return p.r; }
fn flet(r: R, k: bool) -> Box2 { if k { return Box2 { r: mr(85) }; } let p = P2 { r: r, n: 1 }; let q = p.r; return Box2 { r: q }; }
fn fnest(r: R, k: bool) -> W { if k { return W { p: P2 { r: mr(86), n: 0 } }; } let p = P2 { r: r, n: 1 }; let w = W { p: p }; return w; }
fn ftup(r: R, k: bool) -> Box2 { if k { return Box2 { r: mr(87) }; } let t = (r, 1); return Box2 { r: t.0 }; }
fn frebind(r: R, k: bool) -> P2 { if k { return P2 { r: mr(88), n: 0 }; } let p = P2 { r: r, n: 1 }; let q = p; return q; }
fn fearly(r: R, k: bool) -> Box2 { let p = P2 { r: r, n: 1 }; if k { return Box2 { r: mr(89) }; } return Box2 { r: p.r }; }
fn funcond(r: R) -> Box2 { let p = P2 { r: r, n: 1 }; return Box2 { r: p.r }; }
fn fpart(r: R, k: bool) -> i64 { let p = P2 { r: r, n: 1 }; if k { return 0; } return p.r.id; }
fn lproj() -> Box2 { let p = P2 { r: mr(19), n: 1 }; return Box2 { r: p.r }; }
fn lbare() -> R { let p = P2 { r: mr(20), n: 1 }; return p.r; }
fn ltail() -> Box2 { let p = P2 { r: mr(21), n: 1 }; Box2 { r: p.r } }
fn lopt() -> Option[R] { let p = P2 { r: mr(22), n: 1 }; return Option.Some(p.r); }
fn lnest() -> R { let w = W { p: P2 { r: mr(23), n: 1 } }; return w.p.r; }
impl K {
    fn mlocal(self) -> Box2 { let p = P2 { r: mr(24), n: self.n }; return Box2 { r: p.r }; }
    fn m(self, r: R, k: bool) -> Box2 { if k { return Box2 { r: mr(90) }; } let p = P2 { r: r, n: self.n }; return Box2 { r: p.r }; }
    fn mref(ref self, r: R, k: bool) -> Box2 { if k { return Box2 { r: mr(91) }; } let p = P2 { r: r, n: self.n }; return Box2 { r: p.r }; }
    fn muncond(self, r: R) -> Box2 { let p = P2 { r: r, n: self.n }; return Box2 { r: p.r }; }
}

fn main() {
    println("ftwo/t"); let a1 = ftwo(mr(1), true); println(f"  C{a1.r.id}");
    println("ftwo/f"); let a2 = ftwo(mr(2), false); println(f"  C{a2.r.id}");
    println("fone/f"); let a3 = fone(mr(3), false); println(f"  C{a3.r.id}");
    println("fproj/f"); let a4 = fproj(mr(4), false); println(f"  C{a4.id}");
    println("flet/f"); let a5 = flet(mr(5), false); println(f"  C{a5.r.id}");
    println("fnest/f"); let a6 = fnest(mr(6), false); println(f"  C{a6.p.r.id}");
    println("ftup/f"); let a7 = ftup(mr(7), false); println(f"  C{a7.r.id}");
    println("frebind/f"); let a8 = frebind(mr(8), false); println(f"  C{a8.r.id}");
    println("fearly/t"); let a9 = fearly(mr(9), true); println(f"  C{a9.r.id}");
    println("fearly/f"); let a10 = fearly(mr(10), false); println(f"  C{a10.r.id}");
    println("funcond"); let a11 = funcond(mr(11)); println(f"  C{a11.r.id}");
    println("funcond/named"); let x12 = mr(12); let a12 = funcond(x12); println(f"  C{a12.r.id}");
    println("fpart/t"); let a13 = fpart(mr(13), true); println(f"  C{a13}");
    println("fpart/f"); let a14 = fpart(mr(14), false); println(f"  C{a14}");
    println("m/t"); let a15 = K { n: 1 }.m(mr(15), true); println(f"  C{a15.r.id}");
    println("m/f"); let a16 = K { n: 1 }.m(mr(16), false); println(f"  C{a16.r.id}");
    println("mref/f"); let kk = K { n: 2 }; let a17 = kk.mref(mr(17), false); println(f"  C{a17.r.id}");
    println("muncond"); let a18 = K { n: 3 }.muncond(mr(18)); println(f"  C{a18.r.id}");
    println("lproj"); let a19 = lproj(); println(f"  C{a19.r.id}");
    println("lbare"); let a20 = lbare(); println(f"  C{a20.id}");
    println("ltail"); let a21 = ltail(); println(f"  C{a21.r.id}");
    println("lopt"); if let Some(a22) = lopt() { println(f"  C{a22.id}"); }
    println("lnest"); let a23 = lnest(); println(f"  C{a23.id}");
    println("mlocal"); let a24 = K { n: 4 }.mlocal(); println(f"  C{a24.r.id}");
    println("end");
}
"#),
        r#"ftwo/t
  d1
  C82
  d82
ftwo/f
  C2
  d2
fone/f
  C3
  d3
fproj/f
  C4
  d4
flet/f
  C5
  d5
fnest/f
  C6
  d6
ftup/f
  C7
  d7
frebind/f
  C8
  d8
fearly/t
  C89
  d89
fearly/f
  C10
  d10
funcond
  C11
  d11
funcond/named
  C12
  d12
fpart/t
  d13
  C0
fpart/f
  d14
  C14
m/t
  d15
  C90
  d90
m/f
  C16
  d16
mref/f
  C17
  d17
muncond
  C18
  d18
lproj
  C19
  d19
lbare
  C20
  d20
ltail
  C21
  d21
lopt
  C22
  d22
lnest
  C23
  d23
mlocal
  C24
  d24
end
"#
    );
}

/// B-2026-09-07-4, interpreter half — `record_method_arg_moves` did not count a
/// ONE-HOP hand-back as an escape, so a named local passed to
/// `fn thruv(ref self, r: R) -> R { return fwd(r); }` ran its `Drop` body twice
/// while every compiled surface ran it once. A fix pin here.
///
/// Twin of `tests/codegen.rs`'s `e2e_argument_handed_back_through_a_hop_by_a_method`, pinned to the same string.
#[test]
fn test_argument_handed_back_through_a_hop_by_a_method() {
    assert_eq!(
        run(r#"shared struct Inner { v: i64 }
struct R { id: i64, name: String, inner: Inner }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
struct P { id: i64, name: String, xs: Vec[i64] }
impl Drop for P { fn drop(mut ref self) { println(f"  dP{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}", inner: Inner { v: i } }; }
fn mkp(i: i64) -> P { return P { id: i, name: f"p{i}", xs: [i] }; }
fn fwd(r: R) -> R { return r; }
fn pfwd(p: P) -> P { return p; }
fn dies(r: R) -> i64 { return r.id; }
struct Hold { n: i64 }
impl R { fn passb(r: R) -> R { return fwd(r); } }
impl R { fn passa(r: R) -> R { return r; } }
impl P { fn ppassb(p: P) -> P { return pfwd(p); } }
impl Hold { fn thruv(ref self, r: R) -> R { return fwd(r); } }
impl Hold { fn eats(ref self, r: R) -> i64 { return dies(r); } }
fn main() {
  let h = Hold { n: 0 };
  println("assoc_hop"); let z = R.passb(mk(1)); println(f"  v={z.inner.v}");
  println("assoc_direct"); let y = R.passa(mk(2)); println(f"  v={y.inner.v}");
  println("method_hop"); let w = h.thruv(mk(3)); println(f"  v={w.inner.v}");
  println("assoc_hop_copyable"); let c = P.ppassb(mkp(4)); println(f"  v={c.id}");
  println("method_hop_dies"); println(f"  v={h.eats(mk(5))}");
  println("named_into_assoc_hop"); let a = mk(6); let b = R.passb(a); println(f"  v={b.inner.v}");
  println("end");
}
"#),
        r#"assoc_hop
  v=1
  dR1
assoc_direct
  v=2
  dR2
method_hop
  v=3
  dR3
assoc_hop_copyable
  v=4
  dP4
method_hop_dies
  dR5
  v=5
named_into_assoc_hop
  v=6
  dR6
end
"#
    );
}

/// B-2026-09-07-12 — the interpreter half of the pair: it ran the argument's
/// `Drop` body twice for a named local passed to a STORING method, in both copy
/// classes, while the free-function twin was correct. A fix pin here, not a
/// parity-only twin.
///
/// Twin of `tests/codegen.rs`'s `e2e_named_local_into_a_storing_method`, pinned to the same string.
#[test]
fn test_named_local_into_a_storing_method() {
    assert_eq!(
        run(r#"shared struct Inner { v: i64 }
struct R { id: i64, name: String, inner: Inner }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
struct S { id: i64, name: String }
impl Drop for S { fn drop(mut ref self) { println(f"  dS{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}", inner: Inner { v: i } }; }
fn mks(i: i64) -> S { return S { id: i, name: f"s{i}" }; }
struct Box2 { mut xs: Vec[R] }
impl Box2 { fn push(mut ref self, r: R) { self.xs.push(r); } }
struct BoxS { mut ys: Vec[S] }
impl BoxS { fn add(mut ref self, s: S) { self.ys.push(s); } }
fn take(b: mut ref Box2, r: R) { b.xs.push(r); }
fn main() {
  println("named_method");
  let mut b = Box2 { xs: Vec.new() };
  let a = mk(1); b.push(a); println(f"  len={b.xs.len()}");
  println("fresh_method");
  let mut c = Box2 { xs: Vec.new() };
  c.push(mk(2)); println(f"  len={c.xs.len()}");
  println("named_free_fn");
  let mut d = Box2 { xs: Vec.new() };
  let e = mk(3); take(mut d, e); println(f"  len={d.xs.len()}");
  println("copy_supported_method");
  let mut g = BoxS { ys: Vec.new() };
  let h = mks(4); g.add(h); println(f"  len={g.ys.len()}");
  println("two_named");
  let mut i = Box2 { xs: Vec.new() };
  let j = mk(5); i.push(j); let k = mk(6); i.push(k); println(f"  len={i.xs.len()}");
  println("end");
}
"#),
        r#"named_method
  len=1
  dR1
fresh_method
  len=1
  dR2
named_free_fn
  len=1
  dR3
copy_supported_method
  len=1
  dS4
two_named
  len=2
  dR5
  dR6
end
"#
    );
}

/// B-2026-09-07-10 — the interpreter's half of that row: it ran the argument's
/// `Drop` body twice for the one-hop hand-back, so this twin is a fix pin here
/// rather than a parity-only one.
///
/// Twin of `tests/codegen.rs`'s `e2e_named_local_through_a_forwarding_hop`, pinned to the same string.
#[test]
fn test_named_local_through_a_forwarding_hop() {
    assert_eq!(
        run(r#"shared struct Inner { v: i64 }
struct R { id: i64, name: String, inner: Inner }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
struct P { id: i64, name: String, xs: Vec[i64] }
impl Drop for P { fn drop(mut ref self) { println(f"  dP{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}", inner: Inner { v: i } }; }
fn mkp(i: i64) -> P { return P { id: i, name: f"p{i}", xs: [i] }; }
fn f(r: R) -> R { return r; }
fn ppass(p: P) -> P { return p; }
fn dies(r: R) -> i64 { return r.id; }
fn via(r: R) -> R { return f(r); }
fn via2(r: R) -> R { let m = r; return f(m); }
fn pvia(p: P) -> P { return ppass(p); }
fn dvia(r: R) -> i64 { return dies(r); }
fn mvia(r: R, c: bool) -> R { if c { return f(r); } return mk(99); }
fn main() {
  println("named_hop"); let a = mk(1); let z = via(a); println(f"  v={z.inner.v}");
  println("fresh_hop"); let w = via(mk(2)); println(f"  v={w.inner.v}");
  println("rebind_hop"); let b = mk(3); let y = via2(b); println(f"  v={y.inner.v}");
  println("copyable_hop"); let c = mkp(4); let d = pvia(c); println(f"  v={d.id}");
  println("dies_in_hop"); let e = mk(5); println(f"  v={dvia(e)}");
  println("mixed_dies_inside"); let g = mk(6); let h = mvia(g, false); println(f"  v={h.id}");
  println("end");
}
"#),
        r#"named_hop
  v=1
  dR1
fresh_hop
  v=2
  dR2
rebind_hop
  v=3
  dR3
copyable_hop
  v=4
  dP4
dies_in_hop
  v=5
  dR5
mixed_dies_inside
  dR6
  v=99
  dR99
end
"#
    );
}

/// B-2026-09-06-71 — the interpreter was correct on every cell of this row; the
/// twin holds the compiled string to it.
///
/// Twin of `tests/codegen.rs`'s `e2e_named_local_argument_to_a_passthrough_callee`, pinned to the same string.
#[test]
fn test_named_local_argument_to_a_passthrough_callee() {
    assert_eq!(
        run(r#"shared struct Inner { v: i64 }
struct R { id: i64, name: String, inner: Inner }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
struct P { id: i64, name: String, xs: Vec[i64] }
impl Drop for P { fn drop(mut ref self) { println(f"  dP{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}", inner: Inner { v: i } }; }
fn mkp(i: i64) -> P { return P { id: i, name: f"p{i}", xs: [i] }; }
fn pass(r: R) -> R { return r; }
fn rebpass(r: R) -> R { let m = r; return m; }
fn ppass(p: P) -> P { return p; }
fn dies(r: R) -> i64 { return r.id; }
fn main() {
  println("named"); let a = mk(1); let z = pass(a); println(f"  v={z.inner.v}");
  println("named_rebind"); let b = mk(2); let y = rebpass(b); println(f"  v={y.inner.v}");
  println("fresh_temp"); let w = pass(mk(3)); println(f"  v={w.inner.v}");
  println("copyable"); let c = mkp(4); let d = ppass(c); println(f"  v={d.id}");
  println("dies_inside"); let e = mk(5); println(f"  v={dies(e)}");
  println("two_in_a_row"); let g = mk(6); let h = pass(g); let n = mk(7); let q = rebpass(n); println(f"  v={h.inner.v}{q.inner.v}");
  println("end");
}
"#),
        r#"named
  v=1
  dR1
named_rebind
  v=2
  dR2
fresh_temp
  v=3
  dR3
copyable
  v=4
  dP4
dies_inside
  v=5
  dR5
two_in_a_row
  v=67
  dR7
  dR6
end
"#
    );
}

#[test]
fn interp_partial_cmp_value_receiver_resolves_on_a_concrete_type() {
    // B-2026-08-31-13. `x.partial_cmp(y)` was rejected at typecheck on every
    // concrete receiver with "expects 2 argument(s), found 1", while the
    // identically-registered `x.cmp(y)` was accepted.
    //
    // `register_builtin_impl`'s comparison signatures carry the RECEIVER in
    // `params` (`param_names: [self, other]`, `params: [ty, ty]`); a user impl
    // does not, because its receiver rides `self_param`. The value-receiver
    // dispatch counted `params.len()` and so expected 2 where 1 was given.
    // `cmp` escaped only because a name-keyed exemption routed it away from
    // that dispatch entirely. Dropping a leading `self` param is structural, so
    // it covers every baked method with one rather than a name at a time.
    //
    // `nan` is the case that makes this worth having: `PartialOrd` is a
    // separate trait from `Ord` precisely so an incomparable pair can answer
    // `None`, and a bare float is the population that needs it — `f.cmp(g)` is
    // and stays rejected, because floats have no baked `Ord`.
    //
    // The compiled twin is `e2e_partial_cmp_value_receiver_runs_on_a_concrete_-
    // type` in tests/codegen.rs; the row is a typecheck REJECTION, so both
    // backends agreed before the fix and the pair pins that they now agree on
    // running it.
    let m = "match X { Some(Less) => println(\"L\"), Some(Equal) => println(\"E\"),\n\
             Some(Greater) => println(\"G\"), None => println(\"N\") }";
    let cases: Vec<(&str, String, &str)> = vec![
        (
            "i64",
            format!(
                "let n: i64 = 3; let o: i64 = 4; {}",
                m.replace('X', "n.partial_cmp(o)")
            ),
            "L\n",
        ),
        (
            "u64",
            format!(
                "let n: u64 = 18446744073709551615u64; let o: u64 = 2u64; {}",
                m.replace('X', "n.partial_cmp(o)")
            ),
            "G\n",
        ),
        (
            "f64",
            format!(
                "let x: f64 = 1.5; let y: f64 = 2.5; {}",
                m.replace('X', "x.partial_cmp(y)")
            ),
            "L\n",
        ),
        (
            "f64-nan",
            format!(
                "let x: f64 = 0.0 / 0.0; let y: f64 = 2.5; {}",
                m.replace('X', "x.partial_cmp(y)")
            ),
            "N\n",
        ),
        (
            "String",
            format!(
                "let a: String = \"x\"; let b: String = \"y\"; {}",
                m.replace('X', "a.partial_cmp(b)")
            ),
            "L\n",
        ),
        (
            "char",
            format!(
                "let a: char = 'x'; let b: char = 'y'; {}",
                m.replace('X', "a.partial_cmp(b)")
            ),
            "L\n",
        ),
        (
            "bool",
            format!(
                "let a: bool = false; let b: bool = true; {}",
                m.replace('X', "a.partial_cmp(b)")
            ),
            "L\n",
        ),
        (
            "parenthesized-receiver",
            format!(
                "let n: i64 = 3; {}",
                m.replace('X', "(n).partial_cmp(n + 1)")
            ),
            "L\n",
        ),
        // Controls: the sibling that always worked, and the two paths the row
        // notes are unaffected because a type-param receiver resolves by a
        // different route.
        (
            "control-cmp",
            "let n: i64 = 3; let o: i64 = 4;\n\
             match n.cmp(o) { Less => println(\"L\"), Equal => println(\"E\"),\n\
             Greater => println(\"G\") }"
                .into(),
            "L\n",
        ),
        (
            "control-operator",
            "let n: i64 = 3; let o: i64 = 4; println(n < o);".into(),
            "true\n",
        ),
    ];
    for (label, body, want) in cases {
        let src = format!("fn main() {{\n    {body}\n}}");
        assert_eq!(run(&src), want, "{label}");
    }
    // The `T: PartialOrd` bound path, which the row records as unaffected —
    // lowering emits the same value-receiver shape there and it always worked,
    // because a type-param receiver resolves through `dispatch_trait_assoc_fn`
    // rather than the concrete impl table.
    assert_eq!(
        run("fn g[T: PartialOrd](a: T, b: T) -> bool { a < b }\n\
             fn main() { let n: i64 = 3; let o: i64 = 4; println(g(n, o)); }"),
        "true\n",
        "control-bound"
    );
    // The float carve-out — `f64` has no baked `Ord`, so `x.cmp(y)` must stay
    // REJECTED (B-2026-08-11-9) — lives in `tests/typechecker.rs` as
    // `partial_cmp_value_receiver_typechecks_while_float_cmp_stays_rejected`,
    // where the diagnostic helpers are.
}

/// B-2026-09-01-20 — the same call-site default fill, reached through the
/// INSTANCE-METHOD spelling `h.g(..)`.
///
/// Deliberately the same four shapes and the SAME EXPECTED VALUES as its free-
/// function and associated-function siblings: omit every default, omit the tail,
/// skip one by label, and mix a positional override with two labels. A receiver
/// whose `id` is 0 keeps the arithmetic identical, so the three fixtures read as
/// one oracle — a fill that produced anything else for the same signature would
/// mean the spellings disagree, which is the whole complaint the row makes.
///
/// This is the half the pre-resolve pass structurally cannot do. The typechecker
/// plans the fill once the receiver has a type and `lowering` splices it into the
/// AST, which is what keeps this fixture and its `tests/codegen.rs` twin in
/// agreement rather than requiring the two backends to implement the rule twice.
#[test]
fn test_default_parameter_fill_through_an_instance_method_oracle() {
    assert_eq!(
        run(r#"
struct Server { id: i64 }

impl Server {
    fn create(ref self, host: i64, port: i64 = 8080, max_connections: i64 = 1000, timeout_ms: i64 = 5000) -> i64 {
        self.id + host + port + max_connections + timeout_ms
    }
}

fn main() {
    let s = Server { id: 0 };
    println(s.create(1));
    println(s.create(1, 9090));
    println(s.create(1, max_connections: 100));
    println(s.create(1, 9090, max_connections: 100, timeout_ms: 250));
}
"#),
        "14081\n15091\n13181\n9441\n"
    );
}

/// B-2026-08-30-55, leg 2 — a method frame RETRACTS to its caller, but only
/// where the caller is actually firing.
///
/// The retraction guards were a blanket "never inside a method frame", which
/// left a named argument's payload body running twice: the caller's binding
/// fired it and the frame's own slot fired it again.
///
/// Lifting them outright is what the row proposed and it is wrong in the other
/// direction — measured, it loses the body entirely for a fresh temp, exactly
/// as `record_assign_of_param_view`'s own doc warned. The licence is that some
/// OTHER frame owns the value, so the guards ask
/// `method_frame_caller_retains_args`.
///
/// Both spellings are here because they have to agree — the property
/// B-2026-08-29-58 restored — and one predicate now serves both, which is what
/// stops them drifting apart again.
#[test]
fn method_frame_retracts_to_the_caller_only_when_the_caller_fires() {
    const H: &str = "struct R { id: i64, tag: String }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
         enum E { A(R), B }\n\
         impl Drop for E { fn drop(mut ref self) { println(\"dE\") } }\n\
         struct T { n: i64 }\n";
    for (label, body, want) in [
        (
            "assign-spelling",
            "impl T { fn take(ref self, b: E) -> i64 {\n\
             let mut out: R = R { id: 0, tag: f\"t0\" };\n\
             match b { E.A(r) => { out = r; } E.B => { } }\n\
             println(\"m\"); return out.id; } }\n\
             fn main() { let t: T = T { n: 1 }; let c: E = E.A(R { id: 8, tag: f\"t8\" });\n\
             let v: i64 = t.take(c); println(f\"v{v}\") }\n",
            "dR0\nm\ndE\ndR8\nv8\n",
        ),
        (
            "let-spelling",
            "impl T { fn take(ref self, b: E) -> i64 {\n\
             match b { E.A(r) => { let inner: R = r; println(\"m\"); return inner.id; }\n\
             E.B => { } }\n\
             return 0; } }\n\
             fn main() { let t: T = T { n: 1 }; let c: E = E.A(R { id: 8, tag: f\"t8\" });\n\
             let v: i64 = t.take(c); println(f\"v{v}\") }\n",
            "m\ndE\ndR8\nv8\n",
        ),
    ] {
        assert_eq!(run(&format!("{H}{body}")), want, "{label}");
    }
}

#[test]
fn test_ordinary_lowercase_receivers_are_unaffected() {
    // NEGATIVE CONTROL: only names in `PRELUDE_PRIMITIVES` root a path, so an
    // ordinary value receiver still reaches the postfix loop as a method call.
    let out = run("struct V { f: i64 }\n\
                   fn main() {\n\
                       let v = V { f: 9 };\n\
                       println(v.f.to_string());\n\
                       let s = \"ab\";\n\
                       println(s.len().to_string());\n\
                   }");
    assert_eq!(out.trim(), "9\n2");
}

/// The `VecDeque[...]` half. `VecDeque` shares `Vec`'s `{ptr,len,cap}`
/// representation, so the risk is not ordering but that the literal builds
/// something the deque-only operations cannot use. B-2026-09-15-25.
#[test]
fn vecdeque_prefix_literal_builds_a_usable_deque() {
    assert_eq!(
        run("fn main() { let d = VecDeque[1, 2, 3]; print(d.len()); }"),
        "3"
    );
    // Front operations are what distinguish it from a `Vec`.
    assert_eq!(
        run("fn main() { let mut d = VecDeque[1, 2, 3]; \
             d.push_front(0); \
             for x in d { print(x); } }"),
        "0123"
    );
    assert_eq!(
        run("fn main() { let mut d = VecDeque[1, 2, 3]; \
             let f = d.pop_front(); \
             print(f.unwrap()); print(d.len()); }"),
        "12"
    );
    // Empty annotated form.
    assert_eq!(
        run("fn main() { let d: VecDeque[i64] = VecDeque[]; print(d.len()); }"),
        "0"
    );
}

/// B-2026-09-15-24 — a METHOD ARGUMENT that is a PROJECTION out of a caller
/// binding (`k.eat(w.r)`) ran the projected value's user `Drop` body TWICE
/// under `--interp` against once on the JIT and the AOT binary.
///
/// The caller still owns `w`, and `w`'s death fires every Drop-bearing field
/// through `drop_user_drop_fields_of_binding` — so the caller was already
/// firing `w.r`'s body. The three method-frame ownership predicates asked
/// "does the caller still own this argument?" as
/// `matches!(.., ExprKind::Identifier(_))`, which answers for a whole binding
/// and says NO for a projection out of one, so the frame claimed the parameter
/// as well and both fired. The FREE-FUNCTION spelling of the same program was
/// always correct, and that is what places the defect on the method path:
/// `eval_call` claims only conditionally-returned parameters
/// (`cond_returned_param_drop_names`) and never consults an argument's shape.
///
/// Fixed by `arg_place_reaches_caller_drop_fire`, which admits a chain of
/// field / tuple-index projections rooted at an identifier or `self`, at all
/// three sites — `method_param_drop_names` (the parameter's own slot),
/// `method_frame_caller_retains_args` (the let-rebind and destructure slots
/// inside the body), and `method_frame_sole_owned_params`. The first alone
/// fixes the plain cell and leaves the rebind and destructure cells doubled,
/// which is why all three moved together.
///
/// The escape guards are untouched, and the last two rows are what pins that:
/// a callee that HANDS THE PROJECTION BACK and one that STORES it each run two
/// bodies on every surface, before the fix and after — those parameters exit
/// through `fn_always_returns_param` / `fn_always_moves_param_into_outliving_place`
/// before the predicate is ever consulted. They are also rows 3 and 5 of the
/// table in `warn_borrow_projection_copy`'s doc, whose subject is this same
/// copy; that table still reads exactly as written.
///
/// MEMORY: bodies only, no second free. The AOT binary is valgrind-clean on
/// the whole program — 47 allocs / 47 frees, 0 errors — before and after.
#[test]
fn test_method_projection_arg_runs_one_body() {
    let hdr = "struct R { id: i64, name: String }\n\
                impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id} {self.name}\") } }\n\
                fn mk(i: i64) -> R { return R { id: i, name: f\"h{i}\" }; }\n\
                struct W { r: R }\n\
                struct Hold { r: R, n: i64 }\n\
                struct Wh { h: Hold }\n\
                struct K { n: i64, xs: Vec[R] }\n\
                impl K {\n\
                \x20\x20\x20\x20fn eat(mut ref self, x: R) -> i64 { return x.id; }\n\
                \x20\x20\x20\x20fn reb(mut ref self, x: R) -> i64 { let y: R = x; return y.id; }\n\
                \x20\x20\x20\x20fn des(mut ref self, h: Hold) -> i64 { let Hold { r, n } = h; return r.id + n; }\n\
                \x20\x20\x20\x20fn hand(mut ref self, x: R) -> R { return x; }\n\
                \x20\x20\x20\x20fn store(mut ref self, x: R) -> i64 { let n: i64 = x.id; self.xs.push(x); return n; }\n\
                }\n\
                struct Outer { w: W, k: K }\n\
                impl Outer { fn go(mut ref self) -> i64 { return self.k.eat(self.w.r); } }\n\
                fn viaref(w: ref W) -> i64 { let mut k: K = K { n: 0, xs: Vec.new() }; return k.eat(w.r); }\n\
                trait Eater { fn chew(ref self, x: R) -> i64; }\n\
                struct E1 { n: i64 }\n\
                impl Eater for E1 { fn chew(ref self, x: R) -> i64 { return x.id; } }\n\
                fn viagen[T: Eater](e: ref T, w: ref W) -> i64 { return e.chew(w.r); }\n\
                struct Bref { n: i64 }\n\
                impl Bref { fn peek(ref self, x: ref R) -> i64 { return x.id; } }\n
                ";
    for (label, body, want) in [
        (
            "the row: projection out of a `ref` parameter",
            "let w: W = W { r: mk(1) };\n\
              println(f\"a{viaref(w)}\");",
            "a1\ndrop 1 h1\n",
        ),
        (
            "projection out of an OWNED local",
            "let w: W = W { r: mk(2) };\n\
              let mut k: K = K { n: 0, xs: Vec.new() };\n\
              println(f\"b{k.eat(w.r)}\");",
            "b2\ndrop 2 h2\n",
        ),
        (
            "projection whose callee REBINDS the param whole (`let y = x;`)",
            "let w: W = W { r: mk(3) };\n\
              let mut k: K = K { n: 0, xs: Vec.new() };\n\
              println(f\"c{k.reb(w.r)}\");",
            "c3\ndrop 3 h3\n",
        ),
        (
            "projection whose callee DESTRUCTURES the param",
            "let wh: Wh = Wh { h: Hold { r: mk(4), n: 1 } };\n\
              let mut k: K = K { n: 0, xs: Vec.new() };\n\
              println(f\"d{k.des(wh.h)}\");",
            "d5\ndrop 4 h4\n",
        ),
        (
            "projection rooted at `self` inside another method",
            "let mut o: Outer = Outer { w: W { r: mk(9) }, k: K { n: 0, xs: Vec.new() } };\n\
              println(f\"j{o.go()}\");",
            "j9\ndrop 9 h9\n",
        ),
        (
            "control: a NAMED binding argument — always agreed",
            "let r: R = mk(5);\n\
              let mut k: K = K { n: 0, xs: Vec.new() };\n\
              println(f\"e{k.eat(r)}\");",
            "e5\ndrop 5 h5\n",
        ),
        (
            "control: a FRESH TEMP argument — always agreed",
            "let mut k: K = K { n: 0, xs: Vec.new() };\n\
              println(f\"f{k.eat(mk(6))}\");",
            "drop 6 h6\nf6\n",
        ),
        (
            "control: the callee HANDS THE PROJECTION BACK — two bodies on every surface, before and after",
            "let w: W = W { r: mk(7) };\n\
              let mut k: K = K { n: 0, xs: Vec.new() };\n\
              let o: R = k.hand(w.r);\n\
              println(f\"g{o.id}\");",
            "drop 7 h7\ng7\ndrop 7 h7\n",
        ),
        (
            "control: the callee STORES the projection — two bodies on every surface, before and after",
            "let w: W = W { r: mk(8) };\n\
              let mut k: K = K { n: 0, xs: Vec.new() };\n\
              println(f\"i{k.store(w.r)}\");",
            "i8\ndrop 8 h8\ndrop 8 h8\n",
        ),
        // B-2026-09-15-24's last two NOT-MEASURED items, answered as
        // controls rather than as fixes: neither spelling ever diverged, and
        // both are here so a later widening of the predicate cannot move them
        // without saying so.
        //
        // A trait method reached through a GENERIC BOUND takes a different
        // dispatch route in the typechecker (`dispatch_trait_assoc_fn`, the
        // one arm that warns W0299 on a method argument), so it is worth its
        // own row: measured agreeing 15/15 runs on all three surfaces, on the
        // fixed tree and on the parent.
        (
            "control: a trait method through a generic bound",
            "let w: W = W { r: mk(10) };\n\
             let e: E1 = E1 { n: 0 };\n\
             println(f\"k{viagen(e, w)}\");",
            "k10\ndrop 10 h10\n",
        ),
        // A `ref R` PARAMETER separates the projection READ from the by-value
        // parameter: the read alone was never the defect, which is why this
        // one body was always right on every surface.
        (
            "control: a `ref R` parameter — the read alone is not the defect",
            "let w: W = W { r: mk(11) };\n\
             let b: Bref = Bref { n: 0 };\n\
             println(f\"m{b.peek(w.r)}\");",
            "m11\ndrop 11 h11\n",
        ),
    ] {
        let src = format!("{hdr}fn main() {{\n{body}\nprintln(\"end\");\n}}\n");
        assert_eq!(run(&src), format!("{want}end\n"), "[{label}]");
    }
}

/// B-2026-09-21-4 — the interpreter TWIN of
/// `test_e2e_one_binding_passed_twice_by_value_in_one_call`.
///
/// The interpreter was already correct here and is the oracle the compiled
/// fix was measured against, so this cell asserts no change in behaviour. It
/// is not vacuous for that reason: it is what makes the compiled fixture's
/// expected string a claim about BOTH backends rather than about codegen
/// alone, and it is what would catch a future interpreter change that made
/// one binding passed twice diverge in the other direction.
#[test]
fn one_binding_passed_twice_by_value_in_one_call() {
    let src = r#"
enum G1[T] { Y(T), N }

fn one[T](a: G1[T]) {
    match a { G1.Y(v) => { println(f"  1x {v}") } G1.N => { println("  1x NONE") } }
}
fn two[T](a: G1[T], b: G1[T]) {
    match a { G1.Y(v) => { println(f"  ax {v}") } G1.N => { println("  ax NONE") } }
    match b { G1.Y(v) => { println(f"  bx {v}") } G1.N => { println("  bx NONE") } }
}
fn three[T](a: G1[T], b: G1[T], c: G1[T]) {
    match a { G1.Y(v) => { println(f"  ax {v}") } G1.N => { println("  ax NONE") } }
    match b { G1.Y(v) => { println(f"  bx {v}") } G1.N => { println("  bx NONE") } }
    match c { G1.Y(v) => { println(f"  cx {v}") } G1.N => { println("  cx NONE") } }
}
fn twoc(a: G1[String], b: G1[String]) {
    match a { G1.Y(v) => { println(f"  ax {v}") } G1.N => { println("  ax NONE") } }
    match b { G1.Y(v) => { println(f"  bx {v}") } G1.N => { println("  bx NONE") } }
}

fn a_gen_dup() { println("a"); let g: G1[String] = G1.Y(f"pa"); two(g, g) }
fn b_con_dup() { println("b"); let g: G1[String] = G1.Y(f"pb"); twoc(g, g) }
fn c_triple() { println("c"); let g: G1[String] = G1.Y(f"pc"); three(g, g, g) }
fn d_distinct() { println("d"); let g: G1[String] = G1.Y(f"pd"); let h: G1[String] = G1.Y(f"qd"); two(g, h) }
fn e_single() { println("e"); let g: G1[String] = G1.Y(f"pe"); one(g) }
fn f_twocalls() { println("f"); let g: G1[String] = G1.Y(f"pf"); one(g); one(g) }
fn g_middle() { println("g"); let g: G1[String] = G1.Y(f"pg"); let h: G1[String] = G1.Y(f"qg"); three(g, h, g) }

fn main() {
    a_gen_dup();
    b_con_dup();
    c_triple();
    d_distinct();
    e_single();
    f_twocalls();
    g_middle();
    println("end");
}
"#;
    assert_eq!(run(src), "a\n  ax pa\n  bx pa\nb\n  ax pb\n  bx pb\nc\n  ax pc\n  bx pc\n  cx pc\nd\n  ax pd\n  bx qd\ne\n  1x pe\nf\n  1x pf\n  1x pf\ng\n  ax pg\n  bx qg\n  cx pg\nend\n");
}
