//! control flow, loops, bindings, assignment, scopes -- fixtures for `tests/codegen.rs`.
//!
//! Split out of `tests/codegen.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test codegen` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test codegen control::
//!
//! New fixtures about control flow, loops, bindings, assignment, scopes belong in this file.

use super::*;

/// The codegen half of the shadowed-namespace refusal (B-2026-08-02-13
/// class). Its twin lives in `tests/interpreter.rs`; both must refuse the
/// same program, or `karac run` and `karac build` disagree about a digest
/// whose entire contract is that nothing disagrees about it.
#[test]
fn stable_hash_refuses_a_user_shadowed_namespace() {
    let err = ir_result(
        "struct StableHash { }\n\
             impl StableHash {\n\
                 fn siphash24(bytes: Slice[u8], k0: u64, k1: u64) -> u64 { 42 }\n\
             }\n\
             fn main() {\n\
                 let v: Vec[u8] = [1u8, 2u8, 3u8];\n\
                 println(StableHash.siphash24(v, 0u64, 0u64));\n\
             }\n",
    )
    .expect_err("a user struct shadowing `StableHash` must be refused, not silently hijacked");
    assert!(
        err.contains("StableHash") && err.contains("Rename"),
        "expected the rename diagnostic, got: {err}"
    );
}

#[test]
fn e2e_enumerate_loop_body_and_outer_mutations_execute() {
    // B-2026-07-08-5: `for (i, v) in xs.iter().enumerate()` fell through
    // `compile_for`'s dispatch to the silent skip-body arm, so the loop body
    // never ran under codegen — every outer-variable mutation was lost
    // (`sum` stayed 0) while the interpreter was correct. This was a
    // silent-wrong-output divergence surfaced by LLJIT Slice 6b routing
    // `karac run` onto codegen (two_sum printed "No solution"). Guard the
    // full pattern: an accumulator mutated per element, the enumerate index
    // used in the body, and the canonical get-then-insert-in-loop Map idiom.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let vals = [10, 20, 30];\n\
                 let mut sum = 0i64;\n\
                 for (i, v) in vals.iter().enumerate() { sum = sum + v + i; }\n\
                 println(sum);\n\
                 let nums = [2, 7, 11, 15];\n\
                 let mut seen: Map[i64, i64] = Map.new();\n\
                 let mut found = -1i64;\n\
                 for (i, num) in nums.iter().enumerate() {\n\
                     match seen.get(9 - num) {\n\
                         Some(j) => { found = j; }\n\
                         None => { let _ = seen.insert(num, i); }\n\
                     }\n\
                 }\n\
                 println(found);\n\
                 println(seen.len());\n\
             }",
    ) {
        // sum = (10+0)+(20+1)+(30+2) = 63. found = 0: num=2 inserts (2->0),
        // num=7 looks up 9-7=2 -> Some(0). Inserts: num=2 (miss 7), num=11
        // (miss -2), num=15 (miss -6) = 3 keys; num=7 hit the Some arm and
        // did not insert. So len = 3. (Matches interp and AOT.)
        assert_eq!(out, "63\n0\n3\n");
    }
    // Field-access receiver: `for (i, x) in obj.field.iter().enumerate()`
    // routes through the `.iter()` field-iter path, which must ALSO bind the
    // enumerate index (extended `for_receiver_is_indexable`). Was `sum=0`
    // (body skipped) before the field-access extension.
    if let Some(out) = run_program(
        "struct Bag { items: Vec[i64] }\n\
             fn main() {\n\
                 let b = Bag { items: [5, 6, 7] };\n\
                 let mut sum = 0i64;\n\
                 for (i, x) in b.items.iter().enumerate() { sum = sum + x + i; }\n\
                 println(sum);\n\
             }",
    ) {
        // (5+0)+(6+1)+(7+2) = 21
        assert_eq!(out, "21\n");
    }
}

// ── Variables and let bindings ───────────────────────────────

#[test]
fn test_ir_let_binding() {
    let ir = ir_for("fn double(x: i64) -> i64 { let y = x * 2; y }");
    assert!(ir.contains("smul.with.overflow"));
    assert!(ir.contains("alloca"));
}

#[test]
fn test_ir_let_mut_reassign() {
    let ir = ir_for(
        r#"
fn count() -> i64 {
    let mut n = 0;
    n = n + 1;
    n = n + 1;
    n
}
"#,
    );
    assert!(ir.contains("store"));
    assert!(ir.contains("load"));
}

#[test]
fn test_ir_compound_assign() {
    let ir = ir_for(
        r#"
fn accumulate(limit: i64) -> i64 {
    let mut sum = 0;
    let mut i = 1;
    while i <= limit {
        sum += i;
        i += 1;
    }
    sum
}
"#,
    );
    assert!(
        ir.contains("sadd.with.overflow"),
        "should contain checked integer addition for +="
    );
    assert!(
        ir.contains("while.cond"),
        "should contain while condition block"
    );
}

// ── Control flow ─────────────────────────────────────────────

#[test]
fn test_ir_if_else() {
    let ir = ir_for("fn abs(x: i64) -> i64 { if x < 0 { 0 - x } else { x } }");
    assert!(ir.contains("br i1"), "should contain conditional branch");
    assert!(ir.contains("phi"), "if-else result should use phi node");
}

#[test]
fn test_ir_if_no_else() {
    // `mut` on parameters is not Kāra syntax (modes are inferred).
    // Declare x as i64 and reassign via let mut inside the body.
    let ir = ir_for(
        r#"
fn clamp_positive(x: i64) -> i64 {
    let mut v = x;
    if v < 0 { v = 0; }
    v
}
"#,
    );
    assert!(ir.contains("br i1"));
}

#[test]
fn test_ir_while_loop() {
    let ir = ir_for(
        r#"
fn sum_to(n: i64) -> i64 {
    let mut acc = 0;
    let mut i = 1;
    while i <= n {
        acc = acc + i;
        i = i + 1;
    }
    acc
}
"#,
    );
    assert!(ir.contains("while.cond"));
    assert!(ir.contains("while.body"));
    assert!(ir.contains("while.exit"));
}

#[test]
fn test_ir_loop_break() {
    let ir = ir_for(
        r#"
fn find_first_positive(x: i64) -> i64 {
    let mut i = 0;
    loop {
        i = i + 1;
        if i > x { break; }
    }
    i
}
"#,
    );
    assert!(ir.contains("loop.body"));
    assert!(ir.contains("loop.exit"));
}

