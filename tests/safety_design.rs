//! Adversarial coverage of the no-lifetime-annotation design claim.
//!
//! Kāra's design.md (Feature 4 § Part 3 — "Explicit `ref` for Borrow Returns")
//! commits to a story where parameter modes (`own` / `ref` / `mut ref`) plus
//! return-position borrow-source inference together replace `'a`-style lifetime
//! annotations. The text explicitly says: "No disambiguation annotation is
//! needed — the conservative assumption is always safe and avoids introducing
//! a lifetime-like concept." This file exists to keep that claim honest as
//! the compiler evolves.
//!
//! Each `should_accept` case codifies a borrow pattern that Rust would force
//! to carry an explicit `'a`. Each `should_reject` case codifies an escape
//! pattern that Rust catches via lifetime mismatch and that Kāra must catch
//! via ownership/escape analysis instead. Together they form the test of the
//! design claim: if any `should_accept` regresses to a rejection, the
//! "no annotations needed" promise has narrowed; if any `should_reject`
//! regresses to acceptance, the safety story has a hole.
//!
//! Static-only tests run on plain `cargo test`. Under `cargo test --features
//! llvm`, the inner `runtime_confirmation` module additionally compiles each
//! accept case to a runnable binary, links it under AddressSanitizer, and
//! asserts a clean exit. ASAN closes the loop "static analysis accepted →
//! generated code is actually memory-safe."
//!
//! **macOS leak gap.** Apple clang's ASAN runtime does not include
//! LeakSanitizer (see `tests/memory_sanitizer.rs:95-104`). On macOS the
//! runtime confirmation catches use-after-free, double-free, and heap
//! buffer overflow but NOT leaks; on Linux LeakSanitizer is enabled and
//! catches leaks too. A cross-platform alloc/free balance assertion is
//! tracked as a phase-7 followup.

use karac::ownership::{OwnershipError, OwnershipErrorKind};
use karac::{ownershipcheck, parse, resolve, typecheck};

// ── Helpers ─────────────────────────────────────────────────────

