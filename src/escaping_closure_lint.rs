// src/escaping_closure_lint.rs
//! `escaping_closure` — surface codegen's `E_ESCAPING_CLOSURE_NOT_YET`
//! deferral at CHECK time. B-2026-08-16-13.
//!
//! A closure that captures locals and then escapes in a shape the
//! heap-closure-environment epic (B-2026-06-22-2) has not yet lowered —
//! stored into a struct pushed into a `Vec`, passed as an unbound call
//! result, returned from inside a branch, … — is deliberately refused by
//! `karac build`, with one of the compiler's best error messages. What was
//! missing is that NOTHING said so before build: `karac check`,
//! `--targets=native`, and `--output=json` were all silent, `karac fix` had
//! nothing to apply, and `run --interp` executed the program fine — the
//! full run-vs-build gap, one deferral over from B-2026-08-13-12.
//!
//! ## Why this file contains no predicate
//!
//! Unlike `chained_receiver_lint` (a two-line frozen syntactic shape), the
//! escaping-closure boundary is a live state machine the epic has widened
//! four times. B-2026-08-16-13 BUILT the hand-mirrored lint, validated it
//! against a 394-file sweep, and DISCARDED it: every mirrored arm is a bet
//! against the epic's next slice, and a stale arm is a Deny-level false
//! positive on a program that builds — strictly worse than the silence it
//! replaced. So the predicate lives in `crate::closure_escape`, the SAME
//! plain-AST module codegen's build gate runs; this file only enumerates
//! the functions the compile loop would validate and renders the shared
//! violations as diagnostics. One predicate, zero drift.
//!
//! ## Enumeration parity with the compile loop
//!
//! `karac build` runs the validators on every non-comptime, non-generic
//! free function and every synthesized non-generic impl method (the same
//! `make_impl_method_function` synthesis, shared from `closure_escape`).
//! Generic functions/impls are validated only for instantiations codegen
//! actually compiles, and REPL declare-only functions are skipped — both
//! are enumerated here in neither direction's favor: this lint checks the
//! same non-generic set eagerly and skips generic bodies entirely, so a
//! never-instantiated generic body with an escaping closure stays silent at
//! check (it does not fail build either — under-fire, the safe direction),
//! and it can never flag a program `build` accepts.
//!
//! ## Level
//!
//! Registry default `Deny` — the program cannot compile, so a warning would
//! leave `check` exiting 0 on it, the exact gap this closes. The tree-walk
//! interpreter DOES support these shapes, so `-A escaping_closure` opts out
//! for interp-only programs, and the pipeline only runs this lint when
//! bound for codegen (`Pipeline::codegen_bound`) — `run --interp` is exempt
//! by construction.

use crate::ast::{Function, Item, Program, TypeKind};
use crate::closure_escape::{make_impl_method_function, EscapeAnalysis, EscapeViolation};
use crate::typechecker::{TypeError, TypeErrorKind};
use std::collections::HashMap;

const LINT_NAME: &str = "escaping_closure";

fn diagnostic(v: EscapeViolation) -> TypeError {
    // The shared message is codegen-shaped (`error[E_ESCAPING_CLOSURE_NOT_YET]:
    // …`); the diagnostic pipeline adds its own severity prefix, so strip the
    // code header here and keep the prose — the boundary lists and the
    // workaround stay word-for-word what `karac build` would say.
    let core = v
        .message
        .strip_prefix("error[E_ESCAPING_CLOSURE_NOT_YET]: ")
        .unwrap_or(&v.message);
    TypeError {
        message: format!(
            "{core} (This is codegen's E_ESCAPING_CLOSURE_NOT_YET deferral surfaced at check \
             time; the tree-walk interpreter supports this shape — `-A {LINT_NAME}` if the \
             program is only ever run with `--interp`.)"
        ),
        span: v.span,
        kind: TypeErrorKind::TypeMismatch,
        lint_name: Some(LINT_NAME.to_string()),
        // No `fix_it`: the workaround the message prescribes (store the
        // closure's data and dispatch with a plain fn, or restructure the
        // owner) is a design change, not a mechanical rewrite — the row
        // measured this ("NO mechanical rewrite to offer").
        fix_it: None,
        class: Some(crate::diagnostic_class::DiagnosticClass::LintWarning),
        expected: None,
        got: None,
    }
}

/// Run the shared escape analysis over the program exactly as `karac build`
/// would and collect one diagnostic per rejected function, plus whether the
/// resolved level makes them ERRORS. The caller routes them into
/// `TypeCheckResult::errors` or `::warnings` accordingly.
pub fn check_escaping_closures(
    program: &Program,
    cli_lint_overrides: &crate::lints::CliLintOverrides,
) -> (Vec<TypeError>, bool) {
    let severity = crate::lints::effective_level_for_module_lint(
        false,
        false,
        false,
        cli_lint_overrides,
        LINT_NAME,
    );
    if matches!(severity, crate::lints::ModuleLintSeverity::Suppress) {
        return (Vec::new(), false);
    }
    let deny = matches!(severity, crate::lints::ModuleLintSeverity::Deny);

    // The same `fn_asts` map codegen's declaration pass builds: non-generic
    // free functions keyed by name (generic ones route through the mono
    // pipeline and are not producer-set members).
    let mut fn_asts: HashMap<String, Function> = HashMap::new();
    for item in &program.items {
        if let Item::Function(f) = item {
            if f.generic_params.is_none() {
                fn_asts.insert(f.name.clone(), f.clone());
            }
        }
    }
    let mut analysis = EscapeAnalysis::compute(&fn_asts);

    let mut out = Vec::new();
    let check = |analysis: &mut EscapeAnalysis, func: &Function, out: &mut Vec<TypeError>| {
        if let Err(v) = analysis.check_function(func) {
            out.push(diagnostic(v));
        }
    };
    for item in &program.items {
        match item {
            Item::Function(f) => {
                // Mirror the compile loop's gates: comptime-only fns are never
                // emitted; generic fns are validated per instantiation, which
                // this eager pass cannot enumerate — skip both (under-fire).
                if !f.is_comptime && f.generic_params.is_none() {
                    check(&mut analysis, f, &mut out);
                }
            }
            Item::ImplBlock(imp) => {
                // Mirror the impl-method compile loop: a method generic via
                // its own params OR the impl's routes through the mono
                // pipeline — skip. The synthesis is the shared
                // `make_impl_method_function`, so the validated shape (self
                // param, `Self` return rewrite) is byte-identical to what
                // `compile_function` sees.
                if imp.generic_params.is_some() {
                    continue;
                }
                let type_name = match &imp.target_type.kind {
                    TypeKind::Path(p) => match p.segments.last() {
                        Some(head) => head.clone(),
                        None => continue,
                    },
                    _ => continue,
                };
                for impl_item in &imp.items {
                    if let crate::ast::ImplItem::Method(m) = impl_item {
                        if m.generic_params.is_some() || m.is_comptime {
                            continue;
                        }
                        let synth = make_impl_method_function(&type_name, m, &imp.target_type);
                        check(&mut analysis, &synth, &mut out);
                    }
                }
            }
            _ => {}
        }
    }
    (out, deny)
}