/// B-2026-08-31-7 (the hazard the fix above had to clear first) — A `let`
/// STARTS A NEW GENERATION OF ITS NAMES, so a stale param-VIEW mark must not
/// survive it.
///
/// `param_view_locals` (codegen) and `owned_param_names_stack`'s top frame
/// (interpreter) were both write-only: every site inserted, none removed, and
/// each was cleared only at function entry. A name marked a view by an earlier
/// construct therefore stayed one across a later, unrelated `let` of the SAME
/// NAME — and view-ness means "someone else runs the body", so the fresh
/// value's body ran NOWHERE.
///
/// FOUND ON THE ENUM PATH, which predates -31-7 entirely: `enum` below printed
/// `arm2 fresh98 dR2` where `dR98` is due as well, on `--interp` and both
/// compiled backends alike. Both were wrong and AGREED, which is exactly why
/// it stayed invisible — no run-vs-build signal to trip over.
///
/// It surfaced only because -31-7's tuple marking was probed for over-reach:
/// the `tuple` case below regressed the moment bare-tuple elements were marked,
/// and the enum control proved the hazard belonged to the mechanism rather than
/// to the new caller. Repairing codegen alone would have converted an
/// agreed-wrong answer into a fresh divergence, so both sides land together.
///
/// `selfrebind` is the exemption that keeps the clear honest: `let r = r;`
/// reads the very generation being cleared and must still inherit its
/// view-ness — one body, not two.
///
/// Twin of `tests/interpreter.rs`'s
/// `test_a_let_clears_a_stale_param_view_mark`, pinned to the same string.
#[test]
fn e2e_a_let_clears_a_stale_param_view_mark() {
    let Some(out) = run_program(
        r#"struct R { id: i64 }
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
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
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

/// B-2026-09-04-36 — a fresh-temp method RECEIVER dies AS THE CALL
/// RETURNS, not at the enclosing statement's `;`.
///
/// `println(f"ref[{mk(1).peek()}]")` printed the body AFTER the value on
/// jit/aot against `--interp`'s before it. The statement spelling
/// (`let v = mk(3).take()`) agreed on all four surfaces, which is exactly
/// why this hid: there the `;` follows the call return with nothing in
/// between, so the two rows coincide.
///
/// WHY THE CALL RETURN IS THE ANSWER. B-2026-08-29-55 moved the three
/// ARGUMENT registrars onto a per-call window and excluded the receiver on
/// the ground that it "has its OWN row in the position table with a
/// different end". It does not — design.md § Temporary Lifetime Rules has
/// no row for a method receiver. Two readings both land on the argument
/// row: `self` IS a parameter, so the receiver is argument zero; and the
/// canonical rule ends a temporary "at the first program point where the
/// surrounding context's evaluation has produced a value that no longer
/// references the temporary", which for `mk(1).peek() -> i64` is the call
/// return. The statement drain applied the neighbouring longer row, and the
/// composition-with-NLL paragraph forbids that direction outright.
///
/// THE TWO-RECEIVER CELL IS THE ONE THAT SHOWS IT IS NOT MERELY LATE.
/// `mk(4).take() + mk(5).take()` drained BOTH receivers at the `;`, and the
/// statement frame pops LIFO, so the compiled backends ran `dR5 dR4` —
/// the second receiver's body before the first's — against the
/// interpreter's `dR4 dR5`. Each receiver now dies inside its own call
/// window, so the order follows program order. This is the held-too-long
/// footgun the position table exists to close, one row over from the
/// argument case that prompted B-2026-08-29-55.
///
/// Both self-modes are pinned because they reach the placement by
/// different routes: `ref self` always had a body here, and `take(self)`
/// only acquired one in B-2026-09-04-30, inheriting the same split.
/// SCRUTINEE temps are deliberately NOT part of this move and were measured
/// not to split — they genuinely do have their own position-table rows.
#[test]
fn e2e_fresh_receiver_temp_dies_at_the_call_return() {
    let Some(out) = run_program(
        "struct R { id: i64, t: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             fn mk(n: i64) -> R { return R { id: n, t: f\"tag{n}\" } }\n\
             impl R {\n\
             \x20   fn peek(ref self) -> i64 { return self.id * 2 }\n\
             \x20   fn take(self) -> i64 { return self.id * 3 }\n\
             }\n\
             fn main() {\n\
             \x20   println(f\"ref[{mk(1).peek()}]\");\n\
             \x20   println(f\"own[{mk(2).take()}]\");\n\
             \x20   let v = mk(3).take();\n\
             \x20   println(f\"stmt[{v}]\");\n\
             \x20   let two = mk(4).take() + mk(5).take();\n\
             \x20   println(f\"two[{two}]\");\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(
        out,
        "dR1\nref[2]\ndR2\nown[6]\ndR3\nstmt[9]\ndR4\ndR5\ntwo[27]\nend\n"
    );
}

/// The caller-side fresh-temp ARGUMENT `Drop` walk fires at statement end,
/// matching the interpreter (B-2026-08-28-19).
///
/// The interpreter runs that walk as the call returns; codegen registered it
/// as a cleanup ACTION on the caller's scope frame, so it drained at scope
/// exit. Counts always agreed — which is why every assertion in this family,
/// all of them count-based, was satisfied — and the ORDER did not: a tuple
/// argument whose element dies inside the callee printed its body after every
/// later statement in the caller.
///
/// The mechanism was a missing NAME rather than missing machinery.
/// `drain_statement_temp_user_drops` already retires argument temporaries at
/// statement end, and `track_discarded_tuple_elem_bodies`' own comment
/// claimed to register "under a statement-temporary name that
/// `drain_statement_temp_user_drops` retires at statement end" — the name was
/// simply absent from that list.
///
/// THE NAME IS THE ARGUMENT SITE'S ALONE, and that is the constraint this
/// fixture exists to pin. The same helper also serves a `let` whose value
/// stays live past the statement; adding the shared name to the drain ran a
/// body while its owner was still readable, which
/// `nested-no-destructure-control` in the sibling fixture caught. So the
/// argument site got its own temporary name and only that one is retired
/// early.
///
/// `intervening-statement` is what makes this an ORDER test rather than a
/// count test: the body must land before the `println` that follows the call,
/// not merely somewhere in the program. `loop-body` pins per-iteration
/// placement, where scope-exit draining would have collapsed every
/// iteration's body to the end. The two escape rows are controls — nothing
/// fires early for a value that leaves the callee.
#[test]
fn e2e_caller_side_argument_bodies_fire_at_statement_end() {
    const H: &str = "struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id}\") } }\n";
    for (label, body, want) in [
        // The row's own repro, with a statement AFTER the call so the order
        // is observable.
        (
            "intervening-statement",
            "fn dies(p: (R, i64)) -> i64 { let (r, n) = p; return n; }\n\
                 fn main() { let x = dies((R { id: 41 }, 1)); println(\"after\");\n\
                 \x20            println(f\"{x}\"); }\n",
            "drop 41\nafter\n1\n",
        ),
        // One element escapes, one dies: the dying one fires at the call,
        // the escaping one at its binding's last use.
        (
            "one-escapes",
            "fn take2(p: (R, R)) -> R { let (a, b) = p; return a; }\n\
                 fn main() { let y = take2((R { id: 42 }, R { id: 52 }));\n\
                 \x20            println(\"after2\"); println(f\"{y.id}\"); }\n",
            "drop 52\nafter2\n42\ndrop 42\n",
        ),
        // Per-ITERATION placement — scope-exit draining collapsed these to
        // the end of the function.
        (
            "loop-body",
            "fn dies(p: (R, i64)) -> i64 { let (r, n) = p; return n; }\n\
                 fn main() { for i in 0..2 { let q = dies((R { id: 60 + i }, 1));\n\
                 \x20            println(f\"iter {q}\"); } }\n",
            "drop 60\niter 1\ndrop 61\niter 1\n",
        ),
        // CONTROL — the WHOLE param escapes, so the walk declines entirely
        // and nothing may fire early.
        (
            "whole-param-escapes",
            "fn whole(p: (R, i64)) -> (R, i64) { return p; }\n\
                 fn main() { let (z, n) = whole((R { id: 43 }, 1)); println(\"after3\");\n\
                 \x20            println(f\"{z.id}\"); }\n",
            "after3\n43\ndrop 43\n",
        ),
    ] {
        let prog = format!("{H}{body}");
        assert_eq!(run_program(&prog).as_deref(), Some(want), "{label}");
    }
}

/// B-2026-08-29-14 — a METHOD that hands its owned param back with
/// `return r;` rather than as a BLOCK TAIL stands its caller down, exactly
/// as the block-tail spelling does.
///
/// The caller's stand-down asks two predicates, and a `return`-only callee
/// was admitted by NEITHER: `fn_conditionally_returns_param_bare` declines
/// `return` statements outright, and `fn_always_returns_param` declined any
/// body with no tail expression, on the reasoning that such a body was
/// "left to the `return` channel" — a channel that does not accept it. So
/// the caller kept firing alongside the result binding: two `Drop` bodies
/// for one object on all three compiled backends, against one in the
/// interpreter and one for the BLOCK-TAIL spelling of the identical method,
/// which are the oracles here.
///
/// Two bodies, ONE object: the second body ran on the same struct, and the
/// heap payload was still freed exactly once, so the pre-fix defect was a
/// spurious body rather than a leak or a double free. That is what makes
/// removing one body safe. The memory twin
/// `asan_return_spelling_method_param_is_memory_balanced` pins it.
#[test]
fn e2e_return_spelling_method_returned_param_body_runs_once() {
    const DROPPER: &str = "struct R { id: i64, tag: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id}\"); } }\n\
             struct T1 { n: i64 }\n";
    for (label, body, want) in [
        // The row's program. Pre-fix `drop 32` / `32` / `drop 32` on every
        // compiled backend — two bodies for one object.
        (
            "method-return-spelling",
            "impl T1 { fn take(ref self, r: R) -> R { return r; } }\n\
                 fn main() { let t = T1 { n: 1 }; \
                 let b = t.take(R { id: 32, tag: f\"h\" }); println(f\"{b.id}\"); }\n",
            "32\ndrop 32\n",
        ),
        // CONTROL — the BLOCK-TAIL spelling of the identical method. Correct
        // before this fix (B-2026-08-28-70 landed it) and the oracle that
        // makes one body the right answer rather than a preference.
        (
            "method-block-tail-control",
            "impl T1 { fn keep(ref self, r: R) -> R { r } }\n\
                 fn main() { let t = T1 { n: 1 }; \
                 let b = t.keep(R { id: 32, tag: f\"h\" }); println(f\"{b.id}\"); }\n",
            "32\ndrop 32\n",
        ),
        // CONTROL — the FREE-FUNCTION twin of the `return` spelling,
        // unanimous before and after. The whole family's recurring shape is
        // a mechanism free functions reach and methods do not, so the
        // free-fn twin is the reference for every method row.
        (
            "free-fn-return-oracle",
            "fn idf(r: R) -> R { return r; }\n\
                 fn main() { let b = idf(R { id: 32, tag: f\"h\" }); println(f\"{b.id}\"); }\n",
            "32\ndrop 32\n",
        ),
        // An AGGREGATE `return`. `yields` counts a literal that moves the
        // param into itself, so this is admitted too — it went from an
        // AGREED DOUBLE on both backends to one body, matching its free-fn
        // oracle.
        (
            "aggregate-return",
            "struct Hh { r: R }\n\
                 impl T1 { fn wrap(ref self, r: R) -> Hh { return Hh { r: r }; } }\n\
                 fn main() { let t = T1 { n: 1 }; \
                 let h = t.wrap(R { id: 32, tag: f\"h\" }); println(f\"{h.r.id}\"); }\n",
            "32\ndrop 32\n",
        ),
        // The GENERIC method with the `return` spelling, which was a
        // run-vs-build divergence (interp 1 / compiled 2) and now agrees.
        (
            "generic-method-return-spelling",
            "impl T1 { fn gtake[X](ref self, r: R, x: X) -> R { return r; } }\n\
                 fn main() { let t = T1 { n: 1 }; \
                 let b = t.gtake(R { id: 32, tag: f\"h\" }, 5); println(f\"{b.id}\"); }\n",
            "32\ndrop 32\n",
        ),
        // BOUNDARY — a UNIT-returning method whose bare `return;` hands
        // nothing back. The param dies inside, so the caller must keep
        // firing. A function with no declared return type is excluded
        // outright for exactly this reason: there is nothing to hand the
        // param back THROUGH.
        (
            "unit-method-bare-return-keeps-caller-fire",
            "impl T1 { fn eat(ref self, r: R, k: bool) { if k { return; } \
                 println(f\"kept {r.id}\"); } }\n\
                 fn main() { let t = T1 { n: 1 }; \
                 t.eat(R { id: 31, tag: f\"g\" }, true); println(\"after\"); }\n",
            "drop 31\nafter\n",
        ),
        // BOUNDARY — a `let ... else { return .. }` hides the language's
        // most common bare `return` behind a statement kind the old walk
        // never visited. It is a return that does NOT yield the param, so
        // the caller must keep firing; a walk that missed it would stand the
        // caller down and LOSE this body.
        (
            "let-else-return-keeps-caller-fire",
            "impl T1 { fn take(ref self, r: R, o: Option[i64]) -> R {\n\
                 let Some(v) = o else { return R { id: 98, tag: f\"z\" }; };\n\
                 println(f\"v {v}\"); return r; } }\n\
                 fn main() { let t = T1 { n: 1 }; \
                 let b = t.take(R { id: 31, tag: f\"g\" }, None); println(f\"{b.id}\"); }\n",
            "drop 31\n98\ndrop 98\n",
        ),
        // BOUNDARY — a body with NO return that yields the param is not
        // "always returns the param" merely for lacking a counter-example.
        // Here the param dies inside and a different value is returned.
        (
            "returns-other-value-keeps-caller-fire",
            "impl T1 { fn swapout(ref self, r: R) -> R { println(f\"saw {r.id}\"); \
                 return R { id: 99, tag: f\"z\" }; } }\n\
                 fn main() { let t = T1 { n: 1 }; \
                 let b = t.swapout(R { id: 31, tag: f\"g\" }); println(f\"{b.id}\"); }\n",
            "saw 31\ndrop 31\n99\ndrop 99\n",
        ),
        // BOUNDARY, AND THE ONLY CASE HERE THAT THE NO-TAIL ARM'S OWN GUARDS
        // DECIDE. The three cases above are declined by `any_bad_return`
        // alone — each has a visible `return` that hands something else
        // back — so they pass even with the guards deleted. This one has NO
        // `return` at all and NO tail, which is the exact shape
        // `f.return_type.is_some() && any_good_return` exists to reject:
        // without both, "no counter-example" would be mistaken for "always
        // returns the param", the caller would stand down, and nothing
        // would own the body. Verified by deleting the guards: this case
        // drops to `saw 31` / `after` on every compiled backend while the
        // rest stay green.
        (
            "no-return-unit-method-keeps-caller-fire",
            "impl T1 { fn eat2(ref self, r: R) { println(f\"saw {r.id}\"); } }\n\
                 fn main() { let t = T1 { n: 1 }; \
                 t.eat2(R { id: 31, tag: f\"g\" }); println(\"after\"); }\n",
            "saw 31\ndrop 31\nafter\n",
        ),
    ] {
        assert_eq!(
            run_program(&format!("{DROPPER}{body}")).as_deref(),
            Some(want),
            "{label}"
        );
    }
}

/// B-2026-08-28-46 / -47 — an own-`impl Drop` enum held as a struct FIELD
/// or a tuple ELEMENT runs its body when the owner dies, even though it is
/// never destructured.
///
/// The two rows are one gap seen from two positions, and they failed
/// DIFFERENTLY, which is why both spellings are pinned here. The struct
/// field (`-46`) already fired on both compiled backends and was silent in
/// the interpreter. The tuple element (`-47`) was silent on ALL THREE — the
/// agree-on-zero shape no A/B gate can report, and the reason this is a
/// soundness fixture rather than a parity one.
///
/// `destructured-control` is what proves the machinery existed all along:
/// the same value, taken apart one statement later, ran the body on every
/// backend before the fix.
///
/// `payload-only-enum` is the DELIBERATE exclusion and the row that keeps
/// this fix honest. An enum with no own `Drop` but a Drop-bearing payload
/// stays silent on every backend, because codegen reaches an enum member
/// only through `type_runs_user_drop`, which answers for an enum via
/// `drop_method_keys` alone. Running it in the interpreter is a one-line
/// change and was measured to produce exactly the run-vs-build divergence
/// this row is meant to remove; the shape is real and filed separately.
/// B-2026-09-15-14 — an enum element of a HASH CONTAINER ran no user
/// `Drop` body at all: not late, not doubled, never. The storage was still
/// reclaimed exactly once, so ASAN and both ratchet legs were blind to it,
/// and both backends were silent identically so an A/B kata could not see
/// it either.
///
/// The last container position whose enum arm was never written — the peer
/// of B-2026-08-28-55 (`Vec` element), B-2026-08-28-47 (tuple element) and
/// B-2026-08-28-40 (struct field). Codegen declined because
/// `emit_map_half_user_drop_bodies_fn` gated on `struct_types`, which an
/// enum name is never in; the interpreter declined because a plain enum
/// variant fell past the `Value::Struct` destructure in both map walks.
///
/// The CONTROL rows are the half that pins the axis: a `Vec` of the same
/// enum and a `Map` with a STRUCT element were both already correct, so the
/// defect is hash-container storage of an ENUM specifically, not enums and
/// not containers.
/// B-2026-09-14-23 — a whole-container REASSIGNMENT lost the displaced
/// container's element `Drop` bodies on every compiled backend, while the
/// interpreter ran them: a run-vs-build divergence at the DISPLACEMENT
/// site, not in any element walker (the flat spellings diverged exactly as
/// the nested ones did, which is what said it was one site and not four).
///
/// Two distinct gaps behind one symptom. For a `Vec`, the eager-free block
/// that already carries the displaced-bodies call is gated on a classified
/// RHS, and a container LITERAL matched no arm — `rhs_yields_fresh_ref`
/// answers for `StructLiteral`/`Call`/`MethodCall` only, and the two
/// literal spellings are `ArrayLiteral` and `PrefixCollectionLiteral`. For
/// a fixed `Array`, the local is in neither `vec_elem_types` nor
/// `var_elem_type_exprs`, so that block is inapplicable to it at all and it
/// needed its own bodies-only call keyed on `array_elem_type_exprs`.
///
/// The surviving elements (dD3, dD4) were always correct everywhere, which
/// is what isolates the defect to the displaced value.
#[test]
fn e2e_whole_container_reassign_runs_displaced_element_bodies() {
    const H: &str = "struct D { id: i64, s: String }\n\
             impl Drop for D { fn drop(mut ref self) { println(f\"dD{self.id}\") } }\n\
             fn mkd(i: i64) -> D { return D { id: i, s: f\"tttttttttttttttt{i}\" } }\n";
    for (label, body, want) in [
        (
            "vec-flat",
            "fn main() {\n\
                 \x20   let mut v: Vec[D] = [mkd(1), mkd(2)];\n\
                 \x20   v = [mkd(3), mkd(4)];\n\
                 \x20   println(\"mid\");\n\
                 \x20   println(\"end\");\n\
                 }\n",
            "dD1\ndD2\ndD3\ndD4\nmid\nend\n",
        ),
        (
            "array-flat",
            "fn main() {\n\
                 \x20   let mut v: Array[D, 2] = [mkd(1), mkd(2)];\n\
                 \x20   v = [mkd(3), mkd(4)];\n\
                 \x20   println(\"mid\");\n\
                 \x20   println(\"end\");\n\
                 }\n",
            "dD1\ndD2\ndD3\ndD4\nmid\nend\n",
        ),
        (
            "array-nested",
            "fn main() {\n\
                 \x20   let mut v: Array[Array[D, 1], 2] = [[mkd(1)], [mkd(2)]];\n\
                 \x20   v = [[mkd(3)], [mkd(4)]];\n\
                 \x20   println(\"mid\");\n\
                 \x20   println(\"end\");\n\
                 }\n",
            "dD1\ndD2\ndD3\ndD4\nmid\nend\n",
        ),
        // The container stays USABLE after the reassignment — the displaced
        // bodies run for the OLD elements only. Without this cell a fix
        // that ran bodies over the NEW contents would pass the three above,
        // since there the container dies immediately and the two orders are
        // indistinguishable.
        (
            "vec-read-back-after-reassign",
            "fn main() {\n\
                 \x20   let mut v: Vec[D] = [mkd(1), mkd(2)];\n\
                 \x20   v = [mkd(3), mkd(4)];\n\
                 \x20   println(\"mid\");\n\
                 \x20   println(f\"read:{v[0].id},{v[1].id},len={v.len()}\");\n\
                 \x20   println(\"end\");\n\
                 }\n",
            "dD1\ndD2\nmid\nread:3,4,len=2\ndD3\ndD4\nend\n",
        ),
    ] {
        let prog = format!("{H}{body}");
        assert_eq!(run_program(&prog).as_deref(), Some(want), "{label}");
    }
    // AGREED SILENCES, measured on both backends and NOT this row's shape:
    // a struct FIELD assigned a fresh container, and a displaced `Map`.
    // Both were among the row's NOT MEASURED items; both answered "gap, not
    // divergence", so they were pinned here rather than fixed, and a later
    // widening that starts printing their displaced bodies on one backend
    // only has to move this assertion deliberately.
    //
    // B-2026-09-15-28 MOVED THE FIRST OF THEM, deliberately and on the
    // condition this comment sets. The struct-FIELD silence is fixed: the
    // displaced elements' bodies now run, so the expectation below is the
    // same string the IDENTIFIER position asserts above. Crucially it moved
    // on BOTH backends in one commit — codegen's
    // `emit_displaced_field_bodies` head-name gate and the interpreter's
    // displaced-field `match`, whose struct and enum arms let a
    // `Value::Array` fall through — which is precisely the "on one backend
    // only" outcome this pin exists to catch. Paired fixtures assert the
    // shared string: `e2e_field_assign_runs_the_displaced_containers_element_bodies`
    // and `test_field_assign_runs_the_displaced_containers_element_bodies`.
    //
    // The displaced `Map` silence below is UNMOVED and still pinned: the
    // fix resolves a field's element type through `vec_inner_type_expr`,
    // which answers only for `Vec`/`VecDeque`, so a `Map` field never
    // reaches the new arm. That this assertion still passes is the evidence
    // the widening is scoped rather than blanket.
    assert_eq!(
        run_program(&format!(
            "{H}struct Hold {{ mut v: Vec[D] }}\n\
                 fn main() {{\n\
                 \x20   let mut h = Hold {{ v: [mkd(1), mkd(2)] }};\n\
                 \x20   h.v = [mkd(3), mkd(4)];\n\
                 \x20   println(\"mid\");\n\
                 \x20   println(\"end\");\n\
                 }}\n"
        ))
        .as_deref(),
        Some("dD1\ndD2\ndD3\ndD4\nmid\nend\n"),
        "struct-field target: displaced bodies now run on both backends (B-2026-09-15-28)"
    );
}

/// B-2026-08-29-57 and -65 — a `return x` in a block's TAIL position, with
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
fn e2e_tail_return_hands_the_value_out_like_a_return_statement() {
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
            assert_eq!(run_program(&src).as_deref(), Some(want), "{label}");
        }
}

/// B-2026-09-08-5 — a reassign compiled ONE FRAME DEEPER than the moved-out
/// binding lost the new value's body: `let taken = g.one; if f { g.one =
/// mks(7); }` printed `dS2 t1 dS1` against `--interp`'s
/// `dS2 dS7 t1 dS1`. The bare-block spelling read identically, so the
/// trigger was the pushed frame rather than the branch.
///
/// B-2026-09-07-63's re-arm lifts the move-out mask when a field is given a
/// value of its own, but only in the binding's OWN frame — the mask is
/// compile-time state and a deeper store is a runtime condition, so
/// re-arming statically would run the field's body over the moved-out husk
/// on the path that never stored. The row that split this out priced the
/// fix as needing "a second walker selected at runtime, which does not
/// exist". B-2026-09-08-4 built exactly that, so the re-arm now mints the
/// field's flag with a `false` entry-block initializer — inverted, because
/// the move-out already ran on every path reaching the store — stores
/// `true` where the assignment compiles, and un-masks the registered walker
/// so the death-site tree has both arms to pick between.
///
/// Cell (b) is the whole reason the static re-arm was restricted, and it
/// fails if the inverted initializer is ever dropped: with `f` false
/// nothing stored, so `one`'s body must NOT run over the husk `taken` owns.
/// Cell (c) pins the bare block, the spelling that shows this is about the
/// frame and not the condition.
///
/// Twin of `tests/interpreter.rs`'s
/// `test_deeper_frame_reassign_rearms_the_new_value`.
#[test]
fn e2e_deeper_frame_reassign_rearms_the_new_value() {
    // (a) branch that RUNS — the new value is the base's to drop.
    let Some(out) = run_program(
        "struct Rs { id: i64, name: String }\n\
             impl Drop for Rs {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"dS{self.id}\")\n\
             \x20   }\n\
             }\n\
             fn mks(i: i64) -> Rs {\n\
             \x20   return Rs { id: i, name: f\"h{i}\" };\n\
             }\n\
             struct Bs { mut one: Rs, mut two: Rs }\n\
             fn main() {\n\
             \x20   let f = true;\n\
             \x20   let mut g = Bs { one: mks(1), two: mks(2) };\n\
             \x20   let taken = g.one;\n\
             \x20   if f { g.one = mks(7); }\n\
             \x20   println(f\"t{taken.id}\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "dS2\ndS7\nt1\ndS1\n");
    // (b) branch that does NOT run — nothing stored, so no body over the
    // husk. This is the cell the inverted initializer exists for.
    let Some(out) = run_program(
        "struct Rs { id: i64, name: String }\n\
             impl Drop for Rs {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"dS{self.id}\")\n\
             \x20   }\n\
             }\n\
             fn mks(i: i64) -> Rs {\n\
             \x20   return Rs { id: i, name: f\"h{i}\" };\n\
             }\n\
             struct Bs { mut one: Rs, mut two: Rs }\n\
             fn main() {\n\
             \x20   let f = false;\n\
             \x20   let mut g = Bs { one: mks(1), two: mks(2) };\n\
             \x20   let taken = g.one;\n\
             \x20   if f { g.one = mks(7); }\n\
             \x20   println(f\"t{taken.id}\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "dS2\nt1\ndS1\n");
    // (c) BARE BLOCK — a pushed frame with no condition at all.
    let Some(out) = run_program(
        "struct Rs { id: i64, name: String }\n\
             impl Drop for Rs {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"dS{self.id}\")\n\
             \x20   }\n\
             }\n\
             fn mks(i: i64) -> Rs {\n\
             \x20   return Rs { id: i, name: f\"h{i}\" };\n\
             }\n\
             struct Bs { mut one: Rs, mut two: Rs }\n\
             fn main() {\n\
             \x20   let mut g = Bs { one: mks(1), two: mks(2) };\n\
             \x20   let taken = g.one;\n\
             \x20   { g.one = mks(7); }\n\
             \x20   println(f\"t{taken.id}\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "dS2\ndS7\nt1\ndS1\n");
}

/// B-2026-08-01-16 — the ASSIGN sibling of the param-view rebind:
/// (1) `h2 = h;` onto a pre-declared local makes h2 a param view — its
/// armed bodies actions are retracted so the body fires exactly once,
/// caller-side (pre-fix: doubled on both backends); (2) the displaced
/// old h2 value's field bodies fire AT the assignment (pre-fix: `karac
/// build` freed the heap silently while the interpreter printed the
/// body — `drop 9 z9` / `drop 3 w3` missing under AOT); (3) same for a
/// plain local reassign with no params involved; (4) the enum-assign
/// sibling (`w2 = w; match w2`) binds views. Twin of
/// `tests/interpreter.rs`'s
/// `test_assign_param_rebind_and_displaced_bodies`.
#[test]
fn e2e_assign_param_rebind_and_displaced_bodies() {
    let Some(out) = run_program(
        "struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"drop {self.id} {self.name}\")\n\
             \x20   }\n\
             }\n\
             struct Holder { r: Res }\n\
             enum E2 { B(Res), Empty }\n\
             fn take(h: Holder) {\n\
             \x20   let mut h2 = Holder { r: Res { id: 9, name: f\"z{9}\" } };\n\
             \x20   h2 = h;\n\
             \x20   println(f\"held {h2.r.id}\");\n\
             \x20   println(\"take done\");\n\
             }\n\
             fn take_enum(w: E2) {\n\
             \x20   let mut w2 = E2.Empty;\n\
             \x20   w2 = w;\n\
             \x20   match w2 {\n\
             \x20       E2.B(r) => { println(f\"got {r.id}\"); }\n\
             \x20       E2.Empty => { println(\"none\"); }\n\
             \x20   }\n\
             \x20   println(\"take2 done\");\n\
             }\n\
             fn main() {\n\
             \x20   println(\"a\");\n\
             \x20   let x = Holder { r: Res { id: 5, name: f\"y{5}\" } };\n\
             \x20   take(x);\n\
             \x20   println(\"b\");\n\
             \x20   let mut d = Holder { r: Res { id: 3, name: f\"w{3}\" } };\n\
             \x20   d = Holder { r: Res { id: 4, name: f\"v{4}\" } };\n\
             \x20   println(f\"kept {d.r.id}\");\n\
             \x20   println(\"c\");\n\
             \x20   let e = E2.B(Res { id: 7, name: f\"q{7}\" });\n\
             \x20   take_enum(e);\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(
            out,
            "a\ndrop 9 z9\nheld 5\ntake done\ndrop 5 y5\nb\ndrop 3 w3\nkept 4\ndrop 4 v4\nc\ngot 7\ntake2 done\ndrop 7 q7\nend\n"
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
fn e2e_unconditional_ctor_return_of_param_runs_one_body() {
    assert_eq!(
        run_program(
            r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"drop {self.id} {self.name}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}" }; }
fn top(r: R) -> Option[R] { return Option.Some(r); }
fn topwrap(r: R) -> Result[R, String] { return Result.Ok(r); }
fn main() {
    println("top");    let _ = top(mk(1));
    println("wrap");   let _ = topwrap(mk(2));
    println("done")
}"#
        ),
        Some("top\ndrop 1 h1\nwrap\ndrop 2 h2\ndone\n".to_string()),
        "an unconditional ctor-wrapped return of a param runs one body"
    );
}

/// B-2026-09-13-13 — a conditionally-returned param whose OTHER exit leaf
/// merely READS it keeps exactly one `Drop` body, at every call position
/// and on every surface.
///
/// `if flag { return r } return R { id: 90 + r.id }` was wrong in BOTH
/// directions at once, and the two halves hid each other:
///
/// ```text
///   position   flag=true                        flag=false
///   assoc      aot dR1 k:1 dR1 / interp k:1 dR1  interp k:91 dR91 / aot dR1 k:91 dR91
///   method     aot dR1 k:1 dR1 / interp k:1 dR1  correct on all four
///   free       correct on all four              ALL FOUR k:91 dR91  (dR1 lost)
/// ```
///
/// The row was filed for the top-left cell — a doubled body on the
/// associated and method legs — and recorded the free position as "correct
/// here". It is not: the free leg is correct on the hand-back path and
/// silently loses the body on the dies-inside one, agreed by all four
/// surfaces and therefore invisible to the A/B rule. That is the cell this
/// fixture adds to the row, and it is what shows the defect was one root
/// cause rather than a leg-specific gate.
///
/// THE ROOT CAUSE IS THE CALLEE, NOT EITHER CALLER'S GATE.
/// `fn_conditionally_returns_param_bare`'s condition 3 declined the whole
/// function because a leaf mentioned the param, so the callee registered no
/// per-path owner at all. The associated and method legs read that `false`
/// as "nobody else can own this argument" and hung the full
/// `karac_drop_<T>` wrapper on the caller's temp; the free leg gates on the
/// `fn_returns_param` UNION instead, stood down on both paths, and left the
/// dies-inside path with no owner anywhere. The 2026-09-14 attempt on this
/// row widened the two legs to the union and was reverted for exactly that
/// reason — it traded the double for a fresh loss.
///
/// Condition 3 now admits a leaf whose every mention of the param is a
/// SCALAR-FIELD copy read, so the callee registers the per-path owner and
/// all three legs land on the same answer.
///
/// THE LEAF IS A `Call`, NOT A `Binary`, AND THAT IS WHY THE FIRST CUT DID
/// NOTHING. The parser desugars every binary operator to a qualified call,
/// so `90 + r.id` reaches the predicate as
/// `i64.add(90, r.id)` — measured by printing the declining leaf. A
/// classifier with a `Binary` arm and no operator arm changed not one cell.
///
/// THE TWO GUARD CELLS ARE DELIBERATELY STILL WRONG, and pinned as
/// measured. A leaf that CONSUMES the param through a call
/// (`R { id: eat(r) }`) is still declined — admitting it would register a
/// body for a value handed to `eat` — and it keeps the same three-way
/// split this row's leaf had: the free spelling loses the body on both
/// surfaces, the associated spelling loses it on the interpreter only, the
/// method spelling is correct. Filed as B-2026-09-17-33. Reading a
/// NON-scalar field (`R { name: r.name }`) cannot reach the question at
/// all — `partial_move_of_drop_struct` rejects it at the front end — so the
/// scalar-field keying is belt-and-braces there rather than the only guard.
///
/// MEMORY IS CLEAN AND THE DIES-INSIDE READ IS INTACT, which is the thing a
/// body-count fix could plausibly break: with a heap-carrying `R { name:
/// String, id: i64 }` the `flag = false` path prints `dR1/a` with the
/// string readable, and `-O0` valgrind reports 12-14 allocs with equal
/// frees, `0 bytes in 0 blocks` at exit, `0 errors` and no invalid access
/// on every cell.
#[test]
fn e2e_conditionally_returned_param_with_a_reading_leaf_runs_one_body() {
    const R: &str = "struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n";
    const RH: &str = "struct R { name: String, id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}/{self.name}\") } }\n";
    // (label, program, AOT expectation, interpreter expectation)
    for (label, prog, want, interp_want) in [
            // 1-2 — ASSOCIATED, the row's headline position.
            (
                "assoc, hand-back path",
                format!(
                    "{R}struct Sk {{ n: i64 }}\n\
                     impl Sk {{ fn pick(r: R, flag: bool) -> R {{ if flag {{ return r }} return R {{ id: 90 + r.id }} }} }}\n\
                     fn main() {{ let k = Sk.pick(R {{ id: 1 }}, true); println(f\"k:{{k.id}}\"); println(\"end\") }}\n"
                ),
                "k:1\ndR1\nend\n",
                "k:1\ndR1\nend\n",
            ),
            (
                "assoc, dies-inside path",
                format!(
                    "{R}struct Sk {{ n: i64 }}\n\
                     impl Sk {{ fn pick(r: R, flag: bool) -> R {{ if flag {{ return r }} return R {{ id: 90 + r.id }} }} }}\n\
                     fn main() {{ let k = Sk.pick(R {{ id: 1 }}, false); println(f\"k:{{k.id}}\"); println(\"end\") }}\n"
                ),
                "dR1\nk:91\ndR91\nend\n",
                // Was `k:91 dR91 end` under `--interp` — the interpreter's own
                // half of this shape, filed as B-2026-09-14-8 for the
                // associated position specifically. Same root cause, so it
                // moves with this fix.
                "dR1\nk:91\ndR91\nend\n",
            ),
            // 3-4 — METHOD. The hand-back path doubled; the dies-inside path
            //       was already correct and must stay so.
            (
                "method, hand-back path",
                format!(
                    "{R}struct Sk {{ n: i64 }}\n\
                     impl Sk {{ fn pick(ref self, r: R, flag: bool) -> R {{ if flag {{ return r }} return R {{ id: 90 + r.id }} }} }}\n\
                     fn main() {{ let s = Sk {{ n: 1 }}; let k = s.pick(R {{ id: 1 }}, true); println(f\"k:{{k.id}}\"); println(\"end\") }}\n"
                ),
                "k:1\ndR1\nend\n",
                "k:1\ndR1\nend\n",
            ),
            (
                "method, dies-inside path",
                format!(
                    "{R}struct Sk {{ n: i64 }}\n\
                     impl Sk {{ fn pick(ref self, r: R, flag: bool) -> R {{ if flag {{ return r }} return R {{ id: 90 + r.id }} }} }}\n\
                     fn main() {{ let s = Sk {{ n: 1 }}; let k = s.pick(R {{ id: 1 }}, false); println(f\"k:{{k.id}}\"); println(\"end\") }}\n"
                ),
                "dR1\nk:91\ndR91\nend\n",
                "dR1\nk:91\ndR91\nend\n",
            ),
            // 5-6 — FREE. Cell 6 is the one the row did not have: an agreed
            //       loss on all four surfaces, which is why it read as correct.
            (
                "free, hand-back path",
                format!(
                    "{R}fn pick(r: R, flag: bool) -> R {{ if flag {{ return r }} return R {{ id: 90 + r.id }} }}\n\
                     fn main() {{ let k = pick(R {{ id: 1 }}, true); println(f\"k:{{k.id}}\"); println(\"end\") }}\n"
                ),
                "k:1\ndR1\nend\n",
                "k:1\ndR1\nend\n",
            ),
            (
                "free, dies-inside path",
                format!(
                    "{R}fn pick(r: R, flag: bool) -> R {{ if flag {{ return r }} return R {{ id: 90 + r.id }} }}\n\
                     fn main() {{ let k = pick(R {{ id: 1 }}, false); println(f\"k:{{k.id}}\"); println(\"end\") }}\n"
                ),
                "dR1\nk:91\ndR91\nend\n",
                "dR1\nk:91\ndR91\nend\n",
            ),
            // 7-8 — CONTROLS: the CONSTANT leaf, which condition 3 always
            //       admitted (B-2026-09-12-26's headline). Correct before and
            //       after; here so a regression in the admitted path shows.
            (
                "control: assoc, constant leaf, hand-back",
                format!(
                    "{R}struct Sk {{ n: i64 }}\n\
                     impl Sk {{ fn pick(r: R, flag: bool) -> R {{ if flag {{ return r }} return R {{ id: 90 }} }} }}\n\
                     fn main() {{ let k = Sk.pick(R {{ id: 1 }}, true); println(f\"k:{{k.id}}\"); println(\"end\") }}\n"
                ),
                "k:1\ndR1\nend\n",
                "k:1\ndR1\nend\n",
            ),
            (
                "control: assoc, constant leaf, dies-inside",
                format!(
                    "{R}struct Sk {{ n: i64 }}\n\
                     impl Sk {{ fn pick(r: R, flag: bool) -> R {{ if flag {{ return r }} return R {{ id: 90 }} }} }}\n\
                     fn main() {{ let k = Sk.pick(R {{ id: 1 }}, false); println(f\"k:{{k.id}}\"); println(\"end\") }}\n"
                ),
                "dR1\nk:90\ndR90\nend\n",
                "dR1\nk:90\ndR90\nend\n",
            ),
            // 9 — the HEAP-CARRYING payload on the dies-inside path: the body
            //     reads its own string, so the read-then-drop order is right
            //     and this is not a use-after-move.
            (
                "heap payload, dies-inside path reads its string",
                format!(
                    "{RH}struct Sk {{ n: i64 }}\n\
                     impl Sk {{ fn pick(r: R, flag: bool) -> R {{ if flag {{ return r }} return R {{ name: f\"z\", id: 90 + r.id }} }} }}\n\
                     fn main() {{ let k = Sk.pick(R {{ name: f\"a\", id: 1 }}, false); println(f\"k:{{k.id}}\"); println(\"end\") }}\n"
                ),
                "dR1/a\nk:91\ndR91/z\nend\n",
                "dR1/a\nk:91\ndR91/z\nend\n",
            ),
            // 10-11 — GUARDS: a leaf that CONSUMES the param through a call is
            //     still declined, and still wrong in the way this row's leaf
            //     used to be. Pinned as measured; B-2026-09-17-33.
            (
                // 12 — B-2026-09-14-8's headline distinction: adding a
                //      `mut ref self` receiver to the identical body made the
                //      interpreter correct where the ASSOCIATED spelling lost
                //      the body. Both are correct now, and that row's one
                //      unmeasured question — whether a FREE function with the
                //      same body loses it too — is cell 6: it did, on all four
                //      surfaces, which is why it was invisible.
                "mut ref self receiver, dies-inside path",
                format!(
                    "{R}struct Sk {{ n: i64 }}\n\
                     impl Sk {{ fn pick(mut ref self, r: R, flag: bool) -> R {{ if flag {{ return r }} return R {{ id: 90 + r.id }} }} }}\n\
                     fn main() {{ let mut s = Sk {{ n: 0 }}; let k = s.pick(R {{ id: 1 }}, false); println(f\"k:{{k.id}}\"); println(\"end\") }}\n"
                ),
                "dR1\nk:91\ndR91\nend\n",
                "dR1\nk:91\ndR91\nend\n",
            ),
            (
                "guard: assoc, consuming leaf — still divergent",
                format!(
                    "{RH}fn eat(x: R) -> i64 {{ return x.id; }}\n\
                     struct Sk {{ n: i64 }}\n\
                     impl Sk {{ fn pick(r: R, flag: bool) -> R {{ if flag {{ return r }} return R {{ name: f\"z\", id: eat(r) }} }} }}\n\
                     fn main() {{ let k = Sk.pick(R {{ name: f\"a\", id: 1 }}, false); println(f\"k:{{k.id}}\"); println(\"end\") }}\n"
                ),
                "dR1/a\nk:1\ndR1/z\nend\n",
                "k:1\ndR1/z\nend\n",
            ),
            (
                "guard: free, consuming leaf — agreed loss",
                format!(
                    "{RH}fn eat(x: R) -> i64 {{ return x.id; }}\n\
                     fn pick(r: R, flag: bool) -> R {{ if flag {{ return r }} return R {{ name: f\"z\", id: eat(r) }} }}\n\
                     fn main() {{ let k = pick(R {{ name: f\"a\", id: 1 }}, false); println(f\"k:{{k.id}}\"); println(\"end\") }}\n"
                ),
                "k:1\ndR1/z\nend\n",
                "k:1\ndR1/z\nend\n",
            ),
        ] {
            let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(&prog);
            assert!(
                interp_errs.is_empty(),
                "[{label}] interp errored: {interp_errs:?}"
            );
            assert_eq!(interp_out.join(""), interp_want, "[{label}] interpreter");
            if let Some(aot) = run_program(&prog) {
                assert_eq!(aot, want, "[{label}] AOT");
            }
        }
}

#[test]
fn e2e_ctor_wrapped_conditional_return_runs_one_body() {
    assert_eq!(
            run_program(
                r#"struct R { id: i64, name: String }
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
"#
            ),
            Some(
                "S1\ndrop 3 h3\ndrop 4 h4\ndrop 5 h5\nS2\ndrop 14 h14\nS3\ndrop 24 h24\nS4\ndrop 34 h34\nRok\ndrop 41 h41\ndrop 42 h42\nRer\ndrop 43 h43\ndrop 44 h44\ntail\ndrop 51 h51\ndrop 52 h52\ndrop 53 h53\ndrop 54 h54\ndone\n"
                    .to_string()
            ),
            "one body per call, every spelling, ctor-wrapped conditional return"
        );
}

/// B-2026-09-06-13 — a by-value `Drop` param handed BARE to a callee that
/// returns it on SOME exits (`let w: R = keepc(r, k)` over `fn keepc(r: R,
/// k: bool) -> R { if k { return r; } return mk(99); }`) runs its body ONCE
/// on each path: in `keepc` when the value dies there, in `w` when it is
/// handed back — never a second time from the OUTER caller. The hand-over
/// is a conditional store in the passthrough family's terms
/// (`fn_conditionally_hands_param_to_flip_callee`): the caller stands down
/// through the via-call channel, and the frame registers the per-path
/// bodies-only drop cleared at the handing statement, so the NESTED
/// spelling keeps the body on the path that never reaches the call. Cells:
/// read / unread / discarded / statement-position / returned (`return w`
/// and the direct `return keepc(r, k)` control) / an `Option` ctor flip /
/// nested in a branch on all three path combinations / a local source
/// control / a method frame / named arguments, each on both `k` values.
/// Interpreter twin:
/// `test_param_handed_to_conditionally_returning_callee_runs_one_body`.
#[test]
fn e2e_param_handed_to_conditionally_returning_callee_runs_one_body() {
    assert_eq!(
            run_program(
                r#"struct R { id: i64, tag: String, xs: Vec[i64] }
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
}"#
            ),
            Some("one-t\nsc 1\ndR1\ntwo-f\ndR2\nsc 99\ndR99\nthree-t\ndR3\nscu\nfour-f\ndR4\ndR99\nscu\nfive-t\ndR5\nscd\nsix-f\ndR6\ndR99\nscd\nseven-t\ndR7\nscs\neight-f\ndR8\ndR99\nscs\nnine-t\ngot 9\ndR9\nten-f\ndR10\ngot 99\ndR99\neleven-t\ngot 11\ndR11\ntwelve-f\ndR12\ngot 99\ndR99\nthirteen-t\nso 13\ndR13\nfourteen-f\ndR14\nso none\nfifteen-tt\nscn 15\ndR15\nscn-out\nsixteen-ft\ndR16\nscn 99\ndR99\nscn-out\nseventeen-tf\nscn-out\ndR17\neighteen-lt\nsl 50\ndR50\nnineteen-lf\ndR50\nsl 99\ndR99\ntwenty-mt\nmc 20\ndR20\ntwentyone-mf\ndR21\nmc 99\ndR99\ntwentytwo-nt\nsc 22\ndR22\ntwentythree-nf\ndR23\nsc 99\ndR99\nend\n".to_string()),
            "a param handed to a conditionally-returning callee has one owner per path"
        );
}

/// B-2026-09-06-44 — the codegen sibling of B-2026-09-06-41: on the
/// PROJECTION spelling (`let a = s.a` inside `fn g(s: S3)`, `s` a by-value
/// param the callee never came to own) every compiled backend ran the
/// sibling field's body twice (`mid dR2 dR2 dR1`) against `--interp`'s one,
/// while the destructure spelling of the same callee was already agreed.
/// The returned-projection disarm minted a `$keep` walk for the param
/// root itself, and that walk ran `b`'s body in the callee beside the
/// caller's own after-call walk. A root with no walk of its own — a
/// param-view local before, a caller-retained by-value param now — gets
/// none minted there. Pins the no-read, scalar-read, sibling, both-fields,
/// interleaved-reads, whole-field-return, heap-field, `self`-root,
/// named-argument and destructure-control spellings against the
/// interpreter twin.
///
/// Twin of `tests/interpreter.rs`'s `test_projection_off_a_param_runs_the_sibling_body_once`, pinned to the same string.
#[test]
fn e2e_projection_off_a_param_runs_the_sibling_body_once() {
    let Some(out) = run_program(
        r#"struct R { id: i64, name: String, xs: Vec[i64] }
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
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
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
/// Twin of `tests/interpreter.rs`'s `test_scalar_leaf_return_keeps_the_arg_walk`, pinned to the same string.
#[test]
fn e2e_scalar_leaf_return_keeps_the_arg_walk() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
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
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
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

#[test]
fn test_e2e_continue_in_chars_loop_advances_the_byte_offset() {
    // B-2026-07-27-8: `continue` inside the general `for ch in s.chars()`
    // loop was an INFINITE LOOP in compiled code on every backend and both
    // opt levels, while the interpreter ran correctly. The decoded offset
    // was stored at the BODY TAIL, and `continue` branches straight to the
    // incr block — skipping the store, so the offset never advanced and the
    // loop re-read the same character forever. With a `break` also present
    // the program terminated but returned a silently WRONG answer.
    //
    // Every shape here must take the GENERAL decode loop (multibyte, or a
    // binding mutated after its `let`, or a String parameter) — the
    // branch-free ASCII loop from B-2026-07-27-7 advances in its incr block
    // and was never affected, so testing only ASCII constants would miss
    // the regression entirely.
    if let Some(out) = run_program(
        "fn count_non_x(s: String) -> i64 {\n\
             \x20   let mut n = 0i64;\n\
             \x20   for c in s.chars() { if c == 'x' { continue; } n = n + 1i64; }\n\
             \x20   return n;\n\
             }\n\
             fn main() {\n\
             \x20   let s: String = \"a\u{e9}c\";\n\
             \x20   let mut n = 0i64;\n\
             \x20   for ch in s.chars() { if ch == 'c' { continue; } n = n + 1i64; }\n\
             \x20   println(n);\n\
             \x20   let mut g: String = \"abcde\";\n\
             \x20   g.push('\u{e9}');\n\
             \x20   let mut i = 0i64;\n\
             \x20   let mut sum = 0i64;\n\
             \x20   for c in g.chars() {\n\
             \x20       if i == 1i64 { i = i + 1i64; continue; }\n\
             \x20       if i == 4i64 { break; }\n\
             \x20       sum = sum + (c as i64);\n\
             \x20       i = i + 1i64;\n\
             \x20   }\n\
             \x20   println(sum);\n\
             \x20   println(count_non_x(\"axbx\u{e9}\"));\n\
             \x20   let mut t: String = \"a\u{e9}\";\n\
             \x20   t.push('z');\n\
             \x20   let mut m = 0i64;\n\
             \x20   outer: for c in t.chars() {\n\
             \x20       for k in 0i64..3i64 {\n\
             \x20           if k == 1i64 { continue outer; }\n\
             \x20           m = m + (c as i64);\n\
             \x20       }\n\
             \x20   }\n\
             \x20   println(m);\n\
             \x20   let u: String = \"\u{e9}\u{e9}\";\n\
             \x20   let mut z = 0i64;\n\
             \x20   for c in u.chars() { z = z + 1i64; continue; }\n\
             \x20   println(z);\n\
             }",
    ) {
        assert_eq!(
            out,
            // "aéc" skipping 'c' = 2 chars; "abcde"+é with continue at i==1
            // and break at i==4 = 97+99+100 = 296; "axbxé" minus the two x
            // = 3; "aéz" with a labeled continue after one inner step each =
            // 97+233+122 = 452; "éé" continuing every iteration = 2.
            "2\n296\n3\n452\n2\n"
        );
    }
}

#[test]
fn test_ir_ascii_const_chars_loop_is_branch_free_stride_1() {
    // B-2026-07-27-7: `for ch in <ascii-const>.chars()` must lower to a
    // branch-free stride-1 walk — no ASCII peek-and-branch, no
    // `karac_string_decode_char` call, so the byte offset IS the
    // induction variable. That shape is what lets LLVM reduce a search
    // over a constant string to a single indexed load; with the general
    // loop's offset PHI in the way it cannot, and the walk survives into
    // the binary (measured 699.4M vs 42.8M instructions on a 4.25M-call
    // `nth_letter` probe).
    //
    // Assert on the ABSENCE of the decode call, not just on output — a
    // correct-but-unfoldable lowering is exactly the bug.
    let ir = ir_for(
        "fn nth(n: i64) -> char {\n\
             \x20   let alphabet: String = \"abcdefghijklmnopqrstuvwxyz\";\n\
             \x20   let mut i = 0i64;\n\
             \x20   for ch in alphabet.chars() {\n\
             \x20       if i == n { return ch; }\n\
             \x20       i = i + 1i64;\n\
             \x20   }\n\
             \x20   return 'a';\n\
             }\n\
             fn main() { println(nth(3i64)); }",
    );
    let nth = ir
        .split("define")
        .find(|f| f.contains("@nth"))
        .expect("@nth not found in IR");
    assert!(
        !nth.contains("@karac_string_decode_char"),
        "a proven all-ASCII constant must take the branch-free chars loop \
             (no per-char decode call); got:\n{}",
        nth
    );
    assert!(
        nth.contains("for.sa.cond"),
        "expected the branch-free stride-1 chars loop blocks (for.sa.*); got:\n{}",
        nth
    );
}

/// Regression (B-2026-06-14-13): a `for <name> in xs` loop binding that
/// SHARES A NAME with an earlier same-function `let <name>` must not be
/// conflated with it by the ownership RC analysis. Here the spawn result
/// is bound to `handle` (consumed by `push`), then `for handle in handles`
/// reuses the name. Before the fix, the RC predicate paired the `push`
/// consume with the loop body's `handle.join()` use (dominance-incomparable
/// across the loop boundary) and inserted a spurious RC fallback on the
/// shared name; codegen then RC-boxed the binding and mis-lowered the plain
/// `{i64}` loop element as an Rc pointer → segfault (native) / `join`
/// deadlock (wasm-threads). Fixed by scoping the for-loop binding to a
/// per-loop `@forN` rename frame in the CFG (`src/cfg.rs`), like match
/// arms. Same workload as the distinct-name test above; only the names
/// collide — so a regression reappears as a crash/hang here, not a wrong
/// number.
#[test]
fn e2e_for_loop_binding_name_collision_no_false_rc() {
    if let Some(out) = run_program(
            "fn band(n: i64) -> Vec[i64] {\n\
                 let mut v: Vec[i64] = Vec.new();\n\
                 let mut i = 0;\n\
                 while i < n { v.push(i); i = i + 1; }\n\
                 v\n\
             }\n\
             fn main() {\n\
                 let mut pool: TaskGroup = TaskGroup.new();\n\
                 let mut handles: Vec[TaskHandle[Vec[i64]]] = Vec.new();\n\
                 let mut k = 0;\n\
                 while k < 3 {\n\
                     let handle: TaskHandle[Vec[i64]] = pool.spawn(|| band(k + 2));\n\
                     handles.push(handle);\n\
                     k = k + 1;\n\
                 }\n\
                 let mut total = 0;\n\
                 for handle in handles { let c: Vec[i64] = handle.join(); total = total + c.len(); }\n\
                 println(f\"{total}\");\n\
             }",
        ) {
            assert_eq!(out, "9\n");
        }
}

// ── `providers { R => v } in { body }` block form (B-2026-07-31-9) ──
//
// The block sugar compiled to NOTHING: `compile_expr` had no
// `ExprKind::Providers` arm, so it fell to the catch-all returning constant
// 0 and the body was never emitted. A compiled program printed nothing and
// exited 0 while `--interp` ran it correctly — a silent no-output
// run-vs-build divergence on the form users reach for first (the desugared
// `with_provider[R](v, ||{})` call was always fine).

#[test]
fn e2e_providers_block_runs_body() {
    if let Some(out) = run_program(
        "trait Counter { fn get(ref self) -> i64; }\n\
             effect resource Ctr: Counter;\n\
             struct InMem { n: i64 }\n\
             impl Counter for InMem { fn get(ref self) -> i64 { self.n } }\n\
             fn read() -> i64 with reads(Ctr) { Ctr.get() }\n\
             fn main() {\n\
                 providers { Ctr => InMem { n: 42 } } in { println(f\"{read()}\"); }\n\
             }",
    ) {
        assert_eq!(out, "42\n");
    }
}

#[test]
fn e2e_providers_block_multi_binding_nesting_and_order() {
    // Three properties in one program, all of which a nested-`with_provider`
    // lowering would get wrong:
    //  * several bindings in ONE block are all installed;
    //  * an inner block shadows one resource and leaves the other visible,
    //    and the outer value is restored on exit;
    //  * every provider EXPRESSION is evaluated before ANY frame is pushed,
    //    so `B => InMem { n: ra() }` reads the OUTER `A` (5), not the `A`
    //    being installed beside it (7). This is why the lowering resolves
    //    all bindings first rather than nesting — the interpreter's
    //    `eval_providers_block` has the same two-phase order.
    if let Some(out) = run_program(
        "trait Counter { fn get(ref self) -> i64; }\n\
             effect resource A: Counter;\n\
             effect resource B: Counter;\n\
             struct InMem { n: i64 }\n\
             impl Counter for InMem { fn get(ref self) -> i64 { self.n } }\n\
             fn ra() -> i64 with reads(A) { A.get() }\n\
             fn rb() -> i64 with reads(B) { B.get() }\n\
             fn main() {\n\
                 providers { A => InMem { n: 1 }, B => InMem { n: 2 } } in {\n\
                     println(f\"multi {ra()} {rb()}\");\n\
                 }\n\
                 providers { A => InMem { n: 10 }, B => InMem { n: 20 } } in {\n\
                     providers { A => InMem { n: 99 } } in {\n\
                         println(f\"nested {ra()} {rb()}\");\n\
                     }\n\
                     println(f\"restored {ra()} {rb()}\");\n\
                 }\n\
                 providers { A => InMem { n: 5 } } in {\n\
                     providers { A => InMem { n: 7 }, B => InMem { n: ra() } } in {\n\
                         println(f\"order {ra()} {rb()}\");\n\
                     }\n\
                 }\n\
             }",
    ) {
        assert_eq!(out, "multi 1 2\nnested 99 20\nrestored 10 20\norder 7 5\n");
    }
}

// ── Early exit out of a provider body (B-2026-07-31-11) ──
//
// The `karac_provider_pop` used to be emitted inline after the body, so a
// terminator inside the body (an early `return`, or a `break` out of an
// enclosing loop) left the pop unreachable: `with_provider` failed with a
// raw LLVM "Terminator found in the middle of a basic block" and the block
// form refused with a diagnostic, while `--interp` ran both. The pop is now
// a `CleanupAction::ProviderPop` on a dedicated frame around the body, so
// every exit path drains it — after body-local drops, before the
// terminator, which is the interpreter's order.

#[test]
fn e2e_with_provider_early_return_body() {
    // The exact form-A ledger repro: tail-position `with_provider` whose
    // closure body early-returns. Must build AND print 8 like `--interp`.
    if let Some(out) = run_program(
        "trait Counter { fn get(ref self) -> i64; }\n\
             effect resource Ctr: Counter;\n\
             struct InMem { n: i64 }\n\
             impl Counter for InMem { fn get(ref self) -> i64 { self.n } }\n\
             fn read() -> i64 with reads(Ctr) { Ctr.get() }\n\
             fn early() -> i64 with reads(Ctr) {\n\
                 with_provider[Ctr](InMem { n: 8 }, || { return read(); })\n\
             }\n\
             fn main() with reads(Ctr) { println(f\"{early()}\"); }",
    ) {
        assert_eq!(out, "8\n");
    }
}

#[test]
fn e2e_providers_block_early_return_body() {
    // Form-B ledger repro: the block form used to REFUSE this shape with
    // an actionable diagnostic (better than the pre-B-2026-07-31-9
    // constant-0 silent wrong answer, but still uncompilable).
    if let Some(out) = run_program(
        "trait Counter { fn get(ref self) -> i64; }\n\
             effect resource Ctr: Counter;\n\
             struct InMem { n: i64 }\n\
             impl Counter for InMem { fn get(ref self) -> i64 { self.n } }\n\
             fn read() -> i64 with reads(Ctr) { Ctr.get() }\n\
             fn early2() -> i64 with reads(Ctr) {\n\
                 providers { Ctr => InMem { n: 8 } } in { return read(); }\n\
             }\n\
             fn main() with reads(Ctr) { println(f\"{early2()}\"); }",
    ) {
        assert_eq!(out, "8\n");
    }
}

#[test]
fn e2e_providers_block_multi_binding_early_return_pops_all() {
    // Multi-binding block + early return: the return edge must drain BOTH
    // pops (the cleanup frame holds one ProviderPop per pushed binding).
    // A missed pop trips the runtime's head==frame assertion on the next
    // provider operation or corrupts later dispatch.
    if let Some(out) = run_program(
        "trait Counter { fn get(ref self) -> i64; }\n\
             trait Namer { fn name(ref self) -> String; }\n\
             effect resource Ctr: Counter;\n\
             effect resource Who: Namer;\n\
             struct InMem { n: i64 }\n\
             impl Counter for InMem { fn get(ref self) -> i64 { self.n } }\n\
             struct N1 { s: String }\n\
             impl Namer for N1 { fn name(ref self) -> String { self.s.clone() } }\n\
             fn read() -> i64 with reads(Ctr) { Ctr.get() }\n\
             fn multi() -> String with reads(Ctr) reads(Who) {\n\
                 providers { Ctr => InMem { n: 2 }, Who => N1 { s: \"zed\" } } in {\n\
                     return f\"{Who.name()}-{read()}\";\n\
                 }\n\
             }\n\
             fn main() with reads(Ctr) reads(Who) {\n\
                 println(f\"{multi()}\");\n\
                 with_provider[Ctr](InMem { n: 9 }, || { println(f\"{read()}\"); });\n\
             }",
    ) {
        // The second line proves the provider stack is healthy AFTER the
        // early-returning block: a fresh push/dispatch/pop still works.
        assert_eq!(out, "zed-2\n9\n");
    }
}

#[test]
fn e2e_with_provider_early_return_stack_intact_after() {
    // Nested shape: a helper whose with_provider body early-returns is
    // called from INSIDE an outer with_provider body. Both the inner pop
    // (on the return edge) and the outer resolution AFTER the call must
    // work — a dangling inner frame (an entry-block alloca of the
    // already-returned helper) would corrupt the outer read. This is the
    // scenario the "do not skip the pop when terminated" ledger warning
    // describes.
    if let Some(out) = run_program(
        "trait Counter { fn get(ref self) -> i64; }\n\
             effect resource Ctr: Counter;\n\
             struct InMem { n: i64 }\n\
             impl Counter for InMem { fn get(ref self) -> i64 { self.n } }\n\
             fn read() -> i64 with reads(Ctr) { Ctr.get() }\n\
             fn inner_early() -> i64 with reads(Ctr) {\n\
                 with_provider[Ctr](InMem { n: 7 }, || { return read(); })\n\
             }\n\
             fn nested() -> i64 with reads(Ctr) {\n\
                 with_provider[Ctr](InMem { n: 100 }, || {\n\
                     let inner = inner_early();\n\
                     inner + read()\n\
                 })\n\
             }\n\
             fn main() with reads(Ctr) { println(f\"{nested()}\"); }",
    ) {
        assert_eq!(out, "107\n");
    }
}

#[test]
fn e2e_with_provider_enclosing_loop_break_pops_frame() {
    // `break` out of an ENCLOSING loop from inside a with_provider body:
    // the break edge drains the ProviderPop (it sits above the loop's
    // cleanup depth), and the next iteration pushes a fresh frame onto a
    // healthy stack. acc accumulates 0+1+2, breaks when read() == 3.
    //
    // The interpreter agrees since B-2026-07-31-15 (its `Assign` arm used
    // to store the poison Unit before propagating the break); the interp
    // twin lives in tests/interpreter.rs.
    if let Some(out) = run_program(
        "trait Counter { fn get(ref self) -> i64; }\n\
             effect resource Ctr: Counter;\n\
             struct InMem { n: i64 }\n\
             impl Counter for InMem { fn get(ref self) -> i64 { self.n } }\n\
             fn read() -> i64 with reads(Ctr) { Ctr.get() }\n\
             fn brk() -> i64 with reads(Ctr) {\n\
                 let mut acc = 0;\n\
                 for i in 0..5 {\n\
                     acc = acc + with_provider[Ctr](InMem { n: i }, || {\n\
                         if read() == 3 { break; }\n\
                         read()\n\
                     });\n\
                 }\n\
                 acc\n\
             }\n\
             fn main() with reads(Ctr) { println(f\"{brk()}\"); }",
    ) {
        assert_eq!(out, "3\n");
    }
}

#[test]
fn e2e_providers_block_enclosing_loop_break_pops_frame() {
    // Block-form sibling of the loop-break test: pop drains on the break
    // edge, next iteration re-pushes cleanly.
    if let Some(out) = run_program(
        "trait Counter { fn get(ref self) -> i64; }\n\
             effect resource Ctr: Counter;\n\
             struct InMem { n: i64 }\n\
             impl Counter for InMem { fn get(ref self) -> i64 { self.n } }\n\
             fn read() -> i64 with reads(Ctr) { Ctr.get() }\n\
             fn brk2() -> i64 with reads(Ctr) {\n\
                 let mut acc = 0;\n\
                 for i in 0..5 {\n\
                     providers { Ctr => InMem { n: i } } in {\n\
                         if read() == 3 { break; }\n\
                         acc = acc + read();\n\
                     }\n\
                 }\n\
                 acc\n\
             }\n\
             fn main() with reads(Ctr) { println(f\"{brk2()}\"); }",
    ) {
        assert_eq!(out, "3\n");
    }
}

// ── Terminated init/RHS statements (B-2026-07-31-17) ──
//
// A `let` / assignment whose RHS TERMINATES the current block (a `!`-typed
// block expression: `{ return 5; }`, an if/match whose every arm returns,
// a block that breaks the enclosing loop) used to emit its store AFTER the
// terminator, failing module verification ("Terminator found in the middle
// of a basic block") on programs the interpreter runs fine. The guard now
// skips the dead binding/store; compile_block's per-statement terminator
// check stops the rest of the block, exactly as for a bare `return;`.

#[test]
fn e2e_let_init_block_returns() {
    // The ledger repro: the init returns, the binding never materializes,
    // the tail 7 is unreachable.
    if let Some(out) = run_program(
        "fn t() -> i64 {\n\
                 let x = { return 5; };\n\
                 7\n\
             }\n\
             fn main() { println(f\"{t()}\"); }",
    ) {
        assert_eq!(out, "5\n");
    }
}

#[test]
fn e2e_let_init_if_both_arms_return() {
    if let Some(out) = run_program(
        "fn t(c: bool) -> i64 {\n\
                 let x = if c { return 1; } else { return 2; };\n\
                 9\n\
             }\n\
             fn main() {\n\
                 println(f\"{t(true)}\");\n\
                 println(f\"{t(false)}\");\n\
             }",
    ) {
        assert_eq!(out, "1\n2\n");
    }
}

#[test]
fn e2e_let_init_breaks_enclosing_loop() {
    // The break-init variant: the let never binds and the loop exits;
    // 0+1+2 accumulate before i == 3 breaks.
    if let Some(out) = run_program(
        "fn t() -> i64 {\n\
                 let mut acc = 0;\n\
                 for i in 0..5 {\n\
                     if i == 3 {\n\
                         let x = { break; };\n\
                     }\n\
                     acc = acc + i;\n\
                 }\n\
                 acc\n\
             }\n\
             fn main() { println(f\"{t()}\"); }",
    ) {
        assert_eq!(out, "3\n");
    }
}

#[test]
fn e2e_assign_rhs_block_returns() {
    // Assign + CompoundAssign twins: the store/binop must not be emitted
    // after the RHS's terminator.
    if let Some(out) = run_program(
        "fn a1() -> i64 {\n\
                 let mut x = 1;\n\
                 x = { return 5; };\n\
                 x\n\
             }\n\
             fn a2() -> i64 {\n\
                 let mut x = 1;\n\
                 x += { return 6; };\n\
                 x\n\
             }\n\
             fn main() {\n\
                 println(f\"{a1()}\");\n\
                 println(f\"{a2()}\");\n\
             }",
    ) {
        assert_eq!(out, "5\n6\n");
    }
}

#[test]
fn e2e_stmts_after_terminated_let_are_skipped() {
    // Statements after the terminated let must not execute (or emit into
    // the terminated block).
    if let Some(out) = run_program(
        "fn t() -> i64 {\n\
                 let x = { return 5; };\n\
                 println(\"never\");\n\
                 7\n\
             }\n\
             fn main() { println(f\"{t()}\"); }",
    ) {
        assert_eq!(out, "5\n");
    }
}

#[test]
fn test_e2e_for_chain_two_vecs() {
    // B-2026-07-14-8 (chain leg): `for x in xs.iter().chain(ys.iter())`
    // lowers to two sequential index loops sharing ONE exit block — a
    // `break` in the FIRST source's body exits the whole chain (never
    // falls into the second source), and `continue` targets the current
    // source's own increment. Scalar elements; heap shapes bail loud.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let v: Vec[i64] = [1, 2];\n\
                 let w: Vec[i64] = [10, 20, 30];\n\
                 let mut s: i64 = 0;\n\
                 for x in v.iter().chain(w.iter()) { s = s + x; }\n\
                 println(s);\n\
                 let mut t: i64 = 0;\n\
                 for x in v.iter().chain(w.iter()) {\n\
                     if x == 2 { break; }\n\
                     t = t + x;\n\
                 }\n\
                 println(t);\n\
                 let mut u: i64 = 0;\n\
                 for x in v.iter().chain(w.iter()) {\n\
                     if x == 20 { continue; }\n\
                     u = u + x;\n\
                 }\n\
                 println(u);\n\
             }",
    ) {
        assert_eq!(out, "63\n1\n43\n");
    }
}

#[test]
fn test_e2e_for_skip_take_window() {
    // B-2026-07-14-8 (skip/take leg): `skip`/`take` chains over a named
    // scalar Vec fold into a `[start, end)` index window (clamped to the
    // running window), then run the ordinary by-value loop. Covers
    // skip+take, take-only, skip-only, over-length clamps both ways, and
    // take-then-skip ordering.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let v: Vec[i64] = [1, 2, 3, 4, 5, 6];\n\
                 let mut s: i64 = 0;\n\
                 for x in v.iter().skip(2).take(3) { s = s + x; }\n\
                 println(s);\n\
                 let mut t: i64 = 0;\n\
                 for x in v.iter().take(4) { t = t + x; }\n\
                 println(t);\n\
                 let mut u: i64 = 0;\n\
                 for x in v.iter().skip(4) { u = u + x; }\n\
                 println(u);\n\
                 let mut w: i64 = 0;\n\
                 for x in v.iter().skip(10) { w = w + x; }\n\
                 println(w);\n\
                 let mut z: i64 = 0;\n\
                 for x in v.iter().take(100) { z = z + x; }\n\
                 println(z);\n\
                 let mut n: i64 = 0;\n\
                 for x in v.iter().take(4).skip(1) { n = n + x; }\n\
                 println(n);\n\
             }",
    ) {
        assert_eq!(out, "12\n10\n11\n0\n21\n9\n");
    }
}

#[test]
fn test_e2e_for_flat_map() {
    // B-2026-07-14-8 (flat_map leg): `for x in recv.flat_map(|p| inner)`
    // lowers to a nested pair of loops — the closure param is the outer
    // loop var, the user pattern binds each inner element. Unlabeled
    // `break` in the body is retargeted onto the synthesized outer label
    // (exits the WHOLE flat sequence); unlabeled `continue` needs no
    // rewrite (next flat element IS the inner loop's next iteration); a
    // `break` inside a nested user loop stays local to it. Inner and
    // outer may each carry their own fused adaptor chains; heap (String)
    // inner elements work. Must match the interpreter.
    if let Some(out) = run_program(
            "fn main() {\n\
                 let vv: Vec[Vec[i64]] = [[1, 2], [3], [4, 5]];\n\
                 let mut c: i64 = 0;\n\
                 for y in vv.iter().flat_map(|row| row.iter()) { c = c + y; }\n\
                 println(c);\n\
                 let mut d: i64 = 0;\n\
                 for y in vv.iter().flat_map(|row| row.iter()) {\n\
                     if y == 3 { break; }\n\
                     d = d + y;\n\
                 }\n\
                 println(d);\n\
                 let mut e: i64 = 0;\n\
                 for y in vv.iter().flat_map(|row| row.iter()) {\n\
                     if y % 2 == 1 { continue; }\n\
                     e = e + y;\n\
                 }\n\
                 println(e);\n\
                 let mut f: i64 = 0;\n\
                 for y in vv.iter().flat_map(|row| row.iter()) {\n\
                     for k in 0..5 {\n\
                         if k == 2 { break; }\n\
                         f = f + 1;\n\
                     }\n\
                     f = f + y;\n\
                 }\n\
                 println(f);\n\
                 let mut g: i64 = 0;\n\
                 for y in vv.iter().flat_map(|row| row.iter().map(|v| v * 2)) { g = g + y; }\n\
                 println(g);\n\
                 let words: Vec[Vec[String]] = [[f\"a\", f\"bb\"], [f\"ccc\"]];\n\
                 let mut h: i64 = 0;\n\
                 for w in words.iter().flat_map(|ws| ws.iter()) { h = h + w.len(); }\n\
                 println(h);\n\
                 let mut i2: i64 = 0;\n\
                 for y in vv.iter().filter(|row| row.len() > 1).flat_map(|row| row.iter()) { i2 = i2 + y; }\n\
                 println(i2);\n\
                 let mut j: i64 = 0;\n\
                 for y in vv.iter().flat_map(|row| row.iter().take_while(|v| v < 5)) { j = j + y; }\n\
                 println(j);\n\
             }",
        ) {
            assert_eq!(out, "15\n3\n6\n25\n30\n6\n12\n10\n");
        }
}

#[test]
fn test_e2e_for_cycle() {
    // B-2026-07-14-8 (cycle leg): `for x in src.cycle()` lowers to a
    // restart loop around one full pass of the source chain, with a
    // yielded-flag guard so a pass that yields NOTHING ends the loop
    // (matching the interpreter's empty-template stop — an empty or
    // fully-filtered source must not spin forever). Unlabeled `break`
    // exits the whole cycle via the retargeted outer label; per-pass
    // adaptor state (`take(2).cycle()`) resets each restart like the
    // interpreter's fresh template clone. Must match the interpreter —
    // which itself needed the LAZY for-loop pull (B-2026-07-14-22) to
    // run these at all.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let v: Vec[i64] = [1, 2, 3];\n\
                 let mut n: i64 = 0;\n\
                 let mut a: i64 = 0;\n\
                 for x in v.iter().cycle() {\n\
                     if n == 7 { break; }\n\
                     n = n + 1;\n\
                     a = a + x;\n\
                 }\n\
                 println(a);\n\
                 let es: Vec[i64] = [];\n\
                 let mut b: i64 = 0;\n\
                 for x in es.iter().cycle() { b = b + x; }\n\
                 println(b);\n\
                 let mut c: i64 = 0;\n\
                 for x in v.iter().filter(|w| w > 100).cycle() { c = c + x; }\n\
                 println(c);\n\
                 let mut m: i64 = 0;\n\
                 let mut d: i64 = 0;\n\
                 for x in v.iter().take(2).cycle() {\n\
                     if m == 5 { break; }\n\
                     m = m + 1;\n\
                     d = d + x;\n\
                 }\n\
                 println(d);\n\
                 let ws: Vec[String] = [f\"ab\", f\"c\"];\n\
                 let mut h: i64 = 0;\n\
                 let mut cnt: i64 = 0;\n\
                 for w in ws.iter().cycle() {\n\
                     if cnt == 5 { break; }\n\
                     cnt = cnt + 1;\n\
                     h = h + w.len();\n\
                 }\n\
                 println(h);\n\
             }",
    ) {
        assert_eq!(out, "13\n0\n0\n7\n8\n");
    }
}

#[test]
fn test_e2e_for_adaptor_tail_complete() {
    // B-2026-07-14-8 (tail completion): peekable as identity (for-loops,
    // mid-chain, terminals); adaptors chained AFTER structural adaptors
    // (flat_map/cycle/windows/chunks/scan/chunk_by as fused-chain BASES);
    // labeled flat_map/cycle (label on the outer loop, labeled continue
    // renamed to the inner); chunk_by (identity + computed keys); HEAP
    // (String) elements through zip/chain/skip-take windows and the
    // windows/chunks/chunk_by group builds (group pushes clone; sources
    // survive). Must match the interpreter.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let xs: Vec[i64] = [1, 2, 3, 4, 5];\n\
                 let vv: Vec[Vec[i64]] = [[1, 2], [3], [4, 5]];\n\
                 let mut a: i64 = 0;\n\
                 for x in xs.iter().peekable() { a = a + x; }\n\
                 println(a);\n\
                 println(xs.iter().peekable().filter(|v| v % 2 == 1).sum());\n\
                 let mut b: i64 = 0;\n\
                 for y in vv.iter().flat_map(|row| row.iter()).filter(|v| v > 2) { b = b + y; }\n\
                 println(b);\n\
                 let mut c: i64 = 0;\n\
                 for x in xs.iter().cycle().take(7) { c = c + x; }\n\
                 println(c);\n\
                 let mut d: i64 = 0;\n\
                 for w in xs.iter().windows(2).skip(2) { d = d + w[0]; }\n\
                 println(d);\n\
                 let mut g1: i64 = 0;\n\
                 out1: for y in vv.iter().flat_map(|row| row.iter()) {\n\
                     g1 = g1 + y;\n\
                     if y == 3 { break out1 (); }\n\
                 }\n\
                 println(g1);\n\
                 let mut h: i64 = 0;\n\
                 out2: for y in vv.iter().flat_map(|row| row.iter()) {\n\
                     if y % 2 == 0 { continue out2; }\n\
                     h = h + y;\n\
                 }\n\
                 println(h);\n\
                 let mut m: i64 = 0;\n\
                 let mut pulls: i64 = 0;\n\
                 out4: for x in xs.iter().cycle() {\n\
                     pulls = pulls + 1;\n\
                     if pulls == 12 { break; }\n\
                     if x % 2 == 0 { continue out4; }\n\
                     m = m + x;\n\
                 }\n\
                 println(m);\n\
                 let rs: Vec[i64] = [1, 1, 2, 2, 2, 3];\n\
                 for g in rs.iter().chunk_by(|x| x) { println(f\"{g.len()}:{g[0]}\"); }\n\
                 let mut lens: i64 = 0;\n\
                 for g in rs.iter().chunk_by(|x| x % 2) { lens = lens * 10 + g.len(); }\n\
                 println(lens);\n\
                 let mut w2: i64 = 0;\n\
                 for g in rs.iter().chunk_by(|x| x).filter(|q| q.len() >= 2) { w2 = w2 + g[0]; }\n\
                 println(w2);\n\
                 let names: Vec[String] = [f\"ant\", f\"bee\", f\"cat\", f\"dog\"];\n\
                 let tags: Vec[String] = [f\"x\", f\"yy\", f\"zzz\"];\n\
                 let mut za: i64 = 0;\n\
                 for (n, t) in names.iter().zip(tags.iter()) { za = za + n.len() + t.len(); }\n\
                 println(za);\n\
                 let mut ch: String = f\"\";\n\
                 for w in names.iter().chain(tags.iter()) {\n\
                     if w == \"cat\" { break; }\n\
                     ch = ch + w;\n\
                 }\n\
                 println(ch);\n\
                 let mut sk: String = f\"\";\n\
                 for w in names.iter().skip(1).take(2) { sk = sk + w + f\"|\"; }\n\
                 println(sk);\n\
                 let hs: Vec[String] = [f\"aa\", f\"bb\", f\"aa\", f\"cc\", f\"cc\"];\n\
                 for w in hs.iter().windows(2) { println(f\"{w[0]}~{w[1]}\"); }\n\
                 let mut hc: i64 = 0;\n\
                 for c2 in hs.iter().chunks(2) { hc = hc + c2.len(); }\n\
                 println(hc);\n\
                 for g in hs.iter().chunk_by(|x| x) { println(f\"{g.len()}:{g[0]}\"); }\n\
                 println(hs.len());\n\
             }",
    ) {
        assert_eq!(
                out,
                "15\n9\n12\n18\n7\n6\n9\n19\n2:1\n3:2\n1:3\n231\n3\n15\nantbee\nbee|cat|\naa~bb\nbb~aa\naa~cc\ncc~cc\n5\n1:aa\n1:bb\n1:aa\n2:cc\n5\n"
            );
    }
}

#[test]
fn test_e2e_for_windows_chunks() {
    // B-2026-07-14-8 (windows/chunks legs): both adaptors yield a FRESH
    // `Vec[T]` per group (matching the interpreter's per-pull allocation),
    // materialized by an index push-loop over a named scalar-element Vec.
    // Clamp parity: chunks(k<1) clamps to 1; windows' ITERATOR variant
    // clamps k to >= 1 too (windows(0) == windows(1) — the interp clamps
    // n.max(1) at dispatch), and k > len yields nothing. User
    // break/continue/labels bind the outer loop naturally. Must match the
    // interpreter.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let v: Vec[i64] = [1, 2, 3, 4, 5];\n\
                 for w in v.iter().windows(2) { println(w[0] + w[1]); }\n\
                 let mut n: i64 = 0;\n\
                 let mut t: i64 = 0;\n\
                 for w in v.iter().windows(3) {\n\
                     n = n + 1;\n\
                     for y in w { t = t + y; }\n\
                 }\n\
                 println(n);\n\
                 println(t);\n\
                 let mut a: i64 = 0;\n\
                 for w in v.iter().windows(9) { a = a + 1; }\n\
                 println(a);\n\
                 let mut b: i64 = 0;\n\
                 for w in v.iter().windows(0) { b = b + 1; }\n\
                 println(b);\n\
                 for c in v.iter().chunks(2) {\n\
                     let mut s: i64 = 0;\n\
                     for y in c { s = s + y; }\n\
                     println(f\"{c.len()}:{s}\");\n\
                 }\n\
                 let mut d: i64 = 0;\n\
                 for c in v.iter().chunks(0) { d = d + c.len(); }\n\
                 println(d);\n\
                 let mut e: i64 = 0;\n\
                 for w in v.iter().windows(2) {\n\
                     if w[0] == 3 { break; }\n\
                     e = e + w[1];\n\
                 }\n\
                 println(e);\n\
                 let mut f2: i64 = 0;\n\
                 for c in v.iter().chunks(2) {\n\
                     if c.len() == 1 { continue; }\n\
                     f2 = f2 + c[0];\n\
                 }\n\
                 println(f2);\n\
             }",
    ) {
        assert_eq!(out, "3\n5\n7\n9\n3\n27\n0\n5\n2:3\n2:7\n1:5\n5\n5\n4\n");
    }
}

#[test]
fn test_e2e_for_scan() {
    // B-2026-07-14-8 (scan leg): `for out in src.scan(init, |acc, x|
    // Some((new, out)))` lowers to a single accumulator loop (the
    // scan-collect desugar with the user body as the sink). One loop →
    // user break/continue/labels work naturally; the accumulator
    // advances BEFORE the body, so `continue` doesn't desync state.
    // Direct-Some bodies only (the early-stop conditional-None form
    // bails loud); the source may be a fused chain. Must match the
    // interpreter.
    if let Some(out) = run_program(
            "fn main() {\n\
                 let v: Vec[i64] = [1, 2, 3, 4];\n\
                 for s in v.iter().scan(0, |acc, x| Some((acc + x, acc + x))) { println(s); }\n\
                 let mut t: i64 = 0;\n\
                 for y in v.iter().scan(1, |acc, x| Some((acc * 2, acc * x))) { t = t + y; }\n\
                 println(t);\n\
                 let mut u: i64 = 0;\n\
                 for s in v.iter().scan(0, |acc, x| Some((acc + x, acc + x))) {\n\
                     if s > 5 { break; }\n\
                     u = u + s;\n\
                 }\n\
                 println(u);\n\
                 let mut w: i64 = 0;\n\
                 for s in v.iter().scan(0, |acc, x| Some((acc + x, acc + x))) {\n\
                     if s % 2 == 1 { continue; }\n\
                     w = w + s;\n\
                 }\n\
                 println(w);\n\
                 let mut z: i64 = 0;\n\
                 for s in v.iter().filter(|q| q % 2 == 0).scan(0, |acc, x| Some((acc + x, acc + x))) { z = z + s; }\n\
                 println(z);\n\
             }",
        ) {
            assert_eq!(out, "1\n3\n6\n10\n49\n4\n16\n8\n");
        }
}

#[test]
fn test_e2e_for_step_by_inspect_take_skip_fused() {
    // B-2026-07-14-8 (count + inspect legs): `take`/`skip`/`step_by`/
    // `inspect` are fused-chain steps in the SHARED peel, so for-loops and
    // every fused terminal handle them in any composition with
    // map/filter/take_while/skip_while. Counter arithmetic mirrors the
    // collect engine; negative take/skip counts clamp to 0 and step_by
    // strides clamp to 1, matching the interpreter (the skip/take
    // index-window path gets the same signed clamp; the collect engine's
    // step_by gets the same >=1 clamp — `% 0` used to trap). Must match
    // the interpreter.
    if let Some(out) = run_program(
            "fn main() {\n\
                 let v: Vec[i64] = [1, 2, 3, 10, 4, 5, 11, 6];\n\
                 let mut a: i64 = 0;\n\
                 for x in v.iter().step_by(2) { a = a + x; }\n\
                 println(a);\n\
                 let mut b: i64 = 0;\n\
                 for x in v.iter().filter(|w| w % 2 == 0).step_by(2) { b = b + x; }\n\
                 println(b);\n\
                 let mut c: i64 = 0;\n\
                 for x in v.iter().step_by(0) { c = c + x; }\n\
                 println(c);\n\
                 let mut seen: i64 = 0;\n\
                 let mut d: i64 = 0;\n\
                 for x in v.iter().inspect(|w| { seen = seen + 1; }).filter(|w| w > 5) { d = d + x; }\n\
                 println(seen);\n\
                 println(d);\n\
                 let mut e: i64 = 0;\n\
                 for x in v.iter().skip(1).step_by(2).take(3) { e = e + x; }\n\
                 println(e);\n\
                 let mut f: i64 = 0;\n\
                 for y in v.iter().map(|w| w).take(0 - 1) { f = f + y; }\n\
                 println(f);\n\
                 let mut g: i64 = 0;\n\
                 for y in v.iter().map(|w| w).skip(0 - 2) { g = g + y; }\n\
                 println(g);\n\
                 let mut h: i64 = 0;\n\
                 for x in v.iter().take(0 - 1) { h = h + x; }\n\
                 println(h);\n\
                 let mut i2: i64 = 0;\n\
                 for x in v.iter().skip(0 - 1) { i2 = i2 + x; }\n\
                 println(i2);\n\
                 let mut r: i64 = 0;\n\
                 for x in (0..100).take(4) { r = r + x; }\n\
                 println(r);\n\
                 println(v.iter().skip(5).sum());\n\
                 println(v.iter().take(3).fold(1, |p, q| p * q));\n\
                 println(v.iter().step_by(3).count());\n\
                 println(v.iter().skip(6).all(|w| w > 5));\n\
                 let sv: Vec[i64] = v.iter().step_by(0).collect();\n\
                 println(sv.len());\n\
                 let names: Vec[String] = [f\"ant\", f\"bee\", f\"cat\", f\"dog\", f\"elk\"];\n\
                 let mut s: String = f\"\";\n\
                 for n in names.iter().step_by(2) { s = s + n + f\",\"; }\n\
                 println(s);\n\
                 let mut r2: i64 = 0;\n\
                 for y in (0..10).step_by(4).map(|w| w + 1) { r2 = r2 + y; }\n\
                 println(r2);\n\
             }",
        ) {
            assert_eq!(
                out,
                "19\n6\n42\n8\n27\n17\n0\n42\n0\n42\n6\n22\n6\n3\ntrue\n8\nant,cat,elk,\n15\n"
            );
        }
}

#[test]
fn test_e2e_for_take_while_skip_while() {
    // B-2026-07-14-8 (predicate legs): `take_while`/`skip_while` are fused
    // into the shared map/filter chain desugar — `take_while` compiles to
    // `if pred { rest } else { break }`, `skip_while` to a pre-loop latch
    // flag with `if !flag and pred {} else { flag = true; rest }`. Works in
    // for-loops (any composition with map/filter), on heap (String)
    // elements, and through every fused terminal (fold/sum/count/reduce/
    // for_each/any/all). Must match the interpreter.
    if let Some(out) = run_program(
            "fn main() {\n\
                 let v: Vec[i64] = [1, 2, 3, 10, 4, 5, 11, 6];\n\
                 let mut a: i64 = 0;\n\
                 for x in v.iter().take_while(|w| w < 10) { a = a + x; }\n\
                 println(a);\n\
                 let mut b: i64 = 0;\n\
                 for x in v.iter().skip_while(|w| w < 10) { b = b + x; }\n\
                 println(b);\n\
                 let mut c: i64 = 0;\n\
                 for y in v.iter().skip_while(|w| w < 10).map(|w| w * 2).take_while(|z| z < 21) { c = c + y; }\n\
                 println(c);\n\
                 let mut e: i64 = 0;\n\
                 for x in v.iter().take_while(|w| w < 10) {\n\
                     if x == 3 { break; }\n\
                     e = e + x;\n\
                 }\n\
                 println(e);\n\
                 let mut o: i64 = 0;\n\
                 for x in v.iter().skip_while(|w| w < 3).skip_while(|w| w < 5) { o = o + x; }\n\
                 println(o);\n\
                 println(v.iter().take_while(|w| w < 10).fold(1, |acc, x| acc * x));\n\
                 println(v.iter().skip_while(|w| w < 10).sum());\n\
                 println(v.iter().take_while(|w| w < 10).filter(|w| w % 2 == 1).count());\n\
                 println(v.iter().skip_while(|w| w < 10).any(|w| w == 6));\n\
                 println(v.iter().take_while(|w| w < 10).all(|w| w % 2 == 0));\n\
                 let names: Vec[String] = [f\"ant\", f\"bee\", f\"zebra\", f\"dog\"];\n\
                 let mut picked: String = f\"\";\n\
                 for n in names.iter().take_while(|s| s != \"zebra\") { picked = picked + n + f\",\"; }\n\
                 println(picked);\n\
                 let mut rest: String = f\"\";\n\
                 for n in names.iter().skip_while(|s| s != \"zebra\") { rest = rest + n + f\",\"; }\n\
                 println(rest);\n\
                 let mut q: i64 = 0;\n\
                 for x in v.iter().take_while(|w| w < 0) { q = q + x; }\n\
                 println(q);\n\
             }",
        ) {
            assert_eq!(
                out,
                "6\n36\n38\n3\n36\n6\n36\n2\ntrue\nfalse\nant,bee,\nzebra,dog,\n0\n"
            );
        }
}

#[test]
fn test_e2e_for_enumerate_single_var() {
    // B-2026-07-14-8 (enumerate single-var leg): `for p in
    // xs.iter().enumerate()` binds `p` to a `{i64, T}` tuple struct per
    // iteration, so `p.0` (index) / `p.1` (element) extract via the normal
    // TupleIndex path. Scalar elements; the 2-tuple destructure form and
    // heap-element shapes are unaffected (destructure keeps its own peel;
    // heap bails loud).
    if let Some(out) = run_program(
        "fn main() {\n\
                 let a: Vec[i64] = [10, 20, 30];\n\
                 let mut s: i64 = 0;\n\
                 for p in a.iter().enumerate() {\n\
                     s = s + p.0 * p.1;\n\
                 }\n\
                 println(s);\n\
                 let mut t: i64 = 0;\n\
                 for (i, x) in a.iter().enumerate() {\n\
                     t = t + i + x;\n\
                 }\n\
                 println(t);\n\
             }",
    ) {
        assert_eq!(out, "80\n63\n");
    }
}

#[test]
fn test_e2e_for_zip_two_vecs() {
    // B-2026-07-14-8 (zip leg): `for (a, b) in xs.iter().zip(ys.iter())`
    // lowers to a lockstep index loop over `0..min(lenA, lenB)` for two
    // named scalar-element Vecs. Covers i64 and f64, and the shorter
    // source on either side. Heap elements / other shapes still bail loud.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let a: Vec[i64] = [1, 2, 3, 4];\n\
                 let b: Vec[i64] = [10, 20, 30];\n\
                 let mut s: i64 = 0;\n\
                 for (x, y) in a.iter().zip(b.iter()) {\n\
                     s = s + x * y;\n\
                 }\n\
                 println(s);\n\
                 let c: Vec[f64] = [0.5, 1.5];\n\
                 let d: Vec[f64] = [2.0, 4.0, 8.0];\n\
                 let mut fs = 0.0;\n\
                 for (p, q) in c.iter().zip(d.iter()) {\n\
                     fs = fs + p * q;\n\
                 }\n\
                 println(fs);\n\
             }",
    ) {
        assert_eq!(out, "140\n7\n");
    }
}

#[test]
fn test_e2e_for_zip_single_var() {
    // B-2026-07-15-10: `for pair in xs.iter().zip(ys.iter()) { pair.0 … }`
    // — a single-binding (non-destructure) zip over two named scalar Vecs
    // binds the whole `(EA, EB)` tuple, the body reads `.0`/`.1`. Covers the
    // shorter-source min-length stop.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let a: Vec[i64] = [1, 2, 3, 4];\n\
                 let b: Vec[i64] = [10, 20, 30];\n\
                 let mut s: i64 = 0;\n\
                 for pair in a.iter().zip(b.iter()) {\n\
                     s = s + pair.0 * pair.1;\n\
                 }\n\
                 println(s);\n\
             }",
    ) {
        // (1*10)+(2*20)+(3*30) = 140 (b is shorter, len 3)
        assert_eq!(out, "140\n");
    }
}