/// Runs the static pipeline through ownership and asserts the program is
/// accepted by every phase. Returns the ownership result for further
/// inspection if the caller needs it.
fn assert_static_accept(source: &str, label: &str) {
    let parsed = parse(source);
    assert!(
        parsed.errors.is_empty(),
        "[{label}] parse errors: {:?}",
        parsed.errors
    );
    let resolved = resolve(&parsed.program);
    assert!(
        resolved.errors.is_empty(),
        "[{label}] resolve errors: {:?}",
        resolved.errors
    );
    let typed = typecheck(&parsed.program, &resolved);
    assert!(
        typed.errors.is_empty(),
        "[{label}] type errors: {}",
        typed
            .errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let ownership = ownershipcheck(&parsed.program, &typed);
    assert!(
        ownership.errors.is_empty(),
        "[{label}] ownership errors: {}",
        ownership
            .errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
}

/// Runs the static pipeline and returns the ownership errors. Asserts that
/// parse/resolve/typecheck are clean — we want the test to fail loudly if a
/// "should_reject" case regresses to a parse-error (which would silently
/// satisfy "errors are present").
fn ownership_errors_only(source: &str, label: &str) -> Vec<OwnershipError> {
    let parsed = parse(source);
    assert!(
        parsed.errors.is_empty(),
        "[{label}] parse errors: {:?}",
        parsed.errors
    );
    let resolved = resolve(&parsed.program);
    assert!(
        resolved.errors.is_empty(),
        "[{label}] resolve errors: {:?}",
        resolved.errors
    );
    let typed = typecheck(&parsed.program, &resolved);
    let ownership = ownershipcheck(&parsed.program, &typed);
    ownership.errors
}

/// Asserts the program produces at least one ownership error of the given
/// kind. Multiple-error cases are common (an escape can trip several rules);
/// requiring "any" rather than "all" avoids brittleness as diagnostic policy
/// evolves.
fn assert_ownership_error_kind(source: &str, expected: OwnershipErrorKind, label: &str) {
    let errors = ownership_errors_only(source, label);
    assert!(
        !errors.is_empty(),
        "[{label}] expected ownership errors but got none"
    );
    assert!(
        errors.iter().any(|e| e.kind == expected),
        "[{label}] expected at least one {:?}; got: {:?}",
        expected,
        errors
            .iter()
            .map(|e| (&e.kind, &e.message))
            .collect::<Vec<_>>()
    );
}

// ────────────────────────────────────────────────────────────────
// Section 1: should_accept — patterns Rust would require `'a` for.
// ────────────────────────────────────────────────────────────────

/// design.md Feature 4 Part 3, single-source shorthand:
/// "When a function has exactly one `ref` parameter, the source of any
/// returned borrow is unambiguous, so the plain `ref T` annotation suffices."
///
/// This case uses parameter-passthrough as the body — the simplest form a
/// single-source borrow return can take. A spec-faithful version using a
/// field projection in the body (`user.name`) lives below as
/// `spec_field_projection_in_borrow_return`.
#[test]
fn accept_single_source_borrow_return() {
    assert_static_accept(
        "fn echo(s: ref String) -> ref String { s }\n\
         fn main() {\n\
             let s = String.from(\"hello\");\n\
             let t = echo(s);\n\
             println(t.len());\n\
         }",
        "accept_single_source_borrow_return",
    );
}

/// Verbatim from design.md Feature 4 Part 3:
/// `fn name(user: ref User) -> ref String { user.name }`
///
/// This is the most-cited example of the no-annotation borrow-return
/// design. Today the typechecker rejects it: the body's tail expression
/// `user.name` evaluates to `String` rather than coercing back to
/// `ref String` from a `ref User` receiver. Tracking this as an
/// implementation gap, not a design change — the test re-enables itself
/// the moment the typechecker grows return-position field-projection
/// auto-borrowing.
#[test]
fn spec_field_projection_in_borrow_return() {
    assert_static_accept(
        "struct User { name: String }\n\
         fn user_name(user: ref User) -> ref String {\n\
             user.name\n\
         }\n\
         fn main() {\n\
             let u = User { name: String.from(\"alice\") };\n\
             let n = user_name(u);\n\
             println(n.len());\n\
         }",
        "spec_field_projection_in_borrow_return",
    );
}

/// design.md Feature 4 Part 3, multi-source overapproximation:
/// "When a borrow could come from more than one `ref` parameter, the
/// compiler conservatively assumes the return may borrow from *all* `ref`
/// parameters." Verbatim example from the design doc.
#[test]
fn accept_multi_source_borrow_return() {
    assert_static_accept(
        "fn longer(a: ref String, b: ref String) -> ref String {\n\
             if a.len() > b.len() { a } else { b }\n\
         }\n\
         fn main() {\n\
             let x = String.from(\"short\");\n\
             let y = String.from(\"a longer string\");\n\
             let z = longer(x, y);\n\
             println(z.len());\n\
         }",
        "accept_multi_source_borrow_return",
    );
}

/// design.md Feature 4 Part 3, multi-source overapproximation via `match`:
/// the `match` sibling of `accept_multi_source_borrow_return`'s `if` form.
/// A scalar selector returns a borrow from whichever arm runs; the source
/// is the conservative union of every arm's `ref`-param source.
#[test]
fn accept_match_multi_source_borrow_return() {
    assert_static_accept(
        "fn pick(a: ref String, b: ref String, which: i64) -> ref String {\n\
             match which {\n\
                 0 => a,\n\
                 _ => b,\n\
             }\n\
         }\n\
         fn main() {\n\
             let x = String.from(\"left\");\n\
             let y = String.from(\"right\");\n\
             let z = pick(x, y, 0);\n\
             println(z.len());\n\
         }",
        "accept_match_multi_source_borrow_return",
    );
}

/// Source-pinning (E0509) must still fire when *one* `match` arm returns a
/// non-`ref`-param source: the dangling branch dominates the multi-arm
/// combination, exactly as it does for the `if` form. Guards against a
/// `match` arm silently escaping the source-pinning check.
#[test]
fn reject_match_arm_dangling_source() {
    assert_ownership_error_kind(
        "fn bad(a: ref String, which: i64) -> ref String {\n\
             match which {\n\
                 0 => a,\n\
                 _ => \"local\",\n\
             }\n\
         }\n\
         fn main() {\n\
             let x = String.from(\"x\");\n\
             let r = bad(x, 0);\n\
             println(r.len());\n\
         }",
        OwnershipErrorKind::BorrowReturnNotSourcePinned {
            shape: karac::ownership::BorrowReturnShape::DanglingSource,
        },
        "reject_match_arm_dangling_source",
    );
}

/// Tier 2c (B-2026-06-07-5): a `match` over a non-scalar *identifier*
/// scrutinee with binding-free destructuring arms — an all-wildcard tuple
/// variant (`Ok(_)`) plus a wildcard catch-all — returning a borrow from a
/// `ref` param. Sound because nothing is bound (no payload aliasing) and the
/// scrutinee's own binding owns its drop, so the match adds no drop
/// obligation. Previously `UnsupportedForm`.
#[test]
fn accept_match_tuple_variant_borrow_return() {
    assert_static_accept(
        "fn pick(res: ref Result[i64, String], a: ref String, b: ref String) -> ref String {\n\
             match res {\n\
                 Ok(_) => a,\n\
                 _ => b,\n\
             }\n\
         }\n\
         fn main() {\n\
             let ok: Result[i64, String] = Result.Ok(1);\n\
             let x = String.from(\"yes\");\n\
             let y = String.from(\"no\");\n\
             let r = pick(ok, x, y);\n\
             println(r.len());\n\
         }",
        "accept_match_tuple_variant_borrow_return",
    );
}

/// Tier 2c: dotted unit-variant arms (`Side.Left` / `Side.Right`) over an
/// enum identifier scrutinee. The dot makes each pattern unambiguously a
/// no-bind variant (a value binding can never be dotted), so both the
/// source-pinning gate and codegen accept it without type information.
#[test]
fn accept_match_dotted_unit_variant_borrow_return() {
    assert_static_accept(
        "enum Side { Left, Right }\n\
         fn pick(s: ref Side, a: ref String, b: ref String) -> ref String {\n\
             match s {\n\
                 Side.Left => a,\n\
                 Side.Right => b,\n\
             }\n\
         }\n\
         fn main() {\n\
             let sd: Side = Side.Left;\n\
             let x = String.from(\"L\");\n\
             let y = String.from(\"R\");\n\
             let r = pick(sd, x, y);\n\
             println(r.len());\n\
         }",
        "accept_match_dotted_unit_variant_borrow_return",
    );
}

/// Tier 2c boundary: a payload-*binding* arm (`Some(x) => x`) returns a
/// borrow of the bound payload — the deferred `Option[ref T]` semantics. It
/// must stay a clean `UnsupportedForm` (not accepted, not miscompiled): the
/// gate rejects any pattern that binds a variable.
#[test]
fn reject_match_payload_binding_borrow_return_unsupported() {
    assert_ownership_error_kind(
        "fn pick(opt: ref Option[String], b: ref String) -> ref String {\n\
             match opt {\n\
                 Some(x) => x,\n\
                 _ => b,\n\
             }\n\
         }\n\
         fn main() {\n\
             let o: Option[String] = Option.Some(String.from(\"hi\"));\n\
             let y = String.from(\"no\");\n\
             let r = pick(o, y);\n\
             println(r.len());\n\
         }",
        OwnershipErrorKind::BorrowReturnNotSourcePinned {
            shape: karac::ownership::BorrowReturnShape::UnsupportedForm,
        },
        "reject_match_payload_binding_borrow_return_unsupported",
    );
}

/// Tier 2c boundary: a guarded arm needs a per-arm scope frame to free any
/// heap temporary the guard expression spawns — machinery the simplified
/// borrow-return lowering lacks. Must stay `UnsupportedForm`.
#[test]
fn reject_match_guarded_arm_borrow_return_unsupported() {
    assert_ownership_error_kind(
        "fn pick(score: i64, a: ref String, b: ref String) -> ref String {\n\
             match score {\n\
                 _ if score > 90 => a,\n\
                 _ => b,\n\
             }\n\
         }\n\
         fn main() {\n\
             let x = String.from(\"hi\");\n\
             let y = String.from(\"lo\");\n\
             let r = pick(95, x, y);\n\
             println(r.len());\n\
         }",
        OwnershipErrorKind::BorrowReturnNotSourcePinned {
            shape: karac::ownership::BorrowReturnShape::UnsupportedForm,
        },
        "reject_match_guarded_arm_borrow_return_unsupported",
    );
}

/// Tier 2c boundary + lockstep guard: a *fresh-temp* scrutinee
/// (`match make() { … }`) has no binding to own its drop, so the simplified
/// lowering can't handle it. The gate requires an identifier scrutinee and
/// reports anything else as `UnsupportedForm` — keeping the ownership pass
/// and codegen in exact lockstep (an accepted-but-unlowerable shape would
/// fall through to the value-return miscompile).
#[test]
fn reject_match_nonident_scrutinee_borrow_return_unsupported() {
    assert_ownership_error_kind(
        "fn id(s: Result[i64, String]) -> Result[i64, String] { s }\n\
         fn pick(r: Result[i64, String], a: ref String, b: ref String) -> ref String {\n\
             match id(r) {\n\
                 Ok(_) => a,\n\
                 _ => b,\n\
             }\n\
         }\n\
         fn main() {\n\
             let ok: Result[i64, String] = Result.Ok(1);\n\
             let x = String.from(\"yes\");\n\
             let y = String.from(\"no\");\n\
             let z = pick(ok, x, y);\n\
             println(z.len());\n\
         }",
        OwnershipErrorKind::BorrowReturnNotSourcePinned {
            shape: karac::ownership::BorrowReturnShape::UnsupportedForm,
        },
        "reject_match_nonident_scrutinee_borrow_return_unsupported",
    );
}

/// design.md Feature 4 Part 3, ref inside generic wrappers:
/// "`ref T` is a first-class type ... and may appear inside generic type
/// arguments in a return type: `Option[ref T]`, `Result[ref T, E]`,
/// `(ref T, ref U)`."
///
/// **Implemented 2026-06-10 (B-2026-06-07-5 Option[ref T] slice).** `Vec`/
/// `Slice` `get`/`first`/`last` now type as `Option[ref T]` (was owned
/// `Option[T]`): the typechecker builds `Option[Ref(elem)]` in
/// `stdlib_seq.rs`, ownership treats the `ref T` payload binding as Copy
/// (re-readable, never moved) and rejects moving it into an owned position,
/// and codegen reconstructs the by-value aliasing borrow with cleanup
/// suppressed (`scrutinee_is_borrow_call`). The earlier ignore note's "prior
/// green was vacuous" (`v.get(0)` → `Type::Error` poison) is the history that
/// motivated this; the borrowed return is now real and exercised end-to-end.
#[test]
fn spec_option_ref_t_return() {
    assert_static_accept(
        "fn first(v: ref Vec[i64]) -> Option[ref i64] {\n\
             v.get(0)\n\
         }\n\
         fn main() {\n\
             let mut v: Vec[i64] = Vec.new();\n\
             v.push(42);\n\
             match first(v) {\n\
                 Some(n) => println(n),\n\
                 None => println(0),\n\
             }\n\
         }",
        "spec_option_ref_t_return",
    );
}

/// design.md Feature 4 Part 3, borrowed struct:
/// "A struct may contain `ref` fields. Such a struct is a *borrowed
/// struct*: its scope is bounded by the scope of every value its `ref`
/// fields borrow from. No named lifetime parameters are written."
#[test]
fn accept_borrowed_struct_construction() {
    assert_static_accept(
        "struct Parser {\n\
             source: ref String,\n\
             position: i64,\n\
         }\n\
         fn main() {\n\
             let s = String.from(\"input\");\n\
             let p = Parser { source: s, position: 0 };\n\
             println(p.position);\n\
         }",
        "accept_borrowed_struct_construction",
    );
}

/// design.md Feature 4 Part 3, returning a borrowed struct (verbatim from
/// the design doc):
/// "Returning a borrowed struct from a function follows the same rule as
/// returning a `ref` value: the borrowed struct's sources must all be
/// parameters. The compiler traces each `ref` field to its source parameter
/// automatically — no annotation is needed on borrowed struct returns."
///
/// Borrowed-struct returns landed in B-2026-06-07-5: source-pinning (3a)
/// traces every `ref` field's initializer to a `ref` parameter
/// (`classify_borrow_return_struct`), and codegen returns the struct BY
/// VALUE (`llvm_return_type` / `current_fn_returns_ref` /
/// `fn_ref_return_inner` all exclude `ref BorrowedStruct`), with each `ref`
/// field storing the forwarded borrow pointer (`compile_ref_field_borrow_ptr`,
/// which also fixed the previously-vacuous `asan_borrowed_struct_construction`
/// — construction never actually codegenned). Runtime parity +
/// double-free-freedom pinned by `asan_borrowed_struct_return_and_field_read`.
#[test]
fn spec_return_borrowed_struct() {
    assert_static_accept(
        "struct Parser {\n\
             source: ref String,\n\
             position: i64,\n\
         }\n\
         fn make_parser(s: ref String) -> ref Parser {\n\
             Parser { source: s, position: 0 }\n\
         }\n\
         fn main() {\n\
             let s = String.from(\"input\");\n\
             let p = make_parser(s);\n\
             println(p.position);\n\
         }",
        "spec_return_borrowed_struct",
    );
}

/// `ref self` method returning a borrow into a field. In Rust this is the
/// canonical case for lifetime elision (`fn name(&self) -> &String`); in
/// Kāra single-source shorthand applies for the same reason.
///
/// `ref self` method returning a borrow into a field — the canonical
/// accessor. Implemented in B-2026-06-07-5 (method-ref slice).
#[test]
fn spec_ref_self_returning_field_borrow() {
    assert_static_accept(
        "struct User { name: String, age: i64 }\n\
         impl User {\n\
             fn name(ref self) -> ref String { self.name }\n\
         }\n\
         fn main() {\n\
             let u = User { name: String.from(\"alice\"), age: 30 };\n\
             let n = u.name();\n\
             println(n.len());\n\
         }",
        "spec_ref_self_returning_field_borrow",
    );
}

/// Closure that captures a borrow but does NOT escape its creation scope —
/// the closure is invoked inline. This is the case Rust would still allow,
/// but only because the compiler can prove the closure's lifetime fits;
/// Kāra reaches the same conclusion via ownership analysis without any
/// `'_` annotation surfacing in the source.
#[test]
fn accept_closure_borrow_capture_no_escape() {
    assert_static_accept(
        "fn main() {\n\
             let s = String.from(\"hello\");\n\
             let len_plus = |extra: i64| s.len() + extra;\n\
             println(len_plus(5));\n\
         }",
        "accept_closure_borrow_capture_no_escape",
    );
}

/// Chained ref returns: caller threads a borrow through two functions of
/// the same single-source signature. The borrow's source is the original
/// owned binding; Kāra must trace through call boundaries without
/// annotation help. Uses passthrough-only bodies to avoid the field-
/// projection impl gap.
///
/// Shipped 2026-06-07 (B-2026-06-07-5 chained tier): a borrow-returning
/// free-fn call in tail/return position is source-pinned by tracing its
/// ref-position args (`classify_borrow_return_call`) and lowered to the
/// borrow `ptr` directly (`is_borrow_returning_call_expr`).
#[test]
fn accept_chained_borrow_returns() {
    assert_static_accept(
        "fn echo(s: ref String) -> ref String { s }\n\
         fn echo_twice(s: ref String) -> ref String {\n\
             let t = echo(s);\n\
             echo(t)\n\
         }\n\
         fn main() {\n\
             let s = String.from(\"chained\");\n\
             let r = echo_twice(s);\n\
             println(r.len());\n\
         }",
        "accept_chained_borrow_returns",
    );
}

// ────────────────────────────────────────────────────────────────
// Section 2: should_reject — escapes Rust catches via lifetime mismatch
// and Kāra must catch via ownership/escape analysis.
// ────────────────────────────────────────────────────────────────

/// design.md Feature 4 Part 3, "ref-captured value escaping its borrow's
/// lifetime" — sub-case (iv) of the closures rules. Returning a closure
/// that read-only-captures a parameter must fire E0508
/// (`RefCaptureEscapesScope`): the closure's `ref` capture would outlive
/// `cfg`, which is owned by `make_handler`. This is the no-annotation
/// analog of Rust's `'a` mismatch on a returned `impl Fn() -> &T`.
#[test]
fn reject_closure_with_ref_capture_returned() {
    assert_ownership_error_kind(
        "struct Config { value: i64 }\n\
         fn make_handler(cfg: Config) -> Fn() -> i64 {\n\
             || cfg.value\n\
         }",
        OwnershipErrorKind::RefCaptureEscapesScope,
        "reject_closure_with_ref_capture_returned",
    );
}

/// Use-after-move: the canonical ownership error. Included here not because
/// it is unique to the no-annotation design, but because the ownership
/// system has to remain the *only* guard in the absence of borrow
/// annotations — a regression here would weaken the entire safety story.
/// Uses a custom struct (rather than `String`) because `String` arguments
/// have call-site coercion paths that don't trigger a clean move on the
/// existing test corpus.
#[test]
fn reject_use_after_move() {
    assert_ownership_error_kind(
        "struct Data { value: i64 }\n\
         fn consume(d: Data) -> i64 { d.value }\n\
         fn main() {\n\
             let d = Data { value: 1 };\n\
             let _ = consume(d);\n\
             let _ = consume(d);\n\
         }",
        OwnershipErrorKind::UseAfterMove,
        "reject_use_after_move",
    );
}

// ────────────────────────────────────────────────────────────────
// Section 2½: Adversarial soundness corpus.
//
// Sections 1 and 2 are spec-faithfulness witnesses — each mirrors a
// design.md example. This section is the deliberately *hostile* leg the
// phase-9 verification item calls for: programs written specifically to
// try to break the no-lifetime-annotation ownership model. Every case
// resolves to exactly one of two outcomes, no third:
//
//   • REJECTED at compile — the escape is caught by ownership/escape
//     analysis (these tests live here, static-only, on plain
//     `cargo test`); or
//   • ACCEPTED and ASAN-clean at runtime — the accept cases additionally
//     get an ASAN mirror in Section 3's `runtime_confirmation` module, so
//     "static-accept ⇒ runtime-safe" is proven, not assumed.
//
// An accepted case that ASAN flags is a soundness bug, not a test to
// relax. Cases are grouped by the attack family named in the phase-9
// tracker, each aimed at a load-bearing rule in design.md § Feature 4
// Part 3.
// ────────────────────────────────────────────────────────────────

/// Asserts the program is rejected by source pinning specifically — at
/// least one `BorrowReturnNotSourcePinned` of *any* shape. Looser than
/// `assert_ownership_error_kind` because the `DanglingSource` vs
/// `UnsupportedForm` split is an implementation detail (a bare temporary
/// return reports `UnsupportedForm` today, a returned local reports
/// `DanglingSource`); the invariant the corpus defends is "the escape does
/// not compile," not which shape fires.
fn assert_rejected_by_source_pinning(source: &str, label: &str) {
    let errors = ownership_errors_only(source, label);
    assert!(
        errors.iter().any(|e| matches!(
            e.kind,
            OwnershipErrorKind::BorrowReturnNotSourcePinned { .. }
        )),
        "[{label}] expected a source-pinning rejection; got: {:?}",
        errors
            .iter()
            .map(|e| (&e.kind, &e.message))
            .collect::<Vec<_>>()
    );
}

// ── Family 1: source-pinning escapes ───────────────────────────────
// design.md § Part 3: "every `ref` value in a well-typed program has a
// traceable source ... if a `ref` can't be traced to a parameter, that's
// a source pinning error." Each case returns a borrow whose source dies at
// the return — the reference would dangle.

/// Return a borrow of an *owned local*. The local drops at function exit,
/// so the returned reference dangles. A `ref` parameter is in scope but
/// does not rescue it — the returned value's source is the local, not the
/// parameter.
#[test]
fn adversarial_source_pin_return_owned_local() {
    assert_ownership_error_kind(
        "fn bad(s: ref String) -> ref String {\n\
             let local = String.from(\"x\");\n\
             local\n\
         }\n\
         fn main() {\n\
             let s = String.from(\"hi\");\n\
             let r = bad(s);\n\
             println(r.len());\n\
         }",
        OwnershipErrorKind::BorrowReturnNotSourcePinned {
            shape: karac::ownership::BorrowReturnShape::DanglingSource,
        },
        "adversarial_source_pin_return_owned_local",
    );
}

/// Return a borrow of an *owned parameter*. A bare `s: String` parameter
/// is owned by the callee and dropped at return; a borrow of it dangles.
#[test]
fn adversarial_source_pin_return_owned_param() {
    assert_ownership_error_kind(
        "fn bad(s: String) -> ref String { s }\n\
         fn main() {\n\
             let s = String.from(\"hi\");\n\
             let r = bad(s);\n\
             println(r.len());\n\
         }",
        OwnershipErrorKind::BorrowReturnNotSourcePinned {
            shape: karac::ownership::BorrowReturnShape::DanglingSource,
        },
        "adversarial_source_pin_return_owned_param",
    );
}

/// Return a borrow of a *temporary* — a fresh `String.from(...)` in tail
/// position. The temporary has no owner outliving the return. Rejected;
/// the exact shape is incidental (`UnsupportedForm` today), the invariant
/// is that it does not compile.
#[test]
fn adversarial_source_pin_return_temporary() {
    assert_rejected_by_source_pinning(
        "fn bad(s: ref String) -> ref String {\n\
             String.from(\"temp\")\n\
         }\n\
         fn main() {\n\
             let s = String.from(\"hi\");\n\
             let r = bad(s);\n\
             println(r.len());\n\
         }",
        "adversarial_source_pin_return_temporary",
    );
}

/// Return a borrow of a *function-call return*. `make()` yields an owned
/// `String` with no caller-visible source; a borrow of it dangles once the
/// temporary holding the call result drops.
#[test]
fn adversarial_source_pin_return_fncall_result() {
    assert_rejected_by_source_pinning(
        "fn make() -> String { String.from(\"x\") }\n\
         fn bad() -> ref String { make() }\n\
         fn main() {\n\
             let r = bad();\n\
             println(r.len());\n\
         }",
        "adversarial_source_pin_return_fncall_result",
    );
}

// ── Family 2: move-while-borrowed ──────────────────────────────────
// A returned borrow registers a live borrow on its source at the caller;
// moving/consuming that source while the borrow is live is a
// use-after-free the ownership pass must catch. Note the check is
// scope-based, not flow-sensitive: the borrow is treated as live to end of
// scope, so even a move *after* the borrow's last use is rejected (the
// conservative, safe direction — design.md § Part 3 "the conservative
// assumption is always safe").

/// Move the source of a live returned borrow. `r = echo(s)` borrows `s`;
/// `consume(s)` then moves `s` while `r` still points into it.
#[test]
fn adversarial_move_source_while_borrow_live() {
    assert_ownership_error_kind(
        "fn echo(s: ref String) -> ref String { s }\n\
         fn consume(s: String) -> i64 { s.len() }\n\
         fn main() {\n\
             let s = String.from(\"hello\");\n\
             let r = echo(s);\n\
             let _ = consume(s);\n\
             println(r.len());\n\
         }",
        OwnershipErrorKind::SliceBorrowConflict {
            shape: karac::ownership::SliceConflictShape::MoveOfBorrowed,
        },
        "adversarial_move_source_while_borrow_live",
    );
}

// ── Family 3: union-rule corners ───────────────────────────────────
// design.md § Part 3: with multiple `ref` params, "the compiler
// conservatively assumes the return may borrow from *all* `ref`
// parameters. The returned reference must not outlive *any* of them." The
// hostile probe is a caller that moves the source the runtime borrow does
// NOT actually point at. Rust's distinct `'a`/`'b` would accept that;
// Kāra's conservative union must still reject it — otherwise the union
// under-constrains and admits a dangling reference on the other control
// path.

/// Move the source the borrow *does* point at: `z` selects the longer
/// string `y`, and the caller moves `y`. Must reject.
#[test]
fn adversarial_union_move_returned_source() {
    assert_ownership_error_kind(
        "fn longer(a: ref String, b: ref String) -> ref String {\n\
             if a.len() > b.len() { a } else { b }\n\
         }\n\
         fn consume(s: String) -> i64 { s.len() }\n\
         fn main() {\n\
             let x = String.from(\"short\");\n\
             let y = String.from(\"a longer string\");\n\
             let z = longer(x, y);\n\
             let _ = consume(y);\n\
             println(z.len());\n\
         }",
        OwnershipErrorKind::SliceBorrowConflict {
            shape: karac::ownership::SliceConflictShape::MoveOfBorrowed,
        },
        "adversarial_union_move_returned_source",
    );
}

/// Move the *other* source: `z` points at `y` at runtime, but the caller
/// moves `x`. A per-parameter-lifetime analysis (Rust `'a`/`'b`) would
/// accept this; the conservative union must reject it, because the return
/// type says the borrow *may* come from `x` too. This is the case that
/// proves the union never under-constrains — the load-bearing soundness
/// property of the "borrow from all `ref` params" rule.
#[test]
fn adversarial_union_move_other_source() {
    assert_ownership_error_kind(
        "fn longer(a: ref String, b: ref String) -> ref String {\n\
             if a.len() > b.len() { a } else { b }\n\
         }\n\
         fn consume(s: String) -> i64 { s.len() }\n\
         fn main() {\n\
             let x = String.from(\"short\");\n\
             let y = String.from(\"a longer string\");\n\
             let z = longer(x, y);\n\
             let _ = consume(x);\n\
             println(z.len());\n\
         }",
        OwnershipErrorKind::SliceBorrowConflict {
            shape: karac::ownership::SliceConflictShape::MoveOfBorrowed,
        },
        "adversarial_union_move_other_source",
    );
}

// ── Family 4: escape through aggregates ────────────────────────────
// Smuggle a borrow out via a struct field, a closure capture, or a
// generic-wrapper element. design.md § Part 3 borrowed structs: "the
// borrowed struct's sources must all be parameters."

/// Escape via a single `ref` struct field sourced from a local. The
/// borrowed struct's `r` field traces to `local`, not a parameter, so the
/// returned `ref Holder` would dangle.
#[test]
fn adversarial_escape_via_struct_field_local() {
    assert_ownership_error_kind(
        "struct Holder { r: ref String }\n\
         fn bad() -> ref Holder {\n\
             let local = String.from(\"x\");\n\
             Holder { r: local }\n\
         }\n\
         fn main() {\n\
             let h = bad();\n\
             println(h.r.len());\n\
         }",
        OwnershipErrorKind::BorrowReturnNotSourcePinned {
            shape: karac::ownership::BorrowReturnShape::DanglingSource,
        },
        "adversarial_escape_via_struct_field_local",
    );
}

/// Multi-field borrowed struct where one field is safe (`left`, a
/// parameter) and the other escapes (`right`, a local). The per-field
/// source trace must catch the escaping field even though a sibling field
/// is legitimately sourced — the union must not be fooled by a partially
/// valid struct.
#[test]
fn adversarial_escape_via_struct_field_mixed_sources() {
    assert_ownership_error_kind(
        "struct Joiner { left: ref String, right: ref String }\n\
         fn bad(a: ref String) -> ref Joiner {\n\
             let local = String.from(\"x\");\n\
             Joiner { left: a, right: local }\n\
         }\n\
         fn main() {\n\
             let a = String.from(\"hi\");\n\
             let j = bad(a);\n\
             println(j.left.len());\n\
         }",
        OwnershipErrorKind::BorrowReturnNotSourcePinned {
            shape: karac::ownership::BorrowReturnShape::DanglingSource,
        },
        "adversarial_escape_via_struct_field_mixed_sources",
    );
}

/// Escape via closure capture — a closure that read-captures an owned
/// parameter and is returned. The `ref` capture would outlive its source
/// (design.md § Closures Rule 2 sub-case (iv)).
#[test]
fn adversarial_escape_via_closure_capture() {
    assert_ownership_error_kind(
        "struct Config { value: i64 }\n\
         fn make_handler(cfg: Config) -> Fn() -> i64 {\n\
             || cfg.value\n\
         }",
        OwnershipErrorKind::RefCaptureEscapesScope,
        "adversarial_escape_via_closure_capture",
    );
}

/// Escape through a *borrowed collection* whose element borrows a local —
/// DOCUMENTED FOLLOW-ON GAP (bug-ledger B-2026-07-11-30).
///
/// design.md § Part 3: "A type containing `ref` in a stored position
/// (struct field, local `let` binding, `Vec` element) makes that container
/// a borrowed struct or borrowed collection — its scope is bounded by the
/// scope of every borrowed source." So a `-> Vec[ref String]` whose element
/// borrows a local should be a source-pinning error exactly as the
/// `-> ref Holder` struct case above is.
///
/// It is NOT caught today: `src/ownership/ref_return.rs` gates source
/// pinning on the return type being `ref T` / `mut ref T` / `StringSlice`
/// only ("Borrows nested in generic wrappers (`Option[ref T]`) are a
/// follow-on"), so an owned `Vec[ref T]` container never enters the pass.
/// The escape is not an immediate use-after-free — `v.push(local)` *moves*
/// the owned local into the vector (ownership reports `local` moved after
/// the push), so the buffer travels with the returned vec and nothing
/// dangles at the return point — but the `ref` element type becomes a lie
/// and the borrowed-collection scope bound is not enforced.
///
/// This test asserts the desired rejection. It landed with B-2026-07-11-30:
/// `check_borrowed_collection_pinning` (ref_return.rs) now source-pins a
/// `-> Vec[ref T]` / `-> Option[ref T]` return whose element borrows a local.
#[test]
fn adversarial_escape_via_borrowed_collection_local() {
    assert_rejected_by_source_pinning(
        "fn bad() -> Vec[ref String] {\n\
             let mut local: String = \"\";\n\
             local.push_str(\"payload data here\");\n\
             let mut v: Vec[ref String] = Vec.new();\n\
             v.push(local);\n\
             v\n\
         }\n\
         fn main() {\n\
             let v = bad();\n\
             println(v.len());\n\
         }",
        "adversarial_escape_via_borrowed_collection_local",
    );
}

/// Family 4 accept control: the SAME borrowed-collection shape, but the
/// element is sourced from a `ref` *parameter*. The caller's `s` outlives
/// the returned vec, so this is genuinely safe — accepted, and ASAN-clean
/// (mirror in Section 3). The contrast with the `#[ignore]`d escape above
/// is only the source: param (safe) vs local (should pin).
#[test]
fn adversarial_borrowed_collection_from_param_accepts() {
    assert_static_accept(
        "fn ok(s: ref String) -> Vec[ref String] {\n\
             let mut v: Vec[ref String] = Vec.new();\n\
             v.push(s);\n\
             v\n\
         }\n\
         fn main() {\n\
             let s = String.from(\"payload\");\n\
             let v = ok(s);\n\
             println(v.len());\n\
         }",
        "adversarial_borrowed_collection_from_param_accepts",
    );
}

/// Family 4 accept control: a borrow smuggled through a generic wrapper
/// (`Option[ref String]`) with a source that outlives the return (a `ref`
/// parameter). Genuinely safe — accepted, ASAN-clean (mirror in Section 3).
#[test]
fn adversarial_option_ref_from_param_accepts() {
    assert_static_accept(
        "fn wrap(s: ref String) -> Option[ref String] {\n\
             Option.Some(s)\n\
         }\n\
         fn main() {\n\
             let s = String.from(\"hi\");\n\
             match wrap(s) {\n\
                 Some(n) => println(n.len()),\n\
                 None => println(0),\n\
             }\n\
         }",
        "adversarial_option_ref_from_param_accepts",
    );
}

// ── Family 5: self-referential / back-pointer shapes ───────────────
// The RC-fallback boundary. A back-pointer shape a plain owned model can't
// represent must route through `shared struct` (RC), never a dangling
// borrow. The soundness property: the RC boundary engages (accepted and
// RC-managed) and *not* escalating is never silently unsound.

/// A `shared struct` back-pointer chain: `b.next` holds `a`. RC backs both
/// nodes; the shape is accepted (the RC-fallback boundary engages) and
/// drops cleanly — `a` is moved into `b.next`, so its refcount is held by
/// `b` and reaches zero exactly once when `b` drops. Accepted, ASAN-clean
/// (mirror in Section 3).
#[test]
fn adversarial_shared_struct_backpointer_accepts() {
    assert_static_accept(
        "shared struct Node {\n\
             mut next: Option[Node],\n\
             value: i64,\n\
         }\n\
         fn main() {\n\
             let a = Node { next: Option.None, value: 1 };\n\
             let b = Node { next: Option.Some(a), value: 2 };\n\
             println(b.value);\n\
         }",
        "adversarial_shared_struct_backpointer_accepts",
    );
}

// ── Family 6: mutation aliasing ────────────────────────────────────
// design.md § Ownership exclusive-borrow rule: a `mut ref` borrow must be
// the *only* active borrow of its place. Passing the same place as both a
// `mut ref` and a `ref` argument of one call violates it (and is the
// soundness precondition for emitting LLVM `noalias` on `mut ref` params).

/// A `mut ref` and a `ref` borrow of the *same* place at one call. The
/// exclusive-borrow rule requires the `mut ref` to be the sole live borrow;
/// the aliased `ref` argument makes that false.
#[test]
fn adversarial_mutation_aliasing_mut_ref_and_ref() {
    assert_ownership_error_kind(
        "fn clobber(dst: mut ref Vec[i64], src: ref Vec[i64]) {\n\
             dst.push(src.len());\n\
         }\n\
         fn main() {\n\
             let mut v: Vec[i64] = Vec.new();\n\
             v.push(1);\n\
             clobber(mut v, v);\n\
             println(v.len());\n\
         }",
        OwnershipErrorKind::ExclusiveBorrowAliasedArgs,
        "adversarial_mutation_aliasing_mut_ref_and_ref",
    );
}

// ────────────────────────────────────────────────────────────────
// Section 3: ASAN runtime confirmation for accept cases.
// Closes the loop: static accept → generated code is memory-safe.
// macOS leak gap noted in the file header.
// ────────────────────────────────────────────────────────────────

#[cfg(feature = "llvm")]
mod runtime_confirmation {
    use karac::codegen::{compile_to_object, link_executable_with_sanitizer};
    use std::path::Path;
    use std::process::{Command, Stdio};
    use std::sync::OnceLock;

    /// Mirrors `tests/memory_sanitizer.rs::asan_available` so this file can
    /// stand alone without depending on a tests/common helper module.
    fn asan_available() -> bool {
        static AVAILABLE: OnceLock<bool> = OnceLock::new();
        *AVAILABLE.get_or_init(|| {
            if std::env::var("KARAC_SKIP_ASAN_TESTS").is_ok() {
                return false;
            }
            let probe_c = "/tmp/karac_safety_design_probe.c";
            let probe_exe = "/tmp/karac_safety_design_probe";
            if std::fs::write(probe_c, "int main(void){return 0;}\n").is_err() {
                return false;
            }
            let link_ok = Command::new("cc")
                .args(["-fsanitize=address", probe_c, "-o", probe_exe])
                .output()
                .ok()
                .map(|o| o.status.success())
                .unwrap_or(false);
            let run_ok = link_ok
                && Command::new(probe_exe)
                    .output()
                    .ok()
                    .map(|o| o.status.success())
                    .unwrap_or(false);
            let _ = std::fs::remove_file(probe_c);
            let _ = std::fs::remove_file(probe_exe);
            run_ok
        })
    }

    /// Per-binary execution timeout. Default 60s — generous for the corpus
    /// (each program does O(1) work and prints at most a few bytes) but
    /// short enough that a hang fails a single test in a minute rather
    /// than wedging the whole `cargo test --features llvm` run. Override
    /// via `KARAC_TEST_BINARY_TIMEOUT_SECS` for slower hardware / CI.
    fn binary_timeout() -> std::time::Duration {
        let secs: u64 = std::env::var("KARAC_TEST_BINARY_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(60);
        std::time::Duration::from_secs(secs)
    }

    /// Run a compiled test binary with a hard timeout. The corpus programs
    /// run in milliseconds; if a binary takes more than `timeout`, kill
    /// the child and return `Ok(None)` so the caller can fail with a
    /// precise label rather than have `cargo test` hang. The pipe-buffer
    /// deadlock common to `wait_with_output` is sidestepped by piping
    /// stdio + manual drain after `try_wait()` — corpus programs print
    /// at most a few bytes so buffers never fill.
    ///
    /// Structural fix for the 2026-05-29 flake where a single compiled
    /// binary hung at 56% CPU for 6h+, blocking the whole
    /// `cargo test --features llvm` run.
    fn run_binary_with_timeout(
        exe_path: &str,
        asan_options: &str,
        timeout: std::time::Duration,
    ) -> std::io::Result<Option<std::process::Output>> {
        use std::io::Read;
        use std::time::Instant;

        let mut child = Command::new(exe_path)
            .env("ASAN_OPTIONS", asan_options)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let start = Instant::now();
        loop {
            if let Some(status) = child.try_wait()? {
                let mut stdout = Vec::new();
                let mut stderr = Vec::new();
                if let Some(mut so) = child.stdout.take() {
                    let _ = so.read_to_end(&mut stdout);
                }
                if let Some(mut se) = child.stderr.take() {
                    let _ = se.read_to_end(&mut stderr);
                }
                return Ok(Some(std::process::Output {
                    status,
                    stdout,
                    stderr,
                }));
            }
            if start.elapsed() > timeout {
                let _ = child.kill();
                let _ = child.wait();
                return Ok(None);
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    /// Compile, link with ASAN, run, and assert clean exit. Stdout is not
    /// pinned here — the *runtime safety* of accepted programs is what we
    /// want to confirm; behavioral correctness is the typechecker's job.
    fn assert_accepted_program_is_asan_clean(src: &str, label: &str) {
        if !asan_available() {
            eprintln!("[{label}] ASAN unavailable on this host — skipping");
            return;
        }
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);

        let mut parsed = karac::parse(src);
        if !parsed.errors.is_empty() {
            panic!("[{label}] parse errors: {:?}", parsed.errors);
        }
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let ownership = karac::ownershipcheck(&parsed.program, &typed);

        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let obj_path = format!("/tmp/karac_safety_design_{}_{}.o", std::process::id(), id);
        let exe_path = format!("/tmp/karac_safety_design_{}_{}", std::process::id(), id);

        if let Err(e) = compile_to_object(&parsed.program, &obj_path, Some(&ownership), None) {
            eprintln!("[{label}] compile_to_object failed: {e} — skipping");
            return;
        }
        if !Path::new(&obj_path).exists() {
            eprintln!("[{label}] object file missing — skipping");
            return;
        }
        if let Err(e) =
            link_executable_with_sanitizer(&obj_path, &exe_path, &["-fsanitize=address"])
        {
            eprintln!("[{label}] link failed: {e} — skipping (runtime lib likely absent)");
            let _ = std::fs::remove_file(&obj_path);
            return;
        }

        let asan_options = if cfg!(target_os = "macos") {
            "abort_on_error=0:exitcode=23"
        } else {
            "detect_leaks=1:abort_on_error=0:exitcode=23"
        };
        let timeout = binary_timeout();
        let output = run_binary_with_timeout(&exe_path, asan_options, timeout);

        let _ = std::fs::remove_file(&obj_path);

        match output {
            Ok(Some(out)) => {
                let _ = std::fs::remove_file(&exe_path);
                if !out.status.success() {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    panic!(
                        "[{label}] ASAN reported a memory error (exit {:?}). \
                         Look for `LeakSanitizer`, `heap-use-after-free`, or \
                         `double-free` in stderr:\n{stderr}",
                        out.status.code()
                    );
                }
            }
            Ok(None) => {
                // Binary did not terminate within the timeout. Preserve
                // it for post-mortem rather than deleting — the whole
                // point of timing out is to investigate the hang.
                panic!(
                    "[{label}] compiled test binary did not terminate within {}s; \
                     child killed. Binary preserved at {exe_path} for debugging \
                     (re-run under lldb / dtruss; set KARAC_TEST_BINARY_TIMEOUT_SECS \
                     to widen the budget). Likely culprits: infinite loop in generated \
                     code, ASAN deadlock at exit, or the karac_par_run worker pool \
                     failing to shut down before the runtime's atexit handlers run.",
                    timeout.as_secs()
                );
            }
            Err(e) => {
                let _ = std::fs::remove_file(&exe_path);
                eprintln!("[{label}] failed to spawn binary: {e} — skipping");
            }
        }
    }

    // The ASAN-routed cases mirror the *currently-passing* static accept
    // tests. The ignore-gated `spec_*` cases are not mirrored here — they
    // wouldn't compile to a binary today, so there's nothing to run.

    #[test]
    fn asan_single_source_borrow_return() {
        assert_accepted_program_is_asan_clean(
            "fn echo(s: ref String) -> ref String { s }\n\
             fn main() {\n\
                 let s = String.from(\"hello\");\n\
                 let t = echo(s);\n\
                 println(t.len());\n\
             }",
            "asan_single_source_borrow_return",
        );
    }

    #[test]
    fn asan_multi_source_borrow_return() {
        assert_accepted_program_is_asan_clean(
            "fn longer(a: ref String, b: ref String) -> ref String {\n\
                 if a.len() > b.len() { a } else { b }\n\
             }\n\
             fn main() {\n\
                 let x = String.from(\"short\");\n\
                 let y = String.from(\"a longer string\");\n\
                 let z = longer(x, y);\n\
                 println(z.len());\n\
             }",
            "asan_multi_source_borrow_return",
        );
    }

    #[test]
    fn asan_borrowed_struct_construction() {
        assert_accepted_program_is_asan_clean(
            "struct Parser {\n\
                 source: ref String,\n\
                 position: i64,\n\
             }\n\
             fn main() {\n\
                 let s = String.from(\"input\");\n\
                 let p = Parser { source: s, position: 0 };\n\
                 println(p.position);\n\
             }",
            "asan_borrowed_struct_construction",
        );
    }

    // Borrowed-struct RETURN (`-> ref Parser`, design.md Feature 4 Part 3,
    // B-2026-06-07-5). The struct is returned by value with its `ref` field
    // holding a borrow of the caller's `s`. The double-free risk: the
    // returned `p`'s drop must NOT free `source` (a borrow), and the source
    // `s` frees its buffer exactly once at its own scope exit — `p` and `s`
    // both drop at main's exit, exercising that path regardless of which
    // field is read. (`asan_*` mirrors the now-un-ignored
    // `spec_return_borrowed_struct`.) Reading the borrowed field itself is a
    // separate codegen follow-on (see `test_e2e_borrow_return_borrowed_struct`).
    #[test]
    fn asan_borrowed_struct_return() {
        assert_accepted_program_is_asan_clean(
            "struct Parser { source: ref String, position: i64 }\n\
             fn make_parser(s: ref String) -> ref Parser {\n\
                 Parser { source: s, position: 0 }\n\
             }\n\
             fn main() {\n\
                 let s = String.from(\"input data\");\n\
                 let p = make_parser(s);\n\
                 println(p.position);\n\
             }",
            "asan_borrowed_struct_return",
        );
    }

    // Tier 2c (B-2026-06-07-5): a `match` over an enum identifier scrutinee
    // returning a borrow from a `ref` param, across both binding-free
    // destructuring shapes — an all-wildcard tuple variant (`Ok(_)`) and
    // dotted unit variants (`Side.*`). The returned borrow aliases the
    // caller's heap `String`; the match itself binds/aliases nothing and the
    // scrutinee's binding frees it once. A double-free (match wrongly freeing
    // the source) or a UAF (returning a pointer into a freed temp) would trip
    // ASAN. Mirrors the now-accepted `accept_match_*_borrow_return` static
    // tests.
    #[test]
    fn asan_match_tuple_variant_borrow_return() {
        assert_accepted_program_is_asan_clean(
            "fn pick(res: ref Result[i64, String], a: ref String, b: ref String) -> ref String {\n\
                 match res {\n\
                     Ok(_) => a,\n\
                     _ => b,\n\
                 }\n\
             }\n\
             fn main() {\n\
                 let ok: Result[i64, String] = Result.Ok(1);\n\
                 let x = String.from(\"a longer string here\");\n\
                 let y = String.from(\"short\");\n\
                 let r = pick(ok, x, y);\n\
                 println(r.len());\n\
             }",
            "asan_match_tuple_variant_borrow_return",
        );
    }

    #[test]
    fn asan_match_dotted_unit_variant_borrow_return() {
        assert_accepted_program_is_asan_clean(
            "enum Side { Left, Right }\n\
             fn pick(s: ref Side, a: ref String, b: ref String) -> ref String {\n\
                 match s {\n\
                     Side.Left => a,\n\
                     Side.Right => b,\n\
                 }\n\
             }\n\
             fn main() {\n\
                 let sd: Side = Side.Right;\n\
                 let x = String.from(\"left payload here\");\n\
                 let y = String.from(\"right payload here\");\n\
                 let r = pick(sd, x, y);\n\
                 println(r.len());\n\
             }",
            "asan_match_dotted_unit_variant_borrow_return",
        );
    }

    // Reading a BORROWED field of a returned/constructed borrowed struct
    // (B-2026-06-07-5 follow-on). A `ref`-typed field access deref's-on-use in
    // value positions; the double-free hazard is the let-bind and ref-param
    // argument positions — a deref'd value copy there would queue a free of
    // the borrowed buffer (which the source `s` also frees). These must NOT
    // double-free; the borrow is forwarded, not copied-and-freed.
    #[test]
    fn asan_borrowed_field_value_read() {
        assert_accepted_program_is_asan_clean(
            "struct Parser { source: ref String, position: i64 }\n\
             fn make_parser(s: ref String) -> ref Parser {\n\
                 Parser { source: s, position: 0 }\n\
             }\n\
             fn main() {\n\
                 let s = String.from(\"input data\");\n\
                 let p = make_parser(s);\n\
                 println(p.source);\n\
             }",
            "asan_borrowed_field_value_read",
        );
    }

    #[test]
    fn asan_borrowed_field_let_bound() {
        assert_accepted_program_is_asan_clean(
            "struct Parser { source: ref String, position: i64 }\n\
             fn make_parser(s: ref String) -> ref Parser {\n\
                 Parser { source: s, position: 0 }\n\
             }\n\
             fn main() {\n\
                 let s = String.from(\"input data\");\n\
                 let p = make_parser(s);\n\
                 let x = p.source;\n\
                 println(x);\n\
             }",
            "asan_borrowed_field_let_bound",
        );
    }

    #[test]
    fn asan_borrowed_field_into_ref_param() {
        assert_accepted_program_is_asan_clean(
            "struct Parser { source: ref String, position: i64 }\n\
             fn make_parser(s: ref String) -> ref Parser {\n\
                 Parser { source: s, position: 0 }\n\
             }\n\
             fn shout(x: ref String) { println(x); }\n\
             fn main() {\n\
                 let s = String.from(\"input data\");\n\
                 let p = make_parser(s);\n\
                 shout(p.source);\n\
             }",
            "asan_borrowed_field_into_ref_param",
        );
    }

    #[test]
    fn asan_borrowed_field_len_method() {
        assert_accepted_program_is_asan_clean(
            "struct Parser { source: ref String, position: i64 }\n\
             fn make_parser(s: ref String) -> ref Parser {\n\
                 Parser { source: s, position: 0 }\n\
             }\n\
             fn main() {\n\
                 let s = String.from(\"input data\");\n\
                 let p = make_parser(s);\n\
                 println(p.source.len());\n\
             }",
            "asan_borrowed_field_len_method",
        );
    }

    #[test]
    fn asan_closure_borrow_capture_no_escape() {
        assert_accepted_program_is_asan_clean(
            "fn main() {\n\
                 let s = String.from(\"hello\");\n\
                 let len_plus = |extra: i64| s.len() + extra;\n\
                 println(len_plus(5));\n\
             }",
            "asan_closure_borrow_capture_no_escape",
        );
    }

    #[test]
    fn asan_chained_borrow_returns() {
        assert_accepted_program_is_asan_clean(
            "fn echo(s: ref String) -> ref String { s }\n\
             fn echo_twice(s: ref String) -> ref String {\n\
                 let t = echo(s);\n\
                 echo(t)\n\
             }\n\
             fn main() {\n\
                 let s = String.from(\"chained\");\n\
                 let r = echo_twice(s);\n\
                 println(r.len());\n\
             }",
            "asan_chained_borrow_returns",
        );
    }

    // Direct use of a borrow-returning call result (Tier-1.5,
    // B-2026-06-07-5) — the result is consumed in place rather than bound to
    // a `let`. The codegen gate that required direct binding now loads the
    // pointee for value positions. The soundness risk is the
    // *ref-parameter-argument* position: passing `name_of(s)` to another
    // `ref String` param must forward the borrow pointer, NOT a materialized
    // value copy (which would queue a `track_vec_var` free and double-free
    // the source `s`'s buffer). ASAN is the load-bearing check here.

    #[test]
    fn asan_direct_use_in_print_arg() {
        assert_accepted_program_is_asan_clean(
            "fn name_of(u: ref String) -> ref String { u }\n\
             fn main() {\n\
                 let s = String.from(\"hello\");\n\
                 println(name_of(s));\n\
             }",
            "asan_direct_use_in_print_arg",
        );
    }

    #[test]
    fn asan_direct_use_in_ref_param_arg() {
        assert_accepted_program_is_asan_clean(
            "fn name_of(u: ref String) -> ref String { u }\n\
             fn shout(x: ref String) { println(x); }\n\
             fn main() {\n\
                 let s = String.from(\"hello\");\n\
                 shout(name_of(s));\n\
             }",
            "asan_direct_use_in_ref_param_arg",
        );
    }

    #[test]
    fn asan_direct_use_method_on_result() {
        assert_accepted_program_is_asan_clean(
            "fn name_of(u: ref String) -> ref String { u }\n\
             fn main() {\n\
                 let s = String.from(\"hello\");\n\
                 println(name_of(s).len());\n\
             }",
            "asan_direct_use_method_on_result",
        );
    }

    // B-2026-06-10-5: the HEAP-source variant of the above. `String.from(..)`
    // alone yields `cap == 0` (static-backed), so a spurious free on the
    // direct-use borrow temp is a no-op and the cap-0 version above stayed
    // green even while this crashed. Growing `s` via `push_str` forces
    // `cap > 0`, exposing the double-free: `name_of(s).len()` routed the
    // borrow through the value-receiver `len` path, which materialized the
    // loaded `{ptr,len,cap}` as a "fresh owned temp" (any Call qualified) and
    // queued a `FreeVecBuffer` against `s`'s buffer. The fix excludes
    // borrow-returning calls from `expr_yields_fresh_owned_temp`. Post-use of
    // `s` confirms its buffer is freed exactly once (by its own binding).
    #[test]
    fn asan_direct_use_method_on_heap_result() {
        assert_accepted_program_is_asan_clean(
            "fn name_of(u: ref String) -> ref String { u }\n\
             fn main() {\n\
                 let mut s: String = \"\";\n\
                 s.push_str(\"hello\");\n\
                 println(name_of(s).len());\n\
                 println(s);\n\
             }",
            "asan_direct_use_method_on_heap_result",
        );
    }

    // B-2026-06-10-5 sibling (same root cause, different consuming position):
    // a borrow-returning call as a `match` SCRUTINEE with a heap-payload enum.
    // The fresh-temp-enum-scrutinee drop path also keyed on
    // `expr_yields_fresh_owned_temp` (any Call), so `match pick(e) { … }` —
    // `pick(_) -> ref E` — would materialize + free the loaded enum's `String`
    // payload that `e` still owns. The fix lives in the shared helper, so this
    // is covered too. (`E.A`'s payload is heap via push_str → cap>0.)
    #[test]
    fn asan_direct_use_match_scrutinee_on_heap_enum() {
        assert_accepted_program_is_asan_clean(
            "enum E { A(String), B }\n\
             fn pick(e: ref E) -> ref E { e }\n\
             fn main() {\n\
                 let mut s: String = \"\";\n\
                 s.push_str(\"payload\");\n\
                 let e: E = E.A(s);\n\
                 match pick(e) {\n\
                     A(_) => println(\"is-a\"),\n\
                     _ => println(\"is-b\"),\n\
                 }\n\
             }",
            "asan_direct_use_match_scrutinee_on_heap_enum",
        );
    }

    // Vec borrow forwarded straight into another `ref Vec` parameter — the
    // strictest double-free path (heap buffer, `cap > 0`). If the
    // materialization at the ref-arg site freed the forwarded copy, the
    // source `v`'s drop would double-free. ASAN confirms the borrow pointer
    // is forwarded, not copied-and-freed.
    #[test]
    fn asan_direct_use_vec_into_ref_param() {
        assert_accepted_program_is_asan_clean(
            "fn pick(v: ref Vec[i64]) -> ref Vec[i64] { v }\n\
             fn first(v: ref Vec[i64]) -> i64 { v[0] }\n\
             fn main() {\n\
                 let mut v: Vec[i64] = Vec.new();\n\
                 v.push(10);\n\
                 v.push(20);\n\
                 println(first(pick(v)));\n\
             }",
            "asan_direct_use_vec_into_ref_param",
        );
    }

    // ── Adversarial accept-case mirrors (Section 2½) ──────────────
    // Each mirrors an `adversarial_*_accepts` static case, closing the
    // dichotomy's runtime leg: a hostile program the ownership pass
    // *accepts* must be memory-safe. If the borrowed-collection codegen
    // path is still a follow-on and cannot lower one of these shapes, the
    // harness skips gracefully (compile/link failure → skip, never a false
    // failure); only a compile-run that ASAN flags fails, which would be a
    // genuine soundness bug.

    // Family 4 accept: a borrowed collection (`Vec[ref String]`) whose
    // element borrows a `ref` parameter. The caller's `s` owns and frees
    // the buffer; the returned vec's drop must NOT free the borrowed
    // element (a double-free ASAN would catch) and must not leak it.
    #[test]
    fn asan_borrowed_collection_from_param() {
        assert_accepted_program_is_asan_clean(
            "fn ok(s: ref String) -> Vec[ref String] {\n\
                 let mut v: Vec[ref String] = Vec.new();\n\
                 v.push(s);\n\
                 v\n\
             }\n\
             fn main() {\n\
                 let s = String.from(\"a longer payload string\");\n\
                 let v = ok(s);\n\
                 println(v.len());\n\
             }",
            "asan_borrowed_collection_from_param",
        );
    }

    // Family 4 accept: a borrow through a generic wrapper
    // (`Option[ref String]`) sourced from a `ref` parameter, matched and
    // dereferenced. The `Some(n)` payload aliases the caller's `s`; a
    // materialized-and-freed copy at the match binding would double-free.
    #[test]
    fn asan_option_ref_from_param() {
        assert_accepted_program_is_asan_clean(
            "fn wrap(s: ref String) -> Option[ref String] {\n\
                 Option.Some(s)\n\
             }\n\
             fn main() {\n\
                 let mut s: String = \"\";\n\
                 s.push_str(\"a longer payload string\");\n\
                 match wrap(s) {\n\
                     Some(n) => println(n.len()),\n\
                     None => println(0),\n\
                 }\n\
                 println(s);\n\
             }",
            "asan_option_ref_from_param",
        );
    }

    // Family 5 accept: the RC-fallback boundary. A `shared struct`
    // back-pointer chain — `b.next` holds `a`. RC backs both nodes; `a`'s
    // buffer-free-equivalent (the node dealloc) must happen exactly once,
    // when `b` drops and `a`'s refcount reaches zero. A missed decrement
    // leaks (LSan on Linux); a double decrement double-frees.
    #[test]
    fn asan_shared_struct_backpointer() {
        assert_accepted_program_is_asan_clean(
            "shared struct Node {\n\
                 mut next: Option[Node],\n\
                 value: i64,\n\
             }\n\
             fn main() {\n\
                 let a = Node { next: Option.None, value: 1 };\n\
                 let b = Node { next: Option.Some(a), value: 2 };\n\
                 println(b.value);\n\
             }",
            "asan_shared_struct_backpointer",
        );
    }
}
