//! Mutation-based property test of the no-lifetime-annotation safety claim.
//!
//! `tests/safety_design.rs` curates a small corpus of programs that exercise
//! the design's borrow patterns. Each one is a single witness — one shape,
//! one accept. This file generalizes those witnesses into a *property*:
//!
//!   "for every program P that the static pipeline accepts, and for every
//!    semantics-preserving mutation M, the pipeline must still accept M(P)
//!    — and the resulting binary must still run cleanly under ASAN."
//!
//! The ASAN half ran on the legacy backend and went with it at M3; the
//! static half is what this file checks now.
//!
//! That is the closure of the static→runtime safety claim: not just "this
//! one program is safe" but "any small textual perturbation of a safe
//! program stays safe." If a mutation breaks the invariant, the bug is
//! either in the mutation (it wasn't actually semantics-preserving) or in
//! the compiler (a fragile path the curated corpus didn't hit).
//!
//! The mutation operators here are intentionally small and textual rather
//! than AST-based. Textual mutations are easy to audit for soundness
//! ("inserting a `let _x = 0;` cannot change observable behavior of any
//! program in our corpus") and cheap to write — full AST-aware mutation
//! belongs in `fuzz/fuzz_targets/` once the corpus grows beyond what
//! hand-curation can cover.
//!
//! macOS leak gap from `tests/memory_sanitizer.rs:95-104` applies here
//! too — see header of `tests/safety_design.rs`.

use karac::{ownershipcheck, parse, resolve, typecheck};

// ── Corpus ───────────────────────────────────────────────────────
//
// Programs are kept short so the failure messages stay readable when a
// mutation regresses one of them. Each is a witness already shipped in
// the curated suite — keeping them in lockstep means a regression here
// implicates either `safety_design.rs`'s case or the compiler.

const CORPUS: &[(&str, &str)] = &[
    (
        "single_source_borrow_return",
        "fn echo(s: ref String) -> ref String { s }\n\
         fn main() {\n\
             let s = String.from(\"hello\");\n\
             let t = echo(s);\n\
             println(t.len());\n\
         }",
    ),
    (
        "multi_source_borrow_return",
        "fn longer(a: ref String, b: ref String) -> ref String {\n\
             if a.len() > b.len() { a } else { b }\n\
         }\n\
         fn main() {\n\
             let x = String.from(\"short\");\n\
             let y = String.from(\"a longer string\");\n\
             let z = longer(x, y);\n\
             println(z.len());\n\
         }",
    ),
    (
        "borrowed_struct_construction",
        "struct Parser {\n\
             source: ref String,\n\
             position: i64,\n\
         }\n\
         fn main() {\n\
             let s = String.from(\"input\");\n\
             let p = Parser { source: s, position: 0 };\n\
             println(p.position);\n\
         }",
    ),
    (
        "closure_borrow_capture_no_escape",
        "fn main() {\n\
             let s = String.from(\"hello\");\n\
             let len_plus = |extra: i64| s.len() + extra;\n\
             println(len_plus(5));\n\
         }",
    ),
    // ── Adversarial accept controls (mirror tests/safety_design.rs
    // Section 2½). These are hostile-corpus *accept* cases — a borrow
    // routed through a borrowed collection / generic wrapper / RC boundary
    // with a genuinely-outliving source. Adding them here generalizes the
    // "accept survives every semantics-preserving mutation" property from
    // the plain-`ref` seeds to the adversarial accept surface, per the
    // phase-9 tracker ("extend it over the new corpus so the property holds
    // adversarially, not just on the hand-written seeds").
    (
        "adversarial_borrowed_collection_from_param",
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
    ),
    (
        "adversarial_option_ref_from_param",
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
    ),
    (
        "adversarial_shared_struct_backpointer",
        "shared struct Node {\n\
             mut next: Option[Node],\n\
             value: i64,\n\
         }\n\
         fn main() {\n\
             let a = Node { next: Option.None, value: 1 };\n\
             let b = Node { next: Option.Some(a), value: 2 };\n\
             println(b.value);\n\
         }",
    ),
];

// ── Mutation operators ───────────────────────────────────────────
//
// Each operator either returns `Some(mutated)` if it could apply to the
// program, or `None` if the program shape didn't match (e.g., no `main`
// found). `None` is a no-op: the (program, mutation) pair is skipped, not
// failed. The mutation set is deliberately conservative — every operator
// here has been hand-checked against every corpus program for semantic
// invariance.