#[test]
fn test_e2e_script_mode_top_level_statements() {
    // Script mode (design.md § Script mode, phase-8 Q7): a main-less
    // file of top-level statements compiles via the parser-synthesized
    // unit `fn main()` — items hoist, statements form the body in file
    // order. Build must match the interpreter (which runs the same
    // synthesized main).
    if let Some(out) = run_program(
        "fn double(x: i64) -> i64 { x * 2 }\n\
             let y = double(21);\n\
             println(y);\n\
             let msg: String = \"script\".to_string();\n\
             println(msg);\n",
    ) {
        assert_eq!(out, "42\nscript\n");
    }
}

#[test]
fn test_e2e_diverging_branch_in_value_position() {
    // Regression for self-hosting #12: a tail-less block whose body
    // diverges (`{ return e; }`) now types as `Never` rather than
    // `()`, so it can share an `if`/`match` value-expression with a
    // real value arm. This test exercises the resulting codegen path:
    // a diverging else branch must NOT contribute to the merge phi
    // (it terminates the block), while the value branch flows through.
    // Both the taken-value path and the taken-diverging path must
    // produce correct output.
    if let Some(out) = run_program(
        "fn classify(n: i64) -> i64 {\n\
                 // value path AND diverging path share the if-expression\n\
                 let label = if n >= 0 { 'P' } else { return -1; };\n\
                 label as i64\n\
             }\n\
             fn first_or_bail(v: ref Vec[i64]) -> i64 {\n\
                 // match-arm-block diverging variant\n\
                 let x = match v.len() { 0 => { return -7; }, _ => v[0] };\n\
                 x * 2\n\
             }\n\
             fn main() {\n\
                 println(classify(5).to_string());   // 'P' = 80\n\
                 println(classify(-3).to_string());  // -1 via diverging else\n\
                 let mut a: Vec[i64] = Vec.new();\n\
                 a.push(21);\n\
                 println(first_or_bail(a).to_string()); // 42\n\
                 let empty: Vec[i64] = Vec.new();\n\
                 println(first_or_bail(empty).to_string()); // -7 via diverging arm\n\
             }",
    ) {
        assert_eq!(out, "80\n-1\n42\n-7\n");
    }
}

/// phase-7 — an explicit valueless `return;` reachable in `main` must
/// emit `ret i32 0` (main lowers to a C-ABI `i32 main()`), not
/// `ret void`. Before the fix this failed module verification
/// ("ret void / i32"); the implicit end-of-main already returned 0,
/// so only the *explicit* `return;` path was broken. Exercises the
/// nested (while + if) reachable case, and asserts the early return
/// actually stops execution.
#[test]
fn test_e2e_explicit_return_in_main_exits_zero() {
    let out = run_program(
        "fn main() {\n\
             \x20   let mut i = 0;\n\
             \x20   while i < 5 {\n\
             \x20       if i == 2 { println(99); return; }\n\
             \x20       println(i);\n\
             \x20       i = i + 1;\n\
             \x20   }\n\
             \x20   println(0 - 1);\n\
             }",
    );
    if let Some(out) = out {
        // i=0,1 printed; i=2 prints 99 then returns; the post-loop
        // `println` never runs.
        assert_eq!(out.trim(), "0\n1\n99");
    }
}

/// gap-d: a diverging arm (`unreachable()`) must not collapse an
/// `if`-expression's value to the const-0 placeholder — the live arm's
/// value is the result. Before the fix this printed `0`.
#[test]
fn test_e2e_diverge_if_branch_yields_live_value() {
    let out = run_program(
        "fn pick(n: i64) -> i64 { if n > 0 { n } else { unreachable() } }\n\
             fn main() { println(pick(7)); }",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "7");
    }
}

#[test]
fn test_ir_ptr_with_addr_mut_compiles() {
    let ir = ir_for(
        "fn caller(p: *mut i64, a: usize) -> *mut i64 { ptr.with_addr_mut(p, a) } fn main() {}",
    );
    assert!(
        ir.contains("@caller"),
        "caller fn should be emitted; got IR:\n{ir}"
    );
}

#[test]
fn test_ir_ptr_expose_mut_compiles() {
    let ir = ir_for("fn caller(p: *mut i64) -> usize { ptr.expose_mut(p) } fn main() {}");
    assert!(
        ir.contains("@caller"),
        "caller fn should be emitted; got IR:\n{ir}"
    );
}

#[test]
fn test_ir_ptr_container_of_mut_compiles() {
    let ir = ir_for(
        "struct Inner { x: i32, y: i32 } \
             struct Outer { a: i32, inner: Inner } \
             fn recover(fp: *mut i32) -> *mut Outer { \
                 unsafe { ptr.container_of_mut(fp, offset_of[Outer](inner.y)) } \
             } \
             fn main() {}",
    );
    assert!(
        ir.contains("@recover"),
        "recover fn should be emitted; got IR:\n{ir}"
    );
}

#[test]
fn test_ir_ptr_null_mut_compiles() {
    let ir = ir_for("fn caller() -> *mut i64 { ptr.null_mut() } fn main() {}");
    assert!(
        ir.contains("@caller"),
        "caller fn should be emitted; got IR:\n{ir}"
    );
}

#[test]
fn test_e2e_small_counted_loop_full_unroll_is_sound() {
    // B-2026-06-17-7: a small, constant-upper-bounded `while` loop with a
    // constant-step induction var now carries `llvm.loop.unroll.full` so
    // LLVM fully unrolls it (matching rustc; ~1.34x on kata:37). This
    // guards SOUNDNESS — the unrolled codegen must produce byte-identical
    // results to the rolled form. The body mirrors kata:37's candidate
    // loop: a `while d <= 9` with a `1 << d` shift, a conditional update
    // (so a mis-unroll that dropped/duplicated an iteration would change
    // the order-dependent accumulator), nested in an outer `while i < 5`
    // counted loop (also eligible). Pinned against the interpreter.
    let src = r#"
fn f(start: i64) -> i64 {
    let mut acc = 0i64;
    let mut d = 1i64;
    while d <= 9i64 {
        let bit = 1i64 << d;
        if (acc & 1i64) == 0i64 {
            acc = acc + bit * d + start;
        }
        d = d + 1i64;
    }
    acc
}
fn main() {
    let mut total = 0i64;
    let mut i = 0i64;
    while i < 5i64 {
        total = total + f(i);
        i = i + 1i64;
    }
    println(f"{total}");
}
"#;
    let out = run_program(src);
    if let Some(out) = out {
        assert_eq!(out, "24644\n");
    }
}

#[test]
fn test_ir_ptr_mut_on_local_compiles() {
    let ir = ir_for("fn main() { let mut x: i32 = 7; let p: *mut i32 = ptr.mut(x); }");
    assert!(
        !ir.contains("method dispatch fell through"),
        "ptr.mut dispatch must not fall through; got IR:\n{ir}"
    );
}

#[test]
fn test_e2e_break_continue() {
    let out = run_program(
        r#"
fn first_multiple_of_3(limit: i64) -> i64 {
    let mut result = 0;
    let mut i = 1;
    while i <= limit {
        if i % 3 == 0 {
            result = i;
            break;
        }
        i = i + 1;
    }
    result
}
fn main() { println(first_multiple_of_3(100)); }
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "3");
    }
}