type Mutation = fn(&str) -> Option<String>;

/// Inject `let __noop = 0_i64;` as the first statement of `main`.
/// Adds a binding that's never used elsewhere — pure no-op for
/// observable behavior, though it does add one to scope-cleanup actions
/// (which the compiler should handle identically whether the binding is
/// referenced or not).
fn prepend_noop_let_in_main(src: &str) -> Option<String> {
    let needle = "fn main() {\n";
    let idx = src.find(needle)?;
    let mut out = String::with_capacity(src.len() + 32);
    out.push_str(&src[..idx + needle.len()]);
    out.push_str("    let __noop = 0_i64;\n");
    out.push_str(&src[idx + needle.len()..]);
    Some(out)
}

/// Wrap the body of `main` in an extra block. Introduces a new lexical
/// scope but doesn't change observable behavior — the inner block's tail
/// is `()` and main's tail is `()`, so the wrapping is invisible.
fn wrap_main_body_in_block(src: &str) -> Option<String> {
    let open = "fn main() {\n";
    let idx = src.find(open)?;
    let body_start = idx + open.len();

    // Walk from body_start to the matching `}` for `main`'s body. The
    // corpus uses no nested braces inside main except for struct
    // literals, so a depth counter on `{` / `}` characters is enough.
    let bytes = src.as_bytes();
    let mut depth: i32 = 1;
    let mut i = body_start;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
        i += 1;
    }
    if depth != 0 {
        return None;
    }
    let body_end = i;

    let body = &src[body_start..body_end];
    let mut out = String::with_capacity(src.len() + 16);
    out.push_str(&src[..body_start]);
    out.push_str("    {\n");
    out.push_str(body);
    out.push_str("    }\n");
    out.push_str(&src[body_end..]);
    Some(out)
}

/// Trailing whitespace — most innocent mutation possible, included as a
/// sanity check on the mutation harness itself. If this ever fails to
/// accept, the harness has a problem (not the compiler).
fn append_trailing_newline(src: &str) -> Option<String> {
    let mut out = String::with_capacity(src.len() + 1);
    out.push_str(src);
    out.push('\n');
    Some(out)
}

const MUTATIONS: &[(&str, Mutation)] = &[
    ("prepend_noop_let_in_main", prepend_noop_let_in_main),
    ("wrap_main_body_in_block", wrap_main_body_in_block),
    ("append_trailing_newline", append_trailing_newline),
];

// ── Static invariant ─────────────────────────────────────────────

fn assert_static_accept(source: &str, label: &str) {
    let mut parsed = parse(source);
    assert!(
        parsed.errors.is_empty(),
        "[{label}] parse errors: {:?}\n--- source ---\n{source}",
        parsed.errors
    );
    karac::prepare_for_resolve(&mut parsed.program);
    let resolved = resolve(&parsed.program);
    assert!(
        resolved.errors.is_empty(),
        "[{label}] resolve errors: {:?}\n--- source ---\n{source}",
        resolved.errors
    );
    let typed = typecheck(&parsed.program, &resolved);
    assert!(
        typed.errors.is_empty(),
        "[{label}] type errors: {}\n--- source ---\n{source}",
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
        "[{label}] ownership errors: {}\n--- source ---\n{source}",
        ownership
            .errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
}

#[test]
fn static_accept_invariant_under_mutation() {
    let mut applied = 0usize;
    for (prog_label, src) in CORPUS {
        // Sanity-check the un-mutated baseline first. If the corpus
        // itself doesn't accept, the failure should point at the corpus
        // rather than blame a mutation.
        assert_static_accept(src, &format!("baseline:{prog_label}"));

        for (mut_label, mutation) in MUTATIONS {
            let Some(mutated) = mutation(src) else {
                continue;
            };
            let label = format!("{prog_label}::{mut_label}");
            assert_static_accept(&mutated, &label);
            applied += 1;
        }
    }
    // Guard against the silent-pass mode where every mutation returned
    // None and the test trivially succeeded.
    assert!(
        applied >= CORPUS.len(),
        "expected at least one mutation to apply per corpus program; \
         only {applied} applications across {} programs",
        CORPUS.len()
    );
}