/// B-2026-06-20-1: a bare named `fn` passed as a first-class `Fn(...)`
/// value. `apply(doubler, 21)` must lower the `Fn(i64)->i64` parameter to
/// the closure fat-pointer ABI and reify the bare fn name into a
/// `{trampoline, null-env}` fat pointer, so the higher-order call runs.
/// Previously the `Fn`-typed param lowered to `i64` while the bare fn name
/// lowered to a raw `ptr`, failing LLVM module verification.
/// B-2026-06-21-2: a `fn` body that *returns* a first-class `Fn(...)` value
/// (`fn pick() -> Fn(i64)->i64 { doubler }`). The bare fn name in return-tail
/// position now lowers to a closure fat pointer (the free-fn-as-value source
/// arm), matching the `{ptr,ptr}` return slot; `let f = pick()` registers
/// `f` in `closure_fn_types` from `pick`'s declared `Fn(...)` return type, so
/// the direct call through it works. Before: `ret ptr @doubler` vs `{ptr,ptr}`
/// verifier error.
#[test]
fn fn_value_returned_then_called() {
    let out = run_program(
        "fn doubler(n: i64) -> i64 { n * 2i64 }\n\
             fn pick() -> Fn(i64) -> i64 { doubler }\n\
             fn main() { let f = pick(); println(f\"{f(21i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("42\n"));
}

/// A returned fn value passed straight into a `Fn(...)` parameter — the call
/// result is already a fat pointer, so it flows through with no reify.
#[test]
fn fn_value_returned_passed_directly() {
    let out = run_program(
        "fn doubler(n: i64) -> i64 { n * 2i64 }\n\
             fn pick() -> Fn(i64) -> i64 { doubler }\n\
             fn apply(f: Fn(i64) -> i64, x: i64) -> i64 { f(x) }\n\
             fn main() { println(f\"{apply(pick(), 21i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("42\n"));
}

#[test]
fn test_e2e_swap_pairs_recursive_mixed_returns() {
    // Recursive kata-#24 form: `first.next = swap_pairs(second.next)`
    // consumes a fresh +1 chain while `second.next = Some(first)`
    // stores an alias that must retain — both store flavors on
    // re-parented nodes, with the base case returning the bare
    // `head` param (the per-branch-compensation mix from kata #21's
    // fca1e3ea). Pre-fix: SIGBUS.
    let out = run_program(
        r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
fn build(n: i64) -> Option[ListNode] {
    let head = ListNode { val: 1, next: None };
    let mut tail = head;
    for i in 2..n + 1 {
        let node = ListNode { val: i, next: None };
        tail.next = Some(node);
        tail = node;
    }
    Some(head)
}
fn swap_pairs(head: Option[ListNode]) -> Option[ListNode] {
    if let Some(first) = head {
        if let Some(second) = first.next {
            first.next = swap_pairs(second.next);
            second.next = Some(first);
            Some(second)
        } else {
            head
        }
    } else {
        None
    }
}
fn main() {
    let mut cur = swap_pairs(build(6));
    let mut sum = 0;
    let mut acc = 0;
    loop {
        match cur {
            Some(node) => {
                acc = acc * 10 + node.val;
                sum = sum + node.val;
                cur = node.next;
            }
            None => break,
        }
    }
    println(acc);
    println(sum);
}
"#,
    );
    if let Some(out) = out {
        let got: Vec<&str> = out.trim().lines().collect();
        // [2, 1, 4, 3, 6, 5] → digits 214365, sum 21.
        assert_eq!(got, ["214365", "21"]);
    }
}

#[test]
fn test_ir_bug8_assign_from_call_no_double_inc() {
    // Same convention applies to `Assign` targeting a shared local:
    // `x = make()` is rc_dec(old) + store(new), without a receive-
    // side inc on the freshly-transferred ref.  The `mut` rebinding
    // path must emit the dec for the previous value but skip the
    // inc on the new one (whose +1 is delivered by the call return).
    let ir = ir_for(
        r#"
shared struct S { val: i64 }
fn make() -> S {
    let s = S { val: 42 };
    s
}
fn rebind() {
    let mut x = make();
    x = make();
}
"#,
    );
    // make() body (defined once in the module, called twice from
    // rebind()): 1 `add i64 %rc` (move-out inc) + 1 `sub i64 %rc`
    // (scope-exit dec).
    // rebind() body: 0 receive incs across both call sites +
    // 1 dec on old `x` at the reassign + 1 scope-exit dec on
    // the final `x` = 2 decs.
    // Module total: inc=1, dec=3.
    let inc_count = ir.matches("add i64 %rc").count();
    let dec_count = ir.matches("sub i64 %rc").count();
    assert_eq!(
        inc_count, 1,
        "Assign-from-Call must not emit a receive-side rc_inc; the only \
             expected inc is the callee-side move-out inside `make`. Found {} \
             `add i64 %rc` ops in:\n{}",
        inc_count, ir
    );
    assert_eq!(
        dec_count, 3,
        "expected 3 rc_decs (callee scope-exit in make + reassign-old + \
             caller scope-exit); found {} in:\n{}",
        dec_count, ir
    );
}

#[test]
fn test_ir_bug8_if_tail_call_rhs_no_double_inc() {
    // Branch-shape extension of the bug #8 receive-side fix
    // (5323d5d). The outer `ExprKind` of the RHS is `If`, not
    // `Call`, but every branch tail IS a `Call` returning a
    // freshly-transferred +1. Before this fix `is_fresh_construction`
    // only matched the outer kind, so the receive site emitted
    // an extra `add i64 %rc` and the refcount on the bound `x`
    // landed at 2 — leaking one ref per crossing on whichever
    // branch executed at runtime.
    let ir = ir_for(
        r#"
shared struct S { val: i64 }
fn make_a() -> S { let s = S { val: 1 }; s }
fn make_b() -> S { let s = S { val: 2 }; s }
fn use_it(cond: bool) {
    let x = if cond { make_a() } else { make_b() };
}
"#,
    );
    let inc_count = ir.matches("add i64 %rc").count();
    let dec_count = ir.matches("sub i64 %rc").count();
    // Expected incs: one move-out in each of `make_a` / `make_b` = 2.
    // Expected decs: scope-exit in each of `make_a` / `make_b` (2)
    // + caller scope-exit on `x` (1) = 3. The receive site must
    // emit zero incs on the if-tail.
    assert_eq!(
        inc_count, 2,
        "if-tail Call RHS must not emit a receive-side rc_inc; \
             expected 2 callee-side move-out incs only. Found {} \
             `add i64 %rc` ops in:\n{}",
        inc_count, ir
    );
    assert_eq!(
        dec_count, 3,
        "expected 3 rc_decs (2 callee scope-exit + 1 caller \
             scope-exit on x); found {} in:\n{}",
        dec_count, ir
    );
}

#[test]
fn test_ir_let_rebind_tcp_listener_suppresses_double_close() {
    // Production motivation: `let l2 = l1` for TcpListener should
    // close the fd exactly once at scope exit, not twice. The IR
    // pins the same shape: one `@karac_drop_TcpListener` call.
    let ir = ir_for(
        r#"
fn main() {
    let l1 = TcpListener.bind("127.0.0.1:0").unwrap();
    let l2 = l1;
    println(l2.fd);
}
"#,
    );
    let main_body = function_body(&ir, "main").unwrap_or_else(|| {
        panic!("main body not found in IR:\n{}", ir);
    });
    let count = main_body
        .matches("call void @karac_drop_TcpListener(")
        .count();
    assert_eq!(
        count, 1,
        "expected exactly ONE `@karac_drop_TcpListener` call in main \
             after `let l2 = l1` (source `l1` move-suppressed); got {} \
             calls; body was:\n{}",
        count, main_body
    );
}

#[test]
fn test_ir_descending_loop_bce_skip_wiring() {
    // B-2026-07-17-1: the descending-loop skip proves `k < row.len()` for
    // the in-place `row[k] = row[k] + row[k-1]` update, so codegen emits a
    // LOWER-only bounds check (the signed `< 0` half, which LLVM folds from
    // the `k >= 1` guard) — NOT the combined unsigned check. The combined
    // path produces a `v.st.ok` block; the lower-only path produces
    // `v.st.oob.neg` / `v.st.lower.ok`. Assert the wiring pushed the
    // UpperBound (skip fired): the store keeps only its lower half.
    let ir = ir_for(
        r#"
fn get_row(row_index: i64) -> Vec[i64] {
    let mut row: Vec[i64] = Vec.new();
    let mut j = 0i64;
    while j <= row_index { row.push(1i64); j = j + 1i64; }
    let mut i = 2i64;
    while i <= row_index {
        let mut k = i - 1i64;
        while k >= 1i64 { row[k] = row[k] + row[k - 1i64]; k = k - 1i64; }
        i = i + 1i64;
    }
    row
}
"#,
    );
    assert!(
        ir.contains("v.st.oob.neg"),
        "expected the descending store to keep its lower-half check, got:\n{ir}"
    );
    assert!(
        !ir.contains("v.st.ok"),
        "expected NO combined store bounds check (skip should elide the \
             upper half), but found a `v.st.ok` block:\n{ir}"
    );
}

#[test]
fn test_e2e_for_in_indexed_iter() {
    // `for p in coll[i].iter()` — the iter peel-off in
    // `compile_for` recurses on the receiver, but for an
    // indexed receiver the recursion would land on an Index
    // expression which falls through to the silent `_ =>` arm.
    // Fix: synthesize a temp identifier for the indexed
    // element and recurse with it, mirroring
    // `compile_nested_index_read`. Pins the kata's
    // `for p in factors[v].iter() { bucket.entry(p)... }` shape.
    let out = run_program(
        r#"
fn main() {
    let mut factors: Vec[Vec[i64]] = Vec.filled(7, Vec.new());
    factors[6].push(2);
    factors[6].push(3);
    let mut sum = 0i64;
    for p in factors[6].iter() {
        sum = sum + p;
    }
    println(sum);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "5");
    }
}

#[test]
fn test_e2e_lexicographic_comparator_with_early_returns() {
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let mut v: Vec[Vec[i64]] = Vec.new();\n\
                     let mut a: Vec[i64] = Vec.new(); a.push(2i64); a.push(6i64);\n\
                     let mut b: Vec[i64] = Vec.new(); b.push(2i64); b.push(2i64);\n\
                     let mut c: Vec[i64] = Vec.new(); c.push(1i64);\n\
                     v.push(a); v.push(b); v.push(c);\n\
                     v.sort_by(|x, y| {\n\
                         let mut i: i64 = 0i64;\n\
                         while i < x.len() and i < y.len() {\n\
                             if x[i] < y[i] { return Ordering.Less; }\n\
                             if x[i] > y[i] { return Ordering.Greater; }\n\
                             i = i + 1i64;\n\
                         }\n\
                         return x.len().cmp(y.len());\n\
                     });\n\
                     println(f\"{v[0][0]} {v[1][0]}{v[1][1]} {v[2][0]}{v[2][1]}\");\n\
                 }"
        )
        .as_deref(),
        Some("1 22 26\n")
    );
}

#[test]
fn test_e2e_explicit_return_in_a_sort_comparator() {
    // 1. Mono path — all-int element, explicit return.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let mut v: Vec[(i64, i64)] = Vec.new();\n\
                     v.push((3i64, 1i64)); v.push((1i64, 2i64));\n\
                     v.sort_by(|x, y| { return x.0.cmp(y.0) });\n\
                     println(f\"{v[0].0} {v[1].0}\");\n\
                 }"
        )
        .as_deref(),
        Some("1 3\n")
    );
    // 2. Thunk path — container element, explicit return.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let mut v: Vec[String] = Vec.new();\n\
                     v.push(f\"ccc\"); v.push(f\"a\"); v.push(f\"bb\");\n\
                     v.sort_by(|x, y| { return x.len().cmp(y.len()) });\n\
                     println(f\"{v[0]} {v[1]} {v[2]}\");\n\
                 }"
        )
        .as_deref(),
        Some("a bb ccc\n")
    );
    // 3. CONTROL — implicit block tail, which always built.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let mut v: Vec[(i64, i64)] = Vec.new();\n\
                     v.push((3i64, 1i64)); v.push((1i64, 2i64));\n\
                     v.sort_by(|x, y| { let d = x.0; d.cmp(y.0) });\n\
                     println(f\"{v[0].0} {v[1].0}\");\n\
                 }"
        )
        .as_deref(),
        Some("1 3\n")
    );
    // 4. CONTROL — if-expression tail.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let mut v: Vec[(i64, i64)] = Vec.new();\n\
                     v.push((3i64, 1i64)); v.push((1i64, 2i64));\n\
                     v.sort_by(|x, y| if x.0 < y.0 { Ordering.Less } else { Ordering.Greater });\n\
                     println(f\"{v[0].0} {v[1].0}\");\n\
                 }"
        )
        .as_deref(),
        Some("1 3\n")
    );
}

#[test]
fn test_ir_plain_reassign_emits_no_assume() {
    // `k = v[i]` is a non-monotone write — scan must poison.
    let ir = ir_for(
        r#"
fn track(v: mut Slice[i64], n: i64) -> i64 {
    let mut k = 1;
    for i in 1..n {
        v[k] = v[i];
        k = v[i];
    }
    k
}
"#,
    );
    assert!(
        !ir.contains("k.mono.fact"),
        "plain reassignment must poison the var, IR:\n{ir}"
    );
}

#[test]
fn test_e2e_swap_chain_reassign_recycles_transparently() {
    // The LBM ping-pong shape: `grid = next` overwrite-frees the old
    // 2 MiB grid (parks it), and the next step's `Vec.with_capacity`
    // allocation takes it straight back from the cache. Values must
    // survive N swap generations byte-exact.
    let src = r#"
fn main() {
    let mut grid: Vec[i64] = Vec.new();
    let mut i: i64 = 0;
    while i < 262144 {
        grid.push(i);
        i = i + 1;
    }
    let mut step: i64 = 0;
    while step < 4 {
        let mut next: Vec[i64] = Vec.with_capacity(262144);
        let mut j: i64 = 0;
        while j < 262144 {
            next.push(grid[j] + 1);
            j = j + 1;
        }
        grid = next;
        step = step + 1;
    }
    let out: i64 = grid[0] + grid[262143];
    println(f"{out}");
}
"#;
    // grid[0] = 0+4, grid[262143] = 262143+4 → 262151.
    assert_eq!(run_program(src).as_deref(), Some("262151\n"));
}

#[test]
fn test_ir_let_else_emits_branch_not_noop() {
    // phase-6-runtime.md line 489: `let … else` lowers to a real branch
    // (match edge binds + falls through, else edge diverges). Regression
    // against the prior silent no-op (fall-through that emitted nothing).
    let src = r#"
fn maybe(empty: bool) -> Option[i64] {
    if empty {
        return Option.None;
    }
    return Option.Some(7_i64);
}

fn main() {
    let Some(x) = maybe(false) else {
        return
    }
    println(f"bound {x}");
}
"#;
    let ir = ir_for(src);
    assert!(
        ir.contains("letelse.match") && ir.contains("letelse.else"),
        "expected let-else branch blocks in IR; got:\n{}",
        ir
    );
}

#[test]
fn test_e2e_let_else_binds_then_else_diverges() {
    // Match edge: bind the payload and continue to the following code.
    // Non-match edge: run the else block, which diverges (`return`) — the
    // bound-name path is skipped, control returns to the caller.
    let src = r#"
fn maybe(empty: bool) -> Option[i64] {
    if empty {
        return Option.None;
    }
    return Option.Some(7_i64);
}

fn run(empty: bool) {
    let Some(x) = maybe(empty) else {
        println("else fired");
        return
    }
    println(f"bound {x}");
}

fn main() {
    run(false);
    run(true);
    println("done");
}
"#;
    if let Some(out) = run_program(src) {
        assert_eq!(out.trim(), "bound 7\nelse fired\ndone");
    }
}

#[test]
fn test_e2e_compound_assignment_int() {
    // `x += y` desugars to `x = x + y` — regression guard for when Step 6
    // operator lowering rewrites BinOp::Add to a trait method call.
    let out = run_program(
        r#"
fn main() {
    let mut x: i64 = 10;
    x += 5;
    println(x);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "15");
    }
}

#[test]
fn test_e2e_into_widening_at_let_annotation() {
    // `let y: i64 = x.into()` lowers to `i64.from(x)` which codegen
    // compiles as a passthrough for numeric widening.
    let out = run_program(
        r#"
fn main() {
    let x: i32 = 42;
    let y: i64 = x.into();
    println(y);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "42");
    }
}

#[test]
fn test_ir_b2_build_loop_is_count_free() {
    // Phase B2: in the canonical append builder, the entire build
    // loop + walk emit ZERO refcount operations. (Under phase D,
    // which this type-pure program also qualifies for, even the
    // rc=1 header store is gone — see the headerless pins below;
    // this test pins the B2 count-op contract by name: the
    // inc/dec helpers emit `rc_inc`/`rc_dec` value names; the b2
    // paths emit none.)
    let ir = ir_for_with_ownership(
        r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
fn build_and_sum(n: i64) -> i64 {
    let dummy = ListNode { val: 0, next: None };
    let mut tail = dummy;
    let mut i = 1;
    while i <= n {
        let node = ListNode { val: i, next: None };
        tail.next = Some(node);
        tail = node;
        i = i + 1;
    }
    let mut sum = 0;
    let mut cur = dummy.next;
    while cur.is_some() {
        let x = cur.unwrap();
        sum = sum + x.val;
        cur = x.next;
    }
    sum
}
fn main() { println(build_and_sum(5)); }
"#,
    );
    let body = function_body(&ir, "build_and_sum").expect("fn body");
    assert!(
        body.contains("b2.link.slot"),
        "the b2 link-store fast path should engage; body:\n{body}"
    );
    assert!(
        !body.contains("rc_inc") && !body.contains("rc_dec"),
        "b2 build/walk must be count-free; body:\n{body}"
    );
    assert!(
        body.contains("cw_loop"),
        "root keeps the B1 free-walk; body:\n{body}"
    );
    assert!(
        !body.contains("rc_cleanup") && !body.contains("opt_rc_cleanup"),
        "no cursor cleanups under b2; body:\n{body}"
    );
}

#[test]
fn test_ir_fresh_return_someroot_builder_is_count_free() {
    // Phase C1b SomeRoot: `Some(head)` at fn tail transfers the
    // whole b2 cluster — the builder emits ZERO count ops and ZERO
    // cleanups (no cw_loop, no decs; the chain leaves at rc==1 per
    // node straight from rc_alloc and the caller's ordinary
    // dec-drop owns it). Allocation stays HEADERED (rc_alloc, not
    // hl_alloc) — the chain crosses the fn boundary.
    let ir = ir_for_with_ownership(
        r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
fn build(n: i64) -> Option[ListNode] {
    let head = ListNode { val: 1, next: None };
    let mut tail = head;
    let mut i = 2;
    while i <= n {
        let node = ListNode { val: i, next: None };
        tail.next = Some(node);
        tail = node;
        i = i + 1;
    }
    Some(head)
}
fn main() {
    let out = build(5);
    if out.is_some() { println(out.unwrap().val); }
}
"#,
    );
    let body = function_body(&ir, "build").expect("fn body");
    assert!(
        !body.contains("rc_inc") && !body.contains("rc_dec"),
        "SomeRoot builder must be count-free; body:\n{body}"
    );
    assert!(
        !body.contains("cw_loop") && !body.contains("rc_cleanup"),
        "SomeRoot root queues no cleanup; body:\n{body}"
    );
    assert!(
        body.contains("rc_alloc") && !body.contains("hl_alloc"),
        "returned chain stays headered; body:\n{body}"
    );
}

#[test]
fn test_ir_fresh_return_rootlink_frees_root_only() {
    // Phase C1b RootLink: `dummy.next` at fn tail transfers the
    // chain; the root header node frees ALONE at scope exit (the
    // FreeSharedElided shape — `elide_free` blocks, no cw_loop)
    // and the tail link load carries no compensating inner inc.
    let ir = ir_for_with_ownership(
        r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
fn build(n: i64) -> Option[ListNode] {
    let dummy = ListNode { val: 0, next: None };
    let mut tail = dummy;
    let mut i = 1;
    while i <= n {
        let node = ListNode { val: i, next: None };
        tail.next = Some(node);
        tail = node;
        i = i + 1;
    }
    dummy.next
}
fn main() {
    let out = build(5);
    if out.is_some() { println(out.unwrap().val); }
}
"#,
    );
    let body = function_body(&ir, "build").expect("fn body");
    assert!(
        !body.contains("rc_inc") && !body.contains("rc_dec"),
        "RootLink builder must be count-free (incl. no tail compensation inc); body:\n{body}"
    );
    assert!(
        body.contains("elide_free") && !body.contains("cw_loop"),
        "RootLink root frees alone (no walk); body:\n{body}"
    );
}

#[test]
fn test_e2e_shadow_if_block_reverts() {
    let out = run_program(
        "fn main() {\n\
             let x = 1;\n\
             if true { let x = 99; println(x.to_string()); }\n\
             println(x.to_string());\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "99\n1");
    }
}

#[test]
fn test_e2e_shadow_while_body_reverts() {
    let out = run_program(
        "fn main() {\n\
             let x = 100;\n\
             let mut i = 0;\n\
             while i < 2 { let x = i; i = i + 1; }\n\
             println(x.to_string());\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "100");
    }
}

#[test]
fn test_e2e_shadow_for_body_reverts() {
    let out = run_program(
        "fn main() {\n\
             let x = 100;\n\
             for i in 0..2 { let x = i * 5; }\n\
             println(x.to_string());\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "100");
    }
}

#[test]
fn test_e2e_shadow_for_loop_var_reverts() {
    // The for-loop INDUCTION variable itself shadowing an outer binding.
    let out = run_program(
        "fn main() {\n\
             let i = 999;\n\
             let mut sum = 0;\n\
             for i in 0..4 { sum = sum + i; }\n\
             println(sum.to_string());\n\
             println(i.to_string());\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "6\n999");
    }
}

#[test]
fn test_e2e_shadow_labeled_block_reverts() {
    // A labeled block is a lexical scope; a body `let` shadowing an outer
    // binding must not leak past `break label`.
    let out = run_program(
        "fn main() {\n\
             let x = 1;\n\
             let y = lbl: { let x = 50; break lbl x; };\n\
             println(y.to_string());\n\
             println(x.to_string());\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "50\n1");
    }
}

#[test]
fn test_with_provider_returns_body_value() {
    // The body's result becomes the with_provider expression's
    // value. Smoke-test that an `i64` literal body lowers without
    // error and the function returns a non-void path.
    let ir = ir_for(
        "pub trait Recorder { fn record(value: i64); }\n\
             pub struct Counter { n: i64 }\n\
             impl Recorder for Counter { fn record(value: i64) { } }\n\
             pub effect resource Metric: Recorder;\n\
             fn run() -> i64 {\n\
               let p = Counter { n: 0 };\n\
               with_provider[Metric](p, || { 7 })\n\
             }",
    );
    // Non-`pub` fns now emit with internal linkage so LLVM's inliner can
    // elide their standalone symbol after inlining all callers; accept
    // either form here so the test is robust against linkage tweaks.
    assert!(
        ir.contains("define i64 @run") || ir.contains("define internal i64 @run"),
        "expected `run` returns i64; IR: {}",
        ir
    );
    assert!(
        ir.contains("call void @karac_provider_push"),
        "expected push inside run; IR: {}",
        ir
    );
}

// ── Labeled blocks runtime ──────────────────────────────────
//
// Labeled-block codegen + interpreter sibling slice (LBC1-LBC5).
// The frontend slice (commit 85e49c8) shipped parser + resolver +
// typechecker; this slice wires runtime semantics so the typed
// program actually runs correctly. See
// `docs/implementation_checklist/phase-5-diagnostics.md` § 5.2 →
// "Labeled blocks: codegen + interpreter sibling".

/// `lbl: { break lbl 42; -1 }` evaluates to 42. The early `break label
/// expr` exits the labeled block with the given value; the
/// fall-through tail (`-1`) never runs.
#[test]
fn test_labeled_block_break_with_value_e2e() {
    let out = run_program(
        r#"
fn main() {
    let x: i64 = lbl: { break lbl 42; -1 };
    println(x);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "42");
    }
}

/// `lbl: { break lbl; }` typed as `()` — bare break exits with unit.
/// Verifies the post-block code path runs (println marker).
#[test]
fn test_labeled_block_bare_break_e2e() {
    let out = run_program(
        r#"
fn main() {
    lbl: { break lbl; };
    println(7);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "7");
    }
}

/// `lbl: { 99 }` evaluates to 99 — no break path exercised; the
/// labeled block falls through normally and the slot stores the tail.
#[test]
fn test_labeled_block_tail_expression_when_no_break_e2e() {
    let out = run_program(
        r#"
fn main() {
    let x: i64 = lbl: { 99 };
    println(x);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "99");
    }
}

#[test]
fn test_caller_side_intercept_emits_done_block_with_free() {
    // The `kara.poll_done` block is the exit edge from the poll
    // loop. It calls `@free` on the state struct, releasing the
    // heap allocation, and continues with subsequent IR.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver() { fetch(); }
             fn main() { driver(); }",
    );
    let main_body = extract_fn_ir(&ir, "main");
    assert!(
        main_body.contains("kara.poll_done:"),
        "intercept must emit a `kara.poll_done` block:\n{main_body}"
    );
    assert!(
        main_body.contains("call void @free(ptr %kara.state)"),
        "done block must free the state struct:\n{main_body}"
    );
}

#[test]
fn test_caller_side_intercept_yield_block_calls_sched_yield() {
    // The yield block calls the POSIX `sched_yield` libc primitive
    // — an external i32-returning function declared at module-build
    // time. The discriminant is discarded.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver() { fetch(); }
             fn main() { driver(); }",
    );
    assert!(
        ir.contains("declare i32 @sched_yield()"),
        "module must declare extern @sched_yield:\n{ir}"
    );
    let main_body = extract_fn_ir(&ir, "main");
    assert!(
        main_body.contains("call i32 @sched_yield()"),
        "yield block must call @sched_yield:\n{main_body}"
    );
}

#[test]
fn test_caller_side_intercept_pending_branches_to_yield_not_loop() {
    // The Pending conditional branch routes to `kara.poll_yield` —
    // the yield block is the indirection that handles the
    // cooperative yield before re-entering the loop.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver() { fetch(); }
             fn main() { driver(); }",
    );
    let main_body = extract_fn_ir(&ir, "main");
    assert!(
            main_body.contains(
                "br i1 %kara.is_pending, label %kara.poll_yield, label %kara.poll_done"
            ),
            "Pending branch must route to kara.poll_yield (not directly back to poll_loop):\n{main_body}"
        );
}

// ── Phase 6 line 26 slice 8g: method-call network-boundary intercept ─
//
// Mirrors slice 8d's free-function intercept for `obj.method(args)`
// calls where the resolved `Type.method` key is in
// `state_machine_state_constructors`. The receiver `obj` becomes
// `self` and stores into state struct field 1 (layout position 0);
// method args follow at fields 2..K.

#[test]
fn test_method_call_intercept_emits_state_machine_invocation() {
    // A method call to a network-boundary method must emit the
    // state-machine invocation shape (ctor + poll loop) instead
    // of a direct method dispatch.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             struct Hub { count: i64 }
             impl Hub {
                 fn run(self) { fetch(); }
             }
             fn main() {
                 let h = Hub { count: 0 };
                 h.run();
             }",
    );
    let main_body = extract_fn_ir(&ir, "main");
    assert!(
        main_body.contains("call ptr @__kara_state_new_Hub.run()"),
        "method intercept must call Hub.run's state constructor:\n{main_body}"
    );
    assert!(
        main_body.contains("kara.poll_loop:"),
        "method intercept must emit kara.poll_loop:\n{main_body}"
    );
    assert!(
        main_body.contains("call i8 @__kara_poll_Hub.run(ptr %kara.state, ptr null)"),
        "method intercept must invoke Hub.run's poll-fn:\n{main_body}"
    );
}

// ── Phase 6 line 26 slice 8h: body-splitting for void calls ────────
//
// The poll-fn now walks the user function's body AST and partitions
// statements at yield-point spans. Non-yield arg-less Call(Identifier)
// statements with void-returning callees are emitted as `call void
// @<name>()` in the corresponding state arm, between the slice-8a
// reload prologue and the slice-8b tag-store / Ready return.
// Method calls, args-bearing calls, let bindings, and control flow
// are deferred to follow-on slices.

#[test]
fn test_body_splitting_emits_pre_yield_void_call_in_state_0() {
    // `fn driver() { helper(); fetch(); }` — `helper();` runs in
    // state_0 (before the first yield); the yield-point call to
    // `fetch()` advances to state_1 (terminal).
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn helper() {}
             fn driver() { helper(); fetch(); }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    // The helper call should appear in state_0 BEFORE the tag-store
    // that transitions to state 1.
    let helper_pos = body
        .find("call void @helper()")
        .expect("helper void-call must appear in poll-fn body");
    let tag_store_pos = body
        .find("store i32 1, ptr %state_0.next_tag_ptr")
        .expect("state_0 must store next tag = 1");
    assert!(
        helper_pos < tag_store_pos,
        "helper call must precede the tag-store in state_0:\n{body}"
    );
}

#[test]
fn test_body_splitting_emits_post_yield_void_call_in_terminal_arm() {
    // `fn driver() { fetch(); helper(); }` — `helper();` runs in
    // the terminal arm (state_1 for 1-yield) before the Ready return.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn helper() {}
             fn driver() { fetch(); helper(); }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    let helper_pos = body
        .find("call void @helper()")
        .expect("helper void-call must appear in poll-fn body");
    let ready_pos = body
        .find("ret i8 1")
        .expect("terminal arm must return Ready");
    assert!(
        helper_pos < ready_pos,
        "helper call must precede the Ready return in terminal arm:\n{body}"
    );
}

#[test]
fn test_body_splitting_multi_yield_segments_calls_per_arm() {
    // `fn driver() { a(); fetch(); b(); fetch(); c(); }` —
    // a() in state_0, b() in state_1, c() in terminal state_2.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn a() {}
             fn b() {}
             fn c() {}
             fn driver() { a(); fetch(); b(); fetch(); c(); }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    // Each call should appear exactly once in the poll-fn.
    assert_eq!(
        body.matches("call void @a()").count(),
        1,
        "a() should appear once in state_0:\n{body}"
    );
    assert_eq!(
        body.matches("call void @b()").count(),
        1,
        "b() should appear once in state_1:\n{body}"
    );
    assert_eq!(
        body.matches("call void @c()").count(),
        1,
        "c() should appear once in terminal state_2:\n{body}"
    );
    // a() must precede state_0's tag-store; b() must precede
    // state_1's tag-store; c() must precede the Ready return.
    let pos_a = body.find("call void @a()").unwrap();
    let pos_state_0_store = body.find("store i32 1, ptr %state_0.next_tag_ptr").unwrap();
    let pos_b = body.find("call void @b()").unwrap();
    let pos_state_1_store = body.find("store i32 2, ptr %state_1.next_tag_ptr").unwrap();
    let pos_c = body.find("call void @c()").unwrap();
    let pos_ready = body.rfind("ret i8 1").unwrap();
    assert!(pos_a < pos_state_0_store, "a() before state_0 tag-store");
    assert!(pos_b > pos_state_0_store && pos_b < pos_state_1_store);
    assert!(pos_c > pos_state_1_store && pos_c < pos_ready);
}

#[test]
fn test_body_splitting_no_emission_for_trivial_yield_only_body() {
    // `fn driver() { fetch(); }` — no user-code between yields, so
    // the poll-fn body has no extra calls beyond the slice-7
    // switch + slice-8a reload + slice-8b tag-store / Ready.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn helper() {}
             fn driver() { fetch(); }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    // helper is declared but never called inside driver's body —
    // it must NOT appear in the poll-fn body.
    assert!(
        !body.contains("call void @helper()"),
        "helper must not appear in trivial driver's poll-fn:\n{body}"
    );
}

#[test]
fn test_return_value_unit_returns_keep_existing_behavior() {
    // Unit-returning callees (no `-> Type` in source) get no
    // terminal field in the state struct and no caller-side load.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver() { fetch(); }
             fn main() { driver(); }",
    );
    let main_body = extract_fn_ir(&ir, "main");
    // No terminal-field GEP in main.
    assert!(
        !main_body.contains("kara.return.field_ptr"),
        "unit-returning callee must not produce caller-side terminal-field load:\n{main_body}"
    );
    // State struct stays { i32 } — only the tag.
    let line = ir
        .lines()
        .find(|l| l.starts_with("%kara.state.driver = type {"))
        .unwrap_or_else(|| panic!("no driver state struct:\n{ir}"));
    assert!(
        !line.contains("i64") || line.contains("i32, i64"),
        // be conservative — just sanity check that the line isn't malformed
        "unit-return state struct shape:\n{line}"
    );
    // The poll-fn terminal arm must not contain the placeholder store.
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    assert!(
        !body.contains("kara.return.field_ptr"),
        "unit-returning poll-fn must not emit terminal-field store:\n{body}"
    );
}

#[test]
fn test_8ai_caller_side_load_matches_widened_return_type() {
    // The caller-side intercept loads through the typed entry in
    // `state_machine_return_types` — verifies the load instruction
    // for an `i32`-returning callee uses `load i32` rather than
    // the i64 default. Parameter form avoids the integer-literal
    // inference gap noted on the i32 state-struct test.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver(x: i32) -> i32 with sends(Network) receives(Network) { fetch(); x }
             fn run(x: i32) -> i32 { driver(x) }",
    );
    let body = extract_fn_ir(&ir, "run");
    assert!(
        body.contains("load i32, ptr %kara.return.field_ptr"),
        "caller must load i32 from terminal field for an i32-returning callee:\n{body}"
    );
}

// ── Phase 6 line 26 slice 8j: method-call body-splitting ──────────────
//
// Mirrors slice 8h's free-function body-splitting for `<recv>.method()`
// shapes where the receiver is a captured layout-field identifier,
// args are empty, and the resolved `Type.method` LLVM function
// returns void. The reloaded receiver slot from slice 8a feeds the
// method's first param — by-value for owned self, by-pointer for
// `ref self` / `mut ref self`. Tests use a free-fn `driver` body
// with `let h = ...; h.method(); fetch();` shape rather than an
// impl-method body with `self.method()`, because the codegen's
// user-side `compile_method_call` doesn't yet resolve `SelfValue`
// receivers (an orthogonal limitation that doesn't affect the
// body-splitting walker itself).

#[test]
fn test_body_splitting_8j_emits_method_call_on_local_in_state_0() {
    // `fn driver() { let h = Hub { count: 0 }; h.helper(); fetch(); }`
    // — `h.helper()` runs in state_0 before the tag-store that
    // transitions to state_1. `ref self` so the method takes a
    // pointer (slice 8j passes the slot pointer directly).
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             struct Hub { count: i64 }
             impl Hub { fn helper(ref self) {} }
             fn driver() with sends(Network) receives(Network) {
                 let h = Hub { count: 0 };
                 h.helper();
                 fetch();
             }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    let helper_pos = body
        .find("call void @Hub.helper(")
        .expect("Hub.helper void-call must appear in poll-fn body");
    let tag_store_pos = body
        .find("store i32 1, ptr %state_0.next_tag_ptr")
        .expect("state_0 must store next tag = 1");
    assert!(
        helper_pos < tag_store_pos,
        "h.helper() call must precede the tag-store in state_0:\n{body}"
    );
}

#[test]
fn test_body_splitting_8j_emits_method_call_in_terminal_arm() {
    // `fn driver() { let h = Hub { count: 0 }; fetch(); h.helper(); }`
    // — `h.helper()` runs in the terminal arm before the Ready return.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             struct Hub { count: i64 }
             impl Hub { fn helper(ref self) {} }
             fn driver() with sends(Network) receives(Network) {
                 let h = Hub { count: 0 };
                 fetch();
                 h.helper();
             }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    let helper_pos = body
        .find("call void @Hub.helper(")
        .expect("Hub.helper void-call must appear in poll-fn body");
    let ready_pos = body
        .find("ret i8 1")
        .expect("terminal arm must return Ready");
    assert!(
        helper_pos < ready_pos,
        "h.helper() call must precede the Ready return in terminal arm:\n{body}"
    );
}

#[test]
fn test_body_splitting_8j_multi_yield_method_segments_per_arm() {
    // Three method calls between two yields land in three distinct
    // state arms in source order — `h.a()` in state_0, `h.b()` in
    // state_1, `h.c()` in terminal state_2.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             struct Hub { count: i64 }
             impl Hub {
                 fn a(ref self) {}
                 fn b(ref self) {}
                 fn c(ref self) {}
             }
             fn driver() with sends(Network) receives(Network) {
                 let h = Hub { count: 0 };
                 h.a();
                 fetch();
                 h.b();
                 fetch();
                 h.c();
             }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    assert_eq!(
        body.matches("call void @Hub.a(").count(),
        1,
        "Hub.a should appear once in state_0:\n{body}"
    );
    assert_eq!(
        body.matches("call void @Hub.b(").count(),
        1,
        "Hub.b should appear once in state_1:\n{body}"
    );
    assert_eq!(
        body.matches("call void @Hub.c(").count(),
        1,
        "Hub.c should appear once in terminal state_2:\n{body}"
    );
    let pos_a = body.find("call void @Hub.a(").unwrap();
    let pos_state_0_store = body.find("store i32 1, ptr %state_0.next_tag_ptr").unwrap();
    let pos_b = body.find("call void @Hub.b(").unwrap();
    let pos_state_1_store = body.find("store i32 2, ptr %state_1.next_tag_ptr").unwrap();
    let pos_c = body.find("call void @Hub.c(").unwrap();
    let pos_ready = body.rfind("ret i8 1").unwrap();
    assert!(pos_a < pos_state_0_store, "a() before state_0 tag-store");
    assert!(
        pos_b > pos_state_0_store && pos_b < pos_state_1_store,
        "b() between state_0 and state_1 tag-stores"
    );
    assert!(
        pos_c > pos_state_1_store && pos_c < pos_ready,
        "c() between state_1 tag-store and Ready"
    );
}

#[test]
fn test_body_splitting_8m_let_identifier_rhs_loads_source_slot() {
    // `fn driver(n: i64) with sends(Network) { let y = n; fetch(); }`
    // — `let y = n` loads `n` from its captured-local slot and
    // stores it into the new `%y.slot`. Because `y` survives across
    // the fetch yield (referenced after — actually here `y` is
    // unused post-yield so it's NOT in layout, BUT slice 8m still
    // emits the let in state_0 because the walker queues it ahead
    // of the yield-point span match).
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn take(n: i64) {}
             fn driver(n: i64) with sends(Network) receives(Network) {
                 let y = n;
                 take(y);
                 fetch();
             }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    // `let y = n` loads n then stores into y's slot.
    assert!(
        body.contains("%y.slot = alloca i64"),
        "let y must alloca an i64 slot:\n{body}"
    );
    assert!(
        body.contains("%n.let_rhs = load i64, ptr %n.slot"),
        "let y = n must load n via .let_rhs:\n{body}"
    );
    assert!(
        body.contains("store i64 %n.let_rhs, ptr %y.slot"),
        "let RHS load must store into y.slot:\n{body}"
    );
    // Subsequent `take(y)` uses the new slot.
    assert!(
        body.contains("call void @take(i64 %y.arg)"),
        "take(y) must use the slot-loaded y value:\n{body}"
    );
}

#[test]
fn test_body_splitting_8m_let_then_method_call_on_local() {
    // The arm-local let-binding can serve as a method-call receiver
    // via the slice-8j receiver-from-slot mechanic. Here `let h = ...`
    // — wait, struct literal RHS isn't in BodyArg recognised set, so
    // this test actually pins that the let is silently DROPPED for
    // non-recognised RHS, and the subsequent `h.helper()` falls
    // through (h is not in the slot map, so receiver_field=None).
    // This is the v1 conservative behaviour — non-IntLit/non-slot
    // RHS shapes drop the let, downstream receiver becomes invalid.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             struct Hub { count: i64 }
             impl Hub { fn helper(ref self) {} }
             fn driver() with sends(Network) receives(Network) {
                 fetch();
                 let h = Hub { count: 0 };
                 h.helper();
             }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    // Struct-literal RHS isn't recognised — the let is dropped, so
    // no `%h.slot` alloca appears in the poll-fn body. Receiver
    // therefore can't resolve; `h.helper()` also doesn't emit.
    assert!(
        !body.contains("%h.slot = alloca"),
        "unrecognised let RHS must skip the binding emission:\n{body}"
    );
    assert!(
        !body.contains("call void @Hub.helper"),
        "h.helper() must skip when receiver isn't in slot map:\n{body}"
    );
}

#[test]
fn test_body_splitting_8m_let_chained_consumers() {
    // `let a = 7; let b = a; take(b);` in the terminal arm — let-
    // chains work because each let registers into slot_map, making
    // its binding name available to later lets and calls in the
    // same arm.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn take(n: i64) {}
             fn driver() with sends(Network) receives(Network) {
                 fetch();
                 let a = 7;
                 let b = a;
                 take(b);
             }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    assert!(
        body.contains("store i64 7, ptr %a.slot"),
        "let a = 7 must store literal into a.slot:\n{body}"
    );
    assert!(
        body.contains("%a.let_rhs = load i64, ptr %a.slot"),
        "let b = a must load a.slot:\n{body}"
    );
    assert!(
        body.contains("store i64 %a.let_rhs, ptr %b.slot"),
        "let b = a must store loaded value into b.slot:\n{body}"
    );
    assert!(
        body.contains("call void @take(i64 %b.arg)"),
        "take(b) must pass b's slot-loaded value:\n{body}"
    );
}

// ── Phase 6 line 26 slice 8q: arithmetic binary expressions in arm bodies ─
//
// `recognize_body_arg` widens to accept `lhs OP rhs` where `OP` is
// one of the five integer arithmetic ops (`+` / `-` / `*` / `/` /
// `%`) and each operand is itself a recognised `BodyArg`. The shared
// `materialize_body_arg` helper lowers `Binary` to LLVM
// `build_int_*` calls; the four emission paths (let RHS, assign RHS,
// call args, terminal return) consume the helper uniformly so the
// new `Binary` variant lights up everywhere at once. Unblocks
// compound-assign (`+=` / `-=` / `*=` / …) which lowers as
// `Assign { name, value: Binary { Slot(name), <rhs> } }` once the
// parser surface threads compound-assign through the walker.

#[test]
fn test_body_splitting_8q_binary_in_let_rhs() {
    // `let m = n + 1;` — binary expression as let RHS. The helper
    // materialises a slot load for `n`, an i64 const for `1`, and
    // an `add nsw` (or unsigned `add`) producing the result that
    // gets stored into the arm-local `m.slot`.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn helper(_x: i64) {}
             fn driver(n: i64) with sends(Network) receives(Network) {
                 fetch();
                 let m = n + 1;
                 helper(m);
             }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    // The binary result name is `binop.let_rhs`; lhs loads via
    // `.let_rhs` suffix → `%n.let_rhs`.
    assert!(
        body.contains("%n.let_rhs = load i64, ptr %n.slot"),
        "let-rhs binary lhs must load n.slot via .let_rhs:\n{body}"
    );
    assert!(
        body.contains("%binop.let_rhs = add i64 %n.let_rhs, 1"),
        "let-rhs binary must emit `add i64 %n.let_rhs, 1`:\n{body}"
    );
    assert!(
        body.contains("store i64 %binop.let_rhs, ptr %m.slot"),
        "let-rhs binary result must be stored into m.slot:\n{body}"
    );
}

#[test]
fn test_body_splitting_8q_binary_in_assign_rhs() {
    // `n = n + 1;` — assignment whose RHS is a binary expression.
    // The slot for `n` is the captured-local slot from slice 8a;
    // the binary result stores back into the same slot; slice 8n's
    // writeback then transfers the post-arm value to the state
    // struct before the yield.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver(n: i64) with sends(Network) receives(Network) {
                 n = n + 1;
                 fetch();
             }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    assert!(
        body.contains("%n.assign_rhs = load i64, ptr %n.slot"),
        "assign-rhs binary lhs must load n.slot via .assign_rhs:\n{body}"
    );
    assert!(
        body.contains("%binop.assign_rhs = add i64 %n.assign_rhs, 1"),
        "assign-rhs binary must emit `add i64 %n.assign_rhs, 1`:\n{body}"
    );
    assert!(
        body.contains("store i64 %binop.assign_rhs, ptr %n.slot"),
        "assign-rhs binary result must be stored into n.slot:\n{body}"
    );
    // Writeback should observe the post-binop value.
    let assign_pos = body
        .find("store i64 %binop.assign_rhs, ptr %n.slot")
        .expect("assignment store missing");
    let writeback_pos = body
        .find("%n.writeback = load i64, ptr %n.slot")
        .expect("writeback load missing");
    assert!(
        assign_pos < writeback_pos,
        "assignment must precede the writeback load:\n{body}"
    );
}

#[test]
fn test_body_splitting_8q_binary_in_call_arg() {
    // `helper(n + 1);` — binary expression as a free-fn call arg.
    // The helper materialises the binary into a value and threads
    // it into the `build_call` arg list.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn helper(_x: i64) {}
             fn driver(n: i64) with sends(Network) receives(Network) {
                 helper(n + 1);
                 fetch();
             }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    assert!(
        body.contains("%n.arg = load i64, ptr %n.slot"),
        "call-arg binary lhs must load n.slot via .arg:\n{body}"
    );
    assert!(
        body.contains("%binop.arg = add i64 %n.arg, 1"),
        "call-arg binary must emit `add i64 %n.arg, 1`:\n{body}"
    );
    assert!(
        body.contains("call void @helper(i64 %binop.arg)"),
        "helper must be called with the binary result as arg:\n{body}"
    );
}

#[test]
fn test_body_splitting_8q_binary_in_terminal_return() {
    // `fn driver(n: i64) -> i64 { fetch(); n + 99 }` — terminal-arm
    // final expression is a binary. Slice 8q's helper widening
    // makes the terminal-return path consult the same materialiser,
    // so the binary value flows into the state-struct terminal
    // field instead of slice 8i's `i64 0` placeholder.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver(n: i64) -> i64 with sends(Network) receives(Network) {
                 fetch();
                 n + 99
             }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    assert!(
        body.contains("%n.return = load i64, ptr %n.slot"),
        "terminal-return binary lhs must load n.slot via .return:\n{body}"
    );
    assert!(
        body.contains("%binop.return = add i64 %n.return, 99"),
        "terminal-return binary must emit `add i64 %n.return, 99`:\n{body}"
    );
    // The binary result lands in the state-struct terminal field.
    assert!(
        body.contains("store i64 %binop.return, ptr %kara.return.field_ptr"),
        "terminal-return binary result must be stored into kara.return field:\n{body}"
    );
}

#[test]
fn test_body_splitting_8q_binary_all_five_arith_ops() {
    // Pin the lowering of all five recognised arithmetic ops:
    // Add → `add i64`, Sub → `sub i64`, Mul → `mul i64`, Div →
    // `sdiv i64`, Mod → `srem i64`. One driver per op so the
    // assertions stay readable and each op exercises the full
    // materialisation pipeline.
    for (src_op, llvm_pat) in [
        ("+", "add i64"),
        ("-", "sub i64"),
        ("*", "mul i64"),
        ("/", "sdiv i64"),
        ("%", "srem i64"),
    ] {
        let src = format!(
            "effect resource Network;
                 pub fn fetch() with sends(Network) receives(Network) {{}}
                 fn driver(a: i64, b: i64) with sends(Network) receives(Network) {{
                     a = a {src_op} b;
                     fetch();
                 }}"
        );
        let ir = ir_for_with_state_struct_layouts(&src);
        let body = extract_fn_ir(&ir, "__kara_poll_driver");
        let expected = format!("%binop.assign_rhs = {llvm_pat} %a.assign_rhs, %b.assign_rhs");
        assert!(
            body.contains(&expected),
            "op `{src_op}` must emit `{expected}`:\n{body}"
        );
    }
}

#[test]
fn test_body_splitting_8q_unrecognised_binop_skips() {
    // Comparison / logical / bitwise binops stay outside the
    // recognised set — `recognize_body_arg` returns `None`, the
    // walker drops the whole statement, and codegen emits no
    // body-level alloca for it. Use a let introduced AFTER the
    // yield so the binding isn't picked up by the layout's reload
    // prologue (which would alloca a slot regardless of whether
    // the per-arm let-emission ran).
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver(a: i64, b: i64) with sends(Network) receives(Network) {
                 fetch();
                 let cmp = a == b;
             }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    // No `cmp.slot` alloca should appear — the let was silently
    // skipped by the walker, and `cmp` post-dates the fetch so it
    // isn't a captured-local either.
    assert!(
        !body.contains("%cmp.slot"),
        "unrecognised comparison RHS must skip the let entirely:\n{body}"
    );
    // No body-level `icmp` either — the comparison Call wasn't
    // queued for emission.
    assert!(
        !body.contains("icmp"),
        "skipped let must not lower to an icmp:\n{body}"
    );
}

#[test]
fn test_body_splitting_8r_compound_assign_all_five_arith_ops() {
    // Pin the LLVM lowering of all five recognised compound-assign
    // ops: `+=` → `add i64`, `-=` → `sub i64`, `*=` → `mul i64`,
    // `/=` → `sdiv i64`, `%=` → `srem i64`. The walker desugars
    // each into `Assign { Binary { op, Slot(name), rhs } }`.
    for (src_op, llvm_pat) in [
        ("+=", "add i64"),
        ("-=", "sub i64"),
        ("*=", "mul i64"),
        ("/=", "sdiv i64"),
        ("%=", "srem i64"),
    ] {
        let src = format!(
            "effect resource Network;
                 pub fn fetch() with sends(Network) receives(Network) {{}}
                 fn driver(a: i64, b: i64) with sends(Network) receives(Network) {{
                     a {src_op} b;
                     fetch();
                 }}"
        );
        let ir = ir_for_with_state_struct_layouts(&src);
        let body = extract_fn_ir(&ir, "__kara_poll_driver");
        let expected = format!("%binop.assign_rhs = {llvm_pat} %a.assign_rhs, %b.assign_rhs");
        assert!(
            body.contains(&expected),
            "op `{src_op}` must emit `{expected}`:\n{body}"
        );
        assert!(
            body.contains("store i64 %binop.assign_rhs, ptr %a.slot"),
            "op `{src_op}` result must store into a.slot:\n{body}"
        );
    }
}

#[test]
fn test_body_splitting_8r_compound_assign_with_slot_rhs() {
    // `a += b;` — compound-assign with another captured-local on
    // the RHS. Desugars to `a = a + b`; both operands resolve to
    // slot loads.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver(a: i64, b: i64) with sends(Network) receives(Network) {
                 a += b;
                 fetch();
             }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    assert!(
        body.contains("%a.assign_rhs = load i64, ptr %a.slot"),
        "compound-assign lhs must load a.slot:\n{body}"
    );
    assert!(
        body.contains("%b.assign_rhs = load i64, ptr %b.slot"),
        "compound-assign rhs must load b.slot:\n{body}"
    );
    assert!(
        body.contains("%binop.assign_rhs = add i64 %a.assign_rhs, %b.assign_rhs"),
        "compound-assign must emit `add i64 %a.assign_rhs, %b.assign_rhs`:\n{body}"
    );
}

#[test]
fn test_body_splitting_8t_yield_inside_while_advances_arm() {
    // `while !done { fetch(); }` followed by a post-loop call —
    // descent into the while-body finds the yield, advances arm.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn take(n: i64) {}
             fn driver(done: bool) with sends(Network) receives(Network) {
                 while not done { fetch(); }
                 take(7);
             }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    let transition_pos = body
        .find("store i32 1, ptr %state_0.next_tag_ptr")
        .expect("state_0 transition store should be present");
    let take_pos = body
        .find("call void @take(i64 7)")
        .expect("take call should be present");
    assert!(
        take_pos > transition_pos,
        "take(7) must land in state_1 (after the while-loop yield):\n{body}"
    );
}

#[test]
fn test_body_splitting_8t_yield_inside_for_advances_arm() {
    // `for x in items { fetch(); }` followed by a post-loop call.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn take(n: i64) {}
             fn driver(items: Vec[i64]) with sends(Network) receives(Network) {
                 for x in items.iter() { fetch(); }
                 take(11);
             }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    let transition_pos = body
        .find("store i32 1, ptr %state_0.next_tag_ptr")
        .expect("state_0 transition store should be present");
    let take_pos = body
        .find("call void @take(i64 11)")
        .expect("take call should be present");
    assert!(
        take_pos > transition_pos,
        "take(11) must land in state_1 (after the for-loop yield):\n{body}"
    );
}

#[test]
fn test_body_splitting_8t_multi_yield_in_multi_if_advances_per_yield() {
    // Two yields in two distinct ifs — descent must advance cur_arm
    // twice. `take(99)` lands in state_2 (the terminal arm after
    // two yields).
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn take(n: i64) {}
             fn driver(a: bool, b: bool) with sends(Network) receives(Network) {
                 if a { fetch(); }
                 if b { fetch(); }
                 take(99);
             }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    // Three arms: state_0, state_1, state_2 (terminal).
    // Both state_0 and state_1 are non-terminal (transition+Pending).
    let s0_transition_pos = body
        .find("store i32 1, ptr %state_0.next_tag_ptr")
        .expect("state_0 transition should be present");
    let s1_transition_pos = body
        .find("store i32 2, ptr %state_1.next_tag_ptr")
        .expect("state_1 transition should be present");
    let take_pos = body
        .find("call void @take(i64 99)")
        .expect("take call should be present");
    assert!(
        take_pos > s0_transition_pos && take_pos > s1_transition_pos,
        "take(99) must land in state_2 (terminal arm, after both transitions):\n{body}"
    );
}

// ── Phase 6 line 26 slice 8ah: terminal-arm call-returning-value ──
//
// Slice 8o + 8q recognised int literals, captured-local references,
// arm-local lets, and lowered binary arithmetic in the user
// function's final-expression position. Slice 8ah widens
// recognition to bare-identifier `Call` shapes (free-fn calls that
// return a value), closing the third final-expression class called
// out in slice 8i's note. Network-yield callees are filtered at
// recognition time so a yielding call in tail position still falls
// through to the placeholder rather than emitting a synchronous
// call that would skip its yield.

#[test]
fn test_terminal_return_8ah_uses_call_no_args() {
    // `fn driver() -> i64 ... { fetch(); other_fn() }` where
    // `other_fn` is a pure i64-returning callee — slice 8ah
    // emits `call i64 @other_fn()` in the terminal arm and stores
    // the result into the terminal return field.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn other_fn() -> i64 { 99 }
             fn driver() -> i64 with sends(Network) receives(Network) { fetch(); other_fn() }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    assert!(
        body.contains("%other_fn.return = call i64 @other_fn()"),
        "terminal arm must emit named call to @other_fn:\n{body}"
    );
    assert!(
        body.contains("store i64 %other_fn.return, ptr %kara.return.field_ptr"),
        "terminal arm must store call result into terminal field:\n{body}"
    );
}

#[test]
fn test_terminal_return_8ah_yielding_callee_keeps_zero_fallback() {
    // `fn driver() -> i64 ... { let _x = 1; remote_fetch() }` —
    // the tail call is itself a network-yield callee returning
    // i64. Slice 8ah's recognition filter rejects yielding
    // callees so the terminal field keeps the slice-8i `i64 0`
    // placeholder rather than emitting a synchronous
    // `call i64 @remote_fetch` that would skip the yield. The
    // leading let ensures the body isn't trivially yield-only.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn remote_fetch() -> i64 with sends(Network) receives(Network) { 7 }
             fn driver() -> i64 with sends(Network) receives(Network) {
                 let _x = 1;
                 remote_fetch()
             }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    assert!(
        !body.contains("call i64 @remote_fetch"),
        "yielding callee must NOT be lowered as a synchronous call \
             in the terminal arm:\n{body}"
    );
    assert!(
        body.contains("store i64 0, ptr %kara.return.field_ptr"),
        "yielding tail callee must fall back to i64 0 placeholder:\n{body}"
    );
}

#[test]
fn test_body_splitting_8n_writeback_skipped_in_terminal_arm() {
    // The terminal arm doesn't yield — it returns Ready and the
    // caller's `@free` releases the state struct. No writeback
    // needed (or wanted; the field is about to be freed).
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver(n: i64) with sends(Network) receives(Network) {
                 fetch();
             }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    // Single yield → one non-terminal arm (state_0) + one terminal
    // arm (state_1). Writeback shows up exactly once.
    assert_eq!(
        body.matches("%n.writeback = load i64").count(),
        1,
        "writeback must appear in non-terminal arm only:\n{body}"
    );
}

#[test]
fn test_body_splitting_8n_writeback_includes_let_shadowed_local() {
    // `fn driver(n: i64) with sends(Network) { let n = 99; fetch(); }`
    // — slice 8m's let shadows the captured-local `n` slot in the
    // slot map. The writeback before the yield uses the slice-8m
    // alloca's stored value (99), not the slice-8a reload's value
    // (the original n).
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver(n: i64) with sends(Network) receives(Network) {
                 let n = 99;
                 fetch();
             }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    // The let-store (slice 8m) puts `99` into n.slot.
    assert!(
        body.contains("store i64 99, ptr %n.slot"),
        "let n = 99 must store 99 into n.slot:\n{body}"
    );
    // The writeback then loads n.slot (which now has 99) and
    // stores into the state-struct field.
    assert!(
        body.contains("%n.writeback = load i64, ptr %n.slot"),
        "writeback must load from the (now-shadowed) n.slot:\n{body}"
    );
}

#[test]
fn test_body_splitting_8n_multi_yield_writeback_in_each_arm() {
    // Two yields → two non-terminal arms (state_0, state_1) each
    // emit writeback for the captured local; terminal arm (state_2)
    // does not. Total writeback occurrences: 2 (LLVM auto-suffixes
    // the duplicate SSA names so we match the named-GEP suffix
    // which appears once per writeback).
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver(n: i64) with sends(Network) receives(Network) {
                 fetch();
                 fetch();
             }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    // The GEP-defining line ` = getelementptr` for the
    // `writeback_field_ptr` name appears once per writeback site
    // (the store using the name is a separate match site we don't
    // count). LLVM may auto-suffix the SSA name across sites.
    assert_eq!(
        body.matches("writeback_field_ptr").count() / 2,
        2,
        "two non-terminal arms must each emit one writeback site:\n{body}"
    );
}

#[test]
fn test_body_splitting_8l_emits_method_with_identifier_arg_from_slot() {
    // `fn driver(n: i64) { let h = Hub { count: 0 }; h.take(n); fetch(); }`
    // — `n` is a captured layout field, loaded from `%n.slot` and
    // passed as the second arg to the method.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             struct Hub { count: i64 }
             impl Hub { fn take(ref self, x: i64) {} }
             fn driver(n: i64) with sends(Network) receives(Network) {
                 let h = Hub { count: 0 };
                 h.take(n);
                 fetch();
             }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    // Load of n from its slot; call passes the loaded value.
    assert!(
        body.contains("load i64, ptr %n.slot"),
        "method-call arg must load i64 from slice-8a slot:\n{body}"
    );
    assert!(
        body.contains("call void @Hub.take(ptr %h.slot, i64 %n.marg)"),
        "h.take(n) must pass receiver + slot-loaded arg:\n{body}"
    );
}

#[test]
fn test_body_splitting_8l_emits_method_with_multi_arg_mix() {
    // `fn driver(n: i64) { let h = Hub { count: 0 }; h.mix(n, 7); fetch(); }`
    // — receiver + slot-loaded + literal in source order.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             struct Hub { count: i64 }
             impl Hub { fn mix(ref self, a: i64, b: i64) {} }
             fn driver(n: i64) with sends(Network) receives(Network) {
                 let h = Hub { count: 0 };
                 h.mix(n, 7);
                 fetch();
             }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    assert!(
        body.contains("call void @Hub.mix(ptr %h.slot, i64 %n.marg, i64 7)"),
        "h.mix(n, 7) must order receiver + slot + literal:\n{body}"
    );
}

#[test]
fn test_body_splitting_8l_skips_method_with_unrecognised_arg() {
    // `fn driver(n: i64) { let h = ...; h.take(if n > 0 { n } else { 0 }); fetch(); }`
    // — the `if`-expression arg shape stays outside the
    // recognised `BodyArg` set (slice 8q widened to lowered
    // integer arithmetic; slice 8ah widened to bare-identifier
    // free-fn calls; if/match/block expressions remain
    // unrecognised). So the whole method call is silently
    // skipped at body-splitting time. No `call void @Hub.take`
    // appears in the poll-fn body.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             struct Hub { count: i64 }
             impl Hub { fn take(ref self, x: i64) {} }
             fn driver(n: i64) with sends(Network) receives(Network) {
                 let h = Hub { count: 0 };
                 h.take(if n > 0 { n } else { 0 });
                 fetch();
             }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    assert!(
        !body.contains("call void @Hub.take"),
        "unrecognised method-call arg must skip the whole call:\n{body}"
    );
}

#[test]
fn test_body_splitting_8k_emits_identifier_arg_loaded_from_slot() {
    // `fn driver(n: i64) { take(n); fetch(); }` — `n` is a captured
    // layout field, reloaded into `%n.slot` by slice 8a, and the
    // call's arg loads from the slot before passing to `@take`.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn take(x: i64) {}
             fn driver(n: i64) with sends(Network) receives(Network) {
                 take(n);
                 fetch();
             }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    // The load from %n.slot into %n.arg, then the call passing %n.arg.
    assert!(
        body.contains("load i64, ptr %n.slot"),
        "identifier arg must load i64 from slice-8a slot:\n{body}"
    );
    assert!(
        body.contains("call void @take(i64 %n.arg)"),
        "take(n) must pass the loaded slot value as arg:\n{body}"
    );
}

#[test]
fn test_body_splitting_8k_emits_multi_arg_mix_of_literal_and_identifier() {
    // `fn driver(n: i64) { mix(n, 7); fetch(); }` — first arg is a
    // slot-loaded value, second arg is a literal const, both passed
    // to `@mix` in source order.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn mix(a: i64, b: i64) {}
             fn driver(n: i64) with sends(Network) receives(Network) {
                 mix(n, 7);
                 fetch();
             }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    assert!(
        body.contains("call void @mix(i64 %n.arg, i64 7)"),
        "mix(n, 7) must pass slot-load + literal in order:\n{body}"
    );
}

#[test]
fn test_body_splitting_8k_skips_unrecognised_arg_shape() {
    // `fn driver(n: i64) { take(if n > 0 { n } else { 0 }); fetch(); }`
    // — the `if`-expression arg shape stays outside the
    // recognised `BodyArg` set (slice 8q widened to lowered
    // integer arithmetic; slice 8ah widened to bare-identifier
    // free-fn calls; if/match/block expressions remain
    // unrecognised). So the whole `take` call is silently
    // skipped — the poll-fn body emits no `call void @take`
    // between the reload prologue and the tag-store.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn take(x: i64) {}
             fn driver(n: i64) with sends(Network) receives(Network) {
                 take(if n > 0 { n } else { 0 });
                 fetch();
             }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    assert!(
        !body.contains("call void @take"),
        "unrecognised arg shape must skip the whole call:\n{body}"
    );
}

#[test]
fn test_body_splitting_8j_method_call_passes_reloaded_slot_pointer() {
    // Pins that the receiver argument routes through the slice-8a
    // `%h.slot` alloca — slot pointer passed directly to `ref self`.
    // The regression pin: receiver mechanic actually wires through
    // to the method call, not just emits a call with garbage.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             struct Hub { count: i64 }
             impl Hub { fn helper(ref self) {} }
             fn driver() with sends(Network) receives(Network) {
                 let h = Hub { count: 0 };
                 h.helper();
                 fetch();
             }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    assert!(
        body.contains("call void @Hub.helper(ptr %h.slot)"),
        "h.helper(ref self) must receive the slice-8a h.slot pointer:\n{body}"
    );
}

#[test]
fn test_two_distinct_monos_each_get_their_own_state_machine_helpers() {
    // `driver[T]` instantiated twice with two different types
    // produces two distinct per-mono state-machine helper sets.
    // The polymorphic source has no `with` clause mentioning an
    // effect parameter, so all monos share the same concrete
    // effect set (trivially-monomorphic v1 scope — see slice 8v's
    // doc framing). Each mono gets its own state struct, poll-fn,
    // constructor under its mangled name. Uses two int widths
    // (`i64` and `i32`) so both monos produce stable layouts
    // through the unified `llvm_type_to_mangle_str` path without
    // depending on String / collection lowering through
    // `compile_generic_call`'s arg materialisation (which has
    // independent gaps tracked outside slice 8v).
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver[T](item: T) { fetch(); }
             fn caller() {
                 driver(42i64);
                 driver(7i32);
             }",
    );
    assert!(
        ir.contains("%\"kara.state.driver$i64\" = type"),
        "i64 mono state struct must be emitted:\n{ir}"
    );
    assert!(
        ir.contains("%\"kara.state.driver$i32\" = type"),
        "i32 mono state struct must be emitted:\n{ir}"
    );
    // Each layout uses the per-mono concrete field type.
    let i64_line = ir
        .lines()
        .find(|l| l.contains("%\"kara.state.driver$i64\" = type"))
        .unwrap();
    assert!(
        i64_line.contains("{ i32, i64 }"),
        "i64 mono state struct must be {{ i32, i64 }}: {i64_line}"
    );
    let i32_line = ir
        .lines()
        .find(|l| l.contains("%\"kara.state.driver$i32\" = type"))
        .unwrap();
    assert!(
        i32_line.contains("{ i32, i32 }"),
        "i32 mono state struct must be {{ i32, i32 }}: {i32_line}"
    );
    // Both poll-fns + constructors emit at their mangled keys.
    assert!(
        ir.contains("@\"__kara_poll_driver$i64\""),
        "i64 mono poll-fn must emit:\n{ir}"
    );
    assert!(
        ir.contains("@\"__kara_poll_driver$i32\""),
        "i32 mono poll-fn must emit:\n{ir}"
    );
    assert!(
        ir.contains("@\"__kara_state_new_driver$i64\""),
        "i64 mono constructor must emit:\n{ir}"
    );
    assert!(
        ir.contains("@\"__kara_state_new_driver$i32\""),
        "i32 mono constructor must emit:\n{ir}"
    );
}

// ── Slice 9: Module-level let / let mut codegen (design.md §1278-1330) ─
//
// Real LLVM globals — immutable `let X: T = INIT` lowers to
// `@X = internal constant T INIT`; `let mut X: T = INIT` lowers
// to `@X = internal global T INIT`. `#[thread_local]` adds the
// thread-local storage class. Reads route through LLVM `load`,
// writes through LLVM `store`. Tests below exercise each shape
// end-to-end so a regression in any layer surfaces.

#[test]
fn test_e2e_modbind_immutable_let_read() {
    let output = run_program(
        "let MAX: i64 = 100;\n\
             fn main() { println(MAX); }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "100\n");
}

#[test]
fn test_e2e_modbind_let_mut_read_and_write() {
    // Bump in one function, read in main — global state survives
    // across function calls, which is the v1-blocking property
    // captured by §1322's synthetic-resource conflict analysis.
    let output = run_program(
        "let mut COUNTER: i64 = 0;\n\
             fn bump() { COUNTER = COUNTER + 1; }\n\
             fn main() {\n\
                 bump();\n\
                 bump();\n\
                 bump();\n\
                 println(COUNTER);\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "3\n");
}

#[test]
fn test_e2e_modbind_compound_assign() {
    // `+=` / `-=` / `*=` lower to load + binop + store on the
    // global pointer. Catches any drift in the CompoundAssign
    // arm's identifier-LHS fast path.
    let output = run_program(
        "let mut ACC: i64 = 5;\n\
             fn main() {\n\
                 ACC += 10;\n\
                 ACC -= 3;\n\
                 ACC *= 2;\n\
                 println(ACC);\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "24\n");
}

#[test]
fn test_e2e_modbind_bool_let_mut() {
    let output = run_program(
        "let mut FLAG: bool = false;\n\
             fn flip() { FLAG = true; }\n\
             fn main() {\n\
                 flip();\n\
                 println(FLAG);\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "true\n");
}

#[test]
fn test_e2e_modbind_local_shadow_takes_precedence() {
    // A function-local `let mut COUNTER` shadows the
    // module-level binding for the duration of that scope — the
    // identifier read resolves to the local slot first per the
    // compile_expr Identifier arm's lookup order (`variables`
    // before `module_bindings`). Without this guarantee, a
    // function that happened to name a local after a module
    // binding would silently read the global instead.
    let output = run_program(
        "let mut COUNTER: i64 = 100;\n\
             fn local_shadows() -> i64 {\n\
                 let COUNTER: i64 = 7;\n\
                 COUNTER\n\
             }\n\
             fn main() {\n\
                 println(local_shadows());\n\
                 println(COUNTER);\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "7\n100\n");
}

#[test]
fn test_e2e_modbind_container_for_loop_iterates() {
    // B-2026-07-31-30 — `for x in <module-level container>` compiled to a
    // ZERO-ITERATION loop. The `compile_for` Identifier arm gated its whole
    // container dispatch (String/Vec/Slice/Map/Set) on `self.variables`, the
    // LOCALS table, so a module binding matched nothing and fell through to
    // the arm's `const 0` — which is a silently empty loop, not an error.
    //
    // Every OTHER access path on the same binding read the live global
    // correctly (`len()`, `v[i]`, `clone()`), so the collection was provably
    // populated while the loop saw nothing, and `--interp` iterated it fine:
    // a silent run-vs-build divergence. Asserting the sum/count rather than
    // just "non-empty" pins the elements actually observed.
    let output = run_program(
        "let mut MV: Vec[i64] = Vec.new();\n\
             let mut MM: Map[String, i64] = Map.new();\n\
             let mut MS: Set[String] = Set.new();\n\
             fn main() {\n\
                 MV.push(10); MV.push(20);\n\
                 MM.insert(\"a\", 1); MM.insert(\"b\", 2);\n\
                 MS.insert(\"p\"); MS.insert(\"q\");\n\
                 let mut vs = 0;\n\
                 for x in MV { vs = vs + x; }\n\
                 println(vs);\n\
                 let mut ms = 0;\n\
                 for (k, v) in MM { ms = ms + v; }\n\
                 println(ms);\n\
                 let mut sc = 0;\n\
                 for s in MS { sc = sc + 1; }\n\
                 println(sc);\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "30\n3\n2\n");
}

#[test]
fn test_e2e_shadow_same_class_still_works() {
    // Regression guard: a same-class shadow (String→String) must keep
    // working — the dance is a no-op-equivalent when the class is
    // unchanged (old tags purged, identical new tags reinstalled).
    if let Some(out) = run_program(
        "fn main() {\n\
             let s = \"first\";\n\
             let s = \"second\";\n\
             println(s);\n\
             println(s.len());\n\
             }",
    ) {
        assert_eq!(out, "second\n6\n");
    }
}

#[test]
fn test_e2e_lowercase_rand_advances_state() {
    // Lowercase `rand.next_u64()` must reach the same
    // `karac_runtime_rand_next_u64` FFI as the capitalized form: two draws
    // differ when state advances. Lowercase counterpart of
    // `test_e2e_ambient_rand_next_u64_advances_state` — but this one is
    // also covered for resolve/typecheck by the clean-pipeline test above,
    // closing the false-pass gap that test documents.
    let out = run_program(
        r#"
fn main() with reads(RandomSource) {
    let a = rand.next_u64();
    let b = rand.next_u64();
    if a != b { println("rand-advanced"); } else { println("rand-stuck"); }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "rand-advanced");
    }
}

#[test]
fn e2e_user_ord_cmp_body_drives_comparison_operators() {
    // B-2026-08-26-10 — the compiled twin of
    // `test_user_impl_ord_cmp_body_drives_comparison_operators`. `<` on a
    // user type now lowers to `Item.cmp(a, b).is_lt()` rather than a
    // `Item.lt` no impl defines.
    //
    // The `cmp` reverses the order deliberately: field declaration order
    // answers `true false true false` for ids 1 vs 5, the body demands the
    // exact opposite, so only a backend that really calls the body passes.
    // Both spellings are checked in one program — `a < b` and the direct
    // `a.cmp(b).is_lt()` — because they used to disagree with each other
    // as well as with the interpreter.
    let out = run_program(
        r#"
struct Item { id: i64 }
impl PartialEq for Item { fn eq(ref self, other: ref Item) -> bool { self.id == other.id } }
impl Eq for Item {}
impl PartialOrd for Item { fn partial_cmp(ref self, other: ref Item) -> Option[Ordering] { Some(other.id.cmp(self.id)) } }
impl Ord for Item { fn cmp(ref self, other: ref Item) -> Ordering { other.id.cmp(self.id) } }
fn main() {
    let a = Item { id: 1 };
    let b = Item { id: 5 };
    println(a < b);
    println(a > b);
    println(a <= b);
    println(a >= b);
    println(a.cmp(b).is_lt());
}
"#,
    );
    let out = out.expect("user-`impl Ord` comparison operators must build");
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines, vec!["false", "true", "false", "true", "false"]);
}

#[test]
fn test_non_void_tail_return_still_returns_value() {
    // Counter-test: the void-discard guard must NOT fire on
    // functions that genuinely return a value via tail expression.
    // Pre- and post-fix this should work identically; here we
    // pin it so future tightening doesn't accidentally also drop
    // returns from non-void fns.
    let out = run_program(
        r#"
fn double(n: i64) -> i64 {
    n * 2
}

fn main() {
    println(double(21));
}
"#,
    );
    if let Some(s) = out {
        assert_eq!(s.trim(), "42");
    }
}

// ── Refinement types (phase-9 step 4) ───────────────────────

#[test]
fn test_e2e_refinement_arithmetic_returns_base() {
    // `d + d` where `d: Doubled` strips to the base `i64` for the add,
    // and the result is the base type — so it prints as a plain i64.
    let out = run_program(
        r#"
type Doubled = i64 where self % 2 == 0;
fn main() {
    let d = 4 as Doubled;
    println(d + d);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "8");
    }
}

#[test]
fn test_e2e_concurrent_cas_loop_increment_no_lost_updates() {
    // The canonical lock-free pattern: a load + compare_exchange retry
    // loop, run concurrently from two `par {}` branches 50_000× each.
    // CAS guarantees exactly 100_000 — a failed exchange retries with the
    // freshly-observed value, so no update is ever lost.
    let out = run_program(
        r#"
par struct Counter { v: Atomic[i64] }
impl Counter {
    fn get(ref self) -> i64 { self.v.load(MemoryOrdering.SeqCst) }
    fn inc(ref self) {
        let mut done = false;
        while not done {
            let cur = self.v.load(MemoryOrdering.SeqCst);
            match self.v.compare_exchange(cur, cur + 1, MemoryOrdering.SeqCst, MemoryOrdering.SeqCst) {
                Ok(_) => { done = true; }
                Err(_) => { }
            }
        }
    }
}
fn bump(c: Counter, n: i64) {
    let mut i = 0;
    while i < n { c.inc(); i = i + 1; }
}
fn main() {
    let c = Counter { v: Atomic.new(0) };
    par {
        bump(c, 50000);
        bump(c, 50000);
    }
    println(c.get());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "100000");
    }
}

// ── Mutex + lock blocks (slice 1: spinlock, standalone) ──────

#[test]
fn test_ir_lock_block_emits_futex_cmpxchg_and_conditional_wake() {
    let ir = ir_for(
        r#"
fn main() {
    let m = Mutex.new(0);
    lock m x { x = x + 1; }
}
"#,
    );
    // Acquire fast path: cmpxchg(0 -> 1) — uncontended locking stays inline.
    assert!(
        ir.contains("cmpxchg"),
        "lock acquire must emit a `cmpxchg` fast path; got IR:\n{ir}"
    );
    // Contended path calls the runtime to block instead of spinning.
    assert!(
        ir.contains("@karac_runtime_mutex_lock"),
        "lock contended path must call `karac_runtime_mutex_lock`; got IR:\n{ir}"
    );
    // Release: atomic xchg(-> 0) reading the prior state, then a *conditional*
    // wake call (only when the prior state was 2 = contended).
    assert!(
        ir.contains("atomicrmw xchg"),
        "lock release must emit `atomicrmw xchg` to read the prior state; got IR:\n{ir}"
    );
    assert!(
        ir.contains("@karac_runtime_mutex_unlock_wake"),
        "lock release must conditionally call `karac_runtime_mutex_unlock_wake`; got IR:\n{ir}"
    );
}

#[test]
fn test_e2e_passthrough_arg_and_returned_literal_single_fire() {
    // B-2026-08-02-23 leg 2 — a param that flows back OUT to the caller.
    // The caller-drops-the-owned-arg convention fires the arg binding's
    // body at the call, which is right when the value dies inside the
    // callee (`consume` below) and a duplicate when the callee hands it
    // back. Two return shapes, both previously double-firing on BOTH
    // backends: a bare-identifier passthrough, and the param moved into a
    // returned struct literal (which `fn_returns_param` did not even
    // recognize as a return site until it learned to look inside
    // aggregate literals).
    let out = run_program(
        r#"
struct Res { id: i64, name: String }
impl Drop for Res {
    fn drop(mut ref self) { println(f"drop {self.id} {self.name}") }
}
struct Holder { xs: Vec[Res], tag: i64 }
fn passthru(v: Vec[Res]) -> Vec[Res] { v }
fn mk(v: Vec[Res]) -> Holder { Holder { xs: v, tag: 9 } }
fn consume(v: Vec[Res]) -> i64 { v.len() }
fn main() {
    println("a");
    {
        let mut xs: Vec[Res] = Vec.new();
        xs.push(Res { id: 1, name: f"a{1}" });
        let ys = passthru(xs);
        println(ys.len());
    }
    {
        let mut zs: Vec[Res] = Vec.new();
        zs.push(Res { id: 2, name: f"b{2}" });
        let h = mk(zs);
        println(h.tag);
    }
    {
        let mut ws: Vec[Res] = Vec.new();
        ws.push(Res { id: 3, name: f"c{3}" });
        println(consume(ws));
    }
    println("end");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out.trim(),
            "a\n1\ndrop 1 a1\n9\ndrop 2 b2\n1\ndrop 3 c3\nend"
        );
    }
}

#[test]
fn test_e2e_direct_param_view_assign_then_fresh_value() {
    // B-2026-09-03-30 — the DIRECT-param spelling of the re-arm above:
    // `a = h;` where `h` is the by-value PARAM (not a match-arm payload
    // view), then `a` is given a value of its OWN again. The compiled
    // backends were ALWAYS right here — their `cond_move_drop_flags` re-arm
    // (B-2026-08-30-53 second measurement) restores the target's body on any
    // assignment whose RHS is not a param view — so this is the ORACLE the
    // interpreter was moved onto: `karac run --interp` lost `dR5` because
    // `suppress_assign_move_user_drop`'s param branch retracted `a`'s slot
    // and had no re-arm counterpart. Pinned so a future "reconciliation"
    // has to break this, not the interpreter twin.
    //
    // `dR1` displaces the initializer at `a = h`; `dR5` is the surviving
    // fresh value's exit body (the lost fire); then the caller drops `h`
    // (`dR4`). `refresh_twice` shows the displacement fire was always kept
    // (`dR5` as `R{6}` displaces it) and only the exit fire (`dR6`) was at
    // issue; `local_source` is the boundary a local RHS never marks a view.
    let out = run_program(
        r#"
struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }

fn mk9() -> R { return R { id: 9, tag: f"n" } }

fn refresh(h: R) -> i64 {
    let mut a: R = R { id: 1, tag: f"a" };
    a = h;
    a = R { id: 5, tag: f"c" };
    return a.id
}

fn refresh_twice(h: R) -> i64 {
    let mut a: R = R { id: 1, tag: f"a" };
    a = h;
    a = R { id: 5, tag: f"c" };
    a = R { id: 6, tag: f"d" };
    return a.id
}

fn local_source() -> i64 {
    let g: R = mk9();
    let mut a: R = R { id: 1, tag: f"a" };
    a = g;
    a = R { id: 5, tag: f"c" };
    return a.id
}

fn main() {
    println(f"v{refresh(R { id: 4, tag: f"b" })}");
    println("-");
    println(f"v{refresh_twice(R { id: 4, tag: f"b" })}");
    println("-");
    println(f"v{local_source()}");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out.trim(),
            "dR1\ndR5\ndR4\nv5\n-\ndR1\ndR5\ndR6\ndR4\nv6\n-\ndR1\ndR9\ndR5\nv5"
        );
    }
}

#[test]
fn test_e2e_aggregate_literal_at_explicit_return_single_fire() {
    // B-2026-08-30-18 — an aggregate literal built DIRECTLY at an
    // explicit `return`, carrying a `Vec` of `Drop` elements moved in from
    // a named local. The explicit-`return` hook set mirrors the tail set
    // in `suppress_cleanup_for_tail_return` but was missing
    // B-2026-08-02-23 leg 2's aggregate-literal source disarm, so the
    // source's Drop-BODIES action stayed armed and fired once at the
    // callee's exit AND again at the caller's, on all three compiled
    // surfaces:
    //
    //     pre-fix  `mid dR14 v14 dR14 post`
    //     --interp `mid v14 dR14 post`
    //
    // The two working spellings both routed the value through something
    // already covered — `let bx = …; return bx` through the
    // bare-Identifier arm, the bare tail `Box3 { xs: v }` through leg 2
    // itself — so the return-position literal was the only spelling with
    // no owner for the bodies half.
    //
    // BODIES ONLY, which is why nothing crashed and this needed a
    // fire-COUNT oracle to see: the MEMORY half was already transferred by
    // `suppress_source_vec_cleanup_for_arg`, so the buffer had exactly one
    // owner and valgrind reported 0 errors and 0 leaked bytes. What
    // doubled was the observable side effect.
    //
    // Four shapes, each doubling pre-fix and each firing exactly once now:
    // the struct literal, a TUPLE literal at the same position, a
    // two-`Vec`-field literal (two independent sources in one literal),
    // and a CONDITIONAL return (the disarm is static, so it must cover
    // every returning path).
    let out = run_program(
        r#"
struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct Box3 { xs: Vec[R] }
struct Two { xs: Vec[R], ys: Vec[R] }

fn build(k: i64) -> Box3 {
    let mut v: Vec[R] = Vec.new();
    v.push(R { id: k, tag: f"t" });
    println("mid");
    return Box3 { xs: v }
}

fn build_tuple(k: i64) -> (Vec[R], i64) {
    let mut v: Vec[R] = Vec.new();
    v.push(R { id: k, tag: f"t" });
    return (v, 7)
}

fn build_two(k: i64) -> Two {
    let mut a: Vec[R] = Vec.new();
    a.push(R { id: k, tag: f"t" });
    let mut b: Vec[R] = Vec.new();
    b.push(R { id: k + 1, tag: f"u" });
    return Two { xs: a, ys: b }
}

fn build_cond(k: i64) -> Box3 {
    let mut v: Vec[R] = Vec.new();
    v.push(R { id: k, tag: f"t" });
    if k > 0 {
        return Box3 { xs: v }
    }
    return Box3 { xs: v }
}

fn main() {
    {
        let a: Box3 = build(14);
        println(f"v{a.xs[0].id}");
    }
    println("A");
    {
        let t: (Vec[R], i64) = build_tuple(20);
        println(f"t{t.1}");
    }
    println("B");
    {
        let w: Two = build_two(30);
        println(f"w{w.xs[0].id}{w.ys[0].id}");
    }
    println("C");
    {
        let c: Box3 = build_cond(40);
        println(f"c{c.xs[0].id}");
    }
    println("D");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out.trim(),
            "mid\nv14\ndR14\nA\nt7\ndR20\nB\nw3031\ndR31\ndR30\nC\nc40\ndR40\nD"
        );
    }
}

/// The MIXED-tail case, and the reason the recursion quantifies with
/// `all` rather than `any`.
///
/// One tail manufactures a fresh node; the other reads a handle the
/// container still owns. Admitting the branch on the strength of the
/// fresh tail alone would make the slot a second owner on the *other*
/// path — a use-after-free reachable only when `c` is false. So a single
/// place-reading tail has to sink the whole expression, and this test is
/// what fails if someone later "aligns" the quantifier with the ANY-tail
/// sibling walker (`init_projects_out_of_container_element`), which
/// narrows a registration where this widens a permission.
#[test]
fn test_loop_break_mixed_branch_tails_stay_refused() {
    let src = r#"
shared struct Inner { v: i64 }
struct Outer { inner: Inner }

fn pick(o: Outer, c: bool) -> Inner {
    let mut i = 0;
    loop {
        i = i + 1;
        if i == 2 { break if c { Inner { v: 1 } } else { o.inner } }
    }
}

fn main() { let o = Outer { inner: Inner { v: 5 } }; println(pick(o, false).v); }
"#;
    let mut parsed = karac::parse(src);
    assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
    karac::prepare_for_resolve(&mut parsed.program);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);
    let ownership = karac::ownershipcheck(&parsed.program, &typed);
    let err = compile_to_ir(&parsed.program, Some(&ownership), None)
        .expect_err("one place-reading tail must sink the whole branch")
        .message;
    assert!(
        err.contains("Module verification failed"),
        "expected the loud refusal, got: {err}"
    );
}

#[test]
fn test_e2e_value_fn_loop_tail_terminates_with_unreachable() {
    // A value-returning function whose body's final expression is a
    // `loop` with interior `return`s produces no tail value — the
    // function-end path must emit `unreachable` for the dead tail,
    // not a type-mismatched `ret void` (module verification failure:
    // "Function return type does not match operand type of return
    // inst"). Kata-22 closure_number shape.
    let out = run_program(
        r#"
fn build_rows(n: i64) -> Vec[String] {
    let mut m = 1;
    loop {
        let mut row: Vec[String] = Vec.new();
        row.push(f"row{m}");
        if m == n {
            return row;
        }
        m = m + 1;
    }
}

fn main() {
    let rows = build_rows(3);
    println(rows.len());
    println(rows[0]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "1\nrow3");
    }
}

#[test]
fn e2e_multi_assign_swap_locals() {
    // `a, b = b, a;` evaluates both RHS before writing either target, so it
    // swaps. Parser-desugared to a temp-block of let/assign; this confirms
    // the lowering executes correctly under codegen.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let mut a = 1i64;\n\
                 let mut b = 2i64;\n\
                 a, b = b, a;\n\
                 println(f\"{a} {b}\");\n\
             }",
    ) {
        assert_eq!(out, "2 1\n");
    }
}

#[test]
fn e2e_multi_assign_three_way_rotate() {
    // n-ary parallel assignment rotates without a manual temp.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let mut x = 1i64; let mut y = 2i64; let mut z = 3i64;\n\
                 x, y, z = z, x, y;\n\
                 println(f\"{x} {y} {z}\");\n\
             }",
    ) {
        assert_eq!(out, "3 1 2\n");
    }
}

/// B-2026-08-17-17 — deferred-init through a bare `loop` whose
/// assignment dominates every `break` (the DA-table shape the
/// ownership gate used to reject; scalar deferred-init lowering is
/// B-2026-08-17-13's). Paired with
/// `test_loop_break_dominated_init_oracle` in `tests/interpreter.rs`.
#[test]
fn test_e2e_loop_break_dominated_deferred_init() {
    assert_eq!(
        run_program(
            r#"
fn main() {
    let x: i64;
    loop { x = 1; break; }
    println(x);
    let c = true;
    let mut y: i64;
    outer: loop {
        loop {
            if c { y = 9; break outer; }
            y = 1;
            break;
        }
        y = 4;
        break;
    }
    println(y);
}
"#
        ),
        Some("1\n9\n".to_string())
    );
}

/// The block-receiver fix must read the tail's type in the BLOCK's OWN
/// scope, not in whatever scope survives it (B-2026-08-27-49).
///
/// This is the case that separates the fix from the one-line version of it.
/// `type_name_of_expr` is consulted AFTER `compile_block_with_frame` has
/// reverted the block's bindings, so re-deriving the tail's type there
/// resolves the identifier `x` against the OUTER `x` — which the
/// block-local shadows. Both structs here declare an `n`, at DIFFERENT
/// indices, so that mistake is not a build error: it reads `Tag`'s storage
/// at `Other`'s index for `n` and silently prints 99, the value of `Tag.a`.
/// Measured — that is exactly what the naive arm did before the recording
/// one replaced it.
///
/// A silent wrong answer in place of the loud gap would be strictly worse
/// than the bug being fixed, so this guards the capture POINT, which no
/// other fixture can distinguish.
#[test]
fn test_e2e_block_receiver_tail_is_typed_in_the_blocks_own_scope() {
    let src = r#"
struct Other { a: i64, n: i64 }
struct Tag { n: i64, a: i64 }

fn make() -> Tag { return Tag { n: 7, a: 99 }; }

fn main() {
    let x = Other { a: 5, n: 42 };
    println({ let x = make(); x }.n);
    println(x.n);
}
"#;
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(
        interp_errs.is_empty(),
        "interpreter errors: {interp_errs:?}"
    );
    let expected = interp_out.join("");
    // 7 is the block-local `Tag`'s `n`. 99 would mean the receiver typed as
    // the outer `Other` and the read landed on `Tag.a`; 42 would mean it
    // read the outer binding outright.
    assert_eq!(
        expected, "7\n42\n",
        "interpreter oracle is wrong for a shadowed block tail",
    );
    let Some(aot) = run_program(src) else { return };
    assert_eq!(
        aot, expected,
        "a block receiver's tail must be typed in the block's own scope",
    );
}

/// B-2026-08-27-11 — the compiled twin of
/// `a_user_type_shadowing_a_prelude_name_keeps_its_own_comparison`.
///
/// The total-order float wrapper machinery was keyed on the bare NAME, so
/// a user `struct F64 { .. }` inherited the wrapper's comparison — which
/// reads ONE float field and stops. Two values differing only in a second
/// field compared EQUAL: a FALSE-POSITIVE equality, worse than a false
/// negative because a dedup or a guard clause then takes the wrong branch
/// rather than merely doing redundant work. design.md § Module System
/// makes this the user's type to define ("the language does not reserve
/// them"), and the pre-existing shadowing test covers only LAYOUT, which
/// was already correct.
///
/// THREE DECLARATION KINDS, because each reached a different hole. The
/// PLAIN struct is the row's shape. The SHARED struct additionally proved
/// that `user_shadowed_prelude_types` was recorded in the plain branch
/// ONLY, so a `shared struct F64` was never marked and every consumer of
/// that set — not just this one — went on treating it as the stdlib type.
/// The `#[derive(Eq)]` one reaches the lowered `F64.eq(a, b)` form, since
/// `rewrite_binary` consults a NAME-keyed impl table that the stdlib's own
/// `#[derive(Eq, Ord, Hash, ...)] struct F64` answers for.
///
/// `plain-eq` is not padding. Declining the wrapper machinery is only half
/// the fix; the call then reaches the unknown-callee tail and yields a
/// const `i64` 0, which prints as a plausible-looking `false`. A test with
/// only inequality assertions passes on that bug.
#[test]
fn test_e2e_shadowed_prelude_name_keeps_its_own_comparison() {
    assert_eq!(
        run_program(
            r#"
#[derive(PartialEq)]
struct F64 { x: i64, tag: i64 }
#[derive(PartialEq)]
shared struct F32 { x: i64, tag: i64 }
#[derive(Eq, PartialEq)]
struct F16 { x: i64, tag: i64 }
fn main() {
    let a = F64 { x: 1, tag: 1 };
    let b = F64 { x: 1, tag: 2 };
    let c = F64 { x: 1, tag: 1 };
    println(f"plain-ne={a == b}");
    println(f"plain-eq={a == c}");

    let d = F32 { x: 1, tag: 1 };
    let e = F32 { x: 1, tag: 2 };
    println(f"shared-ne={d == e}");

    let g = F16 { x: 1, tag: 1 };
    let h = F16 { x: 1, tag: 2 };
    println(f"derived-ne={g == h}");

    println(f"fields={a.x} {a.tag}");
}
"#
        ),
        Some(
            "plain-ne=false\nplain-eq=true\nshared-ne=false\n\
                 derived-ne=false\nfields=1 1\n"
                .to_string()
        )
    );
}

/// B-2026-08-27-11, the OTHER direction: the real prelude wrapper must
/// keep every behaviour the shadow check now gates. Its comparison is
/// bit-level (two canonicalized NaNs are EQUAL), it sorts, it is a legal
/// `Map` key where both NaN keys collide into one, and its `Display`
/// prints the wrapped float rather than a struct form.
///
/// Worth asserting next to the shadowed case rather than trusting the
/// wrapper's own tests: the fix touches nine name-keyed sites, and a
/// predicate that is too eager silently demotes the wrapper to an ordinary
/// struct at any one of them.
#[test]
fn test_e2e_unshadowed_total_order_wrapper_keeps_its_behaviour() {
    assert_eq!(
        run_program(
            r#"
fn nan64(z: f64) -> f64 { return z / z; }
fn main() {
    let q = nan64(0.0);
    let c: F64 = F64 { value: q };
    let r: F64 = F64.from(q);
    println(f"nan-eq={c == r}");
    let p: F64 = F64 { value: 2.5 };
    println(f"display={p}");
    let mut v: Vec[F64] = Vec.new();
    v.push(F64 { value: 3.0 });
    v.push(F64 { value: 1.0 });
    v.push(F64 { value: 2.0 });
    v.sort();
    println(f"sorted={v[0].value} {v[1].value} {v[2].value}");
    let mut m: Map[F64, i64] = Map.new();
    let _ = m.insert(c, 1);
    let _ = m.insert(r, 2);
    println(f"keylen={m.len()}");
}
"#
        ),
        Some("nan-eq=true\ndisplay=2.5\nsorted=1 2 3\nkeylen=1\n".to_string())
    );
}

/// B-2026-08-21-41, the class half: a `for` source that codegen cannot
/// lower must be an ERROR, not a loop that runs zero times.
///
/// The dispatch catch-all returned unit — a zero-iteration loop — for any
/// source no arm claimed. That is indistinguishable at the call site from
/// an empty collection, and it has now been found five separate times, once
/// per source shape that happened to fall through: B-2026-07-14-9
/// (`iter_mut`), -14-21 (a rejected `map`/`filter` peel), -07-31-30 (a
/// module-level container binding), -08-21-41 (an array-valued temporary),
/// and -07-14-7, which is where the loud adaptor backstops came from. Each
/// of those fixed ONE shape and left the catch-all silent, so the class
/// kept coming back wearing a new shape. This pins the catch-all itself.
///
/// The carrier is a non-scalar-element array temporary, which is exactly
/// what the `-41` lowering declines: its elements own heap, so
/// materializing the temporary would need drop-tracking the fix does not
/// establish. Deferring it is fine; running the body zero times and
/// printing a plausible number is not. If that shape is ever lowered, this
/// test should be re-pointed at whatever still reaches the catch-all rather
/// than deleted — the contract is about the catch-all, not the carrier.
#[test]
fn e2e_for_over_an_unlowerable_source_bails_loud() {
    let err = ir_result(
        "fn mk() -> Array[String, 2] { return [f\"a\", f\"b\"]; }\n\
             fn main() {\n\
                 let mut n = 0;\n\
                 for s in mk() { n = n + 1; }\n\
                 println(n);\n\
             }\n",
    )
    .expect_err("an unlowerable for-source must bail loud, not run zero times");
    assert!(
        err.contains("silently skipped") && err.contains("interp"),
        "expected the unlowered-iterable message naming the risk and the \
             interpreter, got: {err}"
    );
}

/// B-2026-08-23-6 — an EXPLICIT `-> ()` return type on a non-`main`
/// function miscompiled into unbounded recursion on both compiled backends.
///
/// `-> ()` parses as its own `TypeKind::Unit`, NOT as the empty `Tuple`
/// that `llvm_return_type` already mapped to void, so it fell to the
/// wildcard and got the i64 default. The function then carried an i64 LLVM
/// return type while its body emitted `ret void`. With an explicit
/// `return;` the verifier caught it; WITHOUT one nothing did — control ran
/// off the end and resumed at a wrong address, printing `before x before x
/// …` (back to the top of `main`, not a loop inside `f`) until the stack
/// guard aborted. `fn main() -> ()` was unaffected because `main` is
/// lowered separately, which is why every hello-world smoke test passed.
///
/// The body matrix matters because ONE shape masked the bug: an `if` whose
/// terminator supplied the missing one. Every other body — including the
/// EMPTY body — miscompiled, so a single-shape test would have been a coin
/// flip. Each case here asserts the output equals what dropping the `-> ()`
/// produces, since that one-character-different program was always correct.
#[test]
fn test_e2e_explicit_unit_return_type_does_not_miscompile() {
    let bodies = [
        ("println_one", "println(\"x\");", "before\nx\nafter\n"),
        ("let_only", "let a = 1i64;", "before\nafter\n"),
        ("empty", "", "before\nafter\n"),
        (
            "let_then_fstring",
            "let a = 1i64; println(f\"{a}\");",
            "before\n1\nafter\n",
        ),
        (
            "two_prints",
            "println(\"x\"); println(\"y\");",
            "before\nx\ny\nafter\n",
        ),
        ("bare_return", "return;", "before\nafter\n"),
        (
            "let_then_return",
            "let a = 1i64; return;",
            "before\nafter\n",
        ),
        // The masking shape: the `if`'s terminator hid the missing return,
        // so this one was GREEN before the fix. Kept so a regression that
        // only re-breaks the unmasked shapes is still distinguishable.
        (
            "if_true",
            "if true { println(\"x\"); }",
            "before\nx\nafter\n",
        ),
    ];
    for (name, body, want) in bodies {
        let src = format!(
            "fn f() -> () {{ {body} }}\n\
                 fn main() {{ println(\"before\"); f(); println(\"after\"); }}\n"
        );
        let out = run_program(&src)
            .unwrap_or_else(|| panic!("`-> ()` body `{name}` should build and run"));
        assert_eq!(out, want, "explicit `-> ()` with body `{name}`");

        // The control is the same program with the annotation dropped. It
        // was always correct, and the fix's whole claim is that the two
        // spellings now lower identically — so compare them, not just the
        // literal.
        let ctrl = format!(
            "fn f() {{ {body} }}\n\
                 fn main() {{ println(\"before\"); f(); println(\"after\"); }}\n"
        );
        let ctrl_out = run_program(&ctrl)
            .unwrap_or_else(|| panic!("control body `{name}` should build and run"));
        assert_eq!(
            out, ctrl_out,
            "`-> ()` must lower identically to the omitted annotation (body `{name}`)"
        );
    }
}

/// B-2026-08-23-6, the receiver and function-value surfaces — the same
/// miscompile reached through a method and through a call by value, both
/// of which the report exercised.
#[test]
fn test_e2e_explicit_unit_return_on_method_and_fn_value() {
    let method = "struct S { v: i64 }\n\
             impl S { fn show(ref self) -> () { println(\"m\"); } }\n\
             fn main() { println(\"before\"); let s: S = S { v: 1i64 }; s.show(); println(\"after\"); }\n";
    assert_eq!(
        run_program(method).expect("unit-returning method should build and run"),
        "before\nm\nafter\n"
    );

    // A `Fn(i64) -> ()` PARAMETER type was already fine (closures carried
    // the `Unit`-is-void arm); the defect was the declared function's own
    // signature, so both the by-value call and the direct call must work.
    let fnval = "fn f(n: i64) -> () { println(f\"g{n}\"); }\n\
             fn run(g: Fn(i64) -> () with writes(Stdout)) { g(1i64); }\n\
             fn main() { println(\"before\"); let h = f; h(7i64); run(f); println(\"after\"); }\n";
    assert_eq!(
        run_program(fnval).expect("unit-returning fn used as a value should build and run"),
        "before\ng7\ng1\nafter\n"
    );
}

/// `return <unit-valued expr>;` in a VOID function — a SEPARATE defect the
/// `-> ()` fix uncovered, and one that predated it: `fn g() { return f(); }`
/// with the annotation OMITTED failed module verification the same way
/// ("Found return instr that returns non-void in Function of void return
/// type"), so this was never about `-> ()` at all.
///
/// The tail-return site in `functions.rs` had carried a `fn_returns_void`
/// guard for exactly this since it was written; the explicit-`return` site
/// in `exprs.rs` had not. Both spellings are asserted because the point of
/// the pair is that they agree.
#[test]
fn test_e2e_return_of_unit_call_in_void_fn() {
    for (name, ann) in [("annotated", " -> ()"), ("omitted", "")] {
        let src = format!(
            "fn f() -> () {{ println(\"x\"); }}\n\
                 fn g(){ann} {{ return f(); }}\n\
                 fn main() {{ g(); println(\"after\"); }}\n"
        );
        assert_eq!(
            run_program(&src)
                .unwrap_or_else(|| panic!("`return f();` in a void fn ({name}) should build")),
            "x\nafter\n",
            "return of a unit call, {name} return type"
        );
    }
}

/// Leg 5 — a CONDITIONAL store, on the path where it fires. This is the
/// direction that decides the predicate's conservatism, so it is asserted
/// rather than left to the doc comment: the named spelling printed
/// `drop 200  pushed 1  drop 200` — a double body over a value living in
/// `sink` — while the temp spelling of the same callee was already correct.
///
/// The other path (the store does NOT fire) LOSES the body, on both
/// spellings, and that is deliberate: no per-callee static predicate can be
/// right on both paths, and `fn_moves_param_into_outliving_place` documents
/// the leak-over-double-free lean it is answering with. Tracked separately
/// rather than hidden here.
#[test]
fn e2e_conditional_store_that_fires_runs_one_body_on_both_spellings() {
    for (label, call) in [
        (
            "named",
            "let carg: Res = Res { id: 200 };\n     take(mut sink, carg);",
        ),
        ("temp", "take(mut sink, Res { id: 200 });"),
    ] {
        let src = format!(
            "struct Res {{ id: i64 }}\n\
                 impl Drop for Res {{\n\
                 \x20   fn drop(mut ref self) {{ println(f\"drop {{self.id}}\"); }}\n\
                 }}\n\
                 fn take(sink: mut ref Vec[Res], r: Res) {{ if r.id > 100 {{ sink.push(r); }} }}\n\
                 fn main() {{\n\
                 \x20   let mut sink: Vec[Res] = Vec.new();\n\
                 \x20   {call}\n\
                 \x20   println(f\"pushed {{sink.len()}}\");\n\
                 }}\n"
        );
        assert_eq!(
            run_program(&src),
            Some("pushed 1\ndrop 200\n".to_string()),
            "[{label}] a conditional store that FIRES must leave exactly one owner"
        );
    }
}

/// B-2026-09-24-29 — an `if let` whose value is an f-string. Its THEN arm
/// hand-rolls its frame instead of going through `compile_block_with_frame`,
/// so nothing zeroed the tail accumulator's `cap`: the arm's drain freed the
/// buffer the construct's value had just loaded, and the consumer freed it
/// again. On `main` that double freed on every compiled surface whatever the
/// scrutinee (`Some(7)` included), as a `let`, a function tail, a block
/// tail, an `else if` chain, a loop body, a `Vec.push` argument and before
/// an early `return`. With the arm disarmed, an `if let` in argument or
/// receiver position was left owned by nobody, so it is now admitted as a
/// fresh owned branch wrapper beside `if` and `match`. The discarded
/// spellings (a statement, `let _ =`) must stay clean.
#[test]
fn e2e_if_let_fstring_value_is_freed_once() {
    let Some(out) = run_program(
        r#"enum E { A(String), B(i64) }
fn mk(n: i64) -> String { f"heap-string-longer-than-sso-{n}" }
fn tail(doc: Option[String]) -> String { if let Some(s) = doc { f"x {s}" } else { f"none" } }
fn kind(e: E) -> String { if let E.A(s) = e { f"a {s}" } else { f"b" } }
fn early(o: Option[i64]) -> String { let r = if let Some(s) = o { f"x {s}" } else { return f"early" }; r }
fn show(s: String) { println(s) }
fn main() {
    let doc = Some(7);
    let r = if let Some(s) = doc { f"x" } else { f"none" };
    println(r);
    let h = Some(f"heap-string-longer-than-sso-1");
    let r2 = if let Some(s) = h { f"x {s}" } else { f"none" };
    println(r2);
    println(tail(Some(mk(2))));
    println(tail(None));
    let r3 = if let Some(s) = doc { let k = s + 1; f"x {k}" } else { f"none" };
    println(r3);
    if let Some(s) = doc { f"x {s}" } else { f"none" };
    let _ = if let Some(s) = doc { f"x {s}" } else { f"none" };
    let mut n = 0;
    for i in 0..5 { let o = if i % 2 == 0 { Some(i) } else { None }; let q = if let Some(s) = o { mk(s) } else { f"none" }; n = n + q.len(); }
    println(n);
    println(if let Some(s) = doc { f"x {s}" } else { f"none" });
    let no: Option[i64] = None;
    println(if let Some(s) = no { f"x {s}" } else { f"none" });
    println((if let Some(s) = doc { f"x {s}" } else { f"none" }).len());
    show(if let Some(s) = doc { f"x {s}" } else { f"none" });
    println(if let Some(s) = no { f"x {s}" } else if let Some(t) = Some(3) { f"y {t}" } else { f"none" });
    let mut v: Vec[String] = [];
    for i in 0..3 { let o = Some(i); v.push(if let Some(s) = o { mk(s) } else { f"none" }); }
    println(v[2]);
    println(kind(E.A(mk(3))));
    println(kind(E.B(2)));
    println(early(Some(1)));
    println(early(None));
    println((if let Some(s) = doc { mk(s) } else { mk(0) }).len());
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "x\nx heap-string-longer-than-sso-1\nx heap-string-longer-than-sso-2\nnone\nx 8\n95\nx 7\nnone\n3\nx 7\ny 3\nheap-string-longer-than-sso-2\na heap-string-longer-than-sso-3\nb\nx 1\nearly\n29\nend\n", "got:\n{out}");
}
